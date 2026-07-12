use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use axum::{
    body::Body,
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::Response,
};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use parking_lot::Mutex as SyncMutex;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Notify;
use tokio::time::{Instant, timeout};

use crate::{
    anthropic::{
        cache::{
            CacheAmplification, CacheSimulation, CacheUsage, RawUsage, ReportedCacheUsagePolicy,
        },
        envelope,
        model_capabilities::ModelCapabilitiesCatalog,
        payload_guard::{
            PayloadByteBreakdown, PayloadGuardConfig, PayloadGuardError, PayloadGuardReport,
            breakdown_anthropic_messages_request, guard_anthropic_messages_request_reusing_body,
            sanitize_anthropic_messages_for_external_forwarding,
        },
        payload_guard_runtime::{
            PreparedExternalMessagesPayload, prepare_external_messages_payload,
        },
        pricing::PricingCatalog,
        prompt_cache::{
            KiroRsToolPromptCachePlan, PromptCacheBounds, PromptCacheProfile, PromptCacheScope,
            PromptCacheTracker,
        },
        prompt_cache_creation_control::PromptCacheCreationController,
        request_facts::{probe_raw_messages_body, rewrite_raw_top_level_model},
        types::MessagesRequest,
        usage::{
            ExternalPoolAttempt, ExternalPoolBilling, ExternalPoolUsageSnapshot, UsageLatencyTrace,
            UsagePublicError, UsageRecord, UsageRecordStatus, UsageRouteKind, UsageRouteSubtype,
            UsageSource,
        },
    },
    kiro::token_manager::storage_task::{
        block_on_storage, spawn_best_effort_storage_task, spawn_critical_storage_task,
    },
    model::config::{
        ExternalPoolCapacityMode, ExternalPoolStreamResponseMode, ExternalPoolsConfig,
        KiroRsToolCachePolicy, ModelMappingRule, PromptCacheCreationControlConfig,
        PromptCacheSimulationMode, PromptCacheStrategyType, ReportedUsageConfig,
    },
    model::model_processing::{
        ModelProcessingConfig, ModelProcessingError, ModelProcessingInput, ModelProcessingMode,
        process_model,
    },
    model::model_support::model_is_supported_by_list,
    storage::{
        postgres::PostgresStore,
        redis_cache::{LocalPoolCircuitState, RedisStore},
    },
    token,
};

#[path = "external_pool/body_pipeline.rs"]
mod body_pipeline;
#[path = "external_pool/model_pipeline.rs"]
mod model_pipeline;
#[path = "external_pool/retry_pipeline.rs"]
mod retry_pipeline;
#[path = "external_pool/usage_projection.rs"]
mod usage_projection;

use usage_projection::ExternalUsageProjectionContext;

#[cfg(test)]
use crate::anthropic::request_facts::raw_messages_body_hints;

const DEFAULT_EXTERNAL_POOL_REQUEST_TIMEOUT_SECS: u64 = 180;
const EXTERNAL_POOL_LEASE_TOUCH_INTERVAL_SECS: u64 = 30;
const EXTERNAL_POOL_QUEUE_LEASE_TTL_SECS: u64 = 60;
const EXTERNAL_POOL_QUEUE_LEASE_RENEW_INTERVAL_SECS: u64 = 20;
const EXTERNAL_POOL_QUEUE_REDIS_OPERATION_TIMEOUT: Duration = Duration::from_secs(2);
const EXTERNAL_POOL_QUEUE_REDIS_RETRY_DELAY: Duration = Duration::from_millis(50);
const EXTERNAL_POOL_LEASE_RELEASE_ATTEMPTS: usize = 2;
const EXTERNAL_POOL_AVAILABILITY_CACHE_TTL: Duration = Duration::from_millis(250);
const MAX_RECORDED_EXTERNAL_ERROR_MESSAGE_BYTES: usize = 8192;
const EXTERNAL_POOL_MAX_SSE_EVENT_BUFFER_BYTES: usize = 1024 * 1024;
const EXTERNAL_POOL_AUTO_DISABLE_REASONS: &[&str] = &[
    "auth_error",
    "security_lock",
    "quota_exhausted",
    "misconfigured_endpoint",
    "channel_disabled",
];

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExternalPoolAuthType {
    Bearer,
    XApiKey,
}

impl Default for ExternalPoolAuthType {
    fn default() -> Self {
        Self::Bearer
    }
}

impl ExternalPoolAuthType {
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "x_api_key" | "x-api-key" | "xapikey" => Self::XApiKey,
            _ => Self::Bearer,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bearer => "bearer",
            Self::XApiKey => "x_api_key",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExternalPoolUsageProjectionMode {
    PassThrough,
    CurrentPathPolicy,
}

impl Default for ExternalPoolUsageProjectionMode {
    fn default() -> Self {
        Self::PassThrough
    }
}

impl ExternalPoolUsageProjectionMode {
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "current_path_policy" => Self::CurrentPathPolicy,
            _ => Self::PassThrough,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::PassThrough => "pass_through",
            Self::CurrentPathPolicy => "current_path_policy",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExternalPoolRequestBodyMode {
    Normalized,
    RawPassthrough,
}

impl Default for ExternalPoolRequestBodyMode {
    fn default() -> Self {
        Self::Normalized
    }
}

impl ExternalPoolRequestBodyMode {
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "raw_passthrough" | "raw" | "passthrough_body" => Self::RawPassthrough,
            _ => Self::Normalized,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normalized => "normalized",
            Self::RawPassthrough => "raw_passthrough",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExternalPoolRawModelMode {
    None,
    ProbeOnly,
    RewriteTopLevel,
}

impl Default for ExternalPoolRawModelMode {
    fn default() -> Self {
        Self::None
    }
}

impl ExternalPoolRawModelMode {
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "probe_only" | "probe" => Self::ProbeOnly,
            "rewrite_top_level" | "rewrite" | "model_rewrite" => Self::RewriteTopLevel,
            _ => Self::None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ProbeOnly => "probe_only",
            Self::RewriteTopLevel => "rewrite_top_level",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExternalPoolAutoDisablePolicy {
    Inherit,
    Disabled,
    Enabled,
}

impl Default for ExternalPoolAutoDisablePolicy {
    fn default() -> Self {
        Self::Inherit
    }
}

impl ExternalPoolAutoDisablePolicy {
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "disabled" => Self::Disabled,
            "enabled" => Self::Enabled,
            _ => Self::Inherit,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inherit => "inherit",
            Self::Disabled => "disabled",
            Self::Enabled => "enabled",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExternalPoolModelMappingMode {
    Passthrough,
    PassthroughMapping,
    DirectMapping,
    ProcessedMapping,
}

impl Default for ExternalPoolModelMappingMode {
    fn default() -> Self {
        Self::ProcessedMapping
    }
}

impl ExternalPoolModelMappingMode {
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "passthrough" | "pass_through" => Self::Passthrough,
            "passthrough_mapping" | "pass_through_mapping" | "passthrough_with_mapping" => {
                Self::PassthroughMapping
            }
            "direct_mapping" | "direct" => Self::DirectMapping,
            _ => Self::ProcessedMapping,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Passthrough => "passthrough",
            Self::PassthroughMapping => "passthrough_mapping",
            Self::DirectMapping => "direct_mapping",
            Self::ProcessedMapping => "processed_mapping",
        }
    }

