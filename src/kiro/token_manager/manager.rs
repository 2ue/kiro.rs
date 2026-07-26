//! Token 管理模块
//!
//! 负责 Token 过期检测和刷新，支持 Social 和 IdC 认证方式
//! 支持多凭据 (MultiTokenManager) 管理

use chrono::Utc;
use futures::FutureExt;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex as TokioMutex, OwnedSemaphorePermit, Semaphore, oneshot};
use tokio::task::JoinHandle;

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::error::Error as StdError;
use std::fmt;
use std::future::Future;
use std::panic::AssertUnwindSafe;
#[cfg(test)]
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration as StdDuration, Instant};

use crate::anthropic::inference_attempt_budget::{
    AuxiliaryAttemptBudget, AuxiliaryAttemptBudgetExhausted, AuxiliaryAttemptKind,
};
use crate::common::capacity_signal::{CapacitySignal, CapacityWaiter};
use crate::http_client::ProxyConfig;
use crate::kiro::call_trace::{
    AccountRejectReason, RejectedAccountSample, SelectionFailureStage, SelectionFailureSummary,
};
use crate::kiro::machine_id;
use crate::kiro::model::available_models::{
    KiroModelCapabilityCohort, KiroModelCapabilityCohortKey,
};
use crate::kiro::model::credentials::{KiroCredentials, profile_arn_region};
use crate::kiro::model::usage_limits::UsageLimitsResponse;
use crate::model::config::{
    Config, MAX_TOKEN_REFRESH_BURST, MAX_TOKEN_REFRESH_MAX_RPM, MIN_TOKEN_REFRESH_BURST,
    MIN_TOKEN_REFRESH_MAX_RPM,
};
use crate::storage::postgres::{
    CredentialRefreshExpectedContext, CredentialRefreshFieldsCasOutcome,
    CredentialRefreshFieldsPatch, CredentialRuntimeDisabledReasonPatch,
    CredentialRuntimeFailureCounts, CredentialRuntimeStateMutationResult,
    CredentialRuntimeStatePatch, CredentialRuntimeStateRow, CredentialStatsDeltaRow,
    CredentialUpsertCasOutcome, CredentialWithRuntimePatchCasOutcome, PostgresStore,
};
use crate::storage::redis_cache::{
    RedisRefreshBegin, RedisRefreshFailure, RedisRefreshFailureKind, RedisRefreshFailureStage,
    RedisRefreshHealthClaim, RedisRefreshLease, RedisStore, SchedulerCredentialState,
    SchedulerGlobalCapacityState, SchedulerHealthState, SchedulerSelectionReservation,
    SchedulerSessionBinding,
};

use super::account_state::{
    CredentialEntry, CredentialModelCooldown, CredentialRiskControlReason, DisabledReason,
    InFlightLease, ProxyResourceAvailability, ProxyResourceRuntime, SessionBinding,
};
use super::admin_snapshot::{
    ManagerBaseSnapshot, ManagerRuntimeSnapshot, ManagerSnapshot, ManagerSummarySnapshot,
    base_snapshot_from_entry, runtime_snapshot_from_entry,
};
use super::auxiliary::{
    AuxiliaryConcurrencyController, AuxiliaryConcurrencySaturated, AuxiliaryConcurrencySnapshot,
    AuxiliaryRuntime, RefreshClientCacheSnapshot, TokenRefreshAdmissionRejected,
    TokenRefreshAdmissionSnapshot,
};
use super::capacity::{
    credential_is_dispatch_candidate, credential_is_dispatchable,
    credential_is_temporarily_available, credential_is_usable_for_model,
    credential_proxy_availability, credential_proxy_is_dispatchable,
    effective_max_concurrent_requests, effective_weight_for_limit, entry_has_concurrency_capacity,
    global_has_concurrency_capacity, is_opus_model, normalize_capacity_weight_units,
    proxy_unavailable_error,
};
#[cfg(test)]
use super::concurrency::record_released_in_flight_lease_tombstone;
use super::concurrency::{
    DispatchQueueGuard, InFlightLeaseGuard, ReleasedInFlightLeaseTombstones,
    SchedulerRedisReleaseDispatcher, filter_released_in_flight_leases_from_scheduler_states,
};
use super::cooldown::{entry_any_cooldown_remaining, entry_cooldown_remaining, model_state_key};
use super::queue::{
    concurrency_blocked_count, effective_concurrency_range_for_candidates,
    format_effective_concurrency_range, min_dispatch_wait, rate_limit_blocked_count,
};
use super::redis_runtime::{
    apply_scheduler_states as apply_redis_scheduler_states,
    apply_scheduler_states_for_ids as apply_redis_scheduler_states_for_ids,
    apply_scheduler_states_for_ids_with_global_rpm as apply_redis_scheduler_states_for_ids_with_global_rpm,
    apply_scheduler_states_with_global_rpm as apply_redis_scheduler_states_with_global_rpm,
    clear_scheduler_state_for_credential_local, clear_scheduler_state_for_credential_redis,
    publish_credentials_changed as publish_redis_credentials_changed,
    publish_runtime_config_changed as publish_redis_runtime_config_changed,
};
use super::refresh::{
    RefreshFailure, RefreshFailureKind, RefreshFailureStage, RefreshSendAdmission,
    get_usage_limits, is_token_expired, refresh_token_with_client, set_overage_status,
    validate_refresh_token,
};
use super::route_state::{LocalPoolRouteState, LocalPoolRouteStateKind};
use super::rpm::{
    effective_rpm, entry_rate_limit_remaining, entry_rate_limit_window_remaining,
    rate_limit_interval_for_rpm,
};
use super::sticky::{
    MAX_SESSION_SOFT_FAILURES, SESSION_BINDING_TTL_SECS,
    bind_session_to_credential as bind_sticky_session_to_credential,
    bound_credential_id as sticky_bound_credential_id,
    cache_redis_binding as cache_sticky_redis_binding,
    clear_session_soft_failure as clear_sticky_session_soft_failure,
    record_session_soft_failure as record_sticky_session_soft_failure,
    unbind_session_if_bound_to as unbind_sticky_session_if_bound_to,
    unbind_sessions_for_credential as unbind_sticky_sessions_for_credential,
};
use super::storage_task::{
    block_on_storage, spawn_best_effort_storage_task, spawn_critical_storage_task,
};
use super::strategy::{
    balanced_selection_key, entry_effective_health_mut, priority_selection_key,
    record_local_selection, refresh_local_selection_windows_locked, select_health_weighted,
    select_weighted_least_inflight, should_select_warming_from_totals,
};
use super::types::{
    AcquireMode, CallContext, CredentialAuthUpdate, EXTERNAL_CREDENTIAL_CONTEXT_ID, InFlightKind,
    TransientFailureKind,
};

#[cfg(test)]
use super::refresh::{
    is_invalid_grant_response, usage_limits_amz_user_agent, usage_limits_user_agent,
};
#[cfg(test)]
use super::strategy::scheduler_score_with_config;
#[cfg(test)]
use chrono::Duration;

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    format!("{:x}", result)
}

fn validate_token_refresh_admission_config(config: &Config) -> anyhow::Result<()> {
    if !(MIN_TOKEN_REFRESH_MAX_RPM..=MAX_TOKEN_REFRESH_MAX_RPM)
        .contains(&config.token_refresh_max_rpm)
    {
        anyhow::bail!(
            "tokenRefreshMaxRpm must be between {} and {}",
            MIN_TOKEN_REFRESH_MAX_RPM,
            MAX_TOKEN_REFRESH_MAX_RPM
        );
    }
    if !(MIN_TOKEN_REFRESH_BURST..=MAX_TOKEN_REFRESH_BURST).contains(&config.token_refresh_burst) {
        anyhow::bail!(
            "tokenRefreshBurst must be between {} and {}",
            MIN_TOKEN_REFRESH_BURST,
            MAX_TOKEN_REFRESH_BURST
        );
    }
    Ok(())
}

/// 生成 API Key 脱敏展示(前 4 + ... + 后 4,长度不足或非 ASCII 回退 ***)
fn mask_api_key(key: &str) -> String {
    if key.is_ascii() && key.len() > 16 {
        format!("{}...{}", &key[..4], &key[key.len() - 4..])
    } else {
        "***".to_string()
    }
}

fn trimmed_optional(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn apply_optional_string(target: &mut Option<String>, value: Option<String>) {
    if let Some(value) = value {
        *target = trimmed_optional(value);
    }
}

fn api_region_conflicts_with_profile_arn(credential: &KiroCredentials) -> bool {
    let Some(profile_region) = credential
        .profile_arn
        .as_deref()
        .and_then(profile_arn_region)
    else {
        return false;
    };

    let Some(api_region) = credential
        .api_region
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return false;
    };

    !api_region.eq_ignore_ascii_case(profile_region)
}

fn apply_credential_auth_update(credential: &mut KiroCredentials, update: CredentialAuthUpdate) {
    let mut clear_access_token = false;
    let explicit_access_token = update.access_token;
    let explicit_expires_at = update.expires_at;

    if let Some(api_key) = update.kiro_api_key {
        credential.kiro_api_key = trimmed_optional(api_key);
        credential.refresh_token = None;
        credential.provider = None;
        credential.client_id = None;
        credential.client_secret = None;
        credential.token_endpoint = None;
        credential.issuer_url = None;
        credential.scopes = None;
        credential.auth_method = Some("api_key".to_string());
        clear_access_token = true;
    }

    if let Some(refresh_token) = update.refresh_token {
        credential.refresh_token = trimmed_optional(refresh_token);
        credential.kiro_api_key = None;
        if credential
            .auth_method
            .as_deref()
            .is_none_or(|method| method.eq_ignore_ascii_case("api_key"))
        {
            credential.auth_method = Some("social".to_string());
        }
        clear_access_token = true;
    }

    if let Some(auth_method) = update.auth_method.and_then(trimmed_optional) {
        credential.auth_method = Some(auth_method);
        clear_access_token = true;
    }
    if update.provider.is_some() {
        apply_optional_string(&mut credential.provider, update.provider);
    }
    if update.client_id.is_some() {
        apply_optional_string(&mut credential.client_id, update.client_id);
        clear_access_token = true;
    }
    if update.client_secret.is_some() {
        apply_optional_string(&mut credential.client_secret, update.client_secret);
        clear_access_token = true;
    }
    if update.token_endpoint.is_some() {
        apply_optional_string(&mut credential.token_endpoint, update.token_endpoint);
        clear_access_token = true;
    }
    if update.issuer_url.is_some() {
        apply_optional_string(&mut credential.issuer_url, update.issuer_url);
    }
    if update.scopes.is_some() {
        apply_optional_string(&mut credential.scopes, update.scopes);
        clear_access_token = true;
    }
    if update.region.is_some() {
        apply_optional_string(&mut credential.region, update.region);
        clear_access_token = true;
    }
    if update.auth_region.is_some() {
        apply_optional_string(&mut credential.auth_region, update.auth_region);
        clear_access_token = true;
    }
    apply_optional_string(&mut credential.api_region, update.api_region);
    apply_optional_string(&mut credential.machine_id, update.machine_id);
    apply_optional_string(&mut credential.email, update.email);
    apply_optional_string(&mut credential.endpoint, update.endpoint);

    if clear_access_token {
        credential.access_token = None;
        credential.expires_at = None;
        credential.profile_arn = None;
        credential.subscription_title = None;
    }
    if explicit_access_token.is_some() {
        apply_optional_string(&mut credential.access_token, explicit_access_token);
    }
    if explicit_expires_at.is_some() {
        apply_optional_string(&mut credential.expires_at, explicit_expires_at);
    }
    credential.normalize_api_key_defaults();
    credential.normalize_external_idp_defaults();
}

// ============================================================================
// 多凭据 Token 管理器
// ============================================================================

fn truncate_for_audit(value: &str, max_chars: usize) -> String {
    let mut truncated = value.chars().take(max_chars).collect::<String>();
    if value.chars().count() > max_chars {
        truncated.push_str("...");
    }
    truncated
}

fn rfc3339_is_after(candidate: &str, existing: &str) -> bool {
    match (
        chrono::DateTime::parse_from_rfc3339(candidate),
        chrono::DateTime::parse_from_rfc3339(existing),
    ) {
        (Ok(candidate), Ok(existing)) => candidate > existing,
        _ => candidate > existing,
    }
}

fn merge_json_object(base: &mut serde_json::Value, extra: serde_json::Value) {
    let (Some(base), serde_json::Value::Object(extra)) = (base.as_object_mut(), extra) else {
        return;
    };
    for (key, value) in extra {
        base.insert(key, value);
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StatsFlushShutdownReport {
    pub signal_sent: bool,
    pub flushed: bool,
    pub timed_out: bool,
    pub task_failed: bool,
    pub pending_stats_batches: usize,
    pub pending_stats_deltas: usize,
    pub pending_runtime_mutations: usize,
    pub overflow_runtime_mutations: u64,
}

pub struct StatsFlushWorkerHandle {
    shutdown: Option<oneshot::Sender<Instant>>,
    task: JoinHandle<bool>,
    manager: Arc<MultiTokenManager>,
}

impl StatsFlushWorkerHandle {
    pub async fn shutdown(mut self, timeout: StdDuration) -> StatsFlushShutdownReport {
        const COMPLETION_RESERVE: StdDuration = StdDuration::from_millis(100);
        const ABORT_JOIN_TIMEOUT: StdDuration = StdDuration::from_secs(1);

        let wait_deadline = Instant::now()
            .checked_add(timeout)
            .unwrap_or_else(Instant::now);
        let flush_deadline = wait_deadline
            .checked_sub(timeout.min(COMPLETION_RESERVE))
            .unwrap_or_else(Instant::now);
        let signal_sent = self
            .shutdown
            .take()
            .is_some_and(|shutdown| shutdown.send(flush_deadline).is_ok());
        let mut task = self.task;
        let mut report = match tokio::time::timeout_at(wait_deadline.into(), &mut task).await {
            Ok(Ok(flushed)) => StatsFlushShutdownReport {
                signal_sent,
                flushed,
                ..StatsFlushShutdownReport::default()
            },
            Ok(Err(err)) => {
                tracing::warn!("凭据统计刷盘任务异常退出: {}", err);
                StatsFlushShutdownReport {
                    signal_sent,
                    task_failed: true,
                    ..StatsFlushShutdownReport::default()
                }
            }
            Err(_) => {
                task.abort();
                if tokio::time::timeout(ABORT_JOIN_TIMEOUT, &mut task)
                    .await
                    .is_err()
                {
                    tracing::warn!("凭据统计任务 abort 后仍未退出，停止等待该任务");
                }
                tracing::warn!("等待凭据统计最终刷盘超时，已停止后台任务");
                StatsFlushShutdownReport {
                    signal_sent,
                    timed_out: true,
                    ..StatsFlushShutdownReport::default()
                }
            }
        };
        let pending = self.manager.pending_persistence_backlog();
        report.pending_stats_batches = pending.stats_batches;
        report.pending_stats_deltas = pending.stats_deltas;
        report.pending_runtime_mutations = pending.runtime_mutations;
        let (_, overflow) = self.manager.runtime_mutation_backlog();
        report.overflow_runtime_mutations = overflow;
        if !report.timed_out && !report.task_failed {
            report.flushed = self.manager.refresh_stats_dirty_from_pending();
        }
        report
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RiskControlReportOutcome {
    pub has_available_credentials: bool,
    pub circuit_open: bool,
    pub retry_after_secs: Option<u64>,
}

impl RiskControlReportOutcome {
    pub(crate) fn can_retry_local(self) -> bool {
        self.has_available_credentials && !self.circuit_open
    }
}

#[derive(Debug, Clone, Copy)]
struct LocalPoolRiskCircuitEvent {
    at: Instant,
    credential_id: u64,
}

#[derive(Debug, Default)]
struct LocalPoolRiskCircuit {
    failures: VecDeque<LocalPoolRiskCircuitEvent>,
    open_until: Option<Instant>,
    reason: Option<CredentialRiskControlReason>,
}

#[derive(Debug, Clone, Copy, Default)]
struct LocalPoolRiskCircuitSnapshot {
    open: bool,
    retry_after: Option<StdDuration>,
}

#[derive(Debug)]
struct ModelCapabilityCohortCache {
    generation: u64,
    cohorts: Arc<Vec<KiroModelCapabilityCohort>>,
    keys: Arc<Vec<KiroModelCapabilityCohortKey>>,
    #[cfg(test)]
    rebuilds: u64,
}

impl Default for ModelCapabilityCohortCache {
    fn default() -> Self {
        Self {
            generation: 0,
            cohorts: Arc::new(Vec::new()),
            keys: Arc::new(Vec::new()),
            #[cfg(test)]
            rebuilds: 0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct RefreshAttemptIdentity([u8; 32]);

impl std::fmt::Debug for RefreshAttemptIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RefreshAttemptIdentity(<redacted>)")
    }
}

impl RefreshAttemptIdentity {
    fn from_credentials(credentials: &KiroCredentials) -> Self {
        fn update_optional(hasher: &mut Sha256, value: Option<&str>) {
            match value {
                Some(value) => {
                    hasher.update([1]);
                    hasher.update((value.len() as u64).to_le_bytes());
                    hasher.update(value.as_bytes());
                }
                None => hasher.update([0]),
            }
        }

        let mut hasher = Sha256::new();
        for value in [
            credentials.access_token.as_deref(),
            credentials.auth_method.as_deref(),
            credentials.provider.as_deref(),
            credentials.refresh_token.as_deref(),
            credentials.client_id.as_deref(),
            credentials.client_secret.as_deref(),
            credentials.token_endpoint.as_deref(),
            credentials.auth_region.as_deref(),
            credentials.region.as_deref(),
            credentials.scopes.as_deref(),
            credentials.machine_id.as_deref(),
        ] {
            update_optional(&mut hasher, value);
        }
        Self(hasher.finalize().into())
    }

    fn from_refresh_request(
        credentials: &KiroCredentials,
        config: &Config,
        effective_proxy: Option<&ProxyConfig>,
    ) -> Self {
        let base = Self::from_credentials(credentials);
        let mut hasher = Sha256::new();
        hasher.update(base.0);
        hasher.update([match config.tls_backend {
            crate::model::config::TlsBackend::Rustls => 0,
            crate::model::config::TlsBackend::NativeTls => 1,
        }]);
        for value in [
            Some(credentials.effective_auth_region(config)),
            Some(config.kiro_version.as_str()),
            config.machine_id.as_deref(),
            Some(config.system_version.as_str()),
            Some(config.node_version.as_str()),
            effective_proxy.map(|proxy| proxy.url.as_str()),
            effective_proxy.and_then(|proxy| proxy.username.as_deref()),
            effective_proxy.and_then(|proxy| proxy.password.as_deref()),
        ] {
            match value {
                Some(value) => {
                    hasher.update([1]);
                    hasher.update((value.len() as u64).to_le_bytes());
                    hasher.update(value.as_bytes());
                }
                None => hasher.update([0]),
            }
        }
        Self(hasher.finalize().into())
    }

    fn from_automatic_recovery_request(
        credentials: &KiroCredentials,
        rejected_access_token: &str,
        config: &Config,
        effective_proxy: Option<&ProxyConfig>,
    ) -> Self {
        let mut identity_credentials = credentials.clone();
        // Metadata-only storage revisions must not split callers that observed the same rejected
        // bearer token. Auth/proxy fields remain part of the identity and still fence real changes.
        identity_credentials.storage_revision = 0;
        identity_credentials.access_token = Some(rejected_access_token.to_string());
        Self::from_refresh_request(&identity_credentials, config, effective_proxy)
    }

    fn stable_jitter_percent(&self) -> u32 {
        let seed = u64::from_le_bytes(self.0[..8].try_into().expect("SHA-256 prefix"));
        80 + (seed % 21) as u32
    }
}

#[derive(Clone, Debug)]
struct CachedRefreshFailure {
    identity: RefreshAttemptIdentity,
    failure: RefreshFailure,
    consecutive_failures: u8,
    retry_at: Instant,
    health_action_pending: bool,
}

#[derive(Debug)]
struct CredentialRefreshState {
    gate: TokioMutex<()>,
    negative_result: Mutex<Option<CachedRefreshFailure>>,
    reset_generation: AtomicU64,
}

impl Default for CredentialRefreshState {
    fn default() -> Self {
        Self {
            gate: TokioMutex::new(()),
            negative_result: Mutex::new(None),
            reset_generation: AtomicU64::new(0),
        }
    }
}

impl CredentialRefreshState {
    fn replay_failure(
        &self,
        identity: &RefreshAttemptIdentity,
        now: Instant,
        caller_owns_health_action: bool,
    ) -> Option<RefreshFailure> {
        let mut cached = self.negative_result.lock();
        let cached = cached
            .as_mut()
            .filter(|cached| cached.identity == *identity && now < cached.retry_at)?;
        let mut failure = cached.failure.clone();
        if cached.health_action_pending && caller_owns_health_action {
            cached.health_action_pending = false;
        } else {
            failure = failure.into_shared_failure_wave();
        }
        Some(failure)
    }

    fn record_failure(
        &self,
        identity: RefreshAttemptIdentity,
        failure: &RefreshFailure,
        now: Instant,
        leader_owns_health_action: bool,
    ) -> Option<StdDuration> {
        if !refresh_failure_is_shareable(failure) {
            return None;
        }

        let mut cached = self.negative_result.lock();
        let consecutive_failures = cached
            .as_ref()
            .filter(|cached| {
                cached.identity == identity
                    && now
                        .checked_duration_since(cached.retry_at)
                        .is_none_or(|idle| idle <= TOKEN_REFRESH_NEGATIVE_STREAK_RESET_AFTER)
            })
            .map(|cached| cached.consecutive_failures.saturating_add(1))
            .unwrap_or(1)
            .min(TOKEN_REFRESH_NEGATIVE_BACKOFF_MAX_STREAK);
        let shift = u32::from(consecutive_failures.saturating_sub(1)).min(20);
        let exponential = TOKEN_REFRESH_NEGATIVE_BACKOFF_BASE
            .saturating_mul(1_u32 << shift)
            .min(TOKEN_REFRESH_NEGATIVE_BACKOFF_MAX);
        let jittered = exponential.saturating_mul(identity.stable_jitter_percent()) / 100;
        // The scheduler owns longer credential cooldowns. This short result cache only closes
        // the race before the leader records that cooldown, while still honoring ordinary
        // Retry-After values and bounding malicious headers.
        let retry_after = failure
            .retry_after
            .map(|duration| duration.min(TOKEN_REFRESH_NEGATIVE_RETRY_AFTER_MAX))
            .unwrap_or_default();
        let delay = jittered.max(retry_after).max(StdDuration::from_millis(1));
        *cached = Some(CachedRefreshFailure {
            identity,
            failure: failure.clone(),
            consecutive_failures,
            retry_at: now + delay,
            health_action_pending: refresh_failure_requires_health_action(failure)
                && !leader_owns_health_action,
        });
        Some(delay)
    }

    fn clear_failure(&self) {
        self.negative_result.lock().take();
    }

    fn generation(&self) -> u64 {
        self.reset_generation.load(Ordering::Acquire)
    }

    fn is_generation_current(&self, expected: u64) -> bool {
        self.generation() == expected
    }

    fn invalidate(&self) {
        self.reset_generation.fetch_add(1, Ordering::AcqRel);
        self.clear_failure();
    }
}

struct RefreshFailureWaveDropGuard {
    state: Arc<CredentialRefreshState>,
    identity: RefreshAttemptIdentity,
    reset_generation: u64,
    send_started: Arc<AtomicBool>,
    disarmed: bool,
}

impl RefreshFailureWaveDropGuard {
    fn new(
        state: Arc<CredentialRefreshState>,
        identity: RefreshAttemptIdentity,
        reset_generation: u64,
        send_started: Arc<AtomicBool>,
    ) -> Self {
        Self {
            state,
            identity,
            reset_generation,
            send_started,
            disarmed: false,
        }
    }

    fn disarm(&mut self) {
        self.disarmed = true;
    }
}

impl Drop for RefreshFailureWaveDropGuard {
    fn drop(&mut self) {
        if self.disarmed
            || !self.send_started.load(Ordering::Acquire)
            || !self.state.is_generation_current(self.reset_generation)
        {
            return;
        }
        let failure = RefreshFailure::new(
            RefreshFailureStage::Internal,
            RefreshFailureKind::Internal,
            None,
            None,
            true,
        );
        self.state
            .record_failure(self.identity, &failure, Instant::now(), false);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AutomaticTokenRecoveryOutcome {
    Refreshed,
    CredentialChanged,
}

fn refresh_failure_requires_health_action(failure: &RefreshFailure) -> bool {
    matches!(
        failure.kind,
        RefreshFailureKind::InvalidGrant
            | RefreshFailureKind::CredentialAuth
            | RefreshFailureKind::RateLimited
    )
}

fn refresh_failure_is_shareable(failure: &RefreshFailure) -> bool {
    if failure.shared_failure_wave || failure.kind == RefreshFailureKind::InvalidConfiguration {
        return false;
    }
    failure.send_committed
        || (failure.stage == RefreshFailureStage::Coordination
            && matches!(
                failure.kind,
                RefreshFailureKind::Coordination | RefreshFailureKind::Timeout
            ))
        || (failure.stage == RefreshFailureStage::RequestSend
            && matches!(
                failure.kind,
                RefreshFailureKind::Network | RefreshFailureKind::Timeout
            ))
}

fn redis_refresh_failure_stage(stage: RefreshFailureStage) -> RedisRefreshFailureStage {
    match stage {
        RefreshFailureStage::Validation => RedisRefreshFailureStage::Validation,
        RefreshFailureStage::RequestSend => RedisRefreshFailureStage::RequestSend,
        RefreshFailureStage::ResponseHeaders => RedisRefreshFailureStage::ResponseHeaders,
        RefreshFailureStage::ResponseBody => RedisRefreshFailureStage::ResponseBody,
        RefreshFailureStage::ResponseStatus => RedisRefreshFailureStage::ResponseStatus,
        RefreshFailureStage::ResponseDecode => RedisRefreshFailureStage::ResponseDecode,
        RefreshFailureStage::ResponseValidate => RedisRefreshFailureStage::ResponseValidate,
        RefreshFailureStage::Coordination => RedisRefreshFailureStage::Coordination,
        RefreshFailureStage::Persistence => RedisRefreshFailureStage::Persistence,
        RefreshFailureStage::Internal => RedisRefreshFailureStage::Internal,
    }
}

fn redis_refresh_failure_kind(kind: RefreshFailureKind) -> RedisRefreshFailureKind {
    match kind {
        RefreshFailureKind::InvalidGrant => RedisRefreshFailureKind::InvalidGrant,
        RefreshFailureKind::CredentialAuth => RedisRefreshFailureKind::CredentialAuth,
        RefreshFailureKind::RateLimited => RedisRefreshFailureKind::RateLimited,
        RefreshFailureKind::UpstreamUnavailable => RedisRefreshFailureKind::UpstreamUnavailable,
        RefreshFailureKind::Network => RedisRefreshFailureKind::Network,
        RefreshFailureKind::Timeout => RedisRefreshFailureKind::Timeout,
        RefreshFailureKind::Protocol => RedisRefreshFailureKind::Protocol,
        RefreshFailureKind::Oversize => RedisRefreshFailureKind::Oversize,
        RefreshFailureKind::MalformedResponse => RedisRefreshFailureKind::MalformedResponse,
        RefreshFailureKind::MissingToken => RedisRefreshFailureKind::MissingToken,
        RefreshFailureKind::InvalidConfiguration => RedisRefreshFailureKind::InvalidConfiguration,
        RefreshFailureKind::Coordination => RedisRefreshFailureKind::Coordination,
        RefreshFailureKind::Persistence => RedisRefreshFailureKind::Persistence,
        RefreshFailureKind::Internal => RedisRefreshFailureKind::Internal,
    }
}

fn refresh_failure_stage_from_redis(stage: RedisRefreshFailureStage) -> RefreshFailureStage {
    match stage {
        RedisRefreshFailureStage::Validation => RefreshFailureStage::Validation,
        RedisRefreshFailureStage::RequestSend => RefreshFailureStage::RequestSend,
        RedisRefreshFailureStage::ResponseHeaders => RefreshFailureStage::ResponseHeaders,
        RedisRefreshFailureStage::ResponseBody => RefreshFailureStage::ResponseBody,
        RedisRefreshFailureStage::ResponseStatus => RefreshFailureStage::ResponseStatus,
        RedisRefreshFailureStage::ResponseDecode => RefreshFailureStage::ResponseDecode,
        RedisRefreshFailureStage::ResponseValidate => RefreshFailureStage::ResponseValidate,
        RedisRefreshFailureStage::Coordination => RefreshFailureStage::Coordination,
        RedisRefreshFailureStage::Persistence => RefreshFailureStage::Persistence,
        RedisRefreshFailureStage::Internal => RefreshFailureStage::Internal,
    }
}

fn refresh_failure_kind_from_redis(kind: RedisRefreshFailureKind) -> RefreshFailureKind {
    match kind {
        RedisRefreshFailureKind::InvalidGrant => RefreshFailureKind::InvalidGrant,
        RedisRefreshFailureKind::CredentialAuth => RefreshFailureKind::CredentialAuth,
        RedisRefreshFailureKind::RateLimited => RefreshFailureKind::RateLimited,
        RedisRefreshFailureKind::UpstreamUnavailable => RefreshFailureKind::UpstreamUnavailable,
        RedisRefreshFailureKind::Network => RefreshFailureKind::Network,
        RedisRefreshFailureKind::Timeout => RefreshFailureKind::Timeout,
        RedisRefreshFailureKind::Protocol => RefreshFailureKind::Protocol,
        RedisRefreshFailureKind::Oversize => RefreshFailureKind::Oversize,
        RedisRefreshFailureKind::MalformedResponse => RefreshFailureKind::MalformedResponse,
        RedisRefreshFailureKind::MissingToken => RefreshFailureKind::MissingToken,
        RedisRefreshFailureKind::InvalidConfiguration => RefreshFailureKind::InvalidConfiguration,
        RedisRefreshFailureKind::Coordination => RefreshFailureKind::Coordination,
        RedisRefreshFailureKind::Persistence => RefreshFailureKind::Persistence,
        RedisRefreshFailureKind::Internal => RefreshFailureKind::Internal,
    }
}

fn refresh_failure_to_redis(failure: &RefreshFailure) -> RedisRefreshFailure {
    RedisRefreshFailure {
        stage: redis_refresh_failure_stage(failure.stage),
        kind: redis_refresh_failure_kind(failure.kind),
        status: failure.status,
        retry_after: failure.retry_after,
        send_committed: failure.send_committed,
        health_action_required: refresh_failure_requires_health_action(failure),
    }
}

fn refresh_failure_from_redis(failure: &RedisRefreshFailure) -> RefreshFailure {
    RefreshFailure::new(
        refresh_failure_stage_from_redis(failure.stage),
        refresh_failure_kind_from_redis(failure.kind),
        failure.status,
        failure.retry_after,
        failure.send_committed,
    )
}

enum DistributedRefreshDecision {
    Leader(RedisRefreshLease),
    Replay(RefreshFailure),
    Succeeded {
        generation: u64,
        storage_revision: u64,
    },
}

/// 多凭据 Token 管理器
///
/// 支持多个凭据的管理，实现固定优先级 + 故障转移策略
/// 故障统计基于 API 调用结果，而非 Token 刷新结果
pub struct MultiTokenManager {
    config: Mutex<Config>,
    proxy: Option<ProxyConfig>,
    /// 凭据条目列表
    entries: Arc<Mutex<Vec<CredentialEntry>>>,
    /// 当前活动凭据 ID
    current_id: Mutex<u64>,
    /// Token 刷新状态按凭据隔离。每个槽位只有一个 gate 和一个短期 typed 失败结果，
    /// 因而状态量受 credential 数量硬限制，不会随请求或失败波增长。
    refresh_states: Mutex<HashMap<u64, Arc<CredentialRefreshState>>>,
    /// 单实例辅助上游并发边界与有界 refresh HTTP client cache。
    auxiliary_runtime: AuxiliaryRuntime,
    /// PgSQL 存储后端。生产运行必须配置；测试可使用 None 避免依赖外部数据库。
    postgres_store: Option<Arc<PostgresStore>>,
    /// Redis 运行态存储后端。生产用于跨实例调度状态；测试可使用 None 走本地内存。
    redis_store: Option<Arc<RedisStore>>,
    /// 代理/家宽资源快照。凭据直接代理优先，未设置时可通过 proxy_resource_id 绑定这里的资源。
    proxy_resources: Arc<Mutex<HashMap<u64, ProxyResourceRuntime>>>,
    /// 负载均衡模式（运行时可修改）
    load_balancing_mode: Mutex<String>,
    /// 最近一次统计持久化时间（用于 debounce）
    last_stats_save_at: Mutex<Option<Instant>>,
    /// 最近一次清理 PgSQL 运行态 mutation 幂等记录的时间。
    last_runtime_mutation_cleanup_at: Mutex<Option<Instant>>,
    /// 最近一次从 Redis 全量同步调度状态的时间，用于避免每个请求重复拉取所有凭据状态。
    last_scheduler_redis_sync_at: Arc<Mutex<Option<Instant>>>,
    /// 最近一次执行 Redis 超时 lease 清理的时间。清理是全局操作，不能放在请求热路径每轮执行。
    last_scheduler_redis_cleanup_at: Mutex<Option<Instant>>,
    /// Redis 容量协调 breaker；Open/HalfOpen 都保持 fail-closed，恢复时严格单 probe。
    scheduler_redis_breaker: Arc<SchedulerRedisBreaker>,
    /// 后台 snapshot 独立降级；失败只能让本地快照变旧，不能冻结原子容量准入。
    scheduler_redis_snapshot_breaker: Arc<SchedulerRedisBreaker>,
    /// 会话粘性是可降级的辅助能力，不能因其 Redis 读写失败冻结凭据并发准入。
    scheduler_redis_affinity_breaker: Arc<SchedulerRedisBreaker>,
    /// lease/queue release 使用独立硬有界 reconciliation lane，不占请求热路径 semaphore。
    scheduler_redis_release_dispatcher: Option<Arc<SchedulerRedisReleaseDispatcher>>,
    #[cfg(test)]
    request_binding_snapshot_reads: AtomicU64,
    /// Redis 全量调度状态同步后台任务是否正在运行，避免高并发下重复扫全部账号状态。
    scheduler_redis_sync_in_flight: Arc<AtomicBool>,
    scheduler_redis_full_sync_requested: Arc<AtomicU64>,
    scheduler_redis_full_sync_applied: Arc<AtomicU64>,
    scheduler_redis_dirty_credential_ids: Arc<Mutex<HashSet<u64>>>,
    scheduler_instance_id: Arc<str>,
    /// 统计数据是否有未落盘更新
    stats_dirty: AtomicBool,
    /// 未落盘的统计增量。请求热路径只合并内存 delta，后台任务定期写入 PgSQL。
    pending_stats_deltas: Mutex<HashMap<u64, CredentialStatsDeltaRow>>,
    /// 已冻结的统计批次；重试必须保留相同 operation ID 和完全相同的 payload。
    pending_stats_batches: Mutex<VecDeque<PendingCredentialStatsBatch>>,
    /// PgSQL 临时不可用时保留的权威运行态 mutation，按凭据 FIFO 重放。
    pending_runtime_mutations: Mutex<HashMap<u64, VecDeque<PendingCredentialRuntimeMutation>>>,
    /// 成功请求用于发现“其他实例写入的 PgSQL 运行态脏状态”的限频探测。
    ///
    /// 本地已知脏态和待重放 mutation 仍立即持久化；只有本进程内存干净、但可能存在
    /// 跨实例失败计数时，才按凭据短 TTL 触发一次 PgSQL reconcile，避免稳态成功请求
    /// 对 PgSQL 做一请求一事务的热路径放大。
    last_runtime_success_reconcile_probe_at: Mutex<HashMap<u64, Instant>>,
    #[cfg(test)]
    runtime_success_reconcile_probe_attempts: AtomicU64,
    overflow_runtime_mutations: AtomicU64,
    runtime_mutation_flush_cursor: AtomicU64,
    /// 会话粘性绑定：conversationId -> credential id
    session_bindings: Mutex<HashMap<String, SessionBinding>>,
    /// 凭据容量 credit 与广播 generation；多槽释放不会折叠成一个通知。
    capacity_signal: Arc<CapacitySignal>,
    /// 进程内本地账号池风控熔断。它不依赖 Redis，确保 Redis degraded 或外部池
    /// 配置异常时也能先停止继续探测剩余本地账号。
    local_pool_risk_circuit: Mutex<LocalPoolRiskCircuit>,
    /// 本进程近期已经释放的 Redis lease。Redis release 是异步写，后台同步可能先读到旧快照；
    /// 这里用短 TTL tombstone 避免旧 lease 被重新导入本地并发槽。
    released_in_flight_lease_tombstones: ReleasedInFlightLeaseTombstones,
    next_in_flight_lease_id: AtomicU64,
    queued_requests: Arc<AtomicU32>,
    /// Serializes only candidate selection plus the local provisional reservation.
    /// Redis and upstream awaits always run after this guard is dropped.
    selection_reservation_gate: Mutex<()>,
    /// Secret-free capability cohorts are rebuilt only after a relevant credential/config/proxy
    /// mutation. Request reads clone one Arc and never scan the account pool.
    model_capability_cohort_generation: AtomicU64,
    model_capability_cohort_cache: Mutex<ModelCapabilityCohortCache>,
}

/// 每个凭据最大 API 调用失败次数
const MAX_FAILURES_PER_CREDENTIAL: u32 = 3;
/// 并发排队等待的周期性唤醒间隔，避免极端竞态下丢失通知后永久睡眠。
const CONCURRENCY_WAIT_WAKEUP_SECS: u64 = 30;
const DISPATCH_QUEUE_LEASE_SAFETY_MARGIN_SECS: u64 = 60;
const SCHEDULER_REDIS_SYNC_MIN_INTERVAL: StdDuration = StdDuration::from_secs(1);
const SCHEDULER_REDIS_CLEANUP_MIN_INTERVAL: StdDuration = StdDuration::from_secs(5);
const PERSISTED_RUNTIME_SUCCESS_RECONCILE_MIN_INTERVAL: StdDuration = StdDuration::from_secs(2);
/// Capacity/queue operations are correctness-critical, but a single Docker/Redis/runtime jitter
/// above the old 75ms budget must not turn a healthy local pool into a global 429 wave.
/// Affinity/sticky operations keep their stricter budget on a separate breaker.
const SCHEDULER_REDIS_HOT_OP_TIMEOUT: StdDuration = StdDuration::from_millis(250);
const SCHEDULER_REDIS_AFFINITY_OP_TIMEOUT: StdDuration = StdDuration::from_millis(75);
const SCHEDULER_REDIS_TIMEOUT_FAILURES_TO_OPEN: u32 = 3;
const SCHEDULER_REDIS_SNAPSHOT_OP_TIMEOUT: StdDuration = StdDuration::from_millis(500);
const SCHEDULER_REDIS_MAX_IN_FLIGHT_OPERATIONS: usize = 256;
const SCHEDULER_REDIS_SNAPSHOT_MAX_IN_FLIGHT_OPERATIONS: usize = 1;
const SCHEDULER_REDIS_AFFINITY_MAX_IN_FLIGHT_OPERATIONS: usize = 128;
const SCHEDULER_REDIS_REMOTE_SYNC_DEBOUNCE: StdDuration = StdDuration::from_millis(25);
const SCHEDULER_REDIS_DEGRADED_BACKOFF_BASE: StdDuration = StdDuration::from_secs(2);
const SCHEDULER_REDIS_DEGRADED_BACKOFF_MAX: StdDuration = StdDuration::from_secs(30);
/// In local-preflight/external-fallback mode, a sticky-bound request should not immediately
/// migrate to another credential solely because the previous cross-instance Redis release has
/// not become visible yet. Keep this window short so a real holder still falls back quickly.
const STICKY_BOUND_RELEASE_PROPAGATION_GRACE: StdDuration = StdDuration::from_millis(250);
const CREDENTIAL_STATS_FLUSH_MIN_INTERVAL: StdDuration = StdDuration::from_secs(5);
const CREDENTIAL_RUNTIME_MUTATION_CLEANUP_INTERVAL: StdDuration = StdDuration::from_secs(60);
const CREDENTIAL_RUNTIME_MUTATION_RETENTION: StdDuration =
    StdDuration::from_secs(30 * 24 * 60 * 60);
const CREDENTIAL_RUNTIME_MUTATION_CLEANUP_LIMIT: usize = 10_000;
const CREDENTIAL_RUNTIME_MUTATION_CLEANUP_MAX_BATCHES: usize = 64;
const CREDENTIAL_RUNTIME_MUTATION_CLEANUP_BUDGET: StdDuration = StdDuration::from_secs(5);
const SOFT_PENDING_RUNTIME_MUTATIONS_PER_CREDENTIAL: usize = 4_096;
const SOFT_PENDING_RUNTIME_MUTATIONS_TOTAL: usize = 65_536;
const RUNTIME_MUTATION_FLUSH_LIMIT: usize = 256;
const RUNTIME_MUTATION_FLUSH_BUDGET: StdDuration = StdDuration::from_secs(10);
const CREDENTIAL_PGSQL_SYNC_TIMEOUT: StdDuration = StdDuration::from_secs(5);
const CREDENTIAL_PGSQL_WORKFLOW_TIMEOUT: StdDuration = StdDuration::from_secs(15);
const REFRESH_REDIS_LOCK_OP_TIMEOUT: StdDuration = StdDuration::from_secs(2);
const TOKEN_REFRESH_WORKFLOW_TIMEOUT: StdDuration = StdDuration::from_secs(90);
const TOKEN_REFRESH_COORDINATION_WAIT_TIMEOUT: StdDuration = StdDuration::from_secs(15);
const TOKEN_REFRESH_RECONCILIATION_RESERVE: StdDuration = StdDuration::from_secs(15);
const TOKEN_REFRESH_REDIS_LOCK_TTL_SECS: usize = 120;
const TOKEN_REFRESH_NEGATIVE_BACKOFF_BASE: StdDuration = StdDuration::from_millis(500);
const TOKEN_REFRESH_NEGATIVE_BACKOFF_MAX: StdDuration = StdDuration::from_secs(30);
const TOKEN_REFRESH_NEGATIVE_RETRY_AFTER_MAX: StdDuration = StdDuration::from_secs(60);
const TOKEN_REFRESH_NEGATIVE_STREAK_RESET_AFTER: StdDuration = StdDuration::from_secs(60);
const TOKEN_REFRESH_NEGATIVE_BACKOFF_MAX_STREAK: u8 = 16;
const LOCAL_REDIS_LEASE_COUNTER_SPACE: u64 = 1_000_000_000;
const LOCAL_REDIS_LEASE_NAMESPACE_COUNT: u64 = 9_000_000_000;
static LOCAL_REDIS_LEASE_NAMESPACE_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DispatchQueueLeasePolicy {
    ttl_secs: u64,
    renewal_required: bool,
}

fn dispatch_queue_lease_policy(max_wait: Option<StdDuration>) -> DispatchQueueLeasePolicy {
    let Some(max_wait) = max_wait else {
        return DispatchQueueLeasePolicy {
            ttl_secs: DISPATCH_QUEUE_LEASE_SAFETY_MARGIN_SECS,
            renewal_required: true,
        };
    };
    let rounded_wait_secs = max_wait
        .as_secs()
        .saturating_add(u64::from(max_wait.subsec_nanos() > 0));
    DispatchQueueLeasePolicy {
        ttl_secs: rounded_wait_secs
            .saturating_add(DISPATCH_QUEUE_LEASE_SAFETY_MARGIN_SECS)
            .max(DISPATCH_QUEUE_LEASE_SAFETY_MARGIN_SECS),
        renewal_required: false,
    }
}

enum SchedulerRedisHotOutcome<T> {
    Completed(T),
    Skipped,
    LocalSchedulerOverloaded,
    Failed { commit_unknown: bool },
}

struct SuccessReportOutcome {
    expected_generation: u64,
    last_used_at: String,
    runtime_state_changed: bool,
    alpha: f64,
}

#[derive(Debug, Clone, Copy)]
struct SchedulerRedisUnavailableError {
    retry_after_secs: u64,
    local_overloaded: bool,
}

impl fmt::Display for SchedulerRedisUnavailableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.local_overloaded {
            f.write_str("本地账号调度容量暂不可用（本地 scheduler admission 已饱和）")
        } else {
            write!(
                f,
                "本地账号调度容量暂不可用（Redis 调度协调状态不可用，retry_after_secs={}）",
                self.retry_after_secs
            )
        }
    }
}

impl StdError for SchedulerRedisUnavailableError {}

#[derive(Debug, Clone, Copy)]
struct SchedulerRedisRateLimitWait {
    retry_after: StdDuration,
    available_at: Instant,
}

#[derive(Debug, Clone, Copy)]
enum SchedulerRedisSelectionAdmission {
    NotRequired,
    Recorded {
        rate_limit_available_at: Option<Instant>,
    },
    RateLimited(SchedulerRedisRateLimitWait),
}

enum SchedulerRedisExecutionOutcome<T> {
    Completed(T),
    NotStarted(SchedulerRedisAdmissionError),
    Failed { commit_unknown: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SchedulerRedisAdmissionError {
    BreakerOpen,
    LocalSchedulerOverloaded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SchedulerRedisBreakerPhase {
    Closed,
    Open { until: Instant },
    HalfOpen,
}

#[derive(Debug)]
struct SchedulerRedisBreakerState {
    failure_generation: u64,
    consecutive_failures: u32,
    phase: SchedulerRedisBreakerPhase,
}

impl Default for SchedulerRedisBreakerState {
    fn default() -> Self {
        Self {
            failure_generation: 0,
            consecutive_failures: 0,
            phase: SchedulerRedisBreakerPhase::Closed,
        }
    }
}

fn scheduler_redis_failure_commit_unknown(err: &anyhow::Error) -> bool {
    let redis_error = err
        .chain()
        .find_map(|cause| cause.downcast_ref::<redis::RedisError>());
    let Some(redis_error) = redis_error else {
        return true;
    };

    if redis_error.is_connection_refusal() {
        return false;
    }

    redis_error.is_timeout()
        || redis_error.is_connection_dropped()
        || redis_error.kind() == redis::ErrorKind::IoError
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SchedulerRedisBreakerKind {
    Capacity,
    Snapshot,
    Affinity,
}

impl SchedulerRedisBreakerKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Capacity => "capacity",
            Self::Snapshot => "snapshot",
            Self::Affinity => "affinity",
        }
    }
}

#[derive(Debug, Default)]
struct SchedulerRedisBreakerStats {
    admitted: AtomicU64,
    local_saturated: AtomicU64,
    fail_fast: AtomicU64,
    recovery_probes: AtomicU64,
    failures: AtomicU64,
    recoveries: AtomicU64,
    cancelled_probes: AtomicU64,
    stale_successes: AtomicU64,
    stale_failures: AtomicU64,
    suppressed: AtomicU64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct SchedulerRedisBreakerStatsSnapshot {
    admitted: u64,
    local_saturated: u64,
    fail_fast: u64,
    recovery_probes: u64,
    failures: u64,
    recoveries: u64,
    cancelled_probes: u64,
    stale_successes: u64,
    stale_failures: u64,
    suppressed: u64,
}

#[derive(Debug)]
struct SchedulerRedisBreaker {
    kind: SchedulerRedisBreakerKind,
    state: Mutex<SchedulerRedisBreakerState>,
    stats: SchedulerRedisBreakerStats,
    operation_semaphore: Arc<Semaphore>,
    jitter_seed: u64,
}

impl SchedulerRedisBreaker {
    fn new(kind: SchedulerRedisBreakerKind, max_in_flight: usize, jitter_seed: u64) -> Arc<Self> {
        Arc::new(Self {
            kind,
            state: Mutex::new(SchedulerRedisBreakerState::default()),
            stats: SchedulerRedisBreakerStats::default(),
            operation_semaphore: Arc::new(Semaphore::new(max_in_flight.max(1))),
            jitter_seed,
        })
    }

    fn base_backoff_for_failure(consecutive_failures: u32) -> StdDuration {
        let shift = consecutive_failures.saturating_sub(1).min(4);
        SCHEDULER_REDIS_DEGRADED_BACKOFF_BASE
            .saturating_mul(1u32 << shift)
            .min(SCHEDULER_REDIS_DEGRADED_BACKOFF_MAX)
    }

    fn backoff_for_failure(
        &self,
        consecutive_failures: u32,
        failure_generation: u64,
    ) -> StdDuration {
        stable_scheduler_backoff_jitter(
            Self::base_backoff_for_failure(consecutive_failures),
            self.jitter_seed
                ^ failure_generation.wrapping_mul(0x9e37_79b9_7f4a_7c15)
                ^ u64::from(consecutive_failures).rotate_left(17),
        )
    }

    async fn begin_until(
        self: &Arc<Self>,
        deadline: tokio::time::Instant,
        operation_timeout: StdDuration,
    ) -> Result<SchedulerRedisOperationAdmission, SchedulerRedisAdmissionError> {
        let (failure_generation, recovery_probe) = {
            let now = Instant::now();
            let mut state = self.state.lock();
            match state.phase {
                SchedulerRedisBreakerPhase::Closed => (state.failure_generation, false),
                SchedulerRedisBreakerPhase::Open { until } if until > now => {
                    self.stats.fail_fast.fetch_add(1, Ordering::Relaxed);
                    self.stats.suppressed.fetch_add(1, Ordering::Relaxed);
                    return Err(SchedulerRedisAdmissionError::BreakerOpen);
                }
                SchedulerRedisBreakerPhase::Open { .. } => {
                    state.phase = SchedulerRedisBreakerPhase::HalfOpen;
                    self.stats.recovery_probes.fetch_add(1, Ordering::Relaxed);
                    (state.failure_generation, true)
                }
                SchedulerRedisBreakerPhase::HalfOpen => {
                    self.stats.fail_fast.fetch_add(1, Ordering::Relaxed);
                    self.stats.suppressed.fetch_add(1, Ordering::Relaxed);
                    return Err(SchedulerRedisAdmissionError::BreakerOpen);
                }
            }
        };
        let mut admission = SchedulerRedisOperationAdmission {
            breaker: self.clone(),
            operation_permit: None,
            failure_generation,
            recovery_probe,
            operation_timeout,
            completed: false,
        };
        let permit = match tokio::time::timeout_at(
            deadline,
            self.operation_semaphore.clone().acquire_owned(),
        )
        .await
        {
            Ok(Ok(permit)) if tokio::time::Instant::now() < deadline => permit,
            Ok(Ok(_)) | Ok(Err(_)) | Err(_) => {
                let saturated = self.stats.local_saturated.fetch_add(1, Ordering::Relaxed) + 1;
                if saturated == 1 || saturated.is_power_of_two() {
                    tracing::warn!(
                        breaker = self.kind.as_str(),
                        saturated,
                        timeout_ms = operation_timeout.as_millis() as u64,
                        "本地 Redis scheduler operation semaphore 饱和，Redis breaker 保持原状态"
                    );
                }
                return Err(SchedulerRedisAdmissionError::LocalSchedulerOverloaded);
            }
        };
        let still_current = {
            let state = self.state.lock();
            state.failure_generation == failure_generation
                && if recovery_probe {
                    state.phase == SchedulerRedisBreakerPhase::HalfOpen
                } else {
                    state.phase == SchedulerRedisBreakerPhase::Closed
                }
        };
        if !still_current {
            admission.completed = true;
            self.stats.fail_fast.fetch_add(1, Ordering::Relaxed);
            self.stats.suppressed.fetch_add(1, Ordering::Relaxed);
            return Err(SchedulerRedisAdmissionError::BreakerOpen);
        }
        admission.operation_permit = Some(permit);
        self.stats.admitted.fetch_add(1, Ordering::Relaxed);
        Ok(admission)
    }

    fn complete_success(&self, failure_generation: u64, recovery_probe: bool) {
        let mut state = self.state.lock();
        if state.failure_generation != failure_generation {
            let stale = self.stats.stale_successes.fetch_add(1, Ordering::Relaxed) + 1;
            if stale == 1 || stale.is_power_of_two() {
                tracing::debug!(
                    breaker = self.kind.as_str(),
                    failure_generation,
                    current_generation = state.failure_generation,
                    stale_successes = stale,
                    "忽略旧 generation 的 Redis scheduler success"
                );
            }
            return;
        }
        match (recovery_probe, state.phase) {
            (true, SchedulerRedisBreakerPhase::HalfOpen) => {
                state.phase = SchedulerRedisBreakerPhase::Closed;
                state.consecutive_failures = 0;
                self.stats.recoveries.fetch_add(1, Ordering::Relaxed);
                let suppressed = self.stats.suppressed.swap(0, Ordering::Relaxed);
                tracing::info!(
                    breaker = self.kind.as_str(),
                    failure_generation,
                    suppressed_requests = suppressed,
                    "Redis scheduler breaker 恢复，关闭 HalfOpen"
                );
            }
            (false, SchedulerRedisBreakerPhase::Closed) => {
                state.consecutive_failures = 0;
            }
            _ => {
                self.stats.stale_successes.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn complete_failure(
        &self,
        failure_generation: u64,
        recovery_probe: bool,
        operation: &'static str,
        operation_timeout: StdDuration,
        err: &anyhow::Error,
    ) {
        self.stats.failures.fetch_add(1, Ordering::Relaxed);
        let timeout_failure = err
            .chain()
            .any(|cause| cause.to_string().contains("超过共享总期限"));
        let (next_generation, consecutive_failures, backoff) = {
            let mut state = self.state.lock();
            if state.failure_generation != failure_generation {
                self.stats.stale_failures.fetch_add(1, Ordering::Relaxed);
                return;
            }
            state.consecutive_failures = state.consecutive_failures.saturating_add(1).max(1);
            if self.kind == SchedulerRedisBreakerKind::Snapshot && !recovery_probe {
                tracing::warn!(
                    operation,
                    breaker = self.kind.as_str(),
                    failure_generation,
                    consecutive_failures = state.consecutive_failures,
                    timeout_ms = operation_timeout.as_millis() as u64,
                    "Redis scheduler snapshot operation 失败但不打开 breaker，沿用本地调度缓存: {}",
                    err
                );
                return;
            }
            if self.kind == SchedulerRedisBreakerKind::Capacity
                && timeout_failure
                && !recovery_probe
                && state.consecutive_failures < SCHEDULER_REDIS_TIMEOUT_FAILURES_TO_OPEN
            {
                tracing::warn!(
                    operation,
                    breaker = self.kind.as_str(),
                    failure_generation,
                    consecutive_failures = state.consecutive_failures,
                    timeout_ms = operation_timeout.as_millis() as u64,
                    open_after_failures = SCHEDULER_REDIS_TIMEOUT_FAILURES_TO_OPEN,
                    "Redis scheduler capacity operation 超时但未打开 breaker: {}",
                    err
                );
                return;
            }
            state.failure_generation = state.failure_generation.wrapping_add(1);
            let backoff =
                self.backoff_for_failure(state.consecutive_failures, state.failure_generation);
            state.phase = SchedulerRedisBreakerPhase::Open {
                until: Instant::now() + backoff,
            };
            (
                state.failure_generation,
                state.consecutive_failures,
                backoff,
            )
        };
        let suppressed = self.stats.suppressed.swap(0, Ordering::Relaxed);
        tracing::warn!(
            operation,
            breaker = self.kind.as_str(),
            failure_generation,
            next_generation,
            recovery_probe,
            consecutive_failures,
            timeout_ms = operation_timeout.as_millis() as u64,
            backoff_ms = backoff.as_millis() as u64,
            suppressed_requests = suppressed,
            "Redis scheduler operation 失败，打开 breaker: {}",
            err
        );
    }

    fn cancel_probe(&self, failure_generation: u64) {
        let mut state = self.state.lock();
        if state.failure_generation != failure_generation
            || state.phase != SchedulerRedisBreakerPhase::HalfOpen
        {
            return;
        }
        let backoff =
            self.backoff_for_failure(state.consecutive_failures.max(1), state.failure_generation);
        state.phase = SchedulerRedisBreakerPhase::Open {
            until: Instant::now() + backoff,
        };
        self.stats.cancelled_probes.fetch_add(1, Ordering::Relaxed);
    }

    fn recovery_probe_due(&self) -> bool {
        matches!(
            self.state.lock().phase,
            SchedulerRedisBreakerPhase::Open { until } if until <= Instant::now()
        )
    }

    #[cfg(test)]
    fn is_degraded(&self) -> bool {
        self.state.lock().phase != SchedulerRedisBreakerPhase::Closed
    }

    fn retry_after(&self) -> Option<StdDuration> {
        let now = Instant::now();
        match self.state.lock().phase {
            SchedulerRedisBreakerPhase::Closed => None,
            SchedulerRedisBreakerPhase::Open { until } => Some(
                until
                    .checked_duration_since(now)
                    .unwrap_or_else(|| StdDuration::from_millis(1)),
            ),
            SchedulerRedisBreakerPhase::HalfOpen => Some(StdDuration::from_millis(1)),
        }
    }

    fn stats_snapshot(&self) -> SchedulerRedisBreakerStatsSnapshot {
        SchedulerRedisBreakerStatsSnapshot {
            admitted: self.stats.admitted.load(Ordering::Relaxed),
            local_saturated: self.stats.local_saturated.load(Ordering::Relaxed),
            fail_fast: self.stats.fail_fast.load(Ordering::Relaxed),
            recovery_probes: self.stats.recovery_probes.load(Ordering::Relaxed),
            failures: self.stats.failures.load(Ordering::Relaxed),
            recoveries: self.stats.recoveries.load(Ordering::Relaxed),
            cancelled_probes: self.stats.cancelled_probes.load(Ordering::Relaxed),
            stale_successes: self.stats.stale_successes.load(Ordering::Relaxed),
            stale_failures: self.stats.stale_failures.load(Ordering::Relaxed),
            suppressed: self.stats.suppressed.load(Ordering::Relaxed),
        }
    }
}

struct SchedulerRedisOperationAdmission {
    breaker: Arc<SchedulerRedisBreaker>,
    operation_permit: Option<OwnedSemaphorePermit>,
    failure_generation: u64,
    recovery_probe: bool,
    operation_timeout: StdDuration,
    completed: bool,
}

impl SchedulerRedisOperationAdmission {
    #[cfg(test)]
    fn recovery_probe(&self) -> bool {
        self.recovery_probe
    }

    fn success(mut self) {
        self.completed = true;
        self.breaker
            .complete_success(self.failure_generation, self.recovery_probe);
    }

    fn failure(mut self, operation: &'static str, err: &anyhow::Error) {
        self.completed = true;
        self.breaker.complete_failure(
            self.failure_generation,
            self.recovery_probe,
            operation,
            self.operation_timeout,
            err,
        );
    }
}

impl Drop for SchedulerRedisOperationAdmission {
    fn drop(&mut self) {
        if !self.completed && self.recovery_probe {
            self.breaker.cancel_probe(self.failure_generation);
        }
    }
}

fn stable_scheduler_backoff_jitter(base: StdDuration, seed: u64) -> StdDuration {
    let base_millis = base.as_millis().max(1);
    let spread = (base_millis / 10).max(1);
    let mixed = seed
        .wrapping_add(0x9e37_79b9_7f4a_7c15)
        .wrapping_mul(0xbf58_476d_1ce4_e5b9)
        ^ seed.rotate_left(27);
    let reduction = mixed as u128 % spread.saturating_add(1);
    StdDuration::from_millis(base_millis.saturating_sub(reduction).min(u64::MAX as u128) as u64)
}

enum PreparedInFlightSlot {
    Local(InFlightLeaseGuard),
    Redis {
        guard: InFlightLeaseGuard,
        effective_max_concurrent_requests: u32,
        global_max_concurrent_requests: u32,
        request_weight_units: u32,
        max_age: Option<StdDuration>,
    },
}

#[derive(Clone, Debug)]
enum PendingCredentialRuntimeMutation {
    Success {
        operation_id: uuid::Uuid,
        expected_generation: u64,
        success_count: u32,
    },
    ApiFailure {
        operation_id: uuid::Uuid,
        expected_generation: u64,
        last_used_at: String,
    },
    #[cfg(test)]
    RefreshFailure {
        operation_id: uuid::Uuid,
        expected_generation: u64,
        last_used_at: String,
    },
    Disable {
        operation_id: uuid::Uuid,
        expected_generation: u64,
        reason: String,
        failure_count: Option<u32>,
        refresh_failure_count: Option<u32>,
        last_used_at: String,
    },
    Patch {
        operation_id: uuid::Uuid,
        patch: CredentialRuntimeStatePatch,
    },
}

struct PersistedCredentialRuntimeMutation {
    state: CredentialRuntimeStateRow,
    credential_disabled: Option<bool>,
    applied: bool,
}

#[derive(Clone, Copy)]
struct TokenRefreshBudgets {
    workflow: StdDuration,
    coordination: StdDuration,
    reconciliation: StdDuration,
}

impl Default for TokenRefreshBudgets {
    fn default() -> Self {
        Self {
            workflow: TOKEN_REFRESH_WORKFLOW_TIMEOUT,
            coordination: TOKEN_REFRESH_COORDINATION_WAIT_TIMEOUT,
            reconciliation: TOKEN_REFRESH_RECONCILIATION_RESERVE,
        }
    }
}

#[derive(Clone, Copy)]
struct TokenRefreshDeadlines {
    work: tokio::time::Instant,
    total: tokio::time::Instant,
    coordination: tokio::time::Instant,
}

impl TokenRefreshBudgets {
    fn deadlines(self) -> anyhow::Result<TokenRefreshDeadlines> {
        if self.reconciliation > self.workflow {
            return Err(RefreshFailure::new(
                RefreshFailureStage::Internal,
                RefreshFailureKind::InvalidConfiguration,
                None,
                None,
                false,
            )
            .into());
        }
        let now = tokio::time::Instant::now();
        let work_budget = self.workflow.saturating_sub(self.reconciliation);
        Ok(TokenRefreshDeadlines {
            work: now + work_budget,
            total: now + self.workflow,
            coordination: now + self.coordination.min(work_budget),
        })
    }
}

async fn run_refresh_step_until<T>(
    operation: &'static str,
    deadline: tokio::time::Instant,
    future: impl Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    // Keep the permit-owning refresh future in the request task. Dropping a request must drop the
    // HTTP future and its auxiliary concurrency permit synchronously; aborting an unjoined child
    // task only schedules that cleanup and can transiently strand the shared permit. Potentially
    // blocking client construction is already isolated by AuxiliaryRuntime::refresh_client.
    match tokio::time::timeout_at(deadline, AssertUnwindSafe(future).catch_unwind()).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => {
            tracing::warn!(operation, "token refresh task terminated unexpectedly");
            Err(RefreshFailure::new(
                RefreshFailureStage::Internal,
                RefreshFailureKind::Internal,
                None,
                None,
                true,
            )
            .into())
        }
        Err(_) => {
            tracing::warn!(operation, "token refresh step exceeded its deadline");
            Err(RefreshFailure::new(
                RefreshFailureStage::Internal,
                RefreshFailureKind::Timeout,
                None,
                None,
                true,
            )
            .into())
        }
    }
}

async fn refresh_token_until(
    credentials: &KiroCredentials,
    config: &Config,
    client: Arc<reqwest::Client>,
    admission: Option<RefreshSendAdmission>,
    deadline: tokio::time::Instant,
    operation: &'static str,
) -> anyhow::Result<KiroCredentials> {
    let credentials = credentials.clone();
    let config = config.clone();
    run_refresh_step_until(operation, deadline, async move {
        refresh_token_with_client(&credentials, &config, client, admission).await
    })
    .await
}

struct RedisRefreshLockGuard {
    redis: Arc<RedisStore>,
    credential_id: u64,
    lock_token: Option<String>,
}

impl RedisRefreshLockGuard {
    fn new(redis: Arc<RedisStore>, credential_id: u64, lock_token: String) -> Self {
        Self {
            redis,
            credential_id,
            lock_token: Some(lock_token),
        }
    }

    async fn release(mut self) {
        let Some(lock_token) = self.lock_token.as_deref() else {
            return;
        };
        match tokio::time::timeout(
            REFRESH_REDIS_LOCK_OP_TIMEOUT,
            self.redis
                .release_refresh_lock(self.credential_id, lock_token),
        )
        .await
        {
            Ok(Ok(true)) => {
                self.lock_token.take();
            }
            Ok(Ok(false)) => {
                tracing::warn!(
                    credential_id = self.credential_id,
                    "Redis Token 刷新锁已过期或不再属于本实例"
                );
                self.lock_token.take();
            }
            Ok(Err(err)) => {
                tracing::warn!(
                    credential_id = self.credential_id,
                    "释放 Redis Token 刷新锁失败，将提交 critical cleanup: {}",
                    err
                );
            }
            Err(_) => tracing::warn!(
                credential_id = self.credential_id,
                "释放 Redis Token 刷新锁超过 {}ms，将提交 critical cleanup",
                REFRESH_REDIS_LOCK_OP_TIMEOUT.as_millis()
            ),
        }
    }
}

impl Drop for RedisRefreshLockGuard {
    fn drop(&mut self) {
        let Some(lock_token) = self.lock_token.take() else {
            return;
        };
        let redis = self.redis.clone();
        let credential_id = self.credential_id;
        let accepted = spawn_critical_storage_task("释放取消中的 Redis Token 刷新锁", async move {
            let released = tokio::time::timeout(
                REFRESH_REDIS_LOCK_OP_TIMEOUT,
                redis.release_refresh_lock(credential_id, &lock_token),
            )
            .await
            .map_err(|_| anyhow::anyhow!("取消清理 Redis Token 刷新锁超时"))??;
            if !released {
                tracing::debug!(credential_id, "取消清理时 Redis Token 刷新锁已过期或已转移");
            }
            Ok(())
        });
        if !accepted {
            tracing::error!(
                credential_id,
                "critical storage lane 拒绝 Redis Token 刷新锁 cleanup；等待 TTL 兜底"
            );
        }
    }
}

struct RedisRefreshLeaseDropGuard {
    redis: Arc<RedisStore>,
    postgres: Option<Arc<PostgresStore>>,
    lease: Option<RedisRefreshLease>,
    send_started: Arc<AtomicBool>,
    persisted_storage_revision: Arc<AtomicU64>,
    rejected_access_token: Option<String>,
}

impl RedisRefreshLeaseDropGuard {
    fn new(
        redis: Arc<RedisStore>,
        postgres: Option<Arc<PostgresStore>>,
        lease: RedisRefreshLease,
        send_started: Arc<AtomicBool>,
        persisted_storage_revision: Arc<AtomicU64>,
        rejected_access_token: Option<String>,
    ) -> Self {
        Self {
            redis,
            postgres,
            lease: Some(lease),
            send_started,
            persisted_storage_revision,
            rejected_access_token,
        }
    }

    fn disarm(&mut self) {
        self.lease.take();
    }
}

impl Drop for RedisRefreshLeaseDropGuard {
    fn drop(&mut self) {
        let Some(lease) = self.lease.take() else {
            return;
        };
        let redis = self.redis.clone();
        let postgres = self.postgres.clone();
        let send_committed = self.send_started.load(Ordering::Acquire);
        let mut persisted_storage_revision =
            self.persisted_storage_revision.load(Ordering::Acquire);
        let rejected_access_token = self.rejected_access_token.take();
        let credential_id = lease.credential_id;
        let refresh_generation = lease.generation;
        let accepted =
            spawn_critical_storage_task("提交取消中的 Redis Token 刷新 outcome", async move {
                if !send_committed {
                    let _ = tokio::time::timeout(
                        REFRESH_REDIS_LOCK_OP_TIMEOUT,
                        redis.cancel_token_refresh(&lease),
                    )
                    .await
                    .map_err(|_| anyhow::anyhow!("取消未发送的 Redis Token 刷新 leader 超时"))??;
                    return Ok(());
                }
                if persisted_storage_revision == 0 && send_committed {
                    if let Some(postgres) = postgres {
                        if let Ok(Ok(credentials)) = tokio::time::timeout(
                            CREDENTIAL_PGSQL_SYNC_TIMEOUT,
                            postgres.load_credentials(),
                        )
                        .await
                        {
                            if let Some(authoritative) =
                                credentials.into_iter().find(|credential| {
                                    let token_changed =
                                        rejected_access_token.as_deref().map_or_else(
                                            || credential.access_token.is_some(),
                                            |rejected| {
                                                credential.access_token.as_deref() != Some(rejected)
                                            },
                                        );
                                    credential.id == Some(credential_id) && token_changed
                                })
                            {
                                persisted_storage_revision = authoritative.storage_revision;
                            }
                        }
                    }
                }
                if persisted_storage_revision > 0 {
                    let _ = tokio::time::timeout(
                        REFRESH_REDIS_LOCK_OP_TIMEOUT,
                        redis.complete_token_refresh_success(&lease, persisted_storage_revision),
                    )
                    .await
                    .map_err(|_| anyhow::anyhow!("取消清理 Redis Token 刷新 success 超时"))??;
                    return Ok(());
                }
                let failure = RedisRefreshFailure {
                    stage: RedisRefreshFailureStage::Internal,
                    kind: RedisRefreshFailureKind::Internal,
                    status: None,
                    retry_after: None,
                    send_committed,
                    health_action_required: false,
                };
                let _ = tokio::time::timeout(
                    REFRESH_REDIS_LOCK_OP_TIMEOUT,
                    redis.complete_token_refresh_failure(&lease, &failure, false),
                )
                .await
                .map_err(|_| anyhow::anyhow!("取消清理 Redis Token 刷新 outcome 超时"))??;
                Ok(())
            });
        if !accepted {
            tracing::error!(
                credential_id,
                refresh_generation,
                send_committed,
                "Critical cleanup lane rejected a cancelled Redis token refresh outcome"
            );
        }
    }
}

#[derive(Clone)]
struct PendingCredentialStatsBatch {
    operation_id: uuid::Uuid,
    deltas: HashMap<u64, CredentialStatsDeltaRow>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PendingPersistenceBacklog {
    stats_batches: usize,
    stats_deltas: usize,
    runtime_mutations: usize,
}

impl PendingPersistenceBacklog {
    fn is_empty(self) -> bool {
        self.stats_batches == 0 && self.stats_deltas == 0 && self.runtime_mutations == 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatsBatchFlushOutcome {
    NoWork,
    Persisted,
    Failed,
}

impl PendingCredentialRuntimeMutation {
    fn operation_id(&self) -> uuid::Uuid {
        match self {
            Self::Success { operation_id, .. }
            | Self::ApiFailure { operation_id, .. }
            | Self::Disable { operation_id, .. }
            | Self::Patch { operation_id, .. } => *operation_id,
            #[cfg(test)]
            Self::RefreshFailure { operation_id, .. } => *operation_id,
        }
    }

    fn requires_dispatch_quarantine(&self) -> bool {
        match self {
            Self::Disable { .. } => true,
            Self::Patch { patch, .. } => {
                patch.credential_disabled.is_some()
                    || !matches!(
                        patch.disabled_reason,
                        CredentialRuntimeDisabledReasonPatch::Preserve
                    )
                    || patch.advance_generation
            }
            Self::Success { .. } | Self::ApiFailure { .. } => false,
            #[cfg(test)]
            Self::RefreshFailure { .. } => false,
        }
    }

    fn coalesce_into_previous(&self, previous: &mut Self) -> bool {
        match (previous, self) {
            (
                Self::Success {
                    expected_generation: previous_generation,
                    success_count: previous_success_count,
                    ..
                },
                Self::Success {
                    expected_generation,
                    success_count,
                    ..
                },
            ) if previous_generation == expected_generation => {
                *previous_success_count =
                    previous_success_count.saturating_add((*success_count).max(1));
                true
            }
            _ => false,
        }
    }
}

async fn credential_pgsql_sync_with_timeout<T: Send>(
    operation: &'static str,
    timeout: StdDuration,
    future: impl Future<Output = anyhow::Result<T>> + Send,
) -> anyhow::Result<T> {
    tokio::time::timeout(timeout, future)
        .await
        .map_err(|_| anyhow::anyhow!("{}超过 {}ms", operation, timeout.as_millis()))?
}

async fn credential_pgsql_sync_until<T: Send>(
    operation: &'static str,
    deadline: tokio::time::Instant,
    future: impl Future<Output = anyhow::Result<T>> + Send,
) -> anyhow::Result<T> {
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() {
        anyhow::bail!("{}超过工作流总期限", operation);
    }
    credential_pgsql_sync_with_timeout(
        operation,
        remaining.min(CREDENTIAL_PGSQL_SYNC_TIMEOUT),
        future,
    )
    .await
}

async fn acquire_refresh_lock_until<'a>(
    lock: &'a TokioMutex<()>,
    deadline: tokio::time::Instant,
    credential_id: u64,
) -> anyhow::Result<tokio::sync::MutexGuard<'a, ()>> {
    tokio::time::timeout_at(deadline, lock.lock())
        .await
        .map_err(|_| {
            tracing::warn!(credential_id, "local token refresh lock wait timed out");
            anyhow::Error::new(RefreshFailure::new(
                RefreshFailureStage::Coordination,
                RefreshFailureKind::Timeout,
                None,
                None,
                false,
            ))
        })
}

#[cfg(test)]
fn refresh_step_timeout(deadline: tokio::time::Instant, cap: StdDuration) -> Option<StdDuration> {
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    (!remaining.is_zero()).then_some(remaining.min(cap))
}

#[derive(Debug, Default, PartialEq, Eq)]
struct RuntimeMutationCleanupReport {
    removed: u64,
    batches: usize,
    saturated: bool,
}

async fn cleanup_runtime_mutation_history_batches(
    store: Arc<PostgresStore>,
    retention: StdDuration,
    batch_limit: usize,
    max_batches: usize,
    budget: StdDuration,
) -> anyhow::Result<RuntimeMutationCleanupReport> {
    cleanup_runtime_mutation_history_batches_with(batch_limit, max_batches, budget, move || {
        let store = store.clone();
        async move {
            store
                .cleanup_credential_runtime_mutations(retention, batch_limit)
                .await
        }
    })
    .await
}

async fn cleanup_runtime_mutation_history_batches_with<F, Fut>(
    batch_limit: usize,
    max_batches: usize,
    budget: StdDuration,
    mut cleanup_batch: F,
) -> anyhow::Result<RuntimeMutationCleanupReport>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = anyhow::Result<u64>> + Send,
{
    if batch_limit == 0 || max_batches == 0 || budget.is_zero() {
        return Ok(RuntimeMutationCleanupReport::default());
    }
    let deadline = tokio::time::Instant::now() + budget;
    let mut report = RuntimeMutationCleanupReport::default();
    while report.batches < max_batches {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            report.saturated = true;
            break;
        }
        let removed = credential_pgsql_sync_with_timeout(
            "清理 PgSQL 凭据 mutation 幂等记录",
            remaining.min(CREDENTIAL_PGSQL_SYNC_TIMEOUT),
            cleanup_batch(),
        )
        .await?;
        report.batches += 1;
        report.removed = report.removed.saturating_add(removed);
        if removed < batch_limit as u64 {
            return Ok(report);
        }
    }
    report.saturated = true;
    Ok(report)
}

fn block_on_credential_pgsql<T: Send>(
    operation: &'static str,
    future: impl Future<Output = anyhow::Result<T>> + Send,
) -> anyhow::Result<T> {
    block_on_storage(operation, async move {
        credential_pgsql_sync_with_timeout(operation, CREDENTIAL_PGSQL_SYNC_TIMEOUT, future).await
    })
}

fn initial_in_flight_lease_id(redis_enabled: bool) -> u64 {
    if !redis_enabled {
        return 1;
    }
    let seq = LOCAL_REDIS_LEASE_NAMESPACE_SEQ.fetch_add(1, Ordering::Relaxed);
    let now = Utc::now()
        .timestamp_nanos_opt()
        .map(|value| value as u64)
        .unwrap_or_else(|| Utc::now().timestamp_micros() as u64);
    let pid = std::process::id() as u64;
    let mixed = now ^ pid.rotate_left(17) ^ seq.rotate_left(33);
    let namespace = mixed % LOCAL_REDIS_LEASE_NAMESPACE_COUNT + 1;
    namespace.saturating_mul(LOCAL_REDIS_LEASE_COUNTER_SPACE)
}

impl MultiTokenManager {
    /// 创建多凭据 Token 管理器
    ///
    /// # Arguments
    /// * `config` - 应用配置
    /// * `credentials` - 凭据列表
    /// * `proxy` - 可选的代理配置
    /// * `credentials_path` - 已废弃，保留参数用于测试和调用兼容；生产不写凭据文件
    /// * `is_multiple_format` - 已废弃，保留参数用于测试和调用兼容
    #[cfg(test)]
    pub fn new(
        config: Config,
        credentials: Vec<KiroCredentials>,
        proxy: Option<ProxyConfig>,
        credentials_path: Option<PathBuf>,
        is_multiple_format: bool,
    ) -> anyhow::Result<Self> {
        let _ = (credentials_path, is_multiple_format);
        Self::new_with_postgres_store(config, credentials, proxy, None, false, None)
    }

    #[cfg(test)]
    pub fn new_with_postgres_store(
        config: Config,
        credentials: Vec<KiroCredentials>,
        proxy: Option<ProxyConfig>,
        credentials_path: Option<PathBuf>,
        is_multiple_format: bool,
        postgres_store: Option<Arc<PostgresStore>>,
    ) -> anyhow::Result<Self> {
        Self::new_with_stores(
            config,
            credentials,
            proxy,
            credentials_path,
            is_multiple_format,
            postgres_store,
            None,
        )
    }

    #[cfg(test)]
    pub fn new_with_stores(
        config: Config,
        credentials: Vec<KiroCredentials>,
        proxy: Option<ProxyConfig>,
        credentials_path: Option<PathBuf>,
        is_multiple_format: bool,
        postgres_store: Option<Arc<PostgresStore>>,
        redis_store: Option<Arc<RedisStore>>,
    ) -> anyhow::Result<Self> {
        let _ = (credentials_path, is_multiple_format);
        Self::new_with_stores_and_runtime_state(
            config,
            credentials,
            proxy,
            postgres_store,
            redis_store,
            None,
        )
    }

    pub(crate) fn new_with_stores_and_runtime_state(
        config: Config,
        mut credentials: Vec<KiroCredentials>,
        proxy: Option<ProxyConfig>,
        postgres_store: Option<Arc<PostgresStore>>,
        redis_store: Option<Arc<RedisStore>>,
        initial_runtime_states: Option<HashMap<u64, CredentialRuntimeStateRow>>,
    ) -> anyhow::Result<Self> {
        validate_token_refresh_admission_config(&config)?;
        if credentials.iter().any(|credential| credential.id.is_none()) {
            if let Some(store) = &postgres_store {
                let store = store.clone();
                credentials =
                    block_on_credential_pgsql("通过 PgSQL 为无 ID 凭据分配 ID", async move {
                        let mut resolved = Vec::with_capacity(credentials.len());
                        for credential in credentials {
                            if credential.id.is_some() {
                                resolved.push(credential);
                            } else {
                                resolved.push(store.insert_credential(&credential).await?);
                            }
                        }
                        Ok(resolved)
                    })?;
            }
        }

        // 计算当前最大 ID，为没有 ID 的凭据分配新 ID
        let max_existing_id = credentials.iter().filter_map(|c| c.id).max().unwrap_or(0);
        let mut next_id = max_existing_id + 1;
        let mut has_new_ids = false;
        let mut has_new_machine_ids = false;
        let config_ref = &config;

        let entries: Vec<CredentialEntry> = credentials
            .into_iter()
            .map(|mut cred| {
                cred.canonicalize_auth_method();
                cred.normalize_supported_models();
                cred.normalize_api_key_defaults();
                cred.normalize_external_idp_defaults();
                let id = cred.id.unwrap_or_else(|| {
                    let id = next_id;
                    next_id += 1;
                    cred.id = Some(id);
                    has_new_ids = true;
                    id
                });
                if cred.machine_id.is_none() {
                    cred.machine_id =
                        Some(machine_id::generate_from_credentials(&cred, config_ref));
                    has_new_machine_ids = true;
                }
                CredentialEntry {
                    id,
                    credentials: cred.clone(),
                    failure_count: 0,
                    refresh_failure_count: 0,
                    runtime_revision: 0,
                    runtime_generation: 0,
                    runtime_persistence_degraded: false,
                    runtime_persistence_quarantined: false,
                    disabled: cred.disabled, // 从配置文件读取 disabled 状态
                    disabled_reason: if cred.disabled {
                        Some(DisabledReason::Manual)
                    } else {
                        None
                    },
                    success_count: 0,
                    total_selection_count: 0,
                    last_used_at: None,
                    cooldown_until: None,
                    cooldown_reason: None,
                    model_cooldowns: HashMap::new(),
                    rate_limit_available_at: None,
                    in_flight_requests: 0,
                    in_flight_leases: Vec::new(),
                    warmup_remaining: 0,
                    health: SchedulerHealthState::default(),
                    model_health: HashMap::new(),
                    selection_events: VecDeque::new(),
                }
            })
            .collect();

        // 校验 API Key 凭据配置完整性：authMethod=api_key 时必须提供 kiroApiKey
        let mut entries = entries;
        let mut invalid_config_credential_ids = Vec::new();
        for entry in &mut entries {
            if entry.credentials.kiro_api_key.is_none()
                && entry
                    .credentials
                    .auth_method
                    .as_deref()
                    .map(|m| m.eq_ignore_ascii_case("api_key") || m.eq_ignore_ascii_case("apikey"))
                    .unwrap_or(false)
            {
                tracing::warn!(
                    "凭据 #{} 配置了 authMethod=api_key 但缺少 kiroApiKey 字段，已自动禁用",
                    entry.id
                );
                entry.disabled = true;
                entry.disabled_reason = Some(DisabledReason::InvalidConfig);
                invalid_config_credential_ids.push(entry.id);
            }
        }

        // 检测重复 ID
        let mut seen_ids = std::collections::HashSet::new();
        let mut duplicate_ids = Vec::new();
        for entry in &entries {
            if !seen_ids.insert(entry.id) {
                duplicate_ids.push(entry.id);
            }
        }
        if !duplicate_ids.is_empty() {
            anyhow::bail!("检测到重复的凭据 ID: {:?}", duplicate_ids);
        }

        // 选择初始凭据：优先级最高（priority 最小）的可用凭据，无可用凭据时为 0
        let initial_id = entries
            .iter()
            .filter(|e| !e.disabled)
            .min_by_key(|e| e.credentials.priority)
            .map(|e| e.id)
            .unwrap_or(0);

        let load_balancing_mode = config.load_balancing_mode.clone();
        let proxy_resources = if let Some(store) = &postgres_store {
            let store = store.clone();
            match block_on_credential_pgsql("从 PgSQL 加载代理资源", async move {
                store.load_proxy_resources().await
            }) {
                Ok(resources) => resources
                    .into_iter()
                    .map(ProxyResourceRuntime::from)
                    .map(|resource| (resource.id, resource))
                    .collect(),
                Err(err) => {
                    tracing::warn!("加载代理资源失败: {}", err);
                    HashMap::new()
                }
            }
        } else {
            HashMap::new()
        };
        let redis_enabled = redis_store.is_some();
        let initial_lease_id = initial_in_flight_lease_id(redis_enabled);
        let scheduler_instance_id: Arc<str> = Arc::from(uuid::Uuid::new_v4().to_string());
        let scheduler_redis_breaker = SchedulerRedisBreaker::new(
            SchedulerRedisBreakerKind::Capacity,
            SCHEDULER_REDIS_MAX_IN_FLIGHT_OPERATIONS,
            initial_lease_id ^ 0x4341_5041_4349_5459,
        );
        let scheduler_redis_snapshot_breaker = SchedulerRedisBreaker::new(
            SchedulerRedisBreakerKind::Snapshot,
            SCHEDULER_REDIS_SNAPSHOT_MAX_IN_FLIGHT_OPERATIONS,
            initial_lease_id ^ 0x534e_4150_5348_4f54,
        );
        let scheduler_redis_affinity_breaker = SchedulerRedisBreaker::new(
            SchedulerRedisBreakerKind::Affinity,
            SCHEDULER_REDIS_AFFINITY_MAX_IN_FLIGHT_OPERATIONS,
            initial_lease_id ^ 0x4146_4649_4e49_5459,
        );
        let scheduler_redis_release_dispatcher = redis_store.as_ref().map(|redis| {
            SchedulerRedisReleaseDispatcher::new(redis.clone(), scheduler_instance_id.clone())
        });
        let auxiliary_runtime =
            AuxiliaryRuntime::new(&config, proxy.as_ref(), postgres_store.is_some())?;
        auxiliary_runtime.set_token_refresh_redis_store(redis_store.clone());
        let manager = Self {
            config: Mutex::new(config),
            proxy,
            entries: Arc::new(Mutex::new(entries)),
            current_id: Mutex::new(initial_id),
            refresh_states: Mutex::new(HashMap::new()),
            auxiliary_runtime,
            postgres_store,
            redis_store,
            proxy_resources: Arc::new(Mutex::new(proxy_resources)),
            load_balancing_mode: Mutex::new(load_balancing_mode),
            last_stats_save_at: Mutex::new(None),
            last_runtime_mutation_cleanup_at: Mutex::new(None),
            last_scheduler_redis_sync_at: Arc::new(Mutex::new(None)),
            last_scheduler_redis_cleanup_at: Mutex::new(None),
            scheduler_redis_breaker,
            scheduler_redis_snapshot_breaker,
            scheduler_redis_affinity_breaker,
            scheduler_redis_release_dispatcher,
            #[cfg(test)]
            request_binding_snapshot_reads: AtomicU64::new(0),
            scheduler_redis_sync_in_flight: Arc::new(AtomicBool::new(false)),
            scheduler_redis_full_sync_requested: Arc::new(AtomicU64::new(0)),
            scheduler_redis_full_sync_applied: Arc::new(AtomicU64::new(0)),
            scheduler_redis_dirty_credential_ids: Arc::new(Mutex::new(HashSet::new())),
            scheduler_instance_id,
            stats_dirty: AtomicBool::new(false),
            pending_stats_deltas: Mutex::new(HashMap::new()),
            pending_stats_batches: Mutex::new(VecDeque::new()),
            pending_runtime_mutations: Mutex::new(HashMap::new()),
            last_runtime_success_reconcile_probe_at: Mutex::new(HashMap::new()),
            #[cfg(test)]
            runtime_success_reconcile_probe_attempts: AtomicU64::new(0),
            overflow_runtime_mutations: AtomicU64::new(0),
            runtime_mutation_flush_cursor: AtomicU64::new(0),
            session_bindings: Mutex::new(HashMap::new()),
            capacity_signal: Arc::new(CapacitySignal::default()),
            local_pool_risk_circuit: Mutex::new(LocalPoolRiskCircuit::default()),
            released_in_flight_lease_tombstones: Arc::new(Mutex::new(HashMap::new())),
            next_in_flight_lease_id: AtomicU64::new(initial_lease_id),
            queued_requests: Arc::new(AtomicU32::new(0)),
            selection_reservation_gate: Mutex::new(()),
            model_capability_cohort_generation: AtomicU64::new(1),
            model_capability_cohort_cache: Mutex::new(ModelCapabilityCohortCache::default()),
        };

        // 如果有新分配的 ID 或新生成的 machineId，立即持久化到 PgSQL。
        if has_new_ids || has_new_machine_ids {
            if let Err(e) = manager.persist_credentials() {
                tracing::warn!("补全凭据 ID/machineId 后持久化失败: {}", e);
            } else {
                tracing::info!("已补全凭据 ID/machineId 并写入数据库");
            }
        }

        // 加载持久化的统计数据（success_count, last_used_at）
        manager.load_stats();
        if let Some(states) = initial_runtime_states {
            manager.apply_loaded_runtime_states(&states);
        } else {
            manager.load_runtime_state();
        }
        manager.select_highest_priority();
        manager.refresh_scheduler_state_from_redis_force_best_effort();
        for id in invalid_config_credential_ids {
            manager.record_scheduler_credential_audit(
                "auto_disable_credential",
                id,
                DisabledReason::InvalidConfig,
                "startup_invalid_config",
                "凭据配置无效，启动时已自动禁用",
                serde_json::json!({
                    "configError": "api_key_auth_without_kiro_api_key",
                }),
            );
        }

        Ok(manager)
    }

    /// 获取当前运行时配置快照。
    pub fn runtime_config(&self) -> Config {
        self.config.lock().clone()
    }

    fn runtime_capacity_change_should_reset_warmup(previous: &Config, current: &Config) -> bool {
        previous.credential_rpm != current.credential_rpm
            || previous.credential_max_concurrent_requests
                != current.credential_max_concurrent_requests
            || previous.dispatch_global_max_concurrent_requests
                != current.dispatch_global_max_concurrent_requests
            || previous.weighted_capacity != current.weighted_capacity
            || previous.load_balancing_mode != current.load_balancing_mode
            || previous.credential_warmup_requests != current.credential_warmup_requests
            || previous.credential_warmup_selection_percent
                != current.credential_warmup_selection_percent
            || previous.credential_warmup_max_selection_percent
                != current.credential_warmup_max_selection_percent
    }

    fn reset_active_warmup_after_runtime_capacity_change(
        &self,
        previous: &Config,
        current: &Config,
        reason: &'static str,
    ) -> usize {
        if current.credential_warmup_requests == 0
            || !Self::runtime_capacity_change_should_reset_warmup(previous, current)
        {
            return 0;
        }
        let warmup_remaining = current.credential_warmup_requests;
        let mut entries = self.entries.lock();
        let mut reset = 0usize;
        for entry in entries.iter_mut().filter(|entry| !entry.disabled) {
            if entry.warmup_remaining != warmup_remaining {
                entry.warmup_remaining = warmup_remaining;
                reset += 1;
            }
        }
        if reset > 0 {
            tracing::info!(
                reason,
                reset,
                warmup_remaining,
                "运行时调度容量配置变化，已重置本进程活跃凭据 warmup"
            );
        }
        reset
    }

    pub(crate) fn auxiliary_concurrency_controller(&self) -> Arc<AuxiliaryConcurrencyController> {
        self.auxiliary_runtime.controller()
    }

    pub(crate) async fn refresh_http_client(
        &self,
        tls_backend: crate::model::config::TlsBackend,
        proxy: Option<&ProxyConfig>,
    ) -> anyhow::Result<Arc<reqwest::Client>> {
        self.auxiliary_runtime
            .refresh_client(tls_backend, proxy)
            .await
    }

    pub(crate) fn auxiliary_concurrency_snapshot(&self) -> AuxiliaryConcurrencySnapshot {
        self.auxiliary_runtime.concurrency_snapshot()
    }

    pub(crate) fn refresh_client_cache_snapshot(&self) -> RefreshClientCacheSnapshot {
        self.auxiliary_runtime.refresh_client_cache_snapshot()
    }

    pub(crate) fn token_refresh_admission_snapshot(&self) -> TokenRefreshAdmissionSnapshot {
        self.auxiliary_runtime.token_refresh_admission_snapshot()
    }

    pub(crate) async fn drain_scheduler_redis_releases(&self, timeout: StdDuration) -> bool {
        for (breaker, stats) in [
            ("capacity", self.scheduler_redis_breaker.stats_snapshot()),
            (
                "snapshot",
                self.scheduler_redis_snapshot_breaker.stats_snapshot(),
            ),
            (
                "affinity",
                self.scheduler_redis_affinity_breaker.stats_snapshot(),
            ),
        ] {
            tracing::info!(
                breaker,
                admitted = stats.admitted,
                local_saturated = stats.local_saturated,
                fail_fast = stats.fail_fast,
                recovery_probes = stats.recovery_probes,
                failures = stats.failures,
                recoveries = stats.recoveries,
                cancelled_probes = stats.cancelled_probes,
                stale_successes = stats.stale_successes,
                stale_failures = stats.stale_failures,
                suppressed = stats.suppressed,
                "Redis scheduler breaker 累计指标"
            );
        }
        match &self.scheduler_redis_release_dispatcher {
            Some(dispatcher) => {
                let drained = dispatcher.drain(timeout).await;
                let snapshot = dispatcher.snapshot();
                tracing::info!(
                    drained,
                    pending = snapshot.pending,
                    capacity_available = snapshot.capacity_available,
                    enqueued = snapshot.enqueued,
                    completed = snapshot.completed,
                    retries = snapshot.retries,
                    worker_starts = snapshot.worker_starts,
                    spawn_failures = snapshot.spawn_failures,
                    saturated = snapshot.saturated,
                    "Redis scheduler release reconciliation 排空阶段已结束"
                );
                drained
            }
            None => true,
        }
    }

    pub fn current_id(&self) -> u64 {
        *self.current_id.lock()
    }

    /// 获取请求热路径使用的账号展示名。
    ///
    /// 这里只读取内存中的凭据基础字段，不触发 PgSQL/Redis 同步。调用方用于 usage
    /// 记录和日志展示，不能因为展示字段读取把 Admin 完整快照放进模型请求热路径。
    pub fn credential_display_label(&self, id: u64) -> Option<String> {
        let entries = self.entries.lock();
        entries
            .iter()
            .find(|entry| entry.id == id)
            .and_then(|entry| {
                entry
                    .credentials
                    .email
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .or_else(|| entry.credentials.kiro_api_key.as_deref().map(mask_api_key))
                    .or_else(|| {
                        entry
                            .credentials
                            .endpoint
                            .as_ref()
                            .map(|endpoint| format!("#{} {}", id, endpoint))
                    })
            })
    }

    /// 获取错误响应热路径使用的 Retry-After 提示。
    ///
    /// 该方法只基于本进程内存中的冷却状态计算，不同步 PgSQL 统计或 Redis 调度运行态。
    /// 调度决策本身仍由 acquire 路径负责；这里仅用于构造下游响应头。
    pub fn cooldown_retry_after_hint_secs(&self, fallback_secs: u64) -> u64 {
        self.cleanup_expired_in_flight_leases_local_first();
        let fallback_secs = fallback_secs.max(1);
        let now = Instant::now();
        let entries = self.entries.lock();
        entries
            .iter()
            .filter(|entry| !entry.disabled)
            .filter_map(|entry| {
                entry_any_cooldown_remaining(entry, now)
                    .map(|duration| duration.as_secs().saturating_add(1))
            })
            .min()
            .unwrap_or(fallback_secs)
            .max(1)
    }

    fn credential_audit_label(entry: &CredentialEntry) -> String {
        let prefix = format!("#{}", entry.id);
        let label = entry
            .credentials
            .email
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| entry.credentials.kiro_api_key.as_deref().map(mask_api_key))
            .or_else(|| entry.credentials.endpoint.clone());

        match label {
            Some(label) if label.starts_with(&prefix) => label,
            Some(label) => format!("{} {}", prefix, label),
            None => prefix,
        }
    }

    fn record_scheduler_credential_audit(
        &self,
        action: &'static str,
        id: u64,
        reason: DisabledReason,
        trigger: &'static str,
        message: &'static str,
        extra_detail: serde_json::Value,
    ) {
        let Some(store) = self.postgres_store.clone() else {
            return;
        };

        let current_id = *self.current_id.lock();
        let (
            label,
            auth_method,
            endpoint,
            disabled,
            failure_count,
            refresh_failure_count,
            available,
        ) = {
            let entries = self.entries.lock();
            let available = entries.iter().filter(|entry| !entry.disabled).count();
            match entries.iter().find(|entry| entry.id == id) {
                Some(entry) => (
                    Self::credential_audit_label(entry),
                    entry.credentials.auth_method.clone(),
                    entry.credentials.endpoint.clone(),
                    entry.disabled,
                    entry.failure_count,
                    entry.refresh_failure_count,
                    available,
                ),
                None => (format!("#{}", id), None, None, false, 0, 0, available),
            }
        };

        let mut detail = serde_json::json!({
            "credentialId": id,
            "credentialLabel": label,
            "reason": reason.as_str(),
            "reasonLabel": reason.label(),
            "trigger": trigger,
            "message": message,
            "disabled": disabled,
            "failureCount": failure_count,
            "refreshFailureCount": refresh_failure_count,
            "maxFailures": MAX_FAILURES_PER_CREDENTIAL,
            "availableCredentials": available,
            "currentCredentialId": current_id,
            "source": "scheduler",
        });
        if let Some(auth_method) = auth_method {
            detail["authMethod"] = serde_json::Value::String(auth_method);
        }
        if let Some(endpoint) = endpoint {
            detail["endpoint"] = serde_json::Value::String(endpoint);
        }
        merge_json_object(&mut detail, extra_detail);

        let object_id = id.to_string();
        spawn_best_effort_storage_task("记录调度审计日志", async move {
            store
                .record_admin_audit_log(
                    "system-scheduler",
                    action,
                    "credential",
                    Some(&object_id),
                    true,
                    None,
                    detail,
                )
                .await
        });
    }

    /// 更新当前运行时配置并写入 PgSQL。
    pub fn update_runtime_config(&self, update: impl FnOnce(&mut Config)) -> anyhow::Result<()> {
        let previous_config = self.runtime_config();
        let mut updated = previous_config.clone();
        update(&mut updated);
        updated.set_config_path_for_runtime(None);
        validate_token_refresh_admission_config(&updated)?;

        let mut saved_version: Option<i64> = None;
        if let Some(store) = &self.postgres_store {
            let store = store.clone();
            let to_save = updated.clone();
            saved_version = Some(block_on_credential_pgsql(
                "持久化运行时配置到 PgSQL",
                async move { store.save_runtime_config_returning_version(&to_save).await },
            )?);
        }

        {
            let mut config = self.config.lock();
            *config = updated;
        }
        let config = self.config.lock().clone();
        self.reset_active_warmup_after_runtime_capacity_change(
            &previous_config,
            &config,
            "runtime_config_updated",
        );
        let auxiliary_limit = config.auxiliary_upstream_max_concurrent_requests;
        self.auxiliary_runtime.update_limit(auxiliary_limit);
        self.auxiliary_runtime
            .update_token_refresh_limits(config.token_refresh_max_rpm, config.token_refresh_burst);
        self.update_credential_rpm_from_config();
        self.invalidate_model_capability_cohorts();
        self.notify_dispatch_state_changed();
        self.publish_runtime_config_changed(saved_version, "runtime_config_updated");

        Ok(())
    }

    /// 从 PgSQL 重新加载运行配置。用于 Redis pub/sub 通知或定时兜底检查。
    pub fn reload_runtime_config_from_postgres(&self) -> anyhow::Result<bool> {
        let Some(store) = &self.postgres_store else {
            return Ok(false);
        };
        let store = store.clone();
        let Some(mut config) =
            block_on_credential_pgsql("从 PgSQL 重新加载运行时配置", async move {
                store.load_runtime_config().await
            })?
        else {
            return Ok(false);
        };
        config.set_config_path_for_runtime(None);
        validate_token_refresh_admission_config(&config)?;
        let previous_config = self.runtime_config();
        {
            let mut current = self.config.lock();
            *current = config.clone();
        }
        self.reset_active_warmup_after_runtime_capacity_change(
            &previous_config,
            &config,
            "runtime_config_reloaded",
        );
        self.auxiliary_runtime
            .update_limit(config.auxiliary_upstream_max_concurrent_requests);
        self.auxiliary_runtime
            .update_token_refresh_limits(config.token_refresh_max_rpm, config.token_refresh_burst);
        *self.load_balancing_mode.lock() = config.load_balancing_mode.clone();
        self.update_credential_rpm_from_config();
        self.invalidate_model_capability_cohorts();
        self.notify_dispatch_state_changed();
        Ok(true)
    }

    pub fn update_load_balancing_mode_in_config(&self, mode: &str) {
        self.config.lock().load_balancing_mode = mode.to_string();
    }

    /// 获取凭据总数
    pub fn total_count(&self) -> usize {
        self.entries.lock().len()
    }

    /// 获取可用凭据数量
    pub fn available_count(&self) -> usize {
        self.entries.lock().iter().filter(|e| !e.disabled).count()
    }

    #[cfg(test)]
    pub fn local_pool_route_state(&self, model: Option<&str>) -> LocalPoolRouteState {
        self.local_pool_route_state_fresh(model)
    }

    pub fn local_pool_route_state_fresh(&self, model: Option<&str>) -> LocalPoolRouteState {
        let mut state = self.compute_local_pool_route_state(model, true);
        if state.kind.should_route_external() && self.auto_heal_too_many_failures_if_applicable() {
            state = self.compute_local_pool_route_state(model, true);
        }
        state
    }

    /// Returns a local-memory routing snapshot without performing scheduler Redis reads/probes.
    ///
    /// This is intentionally weaker than `local_pool_route_state_fresh`: it is suitable for
    /// request-entry fail-fast guards that must avoid adding Redis work on an already degraded hot
    /// path. It still preserves the existing TooManyFailures auto-heal behavior so entry-level
    /// fast-fail does not strand a self-healable local pool. The authoritative scheduler state is
    /// still refreshed by normal dispatch/preflight paths before a local upstream call is made.
    pub fn local_pool_route_state_cached(&self, model: Option<&str>) -> LocalPoolRouteState {
        let mut state = self.compute_local_pool_route_state(model, false);
        if state.kind.should_route_external() && self.auto_heal_too_many_failures_if_applicable() {
            state = self.compute_local_pool_route_state(model, false);
        }
        state
    }

    pub fn selection_failure_summary(
        &self,
        request_id: impl Into<String>,
        route: impl Into<String>,
        model: Option<&str>,
        error_message: &str,
    ) -> SelectionFailureSummary {
        let state = self.compute_local_pool_route_state(model, true);
        let (stage, primary_reason) =
            Self::selection_failure_stage_and_reason(&state, error_message);
        let config = self.config.lock().clone();
        let sample_limit = if config.selection_failure_record_enabled {
            config.selection_failure_sample_limit
        } else {
            0
        };
        let (reason_counts, sampled_accounts, rejected_account_count, waitable_account_count) =
            self.selection_failure_account_breakdown(model, sample_limit);
        let retry_after_ms = state.retry_after_secs.map(|secs| secs.saturating_mul(1000));

        SelectionFailureSummary {
            request_id: request_id.into(),
            route: route.into(),
            model: model.unwrap_or("").to_string(),
            stage,
            primary_reason,
            rejected_account_count,
            waitable_account_count,
            retry_after_ms,
            reason_counts,
            sampled_accounts,
            dispatch_wait_ms: None,
            queue_depth: state.queued_requests,
            global_in_flight: state.global_in_flight_requests,
        }
    }

    fn selection_failure_stage_and_reason(
        state: &LocalPoolRouteState,
        error_message: &str,
    ) -> (SelectionFailureStage, AccountRejectReason) {
        if error_message.contains("Redis 调度协调状态不可用") {
            return (
                SelectionFailureStage::DispatchQueue,
                AccountRejectReason::Unknown,
            );
        }
        if error_message.contains("本地账号池风险保护") {
            return (
                SelectionFailureStage::UpstreamPreflight,
                AccountRejectReason::RiskCircuitOpen,
            );
        }
        if error_message.contains("等待队列已满") {
            return (
                SelectionFailureStage::DispatchQueue,
                AccountRejectReason::GlobalConcurrencyFull,
            );
        }
        if error_message.contains("凭据 RPM 限制") {
            return (
                SelectionFailureStage::RpmLimit,
                AccountRejectReason::RpmLimited,
            );
        }
        if error_message.contains("排队等待超时") {
            return (
                SelectionFailureStage::DispatchWait,
                if state.global_max_concurrent_requests > 0
                    && state.global_in_flight_requests >= state.global_max_concurrent_requests
                {
                    AccountRejectReason::GlobalConcurrencyFull
                } else {
                    AccountRejectReason::AccountConcurrencyFull
                },
            );
        }
        if error_message.contains("临时排除") {
            return (
                SelectionFailureStage::StickyBinding,
                AccountRejectReason::StickyTargetUnavailable,
            );
        }

        match state.kind {
            LocalPoolRouteStateKind::NoCredentials => (
                SelectionFailureStage::AccountEligibility,
                AccountRejectReason::NoAccounts,
            ),
            LocalPoolRouteStateKind::AllDisabled => (
                SelectionFailureStage::AccountEligibility,
                AccountRejectReason::Disabled,
            ),
            LocalPoolRouteStateKind::NoModelCompatible => (
                SelectionFailureStage::ModelEligibility,
                AccountRejectReason::ModelNotSupported,
            ),
            LocalPoolRouteStateKind::ProxyBlocked => (
                SelectionFailureStage::RouteValidation,
                AccountRejectReason::ProxyUnavailable,
            ),
            LocalPoolRouteStateKind::AllCoolingDown => {
                if state.rate_limit_blocked >= state.cooldown_blocked {
                    (
                        SelectionFailureStage::RpmLimit,
                        AccountRejectReason::RpmLimited,
                    )
                } else {
                    (
                        SelectionFailureStage::Cooldown,
                        AccountRejectReason::CooldownActive,
                    )
                }
            }
            LocalPoolRouteStateKind::CapacityFull => {
                if state.global_max_concurrent_requests > 0
                    && state.global_in_flight_requests >= state.global_max_concurrent_requests
                {
                    (
                        SelectionFailureStage::GlobalConcurrency,
                        AccountRejectReason::GlobalConcurrencyFull,
                    )
                } else {
                    (
                        SelectionFailureStage::AccountConcurrency,
                        AccountRejectReason::AccountConcurrencyFull,
                    )
                }
            }
            LocalPoolRouteStateKind::SchedulerRedisDegraded => (
                SelectionFailureStage::DispatchQueue,
                AccountRejectReason::Unknown,
            ),
            LocalPoolRouteStateKind::RiskCircuitOpen => (
                SelectionFailureStage::UpstreamPreflight,
                AccountRejectReason::RiskCircuitOpen,
            ),
            LocalPoolRouteStateKind::Ready => (
                SelectionFailureStage::AccountEligibility,
                AccountRejectReason::Unknown,
            ),
        }
    }

    fn selection_failure_account_breakdown(
        &self,
        model: Option<&str>,
        sample_limit: usize,
    ) -> (
        BTreeMap<AccountRejectReason, usize>,
        Vec<RejectedAccountSample>,
        usize,
        usize,
    ) {
        let entries = self.entries.lock();
        let proxy_resources = self.proxy_resources.lock();
        let config = self.config.lock().clone();
        let now = Instant::now();
        let global_in_flight = entries
            .iter()
            .map(|entry| entry.in_flight_requests)
            .sum::<u32>();
        let global_has_capacity = global_has_concurrency_capacity(
            global_in_flight,
            config.dispatch_global_max_concurrent_requests,
            1,
        );
        let global_rpm = config.credential_rpm.unwrap_or(0);
        let risk_circuit_open = self
            .local_pool_risk_circuit_snapshot_from_config(now, &config)
            .open;
        let mut reason_counts: BTreeMap<AccountRejectReason, usize> = BTreeMap::new();
        let mut sampled_accounts = Vec::new();
        let mut rejected_account_count = 0usize;
        let mut waitable_account_count = 0usize;

        for entry in entries.iter() {
            let (reason, cooldown_remaining) = if entry.disabled {
                (AccountRejectReason::Disabled, None)
            } else if !entry.credentials.supports_model(&[model])
                || (is_opus_model(model) && !entry.credentials.supports_opus())
            {
                (AccountRejectReason::ModelNotSupported, None)
            } else if !credential_proxy_is_dispatchable(&entry.credentials, &proxy_resources) {
                (AccountRejectReason::ProxyUnavailable, None)
            } else if risk_circuit_open {
                (AccountRejectReason::RiskCircuitOpen, None)
            } else if let Some(remaining) = entry_cooldown_remaining(entry, model, now) {
                (AccountRejectReason::CooldownActive, Some(remaining))
            } else if entry_rate_limit_remaining(entry, global_rpm, now).is_some() {
                waitable_account_count = waitable_account_count.saturating_add(1);
                (AccountRejectReason::RpmLimited, None)
            } else if !global_has_capacity {
                waitable_account_count = waitable_account_count.saturating_add(1);
                (AccountRejectReason::GlobalConcurrencyFull, None)
            } else if !entry_has_concurrency_capacity(
                entry,
                config.credential_max_concurrent_requests,
                1,
            ) {
                waitable_account_count = waitable_account_count.saturating_add(1);
                (AccountRejectReason::AccountConcurrencyFull, None)
            } else {
                continue;
            };

            rejected_account_count = rejected_account_count.saturating_add(1);
            *reason_counts.entry(reason).or_insert(0) += 1;

            if sampled_accounts.len() < sample_limit {
                sampled_accounts.push(RejectedAccountSample {
                    account_id: entry.id,
                    reason,
                    rpm_limit: Some(effective_rpm(entry, global_rpm)),
                    in_flight: Some(entry.in_flight_requests),
                    max_concurrent: Some(effective_max_concurrent_requests(
                        entry,
                        config.credential_max_concurrent_requests,
                    )),
                    cooldown_remaining_ms: cooldown_remaining
                        .map(|duration| duration.as_millis().min(u64::MAX as u128) as u64),
                });
            }
        }

        (
            reason_counts,
            sampled_accounts,
            rejected_account_count,
            waitable_account_count,
        )
    }

    fn local_pool_risk_circuit_snapshot_from_config(
        &self,
        now: Instant,
        config: &Config,
    ) -> LocalPoolRiskCircuitSnapshot {
        if !config.external_pools.local_pool_circuit_enabled {
            return LocalPoolRiskCircuitSnapshot::default();
        }
        let window =
            StdDuration::from_secs(config.external_pools.local_pool_circuit_window_secs.max(1));
        let mut circuit = self.local_pool_risk_circuit.lock();
        while circuit
            .failures
            .front()
            .is_some_and(|event| now.saturating_duration_since(event.at) > window)
        {
            circuit.failures.pop_front();
        }
        let open_until = circuit.open_until.filter(|until| *until > now);
        if open_until.is_none() {
            circuit.open_until = None;
            circuit.reason = None;
        }
        LocalPoolRiskCircuitSnapshot {
            open: open_until.is_some(),
            retry_after: open_until.map(|until| until.saturating_duration_since(now)),
        }
    }

    fn local_pool_risk_circuit_snapshot(&self, now: Instant) -> LocalPoolRiskCircuitSnapshot {
        let config = self.config.lock().clone();
        self.local_pool_risk_circuit_snapshot_from_config(now, &config)
    }

    fn record_local_pool_risk_circuit_failure(
        &self,
        credential_id: u64,
        reason: CredentialRiskControlReason,
    ) -> LocalPoolRiskCircuitSnapshot {
        let config = self.config.lock().clone();
        if !config.external_pools.local_pool_circuit_enabled {
            return LocalPoolRiskCircuitSnapshot::default();
        }
        let now = Instant::now();
        let window =
            StdDuration::from_secs(config.external_pools.local_pool_circuit_window_secs.max(1));
        let open_for =
            StdDuration::from_secs(config.external_pools.local_pool_circuit_open_secs.max(1));
        let open_after_failures = config
            .external_pools
            .local_pool_circuit_open_after_failures
            .max(1);
        let required_distinct = config
            .external_pools
            .local_pool_circuit_require_distinct_credentials
            .max(1);

        let mut circuit = self.local_pool_risk_circuit.lock();
        while circuit
            .failures
            .front()
            .is_some_and(|event| now.saturating_duration_since(event.at) > window)
        {
            circuit.failures.pop_front();
        }
        circuit.failures.push_back(LocalPoolRiskCircuitEvent {
            at: now,
            credential_id,
        });

        let distinct_credentials = circuit
            .failures
            .iter()
            .map(|event| event.credential_id)
            .collect::<HashSet<_>>()
            .len()
            .min(u32::MAX as usize) as u32;
        let recent_failures = circuit.failures.len().min(u32::MAX as usize) as u32;

        let already_open = circuit.open_until.is_some_and(|until| until > now);
        if already_open {
            if circuit.reason.is_none() {
                circuit.reason = Some(reason);
            }
        } else if recent_failures >= open_after_failures
            && distinct_credentials >= required_distinct
        {
            let open_until = now.checked_add(open_for).unwrap_or(now);
            circuit.open_until = Some(open_until);
            circuit.reason = Some(reason);
            tracing::error!(
                credential_id,
                risk_reason = reason.event_reason(),
                recent_failures,
                distinct_credentials,
                open_secs = open_for.as_secs(),
                "本地账号池风控熔断已打开，暂停继续探测剩余本地账号"
            );
        }

        let open_until = circuit.open_until.filter(|until| *until > now);
        LocalPoolRiskCircuitSnapshot {
            open: open_until.is_some(),
            retry_after: open_until.map(|until| until.saturating_duration_since(now)),
        }
    }

    fn compute_local_pool_route_state(
        &self,
        model: Option<&str>,
        refresh_scheduler_state: bool,
    ) -> LocalPoolRouteState {
        let config = self.config.lock().clone();
        let now = Instant::now();
        let risk_circuit = self.local_pool_risk_circuit_snapshot_from_config(now, &config);
        let scheduler_refresh_result = if refresh_scheduler_state && !risk_circuit.open {
            self.refresh_scheduler_state_from_redis()
        } else {
            Ok(())
        };
        if let Err(err) = scheduler_refresh_result {
            tracing::warn!("本地池路由预检同步 Redis 调度状态失败: {}", err);
        }
        if refresh_scheduler_state {
            self.cleanup_expired_in_flight_leases_local_first();
        } else if let Some(max_age) = self.in_flight_lease_max_age() {
            self.cleanup_expired_local_in_flight_leases(max_age, Instant::now());
        }

        let entries = self.entries.lock();
        let proxy_resources = self.proxy_resources.lock();
        let total = entries.len();
        let available = entries.iter().filter(|entry| !entry.disabled).count();
        let max_concurrent_requests = config.credential_max_concurrent_requests;
        let global_rpm = config.credential_rpm.unwrap_or(0);
        let global_in_flight_requests = entries
            .iter()
            .map(|entry| entry.in_flight_requests)
            .sum::<u32>();
        let global_has_capacity = global_has_concurrency_capacity(
            global_in_flight_requests,
            config.dispatch_global_max_concurrent_requests,
            1,
        );
        if refresh_scheduler_state && !risk_circuit.open {
            self.probe_scheduler_redis_capacity_recovery_if_due();
        }
        let scheduler_redis_retry_after_secs = self.scheduler_redis_degraded_retry_after_secs();
        let mut model_usable = 0usize;
        let mut usable = 0usize;
        let mut proxy_blocked = 0usize;
        let mut dispatchable = 0usize;
        let mut cooldown_blocked = 0usize;
        let mut rate_limit_blocked = 0usize;
        let mut concurrency_blocked = 0usize;
        let mut wait_for: Option<StdDuration> = None;
        let mut effective_concurrency_range: Option<(u32, u32)> = None;

        for entry in entries.iter() {
            if !credential_is_usable_for_model(entry, model) {
                continue;
            }
            model_usable += 1;

            if !credential_proxy_is_dispatchable(&entry.credentials, &proxy_resources) {
                proxy_blocked += 1;
                continue;
            }

            usable += 1;
            let effective_concurrency =
                effective_max_concurrent_requests(entry, max_concurrent_requests);
            effective_concurrency_range = Some(match effective_concurrency_range {
                Some((min, max)) => (
                    min.min(effective_concurrency),
                    max.max(effective_concurrency),
                ),
                None => (effective_concurrency, effective_concurrency),
            });

            let cooldown_remaining = entry_cooldown_remaining(entry, model, now);
            let rate_limit_remaining = entry_rate_limit_remaining(entry, global_rpm, now);

            if let Some(remaining) = cooldown_remaining {
                cooldown_blocked += 1;
                let blocking_wait = rate_limit_remaining
                    .map(|rate_limit| remaining.max(rate_limit))
                    .unwrap_or(remaining);
                wait_for =
                    Some(wait_for.map_or(blocking_wait, |current| current.min(blocking_wait)));
                continue;
            }
            if let Some(remaining) = rate_limit_remaining {
                rate_limit_blocked += 1;
                wait_for = Some(wait_for.map_or(remaining, |current| current.min(remaining)));
                continue;
            }

            if !global_has_capacity
                || !entry_has_concurrency_capacity(entry, max_concurrent_requests, 1)
            {
                concurrency_blocked += 1;
                continue;
            }

            dispatchable += 1;
        }
        if risk_circuit.open {
            dispatchable = 0;
        }

        let dispatch_candidate_count = usable;
        let retry_after_secs = wait_for
            .map(|duration| duration.as_secs().saturating_add(1))
            .or_else(|| {
                risk_circuit
                    .retry_after
                    .map(|duration| duration.as_secs().saturating_add(1).max(1))
            })
            .or(scheduler_redis_retry_after_secs)
            .filter(|value| *value > 0);
        let effective_credential_max_concurrent_requests =
            format_effective_concurrency_range(effective_concurrency_range);

        let kind = if total == 0 {
            LocalPoolRouteStateKind::NoCredentials
        } else if available == 0 {
            LocalPoolRouteStateKind::AllDisabled
        } else if risk_circuit.open {
            LocalPoolRouteStateKind::RiskCircuitOpen
        } else if model.is_some() && model_usable == 0 {
            LocalPoolRouteStateKind::NoModelCompatible
        } else if model_usable > 0 && usable == 0 && proxy_blocked >= model_usable {
            LocalPoolRouteStateKind::ProxyBlocked
        } else if scheduler_redis_retry_after_secs.is_some() {
            LocalPoolRouteStateKind::SchedulerRedisDegraded
        } else if dispatchable > 0 {
            LocalPoolRouteStateKind::Ready
        } else if dispatch_candidate_count > 0
            && cooldown_blocked.saturating_add(rate_limit_blocked) >= dispatch_candidate_count
        {
            LocalPoolRouteStateKind::AllCoolingDown
        } else {
            LocalPoolRouteStateKind::CapacityFull
        };

        LocalPoolRouteState {
            kind,
            total,
            available,
            model_usable,
            usable,
            dispatchable,
            proxy_blocked,
            cooldown_blocked,
            rate_limit_blocked,
            concurrency_blocked,
            global_in_flight_requests,
            global_max_concurrent_requests: config.dispatch_global_max_concurrent_requests,
            global_credential_max_concurrent_requests: max_concurrent_requests,
            effective_credential_max_concurrent_requests,
            queued_requests: self.queued_requests.load(Ordering::Relaxed),
            max_queued_requests: config.dispatch_max_queued_requests,
            retry_after_secs,
        }
    }

    fn auto_heal_too_many_failures_if_applicable(&self) -> bool {
        let candidate_ids = {
            let entries = self.entries.lock();
            if !entries.iter().any(|entry| {
                entry.disabled
                    && entry.disabled_reason == Some(DisabledReason::TooManyFailures)
                    && !entry.runtime_persistence_degraded
            }) {
                return false;
            }

            tracing::warn!(
                "所有可调度凭据均不可用且存在连续失败自动禁用凭据，执行自愈：重置失败计数并重新启用"
            );

            entries
                .iter()
                .filter(|entry| {
                    entry.disabled
                        && entry.disabled_reason == Some(DisabledReason::TooManyFailures)
                        && !entry.runtime_persistence_degraded
                })
                .map(|entry| entry.id)
                .collect::<Vec<_>>()
        };

        let mut healed_ids = Vec::new();
        for id in candidate_ids {
            let persisted_state = if let Some(store) = &self.postgres_store {
                let store = store.clone();
                match block_on_credential_pgsql("原子恢复 PgSQL 连续失败禁用凭据", async move {
                    store.heal_credential_api_failures(id).await
                }) {
                    Ok(Some(state)) => Some(state),
                    Ok(None) => continue,
                    Err(err) => {
                        tracing::warn!(credential_id = id, "{}", err);
                        continue;
                    }
                }
            } else {
                None
            };
            let mut entries = self.entries.lock();
            let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) else {
                continue;
            };
            if entry.disabled_reason != Some(DisabledReason::TooManyFailures) {
                continue;
            }
            if let Some(state) = persisted_state.as_ref() {
                if state.revision <= entry.runtime_revision {
                    continue;
                }
                entry.credentials.disabled = false;
                Self::apply_runtime_state_if_newer(entry, state);
            } else {
                entry.failure_count = 0;
                entry.credentials.disabled = false;
                entry.disabled = false;
                entry.disabled_reason = None;
            }
            if entry.disabled {
                continue;
            }
            healed_ids.push(id);
        }

        if healed_ids.is_empty() {
            return false;
        }

        for healed_id in healed_ids {
            self.record_scheduler_credential_audit(
                "auto_enable_credential",
                healed_id,
                DisabledReason::TooManyFailures,
                "auto_heal_all_too_many_failures",
                "所有可调度凭据均因连续失败自动禁用，调度器已自动恢复该凭据",
                serde_json::json!({
                    "previousReason": DisabledReason::TooManyFailures.as_str(),
                }),
            );
        }
        self.invalidate_model_capability_cohorts();
        self.notify_dispatch_state_changed();
        self.publish_credentials_changed("auto_heal_too_many_failures");
        true
    }

    fn max_concurrent_requests(&self) -> u32 {
        self.config.lock().credential_max_concurrent_requests
    }

    fn effective_max_concurrent_requests_for_id(
        &self,
        id: u64,
        global_max_concurrent_requests: u32,
    ) -> u32 {
        let entries = self.entries.lock();
        entries
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| effective_max_concurrent_requests(entry, global_max_concurrent_requests))
            .unwrap_or(global_max_concurrent_requests)
    }

    fn global_max_concurrent_requests(&self) -> u32 {
        self.config.lock().dispatch_global_max_concurrent_requests
    }

    fn max_queued_requests(&self) -> u32 {
        self.config.lock().dispatch_max_queued_requests
    }

    async fn execute_scheduler_redis_operation<T, F>(
        breaker: Arc<SchedulerRedisBreaker>,
        operation_timeout: StdDuration,
        operation: &'static str,
        on_started: F,
        future: impl Future<Output = anyhow::Result<T>>,
    ) -> SchedulerRedisExecutionOutcome<T>
    where
        F: FnOnce(),
    {
        let deadline = tokio::time::Instant::now() + operation_timeout;
        let admission = match breaker.begin_until(deadline, operation_timeout).await {
            Ok(admission) => admission,
            Err(err) => return SchedulerRedisExecutionOutcome::NotStarted(err),
        };
        on_started();
        match tokio::time::timeout_at(deadline, future).await {
            Ok(Ok(value)) => {
                admission.success();
                SchedulerRedisExecutionOutcome::Completed(value)
            }
            Ok(Err(err)) => {
                let commit_unknown = scheduler_redis_failure_commit_unknown(&err);
                admission.failure(operation, &err);
                SchedulerRedisExecutionOutcome::Failed { commit_unknown }
            }
            Err(_) => {
                let err = anyhow::anyhow!(
                    "{}超过共享总期限 {}ms",
                    operation,
                    operation_timeout.as_millis()
                );
                admission.failure(operation, &err);
                SchedulerRedisExecutionOutcome::Failed {
                    commit_unknown: true,
                }
            }
        }
    }

    fn scheduler_redis_retry_after_secs(&self) -> u64 {
        self.scheduler_redis_breaker
            .retry_after()
            .map(|remaining| remaining.as_secs().saturating_add(1))
            .unwrap_or(1)
            .max(1)
    }

    fn scheduler_redis_degraded_retry_after_secs(&self) -> Option<u64> {
        self.redis_store.as_ref()?;
        self.scheduler_redis_breaker
            .retry_after()
            .map(|remaining| remaining.as_secs().saturating_add(1).max(1))
    }

    fn block_on_scheduler_redis_affinity<T: Send>(
        &self,
        operation: &'static str,
        future: impl Future<Output = anyhow::Result<T>> + Send,
    ) -> Option<T> {
        let breaker = self.scheduler_redis_affinity_breaker.clone();
        match block_on_storage(operation, async move {
            Ok(Self::execute_scheduler_redis_operation(
                breaker,
                SCHEDULER_REDIS_AFFINITY_OP_TIMEOUT,
                operation,
                || {},
                future,
            )
            .await)
        }) {
            Ok(SchedulerRedisExecutionOutcome::Completed(value)) => Some(value),
            Ok(
                SchedulerRedisExecutionOutcome::NotStarted(_)
                | SchedulerRedisExecutionOutcome::Failed { .. },
            )
            | Err(_) => None,
        }
    }

    async fn await_scheduler_redis_affinity<T>(
        &self,
        operation: &'static str,
        future: impl Future<Output = anyhow::Result<T>>,
    ) -> Option<T> {
        match Self::execute_scheduler_redis_operation(
            self.scheduler_redis_affinity_breaker.clone(),
            SCHEDULER_REDIS_AFFINITY_OP_TIMEOUT,
            operation,
            || {},
            future,
        )
        .await
        {
            SchedulerRedisExecutionOutcome::Completed(value) => Some(value),
            SchedulerRedisExecutionOutcome::NotStarted(_)
            | SchedulerRedisExecutionOutcome::Failed { .. } => None,
        }
    }

    fn block_on_scheduler_redis_hot_outcome<T: Send, F: FnOnce() + Send>(
        &self,
        operation: &'static str,
        on_started: F,
        future: impl Future<Output = anyhow::Result<T>> + Send,
    ) -> SchedulerRedisHotOutcome<T> {
        if self.redis_store.is_none() {
            return SchedulerRedisHotOutcome::Skipped;
        }
        let breaker = self.scheduler_redis_breaker.clone();
        let execution = block_on_storage(operation, async move {
            Ok(Self::execute_scheduler_redis_operation(
                breaker,
                SCHEDULER_REDIS_HOT_OP_TIMEOUT,
                operation,
                on_started,
                future,
            )
            .await)
        });
        match execution {
            Ok(SchedulerRedisExecutionOutcome::Completed(value)) => {
                SchedulerRedisHotOutcome::Completed(value)
            }
            Ok(SchedulerRedisExecutionOutcome::NotStarted(
                SchedulerRedisAdmissionError::BreakerOpen,
            )) => SchedulerRedisHotOutcome::Skipped,
            Ok(SchedulerRedisExecutionOutcome::NotStarted(
                SchedulerRedisAdmissionError::LocalSchedulerOverloaded,
            )) => SchedulerRedisHotOutcome::LocalSchedulerOverloaded,
            Ok(SchedulerRedisExecutionOutcome::Failed { commit_unknown }) => {
                SchedulerRedisHotOutcome::Failed { commit_unknown }
            }
            Err(_) => SchedulerRedisHotOutcome::Failed {
                commit_unknown: true,
            },
        }
    }

    fn probe_scheduler_redis_capacity_recovery_if_due(&self) {
        if !self.scheduler_redis_breaker.recovery_probe_due() {
            return;
        }
        let Some(redis) = &self.redis_store else {
            return;
        };
        let redis = redis.clone();
        match self.block_on_scheduler_redis_hot_outcome(
            "探测 Redis 调度协调恢复",
            || {},
            async move { redis.ping().await },
        ) {
            SchedulerRedisHotOutcome::Completed(()) => {
                tracing::debug!("Redis 调度容量 breaker 预检恢复探测成功");
            }
            SchedulerRedisHotOutcome::Skipped
            | SchedulerRedisHotOutcome::LocalSchedulerOverloaded => {}
            SchedulerRedisHotOutcome::Failed { .. } => {
                tracing::debug!("Redis 调度容量 breaker 预检恢复探测失败，继续 degraded");
            }
        }
    }

    async fn await_scheduler_redis_hot_outcome<T, F: FnOnce()>(
        &self,
        operation: &'static str,
        on_started: F,
        future: impl Future<Output = anyhow::Result<T>>,
    ) -> SchedulerRedisHotOutcome<T> {
        if self.redis_store.is_none() {
            return SchedulerRedisHotOutcome::Skipped;
        }
        match Self::execute_scheduler_redis_operation(
            self.scheduler_redis_breaker.clone(),
            SCHEDULER_REDIS_HOT_OP_TIMEOUT,
            operation,
            on_started,
            future,
        )
        .await
        {
            SchedulerRedisExecutionOutcome::Completed(value) => {
                SchedulerRedisHotOutcome::Completed(value)
            }
            SchedulerRedisExecutionOutcome::NotStarted(
                SchedulerRedisAdmissionError::BreakerOpen,
            ) => SchedulerRedisHotOutcome::Skipped,
            SchedulerRedisExecutionOutcome::NotStarted(
                SchedulerRedisAdmissionError::LocalSchedulerOverloaded,
            ) => SchedulerRedisHotOutcome::LocalSchedulerOverloaded,
            SchedulerRedisExecutionOutcome::Failed { commit_unknown } => {
                SchedulerRedisHotOutcome::Failed { commit_unknown }
            }
        }
    }

    fn block_on_scheduler_redis_state_sync<T: Send>(
        &self,
        operation: &'static str,
        future: impl Future<Output = anyhow::Result<T>> + Send,
    ) -> Option<T> {
        let breaker = self.scheduler_redis_snapshot_breaker.clone();
        let result = block_on_storage(operation, async move {
            match Self::execute_scheduler_redis_operation(
                breaker,
                SCHEDULER_REDIS_SNAPSHOT_OP_TIMEOUT,
                operation,
                || {},
                future,
            )
            .await
            {
                SchedulerRedisExecutionOutcome::Completed(value) => Ok(value),
                SchedulerRedisExecutionOutcome::NotStarted(reason) => Err(anyhow::anyhow!(
                    "Redis scheduler state-sync 未准入: {reason:?}"
                )),
                SchedulerRedisExecutionOutcome::Failed { .. } => {
                    Err(anyhow::anyhow!("Redis scheduler state-sync 执行失败"))
                }
            }
        });
        match result {
            Ok(value) => Some(value),
            Err(err) => {
                tracing::debug!(
                    operation,
                    timeout_ms = SCHEDULER_REDIS_SNAPSHOT_OP_TIMEOUT.as_millis() as u64,
                    "Redis 调度状态同步未在预算内完成，沿用本地调度缓存: {}",
                    err
                );
                None
            }
        }
    }

    fn refresh_state_for_credential(&self, id: u64) -> Arc<CredentialRefreshState> {
        let mut states = self.refresh_states.lock();
        states
            .entry(id)
            .or_insert_with(|| Arc::new(CredentialRefreshState::default()))
            .clone()
    }

    fn retain_refresh_states_for_ids(&self, ids: &HashSet<u64>) {
        self.refresh_states.lock().retain(|id, _| ids.contains(id));
    }

    fn remove_refresh_state_for_credential(&self, id: u64) {
        self.refresh_states.lock().remove(&id);
    }

    fn invalidate_refresh_state_for_credential(&self, id: u64) {
        self.refresh_state_for_credential(id).invalidate();
    }

    fn ensure_refresh_state_generation(
        state: &CredentialRefreshState,
        expected: u64,
        send_committed: bool,
    ) -> anyhow::Result<()> {
        if state.is_generation_current(expected) {
            return Ok(());
        }
        Err(RefreshFailure::new(
            RefreshFailureStage::Coordination,
            RefreshFailureKind::Coordination,
            None,
            None,
            send_committed,
        )
        .into())
    }

    async fn apply_refresh_health_action(
        &self,
        id: u64,
        failure: &RefreshFailure,
        claim: Option<&RedisRefreshHealthClaim>,
        deadline: tokio::time::Instant,
    ) -> anyhow::Result<()> {
        if let (Some(redis), Some(claim)) = (&self.redis_store, claim) {
            let consumed =
                tokio::time::timeout_at(deadline, redis.ack_token_refresh_health_claim(id, claim))
                    .await
                    .map_err(|_| {
                        anyhow::Error::new(RefreshFailure::new(
                            RefreshFailureStage::Coordination,
                            RefreshFailureKind::Timeout,
                            None,
                            None,
                            false,
                        ))
                    })?
                    .map_err(|_| {
                        anyhow::Error::new(RefreshFailure::new(
                            RefreshFailureStage::Coordination,
                            RefreshFailureKind::Coordination,
                            None,
                            None,
                            false,
                        ))
                    })?;
            if !consumed {
                tracing::debug!(
                    credential_id = id,
                    refresh_generation = claim.generation,
                    "Skipped a stale or already-consumed token refresh health claim"
                );
                return Ok(());
            }
        }

        // Redis claims are consumed before the idempotent mutation. This intentionally chooses
        // at-most-once behavior: a process crash in this small interval may lose one health action,
        // but a stale owner can never apply it to a newer credential generation.
        match failure.kind {
            RefreshFailureKind::InvalidGrant => {
                self.report_refresh_token_invalid(id);
            }
            RefreshFailureKind::CredentialAuth => {
                self.report_transient_failure_kind(
                    id,
                    None,
                    TransientFailureKind::Auth,
                    failure.retry_after,
                    "token_refresh_credential_auth",
                )?;
            }
            RefreshFailureKind::RateLimited => {
                self.report_transient_failure_kind(
                    id,
                    None,
                    TransientFailureKind::RateLimit,
                    failure.retry_after,
                    "token_refresh_rate_limited",
                )?;
            }
            _ => return Ok(()),
        }
        Ok(())
    }

    async fn begin_distributed_refresh_until(
        &self,
        id: u64,
        identity: RefreshAttemptIdentity,
        caller_can_claim_health: bool,
        deadline: tokio::time::Instant,
    ) -> anyhow::Result<DistributedRefreshDecision> {
        let redis = self.redis_store.as_ref().ok_or_else(|| {
            anyhow::Error::new(RefreshFailure::new(
                RefreshFailureStage::Coordination,
                RefreshFailureKind::Coordination,
                None,
                None,
                false,
            ))
        })?;
        loop {
            if tokio::time::Instant::now() >= deadline {
                return Err(RefreshFailure::new(
                    RefreshFailureStage::Coordination,
                    RefreshFailureKind::Timeout,
                    None,
                    None,
                    false,
                )
                .into());
            }
            let begin = tokio::time::timeout_at(
                deadline,
                redis.begin_token_refresh(id, identity.0, caller_can_claim_health),
            )
            .await
            .map_err(|_| {
                anyhow::Error::new(RefreshFailure::new(
                    RefreshFailureStage::Coordination,
                    RefreshFailureKind::Timeout,
                    None,
                    None,
                    false,
                ))
            })?
            .map_err(|_| {
                anyhow::Error::new(RefreshFailure::new(
                    RefreshFailureStage::Coordination,
                    RefreshFailureKind::Coordination,
                    None,
                    None,
                    false,
                ))
            })?;
            match begin {
                RedisRefreshBegin::Leader(lease) => {
                    return Ok(DistributedRefreshDecision::Leader(lease));
                }
                RedisRefreshBegin::Wait { poll_after, .. } => {
                    let wake_at = (tokio::time::Instant::now() + poll_after).min(deadline);
                    tokio::time::sleep_until(wake_at).await;
                }
                RedisRefreshBegin::Replay {
                    outcome,
                    health_claim,
                } => {
                    let failure = refresh_failure_from_redis(&outcome.failure);
                    if let Some(claim) = health_claim.as_ref() {
                        self.apply_refresh_health_action(id, &failure, Some(claim), deadline)
                            .await?;
                    }
                    return Ok(DistributedRefreshDecision::Replay(
                        failure.into_shared_failure_wave(),
                    ));
                }
                RedisRefreshBegin::Succeeded {
                    generation,
                    storage_revision,
                } => {
                    return Ok(DistributedRefreshDecision::Succeeded {
                        generation,
                        storage_revision,
                    });
                }
            }
        }
    }

    async fn complete_distributed_refresh_failure_until(
        &self,
        lease: &RedisRefreshLease,
        failure: &RefreshFailure,
        leader_can_claim_health: bool,
        deadline: tokio::time::Instant,
    ) -> anyhow::Result<RefreshFailure> {
        let redis = self.redis_store.as_ref().ok_or_else(|| {
            anyhow::Error::new(RefreshFailure::new(
                RefreshFailureStage::Coordination,
                RefreshFailureKind::Coordination,
                None,
                None,
                failure.send_committed,
            ))
        })?;
        let committed = tokio::time::timeout_at(
            deadline,
            redis.complete_token_refresh_failure(
                lease,
                &refresh_failure_to_redis(failure),
                leader_can_claim_health,
            ),
        )
        .await
        .map_err(|_| {
            anyhow::Error::new(RefreshFailure::new(
                RefreshFailureStage::Coordination,
                RefreshFailureKind::Timeout,
                None,
                None,
                failure.send_committed,
            ))
        })?
        .map_err(|_| {
            anyhow::Error::new(RefreshFailure::new(
                RefreshFailureStage::Coordination,
                RefreshFailureKind::Coordination,
                None,
                None,
                failure.send_committed,
            ))
        })?
        .ok_or_else(|| {
            anyhow::Error::new(RefreshFailure::new(
                RefreshFailureStage::Coordination,
                RefreshFailureKind::Coordination,
                None,
                None,
                failure.send_committed,
            ))
        })?;
        let committed_failure = refresh_failure_from_redis(&committed.outcome.failure);
        if let Some(claim) = committed.health_claim.as_ref() {
            self.apply_refresh_health_action(
                lease.credential_id,
                &committed_failure,
                Some(claim),
                deadline,
            )
            .await?;
        }
        Ok(committed_failure.into_shared_failure_wave())
    }

    async fn complete_distributed_refresh_success_until(
        &self,
        lease: &RedisRefreshLease,
        storage_revision: u64,
        deadline: tokio::time::Instant,
    ) -> anyhow::Result<()> {
        let redis = self.redis_store.as_ref().ok_or_else(|| {
            anyhow::Error::new(RefreshFailure::new(
                RefreshFailureStage::Coordination,
                RefreshFailureKind::Coordination,
                None,
                None,
                true,
            ))
        })?;
        let committed = tokio::time::timeout_at(
            deadline,
            redis.complete_token_refresh_success(lease, storage_revision),
        )
        .await
        .map_err(|_| {
            anyhow::Error::new(RefreshFailure::new(
                RefreshFailureStage::Coordination,
                RefreshFailureKind::Timeout,
                None,
                None,
                true,
            ))
        })?
        .map_err(|_| {
            anyhow::Error::new(RefreshFailure::new(
                RefreshFailureStage::Coordination,
                RefreshFailureKind::Coordination,
                None,
                None,
                true,
            ))
        })?;
        if !committed {
            return Err(RefreshFailure::new(
                RefreshFailureStage::Coordination,
                RefreshFailureKind::Coordination,
                None,
                None,
                true,
            )
            .into());
        }
        Ok(())
    }

    async fn complete_or_cancel_distributed_refresh_success_until(
        &self,
        lease: &RedisRefreshLease,
        storage_revision: u64,
        deadline: tokio::time::Instant,
    ) -> anyhow::Result<()> {
        if self.postgres_store.is_some() {
            return self
                .complete_distributed_refresh_success_until(lease, storage_revision, deadline)
                .await;
        }

        let redis = self.redis_store.as_ref().ok_or_else(|| {
            anyhow::Error::new(RefreshFailure::new(
                RefreshFailureStage::Coordination,
                RefreshFailureKind::Coordination,
                None,
                None,
                true,
            ))
        })?;
        let cancelled = tokio::time::timeout_at(deadline, redis.cancel_token_refresh(lease))
            .await
            .map_err(|_| {
                anyhow::Error::new(RefreshFailure::new(
                    RefreshFailureStage::Coordination,
                    RefreshFailureKind::Timeout,
                    None,
                    None,
                    true,
                ))
            })?
            .map_err(|_| {
                anyhow::Error::new(RefreshFailure::new(
                    RefreshFailureStage::Coordination,
                    RefreshFailureKind::Coordination,
                    None,
                    None,
                    true,
                ))
            })?;
        if !cancelled {
            return Err(RefreshFailure::new(
                RefreshFailureStage::Coordination,
                RefreshFailureKind::Coordination,
                None,
                None,
                true,
            )
            .into());
        }
        tracing::debug!(
            credential_id = lease.credential_id,
            refresh_generation = lease.generation,
            "Redis Token 刷新成功仅更新本进程内存；无 PgSQL 权威存储时取消 lease，避免发布不可复用的跨实例 success outcome"
        );
        Ok(())
    }

    fn local_global_capacity_state(&self) -> SchedulerGlobalCapacityState {
        let in_flight_requests = self
            .entries
            .lock()
            .iter()
            .map(|entry| entry.in_flight_requests)
            .sum();
        SchedulerGlobalCapacityState {
            in_flight_requests,
            queued_requests: self.queued_requests.load(Ordering::Acquire),
        }
    }

    fn acquire_local_in_flight_slot_with_id(
        &self,
        id: u64,
        lease_id: u64,
        now: Instant,
        max_concurrent_requests: u32,
        global_max_concurrent_requests: u32,
        request_weight_units: u32,
    ) -> bool {
        let mut entries = self.entries.lock();
        let global_in_flight: u32 = entries.iter().map(|entry| entry.in_flight_requests).sum();
        if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
            let mut lease_weight_units =
                effective_weight_for_limit(request_weight_units, max_concurrent_requests);
            lease_weight_units =
                effective_weight_for_limit(lease_weight_units, global_max_concurrent_requests);
            if global_max_concurrent_requests > 0
                && global_in_flight.saturating_add(lease_weight_units)
                    > global_max_concurrent_requests
            {
                return false;
            }
            if !entry_has_concurrency_capacity(entry, max_concurrent_requests, lease_weight_units) {
                return false;
            }
            entry.in_flight_requests = entry.in_flight_requests.saturating_add(lease_weight_units);
            entry.in_flight_leases.push(InFlightLease {
                id: lease_id,
                acquired_at: now,
                last_seen_at: now,
                kind: InFlightKind::Api,
                weight_units: lease_weight_units,
                locally_owned: true,
            });
            return true;
        }
        false
    }

    /// Reserve capacity owned by this process before awaiting Redis.
    ///
    /// Redis remains authoritative for cross-instance capacity. Remote leases in the local
    /// snapshot may be stale, so this provisional gate only counts locally owned leases. This
    /// still prevents same-process contenders from stampeding the same Redis candidate.
    fn acquire_provisional_local_in_flight_slot_with_id(
        &self,
        id: u64,
        lease_id: u64,
        now: Instant,
        max_concurrent_requests: u32,
        global_max_concurrent_requests: u32,
        request_weight_units: u32,
    ) -> bool {
        let mut entries = self.entries.lock();
        let locally_owned_global_in_flight = entries
            .iter()
            .flat_map(|entry| entry.in_flight_leases.iter())
            .filter(|lease| lease.locally_owned)
            .fold(0u32, |sum, lease| sum.saturating_add(lease.weight_units));
        let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) else {
            return false;
        };
        let locally_owned_credential_in_flight = entry
            .in_flight_leases
            .iter()
            .filter(|lease| lease.locally_owned)
            .fold(0u32, |sum, lease| sum.saturating_add(lease.weight_units));
        let effective_max_concurrent_requests =
            effective_max_concurrent_requests(entry, max_concurrent_requests);
        let mut lease_weight_units =
            effective_weight_for_limit(request_weight_units, effective_max_concurrent_requests);
        lease_weight_units =
            effective_weight_for_limit(lease_weight_units, global_max_concurrent_requests);
        if global_max_concurrent_requests > 0
            && locally_owned_global_in_flight.saturating_add(lease_weight_units)
                > global_max_concurrent_requests
        {
            return false;
        }
        if effective_max_concurrent_requests > 0
            && locally_owned_credential_in_flight.saturating_add(lease_weight_units)
                > effective_max_concurrent_requests
        {
            return false;
        }
        entry.in_flight_requests = entry.in_flight_requests.saturating_add(lease_weight_units);
        entry.in_flight_leases.push(InFlightLease {
            id: lease_id,
            acquired_at: now,
            last_seen_at: now,
            kind: InFlightKind::Api,
            weight_units: lease_weight_units,
            locally_owned: true,
        });
        true
    }

    fn try_enter_local_dispatch_queue(&self, max_queued: u32) -> bool {
        loop {
            let queued = self.queued_requests.load(Ordering::Acquire);
            if max_queued > 0 && queued >= max_queued {
                return false;
            }
            if self
                .queued_requests
                .compare_exchange(queued, queued + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return true;
            }
        }
    }

    fn in_flight_lease_max_age(&self) -> Option<StdDuration> {
        let secs = self.config.lock().credential_in_flight_lease_max_secs;
        (secs > 0).then(|| StdDuration::from_secs(secs))
    }

    fn transient_failure_settings(
        &self,
        kind: TransientFailureKind,
    ) -> (StdDuration, StdDuration, f64, f64, StdDuration, f64) {
        let config = self.config.lock();
        let base = match kind {
            TransientFailureKind::RateLimit => config.credential_rate_limit_cooldown_secs,
            TransientFailureKind::Server => config.credential_server_error_cooldown_secs,
            TransientFailureKind::Network => config.credential_network_error_cooldown_secs,
            TransientFailureKind::Stream => config.credential_stream_error_cooldown_secs,
            TransientFailureKind::Protocol => config.credential_protocol_error_cooldown_secs,
            TransientFailureKind::Auth => config.credential_auth_error_cooldown_secs,
        }
        .max(1);
        let max = config.credential_max_cooldown_secs.max(1);
        let multiplier = config.credential_cooldown_backoff_multiplier.max(1.0);
        let jitter_percent = config.credential_cooldown_jitter_percent.min(100) as f64 / 100.0;
        let offset = if jitter_percent == 0.0 {
            0.0
        } else {
            fastrand::f64() * jitter_percent * 2.0 - jitter_percent
        };
        (
            StdDuration::from_secs(base),
            StdDuration::from_secs(max),
            multiplier,
            1.0 + offset,
            StdDuration::from_secs(config.credential_probation_secs),
            config.scheduler_error_ewma_alpha.clamp(0.01, 1.0),
        )
    }

    fn local_cooldown_duration(
        retry_after: Option<StdDuration>,
        base: StdDuration,
        max: StdDuration,
        multiplier: f64,
        jitter: f64,
        streak: u32,
    ) -> StdDuration {
        let requested = retry_after.unwrap_or_else(|| {
            let millis = base.as_millis() as f64
                * multiplier.powi(streak.saturating_sub(1).min(20) as i32)
                * jitter;
            StdDuration::from_millis(millis.max(1.0).round() as u64)
        });
        requested.clamp(StdDuration::from_secs(1), max)
    }

    fn transient_failure_coalesce_window(base: StdDuration) -> StdDuration {
        base.clamp(StdDuration::from_secs(1), StdDuration::from_secs(5))
    }

    fn update_credential_rpm_from_config(&self) {
        let global_rpm = self.config.lock().credential_rpm.unwrap_or(0);
        let ids: Vec<u64> = {
            let mut entries = self.entries.lock();
            entries
                .iter_mut()
                .filter_map(|entry| {
                    let rpm = effective_rpm(entry, global_rpm);
                    if rate_limit_interval_for_rpm(rpm).is_some() {
                        return None;
                    }
                    entry.rate_limit_available_at?;
                    let id = entry.id;
                    entry.rate_limit_available_at = None;
                    Some(id)
                })
                .collect()
        };
        if ids.is_empty() {
            return;
        }
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            spawn_best_effort_storage_task("清理 Redis 凭据限流状态", async move {
                for id in ids {
                    redis.clear_rate_limit(id).await?;
                }
                Ok(())
            });
        }
    }

    fn clear_rate_limit_for_credential(&self, id: u64) {
        {
            let mut entries = self.entries.lock();
            if let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) {
                entry.rate_limit_available_at = None;
            }
        }
        let Some(redis) = &self.redis_store else {
            return;
        };
        let redis = redis.clone();
        spawn_best_effort_storage_task("清理 Redis 凭据限流状态", async move {
            redis.clear_rate_limit(id).await?;
            Ok(())
        });
    }

    fn apply_rate_limit_available_at_for_credential(&self, id: u64, available_at: Instant) {
        let mut entries = self.entries.lock();
        if let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) {
            entry.rate_limit_available_at = Some(
                entry
                    .rate_limit_available_at
                    .map_or(available_at, |current| current.max(available_at)),
            );
        }
    }

    fn instant_from_epoch_ms(target_ms: i64, now_ms: i64, now: Instant) -> Option<Instant> {
        let delta_ms = target_ms.saturating_sub(now_ms);
        if delta_ms <= 0 {
            return None;
        }
        Some(now + StdDuration::from_millis(delta_ms as u64))
    }

    fn mark_rate_limited_at(&self, id: u64, now: Instant) -> anyhow::Result<()> {
        let global_rpm = self.config.lock().credential_rpm.unwrap_or(0);
        {
            let mut entries = self.entries.lock();
            let Some(entry) = entries.iter_mut().find(|e| e.id == id) else {
                return Ok(());
            };
            let rpm = effective_rpm(entry, global_rpm);
            if rate_limit_interval_for_rpm(rpm).is_none() {
                entry.rate_limit_available_at = None;
                return Ok(());
            }
            entry.rate_limit_available_at =
                entry_rate_limit_window_remaining(entry, global_rpm, now)
                    .and_then(|remaining| now.checked_add(remaining));
        }
        Ok(())
    }

    async fn reserve_scheduler_selection_for_request(
        &self,
        id: u64,
        request_weight_units: u32,
    ) -> anyhow::Result<SchedulerRedisSelectionAdmission> {
        let now = Instant::now();
        let now_ms = Utc::now().timestamp_millis();
        let rpm = {
            let global_rpm = self.config.lock().credential_rpm.unwrap_or(0);
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|entry| entry.id == id)
                .map(|entry| effective_rpm(entry, global_rpm))
                .unwrap_or(global_rpm)
        };
        if rpm == 0 || self.redis_store.is_none() {
            return Ok(SchedulerRedisSelectionAdmission::NotRequired);
        }

        let Some(redis) = &self.redis_store else {
            return Ok(SchedulerRedisSelectionAdmission::NotRequired);
        };
        let redis = redis.clone();
        let response = match self
            .await_scheduler_redis_hot_outcome("原子记录 Redis 调度选中次数", || {}, async move {
                redis
                    .try_record_scheduler_selection(id, rpm, request_weight_units)
                    .await
            })
            .await
        {
            SchedulerRedisHotOutcome::Completed(response) => response,
            SchedulerRedisHotOutcome::LocalSchedulerOverloaded => {
                return Err(SchedulerRedisUnavailableError {
                    retry_after_secs: 1,
                    local_overloaded: true,
                }
                .into());
            }
            SchedulerRedisHotOutcome::Skipped | SchedulerRedisHotOutcome::Failed { .. } => {
                return Err(SchedulerRedisUnavailableError {
                    retry_after_secs: self.scheduler_redis_retry_after_secs(),
                    local_overloaded: false,
                }
                .into());
            }
        };

        match response {
            SchedulerSelectionReservation::Recorded {
                rate_limit_available_at_ms,
                ..
            } => {
                if let Some(available_at_ms) = rate_limit_available_at_ms
                    .and_then(|until_ms| Self::instant_from_epoch_ms(until_ms, now_ms, now))
                {
                    self.apply_rate_limit_available_at_for_credential(id, available_at_ms);
                }
                Ok(SchedulerRedisSelectionAdmission::Recorded {
                    rate_limit_available_at: rate_limit_available_at_ms
                        .and_then(|until_ms| Self::instant_from_epoch_ms(until_ms, now_ms, now)),
                })
            }
            SchedulerSelectionReservation::RateLimited {
                retry_after_ms,
                rate_limit_available_at_ms,
            } => {
                let available_at =
                    Self::instant_from_epoch_ms(rate_limit_available_at_ms, now_ms, now)
                        .unwrap_or(now + StdDuration::from_millis(retry_after_ms.max(1)));
                self.apply_rate_limit_available_at_for_credential(id, available_at);
                Ok(SchedulerRedisSelectionAdmission::RateLimited(
                    SchedulerRedisRateLimitWait {
                        retry_after: StdDuration::from_millis(retry_after_ms.max(1)),
                        available_at,
                    },
                ))
            }
        }
    }

    fn prepare_in_flight_slot(
        &self,
        id: u64,
        request_weight_units: u32,
    ) -> Option<PreparedInFlightSlot> {
        let max_concurrent_requests = self.max_concurrent_requests();
        let effective_max_concurrent_requests =
            self.effective_max_concurrent_requests_for_id(id, max_concurrent_requests);
        let global_max_concurrent_requests = self.global_max_concurrent_requests();
        let now = Instant::now();
        let lease_id = self.next_in_flight_lease_id.fetch_add(1, Ordering::Relaxed);

        if self.redis_store.is_some() {
            let release_dispatcher = self.scheduler_redis_release_dispatcher.as_ref()?.clone();
            let release_reservation = release_dispatcher.try_reserve()?;
            if !self.acquire_provisional_local_in_flight_slot_with_id(
                id,
                lease_id,
                now,
                max_concurrent_requests,
                global_max_concurrent_requests,
                request_weight_units,
            ) {
                return None;
            }
            return Some(PreparedInFlightSlot::Redis {
                guard: InFlightLeaseGuard::new(
                    self.entries.clone(),
                    self.redis_store.clone(),
                    Some(release_dispatcher),
                    Some(release_reservation),
                    Some(self.released_in_flight_lease_tombstones.clone()),
                    self.capacity_signal.clone(),
                    id,
                    lease_id,
                    request_weight_units,
                ),
                effective_max_concurrent_requests,
                global_max_concurrent_requests,
                request_weight_units,
                max_age: self.in_flight_lease_max_age(),
            });
        }

        if self.acquire_local_in_flight_slot_with_id(
            id,
            lease_id,
            now,
            max_concurrent_requests,
            global_max_concurrent_requests,
            request_weight_units,
        ) {
            return Some(PreparedInFlightSlot::Local(InFlightLeaseGuard::new(
                self.entries.clone(),
                None,
                None,
                None,
                None,
                self.capacity_signal.clone(),
                id,
                lease_id,
                request_weight_units,
            )));
        }
        None
    }

    async fn confirm_prepared_in_flight_slot(
        &self,
        prepared: PreparedInFlightSlot,
    ) -> anyhow::Result<Option<InFlightLeaseGuard>> {
        let (
            mut guard,
            effective_max_concurrent_requests,
            global_max_concurrent_requests,
            request_weight_units,
            max_age,
        ) = match prepared {
            PreparedInFlightSlot::Local(guard) => return Ok(Some(guard)),
            PreparedInFlightSlot::Redis {
                guard,
                effective_max_concurrent_requests,
                global_max_concurrent_requests,
                request_weight_units,
                max_age,
            } => (
                guard,
                effective_max_concurrent_requests,
                global_max_concurrent_requests,
                request_weight_units,
                max_age,
            ),
        };
        let redis = self
            .redis_store
            .as_ref()
            .expect("prepared Redis lease requires a Redis store")
            .clone();
        let credential_id = guard.credential_id();
        let lease_id = guard.id();
        let redis_acquire = self
            .await_scheduler_redis_hot_outcome(
                "占用 Redis 凭据并发槽",
                || guard.arm_redis_commit_unknown(),
                async move {
                    redis
                        .acquire_dispatch_lease(
                            credential_id,
                            lease_id,
                            effective_max_concurrent_requests,
                            global_max_concurrent_requests,
                            request_weight_units,
                            max_age,
                            InFlightKind::Api.as_str(),
                        )
                        .await
                },
            )
            .await;
        match redis_acquire {
            SchedulerRedisHotOutcome::Completed(Some(_count)) => {
                guard.configure_redis_touch_interval(max_age);
                guard.confirm_redis_acquired();
                Ok(Some(guard))
            }
            SchedulerRedisHotOutcome::Completed(None) => {
                guard.confirm_redis_not_acquired();
                drop(guard);
                Ok(None)
            }
            SchedulerRedisHotOutcome::Failed { commit_unknown } => {
                if !commit_unknown {
                    guard.confirm_redis_not_acquired();
                }
                let retry_after_secs = self.scheduler_redis_retry_after_secs();
                drop(guard);
                Err(SchedulerRedisUnavailableError {
                    retry_after_secs,
                    local_overloaded: false,
                }
                .into())
            }
            SchedulerRedisHotOutcome::Skipped => {
                let retry_after_secs = self.scheduler_redis_retry_after_secs();
                guard.confirm_redis_not_acquired();
                drop(guard);
                Err(SchedulerRedisUnavailableError {
                    retry_after_secs,
                    local_overloaded: false,
                }
                .into())
            }
            SchedulerRedisHotOutcome::LocalSchedulerOverloaded => {
                guard.confirm_redis_not_acquired();
                drop(guard);
                Err(SchedulerRedisUnavailableError {
                    retry_after_secs: 1,
                    local_overloaded: true,
                }
                .into())
            }
        }
    }

    #[cfg(test)]
    async fn acquire_in_flight_slot(
        &self,
        id: u64,
        request_weight_units: u32,
    ) -> anyhow::Result<Option<InFlightLeaseGuard>> {
        self.cleanup_expired_in_flight_leases_local_first();
        let Some(prepared) = self.prepare_in_flight_slot(id, request_weight_units) else {
            return Ok(None);
        };
        self.confirm_prepared_in_flight_slot(prepared).await
    }

    #[cfg(test)]
    pub(crate) fn acquire_in_flight_lease_for_test(&self, id: u64) -> Option<InFlightLeaseGuard> {
        self.cleanup_expired_in_flight_leases_local_first();
        let max_concurrent_requests = self.max_concurrent_requests();
        let global_max_concurrent_requests = self.global_max_concurrent_requests();
        let lease_id = self.next_in_flight_lease_id.fetch_add(1, Ordering::Relaxed);
        if self.acquire_local_in_flight_slot_with_id(
            id,
            lease_id,
            Instant::now(),
            max_concurrent_requests,
            global_max_concurrent_requests,
            1,
        ) {
            return Some(InFlightLeaseGuard::new(
                self.entries.clone(),
                None,
                None,
                None,
                None,
                self.capacity_signal.clone(),
                id,
                lease_id,
                1,
            ));
        }
        None
    }

    #[cfg(test)]
    pub(crate) fn age_in_flight_lease_for_test(
        &self,
        credential_id: u64,
        lease_id: u64,
        age: StdDuration,
    ) {
        let mut entries = self.entries.lock();
        if let Some(entry) = entries.iter_mut().find(|e| e.id == credential_id) {
            if let Some(lease) = entry
                .in_flight_leases
                .iter_mut()
                .find(|lease| lease.id == lease_id)
            {
                let now = Instant::now();
                lease.acquired_at = now - age;
                lease.last_seen_at = now - age;
            }
        }
    }

    #[cfg(test)]
    pub fn cleanup_expired_in_flight_leases(&self) -> usize {
        match self.cleanup_expired_in_flight_leases_result() {
            Ok(cleaned) => cleaned,
            Err(err) => {
                tracing::warn!("清理超时并发 lease 失败: {}", err);
                0
            }
        }
    }

    #[cfg(test)]
    fn cleanup_expired_in_flight_leases_result(&self) -> anyhow::Result<usize> {
        let Some(max_age) = self.in_flight_lease_max_age() else {
            return Ok(0);
        };
        if let Some(redis) = &self.redis_store {
            let ids: Vec<u64> = {
                let entries = self.entries.lock();
                entries.iter().map(|entry| entry.id).collect()
            };
            let redis = redis.clone();
            let cleaned = block_on_storage("清理 Redis 超时并发 lease", async move {
                redis.cleanup_expired_in_flight_leases(&ids, max_age).await
            })?;
            if cleaned > 0 {
                tracing::warn!(
                    removed = cleaned,
                    max_age_secs = max_age.as_secs(),
                    "清理超时未释放的 Redis 凭据并发占用 lease"
                );
                self.notify_dispatch_state_changed();
                self.refresh_scheduler_state_from_redis_force()?;
            }
            return Ok(cleaned);
        }
        Ok(self.cleanup_expired_local_in_flight_leases(max_age, Instant::now()))
    }

    fn cleanup_expired_local_in_flight_leases(&self, max_age: StdDuration, now: Instant) -> usize {
        let mut cleaned = 0usize;
        {
            let mut entries = self.entries.lock();
            for entry in entries.iter_mut() {
                let before = entry.in_flight_leases.len();
                let mut removed_weight = 0u32;
                entry.in_flight_leases.retain(|lease| {
                    let keep = now.saturating_duration_since(lease.last_seen_at) <= max_age;
                    if !keep {
                        removed_weight = removed_weight.saturating_add(lease.weight_units.max(1));
                    }
                    keep
                });
                let removed = before.saturating_sub(entry.in_flight_leases.len());
                if removed > 0 {
                    cleaned += removed;
                    entry.in_flight_requests =
                        entry.in_flight_requests.saturating_sub(removed_weight);
                    tracing::warn!(
                        credential_id = entry.id,
                        removed,
                        removed_weight_units = removed_weight,
                        max_age_secs = max_age.as_secs(),
                        "清理超时未释放的凭据并发占用 lease"
                    );
                }
            }
        }

        if cleaned > 0 {
            self.notify_dispatch_state_changed();
        }
        cleaned
    }

    fn spawn_redis_expired_in_flight_cleanup(&self, max_age: StdDuration) {
        let Some(redis) = &self.redis_store else {
            return;
        };
        let redis = redis.clone();
        let source_instance_id = self.scheduler_instance_id.clone();
        let ids: Vec<u64> = {
            let entries = self.entries.lock();
            entries.iter().map(|entry| entry.id).collect()
        };
        spawn_best_effort_storage_task("清理 Redis 超时并发 lease", async move {
            let removed = redis
                .cleanup_expired_in_flight_leases(&ids, max_age)
                .await?;
            if removed > 0 {
                let payload = serde_json::json!({
                    "kind": "dispatch_wakeup",
                    "removed": removed,
                    "sourceInstanceId": source_instance_id,
                    "changedAt": Utc::now().to_rfc3339(),
                })
                .to_string();
                redis.publish_dispatch_wakeup(payload).await?;
            }
            Ok(())
        });
    }

    fn cleanup_expired_in_flight_leases_throttled(&self) -> anyhow::Result<usize> {
        let now = Instant::now();
        {
            let mut last_cleanup_at = self.last_scheduler_redis_cleanup_at.lock();
            if last_cleanup_at.is_some_and(|last| {
                now.saturating_duration_since(last) < SCHEDULER_REDIS_CLEANUP_MIN_INTERVAL
            }) {
                return Ok(0);
            }
            *last_cleanup_at = Some(now);
        }

        let mut cleaned = 0usize;
        if let Some(max_age) = self.in_flight_lease_max_age() {
            cleaned = self.cleanup_expired_local_in_flight_leases(max_age, now);
            self.spawn_redis_expired_in_flight_cleanup(max_age);
        }
        Ok(cleaned)
    }

    fn cleanup_expired_in_flight_leases_local_first(&self) {
        if let Some(max_age) = self.in_flight_lease_max_age() {
            let cleaned = self.cleanup_expired_local_in_flight_leases(max_age, Instant::now());
            if cleaned > 0 {
                self.spawn_redis_expired_in_flight_cleanup(max_age);
                return;
            }
        }
        if let Err(err) = self.cleanup_expired_in_flight_leases_throttled() {
            tracing::warn!("快照读取前清理超时并发 lease 失败: {}", err);
        }
    }

    pub fn clear_in_flight_leases(
        &self,
        credential_id: u64,
        min_idle: Option<StdDuration>,
    ) -> usize {
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            match block_on_storage("清理 Redis 凭据并发占用", async move {
                redis.clear_in_flight_leases(credential_id, min_idle).await
            }) {
                Ok(cleared) => {
                    self.refresh_scheduler_state_from_redis_force_best_effort();
                    if cleared > 0 {
                        self.notify_dispatch_state_changed();
                    }
                    return cleared;
                }
                Err(err) => tracing::warn!(credential_id, "清理 Redis 凭据并发占用失败: {}", err),
            }
            return 0;
        }
        let now = Instant::now();
        let mut cleared = 0usize;
        {
            let mut entries = self.entries.lock();
            if let Some(entry) = entries.iter_mut().find(|e| e.id == credential_id) {
                let before = entry.in_flight_leases.len();
                let mut removed_weight = 0u32;
                match min_idle {
                    Some(min_idle) => {
                        entry.in_flight_leases.retain(|lease| {
                            let keep = now.saturating_duration_since(lease.last_seen_at) < min_idle;
                            if !keep {
                                removed_weight =
                                    removed_weight.saturating_add(lease.weight_units.max(1));
                            }
                            keep
                        });
                    }
                    None => {
                        removed_weight = entry.in_flight_leases.iter().fold(0u32, |sum, lease| {
                            sum.saturating_add(lease.weight_units.max(1))
                        });
                        entry.in_flight_leases.clear();
                    }
                }
                cleared = before.saturating_sub(entry.in_flight_leases.len());
                if cleared > 0 {
                    entry.in_flight_requests =
                        entry.in_flight_requests.saturating_sub(removed_weight);
                }
            }
        }
        if cleared > 0 {
            self.notify_dispatch_state_changed();
        }
        cleared
    }

    async fn wait_for_dispatch_capacity(
        &self,
        wait_for: Option<StdDuration>,
        max_wait_remaining: Option<StdDuration>,
        capacity_waiter: &mut CapacityWaiter,
    ) {
        let fallback = if self.redis_store.is_some() {
            StdDuration::from_secs(1)
        } else {
            StdDuration::from_secs(CONCURRENCY_WAIT_WAKEUP_SECS)
        };
        let mut wakeup = wait_for.unwrap_or(fallback).min(fallback);
        if let Some(remaining) = max_wait_remaining {
            wakeup = wakeup.min(remaining);
        }
        if wakeup.is_zero() {
            return;
        }
        let _ = tokio::time::timeout(wakeup, capacity_waiter.wait_for_change()).await;
        let scheduler_refresh_result = if self.redis_store.is_some() {
            self.refresh_scheduler_state_from_redis()
        } else {
            Ok(())
        };
        if let Err(err) = scheduler_refresh_result {
            tracing::debug!("调度等待唤醒后同步 Redis 并发状态失败: {}", err);
        }
    }

    async fn sleep_for_scheduler_redis_recovery(
        &self,
        wait_for: StdDuration,
        max_wait_remaining: Option<StdDuration>,
    ) {
        let mut wakeup = wait_for;
        if let Some(remaining) = max_wait_remaining {
            wakeup = wakeup.min(remaining);
        }
        if wakeup.is_zero() {
            return;
        }
        tokio::time::sleep(wakeup).await;
        let scheduler_refresh_result = if self.redis_store.is_some() {
            self.refresh_scheduler_state_from_redis()
        } else {
            Ok(())
        };
        if let Err(err) = scheduler_refresh_result {
            tracing::debug!("Redis 调度协调恢复等待后同步并发状态失败: {}", err);
        }
    }

    async fn renew_dispatch_queue_lease_if_needed(
        &self,
        queue_guard: &mut Option<DispatchQueueGuard>,
    ) -> anyhow::Result<()> {
        let Some(guard) = queue_guard.as_mut() else {
            return Ok(());
        };
        if self.redis_store.is_none() || !guard.redis_renewal_is_due() {
            return Ok(());
        }
        match self
            .await_scheduler_redis_hot_outcome(
                "续期 Redis 调度排队 lease",
                || {},
                guard.renew_if_needed(),
            )
            .await
        {
            SchedulerRedisHotOutcome::Completed(()) => Ok(()),
            SchedulerRedisHotOutcome::LocalSchedulerOverloaded => {
                anyhow::bail!("本地账号调度容量暂不可用（本地 scheduler admission 已饱和）")
            }
            SchedulerRedisHotOutcome::Skipped => anyhow::bail!(
                "本地账号调度容量暂不可用（Redis 调度协调状态不可用，retry_after_secs={}）",
                self.scheduler_redis_retry_after_secs()
            ),
            SchedulerRedisHotOutcome::Failed { .. } => anyhow::bail!(
                "本地账号调度容量暂不可用（Redis 调度协调状态不可用，retry_after_secs={}）",
                self.scheduler_redis_retry_after_secs()
            ),
        }
    }

    fn try_enter_dispatch_queue(
        &self,
        max_wait: Option<StdDuration>,
    ) -> anyhow::Result<Option<DispatchQueueGuard>> {
        let max_queued = self.max_queued_requests();
        if !self.try_enter_local_dispatch_queue(max_queued) {
            return Ok(None);
        }
        let lease_policy = dispatch_queue_lease_policy(max_wait);
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            let lease_id = uuid::Uuid::new_v4().to_string();
            let acquire_lease_id = lease_id.clone();
            let Some(release_dispatcher) =
                self.scheduler_redis_release_dispatcher.as_ref().cloned()
            else {
                self.queued_requests.fetch_sub(1, Ordering::AcqRel);
                anyhow::bail!("本地账号调度容量暂不可用（release reconciliation 未初始化）");
            };
            let Some(release_reservation) = release_dispatcher.try_reserve() else {
                self.queued_requests.fetch_sub(1, Ordering::AcqRel);
                anyhow::bail!("本地账号调度容量暂不可用（release reconciliation 已饱和）");
            };
            let mut guard = DispatchQueueGuard::new(
                self.redis_store.clone(),
                Some(release_dispatcher),
                Some(release_reservation),
                self.queued_requests.clone(),
                Some(lease_id),
                lease_policy.ttl_secs,
                lease_policy.renewal_required,
            );
            match self.block_on_scheduler_redis_hot_outcome(
                "占用 Redis 调度排队名额",
                || guard.arm_redis_commit_unknown(),
                async move {
                    redis
                        .try_enter_dispatch_queue(
                            &acquire_lease_id,
                            max_queued,
                            lease_policy.ttl_secs,
                        )
                        .await
                },
            ) {
                SchedulerRedisHotOutcome::Completed(true) => {
                    guard.confirm_redis_acquired();
                    return Ok(Some(guard));
                }
                SchedulerRedisHotOutcome::Completed(false) => {
                    guard.confirm_redis_not_acquired();
                    drop(guard);
                    return Ok(None);
                }
                SchedulerRedisHotOutcome::Failed { commit_unknown } => {
                    if !commit_unknown {
                        guard.confirm_redis_not_acquired();
                    }
                    drop(guard);
                    anyhow::bail!(
                        "本地账号调度容量暂不可用（Redis 调度协调状态不可用，retry_after_secs={}）",
                        self.scheduler_redis_retry_after_secs()
                    );
                }
                SchedulerRedisHotOutcome::Skipped => {
                    guard.confirm_redis_not_acquired();
                    drop(guard);
                    anyhow::bail!(
                        "本地账号调度容量暂不可用（Redis 调度协调状态不可用，retry_after_secs={}）",
                        self.scheduler_redis_retry_after_secs()
                    );
                }
                SchedulerRedisHotOutcome::LocalSchedulerOverloaded => {
                    guard.confirm_redis_not_acquired();
                    drop(guard);
                    anyhow::bail!("本地账号调度容量暂不可用（本地 scheduler admission 已饱和）");
                }
            }
        }
        Ok(Some(DispatchQueueGuard::new(
            None,
            None,
            None,
            self.queued_requests.clone(),
            None,
            lease_policy.ttl_secs,
            lease_policy.renewal_required,
        )))
    }

    fn try_enter_local_only_dispatch_queue(
        &self,
        max_wait: Option<StdDuration>,
    ) -> Option<DispatchQueueGuard> {
        let max_queued = self.max_queued_requests();
        if !self.try_enter_local_dispatch_queue(max_queued) {
            return None;
        }
        let lease_policy = dispatch_queue_lease_policy(max_wait);
        Some(DispatchQueueGuard::new(
            None,
            None,
            None,
            self.queued_requests.clone(),
            None,
            lease_policy.ttl_secs,
            lease_policy.renewal_required,
        ))
    }

    fn global_capacity_state(&self) -> SchedulerGlobalCapacityState {
        self.local_global_capacity_state()
    }

    fn dispatch_max_wait(&self, acquire_mode: AcquireMode) -> Option<StdDuration> {
        if let Some(max_wait) = acquire_mode.max_wait_override() {
            return Some(max_wait);
        }
        let secs = self.config.lock().credential_dispatch_max_wait_secs;
        (secs > 0).then(|| StdDuration::from_secs(secs))
    }

    fn dispatch_wait_exceeded(
        &self,
        max_wait: Option<StdDuration>,
        started_at: Instant,
        now: Instant,
    ) -> Option<(StdDuration, StdDuration)> {
        let max_wait = max_wait?;
        let waited = now.saturating_duration_since(started_at);
        (waited >= max_wait).then_some((waited, max_wait))
    }

    fn dispatch_wait_remaining(
        &self,
        max_wait: Option<StdDuration>,
        started_at: Instant,
        now: Instant,
    ) -> Option<StdDuration> {
        let max_wait = max_wait?;
        Some(max_wait.saturating_sub(now.saturating_duration_since(started_at)))
    }

    /// 判断排除当前凭据后，本次请求是否还有其他可用凭据可 fallback。
    pub fn has_alternate_usable_credential(
        &self,
        model: Option<&str>,
        excluded_ids: &HashSet<u64>,
        current_id: u64,
    ) -> bool {
        if let Err(err) = self.refresh_scheduler_state_from_redis() {
            tracing::warn!("判断备选凭据前同步 Redis 调度状态失败: {}", err);
            return false;
        }
        self.has_alternate_usable_credential_from_current_state(model, excluded_ids, current_id)
    }

    /// 判断当前本机内存态中是否还有其他可调度凭据。
    ///
    /// 这个方法不触发 Redis/PgSQL 同步，用于上游瞬态失败后的当前请求 retry
    /// 决策，避免失败路径为了换账号再增加一次存储热路径等待。
    pub fn has_alternate_usable_credential_cached(
        &self,
        model: Option<&str>,
        excluded_ids: &HashSet<u64>,
        current_id: u64,
    ) -> bool {
        self.has_alternate_usable_credential_from_current_state(model, excluded_ids, current_id)
    }

    fn has_alternate_usable_credential_from_current_state(
        &self,
        model: Option<&str>,
        excluded_ids: &HashSet<u64>,
        current_id: u64,
    ) -> bool {
        let mut entries = self.entries.lock();
        let now = Instant::now();
        let config = self.config.lock().clone();
        let max_concurrent_requests = config.credential_max_concurrent_requests;
        let global_rpm = config.credential_rpm.unwrap_or(0);
        if self.redis_store.is_none() {
            refresh_local_selection_windows_locked(&mut entries, now);
        }
        let proxy_resources = self.proxy_resources.lock();
        entries.iter().any(|entry| {
            entry.id != current_id
                && !excluded_ids.contains(&entry.id)
                && credential_is_dispatchable(
                    &proxy_resources,
                    entry,
                    model,
                    now,
                    max_concurrent_requests,
                    global_rpm,
                    1,
                )
        })
    }

    fn exclude_credentials_requiring_refresh(&self, excluded_ids: &mut HashSet<u64>) -> usize {
        let entries = self.entries.lock();
        let before = excluded_ids.len();
        for entry in entries.iter() {
            if !entry.credentials.is_api_key_credential() && is_token_expired(&entry.credentials) {
                excluded_ids.insert(entry.id);
            }
        }
        excluded_ids.len().saturating_sub(before)
    }

    fn has_usable_credential_excluding_from_current_state(
        &self,
        model: Option<&str>,
        excluded_ids: &HashSet<u64>,
    ) -> bool {
        let entries = self.entries.lock();
        let proxy_resources = self.proxy_resources.lock();
        entries.iter().any(|entry| {
            credential_is_dispatch_candidate(&proxy_resources, entry, model, excluded_ids)
        })
    }

    /// 根据负载均衡模式选择下一个凭据，并排除本次请求已临时失败的凭据。
    fn select_next_credential_excluding(
        &self,
        model: Option<&str>,
        excluded_ids: &HashSet<u64>,
        request_weight_units: u32,
    ) -> Option<(u64, KiroCredentials)> {
        if let Err(err) = self.refresh_scheduler_state_from_redis() {
            tracing::warn!("选择凭据前同步 Redis 调度状态失败: {}", err);
            return None;
        }
        let entries = self.entries.lock();
        let proxy_resources = self.proxy_resources.lock();
        let now = Instant::now();
        let config = self.config.lock().clone();
        let max_concurrent_requests = config.credential_max_concurrent_requests;
        let global_rpm = config.credential_rpm.unwrap_or(0);
        let global_in_flight = entries
            .iter()
            .map(|entry| entry.in_flight_requests)
            .sum::<u32>();
        let global_has_capacity = global_has_concurrency_capacity(
            global_in_flight,
            config.dispatch_global_max_concurrent_requests,
            request_weight_units,
        );

        let mut available = Vec::new();
        let mut ready = Vec::new();
        let mut warming = Vec::new();
        let mut total_recent = 0u64;
        let mut warming_recent = 0u64;

        if global_has_capacity {
            for entry in entries.iter() {
                if excluded_ids.contains(&entry.id)
                    || !credential_is_dispatchable(
                        &proxy_resources,
                        entry,
                        model,
                        now,
                        max_concurrent_requests,
                        global_rpm,
                        request_weight_units,
                    )
                {
                    continue;
                }

                let recent = entry.health.recent_selection_count_60s as u64;
                total_recent = total_recent.saturating_add(recent);
                available.push(entry);
                if entry.warmup_remaining > 0 {
                    warming_recent = warming_recent.saturating_add(recent);
                    warming.push(entry);
                } else {
                    ready.push(entry);
                }
            }
        }

        if available.is_empty() {
            return None;
        }

        let select_warming = should_select_warming_from_totals(
            &config,
            ready.len(),
            warming.len(),
            total_recent,
            warming_recent,
        );
        let candidates = if select_warming { &warming } else { &ready };
        let mode = self.load_balancing_mode.lock().clone();

        match mode.as_str() {
            "health_balanced" => {
                // 预热不是后台主动打流量，而是在真实业务请求中按目标份额参与调度；
                // 份额按预热账号数量放大并受最大预热份额限制，避免批量导入后长期吃不到流量。
                let entry = select_health_weighted(
                    candidates,
                    model,
                    Utc::now().timestamp_millis(),
                    &config,
                )?;

                Some((entry.id, entry.credentials.clone()))
            }
            "balanced" => {
                let entry = candidates
                    .iter()
                    .min_by_key(|entry| balanced_selection_key(entry))?;
                Some((entry.id, entry.credentials.clone()))
            }
            "weighted_least_inflight" => {
                let entry = select_weighted_least_inflight(
                    candidates,
                    model,
                    Utc::now().timestamp_millis(),
                    &config,
                )?;
                Some((entry.id, entry.credentials.clone()))
            }
            _ => {
                // priority 模式（默认）：优先级仍是第一排序，但同优先级账号优先选低并发。
                let entry = candidates
                    .iter()
                    .min_by_key(|e| priority_selection_key(e))?;
                Some((entry.id, entry.credentials.clone()))
            }
        }
    }

    fn get_bound_credential(
        &self,
        bound_id: u64,
        model: Option<&str>,
        excluded_ids: &HashSet<u64>,
    ) -> Option<(u64, KiroCredentials)> {
        if excluded_ids.contains(&bound_id) {
            return None;
        }

        let entries = self.entries.lock();
        let proxy_resources = self.proxy_resources.lock();
        let now = Instant::now();
        let global_rpm = self.config.lock().credential_rpm.unwrap_or(0);
        entries
            .iter()
            .find(|e| {
                e.id == bound_id
                    && credential_is_temporarily_available(
                        &proxy_resources,
                        e,
                        model,
                        now,
                        global_rpm,
                    )
            })
            .map(|e| (e.id, e.credentials.clone()))
    }

    fn bound_credential_is_capacity_blocked(
        &self,
        bound_id: u64,
        model: Option<&str>,
        excluded_ids: &HashSet<u64>,
        request_weight_units: u32,
    ) -> bool {
        if excluded_ids.contains(&bound_id) {
            return false;
        }

        let entries = self.entries.lock();
        let proxy_resources = self.proxy_resources.lock();
        let config = self.config.lock();
        let Some(entry) = entries.iter().find(|entry| entry.id == bound_id) else {
            return false;
        };
        if !credential_is_usable_for_model(entry, model)
            || !credential_proxy_is_dispatchable(&entry.credentials, &proxy_resources)
        {
            return false;
        }
        let now = Instant::now();
        let global_rpm = config.credential_rpm.unwrap_or(0);
        if entry_cooldown_remaining(entry, model, now).is_some()
            || entry_rate_limit_remaining(entry, global_rpm, now).is_some()
        {
            return false;
        }

        let effective_max =
            effective_max_concurrent_requests(entry, config.credential_max_concurrent_requests);
        let lease_weight_units = effective_weight_for_limit(
            effective_weight_for_limit(request_weight_units, effective_max),
            config.dispatch_global_max_concurrent_requests,
        );
        let global_in_flight = entries
            .iter()
            .map(|entry| entry.in_flight_requests)
            .sum::<u32>();
        !entry_has_concurrency_capacity(
            entry,
            config.credential_max_concurrent_requests,
            lease_weight_units,
        ) || !global_has_concurrency_capacity(
            global_in_flight,
            config.dispatch_global_max_concurrent_requests,
            lease_weight_units,
        )
    }

    async fn bound_credential_id_for_request(&self, session_id: &str) -> Option<u64> {
        #[cfg(test)]
        self.request_binding_snapshot_reads
            .fetch_add(1, Ordering::AcqRel);
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            let session_id_owned = session_id.to_string();
            if let Some(binding) = self
                .await_scheduler_redis_affinity("读取 Redis 会话绑定", async move {
                    redis.get_session_binding(&session_id_owned).await
                })
                .await
            {
                cache_sticky_redis_binding(&self.session_bindings, session_id, binding);
            }
        }
        sticky_bound_credential_id(&self.session_bindings, session_id)
    }

    #[allow(dead_code)]
    fn bound_credential_id(&self, session_id: &str) -> Option<u64> {
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            let session_id_owned = session_id.to_string();
            if let Some(binding) = self
                .block_on_scheduler_redis_affinity("读取 Redis 会话绑定", async move {
                    redis.get_session_binding(&session_id_owned).await
                })
            {
                cache_sticky_redis_binding(&self.session_bindings, session_id, binding);
            }
        }
        sticky_bound_credential_id(&self.session_bindings, session_id)
    }

    fn bound_credential_is_permanently_unavailable(&self, bound_id: u64) -> bool {
        self.entries
            .lock()
            .iter()
            .find(|entry| entry.id == bound_id)
            .is_none_or(|entry| entry.disabled)
    }

    #[cfg(test)]
    fn bind_session_to_credential(&self, session_id: &str, credential_id: u64) {
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            let session_id_owned = session_id.to_string();
            let binding = SchedulerSessionBinding {
                credential_id,
                last_used_at: Utc::now(),
                soft_failure_count: 0,
            };
            if let Some(actual) =
                self.block_on_scheduler_redis_affinity("原子写入 Redis 会话绑定", async move {
                    redis
                        .set_session_binding(
                            &session_id_owned,
                            &binding,
                            SESSION_BINDING_TTL_SECS as usize,
                        )
                        .await
                })
            {
                cache_sticky_redis_binding(&self.session_bindings, session_id, Some(actual));
                return;
            }
        }
        bind_sticky_session_to_credential(&self.session_bindings, session_id, credential_id);
    }

    async fn bind_session_to_credential_for_request(&self, session_id: &str, credential_id: u64) {
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            let session_id_owned = session_id.to_string();
            let binding = SchedulerSessionBinding {
                credential_id,
                last_used_at: Utc::now(),
                soft_failure_count: 0,
            };
            if let Some(actual) = self
                .await_scheduler_redis_affinity("原子写入 Redis 会话绑定", async move {
                    redis
                        .set_session_binding(
                            &session_id_owned,
                            &binding,
                            SESSION_BINDING_TTL_SECS as usize,
                        )
                        .await
                })
                .await
            {
                cache_sticky_redis_binding(&self.session_bindings, session_id, Some(actual));
                return;
            }
        }
        bind_sticky_session_to_credential(&self.session_bindings, session_id, credential_id);
    }

    /// 仅当指定会话当前绑定到该凭据时清理绑定。
    #[allow(dead_code)]
    pub fn unbind_session_if_bound_to(&self, session_id: &str, credential_id: u64) {
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            let session_id_owned = session_id.to_string();
            if let Some(deleted) =
                self.block_on_scheduler_redis_affinity("按凭据删除 Redis 会话绑定", async move {
                    redis
                        .delete_session_binding_if_bound_to(&session_id_owned, credential_id)
                        .await
                })
            {
                if deleted {
                    unbind_sticky_session_if_bound_to(
                        &self.session_bindings,
                        session_id,
                        credential_id,
                    );
                } else {
                    let _ = self.bound_credential_id(session_id);
                }
                return;
            }
        }
        unbind_sticky_session_if_bound_to(&self.session_bindings, session_id, credential_id);
    }

    /// 请求热路径使用的会话解绑：本地 sticky cache 先行，Redis affinity 异步 best-effort。
    ///
    /// 同步版 [`Self::unbind_session_if_bound_to`] 仍保留给管理/测试/需要读取 Redis
    /// 实际结果的路径；真实 API 请求不应因为 Redis affinity 抖动阻塞 terminal/retry path。
    pub fn unbind_session_if_bound_to_deferred(&self, session_id: &str, credential_id: u64) {
        unbind_sticky_session_if_bound_to(&self.session_bindings, session_id, credential_id);
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            let session_id_owned = session_id.to_string();
            spawn_best_effort_storage_task("异步按凭据删除 Redis 会话绑定", async move {
                redis
                    .delete_session_binding_if_bound_to(&session_id_owned, credential_id)
                    .await?;
                Ok(())
            });
        }
    }

    /// 清理某个凭据关联的所有会话绑定。
    pub fn unbind_sessions_for_credential(&self, credential_id: u64) {
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            let _ = self.block_on_scheduler_redis_affinity("删除 Redis 凭据会话绑定", async move {
                redis.delete_sessions_for_credential(credential_id).await?;
                Ok(())
            });
        }
        unbind_sticky_sessions_for_credential(&self.session_bindings, credential_id);
    }

    /// 记录绑定账号的一次软失败。返回 true 表示本次请求可以临时 fallback。
    #[allow(dead_code)]
    pub fn record_session_soft_failure(&self, session_id: &str, credential_id: u64) -> bool {
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            let session_id_owned = session_id.to_string();
            if let Some(binding) =
                self.block_on_scheduler_redis_affinity("原子记录 Redis 会话软失败", async move {
                    redis
                        .record_session_soft_failure_with_state(
                            &session_id_owned,
                            credential_id,
                            SESSION_BINDING_TTL_SECS as usize,
                        )
                        .await
                })
            {
                let should_fallback = binding
                    .as_ref()
                    .is_some_and(|binding| binding.soft_failure_count >= MAX_SESSION_SOFT_FAILURES);
                cache_sticky_redis_binding(&self.session_bindings, session_id, binding);
                return should_fallback;
            }
        }
        record_sticky_session_soft_failure(&self.session_bindings, session_id, credential_id)
    }

    pub fn record_session_soft_failure_deferred(
        &self,
        session_id: &str,
        credential_id: u64,
    ) -> bool {
        let should_fallback =
            record_sticky_session_soft_failure(&self.session_bindings, session_id, credential_id);
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            let session_id_owned = session_id.to_string();
            spawn_best_effort_storage_task("异步记录 Redis 会话软失败", async move {
                redis
                    .record_session_soft_failure_with_state(
                        &session_id_owned,
                        credential_id,
                        SESSION_BINDING_TTL_SECS as usize,
                    )
                    .await?;
                Ok(())
            });
        }
        should_fallback
    }

    /// 清理绑定账号的软失败计数。
    pub fn clear_session_soft_failure(&self, session_id: &str, credential_id: u64) {
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            let session_id_owned = session_id.to_string();
            if let Some(binding) =
                self.block_on_scheduler_redis_affinity("原子清理 Redis 会话软失败", async move {
                    redis
                        .clear_session_soft_failure_with_state(
                            &session_id_owned,
                            credential_id,
                            SESSION_BINDING_TTL_SECS as usize,
                        )
                        .await
                })
            {
                cache_sticky_redis_binding(&self.session_bindings, session_id, binding);
                return;
            }
        }
        clear_sticky_session_soft_failure(&self.session_bindings, session_id, credential_id);
    }

    pub fn clear_session_soft_failure_deferred(&self, session_id: &str, credential_id: u64) {
        clear_sticky_session_soft_failure(&self.session_bindings, session_id, credential_id);
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            let session_id_owned = session_id.to_string();
            spawn_best_effort_storage_task("异步清理 Redis 会话软失败", async move {
                redis
                    .clear_session_soft_failure_with_state(
                        &session_id_owned,
                        credential_id,
                        SESSION_BINDING_TTL_SECS as usize,
                    )
                    .await?;
                Ok(())
            });
        }
    }

    /// 获取 API 调用上下文
    ///
    /// 返回绑定了 id、credentials 和 token 的调用上下文
    /// 确保整个 API 调用过程中使用一致的凭据信息
    ///
    /// 如果 Token 过期或即将过期，会自动刷新
    /// 只有明确的 400 invalid_grant 会永久禁用凭据；credential auth 与 429 进入有限
    /// 冷却，传输、上游、解析及内部协调失败只在当前请求中排除，不改变凭据健康。
    ///
    /// # 参数
    /// - `model`: 可选的模型名称，用于过滤支持该模型的凭据（如 opus 模型需要付费订阅）
    #[cfg(test)]
    pub async fn acquire_context(&self, model: Option<&str>) -> anyhow::Result<CallContext> {
        self.acquire_context_for_session(model, None, &HashSet::new())
            .await
    }

    /// 获取指定凭据的 API 调用上下文，不参与负载均衡或会话绑定。
    ///
    /// 仅用于 Admin 手动模型调用测试；即使凭据已被禁用，也允许验证一次上游模型调用。
    pub async fn acquire_context_for_credential(&self, id: u64) -> anyhow::Result<CallContext> {
        let credentials = {
            let entries = self.entries.lock();
            let entry = entries
                .iter()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            entry.credentials.clone()
        };

        self.try_ensure_token(id, &credentials, false).await
    }

    /// 获取 API 调用上下文，优先保持同一会话使用同一个凭据。
    ///
    /// `excluded_ids` 只作用于本次请求，用于 sticky-aware retry 的临时 fallback。
    #[cfg(test)]
    pub async fn acquire_context_for_session(
        &self,
        model: Option<&str>,
        session_id: Option<&str>,
        excluded_ids: &HashSet<u64>,
    ) -> anyhow::Result<CallContext> {
        self.acquire_context_for_session_with_mode(
            model,
            session_id,
            excluded_ids,
            AcquireMode::WaitForCapacity,
            1,
        )
        .await
    }

    /// 获取 API 调用上下文，可选择在本地容量不足时立即返回。
    ///
    /// `FailFastOnCapacity` 仅用于外部备用池预检：如果无法立即拿到本地凭据并发
    /// lease，不进入本地等待队列，让上层可以直接路由到外部池。默认调用仍应使用
    /// [`Self::acquire_context_for_session`]，保持原有等待/排队语义。
    #[cfg(test)]
    pub async fn acquire_context_for_session_with_mode(
        &self,
        model: Option<&str>,
        session_id: Option<&str>,
        excluded_ids: &HashSet<u64>,
        acquire_mode: AcquireMode,
        request_weight_units: u32,
    ) -> anyhow::Result<CallContext> {
        self.acquire_context_for_session_with_mode_and_auxiliary_budget(
            model,
            session_id,
            excluded_ids,
            acquire_mode,
            request_weight_units,
            None,
        )
        .await
    }

    pub(crate) async fn acquire_context_for_session_with_mode_and_auxiliary_budget(
        &self,
        model: Option<&str>,
        session_id: Option<&str>,
        excluded_ids: &HashSet<u64>,
        acquire_mode: AcquireMode,
        request_weight_units: u32,
        auxiliary_attempt_budget: Option<Arc<AuxiliaryAttemptBudget>>,
    ) -> anyhow::Result<CallContext> {
        let request_weight_units = normalize_capacity_weight_units(request_weight_units);
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum DispatchWaitReason {
            TemporarilyUnavailable,
            CapacityFull,
            RpmLimited,
        }
        enum AcquireDecision {
            Selected(u64, KiroCredentials, bool, bool),
            WaitForDispatch {
                reason: DispatchWaitReason,
                available: usize,
                total: usize,
                global_credential_max_concurrent_requests: u32,
                effective_credential_max_concurrent_requests: String,
                wait_for: Option<StdDuration>,
            },
        }

        let total = self.total_count();
        let max_attempts = (total * MAX_FAILURES_PER_CREDENTIAL as usize).max(1);
        let mut attempt_count = 0;
        let dispatch_wait_started_at = Instant::now();
        // One request uses one stable queue deadline. Runtime config updates apply to later
        // requests instead of changing an already-admitted Redis lease lifetime mid-wait.
        let dispatch_max_wait = self.dispatch_max_wait(acquire_mode);
        let mut queue_guard: Option<DispatchQueueGuard> = None;
        let mut local_excluded_ids = excluded_ids.clone();
        let mut slot_race_excluded_count = 0usize;
        let mut sticky_bound_release_grace_waited = false;
        let mut auxiliary_refresh_error: Option<anyhow::Error> = None;
        let mut health_neutral_refresh_error: Option<anyhow::Error> = None;
        let request_bound_id = match session_id {
            Some(session_id) => self.bound_credential_id_for_request(session_id).await,
            None => None,
        };

        loop {
            if let Some(retry_after) = self
                .local_pool_risk_circuit_snapshot(Instant::now())
                .retry_after
            {
                anyhow::bail!(
                    "本地账号池风险保护已打开（retry_after_secs={}）",
                    retry_after.as_secs().saturating_add(1).max(1)
                );
            }
            let mut capacity_waiter = self.capacity_signal.register();

            self.refresh_scheduler_state_from_redis()?;
            self.cleanup_expired_in_flight_leases_local_first();
            if auxiliary_refresh_error.is_none()
                && auxiliary_attempt_budget
                    .as_ref()
                    .is_some_and(|budget| budget.available_attempts() == 0)
            {
                let budget = auxiliary_attempt_budget
                    .as_ref()
                    .expect("checked auxiliary attempt budget");
                let snapshot = budget.snapshot();
                auxiliary_refresh_error = Some(
                    AuxiliaryAttemptBudgetExhausted {
                        kind: AuxiliaryAttemptKind::TokenRefresh,
                        max_attempts: snapshot.max_attempts,
                        consumed: snapshot.consumed,
                    }
                    .into(),
                );
                let excluded = self.exclude_credentials_requiring_refresh(&mut local_excluded_ids);
                tracing::debug!(
                    excluded_refresh_credentials = excluded,
                    "request auxiliary budget is exhausted; scanning only credentials that do not require refresh"
                );
            }
            if auxiliary_refresh_error.is_some()
                && !self
                    .has_usable_credential_excluding_from_current_state(model, &local_excluded_ids)
            {
                return Err(auxiliary_refresh_error
                    .take()
                    .expect("checked auxiliary refresh error"));
            }
            if health_neutral_refresh_error.is_some()
                && !self
                    .has_usable_credential_excluding_from_current_state(model, &local_excluded_ids)
            {
                return Err(health_neutral_refresh_error
                    .take()
                    .expect("checked health-neutral refresh error"));
            }
            if attempt_count >= max_attempts {
                anyhow::bail!(
                    "所有账号均无法获取有效 Token（可用: {}/{}）",
                    self.available_count(),
                    total
                );
            }
            if let Some(bound_id) = request_bound_id {
                if !sticky_bound_release_grace_waited
                    && matches!(acquire_mode, AcquireMode::FailFastOnCapacityWaitForRedis(_))
                    && self.bound_credential_is_capacity_blocked(
                        bound_id,
                        model,
                        &local_excluded_ids,
                        request_weight_units,
                    )
                {
                    sticky_bound_release_grace_waited = true;
                    let now = Instant::now();
                    if self
                        .dispatch_wait_exceeded(dispatch_max_wait, dispatch_wait_started_at, now)
                        .is_none()
                    {
                        let grace = self
                            .dispatch_wait_remaining(
                                dispatch_max_wait,
                                dispatch_wait_started_at,
                                now,
                            )
                            .map(|remaining| remaining.min(STICKY_BOUND_RELEASE_PROPAGATION_GRACE))
                            .unwrap_or(STICKY_BOUND_RELEASE_PROPAGATION_GRACE);
                        if !grace.is_zero() {
                            tracing::debug!(
                                credential_id = bound_id,
                                grace_ms = grace.as_millis() as u64,
                                "sticky 绑定账号当前仅受容量占用阻塞，短暂等待跨实例 Redis release 可见后重试原绑定"
                            );
                            self.sleep_for_scheduler_redis_recovery(
                                grace,
                                self.dispatch_wait_remaining(
                                    dispatch_max_wait,
                                    dispatch_wait_started_at,
                                    now,
                                ),
                            )
                            .await;
                            continue;
                        }
                    }
                }
            }

            let selection_reservation_guard = self.selection_reservation_gate.lock();
            let decision = {
                let bound_hit = request_bound_id.and_then(|bound_id| {
                    self.get_bound_credential(bound_id, model, &local_excluded_ids)
                });

                if let Some(hit) = bound_hit {
                    AcquireDecision::Selected(hit.0, hit.1, true, false)
                } else {
                    let fallback_from_sticky = request_bound_id.is_some();
                    {
                        // 根据负载均衡策略选择；priority 模式也会在同优先级账号之间优先低并发。
                        let mut best = self.select_next_credential_excluding(
                            model,
                            &local_excluded_ids,
                            request_weight_units,
                        );

                        // 没有可用凭据：如果是"自动禁用导致全灭"，做一次类似重启的自愈
                        if best.is_none() && self.auto_heal_too_many_failures_if_applicable() {
                            best = self.select_next_credential_excluding(
                                model,
                                &local_excluded_ids,
                                request_weight_units,
                            );
                        }

                        if let Some((new_id, new_creds)) = best {
                            // 更新 current_id
                            let mut current_id = self.current_id.lock();
                            *current_id = new_id;
                            AcquireDecision::Selected(
                                new_id,
                                new_creds,
                                false,
                                fallback_from_sticky,
                            )
                        } else {
                            let entries = self.entries.lock();
                            let proxy_resources = self.proxy_resources.lock();
                            let config = self.config.lock().clone();
                            // 注意：必须在 bail! 之前计算 available_count，
                            // 因为 available_count() 会尝试获取 entries 锁，
                            // 而此时我们已经持有该锁，会导致死锁
                            let available = entries.iter().filter(|e| !e.disabled).count();
                            let now = Instant::now();
                            let max_concurrent_requests = config.credential_max_concurrent_requests;
                            let global_rpm = config.credential_rpm.unwrap_or(0);
                            let global_in_flight = entries
                                .iter()
                                .map(|entry| entry.in_flight_requests)
                                .sum::<u32>();
                            let global_has_capacity = global_has_concurrency_capacity(
                                global_in_flight,
                                config.dispatch_global_max_concurrent_requests,
                                request_weight_units,
                            );
                            let model_usable = entries
                                .iter()
                                .filter(|e| credential_is_usable_for_model(e, model))
                                .count();
                            let usable = entries
                                .iter()
                                .filter(|e| {
                                    credential_is_usable_for_model(e, model)
                                        && credential_proxy_is_dispatchable(
                                            &e.credentials,
                                            &proxy_resources,
                                        )
                                })
                                .count();
                            let proxy_blocked = entries
                                .iter()
                                .filter(|e| {
                                    credential_is_usable_for_model(e, model)
                                        && !credential_proxy_is_dispatchable(
                                            &e.credentials,
                                            &proxy_resources,
                                        )
                                })
                                .count();
                            let dispatchable = if global_has_capacity {
                                entries
                                    .iter()
                                    .filter(|e| {
                                        !local_excluded_ids.contains(&e.id)
                                            && credential_is_dispatchable(
                                                &proxy_resources,
                                                e,
                                                model,
                                                now,
                                                max_concurrent_requests,
                                                global_rpm,
                                                request_weight_units,
                                            )
                                    })
                                    .count()
                            } else {
                                0
                            };
                            let effective_concurrency_range = format_effective_concurrency_range(
                                effective_concurrency_range_for_candidates(
                                    &entries,
                                    &proxy_resources,
                                    model,
                                    &local_excluded_ids,
                                    max_concurrent_requests,
                                ),
                            );
                            let excluded_usable = entries
                                .iter()
                                .filter(|e| {
                                    local_excluded_ids.contains(&e.id)
                                        && credential_is_usable_for_model(e, model)
                                        && credential_proxy_is_dispatchable(
                                            &e.credentials,
                                            &proxy_resources,
                                        )
                                })
                                .count();
                            if usable > 0 && excluded_usable >= usable {
                                if acquire_mode.is_fail_fast() && slot_race_excluded_count > 0 {
                                    anyhow::bail!(
                                        "本地账号调度容量暂不可用（本次请求因并发槽竞争临时排除了所有可用账号，可用: {}/{}, global_credential_max_concurrent_requests={}, effective_credential_max_concurrent_requests={}）",
                                        available,
                                        total,
                                        max_concurrent_requests,
                                        effective_concurrency_range
                                    );
                                }
                                anyhow::bail!(
                                    "本次请求临时排除了所有可用账号（可用: {}/{}, 临时排除: {}）",
                                    available,
                                    total,
                                    excluded_usable
                                );
                            }
                            if available > 0 && model_usable == 0 && model.is_some() {
                                anyhow::bail!(
                                    "没有支持当前模型的可用账号（可用: {}/{}）",
                                    available,
                                    total
                                );
                            }
                            if model_usable > 0 && usable == 0 && proxy_blocked >= model_usable {
                                anyhow::bail!(
                                    "所有可用账号均因代理资源不可用而不可调度（可用: {}/{}, 代理不可用: {}）",
                                    available,
                                    total,
                                    proxy_blocked
                                );
                            }
                            if usable > 0 && dispatchable > 0 {
                                tracing::debug!(
                                    available,
                                    total,
                                    usable,
                                    dispatchable,
                                    "调度候选在重检时恢复可用，重新选择凭据"
                                );
                                continue;
                            }
                            if usable > 0 && dispatchable == 0 {
                                let dispatch_candidate_count = entries
                                    .iter()
                                    .filter(|e| {
                                        credential_is_dispatch_candidate(
                                            &proxy_resources,
                                            e,
                                            model,
                                            &local_excluded_ids,
                                        )
                                    })
                                    .count();
                                let cooldown_blocked = entries
                                    .iter()
                                    .filter(|e| {
                                        credential_is_dispatch_candidate(
                                            &proxy_resources,
                                            e,
                                            model,
                                            &local_excluded_ids,
                                        ) && entry_cooldown_remaining(e, model, now).is_some()
                                    })
                                    .count();
                                let wait_for = min_dispatch_wait(
                                    &entries,
                                    &proxy_resources,
                                    model,
                                    &local_excluded_ids,
                                    now,
                                    global_rpm,
                                );
                                if dispatch_candidate_count > 0
                                    && cooldown_blocked >= dispatch_candidate_count
                                {
                                    let retry_after_secs = wait_for
                                        .map(|duration| duration.as_secs().saturating_add(1))
                                        .unwrap_or(1)
                                        .max(1);
                                    anyhow::bail!(
                                        "所有可用账号均处于上游临时冷却（可用: {}/{}, 临时可调度: 0, global_credential_max_concurrent_requests={}, effective_credential_max_concurrent_requests={}, retry_after_secs={}）",
                                        available,
                                        total,
                                        max_concurrent_requests,
                                        effective_concurrency_range,
                                        retry_after_secs
                                    );
                                }
                                let concurrency_blocked = concurrency_blocked_count(
                                    &entries,
                                    &proxy_resources,
                                    model,
                                    &local_excluded_ids,
                                    now,
                                    max_concurrent_requests,
                                    global_rpm,
                                    global_has_capacity,
                                    request_weight_units,
                                );
                                let rate_limit_blocked = rate_limit_blocked_count(
                                    &entries,
                                    &proxy_resources,
                                    model,
                                    &local_excluded_ids,
                                    now,
                                    global_rpm,
                                );
                                if concurrency_blocked > 0
                                    && concurrency_blocked >= dispatch_candidate_count
                                {
                                    AcquireDecision::WaitForDispatch {
                                        reason: DispatchWaitReason::CapacityFull,
                                        available,
                                        total,
                                        global_credential_max_concurrent_requests:
                                            max_concurrent_requests,
                                        effective_credential_max_concurrent_requests:
                                            effective_concurrency_range,
                                        wait_for: None,
                                    }
                                } else if rate_limit_blocked > 0
                                    && rate_limit_blocked >= dispatch_candidate_count
                                {
                                    AcquireDecision::WaitForDispatch {
                                        reason: DispatchWaitReason::RpmLimited,
                                        available,
                                        total,
                                        global_credential_max_concurrent_requests:
                                            max_concurrent_requests,
                                        effective_credential_max_concurrent_requests:
                                            effective_concurrency_range,
                                        wait_for,
                                    }
                                } else {
                                    AcquireDecision::WaitForDispatch {
                                        reason: DispatchWaitReason::TemporarilyUnavailable,
                                        available,
                                        total,
                                        global_credential_max_concurrent_requests:
                                            max_concurrent_requests,
                                        effective_credential_max_concurrent_requests:
                                            effective_concurrency_range,
                                        wait_for,
                                    }
                                }
                            } else {
                                anyhow::bail!("所有账号均已禁用（{}/{}）", available, total);
                            }
                        }
                    }
                }
            };
            let prepared_slot = match &decision {
                AcquireDecision::Selected(id, _, _, _) => {
                    self.prepare_in_flight_slot(*id, request_weight_units)
                }
                AcquireDecision::WaitForDispatch { .. } => None,
            };
            drop(selection_reservation_guard);

            let (id, credentials, sticky_bound, fallback_from_sticky) = match decision {
                AcquireDecision::Selected(id, credentials, sticky_bound, fallback_from_sticky) => {
                    (id, credentials, sticky_bound, fallback_from_sticky)
                }
                AcquireDecision::WaitForDispatch {
                    reason,
                    available,
                    total,
                    global_credential_max_concurrent_requests,
                    effective_credential_max_concurrent_requests,
                    wait_for,
                } => {
                    if acquire_mode.is_fail_fast() {
                        let retry_after_secs = wait_for
                            .map(|duration| duration.as_secs().saturating_add(1))
                            .unwrap_or(1)
                            .max(1);
                        if reason == DispatchWaitReason::RpmLimited {
                            anyhow::bail!(
                                "本地账号调度容量暂不可用（凭据 RPM 限制，可用: {}/{}, 临时可调度: 0, retry_after_secs={}）",
                                available,
                                total,
                                retry_after_secs
                            );
                        } else {
                            anyhow::bail!(
                                "本地账号调度容量暂不可用（可用: {}/{}, 临时可调度: 0, global_credential_max_concurrent_requests={}, effective_credential_max_concurrent_requests={}, retry_after_secs={}）",
                                available,
                                total,
                                global_credential_max_concurrent_requests,
                                effective_credential_max_concurrent_requests,
                                retry_after_secs
                            );
                        }
                    }
                    if queue_guard.is_none() {
                        queue_guard = self.try_enter_dispatch_queue(dispatch_max_wait)?;
                        if queue_guard.is_none() {
                            anyhow::bail!(
                                "账号调度等待队列已满（max_queued_requests={}, global_max_concurrent_requests={}）",
                                self.max_queued_requests(),
                                self.global_max_concurrent_requests()
                            );
                        }
                    }
                    let now = Instant::now();
                    let retry_after_secs = wait_for
                        .map(|duration| duration.as_secs().saturating_add(1))
                        .unwrap_or(0);
                    if let Some((waited, max_wait)) = self.dispatch_wait_exceeded(
                        dispatch_max_wait,
                        dispatch_wait_started_at,
                        now,
                    ) {
                        if reason == DispatchWaitReason::RpmLimited {
                            anyhow::bail!(
                                "账号调度排队等待超时（凭据 RPM 限制，可用: {}/{}, 临时可调度: 0, waited_secs={}, max_wait_secs={}, retry_after_secs={}）",
                                available,
                                total,
                                waited.as_secs(),
                                max_wait.as_secs(),
                                retry_after_secs.max(1)
                            );
                        } else {
                            anyhow::bail!(
                                "账号调度排队等待超时（可用: {}/{}, 临时可调度: 0, global_credential_max_concurrent_requests={}, effective_credential_max_concurrent_requests={}, waited_secs={}, max_wait_secs={}, retry_after_secs={}）",
                                available,
                                total,
                                global_credential_max_concurrent_requests,
                                effective_credential_max_concurrent_requests,
                                waited.as_secs(),
                                max_wait.as_secs(),
                                retry_after_secs.max(1)
                            );
                        }
                    }
                    tracing::debug!(
                        available,
                        total,
                        ?reason,
                        global_credential_max_concurrent_requests,
                        effective_credential_max_concurrent_requests,
                        retry_after_secs,
                        "所有可用凭据暂不可调度，进入排队等待"
                    );
                    self.renew_dispatch_queue_lease_if_needed(&mut queue_guard)
                        .await?;
                    self.wait_for_dispatch_capacity(
                        wait_for,
                        self.dispatch_wait_remaining(
                            dispatch_max_wait,
                            dispatch_wait_started_at,
                            now,
                        ),
                        &mut capacity_waiter,
                    )
                    .await;
                    continue;
                }
            };

            let in_flight_lease = match prepared_slot {
                Some(prepared) => match self.confirm_prepared_in_flight_slot(prepared).await {
                    Ok(lease) => lease,
                    Err(err) => {
                        let scheduler_redis_wait = err
                            .downcast_ref::<SchedulerRedisUnavailableError>()
                            .map(|err| (err.retry_after_secs, err.local_overloaded));
                        let Some((retry_after_secs, local_overloaded)) = scheduler_redis_wait
                        else {
                            return Err(err);
                        };
                        if queue_guard
                            .as_ref()
                            .is_some_and(|guard| guard.has_redis_lease())
                        {
                            drop(queue_guard.take());
                            return Err(err);
                        }
                        if acquire_mode.is_redis_degraded_fail_fast() {
                            return Err(err);
                        }
                        if queue_guard.is_none() {
                            queue_guard =
                                self.try_enter_local_only_dispatch_queue(dispatch_max_wait);
                            if queue_guard.is_none() {
                                anyhow::bail!(
                                    "账号调度等待队列已满（max_queued_requests={}, global_max_concurrent_requests={}）",
                                    self.max_queued_requests(),
                                    self.global_max_concurrent_requests()
                                );
                            }
                        }
                        let now = Instant::now();
                        if let Some((waited, max_wait)) = self.dispatch_wait_exceeded(
                            dispatch_max_wait,
                            dispatch_wait_started_at,
                            now,
                        ) {
                            anyhow::bail!(
                                "账号调度排队等待超时（Redis 调度协调状态不可用，waited_secs={}, max_wait_secs={}, retry_after_secs={}）",
                                waited.as_secs(),
                                max_wait.as_secs(),
                                retry_after_secs.max(1)
                            );
                        }
                        let wait_for = if local_overloaded {
                            StdDuration::from_millis(100)
                        } else {
                            StdDuration::from_secs(retry_after_secs.clamp(1, 30))
                        };
                        tracing::debug!(
                            credential_id = id,
                            retry_after_secs,
                            local_overloaded,
                            "Redis 调度协调暂不可用，普通本地请求进入本进程有界等待而不是快速失败"
                        );
                        self.sleep_for_scheduler_redis_recovery(
                            wait_for,
                            self.dispatch_wait_remaining(
                                dispatch_max_wait,
                                dispatch_wait_started_at,
                                now,
                            ),
                        )
                        .await;
                        continue;
                    }
                },
                None => None,
            };
            let Some(in_flight_lease) = in_flight_lease else {
                if sticky_bound {
                    if !sticky_bound_release_grace_waited
                        && matches!(acquire_mode, AcquireMode::FailFastOnCapacityWaitForRedis(_))
                    {
                        sticky_bound_release_grace_waited = true;
                        let now = Instant::now();
                        if self
                            .dispatch_wait_exceeded(
                                dispatch_max_wait,
                                dispatch_wait_started_at,
                                now,
                            )
                            .is_none()
                        {
                            let grace = self
                                .dispatch_wait_remaining(
                                    dispatch_max_wait,
                                    dispatch_wait_started_at,
                                    now,
                                )
                                .map(|remaining| {
                                    remaining.min(STICKY_BOUND_RELEASE_PROPAGATION_GRACE)
                                })
                                .unwrap_or(STICKY_BOUND_RELEASE_PROPAGATION_GRACE);
                            if !grace.is_zero() {
                                tracing::debug!(
                                    credential_id = id,
                                    grace_ms = grace.as_millis() as u64,
                                    "sticky 绑定账号并发槽暂满，短暂等待跨实例 Redis release 可见后重试原绑定"
                                );
                                self.sleep_for_scheduler_redis_recovery(
                                    grace,
                                    self.dispatch_wait_remaining(
                                        dispatch_max_wait,
                                        dispatch_wait_started_at,
                                        now,
                                    ),
                                )
                                .await;
                                continue;
                            }
                        }
                    }
                    if self.has_alternate_usable_credential_from_current_state(
                        model,
                        &local_excluded_ids,
                        id,
                    ) {
                        local_excluded_ids.insert(id);
                        attempt_count += 1;
                        slot_race_excluded_count += 1;
                        tracing::debug!(
                            credential_id = id,
                            excluded_count = local_excluded_ids.len(),
                            "sticky 绑定账号并发槽已满，本次请求保留原绑定并临时重选"
                        );
                        continue;
                    }
                }
                if acquire_mode.is_fail_fast() {
                    local_excluded_ids.insert(id);
                    attempt_count += 1;
                    slot_race_excluded_count += 1;
                    tracing::debug!(
                        credential_id = id,
                        excluded_count = local_excluded_ids.len(),
                        "fail-fast 预检选中账号后并发槽已满，本次请求临时排除并重选"
                    );
                    continue;
                }
                if queue_guard.is_none() {
                    queue_guard = self.try_enter_dispatch_queue(dispatch_max_wait)?;
                    if queue_guard.is_none() {
                        anyhow::bail!(
                            "账号调度等待队列已满（max_queued_requests={}, global_max_concurrent_requests={}）",
                            self.max_queued_requests(),
                            self.global_max_concurrent_requests()
                        );
                    }
                }
                let now = Instant::now();
                if let Some((waited, max_wait)) =
                    self.dispatch_wait_exceeded(dispatch_max_wait, dispatch_wait_started_at, now)
                {
                    let global_credential_max_concurrent_requests = self.max_concurrent_requests();
                    let effective_credential_max_concurrent_requests = {
                        let entries = self.entries.lock();
                        let proxy_resources = self.proxy_resources.lock();
                        format_effective_concurrency_range(
                            effective_concurrency_range_for_candidates(
                                &entries,
                                &proxy_resources,
                                model,
                                &local_excluded_ids,
                                global_credential_max_concurrent_requests,
                            ),
                        )
                    };
                    anyhow::bail!(
                        "账号调度排队等待超时（可用: {}/{}, 临时可调度: 0, global_credential_max_concurrent_requests={}, effective_credential_max_concurrent_requests={}, waited_secs={}, max_wait_secs={}, retry_after_secs=1）",
                        self.available_count(),
                        total,
                        global_credential_max_concurrent_requests,
                        effective_credential_max_concurrent_requests,
                        waited.as_secs(),
                        max_wait.as_secs()
                    );
                }
                tracing::debug!(
                    credential_id = id,
                    "选中凭据后并发槽已被其他请求占用，进入排队等待"
                );
                self.renew_dispatch_queue_lease_if_needed(&mut queue_guard)
                    .await?;
                self.wait_for_dispatch_capacity(
                    None,
                    self.dispatch_wait_remaining(dispatch_max_wait, dispatch_wait_started_at, now),
                    &mut capacity_waiter,
                )
                .await;
                continue;
            };
            capacity_waiter.finish_acquired();
            drop(queue_guard.take());

            // 尝试获取/刷新 Token
            match self
                .try_ensure_token_with_auxiliary_budget(
                    id,
                    &credentials,
                    true,
                    auxiliary_attempt_budget.clone(),
                )
                .await
            {
                Ok(ctx) => {
                    let selection_admission = match self
                        .reserve_scheduler_selection_for_request(ctx.id, request_weight_units)
                        .await
                    {
                        Ok(admission) => admission,
                        Err(err) => {
                            drop(in_flight_lease);
                            let Some(redis_error) =
                                err.downcast_ref::<SchedulerRedisUnavailableError>()
                            else {
                                return Err(err);
                            };
                            if queue_guard
                                .as_ref()
                                .is_some_and(|guard| guard.has_redis_lease())
                            {
                                drop(queue_guard.take());
                                return Err(err);
                            }
                            if acquire_mode.is_redis_degraded_fail_fast() {
                                return Err(err);
                            }
                            if queue_guard.is_none() {
                                queue_guard =
                                    self.try_enter_local_only_dispatch_queue(dispatch_max_wait);
                                if queue_guard.is_none() {
                                    anyhow::bail!(
                                        "账号调度等待队列已满（max_queued_requests={}, global_max_concurrent_requests={}）",
                                        self.max_queued_requests(),
                                        self.global_max_concurrent_requests()
                                    );
                                }
                            }
                            let now = Instant::now();
                            if let Some((waited, max_wait)) = self.dispatch_wait_exceeded(
                                dispatch_max_wait,
                                dispatch_wait_started_at,
                                now,
                            ) {
                                anyhow::bail!(
                                    "账号调度排队等待超时（Redis 调度协调状态不可用，waited_secs={}, max_wait_secs={}, retry_after_secs={}）",
                                    waited.as_secs(),
                                    max_wait.as_secs(),
                                    redis_error.retry_after_secs.max(1)
                                );
                            }
                            let wait_for = if redis_error.local_overloaded {
                                StdDuration::from_millis(100)
                            } else {
                                StdDuration::from_secs(redis_error.retry_after_secs.clamp(1, 30))
                            };
                            tracing::debug!(
                                credential_id = id,
                                retry_after_secs = redis_error.retry_after_secs,
                                local_overloaded = redis_error.local_overloaded,
                                "Redis 调度协调暂不可用，普通本地请求进入本进程有界等待而不是快速失败"
                            );
                            self.sleep_for_scheduler_redis_recovery(
                                wait_for,
                                self.dispatch_wait_remaining(
                                    dispatch_max_wait,
                                    dispatch_wait_started_at,
                                    now,
                                ),
                            )
                            .await;
                            continue;
                        }
                    };
                    match selection_admission {
                        SchedulerRedisSelectionAdmission::NotRequired => {
                            self.record_scheduler_selection(ctx.id, request_weight_units, true);
                            if let Err(err) = self.mark_rate_limited_at(ctx.id, Instant::now()) {
                                drop(in_flight_lease);
                                return Err(err);
                            }
                        }
                        SchedulerRedisSelectionAdmission::Recorded {
                            rate_limit_available_at,
                        } => {
                            self.record_scheduler_selection(ctx.id, request_weight_units, false);
                            if rate_limit_available_at.is_none() {
                                if let Err(err) = self.mark_rate_limited_at(ctx.id, Instant::now())
                                {
                                    drop(in_flight_lease);
                                    return Err(err);
                                }
                            }
                        }
                        SchedulerRedisSelectionAdmission::RateLimited(wait) => {
                            drop(in_flight_lease);
                            if acquire_mode.is_fail_fast() {
                                anyhow::bail!(
                                    "本地账号调度容量暂不可用（凭据 RPM 限制，retry_after_secs={}）",
                                    wait.retry_after.as_secs().saturating_add(1).max(1)
                                );
                            }
                            if queue_guard.is_none() {
                                queue_guard = self.try_enter_dispatch_queue(dispatch_max_wait)?;
                                if queue_guard.is_none() {
                                    anyhow::bail!(
                                        "账号调度等待队列已满（max_queued_requests={}, global_max_concurrent_requests={}）",
                                        self.max_queued_requests(),
                                        self.global_max_concurrent_requests()
                                    );
                                }
                            }
                            let now = Instant::now();
                            if let Some((waited, max_wait)) = self.dispatch_wait_exceeded(
                                dispatch_max_wait,
                                dispatch_wait_started_at,
                                now,
                            ) {
                                anyhow::bail!(
                                    "账号调度排队等待超时（凭据 RPM 限制，waited_secs={}, max_wait_secs={}, retry_after_secs={}）",
                                    waited.as_secs(),
                                    max_wait.as_secs(),
                                    wait.retry_after.as_secs().saturating_add(1)
                                );
                            }
                            tracing::debug!(
                                credential_id = id,
                                retry_after_secs = wait.retry_after.as_secs().saturating_add(1),
                                rate_limit_available_in_ms =
                                    wait.available_at.saturating_duration_since(now).as_millis()
                                        as u64,
                                "Redis 凭据 RPM 暂不可用，进入本进程等待"
                            );
                            self.renew_dispatch_queue_lease_if_needed(&mut queue_guard)
                                .await?;
                            self.wait_for_dispatch_capacity(
                                Some(wait.retry_after),
                                self.dispatch_wait_remaining(
                                    dispatch_max_wait,
                                    dispatch_wait_started_at,
                                    now,
                                ),
                                &mut capacity_waiter,
                            )
                            .await;
                            continue;
                        }
                    }
                    if let Some(sid) = session_id {
                        let should_bind = match request_bound_id {
                            None => true,
                            Some(bound_id) if bound_id == ctx.id => true,
                            Some(bound_id) => {
                                self.bound_credential_is_permanently_unavailable(bound_id)
                            }
                        };
                        if should_bind {
                            self.bind_session_to_credential_for_request(sid, ctx.id)
                                .await;
                        }
                    }
                    return Ok(CallContext {
                        sticky_bound,
                        fallback_from_sticky,
                        in_flight_lease: Some(in_flight_lease),
                        ..ctx
                    });
                }
                Err(e) => {
                    if e.downcast_ref::<AuxiliaryAttemptBudgetExhausted>()
                        .is_some()
                        || e.downcast_ref::<AuxiliaryConcurrencySaturated>().is_some()
                        || e.downcast_ref::<TokenRefreshAdmissionRejected>().is_some()
                    {
                        drop(in_flight_lease);
                        attempt_count += 1;
                        let excluded =
                            self.exclude_credentials_requiring_refresh(&mut local_excluded_ids);
                        tracing::warn!(
                            credential_id = id,
                            excluded_refresh_credentials = excluded,
                            "auxiliary refresh admission rejected; credential health is unchanged and this request will scan only ready-token/API-key candidates"
                        );
                        auxiliary_refresh_error = Some(e);
                        continue;
                    }
                    let failure =
                        e.downcast_ref::<RefreshFailure>()
                            .cloned()
                            .unwrap_or_else(|| {
                                RefreshFailure::new(
                                    RefreshFailureStage::Internal,
                                    RefreshFailureKind::Internal,
                                    None,
                                    None,
                                    false,
                                )
                            });
                    tracing::warn!(
                        credential_id = id,
                        refresh_stage = failure.stage.as_str(),
                        refresh_kind = failure.kind.as_str(),
                        upstream_status = ?failure.status,
                        retry_after_ms = ?failure
                            .retry_after
                            .map(|duration| duration.as_millis() as u64),
                        send_committed = failure.send_committed,
                        shared_failure_wave = failure.shared_failure_wave,
                        "token refresh failed"
                    );
                    let has_available = if failure.shared_failure_wave {
                        // The leader owns the credential-health mutation for this wave. A waiter
                        // only excludes this credential from its own request; repeating an auth
                        // cooldown, invalid-grant disable, or Retry-After here would turn caller
                        // concurrency into health-state amplification.
                        local_excluded_ids.insert(id);
                        health_neutral_refresh_error = Some(failure.into());
                        self.entries.lock().iter().any(|entry| !entry.disabled)
                    } else {
                        match failure.kind {
                            RefreshFailureKind::InvalidGrant => {
                                self.report_refresh_token_invalid(id)
                            }
                            RefreshFailureKind::CredentialAuth => self
                                .report_transient_failure_kind(
                                    id,
                                    None,
                                    TransientFailureKind::Auth,
                                    failure.retry_after,
                                    format!(
                                        "token_refresh_{}",
                                        RefreshFailureKind::CredentialAuth.as_str()
                                    ),
                                )?,
                            RefreshFailureKind::RateLimited => self.report_transient_failure_kind(
                                id,
                                None,
                                TransientFailureKind::RateLimit,
                                failure.retry_after,
                                format!(
                                    "token_refresh_{}",
                                    RefreshFailureKind::RateLimited.as_str()
                                ),
                            )?,
                            _ => {
                                // Infrastructure, upstream availability, transport, and response
                                // integrity failures do not describe credential health. Exclude
                                // this credential only for the current request so it cannot be
                                // retried in a tight loop, while leaving global health and
                                // persisted failure counts unchanged for natural recovery.
                                local_excluded_ids.insert(id);
                                health_neutral_refresh_error = Some(failure.into());
                                self.entries.lock().iter().any(|entry| !entry.disabled)
                            }
                        }
                    };
                    drop(in_flight_lease);
                    attempt_count += 1;
                    if !has_available {
                        anyhow::bail!("所有账号均已禁用（0/{}）", total);
                    }
                }
            }
        }
    }

    /// 选择优先级最高的未禁用凭据作为当前凭据（内部方法）
    ///
    /// 纯粹按优先级选择，不排除当前凭据，用于优先级变更后立即生效
    fn select_highest_priority(&self) {
        let entries = self.entries.lock();
        let mut current_id = self.current_id.lock();

        // 选择优先级最高的未禁用凭据（不排除当前凭据）
        if let Some(best) = entries
            .iter()
            .filter(|e| !e.disabled)
            .min_by_key(|e| e.credentials.priority)
        {
            if best.id != *current_id {
                tracing::info!(
                    "优先级变更后切换凭据: #{} -> #{}（优先级 {}）",
                    *current_id,
                    best.id,
                    best.credentials.priority
                );
                *current_id = best.id;
            }
        }
    }

    /// 尝试使用指定凭据获取有效 Token
    ///
    /// 使用双重检查锁定模式，确保同一时间只有一个刷新操作
    ///
    /// # Arguments
    /// * `id` - 凭据 ID，用于更新正确的条目
    /// * `credentials` - 凭据信息
    async fn reload_credential_for_refresh_until(
        &self,
        id: u64,
        deadline: tokio::time::Instant,
    ) -> anyhow::Result<KiroCredentials> {
        let Some(store) = self.postgres_store.as_ref() else {
            let entries = self.entries.lock();
            return entries
                .iter()
                .find(|entry| entry.id == id)
                .map(|entry| entry.credentials.clone())
                .ok_or_else(|| {
                    anyhow::Error::new(RefreshFailure::new(
                        RefreshFailureStage::Internal,
                        RefreshFailureKind::Internal,
                        None,
                        None,
                        false,
                    ))
                });
        };
        let (credentials, runtime_states) = credential_pgsql_sync_until(
            "等待跨实例刷新时读取 PgSQL 权威凭据",
            deadline,
            store.load_credentials_with_runtime_state(),
        )
        .await
        .map_err(|_| {
            let kind = if tokio::time::Instant::now() >= deadline {
                RefreshFailureKind::Timeout
            } else {
                RefreshFailureKind::Persistence
            };
            anyhow::Error::new(RefreshFailure::new(
                RefreshFailureStage::Persistence,
                kind,
                None,
                None,
                false,
            ))
        })?;
        let latest = credentials
            .into_iter()
            .find(|credential| credential.id == Some(id))
            .ok_or_else(|| {
                anyhow::Error::new(RefreshFailure::new(
                    RefreshFailureStage::Coordination,
                    RefreshFailureKind::Coordination,
                    None,
                    None,
                    false,
                ))
            })?;
        let mut entries = self.entries.lock();
        let entry = entries
            .iter_mut()
            .find(|entry| entry.id == id)
            .ok_or_else(|| {
                anyhow::Error::new(RefreshFailure::new(
                    RefreshFailureStage::Internal,
                    RefreshFailureKind::Internal,
                    None,
                    None,
                    false,
                ))
            })?;
        if latest.storage_revision >= entry.credentials.storage_revision {
            entry.credentials = latest;
            Self::recompute_entry_disabled(entry);
        }
        if let Some(state) = runtime_states.get(&id) {
            Self::apply_runtime_state_if_newer(entry, state);
        }
        Ok(entry.credentials.clone())
    }

    async fn try_ensure_token(
        &self,
        id: u64,
        credentials: &KiroCredentials,
        update_refresh_health: bool,
    ) -> anyhow::Result<CallContext> {
        self.try_ensure_token_with_budgets_and_auxiliary_budget(
            id,
            credentials,
            update_refresh_health,
            TokenRefreshBudgets::default(),
            None,
        )
        .await
    }

    async fn try_ensure_token_with_auxiliary_budget(
        &self,
        id: u64,
        credentials: &KiroCredentials,
        update_refresh_health: bool,
        auxiliary_attempt_budget: Option<Arc<AuxiliaryAttemptBudget>>,
    ) -> anyhow::Result<CallContext> {
        self.try_ensure_token_with_budgets_and_auxiliary_budget(
            id,
            credentials,
            update_refresh_health,
            TokenRefreshBudgets::default(),
            auxiliary_attempt_budget,
        )
        .await
    }

    #[cfg(test)]
    async fn try_ensure_token_with_budgets(
        &self,
        id: u64,
        credentials: &KiroCredentials,
        update_refresh_health: bool,
        budgets: TokenRefreshBudgets,
    ) -> anyhow::Result<CallContext> {
        self.try_ensure_token_with_budgets_and_auxiliary_budget(
            id,
            credentials,
            update_refresh_health,
            budgets,
            None,
        )
        .await
    }

    async fn try_ensure_token_with_budgets_and_auxiliary_budget(
        &self,
        id: u64,
        credentials: &KiroCredentials,
        update_refresh_health: bool,
        budgets: TokenRefreshBudgets,
        auxiliary_attempt_budget: Option<Arc<AuxiliaryAttemptBudget>>,
    ) -> anyhow::Result<CallContext> {
        // API Key 凭据直接使用 kiro_api_key 作为 Bearer Token，无需刷新
        if credentials.is_api_key_credential() {
            let credentials = self.resolve_proxy_for_credential(credentials.clone())?;
            let token = credentials
                .kiro_api_key
                .clone()
                .ok_or_else(|| anyhow::anyhow!("API Key 凭据缺少 kiroApiKey"))?;
            return Ok(CallContext {
                id,
                credentials,
                token,
                sticky_bound: false,
                fallback_from_sticky: false,
                in_flight_lease: None,
            });
        }

        // `is_token_expired` includes a five-minute safety margin. Using the wider ten-minute
        // "expiring soon" window here makes a freshly issued short-lived token immediately
        // refreshable again, so lock waiters serialize duplicate refresh sends instead of
        // coalescing behind the first successful refresh.
        let needs_refresh = is_token_expired(credentials);
        let deadlines = budgets.deadlines()?;

        let creds = if needs_refresh {
            // 同一凭据只允许一个刷新任务；不同凭据互不阻塞。
            let refresh_state = self.refresh_state_for_credential(id);
            let _guard =
                acquire_refresh_lock_until(&refresh_state.gate, deadlines.coordination, id).await?;
            let refresh_reset_generation = refresh_state.generation();

            // 第二次检查：获取锁后重新读取凭据，因为其他请求可能已经完成刷新
            let current_creds = {
                let entries = self.entries.lock();
                entries
                    .iter()
                    .find(|e| e.id == id)
                    .map(|e| e.credentials.clone())
                    .ok_or_else(|| anyhow::anyhow!("凭据 #{} 不存在", id))?
            };

            if is_token_expired(&current_creds) {
                let current_creds_for_proxy = self
                    .resolve_proxy_for_credential(current_creds.clone())
                    .map_err(|_| {
                        anyhow::Error::new(RefreshFailure::new(
                            RefreshFailureStage::Validation,
                            RefreshFailureKind::InvalidConfiguration,
                            None,
                            None,
                            false,
                        ))
                    })?;
                let effective_proxy = current_creds_for_proxy.effective_proxy(self.proxy.as_ref());
                let refresh_identity = {
                    let config = self.config.lock();
                    RefreshAttemptIdentity::from_refresh_request(
                        &current_creds_for_proxy,
                        &config,
                        effective_proxy.as_ref(),
                    )
                };
                if let Some(failure) = refresh_state.replay_failure(
                    &refresh_identity,
                    Instant::now(),
                    update_refresh_health,
                ) {
                    tracing::debug!(
                        credential_id = id,
                        refresh_stage = failure.stage.as_str(),
                        refresh_kind = failure.kind.as_str(),
                        "coalesced token refresh caller reused the current typed failure wave"
                    );
                    if update_refresh_health
                        && !failure.shared_failure_wave
                        && refresh_failure_requires_health_action(&failure)
                    {
                        self.apply_refresh_health_action(id, &failure, None, deadlines.total)
                            .await?;
                        return Err(failure.into_shared_failure_wave().into());
                    }
                    return Err(failure.into());
                }
                let mut redis_refresh_lease: Option<RedisRefreshLease> = None;
                let send_started = Arc::new(AtomicBool::new(false));
                let persisted_storage_revision = Arc::new(AtomicU64::new(0));
                let mut redis_lease_drop_guard = None;
                let mut refreshed_by_peer = None;
                if self.redis_store.is_some() {
                    match self
                        .begin_distributed_refresh_until(
                            id,
                            refresh_identity,
                            update_refresh_health,
                            deadlines.coordination,
                        )
                        .await?
                    {
                        DistributedRefreshDecision::Leader(lease) => {
                            redis_lease_drop_guard = self.redis_store.as_ref().map(|redis| {
                                RedisRefreshLeaseDropGuard::new(
                                    redis.clone(),
                                    self.postgres_store.clone(),
                                    lease.clone(),
                                    send_started.clone(),
                                    persisted_storage_revision.clone(),
                                    current_creds.access_token.clone(),
                                )
                            });
                            redis_refresh_lease = Some(lease);
                        }
                        DistributedRefreshDecision::Replay(failure) => {
                            return Err(failure.into());
                        }
                        DistributedRefreshDecision::Succeeded {
                            generation,
                            storage_revision,
                        } => {
                            let latest = self
                                .reload_credential_for_refresh_until(id, deadlines.coordination)
                                .await?;
                            let token_changed = latest.access_token != current_creds.access_token;
                            if latest.storage_revision < storage_revision
                                || storage_revision == 0
                                || is_token_expired(&latest)
                                || !token_changed
                            {
                                tracing::warn!(
                                    credential_id = id,
                                    refresh_generation = generation,
                                    announced_storage_revision = storage_revision,
                                    authoritative_storage_revision = latest.storage_revision,
                                    token_changed,
                                    "Redis refresh success did not match the PostgreSQL authority"
                                );
                                return Err(RefreshFailure::new(
                                    RefreshFailureStage::Coordination,
                                    RefreshFailureKind::Coordination,
                                    None,
                                    None,
                                    false,
                                )
                                .into());
                            }
                            refreshed_by_peer = Some(latest);
                        }
                    }
                }

                if let Some(latest) = refreshed_by_peer {
                    refresh_state.clear_failure();
                    latest
                } else {
                    let (refresh_source, refresh_source_for_proxy, refresh_effective_proxy) =
                        if let Some(lease) = redis_refresh_lease.as_ref() {
                            let authoritative = self
                                .reload_credential_for_refresh_until(id, deadlines.coordination)
                                .await?;
                            Self::ensure_refresh_state_generation(
                                &refresh_state,
                                refresh_reset_generation,
                                false,
                            )?;
                            let authoritative_for_proxy = self
                                .resolve_proxy_for_credential(authoritative.clone())
                                .map_err(|_| {
                                    anyhow::Error::new(RefreshFailure::new(
                                        RefreshFailureStage::Validation,
                                        RefreshFailureKind::InvalidConfiguration,
                                        None,
                                        None,
                                        false,
                                    ))
                                })?;
                            let authoritative_proxy =
                                authoritative_for_proxy.effective_proxy(self.proxy.as_ref());
                            let authoritative_identity = {
                                let config = self.config.lock();
                                RefreshAttemptIdentity::from_refresh_request(
                                    &authoritative_for_proxy,
                                    &config,
                                    authoritative_proxy.as_ref(),
                                )
                            };
                            if lease.identity != authoritative_identity.0 {
                                let failure = RefreshFailure::new(
                                    RefreshFailureStage::Coordination,
                                    RefreshFailureKind::Coordination,
                                    None,
                                    None,
                                    false,
                                );
                                let failure = self
                                    .complete_distributed_refresh_failure_until(
                                        lease,
                                        &failure,
                                        false,
                                        deadlines.total,
                                    )
                                    .await?;
                                if let Some(guard) = redis_lease_drop_guard.as_mut() {
                                    guard.disarm();
                                }
                                return Err(failure.into());
                            }
                            (authoritative, authoritative_for_proxy, authoritative_proxy)
                        } else {
                            (
                                current_creds.clone(),
                                current_creds_for_proxy.clone(),
                                effective_proxy.clone(),
                            )
                        };
                    let mut cancellation_guard = RefreshFailureWaveDropGuard::new(
                        refresh_state.clone(),
                        refresh_identity,
                        refresh_reset_generation,
                        send_started.clone(),
                    );
                    let refreshed_and_persisted = async {
                        // Validate the final refresh source after any Redis/PostgreSQL authority
                        // reload, but before constructing an HTTP client or consuming admission.
                        // Invalid local configuration is health-neutral and must not create
                        // avoidable client-cache/RPM/concurrency pressure during a failure wave.
                        validate_refresh_token(&refresh_source_for_proxy)?;
                        let config = self.runtime_config();
                        let admission = RefreshSendAdmission::new_with_send_started_marker(
                            auxiliary_attempt_budget.clone(),
                            self.auxiliary_concurrency_controller(),
                            self.auxiliary_runtime.token_refresh_admission_controller(),
                            send_started.clone(),
                        );
                        let client = self
                            .refresh_http_client(
                                config.tls_backend,
                                refresh_effective_proxy.as_ref(),
                            )
                            .await
                            .map_err(|_| {
                                anyhow::Error::new(RefreshFailure::new(
                                    RefreshFailureStage::Internal,
                                    RefreshFailureKind::Internal,
                                    None,
                                    None,
                                    false,
                                ))
                            })?;
                        let refresh_result = refresh_token_until(
                            &refresh_source_for_proxy,
                            &config,
                            client,
                            Some(admission),
                            deadlines.work,
                            "Token 刷新上游阶段",
                        )
                        .await;
                        let mut new_creds = refresh_result?;
                        Self::ensure_refresh_state_generation(
                            &refresh_state,
                            refresh_reset_generation,
                            true,
                        )?;
                        new_creds.proxy_url = refresh_source.proxy_url.clone();
                        new_creds.proxy_username = refresh_source.proxy_username.clone();
                        new_creds.proxy_password = refresh_source.proxy_password.clone();
                        new_creds.proxy_resource_id = refresh_source.proxy_resource_id;

                        if is_token_expired(&new_creds)
                            || new_creds.access_token == refresh_source.access_token
                        {
                            return Err(RefreshFailure::new(
                                RefreshFailureStage::ResponseValidate,
                                RefreshFailureKind::MissingToken,
                                Some(200),
                                None,
                                true,
                            )
                            .into());
                        }

                        new_creds = self
                            .persist_refreshed_credential_fields(
                                id,
                                &refresh_source,
                                new_creds,
                                true,
                                None,
                                deadlines.work,
                                deadlines.total,
                            )
                            .await
                            .map_err(|_| {
                                anyhow::Error::new(RefreshFailure::new(
                                    RefreshFailureStage::Persistence,
                                    RefreshFailureKind::Persistence,
                                    None,
                                    None,
                                    true,
                                ))
                            })?;
                        persisted_storage_revision
                            .store(new_creds.storage_revision, Ordering::Release);
                        Ok::<KiroCredentials, anyhow::Error>(new_creds)
                    }
                    .await;
                    if redis_refresh_lease.is_some() {
                        cancellation_guard.disarm();
                    }
                    let refreshed = match (redis_refresh_lease.as_ref(), refreshed_and_persisted) {
                        (Some(lease), Ok(refreshed)) => {
                            Self::ensure_refresh_state_generation(
                                &refresh_state,
                                refresh_reset_generation,
                                true,
                            )?;
                            self.complete_or_cancel_distributed_refresh_success_until(
                                lease,
                                refreshed.storage_revision,
                                deadlines.total,
                            )
                            .await?;
                            if let Some(guard) = redis_lease_drop_guard.as_mut() {
                                guard.disarm();
                            }
                            refreshed
                        }
                        (Some(lease), Err(error)) => {
                            let synthetic_failure = RefreshFailure::new(
                                RefreshFailureStage::Coordination,
                                RefreshFailureKind::Coordination,
                                None,
                                None,
                                false,
                            );
                            let failure = error
                                .downcast_ref::<RefreshFailure>()
                                .unwrap_or(&synthetic_failure);
                            let shared = self
                                .complete_distributed_refresh_failure_until(
                                    lease,
                                    failure,
                                    update_refresh_health,
                                    deadlines.total,
                                )
                                .await?;
                            if let Some(guard) = redis_lease_drop_guard.as_mut() {
                                guard.disarm();
                            }
                            if error.downcast_ref::<RefreshFailure>().is_some() {
                                return Err(shared.into());
                            }
                            return Err(error);
                        }
                        (None, Ok(refreshed)) => refreshed,
                        (None, Err(error)) => {
                            if let Some(failure) = error.downcast_ref::<RefreshFailure>() {
                                refresh_state.record_failure(
                                    refresh_identity,
                                    failure,
                                    Instant::now(),
                                    update_refresh_health,
                                );
                                cancellation_guard.disarm();
                                if update_refresh_health
                                    && refresh_failure_requires_health_action(failure)
                                {
                                    self.apply_refresh_health_action(
                                        id,
                                        failure,
                                        None,
                                        deadlines.total,
                                    )
                                    .await?;
                                    return Err(failure.clone().into_shared_failure_wave().into());
                                }
                            }
                            if !send_started.load(Ordering::Acquire) {
                                cancellation_guard.disarm();
                            }
                            return Err(error);
                        }
                    };
                    Self::ensure_refresh_state_generation(
                        &refresh_state,
                        refresh_reset_generation,
                        true,
                    )?;
                    {
                        let mut entries = self.entries.lock();
                        if let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) {
                            Self::merge_refresh_fields(&mut entry.credentials, &refreshed);
                        }
                    }
                    if let Some(redis) = &self.redis_store {
                        let payload = serde_json::json!({
                            "kind": "credentials_changed",
                            "reason": "token_refreshed",
                            "changedAt": Utc::now().to_rfc3339(),
                        })
                        .to_string();
                        match tokio::time::timeout_at(
                            deadlines.total,
                            redis.publish_credentials_changed(payload),
                        )
                        .await
                        {
                            Ok(Ok(())) => {}
                            Ok(Err(err)) => tracing::warn!(
                                credential_id = id,
                                "发布 Token 刷新通知失败，其他实例将通过定时同步恢复: {}",
                                err
                            ),
                            Err(_) => tracing::warn!(
                                credential_id = id,
                                "发布 Token 刷新通知超过总期限，其他实例将通过定时同步恢复"
                            ),
                        }
                    }
                    cancellation_guard.disarm();
                    refresh_state.clear_failure();
                    refreshed
                }
            } else {
                // 其他请求已经完成刷新，直接使用新凭据
                refresh_state.clear_failure();
                tracing::debug!("Token 已被其他请求刷新，跳过刷新");
                current_creds
            }
        } else {
            credentials.clone()
        };

        self.token_context_from_credentials_until(id, creds, update_refresh_health, deadlines.total)
            .await
    }

    async fn token_context_from_credentials_until(
        &self,
        id: u64,
        creds: KiroCredentials,
        update_refresh_health: bool,
        deadline: tokio::time::Instant,
    ) -> anyhow::Result<CallContext> {
        let creds = self.resolve_proxy_for_credential(creds)?;
        let token = creds
            .access_token
            .clone()
            .ok_or_else(|| anyhow::anyhow!("没有可用的 accessToken"))?;

        if update_refresh_health {
            let reset_generation = {
                let mut entries = self.entries.lock();
                if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                    let reset_generation =
                        (entry.refresh_failure_count != 0).then_some(entry.runtime_generation);
                    entry.refresh_failure_count = 0;
                    reset_generation
                } else {
                    None
                }
            };
            if let Some(expected_generation) = reset_generation {
                self.persist_runtime_patch_best_effort_until(
                    id,
                    CredentialRuntimeStatePatch {
                        refresh_failure_count: Some(0),
                        expected_generation: Some(expected_generation),
                        ..Default::default()
                    },
                    deadline,
                )
                .await;
            }
        }

        Ok(CallContext {
            id,
            credentials: creds,
            token,
            sticky_bound: false,
            fallback_from_sticky: false,
            in_flight_lease: None,
        })
    }

    fn resolve_proxy_for_credential(
        &self,
        mut creds: KiroCredentials,
    ) -> anyhow::Result<KiroCredentials> {
        if creds.proxy_url.is_some() {
            return Ok(creds);
        }
        let availability = {
            let resources = self.proxy_resources.lock();
            credential_proxy_availability(&creds, &resources)
        };
        let Some(availability) = availability else {
            return Ok(creds);
        };

        match availability {
            ProxyResourceAvailability::Available(resource) => {
                creds.proxy_url = Some(resource.proxy_url);
                creds.proxy_username = resource.proxy_username;
                creds.proxy_password = resource.proxy_password;
                Ok(creds)
            }
            unavailable => Err(proxy_unavailable_error(creds.id, unavailable)),
        }
    }

    fn preserve_proxy_fields(
        mut credentials: KiroCredentials,
        source: &KiroCredentials,
    ) -> KiroCredentials {
        credentials.proxy_url = source.proxy_url.clone();
        credentials.proxy_username = source.proxy_username.clone();
        credentials.proxy_password = source.proxy_password.clone();
        credentials.proxy_resource_id = source.proxy_resource_id;
        credentials
    }

    fn credential_from_entry(entry: &CredentialEntry) -> KiroCredentials {
        let mut cred = entry.credentials.clone();
        cred.id = Some(entry.id);
        cred.disabled = if entry.runtime_persistence_degraded {
            entry.credentials.disabled
        } else {
            entry.disabled
        };
        cred.canonicalize_auth_method();
        cred.normalize_supported_models();
        cred.normalize_api_key_defaults();
        cred.normalize_external_idp_defaults();
        cred
    }

    fn merge_refresh_fields(target: &mut KiroCredentials, source: &KiroCredentials) {
        target.access_token = source.access_token.clone();
        target.refresh_token = source.refresh_token.clone();
        target.profile_arn = source.profile_arn.clone();
        target.expires_at = source.expires_at.clone();
        target.scopes = source.scopes.clone();
        target.created_at = source.created_at.clone();
        target.updated_at = source.updated_at.clone();
        target.storage_revision = source.storage_revision;
    }

    fn merge_credential_update(
        base: &KiroCredentials,
        requested: &KiroCredentials,
        current: &KiroCredentials,
    ) -> anyhow::Result<KiroCredentials> {
        if base.id != requested.id || base.id != current.id {
            anyhow::bail!("凭据 CAS 三方合并的 id 不一致");
        }
        let base_value = serde_json::to_value(base)?;
        let requested_value = serde_json::to_value(requested)?;
        let mut merged_value = serde_json::to_value(current)?;
        let base_fields = base_value
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("凭据基线不是 JSON 对象"))?;
        let requested_fields = requested_value
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("凭据更新不是 JSON 对象"))?;
        let merged_fields = merged_value
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("当前凭据不是 JSON 对象"))?;
        let changed_fields: HashSet<&String> = base_fields
            .keys()
            .chain(requested_fields.keys())
            .filter(|key| base_fields.get(*key) != requested_fields.get(*key))
            .collect();
        for key in changed_fields {
            let base_value = base_fields.get(key);
            let requested_value = requested_fields.get(key);
            let current_value = merged_fields.get(key);
            if current_value != base_value && current_value != requested_value {
                anyhow::bail!("凭据字段 {} 已被其他实例并发修改", key);
            }
            if let Some(value) = requested_fields.get(key) {
                merged_fields.insert(key.clone(), value.clone());
            } else {
                merged_fields.remove(key);
            }
        }

        let mut merged: KiroCredentials = serde_json::from_value(merged_value)?;
        merged.id = current.id;
        merged.created_at = current.created_at.clone();
        merged.updated_at = current.updated_at.clone();
        merged.storage_revision = current.storage_revision;
        merged.canonicalize_auth_method();
        merged.normalize_supported_models();
        merged.normalize_api_key_defaults();
        merged.normalize_external_idp_defaults();
        Ok(merged)
    }

    fn credential_update_is_applied(
        base: &KiroCredentials,
        requested: &KiroCredentials,
        current: &KiroCredentials,
    ) -> anyhow::Result<bool> {
        let base_value = serde_json::to_value(base)?;
        let requested_value = serde_json::to_value(requested)?;
        let current_value = serde_json::to_value(current)?;
        let base_fields = base_value
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("凭据基线不是 JSON 对象"))?;
        let requested_fields = requested_value
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("凭据更新不是 JSON 对象"))?;
        let current_fields = current_value
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("当前凭据不是 JSON 对象"))?;
        Ok(base_fields
            .keys()
            .chain(requested_fields.keys())
            .filter(|key| base_fields.get(*key) != requested_fields.get(*key))
            .all(|key| current_fields.get(key) == requested_fields.get(key)))
    }

    fn credential_runtime_patch_is_applied(
        base: &KiroCredentials,
        requested: &KiroCredentials,
        current: &KiroCredentials,
        runtime: Option<&CredentialRuntimeStateRow>,
        patch: &CredentialRuntimeStatePatch,
    ) -> anyhow::Result<bool> {
        if !Self::credential_update_is_applied(base, requested, current)?
            || patch
                .credential_disabled
                .is_some_and(|disabled| current.disabled != disabled)
            || patch.last_used_at.is_some()
        {
            return Ok(false);
        }
        let Some(runtime) = runtime else {
            return Ok(false);
        };
        if let Some(expected_generation) = patch.expected_generation {
            let applied_generation = expected_generation
                .checked_add(u64::from(patch.advance_generation))
                .ok_or_else(|| anyhow::anyhow!("凭据运行态 generation 已溢出"))?;
            if runtime.generation != applied_generation {
                return Ok(false);
            }
        }
        if patch
            .failure_count
            .is_some_and(|value| runtime.failure_count != value)
            || patch
                .refresh_failure_count
                .is_some_and(|value| runtime.refresh_failure_count != value)
            || patch
                .warmup_remaining
                .is_some_and(|value| runtime.warmup_remaining != value)
        {
            return Ok(false);
        }
        Ok(match &patch.disabled_reason {
            CredentialRuntimeDisabledReasonPatch::Preserve => true,
            CredentialRuntimeDisabledReasonPatch::Set(reason) => {
                runtime.disabled_reason.as_ref() == Some(reason)
            }
            CredentialRuntimeDisabledReasonPatch::Clear => runtime.disabled_reason.is_none(),
        })
    }

    fn refresh_fields_match(
        current: &KiroCredentials,
        requested: &CredentialRefreshFieldsPatch,
    ) -> bool {
        requested
            .access_token
            .as_ref()
            .is_none_or(|value| current.access_token.as_ref() == Some(value))
            && requested
                .refresh_token
                .as_ref()
                .is_none_or(|value| current.refresh_token.as_ref() == Some(value))
            && requested
                .profile_arn
                .as_ref()
                .is_none_or(|value| current.profile_arn.as_ref() == Some(value))
            && requested
                .expires_at
                .as_ref()
                .is_none_or(|value| current.expires_at.as_ref() == Some(value))
            && requested
                .scopes
                .as_ref()
                .is_none_or(|value| current.scopes.as_ref() == Some(value))
    }

    async fn persist_refreshed_credential_fields(
        &self,
        id: u64,
        expected_credentials: &KiroCredentials,
        refreshed_credentials: KiroCredentials,
        accept_fresh_conflict: bool,
        rejected_access_token: Option<&str>,
        work_deadline: tokio::time::Instant,
        reconciliation_deadline: tokio::time::Instant,
    ) -> anyhow::Result<KiroCredentials> {
        let Some(store) = &self.postgres_store else {
            return Ok(refreshed_credentials);
        };
        let expected = CredentialRefreshExpectedContext::from_credentials(expected_credentials)?;
        let patch = CredentialRefreshFieldsPatch {
            access_token: refreshed_credentials.access_token.clone(),
            refresh_token: refreshed_credentials.refresh_token.clone(),
            profile_arn: refreshed_credentials.profile_arn.clone(),
            expires_at: refreshed_credentials.expires_at.clone(),
            scopes: refreshed_credentials.scopes.clone(),
        };
        let first_attempt = credential_pgsql_sync_until(
            "Token 刷新后字段级 CAS 写入 PgSQL",
            work_deadline,
            store.update_credential_refresh_fields_cas(id, &expected, &patch),
        )
        .await;
        let outcome = match first_attempt {
            Ok(outcome) => outcome,
            Err(first_err) => {
                tracing::warn!(
                    credential_id = id,
                    "Token 字段级 CAS 首次结果不确定，使用相同前置条件安全重试: {}",
                    first_err
                );
                match credential_pgsql_sync_until(
                    "Token 刷新后字段级 CAS 安全重试",
                    reconciliation_deadline,
                    store.update_credential_refresh_fields_cas(id, &expected, &patch),
                )
                .await
                {
                    Ok(outcome) => outcome,
                    Err(retry_err) => {
                        let current = credential_pgsql_sync_until(
                            "Token 刷新 CAS 未知提交后读取 PgSQL 权威凭据",
                            reconciliation_deadline,
                            store.load_credentials(),
                        )
                        .await?
                        .into_iter()
                        .find(|credential| credential.id == Some(id));
                        if let Some(current) = current {
                            if Self::refresh_fields_match(&current, &patch) {
                                return Ok(current);
                            }
                        }
                        return Err(anyhow::anyhow!(
                            "Token 刷新 CAS 结果不确定；首次错误: {}；重试错误: {}",
                            first_err,
                            retry_err
                        ));
                    }
                }
            }
        };
        match outcome {
            CredentialRefreshFieldsCasOutcome::Applied(saved) => Ok(saved),
            CredentialRefreshFieldsCasOutcome::Conflict {
                current: Some(current),
            } if Self::refresh_fields_match(&current, &patch)
                || (accept_fresh_conflict
                    && !is_token_expired(&current)
                    && rejected_access_token.is_none_or(|rejected| {
                        current.access_token.as_deref() != Some(rejected)
                    })) =>
            {
                tracing::debug!(
                    credential_id = id,
                    "Token 已由其他写入刷新，采用 PgSQL 权威凭据"
                );
                Ok(current)
            }
            CredentialRefreshFieldsCasOutcome::Conflict { current: Some(_) } => {
                anyhow::bail!("凭据 #{} 刷新上下文已变更，拒绝写入旧刷新结果", id)
            }
            CredentialRefreshFieldsCasOutcome::Conflict { current: None } => {
                anyhow::bail!("凭据 #{} 已删除，拒绝写入刷新结果", id)
            }
        }
    }

    fn apply_runtime_state_if_newer(
        entry: &mut CredentialEntry,
        state: &CredentialRuntimeStateRow,
    ) -> bool {
        if state.revision <= entry.runtime_revision {
            return false;
        }
        let persisted_reason = state
            .disabled_reason
            .as_deref()
            .and_then(DisabledReason::from_str);
        let next_reason = persisted_reason
            .or_else(|| entry.credentials.disabled.then_some(DisabledReason::Manual));
        let next_disabled = entry.runtime_persistence_quarantined || next_reason.is_some();
        let changed = entry.failure_count != state.failure_count
            || entry.refresh_failure_count != state.refresh_failure_count
            || entry.warmup_remaining != state.warmup_remaining
            || entry.runtime_generation != state.generation
            || entry.disabled != next_disabled
            || entry.disabled_reason != next_reason;
        entry.failure_count = state.failure_count;
        entry.refresh_failure_count = state.refresh_failure_count;
        entry.warmup_remaining = state.warmup_remaining;
        entry.disabled = next_disabled;
        entry.disabled_reason = next_reason;
        entry.runtime_generation = state.generation;
        entry.runtime_revision = state.revision;
        changed
    }

    fn recompute_entry_disabled(entry: &mut CredentialEntry) {
        if entry.credentials.disabled && entry.disabled_reason.is_none() {
            entry.disabled_reason = Some(DisabledReason::Manual);
        }
        entry.disabled = entry.runtime_persistence_quarantined
            || entry.credentials.disabled
            || entry.disabled_reason.is_some();
    }

    /// 非破坏性地将当前凭据快照 upsert 到 PgSQL。
    ///
    /// 该方法只用于旧数据补全、环境变量凭据导入等 bootstrap 场景。底层
    /// `save_credentials` 不再删除未出现在当前内存快照里的数据库凭据。
    fn persist_credentials(&self) -> anyhow::Result<bool> {
        let credentials: Vec<KiroCredentials> = {
            let entries = self.entries.lock();
            entries.iter().map(Self::credential_from_entry).collect()
        };

        let Some(store) = &self.postgres_store else {
            return Ok(false);
        };
        let store = store.clone();
        let saved = block_on_credential_pgsql("保存凭据到 PgSQL", async move {
            store.save_credentials(&credentials).await
        })?;
        let saved_by_id: HashMap<u64, KiroCredentials> = saved
            .into_iter()
            .filter_map(|credential| credential.id.map(|id| (id, credential)))
            .collect();
        let mut entries = self.entries.lock();
        for entry in entries.iter_mut() {
            let Some(saved) = saved_by_id.get(&entry.id) else {
                continue;
            };
            if saved.storage_revision >= entry.credentials.storage_revision {
                entry.credentials = saved.clone();
                Self::recompute_entry_disabled(entry);
            }
        }
        tracing::debug!("已保存凭据到 PgSQL");
        Ok(true)
    }

    fn persist_credential_update(
        &self,
        base: &KiroCredentials,
        requested: &KiroCredentials,
    ) -> anyhow::Result<KiroCredentials> {
        let id = base
            .id
            .filter(|id| requested.id == Some(*id))
            .ok_or_else(|| anyhow::anyhow!("凭据 CAS 更新缺少一致的 id"))?;
        let saved = if let Some(store) = &self.postgres_store {
            let store = store.clone();
            let base = base.clone();
            let requested = requested.clone();
            block_on_storage("保存凭据到 PgSQL", async move {
                let deadline = tokio::time::Instant::now() + CREDENTIAL_PGSQL_WORKFLOW_TIMEOUT;
                let mut candidate = requested.clone();
                let mut last_error = None;
                for attempt in 1..=4 {
                    match credential_pgsql_sync_until(
                        "保存凭据到 PgSQL",
                        deadline,
                        store.upsert_credential(&candidate),
                    )
                    .await
                    {
                        Ok(CredentialUpsertCasOutcome::Applied(saved)) => return Ok(saved),
                        Ok(CredentialUpsertCasOutcome::Conflict { current }) => {
                            if Self::credential_update_is_applied(&base, &requested, &current)? {
                                return Ok(current);
                            }
                            candidate = Self::merge_credential_update(&base, &requested, &current)?;
                        }
                        Err(err) => {
                            tracing::warn!(
                                credential_id = id,
                                attempt,
                                "凭据 CAS 写入结果不确定，读取 PgSQL 权威值后重试: {}",
                                err
                            );
                            last_error = Some(err);
                            let current = credential_pgsql_sync_until(
                                "凭据 CAS 未知提交后读取 PgSQL 权威值",
                                deadline,
                                store.load_credentials(),
                            )
                            .await?
                            .into_iter()
                            .find(|credential| credential.id == Some(id))
                            .ok_or_else(|| anyhow::anyhow!("凭据 #{} 已删除", id))?;
                            if Self::credential_update_is_applied(&base, &requested, &current)? {
                                return Ok(current);
                            }
                            candidate = Self::merge_credential_update(&base, &requested, &current)?;
                        }
                    }
                }
                if let Some(err) = last_error {
                    anyhow::bail!("凭据 #{} CAS 在总期限内未确认: {}", id, err);
                }
                anyhow::bail!("凭据 #{} CAS 冲突重试耗尽或超过总期限", id)
            })?
        } else {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|entry| entry.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            let current = Self::credential_from_entry(entry);
            let saved = Self::merge_credential_update(base, requested, &current)?;
            entry.credentials = saved.clone();
            Self::recompute_entry_disabled(entry);
            self.invalidate_model_capability_cohorts();
            return Ok(saved);
        };

        let mut entries = self.entries.lock();
        if let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) {
            if saved.storage_revision >= entry.credentials.storage_revision {
                entry.credentials = saved.clone();
                Self::recompute_entry_disabled(entry);
            }
        }
        self.invalidate_model_capability_cohorts();
        Ok(saved)
    }

    fn persist_credential_update_with_runtime_patch(
        &self,
        base: &KiroCredentials,
        requested: &KiroCredentials,
        mut patch: CredentialRuntimeStatePatch,
    ) -> anyhow::Result<KiroCredentials> {
        let id = base
            .id
            .filter(|id| requested.id == Some(*id))
            .ok_or_else(|| anyhow::anyhow!("凭据 CAS 更新缺少一致的 id"))?;
        let current_generation = self
            .entries
            .lock()
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.runtime_generation)
            .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
        patch.expected_generation.get_or_insert(current_generation);
        let Some(store) = &self.postgres_store else {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|entry| entry.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            let current = Self::credential_from_entry(entry);
            let mut saved = Self::merge_credential_update(base, requested, &current)?;
            if let Some(value) = patch.failure_count {
                entry.failure_count = value;
            }
            if let Some(value) = patch.refresh_failure_count {
                entry.refresh_failure_count = value;
            }
            if let Some(value) = patch.warmup_remaining {
                entry.warmup_remaining = value;
            }
            match &patch.disabled_reason {
                CredentialRuntimeDisabledReasonPatch::Preserve => {}
                CredentialRuntimeDisabledReasonPatch::Set(reason) => {
                    entry.disabled_reason = DisabledReason::from_str(reason);
                }
                CredentialRuntimeDisabledReasonPatch::Clear => entry.disabled_reason = None,
            }
            if let Some(disabled) = patch.credential_disabled {
                saved.disabled = disabled;
            }
            if let Some(last_used_at) = patch.last_used_at {
                let replace = entry
                    .last_used_at
                    .as_ref()
                    .is_none_or(|existing| rfc3339_is_after(&last_used_at, existing));
                if replace {
                    entry.last_used_at = Some(last_used_at);
                }
            }
            if patch.advance_generation {
                entry.runtime_generation = entry
                    .runtime_generation
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("凭据运行态 generation 已溢出"))?;
            }
            entry.credentials = saved.clone();
            Self::recompute_entry_disabled(entry);
            self.invalidate_model_capability_cohorts();
            return Ok(saved);
        };
        if self.has_pending_runtime_mutations(id)
            || self
                .entries
                .lock()
                .iter()
                .find(|entry| entry.id == id)
                .is_some_and(|entry| entry.runtime_persistence_degraded)
        {
            anyhow::bail!("凭据 #{} 运行态持久化正在恢复，请稍后重试", id);
        }

        let store = store.clone();
        let base = base.clone();
        let requested = requested.clone();
        let operation_id = uuid::Uuid::new_v4();
        let patch_for_store = patch.clone();
        let (saved, runtime) = block_on_storage("原子更新凭据及运行态", async move {
            let deadline = tokio::time::Instant::now() + CREDENTIAL_PGSQL_SYNC_TIMEOUT;
            let mut candidate = requested.clone();
            let mut last_error = None;
            for attempt in 1..=2 {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    break;
                }
                let attempt_timeout = remaining / 2;
                let outcome = credential_pgsql_sync_with_timeout(
                    "原子更新凭据及运行态",
                    attempt_timeout,
                    store.update_credential_with_runtime_patch_cas(
                        &candidate,
                        operation_id,
                        &patch_for_store,
                    ),
                )
                .await;
                match outcome {
                    Ok(CredentialWithRuntimePatchCasOutcome::Applied {
                        credential,
                        runtime,
                    }) => return Ok((credential, runtime)),
                    Ok(CredentialWithRuntimePatchCasOutcome::Conflict { current }) => {
                        tracing::debug!(
                            credential_id = id,
                            current_revision = current.storage_revision,
                            "原子凭据 CAS 冲突，一致读取凭据及运行态后协调"
                        );
                    }
                    Err(err) => {
                        last_error = Some(err);
                    }
                }

                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    break;
                }
                let read_timeout = if attempt == 1 {
                    remaining / 2
                } else {
                    remaining
                };
                let (credentials, runtime_states) = credential_pgsql_sync_with_timeout(
                    "原子凭据 CAS 后一致读取权威值",
                    read_timeout,
                    store.load_credentials_with_runtime_state(),
                )
                .await?;
                let current = credentials
                    .into_iter()
                    .find(|credential| credential.id == Some(id))
                    .ok_or_else(|| anyhow::anyhow!("凭据 #{} 已删除", id))?;
                let runtime = runtime_states.get(&id);
                if Self::credential_runtime_patch_is_applied(
                    &base,
                    &requested,
                    &current,
                    runtime,
                    &patch_for_store,
                )? {
                    return Ok((
                        current.clone(),
                        CredentialRuntimeStateMutationResult {
                            state: runtime
                                .cloned()
                                .ok_or_else(|| anyhow::anyhow!("凭据 #{} 缺少运行态", id))?,
                            credential_disabled: current.disabled,
                            applied: true,
                        },
                    ));
                }
                if attempt == 2 {
                    break;
                }
                candidate = Self::merge_credential_update(&base, &requested, &current)?;
            }
            if let Some(err) = last_error {
                anyhow::bail!("凭据 #{} 原子 CAS 在总期限内未确认: {}", id, err);
            }
            anyhow::bail!("凭据 #{} 原子 CAS 超过总期限", id)
        })?;

        let mut entries = self.entries.lock();
        if let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) {
            if saved.storage_revision >= entry.credentials.storage_revision {
                entry.credentials = saved.clone();
                entry.credentials.disabled = runtime.credential_disabled;
                if runtime.state.revision > entry.runtime_revision {
                    Self::apply_runtime_state_if_newer(entry, &runtime.state);
                } else {
                    Self::recompute_entry_disabled(entry);
                }
            }
        }
        self.invalidate_model_capability_cohorts();
        Ok(saved)
    }

    fn persist_credential_mutation(
        &self,
        id: u64,
        mutate: impl FnOnce(&mut KiroCredentials) -> anyhow::Result<()>,
    ) -> anyhow::Result<KiroCredentials> {
        let base = {
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|entry| entry.id == id)
                .map(Self::credential_from_entry)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?
        };
        let mut requested = base.clone();
        mutate(&mut requested)?;
        requested.canonicalize_auth_method();
        requested.normalize_supported_models();
        requested.normalize_api_key_defaults();
        requested.normalize_external_idp_defaults();
        self.persist_credential_update(&base, &requested)
    }

    fn persist_credential_capacity_mutation(
        &self,
        id: u64,
        mutate: impl FnOnce(&mut KiroCredentials) -> anyhow::Result<bool>,
    ) -> anyhow::Result<KiroCredentials> {
        let base = {
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|entry| entry.id == id)
                .map(Self::credential_from_entry)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?
        };
        let mut requested = base.clone();
        let capacity_changed = mutate(&mut requested)?;
        requested.canonicalize_auth_method();
        requested.normalize_supported_models();
        requested.normalize_api_key_defaults();
        requested.normalize_external_idp_defaults();

        let warmup_remaining = self.config.lock().credential_warmup_requests;
        if capacity_changed && warmup_remaining > 0 {
            self.persist_credential_update_with_runtime_patch(
                &base,
                &requested,
                CredentialRuntimeStatePatch {
                    warmup_remaining: Some(warmup_remaining),
                    advance_generation: true,
                    ..Default::default()
                },
            )
        } else {
            self.persist_credential_update(&base, &requested)
        }
    }

    pub fn reload_credentials_from_postgres(&self) -> anyhow::Result<bool> {
        let Some(store) = &self.postgres_store else {
            return Ok(false);
        };
        let proxy_changed = self.reload_proxy_resources_from_postgres()?;
        let store = store.clone();
        let (credentials, runtime_states) =
            block_on_storage("从 PgSQL 一致性重新加载凭据和运行态", async move {
                credential_pgsql_sync_with_timeout(
                    "从 PgSQL 一致性重新加载凭据和运行态",
                    CREDENTIAL_PGSQL_SYNC_TIMEOUT,
                    store.load_credentials_with_runtime_state(),
                )
                .await
            })?;
        let by_id: HashMap<u64, KiroCredentials> = credentials
            .into_iter()
            .filter_map(|credential| credential.id.map(|id| (id, credential)))
            .collect();
        let mut entries = self.entries.lock();
        let mut changed = false;
        let non_deleted_ids: HashSet<u64> = by_id.keys().copied().collect();
        let removed_ids: Vec<u64> = entries
            .iter()
            .filter(|entry| !non_deleted_ids.contains(&entry.id))
            .map(|entry| entry.id)
            .collect();
        let before_len = entries.len();
        entries.retain(|entry| non_deleted_ids.contains(&entry.id));
        if entries.len() != before_len {
            changed = true;
        }
        self.retain_refresh_states_for_ids(&non_deleted_ids);
        let existing_ids: HashSet<u64> = entries.iter().map(|entry| entry.id).collect();
        for entry in entries.iter_mut() {
            if let Some(credential) = by_id.get(&entry.id) {
                let mut credential = credential.clone();
                credential.canonicalize_auth_method();
                credential.normalize_supported_models();
                credential.normalize_api_key_defaults();
                credential.normalize_external_idp_defaults();
                if entry.credentials.disabled != credential.disabled
                    || !entry.credentials.same_dispatch_config(&credential)
                {
                    changed = true;
                }
                if entry.runtime_persistence_degraded {
                    credential.disabled = entry.credentials.disabled;
                } else {
                    let previous_disabled = entry.disabled;
                    let previous_reason = entry.disabled_reason;
                    entry.credentials = credential.clone();
                    if let Some(state) = runtime_states.get(&entry.id) {
                        if state.revision > entry.runtime_revision {
                            changed |= Self::apply_runtime_state_if_newer(entry, state);
                        } else if state.revision == entry.runtime_revision {
                            if entry.disabled_reason == Some(DisabledReason::Manual)
                                && !credential.disabled
                            {
                                entry.disabled_reason = None;
                            } else if entry.disabled_reason.is_none() && credential.disabled {
                                entry.disabled_reason = Some(DisabledReason::Manual);
                            }
                            Self::recompute_entry_disabled(entry);
                        }
                    } else if entry.runtime_revision == 0 {
                        entry.disabled_reason =
                            credential.disabled.then_some(DisabledReason::Manual);
                        Self::recompute_entry_disabled(entry);
                    }
                    changed |= entry.disabled != previous_disabled
                        || entry.disabled_reason != previous_reason;
                }
                entry.credentials = credential;
            }
        }
        for (id, mut credential) in by_id {
            if existing_ids.contains(&id) {
                continue;
            }
            credential.canonicalize_auth_method();
            credential.normalize_supported_models();
            credential.normalize_api_key_defaults();
            credential.normalize_external_idp_defaults();
            let mut entry = CredentialEntry {
                id,
                disabled: credential.disabled,
                disabled_reason: if credential.disabled {
                    Some(DisabledReason::Manual)
                } else {
                    None
                },
                credentials: credential,
                failure_count: 0,
                refresh_failure_count: 0,
                runtime_revision: 0,
                runtime_generation: 0,
                runtime_persistence_degraded: false,
                runtime_persistence_quarantined: false,
                success_count: 0,
                total_selection_count: 0,
                last_used_at: None,
                cooldown_until: None,
                cooldown_reason: None,
                model_cooldowns: HashMap::new(),
                rate_limit_available_at: None,
                in_flight_requests: 0,
                in_flight_leases: Vec::new(),
                warmup_remaining: 0,
                health: SchedulerHealthState::default(),
                model_health: HashMap::new(),
                selection_events: VecDeque::new(),
            };
            if let Some(state) = runtime_states.get(&id) {
                Self::apply_runtime_state_if_newer(&mut entry, state);
            }
            entries.push(entry);
            changed = true;
        }
        if changed {
            entries.sort_by_key(|entry| (entry.credentials.priority, entry.id));
        }
        drop(entries);
        for id in removed_ids {
            self.clear_pending_persistence_for_credential(id);
        }
        if changed {
            self.load_stats();
        }
        if changed {
            self.select_highest_priority();
        }
        if changed || proxy_changed {
            self.invalidate_model_capability_cohorts();
        }
        Ok(changed || proxy_changed)
    }

    pub fn reload_proxy_resources_from_postgres(&self) -> anyhow::Result<bool> {
        let Some(store) = &self.postgres_store else {
            return Ok(false);
        };
        let store = store.clone();
        let resources = block_on_credential_pgsql("从 PgSQL 重新加载代理资源", async move {
            store.load_proxy_resources().await
        })?;
        let next: HashMap<u64, ProxyResourceRuntime> = resources
            .into_iter()
            .map(ProxyResourceRuntime::from)
            .map(|resource| (resource.id, resource))
            .collect();
        let mut current = self.proxy_resources.lock();
        let changed = current.len() != next.len()
            || next.iter().any(|(id, next_resource)| {
                current.get(id).is_none_or(|current_resource| {
                    current_resource.name != next_resource.name
                        || current_resource.proxy_url != next_resource.proxy_url
                        || current_resource.proxy_username != next_resource.proxy_username
                        || current_resource.proxy_password != next_resource.proxy_password
                        || current_resource.enabled != next_resource.enabled
                })
            });
        if changed {
            *current = next;
            tracing::info!("已重新加载 {} 个代理资源", current.len());
        }
        drop(current);
        if changed {
            self.invalidate_model_capability_cohorts();
        }
        Ok(changed)
    }

    fn publish_runtime_config_changed(&self, version: Option<i64>, reason: &str) {
        publish_redis_runtime_config_changed(self.redis_store.as_ref(), version, reason);
    }

    fn publish_credentials_changed(&self, reason: &str) {
        publish_redis_credentials_changed(
            self.postgres_store.as_ref(),
            self.redis_store.as_ref(),
            reason,
        );
    }

    pub fn publish_admin_credentials_changed(&self, reason: &str) {
        self.publish_credentials_changed(reason);
    }

    pub(crate) fn notify_dispatch_state_changed(&self) {
        self.capacity_signal.notify_state_changed();
    }

    pub(crate) fn notify_remote_dispatch_state_changed(&self, payload: &str) -> bool {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(payload) else {
            tracing::debug!("忽略无法解析的 Redis dispatch wakeup");
            return false;
        };
        if event
            .get("sourceInstanceId")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|source| source == self.scheduler_instance_id.as_ref())
        {
            return false;
        }

        if let Some(credential_id) = event
            .get("credentialId")
            .and_then(serde_json::Value::as_u64)
        {
            self.scheduler_redis_dirty_credential_ids
                .lock()
                .insert(credential_id);
        } else {
            self.scheduler_redis_full_sync_requested
                .fetch_add(1, Ordering::AcqRel);
            *self.last_scheduler_redis_sync_at.lock() = None;
        }
        self.refresh_scheduler_state_from_redis_best_effort();
        true
    }

    /// 从 PgSQL 加载统计数据并应用到当前条目。
    fn load_stats(&self) {
        self.load_stats_inner(true);
    }

    fn refresh_stats_from_postgres(&self) {
        self.load_stats_inner(false);
    }

    fn load_stats_inner(&self, log_info: bool) {
        let Some(store) = &self.postgres_store else {
            return;
        };
        let store = store.clone();
        let stats = match block_on_storage("从 PgSQL 加载凭据统计", async move {
            credential_pgsql_sync_with_timeout(
                "从 PgSQL 加载凭据统计",
                CREDENTIAL_PGSQL_SYNC_TIMEOUT,
                store.load_credential_stats(),
            )
            .await
        }) {
            Ok(stats) => stats,
            Err(e) => {
                tracing::warn!("{}", e);
                return;
            }
        };

        let mut entries = self.entries.lock();
        for entry in entries.iter_mut() {
            if let Some(s) = stats.get(&entry.id) {
                entry.success_count = entry.success_count.max(s.success_count);
                entry.total_selection_count = entry.total_selection_count.max(s.selection_count);
                if let Some(last_used_at) = s.last_used_at.as_ref() {
                    let replace = entry
                        .last_used_at
                        .as_ref()
                        .is_none_or(|existing| rfc3339_is_after(last_used_at, existing));
                    if replace {
                        entry.last_used_at = Some(last_used_at.clone());
                    }
                }
            }
        }
        *self.last_stats_save_at.lock() = Some(Instant::now());
        if log_info {
            tracing::info!("已从 PgSQL 加载 {} 条凭据统计", stats.len());
        } else {
            tracing::debug!("已刷新 {} 条 PgSQL 凭据统计", stats.len());
        }
    }

    /// 将当前统计数据持久化到 PgSQL
    fn save_stats(&self) {
        if self.postgres_store.is_none() {
            return;
        }
        self.flush_pending_runtime_mutations();
        let deadline = Instant::now()
            .checked_add(CREDENTIAL_PGSQL_SYNC_TIMEOUT)
            .unwrap_or_else(Instant::now);
        let _ = self.flush_one_pending_stats_batch_until(deadline);
        if self.refresh_stats_dirty_from_pending() {
            *self.last_stats_save_at.lock() = Some(Instant::now());
        }
    }

    fn flush_one_pending_stats_batch_until(&self, deadline: Instant) -> StatsBatchFlushOutcome {
        let Some(store) = &self.postgres_store else {
            return StatsBatchFlushOutcome::NoWork;
        };
        let stats_batch = {
            let mut deltas = self.pending_stats_deltas.lock();
            let mut batches = self.pending_stats_batches.lock();
            if batches.is_empty() && !deltas.is_empty() {
                batches.push_back(PendingCredentialStatsBatch {
                    operation_id: uuid::Uuid::new_v4(),
                    deltas: std::mem::take(&mut *deltas),
                });
            }
            batches.front().cloned()
        };
        let Some(batch) = stats_batch else {
            return StatsBatchFlushOutcome::NoWork;
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            self.mark_stats_dirty();
            return StatsBatchFlushOutcome::Failed;
        }

        let store = store.clone();
        let batch_to_write = batch.clone();
        if let Err(e) = block_on_storage("保存凭据统计增量到 PgSQL", async move {
            credential_pgsql_sync_with_timeout(
                "保存凭据统计增量到 PgSQL",
                remaining.min(CREDENTIAL_PGSQL_SYNC_TIMEOUT),
                store.apply_credential_stats_deltas(
                    batch_to_write.operation_id,
                    &batch_to_write.deltas,
                ),
            )
            .await
        }) {
            tracing::warn!("{}", e);
            self.mark_stats_dirty();
            return StatsBatchFlushOutcome::Failed;
        }
        let mut batches = self.pending_stats_batches.lock();
        if batches
            .front()
            .is_some_and(|pending| pending.operation_id == batch.operation_id)
        {
            batches.pop_front();
        }
        StatsBatchFlushOutcome::Persisted
    }

    fn drain_stats_until_with_runtime_limit(
        &self,
        deadline: Instant,
        runtime_attempt_limit: usize,
    ) -> bool {
        let mut failed_runtime_ids = HashSet::new();
        let mut stats_batch_failed = false;
        loop {
            let before = self.pending_persistence_backlog();
            if before.is_empty() {
                self.stats_dirty.store(false, Ordering::Release);
                return true;
            }
            if Instant::now() >= deadline {
                break;
            }

            let stats_outcome = if stats_batch_failed {
                StatsBatchFlushOutcome::NoWork
            } else {
                let outcome = self.flush_one_pending_stats_batch_until(deadline);
                stats_batch_failed = outcome == StatsBatchFlushOutcome::Failed;
                outcome
            };
            let runtime_progress = self.flush_pending_runtime_mutations_until(
                deadline,
                runtime_attempt_limit,
                &mut failed_runtime_ids,
            );
            let after = self.pending_persistence_backlog();
            if after.is_empty() {
                self.stats_dirty.store(false, Ordering::Release);
                *self.last_stats_save_at.lock() = Some(Instant::now());
                return true;
            }

            let made_progress = runtime_progress
                || stats_outcome == StatsBatchFlushOutcome::Persisted
                || after != before;
            if !made_progress {
                break;
            }
        }

        self.refresh_stats_dirty_from_pending()
    }

    fn mark_stats_dirty(&self) {
        self.stats_dirty.store(true, Ordering::Release);
    }

    fn refresh_stats_dirty_from_pending(&self) -> bool {
        let stats = self.pending_stats_deltas.lock();
        let batches = self.pending_stats_batches.lock();
        let mutations = self.pending_runtime_mutations.lock();
        let empty = stats.is_empty() && batches.is_empty() && mutations.is_empty();
        self.stats_dirty.store(!empty, Ordering::Release);
        empty
    }

    fn clear_pending_persistence_for_credential(&self, id: u64) {
        let mut stats = self.pending_stats_deltas.lock();
        let batches = self.pending_stats_batches.lock();
        let mut mutations = self.pending_runtime_mutations.lock();
        stats.remove(&id);
        mutations.remove(&id);

        if stats.is_empty() && batches.is_empty() && mutations.is_empty() {
            self.stats_dirty.store(false, Ordering::Release);
        }
    }

    fn pending_persistence_backlog(&self) -> PendingPersistenceBacklog {
        let stats = self.pending_stats_deltas.lock();
        let batches = self.pending_stats_batches.lock();
        let mutations = self.pending_runtime_mutations.lock();
        PendingPersistenceBacklog {
            stats_batches: batches.len(),
            stats_deltas: stats.len(),
            runtime_mutations: mutations.values().map(VecDeque::len).sum(),
        }
    }

    fn runtime_mutation_backlog(&self) -> (usize, u64) {
        let pending = self
            .pending_runtime_mutations
            .lock()
            .values()
            .map(VecDeque::len)
            .sum();
        (
            pending,
            self.overflow_runtime_mutations.load(Ordering::Acquire),
        )
    }

    fn has_pending_runtime_mutations(&self, id: u64) -> bool {
        self.pending_runtime_mutations
            .lock()
            .get(&id)
            .is_some_and(|queue| !queue.is_empty())
    }

    fn enqueue_pending_runtime_mutation(
        &self,
        id: u64,
        mutation: PendingCredentialRuntimeMutation,
    ) -> bool {
        let mut entries = self.entries.lock();
        let Some(entry_index) = entries.iter().position(|entry| entry.id == id) else {
            return false;
        };
        let operation_id = mutation.operation_id();
        let mutation_requires_quarantine = mutation.requires_dispatch_quarantine();
        let (over_soft_budget, requires_quarantine) = {
            let mut pending = self.pending_runtime_mutations.lock();
            if pending.values().any(|queue| {
                queue
                    .iter()
                    .any(|queued| queued.operation_id() == operation_id)
            }) {
                let requires_quarantine = pending.get(&id).is_some_and(|queue| {
                    queue.iter().any(|item| item.requires_dispatch_quarantine())
                });
                (false, requires_quarantine)
            } else {
                let total = pending.values().map(VecDeque::len).sum::<usize>();
                let queue = pending.entry(id).or_default();
                let requires_quarantine = mutation_requires_quarantine
                    || queue.iter().any(|item| item.requires_dispatch_quarantine());
                let coalesced_tail = queue
                    .back_mut()
                    .is_some_and(|previous| mutation.coalesce_into_previous(previous));
                let over_soft_budget = !coalesced_tail
                    && (total >= SOFT_PENDING_RUNTIME_MUTATIONS_TOTAL
                        || queue.len() >= SOFT_PENDING_RUNTIME_MUTATIONS_PER_CREDENTIAL);
                if !coalesced_tail {
                    queue.push_back(mutation);
                }
                (over_soft_budget, requires_quarantine)
            }
        };
        if over_soft_budget {
            let overflow = self
                .overflow_runtime_mutations
                .fetch_add(1, Ordering::AcqRel)
                .saturating_add(1);
            tracing::warn!(
                credential_id = id,
                %operation_id,
                overflow,
                "PgSQL 凭据运行态 FIFO 超过软预算；为避免丢失已在途结果仍保留 mutation，仅含调度状态转换的队列继续隔离"
            );
        }
        entries[entry_index].runtime_persistence_degraded = true;
        entries[entry_index].runtime_persistence_quarantined = requires_quarantine;
        Self::recompute_entry_disabled(&mut entries[entry_index]);
        drop(entries);
        self.mark_stats_dirty();
        true
    }

    async fn persist_pending_runtime_mutation(
        store: Arc<PostgresStore>,
        id: u64,
        mutation: PendingCredentialRuntimeMutation,
    ) -> anyhow::Result<PersistedCredentialRuntimeMutation> {
        match mutation {
            PendingCredentialRuntimeMutation::Success {
                operation_id,
                expected_generation,
                success_count,
            } => store
                .record_credential_success_at_generation_with_count(
                    id,
                    operation_id,
                    expected_generation,
                    success_count,
                )
                .await
                .map(|result| PersistedCredentialRuntimeMutation {
                    state: result.state,
                    credential_disabled: Some(result.credential_disabled),
                    applied: result.applied,
                }),
            PendingCredentialRuntimeMutation::ApiFailure {
                operation_id,
                expected_generation,
                last_used_at,
            } => store
                .record_credential_api_failure_at_generation(
                    id,
                    operation_id,
                    expected_generation,
                    &last_used_at,
                    MAX_FAILURES_PER_CREDENTIAL,
                )
                .await
                .map(|result| PersistedCredentialRuntimeMutation {
                    state: result.state,
                    credential_disabled: Some(result.credential_disabled),
                    applied: result.applied,
                }),
            #[cfg(test)]
            PendingCredentialRuntimeMutation::RefreshFailure {
                operation_id,
                expected_generation,
                last_used_at,
            } => store
                .record_credential_refresh_failure_at_generation(
                    id,
                    operation_id,
                    expected_generation,
                    &last_used_at,
                    MAX_FAILURES_PER_CREDENTIAL,
                )
                .await
                .map(|result| PersistedCredentialRuntimeMutation {
                    state: result.state,
                    credential_disabled: Some(result.credential_disabled),
                    applied: result.applied,
                }),
            PendingCredentialRuntimeMutation::Disable {
                operation_id,
                expected_generation,
                reason,
                failure_count,
                refresh_failure_count,
                last_used_at,
            } => store
                .mark_credential_disabled_at_generation(
                    id,
                    operation_id,
                    expected_generation,
                    &reason,
                    CredentialRuntimeFailureCounts {
                        failure_count,
                        refresh_failure_count,
                    },
                    &last_used_at,
                )
                .await
                .map(|result| PersistedCredentialRuntimeMutation {
                    state: result.state,
                    credential_disabled: Some(result.credential_disabled),
                    applied: result.applied,
                }),
            PendingCredentialRuntimeMutation::Patch {
                operation_id,
                patch,
            } => store
                .patch_credential_runtime_state(id, operation_id, &patch)
                .await
                .map(|result| PersistedCredentialRuntimeMutation {
                    state: result.state,
                    credential_disabled: Some(result.credential_disabled),
                    applied: result.applied,
                }),
        }
    }

    fn flush_pending_runtime_mutations(&self) {
        self.flush_pending_runtime_mutations_with_budget(RUNTIME_MUTATION_FLUSH_BUDGET);
    }

    fn flush_pending_runtime_mutations_with_budget(&self, budget: StdDuration) {
        let deadline = Instant::now()
            .checked_add(budget)
            .unwrap_or_else(Instant::now);
        let mut failed_ids = HashSet::new();
        self.flush_pending_runtime_mutations_until(
            deadline,
            RUNTIME_MUTATION_FLUSH_LIMIT,
            &mut failed_ids,
        );
    }

    fn flush_pending_runtime_mutations_until(
        &self,
        deadline: Instant,
        attempt_limit: usize,
        failed_ids: &mut HashSet<u64>,
    ) -> bool {
        let Some(store) = &self.postgres_store else {
            return false;
        };
        let mut attempted = 0;
        let mut recovered_any = false;
        let mut persisted_any = false;
        while attempted < attempt_limit {
            let mut ids: Vec<u64> = self
                .pending_runtime_mutations
                .lock()
                .keys()
                .filter(|&&id| !failed_ids.contains(&id))
                .copied()
                .collect();
            if ids.is_empty() {
                break;
            }
            ids.sort_unstable();
            let cursor = self.runtime_mutation_flush_cursor.load(Ordering::Acquire);
            if let Some(split) = ids.iter().position(|id| *id > cursor) {
                ids.rotate_left(split);
            }

            let mut round_succeeded = false;
            for id in ids {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if attempted >= attempt_limit || remaining.is_zero() {
                    break;
                }
                self.runtime_mutation_flush_cursor
                    .store(id, Ordering::Release);
                let mutation = {
                    let pending = self.pending_runtime_mutations.lock();
                    pending.get(&id).and_then(|queue| queue.front()).cloned()
                };
                let Some(mutation) = mutation else {
                    continue;
                };
                attempted += 1;
                let operation_id = mutation.operation_id();
                let store = store.clone();
                let result = block_on_storage("重放 PgSQL 凭据运行态 mutation", async move {
                    credential_pgsql_sync_with_timeout(
                        "重放 PgSQL 凭据运行态 mutation",
                        remaining.min(CREDENTIAL_PGSQL_SYNC_TIMEOUT),
                        Self::persist_pending_runtime_mutation(store, id, mutation),
                    )
                    .await
                });
                let persisted = match result {
                    Ok(persisted) => persisted,
                    Err(err) => {
                        tracing::warn!(
                            credential_id = id,
                            %operation_id,
                            "PgSQL 凭据运行态 mutation 重放失败，保留 FIFO 等待下次重试: {}",
                            err
                        );
                        failed_ids.insert(id);
                        self.mark_stats_dirty();
                        continue;
                    }
                };
                if !persisted.applied {
                    tracing::info!(
                        credential_id = id,
                        %operation_id,
                        authoritative_generation = persisted.state.generation,
                        "已丢弃 reset 前创建的过期 PgSQL 凭据运行态 mutation"
                    );
                }
                round_succeeded = true;
                persisted_any = true;
                let (queue_empty, requires_quarantine) = {
                    let mut pending = self.pending_runtime_mutations.lock();
                    let mut queue_empty = false;
                    let mut requires_quarantine = false;
                    if let Some(queue) = pending.get_mut(&id) {
                        if queue
                            .front()
                            .is_some_and(|queued| queued.operation_id() == operation_id)
                        {
                            queue.pop_front();
                        }
                        queue_empty = queue.is_empty();
                        requires_quarantine =
                            queue.iter().any(|item| item.requires_dispatch_quarantine());
                    }
                    if queue_empty {
                        pending.remove(&id);
                    }
                    (queue_empty, requires_quarantine)
                };
                {
                    let mut entries = self.entries.lock();
                    if let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) {
                        entry.runtime_persistence_degraded = !queue_empty;
                        entry.runtime_persistence_quarantined = requires_quarantine;
                        Self::apply_persisted_runtime_mutation_to_entry(entry, &persisted);
                        Self::recompute_entry_disabled(entry);
                    }
                }
                recovered_any |= queue_empty;
            }
            if !round_succeeded {
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
        }
        if recovered_any {
            self.select_highest_priority();
            self.invalidate_model_capability_cohorts();
            self.notify_dispatch_state_changed();
            self.publish_credentials_changed("credential_runtime_persistence_recovered");
        }
        if !self.pending_runtime_mutations.lock().is_empty() {
            self.mark_stats_dirty();
        }
        persisted_any
    }

    fn cleanup_runtime_mutation_history_throttled(&self) {
        let Some(store) = &self.postgres_store else {
            return;
        };
        let now = Instant::now();
        {
            let mut last_cleanup_at = self.last_runtime_mutation_cleanup_at.lock();
            if last_cleanup_at.is_some_and(|last| {
                now.saturating_duration_since(last) < CREDENTIAL_RUNTIME_MUTATION_CLEANUP_INTERVAL
            }) {
                return;
            }
            *last_cleanup_at = Some(now);
        }

        let store = store.clone();
        match block_on_storage("清理 PgSQL 凭据运行态 mutation 幂等记录", async move {
            cleanup_runtime_mutation_history_batches(
                store,
                CREDENTIAL_RUNTIME_MUTATION_RETENTION,
                CREDENTIAL_RUNTIME_MUTATION_CLEANUP_LIMIT,
                CREDENTIAL_RUNTIME_MUTATION_CLEANUP_MAX_BATCHES,
                CREDENTIAL_RUNTIME_MUTATION_CLEANUP_BUDGET,
            )
            .await
        }) {
            Ok(report) => {
                if report.removed > 0 {
                    tracing::info!(
                        removed = report.removed,
                        batches = report.batches,
                        "已清理过期的 PgSQL 凭据运行态 mutation 幂等记录"
                    );
                }
                if report.saturated {
                    tracing::warn!(
                        removed = report.removed,
                        batches = report.batches,
                        "PgSQL 凭据运行态 mutation 清理达到单次预算或批次上限"
                    );
                }
            }
            Err(err) => {
                tracing::warn!("清理 PgSQL 凭据运行态 mutation 幂等记录失败: {}", err);
            }
        }
    }

    pub fn spawn_stats_flush_worker(self: &Arc<Self>) -> StatsFlushWorkerHandle {
        self.spawn_stats_flush_worker_inner(
            tokio::time::Instant::now(),
            RUNTIME_MUTATION_FLUSH_LIMIT,
        )
    }

    fn spawn_stats_flush_worker_inner(
        self: &Arc<Self>,
        first_tick: tokio::time::Instant,
        final_runtime_attempt_limit: usize,
    ) -> StatsFlushWorkerHandle {
        let (shutdown, mut shutdown_requested) = oneshot::channel();
        let manager = Arc::clone(self);
        let task = tokio::spawn(async move {
            let mut interval =
                tokio::time::interval_at(first_tick, CREDENTIAL_STATS_FLUSH_MIN_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let shutdown_deadline = loop {
                tokio::select! {
                    request = &mut shutdown_requested => {
                        break request.unwrap_or_else(|_| {
                            Instant::now()
                                .checked_add(
                                    RUNTIME_MUTATION_FLUSH_BUDGET
                                        .saturating_add(CREDENTIAL_PGSQL_SYNC_TIMEOUT),
                                )
                                .unwrap_or_else(Instant::now)
                        });
                    }
                    _ = interval.tick() => {
                        let _ = manager.cleanup_expired_in_flight_leases_throttled();
                        manager.save_stats();
                        manager.cleanup_runtime_mutation_history_throttled();
                    }
                }
            };
            manager.drain_stats_until_with_runtime_limit(
                shutdown_deadline,
                final_runtime_attempt_limit,
            )
        });
        StatsFlushWorkerHandle {
            shutdown: Some(shutdown),
            task,
            manager: Arc::clone(self),
        }
    }

    /// 从 PgSQL 加载凭据运行态（失败计数、禁用原因、预热次数）。
    fn load_runtime_state(&self) -> bool {
        let Some(store) = &self.postgres_store else {
            return false;
        };
        let store = store.clone();
        let states = match block_on_storage("从 PgSQL 加载凭据运行态", async move {
            credential_pgsql_sync_with_timeout(
                "从 PgSQL 加载凭据运行态",
                CREDENTIAL_PGSQL_SYNC_TIMEOUT,
                store.load_credential_runtime_state(),
            )
            .await
        }) {
            Ok(states) => states,
            Err(e) => {
                tracing::warn!("{}", e);
                return false;
            }
        };

        let changed = self.apply_loaded_runtime_states(&states);
        tracing::info!("已从 PgSQL 加载 {} 条凭据运行态", states.len());
        changed
    }

    fn apply_loaded_runtime_states(
        &self,
        states: &HashMap<u64, CredentialRuntimeStateRow>,
    ) -> bool {
        let mut entries = self.entries.lock();
        let mut changed = false;
        for entry in entries.iter_mut() {
            if entry.runtime_persistence_degraded {
                continue;
            }
            if let Some(state) = states.get(&entry.id) {
                changed |= Self::apply_runtime_state_if_newer(entry, state);
            }
        }
        changed
    }

    fn persist_success_state(&self, id: u64, expected_generation: u64, last_used_at: &str) -> bool {
        let operation_id = uuid::Uuid::new_v4();
        let queue_without_attempt = self.postgres_store.is_some()
            && (self.has_pending_runtime_mutations(id)
                || self
                    .entries
                    .lock()
                    .iter()
                    .find(|entry| entry.id == id)
                    .is_some_and(|entry| entry.runtime_persistence_degraded));
        if queue_without_attempt {
            self.enqueue_pending_runtime_mutation(
                id,
                PendingCredentialRuntimeMutation::Success {
                    operation_id,
                    expected_generation,
                    success_count: 1,
                },
            );
        }
        if !queue_without_attempt {
            if let Some(store) = &self.postgres_store {
                let store = store.clone();
                match block_on_credential_pgsql("原子记录 PgSQL 凭据调用成功", async move {
                    store
                        .record_credential_success_if_runtime_dirty_at_generation(
                            id,
                            operation_id,
                            expected_generation,
                        )
                        .await
                }) {
                    Ok(result) => {
                        let disabled = {
                            let mut entries = self.entries.lock();
                            let Some(entry) = entries.iter_mut().find(|entry| entry.id == id)
                            else {
                                return false;
                            };
                            Self::apply_runtime_mutation_result_to_entry(entry, &result);
                            entry.disabled
                        };
                        self.record_success_stats_delta(id, last_used_at);
                        return disabled;
                    }
                    Err(err) => {
                        tracing::warn!(
                            credential_id = id,
                            %operation_id,
                            "{}；保留相同 operation ID 等待 PgSQL 恢复后重放",
                            err
                        );
                        self.enqueue_pending_runtime_mutation(
                            id,
                            PendingCredentialRuntimeMutation::Success {
                                operation_id,
                                expected_generation,
                                success_count: 1,
                            },
                        );
                    }
                }
            }
        }

        self.record_success_stats_delta(id, last_used_at);
        self.entries
            .lock()
            .iter()
            .find(|entry| entry.id == id)
            .is_some_and(|entry| entry.disabled)
    }

    fn should_probe_persisted_runtime_success_state(&self, id: u64) -> bool {
        if self.postgres_store.is_none() {
            return false;
        }
        let now = Instant::now();
        let mut last_probe_at = self.last_runtime_success_reconcile_probe_at.lock();
        if last_probe_at.get(&id).is_some_and(|last| {
            now.saturating_duration_since(*last) < PERSISTED_RUNTIME_SUCCESS_RECONCILE_MIN_INTERVAL
        }) {
            return false;
        }
        last_probe_at.insert(id, now);
        #[cfg(test)]
        self.runtime_success_reconcile_probe_attempts
            .fetch_add(1, Ordering::AcqRel);
        true
    }

    #[cfg(test)]
    fn runtime_success_reconcile_probe_attempts(&self) -> u64 {
        self.runtime_success_reconcile_probe_attempts
            .load(Ordering::Acquire)
    }

    fn record_success_stats_delta(&self, id: u64, last_used_at: &str) {
        let mut pending = self.pending_stats_deltas.lock();
        let entry = pending.entry(id).or_default();
        entry.success_delta = entry.success_delta.saturating_add(1);
        entry.last_used_at = Some(last_used_at.to_string());
        drop(pending);
        self.mark_stats_dirty();
    }

    fn persist_disabled_state(
        &self,
        id: u64,
        expected_generation: u64,
        reason: DisabledReason,
        failure_count: Option<u32>,
        refresh_failure_count: Option<u32>,
        last_used_at: &str,
    ) {
        let operation_id = uuid::Uuid::new_v4();
        let mutation = PendingCredentialRuntimeMutation::Disable {
            operation_id,
            expected_generation,
            reason: reason.as_str().to_string(),
            failure_count,
            refresh_failure_count,
            last_used_at: last_used_at.to_string(),
        };
        let Some(store) = &self.postgres_store else {
            return;
        };
        if self.has_pending_runtime_mutations(id)
            || self
                .entries
                .lock()
                .iter()
                .find(|entry| entry.id == id)
                .is_some_and(|entry| entry.runtime_persistence_degraded)
        {
            self.enqueue_pending_runtime_mutation(id, mutation);
            return;
        }

        let store = store.clone();
        let mutation_to_write = mutation.clone();
        let result = block_on_storage("原子禁用 PgSQL 凭据运行态", async move {
            credential_pgsql_sync_with_timeout(
                "原子禁用 PgSQL 凭据运行态",
                CREDENTIAL_PGSQL_SYNC_TIMEOUT,
                Self::persist_pending_runtime_mutation(store, id, mutation_to_write),
            )
            .await
        });
        match result {
            Ok(persisted) => {
                let mut entries = self.entries.lock();
                if let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) {
                    Self::apply_persisted_runtime_mutation_to_entry(entry, &persisted);
                }
            }
            Err(err) => {
                tracing::warn!(
                    credential_id = id,
                    %operation_id,
                    "{}；保留相同 operation ID 等待 PgSQL 恢复后重放",
                    err
                );
                self.enqueue_pending_runtime_mutation(id, mutation);
            }
        }
    }

    fn apply_runtime_mutation_result(
        &self,
        id: u64,
        result: &CredentialRuntimeStateMutationResult,
    ) {
        let mut entries = self.entries.lock();
        let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) else {
            return;
        };
        Self::apply_runtime_mutation_result_to_entry(entry, result);
    }

    fn apply_runtime_mutation_result_to_entry(
        entry: &mut CredentialEntry,
        result: &CredentialRuntimeStateMutationResult,
    ) {
        Self::apply_authoritative_runtime_state_to_entry(
            entry,
            &result.state,
            Some(result.credential_disabled),
        );
    }

    fn apply_persisted_runtime_mutation_to_entry(
        entry: &mut CredentialEntry,
        persisted: &PersistedCredentialRuntimeMutation,
    ) {
        Self::apply_authoritative_runtime_state_to_entry(
            entry,
            &persisted.state,
            persisted.credential_disabled,
        );
    }

    fn apply_authoritative_runtime_state_to_entry(
        entry: &mut CredentialEntry,
        state: &CredentialRuntimeStateRow,
        credential_disabled: Option<bool>,
    ) {
        if state.revision < entry.runtime_revision {
            return;
        }
        if let Some(disabled) = credential_disabled {
            entry.credentials.disabled = disabled;
        }
        if state.revision > entry.runtime_revision {
            Self::apply_runtime_state_if_newer(entry, state);
        } else {
            Self::recompute_entry_disabled(entry);
        }
    }

    fn persist_runtime_patch_value(
        &self,
        id: u64,
        mut patch: CredentialRuntimeStatePatch,
    ) -> anyhow::Result<bool> {
        for (field, value) in [
            ("failure_count", patch.failure_count),
            ("refresh_failure_count", patch.refresh_failure_count),
            ("warmup_remaining", patch.warmup_remaining),
        ] {
            if value.is_some_and(|value| i32::try_from(value).is_err()) {
                anyhow::bail!("凭据运行态字段 {} 超出 PgSQL INTEGER 范围", field);
            }
        }
        let Some(store) = &self.postgres_store else {
            return Ok(false);
        };
        if self.has_pending_runtime_mutations(id)
            || self
                .entries
                .lock()
                .iter()
                .find(|entry| entry.id == id)
                .is_some_and(|entry| entry.runtime_persistence_degraded)
        {
            anyhow::bail!("凭据 #{} 运行态持久化正在恢复，请稍后重试", id);
        }
        let expected_generation = self
            .entries
            .lock()
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.runtime_generation)
            .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
        patch.expected_generation.get_or_insert(expected_generation);

        let operation_id = uuid::Uuid::new_v4();
        let store = store.clone();
        let result = block_on_storage("原子更新 PgSQL 凭据运行态字段", async move {
            credential_pgsql_sync_with_timeout(
                "原子更新 PgSQL 凭据运行态字段",
                CREDENTIAL_PGSQL_SYNC_TIMEOUT,
                store.patch_credential_runtime_state(id, operation_id, &patch),
            )
            .await
        });
        match result {
            Ok(result) => {
                self.apply_runtime_mutation_result(id, &result);
                if result.applied {
                    Ok(true)
                } else {
                    anyhow::bail!(
                        "凭据 #{} 运行态 generation 已由其他实例推进，请重试 Admin 更新",
                        id
                    )
                }
            }
            Err(err) => {
                if let Err(reload_err) = self.reload_credentials_from_postgres() {
                    tracing::warn!(
                        credential_id = id,
                        %operation_id,
                        "Admin 运行态 patch 失败后重新加载 PgSQL 权威状态也失败: {}",
                        reload_err
                    );
                }
                Err(anyhow::anyhow!(
                    "{}；operation {} 未进入后台重放队列",
                    err,
                    operation_id
                ))
            }
        }
    }

    async fn persist_runtime_patch_best_effort_until(
        &self,
        id: u64,
        mut patch: CredentialRuntimeStatePatch,
        deadline: tokio::time::Instant,
    ) {
        let Some(store) = &self.postgres_store else {
            return;
        };
        if [
            patch.failure_count,
            patch.refresh_failure_count,
            patch.warmup_remaining,
        ]
        .into_iter()
        .flatten()
        .any(|value| i32::try_from(value).is_err())
        {
            tracing::error!(credential_id = id, "拒绝不可持久化的凭据运行态 patch");
            return;
        }
        let Some(expected_generation) = self
            .entries
            .lock()
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.runtime_generation)
        else {
            return;
        };
        patch.expected_generation.get_or_insert(expected_generation);
        let operation_id = uuid::Uuid::new_v4();
        let mutation = PendingCredentialRuntimeMutation::Patch {
            operation_id,
            patch: patch.clone(),
        };
        if self.has_pending_runtime_mutations(id)
            || self
                .entries
                .lock()
                .iter()
                .find(|entry| entry.id == id)
                .is_some_and(|entry| entry.runtime_persistence_degraded)
        {
            self.enqueue_pending_runtime_mutation(id, mutation);
            return;
        }
        let store = store.clone();
        let result = credential_pgsql_sync_until(
            "原子更新 PgSQL 凭据运行态字段",
            deadline,
            store.patch_credential_runtime_state(id, operation_id, &patch),
        )
        .await;
        match result {
            Ok(result) => self.apply_runtime_mutation_result(id, &result),
            Err(err) => {
                tracing::warn!(
                    credential_id = id,
                    %operation_id,
                    "{}；保留字段级 mutation 等待 PgSQL 恢复后重放",
                    err
                );
                if self.entries.lock().iter().any(|entry| entry.id == id) {
                    self.enqueue_pending_runtime_mutation(id, mutation);
                }
            }
        }
    }

    fn delete_persisted_credential_state(&self, id: u64) -> anyhow::Result<bool> {
        let Some(store) = &self.postgres_store else {
            return Ok(false);
        };
        let store = store.clone();
        block_on_credential_pgsql("删除凭据持久化状态", async move {
            store.soft_delete_credential(id).await?;
            store.delete_credential_stats_and_runtime(id).await
        })?;
        Ok(true)
    }

    fn refresh_scheduler_state_from_redis(&self) -> anyhow::Result<()> {
        let Some(redis) = &self.redis_store else {
            return Ok(());
        };

        let now = Instant::now();
        let targeted_dirty = !self.scheduler_redis_dirty_credential_ids.lock().is_empty();
        let full_sync_dirty = self
            .scheduler_redis_full_sync_requested
            .load(Ordering::Acquire)
            != self
                .scheduler_redis_full_sync_applied
                .load(Ordering::Acquire);
        if !targeted_dirty && !full_sync_dirty {
            let mut last_sync_at = self.last_scheduler_redis_sync_at.lock();
            if last_sync_at.is_some_and(|last| {
                now.saturating_duration_since(last) < SCHEDULER_REDIS_SYNC_MIN_INTERVAL
            }) {
                return Ok(());
            }
            *last_sync_at = Some(now);
            self.scheduler_redis_full_sync_requested
                .fetch_add(1, Ordering::AcqRel);
        }

        if self
            .scheduler_redis_sync_in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }

        if tokio::runtime::Handle::try_current().is_err() {
            self.scheduler_redis_sync_in_flight
                .store(false, Ordering::Release);
            return self.refresh_scheduler_state_from_redis_force();
        }

        let redis = redis.clone();
        let entries = self.entries.clone();
        let (config_rpm, in_flight_max_age) = {
            let config = self.config.lock();
            (
                config.credential_rpm.unwrap_or(0),
                (config.credential_in_flight_lease_max_secs > 0)
                    .then(|| StdDuration::from_secs(config.credential_in_flight_lease_max_secs)),
            )
        };
        let sync_in_flight = self.scheduler_redis_sync_in_flight.clone();
        let full_sync_requested = self.scheduler_redis_full_sync_requested.clone();
        let full_sync_applied = self.scheduler_redis_full_sync_applied.clone();
        let dirty_credential_ids = self.scheduler_redis_dirty_credential_ids.clone();
        let last_sync_at = self.last_scheduler_redis_sync_at.clone();
        let capacity_signal = self.capacity_signal.clone();
        let released_lease_tombstones = self.released_in_flight_lease_tombstones.clone();
        let breaker = self.scheduler_redis_snapshot_breaker.clone();

        tokio::spawn(async move {
            loop {
                tokio::time::sleep(SCHEDULER_REDIS_REMOTE_SYNC_DEBOUNCE).await;
                let target_full_generation = full_sync_requested.load(Ordering::Acquire);
                let full_sync = target_full_generation != full_sync_applied.load(Ordering::Acquire);
                let ids = if full_sync {
                    dirty_credential_ids.lock().clear();
                    entries
                        .lock()
                        .iter()
                        .map(|entry| entry.id)
                        .collect::<Vec<_>>()
                } else {
                    dirty_credential_ids.lock().drain().collect::<Vec<_>>()
                };

                if ids.is_empty() {
                    if full_sync {
                        full_sync_applied.store(target_full_generation, Ordering::Release);
                        *last_sync_at.lock() = Some(Instant::now());
                    }
                } else {
                    let result = Self::execute_scheduler_redis_operation(
                        breaker.clone(),
                        SCHEDULER_REDIS_SNAPSHOT_OP_TIMEOUT,
                        if full_sync {
                            "从 Redis 后台全量同步调度运行态"
                        } else {
                            "从 Redis 后台定点同步调度运行态"
                        },
                        || {},
                        redis.scheduler_state_for_credentials(&ids),
                    )
                    .await;
                    match result {
                        SchedulerRedisExecutionOutcome::Completed(mut states) => {
                            filter_released_in_flight_leases_from_scheduler_states(
                                &released_lease_tombstones,
                                &mut states,
                            );
                            if full_sync {
                                apply_redis_scheduler_states_with_global_rpm(
                                    &entries,
                                    config_rpm,
                                    in_flight_max_age,
                                    states,
                                );
                                full_sync_applied.store(target_full_generation, Ordering::Release);
                                *last_sync_at.lock() = Some(Instant::now());
                            } else {
                                apply_redis_scheduler_states_for_ids_with_global_rpm(
                                    &entries,
                                    config_rpm,
                                    in_flight_max_age,
                                    states,
                                );
                            }
                            capacity_signal.notify_state_changed();
                        }
                        SchedulerRedisExecutionOutcome::NotStarted(reason) => {
                            if full_sync {
                                *last_sync_at.lock() = None;
                            } else {
                                dirty_credential_ids.lock().extend(ids.iter().copied());
                            }
                            tracing::debug!(
                                operation = "从 Redis 后台同步调度运行态",
                                ?reason,
                                "Redis 调度状态后台同步未准入，沿用本地调度缓存"
                            );
                            let delay = breaker.retry_after().unwrap_or_else(|| {
                                SCHEDULER_REDIS_SYNC_MIN_INTERVAL
                                    .saturating_mul(2)
                                    .min(SCHEDULER_REDIS_DEGRADED_BACKOFF_MAX)
                            });
                            tokio::time::sleep(delay).await;
                        }
                        SchedulerRedisExecutionOutcome::Failed { .. } => {
                            if full_sync {
                                *last_sync_at.lock() = None;
                            } else {
                                dirty_credential_ids.lock().extend(ids.iter().copied());
                            }
                            tracing::debug!(
                                operation = "从 Redis 后台同步调度运行态",
                                timeout_ms = SCHEDULER_REDIS_SNAPSHOT_OP_TIMEOUT.as_millis() as u64,
                                "Redis 调度状态后台同步失败，沿用本地调度缓存"
                            );
                            let delay = breaker.retry_after().unwrap_or_else(|| {
                                SCHEDULER_REDIS_SYNC_MIN_INTERVAL
                                    .saturating_mul(2)
                                    .min(SCHEDULER_REDIS_DEGRADED_BACKOFF_MAX)
                            });
                            tokio::time::sleep(delay).await;
                        }
                    }
                }

                let still_dirty = full_sync_requested.load(Ordering::Acquire)
                    != full_sync_applied.load(Ordering::Acquire)
                    || !dirty_credential_ids.lock().is_empty();
                if still_dirty {
                    continue;
                }

                sync_in_flight.store(false, Ordering::Release);
                let raced_dirty = full_sync_requested.load(Ordering::Acquire)
                    != full_sync_applied.load(Ordering::Acquire)
                    || !dirty_credential_ids.lock().is_empty();
                if raced_dirty
                    && sync_in_flight
                        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                {
                    continue;
                }
                break;
            }
        });
        Ok(())
    }

    fn refresh_scheduler_state_from_redis_force(&self) -> anyhow::Result<()> {
        let Some(redis) = &self.redis_store else {
            return Ok(());
        };
        let ids: Vec<u64> = {
            let entries = self.entries.lock();
            entries.iter().map(|entry| entry.id).collect()
        };
        if ids.is_empty() {
            return Ok(());
        }

        let redis = redis.clone();
        if let Some(states) = self
            .block_on_scheduler_redis_state_sync("从 Redis 同步调度运行态", async move {
                redis.scheduler_state_for_credentials(&ids).await
            })
        {
            self.apply_scheduler_states(states);
            *self.last_scheduler_redis_sync_at.lock() = Some(Instant::now());
            let requested = self
                .scheduler_redis_full_sync_requested
                .load(Ordering::Acquire);
            self.scheduler_redis_full_sync_applied
                .store(requested, Ordering::Release);
            self.scheduler_redis_dirty_credential_ids.lock().clear();
            self.capacity_signal.notify_state_changed();
        }
        Ok(())
    }

    fn refresh_scheduler_state_from_redis_for_ids(&self, ids: &HashSet<u64>) -> anyhow::Result<()> {
        let Some(redis) = &self.redis_store else {
            return Ok(());
        };
        if ids.is_empty() {
            return Ok(());
        }
        let ids: Vec<u64> = {
            let entries = self.entries.lock();
            entries
                .iter()
                .filter(|entry| ids.contains(&entry.id))
                .map(|entry| entry.id)
                .collect()
        };
        if ids.is_empty() {
            return Ok(());
        }
        let redis = redis.clone();
        if let Some(states) = self
            .block_on_scheduler_redis_state_sync("从 Redis 同步指定凭据调度运行态", async move {
                redis.scheduler_state_for_credentials(&ids).await
            })
        {
            self.apply_scheduler_states_for_ids(states);
            Ok(())
        } else {
            anyhow::bail!("Redis 调度运行态未在热路径超时内返回")
        }
    }

    fn refresh_scheduler_state_from_redis_best_effort(&self) {
        if let Err(err) = self.refresh_scheduler_state_from_redis() {
            tracing::warn!("从 Redis 同步调度运行态失败: {}", err);
        }
    }

    fn refresh_scheduler_state_from_redis_force_best_effort(&self) {
        if let Err(err) = self.refresh_scheduler_state_from_redis_force() {
            tracing::warn!("从 Redis 同步调度运行态失败: {}", err);
        }
    }

    fn apply_scheduler_states(&self, mut states: HashMap<u64, SchedulerCredentialState>) {
        filter_released_in_flight_leases_from_scheduler_states(
            &self.released_in_flight_lease_tombstones,
            &mut states,
        );
        apply_redis_scheduler_states(&self.entries, &self.config, states);
    }

    fn apply_scheduler_states_for_ids(&self, mut states: HashMap<u64, SchedulerCredentialState>) {
        filter_released_in_flight_leases_from_scheduler_states(
            &self.released_in_flight_lease_tombstones,
            &mut states,
        );
        apply_redis_scheduler_states_for_ids(&self.entries, &self.config, states);
    }

    fn record_scheduler_selection(
        &self,
        id: u64,
        request_weight_units: u32,
        record_in_redis: bool,
    ) {
        let request_weight_units = normalize_capacity_weight_units(request_weight_units);
        let now = Instant::now();
        {
            let mut entries = self.entries.lock();
            if let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) {
                record_local_selection(entry, now, request_weight_units);
            }
        }
        if record_in_redis {
            if let Some(redis) = &self.redis_store {
                let redis = redis.clone();
                let global_rpm = self.config.lock().credential_rpm.unwrap_or(0);
                let rpm = {
                    let entries = self.entries.lock();
                    entries
                        .iter()
                        .find(|entry| entry.id == id)
                        .map(|entry| effective_rpm(entry, global_rpm))
                        .unwrap_or(global_rpm)
                };
                spawn_best_effort_storage_task("记录 Redis 调度选中次数", async move {
                    redis
                        .record_scheduler_selection(id, rpm, request_weight_units)
                        .await?;
                    Ok(())
                });
            }
        }
        let mut pending = self.pending_stats_deltas.lock();
        let entry = pending.entry(id).or_default();
        entry.selection_delta = entry.selection_delta.saturating_add(1);
        self.mark_stats_dirty();
    }

    fn clear_scheduler_state_for_credential(&self, id: u64, clear_in_flight: bool) {
        clear_scheduler_state_for_credential_local(&self.entries, id, clear_in_flight);
        clear_scheduler_state_for_credential_redis(self.redis_store.as_ref(), id, clear_in_flight);
    }

    /// 报告指定凭据 API 调用成功
    ///
    /// 重置该凭据的失败计数
    ///
    /// # Arguments
    /// * `id` - 凭据 ID（来自 CallContext）
    #[allow(dead_code)]
    pub fn report_success(&self, id: u64) {
        self.report_success_with_latency(id, None, None);
    }

    pub fn report_success_with_latency(
        &self,
        id: u64,
        model: Option<&str>,
        latency: Option<StdDuration>,
    ) {
        let Some(success) = self.record_success_local(id, model, latency) else {
            return;
        };
        let should_persist = success.runtime_state_changed
            || self.has_pending_runtime_mutations(id)
            || self.should_probe_persisted_runtime_success_state(id);
        let disabled = if should_persist {
            self.persist_success_state(id, success.expected_generation, &success.last_used_at)
        } else {
            self.record_success_stats_delta(id, &success.last_used_at);
            false
        };
        if disabled {
            self.select_highest_priority();
            self.unbind_sessions_for_credential(id);
            self.clear_scheduler_state_for_credential(id, false);
        }
        self.record_scheduler_success_health(id, model, latency, success.alpha);
        self.notify_dispatch_state_changed();
    }

    /// Request/stream completion path variant: update local scheduling state immediately and
    /// enqueue durable PgSQL/Redis side effects. This keeps stream terminal and response body
    /// cleanup independent from PgSQL/Redis latency spikes.
    pub fn report_success_with_latency_deferred(
        &self,
        id: u64,
        model: Option<&str>,
        latency: Option<StdDuration>,
    ) {
        let Some(success) = self.record_success_local(id, model, latency) else {
            return;
        };
        if success.runtime_state_changed
            || self.has_pending_runtime_mutations(id)
            || self.should_probe_persisted_runtime_success_state(id)
        {
            let operation_id = uuid::Uuid::new_v4();
            self.enqueue_pending_runtime_mutation(
                id,
                PendingCredentialRuntimeMutation::Success {
                    operation_id,
                    expected_generation: success.expected_generation,
                    success_count: 1,
                },
            );
        }
        self.record_success_stats_delta(id, &success.last_used_at);
        self.record_scheduler_success_health(id, model, latency, success.alpha);
        self.notify_dispatch_state_changed();
    }

    fn record_success_local(
        &self,
        id: u64,
        model: Option<&str>,
        latency: Option<StdDuration>,
    ) -> Option<SuccessReportOutcome> {
        let alpha = self
            .config
            .lock()
            .scheduler_error_ewma_alpha
            .clamp(0.01, 1.0);
        let mut entries = self.entries.lock();
        let entry = entries.iter_mut().find(|e| e.id == id)?;
        let runtime_state_changed = entry.failure_count > 0
            || entry.refresh_failure_count > 0
            || entry.warmup_remaining > 0
            || entry.runtime_persistence_degraded
            || entry.disabled;
        entry.failure_count = 0;
        entry.refresh_failure_count = 0;
        let expected_generation = entry.runtime_generation;
        if entry.warmup_remaining > 0 {
            entry.warmup_remaining -= 1;
        }
        entry.success_count += 1;
        let now = Utc::now().to_rfc3339();
        entry.last_used_at = Some(now.clone());
        {
            let health = entry_effective_health_mut(entry, model);
            health.recent_error_rate *= 1.0 - alpha;
            health.transient_failure_streak = health.transient_failure_streak.saturating_sub(1);
            if let Some(latency) = latency {
                let latency_ms = latency.as_millis() as f64;
                health.latency_ewma_ms = Some(
                    health
                        .latency_ewma_ms
                        .map(|previous| previous + alpha * (latency_ms - previous))
                        .unwrap_or(latency_ms),
                );
            }
        }
        tracing::debug!(
            "凭据 #{} API 调用成功（累计 {} 次）",
            id,
            entry.success_count
        );
        Some(SuccessReportOutcome {
            expected_generation,
            last_used_at: now,
            runtime_state_changed,
            alpha,
        })
    }

    fn record_scheduler_success_health(
        &self,
        id: u64,
        model: Option<&str>,
        latency: Option<StdDuration>,
        alpha: f64,
    ) {
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            let model_for_redis = model.map(str::to_string);
            spawn_best_effort_storage_task("记录 Redis 调度成功健康状态", async move {
                redis
                    .record_scheduler_success(id, model_for_redis.as_deref(), latency, alpha)
                    .await?;
                Ok(())
            });
        }
    }

    /// 报告指定凭据遇到上游瞬态错误，按 Retry-After 或默认值临时冷却。
    ///
    /// 不增加 failure_count，不禁用凭据；冷却到期后自动重新参与调度。
    #[allow(dead_code)]
    pub fn report_transient_failure(
        &self,
        id: u64,
        model: Option<&str>,
        retry_after: Option<StdDuration>,
        reason: impl Into<String>,
    ) -> anyhow::Result<bool> {
        self.report_transient_failure_kind(
            id,
            model,
            TransientFailureKind::Protocol,
            retry_after,
            reason,
        )
    }

    pub fn report_transient_failure_kind(
        &self,
        id: u64,
        model: Option<&str>,
        kind: TransientFailureKind,
        retry_after: Option<StdDuration>,
        reason: impl Into<String>,
    ) -> anyhow::Result<bool> {
        let reason = reason.into();
        let (base, max, multiplier, jitter, probation, alpha) =
            self.transient_failure_settings(kind);
        let coalesce_window = Self::transient_failure_coalesce_window(base);
        let now = Instant::now();
        let now_ms = Utc::now().timestamp_millis();

        {
            let mut entries = self.entries.lock();
            let Some(entry) = entries.iter_mut().find(|e| e.id == id) else {
                return Ok(entries.iter().any(|e| !e.disabled));
            };

            if entry.disabled {
                return Ok(entries.iter().any(|e| !e.disabled));
            }
        }

        let (duration, coalesced) = {
            let mut entries = self.entries.lock();
            let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) else {
                return Ok(entries.iter().any(|entry| !entry.disabled));
            };
            let model = model.map(str::trim).filter(|value| !value.is_empty());
            let model_key = model.map(model_state_key);
            let existing_until = if let Some(key) = &model_key {
                entry
                    .model_cooldowns
                    .get(key)
                    .map(|cooldown| cooldown.until)
            } else {
                entry.cooldown_until
            };
            let coalesced = {
                let health = entry_effective_health_mut(entry, model);
                let same_kind = health.last_error_kind.as_deref() == Some(kind.as_str());
                let in_same_failure_wave = health.last_error_at_ms.is_some_and(|last_at| {
                    now_ms >= last_at
                        && now_ms.saturating_sub(last_at) <= coalesce_window.as_millis() as i64
                });
                retry_after.is_none()
                    && existing_until.is_some_and(|until| until > now)
                    && same_kind
                    && in_same_failure_wave
            };
            let streak = {
                let health = entry_effective_health_mut(entry, model);
                if coalesced {
                    health.transient_failure_streak = health.transient_failure_streak.max(1);
                } else {
                    health.transient_failure_streak =
                        health.transient_failure_streak.saturating_add(1);
                }
                health.recent_error_rate += alpha * (1.0 - health.recent_error_rate);
                health.last_error_kind = Some(kind.as_str().to_string());
                health.last_error_reason = Some(reason.clone());
                health.last_error_at_ms = Some(now_ms);
                health.transient_failure_streak
            };
            let duration =
                Self::local_cooldown_duration(retry_after, base, max, multiplier, jitter, streak);
            let candidate_until = now + duration;
            let until = if coalesced {
                existing_until.unwrap_or(candidate_until)
            } else {
                existing_until
                    .filter(|existing| *existing >= candidate_until)
                    .unwrap_or(candidate_until)
            };
            if let (Some(model), Some(key)) = (model, model_key) {
                let should_update = entry
                    .model_cooldowns
                    .get(&key)
                    .is_none_or(|existing| until > existing.until);
                if should_update {
                    entry.model_cooldowns.insert(
                        key,
                        CredentialModelCooldown {
                            model: model.to_string(),
                            until,
                            reason: Some(reason.clone()),
                        },
                    );
                }
            } else if entry.cooldown_until.is_none_or(|existing| until > existing) {
                entry.cooldown_until = Some(until);
                entry.cooldown_reason = Some(reason.clone());
            }
            let health = entry_effective_health_mut(entry, model);
            health.probation_until_ms = Some(health.probation_until_ms.unwrap_or(0).max(
                now_ms
                    + until.saturating_duration_since(now).as_millis() as i64
                    + probation.as_millis() as i64,
            ));
            entry.last_used_at = Some(Utc::now().to_rfc3339());
            (until.saturating_duration_since(now), coalesced)
        };

        if !coalesced {
            if let Some(redis) = &self.redis_store {
                let redis = redis.clone();
                let reason_for_redis = reason.clone();
                let model_for_redis = model.map(str::to_string);
                spawn_best_effort_storage_task("写入 Redis 凭据临时冷却与健康状态", async move {
                    redis
                        .record_scheduler_transient_failure(
                            id,
                            model_for_redis.as_deref(),
                            kind.as_str(),
                            &reason_for_redis,
                            retry_after,
                            base,
                            max,
                            multiplier,
                            jitter,
                            probation,
                            alpha,
                            coalesce_window,
                        )
                        .await?;
                    Ok(())
                });
            }
        }

        if coalesced {
            tracing::debug!(
                "凭据 #{} 因 {} 瞬态错误仍处于同一波冷却，未放大退避，剩余约 {} 秒: {}",
                id,
                kind.as_str(),
                duration.as_secs(),
                reason
            );
        } else {
            tracing::warn!(
                "凭据 #{} 因 {} 瞬态错误进入临时冷却 {} 秒: {}",
                id,
                kind.as_str(),
                duration.as_secs(),
                reason
            );
        }

        let has_alternate = {
            let entries = self.entries.lock();
            let proxy_resources = self.proxy_resources.lock();
            let config = self.config.lock().clone();
            let max_concurrent_requests = config.credential_max_concurrent_requests;
            let global_rpm = config.credential_rpm.unwrap_or(0);
            entries.iter().any(|e| {
                e.id != id
                    && credential_is_dispatchable(
                        &proxy_resources,
                        e,
                        model,
                        Instant::now(),
                        max_concurrent_requests,
                        global_rpm,
                        1,
                    )
            })
        };
        self.notify_dispatch_state_changed();
        Ok(has_alternate)
    }

    /// 报告指定会话在该凭据上的 API 调用成功，并清理 sticky 软失败计数。
    #[allow(dead_code)]
    pub fn report_success_for_session(&self, id: u64, session_id: Option<&str>) {
        self.report_success_for_session_with_latency(id, None, session_id, None);
    }

    pub fn report_success_for_session_with_latency(
        &self,
        id: u64,
        model: Option<&str>,
        session_id: Option<&str>,
        latency: Option<StdDuration>,
    ) {
        self.report_success_with_latency(id, model, latency);
        if let Some(sid) = session_id {
            self.clear_session_soft_failure(sid, id);
        }
    }

    pub fn report_success_for_session_with_latency_deferred(
        &self,
        id: u64,
        model: Option<&str>,
        session_id: Option<&str>,
        latency: Option<StdDuration>,
    ) {
        self.report_success_with_latency_deferred(id, model, latency);
        if let Some(sid) = session_id {
            self.clear_session_soft_failure_deferred(sid, id);
        }
    }

    /// 报告指定凭据 API 调用失败
    ///
    /// 增加失败计数，达到阈值时禁用凭据并切换到优先级最高的可用凭据
    /// 返回是否还有可用凭据可以重试
    ///
    /// # Arguments
    /// * `id` - 凭据 ID（来自 CallContext）
    pub fn report_failure(&self, id: u64) -> bool {
        let expected_generation = {
            let entries = self.entries.lock();
            let Some(entry) = entries.iter().find(|entry| entry.id == id) else {
                return entries.iter().any(|entry| !entry.disabled);
            };
            if entry.disabled && !entry.runtime_persistence_degraded {
                return entries.iter().any(|entry| !entry.disabled);
            }
            entry.runtime_generation
        };

        let last_used_at = Utc::now().to_rfc3339();
        let operation_id = uuid::Uuid::new_v4();
        let queue_without_attempt = self.postgres_store.is_some()
            && (self.has_pending_runtime_mutations(id)
                || self
                    .entries
                    .lock()
                    .iter()
                    .find(|entry| entry.id == id)
                    .is_some_and(|entry| entry.runtime_persistence_degraded));
        let mut persistence_failed = queue_without_attempt;
        let persisted_state = (!queue_without_attempt)
            .then_some(self.postgres_store.as_ref())
            .flatten()
            .and_then(|store| {
                let store = store.clone();
                let last_used_at = last_used_at.clone();
                match block_on_credential_pgsql("原子记录 PgSQL 凭据 API 失败", async move {
                    store
                        .record_credential_api_failure_at_generation(
                            id,
                            operation_id,
                            expected_generation,
                            &last_used_at,
                            MAX_FAILURES_PER_CREDENTIAL,
                        )
                        .await
                }) {
                    Ok(state) => Some(state),
                    Err(err) => {
                        persistence_failed = true;
                        tracing::warn!(
                            credential_id = id,
                            %operation_id,
                            "{}；本次先更新本地失败计数，等待 PgSQL 恢复后按 FIFO 重放",
                            err
                        );
                        None
                    }
                }
            });

        let (failure_count, mut disabled, auto_disabled) = {
            let mut entries = self.entries.lock();
            let entry = match entries.iter_mut().find(|e| e.id == id) {
                Some(e) => e,
                None => return entries.iter().any(|e| !e.disabled),
            };

            if entry.disabled && !entry.runtime_persistence_degraded {
                return entries.iter().any(|e| !e.disabled);
            }

            if let Some(result) = persisted_state.as_ref() {
                Self::apply_runtime_mutation_result_to_entry(entry, result);
            } else {
                entry.failure_count = entry.failure_count.saturating_add(1);
                if entry.failure_count >= MAX_FAILURES_PER_CREDENTIAL {
                    entry.disabled = true;
                    entry.disabled_reason = Some(DisabledReason::TooManyFailures);
                }
            }
            entry.last_used_at = Some(last_used_at.clone());
            let failure_count = entry.failure_count;
            let disabled = entry.disabled;
            let auto_disabled = entry.disabled_reason == Some(DisabledReason::TooManyFailures);

            tracing::warn!(
                "凭据 #{} API 调用失败（{}/{}）",
                id,
                failure_count,
                MAX_FAILURES_PER_CREDENTIAL
            );

            if disabled {
                tracing::error!("凭据 #{} 已连续失败 {} 次，已被禁用", id, failure_count);
            }
            (failure_count, disabled, auto_disabled)
        };

        if persistence_failed {
            self.enqueue_pending_runtime_mutation(
                id,
                PendingCredentialRuntimeMutation::ApiFailure {
                    operation_id,
                    expected_generation,
                    last_used_at: last_used_at.clone(),
                },
            );
            disabled = true;
        }

        if disabled {
            self.select_highest_priority();
            self.unbind_sessions_for_credential(id);
            self.clear_scheduler_state_for_credential(id, false);
        }
        if auto_disabled {
            self.record_scheduler_credential_audit(
                "auto_disable_credential",
                id,
                DisabledReason::TooManyFailures,
                "api_failure_threshold",
                "连续 API 调用失败达到阈值，已自动禁用凭据",
                serde_json::json!({ "failureCount": failure_count }),
            );
        }
        let result = {
            let entries = self.entries.lock();
            entries.iter().any(|e| !e.disabled)
        };
        if auto_disabled {
            self.publish_credentials_changed("credential_failure_reported");
            self.invalidate_model_capability_cohorts();
        }
        self.notify_dispatch_state_changed();
        result
    }

    /// 报告指定凭据额度已用尽
    ///
    /// 用于处理 402 Payment Required 且 reason 表示额度用尽的场景：
    /// - 立即禁用该凭据（不等待连续失败阈值）
    /// - 切换到下一个可用凭据继续重试
    /// - 返回是否还有可用凭据
    pub fn report_quota_exhausted(&self, id: u64) -> bool {
        let last_used_at: String;
        let expected_generation: u64;
        let result = {
            let mut entries = self.entries.lock();
            let mut current_id = self.current_id.lock();

            let entry = match entries.iter_mut().find(|e| e.id == id) {
                Some(e) => e,
                None => return entries.iter().any(|e| !e.disabled),
            };

            if entry.disabled {
                return entries.iter().any(|e| !e.disabled);
            }
            expected_generation = entry.runtime_generation;

            entry.disabled = true;
            entry.disabled_reason = Some(DisabledReason::QuotaExceeded);
            let now = Utc::now().to_rfc3339();
            entry.last_used_at = Some(now.clone());
            last_used_at = now;
            // 设为阈值，便于在管理面板中直观看到该凭据已不可用
            entry.failure_count = MAX_FAILURES_PER_CREDENTIAL;

            tracing::error!("凭据 #{} 额度已用尽，已被禁用", id);

            // 切换到优先级最高的可用凭据
            if let Some(next) = entries
                .iter()
                .filter(|e| !e.disabled)
                .min_by_key(|e| e.credentials.priority)
            {
                *current_id = next.id;
                tracing::info!(
                    "已切换到凭据 #{}（优先级 {}）",
                    next.id,
                    next.credentials.priority
                );
                true
            } else {
                tracing::error!("所有凭据均已禁用！");
                false
            }
        };
        self.unbind_sessions_for_credential(id);
        self.clear_scheduler_state_for_credential(id, false);
        self.persist_disabled_state(
            id,
            expected_generation,
            DisabledReason::QuotaExceeded,
            Some(MAX_FAILURES_PER_CREDENTIAL),
            None,
            &last_used_at,
        );
        self.record_scheduler_credential_audit(
            "auto_disable_credential",
            id,
            DisabledReason::QuotaExceeded,
            "upstream_quota_exhausted",
            "上游返回额度耗尽，已自动禁用凭据并切换调度",
            serde_json::json!({
                "failureCountSetTo": MAX_FAILURES_PER_CREDENTIAL,
            }),
        );
        self.publish_credentials_changed("credential_quota_exhausted");
        self.invalidate_model_capability_cohorts();
        self.notify_dispatch_state_changed();
        result
    }

    /// 报告指定凭据被上游明确风控、暂停或锁定。
    ///
    /// 这类错误不是普通瞬态 429，也不是连续失败阈值问题；继续调度该凭据通常只会
    /// 放大风控。这里立即禁用并记录独立原因，后台可通过 reset/enable 人工恢复。
    pub(crate) fn report_risk_controlled_outcome(
        &self,
        id: u64,
        reason: CredentialRiskControlReason,
        detail: impl Into<String>,
    ) -> RiskControlReportOutcome {
        let detail = detail.into();
        let detail_summary = truncate_for_audit(&detail, 500);
        let disabled_reason = reason.disabled_reason();
        let last_used_at: String;
        let expected_generation: u64;
        let result = {
            let mut entries = self.entries.lock();
            let mut current_id = self.current_id.lock();

            let entry = match entries.iter_mut().find(|e| e.id == id) {
                Some(e) => e,
                None => {
                    let circuit = self.record_local_pool_risk_circuit_failure(id, reason);
                    return RiskControlReportOutcome {
                        has_available_credentials: entries.iter().any(|e| !e.disabled),
                        circuit_open: circuit.open,
                        retry_after_secs: circuit
                            .retry_after
                            .map(|duration| duration.as_secs().saturating_add(1).max(1)),
                    };
                }
            };

            if entry.disabled {
                let circuit = self.record_local_pool_risk_circuit_failure(id, reason);
                return RiskControlReportOutcome {
                    has_available_credentials: entries.iter().any(|e| !e.disabled),
                    circuit_open: circuit.open,
                    retry_after_secs: circuit
                        .retry_after
                        .map(|duration| duration.as_secs().saturating_add(1).max(1)),
                };
            }
            expected_generation = entry.runtime_generation;

            entry.disabled = true;
            entry.disabled_reason = Some(disabled_reason);
            let now = Utc::now().to_rfc3339();
            entry.last_used_at = Some(now.clone());
            last_used_at = now;
            entry.failure_count = MAX_FAILURES_PER_CREDENTIAL;

            tracing::error!(
                credential_id = id,
                reason = reason.event_reason(),
                detail = %detail,
                "凭据 #{} 命中上游{}，已被禁用",
                id,
                reason.label()
            );

            if let Some(next) = entries
                .iter()
                .filter(|e| !e.disabled)
                .min_by_key(|e| e.credentials.priority)
            {
                *current_id = next.id;
                tracing::info!(
                    "已切换到凭据 #{}（优先级 {}）",
                    next.id,
                    next.credentials.priority
                );
                true
            } else {
                tracing::error!("所有凭据均已禁用！");
                false
            }
        };
        let circuit = self.record_local_pool_risk_circuit_failure(id, reason);
        self.unbind_sessions_for_credential(id);
        self.clear_scheduler_state_for_credential(id, false);
        self.persist_disabled_state(
            id,
            expected_generation,
            disabled_reason,
            Some(MAX_FAILURES_PER_CREDENTIAL),
            None,
            &last_used_at,
        );
        if let Some(store) = &self.postgres_store {
            let store = store.clone();
            let event_reason = reason.event_reason().to_string();
            let detail_value = serde_json::json!({
                "reason": event_reason,
                "detail": detail,
            });
            spawn_best_effort_storage_task("记录凭据风控事件到 PgSQL", async move {
                store
                    .record_credential_event(
                        Some(id),
                        "credential_risk_controlled",
                        Some(&event_reason),
                        detail_value,
                    )
                    .await
            });
        }
        self.record_scheduler_credential_audit(
            "auto_disable_credential",
            id,
            disabled_reason,
            "upstream_risk_controlled",
            "上游返回风控、暂停或锁定状态，已自动禁用凭据并切换调度",
            serde_json::json!({
                "riskReason": reason.event_reason(),
                "upstreamErrorSummary": detail_summary,
                "failureCountSetTo": MAX_FAILURES_PER_CREDENTIAL,
            }),
        );
        self.publish_credentials_changed("credential_risk_controlled");
        self.invalidate_model_capability_cohorts();
        self.notify_dispatch_state_changed();
        RiskControlReportOutcome {
            has_available_credentials: result,
            circuit_open: circuit.open,
            retry_after_secs: circuit
                .retry_after
                .map(|duration| duration.as_secs().saturating_add(1).max(1)),
        }
    }

    /// 报告指定凭据刷新 Token 失败。
    ///
    /// 连续刷新失败达到阈值后禁用凭据并切换，阈值内保持当前凭据不切换，
    /// 与 API 401/403 的累计失败策略保持一致。
    #[cfg(test)]
    pub fn report_refresh_failure(&self, id: u64) -> bool {
        let expected_generation = {
            let entries = self.entries.lock();
            let Some(entry) = entries.iter().find(|entry| entry.id == id) else {
                return entries.iter().any(|entry| !entry.disabled);
            };
            if entry.disabled && !entry.runtime_persistence_degraded {
                return entries.iter().any(|entry| !entry.disabled);
            }
            entry.runtime_generation
        };

        let last_used_at = Utc::now().to_rfc3339();
        let operation_id = uuid::Uuid::new_v4();
        let queue_without_attempt = self.postgres_store.is_some()
            && (self.has_pending_runtime_mutations(id)
                || self
                    .entries
                    .lock()
                    .iter()
                    .find(|entry| entry.id == id)
                    .is_some_and(|entry| entry.runtime_persistence_degraded));
        let mut persistence_failed = queue_without_attempt;
        let persisted_state = (!queue_without_attempt)
            .then_some(self.postgres_store.as_ref())
            .flatten()
            .and_then(|store| {
                let store = store.clone();
                let last_used_at = last_used_at.clone();
                match block_on_credential_pgsql("原子记录 PgSQL 凭据 Token 刷新失败", async move {
                    store
                        .record_credential_refresh_failure_at_generation(
                            id,
                            operation_id,
                            expected_generation,
                            &last_used_at,
                            MAX_FAILURES_PER_CREDENTIAL,
                        )
                        .await
                }) {
                    Ok(state) => Some(state),
                    Err(err) => {
                        persistence_failed = true;
                        tracing::warn!(
                            credential_id = id,
                            %operation_id,
                            "{}；本次先更新本地刷新失败计数，等待 PgSQL 恢复后按 FIFO 重放",
                            err
                        );
                        None
                    }
                }
            });

        let (refresh_failure_count, mut disabled, auto_disabled) = {
            let mut entries = self.entries.lock();
            let entry = match entries.iter_mut().find(|entry| entry.id == id) {
                Some(entry) => entry,
                None => return entries.iter().any(|entry| !entry.disabled),
            };
            if entry.disabled && !entry.runtime_persistence_degraded {
                return entries.iter().any(|entry| !entry.disabled);
            }

            if let Some(result) = persisted_state.as_ref() {
                Self::apply_runtime_mutation_result_to_entry(entry, result);
            } else {
                entry.refresh_failure_count = entry.refresh_failure_count.saturating_add(1);
                if entry.refresh_failure_count >= MAX_FAILURES_PER_CREDENTIAL {
                    entry.disabled = true;
                    entry.disabled_reason = Some(DisabledReason::TooManyRefreshFailures);
                }
            }
            entry.last_used_at = Some(last_used_at.clone());

            let refresh_failure_count = entry.refresh_failure_count;
            let auto_disabled =
                entry.disabled_reason == Some(DisabledReason::TooManyRefreshFailures);
            tracing::warn!(
                "凭据 #{} Token 刷新失败（{}/{}）",
                id,
                refresh_failure_count,
                MAX_FAILURES_PER_CREDENTIAL
            );
            if auto_disabled {
                tracing::error!(
                    "凭据 #{} Token 已连续刷新失败 {} 次，已被禁用",
                    id,
                    refresh_failure_count
                );
            }
            (refresh_failure_count, entry.disabled, auto_disabled)
        };

        if persistence_failed {
            self.enqueue_pending_runtime_mutation(
                id,
                PendingCredentialRuntimeMutation::RefreshFailure {
                    operation_id,
                    expected_generation,
                    last_used_at: last_used_at.clone(),
                },
            );
            disabled = true;
        }

        if disabled {
            self.select_highest_priority();
            self.unbind_sessions_for_credential(id);
            self.clear_scheduler_state_for_credential(id, false);
        }
        if auto_disabled {
            self.record_scheduler_credential_audit(
                "auto_disable_credential",
                id,
                DisabledReason::TooManyRefreshFailures,
                "token_refresh_failure_threshold",
                "连续 Token 刷新失败达到阈值，已自动禁用凭据",
                serde_json::json!({ "refreshFailureCount": refresh_failure_count }),
            );
        }
        let result = {
            let entries = self.entries.lock();
            entries.iter().any(|e| !e.disabled)
        };
        if auto_disabled {
            self.publish_credentials_changed("credential_refresh_failure_reported");
            self.invalidate_model_capability_cohorts();
        }
        self.notify_dispatch_state_changed();
        result
    }

    /// 报告指定凭据的 refreshToken 永久失效（invalid_grant）。
    ///
    /// 立即禁用凭据，不累计、不重试。
    /// 返回是否还有可用凭据。
    pub fn report_refresh_token_invalid(&self, id: u64) -> bool {
        let last_used_at: String;
        let expected_generation: u64;
        let result = {
            let mut entries = self.entries.lock();
            let mut current_id = self.current_id.lock();

            let entry = match entries.iter_mut().find(|e| e.id == id) {
                Some(e) => e,
                None => return entries.iter().any(|e| !e.disabled),
            };

            if entry.disabled {
                return entries.iter().any(|e| !e.disabled);
            }
            expected_generation = entry.runtime_generation;

            let now = Utc::now().to_rfc3339();
            entry.last_used_at = Some(now.clone());
            last_used_at = now;
            entry.disabled = true;
            entry.disabled_reason = Some(DisabledReason::InvalidRefreshToken);

            tracing::error!(
                "凭据 #{} refreshToken 已失效 (invalid_grant)，已立即禁用",
                id
            );

            if let Some(next) = entries
                .iter()
                .filter(|e| !e.disabled)
                .min_by_key(|e| e.credentials.priority)
            {
                *current_id = next.id;
                tracing::info!(
                    "已切换到凭据 #{}（优先级 {}）",
                    next.id,
                    next.credentials.priority
                );
                true
            } else {
                tracing::error!("所有凭据均已禁用！");
                false
            }
        };
        self.unbind_sessions_for_credential(id);
        self.clear_scheduler_state_for_credential(id, false);
        self.persist_disabled_state(
            id,
            expected_generation,
            DisabledReason::InvalidRefreshToken,
            None,
            None,
            &last_used_at,
        );
        self.record_scheduler_credential_audit(
            "auto_disable_credential",
            id,
            DisabledReason::InvalidRefreshToken,
            "refresh_token_invalid_grant",
            "refreshToken 永久失效，已自动禁用凭据并切换调度",
            serde_json::json!({
                "upstreamReason": "invalid_grant",
            }),
        );
        self.publish_credentials_changed("credential_refresh_token_invalid");
        self.invalidate_model_capability_cohorts();
        self.notify_dispatch_state_changed();
        result
    }

    /// 切换到优先级最高的可用凭据
    ///
    /// 返回是否成功切换
    pub fn switch_to_next(&self) -> bool {
        let entries = self.entries.lock();
        let mut current_id = self.current_id.lock();

        // 选择优先级最高的未禁用凭据（排除当前凭据）
        if let Some(next) = entries
            .iter()
            .filter(|e| !e.disabled && e.id != *current_id)
            .min_by_key(|e| e.credentials.priority)
        {
            *current_id = next.id;
            tracing::info!(
                "已切换到凭据 #{}（优先级 {}）",
                next.id,
                next.credentials.priority
            );
            true
        } else {
            // 没有其他可用凭据，检查当前凭据是否可用
            entries.iter().any(|e| e.id == *current_id && !e.disabled)
        }
    }

    // ========================================================================
    // Admin API 方法
    // ========================================================================

    /// Return the stable local credential cohort that can participate in model dispatch.
    ///
    /// This is a deliberately small, storage-free snapshot for validating a previously observed
    /// model-capability contract. It does not refresh tokens, synchronize Redis/PgSQL state, hash
    /// credentials, or inspect transient cooldown/RPM/concurrency state. Transient availability
    /// must not make an upstream schema appear and disappear between retries.
    pub(crate) fn local_model_capability_cohorts(&self) -> Arc<Vec<KiroModelCapabilityCohort>> {
        let generation = self
            .model_capability_cohort_generation
            .load(Ordering::Acquire);
        let mut cache = self.model_capability_cohort_cache.lock();
        if cache.generation == generation {
            return cache.cohorts.clone();
        }
        let config = self.config.lock().clone();
        let entries = self.entries.lock();
        let proxy_resources = self.proxy_resources.lock();
        let mut cohorts = BTreeMap::<KiroModelCapabilityCohortKey, Vec<u64>>::new();
        for entry in entries.iter().filter(|entry| {
            !entry.disabled
                && credential_proxy_is_dispatchable(&entry.credentials, &proxy_resources)
        }) {
            let credentials = &entry.credentials;
            let normalize = |value: Option<&str>, fallback: &str| {
                value
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .unwrap_or(fallback)
                    .to_ascii_lowercase()
            };
            let mut supported_models = credentials
                .supported_models
                .iter()
                .map(|model| model.trim().to_ascii_lowercase())
                .filter(|model| !model.is_empty())
                .collect::<Vec<_>>();
            supported_models.sort_unstable();
            supported_models.dedup();
            let key = KiroModelCapabilityCohortKey {
                endpoint_family: normalize(
                    credentials.endpoint.as_deref(),
                    &config.default_endpoint,
                ),
                auth_method: if credentials.is_api_key_credential() {
                    "api_key".to_string()
                } else {
                    normalize(credentials.auth_method.as_deref(), "unknown")
                },
                provider: normalize(credentials.provider.as_deref(), "unknown"),
                effective_auth_region: credentials
                    .effective_auth_region(&config)
                    .trim()
                    .to_ascii_lowercase(),
                effective_api_region: credentials
                    .effective_api_region(&config)
                    .trim()
                    .to_ascii_lowercase(),
                subscription_class: normalize(credentials.subscription_title.as_deref(), "unknown"),
                supported_models,
            };
            cohorts.entry(key).or_default().push(entry.id);
        }
        let cohorts = cohorts
            .into_iter()
            .map(|(key, mut credential_ids)| {
                credential_ids.sort_unstable();
                KiroModelCapabilityCohort {
                    key,
                    credential_ids,
                }
            })
            .collect::<Vec<_>>();
        cache.generation = generation;
        cache.cohorts = Arc::new(cohorts);
        cache.keys = Arc::new(
            cache
                .cohorts
                .iter()
                .map(|cohort| cohort.key.clone())
                .collect(),
        );
        #[cfg(test)]
        {
            cache.rebuilds = cache.rebuilds.saturating_add(1);
        }
        cache.cohorts.clone()
    }

    pub(crate) fn local_model_capability_cohort_keys(
        &self,
    ) -> Arc<Vec<KiroModelCapabilityCohortKey>> {
        let generation = self
            .model_capability_cohort_generation
            .load(Ordering::Acquire);
        {
            let cache = self.model_capability_cohort_cache.lock();
            if cache.generation == generation {
                return cache.keys.clone();
            }
        }
        // Populate or refresh both Arc snapshots through the single cold rebuild path.
        let _ = self.local_model_capability_cohorts();
        self.model_capability_cohort_cache.lock().keys.clone()
    }

    fn invalidate_model_capability_cohorts(&self) {
        self.model_capability_cohort_generation
            .fetch_add(1, Ordering::AcqRel);
    }

    #[cfg(test)]
    pub(crate) fn model_capability_cohort_rebuilds(&self) -> u64 {
        self.model_capability_cohort_cache.lock().rebuilds
    }

    /// 获取管理器状态快照（用于 Admin API）
    pub fn snapshot(&self) -> ManagerSnapshot {
        self.cleanup_expired_in_flight_leases_local_first();
        self.refresh_stats_from_postgres();
        self.refresh_scheduler_state_from_redis_best_effort();
        let config = self.config.lock().clone();
        let mut entries = self.entries.lock();
        if self.redis_store.is_none() {
            refresh_local_selection_windows_locked(&mut entries, Instant::now());
        }
        let current_id = *self.current_id.lock();
        let available = entries.iter().filter(|e| !e.disabled).count();
        let now = Instant::now();
        let now_ms = Utc::now().timestamp_millis();
        let max_concurrent_requests = config.credential_max_concurrent_requests;
        let global_rpm = config.credential_rpm.unwrap_or(0);
        let lease_max_age = (config.credential_in_flight_lease_max_secs > 0)
            .then(|| StdDuration::from_secs(config.credential_in_flight_lease_max_secs));
        let local_global_in_flight = entries
            .iter()
            .map(|entry| entry.in_flight_requests)
            .sum::<u32>();
        let global_capacity = SchedulerGlobalCapacityState {
            in_flight_requests: local_global_in_flight,
            queued_requests: self.queued_requests.load(Ordering::Acquire),
        };
        let proxy_resources = self.proxy_resources.lock();
        let score_candidates: Vec<_> = entries
            .iter()
            .filter(|entry| {
                credential_is_dispatchable(
                    &proxy_resources,
                    entry,
                    None,
                    now,
                    max_concurrent_requests,
                    global_rpm,
                    1,
                )
            })
            .collect();
        let score_total_recent: u64 = score_candidates
            .iter()
            .map(|entry| entry.health.recent_selection_count_60s as u64)
            .sum();
        let score_candidate_count = score_candidates.len();
        let entries_snapshot = entries
            .iter()
            .map(|entry| {
                runtime_snapshot_from_entry(
                    entry,
                    &config,
                    &proxy_resources,
                    self.proxy.as_ref(),
                    max_concurrent_requests,
                    lease_max_age,
                    now,
                    now_ms,
                    score_total_recent,
                    score_candidate_count,
                    sha256_hex,
                    mask_api_key,
                )
            })
            .collect();

        ManagerSnapshot {
            entries: entries_snapshot,
            current_id,
            total: entries.len(),
            available,
            global_in_flight_requests: global_capacity.in_flight_requests,
            queued_requests: global_capacity.queued_requests,
            global_max_concurrent_requests: config.dispatch_global_max_concurrent_requests,
            max_queued_requests: config.dispatch_max_queued_requests,
        }
    }

    /// 获取轻量凭据基础快照（用于 Admin 列表首屏）。
    ///
    /// 该路径不主动同步 PgSQL/Redis，也不计算调度评分，避免列表请求被外部存储拖慢。
    pub fn base_snapshot(&self) -> ManagerBaseSnapshot {
        self.cleanup_expired_in_flight_leases_local_first();
        let config = self.config.lock().clone();
        let entries = self.entries.lock();
        let current_id = *self.current_id.lock();
        let available = entries.iter().filter(|e| !e.disabled).count();
        let global_in_flight_requests = entries.iter().map(|entry| entry.in_flight_requests).sum();
        let proxy_resources = self.proxy_resources.lock();
        let entries_snapshot = entries
            .iter()
            .map(|entry| {
                base_snapshot_from_entry(
                    entry,
                    &config,
                    &proxy_resources,
                    self.proxy.as_ref(),
                    sha256_hex,
                    mask_api_key,
                )
            })
            .collect();

        ManagerBaseSnapshot {
            entries: entries_snapshot,
            current_id,
            total: entries.len(),
            available,
            global_in_flight_requests,
            queued_requests: self.queued_requests.load(Ordering::Acquire),
            global_max_concurrent_requests: config.dispatch_global_max_concurrent_requests,
            max_queued_requests: config.dispatch_max_queued_requests,
            runtime_fresh: self.redis_store.is_none(),
        }
    }

    /// 获取凭据数量和全局调度容量快照。
    ///
    /// 该路径只读计数和全局容量，不构造每个凭据的运行态详情，供 Admin 顶部概览高频轮询使用。
    pub fn summary_snapshot(&self) -> ManagerSummarySnapshot {
        self.cleanup_expired_in_flight_leases_local_first();
        let config = self.config.lock().clone();
        let (current_id, total, available, local_global_in_flight) = {
            let entries = self.entries.lock();
            (
                *self.current_id.lock(),
                entries.len(),
                entries.iter().filter(|e| !e.disabled).count(),
                entries.iter().map(|entry| entry.in_flight_requests).sum(),
            )
        };
        let (global_capacity, runtime_fresh) = if self.redis_store.is_some() {
            (self.global_capacity_state(), true)
        } else {
            (
                SchedulerGlobalCapacityState {
                    in_flight_requests: local_global_in_flight,
                    queued_requests: self.queued_requests.load(Ordering::Acquire),
                },
                true,
            )
        };

        ManagerSummarySnapshot {
            current_id,
            total,
            available,
            global_in_flight_requests: global_capacity.in_flight_requests,
            queued_requests: global_capacity.queued_requests,
            global_max_concurrent_requests: config.dispatch_global_max_concurrent_requests,
            max_queued_requests: config.dispatch_max_queued_requests,
            runtime_fresh,
        }
    }

    /// 获取指定凭据的运行态快照（用于 Admin 当前页补充）。
    pub fn runtime_snapshot_for_ids(&self, ids: &[u64]) -> ManagerRuntimeSnapshot {
        self.cleanup_expired_in_flight_leases_local_first();
        let ids: HashSet<u64> = ids.iter().copied().collect();
        let mut runtime_fresh = self.redis_store.is_none();
        if self.redis_store.is_some() && !ids.is_empty() {
            runtime_fresh = match self.refresh_scheduler_state_from_redis_for_ids(&ids) {
                Ok(()) => true,
                Err(err) => {
                    tracing::warn!("从 Redis 同步指定凭据调度运行态失败: {}", err);
                    false
                }
            };
        }

        let config = self.config.lock().clone();
        let mut entries = self.entries.lock();
        let now = Instant::now();
        if self.redis_store.is_none() {
            refresh_local_selection_windows_locked(&mut entries, now);
        }
        let current_id = *self.current_id.lock();
        let available = entries.iter().filter(|e| !e.disabled).count();
        let now_ms = Utc::now().timestamp_millis();
        let max_concurrent_requests = config.credential_max_concurrent_requests;
        let global_rpm = config.credential_rpm.unwrap_or(0);
        let lease_max_age = (config.credential_in_flight_lease_max_secs > 0)
            .then(|| StdDuration::from_secs(config.credential_in_flight_lease_max_secs));
        let proxy_resources = self.proxy_resources.lock();
        let score_candidates: Vec<_> = entries
            .iter()
            .filter(|entry| {
                credential_is_dispatchable(
                    &proxy_resources,
                    entry,
                    None,
                    now,
                    max_concurrent_requests,
                    global_rpm,
                    1,
                )
            })
            .collect();
        let score_total_recent: u64 = score_candidates
            .iter()
            .map(|entry| entry.health.recent_selection_count_60s as u64)
            .sum();
        let score_candidate_count = score_candidates.len();
        let entries_snapshot = entries
            .iter()
            .filter(|entry| ids.is_empty() || ids.contains(&entry.id))
            .map(|entry| {
                runtime_snapshot_from_entry(
                    entry,
                    &config,
                    &proxy_resources,
                    self.proxy.as_ref(),
                    max_concurrent_requests,
                    lease_max_age,
                    now,
                    now_ms,
                    score_total_recent,
                    score_candidate_count,
                    sha256_hex,
                    mask_api_key,
                )
            })
            .collect();

        ManagerRuntimeSnapshot {
            entries: entries_snapshot,
            current_id,
            total: entries.len(),
            available,
            runtime_fresh,
        }
    }

    /// 导出完整凭据快照（包含 refreshToken / kiroApiKey 等敏感字段）。
    ///
    /// 仅供 Admin API 显式导出使用；不改变调度状态，也不触发持久化。
    pub fn export_credentials(&self) -> Vec<KiroCredentials> {
        let mut credentials: Vec<KiroCredentials> = {
            let entries = self.entries.lock();
            entries
                .iter()
                .map(|entry| {
                    let mut credentials = entry.credentials.clone();
                    credentials.canonicalize_auth_method();
                    credentials.normalize_api_key_defaults();
                    credentials.normalize_external_idp_defaults();
                    credentials.disabled = entry.disabled;
                    credentials
                })
                .collect()
        };

        credentials.sort_by_key(|credential| (credential.priority, credential.id.unwrap_or(0)));
        credentials
    }

    /// 设置凭据禁用状态（Admin API）
    pub fn set_disabled(&self, id: u64, disabled: bool) -> anyhow::Result<()> {
        {
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
        }
        self.invalidate_refresh_state_for_credential(id);
        let persisted = self.persist_runtime_patch_value(
            id,
            CredentialRuntimeStatePatch {
                failure_count: (!disabled).then_some(0),
                refresh_failure_count: (!disabled).then_some(0),
                disabled_reason: if disabled {
                    CredentialRuntimeDisabledReasonPatch::Set(
                        DisabledReason::Manual.as_str().to_string(),
                    )
                } else {
                    CredentialRuntimeDisabledReasonPatch::Clear
                },
                credential_disabled: Some(disabled),
                advance_generation: true,
                ..Default::default()
            },
        )?;

        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            if !persisted {
                entry.credentials.disabled = disabled;
                entry.disabled = disabled;
                entry.disabled_reason = disabled.then_some(DisabledReason::Manual);
                if !disabled {
                    entry.failure_count = 0;
                    entry.refresh_failure_count = 0;
                }
            }
            entry.cooldown_until = None;
            entry.cooldown_reason = None;
            entry.model_cooldowns.clear();
            entry.rate_limit_available_at = None;
        }
        if disabled {
            self.unbind_sessions_for_credential(id);
            self.clear_scheduler_state_for_credential(id, false);
        } else {
            self.clear_scheduler_state_for_credential(id, false);
            self.select_highest_priority();
        }
        self.invalidate_model_capability_cohorts();
        self.notify_dispatch_state_changed();
        self.publish_credentials_changed("credential_disabled_updated");
        Ok(())
    }

    /// 设置凭据优先级（Admin API）
    ///
    /// 修改优先级后会立即按新优先级重新选择当前凭据。
    pub fn set_priority(&self, id: u64, priority: u32) -> anyhow::Result<()> {
        self.persist_credential_mutation(id, |credential| {
            credential.priority = priority;
            Ok(())
        })?;
        self.select_highest_priority();
        self.notify_dispatch_state_changed();
        self.publish_credentials_changed("credential_priority_updated");
        Ok(())
    }

    /// 设置凭据级最大并发覆盖（Admin API）。
    ///
    /// `None` 表示继承全局 `credentialMaxConcurrentRequests`；
    /// `Some(0)` 表示该凭据不限并发；
    /// `Some(n)` 表示该凭据最多同时处理 n 个请求。
    pub fn set_credential_max_concurrent_requests(
        &self,
        id: u64,
        max_concurrent_requests: Option<u32>,
    ) -> anyhow::Result<()> {
        self.persist_credential_capacity_mutation(id, |credential| {
            let changed = credential.max_concurrent_requests != max_concurrent_requests;
            credential.max_concurrent_requests = max_concurrent_requests;
            Ok(changed)
        })?;

        self.notify_dispatch_state_changed();
        self.publish_credentials_changed("credential_concurrency_updated");
        Ok(())
    }

    /// 设置凭据级 RPM 覆盖（Admin API）。
    ///
    /// `None` 表示继承全局 `credentialRpm`；
    /// `Some(0)` 表示该凭据不做本地 RPM 限制；
    /// `Some(n)` 表示该凭据每分钟最多调度 n 次。
    pub fn set_credential_rpm(&self, id: u64, rpm: Option<u32>) -> anyhow::Result<()> {
        self.persist_credential_capacity_mutation(id, |credential| {
            let changed = credential.rpm != rpm;
            credential.rpm = rpm;
            Ok(changed)
        })?;

        self.clear_rate_limit_for_credential(id);
        self.notify_dispatch_state_changed();
        self.publish_credentials_changed("credential_rpm_updated");
        Ok(())
    }

    /// 设置 429 临时风控自动禁用开关（Admin API）。
    pub fn set_credential_rate_limit_auto_disable(
        &self,
        id: u64,
        enabled: bool,
    ) -> anyhow::Result<()> {
        self.persist_credential_mutation(id, |credential| {
            credential.rate_limit_auto_disable_enabled = Some(enabled);
            Ok(())
        })?;

        self.notify_dispatch_state_changed();
        self.publish_credentials_changed("credential_rate_limit_auto_disable_updated");
        Ok(())
    }

    /// 设置凭据支持模型列表（Admin API）。
    ///
    /// 空列表表示不限制该凭据可调度的模型。
    pub fn set_credential_supported_models(
        &self,
        id: u64,
        supported_models: Vec<String>,
    ) -> anyhow::Result<Vec<String>> {
        let saved = self.persist_credential_mutation(id, |credential| {
            credential.supported_models = supported_models;
            credential.normalize_supported_models();
            Ok(())
        })?;

        self.notify_dispatch_state_changed();
        self.publish_credentials_changed("credential_supported_models_updated");
        Ok(saved.supported_models)
    }

    /// 设置凭据 Region 覆盖值（Admin API）。
    ///
    /// `region` 是旧兼容字段，主要作为 Auth Region 回退；`auth_region`
    /// 控制 token 刷新；`api_region` 控制 q.{region}.amazonaws.com 请求。
    pub fn set_credential_regions(
        &self,
        id: u64,
        region: Option<Option<String>>,
        auth_region: Option<Option<String>>,
        api_region: Option<Option<String>>,
    ) -> anyhow::Result<()> {
        self.persist_credential_mutation(id, |credential| {
            let region_changed = region.is_some() || auth_region.is_some() || api_region.is_some();
            if let Some(value) = region {
                credential.region = value;
            }
            if let Some(value) = auth_region {
                credential.auth_region = value;
            }
            if let Some(value) = api_region {
                credential.api_region = value;
            }
            if region_changed && !credential.is_api_key_credential() {
                credential.access_token = None;
                credential.expires_at = None;
                credential.subscription_title = None;
                if api_region_conflicts_with_profile_arn(credential) {
                    credential.profile_arn = None;
                }
            }
            Ok(())
        })?;

        self.publish_credentials_changed("credential_regions_updated");
        self.notify_dispatch_state_changed();
        Ok(())
    }

    pub fn update_credential_auth(
        &self,
        id: u64,
        update: CredentialAuthUpdate,
        reset_runtime_state: bool,
    ) -> anyhow::Result<()> {
        let (base, mut credential) = {
            let entries = self.entries.lock();
            let entry = entries
                .iter()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            let base = Self::credential_from_entry(entry);
            (base.clone(), base)
        };

        apply_credential_auth_update(&mut credential, update);
        credential.canonicalize_auth_method();
        credential.normalize_api_key_defaults();
        credential.normalize_external_idp_defaults();
        if credential.is_api_key_credential() {
            let api_key = credential
                .kiro_api_key
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("API Key 凭据缺少 kiroApiKey"))?;
            if api_key.trim().is_empty() {
                anyhow::bail!("kiroApiKey 为空");
            }
        } else {
            validate_refresh_token(&credential)?;
        }

        {
            let entries = self.entries.lock();
            if credential.is_api_key_credential() {
                let new_hash = credential.kiro_api_key.as_deref().map(sha256_hex);
                if let Some(new_hash) = new_hash.as_deref() {
                    let duplicate = entries.iter().any(|entry| {
                        entry.id != id
                            && entry
                                .credentials
                                .kiro_api_key
                                .as_deref()
                                .map(sha256_hex)
                                .as_deref()
                                == Some(new_hash)
                    });
                    if duplicate {
                        anyhow::bail!("凭据已存在（kiroApiKey 重复）");
                    }
                }
            } else {
                let new_hash = credential.refresh_token.as_deref().map(sha256_hex);
                if let Some(new_hash) = new_hash.as_deref() {
                    let duplicate = entries.iter().any(|entry| {
                        entry.id != id
                            && entry
                                .credentials
                                .refresh_token
                                .as_deref()
                                .map(sha256_hex)
                                .as_deref()
                                == Some(new_hash)
                    });
                    if duplicate {
                        anyhow::bail!("凭据已存在（refreshToken 重复）");
                    }
                }
            }
        }

        self.invalidate_refresh_state_for_credential(id);

        if reset_runtime_state {
            self.persist_credential_update_with_runtime_patch(
                &base,
                &credential,
                CredentialRuntimeStatePatch {
                    failure_count: Some(0),
                    refresh_failure_count: Some(0),
                    disabled_reason: CredentialRuntimeDisabledReasonPatch::Clear,
                    credential_disabled: Some(false),
                    advance_generation: true,
                    ..Default::default()
                },
            )?;
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            entry.cooldown_until = None;
            entry.cooldown_reason = None;
            entry.model_cooldowns.clear();
            entry.rate_limit_available_at = None;
            entry.health = SchedulerHealthState::default();
            entry.model_health.clear();
            entry.selection_events.clear();
            drop(entries);
            self.clear_scheduler_state_for_credential(id, false);
        } else {
            self.persist_credential_update(&base, &credential)?;
        }

        self.publish_credentials_changed("credential_auth_updated");
        self.notify_dispatch_state_changed();
        Ok(())
    }

    pub fn update_credential_profile_arn(
        &self,
        id: u64,
        profile_arn: Option<String>,
    ) -> anyhow::Result<()> {
        let profile_arn = profile_arn
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        self.persist_credential_mutation(id, |credential| {
            credential.profile_arn = profile_arn;
            Ok(())
        })?;
        self.publish_credentials_changed("credential_profile_arn_updated");
        Ok(())
    }

    /// 重置凭据失败计数并重新启用（Admin API）
    pub fn reset_and_enable(&self, id: u64) -> anyhow::Result<()> {
        {
            let entries = self.entries.lock();
            let entry = entries
                .iter()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            if entry.disabled_reason == Some(DisabledReason::InvalidConfig) {
                anyhow::bail!("凭据 #{} 因配置无效被禁用，请修正配置后重启服务", id);
            }
        }
        self.invalidate_refresh_state_for_credential(id);
        let persisted = self.persist_runtime_patch_value(
            id,
            CredentialRuntimeStatePatch {
                failure_count: Some(0),
                refresh_failure_count: Some(0),
                disabled_reason: CredentialRuntimeDisabledReasonPatch::Clear,
                credential_disabled: Some(false),
                advance_generation: true,
                ..Default::default()
            },
        )?;

        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            if entry.disabled_reason == Some(DisabledReason::InvalidConfig) {
                anyhow::bail!("凭据 #{} 因配置无效被禁用，请修正配置后重启服务", id);
            }
            if !persisted {
                entry.failure_count = 0;
                entry.refresh_failure_count = 0;
                entry.credentials.disabled = false;
                entry.disabled = false;
                entry.disabled_reason = None;
            }
            entry.cooldown_until = None;
            entry.cooldown_reason = None;
            entry.model_cooldowns.clear();
            entry.rate_limit_available_at = None;
        }
        self.select_highest_priority();
        self.clear_scheduler_state_for_credential(id, false);
        self.invalidate_model_capability_cohorts();
        self.notify_dispatch_state_changed();
        self.publish_credentials_changed("credential_reset_and_enabled");
        Ok(())
    }

    /// 获取指定凭据的使用额度（Admin API）
    pub async fn get_usage_limits_for(&self, id: u64) -> anyhow::Result<UsageLimitsResponse> {
        let ctx = self.acquire_context_for_credential(id).await?;
        let token = ctx.token;
        let credentials = ctx.credentials;

        let effective_proxy = credentials.effective_proxy(self.proxy.as_ref());
        let config = self.runtime_config();
        let usage_limits =
            get_usage_limits(&credentials, &config, &token, effective_proxy.as_ref()).await?;

        // 更新订阅等级到凭据（仅在发生变化时持久化）
        if let Some(subscription_title) = usage_limits.subscription_title() {
            let old_title = self
                .entries
                .lock()
                .iter()
                .find(|entry| entry.id == id)
                .map(|entry| entry.credentials.subscription_title.clone());
            if old_title
                .as_ref()
                .is_some_and(|title| title.as_deref() != Some(subscription_title))
            {
                let requested_title = subscription_title.to_string();
                if let Err(e) = self.persist_credential_mutation(id, |credential| {
                    credential.subscription_title = Some(requested_title);
                    Ok(())
                }) {
                    tracing::warn!("订阅等级更新后持久化失败（不影响本次请求）: {}", e);
                } else {
                    tracing::info!(
                        "凭据 #{} 订阅等级已更新: {:?} -> {}",
                        id,
                        old_title.flatten(),
                        subscription_title
                    );
                    self.publish_credentials_changed("subscription_title_updated");
                }
            }
        }

        Ok(usage_limits)
    }

    /// 设置指定凭据的上游 Overages 开关并返回刷新后的 usageLimits。
    pub async fn set_overage_status_for(
        &self,
        id: u64,
        enabled: bool,
    ) -> anyhow::Result<UsageLimitsResponse> {
        let ctx = self.acquire_context_for_credential(id).await?;
        let token = ctx.token;
        let credentials = ctx.credentials;

        let effective_proxy = credentials.effective_proxy(self.proxy.as_ref());
        let config = self.runtime_config();
        set_overage_status(
            &credentials,
            &config,
            &token,
            effective_proxy.as_ref(),
            enabled,
        )
        .await?;

        let usage_limits =
            get_usage_limits(&credentials, &config, &token, effective_proxy.as_ref()).await?;
        Ok(usage_limits)
    }

    /// 使用一份外部凭据临时查询账号信息，不加入凭据池、不改变调度状态。
    ///
    /// 这个方法用于 Admin 的外部 JSON 订阅校验。它允许使用凭据绑定的代理资源，
    /// 但不会保存 token、不会启用/禁用任何系统凭据，也不会占用调度并发槽。
    pub async fn acquire_context_for_external_credentials(
        &self,
        mut credentials: KiroCredentials,
    ) -> anyhow::Result<CallContext> {
        credentials.canonicalize_auth_method();
        credentials.normalize_api_key_defaults();
        credentials.normalize_external_idp_defaults();

        if credentials.is_api_key_credential() {
            let credentials = self.resolve_proxy_for_credential(credentials)?;
            let token = credentials
                .kiro_api_key
                .clone()
                .ok_or_else(|| anyhow::anyhow!("API Key 凭据缺少 kiroApiKey"))?;
            return Ok(CallContext {
                id: EXTERNAL_CREDENTIAL_CONTEXT_ID,
                credentials,
                token,
                sticky_bound: false,
                fallback_from_sticky: false,
                in_flight_lease: None,
            });
        }

        let source_credentials = credentials.clone();
        let credentials = self.resolve_proxy_for_credential(credentials)?;
        let effective_proxy = credentials.effective_proxy(self.proxy.as_ref());
        let config = self.runtime_config();
        let admission = RefreshSendAdmission::new(
            None,
            self.auxiliary_concurrency_controller(),
            self.auxiliary_runtime.token_refresh_admission_controller(),
        );
        let client = self
            .refresh_http_client(config.tls_backend, effective_proxy.as_ref())
            .await?;
        let refreshed =
            refresh_token_with_client(&credentials, &config, client, Some(admission)).await?;
        let refreshed = Self::preserve_proxy_fields(refreshed, &source_credentials);
        self.token_context_from_credentials_until(
            EXTERNAL_CREDENTIAL_CONTEXT_ID,
            refreshed,
            false,
            tokio::time::Instant::now() + CREDENTIAL_PGSQL_WORKFLOW_TIMEOUT,
        )
        .await
    }

    /// 使用一份外部凭据临时查询账号信息，不加入凭据池、不改变调度状态。
    ///
    /// 这个方法用于 Admin 的外部 JSON 订阅校验。它允许使用凭据绑定的代理资源，
    /// 但不会保存 token、不会启用/禁用任何系统凭据，也不会占用调度并发槽。
    pub async fn probe_usage_limits_for_credentials(
        &self,
        credentials: KiroCredentials,
    ) -> anyhow::Result<UsageLimitsResponse> {
        let ctx = self
            .acquire_context_for_external_credentials(credentials)
            .await?;
        let effective_proxy = ctx.credentials.effective_proxy(self.proxy.as_ref());
        let config = self.runtime_config();
        get_usage_limits(
            &ctx.credentials,
            &config,
            &ctx.token,
            effective_proxy.as_ref(),
        )
        .await
    }

    /// 添加新凭据（Admin API）
    ///
    /// # 流程
    /// 1. 验证凭据基本字段（API Key: kiroApiKey 不为空; OAuth: refreshToken 不为空）
    /// 2. 基于 kiroApiKey 或 refreshToken 的 SHA-256 哈希检测重复
    /// 3. OAuth: 尝试刷新 Token 验证凭据有效性; API Key: 跳过
    /// 4. 分配新 ID（PgSQL 模式由数据库序列生成；测试无 PgSQL 时回退内存 max + 1）
    /// 5. 添加到 entries 列表
    /// 6. 行级持久化到 PgSQL
    ///
    /// # 返回
    /// - `Ok(u64)` - 新凭据 ID
    /// - `Err(_)` - 验证失败或添加失败
    pub async fn add_credential(&self, new_cred: KiroCredentials) -> anyhow::Result<u64> {
        let mut new_cred = new_cred;
        new_cred.canonicalize_auth_method();
        new_cred.normalize_api_key_defaults();
        new_cred.normalize_external_idp_defaults();

        // 1. 基本验证
        if let Some(resource_id) = new_cred.proxy_resource_id {
            let exists = self.proxy_resources.lock().contains_key(&resource_id);
            if !exists {
                anyhow::bail!("代理资源不存在: {}", resource_id);
            }
        }

        if new_cred.is_api_key_credential() {
            let api_key = new_cred
                .kiro_api_key
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("API Key 凭据缺少 kiroApiKey"))?;
            if api_key.trim().is_empty() {
                anyhow::bail!("kiroApiKey 为空");
            }
        } else {
            validate_refresh_token(&new_cred)?;
        }

        // 2. 基于哈希检测重复
        if new_cred.is_api_key_credential() {
            let new_api_key = new_cred
                .kiro_api_key
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("缺少 kiroApiKey"))?;
            let new_api_key_hash = sha256_hex(new_api_key);
            let duplicate_exists = {
                let entries = self.entries.lock();
                entries.iter().any(|entry| {
                    entry
                        .credentials
                        .kiro_api_key
                        .as_deref()
                        .map(sha256_hex)
                        .as_deref()
                        == Some(new_api_key_hash.as_str())
                })
            };
            if duplicate_exists {
                anyhow::bail!("凭据已存在（kiroApiKey 重复）");
            }
        } else {
            let new_refresh_token = new_cred
                .refresh_token
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("缺少 refreshToken"))?;
            let new_refresh_token_hash = sha256_hex(new_refresh_token);
            let duplicate_exists = {
                let entries = self.entries.lock();
                entries.iter().any(|entry| {
                    entry
                        .credentials
                        .refresh_token
                        .as_deref()
                        .map(sha256_hex)
                        .as_deref()
                        == Some(new_refresh_token_hash.as_str())
                })
            };
            if duplicate_exists {
                anyhow::bail!("凭据已存在（refreshToken 重复）");
            }
        }

        // 3. 验证凭据有效性（API Key 无需网络刷新）
        let mut validated_cred = if new_cred.is_api_key_credential() {
            new_cred.clone()
        } else {
            let new_cred_for_proxy = self.resolve_proxy_for_credential(new_cred.clone())?;
            let effective_proxy = new_cred_for_proxy.effective_proxy(self.proxy.as_ref());
            let config = self.runtime_config();
            let admission = RefreshSendAdmission::new(
                None,
                self.auxiliary_concurrency_controller(),
                self.auxiliary_runtime.token_refresh_admission_controller(),
            );
            let client = self
                .refresh_http_client(config.tls_backend, effective_proxy.as_ref())
                .await?;
            refresh_token_with_client(&new_cred_for_proxy, &config, client, Some(admission)).await?
        };

        // 4. 保留用户输入的元数据
        validated_cred.priority = new_cred.priority;
        validated_cred.auth_method = new_cred.auth_method.map(|m| {
            if m.eq_ignore_ascii_case("builder-id") || m.eq_ignore_ascii_case("iam") {
                "idc".to_string()
            } else {
                m
            }
        });
        if new_cred.profile_arn.is_some() {
            validated_cred.profile_arn = new_cred.profile_arn;
        }
        validated_cred.client_id = new_cred.client_id;
        validated_cred.client_secret = new_cred.client_secret;
        validated_cred.token_endpoint = new_cred.token_endpoint;
        validated_cred.issuer_url = new_cred.issuer_url;
        validated_cred.scopes = new_cred.scopes;
        validated_cred.region = new_cred.region;
        validated_cred.auth_region = new_cred.auth_region;
        validated_cred.api_region = new_cred.api_region;
        validated_cred.machine_id = new_cred.machine_id;
        validated_cred.email = new_cred.email;
        validated_cred.proxy_url = new_cred.proxy_url;
        validated_cred.proxy_username = new_cred.proxy_username;
        validated_cred.proxy_password = new_cred.proxy_password;
        validated_cred.proxy_resource_id = new_cred.proxy_resource_id;
        validated_cred.kiro_api_key = new_cred.kiro_api_key;
        validated_cred.endpoint = new_cred.endpoint;
        validated_cred.normalize_api_key_defaults();
        validated_cred.normalize_external_idp_defaults();
        if validated_cred.machine_id.is_none() {
            validated_cred.machine_id = Some(machine_id::generate_from_credentials(
                &validated_cred,
                &self.runtime_config(),
            ));
        }

        let initial_disabled = new_cred.disabled;
        let warmup_remaining = self.runtime_config().credential_warmup_requests;
        let initial_runtime_patch = CredentialRuntimeStatePatch {
            failure_count: Some(0),
            refresh_failure_count: Some(0),
            disabled_reason: if initial_disabled {
                CredentialRuntimeDisabledReasonPatch::Set(
                    DisabledReason::Manual.as_str().to_string(),
                )
            } else {
                CredentialRuntimeDisabledReasonPatch::Clear
            },
            warmup_remaining: Some(warmup_remaining),
            credential_disabled: Some(initial_disabled),
            ..Default::default()
        };
        let (new_id, persisted_runtime) = if let Some(store) = &self.postgres_store {
            validated_cred.disabled = initial_disabled;
            let operation_id = uuid::Uuid::new_v4();
            let (inserted, runtime) = credential_pgsql_sync_with_timeout(
                "原子新增凭据及初始运行态",
                CREDENTIAL_PGSQL_SYNC_TIMEOUT,
                store.insert_credential_with_runtime_patch(
                    &validated_cred,
                    operation_id,
                    &initial_runtime_patch,
                ),
            )
            .await?;
            let id = inserted
                .id
                .ok_or_else(|| anyhow::anyhow!("PgSQL 新增凭据未返回 id"))?;
            validated_cred = inserted;
            (id, Some(runtime))
        } else {
            let id = {
                let entries = self.entries.lock();
                entries.iter().map(|e| e.id).max().unwrap_or(0) + 1
            };
            validated_cred.id = Some(id);
            (id, None)
        };

        let persisted_state = persisted_runtime.as_ref().map(|runtime| &runtime.state);
        let persisted_disabled = persisted_runtime
            .as_ref()
            .map_or(initial_disabled, |runtime| runtime.credential_disabled);
        let persisted_reason = persisted_state
            .and_then(|state| state.disabled_reason.as_deref())
            .and_then(DisabledReason::from_str)
            .or_else(|| persisted_disabled.then_some(DisabledReason::Manual));

        {
            let mut entries = self.entries.lock();
            entries.push(CredentialEntry {
                id: new_id,
                credentials: validated_cred,
                failure_count: persisted_state.map_or(0, |state| state.failure_count),
                refresh_failure_count: persisted_state
                    .map_or(0, |state| state.refresh_failure_count),
                runtime_revision: persisted_state.map_or(0, |state| state.revision),
                runtime_generation: persisted_state.map_or(0, |state| state.generation),
                runtime_persistence_degraded: false,
                runtime_persistence_quarantined: false,
                disabled: persisted_disabled,
                disabled_reason: persisted_reason,
                success_count: 0,
                total_selection_count: 0,
                last_used_at: None,
                cooldown_until: None,
                cooldown_reason: None,
                model_cooldowns: HashMap::new(),
                rate_limit_available_at: None,
                in_flight_requests: 0,
                in_flight_leases: Vec::new(),
                warmup_remaining: persisted_state
                    .map_or(warmup_remaining, |state| state.warmup_remaining),
                health: SchedulerHealthState::default(),
                model_health: HashMap::new(),
                selection_events: VecDeque::new(),
            });
        }

        // 6. 无 PgSQL 的测试模式需要在这里补持久化；PgSQL 模式上面已先写库再更新内存。
        if self.postgres_store.is_none() {
            self.persist_credentials()?;
        }
        self.invalidate_model_capability_cohorts();
        self.publish_credentials_changed("credential_added");
        self.notify_dispatch_state_changed();

        tracing::info!("成功添加凭据 #{}", new_id);
        Ok(new_id)
    }

    /// 设置凭据预热剩余请求数。0 表示关闭预热。
    pub fn set_warmup_remaining(&self, id: u64, warmup_remaining: u32) -> anyhow::Result<()> {
        {
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
        }
        let persisted = self.persist_runtime_patch_value(
            id,
            CredentialRuntimeStatePatch {
                warmup_remaining: Some(warmup_remaining),
                advance_generation: true,
                ..Default::default()
            },
        )?;

        if !persisted {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            entry.warmup_remaining = warmup_remaining;
        }
        self.publish_credentials_changed("credential_warmup_updated");
        self.notify_dispatch_state_changed();
        Ok(())
    }

    pub fn set_credential_proxy(
        &self,
        id: u64,
        proxy_resource_id: Option<u64>,
        proxy_url: Option<String>,
        proxy_username: Option<String>,
        proxy_password: Option<String>,
    ) -> anyhow::Result<()> {
        if let Some(resource_id) = proxy_resource_id {
            let exists = self.proxy_resources.lock().contains_key(&resource_id);
            if !exists {
                anyhow::bail!("代理资源不存在: {}", resource_id);
            }
        }

        self.persist_credential_mutation(id, |credential| {
            credential.proxy_resource_id = proxy_resource_id;
            credential.proxy_url = proxy_url;
            credential.proxy_username = proxy_username;
            credential.proxy_password = proxy_password;
            Ok(())
        })?;
        self.publish_credentials_changed("credential_proxy_updated");
        self.notify_dispatch_state_changed();
        Ok(())
    }

    /// 删除凭据（Admin API）
    ///
    /// # 前置条件
    /// - 凭据必须已禁用（disabled = true）
    ///
    /// # 行为
    /// 1. 验证凭据存在
    /// 2. 验证凭据已禁用
    /// 3. 从 entries 移除
    /// 4. 如果删除的是当前凭据，切换到优先级最高的可用凭据
    /// 5. 如果删除后没有凭据，将 current_id 重置为 0
    /// 6. 软删除 PgSQL 中的凭据行并清理运行态
    ///
    /// # 返回
    /// - `Ok(())` - 删除成功
    /// - `Err(_)` - 凭据不存在、未禁用或持久化失败
    pub fn delete_credential(&self, id: u64) -> anyhow::Result<()> {
        {
            let entries = self.entries.lock();
            let entry = entries
                .iter()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;

            if !entry.disabled {
                anyhow::bail!("只能删除已禁用的凭据（请先禁用凭据 #{}）", id);
            }
        }

        // 行级软删除，并清理该凭据的统计/运行态残留。成功后再更新本进程快照。
        let refresh_state = self.refresh_state_for_credential(id);
        let _refresh_guard = refresh_state
            .gate
            .try_lock()
            .map_err(|_| anyhow::anyhow!("凭据 #{} 正在刷新 Token，请稍后再删除", id))?;
        self.delete_persisted_credential_state(id)?;

        let was_current = {
            let mut entries = self.entries.lock();
            let current_id = *self.current_id.lock();
            let was_current = current_id == id;
            entries.retain(|e| e.id != id);
            was_current
        };
        self.clear_pending_persistence_for_credential(id);

        // 如果删除的是当前凭据，切换到优先级最高的可用凭据
        if was_current {
            self.select_highest_priority();
        }
        self.unbind_sessions_for_credential(id);
        self.clear_scheduler_state_for_credential(id, true);
        self.remove_refresh_state_for_credential(id);
        self.invalidate_model_capability_cohorts();
        self.notify_dispatch_state_changed();

        // 如果删除后没有任何凭据，将 current_id 重置为 0（与初始化行为保持一致）
        {
            let entries = self.entries.lock();
            if entries.is_empty() {
                let mut current_id = self.current_id.lock();
                *current_id = 0;
                tracing::info!("所有凭据已删除，current_id 已重置为 0");
            }
        }

        self.publish_credentials_changed("credential_deleted");

        tracing::info!("已删除凭据 #{}", id);
        Ok(())
    }

    /// 强制刷新指定凭据的 Token（Admin API）
    ///
    /// 无条件调用上游 API 重新获取 access token，不检查是否过期。
    /// 适用于排查问题、Token 异常但未过期、主动更新凭据状态等场景。
    pub async fn force_refresh_token_for(&self, id: u64) -> anyhow::Result<()> {
        self.force_refresh_token_for_with_budgets(id, TokenRefreshBudgets::default())
            .await
    }

    fn automatic_recovery_context_is_current(
        credentials: &KiroCredentials,
        expected_access_token: &str,
        expected_storage_revision: u64,
    ) -> anyhow::Result<bool> {
        if credentials.storage_revision < expected_storage_revision {
            return Err(RefreshFailure::new(
                RefreshFailureStage::Coordination,
                RefreshFailureKind::Coordination,
                None,
                None,
                false,
            )
            .into());
        }
        Ok(credentials.access_token.as_deref() == Some(expected_access_token))
    }

    /// Conditionally recover an access token rejected by an inference or MCP request.
    ///
    /// Unlike the Admin force-refresh API, this request-path operation is tied to the exact
    /// access-token generation observed by the failed call and consumes that request's auxiliary
    /// attempt budget. Concurrent callers share the same typed positive or negative result.
    pub(crate) async fn recover_invalid_access_token_for(
        &self,
        id: u64,
        expected_access_token: &str,
        expected_storage_revision: u64,
        auxiliary_attempt_budget: Arc<AuxiliaryAttemptBudget>,
    ) -> anyhow::Result<AutomaticTokenRecoveryOutcome> {
        let initial_credentials = {
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|entry| entry.id == id)
                .map(|entry| entry.credentials.clone())
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?
        };
        if !Self::automatic_recovery_context_is_current(
            &initial_credentials,
            expected_access_token,
            expected_storage_revision,
        )? {
            return Ok(AutomaticTokenRecoveryOutcome::CredentialChanged);
        }
        auxiliary_attempt_budget.ensure_available(AuxiliaryAttemptKind::TokenRefresh)?;

        let deadlines = TokenRefreshBudgets::default().deadlines()?;
        let refresh_state = self.refresh_state_for_credential(id);
        let _guard =
            acquire_refresh_lock_until(&refresh_state.gate, deadlines.coordination, id).await?;
        let refresh_reset_generation = refresh_state.generation();

        // The failed request can wait behind another refresh or an Admin credential update. Only
        // the exact token generation that produced the upstream auth error may initiate a send.
        let locked_credentials = {
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|entry| entry.id == id)
                .map(|entry| entry.credentials.clone())
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?
        };
        if !Self::automatic_recovery_context_is_current(
            &locked_credentials,
            expected_access_token,
            expected_storage_revision,
        )? {
            refresh_state.clear_failure();
            return Ok(AutomaticTokenRecoveryOutcome::CredentialChanged);
        }
        auxiliary_attempt_budget.ensure_available(AuxiliaryAttemptKind::TokenRefresh)?;

        let credentials_for_proxy = self
            .resolve_proxy_for_credential(locked_credentials.clone())
            .map_err(|_| {
                anyhow::Error::new(RefreshFailure::new(
                    RefreshFailureStage::Validation,
                    RefreshFailureKind::InvalidConfiguration,
                    None,
                    None,
                    false,
                ))
            })?;
        let effective_proxy = credentials_for_proxy.effective_proxy(self.proxy.as_ref());
        let refresh_identity = {
            let config = self.config.lock();
            RefreshAttemptIdentity::from_automatic_recovery_request(
                &credentials_for_proxy,
                expected_access_token,
                &config,
                effective_proxy.as_ref(),
            )
        };
        if let Some(failure) = refresh_state.replay_failure(&refresh_identity, Instant::now(), true)
        {
            tracing::debug!(
                credential_id = id,
                refresh_stage = failure.stage.as_str(),
                refresh_kind = failure.kind.as_str(),
                "automatic auth recovery reused the current typed failure wave"
            );
            if !failure.shared_failure_wave && refresh_failure_requires_health_action(&failure) {
                self.apply_refresh_health_action(id, &failure, None, deadlines.total)
                    .await?;
            }
            return Err(failure.into_shared_failure_wave().into());
        }

        let mut redis_refresh_lease = None;
        let send_started = Arc::new(AtomicBool::new(false));
        let persisted_storage_revision = Arc::new(AtomicU64::new(0));
        let mut redis_lease_drop_guard = None;
        let mut refresh_source = locked_credentials;
        let mut refresh_source_for_proxy = credentials_for_proxy;
        let mut refresh_effective_proxy = effective_proxy;
        if self.redis_store.is_some() {
            match self
                .begin_distributed_refresh_until(id, refresh_identity, true, deadlines.coordination)
                .await?
            {
                DistributedRefreshDecision::Replay(failure) => return Err(failure.into()),
                DistributedRefreshDecision::Succeeded {
                    generation,
                    storage_revision,
                } => {
                    let latest = self
                        .reload_credential_for_refresh_until(id, deadlines.coordination)
                        .await?;
                    let token_changed =
                        latest.access_token.as_deref() != Some(expected_access_token);
                    if storage_revision == 0
                        || latest.storage_revision < storage_revision
                        || is_token_expired(&latest)
                        || !token_changed
                    {
                        tracing::warn!(
                            credential_id = id,
                            refresh_generation = generation,
                            announced_storage_revision = storage_revision,
                            authoritative_storage_revision = latest.storage_revision,
                            token_changed,
                            "Automatic recovery Redis success failed PostgreSQL authority checks"
                        );
                        return Err(RefreshFailure::new(
                            RefreshFailureStage::Coordination,
                            RefreshFailureKind::Coordination,
                            None,
                            None,
                            false,
                        )
                        .into());
                    }
                    {
                        let mut entries = self.entries.lock();
                        let entry = entries
                            .iter_mut()
                            .find(|entry| entry.id == id)
                            .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
                        Self::merge_refresh_fields(&mut entry.credentials, &latest);
                    }
                    refresh_state.clear_failure();
                    return Ok(AutomaticTokenRecoveryOutcome::CredentialChanged);
                }
                DistributedRefreshDecision::Leader(lease) => {
                    redis_lease_drop_guard = self.redis_store.as_ref().map(|redis| {
                        RedisRefreshLeaseDropGuard::new(
                            redis.clone(),
                            self.postgres_store.clone(),
                            lease.clone(),
                            send_started.clone(),
                            persisted_storage_revision.clone(),
                            Some(expected_access_token.to_string()),
                        )
                    });
                    let authoritative = self
                        .reload_credential_for_refresh_until(id, deadlines.coordination)
                        .await?;
                    if !Self::automatic_recovery_context_is_current(
                        &authoritative,
                        expected_access_token,
                        expected_storage_revision,
                    )? {
                        let failure = RefreshFailure::new(
                            RefreshFailureStage::Coordination,
                            RefreshFailureKind::Coordination,
                            None,
                            None,
                            false,
                        );
                        let failure = self
                            .complete_distributed_refresh_failure_until(
                                &lease,
                                &failure,
                                false,
                                deadlines.total,
                            )
                            .await?;
                        if let Some(guard) = redis_lease_drop_guard.as_mut() {
                            guard.disarm();
                        }
                        return Err(failure.into());
                    }
                    let authoritative_for_proxy = self
                        .resolve_proxy_for_credential(authoritative.clone())
                        .map_err(|_| {
                            anyhow::Error::new(RefreshFailure::new(
                                RefreshFailureStage::Validation,
                                RefreshFailureKind::InvalidConfiguration,
                                None,
                                None,
                                false,
                            ))
                        })?;
                    let authoritative_proxy =
                        authoritative_for_proxy.effective_proxy(self.proxy.as_ref());
                    let authoritative_identity = {
                        let config = self.config.lock();
                        RefreshAttemptIdentity::from_automatic_recovery_request(
                            &authoritative_for_proxy,
                            expected_access_token,
                            &config,
                            authoritative_proxy.as_ref(),
                        )
                    };
                    if authoritative_identity.0 != lease.identity {
                        let failure = RefreshFailure::new(
                            RefreshFailureStage::Coordination,
                            RefreshFailureKind::Coordination,
                            None,
                            None,
                            false,
                        );
                        let failure = self
                            .complete_distributed_refresh_failure_until(
                                &lease,
                                &failure,
                                false,
                                deadlines.total,
                            )
                            .await?;
                        if let Some(guard) = redis_lease_drop_guard.as_mut() {
                            guard.disarm();
                        }
                        return Err(failure.into());
                    }
                    refresh_source = authoritative;
                    refresh_source_for_proxy = authoritative_for_proxy;
                    refresh_effective_proxy = authoritative_proxy;
                    redis_refresh_lease = Some(lease);
                }
            }
        }

        Self::ensure_refresh_state_generation(&refresh_state, refresh_reset_generation, false)?;
        auxiliary_attempt_budget.ensure_available(AuxiliaryAttemptKind::TokenRefresh)?;
        let config = self.runtime_config();
        let admission = RefreshSendAdmission::new_with_send_started_marker(
            Some(auxiliary_attempt_budget),
            self.auxiliary_concurrency_controller(),
            self.auxiliary_runtime.token_refresh_admission_controller(),
            send_started.clone(),
        );
        let mut cancellation_guard = RefreshFailureWaveDropGuard::new(
            refresh_state.clone(),
            refresh_identity,
            refresh_reset_generation,
            send_started.clone(),
        );
        let client = self
            .refresh_http_client(config.tls_backend, refresh_effective_proxy.as_ref())
            .await
            .map_err(|_| {
                anyhow::Error::new(RefreshFailure::new(
                    RefreshFailureStage::Internal,
                    RefreshFailureKind::Internal,
                    None,
                    None,
                    false,
                ))
            })?;
        let refresh_result = refresh_token_until(
            &refresh_source_for_proxy,
            &config,
            client,
            Some(admission),
            deadlines.work,
            "automatic auth recovery upstream stage",
        )
        .await;
        if redis_refresh_lease.is_some() {
            cancellation_guard.disarm();
        }

        let mut new_credentials = match refresh_result {
            Ok(credentials) => credentials,
            Err(error) => {
                if let Some(lease) = redis_refresh_lease.as_ref() {
                    let synthetic_failure = RefreshFailure::new(
                        RefreshFailureStage::Coordination,
                        RefreshFailureKind::Coordination,
                        None,
                        None,
                        false,
                    );
                    let failure = error
                        .downcast_ref::<RefreshFailure>()
                        .unwrap_or(&synthetic_failure);
                    let shared = self
                        .complete_distributed_refresh_failure_until(
                            lease,
                            failure,
                            true,
                            deadlines.total,
                        )
                        .await?;
                    if let Some(guard) = redis_lease_drop_guard.as_mut() {
                        guard.disarm();
                    }
                    if error.downcast_ref::<RefreshFailure>().is_some() {
                        return Err(shared.into());
                    }
                } else if let Some(failure) = error.downcast_ref::<RefreshFailure>() {
                    refresh_state.record_failure(refresh_identity, failure, Instant::now(), true);
                    cancellation_guard.disarm();
                    if refresh_failure_requires_health_action(failure) {
                        self.apply_refresh_health_action(id, failure, None, deadlines.total)
                            .await?;
                        return Err(failure.clone().into_shared_failure_wave().into());
                    }
                }
                if !send_started.load(Ordering::Acquire) {
                    cancellation_guard.disarm();
                }
                return Err(error);
            }
        };
        Self::ensure_refresh_state_generation(&refresh_state, refresh_reset_generation, true)?;
        new_credentials.proxy_url = refresh_source.proxy_url.clone();
        new_credentials.proxy_username = refresh_source.proxy_username.clone();
        new_credentials.proxy_password = refresh_source.proxy_password.clone();
        new_credentials.proxy_resource_id = refresh_source.proxy_resource_id;
        if is_token_expired(&new_credentials)
            || new_credentials.access_token.as_deref() == Some(expected_access_token)
        {
            let failure = RefreshFailure::new(
                RefreshFailureStage::ResponseValidate,
                RefreshFailureKind::MissingToken,
                Some(200),
                None,
                true,
            );
            if let Some(lease) = redis_refresh_lease.as_ref() {
                let shared = self
                    .complete_distributed_refresh_failure_until(
                        lease,
                        &failure,
                        false,
                        deadlines.total,
                    )
                    .await?;
                if let Some(guard) = redis_lease_drop_guard.as_mut() {
                    guard.disarm();
                }
                return Err(shared.into());
            }
            refresh_state.record_failure(refresh_identity, &failure, Instant::now(), true);
            cancellation_guard.disarm();
            return Err(failure.into());
        }
        new_credentials = self
            .persist_refreshed_credential_fields(
                id,
                &refresh_source,
                new_credentials,
                true,
                Some(expected_access_token),
                deadlines.work,
                deadlines.total,
            )
            .await
            .map_err(|_| {
                anyhow::Error::new(RefreshFailure::new(
                    RefreshFailureStage::Persistence,
                    RefreshFailureKind::Persistence,
                    None,
                    None,
                    true,
                ))
            })?;
        persisted_storage_revision.store(new_credentials.storage_revision, Ordering::Release);
        if new_credentials.access_token.as_deref() == Some(expected_access_token) {
            persisted_storage_revision.store(0, Ordering::Release);
            let failure = RefreshFailure::new(
                RefreshFailureStage::Persistence,
                RefreshFailureKind::Persistence,
                None,
                None,
                true,
            );
            if let Some(lease) = redis_refresh_lease.as_ref() {
                let shared = self
                    .complete_distributed_refresh_failure_until(
                        lease,
                        &failure,
                        false,
                        deadlines.total,
                    )
                    .await?;
                if let Some(guard) = redis_lease_drop_guard.as_mut() {
                    guard.disarm();
                }
                return Err(shared.into());
            }
            refresh_state.record_failure(refresh_identity, &failure, Instant::now(), true);
            cancellation_guard.disarm();
            return Err(failure.into());
        }
        Self::ensure_refresh_state_generation(&refresh_state, refresh_reset_generation, true)?;
        if let Some(lease) = redis_refresh_lease.as_ref() {
            self.complete_or_cancel_distributed_refresh_success_until(
                lease,
                new_credentials.storage_revision,
                deadlines.total,
            )
            .await?;
            if let Some(guard) = redis_lease_drop_guard.as_mut() {
                guard.disarm();
            }
        }
        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|entry| entry.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            Self::merge_refresh_fields(&mut entry.credentials, &new_credentials);
        }
        if let Some(redis) = &self.redis_store {
            let payload = serde_json::json!({
                "kind": "credentials_changed",
                "reason": "token_refreshed",
                "changedAt": Utc::now().to_rfc3339(),
            })
            .to_string();
            let _ = tokio::time::timeout_at(
                deadlines.total,
                redis.publish_credentials_changed(payload),
            )
            .await;
        }
        cancellation_guard.disarm();
        refresh_state.clear_failure();
        Ok(AutomaticTokenRecoveryOutcome::Refreshed)
    }

    async fn force_refresh_token_for_with_budgets(
        &self,
        id: u64,
        budgets: TokenRefreshBudgets,
    ) -> anyhow::Result<()> {
        if !self.entries.lock().iter().any(|entry| entry.id == id) {
            anyhow::bail!("凭据不存在: {}", id);
        }
        let deadlines = budgets.deadlines()?;
        // 获取该凭据的刷新锁，防止同一账号并发刷新。
        let refresh_state = self.refresh_state_for_credential(id);
        let _guard =
            acquire_refresh_lock_until(&refresh_state.gate, deadlines.coordination, id).await?;
        let mut redis_refresh_lock: Option<RedisRefreshLockGuard> = None;
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            while tokio::time::Instant::now() < deadlines.coordination {
                let remaining = deadlines
                    .coordination
                    .saturating_duration_since(tokio::time::Instant::now());
                match tokio::time::timeout(
                    remaining.min(REFRESH_REDIS_LOCK_OP_TIMEOUT),
                    redis.acquire_refresh_lock(id, TOKEN_REFRESH_REDIS_LOCK_TTL_SECS),
                )
                .await
                {
                    Ok(Ok(Some(lock_token))) => {
                        redis_refresh_lock =
                            Some(RedisRefreshLockGuard::new(redis.clone(), id, lock_token));
                        break;
                    }
                    Ok(Ok(None)) => {
                        let now = tokio::time::Instant::now();
                        tokio::time::sleep_until(
                            (now + StdDuration::from_millis(500)).min(deadlines.coordination),
                        )
                        .await;
                    }
                    Ok(Err(_)) => {
                        return Err(RefreshFailure::new(
                            RefreshFailureStage::Coordination,
                            RefreshFailureKind::Coordination,
                            None,
                            None,
                            false,
                        )
                        .into());
                    }
                    Err(_) => {
                        return Err(RefreshFailure::new(
                            RefreshFailureStage::Coordination,
                            RefreshFailureKind::Timeout,
                            None,
                            None,
                            false,
                        )
                        .into());
                    }
                }
            }
            if redis_refresh_lock.is_none() {
                return Err(RefreshFailure::new(
                    RefreshFailureStage::Coordination,
                    RefreshFailureKind::Timeout,
                    None,
                    None,
                    false,
                )
                .into());
            }
        }

        let refresh_and_publish_result = async {
            let credentials = {
                let entries = self.entries.lock();
                entries
                    .iter()
                    .find(|entry| entry.id == id)
                    .map(|entry| entry.credentials.clone())
                    .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?
            };
            // 无条件调用 refresh_token。
            let mut new_creds = match self.resolve_proxy_for_credential(credentials.clone()) {
                Ok(credentials_for_proxy) => {
                    let effective_proxy =
                        credentials_for_proxy.effective_proxy(self.proxy.as_ref());
                    let config = self.runtime_config();
                    let admission = RefreshSendAdmission::new(
                        None,
                        self.auxiliary_concurrency_controller(),
                        self.auxiliary_runtime.token_refresh_admission_controller(),
                    );
                    let client = self
                        .refresh_http_client(config.tls_backend, effective_proxy.as_ref())
                        .await
                        .map_err(|_| {
                            anyhow::Error::new(RefreshFailure::new(
                                RefreshFailureStage::Internal,
                                RefreshFailureKind::Internal,
                                None,
                                None,
                                false,
                            ))
                        })?;
                    refresh_token_until(
                        &credentials_for_proxy,
                        &config,
                        client,
                        Some(admission),
                        deadlines.work,
                        "强制刷新 Token 上游阶段",
                    )
                    .await?
                }
                Err(_) => {
                    return Err(RefreshFailure::new(
                        RefreshFailureStage::Validation,
                        RefreshFailureKind::InvalidConfiguration,
                        None,
                        None,
                        false,
                    )
                    .into());
                }
            };
            new_creds.proxy_url = credentials.proxy_url.clone();
            new_creds.proxy_username = credentials.proxy_username.clone();
            new_creds.proxy_password = credentials.proxy_password.clone();
            new_creds.proxy_resource_id = credentials.proxy_resource_id;

            // PgSQL 是多实例的权威凭据存储。字段级 CAS 成功后才能更新本机并通知其他实例。
            new_creds = self
                .persist_refreshed_credential_fields(
                    id,
                    &credentials,
                    new_creds,
                    false,
                    None,
                    deadlines.work,
                    deadlines.total,
                )
                .await
                .map_err(|_| {
                    anyhow::Error::new(RefreshFailure::new(
                        RefreshFailureStage::Persistence,
                        RefreshFailureKind::Persistence,
                        None,
                        None,
                        true,
                    ))
                })?;

            let expected_generation = {
                let mut entries = self.entries.lock();
                let entry = entries
                    .iter_mut()
                    .find(|entry| entry.id == id)
                    .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
                Self::merge_refresh_fields(&mut entry.credentials, &new_creds);
                entry.refresh_failure_count = 0;
                entry.runtime_generation
            };
            self.persist_runtime_patch_best_effort_until(
                id,
                CredentialRuntimeStatePatch {
                    refresh_failure_count: Some(0),
                    expected_generation: Some(expected_generation),
                    ..Default::default()
                },
                deadlines.total,
            )
            .await;

            if let Some(store) = &self.postgres_store {
                let store = store.clone();
                spawn_best_effort_storage_task("记录强制刷新凭据事件到 PgSQL", async move {
                    store
                        .record_credential_event(
                            Some(id),
                            "credentials_changed",
                            Some("credential_force_refreshed"),
                            serde_json::json!({ "reason": "credential_force_refreshed" }),
                        )
                        .await
                });
            }
            if let Some(redis) = &self.redis_store {
                let payload = serde_json::json!({
                    "kind": "credentials_changed",
                    "reason": "credential_force_refreshed",
                    "changedAt": Utc::now().to_rfc3339(),
                })
                .to_string();
                match tokio::time::timeout_at(
                    deadlines.total,
                    redis.publish_credentials_changed(payload),
                )
                .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(err)) => tracing::warn!(
                        credential_id = id,
                        "发布强制刷新凭据通知失败，其他实例将通过定时同步恢复: {}",
                        err
                    ),
                    Err(_) => tracing::warn!(
                        credential_id = id,
                        "发布强制刷新凭据通知超过总期限，其他实例将通过定时同步恢复"
                    ),
                }
            }
            Ok::<(), anyhow::Error>(())
        }
        .await;

        // 所有成功步骤和任一失败路径都在释放分布式锁后再返回。
        if let Some(redis_refresh_lock) = redis_refresh_lock {
            redis_refresh_lock.release().await;
        }
        refresh_and_publish_result?;
        refresh_state.clear_failure();

        tracing::info!("凭据 #{} Token 已强制刷新", id);
        Ok(())
    }

    /// 获取负载均衡模式（Admin API）
    pub fn get_load_balancing_mode(&self) -> String {
        self.load_balancing_mode.lock().clone()
    }

    fn persist_load_balancing_mode(&self, mode: &str) -> anyhow::Result<()> {
        let mut config = self.runtime_config();
        config.load_balancing_mode = mode.to_string();
        config.set_config_path_for_runtime(None);
        let mut saved_version: Option<i64> = None;
        if let Some(store) = &self.postgres_store {
            let store = store.clone();
            saved_version = Some(block_on_credential_pgsql(
                "持久化负载均衡模式到 PgSQL",
                async move { store.save_runtime_config_returning_version(&config).await },
            )?);
        }
        self.publish_runtime_config_changed(saved_version, "load_balancing_mode_updated");
        Ok(())
    }

    /// 设置负载均衡模式（Admin API）
    pub fn set_load_balancing_mode(&self, mode: String) -> anyhow::Result<()> {
        // 验证模式值
        if !matches!(
            mode.as_str(),
            "priority" | "balanced" | "health_balanced" | "weighted_least_inflight"
        ) {
            anyhow::bail!("无效的负载均衡模式: {}", mode);
        }

        let previous_mode = self.get_load_balancing_mode();
        if previous_mode == mode {
            return Ok(());
        }

        *self.load_balancing_mode.lock() = mode.clone();

        if let Err(err) = self.persist_load_balancing_mode(&mode) {
            *self.load_balancing_mode.lock() = previous_mode;
            return Err(err);
        }
        self.update_load_balancing_mode_in_config(&mode);
        self.notify_dispatch_state_changed();

        tracing::info!("负载均衡模式已设置为: {}", mode);
        Ok(())
    }
}

impl Drop for MultiTokenManager {
    fn drop(&mut self) {
        if !self.refresh_stats_dirty_from_pending() {
            tracing::warn!("MultiTokenManager 释放时仍有未刷盘统计；应先关闭 stats flush worker");
        }
    }
}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;
