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
            PayloadByteBreakdown, PayloadGuardConfig, PayloadGuardReport,
            breakdown_anthropic_messages_request, guard_anthropic_messages_request_reusing_body,
        },
        pricing::PricingCatalog,
        prompt_cache::{
            KiroRsToolPromptCachePlan, PromptCacheBounds, PromptCacheProfile, PromptCacheScope,
            PromptCacheTracker,
        },
        prompt_cache_creation_control::PromptCacheCreationController,
        types::MessagesRequest,
        usage::{
            ExternalPoolAttempt, ExternalPoolBilling, ExternalPoolUsageSnapshot, UsageLatencyTrace,
            UsagePublicError, UsageRecord, UsageRecordStatus, UsageRouteKind, UsageRouteSubtype,
            UsageSource,
        },
    },
    model::config::{
        ExternalPoolCapacityMode, ExternalPoolsConfig, KiroRsToolCachePolicy, ModelMappingRule,
        PromptCacheCreationControlConfig, PromptCacheSimulationMode, PromptCacheStrategyType,
        ReportedUsageConfig,
    },
    model::model_processing::{
        ModelProcessingConfig, ModelProcessingError, ModelProcessingInput, ModelProcessingMode,
        process_model,
    },
    storage::{
        postgres::PostgresStore,
        redis_cache::{LocalPoolCircuitState, RedisStore},
    },
};

const DEFAULT_EXTERNAL_POOL_REQUEST_TIMEOUT_SECS: u64 = 180;
const EXTERNAL_POOL_LEASE_TOUCH_INTERVAL_SECS: u64 = 30;
const EXTERNAL_POOL_AVAILABILITY_CACHE_TTL: Duration = Duration::from_millis(250);
const MAX_RECORDED_EXTERNAL_ERROR_MESSAGE_BYTES: usize = 8192;
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
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
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
    pub notes: Option<String>,
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
    pub payload: MessagesRequest,
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
    pub payload_guard_retry_config: Option<PayloadGuardConfig>,
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
            client_dropped_ms: None,
            terminal_reason: None,
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

struct PreparedExternalRequest {
    body: Bytes,
    outbound_model: Option<String>,
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

    pub fn is_capacity_like(&self) -> bool {
        matches!(
            self.route_error_type.as_str(),
            "external_pool_capacity_full"
                | "external_pool_queue_full"
                | "external_pool_wait_timeout"
                | "external_pool_cooldown"
        )
    }
}

#[derive(Debug, Clone, Default)]
struct ExternalUsageCapture {
    raw: Option<CacheUsage>,
    shaped: Option<CacheUsage>,
    reported: Option<CacheUsage>,
    projected: bool,
    stream_error_message: Option<String>,
}

#[derive(Debug, Default)]
struct ExternalUsageProjectionState {
    committed_controlled_usage: Option<CacheUsage>,
}

#[derive(Clone)]
struct ExternalUsageProjectionContext {
    mode: ExternalPoolUsageProjectionMode,
    raw_input_tokens: i32,
    cache_state_enabled: bool,
    credential_key: Option<String>,
    model: String,
    simulated_usage: Option<CacheSimulation>,
    reported_policy: Option<ReportedCacheUsagePolicy>,
    scope: Option<PromptCacheScope>,
    prompt_cache: Arc<PromptCacheTracker>,
    prompt_cache_profile: Option<PromptCacheProfile>,
    kiro_rs_tool_prompt_cache_plan: Option<KiroRsToolPromptCachePlan>,
    prompt_cache_target_read_ratio: f64,
    prompt_cache_bounds: PromptCacheBounds,
    prompt_cache_creation_controller: Arc<PromptCacheCreationController>,
    prompt_cache_creation_control: PromptCacheCreationControlConfig,
    uplift_percent: u32,
    output_uplift_min_tokens: i32,
    output_uplift_percent: u32,
    state: Arc<SyncMutex<ExternalUsageProjectionState>>,
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
    fn touch(&self) {
        let manager = self.manager.clone();
        let pool_id = self.pool_id;
        let lease_id = self.lease_id;
        tokio::spawn(async move {
            manager.touch_pool(pool_id, lease_id).await;
        });
    }
}

impl Drop for ExternalPoolLease {
    fn drop(&mut self) {
        let manager = self.manager.clone();
        let pool_id = self.pool_id;
        let lease_id = self.lease_id;
        tokio::spawn(async move {
            manager.release_pool(pool_id, lease_id).await;
        });
    }
}

struct ExternalPoolQueueGuard {
    manager: ExternalPoolManager,
    released: bool,
}

impl ExternalPoolQueueGuard {
    fn new(manager: ExternalPoolManager) -> Self {
        Self {
            manager,
            released: false,
        }
    }

    fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        let manager = self.manager.clone();
        tokio::spawn(async move {
            manager.leave_external_pool_queue().await;
        });
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
                self.pool_runtime_snapshot(pool.id).await;
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
                .select_pool_with_availability_uncached(&excluded, &config)
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
                PoolAcquireResult::Unavailable(unavailable) => {
                    tracing::debug!(
                        pool_id,
                        reason = ?unavailable.reason,
                        detail = unavailable.detail,
                        exclude_pool_for_reselect = unavailable.exclude_pool_for_reselect,
                        "外部池选中后并发 lease 未占用，本次请求尝试重选或按配置等待"
                    );
                    if unavailable.exclude_pool_for_reselect {
                        excluded.insert(pool_id);
                        if self.select_pool(&excluded, &config).await.is_some() {
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
                    if route.payload.stream {
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
                    if should_retry_external_payload_guard(&route, &err) {
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
        if route.payload.stream {
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
        if route.payload.stream {
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
            let usage_capture = Arc::new(SyncMutex::new(ExternalUsageCapture::default()));
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
                                            let projected = drain_projected_sse_events(
                                                &mut buffer,
                                                projection_context.as_ref(),
                                                Some(&usage_capture),
                                                Some(stream_error_mask.as_ref()),
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
                                                maybe_project_sse_event(
                                                    &buffer,
                                                    projection_context.as_ref(),
                                                    Some(&usage_capture),
                                                    Some(stream_error_mask.as_ref()),
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
                                        lease.touch();
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

    async fn select_pool(
        &self,
        excluded: &HashSet<u64>,
        config: &ExternalPoolsConfig,
    ) -> Option<ExternalPool> {
        self.scan_pool_availability_uncached(excluded, config, true)
            .await
            .selected_pool
    }

    async fn select_pool_with_availability_uncached(
        &self,
        excluded: &HashSet<u64>,
        config: &ExternalPoolsConfig,
    ) -> PoolSelectionSnapshot {
        self.scan_pool_availability_uncached(excluded, config, true)
            .await
    }

    async fn scan_pool_availability_uncached(
        &self,
        excluded: &HashSet<u64>,
        config: &ExternalPoolsConfig,
        include_selection: bool,
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
            availability.eligible_pools += 1;
            let (in_flight, global_in_flight, cooldown_remaining_secs, _) =
                self.pool_runtime_snapshot(pool.id).await;
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
        PoolSelectionSnapshot {
            selected_pool: candidates.and_then(select_external_pool_candidate),
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
            .scan_pool_availability_uncached(excluded, config, false)
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
        if config.external_pool_capacity_mode != ExternalPoolCapacityMode::Wait {
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
                    let message = format!("Request dispatch queue unavailable: {}", err);
                    self.record_external_failure(
                        route,
                        None,
                        attempts,
                        "external_pool_queue_error",
                        &message,
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
        let (_, _, cooldown_remaining_secs, _) = self.pool_runtime_snapshot(pool.id).await;
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
                    reason: PoolCapacityWaitReason::Full,
                    wait_for: Some(Duration::from_secs(1)),
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
                    self.pool_runtime_snapshot(pool.id).await;
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
                    reason: PoolCapacityWaitReason::Full,
                    wait_for: Some(Duration::from_secs(1)),
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
        if self
            .redis
            .try_enter_external_pool_dispatch_queue(max_queued)
            .await?
        {
            Ok(Some(ExternalPoolQueueGuard::new(self.clone())))
        } else {
            Ok(None)
        }
    }

    async fn leave_external_pool_queue(&self) {
        if let Err(err) = self.redis.leave_external_pool_dispatch_queue().await {
            tracing::warn!("释放外部池 Redis 调度排队占位失败: {}", err);
        }
        self.capacity_notify.notify_waiters();
    }

    async fn release_pool(&self, pool_id: u64, lease_id: u64) {
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
            Err(err) => tracing::warn!(
                pool_id,
                lease_id,
                "释放外部池 Redis 并发 lease 失败: {}",
                err
            ),
        }
        self.capacity_notify.notify_waiters();
    }

    async fn touch_pool(&self, pool_id: u64, lease_id: u64) {
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
            Err(err) => tracing::warn!(
                pool_id,
                lease_id,
                "续期外部池 Redis 并发 lease 失败: {}",
                err
            ),
        }
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

    async fn pool_runtime_snapshot(&self, pool_id: u64) -> (u32, u32, u64, Option<String>) {
        let capacity_state = self
            .redis
            .external_pool_capacity_state(
                pool_id,
                Some(Duration::from_secs(
                    DEFAULT_EXTERNAL_POOL_REQUEST_TIMEOUT_SECS.saturating_mul(2),
                )),
            )
            .await
            .unwrap_or_else(|err| {
                tracing::warn!(pool_id, "读取外部池 Redis 并发状态失败: {}", err);
                Default::default()
            });
        let in_flight = capacity_state.pool_in_flight_requests;
        let global_in_flight = capacity_state.global_in_flight_requests;
        match self
            .redis
            .get_json::<ExternalPoolCooldownState>(format!("external_pool:{}:cooldown", pool_id))
            .await
        {
            Ok(Some(cooldown)) => {
                let now = Utc::now();
                if cooldown.until > now {
                    return (
                        in_flight,
                        global_in_flight,
                        (cooldown.until - now).num_seconds().max(1) as u64,
                        cooldown.reason,
                    );
                }
                let _ = self
                    .redis
                    .del(format!("external_pool:{}:cooldown", pool_id))
                    .await;
            }
            Ok(None) => {}
            Err(err) => tracing::warn!(pool_id, "读取外部池 Redis cooldown 失败: {}", err),
        }
        (in_flight, global_in_flight, 0, None)
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
        let request_input_tokens = route.request_input_tokens;
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
            stream: route.payload.stream,
            model: route.payload.model.clone(),
            requested_max_tokens: (route.payload.max_tokens > 0)
                .then_some(route.payload.max_tokens),
            downstream_stop_reason: None,
            upstream_model: route.upstream_model.clone(),
            external_outbound_model,
            model_resolution_source: route.model_resolution_source.clone(),
            model_resolution_note: route.model_resolution_note.clone(),
            conversation_id: crate::anthropic::converter::extract_stable_conversation_id(
                &route.payload,
            ),
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

fn external_pool_prepare_request(
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
) -> Result<PreparedExternalRequest, ExternalPoolError> {
    let mut value = match serde_json::to_value(&route.payload) {
        Ok(value) => value,
        Err(_) => match serde_json::from_slice::<serde_json::Value>(&route.raw_body) {
            Ok(value) => value,
            Err(_) => {
                return Ok(PreparedExternalRequest {
                    body: route.raw_body.clone(),
                    outbound_model: None,
                });
            }
        },
    };

    let outbound_model = external_pool_outbound_model(route, pool, &value)?;
    if let Some(outbound_model) = outbound_model.as_deref() {
        if value.get("model").and_then(|model| model.as_str()) != Some(outbound_model) {
            value["model"] = serde_json::Value::String(outbound_model.to_string());
        }
    }
    normalize_external_pool_thinking_value(&mut value);

    let body = serde_json::to_vec(&value)
        .map(Bytes::from)
        .unwrap_or_else(|_| route.raw_body.clone());
    Ok(PreparedExternalRequest {
        body,
        outbound_model,
    })
}

fn external_pool_outbound_model(
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
    value: &serde_json::Value,
) -> Result<Option<String>, ExternalPoolError> {
    let original_model = value
        .get("model")
        .and_then(|model| model.as_str())
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .or_else(|| {
            let model = route.payload.model.trim();
            (!model.is_empty()).then_some(model)
        });
    let processed_model = route
        .upstream_model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .or(original_model);
    let fallback_transform = (pool.normalize_model_version_dots
        && matches!(
            pool.model_mapping_mode,
            ExternalPoolModelMappingMode::DirectMapping
                | ExternalPoolModelMappingMode::ProcessedMapping
        ))
    .then_some(normalize_external_pool_outbound_model as fn(&str) -> String);
    let result = process_model(
        ModelProcessingInput {
            original_model,
            processed_model,
        },
        ModelProcessingConfig {
            mode: pool.model_mapping_mode.processing_mode(),
            rules: &pool.model_mapping_rules,
            require_mapping_match: pool.model_mapping_require_match,
            fallback_transform,
        },
    )
    .map_err(|err| external_pool_model_processing_error(pool, err))?;
    Ok(Some(result.model))
}

pub fn normalize_external_pool_model_mapping_rules(
    rules: Vec<ModelMappingRule>,
) -> Vec<ModelMappingRule> {
    rules
        .into_iter()
        .filter_map(|mut rule| {
            rule.source = rule.source.trim().to_ascii_lowercase();
            rule.target = rule.target.trim().to_string();
            rule.note = rule.note.and_then(|value| {
                let value = value.trim().to_string();
                (!value.is_empty()).then_some(value)
            });
            (!rule.source.is_empty() && !rule.target.is_empty()).then_some(rule)
        })
        .collect()
}

fn normalize_external_pool_outbound_model(model: &str) -> String {
    let trimmed = model.trim();
    if !trimmed.starts_with("claude-") || !trimmed.contains('.') {
        return trimmed.to_string();
    }

    let chars: Vec<char> = trimmed.chars().collect();
    let mut out = String::with_capacity(trimmed.len());
    for (idx, ch) in chars.iter().enumerate() {
        if *ch == '.'
            && idx > 0
            && idx + 1 < chars.len()
            && chars[idx - 1].is_ascii_digit()
            && chars[idx + 1].is_ascii_digit()
        {
            out.push('-');
        } else {
            out.push(*ch);
        }
    }
    out
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

fn external_pool_model_processing_error(
    pool: &ExternalPool,
    err: ModelProcessingError,
) -> ExternalPoolError {
    match err {
        ModelProcessingError::MissingModel => ExternalPoolError {
            status: Some(StatusCode::BAD_REQUEST),
            message: format!("external pool #{} model is missing", pool.id),
            retryable: false,
            auto_disable_reason: None,
            cooldown: None,
            response_body: None,
        },
        ModelProcessingError::MappingMiss { model } => ExternalPoolError {
            status: Some(StatusCode::BAD_GATEWAY),
            message: format!(
                "external pool #{} requires model mapping match, but no rule matched model {}",
                pool.id, model
            ),
            retryable: true,
            auto_disable_reason: None,
            cooldown: Some((Duration::ZERO, "model_mapping_miss".to_string())),
            response_body: None,
        },
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
    if route.payload_guard_retry_config.is_none() {
        return false;
    }
    if err.status != Some(StatusCode::BAD_REQUEST) {
        return false;
    }
    external_payload_too_long_message(&err.message)
        || err
            .response_body
            .as_ref()
            .is_some_and(|body| external_payload_too_long_message(&String::from_utf8_lossy(body)))
}

fn external_payload_too_long_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("context window is full")
        || lower.contains("input is too long")
        || lower.contains("content_length_exceeds_threshold")
        || lower.contains("request payload is too large")
        || lower.contains("payload is too large")
}

fn external_payload_guard_retry_route(
    route: &ExternalRouteRequest,
) -> Option<ExternalRouteRequest> {
    let config = route.payload_guard_retry_config?;
    let mut payload = route.payload.clone();
    let (body, report) = match guard_anthropic_messages_request_reusing_body(
        &mut payload,
        config,
        &route.raw_body,
    ) {
        Ok(result) => result,
        Err(err) => {
            tracing::warn!(
                request_id = %route.request_id,
                error = %err,
                "external pool payload guard retry failed to build trimmed request"
            );
            return None;
        }
    };
    let breakdown = breakdown_anthropic_messages_request(&payload, body.len());
    let mut next = route.clone();
    next.raw_body = body;
    next.payload = payload;
    next.payload_breakdown = Some(breakdown);
    next.payload_guard_report = Some(report);
    next.payload_guard_retry_config = None;
    Some(next)
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

    if let Some(projected) = project_usage_value(usage, projection) {
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

fn maybe_project_sse_event(
    event: &[u8],
    projection: Option<&ExternalUsageProjectionContext>,
    capture: Option<&Arc<SyncMutex<ExternalUsageCapture>>>,
    stream_error_mask: Option<&ExternalStreamErrorMask>,
) -> Vec<u8> {
    if let Some(masked) = maybe_mask_external_stream_error_event(event, capture, stream_error_mask)
    {
        return masked;
    }
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
        let Some(usage) = value.get_mut("usage") else {
            out.extend_from_slice(line.as_bytes());
            continue;
        };
        let raw_usage = cache_usage_from_value(usage);
        let Some(projected_usage) = project_usage_value(usage, projection) else {
            update_external_usage_capture(capture, raw_usage, raw_usage, raw_usage, false);
            out.extend_from_slice(line.as_bytes());
            continue;
        };
        update_external_usage_capture(
            capture,
            raw_usage,
            Some(projected_usage.shaped),
            Some(projected_usage.reported),
            true,
        );
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

fn drain_projected_sse_events(
    buffer: &mut Vec<u8>,
    projection: Option<&ExternalUsageProjectionContext>,
    capture: Option<&Arc<SyncMutex<ExternalUsageCapture>>>,
    stream_error_mask: Option<&ExternalStreamErrorMask>,
) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some((idx, delimiter_len)) = find_sse_event_delimiter(buffer) {
        let end = idx + delimiter_len;
        let event = buffer.drain(..end).collect::<Vec<u8>>();
        out.extend(maybe_project_sse_event(
            &event,
            projection,
            capture,
            stream_error_mask,
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
    shaped: CacheUsage,
    reported: CacheUsage,
}

fn project_usage_value(
    usage: &mut serde_json::Value,
    projection: Option<&ExternalUsageProjectionContext>,
) -> Option<ProjectedExternalUsage> {
    let projection = projection?;
    if projection.mode != ExternalPoolUsageProjectionMode::CurrentPathPolicy {
        return None;
    }
    let output_tokens = usage_i32(usage, "output_tokens");
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
    projection.mark_committed(computed);
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
        .map(|policy| controlled.with_reported_cache_usage_policy_and_raw(policy, raw_usage))
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
    obj.remove("cache_creation");
    Some(ProjectedExternalUsage {
        shaped,
        reported: projected,
    })
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
    let cache_creation_input_tokens = usage_i32(value, "cache_creation_input_tokens");
    let cache_read_input_tokens = usage_i32(value, "cache_read_input_tokens");
    let cache_creation_5m_input_tokens = usage_i32(value, "cache_creation_5m_input_tokens");
    let cache_creation_1h_input_tokens = usage_i32(value, "cache_creation_1h_input_tokens");
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
    Some(external_pool_billing(
        route,
        pool,
        raw,
        shaped,
        reported,
        capture.projected,
    ))
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
    raw_usage: CacheUsage,
    shaped_usage: CacheUsage,
    reported_usage: CacheUsage,
    usage_projection_applied: bool,
) -> ExternalPoolBilling {
    let pricing_model = route
        .upstream_model
        .as_deref()
        .unwrap_or(&route.payload.model);
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
    }
}

fn build_external_usage_projection_context(
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
    uplift_percent: u32,
    output_uplift_min_tokens: i32,
    output_uplift_percent: u32,
) -> Option<ExternalUsageProjectionContext> {
    if pool.usage_projection_mode != ExternalPoolUsageProjectionMode::CurrentPathPolicy {
        return None;
    }

    let model = route
        .upstream_model
        .clone()
        .unwrap_or_else(|| route.payload.model.clone());
    let prompt_cache_supported = route
        .model_capabilities
        .supports_prompt_caching_for(&model)
        .unwrap_or(true);

    let raw_input_tokens = route.request_input_tokens;
    let cache_state_enabled = route.prompt_cache_strategy_type != PromptCacheStrategyType::NoCache
        && prompt_cache_supported;
    let scope = cache_state_enabled
        .then(|| external_prompt_cache_scope(route, pool, &model))
        .flatten();
    let (profile, kiro_rs_tool_prompt_cache_plan, simulated_usage) = match route
        .prompt_cache_strategy_type
    {
        PromptCacheStrategyType::CurrentHighCache
            if prompt_cache_supported
                && route.prompt_cache_simulation_mode == PromptCacheSimulationMode::HighCache =>
        {
            let profile = route.prompt_cache.build_high_cache_profile_for_model(
                &route.payload,
                raw_input_tokens,
                &model,
            );
            let prompt_usage = route.prompt_cache.compute_with_bounds(
                scope.clone(),
                profile.as_ref(),
                route.prompt_cache_target_read_ratio,
                route.prompt_cache_bounds,
            );
            let simulated_usage = profile.as_ref().and_then(|profile| {
                CacheSimulation::from_prompt_cache_with_ratio_and_amplification(
                    prompt_usage,
                    route.prompt_cache_target_read_ratio,
                    external_cache_amplification(route, profile),
                )
            });
            (profile, None, simulated_usage)
        }
        PromptCacheStrategyType::KiroRsTool if prompt_cache_supported => {
            let plan = route.prompt_cache.compute_kiro_rs_tool_with_bounds(
                scope.clone(),
                &route.payload,
                raw_input_tokens,
                &model,
                route.prompt_cache_bounds,
                route.kiro_rs_tool_cache_policy,
            );
            let policy = route.kiro_rs_tool_cache_policy.normalized();
            let simulated_usage =
                CacheSimulation::from_prompt_cache_split_input_with_reported_input_range(
                    plan.usage(),
                    policy.reported_input_min_tokens,
                    policy.reported_input_max_tokens,
                    plan.cache_jitter_seed(),
                );
            (None, Some(plan), simulated_usage)
        }
        _ => (None, None, None),
    };
    let reported_usage = route.reported_usage.policy_for_path(&route.endpoint);
    let reported_policy = match route.prompt_cache_strategy_type {
        PromptCacheStrategyType::NoCache if reported_usage.enabled => {
            ReportedCacheUsagePolicy::from_path_policy(reported_usage, fastrand::u64(..))
        }
        PromptCacheStrategyType::CurrentHighCache
            if prompt_cache_supported
                && route.prompt_cache_simulation_mode == PromptCacheSimulationMode::HighCache =>
        {
            ReportedCacheUsagePolicy::from_path_policy(
                reported_usage,
                profile
                    .as_ref()
                    .map(|profile| profile.cache_jitter_seed())
                    .unwrap_or(0)
                    ^ fastrand::u64(..),
            )
        }
        PromptCacheStrategyType::CurrentHighCache
        | PromptCacheStrategyType::KiroRsTool
        | PromptCacheStrategyType::NoCache => None,
    };
    Some(ExternalUsageProjectionContext {
        mode: pool.usage_projection_mode,
        raw_input_tokens,
        cache_state_enabled,
        credential_key: Some(format!("external_pool:{}", pool.id)),
        model,
        simulated_usage,
        reported_policy,
        scope,
        prompt_cache: route.prompt_cache.clone(),
        prompt_cache_profile: profile,
        kiro_rs_tool_prompt_cache_plan,
        prompt_cache_target_read_ratio: route.prompt_cache_target_read_ratio,
        prompt_cache_bounds: route.prompt_cache_bounds,
        prompt_cache_creation_controller: route.prompt_cache_creation_controller.clone(),
        prompt_cache_creation_control: route.prompt_cache_creation_control,
        uplift_percent,
        output_uplift_min_tokens: output_uplift_min_tokens.max(0),
        output_uplift_percent: output_uplift_percent.min(200),
        state: Arc::new(SyncMutex::new(ExternalUsageProjectionState::default())),
    })
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

fn external_prompt_cache_scope(
    route: &ExternalRouteRequest,
    _pool: &ExternalPool,
    _model: &str,
) -> Option<PromptCacheScope> {
    Some(PromptCacheScope::new(
        crate::anthropic::converter::extract_stable_conversation_id(&route.payload)?,
        route.prompt_cache_route_namespace.clone(),
    ))
}

fn external_cache_amplification(
    route: &ExternalRouteRequest,
    profile: &PromptCacheProfile,
) -> Option<CacheAmplification> {
    Some(CacheAmplification::new(
        route.prompt_cache_token_scale,
        route.prompt_cache_max_simulated_input_tokens,
        route.prompt_cache_cap_jitter_min_tokens,
        route.prompt_cache_cap_jitter_max_tokens,
        route.prompt_cache_scale_min_input_tokens,
        profile.cache_jitter_seed(),
    ))
}

impl ExternalUsageProjectionContext {
    fn mark_committed(&self, usage: CacheUsage) {
        let mut state = self.state.lock();
        state.committed_controlled_usage = Some(usage);
    }

    fn record_success(&self) {
        let Some(usage) = self.state.lock().committed_controlled_usage else {
            return;
        };
        if !self.cache_state_enabled {
            return;
        }
        if self.reported_policy.is_some() {
            let _ = self
                .prompt_cache_creation_controller
                .apply_success_with_context(
                    self.scope.as_ref(),
                    self.prompt_cache_creation_control,
                    usage,
                    self.credential_key.as_deref(),
                    Some(self.model.as_str()),
                );
        }
        if let Some(plan) = self.kiro_rs_tool_prompt_cache_plan.as_ref() {
            self.prompt_cache.commit_kiro_rs_tool_success_with_bounds(
                self.scope.clone(),
                plan,
                self.prompt_cache_bounds,
            );
        } else {
            self.prompt_cache.update_with_bounds(
                self.scope.clone(),
                self.prompt_cache_profile.as_ref(),
                self.prompt_cache_target_read_ratio,
                self.prompt_cache_bounds,
            );
        }
    }
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
mod tests {
    use super::*;
    use crate::anthropic::types::{Message, Metadata, OutputConfig, SystemMessage, Thinking};
    use crate::model::config::Config;
    use crate::model::config::{ReportedUsageFieldPolicy, ReportedUsagePathPolicy};

    fn test_postgres_config() -> Option<Config> {
        let url = std::env::var("KIRO_RS_TEST_POSTGRES_URL").ok()?;
        let mut config = Config::default();
        config.postgres.url = Some(url);
        config.postgres.max_connections = 2;
        Some(config)
    }

    fn test_redis_config() -> Option<Config> {
        let url = std::env::var("KIRO_RS_TEST_REDIS_URL").ok()?;
        let mut config = Config::default();
        config.redis.url = Some(url);
        config.redis.key_prefix = format!("kiro_rs:test:external_pool:{}", uuid::Uuid::new_v4());
        Some(config)
    }

    async fn test_external_pool_manager() -> Option<(ExternalPoolManager, Arc<PostgresStore>)> {
        let Some(postgres_config) = test_postgres_config() else {
            eprintln!("跳过外部备用池集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return None;
        };
        let Some(redis_config) = test_redis_config() else {
            eprintln!("跳过外部备用池集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return None;
        };
        let postgres = Arc::new(PostgresStore::connect_test(&postgres_config).await.unwrap());
        let redis = Arc::new(RedisStore::connect(&redis_config).await.unwrap());
        Some((ExternalPoolManager::new(postgres.clone(), redis), postgres))
    }

    fn create_pool_request(name: &str, priority: i32, enabled: bool) -> CreateExternalPoolRequest {
        CreateExternalPoolRequest {
            name: name.to_string(),
            base_url: format!("https://{}.example.test", name),
            api_key: format!("sk-{}", name),
            auth_type: ExternalPoolAuthType::Bearer,
            enabled,
            priority,
            max_concurrent_requests: 1,
            usage_projection_mode: ExternalPoolUsageProjectionMode::PassThrough,
            auto_disable_policy: ExternalPoolAutoDisablePolicy::Inherit,
            preserve_path: true,
            normalize_model_version_dots: false,
            model_mapping_mode: ExternalPoolModelMappingMode::ProcessedMapping,
            model_mapping_require_match: false,
            model_mapping_rules: Vec::new(),
            notes: None,
        }
    }

    #[test]
    fn pool_auto_disable_policy_can_override_global_switch() {
        let mut config = ExternalPoolsConfig::default();
        config.external_pool_auto_disable_enabled = false;

        assert!(!pool_auto_disable_policy_enabled(
            ExternalPoolAutoDisablePolicy::Inherit,
            &config
        ));
        assert!(pool_auto_disable_policy_enabled(
            ExternalPoolAutoDisablePolicy::Enabled,
            &config
        ));
        assert!(!pool_auto_disable_policy_enabled(
            ExternalPoolAutoDisablePolicy::Disabled,
            &config
        ));

        config.external_pool_auto_disable_enabled = true;
        assert!(pool_auto_disable_policy_enabled(
            ExternalPoolAutoDisablePolicy::Inherit,
            &config
        ));
    }

    #[test]
    fn external_pool_default_retry_attempts_cover_eligible_pools_and_payload_guard_retry() {
        assert_eq!(
            PoolAvailabilitySnapshot {
                eligible_pools: 0,
                ..PoolAvailabilitySnapshot::default()
            }
            .default_retry_attempts(false),
            1
        );
        assert_eq!(
            PoolAvailabilitySnapshot {
                eligible_pools: 2,
                ..PoolAvailabilitySnapshot::default()
            }
            .default_retry_attempts(false),
            2
        );
        assert_eq!(
            PoolAvailabilitySnapshot {
                eligible_pools: 2,
                ..PoolAvailabilitySnapshot::default()
            }
            .default_retry_attempts(true),
            3
        );
    }

    #[test]
    fn external_pool_skip_reason_respects_enabled_switches_and_capacity() {
        let mut config = ExternalPoolsConfig::default();
        config.external_pools_enabled = false;
        let mut pool = test_pool("https://pool.example.test", true);

        assert_eq!(
            ExternalPoolManager::skip_reason(&pool, 0, 0, 0, &config).as_deref(),
            Some("external_pools_disabled")
        );

        config.external_pools_enabled = true;
        pool.enabled = false;
        assert_eq!(
            ExternalPoolManager::skip_reason(&pool, 0, 0, 0, &config).as_deref(),
            Some("disabled")
        );

        pool.enabled = true;
        assert_eq!(
            ExternalPoolManager::skip_reason(&pool, 0, 0, 3, &config).as_deref(),
            Some("cooldown")
        );

        pool.max_concurrent_requests = 2;
        assert_eq!(
            ExternalPoolManager::skip_reason(&pool, 2, 0, 0, &config).as_deref(),
            Some("pool_concurrency_full")
        );

        config.external_pool_global_max_concurrent_requests = 4;
        assert_eq!(
            ExternalPoolManager::skip_reason(&pool, 0, 4, 0, &config).as_deref(),
            Some("global_concurrency_full")
        );

        assert!(ExternalPoolManager::skip_reason(&pool, 0, 3, 0, &config).is_none());
    }

    #[test]
    fn external_pool_candidate_selection_handles_multiple_backup_pools() {
        let mut primary = test_pool("https://primary.example.test", true);
        primary.id = 11;
        primary.priority = 1;
        primary.max_concurrent_requests = 1;
        let mut secondary = test_pool("https://secondary.example.test", true);
        secondary.id = 22;
        secondary.priority = 2;
        secondary.max_concurrent_requests = 1;
        let mut tertiary = test_pool("https://tertiary.example.test", true);
        tertiary.id = 33;
        tertiary.priority = 3;
        tertiary.max_concurrent_requests = 1;

        let selected = select_external_pool_candidate(vec![
            (secondary.clone(), 0),
            (tertiary.clone(), 0),
            (primary.clone(), 0),
        ])
        .expect("candidate should be selected");
        assert_eq!(selected.id, primary.id);

        let selected =
            select_external_pool_candidate(vec![(secondary.clone(), 0), (tertiary.clone(), 0)])
                .expect("fallback candidate should be selected when primary is excluded/full");
        assert_eq!(selected.id, secondary.id);

        primary.priority = 1;
        secondary.priority = 1;
        primary.max_concurrent_requests = 2;
        secondary.max_concurrent_requests = 4;
        let selected =
            select_external_pool_candidate(vec![(primary.clone(), 1), (secondary.clone(), 1)])
                .expect("lower same-priority load should be selected");
        assert_eq!(selected.id, secondary.id);
    }

    #[tokio::test]
    async fn external_pool_manager_respects_disabled_switch_and_disabled_pools() {
        let Some((manager, postgres)) = test_external_pool_manager().await else {
            return;
        };
        let mut config = ExternalPoolsConfig::default();
        config.external_pools_enabled = false;

        let disabled = postgres
            .create_external_pool(create_pool_request("external-disabled", 1, false))
            .await
            .unwrap();
        let enabled = postgres
            .create_external_pool(create_pool_request("external-enabled", 2, true))
            .await
            .unwrap();

        assert!(!manager.has_available_pool(&config).await);
        config.external_pools_enabled = true;
        let selected = manager
            .select_pool(&HashSet::new(), &config)
            .await
            .expect("enabled external pool should be selected");
        assert_eq!(selected.id, enabled.id);
        assert_ne!(selected.id, disabled.id);

        postgres.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn external_pool_manager_selects_multiple_pools_by_priority_and_capacity() {
        let Some((manager, postgres)) = test_external_pool_manager().await else {
            return;
        };
        let mut config = ExternalPoolsConfig::default();
        config.external_pools_enabled = true;
        config.external_pool_global_max_concurrent_requests = 0;

        let primary = postgres
            .create_external_pool(create_pool_request("external-primary", 1, true))
            .await
            .unwrap();
        let secondary = postgres
            .create_external_pool(create_pool_request("external-secondary", 2, true))
            .await
            .unwrap();
        let tertiary = postgres
            .create_external_pool(create_pool_request("external-tertiary", 3, true))
            .await
            .unwrap();

        let first = manager
            .select_pool(&HashSet::new(), &config)
            .await
            .expect("primary pool should be selected first");
        assert_eq!(first.id, primary.id);

        let first_lease = match manager.acquire_pool(&primary, &config).await {
            PoolAcquireResult::Acquired(lease) => lease,
            PoolAcquireResult::Unavailable(_) => panic!("primary pool lease should be acquired"),
        };
        let second = manager
            .select_pool(&HashSet::new(), &config)
            .await
            .expect("secondary pool should be selected when primary is full");
        assert_eq!(second.id, secondary.id);

        let second_lease = match manager.acquire_pool(&secondary, &config).await {
            PoolAcquireResult::Acquired(lease) => lease,
            PoolAcquireResult::Unavailable(_) => panic!("secondary pool lease should be acquired"),
        };
        let third = manager
            .select_pool(&HashSet::new(), &config)
            .await
            .expect("tertiary pool should be selected when higher-priority pools are full");
        assert_eq!(third.id, tertiary.id);

        drop(first_lease);
        drop(second_lease);
        let mut after_release = None;
        for _ in 0..20 {
            if let Some(pool) = manager.select_pool(&HashSet::new(), &config).await {
                after_release = Some(pool);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let after_release = after_release.expect("primary should be selected again after release");
        assert_eq!(after_release.id, primary.id);

        postgres.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn external_pool_manager_distinguishes_global_capacity_from_no_pool() {
        let Some((manager, postgres)) = test_external_pool_manager().await else {
            return;
        };
        let mut config = ExternalPoolsConfig::default();
        config.external_pools_enabled = true;
        config.external_pool_global_max_concurrent_requests = 1;

        let primary = postgres
            .create_external_pool(create_pool_request("external-global-a", 1, true))
            .await
            .unwrap();
        let secondary = postgres
            .create_external_pool(create_pool_request("external-global-b", 2, true))
            .await
            .unwrap();

        let lease = match manager.acquire_pool(&primary, &config).await {
            PoolAcquireResult::Acquired(lease) => lease,
            PoolAcquireResult::Unavailable(_) => panic!("primary pool lease should be acquired"),
        };

        assert!(
            manager
                .select_pool(&HashSet::new(), &config)
                .await
                .is_none()
        );
        assert!(manager.has_eligible_pool(&config).await);
        let snapshot = manager
            .pool_availability_snapshot(&HashSet::new(), &config)
            .await;
        assert_eq!(snapshot.eligible_pools, 2);
        assert_eq!(snapshot.available_pools, 0);
        assert_eq!(snapshot.temporary_unavailable_pools, 2);
        assert_eq!(snapshot.wait_reason, Some(PoolCapacityWaitReason::Full));

        drop(lease);
        let mut selected = None;
        for _ in 0..20 {
            if let Some(pool) = manager.select_pool(&HashSet::new(), &config).await {
                selected = Some(pool);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let selected = selected.expect("pool should become selectable after global lease release");
        assert!(selected.id == primary.id || selected.id == secondary.id);

        postgres.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn external_pool_manager_uncached_snapshot_detects_full_pool_after_available_cache() {
        let Some((manager, postgres)) = test_external_pool_manager().await else {
            return;
        };
        let mut config = ExternalPoolsConfig::default();
        config.external_pools_enabled = true;
        config.external_pool_global_max_concurrent_requests = 0;

        let pool = postgres
            .create_external_pool(create_pool_request("external-stale-cache", 1, true))
            .await
            .unwrap();

        let cached_available = manager
            .pool_availability_snapshot(&HashSet::new(), &config)
            .await;
        assert_eq!(cached_available.eligible_pools, 1);
        assert_eq!(cached_available.available_pools, 1);

        let lease = match manager.acquire_pool(&pool, &config).await {
            PoolAcquireResult::Acquired(lease) => lease,
            PoolAcquireResult::Unavailable(_) => panic!("pool lease should be acquired"),
        };
        let selection = manager
            .scan_pool_availability_uncached(&HashSet::new(), &config, true)
            .await;
        assert!(selection.selected_pool.is_none());
        let uncached_full = selection.availability;
        assert_eq!(uncached_full.eligible_pools, 1);
        assert_eq!(uncached_full.available_pools, 0);
        assert_eq!(uncached_full.temporary_unavailable_pools, 1);
        assert_eq!(
            uncached_full.wait_reason,
            Some(PoolCapacityWaitReason::Full)
        );

        drop(lease);
        postgres.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn external_pool_error_response_masks_raw_error_body_with_trace_id() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        headers.insert(
            HeaderName::from_static("anthropic-request-id"),
            HeaderValue::from_static("req_upstream"),
        );
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("999"));
        let body = Bytes::from_static(
            br#"{"error":{"type":"invalid_request_error","message":"bad input"},"type":"error"}"#,
        );

        let err = classify_external_error(
            StatusCode::BAD_REQUEST,
            body.clone(),
            headers,
            &ExternalPoolsConfig::default(),
        );
        let response =
            external_final_error_from_error(None, Vec::new(), &err, "req_01gatewayerror")
                .into_response("req_gateway");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response
                .headers()
                .get(HeaderName::from_static("request-id"))
                .unwrap(),
            "req_gateway"
        );
        assert_eq!(
            response
                .headers()
                .get(HeaderName::from_static("request-id"))
                .unwrap(),
            "req_gateway"
        );
        assert!(
            response
                .headers()
                .get(HeaderName::from_static("x-kiro-rs-error-id"))
                .and_then(|value| value.to_str().ok())
                .is_some_and(|error_id| error_id.starts_with("req_01"))
        );
        assert!(response.headers().get(header::CONTENT_LENGTH).is_none());

        let actual = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read external error body");
        let value: serde_json::Value = serde_json::from_slice(&actual).expect("json envelope");
        assert_eq!(value["error"]["type"], "invalid_request_error");
        let message = value["error"]["message"].as_str().unwrap();
        assert!(message.contains(envelope::PUBLIC_INVALID_REQUEST_MESSAGE));
        assert!(message.contains("error ID: req_01"));
        assert!(!message.contains("bad input"));
        assert!(!message.contains("invalid_request_error"));
        assert_public_message_hides_internal_routing(message);
        assert_eq!(value["request_id"], "req_gateway");
    }

    #[test]
    fn external_public_error_masks_raw_message() {
        let public_error = external_public_error_from_parts(
            StatusCode::BAD_GATEWAY,
            "server_error",
            true,
            "provider says buy credits at https://example.invalid",
            "req_01public",
        );

        assert_eq!(public_error.status_code, StatusCode::BAD_GATEWAY.as_u16());
        assert_eq!(public_error.error_type, "api_error");
        assert!(
            public_error
                .message
                .contains(envelope::PUBLIC_TEMPORARY_FAILURE_MESSAGE)
        );
        assert!(public_error.message.contains("error ID: req_01public"));
        assert!(!public_error.message.contains("buy credits"));
    }

    #[tokio::test]
    async fn external_pool_retryable_final_error_uses_gateway_error_envelope() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        let body = Bytes::from_static(
            br#"{"error":{"type":"rate_limit_error","message":"slow down"},"type":"error"}"#,
        );

        let err = classify_external_error(
            StatusCode::TOO_MANY_REQUESTS,
            body.clone(),
            headers,
            &ExternalPoolsConfig::default(),
        );
        assert!(err.retryable);
        assert_eq!(error_type_for_external_error(&err), "rate_limit");

        let response =
            external_final_error_from_error(None, Vec::new(), &err, "req_01gatewayerror")
                .into_response("req_gateway");

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(
            response
                .headers()
                .get(HeaderName::from_static("x-kiro-rs-error-id"))
                .and_then(|value| value.to_str().ok())
                .is_some_and(|error_id| error_id.starts_with("req_01"))
        );
        let actual = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read external final retryable body");
        let value: serde_json::Value = serde_json::from_slice(&actual).expect("json envelope");
        assert_eq!(value["error"]["type"], "rate_limit_error");
        let message = value["error"]["message"].as_str().unwrap();
        assert!(message.contains(envelope::PUBLIC_RATE_LIMIT_MESSAGE));
        assert!(message.contains("error ID: req_01"));
        assert!(!message.contains("slow down"));
        assert!(!message.contains("rate_limit_error"));
        assert_public_message_hides_internal_routing(message);
        assert_eq!(value["request_id"], "req_gateway");
    }

    #[test]
    fn external_pool_error_classifies_nested_rate_limit_body() {
        let err = classify_external_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            Bytes::from_static(br#"{"error":"SERVICE_REQUEST_RATE_EXCEEDED: Too many requests"}"#),
            HeaderMap::new(),
            &ExternalPoolsConfig::default(),
        );

        assert!(err.retryable);
        assert_eq!(error_type_for_external_error(&err), "rate_limit");
        assert!(err.auto_disable_reason.is_none());
    }

    #[test]
    fn external_error_diagnostics_records_status_and_non_duplicate_metadata() {
        let mut route = test_route("claude-sonnet-4-6");
        route.error_id = "req_01diagnostic".to_string();
        let err = classify_external_error(
            StatusCode::TOO_MANY_REQUESTS,
            Bytes::from_static(br#"{"error":{"type":"rate_limit_error","message":"slow down"}}"#),
            HeaderMap::new(),
            &ExternalPoolsConfig::default(),
        );
        let response_error_type = anthropic_error_type_for_external_error(&err);
        let (record_message, message_truncated) = external_error_record_message(&err);
        let diagnostics =
            external_error_diagnostics(&route, &err, response_error_type, message_truncated);

        assert_eq!(record_message, err.message);
        assert_eq!(diagnostics.status_code, Some(429));
        assert_eq!(diagnostics.source.as_deref(), Some("external_account"));
        assert_eq!(diagnostics.error_id.as_deref(), Some("req_01diagnostic"));
        let metadata = diagnostics.metadata.unwrap();
        assert_eq!(metadata["responseErrorType"], "rate_limit_error");
        assert_eq!(metadata["retryable"], true);
        assert_eq!(metadata["cooldownReason"], "rate_limit");
        for duplicate_key in [
            "message",
            "rawMessage",
            "attempts",
            "poolId",
            "poolName",
            "requestId",
            "errorId",
            "statusCode",
        ] {
            assert!(
                metadata.get(duplicate_key).is_none(),
                "metadata duplicated {duplicate_key}: {metadata}"
            );
        }
    }

    #[test]
    fn external_pool_error_classifies_database_busy_without_auto_disable() {
        let err = classify_external_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            Bytes::from_static(br#"database is locked (SQLITE_BUSY)"#),
            HeaderMap::new(),
            &ExternalPoolsConfig::default(),
        );

        assert!(err.retryable);
        assert_eq!(error_type_for_external_error(&err), "database_busy");
        assert!(err.auto_disable_reason.is_none());
    }

    #[test]
    fn external_pool_error_classifies_channel_disabled_for_optional_auto_disable() {
        let config = ExternalPoolsConfig::default();
        let err = classify_external_error(
            StatusCode::BAD_GATEWAY,
            Bytes::from_static(br#"channel affinity has been disabled"#),
            HeaderMap::new(),
            &config,
        );

        assert!(err.retryable);
        assert_eq!(err.auto_disable_reason.as_deref(), Some("channel_disabled"));
        assert_eq!(error_type_for_external_error(&err), "channel_disabled");
        assert!(auto_disable_reason_enabled(&config, "channel_disabled"));
    }

    #[test]
    fn external_pool_error_classifies_model_unavailable_as_retryable() {
        let err = classify_external_error(
            StatusCode::BAD_REQUEST,
            Bytes::from_static(br#"{"error":{"code":"model_not_found"}}"#),
            HeaderMap::new(),
            &ExternalPoolsConfig::default(),
        );

        assert!(err.retryable);
        assert_eq!(error_type_for_external_error(&err), "model_unavailable");
        assert!(err.auto_disable_reason.is_none());
    }

    #[test]
    fn external_payload_guard_retry_route_trims_and_disables_second_retry() {
        let mut route = test_route("claude-sonnet-4-6");
        let mut messages = Vec::new();
        for idx in 0..32 {
            messages.push(Message {
                role: "user".to_string(),
                content: serde_json::json!(format!("history {} {}", idx, "x".repeat(700))),
            });
            messages.push(Message {
                role: "assistant".to_string(),
                content: serde_json::json!([{
                    "type": "text",
                    "text": format!("answer {} {}", idx, "y".repeat(500)),
                }]),
            });
        }
        messages.push(Message {
            role: "user".to_string(),
            content: serde_json::json!("current question"),
        });
        route.payload.messages = messages;
        let body = serde_json::to_string(&route.payload).expect("serialize route payload");
        route.raw_body = Bytes::from(body);
        route.payload_guard_retry_config = Some(PayloadGuardConfig {
            enabled: true,
            max_bytes: 8_000,
            trim_history: true,
            shaping: crate::model::config::PayloadShapingConfig::default(),
        });
        let err = classify_external_error(
            StatusCode::BAD_REQUEST,
            Bytes::from_static(br#"{"error":{"message":"Context window is full"}}"#),
            HeaderMap::new(),
            &ExternalPoolsConfig::default(),
        );

        assert!(should_retry_external_payload_guard(&route, &err));
        let retry_route = external_payload_guard_retry_route(&route).expect("retry route");

        assert!(retry_route.raw_body.len() <= 8_000);
        assert!(retry_route.payload_guard_retry_config.is_none());
        assert!(
            retry_route
                .payload_guard_report
                .as_ref()
                .is_some_and(|report| report.trimmed_history_entries > 0)
        );
        assert_eq!(
            retry_route.payload.messages.last().unwrap().content,
            serde_json::json!("current question")
        );
    }

    #[tokio::test]
    async fn external_capacity_scheduler_error_uses_request_id_and_error_type() {
        let route = ExternalRouteRequest {
            raw_body: Bytes::new(),
            headers: HeaderMap::new(),
            endpoint: "/v1/messages".to_string(),
            payload: MessagesRequest {
                model: "claude-sonnet-4-5-20250929".to_string(),
                max_tokens: 8,
                messages: Vec::new(),
                stream: false,
                system: None,
                tools: None,
                tool_choice: None,
                thinking: None,
                output_config: None,
                metadata: None,
            },
            request_input_tokens: 1,
            upstream_model: None,
            model_resolution_source: None,
            model_resolution_note: None,
            route_subtype: UsageRouteSubtype::ExternalDirectPolicy,
            fallback_reason: None,
            direct_policy_reason: None,
            local_attempted: false,
            local_preflight: None,
            local_attempts: Vec::new(),
            reported_usage: ReportedUsageConfig::default(),
            prompt_cache: Arc::new(PromptCacheTracker::default()),
            prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
            prompt_cache_strategy_type: PromptCacheStrategyType::CurrentHighCache,
            prompt_cache_simulation_mode: PromptCacheSimulationMode::HighCache,
            prompt_cache_route_namespace: None,
            prompt_cache_target_read_ratio: 0.98,
            prompt_cache_token_scale: 1.6,
            prompt_cache_max_simulated_input_tokens: 300_000,
            prompt_cache_cap_jitter_min_tokens: 12_000,
            prompt_cache_cap_jitter_max_tokens: 24_000,
            prompt_cache_scale_min_input_tokens: 20_000,
            prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
            prompt_cache_bounds: PromptCacheBounds::default(),
            kiro_rs_tool_cache_policy: KiroRsToolCachePolicy::default(),
            model_capabilities: Arc::new(ModelCapabilitiesCatalog::new()),
            pricing_catalog: Arc::new(PricingCatalog::new()),
            request_id: "req_external_capacity".to_string(),
            error_id: "req_01capacity".to_string(),
            recorder: Arc::new(crate::anthropic::usage::UsageRecorder::new(1)),
            started_at: Instant::now(),
            first_token_latency_ms: Arc::new(AtomicU64::new(0)),
            latency_trace: Arc::new(ExternalLatencyTraceState::default()),
            payload_breakdown: None,
            payload_guard_report: None,
            payload_guard_retry_config: None,
        };

        let (error_type, message) = external_capacity_error(PoolCapacityWaitReason::Full);
        let err = external_capacity_final_error(
            StatusCode::SERVICE_UNAVAILABLE,
            error_type,
            message,
            &route.error_id,
        );
        assert!(err.is_capacity_like());
        assert_eq!(err.route_error_type, "external_pool_capacity_full");
        let response = err.into_response(&route.request_id);

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response.headers().get("request-id").unwrap(),
            "req_external_capacity"
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read scheduler error body");
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["type"], "api_error");
        let message = body["error"]["message"].as_str().unwrap();
        assert!(message.contains(envelope::PUBLIC_TEMPORARY_FAILURE_MESSAGE));
        assert!(message.contains("error ID: req_01"));
        assert!(!message.contains("Request capacity is full"));
        assert_public_message_hides_internal_routing(message);
    }

    #[test]
    fn successful_external_html_response_is_protocol_error() {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html"));
        let body = Bytes::from_static(br#"<!doctype html><html><body>admin</body></html>"#);

        assert!(success_response_looks_like_html(&headers, &body));
        let err = success_protocol_error(
            &headers,
            Some(&body),
            &ExternalPoolsConfig::default(),
            "model endpoint returned an HTML response",
        );

        assert!(err.retryable);
        assert_eq!(err.status, Some(StatusCode::OK));
        assert_eq!(
            err.auto_disable_reason.as_deref(),
            Some("misconfigured_endpoint")
        );
        assert_eq!(
            error_type_for_external_error(&err),
            "misconfigured_endpoint"
        );
    }

    #[test]
    fn successful_external_error_body_is_treated_as_protocol_error() {
        let body = Bytes::from_static(
            br#"{"type":"error","error":{"type":"api_error","message":"raw pool failure"}}"#,
        );

        assert!(success_response_looks_like_error_body(&body));
        let err = success_error_body_protocol_error(&body, &ExternalPoolsConfig::default());

        assert!(err.retryable);
        assert_eq!(err.status, Some(StatusCode::OK));
        assert_eq!(err.response_body.as_deref(), Some(body.as_ref()));
        assert!(err.message.contains("success status"));
    }

    #[test]
    fn external_stream_error_event_is_masked_and_raw_event_is_recorded() {
        let capture = Arc::new(SyncMutex::new(ExternalUsageCapture::default()));
        let mask = ExternalStreamErrorMask {
            request_id: "req_stream_mask".to_string(),
            error_id: "req_01streammask".to_string(),
            pool_id: 7,
            pool_name: "pool-a".to_string(),
        };
        let event = br#"event: error
data: {"type":"error","error":{"type":"api_error","message":"raw external promo text"}}

"#;

        let masked = maybe_project_sse_event(event, None, Some(&capture), Some(&mask));
        let text = std::str::from_utf8(&masked).expect("masked event utf8");

        assert!(text.contains("event: error"));
        assert!(text.contains(envelope::PUBLIC_TEMPORARY_FAILURE_MESSAGE));
        assert!(text.contains("error ID: req_01streammask"));
        assert!(text.contains("req_stream_mask"));
        assert!(!text.contains("raw external promo text"));
        assert!(!text.contains("pool-a"));
        assert_public_message_hides_internal_routing(text);

        let recorded = capture
            .lock()
            .stream_error_message
            .clone()
            .expect("raw stream error recorded");
        assert!(recorded.contains("raw external promo text"));
        assert!(!recorded.contains("req_01streammask"));
    }

    fn assert_public_message_hides_internal_routing(message: &str) {
        let lower = message.to_ascii_lowercase();
        for forbidden in [
            "credential",
            "external pool",
            "external_pool",
            "fallback",
            "preflight",
            "备用池",
            "外部池",
            "凭据",
        ] {
            assert!(
                !lower.contains(forbidden),
                "public message leaked internal term {forbidden:?}: {message}"
            );
        }
    }

    #[test]
    fn external_latency_trace_records_stream_markers_without_changing_first_output_semantics() {
        let trace = ExternalLatencyTraceState::default();
        let started_at = Instant::now() - Duration::from_millis(25);

        trace.mark_upstream_header(started_at);
        trace.mark_first_upstream_chunk(started_at);

        let text_start = Bytes::from_static(
            b"event: content_block_start\ndata: {\"type\":\"content_block_start\",\"content_block\":{\"type\":\"text\"}}\n\n",
        );
        assert!(!external_stream_chunk_has_first_output(&text_start));

        let output = Bytes::from_static(
            b"event: ping\ndata: {}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n\n",
        );
        assert!(external_stream_chunk_has_first_output(&output));
        assert_eq!(count_external_stream_events_before_first_output(&output), 1);

        trace.mark_first_output(50, 1, 2);
        let snapshot = trace.snapshot().expect("latency trace snapshot");
        assert!(snapshot.upstream_header_ms.is_some());
        assert!(snapshot.first_upstream_chunk_ms.is_some());
        assert_eq!(snapshot.first_output_delta_ms, Some(50));
        assert_eq!(snapshot.chunks_before_first_output, Some(1));
        assert_eq!(snapshot.events_before_first_output, Some(2));
        assert!(snapshot.stream_gap_to_first_output_ms.is_some());
    }

    #[test]
    fn external_first_output_parser_uses_sse_json_semantics() {
        let empty_delta = Bytes::from_static(
            br#"event: content_block_delta
data: {"type":"content_block_delta","delta":{"type":"text_delta","text":""}}

"#,
        );
        assert!(!external_stream_chunk_has_first_output(&empty_delta));

        let text_then_tool_start = Bytes::from_static(
            br#"event: content_block_start
data: {"type":"content_block_start","content_block":{"type":"text"}}

event: content_block_start
data: {"type":"content_block_start","content_block":{"type":"tool_use","id":"toolu_1","name":"read","input":{}}}

"#,
        );
        assert!(external_stream_chunk_has_first_output(
            &text_then_tool_start
        ));
        assert_eq!(
            count_external_stream_events_before_first_output(&text_then_tool_start),
            1
        );

        let content_in_payload_string = Bytes::from_static(
            br#"event: message_delta
data: {"type":"message_delta","note":"content_block_delta"}

"#,
        );
        assert!(!external_stream_chunk_has_first_output(
            &content_in_payload_string
        ));
    }

    fn test_pool(base_url: &str, preserve_path: bool) -> ExternalPool {
        let now = Utc::now();
        ExternalPool {
            id: 1,
            name: "test".to_string(),
            base_url: base_url.to_string(),
            api_key: Some("sk-test".to_string()),
            masked_api_key: None,
            auth_type: ExternalPoolAuthType::Bearer,
            enabled: true,
            priority: 10,
            max_concurrent_requests: 10,
            usage_projection_mode: ExternalPoolUsageProjectionMode::PassThrough,
            auto_disable_policy: ExternalPoolAutoDisablePolicy::Inherit,
            auto_disabled: false,
            auto_disabled_reason: None,
            auto_disabled_at: None,
            auto_disabled_until: None,
            auto_disabled_last_error: None,
            preserve_path,
            normalize_model_version_dots: false,
            model_mapping_mode: ExternalPoolModelMappingMode::ProcessedMapping,
            model_mapping_require_match: false,
            model_mapping_rules: Vec::new(),
            notes: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn model_rule(source: &str, target: &str) -> ModelMappingRule {
        ModelMappingRule {
            enabled: true,
            source: source.to_string(),
            target: target.to_string(),
            kind: Default::default(),
            note: None,
        }
    }

    fn test_pool_with_model_dot_normalization() -> ExternalPool {
        let mut pool = test_pool("https://example.com/v1", true);
        pool.normalize_model_version_dots = true;
        pool
    }

    fn test_external_pool_outbound_body(
        route: &ExternalRouteRequest,
        pool: &ExternalPool,
    ) -> Bytes {
        external_pool_outbound_body(route, pool).expect("build external outbound body")
    }

    fn test_route(model: &str) -> ExternalRouteRequest {
        let payload = test_payload(model);
        let request_input_tokens = count_external_route_input_tokens(&payload);
        ExternalRouteRequest {
            raw_body: Bytes::new(),
            headers: HeaderMap::new(),
            endpoint: "/cc/v1/messages".to_string(),
            payload,
            request_input_tokens,
            upstream_model: Some(model.to_string()),
            model_resolution_source: Some("exact_upstream".to_string()),
            model_resolution_note: None,
            route_subtype: UsageRouteSubtype::ExternalDirectPolicy,
            fallback_reason: None,
            direct_policy_reason: None,
            local_attempted: false,
            local_preflight: None,
            local_attempts: Vec::new(),
            reported_usage: ReportedUsageConfig::default(),
            prompt_cache: Arc::new(PromptCacheTracker::default()),
            prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
            prompt_cache_strategy_type: PromptCacheStrategyType::CurrentHighCache,
            prompt_cache_simulation_mode: PromptCacheSimulationMode::HighCache,
            prompt_cache_route_namespace: None,
            prompt_cache_target_read_ratio: 0.98,
            prompt_cache_token_scale: 1.6,
            prompt_cache_max_simulated_input_tokens: 300_000,
            prompt_cache_cap_jitter_min_tokens: 12_000,
            prompt_cache_cap_jitter_max_tokens: 24_000,
            prompt_cache_scale_min_input_tokens: 20_000,
            prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
            prompt_cache_bounds: PromptCacheBounds::default(),
            kiro_rs_tool_cache_policy: KiroRsToolCachePolicy::default(),
            model_capabilities: Arc::new(ModelCapabilitiesCatalog::new()),
            pricing_catalog: Arc::new(PricingCatalog::new()),
            request_id: "req_external_billing".to_string(),
            error_id: "req_error_external_billing".to_string(),
            recorder: Arc::new(crate::anthropic::usage::UsageRecorder::new(1)),
            started_at: Instant::now(),
            first_token_latency_ms: Arc::new(AtomicU64::new(0)),
            latency_trace: Arc::new(ExternalLatencyTraceState::default()),
            payload_breakdown: None,
            payload_guard_report: None,
            payload_guard_retry_config: None,
        }
    }

    fn test_payload(model: &str) -> MessagesRequest {
        MessagesRequest {
            model: model.to_string(),
            max_tokens: 8,
            messages: vec![Message {
                role: "user".to_string(),
                content: serde_json::json!("hello"),
            }],
            stream: false,
            system: Some(vec![SystemMessage {
                text: "You are a careful coding assistant. ".repeat(180),
                cache_control: None,
            }]),
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: Some(Metadata {
                user_id: Some("user_test_account__session_external-projection-session".to_string()),
            }),
        }
    }

    #[test]
    fn direct_external_policy_enabled_is_global_direct_reason() {
        let mut config = ExternalPoolsConfig::default();

        assert_eq!(
            direct_external_policy_static_reason(&config, "/cc/v1/messages", "claude-custom"),
            None
        );

        config.external_pools_enabled = true;
        config.external_direct_policy_enabled = true;
        assert_eq!(
            direct_external_policy_static_reason(&config, "/cc/v1/messages", "claude-custom")
                .as_deref(),
            Some("explicit_direct")
        );

        config.direct_external_model_rules = vec!["sonnet".to_string()];
        assert_eq!(
            direct_external_policy_static_reason(&config, "/cc/v1/messages", "claude-sonnet-4-5")
                .as_deref(),
            Some("model_rule:claude-sonnet-4-5")
        );

        config.direct_external_model_rules.clear();
        config.direct_external_path_rules = vec!["/ha/".to_string()];
        assert_eq!(
            direct_external_policy_static_reason(&config, "/ha/v1/messages", "custom-model")
                .as_deref(),
            Some("path_rule:/ha/v1/messages")
        );
    }

    #[test]
    fn external_pool_outbound_body_strips_budget_tokens_for_adaptive_thinking() {
        let mut route = test_route("claude-opus-4-7-thinking");
        route.payload.thinking = Some(Thinking {
            thinking_type: "adaptive".to_string(),
            budget_tokens: 20000,
        });
        route.payload.output_config = Some(OutputConfig {
            effort: "xhigh".to_string(),
        });
        route.raw_body = Bytes::from_static(
            br#"{"model":"claude-opus-4-7-thinking","max_tokens":8,"messages":[{"role":"user","content":"hello"}],"stream":false,"thinking":{"type":"adaptive","budget_tokens":20000},"output_config":{"effort":"xhigh"}}"#,
        );

        let pool = test_pool("https://example.com/v1", true);
        let outbound = test_external_pool_outbound_body(&route, &pool);
        let value: serde_json::Value =
            serde_json::from_slice(&outbound).expect("parse outbound body");

        assert_eq!(value["thinking"]["type"], "adaptive");
        assert!(value["thinking"].get("budget_tokens").is_none());
        assert_eq!(value["output_config"]["effort"], "xhigh");
    }

    #[test]
    fn external_pool_outbound_body_applies_resolved_upstream_model() {
        let mut route = test_route("claude-sonnet-4-5-20250929");
        route.upstream_model = Some("claude-sonnet-4.5".to_string());
        route.model_resolution_source = Some("alias".to_string());
        route.raw_body = Bytes::from_static(
            br#"{"model":"claude-sonnet-4-5-20250929","max_tokens":8,"messages":[{"role":"user","content":"hello"}],"stream":true}"#,
        );

        let pool = test_pool_with_model_dot_normalization();
        let outbound = test_external_pool_outbound_body(&route, &pool);
        let prepared = external_pool_prepare_request(&route, &pool).unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&outbound).expect("parse outbound body");

        assert_eq!(value["model"], "claude-sonnet-4-5");
        assert_eq!(
            prepared.outbound_model.as_deref(),
            Some("claude-sonnet-4-5")
        );
        assert_eq!(route.payload.model, "claude-sonnet-4-5-20250929");
    }

    #[test]
    fn external_pool_outbound_body_uses_normalized_payload_not_stale_raw_body() {
        let mut route = test_route("claude-sonnet-4-5-20250929");
        route.upstream_model = Some("claude-sonnet-4.5".to_string());
        route.model_resolution_source = Some("alias".to_string());
        route.payload.messages = vec![Message {
            role: "user".to_string(),
            content: serde_json::json!([{
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/jpeg",
                    "data": "/9j/normalized"
                }
            }]),
        }];
        route.raw_body = Bytes::from_static(
            br#"{"model":"claude-sonnet-4-5-20250929","max_tokens":8,"messages":[{"role":"user","content":[{"type":"image","source":{"type":"base64","media_type":"image/png","data":"/9j/stale"}}]}],"stream":true}"#,
        );

        let pool = test_pool_with_model_dot_normalization();
        let outbound = test_external_pool_outbound_body(&route, &pool);
        let value: serde_json::Value =
            serde_json::from_slice(&outbound).expect("parse outbound body");

        assert_eq!(value["model"], "claude-sonnet-4-5");
        assert_eq!(
            value["messages"][0]["content"][0]["source"]["media_type"],
            "image/jpeg"
        );
        assert_eq!(
            value["messages"][0]["content"][0]["source"]["data"],
            "/9j/normalized"
        );
    }

    #[test]
    fn external_pool_outbound_body_applies_model_mapping_and_thinking_normalization() {
        let mut route = test_route("claude-opus-4-5-20251101");
        route.upstream_model = Some("claude-opus-4.5".to_string());
        route.model_resolution_source = Some("alias".to_string());
        route.payload.thinking = Some(Thinking {
            thinking_type: "adaptive".to_string(),
            budget_tokens: 20000,
        });
        route.payload.output_config = Some(OutputConfig {
            effort: "xhigh".to_string(),
        });
        route.raw_body = Bytes::from_static(
            br#"{"model":"claude-opus-4-5-20251101","max_tokens":8,"messages":[{"role":"user","content":"hello"}],"stream":false,"thinking":{"type":"adaptive","budget_tokens":20000},"output_config":{"effort":"xhigh"}}"#,
        );

        let pool = test_pool_with_model_dot_normalization();
        let outbound = test_external_pool_outbound_body(&route, &pool);
        let value: serde_json::Value =
            serde_json::from_slice(&outbound).expect("parse outbound body");

        assert_eq!(value["model"], "claude-opus-4-5");
        assert_eq!(value["thinking"]["type"], "adaptive");
        assert!(value["thinking"].get("budget_tokens").is_none());
        assert_eq!(value["output_config"]["effort"], "xhigh");
    }

    #[test]
    fn external_pool_outbound_body_normalizes_payload_claude_model_without_mapping() {
        let route = test_route("claude-haiku-4.5");

        let pool = test_pool_with_model_dot_normalization();
        let outbound = test_external_pool_outbound_body(&route, &pool);
        let value: serde_json::Value =
            serde_json::from_slice(&outbound).expect("parse outbound body");

        assert_eq!(value["model"], "claude-haiku-4-5");
    }

    #[test]
    fn external_pool_outbound_body_preserves_dot_model_when_pool_normalization_disabled() {
        let route = test_route("claude-haiku-4.5");
        let pool = test_pool("https://example.com/v1", true);

        let outbound = test_external_pool_outbound_body(&route, &pool);
        let value: serde_json::Value =
            serde_json::from_slice(&outbound).expect("parse outbound body");

        assert_eq!(value["model"], "claude-haiku-4.5");
    }

    #[test]
    fn external_pool_outbound_body_passthrough_uses_original_request_model() {
        let mut route = test_route("claude-sonnet-4-5-20250929");
        route.upstream_model = Some("claude-sonnet-4.5".to_string());
        route.raw_body = Bytes::from_static(
            br#"{"model":"claude-sonnet-4-5-20250929","max_tokens":8,"messages":[{"role":"user","content":"hello"}],"stream":true}"#,
        );
        let mut pool = test_pool_with_model_dot_normalization();
        pool.model_mapping_mode = ExternalPoolModelMappingMode::Passthrough;
        pool.model_mapping_rules = vec![model_rule("claude-sonnet-4-5-20250929", "custom-sonnet")];

        let outbound = test_external_pool_outbound_body(&route, &pool);
        let prepared = external_pool_prepare_request(&route, &pool).unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&outbound).expect("parse outbound body");

        assert_eq!(value["model"], "claude-sonnet-4-5-20250929");
        assert_eq!(
            prepared.outbound_model.as_deref(),
            Some("claude-sonnet-4-5-20250929")
        );
    }

    #[test]
    fn external_pool_outbound_body_passthrough_mapping_maps_hit_and_keeps_original_on_miss() {
        let mut route = test_route("claude-sonnet-4-5-20250929");
        route.upstream_model = Some("claude-sonnet-4.5".to_string());
        route.raw_body = Bytes::from_static(
            br#"{"model":"claude-sonnet-4-5-20250929","max_tokens":8,"messages":[{"role":"user","content":"hello"}],"stream":true}"#,
        );
        let mut pool = test_pool_with_model_dot_normalization();
        pool.model_mapping_mode = ExternalPoolModelMappingMode::PassthroughMapping;
        pool.model_mapping_rules =
            vec![model_rule("claude-sonnet-4-5-20250929", "external-sonnet")];

        let outbound = test_external_pool_outbound_body(&route, &pool);
        let value: serde_json::Value =
            serde_json::from_slice(&outbound).expect("parse outbound body");
        assert_eq!(value["model"], "external-sonnet");

        pool.model_mapping_rules = vec![model_rule("claude-opus-4-8", "external-opus")];
        let outbound = test_external_pool_outbound_body(&route, &pool);
        let value: serde_json::Value =
            serde_json::from_slice(&outbound).expect("parse outbound body");
        assert_eq!(value["model"], "claude-sonnet-4-5-20250929");
    }

    #[test]
    fn external_pool_outbound_body_require_mapping_match_rejects_miss_before_send() {
        let mut route = test_route("claude-sonnet-4-5-20250929");
        route.upstream_model = Some("claude-sonnet-4.5".to_string());
        let mut pool = test_pool_with_model_dot_normalization();
        pool.model_mapping_mode = ExternalPoolModelMappingMode::PassthroughMapping;
        pool.model_mapping_require_match = true;
        pool.model_mapping_rules = vec![model_rule("claude-opus-4-8", "external-opus")];

        let err = external_pool_outbound_body(&route, &pool).unwrap_err();

        assert!(err.retryable);
        assert_eq!(err.status, Some(StatusCode::BAD_GATEWAY));
        assert_eq!(error_type_for_external_error(&err), "model_mapping_miss");
        assert!(err.message.contains("requires model mapping match"));
    }

    #[test]
    fn external_pool_outbound_body_direct_mapping_uses_original_model() {
        let mut route = test_route("claude-sonnet-4-5-20250929");
        route.upstream_model = Some("claude-sonnet-4.5".to_string());
        route.raw_body = Bytes::from_static(
            br#"{"model":"claude-sonnet-4-5-20250929","max_tokens":8,"messages":[{"role":"user","content":"hello"}],"stream":true}"#,
        );
        let mut pool = test_pool_with_model_dot_normalization();
        pool.model_mapping_mode = ExternalPoolModelMappingMode::DirectMapping;
        pool.model_mapping_rules =
            vec![model_rule("claude-sonnet-4-5-20250929", "external-sonnet")];

        let outbound = test_external_pool_outbound_body(&route, &pool);
        let prepared = external_pool_prepare_request(&route, &pool).unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&outbound).expect("parse outbound body");

        assert_eq!(value["model"], "external-sonnet");
        assert_eq!(prepared.outbound_model.as_deref(), Some("external-sonnet"));
    }

    #[test]
    fn external_pool_outbound_body_processed_mapping_uses_upstream_model() {
        let mut route = test_route("claude-sonnet-4-5-20250929");
        route.upstream_model = Some("claude-sonnet-4.5".to_string());
        route.raw_body = Bytes::from_static(
            br#"{"model":"claude-sonnet-4-5-20250929","max_tokens":8,"messages":[{"role":"user","content":"hello"}],"stream":true}"#,
        );
        let mut pool = test_pool_with_model_dot_normalization();
        pool.model_mapping_mode = ExternalPoolModelMappingMode::ProcessedMapping;
        pool.model_mapping_rules = vec![model_rule("claude-sonnet-4.5", "external-sonnet")];

        let outbound = test_external_pool_outbound_body(&route, &pool);
        let value: serde_json::Value =
            serde_json::from_slice(&outbound).expect("parse outbound body");

        assert_eq!(value["model"], "external-sonnet");
    }

    #[test]
    fn external_pool_outbound_body_mapping_miss_falls_back_to_existing_conversion() {
        let mut route = test_route("claude-sonnet-4-5-20250929");
        route.upstream_model = Some("claude-sonnet-4.5".to_string());
        route.raw_body = Bytes::from_static(
            br#"{"model":"claude-sonnet-4-5-20250929","max_tokens":8,"messages":[{"role":"user","content":"hello"}],"stream":true}"#,
        );
        let mut pool = test_pool_with_model_dot_normalization();
        pool.model_mapping_mode = ExternalPoolModelMappingMode::DirectMapping;
        pool.model_mapping_rules = vec![model_rule("claude-opus-4.8", "external-opus")];

        let outbound = test_external_pool_outbound_body(&route, &pool);
        let value: serde_json::Value =
            serde_json::from_slice(&outbound).expect("parse outbound body");

        assert_eq!(value["model"], "claude-sonnet-4-5");
    }

    #[test]
    fn external_pool_outbound_body_mapping_target_is_final() {
        let mut route = test_route("claude-sonnet-4-5-20250929");
        route.upstream_model = Some("claude-sonnet-4.5".to_string());
        route.raw_body = Bytes::from_static(
            br#"{"model":"claude-sonnet-4-5-20250929","max_tokens":8,"messages":[{"role":"user","content":"hello"}],"stream":true}"#,
        );
        let mut pool = test_pool_with_model_dot_normalization();
        pool.model_mapping_mode = ExternalPoolModelMappingMode::ProcessedMapping;
        pool.model_mapping_rules = vec![model_rule("claude-sonnet-4.5", "claude-sonnet-4.5")];

        let outbound = test_external_pool_outbound_body(&route, &pool);
        let value: serde_json::Value =
            serde_json::from_slice(&outbound).expect("parse outbound body");

        assert_eq!(value["model"], "claude-sonnet-4.5");
    }

    #[test]
    fn external_pool_mapping_rules_normalize_and_match_on_call_path() {
        let mut route = test_route("claude-sonnet-4-5-20250929");
        route.upstream_model = Some("claude-sonnet-4.5".to_string());
        route.raw_body = Bytes::from_static(
            br#"{"model":"claude-sonnet-4-5-20250929","max_tokens":8,"messages":[{"role":"user","content":"hello"}],"stream":true}"#,
        );
        let mut pool = test_pool_with_model_dot_normalization();
        pool.model_mapping_mode = ExternalPoolModelMappingMode::ProcessedMapping;
        pool.model_mapping_rules = normalize_external_pool_model_mapping_rules(vec![
            model_rule("  CLAUDE-SONNET-4.5  ", "  CLAUDE-SONNET-4-5  "),
            model_rule("", "ignored-target"),
            model_rule("ignored-source", ""),
        ]);

        let outbound = test_external_pool_outbound_body(&route, &pool);
        let value: serde_json::Value =
            serde_json::from_slice(&outbound).expect("parse outbound body");

        assert_eq!(pool.model_mapping_rules.len(), 1);
        assert_eq!(pool.model_mapping_rules[0].target, "CLAUDE-SONNET-4-5");
        assert_eq!(value["model"], "CLAUDE-SONNET-4-5");
    }

    #[test]
    fn external_pool_mapping_supports_common_direct_date_to_dot_rule() {
        let mut route = test_route("claude-opus-4-5-20251101");
        route.upstream_model = Some("claude-opus-4.5".to_string());
        route.raw_body = Bytes::from_static(
            br#"{"model":"claude-opus-4-5-20251101","max_tokens":8,"messages":[{"role":"user","content":"hello"}],"stream":true}"#,
        );
        let mut pool = test_pool_with_model_dot_normalization();
        pool.model_mapping_mode = ExternalPoolModelMappingMode::DirectMapping;
        pool.model_mapping_rules = vec![model_rule("claude-opus-4-5-20251101", "claude-opus-4.5")];

        let outbound = test_external_pool_outbound_body(&route, &pool);
        let value: serde_json::Value =
            serde_json::from_slice(&outbound).expect("parse outbound body");

        assert_eq!(value["model"], "claude-opus-4.5");
    }

    #[test]
    fn external_pool_mapping_supports_common_processed_thinking_to_dash_rule() {
        let mut route = test_route("claude-opus-4-8-thinking");
        route.upstream_model = Some("claude-opus-4.8-thinking".to_string());
        route.raw_body = Bytes::from_static(
            br#"{"model":"claude-opus-4-8-thinking","max_tokens":8,"messages":[{"role":"user","content":"hello"}],"stream":true}"#,
        );
        let mut pool = test_pool("https://example.com/v1", true);
        pool.model_mapping_mode = ExternalPoolModelMappingMode::ProcessedMapping;
        pool.model_mapping_rules = vec![model_rule(
            "claude-opus-4.8-thinking",
            "claude-opus-4-8-thinking",
        )];

        let outbound = test_external_pool_outbound_body(&route, &pool);
        let value: serde_json::Value =
            serde_json::from_slice(&outbound).expect("parse outbound body");

        assert_eq!(value["model"], "claude-opus-4-8-thinking");
    }

    #[test]
    fn external_pool_outbound_model_normalization_only_changes_claude_numeric_versions() {
        assert_eq!(
            normalize_external_pool_outbound_model("claude-opus-4.8"),
            "claude-opus-4-8"
        );
        assert_eq!(
            normalize_external_pool_outbound_model("claude-opus-4.8-thinking"),
            "claude-opus-4-8-thinking"
        );
        assert_eq!(
            normalize_external_pool_outbound_model(" claude-sonnet-4.5[1m] "),
            "claude-sonnet-4-5[1m]"
        );
        assert_eq!(
            normalize_external_pool_outbound_model("deepseek-3.2"),
            "deepseek-3.2"
        );
    }

    #[test]
    fn external_pool_outbound_body_strips_budget_tokens_for_disabled_thinking() {
        let mut route = test_route("claude-opus-4-7");
        route.payload.thinking = Some(Thinking {
            thinking_type: "disabled".to_string(),
            budget_tokens: 20000,
        });
        route.raw_body = Bytes::from_static(
            br#"{"model":"claude-opus-4-7","max_tokens":8,"messages":[{"role":"user","content":"hello"}],"stream":false,"thinking":{"type":"disabled","budget_tokens":20000}}"#,
        );

        let pool = test_pool("https://example.com/v1", true);
        let outbound = test_external_pool_outbound_body(&route, &pool);
        let value: serde_json::Value =
            serde_json::from_slice(&outbound).expect("parse outbound body");

        assert_eq!(value["thinking"]["type"], "disabled");
        assert!(value["thinking"].get("budget_tokens").is_none());
    }

    #[test]
    fn external_pool_outbound_body_preserves_enabled_budget_tokens() {
        let mut route = test_route("claude-sonnet-4-6-thinking");
        route.payload.thinking = Some(Thinking {
            thinking_type: "enabled".to_string(),
            budget_tokens: 12345,
        });
        route.raw_body = Bytes::from_static(
            br#"{"model":"claude-sonnet-4-6-thinking","max_tokens":8,"messages":[{"role":"user","content":"hello"}],"stream":false,"thinking":{"type":"enabled","budget_tokens":12345}}"#,
        );

        let pool = test_pool("https://example.com/v1", true);
        let outbound = test_external_pool_outbound_body(&route, &pool);
        let value: serde_json::Value =
            serde_json::from_slice(&outbound).expect("parse outbound body");

        assert_eq!(value["thinking"]["type"], "enabled");
        assert_eq!(value["thinking"]["budget_tokens"], 12345);
    }

    fn projection_context(
        route: &ExternalRouteRequest,
        pool: &ExternalPool,
        uplift_percent: u32,
    ) -> Option<ExternalUsageProjectionContext> {
        projection_context_with_output_uplift(route, pool, uplift_percent, 0, 0)
    }

    fn projection_context_with_output_uplift(
        route: &ExternalRouteRequest,
        pool: &ExternalPool,
        uplift_percent: u32,
        output_uplift_min_tokens: i32,
        output_uplift_percent: u32,
    ) -> Option<ExternalUsageProjectionContext> {
        build_external_usage_projection_context(
            route,
            pool,
            uplift_percent,
            output_uplift_min_tokens,
            output_uplift_percent,
        )
    }

    fn event_usage_i64(event: &str, key: &str) -> i64 {
        event
            .lines()
            .find_map(|line| line.trim_start().strip_prefix("data:"))
            .and_then(|json| serde_json::from_str::<serde_json::Value>(json.trim()).ok())
            .and_then(|value| value.get("usage").and_then(|usage| usage.get(key)).cloned())
            .and_then(|value| value.as_i64())
            .expect("usage field")
    }

    #[test]
    fn external_pool_url_adds_single_v1_for_standard_message_path() {
        let config = ExternalPoolsConfig::default();
        let cases = [
            (
                "http://pool.example.com",
                "http://pool.example.com/v1/messages",
            ),
            (
                "http://pool.example.com/",
                "http://pool.example.com/v1/messages",
            ),
            (
                "http://pool.example.com/v1",
                "http://pool.example.com/v1/messages",
            ),
            (
                "http://pool.example.com/v1/",
                "http://pool.example.com/v1/messages",
            ),
            (
                "http://pool.example.com/api",
                "http://pool.example.com/api/v1/messages",
            ),
            (
                "http://pool.example.com/api/v1",
                "http://pool.example.com/api/v1/messages",
            ),
        ];

        for (base_url, expected) in cases {
            let actual = external_pool_url(&test_pool(base_url, false), "/cc/v1/messages", &config)
                .expect("valid external pool url");
            assert_eq!(actual.as_str(), expected);
        }
    }

    #[test]
    fn external_pool_url_uses_pool_messages_path_even_when_preserve_path_is_true() {
        let config = ExternalPoolsConfig::default();
        let base_v1 = external_pool_url(
            &test_pool("http://pool.example.com/v1", true),
            "/v1/messages",
            &config,
        )
        .expect("valid external pool url");
        assert_eq!(base_v1.as_str(), "http://pool.example.com/v1/messages");

        let cc_path = external_pool_url(
            &test_pool("http://pool.example.com", true),
            "/cc/v1/messages",
            &config,
        )
        .expect("valid external pool url");
        assert_eq!(cc_path.as_str(), "http://pool.example.com/v1/messages");
    }

    #[test]
    fn external_pool_models_url_adds_single_v1() {
        let cases = [
            (
                "http://pool.example.com",
                "http://pool.example.com/v1/models",
            ),
            (
                "http://pool.example.com/",
                "http://pool.example.com/v1/models",
            ),
            (
                "http://pool.example.com/v1",
                "http://pool.example.com/v1/models",
            ),
            (
                "http://pool.example.com/v1/",
                "http://pool.example.com/v1/models",
            ),
            (
                "http://pool.example.com/api",
                "http://pool.example.com/api/v1/models",
            ),
            (
                "http://pool.example.com/api/v1",
                "http://pool.example.com/api/v1/models",
            ),
        ];

        for (base_url, expected) in cases {
            let actual = external_pool_models_url(base_url).expect("valid models url");
            assert_eq!(actual.as_str(), expected);
        }
    }

    #[test]
    fn external_pool_auto_disable_window_has_own_default() {
        let config = ExternalPoolsConfig::default();

        assert_eq!(config.external_pool_auto_disable_window_secs, 60);
        assert_eq!(config.local_pool_circuit_window_secs, 60);
    }

    #[test]
    fn usage_projection_pass_through_keeps_body_unchanged() {
        let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}"#,
        );
        let projected = maybe_project_non_stream_usage(body.clone(), None);

        assert_eq!(projected.body, body);
        assert_eq!(
            projected.usage_capture.raw,
            projected.usage_capture.reported
        );
    }

    #[test]
    fn usage_projection_applies_current_path_policy_to_json_body() {
        let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}"#,
        );
        let route = test_route("claude-sonnet-4-5");
        let mut pool = test_pool("http://pool.example.com", false);
        pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
        let projection = projection_context(&route, &pool, 0);
        let projected = maybe_project_non_stream_usage(body.clone(), projection.as_ref());

        let value: serde_json::Value =
            serde_json::from_slice(&projected.body).expect("projected json");
        let usage = value.get("usage").expect("usage object");
        assert!(
            usage
                .get("input_tokens")
                .and_then(|value| value.as_i64())
                .is_some_and(|tokens| (1..=96).contains(&tokens))
        );
        assert!(
            usage
                .get("cache_read_input_tokens")
                .and_then(|value| value.as_i64())
                .unwrap_or_default()
                == 0
        );
        assert!(
            usage
                .get("cache_creation_input_tokens")
                .and_then(|value| value.as_i64())
                .unwrap_or_default()
                > 0
        );
    }

    #[test]
    fn usage_projection_ignores_external_cache_when_local_policy_has_no_cache() {
        let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":7,"cache_creation_input_tokens":50000,"cache_read_input_tokens":25000}}"#,
        );
        let route = test_route("deepseek-3.2");
        let mut pool = test_pool("http://pool.example.com", false);
        pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
        let projection = projection_context(&route, &pool, 25).expect("projection");
        let projected = maybe_project_non_stream_usage(body.clone(), Some(&projection));

        let value: serde_json::Value =
            serde_json::from_slice(&projected.body).expect("projected json");
        let usage = value.get("usage").expect("usage object");
        assert_eq!(
            usage
                .get("input_tokens")
                .and_then(|value| value.as_i64())
                .unwrap_or_default(),
            count_external_route_input_tokens(&route.payload) as i64
        );
        assert_eq!(
            usage
                .get("cache_creation_input_tokens")
                .and_then(|value| value.as_i64())
                .unwrap_or_default(),
            0
        );
        assert_eq!(
            usage
                .get("cache_read_input_tokens")
                .and_then(|value| value.as_i64())
                .unwrap_or_default(),
            0
        );
        assert!(projected.usage_capture.projected);
        assert_eq!(
            projected
                .usage_capture
                .raw
                .expect("raw")
                .cache_creation_input_tokens,
            50_000
        );
        assert_eq!(
            projected
                .usage_capture
                .reported
                .expect("reported")
                .cache_creation_input_tokens,
            0
        );
    }

    #[test]
    fn usage_projection_no_cache_route_projects_without_cache_state() {
        let mut route = test_route("claude-sonnet-4-5");
        route.prompt_cache_strategy_type = PromptCacheStrategyType::NoCache;
        route.prompt_cache_simulation_mode = PromptCacheSimulationMode::Disabled;
        route.reported_usage.path_overrides.insert(
            "/cc".to_string(),
            ReportedUsagePathPolicy {
                input: ReportedUsageFieldPolicy::sample_input_max(64),
                output: ReportedUsageFieldPolicy::sample_max(5),
                ..ReportedUsagePathPolicy::default()
            },
        );
        let mut pool = test_pool("http://pool.example.com", false);
        pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

        let projection = projection_context(&route, &pool, 0).expect("projection");
        assert!(!projection.cache_state_enabled);
        assert!(projection.simulated_usage.is_none());
        assert!(projection.scope.is_none());
        assert!(projection.prompt_cache_profile.is_none());
        assert!(projection.kiro_rs_tool_prompt_cache_plan.is_none());

        let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":9,"cache_creation_input_tokens":50000,"cache_read_input_tokens":25000}}"#,
        );
        let projected = maybe_project_non_stream_usage(body, Some(&projection));
        let value: serde_json::Value =
            serde_json::from_slice(&projected.body).expect("projected json");
        let usage = value.get("usage").expect("usage object");
        assert!(
            usage
                .get("input_tokens")
                .and_then(|value| value.as_i64())
                .is_some_and(|tokens| (1..=64).contains(&tokens))
        );
        assert!(
            usage
                .get("output_tokens")
                .and_then(|value| value.as_i64())
                .is_some_and(|tokens| (1..=5).contains(&tokens))
        );
        assert_eq!(
            usage
                .get("cache_creation_input_tokens")
                .and_then(|value| value.as_i64())
                .unwrap_or_default(),
            0
        );
        assert_eq!(
            usage
                .get("cache_read_input_tokens")
                .and_then(|value| value.as_i64())
                .unwrap_or_default(),
            0
        );
    }

    #[test]
    fn usage_projection_applies_external_pool_uplift_after_path_policy() {
        let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}"#,
        );
        let mut route = test_route("claude-sonnet-4-5");
        route.endpoint = "/v1/messages".to_string();
        let mut pool = test_pool("http://pool.example.com", false);
        pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

        let no_uplift_projection = projection_context(&route, &pool, 0).expect("projection");
        let no_uplift = maybe_project_non_stream_usage(body.clone(), Some(&no_uplift_projection));
        let with_uplift_projection = projection_context(&route, &pool, 25).expect("projection");
        let with_uplift = maybe_project_non_stream_usage(body, Some(&with_uplift_projection));

        let no_uplift_usage = no_uplift.usage_capture.reported.expect("no uplift usage");
        let with_uplift_shaped = with_uplift
            .usage_capture
            .shaped
            .expect("with uplift shaped usage");
        let with_uplift_usage = with_uplift.usage_capture.reported.expect("uplift usage");
        assert_eq!(
            with_uplift_shaped.total_input_tokens,
            no_uplift_usage.total_input_tokens
        );
        assert_eq!(
            with_uplift_shaped.input_tokens,
            no_uplift_usage.input_tokens
        );
        assert_eq!(
            with_uplift_shaped.output_tokens,
            no_uplift_usage.output_tokens
        );
        assert_eq!(
            with_uplift_shaped.cache_creation_input_tokens,
            no_uplift_usage.cache_creation_input_tokens
        );
        assert_eq!(
            with_uplift_shaped.cache_read_input_tokens,
            no_uplift_usage.cache_read_input_tokens
        );
        assert_eq!(with_uplift_usage.input_tokens, no_uplift_usage.input_tokens);
        assert_eq!(
            with_uplift_usage.cache_creation_input_tokens,
            uplift_tokens(no_uplift_usage.cache_creation_input_tokens, 25)
        );
        assert_eq!(
            with_uplift_usage.cache_read_input_tokens,
            uplift_tokens(no_uplift_usage.cache_read_input_tokens, 25)
        );
    }

    #[test]
    fn usage_projection_final_cache_read_guard_runs_after_external_pool_uplift() {
        let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}"#,
        );
        let mut route = test_route("claude-sonnet-4-5");
        route.reported_usage.path_overrides.insert(
            "/cc".to_string(),
            ReportedUsagePathPolicy {
                final_cache_read_max_tokens: 100,
                input: ReportedUsageFieldPolicy::sample_input_max(1),
                ..ReportedUsagePathPolicy::default()
            },
        );
        let mut pool = test_pool("http://pool.example.com", false);
        pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

        let warmup_projection = projection_context(&route, &pool, 0).expect("warmup projection");
        let _warmup = maybe_project_non_stream_usage(body.clone(), Some(&warmup_projection));
        warmup_projection.record_success();

        route.payload.messages.extend([
            Message {
                role: "assistant".to_string(),
                content: serde_json::json!("ready"),
            },
            Message {
                role: "user".to_string(),
                content: serde_json::json!("continue external projection session"),
            },
        ]);
        let projection = projection_context(&route, &pool, 200).expect("projection");
        let projected = maybe_project_non_stream_usage(body, Some(&projection));
        let reported = projected.usage_capture.reported.expect("reported usage");

        assert_eq!(reported.cache_read_input_tokens, 100);
        assert_eq!(
            reported.total_input_tokens,
            reported
                .input_tokens
                .saturating_add(reported.cache_read_input_tokens)
                .saturating_add(reported.cache_creation_input_tokens)
        );
    }

    #[test]
    fn usage_projection_final_input_guard_reapplies_path_input_limit_after_uplift() {
        let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}"#,
        );
        let mut route = test_route("claude-sonnet-4-5");
        route.reported_usage.path_overrides.insert(
            "/v1".to_string(),
            ReportedUsagePathPolicy {
                input: ReportedUsageFieldPolicy::sample_input_max(96),
                ..ReportedUsagePathPolicy::default()
            },
        );
        let mut pool = test_pool("http://pool.example.com", false);
        pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

        let projection = projection_context(&route, &pool, 200).expect("projection");
        let projected = maybe_project_non_stream_usage(body, Some(&projection));
        let reported = projected.usage_capture.reported.expect("reported usage");

        assert!((1..=96).contains(&reported.input_tokens));
        assert_eq!(reported.cache_read_input_tokens, 0);
        assert!(reported.cache_creation_input_tokens > 0);
        assert_eq!(
            reported.total_input_tokens,
            reported
                .input_tokens
                .saturating_add(reported.cache_read_input_tokens)
                .saturating_add(reported.cache_creation_input_tokens)
        );
    }

    #[test]
    fn usage_projection_final_input_guard_leaves_compliant_input_unchanged() {
        let policy = ReportedCacheUsagePolicy::from_path_policy(
            ReportedUsagePathPolicy {
                input: ReportedUsageFieldPolicy::sample_input_max(96),
                ..ReportedUsagePathPolicy::default()
            },
            42,
        )
        .expect("policy");
        let usage = CacheUsage {
            total_input_tokens: 50_000,
            input_tokens: 42,
            output_tokens: 1,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 49_958,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        };

        let guarded = policy.apply_final_input_guard(usage);

        assert_eq!(guarded.input_tokens, 42);
        assert_eq!(guarded.cache_read_input_tokens, 49_958);
        assert_eq!(guarded.total_input_tokens, 50_000);
    }

    #[test]
    fn usage_projection_stream_capture_uses_latest_projected_reported_usage() {
        let mut route = test_route("claude-sonnet-4-5");
        route.reported_usage.path_overrides.insert(
            "/v1".to_string(),
            ReportedUsagePathPolicy {
                input: ReportedUsageFieldPolicy::sample_input_max(96),
                ..ReportedUsagePathPolicy::default()
            },
        );
        let mut pool = test_pool("http://pool.example.com", false);
        pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
        let projection = projection_context(&route, &pool, 0).expect("projection");
        let capture = Arc::new(SyncMutex::new(ExternalUsageCapture {
            reported: Some(CacheUsage {
                total_input_tokens: 120_000,
                input_tokens: 10_000,
                output_tokens: 1,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 110_000,
                cache_creation_5m_input_tokens: 0,
                cache_creation_1h_input_tokens: 0,
            }),
            ..ExternalUsageCapture::default()
        }));

        let event =
            br#"data: {"type":"message_delta","usage":{"input_tokens":100000,"output_tokens":2,"cache_creation_input_tokens":0,"cache_read_input_tokens":120000}}

"#;

        let out = maybe_project_sse_event(event, Some(&projection), Some(&capture), None);
        let text = std::str::from_utf8(&out).expect("event text");
        let event_input = event_usage_i64(text, "input_tokens");
        let reported = capture.lock().reported.expect("reported usage");

        assert!((1..=96).contains(&event_input));
        assert_eq!(reported.input_tokens as i64, event_input);
        assert!(reported.input_tokens < 10_000);
    }

    #[test]
    fn usage_projection_output_uplift_only_applies_above_threshold() {
        let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":800,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}"#,
        );
        let mut route = test_route("claude-sonnet-4-5");
        route.endpoint = "/v1/messages".to_string();
        let mut pool = test_pool("http://pool.example.com", false);
        pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

        let projection =
            projection_context_with_output_uplift(&route, &pool, 0, 1_000, 50).expect("projection");
        let projected = maybe_project_non_stream_usage(body, Some(&projection));
        let shaped = projected.usage_capture.shaped.expect("shaped usage");
        let reported = projected.usage_capture.reported.expect("reported usage");

        assert_eq!(shaped.output_tokens, 800);
        assert_eq!(reported.output_tokens, 800);
    }

    #[test]
    fn usage_projection_output_uplift_changes_only_final_reported_usage() {
        let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":1200,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}"#,
        );
        let mut route = test_route("claude-sonnet-4-5");
        route.endpoint = "/v1/messages".to_string();
        let mut pool = test_pool("http://pool.example.com", false);
        pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

        let projection =
            projection_context_with_output_uplift(&route, &pool, 0, 1_000, 50).expect("projection");
        let projected = maybe_project_non_stream_usage(body, Some(&projection));
        let value: serde_json::Value =
            serde_json::from_slice(&projected.body).expect("projected json");
        let usage = value.get("usage").expect("usage object");
        let shaped = projected.usage_capture.shaped.expect("shaped usage");
        let reported = projected.usage_capture.reported.expect("reported usage");

        assert_eq!(shaped.output_tokens, 1200);
        assert_eq!(reported.output_tokens, uplift_tokens(1200, 50));
        assert_eq!(
            usage
                .get("output_tokens")
                .and_then(|value| value.as_i64())
                .unwrap_or_default(),
            uplift_tokens(1200, 50) as i64
        );
        assert_eq!(reported.input_tokens, shaped.input_tokens);
        assert_eq!(
            reported.cache_read_input_tokens,
            shaped.cache_read_input_tokens
        );
        assert_eq!(
            reported.cache_creation_input_tokens,
            shaped.cache_creation_input_tokens
        );
    }

    #[test]
    fn usage_projection_uses_resolved_model_without_mutating_payload_model() {
        let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}"#,
        );
        let mut route = test_route("sonnet");
        route.endpoint = "/v1/messages".to_string();
        route.upstream_model = Some("claude-sonnet-4-5".to_string());
        route.model_resolution_source = Some("alias".to_string());
        let mut pool = test_pool("http://pool.example.com", false);
        pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

        let projection = projection_context(&route, &pool, 0).expect("projection");
        let projected = maybe_project_non_stream_usage(body, Some(&projection));
        let billing = external_pool_billing_from_capture(&route, &pool, projected.usage_capture)
            .expect("billing");

        assert_eq!(route.payload.model, "sonnet");
        assert_eq!(billing.pricing_model.as_deref(), Some("claude-sonnet-4-5"));
        assert!(billing.pricing_available);
    }

    #[test]
    fn usage_projection_updates_external_pool_cache_after_success() {
        let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}"#,
        );
        let mut route = test_route("claude-sonnet-4-5");
        route.endpoint = "/v1/messages".to_string();
        let mut pool = test_pool("http://pool.example.com", false);
        pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

        let first_projection = projection_context(&route, &pool, 0).expect("first projection");
        let first = maybe_project_non_stream_usage(body.clone(), Some(&first_projection));
        let first_value: serde_json::Value =
            serde_json::from_slice(&first.body).expect("first projected json");
        let first_usage = first_value.get("usage").expect("first usage");
        assert_eq!(
            first_usage
                .get("cache_read_input_tokens")
                .and_then(|value| value.as_i64())
                .unwrap_or_default(),
            0
        );
        assert!(
            first_usage
                .get("cache_creation_input_tokens")
                .and_then(|value| value.as_i64())
                .unwrap_or_default()
                > 0
        );
        first_projection.record_success();

        route.payload.messages.extend([
            Message {
                role: "assistant".to_string(),
                content: serde_json::json!("ready"),
            },
            Message {
                role: "user".to_string(),
                content: serde_json::json!("continue external projection session"),
            },
        ]);
        let second_projection = projection_context(&route, &pool, 0).expect("second projection");
        let second = maybe_project_non_stream_usage(body, Some(&second_projection));
        let second_value: serde_json::Value =
            serde_json::from_slice(&second.body).expect("second projected json");
        let second_usage = second_value.get("usage").expect("second usage");
        assert!(
            second_usage
                .get("cache_read_input_tokens")
                .and_then(|value| value.as_i64())
                .unwrap_or_default()
                > 0
        );
    }

    #[test]
    fn kiro_rs_tool_usage_projection_commits_external_pool_cache_only_after_success() {
        let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}"#,
        );
        let mut route = test_route("claude-sonnet-4-5");
        route.endpoint = "/kiro/v1/messages".to_string();
        route.prompt_cache_strategy_type = PromptCacheStrategyType::KiroRsTool;
        route.prompt_cache_simulation_mode = PromptCacheSimulationMode::Disabled;
        route.payload.metadata = Some(Metadata {
            user_id: Some(
                "user_test_account__session_8bb5523b-ec7c-4540-a9ca-beb6d79f1552".to_string(),
            ),
        });
        route.payload.system = Some(vec![SystemMessage {
            text: "stable external kiro strategy prompt ".repeat(700),
            cache_control: Some(serde_json::json!({"type": "ephemeral"})),
        }]);
        let mut pool = test_pool("http://pool.example.com", false);
        pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

        let failed_projection = projection_context(&route, &pool, 0).expect("failed projection");
        let failed = maybe_project_non_stream_usage(body.clone(), Some(&failed_projection));
        let failed_value: serde_json::Value =
            serde_json::from_slice(&failed.body).expect("failed projected json");
        assert_eq!(
            failed_value["usage"]["cache_read_input_tokens"]
                .as_i64()
                .unwrap_or_default(),
            0
        );
        assert!(
            failed_value["usage"]["cache_creation_input_tokens"]
                .as_i64()
                .unwrap_or_default()
                > 0
        );

        let retry_projection = projection_context(&route, &pool, 0).expect("retry projection");
        let retry = maybe_project_non_stream_usage(body.clone(), Some(&retry_projection));
        let retry_value: serde_json::Value =
            serde_json::from_slice(&retry.body).expect("retry projected json");
        assert_eq!(
            retry_value["usage"]["cache_read_input_tokens"]
                .as_i64()
                .unwrap_or_default(),
            0
        );
        assert!(
            retry_value["usage"]["cache_creation_input_tokens"]
                .as_i64()
                .unwrap_or_default()
                > 0
        );
        retry_projection.record_success();

        route.payload.messages.extend([
            Message {
                role: "assistant".to_string(),
                content: serde_json::json!("ready"),
            },
            Message {
                role: "user".to_string(),
                content: serde_json::json!("continue external kiro strategy session"),
            },
        ]);
        let second_projection = projection_context(&route, &pool, 0).expect("second projection");
        let second = maybe_project_non_stream_usage(body, Some(&second_projection));
        let second_value: serde_json::Value =
            serde_json::from_slice(&second.body).expect("second projected json");
        assert!(
            second_value["usage"]["cache_read_input_tokens"]
                .as_i64()
                .unwrap_or_default()
                > 0
        );
        let raw = second.usage_capture.raw.expect("raw usage");
        let reported = second.usage_capture.reported.expect("reported usage");
        assert_eq!(raw.input_tokens, 100000);
        assert_eq!(raw.cache_read_input_tokens, 0);
        assert!(reported.cache_read_input_tokens > 0);
        assert!((32..=4_096).contains(&reported.input_tokens));
        assert_eq!(
            reported.input_tokens
                + reported.cache_creation_input_tokens
                + reported.cache_read_input_tokens,
            reported.total_input_tokens
        );
    }

    #[test]
    fn usage_projection_ignores_external_raw_cache_when_local_policy_reads() {
        let raw_creation_body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":80000,"cache_read_input_tokens":0}}"#,
        );
        let mut route = test_route("claude-sonnet-4-5");
        route.endpoint = "/v1/messages".to_string();
        let mut pool = test_pool("http://pool.example.com", false);
        pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;

        let first_projection = projection_context(&route, &pool, 0).expect("first projection");
        let first =
            maybe_project_non_stream_usage(raw_creation_body.clone(), Some(&first_projection));
        let first_value: serde_json::Value =
            serde_json::from_slice(&first.body).expect("first projected json");
        let first_usage = first_value.get("usage").expect("first usage");
        assert!(
            first_usage
                .get("cache_creation_input_tokens")
                .and_then(|value| value.as_i64())
                .unwrap_or_default()
                > 0
        );
        first_projection.record_success();

        route.payload.messages.extend([
            Message {
                role: "assistant".to_string(),
                content: serde_json::json!("ready"),
            },
            Message {
                role: "user".to_string(),
                content: serde_json::json!("continue external projection session"),
            },
        ]);
        let second_projection = projection_context(&route, &pool, 0).expect("second projection");
        let second = maybe_project_non_stream_usage(raw_creation_body, Some(&second_projection));
        let second_value: serde_json::Value =
            serde_json::from_slice(&second.body).expect("second projected json");
        let second_usage = second_value.get("usage").expect("second usage");
        let second_creation = second_usage
            .get("cache_creation_input_tokens")
            .and_then(|value| value.as_i64())
            .unwrap_or_default();
        let second_read = second_usage
            .get("cache_read_input_tokens")
            .and_then(|value| value.as_i64())
            .unwrap_or_default();

        assert_eq!(second_creation, 0);
        assert!(second_read > 0);
        assert_ne!(second_creation, 80_000);
    }

    #[test]
    fn external_pool_billing_pass_through_uses_reported_cost_without_floor() {
        let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":1000,"output_tokens":20,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}"#,
        );
        let projected = maybe_project_non_stream_usage(body, None);
        let route = test_route("claude-sonnet-4-5");
        let pool = test_pool("http://pool.example.com", false);
        let billing = external_pool_billing_from_capture(&route, &pool, projected.usage_capture)
            .expect("billing");

        assert!(billing.pricing_available);
        assert!(!billing.cost_floor_applied);
        assert!((billing.raw_cost_usd - billing.shaped_cost_usd).abs() < f64::EPSILON);
        assert!((billing.raw_cost_usd - billing.uplifted_cost_usd).abs() < f64::EPSILON);
        assert!(billing.profit_usd.abs() < f64::EPSILON);
        assert!((billing.raw_cost_usd - billing.reported_cost_usd).abs() < f64::EPSILON);
        assert!((billing.billable_cost_usd - billing.reported_cost_usd).abs() < f64::EPSILON);
    }

    #[test]
    fn external_pool_billing_tracks_raw_shaped_uplifted_costs() {
        let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}"#,
        );
        let route = test_route("claude-sonnet-4-5");
        let mut pool = test_pool("http://pool.example.com", false);
        pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
        let projection = projection_context(&route, &pool, 25);
        let projected = maybe_project_non_stream_usage(body, projection.as_ref());
        let billing = external_pool_billing_from_capture(&route, &pool, projected.usage_capture)
            .expect("billing");

        assert!(billing.pricing_available);
        assert!(billing.raw_cost_usd > billing.shaped_cost_usd);
        assert!(billing.uplifted_cost_usd > billing.shaped_cost_usd);
        assert!((billing.reported_cost_usd - billing.uplifted_cost_usd).abs() < f64::EPSILON);
        assert!((billing.billable_cost_usd - billing.uplifted_cost_usd).abs() < f64::EPSILON);
        assert!(
            (billing.profit_usd - (billing.uplifted_cost_usd - billing.raw_cost_usd)).abs()
                < 0.000000001
        );
    }

    #[test]
    fn external_pool_billing_uses_output_uplift_as_final_reported_cost() {
        let body = Bytes::from_static(
            br#"{"type":"message","usage":{"input_tokens":1000,"output_tokens":1200,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}"#,
        );
        let mut route = test_route("claude-sonnet-4-5");
        route.endpoint = "/v1/messages".to_string();
        let mut pool = test_pool("http://pool.example.com", false);
        pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
        let projection =
            projection_context_with_output_uplift(&route, &pool, 0, 1_000, 50).expect("projection");

        let projected = maybe_project_non_stream_usage(body, Some(&projection));
        let billing = external_pool_billing_from_capture(&route, &pool, projected.usage_capture)
            .expect("billing");

        assert!(billing.pricing_available);
        assert_eq!(billing.raw_usage.output_tokens, 1200);
        assert_eq!(billing.shaped_usage.output_tokens, 1200);
        assert_eq!(
            billing.reported_usage.output_tokens,
            uplift_tokens(1200, 50)
        );
        assert!(billing.uplifted_cost_usd > billing.shaped_cost_usd);
        assert!((billing.reported_cost_usd - billing.uplifted_cost_usd).abs() < f64::EPSILON);
        assert!((billing.billable_cost_usd - billing.uplifted_cost_usd).abs() < f64::EPSILON);
    }

    #[test]
    fn sse_usage_projection_preserves_delimiters_and_done_events() {
        let event = br#"event: message_delta
data: {"type":"message_delta","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}

data: [DONE]

"#;
        let route = test_route("claude-sonnet-4-5");
        let mut pool = test_pool("http://pool.example.com", false);
        pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
        let projection = projection_context(&route, &pool, 0);
        let projected = maybe_project_sse_event(event, projection.as_ref(), None, None);
        let text = String::from_utf8(projected).expect("utf8");

        assert!(text.contains("data: [DONE]"));
        assert!(text.contains("\n\n"));
        assert!(!text.contains(r#""input_tokens":100000"#));
    }

    #[test]
    fn sse_usage_projection_captures_raw_and_reported_usage() {
        let event = br#"event: message_delta
data: {"type":"message_delta","usage":{"input_tokens":100000,"output_tokens":1,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}

"#;
        let capture = Arc::new(SyncMutex::new(ExternalUsageCapture::default()));
        let route = test_route("claude-sonnet-4-5");
        let mut pool = test_pool("http://pool.example.com", false);
        pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
        let projection = projection_context(&route, &pool, 0);
        let _projected = maybe_project_sse_event(event, projection.as_ref(), Some(&capture), None);
        let capture = capture.lock().clone();
        let raw = capture.raw.expect("raw usage");
        let reported = capture.reported.expect("reported usage");

        assert_eq!(raw.input_tokens, 100000);
        assert!(reported.input_tokens <= 96);
        assert_eq!(reported.cache_read_input_tokens, 0);
        assert!(reported.cache_creation_input_tokens > 0);
    }

    #[test]
    fn sse_usage_projection_applies_output_uplift_to_reported_usage() {
        let event = br#"event: message_delta
data: {"type":"message_delta","usage":{"input_tokens":100000,"output_tokens":1200,"cache_creation_input_tokens":50000,"cache_read_input_tokens":0}}

"#;
        let capture = Arc::new(SyncMutex::new(ExternalUsageCapture::default()));
        let mut route = test_route("claude-sonnet-4-5");
        route.endpoint = "/v1/messages".to_string();
        let mut pool = test_pool("http://pool.example.com", false);
        pool.usage_projection_mode = ExternalPoolUsageProjectionMode::CurrentPathPolicy;
        let projection =
            projection_context_with_output_uplift(&route, &pool, 0, 1_000, 50).expect("projection");
        let projected = maybe_project_sse_event(event, Some(&projection), Some(&capture), None);
        let text = std::str::from_utf8(&projected).expect("projected sse");
        assert!(text.contains(r#""output_tokens":1800"#));

        let capture = capture.lock().clone();
        let shaped = capture.shaped.expect("shaped usage");
        let reported = capture.reported.expect("reported usage");
        assert_eq!(shaped.output_tokens, 1200);
        assert_eq!(reported.output_tokens, uplift_tokens(1200, 50));
    }

    #[test]
    fn finds_sse_event_delimiters_for_lf_and_crlf() {
        assert_eq!(find_sse_event_delimiter(b"data: {}\n\nrest"), Some((8, 2)));
        assert_eq!(
            find_sse_event_delimiter(b"data: {}\r\n\r\nrest"),
            Some((8, 4))
        );
        assert_eq!(find_sse_event_delimiter(b"data: {}"), None);
    }
}