    fn processing_mode(self) -> ModelProcessingMode {
        match self {
            Self::Passthrough => ModelProcessingMode::Passthrough,
            Self::PassthroughMapping => ModelProcessingMode::PassthroughMapping,
            Self::DirectMapping => ModelProcessingMode::MappingThenProcessed,
            Self::ProcessedMapping => ModelProcessingMode::ProcessedThenMapping,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalPool {
    pub id: u64,
    pub name: String,
    pub base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub masked_api_key: Option<String>,
    pub auth_type: ExternalPoolAuthType,
    pub enabled: bool,
    pub priority: i32,
    pub max_concurrent_requests: u32,
    pub usage_projection_mode: ExternalPoolUsageProjectionMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_response_mode: Option<ExternalPoolStreamResponseMode>,
    #[serde(default)]
    pub request_body_mode: ExternalPoolRequestBodyMode,
    #[serde(default)]
    pub raw_model_mode: ExternalPoolRawModelMode,
    pub auto_disable_policy: ExternalPoolAutoDisablePolicy,
    pub auto_disabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_disabled_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_disabled_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_disabled_until: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_disabled_last_error: Option<String>,
    pub preserve_path: bool,
    #[serde(default)]
    pub normalize_model_version_dots: bool,
    #[serde(default)]
    pub model_mapping_mode: ExternalPoolModelMappingMode,
    #[serde(default)]
    pub model_mapping_require_match: bool,
    #[serde(default)]
    pub model_mapping_rules: Vec<ModelMappingRule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_models: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ExternalPool {
    pub fn is_auto_disabled_now(&self) -> bool {
        if !self.auto_disabled {
            return false;
        }
        self.auto_disabled_until
            .map(|until| until > Utc::now())
            .unwrap_or(true)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateExternalPoolRequest {
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    #[serde(default)]
    pub auth_type: ExternalPoolAuthType,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_priority")]
    pub priority: i32,
    #[serde(default = "default_external_pool_concurrency")]
    pub max_concurrent_requests: u32,
    #[serde(default)]
    pub usage_projection_mode: ExternalPoolUsageProjectionMode,
    #[serde(default)]
    pub stream_response_mode: Option<ExternalPoolStreamResponseMode>,
    #[serde(default)]
    pub request_body_mode: ExternalPoolRequestBodyMode,
    #[serde(default)]
    pub raw_model_mode: ExternalPoolRawModelMode,
    #[serde(default)]
    pub auto_disable_policy: ExternalPoolAutoDisablePolicy,
    #[serde(default = "default_true")]
    pub preserve_path: bool,
    #[serde(default)]
    pub normalize_model_version_dots: bool,
    #[serde(default)]
    pub model_mapping_mode: ExternalPoolModelMappingMode,
    #[serde(default)]
    pub model_mapping_require_match: bool,
    #[serde(default)]
    pub model_mapping_rules: Vec<ModelMappingRule>,
    #[serde(default)]
    pub supported_models: Vec<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateExternalPoolRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub auth_type: Option<ExternalPoolAuthType>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub max_concurrent_requests: Option<u32>,
    #[serde(default)]
    pub usage_projection_mode: Option<ExternalPoolUsageProjectionMode>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_stream_response_mode_update"
    )]
    pub stream_response_mode: Option<Option<ExternalPoolStreamResponseMode>>,
    #[serde(default)]
    pub request_body_mode: Option<ExternalPoolRequestBodyMode>,
    #[serde(default)]
    pub raw_model_mode: Option<ExternalPoolRawModelMode>,
    #[serde(default)]
    pub auto_disable_policy: Option<ExternalPoolAutoDisablePolicy>,
    #[serde(default)]
    pub preserve_path: Option<bool>,
    #[serde(default)]
    pub normalize_model_version_dots: Option<bool>,
    #[serde(default)]
    pub model_mapping_mode: Option<ExternalPoolModelMappingMode>,
    #[serde(default)]
    pub model_mapping_require_match: Option<bool>,
    #[serde(default)]
    pub model_mapping_rules: Option<Vec<ModelMappingRule>>,
    #[serde(default)]
    pub supported_models: Option<Vec<String>>,
    #[serde(default)]
    pub notes: Option<String>,
}

fn deserialize_optional_stream_response_mode_update<'de, D>(
    deserializer: D,
) -> Result<Option<Option<ExternalPoolStreamResponseMode>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<ExternalPoolStreamResponseMode>::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetExternalPoolEnabledRequest {
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalPoolStatus {
    pub pool: ExternalPool,
    pub in_flight: u32,
    pub cooldown_remaining_secs: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cooldown_reason: Option<String>,
    pub dispatchable: bool,
    pub skipped_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalPoolsListResponse {
    pub pools: Vec<ExternalPool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalPoolsStatusResponse {
    pub pools: Vec<ExternalPoolStatus>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalPoolTestResponse {
    pub ok: bool,
    pub status: Option<u16>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
}

#[derive(Clone)]
pub struct ExternalRouteRequest {
    pub raw_body: Bytes,
    pub headers: HeaderMap,
    pub endpoint: String,
    pub payload: Option<MessagesRequest>,
    pub body_mode_filter: Option<ExternalPoolRequestBodyMode>,
    pub model_hint: Option<String>,
    pub stream_hint: Option<bool>,
    pub request_input_tokens: i32,
    pub upstream_model: Option<String>,
    pub model_resolution_source: Option<String>,
    pub model_resolution_note: Option<String>,
    pub route_subtype: UsageRouteSubtype,
    pub fallback_reason: Option<String>,
    pub direct_policy_reason: Option<String>,
    pub local_attempted: bool,
    pub local_preflight: Option<serde_json::Value>,
    pub local_attempts: Vec<crate::kiro::call_trace::KiroCredentialAttempt>,
    pub reported_usage: ReportedUsageConfig,
    pub prompt_cache: Arc<PromptCacheTracker>,
    pub prompt_cache_creation_controller: Arc<PromptCacheCreationController>,
    pub prompt_cache_strategy_type: PromptCacheStrategyType,
    pub prompt_cache_simulation_mode: PromptCacheSimulationMode,
    pub prompt_cache_route_namespace: Option<String>,
    pub prompt_cache_target_read_ratio: f64,
    pub prompt_cache_token_scale: f64,
    pub prompt_cache_max_simulated_input_tokens: i32,
    pub prompt_cache_cap_jitter_min_tokens: i32,
    pub prompt_cache_cap_jitter_max_tokens: i32,
    pub prompt_cache_scale_min_input_tokens: i32,
    pub prompt_cache_creation_control: PromptCacheCreationControlConfig,
    pub prompt_cache_bounds: PromptCacheBounds,
    pub kiro_rs_tool_cache_policy: KiroRsToolCachePolicy,
    pub model_capabilities: Arc<ModelCapabilitiesCatalog>,
    pub pricing_catalog: Arc<PricingCatalog>,
    pub request_id: String,
    pub error_id: String,
    pub recorder: Arc<crate::anthropic::usage::UsageRecorder>,
    pub started_at: Instant,
    pub first_token_latency_ms: Arc<AtomicU64>,
    pub latency_trace: Arc<ExternalLatencyTraceState>,
    pub payload_breakdown: Option<PayloadByteBreakdown>,
    pub payload_guard_report: Option<PayloadGuardReport>,
    pub payload_guard_external_enabled: bool,
    pub payload_guard_initial_config: PayloadGuardConfig,
    pub payload_guard_retry_config: Option<PayloadGuardConfig>,
}

impl ExternalRouteRequest {
    fn payload(&self) -> Option<&MessagesRequest> {
        self.payload.as_ref()
    }

    fn is_stream(&self) -> bool {
        self.payload
            .as_ref()
            .map(|payload| payload.stream)
            .or(self.stream_hint)
            .unwrap_or(false)
    }

    fn requested_model(&self) -> String {
        self.payload
            .as_ref()
            .map(|payload| payload.model.clone())
            .or_else(|| self.model_hint.clone())
            .unwrap_or_else(|| "unknown".to_string())
    }

    fn model_candidates_for_support(&self) -> [Option<&str>; 3] {
        [
            self.payload.as_ref().map(|payload| payload.model.as_str()),
            self.model_hint.as_deref(),
            None,
        ]
    }

    fn requested_max_tokens(&self) -> Option<i32> {
        self.payload
            .as_ref()
            .and_then(|payload| (payload.max_tokens > 0).then_some(payload.max_tokens))
    }

    fn stable_conversation_id(&self) -> Option<String> {
        self.payload
            .as_ref()
            .and_then(crate::anthropic::converter::extract_stable_conversation_id)
    }
}

#[derive(Debug, Default)]
pub struct ExternalLatencyTraceState {
    upstream_header_ms: AtomicU64,
    first_upstream_chunk_ms: AtomicU64,
    first_output_delta_ms: AtomicU64,
    stream_gap_to_first_output_ms: AtomicU64,
    chunks_before_first_output: AtomicU64,
    events_before_first_output: AtomicU64,
}

impl ExternalLatencyTraceState {
    fn elapsed_ms(started_at: Instant) -> u64 {
        started_at.elapsed().as_millis().max(1) as u64
    }

    fn store_once(field: &AtomicU64, value: u64) {
        if field.load(Ordering::Relaxed) > 0 {
            return;
        }
        let _ = field.compare_exchange(0, value, Ordering::AcqRel, Ordering::Acquire);
    }

    fn mark_upstream_header(&self, started_at: Instant) {
        Self::store_once(&self.upstream_header_ms, Self::elapsed_ms(started_at));
    }

    fn mark_first_upstream_chunk(&self, started_at: Instant) {
        Self::store_once(&self.first_upstream_chunk_ms, Self::elapsed_ms(started_at));
    }

    fn mark_first_output(&self, elapsed_ms: u64, chunks_before: u32, events_before: u32) {
        Self::store_once(&self.first_output_delta_ms, elapsed_ms);
        self.chunks_before_first_output
            .store(chunks_before as u64, Ordering::Release);
        self.events_before_first_output
            .store(events_before as u64, Ordering::Release);
        if let Some(first_chunk_ms) = load_nonzero(&self.first_upstream_chunk_ms) {
            self.stream_gap_to_first_output_ms
                .store(elapsed_ms.saturating_sub(first_chunk_ms), Ordering::Release);
        }
    }

    fn snapshot(&self) -> Option<UsageLatencyTrace> {
        let first_output_delta_ms = load_nonzero(&self.first_output_delta_ms);
        let trace = UsageLatencyTrace {
            capacity_weight_units: None,
            estimated_input_tokens: None,
            payload_guard_ms: None,
            upstream_header_ms: load_nonzero(&self.upstream_header_ms),
            first_upstream_chunk_ms: load_nonzero(&self.first_upstream_chunk_ms),
            first_output_delta_ms,
            first_thinking_delta_ms: None,
            first_visible_text_delta_ms: None,
            stream_gap_to_first_output_ms: load_nonzero(&self.stream_gap_to_first_output_ms),
            chunks_before_first_output: first_output_delta_ms
                .map(|_| self.chunks_before_first_output.load(Ordering::Acquire) as u32),
            events_before_first_output: first_output_delta_ms
                .map(|_| self.events_before_first_output.load(Ordering::Acquire) as u32),
            upstream_bytes_before_first_output: None,
            upstream_frames_before_first_output: None,
            upstream_events_before_first_output: None,
            upstream_frames_without_downstream_events_before_first_output: None,
            upstream_pending_chunks_before_first_output: None,
            upstream_frame_decode_errors_before_first_output: None,
            upstream_event_parse_errors_before_first_output: None,
            upstream_event_types_before_first_output: None,
            client_dropped_ms: None,
            terminal_reason: None,
            upstream_message_status: None,
            saw_upstream_completed: None,
            stop_reason_source: None,
            suspected_intent_preamble_end_turn: None,
        };
        (!trace.is_empty()).then_some(trace)
    }
}

fn load_nonzero(value: &AtomicU64) -> Option<u64> {
    let value = value.load(Ordering::Acquire);
    (value > 0).then_some(value)
}

struct ExternalForwardResponse {
    response: Response,
    outbound_model: Option<String>,
    billing: Option<ExternalPoolBilling>,
    stream_usage_capture: Option<Arc<SyncMutex<ExternalUsageCapture>>>,
    stream_usage_projection: Option<ExternalUsageProjectionContext>,
}

struct ExternalForwardError {
    err: ExternalPoolError,
    outbound_model: Option<String>,
}

impl ExternalForwardError {
    fn new(err: ExternalPoolError, outbound_model: Option<String>) -> Self {
        Self {
            err,
            outbound_model,
        }
    }
}

impl From<ExternalPoolError> for ExternalForwardError {
    fn from(err: ExternalPoolError) -> Self {
        Self::new(err, None)
    }
}

#[derive(Debug, Clone)]
struct ExternalStreamErrorMask {
    request_id: String,
    error_id: String,
    pool_id: u64,
    pool_name: String,
}

pub enum ExternalPoolForwardOutcome {
    Response(Response),
    FinalError(ExternalPoolFinalError),
}

#[derive(Debug, Clone)]
pub struct ExternalPoolFinalError {
    pub status: StatusCode,
    pub response_error_type: String,
    pub route_error_type: String,
    pub message: String,
    pub error_id: String,
    pub retryable: bool,
    pub attempts: Vec<ExternalPoolAttempt>,
    pub pool_id: Option<u64>,
    pub pool_name: Option<String>,
}

impl ExternalPoolFinalError {
    pub fn public_error(&self) -> UsagePublicError {
        UsagePublicError {
            status_code: self.public_status().as_u16(),
            error_type: self.public_error_type().to_string(),
            message: self.public_message(&self.error_id),
        }
    }

    pub fn into_response(self, request_id: &str) -> Response {
        let public_status = self.public_status();
        let public_error_type = self.public_error_type();
        let message = self.public_message(&self.error_id);
        tracing::warn!(
            request_id,
            error_id = %self.error_id,
            status = self.status.as_u16(),
            public_status = public_status.as_u16(),
            response_error_type = %self.response_error_type,
            public_error_type = public_error_type,
            route_error_type = %self.route_error_type,
            retryable = self.retryable,
            pool_id = ?self.pool_id,
            pool_name = ?self.pool_name,
            attempts = ?self.attempts,
            external_message = %self.message,
            "external pool final error"
        );
        envelope::error_response_with_id_and_headers(
            public_status,
            public_error_type,
            message,
            request_id,
            [("x-kiro-rs-error-id", self.error_id)],
        )
    }

    fn public_status(&self) -> StatusCode {
        if self.is_rate_limit() {
            return StatusCode::TOO_MANY_REQUESTS;
        }
        if self.is_public_invalid_request() {
            return StatusCode::BAD_REQUEST;
        }
        if self.status == StatusCode::SERVICE_UNAVAILABLE || self.is_capacity_like() {
            return StatusCode::SERVICE_UNAVAILABLE;
        }
        StatusCode::BAD_GATEWAY
    }

    fn public_error_type(&self) -> &'static str {
        if self.is_rate_limit() {
            return "rate_limit_error";
        }
        if self.is_public_invalid_request() {
            return "invalid_request_error";
        }
        "api_error"
    }

    fn public_message(&self, external_error_id: &str) -> String {
        let message = if self.is_rate_limit() {
            envelope::PUBLIC_RATE_LIMIT_MESSAGE
        } else if self.is_external_prompt_too_long() {
            "Prompt is too long for the external model context window. Reduce conversation history, system prompt, tools, documents, images, or tool results and retry."
        } else if self.is_public_invalid_request() {
            envelope::PUBLIC_INVALID_REQUEST_MESSAGE
        } else {
            envelope::PUBLIC_TEMPORARY_FAILURE_MESSAGE
        };
        envelope::public_message_with_error_id(message, external_error_id)
    }

    pub fn is_public_invalid_request(&self) -> bool {
        self.status == StatusCode::BAD_REQUEST
            && !self.retryable
            && matches!(
                self.route_error_type.as_str(),
                "bad_request" | "client_error"
            )
    }

    pub fn is_rate_limit(&self) -> bool {
        self.status == StatusCode::TOO_MANY_REQUESTS
            || self.route_error_type == "rate_limit"
            || self
                .message
                .to_ascii_lowercase()
                .contains("too many requests")
            || self
                .message
                .to_ascii_lowercase()
                .contains("service_request_rate_exceeded")
    }

    pub fn is_timeout_like(&self) -> bool {
        let lower = self.message.to_ascii_lowercase();
        self.status == StatusCode::REQUEST_TIMEOUT
            || self.status == StatusCode::GATEWAY_TIMEOUT
            || lower.contains("timeout")
            || lower.contains("timed out")
            || lower.contains("deadline")
    }

    pub fn is_external_prompt_too_long(&self) -> bool {
        if self.status != StatusCode::BAD_REQUEST {
            return false;
        }
        let lower = self.message.to_ascii_lowercase();
        lower.contains("prompt is too long")
            || lower.contains("input is too long")
            || lower.contains("context window is full")
            || lower.contains("content_length_exceeds_threshold")
    }

    pub fn is_capacity_like(&self) -> bool {
        matches!(
            self.route_error_type.as_str(),
            "external_pool_capacity_full"
                | "external_pool_queue_full"
                | "external_pool_wait_timeout"
                | "external_pool_cooldown"
                | "external_pool_coordinator_unavailable"
        )
    }
}

#[derive(Debug, Clone, Default)]
struct ExternalUsageCapture {
    request_input_tokens: Option<i32>,
    raw: Option<CacheUsage>,
    shaped: Option<CacheUsage>,
    reported: Option<CacheUsage>,
    projected: bool,
    stream_error_message: Option<String>,
    stream_response_mode: Option<ExternalPoolStreamResponseMode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExternalStreamProcessingPlan {
    response_mode: ExternalPoolStreamResponseMode,
    mask_errors: bool,
    capture_usage: bool,
}

impl ExternalStreamProcessingPlan {
    fn from_mode(response_mode: ExternalPoolStreamResponseMode) -> Self {
        match response_mode {
            ExternalPoolStreamResponseMode::EventPassthrough => Self {
                response_mode,
                mask_errors: true,
                capture_usage: true,
            },
        }
    }

    fn for_pool(pool: &ExternalPool, config: &ExternalPoolsConfig) -> Self {
        Self::from_mode(effective_external_pool_stream_response_mode(pool, config))
    }
}

fn effective_external_pool_stream_response_mode(
    pool: &ExternalPool,
    config: &ExternalPoolsConfig,
) -> ExternalPoolStreamResponseMode {
    pool.stream_response_mode
        .unwrap_or(config.external_pool_stream_response_mode)
}

fn external_pool_max_input_tokens_for_route(
    config: &ExternalPoolsConfig,
    route: &ExternalRouteRequest,
) -> Option<i32> {
    let max_input_tokens = config.external_pool_max_input_tokens.max(0);
    if max_input_tokens == 0 {
        return None;
    }
    let request_input_tokens = route.request_input_tokens.max(0);
    if request_input_tokens > max_input_tokens {
        Some(max_input_tokens)
    } else {
        None
    }
}

#[derive(Clone)]
pub struct ExternalPoolManager {
    postgres: Arc<PostgresStore>,
    redis: Arc<RedisStore>,
    client: reqwest::Client,
    capacity_notify: Arc<Notify>,
    availability_cache: Arc<SyncMutex<Option<CachedPoolAvailabilitySnapshot>>>,
}

struct ExternalPoolLease {
    manager: ExternalPoolManager,
    pool_id: u64,
    lease_id: u64,
}

impl ExternalPoolLease {
    fn touch(&self) -> bool {
        let manager = self.manager.clone();
        let pool_id = self.pool_id;
        let lease_id = self.lease_id;
        spawn_best_effort_storage_task("更新外部池 Redis 并发 lease 活跃时间", async move {
            manager.touch_pool(pool_id, lease_id).await
        })
    }
}

impl Drop for ExternalPoolLease {
    fn drop(&mut self) {
        release_external_pool_lease_reliably(self.manager.clone(), self.pool_id, self.lease_id);
    }
}

async fn release_external_pool_lease_with_retry(
    manager: ExternalPoolManager,
    pool_id: u64,
    lease_id: u64,
    attempts: usize,
) -> anyhow::Result<()> {
    let attempts = attempts.max(1);
    let mut last_error = None;
    for attempt in 0..attempts {
        match timeout(
            EXTERNAL_POOL_QUEUE_REDIS_OPERATION_TIMEOUT,
            manager.release_pool(pool_id, lease_id),
        )
        .await
        {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(err)) => last_error = Some(err),
            Err(_) => {
                last_error = Some(anyhow::anyhow!(
                    "Redis external pool lease release timed out after {}ms",
                    EXTERNAL_POOL_QUEUE_REDIS_OPERATION_TIMEOUT.as_millis()
                ));
            }
        }
        if attempt + 1 < attempts {
            tokio::time::sleep(EXTERNAL_POOL_QUEUE_REDIS_RETRY_DELAY).await;
        }
    }
    Err(anyhow::anyhow!(
        "external pool lease will be reclaimed by its TTL: {}",
        last_error.expect("at least one external pool lease release attempt must run")
    ))
}

fn release_external_pool_lease_reliably(manager: ExternalPoolManager, pool_id: u64, lease_id: u64) {
    let fallback_manager = manager.clone();
    let admitted = spawn_critical_storage_task("释放外部池 Redis 并发 lease", async move {
        release_external_pool_lease_with_retry(
            manager,
            pool_id,
            lease_id,
            EXTERNAL_POOL_LEASE_RELEASE_ATTEMPTS,
        )
        .await
    });
    if !admitted
        && let Err(err) = block_on_storage(
            "关键队列拒绝后同步释放外部池 Redis 并发 lease",
            async move {
                release_external_pool_lease_with_retry(
                    fallback_manager,
                    pool_id,
                    lease_id,
                    EXTERNAL_POOL_LEASE_RELEASE_ATTEMPTS,
                )
                .await
            },
        )
    {
        tracing::error!(
            pool_id,
            lease_id,
            "外部池 Redis 并发 lease 有界重试仍失败，将由 lease TTL 回收: {}",
            err
        );
    }
}

async fn release_external_pool_queue_lease_with_retry(
    manager: ExternalPoolManager,
    lease_id: String,
    attempts: usize,
) -> anyhow::Result<()> {
    let attempts = attempts.max(1);
    let mut last_error = None;
    for attempt in 0..attempts {
        match timeout(
            EXTERNAL_POOL_QUEUE_REDIS_OPERATION_TIMEOUT,
            manager.redis.leave_external_pool_dispatch_queue(&lease_id),
        )
        .await
        {
            Ok(Ok(_)) => {
                manager.capacity_notify.notify_waiters();
                return Ok(());
            }
            Ok(Err(err)) => last_error = Some(err),
            Err(_) => {
                last_error = Some(anyhow::anyhow!(
                    "Redis external pool queue release timed out after {}ms",
                    EXTERNAL_POOL_QUEUE_REDIS_OPERATION_TIMEOUT.as_millis()
                ));
            }
        }
        if attempt + 1 < attempts {
            tokio::time::sleep(EXTERNAL_POOL_QUEUE_REDIS_RETRY_DELAY).await;
        }
    }
    Err(last_error.expect("at least one external pool queue release attempt must run"))
}

fn release_external_pool_queue_lease_reliably(manager: ExternalPoolManager, lease_id: String) {
    let fallback_manager = manager.clone();
    let fallback_lease_id = lease_id.clone();
    let admitted = spawn_critical_storage_task("释放外部池 Redis 调度排队 lease", async move {
        release_external_pool_queue_lease_with_retry(manager, lease_id, 2).await
    });
    if !admitted
        && let Err(err) = block_on_storage(
            "关键队列拒绝后同步释放外部池 Redis 调度排队 lease",
            async move {
                release_external_pool_queue_lease_with_retry(fallback_manager, fallback_lease_id, 2)
                    .await
            },
        )
    {
        tracing::error!(
            "外部池 Redis 调度排队 lease 有界重试仍失败，将由 lease TTL 回收: {}",
            err
        );
    }
}

struct ExternalPoolQueueGuard {
    manager: ExternalPoolManager,
    lease_id: String,
    next_renew_at: Instant,
    released: bool,
}

impl ExternalPoolQueueGuard {
    fn new(manager: ExternalPoolManager, lease_id: String) -> Self {
        Self {
            manager,
            lease_id,
            next_renew_at: Instant::now()
                + Duration::from_secs(EXTERNAL_POOL_QUEUE_LEASE_RENEW_INTERVAL_SECS),
            released: false,
        }
    }

    fn disarm(mut self) {
        self.released = true;
    }

    async fn renew_if_needed(&mut self) -> anyhow::Result<()> {
        if Instant::now() < self.next_renew_at {
            return Ok(());
        }
        let renewed = timeout(
            EXTERNAL_POOL_QUEUE_REDIS_OPERATION_TIMEOUT,
            self.manager.redis.renew_external_pool_dispatch_queue(
                &self.lease_id,
                EXTERNAL_POOL_QUEUE_LEASE_TTL_SECS,
            ),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "Redis external pool queue renewal timed out after {}ms",
                EXTERNAL_POOL_QUEUE_REDIS_OPERATION_TIMEOUT.as_millis()
            )
        })??;
        if !renewed {
            anyhow::bail!("Redis external pool queue lease expired before renewal");
        }
        self.next_renew_at =
            Instant::now() + Duration::from_secs(EXTERNAL_POOL_QUEUE_LEASE_RENEW_INTERVAL_SECS);
        Ok(())
    }

    fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        release_external_pool_queue_lease_reliably(self.manager.clone(), self.lease_id.clone());
    }
}

impl Drop for ExternalPoolQueueGuard {
    fn drop(&mut self) {
        self.release();
    }
}

#[derive(Debug, Clone)]
struct ExternalPoolError {
    status: Option<StatusCode>,
    message: String,
    retryable: bool,
    auto_disable_reason: Option<String>,
    cooldown: Option<(Duration, String)>,
    response_body: Option<Bytes>,
}

#[derive(Debug, Clone, Default)]
struct UsageErrorDiagnostics {
    status_code: Option<u16>,
    source: Option<String>,
    error_id: Option<String>,
    metadata: Option<serde_json::Value>,
    public_error: Option<UsagePublicError>,
}

#[derive(Debug, Clone, Deserialize)]
struct ExternalPoolCooldownState {
    until: DateTime<Utc>,
    reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PoolCapacityWaitReason {
    Full,
    Cooldown,
    CoordinatorUnavailable,
}

#[derive(Debug, Clone)]
struct PoolAcquireUnavailable {
    reason: PoolCapacityWaitReason,
    wait_for: Option<Duration>,
    exclude_pool_for_reselect: bool,
    detail: &'static str,
}

enum PoolAcquireResult {
    Acquired(ExternalPoolLease),
    Unavailable(PoolAcquireUnavailable),
}

#[derive(Debug, Clone, Default)]
struct PoolAvailabilitySnapshot {
    eligible_pools: usize,
    available_pools: usize,
    temporary_unavailable_pools: usize,
    coordinator_unavailable: bool,
    wait_reason: Option<PoolCapacityWaitReason>,
    wait_for: Option<Duration>,
}

#[derive(Debug, Clone, Default)]
struct PoolSelectionSnapshot {
    selected_pool: Option<ExternalPool>,
    availability: PoolAvailabilitySnapshot,
}

#[derive(Debug, Clone)]
struct CachedPoolAvailabilitySnapshot {
    snapshot: PoolAvailabilitySnapshot,
    expires_at: Instant,
}

impl PoolAvailabilitySnapshot {
    fn has_eligible_pool(&self) -> bool {
        self.eligible_pools > 0
    }

    fn has_temporary_unavailable_pool(&self) -> bool {
        self.temporary_unavailable_pools > 0
    }

    fn default_retry_attempts(&self, payload_guard_retry_enabled: bool) -> usize {
        self.eligible_pools
            .max(1)
            .saturating_add(usize::from(payload_guard_retry_enabled))
    }
}

enum ExternalCapacityDecision {
    Retry,
    FinalError(ExternalPoolFinalError),
}

fn default_true() -> bool {
    true
}

fn default_priority() -> i32 {
    100
}

fn default_external_pool_concurrency() -> u32 {
    10
}

pub fn mask_external_pool_key(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() <= 10 {
        return "***".to_string();
    }
    format!(
        "{}...{}",
        &trimmed[..6],
        &trimmed[trimmed.len().saturating_sub(4)..]
    )
}

const EXPLICIT_DIRECT_POLICY_REASON: &str = "explicit_direct";

fn direct_external_policy_static_reason(
    config: &ExternalPoolsConfig,
    endpoint: &str,
    model: &str,
) -> Option<String> {
    if !config.external_pools_enabled || !config.external_direct_policy_enabled {
        return None;
    }
    if config
        .direct_external_model_rules
        .iter()
        .any(|rule| rule_matches(rule, model))
    {
        return Some(format!("model_rule:{}", model));
    }
    if config
        .direct_external_path_rules
        .iter()
        .any(|rule| rule_matches(rule, endpoint))
    {
        return Some(format!("path_rule:{}", endpoint));
    }
    Some(EXPLICIT_DIRECT_POLICY_REASON.to_string())
}

impl ExternalPoolManager {
    pub fn new(postgres: Arc<PostgresStore>, redis: Arc<RedisStore>) -> Self {
        Self {
            postgres,
            redis,
            client: reqwest::Client::builder()
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            capacity_notify: Arc::new(Notify::new()),
            availability_cache: Arc::new(SyncMutex::new(None)),
        }
    }

    pub async fn status(
        &self,
        config: &ExternalPoolsConfig,
    ) -> anyhow::Result<Vec<ExternalPoolStatus>> {
        let pools = self.postgres.list_external_pools(true).await?;
        let mut statuses = Vec::with_capacity(pools.len());
        for pool in pools {
            let (in_flight, global_in_flight, cooldown_remaining_secs, cooldown_reason) =
                self.pool_runtime_snapshot(pool.id).await?;
            let skipped_reason = Self::skip_reason(
                &pool,
                in_flight,
                global_in_flight,
                cooldown_remaining_secs,
                config,
            );
            statuses.push(ExternalPoolStatus {
                dispatchable: skipped_reason.is_none(),
                pool,
                in_flight,
                cooldown_remaining_secs,
                cooldown_reason,
                skipped_reason,
            });
        }
        Ok(statuses)
    }

    pub async fn has_available_pool(&self, config: &ExternalPoolsConfig) -> bool {
        self.pool_availability_snapshot(&HashSet::new(), config)
            .await
            .available_pools
            > 0
    }

    pub async fn has_eligible_pool(&self, config: &ExternalPoolsConfig) -> bool {
        self.pool_availability_snapshot(&HashSet::new(), config)
            .await
            .has_eligible_pool()
    }

    pub async fn has_eligible_pool_for_body_mode(
        &self,
        config: &ExternalPoolsConfig,
        body_mode: ExternalPoolRequestBodyMode,
    ) -> bool {
        self.scan_pool_availability_uncached(&HashSet::new(), config, false, Some(body_mode), None)
            .await
            .availability
            .has_eligible_pool()
    }

    pub async fn has_available_pool_for_body_mode(
        &self,
        config: &ExternalPoolsConfig,
        body_mode: ExternalPoolRequestBodyMode,
    ) -> bool {
        self.scan_pool_availability_uncached(&HashSet::new(), config, false, Some(body_mode), None)
            .await
            .availability
            .available_pools
            > 0
    }

    pub async fn record_local_pool_failure(
        &self,
        config: &ExternalPoolsConfig,
        credential_id: Option<u64>,
        reason: &str,
    ) -> Option<LocalPoolCircuitState> {
        if !config.local_pool_circuit_enabled {
            return None;
        }
        match self
            .redis
            .record_local_pool_circuit_failure(
                credential_id,
                reason,
                Duration::from_secs(config.local_pool_circuit_window_secs.max(1)),
                config.local_pool_circuit_open_after_failures.max(1),
                config
                    .local_pool_circuit_require_distinct_credentials
                    .max(1),
                Duration::from_secs(config.local_pool_circuit_open_secs.max(1)),
            )
            .await
        {
            Ok(state) => Some(state),
            Err(err) => {
                tracing::warn!("记录本地凭据池熔断失败状态到 Redis 失败: {}", err);
                None
            }
        }
    }

    pub async fn local_pool_circuit_state(
        &self,
        config: &ExternalPoolsConfig,
    ) -> LocalPoolCircuitState {
        if !config.local_pool_circuit_enabled {
            return LocalPoolCircuitState::default();
        }
        match self
            .redis
            .local_pool_circuit_state(Duration::from_secs(
                config.local_pool_circuit_window_secs.max(1),
            ))
            .await
        {
            Ok(state) => state,
            Err(err) => {
                tracing::warn!("读取本地凭据池 Redis 熔断状态失败: {}", err);
                LocalPoolCircuitState::default()
            }
        }
    }

    pub async fn direct_policy_reason(
        &self,
        config: &ExternalPoolsConfig,
        endpoint: &str,
        model: &str,
    ) -> Option<String> {
        let reason = direct_external_policy_static_reason(config, endpoint, model)?;
        if reason != EXPLICIT_DIRECT_POLICY_REASON {
            return Some(reason);
        }
        if config.direct_external_on_local_maintenance
            && self.local_pool_circuit_state(config).await.open
        {
            return Some("local_pool_circuit_open".to_string());
        }
        Some(reason)
    }

    pub async fn forward_with_failover(
        &self,
        config: ExternalPoolsConfig,
        route: ExternalRouteRequest,
    ) -> Response {
        let request_id = route.request_id.clone();
        match self.forward_with_failover_result(config, route).await {
            ExternalPoolForwardOutcome::Response(response) => response,
            ExternalPoolForwardOutcome::FinalError(err) => err.into_response(&request_id),
        }
    }

    pub async fn forward_with_failover_result(
        &self,
        config: ExternalPoolsConfig,
        route: ExternalRouteRequest,
    ) -> ExternalPoolForwardOutcome {
        let mut route = route;
        if !config.external_pools_enabled {
            self.record_external_failure(
                &route,
                None,
                Vec::new(),
                "external_pool_disabled",
                "request route is disabled",
                synthetic_external_error_diagnostics(
                    &route,
                    StatusCode::SERVICE_UNAVAILABLE,
                    "external_dispatch",
                ),
            );
            return ExternalPoolForwardOutcome::FinalError(ExternalPoolFinalError {
                status: StatusCode::SERVICE_UNAVAILABLE,
                response_error_type: "service_unavailable".to_string(),
                route_error_type: "external_pool_unavailable".to_string(),
                message: "request route is disabled".to_string(),
                error_id: route.error_id.clone(),
                retryable: false,
                attempts: Vec::new(),
                pool_id: None,
                pool_name: None,
            });
        }

        if let Some(max_input_tokens) = external_pool_max_input_tokens_for_route(&config, &route) {
            let message = format!(
                "prompt is too long: estimated input tokens {} exceed configured external pool maximum {}",
                route.request_input_tokens.max(0),
                max_input_tokens
            );
            tracing::warn!(
                request_id = %route.request_id,
                error_id = %route.error_id,
                request_input_tokens = route.request_input_tokens,
                external_pool_max_input_tokens = max_input_tokens,
                "external pool request rejected before dispatch because prompt is too long"
            );
            self.record_external_failure(
                &route,
                None,
                Vec::new(),
                "bad_request",
                &message,
                synthetic_external_error_diagnostics(
                    &route,
                    StatusCode::BAD_REQUEST,
                    "external_prompt_too_long_preflight",
                ),
            );
            return ExternalPoolForwardOutcome::FinalError(ExternalPoolFinalError {
                status: StatusCode::BAD_REQUEST,
                response_error_type: "invalid_request_error".to_string(),
                route_error_type: "bad_request".to_string(),
                message,
                error_id: route.error_id.clone(),
                retryable: false,
                attempts: Vec::new(),
                pool_id: None,
                pool_name: None,
            });
        }

        let payload_guard_retry_enabled = route.payload_guard_retry_config.is_some();
        let mut max_attempts = if config.external_pool_retry_max_attempts == 0 {
            None
        } else {
            Some(
                (config.external_pool_retry_max_attempts as usize)
                    .saturating_add(usize::from(payload_guard_retry_enabled)),
            )
        };

        let mut excluded = HashSet::new();
        let mut attempts = Vec::new();
        let mut last_error: Option<(ExternalPool, ExternalPoolError)> = None;
        let mut queue_guard: Option<ExternalPoolQueueGuard> = None;
        let mut wait_started_at: Option<Instant> = None;
        let mut attempt_index = 0usize;

        loop {
            if max_attempts.is_some_and(|max_attempts| attempt_index >= max_attempts) {
                break;
            }
            let selection = self
                .select_pool_with_availability_uncached(
                    &excluded,
                    &config,
                    route.body_mode_filter,
                    Some(&route.model_candidates_for_support()),
                )
                .await;
            if max_attempts.is_none() {
                max_attempts = Some(
                    selection
                        .availability
                        .default_retry_attempts(payload_guard_retry_enabled),
                );
            }
            let Some(pool) = selection.selected_pool else {
                let snapshot = selection.availability;
                if snapshot.has_temporary_unavailable_pool() {
                    match self
                        .handle_capacity_unavailable(
                            &route,
                            attempts.clone(),
                            &config,
                            snapshot.wait_reason.unwrap_or(PoolCapacityWaitReason::Full),
                            snapshot.wait_for,
                            &mut queue_guard,
                            &mut wait_started_at,
                        )
                        .await
                    {
                        ExternalCapacityDecision::Retry => continue,
                        ExternalCapacityDecision::FinalError(err) => {
                            return ExternalPoolForwardOutcome::FinalError(err);
                        }
                    }
                }
                if snapshot.available_pools > 0 {
                    continue;
                }
                break;
            };
            let pool_id = pool.id;
            let lease = match self.acquire_pool(&pool, &config).await {
                PoolAcquireResult::Acquired(lease) => lease,
                PoolAcquireResult::Unavailable(mut unavailable) => {
                    tracing::debug!(
                        pool_id,
                        reason = ?unavailable.reason,
                        detail = unavailable.detail,
                        exclude_pool_for_reselect = unavailable.exclude_pool_for_reselect,
                        "外部池选中后并发 lease 未占用，本次请求尝试重选或按配置等待"
                    );
                    if unavailable.exclude_pool_for_reselect {
                        excluded.insert(pool_id);
                        let reselection =
                            self.select_pool_for_route(&excluded, &config, &route).await;
                        if reselection.availability.coordinator_unavailable {
                            unavailable = PoolAcquireUnavailable {
                                reason: PoolCapacityWaitReason::CoordinatorUnavailable,
                                wait_for: None,
                                exclude_pool_for_reselect: false,
                                detail: "reselection_coordinator_error",
                            };
                        } else if reselection.selected_pool.is_some() {
                            continue;
                        }
                        excluded.remove(&pool_id);
                    }
                    match self
                        .handle_capacity_unavailable(
                            &route,
                            attempts.clone(),
                            &config,
                            unavailable.reason,
                            unavailable.wait_for,
                            &mut queue_guard,
                            &mut wait_started_at,
                        )
                        .await
                    {
                        ExternalCapacityDecision::Retry => continue,
                        ExternalCapacityDecision::FinalError(err) => {
                            return ExternalPoolForwardOutcome::FinalError(err);
                        }
                    }
                }
            };
            drop(queue_guard.take());
            let started = std::time::Instant::now();
            let current_attempt = attempt_index.saturating_add(1) as u32;
            attempt_index = attempt_index.saturating_add(1);
            let result = self.forward_once(&pool, &route, lease, &config).await;
            match result {
                Ok(forwarded) => {
                    attempts.push(ExternalPoolAttempt {
                        attempt: current_attempt,
                        pool_id,
                        pool_name: pool.name.clone(),
                        outbound_model: forwarded.outbound_model.clone(),
                        status: Some(forwarded.response.status().as_u16()),
                        action: "success".to_string(),
                        duration_ms: started.elapsed().as_millis() as u64,
                        error_type: None,
                        error_message: None,
                    });
                    if route.is_stream() {
                        return ExternalPoolForwardOutcome::Response(
                            self.wrap_external_stream_usage_record(
                                forwarded.response,
                                route.clone(),
                                pool,
                                attempts.clone(),
                                forwarded.stream_usage_capture,
                                forwarded.stream_usage_projection,
                            ),
                        );
                    }
                    if let Some(projection) = forwarded.stream_usage_projection.as_ref() {
                        projection.record_success();
                    }
                    self.record_external_success(
                        &route,
                        &pool,
                        attempts.clone(),
                        forwarded.billing,
                    );
                    return ExternalPoolForwardOutcome::Response(forwarded.response);
                }
                Err(forward_err) => {
                    let outbound_model = forward_err.outbound_model;
                    let err = forward_err.err;
                    let action = if err.retryable { "retry_next" } else { "fail" };
                    attempts.push(ExternalPoolAttempt {
                        attempt: current_attempt,
                        pool_id,
                        pool_name: pool.name.clone(),
                        outbound_model,
                        status: err.status.map(|status| status.as_u16()),
                        action: action.to_string(),
                        duration_ms: started.elapsed().as_millis() as u64,
                        error_type: Some(error_type_for_external_error(&err).to_string()),
                        error_message: Some(err.message.clone()),
                    });
                    if pool.request_body_mode == ExternalPoolRequestBodyMode::Normalized
                        && should_retry_external_payload_guard(&route, &err)
                    {
                        if let Some(retry_route) = external_payload_guard_retry_route(&route) {
                            if let Some(last) = attempts.last_mut() {
                                last.action = "payload_guard_retry".to_string();
                            }
                            route = retry_route;
                            excluded.clear();
                            last_error = None;
                            continue;
                        }
                    }
                    if let Some((duration, reason)) = &err.cooldown {
                        if reason == "model_mapping_miss" {
                            // Request-scoped mismatch: skip this pool for the current request, but
                            // do not cool down the pool globally for other models.
                        } else {
                            self.mark_pool_cooldown(pool_id, *duration, reason.clone())
                                .await;
                        }
                    }
                    if let Some(reason) = &err.auto_disable_reason {
                        self.auto_disable_pool_if_configured(&pool, &config, reason, &err.message)
                            .await;
                    }
                    if err.retryable {
                        excluded.insert(pool_id);
                        last_error = Some((pool, err));
                        continue;
                    }
                    let error_type = error_type_for_external_error(&err);
                    let response_error_type = anthropic_error_type_for_external_error(&err);
                    let (record_message, message_truncated) = external_error_record_message(&err);
                    let diagnostics = external_error_diagnostics(
                        &route,
                        &err,
                        response_error_type,
                        message_truncated,
                    );
                    self.record_external_failure(
                        &route,
                        Some(&pool),
                        attempts.clone(),
                        &error_type,
                        &record_message,
                        diagnostics,
                    );
                    return ExternalPoolForwardOutcome::FinalError(
                        external_final_error_from_error(
                            Some(&pool),
                            attempts,
                            &err,
                            &route.error_id,
                        ),
                    );
                }
            }
        }

        if let Some((pool, err)) = last_error {
            let error_type = error_type_for_external_error(&err);
            let response_error_type = anthropic_error_type_for_external_error(&err);
            let (record_message, message_truncated) = external_error_record_message(&err);
            let diagnostics =
                external_error_diagnostics(&route, &err, response_error_type, message_truncated);
            self.record_external_failure(
                &route,
                Some(&pool),
                attempts.clone(),
                &error_type,
                &record_message,
                diagnostics,
            );
            return ExternalPoolForwardOutcome::FinalError(external_final_error_from_error(
                Some(&pool),
                attempts,
                &err,
                &route.error_id,
            ));
        }

        self.record_external_failure(
            &route,
            None,
            attempts.clone(),
            "external_pool_unavailable",
            "No available external fallback pools",
            synthetic_external_error_diagnostics(
                &route,
                StatusCode::SERVICE_UNAVAILABLE,
                "external_dispatch",
            ),
        );
        ExternalPoolForwardOutcome::FinalError(ExternalPoolFinalError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            response_error_type: "service_unavailable".to_string(),
            route_error_type: "external_pool_unavailable".to_string(),
            message: "No available external fallback pools".to_string(),
            error_id: route.error_id.clone(),
            retryable: false,
            attempts,
            pool_id: None,
            pool_name: None,
        })
    }

    async fn forward_once(
        &self,
        pool: &ExternalPool,
        route: &ExternalRouteRequest,
        lease: ExternalPoolLease,
        config: &ExternalPoolsConfig,
    ) -> Result<ExternalForwardResponse, ExternalForwardError> {
        let url = external_pool_url(pool, &route.endpoint, config)?;
        let mut headers = forward_headers(&route.headers, pool)?;
        if !headers.contains_key(header::CONTENT_TYPE) {
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
        }
        let prepared = external_pool_prepare_request(route, pool)?;
        let outbound_model = prepared.outbound_model.clone();
        let mut request = self.client.post(url).headers(headers).body(prepared.body);
        if route.is_stream() {
            if config.external_pool_stream_request_timeout_secs > 0 {
                request = request.timeout(Duration::from_secs(
                    config.external_pool_stream_request_timeout_secs,
                ));
            }
        } else if config.external_pool_request_timeout_secs > 0 {
            request = request.timeout(Duration::from_secs(
                config.external_pool_request_timeout_secs,
            ));
        }
        let response = request.send().await.map_err(|err| {
            tracing::warn!(
                request_id = %route.request_id,
                error_id = %route.error_id,
                pool_id = pool.id,
                error = %err,
                "external pool request send failed"
            );
            ExternalForwardError::new(
                ExternalPoolError {
                    status: None,
                    message: sanitized_external_network_error("request send failed", &err),
                    retryable: true,
                    auto_disable_reason: None,
                    cooldown: Some((
                        Duration::from_secs(
                            config.external_pool_network_error_cooldown_secs.max(1),
                        ),
                        "network_error".to_string(),
                    )),
                    response_body: None,
                },
                outbound_model.clone(),
            )
        })?;

        let status = response.status();
        if !status.is_success() {
            let headers = response.headers().clone();
            let body = response.bytes().await.unwrap_or_default();
            return Err(ExternalForwardError::new(
                classify_external_error(status, body, headers, config),
                outbound_model.clone(),
            ));
        }
        let response_headers = response.headers().clone();
        let status = response.status();
        let response_is_stream =
            route.is_stream() || response_headers_look_like_sse(&response_headers);
        if response_is_stream {
            if success_response_headers_look_like_html(&response_headers) {
                return Err(ExternalForwardError::new(
                    success_protocol_error(
                        &response_headers,
                        None,
                        config,
                        "model endpoint returned an HTML response for a streaming request",
                    ),
                    outbound_model.clone(),
                ));
            }
            route.latency_trace.mark_upstream_header(route.started_at);
            let body_stream = response.bytes_stream();
            let stream_plan = ExternalStreamProcessingPlan::for_pool(pool, config);
            let projection_context = build_external_usage_projection_context(
                route,
                pool,
                config.external_pool_usage_projection_uplift_percent,
                config.external_pool_usage_projection_output_uplift_min_tokens,
                config.external_pool_usage_projection_output_uplift_percent,
            );
            let stream_idle_timeout = (config.external_pool_stream_idle_timeout_secs > 0)
                .then(|| Duration::from_secs(config.external_pool_stream_idle_timeout_secs));
            let stream_usage_projection = projection_context.clone();
            let usage_capture = Arc::new(SyncMutex::new(ExternalUsageCapture {
                stream_response_mode: Some(stream_plan.response_mode),
                ..ExternalUsageCapture::default()
            }));
            let stream_usage_capture = usage_capture.clone();
            let latency_trace = route.latency_trace.clone();
            let route_started_at = route.started_at;
            let stream_error_mask = Arc::new(ExternalStreamErrorMask {
                request_id: route.request_id.clone(),
                error_id: route.error_id.clone(),
                pool_id: pool.id,
                pool_name: pool.name.clone(),
            });
            let stream = futures::stream::unfold(
                (
                    body_stream,
                    Vec::<u8>::new(),
                    Some(lease),
                    Instant::now(),
                    Instant::now(),
                    false,
                ),
                move |(
                    mut body_stream,
                    mut buffer,
                    lease,
                    mut last_touch_at,
                    mut last_chunk_at,
                    finished,
                )| {
                    let projection_context = projection_context.clone();
                    let usage_capture = usage_capture.clone();
                    let latency_trace = latency_trace.clone();
                    let stream_error_mask = stream_error_mask.clone();
                    async move {
                        if finished {
                            return None;
                        }
                        loop {
                            tokio::select! {
                                chunk = body_stream.next() => {
                                    match chunk {
                                        Some(Ok(chunk)) => {
                                            latency_trace
                                                .mark_first_upstream_chunk(route_started_at);
                                            last_chunk_at = Instant::now();
                                            buffer.extend_from_slice(&chunk);
                                            let projected = drain_sse_events(
                                                &mut buffer,
                                                projection_context.as_ref(),
                                                Some(&usage_capture),
                                                Some(stream_error_mask.as_ref()),
                                                stream_plan,
                                            );
                                            if !projected.is_empty() {
                                                return Some((
                                                    Ok(Bytes::from(projected)),
                                                    (
                                                        body_stream,
                                                        buffer,
                                                        lease,
                                                        last_touch_at,
                                                        last_chunk_at,
                                                        false,
                                                    ),
                                                ));
                                            }
                                            if buffer.len() > EXTERNAL_POOL_MAX_SSE_EVENT_BUFFER_BYTES {
                                                tracing::warn!(
                                                    request_id = %stream_error_mask.request_id,
                                                    error_id = %stream_error_mask.error_id,
                                                    pool_id = stream_error_mask.pool_id,
                                                    pool_name = %stream_error_mask.pool_name,
                                                    buffered_bytes = buffer.len(),
                                                    max_buffered_bytes = EXTERNAL_POOL_MAX_SSE_EVENT_BUFFER_BYTES,
                                                    "external pool stream event buffer exceeded limit"
                                                );
                                                drop(lease);
                                                return Some((
                                                    Err(std::io::Error::other(
                                                        "external stream event exceeded buffer limit".to_string(),
                                                    )),
                                                    (
                                                        body_stream,
                                                        Vec::new(),
                                                        None,
                                                        last_touch_at,
                                                        last_chunk_at,
                                                        true,
                                                    ),
                                                ));
                                            }
                                        }
                                        Some(Err(err)) => {
                                            tracing::warn!(
                                                request_id = %stream_error_mask.request_id,
                                                error_id = %stream_error_mask.error_id,
                                                pool_id = stream_error_mask.pool_id,
                                                pool_name = %stream_error_mask.pool_name,
                                                error = %err,
                                                "external pool stream read failed"
                                            );
                                            drop(lease);
                                            return Some((
                                                Err(std::io::Error::other(
                                                    "external stream read error".to_string(),
                                                )),
                                                (
                                                    body_stream,
                                                    Vec::new(),
                                                    None,
                                                    last_touch_at,
                                                    last_chunk_at,
                                                    true,
                                                ),
                                            ));
                                        }
                                        None => {
                                            let tail = if buffer.is_empty() {
                                                Vec::new()
                                            } else {
                                                process_sse_event_with_plan(
                                                    &buffer,
                                                    projection_context.as_ref(),
                                                    Some(&usage_capture),
                                                    Some(stream_error_mask.as_ref()),
                                                    stream_plan,
                                                )
                                            };
                                            drop(lease);
                                            if tail.is_empty() {
                                                return None;
                                            }
                                            return Some((
                                                Ok(Bytes::from(tail)),
                                                (
                                                    body_stream,
                                                    Vec::new(),
                                                    None,
                                                    last_touch_at,
                                                    last_chunk_at,
                                                    true,
                                                ),
                                            ));
                                        }
                                    }
                                }
                                _ = tokio::time::sleep_until(external_pool_lease_touch_deadline(last_touch_at)) => {
                                    if let Some(lease) = lease.as_ref() {
                                        let _ = lease.touch();
                                    }
                                    last_touch_at = Instant::now();
                                }
                                _ = external_pool_stream_idle_deadline(last_chunk_at, stream_idle_timeout) => {
                                    drop(lease);
                                    let seconds = stream_idle_timeout
                                        .map(|timeout| timeout.as_secs())
                                        .unwrap_or_default();
                                    return Some((
                                        Err(std::io::Error::other(format!(
                                            "model endpoint stream idle timeout after {} seconds",
                                            seconds
                                        ))),
                                        (
                                            body_stream,
                                            Vec::new(),
                                            None,
                                            last_touch_at,
                                            last_chunk_at,
                                            true,
                                        ),
                                    ));
                                }
                            }
                        }
                    }
                },
            );
            let stream = Body::from_stream(stream);
            let mut builder = Response::builder().status(status);
            apply_forwarded_response_headers(&mut builder, &response_headers, &route.request_id);
            disable_proxy_buffering_for_stream_response(&mut builder);
            let response = builder.body(stream).map_err(|err| {
                ExternalForwardError::new(
                    ExternalPoolError {
                        status: None,
                        message: format!("build external stream response failed: {}", err),
                        retryable: false,
                        auto_disable_reason: None,
                        cooldown: None,
                        response_body: None,
                    },
                    outbound_model.clone(),
                )
            })?;
            Ok(ExternalForwardResponse {
                response,
                outbound_model,
                billing: None,
                stream_usage_capture: Some(stream_usage_capture),
                stream_usage_projection,
            })
        } else {
            let bytes = response.bytes().await.map_err(|err| {
                tracing::warn!(
                    request_id = %route.request_id,
                    error_id = %route.error_id,
                    pool_id = pool.id,
                    error = %err,
                    "external pool response read failed"
                );
                ExternalForwardError::new(
                    ExternalPoolError {
                        status: None,
                        message: sanitized_external_network_error("response read failed", &err),
                        retryable: true,
                        auto_disable_reason: None,
                        cooldown: Some((
                            Duration::from_secs(
                                config.external_pool_network_error_cooldown_secs.max(1),
                            ),
                            "network_error".to_string(),
                        )),
                        response_body: None,
                    },
                    outbound_model.clone(),
                )
            })?;
            if success_response_looks_like_html(&response_headers, &bytes) {
                return Err(ExternalForwardError::new(
                    success_protocol_error(
                        &response_headers,
                        Some(&bytes),
                        config,
                        "model endpoint returned an HTML response for a non-streaming request",
                    ),
                    outbound_model.clone(),
                ));
            }
            if success_response_looks_like_error_body(&bytes) {
                return Err(ExternalForwardError::new(
                    success_error_body_protocol_error(&bytes, config),
                    outbound_model.clone(),
                ));
            }
            route.latency_trace.mark_upstream_header(route.started_at);
            drop(lease);
            let projection_context = build_external_usage_projection_context(
                route,
                pool,
                config.external_pool_usage_projection_uplift_percent,
                config.external_pool_usage_projection_output_uplift_min_tokens,
                config.external_pool_usage_projection_output_uplift_percent,
            );
            let projected = maybe_project_non_stream_usage(bytes, projection_context.as_ref());
            let billing = external_pool_billing_from_capture(route, pool, projected.usage_capture);
            let mut builder = Response::builder().status(status);
            apply_forwarded_response_headers(&mut builder, &response_headers, &route.request_id);
            let response = builder.body(Body::from(projected.body)).map_err(|err| {
                ExternalForwardError::new(
                    ExternalPoolError {
                        status: None,
                        message: format!("build external response failed: {}", err),
                        retryable: false,
                        auto_disable_reason: None,
                        cooldown: None,
                        response_body: None,
                    },
                    outbound_model.clone(),
                )
            })?;
            Ok(ExternalForwardResponse {
                response,
                outbound_model,
                billing,
                stream_usage_capture: None,
                stream_usage_projection: projection_context,
            })
        }
    }

    #[cfg(test)]
    async fn select_pool(
        &self,
        excluded: &HashSet<u64>,
        config: &ExternalPoolsConfig,
    ) -> Option<ExternalPool> {
        self.scan_pool_availability_uncached(excluded, config, true, None, None)
            .await
            .selected_pool
    }

    async fn select_pool_for_route(
        &self,
        excluded: &HashSet<u64>,
        config: &ExternalPoolsConfig,
        route: &ExternalRouteRequest,
    ) -> PoolSelectionSnapshot {
        self.scan_pool_availability_uncached(
            excluded,
            config,
            true,
            route.body_mode_filter,
            Some(&route.model_candidates_for_support()),
        )
        .await
    }

    async fn select_pool_with_availability_uncached(
        &self,
        excluded: &HashSet<u64>,
        config: &ExternalPoolsConfig,
        body_mode_filter: Option<ExternalPoolRequestBodyMode>,
        model_candidates: Option<&[Option<&str>]>,
    ) -> PoolSelectionSnapshot {
        self.scan_pool_availability_uncached(
            excluded,
            config,
            true,
            body_mode_filter,
            model_candidates,
        )
        .await
    }

    async fn scan_pool_availability_uncached(
        &self,
        excluded: &HashSet<u64>,
        config: &ExternalPoolsConfig,
        include_selection: bool,
        body_mode_filter: Option<ExternalPoolRequestBodyMode>,
        model_candidates: Option<&[Option<&str>]>,
    ) -> PoolSelectionSnapshot {
        if !config.external_pools_enabled {
            return PoolSelectionSnapshot::default();
        }
        let Ok(pools) = self.postgres.list_external_pools(false).await else {
            return PoolSelectionSnapshot::default();
        };
        let mut candidates = include_selection.then(Vec::new);
        let mut availability = PoolAvailabilitySnapshot::default();
        for pool in pools {
            if excluded.contains(&pool.id) {
                continue;
            }
            if !pool.enabled || pool.is_auto_disabled_now() {
                continue;
            }
            if !external_pool_matches_body_mode_filter(&pool, body_mode_filter) {
                continue;
            }
            if !external_pool_matches_supported_models(&pool, model_candidates) {
                continue;
            }
            availability.eligible_pools += 1;
            let (in_flight, global_in_flight, cooldown_remaining_secs, _) =
                match self.pool_runtime_snapshot(pool.id).await {
                    Ok(snapshot) => snapshot,
                    Err(err) => {
                        tracing::warn!(
                            pool_id = pool.id,
                            error = %err,
                            "读取外部池 Redis 调度协调状态失败，暂停外部池准入"
                        );
                        availability.temporary_unavailable_pools += 1;
                        availability.coordinator_unavailable = true;
                        availability.wait_reason =
                            Some(PoolCapacityWaitReason::CoordinatorUnavailable);
                        continue;
                    }
                };
            match Self::skip_reason(
                &pool,
                in_flight,
                global_in_flight,
                cooldown_remaining_secs,
                config,
            )
            .as_deref()
            {
                None => {
                    availability.available_pools += 1;
                    if let Some(candidates) = candidates.as_mut() {
                        candidates.push((pool, in_flight));
                    }
                }
                Some("pool_concurrency_full" | "global_concurrency_full") => {
                    availability.temporary_unavailable_pools += 1;
                    if availability.wait_reason.is_none() {
                        availability.wait_reason = Some(PoolCapacityWaitReason::Full);
                    }
                }
                Some("cooldown") => {
                    availability.temporary_unavailable_pools += 1;
                    availability
                        .wait_reason
                        .get_or_insert(PoolCapacityWaitReason::Cooldown);
                    let wait_for = Duration::from_secs(cooldown_remaining_secs.max(1));
                    availability.wait_for = Some(
                        availability
                            .wait_for
                            .map(|existing| existing.min(wait_for))
                            .unwrap_or(wait_for),
                    );
                }
                _ => {}
            }
        }
        let selected_pool = if availability.coordinator_unavailable {
            None
        } else {
            candidates.and_then(select_external_pool_candidate)
        };
        PoolSelectionSnapshot {
            selected_pool,
            availability,
        }
    }

    async fn pool_availability_snapshot(
        &self,
        excluded: &HashSet<u64>,
        config: &ExternalPoolsConfig,
    ) -> PoolAvailabilitySnapshot {
        self.pool_availability_snapshot_inner(excluded, config, true)
            .await
    }

    async fn pool_availability_snapshot_inner(
        &self,
        excluded: &HashSet<u64>,
        config: &ExternalPoolsConfig,
        allow_cache: bool,
    ) -> PoolAvailabilitySnapshot {
        if !config.external_pools_enabled {
            return PoolAvailabilitySnapshot::default();
        }
        let cacheable = allow_cache && excluded.is_empty();
        let now = Instant::now();
        if cacheable {
            if let Some(snapshot) = self
                .availability_cache
                .lock()
                .as_ref()
                .filter(|cached| cached.expires_at > now)
                .map(|cached| cached.snapshot.clone())
            {
                return snapshot;
            }
        }

        let snapshot = self
            .scan_pool_availability_uncached(excluded, config, false, None, None)
            .await
            .availability;
        if cacheable {
            *self.availability_cache.lock() = Some(CachedPoolAvailabilitySnapshot {
                snapshot: snapshot.clone(),
                expires_at: Instant::now() + EXTERNAL_POOL_AVAILABILITY_CACHE_TTL,
            });
        }
        snapshot
    }

    async fn handle_capacity_unavailable(
        &self,
        route: &ExternalRouteRequest,
        attempts: Vec<ExternalPoolAttempt>,
        config: &ExternalPoolsConfig,
        reason: PoolCapacityWaitReason,
        wait_for: Option<Duration>,
        queue_guard: &mut Option<ExternalPoolQueueGuard>,
        wait_started_at: &mut Option<Instant>,
    ) -> ExternalCapacityDecision {
        if reason == PoolCapacityWaitReason::CoordinatorUnavailable
            || config.external_pool_capacity_mode != ExternalPoolCapacityMode::Wait
        {
            let (error_type, message) = external_capacity_error(reason);
            self.record_external_failure(
                route,
                None,
                attempts,
                error_type,
                message,
                synthetic_external_error_diagnostics(
                    route,
                    StatusCode::SERVICE_UNAVAILABLE,
                    "external_dispatch",
                ),
            );
            return ExternalCapacityDecision::FinalError(external_capacity_final_error(
                StatusCode::SERVICE_UNAVAILABLE,
                error_type,
                message,
                &route.error_id,
            ));
        }

        let started = *wait_started_at.get_or_insert_with(Instant::now);
        let max_wait = if config.external_pool_dispatch_max_wait_secs == 0 {
            None
        } else {
            Some(Duration::from_secs(
                config.external_pool_dispatch_max_wait_secs,
            ))
        };
        if let Some(max_wait) = max_wait {
            if started.elapsed() >= max_wait {
                let message = format!(
                    "Request capacity wait timed out after {} seconds",
                    max_wait.as_secs()
                );
                self.record_external_failure(
                    route,
                    None,
                    attempts,
                    "external_pool_wait_timeout",
                    &message,
                    synthetic_external_error_diagnostics(
                        route,
                        StatusCode::SERVICE_UNAVAILABLE,
                        "external_dispatch",
                    ),
                );
                return ExternalCapacityDecision::FinalError(external_capacity_final_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "external_pool_wait_timeout",
                    message,
                    &route.error_id,
                ));
            }
        }

        if queue_guard.is_none() {
            match self
                .enter_external_pool_queue(config.external_pool_max_queued_requests)
                .await
            {
                Ok(Some(guard)) => *queue_guard = Some(guard),
                Ok(None) => {
                    let message = "Request dispatch queue is full";
                    self.record_external_failure(
                        route,
                        None,
                        attempts,
                        "external_pool_queue_full",
                        message,
                        synthetic_external_error_diagnostics(
                            route,
                            StatusCode::SERVICE_UNAVAILABLE,
                            "external_dispatch",
                        ),
                    );
                    return ExternalCapacityDecision::FinalError(external_capacity_final_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "external_pool_queue_full",
                        message,
                        &route.error_id,
                    ));
                }
                Err(err) => {
                    tracing::warn!(error = %err, "占用外部池 Redis 调度排队 lease 失败");
                    let message =
                        "Request dispatch queue unavailable: Redis coordination unavailable";
                    self.record_external_failure(
                        route,
                        None,
                        attempts,
                        "external_pool_queue_error",
                        message,
                        synthetic_external_error_diagnostics(
                            route,
                            StatusCode::SERVICE_UNAVAILABLE,
                            "external_dispatch",
                        ),
                    );
                    return ExternalCapacityDecision::FinalError(external_capacity_final_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "external_pool_queue_error",
                        message,
                        &route.error_id,
                    ));
                }
            }
        }
        if let Some(guard) = queue_guard.as_mut()
            && let Err(err) = guard.renew_if_needed().await
        {
            tracing::warn!(error = %err, "续期外部池 Redis 调度排队 lease 失败");
            let message = "Request dispatch queue unavailable: Redis coordination unavailable";
            self.record_external_failure(
                route,
                None,
                attempts,
                "external_pool_queue_error",
                message,
                synthetic_external_error_diagnostics(
                    route,
                    StatusCode::SERVICE_UNAVAILABLE,
                    "external_dispatch",
                ),
            );
            return ExternalCapacityDecision::FinalError(external_capacity_final_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "external_pool_queue_error",
                message,
                &route.error_id,
            ));
        }

