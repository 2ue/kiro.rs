use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    future::Future,
    path::PathBuf,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use axum::{
    body::Body,
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::Response,
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures::{Stream, StreamExt};
use parking_lot::Mutex as SyncMutex;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex as AsyncMutex, Notify, OwnedMutexGuard, OwnedSemaphorePermit, Semaphore};
use tokio::time::{Instant, timeout};

use crate::{
    anthropic::{
        cache::{
            CacheAmplification, CacheSimulation, CacheUsage, RawUsage, ReportedCacheUsagePolicy,
        },
        envelope,
        inference_attempt_budget::{
            InferenceAttemptBudget, InferenceAttemptKind, InferenceAttemptRejection,
        },
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
        request_facts::{
            RawMessagesBodyProbe, deserialize_messages_request_with_probe, probe_raw_messages_body,
            rewrite_raw_top_level_model_with_probe,
        },
        transcript_sanitizer::{
            RESPONSE_PROTOCOL_CONTAMINATION_DETAIL, ToolTranscriptSanitizer,
            collect_known_tool_names_from_request, sanitize_response_content,
        },
        types::MessagesRequest,
        usage::{
            ExternalPoolAttempt, ExternalPoolBilling, ExternalPoolUsageSnapshot, UsageLatencyTrace,
            UsagePublicError, UsageRecord, UsageRecordStatus, UsageRouteKind, UsageRouteSubtype,
            UsageSource,
        },
    },
    common::capacity_signal::{CapacitySignal, CapacityWaiter},
    http_client::{HttpSendError, response_bytes_with_limit_and_body_timeout},
    kiro::token_manager::storage_task::spawn_critical_storage_task,
    model::config::{
        ExternalPoolCapacityMode, ExternalPoolModelUnavailableCooldownMode, ExternalPoolRouteMode,
        ExternalPoolStreamResponseMode, ExternalPoolsConfig, KiroRsToolCachePolicy,
        ModelMappingRule, PromptCacheCreationControlConfig, PromptCacheSimulationMode,
        PromptCacheStrategyType, ReportedUsageConfig, normalize_route_rules, route_rule_matches,
    },
    model::model_processing::{
        ModelProcessingConfig, ModelProcessingError, ModelProcessingInput, ModelProcessingMode,
        process_model,
    },
    model::model_support::{normalize_model_id, normalize_supported_models},
    storage::{
        postgres::PostgresStore,
        redis_cache::{
            ExternalPoolCoordinatorGuardError, ExternalPoolCoordinatorGuardState,
            ExternalPoolCoordinatorSnapshot, ExternalPoolCoordinatorSnapshotRequest,
            ExternalPoolLeaseAcquireResult as RedisExternalPoolLeaseAcquireResult,
            ExternalPoolLeaseReleaseRequest, LocalPoolCircuitState, RedisStore,
        },
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
use crate::anthropic::request_facts::{
    raw_body_probe_invocations_for_current_thread, raw_messages_body_hints,
    rewrite_raw_top_level_model,
};

const DEFAULT_EXTERNAL_POOL_REQUEST_TIMEOUT_SECS: u64 = 180;
const EXTERNAL_POOL_LEASE_TOUCH_INTERVAL_SECS: u64 = 30;
const EXTERNAL_POOL_LEASE_MAX_AGE_SECS: u64 = DEFAULT_EXTERNAL_POOL_REQUEST_TIMEOUT_SECS * 2;
const EXTERNAL_POOL_LEASE_HEARTBEAT_RETRY_MILLIS: u64 = 250;
const EXTERNAL_POOL_QUEUE_LEASE_SAFETY_MARGIN_SECS: u64 = 60;
const EXTERNAL_POOL_QUEUE_LEASE_RENEW_INTERVAL_MAX_SECS: u64 = 20;
const EXTERNAL_POOL_QUEUE_REDIS_OPERATION_TIMEOUT: Duration = Duration::from_secs(2);
const EXTERNAL_POOL_COORDINATOR_REDIS_OPERATION_TIMEOUT: Duration = Duration::from_secs(2);
const EXTERNAL_POOL_QUEUE_REDIS_RETRY_DELAY: Duration = Duration::from_millis(50);
const EXTERNAL_POOL_RELEASE_FALLBACK_MAX_IN_FLIGHT: usize = 64;
const EXTERNAL_POOL_RELEASE_BATCH_SIZE: usize = 256;
const EXTERNAL_POOL_RELEASE_CAPACITY: usize = 65_536;
const EXTERNAL_POOL_RELEASE_RETRY_MAX_DELAY: Duration = Duration::from_secs(30);
const EXTERNAL_POOL_RETRY_AFTER_MAX_SECS: u64 = 7 * 24 * 60 * 60;
const EXTERNAL_POOL_COORDINATOR_MAX_IN_FLIGHT: usize = 256;
const EXTERNAL_POOL_COORDINATOR_BREAKER_BACKOFF_SECS: [u64; 6] = [1, 2, 4, 8, 16, 30];
const EXTERNAL_POOL_COORDINATOR_RECOVERY_GRACE_SECS: u64 = 35;
const EXTERNAL_POOL_COORDINATOR_RUN_ID_PROBE_INTERVAL_SECS: u64 = 5;
const EXTERNAL_POOL_COORDINATOR_RUNTIME_CONFIG_ID: &str = "external_pool_redis_coordination_epoch";
const EXTERNAL_POOL_COORDINATOR_POSTGRES_LOCK_ID: i64 = 0x4b49_524f_4558_5450;
const EXTERNAL_POOL_USAGE_DEBUG_DEFAULT_DIR: &str = "/tmp/kiro-rs/external-pool-usage-debug";
const EXTERNAL_POOL_USAGE_DEBUG_DEFAULT_MAX_BODY_BYTES: usize = 8 * 1024;
const EXTERNAL_POOL_USAGE_DEBUG_MAX_BODY_BYTES: usize = 1024 * 1024;
const EXTERNAL_POOL_USAGE_DEBUG_DEFAULT_MAX_FILES: u64 = 1_000;
const EXTERNAL_POOL_USAGE_DEBUG_USAGE_SAMPLE_LIMIT: usize = 20;
const EXTERNAL_POOL_USAGE_DEBUG_EVENT_SAMPLE_BYTES: usize = 2 * 1024;
const EXTERNAL_POOL_USAGE_DEBUG_USAGE_JSON_BYTES: usize = 4 * 1024;
static EXTERNAL_POOL_USAGE_DEBUG_WRITE_COUNT: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
const EXTERNAL_POOL_AVAILABILITY_CACHE_TTL: Duration = Duration::from_millis(250);
const EXTERNAL_POOL_STATIC_SNAPSHOT_FRESH_TTL: Duration = Duration::from_secs(5);
const EXTERNAL_POOL_STATIC_SNAPSHOT_STALE_TTL: Duration = Duration::from_secs(30);
const EXTERNAL_POOL_STATIC_SNAPSHOT_FAILURE_RETRY: Duration = Duration::from_secs(1);
const EXTERNAL_POOL_STATIC_SNAPSHOT_REFRESH_TIMEOUT: Duration = Duration::from_millis(500);
const EXTERNAL_POOL_SELECTION_RUNTIME_SNAPSHOT_TTL: Duration = Duration::from_millis(100);
const EXTERNAL_POOL_SELECTION_POSTGRES_TIMEOUT: Duration = Duration::from_secs(2);
const EXTERNAL_POOL_AUTHORITATIVE_SNAPSHOT_TTL: Duration = Duration::from_millis(250);
const EXTERNAL_POOL_AUTHORITATIVE_SNAPSHOT_STALE_TTL: Duration = Duration::from_secs(30);
const EXTERNAL_POOL_AUTHORITATIVE_SNAPSHOT_FAILURE_RETRY: Duration = Duration::from_millis(100);
const EXTERNAL_POOL_AUTHORITATIVE_SNAPSHOT_WAIT_TIMEOUT: Duration = Duration::from_millis(2_250);
const EXTERNAL_POOL_SELECTION_MAX_IN_FLIGHT: usize = 32;
const EXTERNAL_POOL_SELECTION_BREAKER_BACKOFF_MILLIS: [u64; 4] = [100, 250, 500, 1_000];
const EXTERNAL_POOL_SELECTION_BREAKER_JITTER_PERCENT: u8 = 20;
const EXTERNAL_POOL_SELECTION_RETRY_AFTER: Duration = Duration::from_millis(250);
const EXTERNAL_POOL_DISPATCH_FENCE_TIMEOUT: Duration = Duration::from_millis(500);
const EXTERNAL_POOL_DISPATCH_FENCE_SHARED_WAIT_TIMEOUT: Duration = Duration::from_millis(600);
const EXTERNAL_POOL_LOCAL_MUTATION_AUTHORITATIVE_FRESH_TTL: Duration = Duration::from_secs(5);
const EXTERNAL_POOL_LOCAL_MUTATION_DISPATCH_TRUST_TTL: Duration = Duration::from_secs(30);
const EXTERNAL_POOL_AUTO_DISABLE_REDIS_TIMEOUT: Duration = Duration::from_millis(500);
const EXTERNAL_POOL_AUTO_DISABLE_POSTGRES_TIMEOUT: Duration = Duration::from_millis(500);
const EXTERNAL_POOL_AUTO_DISABLE_TRANSITION_CLAIM_TTL_SECS: usize = 5;
const EXTERNAL_POOL_SUCCESS_RESET_COALESCE_WINDOW: Duration = Duration::from_secs(1);
const EXTERNAL_POOL_SUCCESS_RESET_REDIS_TIMEOUT: Duration = Duration::from_millis(500);
const EXTERNAL_POOL_SUCCESS_RESET_MAX_TASKS: usize = 64;
const EXTERNAL_POOL_SUCCESS_RESET_MAX_TRACKED_POOLS: usize = 4_096;
const EXTERNAL_POOL_TRANSIENT_FAILURE_WINDOW_SECS: usize = 30;
const EXTERNAL_POOL_STATIC_SNAPSHOT_JITTER_PERCENT: u8 = 10;
const MAX_RECORDED_EXTERNAL_ERROR_MESSAGE_BYTES: usize = 8192;
const EXTERNAL_POOL_MAX_SSE_EVENT_BUFFER_BYTES: usize = 1024 * 1024;
const EXTERNAL_POOL_ERROR_RESPONSE_MAX_BYTES: usize = 1024 * 1024;
const EXTERNAL_POOL_NON_STREAM_RESPONSE_MAX_BYTES: usize = 16 * 1024 * 1024;
const EXTERNAL_POOL_DEFAULT_RESPONSE_BODY_TIMEOUT_SECS: u64 = 180;
const SAFE_EXTERNAL_STREAM_ERROR_EVENT: &str = "external upstream emitted an error event";
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
        Self::parse_known(value).unwrap_or_default()
    }

    pub(crate) fn parse_known(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "bearer" => Some(Self::Bearer),
            "x_api_key" | "x-api-key" | "xapikey" => Some(Self::XApiKey),
            _ => None,
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
        Self::parse_known(value).unwrap_or_default()
    }

    pub(crate) fn parse_known(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "pass_through" | "passthrough" => Some(Self::PassThrough),
            "current_path_policy" => Some(Self::CurrentPathPolicy),
            _ => None,
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
        Self::parse_known(value).unwrap_or_default()
    }

    pub(crate) fn parse_known(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "normalized" => Some(Self::Normalized),
            "raw_passthrough" | "raw" | "passthrough_body" => Some(Self::RawPassthrough),
            _ => None,
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
        Self::parse_known(value).unwrap_or_default()
    }

    pub(crate) fn parse_known(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" => Some(Self::None),
            "probe_only" | "probe" => Some(Self::ProbeOnly),
            "rewrite_top_level" | "rewrite" | "model_rewrite" => Some(Self::RewriteTopLevel),
            _ => None,
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
        Self::parse_known(value).unwrap_or_default()
    }

    pub(crate) fn parse_known(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "inherit" => Some(Self::Inherit),
            "disabled" => Some(Self::Disabled),
            "enabled" => Some(Self::Enabled),
            _ => None,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExternalPoolStreamRetryMode {
    #[default]
    Inherit,
    Enabled,
    Disabled,
}

impl ExternalPoolStreamRetryMode {
    pub fn parse(value: &str) -> Self {
        Self::parse_known(value).unwrap_or_default()
    }

    pub(crate) fn parse_known(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "inherit" => Some(Self::Inherit),
            "enabled" => Some(Self::Enabled),
            "disabled" => Some(Self::Disabled),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inherit => "inherit",
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
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
        Self::parse_known(value).unwrap_or_default()
    }

    pub(crate) fn parse_known(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "passthrough" | "pass_through" => Some(Self::Passthrough),
            "passthrough_mapping" | "pass_through_mapping" | "passthrough_with_mapping" => {
                Some(Self::PassthroughMapping)
            }
            "direct_mapping" | "direct" => Some(Self::DirectMapping),
            "processed_mapping" | "processed" => Some(Self::ProcessedMapping),
            _ => None,
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
    /// Monotonic PostgreSQL row revision used to fence selection-to-send races.
    #[serde(default = "default_external_pool_revision")]
    pub revision: u64,
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
    #[serde(default)]
    pub pre_output_stream_retry_mode: ExternalPoolStreamRetryMode,
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
    #[serde(default)]
    pub route_mode: ExternalPoolRouteMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub route_rules: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Secret-free, pre-normalized projection used only for cheap routing eligibility checks.
/// Dispatch always reloads an authoritative request-scoped `ExternalPool` from PostgreSQL.
#[derive(Debug, Clone)]
pub(crate) struct ExternalPoolEligibility {
    pub(crate) id: u64,
    pub(crate) enabled: bool,
    pub(crate) auto_disabled: bool,
    pub(crate) auto_disabled_until: Option<DateTime<Utc>>,
    pub(crate) supported_models: Arc<HashSet<String>>,
    pub(crate) route_mode: ExternalPoolRouteMode,
    pub(crate) route_rules: Arc<Vec<String>>,
}

impl ExternalPoolEligibility {
    fn is_auto_disabled_at(&self, now: DateTime<Utc>) -> bool {
        self.auto_disabled
            && self
                .auto_disabled_until
                .map(|until| until > now)
                .unwrap_or(true)
    }
}

fn external_pool_eligibility_from_pool(pool: &ExternalPool) -> ExternalPoolEligibility {
    ExternalPoolEligibility {
        id: pool.id,
        enabled: pool.enabled,
        auto_disabled: pool.auto_disabled,
        auto_disabled_until: pool.auto_disabled_until,
        supported_models: Arc::new(
            normalize_supported_models(pool.supported_models.clone())
                .into_iter()
                .collect::<HashSet<_>>(),
        ),
        route_mode: pool.route_mode,
        route_rules: Arc::new(normalize_route_rules(&pool.route_rules)),
    }
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

    pub(crate) fn masked_for_admin_response(&self) -> Self {
        let mut masked = self.clone();
        if masked.masked_api_key.is_none() {
            masked.masked_api_key = self
                .api_key
                .as_deref()
                .map(mask_external_pool_key)
                .or_else(|| self.masked_api_key.clone());
        }
        masked.api_key = None;
        masked
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
    #[serde(default)]
    pub pre_output_stream_retry_mode: ExternalPoolStreamRetryMode,
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
    pub route_mode: ExternalPoolRouteMode,
    #[serde(default)]
    pub route_rules: Vec<String>,
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
    pub pre_output_stream_retry_mode: Option<ExternalPoolStreamRetryMode>,
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
    pub route_mode: Option<ExternalPoolRouteMode>,
    #[serde(default)]
    pub route_rules: Option<Vec<String>>,
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
    pub transient_failure_streak: u32,
    pub transient_failure_ttl_secs: u64,
    pub dispatchable: bool,
    pub skipped_reason: Option<String>,
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

#[derive(Default)]
pub(crate) struct ExternalRouteRequestPreparationCache {
    normalized_base: OnceLock<Result<Arc<body_pipeline::NormalizedRequestBase>, PayloadGuardError>>,
    raw_projection_payload: OnceLock<Option<Arc<MessagesRequest>>>,
    known_tool_names: OnceLock<Arc<Vec<String>>>,
    usage_projection_template:
        OnceLock<Option<Arc<usage_projection::ExternalUsageProjectionTemplate>>>,
    #[cfg(test)]
    operation_counts: ExternalRequestPreparationOperationCountState,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ExternalRequestPreparationOperationCounts {
    pub(crate) raw_payload_parses: u64,
    pub(crate) normalized_base_builds: u64,
    pub(crate) normalized_original_value_parses: u64,
    pub(crate) normalized_json_serializations: u64,
    pub(crate) payload_guard_serializations: u64,
    pub(crate) known_tool_name_builds: u64,
    pub(crate) usage_projection_builds: u64,
}

#[cfg(test)]
#[derive(Default)]
struct ExternalRequestPreparationOperationCountState {
    raw_payload_parses: AtomicU64,
    normalized_base_builds: AtomicU64,
    normalized_original_value_parses: AtomicU64,
    normalized_json_serializations: AtomicU64,
    payload_guard_serializations: AtomicU64,
    known_tool_name_builds: AtomicU64,
    usage_projection_builds: AtomicU64,
}

impl ExternalRouteRequestPreparationCache {
    fn record_raw_payload_parse(&self) {
        #[cfg(test)]
        self.operation_counts
            .raw_payload_parses
            .fetch_add(1, Ordering::Relaxed);
    }

    fn record_normalized_base_build(&self) {
        #[cfg(test)]
        self.operation_counts
            .normalized_base_builds
            .fetch_add(1, Ordering::Relaxed);
    }

    fn record_normalized_original_value_parse(&self) {
        #[cfg(test)]
        self.operation_counts
            .normalized_original_value_parses
            .fetch_add(1, Ordering::Relaxed);
    }

    fn record_normalized_json_serialization(&self) {
        #[cfg(test)]
        self.operation_counts
            .normalized_json_serializations
            .fetch_add(1, Ordering::Relaxed);
    }

    fn record_payload_guard_serializations(&self, count: usize) {
        #[cfg(test)]
        self.operation_counts
            .payload_guard_serializations
            .fetch_add(count as u64, Ordering::Relaxed);
        #[cfg(not(test))]
        let _ = count;
    }

    fn record_known_tool_name_build(&self) {
        #[cfg(test)]
        self.operation_counts
            .known_tool_name_builds
            .fetch_add(1, Ordering::Relaxed);
    }

    fn record_usage_projection_build(&self) {
        #[cfg(test)]
        self.operation_counts
            .usage_projection_builds
            .fetch_add(1, Ordering::Relaxed);
    }

    #[cfg(test)]
    fn operation_counts(&self) -> ExternalRequestPreparationOperationCounts {
        ExternalRequestPreparationOperationCounts {
            raw_payload_parses: self
                .operation_counts
                .raw_payload_parses
                .load(Ordering::Relaxed),
            normalized_base_builds: self
                .operation_counts
                .normalized_base_builds
                .load(Ordering::Relaxed),
            normalized_original_value_parses: self
                .operation_counts
                .normalized_original_value_parses
                .load(Ordering::Relaxed),
            normalized_json_serializations: self
                .operation_counts
                .normalized_json_serializations
                .load(Ordering::Relaxed),
            payload_guard_serializations: self
                .operation_counts
                .payload_guard_serializations
                .load(Ordering::Relaxed),
            known_tool_name_builds: self
                .operation_counts
                .known_tool_name_builds
                .load(Ordering::Relaxed),
            usage_projection_builds: self
                .operation_counts
                .usage_projection_builds
                .load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone)]
pub struct ExternalRouteRequest {
    /// Immutable bytes after entry defaults, before compatibility/body transformations.
    pub effective_raw_body: Bytes,
    /// Probe bound to `effective_raw_body`; shared across every failover candidate.
    pub(crate) effective_raw_probe: Option<Arc<RawMessagesBodyProbe>>,
    pub(crate) preparation_cache: Arc<ExternalRouteRequestPreparationCache>,
    /// Mutable working serialization used by normalized/local processing.
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
    pub inference_attempt_budget: Arc<InferenceAttemptBudget>,
    pub request_api_key_id: Option<String>,
}

fn route_allows_degraded_fallback_local_lease(route: &ExternalRouteRequest) -> bool {
    matches!(
        route.route_subtype,
        UsageRouteSubtype::ExternalFallbackPreflight
            | UsageRouteSubtype::ExternalFallbackAfterLocalAttempts
    ) && route.fallback_reason.as_deref() == Some("local_scheduler_redis_degraded")
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

    fn model_cooldown_candidates(&self) -> Vec<String> {
        let mut out = Vec::new();
        for model in [
            self.payload.as_ref().map(|payload| payload.model.as_str()),
            self.model_hint.as_deref(),
            self.upstream_model.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(normalized) = normalize_external_pool_model_cooldown_key_model(model)
                .filter(|normalized| !out.iter().any(|existing| existing == normalized))
            {
                out.push(normalized);
            }
        }
        out
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

    fn reset_preparation_cache(&mut self) {
        self.preparation_cache = Arc::new(ExternalRouteRequestPreparationCache::default());
    }

    #[cfg(test)]
    fn preparation_operation_counts(&self) -> ExternalRequestPreparationOperationCounts {
        self.preparation_cache.operation_counts()
    }
}

fn external_route_raw_projection_payload(
    route: &ExternalRouteRequest,
) -> Option<Arc<MessagesRequest>> {
    route
        .preparation_cache
        .raw_projection_payload
        .get_or_init(|| {
            let probe = route
                .effective_raw_probe
                .as_deref()
                .filter(|probe| probe.matches_body(&route.effective_raw_body))?;
            route.preparation_cache.record_raw_payload_parse();
            deserialize_messages_request_with_probe(&route.effective_raw_body, probe)
                .ok()
                .map(Arc::new)
        })
        .clone()
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
            inference_attempts: None,
            auxiliary_attempts: None,
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
            stream_retry_attempts: None,
            stream_retry_reasons: None,
            stream_retry_dispatch_failures: None,
            client_dropped_ms: None,
            terminal_reason: None,
            upstream_message_status: None,
            saw_upstream_completed: None,
            stop_reason_source: None,
            suspected_intent_preamble_end_turn: None,
            intent_preamble_risk: None,
            suspected_tool_context_leak_end_turn: None,
            tool_context_leak_markers: None,
            suppressed_tool_context_leak_blocks: None,
            suppressed_tool_context_leak_chars: None,
            suppressed_tool_context_leak_kinds: None,
            assistant_tail_intent_hint: None,
            end_turn_anomaly_reason: None,
            end_turn_anomaly_risk: None,
            upstream_eof_without_completed: None,
            last_upstream_event_type: None,
            last_upstream_events: None,
            saw_upstream_assistant_response: None,
            saw_upstream_tool_use: None,
            saw_upstream_metadata: None,
            last_assistant_content_chars: None,
            filtered_trivial_text_blocks: None,
            filtered_trivial_text_chars: None,
        };
        (!trace.is_empty()).then_some(trace)
    }
}

fn load_nonzero(value: &AtomicU64) -> Option<u64> {
    let value = value.load(Ordering::Acquire);
    (value > 0).then_some(value)
}

fn external_pool_coordination_monotonic_ms() -> u64 {
    static STARTED_AT: OnceLock<Instant> = OnceLock::new();
    STARTED_AT
        .get_or_init(Instant::now)
        .elapsed()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn external_dispatch_deadline(
    route: &ExternalRouteRequest,
    config: &ExternalPoolsConfig,
) -> Option<Instant> {
    let timeout_secs = if route.is_stream() {
        config
            .external_pool_stream_request_timeout_secs
            .max(config.external_pool_request_timeout_secs)
    } else {
        config.external_pool_request_timeout_secs
    };
    let configured =
        (timeout_secs > 0).then(|| route.started_at + Duration::from_secs(timeout_secs));
    let shared = route
        .inference_attempt_budget
        .dispatch_deadline()
        .map(Instant::from_std);
    match (configured, shared) {
        (Some(configured), Some(shared)) => Some(configured.min(shared)),
        (Some(configured), None) => Some(configured),
        (None, Some(shared)) => Some(shared),
        (None, None) => None,
    }
}

struct ExternalForwardResponse {
    response: Response,
    outbound_model: Option<String>,
    outbound_body: Bytes,
    billing: Option<ExternalPoolBilling>,
    stream_usage_capture: Option<Arc<SyncMutex<ExternalUsageCapture>>>,
    stream_usage_projection: Option<ExternalUsageProjectionContext>,
}

struct PreparedExternalForwardRequest {
    request: reqwest::RequestBuilder,
    outbound_model: Option<String>,
    outbound_body: Bytes,
    known_tool_names: Arc<Vec<String>>,
    response_body_timeout_secs: u64,
}

struct ExternalStreamUsageRecordContext {
    config: ExternalPoolsConfig,
    route: ExternalRouteRequest,
    pool: ExternalPool,
    attempts: Vec<ExternalPoolAttempt>,
    outbound_model: Option<String>,
    outbound_body: Bytes,
    usage_capture: Option<Arc<SyncMutex<ExternalUsageCapture>>>,
    usage_projection: Option<ExternalUsageProjectionContext>,
}

struct ExternalForwardError {
    err: ExternalPoolError,
    outbound_model: Option<String>,
    attempt_rejection: Option<InferenceAttemptRejection>,
}

impl ExternalForwardError {
    fn new(err: ExternalPoolError, outbound_model: Option<String>) -> Self {
        Self {
            err,
            outbound_model,
            attempt_rejection: None,
        }
    }

    fn dispatch_rejected(
        err: ExternalPoolError,
        outbound_model: Option<String>,
        rejection: InferenceAttemptRejection,
    ) -> Self {
        Self {
            err,
            outbound_model,
            attempt_rejection: Some(rejection),
        }
    }
}

impl From<ExternalPoolError> for ExternalForwardError {
    fn from(err: ExternalPoolError) -> Self {
        Self::new(err, None)
    }
}

fn external_pool_lease_lost_forward_error(outbound_model: Option<String>) -> ExternalForwardError {
    ExternalForwardError::new(
        ExternalPoolError {
            status: Some(StatusCode::SERVICE_UNAVAILABLE),
            message: "external pool lease coordination was lost".to_string(),
            retryable: false,
            auto_disable_reason: None,
            cooldown: None,
            protocol_error: None,
        },
        outbound_model,
    )
}

fn external_response_body_read_error(
    err: HttpSendError,
    status: StatusCode,
    max_bytes: usize,
    outbound_model: Option<String>,
) -> ExternalForwardError {
    let (message, retryable) = match err {
        HttpSendError::ResponseBodyTooLarge { .. } => (
            format!("model endpoint response body exceeds {max_bytes} bytes"),
            false,
        ),
        HttpSendError::ResponseBodyTimeout { timeout_secs } => (
            format!("model endpoint response body timeout after {timeout_secs} seconds"),
            true,
        ),
        HttpSendError::ResponseHeaderTimeout { timeout_secs } => (
            format!("model endpoint response header timeout after {timeout_secs} seconds"),
            true,
        ),
        HttpSendError::Request(err) => (
            sanitized_external_network_error("response read failed", &err),
            true,
        ),
    };
    ExternalForwardError::new(
        ExternalPoolError {
            status: Some(status),
            message,
            retryable,
            auto_disable_reason: None,
            cooldown: None,
            protocol_error: None,
        },
        outbound_model,
    )
}

fn inference_attempt_rejection_reason(rejection: InferenceAttemptRejection) -> &'static str {
    match rejection {
        InferenceAttemptRejection::Exhausted => "inference_attempt_limit",
        InferenceAttemptRejection::ReservedForFallback => "inference_attempt_reserved_for_fallback",
        InferenceAttemptRejection::DownstreamCommitted => "downstream_committed",
    }
}

fn external_attempt_rejection_final_error(
    route: &ExternalRouteRequest,
    attempts: Vec<ExternalPoolAttempt>,
) -> ExternalPoolFinalError {
    ExternalPoolFinalError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        response_error_type: "service_unavailable".to_string(),
        route_error_type: "inference_attempt_policy".to_string(),
        message: "Request could not be completed. Please retry shortly.".to_string(),
        error_id: route.error_id.clone(),
        retryable: false,
        attempts,
        pool_id: None,
        pool_name: None,
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
            [("x-error-id", self.error_id)],
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
    estimated_output_tokens: i32,
    raw: Option<CacheUsage>,
    shaped: Option<CacheUsage>,
    reported: Option<CacheUsage>,
    projected: bool,
    usage_estimated: bool,
    usage_estimate_reason: Option<String>,
    usage_candidate_path: Option<String>,
    body_usage_projection_applied: bool,
    stream_error_message: Option<String>,
    stream_response_mode: Option<ExternalPoolStreamResponseMode>,
    debug_stream: ExternalUsageDebugStreamCapture,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExternalUsageDebugStreamCapture {
    events_seen: u64,
    data_lines_seen: u64,
    done_events_seen: u64,
    json_parse_errors: u64,
    usage_events_seen: u64,
    raw_stream_bytes_seen: u64,
    raw_stream_preview_base64: String,
    raw_stream_preview_utf8: String,
    raw_stream_preview_truncated: bool,
    event_types: BTreeMap<String, u64>,
    usage_paths: BTreeMap<String, u64>,
    raw_usage_event_samples: Vec<ExternalUsageDebugRawEventSample>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExternalUsageDebugRawEventSample {
    event: Option<String>,
    payload_type: Option<String>,
    raw_event_utf8: String,
    raw_event_base64: String,
    raw_event_truncated: bool,
    usage_candidates: Vec<ExternalUsageDebugUsageCandidate>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExternalUsageDebugUsageCandidate {
    path: String,
    raw_json: String,
    raw_json_truncated: bool,
    normalized_anthropic_usage: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExternalStreamProcessingPlan {
    response_mode: ExternalPoolStreamResponseMode,
    mask_errors: bool,
    capture_usage: bool,
    usage_debug_enabled: bool,
    usage_debug_max_body_bytes: usize,
}

impl ExternalStreamProcessingPlan {
    fn from_mode(response_mode: ExternalPoolStreamResponseMode) -> Self {
        match response_mode {
            ExternalPoolStreamResponseMode::EventPassthrough => Self {
                response_mode,
                mask_errors: true,
                capture_usage: true,
                usage_debug_enabled: false,
                usage_debug_max_body_bytes: 0,
            },
        }
    }

    fn for_pool(pool: &ExternalPool, config: &ExternalPoolsConfig) -> Self {
        let mut plan = Self::from_mode(effective_external_pool_stream_response_mode(pool, config));
        if external_pool_usage_debug_enabled(config) {
            plan.usage_debug_enabled = true;
            plan.usage_debug_max_body_bytes = external_pool_usage_debug_max_body_bytes(config);
        }
        plan
    }
}

fn effective_external_pool_stream_response_mode(
    pool: &ExternalPool,
    config: &ExternalPoolsConfig,
) -> ExternalPoolStreamResponseMode {
    pool.stream_response_mode
        .unwrap_or(config.external_pool_stream_response_mode)
}

fn external_pool_usage_debug_enabled(config: &ExternalPoolsConfig) -> bool {
    config.external_pool_usage_debug_enabled && config.external_pool_usage_debug_max_files > 0
}

fn external_pool_usage_debug_dir(config: &ExternalPoolsConfig) -> PathBuf {
    let configured = config.external_pool_usage_debug_dir.trim();
    if configured.is_empty() {
        PathBuf::from(EXTERNAL_POOL_USAGE_DEBUG_DEFAULT_DIR)
    } else {
        PathBuf::from(configured)
    }
}

fn external_pool_usage_debug_max_body_bytes(config: &ExternalPoolsConfig) -> usize {
    let configured = config.external_pool_usage_debug_max_body_bytes as usize;
    if configured == 0 {
        EXTERNAL_POOL_USAGE_DEBUG_DEFAULT_MAX_BODY_BYTES
    } else {
        configured.min(EXTERNAL_POOL_USAGE_DEBUG_MAX_BODY_BYTES)
    }
}

fn external_pool_usage_debug_max_files(config: &ExternalPoolsConfig) -> u64 {
    let configured = config.external_pool_usage_debug_max_files as u64;
    if configured == 0 {
        EXTERNAL_POOL_USAGE_DEBUG_DEFAULT_MAX_FILES
    } else {
        configured
    }
}

fn spawn_external_pool_usage_debug_write(
    config: &ExternalPoolsConfig,
    route: &ExternalRouteRequest,
    stage: &'static str,
    payload: serde_json::Value,
) {
    if !external_pool_usage_debug_enabled(config) {
        return;
    }
    let max_files = external_pool_usage_debug_max_files(config);
    let sequence = EXTERNAL_POOL_USAGE_DEBUG_WRITE_COUNT.fetch_add(1, Ordering::Relaxed);
    if sequence >= max_files {
        if sequence == max_files {
            tracing::warn!(
                request_id = %route.request_id,
                max_files,
                "external pool usage debug file limit reached"
            );
        }
        return;
    }
    let dir = external_pool_usage_debug_dir(config);
    let request_id = sanitize_usage_debug_filename_part(&route.request_id);
    let timestamp = Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string();
    let file_name = format!("{timestamp}-{request_id}-{sequence:06}-{stage}.json");
    let path = dir.join(file_name);
    let request_id_for_log = route.request_id.clone();
    tokio::spawn(async move {
        let result = async {
            tokio::fs::create_dir_all(&dir).await?;
            let body = serde_json::to_vec_pretty(&payload)
                .map_err(|err| std::io::Error::other(err.to_string()))?;
            tokio::fs::write(&path, body).await
        }
        .await;
        if let Err(err) = result {
            tracing::warn!(
                request_id = %request_id_for_log,
                path = %path.display(),
                error = %err,
                "external pool usage debug write failed"
            );
        }
    });
}

fn sanitize_usage_debug_filename_part(value: &str) -> String {
    let mut out = String::with_capacity(value.len().min(64));
    for ch in value.chars().take(80) {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "request".to_string()
    } else {
        out
    }
}

fn external_pool_usage_debug_route_context(
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
    outbound_model: Option<&str>,
) -> serde_json::Value {
    json!({
        "requestId": route.request_id.as_str(),
        "errorId": route.error_id.as_str(),
        "createdAt": Utc::now().to_rfc3339(),
        "endpoint": route.endpoint.as_str(),
        "stream": route.is_stream(),
        "requestedModel": route.requested_model(),
        "requestedMaxTokens": route.requested_max_tokens(),
        "upstreamModel": route.upstream_model.as_deref(),
        "externalOutboundModel": outbound_model,
        "conversationId": route.stable_conversation_id(),
        "requestApiKeyId": route.request_api_key_id.as_deref(),
        "routeSubtype": route.route_subtype,
        "fallbackReason": route.fallback_reason.as_deref(),
        "directPolicyReason": route.direct_policy_reason.as_deref(),
        "localAttempted": route.local_attempted,
        "requestInputTokens": route.request_input_tokens,
        "pool": {
            "id": pool.id,
            "name": pool.name.as_str(),
            "usageProjectionMode": pool.usage_projection_mode.as_str(),
        },
    })
}

fn external_pool_usage_debug_bytes(value: &[u8], max_bytes: usize) -> serde_json::Value {
    let capped = value.len().min(max_bytes);
    let preview = &value[..capped];
    let mut hasher = Sha256::new();
    hasher.update(value);
    json!({
        "bytes": value.len(),
        "sha256": hex::encode(hasher.finalize()),
        "previewBytes": capped,
        "previewTruncated": value.len() > capped,
        "previewUtf8": String::from_utf8_lossy(preview),
        "previewBase64": BASE64_STANDARD.encode(preview),
    })
}

fn truncate_usage_debug_text(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_string(), false);
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_string(), true)
}

fn external_pool_usage_debug_cache_usage(value: Option<CacheUsage>) -> Option<serde_json::Value> {
    value.map(|usage| usage.to_anthropic_usage_json())
}

fn external_pool_usage_debug_capture(capture: &ExternalUsageCapture) -> serde_json::Value {
    json!({
        "requestInputTokens": capture.request_input_tokens,
        "estimatedOutputTokens": capture.estimated_output_tokens,
        "rawUsage": external_pool_usage_debug_cache_usage(capture.raw),
        "shapedUsage": external_pool_usage_debug_cache_usage(capture.shaped),
        "reportedUsage": external_pool_usage_debug_cache_usage(capture.reported),
        "projected": capture.projected,
        "usageEstimated": capture.usage_estimated,
        "usageEstimateReason": capture.usage_estimate_reason.as_deref(),
        "usageCandidatePath": capture.usage_candidate_path.as_deref(),
        "bodyUsageProjectionApplied": capture.body_usage_projection_applied,
        "streamErrorMessage": capture.stream_error_message.as_deref(),
        "streamResponseMode": capture.stream_response_mode.map(|mode| mode.as_str()),
    })
}

fn external_pool_usage_debug_header_context(headers: &HeaderMap) -> serde_json::Value {
    json!({
        "contentType": headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default(),
        "anthropicRequestId": headers
            .get("request-id")
            .or_else(|| headers.get("x-request-id"))
            .or_else(|| headers.get("anthropic-request-id"))
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default(),
    })
}

struct ExternalUsageDebugNonStreamRecordContext<'a> {
    config: &'a ExternalPoolsConfig,
    route: &'a ExternalRouteRequest,
    pool: &'a ExternalPool,
    outbound_model: Option<&'a str>,
    status: StatusCode,
    response_headers: &'a HeaderMap,
    outbound_body: &'a Bytes,
    upstream_body: &'a Bytes,
    projected: &'a ProjectedNonStreamBody,
}

fn external_pool_usage_debug_non_stream_record(ctx: ExternalUsageDebugNonStreamRecordContext<'_>) {
    let ExternalUsageDebugNonStreamRecordContext {
        config,
        route,
        pool,
        outbound_model,
        status,
        response_headers,
        outbound_body,
        upstream_body,
        projected,
    } = ctx;
    if !external_pool_usage_debug_enabled(config) {
        return;
    }
    let max_bytes = external_pool_usage_debug_max_body_bytes(config);
    let raw_json = serde_json::from_slice::<serde_json::Value>(upstream_body).ok();
    let payload = json!({
        "kind": "external_pool_usage_debug",
        "stage": "non_stream_success",
        "route": external_pool_usage_debug_route_context(route, pool, outbound_model),
        "upstream": {
            "status": status.as_u16(),
            "headers": external_pool_usage_debug_header_context(response_headers),
            "body": external_pool_usage_debug_bytes(upstream_body, max_bytes),
            "rawUsageCandidates": raw_json
                .as_ref()
                .map(collect_external_pool_usage_debug_candidates)
                .unwrap_or_default(),
        },
        "outboundRequest": {
            "body": external_pool_usage_debug_bytes(outbound_body, max_bytes),
        },
        "kiroRsProcessing": {
            "usageCapture": external_pool_usage_debug_capture(&projected.usage_capture),
            "protocolContamination": projected.protocol_contamination,
            "downstreamBodyChanged": projected.body.as_ref() != upstream_body.as_ref(),
            "downstreamBody": external_pool_usage_debug_bytes(&projected.body, max_bytes),
        },
    });
    spawn_external_pool_usage_debug_write(config, route, "non-stream", payload);
}

struct ExternalUsageDebugStreamRecordContext<'a> {
    config: &'a ExternalPoolsConfig,
    route: &'a ExternalRouteRequest,
    pool: &'a ExternalPool,
    outbound_model: Option<&'a str>,
    status: UsageRecordStatus,
    response_status: StatusCode,
    response_content_type: Option<&'a str>,
    outbound_body: &'a Bytes,
    capture: Option<&'a Arc<SyncMutex<ExternalUsageCapture>>>,
    billing: Option<&'a ExternalPoolBilling>,
    estimated_output_tokens: i32,
    terminal_message: Option<&'a str>,
}

fn external_pool_usage_debug_stream_record(ctx: ExternalUsageDebugStreamRecordContext<'_>) {
    let ExternalUsageDebugStreamRecordContext {
        config,
        route,
        pool,
        outbound_model,
        status,
        response_status,
        response_content_type,
        outbound_body,
        capture,
        billing,
        estimated_output_tokens,
        terminal_message,
    } = ctx;
    if !external_pool_usage_debug_enabled(config) {
        return;
    }
    let max_bytes = external_pool_usage_debug_max_body_bytes(config);
    let capture_snapshot = capture.map(|capture| capture.lock().clone());
    let payload = json!({
        "kind": "external_pool_usage_debug",
        "stage": "stream_final",
        "route": external_pool_usage_debug_route_context(route, pool, outbound_model),
        "upstream": {
            "status": response_status.as_u16(),
            "headers": {
                "contentType": response_content_type.unwrap_or_default(),
            },
            "rawStream": capture_snapshot
                .as_ref()
                .map(|capture| &capture.debug_stream),
        },
        "outboundRequest": {
            "body": external_pool_usage_debug_bytes(outbound_body, max_bytes),
        },
        "kiroRsProcessing": {
            "terminalStatus": status,
            "terminalMessage": terminal_message,
            "estimatedOutputTokensFromForwardedStream": estimated_output_tokens,
            "usageCapture": capture_snapshot
                .as_ref()
                .map(external_pool_usage_debug_capture),
            "billing": billing.map(|billing| {
                json!({
                    "rawUsage": billing.raw_usage,
                    "shapedUsage": billing.shaped_usage,
                    "reportedUsage": billing.reported_usage,
                    "usageEstimated": billing.usage_estimated,
                    "usageEstimateReason": billing.usage_estimate_reason.as_deref(),
                    "usageCandidatePath": billing.usage_candidate_path.as_deref(),
                    "usageProjectionApplied": billing.usage_projection_applied,
                    "bodyUsageProjectionApplied": billing.body_usage_projection_applied,
                })
            }),
        },
    });
    spawn_external_pool_usage_debug_write(config, route, "stream", payload);
}

fn collect_external_pool_usage_debug_candidates(
    value: &serde_json::Value,
) -> Vec<ExternalUsageDebugUsageCandidate> {
    let mut out = Vec::new();
    collect_external_pool_usage_debug_candidates_inner(value, "$", false, &mut out);
    out
}

fn collect_external_pool_usage_debug_candidates_inner(
    value: &serde_json::Value,
    path: &str,
    key_is_usage: bool,
    out: &mut Vec<ExternalUsageDebugUsageCandidate>,
) {
    if out.len() >= EXTERNAL_POOL_USAGE_DEBUG_USAGE_SAMPLE_LIMIT {
        return;
    }
    if (key_is_usage || external_pool_usage_debug_value_has_usage_tokens(value))
        && value.is_object()
    {
        let normalized =
            cache_usage_from_any_value(value).map(|usage| usage.to_anthropic_usage_json());
        let raw_json = serde_json::to_string(value).unwrap_or_else(|_| "null".to_string());
        let (raw_json, raw_json_truncated) =
            truncate_usage_debug_text(&raw_json, EXTERNAL_POOL_USAGE_DEBUG_USAGE_JSON_BYTES);
        out.push(ExternalUsageDebugUsageCandidate {
            path: path.to_string(),
            raw_json,
            raw_json_truncated,
            normalized_anthropic_usage: normalized,
        });
    }
    if out.len() >= EXTERNAL_POOL_USAGE_DEBUG_USAGE_SAMPLE_LIMIT {
        return;
    }
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                if out.len() >= EXTERNAL_POOL_USAGE_DEBUG_USAGE_SAMPLE_LIMIT {
                    break;
                }
                let child_path = external_pool_usage_debug_object_path(path, key);
                collect_external_pool_usage_debug_candidates_inner(
                    child,
                    &child_path,
                    key.eq_ignore_ascii_case("usage"),
                    out,
                );
            }
        }
        serde_json::Value::Array(items) => {
            for (idx, child) in items.iter().enumerate() {
                if out.len() >= EXTERNAL_POOL_USAGE_DEBUG_USAGE_SAMPLE_LIMIT {
                    break;
                }
                let child_path = format!("{path}[{idx}]");
                collect_external_pool_usage_debug_candidates_inner(child, &child_path, false, out);
            }
        }
        _ => {}
    }
}

fn external_pool_usage_debug_object_path(parent: &str, key: &str) -> String {
    let needs_brackets = key
        .chars()
        .any(|ch| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'));
    if needs_brackets {
        format!(
            "{parent}[{}]",
            serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_string())
        )
    } else if parent == "$" {
        format!("$.{key}")
    } else {
        format!("{parent}.{key}")
    }
}

fn external_pool_usage_debug_value_has_usage_tokens(value: &serde_json::Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    [
        "input_tokens",
        "output_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
        "prompt_tokens",
        "completion_tokens",
        "total_tokens",
    ]
    .into_iter()
    .any(|key| obj.get(key).is_some())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExternalPoolSelectionFailureKind {
    AdmissionSaturated,
    BreakerOpen,
    PostgresError,
    PostgresTimeout,
}

impl ExternalPoolSelectionFailureKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::AdmissionSaturated => "admission_saturated",
            Self::BreakerOpen => "breaker_open",
            Self::PostgresError => "postgres_error",
            Self::PostgresTimeout => "postgres_timeout",
        }
    }

    fn coordinator_kind(self) -> PoolCoordinatorUnavailableKind {
        match self {
            Self::AdmissionSaturated => PoolCoordinatorUnavailableKind::AdmissionSaturated,
            Self::BreakerOpen => PoolCoordinatorUnavailableKind::PostgresCircuitOpen,
            Self::PostgresError => PoolCoordinatorUnavailableKind::PostgresError,
            Self::PostgresTimeout => PoolCoordinatorUnavailableKind::PostgresTimeout,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExternalPoolSelectionUnavailable {
    kind: ExternalPoolSelectionFailureKind,
    retry_after: Duration,
}

#[derive(Debug, Default)]
struct ExternalPoolSelectionBreakerState {
    generation: u64,
    consecutive_failures: usize,
    open_until: Option<Instant>,
    recovery_probe_in_flight: bool,
}

#[derive(Debug, Default)]
struct ExternalPoolSelectionBreakerStats {
    postgres_attempts: AtomicU64,
    saturated: AtomicU64,
    fail_fast: AtomicU64,
    recovery_probes: AtomicU64,
    failures: AtomicU64,
    transitions: AtomicU64,
    recoveries: AtomicU64,
    cancelled_probes: AtomicU64,
    suppressed: AtomicU64,
}

#[derive(Debug)]
struct ExternalPoolSelectionBreaker {
    state: SyncMutex<ExternalPoolSelectionBreakerState>,
    stats: ExternalPoolSelectionBreakerStats,
    operation_semaphore: Arc<Semaphore>,
    backoffs: Vec<Duration>,
    jitter_percent: u8,
    min_failure_backoff: Duration,
}

impl Default for ExternalPoolSelectionBreaker {
    fn default() -> Self {
        Self::new_with_min_backoff(
            EXTERNAL_POOL_SELECTION_MAX_IN_FLIGHT,
            EXTERNAL_POOL_SELECTION_BREAKER_BACKOFF_MILLIS
                .into_iter()
                .map(Duration::from_millis)
                .collect(),
            EXTERNAL_POOL_SELECTION_BREAKER_JITTER_PERCENT,
            EXTERNAL_POOL_SELECTION_RETRY_AFTER,
        )
    }
}

impl ExternalPoolSelectionBreaker {
    #[cfg(test)]
    fn new(max_in_flight: usize, backoffs: Vec<Duration>, jitter_percent: u8) -> Self {
        Self::new_with_min_backoff(max_in_flight, backoffs, jitter_percent, Duration::ZERO)
    }

    fn new_with_min_backoff(
        max_in_flight: usize,
        backoffs: Vec<Duration>,
        jitter_percent: u8,
        min_failure_backoff: Duration,
    ) -> Self {
        let backoffs = if backoffs.is_empty() {
            vec![Duration::from_millis(100)]
        } else {
            backoffs
                .into_iter()
                .map(|backoff| backoff.max(Duration::from_millis(1)))
                .collect()
        };
        Self {
            state: SyncMutex::new(ExternalPoolSelectionBreakerState::default()),
            stats: ExternalPoolSelectionBreakerStats::default(),
            operation_semaphore: Arc::new(Semaphore::new(max_in_flight.max(1))),
            backoffs,
            jitter_percent: jitter_percent.min(50),
            min_failure_backoff,
        }
    }

    fn base_backoff_for_failure(&self, consecutive_failures: usize) -> Duration {
        self.backoffs[consecutive_failures
            .saturating_sub(1)
            .min(self.backoffs.len().saturating_sub(1))]
    }

    fn backoff_for_failure(&self, consecutive_failures: usize, generation: u64) -> Duration {
        deterministic_duration_jitter(
            self.base_backoff_for_failure(consecutive_failures),
            self.jitter_percent,
            generation ^ (consecutive_failures as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15),
        )
        .max(self.min_failure_backoff)
    }

    fn try_begin(
        self: &Arc<Self>,
    ) -> Result<ExternalPoolSelectionPermit, ExternalPoolSelectionUnavailable> {
        let operation_permit = match self.operation_semaphore.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                self.stats.saturated.fetch_add(1, Ordering::Relaxed);
                self.stats.fail_fast.fetch_add(1, Ordering::Relaxed);
                self.stats.suppressed.fetch_add(1, Ordering::Relaxed);
                return Err(ExternalPoolSelectionUnavailable {
                    kind: ExternalPoolSelectionFailureKind::AdmissionSaturated,
                    retry_after: Duration::from_millis(100),
                });
            }
        };
        let now = Instant::now();
        let mut state = self.state.lock();
        let recovery_probe = match state.open_until {
            Some(open_until) if open_until > now => {
                self.stats.fail_fast.fetch_add(1, Ordering::Relaxed);
                self.stats.suppressed.fetch_add(1, Ordering::Relaxed);
                return Err(ExternalPoolSelectionUnavailable {
                    kind: ExternalPoolSelectionFailureKind::BreakerOpen,
                    retry_after: open_until.saturating_duration_since(now),
                });
            }
            Some(_) => {
                if state.recovery_probe_in_flight {
                    self.stats.fail_fast.fetch_add(1, Ordering::Relaxed);
                    self.stats.suppressed.fetch_add(1, Ordering::Relaxed);
                    return Err(ExternalPoolSelectionUnavailable {
                        kind: ExternalPoolSelectionFailureKind::BreakerOpen,
                        retry_after: EXTERNAL_POOL_SELECTION_RETRY_AFTER,
                    });
                }
                state.recovery_probe_in_flight = true;
                self.stats.recovery_probes.fetch_add(1, Ordering::Relaxed);
                true
            }
            None => false,
        };
        self.stats.postgres_attempts.fetch_add(1, Ordering::Relaxed);
        Ok(ExternalPoolSelectionPermit {
            breaker: self.clone(),
            generation: state.generation,
            recovery_probe,
            completed: false,
            _operation_permit: operation_permit,
        })
    }

    fn complete_success(&self, generation: u64, recovery_probe: bool) {
        let mut state = self.state.lock();
        if generation != state.generation {
            return;
        }
        if recovery_probe {
            state.generation = state.generation.wrapping_add(1);
            state.consecutive_failures = 0;
            state.open_until = None;
            state.recovery_probe_in_flight = false;
            self.stats.recoveries.fetch_add(1, Ordering::Relaxed);
            let suppressed = self.stats.suppressed.swap(0, Ordering::Relaxed);
            tracing::info!(
                suppressed_requests = suppressed,
                "外部池 PostgreSQL selection 恢复，关闭准入熔断"
            );
        }
    }

    fn complete_failure(
        &self,
        generation: u64,
        recovery_probe: bool,
        kind: ExternalPoolSelectionFailureKind,
    ) {
        self.stats.failures.fetch_add(1, Ordering::Relaxed);
        let mut state = self.state.lock();
        if generation != state.generation {
            return;
        }
        state.recovery_probe_in_flight = false;
        state.consecutive_failures = state.consecutive_failures.saturating_add(1);
        let backoff = self.backoff_for_failure(state.consecutive_failures, state.generation);
        state.open_until = Some(Instant::now() + backoff);
        state.generation = state.generation.wrapping_add(1);
        self.stats.transitions.fetch_add(1, Ordering::Relaxed);
        let suppressed = self.stats.suppressed.swap(0, Ordering::Relaxed);
        tracing::warn!(
            failure_kind = kind.as_str(),
            recovery_probe,
            consecutive_failures = state.consecutive_failures,
            backoff_ms = backoff.as_millis(),
            suppressed_requests = suppressed,
            "外部池 PostgreSQL selection 不可用，短期暂停新的查询"
        );
    }

    fn cancel_probe(&self, generation: u64) {
        let mut state = self.state.lock();
        if generation != state.generation || !state.recovery_probe_in_flight {
            return;
        }
        state.recovery_probe_in_flight = false;
        let failure_count = state.consecutive_failures.max(1);
        let backoff = self.backoff_for_failure(failure_count, state.generation);
        state.open_until = Some(Instant::now() + backoff);
        state.generation = state.generation.wrapping_add(1);
        self.stats.cancelled_probes.fetch_add(1, Ordering::Relaxed);
    }
}

struct ExternalPoolSelectionPermit {
    breaker: Arc<ExternalPoolSelectionBreaker>,
    generation: u64,
    recovery_probe: bool,
    completed: bool,
    _operation_permit: OwnedSemaphorePermit,
}

impl ExternalPoolSelectionPermit {
    fn success(mut self) {
        self.completed = true;
        self.breaker
            .complete_success(self.generation, self.recovery_probe);
    }

    fn failure(mut self, kind: ExternalPoolSelectionFailureKind) {
        self.completed = true;
        self.breaker
            .complete_failure(self.generation, self.recovery_probe, kind);
    }
}

impl Drop for ExternalPoolSelectionPermit {
    fn drop(&mut self) {
        if !self.completed && self.recovery_probe {
            self.breaker.cancel_probe(self.generation);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExternalPoolCoordinatorFailureKind {
    RedisError,
    Timeout,
}

impl ExternalPoolCoordinatorFailureKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::RedisError => "redis_error",
            Self::Timeout => "timeout",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ExternalPoolCoordinatorOpen {
    retry_after: Duration,
}

#[derive(Debug)]
struct ExternalPoolCoordinatorBreakerState {
    generation: u64,
    consecutive_failures: usize,
    open_until: Option<Instant>,
    recovery_probe_in_flight: bool,
}

impl Default for ExternalPoolCoordinatorBreakerState {
    fn default() -> Self {
        Self {
            generation: 0,
            consecutive_failures: 0,
            open_until: None,
            recovery_probe_in_flight: false,
        }
    }
}

#[derive(Debug, Default)]
struct ExternalPoolCoordinatorBreakerStats {
    admitted: AtomicU64,
    saturated: AtomicU64,
    fail_fast: AtomicU64,
    recovery_probes: AtomicU64,
    failures: AtomicU64,
    recoveries: AtomicU64,
    cancelled_probes: AtomicU64,
    suppressed: AtomicU64,
}

#[derive(Debug)]
struct ExternalPoolCoordinatorBreaker {
    state: SyncMutex<ExternalPoolCoordinatorBreakerState>,
    stats: ExternalPoolCoordinatorBreakerStats,
    backoffs: Vec<Duration>,
    operation_semaphore: Arc<Semaphore>,
}

impl Default for ExternalPoolCoordinatorBreaker {
    fn default() -> Self {
        Self::new(
            EXTERNAL_POOL_COORDINATOR_BREAKER_BACKOFF_SECS
                .into_iter()
                .map(Duration::from_secs)
                .collect(),
        )
    }
}

impl ExternalPoolCoordinatorBreaker {
    fn new(backoffs: Vec<Duration>) -> Self {
        let backoffs = if backoffs.is_empty() {
            vec![Duration::from_secs(1)]
        } else {
            backoffs
                .into_iter()
                .map(|backoff| backoff.max(Duration::from_millis(1)))
                .collect()
        };
        Self {
            state: SyncMutex::new(ExternalPoolCoordinatorBreakerState::default()),
            stats: ExternalPoolCoordinatorBreakerStats::default(),
            backoffs,
            operation_semaphore: Arc::new(Semaphore::new(EXTERNAL_POOL_COORDINATOR_MAX_IN_FLIGHT)),
        }
    }

    fn backoff_for_failure(&self, consecutive_failures: usize) -> Duration {
        self.backoffs[consecutive_failures
            .saturating_sub(1)
            .min(self.backoffs.len().saturating_sub(1))]
    }

    fn try_begin(
        self: &Arc<Self>,
    ) -> Result<ExternalPoolCoordinatorPermit, ExternalPoolCoordinatorOpen> {
        let operation_permit = match self.operation_semaphore.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                self.stats.saturated.fetch_add(1, Ordering::Relaxed);
                self.stats.fail_fast.fetch_add(1, Ordering::Relaxed);
                self.stats.suppressed.fetch_add(1, Ordering::Relaxed);
                return Err(ExternalPoolCoordinatorOpen {
                    retry_after: Duration::from_millis(100),
                });
            }
        };
        let now = Instant::now();
        let mut state = self.state.lock();
        let recovery_probe = match state.open_until {
            Some(open_until) if open_until > now => {
                self.stats.fail_fast.fetch_add(1, Ordering::Relaxed);
                self.stats.suppressed.fetch_add(1, Ordering::Relaxed);
                return Err(ExternalPoolCoordinatorOpen {
                    retry_after: open_until.saturating_duration_since(now),
                });
            }
            Some(_) => {
                if state.recovery_probe_in_flight {
                    self.stats.fail_fast.fetch_add(1, Ordering::Relaxed);
                    self.stats.suppressed.fetch_add(1, Ordering::Relaxed);
                    return Err(ExternalPoolCoordinatorOpen {
                        retry_after: Duration::from_millis(100),
                    });
                }
                state.recovery_probe_in_flight = true;
                self.stats.recovery_probes.fetch_add(1, Ordering::Relaxed);
                true
            }
            None => false,
        };
        self.stats.admitted.fetch_add(1, Ordering::Relaxed);
        Ok(ExternalPoolCoordinatorPermit {
            breaker: self.clone(),
            generation: state.generation,
            recovery_probe,
            completed: false,
            _operation_permit: operation_permit,
        })
    }

    fn complete_success(&self, generation: u64, recovery_probe: bool) {
        let mut state = self.state.lock();
        if generation != state.generation {
            return;
        }
        if recovery_probe {
            state.generation = state.generation.wrapping_add(1);
            state.consecutive_failures = 0;
            state.open_until = None;
            state.recovery_probe_in_flight = false;
            self.stats.recoveries.fetch_add(1, Ordering::Relaxed);
            let suppressed = self.stats.suppressed.swap(0, Ordering::Relaxed);
            tracing::info!(
                suppressed_requests = suppressed,
                "外部池 Redis coordinator 恢复，关闭准入熔断"
            );
        }
    }

    fn complete_failure(
        &self,
        generation: u64,
        recovery_probe: bool,
        kind: ExternalPoolCoordinatorFailureKind,
    ) {
        let mut state = self.state.lock();
        if generation != state.generation {
            return;
        }
        state.recovery_probe_in_flight = false;
        state.consecutive_failures = state.consecutive_failures.saturating_add(1);
        let backoff = self.backoff_for_failure(state.consecutive_failures);
        state.open_until = Some(Instant::now() + backoff);
        state.generation = state.generation.wrapping_add(1);
        self.stats.failures.fetch_add(1, Ordering::Relaxed);
        let suppressed = self.stats.suppressed.swap(0, Ordering::Relaxed);
        tracing::warn!(
            failure_kind = kind.as_str(),
            recovery_probe,
            consecutive_failures = state.consecutive_failures,
            backoff_ms = backoff.as_millis(),
            suppressed_requests = suppressed,
            "外部池 Redis coordinator 不可用，暂停新的外部池准入"
        );
    }

    fn cancel_probe(&self, generation: u64) {
        let mut state = self.state.lock();
        if generation != state.generation || !state.recovery_probe_in_flight {
            return;
        }
        state.recovery_probe_in_flight = false;
        let backoff = self.backoff_for_failure(state.consecutive_failures.max(1));
        state.open_until = Some(Instant::now() + backoff);
        state.generation = state.generation.wrapping_add(1);
        self.stats.cancelled_probes.fetch_add(1, Ordering::Relaxed);
    }
}

struct ExternalPoolCoordinatorPermit {
    breaker: Arc<ExternalPoolCoordinatorBreaker>,
    generation: u64,
    recovery_probe: bool,
    completed: bool,
    _operation_permit: OwnedSemaphorePermit,
}

impl ExternalPoolCoordinatorPermit {
    fn success(mut self) {
        self.completed = true;
        self.breaker
            .complete_success(self.generation, self.recovery_probe);
    }

    fn failure(mut self, kind: ExternalPoolCoordinatorFailureKind) {
        self.completed = true;
        self.breaker
            .complete_failure(self.generation, self.recovery_probe, kind);
    }
}

impl Drop for ExternalPoolCoordinatorPermit {
    fn drop(&mut self) {
        if !self.completed && self.recovery_probe {
            self.breaker.cancel_probe(self.generation);
        }
    }
}

#[derive(Clone)]
pub struct ExternalPoolManager {
    postgres: Arc<PostgresStore>,
    redis: Arc<RedisStore>,
    instance_id: String,
    client: reqwest::Client,
    capacity_signal: Arc<CapacitySignal>,
    #[cfg(test)]
    availability_cache: Arc<SyncMutex<Option<CachedPoolAvailabilitySnapshot>>>,
    static_pool_snapshot: Arc<SyncMutex<Option<CachedStaticPoolSnapshot>>>,
    static_pool_snapshot_generation: Arc<AtomicU64>,
    static_pool_snapshot_refresh_lock: Arc<AsyncMutex<()>>,
    authoritative_pool_snapshot: Arc<SyncMutex<Option<CachedAuthoritativePoolSnapshot>>>,
    authoritative_pool_snapshot_refresh_lock: Arc<AsyncMutex<()>>,
    authoritative_pool_snapshot_changed: Arc<Notify>,
    selection_runtime_snapshot: Arc<SyncMutex<Option<CachedSelectionRuntimeSnapshot>>>,
    selection_runtime_snapshot_refresh_lock: Arc<AsyncMutex<()>>,
    static_pool_snapshot_fresh_ttl: Duration,
    static_pool_snapshot_stale_ttl: Duration,
    static_pool_snapshot_failure_retry: Duration,
    static_pool_snapshot_refresh_timeout: Duration,
    #[cfg(test)]
    static_pool_snapshot_pg_loads: Arc<AtomicU64>,
    #[cfg(test)]
    static_pool_snapshot_background_refreshes: Arc<AtomicU64>,
    #[cfg(test)]
    static_pool_snapshot_background_in_flight: Arc<AtomicU64>,
    #[cfg(test)]
    authoritative_pool_snapshot_pg_loads: Arc<AtomicU64>,
    coordinator_breaker: Arc<ExternalPoolCoordinatorBreaker>,
    selection_breaker: Arc<ExternalPoolSelectionBreaker>,
    selection_saturated: Arc<AtomicU64>,
    dispatch_fence_flights: Arc<SyncMutex<HashMap<(u64, u64), Arc<PoolDispatchFenceFlight>>>>,
    #[cfg(test)]
    dispatch_fence_pg_loads: Arc<AtomicU64>,
    #[cfg(test)]
    dispatch_after_prepare_gate: Arc<SyncMutex<Option<Arc<TestDispatchAfterPrepareGate>>>>,
    observed_pool_data_generation: Arc<AtomicU64>,
    success_reset_recent: Arc<SyncMutex<HashMap<u64, Instant>>>,
    success_reset_semaphore: Arc<Semaphore>,
    #[cfg(test)]
    success_reset_tasks_started: Arc<AtomicU64>,
    #[cfg(test)]
    success_reset_tasks_in_flight: Arc<AtomicU64>,
    coordinator_reconcile_lock: Arc<AsyncMutex<()>>,
    coordinator_epoch: Arc<SyncMutex<Option<String>>>,
    coordinator_run_id: Arc<SyncMutex<Option<String>>>,
    coordinator_probe_required: Arc<AtomicBool>,
    coordinator_next_probe_ms: Arc<AtomicU64>,
    coordinator_recovery_grace: Duration,
    release_dispatcher: Arc<ExternalPoolReleaseDispatcher>,
    degraded_fallback_local_leases: Arc<SyncMutex<HashMap<(u64, u32), Arc<Semaphore>>>>,
    local_mutation_dispatch_trust: Arc<SyncMutex<HashMap<(u64, u64), Instant>>>,
}

struct ExternalPoolLease {
    manager: ExternalPoolManager,
    pool_id: u64,
    lease_id: String,
    coordination_epoch: String,
    state: ExternalPoolLeaseState,
    max_age: Duration,
    heartbeat: Option<ExternalPoolLeaseHeartbeat>,
    release_permit: Option<OwnedSemaphorePermit>,
}

struct ExternalPoolLeaseHeartbeat {
    state: Arc<ExternalPoolLeaseHeartbeatState>,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Default)]
struct ExternalPoolLeaseHeartbeatState {
    lost: AtomicBool,
    attempts: AtomicU64,
    failures: AtomicU64,
    lost_notify: Notify,
}

impl ExternalPoolLeaseHeartbeatState {
    fn mark_lost(&self) {
        if !self.lost.swap(true, Ordering::AcqRel) {
            self.lost_notify.notify_waiters();
        }
    }

    async fn wait_until_lost(&self) {
        loop {
            if self.lost.load(Ordering::Acquire) {
                return;
            }
            let notified = self.lost_notify.notified();
            if self.lost.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
}

impl ExternalPoolLeaseHeartbeat {
    fn spawn(
        manager: ExternalPoolManager,
        pool_id: u64,
        lease_id: String,
        coordination_epoch: String,
        max_age: Duration,
    ) -> Self {
        let state = Arc::new(ExternalPoolLeaseHeartbeatState::default());
        let task_state = state.clone();
        let task = tokio::spawn(async move {
            run_external_pool_lease_heartbeat(
                manager,
                pool_id,
                lease_id,
                coordination_epoch,
                max_age,
                task_state,
            )
            .await;
        });
        Self { state, task }
    }

    async fn wait_until_lost(&self) {
        self.state.wait_until_lost().await;
    }

    fn stop(self) {
        self.task.abort();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExternalPoolLeaseState {
    Pending,
    Confirmed,
    DegradedFallbackLocal,
    Disarmed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ExternalPoolLeaseReleaseKind {
    Pending,
    Confirmed,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ExternalPoolReleaseIntent {
    pool_id: u64,
    lease_id: String,
    release_kind: ExternalPoolLeaseReleaseKind,
}

#[derive(Default)]
struct ExternalPoolReleaseDispatcherState {
    pending: HashMap<ExternalPoolReleaseIntent, ExternalPoolPendingRelease>,
    order: VecDeque<ExternalPoolReleaseIntent>,
    system_failures: u32,
    system_retry_at: Option<Instant>,
    worker_running: bool,
}

struct ExternalPoolPendingRelease {
    _permit: OwnedSemaphorePermit,
    failures: u32,
    next_attempt_at: Instant,
}

impl ExternalPoolReleaseDispatcherState {
    fn next_ready_batch(
        &mut self,
        now: Instant,
        limit: usize,
    ) -> (Vec<ExternalPoolReleaseIntent>, Option<Instant>) {
        if let Some(retry_at) = self.system_retry_at {
            if retry_at > now {
                return (Vec::new(), Some(retry_at));
            }
            self.system_retry_at = None;
        }
        let candidates = self.order.len();
        let mut batch = Vec::with_capacity(limit);
        let mut next_attempt_at: Option<Instant> = None;
        for _ in 0..candidates {
            if batch.len() >= limit {
                break;
            }
            let Some(intent) = self.order.pop_front() else {
                break;
            };
            if let Some(next_attempt) = self
                .pending
                .get(&intent)
                .map(|pending| pending.next_attempt_at)
            {
                self.order.push_back(intent.clone());
                if next_attempt <= now {
                    batch.push(intent);
                } else {
                    next_attempt_at = Some(
                        next_attempt_at
                            .map(|next| next.min(next_attempt))
                            .unwrap_or(next_attempt),
                    );
                }
            }
        }
        (batch, next_attempt_at)
    }
}

#[derive(Debug, Default)]
struct ExternalPoolReleaseDispatcherStats {
    enqueued: AtomicU64,
    deduplicated: AtomicU64,
    completed: AtomicU64,
    retries: AtomicU64,
    worker_starts: AtomicU64,
    spawn_failures: AtomicU64,
}

struct ExternalPoolReleaseDispatcher {
    redis: Arc<RedisStore>,
    capacity_signal: Arc<CapacitySignal>,
    selection_runtime_snapshot: Arc<SyncMutex<Option<CachedSelectionRuntimeSnapshot>>>,
    capacity: Arc<Semaphore>,
    drained_notify: Notify,
    work_notify: Notify,
    state: SyncMutex<ExternalPoolReleaseDispatcherState>,
    stats: ExternalPoolReleaseDispatcherStats,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExternalPoolReleaseDrainReport {
    pub drained: bool,
    pub pending: usize,
    pub enqueued: u64,
    pub completed: u64,
    pub retries: u64,
    pub worker_starts: u64,
    pub spawn_failures: u64,
}

impl ExternalPoolReleaseDispatcher {
    fn new(
        redis: Arc<RedisStore>,
        capacity_signal: Arc<CapacitySignal>,
        selection_runtime_snapshot: Arc<SyncMutex<Option<CachedSelectionRuntimeSnapshot>>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            redis,
            capacity_signal,
            selection_runtime_snapshot,
            capacity: Arc::new(Semaphore::new(EXTERNAL_POOL_RELEASE_CAPACITY)),
            drained_notify: Notify::new(),
            work_notify: Notify::new(),
            state: SyncMutex::new(ExternalPoolReleaseDispatcherState::default()),
            stats: ExternalPoolReleaseDispatcherStats::default(),
        })
    }

    fn try_reserve(&self) -> Option<OwnedSemaphorePermit> {
        self.capacity.clone().try_acquire_owned().ok()
    }

    fn enqueue(self: &Arc<Self>, intent: ExternalPoolReleaseIntent, permit: OwnedSemaphorePermit) {
        let should_start = {
            let mut state = self.state.lock();
            if state.pending.contains_key(&intent) {
                self.stats.deduplicated.fetch_add(1, Ordering::Relaxed);
            } else {
                state.order.push_back(intent.clone());
                state.pending.insert(
                    intent,
                    ExternalPoolPendingRelease {
                        _permit: permit,
                        failures: 0,
                        next_attempt_at: Instant::now(),
                    },
                );
                self.stats.enqueued.fetch_add(1, Ordering::Relaxed);
            }
            if state.worker_running {
                false
            } else {
                state.worker_running = true;
                true
            }
        };
        if !should_start {
            self.work_notify.notify_one();
            return;
        }

        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            self.state.lock().worker_running = false;
            let failures = self.stats.spawn_failures.fetch_add(1, Ordering::Relaxed) + 1;
            tracing::warn!(
                failures,
                pending = self.pending_len(),
                "外部池 Redis release worker 无可用 Tokio runtime，lease 将保留到 TTL 回收"
            );
            return;
        };
        self.stats.worker_starts.fetch_add(1, Ordering::Relaxed);
        let dispatcher = self.clone();
        handle.spawn(async move {
            dispatcher.run().await;
        });
    }

    async fn run(self: Arc<Self>) {
        loop {
            let work_available = self.work_notify.notified();
            tokio::pin!(work_available);
            work_available.as_mut().enable();
            let (batch, next_attempt_at) = {
                let mut state = self.state.lock();
                if state.pending.is_empty() {
                    state.order.clear();
                    state.worker_running = false;
                    self.drained_notify.notify_waiters();
                    return;
                }
                state.next_ready_batch(Instant::now(), EXTERNAL_POOL_RELEASE_BATCH_SIZE)
            };
            if batch.is_empty() {
                match next_attempt_at {
                    Some(next_attempt_at) => {
                        tokio::select! {
                            _ = tokio::time::sleep_until(next_attempt_at) => {}
                            _ = work_available.as_mut() => {}
                        }
                    }
                    None => tokio::task::yield_now().await,
                }
                continue;
            }
            let requests = batch
                .iter()
                .map(|intent| ExternalPoolLeaseReleaseRequest {
                    pool_id: intent.pool_id,
                    lease_id: intent.lease_id.clone(),
                    pending: intent.release_kind == ExternalPoolLeaseReleaseKind::Pending,
                })
                .collect::<Vec<_>>();
            let result = timeout(
                EXTERNAL_POOL_QUEUE_REDIS_OPERATION_TIMEOUT,
                self.redis.release_external_pool_leases_batch(&requests),
            )
            .await;

            let mut completed = 0usize;
            let mut released_capacity = 0usize;
            let batch_response_valid = match result {
                Ok(Ok(results)) if results.len() == batch.len() => {
                    let mut state = self.state.lock();
                    for (intent, result) in batch.iter().zip(results) {
                        if result.completed && state.pending.remove(intent).is_some() {
                            completed += 1;
                            if result.removed {
                                released_capacity += 1;
                            }
                        }
                    }
                    state.system_failures = 0;
                    state.system_retry_at = None;
                    true
                }
                Ok(Ok(_)) | Ok(Err(_)) | Err(_) => false,
            };
            let mut failed = 0usize;
            {
                let mut state = self.state.lock();
                if batch_response_valid {
                    for intent in &batch {
                        if let Some(pending) = state.pending.get_mut(intent) {
                            failed += 1;
                            pending.failures = pending.failures.saturating_add(1);
                            pending.next_attempt_at = Instant::now()
                                + external_release_retry_delay(pending.failures, intent);
                        }
                    }
                } else {
                    failed = batch.len();
                    state.system_failures = state.system_failures.saturating_add(1);
                    state.system_retry_at = Some(
                        Instant::now() + external_release_system_retry_delay(state.system_failures),
                    );
                }
            }
            if completed > 0 {
                self.stats
                    .completed
                    .fetch_add(completed as u64, Ordering::Relaxed);
                self.selection_runtime_snapshot.lock().take();
            }
            self.capacity_signal.capacity_released(released_capacity);
            if failed == 0 {
                tokio::task::yield_now().await;
                continue;
            }

            let retries = self.stats.retries.fetch_add(1, Ordering::Relaxed) + 1;
            if retries == 1 || retries.is_power_of_two() {
                tracing::warn!(
                    retries,
                    pending = self.pending_len(),
                    batch_size = batch.len(),
                    completed,
                    failed,
                    "外部池 Redis release 批处理未完整完成，保留 intent 并重试"
                );
            }
        }
    }

    fn pending_len(&self) -> usize {
        self.state.lock().pending.len()
    }

    fn is_drained(&self) -> bool {
        let state = self.state.lock();
        state.pending.is_empty() && !state.worker_running
    }

    async fn drain(&self, drain_timeout: Duration) -> ExternalPoolReleaseDrainReport {
        let deadline = Instant::now() + drain_timeout;
        let drained = loop {
            if self.is_drained() {
                break true;
            }
            let notified = self.drained_notify.notified();
            if self.is_drained() {
                break true;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                break self.is_drained();
            }
        };
        ExternalPoolReleaseDrainReport {
            drained,
            pending: self.pending_len(),
            enqueued: self.stats.enqueued.load(Ordering::Relaxed),
            completed: self.stats.completed.load(Ordering::Relaxed),
            retries: self.stats.retries.load(Ordering::Relaxed),
            worker_starts: self.stats.worker_starts.load(Ordering::Relaxed),
            spawn_failures: self.stats.spawn_failures.load(Ordering::Relaxed),
        }
    }
}

impl ExternalPoolLease {
    fn pending(
        manager: ExternalPoolManager,
        pool_id: u64,
        lease_id: String,
        coordination_epoch: String,
        max_age: Duration,
        release_permit: OwnedSemaphorePermit,
    ) -> Self {
        Self {
            manager,
            pool_id,
            lease_id,
            coordination_epoch,
            state: ExternalPoolLeaseState::Pending,
            max_age,
            heartbeat: None,
            release_permit: Some(release_permit),
        }
    }

    fn degraded_fallback_local(
        manager: ExternalPoolManager,
        pool_id: u64,
        lease_id: String,
        release_permit: OwnedSemaphorePermit,
    ) -> Self {
        Self {
            manager,
            pool_id,
            lease_id,
            coordination_epoch: "degraded-fallback-local".to_string(),
            state: ExternalPoolLeaseState::DegradedFallbackLocal,
            max_age: Duration::from_secs(EXTERNAL_POOL_LEASE_MAX_AGE_SECS),
            heartbeat: None,
            release_permit: Some(release_permit),
        }
    }

    fn disarm(mut self) {
        self.state = ExternalPoolLeaseState::Disarmed;
    }

    fn confirm(&mut self) {
        debug_assert_eq!(self.state, ExternalPoolLeaseState::Pending);
        self.state = ExternalPoolLeaseState::Confirmed;
        self.manager
            .invalidate_external_pool_runtime_capacity_state();
        self.heartbeat = Some(ExternalPoolLeaseHeartbeat::spawn(
            self.manager.clone(),
            self.pool_id,
            self.lease_id.clone(),
            self.coordination_epoch.clone(),
            self.max_age,
        ));
    }

    async fn wait_until_lost(&self) {
        match self.heartbeat.as_ref() {
            Some(heartbeat) => heartbeat.wait_until_lost().await,
            None => std::future::pending().await,
        }
    }
}

impl Drop for ExternalPoolLease {
    fn drop(&mut self) {
        if let Some(heartbeat) = self.heartbeat.take() {
            heartbeat.stop();
        }
        let release_kind = match self.state {
            ExternalPoolLeaseState::Pending => ExternalPoolLeaseReleaseKind::Pending,
            ExternalPoolLeaseState::Confirmed => ExternalPoolLeaseReleaseKind::Confirmed,
            ExternalPoolLeaseState::DegradedFallbackLocal => {
                let _ = self.release_permit.take();
                return;
            }
            ExternalPoolLeaseState::Disarmed => return,
        };
        self.manager
            .invalidate_external_pool_runtime_capacity_state();
        let release_permit = self
            .release_permit
            .take()
            .expect("armed external pool lease must retain release capacity");
        release_external_pool_lease_reliably(
            self.manager.clone(),
            self.pool_id,
            self.lease_id.clone(),
            release_kind,
            release_permit,
        );
    }
}

async fn run_external_pool_lease_heartbeat(
    manager: ExternalPoolManager,
    pool_id: u64,
    lease_id: String,
    coordination_epoch: String,
    max_age: Duration,
    state: Arc<ExternalPoolLeaseHeartbeatState>,
) {
    let max_age_ms = max_age.as_millis().max(300) as u64;
    let interval = Duration::from_millis(
        (max_age_ms / 3)
            .clamp(100, EXTERNAL_POOL_LEASE_TOUCH_INTERVAL_SECS * 1_000)
            .min(max_age_ms / 2),
    );
    let operation_timeout = Duration::from_millis((max_age_ms / 4).clamp(
        50,
        EXTERNAL_POOL_COORDINATOR_REDIS_OPERATION_TIMEOUT.as_millis() as u64,
    ));
    let loss_after = max_age.saturating_sub(operation_timeout).max(interval);
    let retry_delay = Duration::from_millis(EXTERNAL_POOL_LEASE_HEARTBEAT_RETRY_MILLIS)
        .min(interval)
        .max(Duration::from_millis(50));
    let retry_cap = interval.min(Duration::from_secs(30)).max(retry_delay);
    let lease_ttl_secs = max_age.as_secs().saturating_mul(2).max(1) as usize;
    let mut last_success = Instant::now();
    let mut delay = interval;
    let mut consecutive_failures = 0u32;

    loop {
        let loss_deadline = last_success + loss_after;
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = tokio::time::sleep_until(loss_deadline) => {
                state.mark_lost();
                tracing::warn!(
                    pool_id,
                    heartbeat_attempts = state.attempts.load(Ordering::Relaxed),
                    heartbeat_failures = state.failures.load(Ordering::Relaxed),
                    "外部池 Redis lease heartbeat 到达 prune 安全截止时间，中止当前上游响应"
                );
                return;
            }
        }
        state.attempts.fetch_add(1, Ordering::Relaxed);
        let touch = tokio::select! {
            touch = timeout(
                operation_timeout,
                manager.touch_pool(
                    pool_id,
                    &lease_id,
                    lease_ttl_secs,
                    &coordination_epoch,
                ),
            ) => touch,
            _ = tokio::time::sleep_until(loss_deadline) => {
                state.mark_lost();
                tracing::warn!(
                    pool_id,
                    heartbeat_attempts = state.attempts.load(Ordering::Relaxed),
                    heartbeat_failures = state.failures.load(Ordering::Relaxed),
                    "外部池 Redis lease heartbeat 操作未在 prune 安全截止时间前完成，中止当前上游响应"
                );
                return;
            }
        };
        match touch {
            Ok(Ok(true)) => {
                last_success = Instant::now();
                delay = interval;
                consecutive_failures = 0;
            }
            Ok(Ok(false)) => {
                state.failures.fetch_add(1, Ordering::Relaxed);
                state.mark_lost();
                tracing::warn!(
                    pool_id,
                    "外部池 Redis lease heartbeat 发现 lease 已丢失，中止当前上游响应"
                );
                return;
            }
            Ok(Err(_)) | Err(_) => {
                state.failures.fetch_add(1, Ordering::Relaxed);
                if Instant::now() >= loss_deadline {
                    state.mark_lost();
                    tracing::warn!(
                        pool_id,
                        heartbeat_attempts = state.attempts.load(Ordering::Relaxed),
                        heartbeat_failures = state.failures.load(Ordering::Relaxed),
                        "外部池 Redis lease heartbeat 在 prune 安全期限前未恢复，中止当前上游响应"
                    );
                    return;
                }
                consecutive_failures = consecutive_failures.saturating_add(1);
                let multiplier = 1u32
                    .checked_shl(consecutive_failures.saturating_sub(1).min(16))
                    .unwrap_or(u32::MAX);
                delay = retry_delay.saturating_mul(multiplier).min(retry_cap);
            }
        }
    }
}

fn release_external_pool_lease_reliably(
    manager: ExternalPoolManager,
    pool_id: u64,
    lease_id: String,
    release_kind: ExternalPoolLeaseReleaseKind,
    release_permit: OwnedSemaphorePermit,
) {
    manager.release_dispatcher.enqueue(
        ExternalPoolReleaseIntent {
            pool_id,
            lease_id,
            release_kind,
        },
        release_permit,
    );
}

async fn release_external_pool_queue_lease_with_retry(
    manager: ExternalPoolManager,
    lease_id: String,
    attempts: usize,
) -> anyhow::Result<bool> {
    let attempts = attempts.max(1);
    let mut last_error = None;
    for attempt in 0..attempts {
        match timeout(
            EXTERNAL_POOL_QUEUE_REDIS_OPERATION_TIMEOUT,
            manager.redis.leave_external_pool_dispatch_queue(&lease_id),
        )
        .await
        {
            Ok(Ok(removed)) => return Ok(removed),
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
        release_external_pool_queue_lease_with_retry(manager, lease_id, 2)
            .await
            .map(|_| ())
    });
    if admitted {
        return;
    }
    spawn_external_release_fallback(
        "关键队列拒绝后异步释放外部池 Redis 调度排队 lease",
        async move {
            release_external_pool_queue_lease_with_retry(fallback_manager, fallback_lease_id, 2)
                .await
                .map(|_| ())
        },
    );
}

static EXTERNAL_POOL_RELEASE_FALLBACK_SEMAPHORE: OnceLock<Arc<Semaphore>> = OnceLock::new();
static EXTERNAL_POOL_RELEASE_FALLBACK_ACCEPTED: AtomicU64 = AtomicU64::new(0);
static EXTERNAL_POOL_RELEASE_FALLBACK_REJECTED: AtomicU64 = AtomicU64::new(0);
static EXTERNAL_POOL_RELEASE_FALLBACK_FINISHED: AtomicU64 = AtomicU64::new(0);

fn external_pool_release_fallback_semaphore() -> Arc<Semaphore> {
    EXTERNAL_POOL_RELEASE_FALLBACK_SEMAPHORE
        .get_or_init(|| Arc::new(Semaphore::new(EXTERNAL_POOL_RELEASE_FALLBACK_MAX_IN_FLIGHT)))
        .clone()
}

fn record_external_release_fallback_rejection(description: &'static str, reason: &'static str) {
    let rejected = EXTERNAL_POOL_RELEASE_FALLBACK_REJECTED.fetch_add(1, Ordering::Relaxed) + 1;
    if rejected == 1 || rejected.is_power_of_two() {
        tracing::warn!(
            description,
            reason,
            rejected,
            max_in_flight = EXTERNAL_POOL_RELEASE_FALLBACK_MAX_IN_FLIGHT,
            "外部池 Redis release fallback 已饱和，剩余 lease 将由 TTL 回收"
        );
    }
}

fn spawn_external_release_fallback(
    description: &'static str,
    future: impl Future<Output = anyhow::Result<()>> + Send + 'static,
) -> bool {
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        record_external_release_fallback_rejection(description, "runtime_unavailable");
        return false;
    };
    let Ok(permit) = external_pool_release_fallback_semaphore().try_acquire_owned() else {
        record_external_release_fallback_rejection(description, "fallback_saturated");
        return false;
    };
    EXTERNAL_POOL_RELEASE_FALLBACK_ACCEPTED.fetch_add(1, Ordering::Relaxed);
    handle.spawn(async move {
        let _permit = permit;
        if let Err(err) = future.await {
            tracing::error!(description, error = %err, "外部池 Redis release 有界重试失败，将由 TTL 回收");
        }
        EXTERNAL_POOL_RELEASE_FALLBACK_FINISHED.fetch_add(1, Ordering::Release);
    });
    true
}

struct ExternalPoolQueueGuard {
    manager: ExternalPoolManager,
    lease_id: String,
    lease_ttl_secs: u64,
    next_renew_at: Option<Instant>,
    released: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExternalPoolQueueLeasePolicy {
    ttl_secs: u64,
    renewal_required: bool,
}

fn external_pool_queue_lease_policy(max_wait: Option<Duration>) -> ExternalPoolQueueLeasePolicy {
    let Some(max_wait) = max_wait else {
        return ExternalPoolQueueLeasePolicy {
            ttl_secs: EXTERNAL_POOL_QUEUE_LEASE_SAFETY_MARGIN_SECS,
            renewal_required: true,
        };
    };
    let rounded_wait_secs = max_wait
        .as_secs()
        .saturating_add(u64::from(max_wait.subsec_nanos() > 0));
    ExternalPoolQueueLeasePolicy {
        ttl_secs: rounded_wait_secs
            .saturating_add(EXTERNAL_POOL_QUEUE_LEASE_SAFETY_MARGIN_SECS)
            .max(EXTERNAL_POOL_QUEUE_LEASE_SAFETY_MARGIN_SECS),
        renewal_required: false,
    }
}

impl ExternalPoolQueueGuard {
    fn new(
        manager: ExternalPoolManager,
        lease_id: String,
        policy: ExternalPoolQueueLeasePolicy,
    ) -> Self {
        Self {
            manager,
            lease_id,
            lease_ttl_secs: policy.ttl_secs,
            next_renew_at: policy.renewal_required.then(|| {
                let renew_interval_secs = (policy.ttl_secs / 3)
                    .clamp(1, EXTERNAL_POOL_QUEUE_LEASE_RENEW_INTERVAL_MAX_SECS);
                Instant::now() + Duration::from_secs(renew_interval_secs)
            }),
            released: false,
        }
    }

    fn disarm(mut self) {
        self.released = true;
    }

    async fn renew_if_needed(&mut self) -> anyhow::Result<()> {
        if self
            .next_renew_at
            .is_none_or(|renew_at| Instant::now() < renew_at)
        {
            return Ok(());
        }
        let renewed = timeout(
            EXTERNAL_POOL_QUEUE_REDIS_OPERATION_TIMEOUT,
            self.manager
                .redis
                .renew_external_pool_dispatch_queue(&self.lease_id, self.lease_ttl_secs),
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
        let renew_interval_secs =
            (self.lease_ttl_secs / 3).clamp(1, EXTERNAL_POOL_QUEUE_LEASE_RENEW_INTERVAL_MAX_SECS);
        self.next_renew_at = Some(Instant::now() + Duration::from_secs(renew_interval_secs));
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
    protocol_error: Option<&'static str>,
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
    reason: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    model: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PoolCapacityWaitReason {
    Full,
    Cooldown,
    ModelUnavailable,
    CoordinatorUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PoolCoordinatorUnavailableKind {
    AdmissionSaturated,
    PostgresCircuitOpen,
    PostgresError,
    PostgresTimeout,
    RedisError,
}

impl PoolCoordinatorUnavailableKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::AdmissionSaturated => "admission_saturated",
            Self::PostgresCircuitOpen => "postgres_circuit_open",
            Self::PostgresError => "postgres_error",
            Self::PostgresTimeout => "postgres_timeout",
            Self::RedisError => "redis_error",
        }
    }
}

impl PoolCapacityWaitReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Cooldown => "cooldown",
            Self::ModelUnavailable => "model_unavailable",
            Self::CoordinatorUnavailable => "coordinator_unavailable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PoolCooldownScope {
    Pool,
    Model,
}

impl PoolCooldownScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pool => "pool",
            Self::Model => "model",
        }
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PoolDispatchFenceResult {
    Current,
    Changed,
    CoordinatorUnavailable(ExternalPoolSelectionUnavailable),
}

#[derive(Debug, Clone, Default)]
struct PoolAvailabilitySnapshot {
    eligible_pools: usize,
    available_pools: usize,
    temporary_unavailable_pools: usize,
    coordinator_unavailable: bool,
    coordinator_unavailable_kind: Option<PoolCoordinatorUnavailableKind>,
    invalid_runtime_pools: usize,
    wait_reason: Option<PoolCapacityWaitReason>,
    wait_for: Option<Duration>,
    cooldown_reason: Option<String>,
    cooldown_scope: Option<PoolCooldownScope>,
    cooldown_remaining_secs: Option<u64>,
}

#[derive(Debug, Clone, Default)]
struct PoolSelectionSnapshot {
    selected_pool: Option<ExternalPool>,
    availability: PoolAvailabilitySnapshot,
    degraded_fallback_local_lease: bool,
}

#[derive(Debug, Clone, Default)]
struct PoolRuntimeSnapshot {
    in_flight: u32,
    global_in_flight: u32,
    pool_cooldown_remaining_secs: u64,
    pool_cooldown_reason: Option<String>,
    model_cooldown: Option<(u64, Option<String>)>,
    transient_failure_streak: u32,
    transient_failure_ttl: Option<Duration>,
}

#[cfg(test)]
#[derive(Debug, Clone)]
struct CachedPoolAvailabilitySnapshot {
    cache_key: String,
    snapshot: PoolAvailabilitySnapshot,
    expires_at: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SelectionRuntimeSnapshotKey {
    pool_ids: Vec<u64>,
    models: Vec<String>,
}

#[derive(Debug, Clone)]
struct CachedSelectionRuntimeSnapshot {
    key: SelectionRuntimeSnapshotKey,
    snapshots: Vec<Result<PoolRuntimeSnapshot, String>>,
    expires_at: Instant,
}

fn materialize_selection_runtime_snapshots(
    snapshots: Vec<Result<PoolRuntimeSnapshot, String>>,
) -> Vec<anyhow::Result<PoolRuntimeSnapshot>> {
    snapshots
        .into_iter()
        .map(|snapshot| snapshot.map_err(anyhow::Error::msg))
        .collect()
}

#[derive(Debug, Clone)]
struct CachedStaticPoolSnapshot {
    generation: u64,
    pools: Arc<Vec<ExternalPoolEligibility>>,
    fresh_until: Instant,
    stale_until: Instant,
    load_succeeded: bool,
}

#[derive(Debug, Clone)]
struct CachedAuthoritativePoolSnapshot {
    generation: u64,
    result: Result<Arc<Vec<ExternalPool>>, ExternalPoolSelectionUnavailable>,
    fresh_until: Instant,
    stale_until: Instant,
}

struct PoolDispatchFenceFlight {
    result: SyncMutex<Option<PoolDispatchFenceResult>>,
    ready: Notify,
}

#[cfg(test)]
struct TestDispatchAfterPrepareGate {
    prepared: tokio::sync::Barrier,
    resume: tokio::sync::Barrier,
}

#[cfg(test)]
impl TestDispatchAfterPrepareGate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            prepared: tokio::sync::Barrier::new(2),
            resume: tokio::sync::Barrier::new(2),
        })
    }
}

impl PoolDispatchFenceFlight {
    fn new() -> Self {
        Self {
            result: SyncMutex::new(None),
            ready: Notify::new(),
        }
    }

    fn complete(&self, result: PoolDispatchFenceResult) {
        *self.result.lock() = Some(result);
        self.ready.notify_waiters();
    }

    async fn wait(&self) -> PoolDispatchFenceResult {
        loop {
            if let Some(result) = *self.result.lock() {
                return result;
            }
            let notified = self.ready.notified();
            if let Some(result) = *self.result.lock() {
                return result;
            }
            notified.await;
        }
    }
}

#[derive(Debug, Clone)]
enum CachedStaticPoolSnapshotState {
    Fresh(Arc<Vec<ExternalPoolEligibility>>),
    Stale(Arc<Vec<ExternalPoolEligibility>>),
}

#[derive(Debug, Clone)]
enum CachedAuthoritativePoolSnapshotState {
    Fresh(Result<Arc<Vec<ExternalPool>>, ExternalPoolSelectionUnavailable>),
    Stale(Arc<Vec<ExternalPool>>),
}

#[cfg(test)]
struct StaticPoolBackgroundRefreshActivity {
    in_flight: Arc<AtomicU64>,
}

#[cfg(test)]
impl Drop for StaticPoolBackgroundRefreshActivity {
    fn drop(&mut self) {
        self.in_flight.fetch_sub(1, Ordering::AcqRel);
    }
}

impl PoolAvailabilitySnapshot {
    fn has_temporary_unavailable_pool(&self) -> bool {
        self.temporary_unavailable_pools > 0
    }

    fn default_retry_attempts(&self, payload_guard_retry_enabled: bool) -> usize {
        self.eligible_pools
            .max(1)
            .saturating_add(usize::from(payload_guard_retry_enabled))
    }

    fn mark_cooldown(
        &mut self,
        remaining_secs: u64,
        reason: Option<String>,
        scope: PoolCooldownScope,
    ) {
        let remaining_secs = remaining_secs.max(1);
        let wait_for = Duration::from_secs(remaining_secs);
        let should_replace = self
            .wait_for
            .map(|existing| wait_for < existing)
            .unwrap_or(true);
        if should_replace {
            self.wait_for = Some(wait_for);
            self.cooldown_remaining_secs = Some(remaining_secs);
            self.cooldown_reason = reason;
            self.cooldown_scope = Some(scope);
        }
        let wait_reason = if self.cooldown_reason.as_deref() == Some("model_unavailable") {
            PoolCapacityWaitReason::ModelUnavailable
        } else {
            PoolCapacityWaitReason::Cooldown
        };
        self.wait_reason.get_or_insert(wait_reason);
        if self.wait_reason == Some(PoolCapacityWaitReason::Cooldown)
            && wait_reason == PoolCapacityWaitReason::ModelUnavailable
        {
            self.wait_reason = Some(wait_reason);
        }
    }

    fn capacity_context(&self) -> PoolCapacityWaitContext {
        PoolCapacityWaitContext {
            reason: self.wait_reason.unwrap_or(PoolCapacityWaitReason::Full),
            wait_for: self.wait_for,
            cooldown_reason: self.cooldown_reason.clone(),
            cooldown_scope: self.cooldown_scope,
            cooldown_remaining_secs: self.cooldown_remaining_secs,
            eligible_pools: self.eligible_pools,
            available_pools: self.available_pools,
            temporary_unavailable_pools: self.temporary_unavailable_pools,
            coordinator_unavailable_kind: self.coordinator_unavailable_kind,
        }
    }
}

fn selection_unavailable_snapshot(
    unavailable: ExternalPoolSelectionUnavailable,
) -> PoolSelectionSnapshot {
    PoolSelectionSnapshot {
        selected_pool: None,
        availability: PoolAvailabilitySnapshot {
            temporary_unavailable_pools: 1,
            coordinator_unavailable: true,
            coordinator_unavailable_kind: Some(unavailable.kind.coordinator_kind()),
            wait_reason: Some(PoolCapacityWaitReason::CoordinatorUnavailable),
            wait_for: Some(unavailable.retry_after),
            ..PoolAvailabilitySnapshot::default()
        },
        degraded_fallback_local_lease: false,
    }
}

#[derive(Debug, Clone)]
struct PoolCapacityWaitContext {
    reason: PoolCapacityWaitReason,
    wait_for: Option<Duration>,
    cooldown_reason: Option<String>,
    cooldown_scope: Option<PoolCooldownScope>,
    cooldown_remaining_secs: Option<u64>,
    eligible_pools: usize,
    available_pools: usize,
    temporary_unavailable_pools: usize,
    coordinator_unavailable_kind: Option<PoolCoordinatorUnavailableKind>,
}

impl PoolCapacityWaitContext {
    fn from_unavailable(unavailable: &PoolAcquireUnavailable) -> Self {
        Self {
            reason: unavailable.reason,
            wait_for: unavailable.wait_for,
            cooldown_reason: None,
            cooldown_scope: None,
            cooldown_remaining_secs: unavailable.wait_for.map(|duration| duration.as_secs()),
            eligible_pools: 0,
            available_pools: 0,
            temporary_unavailable_pools: 0,
            coordinator_unavailable_kind: None,
        }
    }

    fn is_model_unavailable(&self) -> bool {
        self.reason == PoolCapacityWaitReason::ModelUnavailable
            || self.cooldown_reason.as_deref() == Some("model_unavailable")
    }
}

enum ExternalCapacityDecision {
    Retry,
    FinalError(ExternalPoolFinalError),
}

fn default_true() -> bool {
    true
}

fn default_external_pool_revision() -> u64 {
    1
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
    if !config.external_pool_route_allowed(endpoint) {
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
        let capacity_signal = Arc::new(CapacitySignal::default());
        let selection_runtime_snapshot = Arc::new(SyncMutex::new(None));
        let release_dispatcher = ExternalPoolReleaseDispatcher::new(
            redis.clone(),
            capacity_signal.clone(),
            selection_runtime_snapshot.clone(),
        );
        Self {
            postgres,
            redis,
            instance_id: uuid::Uuid::new_v4().to_string(),
            client: reqwest::Client::builder()
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            capacity_signal,
            #[cfg(test)]
            availability_cache: Arc::new(SyncMutex::new(None)),
            static_pool_snapshot: Arc::new(SyncMutex::new(None)),
            static_pool_snapshot_generation: Arc::new(AtomicU64::new(0)),
            static_pool_snapshot_refresh_lock: Arc::new(AsyncMutex::new(())),
            authoritative_pool_snapshot: Arc::new(SyncMutex::new(None)),
            authoritative_pool_snapshot_refresh_lock: Arc::new(AsyncMutex::new(())),
            authoritative_pool_snapshot_changed: Arc::new(Notify::new()),
            selection_runtime_snapshot,
            selection_runtime_snapshot_refresh_lock: Arc::new(AsyncMutex::new(())),
            static_pool_snapshot_fresh_ttl: EXTERNAL_POOL_STATIC_SNAPSHOT_FRESH_TTL,
            static_pool_snapshot_stale_ttl: EXTERNAL_POOL_STATIC_SNAPSHOT_STALE_TTL,
            static_pool_snapshot_failure_retry: EXTERNAL_POOL_STATIC_SNAPSHOT_FAILURE_RETRY,
            static_pool_snapshot_refresh_timeout: EXTERNAL_POOL_STATIC_SNAPSHOT_REFRESH_TIMEOUT,
            #[cfg(test)]
            static_pool_snapshot_pg_loads: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            static_pool_snapshot_background_refreshes: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            static_pool_snapshot_background_in_flight: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            authoritative_pool_snapshot_pg_loads: Arc::new(AtomicU64::new(0)),
            coordinator_breaker: Arc::new(ExternalPoolCoordinatorBreaker::default()),
            selection_breaker: Arc::new(ExternalPoolSelectionBreaker::default()),
            selection_saturated: Arc::new(AtomicU64::new(0)),
            dispatch_fence_flights: Arc::new(SyncMutex::new(HashMap::new())),
            #[cfg(test)]
            dispatch_fence_pg_loads: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            dispatch_after_prepare_gate: Arc::new(SyncMutex::new(None)),
            observed_pool_data_generation: Arc::new(AtomicU64::new(0)),
            success_reset_recent: Arc::new(SyncMutex::new(HashMap::new())),
            success_reset_semaphore: Arc::new(Semaphore::new(
                EXTERNAL_POOL_SUCCESS_RESET_MAX_TASKS,
            )),
            #[cfg(test)]
            success_reset_tasks_started: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            success_reset_tasks_in_flight: Arc::new(AtomicU64::new(0)),
            coordinator_reconcile_lock: Arc::new(AsyncMutex::new(())),
            coordinator_epoch: Arc::new(SyncMutex::new(None)),
            coordinator_run_id: Arc::new(SyncMutex::new(None)),
            coordinator_probe_required: Arc::new(AtomicBool::new(true)),
            coordinator_next_probe_ms: Arc::new(AtomicU64::new(0)),
            coordinator_recovery_grace: Duration::from_secs(
                EXTERNAL_POOL_COORDINATOR_RECOVERY_GRACE_SECS,
            ),
            release_dispatcher,
            degraded_fallback_local_leases: Arc::new(SyncMutex::new(HashMap::new())),
            local_mutation_dispatch_trust: Arc::new(SyncMutex::new(HashMap::new())),
        }
    }

    pub async fn drain_release_intents(
        &self,
        drain_timeout: Duration,
    ) -> ExternalPoolReleaseDrainReport {
        self.release_dispatcher.drain(drain_timeout).await
    }

    #[cfg(test)]
    fn with_coordinator_recovery_grace(mut self, recovery_grace: Duration) -> Self {
        self.coordinator_recovery_grace = recovery_grace.max(Duration::from_millis(1));
        self
    }

    #[cfg(test)]
    fn with_static_pool_snapshot_ttl(mut self, ttl: Duration) -> Self {
        self.static_pool_snapshot_fresh_ttl = ttl.max(Duration::from_millis(1));
        self.static_pool_snapshot_stale_ttl = self
            .static_pool_snapshot_stale_ttl
            .max(self.static_pool_snapshot_fresh_ttl);
        self
    }

    #[cfg(test)]
    fn with_static_pool_snapshot_timing(
        mut self,
        fresh_ttl: Duration,
        stale_ttl: Duration,
        failure_retry: Duration,
        refresh_timeout: Duration,
    ) -> Self {
        self.static_pool_snapshot_fresh_ttl = fresh_ttl.max(Duration::from_millis(1));
        self.static_pool_snapshot_stale_ttl = stale_ttl
            .max(self.static_pool_snapshot_fresh_ttl)
            .max(Duration::from_millis(1));
        self.static_pool_snapshot_failure_retry = failure_retry.max(Duration::from_millis(1));
        self.static_pool_snapshot_refresh_timeout = refresh_timeout.max(Duration::from_millis(1));
        self
    }

    pub fn invalidate_static_pool_snapshot(&self) {
        self.static_pool_snapshot_generation
            .fetch_add(1, Ordering::AcqRel);
        self.static_pool_snapshot.lock().take();
        self.authoritative_pool_snapshot.lock().take();
        self.local_mutation_dispatch_trust.lock().clear();
        self.invalidate_external_pool_policy_state();
    }

    fn static_snapshot_ttls(&self, generation: u64) -> (Duration, Duration) {
        let fresh_ttl = deterministic_duration_jitter(
            self.static_pool_snapshot_fresh_ttl,
            EXTERNAL_POOL_STATIC_SNAPSHOT_JITTER_PERCENT,
            generation ^ 0x4652_4553_48,
        );
        let stale_ttl = deterministic_duration_jitter(
            self.static_pool_snapshot_stale_ttl,
            EXTERNAL_POOL_STATIC_SNAPSHOT_JITTER_PERCENT,
            generation ^ 0x5354_414c_45,
        )
        .max(fresh_ttl);
        (fresh_ttl, stale_ttl)
    }

    fn publish_static_pool_snapshot_arc(
        &self,
        generation: u64,
        pools: Arc<Vec<ExternalPoolEligibility>>,
        load_succeeded: bool,
        fresh_ttl: Duration,
        stale_ttl: Duration,
    ) -> Option<Arc<Vec<ExternalPoolEligibility>>> {
        let now = Instant::now();
        let mut snapshot = self.static_pool_snapshot.lock();
        if self.static_pool_snapshot_generation.load(Ordering::Acquire) != generation {
            return None;
        }
        *snapshot = Some(CachedStaticPoolSnapshot {
            generation,
            pools: pools.clone(),
            fresh_until: now + fresh_ttl,
            stale_until: now + stale_ttl.max(fresh_ttl),
            load_succeeded,
        });
        if self.static_pool_snapshot_generation.load(Ordering::Acquire) != generation {
            snapshot.take();
            return None;
        }
        Some(pools)
    }

    fn publish_authoritative_pool_snapshot_success_arc(
        &self,
        generation: u64,
        pools: Arc<Vec<ExternalPool>>,
        fresh_ttl: Duration,
        stale_ttl: Duration,
    ) -> bool {
        if self.static_pool_snapshot_generation.load(Ordering::Acquire) != generation {
            return false;
        }
        let now = Instant::now();
        let mut snapshot = self.authoritative_pool_snapshot.lock();
        if self.static_pool_snapshot_generation.load(Ordering::Acquire) != generation {
            return false;
        }
        *snapshot = Some(CachedAuthoritativePoolSnapshot {
            generation,
            result: Ok(pools),
            fresh_until: now + fresh_ttl,
            stale_until: now + stale_ttl.max(fresh_ttl),
        });
        true
    }

    fn sort_authoritative_pools(pools: &mut [ExternalPool]) {
        pools.sort_by_key(|pool| (pool.priority, pool.id));
    }

    fn sort_static_pool_eligibility(pools: &mut [ExternalPoolEligibility]) {
        pools.sort_by_key(|pool| pool.id);
    }

    fn apply_local_external_pool_snapshot_mutation(
        &self,
        upsert_pool: Option<&ExternalPool>,
        delete_pool_id: Option<u64>,
    ) {
        let generation = self
            .static_pool_snapshot_generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        let now = Instant::now();
        let delete_pool_id = delete_pool_id.or_else(|| upsert_pool.map(|pool| pool.id));
        let upsert_pool_id = upsert_pool.map(|pool| pool.id);

        let previous_static_pools = self
            .static_pool_snapshot
            .lock()
            .as_ref()
            .filter(|snapshot| snapshot.load_succeeded && snapshot.stale_until > now)
            .map(|snapshot| snapshot.pools.as_ref().clone());
        let had_previous_static_pools = previous_static_pools.is_some();
        let mut static_pools = previous_static_pools.unwrap_or_default();
        static_pools.retain(|pool| Some(pool.id) != delete_pool_id);
        if let Some(pool) = upsert_pool {
            static_pools.push(external_pool_eligibility_from_pool(pool));
        }
        Self::sort_static_pool_eligibility(&mut static_pools);
        if upsert_pool.is_some() || had_previous_static_pools {
            let (static_fresh_ttl, static_stale_ttl) = self.static_snapshot_ttls(generation);
            let _ = self.publish_static_pool_snapshot_arc(
                generation,
                Arc::new(static_pools),
                true,
                static_fresh_ttl,
                static_stale_ttl,
            );
        } else {
            self.static_pool_snapshot.lock().take();
        }

        let previous_authoritative_pools = self
            .authoritative_pool_snapshot
            .lock()
            .as_ref()
            .filter(|snapshot| {
                snapshot.stale_until > now
                    && snapshot
                        .result
                        .as_ref()
                        .is_ok_and(|pools| !pools.is_empty())
            })
            .and_then(|snapshot| {
                snapshot
                    .result
                    .as_ref()
                    .ok()
                    .map(|pools| pools.as_ref().clone())
            });
        let had_previous_authoritative_pools = previous_authoritative_pools.is_some();
        let mut authoritative_pools = previous_authoritative_pools.unwrap_or_default();
        authoritative_pools.retain(|pool| Some(pool.id) != delete_pool_id);
        if let Some(pool) = upsert_pool {
            authoritative_pools.push(pool.clone());
        }
        Self::sort_authoritative_pools(&mut authoritative_pools);
        if upsert_pool.is_some() || had_previous_authoritative_pools {
            let _ = self.publish_authoritative_pool_snapshot_success_arc(
                generation,
                Arc::new(authoritative_pools),
                EXTERNAL_POOL_LOCAL_MUTATION_AUTHORITATIVE_FRESH_TTL,
                EXTERNAL_POOL_AUTHORITATIVE_SNAPSHOT_STALE_TTL,
            );
        } else {
            self.authoritative_pool_snapshot.lock().take();
        }

        let mut trust = self.local_mutation_dispatch_trust.lock();
        if let Some(pool_id) = delete_pool_id {
            trust.retain(|(existing_pool_id, _), _| *existing_pool_id != pool_id);
        }
        if let Some(pool) = upsert_pool.filter(|pool| {
            pool.api_key
                .as_deref()
                .is_some_and(|key| !key.trim().is_empty())
        }) {
            trust.insert(
                (pool.id, pool.revision),
                now + EXTERNAL_POOL_LOCAL_MUTATION_DISPATCH_TRUST_TTL,
            );
        }
        match upsert_pool_id {
            Some(pool_id) if upsert_pool_id != delete_pool_id => {
                trust.retain(|(existing_pool_id, revision), _| {
                    *existing_pool_id != pool_id
                        || Some(*revision) == upsert_pool.map(|pool| pool.revision)
                });
            }
            _ => {}
        }
        self.invalidate_external_pool_policy_state();
    }

    pub fn notify_external_pool_data_changed_with_local_pool(
        &self,
        reason: &'static str,
        pool: &ExternalPool,
    ) {
        self.apply_local_external_pool_snapshot_mutation(Some(pool), None);
        self.publish_external_pool_data_changed_async(reason, Some(pool.id));
    }

    pub fn notify_external_pool_deleted(&self, reason: &'static str, pool_id: u64) {
        self.apply_local_external_pool_snapshot_mutation(None, Some(pool_id));
        self.publish_external_pool_data_changed_async(reason, Some(pool_id));
    }

    pub fn invalidate_external_pool_policy_state(&self) {
        self.selection_runtime_snapshot.lock().take();
        #[cfg(test)]
        self.availability_cache.lock().take();
    }

    fn invalidate_external_pool_runtime_capacity_state(&self) {
        self.selection_runtime_snapshot.lock().take();
        #[cfg(test)]
        self.availability_cache.lock().take();
    }

    fn observe_pool_data_generation(&self, generation: u64) -> bool {
        let mut observed = self.observed_pool_data_generation.load(Ordering::Acquire);
        loop {
            if generation <= observed {
                return false;
            }
            match self.observed_pool_data_generation.compare_exchange_weak(
                observed,
                generation,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.invalidate_static_pool_snapshot();
                    return true;
                }
                Err(current) => observed = current,
            }
        }
    }

    pub fn observe_external_pool_data_event(&self, payload: &str) -> bool {
        let value = match serde_json::from_str::<serde_json::Value>(payload) {
            Ok(value) => value,
            Err(_) => return false,
        };
        let generation = value.get("generation").and_then(serde_json::Value::as_u64);
        let origin = value.get("origin").and_then(serde_json::Value::as_str);
        if origin.is_some_and(|origin| origin == self.instance_id) {
            if let Some(generation) = generation {
                self.observed_pool_data_generation
                    .fetch_max(generation, Ordering::AcqRel);
            }
            return false;
        }
        generation.is_some_and(|generation| self.observe_pool_data_generation(generation))
    }

    pub async fn publish_external_pool_data_changed(
        &self,
        reason: &str,
        pool_id: Option<u64>,
    ) -> anyhow::Result<u64> {
        self.invalidate_static_pool_snapshot();
        let generation = self
            .redis
            .publish_external_pool_data_changed(reason, pool_id)
            .await?;
        self.observed_pool_data_generation
            .fetch_max(generation, Ordering::AcqRel);
        Ok(generation)
    }

    fn publish_external_pool_data_changed_async(&self, reason: &'static str, pool_id: Option<u64>) {
        let redis = self.redis.clone();
        let observed_generation = self.observed_pool_data_generation.clone();
        let origin = self.instance_id.clone();
        tokio::spawn(async move {
            match redis
                .publish_external_pool_data_changed_with_origin(
                    reason,
                    pool_id,
                    Some(origin.as_str()),
                )
                .await
            {
                Ok(generation) => {
                    observed_generation.fetch_max(generation, Ordering::AcqRel);
                }
                Err(err) => tracing::warn!(
                    reason,
                    pool_id = ?pool_id,
                    error = %err,
                    "发布外部池数据跨实例失效通知失败"
                ),
            }
        });
    }

    fn static_pool_snapshot_state(&self, generation: u64) -> Option<CachedStaticPoolSnapshotState> {
        let now = Instant::now();
        let snapshot = self.static_pool_snapshot.lock();
        let cached = snapshot
            .as_ref()
            .filter(|cached| cached.generation == generation && cached.stale_until > now)?;
        Some(if cached.fresh_until > now {
            CachedStaticPoolSnapshotState::Fresh(cached.pools.clone())
        } else {
            CachedStaticPoolSnapshotState::Stale(cached.pools.clone())
        })
    }

    fn spawn_static_pool_snapshot_refresh(
        &self,
        generation: u64,
        refresh_guard: OwnedMutexGuard<()>,
    ) {
        #[cfg(test)]
        let activity = {
            self.static_pool_snapshot_background_refreshes
                .fetch_add(1, Ordering::Relaxed);
            self.static_pool_snapshot_background_in_flight
                .fetch_add(1, Ordering::AcqRel);
            StaticPoolBackgroundRefreshActivity {
                in_flight: self.static_pool_snapshot_background_in_flight.clone(),
            }
        };
        let manager = self.clone();
        tokio::spawn(async move {
            #[cfg(test)]
            let _activity = activity;
            let deadline = Instant::now() + manager.static_pool_snapshot_refresh_timeout;
            let _ = manager
                .refresh_static_pool_snapshot(generation, refresh_guard, deadline)
                .await;
        });
    }

    fn maybe_spawn_static_pool_snapshot_refresh(&self, generation: u64) {
        let Ok(refresh_guard) = self
            .static_pool_snapshot_refresh_lock
            .clone()
            .try_lock_owned()
        else {
            return;
        };
        self.spawn_static_pool_snapshot_refresh(generation, refresh_guard);
    }

    fn publish_static_pool_snapshot_success(
        &self,
        generation: u64,
        pools: Vec<ExternalPoolEligibility>,
    ) -> Option<Arc<Vec<ExternalPoolEligibility>>> {
        let pools = Arc::new(pools);
        let (fresh_ttl, stale_ttl) = self.static_snapshot_ttls(generation);
        self.publish_static_pool_snapshot_arc(generation, pools, true, fresh_ttl, stale_ttl)
    }

    fn publish_static_pool_snapshot_failure(
        &self,
        generation: u64,
    ) -> Option<Arc<Vec<ExternalPoolEligibility>>> {
        let now = Instant::now();
        let failure_retry = deterministic_duration_jitter(
            self.static_pool_snapshot_failure_retry,
            EXTERNAL_POOL_STATIC_SNAPSHOT_JITTER_PERCENT,
            generation ^ 0x4641_494c,
        );
        let stale_ttl = deterministic_duration_jitter(
            self.static_pool_snapshot_stale_ttl,
            EXTERNAL_POOL_STATIC_SNAPSHOT_JITTER_PERCENT,
            generation ^ 0x5354_414c_45,
        );
        let mut snapshot = self.static_pool_snapshot.lock();
        if self.static_pool_snapshot_generation.load(Ordering::Acquire) != generation {
            return None;
        }
        let preserved = snapshot.as_ref().filter(|cached| {
            cached.generation == generation && cached.load_succeeded && cached.stale_until > now
        });
        let (pools, fresh_until, stale_until, load_succeeded) = match preserved {
            Some(cached) => (
                cached.pools.clone(),
                (now + failure_retry).min(cached.stale_until),
                cached.stale_until,
                true,
            ),
            None => (
                Arc::new(Vec::new()),
                now + failure_retry,
                now + stale_ttl,
                false,
            ),
        };
        *snapshot = Some(CachedStaticPoolSnapshot {
            generation,
            pools: pools.clone(),
            fresh_until,
            stale_until: stale_until.max(fresh_until),
            load_succeeded,
        });
        if self.static_pool_snapshot_generation.load(Ordering::Acquire) != generation {
            snapshot.take();
            return None;
        }
        Some(pools)
    }

    async fn refresh_static_pool_snapshot(
        &self,
        generation: u64,
        _refresh_guard: OwnedMutexGuard<()>,
        deadline: Instant,
    ) -> Option<Arc<Vec<ExternalPoolEligibility>>> {
        if self.static_pool_snapshot_generation.load(Ordering::Acquire) != generation {
            return None;
        }

        #[cfg(test)]
        self.static_pool_snapshot_pg_loads
            .fetch_add(1, Ordering::Relaxed);
        let started = Instant::now();
        let pools =
            match tokio::time::timeout_at(deadline, self.postgres.list_external_pool_eligibility())
                .await
            {
                Ok(Ok(pools)) => pools,
                Ok(Err(err)) => {
                    tracing::warn!(
                        error = %err,
                        snapshot_generation = generation,
                        refresh_ms = started.elapsed().as_millis(),
                        "加载外部池静态资格快照失败，短期负缓存后重试"
                    );
                    return self.publish_static_pool_snapshot_failure(generation);
                }
                Err(_) => {
                    tracing::warn!(
                        snapshot_generation = generation,
                        refresh_timeout_ms = self.static_pool_snapshot_refresh_timeout.as_millis(),
                        "加载外部池静态资格快照超时，短期负缓存后重试"
                    );
                    return self.publish_static_pool_snapshot_failure(generation);
                }
            };
        self.publish_static_pool_snapshot_success(generation, pools)
    }

    async fn load_static_pool_snapshot(&self) -> Arc<Vec<ExternalPoolEligibility>> {
        let generation = self.static_pool_snapshot_generation.load(Ordering::Acquire);
        match self.static_pool_snapshot_state(generation) {
            Some(CachedStaticPoolSnapshotState::Fresh(pools)) => return pools,
            Some(CachedStaticPoolSnapshotState::Stale(pools)) => {
                self.maybe_spawn_static_pool_snapshot_refresh(generation);
                return pools;
            }
            None => {}
        }

        let deadline = Instant::now() + self.static_pool_snapshot_refresh_timeout;
        let refresh_guard = match tokio::time::timeout_at(
            deadline,
            self.static_pool_snapshot_refresh_lock.clone().lock_owned(),
        )
        .await
        {
            Ok(refresh_guard) => refresh_guard,
            Err(_) => return Arc::new(Vec::new()),
        };
        let generation = self.static_pool_snapshot_generation.load(Ordering::Acquire);
        match self.static_pool_snapshot_state(generation) {
            Some(CachedStaticPoolSnapshotState::Fresh(pools)) => pools,
            Some(CachedStaticPoolSnapshotState::Stale(pools)) => {
                self.spawn_static_pool_snapshot_refresh(generation, refresh_guard);
                pools
            }
            None => self
                .refresh_static_pool_snapshot(generation, refresh_guard, deadline)
                .await
                .unwrap_or_else(|| Arc::new(Vec::new())),
        }
    }

    #[cfg(test)]
    fn static_pool_snapshot_pg_loads_for_test(&self) -> u64 {
        self.static_pool_snapshot_pg_loads.load(Ordering::Acquire)
    }

    #[cfg(test)]
    fn static_pool_snapshot_generation_for_test(&self) -> u64 {
        self.static_pool_snapshot_generation.load(Ordering::Acquire)
    }

    #[cfg(test)]
    fn static_pool_snapshot_cached_generation_for_test(&self) -> Option<u64> {
        self.static_pool_snapshot
            .lock()
            .as_ref()
            .map(|snapshot| snapshot.generation)
    }

    #[cfg(test)]
    fn static_pool_snapshot_background_refreshes_for_test(&self) -> u64 {
        self.static_pool_snapshot_background_refreshes
            .load(Ordering::Acquire)
    }

    #[cfg(test)]
    fn static_pool_snapshot_background_in_flight_for_test(&self) -> u64 {
        self.static_pool_snapshot_background_in_flight
            .load(Ordering::Acquire)
    }

    #[cfg(test)]
    fn static_pool_snapshot_pool_count_for_test(&self) -> Option<usize> {
        self.static_pool_snapshot
            .lock()
            .as_ref()
            .map(|snapshot| snapshot.pools.len())
    }

    fn cached_authoritative_pool_snapshot(
        &self,
        generation: u64,
    ) -> Option<CachedAuthoritativePoolSnapshotState> {
        let now = Instant::now();
        self.authoritative_pool_snapshot
            .lock()
            .as_ref()
            .filter(|snapshot| snapshot.generation == generation && snapshot.stale_until > now)
            .map(|snapshot| {
                if snapshot.fresh_until > now {
                    CachedAuthoritativePoolSnapshotState::Fresh(snapshot.result.clone())
                } else {
                    match &snapshot.result {
                        Ok(pools) => CachedAuthoritativePoolSnapshotState::Stale(pools.clone()),
                        Err(err) => CachedAuthoritativePoolSnapshotState::Fresh(Err(err.clone())),
                    }
                }
            })
    }

    fn publish_authoritative_pool_snapshot(
        &self,
        generation: u64,
        result: Result<Arc<Vec<ExternalPool>>, ExternalPoolSelectionUnavailable>,
    ) -> bool {
        if self.static_pool_snapshot_generation.load(Ordering::Acquire) != generation {
            return false;
        }
        let now = Instant::now();
        let mut snapshot = self.authoritative_pool_snapshot.lock();
        if self.static_pool_snapshot_generation.load(Ordering::Acquire) != generation {
            return false;
        }
        let result = match result {
            Ok(pools) => Ok(pools),
            Err(err) => {
                if let Some(existing) = snapshot.as_ref().filter(|existing| {
                    existing.generation == generation
                        && existing.stale_until > now
                        && existing.result.is_ok()
                }) {
                    if let Ok(pools) = &existing.result {
                        *snapshot = Some(CachedAuthoritativePoolSnapshot {
                            generation,
                            result: Ok(pools.clone()),
                            fresh_until: now + EXTERNAL_POOL_AUTHORITATIVE_SNAPSHOT_FAILURE_RETRY,
                            stale_until: existing.stale_until,
                        });
                        return true;
                    }
                }
                Err(err)
            }
        };
        let (fresh_until, stale_until) = if result.is_ok() {
            (
                now + EXTERNAL_POOL_AUTHORITATIVE_SNAPSHOT_TTL,
                now + EXTERNAL_POOL_AUTHORITATIVE_SNAPSHOT_STALE_TTL,
            )
        } else {
            let retry_until = now + EXTERNAL_POOL_AUTHORITATIVE_SNAPSHOT_FAILURE_RETRY;
            (retry_until, retry_until)
        };
        *snapshot = Some(CachedAuthoritativePoolSnapshot {
            generation,
            result,
            fresh_until,
            stale_until,
        });
        true
    }

    fn spawn_authoritative_pool_snapshot_refresh(
        &self,
        generation: u64,
        refresh_guard: OwnedMutexGuard<()>,
    ) {
        let manager = self.clone();
        tokio::spawn(async move {
            manager
                .refresh_authoritative_pool_snapshot(generation, refresh_guard)
                .await;
        });
    }

    fn maybe_spawn_authoritative_pool_snapshot_refresh(&self, generation: u64) {
        let Ok(refresh_guard) = self
            .authoritative_pool_snapshot_refresh_lock
            .clone()
            .try_lock_owned()
        else {
            return;
        };
        self.spawn_authoritative_pool_snapshot_refresh(generation, refresh_guard);
    }

    fn cached_static_pool_snapshot_for_local_route(
        &self,
    ) -> Option<Arc<Vec<ExternalPoolEligibility>>> {
        let generation = self.static_pool_snapshot_generation.load(Ordering::Acquire);
        match self.static_pool_snapshot_state(generation) {
            Some(CachedStaticPoolSnapshotState::Fresh(pools)) => Some(pools),
            Some(CachedStaticPoolSnapshotState::Stale(pools)) => {
                self.maybe_spawn_static_pool_snapshot_refresh(generation);
                Some(pools)
            }
            None => {
                self.maybe_spawn_static_pool_snapshot_refresh(generation);
                None
            }
        }
    }

    fn cached_authoritative_pool_snapshot_for_local_route(&self) -> Option<Arc<Vec<ExternalPool>>> {
        let generation = self.static_pool_snapshot_generation.load(Ordering::Acquire);
        match self.cached_authoritative_pool_snapshot(generation) {
            Some(CachedAuthoritativePoolSnapshotState::Fresh(Ok(pools))) => Some(pools),
            Some(CachedAuthoritativePoolSnapshotState::Fresh(Err(err))) => {
                tracing::debug!(
                    unavailable_kind = err.kind.as_str(),
                    retry_after_ms = err.retry_after.as_millis() as u64,
                    "外部池权威快照当前不可用；本地主路径保持本地调度语义"
                );
                None
            }
            Some(CachedAuthoritativePoolSnapshotState::Stale(pools)) => {
                self.maybe_spawn_authoritative_pool_snapshot_refresh(generation);
                Some(pools)
            }
            None => {
                self.maybe_spawn_authoritative_pool_snapshot_refresh(generation);
                None
            }
        }
    }

    async fn refresh_authoritative_pool_snapshot(
        &self,
        generation: u64,
        _refresh_guard: OwnedMutexGuard<()>,
    ) {
        let result = match self.selection_breaker.try_begin() {
            Ok(permit) => {
                #[cfg(test)]
                self.authoritative_pool_snapshot_pg_loads
                    .fetch_add(1, Ordering::Relaxed);
                match timeout(
                    EXTERNAL_POOL_SELECTION_POSTGRES_TIMEOUT,
                    self.postgres.list_dispatchable_external_pools(),
                )
                .await
                {
                    Ok(Ok(pools)) => {
                        permit.success();
                        Ok(Arc::new(pools))
                    }
                    Ok(Err(_)) => {
                        permit.failure(ExternalPoolSelectionFailureKind::PostgresError);
                        Err(ExternalPoolSelectionUnavailable {
                            kind: ExternalPoolSelectionFailureKind::PostgresError,
                            retry_after: EXTERNAL_POOL_SELECTION_RETRY_AFTER,
                        })
                    }
                    Err(_) => {
                        permit.failure(ExternalPoolSelectionFailureKind::PostgresTimeout);
                        Err(ExternalPoolSelectionUnavailable {
                            kind: ExternalPoolSelectionFailureKind::PostgresTimeout,
                            retry_after: EXTERNAL_POOL_SELECTION_RETRY_AFTER,
                        })
                    }
                }
            }
            Err(unavailable) => {
                if unavailable.kind == ExternalPoolSelectionFailureKind::AdmissionSaturated {
                    self.selection_saturated.fetch_add(1, Ordering::Relaxed);
                }
                Err(unavailable)
            }
        };
        self.publish_authoritative_pool_snapshot(generation, result);
        self.authoritative_pool_snapshot_changed.notify_waiters();
    }

    async fn load_authoritative_pool_snapshot(
        &self,
    ) -> Result<Arc<Vec<ExternalPool>>, ExternalPoolSelectionUnavailable> {
        let deadline = Instant::now() + EXTERNAL_POOL_AUTHORITATIVE_SNAPSHOT_WAIT_TIMEOUT;
        loop {
            let changed = self.authoritative_pool_snapshot_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();

            let generation = self.static_pool_snapshot_generation.load(Ordering::Acquire);
            match self.cached_authoritative_pool_snapshot(generation) {
                Some(CachedAuthoritativePoolSnapshotState::Fresh(result)) => return result,
                Some(CachedAuthoritativePoolSnapshotState::Stale(pools)) => {
                    self.maybe_spawn_authoritative_pool_snapshot_refresh(generation);
                    return Ok(pools);
                }
                None => {}
            }
            if let Ok(refresh_guard) = self
                .authoritative_pool_snapshot_refresh_lock
                .clone()
                .try_lock_owned()
            {
                let generation = self.static_pool_snapshot_generation.load(Ordering::Acquire);
                match self.cached_authoritative_pool_snapshot(generation) {
                    Some(CachedAuthoritativePoolSnapshotState::Fresh(result)) => return result,
                    Some(CachedAuthoritativePoolSnapshotState::Stale(pools)) => {
                        self.spawn_authoritative_pool_snapshot_refresh(generation, refresh_guard);
                        return Ok(pools);
                    }
                    None => {}
                }
                self.spawn_authoritative_pool_snapshot_refresh(generation, refresh_guard);
            }

            let generation = self.static_pool_snapshot_generation.load(Ordering::Acquire);
            match self.cached_authoritative_pool_snapshot(generation) {
                Some(CachedAuthoritativePoolSnapshotState::Fresh(result)) => return result,
                Some(CachedAuthoritativePoolSnapshotState::Stale(pools)) => return Ok(pools),
                None => {}
            }
            if tokio::time::timeout_at(deadline, changed.as_mut())
                .await
                .is_err()
            {
                return Err(ExternalPoolSelectionUnavailable {
                    kind: ExternalPoolSelectionFailureKind::PostgresTimeout,
                    retry_after: EXTERNAL_POOL_SELECTION_RETRY_AFTER,
                });
            }
        }
    }

    #[cfg(test)]
    fn authoritative_pool_snapshot_pg_loads_for_test(&self) -> u64 {
        self.authoritative_pool_snapshot_pg_loads
            .load(Ordering::Acquire)
    }

    fn mark_external_pool_coordinator_probe_required(&self) {
        self.coordinator_probe_required
            .store(true, Ordering::Release);
        self.coordinator_next_probe_ms.store(0, Ordering::Release);
    }

    fn local_mutation_dispatch_trusted(&self, pool: &ExternalPool) -> bool {
        let now = Instant::now();
        let mut trust = self.local_mutation_dispatch_trust.lock();
        trust.retain(|_, expires_at| *expires_at > now);
        trust
            .get(&(pool.id, pool.revision))
            .is_some_and(|expires_at| *expires_at > now)
    }

    async fn external_pool_coordination_epoch(&self) -> anyhow::Result<String> {
        let now_ms = external_pool_coordination_monotonic_ms();
        let cached_epoch = self.coordinator_epoch.lock().clone();
        if let Some(epoch) = cached_epoch.filter(|_| {
            !self.coordinator_probe_required.load(Ordering::Acquire)
                && now_ms < self.coordinator_next_probe_ms.load(Ordering::Acquire)
        }) {
            return Ok(epoch);
        }
        let _local_lock = self.coordinator_reconcile_lock.lock().await;
        let now_ms = external_pool_coordination_monotonic_ms();
        let probe_required = self.coordinator_probe_required.load(Ordering::Acquire);
        let cached_epoch = self.coordinator_epoch.lock().clone();
        let cached_run_id = self.coordinator_run_id.lock().clone();
        if let Some(epoch) = cached_epoch.clone().filter(|_| {
            !probe_required && now_ms < self.coordinator_next_probe_ms.load(Ordering::Acquire)
        }) {
            return Ok(epoch);
        }

        let current_run_id = self.redis.external_pool_redis_run_id().await?;
        match (cached_epoch.as_ref(), cached_run_id.as_ref()) {
            (Some(epoch), Some(run_id)) if *run_id == current_run_id => {
                if !probe_required {
                    self.record_external_pool_coordinator_probe(&current_run_id, epoch);
                    return Ok(epoch.clone());
                }
                match self
                    .redis
                    .external_pool_coordinator_guard_state_for_epoch(epoch)
                    .await?
                {
                    ExternalPoolCoordinatorGuardState::Ready { .. }
                    | ExternalPoolCoordinatorGuardState::Recovering { .. } => {
                        self.record_external_pool_coordinator_probe(&current_run_id, epoch);
                        return Ok(epoch.clone());
                    }
                    ExternalPoolCoordinatorGuardState::EpochMismatch { .. } => {}
                }
            }
            _ => {}
        }

        self.reconcile_external_pool_coordinator_guard(current_run_id)
            .await
    }

    fn record_external_pool_coordinator_probe(&self, run_id: &str, epoch: &str) {
        *self.coordinator_run_id.lock() = Some(run_id.to_string());
        *self.coordinator_epoch.lock() = Some(epoch.to_string());
        self.coordinator_probe_required
            .store(false, Ordering::Release);
        self.coordinator_next_probe_ms.store(
            external_pool_coordination_monotonic_ms().saturating_add(
                Duration::from_secs(EXTERNAL_POOL_COORDINATOR_RUN_ID_PROBE_INTERVAL_SECS)
                    .as_millis() as u64,
            ),
            Ordering::Release,
        );
    }

    async fn reconcile_external_pool_coordinator_guard(
        &self,
        mut current_run_id: String,
    ) -> anyhow::Result<String> {
        let mut postgres = self.postgres.pool().acquire().await?;
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(EXTERNAL_POOL_COORDINATOR_POSTGRES_LOCK_ID)
            .execute(&mut *postgres)
            .await?;

        let result = async {
            current_run_id = self.redis.external_pool_redis_run_id().await?;
            let existing: Option<serde_json::Value> =
                sqlx::query_scalar("SELECT config FROM runtime_config WHERE id = $1")
                    .bind(EXTERNAL_POOL_COORDINATOR_RUNTIME_CONFIG_ID)
                    .fetch_optional(&mut *postgres)
                    .await?;
            let existing_run_id = existing
                .as_ref()
                .and_then(|value| value.get("redisRunId"))
                .and_then(serde_json::Value::as_str);
            let existing_epoch = existing
                .as_ref()
                .and_then(|value| value.get("coordinationEpoch"))
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty());

            if let (Some(run_id), Some(epoch)) = (existing_run_id, existing_epoch) {
                if run_id == current_run_id {
                    match self
                        .redis
                        .external_pool_coordinator_guard_state_for_epoch(epoch)
                        .await?
                    {
                        ExternalPoolCoordinatorGuardState::Ready { .. }
                        | ExternalPoolCoordinatorGuardState::Recovering { .. } => {
                            return Ok(epoch.to_string());
                        }
                        ExternalPoolCoordinatorGuardState::EpochMismatch { .. } => {}
                    }
                }
            }

            let recovery_required = existing.is_some();
            let coordination_epoch = uuid::Uuid::new_v4().to_string();
            let coordination_record = json!({
                "redisRunId": current_run_id,
                "coordinationEpoch": coordination_epoch,
            });
            sqlx::query(
                r#"
                INSERT INTO runtime_config (id, config, updated_at)
                VALUES ($1, $2, now())
                ON CONFLICT (id) DO UPDATE
                SET config = EXCLUDED.config,
                    version = runtime_config.version + 1,
                    updated_at = now()
                "#,
            )
            .bind(EXTERNAL_POOL_COORDINATOR_RUNTIME_CONFIG_ID)
            .bind(coordination_record)
            .execute(&mut *postgres)
            .await?;
            self.redis
                .install_external_pool_coordinator_guard(
                    &coordination_epoch,
                    if recovery_required {
                        self.coordinator_recovery_grace
                    } else {
                        Duration::ZERO
                    },
                )
                .await?;
            let confirmed_run_id = self.redis.external_pool_redis_run_id().await?;
            if confirmed_run_id != current_run_id {
                anyhow::bail!(
                    "Redis restarted while external pool coordinator epoch was installed"
                );
            }
            Ok(coordination_epoch)
        }
        .await;
        let unlock = sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(EXTERNAL_POOL_COORDINATOR_POSTGRES_LOCK_ID)
            .execute(&mut *postgres)
            .await;
        match result {
            Ok(epoch) => {
                unlock?;
                self.record_external_pool_coordinator_probe(&current_run_id, &epoch);
                Ok(epoch)
            }
            Err(err) => {
                let _ = unlock;
                self.mark_external_pool_coordinator_probe_required();
                Err(err)
            }
        }
    }

    pub async fn status(
        &self,
        config: &ExternalPoolsConfig,
    ) -> anyhow::Result<Vec<ExternalPoolStatus>> {
        let pools = self.postgres.list_external_pools(true).await?;
        let pool_ids = pools.iter().map(|pool| pool.id).collect::<Vec<_>>();
        let runtimes = self.load_pool_runtime_snapshots(&pool_ids, &[]).await?;
        let mut statuses = Vec::with_capacity(pools.len());
        for (pool, runtime) in pools.into_iter().zip(runtimes) {
            let Ok(runtime) = runtime else {
                statuses.push(ExternalPoolStatus {
                    pool,
                    in_flight: 0,
                    cooldown_remaining_secs: 0,
                    cooldown_reason: None,
                    transient_failure_streak: 0,
                    transient_failure_ttl_secs: 0,
                    dispatchable: false,
                    skipped_reason: Some("coordinator_state_invalid".to_string()),
                });
                continue;
            };
            let in_flight = runtime.in_flight;
            let global_in_flight = runtime.global_in_flight;
            let cooldown_remaining_secs = runtime.pool_cooldown_remaining_secs;
            let cooldown_reason = runtime.pool_cooldown_reason;
            let transient_failure_streak = runtime.transient_failure_streak;
            let transient_failure_ttl_secs = runtime
                .transient_failure_ttl
                .map(|ttl| ttl.as_secs().max(1))
                .unwrap_or(0);
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
                transient_failure_streak,
                transient_failure_ttl_secs,
                skipped_reason,
            });
        }
        Ok(statuses)
    }

    #[cfg(test)]
    pub async fn has_available_pool(&self, config: &ExternalPoolsConfig) -> bool {
        self.pool_availability_snapshot(&HashSet::new(), config)
            .await
            .available_pools
            > 0
    }

    #[cfg(test)]
    pub async fn has_eligible_pool(&self, config: &ExternalPoolsConfig) -> bool {
        self.has_eligible_pool_matching(config, None, None, None)
            .await
    }

    #[cfg(test)]
    pub async fn has_eligible_pool_for_model(
        &self,
        config: &ExternalPoolsConfig,
        model: &str,
    ) -> bool {
        let model_candidates = normalize_external_pool_support_candidates([model]);
        self.has_eligible_pool_matching(config, None, Some(&model_candidates), None)
            .await
    }

    pub async fn has_eligible_pool_for_route_and_model(
        &self,
        config: &ExternalPoolsConfig,
        endpoint: &str,
        model: &str,
    ) -> bool {
        let model_candidates = normalize_external_pool_support_candidates([model]);
        self.has_eligible_pool_matching(config, None, Some(&model_candidates), Some(endpoint))
            .await
    }

    #[cfg(test)]
    pub async fn has_eligible_pool_for_body_mode_and_model(
        &self,
        config: &ExternalPoolsConfig,
        body_mode: ExternalPoolRequestBodyMode,
        model: Option<&str>,
    ) -> bool {
        let model_candidates = normalize_external_pool_support_candidates(model);
        self.has_eligible_pool_matching(config, Some(body_mode), Some(&model_candidates), None)
            .await
    }

    pub fn has_cached_eligible_pool_for_route_and_model(
        &self,
        config: &ExternalPoolsConfig,
        endpoint: &str,
        model: &str,
    ) -> bool {
        let model_candidates = normalize_external_pool_support_candidates([model]);
        self.has_cached_eligible_pool_matching(
            config,
            None,
            Some(&model_candidates),
            Some(endpoint),
        )
    }

    fn has_cached_eligible_pool_matching(
        &self,
        config: &ExternalPoolsConfig,
        body_mode_filter: Option<ExternalPoolRequestBodyMode>,
        model_candidates: Option<&[String]>,
        endpoint: Option<&str>,
    ) -> bool {
        let _ = body_mode_filter;
        if !config.external_pools_enabled {
            return false;
        }
        let Some(pools) = self.cached_static_pool_snapshot_for_local_route() else {
            return false;
        };
        let now = Utc::now();
        pools.iter().any(|pool| {
            pool.enabled
                && !pool.is_auto_disabled_at(now)
                && external_pool_eligibility_route_allowed(pool, endpoint)
                && external_pool_eligibility_matches_supported_models(pool, model_candidates)
        })
    }

    #[cfg(test)]
    pub async fn has_immediately_available_pool_for_model(
        &self,
        config: &ExternalPoolsConfig,
        model: &str,
        max_wait: Duration,
    ) -> bool {
        let model_candidates = normalize_external_pool_support_candidates([model]);
        self.has_immediately_available_pool_matching(
            config,
            None,
            Some(&model_candidates),
            None,
            max_wait,
        )
        .await
    }

    pub async fn has_immediately_available_pool_for_route_and_model(
        &self,
        config: &ExternalPoolsConfig,
        endpoint: &str,
        model: &str,
        max_wait: Duration,
    ) -> bool {
        let model_candidates = normalize_external_pool_support_candidates([model]);
        self.has_immediately_available_pool_matching(
            config,
            None,
            Some(&model_candidates),
            Some(endpoint),
            max_wait,
        )
        .await
    }

    #[cfg(test)]
    pub fn has_cached_immediately_available_pool_for_model(
        &self,
        config: &ExternalPoolsConfig,
        model: &str,
    ) -> bool {
        let model_candidates = normalize_external_pool_support_candidates([model]);
        self.has_cached_immediately_available_pool_matching(
            config,
            None,
            Some(&model_candidates),
            None,
        )
    }

    pub fn has_cached_immediately_available_pool_for_route_and_model(
        &self,
        config: &ExternalPoolsConfig,
        endpoint: &str,
        model: &str,
    ) -> bool {
        let model_candidates = normalize_external_pool_support_candidates([model]);
        self.has_cached_immediately_available_pool_matching(
            config,
            None,
            Some(&model_candidates),
            Some(endpoint),
        )
    }

    async fn has_eligible_pool_matching(
        &self,
        config: &ExternalPoolsConfig,
        body_mode_filter: Option<ExternalPoolRequestBodyMode>,
        model_candidates: Option<&[String]>,
        endpoint: Option<&str>,
    ) -> bool {
        let _ = body_mode_filter;
        if !config.external_pools_enabled {
            return false;
        }
        let pools = self.load_static_pool_snapshot().await;
        let now = Utc::now();
        pools.iter().any(|pool| {
            pool.enabled
                && !pool.is_auto_disabled_at(now)
                && external_pool_eligibility_route_allowed(pool, endpoint)
                && external_pool_eligibility_matches_supported_models(pool, model_candidates)
        })
    }

    fn has_cached_immediately_available_pool_matching(
        &self,
        config: &ExternalPoolsConfig,
        body_mode_filter: Option<ExternalPoolRequestBodyMode>,
        model_candidates: Option<&[String]>,
        endpoint: Option<&str>,
    ) -> bool {
        let _ = body_mode_filter;
        if !config.external_pools_enabled {
            return false;
        }
        let Some(authoritative_pools) = self.cached_authoritative_pool_snapshot_for_local_route()
        else {
            return false;
        };
        let pools = authoritative_pools
            .iter()
            .filter(|pool| {
                pool.enabled
                    && !pool.is_auto_disabled_now()
                    && external_pool_route_allowed(pool, endpoint)
                    && external_pool_matches_supported_models_normalized(pool, model_candidates)
            })
            .cloned()
            .collect::<Vec<_>>();
        if pools.is_empty() {
            return false;
        }

        let requested_model_cooldowns: &[String] = if config
            .external_pool_model_unavailable_cooldown_mode
            == ExternalPoolModelUnavailableCooldownMode::Model
        {
            model_candidates.unwrap_or(&[])
        } else {
            &[]
        };
        let pool_ids = pools.iter().map(|pool| pool.id).collect::<Vec<_>>();
        let key = SelectionRuntimeSnapshotKey {
            pool_ids,
            models: requested_model_cooldowns.to_vec(),
        };
        let Some(cached_runtimes) = self.cached_selection_runtime_snapshots(&key) else {
            self.maybe_spawn_selection_runtime_snapshot_refresh(key);
            return false;
        };
        let selection = self.scan_pool_availability_from_filtered_pools_and_runtimes(
            pools,
            materialize_selection_runtime_snapshots(cached_runtimes),
            config,
            true,
        );
        selection.selected_pool.is_some() || selection.availability.available_pools > 0
    }

    async fn has_immediately_available_pool_matching(
        &self,
        config: &ExternalPoolsConfig,
        body_mode_filter: Option<ExternalPoolRequestBodyMode>,
        model_candidates: Option<&[String]>,
        endpoint: Option<&str>,
        max_wait: Duration,
    ) -> bool {
        if !config.external_pools_enabled {
            return false;
        }
        let max_wait = max_wait.max(Duration::from_millis(1));
        let check = async {
            let authoritative_pools = match self.load_authoritative_pool_snapshot().await {
                Ok(pools) => pools,
                Err(err) => {
                    tracing::debug!(
                        unavailable_kind = err.kind.as_str(),
                        retry_after_ms = err.retry_after.as_millis() as u64,
                        "外部池当前可用性检查未能快速加载权威快照"
                    );
                    return false;
                }
            };
            let cooldown_candidates = model_candidates.unwrap_or(&[]);
            let selection = self
                .scan_pool_availability_from_snapshot(
                    &authoritative_pools,
                    &HashSet::new(),
                    config,
                    true,
                    body_mode_filter,
                    model_candidates,
                    Some(cooldown_candidates),
                    endpoint,
                )
                .await;
            selection.selected_pool.is_some() || selection.availability.available_pools > 0
        };
        match timeout(max_wait, check).await {
            Ok(available) => available,
            Err(_) => {
                tracing::debug!(
                    max_wait_ms = max_wait.as_millis() as u64,
                    "外部池当前可用性检查超过本地调度保护预算，保持本地原始排队语义"
                );
                false
            }
        }
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

        if !config.external_pool_route_allowed(&route.endpoint) {
            self.record_external_failure(
                &route,
                None,
                Vec::new(),
                "external_pool_route_blocked",
                "request route is blocked by external pool route policy",
                synthetic_external_error_diagnostics(
                    &route,
                    StatusCode::SERVICE_UNAVAILABLE,
                    "external_dispatch",
                ),
            );
            return ExternalPoolForwardOutcome::FinalError(ExternalPoolFinalError {
                status: StatusCode::SERVICE_UNAVAILABLE,
                response_error_type: "service_unavailable".to_string(),
                route_error_type: "external_pool_route_blocked".to_string(),
                message: "request route is blocked by external pool route policy".to_string(),
                error_id: route.error_id.clone(),
                retryable: false,
                attempts: Vec::new(),
                pool_id: None,
                pool_name: None,
            });
        }

        let payload_guard_retry_enabled = route.payload_guard_retry_config.is_some();
        let mut max_pool_attempts = if config.external_pool_retry_max_attempts == 0 {
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
        let mut send_attempt_index = 0usize;
        let mut pool_attempt_index = 0usize;
        let mut same_pool_retry_counts: HashMap<u64, u32> = HashMap::new();
        let mut attempt_rejection = None;
        let mut preselected_pool: Option<ExternalPool> = None;
        let dispatch_deadline = external_dispatch_deadline(&route, &config);
        let authoritative_pools = match self.load_authoritative_pool_snapshot().await {
            Ok(pools) => pools,
            Err(unavailable) => {
                let snapshot = selection_unavailable_snapshot(unavailable).availability;
                let context = snapshot.capacity_context();
                let (error_type, message) = external_capacity_error(context.reason);
                self.record_external_failure(
                    &route,
                    None,
                    Vec::new(),
                    error_type,
                    message,
                    synthetic_external_capacity_error_diagnostics(
                        &route,
                        StatusCode::SERVICE_UNAVAILABLE,
                        "external_dispatch",
                        &config,
                        Some(&context),
                    ),
                );
                return ExternalPoolForwardOutcome::FinalError(external_capacity_final_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    error_type,
                    message,
                    &route.error_id,
                ));
            }
        };

        loop {
            if dispatch_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return self.external_dispatch_deadline_outcome(&route, attempts, &config);
            }
            let mut capacity_waiter = self.capacity_signal.register();
            if max_pool_attempts.is_some_and(|max_attempts| pool_attempt_index >= max_attempts) {
                break;
            }
            if route.inference_attempt_budget.available_attempts(0) == 0 {
                let snapshot = route.inference_attempt_budget.snapshot();
                attempt_rejection = Some(if snapshot.downstream_committed {
                    InferenceAttemptRejection::DownstreamCommitted
                } else {
                    InferenceAttemptRejection::Exhausted
                });
                break;
            }
            let mut selection = match preselected_pool.take() {
                Some(pool) => PoolSelectionSnapshot {
                    selected_pool: Some(pool),
                    availability: PoolAvailabilitySnapshot::default(),
                    degraded_fallback_local_lease: false,
                },
                None => {
                    self.select_pool_for_route_from_snapshot(
                        &authoritative_pools,
                        &excluded,
                        &config,
                        &route,
                    )
                    .await
                }
            };
            if selection.selected_pool.is_none()
                && selection.availability.coordinator_unavailable
                && route_allows_degraded_fallback_local_lease(&route)
            {
                if let Some(degraded_selection) = self
                    .select_degraded_fallback_local_pool_from_snapshot(
                        &authoritative_pools,
                        &excluded,
                        &route,
                        &config,
                    )
                {
                    tracing::warn!(
                        request_id = %route.request_id,
                        fallback_reason = ?route.fallback_reason,
                        "外部池 Redis coordinator 与本地 scheduler 同时不可用，使用进程内有界 emergency external lease"
                    );
                    selection = degraded_selection;
                }
            }
            if max_pool_attempts.is_none() {
                max_pool_attempts = Some(
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
                            snapshot.capacity_context(),
                            &mut queue_guard,
                            &mut wait_started_at,
                            &mut capacity_waiter,
                            dispatch_deadline,
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
            let acquire_result = if selection.degraded_fallback_local_lease {
                self.acquire_degraded_fallback_local_pool(&pool)
            } else {
                match self.acquire_pool_for_route(&pool, &config, &route).await {
                    PoolAcquireResult::Acquired(lease) => PoolAcquireResult::Acquired(lease),
                    PoolAcquireResult::Unavailable(unavailable)
                        if unavailable.reason == PoolCapacityWaitReason::CoordinatorUnavailable
                            && route_allows_degraded_fallback_local_lease(&route) =>
                    {
                        tracing::warn!(
                            request_id = %route.request_id,
                            pool_id,
                            detail = unavailable.detail,
                            "外部池 Redis lease 准入在 scheduler degraded fallback 中不可用，尝试进程内有界 emergency external lease"
                        );
                        self.acquire_degraded_fallback_local_pool(&pool)
                    }
                    PoolAcquireResult::Unavailable(unavailable) => {
                        PoolAcquireResult::Unavailable(unavailable)
                    }
                }
            };
            let lease = match acquire_result {
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
                        let reselection = self
                            .select_pool_for_route_from_snapshot(
                                &authoritative_pools,
                                &excluded,
                                &config,
                                &route,
                            )
                            .await;
                        if reselection.availability.coordinator_unavailable {
                            unavailable = PoolAcquireUnavailable {
                                reason: PoolCapacityWaitReason::CoordinatorUnavailable,
                                wait_for: None,
                                exclude_pool_for_reselect: false,
                                detail: "reselection_coordinator_error",
                            };
                        } else if let Some(next_pool) = reselection.selected_pool {
                            preselected_pool = Some(next_pool);
                            continue;
                        }
                        excluded.remove(&pool_id);
                    }
                    match self
                        .handle_capacity_unavailable(
                            &route,
                            attempts.clone(),
                            &config,
                            PoolCapacityWaitContext::from_unavailable(&unavailable),
                            &mut queue_guard,
                            &mut wait_started_at,
                            &mut capacity_waiter,
                            dispatch_deadline,
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
            capacity_waiter.finish_acquired();
            drop(queue_guard.take());
            let started = std::time::Instant::now();
            let current_attempt = send_attempt_index.saturating_add(1) as u32;
            let prepared = self.prepare_forward_once(&pool, &route, &config);
            let result = match prepared {
                Ok(prepared) => {
                    #[cfg(test)]
                    self.wait_at_dispatch_after_prepare_gate().await;
                    match self.validate_pool_dispatch_fence(&pool).await {
                        PoolDispatchFenceResult::Current => {}
                        PoolDispatchFenceResult::Changed => {
                            drop(lease);
                            excluded.insert(pool_id);
                            tracing::debug!(
                                pool_id,
                                expected_revision = pool.revision,
                                "外部池在请求准备后、HTTP send 前发生配置变更，拒绝旧快照并重选"
                            );
                            continue;
                        }
                        PoolDispatchFenceResult::CoordinatorUnavailable(unavailable) => {
                            if route_allows_degraded_fallback_local_lease(&route)
                                && self.local_mutation_dispatch_trusted(&pool)
                            {
                                tracing::warn!(
                                    request_id = %route.request_id,
                                    pool_id = pool.id,
                                    pool_revision = pool.revision,
                                    fallback_reason = ?route.fallback_reason,
                                    unavailable_kind = unavailable.kind.as_str(),
                                    "本地 scheduler degraded 且外部池 dispatch fence 暂不可用，使用本进程刚提交的外部池 revision 有界兜底"
                                );
                            } else {
                                drop(lease);
                                let snapshot =
                                    selection_unavailable_snapshot(unavailable).availability;
                                match self
                                    .handle_capacity_unavailable(
                                        &route,
                                        attempts.clone(),
                                        &config,
                                        snapshot.capacity_context(),
                                        &mut queue_guard,
                                        &mut wait_started_at,
                                        &mut capacity_waiter,
                                        dispatch_deadline,
                                    )
                                    .await
                                {
                                    ExternalCapacityDecision::Retry => continue,
                                    ExternalCapacityDecision::FinalError(err) => {
                                        return ExternalPoolForwardOutcome::FinalError(err);
                                    }
                                }
                            }
                        }
                    }
                    self.forward_prepared_once(&pool, &route, lease, &config, prepared)
                        .await
                }
                Err(err) => {
                    drop(lease);
                    Err(err)
                }
            };
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
                                ExternalStreamUsageRecordContext {
                                    config: config.clone(),
                                    route: route.clone(),
                                    pool,
                                    attempts: attempts.clone(),
                                    outbound_model: forwarded.outbound_model,
                                    outbound_body: forwarded.outbound_body,
                                    usage_capture: forwarded.stream_usage_capture,
                                    usage_projection: forwarded.stream_usage_projection,
                                },
                            ),
                        );
                    }
                    if let Some(projection) = forwarded.stream_usage_projection.as_ref() {
                        projection.record_success();
                    }
                    route.inference_attempt_budget.mark_downstream_committed();
                    self.record_external_success(
                        &route,
                        &pool,
                        attempts.clone(),
                        forwarded.billing,
                    );
                    return ExternalPoolForwardOutcome::Response(forwarded.response);
                }
                Err(forward_err) => {
                    if let Some(rejection) = forward_err.attempt_rejection {
                        return self
                            .external_attempt_rejection_outcome(&route, attempts, rejection);
                    }
                    send_attempt_index = send_attempt_index.saturating_add(1);
                    let outbound_model = forward_err.outbound_model;
                    let err = forward_err.err;
                    let cross_pool_retry = should_retry_external_cross_pool(&config, &err);
                    let action = if cross_pool_retry {
                        "retry_next"
                    } else {
                        "fail"
                    };
                    attempts.push(ExternalPoolAttempt {
                        attempt: current_attempt,
                        pool_id,
                        pool_name: pool.name.clone(),
                        outbound_model: outbound_model.clone(),
                        status: err.status.map(|status| status.as_u16()),
                        action: action.to_string(),
                        duration_ms: started.elapsed().as_millis() as u64,
                        error_type: Some(error_type_for_external_error(&err).to_string()),
                        error_message: Some(err.message.clone()),
                    });
                    if let Some(retry_route) = (pool.request_body_mode
                        == ExternalPoolRequestBodyMode::Normalized
                        && should_retry_external_payload_guard(&route, &err))
                    .then(|| external_payload_guard_retry_route(&route))
                    .flatten()
                    {
                        if let Some(last) = attempts.last_mut() {
                            last.action = "payload_guard_retry".to_string();
                        }
                        route = retry_route;
                        excluded.clear();
                        same_pool_retry_counts.clear();
                        last_error = None;
                        continue;
                    }
                    let cooldown_hint = err.cooldown.clone();
                    if let Some((_, reason)) = &cooldown_hint {
                        if should_record_external_pool_soft_failure(reason) {
                            self.record_external_pool_soft_failure(pool_id, reason)
                                .await;
                        }
                    }
                    let same_pool_retry_count = same_pool_retry_counts.entry(pool_id).or_insert(0);
                    if should_retry_external_same_pool(&config, &err)
                        && *same_pool_retry_count < config.external_pool_same_pool_retry_count
                        && route.inference_attempt_budget.available_attempts(0) > 0
                    {
                        *same_pool_retry_count = (*same_pool_retry_count).saturating_add(1);
                        if let Some(last) = attempts.last_mut() {
                            last.action = "retry_same_pool".to_string();
                        }
                        tracing::warn!(
                            request_id = %route.request_id,
                            error_id = %route.error_id,
                            pool_id,
                            pool_name = %pool.name,
                            status = err.status.map(|status| status.as_u16()),
                            same_pool_retry_count = *same_pool_retry_count,
                            same_pool_retry_max = config.external_pool_same_pool_retry_count,
                            "external pool retryable status will be retried on the same pool before cross-pool failover"
                        );
                        last_error = Some((pool.clone(), err));
                        if let Some(delay) = external_same_pool_retry_delay(&config) {
                            let delay = dispatch_deadline
                                .map(|deadline| {
                                    delay.min(deadline.saturating_duration_since(Instant::now()))
                                })
                                .unwrap_or(delay);
                            if !delay.is_zero() {
                                tokio::time::sleep(delay).await;
                            }
                        }
                        preselected_pool = Some(pool);
                        continue;
                    }
                    if let Some((duration, reason)) = &cooldown_hint {
                        if reason == "model_mapping_miss" {
                            // Request-scoped mismatch: skip this pool for the current request, but
                            // do not cool down the pool globally for other models.
                        } else if reason == "model_unavailable" {
                            match config.external_pool_model_unavailable_cooldown_mode {
                                ExternalPoolModelUnavailableCooldownMode::Disabled => {}
                                ExternalPoolModelUnavailableCooldownMode::Model => {
                                    let mut models = route.model_cooldown_candidates();
                                    if let Some(model) = outbound_model
                                        .as_deref()
                                        .and_then(normalize_external_pool_model_cooldown_key_model)
                                        .filter(|model| {
                                            !models.iter().any(|existing| existing == model)
                                        })
                                    {
                                        models.push(model);
                                    }
                                    self.mark_pool_model_cooldowns(
                                        pool_id,
                                        *duration,
                                        reason.clone(),
                                        &models,
                                    )
                                    .await;
                                }
                                ExternalPoolModelUnavailableCooldownMode::Pool => {
                                    self.mark_pool_cooldown(pool_id, *duration, reason.clone())
                                        .await;
                                }
                            }
                        } else if should_mark_external_pool_hard_cooldown(reason) {
                            self.mark_pool_cooldown(pool_id, *duration, reason.clone())
                                .await;
                        }
                    }
                    if let Some(reason) = &err.auto_disable_reason {
                        self.auto_disable_pool_if_configured(&pool, &config, reason, &err.message)
                            .await;
                    }
                    if cross_pool_retry {
                        excluded.insert(pool_id);
                        last_error = Some((pool, err));
                        pool_attempt_index = pool_attempt_index.saturating_add(1);
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

        if let Some(rejection) = attempt_rejection {
            return self.external_attempt_rejection_outcome(&route, attempts, rejection);
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

    fn external_attempt_rejection_outcome(
        &self,
        route: &ExternalRouteRequest,
        attempts: Vec<ExternalPoolAttempt>,
        rejection: InferenceAttemptRejection,
    ) -> ExternalPoolForwardOutcome {
        let reason = inference_attempt_rejection_reason(rejection);
        let record_message =
            format!("external dispatch rejected by request attempt policy: {reason}");
        self.record_external_failure(
            route,
            None,
            attempts.clone(),
            "inference_attempt_policy",
            &record_message,
            synthetic_external_error_diagnostics(route, StatusCode::SERVICE_UNAVAILABLE, reason),
        );
        ExternalPoolForwardOutcome::FinalError(external_attempt_rejection_final_error(
            route, attempts,
        ))
    }

    fn external_dispatch_deadline_outcome(
        &self,
        route: &ExternalRouteRequest,
        attempts: Vec<ExternalPoolAttempt>,
        config: &ExternalPoolsConfig,
    ) -> ExternalPoolForwardOutcome {
        let message = "Request dispatch deadline exceeded";
        self.record_external_failure(
            route,
            None,
            attempts.clone(),
            "external_pool_deadline_exceeded",
            message,
            synthetic_external_capacity_error_diagnostics(
                route,
                StatusCode::SERVICE_UNAVAILABLE,
                "external_dispatch",
                config,
                None,
            ),
        );
        ExternalPoolForwardOutcome::FinalError(external_capacity_final_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "external_pool_deadline_exceeded",
            message,
            &route.error_id,
        ))
    }

    fn prepare_forward_once(
        &self,
        pool: &ExternalPool,
        route: &ExternalRouteRequest,
        config: &ExternalPoolsConfig,
    ) -> Result<PreparedExternalForwardRequest, ExternalForwardError> {
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
        let outbound_body = prepared.body.clone();
        let known_tool_names = external_route_known_tool_names(route);
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
        let response_body_timeout_secs = if config.external_pool_request_timeout_secs == 0 {
            EXTERNAL_POOL_DEFAULT_RESPONSE_BODY_TIMEOUT_SECS
        } else {
            config.external_pool_request_timeout_secs
        };
        Ok(PreparedExternalForwardRequest {
            request,
            outbound_model,
            outbound_body,
            known_tool_names,
            response_body_timeout_secs,
        })
    }

    #[cfg(test)]
    fn set_dispatch_after_prepare_gate(&self, gate: Arc<TestDispatchAfterPrepareGate>) {
        *self.dispatch_after_prepare_gate.lock() = Some(gate);
    }

    #[cfg(test)]
    async fn wait_at_dispatch_after_prepare_gate(&self) {
        let gate = self.dispatch_after_prepare_gate.lock().take();
        if let Some(gate) = gate {
            gate.prepared.wait().await;
            gate.resume.wait().await;
        }
    }

    async fn forward_prepared_once(
        &self,
        pool: &ExternalPool,
        route: &ExternalRouteRequest,
        lease: ExternalPoolLease,
        config: &ExternalPoolsConfig,
        prepared: PreparedExternalForwardRequest,
    ) -> Result<ExternalForwardResponse, ExternalForwardError> {
        let PreparedExternalForwardRequest {
            request,
            outbound_model,
            outbound_body,
            known_tool_names,
            response_body_timeout_secs,
        } = prepared;
        if let Err(rejection) = route
            .inference_attempt_budget
            .reserve(InferenceAttemptKind::ExternalPool, 0)
        {
            tracing::warn!(
                request_id = %route.request_id,
                error_id = %route.error_id,
                pool_id = pool.id,
                rejection = ?rejection,
                "shared inference attempt policy rejected external upstream send"
            );
            return Err(ExternalForwardError::dispatch_rejected(
                ExternalPoolError {
                    status: Some(StatusCode::SERVICE_UNAVAILABLE),
                    message: "inference request dispatch was not available".to_string(),
                    retryable: false,
                    auto_disable_reason: None,
                    cooldown: None,
                    protocol_error: None,
                },
                outbound_model,
                rejection,
            ));
        }
        let response = tokio::select! {
            response = request.send() => response.map_err(|err| {
                tracing::warn!(
                    request_id = %route.request_id,
                    error_id = %route.error_id,
                    pool_id = pool.id,
                    error_class = %sanitized_external_network_error("request send failed", &err),
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
                        protocol_error: None,
                    },
                    outbound_model.clone(),
                )
            })?,
            _ = lease.wait_until_lost() => {
                return Err(external_pool_lease_lost_forward_error(outbound_model.clone()));
            }
        };

        let status = response.status();
        if !status.is_success() {
            let headers = response.headers().clone();
            let body = tokio::select! {
                body = response_bytes_with_limit_and_body_timeout(
                    response,
                    response_body_timeout_secs,
                    EXTERNAL_POOL_ERROR_RESPONSE_MAX_BYTES,
                ) => body.map_err(|err| external_response_body_read_error(
                    err,
                    status,
                    EXTERNAL_POOL_ERROR_RESPONSE_MAX_BYTES,
                    outbound_model.clone(),
                ))?,
                _ = lease.wait_until_lost() => {
                    return Err(external_pool_lease_lost_forward_error(outbound_model.clone()));
                }
            };
            return Err(ExternalForwardError::new(
                classify_external_error(status, body, headers, config),
                outbound_model.clone(),
            ));
        }
        let response_headers = response.headers().clone();
        let status = response.status();
        let response_is_stream = route.is_stream();
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
            let mut body_stream = response.bytes_stream();
            let stream_plan = ExternalStreamProcessingPlan::for_pool(pool, config);
            let projection_context = build_external_usage_projection_context(
                route,
                pool,
                config.external_pool_usage_projection_uplift_percent,
                config.external_pool_usage_projection_cost_floor_enabled,
                config.external_pool_usage_projection_cost_floor_margin_percent,
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
            let transcript_state = Arc::new(SyncMutex::new(
                ExternalAnthropicTranscriptState::new_with_error_context(
                    known_tool_names.iter().cloned(),
                    Some(route.request_id.clone()),
                    Some(route.error_id.clone()),
                ),
            ));
            let mut prefix = Vec::<u8>::new();
            let mut buffer = Vec::<u8>::new();
            let mut last_chunk_at = Instant::now();
            if effective_external_pool_pre_output_stream_retry_enabled(pool, config) {
                last_chunk_at = match pre_read_external_stream_before_commit(
                    &mut body_stream,
                    &mut buffer,
                    &mut prefix,
                    &lease,
                    route,
                    pool,
                    config,
                    projection_context.as_ref(),
                    &usage_capture,
                    stream_plan,
                    stream_error_mask.as_ref(),
                    &transcript_state,
                    stream_idle_timeout,
                    &outbound_model,
                )
                .await
                {
                    Ok(last_chunk_at) => last_chunk_at,
                    Err(err) => {
                        drop(lease);
                        return Err(err);
                    }
                };
            }
            let stream = futures::stream::unfold(
                (
                    body_stream,
                    (!prefix.is_empty()).then_some(prefix),
                    buffer,
                    Some(lease),
                    last_chunk_at,
                    false,
                ),
                move |(
                    mut body_stream,
                    mut prefix,
                    mut buffer,
                    lease,
                    mut last_chunk_at,
                    finished,
                )| {
                    let projection_context = projection_context.clone();
                    let usage_capture = usage_capture.clone();
                    let latency_trace = latency_trace.clone();
                    let stream_error_mask = stream_error_mask.clone();
                    let transcript_state = transcript_state.clone();
                    async move {
                        if finished {
                            return None;
                        }
                        if let Some(prefix_chunk) = prefix.take().filter(|chunk| !chunk.is_empty())
                        {
                            return Some((
                                Ok(Bytes::from(prefix_chunk)),
                                (body_stream, None, buffer, lease, last_chunk_at, false),
                            ));
                        }
                        loop {
                            let projected = drain_sse_events_with_transcript(
                                &mut buffer,
                                projection_context.as_ref(),
                                Some(&usage_capture),
                                Some(stream_error_mask.as_ref()),
                                stream_plan,
                                Some(&mut transcript_state.lock()),
                            );
                            if !projected.is_empty() {
                                return Some((
                                    Ok(Bytes::from(projected)),
                                    (body_stream, None, buffer, lease, last_chunk_at, false),
                                ));
                            }
                            tokio::select! {
                                chunk = body_stream.next() => {
                                    match chunk {
                                        Some(Ok(chunk)) => {
                                            latency_trace
                                                .mark_first_upstream_chunk(route_started_at);
                                            last_chunk_at = Instant::now();
                                            buffer.extend_from_slice(&chunk);
                                            let projected = drain_sse_events_with_transcript(
                                                &mut buffer,
                                                projection_context.as_ref(),
                                                Some(&usage_capture),
                                                Some(stream_error_mask.as_ref()),
                                                stream_plan,
                                                Some(&mut transcript_state.lock()),
                                            );
                                            if !projected.is_empty() {
                                                return Some((
                                                    Ok(Bytes::from(projected)),
                                                    (
                                                        body_stream,
                                                        None,
                                                        buffer,
                                                        lease,
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
                                                        None,
                                                        Vec::new(),
                                                        None,
                                                        last_chunk_at,
                                                        true,
                                                    ),
                                                ));
                                            }
                                        }
                                        Some(Err(_)) => {
                                            tracing::warn!(
                                                request_id = %stream_error_mask.request_id,
                                                error_id = %stream_error_mask.error_id,
                                                pool_id = stream_error_mask.pool_id,
                                                pool_name = %stream_error_mask.pool_name,
                                                error_class = "external_stream_read_error",
                                                "external pool stream read failed"
                                            );
                                            drop(lease);
                                            return Some((
                                                Err(std::io::Error::other(
                                                    "external stream read error".to_string(),
                                                )),
                                                (
                                                    body_stream,
                                                    None,
                                                    Vec::new(),
                                                    None,
                                                    last_chunk_at,
                                                    true,
                                                ),
                                            ));
                                        }
                                        None => {
                                            let mut tail = if buffer.is_empty() {
                                                Vec::new()
                                            } else {
                                                process_sse_event_with_plan_and_transcript(
                                                    &buffer,
                                                    projection_context.as_ref(),
                                                    Some(&usage_capture),
                                                    Some(stream_error_mask.as_ref()),
                                                    stream_plan,
                                                    Some(&mut transcript_state.lock()),
                                                )
                                            };
                                            {
                                                let mut transcript_state = transcript_state.lock();
                                                tail.extend(finish_external_transcript_state(
                                                    &mut transcript_state,
                                                    Some(&usage_capture),
                                                ));
                                            }
                                            drop(lease);
                                            if tail.is_empty() {
                                                return None;
                                            }
                                            return Some((
                                                Ok(Bytes::from(tail)),
                                                (
                                                    body_stream,
                                                    None,
                                                    Vec::new(),
                                                    None,
                                                    last_chunk_at,
                                                    true,
                                                ),
                                            ));
                                        }
                                    }
                                }
                                _ = external_pool_lease_lost(lease.as_ref()) => {
                                    drop(lease);
                                    return Some((
                                        Err(std::io::Error::other(
                                            "external pool lease coordination was lost".to_string(),
                                        )),
                                        (
                                            body_stream,
                                            None,
                                            Vec::new(),
                                            None,
                                            last_chunk_at,
                                            true,
                                        ),
                                    ));
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
                                            None,
                                            Vec::new(),
                                            None,
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
                        protocol_error: None,
                    },
                    outbound_model.clone(),
                )
            })?;
            Ok(ExternalForwardResponse {
                response,
                outbound_model,
                outbound_body,
                billing: None,
                stream_usage_capture: Some(stream_usage_capture),
                stream_usage_projection,
            })
        } else {
            let bytes = tokio::select! {
                body = response_bytes_with_limit_and_body_timeout(
                    response,
                    response_body_timeout_secs,
                    EXTERNAL_POOL_NON_STREAM_RESPONSE_MAX_BYTES,
                ) => body.map_err(|err| external_response_body_read_error(
                    err,
                    status,
                    EXTERNAL_POOL_NON_STREAM_RESPONSE_MAX_BYTES,
                    outbound_model.clone(),
                ))?,
                _ = lease.wait_until_lost() => {
                    return Err(external_pool_lease_lost_forward_error(outbound_model.clone()));
                }
            };
            if response_headers_look_like_sse(&response_headers)
                && serde_json::from_slice::<serde_json::Value>(&bytes).is_err()
            {
                return Err(ExternalForwardError::new(
                    success_protocol_error(
                        &response_headers,
                        Some(&bytes),
                        config,
                        "model endpoint returned an SSE response for a non-streaming request",
                    ),
                    outbound_model.clone(),
                ));
            }
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
                config.external_pool_usage_projection_cost_floor_enabled,
                config.external_pool_usage_projection_cost_floor_margin_percent,
                config.external_pool_usage_projection_output_uplift_min_tokens,
                config.external_pool_usage_projection_output_uplift_percent,
            );
            let upstream_body = bytes.clone();
            let projected = process_non_stream_response_usage(
                bytes,
                Some(route),
                projection_context.as_ref(),
                known_tool_names.iter().cloned(),
            );
            external_pool_usage_debug_non_stream_record(ExternalUsageDebugNonStreamRecordContext {
                config,
                route,
                pool,
                outbound_model: outbound_model.as_deref(),
                status,
                response_headers: &response_headers,
                outbound_body: &outbound_body,
                upstream_body: &upstream_body,
                projected: &projected,
            });
            if projected.protocol_contamination {
                return Err(ExternalForwardError::new(
                    external_protocol_contamination_error(config),
                    outbound_model.clone(),
                ));
            }
            let billing = external_pool_billing_from_capture(route, pool, projected.usage_capture);
            let mut builder = Response::builder().status(status);
            apply_forwarded_response_headers(&mut builder, &response_headers, &route.request_id);
            let upstream_declared_sse = response_headers_look_like_sse(&response_headers);
            let downstream_body = projected.body;
            let mut response =
                builder
                    .body(Body::from(downstream_body.clone()))
                    .map_err(|err| {
                        ExternalForwardError::new(
                            ExternalPoolError {
                                status: None,
                                message: format!("build external response failed: {}", err),
                                retryable: false,
                                auto_disable_reason: None,
                                cooldown: None,
                                protocol_error: None,
                            },
                            outbound_model.clone(),
                        )
                    })?;
            if upstream_declared_sse
                && serde_json::from_slice::<serde_json::Value>(&downstream_body).is_ok()
            {
                response.headers_mut().insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );
            }
            Ok(ExternalForwardResponse {
                response,
                outbound_model,
                outbound_body,
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
        self.scan_pool_availability_uncached(excluded, config, true, None, None, None)
            .await
            .selected_pool
    }

    async fn select_pool_for_route_from_snapshot(
        &self,
        authoritative_pools: &Arc<Vec<ExternalPool>>,
        excluded: &HashSet<u64>,
        config: &ExternalPoolsConfig,
        route: &ExternalRouteRequest,
    ) -> PoolSelectionSnapshot {
        let support_candidates = normalize_external_pool_support_candidates(
            route.model_candidates_for_support().into_iter().flatten(),
        );
        let cooldown_candidates = route.model_cooldown_candidates();
        self.scan_pool_availability_from_snapshot(
            authoritative_pools,
            excluded,
            config,
            true,
            None,
            Some(&support_candidates),
            Some(&cooldown_candidates),
            Some(&route.endpoint),
        )
        .await
    }

    fn select_degraded_fallback_local_pool_from_snapshot(
        &self,
        authoritative_pools: &Arc<Vec<ExternalPool>>,
        excluded: &HashSet<u64>,
        route: &ExternalRouteRequest,
        config: &ExternalPoolsConfig,
    ) -> Option<PoolSelectionSnapshot> {
        let support_candidates = normalize_external_pool_support_candidates(
            route.model_candidates_for_support().into_iter().flatten(),
        );
        let candidates = authoritative_pools
            .iter()
            .filter(|pool| {
                !excluded.contains(&pool.id)
                    && pool.enabled
                    && !pool.is_auto_disabled_now()
                    && external_pool_route_allowed(pool, Some(&route.endpoint))
                    && external_pool_matches_supported_models_normalized(
                        pool,
                        Some(&support_candidates),
                    )
            })
            .cloned()
            .map(|pool| (pool, 0, 0))
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return None;
        }
        let eligible_pools = candidates.len();
        let selected_pool = select_external_pool_candidate(candidates, config)?;
        Some(PoolSelectionSnapshot {
            selected_pool: Some(selected_pool),
            availability: PoolAvailabilitySnapshot {
                eligible_pools,
                available_pools: 1,
                ..PoolAvailabilitySnapshot::default()
            },
            degraded_fallback_local_lease: true,
        })
    }

    #[cfg(test)]
    async fn select_pool_for_route(
        &self,
        excluded: &HashSet<u64>,
        config: &ExternalPoolsConfig,
        route: &ExternalRouteRequest,
    ) -> PoolSelectionSnapshot {
        if !config.external_pools_enabled {
            return PoolSelectionSnapshot::default();
        }
        let authoritative_pools = match self.load_authoritative_pool_snapshot().await {
            Ok(pools) => pools,
            Err(unavailable) => return selection_unavailable_snapshot(unavailable),
        };
        self.select_pool_for_route_from_snapshot(&authoritative_pools, excluded, config, route)
            .await
    }

    #[cfg(test)]
    async fn scan_pool_availability_uncached(
        &self,
        excluded: &HashSet<u64>,
        config: &ExternalPoolsConfig,
        include_selection: bool,
        body_mode_filter: Option<ExternalPoolRequestBodyMode>,
        model_candidates: Option<&[Option<&str>]>,
        model_cooldown_candidates: Option<&[String]>,
    ) -> PoolSelectionSnapshot {
        if !config.external_pools_enabled {
            return PoolSelectionSnapshot::default();
        }
        let authoritative_pools = match self.load_authoritative_pool_snapshot().await {
            Ok(pools) => pools,
            Err(unavailable) => return selection_unavailable_snapshot(unavailable),
        };
        let normalized_candidates = model_candidates.map(|candidates| {
            normalize_external_pool_support_candidates(candidates.iter().flatten().copied())
        });
        self.scan_pool_availability_from_snapshot(
            &authoritative_pools,
            excluded,
            config,
            include_selection,
            body_mode_filter,
            normalized_candidates.as_deref(),
            model_cooldown_candidates,
            None,
        )
        .await
    }

    async fn scan_pool_availability_from_snapshot(
        &self,
        authoritative_pools: &Arc<Vec<ExternalPool>>,
        excluded: &HashSet<u64>,
        config: &ExternalPoolsConfig,
        include_selection: bool,
        body_mode_filter: Option<ExternalPoolRequestBodyMode>,
        model_candidates: Option<&[String]>,
        model_cooldown_candidates: Option<&[String]>,
        endpoint: Option<&str>,
    ) -> PoolSelectionSnapshot {
        let _ = body_mode_filter;
        if !config.external_pools_enabled {
            return PoolSelectionSnapshot::default();
        }
        let pools = authoritative_pools
            .iter()
            .filter(|pool| {
                !excluded.contains(&pool.id)
                    && pool.enabled
                    && !pool.is_auto_disabled_now()
                    && external_pool_route_allowed(pool, endpoint)
                    && external_pool_matches_supported_models_normalized(pool, model_candidates)
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut availability = PoolAvailabilitySnapshot {
            eligible_pools: pools.len(),
            ..PoolAvailabilitySnapshot::default()
        };
        if pools.is_empty() {
            return PoolSelectionSnapshot {
                selected_pool: None,
                availability,
                degraded_fallback_local_lease: false,
            };
        }
        let requested_model_cooldowns: &[String] = if config
            .external_pool_model_unavailable_cooldown_mode
            == ExternalPoolModelUnavailableCooldownMode::Model
        {
            model_cooldown_candidates.unwrap_or(&[])
        } else {
            &[]
        };
        let pool_ids = pools.iter().map(|pool| pool.id).collect::<Vec<_>>();
        let runtimes = match self
            .load_selection_runtime_snapshots(&pool_ids, requested_model_cooldowns)
            .await
        {
            Ok(runtimes) => runtimes,
            Err(err) => {
                let recovery_wait = err
                    .downcast_ref::<ExternalPoolCoordinatorGuardError>()
                    .and_then(|guard| match &guard.state {
                        ExternalPoolCoordinatorGuardState::Recovering { remaining, .. } => {
                            Some(*remaining)
                        }
                        _ => None,
                    });
                tracing::debug!(
                    pool_count = pools.len(),
                    error = %err,
                    "批量读取外部池 Redis 调度协调状态失败，暂停外部池准入"
                );
                availability.temporary_unavailable_pools = pools.len();
                availability.coordinator_unavailable = true;
                availability.coordinator_unavailable_kind =
                    Some(PoolCoordinatorUnavailableKind::RedisError);
                availability.wait_reason = Some(PoolCapacityWaitReason::CoordinatorUnavailable);
                availability.wait_for = recovery_wait;
                return PoolSelectionSnapshot {
                    selected_pool: None,
                    availability,
                    degraded_fallback_local_lease: false,
                };
            }
        };
        self.scan_pool_availability_from_filtered_pools_and_runtimes(
            pools,
            runtimes,
            config,
            include_selection,
        )
    }

    fn scan_pool_availability_from_filtered_pools_and_runtimes(
        &self,
        pools: Vec<ExternalPool>,
        runtimes: Vec<anyhow::Result<PoolRuntimeSnapshot>>,
        config: &ExternalPoolsConfig,
        include_selection: bool,
    ) -> PoolSelectionSnapshot {
        let mut candidates = include_selection.then(Vec::new);
        let mut availability = PoolAvailabilitySnapshot {
            eligible_pools: pools.len(),
            ..PoolAvailabilitySnapshot::default()
        };
        for (pool, runtime) in pools.into_iter().zip(runtimes) {
            let runtime = match runtime {
                Ok(runtime) => runtime,
                Err(err) => {
                    tracing::debug!(
                        pool_id = pool.id,
                        error = %err,
                        "外部池 Redis cooldown 状态无效，仅隔离当前池"
                    );
                    availability.invalid_runtime_pools += 1;
                    availability.temporary_unavailable_pools += 1;
                    continue;
                }
            };
            let in_flight = runtime.in_flight;
            let global_in_flight = runtime.global_in_flight;
            let mut cooldown_remaining_secs = runtime.pool_cooldown_remaining_secs;
            let mut cooldown_reason = runtime.pool_cooldown_reason;
            let mut cooldown_scope =
                (cooldown_remaining_secs > 0).then_some(PoolCooldownScope::Pool);
            if cooldown_remaining_secs == 0 {
                if let Some((model_remaining_secs, model_reason)) = runtime.model_cooldown {
                    cooldown_remaining_secs = model_remaining_secs;
                    cooldown_reason = model_reason;
                    cooldown_scope = Some(PoolCooldownScope::Model);
                }
            }
            // Soft failures are a short-lived health signal, not a hard filter.
            // They must affect ranking even when no pool cooldown exists so a
            // healthy lower-priority pool can take over during upstream jitter.
            let transient_failure_streak = runtime.transient_failure_streak;
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
                        candidates.push((pool, in_flight, transient_failure_streak));
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
                    availability.mark_cooldown(
                        cooldown_remaining_secs,
                        cooldown_reason,
                        cooldown_scope.unwrap_or(PoolCooldownScope::Pool),
                    );
                }
                _ => {}
            }
        }
        if availability.available_pools == 0 && availability.invalid_runtime_pools > 0 {
            availability.coordinator_unavailable = true;
            availability.coordinator_unavailable_kind =
                Some(PoolCoordinatorUnavailableKind::RedisError);
            availability.wait_reason = Some(PoolCapacityWaitReason::CoordinatorUnavailable);
        }
        let selected_pool = if availability.coordinator_unavailable {
            None
        } else {
            candidates.and_then(|candidates| select_external_pool_candidate(candidates, config))
        };
        PoolSelectionSnapshot {
            selected_pool,
            availability,
            degraded_fallback_local_lease: false,
        }
    }

    #[cfg(test)]
    async fn pool_availability_snapshot(
        &self,
        excluded: &HashSet<u64>,
        config: &ExternalPoolsConfig,
    ) -> PoolAvailabilitySnapshot {
        self.pool_availability_snapshot_inner(excluded, config, true)
            .await
    }

    #[cfg(test)]
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
        let cache_key = "all";
        let now = Instant::now();
        let cached_snapshot = cacheable
            .then(|| {
                self.availability_cache
                    .lock()
                    .as_ref()
                    .filter(|cached| cached.cache_key == cache_key && cached.expires_at > now)
                    .map(|cached| cached.snapshot.clone())
            })
            .flatten();
        if let Some(snapshot) = cached_snapshot {
            return snapshot;
        }

        let snapshot = self
            .scan_pool_availability_uncached(excluded, config, false, None, None, None)
            .await
            .availability;
        if cacheable {
            *self.availability_cache.lock() = Some(CachedPoolAvailabilitySnapshot {
                cache_key: cache_key.to_string(),
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
        context: PoolCapacityWaitContext,
        queue_guard: &mut Option<ExternalPoolQueueGuard>,
        wait_started_at: &mut Option<Instant>,
        capacity_waiter: &mut CapacityWaiter,
        dispatch_deadline: Option<Instant>,
    ) -> ExternalCapacityDecision {
        let reason = context.reason;
        let wait_for = context.wait_for;
        if reason == PoolCapacityWaitReason::CoordinatorUnavailable
            || context.is_model_unavailable()
            || config.external_pool_capacity_mode != ExternalPoolCapacityMode::Wait
        {
            let (error_type, message) = external_capacity_error(reason);
            self.record_external_failure(
                route,
                None,
                attempts,
                error_type,
                message,
                synthetic_external_capacity_error_diagnostics(
                    route,
                    StatusCode::SERVICE_UNAVAILABLE,
                    "external_dispatch",
                    config,
                    Some(&context),
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
        let configured_wait = Duration::from_secs(config.effective_dispatch_max_wait_secs());
        let max_wait = dispatch_deadline
            .map(|deadline| configured_wait.min(deadline.saturating_duration_since(Instant::now())))
            .unwrap_or(configured_wait);
        if max_wait.is_zero() {
            let message = "Request dispatch deadline exceeded";
            self.record_external_failure(
                route,
                None,
                attempts,
                "external_pool_deadline_exceeded",
                message,
                synthetic_external_capacity_error_diagnostics(
                    route,
                    StatusCode::SERVICE_UNAVAILABLE,
                    "external_dispatch",
                    config,
                    Some(&context),
                ),
            );
            return ExternalCapacityDecision::FinalError(external_capacity_final_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "external_pool_deadline_exceeded",
                message,
                &route.error_id,
            ));
        }
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
                synthetic_external_capacity_error_diagnostics(
                    route,
                    StatusCode::SERVICE_UNAVAILABLE,
                    "external_dispatch",
                    config,
                    Some(&context),
                ),
            );
            return ExternalCapacityDecision::FinalError(external_capacity_final_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "external_pool_wait_timeout",
                message,
                &route.error_id,
            ));
        }

        if queue_guard.is_none() {
            match self
                .enter_external_pool_queue(config.external_pool_max_queued_requests, max_wait)
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
                        synthetic_external_capacity_error_diagnostics(
                            route,
                            StatusCode::SERVICE_UNAVAILABLE,
                            "external_dispatch",
                            config,
                            Some(&context),
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
                        synthetic_external_capacity_error_diagnostics(
                            route,
                            StatusCode::SERVICE_UNAVAILABLE,
                            "external_dispatch",
                            config,
                            Some(&context),
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
        let queue_renew_error = match queue_guard.as_mut() {
            Some(guard) => guard.renew_if_needed().await.err(),
            None => None,
        };
        if let Some(err) = queue_renew_error {
            tracing::warn!(error = %err, "续期外部池 Redis 调度排队 lease 失败");
            let message = "Request dispatch queue unavailable: Redis coordination unavailable";
            self.record_external_failure(
                route,
                None,
                attempts,
                "external_pool_queue_error",
                message,
                synthetic_external_capacity_error_diagnostics(
                    route,
                    StatusCode::SERVICE_UNAVAILABLE,
                    "external_dispatch",
                    config,
                    Some(&context),
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
                synthetic_external_capacity_error_diagnostics(
                    route,
                    StatusCode::SERVICE_UNAVAILABLE,
                    "external_dispatch",
                    config,
                    Some(&context),
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
        if !wakeup.is_zero() {
            let _ = timeout(wakeup, capacity_waiter.wait_for_change()).await;
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

    async fn acquire_pool_for_route(
        &self,
        pool: &ExternalPool,
        config: &ExternalPoolsConfig,
        route: &ExternalRouteRequest,
    ) -> PoolAcquireResult {
        let model_cooldown_candidates = route.model_cooldown_candidates();
        self.acquire_pool_with_model_cooldowns(pool, config, &model_cooldown_candidates)
            .await
    }

    fn acquire_degraded_fallback_local_pool(&self, pool: &ExternalPool) -> PoolAcquireResult {
        let max_concurrent = pool.max_concurrent_requests.max(1);
        let limiter = {
            let mut limiters = self.degraded_fallback_local_leases.lock();
            limiters
                .entry((pool.id, max_concurrent))
                .or_insert_with(|| Arc::new(Semaphore::new(max_concurrent as usize)))
                .clone()
        };
        match limiter.try_acquire_owned() {
            Ok(permit) => PoolAcquireResult::Acquired(ExternalPoolLease::degraded_fallback_local(
                self.clone(),
                pool.id,
                uuid::Uuid::new_v4().to_string(),
                permit,
            )),
            Err(_) => PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                reason: PoolCapacityWaitReason::Full,
                wait_for: None,
                exclude_pool_for_reselect: true,
                detail: "degraded_fallback_local_capacity_full",
            }),
        }
    }

    fn validate_pool_dispatch_fence(
        &self,
        pool: &ExternalPool,
    ) -> impl Future<Output = PoolDispatchFenceResult> + Send + 'static {
        let key = (pool.id, pool.revision);
        let (flight, start_query) = {
            let mut flights = self.dispatch_fence_flights.lock();
            match flights.get(&key) {
                Some(flight) => (flight.clone(), false),
                None => {
                    let flight = Arc::new(PoolDispatchFenceFlight::new());
                    flights.insert(key, flight.clone());
                    (flight, true)
                }
            }
        };
        if start_query {
            let manager = self.clone();
            let query_flight = flight.clone();
            tokio::spawn(async move {
                tokio::task::yield_now().await;
                let result = manager.query_pool_dispatch_fence(key.0, key.1).await;
                {
                    let mut flights = manager.dispatch_fence_flights.lock();
                    if flights
                        .get(&key)
                        .is_some_and(|current| Arc::ptr_eq(current, &query_flight))
                    {
                        flights.remove(&key);
                    }
                }
                query_flight.complete(result);
            });
        }
        async move {
            match timeout(
                EXTERNAL_POOL_DISPATCH_FENCE_SHARED_WAIT_TIMEOUT,
                flight.wait(),
            )
            .await
            {
                Ok(result) => result,
                Err(_) => PoolDispatchFenceResult::CoordinatorUnavailable(
                    ExternalPoolSelectionUnavailable {
                        kind: ExternalPoolSelectionFailureKind::PostgresTimeout,
                        retry_after: EXTERNAL_POOL_SELECTION_RETRY_AFTER,
                    },
                ),
            }
        }
    }

    async fn query_pool_dispatch_fence(
        &self,
        pool_id: u64,
        revision: u64,
    ) -> PoolDispatchFenceResult {
        let permit = match self.selection_breaker.try_begin() {
            Ok(permit) => permit,
            Err(unavailable) => {
                if unavailable.kind == ExternalPoolSelectionFailureKind::AdmissionSaturated {
                    self.selection_saturated.fetch_add(1, Ordering::Relaxed);
                }
                return PoolDispatchFenceResult::CoordinatorUnavailable(unavailable);
            }
        };
        #[cfg(test)]
        self.dispatch_fence_pg_loads.fetch_add(1, Ordering::Relaxed);
        match timeout(
            EXTERNAL_POOL_DISPATCH_FENCE_TIMEOUT,
            self.postgres
                .external_pool_dispatch_revision_matches(pool_id, revision),
        )
        .await
        {
            Ok(Ok(true)) => {
                permit.success();
                PoolDispatchFenceResult::Current
            }
            Ok(Ok(false)) => {
                permit.success();
                PoolDispatchFenceResult::Changed
            }
            Ok(Err(_)) => {
                permit.failure(ExternalPoolSelectionFailureKind::PostgresError);
                PoolDispatchFenceResult::CoordinatorUnavailable(ExternalPoolSelectionUnavailable {
                    kind: ExternalPoolSelectionFailureKind::PostgresError,
                    retry_after: EXTERNAL_POOL_SELECTION_RETRY_AFTER,
                })
            }
            Err(_) => {
                permit.failure(ExternalPoolSelectionFailureKind::PostgresTimeout);
                PoolDispatchFenceResult::CoordinatorUnavailable(ExternalPoolSelectionUnavailable {
                    kind: ExternalPoolSelectionFailureKind::PostgresTimeout,
                    retry_after: EXTERNAL_POOL_SELECTION_RETRY_AFTER,
                })
            }
        }
    }

    #[cfg(test)]
    fn dispatch_fence_pg_loads_for_test(&self) -> u64 {
        self.dispatch_fence_pg_loads.load(Ordering::Acquire)
    }

    #[cfg(test)]
    async fn acquire_pool(
        &self,
        pool: &ExternalPool,
        config: &ExternalPoolsConfig,
    ) -> PoolAcquireResult {
        self.acquire_pool_with_model_cooldowns(pool, config, &[])
            .await
    }

    async fn acquire_pool_with_model_cooldowns(
        &self,
        pool: &ExternalPool,
        config: &ExternalPoolsConfig,
        model_cooldown_candidates: &[String],
    ) -> PoolAcquireResult {
        self.acquire_pool_with_model_cooldowns_and_max_age(
            pool,
            config,
            model_cooldown_candidates,
            Duration::from_secs(EXTERNAL_POOL_LEASE_MAX_AGE_SECS),
        )
        .await
    }

    async fn acquire_pool_with_model_cooldowns_and_max_age(
        &self,
        pool: &ExternalPool,
        config: &ExternalPoolsConfig,
        model_cooldown_candidates: &[String],
        max_age: Duration,
    ) -> PoolAcquireResult {
        self.acquire_pool_with_model_cooldowns_and_max_age_inner(
            pool,
            config,
            model_cooldown_candidates,
            max_age,
            0,
        )
        .await
    }

    async fn acquire_pool_with_model_cooldowns_and_max_age_inner(
        &self,
        pool: &ExternalPool,
        config: &ExternalPoolsConfig,
        model_cooldown_candidates: &[String],
        max_age: Duration,
        epoch_reconciliations: usize,
    ) -> PoolAcquireResult {
        let Some(release_permit) = self.release_dispatcher.try_reserve() else {
            return PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                reason: PoolCapacityWaitReason::CoordinatorUnavailable,
                wait_for: Some(Duration::from_millis(50)),
                exclude_pool_for_reselect: false,
                detail: "release_backlog_saturated",
            });
        };
        let coordination_epoch = match self.external_pool_coordination_epoch().await {
            Ok(epoch) => epoch,
            Err(err) => {
                self.mark_external_pool_coordinator_probe_required();
                tracing::warn!(pool_id = pool.id, error = %err, "外部池 Redis 协调 epoch 暂不可用");
                return PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                    reason: PoolCapacityWaitReason::CoordinatorUnavailable,
                    wait_for: None,
                    exclude_pool_for_reselect: false,
                    detail: "coordinator_epoch_unavailable",
                });
            }
        };
        let mut cooldown_keys = vec![format!("external_pool:{}:cooldown", pool.id)];
        if config.external_pool_model_unavailable_cooldown_mode
            == ExternalPoolModelUnavailableCooldownMode::Model
        {
            cooldown_keys.extend(
                model_cooldown_candidates
                    .iter()
                    .map(|model| external_pool_model_cooldown_key(pool.id, model)),
            );
        }
        let coordinator_permit = match self.coordinator_breaker.try_begin() {
            Ok(permit) => permit,
            Err(open) => {
                return PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                    reason: PoolCapacityWaitReason::CoordinatorUnavailable,
                    wait_for: Some(open.retry_after),
                    exclude_pool_for_reselect: false,
                    detail: "coordinator_breaker_open",
                });
            }
        };
        let lease_id = uuid::Uuid::new_v4().to_string();
        let mut pending_lease = Some(ExternalPoolLease::pending(
            self.clone(),
            pool.id,
            lease_id.clone(),
            coordination_epoch.clone(),
            max_age,
            release_permit,
        ));
        let acquire_result = match timeout(
            EXTERNAL_POOL_COORDINATOR_REDIS_OPERATION_TIMEOUT,
            self.redis.acquire_external_pool_lease(
                pool.id,
                &lease_id,
                &coordination_epoch,
                pool.max_concurrent_requests.max(1),
                config.external_pool_global_max_concurrent_requests,
                Some(max_age),
                &cooldown_keys,
            ),
        )
        .await
        {
            Ok(Ok(result)) => {
                coordinator_permit.success();
                result
            }
            Ok(Err(err)) => {
                self.mark_external_pool_coordinator_probe_required();
                coordinator_permit.failure(ExternalPoolCoordinatorFailureKind::RedisError);
                tracing::debug!(
                    pool_id = pool.id,
                    error = %err,
                    "占用外部池 Redis 并发槽失败"
                );
                return PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                    reason: PoolCapacityWaitReason::CoordinatorUnavailable,
                    wait_for: None,
                    exclude_pool_for_reselect: false,
                    detail: "lease_acquire_error",
                });
            }
            Err(_) => {
                self.mark_external_pool_coordinator_probe_required();
                coordinator_permit.failure(ExternalPoolCoordinatorFailureKind::Timeout);
                return PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                    reason: PoolCapacityWaitReason::CoordinatorUnavailable,
                    wait_for: None,
                    exclude_pool_for_reselect: false,
                    detail: "lease_acquire_timeout",
                });
            }
        };
        match acquire_result {
            RedisExternalPoolLeaseAcquireResult::Acquired {
                lease_id: acquired_lease_id,
                ..
            } if acquired_lease_id == lease_id => {
                let mut lease = pending_lease.take().expect("pending external pool lease");
                lease.confirm();
                PoolAcquireResult::Acquired(lease)
            }
            RedisExternalPoolLeaseAcquireResult::Acquired { .. } => {
                PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                    reason: PoolCapacityWaitReason::CoordinatorUnavailable,
                    wait_for: None,
                    exclude_pool_for_reselect: false,
                    detail: "lease_acquire_id_mismatch",
                })
            }
            RedisExternalPoolLeaseAcquireResult::PoolCooldown { remaining } => {
                pending_lease
                    .take()
                    .expect("pending external pool lease")
                    .disarm();
                PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                    reason: PoolCapacityWaitReason::Cooldown,
                    wait_for: Some(remaining.unwrap_or_else(|| Duration::from_secs(1))),
                    exclude_pool_for_reselect: true,
                    detail: "cooldown_during_atomic_acquire",
                })
            }
            RedisExternalPoolLeaseAcquireResult::ModelCooldown { remaining } => {
                pending_lease
                    .take()
                    .expect("pending external pool lease")
                    .disarm();
                PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                    reason: PoolCapacityWaitReason::ModelUnavailable,
                    wait_for: Some(remaining.unwrap_or_else(|| Duration::from_secs(1))),
                    exclude_pool_for_reselect: true,
                    detail: "model_cooldown_during_atomic_acquire",
                })
            }
            RedisExternalPoolLeaseAcquireResult::PoolCapacityFull { .. } => {
                pending_lease
                    .take()
                    .expect("pending external pool lease")
                    .disarm();
                PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                    reason: PoolCapacityWaitReason::Full,
                    wait_for: None,
                    exclude_pool_for_reselect: true,
                    detail: "pool_concurrency_full_during_atomic_acquire",
                })
            }
            RedisExternalPoolLeaseAcquireResult::GlobalCapacityFull { .. } => {
                pending_lease
                    .take()
                    .expect("pending external pool lease")
                    .disarm();
                PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                    reason: PoolCapacityWaitReason::Full,
                    wait_for: None,
                    exclude_pool_for_reselect: false,
                    detail: "global_concurrency_full_during_atomic_acquire",
                })
            }
            RedisExternalPoolLeaseAcquireResult::Released => {
                pending_lease
                    .take()
                    .expect("pending external pool lease")
                    .disarm();
                PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                    reason: PoolCapacityWaitReason::CoordinatorUnavailable,
                    wait_for: None,
                    exclude_pool_for_reselect: false,
                    detail: "lease_released_before_acquire",
                })
            }
            RedisExternalPoolLeaseAcquireResult::CoordinatorEpochMismatch { .. } => {
                pending_lease
                    .take()
                    .expect("pending external pool lease")
                    .disarm();
                if epoch_reconciliations >= 3 {
                    return PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                        reason: PoolCapacityWaitReason::CoordinatorUnavailable,
                        wait_for: None,
                        exclude_pool_for_reselect: false,
                        detail: "coordinator_epoch_churn",
                    });
                }
                self.mark_external_pool_coordinator_probe_required();
                Box::pin(self.acquire_pool_with_model_cooldowns_and_max_age_inner(
                    pool,
                    config,
                    model_cooldown_candidates,
                    max_age,
                    epoch_reconciliations + 1,
                ))
                .await
            }
            RedisExternalPoolLeaseAcquireResult::CoordinatorRecovering { remaining, .. } => {
                pending_lease
                    .take()
                    .expect("pending external pool lease")
                    .disarm();
                PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                    reason: PoolCapacityWaitReason::CoordinatorUnavailable,
                    wait_for: Some(remaining),
                    exclude_pool_for_reselect: false,
                    detail: "coordinator_restart_recovery",
                })
            }
        }
    }

    async fn enter_external_pool_queue(
        &self,
        max_queued: u32,
        max_wait: Duration,
    ) -> anyhow::Result<Option<ExternalPoolQueueGuard>> {
        let lease_id = uuid::Uuid::new_v4().to_string();
        let lease_policy = external_pool_queue_lease_policy(Some(max_wait));
        // Arm cleanup before awaiting Redis so cancellation and commit-unknown both remove this ID.
        let guard = ExternalPoolQueueGuard::new(self.clone(), lease_id.clone(), lease_policy);
        let admitted = timeout(
            EXTERNAL_POOL_QUEUE_REDIS_OPERATION_TIMEOUT,
            self.redis.try_enter_external_pool_dispatch_queue(
                &lease_id,
                max_queued,
                lease_policy.ttl_secs,
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

    async fn touch_pool(
        &self,
        pool_id: u64,
        lease_id: &str,
        ttl_secs: usize,
        coordination_epoch: &str,
    ) -> anyhow::Result<bool> {
        let current_epoch = self.external_pool_coordination_epoch().await?;
        if current_epoch != coordination_epoch {
            return Ok(false);
        }
        match self
            .redis
            .touch_external_pool_lease(pool_id, lease_id, ttl_secs, coordination_epoch)
            .await
        {
            Ok(touched) => return Ok(touched),
            Err(err) => {
                self.mark_external_pool_coordinator_probe_required();
                return Err(err);
            }
        }
    }

    async fn record_external_pool_soft_failure(&self, pool_id: u64, reason: &str) {
        let key = external_pool_transient_failure_key(pool_id);
        match timeout(
            EXTERNAL_POOL_AUTO_DISABLE_REDIS_TIMEOUT,
            self.redis
                .incr_with_ttl(&key, EXTERNAL_POOL_TRANSIENT_FAILURE_WINDOW_SECS),
        )
        .await
        {
            Ok(Ok(streak)) => {
                tracing::debug!(
                    pool_id,
                    reason,
                    streak = streak.max(1),
                    window_secs = EXTERNAL_POOL_TRANSIENT_FAILURE_WINDOW_SECS,
                    "recorded external pool soft failure for health-aware selection"
                );
            }
            Ok(Err(err)) => {
                tracing::warn!(
                    pool_id,
                    reason,
                    error = %err,
                    "外部池软失败计数写入失败"
                );
            }
            Err(_) => {
                tracing::warn!(
                    pool_id,
                    reason,
                    timeout_ms = EXTERNAL_POOL_AUTO_DISABLE_REDIS_TIMEOUT.as_millis(),
                    "外部池软失败计数写入超时"
                );
            }
        }
        self.invalidate_external_pool_runtime_capacity_state();
        self.capacity_signal.notify_state_changed();
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
        self.invalidate_external_pool_runtime_capacity_state();
        self.capacity_signal.notify_state_changed();
    }

    async fn mark_pool_model_cooldowns(
        &self,
        pool_id: u64,
        duration: Duration,
        reason: String,
        models: &[String],
    ) {
        if models.is_empty() {
            return;
        }
        let until = Utc::now() + chrono::Duration::from_std(duration).unwrap_or_default();
        let ttl = duration.as_secs().max(1) as usize;
        for model in models {
            let key = external_pool_model_cooldown_key(pool_id, model);
            if let Err(err) = self
                .redis
                .set_json(
                    key,
                    &json!({
                        "until": until.to_rfc3339(),
                        "reason": reason,
                        "model": model,
                    }),
                    ttl,
                )
                .await
            {
                tracing::warn!(
                    pool_id,
                    model = %model,
                    "写入外部池模型级 Redis cooldown 失败: {}",
                    err
                );
            }
        }
        self.invalidate_external_pool_runtime_capacity_state();
        self.capacity_signal.notify_state_changed();
    }

    pub async fn clear_pool_cooldowns(&self, pool_id: u64) -> anyhow::Result<usize> {
        let pool_keys = [
            format!("external_pool:{pool_id}:cooldown"),
            external_pool_transient_failure_key(pool_id),
        ];
        let pool_deleted = self.redis.del_many(&pool_keys).await?;
        let model_deleted = self
            .redis
            .del_pattern(format!("external_pool:{pool_id}:model_cooldown:*"))
            .await?;
        self.invalidate_external_pool_runtime_capacity_state();
        self.capacity_signal.notify_state_changed();
        Ok(pool_deleted.saturating_add(model_deleted))
    }

    #[cfg(test)]
    async fn load_pool_runtime_snapshot(
        &self,
        pool_id: u64,
        models: &[String],
    ) -> anyhow::Result<PoolRuntimeSnapshot> {
        self.load_pool_runtime_snapshots(&[pool_id], models)
            .await?
            .pop()
            .ok_or_else(|| anyhow::anyhow!("Redis returned no external pool runtime snapshot"))
            .and_then(|runtime| runtime)
    }

    async fn load_pool_runtime_snapshots(
        &self,
        pool_ids: &[u64],
        models: &[String],
    ) -> anyhow::Result<Vec<anyhow::Result<PoolRuntimeSnapshot>>> {
        if pool_ids.is_empty() {
            return Ok(Vec::new());
        }
        let requests = pool_ids
            .iter()
            .map(|pool_id| ExternalPoolCoordinatorSnapshotRequest {
                pool_id: *pool_id,
                cooldown_keys: external_pool_cooldown_keys(*pool_id, models),
                transient_failure_key: external_pool_transient_failure_key(*pool_id),
            })
            .collect::<Vec<_>>();
        let mut epoch_reconciliations = 0usize;
        let coordinators = loop {
            let coordinator_permit = self.coordinator_breaker.try_begin().map_err(|open| {
                anyhow::anyhow!(
                    "external pool coordinator breaker is open; retry after {}ms",
                    open.retry_after.as_millis().max(1)
                )
            })?;
            let coordination_epoch = match self.external_pool_coordination_epoch().await {
                Ok(epoch) => epoch,
                Err(err) => {
                    self.mark_external_pool_coordinator_probe_required();
                    coordinator_permit.failure(ExternalPoolCoordinatorFailureKind::RedisError);
                    return Err(err);
                }
            };
            if coordination_epoch.is_empty() {
                coordinator_permit.failure(ExternalPoolCoordinatorFailureKind::RedisError);
                anyhow::bail!("external pool coordinator epoch is empty");
            }
            match timeout(
                EXTERNAL_POOL_COORDINATOR_REDIS_OPERATION_TIMEOUT,
                self.redis.external_pool_coordinator_snapshots(
                    &requests,
                    Some(Duration::from_secs(
                        DEFAULT_EXTERNAL_POOL_REQUEST_TIMEOUT_SECS.saturating_mul(2),
                    )),
                    &coordination_epoch,
                ),
            )
            .await
            {
                Ok(Ok(coordinators)) => {
                    coordinator_permit.success();
                    break coordinators;
                }
                Ok(Err(err)) => {
                    let guard_state = err
                        .downcast_ref::<ExternalPoolCoordinatorGuardError>()
                        .map(|guard| guard.state.clone());
                    if let Some(guard_state) = guard_state {
                        coordinator_permit.success();
                        match guard_state {
                            ExternalPoolCoordinatorGuardState::EpochMismatch { .. }
                                if epoch_reconciliations < 3 =>
                            {
                                epoch_reconciliations += 1;
                                self.mark_external_pool_coordinator_probe_required();
                                continue;
                            }
                            state => {
                                return Err(anyhow::Error::new(
                                    ExternalPoolCoordinatorGuardError { state },
                                ));
                            }
                        }
                    }
                    self.mark_external_pool_coordinator_probe_required();
                    coordinator_permit.failure(ExternalPoolCoordinatorFailureKind::RedisError);
                    return Err(err);
                }
                Err(_) => {
                    self.mark_external_pool_coordinator_probe_required();
                    coordinator_permit.failure(ExternalPoolCoordinatorFailureKind::Timeout);
                    return Err(anyhow::anyhow!(
                        "Redis external pool coordinator snapshot timed out after {}ms",
                        EXTERNAL_POOL_COORDINATOR_REDIS_OPERATION_TIMEOUT.as_millis()
                    ));
                }
            }
        };
        if coordinators.len() != requests.len() {
            anyhow::bail!("Redis returned an incomplete external pool coordinator snapshot batch");
        }
        Ok(requests
            .iter()
            .zip(coordinators)
            .map(|(request, coordinator)| {
                decode_pool_runtime_snapshot(
                    request.pool_id,
                    models,
                    &request.cooldown_keys,
                    coordinator,
                )
            })
            .collect())
    }

    async fn load_selection_runtime_snapshots(
        &self,
        pool_ids: &[u64],
        models: &[String],
    ) -> anyhow::Result<Vec<anyhow::Result<PoolRuntimeSnapshot>>> {
        let key = SelectionRuntimeSnapshotKey {
            pool_ids: pool_ids.to_vec(),
            models: models.to_vec(),
        };
        if let Some(snapshots) = self.cached_selection_runtime_snapshots(&key) {
            return Ok(materialize_selection_runtime_snapshots(snapshots));
        }

        let _refresh = self.selection_runtime_snapshot_refresh_lock.lock().await;
        if let Some(snapshots) = self.cached_selection_runtime_snapshots(&key) {
            return Ok(materialize_selection_runtime_snapshots(snapshots));
        }

        let snapshots = self.load_pool_runtime_snapshots(pool_ids, models).await?;
        let cacheable = snapshots
            .into_iter()
            .map(|snapshot| snapshot.map_err(|err| err.to_string()))
            .collect::<Vec<_>>();
        *self.selection_runtime_snapshot.lock() = Some(CachedSelectionRuntimeSnapshot {
            key,
            snapshots: cacheable.clone(),
            expires_at: Instant::now() + EXTERNAL_POOL_SELECTION_RUNTIME_SNAPSHOT_TTL,
        });
        Ok(materialize_selection_runtime_snapshots(cacheable))
    }

    fn maybe_spawn_selection_runtime_snapshot_refresh(&self, key: SelectionRuntimeSnapshotKey) {
        if self.cached_selection_runtime_snapshots(&key).is_some() {
            return;
        }
        let Ok(refresh_guard) = self
            .selection_runtime_snapshot_refresh_lock
            .clone()
            .try_lock_owned()
        else {
            return;
        };
        let manager = self.clone();
        tokio::spawn(async move {
            let _refresh_guard = refresh_guard;
            if manager.cached_selection_runtime_snapshots(&key).is_some() {
                return;
            }
            let snapshots = match manager
                .load_pool_runtime_snapshots(&key.pool_ids, &key.models)
                .await
            {
                Ok(snapshots) => snapshots,
                Err(err) => {
                    tracing::debug!(
                        pool_count = key.pool_ids.len(),
                        model_count = key.models.len(),
                        error = %err,
                        "外部池运行态后台刷新失败；本地主路径保持本地调度语义"
                    );
                    return;
                }
            };
            let cacheable = snapshots
                .into_iter()
                .map(|snapshot| snapshot.map_err(|err| err.to_string()))
                .collect::<Vec<_>>();
            *manager.selection_runtime_snapshot.lock() = Some(CachedSelectionRuntimeSnapshot {
                key,
                snapshots: cacheable,
                expires_at: Instant::now() + EXTERNAL_POOL_SELECTION_RUNTIME_SNAPSHOT_TTL,
            });
        });
    }

    fn cached_selection_runtime_snapshots(
        &self,
        key: &SelectionRuntimeSnapshotKey,
    ) -> Option<Vec<Result<PoolRuntimeSnapshot, String>>> {
        let now = Instant::now();
        self.selection_runtime_snapshot
            .lock()
            .as_ref()
            .filter(|cached| &cached.key == key && cached.expires_at > now)
            .map(|cached| cached.snapshots.clone())
    }

    #[cfg(test)]
    async fn pool_runtime_snapshot(
        &self,
        pool_id: u64,
    ) -> anyhow::Result<(u32, u32, u64, Option<String>)> {
        let snapshot = self.load_pool_runtime_snapshot(pool_id, &[]).await?;
        Ok((
            snapshot.in_flight,
            snapshot.global_in_flight,
            snapshot.pool_cooldown_remaining_secs,
            snapshot.pool_cooldown_reason,
        ))
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
        let claim = timeout(
            EXTERNAL_POOL_AUTO_DISABLE_REDIS_TIMEOUT,
            self.redis.claim_external_pool_auto_disable_transition(
                pool.id,
                pool.revision,
                reason,
                threshold,
                config.external_pool_auto_disable_window_secs.max(1) as usize,
                EXTERNAL_POOL_AUTO_DISABLE_TRANSITION_CLAIM_TTL_SECS,
            ),
        )
        .await;
        let (count, claimed) = match claim {
            Ok(Ok(claim)) => claim,
            Ok(Err(err)) => {
                tracing::warn!(
                    pool_id = pool.id,
                    pool_revision = pool.revision,
                    reason,
                    error = %err,
                    "外部池自动禁用 Redis 计数/claim 失败，跳过 PostgreSQL 变更"
                );
                return;
            }
            Err(_) => {
                tracing::warn!(
                    pool_id = pool.id,
                    pool_revision = pool.revision,
                    reason,
                    timeout_ms = EXTERNAL_POOL_AUTO_DISABLE_REDIS_TIMEOUT.as_millis(),
                    "外部池自动禁用 Redis 计数/claim 超时，跳过 PostgreSQL 变更"
                );
                return;
            }
        };
        if count < threshold || !claimed {
            return;
        }

        let changed = timeout(
            EXTERNAL_POOL_AUTO_DISABLE_POSTGRES_TIMEOUT,
            self.postgres.auto_disable_external_pool_if_unchanged(
                pool.id,
                pool.revision,
                reason,
                last_error,
                config.external_pool_auto_disable_duration_secs,
            ),
        )
        .await;
        match changed {
            Ok(Ok(true)) => {
                if let Err(err) = self
                    .publish_external_pool_data_changed("auto_disable", Some(pool.id))
                    .await
                {
                    tracing::warn!(
                        pool_id = pool.id,
                        pool_revision = pool.revision,
                        error = %err,
                        "自动禁用外部池已提交，但跨实例失效通知发布失败"
                    );
                }
            }
            Ok(Ok(false)) => {
                tracing::debug!(
                    pool_id = pool.id,
                    pool_revision = pool.revision,
                    reason,
                    "外部池自动禁用条件更新未生效，pool revision 已变化或状态已转换"
                );
            }
            Ok(Err(err)) => {
                tracing::warn!(
                    pool_id = pool.id,
                    pool_revision = pool.revision,
                    error = %err,
                    "自动禁用外部池 PostgreSQL 条件更新失败"
                );
            }
            Err(_) => {
                tracing::warn!(
                    pool_id = pool.id,
                    pool_revision = pool.revision,
                    timeout_ms = EXTERNAL_POOL_AUTO_DISABLE_POSTGRES_TIMEOUT.as_millis(),
                    "自动禁用外部池 PostgreSQL 条件更新超时"
                );
            }
        }
    }

    fn reset_pool_auto_disable_failure_counts(&self, pool_id: u64) {
        let now = Instant::now();
        let mut recent = self.success_reset_recent.lock();
        recent.retain(|_, reset_at| {
            now.saturating_duration_since(*reset_at) < EXTERNAL_POOL_SUCCESS_RESET_COALESCE_WINDOW
        });
        if recent.contains_key(&pool_id) {
            return;
        }
        if recent.len() >= EXTERNAL_POOL_SUCCESS_RESET_MAX_TRACKED_POOLS {
            tracing::warn!(
                pool_id,
                tracked_pools = recent.len(),
                "外部池 success reset 合并索引已达硬上限，跳过本次清理"
            );
            return;
        }
        let Ok(task_permit) = self.success_reset_semaphore.clone().try_acquire_owned() else {
            tracing::warn!(
                pool_id,
                max_tasks = EXTERNAL_POOL_SUCCESS_RESET_MAX_TASKS,
                "外部池 success reset 后台任务已达硬上限，跳过本次清理"
            );
            return;
        };
        recent.insert(pool_id, now);
        drop(recent);

        let redis = self.redis.clone();
        let keys = EXTERNAL_POOL_AUTO_DISABLE_REASONS
            .iter()
            .map(|reason| format!("external_pool:{}:auto_disable_failures:{}", pool_id, reason))
            .collect::<Vec<_>>();
        #[cfg(test)]
        let tasks_in_flight = self.success_reset_tasks_in_flight.clone();
        #[cfg(test)]
        {
            self.success_reset_tasks_started
                .fetch_add(1, Ordering::Relaxed);
            tasks_in_flight.fetch_add(1, Ordering::Relaxed);
        }
        tokio::spawn(async move {
            let result = timeout(
                EXTERNAL_POOL_SUCCESS_RESET_REDIS_TIMEOUT,
                redis.del_many(&keys),
            )
            .await;
            match result {
                Ok(Ok(_)) => {}
                Ok(Err(err)) => tracing::warn!(
                    pool_id,
                    error = %err,
                    "单命令清理外部池 5 个自动禁用失败计数失败"
                ),
                Err(_) => tracing::warn!(
                    pool_id,
                    timeout_ms = EXTERNAL_POOL_SUCCESS_RESET_REDIS_TIMEOUT.as_millis(),
                    "单命令清理外部池 5 个自动禁用失败计数超时"
                ),
            }
            #[cfg(test)]
            tasks_in_flight.fetch_sub(1, Ordering::Relaxed);
            drop(task_permit);
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
        ctx: ExternalStreamUsageRecordContext,
    ) -> Response {
        let ExternalStreamUsageRecordContext {
            config,
            route,
            pool,
            attempts,
            outbound_model,
            outbound_body,
            usage_capture,
            usage_projection,
        } = ctx;
        let (parts, body) = response.into_parts();
        let response_status = parts.status;
        let response_content_type = parts
            .headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let data_stream = body.into_data_stream();
        let guard = ExternalStreamUsageGuard {
            manager: self.clone(),
            config,
            route,
            pool,
            attempts,
            outbound_model,
            outbound_body,
            usage_capture,
            usage_projection,
            response_status,
            response_content_type,
            chunks_before_first_output: 0,
            events_before_first_output: 0,
            estimated_output_tokens: 0,
            completed: false,
        };
        let stream = futures::stream::unfold(
            (data_stream, Some(guard)),
            |(mut data_stream, mut guard)| async move {
                match data_stream.next().await {
                    Some(Ok(chunk)) => {
                        if let Some(guard_ref) = guard.as_mut() {
                            if !chunk.is_empty() {
                                guard_ref
                                    .route
                                    .inference_attempt_budget
                                    .mark_downstream_committed();
                            }
                            guard_ref.mark_first_token_if_output(&chunk);
                        }
                        Some((Ok(chunk), (data_stream, guard)))
                    }
                    Some(Err(_)) => {
                        if let Some(mut guard) = guard.take() {
                            tracing::warn!(
                                request_id = %guard.route.request_id,
                                error_id = %guard.route.error_id,
                                pool_id = guard.pool.id,
                                pool_name = %guard.pool.name,
                                error_class = "external_stream_response_error",
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
        let standard_usage =
            external_standard_usage_for_status(status, request_input_tokens, usage);
        let compat_input_tokens = standard_usage.input_tokens;
        let billable_input_tokens = standard_usage.billable_input_tokens;
        let output_tokens = standard_usage.output_tokens;
        let cache_read_input_tokens = standard_usage.cache_read_input_tokens;
        let cache_creation_input_tokens = standard_usage.cache_creation_input_tokens;
        let cache_creation_5m_input_tokens = standard_usage.cache_creation_5m_input_tokens;
        let cache_creation_1h_input_tokens = standard_usage.cache_creation_1h_input_tokens;
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
        let usage_source = external_record_usage_source(billing.as_ref(), usage.is_some());
        let duration_ms = route.started_at.elapsed().as_millis() as u64;
        let external_outbound_model = attempts
            .iter()
            .rev()
            .find_map(|attempt| attempt.outbound_model.clone());
        let latency_trace = external_usage_latency_trace(route);
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
            request_api_key_id: route.request_api_key_id.clone(),
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
            // External-pool raw cost is either priced from the upstream usage
            // or, when upstream omitted usage, from the explicit local
            // estimate captured in the billing object. Do not replace an
            // unavailable raw price with the shaped/billable estimate: that
            // would make the generic "original cost" field silently change
            // meaning.
            original_cost_usd: if status == UsageRecordStatus::Success {
                billing
                    .as_ref()
                    .map(|billing| billing.raw_cost_usd)
                    .unwrap_or(estimated_cost_usd)
            } else {
                0.0
            },
            kiro_metering_usage: 0.0,
            pricing_available,
            pricing_model,
            duration_ms,
            first_token_latency_ms: {
                let value = route.first_token_latency_ms.load(Ordering::Acquire);
                (value > 0).then_some(value)
            },
            response_latency_ms: Some(duration_ms),
            latency_trace: Some(latency_trace),
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

fn external_standard_usage_for_status(
    status: UsageRecordStatus,
    request_input_tokens: i32,
    usage: Option<ExternalPoolUsageSnapshot>,
) -> ExternalPoolUsageSnapshot {
    if status == UsageRecordStatus::Success {
        return usage.unwrap_or(ExternalPoolUsageSnapshot {
            total_input_tokens: request_input_tokens.max(0),
            input_tokens: request_input_tokens.max(0),
            billable_input_tokens: request_input_tokens.max(0),
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        });
    }

    ExternalPoolUsageSnapshot {
        total_input_tokens: 0,
        input_tokens: 0,
        billable_input_tokens: 0,
        output_tokens: 0,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        cache_creation_5m_input_tokens: 0,
        cache_creation_1h_input_tokens: 0,
    }
}

fn external_record_usage_source(
    billing: Option<&ExternalPoolBilling>,
    usage_present: bool,
) -> UsageSource {
    if billing.is_some_and(|billing| billing.usage_estimated) {
        UsageSource::RequestEstimate
    } else if billing.is_some_and(|billing| billing.usage_projection_applied) {
        UsageSource::LocalPromptCache
    } else if usage_present {
        UsageSource::UpstreamMetadata
    } else {
        UsageSource::RequestEstimate
    }
}

fn external_usage_latency_trace(route: &ExternalRouteRequest) -> UsageLatencyTrace {
    let mut latency_trace = route.latency_trace.snapshot().unwrap_or_default();
    latency_trace.inference_attempts = Some(route.inference_attempt_budget.snapshot());
    latency_trace.auxiliary_attempts = Some(route.inference_attempt_budget.auxiliary_snapshot());
    latency_trace
}

struct ExternalStreamUsageGuard {
    manager: ExternalPoolManager,
    config: ExternalPoolsConfig,
    route: ExternalRouteRequest,
    pool: ExternalPool,
    attempts: Vec<ExternalPoolAttempt>,
    outbound_model: Option<String>,
    outbound_body: Bytes,
    usage_capture: Option<Arc<SyncMutex<ExternalUsageCapture>>>,
    usage_projection: Option<ExternalUsageProjectionContext>,
    response_status: StatusCode,
    response_content_type: Option<String>,
    chunks_before_first_output: u32,
    events_before_first_output: u32,
    estimated_output_tokens: i32,
    completed: bool,
}

impl ExternalStreamUsageGuard {
    fn mark_first_token_if_output(&mut self, chunk: &Bytes) {
        self.estimated_output_tokens = self
            .estimated_output_tokens
            .saturating_add(estimate_external_stream_output_tokens(chunk));
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
            external_pool_usage_debug_stream_record(ExternalUsageDebugStreamRecordContext {
                config: &self.config,
                route: &self.route,
                pool: &self.pool,
                outbound_model: self.outbound_model.as_deref(),
                status: UsageRecordStatus::StreamError,
                response_status: self.response_status,
                response_content_type: self.response_content_type.as_deref(),
                outbound_body: &self.outbound_body,
                capture: self.usage_capture.as_ref(),
                billing: None,
                estimated_output_tokens: self.estimated_output_tokens,
                terminal_message: Some(&message),
            });
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
        let billing = self
            .usage_capture
            .as_ref()
            .and_then(|capture| {
                external_pool_billing_from_capture_ref(&self.route, &self.pool, capture)
            })
            .or_else(|| {
                self.usage_capture.as_ref().and_then(|capture| {
                    external_pool_billing_from_stream_estimate(
                        &self.route,
                        &self.pool,
                        capture,
                        self.usage_projection.as_ref(),
                        self.estimated_output_tokens,
                    )
                })
            });
        if let Some(projection) = self.usage_projection.as_ref() {
            projection.record_success();
        }
        external_pool_usage_debug_stream_record(ExternalUsageDebugStreamRecordContext {
            config: &self.config,
            route: &self.route,
            pool: &self.pool,
            outbound_model: self.outbound_model.as_deref(),
            status: UsageRecordStatus::Success,
            response_status: self.response_status,
            response_content_type: self.response_content_type.as_deref(),
            outbound_body: &self.outbound_body,
            capture: self.usage_capture.as_ref(),
            billing: billing.as_ref(),
            estimated_output_tokens: self.estimated_output_tokens,
            terminal_message: None,
        });
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
        external_pool_usage_debug_stream_record(ExternalUsageDebugStreamRecordContext {
            config: &self.config,
            route: &self.route,
            pool: &self.pool,
            outbound_model: self.outbound_model.as_deref(),
            status: UsageRecordStatus::StreamError,
            response_status: self.response_status,
            response_content_type: self.response_content_type.as_deref(),
            outbound_body: &self.outbound_body,
            capture: self.usage_capture.as_ref(),
            billing: None,
            estimated_output_tokens: self.estimated_output_tokens,
            terminal_message: Some(message),
        });
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
        external_pool_usage_debug_stream_record(ExternalUsageDebugStreamRecordContext {
            config: &self.config,
            route: &self.route,
            pool: &self.pool,
            outbound_model: self.outbound_model.as_deref(),
            status: UsageRecordStatus::ClientDropped,
            response_status: self.response_status,
            response_content_type: self.response_content_type.as_deref(),
            outbound_body: &self.outbound_body,
            capture: self.usage_capture.as_ref(),
            billing: None,
            estimated_output_tokens: self.estimated_output_tokens,
            terminal_message: Some(message),
        });
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

fn estimate_external_stream_output_tokens(chunk: &Bytes) -> i32 {
    let bytes = chunk.as_ref();
    let mut tokens = 0i32;
    let mut offset = 0usize;
    while let Some((idx, delimiter_len)) = find_sse_event_delimiter(&bytes[offset..]) {
        let event_end = offset + idx + delimiter_len;
        if let Ok(event) = std::str::from_utf8(&bytes[offset..event_end]) {
            tokens = tokens.saturating_add(estimate_external_stream_event_output_tokens(event));
        }
        offset = event_end;
    }
    tokens.max(0)
}

fn estimate_external_stream_event_output_tokens(event: &str) -> i32 {
    let mut tokens = 0i32;
    for line in event.lines() {
        let Some(data) = line.trim_end_matches('\r').strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(data) {
            tokens = tokens.saturating_add(estimate_external_stream_data_output_tokens(&value));
        }
    }
    tokens.max(0)
}

fn estimate_external_stream_data_output_tokens(value: &serde_json::Value) -> i32 {
    let mut tokens = estimate_openai_choices_output_tokens(value).unwrap_or(0);
    match value.get("type").and_then(|value| value.as_str()) {
        Some("content_block_start") => {
            if let Some(block) = value.get("content_block") {
                tokens =
                    tokens.saturating_add(estimate_external_content_block_output_tokens(block));
            }
        }
        Some("content_block_delta") => {
            if let Some(delta) = value.get("delta") {
                tokens = tokens.saturating_add(estimate_external_delta_output_tokens(delta));
            }
        }
        _ => {}
    }
    tokens.max(0)
}

fn estimate_external_content_block_output_tokens(block: &serde_json::Value) -> i32 {
    match block.get("type").and_then(|value| value.as_str()) {
        Some("text") => string_output_tokens(block.get("text")).unwrap_or(0),
        Some("thinking") => string_output_tokens(block.get("thinking")).unwrap_or(0),
        Some("redacted_thinking") => string_output_tokens(block.get("data")).unwrap_or(0),
        Some("tool_use" | "server_tool_use") => 1,
        _ => 0,
    }
}

fn estimate_external_delta_output_tokens(delta: &serde_json::Value) -> i32 {
    match delta.get("type").and_then(|value| value.as_str()) {
        Some("text_delta") => string_output_tokens(delta.get("text")).unwrap_or(0),
        Some("thinking_delta") => string_output_tokens(delta.get("thinking")).unwrap_or(0),
        Some("input_json_delta") => string_output_tokens(delta.get("partial_json")).unwrap_or(0),
        _ => 0,
    }
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
    mut candidates: Vec<(ExternalPool, u32, u32)>,
    config: &ExternalPoolsConfig,
) -> Option<ExternalPool> {
    let transient_failure_penalty = config.external_pool_transient_failure_priority_penalty as u64;
    candidates.sort_by(|(a, a_in_flight, a_streak), (b, b_in_flight, b_streak)| {
        let a_effective_priority =
            a.priority.max(0) as u64 + (*a_streak as u64).saturating_mul(transient_failure_penalty);
        let b_effective_priority =
            b.priority.max(0) as u64 + (*b_streak as u64).saturating_mul(transient_failure_penalty);
        let a_load = load_score(*a_in_flight, a.max_concurrent_requests);
        let b_load = load_score(*b_in_flight, b.max_concurrent_requests);
        a_effective_priority
            .cmp(&b_effective_priority)
            .then_with(|| a_load.cmp(&b_load))
            .then_with(|| a.id.cmp(&b.id))
    });
    let (best_priority, best_load) = candidates.first().map(|(pool, in_flight, streak)| {
        let effective_priority = pool.priority.max(0) as u64
            + (*streak as u64).saturating_mul(transient_failure_penalty);
        (
            effective_priority,
            load_score(*in_flight, pool.max_concurrent_requests),
        )
    })?;
    let best: Vec<_> = candidates
        .into_iter()
        .filter(|(pool, in_flight, streak)| {
            pool.priority.max(0) as u64 + (*streak as u64).saturating_mul(transient_failure_penalty)
                == best_priority
                && load_score(*in_flight, pool.max_concurrent_requests) == best_load
        })
        .collect();
    if best.is_empty() {
        return None;
    }
    let idx = fastrand::usize(..best.len());
    best.into_iter().nth(idx).map(|(pool, _, _)| pool)
}

async fn external_pool_lease_lost(lease: Option<&ExternalPoolLease>) {
    match lease {
        Some(lease) => lease.wait_until_lost().await,
        None => std::future::pending().await,
    }
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

fn effective_external_pool_pre_output_stream_retry_enabled(
    pool: &ExternalPool,
    config: &ExternalPoolsConfig,
) -> bool {
    match pool.pre_output_stream_retry_mode {
        ExternalPoolStreamRetryMode::Enabled => true,
        ExternalPoolStreamRetryMode::Disabled => false,
        ExternalPoolStreamRetryMode::Inherit => {
            config.external_pool_stream_pre_output_retry_enabled
        }
    }
}

fn external_pre_output_stream_protocol_error(
    message: impl Into<String>,
    protocol_error: &'static str,
    config: &ExternalPoolsConfig,
    outbound_model: Option<String>,
) -> ExternalForwardError {
    ExternalForwardError::new(
        ExternalPoolError {
            status: Some(StatusCode::OK),
            message: message.into(),
            retryable: true,
            auto_disable_reason: None,
            cooldown: Some((
                Duration::from_secs(config.external_pool_protocol_error_cooldown_secs.max(1)),
                "protocol_error".to_string(),
            )),
            protocol_error: Some(protocol_error),
        },
        outbound_model,
    )
}

fn external_pre_output_stream_network_error(
    message: impl Into<String>,
    config: &ExternalPoolsConfig,
    outbound_model: Option<String>,
) -> ExternalForwardError {
    ExternalForwardError::new(
        ExternalPoolError {
            status: None,
            message: message.into(),
            retryable: true,
            auto_disable_reason: None,
            cooldown: Some((
                Duration::from_secs(config.external_pool_network_error_cooldown_secs.max(1)),
                "network_error".to_string(),
            )),
            protocol_error: None,
        },
        outbound_model,
    )
}

fn external_sse_event_name(event: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(event).ok()?;
    text.lines().find_map(|line| {
        line.trim_end_matches('\r')
            .strip_prefix("event:")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_ascii_lowercase())
    })
}

fn external_sse_event_is_error_event(event: &[u8]) -> bool {
    let text = match std::str::from_utf8(event) {
        Ok(text) => text,
        Err(_) => return false,
    };
    let explicit_error_event = text.lines().any(|line| {
        line.trim_end_matches('\r')
            .strip_prefix("event:")
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("error"))
    });
    if explicit_error_event {
        return true;
    }
    for line in text.split_inclusive('\n') {
        let trimmed_line_end = line.trim_end_matches(['\r', '\n']);
        let Some(data) = trimmed_line_end.strip_prefix("data:") else {
            continue;
        };
        let data_json = data.trim();
        if data_json.is_empty() || data_json == "[DONE]" {
            continue;
        }
        if serde_json::from_str::<serde_json::Value>(data_json)
            .ok()
            .is_some_and(|value| external_stream_payload_is_error(false, &value))
        {
            return true;
        }
    }
    false
}

fn external_sse_event_commits_pre_output_stream(event: &[u8]) -> bool {
    if let Some("content_block_start" | "message_stop") = external_sse_event_name(event).as_deref()
    {
        return true;
    }
    let text = match std::str::from_utf8(event) {
        Ok(text) => text,
        Err(_) => return false,
    };
    for line in text.split_inclusive('\n') {
        let trimmed_line_end = line.trim_end_matches(['\r', '\n']);
        let Some(data) = trimmed_line_end.strip_prefix("data:") else {
            continue;
        };
        let data_json = data.trim();
        if data_json.is_empty() {
            continue;
        }
        if data_json == "[DONE]" {
            return true;
        }
        if serde_json::from_str::<serde_json::Value>(data_json)
            .ok()
            .is_some_and(|value| external_sse_data_commits_pre_output_stream(&value))
        {
            return true;
        }
    }
    false
}

fn external_sse_data_commits_pre_output_stream(value: &serde_json::Value) -> bool {
    match value.get("type").and_then(|value| value.as_str()) {
        Some("content_block_start" | "message_stop") => return true,
        Some("content_block_delta") if external_sse_data_has_first_output(value) => return true,
        _ => {}
    }
    external_sse_data_has_first_output(value) || openai_stream_data_commits_pre_output(value)
}

fn openai_stream_data_commits_pre_output(value: &serde_json::Value) -> bool {
    let Some(choices) = value.get("choices").and_then(|value| value.as_array()) else {
        return false;
    };
    choices.iter().any(|choice| {
        choice
            .get("finish_reason")
            .is_some_and(|value| !value.is_null())
            || ["/message/content", "/delta/content", "/text"]
                .iter()
                .filter_map(|pointer| choice.pointer(pointer))
                .any(|candidate| {
                    estimate_json_output_fragment_tokens(candidate).is_some_and(|tokens| tokens > 0)
                })
    })
}

#[allow(clippy::too_many_arguments)]
async fn pre_read_external_stream_before_commit<S>(
    body_stream: &mut S,
    buffer: &mut Vec<u8>,
    prefix: &mut Vec<u8>,
    lease: &ExternalPoolLease,
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
    config: &ExternalPoolsConfig,
    projection_context: Option<&ExternalUsageProjectionContext>,
    usage_capture: &Arc<SyncMutex<ExternalUsageCapture>>,
    stream_plan: ExternalStreamProcessingPlan,
    stream_error_mask: &ExternalStreamErrorMask,
    transcript_state: &Arc<SyncMutex<ExternalAnthropicTranscriptState>>,
    stream_idle_timeout: Option<Duration>,
    outbound_model: &Option<String>,
) -> Result<Instant, ExternalForwardError>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Unpin,
{
    let mut last_chunk_at = Instant::now();
    loop {
        while let Some((idx, delimiter_len)) = find_sse_event_delimiter(buffer) {
            let end = idx + delimiter_len;
            let event = buffer.drain(..end).collect::<Vec<u8>>();
            if external_sse_event_is_error_event(&event) {
                tracing::warn!(
                    request_id = %route.request_id,
                    error_id = %route.error_id,
                    pool_id = pool.id,
                    pool_name = %pool.name,
                    "external pool stream emitted error event before downstream semantic output; retrying within external pool budget"
                );
                return Err(external_pre_output_stream_protocol_error(
                    SAFE_EXTERNAL_STREAM_ERROR_EVENT,
                    "stream_pre_output_error_event",
                    config,
                    outbound_model.clone(),
                ));
            }

            let commits_downstream = external_sse_event_commits_pre_output_stream(&event);
            prefix.extend(process_sse_event_with_plan_and_transcript(
                &event,
                projection_context,
                Some(usage_capture),
                Some(stream_error_mask),
                stream_plan,
                Some(&mut transcript_state.lock()),
            ));
            if prefix.len() > EXTERNAL_POOL_MAX_SSE_EVENT_BUFFER_BYTES {
                tracing::warn!(
                    request_id = %route.request_id,
                    error_id = %route.error_id,
                    pool_id = pool.id,
                    pool_name = %pool.name,
                    buffered_bytes = prefix.len(),
                    max_buffered_bytes = EXTERNAL_POOL_MAX_SSE_EVENT_BUFFER_BYTES,
                    "external pool pre-output stream prefix exceeded limit before commit"
                );
                return Err(external_pre_output_stream_protocol_error(
                    "external pre-output stream buffer exceeded limit",
                    "stream_pre_output_buffer_limit",
                    config,
                    outbound_model.clone(),
                ));
            }
            if commits_downstream {
                return Ok(last_chunk_at);
            }
        }

        if buffer.len() > EXTERNAL_POOL_MAX_SSE_EVENT_BUFFER_BYTES {
            tracing::warn!(
                request_id = %route.request_id,
                error_id = %route.error_id,
                pool_id = pool.id,
                pool_name = %pool.name,
                buffered_bytes = buffer.len(),
                max_buffered_bytes = EXTERNAL_POOL_MAX_SSE_EVENT_BUFFER_BYTES,
                "external pool pre-output stream event buffer exceeded limit"
            );
            return Err(external_pre_output_stream_protocol_error(
                "external pre-output stream event exceeded buffer limit",
                "stream_pre_output_buffer_limit",
                config,
                outbound_model.clone(),
            ));
        }

        tokio::select! {
            chunk = body_stream.next() => {
                match chunk {
                    Some(Ok(chunk)) => {
                        route.latency_trace.mark_first_upstream_chunk(route.started_at);
                        last_chunk_at = Instant::now();
                        buffer.extend_from_slice(&chunk);
                    }
                    Some(Err(err)) => {
                        tracing::warn!(
                            request_id = %route.request_id,
                            error_id = %route.error_id,
                            pool_id = pool.id,
                            pool_name = %pool.name,
                            error_class = %sanitized_external_network_error("stream pre-output read failed", &err),
                            "external pool stream read failed before downstream semantic output"
                        );
                        return Err(external_pre_output_stream_network_error(
                            sanitized_external_network_error("stream pre-output read failed", &err),
                            config,
                            outbound_model.clone(),
                        ));
                    }
                    None => {
                        tracing::warn!(
                            request_id = %route.request_id,
                            error_id = %route.error_id,
                            pool_id = pool.id,
                            pool_name = %pool.name,
                            buffered_bytes = buffer.len(),
                            prefix_bytes = prefix.len(),
                            "external pool stream ended before downstream semantic output or terminal event"
                        );
                        return Err(external_pre_output_stream_protocol_error(
                            "external upstream ended stream before semantic output",
                            "stream_pre_output_eof",
                            config,
                            outbound_model.clone(),
                        ));
                    }
                }
            }
            _ = lease.wait_until_lost() => {
                return Err(external_pool_lease_lost_forward_error(outbound_model.clone()));
            }
            _ = external_pool_stream_idle_deadline(last_chunk_at, stream_idle_timeout) => {
                let seconds = stream_idle_timeout
                    .map(|timeout| timeout.as_secs())
                    .unwrap_or_default();
                tracing::warn!(
                    request_id = %route.request_id,
                    error_id = %route.error_id,
                    pool_id = pool.id,
                    pool_name = %pool.name,
                    idle_timeout_secs = seconds,
                    "external pool stream idled before downstream semantic output"
                );
                return Err(external_pre_output_stream_network_error(
                    format!(
                        "model endpoint stream idle timeout before semantic output after {} seconds",
                        seconds
                    ),
                    config,
                    outbound_model.clone(),
                ));
            }
        }
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
        protocol_error: None,
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
    if !out.contains_key(HeaderName::from_static("anthropic-version")) {
        out.insert(
            HeaderName::from_static("anthropic-version"),
            HeaderValue::from_static("2023-06-01"),
        );
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
                    protocol_error: None,
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
                protocol_error: None,
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

#[cfg(test)]
fn external_pool_matches_body_mode_filter(
    pool: &ExternalPool,
    filter: Option<ExternalPoolRequestBodyMode>,
) -> bool {
    filter.is_none_or(|mode| pool.request_body_mode == mode)
}

fn normalize_external_pool_support_candidates<'a>(
    candidates: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    let mut normalized = Vec::new();
    for candidate in candidates {
        if let Some(candidate) = normalize_model_id(candidate) {
            if !normalized.iter().any(|existing| existing == &candidate) {
                normalized.push(candidate);
            }
        }
    }
    normalized
}

fn external_pool_matches_supported_models_normalized(
    pool: &ExternalPool,
    model_candidates: Option<&[String]>,
) -> bool {
    if pool.supported_models.is_empty() {
        return true;
    }
    let Some(model_candidates) = model_candidates else {
        return false;
    };
    model_candidates
        .iter()
        .any(|candidate| pool.supported_models.iter().any(|model| model == candidate))
}

fn external_pool_route_policy_allows(
    mode: ExternalPoolRouteMode,
    rules: &[String],
    endpoint: Option<&str>,
) -> bool {
    let Some(endpoint) = endpoint else {
        return true;
    };
    match mode {
        ExternalPoolRouteMode::AllowAll => true,
        ExternalPoolRouteMode::AllowList => {
            rules.iter().any(|rule| route_rule_matches(rule, endpoint))
        }
        ExternalPoolRouteMode::DenyList => {
            !rules.iter().any(|rule| route_rule_matches(rule, endpoint))
        }
    }
}

fn external_pool_route_allowed(pool: &ExternalPool, endpoint: Option<&str>) -> bool {
    external_pool_route_policy_allows(pool.route_mode, &pool.route_rules, endpoint)
}

fn external_pool_eligibility_route_allowed(
    pool: &ExternalPoolEligibility,
    endpoint: Option<&str>,
) -> bool {
    external_pool_route_policy_allows(pool.route_mode, &pool.route_rules, endpoint)
}

fn external_pool_eligibility_matches_supported_models(
    pool: &ExternalPoolEligibility,
    model_candidates: Option<&[String]>,
) -> bool {
    if pool.supported_models.is_empty() {
        return true;
    }
    let Some(model_candidates) = model_candidates else {
        return false;
    };
    model_candidates
        .iter()
        .any(|candidate| pool.supported_models.contains(candidate))
}

fn deterministic_duration_jitter(base: Duration, percent: u8, seed: u64) -> Duration {
    let percent = percent.min(50) as u128;
    if percent == 0 {
        return base.max(Duration::from_millis(1));
    }
    let base_millis = base.as_millis().max(1);
    let spread = (base_millis.saturating_mul(percent) / 100).max(1);
    let slots = spread.saturating_mul(2).saturating_add(1);
    let mixed = seed
        .wrapping_add(0x9e37_79b9_7f4a_7c15)
        .wrapping_mul(0xbf58_476d_1ce4_e5b9)
        ^ seed.rotate_left(27);
    let offset = (mixed as u128 % slots) as i128 - spread as i128;
    let jittered = (base_millis as i128 + offset).max(1) as u128;
    Duration::from_millis(jittered.min(u64::MAX as u128) as u64)
}

fn external_release_retry_delay(failures: u32, intent: &ExternalPoolReleaseIntent) -> Duration {
    let multiplier = 1u32
        .checked_shl(failures.saturating_sub(1).min(16))
        .unwrap_or(u32::MAX);
    let base = EXTERNAL_POOL_QUEUE_REDIS_RETRY_DELAY
        .saturating_mul(multiplier)
        .min(EXTERNAL_POOL_RELEASE_RETRY_MAX_DELAY);
    let seed = intent.pool_id
        ^ ((intent.lease_id.len() as u64) << 32)
        ^ u64::from(failures)
        ^ match intent.release_kind {
            ExternalPoolLeaseReleaseKind::Pending => 0x5045_4e44_494e_47,
            ExternalPoolLeaseReleaseKind::Confirmed => 0x434f_4e46_4952_4d,
        };
    deterministic_duration_jitter(base, 10, seed)
}

fn external_release_system_retry_delay(failures: u32) -> Duration {
    let multiplier = 1u32
        .checked_shl(failures.saturating_sub(1).min(16))
        .unwrap_or(u32::MAX);
    EXTERNAL_POOL_QUEUE_REDIS_RETRY_DELAY
        .saturating_mul(multiplier)
        .min(EXTERNAL_POOL_RELEASE_RETRY_MAX_DELAY)
}

#[cfg(test)]
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
    let normalized =
        normalize_external_pool_support_candidates(model_candidates.iter().flatten().copied());
    external_pool_matches_supported_models_normalized(pool, Some(&normalized))
}

fn normalize_external_pool_model_cooldown_key_model(model: &str) -> Option<String> {
    let normalized = model.trim().to_ascii_lowercase();
    (!normalized.is_empty()).then_some(normalized)
}

fn external_pool_model_cooldown_key(pool_id: u64, normalized_model: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(normalized_model.as_bytes());
    let digest = hasher.finalize();
    format!("external_pool:{}:model_cooldown:{:x}", pool_id, digest)
}

fn external_pool_cooldown_keys(pool_id: u64, models: &[String]) -> Vec<String> {
    let mut keys = Vec::with_capacity(models.len().saturating_add(1));
    keys.push(format!("external_pool:{}:cooldown", pool_id));
    keys.extend(
        models
            .iter()
            .map(|model| external_pool_model_cooldown_key(pool_id, model)),
    );
    keys
}

fn external_pool_transient_failure_key(pool_id: u64) -> String {
    format!("external_pool:{}:transient_failures", pool_id)
}

fn decode_pool_runtime_snapshot(
    pool_id: u64,
    models: &[String],
    cooldown_keys: &[String],
    coordinator: ExternalPoolCoordinatorSnapshot,
) -> anyhow::Result<PoolRuntimeSnapshot> {
    if coordinator.cooldown_values.len() != cooldown_keys.len()
        || coordinator.cooldown_ttls.len() != cooldown_keys.len()
    {
        anyhow::bail!("Redis returned an incomplete external pool coordinator snapshot");
    }

    let mut snapshot = PoolRuntimeSnapshot {
        in_flight: coordinator.capacity.pool_in_flight_requests,
        global_in_flight: coordinator.capacity.global_in_flight_requests,
        transient_failure_streak: coordinator.transient_failure_streak,
        transient_failure_ttl: coordinator.transient_failure_ttl,
        ..PoolRuntimeSnapshot::default()
    };
    if let Some(raw) = coordinator
        .cooldown_values
        .first()
        .and_then(Option::as_deref)
    {
        let cooldown = serde_json::from_str::<ExternalPoolCooldownState>(raw).map_err(|err| {
            anyhow::anyhow!("invalid external pool cooldown for pool {pool_id}: {err}")
        })?;
        let remaining = coordinator
            .cooldown_ttls
            .first()
            .and_then(|remaining| *remaining)
            .ok_or_else(|| {
                anyhow::anyhow!("external pool cooldown for pool {pool_id} is missing a Redis TTL")
            })?;
        snapshot.pool_cooldown_remaining_secs =
            remaining.as_millis().saturating_add(999) as u64 / 1_000;
        snapshot.pool_cooldown_remaining_secs = snapshot.pool_cooldown_remaining_secs.max(1);
        snapshot.pool_cooldown_reason = cooldown.reason;
    }

    if snapshot.pool_cooldown_remaining_secs == 0 {
        for (((model, _key), raw), remaining) in models
            .iter()
            .zip(cooldown_keys.iter().skip(1))
            .zip(coordinator.cooldown_values.iter().skip(1))
            .zip(coordinator.cooldown_ttls.iter().skip(1))
        {
            let Some(raw) = raw.as_deref() else {
                continue;
            };
            let cooldown =
                serde_json::from_str::<ExternalPoolCooldownState>(raw).map_err(|err| {
                anyhow::anyhow!(
                    "invalid external pool model cooldown for pool {pool_id}, model {model}: {err}"
                )
                })?;
            let remaining = remaining.ok_or_else(|| {
                anyhow::anyhow!(
                    "external pool model cooldown for pool {pool_id}, model {model} is missing a Redis TTL"
                )
            })?;
            let remaining_secs = (remaining.as_millis().saturating_add(999) / 1_000).max(1) as u64;
            if snapshot
                .model_cooldown
                .as_ref()
                .map(|(selected_secs, _)| remaining_secs < *selected_secs)
                .unwrap_or(true)
            {
                snapshot.model_cooldown = Some((remaining_secs, cooldown.reason));
            }
        }
    }
    Ok(snapshot)
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
            | "accept-encoding"
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
    _body: &Bytes,
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
        protocol_error: Some("success_error_envelope"),
    }
}

fn external_protocol_contamination_error(config: &ExternalPoolsConfig) -> ExternalPoolError {
    ExternalPoolError {
        status: Some(StatusCode::OK),
        message: RESPONSE_PROTOCOL_CONTAMINATION_DETAIL.to_string(),
        retryable: true,
        auto_disable_reason: None,
        cooldown: Some((
            Duration::from_secs(config.external_pool_protocol_error_cooldown_secs.max(1)),
            "protocol_contamination".to_string(),
        )),
        protocol_error: None,
    }
}

fn success_protocol_error(
    headers: &HeaderMap,
    _body: Option<&Bytes>,
    config: &ExternalPoolsConfig,
    context: &str,
) -> ExternalPoolError {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_ascii_lowercase);
    let content_type_class = match content_type.as_deref() {
        Some(value) if value.contains("application/json") => "json",
        Some(value) if value.contains("text/event-stream") => "event_stream",
        Some(value) if value.contains("text/html") => "html",
        Some(_) => "other",
        None => "missing_or_invalid",
    };
    let message = format!("{context}; content_type_class={content_type_class}");
    ExternalPoolError {
        status: Some(StatusCode::OK),
        message,
        retryable: true,
        auto_disable_reason: Some("misconfigured_endpoint".to_string()),
        cooldown: Some((
            Duration::from_secs(config.external_pool_protocol_error_cooldown_secs.max(1)),
            "misconfigured_endpoint".to_string(),
        )),
        protocol_error: Some("unexpected_response_shape"),
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
    ExternalPoolFinalError {
        status: err.status.unwrap_or(StatusCode::BAD_GATEWAY),
        response_error_type: anthropic_error_type_for_external_error(err).to_string(),
        route_error_type: error_type_for_external_error(err),
        message: err.message.clone(),
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
        PoolCapacityWaitReason::ModelUnavailable => (
            "model_unavailable",
            "External fallback model is temporarily unavailable",
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
    let retryable = code != "external_pool_wait_timeout";
    ExternalPoolFinalError {
        status,
        response_error_type: code.to_string(),
        route_error_type: code.to_string(),
        message: message.into(),
        error_id: error_id.to_string(),
        retryable,
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
    compact_usage_error_message(&err.message)
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
        if reason == "model_unavailable" || should_mark_external_pool_hard_cooldown(reason) {
            metadata_insert(&mut metadata, "cooldownReason", reason.as_str());
            if !duration.is_zero() {
                metadata_insert(
                    &mut metadata,
                    "cooldownMs",
                    serde_json::Value::from(duration.as_millis() as u64),
                );
            }
        } else if should_record_external_pool_soft_failure(reason) {
            metadata_insert(&mut metadata, "softFailureReason", reason.as_str());
            metadata_insert(
                &mut metadata,
                "softFailureWindowSecs",
                EXTERNAL_POOL_TRANSIENT_FAILURE_WINDOW_SECS as u64,
            );
        }
    }
    if message_truncated {
        metadata_insert(&mut metadata, "messageTruncated", true);
    }
    if let Some(protocol_error) = err.protocol_error {
        metadata_insert(&mut metadata, "protocolError", protocol_error);
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

fn synthetic_external_capacity_error_diagnostics(
    route: &ExternalRouteRequest,
    status: StatusCode,
    source: &'static str,
    config: &ExternalPoolsConfig,
    context: Option<&PoolCapacityWaitContext>,
) -> UsageErrorDiagnostics {
    let mut metadata = serde_json::Map::new();
    metadata_insert(&mut metadata, "syntheticStatus", true);
    metadata_insert(
        &mut metadata,
        "externalPoolCapacityMode",
        match config.external_pool_capacity_mode {
            ExternalPoolCapacityMode::FailFast => "fail_fast",
            ExternalPoolCapacityMode::Wait => "wait",
        },
    );
    metadata_insert(
        &mut metadata,
        "externalPoolMaxQueuedRequests",
        config.external_pool_max_queued_requests,
    );
    if let Some(context) = context {
        metadata_insert(&mut metadata, "capacityWaitReason", context.reason.as_str());
        metadata_insert(
            &mut metadata,
            "eligiblePools",
            context.eligible_pools as u64,
        );
        metadata_insert(
            &mut metadata,
            "availablePools",
            context.available_pools as u64,
        );
        metadata_insert(
            &mut metadata,
            "temporaryUnavailablePools",
            context.temporary_unavailable_pools as u64,
        );
        if let Some(kind) = context.coordinator_unavailable_kind {
            metadata_insert(&mut metadata, "coordinatorUnavailableKind", kind.as_str());
        }
        if let Some(reason) = context.cooldown_reason.as_deref() {
            metadata_insert(&mut metadata, "cooldownReason", reason);
        }
        if let Some(scope) = context.cooldown_scope {
            metadata_insert(&mut metadata, "cooldownScope", scope.as_str());
        }
        if let Some(remaining_secs) = context.cooldown_remaining_secs {
            metadata_insert(&mut metadata, "cooldownRemainingSecs", remaining_secs);
        }
    }
    UsageErrorDiagnostics {
        status_code: Some(status.as_u16()),
        source: Some(source.to_string()),
        error_id: Some(route.error_id.clone()),
        metadata: Some(serde_json::Value::Object(metadata)),
        public_error: None,
    }
}

fn external_error_message_indicates_model_unavailable(lower_message: &str) -> bool {
    lower_message.contains("model_not_found")
        || lower_message.contains("failed to get available channel for model")
        || lower_message.contains("no available channel")
        || lower_message.contains("model is unavailable")
}

fn external_error_message_indicates_payload_too_long(lower_message: &str) -> bool {
    lower_message.contains("context window is full")
        || lower_message.contains("input is too long")
        || lower_message.contains("prompt is too long")
        || lower_message.contains("content_length_exceeds_threshold")
        || lower_message.contains("request payload is too large")
        || lower_message.contains("payload is too large")
}

fn classified_external_error(
    status: StatusCode,
    message: &'static str,
    retryable: bool,
    auto_disable_reason: Option<&'static str>,
    cooldown: Option<(Duration, &'static str)>,
) -> ExternalPoolError {
    ExternalPoolError {
        status: Some(status),
        message: message.to_string(),
        retryable,
        auto_disable_reason: auto_disable_reason.map(str::to_string),
        cooldown: cooldown.map(|(duration, reason)| (duration, reason.to_string())),
        protocol_error: None,
    }
}

fn external_retry_after_duration(headers: &HeaderMap) -> Option<Duration> {
    let value = headers.get(header::RETRY_AFTER)?.to_str().ok()?.trim();
    if value.is_empty() {
        return None;
    }

    let duration = if let Ok(seconds) = value.parse::<u64>() {
        Duration::from_secs(seconds.max(1))
    } else {
        let retry_at = DateTime::parse_from_rfc2822(value)
            .ok()?
            .with_timezone(&Utc);
        let seconds = retry_at
            .signed_duration_since(Utc::now())
            .num_seconds()
            .max(1) as u64;
        Duration::from_secs(seconds)
    };

    Some(
        duration
            .max(Duration::from_secs(1))
            .min(Duration::from_secs(EXTERNAL_POOL_RETRY_AFTER_MAX_SECS)),
    )
}

fn external_pool_cooldown_duration(
    retry_after: Option<Duration>,
    default_cooldown_secs: u64,
) -> Duration {
    retry_after.unwrap_or_else(|| Duration::from_secs(default_cooldown_secs.max(1)))
}

fn should_record_external_pool_soft_failure(reason: &str) -> bool {
    !matches!(reason, "model_mapping_miss" | "model_unavailable")
}

fn should_mark_external_pool_hard_cooldown(reason: &str) -> bool {
    matches!(reason, "misconfigured_endpoint")
}

fn classify_external_error(
    status: StatusCode,
    body: Bytes,
    headers: HeaderMap,
    config: &ExternalPoolsConfig,
) -> ExternalPoolError {
    let retry_after = external_retry_after_duration(&headers);
    let lower = String::from_utf8_lossy(&body).to_ascii_lowercase();
    if lower.contains("too many requests")
        || lower.contains("service_request_rate_exceeded")
        || lower.contains("rate limit")
        || lower.contains("ratelimit")
    {
        return classified_external_error(
            status,
            "external upstream rate limited the request",
            true,
            None,
            Some((
                external_pool_cooldown_duration(
                    retry_after,
                    config.external_pool_rate_limit_cooldown_secs,
                ),
                "rate_limit",
            )),
        );
    }
    if lower.contains("database is locked") || lower.contains("sqlite_busy") {
        return classified_external_error(
            status,
            "external upstream database was busy",
            true,
            None,
            Some((
                external_pool_cooldown_duration(
                    retry_after,
                    config.external_pool_server_error_cooldown_secs,
                ),
                "database_busy",
            )),
        );
    }
    if lower.contains("invalid token") {
        return classified_external_error(
            status,
            "external upstream rejected authentication",
            true,
            Some("auth_error"),
            Some((
                external_pool_cooldown_duration(
                    retry_after,
                    config.external_pool_protocol_error_cooldown_secs,
                ),
                "auth_error",
            )),
        );
    }
    if lower.contains("channel affinity") && lower.contains("disabled") {
        return classified_external_error(
            status,
            "external upstream channel is disabled",
            true,
            Some("channel_disabled"),
            Some((
                external_pool_cooldown_duration(
                    retry_after,
                    config.external_pool_protocol_error_cooldown_secs,
                ),
                "channel_disabled",
            )),
        );
    }
    if external_error_message_indicates_model_unavailable(&lower) {
        return classified_external_error(
            status,
            "external upstream model is unavailable",
            true,
            None,
            (config.external_pool_model_unavailable_cooldown_mode
                != ExternalPoolModelUnavailableCooldownMode::Disabled)
                .then(|| {
                    (
                        external_pool_cooldown_duration(
                            retry_after,
                            config.external_pool_model_unavailable_cooldown_secs,
                        ),
                        "model_unavailable",
                    )
                }),
        );
    }
    if status == StatusCode::BAD_REQUEST {
        let message = if external_error_message_indicates_payload_too_long(&lower) {
            "external upstream rejected the request because the prompt is too long"
        } else {
            "external upstream rejected the request"
        };
        return classified_external_error(status, message, false, None, None);
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        return classified_external_error(
            status,
            "external upstream rate limited the request",
            true,
            None,
            Some((
                external_pool_cooldown_duration(
                    retry_after,
                    config.external_pool_rate_limit_cooldown_secs,
                ),
                "rate_limit",
            )),
        );
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
        return classified_external_error(
            status,
            "external upstream rejected account authorization",
            true,
            Some(reason),
            Some((
                external_pool_cooldown_duration(
                    retry_after,
                    config.external_pool_protocol_error_cooldown_secs,
                ),
                reason,
            )),
        );
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
        return classified_external_error(
            status,
            "external upstream quota is unavailable",
            true,
            Some("quota_exhausted"),
            Some((
                external_pool_cooldown_duration(
                    retry_after,
                    config.external_pool_rate_limit_cooldown_secs,
                ),
                "quota_exhausted",
            )),
        );
    }
    if status.is_server_error() || status == StatusCode::REQUEST_TIMEOUT {
        return classified_external_error(
            status,
            "external upstream server was temporarily unavailable",
            true,
            None,
            Some((
                external_pool_cooldown_duration(
                    retry_after,
                    config.external_pool_server_error_cooldown_secs,
                ),
                "server_error",
            )),
        );
    }
    classified_external_error(
        status,
        "external upstream returned an unexpected error",
        false,
        None,
        None,
    )
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

fn should_retry_external_same_pool(config: &ExternalPoolsConfig, err: &ExternalPoolError) -> bool {
    retry_pipeline::should_retry_same_pool(config, err)
}

fn should_retry_external_cross_pool(config: &ExternalPoolsConfig, err: &ExternalPoolError) -> bool {
    retry_pipeline::should_retry_cross_pool(config, err)
}

fn external_same_pool_retry_delay(config: &ExternalPoolsConfig) -> Option<Duration> {
    retry_pipeline::same_pool_retry_delay(config)
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
    if external_error_message_indicates_model_unavailable(&err.message.to_ascii_lowercase()) {
        return "model_unavailable".to_string();
    }
    if let Some((_, reason)) = err.cooldown.as_ref().filter(|(_, reason)| {
        reason == "rate_limit"
            || reason == "database_busy"
            || reason == "model_unavailable"
            || reason == "model_mapping_miss"
            || reason == "server_error"
            || reason == "protocol_error"
            || reason.starts_with("network_error")
            || reason == "inference_attempt_limit"
            || reason == "inference_attempt_reserved_for_fallback"
            || reason == "downstream_committed"
    }) {
        return reason
            .split_whitespace()
            .next()
            .unwrap_or("external_pool_error")
            .to_string();
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

fn external_route_known_tool_names(route: &ExternalRouteRequest) -> Arc<Vec<String>> {
    route
        .preparation_cache
        .known_tool_names
        .get_or_init(|| {
            route.preparation_cache.record_known_tool_name_build();
            let names = route
                .payload
                .as_ref()
                .map(collect_known_tool_names_from_request)
                .or_else(|| {
                    external_route_raw_projection_payload(route)
                        .as_deref()
                        .map(collect_known_tool_names_from_request)
                })
                .unwrap_or_default();
            Arc::new(names)
        })
        .clone()
}

struct ProjectedNonStreamBody {
    body: Bytes,
    usage_capture: ExternalUsageCapture,
    protocol_contamination: bool,
}

impl ProjectedNonStreamBody {
    fn without_protocol_contamination(body: Bytes, usage_capture: ExternalUsageCapture) -> Self {
        Self {
            body,
            usage_capture,
            protocol_contamination: false,
        }
    }
}

#[cfg(test)]
fn maybe_project_non_stream_usage(
    bytes: Bytes,
    projection: Option<&ExternalUsageProjectionContext>,
) -> ProjectedNonStreamBody {
    maybe_project_non_stream_usage_with_tools(bytes, projection, std::iter::empty())
}

#[cfg(test)]
fn maybe_project_non_stream_usage_with_tools(
    bytes: Bytes,
    projection: Option<&ExternalUsageProjectionContext>,
    known_tool_names: impl IntoIterator<Item = String>,
) -> ProjectedNonStreamBody {
    process_non_stream_response_usage(bytes, None, projection, known_tool_names)
}

fn process_non_stream_response_usage(
    bytes: Bytes,
    route: Option<&ExternalRouteRequest>,
    projection: Option<&ExternalUsageProjectionContext>,
    known_tool_names: impl IntoIterator<Item = String>,
) -> ProjectedNonStreamBody {
    let mut usage_capture = ExternalUsageCapture::default();
    usage_capture.request_input_tokens =
        route.map(|route| estimated_external_request_input_tokens(route, projection));
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        if let Some(estimated) =
            estimate_unrecognized_non_stream_response_usage(route, projection, None)
        {
            apply_estimated_usage_capture(
                &mut usage_capture,
                estimated,
                "unrecognized_success_body",
            );
        }
        return ProjectedNonStreamBody::without_protocol_contamination(bytes, usage_capture);
    };
    let sanitization = sanitize_response_content(&mut value, known_tool_names);
    let sanitized = sanitization.blocks > 0;

    if let Some((candidate_path, pointer)) = select_non_stream_usage_candidate(&value) {
        usage_capture.usage_candidate_path = Some(candidate_path.to_string());
        let response_output_estimate = estimate_non_stream_output_tokens(&value);
        usage_capture.estimated_output_tokens = response_output_estimate.unwrap_or(0).max(0);
        let mut changed = false;
        {
            let Some(usage) = value.pointer_mut(pointer) else {
                return ProjectedNonStreamBody {
                    body: bytes,
                    usage_capture,
                    protocol_contamination: sanitized,
                };
            };
            let normalized = normalize_external_usage_value(usage);
            usage_capture.raw = normalized.usage;
            usage_capture.reported = normalized.usage;
            changed |= normalized.changed;

            if let Some(projected) =
                project_usage_value(usage, projection, true, response_output_estimate)
            {
                usage_capture.request_input_tokens = Some(projected.request_input_tokens);
                usage_capture.shaped = Some(projected.shaped);
                usage_capture.reported = cache_usage_from_value(usage)
                    .or(Some(projected.reported))
                    .or(normalized.usage);
                usage_capture.projected = true;
                changed = true;
            }
        }

        if let Some(reported) = usage_capture
            .reported
            .or(usage_capture.raw)
            .filter(|_| pointer != "/usage")
        {
            changed |= set_top_level_usage_value(
                &mut value,
                anthropic_usage_value_for_body(reported, usage_capture.projected),
            );
        }

        let body = if sanitized || changed {
            serde_json::to_vec(&value)
                .map(Bytes::from)
                .unwrap_or_else(|_| bytes.clone())
        } else {
            bytes
        };
        return ProjectedNonStreamBody {
            body,
            usage_capture,
            protocol_contamination: sanitized,
        };
    }

    if let Some(estimated) = normal_non_stream_model_response(&value)
        .then(|| estimate_non_stream_response_usage(route, projection, &value))
        .flatten()
    {
        apply_estimated_usage_capture(&mut usage_capture, estimated, "missing_upstream_usage");
        set_top_level_usage_value(
            &mut value,
            anthropic_usage_value_for_body(estimated.reported, estimated.projected),
        );
        let body = serde_json::to_vec(&value)
            .map(Bytes::from)
            .unwrap_or_else(|_| bytes.clone());
        return ProjectedNonStreamBody {
            body,
            usage_capture,
            protocol_contamination: sanitized,
        };
    }

    if let Some(estimated) =
        estimate_unrecognized_non_stream_response_usage(route, projection, Some(&value))
    {
        apply_estimated_usage_capture(&mut usage_capture, estimated, "unrecognized_success_body");
        if value.is_object() {
            set_top_level_usage_value(
                &mut value,
                anthropic_usage_value_for_body(estimated.reported, estimated.projected),
            );
            let body = serde_json::to_vec(&value)
                .map(Bytes::from)
                .unwrap_or_else(|_| bytes.clone());
            return ProjectedNonStreamBody {
                body,
                usage_capture,
                protocol_contamination: sanitized,
            };
        }
    }

    ProjectedNonStreamBody {
        body: if sanitized {
            serde_json::to_vec(&value).map(Bytes::from).unwrap_or(bytes)
        } else {
            bytes
        },
        usage_capture,
        protocol_contamination: sanitized,
    }
}

#[derive(Debug, Clone, Copy)]
struct NormalizedUsageValue {
    usage: Option<CacheUsage>,
    changed: bool,
}

#[derive(Debug, Clone, Copy)]
struct EstimatedExternalUsage {
    request_input_tokens: i32,
    raw: CacheUsage,
    shaped: CacheUsage,
    reported: CacheUsage,
    projected: bool,
}

fn apply_estimated_usage_capture(
    usage_capture: &mut ExternalUsageCapture,
    estimated: EstimatedExternalUsage,
    reason: &'static str,
) {
    usage_capture.raw = Some(estimated.raw);
    usage_capture.shaped = Some(estimated.shaped);
    usage_capture.reported = Some(estimated.reported);
    usage_capture.projected = estimated.projected;
    usage_capture.usage_estimated = true;
    usage_capture.usage_estimate_reason = Some(reason.to_string());
    usage_capture.request_input_tokens = Some(estimated.request_input_tokens);
    usage_capture.estimated_output_tokens = estimated.raw.output_tokens.max(0);
}

fn select_non_stream_usage_candidate(
    value: &serde_json::Value,
) -> Option<(&'static str, &'static str)> {
    const CANDIDATES: [(&str, &str); 5] = [
        ("$.usage", "/usage"),
        ("$.message.usage", "/message/usage"),
        ("$.delta.usage", "/delta/usage"),
        ("$.data.usage", "/data/usage"),
        ("$.response.usage", "/response/usage"),
    ];
    CANDIDATES.into_iter().find(|(_, pointer)| {
        value
            .pointer(pointer)
            .and_then(cache_usage_from_any_value)
            .is_some()
    })
}

fn normalize_external_usage_value(usage: &mut serde_json::Value) -> NormalizedUsageValue {
    if let Some(usage) = cache_usage_from_value(usage) {
        return NormalizedUsageValue {
            usage: Some(usage),
            changed: false,
        };
    }

    let Some(openai_usage) = openai_usage_from_value(usage) else {
        return NormalizedUsageValue {
            usage: None,
            changed: false,
        };
    };
    let Some(obj) = usage.as_object_mut() else {
        return NormalizedUsageValue {
            usage: Some(openai_usage),
            changed: false,
        };
    };
    obj.insert("input_tokens".to_string(), json!(openai_usage.input_tokens));
    obj.insert(
        "output_tokens".to_string(),
        json!(openai_usage.output_tokens),
    );
    obj.entry("cache_creation_input_tokens".to_string())
        .or_insert_with(|| json!(0));
    obj.entry("cache_read_input_tokens".to_string())
        .or_insert_with(|| json!(0));
    NormalizedUsageValue {
        usage: Some(openai_usage),
        changed: true,
    }
}

fn cache_usage_from_any_value(value: &serde_json::Value) -> Option<CacheUsage> {
    cache_usage_from_value(value).or_else(|| openai_usage_from_value(value))
}

fn openai_usage_from_value(value: &serde_json::Value) -> Option<CacheUsage> {
    let input_tokens = usage_i32(value, "prompt_tokens");
    let output_tokens = usage_i32(value, "completion_tokens");
    if input_tokens == 0 && output_tokens == 0 {
        return None;
    }
    Some(CacheUsage {
        total_input_tokens: input_tokens,
        input_tokens,
        output_tokens,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        cache_creation_5m_input_tokens: 0,
        cache_creation_1h_input_tokens: 0,
    })
}

fn set_top_level_usage_value(value: &mut serde_json::Value, usage: serde_json::Value) -> bool {
    let Some(obj) = value.as_object_mut() else {
        return false;
    };
    if obj.get("usage") == Some(&usage) {
        return false;
    }
    obj.insert("usage".to_string(), usage);
    true
}

fn anthropic_usage_value_for_body(
    usage: CacheUsage,
    include_cache_creation_breakdown: bool,
) -> serde_json::Value {
    let mut value = usage.to_anthropic_usage_json();
    if let Some(obj) = value
        .as_object_mut()
        .filter(|_| include_cache_creation_breakdown)
    {
        apply_projected_cache_creation_breakdown(obj, usage);
    }
    value
}

fn normal_non_stream_model_response(value: &serde_json::Value) -> bool {
    value
        .get("type")
        .and_then(|value| value.as_str())
        .is_some_and(|value| value == "message")
        || value.get("content").is_some()
        || value.pointer("/message/content").is_some()
        || value.pointer("/data/content").is_some()
        || value.pointer("/response/content").is_some()
}

fn estimate_non_stream_response_usage(
    route: Option<&ExternalRouteRequest>,
    projection: Option<&ExternalUsageProjectionContext>,
    value: &serde_json::Value,
) -> Option<EstimatedExternalUsage> {
    let route = route?;
    let request_input_tokens = estimated_external_request_input_tokens(route, projection);
    let output_tokens = estimate_non_stream_output_tokens(value).unwrap_or(0);
    Some(estimated_external_usage_from_parts(
        request_input_tokens,
        output_tokens,
        projection,
        true,
    ))
}

fn estimate_unrecognized_non_stream_response_usage(
    route: Option<&ExternalRouteRequest>,
    projection: Option<&ExternalUsageProjectionContext>,
    value: Option<&serde_json::Value>,
) -> Option<EstimatedExternalUsage> {
    let route = route?;
    let request_input_tokens = estimated_external_request_input_tokens(route, projection);
    let output_tokens = value
        .and_then(estimate_unrecognized_non_stream_output_tokens)
        .unwrap_or(0);
    Some(estimated_external_usage_from_parts(
        request_input_tokens,
        output_tokens,
        projection,
        true,
    ))
}

fn estimated_external_request_input_tokens(
    route: &ExternalRouteRequest,
    projection: Option<&ExternalUsageProjectionContext>,
) -> i32 {
    projection
        .map(|projection| projection.raw_input_tokens)
        .filter(|tokens| *tokens > 0)
        .or_else(|| (route.request_input_tokens > 0).then_some(route.request_input_tokens))
        .or_else(|| {
            route
                .payload
                .as_ref()
                .map(count_external_route_input_tokens)
        })
        .unwrap_or(0)
        .max(0)
}

fn estimate_unrecognized_non_stream_output_tokens(value: &serde_json::Value) -> Option<i32> {
    estimate_openai_choices_output_tokens(value).or_else(|| {
        const STRING_POINTERS: [&str; 7] = [
            "/output_text",
            "/text",
            "/result",
            "/data/output_text",
            "/data/text",
            "/data/result",
            "/response/output_text",
        ];
        STRING_POINTERS
            .iter()
            .find_map(|pointer| string_output_tokens(value.pointer(pointer)))
    })
}

fn estimate_openai_choices_output_tokens(value: &serde_json::Value) -> Option<i32> {
    let choices = value.get("choices")?.as_array()?;
    let mut tokens = 0i32;
    let mut observed = false;
    for choice in choices {
        for pointer in ["/message/content", "/delta/content", "/text"] {
            let Some(candidate) = choice.pointer(pointer) else {
                continue;
            };
            if let Some(candidate_tokens) = estimate_json_output_fragment_tokens(candidate) {
                tokens = tokens.saturating_add(candidate_tokens);
                observed = true;
            }
        }
    }
    Some(if observed { tokens.max(1) } else { 0 })
}

fn estimate_json_output_fragment_tokens(value: &serde_json::Value) -> Option<i32> {
    match value {
        serde_json::Value::String(text) => {
            (!text.trim().is_empty()).then(|| (token::count_tokens(text) as i32).max(1))
        }
        serde_json::Value::Array(items) if content_items_have_external_output(items) => {
            Some(token::estimate_output_tokens(items).max(0))
        }
        serde_json::Value::Array(_) => Some(0),
        _ => None,
    }
}

fn string_output_tokens(value: Option<&serde_json::Value>) -> Option<i32> {
    let text = value?.as_str()?;
    (!text.trim().is_empty()).then(|| (token::count_tokens(text) as i32).max(1))
}

fn estimate_non_stream_output_tokens(value: &serde_json::Value) -> Option<i32> {
    const CONTENT_POINTERS: [&str; 4] = [
        "/content",
        "/message/content",
        "/data/content",
        "/response/content",
    ];
    for pointer in CONTENT_POINTERS {
        let Some(content) = value.pointer(pointer) else {
            continue;
        };
        match content {
            serde_json::Value::Array(items) if content_items_have_external_output(items) => {
                return Some(token::estimate_output_tokens(items).max(0));
            }
            serde_json::Value::Array(_) => {
                return Some(0);
            }
            serde_json::Value::String(text) => {
                let output_tokens = if text.trim().is_empty() {
                    0
                } else {
                    (token::count_tokens(text) as i32).max(1)
                };
                return Some(output_tokens);
            }
            _ => {}
        }
    }
    if non_stream_response_has_stop_reason(value) {
        return Some(0);
    }
    None
}

fn content_items_have_external_output(items: &[serde_json::Value]) -> bool {
    items.iter().any(|item| {
        item.get("text")
            .and_then(|value| value.as_str())
            .is_some_and(|text| !text.trim().is_empty())
            || item
                .get("thinking")
                .and_then(|value| value.as_str())
                .is_some_and(|text| !text.trim().is_empty())
            || item
                .get("type")
                .and_then(|value| value.as_str())
                .is_some_and(|value| value == "tool_use")
    })
}

fn non_stream_response_has_stop_reason(value: &serde_json::Value) -> bool {
    value.get("stop_reason").is_some()
        || value.pointer("/message/stop_reason").is_some()
        || value.pointer("/data/stop_reason").is_some()
        || value.pointer("/response/stop_reason").is_some()
}

fn estimated_external_usage_from_parts(
    request_input_tokens: i32,
    output_tokens: i32,
    projection: Option<&ExternalUsageProjectionContext>,
    commit_cache_state: bool,
) -> EstimatedExternalUsage {
    let raw = CacheUsage {
        total_input_tokens: request_input_tokens.max(0),
        input_tokens: request_input_tokens.max(0),
        output_tokens: output_tokens.max(0),
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        cache_creation_5m_input_tokens: 0,
        cache_creation_1h_input_tokens: 0,
    };
    let mut usage_value = raw.to_anthropic_usage_json();
    if let Some(projected) =
        project_usage_value(&mut usage_value, projection, commit_cache_state, None)
    {
        return EstimatedExternalUsage {
            request_input_tokens: projected.request_input_tokens,
            raw,
            shaped: projected.shaped,
            reported: projected.reported,
            projected: true,
        };
    }
    EstimatedExternalUsage {
        request_input_tokens: request_input_tokens.max(0),
        raw,
        shaped: raw,
        reported: raw,
        projected: false,
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

fn record_external_usage_debug_sse_event(
    event: &[u8],
    capture: Option<&Arc<SyncMutex<ExternalUsageCapture>>>,
    plan: ExternalStreamProcessingPlan,
) {
    if !plan.usage_debug_enabled {
        return;
    }
    let Some(capture) = capture else {
        return;
    };
    let text = String::from_utf8_lossy(event);
    let mut event_name: Option<String> = None;
    let mut payload_type: Option<String> = None;
    let mut usage_candidates = Vec::new();
    let mut saw_usage = false;
    let mut data_lines_seen = 0u64;
    let mut done_events_seen = 0u64;
    let mut json_parse_errors = 0u64;

    for line in text.split_inclusive('\n') {
        let trimmed_line_end = line.trim_end_matches(['\r', '\n']);
        if let Some(value) = trimmed_line_end.strip_prefix("event:") {
            let value = value.trim();
            if !value.is_empty() {
                event_name = Some(value.to_string());
            }
            continue;
        }
        let Some(data) = trimmed_line_end.strip_prefix("data:") else {
            continue;
        };
        data_lines_seen = data_lines_seen.saturating_add(1);
        let data_json = data.trim();
        if data_json.is_empty() {
            continue;
        }
        if data_json == "[DONE]" {
            done_events_seen = done_events_seen.saturating_add(1);
            continue;
        }
        match serde_json::from_str::<serde_json::Value>(data_json) {
            Ok(value) => {
                payload_type = payload_type.or_else(|| {
                    value
                        .get("type")
                        .and_then(|value| value.as_str())
                        .map(str::to_string)
                });
                let candidates = collect_external_pool_usage_debug_candidates(&value);
                if !candidates.is_empty() {
                    saw_usage = true;
                    usage_candidates.extend(candidates);
                    if usage_candidates.len() > EXTERNAL_POOL_USAGE_DEBUG_USAGE_SAMPLE_LIMIT {
                        usage_candidates.truncate(EXTERNAL_POOL_USAGE_DEBUG_USAGE_SAMPLE_LIMIT);
                    }
                }
            }
            Err(_) => {
                json_parse_errors = json_parse_errors.saturating_add(1);
            }
        }
    }

    let mut capture = capture.lock();
    let debug = &mut capture.debug_stream;
    debug.events_seen = debug.events_seen.saturating_add(1);
    debug.data_lines_seen = debug.data_lines_seen.saturating_add(data_lines_seen);
    debug.done_events_seen = debug.done_events_seen.saturating_add(done_events_seen);
    debug.json_parse_errors = debug.json_parse_errors.saturating_add(json_parse_errors);
    debug.raw_stream_bytes_seen = debug
        .raw_stream_bytes_seen
        .saturating_add(event.len() as u64);
    if let Some(event_name) = event_name.as_deref().or(payload_type.as_deref()) {
        *debug.event_types.entry(event_name.to_string()).or_default() += 1;
    }
    if saw_usage {
        debug.usage_events_seen = debug.usage_events_seen.saturating_add(1);
        for candidate in &usage_candidates {
            *debug.usage_paths.entry(candidate.path.clone()).or_default() += 1;
        }
        if debug.raw_usage_event_samples.len() < EXTERNAL_POOL_USAGE_DEBUG_USAGE_SAMPLE_LIMIT {
            let raw_event_cap = event
                .len()
                .min(EXTERNAL_POOL_USAGE_DEBUG_EVENT_SAMPLE_BYTES);
            let raw_event_preview = &event[..raw_event_cap];
            debug
                .raw_usage_event_samples
                .push(ExternalUsageDebugRawEventSample {
                    event: event_name,
                    payload_type,
                    raw_event_utf8: String::from_utf8_lossy(raw_event_preview).to_string(),
                    raw_event_base64: BASE64_STANDARD.encode(raw_event_preview),
                    raw_event_truncated: event.len() > raw_event_cap,
                    usage_candidates,
                });
        }
    }
    let max_preview = plan.usage_debug_max_body_bytes;
    if max_preview > 0 && debug.raw_stream_preview_base64.len() < max_preview.saturating_mul(2) {
        let existing = BASE64_STANDARD
            .decode(debug.raw_stream_preview_base64.as_bytes())
            .unwrap_or_default();
        if existing.len() < max_preview {
            let remaining = max_preview - existing.len();
            let append = &event[..event.len().min(remaining)];
            let mut combined = existing;
            combined.extend_from_slice(append);
            debug.raw_stream_preview_base64 = BASE64_STANDARD.encode(&combined);
            debug.raw_stream_preview_utf8 = String::from_utf8_lossy(&combined).to_string();
            if event.len() > append.len() {
                debug.raw_stream_preview_truncated = true;
            }
        } else {
            debug.raw_stream_preview_truncated = true;
        }
    }
    if max_preview > 0 && debug.raw_stream_bytes_seen as usize > max_preview {
        debug.raw_stream_preview_truncated = true;
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
    let delta_usage = (!handled_top_level)
        .then(|| {
            value
                .get_mut("delta")
                .and_then(|delta| delta.get_mut("usage"))
        })
        .flatten();
    if let Some(usage) = delta_usage {
        result.changed |= process_single_usage_value(usage, projection, capture, rewrite, true);
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
    // Normalize OpenAI-compatible usage before projection/capture. External pools
    // may return either Anthropic usage fields or prompt/completion token fields.
    // When rewriting is disabled this only changes the parsed event copy, not
    // the bytes forwarded to the downstream client.
    let normalized = normalize_external_usage_value(usage);
    let raw_usage = normalized.usage;
    let response_output_estimate = external_usage_capture_output_estimate(capture);
    let Some(projected_usage) = project_usage_value(
        usage,
        projection,
        commit_cache_state,
        response_output_estimate,
    ) else {
        update_external_usage_capture(capture, raw_usage, raw_usage, raw_usage, false);
        return normalized.changed && rewrite;
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

struct ExternalAnthropicTranscriptState {
    sanitizer: ToolTranscriptSanitizer,
    thinking_sanitizer: ToolTranscriptSanitizer,
    current_text_index: Option<u64>,
    current_text_visible: bool,
    current_text_suppressed: bool,
    buffered_thinking: Option<ExternalBufferedThinkingBlock>,
    fatal: bool,
    pending_fatal_error: Option<String>,
    request_id: Option<String>,
    error_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExternalBufferedThinkingKind {
    Thinking,
    Redacted,
}

struct ExternalBufferedThinkingBlock {
    kind: ExternalBufferedThinkingKind,
    events: Vec<Vec<u8>>,
    content: String,
    integrity_values: Vec<String>,
    buffered_bytes: usize,
    overflow: bool,
}

impl ExternalBufferedThinkingBlock {
    fn new(kind: ExternalBufferedThinkingKind, event: &[u8], content: Option<&str>) -> Self {
        let mut block = Self {
            kind,
            events: Vec::new(),
            content: String::new(),
            integrity_values: Vec::new(),
            buffered_bytes: 0,
            overflow: false,
        };
        block.push_event(event);
        if let Some(content) = content {
            block.push_content(content);
        }
        block
    }

    fn reserve(&mut self, bytes: usize) -> bool {
        if self.overflow
            || self.buffered_bytes.saturating_add(bytes) > EXTERNAL_POOL_MAX_SSE_EVENT_BUFFER_BYTES
        {
            if !self.overflow {
                tracing::warn!(
                    buffered_bytes = self.buffered_bytes.saturating_add(bytes),
                    max_buffered_bytes = EXTERNAL_POOL_MAX_SSE_EVENT_BUFFER_BYTES,
                    "external thinking block exceeded bounded atomic buffer and was suppressed"
                );
            }
            self.overflow = true;
            self.events.clear();
            self.content.clear();
            self.integrity_values.clear();
            return false;
        }
        self.buffered_bytes = self.buffered_bytes.saturating_add(bytes);
        true
    }

    fn push_event(&mut self, event: &[u8]) {
        if self.reserve(event.len()) {
            self.events.push(event.to_vec());
        }
    }

    fn push_content(&mut self, content: &str) {
        if self.reserve(content.len()) {
            self.content.push_str(content);
        }
    }

    fn push_integrity_value(&mut self, value: &str) {
        if self.reserve(value.len()) {
            self.integrity_values.push(value.to_string());
        }
    }
}

impl ExternalAnthropicTranscriptState {
    #[cfg(test)]
    fn new(known_tool_names: impl IntoIterator<Item = String>) -> Self {
        Self::new_with_error_context(known_tool_names, None, None)
    }

    fn new_with_error_context(
        known_tool_names: impl IntoIterator<Item = String>,
        request_id: Option<String>,
        error_id: Option<String>,
    ) -> Self {
        let known_tool_names = known_tool_names.into_iter().collect::<Vec<_>>();
        Self {
            sanitizer: ToolTranscriptSanitizer::new(known_tool_names.iter().cloned()),
            thinking_sanitizer: ToolTranscriptSanitizer::new(known_tool_names),
            current_text_index: None,
            current_text_visible: false,
            current_text_suppressed: false,
            buffered_thinking: None,
            fatal: false,
            pending_fatal_error: None,
            request_id,
            error_id,
        }
    }

    fn process(&mut self, event: &[u8]) -> Vec<u8> {
        if self.fatal {
            return Vec::new();
        }
        let Some(mut value) = external_sse_data_value(event) else {
            return event.to_vec();
        };
        let event_type = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();

        if self.buffered_thinking.is_some() {
            match event_type {
                "content_block_delta" => {
                    let delta_type = value
                        .pointer("/delta/type")
                        .and_then(serde_json::Value::as_str);
                    let content = (delta_type == Some("thinking_delta"))
                        .then(|| {
                            value
                                .pointer("/delta/thinking")
                                .and_then(serde_json::Value::as_str)
                        })
                        .flatten();
                    let integrity = (delta_type == Some("signature_delta"))
                        .then(|| {
                            value
                                .pointer("/delta/signature")
                                .and_then(serde_json::Value::as_str)
                        })
                        .flatten();
                    let overflow = {
                        let block = self
                            .buffered_thinking
                            .as_mut()
                            .expect("buffered thinking exists");
                        block.push_event(event);
                        if let Some(content) = content {
                            block.push_content(content);
                        }
                        if let Some(integrity) = integrity {
                            block.push_integrity_value(integrity);
                        }
                        block.overflow
                    };
                    if overflow {
                        self.buffered_thinking = None;
                        return self.fail_atomic_thinking_buffer();
                    }
                    return Vec::new();
                }
                "content_block_stop" => {
                    if let Some(block) = self.buffered_thinking.as_mut() {
                        block.push_event(event);
                    }
                    return self.finish_buffered_thinking(true);
                }
                "content_block_start" | "message_delta" | "message_stop" | "error" => {
                    let mut out = self.finish_buffered_thinking(false);
                    out.extend(self.process(event));
                    return out;
                }
                "ping" => return event.to_vec(),
                _ => {
                    let overflow = if let Some(block) = self.buffered_thinking.as_mut() {
                        block.push_event(event);
                        block.overflow
                    } else {
                        false
                    };
                    if overflow {
                        self.buffered_thinking = None;
                        return self.fail_atomic_thinking_buffer();
                    }
                    return Vec::new();
                }
            }
        }

        match event_type {
            "content_block_start" => {
                let mut out = self.flush_segment(false);
                if self.fatal {
                    return out;
                }
                let block_type = value
                    .pointer("/content_block/type")
                    .and_then(serde_json::Value::as_str);
                self.reset_text_block();
                if block_type == Some("text") {
                    self.current_text_index =
                        value.get("index").and_then(serde_json::Value::as_u64);
                    if let Some(text) = value
                        .pointer("/content_block/text")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                    {
                        let suppressed_before = self.sanitizer.suppressed_blocks();
                        let safe = self.sanitizer.push(&text);
                        if self.sanitizer.suppressed_blocks() > suppressed_before {
                            out.extend(self.fail_protocol_contamination());
                            return out;
                        }
                        self.current_text_visible = !safe.is_empty();
                        if safe != text {
                            value["content_block"]["text"] = serde_json::Value::String(safe);
                            out.extend(rewrite_external_sse_data_value(event, &value));
                            return out;
                        }
                    }
                } else if matches!(block_type, Some("thinking" | "redacted_thinking")) {
                    let kind = if block_type == Some("thinking") {
                        ExternalBufferedThinkingKind::Thinking
                    } else {
                        ExternalBufferedThinkingKind::Redacted
                    };
                    let content = match kind {
                        ExternalBufferedThinkingKind::Thinking => value
                            .pointer("/content_block/thinking")
                            .and_then(serde_json::Value::as_str),
                        ExternalBufferedThinkingKind::Redacted => value
                            .pointer("/content_block/data")
                            .and_then(serde_json::Value::as_str),
                    };
                    let mut buffered = ExternalBufferedThinkingBlock::new(kind, event, content);
                    if let Some(signature) = value
                        .pointer("/content_block/signature")
                        .and_then(serde_json::Value::as_str)
                        .filter(|signature| !signature.is_empty())
                    {
                        buffered.push_integrity_value(signature);
                    }
                    if buffered.overflow {
                        return self.fail_atomic_thinking_buffer();
                    }
                    self.buffered_thinking = Some(buffered);
                    return out;
                }
                out.extend_from_slice(event);
                out
            }
            "content_block_delta"
                if value
                    .pointer("/delta/type")
                    .and_then(serde_json::Value::as_str)
                    == Some("text_delta") =>
            {
                if let Some(index) = value.get("index").and_then(serde_json::Value::as_u64) {
                    self.current_text_index = Some(index);
                }
                let Some(text) = value
                    .pointer("/delta/text")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
                else {
                    return event.to_vec();
                };
                let before = self.sanitizer.suppressed_blocks();
                let safe = self.sanitizer.push(&text);
                let suppressed = self.sanitizer.suppressed_blocks() > before;
                self.current_text_suppressed |= suppressed;
                if suppressed {
                    return self.fail_protocol_contamination();
                }
                if safe.is_empty() {
                    return Vec::new();
                }
                self.current_text_visible = true;
                if safe == text {
                    return event.to_vec();
                }
                value["delta"]["text"] = serde_json::Value::String(safe);
                rewrite_external_sse_data_value(event, &value)
            }
            "content_block_delta" => {
                let mut out = self.flush_segment(false);
                if self.fatal {
                    return out;
                }
                match value
                    .pointer("/delta/type")
                    .and_then(serde_json::Value::as_str)
                {
                    Some("thinking_delta") => {
                        let Some(thinking) = value
                            .pointer("/delta/thinking")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                        else {
                            return out;
                        };
                        let suppressed_before = self.thinking_sanitizer.suppressed_blocks();
                        let mut safe = self.thinking_sanitizer.push(&thinking);
                        safe.push_str(&self.thinking_sanitizer.finish());
                        if self.thinking_sanitizer.suppressed_blocks() > suppressed_before {
                            out.extend(self.fail_protocol_contamination());
                            return out;
                        }
                        if !safe.is_empty() {
                            value["delta"]["thinking"] = serde_json::Value::String(safe);
                            out.extend(rewrite_external_sse_data_value(event, &value));
                        }
                    }
                    Some("signature_delta") => {
                        if let Some(signature) = value
                            .pointer("/delta/signature")
                            .and_then(serde_json::Value::as_str)
                        {
                            let suppressed_before = self.thinking_sanitizer.suppressed_blocks();
                            let _ = self.thinking_sanitizer.push(signature);
                            let _ = self.thinking_sanitizer.finish();
                            if self.thinking_sanitizer.suppressed_blocks() > suppressed_before {
                                out.extend(self.fail_protocol_contamination());
                                return out;
                            }
                        }
                        tracing::warn!("suppressed orphan external signature delta");
                    }
                    _ => out.extend_from_slice(event),
                }
                out
            }
            "content_block_stop" => {
                let mut out = self.flush_segment(false);
                if self.fatal {
                    return out;
                }
                self.reset_text_block();
                out.extend_from_slice(event);
                out
            }
            "message_delta" | "message_stop" | "error" => {
                let mut out = self.flush_segment(true);
                if self.fatal {
                    return out;
                }
                self.reset_text_block();
                out.extend_from_slice(event);
                out
            }
            _ => event.to_vec(),
        }
    }

    fn finish(&mut self) -> Vec<u8> {
        if self.fatal {
            return Vec::new();
        }
        let mut out = self.finish_buffered_thinking(false);
        out.extend(self.flush_segment(true));
        self.reset_text_block();
        out
    }

    fn finish_buffered_thinking(&mut self, complete: bool) -> Vec<u8> {
        let Some(block) = self.buffered_thinking.take() else {
            return Vec::new();
        };
        if block.overflow {
            return self.fail_atomic_thinking_buffer();
        }

        let suppressed_before = self.thinking_sanitizer.suppressed_blocks();
        let _ = self.thinking_sanitizer.push(&block.content);
        let _ = self.thinking_sanitizer.finish();
        for integrity in &block.integrity_values {
            let _ = self.thinking_sanitizer.push(integrity);
            let _ = self.thinking_sanitizer.finish();
        }
        let polluted = self.thinking_sanitizer.suppressed_blocks() > suppressed_before;
        let signed = !block.integrity_values.is_empty();
        if polluted {
            tracing::warn!(
                thinking_block_kind = ?block.kind,
                signed,
                buffered_bytes = block.buffered_bytes,
                "suppressed polluted external thinking block atomically"
            );
            return self.fail_protocol_contamination();
        }
        if !complete || block.events.len() < 2 {
            return Vec::new();
        }
        block.events.concat()
    }

    fn fail_atomic_thinking_buffer(&mut self) -> Vec<u8> {
        const DETAIL: &str = "external thinking block exceeded bounded atomic buffer";
        self.fail_with_processing_error(DETAIL)
    }

    fn fail_protocol_contamination(&mut self) -> Vec<u8> {
        self.fail_with_processing_error(RESPONSE_PROTOCOL_CONTAMINATION_DETAIL)
    }

    fn fail_with_processing_error(&mut self, detail: &str) -> Vec<u8> {
        self.buffered_thinking = None;
        self.fatal = true;
        self.pending_fatal_error = Some(detail.to_string());
        external_safe_processing_error_event(self.request_id.as_deref(), self.error_id.as_deref())
    }

    fn take_pending_fatal_error(&mut self) -> Option<String> {
        self.pending_fatal_error.take()
    }

    fn flush_segment(&mut self, finish: bool) -> Vec<u8> {
        let before = self.sanitizer.suppressed_blocks();
        let pending = if finish {
            self.sanitizer.finish()
        } else {
            self.sanitizer.structured_tool_boundary()
        };
        let suppressed = self.sanitizer.suppressed_blocks() > before;
        self.current_text_suppressed |= suppressed;

        if suppressed {
            return self.fail_protocol_contamination();
        }

        let mut out = Vec::new();
        if !pending.is_empty() {
            if let Some(index) = self.current_text_index {
                out.extend(external_text_delta_event(index, &pending));
                self.current_text_visible = true;
            }
        }
        out
    }

    fn reset_text_block(&mut self) {
        self.current_text_index = None;
        self.current_text_visible = false;
        self.current_text_suppressed = false;
    }
}

fn external_sse_data_value(event: &[u8]) -> Option<serde_json::Value> {
    let text = std::str::from_utf8(event).ok()?;
    let mut data = String::new();
    let mut saw_data = false;
    for line in text.lines() {
        let Some(value) = line.trim_end_matches('\r').strip_prefix("data:") else {
            continue;
        };
        if saw_data {
            data.push('\n');
        }
        saw_data = true;
        data.push_str(value.strip_prefix(' ').unwrap_or(value));
    }
    let data = data.trim();
    (!data.is_empty() && data != "[DONE]")
        .then(|| serde_json::from_str::<serde_json::Value>(data).ok())
        .flatten()
}

fn rewrite_external_sse_data_value(event: &[u8], value: &serde_json::Value) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(event) else {
        return event.to_vec();
    };
    let Ok(serialized) = serde_json::to_string(value) else {
        return event.to_vec();
    };
    let mut replaced = false;
    let mut out = Vec::with_capacity(event.len());
    for line in text.split_inclusive('\n') {
        let trimmed_line_end = line.trim_end_matches(['\r', '\n']);
        let line_ending = &line[trimmed_line_end.len()..];
        let Some(data) = trimmed_line_end.strip_prefix("data:") else {
            out.extend_from_slice(line.as_bytes());
            continue;
        };
        if replaced {
            continue;
        }
        if data.trim().is_empty() || data.trim() == "[DONE]" {
            out.extend_from_slice(line.as_bytes());
            continue;
        }
        let leading_ws_len = data.len().saturating_sub(data.trim_start().len());
        out.extend_from_slice(b"data:");
        out.extend_from_slice(&data.as_bytes()[..leading_ws_len]);
        out.extend_from_slice(serialized.as_bytes());
        out.extend_from_slice(line_ending.as_bytes());
        replaced = true;
    }
    if replaced { out } else { event.to_vec() }
}

fn external_text_delta_event(index: u64, text: &str) -> Vec<u8> {
    let value = serde_json::json!({
        "type": "content_block_delta",
        "index": index,
        "delta": {"type": "text_delta", "text": text},
    });
    format!("event: content_block_delta\ndata: {value}\n\n").into_bytes()
}

fn external_safe_processing_error_event(
    request_id: Option<&str>,
    error_id: Option<&str>,
) -> Vec<u8> {
    let message = error_id
        .map(|error_id| {
            envelope::public_message_with_error_id(
                envelope::PUBLIC_PROCESSING_FAILED_MESSAGE,
                error_id,
            )
        })
        .unwrap_or_else(|| envelope::PUBLIC_PROCESSING_FAILED_MESSAGE.to_string());
    let mut value = serde_json::json!({
        "type": "error",
        "error": {"type": "api_error", "message": message},
    });
    if let Some(request_id) = request_id {
        value["request_id"] = serde_json::Value::String(request_id.to_string());
    }
    format!("event: error\ndata: {value}\n\n").into_bytes()
}

#[cfg(test)]
fn process_sse_event_with_plan(
    event: &[u8],
    projection: Option<&ExternalUsageProjectionContext>,
    capture: Option<&Arc<SyncMutex<ExternalUsageCapture>>>,
    stream_error_mask: Option<&ExternalStreamErrorMask>,
    plan: ExternalStreamProcessingPlan,
) -> Vec<u8> {
    process_sse_event_with_plan_and_transcript(
        event,
        projection,
        capture,
        stream_error_mask,
        plan,
        None,
    )
}

fn process_sse_event_with_plan_and_transcript(
    event: &[u8],
    projection: Option<&ExternalUsageProjectionContext>,
    capture: Option<&Arc<SyncMutex<ExternalUsageCapture>>>,
    stream_error_mask: Option<&ExternalStreamErrorMask>,
    plan: ExternalStreamProcessingPlan,
    transcript_state: Option<&mut ExternalAnthropicTranscriptState>,
) -> Vec<u8> {
    record_external_usage_debug_sse_event(event, capture, plan);
    let masked = plan
        .mask_errors
        .then(|| maybe_mask_external_stream_error_event(event, capture, stream_error_mask))
        .flatten();
    if let Some(masked) = masked {
        return transcript_state
            .map(|state| process_external_transcript_state(state, &masked, capture))
            .unwrap_or(masked);
    }
    let processed = if projection.is_some() {
        rewrite_sse_event_usage(event, projection, capture)
    } else {
        if plan.capture_usage {
            capture_sse_event_usage(event, projection, capture);
        }
        event.to_vec()
    };
    let output = if let Some(state) = transcript_state {
        process_external_transcript_state(state, &processed, capture)
    } else {
        processed
    };
    update_external_usage_capture_output_estimate(
        capture,
        estimate_external_stream_output_tokens(&Bytes::from(output.clone())),
    );
    output
}

fn process_external_transcript_state(
    state: &mut ExternalAnthropicTranscriptState,
    event: &[u8],
    capture: Option<&Arc<SyncMutex<ExternalUsageCapture>>>,
) -> Vec<u8> {
    let output = state.process(event);
    capture_external_transcript_fatal_error(state, capture);
    output
}

fn finish_external_transcript_state(
    state: &mut ExternalAnthropicTranscriptState,
    capture: Option<&Arc<SyncMutex<ExternalUsageCapture>>>,
) -> Vec<u8> {
    let output = state.finish();
    capture_external_transcript_fatal_error(state, capture);
    output
}

fn capture_external_transcript_fatal_error(
    state: &mut ExternalAnthropicTranscriptState,
    capture: Option<&Arc<SyncMutex<ExternalUsageCapture>>>,
) {
    if let Some(detail) = state.take_pending_fatal_error() {
        if let Some(capture) = capture {
            let mut capture = capture.lock();
            if capture.stream_error_message.is_none() {
                capture.stream_error_message = Some(detail);
            }
        }
    }
}

#[cfg(test)]
fn drain_sse_events(
    buffer: &mut Vec<u8>,
    projection: Option<&ExternalUsageProjectionContext>,
    capture: Option<&Arc<SyncMutex<ExternalUsageCapture>>>,
    stream_error_mask: Option<&ExternalStreamErrorMask>,
    plan: ExternalStreamProcessingPlan,
) -> Vec<u8> {
    drain_sse_events_with_transcript(buffer, projection, capture, stream_error_mask, plan, None)
}

fn drain_sse_events_with_transcript(
    buffer: &mut Vec<u8>,
    projection: Option<&ExternalUsageProjectionContext>,
    capture: Option<&Arc<SyncMutex<ExternalUsageCapture>>>,
    stream_error_mask: Option<&ExternalStreamErrorMask>,
    plan: ExternalStreamProcessingPlan,
    mut transcript_state: Option<&mut ExternalAnthropicTranscriptState>,
) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some((idx, delimiter_len)) = find_sse_event_delimiter(buffer) {
        let end = idx + delimiter_len;
        let event = buffer.drain(..end).collect::<Vec<u8>>();
        out.extend(process_sse_event_with_plan_and_transcript(
            &event,
            projection,
            capture,
            stream_error_mask,
            plan,
            transcript_state.as_deref_mut(),
        ));
    }
    out
}

/*
 * The wrappers above keep the existing unit-test surface for usage-only processing while the live
 * external route passes a request-scoped transcript state through the extended functions.
 */
#[allow(dead_code)]
fn capture_sse_usage_without_rewrite(
    event: &[u8],
    projection: Option<&ExternalUsageProjectionContext>,
    capture: Option<&Arc<SyncMutex<ExternalUsageCapture>>>,
    plan: ExternalStreamProcessingPlan,
) {
    if plan.capture_usage {
        capture_sse_event_usage(event, projection, capture);
    }
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

    if let Some(capture) = capture {
        let mut capture = capture.lock();
        if capture.stream_error_message.is_none() {
            capture.stream_error_message = Some(SAFE_EXTERNAL_STREAM_ERROR_EVENT.to_string());
        }
    }
    tracing::warn!(
        request_id = %mask.request_id,
        error_id = %mask.error_id,
        pool_id = mask.pool_id,
        pool_name = %mask.pool_name,
        error_class = "external_stream_error_event",
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

fn update_external_usage_capture_output_estimate(
    capture: Option<&Arc<SyncMutex<ExternalUsageCapture>>>,
    output_tokens: i32,
) {
    if output_tokens <= 0 {
        return;
    }
    let Some(capture) = capture else {
        return;
    };
    let mut capture = capture.lock();
    capture.estimated_output_tokens = capture
        .estimated_output_tokens
        .saturating_add(output_tokens.max(0));
}

fn external_usage_capture_output_estimate(
    capture: Option<&Arc<SyncMutex<ExternalUsageCapture>>>,
) -> Option<i32> {
    let output_tokens = capture?.lock().estimated_output_tokens.max(0);
    (output_tokens > 0).then_some(output_tokens)
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
    response_output_estimate: Option<i32>,
) -> Option<ProjectedExternalUsage> {
    let projection = projection?;
    if projection.mode != ExternalPoolUsageProjectionMode::CurrentPathPolicy {
        return None;
    }
    let upstream_raw_usage = cache_usage_from_any_value(usage);
    let response_output_estimate = response_output_estimate.unwrap_or(0).max(0);
    let output_tokens = upstream_raw_usage
        .map(|usage| usage.output_tokens.max(response_output_estimate))
        .unwrap_or_else(|| usage_i32(usage, "output_tokens").max(response_output_estimate));
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
            policy.apply_final_standard_cache_guards(shaped)
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
        .map(|policy| policy.apply_final_standard_cache_guards(projected))
        .unwrap_or(projected);
    let projected = projection
        .reported_policy
        .clone()
        .map(|policy| policy.apply_final_output_guard_to_usage(projected))
        .unwrap_or(projected);
    let parsed_or_estimated_raw = upstream_raw_usage.unwrap_or(CacheUsage {
        total_input_tokens: projection.raw_input_tokens,
        input_tokens: projection.raw_input_tokens,
        output_tokens,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        cache_creation_5m_input_tokens: 0,
        cache_creation_1h_input_tokens: 0,
    });
    let raw_for_floor = effective_external_raw_usage_for_estimate_repair(
        parsed_or_estimated_raw,
        projection.raw_input_tokens,
        response_output_estimate,
    );
    let projected = apply_external_pool_usage_cost_floor(projected, raw_for_floor, projection);
    let projected_json = projected.to_anthropic_usage_json();
    let obj = usage.as_object_mut()?;
    let projected_obj = projected_json.as_object()?;
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

fn effective_external_raw_usage_for_estimate_repair(
    raw: CacheUsage,
    request_input_tokens: i32,
    response_output_estimate: i32,
) -> CacheUsage {
    let request_input_tokens = request_input_tokens.max(0);
    let response_output_estimate = response_output_estimate.max(0);
    let raw_total = raw
        .input_tokens
        .max(0)
        .saturating_add(raw.cache_read_input_tokens.max(0))
        .saturating_add(raw.cache_creation_input_tokens.max(0));
    let raw_has_cache = raw.cache_read_input_tokens > 0 || raw.cache_creation_input_tokens > 0;
    let suspicious_small_input = !raw_has_cache
        && request_input_tokens >= 512
        && (raw_total <= 1 || i64::from(raw_total) * 20 < i64::from(request_input_tokens));

    if !suspicious_small_input && raw.output_tokens >= response_output_estimate {
        return raw;
    }

    let input_tokens = if suspicious_small_input {
        request_input_tokens.max(raw.input_tokens.max(0))
    } else {
        raw.input_tokens.max(0)
    };
    let output_tokens = raw.output_tokens.max(response_output_estimate);

    CacheUsage {
        total_input_tokens: input_tokens
            .saturating_add(raw.cache_read_input_tokens.max(0))
            .saturating_add(raw.cache_creation_input_tokens.max(0)),
        input_tokens,
        output_tokens,
        cache_creation_input_tokens: raw.cache_creation_input_tokens.max(0),
        cache_read_input_tokens: raw.cache_read_input_tokens.max(0),
        cache_creation_5m_input_tokens: raw.cache_creation_5m_input_tokens.max(0),
        cache_creation_1h_input_tokens: raw.cache_creation_1h_input_tokens.max(0),
    }
}

fn apply_external_pool_usage_final_path_guards(
    usage: CacheUsage,
    projection: &ExternalUsageProjectionContext,
) -> CacheUsage {
    let Some(policy) = projection.reported_policy.clone() else {
        return usage;
    };
    let usage = policy.apply_final_input_guard(usage);
    let usage = policy.apply_final_standard_cache_guards(usage);
    policy.apply_final_output_guard_to_usage(usage)
}

fn apply_external_pool_usage_cost_floor(
    reported: CacheUsage,
    raw: CacheUsage,
    projection: &ExternalUsageProjectionContext,
) -> CacheUsage {
    if !projection.cost_floor_enabled {
        return reported;
    }

    let raw_estimate = projection.pricing_catalog.estimate(&projection.model, raw);
    let reported_estimate = projection
        .pricing_catalog
        .estimate(&projection.model, reported);
    if !raw_estimate.available || !reported_estimate.available {
        return reported;
    }

    let target_cost = external_usage_cost_floor_target_cost(
        raw_estimate.cost_usd,
        projection.cost_floor_margin_percent,
    );
    let current_cost = reported_estimate.cost_usd.max(0.0);
    if current_cost + f64::EPSILON >= target_cost {
        return reported;
    }

    let mut candidate = reported;
    let raw_has_cache_creation = raw.cache_creation_input_tokens > 0;
    let raw_has_cache_read = raw.cache_read_input_tokens > 0;
    if external_usage_cache_repair_allowed(projection)
        && (raw_has_cache_creation || raw_has_cache_read)
    {
        if raw_has_cache_creation {
            candidate = repair_external_usage_cost_floor_field(
                candidate,
                raw,
                projection,
                target_cost,
                ExternalUsageRepairField::CacheCreation,
            );
            if external_usage_cost_covers(candidate, projection, target_cost) {
                return candidate;
            }
        }
        candidate = repair_external_usage_cost_floor_field(
            candidate,
            raw,
            projection,
            target_cost,
            ExternalUsageRepairField::CacheRead,
        );
        return external_usage_best_improved_candidate(
            reported,
            candidate,
            projection,
            current_cost,
        );
    }

    if raw_has_cache_creation || raw_has_cache_read {
        return reported;
    }

    candidate = repair_external_usage_cost_floor_field(
        candidate,
        raw,
        projection,
        target_cost,
        ExternalUsageRepairField::Input,
    );
    external_usage_best_improved_candidate(reported, candidate, projection, current_cost)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExternalUsageRepairField {
    Input,
    CacheCreation,
    CacheRead,
}

fn external_usage_cost_floor_target_cost(raw_cost_usd: f64, margin_percent: u32) -> f64 {
    let margin_percent = margin_percent.min(200) as f64;
    raw_cost_usd.max(0.0) * (1.0 + margin_percent / 100.0)
}

fn external_usage_cache_repair_allowed(projection: &ExternalUsageProjectionContext) -> bool {
    projection.cache_state_enabled && projection.reported_policy.is_some()
}

fn repair_external_usage_cost_floor_field(
    usage: CacheUsage,
    raw: CacheUsage,
    projection: &ExternalUsageProjectionContext,
    target_cost: f64,
    field: ExternalUsageRepairField,
) -> CacheUsage {
    let current_cost = match external_usage_cost_usd(usage, projection) {
        Some(cost) => cost,
        None => return usage,
    };
    if current_cost + f64::EPSILON >= target_cost {
        return usage;
    }

    let Some(unit_cost) = external_usage_effective_repair_unit_cost(usage, raw, projection, field)
        .or_else(|| external_usage_repair_unit_cost(usage, raw, projection, field))
        .filter(|cost| *cost > 0.0 && cost.is_finite())
    else {
        return usage;
    };

    // Repair each field at most once, then re-apply path guards. This keeps the
    // compensation bounded by the route policy instead of repeatedly chasing a
    // target through jittered caps.
    let missing_cost = target_cost - current_cost;
    let repair_tokens = (missing_cost / unit_cost)
        .ceil()
        .clamp(1.0, i32::MAX as f64) as i32;
    let candidate = apply_external_pool_usage_final_path_guards(
        add_external_usage_repair_tokens(usage, raw, repair_tokens, field),
        projection,
    );
    external_usage_best_improved_candidate(usage, candidate, projection, current_cost)
}

fn external_usage_cost_covers(
    usage: CacheUsage,
    projection: &ExternalUsageProjectionContext,
    target_cost: f64,
) -> bool {
    external_usage_cost_usd(usage, projection)
        .map(|cost| cost + f64::EPSILON >= target_cost)
        .unwrap_or(false)
}

fn external_usage_best_improved_candidate(
    base: CacheUsage,
    candidate: CacheUsage,
    projection: &ExternalUsageProjectionContext,
    base_cost: f64,
) -> CacheUsage {
    let Some(candidate_cost) = external_usage_cost_usd(candidate, projection) else {
        return base;
    };
    if candidate_cost <= base_cost + f64::EPSILON {
        return base;
    }
    candidate
}

fn external_usage_cost_usd(
    usage: CacheUsage,
    projection: &ExternalUsageProjectionContext,
) -> Option<f64> {
    let estimate = projection
        .pricing_catalog
        .estimate(&projection.model, usage);
    estimate.available.then_some(estimate.cost_usd.max(0.0))
}

fn add_external_usage_repair_tokens(
    usage: CacheUsage,
    raw: CacheUsage,
    tokens: i32,
    field: ExternalUsageRepairField,
) -> CacheUsage {
    match field {
        ExternalUsageRepairField::Input => add_input_repair_tokens(usage, tokens),
        ExternalUsageRepairField::CacheCreation => {
            add_cache_creation_repair_tokens(usage, raw, tokens)
        }
        ExternalUsageRepairField::CacheRead => add_cache_read_repair_tokens(usage, tokens),
    }
}

fn add_input_repair_tokens(mut usage: CacheUsage, tokens: i32) -> CacheUsage {
    usage.input_tokens = usage.input_tokens.max(0).saturating_add(tokens.max(0));
    update_usage_total_input_tokens(&mut usage);
    usage
}

fn add_cache_read_repair_tokens(mut usage: CacheUsage, tokens: i32) -> CacheUsage {
    usage.cache_read_input_tokens = usage
        .cache_read_input_tokens
        .max(0)
        .saturating_add(tokens.max(0));
    update_usage_total_input_tokens(&mut usage);
    usage
}

fn add_cache_creation_repair_tokens(
    mut usage: CacheUsage,
    raw: CacheUsage,
    tokens: i32,
) -> CacheUsage {
    let tokens = tokens.max(0);
    usage.cache_creation_input_tokens = usage
        .cache_creation_input_tokens
        .max(0)
        .saturating_add(tokens);
    let (additional_5m, additional_1h) = split_cache_creation_repair_tokens(tokens, raw);
    usage.cache_creation_5m_input_tokens = usage
        .cache_creation_5m_input_tokens
        .max(0)
        .saturating_add(additional_5m)
        .min(usage.cache_creation_input_tokens);
    usage.cache_creation_1h_input_tokens = usage
        .cache_creation_1h_input_tokens
        .max(0)
        .saturating_add(additional_1h)
        .min(
            usage
                .cache_creation_input_tokens
                .saturating_sub(usage.cache_creation_5m_input_tokens),
        );
    update_usage_total_input_tokens(&mut usage);
    usage
}

fn split_cache_creation_repair_tokens(tokens: i32, raw: CacheUsage) -> (i32, i32) {
    let tokens = tokens.max(0);
    if tokens == 0 {
        return (0, 0);
    }
    let raw_5m = raw.cache_creation_5m_input_tokens.max(0);
    let raw_1h = raw.cache_creation_1h_input_tokens.max(0);
    let raw_split_total = raw_5m.saturating_add(raw_1h);
    if raw_split_total <= 0 {
        return (tokens, 0);
    }
    let one_hour =
        ((tokens as i64 * raw_1h as i64) + (raw_split_total as i64 / 2)) / raw_split_total as i64;
    let one_hour = one_hour.clamp(0, tokens as i64) as i32;
    (tokens.saturating_sub(one_hour), one_hour)
}

fn update_usage_total_input_tokens(usage: &mut CacheUsage) {
    usage.total_input_tokens = usage
        .input_tokens
        .max(0)
        .saturating_add(usage.cache_read_input_tokens.max(0))
        .saturating_add(usage.cache_creation_input_tokens.max(0));
}

fn external_usage_effective_repair_unit_cost(
    usage: CacheUsage,
    raw: CacheUsage,
    projection: &ExternalUsageProjectionContext,
    field: ExternalUsageRepairField,
) -> Option<f64> {
    let base = projection
        .pricing_catalog
        .estimate(&projection.model, usage);
    if !base.available {
        return None;
    }

    let next = apply_external_pool_usage_final_path_guards(
        add_external_usage_repair_tokens(usage, raw, 1, field),
        projection,
    );
    let estimate = projection.pricing_catalog.estimate(&projection.model, next);
    if !estimate.available {
        return None;
    }
    let delta = estimate.cost_usd - base.cost_usd;
    (delta > 0.0 && delta.is_finite()).then_some(delta)
}

fn external_usage_repair_unit_cost(
    usage: CacheUsage,
    raw: CacheUsage,
    projection: &ExternalUsageProjectionContext,
    field: ExternalUsageRepairField,
) -> Option<f64> {
    let base = projection
        .pricing_catalog
        .estimate(&projection.model, usage);
    if !base.available {
        return None;
    }

    let next = add_external_usage_repair_tokens(usage, raw, 1, field);
    let estimate = projection.pricing_catalog.estimate(&projection.model, next);
    if !estimate.available {
        return None;
    }
    Some((estimate.cost_usd - base.cost_usd).max(0.0))
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
    billing.usage_estimated = capture.usage_estimated;
    billing.usage_estimate_reason = capture.usage_estimate_reason;
    billing.usage_candidate_path = capture.usage_candidate_path;
    billing.body_usage_projection_applied =
        capture.body_usage_projection_applied || capture.projected;
    Some(billing)
}

fn external_pool_billing_from_capture_ref(
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
    capture: &Arc<SyncMutex<ExternalUsageCapture>>,
) -> Option<ExternalPoolBilling> {
    external_pool_billing_from_capture(route, pool, capture.lock().clone())
}

fn external_pool_billing_from_stream_estimate(
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
    capture: &Arc<SyncMutex<ExternalUsageCapture>>,
    projection: Option<&ExternalUsageProjectionContext>,
    estimated_output_tokens: i32,
) -> Option<ExternalPoolBilling> {
    let capture = capture.lock().clone();
    let request_input_tokens = capture
        .request_input_tokens
        .unwrap_or_else(|| estimated_external_request_input_tokens(route, projection));
    if request_input_tokens <= 0 && estimated_output_tokens <= 0 {
        return None;
    }

    let estimated = estimated_external_usage_from_parts(
        request_input_tokens,
        estimated_output_tokens,
        projection,
        true,
    );
    let mut billing = external_pool_billing(
        route,
        pool,
        Some(estimated.request_input_tokens),
        estimated.raw,
        estimated.shaped,
        estimated.reported,
        estimated.projected,
    );
    billing.stream_response_mode = capture
        .stream_response_mode
        .map(|mode| mode.as_str().to_string());
    billing.usage_estimated = true;
    billing.usage_estimate_reason = Some("missing_stream_usage".to_string());
    billing.usage_candidate_path = Some("$stream.estimated".to_string());
    billing.body_usage_projection_applied = estimated.projected;
    Some(billing)
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
        usage_estimated: false,
        usage_estimate_reason: None,
        usage_candidate_path: None,
        body_usage_projection_applied: usage_projection_applied,
    }
}

fn build_external_usage_projection_context(
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
    uplift_percent: u32,
    cost_floor_enabled: bool,
    cost_floor_margin_percent: u32,
    output_uplift_min_tokens: i32,
    output_uplift_percent: u32,
) -> Option<ExternalUsageProjectionContext> {
    usage_projection::build_context(
        route,
        pool,
        uplift_percent,
        cost_floor_enabled,
        cost_floor_margin_percent,
        output_uplift_min_tokens,
        output_uplift_percent,
    )
}

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
