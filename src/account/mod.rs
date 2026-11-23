use crate::common::upstream_usage::{RawUsageSource, UpstreamUsage};
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
            PayloadByteBreakdown, PayloadGuardConfig, PayloadGuardReport,
            breakdown_anthropic_messages_request,
        },
        pricing::PricingCatalog,
        prompt_cache::{
            PrefixToolPromptCachePlan, PromptCacheBounds, PromptCacheProfile, PromptCacheScope,
            PromptCacheTracker, StableSegmentPromptCachePlan,
        },
        prompt_cache_creation_control::PromptCacheCreationController,
        request_facts::{
            RawMessagesBodyProbe, deserialize_messages_request_with_probe, probe_raw_messages_body,
        },
        transcript_sanitizer::{
            RESPONSE_PROTOCOL_CONTAMINATION_DETAIL, ToolTranscriptSanitizer,
            collect_known_tool_names_from_request, sanitize_response_content,
        },
        types::MessagesRequest,
        usage::{
            AccountAttempt, AccountBilling, AccountUsageSnapshot, UsageLatencyTrace,
            UsagePublicError, UsageRecord, UsageRecordStatus, UsageRouteKind, UsageRouteSubtype,
            UsageSource,
        },
    },
    common::storage_task::{spawn_best_effort_storage_task, spawn_critical_storage_task},
    common::{
        capacity_signal::{CapacitySignal, CapacityWaiter},
        upstream_error::RawUpstreamError,
    },
    http_client::{HttpSendError, response_bytes_with_limit_and_body_timeout},
    model::config::{
        AccountsConfig, CapacityMode, ModelMappingRule, ModelUnavailableCooldownMode,
        PrefixToolCachePolicy, PromptCacheCreationControlConfig, PromptCacheSimulationMode,
        PromptCacheStrategyType, ReportedUsageConfig, RouteRuleMode, StableSegmentCachePolicy,
        StreamResponseMode, normalize_route_rules, route_rule_matches,
    },
    model::model_processing::{
        ModelProcessingConfig, ModelProcessingError, ModelProcessingInput, ModelProcessingMode,
        process_model,
    },
    model::model_support::{expand_claude_supported_model_variants, normalize_model_id},
    storage::{
        account_events::{
            AccountEvent, AccountEventKind, AccountEventThrottle, AccountSource,
            account_disable_event, record_account_event,
        },
        postgres::{AccountQualitySampleRow, PostgresStore},
        redis_cache::{
            AccountCoordinatorGuardError, AccountCoordinatorGuardState, AccountCoordinatorSnapshot,
            AccountCoordinatorSnapshotRequest,
            AccountLeaseAcquireResult as RedisAccountLeaseAcquireResult,
            AccountLeaseReleaseRequest, AccountQualityState, RedisStore, account_quality_key,
        },
    },
    token,
};

mod body_pipeline;
mod early_commit;
mod model_pipeline;
mod response_model;
mod retry_pipeline;
mod usage_projection;
mod zero_usage_repair;

use early_commit::EarlyCommitTaskGuard;
pub use early_commit::ExternalEarlyCommitDownstream;
use early_commit::{EarlyCommitResponseReady, EarlyCommitTail, early_commit_response};
use usage_projection::ExternalUsageProjectionContext;

#[cfg(test)]
use crate::anthropic::request_facts::{raw_messages_body_hints, rewrite_raw_top_level_model};

const DEFAULT_ACCOUNT_REQUEST_TIMEOUT_SECS: u64 = 180;
const ACCOUNT_LEASE_TOUCH_INTERVAL_SECS: u64 = 30;
/// Extra pre-output failovers allowed after the shared dispatch deadline has passed.
const ACCOUNT_PRE_OUTPUT_FAILOVERS_PAST_DEADLINE: u32 = 2;
const ACCOUNT_LEASE_MAX_AGE_SECS: u64 = DEFAULT_ACCOUNT_REQUEST_TIMEOUT_SECS * 2;
const ACCOUNT_LEASE_HEARTBEAT_RETRY_MILLIS: u64 = 250;
const ACCOUNT_QUEUE_LEASE_SAFETY_MARGIN_SECS: u64 = 60;
const ACCOUNT_QUEUE_LEASE_RENEW_INTERVAL_MAX_SECS: u64 = 20;
const ACCOUNT_QUEUE_REDIS_OPERATION_TIMEOUT: Duration = Duration::from_secs(2);
const ACCOUNT_COORDINATOR_REDIS_OPERATION_TIMEOUT: Duration = Duration::from_secs(2);
const ACCOUNT_QUEUE_REDIS_RETRY_DELAY: Duration = Duration::from_millis(50);
const ACCOUNT_RELEASE_FALLBACK_MAX_IN_FLIGHT: usize = 64;
const ACCOUNT_RELEASE_BATCH_SIZE: usize = 256;
const ACCOUNT_RELEASE_CAPACITY: usize = 65_536;
const ACCOUNT_RELEASE_RETRY_MAX_DELAY: Duration = Duration::from_secs(30);
const ACCOUNT_RETRY_AFTER_MAX_SECS: u64 = 7 * 24 * 60 * 60;
const ACCOUNT_COORDINATOR_MAX_IN_FLIGHT: usize = 256;
const ACCOUNT_COORDINATOR_BREAKER_BACKOFF_SECS: [u64; 6] = [1, 2, 4, 8, 16, 30];
const ACCOUNT_COORDINATOR_RECOVERY_GRACE_SECS: u64 = 35;
const ACCOUNT_COORDINATOR_RUN_ID_PROBE_INTERVAL_SECS: u64 = 5;
const ACCOUNT_COORDINATOR_RUNTIME_CONFIG_ID: &str = "account_redis_coordination_epoch";
const ACCOUNT_COORDINATOR_POSTGRES_LOCK_ID: i64 = 0x4b49_524f_4558_5450;
/// 一次 epoch 对齐（PG 事务锁 + Redis 安装）允许的总时长。超时后事务随 future 一起丢弃并回滚，
/// 事务级 advisory lock 随之释放，不会像会话锁那样跟着连接泄漏回池。
const ACCOUNT_COORDINATOR_RECONCILE_TIMEOUT: Duration = Duration::from_secs(10);
/// 等待其它实例释放 epoch 对齐锁的上限；拿不到就本轮放弃，调度走 CoordinatorUnavailable 分支。
const ACCOUNT_COORDINATOR_POSTGRES_LOCK_TIMEOUT: Duration = Duration::from_secs(2);
const ACCOUNT_COORDINATOR_POSTGRES_STATEMENT_TIMEOUT: Duration = Duration::from_secs(5);
const ACCOUNT_USAGE_DEBUG_DEFAULT_DIR: &str = "/tmp/claude-pool/account-usage-debug";
const ACCOUNT_USAGE_DEBUG_DEFAULT_MAX_BODY_BYTES: usize = 8 * 1024;
const ACCOUNT_USAGE_DEBUG_MAX_BODY_BYTES: usize = 1024 * 1024;
const ACCOUNT_USAGE_DEBUG_DEFAULT_MAX_FILES: u64 = 1_000;
const ACCOUNT_USAGE_DEBUG_USAGE_SAMPLE_LIMIT: usize = 20;
const ACCOUNT_USAGE_DEBUG_EVENT_SAMPLE_BYTES: usize = 2 * 1024;
const ACCOUNT_USAGE_DEBUG_USAGE_JSON_BYTES: usize = 4 * 1024;
static ACCOUNT_USAGE_DEBUG_WRITE_COUNT: AtomicU64 = AtomicU64::new(0);
const ACCOUNT_WIRE_DEBUG_DEFAULT_DIR: &str = "/tmp/claude-pool/account-wire-debug";
const ACCOUNT_WIRE_DEBUG_DEFAULT_MAX_BODY_BYTES: usize = 8 * 1024;
const ACCOUNT_WIRE_DEBUG_MAX_BODY_BYTES: usize = 1024 * 1024;
const ACCOUNT_WIRE_DEBUG_DEFAULT_MAX_FILES: u64 = 1_000;
const ACCOUNT_WIRE_DEBUG_TEXT_PREVIEW_CHARS: usize = 160;
const ACCOUNT_WIRE_DEBUG_LAST_MESSAGE_LIMIT: usize = 8;
const ACCOUNT_WIRE_DEBUG_TEXT_SAMPLE_LIMIT: usize = 4;
const ACCOUNT_WIRE_DEBUG_USAGE_SAMPLE_LIMIT: usize = 20;
const ACCOUNT_WIRE_DEBUG_EVENT_SAMPLE_BYTES: usize = 2 * 1024;
static ACCOUNT_WIRE_DEBUG_WRITE_COUNT: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
const ACCOUNT_AVAILABILITY_CACHE_TTL: Duration = Duration::from_millis(250);
const ACCOUNT_STATIC_SNAPSHOT_FRESH_TTL: Duration = Duration::from_secs(5);
const ACCOUNT_STATIC_SNAPSHOT_STALE_TTL: Duration = Duration::from_secs(30);
const ACCOUNT_STATIC_SNAPSHOT_FAILURE_RETRY: Duration = Duration::from_secs(1);
const ACCOUNT_STATIC_SNAPSHOT_REFRESH_TIMEOUT: Duration = Duration::from_millis(500);
const ACCOUNT_SELECTION_RUNTIME_SNAPSHOT_TTL: Duration = Duration::from_millis(100);
const ACCOUNT_SELECTION_POSTGRES_TIMEOUT: Duration = Duration::from_secs(2);
const ACCOUNT_AUTHORITATIVE_SNAPSHOT_TTL: Duration = Duration::from_millis(250);
const ACCOUNT_AUTHORITATIVE_SNAPSHOT_STALE_TTL: Duration = Duration::from_secs(30);
const ACCOUNT_AUTHORITATIVE_SNAPSHOT_FAILURE_RETRY: Duration = Duration::from_millis(100);
const ACCOUNT_AUTHORITATIVE_SNAPSHOT_WAIT_TIMEOUT: Duration = Duration::from_millis(2_250);
const ACCOUNT_SELECTION_MAX_IN_FLIGHT: usize = 32;
const ACCOUNT_SELECTION_BREAKER_BACKOFF_MILLIS: [u64; 4] = [100, 250, 500, 1_000];
const ACCOUNT_SELECTION_BREAKER_JITTER_PERCENT: u8 = 20;
const ACCOUNT_SELECTION_RETRY_AFTER: Duration = Duration::from_millis(250);
const ACCOUNT_DISPATCH_FENCE_TIMEOUT: Duration = Duration::from_millis(500);
const ACCOUNT_DISPATCH_FENCE_SHARED_WAIT_TIMEOUT: Duration = Duration::from_millis(600);
const ACCOUNT_LOCAL_MUTATION_AUTHORITATIVE_FRESH_TTL: Duration = Duration::from_secs(5);
const ACCOUNT_LOCAL_MUTATION_DISPATCH_TRUST_TTL: Duration = Duration::from_secs(30);
const ACCOUNT_AUTO_DISABLE_REDIS_TIMEOUT: Duration = Duration::from_millis(500);
const ACCOUNT_AUTO_DISABLE_POSTGRES_TIMEOUT: Duration = Duration::from_millis(500);
const ACCOUNT_AUTO_DISABLE_TRANSITION_CLAIM_TTL_SECS: usize = 5;
const ACCOUNT_SUCCESS_RESET_COALESCE_WINDOW: Duration = Duration::from_secs(1);
const ACCOUNT_SUCCESS_RESET_REDIS_TIMEOUT: Duration = Duration::from_millis(500);
/// Cooldown markers are written on the failover path, between one account failing and the
/// next being tried. They are a ranking hint, not a correctness requirement, so a slow Redis
/// must not hold the request: give up after this and move on to the next account.
const ACCOUNT_COOLDOWN_REDIS_TIMEOUT: Duration = Duration::from_millis(500);
/// 质量样本写入超时。质量数据是调度优化项，写入慢时直接放弃本次样本，
/// 绝不允许拖住后台任务或影响主请求。
const ACCOUNT_QUALITY_SAMPLE_REDIS_TIMEOUT: Duration = Duration::from_millis(500);
const ACCOUNT_QUALITY_SAMPLE_MAX_TASKS: usize = 32;
const ACCOUNT_SUCCESS_RESET_MAX_TASKS: usize = 64;
const ACCOUNT_SUCCESS_RESET_MAX_TRACKED_POOLS: usize = 4_096;
const ACCOUNT_TRANSIENT_FAILURE_WINDOW_SECS: usize = 30;
const ACCOUNT_STATIC_SNAPSHOT_JITTER_PERCENT: u8 = 10;
const MAX_RECORDED_EXTERNAL_ERROR_MESSAGE_BYTES: usize = 8192;
const ACCOUNT_MAX_SSE_EVENT_BUFFER_BYTES: usize = 1024 * 1024;
const ACCOUNT_STREAM_KEEPALIVE_INTERVAL_SECS: u64 = 5;
const ACCOUNT_ERROR_RESPONSE_MAX_BYTES: usize = 1024 * 1024;
const ACCOUNT_NON_STREAM_RESPONSE_MAX_BYTES: usize = 16 * 1024 * 1024;
const ACCOUNT_DEFAULT_RESPONSE_BODY_TIMEOUT_SECS: u64 = 180;
const SAFE_EXTERNAL_STREAM_ERROR_EVENT: &str = "external upstream emitted an error event";
const ACCOUNT_AUTO_DISABLE_REASONS: &[&str] = &[
    "auth_error",
    "security_lock",
    "quota_exhausted",
    "misconfigured_endpoint",
    "channel_disabled",
];

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AccountAuthType {
    Bearer,
    XApiKey,
}

impl Default for AccountAuthType {
    fn default() -> Self {
        Self::Bearer
    }
}

impl AccountAuthType {
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AccountHeaderProfile {
    #[default]
    Generic,
    AnthropicPassthrough,
    ClaudeCodeMimic,
}

impl AccountHeaderProfile {
    pub fn parse(value: &str) -> Self {
        Self::parse_known(value).unwrap_or_default()
    }

    pub(crate) fn parse_known(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "generic" | "default" => Some(Self::Generic),
            "anthropic_passthrough" | "anthropic" | "claude_passthrough" => {
                Some(Self::AnthropicPassthrough)
            }
            "claude_code_mimic" | "claude-code-mimic" | "claude_mimic" => {
                Some(Self::ClaudeCodeMimic)
            }
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Generic => "generic",
            Self::AnthropicPassthrough => "anthropic_passthrough",
            Self::ClaudeCodeMimic => "claude_code_mimic",
        }
    }

    fn uses_anthropic_header_policy(self) -> bool {
        matches!(self, Self::AnthropicPassthrough | Self::ClaudeCodeMimic)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AccountWireProfile {
    #[default]
    Default,
    Http1TitleCase,
}

impl AccountWireProfile {
    pub fn parse(value: &str) -> Self {
        Self::parse_known(value).unwrap_or_default()
    }

    pub(crate) fn parse_known(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "default" | "generic" => Some(Self::Default),
            "http1_title_case" | "http1-title-case" | "claude_code_http1" => {
                Some(Self::Http1TitleCase)
            }
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Http1TitleCase => "http1_title_case",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AccountTlsProfile {
    #[default]
    Default,
    NativeTls,
}

impl AccountTlsProfile {
    pub fn parse(value: &str) -> Self {
        Self::parse_known(value).unwrap_or_default()
    }

    pub(crate) fn parse_known(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "default" | "rustls" => Some(Self::Default),
            "native_tls" | "native-tls" | "platform_tls" | "platform-tls" => Some(Self::NativeTls),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::NativeTls => "native_tls",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UsageProjectionMode {
    PassThrough,
    CurrentPathPolicy,
}

impl Default for UsageProjectionMode {
    fn default() -> Self {
        Self::CurrentPathPolicy
    }
}

impl UsageProjectionMode {
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
pub enum AutoDisablePolicy {
    Inherit,
    Disabled,
    Enabled,
}

impl Default for AutoDisablePolicy {
    fn default() -> Self {
        Self::Inherit
    }
}

impl AutoDisablePolicy {
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
pub enum StreamRetryMode {
    #[default]
    Inherit,
    Enabled,
    Disabled,
    /// 立即把上游 message_start（usage 清零）和 ping 发给下游；首个内容前出错仍换池重放，
    /// 重放成功时丢弃新一次的 message_start，全部失败时以 SSE error 事件收尾。
    EarlyCommit,
}

impl StreamRetryMode {
    pub fn parse(value: &str) -> Self {
        Self::parse_known(value).unwrap_or_default()
    }

    pub(crate) fn parse_known(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "inherit" => Some(Self::Inherit),
            "enabled" => Some(Self::Enabled),
            "disabled" => Some(Self::Disabled),
            "early_commit" => Some(Self::EarlyCommit),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inherit => "inherit",
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
            Self::EarlyCommit => "early_commit",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelMappingMode {
    Passthrough,
    PassthroughMapping,
    DirectMapping,
    ProcessedMapping,
}

impl Default for ModelMappingMode {
    fn default() -> Self {
        Self::PassthroughMapping
    }
}

impl ModelMappingMode {
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

#[derive(Default, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountOverrides {
    pub request_timeout_secs: Option<u64>,
    pub stream_request_timeout_secs: Option<u64>,
    pub stream_idle_timeout_secs: Option<u64>,
    pub stream_first_output_timeout_secs: Option<u64>,
    pub usage_projection_uplift_percent: Option<u32>,
    pub usage_projection_cost_floor_enabled: Option<bool>,
    pub usage_projection_cost_floor_margin_percent: Option<u32>,
    pub usage_projection_output_uplift_min_tokens: Option<i32>,
    pub usage_projection_output_uplift_percent: Option<u32>,
}
impl AccountOverrides {
    pub fn merge(&self, config: &AccountsConfig) -> AccountsConfig {
        let mut effective = config.clone();
        effective.request_timeout_secs = self
            .request_timeout_secs
            .unwrap_or(config.request_timeout_secs);
        effective.stream_request_timeout_secs = self
            .stream_request_timeout_secs
            .unwrap_or(config.stream_request_timeout_secs);
        effective.stream_idle_timeout_secs = self
            .stream_idle_timeout_secs
            .unwrap_or(config.stream_idle_timeout_secs);
        effective.stream_first_output_timeout_secs = self
            .stream_first_output_timeout_secs
            .unwrap_or(config.stream_first_output_timeout_secs);
        effective.usage_projection_uplift_percent = self
            .usage_projection_uplift_percent
            .unwrap_or(config.usage_projection_uplift_percent);
        effective.usage_projection_cost_floor_enabled = self
            .usage_projection_cost_floor_enabled
            .unwrap_or(config.usage_projection_cost_floor_enabled);
        effective.usage_projection_cost_floor_margin_percent = self
            .usage_projection_cost_floor_margin_percent
            .unwrap_or(config.usage_projection_cost_floor_margin_percent);
        effective.usage_projection_output_uplift_min_tokens = self
            .usage_projection_output_uplift_min_tokens
            .unwrap_or(config.usage_projection_output_uplift_min_tokens);
        effective.usage_projection_output_uplift_percent = self
            .usage_projection_output_uplift_percent
            .unwrap_or(config.usage_projection_output_uplift_percent);
        effective
    }
    pub fn validate(&self) -> Result<(), String> {
        crate::admin::service::validate_accounts_config(&self.merge(&AccountsConfig::default()))
    }
}
fn default_cost_multiplier() -> f64 {
    1.0
}
pub(crate) fn validate_account_fields(
    proxy_url: Option<&str>,
    multiplier: f64,
    overrides: &AccountOverrides,
) -> anyhow::Result<()> {
    if !multiplier.is_finite() || multiplier <= 0.0 {
        anyhow::bail!("costMultiplier 必须大于 0");
    }
    if let Some(proxy) = proxy_url.filter(|url| !url.trim().is_empty()) {
        let url = reqwest::Url::parse(proxy)?;
        if !matches!(url.scheme(), "http" | "https" | "socks5" | "socks5h")
            || url.host_str().is_none()
        {
            anyhow::bail!("proxyUrl 必须使用 http/https/socks5/socks5h");
        }
        reqwest::Proxy::all(proxy)?;
    }
    overrides.validate().map_err(anyhow::Error::msg)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub proxy_url: Option<String>,
    #[serde(default = "default_cost_multiplier")]
    pub cost_multiplier: f64,
    #[serde(default)]
    pub overrides: AccountOverrides,
    pub id: u64,
    /// Monotonic PostgreSQL row revision used to fence selection-to-send races.
    #[serde(default = "default_account_revision")]
    pub revision: u64,
    pub name: String,
    pub base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub masked_api_key: Option<String>,
    pub auth_type: AccountAuthType,
    #[serde(default)]
    pub header_profile: AccountHeaderProfile,
    #[serde(default)]
    pub append_beta_query: bool,
    #[serde(default)]
    pub header_overrides: HashMap<String, String>,
    #[serde(default)]
    pub wire_profile: AccountWireProfile,
    #[serde(default)]
    pub tls_profile: AccountTlsProfile,
    pub enabled: bool,
    pub priority: i32,
    pub max_concurrent_requests: u32,
    pub usage_projection_mode: UsageProjectionMode,
    #[serde(default)]
    pub raw_usage_source: RawUsageSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_response_mode: Option<StreamResponseMode>,
    pub auto_disable_policy: AutoDisablePolicy,
    #[serde(default)]
    pub pre_output_stream_retry_mode: StreamRetryMode,
    pub auto_disabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_disabled_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_disabled_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_disabled_until: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_disabled_last_error: Option<String>,
    #[serde(default)]
    pub normalize_model_version_dots: bool,
    #[serde(default)]
    pub model_mapping_mode: ModelMappingMode,
    #[serde(default)]
    pub model_mapping_require_match: bool,
    #[serde(default)]
    pub model_mapping_rules: Vec<ModelMappingRule>,
    #[serde(default)]
    pub supported_models: Vec<String>,
    #[serde(default)]
    pub route_mode: RouteRuleMode,
    #[serde(default)]
    pub route_rules: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Secret-free, pre-normalized projection used only for cheap routing eligibility checks.
/// Dispatch always reloads an authoritative request-scoped `Account` from PostgreSQL.
#[derive(Debug, Clone)]
pub(crate) struct AccountEligibility {
    pub(crate) id: u64,
    pub(crate) enabled: bool,
    pub(crate) auto_disabled: bool,
    pub(crate) auto_disabled_until: Option<DateTime<Utc>>,
    /// 原始 `supportedModels` 列表；匹配时经 [`supported_models_match`] 的线程局部缓存展开，
    /// 与权威 `Account` 路径同一套 allowlist 规则（含家族别名），避免快照与权威判定不一致。
    pub(crate) supported_models: Arc<Vec<String>>,
    pub(crate) route_mode: RouteRuleMode,
    pub(crate) route_rules: Arc<Vec<String>>,
}

impl AccountEligibility {
    fn is_auto_disabled_at(&self, now: DateTime<Utc>) -> bool {
        self.auto_disabled
            && self
                .auto_disabled_until
                .map(|until| until > now)
                .unwrap_or(true)
    }
}

fn account_eligibility_from_pool(pool: &Account) -> AccountEligibility {
    AccountEligibility {
        id: pool.id,
        enabled: pool.enabled,
        auto_disabled: pool.auto_disabled,
        auto_disabled_until: pool.auto_disabled_until,
        supported_models: Arc::new(pool.supported_models.clone()),
        route_mode: pool.route_mode,
        route_rules: Arc::new(normalize_route_rules(&pool.route_rules)),
    }
}

impl Account {
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
                .map(mask_account_key)
                .or_else(|| self.masked_api_key.clone());
        }
        masked.api_key = None;
        masked
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateAccountRequest {
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub proxy_url: Option<String>,
    #[serde(default = "default_cost_multiplier")]
    pub cost_multiplier: f64,
    #[serde(default)]
    pub overrides: AccountOverrides,
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    #[serde(default)]
    pub auth_type: AccountAuthType,
    #[serde(default)]
    pub header_profile: AccountHeaderProfile,
    #[serde(default)]
    pub append_beta_query: bool,
    #[serde(default)]
    pub header_overrides: HashMap<String, String>,
    #[serde(default)]
    pub wire_profile: AccountWireProfile,
    #[serde(default)]
    pub tls_profile: AccountTlsProfile,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_priority")]
    pub priority: i32,
    #[serde(default = "default_account_concurrency")]
    pub max_concurrent_requests: u32,
    #[serde(default)]
    pub usage_projection_mode: UsageProjectionMode,
    #[serde(default)]
    pub raw_usage_source: RawUsageSource,
    #[serde(default)]
    pub stream_response_mode: Option<StreamResponseMode>,
    #[serde(default)]
    pub auto_disable_policy: AutoDisablePolicy,
    #[serde(default)]
    pub pre_output_stream_retry_mode: StreamRetryMode,
    #[serde(default)]
    pub normalize_model_version_dots: bool,
    #[serde(default)]
    pub model_mapping_mode: ModelMappingMode,
    #[serde(default)]
    pub model_mapping_require_match: bool,
    #[serde(default)]
    pub model_mapping_rules: Vec<ModelMappingRule>,
    #[serde(default)]
    pub supported_models: Vec<String>,
    #[serde(default)]
    pub route_mode: RouteRuleMode,
    #[serde(default)]
    pub route_rules: Vec<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAccountRequest {
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default, deserialize_with = "deserialize_proxy_url_update")]
    pub proxy_url: Option<Option<String>>,
    #[serde(default)]
    pub cost_multiplier: Option<f64>,
    #[serde(default)]
    pub overrides: Option<AccountOverrides>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub auth_type: Option<AccountAuthType>,
    #[serde(default)]
    pub header_profile: Option<AccountHeaderProfile>,
    #[serde(default)]
    pub append_beta_query: Option<bool>,
    #[serde(default)]
    pub header_overrides: Option<HashMap<String, String>>,
    #[serde(default)]
    pub wire_profile: Option<AccountWireProfile>,
    #[serde(default)]
    pub tls_profile: Option<AccountTlsProfile>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub max_concurrent_requests: Option<u32>,
    #[serde(default)]
    pub usage_projection_mode: Option<UsageProjectionMode>,
    #[serde(default)]
    pub raw_usage_source: Option<RawUsageSource>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_stream_response_mode_update"
    )]
    pub stream_response_mode: Option<Option<StreamResponseMode>>,
    #[serde(default)]
    pub auto_disable_policy: Option<AutoDisablePolicy>,
    #[serde(default)]
    pub pre_output_stream_retry_mode: Option<StreamRetryMode>,
    #[serde(default)]
    pub normalize_model_version_dots: Option<bool>,
    #[serde(default)]
    pub model_mapping_mode: Option<ModelMappingMode>,
    #[serde(default)]
    pub model_mapping_require_match: Option<bool>,
    #[serde(default)]
    pub model_mapping_rules: Option<Vec<ModelMappingRule>>,
    #[serde(default)]
    pub supported_models: Option<Vec<String>>,
    #[serde(default)]
    pub route_mode: Option<RouteRuleMode>,
    #[serde(default)]
    pub route_rules: Option<Vec<String>>,
    #[serde(default)]
    pub notes: Option<String>,
}

fn deserialize_optional_stream_response_mode_update<'de, D>(
    deserializer: D,
) -> Result<Option<Option<StreamResponseMode>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<StreamResponseMode>::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetAccountEnabledRequest {
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountStatus {
    pub account: Account,
    pub in_flight: u32,
    #[serde(default)]
    pub dispatched_total: u64,
    pub cooldown_remaining_secs: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cooldown_reason: Option<String>,
    pub transient_failure_streak: u32,
    pub transient_failure_ttl_secs: u64,
    pub dispatchable: bool,
    pub skipped_reason: Option<String>,
    /// 被动质量采样视图；主开关关闭或样本为空时为 `None`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<AccountQualityView>,
}

/// 管理端展示用的质量状态视图。
///
/// 与内部 `AccountQualityState` 分离：内部结构用绝对时间戳，
/// 管理端需要的是"还剩多少秒"这类相对量，且不应随内部字段调整而破坏 API。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountQualityView {
    /// 失败率 EWMA，范围 `0.0..=1.0`。
    pub recent_error_rate: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttft_ewma_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ewma_ms: Option<f64>,
    pub sample_count: u64,
    /// 样本是否已达到参与评分的门槛。
    pub scoring_active: bool,
    /// 是否处于临时避让（降低调度优先级）。
    pub in_probation: bool,
    /// 避让剩余秒数；未避让时为 0。
    pub probation_remaining_secs: u64,
    /// 连续避让层级，用于展示指数退避进展。
    pub probation_level: u32,
    /// 恢复爬坡进度 `0.0..=1.0`；`1.0` 表示已完全恢复。
    pub recovery_progress: f64,
}

impl AccountQualityView {
    fn from_state(quality: &AccountQualityState, config: &AccountsConfig, now_ms: i64) -> Self {
        let in_probation = quality.is_in_probation(now_ms);
        let probation_remaining_secs = quality
            .probation_until_ms
            .map(|until_ms| until_ms.saturating_sub(now_ms).max(0) as u64 / 1_000)
            .unwrap_or(0);
        let ramp_ms = (config.recovery_ramp_secs as i64).saturating_mul(1_000);
        Self {
            recent_error_rate: quality.recent_error_rate,
            ttft_ewma_ms: quality.ttft_ewma_ms,
            latency_ewma_ms: quality.latency_ewma_ms,
            sample_count: quality.sample_count,
            scoring_active: quality.has_enough_samples(config.quality_min_samples),
            in_probation,
            probation_remaining_secs,
            probation_level: quality.probation_level,
            recovery_progress: quality.recovery_progress(now_ms, ramp_ms),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountsStatusResponse {
    pub accounts: Vec<AccountStatus>,
    #[serde(default)]
    pub summary: AccountsSummary,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountsSummary {
    pub total: usize,
    pub enabled: usize,
    pub dispatchable: usize,
    pub in_cooldown: usize,
    pub auto_disabled: usize,
    pub in_flight_total: u64,
    pub global_in_flight: u64,
    pub queued_requests: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchStatus {
    pub queued_requests: Option<u32>,
    pub global_in_flight: u64,
    pub pending_redis_releases: usize,
    pub coordinator_admission_saturated: u64,
    pub dispatched_total: u64,
    pub dispatch_rate_per_sec: f64,
    pub secs_since_last_dispatch: Option<f64>,
    pub progressing: bool,
}

#[derive(Default)]
pub(crate) struct DispatchCounters {
    total: AtomicU64,
    in_flight: AtomicU64,
    per_account: SyncMutex<HashMap<u64, AtomicU64>>,
    window: SyncMutex<VecDeque<(Instant, u64)>>,
    last_dispatch: SyncMutex<Option<Instant>>,
}

impl DispatchCounters {
    fn record(&self, account_id: u64) {
        let now = Instant::now();
        let mut window = self.window.lock();
        self.total.fetch_add(1, Ordering::Relaxed);
        self.per_account
            .lock()
            .entry(account_id)
            .or_default()
            .fetch_add(1, Ordering::Relaxed);
        *self.last_dispatch.lock() = Some(now);
        while window
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at) >= Duration::from_secs(5))
        {
            window.pop_front();
        }
        if let Some((_, count)) = window
            .back_mut()
            .filter(|(at, _)| now.duration_since(*at) < Duration::from_millis(100))
        {
            *count += 1;
        } else {
            window.push_back((now, 1));
        }
    }

    pub(crate) fn snapshot(
        &self,
        queued_requests: Option<u32>,
        pending_redis_releases: usize,
    ) -> DispatchStatus {
        let now = Instant::now();
        let mut window = self.window.lock();
        while window
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at) >= Duration::from_secs(5))
        {
            window.pop_front();
        }
        let secs_since_last_dispatch = self
            .last_dispatch
            .lock()
            .map(|at| now.duration_since(at).as_secs_f64());
        DispatchStatus {
            queued_requests,
            global_in_flight: self.in_flight.load(Ordering::Relaxed),
            pending_redis_releases,
            coordinator_admission_saturated: 0,
            dispatched_total: self.total.load(Ordering::Relaxed),
            dispatch_rate_per_sec: window.iter().map(|(_, count)| count).sum::<u64>() as f64 / 5.0,
            secs_since_last_dispatch,
            progressing: queued_requests == Some(0)
                || secs_since_last_dispatch.is_some_and(|secs| secs < 5.0),
        }
    }
}

#[cfg(test)]
mod dispatch_observation_tests {
    use super::*;

    #[test]
    fn readyz_dispatch_counts_attempts_and_expires_window_without_losing_total() {
        let counters = Arc::new(DispatchCounters::default());
        counters.record(7);
        counters.record(7);
        counters.record(9);
        let lease = InFlightObservation::new(counters.clone());
        let busy = counters.snapshot(Some(1), 2);
        assert_eq!(busy.dispatched_total, 3);
        assert_eq!(busy.dispatch_rate_per_sec, 0.6);
        assert_eq!(busy.global_in_flight, 1);
        assert_eq!(busy.pending_redis_releases, 2);
        assert!(busy.progressing);
        assert_eq!(counters.per_account.lock()[&7].load(Ordering::Relaxed), 2);
        drop(lease);
        let old = Instant::now() - Duration::from_secs(6);
        *counters.last_dispatch.lock() = Some(old);
        *counters.window.lock() = VecDeque::from([(old, 3)]);
        let stalled = counters.snapshot(Some(1), 0);
        assert_eq!(stalled.dispatched_total, 3);
        assert_eq!(stalled.dispatch_rate_per_sec, 0.0);
        assert_eq!(stalled.global_in_flight, 0);
        assert!(!stalled.progressing);
        assert!(counters.snapshot(Some(0), 0).progressing);
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountTestResponse {
    pub ok: bool,
    pub status: Option<u16>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
}

#[derive(Default)]
pub(crate) struct AccountRouteRequestPreparationCache {
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

impl AccountRouteRequestPreparationCache {
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
pub struct AccountRouteRequest {
    /// Final entry bytes after optional route patches.
    pub effective_raw_body: Bytes,
    /// Probe bound to `effective_raw_body`; shared across every failover candidate.
    pub(crate) effective_raw_probe: Option<Arc<RawMessagesBodyProbe>>,
    pub(crate) preparation_cache: Arc<AccountRouteRequestPreparationCache>,
    /// Original downstream bytes, retained for diagnostics and usage records.
    pub raw_body: Bytes,
    pub headers: HeaderMap,
    pub endpoint: String,
    pub payload: Option<MessagesRequest>,
    pub history_sanitize_enabled: bool,
    pub model_hint: Option<String>,
    /// 调用方请求体里的原始模型名；返回给调用方的 `model` 一律用它，不暴露映射后或上游实际的模型。
    pub client_model: Option<String>,
    /// 路由缓存策略里的零用量估算规则。
    pub zero_usage_repair: crate::model::config::ZeroUsageRepairPolicy,
    pub stream_hint: Option<bool>,
    pub request_input_tokens: i32,
    pub upstream_model: Option<String>,
    pub model_resolution_source: Option<String>,
    pub model_resolution_note: Option<String>,
    pub route_subtype: UsageRouteSubtype,
    pub reported_usage: ReportedUsageConfig,
    pub prompt_cache: Arc<PromptCacheTracker>,
    pub prompt_cache_creation_controller: Arc<PromptCacheCreationController>,
    pub prompt_cache_strategy_type: PromptCacheStrategyType,
    pub prompt_cache_simulation_mode: PromptCacheSimulationMode,
    pub prompt_cache_route_namespace: Option<String>,
    /// Route cache TTL mode for accounts (`cacheTtl.account`).
    pub cache_ttl_mode: crate::model::config::CacheTtlMode,
    pub prompt_cache_target_read_ratio: f64,
    pub prompt_cache_token_scale: f64,
    pub prompt_cache_max_simulated_input_tokens: i32,
    pub prompt_cache_cap_jitter_min_tokens: i32,
    pub prompt_cache_cap_jitter_max_tokens: i32,
    pub prompt_cache_scale_min_input_tokens: i32,
    pub prompt_cache_creation_control: PromptCacheCreationControlConfig,
    pub prompt_cache_bounds: PromptCacheBounds,
    pub prefix_tool_cache_policy: PrefixToolCachePolicy,
    pub prefix_tool_plus_cache_policy: crate::model::config::PrefixToolPlusCachePolicy,
    pub stable_segment_cache_policy: StableSegmentCachePolicy,
    /// 路由的 hitShaping 参数；`bounded_high_cache` 账号投影收口时使用（读取倍数上限、读写最小值）。
    pub cache_hit_shaping_policy: crate::model::config::CacheHitShapingPolicy,
    pub model_capabilities: Arc<ModelCapabilitiesCatalog>,
    pub pricing_catalog: Arc<PricingCatalog>,
    pub request_id: String,
    pub error_id: String,
    pub recorder: Arc<crate::anthropic::usage::UsageRecorder>,
    pub started_at: Instant,
    /// 流式首个语义输出前最多静默保留多久（与本地 `streamPreOutputHoldSecs` 一致，从请求开始计）。
    /// 到期且已缓冲 message_start 时提前提交并开始保活，放弃之后的首输出前换池；None 表示一直保留。
    pub stream_pre_output_hold: Option<Duration>,
    /// 提前提交模式下已打开的下游 SSE 通道。设置后，首个内容前的 message_start（usage 清零）
    /// 和 ping 直接写入通道，换池重放时不再重复发送 message_start。
    pub early_commit: Option<Arc<ExternalEarlyCommitDownstream>>,
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

impl AccountRouteRequest {
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

    fn model_candidates_for_support(&self) -> [Option<&str>; 4] {
        [
            self.payload.as_ref().map(|payload| payload.model.as_str()),
            self.model_hint.as_deref(),
            (matches!(
                self.model_resolution_source.as_deref(),
                Some("alias" | "family_normalized")
            ))
            .then_some(self.upstream_model.as_deref())
            .flatten(),
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
            if let Some(normalized) = normalize_account_model_cooldown_key_model(model)
                .filter(|normalized| !out.iter().any(|existing| existing == normalized))
            {
                out.push(normalized);
            }
        }
        out
    }

    fn requested_max_tokens(&self) -> Option<i32> {
        if let Some(max_tokens) = self
            .payload
            .as_ref()
            .and_then(|payload| (payload.max_tokens > 0).then_some(payload.max_tokens))
        {
            return Some(max_tokens);
        }

        requested_max_tokens_from_raw_body(&self.effective_raw_body).or_else(|| {
            if self.raw_body == self.effective_raw_body {
                None
            } else {
                requested_max_tokens_from_raw_body(&self.raw_body)
            }
        })
    }

    fn stable_conversation_id(&self) -> Option<String> {
        self.payload
            .as_ref()
            .and_then(crate::anthropic::conversation_id::extract_stable_conversation_id)
    }

    fn reset_preparation_cache(&mut self) {
        self.preparation_cache = Arc::new(AccountRouteRequestPreparationCache::default());
    }

    #[cfg(test)]
    fn preparation_operation_counts(&self) -> ExternalRequestPreparationOperationCounts {
        self.preparation_cache.operation_counts()
    }
}

fn external_route_raw_projection_payload(
    route: &AccountRouteRequest,
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

fn requested_max_tokens_from_raw_body(body: &Bytes) -> Option<i32> {
    if body.is_empty() {
        return None;
    }
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("max_tokens").and_then(|value| value.as_i64()))
        .filter(|value| *value > 0 && *value <= i32::MAX as i64)
        .map(|value| value as i32)
}

#[derive(Debug, Default)]
pub struct ExternalLatencyTraceState {
    upstream_header_ms: AtomicU64,
    downstream_first_byte_ms: AtomicU64,
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

    fn mark_downstream_first_byte(&self, started_at: Instant) {
        Self::store_once(&self.downstream_first_byte_ms, Self::elapsed_ms(started_at));
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
            capacity_weight_units: None,
            estimated_input_tokens: None,
            payload_guard_ms: None,
            upstream_header_ms: load_nonzero(&self.upstream_header_ms),
            credential_dispatch_elapsed_ms: None,
            upstream_header_wait_ms: None,
            downstream_first_byte_ms: load_nonzero(&self.downstream_first_byte_ms),
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
            tool_choice_violation: None,
            dropped_tool_uses: None,
            websearch_mode: None,
            web_search_requests: None,
        };
        (!trace.is_empty()).then_some(trace)
    }
}

fn load_nonzero(value: &AtomicU64) -> Option<u64> {
    let value = value.load(Ordering::Acquire);
    (value > 0).then_some(value)
}

/// 协调路径上的单次 Redis 调用：统一套 `ACCOUNT_COORDINATOR_REDIS_OPERATION_TIMEOUT`，
/// 超时与错误都作为普通错误返回，由调用方决定是否标记需要重新探测。
async fn account_coordinator_redis_call<T>(
    operation: &'static str,
    future: impl std::future::Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    match timeout(ACCOUNT_COORDINATOR_REDIS_OPERATION_TIMEOUT, future).await {
        Ok(result) => result,
        Err(_) => anyhow::bail!(
            "{operation} timed out after {}ms",
            ACCOUNT_COORDINATOR_REDIS_OPERATION_TIMEOUT.as_millis()
        ),
    }
}

fn account_coordination_monotonic_ms() -> u64 {
    static STARTED_AT: OnceLock<Instant> = OnceLock::new();
    STARTED_AT
        .get_or_init(Instant::now)
        .elapsed()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn external_dispatch_deadline(
    route: &AccountRouteRequest,
    config: &AccountsConfig,
) -> Option<Instant> {
    let timeout_secs = if route.is_stream() {
        config
            .stream_request_timeout_secs
            .max(config.request_timeout_secs)
    } else {
        config.request_timeout_secs
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

/// 非流式请求自身的超时预算：从请求开始算 `requestTimeoutSecs`，换池重试共用这一个预算。
///
/// 共享调度截止时间（`credentialDispatchMaxWaitSecs`，默认 30 秒）只约束排队和换池前的等待，
/// 不参与这里的计算；否则请求发出后会在 30 秒处被切断，慢但正常的非流式响应永远拿不到。
fn external_non_stream_request_remaining(
    route: &AccountRouteRequest,
    config: &AccountsConfig,
) -> Option<Duration> {
    (!route.is_stream() && config.request_timeout_secs > 0)
        .then(|| route.started_at + Duration::from_secs(config.request_timeout_secs))
        .map(|deadline| deadline.saturating_duration_since(Instant::now()))
        .filter(|duration| !duration.is_zero())
}

fn external_request_send_timeout(
    route: &AccountRouteRequest,
    config: &AccountsConfig,
) -> Option<Duration> {
    let configured = if route.is_stream() {
        (config.stream_request_timeout_secs > 0)
            .then(|| Duration::from_secs(config.stream_request_timeout_secs))
    } else {
        (config.request_timeout_secs > 0).then(|| Duration::from_secs(config.request_timeout_secs))
    };
    match (
        configured,
        external_non_stream_request_remaining(route, config),
    ) {
        (Some(configured), Some(remaining)) => Some(configured.min(remaining)),
        (configured, remaining) => configured.or(remaining),
    }
}

fn external_response_body_timeout_secs(
    route: &AccountRouteRequest,
    config: &AccountsConfig,
) -> u64 {
    let configured = if config.request_timeout_secs == 0 {
        ACCOUNT_DEFAULT_RESPONSE_BODY_TIMEOUT_SECS
    } else {
        config.request_timeout_secs
    };
    external_non_stream_request_remaining(route, config)
        .map(|remaining| configured.min(remaining.as_secs().max(1)))
        .unwrap_or(configured)
}

fn account_reqwest_client(
    wire_profile: AccountWireProfile,
    tls_profile: AccountTlsProfile,
) -> reqwest::Client {
    let mut builder = reqwest::Client::builder().no_proxy();
    if matches!(wire_profile, AccountWireProfile::Http1TitleCase) {
        builder = builder.http1_only().http1_title_case_headers();
    }
    if matches!(tls_profile, AccountTlsProfile::NativeTls) {
        builder = apply_account_native_tls_profile(builder);
    }
    builder.build().unwrap_or_else(|err| {
        tracing::warn!(
            error = %err,
            wire_profile = wire_profile.as_str(),
            tls_profile = tls_profile.as_str(),
            "account transport profile client build failed, falling back to default reqwest client"
        );
        reqwest::Client::new()
    })
}

#[cfg(feature = "native-tls")]
fn apply_account_native_tls_profile(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    builder.use_native_tls()
}

#[cfg(not(feature = "native-tls"))]
fn apply_account_native_tls_profile(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    tracing::warn!(
        "account tlsProfile=native_tls requested but binary was built without native-tls feature"
    );
    builder
}

struct ExternalForwardResponse {
    response: Response,
    outbound_model: Option<String>,
    outbound_body: Bytes,
    billing: Option<AccountBilling>,
    downstream_stop_reason: Option<String>,
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
    config: AccountsConfig,
    route: AccountRouteRequest,
    pool: Account,
    attempts: Vec<AccountAttempt>,
    outbound_model: Option<String>,
    outbound_body: Bytes,
    usage_capture: Option<Arc<SyncMutex<ExternalUsageCapture>>>,
    usage_projection: Option<ExternalUsageProjectionContext>,
}

struct ExternalForwardError {
    err: AccountError,
    outbound_model: Option<String>,
    attempt_rejection: Option<InferenceAttemptRejection>,
}

impl ExternalForwardError {
    fn new(err: AccountError, outbound_model: Option<String>) -> Self {
        Self {
            err,
            outbound_model,
            attempt_rejection: None,
        }
    }

    fn dispatch_rejected(
        err: AccountError,
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

impl From<AccountError> for ExternalForwardError {
    fn from(err: AccountError) -> Self {
        Self::new(err, None)
    }
}

/// 提前提交模式下下游客户端已断开：不再换池，也不计入池质量或冷却。
fn external_early_commit_downstream_gone_error(
    outbound_model: Option<String>,
) -> ExternalForwardError {
    ExternalForwardError::new(
        AccountError {
            status: None,
            message: EXTERNAL_EARLY_COMMIT_CLIENT_GONE_MESSAGE.to_string(),
            retryable: false,
            auto_disable_reason: None,
            cooldown: None,
            protocol_error: Some(EXTERNAL_EARLY_COMMIT_CLIENT_GONE_REASON),
            raw_upstream_error: None,
        },
        outbound_model,
    )
}

const EXTERNAL_EARLY_COMMIT_CLIENT_GONE_REASON: &str = "early_commit_client_gone";
const EXTERNAL_EARLY_COMMIT_CLIENT_GONE_MESSAGE: &str =
    "downstream client disconnected before the external stream produced output";

fn account_lease_lost_forward_error(outbound_model: Option<String>) -> ExternalForwardError {
    ExternalForwardError::new(
        AccountError {
            status: Some(StatusCode::SERVICE_UNAVAILABLE),
            message: "account lease coordination was lost".to_string(),
            retryable: false,
            auto_disable_reason: None,
            cooldown: None,
            protocol_error: None,
            raw_upstream_error: None,
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
        AccountError {
            status: Some(status),
            message,
            retryable,
            auto_disable_reason: None,
            cooldown: None,
            protocol_error: None,
            raw_upstream_error: None,
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
    route: &AccountRouteRequest,
    attempts: Vec<AccountAttempt>,
) -> AccountFinalError {
    AccountFinalError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        response_error_type: "service_unavailable".to_string(),
        route_error_type: "inference_attempt_policy".to_string(),
        message: "The request could not be completed. Please try again shortly.".to_string(),
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

pub enum AccountForwardOutcome {
    Response(Response),
    FinalError(AccountFinalError),
}

#[derive(Debug, Clone)]
pub struct AccountFinalError {
    pub status: StatusCode,
    pub response_error_type: String,
    pub route_error_type: String,
    pub message: String,
    pub error_id: String,
    pub retryable: bool,
    pub attempts: Vec<AccountAttempt>,
    pub pool_id: Option<u64>,
    pub pool_name: Option<String>,
}

impl AccountFinalError {
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
            "account final error"
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
        if self.route_error_type == "model_not_found" {
            return StatusCode::NOT_FOUND;
        }
        if self.is_rate_limit() {
            return StatusCode::TOO_MANY_REQUESTS;
        }
        if self.is_public_invalid_request() {
            return StatusCode::BAD_REQUEST;
        }
        if self.is_upstream_overloaded() || self.is_capacity_like() {
            return StatusCode::from_u16(529).unwrap();
        }
        StatusCode::BAD_GATEWAY
    }

    /// 过载：上游报 503 / 官方 529 overloaded，或内部容量类错误（内部同样用 503 标记）。
    /// 对外统一 529 overloaded_error + "服务繁忙"，客户端会退避重试；不能当成 502 网关错误，
    /// 也不能说成"没有及时响应"。
    fn is_upstream_overloaded(&self) -> bool {
        self.status == StatusCode::SERVICE_UNAVAILABLE || self.status.as_u16() == 529
    }

    fn public_error_type(&self) -> &'static str {
        if self.route_error_type == "model_not_found" {
            return "not_found_error";
        }
        if self.is_rate_limit() {
            return "rate_limit_error";
        }
        if self.is_public_invalid_request() {
            return "invalid_request_error";
        }
        if self.is_upstream_overloaded() || self.is_capacity_like() {
            return "overloaded_error";
        }
        "api_error"
    }

    fn public_message(&self, external_error_id: &str) -> String {
        if self.route_error_type == "model_not_found" {
            return envelope::public_message_with_error_id(&self.message, external_error_id);
        }
        let message = if self.is_rate_limit() {
            envelope::PUBLIC_RATE_LIMIT_MESSAGE
        } else if self.is_external_image_too_large() {
            envelope::PUBLIC_IMAGE_TOO_LARGE_MESSAGE
        } else if self.is_external_payload_too_large() {
            "prompt is too long: Request input content length exceeded the external upstream threshold. This limit is separate from the model context window. Reduce oversized tools, system prompt, documents, images, tool results, or conversation history and retry."
        } else if self.is_external_context_window_full() {
            "prompt is too long: Context window is full for the external model. Reduce conversation history, system prompt, tools, documents, images, or tool results and retry."
        } else if self.is_public_invalid_request() {
            envelope::PUBLIC_INVALID_REQUEST_MESSAGE
        } else if self.is_upstream_overloaded() {
            envelope::PUBLIC_ACCOUNT_UNAVAILABLE_MESSAGE
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

    pub fn is_external_image_too_large(&self) -> bool {
        self.status == StatusCode::BAD_REQUEST && self.message == EXTERNAL_IMAGE_TOO_LARGE_MESSAGE
    }

    pub fn is_external_payload_too_large(&self) -> bool {
        if self.status != StatusCode::BAD_REQUEST {
            return false;
        }
        let lower = self.message.to_ascii_lowercase();
        lower.contains("request payload is too large")
            || lower.contains("content length exceeded")
            || lower.contains("content_length_exceeds_threshold")
    }

    pub fn is_external_context_window_full(&self) -> bool {
        if self.status != StatusCode::BAD_REQUEST {
            return false;
        }
        let lower = self.message.to_ascii_lowercase();
        lower.contains("context window is full")
            || lower.contains("prompt is too long")
            || lower.contains("input is too long")
    }

    pub fn is_capacity_like(&self) -> bool {
        matches!(
            self.route_error_type.as_str(),
            "account_capacity_full"
                | "account_queue_full"
                | "account_wait_timeout"
                | "account_cooldown"
                | "account_coordinator_unavailable"
        )
    }
}

#[derive(Debug, Clone, Default)]
struct ExternalUsageCapture {
    request_input_tokens: Option<i32>,
    estimated_output_tokens: i32,
    downstream_stop_reason: Option<String>,
    raw: Option<CacheUsage>,
    upstream_usage: UpstreamUsage,
    shaped: Option<CacheUsage>,
    reported: Option<CacheUsage>,
    projected: bool,
    usage_estimated: bool,
    usage_estimate_reason: Option<String>,
    usage_candidate_path: Option<String>,
    body_usage_projection_applied: bool,
    stream_error_message: Option<String>,
    raw_upstream_error: Option<RawUpstreamError>,
    stream_response_mode: Option<StreamResponseMode>,
    /// 上游 usage 输入或输出为 0 时的估算修复；账号透传上游 usage 或路由关闭时为 `None`。
    zero_usage_repair: Option<zero_usage_repair::ZeroUsageRepairContext>,
    /// 上游实际返回的模型（改写成调用方模型之前）。
    upstream_response_model: Option<String>,
    debug_stream: ExternalUsageDebugStreamCapture,
    wire_debug: ExternalWireDebugCapture,
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

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExternalWireDebugCapture {
    raw_upstream_stream: ExternalWireDebugSseCapture,
    processed_downstream_stream: ExternalWireDebugSseCapture,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExternalWireDebugSseCapture {
    chunks_seen: u64,
    empty_outputs_seen: u64,
    events_seen: u64,
    data_lines_seen: u64,
    done_events_seen: u64,
    json_parse_errors: u64,
    usage_events_seen: u64,
    bytes_seen: u64,
    preview_base64: String,
    preview_utf8: String,
    preview_truncated: bool,
    event_types: BTreeMap<String, u64>,
    usage_paths: BTreeMap<String, u64>,
    usage_event_samples: Vec<ExternalUsageDebugRawEventSample>,
    semantic: ExternalWireDebugSseSemanticSummary,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExternalWireDebugSseSemanticSummary {
    message_start_events: u64,
    message_delta_events: u64,
    message_stop_events: u64,
    content_block_start_events: u64,
    content_block_delta_events: u64,
    content_block_stop_events: u64,
    text_block_start_events: u64,
    text_block_start_chars: u64,
    text_delta_events: u64,
    text_delta_chars: u64,
    thinking_block_start_events: u64,
    thinking_block_start_chars: u64,
    thinking_delta_events: u64,
    thinking_delta_chars: u64,
    redacted_thinking_block_start_events: u64,
    redacted_thinking_block_start_chars: u64,
    signature_delta_events: u64,
    tool_use_block_start_events: u64,
    server_tool_use_block_start_events: u64,
    input_json_delta_events: u64,
    input_json_delta_chars: u64,
    error_events: u64,
    ping_events: u64,
    first_visible_text_event_index: Option<u64>,
    first_semantic_output_event_index: Option<u64>,
    last_stop_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ExternalStreamProcessingPlan {
    response_mode: StreamResponseMode,
    mask_errors: bool,
    capture_usage: bool,
    usage_debug_enabled: bool,
    usage_debug_max_body_bytes: usize,
    wire_debug_enabled: bool,
    wire_debug_max_body_bytes: usize,
}

impl ExternalStreamProcessingPlan {
    fn from_mode(response_mode: StreamResponseMode) -> Self {
        match response_mode {
            StreamResponseMode::EventPassthrough => Self {
                response_mode,
                mask_errors: true,
                capture_usage: true,
                usage_debug_enabled: false,
                usage_debug_max_body_bytes: 0,
                wire_debug_enabled: false,
                wire_debug_max_body_bytes: 0,
            },
        }
    }

    fn for_pool(pool: &Account, config: &AccountsConfig) -> Self {
        let mut plan = Self::from_mode(effective_account_stream_response_mode(pool, config));
        if usage_debug_enabled(config) {
            plan.usage_debug_enabled = true;
            plan.usage_debug_max_body_bytes = usage_debug_max_body_bytes(config);
        }
        if wire_debug_enabled(config) {
            plan.wire_debug_enabled = true;
            plan.wire_debug_max_body_bytes = wire_debug_max_body_bytes(config);
        }
        plan
    }
}

fn effective_account_stream_response_mode(
    pool: &Account,
    config: &AccountsConfig,
) -> StreamResponseMode {
    pool.stream_response_mode
        .unwrap_or(config.stream_response_mode)
}

fn usage_debug_enabled(config: &AccountsConfig) -> bool {
    config.usage_debug_enabled && config.usage_debug_max_files > 0
}

fn usage_debug_dir(config: &AccountsConfig) -> PathBuf {
    let configured = config.usage_debug_dir.trim();
    if configured.is_empty() {
        PathBuf::from(ACCOUNT_USAGE_DEBUG_DEFAULT_DIR)
    } else {
        PathBuf::from(configured)
    }
}

fn usage_debug_max_body_bytes(config: &AccountsConfig) -> usize {
    let configured = config.usage_debug_max_body_bytes as usize;
    if configured == 0 {
        ACCOUNT_USAGE_DEBUG_DEFAULT_MAX_BODY_BYTES
    } else {
        configured.min(ACCOUNT_USAGE_DEBUG_MAX_BODY_BYTES)
    }
}

fn usage_debug_max_files(config: &AccountsConfig) -> u64 {
    let configured = config.usage_debug_max_files as u64;
    if configured == 0 {
        ACCOUNT_USAGE_DEBUG_DEFAULT_MAX_FILES
    } else {
        configured
    }
}

fn wire_debug_enabled(config: &AccountsConfig) -> bool {
    config.wire_debug_enabled && config.wire_debug_max_files > 0
}

fn wire_debug_dir(config: &AccountsConfig) -> PathBuf {
    let configured = config.wire_debug_dir.trim();
    if configured.is_empty() {
        PathBuf::from(ACCOUNT_WIRE_DEBUG_DEFAULT_DIR)
    } else {
        PathBuf::from(configured)
    }
}

fn wire_debug_max_body_bytes(config: &AccountsConfig) -> usize {
    let configured = config.wire_debug_max_body_bytes as usize;
    if configured == 0 {
        ACCOUNT_WIRE_DEBUG_DEFAULT_MAX_BODY_BYTES
    } else {
        configured.min(ACCOUNT_WIRE_DEBUG_MAX_BODY_BYTES)
    }
}

fn wire_debug_max_files(config: &AccountsConfig) -> u64 {
    let configured = config.wire_debug_max_files as u64;
    if configured == 0 {
        ACCOUNT_WIRE_DEBUG_DEFAULT_MAX_FILES
    } else {
        configured
    }
}

fn spawn_account_usage_debug_write(
    config: &AccountsConfig,
    route: &AccountRouteRequest,
    stage: &'static str,
    payload: serde_json::Value,
) {
    if !usage_debug_enabled(config) {
        return;
    }
    let max_files = usage_debug_max_files(config);
    let sequence = ACCOUNT_USAGE_DEBUG_WRITE_COUNT.fetch_add(1, Ordering::Relaxed);
    if sequence >= max_files {
        if sequence == max_files {
            tracing::warn!(
                request_id = %route.request_id,
                max_files,
                "account usage debug file limit reached"
            );
        }
        return;
    }
    let dir = usage_debug_dir(config);
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
                "account usage debug write failed"
            );
        }
    });
}

fn spawn_account_wire_debug_write(
    config: &AccountsConfig,
    route: &AccountRouteRequest,
    stage: &'static str,
    payload: serde_json::Value,
) {
    spawn_account_wire_debug_write_with_id(config, &route.request_id, stage, payload);
}

fn spawn_account_wire_debug_write_with_id(
    config: &AccountsConfig,
    request_id: &str,
    stage: &'static str,
    payload: serde_json::Value,
) {
    if !wire_debug_enabled(config) {
        return;
    }
    let max_files = wire_debug_max_files(config);
    let sequence = ACCOUNT_WIRE_DEBUG_WRITE_COUNT.fetch_add(1, Ordering::Relaxed);
    if sequence >= max_files {
        if sequence == max_files {
            tracing::warn!(
                request_id = %request_id,
                max_files,
                "account wire debug file limit reached"
            );
        }
        return;
    }
    let dir = wire_debug_dir(config);
    let request_id_part = sanitize_usage_debug_filename_part(request_id);
    let timestamp = Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string();
    let file_name = format!("{timestamp}-{request_id_part}-{sequence:06}-{stage}.json");
    let path = dir.join(file_name);
    let request_id_for_log = request_id.to_string();
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
                "account wire debug write failed"
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

fn account_usage_debug_route_context(
    route: &AccountRouteRequest,
    pool: &Account,
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



        "requestInputTokens": route.request_input_tokens,
        "account": {
            "id": pool.id,
            "name": pool.name.as_str(),
            "usageProjectionMode": pool.usage_projection_mode.as_str(),
        },
    })
}

fn account_usage_debug_bytes(value: &[u8], max_bytes: usize) -> serde_json::Value {
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

fn account_usage_debug_cache_usage(value: Option<CacheUsage>) -> Option<serde_json::Value> {
    value.map(|usage| usage.to_anthropic_usage_json())
}

fn account_usage_debug_capture(capture: &ExternalUsageCapture) -> serde_json::Value {
    json!({
        "requestInputTokens": capture.request_input_tokens,
        "estimatedOutputTokens": capture.estimated_output_tokens,
        "downstreamStopReason": capture.downstream_stop_reason.as_deref(),
        "rawUsage": account_usage_debug_cache_usage(capture.raw),
        "shapedUsage": account_usage_debug_cache_usage(capture.shaped),
        "reportedUsage": account_usage_debug_cache_usage(capture.reported),
        "projected": capture.projected,
        "usageEstimated": capture.usage_estimated,
        "usageEstimateReason": capture.usage_estimate_reason.as_deref(),
        "usageCandidatePath": capture.usage_candidate_path.as_deref(),
        "bodyUsageProjectionApplied": capture.body_usage_projection_applied,
        "streamErrorMessage": capture.stream_error_message.as_deref(),
        "streamResponseMode": capture.stream_response_mode.map(|mode| mode.as_str()),
    })
}

fn account_usage_debug_header_context(headers: &HeaderMap) -> serde_json::Value {
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

fn account_wire_debug_route_context(
    route: &AccountRouteRequest,
    pool: &Account,
    config: &AccountsConfig,
    outbound_model: Option<&str>,
) -> serde_json::Value {
    let effective_stream_response_mode = effective_account_stream_response_mode(pool, config);
    let stream_response_mode_source = if pool.stream_response_mode.is_some() {
        "pool"
    } else {
        "global"
    };
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



        "requestInputTokens": route.request_input_tokens,
        "account": {
            "id": pool.id,
            "name": pool.name.as_str(),
            "usageProjectionMode": pool.usage_projection_mode.as_str(),
            "streamResponseMode": effective_stream_response_mode.as_str(),
            "streamResponseModeSource": stream_response_mode_source,
            "preOutputStreamRetryMode": pool.pre_output_stream_retry_mode.as_str(),
            "effectivePreOutputStreamRetryEnabled": effective_account_pre_output_stream_retry_enabled(pool, config),
            "normalizeModelVersionDots": pool.normalize_model_version_dots,
            "modelMappingMode": pool.model_mapping_mode.as_str(),
            "modelMappingRequireMatch": pool.model_mapping_require_match,
        },
    })
}

fn account_wire_debug_request_snapshots(
    route: &AccountRouteRequest,
    max_bytes: usize,
) -> serde_json::Value {
    json!({
        "effectiveRawBody": {
            "body": account_wire_debug_bytes(&route.effective_raw_body, max_bytes),
            "summary": account_wire_debug_request_summary(&route.effective_raw_body),
        },
        "workingRawBody": {
            "body": account_wire_debug_bytes(&route.raw_body, max_bytes),
            "summary": account_wire_debug_request_summary(&route.raw_body),
            "differsFromEffectiveRawBody": route.raw_body.as_ref() != route.effective_raw_body.as_ref(),
        },
    })
}

fn account_wire_debug_bytes(value: &[u8], max_bytes: usize) -> serde_json::Value {
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

fn account_wire_debug_request_summary(body: &[u8]) -> serde_json::Value {
    let mut body_hasher = Sha256::new();
    body_hasher.update(body);
    let body_sha256 = hex::encode(body_hasher.finalize());
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return json!({
            "parseOk": false,
            "bodyBytes": body.len(),
            "bodySha256": body_sha256,
        });
    };

    let messages = value
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let system_items = match value.get("system") {
        Some(serde_json::Value::Array(items)) => items.len(),
        Some(_) => 1,
        None => 0,
    };
    let tool_count = value
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let user_message_count = account_wire_debug_message_role_count(messages, "user");
    let assistant_message_count = account_wire_debug_message_role_count(messages, "assistant");
    let last_messages_start = messages
        .len()
        .saturating_sub(ACCOUNT_WIRE_DEBUG_LAST_MESSAGE_LIMIT);
    let last_messages = messages[last_messages_start..]
        .iter()
        .enumerate()
        .map(|(offset, message)| {
            let index = last_messages_start + offset;
            let role = message
                .get("role")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            json!({
                "index": index,
                "role": role,
                "content": account_wire_debug_content_summary(message.get("content")),
            })
        })
        .collect::<Vec<_>>();
    let last_user_content = messages.iter().rev().find_map(|message| {
        (message.get("role").and_then(serde_json::Value::as_str) == Some("user"))
            .then(|| account_wire_debug_content_summary(message.get("content")))
    });

    json!({
        "parseOk": true,
        "bodyBytes": body.len(),
        "bodySha256": body_sha256,
        "model": value.get("model").and_then(serde_json::Value::as_str),
        "stream": value.get("stream").and_then(serde_json::Value::as_bool),
        "maxTokens": value.get("max_tokens").and_then(serde_json::Value::as_i64),
        "systemItems": system_items,
        "toolCount": tool_count,
        "messageCount": messages.len(),
        "userMessageCount": user_message_count,
        "assistantMessageCount": assistant_message_count,
        "lastUserContent": last_user_content,
        "lastMessages": last_messages,
    })
}

fn account_wire_debug_message_role_count(messages: &[serde_json::Value], role: &str) -> usize {
    messages
        .iter()
        .filter(|message| {
            message
                .get("role")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|candidate| candidate == role)
        })
        .count()
}

fn account_wire_debug_response_summary(body: &[u8]) -> serde_json::Value {
    let mut body_hasher = Sha256::new();
    body_hasher.update(body);
    let body_sha256 = hex::encode(body_hasher.finalize());
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return json!({
            "parseOk": false,
            "bodyBytes": body.len(),
            "bodySha256": body_sha256,
        });
    };
    let content = value
        .get("content")
        .or_else(|| value.pointer("/message/content"));
    json!({
        "parseOk": true,
        "bodyBytes": body.len(),
        "bodySha256": body_sha256,
        "type": value.get("type").and_then(serde_json::Value::as_str),
        "id": value.get("id").and_then(serde_json::Value::as_str),
        "model": value.get("model").and_then(serde_json::Value::as_str),
        "stopReason": external_stop_reason_from_value(&value),
        "content": account_wire_debug_content_summary(content),
        "usageCandidates": collect_account_usage_debug_candidates(&value),
    })
}

fn account_wire_debug_content_summary(content: Option<&serde_json::Value>) -> serde_json::Value {
    let Some(content) = content else {
        return json!({ "kind": "missing" });
    };
    match content {
        serde_json::Value::String(text) => {
            json!({
                "kind": "string",
                "text": account_wire_debug_text_summary(text),
            })
        }
        serde_json::Value::Array(items) => {
            let mut item_types: BTreeMap<String, u64> = BTreeMap::new();
            let mut text_part_count = 0u64;
            let mut text_chars = 0u64;
            let mut text_bytes = 0u64;
            let mut text_hasher = Sha256::new();
            let mut text_samples = Vec::new();
            let mut thinking_part_count = 0u64;
            let mut thinking_chars = 0u64;
            let mut tool_result_count = 0u64;
            let mut tool_use_count = 0u64;
            let mut server_tool_use_count = 0u64;
            for item in items {
                let item_type = item
                    .get("type")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown");
                *item_types.entry(item_type.to_string()).or_default() += 1;
                match item_type {
                    "text" => {
                        if let Some(text) = item.get("text").and_then(serde_json::Value::as_str) {
                            let chars = text.chars().count() as u64;
                            text_part_count = text_part_count.saturating_add(1);
                            text_chars = text_chars.saturating_add(chars);
                            text_bytes = text_bytes.saturating_add(text.len() as u64);
                            text_hasher.update(text.as_bytes());
                            if text_samples.len() < ACCOUNT_WIRE_DEBUG_TEXT_SAMPLE_LIMIT {
                                text_samples.push(account_wire_debug_text_summary(text));
                            }
                        }
                    }
                    "thinking" => {
                        if let Some(thinking) =
                            item.get("thinking").and_then(serde_json::Value::as_str)
                        {
                            thinking_part_count = thinking_part_count.saturating_add(1);
                            thinking_chars =
                                thinking_chars.saturating_add(thinking.chars().count() as u64);
                        }
                    }
                    "tool_result" => {
                        tool_result_count = tool_result_count.saturating_add(1);
                    }
                    "tool_use" => {
                        tool_use_count = tool_use_count.saturating_add(1);
                    }
                    "server_tool_use" => {
                        server_tool_use_count = server_tool_use_count.saturating_add(1);
                    }
                    _ => {}
                }
            }
            let text_sha256 = if text_part_count > 0 {
                Some(hex::encode(text_hasher.finalize()))
            } else {
                None
            };
            json!({
                "kind": "array",
                "items": items.len(),
                "itemTypes": item_types,
                "textPartCount": text_part_count,
                "textChars": text_chars,
                "textBytes": text_bytes,
                "textSha256": text_sha256,
                "textSamples": text_samples,
                "thinkingPartCount": thinking_part_count,
                "thinkingChars": thinking_chars,
                "toolUseCount": tool_use_count,
                "serverToolUseCount": server_tool_use_count,
                "toolResultCount": tool_result_count,
            })
        }
        other => json!({
            "kind": match other {
                serde_json::Value::Null => "null",
                serde_json::Value::Bool(_) => "bool",
                serde_json::Value::Number(_) => "number",
                serde_json::Value::Object(_) => "object",
                serde_json::Value::Array(_) => "array",
                serde_json::Value::String(_) => "string",
            },
        }),
    }
}

fn account_wire_debug_text_summary(text: &str) -> serde_json::Value {
    let chars = text.chars().count();
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let preview = text
        .chars()
        .take(ACCOUNT_WIRE_DEBUG_TEXT_PREVIEW_CHARS)
        .collect::<String>();
    json!({
        "chars": chars,
        "bytes": text.len(),
        "sha256": hex::encode(hasher.finalize()),
        "preview": preview,
        "previewTruncated": chars > ACCOUNT_WIRE_DEBUG_TEXT_PREVIEW_CHARS,
    })
}

struct ExternalUsageDebugNonStreamRecordContext<'a> {
    config: &'a AccountsConfig,
    route: &'a AccountRouteRequest,
    pool: &'a Account,
    outbound_model: Option<&'a str>,
    status: StatusCode,
    response_headers: &'a HeaderMap,
    outbound_body: &'a Bytes,
    upstream_body: &'a Bytes,
    projected: &'a ProjectedNonStreamBody,
}

fn account_usage_debug_non_stream_record(ctx: ExternalUsageDebugNonStreamRecordContext<'_>) {
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
    if !usage_debug_enabled(config) {
        return;
    }
    let max_bytes = usage_debug_max_body_bytes(config);
    let raw_json = serde_json::from_slice::<serde_json::Value>(upstream_body).ok();
    let payload = json!({
        "kind": "account_usage_debug",
        "stage": "non_stream_success",
        "route": account_usage_debug_route_context(route, pool, outbound_model),
        "upstream": {
            "status": status.as_u16(),
            "headers": account_usage_debug_header_context(response_headers),
            "body": account_usage_debug_bytes(upstream_body, max_bytes),
            "rawUsageCandidates": raw_json
                .as_ref()
                .map(collect_account_usage_debug_candidates)
                .unwrap_or_default(),
        },
        "outboundRequest": {
            "body": account_usage_debug_bytes(outbound_body, max_bytes),
        },
        "accountProcessing": {
            "usageCapture": account_usage_debug_capture(&projected.usage_capture),
            "protocolContamination": projected.protocol_contamination,
            "downstreamBodyChanged": projected.body.as_ref() != upstream_body.as_ref(),
            "downstreamBody": account_usage_debug_bytes(&projected.body, max_bytes),
        },
    });
    spawn_account_usage_debug_write(config, route, "non-stream", payload);
}

fn account_wire_debug_non_stream_record(ctx: ExternalUsageDebugNonStreamRecordContext<'_>) {
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
    if !wire_debug_enabled(config) {
        return;
    }
    let max_bytes = wire_debug_max_body_bytes(config);
    let upstream_json = serde_json::from_slice::<serde_json::Value>(upstream_body).ok();
    let downstream_json = serde_json::from_slice::<serde_json::Value>(&projected.body).ok();
    let payload = json!({
        "kind": "account_wire_debug",
        "stage": "non_stream_final",
        "route": account_wire_debug_route_context(route, pool, config, outbound_model),
        "inboundRequest": account_wire_debug_request_snapshots(route, max_bytes),
        "outboundRequest": {
            "body": account_wire_debug_bytes(outbound_body, max_bytes),
            "summary": account_wire_debug_request_summary(outbound_body),
            "differsFromEffectiveRawBody": outbound_body.as_ref() != route.effective_raw_body.as_ref(),
        },
        "upstream": {
            "status": status.as_u16(),
            "headers": account_usage_debug_header_context(response_headers),
            "body": account_wire_debug_bytes(upstream_body, max_bytes),
            "summary": account_wire_debug_response_summary(upstream_body),
            "usageCandidates": upstream_json
                .as_ref()
                .map(collect_account_usage_debug_candidates)
                .unwrap_or_default(),
        },
        "processing": {
            "usageCapture": account_usage_debug_capture(&projected.usage_capture),
            "protocolContamination": projected.protocol_contamination,
            "downstreamBodyChanged": projected.body.as_ref() != upstream_body.as_ref(),
        },
        "downstream": {
            "body": account_wire_debug_bytes(&projected.body, max_bytes),
            "summary": account_wire_debug_response_summary(&projected.body),
            "usageCandidates": downstream_json
                .as_ref()
                .map(collect_account_usage_debug_candidates)
                .unwrap_or_default(),
        },
    });
    spawn_account_wire_debug_write(config, route, "non-stream", payload);
}

struct ExternalUsageDebugStreamRecordContext<'a> {
    config: &'a AccountsConfig,
    route: &'a AccountRouteRequest,
    pool: &'a Account,
    outbound_model: Option<&'a str>,
    status: UsageRecordStatus,
    response_status: StatusCode,
    response_content_type: Option<&'a str>,
    outbound_body: &'a Bytes,
    capture: Option<&'a Arc<SyncMutex<ExternalUsageCapture>>>,
    billing: Option<&'a AccountBilling>,
    estimated_output_tokens: i32,
    terminal_message: Option<&'a str>,
}

fn account_usage_debug_stream_record(ctx: ExternalUsageDebugStreamRecordContext<'_>) {
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
    if !usage_debug_enabled(config) {
        return;
    }
    let max_bytes = usage_debug_max_body_bytes(config);
    let capture_snapshot = capture.map(|capture| capture.lock().clone());
    let payload = json!({
        "kind": "account_usage_debug",
        "stage": "stream_final",
        "route": account_usage_debug_route_context(route, pool, outbound_model),
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
            "body": account_usage_debug_bytes(outbound_body, max_bytes),
        },
        "accountProcessing": {
            "terminalStatus": status,
            "terminalMessage": terminal_message,
            "estimatedOutputTokensFromForwardedStream": estimated_output_tokens,
            "usageCapture": capture_snapshot
                .as_ref()
                .map(account_usage_debug_capture),
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
    spawn_account_usage_debug_write(config, route, "stream", payload);
}

fn account_wire_debug_stream_record(ctx: ExternalUsageDebugStreamRecordContext<'_>) {
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
    if !wire_debug_enabled(config) {
        return;
    }
    let max_bytes = wire_debug_max_body_bytes(config);
    let capture_snapshot = capture.map(|capture| capture.lock().clone());
    let payload = json!({
        "kind": "account_wire_debug",
        "stage": "stream_final",
        "route": account_wire_debug_route_context(route, pool, config, outbound_model),
        "inboundRequest": account_wire_debug_request_snapshots(route, max_bytes),
        "outboundRequest": {
            "body": account_wire_debug_bytes(outbound_body, max_bytes),
            "summary": account_wire_debug_request_summary(outbound_body),
            "differsFromEffectiveRawBody": outbound_body.as_ref() != route.effective_raw_body.as_ref(),
        },
        "upstream": {
            "status": response_status.as_u16(),
            "headers": {
                "contentType": response_content_type.unwrap_or_default(),
            },
            "rawStream": capture_snapshot
                .as_ref()
                .map(|capture| &capture.wire_debug.raw_upstream_stream),
        },
        "processing": {
            "terminalStatus": status,
            "terminalMessage": terminal_message,
            "estimatedOutputTokensFromForwardedStream": estimated_output_tokens,
            "usageCapture": capture_snapshot
                .as_ref()
                .map(account_usage_debug_capture),
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
        "downstream": {
            "processedStream": capture_snapshot
                .as_ref()
                .map(|capture| &capture.wire_debug.processed_downstream_stream),
        },
    });
    spawn_account_wire_debug_write(config, route, "stream", payload);
}

fn collect_account_usage_debug_candidates(
    value: &serde_json::Value,
) -> Vec<ExternalUsageDebugUsageCandidate> {
    let mut out = Vec::new();
    collect_account_usage_debug_candidates_inner(value, "$", false, &mut out);
    out
}

fn collect_account_usage_debug_candidates_inner(
    value: &serde_json::Value,
    path: &str,
    key_is_usage: bool,
    out: &mut Vec<ExternalUsageDebugUsageCandidate>,
) {
    if out.len() >= ACCOUNT_USAGE_DEBUG_USAGE_SAMPLE_LIMIT {
        return;
    }
    if (key_is_usage || account_usage_debug_value_has_usage_tokens(value)) && value.is_object() {
        let normalized =
            cache_usage_from_any_value(value).map(|usage| usage.to_anthropic_usage_json());
        let raw_json = serde_json::to_string(value).unwrap_or_else(|_| "null".to_string());
        let (raw_json, raw_json_truncated) =
            truncate_usage_debug_text(&raw_json, ACCOUNT_USAGE_DEBUG_USAGE_JSON_BYTES);
        out.push(ExternalUsageDebugUsageCandidate {
            path: path.to_string(),
            raw_json,
            raw_json_truncated,
            normalized_anthropic_usage: normalized,
        });
    }
    if out.len() >= ACCOUNT_USAGE_DEBUG_USAGE_SAMPLE_LIMIT {
        return;
    }
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                if out.len() >= ACCOUNT_USAGE_DEBUG_USAGE_SAMPLE_LIMIT {
                    break;
                }
                let child_path = account_usage_debug_object_path(path, key);
                collect_account_usage_debug_candidates_inner(
                    child,
                    &child_path,
                    key.eq_ignore_ascii_case("usage"),
                    out,
                );
            }
        }
        serde_json::Value::Array(items) => {
            for (idx, child) in items.iter().enumerate() {
                if out.len() >= ACCOUNT_USAGE_DEBUG_USAGE_SAMPLE_LIMIT {
                    break;
                }
                let child_path = format!("{path}[{idx}]");
                collect_account_usage_debug_candidates_inner(child, &child_path, false, out);
            }
        }
        _ => {}
    }
}

fn account_usage_debug_object_path(parent: &str, key: &str) -> String {
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

fn account_usage_debug_value_has_usage_tokens(value: &serde_json::Value) -> bool {
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
enum AccountSelectionFailureKind {
    AdmissionSaturated,
    BreakerOpen,
    PostgresError,
    PostgresTimeout,
}

impl AccountSelectionFailureKind {
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
struct AccountSelectionUnavailable {
    kind: AccountSelectionFailureKind,
    retry_after: Duration,
}

#[derive(Debug, Default)]
struct AccountSelectionBreakerState {
    generation: u64,
    consecutive_failures: usize,
    open_until: Option<Instant>,
    recovery_probe_in_flight: bool,
}

#[derive(Debug, Default)]
struct AccountSelectionBreakerStats {
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
struct AccountSelectionBreaker {
    state: SyncMutex<AccountSelectionBreakerState>,
    stats: AccountSelectionBreakerStats,
    operation_semaphore: Arc<Semaphore>,
    backoffs: Vec<Duration>,
    jitter_percent: u8,
    min_failure_backoff: Duration,
}

impl Default for AccountSelectionBreaker {
    fn default() -> Self {
        Self::new_with_min_backoff(
            ACCOUNT_SELECTION_MAX_IN_FLIGHT,
            ACCOUNT_SELECTION_BREAKER_BACKOFF_MILLIS
                .into_iter()
                .map(Duration::from_millis)
                .collect(),
            ACCOUNT_SELECTION_BREAKER_JITTER_PERCENT,
            ACCOUNT_SELECTION_RETRY_AFTER,
        )
    }
}

impl AccountSelectionBreaker {
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
            state: SyncMutex::new(AccountSelectionBreakerState::default()),
            stats: AccountSelectionBreakerStats::default(),
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

    fn try_begin(self: &Arc<Self>) -> Result<AccountSelectionPermit, AccountSelectionUnavailable> {
        let operation_permit = match self.operation_semaphore.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                self.stats.saturated.fetch_add(1, Ordering::Relaxed);
                self.stats.fail_fast.fetch_add(1, Ordering::Relaxed);
                self.stats.suppressed.fetch_add(1, Ordering::Relaxed);
                return Err(AccountSelectionUnavailable {
                    kind: AccountSelectionFailureKind::AdmissionSaturated,
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
                return Err(AccountSelectionUnavailable {
                    kind: AccountSelectionFailureKind::BreakerOpen,
                    retry_after: open_until.saturating_duration_since(now),
                });
            }
            Some(_) => {
                if state.recovery_probe_in_flight {
                    self.stats.fail_fast.fetch_add(1, Ordering::Relaxed);
                    self.stats.suppressed.fetch_add(1, Ordering::Relaxed);
                    return Err(AccountSelectionUnavailable {
                        kind: AccountSelectionFailureKind::BreakerOpen,
                        retry_after: ACCOUNT_SELECTION_RETRY_AFTER,
                    });
                }
                state.recovery_probe_in_flight = true;
                self.stats.recovery_probes.fetch_add(1, Ordering::Relaxed);
                true
            }
            None => false,
        };
        self.stats.postgres_attempts.fetch_add(1, Ordering::Relaxed);
        Ok(AccountSelectionPermit {
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
                "账号 PostgreSQL selection 恢复，关闭准入熔断"
            );
        }
    }

    fn complete_failure(
        &self,
        generation: u64,
        recovery_probe: bool,
        kind: AccountSelectionFailureKind,
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
            "账号 PostgreSQL selection 不可用，短期暂停新的查询"
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

struct AccountSelectionPermit {
    breaker: Arc<AccountSelectionBreaker>,
    generation: u64,
    recovery_probe: bool,
    completed: bool,
    _operation_permit: OwnedSemaphorePermit,
}

impl AccountSelectionPermit {
    fn success(mut self) {
        self.completed = true;
        self.breaker
            .complete_success(self.generation, self.recovery_probe);
    }

    fn failure(mut self, kind: AccountSelectionFailureKind) {
        self.completed = true;
        self.breaker
            .complete_failure(self.generation, self.recovery_probe, kind);
    }
}

impl Drop for AccountSelectionPermit {
    fn drop(&mut self) {
        if !self.completed && self.recovery_probe {
            self.breaker.cancel_probe(self.generation);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccountCoordinatorFailureKind {
    RedisError,
    Timeout,
}

impl AccountCoordinatorFailureKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::RedisError => "redis_error",
            Self::Timeout => "timeout",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccountCoordinatorOpenKind {
    BreakerOpen,
    AdmissionSaturated,
}

#[derive(Debug, Clone, Copy)]
struct AccountCoordinatorOpen {
    kind: AccountCoordinatorOpenKind,
    retry_after: Duration,
}

impl std::fmt::Display for AccountCoordinatorOpen {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reason = match self.kind {
            AccountCoordinatorOpenKind::BreakerOpen => "breaker is open",
            AccountCoordinatorOpenKind::AdmissionSaturated => "admission saturated",
        };
        write!(
            formatter,
            "account coordinator {reason}; retry after {}ms",
            self.retry_after.as_millis().max(1)
        )
    }
}

impl std::error::Error for AccountCoordinatorOpen {}

#[derive(Debug)]
struct AccountCoordinatorBreakerState {
    generation: u64,
    consecutive_failures: usize,
    open_until: Option<Instant>,
    recovery_probe_in_flight: bool,
}

impl Default for AccountCoordinatorBreakerState {
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
struct AccountCoordinatorBreakerStats {
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
struct AccountCoordinatorBreaker {
    state: SyncMutex<AccountCoordinatorBreakerState>,
    stats: AccountCoordinatorBreakerStats,
    backoffs: Vec<Duration>,
    operation_semaphore: Arc<Semaphore>,
}

impl Default for AccountCoordinatorBreaker {
    fn default() -> Self {
        Self::new(
            ACCOUNT_COORDINATOR_BREAKER_BACKOFF_SECS
                .into_iter()
                .map(Duration::from_secs)
                .collect(),
        )
    }
}

impl AccountCoordinatorBreaker {
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
            state: SyncMutex::new(AccountCoordinatorBreakerState::default()),
            stats: AccountCoordinatorBreakerStats::default(),
            backoffs,
            operation_semaphore: Arc::new(Semaphore::new(ACCOUNT_COORDINATOR_MAX_IN_FLIGHT)),
        }
    }

    fn backoff_for_failure(&self, consecutive_failures: usize) -> Duration {
        self.backoffs[consecutive_failures
            .saturating_sub(1)
            .min(self.backoffs.len().saturating_sub(1))]
    }

    fn try_begin(self: &Arc<Self>) -> Result<AccountCoordinatorPermit, AccountCoordinatorOpen> {
        let operation_permit = match self.operation_semaphore.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                self.stats.saturated.fetch_add(1, Ordering::Relaxed);
                self.stats.fail_fast.fetch_add(1, Ordering::Relaxed);
                self.stats.suppressed.fetch_add(1, Ordering::Relaxed);
                return Err(AccountCoordinatorOpen {
                    kind: AccountCoordinatorOpenKind::AdmissionSaturated,
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
                return Err(AccountCoordinatorOpen {
                    kind: AccountCoordinatorOpenKind::BreakerOpen,
                    retry_after: open_until.saturating_duration_since(now),
                });
            }
            Some(_) => {
                if state.recovery_probe_in_flight {
                    self.stats.fail_fast.fetch_add(1, Ordering::Relaxed);
                    self.stats.suppressed.fetch_add(1, Ordering::Relaxed);
                    return Err(AccountCoordinatorOpen {
                        kind: AccountCoordinatorOpenKind::BreakerOpen,
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
        Ok(AccountCoordinatorPermit {
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
                "账号 Redis coordinator 恢复，关闭准入熔断"
            );
        }
    }

    fn complete_failure(
        &self,
        generation: u64,
        recovery_probe: bool,
        kind: AccountCoordinatorFailureKind,
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
            "账号 Redis coordinator 不可用，暂停新的账号准入"
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

struct AccountCoordinatorPermit {
    breaker: Arc<AccountCoordinatorBreaker>,
    generation: u64,
    recovery_probe: bool,
    completed: bool,
    _operation_permit: OwnedSemaphorePermit,
}

impl AccountCoordinatorPermit {
    fn success(mut self) {
        self.completed = true;
        self.breaker
            .complete_success(self.generation, self.recovery_probe);
    }

    fn failure(mut self, kind: AccountCoordinatorFailureKind) {
        self.completed = true;
        self.breaker
            .complete_failure(self.generation, self.recovery_probe, kind);
    }
}

impl Drop for AccountCoordinatorPermit {
    fn drop(&mut self) {
        if !self.completed && self.recovery_probe {
            self.breaker.cancel_probe(self.generation);
        }
    }
}

#[derive(Clone)]
pub struct AccountManager {
    postgres: Arc<PostgresStore>,
    redis: Arc<RedisStore>,
    instance_id: String,
    dispatch_counters: Arc<DispatchCounters>,
    proxy_clients: Arc<SyncMutex<HashMap<String, reqwest::Client>>>,
    client: reqwest::Client,
    http1_title_case_client: reqwest::Client,
    native_tls_client: reqwest::Client,
    native_tls_http1_title_case_client: reqwest::Client,
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
    /// PG query bound and caller wait bound for the authoritative snapshot. Production uses
    /// the constants; tests that hold a table lock raise them so the refresh cannot time out
    /// while the test is still releasing the lock.
    authoritative_pool_snapshot_query_timeout: Duration,
    authoritative_pool_snapshot_wait_timeout: Duration,
    #[cfg(test)]
    static_pool_snapshot_pg_loads: Arc<AtomicU64>,
    #[cfg(test)]
    static_pool_snapshot_background_refreshes: Arc<AtomicU64>,
    #[cfg(test)]
    static_pool_snapshot_background_in_flight: Arc<AtomicU64>,
    #[cfg(test)]
    authoritative_pool_snapshot_pg_loads: Arc<AtomicU64>,
    coordinator_breaker: Arc<AccountCoordinatorBreaker>,
    selection_breaker: Arc<AccountSelectionBreaker>,
    selection_saturated: Arc<AtomicU64>,
    dispatch_fence_flights: Arc<SyncMutex<HashMap<(u64, u64), Arc<PoolDispatchFenceFlight>>>>,
    #[cfg(test)]
    dispatch_fence_pg_loads: Arc<AtomicU64>,
    #[cfg(test)]
    dispatch_after_prepare_gate: Arc<SyncMutex<Option<Arc<TestDispatchAfterPrepareGate>>>>,
    #[cfg(test)]
    authoritative_refresh_end_gate: Arc<SyncMutex<Option<Arc<TestDispatchAfterPrepareGate>>>>,
    observed_pool_data_generation: Arc<AtomicU64>,
    success_reset_recent: Arc<SyncMutex<HashMap<u64, Instant>>>,
    success_reset_semaphore: Arc<Semaphore>,
    quality_sample_semaphore: Arc<Semaphore>,
    /// 冷却 / 观察期落审计的节流：同一账号同一原因 60 秒内只记一次。
    account_event_throttle: Arc<AccountEventThrottle>,
    /// 质量采样快照落库的节流：每账号每 60 秒最多一行。
    quality_history_throttle: Arc<AccountEventThrottle>,
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
    release_dispatcher: Arc<AccountReleaseDispatcher>,
    degraded_fallback_local_leases: Arc<SyncMutex<HashMap<(u64, u32), Arc<Semaphore>>>>,
    degraded_global_leases: Arc<SyncMutex<HashMap<u32, Arc<Semaphore>>>>,
    local_mutation_dispatch_trust: Arc<SyncMutex<HashMap<(u64, u64), Instant>>>,
}

struct AccountLease {
    manager: AccountManager,
    pool_id: u64,
    lease_id: String,
    coordination_epoch: String,
    state: AccountLeaseState,
    max_age: Duration,
    heartbeat: Option<AccountLeaseHeartbeat>,
    release_permit: Option<OwnedSemaphorePermit>,
    global_permit: Option<OwnedSemaphorePermit>,
    _in_flight_observation: InFlightObservation,
}

struct InFlightObservation(Arc<DispatchCounters>);

impl InFlightObservation {
    fn new(counters: Arc<DispatchCounters>) -> Self {
        counters.in_flight.fetch_add(1, Ordering::Relaxed);
        Self(counters)
    }
}

impl Drop for InFlightObservation {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::Relaxed);
    }
}

struct AccountLeaseHeartbeat {
    state: Arc<AccountLeaseHeartbeatState>,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Default)]
struct AccountLeaseHeartbeatState {
    lost: AtomicBool,
    attempts: AtomicU64,
    failures: AtomicU64,
    lost_notify: Notify,
}

impl AccountLeaseHeartbeatState {
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

impl AccountLeaseHeartbeat {
    fn spawn(
        manager: AccountManager,
        pool_id: u64,
        lease_id: String,
        coordination_epoch: String,
        max_age: Duration,
    ) -> Self {
        let state = Arc::new(AccountLeaseHeartbeatState::default());
        let task_state = state.clone();
        let task = tokio::spawn(async move {
            run_account_lease_heartbeat(
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
enum AccountLeaseState {
    Pending,
    Confirmed,
    DegradedFallbackLocal,
    Disarmed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum AccountLeaseReleaseKind {
    Pending,
    Confirmed,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AccountReleaseIntent {
    pool_id: u64,
    lease_id: String,
    release_kind: AccountLeaseReleaseKind,
}

#[derive(Default)]
struct AccountReleaseDispatcherState {
    pending: HashMap<AccountReleaseIntent, AccountPendingRelease>,
    order: VecDeque<AccountReleaseIntent>,
    system_failures: u32,
    system_retry_at: Option<Instant>,
    worker_running: bool,
}

struct AccountPendingRelease {
    _permit: OwnedSemaphorePermit,
    failures: u32,
    next_attempt_at: Instant,
}

impl AccountReleaseDispatcherState {
    fn next_ready_batch(
        &mut self,
        now: Instant,
        limit: usize,
    ) -> (Vec<AccountReleaseIntent>, Option<Instant>) {
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
struct AccountReleaseDispatcherStats {
    enqueued: AtomicU64,
    deduplicated: AtomicU64,
    completed: AtomicU64,
    retries: AtomicU64,
    worker_starts: AtomicU64,
    spawn_failures: AtomicU64,
}

struct AccountReleaseDispatcher {
    redis: Arc<RedisStore>,
    capacity_signal: Arc<CapacitySignal>,
    selection_runtime_snapshot: Arc<SyncMutex<Option<CachedSelectionRuntimeSnapshot>>>,
    capacity: Arc<Semaphore>,
    drained_notify: Notify,
    work_notify: Notify,
    state: SyncMutex<AccountReleaseDispatcherState>,
    stats: AccountReleaseDispatcherStats,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountReleaseDrainReport {
    pub drained: bool,
    pub pending: usize,
    pub enqueued: u64,
    pub completed: u64,
    pub retries: u64,
    pub worker_starts: u64,
    pub spawn_failures: u64,
}

impl AccountReleaseDispatcher {
    fn new(
        redis: Arc<RedisStore>,
        capacity_signal: Arc<CapacitySignal>,
        selection_runtime_snapshot: Arc<SyncMutex<Option<CachedSelectionRuntimeSnapshot>>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            redis,
            capacity_signal,
            selection_runtime_snapshot,
            capacity: Arc::new(Semaphore::new(ACCOUNT_RELEASE_CAPACITY)),
            drained_notify: Notify::new(),
            work_notify: Notify::new(),
            state: SyncMutex::new(AccountReleaseDispatcherState::default()),
            stats: AccountReleaseDispatcherStats::default(),
        })
    }

    fn try_reserve(&self) -> Option<OwnedSemaphorePermit> {
        self.capacity.clone().try_acquire_owned().ok()
    }

    fn enqueue(self: &Arc<Self>, intent: AccountReleaseIntent, permit: OwnedSemaphorePermit) {
        let should_start = {
            let mut state = self.state.lock();
            if state.pending.contains_key(&intent) {
                self.stats.deduplicated.fetch_add(1, Ordering::Relaxed);
            } else {
                state.order.push_back(intent.clone());
                state.pending.insert(
                    intent,
                    AccountPendingRelease {
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
                "账号 Redis release worker 无可用 Tokio runtime，lease 将保留到 TTL 回收"
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
                state.next_ready_batch(Instant::now(), ACCOUNT_RELEASE_BATCH_SIZE)
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
                .map(|intent| AccountLeaseReleaseRequest {
                    pool_id: intent.pool_id,
                    lease_id: intent.lease_id.clone(),
                    pending: intent.release_kind == AccountLeaseReleaseKind::Pending,
                })
                .collect::<Vec<_>>();
            let result = timeout(
                ACCOUNT_QUEUE_REDIS_OPERATION_TIMEOUT,
                self.redis.release_account_leases_batch(&requests),
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
                    "账号 Redis release 批处理未完整完成，保留 intent 并重试"
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

    async fn drain(&self, drain_timeout: Duration) -> AccountReleaseDrainReport {
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
        AccountReleaseDrainReport {
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

impl AccountLease {
    fn pending(
        manager: AccountManager,
        pool_id: u64,
        lease_id: String,
        coordination_epoch: String,
        max_age: Duration,
        release_permit: OwnedSemaphorePermit,
    ) -> Self {
        Self {
            _in_flight_observation: InFlightObservation::new(manager.dispatch_counters.clone()),
            manager,
            pool_id,
            lease_id,
            coordination_epoch,
            state: AccountLeaseState::Pending,
            max_age,
            heartbeat: None,
            release_permit: Some(release_permit),
            global_permit: None,
        }
    }

    fn degraded_fallback_local(
        manager: AccountManager,
        pool_id: u64,
        lease_id: String,
        release_permit: OwnedSemaphorePermit,
        global_permit: Option<OwnedSemaphorePermit>,
    ) -> Self {
        Self {
            _in_flight_observation: InFlightObservation::new(manager.dispatch_counters.clone()),
            manager,
            pool_id,
            lease_id,
            coordination_epoch: "degraded-fallback-local".to_string(),
            state: AccountLeaseState::DegradedFallbackLocal,
            max_age: Duration::from_secs(ACCOUNT_LEASE_MAX_AGE_SECS),
            heartbeat: None,
            release_permit: Some(release_permit),
            global_permit,
        }
    }

    fn disarm(mut self) {
        self.state = AccountLeaseState::Disarmed;
    }

    fn confirm(&mut self) {
        debug_assert_eq!(self.state, AccountLeaseState::Pending);
        self.state = AccountLeaseState::Confirmed;
        self.manager.invalidate_account_runtime_capacity_state();
        self.heartbeat = Some(AccountLeaseHeartbeat::spawn(
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

impl Drop for AccountLease {
    fn drop(&mut self) {
        if let Some(heartbeat) = self.heartbeat.take() {
            heartbeat.stop();
        }
        let release_kind = match self.state {
            AccountLeaseState::Pending => AccountLeaseReleaseKind::Pending,
            AccountLeaseState::Confirmed => AccountLeaseReleaseKind::Confirmed,
            AccountLeaseState::DegradedFallbackLocal => {
                let _ = self.release_permit.take();
                let _ = self.global_permit.take();
                return;
            }
            AccountLeaseState::Disarmed => return,
        };
        self.manager.invalidate_account_runtime_capacity_state();
        let release_permit = self
            .release_permit
            .take()
            .expect("armed account lease must retain release capacity");
        release_account_lease_reliably(
            self.manager.clone(),
            self.pool_id,
            self.lease_id.clone(),
            release_kind,
            release_permit,
        );
    }
}

async fn run_account_lease_heartbeat(
    manager: AccountManager,
    pool_id: u64,
    lease_id: String,
    coordination_epoch: String,
    max_age: Duration,
    state: Arc<AccountLeaseHeartbeatState>,
) {
    let max_age_ms = max_age.as_millis().max(300) as u64;
    let interval = Duration::from_millis(
        (max_age_ms / 3)
            .clamp(100, ACCOUNT_LEASE_TOUCH_INTERVAL_SECS * 1_000)
            .min(max_age_ms / 2),
    );
    let operation_timeout = Duration::from_millis((max_age_ms / 4).clamp(
        50,
        ACCOUNT_COORDINATOR_REDIS_OPERATION_TIMEOUT.as_millis() as u64,
    ));
    let loss_after = max_age.saturating_sub(operation_timeout).max(interval);
    let retry_delay = Duration::from_millis(ACCOUNT_LEASE_HEARTBEAT_RETRY_MILLIS)
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
                    "账号 Redis lease heartbeat 到达 prune 安全截止时间，中止当前上游响应"
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
                    "账号 Redis lease heartbeat 操作未在 prune 安全截止时间前完成，中止当前上游响应"
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
                    "账号 Redis lease heartbeat 发现 lease 已丢失，中止当前上游响应"
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
                        "账号 Redis lease heartbeat 在 prune 安全期限前未恢复，中止当前上游响应"
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

fn release_account_lease_reliably(
    manager: AccountManager,
    pool_id: u64,
    lease_id: String,
    release_kind: AccountLeaseReleaseKind,
    release_permit: OwnedSemaphorePermit,
) {
    manager.release_dispatcher.enqueue(
        AccountReleaseIntent {
            pool_id,
            lease_id,
            release_kind,
        },
        release_permit,
    );
}

async fn release_account_queue_lease_with_retry(
    manager: AccountManager,
    lease_id: String,
    attempts: usize,
) -> anyhow::Result<bool> {
    let attempts = attempts.max(1);
    let mut last_error = None;
    for attempt in 0..attempts {
        match timeout(
            ACCOUNT_QUEUE_REDIS_OPERATION_TIMEOUT,
            manager.redis.leave_account_dispatch_queue(&lease_id),
        )
        .await
        {
            Ok(Ok(removed)) => return Ok(removed),
            Ok(Err(err)) => last_error = Some(err),
            Err(_) => {
                last_error = Some(anyhow::anyhow!(
                    "Redis account queue release timed out after {}ms",
                    ACCOUNT_QUEUE_REDIS_OPERATION_TIMEOUT.as_millis()
                ));
            }
        }
        if attempt + 1 < attempts {
            tokio::time::sleep(ACCOUNT_QUEUE_REDIS_RETRY_DELAY).await;
        }
    }
    Err(last_error.expect("at least one account queue release attempt must run"))
}

fn release_account_queue_lease_reliably(manager: AccountManager, lease_id: String) {
    let fallback_manager = manager.clone();
    let fallback_lease_id = lease_id.clone();
    let admitted = spawn_critical_storage_task("释放账号 Redis 调度排队 lease", async move {
        release_account_queue_lease_with_retry(manager, lease_id, 2)
            .await
            .map(|_| ())
    });
    if admitted {
        return;
    }
    spawn_external_release_fallback(
        "关键队列拒绝后异步释放账号 Redis 调度排队 lease",
        async move {
            release_account_queue_lease_with_retry(fallback_manager, fallback_lease_id, 2)
                .await
                .map(|_| ())
        },
    );
}

static ACCOUNT_RELEASE_FALLBACK_SEMAPHORE: OnceLock<Arc<Semaphore>> = OnceLock::new();
static ACCOUNT_RELEASE_FALLBACK_ACCEPTED: AtomicU64 = AtomicU64::new(0);
static ACCOUNT_RELEASE_FALLBACK_REJECTED: AtomicU64 = AtomicU64::new(0);
static ACCOUNT_RELEASE_FALLBACK_FINISHED: AtomicU64 = AtomicU64::new(0);

fn account_release_fallback_semaphore() -> Arc<Semaphore> {
    ACCOUNT_RELEASE_FALLBACK_SEMAPHORE
        .get_or_init(|| Arc::new(Semaphore::new(ACCOUNT_RELEASE_FALLBACK_MAX_IN_FLIGHT)))
        .clone()
}

fn record_external_release_fallback_rejection(description: &'static str, reason: &'static str) {
    let rejected = ACCOUNT_RELEASE_FALLBACK_REJECTED.fetch_add(1, Ordering::Relaxed) + 1;
    if rejected == 1 || rejected.is_power_of_two() {
        tracing::warn!(
            description,
            reason,
            rejected,
            max_in_flight = ACCOUNT_RELEASE_FALLBACK_MAX_IN_FLIGHT,
            "账号 Redis release fallback 已饱和，剩余 lease 将由 TTL 回收"
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
    let Ok(permit) = account_release_fallback_semaphore().try_acquire_owned() else {
        record_external_release_fallback_rejection(description, "fallback_saturated");
        return false;
    };
    ACCOUNT_RELEASE_FALLBACK_ACCEPTED.fetch_add(1, Ordering::Relaxed);
    handle.spawn(async move {
        if let Err(err) = future.await {
            tracing::error!(description, error = %err, "账号 Redis release 有界重试失败，将由 TTL 回收");
        }
        // Return the admission permit before publishing completion. Tests and
        // operational gauges use the completion counter as the point at which
        // the fallback slot is reusable; keeping the permit alive through the
        // counter update creates a one-tick race where `finished == accepted`
        // but one semaphore permit is still held.
        drop(permit);
        ACCOUNT_RELEASE_FALLBACK_FINISHED.fetch_add(1, Ordering::Release);
    });
    true
}

struct AccountQueueGuard {
    manager: AccountManager,
    lease_id: String,
    lease_ttl_secs: u64,
    next_renew_at: Option<Instant>,
    released: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct QueueLeasePolicy {
    ttl_secs: u64,
    renewal_required: bool,
}

fn account_queue_lease_policy(max_wait: Option<Duration>) -> QueueLeasePolicy {
    let Some(max_wait) = max_wait else {
        return QueueLeasePolicy {
            ttl_secs: ACCOUNT_QUEUE_LEASE_SAFETY_MARGIN_SECS,
            renewal_required: true,
        };
    };
    let rounded_wait_secs = max_wait
        .as_secs()
        .saturating_add(u64::from(max_wait.subsec_nanos() > 0));
    QueueLeasePolicy {
        ttl_secs: rounded_wait_secs
            .saturating_add(ACCOUNT_QUEUE_LEASE_SAFETY_MARGIN_SECS)
            .max(ACCOUNT_QUEUE_LEASE_SAFETY_MARGIN_SECS),
        renewal_required: false,
    }
}

impl AccountQueueGuard {
    fn new(manager: AccountManager, lease_id: String, policy: QueueLeasePolicy) -> Self {
        Self {
            manager,
            lease_id,
            lease_ttl_secs: policy.ttl_secs,
            next_renew_at: policy.renewal_required.then(|| {
                let renew_interval_secs =
                    (policy.ttl_secs / 3).clamp(1, ACCOUNT_QUEUE_LEASE_RENEW_INTERVAL_MAX_SECS);
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
            ACCOUNT_QUEUE_REDIS_OPERATION_TIMEOUT,
            self.manager
                .redis
                .renew_account_dispatch_queue(&self.lease_id, self.lease_ttl_secs),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "Redis account queue renewal timed out after {}ms",
                ACCOUNT_QUEUE_REDIS_OPERATION_TIMEOUT.as_millis()
            )
        })??;
        if !renewed {
            anyhow::bail!("Redis account queue lease expired before renewal");
        }
        let renew_interval_secs =
            (self.lease_ttl_secs / 3).clamp(1, ACCOUNT_QUEUE_LEASE_RENEW_INTERVAL_MAX_SECS);
        self.next_renew_at = Some(Instant::now() + Duration::from_secs(renew_interval_secs));
        Ok(())
    }

    fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        release_account_queue_lease_reliably(self.manager.clone(), self.lease_id.clone());
    }
}

impl Drop for AccountQueueGuard {
    fn drop(&mut self) {
        self.release();
    }
}

#[derive(Debug, Clone)]
struct AccountError {
    status: Option<StatusCode>,
    message: String,
    retryable: bool,
    auto_disable_reason: Option<String>,
    cooldown: Option<(Duration, String)>,
    protocol_error: Option<&'static str>,
    raw_upstream_error: Option<RawUpstreamError>,
}

impl AccountError {
    fn with_raw_upstream_error(mut self, raw_upstream_error: Option<RawUpstreamError>) -> Self {
        self.raw_upstream_error = raw_upstream_error;
        self
    }
}

#[derive(Debug, Clone, Default)]
struct UsageErrorDiagnostics {
    status_code: Option<u16>,
    source: Option<String>,
    error_id: Option<String>,
    metadata: Option<serde_json::Value>,
    public_error: Option<UsagePublicError>,
    raw_upstream_error: Option<RawUpstreamError>,
}

#[derive(Debug, Clone, Deserialize)]
struct AccountCooldownState {
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
    Acquired(AccountLease),
    Unavailable(PoolAcquireUnavailable),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PoolDispatchFenceResult {
    Current,
    Changed,
    CoordinatorUnavailable(AccountSelectionUnavailable),
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
    selected_pool: Option<Account>,
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
    /// 被动质量采样状态；`None` 表示无数据，评分时按中性值处理。
    quality: Option<AccountQualityState>,
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
    pools: Arc<Vec<AccountEligibility>>,
    fresh_until: Instant,
    stale_until: Instant,
    load_succeeded: bool,
}

#[derive(Debug, Clone)]
struct CachedAuthoritativePoolSnapshot {
    generation: u64,
    result: Result<Arc<Vec<Account>>, AccountSelectionUnavailable>,
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
    Fresh(Arc<Vec<AccountEligibility>>),
    Stale(Arc<Vec<AccountEligibility>>),
}

#[derive(Debug, Clone)]
enum CachedAuthoritativePoolSnapshotState {
    Fresh(Result<Arc<Vec<Account>>, AccountSelectionUnavailable>),
    Stale(Arc<Vec<Account>>),
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
    unavailable: AccountSelectionUnavailable,
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
    FinalError(AccountFinalError),
}

fn default_true() -> bool {
    true
}

fn default_account_revision() -> u64 {
    1
}

fn default_priority() -> i32 {
    100
}

fn default_account_concurrency() -> u32 {
    10
}

pub fn mask_account_key(value: &str) -> String {
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

impl AccountManager {
    pub fn new(postgres: Arc<PostgresStore>, redis: Arc<RedisStore>) -> Self {
        let capacity_signal = Arc::new(CapacitySignal::default());
        let selection_runtime_snapshot = Arc::new(SyncMutex::new(None));
        let release_dispatcher = AccountReleaseDispatcher::new(
            redis.clone(),
            capacity_signal.clone(),
            selection_runtime_snapshot.clone(),
        );
        Self {
            postgres,
            redis,
            instance_id: uuid::Uuid::new_v4().to_string(),
            dispatch_counters: Arc::new(DispatchCounters::default()),
            proxy_clients: Arc::new(SyncMutex::new(HashMap::new())),
            client: account_reqwest_client(AccountWireProfile::Default, AccountTlsProfile::Default),
            http1_title_case_client: account_reqwest_client(
                AccountWireProfile::Http1TitleCase,
                AccountTlsProfile::Default,
            ),
            native_tls_client: account_reqwest_client(
                AccountWireProfile::Default,
                AccountTlsProfile::NativeTls,
            ),
            native_tls_http1_title_case_client: account_reqwest_client(
                AccountWireProfile::Http1TitleCase,
                AccountTlsProfile::NativeTls,
            ),
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
            static_pool_snapshot_fresh_ttl: ACCOUNT_STATIC_SNAPSHOT_FRESH_TTL,
            static_pool_snapshot_stale_ttl: ACCOUNT_STATIC_SNAPSHOT_STALE_TTL,
            static_pool_snapshot_failure_retry: ACCOUNT_STATIC_SNAPSHOT_FAILURE_RETRY,
            static_pool_snapshot_refresh_timeout: ACCOUNT_STATIC_SNAPSHOT_REFRESH_TIMEOUT,
            authoritative_pool_snapshot_query_timeout: ACCOUNT_SELECTION_POSTGRES_TIMEOUT,
            authoritative_pool_snapshot_wait_timeout: ACCOUNT_AUTHORITATIVE_SNAPSHOT_WAIT_TIMEOUT,
            #[cfg(test)]
            static_pool_snapshot_pg_loads: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            static_pool_snapshot_background_refreshes: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            static_pool_snapshot_background_in_flight: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            authoritative_pool_snapshot_pg_loads: Arc::new(AtomicU64::new(0)),
            coordinator_breaker: Arc::new(AccountCoordinatorBreaker::default()),
            selection_breaker: Arc::new(AccountSelectionBreaker::default()),
            selection_saturated: Arc::new(AtomicU64::new(0)),
            dispatch_fence_flights: Arc::new(SyncMutex::new(HashMap::new())),
            #[cfg(test)]
            dispatch_fence_pg_loads: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            dispatch_after_prepare_gate: Arc::new(SyncMutex::new(None)),
            #[cfg(test)]
            authoritative_refresh_end_gate: Arc::new(SyncMutex::new(None)),
            observed_pool_data_generation: Arc::new(AtomicU64::new(0)),
            success_reset_recent: Arc::new(SyncMutex::new(HashMap::new())),
            success_reset_semaphore: Arc::new(Semaphore::new(ACCOUNT_SUCCESS_RESET_MAX_TASKS)),
            quality_sample_semaphore: Arc::new(Semaphore::new(ACCOUNT_QUALITY_SAMPLE_MAX_TASKS)),
            account_event_throttle: Arc::new(AccountEventThrottle::default()),
            quality_history_throttle: Arc::new(AccountEventThrottle::default()),
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
                ACCOUNT_COORDINATOR_RECOVERY_GRACE_SECS,
            ),
            release_dispatcher,
            degraded_fallback_local_leases: Arc::new(SyncMutex::new(HashMap::new())),
            degraded_global_leases: Arc::new(SyncMutex::new(HashMap::new())),
            local_mutation_dispatch_trust: Arc::new(SyncMutex::new(HashMap::new())),
        }
    }

    pub async fn drain_release_intents(
        &self,
        drain_timeout: Duration,
    ) -> AccountReleaseDrainReport {
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

    #[cfg(test)]
    fn with_authoritative_pool_snapshot_timeouts(
        mut self,
        query_timeout: Duration,
        wait_timeout: Duration,
    ) -> Self {
        self.authoritative_pool_snapshot_query_timeout =
            query_timeout.max(Duration::from_millis(1));
        self.authoritative_pool_snapshot_wait_timeout = wait_timeout.max(Duration::from_millis(1));
        self
    }

    pub fn invalidate_static_pool_snapshot(&self) {
        self.static_pool_snapshot_generation
            .fetch_add(1, Ordering::AcqRel);
        self.static_pool_snapshot.lock().take();
        self.authoritative_pool_snapshot.lock().take();
        self.local_mutation_dispatch_trust.lock().clear();
        self.invalidate_account_policy_state();
    }

    fn static_snapshot_ttls(&self, generation: u64) -> (Duration, Duration) {
        let fresh_ttl = deterministic_duration_jitter(
            self.static_pool_snapshot_fresh_ttl,
            ACCOUNT_STATIC_SNAPSHOT_JITTER_PERCENT,
            generation ^ 0x4652_4553_48,
        );
        let stale_ttl = deterministic_duration_jitter(
            self.static_pool_snapshot_stale_ttl,
            ACCOUNT_STATIC_SNAPSHOT_JITTER_PERCENT,
            generation ^ 0x5354_414c_45,
        )
        .max(fresh_ttl);
        (fresh_ttl, stale_ttl)
    }

    fn publish_static_pool_snapshot_arc(
        &self,
        generation: u64,
        pools: Arc<Vec<AccountEligibility>>,
        load_succeeded: bool,
        fresh_ttl: Duration,
        stale_ttl: Duration,
    ) -> Option<Arc<Vec<AccountEligibility>>> {
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
        pools: Arc<Vec<Account>>,
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

    fn sort_authoritative_pools(pools: &mut [Account]) {
        pools.sort_by_key(|pool| (pool.priority, pool.id));
    }

    fn sort_static_pool_eligibility(pools: &mut [AccountEligibility]) {
        pools.sort_by_key(|pool| pool.id);
    }

    fn apply_local_account_snapshot_mutation(
        &self,
        upsert_pool: Option<&Account>,
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
            static_pools.push(account_eligibility_from_pool(pool));
        }
        Self::sort_static_pool_eligibility(&mut static_pools);
        // 只有手里有一份完整的旧快照时才能就地合并本次变更；旧快照已过期时直接作废，
        // 让下一次请求从 PgSQL 读全量。否则"只含本次变更账号"的快照会被当成完整列表，
        // 其它账号在新快照过期前全部不可调度（低流量时编辑账号即触发）。
        if had_previous_static_pools {
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
        // 同上：没有完整旧快照就不发布局部快照，交给下一次读取走 PgSQL 全量加载。
        if had_previous_authoritative_pools {
            let _ = self.publish_authoritative_pool_snapshot_success_arc(
                generation,
                Arc::new(authoritative_pools),
                ACCOUNT_LOCAL_MUTATION_AUTHORITATIVE_FRESH_TTL,
                ACCOUNT_AUTHORITATIVE_SNAPSHOT_STALE_TTL,
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
                now + ACCOUNT_LOCAL_MUTATION_DISPATCH_TRUST_TTL,
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
        self.invalidate_account_policy_state();
    }

    pub fn notify_account_data_changed_with_local_pool(
        &self,
        reason: &'static str,
        pool: &Account,
    ) {
        self.apply_local_account_snapshot_mutation(Some(pool), None);
        self.publish_account_data_changed_async(reason, Some(pool.id));
    }

    pub fn notify_account_deleted(&self, reason: &'static str, pool_id: u64) {
        self.apply_local_account_snapshot_mutation(None, Some(pool_id));
        self.forget_account_dispatch_counter(pool_id);
        self.degraded_fallback_local_leases
            .lock()
            .retain(|(existing_pool_id, _), _| *existing_pool_id != pool_id);
        self.publish_account_data_changed_async(reason, Some(pool_id));
    }

    pub fn invalidate_account_policy_state(&self) {
        self.selection_runtime_snapshot.lock().take();
        #[cfg(test)]
        self.availability_cache.lock().take();
    }

    fn invalidate_account_runtime_capacity_state(&self) {
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

    pub fn observe_account_data_event(&self, payload: &str) -> bool {
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

    pub async fn publish_account_data_changed(
        &self,
        reason: &str,
        pool_id: Option<u64>,
    ) -> anyhow::Result<u64> {
        self.invalidate_static_pool_snapshot();
        let generation = self
            .redis
            .publish_account_data_changed(reason, pool_id)
            .await?;
        self.observed_pool_data_generation
            .fetch_max(generation, Ordering::AcqRel);
        Ok(generation)
    }

    fn publish_account_data_changed_async(&self, reason: &'static str, pool_id: Option<u64>) {
        let redis = self.redis.clone();
        let observed_generation = self.observed_pool_data_generation.clone();
        let origin = self.instance_id.clone();
        tokio::spawn(async move {
            match redis
                .publish_account_data_changed_with_origin(reason, pool_id, Some(origin.as_str()))
                .await
            {
                Ok(generation) => {
                    observed_generation.fetch_max(generation, Ordering::AcqRel);
                }
                Err(err) => tracing::warn!(
                    reason,
                    pool_id = ?pool_id,
                    error = %err,
                    "发布账号数据跨实例失效通知失败"
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
        pools: Vec<AccountEligibility>,
    ) -> Option<Arc<Vec<AccountEligibility>>> {
        let pools = Arc::new(pools);
        let (fresh_ttl, stale_ttl) = self.static_snapshot_ttls(generation);
        self.publish_static_pool_snapshot_arc(generation, pools, true, fresh_ttl, stale_ttl)
    }

    fn publish_static_pool_snapshot_failure(
        &self,
        generation: u64,
    ) -> Option<Arc<Vec<AccountEligibility>>> {
        let now = Instant::now();
        let failure_retry = deterministic_duration_jitter(
            self.static_pool_snapshot_failure_retry,
            ACCOUNT_STATIC_SNAPSHOT_JITTER_PERCENT,
            generation ^ 0x4641_494c,
        );
        let stale_ttl = deterministic_duration_jitter(
            self.static_pool_snapshot_stale_ttl,
            ACCOUNT_STATIC_SNAPSHOT_JITTER_PERCENT,
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
    ) -> Option<Arc<Vec<AccountEligibility>>> {
        if self.static_pool_snapshot_generation.load(Ordering::Acquire) != generation {
            return None;
        }

        #[cfg(test)]
        self.static_pool_snapshot_pg_loads
            .fetch_add(1, Ordering::Relaxed);
        let started = Instant::now();
        let pools =
            match tokio::time::timeout_at(deadline, self.postgres.list_account_eligibility()).await
            {
                Ok(Ok(pools)) => pools,
                Ok(Err(err)) => {
                    tracing::warn!(
                        error = %err,
                        snapshot_generation = generation,
                        refresh_ms = started.elapsed().as_millis(),
                        "加载账号静态资格快照失败，短期负缓存后重试"
                    );
                    return self.publish_static_pool_snapshot_failure(generation);
                }
                Err(_) => {
                    tracing::warn!(
                        snapshot_generation = generation,
                        refresh_timeout_ms = self.static_pool_snapshot_refresh_timeout.as_millis(),
                        "加载账号静态资格快照超时，短期负缓存后重试"
                    );
                    return self.publish_static_pool_snapshot_failure(generation);
                }
            };
        self.publish_static_pool_snapshot_success(generation, pools)
    }

    async fn load_static_pool_snapshot(&self) -> Arc<Vec<AccountEligibility>> {
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
        result: Result<Arc<Vec<Account>>, AccountSelectionUnavailable>,
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
                let existing_pools = snapshot
                    .as_ref()
                    .filter(|existing| {
                        existing.generation == generation
                            && existing.stale_until > now
                            && existing.result.is_ok()
                    })
                    .and_then(|existing| match &existing.result {
                        Ok(pools) => Some((existing.stale_until, pools.clone())),
                        Err(_) => None,
                    });
                if let Some((stale_until, pools)) = existing_pools {
                    *snapshot = Some(CachedAuthoritativePoolSnapshot {
                        generation,
                        result: Ok(pools),
                        fresh_until: now + ACCOUNT_AUTHORITATIVE_SNAPSHOT_FAILURE_RETRY,
                        stale_until,
                    });
                    return true;
                }
                Err(err)
            }
        };
        let (fresh_until, stale_until) = if result.is_ok() {
            (
                now + ACCOUNT_AUTHORITATIVE_SNAPSHOT_TTL,
                now + ACCOUNT_AUTHORITATIVE_SNAPSHOT_STALE_TTL,
            )
        } else {
            let retry_until = now + ACCOUNT_AUTHORITATIVE_SNAPSHOT_FAILURE_RETRY;
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

    fn cached_static_pool_snapshot_for_local_route(&self) -> Option<Arc<Vec<AccountEligibility>>> {
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

    fn cached_authoritative_pool_snapshot_for_local_route(&self) -> Option<Arc<Vec<Account>>> {
        let generation = self.static_pool_snapshot_generation.load(Ordering::Acquire);
        match self.cached_authoritative_pool_snapshot(generation) {
            Some(CachedAuthoritativePoolSnapshotState::Fresh(Ok(pools))) => Some(pools),
            Some(CachedAuthoritativePoolSnapshotState::Fresh(Err(err))) => {
                tracing::debug!(
                    unavailable_kind = err.kind.as_str(),
                    retry_after_ms = err.retry_after.as_millis() as u64,
                    "账号权威快照当前不可用；本地主路径保持本地调度语义"
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
        refresh_guard: OwnedMutexGuard<()>,
    ) {
        let result = match self.selection_breaker.try_begin() {
            Ok(permit) => {
                #[cfg(test)]
                self.authoritative_pool_snapshot_pg_loads
                    .fetch_add(1, Ordering::Relaxed);
                match timeout(
                    self.authoritative_pool_snapshot_query_timeout,
                    self.postgres.list_dispatchable_accounts(),
                )
                .await
                {
                    Ok(Ok(pools)) => {
                        permit.success();
                        Ok(Arc::new(pools))
                    }
                    Ok(Err(_)) => {
                        permit.failure(AccountSelectionFailureKind::PostgresError);
                        Err(AccountSelectionUnavailable {
                            kind: AccountSelectionFailureKind::PostgresError,
                            retry_after: ACCOUNT_SELECTION_RETRY_AFTER,
                        })
                    }
                    Err(_) => {
                        permit.failure(AccountSelectionFailureKind::PostgresTimeout);
                        Err(AccountSelectionUnavailable {
                            kind: AccountSelectionFailureKind::PostgresTimeout,
                            retry_after: ACCOUNT_SELECTION_RETRY_AFTER,
                        })
                    }
                }
            }
            Err(unavailable) => {
                if unavailable.kind == AccountSelectionFailureKind::AdmissionSaturated {
                    self.selection_saturated.fetch_add(1, Ordering::Relaxed);
                }
                Err(unavailable)
            }
        };
        self.publish_authoritative_pool_snapshot(generation, result);
        // Release the refresh lock before waking waiters. A caller that arrived after the snapshot
        // was invalidated could not take the lock and is waiting for this notification; woken
        // with the lock still held, it would find the lock taken again and wait for a
        // notification that never comes, until its wait timeout.
        drop(refresh_guard);
        self.authoritative_pool_snapshot_changed.notify_waiters();
        #[cfg(test)]
        self.wait_at_authoritative_refresh_end_gate().await;
    }

    #[cfg(test)]
    fn set_authoritative_refresh_end_gate(&self, gate: Arc<TestDispatchAfterPrepareGate>) {
        *self.authoritative_refresh_end_gate.lock() = Some(gate);
    }

    /// Holds a finished refresh before its task returns, as a busy runtime can after the
    /// refresh has woken its waiters. Callers must not depend on this task running again.
    #[cfg(test)]
    async fn wait_at_authoritative_refresh_end_gate(&self) {
        let gate = self.authoritative_refresh_end_gate.lock().take();
        if let Some(gate) = gate {
            gate.prepared.wait().await;
            gate.resume.wait().await;
        }
    }

    async fn load_authoritative_pool_snapshot(
        &self,
    ) -> Result<Arc<Vec<Account>>, AccountSelectionUnavailable> {
        let deadline = Instant::now() + self.authoritative_pool_snapshot_wait_timeout;
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
                return Err(AccountSelectionUnavailable {
                    kind: AccountSelectionFailureKind::PostgresTimeout,
                    retry_after: ACCOUNT_SELECTION_RETRY_AFTER,
                });
            }
        }
    }

    #[cfg(test)]
    fn authoritative_pool_snapshot_pg_loads_for_test(&self) -> u64 {
        self.authoritative_pool_snapshot_pg_loads
            .load(Ordering::Acquire)
    }

    fn mark_account_coordinator_probe_required(&self) {
        self.coordinator_probe_required
            .store(true, Ordering::Release);
        self.coordinator_next_probe_ms.store(0, Ordering::Release);
    }

    fn local_mutation_dispatch_trusted(&self, pool: &Account) -> bool {
        let now = Instant::now();
        let mut trust = self.local_mutation_dispatch_trust.lock();
        trust.retain(|_, expires_at| *expires_at > now);
        trust
            .get(&(pool.id, pool.revision))
            .is_some_and(|expires_at| *expires_at > now)
    }

    async fn account_coordination_epoch(&self) -> anyhow::Result<String> {
        let now_ms = account_coordination_monotonic_ms();
        let cached_epoch = self.coordinator_epoch.lock().clone();
        if let Some(epoch) = cached_epoch.filter(|_| {
            !self.coordinator_probe_required.load(Ordering::Acquire)
                && now_ms < self.coordinator_next_probe_ms.load(Ordering::Acquire)
        }) {
            return Ok(epoch);
        }
        let _local_lock = self.coordinator_reconcile_lock.lock().await;
        let now_ms = account_coordination_monotonic_ms();
        let probe_required = self.coordinator_probe_required.load(Ordering::Acquire);
        let cached_epoch = self.coordinator_epoch.lock().clone();
        let cached_run_id = self.coordinator_run_id.lock().clone();
        if let Some(epoch) = cached_epoch.clone().filter(|_| {
            !probe_required && now_ms < self.coordinator_next_probe_ms.load(Ordering::Acquire)
        }) {
            return Ok(epoch);
        }

        // 持有本地对齐锁期间的每一次 Redis 往返都必须有上限：Redis 变慢时这里不能把
        // 所有等 epoch 的调度请求一起挂住。
        let current_run_id =
            account_coordinator_redis_call("Redis INFO run_id", self.redis.account_redis_run_id())
                .await?;
        match (cached_epoch.as_ref(), cached_run_id.as_ref()) {
            (Some(epoch), Some(run_id)) if *run_id == current_run_id => {
                if !probe_required {
                    self.record_account_coordinator_probe(&current_run_id, epoch);
                    return Ok(epoch.clone());
                }
                match account_coordinator_redis_call(
                    "Redis coordinator guard probe",
                    self.redis.account_coordinator_guard_state_for_epoch(epoch),
                )
                .await?
                {
                    AccountCoordinatorGuardState::Ready { .. }
                    | AccountCoordinatorGuardState::Recovering { .. } => {
                        self.record_account_coordinator_probe(&current_run_id, epoch);
                        return Ok(epoch.clone());
                    }
                    AccountCoordinatorGuardState::EpochMismatch { .. } => {}
                }
            }
            _ => {}
        }

        self.reconcile_account_coordinator_guard(current_run_id)
            .await
    }

    fn record_account_coordinator_probe(&self, run_id: &str, epoch: &str) {
        *self.coordinator_run_id.lock() = Some(run_id.to_string());
        *self.coordinator_epoch.lock() = Some(epoch.to_string());
        self.coordinator_probe_required
            .store(false, Ordering::Release);
        self.coordinator_next_probe_ms.store(
            account_coordination_monotonic_ms().saturating_add(
                Duration::from_secs(ACCOUNT_COORDINATOR_RUN_ID_PROBE_INTERVAL_SECS).as_millis()
                    as u64,
            ),
            Ordering::Release,
        );
    }

    async fn reconcile_account_coordinator_guard(
        &self,
        current_run_id: String,
    ) -> anyhow::Result<String> {
        match timeout(
            ACCOUNT_COORDINATOR_RECONCILE_TIMEOUT,
            self.reconcile_account_coordinator_guard_in_transaction(current_run_id),
        )
        .await
        {
            Ok(Ok((run_id, epoch))) => {
                self.record_account_coordinator_probe(&run_id, &epoch);
                Ok(epoch)
            }
            Ok(Err(err)) => {
                self.mark_account_coordinator_probe_required();
                Err(err)
            }
            Err(_) => {
                self.mark_account_coordinator_probe_required();
                anyhow::bail!(
                    "account coordinator epoch reconcile timed out after {}s",
                    ACCOUNT_COORDINATOR_RECONCILE_TIMEOUT.as_secs()
                )
            }
        }
    }

    /// 在一个带 `lock_timeout` / `statement_timeout` 的事务里完成 epoch 对齐。
    /// 用 `pg_advisory_xact_lock` 而不是会话级锁：外层超时把这个 future 连同事务一起丢弃时，
    /// 连接回池前会回滚，锁随事务释放；会话级锁在这种情况下会一直挂在连接上，
    /// 后续所有实例的对齐最长要等到该连接被回收。
    async fn reconcile_account_coordinator_guard_in_transaction(
        &self,
        _probe_run_id: String,
    ) -> anyhow::Result<(String, String)> {
        let mut tx = timeout(
            ACCOUNT_SELECTION_POSTGRES_TIMEOUT,
            self.postgres.pool().begin(),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "acquiring a PostgreSQL connection for coordinator reconcile timed out after {}ms",
                ACCOUNT_SELECTION_POSTGRES_TIMEOUT.as_millis()
            )
        })??;
        sqlx::query(&format!(
            "SET LOCAL lock_timeout = '{}ms'",
            ACCOUNT_COORDINATOR_POSTGRES_LOCK_TIMEOUT.as_millis()
        ))
        .execute(&mut *tx)
        .await?;
        sqlx::query(&format!(
            "SET LOCAL statement_timeout = '{}ms'",
            ACCOUNT_COORDINATOR_POSTGRES_STATEMENT_TIMEOUT.as_millis()
        ))
        .execute(&mut *tx)
        .await?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(ACCOUNT_COORDINATOR_POSTGRES_LOCK_ID)
            .execute(&mut *tx)
            .await?;

        // 拿到锁后重新读 run_id：探测时读到的值在等锁期间可能已经因 Redis 重启失效。
        let current_run_id =
            account_coordinator_redis_call("Redis INFO run_id", self.redis.account_redis_run_id())
                .await?;
        let existing: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT config FROM runtime_config WHERE id = $1")
                .bind(ACCOUNT_COORDINATOR_RUNTIME_CONFIG_ID)
                .fetch_optional(&mut *tx)
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

        let existing_epoch =
            existing_epoch.filter(|_| existing_run_id == Some(current_run_id.as_str()));
        if let Some(epoch) = existing_epoch {
            match account_coordinator_redis_call(
                "Redis coordinator guard probe",
                self.redis.account_coordinator_guard_state_for_epoch(epoch),
            )
            .await?
            {
                AccountCoordinatorGuardState::Ready { .. }
                | AccountCoordinatorGuardState::Recovering { .. } => {
                    let epoch = epoch.to_string();
                    tx.commit().await?;
                    return Ok((current_run_id, epoch));
                }
                AccountCoordinatorGuardState::EpochMismatch { .. } => {}
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
        .bind(ACCOUNT_COORDINATOR_RUNTIME_CONFIG_ID)
        .bind(coordination_record)
        .execute(&mut *tx)
        .await?;
        account_coordinator_redis_call(
            "Redis coordinator guard install",
            self.redis.install_account_coordinator_guard(
                &coordination_epoch,
                if recovery_required {
                    self.coordinator_recovery_grace
                } else {
                    Duration::ZERO
                },
            ),
        )
        .await?;
        let confirmed_run_id =
            account_coordinator_redis_call("Redis INFO run_id", self.redis.account_redis_run_id())
                .await?;
        if confirmed_run_id != current_run_id {
            anyhow::bail!("Redis restarted while account coordinator epoch was installed");
        }
        tx.commit().await?;
        Ok((current_run_id, coordination_epoch))
    }

    fn forget_account_dispatch_counter(&self, account_id: u64) {
        self.dispatch_counters
            .per_account
            .lock()
            .remove(&account_id);
    }

    pub fn account_dispatched_total(&self, account_id: u64) -> u64 {
        self.dispatch_counters
            .per_account
            .lock()
            .get(&account_id)
            .map(|count| count.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    pub async fn dispatch_status(&self) -> DispatchStatus {
        let queued = timeout(
            Duration::from_millis(500),
            self.redis.account_dispatch_queue_size(),
        )
        .await
        .ok()
        .and_then(Result::ok);
        let mut status = self
            .dispatch_counters
            .snapshot(queued, self.release_dispatcher.pending_len());
        status.coordinator_admission_saturated = self
            .coordinator_breaker
            .stats
            .saturated
            .load(Ordering::Relaxed);
        status
    }

    pub async fn status_summary(&self, accounts: &[AccountStatus]) -> AccountsSummary {
        let dispatch = self.dispatch_status().await;
        AccountsSummary {
            total: accounts.len(),
            enabled: accounts
                .iter()
                .filter(|status| status.account.enabled)
                .count(),
            dispatchable: accounts.iter().filter(|status| status.dispatchable).count(),
            in_cooldown: accounts
                .iter()
                .filter(|status| status.cooldown_remaining_secs > 0)
                .count(),
            auto_disabled: accounts
                .iter()
                .filter(|status| status.account.auto_disabled)
                .count(),
            in_flight_total: accounts
                .iter()
                .map(|status| u64::from(status.in_flight))
                .sum(),
            global_in_flight: dispatch.global_in_flight,
            queued_requests: dispatch.queued_requests,
        }
    }

    pub async fn status(&self, config: &AccountsConfig) -> anyhow::Result<Vec<AccountStatus>> {
        let pools = self.postgres.list_accounts(true).await?;
        let pool_ids = pools.iter().map(|pool| pool.id).collect::<Vec<_>>();
        let runtimes = self.load_pool_runtime_snapshots(&pool_ids, &[]).await?;
        let mut statuses = Vec::with_capacity(pools.len());
        for (pool, runtime) in pools.into_iter().zip(runtimes) {
            let Ok(runtime) = runtime else {
                statuses.push(AccountStatus {
                    dispatched_total: self.account_dispatched_total(pool.id),
                    account: pool,
                    in_flight: 0,
                    cooldown_remaining_secs: 0,
                    cooldown_reason: None,
                    transient_failure_streak: 0,
                    transient_failure_ttl_secs: 0,
                    dispatchable: false,
                    skipped_reason: Some("coordinator_state_invalid".to_string()),
                    quality: None,
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
            // 主开关关闭时不展示质量数据：此时它既不参与调度，
            // 展示出来只会让运维误以为调度正在依据它决策。
            let quality = config
                .quality_aware_scheduling_enabled
                .then_some(runtime.quality.as_ref())
                .flatten()
                .map(|quality| AccountQualityView::from_state(quality, config, account_now_ms()));
            statuses.push(AccountStatus {
                dispatchable: skipped_reason.is_none(),
                dispatched_total: self.account_dispatched_total(pool.id),
                account: pool,
                in_flight,
                cooldown_remaining_secs,
                cooldown_reason,
                transient_failure_streak,
                transient_failure_ttl_secs,
                skipped_reason,
                quality,
            });
        }
        Ok(statuses)
    }

    pub async fn filter_route_models(
        &self,
        route: &crate::model::routes::RouteConfig,
        endpoint: &str,
        models: Vec<crate::anthropic::types::Model>,
    ) -> Vec<crate::anthropic::types::Model> {
        let pools = self.load_static_pool_snapshot().await;
        filter_models_for_route(route, endpoint, models, &pools)
    }

    #[cfg(test)]
    pub async fn has_available_pool(&self, config: &AccountsConfig) -> bool {
        self.pool_availability_snapshot(&HashSet::new(), config)
            .await
            .available_pools
            > 0
    }

    #[cfg(test)]
    pub async fn has_eligible_pool(&self, config: &AccountsConfig) -> bool {
        self.has_eligible_pool_matching(config, None, None).await
    }

    #[cfg(test)]
    pub async fn has_eligible_pool_for_model(&self, config: &AccountsConfig, model: &str) -> bool {
        let model_candidates = normalize_account_support_candidates([model]);
        self.has_eligible_pool_matching(config, Some(&model_candidates), None)
            .await
    }

    pub async fn has_eligible_pool_for_route_and_model(
        &self,
        config: &AccountsConfig,
        endpoint: &str,
        model: &str,
    ) -> bool {
        let model_candidates = normalize_account_support_candidates([model]);
        self.has_eligible_pool_matching(config, Some(&model_candidates), Some(endpoint))
            .await
    }

    pub fn has_cached_eligible_pool_for_route_and_model(
        &self,
        config: &AccountsConfig,
        endpoint: &str,
        model: &str,
    ) -> bool {
        let model_candidates = normalize_account_support_candidates([model]);
        self.has_cached_eligible_pool_matching(config, Some(&model_candidates), Some(endpoint))
    }

    fn has_cached_eligible_pool_matching(
        &self,
        config: &AccountsConfig,
        model_candidates: Option<&[String]>,
        endpoint: Option<&str>,
    ) -> bool {
        if !config.enabled {
            return false;
        }
        let Some(pools) = self.cached_static_pool_snapshot_for_local_route() else {
            return false;
        };
        let now = Utc::now();
        pools.iter().any(|pool| {
            pool.enabled
                && !pool.is_auto_disabled_at(now)
                && route_account_allowed(config, pool.id, endpoint, model_candidates)
                && account_eligibility_route_allowed(pool, endpoint)
                && account_eligibility_matches_supported_models(pool, model_candidates)
        })
    }

    #[cfg(test)]
    pub async fn has_immediately_available_pool_for_model(
        &self,
        config: &AccountsConfig,
        model: &str,
        max_wait: Duration,
    ) -> bool {
        let model_candidates = normalize_account_support_candidates([model]);
        self.has_immediately_available_pool_matching(
            config,
            Some(&model_candidates),
            None,
            max_wait,
        )
        .await
    }

    pub async fn has_immediately_available_pool_for_route_and_model(
        &self,
        config: &AccountsConfig,
        endpoint: &str,
        model: &str,
        max_wait: Duration,
    ) -> bool {
        let model_candidates = normalize_account_support_candidates([model]);
        self.has_immediately_available_pool_matching(
            config,
            Some(&model_candidates),
            Some(endpoint),
            max_wait,
        )
        .await
    }

    #[cfg(test)]
    pub fn has_cached_immediately_available_pool_for_model(
        &self,
        config: &AccountsConfig,
        model: &str,
    ) -> bool {
        let model_candidates = normalize_account_support_candidates([model]);
        self.has_cached_immediately_available_pool_matching(config, Some(&model_candidates), None)
    }

    pub fn has_cached_immediately_available_pool_for_route_and_model(
        &self,
        config: &AccountsConfig,
        endpoint: &str,
        model: &str,
    ) -> bool {
        let model_candidates = normalize_account_support_candidates([model]);
        self.has_cached_immediately_available_pool_matching(
            config,
            Some(&model_candidates),
            Some(endpoint),
        )
    }

    async fn has_eligible_pool_matching(
        &self,
        config: &AccountsConfig,
        model_candidates: Option<&[String]>,
        endpoint: Option<&str>,
    ) -> bool {
        if !config.enabled {
            return false;
        }
        let pools = self.load_static_pool_snapshot().await;
        let now = Utc::now();
        pools.iter().any(|pool| {
            pool.enabled
                && !pool.is_auto_disabled_at(now)
                && route_account_allowed(config, pool.id, endpoint, model_candidates)
                && account_eligibility_route_allowed(pool, endpoint)
                && account_eligibility_matches_supported_models(pool, model_candidates)
        })
    }

    fn has_cached_immediately_available_pool_matching(
        &self,
        config: &AccountsConfig,
        model_candidates: Option<&[String]>,
        endpoint: Option<&str>,
    ) -> bool {
        if !config.enabled {
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
                    && route_account_allowed(config, pool.id, endpoint, model_candidates)
                    && account_route_allowed(pool, endpoint)
                    && account_matches_supported_models_normalized(pool, model_candidates)
            })
            .cloned()
            .collect::<Vec<_>>();
        if pools.is_empty() {
            return false;
        }

        let requested_model_cooldowns: &[String] =
            if config.model_unavailable_cooldown_mode == ModelUnavailableCooldownMode::Model {
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
        config: &AccountsConfig,
        model_candidates: Option<&[String]>,
        endpoint: Option<&str>,
        max_wait: Duration,
    ) -> bool {
        if !config.enabled {
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
                        "账号当前可用性检查未能快速加载权威快照"
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
                    "账号当前可用性检查超过本地调度保护预算，保持本地原始排队语义"
                );
                false
            }
        }
    }

    pub async fn forward_with_failover(
        &self,
        config: AccountsConfig,
        route: AccountRouteRequest,
    ) -> Response {
        let request_id = route.request_id.clone();
        match self.forward_with_failover_result(config, route).await {
            AccountForwardOutcome::Response(response) => response,
            AccountForwardOutcome::FinalError(err) => err.into_response(&request_id),
        }
    }

    pub async fn forward_with_failover_result(
        &self,
        config: AccountsConfig,
        route: AccountRouteRequest,
    ) -> AccountForwardOutcome {
        // 装箱：换池循环的 future 很大，直接内联会撑大所有调用方的栈帧。
        if route.early_commit.is_some() || !route.is_stream() || !config.enabled {
            return Box::pin(self.forward_with_failover_inner(config, route, None)).await;
        }
        // 只加载一次权威快照并交给换池循环复用，不因判断模式多等一次。
        let snapshot = self.load_authoritative_pool_snapshot().await;
        let early_commit = snapshot
            .as_ref()
            .is_ok_and(|pools| route_prefers_early_commit(pools, &route, &config));
        if early_commit {
            return Box::pin(self.forward_with_early_commit(config, route, snapshot)).await;
        }
        Box::pin(self.forward_with_failover_inner(config, route, Some(snapshot))).await
    }

    /// 收到首个上游 message_start 后返回 SSE，之前的失败保持 HTTP 错误语义。
    /// 后台换池循环的后续内容接到同一个 body，下游断开后停止重试。
    async fn forward_with_early_commit(
        &self,
        config: AccountsConfig,
        mut route: AccountRouteRequest,
        snapshot: Result<Arc<Vec<Account>>, AccountSelectionUnavailable>,
    ) -> AccountForwardOutcome {
        let request_id = route.request_id.clone();
        let error_id = route.error_id.clone();
        let (ready_sender, ready_receiver) = tokio::sync::oneshot::channel();
        let (downstream, receiver) = early_commit::ExternalEarlyCommitDownstream::new(
            route.started_at,
            route.latency_trace.clone(),
            ready_sender,
        );
        route.early_commit = Some(downstream.clone());
        let manager = self.clone();
        let (tail_sender, tail_receiver) = tokio::sync::oneshot::channel::<EarlyCommitTail>();
        let task_downstream = downstream.clone();
        let task_request_id = request_id.clone();
        let task_error_id = error_id.clone();
        let task = tokio::spawn(async move {
            let idle_timeout = (config.stream_idle_timeout_secs > 0)
                .then(|| Duration::from_secs(config.stream_idle_timeout_secs));
            let mut keepalive =
                tokio::time::interval(account_stream_keepalive_interval(idle_timeout));
            keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let forward = manager.forward_with_failover_inner(config, route, Some(snapshot));
            tokio::pin!(forward);
            let outcome = loop {
                tokio::select! {
                    outcome = &mut forward => break outcome,
                    _ = keepalive.tick() => {
                        if task_downstream.message_start_sent() {
                            let _ = task_downstream.send_ping().await;
                        }
                    }
                }
            };
            let tail = match outcome {
                AccountForwardOutcome::Response(response) => {
                    if !task_downstream.response_committed() {
                        task_downstream.mark_response_committed();
                    }
                    EarlyCommitTail::Stream(response.into_body().into_data_stream().boxed())
                }
                AccountForwardOutcome::FinalError(err) => {
                    if task_downstream.response_committed() {
                        tracing::warn!(
                            request_id = %task_request_id,
                            error_id = %task_error_id,
                            route_error_type = %err.route_error_type,
                            attempts = err.attempts.len(),
                            "account early-commit request failed after committing the stream; ending with an SSE error event"
                        );
                        task_downstream
                            .send_terminal_error(&task_error_id, &task_request_id)
                            .await;
                    } else {
                        let _ = task_downstream.fail_before_response_commit(err.clone());
                    }
                    EarlyCommitTail::Done
                }
            };
            let _ = tail_sender.send(tail);
        });
        drop(downstream);
        let mut task_guard = EarlyCommitTaskGuard::new(task);
        match ready_receiver.await {
            Ok(EarlyCommitResponseReady::Committed) => {
                task_guard.detach();
                AccountForwardOutcome::Response(early_commit_response(
                    &request_id,
                    receiver,
                    tail_receiver,
                ))
            }
            Ok(EarlyCommitResponseReady::Failed(error)) => AccountForwardOutcome::FinalError(error),
            Err(_) => AccountForwardOutcome::FinalError(AccountFinalError {
                status: StatusCode::BAD_GATEWAY,
                response_error_type: "bad_gateway".to_string(),
                route_error_type: "account_early_commit_start_failed".to_string(),
                message: "account early-commit response failed before commitment".to_string(),
                error_id,
                retryable: false,
                attempts: Vec::new(),
                pool_id: None,
                pool_name: None,
            }),
        }
    }

    async fn forward_with_failover_inner(
        &self,
        config: AccountsConfig,
        route: AccountRouteRequest,
        preloaded_snapshot: Option<Result<Arc<Vec<Account>>, AccountSelectionUnavailable>>,
    ) -> AccountForwardOutcome {
        let mut route = route;
        if !config.enabled {
            self.record_external_failure(
                &route,
                None,
                Vec::new(),
                "account_disabled",
                "request route is disabled",
                synthetic_external_error_diagnostics(
                    &route,
                    StatusCode::SERVICE_UNAVAILABLE,
                    "external_dispatch",
                ),
            );
            return AccountForwardOutcome::FinalError(AccountFinalError {
                status: StatusCode::SERVICE_UNAVAILABLE,
                response_error_type: "service_unavailable".to_string(),
                route_error_type: "account_unavailable".to_string(),
                message: "request route is disabled".to_string(),
                error_id: route.error_id.clone(),
                retryable: false,
                attempts: Vec::new(),
                pool_id: None,
                pool_name: None,
            });
        }

        let payload_guard_retry_enabled = route.payload_guard_retry_config.is_some();
        let mut max_pool_attempts = if config.retry_max_attempts == 0 {
            None
        } else {
            Some(
                (config.retry_max_attempts as usize)
                    .saturating_add(usize::from(payload_guard_retry_enabled)),
            )
        };

        let mut excluded = HashSet::new();
        let mut attempts = Vec::new();
        let mut last_error: Option<(Account, AccountError)> = None;
        let mut queue_guard: Option<AccountQueueGuard> = None;
        let mut wait_started_at: Option<Instant> = None;
        let mut send_attempt_index = 0usize;
        let mut pool_attempt_index = 0usize;
        let mut same_pool_retry_counts: HashMap<u64, u32> = HashMap::new();
        let mut attempt_rejection = None;
        let mut preselected_pool: Option<Account> = None;
        let dispatch_deadline = external_dispatch_deadline(&route, &config);
        let authoritative_pools = match preloaded_snapshot {
            Some(snapshot) => snapshot,
            None => self.load_authoritative_pool_snapshot().await,
        };
        let authoritative_pools = match authoritative_pools {
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
                return AccountForwardOutcome::FinalError(external_capacity_final_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    error_type,
                    message,
                    &route.error_id,
                ));
            }
        };

        let models = normalize_account_support_candidates(
            route.model_candidates_for_support().into_iter().flatten(),
        );
        let enabled = authoritative_pools
            .iter()
            .filter(|pool| pool.enabled)
            .collect::<Vec<_>>();
        if !enabled.is_empty()
            && !enabled.iter().any(|pool| {
                route_account_allowed(&config, pool.id, Some(&route.endpoint), Some(&models))
                    && account_route_allowed(pool, Some(&route.endpoint))
                    && account_matches_supported_models_normalized(pool, Some(&models))
            })
        {
            self.record_external_failure(
                &route,
                None,
                Vec::new(),
                "model_not_found",
                "model rejected by route or account rules",
                synthetic_external_error_diagnostics(
                    &route,
                    StatusCode::NOT_FOUND,
                    "external_dispatch",
                ),
            );
            return AccountForwardOutcome::FinalError(AccountFinalError {
                status: StatusCode::NOT_FOUND,
                response_error_type: "not_found_error".into(),
                route_error_type: "model_not_found".into(),
                message: format!("model: {}", route.requested_model()),
                error_id: route.error_id.clone(),
                retryable: false,
                attempts: Vec::new(),
                pool_id: None,
                pool_name: None,
            });
        }

        // The shared dispatch deadline bounds queueing and capacity waits (it is ~15 seconds from
        // request start). A pool that already failed *before any downstream output* has used that
        // time on the upstream, not in a queue; a bounded number of such pre-output failovers may
        // continue past the deadline so the request still lands on a working pool.
        let mut pre_output_failovers_left = ACCOUNT_PRE_OUTPUT_FAILOVERS_PAST_DEADLINE;
        let mut last_failure_was_pre_output = false;
        loop {
            if route
                .early_commit
                .as_ref()
                .is_some_and(|downstream| downstream.downstream_closed())
            {
                self.record_external_client_dropped(&route, None, attempts.clone());
                let gone = external_early_commit_downstream_gone_error(None);
                return AccountForwardOutcome::FinalError(external_final_error_from_error(
                    None,
                    attempts,
                    &gone.err,
                    &route.error_id,
                ));
            }
            let past_dispatch_deadline =
                dispatch_deadline.is_some_and(|deadline| Instant::now() >= deadline);
            let pre_output_failover_allowed = past_dispatch_deadline
                && last_failure_was_pre_output
                && pre_output_failovers_left > 0
                && !route
                    .inference_attempt_budget
                    .snapshot()
                    .downstream_committed;
            if past_dispatch_deadline && !pre_output_failover_allowed {
                // 已经有具体池的失败时，用它作为最终错误；合成的 deadline 错误会丢掉
                // attempts 和上游错误详情（与 synthetic capacity error 的处理一致）。
                if last_error.is_some() {
                    break;
                }
                return self.external_dispatch_deadline_outcome(&route, attempts, &config);
            }
            if pre_output_failover_allowed {
                pre_output_failovers_left -= 1;
                tracing::info!(
                    request_id = %route.request_id,
                    remaining = pre_output_failovers_left,
                    "account failed before output; failing over past the dispatch deadline"
                );
            }
            last_failure_was_pre_output = false;
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
            let degraded_selection = (selection.selected_pool.is_none()
                && selection.availability.coordinator_unavailable)
                .then(|| {
                    self.select_degraded_fallback_local_pool_from_snapshot(
                        &authoritative_pools,
                        &excluded,
                        &route,
                        &config,
                    )
                })
                .flatten();
            if let Some(degraded_selection) = degraded_selection {
                tracing::warn!(
                    request_id = %route.request_id,

                    "账号 Redis coordinator 不可用，使用进程内有界 lease"
                );
                selection = degraded_selection;
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
                            if last_error.is_some()
                                && should_defer_synthetic_capacity_error_to_last_pool_error(&err)
                            {
                                tracing::warn!(
                                    request_id = %route.request_id,
                                    error_id = %route.error_id,
                                    route_error_type = %err.route_error_type,
                                    "external dispatch produced a synthetic capacity error after concrete pool attempts; using the last concrete pool error for fallback"
                                );
                                break;
                            }
                            return AccountForwardOutcome::FinalError(err);
                        }
                    }
                }
                if snapshot.available_pools > 0 {
                    tracing::error!(
                        request_id = %route.request_id,
                        error_id = %route.error_id,
                        available_pools = snapshot.available_pools,
                        eligible_pools = snapshot.eligible_pools,
                        "account selector returned no pool despite available candidates"
                    );
                    let error_type = "account_selection_invariant";
                    let message = "Account selection was inconsistent with availability";
                    let context = snapshot.capacity_context();
                    self.record_external_failure(
                        &route,
                        None,
                        attempts.clone(),
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
                    return AccountForwardOutcome::FinalError(AccountFinalError {
                        status: StatusCode::SERVICE_UNAVAILABLE,
                        response_error_type: "service_unavailable".to_string(),
                        route_error_type: error_type.to_string(),
                        message: message.to_string(),
                        error_id: route.error_id.clone(),
                        retryable: false,
                        attempts,
                        pool_id: None,
                        pool_name: None,
                    });
                }
                break;
            };
            let pool_id = pool.id;
            let acquire_result = if selection.degraded_fallback_local_lease {
                self.acquire_degraded_fallback_local_pool(&pool, &config)
            } else {
                match self.acquire_pool_for_route(&pool, &config, &route).await {
                    PoolAcquireResult::Acquired(lease) => PoolAcquireResult::Acquired(lease),
                    PoolAcquireResult::Unavailable(unavailable)
                        if unavailable.reason == PoolCapacityWaitReason::CoordinatorUnavailable =>
                    {
                        tracing::warn!(
                            request_id = %route.request_id,
                            pool_id,
                            detail = unavailable.detail,
                            "账号 Redis coordinator 熔断已打开，改用进程内有界 lease"
                        );
                        self.acquire_degraded_fallback_local_pool(&pool, &config)
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
                        "账号选中后并发 lease 未占用，本次请求尝试重选或按配置等待"
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
                            if last_error.is_some()
                                && should_defer_synthetic_capacity_error_to_last_pool_error(&err)
                            {
                                tracing::warn!(
                                    request_id = %route.request_id,
                                    error_id = %route.error_id,
                                    route_error_type = %err.route_error_type,
                                    pool_id,
                                    "account acquire produced a synthetic capacity error after concrete pool attempts; using the last concrete pool error for fallback"
                                );
                                break;
                            }
                            return AccountForwardOutcome::FinalError(err);
                        }
                    }
                }
            };
            capacity_waiter.finish_acquired();
            drop(queue_guard.take());
            if dispatch_deadline.is_some_and(|deadline| Instant::now() >= deadline)
                && !pre_output_failover_allowed
            {
                drop(lease);
                if last_error.is_some() {
                    break;
                }
                return self.external_dispatch_deadline_outcome(&route, attempts, &config);
            }
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
                                "账号在请求准备后、HTTP send 前发生配置变更，拒绝旧快照并重选"
                            );
                            continue;
                        }
                        PoolDispatchFenceResult::CoordinatorUnavailable(unavailable) => {
                            if self.local_mutation_dispatch_trusted(&pool) {
                                tracing::warn!(
                                    request_id = %route.request_id,
                                    pool_id = pool.id,
                                    pool_revision = pool.revision,

                                    unavailable_kind = unavailable.kind.as_str(),
                                    "账号 coordinator 与 dispatch fence 暂不可用，使用本进程刚提交的账号 revision 有界兜底"
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
                                        if last_error.is_some()
                                            && should_defer_synthetic_capacity_error_to_last_pool_error(&err)
                                        {
                                            tracing::warn!(
                                                request_id = %route.request_id,
                                                error_id = %route.error_id,
                                                route_error_type = %err.route_error_type,
                                                pool_id = pool.id,
                                                "external dispatch fence produced a synthetic capacity error after concrete pool attempts; using the last concrete pool error for fallback"
                                            );
                                            break;
                                        }
                                        return AccountForwardOutcome::FinalError(err);
                                    }
                                }
                            }
                        }
                    }
                    self.dispatch_counters.record(pool.id);
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
                    attempts.push(AccountAttempt {
                        attempt: current_attempt,
                        pool_id,
                        pool_name: pool.name.clone(),
                        outbound_model: forwarded.outbound_model.clone(),
                        status: Some(forwarded.response.status().as_u16()),
                        action: "success".to_string(),
                        duration_ms: started.elapsed().as_millis() as u64,
                        error_type: None,
                        error_message: None,
                        raw_upstream_error: None,
                    });
                    if route.is_stream() {
                        return AccountForwardOutcome::Response(
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
                        &config,
                        &route,
                        &pool,
                        attempts.clone(),
                        forwarded.billing,
                        forwarded.downstream_stop_reason,
                    );
                    return AccountForwardOutcome::Response(forwarded.response);
                }
                Err(forward_err) => {
                    if let Some(rejection) = forward_err.attempt_rejection {
                        return self
                            .external_attempt_rejection_outcome(&route, attempts, rejection);
                    }
                    if forward_err.err.protocol_error
                        == Some(EXTERNAL_EARLY_COMMIT_CLIENT_GONE_REASON)
                    {
                        // 提前提交模式下客户端已断开：不换池、不计入池质量或冷却，按客户端断开记录。
                        attempts.push(AccountAttempt {
                            attempt: send_attempt_index.saturating_add(1) as u32,
                            pool_id,
                            pool_name: pool.name.clone(),
                            outbound_model: forward_err.outbound_model.clone(),
                            status: None,
                            action: "client_dropped".to_string(),
                            duration_ms: started.elapsed().as_millis() as u64,
                            error_type: Some("client_dropped".to_string()),
                            error_message: Some(forward_err.err.message.clone()),
                            raw_upstream_error: None,
                        });
                        self.record_external_client_dropped(&route, Some(&pool), attempts.clone());
                        return AccountForwardOutcome::FinalError(external_final_error_from_error(
                            Some(&pool),
                            attempts,
                            &forward_err.err,
                            &route.error_id,
                        ));
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
                    attempts.push(AccountAttempt {
                        attempt: current_attempt,
                        pool_id,
                        pool_name: pool.name.clone(),
                        outbound_model: outbound_model.clone(),
                        status: err.status.map(|status| status.as_u16()),
                        action: action.to_string(),
                        duration_ms: started.elapsed().as_millis() as u64,
                        error_type: Some(error_type_for_external_error(&err).to_string()),
                        error_message: Some(err.message.clone()),
                        raw_upstream_error: err.raw_upstream_error.clone(),
                    });
                    if let Some(retry_route) = should_retry_external_payload_guard(&route, &err)
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
                    // 被动质量采样：只有"上游质量相关"的错误才计入健康度。
                    // 下游请求自身非法（请求体/schema/超长）与客户端断开不污染账号。
                    if external_error_affects_pool_quality(&err) {
                        self.record_pool_quality_sample(&config, pool_id, false, None, None);
                    }
                    if let Some((_, reason)) = &cooldown_hint {
                        if should_record_account_soft_failure(reason) {
                            // Keep the short-lived failure streak as a ranking
                            // signal, but never turn an ordinary transient
                            // failure into a pool-level cooldown.
                            let _ = self.record_account_soft_failure(pool_id, reason).await;
                        }
                    }
                    let same_pool_retry_count = same_pool_retry_counts.entry(pool_id).or_insert(0);
                    let same_pool_retry_limit =
                        retry_pipeline::same_pool_retry_limit(&config, &err);
                    if *same_pool_retry_count < same_pool_retry_limit
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
                            same_pool_retry_max = same_pool_retry_limit,
                            "account retryable status will be retried on the same pool before cross-pool failover"
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
                            match config.model_unavailable_cooldown_mode {
                                ModelUnavailableCooldownMode::Disabled => {}
                                ModelUnavailableCooldownMode::Model => {
                                    let mut models = route.model_cooldown_candidates();
                                    if let Some(model) = outbound_model
                                        .as_deref()
                                        .and_then(normalize_account_model_cooldown_key_model)
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
                                ModelUnavailableCooldownMode::Pool => {
                                    self.mark_pool_cooldown(pool_id, *duration, reason.clone())
                                        .await;
                                }
                            }
                        } else if should_mark_account_hard_cooldown(reason) {
                            self.mark_pool_cooldown(pool_id, *duration, reason.clone())
                                .await;
                        }
                    }
                    if let Some(reason) = &err.auto_disable_reason {
                        self.auto_disable_pool_if_configured(&pool, &config, reason, &err.message)
                            .await;
                    }
                    if cross_pool_retry {
                        last_failure_was_pre_output =
                            external_error_is_pre_output_stream_failure(&err);
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
                    return AccountForwardOutcome::FinalError(external_final_error_from_error(
                        Some(&pool),
                        attempts,
                        &err,
                        &route.error_id,
                    ));
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
            return AccountForwardOutcome::FinalError(external_final_error_from_error(
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
            "account_unavailable",
            "No available external fallback pools",
            synthetic_external_error_diagnostics(
                &route,
                StatusCode::SERVICE_UNAVAILABLE,
                "external_dispatch",
            ),
        );
        AccountForwardOutcome::FinalError(AccountFinalError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            response_error_type: "service_unavailable".to_string(),
            route_error_type: "account_unavailable".to_string(),
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
        route: &AccountRouteRequest,
        attempts: Vec<AccountAttempt>,
        rejection: InferenceAttemptRejection,
    ) -> AccountForwardOutcome {
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
        AccountForwardOutcome::FinalError(external_attempt_rejection_final_error(route, attempts))
    }

    fn external_dispatch_deadline_outcome(
        &self,
        route: &AccountRouteRequest,
        attempts: Vec<AccountAttempt>,
        config: &AccountsConfig,
    ) -> AccountForwardOutcome {
        let message = "Request dispatch deadline exceeded";
        self.record_external_failure(
            route,
            None,
            attempts.clone(),
            "account_deadline_exceeded",
            message,
            synthetic_external_capacity_error_diagnostics(
                route,
                StatusCode::SERVICE_UNAVAILABLE,
                "external_dispatch",
                config,
                None,
            ),
        );
        AccountForwardOutcome::FinalError(external_capacity_final_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "account_deadline_exceeded",
            message,
            &route.error_id,
        ))
    }

    #[allow(clippy::result_large_err)]
    fn prepare_forward_once(
        &self,
        pool: &Account,
        route: &AccountRouteRequest,
        config: &AccountsConfig,
    ) -> Result<PreparedExternalForwardRequest, ExternalForwardError> {
        let effective = pool.overrides.merge(config);
        let config = &effective;
        let mut prepared = account_prepare_request(route, pool)?;
        let url = account_url(pool, &route.endpoint, config)?;
        let mut headers = forward_headers(&route.headers, pool, route.is_stream())?;
        if pool.header_profile.uses_anthropic_header_policy() {
            prepared.body = sanitize_anthropic_body_for_beta_header(&prepared.body, &headers);
            sync_claude_code_session_header(&mut headers, &prepared.body);
        }
        if !headers.contains_key(header::CONTENT_TYPE) {
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
        }
        let outbound_model = prepared.outbound_model.clone();
        let outbound_body = prepared.body.clone();
        let known_tool_names = external_route_known_tool_names(route);
        let client = self.client_for_pool(pool, config).map_err(|err| {
            ExternalForwardError::new(
                AccountError {
                    status: None,
                    message: err.to_string(),
                    retryable: false,
                    auto_disable_reason: None,
                    cooldown: None,
                    protocol_error: None,
                    raw_upstream_error: None,
                },
                None,
            )
        })?;
        let mut request = client.post(url).headers(headers).body(prepared.body);
        if let Some(timeout) = external_request_send_timeout(route, config) {
            request = request.timeout(timeout);
        }
        let response_body_timeout_secs = external_response_body_timeout_secs(route, config);
        Ok(PreparedExternalForwardRequest {
            request,
            outbound_model,
            outbound_body,
            known_tool_names,
            response_body_timeout_secs,
        })
    }

    /// 账号转发使用的 HTTP 客户端：按账号的 wire/TLS profile 与代理（账号级优先，否则全局
    /// `transport_proxy_*`）选择或构建。管理端的模型发现与账号测试也走这里，保证和真实转发一致。
    pub(crate) fn client_for_pool(
        &self,
        pool: &Account,
        config: &AccountsConfig,
    ) -> anyhow::Result<reqwest::Client> {
        let proxy_url = pool
            .proxy_url
            .as_deref()
            .filter(|url| !url.trim().is_empty())
            .or(config.transport_proxy_url.as_deref());
        if let Some(proxy_url) = proxy_url {
            let key = format!(
                "{}:{}:{proxy_url}:{:?}:{:?}",
                pool.wire_profile.as_str(),
                pool.tls_profile.as_str(),
                config.transport_proxy_username,
                config.transport_proxy_password
            );
            let mut cache = self.proxy_clients.lock();
            if let Some(client) = cache.get(&key) {
                return Ok(client.clone());
            }
            let proxy_config = crate::http_client::ProxyConfig::new(proxy_url);
            let mut proxy = reqwest::Proxy::all(&proxy_config.url)?;
            if pool
                .proxy_url
                .as_deref()
                .filter(|url| !url.trim().is_empty())
                .is_none()
            {
                if let (Some(username), Some(password)) = (
                    &config.transport_proxy_username,
                    &config.transport_proxy_password,
                ) {
                    proxy = proxy.basic_auth(username, password);
                }
            }
            let mut builder = reqwest::Client::builder().proxy(proxy);
            if pool.wire_profile == AccountWireProfile::Http1TitleCase {
                builder = builder.http1_only().http1_title_case_headers();
            }
            if pool.tls_profile == AccountTlsProfile::NativeTls {
                builder = apply_account_native_tls_profile(builder);
            }
            let client = builder.build()?;
            if cache.len() >= 256 {
                cache.clear();
            }
            cache.insert(key, client.clone());
            return Ok(client);
        }
        Ok(match (pool.wire_profile, pool.tls_profile) {
            (AccountWireProfile::Default, AccountTlsProfile::Default) => self.client.clone(),
            (AccountWireProfile::Http1TitleCase, AccountTlsProfile::Default) => {
                self.http1_title_case_client.clone()
            }
            (AccountWireProfile::Default, AccountTlsProfile::NativeTls) => {
                self.native_tls_client.clone()
            }
            (AccountWireProfile::Http1TitleCase, AccountTlsProfile::NativeTls) => {
                self.native_tls_http1_title_case_client.clone()
            }
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
        pool: &Account,
        route: &AccountRouteRequest,
        lease: AccountLease,
        config: &AccountsConfig,
        prepared: PreparedExternalForwardRequest,
    ) -> Result<ExternalForwardResponse, ExternalForwardError> {
        let effective = pool.overrides.merge(config);
        let config = &effective;
        let PreparedExternalForwardRequest {
            request,
            outbound_model,
            outbound_body,
            known_tool_names,
            response_body_timeout_secs,
        } = prepared;
        if let Err(rejection) = route
            .inference_attempt_budget
            .reserve(InferenceAttemptKind::Account, 0)
        {
            tracing::warn!(
                request_id = %route.request_id,
                error_id = %route.error_id,
                pool_id = pool.id,
                rejection = ?rejection,
                "shared inference attempt policy rejected external upstream send"
            );
            return Err(ExternalForwardError::dispatch_rejected(
                AccountError {
                    status: Some(StatusCode::SERVICE_UNAVAILABLE),
                    message: "inference request dispatch was not available".to_string(),
                    retryable: false,
                    auto_disable_reason: None,
                    cooldown: None,
                    protocol_error: None,
                    raw_upstream_error: None,
                },
                outbound_model,
                rejection,
            ));
        }
        if let Some(downstream) = route.early_commit.as_ref() {
            downstream.reset_attempt_usage();
        }
        if crate::diagnostics::capture::is_active() {
            if let Some(built) = request.try_clone().and_then(|builder| builder.build().ok()) {
                crate::diagnostics::capture::record(
                    "external_upstream_request",
                    Some(&route.request_id),
                    || {
                        serde_json::json!({
                            "poolId": pool.id,
                            "endpoint": route.endpoint,
                            "outboundModel": outbound_model,
                            "method": built.method().as_str(),
                            "url": built.url().as_str(),
                            "headers": crate::diagnostics::capture::http_headers_json(built.headers()),
                            "body": crate::diagnostics::capture::body_json(&outbound_body),
                        })
                    },
                );
            }
        }
        let upstream_header_started_at = Instant::now();
        let response = tokio::select! {
            response = request.send() => {
                route.inference_attempt_budget.record_upstream_header_wait(
                    upstream_header_started_at.elapsed(),
                );
                response.map_err(|err| {
                    tracing::warn!(
                        request_id = %route.request_id,
                        error_id = %route.error_id,
                        pool_id = pool.id,
                        error_class = %sanitized_external_network_error("request send failed", &err),
                        "account request send failed"
                    );
                    ExternalForwardError::new(
                        AccountError {
                            status: None,
                            message: sanitized_external_network_error("request send failed", &err),
                            retryable: true,
                            auto_disable_reason: None,
                            cooldown: Some((
                                Duration::from_secs(
                                    config.network_error_cooldown_secs.max(1),
                                ),
                                "network_error".to_string(),
                            )),
                            protocol_error: None,
                            raw_upstream_error: None,
                        },
                        outbound_model.clone(),
                    )
                })?
            },
            _ = lease.wait_until_lost() => {
                return Err(account_lease_lost_forward_error(outbound_model.clone()));
            }
            _ = async {
                match route.early_commit.as_ref() {
                    Some(downstream) => downstream.wait_downstream_closed().await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                return Err(external_early_commit_downstream_gone_error(outbound_model.clone()));
            }
        };

        let status = response.status();
        crate::diagnostics::capture::record(
            "external_upstream_response",
            Some(&route.request_id),
            || {
                serde_json::json!({
                    "poolId": pool.id,
                    "status": status.as_u16(),
                    "httpVersion": format!("{:?}", response.version()),
                    "headerWaitMs": upstream_header_started_at.elapsed().as_millis() as u64,
                    "headers": crate::diagnostics::capture::http_headers_json(response.headers()),
                })
            },
        );
        if !status.is_success() {
            let headers = response.headers().clone();
            let body = tokio::select! {
                body = response_bytes_with_limit_and_body_timeout(
                    response,
                    response_body_timeout_secs,
                    ACCOUNT_ERROR_RESPONSE_MAX_BYTES,
                ) => body.map_err(|err| external_response_body_read_error(
                    err,
                    status,
                    ACCOUNT_ERROR_RESPONSE_MAX_BYTES,
                    outbound_model.clone(),
                ))?,
                _ = lease.wait_until_lost() => {
                    return Err(account_lease_lost_forward_error(outbound_model.clone()));
                }
            };
            crate::diagnostics::capture::record(
                "upstream_error_body",
                Some(&route.request_id),
                || {
                    serde_json::json!({
                        "phase": "external",
                        "poolId": pool.id,
                        "status": status.as_u16(),
                        "body": crate::diagnostics::capture::body_json(&body),
                    })
                },
            );
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
            let upstream_chunk_index =
                crate::diagnostics::capture::is_active().then(|| Arc::new(AtomicU64::new(0)));
            let stream_plan = ExternalStreamProcessingPlan::for_pool(pool, config);
            let client_model: Option<Arc<str>> = route.client_model.as_deref().map(Arc::from);
            let projection_context = build_external_usage_projection_context(
                route,
                pool,
                config.usage_projection_uplift_percent,
                config.usage_projection_cost_floor_enabled,
                config.usage_projection_cost_floor_margin_percent,
                config.usage_projection_output_uplift_min_tokens,
                config.usage_projection_output_uplift_percent,
            );
            let stream_idle_timeout = (config.stream_idle_timeout_secs > 0)
                .then(|| Duration::from_secs(config.stream_idle_timeout_secs));
            let stream_usage_projection = projection_context.clone();
            let usage_capture = Arc::new(SyncMutex::new(ExternalUsageCapture {
                stream_response_mode: Some(stream_plan.response_mode),
                zero_usage_repair: zero_usage_repair::ZeroUsageRepairContext::for_attempt(
                    route,
                    pool,
                    projection_context.as_ref(),
                ),
                ..ExternalUsageCapture::default()
            }));
            let stream_usage_capture = usage_capture.clone();
            let latency_trace = route.latency_trace.clone();
            let early_commit_downstream = route.early_commit.clone();
            let route_started_at = route.started_at;
            let stream_error_mask = Arc::new(ExternalStreamErrorMask {
                request_id: route.request_id.clone(),
                error_id: route.error_id.clone(),
                pool_id: pool.id,
                pool_name: pool.name.clone(),
            });
            let transcript_state =
                Arc::new(SyncMutex::new(route.history_sanitize_enabled.then(|| {
                    ExternalAnthropicTranscriptState::new_with_error_context(
                        known_tool_names.iter().cloned(),
                        Some(route.request_id.clone()),
                        Some(route.error_id.clone()),
                    )
                })));
            let mut prefix = Vec::<u8>::new();
            let mut buffer = Vec::<u8>::new();
            let mut last_chunk_at = Instant::now();
            let stream_keepalive_interval = account_stream_keepalive_interval(stream_idle_timeout);
            let capture_request_id = upstream_chunk_index
                .as_ref()
                .map(|_| route.request_id.clone());
            let capture_pool_id = pool.id;
            if let Some(downstream) = route.early_commit.as_ref() {
                last_chunk_at = match Box::pin(early_commit::pre_read_early_commit(
                    downstream,
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
                    upstream_chunk_index.as_deref(),
                ))
                .await
                {
                    Ok(early_commit::EarlyCommitPreRead::Committed(last_chunk_at)) => last_chunk_at,
                    Ok(early_commit::EarlyCommitPreRead::DownstreamGone) => {
                        drop(lease);
                        return Err(external_early_commit_downstream_gone_error(
                            outbound_model.clone(),
                        ));
                    }
                    Err(err) => {
                        drop(lease);
                        return Err(err);
                    }
                };
            } else if effective_account_pre_output_stream_retry_enabled(pool, config) {
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
                    upstream_chunk_index.as_deref(),
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
                    Instant::now(),
                    false,
                ),
                move |(
                    mut body_stream,
                    mut prefix,
                    mut buffer,
                    lease,
                    mut last_chunk_at,
                    mut last_keepalive_at,
                    finished,
                )| {
                    let projection_context = projection_context.clone();
                    let usage_capture = usage_capture.clone();
                    let latency_trace = latency_trace.clone();
                    let stream_error_mask = stream_error_mask.clone();
                    let transcript_state = transcript_state.clone();
                    let client_model = client_model.clone();
                    let early_commit_downstream = early_commit_downstream.clone();
                    let upstream_chunk_index = upstream_chunk_index.clone();
                    let capture_request_id = capture_request_id.clone();
                    async move {
                        if finished {
                            return None;
                        }
                        if let Some(prefix_chunk) = prefix.take().filter(|chunk| !chunk.is_empty())
                        {
                            last_keepalive_at = Instant::now();
                            return Some((
                                Ok(Bytes::from(prefix_chunk)),
                                (
                                    body_stream,
                                    None,
                                    buffer,
                                    lease,
                                    last_chunk_at,
                                    last_keepalive_at,
                                    false,
                                ),
                            ));
                        }
                        loop {
                            let projected = drain_sse_events_for_downstream(
                                early_commit_downstream.as_deref(),
                                &mut buffer,
                                projection_context.as_ref(),
                                Some(&usage_capture),
                                Some(stream_error_mask.as_ref()),
                                stream_plan,
                                transcript_state.lock().as_mut(),
                                client_model.as_deref(),
                            );
                            if !projected.is_empty() {
                                last_keepalive_at = Instant::now();
                                return Some((
                                    Ok(Bytes::from(projected)),
                                    (
                                        body_stream,
                                        None,
                                        buffer,
                                        lease,
                                        last_chunk_at,
                                        last_keepalive_at,
                                        false,
                                    ),
                                ));
                            }
                            tokio::select! {
                                biased;
                                chunk = body_stream.next() => {
                                    match chunk {
                                        Some(Ok(chunk)) => {
                                            record_external_upstream_chunk(
                                                capture_request_id.as_deref(),
                                                capture_pool_id,
                                                upstream_chunk_index.as_deref(),
                                                &chunk,
                                            );
                                            latency_trace
                                                .mark_first_upstream_chunk(route_started_at);
                                            last_chunk_at = Instant::now();
                                            buffer.extend_from_slice(&chunk);
                                            let projected = drain_sse_events_for_downstream(
                                                early_commit_downstream.as_deref(),
                                                &mut buffer,
                                                projection_context.as_ref(),
                                                Some(&usage_capture),
                                                Some(stream_error_mask.as_ref()),
                                                stream_plan,
                                                transcript_state.lock().as_mut(),
                                                client_model.as_deref(),
                                            );
                                            if !projected.is_empty() {
                                                last_keepalive_at = Instant::now();
                                                return Some((
                                                    Ok(Bytes::from(projected)),
                                                    (
                                                        body_stream,
                                                        None,
                                                        buffer,
                                                        lease,
                                                        last_chunk_at,
                                                        last_keepalive_at,
                                                        false,
                                                    ),
                                                ));
                                            }
                                            if buffer.len() > ACCOUNT_MAX_SSE_EVENT_BUFFER_BYTES {
                                                let error_type = "api_error";
                                                tracing::warn!(
                                                    request_id = %stream_error_mask.request_id,
                                                    error_id = %stream_error_mask.error_id,
                                                    pool_id = stream_error_mask.pool_id,
                                                    pool_name = %stream_error_mask.pool_name,
                                                    buffered_bytes = buffer.len(),
                                                    max_buffered_bytes = ACCOUNT_MAX_SSE_EVENT_BUFFER_BYTES,
                                                    terminal_event = error_type,
                                                    "account stream event buffer exceeded limit"
                                                );
                                                drop(lease);
                                                let event = external_post_output_terminal_error_event(
                                                    stream_error_mask.as_ref(),
                                                    error_type,
                                                    Some(&usage_capture),
                                                    "external stream event exceeded buffer limit",
                                                );
                                                return Some((
                                                    Ok::<Bytes, std::io::Error>(Bytes::from(event)),
                                                    (
                                                        body_stream,
                                                        None,
                                                        Vec::new(),
                                                        None,
                                                        last_chunk_at,
                                                        last_keepalive_at,
                                                        true,
                                                    ),
                                                ));
                                            }
                                        }
                                        Some(Err(_)) => {
                                            let error_type = "overloaded_error";
                                            tracing::warn!(
                                                request_id = %stream_error_mask.request_id,
                                                error_id = %stream_error_mask.error_id,
                                                pool_id = stream_error_mask.pool_id,
                                                pool_name = %stream_error_mask.pool_name,
                                                error_class = "external_stream_read_error",
                                                terminal_event = error_type,
                                                "account stream read failed"
                                            );
                                            drop(lease);
                                            let event = external_post_output_terminal_error_event(
                                                stream_error_mask.as_ref(),
                                                error_type,
                                                Some(&usage_capture),
                                                "external stream read error",
                                            );
                                            return Some((
                                                Ok(Bytes::from(event)),
                                                (
                                                    body_stream,
                                                    None,
                                                    Vec::new(),
                                                    None,
                                                    last_chunk_at,
                                                    last_keepalive_at,
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
                                                    transcript_state.lock().as_mut(),
                                                    client_model.as_deref(),
                                                )
                                            };
                                            {
                                                let mut transcript_state = transcript_state.lock();
                                                let finished_tail = transcript_state.as_mut().map(|state| finish_external_transcript_state(state, Some(&usage_capture))).unwrap_or_default();
                                                record_external_wire_debug_sse_bytes(
                                                    &finished_tail,
                                                    Some(&usage_capture),
                                                    stream_plan,
                                                    ExternalWireDebugStreamSide::ProcessedDownstream,
                                                );
                                                tail.extend(finished_tail);
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
                                                    last_keepalive_at,
                                                    true,
                                                ),
                                            ));
                                        }
                                    }
                                }
                                _ = account_lease_lost(lease.as_ref()) => {
                                    let error_type = "overloaded_error";
                                    tracing::warn!(
                                        request_id = %stream_error_mask.request_id,
                                        error_id = %stream_error_mask.error_id,
                                        pool_id = stream_error_mask.pool_id,
                                        pool_name = %stream_error_mask.pool_name,
                                        terminal_event = error_type,
                                        "account lease coordination was lost"
                                    );
                                    drop(lease);
                                    let event = external_post_output_terminal_error_event(
                                        stream_error_mask.as_ref(),
                                        error_type,
                                        Some(&usage_capture),
                                        "account lease coordination was lost",
                                    );
                                    return Some((
                                        Ok(Bytes::from(event)),
                                        (
                                            body_stream,
                                            None,
                                            Vec::new(),
                                            None,
                                            last_chunk_at,
                                            last_keepalive_at,
                                            true,
                                        ),
                                    ));
                                }
                                _ = account_stream_idle_deadline(last_chunk_at, stream_idle_timeout) => {
                                    drop(lease);
                                    let seconds = stream_idle_timeout
                                        .map(|timeout| timeout.as_secs())
                                        .unwrap_or_default();
                                    let error_type = "overloaded_error";
                                    tracing::warn!(
                                        request_id = %stream_error_mask.request_id,
                                        error_id = %stream_error_mask.error_id,
                                        pool_id = stream_error_mask.pool_id,
                                        pool_name = %stream_error_mask.pool_name,
                                        idle_timeout_secs = seconds,
                                        terminal_event = error_type,
                                        "model endpoint stream idle timeout"
                                    );
                                    let event = external_post_output_terminal_error_event(
                                        stream_error_mask.as_ref(),
                                        error_type,
                                        Some(&usage_capture),
                                        &format!("model endpoint stream idle timeout after {seconds} seconds"),
                                    );
                                    return Some((
                                        Ok(Bytes::from(event)),
                                        (
                                            body_stream,
                                            None,
                                            Vec::new(),
                                            None,
                                            last_chunk_at,
                                            last_keepalive_at,
                                            true,
                                        ),
                                    ));
                                }
                                _ = account_stream_keepalive_deadline(
                                    last_keepalive_at,
                                    stream_keepalive_interval,
                                ) => {
                                    last_keepalive_at = Instant::now();
                                    return Some((
                                        Ok(Bytes::from_static(
                                            b"event: ping\ndata: {\"type\": \"ping\"}\n\n",
                                        )),
                                        (
                                            body_stream,
                                            None,
                                            buffer,
                                            lease,
                                            last_chunk_at,
                                            last_keepalive_at,
                                            false,
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
                    AccountError {
                        status: None,
                        message: format!("build external stream response failed: {}", err),
                        retryable: false,
                        auto_disable_reason: None,
                        cooldown: None,
                        protocol_error: None,
                        raw_upstream_error: None,
                    },
                    outbound_model.clone(),
                )
            })?;
            Ok(ExternalForwardResponse {
                response,
                outbound_model,
                outbound_body,
                billing: None,
                downstream_stop_reason: None,
                stream_usage_capture: Some(stream_usage_capture),
                stream_usage_projection,
            })
        } else {
            let bytes = tokio::select! {
                body = response_bytes_with_limit_and_body_timeout(
                    response,
                    response_body_timeout_secs,
                    ACCOUNT_NON_STREAM_RESPONSE_MAX_BYTES,
                ) => body.map_err(|err| external_response_body_read_error(
                    err,
                    status,
                    ACCOUNT_NON_STREAM_RESPONSE_MAX_BYTES,
                    outbound_model.clone(),
                ))?,
                _ = lease.wait_until_lost() => {
                    return Err(account_lease_lost_forward_error(outbound_model.clone()));
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
                config.usage_projection_uplift_percent,
                config.usage_projection_cost_floor_enabled,
                config.usage_projection_cost_floor_margin_percent,
                config.usage_projection_output_uplift_min_tokens,
                config.usage_projection_output_uplift_percent,
            );
            let upstream_body = bytes.clone();
            crate::diagnostics::capture::record(
                "external_upstream_body",
                Some(&route.request_id),
                || {
                    serde_json::json!({
                        "poolId": pool.id,
                        "status": status.as_u16(),
                        "body": crate::diagnostics::capture::body_json(&upstream_body),
                    })
                },
            );
            let projected = process_non_stream_response_usage_with_repair(
                bytes,
                Some(route),
                projection_context.as_ref(),
                known_tool_names.iter().cloned(),
                zero_usage_repair::ZeroUsageRepairContext::for_attempt(
                    route,
                    pool,
                    projection_context.as_ref(),
                ),
            );
            account_usage_debug_non_stream_record(ExternalUsageDebugNonStreamRecordContext {
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
            account_wire_debug_non_stream_record(ExternalUsageDebugNonStreamRecordContext {
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
            let downstream_stop_reason = projected.usage_capture.downstream_stop_reason.clone();
            let billing = account_billing_from_capture(route, pool, projected.usage_capture);
            let mut builder = Response::builder().status(status);
            apply_forwarded_response_headers(&mut builder, &response_headers, &route.request_id);
            let upstream_declared_sse = response_headers_look_like_sse(&response_headers);
            let downstream_body = match route.client_model.as_deref() {
                Some(model) => response_model::rewrite_body_model(projected.body, model),
                None => projected.body,
            };
            crate::diagnostics::capture::record("client_response", Some(&route.request_id), || {
                serde_json::json!({
                    "kind": "external",
                    "poolId": pool.id,
                    "status": status.as_u16(),
                    "headers": crate::diagnostics::capture::http_headers_json(&response_headers),
                    "body": crate::diagnostics::capture::body_json(&downstream_body),
                })
            });
            let mut response =
                builder
                    .body(Body::from(downstream_body.clone()))
                    .map_err(|err| {
                        ExternalForwardError::new(
                            AccountError {
                                status: None,
                                message: format!("build external response failed: {}", err),
                                retryable: false,
                                auto_disable_reason: None,
                                cooldown: None,
                                protocol_error: None,
                                raw_upstream_error: None,
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
                downstream_stop_reason,
                stream_usage_capture: None,
                stream_usage_projection: projection_context,
            })
        }
    }

    #[cfg(test)]
    async fn select_pool(
        &self,
        excluded: &HashSet<u64>,
        config: &AccountsConfig,
    ) -> Option<Account> {
        self.scan_pool_availability_uncached(excluded, config, true, None, None)
            .await
            .selected_pool
    }

    async fn select_pool_for_route_from_snapshot(
        &self,
        authoritative_pools: &Arc<Vec<Account>>,
        excluded: &HashSet<u64>,
        config: &AccountsConfig,
        route: &AccountRouteRequest,
    ) -> PoolSelectionSnapshot {
        let support_candidates = normalize_account_support_candidates(
            route.model_candidates_for_support().into_iter().flatten(),
        );
        let cooldown_candidates = route.model_cooldown_candidates();
        self.scan_pool_availability_from_snapshot(
            authoritative_pools,
            excluded,
            config,
            true,
            Some(&support_candidates),
            Some(&cooldown_candidates),
            Some(&route.endpoint),
        )
        .await
    }

    fn select_degraded_fallback_local_pool_from_snapshot(
        &self,
        authoritative_pools: &Arc<Vec<Account>>,
        excluded: &HashSet<u64>,
        route: &AccountRouteRequest,
        config: &AccountsConfig,
    ) -> Option<PoolSelectionSnapshot> {
        let support_candidates = normalize_account_support_candidates(
            route.model_candidates_for_support().into_iter().flatten(),
        );
        let candidates = authoritative_pools
            .iter()
            .filter(|pool| {
                !excluded.contains(&pool.id)
                    && pool.enabled
                    && !pool.is_auto_disabled_now()
                    && route_account_allowed(
                        config,
                        pool.id,
                        Some(&route.endpoint),
                        Some(&support_candidates),
                    )
                    && account_route_allowed(pool, Some(&route.endpoint))
                    && account_matches_supported_models_normalized(pool, Some(&support_candidates))
            })
            .cloned()
            .map(|pool| AccountCandidate {
                pool,
                in_flight: 0,
                transient_failure_streak: 0,
                quality: None,
            })
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return None;
        }
        let eligible_pools = candidates.len();
        // 降级兜底路径**刻意只用优先级**：走到这里说明 Redis 不可用，
        // 而质量数据正是存在 Redis 里的——此时它要么读不到、要么已经陈旧，
        // 拿它做调度决策比不用更危险。
        let selected_pool = select_account_candidate_legacy(candidates, config)?;
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
        config: &AccountsConfig,
        route: &AccountRouteRequest,
    ) -> PoolSelectionSnapshot {
        if !config.enabled {
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
        config: &AccountsConfig,
        include_selection: bool,
        model_candidates: Option<&[Option<&str>]>,
        model_cooldown_candidates: Option<&[String]>,
    ) -> PoolSelectionSnapshot {
        if !config.enabled {
            return PoolSelectionSnapshot::default();
        }
        let authoritative_pools = match self.load_authoritative_pool_snapshot().await {
            Ok(pools) => pools,
            Err(unavailable) => return selection_unavailable_snapshot(unavailable),
        };
        let normalized_candidates = model_candidates.map(|candidates| {
            normalize_account_support_candidates(candidates.iter().flatten().copied())
        });
        self.scan_pool_availability_from_snapshot(
            &authoritative_pools,
            excluded,
            config,
            include_selection,
            normalized_candidates.as_deref(),
            model_cooldown_candidates,
            None,
        )
        .await
    }

    async fn scan_pool_availability_from_snapshot(
        &self,
        authoritative_pools: &Arc<Vec<Account>>,
        excluded: &HashSet<u64>,
        config: &AccountsConfig,
        include_selection: bool,
        model_candidates: Option<&[String]>,
        model_cooldown_candidates: Option<&[String]>,
        endpoint: Option<&str>,
    ) -> PoolSelectionSnapshot {
        if !config.enabled {
            return PoolSelectionSnapshot::default();
        }
        let pools = authoritative_pools
            .iter()
            .filter(|pool| {
                !excluded.contains(&pool.id)
                    && pool.enabled
                    && !pool.is_auto_disabled_now()
                    && route_account_allowed(config, pool.id, endpoint, model_candidates)
                    && account_route_allowed(pool, endpoint)
                    && account_matches_supported_models_normalized(pool, model_candidates)
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
        let requested_model_cooldowns: &[String] =
            if config.model_unavailable_cooldown_mode == ModelUnavailableCooldownMode::Model {
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
                if let Some(open) = err
                    .downcast_ref::<AccountCoordinatorOpen>()
                    .filter(|open| open.kind == AccountCoordinatorOpenKind::AdmissionSaturated)
                {
                    availability.temporary_unavailable_pools = pools.len();
                    availability.wait_reason = Some(PoolCapacityWaitReason::Full);
                    availability.wait_for = Some(open.retry_after);
                    return PoolSelectionSnapshot {
                        selected_pool: None,
                        availability,
                        degraded_fallback_local_lease: false,
                    };
                }
                let recovery_wait = err.downcast_ref::<AccountCoordinatorGuardError>().and_then(
                    |guard| match &guard.state {
                        AccountCoordinatorGuardState::Recovering { remaining, .. } => {
                            Some(*remaining)
                        }
                        _ => None,
                    },
                );
                tracing::debug!(
                    pool_count = pools.len(),
                    error = %err,
                    "批量读取账号 Redis 调度协调状态失败，暂停账号准入"
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
        pools: Vec<Account>,
        runtimes: Vec<anyhow::Result<PoolRuntimeSnapshot>>,
        config: &AccountsConfig,
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
                        "账号 Redis cooldown 状态无效，仅隔离当前池"
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
            if let Some((model_remaining_secs, model_reason)) = (cooldown_remaining_secs == 0)
                .then_some(runtime.model_cooldown)
                .flatten()
            {
                cooldown_remaining_secs = model_remaining_secs;
                cooldown_reason = model_reason;
                cooldown_scope = Some(PoolCooldownScope::Model);
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
                        candidates.push(AccountCandidate {
                            pool,
                            in_flight,
                            transient_failure_streak,
                            quality: runtime.quality,
                        });
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
        if let Some(candidates) = candidates.as_ref() {
            self.schedule_relative_quality_probation(candidates, config);
        }
        let selected_pool = if availability.coordinator_unavailable {
            None
        } else {
            candidates.and_then(|candidates| {
                select_account_candidate(candidates, config, account_now_ms())
            })
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
        config: &AccountsConfig,
    ) -> PoolAvailabilitySnapshot {
        self.pool_availability_snapshot_inner(excluded, config, true)
            .await
    }

    #[cfg(test)]
    async fn pool_availability_snapshot_inner(
        &self,
        excluded: &HashSet<u64>,
        config: &AccountsConfig,
        allow_cache: bool,
    ) -> PoolAvailabilitySnapshot {
        if !config.enabled {
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
            .scan_pool_availability_uncached(excluded, config, false, None, None)
            .await
            .availability;
        if cacheable {
            *self.availability_cache.lock() = Some(CachedPoolAvailabilitySnapshot {
                cache_key: cache_key.to_string(),
                snapshot: snapshot.clone(),
                expires_at: Instant::now() + ACCOUNT_AVAILABILITY_CACHE_TTL,
            });
        }
        snapshot
    }

    async fn handle_capacity_unavailable(
        &self,
        route: &AccountRouteRequest,
        attempts: Vec<AccountAttempt>,
        config: &AccountsConfig,
        context: PoolCapacityWaitContext,
        queue_guard: &mut Option<AccountQueueGuard>,
        wait_started_at: &mut Option<Instant>,
        capacity_waiter: &mut CapacityWaiter,
        dispatch_deadline: Option<Instant>,
    ) -> ExternalCapacityDecision {
        let reason = context.reason;
        let wait_for = context.wait_for;
        if reason == PoolCapacityWaitReason::CoordinatorUnavailable {
            if config.capacity_mode != CapacityMode::Wait {
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
                .map(|deadline| {
                    configured_wait.min(deadline.saturating_duration_since(Instant::now()))
                })
                .unwrap_or(configured_wait);
            if max_wait.is_zero() || started.elapsed() >= max_wait {
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

            // Redis itself is the account capacity coordinator, so do not
            // try to enter the Redis-backed queue while the coordinator is
            // unavailable. Sleep briefly within the dispatch deadline, then let
            // the caller re-read the coordinator and select a pool if it has
            // recovered.
            let mut wakeup = wait_for
                .unwrap_or_else(|| Duration::from_millis(100))
                .min(Duration::from_secs(1));
            let remaining = max_wait.saturating_sub(started.elapsed());
            if remaining.is_zero() {
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
            wakeup = wakeup.min(remaining);
            if !wakeup.is_zero() {
                let _ = timeout(wakeup, capacity_waiter.wait_for_change()).await;
            }
            return ExternalCapacityDecision::Retry;
        }

        if context.is_model_unavailable() || config.capacity_mode != CapacityMode::Wait {
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
                "account_deadline_exceeded",
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
                "account_deadline_exceeded",
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
                "account_wait_timeout",
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
                "account_wait_timeout",
                message,
                &route.error_id,
            ));
        }

        if queue_guard.is_none() {
            match self
                .enter_account_queue(config.max_queued_requests, max_wait)
                .await
            {
                Ok(Some(guard)) => *queue_guard = Some(guard),
                Ok(None) => {
                    let message = "Request dispatch queue is full";
                    self.record_external_failure(
                        route,
                        None,
                        attempts,
                        "account_queue_full",
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
                        "account_queue_full",
                        message,
                        &route.error_id,
                    ));
                }
                Err(err) => {
                    tracing::warn!(error = %err, "占用账号 Redis 调度排队 lease 失败");
                    let message =
                        "Request dispatch queue unavailable: Redis coordination unavailable";
                    self.record_external_failure(
                        route,
                        None,
                        attempts,
                        "account_queue_error",
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
                        "account_queue_error",
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
            tracing::warn!(error = %err, "续期账号 Redis 调度排队 lease 失败");
            let message = "Request dispatch queue unavailable: Redis coordination unavailable";
            self.record_external_failure(
                route,
                None,
                attempts,
                "account_queue_error",
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
                "account_queue_error",
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
                "account_wait_timeout",
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
                "account_wait_timeout",
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
        pool: &Account,
        in_flight: u32,
        global_in_flight: u32,
        cooldown_remaining_secs: u64,
        config: &AccountsConfig,
    ) -> Option<String> {
        if !config.enabled {
            return Some("accounts_disabled".to_string());
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
        let global_max = config.global_max_concurrent_requests;
        if global_max > 0 && global_in_flight >= global_max {
            return Some("global_concurrency_full".to_string());
        }
        None
    }

    async fn acquire_pool_for_route(
        &self,
        pool: &Account,
        config: &AccountsConfig,
        route: &AccountRouteRequest,
    ) -> PoolAcquireResult {
        let model_cooldown_candidates = route.model_cooldown_candidates();
        self.acquire_pool_with_model_cooldowns(pool, config, &model_cooldown_candidates)
            .await
    }

    fn acquire_degraded_fallback_local_pool(
        &self,
        pool: &Account,
        config: &AccountsConfig,
    ) -> PoolAcquireResult {
        let global_max = config.global_max_concurrent_requests;
        let global_permit = if global_max > 0 {
            let limiter = {
                let mut limiters = self.degraded_global_leases.lock();
                // 全局上限改过之后旧值的信号量同样只会积累，不会再被取到。
                limiters.retain(|existing_max, _| *existing_max == global_max);
                limiters
                    .entry(global_max)
                    .or_insert_with(|| Arc::new(Semaphore::new(global_max as usize)))
                    .clone()
            };
            match limiter.try_acquire_owned() {
                Ok(permit) => Some(permit),
                Err(_) => {
                    return PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                        reason: PoolCapacityWaitReason::Full,
                        wait_for: None,
                        exclude_pool_for_reselect: false,
                        detail: "degraded_global_capacity_full",
                    });
                }
            }
        } else {
            None
        };
        let max_concurrent = pool.max_concurrent_requests.max(1);
        let limiter = {
            let mut limiters = self.degraded_fallback_local_leases.lock();
            // 账号改过并发上限后旧上限的信号量不再使用：同一账号只保留当前上限的一份，
            // 否则每次改配置都会在这个 map 里多留一个永不回收的 Semaphore。
            limiters.retain(|(existing_pool_id, existing_max), _| {
                *existing_pool_id != pool.id || *existing_max == max_concurrent
            });
            limiters
                .entry((pool.id, max_concurrent))
                .or_insert_with(|| Arc::new(Semaphore::new(max_concurrent as usize)))
                .clone()
        };
        match limiter.try_acquire_owned() {
            Ok(permit) => PoolAcquireResult::Acquired(AccountLease::degraded_fallback_local(
                self.clone(),
                pool.id,
                uuid::Uuid::new_v4().to_string(),
                permit,
                global_permit,
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
        pool: &Account,
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
            match timeout(ACCOUNT_DISPATCH_FENCE_SHARED_WAIT_TIMEOUT, flight.wait()).await {
                Ok(result) => result,
                Err(_) => {
                    PoolDispatchFenceResult::CoordinatorUnavailable(AccountSelectionUnavailable {
                        kind: AccountSelectionFailureKind::PostgresTimeout,
                        retry_after: ACCOUNT_SELECTION_RETRY_AFTER,
                    })
                }
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
                if unavailable.kind == AccountSelectionFailureKind::AdmissionSaturated {
                    self.selection_saturated.fetch_add(1, Ordering::Relaxed);
                }
                return PoolDispatchFenceResult::CoordinatorUnavailable(unavailable);
            }
        };
        #[cfg(test)]
        self.dispatch_fence_pg_loads.fetch_add(1, Ordering::Relaxed);
        match timeout(
            ACCOUNT_DISPATCH_FENCE_TIMEOUT,
            self.postgres
                .account_dispatch_revision_matches(pool_id, revision),
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
                permit.failure(AccountSelectionFailureKind::PostgresError);
                PoolDispatchFenceResult::CoordinatorUnavailable(AccountSelectionUnavailable {
                    kind: AccountSelectionFailureKind::PostgresError,
                    retry_after: ACCOUNT_SELECTION_RETRY_AFTER,
                })
            }
            Err(_) => {
                permit.failure(AccountSelectionFailureKind::PostgresTimeout);
                PoolDispatchFenceResult::CoordinatorUnavailable(AccountSelectionUnavailable {
                    kind: AccountSelectionFailureKind::PostgresTimeout,
                    retry_after: ACCOUNT_SELECTION_RETRY_AFTER,
                })
            }
        }
    }

    #[cfg(test)]
    fn dispatch_fence_pg_loads_for_test(&self) -> u64 {
        self.dispatch_fence_pg_loads.load(Ordering::Acquire)
    }

    #[cfg(test)]
    async fn acquire_pool(&self, pool: &Account, config: &AccountsConfig) -> PoolAcquireResult {
        self.acquire_pool_with_model_cooldowns(pool, config, &[])
            .await
    }

    async fn acquire_pool_with_model_cooldowns(
        &self,
        pool: &Account,
        config: &AccountsConfig,
        model_cooldown_candidates: &[String],
    ) -> PoolAcquireResult {
        self.acquire_pool_with_model_cooldowns_and_max_age(
            pool,
            config,
            model_cooldown_candidates,
            Duration::from_secs(ACCOUNT_LEASE_MAX_AGE_SECS),
        )
        .await
    }

    async fn acquire_pool_with_model_cooldowns_and_max_age(
        &self,
        pool: &Account,
        config: &AccountsConfig,
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
        pool: &Account,
        config: &AccountsConfig,
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
        let coordination_epoch = match self.account_coordination_epoch().await {
            Ok(epoch) => epoch,
            Err(err) => {
                self.mark_account_coordinator_probe_required();
                tracing::warn!(pool_id = pool.id, error = %err, "账号 Redis 协调 epoch 暂不可用");
                return PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                    reason: PoolCapacityWaitReason::CoordinatorUnavailable,
                    wait_for: None,
                    exclude_pool_for_reselect: false,
                    detail: "coordinator_epoch_unavailable",
                });
            }
        };
        let mut cooldown_keys = vec![format!("account:{}:cooldown", pool.id)];
        if config.model_unavailable_cooldown_mode == ModelUnavailableCooldownMode::Model {
            cooldown_keys.extend(
                model_cooldown_candidates
                    .iter()
                    .map(|model| account_model_cooldown_key(pool.id, model)),
            );
        }
        let coordinator_permit = match self.coordinator_breaker.try_begin() {
            Ok(permit) => permit,
            Err(open) if open.kind == AccountCoordinatorOpenKind::AdmissionSaturated => {
                return PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                    reason: PoolCapacityWaitReason::Full,
                    wait_for: Some(open.retry_after),
                    exclude_pool_for_reselect: false,
                    detail: "coordinator_admission_saturated",
                });
            }
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
        let mut pending_lease = Some(AccountLease::pending(
            self.clone(),
            pool.id,
            lease_id.clone(),
            coordination_epoch.clone(),
            max_age,
            release_permit,
        ));
        let acquire_result = match timeout(
            ACCOUNT_COORDINATOR_REDIS_OPERATION_TIMEOUT,
            self.redis.acquire_account_lease(
                pool.id,
                &lease_id,
                &coordination_epoch,
                pool.max_concurrent_requests.max(1),
                config.global_max_concurrent_requests,
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
                self.mark_account_coordinator_probe_required();
                coordinator_permit.failure(AccountCoordinatorFailureKind::RedisError);
                tracing::debug!(
                    pool_id = pool.id,
                    error = %err,
                    "占用账号 Redis 并发槽失败"
                );
                return PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                    reason: PoolCapacityWaitReason::CoordinatorUnavailable,
                    wait_for: None,
                    exclude_pool_for_reselect: false,
                    detail: "lease_acquire_error",
                });
            }
            Err(_) => {
                self.mark_account_coordinator_probe_required();
                coordinator_permit.failure(AccountCoordinatorFailureKind::Timeout);
                return PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                    reason: PoolCapacityWaitReason::CoordinatorUnavailable,
                    wait_for: None,
                    exclude_pool_for_reselect: false,
                    detail: "lease_acquire_timeout",
                });
            }
        };
        match acquire_result {
            RedisAccountLeaseAcquireResult::Acquired {
                lease_id: acquired_lease_id,
                ..
            } if acquired_lease_id == lease_id => {
                let mut lease = pending_lease.take().expect("pending account lease");
                lease.confirm();
                PoolAcquireResult::Acquired(lease)
            }
            RedisAccountLeaseAcquireResult::Acquired { .. } => {
                PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                    reason: PoolCapacityWaitReason::CoordinatorUnavailable,
                    wait_for: None,
                    exclude_pool_for_reselect: false,
                    detail: "lease_acquire_id_mismatch",
                })
            }
            RedisAccountLeaseAcquireResult::PoolCooldown { remaining } => {
                pending_lease
                    .take()
                    .expect("pending account lease")
                    .disarm();
                PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                    reason: PoolCapacityWaitReason::Cooldown,
                    wait_for: Some(remaining.unwrap_or_else(|| Duration::from_secs(1))),
                    exclude_pool_for_reselect: true,
                    detail: "cooldown_during_atomic_acquire",
                })
            }
            RedisAccountLeaseAcquireResult::ModelCooldown { remaining } => {
                pending_lease
                    .take()
                    .expect("pending account lease")
                    .disarm();
                PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                    reason: PoolCapacityWaitReason::ModelUnavailable,
                    wait_for: Some(remaining.unwrap_or_else(|| Duration::from_secs(1))),
                    exclude_pool_for_reselect: true,
                    detail: "model_cooldown_during_atomic_acquire",
                })
            }
            RedisAccountLeaseAcquireResult::PoolCapacityFull { .. } => {
                pending_lease
                    .take()
                    .expect("pending account lease")
                    .disarm();
                PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                    reason: PoolCapacityWaitReason::Full,
                    wait_for: None,
                    exclude_pool_for_reselect: true,
                    detail: "pool_concurrency_full_during_atomic_acquire",
                })
            }
            RedisAccountLeaseAcquireResult::GlobalCapacityFull { .. } => {
                pending_lease
                    .take()
                    .expect("pending account lease")
                    .disarm();
                PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                    reason: PoolCapacityWaitReason::Full,
                    wait_for: None,
                    exclude_pool_for_reselect: false,
                    detail: "global_concurrency_full_during_atomic_acquire",
                })
            }
            RedisAccountLeaseAcquireResult::Released => {
                pending_lease
                    .take()
                    .expect("pending account lease")
                    .disarm();
                PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                    reason: PoolCapacityWaitReason::CoordinatorUnavailable,
                    wait_for: None,
                    exclude_pool_for_reselect: false,
                    detail: "lease_released_before_acquire",
                })
            }
            RedisAccountLeaseAcquireResult::CoordinatorEpochMismatch { .. } => {
                pending_lease
                    .take()
                    .expect("pending account lease")
                    .disarm();
                if epoch_reconciliations >= 3 {
                    return PoolAcquireResult::Unavailable(PoolAcquireUnavailable {
                        reason: PoolCapacityWaitReason::CoordinatorUnavailable,
                        wait_for: None,
                        exclude_pool_for_reselect: false,
                        detail: "coordinator_epoch_churn",
                    });
                }
                self.mark_account_coordinator_probe_required();
                Box::pin(self.acquire_pool_with_model_cooldowns_and_max_age_inner(
                    pool,
                    config,
                    model_cooldown_candidates,
                    max_age,
                    epoch_reconciliations + 1,
                ))
                .await
            }
            RedisAccountLeaseAcquireResult::CoordinatorRecovering { remaining, .. } => {
                pending_lease
                    .take()
                    .expect("pending account lease")
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

    async fn enter_account_queue(
        &self,
        max_queued: u32,
        max_wait: Duration,
    ) -> anyhow::Result<Option<AccountQueueGuard>> {
        let lease_id = uuid::Uuid::new_v4().to_string();
        let lease_policy = account_queue_lease_policy(Some(max_wait));
        // Arm cleanup before awaiting Redis so cancellation and commit-unknown both remove this ID.
        let guard = AccountQueueGuard::new(self.clone(), lease_id.clone(), lease_policy);
        let admitted = timeout(
            ACCOUNT_QUEUE_REDIS_OPERATION_TIMEOUT,
            self.redis.try_enter_account_dispatch_queue(
                &lease_id,
                max_queued,
                lease_policy.ttl_secs,
            ),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "Redis account queue admission timed out after {}ms",
                ACCOUNT_QUEUE_REDIS_OPERATION_TIMEOUT.as_millis()
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
        let current_epoch = self.account_coordination_epoch().await?;
        if current_epoch != coordination_epoch {
            return Ok(false);
        }
        match self
            .redis
            .touch_account_lease(pool_id, lease_id, ttl_secs, coordination_epoch)
            .await
        {
            Ok(touched) => return Ok(touched),
            Err(err) => {
                self.mark_account_coordinator_probe_required();
                return Err(err);
            }
        }
    }

    async fn record_account_soft_failure(&self, pool_id: u64, reason: &str) -> Option<u64> {
        let key = account_transient_failure_key(pool_id);
        match timeout(
            ACCOUNT_AUTO_DISABLE_REDIS_TIMEOUT,
            self.redis
                .incr_with_ttl(&key, ACCOUNT_TRANSIENT_FAILURE_WINDOW_SECS),
        )
        .await
        {
            Ok(Ok(streak)) => {
                tracing::debug!(
                    pool_id,
                    reason,
                    streak = streak.max(1),
                    window_secs = ACCOUNT_TRANSIENT_FAILURE_WINDOW_SECS,
                    "recorded account soft failure for health-aware selection"
                );
            }
            Ok(Err(err)) => {
                tracing::warn!(
                    pool_id,
                    reason,
                    error = %err,
                    "账号软失败计数写入失败"
                );
            }
            Err(_) => {
                tracing::warn!(
                    pool_id,
                    reason,
                    timeout_ms = ACCOUNT_AUTO_DISABLE_REDIS_TIMEOUT.as_millis(),
                    "账号软失败计数写入超时"
                );
            }
        }
        let reason_key = account_transient_failure_reason_key(pool_id, reason);
        let reason_streak = match timeout(
            ACCOUNT_AUTO_DISABLE_REDIS_TIMEOUT,
            self.redis
                .incr_with_ttl(&reason_key, ACCOUNT_TRANSIENT_FAILURE_WINDOW_SECS),
        )
        .await
        {
            Ok(Ok(streak)) => Some(streak.max(1)),
            Ok(Err(err)) => {
                tracing::warn!(
                    pool_id,
                    reason,
                    error = %err,
                    "账号按错误原因统计软失败计数写入失败"
                );
                None
            }
            Err(_) => {
                tracing::warn!(
                    pool_id,
                    reason,
                    timeout_ms = ACCOUNT_AUTO_DISABLE_REDIS_TIMEOUT.as_millis(),
                    "账号按错误原因统计软失败计数写入超时"
                );
                None
            }
        };
        self.invalidate_account_runtime_capacity_state();
        self.capacity_signal.notify_state_changed();
        reason_streak
    }

    async fn mark_pool_cooldown(&self, pool_id: u64, duration: Duration, reason: String) {
        let until = Utc::now() + chrono::Duration::from_std(duration).unwrap_or_default();
        match timeout(
            ACCOUNT_COOLDOWN_REDIS_TIMEOUT,
            self.redis.set_json(
                format!("account:{}:cooldown", pool_id),
                &json!({ "until": until.to_rfc3339(), "reason": reason }),
                duration.as_secs().max(1) as usize,
            ),
        )
        .await
        {
            Ok(Ok(())) => {
                self.record_cooldown_event(pool_id, &reason, duration, None);
            }
            Ok(Err(err)) => tracing::warn!(pool_id, "写入账号 Redis cooldown 失败: {}", err),
            Err(_) => tracing::warn!(
                pool_id,
                timeout_ms = ACCOUNT_COOLDOWN_REDIS_TIMEOUT.as_millis(),
                "写入账号 Redis cooldown 超时，继续换号"
            ),
        }
        self.invalidate_account_runtime_capacity_state();
        self.capacity_signal.notify_state_changed();
    }

    /// 账号名：优先权威快照里的名字，拿不到就用 `#id`。只读缓存，不触发加载。
    fn cached_pool_label(&self, pool_id: u64) -> String {
        let snapshot = self.authoritative_pool_snapshot.lock();
        snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.result.as_ref().ok())
            .and_then(|pools| pools.iter().find(|pool| pool.id == pool_id))
            .map(|pool| pool.name.clone())
            .unwrap_or_else(|| format!("#{pool_id}"))
    }

    /// 冷却落账号异动审计（进入冷却）。冷却可能很频繁，同账号同原因 60 秒内只记一次。
    fn record_cooldown_event(
        &self,
        pool_id: u64,
        reason: &str,
        duration: Duration,
        models: Option<&[String]>,
    ) {
        // 节流键带上模型：同一账号不同模型的冷却分别记，不会被前一个模型的事件吞掉。
        let throttle_key = match models {
            Some(models) if !models.is_empty() => format!("cooldown:{reason}:{}", models.join(",")),
            _ => format!("cooldown:{reason}"),
        };
        if !self.account_event_throttle.should_record_with_window(
            pool_id,
            &throttle_key,
            crate::storage::account_events::ACCOUNT_COOLDOWN_EVENT_THROTTLE_WINDOW,
        ) {
            return;
        }
        let mut detail = json!({
            "reason": reason,
            "durationSecs": duration.as_secs(),
            "untilMs": (Utc::now() + chrono::Duration::from_std(duration).unwrap_or_default())
                .timestamp_millis(),
        });
        if let Some(models) = models {
            detail["models"] = json!(models);
        }
        record_account_event(
            self.postgres.clone(),
            "system-scheduler",
            AccountEvent {
                kind: AccountEventKind::CooldownStarted,
                source: AccountSource::External,
                account_id: pool_id,
                account_label: self.cached_pool_label(pool_id),
                reason: reason.to_string(),
                reason_label: account_cooldown_reason_label(reason).to_string(),
                detail,
            },
        );
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
        // One deadline for the whole list: a request may name several models, and a slow Redis
        // must cost the failover at most one timeout, not one per model.
        let deadline = tokio::time::Instant::now() + ACCOUNT_COOLDOWN_REDIS_TIMEOUT;
        let mut written = Vec::with_capacity(models.len());
        for model in models {
            let key = account_model_cooldown_key(pool_id, model);
            match tokio::time::timeout_at(
                deadline,
                self.redis.set_json(
                    key,
                    &json!({
                        "until": until.to_rfc3339(),
                        "reason": reason,
                        "model": model,
                    }),
                    ttl,
                ),
            )
            .await
            {
                Ok(Ok(())) => written.push(model.clone()),
                Ok(Err(err)) => tracing::warn!(
                    pool_id,
                    model = %model,
                    "写入账号模型级 Redis cooldown 失败: {}",
                    err
                ),
                Err(_) => {
                    tracing::warn!(
                        pool_id,
                        model = %model,
                        timeout_ms = ACCOUNT_COOLDOWN_REDIS_TIMEOUT.as_millis(),
                        "写入账号模型级 Redis cooldown 超时，跳过剩余模型并继续换号"
                    );
                    break;
                }
            }
        }
        if !written.is_empty() {
            self.record_cooldown_event(pool_id, &reason, duration, Some(&written));
        }
        self.invalidate_account_runtime_capacity_state();
        self.capacity_signal.notify_state_changed();
    }

    pub async fn clear_pool_cooldowns(&self, pool_id: u64) -> anyhow::Result<usize> {
        let pool_keys = [
            format!("account:{pool_id}:cooldown"),
            account_transient_failure_key(pool_id),
            account_quality_key(pool_id),
        ];
        let pool_deleted = self.redis.del_many(&pool_keys).await?;
        let model_deleted = self
            .redis
            .del_pattern(format!("account:{pool_id}:model_cooldown:*"))
            .await?;
        let reason_deleted = self
            .redis
            .del_pattern(format!("account:{pool_id}:transient_failures:*"))
            .await?;
        self.invalidate_account_runtime_capacity_state();
        self.capacity_signal.notify_state_changed();
        Ok(pool_deleted
            .saturating_add(model_deleted)
            .saturating_add(reason_deleted))
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
            .ok_or_else(|| anyhow::anyhow!("Redis returned no account runtime snapshot"))
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
            .map(|pool_id| AccountCoordinatorSnapshotRequest {
                pool_id: *pool_id,
                cooldown_keys: account_cooldown_keys(*pool_id, models),
                transient_failure_key: account_transient_failure_key(*pool_id),
                // 质量状态始终随并发/冷却一起读取：它与既有键在同一次 Lua
                // 往返内完成，额外开销只有一个 GET。若改为按开关条件读取，
                // 选择运行态快照的缓存键就要带上开关状态，开关切换会让缓存
                // 失效并在热路径上造成抖动。读取与否和是否参与评分解耦，
                // 由 select_account_candidate 按配置决定。
                quality_key: Some(account_quality_key(*pool_id)),
            })
            .collect::<Vec<_>>();
        let mut epoch_reconciliations = 0usize;
        let coordinators = loop {
            let coordinator_permit = self.coordinator_breaker.try_begin()?;
            let coordination_epoch = match self.account_coordination_epoch().await {
                Ok(epoch) => epoch,
                Err(err) => {
                    self.mark_account_coordinator_probe_required();
                    coordinator_permit.failure(AccountCoordinatorFailureKind::RedisError);
                    return Err(err);
                }
            };
            if coordination_epoch.is_empty() {
                coordinator_permit.failure(AccountCoordinatorFailureKind::RedisError);
                anyhow::bail!("account coordinator epoch is empty");
            }
            match timeout(
                ACCOUNT_COORDINATOR_REDIS_OPERATION_TIMEOUT,
                self.redis.account_coordinator_snapshots(
                    &requests,
                    Some(Duration::from_secs(
                        DEFAULT_ACCOUNT_REQUEST_TIMEOUT_SECS.saturating_mul(2),
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
                        .downcast_ref::<AccountCoordinatorGuardError>()
                        .map(|guard| guard.state.clone());
                    if let Some(guard_state) = guard_state {
                        coordinator_permit.success();
                        match guard_state {
                            AccountCoordinatorGuardState::EpochMismatch { .. }
                                if epoch_reconciliations < 3 =>
                            {
                                epoch_reconciliations += 1;
                                self.mark_account_coordinator_probe_required();
                                continue;
                            }
                            state => {
                                return Err(anyhow::Error::new(AccountCoordinatorGuardError {
                                    state,
                                }));
                            }
                        }
                    }
                    self.mark_account_coordinator_probe_required();
                    coordinator_permit.failure(AccountCoordinatorFailureKind::RedisError);
                    return Err(err);
                }
                Err(_) => {
                    self.mark_account_coordinator_probe_required();
                    coordinator_permit.failure(AccountCoordinatorFailureKind::Timeout);
                    return Err(anyhow::anyhow!(
                        "Redis account coordinator snapshot timed out after {}ms",
                        ACCOUNT_COORDINATOR_REDIS_OPERATION_TIMEOUT.as_millis()
                    ));
                }
            }
        };
        if coordinators.len() != requests.len() {
            anyhow::bail!("Redis returned an incomplete account coordinator snapshot batch");
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
            expires_at: Instant::now() + ACCOUNT_SELECTION_RUNTIME_SNAPSHOT_TTL,
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
                        "账号运行态后台刷新失败；本地主路径保持本地调度语义"
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
                expires_at: Instant::now() + ACCOUNT_SELECTION_RUNTIME_SNAPSHOT_TTL,
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
        pool: &Account,
        config: &AccountsConfig,
        reason: &str,
        last_error: &str,
    ) {
        if !pool_auto_disable_policy_enabled(pool.auto_disable_policy, config) {
            return;
        }
        if !auto_disable_reason_enabled(config, reason) {
            return;
        }
        let threshold = config.auto_disable_failure_threshold.max(1) as u64;
        let claim = timeout(
            ACCOUNT_AUTO_DISABLE_REDIS_TIMEOUT,
            self.redis.claim_account_auto_disable_transition(
                pool.id,
                pool.revision,
                reason,
                threshold,
                config.auto_disable_window_secs.max(1) as usize,
                ACCOUNT_AUTO_DISABLE_TRANSITION_CLAIM_TTL_SECS,
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
                    "账号自动禁用 Redis 计数/claim 失败，跳过 PostgreSQL 变更"
                );
                return;
            }
            Err(_) => {
                tracing::warn!(
                    pool_id = pool.id,
                    pool_revision = pool.revision,
                    reason,
                    timeout_ms = ACCOUNT_AUTO_DISABLE_REDIS_TIMEOUT.as_millis(),
                    "账号自动禁用 Redis 计数/claim 超时，跳过 PostgreSQL 变更"
                );
                return;
            }
        };
        if count < threshold || !claimed {
            return;
        }

        let changed = timeout(
            ACCOUNT_AUTO_DISABLE_POSTGRES_TIMEOUT,
            self.postgres.auto_disable_account_if_unchanged(
                pool.id,
                pool.revision,
                reason,
                last_error,
                config.auto_disable_duration_secs,
            ),
        )
        .await;
        match changed {
            Ok(Ok(true)) => {
                let (kind, reason_label) = account_disable_event(reason);
                record_account_event(
                    self.postgres.clone(),
                    "system-account",
                    AccountEvent {
                        kind,
                        source: AccountSource::External,
                        account_id: pool.id,
                        account_label: pool.name.clone(),
                        reason: reason.to_string(),
                        reason_label: reason_label.to_string(),
                        detail: json!({
                            "baseUrl": pool.base_url,
                            "lastError": last_error.chars().take(500).collect::<String>(),
                            "failureCount": count,
                            "threshold": threshold,
                            "disabledForSecs": config.auto_disable_duration_secs,
                        }),
                    },
                );
                // 在请求失败路径上，通知发布只能占用有限时间；超时也只是其它实例晚几秒
                // 看到禁用状态（它们有自己的探测），不能让这里卡住失败处理。
                match timeout(
                    ACCOUNT_AUTO_DISABLE_REDIS_TIMEOUT,
                    self.publish_account_data_changed("auto_disable", Some(pool.id)),
                )
                .await
                {
                    Ok(Ok(_)) => {}
                    Ok(Err(err)) => tracing::warn!(
                        pool_id = pool.id,
                        pool_revision = pool.revision,
                        error = %err,
                        "自动禁用账号已提交，但跨实例失效通知发布失败"
                    ),
                    Err(_) => tracing::warn!(
                        pool_id = pool.id,
                        pool_revision = pool.revision,
                        timeout_ms = ACCOUNT_AUTO_DISABLE_REDIS_TIMEOUT.as_millis(),
                        "自动禁用账号已提交，但跨实例失效通知发布超时"
                    ),
                }
            }
            Ok(Ok(false)) => {
                tracing::debug!(
                    pool_id = pool.id,
                    pool_revision = pool.revision,
                    reason,
                    "账号自动禁用条件更新未生效，pool revision 已变化或状态已转换"
                );
            }
            Ok(Err(err)) => {
                tracing::warn!(
                    pool_id = pool.id,
                    pool_revision = pool.revision,
                    error = %err,
                    "自动禁用账号 PostgreSQL 条件更新失败"
                );
            }
            Err(_) => {
                tracing::warn!(
                    pool_id = pool.id,
                    pool_revision = pool.revision,
                    timeout_ms = ACCOUNT_AUTO_DISABLE_POSTGRES_TIMEOUT.as_millis(),
                    "自动禁用账号 PostgreSQL 条件更新超时"
                );
            }
        }
    }

    fn reset_pool_auto_disable_failure_counts(&self, pool_id: u64) {
        let now = Instant::now();
        let mut recent = self.success_reset_recent.lock();
        recent.retain(|_, reset_at| {
            now.saturating_duration_since(*reset_at) < ACCOUNT_SUCCESS_RESET_COALESCE_WINDOW
        });
        if recent.contains_key(&pool_id) {
            return;
        }
        if recent.len() >= ACCOUNT_SUCCESS_RESET_MAX_TRACKED_POOLS {
            tracing::warn!(
                pool_id,
                tracked_pools = recent.len(),
                "账号 success reset 合并索引已达硬上限，跳过本次清理"
            );
            return;
        }
        let Ok(task_permit) = self.success_reset_semaphore.clone().try_acquire_owned() else {
            tracing::warn!(
                pool_id,
                max_tasks = ACCOUNT_SUCCESS_RESET_MAX_TASKS,
                "账号 success reset 后台任务已达硬上限，跳过本次清理"
            );
            return;
        };
        recent.insert(pool_id, now);
        drop(recent);

        let redis = self.redis.clone();
        let keys = ACCOUNT_AUTO_DISABLE_REASONS
            .iter()
            .map(|reason| format!("account:{}:auto_disable_failures:{}", pool_id, reason))
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
            let result = timeout(ACCOUNT_SUCCESS_RESET_REDIS_TIMEOUT, redis.del_many(&keys)).await;
            match result {
                Ok(Ok(_)) => {}
                Ok(Err(err)) => tracing::warn!(
                    pool_id,
                    error = %err,
                    "单命令清理账号 5 个自动禁用失败计数失败"
                ),
                Err(_) => tracing::warn!(
                    pool_id,
                    timeout_ms = ACCOUNT_SUCCESS_RESET_REDIS_TIMEOUT.as_millis(),
                    "单命令清理账号 5 个自动禁用失败计数超时"
                ),
            }
            #[cfg(test)]
            tasks_in_flight.fetch_sub(1, Ordering::Relaxed);
            drop(task_permit);
        });
    }

    /// 记录一次账号被动质量样本。
    ///
    /// 与 `reset_pool_auto_disable_failure_counts` 不同，这里**不做合并**：
    /// 每个请求都是一个独立数据点，合并会直接破坏 EWMA 的统计意义。
    /// 写入是 fire-and-forget 的后台任务，失败只记日志，绝不影响主请求。
    fn record_pool_quality_sample(
        &self,
        config: &AccountsConfig,
        pool_id: u64,
        success: bool,
        ttft_ms: Option<u64>,
        latency_ms: Option<u64>,
    ) {
        if !config.quality_aware_scheduling_enabled {
            return;
        }
        let Ok(task_permit) = self.quality_sample_semaphore.clone().try_acquire_owned() else {
            tracing::debug!(
                pool_id,
                max_tasks = ACCOUNT_QUALITY_SAMPLE_MAX_TASKS,
                "账号质量采样后台任务已达上限，丢弃本次样本"
            );
            return;
        };
        let redis = self.redis.clone();
        let postgres = self.postgres.clone();
        let history_throttle = self.quality_history_throttle.clone();
        let alpha = config.quality_ewma_alpha;
        let ttl = Duration::from_secs(config.quality_sample_ttl_secs.max(1));
        let degrade_window = Duration::from_secs(config.degrade_window_secs.max(1));
        tokio::spawn(async move {
            let _task_permit = task_permit;
            let result = timeout(
                ACCOUNT_QUALITY_SAMPLE_REDIS_TIMEOUT,
                redis.record_account_quality_sample_with_window(
                    pool_id,
                    success,
                    ttft_ms,
                    latency_ms,
                    crate::storage::redis_cache::AccountQualitySampleTuning {
                        alpha,
                        ttl,
                        degrade_window,
                    },
                ),
            )
            .await;
            match result {
                Ok(Ok(quality)) => {
                    // 质量分历史：每账号每 60 秒落一行 EWMA 快照，风控页画趋势用。
                    // 写入走尽力而为队列，失败只记日志。
                    if history_throttle.should_record(pool_id, "") {
                        let sample = AccountQualitySampleRow {
                            account_id: pool_id,
                            sampled_at: Utc::now(),
                            recent_error_rate: quality.recent_error_rate,
                            ttft_ewma_ms: quality.ttft_ewma_ms,
                            latency_ewma_ms: quality.latency_ewma_ms,
                            sample_count: quality.sample_count,
                        };
                        spawn_best_effort_storage_task("记录账号质量采样快照", async move {
                            postgres.record_account_quality_sample(&sample).await
                        });
                    }
                }
                Ok(Err(err)) => {
                    tracing::debug!(
                        pool_id,
                        error = %err,
                        "记录账号质量样本失败"
                    );
                }
                Err(_) => {
                    tracing::debug!(pool_id, "记录账号质量样本超时");
                }
            }
        });
    }

    /// 在当前请求已经通过路径和模型准入的整个候选池内计算相对劣化。
    ///
    /// 质量采样是被动的，不能在单个样本写入时拿一个绝对红线决定账号
    /// 是否降级。这里使用同一请求候选集合的中位数作为基线：全体一起慢/报错
    /// 时没有相对落差，不会把整个账号一起打入 probation。
    fn schedule_relative_quality_probation(
        &self,
        candidates: &[AccountCandidate],
        config: &AccountsConfig,
    ) {
        if !config.quality_aware_scheduling_enabled || candidates.len() < 2 {
            return;
        }
        let qualified_count = candidates
            .iter()
            .filter(|candidate| {
                candidate
                    .quality
                    .as_ref()
                    .is_some_and(|quality| quality.has_enough_samples(config.quality_min_samples))
            })
            .count();
        if qualified_count < 2 {
            return;
        }
        let baseline =
            AccountQualityBaseline::from_candidates(candidates, config.quality_min_samples);
        let now_ms = account_now_ms();
        for candidate in candidates.iter().cloned() {
            let Some(decision) =
                evaluate_account_degrade_relative(&candidate, &baseline, config, now_ms)
            else {
                continue;
            };
            let AccountDegradeDecision::Probation {
                until_ms,
                level,
                ttl_secs,
                reset_level,
            } = decision
            else {
                continue;
            };
            let Ok(task_permit) = self.quality_sample_semaphore.clone().try_acquire_owned() else {
                tracing::debug!(
                    pool_id = candidate.pool.id,
                    max_tasks = ACCOUNT_QUALITY_SAMPLE_MAX_TASKS,
                    "账号质量 probation 写入任务已达上限，跳过本次标记"
                );
                continue;
            };
            let redis = self.redis.clone();
            let pool_id = candidate.pool.id;
            let pool_name = candidate.pool.name.clone();
            let postgres = self.postgres.clone();
            let event_throttle = self.account_event_throttle.clone();
            tokio::spawn(async move {
                let _task_permit = task_permit;
                let marked = timeout(
                    ACCOUNT_QUALITY_SAMPLE_REDIS_TIMEOUT,
                    redis.mark_account_probation_with_reset(
                        pool_id,
                        until_ms,
                        level,
                        Duration::from_secs(ttl_secs.max(1)),
                        reset_level,
                    ),
                )
                .await;
                match marked {
                    Ok(Ok(())) => {
                        // 观察期落账号异动审计；同账号 60 秒内只记一次。
                        if event_throttle.should_record(pool_id, "relative_quality") {
                            record_account_event(
                                postgres,
                                "system-scheduler",
                                AccountEvent {
                                    kind: AccountEventKind::ProbationStarted,
                                    source: AccountSource::External,
                                    account_id: pool_id,
                                    account_label: pool_name,
                                    reason: "relative_quality".to_string(),
                                    reason_label: "相对质量劣化".to_string(),
                                    detail: json!({
                                        "level": level,
                                        "untilMs": until_ms,
                                        "ttlSecs": ttl_secs,
                                        "resetLevel": reset_level,
                                    }),
                                },
                            );
                        }
                    }
                    Ok(Err(err)) => tracing::debug!(
                        pool_id,
                        error = %err,
                        "写入账号相对质量 probation 失败"
                    ),
                    Err(_) => tracing::debug!(pool_id, "写入账号相对质量 probation 超时"),
                }
            });
        }
    }

    fn record_external_success(
        &self,
        config: &AccountsConfig,
        route: &AccountRouteRequest,
        pool: &Account,
        attempts: Vec<AccountAttempt>,
        billing: Option<AccountBilling>,
        downstream_stop_reason: Option<String>,
    ) {
        self.reset_pool_auto_disable_failure_counts(pool.id);
        // 成功回灌：此前账号只有失败信号靠 TTL 过期，成功请求对调度零反馈，
        // 导致质量曲线只降不升。这里补上延迟 EWMA 与失败率衰减。
        let ttft_ms = match route.first_token_latency_ms.load(Ordering::Acquire) {
            0 => None,
            value => Some(value),
        };
        let latency_ms = route.started_at.elapsed().as_millis().min(u64::MAX as u128) as u64;
        self.record_pool_quality_sample(config, pool.id, true, ttft_ms, Some(latency_ms));
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
            downstream_stop_reason,
            None,
        );
    }

    fn record_external_client_dropped(
        &self,
        route: &AccountRouteRequest,
        pool: Option<&Account>,
        attempts: Vec<AccountAttempt>,
    ) {
        self.record_external(
            route,
            pool,
            attempts,
            UsageRecordStatus::ClientDropped,
            Some("client_dropped".to_string()),
            Some(EXTERNAL_EARLY_COMMIT_CLIENT_GONE_MESSAGE.to_string()),
            Some(format!(
                "client_dropped: {}",
                EXTERNAL_EARLY_COMMIT_CLIENT_GONE_MESSAGE
            )),
            UsageErrorDiagnostics {
                status_code: None,
                source: Some("downstream_client".to_string()),
                error_id: Some(route.error_id.clone()),
                metadata: None,
                public_error: None,
                raw_upstream_error: None,
            },
            None,
            None,
            None,
        );
    }

    fn record_external_failure(
        &self,
        route: &AccountRouteRequest,
        pool: Option<&Account>,
        attempts: Vec<AccountAttempt>,
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
            None,
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
        let capture_request_id = route.request_id.clone();
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
            downstream_chunks_forwarded: 0,
            downstream_bytes_forwarded: 0,
            estimated_output_tokens: 0,
            terminal_event_forwarded: false,
            completed: false,
        };
        let stream = futures::stream::unfold(
            (data_stream, Some(guard)),
            |(mut data_stream, mut guard)| async move {
                match data_stream.next().await {
                    Some(Ok(chunk)) => {
                        if let Some(guard_ref) = guard.as_mut() {
                            guard_ref.record_downstream_chunk(&chunk);
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
        if crate::diagnostics::capture::is_active() {
            let stream = crate::diagnostics::capture::tap_client_stream(
                capture_request_id,
                "external",
                stream,
            );
            return Response::from_parts(parts, Body::from_stream(stream));
        }
        Response::from_parts(parts, Body::from_stream(stream))
    }

    fn record_external(
        &self,
        route: &AccountRouteRequest,
        pool: Option<&Account>,
        attempts: Vec<AccountAttempt>,
        status: UsageRecordStatus,
        error_type: Option<String>,
        error_message: Option<String>,
        error_detail: Option<String>,
        error_diagnostics: UsageErrorDiagnostics,
        billing: Option<AccountBilling>,
        downstream_stop_reason: Option<String>,
        upstream_response_model: Option<String>,
    ) {
        // 失败或断开的流没有计费明细，上游模型由调用方从采集状态直接给出。
        let upstream_response_model = upstream_response_model.or_else(|| {
            billing
                .as_ref()
                .and_then(|billing| billing.upstream_response_model.clone())
        });
        let request_input_tokens = if route.request_input_tokens > 0 {
            route.request_input_tokens
        } else {
            billing
                .as_ref()
                .and_then(|billing| billing.request_input_tokens)
                .filter(|tokens| *tokens > 0)
                .unwrap_or_else(|| estimated_external_request_input_tokens(route, None))
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
            account_cost_usd: if status == UsageRecordStatus::Success {
                billing
                    .as_ref()
                    .map(|billing| {
                        billing.raw_cost_usd * pool.map(|p| p.cost_multiplier).unwrap_or(1.0)
                    })
                    .unwrap_or(0.0)
            } else {
                0.0
            },
            id: route.request_id.clone(),
            created_at: Utc::now().to_rfc3339(),
            endpoint: route.endpoint.to_string(),
            stream: route.is_stream(),
            model: route.requested_model(),
            requested_max_tokens: route.requested_max_tokens(),
            downstream_stop_reason,
            upstream_model: route.upstream_model.clone(),
            external_outbound_model,
            upstream_response_model,
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

            route_kind: Some(UsageRouteKind::Account),
            route_subtype: Some(route.route_subtype),

            account_id: pool.map(|pool| pool.id),
            account_name: pool.map(|pool| pool.name.clone()),
            external_attempts: attempts,
            usage_projection_applied: billing
                .as_ref()
                .map(|billing| billing.usage_projection_applied),
            account_billing: billing,
            error_type,
            error_message,
            error_detail,
            error_status_code: error_diagnostics.status_code,
            error_source: error_diagnostics.source,
            error_id: error_diagnostics.error_id,
            error_metadata: error_diagnostics.metadata,
            raw_upstream_error: error_diagnostics.raw_upstream_error,
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
    usage: Option<AccountUsageSnapshot>,
) -> AccountUsageSnapshot {
    if status == UsageRecordStatus::Success {
        return usage.unwrap_or(AccountUsageSnapshot {
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

    AccountUsageSnapshot {
        total_input_tokens: request_input_tokens.max(0),
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
    billing: Option<&AccountBilling>,
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

fn external_usage_latency_trace(route: &AccountRouteRequest) -> UsageLatencyTrace {
    let mut latency_trace = route.latency_trace.snapshot().unwrap_or_default();
    latency_trace.inference_attempts = Some(route.inference_attempt_budget.snapshot());
    latency_trace.credential_dispatch_elapsed_ms = route
        .inference_attempt_budget
        .credential_dispatch_elapsed_ms();
    latency_trace.upstream_header_wait_ms =
        route.inference_attempt_budget.upstream_header_wait_ms();
    latency_trace
}

struct ExternalStreamUsageGuard {
    manager: AccountManager,
    config: AccountsConfig,
    route: AccountRouteRequest,
    pool: Account,
    attempts: Vec<AccountAttempt>,
    outbound_model: Option<String>,
    outbound_body: Bytes,
    usage_capture: Option<Arc<SyncMutex<ExternalUsageCapture>>>,
    usage_projection: Option<ExternalUsageProjectionContext>,
    response_status: StatusCode,
    response_content_type: Option<String>,
    chunks_before_first_output: u32,
    events_before_first_output: u32,
    downstream_chunks_forwarded: u64,
    downstream_bytes_forwarded: u64,
    estimated_output_tokens: i32,
    /// 已经把终止事件（`message_stop`、带 `stop_reason` 的 `message_delta`、`[DONE]`）
    /// 转发给下游。此后下游关连接是“读完即走”，不是中途断开。
    terminal_event_forwarded: bool,
    completed: bool,
}

fn record_external_stream_end(
    route: &AccountRouteRequest,
    pool: &Account,
    attempts: &[AccountAttempt],
    reason: &str,
    detail: Option<&str>,
    chunks_forwarded: u64,
    bytes_forwarded: u64,
) {
    if !crate::diagnostics::capture::is_active() {
        return;
    }
    let downstream_committed = route
        .inference_attempt_budget
        .snapshot()
        .downstream_committed;
    crate::diagnostics::capture::record("upstream_stream_end", Some(&route.request_id), || {
        json!({
            "poolId": pool.id,
            "reason": reason,
            "detail": detail,
            "attempt": attempts.len(),
            "chunksForwarded": chunks_forwarded,
            "bytesForwarded": bytes_forwarded,
            "downstreamCommitted": downstream_committed,
        })
    });
}

impl ExternalStreamUsageGuard {
    fn upstream_response_model(&self) -> Option<String> {
        self.usage_capture
            .as_ref()
            .and_then(|capture| capture.lock().upstream_response_model.clone())
    }

    fn record_downstream_chunk(&mut self, chunk: &Bytes) {
        self.downstream_chunks_forwarded = self.downstream_chunks_forwarded.saturating_add(1);
        self.downstream_bytes_forwarded = self
            .downstream_bytes_forwarded
            .saturating_add(chunk.len() as u64);
        if !self.terminal_event_forwarded && external_stream_chunk_has_terminal_event(chunk) {
            self.terminal_event_forwarded = true;
        }
    }

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
            record_external_stream_end(
                &self.route,
                &self.pool,
                &self.attempts,
                "upstream_status_error",
                Some(&message),
                self.downstream_chunks_forwarded,
                self.downstream_bytes_forwarded,
            );
            // SSE error events arrive with an HTTP 2xx response and therefore
            // bypass the outer request error path. They are still genuine
            // upstream failures and must feed passive pool quality.
            self.manager
                .record_pool_quality_sample(&self.config, self.pool.id, false, None, None);
            account_usage_debug_stream_record(ExternalUsageDebugStreamRecordContext {
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
            account_wire_debug_stream_record(ExternalUsageDebugStreamRecordContext {
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
                    raw_upstream_error: self
                        .usage_capture
                        .as_ref()
                        .and_then(|capture| capture.lock().raw_upstream_error.clone()),
                },
                None,
                None,
                self.upstream_response_model(),
            );
            self.completed = true;
            return;
        }
        let downstream_stop_reason = self
            .usage_capture
            .as_ref()
            .and_then(|capture| capture.lock().downstream_stop_reason.clone());
        let billing = self
            .usage_capture
            .as_ref()
            .and_then(|capture| account_billing_from_capture_ref(&self.route, &self.pool, capture))
            .or_else(|| {
                self.usage_capture.as_ref().and_then(|capture| {
                    account_billing_from_stream_estimate(
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
        account_usage_debug_stream_record(ExternalUsageDebugStreamRecordContext {
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
        account_wire_debug_stream_record(ExternalUsageDebugStreamRecordContext {
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
        record_external_stream_end(
            &self.route,
            &self.pool,
            &self.attempts,
            "completed",
            None,
            self.downstream_chunks_forwarded,
            self.downstream_bytes_forwarded,
        );
        self.manager.record_external_success(
            &self.config,
            &self.route,
            &self.pool,
            self.attempts.clone(),
            billing,
            downstream_stop_reason,
        );
        self.completed = true;
    }

    fn record_stream_error(&mut self, message: &str) {
        if self.completed {
            return;
        }
        record_external_stream_end(
            &self.route,
            &self.pool,
            &self.attempts,
            "internal_error",
            Some(message),
            self.downstream_chunks_forwarded,
            self.downstream_bytes_forwarded,
        );
        // A body/read failure happens after response headers were returned, so
        // the outer dispatch loop cannot record it. Count it here instead.
        self.manager
            .record_pool_quality_sample(&self.config, self.pool.id, false, None, None);
        account_usage_debug_stream_record(ExternalUsageDebugStreamRecordContext {
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
        account_wire_debug_stream_record(ExternalUsageDebugStreamRecordContext {
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
                raw_upstream_error: None,
            },
            None,
            None,
            self.upstream_response_model(),
        );
        self.completed = true;
    }

    fn record_client_dropped(&mut self) {
        if self.completed {
            return;
        }
        let message = "external stream body dropped before completion";
        record_external_stream_end(
            &self.route,
            &self.pool,
            &self.attempts,
            "client_dropped",
            Some(message),
            self.downstream_chunks_forwarded,
            self.downstream_bytes_forwarded,
        );
        account_usage_debug_stream_record(ExternalUsageDebugStreamRecordContext {
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
        account_wire_debug_stream_record(ExternalUsageDebugStreamRecordContext {
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
                raw_upstream_error: None,
            },
            None,
            None,
            self.upstream_response_model(),
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
        // 下游已经收到终止事件后才关连接（例如读到 message_stop 就 Close 的客户端），
        // 这次响应是完整的：按成功落账并保留 usage，只是比上游 EOF 早了一步。
        if !self.completed && self.terminal_event_forwarded {
            self.record_success();
            return;
        }
        self.record_client_dropped();
    }
}

/// 这段下游 chunk 里是否含有流的终止事件。按完整 SSE 事件切分后逐个判断，
/// 识别 Anthropic 的 `message_stop`、带 `stop_reason` 的 `message_delta`，以及
/// OpenAI 风格的 `data: [DONE]` / 非空 `finish_reason`。
fn external_stream_chunk_has_terminal_event(chunk: &Bytes) -> bool {
    let bytes = chunk.as_ref();
    let mut offset = 0usize;
    while let Some((idx, delimiter_len)) = find_sse_event_delimiter(&bytes[offset..]) {
        let event_end = offset + idx + delimiter_len;
        if external_sse_event_is_terminal(&bytes[offset..event_end]) {
            return true;
        }
        offset = event_end;
    }
    false
}

fn external_sse_event_is_terminal(event: &[u8]) -> bool {
    if external_sse_event_name(event).as_deref() == Some("message_stop") {
        return true;
    }
    let Ok(text) = std::str::from_utf8(event) else {
        return false;
    };
    for line in text.split_inclusive('\n') {
        let Some(data) = line.trim_end_matches(['\r', '\n']).strip_prefix("data:") else {
            continue;
        };
        let data_json = data.trim();
        if data_json.is_empty() {
            continue;
        }
        if data_json == "[DONE]" {
            return true;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(data_json) else {
            continue;
        };
        match value.get("type").and_then(|value| value.as_str()) {
            Some("message_stop") => return true,
            Some("message_delta")
                if external_sse_stop_reason_is_set(value.pointer("/delta/stop_reason")) =>
            {
                return true;
            }
            _ => {}
        }
        if value
            .get("choices")
            .and_then(|value| value.as_array())
            .is_some_and(|choices| {
                choices
                    .iter()
                    .any(|choice| external_sse_stop_reason_is_set(choice.get("finish_reason")))
            })
        {
            return true;
        }
    }
    false
}

/// 停止原因只有是非空字符串才算流结束（与 `normalize_external_stop_reason` 同一口径）：
/// 有些 OpenAI 兼容上游每个分片都带 `"finish_reason": ""`，不能把中途断开记成成功。
fn external_sse_stop_reason_is_set(reason: Option<&serde_json::Value>) -> bool {
    reason
        .and_then(serde_json::Value::as_str)
        .is_some_and(|reason| !reason.trim().is_empty())
}

fn pool_auto_disable_policy_enabled(policy: AutoDisablePolicy, config: &AccountsConfig) -> bool {
    match policy {
        AutoDisablePolicy::Disabled => false,
        AutoDisablePolicy::Enabled => true,
        AutoDisablePolicy::Inherit => config.auto_disable_enabled,
    }
}

fn load_score(in_flight: u32, max: u32) -> u64 {
    let max = max.max(1) as u64;
    ((in_flight as u64) * 1_000_000) / max
}

/// 一个参与调度选择的账号候选。
///
/// 携带调度所需的全部运行态信号，使 [`select_account_candidate`] 保持
/// **纯函数**——这是高并发分布测试可行的前提（见测试维度设计文档）。
#[derive(Debug, Clone)]
struct AccountCandidate {
    pool: Account,
    in_flight: u32,
    transient_failure_streak: u32,
    /// 被动质量采样状态；`None` 表示无数据，按中性值处理。
    quality: Option<AccountQualityState>,
}

/// 质量感知评分所需的、跨候选集共享的归一化基准。
///
/// 首字与耗时必须**相对于同批候选的中位数**归一化，而不是用绝对毫秒阈值：
/// 不同账号跑的模型不同，绝对延迟没有可比性——用绝对阈值会系统性地偏袒
/// 跑小模型的账号。同时这也是"全体劣化时不降级任何人"的实现基础。
#[derive(Debug, Clone, Copy, Default)]
struct AccountQualityBaseline {
    median_ttft_ms: Option<f64>,
    median_latency_ms: Option<f64>,
    median_error_rate: f64,
    median_window_error_rate: f64,
}

impl AccountQualityBaseline {
    fn from_candidates(candidates: &[AccountCandidate], min_samples: u64) -> Self {
        fn qualified(
            candidate: &AccountCandidate,
            min_samples: u64,
        ) -> Option<&AccountQualityState> {
            candidate
                .quality
                .as_ref()
                .filter(|quality| quality.has_enough_samples(min_samples))
        }
        Self {
            median_ttft_ms: median_of(
                candidates
                    .iter()
                    .filter_map(|candidate| qualified(candidate, min_samples))
                    .filter_map(|quality| quality.ttft_ewma_ms),
            ),
            median_latency_ms: median_of(
                candidates
                    .iter()
                    .filter_map(|candidate| qualified(candidate, min_samples))
                    .filter_map(|quality| quality.latency_ewma_ms),
            ),
            median_error_rate: median_of(
                candidates
                    .iter()
                    .filter_map(|candidate| qualified(candidate, min_samples))
                    .map(|quality| quality.recent_error_rate),
            )
            .unwrap_or(0.0),
            median_window_error_rate: median_of(
                candidates
                    .iter()
                    .filter_map(|candidate| qualified(candidate, min_samples))
                    .filter(|quality| {
                        quality_degrade_window_sample_count(quality) >= min_samples.max(1)
                    })
                    .map(quality_degrade_window_error_rate),
            )
            .unwrap_or(0.0),
        }
    }
}

fn quality_degrade_window_sample_count(quality: &AccountQualityState) -> u64 {
    if quality.degrade_window_sample_count > 0 {
        quality.degrade_window_sample_count
    } else {
        quality.sample_count
    }
}

fn quality_degrade_window_error_rate(quality: &AccountQualityState) -> f64 {
    if quality.degrade_window_sample_count > 0 {
        (quality.degrade_window_failure_count as f64 / quality.degrade_window_sample_count as f64)
            .clamp(0.0, 1.0)
    } else {
        quality.recent_error_rate.clamp(0.0, 1.0)
    }
}

fn median_of(values: impl Iterator<Item = f64>) -> Option<f64> {
    let mut values: Vec<f64> = values.filter(|value| value.is_finite()).collect();
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let middle = values.len() / 2;
    Some(if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    })
}

/// 相对劣化倍率：`value` 比中位数差多少倍。
///
/// 返回 `0.0` 表示不差于中位数（不罚分）。刻意只惩罚"比同伴差"，
/// 不奖励"比同伴好"——否则全体一起变慢时会有人被无端降权。
fn relative_penalty(value: Option<f64>, median: Option<f64>) -> f64 {
    let (Some(value), Some(median)) = (value, median) else {
        return 0.0;
    };
    if !value.is_finite() || !median.is_finite() || median <= 0.0 {
        return 0.0;
    }
    ((value - median) / median).max(0.0)
}

/// 账号调度评分。**分数越低越优先**，与既有 `scheduler_score_with_config` 一致。
///
/// 权重量级设计（见设计文档 §3）：错误率权重远大于延迟权重，
/// 保证"重试/报错多"的判断优先级**高于**首字等延迟曲线——这是用户的明确要求。
///
/// 注意：调用方已按最优先级层过滤，因此层内所有候选的 `effective_priority`
/// 相同，优先级项在比较中是常数。保留该项是为了让分数可直接用于
/// 可观测性展示（管理端要能看出"这个池因为优先级低所以分高"）。
fn account_quality_score(
    candidate: &AccountCandidate,
    baseline: &AccountQualityBaseline,
    config: &AccountsConfig,
    now_ms: i64,
) -> f64 {
    let transient_failure_penalty = config.transient_failure_priority_penalty as f64;
    let effective_priority = candidate.pool.priority.max(0) as f64
        + candidate.transient_failure_streak as f64 * transient_failure_penalty;
    let load = load_score(candidate.in_flight, candidate.pool.max_concurrent_requests) as f64
        / 1_000_000.0;

    let mut score = effective_priority * config.quality_priority_weight.max(0.0)
        + load * config.quality_load_weight.max(0.0);

    // 样本不足的池按中性值处理：不加分也不减分。冷启动时不能因为"没数据"
    // 就把一个池判成差的，否则新加入的账号永远起不来。
    let Some(quality) = candidate
        .quality
        .as_ref()
        .filter(|quality| quality.has_enough_samples(config.quality_min_samples))
    else {
        return score;
    };

    // 失败率：相对中位数惩罚。全体一起坏时中位数同样高，无人被额外降权，
    // 候选集因此不会被清空——这是"降级时保证可用"的核心机制。
    let relative_error =
        (quality.recent_error_rate.clamp(0.0, 1.0) - baseline.median_error_rate).max(0.0);
    score += relative_error * config.quality_error_weight.max(0.0);

    // 首字与总耗时分别归一化后再加权，两者独立生效。
    let ttft_penalty = relative_penalty(quality.ttft_ewma_ms, baseline.median_ttft_ms);
    let latency_penalty = relative_penalty(quality.latency_ewma_ms, baseline.median_latency_ms);
    score += (ttft_penalty + latency_penalty) * config.quality_latency_weight.max(0.0);

    // 临时避让：加一个大额罚分使其排到末尾，但**不从候选集移除**，
    // 保证探测流量仍能到达、账号有机会自证恢复。
    if quality.is_in_probation(now_ms) {
        score += config.quality_probation_weight.max(0.0);
    } else {
        // 恢复爬坡：避让刚结束时仍保留部分罚分并线性衰减，
        // 避免高优先级账号恢复瞬间被重新打满。
        let ramp_ms = (config.recovery_ramp_secs as i64).saturating_mul(1_000);
        let progress = quality.recovery_progress(now_ms, ramp_ms);
        if progress < 1.0 {
            score += (1.0 - progress) * config.quality_probation_weight.max(0.0);
        }
    }

    score
}

/// 一次质量采样后得出的劣化判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccountDegradeDecision {
    /// 不需要改变避让状态。
    #[allow(dead_code)]
    Keep,
    /// 进入（或延长）临时避让。
    Probation {
        until_ms: i64,
        level: u32,
        ttl_secs: u64,
        reset_level: bool,
    },
}

/// Evaluate degradation relative to the current eligible candidate set.
///
/// The candidate set has already passed pool enabled/route/model admission,
/// cooldown, concurrency and runtime-coordinator checks. A single account is
/// therefore compared only with peers that could actually serve this request.
/// If every eligible account degrades together, the median moves with them and
/// no account is probationed solely because of an absolute red line.
fn evaluate_account_degrade_relative(
    candidate: &AccountCandidate,
    baseline: &AccountQualityBaseline,
    config: &AccountsConfig,
    now_ms: i64,
) -> Option<AccountDegradeDecision> {
    if !config.quality_aware_scheduling_enabled {
        return None;
    }
    let quality = candidate
        .quality
        .as_ref()
        .filter(|quality| quality.has_enough_samples(config.quality_min_samples))?;
    if quality.is_in_probation(now_ms) {
        return None;
    }
    let window_sample_count = quality_degrade_window_sample_count(quality);
    if window_sample_count < config.quality_min_samples.max(1) {
        return None;
    }
    let window_error_rate = quality_degrade_window_error_rate(quality);
    let relative_error_gap = (window_error_rate - baseline.median_window_error_rate).max(0.0);
    let threshold = config.degrade_error_rate_threshold.clamp(0.0, 1.0);
    if relative_error_gap < threshold {
        return None;
    }

    let base_secs = config.degrade_probation_secs.max(1);
    let max_secs = config.max_probation_secs.max(base_secs);
    let ramp_ms = (config.recovery_ramp_secs as i64).saturating_mul(1_000);
    let fully_recovered = quality.probation_level > 0
        && quality.probation_cleared_at_ms.is_some()
        && quality.recovery_progress(now_ms, ramp_ms) >= 1.0;
    let previous_level = if fully_recovered {
        0
    } else {
        quality.probation_level
    };
    let level = previous_level.saturating_add(1);
    let shift = (level - 1).min(32);
    let backoff_secs = base_secs.saturating_mul(1u64 << shift).min(max_secs);
    let until_ms = now_ms.saturating_add((backoff_secs as i64).saturating_mul(1_000));
    let ttl_secs = backoff_secs
        .saturating_add(config.recovery_ramp_secs)
        .max(config.quality_sample_ttl_secs);

    Some(AccountDegradeDecision::Probation {
        until_ms,
        level,
        ttl_secs,
        reset_level: fully_recovered,
    })
}

/// 判定一次采样后是否需要把池临时降级。**纯函数**，时间由入参注入。
///
/// 这里刻意只看单个池的绝对失败率，不看候选集中位数——采样回调发生在请求
/// 结束时，此时拿不到全局视图。安全性由评分层保证：避让在评分中是一个
/// **常数罚分**，全体一起被避让时相对顺序不变、候选集也不会被清空，
/// 因此"上游整体故障"不会退化成"无池可用"。
///
/// 指数退避：连续第 n 次进入避让，时长为 `probation_secs * 2^(n-1)`，
/// 并被 `max_probation_secs` 封顶。恢复爬坡完整结束后层级显式归零；
/// 质量键在持续有流量时会刷新 TTL，不能依赖自然过期。
#[cfg(test)]
fn evaluate_account_degrade(
    quality: &AccountQualityState,
    config: &AccountsConfig,
    now_ms: i64,
) -> AccountDegradeDecision {
    if !config.quality_aware_scheduling_enabled {
        return AccountDegradeDecision::Keep;
    }
    // 样本不足不判定：冷启动时几个失败样本不足以定性一个池。新状态使用
    // 独立的劣化窗口；读取旧格式（没有窗口字段）时回退到总样本数，保持
    // Redis 中已有状态与纯逻辑调用方的向后兼容。
    let window_sample_count = if quality.degrade_window_sample_count > 0 {
        quality.degrade_window_sample_count
    } else {
        quality.sample_count
    };
    if window_sample_count < config.quality_min_samples.max(1) {
        return AccountDegradeDecision::Keep;
    }
    // 已在避让中则不重复延长，否则持续失败会让避让无限续期，
    // 池永远等不到到期后的恢复爬坡窗口。
    if quality.is_in_probation(now_ms) {
        return AccountDegradeDecision::Keep;
    }
    let window_error_rate = if quality.degrade_window_sample_count > 0 {
        (quality.degrade_window_failure_count as f64 / quality.degrade_window_sample_count as f64)
            .clamp(0.0, 1.0)
    } else {
        quality.recent_error_rate
    };
    let threshold = config.degrade_error_rate_threshold.clamp(0.0, 1.0);
    if window_error_rate < threshold {
        return AccountDegradeDecision::Keep;
    }

    // 质量键在持续有流量时会被每次采样刷新 TTL，因此不能只依赖
    // Redis 自然过期来清零避让层级。恢复爬坡完整结束后，下一轮独立
    // 劣化应从第一层重新开始，而不是继承很久以前的指数退避。
    let ramp_ms = (config.recovery_ramp_secs as i64).saturating_mul(1_000);
    let fully_recovered = quality.probation_level > 0
        && quality.probation_cleared_at_ms.is_some()
        && quality.recovery_progress(now_ms, ramp_ms) >= 1.0;
    let previous_level = if fully_recovered {
        0
    } else {
        quality.probation_level
    };
    let level = previous_level.saturating_add(1);
    let base_secs = config.degrade_probation_secs.max(1);
    let max_secs = config.max_probation_secs.max(base_secs);
    // 指数退避，用移位前先夹紧指数避免溢出。
    let shift = (level - 1).min(32);
    let backoff_secs = base_secs.saturating_mul(1u64 << shift).min(max_secs);
    let until_ms = now_ms.saturating_add((backoff_secs as i64).saturating_mul(1_000));
    // 状态 TTL 必须覆盖整个避让窗口加恢复爬坡，否则避让会随状态过期被抹掉。
    let ttl_secs = backoff_secs
        .saturating_add(config.recovery_ramp_secs)
        .max(config.quality_sample_ttl_secs);

    AccountDegradeDecision::Probation {
        until_ms,
        level,
        ttl_secs,
        reset_level: fully_recovered,
    }
}

/// 旧版（纯优先级 + 负载）选择逻辑。
///
/// 主开关关闭时**必须**走这里，保证与变更前行为完全等价。
fn select_account_candidate_legacy(
    mut candidates: Vec<AccountCandidate>,
    config: &AccountsConfig,
) -> Option<Account> {
    let transient_failure_penalty = config.transient_failure_priority_penalty as u64;
    let effective_priority_of = |candidate: &AccountCandidate| -> u64 {
        candidate.pool.priority.max(0) as u64
            + (candidate.transient_failure_streak as u64).saturating_mul(transient_failure_penalty)
    };
    candidates.sort_by(|a, b| {
        let a_load = load_score(a.in_flight, a.pool.max_concurrent_requests);
        let b_load = load_score(b.in_flight, b.pool.max_concurrent_requests);
        effective_priority_of(a)
            .cmp(&effective_priority_of(b))
            .then_with(|| a_load.cmp(&b_load))
            .then_with(|| a.pool.id.cmp(&b.pool.id))
    });
    let (best_priority, best_load) = candidates.first().map(|candidate| {
        (
            effective_priority_of(candidate),
            load_score(candidate.in_flight, candidate.pool.max_concurrent_requests),
        )
    })?;
    let best: Vec<_> = candidates
        .into_iter()
        .filter(|candidate| {
            effective_priority_of(candidate) == best_priority
                && load_score(candidate.in_flight, candidate.pool.max_concurrent_requests)
                    == best_load
        })
        .collect();
    if best.is_empty() {
        return None;
    }
    let idx = fastrand::usize(..best.len());
    best.into_iter().nth(idx).map(|candidate| candidate.pool)
}

fn select_account_candidate(
    candidates: Vec<AccountCandidate>,
    config: &AccountsConfig,
    now_ms: i64,
) -> Option<Account> {
    if !config.quality_aware_scheduling_enabled {
        return select_account_candidate_legacy(candidates, config);
    }
    if candidates.is_empty() {
        return None;
    }
    // Quality is passively learned from real requests. During cold start, or
    // after the sample TTL has expired for every candidate, there is no
    // meaningful peer baseline yet. Preserve the pre-quality selector exactly
    // instead of letting Top-K weighted randomness change load balancing.
    let qualified_candidates = candidates
        .iter()
        .filter(|candidate| {
            candidate
                .quality
                .as_ref()
                .is_some_and(|quality| quality.has_enough_samples(config.quality_min_samples))
        })
        .count();
    if qualified_candidates == 0 {
        return select_account_candidate_legacy(candidates, config);
    }

    // 优先级是**硬分层**，不是可被质量分抵消的一项权重。
    // 先取出最优先级层，质量评分只在层内重排——否则 Top-K 加权随机会把
    // 低优先级池提拔到高优先级池之上，破坏用户配置优先级的原意。
    let transient_failure_penalty = config.transient_failure_priority_penalty as u64;
    let effective_priority_of = |candidate: &AccountCandidate| -> u64 {
        candidate.pool.priority.max(0) as u64
            + (candidate.transient_failure_streak as u64).saturating_mul(transient_failure_penalty)
    };
    let best_priority = candidates.iter().map(effective_priority_of).min()?;

    // 探测候选必须在**分层过滤之前**取。
    //
    // 被降级的池通常同时背着既有的瞬态失败连击（`transient_failure_streak`），
    // 而连击会抬高有效优先级、把它挤出最优层——于是它在分层后根本不在候选里，
    // 层内的探测抽样永远抽不到它。这两套降权机制是独立的：
    // 探测流量要救的恰恰是"两者都踩中"的池，否则它拿不到任何样本自证恢复。
    //
    // 只有**真实处于避让期**的池才有探测资格：避让是质量系统主动打的标记，
    // 代表"我降了它，但我想要它回来"；仅有连击而无避让的池不在此列，
    // 那是既有逻辑的判断，不该被这里放行。
    // 关键区分：**用户配置的**低优先级 vs **被失败连击降下去的**低优先级。
    // 前者是用户的明确意图（备用池就该在主池可用时闲着），绝不放行探测流量；
    // 后者是系统自己打的惩罚，正是探测要解救的对象。
    // 判据：把连击惩罚去掉后，它本来是否属于最优层。
    let base_priority_of =
        |candidate: &AccountCandidate| -> u64 { candidate.pool.priority.max(0) as u64 };
    let best_base_priority = candidates.iter().map(base_priority_of).min()?;

    let probe_candidates: Vec<AccountCandidate> = candidates
        .iter()
        .filter(|candidate| {
            effective_priority_of(candidate) != best_priority
                && base_priority_of(candidate) == best_base_priority
                && candidate
                    .quality
                    .as_ref()
                    .is_some_and(|quality| quality.is_in_probation(now_ms))
        })
        .cloned()
        .collect();

    let candidates: Vec<AccountCandidate> = candidates
        .into_iter()
        .filter(|candidate| effective_priority_of(candidate) == best_priority)
        .collect();
    // 质量只在最终的最优有效优先级层内比较。低优先级备用池不能改变
    // 主层候选的中位数基准，否则会稀释或扭曲同层内质量排序。
    let baseline = AccountQualityBaseline::from_candidates(&candidates, config.quality_min_samples);

    let mut scored: Vec<(AccountCandidate, f64)> = candidates
        .into_iter()
        .map(|candidate| {
            let score = account_quality_score(&candidate, &baseline, config, now_ms);
            (candidate, score)
        })
        .collect();
    // 分数升序；同分时按 id 排序保证确定性。
    scored.sort_by(|(a, a_score), (b, b_score)| {
        a_score
            .partial_cmp(b_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.pool.id.cmp(&b.pool.id))
    });

    // 避让期探测流量：这是"不是真冷却，只是临时降低优先级"的落地。
    //
    // 仅靠罚分不够——被避让的池会排到 Top-K 之外拿到**零**流量，
    // 于是永远产生不了新样本来自证恢复，临时避让会变成永久放逐。
    // 因此按 `probe_share_percent` 的概率，直接从避让池中抽一个放行。
    let probe_share = f64::from(config.probe_share_percent.min(100)) / 100.0;
    if probe_share > 0.0 {
        // 层内的避让池 + 被连击挤出最优层的避让池，共同构成探测候选。
        let mut probationary: Vec<&AccountCandidate> = scored
            .iter()
            .map(|(candidate, _)| candidate)
            .filter(|candidate| {
                candidate
                    .quality
                    .as_ref()
                    .is_some_and(|quality| quality.is_in_probation(now_ms))
            })
            .collect();
        let in_tier_probationary = probationary.len();
        probationary.extend(probe_candidates.iter());

        // 全员避让时不走探测分支：此时避让罚分对所有池是常数，
        // 正常评分路径本身就是公平的，再抽一次反而丢掉了质量信号。
        // 注意只在"层内全是避让池且没有层外探测候选"时才算全员避让。
        let everyone_in_tier_is_probationary =
            in_tier_probationary == scored.len() && probe_candidates.is_empty();

        if !probationary.is_empty()
            && !everyone_in_tier_is_probationary
            && fastrand::f64() < probe_share
        {
            let picked = fastrand::usize(..probationary.len());
            return Some(probationary[picked].pool.clone());
        }
    }

    // Top-K 加权随机：只在最优的 K 个里按分差加权抽取。
    // 纯取最优会让所有实例同时扑向同一个池（羊群效应），
    // 而完全随机又会浪费质量信号——Top-K 是两者的平衡。
    let top_k = (config.quality_top_k.max(1) as usize).min(scored.len());
    let top = &scored[..top_k];
    let worst_score = top.last()?.1;
    let weight_of = |score: f64| (worst_score - score + 1.0).max(0.01);
    let total_weight: f64 = top.iter().map(|(_, score)| weight_of(*score)).sum();
    if !total_weight.is_finite() || total_weight <= 0.0 {
        return top.first().map(|(candidate, _)| candidate.pool.clone());
    }
    let mut roll = fastrand::f64() * total_weight;
    for (candidate, score) in top {
        roll -= weight_of(*score);
        if roll <= 0.0 {
            return Some(candidate.pool.clone());
        }
    }
    top.last().map(|(candidate, _)| candidate.pool.clone())
}

/// 调度使用的当前时间（epoch 毫秒）。
fn account_now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

async fn account_lease_lost(lease: Option<&AccountLease>) {
    match lease {
        Some(lease) => lease.wait_until_lost().await,
        None => std::future::pending().await,
    }
}

async fn account_stream_idle_deadline(last_chunk_at: Instant, idle_timeout: Option<Duration>) {
    if let Some(idle_timeout) = idle_timeout {
        tokio::time::sleep_until(last_chunk_at + idle_timeout).await;
    } else {
        std::future::pending::<()>().await;
    }
}

async fn account_stream_pre_output_hold_deadline(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending::<()>().await,
    }
}

async fn account_stream_keepalive_deadline(
    last_keepalive_at: Instant,
    keepalive_interval: Duration,
) {
    tokio::time::sleep_until(last_keepalive_at + keepalive_interval).await;
}

fn account_stream_keepalive_interval(idle_timeout: Option<Duration>) -> Duration {
    match idle_timeout {
        Some(idle_timeout) => {
            let secs =
                (idle_timeout.as_secs() / 2).clamp(1, ACCOUNT_STREAM_KEEPALIVE_INTERVAL_SECS);
            Duration::from_secs(secs)
        }
        None => Duration::from_secs(ACCOUNT_STREAM_KEEPALIVE_INTERVAL_SECS),
    }
}

fn effective_account_pre_output_stream_retry_enabled(
    pool: &Account,
    config: &AccountsConfig,
) -> bool {
    match pool.pre_output_stream_retry_mode {
        StreamRetryMode::Enabled | StreamRetryMode::EarlyCommit => true,
        StreamRetryMode::Disabled => false,
        StreamRetryMode::Inherit => config.stream_pre_output_retry_enabled,
    }
}

/// 只有流式请求、且本次请求所有可选池都设置了“提前提交首包”时才启用。
/// 混合时不启用：普通池的首输出前语义（缓冲、首个输出超时）保持不变。
fn route_prefers_early_commit(
    pools: &[Account],
    route: &AccountRouteRequest,
    config: &AccountsConfig,
) -> bool {
    let support_candidates = normalize_account_support_candidates(
        route.model_candidates_for_support().into_iter().flatten(),
    );
    let eligible = pools
        .iter()
        .filter(|pool| {
            pool.enabled
                && !pool.is_auto_disabled_now()
                && route_account_allowed(
                    config,
                    pool.id,
                    Some(&route.endpoint),
                    Some(&support_candidates),
                )
                && account_route_allowed(pool, Some(&route.endpoint))
                && account_matches_supported_models_normalized(pool, Some(&support_candidates))
        })
        .collect::<Vec<_>>();
    !eligible.is_empty()
        && eligible
            .iter()
            .all(|pool| account_stream_early_commit_enabled(pool))
}

fn account_stream_early_commit_enabled(pool: &Account) -> bool {
    pool.pre_output_stream_retry_mode == StreamRetryMode::EarlyCommit
}

fn external_pre_output_stream_protocol_error(
    message: impl Into<String>,
    protocol_error: &'static str,
    config: &AccountsConfig,
    outbound_model: Option<String>,
) -> ExternalForwardError {
    ExternalForwardError::new(
        AccountError {
            status: Some(StatusCode::OK),
            message: message.into(),
            retryable: true,
            auto_disable_reason: None,
            cooldown: Some((
                Duration::from_secs(config.protocol_error_cooldown_secs.max(1)),
                "protocol_error".to_string(),
            )),
            protocol_error: Some(protocol_error),
            raw_upstream_error: None,
        },
        outbound_model,
    )
}

/// Whether a pool failure happened on a stream before any downstream output: a stream error
/// event, read error, EOF, idle or first-output timeout before semantic output.
fn external_error_is_pre_output_stream_failure(err: &AccountError) -> bool {
    err.protocol_error
        .is_some_and(|reason| reason.starts_with("stream_pre_output_"))
        || err.message.contains("before semantic output")
        || err.message.contains("produced no semantic output")
        || err.message.contains("stream pre-output read failed")
}

fn external_pre_output_stream_network_error(
    message: impl Into<String>,
    config: &AccountsConfig,
    outbound_model: Option<String>,
) -> ExternalForwardError {
    ExternalForwardError::new(
        AccountError {
            status: None,
            message: message.into(),
            retryable: true,
            auto_disable_reason: None,
            cooldown: Some((
                Duration::from_secs(config.network_error_cooldown_secs.max(1)),
                "network_error".to_string(),
            )),
            protocol_error: None,
            raw_upstream_error: None,
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

fn record_external_upstream_chunk(
    request_id: Option<&str>,
    pool_id: u64,
    index: Option<&AtomicU64>,
    chunk: &Bytes,
) {
    if !crate::diagnostics::capture::is_active() {
        return;
    }
    let (Some(request_id), Some(index)) = (request_id, index) else {
        return;
    };
    let index = index.fetch_add(1, Ordering::Relaxed) + 1;
    crate::diagnostics::capture::record_upstream_frame(
        Some(request_id),
        Some(pool_id),
        index,
        chunk,
    );
    crate::diagnostics::capture::record("external_upstream_chunk", Some(request_id), || {
        match std::str::from_utf8(chunk) {
            Ok(text) => json!({
                "poolId": pool_id,
                "index": index,
                "text": text,
            }),
            Err(_) => json!({
                "poolId": pool_id,
                "index": index,
                "base64": crate::diagnostics::capture::base64_encode(chunk),
                "bytes": chunk.len(),
            }),
        }
    });
}

#[allow(clippy::too_many_arguments)]
async fn pre_read_external_stream_before_commit<S>(
    body_stream: &mut S,
    buffer: &mut Vec<u8>,
    prefix: &mut Vec<u8>,
    lease: &AccountLease,
    route: &AccountRouteRequest,
    pool: &Account,
    config: &AccountsConfig,
    projection_context: Option<&ExternalUsageProjectionContext>,
    usage_capture: &Arc<SyncMutex<ExternalUsageCapture>>,
    stream_plan: ExternalStreamProcessingPlan,
    stream_error_mask: &ExternalStreamErrorMask,
    transcript_state: &Arc<SyncMutex<Option<ExternalAnthropicTranscriptState>>>,
    stream_idle_timeout: Option<Duration>,
    outbound_model: &Option<String>,
    upstream_chunk_index: Option<&AtomicU64>,
) -> Result<Instant, ExternalForwardError>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Unpin,
{
    let mut last_chunk_at = Instant::now();
    let hold_deadline = route
        .stream_pre_output_hold
        .map(|hold| route.started_at + hold);
    // With a first-output timeout the stream stays uncommitted until semantic output, so a pool
    // that only sends message_start/ping and then stalls is still replaced by another pool or
    // the local rescue path. Without it the hold window commits the buffered prefix and the
    // request can no longer fail over (production 2026-10-21: 451 external requests ended with
    // no output after 144–261 seconds).
    // Measured per attempt: a failover pool gets its own full window.
    let attempt_started_at = Instant::now();
    let first_output_deadline = (config.stream_first_output_timeout_secs > 0)
        .then(|| attempt_started_at + Duration::from_secs(config.stream_first_output_timeout_secs));
    let hold_deadline = hold_deadline.filter(|_| first_output_deadline.is_none());
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
                    "account stream emitted error event before downstream semantic output; retrying within account budget"
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
                transcript_state.lock().as_mut(),
                route.client_model.as_deref(),
            ));
            if prefix.len() > ACCOUNT_MAX_SSE_EVENT_BUFFER_BYTES {
                tracing::warn!(
                    request_id = %route.request_id,
                    error_id = %route.error_id,
                    pool_id = pool.id,
                    pool_name = %pool.name,
                    buffered_bytes = prefix.len(),
                    max_buffered_bytes = ACCOUNT_MAX_SSE_EVENT_BUFFER_BYTES,
                    "account pre-output stream prefix exceeded limit before commit"
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

        if buffer.len() > ACCOUNT_MAX_SSE_EVENT_BUFFER_BYTES {
            tracing::warn!(
                request_id = %route.request_id,
                error_id = %route.error_id,
                pool_id = pool.id,
                pool_name = %pool.name,
                buffered_bytes = buffer.len(),
                max_buffered_bytes = ACCOUNT_MAX_SSE_EVENT_BUFFER_BYTES,
                "account pre-output stream event buffer exceeded limit"
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
                        record_external_upstream_chunk(
                            Some(&route.request_id),
                            pool.id,
                            upstream_chunk_index,
                            &chunk,
                        );
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
                            "account stream read failed before downstream semantic output"
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
                            "account stream ended before downstream semantic output or terminal event"
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
                return Err(account_lease_lost_forward_error(outbound_model.clone()));
            }
            // 与本地路径一致：保留窗口到期后不再静默等待首个语义输出。已缓冲的
            // message_start 先发给客户端，之后由提交后的流按间隔发送 ping，代价是放弃
            // 之后的首输出前换池。还没收到任何事件时没有可提交的内容，继续等待。
            _ = account_stream_pre_output_hold_deadline(hold_deadline), if !prefix.is_empty() => {
                tracing::info!(
                    request_id = %route.request_id,
                    pool_id = pool.id,
                    pool_name = %pool.name,
                    "account pre-output hold window elapsed; committing buffered events and starting keepalive"
                );
                return Ok(last_chunk_at);
            }
            _ = account_stream_pre_output_hold_deadline(first_output_deadline) => {
                let seconds = config.stream_first_output_timeout_secs;
                tracing::warn!(
                    request_id = %route.request_id,
                    error_id = %route.error_id,
                    pool_id = pool.id,
                    pool_name = %pool.name,
                    first_output_timeout_secs = seconds,
                    buffered_prefix_bytes = prefix.len(),
                    "account stream produced no semantic output before the first-output timeout; failing over"
                );
                return Err(external_pre_output_stream_network_error(
                    format!(
                        "model endpoint produced no semantic output within {} seconds",
                        seconds
                    ),
                    config,
                    outbound_model.clone(),
                ));
            }
            _ = account_stream_idle_deadline(last_chunk_at, stream_idle_timeout) => {
                let seconds = stream_idle_timeout
                    .map(|timeout| timeout.as_secs())
                    .unwrap_or_default();
                tracing::warn!(
                    request_id = %route.request_id,
                    error_id = %route.error_id,
                    pool_id = pool.id,
                    pool_name = %pool.name,
                    idle_timeout_secs = seconds,
                    "account stream idled before downstream semantic output"
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

pub(crate) fn account_models_url(base_url: &str) -> Result<Url, url::ParseError> {
    let mut base = base_url.trim().trim_end_matches('/').to_string();
    if base_url_ends_with_v1(&base) {
        base.push_str("/models");
    } else {
        base.push_str("/v1/models");
    }
    Url::parse(&base)
}

pub(crate) fn account_messages_url(base_url: &str) -> Result<Url, url::ParseError> {
    let mut base = base_url.trim().trim_end_matches('/').to_string();
    if base_url_ends_with_v1(&base) {
        base.push_str("/messages");
    } else {
        base.push_str("/v1/messages");
    }
    Url::parse(&base)
}

#[allow(clippy::result_large_err)]
fn account_url(
    pool: &Account,
    _endpoint: &str,
    config: &AccountsConfig,
) -> Result<Url, AccountError> {
    let mut url = account_messages_url(&pool.base_url).map_err(|err| AccountError {
        status: None,
        message: format!("model endpoint URL is invalid: {}", err),
        retryable: true,
        auto_disable_reason: Some("misconfigured_endpoint".to_string()),
        cooldown: Some((
            Duration::from_secs(config.protocol_error_cooldown_secs.max(1)),
            "misconfigured_endpoint".to_string(),
        )),
        protocol_error: None,
        raw_upstream_error: None,
    })?;
    if pool.append_beta_query || pool.header_profile.uses_anthropic_header_policy() {
        append_or_replace_query_pair(&mut url, "beta", "true");
    }
    Ok(url)
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

fn append_or_replace_query_pair(url: &mut Url, name: &str, value: &str) {
    let pairs = url
        .query_pairs()
        .filter(|(key, _)| key != name)
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    url.set_query(None);
    {
        let mut query = url.query_pairs_mut();
        for (key, value) in pairs {
            query.append_pair(&key, &value);
        }
        query.append_pair(name, value);
    }
}

/// 管理端探测（模型发现、账号测试）用的上游请求头：和真实转发一样按账号的
/// `header_profile` / `auth_type` / `header_overrides` 构造，只是没有下游请求头可继承。
pub(crate) fn account_probe_headers(pool: &Account) -> anyhow::Result<HeaderMap> {
    let mut headers = forward_headers(&HeaderMap::new(), pool, false)
        .map_err(|err| anyhow::anyhow!("{}", err.message))?;
    if !headers.contains_key(header::ACCEPT) {
        headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
    }
    if !headers.contains_key(header::CONTENT_TYPE) {
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
    }
    Ok(headers)
}

#[allow(clippy::result_large_err)]
fn forward_headers(
    headers: &HeaderMap,
    pool: &Account,
    request_is_stream: bool,
) -> Result<HeaderMap, AccountError> {
    let mut out = match pool.header_profile {
        AccountHeaderProfile::Generic => forward_generic_headers(headers),
        AccountHeaderProfile::AnthropicPassthrough => {
            forward_anthropic_passthrough_headers(headers)
        }
        AccountHeaderProfile::ClaudeCodeMimic => claude_code_mimic_headers(request_is_stream),
    };
    if !out.contains_key(HeaderName::from_static("anthropic-version")) {
        out.insert(
            HeaderName::from_static("anthropic-version"),
            HeaderValue::from_static("2023-06-01"),
        );
    }
    match pool.auth_type {
        AccountAuthType::Bearer => {
            let key = pool.api_key.as_deref().unwrap_or_default();
            let value =
                HeaderValue::from_str(&format!("Bearer {}", key)).map_err(|err| AccountError {
                    status: None,
                    message: format!("model endpoint auth header invalid: {}", err),
                    retryable: true,
                    auto_disable_reason: Some("auth_error".to_string()),
                    cooldown: Some((Duration::from_secs(10), "auth_error".to_string())),
                    protocol_error: None,
                    raw_upstream_error: None,
                })?;
            out.insert(header::AUTHORIZATION, value);
        }
        AccountAuthType::XApiKey => {
            let key = pool.api_key.as_deref().unwrap_or_default();
            let value = HeaderValue::from_str(key).map_err(|err| AccountError {
                status: None,
                message: format!("model endpoint x-api-key invalid: {}", err),
                retryable: true,
                auto_disable_reason: Some("auth_error".to_string()),
                cooldown: Some((Duration::from_secs(10), "auth_error".to_string())),
                protocol_error: None,
                raw_upstream_error: None,
            })?;
            out.insert(HeaderName::from_static("x-api-key"), value);
        }
    }
    apply_account_header_overrides(&mut out, &pool.header_overrides);
    Ok(out)
}

fn forward_generic_headers(headers: &HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (name, value) in headers {
        if should_forward_header(name) {
            out.insert(name.clone(), value.clone());
        }
    }
    out
}

fn forward_anthropic_passthrough_headers(headers: &HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (name, value) in headers {
        if should_forward_anthropic_header(name) {
            out.insert(resolve_anthropic_wire_header_name(name), value.clone());
        }
    }
    out
}

fn claude_code_mimic_headers(request_is_stream: bool) -> HeaderMap {
    let mut out = HeaderMap::new();
    for &(name, value) in CLAUDE_CODE_MIMIC_HEADERS {
        out.insert(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        );
    }
    out.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
    if request_is_stream {
        out.insert(
            HeaderName::from_static("x-stainless-helper-method"),
            HeaderValue::from_static("stream"),
        );
    }
    out.insert(
        HeaderName::from_static("x-client-request-id"),
        HeaderValue::from_str(&uuid::Uuid::new_v4().to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("00000000-0000-4000-8000-000000000000")),
    );
    out.insert(
        HeaderName::from_static("anthropic-beta"),
        HeaderValue::from_static(CLAUDE_CODE_MIMIC_BETA_HEADER),
    );
    out
}

const CLAUDE_CODE_MIMIC_BETA_HEADER: &str = "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,prompt-caching-scope-2026-01-05,effort-2025-11-24,context-management-2025-06-27,extended-cache-ttl-2025-04-11";
const CLAUDE_CODE_MIMIC_HEADERS: &[(&str, &str)] = &[
    ("user-agent", "claude-cli/2.1.161 (external, cli)"),
    ("x-stainless-lang", "js"),
    ("x-stainless-package-version", "0.94.0"),
    ("x-stainless-os", "Linux"),
    ("x-stainless-arch", "arm64"),
    ("x-stainless-runtime", "node"),
    ("x-stainless-runtime-version", "v24.3.0"),
    ("x-stainless-retry-count", "0"),
    ("x-stainless-timeout", "600"),
    ("x-app", "cli"),
    ("anthropic-dangerous-direct-browser-access", "true"),
];

#[cfg(test)]
#[allow(clippy::result_large_err)]
fn account_outbound_body(
    route: &AccountRouteRequest,
    pool: &Account,
) -> Result<Bytes, AccountError> {
    account_prepare_request(route, pool).map(|prepared| prepared.body)
}

fn normalize_account_support_candidates<'a>(
    candidates: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    normalize_account_models(candidates, false)
}

fn normalize_account_allowlist_models<'a>(
    models: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    normalize_account_models(models, true)
}

fn normalize_account_models<'a>(
    models: impl IntoIterator<Item = &'a str>,
    add_family_alias: bool,
) -> Vec<String> {
    let mut normalized = Vec::new();
    for model in models {
        let Some(model) = normalize_routing_model_id(model) else {
            continue;
        };
        let variants = if add_family_alias {
            account_model_allowlist_variants(&model)
        } else {
            account_model_request_variants(&model)
        };
        for variant in variants {
            if !normalized.iter().any(|existing| existing == &variant) {
                normalized.push(variant);
            }
        }
    }
    normalized
}

/// Account allowlists are expressed in Claude Code model names, while a
/// resolved route may carry a 上游 model id. Keep this expansion local to
/// account matching so the local credential model-support contract stays
/// exact-only.
fn account_model_request_variants(model: &str) -> Vec<String> {
    account_model_variants(model, false)
}

fn account_model_allowlist_variants(model: &str) -> Vec<String> {
    account_model_variants(model, true)
}

fn account_model_variants(model: &str, add_family_alias: bool) -> Vec<String> {
    let (base, thinking, one_m) = split_account_model_suffix(model);
    let mut variants = vec![model.to_string()];

    let mut add_base_variants = |value: &str| {
        let value = value.trim();
        if value.is_empty() {
            return;
        }
        let official_model = official_claude_model_reference(value);
        let seeds = if let Some(official_model) = official_model.as_deref() {
            expand_claude_supported_model_variants(vec![official_model.to_string()])
        } else {
            Vec::new()
        };
        if seeds.is_empty() {
            push_account_model_variant(
                &mut variants,
                apply_account_model_suffix(value, thinking, one_m),
            );
        } else {
            for seed in seeds {
                push_account_model_variant(
                    &mut variants,
                    apply_account_model_suffix(&seed, thinking, one_m),
                );
                if let Some(short) = seed.strip_prefix("claude-") {
                    push_account_model_variant(
                        &mut variants,
                        apply_account_model_suffix(short, thinking, one_m),
                    );
                }
            }
        }

        // These are compatibility identifiers, not concrete version choices.
        // They let an allowlist expressed as a concrete Claude family accept
        // a Claude Code alias without changing the outbound model id.
        if add_family_alias
            && let Some(family) = official_claude_family(value)
            && is_account_official_claude_model(value)
        {
            push_account_model_variant(
                &mut variants,
                apply_account_model_suffix(family, thinking, one_m),
            );
            let without_claude = value.strip_prefix("claude-").unwrap_or(value);
            push_account_model_variant(
                &mut variants,
                apply_account_model_suffix(without_claude, thinking, one_m),
            );
        }
    };

    // Accounts use their own Claude Code mapping rules. This helper only
    // expands protocol spellings (dot/dash, dated/base, thinking and [1m]);
    // it must never select a local 上游 model version.
    add_base_variants(&base);

    variants
}

/// Returns true only for the Claude model shapes that are safe to family-map.
///
/// External-pool allowlists may contain arbitrary provider-specific names. A
/// substring such as `tenant-haiku-model` must therefore remain exact-only;
/// accepting every `claude-*` value here would make a custom name match a
/// different model family through `map_model`.
fn is_account_official_claude_model(model: &str) -> bool {
    let Some(rest) = model.strip_prefix("claude-") else {
        return false;
    };
    let parts = rest.split('-').collect::<Vec<_>>();
    let is_family = |value: &str| matches!(value, "opus" | "sonnet" | "haiku");
    let all_digits = |value: &str| !value.is_empty() && value.chars().all(|ch| ch.is_ascii_digit());
    let valid_version = |value: &str| {
        if let Some((major, minor)) = value.split_once('.') {
            all_digits(major) && all_digits(minor)
        } else {
            all_digits(value)
        }
    };
    let valid_date = |value: &str| is_yyyymmdd(value);

    match parts.as_slice() {
        ["3.5", family] => is_family(family),
        ["3", "5", family] => is_family(family),
        ["3", "5", family, date] => is_family(family) && valid_date(date),
        [family, version] => is_family(family) && valid_version(version),
        [family, major, third] => {
            is_family(family) && all_digits(major) && (all_digits(third) || valid_date(third))
        }
        [family, major, minor, date] => {
            is_family(family) && all_digits(major) && all_digits(minor) && valid_date(date)
        }
        _ => false,
    }
}

fn official_claude_family(model: &str) -> Option<&'static str> {
    let normalized = model.trim().to_ascii_lowercase();
    if !matches!(
        normalized.as_str(),
        "opus" | "opusplan" | "sonnet" | "haiku"
    ) && !is_account_official_claude_model(&normalized)
    {
        return None;
    }
    match normalized.as_str() {
        "opus" | "opusplan" => Some("opus"),
        "sonnet" => Some("sonnet"),
        "haiku" => Some("haiku"),
        _ if normalized.starts_with("claude-opus-") => Some("opus"),
        _ if normalized.starts_with("claude-sonnet-") => Some("sonnet"),
        _ if normalized.starts_with("claude-haiku-") => Some("haiku"),
        _ => None,
    }
}

/// Return a canonical `claude-*` spelling for an official Claude Code model
/// reference, including the short family forms used by pool allowlists.
fn official_claude_model_reference(model: &str) -> Option<String> {
    let normalized = model.trim().to_ascii_lowercase();
    if matches!(
        normalized.as_str(),
        "opus" | "opusplan" | "sonnet" | "haiku"
    ) {
        return Some(format!("claude-{normalized}"));
    }
    if is_account_official_claude_model(&normalized) {
        return Some(normalized);
    }
    let short = normalized
        .strip_prefix("opus-")
        .or_else(|| normalized.strip_prefix("sonnet-"))
        .or_else(|| normalized.strip_prefix("haiku-"))?;
    let family = normalized.split('-').next()?;
    let candidate = format!("claude-{family}-{short}");
    is_account_official_claude_model(&candidate).then_some(candidate)
}

fn is_yyyymmdd(value: &str) -> bool {
    value.len() == 8 && value.chars().all(|ch| ch.is_ascii_digit())
}

fn split_account_model_suffix(model: &str) -> (String, bool, bool) {
    let mut base = model;
    let one_m = base.strip_suffix("[1m]").is_some();
    if one_m {
        base = base.strip_suffix("[1m]").unwrap_or(base);
    }
    let thinking = base.strip_suffix("-thinking").is_some();
    if thinking {
        base = base.strip_suffix("-thinking").unwrap_or(base);
    }
    (base.to_string(), thinking, one_m)
}

fn apply_account_model_suffix(base: &str, thinking: bool, one_m: bool) -> String {
    let mut model = base.to_string();
    if thinking {
        model.push_str("-thinking");
    }
    if one_m {
        model.push_str("[1m]");
    }
    model
}

fn push_account_model_variant(variants: &mut Vec<String>, model: String) {
    if model.is_empty() || variants.iter().any(|existing| existing == &model) {
        return;
    }
    variants.push(model);
}

fn account_matches_supported_models_normalized(
    pool: &Account,
    model_candidates: Option<&[String]>,
) -> bool {
    supported_models_match(&pool.supported_models, model_candidates)
}

/// 调度每次扫描都要问每个账号"是否支持本次请求的模型"，而账号的 `supportedModels` 列表通常
/// 几百个账号共用同一份；每次都把列表展开成匹配变体会在调度锁下产生成百上千次字符串分配
/// （main 分支现网 850 账号时占到进程 CPU 的 54%）。展开结果只取决于列表内容，这里按内容
/// 缓存在线程局部：命中时比较存储的列表与传入列表，列表变化永远不会用到过期展开。
fn supported_models_match(
    supported_models: &[String],
    model_candidates: Option<&[String]>,
) -> bool {
    if supported_models.is_empty() {
        return true;
    }
    let Some(model_candidates) = model_candidates else {
        return false;
    };
    SUPPORTED_MODELS_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let expanded = cache.expanded(supported_models);
        model_candidates
            .iter()
            .any(|candidate| expanded.variants.contains(candidate))
    })
}

thread_local! {
    static SUPPORTED_MODELS_CACHE: std::cell::RefCell<SupportedModelsCache> =
        std::cell::RefCell::new(SupportedModelsCache::default());
}

/// 每线程最多缓存多少份不同的 `supportedModels` 列表；超过就整体重建（列表种类通常个位数）。
const SUPPORTED_MODELS_CACHE_MAX_LISTS: usize = 128;

#[derive(Default)]
struct SupportedModelsCache {
    /// 上一次命中的列表：连续扫描的账号大多共用同一份，先比它省去哈希。
    last: Option<std::rc::Rc<ExpandedSupportedModels>>,
    lists: HashMap<u64, Vec<std::rc::Rc<ExpandedSupportedModels>>>,
}

struct ExpandedSupportedModels {
    models: Vec<String>,
    variants: HashSet<String>,
}

impl SupportedModelsCache {
    fn expanded(&mut self, models: &[String]) -> std::rc::Rc<ExpandedSupportedModels> {
        if let Some(last) = self.last.as_ref().filter(|last| last.models == models) {
            return last.clone();
        }
        let key = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            models.hash(&mut hasher);
            hasher.finish()
        };
        let cached = self
            .lists
            .get(&key)
            .and_then(|bucket| bucket.iter().find(|list| list.models == models))
            .cloned();
        let list = cached.unwrap_or_else(|| {
            if self.lists.len() >= SUPPORTED_MODELS_CACHE_MAX_LISTS {
                self.lists.clear();
            }
            let list = std::rc::Rc::new(ExpandedSupportedModels {
                models: models.to_vec(),
                variants: normalize_account_allowlist_models(models.iter().map(String::as_str))
                    .into_iter()
                    .collect(),
            });
            self.lists.entry(key).or_default().push(list.clone());
            list
        });
        self.last = Some(list.clone());
        list
    }
}

fn account_route_policy_allows(
    mode: RouteRuleMode,
    rules: &[String],
    endpoint: Option<&str>,
) -> bool {
    let Some(endpoint) = endpoint else {
        return true;
    };
    match mode {
        RouteRuleMode::AllowAll => true,
        RouteRuleMode::AllowList => rules.iter().any(|rule| route_rule_matches(rule, endpoint)),
        RouteRuleMode::DenyList => !rules.iter().any(|rule| route_rule_matches(rule, endpoint)),
    }
}

fn account_route_allowed(pool: &Account, endpoint: Option<&str>) -> bool {
    account_route_policy_allows(pool.route_mode, &pool.route_rules, endpoint)
}

fn account_eligibility_route_allowed(pool: &AccountEligibility, endpoint: Option<&str>) -> bool {
    account_route_policy_allows(pool.route_mode, &pool.route_rules, endpoint)
}

fn account_eligibility_matches_supported_models(
    pool: &AccountEligibility,
    model_candidates: Option<&[String]>,
) -> bool {
    supported_models_match(&pool.supported_models, model_candidates)
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

fn external_release_retry_delay(failures: u32, intent: &AccountReleaseIntent) -> Duration {
    let multiplier = 1u32
        .checked_shl(failures.saturating_sub(1).min(16))
        .unwrap_or(u32::MAX);
    let base = ACCOUNT_QUEUE_REDIS_RETRY_DELAY
        .saturating_mul(multiplier)
        .min(ACCOUNT_RELEASE_RETRY_MAX_DELAY);
    let seed = intent.pool_id
        ^ ((intent.lease_id.len() as u64) << 32)
        ^ u64::from(failures)
        ^ match intent.release_kind {
            AccountLeaseReleaseKind::Pending => 0x5045_4e44_494e_47,
            AccountLeaseReleaseKind::Confirmed => 0x434f_4e46_4952_4d,
        };
    deterministic_duration_jitter(base, 10, seed)
}

fn external_release_system_retry_delay(failures: u32) -> Duration {
    let multiplier = 1u32
        .checked_shl(failures.saturating_sub(1).min(16))
        .unwrap_or(u32::MAX);
    ACCOUNT_QUEUE_REDIS_RETRY_DELAY
        .saturating_mul(multiplier)
        .min(ACCOUNT_RELEASE_RETRY_MAX_DELAY)
}

#[cfg(test)]
fn account_matches_supported_models(
    pool: &Account,
    model_candidates: Option<&[Option<&str>]>,
) -> bool {
    if pool.supported_models.is_empty() {
        return true;
    }
    let Some(model_candidates) = model_candidates else {
        return false;
    };
    let normalized =
        normalize_account_support_candidates(model_candidates.iter().flatten().copied());
    account_matches_supported_models_normalized(pool, Some(&normalized))
}

fn normalize_account_model_cooldown_key_model(model: &str) -> Option<String> {
    let normalized = model.trim().to_ascii_lowercase();
    (!normalized.is_empty()).then_some(normalized)
}

fn account_model_cooldown_key(pool_id: u64, normalized_model: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(normalized_model.as_bytes());
    let digest = hasher.finalize();
    format!("account:{}:model_cooldown:{:x}", pool_id, digest)
}

fn account_cooldown_keys(pool_id: u64, models: &[String]) -> Vec<String> {
    let mut keys = Vec::with_capacity(models.len().saturating_add(1));
    keys.push(format!("account:{}:cooldown", pool_id));
    keys.extend(
        models
            .iter()
            .map(|model| account_model_cooldown_key(pool_id, model)),
    );
    keys
}

fn account_transient_failure_key(pool_id: u64) -> String {
    format!("account:{}:transient_failures", pool_id)
}

fn account_transient_failure_reason_key(pool_id: u64, reason: &str) -> String {
    let normalized = reason
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .take(64)
        .collect::<String>();
    let reason = if normalized.is_empty() {
        "unknown"
    } else {
        normalized.as_str()
    };
    format!("account:{pool_id}:transient_failures:{reason}")
}

fn decode_pool_runtime_snapshot(
    pool_id: u64,
    models: &[String],
    cooldown_keys: &[String],
    coordinator: AccountCoordinatorSnapshot,
) -> anyhow::Result<PoolRuntimeSnapshot> {
    if coordinator.cooldown_values.len() != cooldown_keys.len()
        || coordinator.cooldown_ttls.len() != cooldown_keys.len()
    {
        anyhow::bail!("Redis returned an incomplete account coordinator snapshot");
    }

    let mut snapshot = PoolRuntimeSnapshot {
        in_flight: coordinator.capacity.pool_in_flight_requests,
        global_in_flight: coordinator.capacity.global_in_flight_requests,
        transient_failure_streak: coordinator.transient_failure_streak,
        transient_failure_ttl: coordinator.transient_failure_ttl,
        quality: coordinator.quality,
        ..PoolRuntimeSnapshot::default()
    };
    if let Some(raw) = coordinator
        .cooldown_values
        .first()
        .and_then(Option::as_deref)
    {
        let cooldown = serde_json::from_str::<AccountCooldownState>(raw)
            .map_err(|err| anyhow::anyhow!("invalid account cooldown for pool {pool_id}: {err}"))?;
        let remaining = coordinator
            .cooldown_ttls
            .first()
            .and_then(|remaining| *remaining)
            .ok_or_else(|| {
                anyhow::anyhow!("account cooldown for pool {pool_id} is missing a Redis TTL")
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
            let cooldown = serde_json::from_str::<AccountCooldownState>(raw).map_err(|err| {
                anyhow::anyhow!(
                    "invalid account model cooldown for pool {pool_id}, model {model}: {err}"
                )
            })?;
            let remaining = remaining.ok_or_else(|| {
                anyhow::anyhow!(
                    "account model cooldown for pool {pool_id}, model {model} is missing a Redis TTL"
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

#[allow(clippy::result_large_err)]
fn account_prepare_request(
    route: &AccountRouteRequest,
    pool: &Account,
) -> Result<body_pipeline::PreparedExternalRequest, AccountError> {
    body_pipeline::prepare_request(route, pool)
}

pub fn normalize_account_model_mapping_rules(
    rules: Vec<ModelMappingRule>,
) -> Vec<ModelMappingRule> {
    model_pipeline::normalize_mapping_rules(rules)
}

#[cfg(test)]
fn normalize_account_outbound_model(model: &str) -> String {
    model_pipeline::normalize_outbound_model(model)
}

const ACCOUNT_HEADER_OVERRIDE_MAX_ENTRIES: usize = 64;
const ACCOUNT_HEADER_OVERRIDE_MAX_NAME_LEN: usize = 200;
const ACCOUNT_HEADER_OVERRIDE_MAX_VALUE_LEN: usize = 8192;

pub fn normalize_account_header_overrides(
    raw: HashMap<String, String>,
) -> anyhow::Result<HashMap<String, String>> {
    if raw.len() > ACCOUNT_HEADER_OVERRIDE_MAX_ENTRIES {
        anyhow::bail!(
            "header_overrides cannot contain more than {} entries",
            ACCOUNT_HEADER_OVERRIDE_MAX_ENTRIES
        );
    }
    let mut out = HashMap::new();
    for (name, value) in raw {
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim().to_string();
        if name.is_empty() && value.is_empty() {
            continue;
        }
        if name.is_empty() {
            anyhow::bail!("header override name cannot be empty");
        }
        if value.is_empty() {
            continue;
        }
        if name.len() > ACCOUNT_HEADER_OVERRIDE_MAX_NAME_LEN {
            anyhow::bail!("header override name is too long: {}", name);
        }
        if value.len() > ACCOUNT_HEADER_OVERRIDE_MAX_VALUE_LEN {
            anyhow::bail!("header override value is too long: {}", name);
        }
        if account_header_override_blocked(&name) {
            anyhow::bail!(
                "header override is not allowed for sensitive header: {}",
                name
            );
        }
        HeaderName::from_bytes(name.as_bytes())
            .map_err(|err| anyhow::anyhow!("header override name is invalid: {err}"))?;
        HeaderValue::from_str(&value)
            .map_err(|err| anyhow::anyhow!("header override value is invalid for {name}: {err}"))?;
        out.insert(name, value);
    }
    Ok(out)
}

fn apply_account_header_overrides(headers: &mut HeaderMap, overrides: &HashMap<String, String>) {
    for (name, value) in overrides {
        if account_header_override_blocked(name) {
            continue;
        }
        let Ok(header_name) = HeaderName::from_bytes(name.as_bytes()) else {
            continue;
        };
        let Ok(header_value) = HeaderValue::from_str(value) else {
            continue;
        };
        headers.remove(&header_name);
        headers.insert(header_name, header_value);
    }
}

fn account_header_override_blocked(lower: &str) -> bool {
    matches!(
        lower,
        "host"
            | "content-length"
            | "content-type"
            | "transfer-encoding"
            | "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "proxy-connection"
            | "te"
            | "trailer"
            | "upgrade"
            | "authorization"
            | "x-api-key"
            | "x-goog-api-key"
            | "cookie"
            | "accept-encoding"
            | "sec-websocket-key"
            | "sec-websocket-version"
            | "sec-websocket-extensions"
            | "sec-websocket-protocol"
            | "sec-websocket-accept"
            | "session_id"
            | "conversation_id"
            | "x-codex-turn-state"
            | "x-codex-turn-metadata"
            | "chatgpt-account-id"
            | "x-claude-code-session-id"
            | "x-client-request-id"
            | "x-grok-conv-id"
    )
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
            | "x-goog-api-key"
            | "cookie"
            | "accept-encoding"
    )
}

fn should_forward_anthropic_header(name: &HeaderName) -> bool {
    matches!(
        name.as_str().to_ascii_lowercase().as_str(),
        "accept"
            | "x-stainless-retry-count"
            | "x-stainless-timeout"
            | "x-stainless-lang"
            | "x-stainless-package-version"
            | "x-stainless-os"
            | "x-stainless-arch"
            | "x-stainless-runtime"
            | "x-stainless-runtime-version"
            | "x-stainless-helper-method"
            | "anthropic-dangerous-direct-browser-access"
            | "anthropic-version"
            | "x-app"
            | "anthropic-beta"
            | "accept-language"
            | "sec-fetch-mode"
            | "user-agent"
            | "content-type"
            | "x-claude-code-session-id"
            | "x-client-request-id"
    )
}

fn resolve_anthropic_wire_header_name(name: &HeaderName) -> HeaderName {
    match name.as_str().to_ascii_lowercase().as_str() {
        "x-stainless-retry-count" => HeaderName::from_static("x-stainless-retry-count"),
        "x-stainless-timeout" => HeaderName::from_static("x-stainless-timeout"),
        "x-stainless-lang" => HeaderName::from_static("x-stainless-lang"),
        "x-stainless-package-version" => HeaderName::from_static("x-stainless-package-version"),
        "x-stainless-os" => HeaderName::from_static("x-stainless-os"),
        "x-stainless-arch" => HeaderName::from_static("x-stainless-arch"),
        "x-stainless-runtime" => HeaderName::from_static("x-stainless-runtime"),
        "x-stainless-runtime-version" => HeaderName::from_static("x-stainless-runtime-version"),
        "x-stainless-helper-method" => HeaderName::from_static("x-stainless-helper-method"),
        "anthropic-dangerous-direct-browser-access" => {
            HeaderName::from_static("anthropic-dangerous-direct-browser-access")
        }
        "anthropic-version" => HeaderName::from_static("anthropic-version"),
        "anthropic-beta" => HeaderName::from_static("anthropic-beta"),
        "x-app" => HeaderName::from_static("x-app"),
        "x-claude-code-session-id" => HeaderName::from_static("x-claude-code-session-id"),
        "x-client-request-id" => HeaderName::from_static("x-client-request-id"),
        "content-type" => header::CONTENT_TYPE,
        "user-agent" => header::USER_AGENT,
        "accept" => header::ACCEPT,
        "accept-language" => HeaderName::from_static("accept-language"),
        "sec-fetch-mode" => HeaderName::from_static("sec-fetch-mode"),
        _ => name.clone(),
    }
}

fn sanitize_anthropic_body_for_beta_header(body: &Bytes, headers: &HeaderMap) -> Bytes {
    const CONTEXT_MANAGEMENT_BETA: &str = "context-management-2025-06-27";
    let beta = headers
        .get(HeaderName::from_static("anthropic-beta"))
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if beta
        .split(',')
        .any(|token| token.trim() == CONTEXT_MANAGEMENT_BETA)
    {
        return body.clone();
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return body.clone();
    };
    if value.get("context_management").is_none() {
        return body.clone();
    }
    let mut patches = crate::anthropic::body_patch::BodyPatchSet::default();
    patches.remove("/context_management", "beta_compat");
    patches.apply(body)
}

fn sync_claude_code_session_header(headers: &mut HeaderMap, body: &[u8]) {
    let session_header = HeaderName::from_static("x-claude-code-session-id");
    if !headers.contains_key(&session_header) {
        return;
    }
    let Some(session_id) = extract_metadata_session_id_from_body(body) else {
        return;
    };
    let Ok(value) = HeaderValue::from_str(&session_id) else {
        return;
    };
    headers.insert(session_header, value);
}

fn extract_metadata_session_id_from_body(body: &[u8]) -> Option<String> {
    let value = serde_json::from_slice::<serde_json::Value>(body).ok()?;
    let user_id = value
        .get("metadata")
        .and_then(|metadata| metadata.get("user_id"))
        .and_then(|user_id| user_id.as_str())?;
    extract_session_id_from_metadata_user_id(user_id)
}

fn extract_session_id_from_metadata_user_id(user_id: &str) -> Option<String> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(user_id) {
        if let Some(session_id) = value.get("session_id").and_then(|value| value.as_str()) {
            if uuid::Uuid::parse_str(session_id).is_ok() {
                return Some(session_id.to_string());
            }
        }
    }
    let pos = user_id.find("session_")?;
    let session = user_id.get(pos + "session_".len()..)?;
    let uuid = session.get(..36)?;
    uuid::Uuid::parse_str(uuid).ok()?;
    Some(uuid.to_string())
}

/// Response headers relayed from an account upstream.
///
/// This is an allowlist: provider-specific headers (`server`, `cf-ray`, `x-amzn-*`, upstream
/// request ids, organization ids, ...) would fingerprint the serving channel, so only the content
/// headers needed to read the body and the standard retry hint pass. Upstream
/// `anthropic-ratelimit-*` headers describe the account's own quota, not this gateway's, so they
/// are dropped as well. Request ids are always replaced with this proxy's own ids afterwards.
fn should_forward_response_header(name: &HeaderName) -> bool {
    let lower = name.as_str().to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "content-type" | "content-encoding" | "cache-control" | "retry-after" | "x-should-retry"
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

fn success_error_body_protocol_error(_body: &Bytes, config: &AccountsConfig) -> AccountError {
    AccountError {
        status: Some(StatusCode::OK),
        message: "model endpoint returned an error envelope with a success status".to_string(),
        retryable: true,
        auto_disable_reason: None,
        cooldown: Some((
            Duration::from_secs(config.server_error_cooldown_secs.max(1)),
            "server_error".to_string(),
        )),
        protocol_error: Some("success_error_envelope"),
        raw_upstream_error: None,
    }
}

fn external_protocol_contamination_error(config: &AccountsConfig) -> AccountError {
    AccountError {
        status: Some(StatusCode::OK),
        message: RESPONSE_PROTOCOL_CONTAMINATION_DETAIL.to_string(),
        retryable: true,
        auto_disable_reason: None,
        cooldown: Some((
            Duration::from_secs(config.protocol_error_cooldown_secs.max(1)),
            "protocol_contamination".to_string(),
        )),
        protocol_error: None,
        raw_upstream_error: None,
    }
}

fn success_protocol_error(
    headers: &HeaderMap,
    _body: Option<&Bytes>,
    config: &AccountsConfig,
    context: &str,
) -> AccountError {
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
    AccountError {
        status: Some(StatusCode::OK),
        message,
        retryable: true,
        auto_disable_reason: Some("misconfigured_endpoint".to_string()),
        cooldown: Some((
            Duration::from_secs(config.protocol_error_cooldown_secs.max(1)),
            "misconfigured_endpoint".to_string(),
        )),
        protocol_error: Some("unexpected_response_shape"),
        raw_upstream_error: None,
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
    pool: Option<&Account>,
    attempts: Vec<AccountAttempt>,
    err: &AccountError,
    error_id: &str,
) -> AccountFinalError {
    AccountFinalError {
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
    let final_error = AccountFinalError {
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
        PoolCapacityWaitReason::Full => ("account_capacity_full", "Request capacity is full"),
        PoolCapacityWaitReason::Cooldown => (
            "account_cooldown",
            "Request capacity is temporarily cooling down",
        ),
        PoolCapacityWaitReason::ModelUnavailable => (
            "model_unavailable",
            "External fallback model is temporarily unavailable",
        ),
        PoolCapacityWaitReason::CoordinatorUnavailable => (
            "account_coordinator_unavailable",
            "Request capacity coordinator is temporarily unavailable",
        ),
    }
}

fn external_capacity_final_error(
    status: StatusCode,
    code: &str,
    message: impl Into<String>,
    error_id: &str,
) -> AccountFinalError {
    let retryable = code != "account_deadline_exceeded";
    AccountFinalError {
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

fn should_defer_synthetic_capacity_error_to_last_pool_error(err: &AccountFinalError) -> bool {
    err.pool_id.is_none()
        && matches!(
            err.route_error_type.as_str(),
            "account_capacity_full"
                | "account_queue_full"
                | "account_wait_timeout"
                | "account_cooldown"
                | "account_coordinator_unavailable"
                | "model_unavailable"
        )
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

fn external_error_record_message(err: &AccountError) -> (String, bool) {
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
    route: &AccountRouteRequest,
    err: &AccountError,
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
        if reason == "model_unavailable" || should_mark_account_hard_cooldown(reason) {
            metadata_insert(&mut metadata, "cooldownReason", reason.as_str());
            if !duration.is_zero() {
                metadata_insert(
                    &mut metadata,
                    "cooldownMs",
                    serde_json::Value::from(duration.as_millis() as u64),
                );
            }
        } else if should_record_account_soft_failure(reason) {
            metadata_insert(&mut metadata, "softFailureReason", reason.as_str());
            metadata_insert(
                &mut metadata,
                "softFailureWindowSecs",
                ACCOUNT_TRANSIENT_FAILURE_WINDOW_SECS as u64,
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
        raw_upstream_error: err.raw_upstream_error.clone(),
    }
}

fn synthetic_external_error_diagnostics(
    route: &AccountRouteRequest,
    status: StatusCode,
    source: &'static str,
) -> UsageErrorDiagnostics {
    UsageErrorDiagnostics {
        status_code: Some(status.as_u16()),
        source: Some(source.to_string()),
        error_id: Some(route.error_id.clone()),
        metadata: Some(json!({ "syntheticStatus": true })),
        public_error: None,
        raw_upstream_error: None,
    }
}

fn synthetic_external_capacity_error_diagnostics(
    route: &AccountRouteRequest,
    status: StatusCode,
    source: &'static str,
    config: &AccountsConfig,
    context: Option<&PoolCapacityWaitContext>,
) -> UsageErrorDiagnostics {
    let mut metadata = serde_json::Map::new();
    metadata_insert(&mut metadata, "syntheticStatus", true);
    metadata_insert(
        &mut metadata,
        "capacityMode",
        match config.capacity_mode {
            CapacityMode::FailFast => "fail_fast",
            CapacityMode::Wait => "wait",
        },
    );
    metadata_insert(
        &mut metadata,
        "maxQueuedRequests",
        config.max_queued_requests,
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
        raw_upstream_error: None,
    }
}

fn external_error_message_indicates_model_unavailable(lower_message: &str) -> bool {
    lower_message.contains("model_not_found")
        || lower_message.contains("failed to get available channel for model")
        || lower_message.contains("no available channel")
        || lower_message.contains("model is unavailable")
        || lower_message.contains("requested model is not available")
        || lower_message.contains("model is not available")
        || lower_message.contains("model is not supported in this region")
        || lower_message.contains("model is not available for this account")
        || lower_message.contains("model is not available for this endpoint")
}

/// Anthropic 官方格式的"模型不存在"：`{"type":"error","error":{"type":"not_found_error",
/// "message":"model: claude-xxx"}}`。正文里没有 invalid/unknown model 之类的关键字，
/// 不单独识别就会被当成裸 404 去逐个账号重试。
fn external_error_body_is_anthropic_model_not_found(body: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    let error = value.get("error").unwrap_or(&value);
    error.get("type").and_then(serde_json::Value::as_str) == Some("not_found_error")
        && error
            .get("message")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|message| {
                message
                    .trim_start()
                    .to_ascii_lowercase()
                    .starts_with("model:")
            })
}

fn external_error_message_indicates_invalid_model(lower_message: &str) -> bool {
    lower_message.contains("invalid model")
        || lower_message.contains("invalid_model_id")
        || lower_message.contains("invalid_model")
        || lower_message.contains("model not found")
        || lower_message.contains("model_not_found")
        || lower_message.contains("unsupported model")
        || lower_message.contains("unknown model")
        || lower_message.contains("not available for this endpoint")
}

const EXTERNAL_IMAGE_TOO_LARGE_MESSAGE: &str =
    "external upstream rejected the request because an image exceeds the size limit";

fn external_error_message_indicates_image_too_large(lower_message: &str) -> bool {
    lower_message.contains("image_size_exceeded") || lower_message.contains("image exceeds")
}

fn external_error_message_indicates_payload_too_large(lower_message: &str) -> bool {
    lower_message.contains("content_length_exceeds_threshold")
        || lower_message.contains("request payload is too large")
        || lower_message.contains("payload is too large")
        || lower_message.contains("request body is too large")
        || lower_message.contains("content length exceeded")
}

fn external_error_message_indicates_context_window_full(lower_message: &str) -> bool {
    lower_message.contains("context window is full")
        || lower_message.contains("input is too long")
        || lower_message.contains("prompt is too long")
}

fn classified_external_error(
    status: StatusCode,
    message: &'static str,
    retryable: bool,
    auto_disable_reason: Option<&'static str>,
    cooldown: Option<(Duration, &'static str)>,
) -> AccountError {
    AccountError {
        status: Some(status),
        message: message.to_string(),
        retryable,
        auto_disable_reason: auto_disable_reason.map(str::to_string),
        cooldown: cooldown.map(|(duration, reason)| (duration, reason.to_string())),
        protocol_error: None,
        raw_upstream_error: None,
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
            .min(Duration::from_secs(ACCOUNT_RETRY_AFTER_MAX_SECS)),
    )
}

fn account_cooldown_duration(
    retry_after: Option<Duration>,
    default_cooldown_secs: u64,
) -> Duration {
    retry_after.unwrap_or_else(|| Duration::from_secs(default_cooldown_secs.max(1)))
}

fn should_record_account_soft_failure(reason: &str) -> bool {
    !matches!(reason, "model_mapping_miss" | "model_unavailable")
}

/// 判断一次账号错误是否应计入该池的被动质量健康度。
///
/// 只有"上游质量相关"的失败才计入。以下两类明确排除：
///
/// 1. **下游请求自身的问题**：请求体非法、工具 schema 非法、输入超长。
///    这类错误换任何账号都会失败，计入健康度会凭空冤枉一个健康账号，
///    并在客户端反复发送坏请求时把整个池打成"不健康"。
/// 2. **模型不支持/映射未命中**：属于路由维度问题，已由模型级冷却处理，
///    不应污染账号的整体质量曲线。
///
/// 慢成功不走这里——它是成功样本，只影响延迟 EWMA，永远不触发错误降权。
fn external_error_affects_pool_quality(err: &AccountError) -> bool {
    let lower_message = err.message.to_ascii_lowercase();
    // 模型不可用是路由/模型支持问题，不是账号质量问题。这个判定不能只
    // 依赖 cooldown reason：管理员可以关闭模型级 cooldown，此时错误仍然
    // 必须保持质量中性。
    if external_error_message_indicates_model_unavailable(&lower_message)
        || (err.status == Some(StatusCode::NOT_FOUND)
            && external_error_message_indicates_invalid_model(&lower_message))
    {
        return false;
    }
    // 裸 404（没有冷却也不自动禁用）可能是请求自身引用了不存在的资源，
    // 换号由 retryStatusCodes 处理，不能把高优先级账号拉进观察期。
    if err.status == Some(StatusCode::NOT_FOUND)
        && err.cooldown.is_none()
        && err.auto_disable_reason.is_none()
    {
        return false;
    }
    if let Some(status) = err.status {
        // 400 且可判定为下游请求问题时不计入。
        if status == StatusCode::BAD_REQUEST
            && retry_pipeline::payload_too_long_message(&err.message)
        {
            return false;
        }
    }
    if let Some((_, reason)) = &err.cooldown {
        if !should_record_account_soft_failure(reason) {
            return false;
        }
    }
    true
}

fn should_mark_account_hard_cooldown(reason: &str) -> bool {
    matches!(reason, "misconfigured_endpoint")
}

/// 冷却原因的控制台文案；未知原因原样显示。
fn account_cooldown_reason_label(reason: &str) -> &str {
    match reason {
        "rate_limit" | "rate_limited" => "上游限流",
        "overloaded" => "上游过载",
        "upstream_timeout" => "上游超时",
        "model_unavailable" => "模型不可用",
        "misconfigured_endpoint" => "端点配置错误",
        "quota_exhausted" => "额度耗尽",
        "auth_error" => "鉴权失败",
        "server_error" => "上游服务错误",
        other => other,
    }
}

fn classify_external_error(
    status: StatusCode,
    body: Bytes,
    headers: HeaderMap,
    config: &AccountsConfig,
) -> AccountError {
    let retry_after = external_retry_after_duration(&headers);
    let raw_upstream_error = RawUpstreamError::from_bytes(
        "account",
        Some(status.as_u16()),
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        &body,
    );
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
                account_cooldown_duration(retry_after, config.rate_limit_cooldown_secs),
                "rate_limit",
            )),
        )
        .with_raw_upstream_error(Some(raw_upstream_error));
    }
    if lower.contains("database is locked") || lower.contains("sqlite_busy") {
        return classified_external_error(
            status,
            "external upstream database was busy",
            true,
            None,
            Some((
                account_cooldown_duration(retry_after, config.server_error_cooldown_secs),
                "database_busy",
            )),
        )
        .with_raw_upstream_error(Some(raw_upstream_error));
    }
    if lower.contains("invalid token") {
        return classified_external_error(
            status,
            "external upstream rejected authentication",
            true,
            Some("auth_error"),
            Some((
                account_cooldown_duration(retry_after, config.protocol_error_cooldown_secs),
                "auth_error",
            )),
        )
        .with_raw_upstream_error(Some(raw_upstream_error));
    }
    if lower.contains("channel affinity") && lower.contains("disabled") {
        return classified_external_error(
            status,
            "external upstream channel is disabled",
            true,
            Some("channel_disabled"),
            Some((
                account_cooldown_duration(retry_after, config.protocol_error_cooldown_secs),
                "channel_disabled",
            )),
        )
        .with_raw_upstream_error(Some(raw_upstream_error));
    }
    if external_error_message_indicates_model_unavailable(&lower)
        || (status == StatusCode::NOT_FOUND
            && (external_error_message_indicates_invalid_model(&lower)
                || external_error_body_is_anthropic_model_not_found(&body)))
    {
        return classified_external_error(
            status,
            "external upstream model is unavailable",
            true,
            None,
            (config.model_unavailable_cooldown_mode != ModelUnavailableCooldownMode::Disabled)
                .then(|| {
                    (
                        account_cooldown_duration(
                            retry_after,
                            config.model_unavailable_cooldown_secs,
                        ),
                        "model_unavailable",
                    )
                }),
        )
        .with_raw_upstream_error(Some(raw_upstream_error));
    }
    if status == StatusCode::BAD_REQUEST {
        let message = if external_error_message_indicates_image_too_large(&lower) {
            EXTERNAL_IMAGE_TOO_LARGE_MESSAGE
        } else if external_error_message_indicates_payload_too_large(&lower) {
            "external upstream rejected the request because the request payload is too large"
        } else if external_error_message_indicates_context_window_full(&lower) {
            "external upstream rejected the request because the prompt is too long"
        } else {
            "external upstream rejected the request"
        };
        return classified_external_error(status, message, false, None, None)
            .with_raw_upstream_error(Some(raw_upstream_error));
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        return classified_external_error(
            status,
            "external upstream rate limited the request",
            true,
            None,
            Some((
                account_cooldown_duration(retry_after, config.rate_limit_cooldown_secs),
                "rate_limit",
            )),
        )
        .with_raw_upstream_error(Some(raw_upstream_error));
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
                account_cooldown_duration(retry_after, config.protocol_error_cooldown_secs),
                reason,
            )),
        )
        .with_raw_upstream_error(Some(raw_upstream_error));
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
                account_cooldown_duration(retry_after, config.rate_limit_cooldown_secs),
                "quota_exhausted",
            )),
        )
        .with_raw_upstream_error(Some(raw_upstream_error));
    }
    if status.is_server_error() || status == StatusCode::REQUEST_TIMEOUT {
        return classified_external_error(
            status,
            "external upstream server was temporarily unavailable",
            true,
            None,
            Some((
                account_cooldown_duration(retry_after, config.server_error_cooldown_secs),
                "server_error",
            )),
        )
        .with_raw_upstream_error(Some(raw_upstream_error));
    }
    // 正文没指明模型问题的裸 404，多半是该账号的服务地址或路径配错，换个账号就能成功。
    // 标为可重试只是把是否换号交还给 retryStatusCodes（默认含 404）；不写冷却、不自动禁用，
    // 避免请求本身引用了不存在的资源时误伤账号，降权只靠被动质量采样。
    let retryable = status == StatusCode::NOT_FOUND;
    classified_external_error(
        status,
        "external upstream returned an unexpected error",
        retryable,
        None,
        None,
    )
    .with_raw_upstream_error(Some(raw_upstream_error))
}

fn should_retry_external_payload_guard(route: &AccountRouteRequest, err: &AccountError) -> bool {
    retry_pipeline::should_retry_payload_guard(route, err)
}

fn external_payload_guard_retry_route(route: &AccountRouteRequest) -> Option<AccountRouteRequest> {
    retry_pipeline::payload_guard_retry_route(route)
}

fn should_retry_external_cross_pool(config: &AccountsConfig, err: &AccountError) -> bool {
    retry_pipeline::should_retry_cross_pool(config, err)
}

fn external_same_pool_retry_delay(config: &AccountsConfig) -> Option<Duration> {
    retry_pipeline::same_pool_retry_delay(config)
}

fn auto_disable_reason_enabled(config: &AccountsConfig, reason: &str) -> bool {
    match reason {
        "auth_error" => config.auto_disable_on_auth_error,
        "security_lock" => config.auto_disable_on_security_lock,
        "quota_exhausted" => config.auto_disable_on_quota_exhausted,
        "misconfigured_endpoint" => config.auto_disable_on_misconfigured_endpoint,
        "channel_disabled" => config.auto_disable_on_channel_disabled,
        _ => false,
    }
}

fn error_type_for_external_error(err: &AccountError) -> String {
    if let Some(reason) = err.auto_disable_reason.as_deref() {
        return reason.to_string();
    }
    let lower_message = err.message.to_ascii_lowercase();
    if external_error_message_indicates_model_unavailable(&lower_message)
        || (err.status == Some(StatusCode::NOT_FOUND)
            && external_error_message_indicates_invalid_model(&lower_message))
    {
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
            .unwrap_or("account_error")
            .to_string();
    }
    match err.status {
        Some(StatusCode::TOO_MANY_REQUESTS) => "rate_limit",
        Some(status) if status.is_server_error() => "server_error",
        Some(StatusCode::BAD_REQUEST) => "bad_request",
        _ => "account_error",
    }
    .to_string()
}

fn anthropic_error_type_for_external_error(err: &AccountError) -> &'static str {
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

fn external_route_known_tool_names(route: &AccountRouteRequest) -> Arc<Vec<String>> {
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

#[cfg(test)]
fn process_non_stream_response_usage(
    bytes: Bytes,
    route: Option<&AccountRouteRequest>,
    projection: Option<&ExternalUsageProjectionContext>,
    known_tool_names: impl IntoIterator<Item = String>,
) -> ProjectedNonStreamBody {
    process_non_stream_response_usage_with_repair(bytes, route, projection, known_tool_names, None)
}

fn process_non_stream_response_usage_with_repair(
    bytes: Bytes,
    route: Option<&AccountRouteRequest>,
    projection: Option<&ExternalUsageProjectionContext>,
    known_tool_names: impl IntoIterator<Item = String>,
    zero_repair: Option<zero_usage_repair::ZeroUsageRepairContext>,
) -> ProjectedNonStreamBody {
    let mut usage_capture = ExternalUsageCapture {
        request_input_tokens: route
            .map(|route| estimated_external_request_input_tokens(route, projection)),
        ..Default::default()
    };
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
    usage_capture.upstream_usage.observe_response(&value);
    usage_capture.downstream_stop_reason = external_stop_reason_from_value(&value);
    usage_capture.upstream_response_model = response_model::body_declared_model(&value);
    let sanitized = route.is_none_or(|route| route.history_sanitize_enabled)
        && sanitize_response_content(&mut value, known_tool_names).blocks > 0;

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
            let repaired = zero_repair
                .map(|ctx| {
                    zero_usage_repair::repair_usage_value(
                        usage,
                        None,
                        response_output_estimate.unwrap_or(0),
                        &ctx.with_known_request_input_tokens(usage_capture.request_input_tokens),
                    )
                })
                .unwrap_or_default();
            let raw_usage = if repaired.any() {
                zero_usage_repair::mark_estimated(&mut usage_capture, repaired);
                cache_usage_from_any_value(usage)
            } else {
                normalized.usage
            };
            usage_capture.raw = raw_usage;
            usage_capture.reported = raw_usage;
            changed |= normalized.changed || repaired.any();

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
        if let Some(ctx) = zero_repair.as_ref() {
            zero_usage_repair::apply_fallback_to_estimated_body(
                &mut value,
                &mut usage_capture,
                ctx,
            );
        }
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
    route: Option<&AccountRouteRequest>,
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
    route: Option<&AccountRouteRequest>,
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
    route: &AccountRouteRequest,
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
        .or_else(|| {
            external_route_raw_projection_payload(route)
                .as_deref()
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
                let candidates = collect_account_usage_debug_candidates(&value);
                if !candidates.is_empty() {
                    saw_usage = true;
                    usage_candidates.extend(candidates);
                    if usage_candidates.len() > ACCOUNT_USAGE_DEBUG_USAGE_SAMPLE_LIMIT {
                        usage_candidates.truncate(ACCOUNT_USAGE_DEBUG_USAGE_SAMPLE_LIMIT);
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
        if debug.raw_usage_event_samples.len() < ACCOUNT_USAGE_DEBUG_USAGE_SAMPLE_LIMIT {
            let raw_event_cap = event.len().min(ACCOUNT_USAGE_DEBUG_EVENT_SAMPLE_BYTES);
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

#[derive(Debug, Clone, Copy)]
enum ExternalWireDebugStreamSide {
    RawUpstream,
    ProcessedDownstream,
}

impl ExternalWireDebugSseCapture {
    fn append_preview(&mut self, bytes: &[u8], max_preview: usize) {
        self.bytes_seen = self.bytes_seen.saturating_add(bytes.len() as u64);
        if max_preview == 0 {
            return;
        }
        let existing = BASE64_STANDARD
            .decode(self.preview_base64.as_bytes())
            .unwrap_or_default();
        if existing.len() >= max_preview {
            self.preview_truncated = true;
            return;
        }
        let remaining = max_preview - existing.len();
        let append = &bytes[..bytes.len().min(remaining)];
        let mut combined = existing;
        combined.extend_from_slice(append);
        self.preview_base64 = BASE64_STANDARD.encode(&combined);
        self.preview_utf8 = String::from_utf8_lossy(&combined).to_string();
        if append.len() < bytes.len() || self.bytes_seen as usize > max_preview {
            self.preview_truncated = true;
        }
    }
}

fn record_external_wire_debug_sse_bytes(
    bytes: &[u8],
    capture: Option<&Arc<SyncMutex<ExternalUsageCapture>>>,
    plan: ExternalStreamProcessingPlan,
    side: ExternalWireDebugStreamSide,
) {
    if !plan.wire_debug_enabled {
        return;
    }
    let Some(capture) = capture else {
        return;
    };
    let mut capture = capture.lock();
    let target = match side {
        ExternalWireDebugStreamSide::RawUpstream => &mut capture.wire_debug.raw_upstream_stream,
        ExternalWireDebugStreamSide::ProcessedDownstream => {
            &mut capture.wire_debug.processed_downstream_stream
        }
    };
    if matches!(side, ExternalWireDebugStreamSide::ProcessedDownstream) {
        target.chunks_seen = target.chunks_seen.saturating_add(1);
        if bytes.is_empty() {
            target.empty_outputs_seen = target.empty_outputs_seen.saturating_add(1);
            return;
        }
    }
    target.append_preview(bytes, plan.wire_debug_max_body_bytes);
    let mut offset = 0usize;
    while let Some((idx, delimiter_len)) = find_sse_event_delimiter(&bytes[offset..]) {
        let event_end = offset + idx + delimiter_len;
        account_wire_debug_observe_sse_event(&bytes[offset..event_end], target);
        offset = event_end;
    }
    if offset < bytes.len() {
        target.json_parse_errors = target.json_parse_errors.saturating_add(1);
    }
}

fn account_wire_debug_observe_sse_event(event: &[u8], target: &mut ExternalWireDebugSseCapture) {
    let event_index = target.events_seen.saturating_add(1);
    target.events_seen = target.events_seen.saturating_add(1);
    let text = String::from_utf8_lossy(event);
    let mut event_name: Option<String> = None;
    let mut payload_type: Option<String> = None;
    let mut usage_candidates = Vec::new();
    let mut saw_usage = false;

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
        target.data_lines_seen = target.data_lines_seen.saturating_add(1);
        let data_json = data.trim();
        if data_json.is_empty() {
            continue;
        }
        if data_json == "[DONE]" {
            target.done_events_seen = target.done_events_seen.saturating_add(1);
            continue;
        }
        match serde_json::from_str::<serde_json::Value>(data_json) {
            Ok(value) => {
                payload_type = payload_type.or_else(|| {
                    value
                        .get("type")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                });
                account_wire_debug_record_sse_semantic(&mut target.semantic, event_index, &value);
                let candidates = collect_account_usage_debug_candidates(&value);
                if !candidates.is_empty() {
                    saw_usage = true;
                    usage_candidates.extend(candidates);
                    if usage_candidates.len() > ACCOUNT_WIRE_DEBUG_USAGE_SAMPLE_LIMIT {
                        usage_candidates.truncate(ACCOUNT_WIRE_DEBUG_USAGE_SAMPLE_LIMIT);
                    }
                }
            }
            Err(_) => {
                target.json_parse_errors = target.json_parse_errors.saturating_add(1);
            }
        }
    }

    if let Some(event_name) = event_name.as_deref().or(payload_type.as_deref()) {
        *target
            .event_types
            .entry(event_name.to_string())
            .or_default() += 1;
    }
    if saw_usage {
        target.usage_events_seen = target.usage_events_seen.saturating_add(1);
        for candidate in &usage_candidates {
            *target
                .usage_paths
                .entry(candidate.path.clone())
                .or_default() += 1;
        }
        if target.usage_event_samples.len() < ACCOUNT_WIRE_DEBUG_USAGE_SAMPLE_LIMIT {
            let raw_event_cap = event.len().min(ACCOUNT_WIRE_DEBUG_EVENT_SAMPLE_BYTES);
            let raw_event_preview = &event[..raw_event_cap];
            target
                .usage_event_samples
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
}

fn account_wire_debug_record_sse_semantic(
    summary: &mut ExternalWireDebugSseSemanticSummary,
    event_index: u64,
    value: &serde_json::Value,
) {
    if let Some(stop_reason) = external_stop_reason_from_value(value) {
        summary.last_stop_reason = Some(stop_reason);
    }
    match value.get("type").and_then(serde_json::Value::as_str) {
        Some("message_start") => {
            summary.message_start_events = summary.message_start_events.saturating_add(1);
        }
        Some("message_delta") => {
            summary.message_delta_events = summary.message_delta_events.saturating_add(1);
        }
        Some("message_stop") => {
            summary.message_stop_events = summary.message_stop_events.saturating_add(1);
        }
        Some("content_block_start") => {
            summary.content_block_start_events =
                summary.content_block_start_events.saturating_add(1);
            let block = value.get("content_block");
            match block
                .and_then(|block| block.get("type"))
                .and_then(serde_json::Value::as_str)
            {
                Some("text") => {
                    summary.text_block_start_events =
                        summary.text_block_start_events.saturating_add(1);
                    if let Some(text) = block
                        .and_then(|block| block.get("text"))
                        .and_then(serde_json::Value::as_str)
                    {
                        let chars = text.chars().count() as u64;
                        summary.text_block_start_chars =
                            summary.text_block_start_chars.saturating_add(chars);
                        if chars > 0 {
                            account_wire_debug_mark_visible_text(summary, event_index);
                            account_wire_debug_mark_semantic_output(summary, event_index);
                        }
                    }
                }
                Some("thinking") => {
                    summary.thinking_block_start_events =
                        summary.thinking_block_start_events.saturating_add(1);
                    if let Some(thinking) = block
                        .and_then(|block| block.get("thinking"))
                        .and_then(serde_json::Value::as_str)
                    {
                        let chars = thinking.chars().count() as u64;
                        summary.thinking_block_start_chars =
                            summary.thinking_block_start_chars.saturating_add(chars);
                        if chars > 0 {
                            account_wire_debug_mark_semantic_output(summary, event_index);
                        }
                    }
                }
                Some("redacted_thinking") => {
                    summary.redacted_thinking_block_start_events = summary
                        .redacted_thinking_block_start_events
                        .saturating_add(1);
                    if let Some(data) = block
                        .and_then(|block| block.get("data"))
                        .and_then(serde_json::Value::as_str)
                    {
                        let chars = data.chars().count() as u64;
                        summary.redacted_thinking_block_start_chars = summary
                            .redacted_thinking_block_start_chars
                            .saturating_add(chars);
                        if chars > 0 {
                            account_wire_debug_mark_semantic_output(summary, event_index);
                        }
                    }
                }
                Some("tool_use") => {
                    summary.tool_use_block_start_events =
                        summary.tool_use_block_start_events.saturating_add(1);
                    account_wire_debug_mark_semantic_output(summary, event_index);
                }
                Some("server_tool_use") => {
                    summary.server_tool_use_block_start_events =
                        summary.server_tool_use_block_start_events.saturating_add(1);
                    account_wire_debug_mark_semantic_output(summary, event_index);
                }
                _ => {}
            }
        }
        Some("content_block_delta") => {
            summary.content_block_delta_events =
                summary.content_block_delta_events.saturating_add(1);
            match value
                .get("delta")
                .and_then(|delta| delta.get("type"))
                .and_then(serde_json::Value::as_str)
            {
                Some("text_delta") => {
                    summary.text_delta_events = summary.text_delta_events.saturating_add(1);
                    if let Some(text) = value
                        .pointer("/delta/text")
                        .and_then(serde_json::Value::as_str)
                    {
                        let chars = text.chars().count() as u64;
                        summary.text_delta_chars = summary.text_delta_chars.saturating_add(chars);
                        if chars > 0 {
                            account_wire_debug_mark_visible_text(summary, event_index);
                            account_wire_debug_mark_semantic_output(summary, event_index);
                        }
                    }
                }
                Some("thinking_delta") => {
                    summary.thinking_delta_events = summary.thinking_delta_events.saturating_add(1);
                    if let Some(thinking) = value
                        .pointer("/delta/thinking")
                        .and_then(serde_json::Value::as_str)
                    {
                        let chars = thinking.chars().count() as u64;
                        summary.thinking_delta_chars =
                            summary.thinking_delta_chars.saturating_add(chars);
                        if chars > 0 {
                            account_wire_debug_mark_semantic_output(summary, event_index);
                        }
                    }
                }
                Some("signature_delta") => {
                    summary.signature_delta_events =
                        summary.signature_delta_events.saturating_add(1);
                }
                Some("input_json_delta") => {
                    summary.input_json_delta_events =
                        summary.input_json_delta_events.saturating_add(1);
                    if let Some(partial_json) = value
                        .pointer("/delta/partial_json")
                        .and_then(serde_json::Value::as_str)
                    {
                        let chars = partial_json.chars().count() as u64;
                        summary.input_json_delta_chars =
                            summary.input_json_delta_chars.saturating_add(chars);
                        if chars > 0 {
                            account_wire_debug_mark_semantic_output(summary, event_index);
                        }
                    }
                }
                _ => {}
            }
        }
        Some("content_block_stop") => {
            summary.content_block_stop_events = summary.content_block_stop_events.saturating_add(1);
        }
        Some("error") => {
            summary.error_events = summary.error_events.saturating_add(1);
        }
        Some("ping") => {
            summary.ping_events = summary.ping_events.saturating_add(1);
        }
        _ => {}
    }
}

fn account_wire_debug_mark_visible_text(
    summary: &mut ExternalWireDebugSseSemanticSummary,
    event_index: u64,
) {
    if summary.first_visible_text_event_index.is_none() {
        summary.first_visible_text_event_index = Some(event_index);
    }
}

fn account_wire_debug_mark_semantic_output(
    summary: &mut ExternalWireDebugSseSemanticSummary,
    event_index: u64,
) {
    if summary.first_semantic_output_event_index.is_none() {
        summary.first_semantic_output_event_index = Some(event_index);
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
    update_external_usage_capture_stop_reason(capture, external_stop_reason_from_value(value));
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
    // Normalize OpenAI-compatible usage before projection/capture. Accounts
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
            || self.buffered_bytes.saturating_add(bytes) > ACCOUNT_MAX_SSE_EVENT_BUFFER_BYTES
        {
            if !self.overflow {
                tracing::warn!(
                    buffered_bytes = self.buffered_bytes.saturating_add(bytes),
                    max_buffered_bytes = ACCOUNT_MAX_SSE_EVENT_BUFFER_BYTES,
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
        if let Some(index) = self.current_text_index.filter(|_| !pending.is_empty()) {
            out.extend(external_text_delta_event(index, &pending));
            self.current_text_visible = true;
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

/// 首输出之后上游不可恢复（断流/idle 超时/lease 丢失/缓冲超限）时写给下游的终止事件。
/// overloaded_error 让 Claude Code 重试整个请求；缓冲超限用重试无用的 api_error。
fn external_post_output_terminal_error_event(
    mask: &ExternalStreamErrorMask,
    error_type: &'static str,
    capture: Option<&Arc<SyncMutex<ExternalUsageCapture>>>,
    detail: &str,
) -> Vec<u8> {
    if let Some(capture) = capture {
        let mut capture = capture.lock();
        if capture.stream_error_message.is_none() {
            capture.stream_error_message = Some(detail.to_string());
        }
    }
    let message = envelope::public_message_with_error_id(
        envelope::PUBLIC_TEMPORARY_FAILURE_MESSAGE,
        &mask.error_id,
    );
    let body = json!({
        "type": "error",
        "error": {"type": error_type, "message": message},
        "request_id": mask.request_id,
    });
    format!("event: error\ndata: {body}\n\n").into_bytes()
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
    client_model: Option<&str>,
) -> Vec<u8> {
    if let Some(capture) = capture {
        capture.lock().upstream_usage.observe_sse(event);
    }
    record_external_usage_debug_sse_event(event, capture, plan);
    record_external_wire_debug_sse_bytes(
        event,
        capture,
        plan,
        ExternalWireDebugStreamSide::RawUpstream,
    );
    let repaired = capture.and_then(|capture| zero_usage_repair::repair_sse_event(event, capture));
    let event: &[u8] = repaired.as_deref().unwrap_or(event);
    let masked = plan
        .mask_errors
        .then(|| maybe_mask_external_stream_error_event(event, capture, stream_error_mask))
        .flatten();
    let output = if let Some(masked) = masked {
        transcript_state
            .map(|state| process_external_transcript_state(state, &masked, capture))
            .unwrap_or(masked)
    } else {
        let processed = if projection.is_some() {
            rewrite_sse_event_usage(event, projection, capture)
        } else {
            if plan.capture_usage {
                capture_sse_event_usage(event, projection, capture);
            }
            event.to_vec()
        };
        if let Some(state) = transcript_state {
            process_external_transcript_state(state, &processed, capture)
        } else {
            processed
        }
    };
    let model_rewrite = response_model::process_sse_event_model(&output, client_model);
    if let (Some(model), Some(capture)) = (model_rewrite.upstream_model, capture) {
        capture.lock().upstream_response_model.get_or_insert(model);
    }
    let output = model_rewrite.rewritten.unwrap_or(output);
    record_external_wire_debug_sse_bytes(
        &output,
        capture,
        plan,
        ExternalWireDebugStreamSide::ProcessedDownstream,
    );
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
    if let Some((detail, capture)) = state.take_pending_fatal_error().zip(capture) {
        let mut capture = capture.lock();
        if capture.stream_error_message.is_none() {
            capture.stream_error_message = Some(detail);
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
    drain_sse_events_with_transcript(
        buffer,
        projection,
        capture,
        stream_error_mask,
        plan,
        None,
        None,
    )
}

/// 提交后流的 SSE 处理：普通模式等同 `drain_sse_events_with_transcript`；提前提交模式下额外
/// 丢弃重复的 message_start（下游已经收到过），并用上游真实 message_start 的输入侧用量补全
/// message_delta 缺失的字段（下游收到的 message_start 用量已清零）。
#[allow(clippy::too_many_arguments)]
fn drain_sse_events_for_downstream(
    early_commit: Option<&ExternalEarlyCommitDownstream>,
    buffer: &mut Vec<u8>,
    projection: Option<&ExternalUsageProjectionContext>,
    capture: Option<&Arc<SyncMutex<ExternalUsageCapture>>>,
    stream_error_mask: Option<&ExternalStreamErrorMask>,
    plan: ExternalStreamProcessingPlan,
    mut transcript_state: Option<&mut ExternalAnthropicTranscriptState>,
    client_model: Option<&str>,
) -> Vec<u8> {
    let Some(early_commit) = early_commit else {
        return drain_sse_events_with_transcript(
            buffer,
            projection,
            capture,
            stream_error_mask,
            plan,
            transcript_state,
            client_model,
        );
    };
    let mut out = Vec::new();
    while let Some((idx, delimiter_len)) = find_sse_event_delimiter(buffer) {
        let end = idx + delimiter_len;
        let event = buffer.drain(..end).collect::<Vec<u8>>();
        let processed = process_sse_event_with_plan_and_transcript(
            &event,
            projection,
            capture,
            stream_error_mask,
            plan,
            transcript_state.as_deref_mut(),
            client_model,
        );
        if early_commit.message_start_sent() && early_commit::sse_event_is_message_start(&processed)
        {
            continue;
        }
        match early_commit
            .upstream_start_usage()
            .and_then(|start| early_commit::backfill_message_delta_usage(&processed, &start))
        {
            Some(backfilled) => out.extend(backfilled),
            None => out.extend(processed),
        }
    }
    out
}

fn drain_sse_events_with_transcript(
    buffer: &mut Vec<u8>,
    projection: Option<&ExternalUsageProjectionContext>,
    capture: Option<&Arc<SyncMutex<ExternalUsageCapture>>>,
    stream_error_mask: Option<&ExternalStreamErrorMask>,
    plan: ExternalStreamProcessingPlan,
    mut transcript_state: Option<&mut ExternalAnthropicTranscriptState>,
    client_model: Option<&str>,
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
            client_model,
        ));
    }
    out
}

/*
 * The wrappers above keep the existing unit-test surface for usage-only processing while the live
 * external route passes a request-scoped transcript state through the extended functions.
 */

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
        if capture.raw_upstream_error.is_none() {
            capture.raw_upstream_error = Some(RawUpstreamError::from_bytes(
                "account_stream",
                Some(StatusCode::OK.as_u16()),
                Some("text/event-stream"),
                event,
            ));
        }
    }
    tracing::warn!(
        request_id = %mask.request_id,
        error_id = %mask.error_id,
        pool_id = mask.pool_id,
        pool_name = %mask.pool_name,
        error_class = "external_stream_error_event",
        "account stream error event masked"
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

fn update_external_usage_capture_stop_reason(
    capture: Option<&Arc<SyncMutex<ExternalUsageCapture>>>,
    stop_reason: Option<String>,
) {
    let Some(stop_reason) = stop_reason else {
        return;
    };
    let Some(capture) = capture else {
        return;
    };
    capture.lock().downstream_stop_reason = Some(stop_reason);
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

fn external_stop_reason_from_value(value: &serde_json::Value) -> Option<String> {
    const ANTHROPIC_STOP_REASON_POINTERS: [&str; 5] = [
        "/delta/stop_reason",
        "/stop_reason",
        "/message/stop_reason",
        "/data/stop_reason",
        "/response/stop_reason",
    ];
    for pointer in ANTHROPIC_STOP_REASON_POINTERS {
        if let Some(reason) = value
            .pointer(pointer)
            .and_then(serde_json::Value::as_str)
            .and_then(normalize_external_stop_reason)
        {
            return Some(reason);
        }
    }
    value
        .get("choices")
        .and_then(serde_json::Value::as_array)
        .and_then(|choices| {
            choices.iter().find_map(|choice| {
                choice
                    .get("finish_reason")
                    .and_then(serde_json::Value::as_str)
                    .and_then(normalize_external_stop_reason)
            })
        })
}

fn normalize_external_stop_reason(reason: &str) -> Option<String> {
    let trimmed = reason.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(match trimmed {
        "stop" => "end_turn".to_string(),
        "length" => "max_tokens".to_string(),
        "tool_calls" | "function_call" => "tool_use".to_string(),
        other => other.to_string(),
    })
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
    if projection.mode != UsageProjectionMode::CurrentPathPolicy {
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
    let plus_shaper = projection.prefix_tool_plus.map(|plus| plus.shaper());
    let computed = plus_shaper
        .map(|shaper| shaper.amplify(computed))
        .unwrap_or(computed);
    if commit_cache_state {
        projection.mark_committed(computed);
    }
    let controlled = if let Some(shaper) = plus_shaper {
        match shaper.frequency_control() {
            Some(config) if projection.cache_state_enabled => projection
                .prompt_cache_creation_controller
                .preview_success_with_mode(
                    projection.scope.as_ref(),
                    config,
                    computed,
                    projection.credential_key.as_deref(),
                    Some(projection.model.as_str()),
                    true,
                ),
            _ => computed,
        }
    } else if projection.cache_state_enabled && projection.reported_policy.is_some() {
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
    let shaped = plus_shaper
        .map(|shaper| {
            shaper.finalize(
                computed,
                controlled,
                projection
                    .pricing_catalog
                    .model_pricing(projection.model.as_str()),
                raw_usage.to_cache_usage(),
            )
        })
        .unwrap_or(shaped);
    let projected = shaped
        .with_account_usage_uplift(projection.uplift_percent)
        .with_account_output_uplift(
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
    let projected = apply_account_usage_cost_floor(projected, raw_for_floor, projection);
    let projected = apply_account_bounded_final_caps(projected, controlled, projection);
    // The route's cache TTL mode decides the final 5m/1h split, after every shaping step.
    let write_ttl = projection.resolve_write_ttl(upstream_raw_usage.as_ref(), commit_cache_state);
    let projected = projected.with_cache_creation_ttl(write_ttl);
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

fn apply_account_usage_final_path_guards(
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

/// `bounded_high_cache`：计费补偿之后按上下限收口（input 上限、最终读写上限、
/// 读取倍数上限、creation control 单次上限、读写最小值），收口之后不再改写。
/// creation control 未放行本次写入时最终写入为 0（计费补偿补上的写入也会被清掉）。
/// 其他策略原样返回。
///
/// 读取倍数上限的基准与本地链路一致，用本次请求的真实输入（`raw_input_tokens`），
/// 不用上游 usage（上游可能把放大后的缓存写入计入总量）。
fn apply_account_bounded_final_caps(
    usage: CacheUsage,
    controlled: CacheUsage,
    projection: &ExternalUsageProjectionContext,
) -> CacheUsage {
    if !projection.bounded_final_caps {
        return usage;
    }
    let raw = CacheUsage {
        total_input_tokens: projection.raw_input_tokens.max(0),
        input_tokens: projection.raw_input_tokens.max(0),
        output_tokens: usage.output_tokens,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        cache_creation_5m_input_tokens: 0,
        cache_creation_1h_input_tokens: 0,
    };
    let reported_policy = projection.reported_policy.as_ref();
    let (max_cache_read_tokens, max_cache_creation_tokens) = reported_policy
        .map(|policy| policy.final_cache_caps(usage))
        .unwrap_or((0, 0));
    let creation_control = projection.prompt_cache_creation_control.normalized();
    // 与投影里 creation control 的调用条件一致：只有实际经过频次控制时才可能拦下写入。
    let controls_creation = creation_control.enabled
        && projection.cache_state_enabled
        && projection.reported_policy.is_some();
    let input = crate::anthropic::bounded_shaping::BoundedShapingInput {
        policy: projection.cache_hit_shaping_policy,
        has_reusable_prefix: usage.cache_read_input_tokens > 0,
        creation_allowed: !controls_creation || controlled.cache_creation_input_tokens > 0,
        raw,
        pricing: None,
        input_max_tokens: reported_policy
            .map(|policy| policy.reported_input_max_tokens())
            .unwrap_or(0),
        max_cache_read_tokens,
        max_cache_creation_tokens,
        max_creation_event_tokens: if controls_creation {
            creation_control.max_creation_tokens_per_event.max(0)
        } else {
            0
        },
        carried_cost_deficit_usd: 0.0,
        seed: projection.bounded_seed,
    };
    crate::anthropic::bounded_shaping::enforce_bounded_usage(usage, &input)
}

fn apply_account_usage_cost_floor(
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
    let candidate = apply_account_usage_final_path_guards(
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

    let next = apply_account_usage_final_path_guards(
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
    match usage.cache_creation_breakdown_json() {
        Some(breakdown) => {
            obj.insert("cache_creation".to_string(), breakdown);
        }
        None => {
            obj.remove("cache_creation");
        }
    }
}

trait AccountUsageUplift {
    fn with_account_usage_uplift(self, percent: u32) -> Self;
    fn with_account_output_uplift(self, min_tokens: i32, percent: u32) -> Self;
}

impl AccountUsageUplift for CacheUsage {
    fn with_account_usage_uplift(self, percent: u32) -> Self {
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

    fn with_account_output_uplift(self, min_tokens: i32, percent: u32) -> Self {
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

fn account_billing_from_capture(
    route: &AccountRouteRequest,
    pool: &Account,
    capture: ExternalUsageCapture,
) -> Option<AccountBilling> {
    // Keep the legacy decision to estimate missing usage before choosing raw statistics.
    let legacy_raw = capture.raw?;
    let (raw, raw_cost_usage) = pool
        .raw_usage_source
        .select(legacy_raw, capture.upstream_usage.snapshot());
    let reported = capture.reported.or(capture.raw).unwrap_or(raw);
    let shaped = capture
        .shaped
        .or(capture.reported)
        .or(capture.raw)
        .unwrap_or(raw);
    let stream_response_mode = capture
        .stream_response_mode
        .map(|mode| mode.as_str().to_string());
    let mut billing = account_billing(
        route,
        pool,
        capture.request_input_tokens,
        raw,
        raw_cost_usage,
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
    billing.upstream_response_model = capture.upstream_response_model;
    Some(billing)
}

fn account_billing_from_capture_ref(
    route: &AccountRouteRequest,
    pool: &Account,
    capture: &Arc<SyncMutex<ExternalUsageCapture>>,
) -> Option<AccountBilling> {
    account_billing_from_capture(route, pool, capture.lock().clone())
}

fn account_billing_from_stream_estimate(
    route: &AccountRouteRequest,
    pool: &Account,
    capture: &Arc<SyncMutex<ExternalUsageCapture>>,
    projection: Option<&ExternalUsageProjectionContext>,
    estimated_output_tokens: i32,
) -> Option<AccountBilling> {
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
    let (raw, raw_cost_usage) = pool
        .raw_usage_source
        .select(estimated.raw, capture.upstream_usage.snapshot());
    let mut billing = account_billing(
        route,
        pool,
        Some(estimated.request_input_tokens),
        raw,
        raw_cost_usage,
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
    billing.upstream_response_model = capture.upstream_response_model;
    Some(billing)
}

fn account_billing(
    route: &AccountRouteRequest,
    pool: &Account,
    request_input_tokens: Option<i32>,
    raw_usage: CacheUsage,
    raw_cost_usage: CacheUsage,
    shaped_usage: CacheUsage,
    reported_usage: CacheUsage,
    usage_projection_applied: bool,
) -> AccountBilling {
    let pricing_model = route
        .upstream_model
        .as_deref()
        .or(route.model_hint.as_deref())
        .or_else(|| route.payload.as_ref().map(|payload| payload.model.as_str()))
        .unwrap_or("unknown");
    let raw_estimate = route
        .pricing_catalog
        .estimate(pricing_model, raw_cost_usage);
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

    AccountBilling {
        request_input_tokens: request_input_tokens.map(|tokens| tokens.max(0)),
        upstream_response_model: None,
        raw_usage_source: pool.raw_usage_source,
        raw_usage: external_usage_snapshot(raw_usage),
        raw_cost_usage: external_usage_snapshot(raw_cost_usage),
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
    route: &AccountRouteRequest,
    pool: &Account,
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

fn external_usage_snapshot(usage: CacheUsage) -> AccountUsageSnapshot {
    AccountUsageSnapshot {
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
mod terminal_event_tests {
    use super::*;

    fn sse(name: &str, data: &str) -> Bytes {
        Bytes::from(format!("event: {name}\ndata: {data}\n\n"))
    }

    #[test]
    fn message_stop_and_stop_reason_delta_are_terminal() {
        assert!(external_stream_chunk_has_terminal_event(&sse(
            "message_stop",
            r#"{"type":"message_stop"}"#
        )));
        assert!(external_stream_chunk_has_terminal_event(&sse(
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":5}}"#
        )));
        assert!(external_stream_chunk_has_terminal_event(
            &Bytes::from_static(b"data: [DONE]\n\n")
        ));
        assert!(external_stream_chunk_has_terminal_event(
            &Bytes::from_static(
                b"data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n"
            )
        ));
    }

    #[test]
    fn content_events_and_null_stop_reason_are_not_terminal() {
        for (name, data) in [
            (
                "message_start",
                r#"{"type":"message_start","message":{"id":"m"}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"hi"}}"#,
            ),
            (
                "message_delta",
                r#"{"type":"message_delta","delta":{"stop_reason":null},"usage":{"output_tokens":1}}"#,
            ),
            ("ping", r#"{"type":"ping"}"#),
        ] {
            assert!(
                !external_stream_chunk_has_terminal_event(&sse(name, data)),
                "{name} must not count as terminal"
            );
        }
        assert!(!external_stream_chunk_has_terminal_event(
            &Bytes::from_static(
                b"data: {\"choices\":[{\"finish_reason\":null,\"delta\":{\"content\":\"x\"}}]}\n\n"
            )
        ));
        // 有些 OpenAI 兼容上游每个分片都带空的 finish_reason / stop_reason，不是流结束。
        for chunk in [
            &b"data: {\"choices\":[{\"finish_reason\":\"\",\"delta\":{\"content\":\"x\"}}]}\n\n"[..],
            &b"data: {\"choices\":[{\"finish_reason\":\" \",\"delta\":{\"content\":\"x\"}}]}\n\n"[..],
            &b"data: {\"choices\":[{\"finish_reason\":0,\"delta\":{\"content\":\"x\"}}]}\n\n"[..],
            &b"data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"\"}}\n\n"[..],
        ] {
            assert!(
                !external_stream_chunk_has_terminal_event(&Bytes::copy_from_slice(chunk)),
                "{}",
                String::from_utf8_lossy(chunk)
            );
        }
        assert!(external_stream_chunk_has_terminal_event(
            &Bytes::from_static(
                b"data: {\"choices\":[{\"finish_reason\":\"stop\",\"delta\":{}}]}\n\n"
            )
        ));
    }

    #[test]
    fn terminal_event_is_found_inside_a_multi_event_chunk() {
        let mut chunk = Vec::new();
        chunk.extend_from_slice(&sse(
            "content_block_stop",
            r#"{"type":"content_block_stop","index":0}"#,
        ));
        chunk.extend_from_slice(&sse(
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":9}}"#,
        ));
        chunk.extend_from_slice(&sse("message_stop", r#"{"type":"message_stop"}"#));
        assert!(external_stream_chunk_has_terminal_event(&Bytes::from(
            chunk
        )));
        // 不完整的尾事件（没有空行分隔）不算。
        assert!(!external_stream_chunk_has_terminal_event(
            &Bytes::from_static(b"event: message_stop\ndata: {\"type\":\"message_stop\"}")
        ));
    }
}

#[cfg(test)]
mod golden_tests;
#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
pub(crate) fn passthrough_test_prepare(
    route: &AccountRouteRequest,
    pool: &Account,
    headers: &HeaderMap,
) -> (Bytes, HeaderMap) {
    (
        account_prepare_request(route, pool).unwrap().body,
        forward_headers(headers, pool, false).unwrap(),
    )
}

pub(crate) fn route_model_rule_allows(rule: &crate::model::routes::RouteRule, model: &str) -> bool {
    let candidates = normalize_account_support_candidates([model]);
    let matched = rule.rules.iter().any(|value| {
        let value = value.trim();
        if value == "*" {
            return true;
        }
        if let Some(prefix) = value.strip_suffix('*').and_then(normalize_routing_model_id) {
            return candidates
                .iter()
                .any(|candidate| split_account_model_suffix(candidate).0.starts_with(&prefix));
        }
        let values = normalize_account_allowlist_models([value]);
        candidates
            .iter()
            .any(|candidate| values.contains(candidate))
    });
    rule.allows_match(matched)
}
fn route_account_allowed(
    config: &AccountsConfig,
    id: u64,
    endpoint: Option<&str>,
    models: Option<&[String]>,
) -> bool {
    let Some(endpoint) = endpoint else {
        return true;
    };
    let Some(route) = config.routes.match_endpoint(endpoint) else {
        return false;
    };
    route.enabled
        && route.account_rule.allows_account(id)
        && models.is_none_or(|models| {
            models
                .iter()
                .any(|model| route.model_rule.allows_model(model))
        })
}

pub(crate) fn normalize_catalog_model(model: &str) -> Option<String> {
    normalize_model_id(model)
}

fn normalize_routing_model_id(model: &str) -> Option<String> {
    let normalized = crate::anthropic::model_capabilities::normalize_model_id(model);
    normalize_model_id(normalized.strip_prefix("anthropic/").unwrap_or(&normalized))
}

fn filter_models_for_route(
    route: &crate::model::routes::RouteConfig,
    endpoint: &str,
    models: Vec<crate::anthropic::types::Model>,
    pools: &[AccountEligibility],
) -> Vec<crate::anthropic::types::Model> {
    models
        .into_iter()
        .filter(|model| {
            model.listed
                && route.enabled
                && route.model_rule.allows_model(&model.id)
                && pools.iter().any(|pool| {
                    pool.enabled
                        && route.account_rule.allows_account(pool.id)
                        && account_eligibility_route_allowed(pool, Some(endpoint))
                        && account_eligibility_matches_supported_models(
                            pool,
                            Some(&normalize_account_support_candidates([model.id.as_str()])),
                        )
                })
        })
        .collect()
}

fn deserialize_proxy_url_update<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(deserializer).map(Some)
}