        let mut wakeup = wait_for
            .unwrap_or_else(|| Duration::from_secs(1))
            .min(Duration::from_secs(1));
        if let Some(max_wait) = max_wait {
            let remaining = max_wait.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                let message = format!(
                    "Request capacity wait timed out after {} seconds",
                    max_wait.as_secs()
                );
                self.record_external_failure(
                    route,
                    None,
                    attempts,
                    "external_pool_wait_timeout",
                    &message,
                    synthetic_external_error_diagnostics(
                        route,
                        StatusCode::SERVICE_UNAVAILABLE,
                        "external_dispatch",
                    ),
                );
                return ExternalCapacityDecision::FinalError(external_capacity_final_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "external_pool_wait_timeout",
                    message,
                    &route.error_id,
                ));
            }
            wakeup = wakeup.min(remaining);
        }
        if !wakeup.is_zero() {
            let _ = timeout(wakeup, self.capacity_notify.notified()).await;
        }
        ExternalCapacityDecision::Retry
    }

    fn skip_reason(
        pool: &ExternalPool,
        in_flight: u32,
        global_in_flight: u32,
        cooldown_remaining_secs: u64,
        config: &ExternalPoolsConfig,
    ) -> Option<String> {
        if !config.external_pools_enabled {
            return Some("external_pools_disabled".to_string());
        }
        if !pool.enabled {
            return Some("disabled".to_string());
        }
        if pool.is_auto_disabled_now() {
            return Some("auto_disabled".to_string());
        }
        if cooldown_remaining_secs > 0 {
            return Some("cooldown".to_string());
        }
        let per_pool_max = pool.max_concurrent_requests.max(1);
        if in_flight >= per_pool_max {
            return Some("pool_concurrency_full".to_string());
        }
        let global_max = config.external_pool_global_max_concurrent_requests;
        if global_max > 0 && global_in_flight >= global_max {
            return Some("global_concurrency_full".to_string());
        }
        None
    }

