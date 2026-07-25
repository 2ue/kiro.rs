//! Kiro API Provider
//!
//! 核心组件，负责与 Kiro API 通信
//! 支持流式和非流式请求
//! 支持多凭据故障转移和重试
//! 支持按凭据级 endpoint 切换不同 Kiro API 端点

use chrono::Utc;
use once_cell::sync::OnceCell;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use reqwest::{Client, Method};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::{Duration, Instant};
use tokio::time::sleep;

use crate::anthropic::inference_attempt_budget::{
    AuxiliaryAttemptBudget, AuxiliaryAttemptBudgetExhausted, AuxiliaryAttemptKind,
    InferenceAttemptBudget, InferenceAttemptKind, InferenceAttemptRejection,
};
use crate::anthropic::model_capabilities::{
    KiroReasoningCapabilityState, intersect_authoritative_reasoning_schemas,
};
use crate::http_client::{
    HttpSendError, ProxyConfig, build_client, execute_with_response_header_timeout,
    response_bytes_with_limit_and_body_timeout, response_text_with_limit_and_body_timeout,
    send_with_response_header_timeout,
};
use crate::kiro::call_trace::{
    KiroCallError, KiroCallFailureKind, KiroCredentialAttempt, McpCallAttributionSink,
    SelectionFailureSummary, summarize_attempts,
};
use crate::kiro::endpoint::{KiroEndpoint, RequestContext, configured_upstream_url};
use crate::kiro::machine_id;
use crate::kiro::model::available_models::{
    KiroAvailableModel, KiroAvailableModelCatalog, KiroAvailableModelsResponse,
};
use crate::kiro::model::credentials::KiroCredentials;
use crate::kiro::protocol::{
    extract_first_profile_arn, is_external_idp_credentials, is_real_profile_arn,
};
use crate::kiro::token_manager::{
    AcquireMode, AutomaticTokenRecoveryOutcome, AuxiliaryConcurrencyKind,
    AuxiliaryConcurrencySaturated, CallContext, CredentialRiskControlReason,
    EXTERNAL_CREDENTIAL_CONTEXT_ID, InFlightKind, InFlightLeaseGuard, LocalPoolRouteState,
    MultiTokenManager, RefreshFailure, TokenRefreshAdmissionRejected, TransientFailureKind,
};
use crate::model::config::{Config, TlsBackend};
use parking_lot::Mutex;

/// 自动模式下单个下游请求共享的 provider 尝试预算，避免随凭据池规模放大上游 RPM。
const DEFAULT_AUTO_RETRY_ATTEMPTS: usize = 3;

/// Global model discovery is an auxiliary operation, not an inference request. Keep its
/// credential fan-out independent from the size of the configured account pool.
const MODEL_DISCOVERY_MAX_CREDENTIAL_ATTEMPTS: usize = 4;
const MODEL_DISCOVERY_MAX_HTTP_SENDS: usize = 20;

#[derive(Debug)]
struct ModelDiscoverySendBudget {
    consumed: usize,
}

impl ModelDiscoverySendBudget {
    fn new() -> Self {
        Self { consumed: 0 }
    }

    fn reserve(&mut self) -> anyhow::Result<()> {
        if self.consumed >= MODEL_DISCOVERY_MAX_HTTP_SENDS {
            anyhow::bail!(
                "model discovery HTTP send budget exhausted ({}/{})",
                self.consumed,
                MODEL_DISCOVERY_MAX_HTTP_SENDS
            );
        }
        self.consumed += 1;
        Ok(())
    }
}

fn merge_model_discovery_catalogs(
    catalogs: Vec<Vec<KiroAvailableModel>>,
    cohort_complete: bool,
) -> Vec<KiroAvailableModel> {
    let mut per_credential = Vec::with_capacity(catalogs.len());
    let mut merged = BTreeMap::<String, KiroAvailableModel>::new();
    for catalog in catalogs {
        let mut by_model = BTreeMap::<String, KiroAvailableModel>::new();
        for mut model in catalog {
            let model_id = model.model_id.trim().to_string();
            if model_id.is_empty() {
                continue;
            }
            model.model_id = model_id.clone();
            if by_model.insert(model_id.clone(), model.clone()).is_some() {
                // Conflicting duplicate entries inside one authoritative catalog cannot establish
                // a safe native-reasoning contract.
                model.additional_model_request_fields_schema = Some(serde_json::Value::Null);
                by_model.insert(model_id.clone(), model.clone());
            }
            merged.entry(model_id).or_insert(model);
        }
        per_credential.push(by_model);
    }

    for (model_id, model) in &mut merged {
        let state = if !cohort_complete {
            KiroReasoningCapabilityState::Unknown
        } else if per_credential
            .iter()
            .any(|catalog| !catalog.contains_key(model_id))
        {
            KiroReasoningCapabilityState::AuthoritativeInvalid
        } else {
            intersect_authoritative_reasoning_schemas(per_credential.iter().map(|catalog| {
                catalog
                    .get(model_id)
                    .and_then(|model| model.additional_model_request_fields_schema.as_ref())
            }))
        };
        model.additional_model_request_fields_schema = match state {
            KiroReasoningCapabilityState::Supported(capability) => Some(capability.to_schema()),
            KiroReasoningCapabilityState::AuthoritativeAbsent => None,
            KiroReasoningCapabilityState::LegacyFallback
            | KiroReasoningCapabilityState::Unknown
            | KiroReasoningCapabilityState::AuthoritativeInvalid => Some(serde_json::Value::Null),
        };
    }
    merged.into_values().collect()
}

/// Failed enterprise profile discovery is best-effort. Keep deterministic upstream failures out
/// of the inference retry loop while still allowing bounded recovery without an operator restart.
const PROFILE_ARN_DISCOVERY_NEGATIVE_BACKOFF_BASE: Duration = Duration::from_secs(5);
const PROFILE_ARN_DISCOVERY_NEGATIVE_BACKOFF_MAX: Duration = Duration::from_secs(60);

/// A short success handoff lets callers that acquired stale contexts before persistence completed
/// observe the leader's result. Normal calls subsequently read the persisted ARN and never touch
/// this state.
const PROFILE_ARN_DISCOVERY_SUCCESS_HANDOFF_TTL: Duration = Duration::from_secs(30);

/// This is process-local coordination state, not a credential registry. Bound it independently
/// from malformed imports or repeated credential replacement.
const PROFILE_ARN_DISCOVERY_MAX_ENTRIES: usize = 2_048;

/// Kiro provider 不设置 reqwest 整请求总超时：流式正文由 Anthropic SSE idle timeout 管控，
/// 请求头和非流式 body 分别由专门的 timeout helper 管控。
const KIRO_CLIENT_TOTAL_TIMEOUT_SECS: u64 = 0;
const KIRO_CLIENT_CACHE_MAX_ENTRIES: usize = 256;
const PROVIDER_DIAGNOSTIC_BODY_MAX_BYTES: usize = 1024 * 1024;
const PROVIDER_AUXILIARY_BODY_MAX_BYTES: usize = 4 * 1024 * 1024;

type ThinkingSignatureRetryBodyBuilder<'a> =
    Box<dyn FnOnce() -> anyhow::Result<String> + Send + 'a>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApiUpstreamFailureKind {
    InvalidRequest,
    Auth,
    RateLimit,
    Quota,
    RiskControl,
    Server,
    Timeout,
    ResponseTooLarge,
    BodyRead,
    Protocol,
    Unknown,
}

impl ApiUpstreamFailureKind {
    fn as_error_type(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::Auth => "auth_error",
            Self::RateLimit => "rate_limit",
            Self::Quota => "quota_exhausted",
            Self::RiskControl => "risk_control",
            Self::Server => "server_error",
            Self::Timeout => "upstream_timeout",
            Self::ResponseTooLarge => "response_too_large",
            Self::BodyRead => "body_read_error",
            Self::Protocol => "protocol_error",
            Self::Unknown => "unknown_upstream_error",
        }
    }

    fn scheduler_reason(self) -> &'static str {
        match self {
            Self::InvalidRequest => "api_invalid_request",
            Self::Auth => "api_auth_error",
            Self::RateLimit => "api_rate_limit",
            Self::Quota => "api_quota_exhausted",
            Self::RiskControl => "api_risk_control",
            Self::Server => "api_server_error",
            Self::Timeout => "api_timeout",
            Self::ResponseTooLarge => "api_response_too_large",
            Self::BodyRead => "api_body_read_error",
            Self::Protocol => "api_protocol_error",
            Self::Unknown => "api_unknown_upstream_error",
        }
    }

    fn transient_failure_kind(self) -> Option<TransientFailureKind> {
        match self {
            Self::RateLimit => Some(TransientFailureKind::RateLimit),
            Self::Server => Some(TransientFailureKind::Server),
            Self::Timeout | Self::BodyRead => Some(TransientFailureKind::Network),
            Self::ResponseTooLarge | Self::Protocol => Some(TransientFailureKind::Protocol),
            Self::InvalidRequest | Self::Auth | Self::Quota | Self::RiskControl | Self::Unknown => {
                None
            }
        }
    }

    fn is_retryable(self) -> bool {
        self.transient_failure_kind().is_some()
    }

    fn effective_public_status(self, upstream_status: reqwest::StatusCode) -> u16 {
        match self {
            Self::RateLimit => 429,
            Self::InvalidRequest => 400,
            _ => upstream_status.as_u16(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpstreamContentKind {
    EventStream,
    Json,
    Other,
    Missing,
}

impl UpstreamContentKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::EventStream => "eventstream",
            Self::Json => "json",
            Self::Other => "other",
            Self::Missing => "missing",
        }
    }
}

struct ApiUpstreamBody {
    text: String,
    bytes: usize,
}

#[derive(Debug, Clone, Copy)]
struct ApiUpstreamBodyReadFailure {
    kind: ApiUpstreamFailureKind,
    body_bytes: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpCallFailureKind {
    Scheduler,
    InvalidRequest,
    RateLimit,
    Timeout,
    ResponseTooLarge,
    BodyRead,
    Protocol,
    Upstream,
    AttemptLimit,
    AuxiliaryAttemptLimit,
    AuxiliaryConcurrency,
}

impl McpCallFailureKind {
    fn as_error_type(self) -> &'static str {
        match self {
            Self::Scheduler => "mcp_scheduler_unavailable",
            Self::InvalidRequest => "mcp_invalid_request",
            Self::RateLimit => "mcp_rate_limit",
            Self::Timeout => "mcp_timeout",
            Self::ResponseTooLarge => "mcp_response_too_large",
            Self::BodyRead => "mcp_body_read",
            Self::Protocol => "mcp_protocol",
            Self::Upstream => "mcp_upstream",
            Self::AttemptLimit => "mcp_attempt_limit",
            Self::AuxiliaryAttemptLimit => "mcp_auxiliary_attempt_limit",
            Self::AuxiliaryConcurrency => "mcp_auxiliary_concurrency",
        }
    }

    fn scheduler_reason(self) -> &'static str {
        match self {
            Self::Scheduler => "scheduler_unavailable",
            Self::InvalidRequest => "invalid_request",
            Self::RateLimit => "rate_limit",
            Self::Timeout => "body_timeout",
            Self::ResponseTooLarge => "response_too_large",
            Self::BodyRead => "body_read",
            Self::Protocol => "protocol_failure",
            Self::Upstream => "upstream_error",
            Self::AttemptLimit => "attempt_limit",
            Self::AuxiliaryAttemptLimit => "auxiliary_attempt_limit",
            Self::AuxiliaryConcurrency => "auxiliary_concurrency",
        }
    }

    fn should_retry_mcp_with_alternate_credential(self) -> bool {
        matches!(
            self,
            Self::RateLimit | Self::Timeout | Self::BodyRead | Self::Upstream
        )
    }
}

#[derive(Debug, Clone, Default)]
pub struct McpCallAttribution {
    pub credential_id: Option<u64>,
    pub credential_label: Option<String>,
    pub attempts: Vec<KiroCredentialAttempt>,
    pub selection_failure: Option<SelectionFailureSummary>,
}

#[derive(Debug)]
struct McpCallError {
    kind: McpCallFailureKind,
    message: String,
    attribution: McpCallAttribution,
}

impl std::fmt::Display for McpCallError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for McpCallError {}

fn effective_payload_guard_limit_for_logging(config: &Config) -> usize {
    const MIN_EFFECTIVE_LIMIT_BYTES: usize = 64 * 1024;
    let max_bytes = config.payload_guard_max_bytes;
    if max_bytes == 0 || config.payload_guard_safety_margin_bytes == 0 {
        return max_bytes;
    }
    if max_bytes <= MIN_EFFECTIVE_LIMIT_BYTES {
        return max_bytes;
    }
    let margin = config
        .payload_guard_safety_margin_bytes
        .min(max_bytes.saturating_sub(MIN_EFFECTIVE_LIMIT_BYTES));
    max_bytes.saturating_sub(margin)
}

fn should_log_upstream_body_size_at_info(body_bytes: usize, config: &Config) -> bool {
    let payload_guard_limit = effective_payload_guard_limit_for_logging(config);
    let near_payload_guard_limit =
        payload_guard_limit > 0 && body_bytes > payload_guard_limit.saturating_mul(70) / 100;
    let compression_enabled =
        config.compression.enabled && config.compression.whitespace_compression;

    near_payload_guard_limit || compression_enabled
}

#[derive(Debug, Clone, Copy)]
struct ProfileArnDiscoveryPolicy {
    negative_backoff_base: Duration,
    negative_backoff_max: Duration,
    success_handoff_ttl: Duration,
    max_entries: usize,
}

impl Default for ProfileArnDiscoveryPolicy {
    fn default() -> Self {
        Self {
            negative_backoff_base: PROFILE_ARN_DISCOVERY_NEGATIVE_BACKOFF_BASE,
            negative_backoff_max: PROFILE_ARN_DISCOVERY_NEGATIVE_BACKOFF_MAX,
            success_handoff_ttl: PROFILE_ARN_DISCOVERY_SUCCESS_HANDOFF_TTL,
            max_entries: PROFILE_ARN_DISCOVERY_MAX_ENTRIES,
        }
    }
}

impl ProfileArnDiscoveryPolicy {
    fn negative_backoff(self, failures: u32) -> Duration {
        let exponent = failures.saturating_sub(1).min(10);
        self.negative_backoff_base
            .saturating_mul(1_u32 << exponent)
            .min(self.negative_backoff_max)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct ProfileArnDiscoveryKey {
    credential_id: u64,
    identity: [u8; 32],
}

enum ProfileArnDiscoveryEntryState {
    Idle {
        previous_failures: u32,
    },
    InFlight {
        previous_failures: u32,
    },
    Negative {
        failures: u32,
        retry_at: Instant,
    },
    Resolved {
        profile_arn: String,
        expires_at: Instant,
    },
}

struct ProfileArnDiscoveryEntry {
    gate: tokio::sync::Mutex<()>,
    state: Mutex<ProfileArnDiscoveryEntryState>,
    last_used: AtomicU64,
}

impl ProfileArnDiscoveryEntry {
    fn new(last_used: u64) -> Self {
        Self {
            gate: tokio::sync::Mutex::new(()),
            state: Mutex::new(ProfileArnDiscoveryEntryState::Idle {
                previous_failures: 0,
            }),
            last_used: AtomicU64::new(last_used),
        }
    }
}

#[derive(Default)]
struct ProfileArnDiscoveryMetrics {
    upstream_attempts: AtomicU64,
    successes: AtomicU64,
    negative_results: AtomicU64,
    coalesced_waiters: AtomicU64,
    negative_cache_suppressions: AtomicU64,
    state_capacity_suppressions: AtomicU64,
    request_budget_suppressions: AtomicU64,
    concurrency_suppressions: AtomicU64,
}

/// Process-local accounting for the `ListAvailableProfiles` auxiliary channel. These counters are
/// intentionally separate from request-scoped inference attempts.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ProfileArnDiscoveryMetricsSnapshot {
    pub upstream_attempts: u64,
    pub successes: u64,
    pub negative_results: u64,
    pub coalesced_waiters: u64,
    pub negative_cache_suppressions: u64,
    pub state_capacity_suppressions: u64,
    pub request_budget_suppressions: u64,
    pub concurrency_suppressions: u64,
}

#[cfg(test)]
impl ProfileArnDiscoveryMetrics {
    fn snapshot(&self) -> ProfileArnDiscoveryMetricsSnapshot {
        ProfileArnDiscoveryMetricsSnapshot {
            upstream_attempts: self.upstream_attempts.load(Ordering::Relaxed),
            successes: self.successes.load(Ordering::Relaxed),
            negative_results: self.negative_results.load(Ordering::Relaxed),
            coalesced_waiters: self.coalesced_waiters.load(Ordering::Relaxed),
            negative_cache_suppressions: self.negative_cache_suppressions.load(Ordering::Relaxed),
            state_capacity_suppressions: self.state_capacity_suppressions.load(Ordering::Relaxed),
            request_budget_suppressions: self.request_budget_suppressions.load(Ordering::Relaxed),
            concurrency_suppressions: self.concurrency_suppressions.load(Ordering::Relaxed),
        }
    }
}

/// Kiro API Provider
///
/// 核心组件，负责与 Kiro API 通信
/// 支持多凭据故障转移和重试机制
/// 按凭据 `endpoint` 字段选择 [`KiroEndpoint`] 实现
pub struct KiroProvider {
    token_manager: Arc<MultiTokenManager>,
    /// 全局代理配置（用于凭据无自定义代理时的回退）
    global_proxy: Option<ProxyConfig>,
    /// Client 缓存：key = effective proxy config, value = single-flight client cell
    /// 不同代理配置的凭据使用不同的 Client，共享相同代理的凭据复用 Client
    client_cache: Mutex<ProviderClientCache>,
    client_cache_builds: AtomicU64,
    /// TLS 后端配置
    tls_backend: TlsBackend,
    /// 端点实现注册表（key: endpoint 名称）
    endpoints: HashMap<String, Arc<dyn KiroEndpoint>>,
    /// 默认端点名称（凭据未指定 endpoint 时使用）
    default_endpoint: String,
    /// Prevent startup and Admin model discovery from scanning the account pool concurrently.
    model_discovery_in_progress: AtomicBool,
    /// Rotates representative accounts within a stable capability cohort across bounded syncs.
    model_discovery_round: AtomicU64,
    /// Per-credential auxiliary profile discovery coordination. The map is bounded and stores
    /// only SHA-256 identities, never raw credential secrets.
    profile_arn_discovery_entries:
        Mutex<HashMap<ProfileArnDiscoveryKey, Arc<ProfileArnDiscoveryEntry>>>,
    profile_arn_discovery_clock: AtomicU64,
    profile_arn_discovery_policy: ProfileArnDiscoveryPolicy,
    profile_arn_discovery_metrics: ProfileArnDiscoveryMetrics,
}

struct ProviderClientCacheEntry {
    client: Arc<OnceCell<Client>>,
    last_used: u64,
}

struct ProviderClientCache {
    entries: HashMap<Option<ProxyConfig>, ProviderClientCacheEntry>,
    clock: u64,
}

struct ModelDiscoveryRunGuard<'a> {
    flag: &'a AtomicBool,
}

impl<'a> ModelDiscoveryRunGuard<'a> {
    fn acquire(flag: &'a AtomicBool) -> anyhow::Result<Self> {
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| anyhow::anyhow!("Kiro model discovery is already in progress"))?;
        Ok(Self { flag })
    }
}

impl Drop for ModelDiscoveryRunGuard<'_> {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Release);
    }
}

pub struct KiroApiResponse {
    response: reqwest::Response,
    completion: KiroApiCompletion,
}

pub struct McpCallResponse {
    response: reqwest::Response,
    completion: McpCallCompletion,
}

impl std::fmt::Debug for McpCallResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpCallResponse")
            .field("status", &self.response.status())
            .field("credential_id", &self.completion.credential_id)
            .field("attempts", &self.completion.attempts())
            .finish()
    }
}

pub struct McpCallCompletion {
    token_manager: Arc<MultiTokenManager>,
    credential_id: u64,
    credential_label: String,
    in_flight_lease: Mutex<Option<InFlightLeaseGuard>>,
    attempts: Mutex<Vec<KiroCredentialAttempt>>,
    attempt: usize,
    status: reqwest::StatusCode,
    reported: AtomicBool,
    started_at: Instant,
    attribution_sink: Arc<McpCallAttributionSink>,
}

impl McpCallCompletion {
    #[cfg(test)]
    fn new(
        token_manager: Arc<MultiTokenManager>,
        credential_id: u64,
        credential_label: String,
        in_flight_lease: Option<InFlightLeaseGuard>,
        attempts: Vec<KiroCredentialAttempt>,
        attempt: usize,
        status: reqwest::StatusCode,
        started_at: Instant,
    ) -> Self {
        Self::new_with_attribution_sink(
            token_manager,
            credential_id,
            credential_label,
            in_flight_lease,
            attempts,
            attempt,
            status,
            started_at,
            Arc::new(McpCallAttributionSink::default()),
        )
    }

    fn new_with_attribution_sink(
        token_manager: Arc<MultiTokenManager>,
        credential_id: u64,
        credential_label: String,
        in_flight_lease: Option<InFlightLeaseGuard>,
        attempts: Vec<KiroCredentialAttempt>,
        attempt: usize,
        status: reqwest::StatusCode,
        started_at: Instant,
        attribution_sink: Arc<McpCallAttributionSink>,
    ) -> Self {
        Self {
            token_manager,
            credential_id,
            credential_label,
            in_flight_lease: Mutex::new(in_flight_lease),
            attempts: Mutex::new(attempts),
            attempt,
            status,
            reported: AtomicBool::new(false),
            started_at,
            attribution_sink,
        }
    }

    pub fn report_success(&self) {
        if self.reported.swap(true, Ordering::AcqRel) {
            return;
        }
        let attempts = {
            let mut attempts = self.attempts.lock();
            KiroProvider::push_attempt(
                &mut attempts,
                self.attempt,
                self.credential_id,
                &self.credential_label,
                Some(self.status),
                "success",
                None,
                None,
                self.started_at,
                None,
            );
            attempts.clone()
        };
        self.attribution_sink.replace(
            Some(self.credential_id),
            Some(self.credential_label.clone()),
            attempts,
        );
        if self.in_flight_lease.lock().take().is_some() {
            self.token_manager.report_success_with_latency(
                self.credential_id,
                None,
                Some(self.started_at.elapsed()),
            );
        }
    }

    pub fn report_failure(&self, kind: McpCallFailureKind) {
        if self.reported.swap(true, Ordering::AcqRel) {
            return;
        }
        let attempts = {
            let mut attempts = self.attempts.lock();
            KiroProvider::push_attempt(
                &mut attempts,
                self.attempt,
                self.credential_id,
                &self.credential_label,
                Some(self.status),
                "fail",
                Some(kind.as_error_type()),
                Some(kind.scheduler_reason().to_string()),
                self.started_at,
                None,
            );
            attempts.clone()
        };
        self.attribution_sink.replace(
            Some(self.credential_id),
            Some(self.credential_label.clone()),
            attempts,
        );

        // MCP/WebSearch is an auxiliary path. Its failures often come from the MCP/search
        // service, response-shape compatibility, or client cancellation after the model request
        // has already been admitted. Applying a global credential cooldown here can incorrectly
        // remove otherwise healthy model credentials from the main scheduler and produce
        // local_all_disabled/local_error_no_fallback waves. The usage record and attribution
        // still keep the MCP failure visible; request admission and auxiliary attempt budgets
        // bound retry pressure without poisoning core model account health.
        self.in_flight_lease.lock().take();
    }

    pub fn attribution(&self) -> McpCallAttribution {
        McpCallAttribution {
            credential_id: Some(self.credential_id),
            credential_label: Some(self.credential_label.clone()),
            attempts: self.attempts.lock().clone(),
            selection_failure: None,
        }
    }

    pub fn attempts(&self) -> Vec<KiroCredentialAttempt> {
        self.attempts.lock().clone()
    }
}

impl Drop for McpCallCompletion {
    fn drop(&mut self) {
        if !self.reported.load(Ordering::Acquire) {
            self.attribution_sink.snapshot_for_client_drop();
            self.in_flight_lease.lock().take();
        }
    }
}

impl McpCallResponse {
    pub fn into_parts(self) -> (reqwest::Response, McpCallCompletion) {
        (self.response, self.completion)
    }
}

struct ApiCallResponse {
    response: reqwest::Response,
    credential_id: u64,
    in_flight_lease: Option<InFlightLeaseGuard>,
    session_id: Option<String>,
    model: Option<String>,
    sticky_bound: bool,
    fallback_from_sticky: bool,
    attempts: Vec<KiroCredentialAttempt>,
    started_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CredentialAuthFailureDecision {
    TokenRecoveryRetry,
    Retry { excluded_current: bool },
    Exhausted,
}

/// 非流式调用完成上报器。
///
/// 非流式响应头返回后，body 读取和事件解析仍可能失败或被取消。
/// 这个 guard 用 Drop 兜底释放并发槽，避免调用链中途退出导致凭据长期不可调度。
pub struct KiroApiCompletion {
    token_manager: Arc<MultiTokenManager>,
    credential_id: u64,
    in_flight_lease: Mutex<Option<InFlightLeaseGuard>>,
    session_id: Option<String>,
    model: Option<String>,
    sticky_bound: bool,
    fallback_from_sticky: bool,
    attempts: Vec<KiroCredentialAttempt>,
    reported: AtomicBool,
    started_at: Instant,
}

impl KiroApiCompletion {
    fn new(
        token_manager: Arc<MultiTokenManager>,
        credential_id: u64,
        in_flight_lease: Option<InFlightLeaseGuard>,
        session_id: Option<String>,
        model: Option<String>,
        sticky_bound: bool,
        fallback_from_sticky: bool,
        attempts: Vec<KiroCredentialAttempt>,
        started_at: Instant,
    ) -> Self {
        Self {
            token_manager,
            credential_id,
            in_flight_lease: Mutex::new(in_flight_lease),
            session_id,
            model,
            sticky_bound,
            fallback_from_sticky,
            attempts,
            reported: AtomicBool::new(false),
            started_at,
        }
    }

    pub fn report_success(&self) {
        if self.reported.swap(true, Ordering::AcqRel) {
            return;
        }
        if self.in_flight_lease.lock().take().is_some() {
            self.token_manager.report_success_for_session_with_latency(
                self.credential_id,
                self.model.as_deref(),
                self.session_id.as_deref(),
                Some(self.started_at.elapsed()),
            );
        }
    }

    pub fn release(&self) {
        if self.reported.swap(true, Ordering::AcqRel) {
            return;
        }
        self.in_flight_lease.lock().take();
    }

    pub fn credential_id(&self) -> u64 {
        self.credential_id
    }

    pub fn sticky_bound(&self) -> bool {
        self.sticky_bound
    }

    pub fn fallback_from_sticky(&self) -> bool {
        self.fallback_from_sticky
    }

    pub fn attempts(&self) -> &[KiroCredentialAttempt] {
        &self.attempts
    }
}

impl Drop for KiroApiCompletion {
    fn drop(&mut self) {
        if self.reported.load(Ordering::Acquire) {
            return;
        }
        self.release();
    }
}

impl KiroApiResponse {
    pub fn credential_id(&self) -> u64 {
        self.completion.credential_id()
    }

    pub fn sticky_bound(&self) -> bool {
        self.completion.sticky_bound()
    }

    pub fn fallback_from_sticky(&self) -> bool {
        self.completion.fallback_from_sticky()
    }

    pub fn attempts(&self) -> &[KiroCredentialAttempt] {
        self.completion.attempts()
    }

    pub fn into_parts(self) -> (reqwest::Response, KiroApiCompletion) {
        (self.response, self.completion)
    }
}

/// 流式调用完成上报器。
///
/// Provider 只能确认上游返回了成功响应头；流式 body 是否完整消费需要
/// SSE 处理链路在 EOF、读错误或 idle timeout 时回报。
pub struct KiroStreamCompletion {
    token_manager: Arc<MultiTokenManager>,
    credential_id: u64,
    in_flight_lease: Mutex<Option<InFlightLeaseGuard>>,
    session_id: Option<String>,
    model: Option<String>,
    sticky_bound: bool,
    fallback_from_sticky: bool,
    attempts: Vec<KiroCredentialAttempt>,
    reported: AtomicBool,
    started_at: Instant,
}

impl KiroStreamCompletion {
    fn new(
        token_manager: Arc<MultiTokenManager>,
        credential_id: u64,
        in_flight_lease: Option<InFlightLeaseGuard>,
        session_id: Option<String>,
        model: Option<String>,
        sticky_bound: bool,
        fallback_from_sticky: bool,
        attempts: Vec<KiroCredentialAttempt>,
        started_at: Instant,
    ) -> Self {
        Self {
            token_manager,
            credential_id,
            in_flight_lease: Mutex::new(in_flight_lease),
            session_id,
            model,
            sticky_bound,
            fallback_from_sticky,
            attempts,
            reported: AtomicBool::new(false),
            started_at,
        }
    }

    /// 上游流正常 EOF 后调用，计入成功并清理 sticky 软失败计数。
    pub fn report_success(&self) {
        if self.reported.swap(true, Ordering::AcqRel) {
            return;
        }
        self.token_manager.report_success_for_session_with_latency(
            self.credential_id,
            self.model.as_deref(),
            self.session_id.as_deref(),
            Some(self.started_at.elapsed()),
        );
        self.in_flight_lease.lock().take();
    }

    /// 上游流中断、idle timeout 或上游错误事件时调用。
    ///
    /// 这里不调用 `report_failure`，避免瞬态流读取问题直接禁用账号。
    pub fn report_soft_failure(&self) {
        if self.reported.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(session_id) = self.session_id.as_deref() {
            self.token_manager
                .record_session_soft_failure(session_id, self.credential_id);
        }
        self.in_flight_lease.lock().take();
    }

    /// 上游流读取错误、idle timeout 或上游错误事件时调用，并让调度器短暂避开该凭据。
    pub fn report_upstream_stream_failure(&self, reason: impl Into<String>) {
        if self.reported.swap(true, Ordering::AcqRel) {
            return;
        }
        let reason = reason.into();
        if let Err(err) = self.token_manager.report_transient_failure_kind(
            self.credential_id,
            self.model.as_deref(),
            TransientFailureKind::Stream,
            None,
            format!("stream_error {}", reason),
        ) {
            tracing::warn!(
                credential_id = self.credential_id,
                "记录上游流式失败冷却失败: {}",
                err
            );
        }
        if let Some(session_id) = self.session_id.as_deref() {
            self.token_manager
                .record_session_soft_failure(session_id, self.credential_id);
        }
        self.in_flight_lease.lock().take();
    }

    pub fn touch(&self) {
        if let Some(lease) = self.in_flight_lease.lock().as_ref() {
            lease.touch();
        }
    }

    pub fn credential_id(&self) -> u64 {
        self.credential_id
    }

    pub fn sticky_bound(&self) -> bool {
        self.sticky_bound
    }

    pub fn fallback_from_sticky(&self) -> bool {
        self.fallback_from_sticky
    }

    pub fn attempts(&self) -> &[KiroCredentialAttempt] {
        &self.attempts
    }
}

impl Drop for KiroStreamCompletion {
    fn drop(&mut self) {
        if self.reported.load(Ordering::Acquire) {
            return;
        }
        self.report_soft_failure();
    }
}

pub struct KiroStreamResponse {
    response: reqwest::Response,
    completion: KiroStreamCompletion,
}

impl KiroStreamResponse {
    pub fn into_parts(self) -> (reqwest::Response, KiroStreamCompletion) {
        (self.response, self.completion)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex as StdMutex};
    use std::time::Instant;

    use axum::body::Bytes;
    use axum::extract::{Query, State};
    use axum::http::{HeaderMap as AxumHeaderMap, StatusCode};
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use chrono::{Duration, Utc};
    use futures::StreamExt;
    use sha2::{Digest, Sha256};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tracing::instrument::WithSubscriber;

    use super::{
        AuxiliaryConcurrencySaturated, CredentialAuthFailureDecision, CredentialRiskControlReason,
        KIRO_CLIENT_CACHE_MAX_ENTRIES, KiroAvailableModel, KiroProvider, KiroStreamCompletion,
        MODEL_DISCOVERY_MAX_CREDENTIAL_ATTEMPTS, MODEL_DISCOVERY_MAX_HTTP_SENDS, McpCallCompletion,
        McpCallFailureKind, PROVIDER_DIAGNOSTIC_BODY_MAX_BYTES, ProfileArnDiscoveryPolicy,
    };
    use crate::anthropic::inference_attempt_budget::{
        AuxiliaryAttemptBudget, AuxiliaryAttemptKind, InferenceAttemptBudget, InferenceAttemptKind,
    };
    use crate::kiro::call_trace::{
        AccountRejectReason, KiroCallFailureKind, SelectionFailureStage,
    };
    use crate::kiro::endpoint::{CliEndpoint, IdeEndpoint, KiroEndpoint};
    use crate::kiro::model::credentials::KiroCredentials;
    use crate::kiro::token_manager::{AcquireMode, AuxiliaryConcurrencyKind, MultiTokenManager};
    use crate::model::config::Config;

    #[derive(Clone, Default)]
    struct FakeBadRequestState {
        hits: Arc<StdMutex<HashMap<String, usize>>>,
        signature_requests: Arc<StdMutex<HashMap<String, Vec<CapturedSignatureRequest>>>>,
        total_hits: Arc<AtomicUsize>,
        mcp_hits: Arc<AtomicUsize>,
        oauth_refresh_hits: Arc<AtomicUsize>,
        profile_active: Arc<AtomicUsize>,
        profile_max_active: Arc<AtomicUsize>,
        auxiliary_marker: Arc<StdMutex<Option<String>>>,
    }

    #[derive(Clone, Debug)]
    struct CapturedSignatureRequest {
        authorization: Option<String>,
        body: serde_json::Value,
    }

    impl FakeBadRequestState {
        fn scenario_hits(&self, scenario: &str) -> usize {
            self.hits
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(scenario)
                .copied()
                .unwrap_or(0)
        }

        fn profile_hits(&self, token: &str) -> usize {
            self.scenario_hits(&format!("profile_token:{token}"))
        }

        fn oauth_refresh_hits(&self) -> usize {
            self.oauth_refresh_hits.load(Ordering::Relaxed)
        }

        fn signature_requests(&self, scenario: &str) -> Vec<CapturedSignatureRequest> {
            self.signature_requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(scenario)
                .cloned()
                .unwrap_or_default()
        }

        fn reset_profile_concurrency(&self) {
            assert_eq!(self.profile_active.load(Ordering::SeqCst), 0);
            self.profile_max_active.store(0, Ordering::SeqCst);
        }

        fn profile_max_active(&self) -> usize {
            self.profile_max_active.load(Ordering::SeqCst)
        }

        fn set_auxiliary_marker(&self, marker: impl Into<String>) {
            *self
                .auxiliary_marker
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(marker.into());
        }

        fn auxiliary_marker(&self) -> Option<String> {
            self.auxiliary_marker
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }
    }

    struct FakeProfileActiveGuard(Arc<AtomicUsize>);

    impl Drop for FakeProfileActiveGuard {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    struct FakeBadRequestServer {
        base_url: String,
        state: FakeBadRequestState,
        task: tokio::task::JoinHandle<()>,
    }

    #[derive(Clone, Debug)]
    struct CapturedProviderBody {
        path: String,
        content_type: String,
        len: usize,
        sha256: String,
        bytes: Vec<u8>,
    }

    struct FakeProviderBodyCaptureServer {
        base_url: String,
        captures: Arc<StdMutex<Vec<CapturedProviderBody>>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl FakeProviderBodyCaptureServer {
        async fn start() -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind fake provider body capture upstream");
            let address = listener.local_addr().expect("body capture address");
            let captures = Arc::new(StdMutex::new(Vec::new()));
            let server_captures = captures.clone();
            let task = tokio::spawn(async move {
                loop {
                    let (mut socket, _) = listener
                        .accept()
                        .await
                        .expect("accept fake provider body capture request");
                    let captures = server_captures.clone();
                    tokio::spawn(async move {
                        let mut received = Vec::with_capacity(16 * 1024);
                        let mut buffer = [0_u8; 64 * 1024];
                        let header_end = loop {
                            let count = socket
                                .read(&mut buffer)
                                .await
                                .expect("read fake provider request headers");
                            assert!(count > 0, "provider closed before request headers");
                            received.extend_from_slice(&buffer[..count]);
                            assert!(
                                received.len() <= 128 * 1024,
                                "provider request headers exceeded test bound"
                            );
                            if let Some(index) =
                                received.windows(4).position(|window| window == b"\r\n\r\n")
                            {
                                break index + 4;
                            }
                        };

                        let header_text = std::str::from_utf8(&received[..header_end])
                            .expect("provider request headers are UTF-8");
                        let mut lines = header_text.split("\r\n");
                        let request_line = lines.next().expect("provider request line");
                        let path = request_line
                            .split_ascii_whitespace()
                            .nth(1)
                            .expect("provider request path")
                            .to_string();
                        let mut content_length = None;
                        let mut content_type = String::new();
                        for line in lines {
                            let Some((name, value)) = line.split_once(':') else {
                                continue;
                            };
                            if name.eq_ignore_ascii_case("content-length") {
                                content_length = Some(
                                    value
                                        .trim()
                                        .parse::<usize>()
                                        .expect("numeric provider content length"),
                                );
                            } else if name.eq_ignore_ascii_case("content-type") {
                                content_type = value.trim().to_string();
                            }
                        }
                        let content_length =
                            content_length.expect("owned String request must have Content-Length");
                        let mut hasher = Sha256::new();
                        let mut body_prefix = received.split_off(header_end);
                        assert!(body_prefix.len() <= content_length);
                        hasher.update(&body_prefix);
                        let mut remaining = content_length - body_prefix.len();
                        let retain_bytes = content_length <= 256 * 1024;
                        if !retain_bytes {
                            body_prefix.clear();
                        }
                        while remaining > 0 {
                            let read_limit = remaining.min(buffer.len());
                            let count = socket
                                .read(&mut buffer[..read_limit])
                                .await
                                .expect("read fake provider request body");
                            assert!(count > 0, "provider closed before Content-Length bytes");
                            hasher.update(&buffer[..count]);
                            if retain_bytes {
                                body_prefix.extend_from_slice(&buffer[..count]);
                            }
                            remaining -= count;
                        }
                        captures
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .push(CapturedProviderBody {
                                path,
                                content_type,
                                len: content_length,
                                sha256: hex::encode(hasher.finalize()),
                                bytes: body_prefix,
                            });
                        socket
                            .write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Type: application/vnd.amazon.eventstream\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            )
                            .await
                            .expect("write fake provider response");
                    });
                }
            });
            Self {
                base_url: format!("http://{address}"),
                captures,
                task,
            }
        }

        fn capture(&self, index: usize) -> CapturedProviderBody {
            self.captures
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(index)
                .cloned()
                .unwrap_or_else(|| panic!("missing provider body capture {index}"))
        }
    }

    impl Drop for FakeProviderBodyCaptureServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    #[derive(Clone, Default)]
    struct CapturedProviderLogs(Arc<StdMutex<Vec<u8>>>);

    struct CapturedProviderLogWriter(Arc<StdMutex<Vec<u8>>>);

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedProviderLogs {
        type Writer = CapturedProviderLogWriter;

        fn make_writer(&'a self) -> Self::Writer {
            CapturedProviderLogWriter(self.0.clone())
        }
    }

    impl std::io::Write for CapturedProviderLogWriter {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl CapturedProviderLogs {
        fn snapshot(&self) -> String {
            String::from_utf8_lossy(
                &self
                    .0
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
            )
            .into_owned()
        }
    }

    impl FakeBadRequestServer {
        async fn start() -> Self {
            let state = FakeBadRequestState::default();
            let app = Router::new()
                .route(
                    "/generateAssistantResponse",
                    post(fake_bad_request_response),
                )
                .route("/ListAvailableModels", get(fake_model_discovery_failure))
                .route(
                    "/ListAvailableProfiles",
                    post(fake_profile_discovery_success),
                )
                .route("/mcp", post(fake_mcp_server_error))
                .route("/oauth-refresh", post(fake_oauth_refresh))
                .with_state(state.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind fake bad-request upstream");
            let address = listener.local_addr().expect("fake upstream address");
            let task = tokio::spawn(async move {
                axum::serve(listener, app)
                    .await
                    .expect("serve fake bad-request upstream");
            });
            Self {
                base_url: format!("http://{address}"),
                state,
                task,
            }
        }
    }

    impl Drop for FakeBadRequestServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    struct FakeSignatureTransportServer {
        base_url: String,
        task: tokio::task::JoinHandle<()>,
    }

    impl FakeSignatureTransportServer {
        async fn start() -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind thinking signature transport server");
            let address = listener
                .local_addr()
                .expect("thinking signature transport server address");
            let task = tokio::spawn(async move {
                for request_index in 0..2 {
                    let (mut socket, _) = listener
                        .accept()
                        .await
                        .expect("accept thinking signature transport request");
                    let mut received = Vec::with_capacity(8 * 1024);
                    let mut buffer = [0_u8; 16 * 1024];
                    let header_end = loop {
                        let count = socket
                            .read(&mut buffer)
                            .await
                            .expect("read thinking signature transport headers");
                        assert!(count > 0, "request closed before transport headers");
                        received.extend_from_slice(&buffer[..count]);
                        if let Some(index) =
                            received.windows(4).position(|window| window == b"\r\n\r\n")
                        {
                            break index + 4;
                        }
                    };
                    let headers = std::str::from_utf8(&received[..header_end])
                        .expect("thinking signature transport headers are UTF-8");
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    let mut remaining = content_length.saturating_sub(received.len() - header_end);
                    while remaining > 0 {
                        let read_len = remaining.min(buffer.len());
                        let count = socket
                            .read(&mut buffer[..read_len])
                            .await
                            .expect("read thinking signature transport body");
                        assert!(count > 0, "request closed before transport body");
                        remaining -= count;
                    }

                    if request_index == 0 {
                        let response_body = r#"{"reason":"THINKING_SIGNATURE_INVALID"}"#;
                        let response = format!(
                            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            response_body.len(),
                            response_body
                        );
                        socket
                            .write_all(response.as_bytes())
                            .await
                            .expect("write thinking signature invalid response");
                    }
                    // The second connection is intentionally closed without response headers.
                }
            });
            Self {
                base_url: format!("http://{address}"),
                task,
            }
        }
    }

    impl Drop for FakeSignatureTransportServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn fake_bad_request_response(
        State(state): State<FakeBadRequestState>,
        headers: AxumHeaderMap,
        body: Bytes,
    ) -> axum::response::Response {
        let request = serde_json::from_slice::<serde_json::Value>(&body).ok();
        let scenario = request
            .as_ref()
            .and_then(|value| value.get("testScenario"))
            .and_then(|scenario| scenario.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| "unknown".to_string());
        let secret_marker = request
            .as_ref()
            .and_then(|value| value.get("secretMarker"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("PRIVATE_PROVIDER_RESPONSE_MARKER")
            .to_string();
        let has_history_reasoning_content = request
            .as_ref()
            .and_then(|value| value.pointer("/conversationState/history"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|history| {
                history.iter().any(|message| {
                    message
                        .pointer("/assistantResponseMessage/reasoningContent")
                        .is_some()
                })
            });
        state.total_hits.fetch_add(1, Ordering::Relaxed);
        {
            let mut hits = state
                .hits
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let hit = hits.entry(scenario.clone()).or_insert(0);
            *hit += 1;
        }
        if scenario.starts_with("thinking_signature_") {
            let authorization = headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            state
                .signature_requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .entry(scenario.clone())
                .or_default()
                .push(CapturedSignatureRequest {
                    authorization,
                    body: request.clone().unwrap_or(serde_json::Value::Null),
                });
        }

        match scenario.as_str() {
            "provider_header_timeout" => {
                tokio::time::sleep(std::time::Duration::from_millis(1_500)).await;
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"message": secret_marker})),
                )
                    .into_response();
            }
            "provider_error_content_length_over_limit" => {
                let mut response_body = secret_marker.into_bytes();
                response_body.resize(PROVIDER_DIAGNOSTIC_BODY_MAX_BYTES + 1, b'x');
                return axum::response::Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .header(
                        axum::http::header::CONTENT_LENGTH,
                        response_body.len().to_string(),
                    )
                    .body(axum::body::Body::from(response_body))
                    .expect("declared over-limit provider response");
            }
            "provider_error_chunked_over_limit" => {
                let first = Bytes::from(secret_marker.into_bytes());
                let chunks = futures::stream::once(async move { Ok::<_, std::io::Error>(first) })
                    .chain(futures::stream::iter((0..129).map(|_| {
                        Ok::<_, std::io::Error>(Bytes::from(vec![b'x'; 8 * 1024]))
                    })))
                    .chain(futures::stream::pending());
                return axum::response::Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from_stream(chunks))
                    .expect("chunked over-limit provider response");
            }
            "provider_body_timeout" => {
                let chunks = futures::stream::once(async move {
                    Ok::<_, std::io::Error>(Bytes::from(secret_marker.into_bytes()))
                })
                .chain(futures::stream::pending());
                return axum::response::Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from_stream(chunks))
                    .expect("stalled provider response");
            }
            "provider_body_disconnect" => {
                let first = Bytes::from(secret_marker.into_bytes());
                let chunks = futures::stream::once(async move { Ok::<_, std::io::Error>(first) })
                    .chain(futures::stream::once(async {
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        Err(std::io::Error::new(
                            std::io::ErrorKind::ConnectionReset,
                            "controlled provider disconnect",
                        ))
                    }));
                return axum::response::Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from_stream(chunks))
                    .expect("disconnecting provider response");
            }
            "provider_malformed_utf8" => {
                let mut response_body = secret_marker.into_bytes();
                response_body.push(0xff);
                return axum::response::Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from(response_body))
                    .expect("malformed UTF-8 provider response");
            }
            "provider_200_invalid_json_error" => {
                return (
                    StatusCode::OK,
                    Json(serde_json::json!({
                        "type": "invalid_request_error",
                        "message": secret_marker
                    })),
                )
                    .into_response();
            }
            "provider_200_throttle_json_error" => {
                return (
                    StatusCode::OK,
                    Json(serde_json::json!({
                        "__type": "ThrottlingException",
                        "message": secret_marker
                    })),
                )
                    .into_response();
            }
            "provider_200_server_json_error" => {
                return (
                    StatusCode::OK,
                    Json(serde_json::json!({
                        "__type": "ServiceUnavailableException",
                        "message": secret_marker
                    })),
                )
                    .into_response();
            }
            "provider_200_timeout_json_error" => {
                return (
                    StatusCode::OK,
                    Json(serde_json::json!({
                        "code": "RequestTimeoutException",
                        "message": secret_marker
                    })),
                )
                    .into_response();
            }
            "provider_200_unknown_json_error" => {
                return (
                    StatusCode::OK,
                    Json(serde_json::json!({
                        "type": "NovelPrivateException",
                        "message": secret_marker
                    })),
                )
                    .into_response();
            }
            "provider_200_non_eventstream" => {
                return (
                    StatusCode::OK,
                    [(axum::http::header::CONTENT_TYPE, "text/plain")],
                    secret_marker,
                )
                    .into_response();
            }
            "provider_eventstream_headers" => {
                return (
                    StatusCode::OK,
                    [(
                        axum::http::header::CONTENT_TYPE,
                        "application/vnd.amazon.eventstream",
                    )],
                    Vec::<u8>::new(),
                )
                    .into_response();
            }
            "provider_json_headers" => {
                return (
                    StatusCode::OK,
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    Vec::<u8>::new(),
                )
                    .into_response();
            }
            "provider_eventstream_json_exception" => {
                return (
                    StatusCode::OK,
                    [(
                        axum::http::header::CONTENT_TYPE,
                        "application/vnd.amazon.eventstream",
                    )],
                    serde_json::json!({
                        "__type": "ThrottlingException",
                        "message": secret_marker
                    })
                    .to_string(),
                )
                    .into_response();
            }
            "thinking_signature_root_success" | "thinking_signature_nested_success"
                if !has_history_reasoning_content =>
            {
                return (
                    StatusCode::OK,
                    [(
                        axum::http::header::CONTENT_TYPE,
                        "application/vnd.amazon.eventstream",
                    )],
                    Vec::<u8>::new(),
                )
                    .into_response();
            }
            "thinking_signature_json_header_stream_success" if !has_history_reasoning_content => {
                let first = Bytes::from_static(b"\0\0\0\x10json-labeled-eventstream");
                let chunks = futures::stream::once(async move { Ok::<_, std::io::Error>(first) })
                    .chain(futures::stream::pending());
                return axum::response::Response::builder()
                    .status(StatusCode::OK)
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from_stream(chunks))
                    .expect("json-labeled signature retry eventstream response");
            }
            "thinking_signature_unexpected_second" if !has_history_reasoning_content => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"message": secret_marker})),
                )
                    .into_response();
            }
            "thinking_signature_rate_limited_second" if !has_history_reasoning_content => {
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    Json(serde_json::json!({"message": secret_marker})),
                )
                    .into_response();
            }
            "thinking_signature_read_failure" if !has_history_reasoning_content => {
                let first = Bytes::from_static(b"{\"message\":\"");
                let chunks = futures::stream::once(async move { Ok::<_, std::io::Error>(first) })
                    .chain(futures::stream::once(async {
                        Err(std::io::Error::new(
                            std::io::ErrorKind::ConnectionReset,
                            "controlled thinking signature retry body disconnect",
                        ))
                    }));
                return axum::response::Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from_stream(chunks))
                    .expect("disconnecting signature retry response");
            }
            _ => {}
        }

        let response = match scenario.as_str() {
            "model_unavailable" => serde_json::json!({
                "message": "The requested model is not available for this endpoint.",
                "reason": "MODEL_UNAVAILABLE"
            }),
            "invalid_model" => serde_json::json!({
                "message": "Invalid model ID. Please select a different model to continue.",
                "reason": "INVALID_MODEL_ID"
            }),
            "image_empty" => serde_json::json!({
                "message": "Image data cannot be empty.",
                "reason": "REQUEST_BODY_INVALID"
            }),
            "image_format_unsupported" => serde_json::json!({
                "message": "Bedrock could not process image.",
                "reason": "IMAGE_FORMAT_UNSUPPORTED"
            }),
            "generic_body_invalid" => serde_json::json!({
                "message": "A required request field is missing.",
                "reason": "REQUEST_BODY_INVALID"
            }),
            "malformed" => serde_json::json!({
                "message": "The request body is improperly formed"
            }),
            "invalid_tool" => serde_json::json!({
                "message": "Invalid tool use format.",
                "reason": "REQUEST_BODY_INVALID"
            }),
            "invalid_tool_schema" => serde_json::json!({
                "message": "tools.26.custom.input_schema is invalid.",
                "reason": "TOOL_SCHEMA_INVALID"
            }),
            "invalid_tool_prompt_retry_disabled" => serde_json::json!({
                "message": "Invalid tool use format.",
                "reason": "REQUEST_BODY_INVALID"
            }),
            "thinking_signature_root_success"
            | "thinking_signature_json_header_stream_success"
            | "thinking_signature_repeat"
            | "thinking_signature_unexpected_second"
            | "thinking_signature_rate_limited_second"
            | "thinking_signature_read_failure"
            | "thinking_signature_root_without_builder" => serde_json::json!({
                "message": "Historical reasoning signature is no longer valid.",
                "reason": "THINKING_SIGNATURE_INVALID"
            }),
            "thinking_signature_nested_success" | "thinking_signature_nested_repeat" => {
                serde_json::json!({
                    "error": {"reason": "THINKING_SIGNATURE_INVALID"}
                })
            }
            "thinking_signature_message_only" => serde_json::json!({
                "message": "THINKING_SIGNATURE_INVALID",
                "reason": "REQUEST_BODY_INVALID"
            }),
            "thinking_signature_code_only" => serde_json::json!({
                "code": "THINKING_SIGNATURE_INVALID"
            }),
            "thinking_signature_lowercase" => serde_json::json!({
                "reason": "thinking_signature_invalid"
            }),
            "thinking_signature_substring" => serde_json::json!({
                "reason": "PREFIX_THINKING_SIGNATURE_INVALID_SUFFIX"
            }),
            "thinking_signature_wrong_status" => serde_json::json!({
                "reason": "THINKING_SIGNATURE_INVALID"
            }),
            "non_model_endpoint_unavailable" => serde_json::json!({
                "message": "The requested feature is not available for this endpoint."
            }),
            "provider_status_400"
            | "provider_status_401"
            | "provider_status_403"
            | "provider_status_408"
            | "provider_status_429"
            | "provider_status_500"
            | "provider_status_503" => serde_json::json!({
                "message": secret_marker,
                "private": secret_marker
            }),
            _ => serde_json::json!({"message": "Bad request"}),
        };
        let status = match scenario.as_str() {
            "rescue_server_error" | "provider_status_500" => StatusCode::INTERNAL_SERVER_ERROR,
            "provider_status_503" => StatusCode::SERVICE_UNAVAILABLE,
            "provider_status_401" => StatusCode::UNAUTHORIZED,
            "provider_status_403" => StatusCode::FORBIDDEN,
            "provider_status_408" => StatusCode::REQUEST_TIMEOUT,
            "provider_status_429" => StatusCode::TOO_MANY_REQUESTS,
            "thinking_signature_wrong_status" => StatusCode::UNPROCESSABLE_ENTITY,
            _ => StatusCode::BAD_REQUEST,
        };
        (status, Json(response)).into_response()
    }

    async fn fake_mcp_server_error(
        State(state): State<FakeBadRequestState>,
        body: Bytes,
    ) -> axum::response::Response {
        state.mcp_hits.fetch_add(1, Ordering::Relaxed);
        let scenario = serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|value| {
                value
                    .get("testScenario")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "default".to_string());
        match scenario.as_str() {
            "mcp_success_header" => (
                StatusCode::OK,
                Json(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": "x",
                    "result": {
                        "content": [{
                            "type": "text",
                            "text": "{\"results\":[],\"totalResults\":0}"
                        }],
                        "isError": false
                    }
                })),
            )
                .into_response(),
            "mcp_error_content_length_over_limit" => axum::response::Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .header(
                    axum::http::header::CONTENT_LENGTH,
                    (PROVIDER_DIAGNOSTIC_BODY_MAX_BYTES + 1).to_string(),
                )
                .body(axum::body::Body::from(vec![
                    b'x';
                    PROVIDER_DIAGNOSTIC_BODY_MAX_BYTES
                        + 1
                ]))
                .expect("declared over-limit MCP error response"),
            "mcp_error_chunked_over_limit" => {
                let chunks =
                    futures::stream::iter((0..129).map(|_| {
                        Ok::<_, std::convert::Infallible>(Bytes::from(vec![b'x'; 8 * 1024]))
                    }))
                    .chain(futures::stream::pending());
                axum::response::Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(axum::body::Body::from_stream(chunks))
                    .expect("chunked over-limit MCP error response")
            }
            "mcp_misleading_500" => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "message": "misleading private body says 429 timeout 400"
                })),
            )
                .into_response(),
            "mcp_auth_failure" => (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "message": "The bearer token included in the request is invalid"
                })),
            )
                .into_response(),
            _ => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"message": "controlled MCP failure"})),
            )
                .into_response(),
        }
    }

    async fn fake_oauth_refresh(
        State(state): State<FakeBadRequestState>,
        _body: Bytes,
    ) -> (StatusCode, Json<serde_json::Value>) {
        state.oauth_refresh_hits.fetch_add(1, Ordering::Relaxed);
        (
            StatusCode::OK,
            Json(serde_json::json!({
                "access_token": "unexpected-refreshed-access-token",
                "expires_in": 3600
            })),
        )
    }

    async fn fake_model_discovery_failure(
        State(state): State<FakeBadRequestState>,
        Query(query): Query<HashMap<String, String>>,
        headers: AxumHeaderMap,
    ) -> axum::response::Response {
        let token = headers
            .get("Authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .unwrap_or_default();
        let scenario = if token.starts_with("pagination-success") {
            "model_discovery_pagination_success"
        } else if token.starts_with("pagination-cycle") {
            "model_discovery_pagination_cycle"
        } else if token.starts_with("pagination-endless") {
            "model_discovery_pagination_endless"
        } else {
            "model_discovery_failure"
        };
        state.total_hits.fetch_add(1, Ordering::Relaxed);
        *state
            .hits
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(scenario.to_string())
            .or_insert(0) += 1;

        if scenario == "model_discovery_pagination_success" {
            let page = query
                .get("nextToken")
                .and_then(|token| token.strip_prefix("success-page-"))
                .and_then(|page| page.parse::<usize>().ok())
                .unwrap_or(0);
            let next_token = (page < 2).then(|| format!("success-page-{}", page + 1));
            return Json(serde_json::json!({
                "models": [{"modelId": format!("success-model-{page}")}],
                "nextToken": next_token,
            }))
            .into_response();
        }
        if scenario == "model_discovery_pagination_cycle" {
            return Json(serde_json::json!({
                "models": [{"modelId": "cycle-model"}],
                "nextToken": "cycle-token",
            }))
            .into_response();
        }
        if scenario == "model_discovery_pagination_endless" {
            let page = query
                .get("nextToken")
                .and_then(|token| token.strip_prefix("endless-page-"))
                .and_then(|page| page.parse::<usize>().ok())
                .unwrap_or(0);
            return Json(serde_json::json!({
                "models": [{"modelId": format!("endless-model-{page}")}],
                "nextToken": format!("endless-page-{}", page + 1),
            }))
            .into_response();
        }

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "message": state
                    .auxiliary_marker()
                    .unwrap_or_else(|| "controlled model discovery failure".to_string())
            })),
        )
            .into_response()
    }

    async fn fake_profile_discovery_success(
        State(state): State<FakeBadRequestState>,
        headers: AxumHeaderMap,
    ) -> (StatusCode, Json<serde_json::Value>) {
        const SCENARIO: &str = "profile_discovery_success";
        let token = headers
            .get("Authorization")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .unwrap_or("missing-token")
            .to_string();
        state.total_hits.fetch_add(1, Ordering::Relaxed);
        let token_hit = {
            let mut hits = state
                .hits
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *hits.entry(SCENARIO.to_string()).or_insert(0) += 1;
            let token_hits = hits.entry(format!("profile_token:{token}")).or_insert(0);
            *token_hits += 1;
            *token_hits
        };
        let active = state.profile_active.fetch_add(1, Ordering::SeqCst) + 1;
        state.profile_max_active.fetch_max(active, Ordering::SeqCst);
        let _active_guard = FakeProfileActiveGuard(state.profile_active.clone());
        let delay_ms = if token.contains("very-slow") {
            100
        } else if token.contains("slow") {
            20
        } else {
            2
        };
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;

        if token.starts_with("forbidden-") {
            return (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({
                    "message": state
                        .auxiliary_marker()
                        .unwrap_or_else(|| "controlled 403".to_string())
                })),
            );
        }
        if token.starts_with("server-error-") || (token.starts_with("recover-") && token_hit == 1) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"message": "controlled 500"})),
            );
        }
        if token.starts_with("empty-") {
            return (StatusCode::OK, Json(serde_json::json!({"profiles": []})));
        }
        let profile_region = if token.starts_with("region-shift-") {
            "eu-central-1"
        } else {
            "us-east-1"
        };
        (
            StatusCode::OK,
            Json(serde_json::json!({
                "profiles": [{
                    "arn": format!("arn:aws:codewhisperer:{profile_region}:123456789012:profile/FAKE")
                }]
            })),
        )
    }

    fn fake_bad_request_credentials(pool_size: usize) -> Vec<KiroCredentials> {
        (1..=pool_size)
            .map(|id| KiroCredentials {
                id: Some(id as u64),
                access_token: Some(format!("fake-token-{id}")),
                expires_at: Some((Utc::now() + Duration::hours(1)).to_rfc3339()),
                auth_method: Some("social".to_string()),
                ..Default::default()
            })
            .collect()
    }

    fn fake_external_idp_credential(id: u64, token: &str) -> KiroCredentials {
        KiroCredentials {
            id: Some(id),
            access_token: Some(token.to_string()),
            refresh_token: Some(format!("refresh-{token}-{}", "x".repeat(256))),
            expires_at: Some((Utc::now() + Duration::hours(1)).to_rfc3339()),
            auth_method: Some("external_idp".to_string()),
            client_id: Some("fake-client".to_string()),
            token_endpoint: Some("http://127.0.0.1/unused".to_string()),
            ..Default::default()
        }
    }

    fn fake_final_attempt_provider(base_url: &str, token: &str) -> KiroProvider {
        let mut credential = fake_external_idp_credential(1, token);
        credential.profile_arn =
            Some("arn:aws:codewhisperer:us-east-1:123456789012:profile/FINAL_ATTEMPT".to_string());
        credential.token_endpoint = Some(format!("{base_url}/oauth-refresh"));

        let mut config = Config::default();
        config.kiro_upstream_base_url = Some(base_url.to_string());
        config.kiro_upstream_response_timeout_secs = 2;
        config.credential_retry_max_attempts = 1;
        let manager = Arc::new(
            MultiTokenManager::new(config, vec![credential], None, None, false)
                .expect("final-attempt fixture token manager"),
        );
        let mut endpoints: HashMap<String, Arc<dyn KiroEndpoint>> = HashMap::new();
        endpoints.insert("ide".to_string(), Arc::new(IdeEndpoint));
        KiroProvider::with_proxy(manager, None, endpoints, "ide".to_string())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn automatic_auth_recovery_auxiliary_admission_is_provider_health_neutral_for_five_rounds()
     {
        for round in 1..=5 {
            let marker = format!("provider-auto-recovery-private-{round}");
            let (provider, manager) = fake_profile_provider(
                "http://127.0.0.1:1",
                vec![fake_external_idp_credential(1, &marker)],
                None,
            );
            let ctx = manager
                .acquire_context_for_credential(1)
                .await
                .expect("non-expired provider recovery context");
            let budget = Arc::new(AuxiliaryAttemptBudget::new(1));
            budget
                .reserve(AuxiliaryAttemptKind::ProfileDiscovery)
                .expect("consume provider request auxiliary budget");
            let mut excluded_ids = HashSet::new();
            let mut recovered_ids = HashSet::new();
            let mut automatic_recovery_allowed = true;

            let decision = provider
                .handle_credential_auth_failure(
                    "provider-test",
                    reqwest::StatusCode::FORBIDDEN,
                    r#"{"message":"The bearer token included in the request is invalid"}"#,
                    "provider_test_auth_error",
                    &ctx,
                    &IdeEndpoint,
                    "#1",
                    None,
                    None,
                    budget.clone(),
                    &mut excluded_ids,
                    &mut recovered_ids,
                    &mut automatic_recovery_allowed,
                    true,
                )
                .await
                .expect("auxiliary admission failure remains a routing decision");

            assert_eq!(
                decision,
                CredentialAuthFailureDecision::Retry {
                    excluded_current: true
                },
                "round {round}"
            );
            assert_eq!(excluded_ids, HashSet::from([1]), "round {round}");
            assert_eq!(recovered_ids, HashSet::from([1]), "round {round}");
            assert!(!automatic_recovery_allowed, "round {round}");
            assert_eq!(budget.snapshot().token_refresh_attempts, 0, "round {round}");
            let snapshot = manager.snapshot();
            assert_eq!(snapshot.entries[0].failure_count, 0, "round {round}");
            assert_eq!(
                snapshot.entries[0].refresh_failure_count, 0,
                "round {round}"
            );
            assert!(!snapshot.entries[0].cooled_down, "round {round}");
            assert!(!snapshot.entries[0].disabled, "round {round}");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn api_and_mcp_final_attempt_fixtures_do_not_start_oauth_refresh_for_five_rounds() {
        const API_SCENARIO: &str = "provider_status_403";
        let server = FakeBadRequestServer::start().await;
        for round in 1..=5 {
            let api_provider = fake_final_attempt_provider(
                &server.base_url,
                &format!("api-final-attempt-{round}"),
            );
            let api_budget = Arc::new(InferenceAttemptBudget::with_auxiliary_max_attempts(4, 4));
            let api_hits_before = server.state.scenario_hits(API_SCENARIO);
            let oauth_hits_before = server.state.oauth_refresh_hits();
            api_provider
                .call_api_with_context_with_request_id_and_attempt_budget(
                    &serde_json::json!({
                        "testScenario": API_SCENARIO,
                        "secretMarker": "The bearer token included in the request is invalid",
                        "conversationState": {
                            "conversationId": format!("api-final-attempt-{round}"),
                            "currentMessage": {
                                "userInputMessage": {
                                    "content": "test",
                                    "modelId": "claude-sonnet-4"
                                }
                            }
                        }
                    })
                    .to_string(),
                    Some("req-api-final-attempt"),
                    AcquireMode::WaitForCapacity,
                    1,
                    Some("claude-sonnet-4"),
                    api_budget.clone(),
                    false,
                )
                .await
                .err()
                .expect("controlled API final attempt must return 403");
            assert_eq!(
                server.state.scenario_hits(API_SCENARIO) - api_hits_before,
                1,
                "API round {round}: exactly one inference send"
            );
            assert_eq!(api_budget.snapshot().consumed, 1, "API round {round}");
            let api_auxiliary = api_budget.auxiliary_snapshot();
            assert_eq!(api_auxiliary.max_attempts, 4, "API round {round}");
            assert_eq!(api_auxiliary.consumed, 0, "API round {round}");
            assert_eq!(
                api_auxiliary.token_refresh_attempts, 0,
                "API round {round}: final attempt cannot spend refresh budget"
            );
            assert_eq!(
                server.state.oauth_refresh_hits(),
                oauth_hits_before,
                "API round {round}: final attempt cannot send OAuth refresh"
            );

            let mcp_provider = fake_final_attempt_provider(
                &server.base_url,
                &format!("mcp-final-attempt-{round}"),
            );
            let mcp_budget = Arc::new(InferenceAttemptBudget::with_auxiliary_max_attempts(4, 4));
            let mcp_hits_before = server.state.mcp_hits.load(Ordering::Relaxed);
            let oauth_hits_before = server.state.oauth_refresh_hits();
            mcp_provider
                .call_mcp(
                    &serde_json::json!({
                        "testScenario": "mcp_auth_failure",
                        "jsonrpc": "2.0",
                        "id": format!("mcp-final-attempt-{round}")
                    })
                    .to_string(),
                    mcp_budget.clone(),
                )
                .await
                .err()
                .expect("controlled MCP final attempt must return 403");
            assert_eq!(
                server.state.mcp_hits.load(Ordering::Relaxed) - mcp_hits_before,
                1,
                "MCP round {round}: exactly one inference send"
            );
            assert_eq!(mcp_budget.snapshot().consumed, 1, "MCP round {round}");
            let mcp_auxiliary = mcp_budget.auxiliary_snapshot();
            assert_eq!(mcp_auxiliary.max_attempts, 4, "MCP round {round}");
            assert_eq!(mcp_auxiliary.consumed, 0, "MCP round {round}");
            assert_eq!(
                mcp_auxiliary.token_refresh_attempts, 0,
                "MCP round {round}: final attempt cannot spend refresh budget"
            );
            assert_eq!(
                server.state.oauth_refresh_hits(),
                oauth_hits_before,
                "MCP round {round}: final attempt cannot send OAuth refresh"
            );
        }
        assert_eq!(server.state.oauth_refresh_hits(), 0);
    }

    fn fake_profile_provider(
        base_url: &str,
        credentials: Vec<KiroCredentials>,
        policy: Option<ProfileArnDiscoveryPolicy>,
    ) -> (Arc<KiroProvider>, Arc<MultiTokenManager>) {
        fake_profile_provider_with_auxiliary_limit(base_url, credentials, policy, None)
    }

    fn fake_profile_provider_with_auxiliary_limit(
        base_url: &str,
        credentials: Vec<KiroCredentials>,
        policy: Option<ProfileArnDiscoveryPolicy>,
        auxiliary_limit: Option<u32>,
    ) -> (Arc<KiroProvider>, Arc<MultiTokenManager>) {
        let mut config = Config::default();
        config.kiro_upstream_base_url = Some(base_url.to_string());
        config.kiro_upstream_response_timeout_secs = 2;
        if let Some(auxiliary_limit) = auxiliary_limit {
            config.auxiliary_upstream_max_concurrent_requests = auxiliary_limit;
        }
        let manager = Arc::new(
            MultiTokenManager::new(config, credentials, None, None, false)
                .expect("fake profile discovery token manager"),
        );
        let mut endpoints: HashMap<String, Arc<dyn KiroEndpoint>> = HashMap::new();
        endpoints.insert("ide".to_string(), Arc::new(IdeEndpoint));
        let mut provider =
            KiroProvider::with_proxy(manager.clone(), None, endpoints, "ide".to_string());
        if let Some(policy) = policy {
            provider.profile_arn_discovery_policy = policy;
        }
        (Arc::new(provider), manager)
    }

    fn fake_model_discovery_provider(
        base_url: &str,
        tokens: impl IntoIterator<Item = String>,
    ) -> Arc<KiroProvider> {
        let credentials = tokens
            .into_iter()
            .enumerate()
            .map(|(index, token)| KiroCredentials {
                id: Some(index as u64 + 1),
                access_token: Some(token),
                expires_at: Some((Utc::now() + Duration::hours(1)).to_rfc3339()),
                auth_method: Some("social".to_string()),
                profile_arn: Some(
                    "arn:aws:codewhisperer:us-east-1:123456789012:profile/MODELTEST".to_string(),
                ),
                ..Default::default()
            })
            .collect();
        let mut config = Config::default();
        config.kiro_upstream_base_url = Some(base_url.to_string());
        config.kiro_upstream_response_timeout_secs = 2;
        let manager = Arc::new(
            MultiTokenManager::new(config, credentials, None, None, false)
                .expect("fake model discovery token manager"),
        );
        let mut endpoints: HashMap<String, Arc<dyn KiroEndpoint>> = HashMap::new();
        endpoints.insert("ide".to_string(), Arc::new(IdeEndpoint));
        Arc::new(KiroProvider::with_proxy(
            manager,
            None,
            endpoints,
            "ide".to_string(),
        ))
    }

    fn fake_body_capture_provider(
        base_url: &str,
        endpoint_name: &str,
        profile_arn: Option<&str>,
        compression_enabled: bool,
    ) -> KiroProvider {
        let mut config = Config::default();
        config.kiro_upstream_base_url = Some(base_url.to_string());
        config.kiro_upstream_response_timeout_secs = 5;
        config.compression.enabled = compression_enabled;
        config.compression.whitespace_compression = true;

        let credentials = if let Some(profile_arn) = profile_arn {
            KiroCredentials {
                id: Some(1),
                access_token: Some("fake-body-capture-access-token".to_string()),
                expires_at: Some((Utc::now() + Duration::hours(1)).to_rfc3339()),
                auth_method: Some("social".to_string()),
                profile_arn: Some(profile_arn.to_string()),
                endpoint: Some(endpoint_name.to_string()),
                ..Default::default()
            }
        } else {
            KiroCredentials {
                id: Some(1),
                auth_method: Some("api_key".to_string()),
                kiro_api_key: Some("ksk_fake_body_capture".to_string()),
                endpoint: Some(endpoint_name.to_string()),
                ..Default::default()
            }
        };
        let manager = Arc::new(
            MultiTokenManager::new(config, vec![credentials], None, None, false)
                .expect("fake body capture token manager"),
        );
        let mut endpoints: HashMap<String, Arc<dyn KiroEndpoint>> = HashMap::new();
        endpoints.insert("ide".to_string(), Arc::new(IdeEndpoint));
        endpoints.insert("cli".to_string(), Arc::new(CliEndpoint::new()));
        KiroProvider::with_proxy(manager, None, endpoints, endpoint_name.to_string())
    }

    async fn call_body_capture_provider(provider: &KiroProvider, body: &str, is_stream: bool) {
        if is_stream {
            let response = provider
                .call_api_stream(body)
                .await
                .expect("stream body capture provider call");
            let (response, completion) = response.into_parts();
            assert_eq!(response.status(), reqwest::StatusCode::OK);
            drop(completion);
        } else {
            let response = provider
                .call_api_with_context(body)
                .await
                .expect("non-stream body capture provider call");
            let (response, completion) = response.into_parts();
            assert_eq!(response.status(), reqwest::StatusCode::OK);
            completion.release();
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn provider_sends_endpoint_and_compression_bytes_exactly_for_five_rounds() {
        const PROFILE_ARN: &str =
            "arn:aws:codewhisperer:us-east-1:123456789012:profile/BODYCAPTURE";
        let server = FakeProviderBodyCaptureServer::start().await;
        let no_op_body = " {\n  \"conversationState\" : {\n    \"conversationId\" : \"body-capture-no-op\",\n    \"currentMessage\" : { \"userInputMessage\" : { \"modelId\" : \"claude-sonnet-4\", \"content\" : \"keep  spaces\\n\\u00e9\" } }\n  },\n  \"z\" : 1.0, \"a\" : 1e+02, \"z\" : 18446744073709551616\n} \n";
        let compressed_no_op_body = "{\"conversationState\":{\"conversationId\":\"body-capture-no-op\",\"currentMessage\":{\"userInputMessage\":{\"modelId\":\"claude-sonnet-4\",\"content\":\"keep  spaces\\n\\u00e9\"}}},\"z\":1.0,\"a\":1e+02,\"z\":18446744073709551616}";

        let cli_mutation_body = r#"{
            "conversationState": {
                "conversationId": "body-capture-cli",
                "currentMessage": {"userInputMessage": {
                    "origin": "AI_EDITOR",
                    "modelId": "claude-sonnet-4",
                    "content": "keep  spaces\n\u00e9"
                }},
                "history": [{"userInputMessage":{"origin":"AI_EDITOR","content":"old"}}]
            },
            "additionalModelRequestFields": {
                "thinking": {"type":"adaptive"},
                "output_config": {"effort":"xhigh"},
                "unknown": [true, null]
            },
            "unknownRoot": {"z":1.0,"a":1e+02}
        }"#;
        let mut expected_cli: serde_json::Value = serde_json::from_str(cli_mutation_body).unwrap();
        expected_cli["conversationState"]["currentMessage"]["userInputMessage"]["origin"] =
            serde_json::json!("KIRO_CLI");
        expected_cli["conversationState"]["history"][0]["userInputMessage"]["origin"] =
            serde_json::json!("KIRO_CLI");
        expected_cli["profileArn"] = serde_json::json!(PROFILE_ARN);
        let expected_cli = serde_json::to_string(&expected_cli).unwrap();

        let ide_mutation_body = r#"{
            "conversationState": {
                "conversationId": "body-capture-ide",
                "currentMessage": {"userInputMessage": {
                    "modelId": "claude-sonnet-4",
                    "content": "keep  spaces\n\u00e9"
                }}
            },
            "additionalModelRequestFields": {
                "output_config": {"effort":"xhigh"},
                "unknown": [true, null]
            },
            "unknownRoot": {"z":1.0,"a":1e+02}
        }"#;
        let mut expected_ide: serde_json::Value = serde_json::from_str(ide_mutation_body).unwrap();
        expected_ide["profileArn"] = serde_json::json!(PROFILE_ARN);
        let expected_ide = serde_json::to_string(&expected_ide).unwrap();

        let mut capture_index = 0;
        for (endpoint_name, path, content_type, profile_arn, body, mutation_expected) in [
            (
                "cli",
                "/",
                "application/x-amz-json-1.0",
                None,
                no_op_body,
                None,
            ),
            (
                "cli",
                "/",
                "application/x-amz-json-1.0",
                Some(PROFILE_ARN),
                cli_mutation_body,
                Some(expected_cli.as_str()),
            ),
            (
                "ide",
                "/generateAssistantResponse",
                "application/json",
                None,
                no_op_body,
                None,
            ),
            (
                "ide",
                "/generateAssistantResponse",
                "application/json",
                Some(PROFILE_ARN),
                ide_mutation_body,
                Some(expected_ide.as_str()),
            ),
        ] {
            for compression_enabled in [false, true] {
                let provider = fake_body_capture_provider(
                    &server.base_url,
                    endpoint_name,
                    profile_arn,
                    compression_enabled,
                );
                let expected = mutation_expected.unwrap_or(if compression_enabled {
                    compressed_no_op_body
                } else {
                    no_op_body
                });
                for is_stream in [false, true] {
                    for round in 0..5 {
                        call_body_capture_provider(&provider, body, is_stream).await;
                        let capture = server.capture(capture_index);
                        capture_index += 1;
                        assert_eq!(capture.path, path, "round={round}, stream={is_stream}");
                        assert_eq!(
                            capture.content_type, content_type,
                            "round={round}, stream={is_stream}"
                        );
                        assert_eq!(
                            capture.len,
                            expected.len(),
                            "endpoint={endpoint_name}, compression={compression_enabled}, round={round}, stream={is_stream}"
                        );
                        assert_eq!(
                            capture.sha256,
                            hex::encode(Sha256::digest(expected.as_bytes())),
                            "endpoint={endpoint_name}, compression={compression_enabled}, round={round}, stream={is_stream}"
                        );
                        assert_eq!(
                            capture.bytes,
                            expected.as_bytes(),
                            "endpoint={endpoint_name}, compression={compression_enabled}, round={round}, stream={is_stream}"
                        );
                    }
                }
            }
        }
        assert_eq!(capture_index, 80);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn provider_sends_converter_max_effort_with_native_adaptive_thinking_for_five_rounds() {
        use crate::anthropic::converter::{ConverterOptions, convert_request_with_options};
        use crate::anthropic::model_capabilities::{
            KiroReasoningCapabilityState, KiroReasoningFieldCapability, KiroReasoningFieldPath,
        };
        use crate::anthropic::types::{
            Message as AnthropicMessage, MessagesRequest, OutputConfig, Thinking,
        };
        use crate::kiro::model::requests::kiro::KiroRequest;

        let server = FakeProviderBodyCaptureServer::start().await;
        let request = MessagesRequest {
            model: "claude-opus-4.8".to_string(),
            max_tokens: 64_000,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("Reply with wire-ok"),
            }],
            stream: true,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: Some(Thinking {
                thinking_type: "adaptive".to_string(),
                budget_tokens: 0,
            }),
            output_config: Some(OutputConfig {
                effort: Some("max".to_string()),
            }),
            metadata: None,
        };
        let options = ConverterOptions {
            native_reasoning_capability: KiroReasoningCapabilityState::Supported(
                KiroReasoningFieldCapability {
                    path: KiroReasoningFieldPath::OutputConfig,
                    efforts: ["high", "max"].map(str::to_string).to_vec(),
                    default_effort: Some("high".to_string()),
                },
            ),
            ..ConverterOptions::default()
        };

        let mut capture_index = 0;
        for (endpoint, expected_path, expected_content_type, expected_origin) in [
            ("cli", "/", "application/x-amz-json-1.0", "KIRO_CLI"),
            (
                "ide",
                "/generateAssistantResponse",
                "application/json",
                "AI_EDITOR",
            ),
        ] {
            let provider = fake_body_capture_provider(&server.base_url, endpoint, None, false);
            for is_stream in [false, true] {
                for round in 0..5 {
                    let converted = convert_request_with_options(&request, options.clone())
                        .unwrap_or_else(|error| {
                            panic!("{endpoint} stream={is_stream} round={round}: {error}")
                        });
                    let converted = KiroRequest {
                        conversation_state: converted.conversation_state,
                        profile_arn: None,
                        additional_model_request_fields: converted.additional_model_request_fields,
                        tool_cache_point_insert_after: converted.tool_cache_point_insert_after,
                        cache_point_plan_recording_enabled: converted
                            .cache_point_plan_recording_enabled,
                    };
                    let body = serde_json::to_string(&converted).expect("serialize converted body");
                    call_body_capture_provider(&provider, &body, is_stream).await;

                    let capture = server.capture(capture_index);
                    capture_index += 1;
                    assert_eq!(capture.path, expected_path, "round={round}");
                    assert_eq!(capture.content_type, expected_content_type, "round={round}");
                    assert_eq!(capture.len, capture.bytes.len(), "round={round}");
                    assert_eq!(
                        capture.sha256,
                        hex::encode(Sha256::digest(&capture.bytes)),
                        "round={round}"
                    );

                    let wire: serde_json::Value =
                        serde_json::from_slice(&capture.bytes).expect("captured wire JSON");
                    assert_eq!(
                        wire["additionalModelRequestFields"],
                        serde_json::json!({
                            "thinking": {"type": "adaptive"},
                            "output_config": {"effort": "max"}
                        }),
                        "{endpoint} stream={is_stream} round={round}: max and adaptive thinking must reach the final wire unchanged"
                    );
                    assert!(
                        wire["additionalModelRequestFields"]["thinking"]
                            .get("budget_tokens")
                            .is_none(),
                        "{endpoint} stream={is_stream} round={round}: native Kiro adaptive thinking must not carry Anthropic budget_tokens"
                    );
                    assert_eq!(
                        wire["conversationState"]["currentMessage"]["userInputMessage"]["origin"],
                        expected_origin,
                        "round={round}"
                    );
                }
            }
        }
        assert_eq!(capture_index, 20);
    }

    async fn ensure_profile_for_credential(
        provider: &KiroProvider,
        manager: &MultiTokenManager,
        id: u64,
    ) -> Option<String> {
        let mut ctx = manager
            .acquire_context_for_credential(id)
            .await
            .expect("acquire fake external IdP context");
        let config = provider.runtime_config();
        provider
            .ensure_profile_arn_for_context(&mut ctx, &config, "fake-machine", None)
            .await;
        ctx.credentials.profile_arn
    }

    fn short_profile_discovery_policy(max_entries: usize) -> ProfileArnDiscoveryPolicy {
        ProfileArnDiscoveryPolicy {
            negative_backoff_base: std::time::Duration::from_millis(30),
            negative_backoff_max: std::time::Duration::from_millis(30),
            success_handoff_ttl: std::time::Duration::from_secs(1),
            max_entries,
        }
    }

    async fn concurrently_ensure_profiles(
        provider: Arc<KiroProvider>,
        manager: Arc<MultiTokenManager>,
        credential_ids: &[u64],
        copies_per_credential: usize,
    ) -> Vec<Option<String>> {
        let mut contexts = Vec::new();
        for id in credential_ids {
            for _ in 0..copies_per_credential {
                contexts.push(
                    manager
                        .acquire_context_for_credential(*id)
                        .await
                        .expect("acquire fake external IdP context"),
                );
            }
        }
        let barrier = Arc::new(tokio::sync::Barrier::new(contexts.len()));
        let mut tasks = Vec::new();
        for mut ctx in contexts {
            let provider = provider.clone();
            let barrier = barrier.clone();
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                let config = provider.runtime_config();
                provider
                    .ensure_profile_arn_for_context(&mut ctx, &config, "fake-machine", None)
                    .await;
                ctx.credentials.profile_arn
            }));
        }
        let mut profiles = Vec::new();
        for task in tasks {
            profiles.push(task.await.expect("profile discovery task joins"));
        }
        profiles
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_profile_discovery_for_one_credential_is_singleflight() {
        const SCENARIO: &str = "profile_discovery_success";
        let server = FakeBadRequestServer::start().await;
        for round in 1..=5 {
            let token = format!("slow-profile-concurrent-{round}");
            let hits_before = server.state.scenario_hits(SCENARIO);
            let (provider, manager) = fake_profile_provider(
                &server.base_url,
                vec![fake_external_idp_credential(1, &token)],
                None,
            );
            let mut contexts = Vec::new();
            for _ in 0..16 {
                contexts.push(
                    manager
                        .acquire_context_for_credential(1)
                        .await
                        .expect("acquire fake external IdP context"),
                );
            }
            let barrier = Arc::new(tokio::sync::Barrier::new(contexts.len()));
            let mut tasks = Vec::new();
            for mut ctx in contexts {
                let provider = provider.clone();
                let barrier = barrier.clone();
                tasks.push(tokio::spawn(async move {
                    barrier.wait().await;
                    let config = provider.runtime_config();
                    provider
                        .ensure_profile_arn_for_context(&mut ctx, &config, "fake-machine", None)
                        .await;
                    ctx.credentials.profile_arn
                }));
            }
            for task in tasks {
                assert!(
                    task.await.expect("profile discovery task joins").is_some(),
                    "round {round}: every coalesced caller should receive the profile ARN"
                );
            }

            assert_eq!(
                server.state.scenario_hits(SCENARIO) - hits_before,
                1,
                "round {round}: same-credential callers must share one auxiliary request"
            );
            assert_eq!(server.state.profile_hits(&token), 1);
            let metrics = provider.profile_arn_discovery_metrics();
            assert_eq!(metrics.upstream_attempts, 1);
            assert_eq!(metrics.successes, 1);
            assert_eq!(metrics.negative_results, 0);
            assert!(metrics.coalesced_waiters >= 1);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn profile_discovery_403_and_500_are_coalesced_and_negatively_cached() {
        let server = FakeBadRequestServer::start().await;
        for failure_mode in ["forbidden", "server-error"] {
            for round in 1..=5 {
                let token = format!("{failure_mode}-slow-{round}");
                let (provider, manager) = fake_profile_provider(
                    &server.base_url,
                    vec![fake_external_idp_credential(1, &token)],
                    None,
                );

                let first_wave =
                    concurrently_ensure_profiles(provider.clone(), manager.clone(), &[1], 16).await;
                assert!(
                    first_wave.iter().all(Option::is_none),
                    "{failure_mode} round {round}: failure must keep fallback profile semantics"
                );
                assert_eq!(
                    server.state.profile_hits(&token),
                    1,
                    "{failure_mode} round {round}: concurrent failure must be singleflight"
                );

                let repeat_wave =
                    concurrently_ensure_profiles(provider.clone(), manager.clone(), &[1], 32).await;
                assert!(repeat_wave.iter().all(Option::is_none));
                assert_eq!(
                    server.state.profile_hits(&token),
                    1,
                    "{failure_mode} round {round}: short repeats must make zero auxiliary calls"
                );

                let metrics = provider.profile_arn_discovery_metrics();
                assert_eq!(metrics.upstream_attempts, 1);
                assert_eq!(metrics.successes, 0);
                assert_eq!(metrics.negative_results, 1);
                assert!(metrics.coalesced_waiters >= 1);
                assert!(metrics.negative_cache_suppressions >= 32);
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn profile_discovery_is_per_credential_for_1_20_60_accounts_over_five_rounds() {
        let server = FakeBadRequestServer::start().await;
        for pool_size in [1_usize, 20, 60] {
            for round in 1..=5 {
                server.state.reset_profile_concurrency();
                let credentials = (1..=pool_size)
                    .map(|id| {
                        fake_external_idp_credential(
                            id as u64,
                            &format!("slow-matrix-{pool_size}-{round}-{id}"),
                        )
                    })
                    .collect();
                let ids = (1..=pool_size as u64).collect::<Vec<_>>();
                let hits_before = server.state.scenario_hits("profile_discovery_success");
                let (provider, manager) = fake_profile_provider_with_auxiliary_limit(
                    &server.base_url,
                    credentials,
                    None,
                    Some(pool_size as u32),
                );

                let profiles =
                    concurrently_ensure_profiles(provider.clone(), manager, &ids, 2).await;
                assert!(
                    profiles.iter().all(Option::is_some),
                    "pool {pool_size}, round {round}: every caller should receive its ARN"
                );
                assert_eq!(
                    server.state.scenario_hits("profile_discovery_success") - hits_before,
                    pool_size,
                    "pool {pool_size}, round {round}: each credential gets exactly one request"
                );
                for id in &ids {
                    let token = format!("slow-matrix-{pool_size}-{round}-{id}");
                    assert_eq!(server.state.profile_hits(&token), 1);
                }
                assert_eq!(
                    provider.profile_arn_discovery_metrics().upstream_attempts,
                    pool_size as u64
                );
                if pool_size > 1 {
                    assert!(
                        server.state.profile_max_active() > 1,
                        "pool {pool_size}, round {round}: different credentials were globally serialized"
                    );
                } else {
                    assert_eq!(server.state.profile_max_active(), 1);
                }
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn profile_discovery_negative_backoff_recovers_and_recoalesces_for_five_rounds() {
        let server = FakeBadRequestServer::start().await;
        for round in 1..=5 {
            let token = format!("recover-slow-{round}");
            let (provider, manager) = fake_profile_provider(
                &server.base_url,
                vec![fake_external_idp_credential(1, &token)],
                Some(short_profile_discovery_policy(64)),
            );

            assert!(
                ensure_profile_for_credential(&provider, &manager, 1)
                    .await
                    .is_none()
            );
            assert_eq!(server.state.profile_hits(&token), 1);
            let suppressed =
                concurrently_ensure_profiles(provider.clone(), manager.clone(), &[1], 8).await;
            assert!(suppressed.iter().all(Option::is_none));
            assert_eq!(server.state.profile_hits(&token), 1);

            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
            let recovered =
                concurrently_ensure_profiles(provider.clone(), manager.clone(), &[1], 16).await;
            assert!(
                recovered.iter().all(Option::is_some),
                "round {round}: callers should recover after bounded backoff"
            );
            assert_eq!(server.state.profile_hits(&token), 2);

            assert!(
                ensure_profile_for_credential(&provider, &manager, 1)
                    .await
                    .is_some(),
                "round {round}: persisted profile ARN should take the normal fast path"
            );
            assert_eq!(server.state.profile_hits(&token), 2);
            let metrics = provider.profile_arn_discovery_metrics();
            assert_eq!(metrics.upstream_attempts, 2);
            assert_eq!(metrics.negative_results, 1);
            assert_eq!(metrics.successes, 1);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn profile_discovery_identity_prevents_id_reuse_backoff_and_state_is_bounded() {
        let server = FakeBadRequestServer::start().await;
        let (provider, manager) = fake_profile_provider(
            &server.base_url,
            vec![fake_external_idp_credential(1, "forbidden-reused-old")],
            Some(short_profile_discovery_policy(8)),
        );
        let config = provider.runtime_config();

        let mut old_ctx = manager
            .acquire_context_for_credential(1)
            .await
            .expect("acquire deleted credential simulation");
        let old_key = KiroProvider::profile_arn_discovery_key(&old_ctx, &config, "fake-machine");
        provider
            .ensure_profile_arn_for_context(&mut old_ctx, &config, "fake-machine", None)
            .await;
        assert!(old_ctx.credentials.profile_arn.is_none());

        let replacement_manager = MultiTokenManager::new(
            config.clone(),
            vec![fake_external_idp_credential(1, "replacement-reused-id")],
            None,
            None,
            false,
        )
        .expect("replacement manager");
        let mut replacement_ctx = replacement_manager
            .acquire_context_for_credential(1)
            .await
            .expect("acquire replacement credential simulation");
        assert_eq!(
            old_ctx.id, replacement_ctx.id,
            "both credentials reuse ID 1"
        );
        let replacement_key =
            KiroProvider::profile_arn_discovery_key(&replacement_ctx, &config, "fake-machine");
        assert_ne!(
            old_key, replacement_key,
            "replacement authentication identity must not inherit deleted credential state"
        );
        provider
            .ensure_profile_arn_for_context(&mut replacement_ctx, &config, "fake-machine", None)
            .await;
        assert!(replacement_ctx.credentials.profile_arn.is_some());
        assert_eq!(server.state.profile_hits("forbidden-reused-old"), 1);
        assert_eq!(server.state.profile_hits("replacement-reused-id"), 1);

        for index in 0..32 {
            let token = format!("forbidden-bounded-{index}");
            let reused_manager = MultiTokenManager::new(
                config.clone(),
                vec![fake_external_idp_credential(1, &token)],
                None,
                None,
                false,
            )
            .expect("bounded-state reused manager");
            let mut ctx = reused_manager
                .acquire_context_for_credential(1)
                .await
                .expect("acquire bounded-state credential");
            provider
                .ensure_profile_arn_for_context(&mut ctx, &config, "fake-machine", None)
                .await;
        }
        assert!(
            provider.profile_arn_discovery_entries.lock().len() <= 8,
            "deleted/replaced credential state must remain under the configured hard bound"
        );
    }

    #[tokio::test]
    async fn cross_region_profile_discovery_uses_a_stable_invalidation_key_for_five_rounds() {
        let server = FakeBadRequestServer::start().await;
        for round in 1..=5 {
            let token = format!("region-shift-{round}");
            let (provider, manager) = fake_profile_provider(
                &server.base_url,
                vec![fake_external_idp_credential(1, &token)],
                None,
            );
            let config = provider.runtime_config();
            let mut ctx = manager
                .acquire_context_for_credential(1)
                .await
                .expect("acquire cross-region context");
            let missing_profile_key =
                KiroProvider::profile_arn_discovery_key(&ctx, &config, "fake-machine");
            provider
                .ensure_profile_arn_for_context(&mut ctx, &config, "fake-machine", None)
                .await;
            assert!(
                ctx.credentials
                    .profile_arn
                    .as_deref()
                    .is_some_and(|arn| arn.contains(":eu-central-1:")),
                "round {round}: fake discovery should shift away from the configured region"
            );
            let discovered_profile_key =
                KiroProvider::profile_arn_discovery_key(&ctx, &config, "fake-machine");
            assert_eq!(
                missing_profile_key, discovered_profile_key,
                "round {round}: discovered ARN region must not change the invalidation identity"
            );

            provider.clear_profile_arn_discovery_state(&ctx, &config, "fake-machine");
            assert!(provider.profile_arn_discovery_entries.lock().is_empty());
            manager
                .update_credential_profile_arn(1, None)
                .expect("simulate profile_arn_bad_request persistence clear");
            assert!(
                ensure_profile_for_credential(&provider, &manager, 1)
                    .await
                    .is_some()
            );
            assert_eq!(
                server.state.profile_hits(&token),
                2,
                "round {round}: cleared state should allow one fresh discovery"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn profile_discovery_state_saturation_is_fail_closed_for_auxiliary_http() {
        let server = FakeBadRequestServer::start().await;
        let credentials = (1..=3)
            .map(|id| fake_external_idp_credential(id, &format!("very-slow-state-capacity-{id}")))
            .collect();
        let (provider, manager) = fake_profile_provider(
            &server.base_url,
            credentials,
            Some(short_profile_discovery_policy(2)),
        );

        let profiles = concurrently_ensure_profiles(provider.clone(), manager, &[1, 2, 3], 1).await;
        let resolved = profiles.iter().filter(|profile| profile.is_some()).count();
        assert_eq!(resolved, 2, "only bounded entries may start auxiliary HTTP");
        assert_eq!(
            server.state.scenario_hits("profile_discovery_success"),
            2,
            "state saturation must suppress, not bypass, singleflight coordination"
        );
        assert_eq!(provider.profile_arn_discovery_entries.lock().len(), 2);
        assert_eq!(
            provider
                .profile_arn_discovery_metrics()
                .state_capacity_suppressions,
            1
        );
    }

    #[tokio::test]
    async fn existing_real_profile_arn_has_zero_auxiliary_state_or_http_work() {
        let server = FakeBadRequestServer::start().await;
        let mut credential = fake_external_idp_credential(1, "existing-profile");
        credential.profile_arn =
            Some("arn:aws:codewhisperer:us-east-1:123456789012:profile/EXISTING".to_string());
        let (provider, manager) = fake_profile_provider(&server.base_url, vec![credential], None);

        for round in 1..=5 {
            for _ in 0..1_000 {
                assert!(
                    ensure_profile_for_credential(&provider, &manager, 1)
                        .await
                        .is_some(),
                    "round {round}: existing ARN should remain available"
                );
            }
        }
        assert_eq!(server.state.profile_hits("existing-profile"), 0);
        assert_eq!(
            provider.profile_arn_discovery_metrics(),
            super::ProfileArnDiscoveryMetricsSnapshot::default()
        );
        assert!(provider.profile_arn_discovery_entries.lock().is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn auxiliary_focus_profile_budget_rejection_does_not_create_negative_backoff_for_five_rounds()
     {
        let server = FakeBadRequestServer::start().await;
        for round in 1..=5 {
            let token = format!("profile-budget-suppression-{round}");
            let (provider, manager) = fake_profile_provider(
                &server.base_url,
                vec![fake_external_idp_credential(1, &token)],
                None,
            );
            let mut context = manager
                .acquire_context_for_credential(1)
                .await
                .expect("acquire profile budget context");
            let config = provider.runtime_config();
            let budget = AuxiliaryAttemptBudget::new(1);
            budget
                .reserve(AuxiliaryAttemptKind::TokenRefresh)
                .expect("simulate prior request refresh send");

            provider
                .ensure_profile_arn_for_context(
                    &mut context,
                    &config,
                    "fake-machine",
                    Some(&budget),
                )
                .await;
            assert!(context.credentials.profile_arn.is_none());
            assert_eq!(server.state.profile_hits(&token), 0);
            let suppressed = provider.profile_arn_discovery_metrics();
            assert_eq!(suppressed.upstream_attempts, 0);
            assert_eq!(suppressed.negative_results, 0);
            assert_eq!(suppressed.request_budget_suppressions, 1);
            let process_limit = manager.auxiliary_concurrency_snapshot();
            assert_eq!(process_limit.in_flight, 0);
            assert_eq!(process_limit.peak_in_flight, 0);

            let recovery_budget = AuxiliaryAttemptBudget::new(1);
            provider
                .ensure_profile_arn_for_context(
                    &mut context,
                    &config,
                    "fake-machine",
                    Some(&recovery_budget),
                )
                .await;
            assert!(context.credentials.profile_arn.is_some());
            assert_eq!(server.state.profile_hits(&token), 1);
            assert_eq!(recovery_budget.snapshot().profile_discovery_attempts, 1);
            let recovered = provider.profile_arn_discovery_metrics();
            assert_eq!(recovered.upstream_attempts, 1);
            assert_eq!(recovered.successes, 1);
            assert_eq!(recovered.negative_results, 0);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn auxiliary_focus_profile_concurrency_rejection_does_not_create_negative_backoff_for_five_rounds()
     {
        let server = FakeBadRequestServer::start().await;
        for round in 1..=5 {
            let token = format!("profile-concurrency-suppression-{round}");
            let (provider, manager) = fake_profile_provider(
                &server.base_url,
                vec![fake_external_idp_credential(1, &token)],
                None,
            );
            manager
                .update_runtime_config(|config| {
                    config.auxiliary_upstream_max_concurrent_requests = 1;
                })
                .expect("set focused auxiliary concurrency limit");
            let controller = manager.auxiliary_concurrency_controller();
            let held = controller
                .try_acquire(AuxiliaryConcurrencyKind::TokenRefresh)
                .expect("hold the only auxiliary permit");
            let mut context = manager
                .acquire_context_for_credential(1)
                .await
                .expect("acquire profile concurrency context");
            let config = provider.runtime_config();

            provider
                .ensure_profile_arn_for_context(&mut context, &config, "fake-machine", None)
                .await;
            assert!(context.credentials.profile_arn.is_none());
            assert_eq!(server.state.profile_hits(&token), 0);
            let suppressed = provider.profile_arn_discovery_metrics();
            assert_eq!(suppressed.upstream_attempts, 0);
            assert_eq!(suppressed.negative_results, 0);
            assert_eq!(suppressed.concurrency_suppressions, 1);

            drop(held);
            provider
                .ensure_profile_arn_for_context(&mut context, &config, "fake-machine", None)
                .await;
            assert!(context.credentials.profile_arn.is_some());
            assert_eq!(server.state.profile_hits(&token), 1);
            let recovered = provider.profile_arn_discovery_metrics();
            assert_eq!(recovered.upstream_attempts, 1);
            assert_eq!(recovered.successes, 1);
            assert_eq!(recovered.negative_results, 0);
        }
    }

    #[tokio::test]
    async fn auxiliary_focus_profile_and_model_permits_precede_client_cache_misses_for_five_rounds()
    {
        let server = FakeBadRequestServer::start().await;
        for round in 1..=5 {
            let mut profile_credential =
                fake_external_idp_credential(1, &format!("permit-profile-{round}"));
            profile_credential.proxy_url = Some(format!("http://127.0.0.1:{}", 31_000 + round));
            let (profile_provider, profile_manager) =
                fake_profile_provider(&server.base_url, vec![profile_credential], None);
            profile_manager
                .update_runtime_config(|config| {
                    config.auxiliary_upstream_max_concurrent_requests = 1;
                })
                .unwrap();
            let profile_controller = profile_manager.auxiliary_concurrency_controller();
            let held_profile = profile_controller
                .try_acquire(AuxiliaryConcurrencyKind::TokenRefresh)
                .unwrap();
            let profile_context = profile_manager
                .acquire_context_for_credential(1)
                .await
                .unwrap();
            let profile_error = profile_provider
                .fetch_enterprise_profile_arn_for_context(
                    &profile_context,
                    &profile_provider.runtime_config(),
                    "fake-machine",
                    None,
                )
                .await
                .unwrap_err();
            assert!(
                profile_error
                    .downcast_ref::<AuxiliaryConcurrencySaturated>()
                    .is_some()
            );
            assert_eq!(profile_provider.client_cache.lock().entries.len(), 1);
            drop(held_profile);

            let mut model_credential = fake_bad_request_credentials(1).remove(0);
            model_credential.proxy_url = Some(format!("http://127.0.0.1:{}", 32_000 + round));
            let (model_provider, model_manager) =
                fake_profile_provider(&server.base_url, vec![model_credential], None);
            model_manager
                .update_runtime_config(|config| {
                    config.auxiliary_upstream_max_concurrent_requests = 1;
                })
                .unwrap();
            let model_controller = model_manager.auxiliary_concurrency_controller();
            let held_model = model_controller
                .try_acquire(AuxiliaryConcurrencyKind::TokenRefresh)
                .unwrap();
            let model_context = model_manager
                .acquire_context_for_credential(1)
                .await
                .unwrap();
            let model_error = model_provider
                .list_available_models_for_context(model_context)
                .await
                .unwrap_err();
            assert!(
                model_error
                    .downcast_ref::<AuxiliaryConcurrencySaturated>()
                    .is_some()
            );
            assert_eq!(model_provider.client_cache.lock().entries.len(), 1);
            drop(held_model);
        }
    }

    #[tokio::test]
    async fn auxiliary_focus_provider_client_cache_is_bounded_and_reuses_hot_keys_for_five_rounds()
    {
        let server = FakeBadRequestServer::start().await;
        for round in 0..5 {
            let (provider, _) =
                fake_profile_provider(&server.base_url, fake_bad_request_credentials(1), None);
            let mut last_credential = None;
            for index in 0..=KIRO_CLIENT_CACHE_MAX_ENTRIES {
                let mut credential = fake_bad_request_credentials(1).remove(0);
                credential.proxy_url = Some(format!(
                    "http://127.0.0.1:{}",
                    33_000 + round * 1_000 + index
                ));
                provider.client_for(&credential).unwrap();
                last_credential = Some(credential);
            }
            assert_eq!(
                provider.client_cache.lock().entries.len(),
                KIRO_CLIENT_CACHE_MAX_ENTRIES
            );
            assert_eq!(
                provider.client_cache_builds.load(Ordering::Acquire),
                (KIRO_CLIENT_CACHE_MAX_ENTRIES + 2) as u64
            );

            let last_credential = last_credential.unwrap();
            let key = last_credential.effective_proxy(None);
            let before = provider.client_cache.lock().entries[&key].client.clone();
            provider.client_for(&last_credential).unwrap();
            let after = provider.client_cache.lock().entries[&key].client.clone();
            assert!(Arc::ptr_eq(&before, &after));

            let mut shared_credential = fake_bad_request_credentials(1).remove(0);
            shared_credential.proxy_url = Some(format!("http://127.0.0.1:{}", 40_000 + round));
            let builds_before = provider.client_cache_builds.load(Ordering::Acquire);
            let barrier = Arc::new(std::sync::Barrier::new(32));
            let threads = (0..32)
                .map(|_| {
                    let provider = provider.clone();
                    let credential = shared_credential.clone();
                    let barrier = barrier.clone();
                    std::thread::spawn(move || {
                        barrier.wait();
                        provider.client_for(&credential)
                    })
                })
                .collect::<Vec<_>>();
            for thread in threads {
                thread.join().unwrap().unwrap();
            }
            assert_eq!(
                provider.client_cache_builds.load(Ordering::Acquire),
                builds_before + 1
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn auxiliary_focus_profile_discovery_is_accounted_separately_from_inference_budget_for_five_rounds()
     {
        let server = FakeBadRequestServer::start().await;
        for round in 1..=5 {
            let token = format!("budget-profile-{round}");
            let (provider, _manager) = fake_profile_provider(
                &server.base_url,
                vec![fake_external_idp_credential(1, &token)],
                None,
            );
            let request_body = serde_json::json!({
                "testScenario": "generic_body_invalid",
                "conversationState": {
                    "conversationId": format!("profile-budget-{round}"),
                    "currentMessage": {
                        "userInputMessage": {
                            "content": "test",
                            "modelId": "claude-sonnet-4"
                        }
                    }
                }
            })
            .to_string();
            let budget = Arc::new(InferenceAttemptBudget::new(4));
            provider
                .call_api_with_context_with_request_id_and_attempt_budget(
                    &request_body,
                    Some("req-profile-budget"),
                    AcquireMode::WaitForCapacity,
                    1,
                    Some("claude-sonnet-4"),
                    budget.clone(),
                    false,
                )
                .await
                .err()
                .expect("controlled inference endpoint returns deterministic 400");

            let inference = budget.snapshot();
            assert_eq!(inference.consumed, 1, "round {round}");
            assert_eq!(inference.local_attempts, 1, "round {round}");
            assert_eq!(inference.external_attempts, 0, "round {round}");
            let request_auxiliary = budget.auxiliary_snapshot();
            assert_eq!(request_auxiliary.consumed, 1, "round {round}");
            assert_eq!(
                request_auxiliary.profile_discovery_attempts, 1,
                "round {round}"
            );
            assert_eq!(request_auxiliary.token_refresh_attempts, 0, "round {round}");
            let auxiliary = provider.profile_arn_discovery_metrics();
            assert_eq!(auxiliary.upstream_attempts, 1, "round {round}");
            assert_eq!(auxiliary.successes, 1, "round {round}");
            assert_eq!(server.state.profile_hits(&token), 1, "round {round}");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn model_discovery_fanout_is_bounded_and_concurrent_runs_are_rejected() {
        const SCENARIO: &str = "model_discovery_failure";
        let server = FakeBadRequestServer::start().await;
        let mut config = Config::default();
        config.kiro_upstream_base_url = Some(server.base_url.clone());
        config.kiro_upstream_response_timeout_secs = 2;
        let mut credentials = fake_bad_request_credentials(60);
        for (index, credential) in credentials.iter_mut().enumerate() {
            credential.subscription_title = Some(format!("capability-class-{index}"));
        }
        let manager = Arc::new(
            MultiTokenManager::new(config, credentials, None, None, false)
                .expect("fake model discovery token manager"),
        );
        let mut endpoints: HashMap<String, Arc<dyn KiroEndpoint>> = HashMap::new();
        endpoints.insert("ide".to_string(), Arc::new(IdeEndpoint));
        let provider = Arc::new(KiroProvider::with_proxy(
            manager,
            None,
            endpoints,
            "ide".to_string(),
        ));

        for round in 1..=5 {
            let hits_before = server.state.scenario_hits(SCENARIO);
            let running_provider = provider.clone();
            let running =
                tokio::spawn(async move { running_provider.list_available_models().await });
            tokio::time::timeout(std::time::Duration::from_secs(1), async {
                while server.state.scenario_hits(SCENARIO) == hits_before {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("first model discovery request should reach the fake upstream");

            let concurrent_error = provider
                .list_available_models()
                .await
                .expect_err("concurrent model discovery must be rejected");
            assert!(
                concurrent_error.to_string().contains("already in progress"),
                "round {round}: {concurrent_error}"
            );

            let bounded_error = running
                .await
                .expect("model discovery task should join")
                .expect_err("controlled upstream always fails");
            assert!(
                bounded_error.to_string().contains("4/60"),
                "round {round}: {bounded_error}"
            );
            assert_eq!(
                server.state.scenario_hits(SCENARIO) - hits_before,
                MODEL_DISCOVERY_MAX_CREDENTIAL_ATTEMPTS,
                "round {round}: auxiliary fan-out must not scale with account count"
            );
        }
    }

    #[test]
    fn model_discovery_reasoning_contract_intersects_or_rejects_heterogeneous_cohorts_five_rounds()
    {
        fn schema(field: &str, efforts: &[&str], default: Option<&str>) -> serde_json::Value {
            let mut effort = serde_json::json!({"type": "string", "enum": efforts});
            if let Some(default) = default {
                effort["default"] = serde_json::json!(default);
            }
            serde_json::json!({
                "type": "object",
                "properties": {
                    (field): {
                        "type": "object",
                        "properties": {"effort": effort}
                    }
                }
            })
        }
        fn model(schema: Option<serde_json::Value>) -> KiroAvailableModel {
            KiroAvailableModel {
                model_id: "claude-cohort-test".to_string(),
                additional_model_request_fields_schema: schema,
                ..Default::default()
            }
        }

        for round in 0..5 {
            let common = super::merge_model_discovery_catalogs(
                vec![
                    vec![model(Some(schema(
                        "output_config",
                        &["low", "high", "max"],
                        Some("high"),
                    )))],
                    vec![model(Some(schema(
                        "output_config",
                        &["medium", "high", "max"],
                        Some("high"),
                    )))],
                ],
                true,
            );
            let capability =
                crate::anthropic::model_capabilities::KiroReasoningFieldCapability::from_schema(
                    common[0]
                        .additional_model_request_fields_schema
                        .as_ref()
                        .expect("common schema"),
                )
                .unwrap_or_else(|| panic!("round {round}: common capability"));
            assert_eq!(capability.efforts, ["high", "max"].map(str::to_string));
            assert_eq!(capability.default_effort.as_deref(), Some("high"));

            let default_mismatch = super::merge_model_discovery_catalogs(
                vec![
                    vec![model(Some(schema(
                        "output_config",
                        &["high", "max"],
                        Some("high"),
                    )))],
                    vec![model(Some(schema(
                        "output_config",
                        &["high", "max"],
                        Some("max"),
                    )))],
                ],
                true,
            );
            let capability =
                crate::anthropic::model_capabilities::KiroReasoningFieldCapability::from_schema(
                    default_mismatch[0]
                        .additional_model_request_fields_schema
                        .as_ref()
                        .expect("default mismatch schema"),
                )
                .unwrap_or_else(|| panic!("round {round}: default intersection"));
            assert_eq!(capability.default_effort, None);

            for unsafe_catalogs in [
                vec![
                    vec![model(Some(schema(
                        "output_config",
                        &["high"],
                        Some("high"),
                    )))],
                    vec![model(Some(schema("reasoning", &["high"], Some("high"))))],
                ],
                vec![
                    vec![model(Some(schema(
                        "output_config",
                        &["high"],
                        Some("high"),
                    )))],
                    vec![KiroAvailableModel {
                        model_id: "other-model".to_string(),
                        ..Default::default()
                    }],
                ],
            ] {
                let merged = super::merge_model_discovery_catalogs(unsafe_catalogs, true);
                let target = merged
                    .iter()
                    .find(|model| model.model_id == "claude-cohort-test")
                    .unwrap_or_else(|| panic!("round {round}: target model"));
                assert_eq!(
                    target.additional_model_request_fields_schema,
                    Some(serde_json::Value::Null),
                    "round {round}: heterogeneous path/missing model must fail closed"
                );
            }

            let absent = super::merge_model_discovery_catalogs(
                vec![
                    vec![model(Some(schema(
                        "output_config",
                        &["high"],
                        Some("high"),
                    )))],
                    vec![model(None)],
                ],
                true,
            );
            assert!(absent[0].additional_model_request_fields_schema.is_none());

            let incomplete = super::merge_model_discovery_catalogs(
                vec![vec![model(Some(schema(
                    "output_config",
                    &["high"],
                    Some("high"),
                )))]],
                false,
            );
            assert_eq!(
                incomplete[0].additional_model_request_fields_schema,
                Some(serde_json::Value::Null),
                "round {round}: partial cohort observation"
            );
        }
    }

    #[test]
    fn model_capability_cohort_keys_scale_by_class_not_account_count_five_rounds() {
        let mut credentials = fake_bad_request_credentials(4_096);
        for (index, credential) in credentials.iter_mut().enumerate() {
            credential.subscription_title = Some(format!("class-{}", index % 3));
        }
        let manager = MultiTokenManager::new(Config::default(), credentials, None, None, false)
            .expect("large capability cohort fixture");
        for round in 0..5 {
            assert_eq!(
                manager.local_model_capability_cohorts().len(),
                3,
                "round {round}: 4096 accounts collapse to three static cohorts"
            );
        }
        let hot_keys = manager.local_model_capability_cohort_keys();
        for _ in 0..10_000 {
            let read = manager.local_model_capability_cohort_keys();
            assert!(Arc::ptr_eq(&hot_keys, &read));
        }
        assert_eq!(manager.model_capability_cohort_rebuilds(), 1);
        manager
            .update_runtime_config(|config| {
                config.api_region = Some("eu-west-1".to_string());
            })
            .expect("mutate capability cohort config");
        let rebuilt_keys = manager.local_model_capability_cohort_keys();
        assert!(!Arc::ptr_eq(&hot_keys, &rebuilt_keys));
        assert_eq!(manager.model_capability_cohort_rebuilds(), 2);

        let sixty = MultiTokenManager::new(
            Config::default(),
            fake_bad_request_credentials(60),
            None,
            None,
            false,
        )
        .expect("60-account cohort fixture");
        let sixty_one = MultiTokenManager::new(
            Config::default(),
            fake_bad_request_credentials(61),
            None,
            None,
            false,
        )
        .expect("same-cohort account addition fixture");
        assert_eq!(
            sixty
                .local_model_capability_cohorts()
                .iter()
                .map(|cohort| cohort.key.clone())
                .collect::<Vec<_>>(),
            sixty_one
                .local_model_capability_cohorts()
                .iter()
                .map(|cohort| cohort.key.clone())
                .collect::<Vec<_>>()
        );

        for round in 0..5 {
            for mutate in 0..4 {
                let mut credentials = fake_bad_request_credentials(2);
                match mutate {
                    0 => credentials[1].endpoint = Some("cli".to_string()),
                    1 => credentials[1].api_region = Some("eu-west-1".to_string()),
                    2 => credentials[1].subscription_title = Some("KIRO FREE".to_string()),
                    _ => credentials[1].supported_models = vec!["claude-haiku-4.5".to_string()],
                }
                let manager =
                    MultiTokenManager::new(Config::default(), credentials, None, None, false)
                        .expect("heterogeneous cohort fixture");
                assert_eq!(
                    manager.local_model_capability_cohorts().len(),
                    2,
                    "round {round}: endpoint/region/subscription/model support class {mutate}"
                );
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn model_discovery_pagination_success_cycle_and_whole_run_budget_repeat_five_rounds() {
        let server = FakeBadRequestServer::start().await;

        let success =
            fake_model_discovery_provider(&server.base_url, ["pagination-success".to_string()]);
        for round in 1..=5 {
            let hits_before = server
                .state
                .scenario_hits("model_discovery_pagination_success");
            let models = success
                .list_available_models()
                .await
                .unwrap_or_else(|error| panic!("round {round}: pagination failed: {error}"));
            assert_eq!(
                models
                    .iter()
                    .map(|model| model.model_id.as_str())
                    .collect::<Vec<_>>(),
                vec!["success-model-0", "success-model-1", "success-model-2"],
                "round {round}"
            );
            assert_eq!(
                server
                    .state
                    .scenario_hits("model_discovery_pagination_success")
                    - hits_before,
                3,
                "round {round}"
            );
        }

        let cyclic =
            fake_model_discovery_provider(&server.base_url, ["pagination-cycle".to_string()]);
        for round in 1..=5 {
            let hits_before = server
                .state
                .scenario_hits("model_discovery_pagination_cycle");
            let error = cyclic
                .list_available_models()
                .await
                .expect_err("repeated pagination token must fail closed");
            assert!(
                error.to_string().contains("repeated pagination token"),
                "round {round}: {error}"
            );
            assert_eq!(
                server
                    .state
                    .scenario_hits("model_discovery_pagination_cycle")
                    - hits_before,
                2,
                "round {round}"
            );
        }

        let endless = fake_model_discovery_provider(
            &server.base_url,
            (0..MODEL_DISCOVERY_MAX_CREDENTIAL_ATTEMPTS)
                .map(|index| format!("pagination-endless-{index}")),
        );
        for round in 1..=5 {
            let hits_before = server
                .state
                .scenario_hits("model_discovery_pagination_endless");
            let error = endless
                .list_available_models()
                .await
                .expect_err("endless pagination must exhaust the whole-run send budget");
            assert!(
                error.to_string().contains("HTTP send budget exhausted"),
                "round {round}: {error}"
            );
            assert_eq!(
                server
                    .state
                    .scenario_hits("model_discovery_pagination_endless")
                    - hits_before,
                MODEL_DISCOVERY_MAX_HTTP_SENDS,
                "round {round}: send count must not multiply by credential count"
            );
        }
    }

    async fn call_fake_bad_request_provider(
        server: &FakeBadRequestServer,
        scenario: &str,
        pool_size: usize,
        round: usize,
        prompt_logic_retry_enabled: bool,
    ) -> (usize, Vec<crate::kiro::call_trace::KiroCredentialAttempt>) {
        let hits_before = server.state.scenario_hits(scenario);
        let total_hits_before = server.state.total_hits.load(Ordering::Relaxed);
        let mut config = Config::default();
        config.kiro_upstream_base_url = Some(server.base_url.clone());
        config.kiro_upstream_response_timeout_secs = 2;
        config.credential_retry_max_attempts = 100;
        config.credential_prompt_logic_retry_enabled = prompt_logic_retry_enabled;
        config.credential_prompt_logic_retry_max_attempts = 100;
        let manager = Arc::new(
            MultiTokenManager::new(
                config,
                fake_bad_request_credentials(pool_size),
                None,
                None,
                false,
            )
            .expect("fake provider token manager"),
        );
        let mut endpoints: HashMap<String, Arc<dyn KiroEndpoint>> = HashMap::new();
        endpoints.insert("ide".to_string(), Arc::new(IdeEndpoint));
        let provider = KiroProvider::with_proxy(manager, None, endpoints, "ide".to_string());
        let request_body = serde_json::json!({
            "testScenario": scenario,
            "conversationState": {
                "conversationId": format!("bad-request-{scenario}-{pool_size}-{round}"),
                "currentMessage": {
                    "userInputMessage": {
                        "content": "test",
                        "modelId": "claude-sonnet-4"
                    }
                }
            }
        })
        .to_string();
        let budget = Arc::new(InferenceAttemptBudget::new(4));
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            provider.call_api_with_context_with_request_id_and_attempt_budget(
                &request_body,
                Some("req-fake-bad-request"),
                AcquireMode::WaitForCapacity,
                1,
                Some("claude-sonnet-4"),
                budget.clone(),
                false,
            ),
        )
        .await
        .expect("fake bad-request provider call timed out")
        .err()
        .expect("fake upstream always returns 400");
        let hits = server.state.scenario_hits(scenario) - hits_before;
        assert_eq!(
            server.state.total_hits.load(Ordering::Relaxed) - total_hits_before,
            hits,
            "unexpected auxiliary upstream hit for {scenario}, pool {pool_size}, round {round}"
        );
        let snapshot = budget.snapshot();
        assert_eq!(snapshot.consumed as usize, hits);
        assert_eq!(snapshot.local_attempts as usize, hits);
        assert_eq!(snapshot.external_attempts, 0);
        (hits, KiroProvider::attempts_from_error(&error))
    }

    fn fake_thinking_signature_provider(
        base_url: &str,
        pool_size: usize,
    ) -> (KiroProvider, Arc<MultiTokenManager>) {
        let mut config = Config::default();
        config.kiro_upstream_base_url = Some(base_url.to_string());
        config.kiro_upstream_response_timeout_secs = 2;
        config.credential_retry_max_attempts = 100;
        config.credential_rate_limit_cooldown_secs = 60;
        config.credential_server_error_cooldown_secs = 60;
        config.credential_network_error_cooldown_secs = 60;
        config.credential_protocol_error_cooldown_secs = 60;
        config.credential_cooldown_jitter_percent = 0;
        let manager = Arc::new(
            MultiTokenManager::new(
                config,
                fake_bad_request_credentials(pool_size),
                None,
                None,
                false,
            )
            .expect("thinking signature retry token manager"),
        );
        let mut endpoints: HashMap<String, Arc<dyn KiroEndpoint>> = HashMap::new();
        endpoints.insert("ide".to_string(), Arc::new(IdeEndpoint));
        (
            KiroProvider::with_proxy(manager.clone(), None, endpoints, "ide".to_string()),
            manager,
        )
    }

    fn thinking_signature_request_body(scenario: &str, round: usize) -> String {
        serde_json::json!({
            "testScenario": scenario,
            "secretMarker": format!("PRIVATE_SIGNATURE_RESPONSE_{scenario}_{round}"),
            "keepMarker": format!("KEEP_SIGNATURE_REQUEST_{scenario}_{round}"),
            "conversationState": {
                "conversationId": format!("thinking-signature-{scenario}-{round}"),
                "history": [{
                    "assistantResponseMessage": {
                        "content": format!("visible-history-{round}"),
                        "toolUses": [{
                            "toolUseId": format!("tool-{round}"),
                            "name": "read",
                            "input": {"path": "README.md"}
                        }],
                        "reasoningContent": {
                            "reasoningText": {
                                "text": format!("private-thought-{round}"),
                                "signature": format!("invalid-signature-{round}")
                            }
                        }
                    }
                }],
                "currentMessage": {
                    "userInputMessage": {
                        "content": "continue",
                        "modelId": "claude-sonnet-4"
                    }
                }
            }
        })
        .to_string()
    }

    fn strip_reasoning_content_for_provider_test(body: &str) -> String {
        let mut value: serde_json::Value =
            serde_json::from_str(body).expect("thinking signature fixture is JSON");
        let history = value
            .pointer_mut("/conversationState/history")
            .and_then(serde_json::Value::as_array_mut)
            .expect("thinking signature fixture history");
        for message in history {
            if let Some(assistant) = message
                .get_mut("assistantResponseMessage")
                .and_then(serde_json::Value::as_object_mut)
            {
                assistant.remove("reasoningContent");
            }
        }
        serde_json::to_string(&value).expect("serialize stripped thinking signature fixture")
    }

    async fn call_thinking_signature_retry<F>(
        provider: &KiroProvider,
        request_body: &str,
        is_stream: bool,
        budget: Arc<InferenceAttemptBudget>,
        preserve_external_attempt: bool,
        max_sends: Option<usize>,
        retry_body_builder: F,
    ) -> anyhow::Result<(u64, Vec<crate::kiro::call_trace::KiroCredentialAttempt>)>
    where
        F: FnOnce() -> anyhow::Result<String> + Send,
    {
        if is_stream {
            let response = provider
                .call_api_stream_with_request_id_and_thinking_signature_retry(
                    request_body,
                    Some("req-thinking-signature-test"),
                    AcquireMode::WaitForCapacity,
                    1,
                    Some("claude-sonnet-4"),
                    budget,
                    preserve_external_attempt,
                    max_sends,
                    retry_body_builder,
                )
                .await?;
            let (_, completion) = response.into_parts();
            let credential_id = completion.credential_id();
            let attempts = completion.attempts().to_vec();
            completion.report_success();
            Ok((credential_id, attempts))
        } else {
            let response = provider
                .call_api_with_context_with_request_id_and_thinking_signature_retry(
                    request_body,
                    Some("req-thinking-signature-test"),
                    AcquireMode::WaitForCapacity,
                    1,
                    Some("claude-sonnet-4"),
                    budget,
                    preserve_external_attempt,
                    max_sends,
                    retry_body_builder,
                )
                .await?;
            let (_, completion) = response.into_parts();
            let credential_id = completion.credential_id();
            let attempts = completion.attempts().to_vec();
            completion.report_success();
            Ok((credential_id, attempts))
        }
    }

    fn assert_signature_retry_did_not_cool_down(manager: &MultiTokenManager, context: &str) {
        let snapshot = manager.snapshot();
        assert!(
            snapshot.entries.iter().all(|entry| !entry.cooled_down),
            "{context}: thinking signature compatibility retry must not cool credentials"
        );
        assert!(
            snapshot
                .entries
                .iter()
                .all(|entry| entry.last_error_kind.is_none()),
            "{context}: compatibility retry must not record transient failure kinds"
        );
    }

    struct ProviderFailureOutcome {
        marker: String,
        error_text: String,
        attempts: Vec<crate::kiro::call_trace::KiroCredentialAttempt>,
        consumed_sends: usize,
        scheduler_snapshot: String,
        cooldown_kinds: Vec<Option<String>>,
        cooldown_reasons: Vec<String>,
        cooled_down: Vec<bool>,
    }

    const PROVIDER_FAILURE_MATRIX_MAX_IN_FLIGHT: usize = 4;

    async fn call_fake_provider_failure(
        server: &FakeBadRequestServer,
        scenario: &str,
        pool_size: usize,
        round: usize,
        is_stream: bool,
        response_timeout_secs: u64,
    ) -> ProviderFailureOutcome {
        let marker = format!(
            "PRIVATE_PROVIDER_MARKER_{scenario}_{pool_size}_{round}_{}",
            if is_stream { "stream" } else { "nonstream" }
        );
        let mut config = Config::default();
        config.kiro_upstream_base_url = Some(server.base_url.clone());
        config.kiro_upstream_response_timeout_secs = response_timeout_secs;
        config.credential_retry_max_attempts = 100;
        config.credential_rate_limit_cooldown_secs = 60;
        config.credential_server_error_cooldown_secs = 60;
        config.credential_network_error_cooldown_secs = 60;
        config.credential_protocol_error_cooldown_secs = 60;
        config.credential_auth_error_cooldown_secs = 60;
        config.credential_max_cooldown_secs = 60;
        config.credential_cooldown_jitter_percent = 0;
        let manager = Arc::new(
            MultiTokenManager::new(
                config,
                fake_bad_request_credentials(pool_size),
                None,
                None,
                false,
            )
            .expect("provider failure token manager"),
        );
        let mut endpoints: HashMap<String, Arc<dyn KiroEndpoint>> = HashMap::new();
        endpoints.insert("ide".to_string(), Arc::new(IdeEndpoint));
        let provider =
            KiroProvider::with_proxy(manager.clone(), None, endpoints, "ide".to_string());
        let request_body = serde_json::json!({
            "testScenario": scenario,
            "secretMarker": marker,
            "conversationState": {
                "conversationId": format!("provider-failure-{scenario}-{pool_size}-{round}-{is_stream}"),
                "currentMessage": {
                    "userInputMessage": {
                        "content": "test",
                        "modelId": "claude-sonnet-4"
                    }
                }
            }
        })
        .to_string();
        let budget = Arc::new(InferenceAttemptBudget::new(4));
        let call = async {
            if is_stream {
                provider
                    .call_api_stream_with_request_id_and_attempt_budget(
                        &request_body,
                        Some("req-provider-failure"),
                        AcquireMode::WaitForCapacity,
                        1,
                        Some("claude-sonnet-4"),
                        budget.clone(),
                        false,
                    )
                    .await
                    .map(|_| ())
            } else {
                provider
                    .call_api_with_context_with_request_id_and_attempt_budget(
                        &request_body,
                        Some("req-provider-failure"),
                        AcquireMode::WaitForCapacity,
                        1,
                        Some("claude-sonnet-4"),
                        budget.clone(),
                        false,
                    )
                    .await
                    .map(|_| ())
            }
        };
        let error = tokio::time::timeout(std::time::Duration::from_secs(30), call)
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "provider failure call timed out: scenario={scenario} stream={is_stream} pool={pool_size} round={round}"
                )
            })
            .expect_err("controlled provider upstream must fail");
        let attempts = KiroProvider::attempts_from_error(&error);
        let budget_snapshot = budget.snapshot();
        let snapshot = manager.snapshot();
        let scheduler_snapshot =
            serde_json::to_string(&snapshot).expect("serialize provider scheduler snapshot");
        let cooldown_kinds = snapshot
            .entries
            .iter()
            .map(|entry| entry.last_error_kind.clone())
            .collect();
        let cooldown_reasons = snapshot
            .entries
            .iter()
            .flat_map(|entry| entry.cooldowns.iter())
            .filter_map(|cooldown| cooldown.reason.clone())
            .collect();
        let cooled_down = snapshot
            .entries
            .iter()
            .map(|entry| entry.cooled_down)
            .collect();
        ProviderFailureOutcome {
            marker,
            error_text: error.to_string(),
            attempts,
            consumed_sends: budget_snapshot.consumed as usize,
            scheduler_snapshot,
            cooldown_kinds,
            cooldown_reasons,
            cooled_down,
        }
    }

    #[test]
    fn extracts_model_and_conversation_id_from_kiro_request() {
        let body = r#"{
            "conversationState": {
                "conversationId": "session-123",
                "currentMessage": {
                    "userInputMessage": {
                        "modelId": "claude-opus-4"
                    }
                }
            }
        }"#;

        assert_eq!(
            KiroProvider::test_extract_conversation_id_from_request(body).as_deref(),
            Some("session-123")
        );
        assert_eq!(
            KiroProvider::test_extract_model_from_request(body).as_deref(),
            Some("claude-opus-4")
        );
    }

    #[test]
    fn ignores_blank_conversation_id() {
        let body = r#"{"conversationState":{"conversationId":"  "}}"#;

        assert_eq!(
            KiroProvider::test_extract_conversation_id_from_request(body),
            None
        );
    }

    #[test]
    fn credential_log_label_always_includes_id() {
        assert_eq!(KiroProvider::format_credential_log_label(6, None), "#6");
        assert_eq!(
            KiroProvider::format_credential_log_label(6, Some("prevotrj@gmail.com".to_string())),
            "#6 prevotrj@gmail.com"
        );
        assert_eq!(
            KiroProvider::format_credential_log_label(6, Some("#6 custom".to_string())),
            "#6 custom"
        );
    }

    #[test]
    fn detects_risk_controlled_upstream_errors() {
        assert_eq!(
            KiroProvider::detect_risk_control_error(
                reqwest::StatusCode::FORBIDDEN,
                r#"{"reason":"TEMPORARILY_SUSPENDED","message":"User ID is temporarily suspended"}"#
            ),
            Some(CredentialRiskControlReason::TemporarilySuspended)
        );
        assert_eq!(
            KiroProvider::detect_risk_control_error(
                reqwest::StatusCode::FORBIDDEN,
                r#"{"message":"Your User ID temporarily is suspended. We've locked your account as a security precaution.","reason":null}"#
            ),
            Some(CredentialRiskControlReason::TemporarilySuspended)
        );
        assert_eq!(
            KiroProvider::detect_risk_control_error(
                reqwest::StatusCode::TOO_MANY_REQUESTS,
                r#"{"message":"Due to suspicious activity, we are imposing temporary limits on your account."}"#
            ),
            Some(CredentialRiskControlReason::TemporarilySuspended)
        );
        assert_eq!(
            KiroProvider::detect_risk_control_error(
                reqwest::StatusCode::FORBIDDEN,
                r#"{"__type":"AccountSuspendedException","message":"Account suspended"}"#
            ),
            Some(CredentialRiskControlReason::AccountSuspended)
        );
        assert_eq!(
            KiroProvider::detect_risk_control_error(
                reqwest::StatusCode::LOCKED,
                r#"{"message":"Locked"}"#
            ),
            Some(CredentialRiskControlReason::AccountLocked)
        );
        assert_eq!(
            KiroProvider::detect_risk_control_error(
                reqwest::StatusCode::FORBIDDEN,
                r#"{"message":"We've locked your account as a security precaution."}"#
            ),
            Some(CredentialRiskControlReason::AccountLocked)
        );
        assert_eq!(
            KiroProvider::detect_risk_control_error(
                reqwest::StatusCode::FORBIDDEN,
                r#"{"message":"The bearer token included in the request is invalid"}"#
            ),
            None
        );
        assert_eq!(
            KiroProvider::detect_risk_control_error(
                reqwest::StatusCode::FORBIDDEN,
                r#"{"message":"User is not authorized to make this call.","reason":null}"#
            ),
            None
        );
    }

    #[test]
    fn downgrades_only_429_temporary_risk_when_credential_opted_out() {
        let opted_out = KiroCredentials {
            rate_limit_auto_disable_enabled: Some(false),
            ..Default::default()
        };
        let default_credential = KiroCredentials::default();

        assert!(KiroProvider::should_downgrade_rate_limit_risk_to_cooldown(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            CredentialRiskControlReason::TemporarilySuspended,
            &opted_out
        ));
        assert!(!KiroProvider::should_downgrade_rate_limit_risk_to_cooldown(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            CredentialRiskControlReason::TemporarilySuspended,
            &default_credential
        ));
        assert!(!KiroProvider::should_downgrade_rate_limit_risk_to_cooldown(
            reqwest::StatusCode::FORBIDDEN,
            CredentialRiskControlReason::TemporarilySuspended,
            &opted_out
        ));
        assert!(!KiroProvider::should_downgrade_rate_limit_risk_to_cooldown(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            CredentialRiskControlReason::AccountSuspended,
            &opted_out
        ));
        assert!(!KiroProvider::should_downgrade_rate_limit_risk_to_cooldown(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            CredentialRiskControlReason::AccountLocked,
            &opted_out
        ));
    }

    #[test]
    fn classifies_bad_request_protocol_reasons() {
        let cases = [
            (
                r#"{"message":"assistant-prefill final message is not supported; last message must be user"}"#,
                "assistant_prefill_bad_request",
            ),
            (
                r#"{"message":"profileArn is required for this request"}"#,
                "profile_arn_bad_request",
            ),
            (
                r#"{"message":"The request body is improperly formed"}"#,
                "malformed_request",
            ),
            (
                r#"{"message":"Invalid JSON in request body"}"#,
                "malformed_request",
            ),
            (
                r#"{"message":"Invalid tool use format.","reason":"REQUEST_BODY_INVALID"}"#,
                "tool_use_format_bad_request",
            ),
            (
                r#"{"error":{"message":"tools.26.custom.input_schema is invalid"},"reason":"TOOL_SCHEMA_INVALID"}"#,
                "tool_use_format_bad_request",
            ),
            (
                r#"{"message":"The request body has an invalid tool-use sequence."}"#,
                "tool_use_format_bad_request",
            ),
            (
                r#"{"message":"unknown model"}"#,
                "model_invalid_bad_request",
            ),
            (
                r#"{"message":"The requested model is not available for this endpoint. If this continues, contact the administrator with error ID: req_01example"}"#,
                "model_unavailable_bad_request",
            ),
            (
                r#"{"error":{"reason":"MODEL_UNAVAILABLE"},"message":"Try another account"}"#,
                "model_unavailable_bad_request",
            ),
            (
                r#"{"reason":"MODEL_NOT_AVAILABLE","message":"Try another account"}"#,
                "model_unavailable_bad_request",
            ),
            (
                r#"{"message":"The model is not supported in this region"}"#,
                "model_unavailable_bad_request",
            ),
            (
                r#"{"message":"The model is not available for this account"}"#,
                "model_unavailable_bad_request",
            ),
            (
                r#"{"message":"Invalid model ID. Please select a different model to continue.","reason":"INVALID_MODEL_ID"}"#,
                "model_invalid_bad_request",
            ),
            (
                r#"{"message":"Image data cannot be empty.","reason":"REQUEST_BODY_INVALID"}"#,
                "image_invalid_bad_request",
            ),
            (
                r#"{"message":"Bedrock could not process image","reason":"IMAGE_FORMAT_UNSUPPORTED"}"#,
                "image_invalid_bad_request",
            ),
            (
                r#"{"message":"Image data cannot be empty in a tool result image.","reason":"REQUEST_BODY_INVALID"}"#,
                "image_invalid_bad_request",
            ),
            (
                r#"{"message":"Request validation failed","reason":"REQUEST_BODY_INVALID"}"#,
                "request_body_invalid_bad_request",
            ),
            (
                r#"{"error":{"reason":"REQUEST_BODY_INVALID"},"message":"A required field is missing"}"#,
                "request_body_invalid_bad_request",
            ),
            (
                r#"{"message":"The requested feature is not available for this endpoint"}"#,
                "bad_request",
            ),
            (
                r#"{"message":"This feature is not available in this region"}"#,
                "bad_request",
            ),
            (
                r#"{"message":"This resource is not supported in this region"}"#,
                "bad_request",
            ),
            (
                r#"{"message":"Feature model_unavailable_preview is disabled"}"#,
                "bad_request",
            ),
            (r#"{"message":"Bad request"}"#, "bad_request"),
        ];

        for round in 0..5 {
            for (body, expected) in cases {
                assert_eq!(
                    KiroProvider::classify_bad_request_reason(body),
                    expected,
                    "classification mismatch in round {round} for body {body}"
                );
            }
        }
    }

    #[test]
    fn model_unavailable_retry_requires_reason_and_model() {
        assert!(KiroProvider::should_retry_model_unavailable_bad_request(
            "model_unavailable_bad_request",
            Some("claude-opus-4-8"),
        ));
        assert!(!KiroProvider::should_retry_model_unavailable_bad_request(
            "bad_request",
            Some("claude-opus-4-8"),
        ));
        assert!(!KiroProvider::should_retry_model_unavailable_bad_request(
            "model_unavailable_bad_request",
            None,
        ));
        assert!(!KiroProvider::should_retry_model_unavailable_bad_request(
            "model_unavailable_bad_request",
            Some("  "),
        ));
    }

    #[test]
    fn thinking_signature_invalid_classifier_is_exact_and_structural_for_five_rounds() {
        let positives = [
            r#"{"reason":"THINKING_SIGNATURE_INVALID"}"#,
            r#"{"error":{"reason":"THINKING_SIGNATURE_INVALID"}}"#,
            r#"{"reason":"OTHER","error":{"reason":"THINKING_SIGNATURE_INVALID"}}"#,
        ];
        let negatives = [
            r#"THINKING_SIGNATURE_INVALID"#,
            r#"{"message":"THINKING_SIGNATURE_INVALID"}"#,
            r#"{"code":"THINKING_SIGNATURE_INVALID"}"#,
            r#"{"reason":"thinking_signature_invalid"}"#,
            r#"{"reason":"PREFIX_THINKING_SIGNATURE_INVALID_SUFFIX"}"#,
            r#"{"error":{"message":"THINKING_SIGNATURE_INVALID"}}"#,
            r#"{"errors":[{"reason":"THINKING_SIGNATURE_INVALID"}]}"#,
            r#"{"reason":null}"#,
            r#"{"reason":400}"#,
            r#"{"reason":"THINKING_SIGNATURE_INVALID""#,
        ];

        for round in 1..=5 {
            for body in positives {
                assert!(
                    KiroProvider::is_thinking_signature_invalid_response(
                        reqwest::StatusCode::BAD_REQUEST,
                        body,
                    ),
                    "round {round}: exact structured reason must match: {body}"
                );
                assert!(
                    !KiroProvider::is_thinking_signature_invalid_response(
                        reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                        body,
                    ),
                    "round {round}: non-400 status must not match"
                );
            }
            for body in negatives {
                assert!(
                    !KiroProvider::is_thinking_signature_invalid_response(
                        reqwest::StatusCode::BAD_REQUEST,
                        body,
                    ),
                    "round {round}: non-exact/non-structural marker must not match: {body}"
                );
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn thinking_signature_retry_success_is_lazy_same_credential_and_bounded_five_rounds() {
        let server = FakeBadRequestServer::start().await;
        for scenario in [
            "thinking_signature_root_success",
            "thinking_signature_nested_success",
            "thinking_signature_json_header_stream_success",
        ] {
            for is_stream in [false, true] {
                let (provider, manager) = fake_thinking_signature_provider(&server.base_url, 3);
                for round in 1..=5 {
                    let request_body = thinking_signature_request_body(scenario, round);
                    let retry_body = strip_reasoning_content_for_provider_test(&request_body);
                    let builder_calls = Arc::new(AtomicUsize::new(0));
                    let builder_counter = builder_calls.clone();
                    let budget = Arc::new(InferenceAttemptBudget::new(4));
                    let captures_before = server.state.signature_requests(scenario).len();
                    let hits_before = server.state.scenario_hits(scenario);

                    let (credential_id, attempts) = call_thinking_signature_retry(
                        &provider,
                        &request_body,
                        is_stream,
                        budget.clone(),
                        false,
                        None,
                        move || {
                            builder_counter.fetch_add(1, Ordering::SeqCst);
                            Ok(retry_body)
                        },
                    )
                    .await
                    .unwrap_or_else(|error| {
                        panic!("{scenario} stream={is_stream} round {round}: {error}")
                    });

                    assert_eq!(
                        server.state.scenario_hits(scenario) - hits_before,
                        2,
                        "{scenario} stream={is_stream} round {round}: exactly two sends"
                    );
                    assert_eq!(builder_calls.load(Ordering::SeqCst), 1);
                    assert_eq!(attempts.len(), 2);
                    assert_eq!(attempts[0].credential_id, credential_id);
                    assert_eq!(attempts[1].credential_id, credential_id);
                    assert_eq!(
                        attempts
                            .iter()
                            .map(|attempt| attempt.credential_id)
                            .collect::<HashSet<_>>()
                            .len(),
                        1,
                        "{scenario} stream={is_stream} round {round}: no credential fan-out"
                    );
                    assert_eq!(
                        attempts[0].action,
                        "thinking_signature_retry_same_credential"
                    );
                    assert_eq!(
                        attempts[1].action,
                        "response_headers_received_after_thinking_signature_retry"
                    );
                    let budget = budget.snapshot();
                    assert_eq!(budget.consumed, 2);
                    assert_eq!(budget.local_attempts, 2);
                    assert_eq!(budget.external_attempts, 0);
                    assert_eq!(budget.mcp_attempts, 0);

                    let captures = server.state.signature_requests(scenario);
                    let pair = &captures[captures_before..];
                    assert_eq!(pair.len(), 2);
                    assert!(
                        pair[0]
                            .body
                            .pointer("/conversationState/history/0/assistantResponseMessage/reasoningContent")
                            .is_some()
                    );
                    assert!(
                        pair[1]
                            .body
                            .pointer("/conversationState/history/0/assistantResponseMessage/reasoningContent")
                            .is_none()
                    );
                    assert_eq!(pair[0].body["keepMarker"], pair[1].body["keepMarker"]);
                    assert_eq!(
                        pair[0].body.pointer(
                            "/conversationState/history/0/assistantResponseMessage/content"
                        ),
                        pair[1].body.pointer(
                            "/conversationState/history/0/assistantResponseMessage/content"
                        )
                    );
                    assert_eq!(
                        pair[0].body.pointer(
                            "/conversationState/history/0/assistantResponseMessage/toolUses"
                        ),
                        pair[1].body.pointer(
                            "/conversationState/history/0/assistantResponseMessage/toolUses"
                        )
                    );
                    assert!(pair[0].authorization.is_some());
                    assert_eq!(pair[0].authorization, pair[1].authorization);
                    assert_signature_retry_did_not_cool_down(
                        &manager,
                        &format!("{scenario} stream={is_stream} round {round}"),
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn thinking_signature_retry_uses_reserved_attempt_locally_and_never_falls_back_five_rounds()
     {
        let server = FakeBadRequestServer::start().await;
        let scenario = "thinking_signature_root_success";
        for is_stream in [false, true] {
            let (provider, manager) = fake_thinking_signature_provider(&server.base_url, 20);
            for round in 1..=5 {
                let request_body = thinking_signature_request_body(scenario, 100 + round);
                let retry_body = strip_reasoning_content_for_provider_test(&request_body);
                let budget = Arc::new(InferenceAttemptBudget::new(2));
                let hits_before = server.state.scenario_hits(scenario);
                let (credential_id, attempts) = call_thinking_signature_retry(
                    &provider,
                    &request_body,
                    is_stream,
                    budget.clone(),
                    true,
                    None,
                    move || Ok(retry_body),
                )
                .await
                .unwrap_or_else(|error| panic!("stream={is_stream} round {round}: {error}"));

                assert_eq!(server.state.scenario_hits(scenario) - hits_before, 2);
                assert_eq!(attempts.len(), 2);
                assert!(
                    attempts
                        .iter()
                        .all(|attempt| attempt.credential_id == credential_id)
                );
                let snapshot = budget.snapshot();
                assert_eq!(snapshot.max_attempts, 2);
                assert_eq!(snapshot.consumed, 2);
                assert_eq!(snapshot.local_attempts, 2);
                assert_eq!(snapshot.external_attempts, 0);
                assert_eq!(snapshot.mcp_attempts, 0);
                assert_signature_retry_did_not_cool_down(
                    &manager,
                    &format!("reserved-attempt stream={is_stream} round {round}"),
                );
            }
        }
    }

    #[tokio::test]
    async fn thinking_signature_retry_ignores_message_code_substring_case_and_status_five_rounds() {
        let server = FakeBadRequestServer::start().await;
        for scenario in [
            "thinking_signature_message_only",
            "thinking_signature_code_only",
            "thinking_signature_lowercase",
            "thinking_signature_substring",
            "thinking_signature_wrong_status",
        ] {
            for is_stream in [false, true] {
                let (provider, manager) = fake_thinking_signature_provider(&server.base_url, 3);
                for round in 1..=5 {
                    let request_body = thinking_signature_request_body(scenario, round);
                    let retry_body = strip_reasoning_content_for_provider_test(&request_body);
                    let builder_calls = Arc::new(AtomicUsize::new(0));
                    let builder_counter = builder_calls.clone();
                    let budget = Arc::new(InferenceAttemptBudget::new(4));
                    let hits_before = server.state.scenario_hits(scenario);
                    let error = call_thinking_signature_retry(
                        &provider,
                        &request_body,
                        is_stream,
                        budget.clone(),
                        false,
                        None,
                        move || {
                            builder_counter.fetch_add(1, Ordering::SeqCst);
                            Ok(retry_body)
                        },
                    )
                    .await
                    .expect_err("non-exact signature marker must stay on normal failure path");

                    assert_eq!(server.state.scenario_hits(scenario) - hits_before, 1);
                    assert_eq!(builder_calls.load(Ordering::SeqCst), 0);
                    assert_eq!(budget.snapshot().consumed, 1);
                    assert_eq!(KiroProvider::attempts_from_error(&error).len(), 1);
                    assert!(!matches!(
                        KiroProvider::call_failure_kind_from_error(&error),
                        Some(
                            KiroCallFailureKind::ThinkingSignatureInvalid
                                | KiroCallFailureKind::ThinkingSignatureRetryFailed
                        )
                    ));
                    assert_signature_retry_did_not_cool_down(
                        &manager,
                        &format!("{scenario} stream={is_stream} round {round}"),
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn thinking_signature_invalid_without_builder_never_retries_five_rounds() {
        let server = FakeBadRequestServer::start().await;
        for is_stream in [false, true] {
            let (provider, manager) = fake_thinking_signature_provider(&server.base_url, 3);
            for round in 1..=5 {
                let scenario = "thinking_signature_root_without_builder";
                let request_body = thinking_signature_request_body(scenario, round);
                let budget = Arc::new(InferenceAttemptBudget::new(4));
                let hits_before = server.state.scenario_hits(scenario);
                let call = async {
                    if is_stream {
                        provider
                            .call_api_stream_with_request_id_and_attempt_budget(
                                &request_body,
                                Some("req-signature-no-builder"),
                                AcquireMode::WaitForCapacity,
                                1,
                                Some("claude-sonnet-4"),
                                budget.clone(),
                                false,
                            )
                            .await
                            .map(|_| ())
                    } else {
                        provider
                            .call_api_with_context_with_request_id_and_attempt_budget(
                                &request_body,
                                Some("req-signature-no-builder"),
                                AcquireMode::WaitForCapacity,
                                1,
                                Some("claude-sonnet-4"),
                                budget.clone(),
                                false,
                            )
                            .await
                            .map(|_| ())
                    }
                };
                let error = call
                    .await
                    .expect_err("exact reason without eligible history must not retry");
                assert_eq!(server.state.scenario_hits(scenario) - hits_before, 1);
                assert_eq!(budget.snapshot().consumed, 1);
                assert_eq!(KiroProvider::attempts_from_error(&error).len(), 1);
                assert!(!matches!(
                    KiroProvider::call_failure_kind_from_error(&error),
                    Some(
                        KiroCallFailureKind::ThinkingSignatureInvalid
                            | KiroCallFailureKind::ThinkingSignatureRetryFailed
                    )
                ));
                assert_signature_retry_did_not_cool_down(
                    &manager,
                    &format!("no-builder stream={is_stream} round {round}"),
                );
            }
        }
    }

    #[tokio::test]
    async fn thinking_signature_second_response_always_terminates_typed_and_bounded_five_rounds() {
        let server = FakeBadRequestServer::start().await;
        for (scenario, expected_kind) in [
            (
                "thinking_signature_repeat",
                KiroCallFailureKind::ThinkingSignatureInvalid,
            ),
            (
                "thinking_signature_nested_repeat",
                KiroCallFailureKind::ThinkingSignatureInvalid,
            ),
            (
                "thinking_signature_read_failure",
                KiroCallFailureKind::ThinkingSignatureRetryFailed,
            ),
        ] {
            for is_stream in [false, true] {
                let (provider, manager) = fake_thinking_signature_provider(&server.base_url, 20);
                for round in 1..=5 {
                    let request_body = thinking_signature_request_body(scenario, round);
                    let retry_body = strip_reasoning_content_for_provider_test(&request_body);
                    let builder_calls = Arc::new(AtomicUsize::new(0));
                    let builder_counter = builder_calls.clone();
                    let budget = Arc::new(InferenceAttemptBudget::new(4));
                    let hits_before = server.state.scenario_hits(scenario);
                    let error = call_thinking_signature_retry(
                        &provider,
                        &request_body,
                        is_stream,
                        budget.clone(),
                        false,
                        None,
                        move || {
                            builder_counter.fetch_add(1, Ordering::SeqCst);
                            Ok(retry_body)
                        },
                    )
                    .await
                    .expect_err("controlled second response must terminate with typed failure");

                    assert_eq!(server.state.scenario_hits(scenario) - hits_before, 2);
                    assert_eq!(builder_calls.load(Ordering::SeqCst), 1);
                    let attempts = KiroProvider::attempts_from_error(&error);
                    assert_eq!(attempts.len(), 2);
                    assert_eq!(attempts[0].credential_id, attempts[1].credential_id);
                    assert_eq!(
                        KiroProvider::call_failure_kind_from_error(&error),
                        Some(expected_kind)
                    );
                    let snapshot = budget.snapshot();
                    assert_eq!(snapshot.consumed, 2);
                    assert_eq!(snapshot.local_attempts, 2);
                    assert_eq!(snapshot.external_attempts, 0);
                    assert!(error.to_string().len() < 1024);
                    assert!(!error.to_string().contains("PRIVATE_SIGNATURE_RESPONSE"));
                    assert_signature_retry_did_not_cool_down(
                        &manager,
                        &format!("{scenario} stream={is_stream} round {round}"),
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn thinking_signature_retry_retryable_second_response_is_transient_for_five_rounds() {
        let server = FakeBadRequestServer::start().await;
        for (scenario, expected_class, expected_status, cooldown_reason) in [
            (
                "thinking_signature_unexpected_second",
                "server_error",
                500,
                "api_server_error",
            ),
            (
                "thinking_signature_rate_limited_second",
                "rate_limit",
                429,
                "api_rate_limit",
            ),
        ] {
            for is_stream in [false, true] {
                let (provider, manager) = fake_thinking_signature_provider(&server.base_url, 20);
                for round in 1..=5 {
                    let request_body = thinking_signature_request_body(scenario, round);
                    let retry_body = strip_reasoning_content_for_provider_test(&request_body);
                    let builder_calls = Arc::new(AtomicUsize::new(0));
                    let builder_counter = builder_calls.clone();
                    let budget = Arc::new(InferenceAttemptBudget::new(4));
                    let hits_before = server.state.scenario_hits(scenario);
                    let error = call_thinking_signature_retry(
                        &provider,
                        &request_body,
                        is_stream,
                        budget.clone(),
                        false,
                        None,
                        move || {
                            builder_counter.fetch_add(1, Ordering::SeqCst);
                            Ok(retry_body)
                        },
                    )
                    .await
                    .expect_err("retryable second response must surface as transient failure");

                    assert_eq!(server.state.scenario_hits(scenario) - hits_before, 2);
                    assert_eq!(builder_calls.load(Ordering::SeqCst), 1);
                    assert!(
                        error
                            .to_string()
                            .contains(&format!("class={expected_class}")),
                        "{scenario} stream={is_stream} round={round}: {}",
                        error
                    );
                    assert!(
                        !error
                            .to_string()
                            .contains("thinking_signature_retry_failed"),
                        "{scenario} stream={is_stream} round={round}: {}",
                        error
                    );
                    assert!(
                        !error.to_string().contains("PRIVATE_SIGNATURE_RESPONSE"),
                        "{scenario} stream={is_stream} round={round}: {}",
                        error
                    );
                    assert_eq!(KiroProvider::call_failure_kind_from_error(&error), None);
                    let attempts = KiroProvider::attempts_from_error(&error);
                    assert_eq!(attempts.len(), 2);
                    assert_eq!(
                        attempts[0].error_type.as_deref(),
                        Some("thinking_signature_invalid")
                    );
                    assert_eq!(attempts[1].status, Some(expected_status));
                    assert_eq!(attempts[1].error_type.as_deref(), Some(expected_class));
                    assert_eq!(attempts[0].credential_id, attempts[1].credential_id);
                    let snapshot = budget.snapshot();
                    assert_eq!(snapshot.consumed, 2);
                    assert_eq!(snapshot.local_attempts, 2);
                    assert_eq!(snapshot.external_attempts, 0);
                    let manager_snapshot = manager.snapshot();
                    assert!(
                        manager_snapshot
                            .entries
                            .iter()
                            .any(|entry| entry.cooled_down),
                        "{scenario} stream={is_stream} round={round}: expected transient cooldown"
                    );
                    let cooldown_reasons = manager_snapshot
                        .entries
                        .iter()
                        .flat_map(|entry| entry.cooldowns.iter())
                        .filter_map(|cooldown| cooldown.reason.as_deref())
                        .collect::<Vec<_>>();
                    assert!(
                        cooldown_reasons
                            .iter()
                            .any(|reason| reason.contains(cooldown_reason)),
                        "{scenario} stream={is_stream} round={round}: {:?}",
                        cooldown_reasons
                    );
                }
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn thinking_signature_retry_budget_send_limit_and_builder_failure_are_lazy_five_rounds() {
        let captured = CapturedProviderLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(captured.clone())
            .finish();
        let dispatch = tracing::Dispatch::new(subscriber);
        let server = FakeBadRequestServer::start().await;
        for is_stream in [false, true] {
            for round in 1..=5 {
                for (case, budget_limit, max_sends, builder_should_run) in [
                    ("budget", 1, None, false),
                    ("send-limit", 4, Some(1), false),
                    ("builder-failure", 4, None, true),
                ] {
                    let scenario = "thinking_signature_repeat";
                    let (provider, manager) =
                        fake_thinking_signature_provider(&server.base_url, 20);
                    let request_body = thinking_signature_request_body(
                        scenario,
                        round * 10 + usize::from(is_stream),
                    );
                    let builder_calls = Arc::new(AtomicUsize::new(0));
                    let builder_counter = builder_calls.clone();
                    let budget = Arc::new(InferenceAttemptBudget::new(budget_limit));
                    let hits_before = server.state.scenario_hits(scenario);
                    let private_builder_marker =
                        format!("PRIVATE_BUILDER_FAILURE_{case}_{is_stream}_{round}");
                    let marker_for_builder = private_builder_marker.clone();
                    let call = call_thinking_signature_retry(
                        &provider,
                        &request_body,
                        is_stream,
                        budget.clone(),
                        false,
                        max_sends,
                        move || {
                            builder_counter.fetch_add(1, Ordering::SeqCst);
                            Err(anyhow::anyhow!(marker_for_builder))
                        },
                    )
                    .with_subscriber(dispatch.clone());
                    let error = call
                        .await
                        .expect_err("budget/send-limit/builder failure must terminate locally");

                    assert_eq!(server.state.scenario_hits(scenario) - hits_before, 1);
                    assert_eq!(
                        builder_calls.load(Ordering::SeqCst),
                        usize::from(builder_should_run),
                        "{case} stream={is_stream} round {round}"
                    );
                    assert_eq!(budget.snapshot().consumed, 1);
                    assert_eq!(
                        KiroProvider::call_failure_kind_from_error(&error),
                        Some(KiroCallFailureKind::ThinkingSignatureRetryFailed)
                    );
                    assert_eq!(KiroProvider::attempts_from_error(&error).len(), 1);
                    assert!(!error.to_string().contains(&private_builder_marker));
                    assert_signature_retry_did_not_cool_down(
                        &manager,
                        &format!("{case} stream={is_stream} round {round}"),
                    );
                }
            }
        }
        let logs = captured.snapshot();
        assert!(!logs.contains("PRIVATE_BUILDER_FAILURE"));
    }

    #[tokio::test]
    async fn thinking_signature_retry_transport_failure_is_typed_and_same_credential_five_rounds() {
        for is_stream in [false, true] {
            for round in 1..=5 {
                let server = FakeSignatureTransportServer::start().await;
                let (provider, manager) = fake_thinking_signature_provider(&server.base_url, 20);
                let request_body =
                    thinking_signature_request_body("thinking_signature_transport", round);
                let retry_body = strip_reasoning_content_for_provider_test(&request_body);
                let builder_calls = Arc::new(AtomicUsize::new(0));
                let builder_counter = builder_calls.clone();
                let budget = Arc::new(InferenceAttemptBudget::new(4));
                let error = call_thinking_signature_retry(
                    &provider,
                    &request_body,
                    is_stream,
                    budget.clone(),
                    false,
                    None,
                    move || {
                        builder_counter.fetch_add(1, Ordering::SeqCst);
                        Ok(retry_body)
                    },
                )
                .await
                .expect_err("second transport must close without response headers");

                assert_eq!(builder_calls.load(Ordering::SeqCst), 1);
                assert_eq!(
                    KiroProvider::call_failure_kind_from_error(&error),
                    Some(KiroCallFailureKind::ThinkingSignatureRetryFailed)
                );
                let attempts = KiroProvider::attempts_from_error(&error);
                assert_eq!(attempts.len(), 2);
                assert_eq!(attempts[0].credential_id, attempts[1].credential_id);
                let snapshot = budget.snapshot();
                assert_eq!(snapshot.consumed, 2);
                assert_eq!(snapshot.local_attempts, 2);
                assert_eq!(snapshot.external_attempts, 0);
                assert_signature_retry_did_not_cool_down(
                    &manager,
                    &format!("transport stream={is_stream} round {round}"),
                );
            }
        }
    }

    #[tokio::test]
    async fn bad_request_retry_matrix_bounds_real_provider_http_hits() {
        let server = FakeBadRequestServer::start().await;
        let cases = [
            (
                "model_unavailable",
                "model_unavailable_bad_request",
                Some("model_unavailable_retry_next"),
            ),
            ("invalid_model", "model_invalid_bad_request", None),
            ("image_empty", "image_invalid_bad_request", None),
            (
                "image_format_unsupported",
                "image_invalid_bad_request",
                None,
            ),
            (
                "generic_body_invalid",
                "request_body_invalid_bad_request",
                None,
            ),
            ("malformed", "malformed_request", None),
            ("invalid_tool", "tool_use_format_bad_request", None),
            ("invalid_tool_schema", "tool_use_format_bad_request", None),
            (
                "invalid_tool_prompt_retry_disabled",
                "tool_use_format_bad_request",
                None,
            ),
            ("non_model_endpoint_unavailable", "bad_request", None),
        ];

        for pool_size in [1, 20, 60] {
            for round in 0..5 {
                for (scenario, expected_reason, retry_action) in cases {
                    let prompt_logic_retry_enabled =
                        scenario != "invalid_tool_prompt_retry_disabled";
                    let (hits, attempts) = call_fake_bad_request_provider(
                        &server,
                        scenario,
                        pool_size,
                        round,
                        prompt_logic_retry_enabled,
                    )
                    .await;
                    let expected_hits = if retry_action.is_some() && pool_size > 1 {
                        4
                    } else {
                        1
                    };
                    assert_eq!(
                        hits, expected_hits,
                        "unexpected hit count for {scenario}, pool {pool_size}, round {round}"
                    );
                    assert_eq!(
                        attempts.len(),
                        expected_hits,
                        "attempt ledger mismatch for {scenario}, pool {pool_size}, round {round}"
                    );
                    assert_eq!(
                        attempts
                            .last()
                            .and_then(|attempt| attempt.error_type.as_deref()),
                        Some(expected_reason),
                        "final reason mismatch for {scenario}, pool {pool_size}, round {round}"
                    );
                    assert_eq!(
                        attempts.last().map(|attempt| attempt.action.as_str()),
                        Some("fail"),
                        "final action mismatch for {scenario}, pool {pool_size}, round {round}"
                    );
                    let unique_credentials = attempts
                        .iter()
                        .map(|attempt| attempt.credential_id)
                        .collect::<HashSet<_>>();
                    assert_eq!(
                        unique_credentials.len(),
                        expected_hits,
                        "credential was reused for {scenario}, pool {pool_size}, round {round}"
                    );
                    if let Some(retry_action) = retry_action {
                        for attempt in attempts.iter().take(expected_hits.saturating_sub(1)) {
                            assert_eq!(
                                attempt.action, retry_action,
                                "retry action mismatch for {scenario}, pool {pool_size}, round {round}"
                            );
                            assert_eq!(attempt.error_type.as_deref(), Some(expected_reason));
                        }
                    } else {
                        assert_eq!(
                            hits, 1,
                            "deterministic 400 retried for {scenario}, pool {pool_size}, round {round}"
                        );
                    }
                }
            }
        }

        assert_eq!(
            server.state.total_hits.load(Ordering::Relaxed),
            180,
            "the full 1/20/60 x five-round matrix should issue exactly 180 inference requests"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn provider_status_and_non_eventstream_matrix_is_private_typed_and_bounded() {
        let captured = CapturedProviderLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(captured.clone())
            .finish();
        let dispatch = tracing::Dispatch::new(subscriber);
        let server = FakeBadRequestServer::start().await;
        let cases = [
            (
                "provider_status_400",
                "invalid_request",
                Some(400),
                false,
                None,
            ),
            (
                "provider_status_401",
                "auth_error",
                Some(401),
                true,
                Some("api_auth_error"),
            ),
            (
                "provider_status_403",
                "auth_error",
                Some(403),
                true,
                Some("api_auth_error"),
            ),
            (
                "provider_status_408",
                "upstream_timeout",
                Some(408),
                true,
                Some("api_timeout"),
            ),
            (
                "provider_status_429",
                "rate_limit",
                Some(429),
                true,
                Some("api_rate_limit"),
            ),
            (
                "provider_status_500",
                "server_error",
                Some(500),
                true,
                Some("api_server_error"),
            ),
            (
                "provider_status_503",
                "server_error",
                Some(503),
                true,
                Some("api_server_error"),
            ),
            (
                "provider_200_non_eventstream",
                "protocol_error",
                Some(200),
                true,
                Some("api_protocol_error"),
            ),
        ];
        let mut all_markers = Vec::new();

        for is_stream in [false, true] {
            for (scenario, expected_class, expected_status, retryable, cooldown_reason) in cases {
                let hits_before = server.state.scenario_hits(scenario);
                let specifications = [1usize, 20, 60]
                    .into_iter()
                    .flat_map(|pool_size| (1..=5).map(move |round| (pool_size, round)))
                    .collect::<Vec<_>>();
                let outcomes =
                    futures::stream::iter(specifications.iter().map(|(pool_size, round)| {
                        call_fake_provider_failure(
                            &server, scenario, *pool_size, *round, is_stream, 0,
                        )
                        .with_subscriber(dispatch.clone())
                    }))
                    .buffered(PROVIDER_FAILURE_MATRIX_MAX_IN_FLIGHT)
                    .collect::<Vec<_>>()
                    .await;
                let mut expected_total_sends = 0usize;
                for ((pool_size, round), outcome) in specifications.iter().zip(outcomes) {
                    let expected_sends = if retryable && *pool_size > 1 { 4 } else { 1 };
                    expected_total_sends += expected_sends;
                    assert_eq!(
                        outcome.consumed_sends, expected_sends,
                        "{scenario} stream={is_stream} pool={pool_size} round={round}"
                    );
                    assert_eq!(
                        outcome.attempts.len(),
                        expected_sends,
                        "{scenario} stream={is_stream} pool={pool_size} round={round}: every real send must have one ledger entry"
                    );
                    assert!(
                        outcome
                            .attempts
                            .iter()
                            .all(|attempt| attempt.status == expected_status),
                        "{scenario} stream={is_stream} pool={pool_size} round={round}: {:?}",
                        outcome.attempts
                    );
                    assert!(
                        outcome
                            .error_text
                            .contains(&format!("class={expected_class}")),
                        "{scenario} stream={is_stream} pool={pool_size} round={round}: {}",
                        outcome.error_text
                    );
                    assert!(outcome.error_text.len() < 1024);
                    let serialized_attempts =
                        serde_json::to_string(&outcome.attempts).expect("serialize attempts");
                    for surface in [
                        outcome.error_text.as_str(),
                        serialized_attempts.as_str(),
                        outcome.scheduler_snapshot.as_str(),
                    ] {
                        assert!(
                            !surface.contains(&outcome.marker),
                            "{scenario} stream={is_stream} pool={pool_size} round={round}: private marker escaped: {surface}"
                        );
                    }
                    if let Some(cooldown_reason) = cooldown_reason {
                        assert!(
                            outcome.cooled_down.iter().any(|cooled| *cooled),
                            "{scenario} stream={is_stream} pool={pool_size} round={round}: expected cooldown"
                        );
                        assert!(
                            outcome
                                .cooldown_reasons
                                .iter()
                                .any(|reason| reason.contains(cooldown_reason)),
                            "{scenario} stream={is_stream} pool={pool_size} round={round}: {:?}",
                            outcome.cooldown_reasons
                        );
                    } else {
                        assert!(
                            outcome.cooled_down.iter().all(|cooled| !*cooled),
                            "{scenario} stream={is_stream} pool={pool_size} round={round}: invalid/unknown failures must not cool credentials"
                        );
                        assert!(outcome.cooldown_reasons.is_empty());
                        assert!(outcome.cooldown_kinds.iter().all(Option::is_none));
                    }
                    all_markers.push(outcome.marker);
                }
                assert_eq!(
                    server.state.scenario_hits(scenario) - hits_before,
                    expected_total_sends,
                    "{scenario} stream={is_stream}: actual HTTP hits must equal shared inference sends"
                );
            }
        }

        let logs = captured.snapshot();
        for marker in all_markers {
            assert!(
                !logs.contains(&marker),
                "provider logs captured private upstream response marker {marker}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn provider_transport_and_body_fault_matrix_is_private_typed_and_bounded() {
        let captured = CapturedProviderLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(captured.clone())
            .finish();
        let dispatch = tracing::Dispatch::new(subscriber);
        let server = FakeBadRequestServer::start().await;
        let cases = [
            (
                "provider_header_timeout",
                "upstream_timeout",
                None,
                "api_timeout",
                1,
            ),
            (
                "provider_error_content_length_over_limit",
                "response_too_large",
                Some(500),
                "api_response_too_large",
                10,
            ),
            (
                "provider_error_chunked_over_limit",
                "response_too_large",
                Some(500),
                "api_response_too_large",
                10,
            ),
            (
                "provider_body_timeout",
                "upstream_timeout",
                Some(500),
                "api_timeout",
                5,
            ),
            (
                "provider_body_disconnect",
                "body_read_error",
                Some(500),
                "api_body_read_error",
                10,
            ),
            (
                "provider_malformed_utf8",
                "protocol_error",
                Some(500),
                "api_protocol_error",
                10,
            ),
        ];
        let mut all_markers = Vec::new();

        for is_stream in [false, true] {
            for (
                scenario,
                expected_class,
                expected_status,
                cooldown_reason,
                response_timeout_secs,
            ) in cases
            {
                let hits_before = server.state.scenario_hits(scenario);
                let specifications = [1usize, 20, 60]
                    .into_iter()
                    .flat_map(|pool_size| (1..=5).map(move |round| (pool_size, round)))
                    .collect::<Vec<_>>();
                let outcomes =
                    futures::stream::iter(specifications.iter().map(|(pool_size, round)| {
                        call_fake_provider_failure(
                            &server,
                            scenario,
                            *pool_size,
                            *round,
                            is_stream,
                            response_timeout_secs,
                        )
                        .with_subscriber(dispatch.clone())
                    }))
                    .buffered(PROVIDER_FAILURE_MATRIX_MAX_IN_FLIGHT)
                    .collect::<Vec<_>>()
                    .await;
                let mut expected_total_sends = 0usize;
                let mut observed_body_stage_status = false;
                for ((pool_size, round), outcome) in specifications.iter().zip(outcomes) {
                    let expected_sends = if *pool_size > 1 { 4 } else { 1 };
                    expected_total_sends += expected_sends;
                    assert_eq!(
                        outcome.consumed_sends, expected_sends,
                        "{scenario} stream={is_stream} pool={pool_size} round={round}"
                    );
                    assert_eq!(outcome.attempts.len(), expected_sends);
                    if scenario == "provider_body_timeout" {
                        observed_body_stage_status |= outcome
                            .attempts
                            .iter()
                            .any(|attempt| attempt.status == expected_status);
                        assert!(
                            outcome.attempts.iter().all(|attempt| {
                                attempt.status == expected_status || attempt.status.is_none()
                            }),
                            "{scenario} stream={is_stream} pool={pool_size} round={round}: {:?}",
                            outcome.attempts
                        );
                    } else {
                        assert!(
                            outcome
                                .attempts
                                .iter()
                                .all(|attempt| attempt.status == expected_status),
                            "{scenario} stream={is_stream} pool={pool_size} round={round}: {:?}",
                            outcome.attempts
                        );
                    }
                    assert!(
                        outcome
                            .error_text
                            .contains(&format!("class={expected_class}")),
                        "{scenario} stream={is_stream} pool={pool_size} round={round}: {}",
                        outcome.error_text
                    );
                    let serialized_attempts =
                        serde_json::to_string(&outcome.attempts).expect("serialize attempts");
                    for surface in [
                        outcome.error_text.as_str(),
                        serialized_attempts.as_str(),
                        outcome.scheduler_snapshot.as_str(),
                    ] {
                        assert!(
                            !surface.contains(&outcome.marker),
                            "{scenario} stream={is_stream} pool={pool_size} round={round}: private marker escaped: {surface}"
                        );
                    }
                    assert!(outcome.cooled_down.iter().any(|cooled| *cooled));
                    assert!(
                        outcome
                            .cooldown_reasons
                            .iter()
                            .any(|reason| reason.contains(cooldown_reason)),
                        "{scenario} stream={is_stream} pool={pool_size} round={round}: {:?}",
                        outcome.cooldown_reasons
                    );
                    all_markers.push(outcome.marker);
                }
                if scenario == "provider_body_timeout" {
                    assert!(
                        observed_body_stage_status,
                        "stream={is_stream}: at least one timeout must occur after response headers"
                    );
                }
                let received_hits = server.state.scenario_hits(scenario) - hits_before;
                if matches!(
                    scenario,
                    "provider_header_timeout" | "provider_body_timeout"
                ) {
                    assert!(
                        (15..=expected_total_sends).contains(&received_hits),
                        "{scenario} stream={is_stream}: server receipts cannot exceed bounded send attempts"
                    );
                } else {
                    assert_eq!(
                        received_hits, expected_total_sends,
                        "{scenario} stream={is_stream}: body-stage faults must account for every real HTTP send"
                    );
                }
            }
        }

        let logs = captured.snapshot();
        for marker in all_markers {
            assert!(
                !logs.contains(&marker),
                "provider logs captured private upstream fault marker {marker}"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn provider_recovers_after_typed_failure_and_never_ledgers_headers_as_success() {
        let server = FakeBadRequestServer::start().await;
        for is_stream in [false, true] {
            for round in 1..=5 {
                let mut config = Config::default();
                config.kiro_upstream_base_url = Some(server.base_url.clone());
                config.kiro_upstream_response_timeout_secs = 2;
                config.credential_retry_max_attempts = 100;
                config.credential_server_error_cooldown_secs = 60;
                config.credential_cooldown_jitter_percent = 0;
                let manager = Arc::new(
                    MultiTokenManager::new(
                        config,
                        fake_bad_request_credentials(1),
                        None,
                        None,
                        false,
                    )
                    .expect("provider recovery token manager"),
                );
                let mut endpoints: HashMap<String, Arc<dyn KiroEndpoint>> = HashMap::new();
                endpoints.insert("ide".to_string(), Arc::new(IdeEndpoint));
                let provider =
                    KiroProvider::with_proxy(manager.clone(), None, endpoints, "ide".to_string());
                let failed_body = serde_json::json!({
                    "testScenario": "provider_status_500",
                    "secretMarker": format!("RECOVERY_PRIVATE_MARKER_{is_stream}_{round}"),
                    "conversationState": {
                        "conversationId": format!("provider-failure-recovery-{is_stream}-{round}"),
                        "currentMessage": {"userInputMessage": {
                            "content": "test",
                            "modelId": "claude-sonnet-4"
                        }}
                    }
                })
                .to_string();
                let failed_budget = Arc::new(InferenceAttemptBudget::new(4));
                let failed = if is_stream {
                    provider
                        .call_api_stream_with_request_id_and_attempt_budget(
                            &failed_body,
                            Some("req-provider-recovery-failure"),
                            AcquireMode::WaitForCapacity,
                            1,
                            Some("claude-sonnet-4"),
                            failed_budget.clone(),
                            false,
                        )
                        .await
                        .map(|_| ())
                } else {
                    provider
                        .call_api_with_context_with_request_id_and_attempt_budget(
                            &failed_body,
                            Some("req-provider-recovery-failure"),
                            AcquireMode::WaitForCapacity,
                            1,
                            Some("claude-sonnet-4"),
                            failed_budget.clone(),
                            false,
                        )
                        .await
                        .map(|_| ())
                };
                assert!(failed.is_err(), "stream={is_stream} round={round}");
                assert_eq!(failed_budget.snapshot().consumed, 1);

                let recovery_body = serde_json::json!({
                    "testScenario": "provider_eventstream_headers",
                    "conversationState": {
                        "conversationId": format!("provider-success-recovery-{is_stream}-{round}"),
                        "currentMessage": {"userInputMessage": {
                            "content": "test",
                            "modelId": "claude-opus-4"
                        }}
                    }
                })
                .to_string();
                let recovery_budget = Arc::new(InferenceAttemptBudget::new(4));
                if is_stream {
                    let response = provider
                        .call_api_stream_with_request_id_and_attempt_budget(
                            &recovery_body,
                            Some("req-provider-recovery-success"),
                            AcquireMode::WaitForCapacity,
                            1,
                            Some("claude-opus-4"),
                            recovery_budget.clone(),
                            false,
                        )
                        .await
                        .unwrap_or_else(|error| {
                            panic!("stream={is_stream} round={round}: {error}")
                        });
                    let (response, completion) = response.into_parts();
                    assert_eq!(response.status(), reqwest::StatusCode::OK);
                    assert_eq!(completion.attempts().len(), 1);
                    assert_eq!(completion.attempts()[0].action, "response_headers_received");
                    let pending = manager.snapshot();
                    assert_eq!(pending.entries[0].success_count, 0);
                    assert_eq!(pending.entries[0].in_flight_requests, 1);
                    completion.report_success();
                } else {
                    let response = provider
                        .call_api_with_context_with_request_id_and_attempt_budget(
                            &recovery_body,
                            Some("req-provider-recovery-success"),
                            AcquireMode::WaitForCapacity,
                            1,
                            Some("claude-opus-4"),
                            recovery_budget.clone(),
                            false,
                        )
                        .await
                        .unwrap_or_else(|error| {
                            panic!("stream={is_stream} round={round}: {error}")
                        });
                    assert_eq!(response.attempts().len(), 1);
                    assert_eq!(response.attempts()[0].action, "response_headers_received");
                    let pending = manager.snapshot();
                    assert_eq!(pending.entries[0].success_count, 0);
                    assert_eq!(pending.entries[0].in_flight_requests, 1);
                    let (response, completion) = response.into_parts();
                    assert_eq!(response.status(), reqwest::StatusCode::OK);
                    completion.report_success();
                }
                assert_eq!(recovery_budget.snapshot().consumed, 1);
                let completed = manager.snapshot();
                assert_eq!(completed.entries[0].success_count, 1);
                assert_eq!(completed.entries[0].in_flight_requests, 0);
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn eventstream_content_type_json_body_remains_for_handler_sniffing_for_five_rounds() {
        let captured = CapturedProviderLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(captured.clone())
            .finish();
        let _subscriber_guard = tracing::subscriber::set_default(subscriber);
        let server = FakeBadRequestServer::start().await;
        let mut markers = Vec::new();

        for is_stream in [false, true] {
            for round in 1..=5 {
                let mut config = Config::default();
                config.kiro_upstream_base_url = Some(server.base_url.clone());
                config.kiro_upstream_response_timeout_secs = 10;
                config.credential_retry_max_attempts = 100;
                let manager = Arc::new(
                    MultiTokenManager::new(
                        config,
                        fake_bad_request_credentials(1),
                        None,
                        None,
                        false,
                    )
                    .expect("eventstream JSON sniff token manager"),
                );
                let mut endpoints: HashMap<String, Arc<dyn KiroEndpoint>> = HashMap::new();
                endpoints.insert("ide".to_string(), Arc::new(IdeEndpoint));
                let provider =
                    KiroProvider::with_proxy(manager.clone(), None, endpoints, "ide".to_string());
                let marker = format!("EVENTSTREAM_JSON_PRIVATE_MARKER_{is_stream}_{round}");
                let request_body = serde_json::json!({
                    "testScenario": "provider_eventstream_json_exception",
                    "secretMarker": marker,
                    "conversationState": {
                        "conversationId": format!("eventstream-json-{is_stream}-{round}"),
                        "currentMessage": {"userInputMessage": {
                            "content": "test",
                            "modelId": "claude-sonnet-4"
                        }}
                    }
                })
                .to_string();
                let budget = Arc::new(InferenceAttemptBudget::new(4));
                if is_stream {
                    let response = provider
                        .call_api_stream_with_request_id_and_attempt_budget(
                            &request_body,
                            Some("req-eventstream-json"),
                            AcquireMode::WaitForCapacity,
                            1,
                            Some("claude-sonnet-4"),
                            budget.clone(),
                            false,
                        )
                        .await
                        .expect("eventstream-labeled JSON reaches handler");
                    let (response, completion) = response.into_parts();
                    assert_eq!(completion.attempts()[0].action, "response_headers_received");
                    let body = crate::http_client::response_bytes_with_limit_and_body_timeout(
                        response,
                        10,
                        64 * 1024,
                    )
                    .await
                    .expect("read eventstream-labeled JSON body");
                    assert!(String::from_utf8_lossy(&body).contains(&marker));
                    assert_eq!(manager.snapshot().entries[0].success_count, 0);
                    drop(completion);
                } else {
                    let response = provider
                        .call_api_with_context_with_request_id_and_attempt_budget(
                            &request_body,
                            Some("req-eventstream-json"),
                            AcquireMode::WaitForCapacity,
                            1,
                            Some("claude-sonnet-4"),
                            budget.clone(),
                            false,
                        )
                        .await
                        .expect("eventstream-labeled JSON reaches handler");
                    assert_eq!(response.attempts()[0].action, "response_headers_received");
                    let (response, completion) = response.into_parts();
                    let body = crate::http_client::response_bytes_with_limit_and_body_timeout(
                        response,
                        10,
                        64 * 1024,
                    )
                    .await
                    .expect("read eventstream-labeled JSON body");
                    assert!(String::from_utf8_lossy(&body).contains(&marker));
                    assert_eq!(manager.snapshot().entries[0].success_count, 0);
                    completion.release();
                }
                assert_eq!(budget.snapshot().consumed, 1);
                assert_eq!(manager.snapshot().entries[0].in_flight_requests, 0);
                markers.push(marker);
            }
        }

        let logs = captured.snapshot();
        for marker in markers {
            assert!(
                !logs.contains(&marker),
                "provider must not log eventstream-labeled JSON body marker {marker}"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn json_content_type_response_headers_remain_for_handler_sniffing_for_five_rounds() {
        let server = FakeBadRequestServer::start().await;

        for is_stream in [false, true] {
            for round in 1..=5 {
                let mut config = Config::default();
                config.kiro_upstream_base_url = Some(server.base_url.clone());
                config.kiro_upstream_response_timeout_secs = 10;
                config.credential_retry_max_attempts = 100;
                let manager = Arc::new(
                    MultiTokenManager::new(
                        config,
                        fake_bad_request_credentials(1),
                        None,
                        None,
                        false,
                    )
                    .expect("JSON content-type sniff token manager"),
                );
                let mut endpoints: HashMap<String, Arc<dyn KiroEndpoint>> = HashMap::new();
                endpoints.insert("ide".to_string(), Arc::new(IdeEndpoint));
                let provider =
                    KiroProvider::with_proxy(manager.clone(), None, endpoints, "ide".to_string());
                let request_body = serde_json::json!({
                    "testScenario": "provider_json_headers",
                    "conversationState": {
                        "conversationId": format!("json-content-type-{is_stream}-{round}"),
                        "currentMessage": {"userInputMessage": {
                            "content": "test",
                            "modelId": "claude-sonnet-4"
                        }}
                    }
                })
                .to_string();
                let budget = Arc::new(InferenceAttemptBudget::new(4));

                if is_stream {
                    let response = provider
                        .call_api_stream_with_request_id_and_attempt_budget(
                            &request_body,
                            Some("req-json-content-type"),
                            AcquireMode::WaitForCapacity,
                            1,
                            Some("claude-sonnet-4"),
                            budget.clone(),
                            false,
                        )
                        .await
                        .expect("JSON content-type reaches handler for stream");
                    let (response, completion) = response.into_parts();
                    assert_eq!(response.status(), reqwest::StatusCode::OK);
                    assert_eq!(completion.attempts()[0].action, "response_headers_received");
                    assert_eq!(manager.snapshot().entries[0].success_count, 0);
                    drop(completion);
                } else {
                    let response = provider
                        .call_api_with_context_with_request_id_and_attempt_budget(
                            &request_body,
                            Some("req-json-content-type"),
                            AcquireMode::WaitForCapacity,
                            1,
                            Some("claude-sonnet-4"),
                            budget.clone(),
                            false,
                        )
                        .await
                        .expect("JSON content-type reaches handler for non-stream");
                    assert_eq!(response.attempts()[0].action, "response_headers_received");
                    let (response, completion) = response.into_parts();
                    assert_eq!(response.status(), reqwest::StatusCode::OK);
                    assert_eq!(manager.snapshot().entries[0].success_count, 0);
                    completion.release();
                }

                assert_eq!(budget.snapshot().consumed, 1);
                assert_eq!(manager.snapshot().entries[0].in_flight_requests, 0);
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn auxiliary_and_manual_provider_errors_never_persist_raw_bodies_for_five_rounds() {
        let captured = CapturedProviderLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(captured.clone())
            .finish();
        let _subscriber_guard = tracing::subscriber::set_default(subscriber);
        let server = FakeBadRequestServer::start().await;
        let mut markers = Vec::new();

        for round in 1..=5 {
            let (provider, manager) =
                fake_profile_provider(&server.base_url, fake_bad_request_credentials(1), None);

            let manual_marker = format!("MANUAL_PROVIDER_PRIVATE_MARKER_{round}");
            let manual_error = provider
                .call_api_with_credential(
                    1,
                    &serde_json::json!({
                        "testScenario": "provider_status_500",
                        "secretMarker": manual_marker
                    })
                    .to_string(),
                )
                .await
                .err()
                .expect("manual credential test upstream failure");
            assert!(manual_error.to_string().contains("class=server_error"));
            assert!(!manual_error.to_string().contains(&manual_marker));
            markers.push(manual_marker);

            let model_marker = format!("MODEL_DISCOVERY_PRIVATE_MARKER_{round}");
            server.state.set_auxiliary_marker(model_marker.clone());
            let model_error = provider
                .list_available_models()
                .await
                .expect_err("model discovery upstream failure");
            assert!(model_error.to_string().contains("class=server_error"));
            assert!(!model_error.to_string().contains(&model_marker));
            markers.push(model_marker);

            let profile_marker = format!("PROFILE_DISCOVERY_PRIVATE_MARKER_{round}");
            server.state.set_auxiliary_marker(profile_marker.clone());
            let (profile_provider, profile_manager) = fake_profile_provider(
                &server.base_url,
                vec![fake_external_idp_credential(
                    1,
                    &format!("forbidden-private-{round}"),
                )],
                None,
            );
            let ctx = profile_manager
                .acquire_context_for_credential(1)
                .await
                .expect("acquire profile privacy credential");
            let profile = profile_provider
                .fetch_enterprise_profile_arn_for_context(
                    &ctx,
                    &profile_provider.runtime_config(),
                    "fake-machine",
                    None,
                )
                .await
                .expect("403 profile discovery is a safe negative result");
            assert!(profile.is_none());
            markers.push(profile_marker);

            let snapshots = format!(
                "{} {}",
                serde_json::to_string(&manager.snapshot()).expect("manual manager snapshot"),
                serde_json::to_string(&profile_manager.snapshot())
                    .expect("profile manager snapshot")
            );
            for marker in markers.iter().rev().take(3) {
                assert!(
                    !snapshots.contains(marker),
                    "scheduler snapshot captured auxiliary marker {marker}"
                );
            }
        }

        let logs = captured.snapshot();
        for marker in markers {
            assert!(
                !logs.contains(&marker),
                "provider logs captured auxiliary response marker {marker}"
            );
        }
    }

    #[test]
    fn list_available_profiles_headers_attach_external_idp_token_type() {
        let credentials = KiroCredentials {
            auth_method: Some("external_idp".to_string()),
            ..Default::default()
        };
        let headers = KiroProvider::list_available_profiles_headers(
            &credentials,
            "token",
            &Config::default(),
            "machine",
            "codewhisperer.us-east-1.amazonaws.com",
        )
        .unwrap();

        assert_eq!(
            headers.get("TokenType").and_then(|v| v.to_str().ok()),
            Some("EXTERNAL_IDP")
        );
        assert_eq!(
            headers.get("Authorization").and_then(|v| v.to_str().ok()),
            Some("Bearer token")
        );
        assert_eq!(
            headers.get("host").and_then(|v| v.to_str().ok()),
            Some("codewhisperer.us-east-1.amazonaws.com")
        );
        let expected_x_amz_user_agent = format!(
            "aws-sdk-js/1.0.34 KiroIDE-{}-machine",
            Config::default().kiro_version
        );
        assert_eq!(
            headers
                .get("x-amz-user-agent")
                .and_then(|v| v.to_str().ok()),
            Some(expected_x_amz_user_agent.as_str())
        );
    }

    #[test]
    fn list_available_profiles_headers_do_not_attach_token_type_for_social() {
        let credentials = KiroCredentials {
            auth_method: Some("social".to_string()),
            ..Default::default()
        };
        let headers = KiroProvider::list_available_profiles_headers(
            &credentials,
            "token",
            &Config::default(),
            "machine",
            "codewhisperer.us-east-1.amazonaws.com",
        )
        .unwrap();

        assert!(headers.get("TokenType").is_none());
    }

    #[test]
    fn stream_completion_reports_success_once() {
        let mut cred = KiroCredentials::default();
        cred.access_token = Some("token".to_string());
        cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let manager = Arc::new(
            MultiTokenManager::new(Config::default(), vec![cred], None, None, false).unwrap(),
        );
        let completion = KiroStreamCompletion::new(
            manager.clone(),
            1,
            None,
            Some("session".into()),
            Some("claude-sonnet-4.5".into()),
            false,
            false,
            Vec::new(),
            Instant::now(),
        );

        completion.report_success();
        completion.report_success();

        let snapshot = manager.snapshot();
        assert_eq!(snapshot.entries[0].success_count, 1);
    }

    #[test]
    fn stream_completion_soft_failure_does_not_count_success() {
        let mut cred = KiroCredentials::default();
        cred.access_token = Some("token".to_string());
        cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let manager = Arc::new(
            MultiTokenManager::new(Config::default(), vec![cred], None, None, false).unwrap(),
        );
        let completion = KiroStreamCompletion::new(
            manager.clone(),
            1,
            None,
            Some("session".into()),
            Some("claude-sonnet-4.5".into()),
            false,
            false,
            Vec::new(),
            Instant::now(),
        );

        completion.report_soft_failure();
        completion.report_success();

        let snapshot = manager.snapshot();
        assert_eq!(snapshot.entries[0].success_count, 0);
    }

    #[test]
    fn stream_completion_upstream_failure_cools_down_credential() {
        let mut cred = KiroCredentials::default();
        cred.access_token = Some("token".to_string());
        cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let manager = Arc::new(
            MultiTokenManager::new(Config::default(), vec![cred], None, None, false).unwrap(),
        );
        let completion = KiroStreamCompletion::new(
            manager.clone(),
            1,
            None,
            Some("session".into()),
            Some("claude-sonnet-4.5".into()),
            false,
            false,
            Vec::new(),
            Instant::now(),
        );

        completion.report_upstream_stream_failure("upstream stream read error");
        completion.report_success();

        let snapshot = manager.snapshot();
        assert_eq!(snapshot.entries[0].success_count, 0);
        assert!(snapshot.entries[0].cooled_down);
        assert_eq!(snapshot.entries[0].failure_count, 0);
    }

    #[test]
    fn api_completion_drop_releases_in_flight_without_counting_success() {
        let mut cred = KiroCredentials::default();
        cred.access_token = Some("token".to_string());
        cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let manager = Arc::new(
            MultiTokenManager::new(Config::default(), vec![cred], None, None, false).unwrap(),
        );
        let lease = manager.acquire_in_flight_lease_for_test(1);

        {
            let _completion = super::KiroApiCompletion::new(
                manager.clone(),
                1,
                lease,
                Some("session".into()),
                Some("claude-sonnet-4.5".into()),
                false,
                false,
                Vec::new(),
                Instant::now(),
            );
        }

        let snapshot = manager.snapshot();
        assert_eq!(snapshot.entries[0].in_flight_requests, 0);
        assert_eq!(snapshot.entries[0].success_count, 0);
    }

    #[test]
    fn api_completion_report_success_once() {
        let mut cred = KiroCredentials::default();
        cred.access_token = Some("token".to_string());
        cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let manager = Arc::new(
            MultiTokenManager::new(Config::default(), vec![cred], None, None, false).unwrap(),
        );
        let lease = manager.acquire_in_flight_lease_for_test(1);

        let completion = super::KiroApiCompletion::new(
            manager.clone(),
            1,
            lease,
            Some("session".into()),
            Some("claude-sonnet-4.5".into()),
            false,
            false,
            Vec::new(),
            Instant::now(),
        );
        completion.report_success();
        completion.report_success();
        drop(completion);

        let snapshot = manager.snapshot();
        assert_eq!(snapshot.entries[0].in_flight_requests, 0);
        assert_eq!(snapshot.entries[0].success_count, 1);
    }

    #[test]
    fn mcp_completion_holds_lease_until_validated_success_for_five_rounds() {
        for _ in 0..5 {
            let mut credential = KiroCredentials::default();
            credential.access_token = Some("mcp-success-token".to_string());
            credential.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
            let manager = Arc::new(
                MultiTokenManager::new(Config::default(), vec![credential], None, None, false)
                    .unwrap(),
            );
            let completion = McpCallCompletion::new(
                manager.clone(),
                1,
                "mcp-account".to_string(),
                manager.acquire_in_flight_lease_for_test(1),
                Vec::new(),
                0,
                reqwest::StatusCode::OK,
                Instant::now(),
            );

            let pending = manager.snapshot();
            assert_eq!(pending.entries[0].in_flight_requests, 1);
            assert_eq!(pending.entries[0].success_count, 0);

            completion.report_success();
            completion.report_success();

            let attribution = completion.attribution();
            assert_eq!(attribution.credential_id, Some(1));
            assert_eq!(attribution.credential_label.as_deref(), Some("mcp-account"));
            assert_eq!(attribution.attempts.len(), 1);
            assert_eq!(attribution.attempts[0].credential_id, 1);
            assert_eq!(attribution.attempts[0].status, Some(200));
            assert_eq!(attribution.attempts[0].action, "success");

            let completed = manager.snapshot();
            assert_eq!(completed.entries[0].in_flight_requests, 0);
            assert_eq!(completed.entries[0].success_count, 1);
        }
    }

    #[test]
    fn mcp_completion_failure_types_release_without_poisoning_core_credentials_for_five_rounds() {
        for kind in [
            McpCallFailureKind::Scheduler,
            McpCallFailureKind::InvalidRequest,
            McpCallFailureKind::AttemptLimit,
            McpCallFailureKind::AuxiliaryAttemptLimit,
            McpCallFailureKind::AuxiliaryConcurrency,
            McpCallFailureKind::RateLimit,
            McpCallFailureKind::Timeout,
            McpCallFailureKind::ResponseTooLarge,
            McpCallFailureKind::BodyRead,
            McpCallFailureKind::Protocol,
            McpCallFailureKind::Upstream,
        ] {
            for _ in 0..5 {
                let mut config = Config::default();
                config.credential_transient_cooldown_secs = 60;
                let mut credential = KiroCredentials::default();
                credential.access_token = Some("mcp-failure-token".to_string());
                credential.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
                let manager = Arc::new(
                    MultiTokenManager::new(config, vec![credential], None, None, false).unwrap(),
                );
                let completion = McpCallCompletion::new(
                    manager.clone(),
                    1,
                    "mcp-account".to_string(),
                    manager.acquire_in_flight_lease_for_test(1),
                    Vec::new(),
                    0,
                    reqwest::StatusCode::OK,
                    Instant::now(),
                );

                completion.report_failure(kind);
                completion.report_success();

                let attribution = completion.attribution();
                assert_eq!(attribution.attempts.len(), 1);
                assert_eq!(attribution.attempts[0].action, "fail");
                assert_eq!(
                    attribution.attempts[0].error_type.as_deref(),
                    Some(kind.as_error_type())
                );
                assert_eq!(
                    attribution.attempts[0].error_message.as_deref(),
                    Some(kind.scheduler_reason())
                );
                assert!(!format!("{:?}", attribution.attempts).contains("private-result-marker"));

                let snapshot = manager.snapshot();
                assert_eq!(snapshot.entries[0].in_flight_requests, 0);
                assert_eq!(snapshot.entries[0].success_count, 0);
                assert!(!snapshot.entries[0].cooled_down);
                assert_eq!(snapshot.entries[0].last_error_kind.as_deref(), None);
                assert_eq!(snapshot.entries[0].last_error_reason.as_deref(), None);
            }
        }
    }

    #[test]
    fn mcp_completion_drop_releases_pending_lease_without_false_success_for_five_rounds() {
        for _ in 0..5 {
            let mut credential = KiroCredentials::default();
            credential.access_token = Some("mcp-cancelled-token".to_string());
            credential.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
            let manager = Arc::new(
                MultiTokenManager::new(Config::default(), vec![credential], None, None, false)
                    .unwrap(),
            );
            let completion = McpCallCompletion::new(
                manager.clone(),
                1,
                "mcp-account".to_string(),
                manager.acquire_in_flight_lease_for_test(1),
                Vec::new(),
                0,
                reqwest::StatusCode::OK,
                Instant::now(),
            );

            assert_eq!(manager.snapshot().entries[0].in_flight_requests, 1);
            drop(completion);

            let snapshot = manager.snapshot();
            assert_eq!(snapshot.entries[0].in_flight_requests, 0);
            assert_eq!(snapshot.entries[0].success_count, 0);
            assert!(!snapshot.entries[0].cooled_down);
        }
    }

    #[test]
    fn accepts_only_binary_event_stream_content_types() {
        assert!(KiroProvider::is_event_stream_content_type(
            "application/vnd.amazon.eventstream"
        ));
        assert!(KiroProvider::is_event_stream_content_type(
            "application/octet-stream; charset=utf-8"
        ));
        assert!(!KiroProvider::is_event_stream_content_type(
            "application/json"
        ));
        assert!(!KiroProvider::is_event_stream_content_type("text/plain"));
    }

    #[test]
    fn auto_retry_attempts_are_bounded_independently_of_credential_pool_size() {
        let config = Config::default();

        assert_eq!(KiroProvider::test_max_retry_attempts(1, &config), 3);
        assert_eq!(KiroProvider::test_max_retry_attempts(3, &config), 3);
        assert_eq!(KiroProvider::test_max_retry_attempts(25, &config), 3);
        assert_eq!(KiroProvider::test_max_retry_attempts(10_000, &config), 3);
    }

    #[test]
    fn configured_retry_attempts_override_auto_pool_size() {
        let mut config = Config::default();
        config.credential_retry_max_attempts = 12;

        assert_eq!(KiroProvider::test_max_retry_attempts(1, &config), 12);
        assert_eq!(KiroProvider::test_max_retry_attempts(25, &config), 12);
        assert_eq!(KiroProvider::test_max_retry_attempts(10_000, &config), 12);
    }

    #[tokio::test]
    async fn mcp_local_acquire_failure_stops_retry_loop() {
        let mut config = Config::default();
        config.credential_retry_max_attempts = 100_000;

        let mut disabled = KiroCredentials::default();
        disabled.disabled = true;
        let manager =
            Arc::new(MultiTokenManager::new(config, vec![disabled], None, None, false).unwrap());
        let mut endpoints: HashMap<String, Arc<dyn KiroEndpoint>> = HashMap::new();
        endpoints.insert("ide".to_string(), Arc::new(IdeEndpoint));
        let provider = KiroProvider::with_proxy(manager, None, endpoints, "ide".to_string());

        let err = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            provider.call_mcp("{}", Arc::new(InferenceAttemptBudget::new(4))),
        )
        .await
        .expect("本地无可用凭据时 MCP 不应跑满 retry 上限")
        .err()
        .unwrap();

        assert!(
            err.to_string().contains("所有账号均已禁用"),
            "错误应直接来自本地调度失败，实际: {}",
            err
        );
        assert_eq!(
            KiroProvider::mcp_failure_kind_from_error(&err),
            Some(McpCallFailureKind::Scheduler)
        );
        let attribution = KiroProvider::mcp_attribution_from_error(&err);
        let selection_failure = attribution
            .selection_failure
            .expect("MCP acquire failure must carry selectionFailure");
        assert_eq!(selection_failure.request_id, "mcp");
        assert_eq!(
            selection_failure.stage,
            crate::kiro::call_trace::SelectionFailureStage::AccountEligibility
        );
        assert_eq!(
            selection_failure.primary_reason,
            crate::kiro::call_trace::AccountRejectReason::Disabled
        );
    }

    #[tokio::test]
    async fn mcp_success_header_keeps_real_lease_until_body_validation_for_five_rounds() {
        let server = FakeBadRequestServer::start().await;
        let mut config = Config::default();
        config.kiro_upstream_base_url = Some(server.base_url.clone());
        config.kiro_upstream_response_timeout_secs = 2;
        config.credential_retry_max_attempts = 1;
        let manager = Arc::new(
            MultiTokenManager::new(config, fake_bad_request_credentials(1), None, None, false)
                .expect("MCP completion token manager"),
        );
        let mut endpoints: HashMap<String, Arc<dyn KiroEndpoint>> = HashMap::new();
        endpoints.insert("ide".to_string(), Arc::new(IdeEndpoint));
        let provider =
            KiroProvider::with_proxy(manager.clone(), None, endpoints, "ide".to_string());

        for round in 1..=5 {
            let response = provider
                .call_mcp(
                    &serde_json::json!({
                        "testScenario": "mcp_success_header",
                        "jsonrpc": "2.0",
                        "id": "x"
                    })
                    .to_string(),
                    Arc::new(InferenceAttemptBudget::new(4)),
                )
                .await
                .unwrap_or_else(|error| panic!("round {round}: {error}"));
            let pending = manager.snapshot();
            assert_eq!(pending.entries[0].in_flight_requests, 1, "round {round}");
            assert_eq!(
                pending.entries[0].success_count,
                (round - 1) as u64,
                "round {round}: response headers alone must not count success"
            );

            let (response, completion) = response.into_parts();
            let body = crate::http_client::response_bytes_with_limit_and_body_timeout(
                response,
                2,
                64 * 1024,
            )
            .await
            .unwrap_or_else(|error| panic!("round {round}: {error}"));
            let envelope: serde_json::Value = serde_json::from_slice(&body)
                .unwrap_or_else(|error| panic!("round {round}: {error}"));
            assert_eq!(envelope["jsonrpc"], "2.0");
            assert_eq!(envelope["id"], "x");
            completion.report_success();

            let completed = manager.snapshot();
            assert_eq!(completed.entries[0].in_flight_requests, 0, "round {round}");
            assert_eq!(completed.entries[0].success_count, round as u64);
            let attribution = completion.attribution();
            assert_eq!(attribution.credential_id, Some(1));
            assert_eq!(attribution.attempts.len(), 1);
            assert_eq!(attribution.attempts[0].action, "success");
        }
    }

    #[tokio::test]
    async fn mcp_real_sends_share_request_budget_for_1_20_60_accounts_over_five_rounds() {
        let server = FakeBadRequestServer::start().await;
        for pool_size in [1, 20, 60] {
            for round in 1..=5 {
                let mut config = Config::default();
                config.kiro_upstream_base_url = Some(server.base_url.clone());
                config.kiro_upstream_response_timeout_secs = 2;
                config.credential_retry_max_attempts = 100;
                let manager = Arc::new(
                    MultiTokenManager::new(
                        config,
                        fake_bad_request_credentials(pool_size),
                        None,
                        None,
                        false,
                    )
                    .expect("fake MCP token manager"),
                );
                let mut endpoints: HashMap<String, Arc<dyn KiroEndpoint>> = HashMap::new();
                endpoints.insert("ide".to_string(), Arc::new(IdeEndpoint));
                let provider =
                    KiroProvider::with_proxy(manager.clone(), None, endpoints, "ide".to_string());
                let budget = Arc::new(InferenceAttemptBudget::new(4));
                let hits_before = server.state.mcp_hits.load(Ordering::Relaxed);

                let error = provider
                    .call_mcp(r#"{"jsonrpc":"2.0","id":"x"}"#, budget.clone())
                    .await
                    .expect_err("controlled MCP upstream always fails");

                let expected_sends = pool_size.min(4);
                assert_eq!(
                    server.state.mcp_hits.load(Ordering::Relaxed) - hits_before,
                    expected_sends,
                    "pool {pool_size} round {round}: sends stay bounded by request budget"
                );
                let snapshot = budget.snapshot();
                assert_eq!(snapshot.consumed, expected_sends as u32);
                assert_eq!(snapshot.local_attempts, 0);
                assert_eq!(snapshot.external_attempts, 0);
                assert_eq!(snapshot.mcp_attempts, expected_sends as u32);
                assert_eq!(
                    snapshot.local_attempts + snapshot.external_attempts + snapshot.mcp_attempts,
                    snapshot.consumed
                );
                let attribution = KiroProvider::mcp_attribution_from_error(&error);
                assert_eq!(
                    attribution.attempts.len(),
                    expected_sends,
                    "pool {pool_size} round {round}: every real send is attributed"
                );
                assert!(attribution.credential_id.is_some());
                assert!(attribution.attempts.iter().all(|attempt| {
                    attempt.attempt <= 4
                        && attempt.error_message.as_deref() == Some("upstream_error")
                }));
                let scheduler_snapshot = manager.snapshot();
                assert!(
                    scheduler_snapshot
                        .entries
                        .iter()
                        .all(|entry| !entry.cooled_down),
                    "pool {pool_size} round {round}: MCP auxiliary failures must not globally cool model credentials"
                );
                assert!(
                    scheduler_snapshot
                        .entries
                        .iter()
                        .all(|entry| entry.last_error_kind.is_none()),
                    "pool {pool_size} round {round}: MCP auxiliary failures must not write model scheduler errors"
                );
            }
        }
    }

    #[tokio::test]
    async fn mcp_error_response_body_is_bounded_while_reading_for_five_rounds() {
        let server = FakeBadRequestServer::start().await;
        for (scenario, expected_kind) in [
            (
                "mcp_error_content_length_over_limit",
                McpCallFailureKind::ResponseTooLarge,
            ),
            (
                "mcp_error_chunked_over_limit",
                McpCallFailureKind::ResponseTooLarge,
            ),
            ("mcp_misleading_500", McpCallFailureKind::Upstream),
        ] {
            for round in 1..=5 {
                let mut config = Config::default();
                config.kiro_upstream_base_url = Some(server.base_url.clone());
                config.kiro_upstream_response_timeout_secs = 2;
                config.credential_retry_max_attempts = 1;
                let manager = Arc::new(
                    MultiTokenManager::new(
                        config,
                        fake_bad_request_credentials(1),
                        None,
                        None,
                        false,
                    )
                    .expect("bounded MCP error token manager"),
                );
                let mut endpoints: HashMap<String, Arc<dyn KiroEndpoint>> = HashMap::new();
                endpoints.insert("ide".to_string(), Arc::new(IdeEndpoint));
                let provider =
                    KiroProvider::with_proxy(manager.clone(), None, endpoints, "ide".to_string());
                let budget = Arc::new(InferenceAttemptBudget::new(4));
                let started = Instant::now();
                let error = provider
                    .call_mcp(
                        &serde_json::json!({"testScenario": scenario}).to_string(),
                        budget.clone(),
                    )
                    .await
                    .expect_err("controlled MCP error response");

                assert!(
                    started.elapsed() < std::time::Duration::from_secs(1),
                    "{scenario} round {round}: bounded read must not wait for pending EOF"
                );
                assert_eq!(
                    KiroProvider::mcp_failure_kind_from_error(&error),
                    Some(expected_kind),
                    "{scenario} round {round}: body text cannot spoof status classification: {error:?}"
                );
                assert_eq!(budget.snapshot().consumed, 1);
                assert!(error.to_string().len() < 1024);
                assert!(!error.to_string().contains("misleading private body"));
                let attribution = KiroProvider::mcp_attribution_from_error(&error);
                assert_eq!(attribution.credential_id, Some(1));
                assert_eq!(attribution.attempts.len(), 1);
                assert_eq!(attribution.attempts[0].credential_id, 1);
                assert_eq!(attribution.attempts[0].status, Some(500));
                assert_eq!(
                    attribution.attempts[0].error_type.as_deref(),
                    Some(expected_kind.as_error_type())
                );
                assert!(!format!("{:?}", attribution).contains("misleading private body"));
                let snapshot = manager.snapshot();
                assert!(
                    snapshot.entries.iter().all(|entry| !entry.cooled_down),
                    "{scenario} round {round}: MCP error body handling must not cool model credentials"
                );
                assert!(
                    snapshot
                        .entries
                        .iter()
                        .all(|entry| entry.last_error_kind.is_none()),
                    "{scenario} round {round}: MCP error body handling must not write model scheduler errors"
                );
            }
        }
    }

    #[tokio::test]
    async fn local_rescue_is_one_real_send_and_zero_when_shared_budget_is_empty_for_five_rounds() {
        const SCENARIO: &str = "rescue_server_error";
        let server = FakeBadRequestServer::start().await;
        let mut config = Config::default();
        config.kiro_upstream_base_url = Some(server.base_url.clone());
        config.kiro_upstream_response_timeout_secs = 2;
        config.credential_retry_max_attempts = 100;
        let manager = Arc::new(
            MultiTokenManager::new(config, fake_bad_request_credentials(20), None, None, false)
                .expect("fake rescue token manager"),
        );
        let mut endpoints: HashMap<String, Arc<dyn KiroEndpoint>> = HashMap::new();
        endpoints.insert("ide".to_string(), Arc::new(IdeEndpoint));
        let provider = KiroProvider::with_proxy(manager, None, endpoints, "ide".to_string());

        for round in 1..=5 {
            let request_body = serde_json::json!({
                "testScenario": SCENARIO,
                "conversationState": {
                    "conversationId": format!("rescue-budget-{round}"),
                    "currentMessage": {
                        "userInputMessage": {
                            "content": "test",
                            "modelId": "claude-sonnet-4"
                        }
                    }
                }
            })
            .to_string();

            let remaining = Arc::new(InferenceAttemptBudget::new(4));
            remaining
                .reserve(InferenceAttemptKind::LocalCredential, 0)
                .unwrap();
            remaining
                .reserve(InferenceAttemptKind::ExternalPool, 0)
                .unwrap();
            let hits_before = server.state.scenario_hits(SCENARIO);
            provider
                .call_api_with_context_with_request_id_and_attempt_budget_max_sends(
                    &request_body,
                    Some("req-rescue-one-send"),
                    AcquireMode::WaitForCapacity,
                    1,
                    Some("claude-sonnet-4"),
                    remaining.clone(),
                    false,
                    Some(1),
                )
                .await
                .err()
                .expect("controlled rescue upstream returns 500");
            assert_eq!(
                server.state.scenario_hits(SCENARIO) - hits_before,
                1,
                "round {round}: local1 + external1 + rescue must send exactly once"
            );
            assert_eq!(remaining.snapshot().consumed, 3, "round {round}");

            let exhausted = Arc::new(InferenceAttemptBudget::new(4));
            for _ in 0..3 {
                exhausted
                    .reserve(InferenceAttemptKind::LocalCredential, 0)
                    .unwrap();
            }
            exhausted
                .reserve(InferenceAttemptKind::ExternalPool, 0)
                .unwrap();
            let hits_before = server.state.scenario_hits(SCENARIO);
            provider
                .call_api_with_context_with_request_id_and_attempt_budget_max_sends(
                    &request_body,
                    Some("req-rescue-no-budget"),
                    AcquireMode::WaitForCapacity,
                    1,
                    Some("claude-sonnet-4"),
                    exhausted.clone(),
                    false,
                    Some(1),
                )
                .await
                .err()
                .expect("exhausted request budget must reject rescue before send");
            assert_eq!(
                server.state.scenario_hits(SCENARIO) - hits_before,
                0,
                "round {round}: local3 + external1 leaves no rescue send"
            );
            assert_eq!(exhausted.snapshot().consumed, 4, "round {round}");
        }
    }

    #[tokio::test]
    async fn api_local_acquire_failure_attaches_selection_summary() {
        let mut config = Config::default();
        config.credential_retry_max_attempts = 100_000;

        let mut disabled = KiroCredentials::default();
        disabled.disabled = true;
        disabled.access_token = Some("secret-token-should-not-leak".to_string());
        let manager =
            Arc::new(MultiTokenManager::new(config, vec![disabled], None, None, false).unwrap());
        let mut endpoints: HashMap<String, Arc<dyn KiroEndpoint>> = HashMap::new();
        endpoints.insert("ide".to_string(), Arc::new(IdeEndpoint));
        let provider = KiroProvider::with_proxy(manager, None, endpoints, "ide".to_string());
        let body = r#"{
            "conversationState": {
                "currentMessage": {
                    "userInputMessage": {
                        "modelId": "claude-sonnet-4.5"
                    }
                }
            }
        }"#;

        let err = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            provider.call_api_with_context_with_request_id(body, Some("req-selection-1")),
        )
        .await
        .expect("本地无可用账号时 API 不应跑满 retry 上限")
        .err()
        .unwrap();

        let summary = KiroProvider::selection_failure_from_error(&err)
            .expect("API 调度失败应携带结构化选择失败摘要");
        assert_eq!(summary.request_id, "req-selection-1");
        assert_eq!(summary.route, "local_account");
        assert_eq!(summary.model, "claude-sonnet-4.5");
        assert_eq!(summary.stage, SelectionFailureStage::AccountEligibility);
        assert_eq!(summary.primary_reason, AccountRejectReason::Disabled);
        assert_eq!(
            summary.reason_counts.get(&AccountRejectReason::Disabled),
            Some(&1)
        );
        assert_eq!(summary.sampled_accounts.len(), 1);

        let serialized = serde_json::to_string(&summary).unwrap();
        assert!(!serialized.contains("secret-token-should-not-leak"));
    }
}

impl KiroProvider {
    /// 创建带代理配置和端点注册表的 KiroProvider 实例
    ///
    /// # Arguments
    /// * `token_manager` - 多凭据 Token 管理器
    /// * `proxy` - 全局代理配置
    /// * `endpoints` - 端点名 → 实现的注册表（至少包含 `default_endpoint` 对应条目）
    /// * `default_endpoint` - 凭据未显式指定 endpoint 时使用的名称
    pub fn with_proxy(
        token_manager: Arc<MultiTokenManager>,
        proxy: Option<ProxyConfig>,
        endpoints: HashMap<String, Arc<dyn KiroEndpoint>>,
        default_endpoint: String,
    ) -> Self {
        assert!(
            endpoints.contains_key(&default_endpoint),
            "默认端点 {} 未在 endpoints 注册表中",
            default_endpoint
        );
        let tls_backend = token_manager.runtime_config().tls_backend;
        // 预热：构建全局代理对应的 Client
        let initial_client =
            build_client(proxy.as_ref(), KIRO_CLIENT_TOTAL_TIMEOUT_SECS, tls_backend)
                .expect("创建 HTTP 客户端失败");
        let initial_cell = Arc::new(OnceCell::new());
        initial_cell
            .set(initial_client)
            .expect("initial Kiro client cache cell must be empty");
        let mut cache = HashMap::new();
        cache.insert(
            proxy.clone(),
            ProviderClientCacheEntry {
                client: initial_cell,
                last_used: 1,
            },
        );

        Self {
            token_manager,
            global_proxy: proxy,
            client_cache: Mutex::new(ProviderClientCache {
                entries: cache,
                clock: 1,
            }),
            client_cache_builds: AtomicU64::new(1),
            tls_backend,
            endpoints,
            default_endpoint,
            model_discovery_in_progress: AtomicBool::new(false),
            model_discovery_round: AtomicU64::new(0),
            profile_arn_discovery_entries: Mutex::new(HashMap::new()),
            profile_arn_discovery_clock: AtomicU64::new(0),
            profile_arn_discovery_policy: ProfileArnDiscoveryPolicy::default(),
            profile_arn_discovery_metrics: ProfileArnDiscoveryMetrics::default(),
        }
    }

    /// 根据凭据的代理配置获取（或创建并缓存）对应的 reqwest::Client
    fn client_for(&self, credentials: &KiroCredentials) -> anyhow::Result<Client> {
        let effective = credentials.effective_proxy(self.global_proxy.as_ref());
        let cell = {
            let mut cache = self.client_cache.lock();
            cache.clock = cache.clock.saturating_add(1);
            let now = cache.clock;
            if let Some(entry) = cache.entries.get_mut(&effective) {
                entry.last_used = now;
                entry.client.clone()
            } else {
                if cache.entries.len() >= KIRO_CLIENT_CACHE_MAX_ENTRIES {
                    let evicted = cache
                        .entries
                        .iter()
                        .filter(|(_, entry)| Arc::strong_count(&entry.client) == 1)
                        .min_by_key(|(_, entry)| entry.last_used)
                        .map(|(key, _)| key.clone())
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "Kiro HTTP client cache is temporarily saturated with active lookups"
                            )
                        })?;
                    cache.entries.remove(&evicted);
                }
                let cell = Arc::new(OnceCell::new());
                cache.entries.insert(
                    effective.clone(),
                    ProviderClientCacheEntry {
                        client: cell.clone(),
                        last_used: now,
                    },
                );
                cell
            }
        };

        let built = cell.get_or_try_init(|| {
            self.client_cache_builds.fetch_add(1, Ordering::Relaxed);
            build_client(
                effective.as_ref(),
                KIRO_CLIENT_TOTAL_TIMEOUT_SECS,
                self.tls_backend,
            )
        });
        built.cloned()
    }

    /// 获取凭据的脱敏展示名称，用于请求级 usage 记录。
    pub fn credential_label(&self, id: u64) -> Option<String> {
        self.token_manager.credential_display_label(id)
    }

    /// 获取当前运行时配置快照。
    pub fn runtime_config(&self) -> Config {
        self.token_manager.runtime_config()
    }

    #[cfg(test)]
    fn profile_arn_discovery_metrics(&self) -> ProfileArnDiscoveryMetricsSnapshot {
        self.profile_arn_discovery_metrics.snapshot()
    }

    /// 获取下游错误响应的 Retry-After 提示。
    ///
    /// 该路径只读本进程内存状态，避免在请求失败热路径触发 Admin 完整快照的
    /// PgSQL/Redis 同步。
    pub fn cooldown_retry_after_hint_secs(&self, fallback_secs: u64) -> u64 {
        self.token_manager
            .cooldown_retry_after_hint_secs(fallback_secs)
    }

    pub fn local_pool_route_state_fresh(&self, model: Option<&str>) -> LocalPoolRouteState {
        self.token_manager.local_pool_route_state_fresh(model)
    }

    pub fn local_pool_route_state_cached(&self, model: Option<&str>) -> LocalPoolRouteState {
        self.token_manager.local_pool_route_state_cached(model)
    }

    fn credential_log_label(&self, id: u64) -> String {
        Self::format_credential_log_label(id, self.credential_label(id))
    }

    fn format_credential_log_label(id: u64, label: Option<String>) -> String {
        let prefix = format!("#{}", id);
        let Some(label) = label
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        else {
            return prefix;
        };

        if label == prefix || label.starts_with(&format!("{} ", prefix)) {
            label
        } else {
            format!("{} {}", prefix, label)
        }
    }

    pub fn attempts_from_error(err: &anyhow::Error) -> Vec<KiroCredentialAttempt> {
        err.downcast_ref::<KiroCallError>()
            .map(|err| err.attempts().to_vec())
            .unwrap_or_default()
    }

    pub fn selection_failure_from_error(err: &anyhow::Error) -> Option<SelectionFailureSummary> {
        err.downcast_ref::<KiroCallError>()
            .and_then(|err| err.selection_failure().cloned())
    }

    pub fn call_failure_kind_from_error(err: &anyhow::Error) -> Option<KiroCallFailureKind> {
        err.downcast_ref::<KiroCallError>()
            .and_then(KiroCallError::failure_kind)
    }

    fn auxiliary_call_failure_kind(err: &anyhow::Error) -> Option<KiroCallFailureKind> {
        if err
            .downcast_ref::<AuxiliaryAttemptBudgetExhausted>()
            .is_some()
        {
            Some(KiroCallFailureKind::AuxiliaryAttemptsExhausted)
        } else if err
            .downcast_ref::<AuxiliaryConcurrencySaturated>()
            .is_some()
        {
            Some(KiroCallFailureKind::AuxiliaryConcurrencySaturated)
        } else {
            None
        }
    }

    fn auxiliary_mcp_failure_kind(err: &anyhow::Error) -> Option<McpCallFailureKind> {
        if err
            .downcast_ref::<AuxiliaryAttemptBudgetExhausted>()
            .is_some()
        {
            Some(McpCallFailureKind::AuxiliaryAttemptLimit)
        } else if err
            .downcast_ref::<AuxiliaryConcurrencySaturated>()
            .is_some()
        {
            Some(McpCallFailureKind::AuxiliaryConcurrency)
        } else {
            None
        }
    }

    fn inference_attempt_rejected_error(
        rejection: InferenceAttemptRejection,
        attempts: &[KiroCredentialAttempt],
    ) -> anyhow::Error {
        let (message, failure_kind) = match rejection {
            InferenceAttemptRejection::Exhausted => (
                "local inference routing limit reached",
                KiroCallFailureKind::InferenceAttemptsExhausted,
            ),
            InferenceAttemptRejection::ReservedForFallback => (
                "local inference attempt reserved for fallback",
                KiroCallFailureKind::InferenceAttemptReservedForFallback,
            ),
            InferenceAttemptRejection::DownstreamCommitted => (
                "downstream response already committed",
                KiroCallFailureKind::DownstreamCommitted,
            ),
        };
        KiroCallError::new(message, attempts.to_vec())
            .with_failure_kind(failure_kind)
            .into()
    }

    fn traced_error(
        message: impl Into<String>,
        attempts: &[KiroCredentialAttempt],
    ) -> anyhow::Error {
        KiroCallError::new(message, attempts.to_vec()).into()
    }

    fn traced_error_with_failure_kind(
        message: impl Into<String>,
        attempts: &[KiroCredentialAttempt],
        failure_kind: KiroCallFailureKind,
    ) -> anyhow::Error {
        KiroCallError::new(message, attempts.to_vec())
            .with_failure_kind(failure_kind)
            .into()
    }

    fn traced_error_with_selection_failure(
        message: impl Into<String>,
        attempts: &[KiroCredentialAttempt],
        selection_failure: Option<SelectionFailureSummary>,
        failure_kind: Option<KiroCallFailureKind>,
    ) -> anyhow::Error {
        let mut error = KiroCallError::new(message, attempts.to_vec())
            .with_selection_failure(selection_failure);
        if let Some(failure_kind) = failure_kind {
            error = error.with_failure_kind(failure_kind);
        }
        error.into()
    }

    fn upstream_content_kind(response: &reqwest::Response) -> UpstreamContentKind {
        let Some(content_type) = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
        else {
            return UpstreamContentKind::Missing;
        };
        let media_type = content_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if Self::is_event_stream_content_type(&media_type) {
            UpstreamContentKind::EventStream
        } else if media_type == "application/json" || media_type.ends_with("+json") {
            UpstreamContentKind::Json
        } else {
            UpstreamContentKind::Other
        }
    }

    async fn read_upstream_body_strict(
        response: reqwest::Response,
        timeout_secs: u64,
        max_bytes: usize,
    ) -> Result<ApiUpstreamBody, ApiUpstreamBodyReadFailure> {
        let declared_body_bytes = response
            .content_length()
            .map(|bytes| usize::try_from(bytes).unwrap_or(usize::MAX));
        let bytes = response_bytes_with_limit_and_body_timeout(response, timeout_secs, max_bytes)
            .await
            .map_err(|error| {
                let kind = match error {
                    HttpSendError::ResponseHeaderTimeout { .. }
                    | HttpSendError::ResponseBodyTimeout { .. } => ApiUpstreamFailureKind::Timeout,
                    HttpSendError::ResponseBodyTooLarge { .. } => {
                        ApiUpstreamFailureKind::ResponseTooLarge
                    }
                    HttpSendError::Request(error) if error.is_timeout() => {
                        ApiUpstreamFailureKind::Timeout
                    }
                    HttpSendError::Request(_) => ApiUpstreamFailureKind::BodyRead,
                };
                ApiUpstreamBodyReadFailure {
                    kind,
                    body_bytes: declared_body_bytes.or_else(|| {
                        (kind == ApiUpstreamFailureKind::ResponseTooLarge)
                            .then_some(max_bytes.saturating_add(1))
                    }),
                }
            })?;
        let body_bytes = bytes.len();
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| ApiUpstreamBodyReadFailure {
                kind: ApiUpstreamFailureKind::Protocol,
                body_bytes: Some(body_bytes),
            })?
            .to_owned();
        Ok(ApiUpstreamBody {
            text,
            bytes: body_bytes,
        })
    }

    fn api_failure_diagnostic(
        kind: ApiUpstreamFailureKind,
        status: reqwest::StatusCode,
        body_bytes: Option<usize>,
        retry_after: Option<Duration>,
        content_kind: Option<UpstreamContentKind>,
        reason: Option<&'static str>,
    ) -> String {
        let body_bytes = body_bytes
            .map(|bytes| bytes.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let retry_after_secs = retry_after
            .map(|duration| duration.as_secs().max(1).to_string())
            .unwrap_or_else(|| "unknown".to_string());
        format!(
            "upstream_failure class={} upstream_status={} public_status={} body_bytes={} retry_after_secs={} content_type={} reason={}",
            kind.as_error_type(),
            status.as_u16(),
            kind.effective_public_status(status),
            body_bytes,
            retry_after_secs,
            content_kind
                .map(UpstreamContentKind::as_str)
                .unwrap_or("unknown"),
            reason.unwrap_or(kind.scheduler_reason())
        )
    }

    fn send_failure_kind(error: &HttpSendError) -> ApiUpstreamFailureKind {
        match error {
            HttpSendError::ResponseHeaderTimeout { .. } => ApiUpstreamFailureKind::Timeout,
            HttpSendError::Request(error) if error.is_timeout() => ApiUpstreamFailureKind::Timeout,
            HttpSendError::Request(_)
            | HttpSendError::ResponseBodyTimeout { .. }
            | HttpSendError::ResponseBodyTooLarge { .. } => ApiUpstreamFailureKind::BodyRead,
        }
    }

    fn classify_http_status_failure(status: reqwest::StatusCode) -> ApiUpstreamFailureKind {
        match status.as_u16() {
            400 => ApiUpstreamFailureKind::InvalidRequest,
            401 | 403 => ApiUpstreamFailureKind::Auth,
            402 => ApiUpstreamFailureKind::Quota,
            408 => ApiUpstreamFailureKind::Timeout,
            429 => ApiUpstreamFailureKind::RateLimit,
            _ if status.is_server_error() => ApiUpstreamFailureKind::Server,
            _ if status.is_client_error() => ApiUpstreamFailureKind::InvalidRequest,
            _ => ApiUpstreamFailureKind::Unknown,
        }
    }

    fn api_transport_failure_diagnostic(kind: ApiUpstreamFailureKind) -> String {
        format!(
            "upstream_failure class={} upstream_status=none public_status=502 body_bytes=0 retry_after_secs=unknown content_type=unknown reason={}",
            kind.as_error_type(),
            kind.scheduler_reason()
        )
    }

    fn risk_control_reason_token(reason: CredentialRiskControlReason) -> &'static str {
        match reason {
            CredentialRiskControlReason::TemporarilySuspended => "temporarily_suspended",
            CredentialRiskControlReason::AccountSuspended => "account_suspended",
            CredentialRiskControlReason::AccountLocked => "account_locked",
        }
    }

    fn bad_request_diagnostic_reason(reason: &'static str) -> &'static str {
        match reason {
            "content_length_exceeds_threshold" => "CONTENT_LENGTH_EXCEEDS_THRESHOLD",
            "context_window_full_bad_request" => {
                "context window is full; reduce conversation history"
            }
            "tool_use_format_bad_request" => "invalid tool use format",
            _ => reason,
        }
    }

    fn structured_upstream_error_codes(body: &str) -> Vec<String> {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
            return Vec::new();
        };
        [
            "/__type",
            "/type",
            "/code",
            "/reason",
            "/exceptionType",
            "/error/__type",
            "/error/type",
            "/error/code",
            "/error/reason",
            "/error/exceptionType",
        ]
        .into_iter()
        .filter_map(|pointer| {
            value
                .pointer(pointer)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .collect()
    }

    fn classify_non_eventstream_body(
        body: &str,
        content_kind: UpstreamContentKind,
    ) -> ApiUpstreamFailureKind {
        let codes = Self::structured_upstream_error_codes(body);
        for code in &codes {
            let normalized = code.to_ascii_lowercase();
            if normalized.contains("throttl")
                || normalized.contains("too_many_request")
                || normalized.contains("toomanyrequest")
                || normalized.contains("rate_limit")
                || normalized.contains("ratelimit")
            {
                return ApiUpstreamFailureKind::RateLimit;
            }
            if normalized.contains("invalid_request")
                || normalized.contains("invalidrequest")
                || normalized.contains("validationexception")
                || normalized.contains("bad_request")
                || normalized.contains("badrequest")
            {
                return ApiUpstreamFailureKind::InvalidRequest;
            }
            if normalized.contains("serviceunavailable")
                || normalized.contains("internalserver")
                || normalized.contains("internalfailure")
                || normalized.contains("temporarilyunavailable")
                || normalized.contains("modelstreamerror")
            {
                return ApiUpstreamFailureKind::Server;
            }
            if normalized.contains("requesttimeout")
                || normalized.contains("request_timeout")
                || normalized == "timeout"
                || normalized == "timeout_error"
            {
                return ApiUpstreamFailureKind::Timeout;
            }
            if normalized.contains("unauthorized")
                || normalized.contains("accessdenied")
                || normalized.contains("unrecognizedclient")
                || normalized.contains("invalidsignature")
            {
                return ApiUpstreamFailureKind::Auth;
            }
        }

        if content_kind == UpstreamContentKind::Json {
            match serde_json::from_str::<serde_json::Value>(body) {
                Ok(value)
                    if !codes.is_empty()
                        || value.get("error").is_some()
                        || value.get("errors").is_some() =>
                {
                    ApiUpstreamFailureKind::Unknown
                }
                Ok(_) | Err(_) => ApiUpstreamFailureKind::Protocol,
            }
        } else {
            ApiUpstreamFailureKind::Protocol
        }
    }

    fn push_attempt(
        attempts: &mut Vec<KiroCredentialAttempt>,
        attempt: usize,
        credential_id: u64,
        credential_label: &str,
        status: Option<reqwest::StatusCode>,
        action: &str,
        error_type: Option<&str>,
        error_message: Option<String>,
        started_at: Instant,
        model: Option<&str>,
    ) {
        attempts.push(
            KiroCredentialAttempt::new(
                attempt,
                credential_id,
                Some(credential_label.to_string()),
                status,
                action,
                error_type,
                error_message,
                started_at.elapsed().as_millis() as u64,
            )
            .with_model(model),
        );
    }

    fn push_mcp_attempt(
        attribution_sink: &McpCallAttributionSink,
        attempts: &mut Vec<KiroCredentialAttempt>,
        attempt: usize,
        credential_id: u64,
        credential_label: &str,
        status: Option<reqwest::StatusCode>,
        action: &str,
        error_type: Option<&str>,
        error_message: Option<String>,
        started_at: Instant,
        model: Option<&str>,
    ) {
        Self::push_attempt(
            attempts,
            attempt,
            credential_id,
            credential_label,
            status,
            action,
            error_type,
            error_message,
            started_at,
            model,
        );
        attribution_sink.replace(
            Some(credential_id),
            Some(credential_label.to_string()),
            attempts.clone(),
        );
    }

    fn log_attempt_chain(
        request_id: Option<&str>,
        api_type: &str,
        attempts: &[KiroCredentialAttempt],
        outcome: &str,
    ) {
        if attempts.is_empty() {
            return;
        }
        let chain = summarize_attempts(attempts);
        match request_id {
            Some(request_id) => tracing::info!(
                request_id,
                api_type,
                outcome,
                credential_chain = %chain,
                "Kiro API 凭据调用链路"
            ),
            None => tracing::info!(
                api_type,
                outcome,
                credential_chain = %chain,
                "Kiro API 凭据调用链路"
            ),
        }
    }

    /// 根据凭据选择 endpoint 实现
    fn endpoint_for(&self, credentials: &KiroCredentials) -> anyhow::Result<Arc<dyn KiroEndpoint>> {
        let name = credentials
            .endpoint
            .as_deref()
            .unwrap_or(&self.default_endpoint);
        self.endpoints
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("未知端点: {}", name))
    }

    fn update_profile_arn_identity_field(hasher: &mut Sha256, label: &str, value: Option<&str>) {
        hasher.update((label.len() as u64).to_be_bytes());
        hasher.update(label.as_bytes());
        match value {
            Some(value) => {
                hasher.update([1]);
                hasher.update((value.len() as u64).to_be_bytes());
                hasher.update(value.as_bytes());
            }
            None => hasher.update([0]),
        }
    }

    fn profile_arn_discovery_key(
        ctx: &CallContext,
        config: &Config,
        machine_id: &str,
    ) -> ProfileArnDiscoveryKey {
        let mut hasher = Sha256::new();
        hasher.update(ctx.id.to_be_bytes());

        // Prefer a refresh credential so a routine access-token rotation does not split the
        // singleflight domain. Access-token-only imports still get an identity that changes when
        // the credential is replaced, preventing numeric ID reuse from inheriting stale backoff.
        let (secret_kind, secret) = if let Some(refresh_token) = ctx
            .credentials
            .refresh_token
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            ("refresh_token", refresh_token)
        } else if let Some(api_key) = ctx
            .credentials
            .kiro_api_key
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            ("kiro_api_key", api_key)
        } else {
            ("access_token", ctx.token.as_str())
        };
        Self::update_profile_arn_identity_field(&mut hasher, "secret_kind", Some(secret_kind));
        Self::update_profile_arn_identity_field(&mut hasher, "secret", Some(secret));
        Self::update_profile_arn_identity_field(
            &mut hasher,
            "auth_method",
            ctx.credentials.auth_method.as_deref(),
        );
        Self::update_profile_arn_identity_field(
            &mut hasher,
            "provider",
            ctx.credentials.provider.as_deref(),
        );
        Self::update_profile_arn_identity_field(
            &mut hasher,
            "client_id",
            ctx.credentials.client_id.as_deref(),
        );
        Self::update_profile_arn_identity_field(
            &mut hasher,
            "token_endpoint",
            ctx.credentials.token_endpoint.as_deref(),
        );
        Self::update_profile_arn_identity_field(
            &mut hasher,
            "api_region",
            Some(
                ctx.credentials
                    .api_region
                    .as_deref()
                    .unwrap_or(config.effective_api_region()),
            ),
        );
        Self::update_profile_arn_identity_field(
            &mut hasher,
            "endpoint",
            ctx.credentials.endpoint.as_deref(),
        );
        Self::update_profile_arn_identity_field(&mut hasher, "machine_id", Some(machine_id));

        let digest = hasher.finalize();
        let mut identity = [0_u8; 32];
        identity.copy_from_slice(&digest);
        ProfileArnDiscoveryKey {
            credential_id: ctx.id,
            identity,
        }
    }

    fn profile_arn_discovery_entry(
        &self,
        key: ProfileArnDiscoveryKey,
    ) -> Option<Arc<ProfileArnDiscoveryEntry>> {
        let last_used = self
            .profile_arn_discovery_clock
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        let mut entries = self.profile_arn_discovery_entries.lock();
        if let Some(entry) = entries.get(&key) {
            entry.last_used.store(last_used, Ordering::Relaxed);
            return Some(entry.clone());
        }

        let max_entries = self.profile_arn_discovery_policy.max_entries.max(1);
        if entries.len() >= max_entries {
            let evict = entries
                .iter()
                .filter(|(_, entry)| Arc::strong_count(entry) == 1)
                .min_by_key(|(_, entry)| entry.last_used.load(Ordering::Relaxed))
                .map(|(key, _)| *key);
            if let Some(evict) = evict {
                entries.remove(&evict);
            }
        }
        if entries.len() >= max_entries {
            let suppressed = self
                .profile_arn_discovery_metrics
                .state_capacity_suppressions
                .fetch_add(1, Ordering::Relaxed)
                .saturating_add(1);
            if suppressed.is_power_of_two() {
                tracing::warn!(
                    auxiliary_channel = "profile_arn_discovery",
                    outcome = "state_capacity_suppressed",
                    state_entries = entries.len(),
                    max_entries,
                    suppressed,
                    "ListAvailableProfiles auxiliary coordination state is saturated; using request fallback"
                );
            }
            return None;
        }

        let entry = Arc::new(ProfileArnDiscoveryEntry::new(last_used));
        entries.insert(key, entry.clone());
        Some(entry)
    }

    fn apply_profile_arn_discovery_cached_state(
        &self,
        entry: &ProfileArnDiscoveryEntry,
        ctx: &mut CallContext,
        now: Instant,
    ) -> bool {
        let state = entry.state.lock();
        match &*state {
            ProfileArnDiscoveryEntryState::Resolved {
                profile_arn,
                expires_at,
            } if *expires_at > now => {
                ctx.credentials.profile_arn = Some(profile_arn.clone());
                true
            }
            ProfileArnDiscoveryEntryState::Negative { retry_at, failures } if *retry_at > now => {
                let retry_after_ms = retry_at.saturating_duration_since(now).as_millis() as u64;
                let suppressed = self
                    .profile_arn_discovery_metrics
                    .negative_cache_suppressions
                    .fetch_add(1, Ordering::Relaxed)
                    .saturating_add(1);
                if suppressed.is_power_of_two() {
                    tracing::debug!(
                        auxiliary_channel = "profile_arn_discovery",
                        outcome = "negative_cache_suppressed",
                        credential_id = ctx.id,
                        failures,
                        retry_after_ms,
                        suppressed,
                        inference_attempt_consumed = false,
                        "ListAvailableProfiles auxiliary request suppressed by per-credential backoff"
                    );
                }
                true
            }
            _ => false,
        }
    }

    fn clear_profile_arn_discovery_state(
        &self,
        ctx: &CallContext,
        config: &Config,
        machine_id: &str,
    ) {
        let key = Self::profile_arn_discovery_key(ctx, config, machine_id);
        self.profile_arn_discovery_entries.lock().remove(&key);
    }

    fn codewhisperer_host_for_region(region: &str) -> &'static str {
        if region.starts_with("eu-") {
            "codewhisperer.eu-central-1.amazonaws.com"
        } else {
            "codewhisperer.us-east-1.amazonaws.com"
        }
    }

    fn list_available_profiles_headers(
        credentials: &KiroCredentials,
        token: &str,
        config: &Config,
        machine_id: &str,
        host: &str,
    ) -> anyhow::Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {}", token))?,
        );
        headers.insert(
            "x-amz-user-agent",
            HeaderValue::from_str(&format!(
                "aws-sdk-js/1.0.34 KiroIDE-{}-{}",
                config.kiro_version, machine_id
            ))?,
        );
        headers.insert(
            "user-agent",
            HeaderValue::from_str(&format!(
                "aws-sdk-js/1.0.34 ua/2.1 os/{} lang/js md/nodejs#{} api/codewhispererruntime#1.0.34 m/E KiroIDE-{}-{}",
                config.system_version, config.node_version, config.kiro_version, machine_id
            ))?,
        );
        headers.insert("host", HeaderValue::from_str(host)?);
        headers.insert(
            "amz-sdk-invocation-id",
            HeaderValue::from_str(&uuid::Uuid::new_v4().to_string())?,
        );
        headers.insert(
            "amz-sdk-request",
            HeaderValue::from_static("attempt=1; max=1"),
        );
        headers.insert("Connection", HeaderValue::from_static("close"));

        if is_external_idp_credentials(credentials) {
            headers.insert("TokenType", HeaderValue::from_static("EXTERNAL_IDP"));
        }

        Ok(headers)
    }

    async fn fetch_enterprise_profile_arn_for_context(
        &self,
        ctx: &CallContext,
        config: &Config,
        machine_id: &str,
        auxiliary_budget: Option<&AuxiliaryAttemptBudget>,
    ) -> anyhow::Result<Option<String>> {
        if !is_external_idp_credentials(&ctx.credentials) {
            return Ok(None);
        }
        if ctx
            .credentials
            .profile_arn
            .as_deref()
            .map(str::trim)
            .is_some_and(|arn| !arn.is_empty() && is_real_profile_arn(arn))
        {
            return Ok(ctx.credentials.profile_arn.clone());
        }

        let region = ctx.credentials.effective_api_region(config);
        let host = Self::codewhisperer_host_for_region(region);
        let url = configured_upstream_url(config, "ListAvailableProfiles")
            .unwrap_or_else(|| format!("https://{}/ListAvailableProfiles", host));
        if let Some(budget) = auxiliary_budget {
            budget.ensure_available(AuxiliaryAttemptKind::ProfileDiscovery)?;
        }
        let auxiliary_controller = self.token_manager.auxiliary_concurrency_controller();
        let _auxiliary_permit =
            auxiliary_controller.try_acquire(AuxiliaryConcurrencyKind::ProfileDiscovery)?;
        let client = self.client_for(&ctx.credentials)?;
        let request = client
            .post(&url)
            .headers(Self::list_available_profiles_headers(
                &ctx.credentials,
                &ctx.token,
                config,
                machine_id,
                host,
            )?)
            .body("{}");
        if let Some(budget) = auxiliary_budget {
            budget.reserve(AuxiliaryAttemptKind::ProfileDiscovery)?;
        }
        let auxiliary_attempt = self
            .profile_arn_discovery_metrics
            .upstream_attempts
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        tracing::debug!(
            auxiliary_channel = "profile_arn_discovery",
            operation = "ListAvailableProfiles",
            credential_id = ctx.id,
            auxiliary_attempt,
            inference_attempt_consumed = false,
            "sending bounded auxiliary profile discovery request"
        );
        let response =
            send_with_response_header_timeout(request, config.kiro_upstream_response_timeout_secs)
                .await?;

        let status = response.status();
        let content_kind = Self::upstream_content_kind(&response);
        let retry_after = Self::retry_after_duration(response.headers());
        if !status.is_success() {
            let diagnostic = match Self::read_upstream_body_strict(
                response,
                config.kiro_upstream_response_timeout_secs,
                PROVIDER_DIAGNOSTIC_BODY_MAX_BYTES,
            )
            .await
            {
                Ok(body) => Self::api_failure_diagnostic(
                    Self::classify_http_status_failure(status),
                    status,
                    Some(body.bytes),
                    retry_after,
                    Some(content_kind),
                    Some("profile_discovery"),
                ),
                Err(failure) => Self::api_failure_diagnostic(
                    failure.kind,
                    status,
                    failure.body_bytes,
                    retry_after,
                    Some(content_kind),
                    Some("profile_discovery_body"),
                ),
            };
            if status.as_u16() == 403 {
                tracing::warn!(
                    credential_id = ctx.id,
                    upstream_status = status.as_u16(),
                    "ListAvailableProfiles denied the auxiliary request; using fallback profile ARN"
                );
                return Ok(None);
            }
            anyhow::bail!("ListAvailableProfiles failed: {}", diagnostic);
        }

        let body = Self::read_upstream_body_strict(
            response,
            config.kiro_upstream_response_timeout_secs,
            PROVIDER_AUXILIARY_BODY_MAX_BYTES,
        )
        .await
        .map_err(|failure| {
            anyhow::anyhow!(Self::api_failure_diagnostic(
                failure.kind,
                status,
                failure.body_bytes,
                retry_after,
                Some(content_kind),
                Some("profile_discovery_body"),
            ))
        })?;

        Ok(extract_first_profile_arn(&body.text).filter(|arn| is_real_profile_arn(arn)))
    }

    async fn ensure_profile_arn_for_context(
        &self,
        ctx: &mut CallContext,
        config: &Config,
        machine_id: &str,
        auxiliary_budget: Option<&AuxiliaryAttemptBudget>,
    ) {
        if !is_external_idp_credentials(&ctx.credentials) {
            return;
        }
        let existing = ctx
            .credentials
            .profile_arn
            .as_deref()
            .map(str::trim)
            .filter(|arn| !arn.is_empty() && is_real_profile_arn(arn));
        if existing.is_some() {
            return;
        }

        let key = Self::profile_arn_discovery_key(ctx, config, machine_id);
        let Some(entry) = self.profile_arn_discovery_entry(key) else {
            return;
        };
        if self.apply_profile_arn_discovery_cached_state(&entry, ctx, Instant::now()) {
            return;
        }

        let _gate = match entry.gate.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                self.profile_arn_discovery_metrics
                    .coalesced_waiters
                    .fetch_add(1, Ordering::Relaxed);
                entry.gate.lock().await
            }
        };
        if self.apply_profile_arn_discovery_cached_state(&entry, ctx, Instant::now()) {
            return;
        }

        let previous_failures = {
            let mut state = entry.state.lock();
            let previous_failures = match &*state {
                ProfileArnDiscoveryEntryState::Idle { previous_failures }
                | ProfileArnDiscoveryEntryState::InFlight { previous_failures } => {
                    *previous_failures
                }
                ProfileArnDiscoveryEntryState::Negative { failures, .. } => *failures,
                ProfileArnDiscoveryEntryState::Resolved { .. } => 0,
            };
            *state = ProfileArnDiscoveryEntryState::InFlight { previous_failures };
            previous_failures
        };

        let discovery = self
            .fetch_enterprise_profile_arn_for_context(ctx, config, machine_id, auxiliary_budget)
            .await;
        match discovery {
            Ok(Some(profile_arn)) => {
                ctx.credentials.profile_arn = Some(profile_arn.clone());
                if ctx.id != EXTERNAL_CREDENTIAL_CONTEXT_ID {
                    if let Err(err) = self
                        .token_manager
                        .update_credential_profile_arn(ctx.id, Some(profile_arn.clone()))
                    {
                        tracing::warn!(
                            credential_id = ctx.id,
                            profile_arn = %profile_arn,
                            "Enterprise profileArn 已解析但持久化失败: {}",
                            err
                        );
                    } else {
                        tracing::info!(
                            credential_id = ctx.id,
                            profile_arn = %profile_arn,
                            "Enterprise profileArn 已解析并持久化"
                        );
                    }
                }
                *entry.state.lock() = ProfileArnDiscoveryEntryState::Resolved {
                    profile_arn,
                    expires_at: Instant::now()
                        + self.profile_arn_discovery_policy.success_handoff_ttl,
                };
                self.profile_arn_discovery_metrics
                    .successes
                    .fetch_add(1, Ordering::Relaxed);
            }
            Ok(None) => {
                let failures = previous_failures.saturating_add(1);
                let backoff = self.profile_arn_discovery_policy.negative_backoff(failures);
                *entry.state.lock() = ProfileArnDiscoveryEntryState::Negative {
                    failures,
                    retry_at: Instant::now() + backoff,
                };
                self.profile_arn_discovery_metrics
                    .negative_results
                    .fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    auxiliary_channel = "profile_arn_discovery",
                    outcome = "negative_result",
                    credential_id = ctx.id,
                    failures,
                    retry_after_ms = backoff.as_millis() as u64,
                    inference_attempt_consumed = false,
                    "ListAvailableProfiles 未返回可用 profileArn，继续使用 fallback profileArn"
                );
            }
            Err(err)
                if err
                    .downcast_ref::<AuxiliaryAttemptBudgetExhausted>()
                    .is_some() =>
            {
                *entry.state.lock() = ProfileArnDiscoveryEntryState::Idle { previous_failures };
                self.profile_arn_discovery_metrics
                    .request_budget_suppressions
                    .fetch_add(1, Ordering::Relaxed);
                tracing::debug!(
                    auxiliary_channel = "profile_arn_discovery",
                    outcome = "request_budget_suppressed",
                    credential_id = ctx.id,
                    inference_attempt_consumed = false,
                    "profile discovery skipped because the request auxiliary budget is exhausted"
                );
            }
            Err(err)
                if err
                    .downcast_ref::<AuxiliaryConcurrencySaturated>()
                    .is_some() =>
            {
                *entry.state.lock() = ProfileArnDiscoveryEntryState::Idle { previous_failures };
                self.profile_arn_discovery_metrics
                    .concurrency_suppressions
                    .fetch_add(1, Ordering::Relaxed);
                tracing::debug!(
                    auxiliary_channel = "profile_arn_discovery",
                    outcome = "concurrency_suppressed",
                    credential_id = ctx.id,
                    inference_attempt_consumed = false,
                    "profile discovery skipped because process auxiliary concurrency is saturated"
                );
            }
            Err(err) => {
                let failures = previous_failures.saturating_add(1);
                let backoff = self.profile_arn_discovery_policy.negative_backoff(failures);
                *entry.state.lock() = ProfileArnDiscoveryEntryState::Negative {
                    failures,
                    retry_at: Instant::now() + backoff,
                };
                self.profile_arn_discovery_metrics
                    .negative_results
                    .fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    auxiliary_channel = "profile_arn_discovery",
                    outcome = "error",
                    credential_id = ctx.id,
                    failures,
                    retry_after_ms = backoff.as_millis() as u64,
                    inference_attempt_consumed = false,
                    "Enterprise profileArn 自愈失败，继续使用 fallback profileArn: {}",
                    err
                );
            }
        }
    }

    fn maybe_exclude_after_soft_failure(
        &self,
        session_id: Option<&str>,
        model: Option<&str>,
        credential_id: u64,
        credential_label: &str,
        excluded_ids: &mut HashSet<u64>,
    ) {
        let Some(session_id) = session_id else {
            return;
        };
        if !self
            .token_manager
            .record_session_soft_failure(session_id, credential_id)
        {
            return;
        }

        if self
            .token_manager
            .has_alternate_usable_credential(model, excluded_ids, credential_id)
        {
            tracing::warn!(
                credential_id,
                credential_label = %credential_label,
                session_id,
                "会话软失败达到阈值，临时排除当前凭据并 fallback"
            );
            excluded_ids.insert(credential_id);
        } else {
            tracing::warn!(
                credential_id,
                credential_label = %credential_label,
                session_id,
                "会话软失败达到阈值，但没有其他可用凭据；保留当前凭据继续重试"
            );
        }
    }

    fn maybe_exclude_after_transient_failure(
        &self,
        model: Option<&str>,
        credential_id: u64,
        credential_label: &str,
        excluded_ids: &mut HashSet<u64>,
    ) -> bool {
        if excluded_ids.contains(&credential_id) {
            return self.token_manager.has_alternate_usable_credential_cached(
                model,
                excluded_ids,
                credential_id,
            );
        }
        if self.token_manager.has_alternate_usable_credential_cached(
            model,
            excluded_ids,
            credential_id,
        ) {
            tracing::warn!(
                credential_id,
                credential_label = %credential_label,
                "账号发生上游瞬态错误，本次请求临时排除当前账号并重试其他账号"
            );
            excluded_ids.insert(credential_id);
            true
        } else {
            false
        }
    }

    async fn handle_credential_auth_failure(
        &self,
        call_scope: &str,
        status: reqwest::StatusCode,
        body: &str,
        recorded_detail: &str,
        ctx: &CallContext,
        endpoint: &dyn KiroEndpoint,
        credential_label: &str,
        model: Option<&str>,
        session_id: Option<&str>,
        auxiliary_attempt_budget: Arc<AuxiliaryAttemptBudget>,
        excluded_ids: &mut HashSet<u64>,
        automatic_recovery_attempted: &mut HashSet<u64>,
        automatic_recovery_allowed: &mut bool,
        can_retry_after_recovery: bool,
    ) -> anyhow::Result<CredentialAuthFailureDecision> {
        // An invalid bearer token gets one conditional, request-budgeted recovery opportunity.
        // Admin force-refresh remains unconditional; request traffic must never call that API.
        if *automatic_recovery_allowed
            && can_retry_after_recovery
            && endpoint.is_bearer_token_invalid(body)
            && !automatic_recovery_attempted.contains(&ctx.id)
        {
            automatic_recovery_attempted.insert(ctx.id);
            tracing::info!(
                credential_id = ctx.id,
                credential_label = %credential_label,
                call_scope,
                "凭据 token 疑似被上游失效，尝试条件自动恢复"
            );
            match self
                .token_manager
                .recover_invalid_access_token_for(
                    ctx.id,
                    &ctx.token,
                    ctx.credentials.storage_revision,
                    auxiliary_attempt_budget,
                )
                .await
            {
                Ok(outcome) => {
                    tracing::info!(
                        credential_id = ctx.id,
                        credential_label = %credential_label,
                        call_scope,
                        recovery_outcome = match outcome {
                            AutomaticTokenRecoveryOutcome::Refreshed => "refreshed",
                            AutomaticTokenRecoveryOutcome::CredentialChanged => {
                                "credential_changed"
                            }
                        },
                        "凭据 token 自动恢复完成，重试请求"
                    );
                    return Ok(CredentialAuthFailureDecision::TokenRecoveryRetry);
                }
                Err(error) => {
                    let auxiliary_suppressed = error
                        .downcast_ref::<AuxiliaryAttemptBudgetExhausted>()
                        .is_some()
                        || error
                            .downcast_ref::<AuxiliaryConcurrencySaturated>()
                            .is_some()
                        || error
                            .downcast_ref::<TokenRefreshAdmissionRejected>()
                            .is_some();
                    if auxiliary_suppressed {
                        *automatic_recovery_allowed = false;
                    }
                    let refresh_failure = error.downcast_ref::<RefreshFailure>();

                    if let Some(failure) = refresh_failure {
                        tracing::warn!(
                            credential_id = ctx.id,
                            credential_label = %credential_label,
                            call_scope,
                            refresh_stage = failure.stage.as_str(),
                            refresh_kind = failure.kind.as_str(),
                            shared_failure_wave = failure.shared_failure_wave,
                            "凭据 token 自动恢复失败"
                        );
                    } else if auxiliary_suppressed {
                        tracing::debug!(
                            credential_id = ctx.id,
                            credential_label = %credential_label,
                            call_scope,
                            "凭据 token 自动恢复被请求预算或进程并发边界抑制"
                        );
                    } else {
                        tracing::warn!(
                            credential_id = ctx.id,
                            credential_label = %credential_label,
                            call_scope,
                            "凭据 token 自动恢复发生健康中性的内部失败"
                        );
                    }

                    // A typed negative wave has exactly one health-action owner. Shared followers,
                    // infrastructure failures, and auxiliary admission failures are request-local
                    // exclusions and must never amplify report_failure or cooldown state.
                    // Refresh-wave health ownership and Redis claim acknowledgement live entirely
                    // in MultiTokenManager. Provider retries only make a request-local routing
                    // decision and must never repeat an account-wide auth/429 mutation.
                    let has_available = self.token_manager.available_count() > 0;

                    excluded_ids.insert(ctx.id);
                    if let Some(session_id) = session_id {
                        self.token_manager
                            .unbind_session_if_bound_to(session_id, ctx.id);
                    }
                    if !has_available {
                        return Ok(CredentialAuthFailureDecision::Exhausted);
                    }
                    return Ok(CredentialAuthFailureDecision::Retry {
                        excluded_current: true,
                    });
                }
            }
        }

        self.token_manager.report_transient_failure_kind(
            ctx.id,
            model,
            TransientFailureKind::Auth,
            None,
            format!("auth_error {} {}", status, recorded_detail),
        )?;

        let has_available = self.token_manager.report_failure(ctx.id);
        if let Some(session_id) = session_id {
            self.token_manager
                .unbind_session_if_bound_to(session_id, ctx.id);
        }

        if !has_available {
            return Ok(CredentialAuthFailureDecision::Exhausted);
        }

        let excluded_current =
            if self
                .token_manager
                .has_alternate_usable_credential(model, excluded_ids, ctx.id)
            {
                excluded_ids.insert(ctx.id);
                true
            } else {
                false
            };

        Ok(CredentialAuthFailureDecision::Retry { excluded_current })
    }

    fn finish_attempt(&self, ctx: &mut CallContext) {
        ctx.release_in_flight();
    }

    /// 发送非流式 API 请求
    ///
    /// 支持多凭据故障转移（见 [`Self::call_api_with_retry`]）
    #[allow(dead_code)]
    pub async fn call_api(&self, request_body: &str) -> anyhow::Result<reqwest::Response> {
        let response = self.call_api_with_context(request_body).await?;
        let (response, completion) = response.into_parts();
        completion.report_success();
        Ok(response)
    }

    /// 发送非流式 API 请求，并返回实际使用的凭据与 sticky 会话信息。
    pub async fn call_api_with_context(
        &self,
        request_body: &str,
    ) -> anyhow::Result<KiroApiResponse> {
        self.call_api_with_context_with_request_id(request_body, None)
            .await
    }

    pub async fn call_api_with_context_with_request_id(
        &self,
        request_body: &str,
        request_id: Option<&str>,
    ) -> anyhow::Result<KiroApiResponse> {
        self.call_api_with_context_with_request_id_and_capacity_weight(request_body, request_id, 1)
            .await
    }

    pub async fn call_api_with_context_with_request_id_and_capacity_weight(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        capacity_weight_units: u32,
    ) -> anyhow::Result<KiroApiResponse> {
        self.call_api_with_context_with_request_id_and_mode(
            request_body,
            request_id,
            AcquireMode::WaitForCapacity,
            capacity_weight_units,
            None,
        )
        .await
    }

    #[allow(dead_code)]
    pub async fn call_api_with_context_with_request_id_fail_fast(
        &self,
        request_body: &str,
        request_id: Option<&str>,
    ) -> anyhow::Result<KiroApiResponse> {
        self.call_api_with_context_with_request_id_and_mode(
            request_body,
            request_id,
            AcquireMode::FailFastOnCapacity,
            1,
            None,
        )
        .await
    }

    #[allow(dead_code)]
    pub async fn call_api_with_context_with_request_id_fail_fast_and_capacity_weight(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        capacity_weight_units: u32,
    ) -> anyhow::Result<KiroApiResponse> {
        self.call_api_with_context_with_request_id_and_mode(
            request_body,
            request_id,
            AcquireMode::FailFastOnCapacity,
            capacity_weight_units,
            None,
        )
        .await
    }

    #[allow(dead_code)]
    pub async fn call_api_with_context_with_request_id_max_wait(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        max_wait: Duration,
    ) -> anyhow::Result<KiroApiResponse> {
        self.call_api_with_context_with_request_id_and_mode(
            request_body,
            request_id,
            AcquireMode::WaitForCapacityMax(max_wait),
            1,
            None,
        )
        .await
    }

    #[allow(dead_code)]
    pub async fn call_api_with_context_with_request_id_max_wait_and_capacity_weight(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        max_wait: Duration,
        capacity_weight_units: u32,
    ) -> anyhow::Result<KiroApiResponse> {
        self.call_api_with_context_with_request_id_and_mode(
            request_body,
            request_id,
            AcquireMode::WaitForCapacityMax(max_wait),
            capacity_weight_units,
            None,
        )
        .await
    }

    async fn call_api_with_context_with_request_id_and_mode(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        acquire_mode: AcquireMode,
        capacity_weight_units: u32,
        dispatch_model_filter: Option<&str>,
    ) -> anyhow::Result<KiroApiResponse> {
        let result = self
            .call_api_with_retry(
                request_body,
                false,
                request_id,
                acquire_mode,
                capacity_weight_units,
                dispatch_model_filter,
                None,
                false,
                None,
                None,
            )
            .await?;
        Ok(KiroApiResponse {
            response: result.response,
            completion: KiroApiCompletion::new(
                self.token_manager.clone(),
                result.credential_id,
                result.in_flight_lease,
                result.session_id,
                result.model,
                result.sticky_bound,
                result.fallback_from_sticky,
                result.attempts,
                result.started_at,
            ),
        })
    }

    pub(crate) async fn call_api_with_context_with_request_id_and_attempt_budget(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        acquire_mode: AcquireMode,
        capacity_weight_units: u32,
        dispatch_model_filter: Option<&str>,
        inference_attempt_budget: Arc<InferenceAttemptBudget>,
        preserve_external_attempt: bool,
    ) -> anyhow::Result<KiroApiResponse> {
        self.call_api_with_context_with_request_id_and_attempt_budget_max_sends(
            request_body,
            request_id,
            acquire_mode,
            capacity_weight_units,
            dispatch_model_filter,
            inference_attempt_budget,
            preserve_external_attempt,
            None,
        )
        .await
    }

    pub(crate) async fn call_api_with_context_with_request_id_and_thinking_signature_retry<F>(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        acquire_mode: AcquireMode,
        capacity_weight_units: u32,
        dispatch_model_filter: Option<&str>,
        inference_attempt_budget: Arc<InferenceAttemptBudget>,
        preserve_external_attempt: bool,
        max_sends: Option<usize>,
        retry_body_builder: F,
    ) -> anyhow::Result<KiroApiResponse>
    where
        F: FnOnce() -> anyhow::Result<String> + Send,
    {
        let result = self
            .call_api_with_retry(
                request_body,
                false,
                request_id,
                acquire_mode,
                capacity_weight_units,
                dispatch_model_filter,
                Some(inference_attempt_budget.as_ref()),
                preserve_external_attempt,
                max_sends,
                Some(Box::new(retry_body_builder)),
            )
            .await?;
        Ok(KiroApiResponse {
            response: result.response,
            completion: KiroApiCompletion::new(
                self.token_manager.clone(),
                result.credential_id,
                result.in_flight_lease,
                result.session_id,
                result.model,
                result.sticky_bound,
                result.fallback_from_sticky,
                result.attempts,
                result.started_at,
            ),
        })
    }

    pub(crate) async fn call_api_with_context_with_request_id_and_attempt_budget_max_sends(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        acquire_mode: AcquireMode,
        capacity_weight_units: u32,
        dispatch_model_filter: Option<&str>,
        inference_attempt_budget: Arc<InferenceAttemptBudget>,
        preserve_external_attempt: bool,
        max_sends: Option<usize>,
    ) -> anyhow::Result<KiroApiResponse> {
        let result = self
            .call_api_with_retry(
                request_body,
                false,
                request_id,
                acquire_mode,
                capacity_weight_units,
                dispatch_model_filter,
                Some(inference_attempt_budget.as_ref()),
                preserve_external_attempt,
                max_sends,
                None,
            )
            .await?;
        Ok(KiroApiResponse {
            response: result.response,
            completion: KiroApiCompletion::new(
                self.token_manager.clone(),
                result.credential_id,
                result.in_flight_lease,
                result.session_id,
                result.model,
                result.sticky_bound,
                result.fallback_from_sticky,
                result.attempts,
                result.started_at,
            ),
        })
    }

    /// 使用指定凭据发送一次非流式 API 请求。
    ///
    /// Admin 测试账号连通性时使用；不参与负载均衡、不做凭据 fallback，
    /// 失败也不累计禁用计数，避免手动测试改变调度状态。
    pub async fn call_api_with_credential(
        &self,
        credential_id: u64,
        request_body: &str,
    ) -> anyhow::Result<KiroApiResponse> {
        let ctx = self
            .token_manager
            .acquire_context_for_credential(credential_id)
            .await?;
        let credential_label = self.credential_log_label(ctx.id);
        let credential_context = format!("账号 {}", credential_label);
        self.call_api_with_single_context(ctx, request_body, credential_label, credential_context)
            .await
    }

    /// 使用外部临时凭据发送一次非流式 API 请求。
    ///
    /// 仅用于 Admin 外部 JSON 验活：不加入凭据池、不参与负载均衡、不累计调度状态。
    pub async fn call_api_with_external_credentials(
        &self,
        credentials: KiroCredentials,
        request_body: &str,
    ) -> anyhow::Result<KiroApiResponse> {
        let ctx = self
            .token_manager
            .acquire_context_for_external_credentials(credentials)
            .await?;
        self.call_api_with_single_context(
            ctx,
            request_body,
            "external".to_string(),
            "外部凭据".to_string(),
        )
        .await
    }

    /// 使用外部临时凭据同步可用模型列表。
    ///
    /// 该方法不把凭据写入调度池，适合 Admin 新增 / 导入 API Key 时先做能力发现，
    /// 然后再把发现结果写回 supportedModels。
    pub async fn list_available_models_for_external_credentials(
        &self,
        credentials: KiroCredentials,
    ) -> anyhow::Result<Vec<KiroAvailableModel>> {
        let ctx = self
            .token_manager
            .acquire_context_for_external_credentials(credentials)
            .await?;
        self.list_available_models_for_context(ctx).await
    }

    async fn call_api_with_single_context(
        &self,
        mut ctx: CallContext,
        request_body: &str,
        credential_label: String,
        credential_context: String,
    ) -> anyhow::Result<KiroApiResponse> {
        ctx.mark_in_flight_kind(InFlightKind::Test);
        let started_at = Instant::now();

        let config = self.token_manager.runtime_config();
        let machine_id = machine_id::generate_from_credentials(&ctx.credentials, &config);
        self.ensure_profile_arn_for_context(&mut ctx, &config, &machine_id, None)
            .await;
        let endpoint = self.endpoint_for(&ctx.credentials).map_err(|e| {
            anyhow::anyhow!(
                "非流式 API 凭据 endpoint 解析失败（{}）: {}",
                credential_context,
                e
            )
        })?;

        let rctx = RequestContext {
            credentials: &ctx.credentials,
            token: &ctx.token,
            machine_id: &machine_id,
            config: &config,
        };

        let url = endpoint.api_url(&rctx);
        let body = crate::http_client::maybe_compress_json_whitespace(
            endpoint.transform_api_body(request_body, &rctx),
            config.compression.enabled && config.compression.whitespace_compression,
        );
        if should_log_upstream_body_size_at_info(body.len(), &config) {
            tracing::info!(
                endpoint = endpoint.name(),
                credential_id = ctx.id,
                credential_label = %credential_label,
                upstream_body_bytes = body.len(),
                pre_endpoint_body_bytes = request_body.len(),
                compression_enabled = config.compression.enabled
                    && config.compression.whitespace_compression,
                "Kiro upstream request body size"
            );
        } else {
            tracing::debug!(
                endpoint = endpoint.name(),
                credential_id = ctx.id,
                credential_label = %credential_label,
                upstream_body_bytes = body.len(),
                pre_endpoint_body_bytes = request_body.len(),
                compression_enabled = config.compression.enabled
                    && config.compression.whitespace_compression,
                "Kiro upstream request body size"
            );
        }
        let base = self
            .client_for(&ctx.credentials)
            .map_err(|e| {
                anyhow::anyhow!(
                    "非流式 API 创建 HTTP client 失败（{}）: {}",
                    credential_context,
                    e
                )
            })?
            .post(&url)
            .body(body)
            .header("content-type", endpoint.content_type())
            .header("Connection", "close");
        let request = endpoint.decorate_api(base, &rctx);

        let response =
            send_with_response_header_timeout(request, config.kiro_upstream_response_timeout_secs)
                .await
                .map_err(|e| {
                    anyhow::anyhow!("非流式 API 请求发送失败（{}）: {}", credential_context, e)
                })?;
        let status = response.status();
        if status.is_success() {
            return Ok(KiroApiResponse {
                response,
                completion: KiroApiCompletion::new(
                    self.token_manager.clone(),
                    ctx.id,
                    ctx.take_in_flight_lease(),
                    None,
                    None,
                    false,
                    false,
                    Vec::new(),
                    started_at,
                ),
            });
        }

        let content_kind = Self::upstream_content_kind(&response);
        let retry_after = Self::retry_after_duration(response.headers());
        let message = match Self::read_upstream_body_strict(
            response,
            config.kiro_upstream_response_timeout_secs,
            PROVIDER_DIAGNOSTIC_BODY_MAX_BYTES,
        )
        .await
        {
            Ok(body) => Self::api_failure_diagnostic(
                Self::classify_http_status_failure(status),
                status,
                Some(body.bytes),
                retry_after,
                Some(content_kind),
                None,
            ),
            Err(failure) => Self::api_failure_diagnostic(
                failure.kind,
                status,
                failure.body_bytes,
                retry_after,
                Some(content_kind),
                None,
            ),
        };
        anyhow::bail!("non_stream credential test failed: {}", message);
    }

    /// 从 Kiro 上游同步可用模型列表。
    ///
    /// 该方法只用于后台模型能力同步：失败会返回给调用方记录状态，不会写入调度失败、
    /// 不会禁用凭据，也不会占用请求并发槽。由于同步会真实调用 Kiro 上游，
    /// 这里只自动使用未禁用凭据，避免后台任务绕过用户手动禁用。
    pub async fn list_available_models(&self) -> anyhow::Result<KiroAvailableModelCatalog> {
        let _run_guard = ModelDiscoveryRunGuard::acquire(&self.model_discovery_in_progress)?;
        let mut send_budget = ModelDiscoverySendBudget::new();
        let cohorts = self.token_manager.local_model_capability_cohorts();
        let cohort_count = cohorts.len();
        if cohort_count == 0 {
            anyhow::bail!("没有健康且可用于同步模型能力的账号");
        }
        let discovery_round = self.model_discovery_round.fetch_add(1, Ordering::AcqRel) as usize;
        let mut samples = Vec::<(usize, u64)>::new();
        for (cohort_index, cohort) in cohorts
            .iter()
            .enumerate()
            .take(MODEL_DISCOVERY_MAX_CREDENTIAL_ATTEMPTS)
        {
            if !cohort.credential_ids.is_empty() {
                let index =
                    discovery_round.wrapping_add(cohort_index) % cohort.credential_ids.len();
                samples.push((cohort_index, cohort.credential_ids[index]));
            }
        }
        // When there are few cohorts, spend at most one remaining attempt per cohort on a second
        // rotating representative. This detects same-class account drift over repeated syncs
        // without scaling auxiliary RPM with account count.
        let mut secondary_depth = 1usize;
        while samples.len() < MODEL_DISCOVERY_MAX_CREDENTIAL_ATTEMPTS {
            let mut added = false;
            for offset in 0..cohort_count {
                if samples.len() >= MODEL_DISCOVERY_MAX_CREDENTIAL_ATTEMPTS {
                    break;
                }
                let cohort_index = discovery_round.wrapping_add(offset) % cohort_count;
                let cohort = &cohorts[cohort_index];
                if cohort.credential_ids.len() <= secondary_depth {
                    continue;
                }
                let index = discovery_round
                    .wrapping_add(cohort_index)
                    .wrapping_add(secondary_depth)
                    % cohort.credential_ids.len();
                let id = cohort.credential_ids[index];
                if !samples.iter().any(|(_, sampled_id)| *sampled_id == id) {
                    samples.push((cohort_index, id));
                    added = true;
                }
            }
            if !added {
                break;
            }
            secondary_depth = secondary_depth.saturating_add(1);
        }
        let attempt_limit = samples.len();
        let mut last_error: Option<anyhow::Error> = None;
        let mut attempted = 0usize;
        let mut successful_catalogs = Vec::new();
        let mut successful_cohorts = HashSet::new();

        for (cohort_index, id) in samples {
            attempted += 1;
            let ctx = match self.token_manager.acquire_context_for_credential(id).await {
                Ok(ctx) => ctx,
                Err(err) => {
                    last_error = Some(anyhow::anyhow!("账号 #{} 获取 token 失败: {}", id, err));
                    continue;
                }
            };
            let ctx_id = ctx.id;
            match self
                .list_available_models_for_context_with_budget(ctx, &mut send_budget)
                .await
            {
                Ok(models) if !models.is_empty() => {
                    successful_cohorts.insert(cohort_index);
                    successful_catalogs.push(models);
                }
                Ok(_) => {
                    last_error = Some(anyhow::anyhow!("账号 #{} 返回空模型列表", id));
                }
                Err(err) => {
                    let label = self.credential_log_label(ctx_id);
                    tracing::warn!(
                        credential_id = ctx_id,
                        credential_label = %label,
                        "同步 Kiro 模型能力失败: {}",
                        err
                    );
                    last_error = Some(err);
                }
            }
        }

        if !successful_catalogs.is_empty() {
            let successful_cohort_count = successful_cohorts.len();
            let complete = cohort_count <= MODEL_DISCOVERY_MAX_CREDENTIAL_ATTEMPTS
                && successful_cohort_count == cohort_count;
            if !complete {
                tracing::warn!(
                    attempted,
                    successful_cohort_count,
                    cohort_count,
                    attempt_limit,
                    "Kiro model capability cohorts were only partially observed; native reasoning will fail closed"
                );
            }
            return Ok(KiroAvailableModelCatalog {
                models: merge_model_discovery_catalogs(successful_catalogs, complete),
                capability_cohort_keys: cohorts.iter().map(|cohort| cohort.key.clone()).collect(),
                successful_cohort_count,
                cohort_count,
                complete,
            });
        }

        let last_error = last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "模型目录为空".to_string());
        tracing::warn!(
            attempted,
            cohort_count,
            attempt_limit,
            "Kiro model discovery exhausted its bounded auxiliary credential attempts"
        );
        Err(anyhow::anyhow!(
            "Kiro model discovery failed after {attempted}/{cohort_count} capability cohorts (limit {attempt_limit}): {last_error}"
        ))
    }

    pub(crate) fn model_capability_cohort_keys(
        &self,
    ) -> Arc<Vec<crate::kiro::model::available_models::KiroModelCapabilityCohortKey>> {
        self.token_manager.local_model_capability_cohort_keys()
    }

    /// 使用指定凭据同步 Kiro 可用模型列表。
    ///
    /// 该方法会真实调用上游模型列表接口，但不占用普通请求并发槽。
    pub async fn list_available_models_for_credential(
        &self,
        id: u64,
    ) -> anyhow::Result<Vec<KiroAvailableModel>> {
        let ctx = self
            .token_manager
            .acquire_context_for_credential(id)
            .await
            .map_err(|err| anyhow::anyhow!("账号 #{} 获取 token 失败: {}", id, err))?;
        self.list_available_models_for_context(ctx).await
    }

    async fn list_available_models_for_context(
        &self,
        ctx: CallContext,
    ) -> anyhow::Result<Vec<KiroAvailableModel>> {
        let mut send_budget = ModelDiscoverySendBudget::new();
        self.list_available_models_for_context_with_budget(ctx, &mut send_budget)
            .await
    }

    async fn list_available_models_for_context_with_budget(
        &self,
        mut ctx: CallContext,
        send_budget: &mut ModelDiscoverySendBudget,
    ) -> anyhow::Result<Vec<KiroAvailableModel>> {
        let config = self.token_manager.runtime_config();
        let machine_id = machine_id::generate_from_credentials(&ctx.credentials, &config);
        self.ensure_profile_arn_for_context(&mut ctx, &config, &machine_id, None)
            .await;
        let auxiliary_controller = self.token_manager.auxiliary_concurrency_controller();
        let _auxiliary_permit =
            auxiliary_controller.try_acquire(AuxiliaryConcurrencyKind::ModelDiscovery)?;
        let endpoint = self.endpoint_for(&ctx.credentials)?;
        let client = self.client_for(&ctx.credentials)?;
        let rctx = RequestContext {
            credentials: &ctx.credentials,
            token: &ctx.token,
            machine_id: &machine_id,
            config: &config,
        };

        let mut all_models = Vec::new();
        let mut next_token: Option<String> = None;
        let mut seen_next_tokens = HashSet::new();
        loop {
            let url = endpoint.models_url(&rctx, next_token.as_deref());
            let mut request = match endpoint.models_method(&rctx) {
                Method::GET => client.get(&url),
                Method::POST => client.post(&url),
                method => client.request(method, &url),
            };
            if let Some(body) = endpoint.models_body(&rctx, next_token.as_deref()) {
                request = request.body(serde_json::to_vec(&body)?);
            }
            let request = endpoint.decorate_models(request, &rctx);
            let request = request
                .build()
                .map_err(|_| anyhow::anyhow!("ListAvailableModels request_build_error"))?;
            send_budget.reserve()?;
            let response = execute_with_response_header_timeout(
                &client,
                request,
                config.kiro_upstream_response_timeout_secs,
            )
            .await?;
            let status = response.status();
            let content_kind = Self::upstream_content_kind(&response);
            let retry_after = Self::retry_after_duration(response.headers());
            let body = Self::read_upstream_body_strict(
                response,
                config.kiro_upstream_response_timeout_secs,
                PROVIDER_AUXILIARY_BODY_MAX_BYTES,
            )
            .await
            .map_err(|failure| {
                anyhow::anyhow!(Self::api_failure_diagnostic(
                    failure.kind,
                    status,
                    failure.body_bytes,
                    retry_after,
                    Some(content_kind),
                    Some("model_discovery_body"),
                ))
            })?;
            if !status.is_success() {
                anyhow::bail!(
                    "ListAvailableModels failed: {}",
                    Self::api_failure_diagnostic(
                        Self::classify_http_status_failure(status),
                        status,
                        Some(body.bytes),
                        retry_after,
                        Some(content_kind),
                        Some("model_discovery")
                    )
                );
            }
            let parsed: KiroAvailableModelsResponse = serde_json::from_str(&body.text)
                .map_err(|_| anyhow::anyhow!("ListAvailableModels protocol_error"))?;
            all_models.extend(
                parsed
                    .models
                    .into_iter()
                    .filter(|model| !model.model_id.trim().is_empty()),
            );
            next_token = parsed
                .next_token
                .map(|token| token.trim().to_string())
                .filter(|token| !token.is_empty());
            let Some(token) = next_token.as_ref() else {
                break;
            };
            if !seen_next_tokens.insert(token.clone()) {
                anyhow::bail!("ListAvailableModels protocol_error: repeated pagination token");
            }
        }

        Ok(all_models)
    }

    /// 发送流式 API 请求
    #[allow(dead_code)]
    pub async fn call_api_stream(&self, request_body: &str) -> anyhow::Result<KiroStreamResponse> {
        self.call_api_stream_with_request_id(request_body, None)
            .await
    }

    pub async fn call_api_stream_with_request_id(
        &self,
        request_body: &str,
        request_id: Option<&str>,
    ) -> anyhow::Result<KiroStreamResponse> {
        self.call_api_stream_with_request_id_and_capacity_weight(request_body, request_id, 1)
            .await
    }

    pub async fn call_api_stream_with_request_id_and_capacity_weight(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        capacity_weight_units: u32,
    ) -> anyhow::Result<KiroStreamResponse> {
        self.call_api_stream_with_request_id_and_mode(
            request_body,
            request_id,
            AcquireMode::WaitForCapacity,
            capacity_weight_units,
            None,
        )
        .await
    }

    #[allow(dead_code)]
    pub async fn call_api_stream_with_request_id_fail_fast(
        &self,
        request_body: &str,
        request_id: Option<&str>,
    ) -> anyhow::Result<KiroStreamResponse> {
        self.call_api_stream_with_request_id_and_mode(
            request_body,
            request_id,
            AcquireMode::FailFastOnCapacity,
            1,
            None,
        )
        .await
    }

    #[allow(dead_code)]
    pub async fn call_api_stream_with_request_id_fail_fast_and_capacity_weight(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        capacity_weight_units: u32,
    ) -> anyhow::Result<KiroStreamResponse> {
        self.call_api_stream_with_request_id_and_mode(
            request_body,
            request_id,
            AcquireMode::FailFastOnCapacity,
            capacity_weight_units,
            None,
        )
        .await
    }

    #[allow(dead_code)]
    pub async fn call_api_stream_with_request_id_max_wait(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        max_wait: Duration,
    ) -> anyhow::Result<KiroStreamResponse> {
        self.call_api_stream_with_request_id_and_mode(
            request_body,
            request_id,
            AcquireMode::WaitForCapacityMax(max_wait),
            1,
            None,
        )
        .await
    }

    #[allow(dead_code)]
    pub async fn call_api_stream_with_request_id_max_wait_and_capacity_weight(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        max_wait: Duration,
        capacity_weight_units: u32,
    ) -> anyhow::Result<KiroStreamResponse> {
        self.call_api_stream_with_request_id_and_mode(
            request_body,
            request_id,
            AcquireMode::WaitForCapacityMax(max_wait),
            capacity_weight_units,
            None,
        )
        .await
    }

    async fn call_api_stream_with_request_id_and_mode(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        acquire_mode: AcquireMode,
        capacity_weight_units: u32,
        dispatch_model_filter: Option<&str>,
    ) -> anyhow::Result<KiroStreamResponse> {
        let result = self
            .call_api_with_retry(
                request_body,
                true,
                request_id,
                acquire_mode,
                capacity_weight_units,
                dispatch_model_filter,
                None,
                false,
                None,
                None,
            )
            .await?;
        Ok(KiroStreamResponse {
            response: result.response,
            completion: KiroStreamCompletion::new(
                self.token_manager.clone(),
                result.credential_id,
                result.in_flight_lease,
                result.session_id,
                result.model,
                result.sticky_bound,
                result.fallback_from_sticky,
                result.attempts,
                result.started_at,
            ),
        })
    }

    pub(crate) async fn call_api_stream_with_request_id_and_attempt_budget(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        acquire_mode: AcquireMode,
        capacity_weight_units: u32,
        dispatch_model_filter: Option<&str>,
        inference_attempt_budget: Arc<InferenceAttemptBudget>,
        preserve_external_attempt: bool,
    ) -> anyhow::Result<KiroStreamResponse> {
        self.call_api_stream_with_request_id_and_attempt_budget_max_sends(
            request_body,
            request_id,
            acquire_mode,
            capacity_weight_units,
            dispatch_model_filter,
            inference_attempt_budget,
            preserve_external_attempt,
            None,
        )
        .await
    }

    pub(crate) async fn call_api_stream_with_request_id_and_thinking_signature_retry<F>(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        acquire_mode: AcquireMode,
        capacity_weight_units: u32,
        dispatch_model_filter: Option<&str>,
        inference_attempt_budget: Arc<InferenceAttemptBudget>,
        preserve_external_attempt: bool,
        max_sends: Option<usize>,
        retry_body_builder: F,
    ) -> anyhow::Result<KiroStreamResponse>
    where
        F: FnOnce() -> anyhow::Result<String> + Send,
    {
        let result = self
            .call_api_with_retry(
                request_body,
                true,
                request_id,
                acquire_mode,
                capacity_weight_units,
                dispatch_model_filter,
                Some(inference_attempt_budget.as_ref()),
                preserve_external_attempt,
                max_sends,
                Some(Box::new(retry_body_builder)),
            )
            .await?;
        Ok(KiroStreamResponse {
            response: result.response,
            completion: KiroStreamCompletion::new(
                self.token_manager.clone(),
                result.credential_id,
                result.in_flight_lease,
                result.session_id,
                result.model,
                result.sticky_bound,
                result.fallback_from_sticky,
                result.attempts,
                result.started_at,
            ),
        })
    }

    pub(crate) async fn call_api_stream_with_request_id_and_attempt_budget_max_sends(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        acquire_mode: AcquireMode,
        capacity_weight_units: u32,
        dispatch_model_filter: Option<&str>,
        inference_attempt_budget: Arc<InferenceAttemptBudget>,
        preserve_external_attempt: bool,
        max_sends: Option<usize>,
    ) -> anyhow::Result<KiroStreamResponse> {
        let result = self
            .call_api_with_retry(
                request_body,
                true,
                request_id,
                acquire_mode,
                capacity_weight_units,
                dispatch_model_filter,
                Some(inference_attempt_budget.as_ref()),
                preserve_external_attempt,
                max_sends,
                None,
            )
            .await?;
        Ok(KiroStreamResponse {
            response: result.response,
            completion: KiroStreamCompletion::new(
                self.token_manager.clone(),
                result.credential_id,
                result.in_flight_lease,
                result.session_id,
                result.model,
                result.sticky_bound,
                result.fallback_from_sticky,
                result.attempts,
                result.started_at,
            ),
        })
    }

    /// 发送 MCP API 请求（WebSearch 等工具调用）
    #[cfg(test)]
    pub async fn call_mcp(
        &self,
        request_body: &str,
        inference_attempt_budget: Arc<InferenceAttemptBudget>,
    ) -> anyhow::Result<McpCallResponse> {
        self.call_mcp_with_attribution_sink(
            request_body,
            inference_attempt_budget,
            Arc::new(McpCallAttributionSink::default()),
            None,
        )
        .await
    }

    pub async fn call_mcp_with_attribution_sink(
        &self,
        request_body: &str,
        inference_attempt_budget: Arc<InferenceAttemptBudget>,
        attribution_sink: Arc<McpCallAttributionSink>,
        request_id: Option<&str>,
    ) -> anyhow::Result<McpCallResponse> {
        match self
            .call_mcp_with_retry(
                request_body,
                inference_attempt_budget.as_ref(),
                attribution_sink.clone(),
                request_id,
            )
            .await
        {
            Ok(response) => Ok(response),
            Err(error) if error.downcast_ref::<McpCallError>().is_some() => Err(error),
            Err(error) => {
                let kind = if Self::call_failure_kind_from_error(&error).is_some() {
                    McpCallFailureKind::AttemptLimit
                } else {
                    McpCallFailureKind::Upstream
                };
                let attribution = attribution_sink.snapshot();
                Err(Self::mcp_failure_error_with_attribution(
                    kind,
                    error.to_string(),
                    attribution.credential_id,
                    attribution.credential_label,
                    attribution.attempts,
                ))
            }
        }
    }

    pub fn mcp_failure_kind_from_error(error: &anyhow::Error) -> Option<McpCallFailureKind> {
        error.downcast_ref::<McpCallError>().map(|error| error.kind)
    }

    pub fn mcp_attribution_from_error(error: &anyhow::Error) -> McpCallAttribution {
        error
            .downcast_ref::<McpCallError>()
            .map(|error| error.attribution.clone())
            .unwrap_or_default()
    }

    pub(crate) fn mcp_failure_error(
        kind: McpCallFailureKind,
        message: impl Into<String>,
    ) -> anyhow::Error {
        McpCallError {
            kind,
            message: message.into(),
            attribution: McpCallAttribution::default(),
        }
        .into()
    }

    fn mcp_failure_error_with_attribution(
        kind: McpCallFailureKind,
        message: impl Into<String>,
        credential_id: Option<u64>,
        credential_label: Option<String>,
        attempts: Vec<KiroCredentialAttempt>,
    ) -> anyhow::Error {
        McpCallError {
            kind,
            message: message.into(),
            attribution: McpCallAttribution {
                credential_id,
                credential_label,
                attempts,
                selection_failure: None,
            },
        }
        .into()
    }

    fn mcp_failure_error_with_selection_failure(
        kind: McpCallFailureKind,
        message: impl Into<String>,
        selection_failure: SelectionFailureSummary,
    ) -> anyhow::Error {
        McpCallError {
            kind,
            message: message.into(),
            attribution: McpCallAttribution {
                selection_failure: Some(selection_failure),
                ..Default::default()
            },
        }
        .into()
    }

    fn mcp_failure_kind_for_status(status: reqwest::StatusCode) -> McpCallFailureKind {
        match status.as_u16() {
            400 => McpCallFailureKind::InvalidRequest,
            408 => McpCallFailureKind::Timeout,
            429 => McpCallFailureKind::RateLimit,
            _ => McpCallFailureKind::Upstream,
        }
    }

    /// 内部方法：带重试逻辑的 MCP API 调用
    async fn call_mcp_with_retry(
        &self,
        request_body: &str,
        inference_attempt_budget: &InferenceAttemptBudget,
        attribution_sink: Arc<McpCallAttributionSink>,
        request_id: Option<&str>,
    ) -> anyhow::Result<McpCallResponse> {
        let total_credentials = self.token_manager.total_count();
        let max_retries =
            Self::max_retry_attempts(total_credentials, &self.token_manager.runtime_config())
                .min(inference_attempt_budget.available_attempts(0) as usize);
        if max_retries == 0 {
            let snapshot = inference_attempt_budget.snapshot();
            let rejection = if snapshot.downstream_committed {
                InferenceAttemptRejection::DownstreamCommitted
            } else {
                InferenceAttemptRejection::Exhausted
            };
            return Err(Self::inference_attempt_rejected_error(rejection, &[]));
        }
        let mut last_error: Option<anyhow::Error> = None;
        let mut last_credential_id = None;
        let mut last_credential_label = None;
        let mut attempts = Vec::new();
        let mut automatic_recovery_attempted: HashSet<u64> = HashSet::new();
        let mut automatic_recovery_allowed = true;
        let mut excluded_ids: HashSet<u64> = HashSet::new();
        let auxiliary_attempt_budget = inference_attempt_budget.auxiliary_budget();

        for attempt in 0..max_retries {
            // MCP 调用（WebSearch 等工具）不涉及模型选择，无需按模型过滤凭据
            let mut ctx = match self
                .token_manager
                .acquire_context_for_session_with_mode_and_auxiliary_budget(
                    None,
                    None,
                    &excluded_ids,
                    AcquireMode::WaitForCapacity,
                    1,
                    Some(auxiliary_attempt_budget.clone()),
                )
                .await
            {
                Ok(c) => c,
                Err(e) => {
                    if last_error.is_none() {
                        let error_message = e.to_string();
                        let failure_kind = Self::auxiliary_mcp_failure_kind(&e)
                            .unwrap_or(McpCallFailureKind::Scheduler);
                        let selection_failure = self.token_manager.selection_failure_summary(
                            request_id.unwrap_or("mcp"),
                            "mcp",
                            None,
                            &error_message,
                        );
                        return Err(Self::mcp_failure_error_with_selection_failure(
                            failure_kind,
                            format!("MCP 获取凭据失败: {error_message}"),
                            selection_failure,
                        ));
                    } else {
                        tracing::warn!(
                            error = %e,
                            "MCP 获取凭据失败，但保留之前的上游错误"
                        );
                    }
                    break;
                }
            };
            ctx.mark_in_flight_kind(InFlightKind::Mcp);
            let credential_label = self.credential_log_label(ctx.id);
            let credential_context = format!("账号 {}", credential_label);
            let attempt_started_at = Instant::now();
            last_credential_id = Some(ctx.id);
            last_credential_label = Some(credential_label.clone());

            let config = self.token_manager.runtime_config();
            let machine_id = machine_id::generate_from_credentials(&ctx.credentials, &config);
            self.ensure_profile_arn_for_context(
                &mut ctx,
                &config,
                &machine_id,
                Some(auxiliary_attempt_budget.as_ref()),
            )
            .await;

            let endpoint = match self.endpoint_for(&ctx.credentials) {
                Ok(e) => e,
                Err(e) => {
                    Self::push_mcp_attempt(
                        attribution_sink.as_ref(),
                        &mut attempts,
                        attempt,
                        ctx.id,
                        &credential_label,
                        None,
                        "fail",
                        Some("mcp_endpoint_resolution"),
                        Some("mcp_endpoint_resolution".to_string()),
                        attempt_started_at,
                        None,
                    );
                    last_error = Some(anyhow::anyhow!(
                        "MCP 凭据 endpoint 解析失败（{}）: {}",
                        credential_context,
                        e
                    ));
                    // endpoint 解析失败：记为失败，换下一张凭据
                    tracing::warn!(
                        credential_id = ctx.id,
                        credential_label = %credential_label,
                        "MCP 凭据 endpoint 解析失败（{}），计入失败: {}",
                        credential_context,
                        e
                    );
                    self.token_manager.report_failure(ctx.id);
                    self.finish_attempt(&mut ctx);
                    continue;
                }
            };

            let rctx = RequestContext {
                credentials: &ctx.credentials,
                token: &ctx.token,
                machine_id: &machine_id,
                config: &config,
            };

            let url = endpoint.mcp_url(&rctx);
            let body = crate::http_client::maybe_compress_json_whitespace(
                endpoint.transform_mcp_body(request_body, &rctx),
                config.compression.enabled && config.compression.whitespace_compression,
            );

            let client = match self.client_for(&ctx.credentials) {
                Ok(client) => client,
                Err(e) => {
                    Self::push_mcp_attempt(
                        attribution_sink.as_ref(),
                        &mut attempts,
                        attempt,
                        ctx.id,
                        &credential_label,
                        None,
                        "fail",
                        Some("mcp_http_client"),
                        Some("mcp_http_client_creation".to_string()),
                        attempt_started_at,
                        None,
                    );
                    self.finish_attempt(&mut ctx);
                    return Err(Self::mcp_failure_error_with_attribution(
                        McpCallFailureKind::Upstream,
                        format!("MCP 创建 HTTP client 失败（{}）: {}", credential_context, e),
                        Some(ctx.id),
                        Some(credential_label),
                        attempts,
                    ));
                }
            };
            let base = client
                .post(&url)
                .body(body)
                .header("content-type", endpoint.content_type())
                .header("Connection", "close");
            let request = endpoint.decorate_mcp(base, &rctx);

            if let Err(rejection) = inference_attempt_budget.reserve(InferenceAttemptKind::Mcp, 0) {
                tracing::warn!(
                    credential_id = ctx.id,
                    credential_label = %credential_label,
                    rejection = ?rejection,
                    "shared inference attempt policy rejected MCP upstream send"
                );
                Self::push_attempt(
                    &mut attempts,
                    attempt,
                    ctx.id,
                    &credential_label,
                    None,
                    "fail",
                    Some("mcp_attempt_limit"),
                    Some("mcp_shared_attempt_limit".to_string()),
                    attempt_started_at,
                    None,
                );
                self.finish_attempt(&mut ctx);
                return Err(Self::mcp_failure_error_with_attribution(
                    McpCallFailureKind::AttemptLimit,
                    Self::inference_attempt_rejected_error(rejection, &attempts).to_string(),
                    Some(ctx.id),
                    Some(credential_label),
                    attempts,
                ));
            }
            attribution_sink.begin_send(attempt, ctx.id, &credential_label);

            let response = match send_with_response_header_timeout(
                request,
                config.kiro_upstream_response_timeout_secs,
            )
            .await
            {
                Ok(resp) => resp,
                Err(e) => {
                    tracing::warn!(
                        credential_id = ctx.id,
                        credential_label = %credential_label,
                        "MCP 请求发送失败（{}，尝试 {}/{}）: {}",
                        credential_context,
                        attempt + 1,
                        max_retries,
                        e
                    );
                    let failure_kind = match e {
                        HttpSendError::ResponseHeaderTimeout { .. }
                        | HttpSendError::ResponseBodyTimeout { .. } => McpCallFailureKind::Timeout,
                        HttpSendError::Request(_) | HttpSendError::ResponseBodyTooLarge { .. } => {
                            McpCallFailureKind::Upstream
                        }
                    };
                    Self::push_mcp_attempt(
                        attribution_sink.as_ref(),
                        &mut attempts,
                        attempt,
                        ctx.id,
                        &credential_label,
                        None,
                        if attempt + 1 < max_retries {
                            "retry"
                        } else {
                            "fail"
                        },
                        Some(failure_kind.as_error_type()),
                        Some(failure_kind.scheduler_reason().to_string()),
                        attempt_started_at,
                        None,
                    );
                    last_error = Some(Self::mcp_failure_error(
                        failure_kind,
                        format!("MCP 请求发送失败（{}）: {}", credential_context, e),
                    ));
                    let retry_target_available = self.maybe_exclude_after_transient_failure(
                        None,
                        ctx.id,
                        &credential_label,
                        &mut excluded_ids,
                    );
                    self.finish_attempt(&mut ctx);
                    if attempt + 1 < max_retries && retry_target_available {
                        sleep(Self::retry_delay(attempt)).await;
                        continue;
                    }
                    if let Some(last) = attempts.last_mut() {
                        last.action = "fail".to_string();
                    }
                    return Err(Self::mcp_failure_error_with_attribution(
                        failure_kind,
                        last_error
                            .take()
                            .map(|error| error.to_string())
                            .unwrap_or_else(|| {
                                format!("MCP 请求发送失败（{}）", credential_context)
                            }),
                        Some(ctx.id),
                        Some(credential_label),
                        attempts,
                    ));
                }
            };

            let status = response.status();
            // 成功响应
            if status.is_success() {
                return Ok(McpCallResponse {
                    response,
                    completion: McpCallCompletion::new_with_attribution_sink(
                        self.token_manager.clone(),
                        ctx.id,
                        credential_label,
                        ctx.take_in_flight_lease(),
                        attempts,
                        attempt,
                        status,
                        attempt_started_at,
                        attribution_sink,
                    ),
                });
            }

            // 失败响应
            let body = match response_text_with_limit_and_body_timeout(
                response,
                config.kiro_upstream_response_timeout_secs,
                PROVIDER_DIAGNOSTIC_BODY_MAX_BYTES,
            )
            .await
            {
                Ok(body) => body,
                Err(error) => {
                    let failure_kind = match status.as_u16() {
                        400 => McpCallFailureKind::InvalidRequest,
                        408 => McpCallFailureKind::Timeout,
                        429 => McpCallFailureKind::RateLimit,
                        _ => match error {
                            HttpSendError::ResponseBodyTimeout { .. }
                            | HttpSendError::ResponseHeaderTimeout { .. } => {
                                McpCallFailureKind::Timeout
                            }
                            HttpSendError::ResponseBodyTooLarge { .. } => {
                                McpCallFailureKind::ResponseTooLarge
                            }
                            HttpSendError::Request(_) => McpCallFailureKind::BodyRead,
                        },
                    };
                    Self::push_mcp_attempt(
                        attribution_sink.as_ref(),
                        &mut attempts,
                        attempt,
                        ctx.id,
                        &credential_label,
                        Some(status),
                        if failure_kind == McpCallFailureKind::InvalidRequest
                            || attempt + 1 >= max_retries
                        {
                            "fail"
                        } else {
                            "retry"
                        },
                        Some(failure_kind.as_error_type()),
                        Some(failure_kind.scheduler_reason().to_string()),
                        attempt_started_at,
                        None,
                    );
                    self.finish_attempt(&mut ctx);

                    if failure_kind == McpCallFailureKind::InvalidRequest {
                        return Err(Self::mcp_failure_error_with_attribution(
                            failure_kind,
                            format!("MCP 请求失败（{}）: {}", credential_context, status),
                            Some(ctx.id),
                            Some(credential_label),
                            attempts,
                        ));
                    }

                    last_error = Some(Self::mcp_failure_error(
                        failure_kind,
                        format!("MCP 响应正文读取失败（{}）", credential_context),
                    ));
                    let retry_target_available = failure_kind
                        .should_retry_mcp_with_alternate_credential()
                        && self.maybe_exclude_after_transient_failure(
                            None,
                            ctx.id,
                            &credential_label,
                            &mut excluded_ids,
                        );
                    if attempt + 1 < max_retries && retry_target_available {
                        sleep(Self::retry_delay(attempt)).await;
                        continue;
                    }
                    if let Some(last) = attempts.last_mut() {
                        last.action = "fail".to_string();
                    }
                    return Err(Self::mcp_failure_error_with_attribution(
                        failure_kind,
                        last_error
                            .take()
                            .map(|error| error.to_string())
                            .unwrap_or_else(|| {
                                format!("MCP 响应正文读取失败（{}）", credential_context)
                            }),
                        Some(ctx.id),
                        Some(credential_label),
                        attempts,
                    ));
                }
            };
            let response_body_bytes = body.len();

            if let Some(risk_reason) = Self::detect_risk_control_error(status, &body) {
                if Self::should_downgrade_rate_limit_risk_to_cooldown(
                    status,
                    risk_reason,
                    &ctx.credentials,
                ) {
                    tracing::warn!(
                        credential_id = ctx.id,
                        credential_label = %credential_label,
                        risk_reason = ?risk_reason,
                        "MCP 请求失败（{}，429 临时风控按账号配置仅进入冷却并切换，尝试 {}/{}）: status={}, body_bytes={}",
                        credential_context,
                        attempt + 1,
                        max_retries,
                        status,
                        response_body_bytes
                    );
                    let failure_kind = Self::mcp_failure_kind_for_status(status);
                    Self::push_mcp_attempt(
                        attribution_sink.as_ref(),
                        &mut attempts,
                        attempt,
                        ctx.id,
                        &credential_label,
                        Some(status),
                        if attempt + 1 < max_retries {
                            "retry"
                        } else {
                            "fail"
                        },
                        Some(failure_kind.as_error_type()),
                        Some("mcp_rate_limit_risk_control".to_string()),
                        attempt_started_at,
                        None,
                    );
                    last_error = Some(Self::mcp_failure_error(
                        failure_kind,
                        format!("MCP 请求失败（{}）: {}", credential_context, status),
                    ));
                    let retry_target_available = self.maybe_exclude_after_transient_failure(
                        None,
                        ctx.id,
                        &credential_label,
                        &mut excluded_ids,
                    );
                    self.finish_attempt(&mut ctx);
                    if attempt + 1 < max_retries && retry_target_available {
                        continue;
                    }
                    if let Some(last) = attempts.last_mut() {
                        last.action = "fail".to_string();
                    }
                    return Err(Self::mcp_failure_error_with_attribution(
                        failure_kind,
                        last_error
                            .take()
                            .map(|error| error.to_string())
                            .unwrap_or_else(|| {
                                format!("MCP 请求失败（{}）: {}", credential_context, status)
                            }),
                        Some(ctx.id),
                        Some(credential_label),
                        attempts,
                    ));
                }

                tracing::error!(
                    credential_id = ctx.id,
                    credential_label = %credential_label,
                    risk_reason = ?risk_reason,
                    "MCP 请求失败（{}，命中上游风控/封禁状态，禁用凭据并切换，尝试 {}/{}）: status={}, body_bytes={}",
                    credential_context,
                    attempt + 1,
                    max_retries,
                    status,
                    response_body_bytes
                );
                let risk_outcome = self.token_manager.report_risk_controlled_outcome(
                    ctx.id,
                    risk_reason,
                    format!("MCP status={status} risk_control={risk_reason:?}"),
                );
                let failure_kind = Self::mcp_failure_kind_for_status(status);
                Self::push_mcp_attempt(
                    attribution_sink.as_ref(),
                    &mut attempts,
                    attempt,
                    ctx.id,
                    &credential_label,
                    Some(status),
                    if risk_outcome.can_retry_local() {
                        "retry"
                    } else {
                        "fail"
                    },
                    Some(failure_kind.as_error_type()),
                    Some("mcp_risk_control".to_string()),
                    attempt_started_at,
                    None,
                );
                if !risk_outcome.can_retry_local() {
                    self.finish_attempt(&mut ctx);
                    return Err(Self::mcp_failure_error_with_attribution(
                        failure_kind,
                        if risk_outcome.circuit_open {
                            format!(
                                "MCP 请求失败（{}，本地账号池风险保护已打开，retry_after_secs={}）: {}",
                                credential_context,
                                risk_outcome.retry_after_secs.unwrap_or(1),
                                status
                            )
                        } else {
                            format!(
                                "MCP 请求失败（{}，所有账号已用尽）: {}",
                                credential_context, status
                            )
                        },
                        Some(ctx.id),
                        Some(credential_label),
                        attempts,
                    ));
                }
                last_error = Some(Self::mcp_failure_error(
                    failure_kind,
                    format!("MCP 请求失败（{}）: {}", credential_context, status),
                ));
                self.finish_attempt(&mut ctx);
                continue;
            }

            // 402 额度用尽
            if status.as_u16() == 402 && endpoint.is_monthly_request_limit(&body) {
                tracing::warn!(
                    credential_id = ctx.id,
                    credential_label = %credential_label,
                    "MCP 请求失败（{}，额度已用尽，禁用凭据并切换，尝试 {}/{}）: status={}, body_bytes={}",
                    credential_context,
                    attempt + 1,
                    max_retries,
                    status,
                    response_body_bytes
                );
                let has_available = self.token_manager.report_quota_exhausted(ctx.id);
                let failure_kind = Self::mcp_failure_kind_for_status(status);
                Self::push_mcp_attempt(
                    attribution_sink.as_ref(),
                    &mut attempts,
                    attempt,
                    ctx.id,
                    &credential_label,
                    Some(status),
                    if has_available { "retry" } else { "fail" },
                    Some(failure_kind.as_error_type()),
                    Some("mcp_quota_exhausted".to_string()),
                    attempt_started_at,
                    None,
                );
                if !has_available {
                    self.finish_attempt(&mut ctx);
                    return Err(Self::mcp_failure_error_with_attribution(
                        failure_kind,
                        format!(
                            "MCP 请求失败（{}，所有账号已用尽）: {}",
                            credential_context, status
                        ),
                        Some(ctx.id),
                        Some(credential_label),
                        attempts,
                    ));
                }
                last_error = Some(Self::mcp_failure_error(
                    failure_kind,
                    format!("MCP 请求失败（{}）: {}", credential_context, status),
                ));
                self.finish_attempt(&mut ctx);
                continue;
            }

            if status.as_u16() == 402 {
                let failure_kind = Self::mcp_failure_kind_for_status(status);
                Self::push_mcp_attempt(
                    attribution_sink.as_ref(),
                    &mut attempts,
                    attempt,
                    ctx.id,
                    &credential_label,
                    Some(status),
                    if attempt + 1 < max_retries {
                        "retry"
                    } else {
                        "fail"
                    },
                    Some(failure_kind.as_error_type()),
                    Some("mcp_payment_required".to_string()),
                    attempt_started_at,
                    None,
                );
                last_error = Some(Self::mcp_failure_error(
                    failure_kind,
                    format!(
                        "MCP 请求失败（{}，支付状态待确认）: {}",
                        credential_context, status
                    ),
                ));
                let retry_target_available = self.maybe_exclude_after_transient_failure(
                    None,
                    ctx.id,
                    &credential_label,
                    &mut excluded_ids,
                );
                self.finish_attempt(&mut ctx);
                if attempt + 1 < max_retries && retry_target_available {
                    continue;
                }
                if let Some(last) = attempts.last_mut() {
                    last.action = "fail".to_string();
                }
                return Err(Self::mcp_failure_error_with_attribution(
                    failure_kind,
                    last_error
                        .take()
                        .map(|error| error.to_string())
                        .unwrap_or_else(|| {
                            format!(
                                "MCP 请求失败（{}，支付状态待确认）: {}",
                                credential_context, status
                            )
                        }),
                    Some(ctx.id),
                    Some(credential_label),
                    attempts,
                ));
            }

            // 400 Bad Request
            if status.as_u16() == 400 {
                let bad_request_reason = Self::classify_bad_request_reason(&body);
                Self::push_mcp_attempt(
                    attribution_sink.as_ref(),
                    &mut attempts,
                    attempt,
                    ctx.id,
                    &credential_label,
                    Some(status),
                    "fail",
                    Some("mcp_invalid_request"),
                    Some("mcp_invalid_request".to_string()),
                    attempt_started_at,
                    None,
                );
                self.finish_attempt(&mut ctx);
                return Err(Self::mcp_failure_error_with_attribution(
                    McpCallFailureKind::InvalidRequest,
                    format!(
                        "MCP 请求失败（{}，{}）: {}",
                        credential_context,
                        Self::bad_request_reason_label(bad_request_reason),
                        status
                    ),
                    Some(ctx.id),
                    Some(credential_label),
                    attempts,
                ));
            }

            // 401/403 凭据问题
            if matches!(status.as_u16(), 401 | 403) {
                tracing::warn!(
                    credential_id = ctx.id,
                    credential_label = %credential_label,
                    "MCP 请求失败（{}，可能为账号认证错误，尝试 {}/{}）: status={}, body_bytes={}",
                    credential_context,
                    attempt + 1,
                    max_retries,
                    status,
                    response_body_bytes
                );
                let decision = match self
                    .handle_credential_auth_failure(
                        "MCP",
                        status,
                        &body,
                        "mcp_response_redacted",
                        &ctx,
                        endpoint.as_ref(),
                        &credential_label,
                        None,
                        None,
                        auxiliary_attempt_budget.clone(),
                        &mut excluded_ids,
                        &mut automatic_recovery_attempted,
                        &mut automatic_recovery_allowed,
                        attempt + 1 < max_retries,
                    )
                    .await
                {
                    Ok(decision) => decision,
                    Err(err) => {
                        Self::push_mcp_attempt(
                            attribution_sink.as_ref(),
                            &mut attempts,
                            attempt,
                            ctx.id,
                            &credential_label,
                            Some(status),
                            "fail",
                            Some("mcp_auth"),
                            Some("mcp_auth_state_write".to_string()),
                            attempt_started_at,
                            None,
                        );
                        self.finish_attempt(&mut ctx);
                        return Err(Self::mcp_failure_error_with_attribution(
                            McpCallFailureKind::Upstream,
                            format!(
                                "MCP 请求失败（{}，调度状态写入失败）: {}",
                                credential_context, err
                            ),
                            Some(ctx.id),
                            Some(credential_label),
                            attempts,
                        ));
                    }
                };
                let failure_kind = Self::mcp_failure_kind_for_status(status);
                Self::push_mcp_attempt(
                    attribution_sink.as_ref(),
                    &mut attempts,
                    attempt,
                    ctx.id,
                    &credential_label,
                    Some(status),
                    if matches!(decision, CredentialAuthFailureDecision::Exhausted) {
                        "fail"
                    } else {
                        "retry"
                    },
                    Some("mcp_auth"),
                    Some("mcp_auth_failure".to_string()),
                    attempt_started_at,
                    None,
                );
                last_error = Some(Self::mcp_failure_error(
                    failure_kind,
                    format!("MCP 请求失败（{}）: {}", credential_context, status),
                ));
                self.finish_attempt(&mut ctx);
                if matches!(decision, CredentialAuthFailureDecision::Exhausted) {
                    return Err(Self::mcp_failure_error_with_attribution(
                        failure_kind,
                        format!(
                            "MCP 请求失败（{}，所有账号已用尽）: {}",
                            credential_context, status
                        ),
                        Some(ctx.id),
                        Some(credential_label),
                        attempts,
                    ));
                }
                continue;
            }

            // 瞬态错误
            if matches!(status.as_u16(), 408 | 429) || status.is_server_error() {
                tracing::warn!(
                    credential_id = ctx.id,
                    credential_label = %credential_label,
                    "MCP 请求失败（{}，上游瞬态错误，尝试 {}/{}）: status={}, body_bytes={}",
                    credential_context,
                    attempt + 1,
                    max_retries,
                    status,
                    response_body_bytes
                );
                let failure_kind = Self::mcp_failure_kind_for_status(status);
                Self::push_mcp_attempt(
                    attribution_sink.as_ref(),
                    &mut attempts,
                    attempt,
                    ctx.id,
                    &credential_label,
                    Some(status),
                    if attempt + 1 < max_retries {
                        "retry"
                    } else {
                        "fail"
                    },
                    Some(failure_kind.as_error_type()),
                    Some(failure_kind.scheduler_reason().to_string()),
                    attempt_started_at,
                    None,
                );
                last_error = Some(Self::mcp_failure_error(
                    failure_kind,
                    format!("MCP 请求失败（{}）: {}", credential_context, status),
                ));
                let retry_target_available = self.maybe_exclude_after_transient_failure(
                    None,
                    ctx.id,
                    &credential_label,
                    &mut excluded_ids,
                );
                self.finish_attempt(&mut ctx);
                if attempt + 1 < max_retries && retry_target_available {
                    sleep(Self::retry_delay(attempt)).await;
                    continue;
                }
                if let Some(last) = attempts.last_mut() {
                    last.action = "fail".to_string();
                }
                return Err(Self::mcp_failure_error_with_attribution(
                    failure_kind,
                    last_error
                        .take()
                        .map(|error| error.to_string())
                        .unwrap_or_else(|| {
                            format!("MCP 请求失败（{}）: {}", credential_context, status)
                        }),
                    Some(ctx.id),
                    Some(credential_label),
                    attempts,
                ));
            }

            // 其他 4xx
            if status.is_client_error() {
                let failure_kind = Self::mcp_failure_kind_for_status(status);
                Self::push_mcp_attempt(
                    attribution_sink.as_ref(),
                    &mut attempts,
                    attempt,
                    ctx.id,
                    &credential_label,
                    Some(status),
                    "fail",
                    Some(failure_kind.as_error_type()),
                    Some("mcp_client_error".to_string()),
                    attempt_started_at,
                    None,
                );
                self.finish_attempt(&mut ctx);
                return Err(Self::mcp_failure_error_with_attribution(
                    failure_kind,
                    format!("MCP 请求失败（{}）: {}", credential_context, status),
                    Some(ctx.id),
                    Some(credential_label),
                    attempts,
                ));
            }

            // 兜底
            tracing::warn!(
                credential_id = ctx.id,
                credential_label = %credential_label,
                "MCP 请求失败（{}，未知错误，尝试 {}/{}）: status={}, body_bytes={}",
                credential_context,
                attempt + 1,
                max_retries,
                status,
                response_body_bytes
            );
            let failure_kind = McpCallFailureKind::Protocol;
            Self::push_mcp_attempt(
                attribution_sink.as_ref(),
                &mut attempts,
                attempt,
                ctx.id,
                &credential_label,
                Some(status),
                if attempt + 1 < max_retries {
                    "retry"
                } else {
                    "fail"
                },
                Some(failure_kind.as_error_type()),
                Some("mcp_protocol_failure".to_string()),
                attempt_started_at,
                None,
            );
            last_error = Some(Self::mcp_failure_error(
                failure_kind,
                format!("MCP 请求失败（{}）: {}", credential_context, status),
            ));
            let retry_target_available = failure_kind.should_retry_mcp_with_alternate_credential()
                && self.maybe_exclude_after_transient_failure(
                    None,
                    ctx.id,
                    &credential_label,
                    &mut excluded_ids,
                );
            self.finish_attempt(&mut ctx);
            if attempt + 1 < max_retries && retry_target_available {
                sleep(Self::retry_delay(attempt)).await;
                continue;
            }
            if let Some(last) = attempts.last_mut() {
                last.action = "fail".to_string();
            }
            return Err(Self::mcp_failure_error_with_attribution(
                failure_kind,
                last_error
                    .take()
                    .map(|error| error.to_string())
                    .unwrap_or_else(|| {
                        format!("MCP 请求失败（{}）: {}", credential_context, status)
                    }),
                Some(ctx.id),
                Some(credential_label),
                attempts,
            ));
        }

        let (kind, message) = last_error
            .map(|error| {
                (
                    Self::auxiliary_mcp_failure_kind(&error)
                        .or_else(|| Self::mcp_failure_kind_from_error(&error))
                        .unwrap_or(McpCallFailureKind::Upstream),
                    error.to_string(),
                )
            })
            .unwrap_or((
                McpCallFailureKind::AttemptLimit,
                format!("MCP 请求失败：已达到最大重试次数（{}次）", max_retries),
            ));
        Err(Self::mcp_failure_error_with_attribution(
            kind,
            message,
            last_credential_id,
            last_credential_label,
            attempts,
        ))
    }

    /// 内部方法：带重试逻辑的 API 调用
    ///
    /// 重试策略：
    /// - `credentialRetryMaxAttempts > 0` 时使用显式上限
    /// - 默认 `0` 使用 3 次固定预算，不随凭据池规模增长
    /// - 每个凭据触发瞬态错误后会进入临时冷却，后续调度优先换其他凭据
    async fn call_api_with_retry(
        &self,
        request_body: &str,
        is_stream: bool,
        request_id: Option<&str>,
        acquire_mode: AcquireMode,
        capacity_weight_units: u32,
        dispatch_model_filter: Option<&str>,
        inference_attempt_budget: Option<&InferenceAttemptBudget>,
        preserve_external_attempt: bool,
        max_sends: Option<usize>,
        thinking_signature_retry_body_builder: Option<ThinkingSignatureRetryBodyBuilder<'_>>,
    ) -> anyhow::Result<ApiCallResponse> {
        let total_credentials = self.token_manager.total_count();
        let mut max_retries =
            Self::max_retry_attempts(total_credentials, &self.token_manager.runtime_config());
        if let Some(max_sends) = max_sends {
            max_retries = max_retries.min(max_sends);
        }
        if let Some(budget) = inference_attempt_budget {
            let preserve_attempts = u32::from(preserve_external_attempt);
            max_retries = max_retries.min(budget.available_attempts(preserve_attempts) as usize);
            if max_retries == 0 {
                let snapshot = budget.snapshot();
                let rejection = if snapshot.downstream_committed {
                    InferenceAttemptRejection::DownstreamCommitted
                } else if snapshot.consumed < snapshot.max_attempts {
                    InferenceAttemptRejection::ReservedForFallback
                } else {
                    InferenceAttemptRejection::Exhausted
                };
                return Err(Self::inference_attempt_rejected_error(rejection, &[]));
            }
        }
        let mut last_error: Option<anyhow::Error> = None;
        let mut last_selection_failure: Option<SelectionFailureSummary> = None;
        let mut last_call_failure_kind: Option<KiroCallFailureKind> = None;
        let mut automatic_recovery_attempted: HashSet<u64> = HashSet::new();
        let mut automatic_recovery_allowed = true;
        let api_type = if is_stream { "流式" } else { "非流式" };
        let mut attempts: Vec<KiroCredentialAttempt> = Vec::new();

        let model = dispatch_model_filter
            .map(str::to_string)
            .or_else(|| Self::extract_model_from_request(request_body));
        let conversation_id = Self::extract_conversation_id_from_request(request_body);
        let mut excluded_ids: HashSet<u64> = HashSet::new();
        let auxiliary_attempt_budget = inference_attempt_budget
            .map(InferenceAttemptBudget::auxiliary_budget)
            .unwrap_or_else(|| {
                Arc::new(AuxiliaryAttemptBudget::new(
                    self.token_manager
                        .runtime_config()
                        .auxiliary_upstream_max_attempts,
                ))
            });
        let mut thinking_signature_retry_body_builder = thinking_signature_retry_body_builder;

        for attempt in 0..max_retries {
            // 获取调用上下文（绑定 index、credentials、token）
            let mut ctx = match self
                .token_manager
                .acquire_context_for_session_with_mode_and_auxiliary_budget(
                    model.as_deref(),
                    conversation_id.as_deref(),
                    &excluded_ids,
                    acquire_mode,
                    capacity_weight_units,
                    Some(auxiliary_attempt_budget.clone()),
                )
                .await
            {
                Ok(c) => c,
                Err(e) => {
                    let call_failure_kind = Self::auxiliary_call_failure_kind(&e);
                    let selection_failure = self.token_manager.selection_failure_summary(
                        request_id.unwrap_or_default(),
                        "local_account",
                        model.as_deref(),
                        &e.to_string(),
                    );
                    if last_error.is_none() {
                        last_selection_failure = Some(selection_failure);
                        last_call_failure_kind = call_failure_kind;
                        last_error = Some(e);
                    } else {
                        tracing::warn!(
                            error = %e,
                            "获取凭据失败，但保留之前的上游错误"
                        );
                    }
                    break;
                }
            };
            ctx.mark_in_flight_kind(if is_stream {
                InFlightKind::Stream
            } else {
                InFlightKind::Api
            });
            let credential_label = self.credential_log_label(ctx.id);
            let credential_context = format!("账号 {}", credential_label);
            let attempt_started_at = Instant::now();

            let config = self.token_manager.runtime_config();
            let machine_id = machine_id::generate_from_credentials(&ctx.credentials, &config);
            self.ensure_profile_arn_for_context(
                &mut ctx,
                &config,
                &machine_id,
                Some(auxiliary_attempt_budget.as_ref()),
            )
            .await;

            let endpoint = match self.endpoint_for(&ctx.credentials) {
                Ok(e) => e,
                Err(e) => {
                    let message = format!(
                        "{} API 凭据 endpoint 解析失败（{}）: {}",
                        api_type, credential_context, e
                    );
                    Self::push_attempt(
                        &mut attempts,
                        attempt,
                        ctx.id,
                        &credential_label,
                        None,
                        "retry",
                        Some("endpoint_error"),
                        Some(message.clone()),
                        attempt_started_at,
                        model.as_deref(),
                    );
                    last_error = Some(anyhow::anyhow!(message.clone()));
                    if let Some(session_id) = conversation_id.as_deref() {
                        self.token_manager
                            .unbind_session_if_bound_to(session_id, ctx.id);
                    }
                    tracing::warn!(
                        credential_id = ctx.id,
                        credential_label = %credential_label,
                        "凭据 endpoint 解析失败（{}），计入失败: {}",
                        credential_context,
                        e
                    );
                    self.token_manager.report_failure(ctx.id);
                    self.finish_attempt(&mut ctx);
                    continue;
                }
            };

            let rctx = RequestContext {
                credentials: &ctx.credentials,
                token: &ctx.token,
                machine_id: &machine_id,
                config: &config,
            };

            let url = endpoint.api_url(&rctx);
            let body = crate::http_client::maybe_compress_json_whitespace(
                endpoint.transform_api_body(request_body, &rctx),
                config.compression.enabled && config.compression.whitespace_compression,
            );
            if should_log_upstream_body_size_at_info(body.len(), &config) {
                tracing::info!(
                    request_id,
                    api_type,
                    endpoint = endpoint.name(),
                    credential_id = ctx.id,
                    credential_label = %credential_label,
                    attempt = attempt + 1,
                    max_retries,
                    model = model.as_deref(),
                    conversation_id = conversation_id.as_deref(),
                    upstream_body_bytes = body.len(),
                    pre_endpoint_body_bytes = request_body.len(),
                    compression_enabled = config.compression.enabled
                        && config.compression.whitespace_compression,
                    "Kiro upstream request body size"
                );
            } else {
                tracing::debug!(
                    request_id,
                    api_type,
                    endpoint = endpoint.name(),
                    credential_id = ctx.id,
                    credential_label = %credential_label,
                    attempt = attempt + 1,
                    max_retries,
                    model = model.as_deref(),
                    conversation_id = conversation_id.as_deref(),
                    upstream_body_bytes = body.len(),
                    pre_endpoint_body_bytes = request_body.len(),
                    compression_enabled = config.compression.enabled
                        && config.compression.whitespace_compression,
                    "Kiro upstream request body size"
                );
            }

            let client = match self.client_for(&ctx.credentials) {
                Ok(client) => client,
                Err(e) => {
                    let message = format!(
                        "{} API 创建 HTTP client 失败（{}）: {}",
                        api_type, credential_context, e
                    );
                    Self::push_attempt(
                        &mut attempts,
                        attempt,
                        ctx.id,
                        &credential_label,
                        None,
                        "fail",
                        Some("client_error"),
                        Some(message.clone()),
                        attempt_started_at,
                        model.as_deref(),
                    );
                    Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                    self.finish_attempt(&mut ctx);
                    return Err(Self::traced_error(message, &attempts));
                }
            };
            let base = client
                .post(&url)
                .body(body)
                .header("content-type", endpoint.content_type())
                .header("Connection", "close");
            let request = endpoint.decorate_api(base, &rctx);

            if let Some(budget) = inference_attempt_budget {
                if let Err(rejection) = budget.reserve(
                    InferenceAttemptKind::LocalCredential,
                    u32::from(preserve_external_attempt),
                ) {
                    tracing::warn!(
                        request_id,
                        credential_id = ctx.id,
                        credential_label = %credential_label,
                        rejection = ?rejection,
                        "shared inference attempt policy rejected local upstream send"
                    );
                    self.finish_attempt(&mut ctx);
                    return Err(Self::inference_attempt_rejected_error(rejection, &attempts));
                }
            }

            let response = match send_with_response_header_timeout(
                request,
                config.kiro_upstream_response_timeout_secs,
            )
            .await
            {
                Ok(resp) => resp,
                Err(e) => {
                    let failure_kind = Self::send_failure_kind(&e);
                    let message = Self::api_transport_failure_diagnostic(failure_kind);
                    tracing::warn!(
                        credential_id = ctx.id,
                        credential_label = %credential_label,
                        failure_class = failure_kind.as_error_type(),
                        attempt = attempt + 1,
                        max_retries,
                        "Kiro API upstream send failed"
                    );
                    // 网络错误通常是上游/链路瞬态问题，不禁用凭据；若本机内存态存在备选，
                    // 仅在当前请求内临时排除失败账号，避免重试链反复命中同一账号。
                    Self::push_attempt(
                        &mut attempts,
                        attempt,
                        ctx.id,
                        &credential_label,
                        None,
                        "retry",
                        Some(failure_kind.as_error_type()),
                        Some(message.clone()),
                        attempt_started_at,
                        model.as_deref(),
                    );
                    last_error = Some(anyhow::anyhow!(message.clone()));
                    if let Err(err) = self.token_manager.report_transient_failure_kind(
                        ctx.id,
                        model.as_deref(),
                        failure_kind
                            .transient_failure_kind()
                            .unwrap_or(TransientFailureKind::Network),
                        None,
                        failure_kind.scheduler_reason(),
                    ) {
                        let final_message = format!(
                            "{} API 请求发送失败（{}，调度状态写入失败）: {}",
                            api_type, credential_context, err
                        );
                        if let Some(last) = attempts.last_mut() {
                            last.action = "fail".to_string();
                            last.error_type = Some("scheduler_state_error".to_string());
                            last.error_message = Some(final_message.clone());
                        }
                        Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                        self.finish_attempt(&mut ctx);
                        return Err(Self::traced_error(final_message, &attempts));
                    }
                    self.maybe_exclude_after_soft_failure(
                        conversation_id.as_deref(),
                        model.as_deref(),
                        ctx.id,
                        &credential_label,
                        &mut excluded_ids,
                    );
                    let retry_target_available = self.maybe_exclude_after_transient_failure(
                        model.as_deref(),
                        ctx.id,
                        &credential_label,
                        &mut excluded_ids,
                    );
                    self.finish_attempt(&mut ctx);
                    if attempt + 1 < max_retries && retry_target_available {
                        sleep(Self::retry_delay(attempt)).await;
                        continue;
                    }
                    if let Some(last) = attempts.last_mut() {
                        last.action = "fail".to_string();
                    }
                    Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                    return Err(Self::traced_error(message, &attempts));
                }
            };

            let status = response.status();
            let retry_after = Self::retry_after_duration(response.headers());

            let content_kind = Self::upstream_content_kind(&response);

            // A 200 response header is not a successful inference. The body parser owns the
            // terminal success decision, so the provider records only a pending header outcome.
            //
            // The legacy IDE endpoint can return a binary AWS EventStream while labeling the
            // response as `application/json`. This is valid Kiro behavior observed in production
            // for social accounts: the body starts with an EventStream prelude and contains
            // assistantResponseEvent/contextUsageEvent/meteringEvent frames. Let the downstream
            // stream/non-stream handlers sniff the body bytes: binary frames pass through to the
            // EventStream decoder, while real JSON error envelopes are still rejected before any
            // downstream success is committed.
            if status.is_success()
                && matches!(
                    content_kind,
                    UpstreamContentKind::EventStream | UpstreamContentKind::Json
                )
            {
                return Ok(ApiCallResponse {
                    response,
                    credential_id: ctx.id,
                    in_flight_lease: ctx.take_in_flight_lease(),
                    session_id: conversation_id.clone(),
                    model: model.clone(),
                    sticky_bound: ctx.sticky_bound,
                    fallback_from_sticky: ctx.fallback_from_sticky,
                    attempts: {
                        Self::push_attempt(
                            &mut attempts,
                            attempt,
                            ctx.id,
                            &credential_label,
                            Some(status),
                            "response_headers_received",
                            None::<&str>,
                            None::<String>,
                            attempt_started_at,
                            model.as_deref(),
                        );
                        Self::log_attempt_chain(
                            request_id,
                            api_type,
                            &attempts,
                            "response_headers_received",
                        );
                        attempts
                    },
                    started_at: attempt_started_at,
                });
            }

            // Error and non-eventstream bodies are classification inputs only. They never leave
            // this scope through logs, attempt attribution, scheduler state, or provider errors.
            let upstream_body = match Self::read_upstream_body_strict(
                response,
                config.kiro_upstream_response_timeout_secs,
                PROVIDER_DIAGNOSTIC_BODY_MAX_BYTES,
            )
            .await
            {
                Ok(body) => body,
                Err(read_failure) => {
                    let message = Self::api_failure_diagnostic(
                        read_failure.kind,
                        status,
                        read_failure.body_bytes,
                        retry_after,
                        Some(content_kind),
                        None,
                    );
                    tracing::warn!(
                        credential_id = ctx.id,
                        credential_label = %credential_label,
                        failure_class = read_failure.kind.as_error_type(),
                        upstream_status = status.as_u16(),
                        body_bytes = ?read_failure.body_bytes,
                        attempt = attempt + 1,
                        max_retries,
                        "Kiro API upstream response body could not be validated"
                    );
                    let mut can_retry =
                        read_failure.kind.is_retryable() && attempt + 1 < max_retries;
                    Self::push_attempt(
                        &mut attempts,
                        attempt,
                        ctx.id,
                        &credential_label,
                        Some(status),
                        if can_retry { "retry" } else { "fail" },
                        Some(read_failure.kind.as_error_type()),
                        Some(message.clone()),
                        attempt_started_at,
                        model.as_deref(),
                    );
                    if let Some(transient_kind) = read_failure.kind.transient_failure_kind() {
                        if let Err(err) = self.token_manager.report_transient_failure_kind(
                            ctx.id,
                            model.as_deref(),
                            transient_kind,
                            retry_after,
                            read_failure.kind.scheduler_reason(),
                        ) {
                            let final_message = format!(
                                "{} API 响应读取失败（{}，调度状态写入失败）: {}",
                                api_type, credential_context, err
                            );
                            if let Some(last) = attempts.last_mut() {
                                last.action = "fail".to_string();
                                last.error_type = Some("scheduler_state_error".to_string());
                                last.error_message = Some(final_message.clone());
                            }
                            Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                            self.finish_attempt(&mut ctx);
                            return Err(Self::traced_error(final_message, &attempts));
                        }
                    }
                    self.maybe_exclude_after_soft_failure(
                        conversation_id.as_deref(),
                        model.as_deref(),
                        ctx.id,
                        &credential_label,
                        &mut excluded_ids,
                    );
                    let retry_target_available = self.maybe_exclude_after_transient_failure(
                        model.as_deref(),
                        ctx.id,
                        &credential_label,
                        &mut excluded_ids,
                    );
                    can_retry &= retry_target_available;
                    self.finish_attempt(&mut ctx);
                    if can_retry {
                        last_error = Some(anyhow::anyhow!(message));
                        sleep(Self::retry_delay(attempt)).await;
                        continue;
                    }
                    if let Some(last) = attempts.last_mut() {
                        last.action = "fail".to_string();
                    }
                    Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                    return Err(Self::traced_error(message, &attempts));
                }
            };
            let body_bytes = upstream_body.bytes;
            let body = upstream_body.text;

            if status.is_success() {
                let failure_kind = Self::classify_non_eventstream_body(&body, content_kind);
                let message = Self::api_failure_diagnostic(
                    failure_kind,
                    status,
                    Some(body_bytes),
                    retry_after,
                    Some(content_kind),
                    Some("non_eventstream"),
                );
                let mut can_retry = failure_kind.is_retryable() && attempt + 1 < max_retries;
                tracing::warn!(
                    credential_id = ctx.id,
                    credential_label = %credential_label,
                    failure_class = failure_kind.as_error_type(),
                    upstream_status = status.as_u16(),
                    body_bytes,
                    content_type = content_kind.as_str(),
                    attempt = attempt + 1,
                    max_retries,
                    "Kiro API returned a non-eventstream success response"
                );
                Self::push_attempt(
                    &mut attempts,
                    attempt,
                    ctx.id,
                    &credential_label,
                    Some(status),
                    if can_retry { "retry" } else { "fail" },
                    Some(failure_kind.as_error_type()),
                    Some(message.clone()),
                    attempt_started_at,
                    model.as_deref(),
                );
                if let Some(transient_kind) = failure_kind.transient_failure_kind() {
                    if let Err(err) = self.token_manager.report_transient_failure_kind(
                        ctx.id,
                        model.as_deref(),
                        transient_kind,
                        retry_after,
                        failure_kind.scheduler_reason(),
                    ) {
                        let final_message = format!(
                            "{} API 非 eventstream 响应（{}，调度状态写入失败）: {}",
                            api_type, credential_context, err
                        );
                        if let Some(last) = attempts.last_mut() {
                            last.action = "fail".to_string();
                            last.error_type = Some("scheduler_state_error".to_string());
                            last.error_message = Some(final_message.clone());
                        }
                        Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                        self.finish_attempt(&mut ctx);
                        return Err(Self::traced_error(final_message, &attempts));
                    }
                }
                self.finish_attempt(&mut ctx);
                if can_retry {
                    self.maybe_exclude_after_soft_failure(
                        conversation_id.as_deref(),
                        model.as_deref(),
                        ctx.id,
                        &credential_label,
                        &mut excluded_ids,
                    );
                    let retry_target_available = self.maybe_exclude_after_transient_failure(
                        model.as_deref(),
                        ctx.id,
                        &credential_label,
                        &mut excluded_ids,
                    );
                    can_retry &= retry_target_available;
                    last_error = Some(anyhow::anyhow!(message.clone()));
                    if can_retry {
                        sleep(Self::retry_delay(attempt)).await;
                        continue;
                    }
                    if let Some(last) = attempts.last_mut() {
                        last.action = "fail".to_string();
                    }
                }
                Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                return Err(Self::traced_error(message, &attempts));
            }

            if let Some(risk_reason) = Self::detect_risk_control_error(status, &body) {
                let risk_reason_token = Self::risk_control_reason_token(risk_reason);
                if Self::should_downgrade_rate_limit_risk_to_cooldown(
                    status,
                    risk_reason,
                    &ctx.credentials,
                ) {
                    let message = Self::api_failure_diagnostic(
                        ApiUpstreamFailureKind::RateLimit,
                        status,
                        Some(body_bytes),
                        retry_after,
                        Some(content_kind),
                        Some("rate_limit_risk_control"),
                    );
                    tracing::warn!(
                        credential_id = ctx.id,
                        credential_label = %credential_label,
                        risk_reason = risk_reason_token,
                        upstream_status = status.as_u16(),
                        body_bytes,
                        attempt = attempt + 1,
                        max_retries,
                        "Kiro API rate-limit risk control entered credential cooldown"
                    );
                    Self::push_attempt(
                        &mut attempts,
                        attempt,
                        ctx.id,
                        &credential_label,
                        Some(status),
                        "rate_limit_cooldown_retry",
                        Some("rate_limit_risk_control"),
                        Some(message.clone()),
                        attempt_started_at,
                        model.as_deref(),
                    );
                    last_error = Some(anyhow::anyhow!(message.clone()));
                    if let Err(err) = self.token_manager.report_transient_failure_kind(
                        ctx.id,
                        model.as_deref(),
                        TransientFailureKind::RateLimit,
                        retry_after,
                        "api_rate_limit_risk_control",
                    ) {
                        let final_message = format!(
                            "{} API 请求失败（{}，调度状态写入失败）: {}",
                            api_type, credential_context, err
                        );
                        if let Some(last) = attempts.last_mut() {
                            last.action = "fail".to_string();
                            last.error_type = Some("scheduler_state_error".to_string());
                            last.error_message = Some(final_message.clone());
                        }
                        Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                        self.finish_attempt(&mut ctx);
                        return Err(Self::traced_error(final_message, &attempts));
                    }
                    self.maybe_exclude_after_soft_failure(
                        conversation_id.as_deref(),
                        model.as_deref(),
                        ctx.id,
                        &credential_label,
                        &mut excluded_ids,
                    );
                    let retry_target_available = self.maybe_exclude_after_transient_failure(
                        model.as_deref(),
                        ctx.id,
                        &credential_label,
                        &mut excluded_ids,
                    );
                    self.finish_attempt(&mut ctx);
                    if attempt + 1 < max_retries && retry_target_available {
                        sleep(Self::retry_delay(attempt)).await;
                        continue;
                    }
                    if let Some(last) = attempts.last_mut() {
                        last.action = "fail".to_string();
                    }
                    Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                    return Err(Self::traced_error(message, &attempts));
                }

                let message = Self::api_failure_diagnostic(
                    ApiUpstreamFailureKind::RiskControl,
                    status,
                    Some(body_bytes),
                    retry_after,
                    Some(content_kind),
                    Some(risk_reason_token),
                );
                tracing::error!(
                    credential_id = ctx.id,
                    credential_label = %credential_label,
                    risk_reason = risk_reason_token,
                    upstream_status = status.as_u16(),
                    body_bytes,
                    attempt = attempt + 1,
                    max_retries,
                    "Kiro API credential entered a terminal risk-control state"
                );
                let risk_outcome = self.token_manager.report_risk_controlled_outcome(
                    ctx.id,
                    risk_reason,
                    message.clone(),
                );
                if let Some(session_id) = conversation_id.as_deref() {
                    self.token_manager
                        .unbind_session_if_bound_to(session_id, ctx.id);
                }
                if !risk_outcome.can_retry_local() {
                    let final_message = if risk_outcome.circuit_open {
                        format!(
                            "{} local_pool_risk_circuit_open=true retry_after_secs={} 本地账号池风险保护已打开",
                            message,
                            risk_outcome.retry_after_secs.unwrap_or(1)
                        )
                    } else {
                        format!("{} all_credentials_exhausted=true 所有账号已用尽", message)
                    };
                    Self::push_attempt(
                        &mut attempts,
                        attempt,
                        ctx.id,
                        &credential_label,
                        Some(status),
                        "fail",
                        Some("risk_control"),
                        Some(final_message.clone()),
                        attempt_started_at,
                        model.as_deref(),
                    );
                    Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                    self.finish_attempt(&mut ctx);
                    return if risk_outcome.circuit_open {
                        Err(Self::traced_error_with_failure_kind(
                            final_message,
                            &attempts,
                            KiroCallFailureKind::LocalPoolRiskCircuitOpen,
                        ))
                    } else {
                        Err(Self::traced_error(final_message, &attempts))
                    };
                }
                Self::push_attempt(
                    &mut attempts,
                    attempt,
                    ctx.id,
                    &credential_label,
                    Some(status),
                    "disable_and_retry",
                    Some("risk_control"),
                    Some(message.clone()),
                    attempt_started_at,
                    model.as_deref(),
                );
                last_error = Some(anyhow::anyhow!(message.clone()));
                let retry_target_available = self.maybe_exclude_after_transient_failure(
                    model.as_deref(),
                    ctx.id,
                    &credential_label,
                    &mut excluded_ids,
                );
                self.finish_attempt(&mut ctx);
                if attempt + 1 < max_retries && retry_target_available {
                    continue;
                }
                if let Some(last) = attempts.last_mut() {
                    last.action = "fail".to_string();
                }
                Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                return Err(Self::traced_error(message, &attempts));
            }

            // 402 Payment Required 且额度用尽：禁用凭据并故障转移
            if status.as_u16() == 402 && endpoint.is_monthly_request_limit(&body) {
                let message = Self::api_failure_diagnostic(
                    ApiUpstreamFailureKind::Quota,
                    status,
                    Some(body_bytes),
                    retry_after,
                    Some(content_kind),
                    None,
                );
                tracing::warn!(
                    credential_id = ctx.id,
                    credential_label = %credential_label,
                    upstream_status = status.as_u16(),
                    body_bytes,
                    attempt = attempt + 1,
                    max_retries,
                    "Kiro API credential quota is exhausted"
                );

                let has_available = self.token_manager.report_quota_exhausted(ctx.id);
                if let Some(session_id) = conversation_id.as_deref() {
                    self.token_manager
                        .unbind_session_if_bound_to(session_id, ctx.id);
                }
                if !has_available {
                    let final_message =
                        format!("{} all_credentials_exhausted=true 所有账号已用尽", message);
                    Self::push_attempt(
                        &mut attempts,
                        attempt,
                        ctx.id,
                        &credential_label,
                        Some(status),
                        "fail",
                        Some("quota_exhausted"),
                        Some(final_message.clone()),
                        attempt_started_at,
                        model.as_deref(),
                    );
                    Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                    self.finish_attempt(&mut ctx);
                    return Err(Self::traced_error(final_message, &attempts));
                }

                Self::push_attempt(
                    &mut attempts,
                    attempt,
                    ctx.id,
                    &credential_label,
                    Some(status),
                    "disable_and_retry",
                    Some("quota_exhausted"),
                    Some(message.clone()),
                    attempt_started_at,
                    model.as_deref(),
                );
                last_error = Some(anyhow::anyhow!(message.clone()));
                self.maybe_exclude_after_soft_failure(
                    conversation_id.as_deref(),
                    model.as_deref(),
                    ctx.id,
                    &credential_label,
                    &mut excluded_ids,
                );
                self.finish_attempt(&mut ctx);
                continue;
            }

            if status.as_u16() == 402 {
                let message = Self::api_failure_diagnostic(
                    ApiUpstreamFailureKind::RateLimit,
                    status,
                    Some(body_bytes),
                    retry_after,
                    Some(content_kind),
                    Some("payment_required"),
                );
                Self::push_attempt(
                    &mut attempts,
                    attempt,
                    ctx.id,
                    &credential_label,
                    Some(status),
                    "transient_retry",
                    Some("payment_required"),
                    Some(message.clone()),
                    attempt_started_at,
                    model.as_deref(),
                );
                last_error = Some(anyhow::anyhow!(message.clone()));
                if let Err(err) = self.token_manager.report_transient_failure_kind(
                    ctx.id,
                    model.as_deref(),
                    TransientFailureKind::RateLimit,
                    retry_after,
                    "api_payment_required",
                ) {
                    let final_message = format!(
                        "{} API 请求失败（{}，调度状态写入失败）: {}",
                        api_type, credential_context, err
                    );
                    Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                    self.finish_attempt(&mut ctx);
                    return Err(Self::traced_error(final_message, &attempts));
                }
                self.finish_attempt(&mut ctx);
                continue;
            }

            if Self::is_thinking_signature_invalid_response(status, &body)
                && thinking_signature_retry_body_builder.is_some()
            {
                let first_message = Self::api_failure_diagnostic(
                    ApiUpstreamFailureKind::InvalidRequest,
                    status,
                    Some(body_bytes),
                    retry_after,
                    Some(content_kind),
                    Some("THINKING_SIGNATURE_INVALID"),
                );
                Self::push_attempt(
                    &mut attempts,
                    attempt,
                    ctx.id,
                    &credential_label,
                    Some(status),
                    "thinking_signature_retry_same_credential",
                    Some("thinking_signature_invalid"),
                    Some(first_message),
                    attempt_started_at,
                    model.as_deref(),
                );

                // An explicit caller send cap remains authoritative. Do not use `max_retries`
                // here: it may have been reduced by `preserve_external_attempt`, while this
                // terminal compatibility retry deliberately reserves with `preserve=0` and
                // cannot subsequently fall back to the external pool.
                if max_sends.is_some_and(|limit| attempt.saturating_add(1) >= limit) {
                    if let Some(last) = attempts.last_mut() {
                        last.action = "thinking_signature_retry_send_limit_rejected".to_string();
                    }
                    let message = Self::thinking_signature_retry_failure_diagnostic(
                        "thinking_signature_retry_send_limit_exhausted",
                        None,
                        None,
                    );
                    Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                    self.finish_attempt(&mut ctx);
                    return Err(Self::traced_error_with_failure_kind(
                        message,
                        &attempts,
                        KiroCallFailureKind::ThinkingSignatureRetryFailed,
                    ));
                }

                if let Some(budget) = inference_attempt_budget {
                    if budget.available_attempts(0) == 0 {
                        let snapshot = budget.snapshot();
                        let rejection = if snapshot.downstream_committed {
                            InferenceAttemptRejection::DownstreamCommitted
                        } else if snapshot.consumed < snapshot.max_attempts {
                            InferenceAttemptRejection::ReservedForFallback
                        } else {
                            InferenceAttemptRejection::Exhausted
                        };
                        if let Some(last) = attempts.last_mut() {
                            last.action = "thinking_signature_retry_budget_rejected".to_string();
                        }
                        let message = Self::thinking_signature_retry_failure_diagnostic(
                            match rejection {
                                InferenceAttemptRejection::Exhausted => {
                                    "thinking_signature_retry_budget_exhausted"
                                }
                                InferenceAttemptRejection::ReservedForFallback => {
                                    "thinking_signature_retry_budget_reserved"
                                }
                                InferenceAttemptRejection::DownstreamCommitted => {
                                    "thinking_signature_retry_downstream_committed"
                                }
                            },
                            None,
                            None,
                        );
                        Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                        self.finish_attempt(&mut ctx);
                        return Err(Self::traced_error_with_failure_kind(
                            message,
                            &attempts,
                            KiroCallFailureKind::ThinkingSignatureRetryFailed,
                        ));
                    }
                }

                let retry_body_builder = match thinking_signature_retry_body_builder.take() {
                    Some(builder) => builder,
                    None => {
                        let message = Self::thinking_signature_retry_failure_diagnostic(
                            "thinking_signature_retry_builder_unavailable",
                            None,
                            None,
                        );
                        Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                        self.finish_attempt(&mut ctx);
                        return Err(Self::traced_error_with_failure_kind(
                            message,
                            &attempts,
                            KiroCallFailureKind::ThinkingSignatureRetryFailed,
                        ));
                    }
                };
                let retry_body = match retry_body_builder() {
                    Ok(body) => body,
                    Err(_) => {
                        tracing::warn!(
                            request_id,
                            credential_id = ctx.id,
                            credential_label = %credential_label,
                            "failed to build Kiro request without historical reasoningContent"
                        );
                        if let Some(last) = attempts.last_mut() {
                            last.action = "thinking_signature_retry_build_failed".to_string();
                        }
                        let message = Self::thinking_signature_retry_failure_diagnostic(
                            "thinking_signature_retry_body_build_failed",
                            None,
                            None,
                        );
                        Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                        self.finish_attempt(&mut ctx);
                        return Err(Self::traced_error_with_failure_kind(
                            message,
                            &attempts,
                            KiroCallFailureKind::ThinkingSignatureRetryFailed,
                        ));
                    }
                };

                if let Some(budget) = inference_attempt_budget {
                    if let Err(rejection) = budget.reserve(InferenceAttemptKind::LocalCredential, 0)
                    {
                        if let Some(last) = attempts.last_mut() {
                            last.action = "thinking_signature_retry_budget_rejected".to_string();
                        }
                        let message = Self::thinking_signature_retry_failure_diagnostic(
                            match rejection {
                                InferenceAttemptRejection::Exhausted => {
                                    "thinking_signature_retry_budget_exhausted"
                                }
                                InferenceAttemptRejection::ReservedForFallback => {
                                    "thinking_signature_retry_budget_reserved"
                                }
                                InferenceAttemptRejection::DownstreamCommitted => {
                                    "thinking_signature_retry_downstream_committed"
                                }
                            },
                            None,
                            None,
                        );
                        Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                        self.finish_attempt(&mut ctx);
                        return Err(Self::traced_error_with_failure_kind(
                            message,
                            &attempts,
                            KiroCallFailureKind::ThinkingSignatureRetryFailed,
                        ));
                    }
                }

                let retry_started_at = Instant::now();
                let retry_rctx = RequestContext {
                    credentials: &ctx.credentials,
                    token: &ctx.token,
                    machine_id: &machine_id,
                    config: &config,
                };
                let retry_upstream_body = crate::http_client::maybe_compress_json_whitespace(
                    endpoint.transform_api_body(&retry_body, &retry_rctx),
                    config.compression.enabled && config.compression.whitespace_compression,
                );
                let retry_base = client
                    .post(&url)
                    .body(retry_upstream_body)
                    .header("content-type", endpoint.content_type())
                    .header("Connection", "close");
                let retry_request = endpoint.decorate_api(retry_base, &retry_rctx);
                let retry_response = match send_with_response_header_timeout(
                    retry_request,
                    config.kiro_upstream_response_timeout_secs,
                )
                .await
                {
                    Ok(response) => response,
                    Err(error) => {
                        let failure_kind = Self::send_failure_kind(&error);
                        let message = Self::thinking_signature_retry_failure_diagnostic(
                            match failure_kind {
                                ApiUpstreamFailureKind::Timeout => {
                                    "thinking_signature_retry_transport_timeout"
                                }
                                _ => "thinking_signature_retry_transport_failed",
                            },
                            None,
                            None,
                        );
                        Self::push_attempt(
                            &mut attempts,
                            attempt.saturating_add(1),
                            ctx.id,
                            &credential_label,
                            None,
                            "fail",
                            Some("thinking_signature_retry_failed"),
                            Some(message.clone()),
                            retry_started_at,
                            model.as_deref(),
                        );
                        Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                        self.finish_attempt(&mut ctx);
                        return Err(Self::traced_error_with_failure_kind(
                            message,
                            &attempts,
                            KiroCallFailureKind::ThinkingSignatureRetryFailed,
                        ));
                    }
                };
                let retry_status = retry_response.status();
                let retry_after = Self::retry_after_duration(retry_response.headers());
                let retry_content_kind = Self::upstream_content_kind(&retry_response);
                // Keep the signature-retry success gate aligned with the normal provider
                // success gate above. The legacy Kiro IDE endpoint can return a binary
                // AWS EventStream while labeling the response as application/json; handlers
                // sniff the body and still reject real JSON error envelopes before committing
                // downstream success.
                if retry_status.is_success()
                    && matches!(
                        retry_content_kind,
                        UpstreamContentKind::EventStream | UpstreamContentKind::Json
                    )
                {
                    Self::push_attempt(
                        &mut attempts,
                        attempt.saturating_add(1),
                        ctx.id,
                        &credential_label,
                        Some(retry_status),
                        "response_headers_received_after_thinking_signature_retry",
                        None::<&str>,
                        None::<String>,
                        retry_started_at,
                        model.as_deref(),
                    );
                    Self::log_attempt_chain(
                        request_id,
                        api_type,
                        &attempts,
                        "response_headers_received",
                    );
                    return Ok(ApiCallResponse {
                        response: retry_response,
                        credential_id: ctx.id,
                        in_flight_lease: ctx.take_in_flight_lease(),
                        session_id: conversation_id.clone(),
                        model: model.clone(),
                        sticky_bound: ctx.sticky_bound,
                        fallback_from_sticky: ctx.fallback_from_sticky,
                        attempts,
                        started_at: attempt_started_at,
                    });
                }

                let retry_body_result = Self::read_upstream_body_strict(
                    retry_response,
                    config.kiro_upstream_response_timeout_secs,
                    PROVIDER_DIAGNOSTIC_BODY_MAX_BYTES,
                )
                .await;
                let retry_upstream_body = match retry_body_result {
                    Ok(body) => body,
                    Err(read_failure) => {
                        let message = Self::thinking_signature_retry_failure_diagnostic(
                            "thinking_signature_retry_response_read_failed",
                            Some(retry_status),
                            read_failure.body_bytes,
                        );
                        Self::push_attempt(
                            &mut attempts,
                            attempt.saturating_add(1),
                            ctx.id,
                            &credential_label,
                            Some(retry_status),
                            "fail",
                            Some("thinking_signature_retry_failed"),
                            Some(message.clone()),
                            retry_started_at,
                            model.as_deref(),
                        );
                        Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                        self.finish_attempt(&mut ctx);
                        return Err(Self::traced_error_with_failure_kind(
                            message,
                            &attempts,
                            KiroCallFailureKind::ThinkingSignatureRetryFailed,
                        ));
                    }
                };

                if Self::is_thinking_signature_invalid_response(
                    retry_status,
                    &retry_upstream_body.text,
                ) {
                    let message = Self::api_failure_diagnostic(
                        ApiUpstreamFailureKind::InvalidRequest,
                        retry_status,
                        Some(retry_upstream_body.bytes),
                        None,
                        Some(retry_content_kind),
                        Some("THINKING_SIGNATURE_INVALID"),
                    );
                    Self::push_attempt(
                        &mut attempts,
                        attempt.saturating_add(1),
                        ctx.id,
                        &credential_label,
                        Some(retry_status),
                        "fail",
                        Some("thinking_signature_invalid"),
                        Some(message.clone()),
                        retry_started_at,
                        model.as_deref(),
                    );
                    Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                    self.finish_attempt(&mut ctx);
                    return Err(Self::traced_error_with_failure_kind(
                        message,
                        &attempts,
                        KiroCallFailureKind::ThinkingSignatureInvalid,
                    ));
                }

                if matches!(retry_status.as_u16(), 408 | 429) || retry_status.is_server_error() {
                    let failure_kind = match retry_status.as_u16() {
                        408 => ApiUpstreamFailureKind::Timeout,
                        429 => ApiUpstreamFailureKind::RateLimit,
                        _ => ApiUpstreamFailureKind::Server,
                    };
                    let message = Self::api_failure_diagnostic(
                        failure_kind,
                        retry_status,
                        Some(retry_upstream_body.bytes),
                        retry_after,
                        Some(retry_content_kind),
                        Some("thinking_signature_retry"),
                    );
                    tracing::warn!(
                        credential_id = ctx.id,
                        credential_label = %credential_label,
                        failure_class = failure_kind.as_error_type(),
                        upstream_status = retry_status.as_u16(),
                        body_bytes = retry_upstream_body.bytes,
                        attempt = attempt + 2,
                        max_retries,
                        "Kiro API thinking-signature retry returned a retryable upstream status"
                    );
                    Self::push_attempt(
                        &mut attempts,
                        attempt.saturating_add(1),
                        ctx.id,
                        &credential_label,
                        Some(retry_status),
                        "fail",
                        Some(failure_kind.as_error_type()),
                        Some(message.clone()),
                        retry_started_at,
                        model.as_deref(),
                    );
                    if let Err(err) = self.token_manager.report_transient_failure_kind(
                        ctx.id,
                        model.as_deref(),
                        failure_kind
                            .transient_failure_kind()
                            .expect("retryable status has a transient failure kind"),
                        retry_after,
                        failure_kind.scheduler_reason(),
                    ) {
                        let final_message = format!(
                            "{} API thinking-signature retry 失败（{}，调度状态写入失败）: {}",
                            api_type, credential_context, err
                        );
                        if let Some(last) = attempts.last_mut() {
                            last.action = "fail".to_string();
                            last.error_type = Some("scheduler_state_error".to_string());
                            last.error_message = Some(final_message.clone());
                        }
                        Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                        self.finish_attempt(&mut ctx);
                        return Err(Self::traced_error(final_message, &attempts));
                    }
                    self.maybe_exclude_after_soft_failure(
                        conversation_id.as_deref(),
                        model.as_deref(),
                        ctx.id,
                        &credential_label,
                        &mut excluded_ids,
                    );
                    self.maybe_exclude_after_transient_failure(
                        model.as_deref(),
                        ctx.id,
                        &credential_label,
                        &mut excluded_ids,
                    );
                    Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                    self.finish_attempt(&mut ctx);
                    return Err(Self::traced_error(message, &attempts));
                }

                if retry_status.is_client_error() {
                    let message = Self::api_failure_diagnostic(
                        ApiUpstreamFailureKind::InvalidRequest,
                        retry_status,
                        Some(retry_upstream_body.bytes),
                        retry_after,
                        Some(retry_content_kind),
                        Some("thinking_signature_retry"),
                    );
                    Self::push_attempt(
                        &mut attempts,
                        attempt.saturating_add(1),
                        ctx.id,
                        &credential_label,
                        Some(retry_status),
                        "fail",
                        Some(ApiUpstreamFailureKind::InvalidRequest.as_error_type()),
                        Some(message.clone()),
                        retry_started_at,
                        model.as_deref(),
                    );
                    Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                    self.finish_attempt(&mut ctx);
                    return Err(Self::traced_error(message, &attempts));
                }

                let message = Self::thinking_signature_retry_failure_diagnostic(
                    "thinking_signature_retry_unexpected_response",
                    Some(retry_status),
                    Some(retry_upstream_body.bytes),
                );
                Self::push_attempt(
                    &mut attempts,
                    attempt.saturating_add(1),
                    ctx.id,
                    &credential_label,
                    Some(retry_status),
                    "fail",
                    Some("thinking_signature_retry_failed"),
                    Some(message.clone()),
                    retry_started_at,
                    model.as_deref(),
                );
                Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                self.finish_attempt(&mut ctx);
                return Err(Self::traced_error_with_failure_kind(
                    message,
                    &attempts,
                    KiroCallFailureKind::ThinkingSignatureRetryFailed,
                ));
            }

            // 400 Bad Request - 大多数是请求问题；模型/账号能力不匹配时允许换凭据重试。
            if status.as_u16() == 400 {
                let bad_request_reason = Self::classify_bad_request_reason(&body);
                let message = Self::api_failure_diagnostic(
                    ApiUpstreamFailureKind::InvalidRequest,
                    status,
                    Some(body_bytes),
                    retry_after,
                    Some(content_kind),
                    Some(Self::bad_request_diagnostic_reason(bad_request_reason)),
                );
                if Self::should_retry_model_unavailable_bad_request(
                    bad_request_reason,
                    model.as_deref(),
                ) && attempt + 1 < max_retries
                    && self.token_manager.has_alternate_usable_credential_cached(
                        model.as_deref(),
                        &excluded_ids,
                        ctx.id,
                    )
                {
                    tracing::warn!(
                        credential_id = ctx.id,
                        credential_label = %credential_label,
                        model = model.as_deref(),
                        upstream_status = status.as_u16(),
                        body_bytes,
                        attempt = attempt + 1,
                        max_retries,
                        "Kiro API model is unavailable for the selected credential"
                    );
                    Self::push_attempt(
                        &mut attempts,
                        attempt,
                        ctx.id,
                        &credential_label,
                        Some(status),
                        "model_unavailable_retry_next",
                        Some(bad_request_reason),
                        Some(message.clone()),
                        attempt_started_at,
                        model.as_deref(),
                    );
                    if let Err(err) = self.token_manager.report_transient_failure_kind(
                        ctx.id,
                        model.as_deref(),
                        TransientFailureKind::Protocol,
                        retry_after,
                        "api_model_unavailable",
                    ) {
                        let final_message = format!(
                            "{} API 请求失败（{}，调度状态写入失败）: {}",
                            api_type, credential_context, err
                        );
                        if let Some(last) = attempts.last_mut() {
                            last.action = "fail".to_string();
                            last.error_type = Some("scheduler_state_error".to_string());
                            last.error_message = Some(final_message.clone());
                        }
                        Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                        self.finish_attempt(&mut ctx);
                        return Err(Self::traced_error(final_message, &attempts));
                    }
                    if let Some(session_id) = conversation_id.as_deref() {
                        self.token_manager
                            .unbind_session_if_bound_to(session_id, ctx.id);
                    }
                    excluded_ids.insert(ctx.id);
                    last_error = Some(anyhow::anyhow!(message));
                    self.finish_attempt(&mut ctx);
                    continue;
                }
                if bad_request_reason == "profile_arn_bad_request" {
                    self.clear_profile_arn_discovery_state(&ctx, &config, &machine_id);
                    tracing::warn!(
                        credential_id = ctx.id,
                        credential_label = %credential_label,
                        upstream_status = status.as_u16(),
                        body_bytes,
                        attempt = attempt + 1,
                        max_retries,
                        "Kiro API rejected the persisted profile ARN"
                    );
                    Self::push_attempt(
                        &mut attempts,
                        attempt,
                        ctx.id,
                        &credential_label,
                        Some(status),
                        "profile_arn_retry",
                        Some(bad_request_reason),
                        Some(message.clone()),
                        attempt_started_at,
                        model.as_deref(),
                    );
                    if let Err(err) = self
                        .token_manager
                        .update_credential_profile_arn(ctx.id, None)
                    {
                        let final_message = format!(
                            "{} API 请求失败（{}，profileArn 状态清理失败）: {}",
                            api_type, credential_context, err
                        );
                        if let Some(last) = attempts.last_mut() {
                            last.action = "fail".to_string();
                            last.error_type = Some("scheduler_state_error".to_string());
                            last.error_message = Some(final_message.clone());
                        }
                        Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                        self.finish_attempt(&mut ctx);
                        return Err(Self::traced_error(final_message, &attempts));
                    }
                    if let Err(err) = self.token_manager.report_transient_failure_kind(
                        ctx.id,
                        model.as_deref(),
                        TransientFailureKind::Protocol,
                        None,
                        "api_profile_arn_invalid",
                    ) {
                        let final_message = format!(
                            "{} API 请求失败（{}，调度状态写入失败）: {}",
                            api_type, credential_context, err
                        );
                        if let Some(last) = attempts.last_mut() {
                            last.action = "fail".to_string();
                            last.error_type = Some("scheduler_state_error".to_string());
                            last.error_message = Some(final_message.clone());
                        }
                        Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                        self.finish_attempt(&mut ctx);
                        return Err(Self::traced_error(final_message, &attempts));
                    }
                    if let Some(session_id) = conversation_id.as_deref() {
                        self.token_manager
                            .unbind_session_if_bound_to(session_id, ctx.id);
                    }
                    if self.token_manager.has_alternate_usable_credential(
                        model.as_deref(),
                        &excluded_ids,
                        ctx.id,
                    ) {
                        excluded_ids.insert(ctx.id);
                    }
                    last_error = Some(anyhow::anyhow!(message));
                    self.finish_attempt(&mut ctx);
                    continue;
                }
                Self::push_attempt(
                    &mut attempts,
                    attempt,
                    ctx.id,
                    &credential_label,
                    Some(status),
                    "fail",
                    Some(bad_request_reason),
                    Some(message.clone()),
                    attempt_started_at,
                    model.as_deref(),
                );
                Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                self.finish_attempt(&mut ctx);
                return Err(Self::traced_error(message, &attempts));
            }

            // 401/403 - 更可能是凭据/权限问题：计入失败并允许故障转移
            if matches!(status.as_u16(), 401 | 403) {
                let message = Self::api_failure_diagnostic(
                    ApiUpstreamFailureKind::Auth,
                    status,
                    Some(body_bytes),
                    retry_after,
                    Some(content_kind),
                    None,
                );
                tracing::warn!(
                    credential_id = ctx.id,
                    credential_label = %credential_label,
                    upstream_status = status.as_u16(),
                    body_bytes,
                    attempt = attempt + 1,
                    max_retries,
                    "Kiro API credential authentication failed"
                );
                let decision = match self
                    .handle_credential_auth_failure(
                        api_type,
                        status,
                        &body,
                        "api_auth_error",
                        &ctx,
                        endpoint.as_ref(),
                        &credential_label,
                        model.as_deref(),
                        conversation_id.as_deref(),
                        auxiliary_attempt_budget.clone(),
                        &mut excluded_ids,
                        &mut automatic_recovery_attempted,
                        &mut automatic_recovery_allowed,
                        attempt + 1 < max_retries,
                    )
                    .await
                {
                    Ok(decision) => decision,
                    Err(err) => {
                        let final_message = format!(
                            "{} API 请求失败（{}，调度状态写入失败）: {}",
                            api_type, credential_context, err
                        );
                        Self::push_attempt(
                            &mut attempts,
                            attempt,
                            ctx.id,
                            &credential_label,
                            Some(status),
                            "fail",
                            Some("scheduler_state_error"),
                            Some(final_message.clone()),
                            attempt_started_at,
                            model.as_deref(),
                        );
                        Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                        self.finish_attempt(&mut ctx);
                        return Err(Self::traced_error(final_message, &attempts));
                    }
                };

                if matches!(decision, CredentialAuthFailureDecision::TokenRecoveryRetry) {
                    Self::push_attempt(
                        &mut attempts,
                        attempt,
                        ctx.id,
                        &credential_label,
                        Some(status),
                        "automatic_token_recovery_and_retry",
                        Some("auth_error"),
                        Some(message.clone()),
                        attempt_started_at,
                        model.as_deref(),
                    );
                    self.finish_attempt(&mut ctx);
                    continue;
                }

                if matches!(decision, CredentialAuthFailureDecision::Exhausted) {
                    let final_message =
                        format!("{} all_credentials_exhausted=true 所有账号已用尽", message);
                    Self::push_attempt(
                        &mut attempts,
                        attempt,
                        ctx.id,
                        &credential_label,
                        Some(status),
                        "fail",
                        Some("credential_failure"),
                        Some(final_message.clone()),
                        attempt_started_at,
                        model.as_deref(),
                    );
                    Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                    self.finish_attempt(&mut ctx);
                    return Err(Self::traced_error(final_message, &attempts));
                }

                let action = match decision {
                    CredentialAuthFailureDecision::Retry {
                        excluded_current: true,
                    } => "failure_count_exclude_and_retry",
                    CredentialAuthFailureDecision::Retry {
                        excluded_current: false,
                    } => "failure_count_and_retry",
                    CredentialAuthFailureDecision::TokenRecoveryRetry
                    | CredentialAuthFailureDecision::Exhausted => unreachable!(),
                };

                Self::push_attempt(
                    &mut attempts,
                    attempt,
                    ctx.id,
                    &credential_label,
                    Some(status),
                    action,
                    Some("credential_failure"),
                    Some(message.clone()),
                    attempt_started_at,
                    model.as_deref(),
                );
                last_error = Some(anyhow::anyhow!(message.clone()));
                self.finish_attempt(&mut ctx);
                continue;
            }

            // 429/408/5xx - 瞬态上游错误：不禁用凭据；若本机内存态存在备选，
            // 仅在当前请求内临时排除失败账号，避免重试链反复命中同一账号。
            if matches!(status.as_u16(), 408 | 429) || status.is_server_error() {
                let failure_kind = match status.as_u16() {
                    408 => ApiUpstreamFailureKind::Timeout,
                    429 => ApiUpstreamFailureKind::RateLimit,
                    _ => ApiUpstreamFailureKind::Server,
                };
                let message = Self::api_failure_diagnostic(
                    failure_kind,
                    status,
                    Some(body_bytes),
                    retry_after,
                    Some(content_kind),
                    None,
                );
                tracing::warn!(
                    credential_id = ctx.id,
                    credential_label = %credential_label,
                    failure_class = failure_kind.as_error_type(),
                    upstream_status = status.as_u16(),
                    body_bytes,
                    attempt = attempt + 1,
                    max_retries,
                    "Kiro API returned a retryable upstream status"
                );
                let mut can_retry = attempt + 1 < max_retries;
                Self::push_attempt(
                    &mut attempts,
                    attempt,
                    ctx.id,
                    &credential_label,
                    Some(status),
                    if can_retry { "transient_retry" } else { "fail" },
                    Some(failure_kind.as_error_type()),
                    Some(message.clone()),
                    attempt_started_at,
                    model.as_deref(),
                );
                last_error = Some(anyhow::anyhow!(message.clone()));
                if let Err(err) = self.token_manager.report_transient_failure_kind(
                    ctx.id,
                    model.as_deref(),
                    failure_kind
                        .transient_failure_kind()
                        .expect("retryable status has a transient failure kind"),
                    retry_after,
                    failure_kind.scheduler_reason(),
                ) {
                    let final_message = format!(
                        "{} API 请求失败（{}，调度状态写入失败）: {}",
                        api_type, credential_context, err
                    );
                    if let Some(last) = attempts.last_mut() {
                        last.action = "fail".to_string();
                        last.error_type = Some("scheduler_state_error".to_string());
                        last.error_message = Some(final_message.clone());
                    }
                    Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                    self.finish_attempt(&mut ctx);
                    return Err(Self::traced_error(final_message, &attempts));
                }
                self.maybe_exclude_after_soft_failure(
                    conversation_id.as_deref(),
                    model.as_deref(),
                    ctx.id,
                    &credential_label,
                    &mut excluded_ids,
                );
                let retry_target_available = self.maybe_exclude_after_transient_failure(
                    model.as_deref(),
                    ctx.id,
                    &credential_label,
                    &mut excluded_ids,
                );
                can_retry &= retry_target_available;
                self.finish_attempt(&mut ctx);
                if can_retry {
                    sleep(Self::retry_delay(attempt)).await;
                    continue;
                }
                if let Some(last) = attempts.last_mut() {
                    last.action = "fail".to_string();
                }
                Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                return Err(Self::traced_error(message, &attempts));
            }

            // 其他 4xx - 通常为请求/配置问题：直接返回，不计入凭据失败
            if status.is_client_error() {
                let message = Self::api_failure_diagnostic(
                    ApiUpstreamFailureKind::InvalidRequest,
                    status,
                    Some(body_bytes),
                    retry_after,
                    Some(content_kind),
                    None,
                );
                Self::push_attempt(
                    &mut attempts,
                    attempt,
                    ctx.id,
                    &credential_label,
                    Some(status),
                    "fail",
                    Some(ApiUpstreamFailureKind::InvalidRequest.as_error_type()),
                    Some(message.clone()),
                    attempt_started_at,
                    model.as_deref(),
                );
                Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                self.finish_attempt(&mut ctx);
                return Err(Self::traced_error(message, &attempts));
            }

            // Unknown statuses fail closed. Retrying an unclassified response on another account
            // can replay a deterministic request error and amplify internal RPM.
            let message = Self::api_failure_diagnostic(
                ApiUpstreamFailureKind::Unknown,
                status,
                Some(body_bytes),
                retry_after,
                Some(content_kind),
                None,
            );
            tracing::warn!(
                credential_id = ctx.id,
                credential_label = %credential_label,
                upstream_status = status.as_u16(),
                body_bytes,
                attempt = attempt + 1,
                max_retries,
                "Kiro API returned an unclassified upstream status"
            );
            Self::push_attempt(
                &mut attempts,
                attempt,
                ctx.id,
                &credential_label,
                Some(status),
                "fail",
                Some(ApiUpstreamFailureKind::Unknown.as_error_type()),
                Some(message.clone()),
                attempt_started_at,
                model.as_deref(),
            );
            Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
            self.finish_attempt(&mut ctx);
            return Err(Self::traced_error(message, &attempts));
        }

        // 所有重试都失败
        Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
        let message = last_error.map(|err| err.to_string()).unwrap_or_else(|| {
            format!(
                "{} API 请求失败：已达到最大重试次数（{}次）",
                api_type, max_retries
            )
        });
        Err(Self::traced_error_with_selection_failure(
            message,
            &attempts,
            last_selection_failure,
            last_call_failure_kind,
        ))
    }

    /// 从请求体中提取模型信息
    ///
    /// 尝试解析 JSON 请求体，提取 conversationState.currentMessage.userInputMessage.modelId
    fn extract_model_from_request(request_body: &str) -> Option<String> {
        use serde_json::Value;

        let json: Value = serde_json::from_str(request_body).ok()?;

        json.get("conversationState")?
            .get("currentMessage")?
            .get("userInputMessage")?
            .get("modelId")?
            .as_str()
            .map(|s| s.to_string())
    }

    /// 从请求体中提取 Kiro conversationId，用于账号粘性调度。
    fn extract_conversation_id_from_request(request_body: &str) -> Option<String> {
        use serde_json::Value;

        let json: Value = serde_json::from_str(request_body).ok()?;

        json.get("conversationState")?
            .get("conversationId")?
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    }

    #[cfg(test)]
    pub(crate) fn test_extract_model_from_request(request_body: &str) -> Option<String> {
        Self::extract_model_from_request(request_body)
    }

    #[cfg(test)]
    pub(crate) fn test_extract_conversation_id_from_request(request_body: &str) -> Option<String> {
        Self::extract_conversation_id_from_request(request_body)
    }

    fn max_retry_attempts(_total_credentials: usize, config: &Config) -> usize {
        let configured = config.credential_retry_max_attempts as usize;
        if configured > 0 {
            return configured.max(1);
        }

        DEFAULT_AUTO_RETRY_ATTEMPTS
    }

    #[cfg(test)]
    pub(crate) fn test_max_retry_attempts(total_credentials: usize, config: &Config) -> usize {
        Self::max_retry_attempts(total_credentials, config)
    }

    fn retry_delay(attempt: usize) -> Duration {
        // 指数退避 + 少量抖动，避免上游抖动时放大故障
        const BASE_MS: u64 = 200;
        const MAX_MS: u64 = 2_000;
        let exp = BASE_MS.saturating_mul(2u64.saturating_pow(attempt.min(6) as u32));
        let backoff = exp.min(MAX_MS);
        let jitter_max = (backoff / 4).max(1);
        let jitter = fastrand::u64(0..=jitter_max);
        Duration::from_millis(backoff.saturating_add(jitter))
    }

    fn retry_after_duration(headers: &HeaderMap) -> Option<Duration> {
        let value = headers.get("retry-after")?.to_str().ok()?.trim();
        if value.is_empty() {
            return None;
        }

        if let Ok(seconds) = value.parse::<u64>() {
            return Some(Duration::from_secs(seconds));
        }

        let retry_at = chrono::DateTime::parse_from_rfc2822(value)
            .ok()
            .map(|dt| dt.with_timezone(&Utc))?;
        let seconds = retry_at.signed_duration_since(Utc::now()).num_seconds();
        if seconds <= 0 {
            Some(Duration::from_secs(1))
        } else {
            Some(Duration::from_secs(seconds as u64))
        }
    }

    fn should_downgrade_rate_limit_risk_to_cooldown(
        status: reqwest::StatusCode,
        reason: CredentialRiskControlReason,
        credentials: &KiroCredentials,
    ) -> bool {
        status == reqwest::StatusCode::TOO_MANY_REQUESTS
            && reason == CredentialRiskControlReason::TemporarilySuspended
            && !credentials.rate_limit_auto_disable_enabled()
    }

    fn detect_risk_control_error(
        status: reqwest::StatusCode,
        body: &str,
    ) -> Option<CredentialRiskControlReason> {
        let lower = body.to_ascii_lowercase();

        if status.as_u16() == 423 {
            return Some(CredentialRiskControlReason::AccountLocked);
        }

        if body.contains("TEMPORARILY_SUSPENDED")
            || lower.contains("temporarily suspended")
            || lower.contains("temporary suspended")
            || lower.contains("temporarily is suspended")
            || lower.contains("is temporarily suspended")
            || (status.as_u16() == 429
                && lower.contains("suspicious activity")
                && lower.contains("temporary limits"))
        {
            return Some(CredentialRiskControlReason::TemporarilySuspended);
        }

        if body.contains("PERMANENTLY_SUSPENDED")
            || body.contains("ACCOUNT_SUSPENDED")
            || body.contains("AccountSuspendedException")
            || lower.contains("account suspended")
            || lower.contains("permanently suspended")
            || lower.contains("user is suspended")
            || lower.contains("user id is suspended")
        {
            return Some(CredentialRiskControlReason::AccountSuspended);
        }

        if lower.contains("account locked")
            || lower.contains("user locked")
            || lower.contains("locked account")
            || lower.contains("locked your account")
        {
            return Some(CredentialRiskControlReason::AccountLocked);
        }

        let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
            return None;
        };
        for pointer in [
            "/reason",
            "/error/reason",
            "/__type",
            "/code",
            "/error/code",
            "/exceptionType",
            "/error/exceptionType",
        ] {
            let Some(text) = value.pointer(pointer).and_then(|v| v.as_str()) else {
                continue;
            };
            match text {
                "TEMPORARILY_SUSPENDED" => {
                    return Some(CredentialRiskControlReason::TemporarilySuspended);
                }
                "ACCOUNT_SUSPENDED" | "PERMANENTLY_SUSPENDED" | "AccountSuspendedException" => {
                    return Some(CredentialRiskControlReason::AccountSuspended);
                }
                "ACCOUNT_LOCKED" | "LOCKED" | "AccountLockedException" => {
                    return Some(CredentialRiskControlReason::AccountLocked);
                }
                _ => {}
            }
        }

        None
    }

    fn classify_bad_request_reason(body: &str) -> &'static str {
        let lower = body.to_ascii_lowercase();
        if lower.contains("content_length_exceeds_threshold")
            || lower.contains("input is too long")
            || lower.contains("prompt is too long")
            || lower.contains("payload is too large")
            || lower.contains("request payload is too large")
            || lower.contains("request body is too large")
            || lower.contains("content length exceeded")
            || lower.contains("content length exceeds")
        {
            return "content_length_exceeds_threshold";
        }
        if lower.contains("context window is full") {
            return "context_window_full_bad_request";
        }
        if lower.contains("assistant-prefill")
            || lower.contains("assistant prefill")
            || lower.contains("last message must be user")
        {
            return "assistant_prefill_bad_request";
        }
        if lower.contains("profilearn")
            || lower.contains("profile arn")
            || lower.contains("profile_arn")
        {
            return "profile_arn_bad_request";
        }
        if Self::bad_request_body_indicates_retryable_model_unavailable(&lower) {
            return "model_unavailable_bad_request";
        }
        if Self::bad_request_body_indicates_invalid_model(&lower) {
            return "model_invalid_bad_request";
        }
        // Image failures can mention tool-result images, so classify them before tool protocol
        // failures. Neither image failures nor a generic REQUEST_BODY_INVALID is account-specific.
        if Self::bad_request_body_indicates_invalid_image(&lower) {
            return "image_invalid_bad_request";
        }
        if Self::bad_request_body_indicates_tool_protocol_error(&lower) {
            return "tool_use_format_bad_request";
        }
        if lower.contains("improperly formed")
            || lower.contains("malformed")
            || lower.contains("invalid request body")
            || lower.contains("invalid json")
            || lower.contains("json is invalid")
            || lower.contains("json parse error")
        {
            return "malformed_request";
        }
        if lower.contains("request_body_invalid")
            || lower.contains("request body invalid")
            || lower.contains("request body is invalid")
        {
            return "request_body_invalid_bad_request";
        }
        "bad_request"
    }

    fn is_thinking_signature_invalid_response(status: reqwest::StatusCode, body: &str) -> bool {
        if status != reqwest::StatusCode::BAD_REQUEST {
            return false;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
            return false;
        };
        ["/reason", "/error/reason"].into_iter().any(|pointer| {
            value.pointer(pointer).and_then(serde_json::Value::as_str)
                == Some("THINKING_SIGNATURE_INVALID")
        })
    }

    fn thinking_signature_retry_failure_diagnostic(
        reason: &'static str,
        upstream_status: Option<reqwest::StatusCode>,
        body_bytes: Option<usize>,
    ) -> String {
        format!(
            "upstream_failure class=thinking_signature_retry_failed upstream_status={} public_status=502 body_bytes={} retry_after_secs=unknown content_type=unknown reason={}",
            upstream_status
                .map(|status| status.as_u16().to_string())
                .unwrap_or_else(|| "none".to_string()),
            body_bytes
                .map(|bytes| bytes.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            reason,
        )
    }

    fn bad_request_body_indicates_retryable_model_unavailable(lower_body: &str) -> bool {
        [
            "requested model is not available",
            "model is not available for this endpoint",
            "model is not available in this region",
            "model is unavailable in this region",
            "model is not supported in this region",
            "model is not available for this account",
            "model is not enabled for this account",
            "\"model_unavailable\"",
            "\"model_not_available\"",
        ]
        .iter()
        .any(|needle| lower_body.contains(needle))
    }

    fn bad_request_body_indicates_invalid_model(lower_body: &str) -> bool {
        [
            "invalid model",
            "invalid_model_id",
            "invalid_model",
            "model not found",
            "model_not_found",
            "unsupported model",
            "unknown model",
        ]
        .iter()
        .any(|needle| lower_body.contains(needle))
    }

    fn bad_request_body_indicates_invalid_image(lower_body: &str) -> bool {
        [
            "image_format_unsupported",
            "image format unsupported",
            "unsupported image format",
            "image data cannot be empty",
            "image cannot be empty",
            "image source cannot be empty",
            "could not process image",
            "unable to process image",
            "invalid image",
            "image data is invalid",
            "image source is invalid",
            "invalid base64 image",
            "image_too_large",
            "image is too large",
        ]
        .iter()
        .any(|needle| lower_body.contains(needle))
    }

    fn bad_request_body_indicates_tool_protocol_error(lower_body: &str) -> bool {
        [
            "invalid tool use format",
            "invalid tool-use format",
            "invalid_tool_use_format",
            "tool use format is invalid",
            "tool-use format is invalid",
            "tool_use_format_invalid",
            "invalid tool schema",
            "tool schema is invalid",
            "tool_schema_invalid",
            "tool input schema",
            "tool input_schema",
            "invalid tool-use sequence",
            "invalid tool use sequence",
            "tool_use_invalid",
        ]
        .iter()
        .any(|needle| lower_body.contains(needle))
            || (lower_body.contains("tools.") && lower_body.contains("input_schema"))
    }

    fn should_retry_model_unavailable_bad_request(reason: &str, model: Option<&str>) -> bool {
        reason == "model_unavailable_bad_request"
            && model.map(str::trim).is_some_and(|value| !value.is_empty())
    }

    fn bad_request_reason_label(reason: &str) -> &'static str {
        match reason {
            "model_unavailable_bad_request" | "model_invalid_bad_request" => "模型不可用",
            "assistant_prefill_bad_request"
            | "profile_arn_bad_request"
            | "tool_use_format_bad_request"
            | "image_invalid_bad_request"
            | "request_body_invalid_bad_request"
            | "content_length_exceeds_threshold"
            | "context_window_full_bad_request"
            | "malformed_request"
            | "bad_request" => "请求无效",
            _ => "请求无效",
        }
    }

    fn is_event_stream_content_type(content_type: &str) -> bool {
        content_type
            .split(';')
            .next()
            .map(|media_type| {
                let media_type = media_type.trim().to_ascii_lowercase();
                media_type == "application/vnd.amazon.eventstream"
                    || media_type == "application/octet-stream"
            })
            .unwrap_or(false)
    }
}