    async fn acquire_pool(
        &self,
        pool: &ExternalPool,
        config: &ExternalPoolsConfig,
    ) -> PoolAcquireResult {
        let (_, _, cooldown_remaining_secs, _) = match self.pool_runtime_snapshot(pool.id).await {
            Ok(snapshot) => snapshot,
            Err(err) => {
                tracing::warn!(
                    pool_id = pool.id,
                    error = %err,
                    "读取外部池 Redis 调度协调状态失败，拒绝占用并发槽"
                );
                return PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                    reason: PoolCapacityWaitReason::CoordinatorUnavailable,
                    wait_for: None,
                    exclude_pool_for_reselect: false,
                    detail: "capacity_state_error",
                });
            }
        };
        if cooldown_remaining_secs > 0 {
            return PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                reason: PoolCapacityWaitReason::Cooldown,
                wait_for: Some(Duration::from_secs(cooldown_remaining_secs.max(1))),
                exclude_pool_for_reselect: true,
                detail: "cooldown_before_acquire",
            });
        }
        let lease_id = match self.redis.next_external_pool_lease_id().await {
            Ok(lease_id) => lease_id,
            Err(err) => {
                tracing::warn!(pool_id = pool.id, "生成外部池 Redis lease ID 失败: {}", err);
                return PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                    reason: PoolCapacityWaitReason::CoordinatorUnavailable,
                    wait_for: None,
                    exclude_pool_for_reselect: false,
                    detail: "lease_id_error",
                });
            }
        };
        let max_age = Some(Duration::from_secs(
            DEFAULT_EXTERNAL_POOL_REQUEST_TIMEOUT_SECS.saturating_mul(2),
        ));
        match self
            .redis
            .acquire_external_pool_lease(
                pool.id,
                lease_id,
                pool.max_concurrent_requests.max(1),
                config.external_pool_global_max_concurrent_requests,
                max_age,
            )
            .await
        {
            Ok(Some(_)) => PoolAcquireResult::Acquired(ExternalPoolLease {
                manager: self.clone(),
                pool_id: pool.id,
                lease_id,
            }),
            Ok(None) => {
                let (in_flight, global_in_flight, cooldown_remaining_secs, _) =
                    match self.pool_runtime_snapshot(pool.id).await {
                        Ok(snapshot) => snapshot,
                        Err(err) => {
                            tracing::warn!(
                                pool_id = pool.id,
                                error = %err,
                                "复核外部池 Redis 调度协调状态失败，暂停外部池准入"
                            );
                            return PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                                reason: PoolCapacityWaitReason::CoordinatorUnavailable,
                                wait_for: None,
                                exclude_pool_for_reselect: false,
                                detail: "capacity_recheck_error",
                            });
                        }
                    };
                let skip_reason = Self::skip_reason(
                    pool,
                    in_flight,
                    global_in_flight,
                    cooldown_remaining_secs,
                    config,
                );
                let unavailable = match skip_reason.as_deref() {
                    Some("cooldown") => PoolAcquireUnavailable {
                        reason: PoolCapacityWaitReason::Cooldown,
                        wait_for: Some(Duration::from_secs(cooldown_remaining_secs.max(1))),
                        exclude_pool_for_reselect: true,
                        detail: "cooldown_after_acquire_race",
                    },
                    Some("global_concurrency_full") => PoolAcquireUnavailable {
                        reason: PoolCapacityWaitReason::Full,
                        wait_for: None,
                        exclude_pool_for_reselect: false,
                        detail: "global_concurrency_full_after_acquire_race",
                    },
                    Some("pool_concurrency_full") => PoolAcquireUnavailable {
                        reason: PoolCapacityWaitReason::Full,
                        wait_for: None,
                        exclude_pool_for_reselect: true,
                        detail: "pool_concurrency_full_after_acquire_race",
                    },
                    _ => PoolAcquireUnavailable {
                        reason: PoolCapacityWaitReason::Full,
                        wait_for: Some(Duration::from_secs(1)),
                        exclude_pool_for_reselect: true,
                        detail: "lease_acquire_race",
                    },
                };
                PoolAcquireResult::Unavailable(unavailable)
            }
            Err(err) => {
                tracing::warn!(pool_id = pool.id, "占用外部池 Redis 并发槽失败: {}", err);
                PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                    reason: PoolCapacityWaitReason::CoordinatorUnavailable,
                    wait_for: None,
                    exclude_pool_for_reselect: false,
                    detail: "lease_acquire_error",
                })
            }
        }
    }

    async fn enter_external_pool_queue(
        &self,
        max_queued: u32,
    ) -> anyhow::Result<Option<ExternalPoolQueueGuard>> {
        let lease_id = uuid::Uuid::new_v4().to_string();
        // Arm cleanup before awaiting Redis so cancellation and commit-unknown both remove this ID.
        let guard = ExternalPoolQueueGuard::new(self.clone(), lease_id.clone());
        let admitted = timeout(
            EXTERNAL_POOL_QUEUE_REDIS_OPERATION_TIMEOUT,
            self.redis.try_enter_external_pool_dispatch_queue(
                &lease_id,
                max_queued,
                EXTERNAL_POOL_QUEUE_LEASE_TTL_SECS,
            ),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "Redis external pool queue admission timed out after {}ms",
                EXTERNAL_POOL_QUEUE_REDIS_OPERATION_TIMEOUT.as_millis()
            )
        })??;
        if admitted {
            Ok(Some(guard))
        } else {
            guard.disarm();
            Ok(None)
        }
    }

    async fn release_pool(&self, pool_id: u64, lease_id: u64) -> anyhow::Result<()> {
        match self
            .redis
            .release_external_pool_lease(pool_id, lease_id)
            .await
        {
            Ok(true) => {}
            Ok(false) => tracing::debug!(
                pool_id,
                lease_id,
                "外部池 Redis 并发 lease 已不存在或已释放"
            ),
            Err(err) => return Err(err),
        }
        self.capacity_notify.notify_waiters();
        Ok(())
    }

    async fn touch_pool(&self, pool_id: u64, lease_id: u64) -> anyhow::Result<()> {
        let ttl_secs = DEFAULT_EXTERNAL_POOL_REQUEST_TIMEOUT_SECS
            .saturating_mul(4)
            .max(60) as usize;
        match self
            .redis
            .touch_external_pool_lease(pool_id, lease_id, ttl_secs)
            .await
        {
            Ok(true) => {}
            Ok(false) => tracing::debug!(
                pool_id,
                lease_id,
                "外部池 Redis 并发 lease touch 时已不存在"
            ),
            Err(err) => return Err(err),
        }
        Ok(())
    }

    async fn mark_pool_cooldown(&self, pool_id: u64, duration: Duration, reason: String) {
        let until = Utc::now() + chrono::Duration::from_std(duration).unwrap_or_default();
        if let Err(err) = self
            .redis
            .set_json(
                format!("external_pool:{}:cooldown", pool_id),
                &json!({ "until": until.to_rfc3339(), "reason": reason }),
                duration.as_secs().max(1) as usize,
            )
            .await
        {
            tracing::warn!(pool_id, "写入外部池 Redis cooldown 失败: {}", err);
        }
    }

    async fn pool_runtime_snapshot(
        &self,
        pool_id: u64,
    ) -> anyhow::Result<(u32, u32, u64, Option<String>)> {
        let capacity_state = self
            .redis
            .external_pool_capacity_state(
                pool_id,
                Some(Duration::from_secs(
                    DEFAULT_EXTERNAL_POOL_REQUEST_TIMEOUT_SECS.saturating_mul(2),
                )),
            )
            .await?;
        let in_flight = capacity_state.pool_in_flight_requests;
        let global_in_flight = capacity_state.global_in_flight_requests;
        let cooldown = self
            .redis
            .get_json::<ExternalPoolCooldownState>(format!("external_pool:{}:cooldown", pool_id))
            .await?;
        if let Some(cooldown) = cooldown {
            let now = Utc::now();
            if cooldown.until > now {
                return Ok((
                    in_flight,
                    global_in_flight,
                    (cooldown.until - now).num_seconds().max(1) as u64,
                    cooldown.reason,
                ));
            }
            let _ = self
                .redis
                .del(format!("external_pool:{}:cooldown", pool_id))
                .await;
        }
        Ok((in_flight, global_in_flight, 0, None))
    }

    async fn auto_disable_pool_if_configured(
        &self,
        pool: &ExternalPool,
        config: &ExternalPoolsConfig,
        reason: &str,
        last_error: &str,
    ) {
        if !pool_auto_disable_policy_enabled(pool.auto_disable_policy, config) {
            return;
        }
        if !auto_disable_reason_enabled(config, reason) {
            return;
        }
        let threshold = config.external_pool_auto_disable_failure_threshold.max(1) as u64;
        if threshold > 1 {
            let key = format!("external_pool:{}:auto_disable_failures:{}", pool.id, reason);
            let count = self
                .redis
                .incr_with_ttl(
                    key,
                    config.external_pool_auto_disable_window_secs.max(1) as usize,
                )
                .await
                .unwrap_or_else(|err| {
                    tracing::warn!(
                        pool_id = pool.id,
                        reason,
                        "记录外部池自动禁用失败计数失败: {}",
                        err
                    );
                    threshold
                });
            if count < threshold {
                return;
            }
        }
        if let Err(err) = self
            .postgres
            .auto_disable_external_pool(
                pool.id,
                reason,
                last_error,
                config.external_pool_auto_disable_duration_secs,
            )
            .await
        {
            tracing::warn!(pool_id = pool.id, "自动禁用外部池失败: {}", err);
        }
    }

    fn reset_pool_auto_disable_failure_counts(&self, pool_id: u64) {
        let redis = self.redis.clone();
        tokio::spawn(async move {
            for reason in EXTERNAL_POOL_AUTO_DISABLE_REASONS {
                let key = format!("external_pool:{}:auto_disable_failures:{}", pool_id, reason);
                if let Err(err) = redis.del(key).await {
                    tracing::warn!(pool_id, reason, "清理外部账号自动禁用失败计数失败: {}", err);
                }
            }
        });
    }

    fn record_external_success(
        &self,
        route: &ExternalRouteRequest,
        pool: &ExternalPool,
        attempts: Vec<ExternalPoolAttempt>,
        billing: Option<ExternalPoolBilling>,
    ) {
        self.reset_pool_auto_disable_failure_counts(pool.id);
        self.record_external(
            route,
            Some(pool),
            attempts,
            UsageRecordStatus::Success,
            None,
            None,
            None,
            UsageErrorDiagnostics::default(),
            billing,
        );
    }

    fn record_external_failure(
        &self,
        route: &ExternalRouteRequest,
        pool: Option<&ExternalPool>,
        attempts: Vec<ExternalPoolAttempt>,
        error_type: &str,
        error_message: &str,
        mut diagnostics: UsageErrorDiagnostics,
    ) {
        let error_detail = format!("{}: {}", error_type, error_message);
        if diagnostics.public_error.is_none() {
            let status = diagnostics
                .status_code
                .and_then(|status| StatusCode::from_u16(status).ok())
                .unwrap_or(StatusCode::BAD_GATEWAY);
            diagnostics.public_error = Some(external_public_error_from_parts(
                status,
                error_type,
                false,
                error_message,
                &route.error_id,
            ));
        }
        self.record_external(
            route,
            pool,
            attempts,
            UsageRecordStatus::Error,
            Some(error_type.to_string()),
            Some(error_message.to_string()),
            Some(error_detail),
            diagnostics,
            None,
        );
    }

    fn wrap_external_stream_usage_record(
        &self,
        response: Response,
        route: ExternalRouteRequest,
        pool: ExternalPool,
        attempts: Vec<ExternalPoolAttempt>,
        usage_capture: Option<Arc<SyncMutex<ExternalUsageCapture>>>,
        usage_projection: Option<ExternalUsageProjectionContext>,
    ) -> Response {
        let (parts, body) = response.into_parts();
        let data_stream = body.into_data_stream();
        let guard = ExternalStreamUsageGuard {
            manager: self.clone(),
            route,
            pool,
            attempts,
            usage_capture,
            usage_projection,
            chunks_before_first_output: 0,
            events_before_first_output: 0,
            completed: false,
        };
        let stream = futures::stream::unfold(
            (data_stream, Some(guard)),
            |(mut data_stream, mut guard)| async move {
                match data_stream.next().await {
                    Some(Ok(chunk)) => {
                        if let Some(guard_ref) = guard.as_mut() {
                            guard_ref.mark_first_token_if_output(&chunk);
                        }
                        Some((Ok(chunk), (data_stream, guard)))
                    }
                    Some(Err(err)) => {
                        if let Some(mut guard) = guard.take() {
                            tracing::warn!(
                                request_id = %guard.route.request_id,
                                error_id = %guard.route.error_id,
                                pool_id = guard.pool.id,
                                pool_name = %guard.pool.name,
                                error = %err,
                                "external stream response failed"
                            );
                            guard.record_stream_error("external stream response failed");
                        }
                        Some((
                            Err(std::io::Error::new(
                                std::io::ErrorKind::Other,
                                "external stream response failed".to_string(),
                            )),
                            (data_stream, None),
                        ))
                    }
                    None => {
                        if let Some(mut guard) = guard.take() {
                            guard.record_success();
                        }
                        None
                    }
                }
            },
        );
        Response::from_parts(parts, Body::from_stream(stream))
    }

    fn record_external(
        &self,
        route: &ExternalRouteRequest,
        pool: Option<&ExternalPool>,
        attempts: Vec<ExternalPoolAttempt>,
        status: UsageRecordStatus,
        error_type: Option<String>,
        error_message: Option<String>,
        error_detail: Option<String>,
        error_diagnostics: UsageErrorDiagnostics,
        billing: Option<ExternalPoolBilling>,
    ) {
        let request_input_tokens = if route.request_input_tokens > 0 {
            route.request_input_tokens
        } else {
            billing
                .as_ref()
                .and_then(|billing| billing.request_input_tokens)
                .unwrap_or(0)
        };
        let usage = billing
            .as_ref()
            .filter(|_| status == UsageRecordStatus::Success)
            .map(|billing| billing.reported_usage);
        let input_tokens = request_input_tokens;
        let compat_input_tokens = usage
            .map(|usage| usage.input_tokens)
            .unwrap_or(request_input_tokens);
        let billable_input_tokens = usage
            .map(|usage| usage.billable_input_tokens)
            .unwrap_or(request_input_tokens);
        let output_tokens = usage.map(|usage| usage.output_tokens).unwrap_or(0);
        let cache_read_input_tokens = usage
            .map(|usage| usage.cache_read_input_tokens)
            .unwrap_or(0);
        let cache_creation_input_tokens = usage
            .map(|usage| usage.cache_creation_input_tokens)
            .unwrap_or(0);
        let cache_creation_5m_input_tokens = usage
            .map(|usage| usage.cache_creation_5m_input_tokens)
            .unwrap_or(0);
        let cache_creation_1h_input_tokens = usage
            .map(|usage| usage.cache_creation_1h_input_tokens)
            .unwrap_or(0);
        let pricing_available = billing
            .as_ref()
            .filter(|_| status == UsageRecordStatus::Success)
            .is_some_and(|billing| billing.pricing_available);
        let estimated_cost_usd = billing
            .as_ref()
            .filter(|_| status == UsageRecordStatus::Success)
            .filter(|billing| billing.pricing_available)
            .map(|billing| billing.billable_cost_usd)
            .unwrap_or(0.0);
        let pricing_model = billing
            .as_ref()
            .filter(|_| status == UsageRecordStatus::Success)
            .and_then(|billing| billing.pricing_model.clone());
        let usage_source = if billing
            .as_ref()
            .is_some_and(|billing| billing.usage_projection_applied)
        {
            UsageSource::LocalPromptCache
        } else if usage.is_some() {
            UsageSource::UpstreamMetadata
        } else {
            UsageSource::RequestEstimate
        };
        let duration_ms = route.started_at.elapsed().as_millis() as u64;
        let external_outbound_model = attempts
            .iter()
            .rev()
            .find_map(|attempt| attempt.outbound_model.clone());
        route.recorder.record(UsageRecord {
            id: route.request_id.clone(),
            created_at: Utc::now().to_rfc3339(),
            endpoint: route.endpoint.to_string(),
            stream: route.is_stream(),
            model: route.requested_model(),
            requested_max_tokens: route.requested_max_tokens(),
            downstream_stop_reason: None,
            upstream_model: route.upstream_model.clone(),
            external_outbound_model,
            model_resolution_source: route.model_resolution_source.clone(),
            model_resolution_note: route.model_resolution_note.clone(),
            conversation_id: route.stable_conversation_id(),
            credential_id: None,
            credential_label: None,
            status,
            usage_source,
            raw_usage: billing
                .as_ref()
                .filter(|_| status == UsageRecordStatus::Success)
                .map(|billing| billing.raw_usage),
            total_input_tokens: input_tokens,
            compat_input_tokens,
            billable_input_tokens,
            output_tokens,
            cache_read_input_tokens,
            cache_creation_input_tokens,
            cache_creation_5m_input_tokens,
            cache_creation_1h_input_tokens,
            estimated_cost_usd,
            original_cost_usd: billing
                .as_ref()
                .filter(|_| status == UsageRecordStatus::Success)
                .filter(|billing| billing.pricing_available)
                .map(|billing| billing.raw_cost_usd)
                .unwrap_or(estimated_cost_usd),
            kiro_metering_usage: 0.0,
            pricing_available,
            pricing_model,
            duration_ms,
            first_token_latency_ms: {
                let value = route.first_token_latency_ms.load(Ordering::Acquire);
                (value > 0).then_some(value)
            },
            response_latency_ms: Some(duration_ms),
            latency_trace: route.latency_trace.snapshot(),
            simulated: usage_source.is_simulated(),
            sticky_bound: false,
            fallback_from_sticky: false,
            credential_attempts: route.local_attempts.clone(),
            route_kind: Some(UsageRouteKind::ExternalPool),
            route_subtype: Some(route.route_subtype),
            fallback_reason: route.fallback_reason.clone(),
            direct_policy_reason: route.direct_policy_reason.clone(),
            local_attempted: Some(route.local_attempted),
            local_preflight: route.local_preflight.clone(),
            external_pool_id: pool.map(|pool| pool.id),
            external_pool_name: pool.map(|pool| pool.name.clone()),
            external_attempts: attempts,
            usage_projection_applied: billing
                .as_ref()
                .map(|billing| billing.usage_projection_applied),
            external_pool_billing: billing,
            error_type,
            error_message,
            error_detail,
            error_status_code: error_diagnostics.status_code,
            error_source: error_diagnostics.source,
            error_id: error_diagnostics.error_id,
            error_metadata: error_diagnostics.metadata,
            public_error_status_code: error_diagnostics
                .public_error
                .as_ref()
                .map(|error| error.status_code),
            public_error_type: error_diagnostics
                .public_error
                .as_ref()
                .map(|error| error.error_type.clone()),
            public_error_message: error_diagnostics.public_error.map(|error| error.message),
            payload_breakdown: route
                .payload_breakdown
                .as_ref()
                .and_then(|breakdown| serde_json::to_value(breakdown).ok()),
            payload_guard_report: route
                .payload_guard_report
                .as_ref()
                .and_then(|report| serde_json::to_value(report).ok()),
        });
    }
}

struct ExternalStreamUsageGuard {
    manager: ExternalPoolManager,
    route: ExternalRouteRequest,
    pool: ExternalPool,
    attempts: Vec<ExternalPoolAttempt>,
    usage_capture: Option<Arc<SyncMutex<ExternalUsageCapture>>>,
    usage_projection: Option<ExternalUsageProjectionContext>,
    chunks_before_first_output: u32,
    events_before_first_output: u32,
    completed: bool,
}

impl ExternalStreamUsageGuard {
    fn mark_first_token_if_output(&mut self, chunk: &Bytes) {
        if self.route.first_token_latency_ms.load(Ordering::Relaxed) > 0 {
            return;
        }
        if !external_stream_chunk_has_first_output(chunk) {
            self.chunks_before_first_output = self.chunks_before_first_output.saturating_add(1);
            self.events_before_first_output = self
                .events_before_first_output
                .saturating_add(count_external_stream_events(chunk));
            return;
        }
        let elapsed = self.route.started_at.elapsed().as_millis().max(1) as u64;
        if self
            .route
            .first_token_latency_ms
            .compare_exchange(0, elapsed, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            let events_before = self
                .events_before_first_output
                .saturating_add(count_external_stream_events_before_first_output(chunk));
            self.route.latency_trace.mark_first_output(
                elapsed,
                self.chunks_before_first_output,
                events_before,
            );
        }
    }

    fn record_success(&mut self) {
        if self.completed {
            return;
        }
        let stream_error_message = self
            .usage_capture
            .as_ref()
            .and_then(|capture| capture.lock().stream_error_message.clone());
        if let Some(message) = stream_error_message {
            self.manager.record_external(
                &self.route,
                Some(&self.pool),
                self.attempts.clone(),
                UsageRecordStatus::StreamError,
                Some("stream_error".to_string()),
                Some(message.clone()),
                Some(format!("stream_error: {}", message)),
                UsageErrorDiagnostics {
                    status_code: Some(StatusCode::OK.as_u16()),
                    source: Some("external_account_stream".to_string()),
                    error_id: Some(self.route.error_id.clone()),
                    metadata: Some(json!({ "streamErrorEvent": true })),
                    public_error: Some(external_stream_public_error(
                        "api_error",
                        &self.route.error_id,
                    )),
                },
                None,
            );
            self.completed = true;
            return;
        }
        let billing = self.usage_capture.as_ref().and_then(|capture| {
            external_pool_billing_from_capture_ref(&self.route, &self.pool, capture)
        });
        if let Some(projection) = self.usage_projection.as_ref() {
            projection.record_success();
        }
        self.manager.record_external_success(
            &self.route,
            &self.pool,
            self.attempts.clone(),
            billing,
        );
        self.completed = true;
    }

    fn record_stream_error(&mut self, message: &str) {
        if self.completed {
            return;
        }
        self.manager.record_external(
            &self.route,
            Some(&self.pool),
            self.attempts.clone(),
            UsageRecordStatus::StreamError,
            Some("stream_error".to_string()),
            Some(message.to_string()),
            Some(format!("stream_error: {}", message)),
            UsageErrorDiagnostics {
                status_code: None,
                source: Some("external_stream".to_string()),
                error_id: Some(self.route.error_id.clone()),
                metadata: Some(json!({ "streamTransportError": true })),
                public_error: Some(external_stream_public_error(
                    "api_error",
                    &self.route.error_id,
                )),
            },
            None,
        );
        self.completed = true;
    }

    fn record_client_dropped(&mut self) {
        if self.completed {
            return;
        }
        let message = "external stream body dropped before completion";
        self.manager.record_external(
            &self.route,
            Some(&self.pool),
            self.attempts.clone(),
            UsageRecordStatus::ClientDropped,
            Some("client_dropped".to_string()),
            Some(message.to_string()),
            Some(format!("client_dropped: {}", message)),
            UsageErrorDiagnostics {
                status_code: None,
                source: Some("downstream_client".to_string()),
                error_id: Some(self.route.error_id.clone()),
                metadata: None,
                public_error: None,
            },
            None,
        );
        self.completed = true;
    }
}

fn external_stream_chunk_has_first_output(chunk: &Bytes) -> bool {
    external_stream_first_output_index(chunk).is_some()
}

fn external_stream_first_output_index(chunk: &Bytes) -> Option<usize> {
    let bytes = chunk.as_ref();
    let mut offset = 0usize;
    while let Some((idx, delimiter_len)) = find_sse_event_delimiter(&bytes[offset..]) {
        let event_end = offset + idx + delimiter_len;
        if std::str::from_utf8(&bytes[offset..event_end])
            .ok()
            .is_some_and(external_sse_event_has_first_output)
        {
            return Some(offset);
        }
        offset = event_end;
    }
    None
}

fn count_external_stream_events(chunk: &Bytes) -> u32 {
    count_complete_sse_events(chunk.as_ref())
}

fn count_external_stream_events_before_first_output(chunk: &Bytes) -> u32 {
    let Some(index) = external_stream_first_output_index(chunk) else {
        return count_external_stream_events(chunk);
    };
    count_complete_sse_events(&chunk.as_ref()[..index])
}

fn count_complete_sse_events(bytes: &[u8]) -> u32 {
    let mut count = 0u32;
    let mut offset = 0usize;
    while let Some((idx, delimiter_len)) = find_sse_event_delimiter(&bytes[offset..]) {
        count = count.saturating_add(1);
        offset = offset.saturating_add(idx).saturating_add(delimiter_len);
    }
    count
}

fn external_sse_event_has_first_output(event: &str) -> bool {
    for line in event.lines() {
        let Some(data) = line.trim_end_matches('\r').strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        if serde_json::from_str::<serde_json::Value>(data)
            .ok()
            .is_some_and(|value| external_sse_data_has_first_output(&value))
        {
            return true;
        }
    }
    false
}

fn external_sse_data_has_first_output(value: &serde_json::Value) -> bool {
    match value.get("type").and_then(|value| value.as_str()) {
        Some("content_block_delta") => {
            let Some(delta) = value.get("delta").and_then(|value| value.as_object()) else {
                return false;
            };
            match delta.get("type").and_then(|value| value.as_str()) {
                Some("text_delta") => delta
                    .get("text")
                    .and_then(|value| value.as_str())
                    .is_some_and(|text| !text.is_empty()),
                Some("thinking_delta") => delta
                    .get("thinking")
                    .and_then(|value| value.as_str())
                    .is_some_and(|thinking| !thinking.is_empty()),
                Some("input_json_delta") => delta
                    .get("partial_json")
                    .and_then(|value| value.as_str())
                    .is_some_and(|partial_json| !partial_json.is_empty()),
                _ => false,
            }
        }
        Some("content_block_start") => value
            .get("content_block")
            .and_then(|value| value.get("type"))
            .and_then(|value| value.as_str())
            .is_some_and(|block_type| {
                matches!(
                    block_type,
                    "tool_use" | "server_tool_use" | "redacted_thinking"
                )
            }),
        _ => false,
    }
}

impl Drop for ExternalStreamUsageGuard {
    fn drop(&mut self) {
        self.record_client_dropped();
    }
}

fn pool_auto_disable_policy_enabled(
    policy: ExternalPoolAutoDisablePolicy,
    config: &ExternalPoolsConfig,
) -> bool {
    match policy {
        ExternalPoolAutoDisablePolicy::Disabled => false,
        ExternalPoolAutoDisablePolicy::Enabled => true,
        ExternalPoolAutoDisablePolicy::Inherit => config.external_pool_auto_disable_enabled,
    }
}

fn rule_matches(rule: &str, value: &str) -> bool {
    let rule = rule.trim();
    if rule.is_empty() {
        return false;
    }
    if rule == "*" {
        return true;
    }
    value.eq_ignore_ascii_case(rule)
        || value
            .to_ascii_lowercase()
            .contains(&rule.to_ascii_lowercase())
}

fn load_score(in_flight: u32, max: u32) -> u64 {
    let max = max.max(1) as u64;
    ((in_flight as u64) * 1_000_000) / max
}

fn select_external_pool_candidate(
    mut candidates: Vec<(ExternalPool, u32)>,
) -> Option<ExternalPool> {
    candidates.sort_by(|(a, a_in_flight), (b, b_in_flight)| {
        let a_load = load_score(*a_in_flight, a.max_concurrent_requests);
        let b_load = load_score(*b_in_flight, b.max_concurrent_requests);
        a.priority
            .cmp(&b.priority)
            .then_with(|| a_load.cmp(&b_load))
            .then_with(|| a.id.cmp(&b.id))
    });
    let (best_priority, best_load) = candidates.first().map(|(pool, in_flight)| {
        (
            pool.priority,
            load_score(*in_flight, pool.max_concurrent_requests),
        )
    })?;
    let best: Vec<_> = candidates
        .into_iter()
        .filter(|(pool, in_flight)| {
            pool.priority == best_priority
                && load_score(*in_flight, pool.max_concurrent_requests) == best_load
        })
        .collect();
    if best.is_empty() {
        return None;
    }
    let idx = fastrand::usize(..best.len());
    best.into_iter().nth(idx).map(|(pool, _)| pool)
}

fn external_pool_lease_touch_deadline(last_touch_at: Instant) -> Instant {
    last_touch_at + Duration::from_secs(EXTERNAL_POOL_LEASE_TOUCH_INTERVAL_SECS)
}

async fn external_pool_stream_idle_deadline(
    last_chunk_at: Instant,
    idle_timeout: Option<Duration>,
) {
    if let Some(idle_timeout) = idle_timeout {
        tokio::time::sleep_until(last_chunk_at + idle_timeout).await;
    } else {
        std::future::pending::<()>().await;
    }
}

pub(crate) fn external_pool_models_url(base_url: &str) -> Result<Url, url::ParseError> {
    let mut base = base_url.trim().trim_end_matches('/').to_string();
    if base_url_ends_with_v1(&base) {
        base.push_str("/models");
    } else {
        base.push_str("/v1/models");
    }
    Url::parse(&base)
}

pub(crate) fn external_pool_messages_url(base_url: &str) -> Result<Url, url::ParseError> {
    let mut base = base_url.trim().trim_end_matches('/').to_string();
    if base_url_ends_with_v1(&base) {
        base.push_str("/messages");
    } else {
        base.push_str("/v1/messages");
    }
    Url::parse(&base)
}

fn external_pool_url(
    pool: &ExternalPool,
    _endpoint: &str,
    config: &ExternalPoolsConfig,
) -> Result<Url, ExternalPoolError> {
    external_pool_messages_url(&pool.base_url).map_err(|err| ExternalPoolError {
        status: None,
        message: format!("model endpoint URL is invalid: {}", err),
        retryable: true,
        auto_disable_reason: Some("misconfigured_endpoint".to_string()),
        cooldown: Some((
            Duration::from_secs(config.external_pool_protocol_error_cooldown_secs.max(1)),
            "misconfigured_endpoint".to_string(),
        )),
        response_body: None,
    })
}

fn base_url_ends_with_v1(base: &str) -> bool {
    Url::parse(base)
        .ok()
        .map(|url| {
            url.path()
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .is_some_and(|segment| segment.eq_ignore_ascii_case("v1"))
        })
        .unwrap_or_else(|| {
            base.trim_end_matches('/')
                .rsplit('/')
                .next()
                .is_some_and(|segment| segment.eq_ignore_ascii_case("v1"))
        })
}

fn forward_headers(
    headers: &HeaderMap,
    pool: &ExternalPool,
) -> Result<HeaderMap, ExternalPoolError> {
    let mut out = HeaderMap::new();
    for (name, value) in headers {
        if should_forward_header(name) {
            out.insert(name.clone(), value.clone());
        }
    }
    match pool.auth_type {
        ExternalPoolAuthType::Bearer => {
            let key = pool.api_key.as_deref().unwrap_or_default();
            let value = HeaderValue::from_str(&format!("Bearer {}", key)).map_err(|err| {
                ExternalPoolError {
                    status: None,
                    message: format!("model endpoint auth header invalid: {}", err),
                    retryable: true,
                    auto_disable_reason: Some("auth_error".to_string()),
                    cooldown: Some((Duration::from_secs(10), "auth_error".to_string())),
                    response_body: None,
                }
            })?;
            out.insert(header::AUTHORIZATION, value);
        }
        ExternalPoolAuthType::XApiKey => {
            let key = pool.api_key.as_deref().unwrap_or_default();
            let value = HeaderValue::from_str(key).map_err(|err| ExternalPoolError {
                status: None,
                message: format!("model endpoint x-api-key invalid: {}", err),
                retryable: true,
                auto_disable_reason: Some("auth_error".to_string()),
                cooldown: Some((Duration::from_secs(10), "auth_error".to_string())),
                response_body: None,
            })?;
            out.insert(HeaderName::from_static("x-api-key"), value);
        }
    }
    Ok(out)
}

#[cfg(test)]
fn external_pool_outbound_body(
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
) -> Result<Bytes, ExternalPoolError> {
    external_pool_prepare_request(route, pool).map(|prepared| prepared.body)
}

fn external_pool_matches_body_mode_filter(
    pool: &ExternalPool,
    filter: Option<ExternalPoolRequestBodyMode>,
) -> bool {
    filter.is_none_or(|mode| pool.request_body_mode == mode)
}

fn external_pool_matches_supported_models(
    pool: &ExternalPool,
    model_candidates: Option<&[Option<&str>]>,
) -> bool {
    if pool.supported_models.is_empty() {
        return true;
    }
    let Some(model_candidates) = model_candidates else {
        return false;
    };
    model_is_supported_by_list(&pool.supported_models, model_candidates)
}

fn external_pool_prepare_request(
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
) -> Result<body_pipeline::PreparedExternalRequest, ExternalPoolError> {
    body_pipeline::prepare_request(route, pool)
}

pub fn normalize_external_pool_model_mapping_rules(
    rules: Vec<ModelMappingRule>,
) -> Vec<ModelMappingRule> {
    model_pipeline::normalize_mapping_rules(rules)
}

#[cfg(test)]
fn normalize_external_pool_outbound_model(model: &str) -> String {
    model_pipeline::normalize_outbound_model(model)
}

fn normalize_external_pool_thinking_value(value: &mut serde_json::Value) -> bool {
    let Some(thinking) = value
        .get_mut("thinking")
        .and_then(|value| value.as_object_mut())
    else {
        return false;
    };
    let thinking_type = thinking
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(thinking_type.as_str(), "adaptive" | "disabled") {
        return false;
    }
    thinking.remove("budget_tokens").is_some()
}

fn should_forward_header(name: &HeaderName) -> bool {
    let lower = name.as_str().to_ascii_lowercase();
    !matches!(
        lower.as_str(),
        "host"
            | "connection"
            | "content-length"
            | "transfer-encoding"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "upgrade"
            | "authorization"
            | "x-api-key"
    )
}

fn should_forward_response_header(name: &HeaderName) -> bool {
    let lower = name.as_str().to_ascii_lowercase();
    !matches!(
        lower.as_str(),
        "connection"
            | "content-length"
            | "transfer-encoding"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "upgrade"
    )
}

fn success_response_headers_look_like_html(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_ascii_lowercase().contains("text/html"))
        .unwrap_or(false)
}

fn response_headers_look_like_sse(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .to_ascii_lowercase()
                .split(';')
                .next()
                .is_some_and(|content_type| content_type.trim() == "text/event-stream")
        })
        .unwrap_or(false)
}

fn success_response_looks_like_html(headers: &HeaderMap, body: &[u8]) -> bool {
    if success_response_headers_look_like_html(headers) {
        return true;
    }
    let trimmed = body
        .iter()
        .copied()
        .skip_while(|byte| byte.is_ascii_whitespace())
        .take(64)
        .collect::<Vec<_>>();
    let prefix = String::from_utf8_lossy(&trimmed).to_ascii_lowercase();
    prefix.starts_with("<!doctype html") || prefix.starts_with("<html")
}

fn success_response_looks_like_error_body(body: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    value
        .get("type")
        .and_then(|value| value.as_str())
        .is_some_and(|value| value == "error")
        || value.get("error").is_some_and(|error| {
            error.is_object()
                && (error.get("message").is_some()
                    || error
                        .get("type")
                        .and_then(|value| value.as_str())
                        .is_some_and(|value| value.ends_with("_error")))
        })
}

fn sanitized_external_network_error(context: &str, err: &reqwest::Error) -> String {
    let kind = if err.is_timeout() {
        "timeout"
    } else if err.is_connect() {
        "connection error"
    } else if err.is_body() {
        "body error"
    } else if err.is_decode() {
        "decode error"
    } else if err.is_request() {
        "request error"
    } else {
        "network error"
    };
    format!("{context}: {kind}")
}

fn success_error_body_protocol_error(
    body: &Bytes,
    config: &ExternalPoolsConfig,
) -> ExternalPoolError {
    ExternalPoolError {
        status: Some(StatusCode::OK),
        message: "model endpoint returned an error envelope with a success status".to_string(),
        retryable: true,
        auto_disable_reason: None,
        cooldown: Some((
            Duration::from_secs(config.external_pool_server_error_cooldown_secs.max(1)),
            "server_error".to_string(),
        )),
        response_body: Some(body.clone()),
    }
}

fn success_protocol_error(
    headers: &HeaderMap,
    body: Option<&Bytes>,
    config: &ExternalPoolsConfig,
    context: &str,
) -> ExternalPoolError {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown");
    let body_prefix = body
        .map(|bytes| {
            String::from_utf8_lossy(&bytes[..bytes.len().min(120)])
                .replace('\n', " ")
                .replace('\r', " ")
        })
        .unwrap_or_default();
    let message = if body_prefix.is_empty() {
        format!("{context}; content-type={content_type}")
    } else {
        format!("{context}; content-type={content_type}; body_prefix={body_prefix}")
    };
    ExternalPoolError {
        status: Some(StatusCode::OK),
        message,
        retryable: true,
        auto_disable_reason: Some("misconfigured_endpoint".to_string()),
        cooldown: Some((
            Duration::from_secs(config.external_pool_protocol_error_cooldown_secs.max(1)),
            "misconfigured_endpoint".to_string(),
        )),
        response_body: None,
    }
}

fn apply_forwarded_response_headers(
    builder: &mut axum::http::response::Builder,
    headers: &HeaderMap,
    request_id: &str,
) {
    let Some(out) = builder.headers_mut() else {
        return;
    };
    for (name, value) in headers {
        if should_forward_response_header(name) {
            out.insert(name.clone(), value.clone());
        }
    }
    envelope::insert_request_id_headers(out, request_id);
}

fn disable_proxy_buffering_for_stream_response(builder: &mut axum::http::response::Builder) {
    let Some(out) = builder.headers_mut() else {
        return;
    };
    out.insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
}

fn external_final_error_from_error(
    pool: Option<&ExternalPool>,
    attempts: Vec<ExternalPoolAttempt>,
    err: &ExternalPoolError,
    error_id: &str,
) -> ExternalPoolFinalError {
    let message = err
        .response_body
        .as_ref()
        .map(|body| String::from_utf8_lossy(body).to_string())
        .filter(|message| !message.trim().is_empty())
        .unwrap_or_else(|| err.message.clone());
    ExternalPoolFinalError {
        status: err.status.unwrap_or(StatusCode::BAD_GATEWAY),
        response_error_type: anthropic_error_type_for_external_error(err).to_string(),
        route_error_type: error_type_for_external_error(err),
        message,
        error_id: error_id.to_string(),
        retryable: err.retryable,
        attempts,
        pool_id: pool.map(|pool| pool.id),
        pool_name: pool.map(|pool| pool.name.clone()),
    }
}

fn external_public_error_from_parts(
    status: StatusCode,
    route_error_type: &str,
    retryable: bool,
    message: &str,
    error_id: &str,
) -> UsagePublicError {
    let final_error = ExternalPoolFinalError {
        status,
        response_error_type: "api_error".to_string(),
        route_error_type: route_error_type.to_string(),
        message: message.to_string(),
        error_id: error_id.to_string(),
        retryable,
        attempts: Vec::new(),
        pool_id: None,
        pool_name: None,
    };
    final_error.public_error()
}

fn external_stream_public_error(error_type: &str, error_id: &str) -> UsagePublicError {
    let message = match error_type {
        "invalid_request_error" => envelope::PUBLIC_INVALID_REQUEST_MESSAGE,
        "rate_limit_error" => envelope::PUBLIC_RATE_LIMIT_MESSAGE,
        _ => envelope::PUBLIC_TEMPORARY_FAILURE_MESSAGE,
    };
    UsagePublicError {
        status_code: StatusCode::OK.as_u16(),
        error_type: error_type.to_string(),
        message: envelope::public_message_with_error_id(message, error_id),
    }
}

fn external_capacity_error(reason: PoolCapacityWaitReason) -> (&'static str, &'static str) {
    match reason {
        PoolCapacityWaitReason::Full => ("external_pool_capacity_full", "Request capacity is full"),
        PoolCapacityWaitReason::Cooldown => (
            "external_pool_cooldown",
            "Request capacity is temporarily cooling down",
        ),
        PoolCapacityWaitReason::CoordinatorUnavailable => (
            "external_pool_coordinator_unavailable",
            "Request capacity coordinator is temporarily unavailable",
        ),
    }
}

fn external_capacity_final_error(
    status: StatusCode,
    code: &str,
    message: impl Into<String>,
    error_id: &str,
) -> ExternalPoolFinalError {
    ExternalPoolFinalError {
        status,
        response_error_type: code.to_string(),
        route_error_type: code.to_string(),
        message: message.into(),
        error_id: error_id.to_string(),
        retryable: true,
        attempts: Vec::new(),
        pool_id: None,
        pool_name: None,
    }
}

fn compact_usage_error_message(message: &str) -> (String, bool) {
    if message.len() <= MAX_RECORDED_EXTERNAL_ERROR_MESSAGE_BYTES {
        return (message.to_string(), false);
    }
    let mut out = String::with_capacity(MAX_RECORDED_EXTERNAL_ERROR_MESSAGE_BYTES + 3);
    for ch in message.chars() {
        if out.len() + ch.len_utf8() > MAX_RECORDED_EXTERNAL_ERROR_MESSAGE_BYTES {
            out.push_str("...");
            return (out, true);
        }
        out.push(ch);
    }
    (out, false)
}

fn external_error_record_message(err: &ExternalPoolError) -> (String, bool) {
    let message = err
        .response_body
        .as_ref()
        .map(|body| String::from_utf8_lossy(body).to_string())
        .filter(|message| !message.trim().is_empty())
        .unwrap_or_else(|| err.message.clone());
    compact_usage_error_message(&message)
}

fn metadata_insert(
    map: &mut serde_json::Map<String, serde_json::Value>,
    key: &'static str,
    value: impl Into<serde_json::Value>,
) {
    map.insert(key.to_string(), value.into());
}

fn external_error_diagnostics(
    route: &ExternalRouteRequest,
    err: &ExternalPoolError,
    response_error_type: &str,
    message_truncated: bool,
) -> UsageErrorDiagnostics {
    let mut metadata = serde_json::Map::new();
    metadata_insert(&mut metadata, "responseErrorType", response_error_type);
    metadata_insert(&mut metadata, "retryable", err.retryable);
    if let Some(reason) = err.auto_disable_reason.as_deref() {
        metadata_insert(&mut metadata, "autoDisableReason", reason);
    }
    if let Some((duration, reason)) = err.cooldown.as_ref() {
        metadata_insert(&mut metadata, "cooldownReason", reason.as_str());
        if !duration.is_zero() {
            metadata_insert(
                &mut metadata,
                "cooldownMs",
                serde_json::Value::from(duration.as_millis() as u64),
            );
        }
    }
    if message_truncated {
        metadata_insert(&mut metadata, "messageTruncated", true);
    }
    if err.status == Some(StatusCode::OK)
        && err
            .response_body
            .as_ref()
            .is_some_and(|body| success_response_looks_like_error_body(body))
    {
        metadata_insert(&mut metadata, "protocolError", "success_error_envelope");
    } else if err
        .auto_disable_reason
        .as_deref()
        .is_some_and(|reason| reason == "misconfigured_endpoint")
    {
        metadata_insert(&mut metadata, "protocolError", "unexpected_response_shape");
    }

    let route_error_type = error_type_for_external_error(err);
    let public_error = Some(external_public_error_from_parts(
        err.status.unwrap_or(StatusCode::BAD_GATEWAY),
        &route_error_type,
        err.retryable,
        &err.message,
        &route.error_id,
    ));

    UsageErrorDiagnostics {
        status_code: err.status.map(|status| status.as_u16()),
        source: Some("external_account".to_string()),
        error_id: Some(route.error_id.clone()),
        metadata: (!metadata.is_empty()).then_some(serde_json::Value::Object(metadata)),
        public_error,
    }
}

fn synthetic_external_error_diagnostics(
    route: &ExternalRouteRequest,
    status: StatusCode,
    source: &'static str,
) -> UsageErrorDiagnostics {
    UsageErrorDiagnostics {
        status_code: Some(status.as_u16()),
        source: Some(source.to_string()),
        error_id: Some(route.error_id.clone()),
        metadata: Some(json!({ "syntheticStatus": true })),
        public_error: None,
    }
}

fn classify_external_error(
    status: StatusCode,
    body: Bytes,
    _headers: HeaderMap,
    config: &ExternalPoolsConfig,
) -> ExternalPoolError {
    let message = String::from_utf8_lossy(&body).to_string();
    let lower = message.to_ascii_lowercase();
    if lower.contains("too many requests")
        || lower.contains("service_request_rate_exceeded")
        || lower.contains("rate limit")
        || lower.contains("ratelimit")
    {
        return ExternalPoolError {
            status: Some(status),
            message,
            retryable: true,
            auto_disable_reason: None,
            cooldown: Some((
                Duration::from_secs(config.external_pool_rate_limit_cooldown_secs.max(1)),
                "rate_limit".to_string(),
            )),
            response_body: Some(body),
        };
    }
    if lower.contains("database is locked") || lower.contains("sqlite_busy") {
        return ExternalPoolError {
            status: Some(status),
            message,
            retryable: true,
            auto_disable_reason: None,
            cooldown: Some((
                Duration::from_secs(config.external_pool_server_error_cooldown_secs.max(1)),
                "database_busy".to_string(),
            )),
            response_body: Some(body),
        };
    }
    if lower.contains("invalid token") {
        return ExternalPoolError {
            status: Some(status),
            message,
            retryable: true,
            auto_disable_reason: Some("auth_error".to_string()),
            cooldown: Some((
                Duration::from_secs(config.external_pool_protocol_error_cooldown_secs.max(1)),
                "auth_error".to_string(),
            )),
            response_body: Some(body),
        };
    }
    if lower.contains("channel affinity") && lower.contains("disabled") {
        return ExternalPoolError {
            status: Some(status),
            message,
            retryable: true,
            auto_disable_reason: Some("channel_disabled".to_string()),
            cooldown: Some((
                Duration::from_secs(config.external_pool_protocol_error_cooldown_secs.max(1)),
                "channel_disabled".to_string(),
            )),
            response_body: Some(body),
        };
    }
    if lower.contains("model_not_found")
        || lower.contains("failed to get available channel for model")
        || lower.contains("no available channel")
    {
        return ExternalPoolError {
            status: Some(status),
            message,
            retryable: true,
            auto_disable_reason: None,
            cooldown: Some((
                Duration::from_secs(config.external_pool_server_error_cooldown_secs.max(1)),
                "model_unavailable".to_string(),
            )),
            response_body: Some(body),
        };
    }
    if status == StatusCode::BAD_REQUEST {
        return ExternalPoolError {
            status: Some(status),
            message,
            retryable: false,
            auto_disable_reason: None,
            cooldown: None,
            response_body: Some(body),
        };
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        return ExternalPoolError {
            status: Some(status),
            message,
            retryable: true,
            auto_disable_reason: None,
            cooldown: Some((
                Duration::from_secs(config.external_pool_rate_limit_cooldown_secs.max(1)),
                "rate_limit".to_string(),
            )),
            response_body: Some(body),
        };
    }
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        let reason = if lower.contains("suspended")
            || lower.contains("risk")
            || lower.contains("locked")
            || lower.contains("security precaution")
        {
            "security_lock"
        } else {
            "auth_error"
        };
        return ExternalPoolError {
            status: Some(status),
            message,
            retryable: true,
            auto_disable_reason: Some(reason.to_string()),
            cooldown: Some((
                Duration::from_secs(config.external_pool_protocol_error_cooldown_secs.max(1)),
                reason.to_string(),
            )),
            response_body: Some(body),
        };
    }
    if status.as_u16() == 402
        || lower.contains("quota")
        || lower.contains("insufficient credit")
        || lower.contains("insufficient balance")
        || lower.contains("billing")
        || lower.contains("payment required")
        || lower.contains("no credit")
        || lower.contains("not enough credit")
    {
        return ExternalPoolError {
            status: Some(status),
            message,
            retryable: true,
            auto_disable_reason: Some("quota_exhausted".to_string()),
            cooldown: Some((
                Duration::from_secs(config.external_pool_rate_limit_cooldown_secs.max(1)),
                "quota_exhausted".to_string(),
            )),
            response_body: Some(body),
        };
    }
    if status.is_server_error() || status == StatusCode::REQUEST_TIMEOUT {
        return ExternalPoolError {
            status: Some(status),
            message,
            retryable: true,
            auto_disable_reason: None,
            cooldown: Some((
                Duration::from_secs(config.external_pool_server_error_cooldown_secs.max(1)),
                "server_error".to_string(),
            )),
            response_body: Some(body),
        };
    }
    ExternalPoolError {
        status: Some(status),
        message,
        retryable: false,
        auto_disable_reason: None,
        cooldown: None,
        response_body: Some(body),
    }
}

fn should_retry_external_payload_guard(
    route: &ExternalRouteRequest,
    err: &ExternalPoolError,
) -> bool {
    retry_pipeline::should_retry_payload_guard(route, err)
}

fn external_payload_guard_retry_route(
    route: &ExternalRouteRequest,
) -> Option<ExternalRouteRequest> {
    retry_pipeline::payload_guard_retry_route(route)
}

fn auto_disable_reason_enabled(config: &ExternalPoolsConfig, reason: &str) -> bool {
    match reason {
        "auth_error" => config.external_pool_auto_disable_on_auth_error,
        "security_lock" => config.external_pool_auto_disable_on_security_lock,
        "quota_exhausted" => config.external_pool_auto_disable_on_quota_exhausted,
        "misconfigured_endpoint" => config.external_pool_auto_disable_on_misconfigured_endpoint,
        "channel_disabled" => config.external_pool_auto_disable_on_channel_disabled,
        _ => false,
    }
}

fn error_type_for_external_error(err: &ExternalPoolError) -> String {
    if let Some(reason) = err.auto_disable_reason.as_deref() {
        return reason.to_string();
    }
    if let Some((_, reason)) = err.cooldown.as_ref() {
        if reason == "rate_limit"
            || reason == "database_busy"
            || reason == "model_unavailable"
            || reason == "model_mapping_miss"
            || reason == "server_error"
            || reason.starts_with("network_error")
        {
            return reason
                .split_whitespace()
                .next()
                .unwrap_or("external_pool_error")
                .to_string();
        }
    }
    match err.status {
        Some(StatusCode::TOO_MANY_REQUESTS) => "rate_limit",
        Some(status) if status.is_server_error() => "server_error",
        Some(StatusCode::BAD_REQUEST) => "bad_request",
        _ => "external_pool_error",
    }
    .to_string()
}

fn anthropic_error_type_for_external_error(err: &ExternalPoolError) -> &'static str {
    match err.status {
        Some(StatusCode::BAD_REQUEST) => "invalid_request_error",
        Some(StatusCode::UNAUTHORIZED) => "authentication_error",
        Some(StatusCode::FORBIDDEN) => "permission_error",
        Some(StatusCode::TOO_MANY_REQUESTS) => "rate_limit_error",
        Some(status) if status.is_server_error() || status == StatusCode::REQUEST_TIMEOUT => {
            "api_error"
        }
        _ => match error_type_for_external_error(err).as_str() {
            "rate_limit" => "rate_limit_error",
            "bad_request" => "invalid_request_error",
            "auth_error" => "authentication_error",
            "security_lock" => "permission_error",
            _ => "api_error",
        },
    }
}

struct ProjectedNonStreamBody {
    body: Bytes,
    usage_capture: ExternalUsageCapture,
}

fn maybe_project_non_stream_usage(
    bytes: Bytes,
    projection: Option<&ExternalUsageProjectionContext>,
) -> ProjectedNonStreamBody {
    let mut usage_capture = ExternalUsageCapture::default();
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return ProjectedNonStreamBody {
            body: bytes,
            usage_capture,
        };
    };
    let Some(usage) = value.get_mut("usage") else {
        return ProjectedNonStreamBody {
            body: bytes,
            usage_capture,
        };
    };
    let raw_usage = cache_usage_from_value(usage);
    usage_capture.raw = raw_usage;
    usage_capture.reported = raw_usage;

    if let Some(projected) = project_usage_value(usage, projection, true) {
        usage_capture.request_input_tokens = Some(projected.request_input_tokens);
        usage_capture.shaped = Some(projected.shaped);
        usage_capture.reported = cache_usage_from_value(usage)
            .or(Some(projected.reported))
            .or(raw_usage);
        usage_capture.projected = true;
        let body = serde_json::to_vec(&value).map(Bytes::from).unwrap_or(bytes);
        return ProjectedNonStreamBody {
            body,
            usage_capture,
        };
    }

    ProjectedNonStreamBody {
        body: bytes,
        usage_capture,
    }
}

fn rewrite_sse_event_usage(
    event: &[u8],
    projection: Option<&ExternalUsageProjectionContext>,
    capture: Option<&Arc<SyncMutex<ExternalUsageCapture>>>,
) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(event) else {
        return event.to_vec();
    };
    let mut changed = false;
    let mut out = Vec::with_capacity(event.len());
    for line in text.split_inclusive('\n') {
        let trimmed_line_end = line.trim_end_matches(['\r', '\n']);
        let line_ending = &line[trimmed_line_end.len()..];
        let Some(data) = trimmed_line_end.strip_prefix("data:") else {
            out.extend_from_slice(line.as_bytes());
            continue;
        };
        let leading_ws_len = data.len().saturating_sub(data.trim_start().len());
        let (leading_ws, data_json) = data.split_at(leading_ws_len);
        let data_json = data_json.trim_end();
        if data_json == "[DONE]" {
            out.extend_from_slice(line.as_bytes());
            continue;
        }
        let Ok(mut value) = serde_json::from_str::<serde_json::Value>(data_json) else {
            out.extend_from_slice(line.as_bytes());
            continue;
        };
        let projected = process_usage_slots_in_sse_value(&mut value, projection, capture, true);
        if !projected.changed {
            out.extend_from_slice(line.as_bytes());
            continue;
        }
        changed = true;
        out.extend_from_slice(b"data:");
        out.extend_from_slice(leading_ws.as_bytes());
        out.extend_from_slice(
            serde_json::to_string(&value)
                .unwrap_or_else(|_| data_json.to_string())
                .as_bytes(),
        );
        out.extend_from_slice(line_ending.as_bytes());
    }
    if changed { out } else { event.to_vec() }
}

fn capture_sse_event_usage(
    event: &[u8],
    projection: Option<&ExternalUsageProjectionContext>,
    capture: Option<&Arc<SyncMutex<ExternalUsageCapture>>>,
) {
    let Ok(text) = std::str::from_utf8(event) else {
        return;
    };
    for line in text.split_inclusive('\n') {
        let trimmed_line_end = line.trim_end_matches(['\r', '\n']);
        let Some(data) = trimmed_line_end.strip_prefix("data:") else {
            continue;
        };
        let data_json = data.trim();
        if data_json.is_empty() || data_json == "[DONE]" {
            continue;
        }
        let Ok(mut value) = serde_json::from_str::<serde_json::Value>(data_json) else {
            continue;
        };
        let _ = process_usage_slots_in_sse_value(&mut value, projection, capture, false);
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct SseUsageProcessingResult {
    changed: bool,
}

fn process_usage_slots_in_sse_value(
    value: &mut serde_json::Value,
    projection: Option<&ExternalUsageProjectionContext>,
    capture: Option<&Arc<SyncMutex<ExternalUsageCapture>>>,
    rewrite: bool,
) -> SseUsageProcessingResult {
    let mut result = SseUsageProcessingResult::default();
    let mut handled_top_level = false;
    if let Some(usage) = value.get_mut("usage") {
        handled_top_level = true;
        result.changed |= process_single_usage_value(usage, projection, capture, rewrite, true);
    }
    if let Some(usage) = value
        .get_mut("message")
        .and_then(|message| message.get_mut("usage"))
    {
        result.changed |= process_single_usage_value(usage, projection, capture, rewrite, false);
    }
    if !handled_top_level {
        if let Some(usage) = value
            .get_mut("delta")
            .and_then(|delta| delta.get_mut("usage"))
        {
            result.changed |= process_single_usage_value(usage, projection, capture, rewrite, true);
        }
    }
    result
}

fn process_single_usage_value(
    usage: &mut serde_json::Value,
    projection: Option<&ExternalUsageProjectionContext>,
    capture: Option<&Arc<SyncMutex<ExternalUsageCapture>>>,
    rewrite: bool,
    commit_cache_state: bool,
) -> bool {
    let raw_usage = cache_usage_from_value(usage);
    let Some(projected_usage) = project_usage_value(usage, projection, commit_cache_state) else {
        update_external_usage_capture(capture, raw_usage, raw_usage, raw_usage, false);
        return false;
    };
    update_external_usage_capture_request_input(
        capture,
        Some(projected_usage.request_input_tokens),
    );
    update_external_usage_capture(
        capture,
        raw_usage,
        Some(projected_usage.shaped),
        Some(projected_usage.reported),
        true,
    );
    rewrite
}

fn process_sse_event_with_plan(
    event: &[u8],
    projection: Option<&ExternalUsageProjectionContext>,
    capture: Option<&Arc<SyncMutex<ExternalUsageCapture>>>,
    stream_error_mask: Option<&ExternalStreamErrorMask>,
    plan: ExternalStreamProcessingPlan,
) -> Vec<u8> {
    if plan.mask_errors {
        if let Some(masked) =
            maybe_mask_external_stream_error_event(event, capture, stream_error_mask)
        {
            return masked;
        }
    }
    if projection.is_some() {
        return rewrite_sse_event_usage(event, projection, capture);
    }
    if plan.capture_usage {
        capture_sse_event_usage(event, projection, capture);
    }
    event.to_vec()
}

fn drain_sse_events(
    buffer: &mut Vec<u8>,
    projection: Option<&ExternalUsageProjectionContext>,
    capture: Option<&Arc<SyncMutex<ExternalUsageCapture>>>,
    stream_error_mask: Option<&ExternalStreamErrorMask>,
    plan: ExternalStreamProcessingPlan,
) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some((idx, delimiter_len)) = find_sse_event_delimiter(buffer) {
        let end = idx + delimiter_len;
        let event = buffer.drain(..end).collect::<Vec<u8>>();
        out.extend(process_sse_event_with_plan(
            &event,
            projection,
            capture,
            stream_error_mask,
            plan,
        ));
    }
    out
}

fn maybe_mask_external_stream_error_event(
    event: &[u8],
    capture: Option<&Arc<SyncMutex<ExternalUsageCapture>>>,
    mask: Option<&ExternalStreamErrorMask>,
) -> Option<Vec<u8>> {
    let mask = mask?;
    let text = std::str::from_utf8(event).ok()?;
    if !text.contains("error") && !text.contains("Error") {
        return None;
    }

    let explicit_error_event = text.lines().any(|line| {
        line.trim_end_matches('\r')
            .strip_prefix("event:")
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("error"))
    });
    let mut has_error_payload = false;
    for line in text.split_inclusive('\n') {
        let trimmed_line_end = line.trim_end_matches(['\r', '\n']);
        let Some(data) = trimmed_line_end.strip_prefix("data:") else {
            continue;
        };
        let data_json = data.trim();
        if data_json.is_empty() || data_json == "[DONE]" {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(data_json) else {
            continue;
        };
        if external_stream_payload_is_error(explicit_error_event, &value) {
            has_error_payload = true;
            break;
        }
    }

    if !explicit_error_event && !has_error_payload {
        return None;
    }

    let raw_event = compact_external_stream_error_event(text, 2048);
    if let Some(capture) = capture {
        let mut capture = capture.lock();
        if capture.stream_error_message.is_none() {
            capture.stream_error_message = Some(raw_event.clone());
        }
    }
    tracing::warn!(
        request_id = %mask.request_id,
        error_id = %mask.error_id,
        pool_id = mask.pool_id,
        pool_name = %mask.pool_name,
        raw_event = %raw_event,
        "external pool stream error event masked"
    );

    let message = envelope::public_message_with_error_id(
        envelope::PUBLIC_TEMPORARY_FAILURE_MESSAGE,
        &mask.error_id,
    );
    let body = json!({
        "type": "error",
        "error": {
            "type": "api_error",
            "message": message,
        },
        "request_id": mask.request_id,
    });
    let data = serde_json::to_string(&body).unwrap_or_else(|_| {
        format!(
            r#"{{"type":"error","error":{{"type":"api_error","message":"{}"}},"request_id":"{}"}}"#,
            message, mask.request_id
        )
    });
    Some(format!("event: error\ndata: {data}\n\n").into_bytes())
}

fn external_stream_payload_is_error(explicit_error_event: bool, value: &serde_json::Value) -> bool {
    value
        .get("type")
        .and_then(|value| value.as_str())
        .is_some_and(|value| value == "error")
        || (explicit_error_event
            && value.get("error").is_some_and(|error| {
                error.is_object() || error.as_str().is_some_and(|value| !value.is_empty())
            }))
}

fn compact_external_stream_error_event(raw: &str, max_bytes: usize) -> String {
    let compact = raw.replace(['\r', '\n'], " ");
    if compact.len() <= max_bytes {
        return compact;
    }
    let mut out = String::with_capacity(max_bytes + 3);
    for ch in compact.chars() {
        if out.len() + ch.len_utf8() > max_bytes {
            out.push_str("...");
            break;
        }
        out.push(ch);
    }
    out
}

fn update_external_usage_capture(
    capture: Option<&Arc<SyncMutex<ExternalUsageCapture>>>,
    raw: Option<CacheUsage>,
    shaped: Option<CacheUsage>,
    reported: Option<CacheUsage>,
    projected: bool,
) {
    let Some(capture) = capture else {
        return;
    };
    let mut capture = capture.lock();
    if let Some(raw) = raw {
        capture.raw = Some(merge_external_usage(capture.raw, raw));
    }
    if let Some(shaped) = shaped {
        capture.shaped = Some(merge_external_usage(capture.shaped, shaped));
    }
    if let Some(reported) = reported {
        capture.reported = if projected {
            Some(reported)
        } else {
            Some(merge_external_usage(capture.reported, reported))
        };
    }
    if projected {
        capture.projected = true;
    }
}

fn update_external_usage_capture_request_input(
    capture: Option<&Arc<SyncMutex<ExternalUsageCapture>>>,
    request_input_tokens: Option<i32>,
) {
    let Some(request_input_tokens) = request_input_tokens else {
        return;
    };
    let Some(capture) = capture else {
        return;
    };
    let mut capture = capture.lock();
    capture.request_input_tokens = Some(request_input_tokens.max(0));
}

fn merge_external_usage(existing: Option<CacheUsage>, incoming: CacheUsage) -> CacheUsage {
    let Some(existing) = existing else {
        return incoming;
    };
    let input_tokens = existing.input_tokens.max(incoming.input_tokens);
    let output_tokens = existing.output_tokens.max(incoming.output_tokens);
    let cache_read_input_tokens = existing
        .cache_read_input_tokens
        .max(incoming.cache_read_input_tokens);
    let cache_creation_input_tokens = existing
        .cache_creation_input_tokens
        .max(incoming.cache_creation_input_tokens);
    let cache_creation_5m_input_tokens = existing
        .cache_creation_5m_input_tokens
        .max(incoming.cache_creation_5m_input_tokens)
        .min(cache_creation_input_tokens);
    let cache_creation_1h_input_tokens = existing
        .cache_creation_1h_input_tokens
        .max(incoming.cache_creation_1h_input_tokens)
        .min(cache_creation_input_tokens.saturating_sub(cache_creation_5m_input_tokens));
    CacheUsage {
        total_input_tokens: input_tokens
            .saturating_add(cache_read_input_tokens)
            .saturating_add(cache_creation_input_tokens),
        input_tokens,
        output_tokens,
        cache_creation_input_tokens,
        cache_read_input_tokens,
        cache_creation_5m_input_tokens,
        cache_creation_1h_input_tokens,
    }
}

fn find_sse_event_delimiter(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|idx| (idx, 2));
    let crlf = buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|idx| (idx, 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ProjectedExternalUsage {
    request_input_tokens: i32,
    shaped: CacheUsage,
    reported: CacheUsage,
}

fn project_usage_value(
    usage: &mut serde_json::Value,
    projection: Option<&ExternalUsageProjectionContext>,
    commit_cache_state: bool,
) -> Option<ProjectedExternalUsage> {
    let projection = projection?;
    if projection.mode != ExternalPoolUsageProjectionMode::CurrentPathPolicy {
        return None;
    }
    let output_tokens = usage_i32(usage, "output_tokens");
    let upstream_cache_read_evidence =
        projection.observe_cache_read_evidence(usage_i32(usage, "cache_read_input_tokens") > 0);
    let computed = projection
        .simulated_usage
        .map(|simulation| {
            CacheSimulation::to_usage(simulation, projection.raw_input_tokens, output_tokens)
        })
        .unwrap_or_else(|| CacheUsage {
            total_input_tokens: projection.raw_input_tokens,
            input_tokens: projection.raw_input_tokens,
            output_tokens,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        });
    if commit_cache_state {
        projection.mark_committed(computed);
    }
    let controlled = if projection.cache_state_enabled && projection.reported_policy.is_some() {
        projection
            .prompt_cache_creation_controller
            .preview_success_with_context(
                projection.scope.as_ref(),
                projection.prompt_cache_creation_control,
                computed,
                projection.credential_key.as_deref(),
                Some(projection.model.as_str()),
            )
    } else {
        computed
    };
    let raw_usage = RawUsage::uncached(projection.raw_input_tokens, output_tokens);
    let shaped = projection
        .reported_policy
        .clone()
        .map(|policy| {
            controlled.with_reported_cache_usage_policy_and_raw_evidence(
                policy,
                raw_usage,
                upstream_cache_read_evidence,
            )
        })
        .unwrap_or(controlled);
    let shaped = projection
        .reported_policy
        .clone()
        .map(|policy| {
            let shaped = policy.apply_final_input_guard(shaped);
            policy.apply_final_cache_read_guard(shaped)
        })
        .unwrap_or(shaped);
    let projected = shaped
        .with_external_pool_usage_uplift(projection.uplift_percent)
        .with_external_pool_output_uplift(
            projection.output_uplift_min_tokens,
            projection.output_uplift_percent,
        );
    let projected = projection
        .reported_policy
        .clone()
        .map(|policy| policy.apply_final_cache_read_guard(projected))
        .unwrap_or(projected);
    let projected_json = projected.to_anthropic_usage_json();
    let Some(obj) = usage.as_object_mut() else {
        return None;
    };
    let Some(projected_obj) = projected_json.as_object() else {
        return None;
    };
    for (key, value) in projected_obj {
        obj.insert(key.clone(), value.clone());
    }
    obj.remove("cache_creation_5m_input_tokens");
    obj.remove("cache_creation_1h_input_tokens");
    apply_projected_cache_creation_breakdown(obj, projected);
    Some(ProjectedExternalUsage {
        request_input_tokens: projection.raw_input_tokens,
        shaped,
        reported: projected,
    })
}

fn apply_projected_cache_creation_breakdown(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    usage: CacheUsage,
) {
    let cache_creation_input_tokens = usage.cache_creation_input_tokens.max(0);
    if cache_creation_input_tokens == 0 {
        obj.remove("cache_creation");
        return;
    }

    let cache_creation_1h_input_tokens = usage
        .cache_creation_1h_input_tokens
        .max(0)
        .min(cache_creation_input_tokens);
    let cache_creation_5m_input_tokens = usage
        .cache_creation_5m_input_tokens
        .max(0)
        .min(cache_creation_input_tokens.saturating_sub(cache_creation_1h_input_tokens));
    let remainder = cache_creation_input_tokens
        .saturating_sub(cache_creation_5m_input_tokens)
        .saturating_sub(cache_creation_1h_input_tokens);

    obj.insert(
        "cache_creation".to_string(),
        json!({
            "ephemeral_5m_input_tokens": cache_creation_5m_input_tokens.saturating_add(remainder),
            "ephemeral_1h_input_tokens": cache_creation_1h_input_tokens,
        }),
    );
}

trait ExternalPoolUsageUplift {
    fn with_external_pool_usage_uplift(self, percent: u32) -> Self;
    fn with_external_pool_output_uplift(self, min_tokens: i32, percent: u32) -> Self;
}

impl ExternalPoolUsageUplift for CacheUsage {
    fn with_external_pool_usage_uplift(self, percent: u32) -> Self {
        let percent = percent.min(200);
        if percent == 0 {
            return self;
        }

        let cache_read_input_tokens = uplift_tokens(self.cache_read_input_tokens.max(0), percent);
        let cache_creation_input_tokens =
            uplift_tokens(self.cache_creation_input_tokens.max(0), percent);
        let (cache_creation_5m_input_tokens, cache_creation_1h_input_tokens) =
            uplift_cache_creation_breakdown(
                self.cache_creation_5m_input_tokens,
                self.cache_creation_1h_input_tokens,
                cache_creation_input_tokens,
            );

        Self {
            total_input_tokens: self
                .input_tokens
                .max(0)
                .saturating_add(cache_read_input_tokens)
                .saturating_add(cache_creation_input_tokens),
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_creation_input_tokens,
            cache_read_input_tokens,
            cache_creation_5m_input_tokens,
            cache_creation_1h_input_tokens,
        }
    }

    fn with_external_pool_output_uplift(self, min_tokens: i32, percent: u32) -> Self {
        let percent = percent.min(200);
        let min_tokens = min_tokens.max(0);
        if percent == 0 || min_tokens == 0 || self.output_tokens < min_tokens {
            return self;
        }

        Self {
            output_tokens: uplift_tokens(self.output_tokens.max(0), percent),
            ..self
        }
    }
}

fn uplift_tokens(tokens: i32, percent: u32) -> i32 {
    if tokens <= 0 || percent == 0 {
        return tokens.max(0);
    }
    let numerator = tokens as i64 * (100 + percent.min(200)) as i64;
    ((numerator + 99) / 100).clamp(0, i32::MAX as i64) as i32
}

fn uplift_cache_creation_breakdown(
    base_5m: i32,
    base_1h: i32,
    cache_creation_input_tokens: i32,
) -> (i32, i32) {
    let cache_creation_input_tokens = cache_creation_input_tokens.max(0);
    if cache_creation_input_tokens == 0 {
        return (0, 0);
    }
    let base_5m = base_5m.max(0);
    let base_1h = base_1h.max(0);
    let base_total = base_5m.saturating_add(base_1h);
    if base_total == 0 {
        return (cache_creation_input_tokens, 0);
    }
    let five_min = ((cache_creation_input_tokens as i64 * base_5m as i64)
        + (base_total as i64 / 2))
        / base_total as i64;
    let five_min = five_min.clamp(0, cache_creation_input_tokens as i64) as i32;
    let one_hour = cache_creation_input_tokens.saturating_sub(five_min);
    (five_min, one_hour)
}

fn cache_usage_from_value(value: &serde_json::Value) -> Option<CacheUsage> {
    let input_tokens = usage_i32(value, "input_tokens");
    let output_tokens = usage_i32(value, "output_tokens");
    let mut cache_creation_input_tokens = usage_i32(value, "cache_creation_input_tokens");
    let cache_read_input_tokens = usage_i32(value, "cache_read_input_tokens");
    let nested_cache_creation = value.get("cache_creation");
    let nested_cache_creation_5m_input_tokens = nested_cache_creation
        .map(|value| usage_i32(value, "ephemeral_5m_input_tokens"))
        .unwrap_or(0);
    let nested_cache_creation_1h_input_tokens = nested_cache_creation
        .map(|value| usage_i32(value, "ephemeral_1h_input_tokens"))
        .unwrap_or(0);
    let mut cache_creation_5m_input_tokens = usage_i32(value, "cache_creation_5m_input_tokens")
        .max(nested_cache_creation_5m_input_tokens);
    let mut cache_creation_1h_input_tokens = usage_i32(value, "cache_creation_1h_input_tokens")
        .max(nested_cache_creation_1h_input_tokens);
    let nested_cache_creation_input_tokens =
        cache_creation_5m_input_tokens.saturating_add(cache_creation_1h_input_tokens);
    cache_creation_input_tokens =
        cache_creation_input_tokens.max(nested_cache_creation_input_tokens);
    cache_creation_5m_input_tokens =
        cache_creation_5m_input_tokens.min(cache_creation_input_tokens);
    cache_creation_1h_input_tokens = cache_creation_1h_input_tokens
        .min(cache_creation_input_tokens.saturating_sub(cache_creation_5m_input_tokens));
    if input_tokens == 0
        && output_tokens == 0
        && cache_creation_input_tokens == 0
        && cache_read_input_tokens == 0
    {
        return None;
    }
    Some(CacheUsage {
        total_input_tokens: input_tokens
            .saturating_add(cache_creation_input_tokens)
            .saturating_add(cache_read_input_tokens),
        input_tokens,
        output_tokens,
        cache_creation_input_tokens,
        cache_read_input_tokens,
        cache_creation_5m_input_tokens,
        cache_creation_1h_input_tokens,
    })
}

fn external_pool_billing_from_capture(
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
    capture: ExternalUsageCapture,
) -> Option<ExternalPoolBilling> {
    let raw = capture.raw?;
    let reported = capture.reported.or(capture.raw)?;
    let shaped = capture.shaped.or(capture.reported).or(capture.raw)?;
    let stream_response_mode = capture
        .stream_response_mode
        .map(|mode| mode.as_str().to_string());
    let mut billing = external_pool_billing(
        route,
        pool,
        capture.request_input_tokens,
        raw,
        shaped,
        reported,
        capture.projected,
    );
    billing.stream_response_mode = stream_response_mode;
    Some(billing)
}

fn external_pool_billing_from_capture_ref(
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
    capture: &Arc<SyncMutex<ExternalUsageCapture>>,
) -> Option<ExternalPoolBilling> {
    external_pool_billing_from_capture(route, pool, capture.lock().clone())
}

fn external_pool_billing(
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
    request_input_tokens: Option<i32>,
    raw_usage: CacheUsage,
    shaped_usage: CacheUsage,
    reported_usage: CacheUsage,
    usage_projection_applied: bool,
) -> ExternalPoolBilling {
    let pricing_model = route
        .upstream_model
        .as_deref()
        .or(route.model_hint.as_deref())
        .or_else(|| route.payload.as_ref().map(|payload| payload.model.as_str()))
        .unwrap_or("unknown");
    let raw_estimate = route.pricing_catalog.estimate(pricing_model, raw_usage);
    let shaped_estimate = route.pricing_catalog.estimate(pricing_model, shaped_usage);
    let reported_estimate = route
        .pricing_catalog
        .estimate(pricing_model, reported_usage);
    let pricing_available =
        raw_estimate.available && shaped_estimate.available && reported_estimate.available;
    let raw_cost_usd = if pricing_available {
        raw_estimate.cost_usd
    } else {
        0.0
    };
    let shaped_cost_usd = if pricing_available {
        shaped_estimate.cost_usd
    } else {
        0.0
    };
    let reported_cost_usd = if pricing_available {
        reported_estimate.cost_usd
    } else {
        0.0
    };
    let uplifted_cost_usd = reported_cost_usd;
    let profit_usd = uplifted_cost_usd - raw_cost_usd;

    ExternalPoolBilling {
        request_input_tokens: request_input_tokens.map(|tokens| tokens.max(0)),
        raw_usage: external_usage_snapshot(raw_usage),
        shaped_usage: external_usage_snapshot(shaped_usage),
        reported_usage: external_usage_snapshot(reported_usage),
        usage_projection_applied,
        raw_cost_usd,
        shaped_cost_usd,
        uplifted_cost_usd,
        profit_usd,
        reported_cost_usd,
        billable_cost_usd: uplifted_cost_usd,
        cost_floor_delta_usd: (raw_cost_usd - uplifted_cost_usd).max(0.0),
        cost_floor_applied: pricing_available && uplifted_cost_usd < raw_cost_usd,
        pricing_available,
        pricing_model: Some(reported_estimate.model),
        usage_projection_mode: pool.usage_projection_mode.as_str().to_string(),
        stream_response_mode: None,
    }
}

fn build_external_usage_projection_context(
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
    uplift_percent: u32,
    output_uplift_min_tokens: i32,
    output_uplift_percent: u32,
) -> Option<ExternalUsageProjectionContext> {
    usage_projection::build_context(
        route,
        pool,
        uplift_percent,
        output_uplift_min_tokens,
        output_uplift_percent,
    )
}

#[cfg(test)]
fn count_external_route_input_tokens(payload: &MessagesRequest) -> i32 {
    crate::token::count_all_tokens(
        &payload.model,
        payload.system.as_deref(),
        &payload.messages,
        payload.tools.as_deref(),
    ) as i32
}

fn external_usage_snapshot(usage: CacheUsage) -> ExternalPoolUsageSnapshot {
    ExternalPoolUsageSnapshot {
        total_input_tokens: usage.total_input_tokens,
        input_tokens: usage.input_tokens,
        billable_input_tokens: usage.billable_input_tokens(),
        output_tokens: usage.output_tokens,
        cache_read_input_tokens: usage.cache_read_input_tokens,
        cache_creation_input_tokens: usage.cache_creation_input_tokens,
        cache_creation_5m_input_tokens: usage.cache_creation_5m_input_tokens,
        cache_creation_1h_input_tokens: usage.cache_creation_1h_input_tokens,
    }
}

fn usage_i32(value: &serde_json::Value, key: &str) -> i32 {
    value
        .get(key)
        .and_then(|value| value.as_i64())
        .unwrap_or(0)
        .clamp(0, i32::MAX as i64) as i32
}

#[cfg(test)]
#[path = "external_pool/tests.rs"]
mod tests;
