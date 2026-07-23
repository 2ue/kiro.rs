use std::collections::{HashMap, HashSet};
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration as ChronoDuration, Timelike, Utc};
use redis::aio::{ConnectionManager, PubSub};
use redis::{AsyncCommands, ToRedisArgs};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use crate::anthropic::usage::{
    REALTIME_USAGE_WINDOW_SECS, UsageAggregate, UsageDashboardSeries, UsageDashboardTop,
    UsageDashboardWindowSpec, UsageExternalPoolBillingSummary, UsageRealtimeStats, UsageRecord,
    UsageRecordQuery, UsageRecordStatus, UsageRecordsPageResult, UsageRouteKind, UsageSeriesPoint,
    UsageSource, UsageSummary, UsageTopAggregate, usage_dashboard_daily_windows,
    usage_dashboard_hourly_windows, usage_dashboard_timezone,
};
#[cfg(test)]
use crate::anthropic::usage::{
    UsageBreakdownItem, UsageDashboardResponse, UsageDashboardSummary, UsageDashboardWindow,
    UsageExternalPoolBillingByPool, usage_dashboard_windows,
};
use crate::model::config::{
    Config, MAX_TOKEN_REFRESH_BURST, MAX_TOKEN_REFRESH_MAX_RPM, MIN_TOKEN_REFRESH_BURST,
    MIN_TOKEN_REFRESH_MAX_RPM, RedisConfig,
};

const REDIS_PATTERN_DELETE_SCAN_COUNT: usize = 128;
const REDIS_PATTERN_DELETE_COMMAND_KEY_LIMIT: usize = 64;
const REDIS_PATTERN_DELETE_MAX_PASSES: usize = 8;
const EXTERNAL_POOL_PENDING_LEASE_TOMBSTONE_TTL_MILLIS: i64 = 5 * 60 * 1_000;
const EXTERNAL_POOL_COORDINATOR_EPOCH_KEY: &str = "external_pool:coordinator:coordination_epoch";
const EXTERNAL_POOL_COORDINATOR_RECOVERY_KEY: &str = "external_pool:coordinator:recovery_until";
const EXTERNAL_POOL_DATA_GENERATION_KEY: &str = "external_pool:data:generation";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RedisPatternDeleteStats {
    pub deleted_keys: usize,
    pub scan_calls: usize,
    pub delete_commands: usize,
    pub max_command_keys: usize,
    pub scan_passes: usize,
    pub used_del_fallback: bool,
    pub cancelled: bool,
    pub pass_limit_reached: bool,
}

impl RedisPatternDeleteStats {
    fn merge(&mut self, other: Self) {
        self.deleted_keys = self.deleted_keys.saturating_add(other.deleted_keys);
        self.scan_calls = self.scan_calls.saturating_add(other.scan_calls);
        self.delete_commands = self.delete_commands.saturating_add(other.delete_commands);
        self.max_command_keys = self.max_command_keys.max(other.max_command_keys);
        self.scan_passes = self.scan_passes.saturating_add(other.scan_passes);
        self.used_del_fallback |= other.used_del_fallback;
        self.cancelled |= other.cancelled;
        self.pass_limit_reached |= other.pass_limit_reached;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SchedulerSessionBinding {
    pub credential_id: u64,
    pub last_used_at: DateTime<Utc>,
    pub soft_failure_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SchedulerCooldownState {
    pub until_ms: i64,
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SchedulerHealthState {
    pub transient_failure_streak: u32,
    pub recent_error_rate: f64,
    pub latency_ewma_ms: Option<f64>,
    pub last_error_kind: Option<String>,
    pub last_error_reason: Option<String>,
    pub last_error_at_ms: Option<i64>,
    pub probation_until_ms: Option<i64>,
    pub selection_count: u64,
    pub recent_selection_count_10s: u32,
    pub recent_selection_count_60s: u32,
    pub recent_selection_count_5m: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerInFlightLease {
    pub id: u64,
    pub acquired_at_ms: i64,
    pub last_seen_at_ms: i64,
    pub kind: String,
    pub weight_units: u32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SchedulerCredentialState {
    pub cooldown: Option<SchedulerCooldownState>,
    pub health: SchedulerHealthState,
    pub model_states: Vec<SchedulerModelState>,
    pub rate_limit_available_at_ms: Option<i64>,
    pub in_flight_leases: Vec<SchedulerInFlightLease>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SchedulerModelState {
    pub model: String,
    pub cooldown: Option<SchedulerCooldownState>,
    pub health: SchedulerHealthState,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SchedulerGlobalCapacityState {
    pub in_flight_requests: u32,
    pub queued_requests: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SchedulerSelectionReservation {
    Recorded {
        health: SchedulerHealthState,
        rate_limit_available_at_ms: Option<i64>,
    },
    RateLimited {
        retry_after_ms: u64,
        rate_limit_available_at_ms: i64,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SchedulerSelectionReservationWire {
    status: String,
    #[serde(default)]
    health: Option<SchedulerHealthState>,
    #[serde(default)]
    retry_after_ms: Option<u64>,
    #[serde(default)]
    rate_limit_available_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RedisRefreshFailureStage {
    Validation,
    RequestSend,
    ResponseHeaders,
    ResponseBody,
    ResponseStatus,
    ResponseDecode,
    ResponseValidate,
    Coordination,
    Persistence,
    Internal,
}

impl RedisRefreshFailureStage {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Validation => "validation",
            Self::RequestSend => "request_send",
            Self::ResponseHeaders => "response_headers",
            Self::ResponseBody => "response_body",
            Self::ResponseStatus => "response_status",
            Self::ResponseDecode => "response_decode",
            Self::ResponseValidate => "response_validate",
            Self::Coordination => "coordination",
            Self::Persistence => "persistence",
            Self::Internal => "internal",
        }
    }

    fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "validation" => Ok(Self::Validation),
            "request_send" => Ok(Self::RequestSend),
            "response_headers" => Ok(Self::ResponseHeaders),
            "response_body" => Ok(Self::ResponseBody),
            "response_status" => Ok(Self::ResponseStatus),
            "response_decode" => Ok(Self::ResponseDecode),
            "response_validate" => Ok(Self::ResponseValidate),
            "coordination" => Ok(Self::Coordination),
            "persistence" => Ok(Self::Persistence),
            "internal" => Ok(Self::Internal),
            _ => anyhow::bail!("Redis returned an unknown token refresh failure stage"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RedisRefreshFailureKind {
    InvalidGrant,
    CredentialAuth,
    RateLimited,
    UpstreamUnavailable,
    Network,
    Timeout,
    Protocol,
    Oversize,
    MalformedResponse,
    MissingToken,
    InvalidConfiguration,
    Coordination,
    Persistence,
    Internal,
}

impl RedisRefreshFailureKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::InvalidGrant => "invalid_grant",
            Self::CredentialAuth => "credential_auth",
            Self::RateLimited => "rate_limited",
            Self::UpstreamUnavailable => "upstream_unavailable",
            Self::Network => "network",
            Self::Timeout => "timeout",
            Self::Protocol => "protocol",
            Self::Oversize => "oversize",
            Self::MalformedResponse => "malformed_response",
            Self::MissingToken => "missing_token",
            Self::InvalidConfiguration => "invalid_configuration",
            Self::Coordination => "coordination",
            Self::Persistence => "persistence",
            Self::Internal => "internal",
        }
    }

    fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "invalid_grant" => Ok(Self::InvalidGrant),
            "credential_auth" => Ok(Self::CredentialAuth),
            "rate_limited" => Ok(Self::RateLimited),
            "upstream_unavailable" => Ok(Self::UpstreamUnavailable),
            "network" => Ok(Self::Network),
            "timeout" => Ok(Self::Timeout),
            "protocol" => Ok(Self::Protocol),
            "oversize" => Ok(Self::Oversize),
            "malformed_response" => Ok(Self::MalformedResponse),
            "missing_token" => Ok(Self::MissingToken),
            "invalid_configuration" => Ok(Self::InvalidConfiguration),
            "coordination" => Ok(Self::Coordination),
            "persistence" => Ok(Self::Persistence),
            "internal" => Ok(Self::Internal),
            _ => anyhow::bail!("Redis returned an unknown token refresh failure kind"),
        }
    }
}

/// Secret-free closed representation of one refresh failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RedisRefreshFailure {
    pub(crate) stage: RedisRefreshFailureStage,
    pub(crate) kind: RedisRefreshFailureKind,
    pub(crate) status: Option<u16>,
    pub(crate) retry_after: Option<StdDuration>,
    pub(crate) send_committed: bool,
    pub(crate) health_action_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RedisRefreshFailureOutcome {
    pub(crate) generation: u64,
    pub(crate) failure: RedisRefreshFailure,
    pub(crate) retry_at_epoch_ms: i64,
    pub(crate) consecutive_failures: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RedisRefreshLease {
    pub(crate) credential_id: u64,
    pub(crate) generation: u64,
    pub(crate) owner: String,
    pub(crate) identity: [u8; 32],
    pub(crate) prior_failure_streak: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RedisRefreshHealthClaim {
    pub(crate) generation: u64,
    pub(crate) token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RedisRefreshBegin {
    Leader(RedisRefreshLease),
    Wait {
        generation: Option<u64>,
        poll_after: StdDuration,
    },
    Replay {
        outcome: RedisRefreshFailureOutcome,
        health_claim: Option<RedisRefreshHealthClaim>,
    },
    Succeeded {
        generation: u64,
        storage_revision: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RedisRefreshFailureCommit {
    pub(crate) outcome: RedisRefreshFailureOutcome,
    pub(crate) health_claim: Option<RedisRefreshHealthClaim>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TokenRefreshBucketDecision {
    pub(crate) admitted: bool,
    pub(crate) retry_after: Option<StdDuration>,
    pub(crate) remaining_milli_tokens: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExternalPoolCapacityState {
    pub pool_in_flight_requests: u32,
    pub global_in_flight_requests: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExternalPoolCoordinatorSnapshot {
    pub capacity: ExternalPoolCapacityState,
    pub cooldown_values: Vec<Option<String>>,
    pub cooldown_ttls: Vec<Option<StdDuration>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalPoolCoordinatorSnapshotRequest {
    pub pool_id: u64,
    pub cooldown_keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalPoolLeaseAcquireResult {
    Acquired {
        lease_id: String,
        pool_in_flight_requests: u32,
        global_in_flight_requests: u32,
    },
    Released,
    PoolCooldown {
        remaining: Option<StdDuration>,
    },
    ModelCooldown {
        remaining: Option<StdDuration>,
    },
    PoolCapacityFull {
        in_flight_requests: u32,
    },
    GlobalCapacityFull {
        in_flight_requests: u32,
    },
    CoordinatorEpochMismatch {
        coordination_epoch: String,
    },
    CoordinatorRecovering {
        coordination_epoch: String,
        remaining: StdDuration,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalPoolLeaseReleaseRequest {
    pub pool_id: u64,
    pub lease_id: String,
    pub pending: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExternalPoolLeaseReleaseResult {
    pub completed: bool,
    pub removed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalPoolCoordinatorGuardState {
    Ready {
        coordination_epoch: String,
    },
    EpochMismatch {
        coordination_epoch: String,
    },
    Recovering {
        coordination_epoch: String,
        remaining: StdDuration,
    },
}

#[derive(Debug, Clone)]
pub struct ExternalPoolCoordinatorGuardError {
    pub state: ExternalPoolCoordinatorGuardState,
}

impl std::fmt::Display for ExternalPoolCoordinatorGuardError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.state {
            ExternalPoolCoordinatorGuardState::Ready { .. } => {
                formatter.write_str("external pool coordinator guard is ready")
            }
            ExternalPoolCoordinatorGuardState::EpochMismatch { .. } => {
                formatter.write_str("external pool coordinator Redis epoch requires reconciliation")
            }
            ExternalPoolCoordinatorGuardState::Recovering { remaining, .. } => write!(
                formatter,
                "external pool coordinator is recovering for {}ms",
                remaining.as_millis().max(1)
            ),
        }
    }
}

impl std::error::Error for ExternalPoolCoordinatorGuardError {}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalPoolCircuitState {
    pub open: bool,
    pub open_until_ms: Option<i64>,
    pub reason: Option<String>,
    pub recent_failures: u32,
    pub distinct_credentials: u32,
}

#[cfg(test)]
#[derive(Debug, Clone)]
struct RedisExternalPoolIndexItem {
    id: String,
    label: String,
}

#[derive(Debug, Default)]
struct DashboardBucketCache {
    buckets: HashMap<String, HashMap<String, String>>,
}

impl DashboardBucketCache {
    fn sum_bucket(
        &self,
        dimension: &str,
        key: &str,
        spec: &UsageDashboardWindowSpec,
    ) -> HashMap<String, String> {
        let epochs = usage_dashboard_hour_epochs(spec.from, spec.to);
        sum_usage_hash_refs(epochs.iter().filter_map(|epoch| {
            self.buckets
                .get(&usage_dashboard_bucket_key(dimension, key, *epoch))
        }))
    }

    #[cfg(test)]
    fn high_cache_requests(
        &self,
        spec: &UsageDashboardWindowSpec,
        high_cache_threshold: i32,
    ) -> usize {
        usage_dashboard_hour_epochs(spec.from, spec.to)
            .iter()
            .filter_map(|epoch| {
                self.buckets
                    .get(&usage_dashboard_cache_read_bucket_key(*epoch))
            })
            .flat_map(|bucket| bucket.iter())
            .filter_map(|(tokens, requests)| {
                let tokens = tokens.parse::<i32>().ok()?;
                let requests = requests.parse::<usize>().ok()?;
                (tokens >= high_cache_threshold).then_some(requests)
            })
            .sum()
    }
}

#[derive(Clone)]
pub struct RedisStore {
    client: redis::Client,
    manager: ConnectionManager,
    scheduler_manager: ConnectionManager,
    scheduler_capacity_manager: ConnectionManager,
    role: RedisStoreRole,
    usage_summary_write_gate: Arc<Semaphore>,
    key_prefix: String,
    usage_cleanup_watermark_micros: Arc<AtomicI64>,
    #[cfg(test)]
    external_pool_hot_path_round_trips: Arc<AtomicU64>,
    #[cfg(test)]
    usage_summary_write_round_trips: Arc<AtomicU64>,
    #[cfg(test)]
    scheduler_state_round_trips: Arc<AtomicU64>,
    #[cfg(test)]
    scheduler_state_delay_millis: Arc<AtomicU64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RedisStoreRole {
    Business,
    Observability,
}

const USAGE_SUMMARY_TOTALS_KEY: &str = "usage:summary:totals";
const USAGE_SUMMARY_CACHE_READ_KEY: &str = "usage:summary:cache_read";
const USAGE_SUMMARY_CACHE_READ_INDEX_KEY: &str = "usage:summary:cache_read:index";
const USAGE_SUMMARY_TOP_CREDENTIALS_KEY: &str = "usage:summary:top:credentials";
const USAGE_SUMMARY_TOP_CONVERSATIONS_KEY: &str = "usage:summary:top:conversations";
const USAGE_REALTIME_BUCKET_TTL_SECS: usize = REALTIME_USAGE_WINDOW_SECS as usize * 3;
const USAGE_SUMMARY_SEEN_TTL_SECS: usize = 60 * 60;
const USAGE_DASHBOARD_BUCKET_TTL_SECS: usize = 35 * 24 * 60 * 60;
const USAGE_DASHBOARD_TOP_MODELS_KEY: &str = "usage:dashboard:top:models";
const USAGE_DASHBOARD_TOP_CREDENTIALS_KEY: &str = "usage:dashboard:top:credentials";
const USAGE_DASHBOARD_TOP_ENDPOINTS_KEY: &str = "usage:dashboard:top:endpoints";
const USAGE_DASHBOARD_TOP_ERRORS_KEY: &str = "usage:dashboard:top:errors";
const USAGE_DASHBOARD_TOP_EXTERNAL_POOLS_KEY: &str = "usage:dashboard:top:external_pools";
#[cfg(test)]
const USAGE_DASHBOARD_EXTERNAL_POOL_LIMIT: isize = 19;
const USAGE_RECORDS_INDEX_KEY: &str = "usage:records:index";
const USAGE_RECORDS_TTL_SECS: usize = 35 * 24 * 60 * 60;
const USAGE_RECORDS_MAX_CACHED: usize = 100_000;
const DEFAULT_USAGE_SUMMARY_WRITE_PERMITS: usize = 1;
const SCHEDULER_STATE_BATCH_SIZE: usize = 16;
const SCHEDULER_STATE_BATCH_SCRIPT: &str = r#"
    local query_now = tonumber(ARGV[1])
    local values = {}
    local credentials = math.floor(#KEYS / 9)
    for index = 0, credentials - 1 do
        local base = index * 9
        table.insert(values, redis.call('GET', KEYS[base + 1]))
        table.insert(values, redis.call('GET', KEYS[base + 2]))
        table.insert(values, redis.call('GET', KEYS[base + 3]))
        table.insert(values, redis.call('ZRANGE', KEYS[base + 4], 0, -1, 'WITHSCORES'))
        table.insert(values, redis.call('ZRANGE', KEYS[base + 5], 0, -1, 'WITHSCORES'))
        table.insert(values, redis.call('HGETALL', KEYS[base + 6]))
        table.insert(values, redis.call('HGETALL', KEYS[base + 7]))
        table.insert(values, redis.call('ZCOUNT', KEYS[base + 8], query_now - 10000, '+inf'))
        table.insert(values, redis.call('ZCOUNT', KEYS[base + 8], query_now - 60000, '+inf'))
        table.insert(values, redis.call('ZCOUNT', KEYS[base + 8], query_now - 300000, '+inf'))
        table.insert(values, redis.call('HGETALL', KEYS[base + 9]))
    end
    return values
"#;
const USAGE_CLEANUP_WATERMARK_KEY: &str = "usage:cleanup:soft_delete_cutoff_micros";
const USAGE_DERIVED_CACHE_INVALIDATED_KEY: &str = "usage:cleanup:derived_cache_invalidated";
const USAGE_RECORDS_TRIM_BATCH: usize = 512;
const USAGE_RECORDS_QUERY_SCAN_LIMIT: usize = 5_000;
const USAGE_CACHE_READ_INLINE_BUCKET_LIMIT: usize = 4_096;
const USAGE_CACHE_READ_INLINE_WARN_BUCKET_LIMIT: usize = 1_024;
const DISPATCH_QUEUE_PRUNE_AND_COUNT_SCRIPT: &str = r#"
    local now = redis.call('TIME')
    local now_ms = tonumber(now[1]) * 1000 + math.floor(tonumber(now[2]) / 1000)
    redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', now_ms)
    return redis.call('ZCARD', KEYS[1])
"#;
const DISPATCH_QUEUE_ADMIT_SCRIPT: &str = r#"
    local max_queued = tonumber(ARGV[1])
    local ttl_ms = tonumber(ARGV[2]) * 1000
    local lease_id = ARGV[3]
    local now = redis.call('TIME')
    local now_ms = tonumber(now[1]) * 1000 + math.floor(tonumber(now[2]) / 1000)

    redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', now_ms)
    if redis.call('ZSCORE', KEYS[1], lease_id) then
        return 1
    end

    local count = redis.call('ZCARD', KEYS[1])
    if max_queued > 0 and count >= max_queued then
        return 0
    end

    local expires_at_ms = now_ms + ttl_ms
    redis.call('ZADD', KEYS[1], expires_at_ms, lease_id)
    local latest = redis.call('ZREVRANGE', KEYS[1], 0, 0, 'WITHSCORES')
    if latest[2] then
        redis.call('PEXPIREAT', KEYS[1], math.ceil(tonumber(latest[2])))
    end
    return 1
"#;
const DISPATCH_QUEUE_RELEASE_SCRIPT: &str = r#"
    local removed = redis.call('ZREM', KEYS[1], ARGV[1])
    if redis.call('ZCARD', KEYS[1]) == 0 then
        redis.call('DEL', KEYS[1])
    end
    return removed
"#;
const DISPATCH_QUEUE_RENEW_SCRIPT: &str = r#"
    local ttl_ms = tonumber(ARGV[1]) * 1000
    local lease_id = ARGV[2]
    local now = redis.call('TIME')
    local now_ms = tonumber(now[1]) * 1000 + math.floor(tonumber(now[2]) / 1000)

    redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', now_ms)
    if not redis.call('ZSCORE', KEYS[1], lease_id) then
        return 0
    end

    local expires_at_ms = now_ms + ttl_ms
    redis.call('ZADD', KEYS[1], 'XX', expires_at_ms, lease_id)
    local latest = redis.call('ZREVRANGE', KEYS[1], 0, 0, 'WITHSCORES')
    if latest[2] then
        redis.call('PEXPIREAT', KEYS[1], math.ceil(tonumber(latest[2])))
    end
    return 1
"#;
const TOKEN_REFRESH_LOCK_TTL_MS: u64 = 120_000;
const TOKEN_REFRESH_SUCCESS_TTL_MS: u64 = 15_000;
const TOKEN_REFRESH_FAILURE_RETENTION_MS: u64 = 60_000;
const TOKEN_REFRESH_OUTCOME_MAX_TTL_MS: u64 = 120_000;
const TOKEN_REFRESH_HEALTH_CLAIM_TTL_MS: u64 = 5_000;
const TOKEN_REFRESH_POLL_AFTER_MS: u64 = 500;
const TOKEN_REFRESH_NEGATIVE_BACKOFF_BASE_MS: u64 = 500;
const TOKEN_REFRESH_NEGATIVE_BACKOFF_MAX_MS: u64 = 30_000;
const TOKEN_REFRESH_NEGATIVE_RETRY_AFTER_MAX_MS: u64 = 60_000;
const TOKEN_REFRESH_NEGATIVE_STREAK_RESET_MS: u64 = 60_000;
const TOKEN_REFRESH_NEGATIVE_MAX_STREAK: u8 = 16;
const TOKEN_REFRESH_NEGATIVE_MIN_REPLAY_MS: u64 = TOKEN_REFRESH_POLL_AFTER_MS + 250;
const TOKEN_REFRESH_BUCKET_TOKEN_UNITS: u64 = 60_000;

const TOKEN_REFRESH_BEGIN_SCRIPT: &str = r#"
    local redis_time = redis.call('TIME')
    local now_ms = tonumber(redis_time[1]) * 1000 + math.floor(tonumber(redis_time[2]) / 1000)
    local owner = ARGV[1]
    local identity = ARGV[2]
    local lock_ttl_ms = tonumber(ARGV[3])
    local caller_can_claim_health = ARGV[4] == '1'
    local health_claim_token = ARGV[5]
    local health_claim_ttl_ms = tonumber(ARGV[6])
    local poll_after_ms = tonumber(ARGV[7])
    local streak_reset_ms = tonumber(ARGV[8])

    local state = redis.call('HGET', KEYS[2], 'state') or ''
    local stored_identity = redis.call('HGET', KEYS[2], 'identity') or ''
    local generation = tonumber(redis.call('HGET', KEYS[2], 'generation') or '0')

    if state == 'failed' and stored_identity == identity then
        local retry_at_ms = tonumber(redis.call('HGET', KEYS[2], 'retry_at_ms') or '0')
        if retry_at_ms > now_ms then
            local claimed_token = ''
            local claim_until_ms = tonumber(redis.call('HGET', KEYS[2], 'health_claim_until_ms') or '0')
            local health_state = redis.call('HGET', KEYS[2], 'health_state') or 'none'
            if caller_can_claim_health and health_state ~= 'none' and health_state ~= 'applied'
                and (health_state == 'pending' or claim_until_ms <= now_ms) then
                claim_until_ms = now_ms + health_claim_ttl_ms
                redis.call('HSET', KEYS[2],
                    'health_state', 'claimed',
                    'health_claim_token', health_claim_token,
                    'health_claim_until_ms', tostring(claim_until_ms))
                claimed_token = health_claim_token
            end
            return {
                'replay', tostring(generation), '',
                redis.call('HGET', KEYS[2], 'stage') or '',
                redis.call('HGET', KEYS[2], 'kind') or '',
                redis.call('HGET', KEYS[2], 'status') or '',
                redis.call('HGET', KEYS[2], 'retry_after_ms') or '',
                redis.call('HGET', KEYS[2], 'send_committed') or '0',
                tostring(retry_at_ms),
                redis.call('HGET', KEYS[2], 'consecutive') or '1',
                claimed_token, tostring(claim_until_ms),
                redis.call('HGET', KEYS[2], 'health_action_required') or '0', ''
            }
        end
    elseif state == 'succeeded' and stored_identity == identity then
        return {
            'succeeded', tostring(generation), '', '', '', '', '', '', '', '', '', '',
            redis.call('HGET', KEYS[2], 'storage_revision') or '0', ''
        }
    elseif state == 'failed' and stored_identity ~= identity then
        local failure_fence = redis.call('HGET', KEYS[2], 'lock_fence') or ''
        if failure_fence ~= '' and redis.call('GET', KEYS[1]) == failure_fence then
            redis.call('DEL', KEYS[1])
        end
    end

    local acquired = redis.call('SET', KEYS[1], owner, 'NX', 'PX', lock_ttl_ms)
    if acquired then
        local prior_failure_streak = 0
        if state == 'failed' and stored_identity == identity then
            local previous_retry_at_ms = tonumber(redis.call('HGET', KEYS[2], 'retry_at_ms') or '0')
            if now_ms >= previous_retry_at_ms and now_ms - previous_retry_at_ms <= streak_reset_ms then
                prior_failure_streak = tonumber(redis.call('HGET', KEYS[2], 'consecutive') or '0')
            end
        end
        generation = generation + 1
        redis.call('DEL', KEYS[2])
        redis.call('HSET', KEYS[2],
            'schema', '1',
            'state', 'running',
            'generation', tostring(generation),
            'owner', owner,
            'identity', identity,
            'prior_consecutive', tostring(prior_failure_streak),
            'updated_at_ms', tostring(now_ms))
        redis.call('PEXPIRE', KEYS[2], lock_ttl_ms)
        return {
            'leader', tostring(generation), owner, '', '', '', '', '', '',
            tostring(prior_failure_streak), '', '', '', ''
        }
    end

    local lock_ttl_remaining_ms = tonumber(redis.call('PTTL', KEYS[1]) or '-1')
    if lock_ttl_remaining_ms > 0 then
        poll_after_ms = math.max(1, math.min(poll_after_ms, lock_ttl_remaining_ms))
    end
    return {'wait', tostring(generation), '', '', '', '', '', '', '', '', '', '', '', tostring(poll_after_ms)}
"#;

const TOKEN_REFRESH_COMPLETE_FAILURE_SCRIPT: &str = r#"
    local redis_time = redis.call('TIME')
    local now_ms = tonumber(redis_time[1]) * 1000 + math.floor(tonumber(redis_time[2]) / 1000)
    local owner = ARGV[1]
    local generation = ARGV[2]
    local identity = ARGV[3]
    if redis.call('GET', KEYS[1]) ~= owner
        or redis.call('HGET', KEYS[2], 'state') ~= 'running'
        or redis.call('HGET', KEYS[2], 'generation') ~= generation
        or redis.call('HGET', KEYS[2], 'owner') ~= owner
        or redis.call('HGET', KEYS[2], 'identity') ~= identity then
        return {'stale'}
    end

    local delay_ms = tonumber(ARGV[12])
    local retry_at_ms = now_ms + delay_ms
    local outcome_ttl_ms = tonumber(ARGV[13])
    local health_state = 'none'
    local claimed_token = ''
    local claim_until_ms = 0
    if ARGV[9] == '1' then
        health_state = 'pending'
        if ARGV[10] == '1' then
            health_state = 'claimed'
            claimed_token = ARGV[11]
            claim_until_ms = now_ms + tonumber(ARGV[14])
        end
    end
    local failure_fence = 'failure:' .. generation .. ':' .. ARGV[15]
    redis.call('SET', KEYS[1], failure_fence, 'PX', math.max(1, delay_ms))
    redis.call('HSET', KEYS[2],
        'schema', '1',
        'state', 'failed',
        'generation', generation,
        'owner', '',
        'identity', identity,
        'stage', ARGV[4],
        'kind', ARGV[5],
        'status', ARGV[6],
        'retry_after_ms', ARGV[7],
        'send_committed', ARGV[8],
        'retry_at_ms', tostring(retry_at_ms),
        'consecutive', ARGV[16],
        'health_action_required', ARGV[9],
        'health_state', health_state,
        'health_claim_token', claimed_token,
        'health_claim_until_ms', tostring(claim_until_ms),
        'lock_fence', failure_fence,
        'updated_at_ms', tostring(now_ms))
    redis.call('PEXPIRE', KEYS[2], outcome_ttl_ms)
    return {
        'committed', generation, '', ARGV[4], ARGV[5], ARGV[6], ARGV[7], ARGV[8],
        tostring(retry_at_ms), ARGV[16], claimed_token, tostring(claim_until_ms), ARGV[9], ''
    }
"#;

const TOKEN_REFRESH_COMPLETE_SUCCESS_SCRIPT: &str = r#"
    local redis_time = redis.call('TIME')
    local now_ms = tonumber(redis_time[1]) * 1000 + math.floor(tonumber(redis_time[2]) / 1000)
    if redis.call('GET', KEYS[1]) ~= ARGV[1]
        or redis.call('HGET', KEYS[2], 'state') ~= 'running'
        or redis.call('HGET', KEYS[2], 'generation') ~= ARGV[2]
        or redis.call('HGET', KEYS[2], 'owner') ~= ARGV[1]
        or redis.call('HGET', KEYS[2], 'identity') ~= ARGV[3] then
        return 0
    end
    redis.call('DEL', KEYS[2])
    redis.call('HSET', KEYS[2],
        'schema', '1',
        'state', 'succeeded',
        'generation', ARGV[2],
        'identity', ARGV[3],
        'storage_revision', ARGV[4],
        'updated_at_ms', tostring(now_ms))
    redis.call('PEXPIRE', KEYS[2], ARGV[5])
    redis.call('DEL', KEYS[1])
    return 1
"#;

const TOKEN_REFRESH_CANCEL_SCRIPT: &str = r#"
    if redis.call('GET', KEYS[1]) ~= ARGV[1]
        or redis.call('HGET', KEYS[2], 'state') ~= 'running'
        or redis.call('HGET', KEYS[2], 'generation') ~= ARGV[2]
        or redis.call('HGET', KEYS[2], 'owner') ~= ARGV[1]
        or redis.call('HGET', KEYS[2], 'identity') ~= ARGV[3] then
        return 0
    end
    redis.call('DEL', KEYS[1])
    redis.call('DEL', KEYS[2])
    return 1
"#;

const TOKEN_REFRESH_ACK_HEALTH_SCRIPT: &str = r#"
    if redis.call('HGET', KEYS[1], 'state') ~= 'failed'
        or redis.call('HGET', KEYS[1], 'generation') ~= ARGV[1]
        or redis.call('HGET', KEYS[1], 'health_state') ~= 'claimed'
        or redis.call('HGET', KEYS[1], 'health_claim_token') ~= ARGV[2] then
        return 0
    end
    redis.call('HSET', KEYS[1],
        'health_state', 'applied',
        'health_claim_token', '',
        'health_claim_until_ms', '0')
    return 1
"#;

const TOKEN_REFRESH_BUCKET_SCRIPT: &str = r#"
    local redis_time = redis.call('TIME')
    local now_ms = tonumber(redis_time[1]) * 1000 + math.floor(tonumber(redis_time[2]) / 1000)
    local rpm = tonumber(ARGV[1])
    local burst = tonumber(ARGV[2])
    local token_units = tonumber(ARGV[3])
    local capacity = burst * token_units
    local previous_units = tonumber(redis.call('HGET', KEYS[1], 'units') or tostring(capacity))
    local previous_ms = tonumber(redis.call('HGET', KEYS[1], 'last_ms') or tostring(now_ms))
    local stored_version = redis.call('HGET', KEYS[1], 'version') or ''
    local current_version = ARGV[4]
    local elapsed_ms = 0
    if stored_version == current_version then
        elapsed_ms = math.max(0, math.min(86400000, now_ms - previous_ms))
    else
        previous_ms = now_ms
        previous_units = math.min(previous_units, capacity)
    end
    local available = math.min(capacity, previous_units + elapsed_ms * rpm)
    local admitted = 0
    local retry_after_ms = 0
    if available >= token_units then
        admitted = 1
        available = available - token_units
    else
        retry_after_ms = math.ceil((token_units - available) / rpm)
    end
    available = math.max(0, math.floor(available))
    redis.call('HSET', KEYS[1], 'units', tostring(available), 'last_ms', tostring(now_ms), 'version', current_version)
    redis.call('PEXPIRE', KEYS[1], ARGV[5])
    return {admitted, retry_after_ms, available}
"#;
const ADVANCE_USAGE_CLEANUP_WATERMARK_SCRIPT: &str = r#"
    local current = tonumber(redis.call('GET', KEYS[1]) or '0')
    local candidate = tonumber(ARGV[1]) or 0
    if candidate > current then
        redis.call('SET', KEYS[1], candidate)
        current = candidate
    end
    return current
"#;
const READ_USAGE_TOTALS_IF_VALID_SCRIPT: &str = r#"
    if redis.call('EXISTS', KEYS[1]) == 1 then
        return {}
    end
    return redis.call('HGETALL', KEYS[2])
"#;
#[cfg(test)]
const GUARDED_USAGE_PIPELINE_SCRIPT: &str = r#"
    local record_micros = tonumber(ARGV[1]) or 0
    local local_cutoff = tonumber(ARGV[2]) or 0
    local persisted_cutoff = tonumber(redis.call('GET', KEYS[1]) or '0')
    local effective_cutoff = persisted_cutoff
    if local_cutoff > effective_cutoff then
        redis.call('SET', KEYS[1], local_cutoff)
        effective_cutoff = local_cutoff
    end
    if record_micros < effective_cutoff then
        return {0, effective_cutoff}
    end
    if redis.call('EXISTS', KEYS[2]) == 1 then
        return {0, effective_cutoff}
    end

    local command_count = tonumber(ARGV[3]) or 0
    local cursor = 4
    for _ = 1, command_count do
        local argc = tonumber(ARGV[cursor]) or 0
        cursor = cursor + 1
        local command = ARGV[cursor]
        cursor = cursor + 1
        local args = {}
        for arg_index = 2, argc do
            args[#args + 1] = ARGV[cursor]
            cursor = cursor + 1
        end
        if command == '__USAGE_DURATION_MAX__' then
            local current = tonumber(redis.call('HGET', args[1], 'duration_ms_max') or '0')
            local candidate = tonumber(args[2]) or 0
            if candidate > current then
                redis.call('HSET', args[1], 'duration_ms_max', candidate)
            end
            redis.call('EXPIRE', args[1], args[3])
        else
            redis.call(command, unpack(args))
        end
    end
    return {1, effective_cutoff}
"#;
const GUARDED_IDEMPOTENT_USAGE_PIPELINE_SCRIPT: &str = r#"
    local function command_failed(result)
        return type(result) == 'table' and result['err'] ~= nil
    end
    local function call_succeeded(command, args)
        return not command_failed(redis.pcall(command, unpack(args)))
    end
    local function invalidate_and_fail(effective_cutoff)
        redis.call('SET', KEYS[2], '1')
        return {-1, effective_cutoff}
    end

    local record_micros = tonumber(ARGV[1]) or 0
    local local_cutoff = tonumber(ARGV[2]) or 0
    local seen_ttl = tonumber(ARGV[3]) or 1
    local cache_read_bucket = ARGV[4]
    local cache_read_bucket_limit = tonumber(ARGV[5]) or 0
    local snapshot_ttl = tonumber(ARGV[6]) or 1
    local snapshot_member = ARGV[7]
    local snapshot_score = tonumber(ARGV[8]) or 0
    local snapshot_cutoff_ms = tonumber(ARGV[9]) or 0
    local snapshot_max_cached = tonumber(ARGV[10]) or 1
    local snapshot_trim_batch = tonumber(ARGV[11]) or 1
    local snapshot_item_key_prefix = ARGV[12]
    local snapshot_encoded = ARGV[13]

    local persisted_cutoff_result = redis.pcall('GET', KEYS[1])
    if command_failed(persisted_cutoff_result) then
        return invalidate_and_fail(local_cutoff)
    end
    local persisted_cutoff = tonumber(persisted_cutoff_result or '0') or 0
    local effective_cutoff = persisted_cutoff
    if local_cutoff > effective_cutoff then
        if not call_succeeded('SET', {KEYS[1], local_cutoff}) then
            return invalidate_and_fail(effective_cutoff)
        end
        effective_cutoff = local_cutoff
    end
    if record_micros < effective_cutoff then
        return {0, effective_cutoff}
    end
    if redis.call('EXISTS', KEYS[2]) == 1 then
        return {0, effective_cutoff}
    end
    if redis.call('EXISTS', KEYS[3]) == 1 then
        return {2, effective_cutoff}
    end

    if cache_read_bucket_limit > 0 then
        for key_index = 4, 5 do
            local key_type_result = redis.call('TYPE', KEYS[key_index])
            local key_type = key_type_result
            if type(key_type_result) == 'table' then
                key_type = key_type_result['ok']
            end
            if key_type ~= 'none' and key_type ~= 'hash' then
                return invalidate_and_fail(effective_cutoff)
            end
            if redis.call('HEXISTS', KEYS[key_index], cache_read_bucket) == 0
                and redis.call('HLEN', KEYS[key_index]) >= cache_read_bucket_limit then
                redis.call('SET', KEYS[2], '1')
                return {0, effective_cutoff}
            end
        end
    end

    if not call_succeeded('SETEX', {KEYS[6], snapshot_ttl, snapshot_encoded})
        or not call_succeeded('ZADD', {KEYS[7], snapshot_score, snapshot_member})
        or not call_succeeded('EXPIRE', {KEYS[7], snapshot_ttl}) then
        return invalidate_and_fail(effective_cutoff)
    end

    local expired = redis.pcall(
        'ZRANGEBYSCORE', KEYS[7], '-inf', snapshot_cutoff_ms,
        'LIMIT', 0, snapshot_trim_batch
    )
    if command_failed(expired) then
        return invalidate_and_fail(effective_cutoff)
    end
    for _, old_member in ipairs(expired) do
        if not call_succeeded('DEL', {snapshot_item_key_prefix .. old_member})
            or not call_succeeded('ZREM', {KEYS[7], old_member}) then
            return invalidate_and_fail(effective_cutoff)
        end
    end

    local cached_count = redis.pcall('ZCARD', KEYS[7])
    if command_failed(cached_count) then
        return invalidate_and_fail(effective_cutoff)
    end
    local overflow = (tonumber(cached_count) or 0) - snapshot_max_cached
    if overflow > 0 then
        local overflow_limit = math.min(overflow, snapshot_trim_batch)
        local old_members = redis.pcall('ZRANGE', KEYS[7], 0, overflow_limit - 1)
        if command_failed(old_members) then
            return invalidate_and_fail(effective_cutoff)
        end
        for _, old_member in ipairs(old_members) do
            if not call_succeeded('DEL', {snapshot_item_key_prefix .. old_member})
                or not call_succeeded('ZREM', {KEYS[7], old_member}) then
                return invalidate_and_fail(effective_cutoff)
            end
        end
    end

    local command_count = tonumber(ARGV[14]) or 0
    local cursor = 15
    for _ = 1, command_count do
        local argc = tonumber(ARGV[cursor]) or 0
        cursor = cursor + 1
        local command = ARGV[cursor]
        cursor = cursor + 1
        local args = {}
        for arg_index = 2, argc do
            args[#args + 1] = ARGV[cursor]
            cursor = cursor + 1
        end
        if command == '__USAGE_DURATION_MAX__' then
            local current_result = redis.pcall('HGET', args[1], 'duration_ms_max')
            if command_failed(current_result) then
                return invalidate_and_fail(effective_cutoff)
            end
            local current = tonumber(current_result or '0') or 0
            local candidate = tonumber(args[2]) or 0
            if candidate > current
                and not call_succeeded('HSET', {args[1], 'duration_ms_max', candidate}) then
                return invalidate_and_fail(effective_cutoff)
            end
            if not call_succeeded('EXPIRE', {args[1], args[3]}) then
                return invalidate_and_fail(effective_cutoff)
            end
        elseif not call_succeeded(command, args) then
            return invalidate_and_fail(effective_cutoff)
        end
    end
    if not call_succeeded('SET', {KEYS[3], '1', 'EX', seen_ttl}) then
        return invalidate_and_fail(effective_cutoff)
    end
    return {1, effective_cutoff}
"#;

#[derive(Default)]
struct GuardedUsagePipeline {
    commands: Vec<Vec<Vec<u8>>>,
}

impl GuardedUsagePipeline {
    fn atomic(&mut self) -> &mut Self {
        self
    }

    fn cmd(&mut self, command: &str) -> &mut Self {
        self.commands.push(vec![command.as_bytes().to_vec()]);
        self
    }

    fn arg<T: ToRedisArgs>(&mut self, value: T) -> &mut Self {
        let args = value.to_redis_args();
        self.commands
            .last_mut()
            .expect("a guarded usage command must precede its arguments")
            .extend(args);
        self
    }

    #[cfg(test)]
    async fn query_guarded(
        &self,
        manager: &mut ConnectionManager,
        watermark_key: &str,
        invalidated_key: &str,
        record_micros: i64,
        local_cutoff_micros: i64,
    ) -> redis::RedisResult<(bool, i64)> {
        let mut command = redis::cmd("EVAL");
        command
            .arg(GUARDED_USAGE_PIPELINE_SCRIPT)
            .arg(2)
            .arg(watermark_key)
            .arg(invalidated_key)
            .arg(record_micros)
            .arg(local_cutoff_micros)
            .arg(self.commands.len());
        for guarded_command in &self.commands {
            command.arg(guarded_command.len());
            for arg in guarded_command {
                command.arg(arg);
            }
        }
        let (accepted, effective_cutoff): (i64, i64) = command.query_async(manager).await?;
        Ok((accepted != 0, effective_cutoff))
    }

    async fn query_guarded_idempotent(
        &self,
        manager: &mut ConnectionManager,
        watermark_key: &str,
        invalidated_key: &str,
        seen_key: &str,
        cache_read_key: &str,
        dashboard_cache_read_key: &str,
        record_micros: i64,
        local_cutoff_micros: i64,
        seen_ttl_secs: usize,
        cache_read_bucket: &str,
        cache_read_bucket_limit: usize,
        snapshot_key: &str,
        snapshot_index_key: &str,
        snapshot_ttl_secs: usize,
        snapshot_member: &str,
        snapshot_score: i64,
        snapshot_cutoff_ms: i64,
        snapshot_max_cached: usize,
        snapshot_trim_batch: usize,
        snapshot_item_key_prefix: &str,
        snapshot_encoded: &str,
    ) -> redis::RedisResult<(bool, i64)> {
        let mut command = redis::cmd("EVAL");
        command
            .arg(GUARDED_IDEMPOTENT_USAGE_PIPELINE_SCRIPT)
            .arg(7)
            .arg(watermark_key)
            .arg(invalidated_key)
            .arg(seen_key)
            .arg(cache_read_key)
            .arg(dashboard_cache_read_key)
            .arg(snapshot_key)
            .arg(snapshot_index_key)
            .arg(record_micros)
            .arg(local_cutoff_micros)
            .arg(seen_ttl_secs.max(1))
            .arg(cache_read_bucket)
            .arg(cache_read_bucket_limit.max(1))
            .arg(snapshot_ttl_secs.max(1))
            .arg(snapshot_member)
            .arg(snapshot_score)
            .arg(snapshot_cutoff_ms)
            .arg(snapshot_max_cached.max(1))
            .arg(snapshot_trim_batch.max(1))
            .arg(snapshot_item_key_prefix)
            .arg(snapshot_encoded)
            .arg(self.commands.len());
        for guarded_command in &self.commands {
            command.arg(guarded_command.len());
            for arg in guarded_command {
                command.arg(arg);
            }
        }
        let (accepted, effective_cutoff): (i64, i64) = command.query_async(manager).await?;
        if accepted < 0 {
            return Err(redis::RedisError::from((
                redis::ErrorKind::TypeError,
                "Redis usage summary pipeline invalidated after a command/type error",
            )));
        }
        Ok((accepted != 0, effective_cutoff))
    }
}

fn refresh_identity_hex(identity: &[u8; 32]) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(64);
    for byte in identity {
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

fn parse_refresh_u64(value: &str, field: &'static str) -> anyhow::Result<u64> {
    value
        .parse::<u64>()
        .map_err(|_| anyhow::anyhow!("Redis returned an invalid token refresh {field}"))
}

fn parse_refresh_i64(value: &str, field: &'static str) -> anyhow::Result<i64> {
    value
        .parse::<i64>()
        .map_err(|_| anyhow::anyhow!("Redis returned an invalid token refresh {field}"))
}

fn parse_refresh_bool(value: &str, field: &'static str) -> anyhow::Result<bool> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => anyhow::bail!("Redis returned an invalid token refresh {field}"),
    }
}

fn decode_refresh_failure_outcome(
    values: &[String],
    expected_tag: &str,
    expected_generation: Option<u64>,
    expected_claim_token: &str,
) -> anyhow::Result<RedisRefreshFailureCommit> {
    if values.len() != 14 || values.first().map(String::as_str) != Some(expected_tag) {
        anyhow::bail!("Redis returned an invalid token refresh failure outcome");
    }
    let generation = parse_refresh_u64(&values[1], "generation")?;
    if generation == 0 || expected_generation.is_some_and(|expected| expected != generation) {
        anyhow::bail!("Redis returned a stale token refresh failure generation");
    }
    let status = if values[5].is_empty() {
        None
    } else {
        Some(
            values[5]
                .parse::<u16>()
                .map_err(|_| anyhow::anyhow!("Redis returned an invalid token refresh status"))?,
        )
    };
    let retry_after_ms = if values[6].is_empty() {
        None
    } else {
        let value = parse_refresh_u64(&values[6], "retry-after")?;
        if value > TOKEN_REFRESH_NEGATIVE_RETRY_AFTER_MAX_MS {
            anyhow::bail!("Redis returned an out-of-range token refresh retry-after");
        }
        Some(value)
    };
    let retry_at_epoch_ms = parse_refresh_i64(&values[8], "retry deadline")?;
    if retry_at_epoch_ms <= 0 {
        anyhow::bail!("Redis returned an invalid token refresh retry deadline");
    }
    let consecutive_failures = parse_refresh_u64(&values[9], "failure streak")?;
    if !(1..=u64::from(TOKEN_REFRESH_NEGATIVE_MAX_STREAK)).contains(&consecutive_failures) {
        anyhow::bail!("Redis returned an out-of-range token refresh failure streak");
    }
    let health_action_required = parse_refresh_bool(&values[12], "health action marker")?;
    let health_claim = if values[10].is_empty() {
        None
    } else {
        if values[10] != expected_claim_token {
            anyhow::bail!("Redis returned an unexpected token refresh health claim");
        }
        let claim_until_ms = parse_refresh_i64(&values[11], "health claim deadline")?;
        if claim_until_ms <= 0 {
            anyhow::bail!("Redis returned an invalid token refresh health claim deadline");
        }
        Some(RedisRefreshHealthClaim {
            generation,
            token: values[10].clone(),
        })
    };
    if health_claim.is_some() && !health_action_required {
        anyhow::bail!("Redis returned a token refresh health claim without an action");
    }
    Ok(RedisRefreshFailureCommit {
        outcome: RedisRefreshFailureOutcome {
            generation,
            failure: RedisRefreshFailure {
                stage: RedisRefreshFailureStage::parse(&values[3])?,
                kind: RedisRefreshFailureKind::parse(&values[4])?,
                status,
                retry_after: retry_after_ms.map(StdDuration::from_millis),
                send_committed: parse_refresh_bool(&values[7], "send marker")?,
                health_action_required,
            },
            retry_at_epoch_ms,
            consecutive_failures: consecutive_failures as u8,
        },
        health_claim,
    })
}

fn decode_refresh_begin(
    values: &[String],
    credential_id: u64,
    identity: [u8; 32],
    expected_owner: &str,
    expected_claim_token: &str,
) -> anyhow::Result<RedisRefreshBegin> {
    if values.len() != 14 {
        anyhow::bail!("Redis returned an invalid token refresh coordination result");
    }
    match values[0].as_str() {
        "leader" => {
            let generation = parse_refresh_u64(&values[1], "generation")?;
            let prior_failure_streak = parse_refresh_u64(&values[9], "prior failure streak")?;
            if generation == 0
                || values[2] != expected_owner
                || prior_failure_streak > u64::from(TOKEN_REFRESH_NEGATIVE_MAX_STREAK)
            {
                anyhow::bail!("Redis returned an invalid token refresh leader lease");
            }
            Ok(RedisRefreshBegin::Leader(RedisRefreshLease {
                credential_id,
                generation,
                owner: values[2].clone(),
                identity,
                prior_failure_streak: prior_failure_streak as u8,
            }))
        }
        "wait" => {
            let generation = parse_refresh_u64(&values[1], "generation")?;
            let poll_after_ms = parse_refresh_u64(&values[13], "poll interval")?;
            if !(1..=TOKEN_REFRESH_POLL_AFTER_MS).contains(&poll_after_ms) {
                anyhow::bail!("Redis returned an out-of-range token refresh poll interval");
            }
            Ok(RedisRefreshBegin::Wait {
                generation: (generation > 0).then_some(generation),
                poll_after: StdDuration::from_millis(poll_after_ms),
            })
        }
        "replay" => {
            let committed =
                decode_refresh_failure_outcome(values, "replay", None, expected_claim_token)?;
            Ok(RedisRefreshBegin::Replay {
                outcome: committed.outcome,
                health_claim: committed.health_claim,
            })
        }
        "succeeded" => {
            let generation = parse_refresh_u64(&values[1], "generation")?;
            let storage_revision = parse_refresh_u64(&values[12], "storage revision")?;
            if generation == 0 || storage_revision == 0 {
                anyhow::bail!("Redis returned an invalid token refresh success fence");
            }
            Ok(RedisRefreshBegin::Succeeded {
                generation,
                storage_revision,
            })
        }
        _ => anyhow::bail!("Redis returned an unknown token refresh coordination result"),
    }
}

fn refresh_failure_delay(
    lease: &RedisRefreshLease,
    retry_after: Option<StdDuration>,
) -> (u8, StdDuration) {
    let consecutive_failures = lease
        .prior_failure_streak
        .saturating_add(1)
        .min(TOKEN_REFRESH_NEGATIVE_MAX_STREAK);
    let shift = u32::from(consecutive_failures.saturating_sub(1)).min(20);
    let exponential_ms = TOKEN_REFRESH_NEGATIVE_BACKOFF_BASE_MS
        .saturating_mul(1_u64 << shift)
        .min(TOKEN_REFRESH_NEGATIVE_BACKOFF_MAX_MS);
    let seed = u64::from_le_bytes(lease.identity[..8].try_into().expect("SHA-256 prefix"));
    let jitter_percent = 80 + seed % 21;
    let jittered_ms = exponential_ms.saturating_mul(jitter_percent) / 100;
    let retry_after_ms = retry_after
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or_default()
        .min(TOKEN_REFRESH_NEGATIVE_RETRY_AFTER_MAX_MS);
    (
        consecutive_failures,
        StdDuration::from_millis(
            jittered_ms
                .max(retry_after_ms)
                .max(TOKEN_REFRESH_NEGATIVE_MIN_REPLAY_MS),
        ),
    )
}

fn token_refresh_bucket_ttl_ms(max_rpm: u32, burst: u32) -> u64 {
    u64::from(burst)
        .saturating_mul(TOKEN_REFRESH_BUCKET_TOKEN_UNITS)
        .saturating_add(u64::from(max_rpm) - 1)
        / u64::from(max_rpm)
        + 60_000
}

fn decode_token_refresh_bucket_decision(
    values: &[i64],
    burst: u32,
) -> anyhow::Result<TokenRefreshBucketDecision> {
    if values.len() != 3 || !matches!(values[0], 0 | 1) {
        anyhow::bail!("Redis returned an invalid token refresh bucket decision");
    }
    let retry_after_ms = u64::try_from(values[1])
        .map_err(|_| anyhow::anyhow!("Redis returned an invalid token refresh bucket retry"))?;
    let remaining_units = u64::try_from(values[2])
        .map_err(|_| anyhow::anyhow!("Redis returned invalid token refresh bucket capacity"))?;
    let capacity = u64::from(burst).saturating_mul(TOKEN_REFRESH_BUCKET_TOKEN_UNITS);
    if retry_after_ms > TOKEN_REFRESH_BUCKET_TOKEN_UNITS || remaining_units > capacity {
        anyhow::bail!("Redis returned an out-of-range token refresh bucket decision");
    }
    let admitted = values[0] == 1;
    if admitted && retry_after_ms != 0 {
        anyhow::bail!("Redis returned a retry delay for an admitted token refresh");
    }
    if !admitted && retry_after_ms == 0 {
        anyhow::bail!("Redis returned no retry delay for a rejected token refresh");
    }
    Ok(TokenRefreshBucketDecision {
        admitted,
        retry_after: (!admitted).then(|| StdDuration::from_millis(retry_after_ms)),
        remaining_milli_tokens: remaining_units / (TOKEN_REFRESH_BUCKET_TOKEN_UNITS / 1_000),
    })
}

impl RedisStore {
    #[cfg(test)]
    pub fn key_prefix_for_test(&self) -> String {
        self.key_prefix.clone()
    }

    pub async fn connect(config: &Config) -> anyhow::Result<Self> {
        Self::connect_config(&config.redis).await
    }

    /// Connect a Redis store from an explicit fault-domain configuration.
    ///
    /// Business call sites continue to use [`Self::connect`]. The explicit variant lets startup
    /// construct an optional observability store without cloning or rewriting the business
    /// configuration.
    pub async fn connect_config(config: &RedisConfig) -> anyhow::Result<Self> {
        Self::connect_config_with_role(config, RedisStoreRole::Business).await
    }

    pub async fn connect_observability(config: &RedisConfig) -> anyhow::Result<Self> {
        Self::connect_config_with_role(config, RedisStoreRole::Observability).await
    }

    async fn connect_config_with_role(
        config: &RedisConfig,
        role: RedisStoreRole,
    ) -> anyhow::Result<Self> {
        let url = config
            .url
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("必须配置 redis.url"))?;
        let client = redis::Client::open(url)?;
        let manager = client.get_connection_manager().await?;
        let (scheduler_manager, scheduler_capacity_manager) = match role {
            RedisStoreRole::Business => (
                client.get_connection_manager().await?,
                client.get_connection_manager().await?,
            ),
            RedisStoreRole::Observability => (manager.clone(), manager.clone()),
        };
        #[cfg(test)]
        let usage_summary_write_permits = std::env::var("KIRO_RS_TEST_USAGE_SUMMARY_WRITE_PERMITS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .map(|value| value.clamp(1, 4))
            .unwrap_or(DEFAULT_USAGE_SUMMARY_WRITE_PERMITS);
        #[cfg(not(test))]
        let usage_summary_write_permits = DEFAULT_USAGE_SUMMARY_WRITE_PERMITS;
        Ok(Self {
            client,
            manager,
            scheduler_manager,
            scheduler_capacity_manager,
            role,
            usage_summary_write_gate: Arc::new(Semaphore::new(usage_summary_write_permits)),
            key_prefix: config.key_prefix.trim_end_matches(':').to_string(),
            usage_cleanup_watermark_micros: Arc::new(AtomicI64::new(0)),
            #[cfg(test)]
            external_pool_hot_path_round_trips: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            usage_summary_write_round_trips: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            scheduler_state_round_trips: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            scheduler_state_delay_millis: Arc::new(AtomicU64::new(0)),
        })
    }

    pub(crate) fn is_observability(&self) -> bool {
        self.role == RedisStoreRole::Observability
    }

    fn ensure_observability_usage_store(&self, operation: &'static str) -> anyhow::Result<()> {
        if cfg!(not(test)) && !self.is_observability() {
            anyhow::bail!(
                "{operation} must use observability Redis; business scheduler Redis cannot be used for usage materialization"
            );
        }
        Ok(())
    }

    /// Return the Redis server process identity used to prove fault-domain separation.
    ///
    /// Host/port validation cannot detect DNS aliases, tunnels, or load balancers that
    /// ultimately point at the same Redis process. Startup compares this opaque identity
    /// once before enabling the optional observability store.
    pub(crate) async fn server_run_id(&self) -> anyhow::Result<String> {
        let mut manager = self.manager.clone();
        let server_info: String = redis::cmd("INFO")
            .arg("server")
            .query_async(&mut manager)
            .await?;
        server_info
            .lines()
            .find_map(|line| {
                line.trim_end_matches('\r')
                    .split_once(':')
                    .filter(|(key, _)| *key == "run_id")
                    .map(|(_, value)| value.trim().to_string())
            })
            .filter(|run_id| !run_id.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Redis INFO server did not include run_id"))
    }

    fn scheduler_manager(&self) -> ConnectionManager {
        self.scheduler_manager.clone()
    }

    fn scheduler_capacity_manager(&self) -> ConnectionManager {
        self.scheduler_capacity_manager.clone()
    }

    #[cfg(test)]
    async fn dashboard_bucket_cache(
        &self,
        window_specs: &[UsageDashboardWindowSpec],
        hourly_specs: &[UsageDashboardWindowSpec],
        daily_specs: &[UsageDashboardWindowSpec],
        external_pool_index: &[RedisExternalPoolIndexItem],
    ) -> anyhow::Result<DashboardBucketCache> {
        let mut suffixes = Vec::new();
        let mut seen = HashSet::new();
        for spec in window_specs {
            collect_dashboard_window_bucket_keys(
                &mut suffixes,
                &mut seen,
                spec,
                external_pool_index,
            );
        }
        for spec in hourly_specs.iter().chain(daily_specs.iter()) {
            collect_dashboard_global_bucket_keys(&mut suffixes, &mut seen, spec);
        }
        if suffixes.is_empty() {
            return Ok(DashboardBucketCache::default());
        }
        self.ensure_dashboard_cache_read_buckets_inline_safe(&suffixes)
            .await?;

        let mut pipe = redis::pipe();
        for suffix in &suffixes {
            pipe.cmd("HGETALL").arg(self.key(suffix));
        }
        let mut manager = self.manager.clone();
        let buckets: Vec<HashMap<String, String>> = pipe.query_async(&mut manager).await?;
        let buckets = suffixes
            .into_iter()
            .zip(buckets)
            .filter(|(_, bucket)| !bucket.is_empty())
            .collect();
        Ok(DashboardBucketCache { buckets })
    }

    async fn dashboard_bucket_cache_for_suffixes(
        &self,
        suffixes: Vec<String>,
    ) -> anyhow::Result<DashboardBucketCache> {
        if suffixes.is_empty() {
            return Ok(DashboardBucketCache::default());
        }
        self.ensure_dashboard_cache_read_buckets_inline_safe(&suffixes)
            .await?;

        let mut pipe = redis::pipe();
        for suffix in &suffixes {
            pipe.cmd("HGETALL").arg(self.key(suffix));
        }
        let mut manager = self.manager.clone();
        let buckets: Vec<HashMap<String, String>> = pipe.query_async(&mut manager).await?;
        let buckets = suffixes
            .into_iter()
            .zip(buckets)
            .filter(|(_, bucket)| !bucket.is_empty())
            .collect();
        Ok(DashboardBucketCache { buckets })
    }

    async fn ensure_dashboard_cache_read_buckets_inline_safe(
        &self,
        suffixes: &[String],
    ) -> anyhow::Result<()> {
        let cache_read_suffixes = suffixes
            .iter()
            .filter(|suffix| usage_dashboard_cache_read_bucket_suffix(suffix))
            .collect::<Vec<_>>();
        if cache_read_suffixes.is_empty() {
            return Ok(());
        }

        let mut pipe = redis::pipe();
        for suffix in &cache_read_suffixes {
            pipe.cmd("HLEN").arg(self.key(*suffix));
        }
        let mut manager = self.manager.clone();
        let bucket_counts: Vec<usize> = pipe.query_async(&mut manager).await?;
        let mut max_bucket_count = 0usize;
        let mut oversized_suffixes = 0usize;
        for count in bucket_counts {
            max_bucket_count = max_bucket_count.max(count);
            if count > USAGE_CACHE_READ_INLINE_BUCKET_LIMIT {
                oversized_suffixes += 1;
            }
        }
        if oversized_suffixes > 0 {
            tracing::warn!(
                cache_read_bucket_count = cache_read_suffixes.len(),
                oversized_cache_read_buckets = oversized_suffixes,
                max_cache_read_fields = max_bucket_count,
                inline_bucket_limit = USAGE_CACHE_READ_INLINE_BUCKET_LIMIT,
                "Redis usage dashboard cache-read bucket read skipped due to high cardinality"
            );
            anyhow::bail!(
                "Redis usage dashboard cache-read bucket cardinality exceeds inline limit"
            );
        }
        if max_bucket_count >= USAGE_CACHE_READ_INLINE_WARN_BUCKET_LIMIT {
            tracing::debug!(
                cache_read_bucket_count = cache_read_suffixes.len(),
                max_cache_read_fields = max_bucket_count,
                inline_bucket_limit = USAGE_CACHE_READ_INLINE_BUCKET_LIMIT,
                "Redis usage dashboard cache-read bucket read is nearing inline limit"
            );
        }
        Ok(())
    }

    async fn dashboard_series_cache(
        &self,
        specs: &[UsageDashboardWindowSpec],
    ) -> anyhow::Result<DashboardBucketCache> {
        let mut suffixes = Vec::new();
        let mut seen = HashSet::new();
        for spec in specs {
            collect_dashboard_global_bucket_keys(&mut suffixes, &mut seen, spec);
        }
        self.dashboard_bucket_cache_for_suffixes(suffixes).await
    }

    #[cfg(test)]
    fn dashboard_breakdown_from_cache(
        &self,
        spec: &UsageDashboardWindowSpec,
        dimension: &str,
        keys: &[&str],
        label: fn(&str) -> String,
        total_requests: usize,
        cache: &DashboardBucketCache,
    ) -> Vec<UsageBreakdownItem> {
        let mut items = Vec::with_capacity(keys.len());
        for key in keys {
            let totals = cache.sum_bucket(dimension, key, spec);
            let requests = usage_usize(&totals, "total_requests");
            if requests == 0 {
                continue;
            }
            items.push(UsageBreakdownItem {
                key: (*key).to_string(),
                label: label(key),
                requests,
                ratio: usage_ratio(requests, total_requests),
            });
        }
        items.sort_by_key(|item| std::cmp::Reverse(item.requests));
        items
    }

    #[cfg(test)]
    fn dashboard_external_pool_billing_by_pool_from_cache(
        &self,
        spec: &UsageDashboardWindowSpec,
        external_pool_index: &[RedisExternalPoolIndexItem],
        cache: &DashboardBucketCache,
    ) -> Vec<UsageExternalPoolBillingByPool> {
        if external_pool_index.is_empty() {
            return Vec::new();
        }

        let mut items = Vec::with_capacity(external_pool_index.len());
        for pool in external_pool_index {
            let totals = cache.sum_bucket("external_pool", &pool.id, spec);
            let requests = usage_usize(&totals, "external_pool_requests");
            if requests == 0 {
                continue;
            }
            items.push(UsageExternalPoolBillingByPool {
                pool_id: pool.id.parse::<u64>().unwrap_or(0),
                pool_name: pool.label.clone(),
                requests,
                priced_requests: usage_usize(&totals, "external_pool_priced_requests"),
                unpriced_requests: usage_usize(&totals, "external_pool_unpriced_requests"),
                cost_floor_applied_requests: usage_usize(
                    &totals,
                    "external_pool_cost_floor_applied_requests",
                ),
                raw_cost_usd: usage_f64(&totals, "external_pool_raw_cost_usd"),
                shaped_cost_usd: usage_f64(&totals, "external_pool_shaped_cost_usd"),
                uplifted_cost_usd: usage_f64(&totals, "external_pool_uplifted_cost_usd"),
                profit_usd: usage_f64(&totals, "external_pool_profit_usd"),
                reported_cost_usd: usage_f64(&totals, "external_pool_reported_cost_usd"),
                billable_cost_usd: usage_f64(&totals, "external_pool_billable_cost_usd"),
                cost_floor_delta_usd: usage_f64(&totals, "external_pool_cost_floor_delta_usd"),
            });
        }
        items.sort_by(|left, right| {
            right
                .uplifted_cost_usd
                .partial_cmp(&left.uplifted_cost_usd)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| right.requests.cmp(&left.requests))
                .then_with(|| left.pool_id.cmp(&right.pool_id))
        });
        items
    }

    fn dashboard_series_from_cache(
        &self,
        specs: &[UsageDashboardWindowSpec],
        cache: &DashboardBucketCache,
    ) -> Vec<UsageSeriesPoint> {
        let mut points = Vec::with_capacity(specs.len());
        for spec in specs {
            let totals = cache.sum_bucket("global", "all", spec);
            points.push(UsageSeriesPoint {
                key: spec.key.clone(),
                label: spec.label.clone(),
                from: spec.from.to_rfc3339(),
                to: spec.to.to_rfc3339(),
                requests: usage_usize(&totals, "total_requests"),
                success_requests: usage_usize(&totals, "success_requests"),
                error_requests: usage_usize(&totals, "error_requests"),
                total_input_tokens: usage_i64(&totals, "total_input_tokens"),
                billable_input_tokens: usage_i64(&totals, "billable_input_tokens"),
                total_output_tokens: usage_i64(&totals, "total_output_tokens"),
                total_estimated_cost_usd: usage_f64(&totals, "total_estimated_cost_usd"),
                total_original_cost_usd: usage_f64(&totals, "total_original_cost_usd"),
            });
        }
        points
    }

    pub fn key(&self, suffix: impl AsRef<str>) -> String {
        format!(
            "{}:{}",
            self.key_prefix,
            suffix.as_ref().trim_start_matches(':')
        )
    }

    pub async fn ping(&self) -> anyhow::Result<()> {
        let mut manager = self.manager.clone();
        let _: String = redis::cmd("PING").query_async(&mut manager).await?;
        Ok(())
    }

    pub fn note_usage_cleanup_watermark(&self, cutoff: DateTime<Utc>) -> i64 {
        let cutoff_micros = cutoff.timestamp_micros().max(0);
        self.usage_cleanup_watermark_micros
            .fetch_max(cutoff_micros, Ordering::AcqRel)
            .max(cutoff_micros)
    }

    pub async fn advance_usage_cleanup_watermark(
        &self,
        cutoff: DateTime<Utc>,
    ) -> anyhow::Result<i64> {
        self.ensure_observability_usage_store("advance usage cleanup watermark")?;
        let local_cutoff = self.note_usage_cleanup_watermark(cutoff);
        let mut manager = self.manager.clone();
        let effective_cutoff: i64 = redis::cmd("EVAL")
            .arg(ADVANCE_USAGE_CLEANUP_WATERMARK_SCRIPT)
            .arg(1)
            .arg(self.key(USAGE_CLEANUP_WATERMARK_KEY))
            .arg(local_cutoff)
            .query_async(&mut manager)
            .await?;
        self.usage_cleanup_watermark_micros
            .fetch_max(effective_cutoff, Ordering::AcqRel);
        Ok(effective_cutoff)
    }

    pub async fn invalidate_usage_derived_cache(&self) -> anyhow::Result<()> {
        self.ensure_observability_usage_store("invalidate usage derived cache")?;
        let mut manager = self.manager.clone();
        let _: () = manager
            .set(self.key(USAGE_DERIVED_CACHE_INVALIDATED_KEY), "1")
            .await?;
        Ok(())
    }

    async fn usage_derived_cache_is_invalidated(&self) -> anyhow::Result<bool> {
        let mut manager = self.manager.clone();
        manager
            .exists(self.key(USAGE_DERIVED_CACHE_INVALIDATED_KEY))
            .await
            .map_err(Into::into)
    }

    async fn usage_totals_if_valid(&self) -> anyhow::Result<Option<HashMap<String, String>>> {
        let mut manager = self.manager.clone();
        let totals: HashMap<String, String> = redis::cmd("EVAL")
            .arg(READ_USAGE_TOTALS_IF_VALID_SCRIPT)
            .arg(2)
            .arg(self.key(USAGE_DERIVED_CACHE_INVALIDATED_KEY))
            .arg(self.key(USAGE_SUMMARY_TOTALS_KEY))
            .query_async(&mut manager)
            .await?;
        if totals.is_empty() {
            Ok(None)
        } else {
            Ok(Some(totals))
        }
    }

    pub fn runtime_config_changed_channel(&self) -> String {
        self.key("events:runtime_config_changed")
    }

    pub fn credentials_changed_channel(&self) -> String {
        self.key("events:credentials_changed")
    }

    pub fn dispatch_wakeup_channel(&self) -> String {
        self.key("events:dispatch_wakeup")
    }

    pub fn external_pool_data_changed_channel(&self) -> String {
        self.key("events:external_pool_data_changed")
    }

    pub async fn subscribe_runtime_events(&self) -> anyhow::Result<PubSub> {
        let mut pubsub = self.client.get_async_pubsub().await?;
        pubsub
            .subscribe(self.runtime_config_changed_channel())
            .await?;
        pubsub.subscribe(self.credentials_changed_channel()).await?;
        pubsub.subscribe(self.dispatch_wakeup_channel()).await?;
        pubsub
            .subscribe(self.external_pool_data_changed_channel())
            .await?;
        Ok(pubsub)
    }

    async fn publish_event(&self, channel: String, payload: impl AsRef<str>) -> anyhow::Result<()> {
        let mut manager = self.manager.clone();
        let _: i64 = manager.publish(channel, payload.as_ref()).await?;
        Ok(())
    }

    pub async fn publish_runtime_config_changed(
        &self,
        payload: impl AsRef<str>,
    ) -> anyhow::Result<()> {
        self.publish_event(self.runtime_config_changed_channel(), payload)
            .await
    }

    pub async fn publish_credentials_changed(
        &self,
        payload: impl AsRef<str>,
    ) -> anyhow::Result<()> {
        self.publish_event(self.credentials_changed_channel(), payload)
            .await
    }

    pub async fn publish_dispatch_wakeup(&self, payload: impl AsRef<str>) -> anyhow::Result<()> {
        self.publish_event(self.dispatch_wakeup_channel(), payload)
            .await
    }

    pub async fn publish_external_pool_data_changed(
        &self,
        reason: &str,
        pool_id: Option<u64>,
    ) -> anyhow::Result<u64> {
        const SCRIPT: &str = r#"
            local generation = redis.call('INCR', KEYS[1])
            local payload = cjson.encode({
                generation = generation,
                reason = ARGV[1],
                poolId = ARGV[2]
            })
            redis.call('PUBLISH', KEYS[2], payload)
            return generation
        "#;
        let mut manager = self.manager.clone();
        let generation: u64 = redis::cmd("EVAL")
            .arg(SCRIPT)
            .arg(2)
            .arg(self.key(EXTERNAL_POOL_DATA_GENERATION_KEY))
            .arg(self.external_pool_data_changed_channel())
            .arg(reason)
            .arg(pool_id.map(|id| id.to_string()).unwrap_or_default())
            .query_async(&mut manager)
            .await?;
        Ok(generation)
    }

    pub async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        key: impl AsRef<str>,
    ) -> anyhow::Result<Option<T>> {
        let mut manager = self.manager.clone();
        let value: Option<String> = manager.get(self.key(key)).await?;
        let Some(value) = value else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_str(&value)?))
    }

    pub async fn set_json<T: serde::Serialize>(
        &self,
        key: impl AsRef<str>,
        value: &T,
        ttl_secs: usize,
    ) -> anyhow::Result<()> {
        let encoded = serde_json::to_string(value)?;
        let mut manager = self.manager.clone();
        let _: () = manager
            .set_ex(self.key(key), encoded, ttl_secs as u64)
            .await?;
        Ok(())
    }

    pub async fn del(&self, key: impl AsRef<str>) -> anyhow::Result<()> {
        let mut manager = self.manager.clone();
        let _: () = manager.del(self.key(key)).await?;
        Ok(())
    }

    pub async fn del_many(&self, keys: &[String]) -> anyhow::Result<usize> {
        if keys.is_empty() {
            return Ok(0);
        }
        let full_keys = keys.iter().map(|key| self.key(key)).collect::<Vec<_>>();
        let mut manager = self.manager.clone();
        let deleted: usize = redis::cmd("DEL")
            .arg(full_keys)
            .query_async(&mut manager)
            .await?;
        Ok(deleted)
    }

    pub async fn unlink_count(&self, key: impl AsRef<str>) -> anyhow::Result<(usize, bool)> {
        let mut manager = self.manager.clone();
        let key = self.key(key);
        unlink_keys_with_fallback(&mut manager, std::slice::from_ref(&key)).await
    }

    pub async fn del_pattern(&self, pattern: impl AsRef<str>) -> anyhow::Result<usize> {
        let stats = self.delete_pattern_bounded(pattern, None).await?;
        if stats.pass_limit_reached {
            anyhow::bail!(
                "Redis pattern delete did not converge after {} passes",
                REDIS_PATTERN_DELETE_MAX_PASSES
            );
        }
        Ok(stats.deleted_keys)
    }

    pub async fn delete_pattern_bounded(
        &self,
        pattern: impl AsRef<str>,
        cancel: Option<&AtomicBool>,
    ) -> anyhow::Result<RedisPatternDeleteStats> {
        let full_pattern = self.key(pattern);
        let mut manager = self.manager.clone();
        let mut stats = RedisPatternDeleteStats::default();
        let mut converged = false;
        for _ in 0..REDIS_PATTERN_DELETE_MAX_PASSES {
            if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
                stats.cancelled = true;
                break;
            }
            stats.scan_passes = stats.scan_passes.saturating_add(1);
            let mut cursor = 0u64;
            let mut deleted_this_pass = 0usize;
            loop {
                let (next_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                    .arg(cursor)
                    .arg("MATCH")
                    .arg(&full_pattern)
                    .arg("COUNT")
                    .arg(REDIS_PATTERN_DELETE_SCAN_COUNT)
                    .query_async(&mut manager)
                    .await?;
                stats.scan_calls = stats.scan_calls.saturating_add(1);
                for chunk in keys.chunks(REDIS_PATTERN_DELETE_COMMAND_KEY_LIMIT) {
                    if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
                        stats.cancelled = true;
                        break;
                    }
                    let (removed, used_del_fallback) =
                        unlink_keys_with_fallback(&mut manager, chunk).await?;
                    deleted_this_pass = deleted_this_pass.saturating_add(removed);
                    stats.deleted_keys = stats.deleted_keys.saturating_add(removed);
                    stats.delete_commands = stats.delete_commands.saturating_add(1);
                    stats.max_command_keys = stats.max_command_keys.max(chunk.len());
                    stats.used_del_fallback |= used_del_fallback;
                    tokio::task::yield_now().await;
                }
                if stats.cancelled {
                    break;
                }
                cursor = next_cursor;
                if cursor == 0 {
                    break;
                }
                tokio::task::yield_now().await;
            }
            if stats.cancelled {
                break;
            }
            if deleted_this_pass == 0 {
                converged = true;
                break;
            }
            tokio::task::yield_now().await;
        }
        if !stats.cancelled && !converged {
            stats.pass_limit_reached = true;
        }
        Ok(stats)
    }

    #[cfg(test)]
    pub async fn incr_with_ttl(
        &self,
        key: impl AsRef<str>,
        ttl_secs: usize,
    ) -> anyhow::Result<u64> {
        let script = r#"
            local value = redis.call('INCR', KEYS[1])
            if value == 1 then
                redis.call('EXPIRE', KEYS[1], ARGV[1])
            end
            return value
        "#;
        let mut manager = self.manager.clone();
        let value: u64 = redis::cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(self.key(key))
            .arg(ttl_secs.max(1))
            .query_async(&mut manager)
            .await?;
        Ok(value)
    }

    /// Counts an auto-disable failure and elects at most one worker for the
    /// pool revision that crossed the configured threshold.
    pub async fn claim_external_pool_auto_disable_transition(
        &self,
        pool_id: u64,
        pool_revision: u64,
        reason: &str,
        threshold: u64,
        window_secs: usize,
        claim_ttl_secs: usize,
    ) -> anyhow::Result<(u64, bool)> {
        let counter_key = format!("external_pool:{}:auto_disable_failures:{}", pool_id, reason);
        let claim_key = format!(
            "external_pool:{}:auto_disable_transition:{}:{}",
            pool_id, pool_revision, reason
        );
        let script = r#"
            local count = redis.call('INCR', KEYS[1])
            local ttl = redis.call('TTL', KEYS[1])
            if count == 1 or ttl < 0 then
                redis.call('EXPIRE', KEYS[1], ARGV[1])
            end
            if count < tonumber(ARGV[2]) then
                return {count, 0}
            end
            local claimed = redis.call('SET', KEYS[2], ARGV[3], 'NX', 'EX', ARGV[4])
            if claimed then
                return {count, 1}
            end
            return {count, 0}
        "#;
        let mut manager = self.manager.clone();
        let result: Vec<i64> = redis::cmd("EVAL")
            .arg(script)
            .arg(2)
            .arg(self.key(counter_key))
            .arg(self.key(claim_key))
            .arg(window_secs.max(1))
            .arg(threshold.max(1))
            .arg(uuid::Uuid::new_v4().to_string())
            .arg(claim_ttl_secs.max(1))
            .query_async(&mut manager)
            .await?;
        let count = result
            .first()
            .copied()
            .ok_or_else(|| anyhow::anyhow!("Redis 未返回外部池自动禁用失败计数"))?
            .max(0) as u64;
        let claimed = result
            .get(1)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("Redis 未返回外部池自动禁用 transition claim"))?
            != 0;
        Ok((count, claimed))
    }

    pub async fn set_nx_ex(
        &self,
        key: impl AsRef<str>,
        value: impl AsRef<str>,
        ttl_secs: usize,
    ) -> anyhow::Result<bool> {
        let mut manager = self.manager.clone();
        let result: Option<String> = redis::cmd("SET")
            .arg(self.key(key))
            .arg(value.as_ref())
            .arg("NX")
            .arg("EX")
            .arg(ttl_secs.max(1))
            .query_async(&mut manager)
            .await?;
        Ok(result.as_deref() == Some("OK"))
    }

    pub async fn release_lock(
        &self,
        key: impl AsRef<str>,
        value: impl AsRef<str>,
    ) -> anyhow::Result<bool> {
        let script = r#"
            if redis.call('GET', KEYS[1]) == ARGV[1] then
                return redis.call('DEL', KEYS[1])
            end
            return 0
        "#;
        let mut manager = self.manager.clone();
        let removed: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(self.key(key))
            .arg(value.as_ref())
            .query_async(&mut manager)
            .await?;
        Ok(removed > 0)
    }

    pub async fn record_usage_summary(&self, record: &UsageRecord) -> anyhow::Result<bool> {
        self.ensure_observability_usage_store("record usage summary")?;
        self.record_usage_summary_with_cache_read_limit(
            record,
            USAGE_CACHE_READ_INLINE_BUCKET_LIMIT,
        )
        .await
    }

    async fn record_usage_summary_with_cache_read_limit(
        &self,
        record: &UsageRecord,
        cache_read_bucket_limit: usize,
    ) -> anyhow::Result<bool> {
        let created_at = DateTime::parse_from_rfc3339(&record.created_at)
            .map(|created_at| created_at.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());
        let permit = self
            .usage_summary_write_gate
            .acquire()
            .await
            .map_err(|_| anyhow::anyhow!("Redis usage summary 写入 gate 已关闭"))?;
        let snapshot_member = usage_dimension_hash(&record.id);
        let seen_key = self.key(format!("usage:summary:seen:{snapshot_member}"));
        let snapshot_key = self.key(usage_record_key(&snapshot_member));
        let snapshot_index_key = self.key(USAGE_RECORDS_INDEX_KEY);
        let snapshot_item_key_prefix = self.key("usage:records:item:");
        let snapshot_encoded = serde_json::to_string(record)?;
        let snapshot_cutoff_ms = Utc::now()
            .timestamp_millis()
            .saturating_sub((USAGE_RECORDS_TTL_SECS as i64).saturating_mul(1000));
        let mut manager = self.manager.clone();
        let realtime_key = usage_realtime_bucket_key(created_at.timestamp());
        let totals_key = self.key(USAGE_SUMMARY_TOTALS_KEY);
        let cache_read_key = self.key(USAGE_SUMMARY_CACHE_READ_KEY);
        let cache_read_index_key = self.key(USAGE_SUMMARY_CACHE_READ_INDEX_KEY);
        let cache_read_bucket = record.cache_read_input_tokens.max(0).to_string();
        let dashboard_cache_read_key = self.key(usage_dashboard_cache_read_bucket_key(
            usage_dashboard_hour_start(created_at.clone()).timestamp(),
        ));
        let realtime_key = self.key(realtime_key);

        let mut pipe = GuardedUsagePipeline::default();
        pipe.atomic()
            .cmd("HINCRBY")
            .arg(&totals_key)
            .arg("total_requests")
            .arg(1i64)
            .cmd("HINCRBY")
            .arg(&totals_key)
            .arg(if record.status == UsageRecordStatus::Success {
                "success_requests"
            } else {
                "error_requests"
            })
            .arg(1i64)
            .cmd("HINCRBY")
            .arg(&totals_key)
            .arg(if record.stream {
                "stream_requests"
            } else {
                "non_stream_requests"
            })
            .arg(1i64)
            .cmd("HINCRBY")
            .arg(&totals_key)
            .arg("total_input_tokens")
            .arg(record.total_input_tokens as i64)
            .cmd("HINCRBY")
            .arg(&totals_key)
            .arg("total_output_tokens")
            .arg(record.output_tokens as i64)
            .cmd("HINCRBY")
            .arg(&totals_key)
            .arg("total_cache_read_input_tokens")
            .arg(record.cache_read_input_tokens as i64)
            .cmd("HINCRBY")
            .arg(&totals_key)
            .arg("total_cache_creation_input_tokens")
            .arg(record.cache_creation_input_tokens as i64)
            .cmd("HINCRBYFLOAT")
            .arg(&totals_key)
            .arg("total_estimated_cost_usd")
            .arg(record.estimated_cost_usd)
            .cmd("HINCRBYFLOAT")
            .arg(&totals_key)
            .arg("total_original_cost_usd")
            .arg(record.original_cost_usd)
            .cmd("HINCRBY")
            .arg(&totals_key)
            .arg(if record.pricing_available {
                "priced_requests"
            } else {
                "unpriced_requests"
            })
            .arg(1i64)
            .cmd("HINCRBY")
            .arg(&totals_key)
            .arg("simulated_requests")
            .arg(if record.simulated { 1i64 } else { 0i64 })
            .cmd("HINCRBY")
            .arg(&totals_key)
            .arg("upstream_metadata_requests")
            .arg(if record.usage_source == UsageSource::UpstreamMetadata {
                1i64
            } else {
                0i64
            })
            .cmd("HINCRBY")
            .arg(&totals_key)
            .arg("local_prompt_cache_requests")
            .arg(if record.usage_source == UsageSource::LocalPromptCache {
                1i64
            } else {
                0i64
            })
            .cmd("HINCRBY")
            .arg(&totals_key)
            .arg("local_prompt_cache_input_tokens")
            .arg(if record.usage_source == UsageSource::LocalPromptCache {
                record.total_input_tokens as i64
            } else {
                0i64
            })
            .cmd("HINCRBY")
            .arg(&totals_key)
            .arg("local_prompt_cache_read_input_tokens")
            .arg(if record.usage_source == UsageSource::LocalPromptCache {
                record.cache_read_input_tokens as i64
            } else {
                0i64
            })
            .cmd("HINCRBY")
            .arg(&totals_key)
            .arg("local_prompt_cache_creation_input_tokens")
            .arg(if record.usage_source == UsageSource::LocalPromptCache {
                record.cache_creation_input_tokens as i64
            } else {
                0i64
            })
            .cmd("HINCRBY")
            .arg(&cache_read_key)
            .arg(&cache_read_bucket)
            .arg(1i64)
            .cmd("ZADD")
            .arg(&cache_read_index_key)
            .arg(record.cache_read_input_tokens)
            .arg(&cache_read_bucket)
            .cmd("HINCRBY")
            .arg(&realtime_key)
            .arg("requests")
            .arg(1i64)
            .cmd("HINCRBY")
            .arg(&realtime_key)
            .arg("input_tokens")
            .arg(record.total_input_tokens as i64)
            .cmd("HINCRBY")
            .arg(&realtime_key)
            .arg("output_tokens")
            .arg(record.output_tokens as i64)
            .cmd("HINCRBY")
            .arg(&realtime_key)
            .arg("billable_input_tokens")
            .arg(record.billable_input_tokens as i64)
            .cmd("EXPIRE")
            .arg(&realtime_key)
            .arg(USAGE_REALTIME_BUCKET_TTL_SECS);

        append_external_pool_usage_summary(&mut pipe, &totals_key, record);
        append_usage_top_aggregate(
            &mut pipe,
            &self.key(USAGE_SUMMARY_TOP_CREDENTIALS_KEY),
            record.credential_id.map(|id| id.to_string()),
            record.credential_label.clone(),
            record,
            |key| self.key(usage_top_metrics_key("credential", key)),
        );
        append_usage_top_aggregate(
            &mut pipe,
            &self.key(USAGE_SUMMARY_TOP_CONVERSATIONS_KEY),
            record.conversation_id.clone(),
            None,
            record,
            |key| self.key(usage_top_metrics_key("conversation", key)),
        );
        append_usage_dashboard_rollups(&mut pipe, self, record, created_at);
        append_usage_dashboard_top_aggregate(
            &mut pipe,
            &self.key(USAGE_DASHBOARD_TOP_MODELS_KEY),
            "model",
            Some(non_empty_or_unknown(&record.model)),
            None,
            record,
            |key| self.key(usage_dashboard_top_metrics_key("model", key)),
        );
        append_usage_dashboard_top_aggregate(
            &mut pipe,
            &self.key(USAGE_DASHBOARD_TOP_CREDENTIALS_KEY),
            "credential",
            record.credential_id.map(|id| id.to_string()),
            record.credential_label.clone(),
            record,
            |key| self.key(usage_dashboard_top_metrics_key("credential", key)),
        );
        append_usage_dashboard_top_aggregate(
            &mut pipe,
            &self.key(USAGE_DASHBOARD_TOP_ENDPOINTS_KEY),
            "endpoint",
            Some(non_empty_or_unknown(&record.endpoint)),
            None,
            record,
            |key| self.key(usage_dashboard_top_metrics_key("endpoint", key)),
        );
        append_usage_dashboard_top_aggregate(
            &mut pipe,
            &self.key(USAGE_DASHBOARD_TOP_EXTERNAL_POOLS_KEY),
            "external_pool",
            record.external_pool_id.map(|id| id.to_string()),
            record.external_pool_name.clone(),
            record,
            |key| self.key(usage_dashboard_top_metrics_key("external_pool", key)),
        );
        if record.status != UsageRecordStatus::Success {
            append_usage_dashboard_top_aggregate(
                &mut pipe,
                &self.key(USAGE_DASHBOARD_TOP_ERRORS_KEY),
                "error",
                Some(
                    record
                        .error_type
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .unwrap_or(usage_status_value(record.status))
                        .to_string(),
                ),
                record
                    .error_message
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string),
                record,
                |key| self.key(usage_dashboard_top_metrics_key("error", key)),
            );
        }

        let local_cutoff = self.usage_cleanup_watermark_micros.load(Ordering::Acquire);
        #[cfg(test)]
        self.usage_summary_write_round_trips
            .fetch_add(1, Ordering::Relaxed);
        let guarded_result = pipe
            .query_guarded_idempotent(
                &mut manager,
                &self.key(USAGE_CLEANUP_WATERMARK_KEY),
                &self.key(USAGE_DERIVED_CACHE_INVALIDATED_KEY),
                &seen_key,
                &cache_read_key,
                &dashboard_cache_read_key,
                created_at.timestamp_micros().max(0),
                local_cutoff,
                USAGE_SUMMARY_SEEN_TTL_SECS,
                &cache_read_bucket,
                cache_read_bucket_limit,
                &snapshot_key,
                &snapshot_index_key,
                USAGE_RECORDS_TTL_SECS,
                &snapshot_member,
                created_at.timestamp_millis(),
                snapshot_cutoff_ms,
                USAGE_RECORDS_MAX_CACHED,
                USAGE_RECORDS_TRIM_BATCH,
                &snapshot_item_key_prefix,
                &snapshot_encoded,
            )
            .await;
        drop(permit);
        let (accepted, effective_cutoff) = guarded_result?;
        self.usage_cleanup_watermark_micros
            .fetch_max(effective_cutoff, Ordering::AcqRel);
        Ok(accepted)
    }

    #[cfg(test)]
    async fn record_usage_record_snapshot(
        &self,
        record: &UsageRecord,
        created_at: DateTime<Utc>,
    ) -> anyhow::Result<bool> {
        self.record_usage_record_snapshot_with_limits(
            record,
            created_at,
            USAGE_RECORDS_MAX_CACHED,
            USAGE_RECORDS_TRIM_BATCH,
        )
        .await
    }

    #[cfg(test)]
    async fn record_usage_record_snapshot_with_limits(
        &self,
        record: &UsageRecord,
        created_at: DateTime<Utc>,
        max_cached: usize,
        trim_batch: usize,
    ) -> anyhow::Result<bool> {
        let member = usage_dimension_hash(&record.id);
        let record_key = self.key(usage_record_key(&member));
        let index_key = self.key(USAGE_RECORDS_INDEX_KEY);
        let item_key_prefix = self.key("usage:records:item:");
        let encoded = serde_json::to_string(record)?;
        let cutoff_ms = Utc::now()
            .timestamp_millis()
            .saturating_sub((USAGE_RECORDS_TTL_SECS as i64).saturating_mul(1000));
        let mut manager = self.manager.clone();
        let script = r#"
            local ttl = tonumber(ARGV[1])
            local member = ARGV[2]
            local score = tonumber(ARGV[3])
            local cutoff_ms = tonumber(ARGV[4])
            local max_cached = tonumber(ARGV[5])
            local trim_batch = tonumber(ARGV[6])
            local item_key_prefix = ARGV[7]
            local encoded = ARGV[8]
            local local_cutoff = tonumber(ARGV[9]) or 0
            local record_micros = tonumber(ARGV[10]) or 0
            local persisted_cutoff = tonumber(redis.call('GET', KEYS[3]) or '0')
            local effective_cutoff = persisted_cutoff
            if local_cutoff > effective_cutoff then
                redis.call('SET', KEYS[3], local_cutoff)
                effective_cutoff = local_cutoff
            end
            if record_micros < effective_cutoff then
                return {0, effective_cutoff}
            end
            if redis.call('EXISTS', KEYS[4]) == 1 then
                return {0, effective_cutoff}
            end

            redis.call('SETEX', KEYS[1], ttl, encoded)
            redis.call('ZADD', KEYS[2], score, member)
            redis.call('EXPIRE', KEYS[2], ttl)

            local expired = redis.call('ZRANGEBYSCORE', KEYS[2], '-inf', cutoff_ms, 'LIMIT', 0, trim_batch)
            if #expired > 0 then
                for i, old_member in ipairs(expired) do
                    redis.call('DEL', item_key_prefix .. old_member)
                end
                redis.call('ZREM', KEYS[2], unpack(expired))
            end

            local overflow = redis.call('ZCARD', KEYS[2]) - max_cached
            if overflow > 0 then
                local limit = overflow
                if limit > trim_batch then
                    limit = trim_batch
                end
                local old_members = redis.call('ZRANGE', KEYS[2], 0, limit - 1)
                if #old_members > 0 then
                    for i, old_member in ipairs(old_members) do
                        redis.call('DEL', item_key_prefix .. old_member)
                    end
                    redis.call('ZREM', KEYS[2], unpack(old_members))
                end
            end

            return {1, effective_cutoff}
        "#;
        let local_cutoff = self.usage_cleanup_watermark_micros.load(Ordering::Acquire);
        let (accepted, effective_cutoff): (i64, i64) = redis::cmd("EVAL")
            .arg(script)
            .arg(4)
            .arg(&record_key)
            .arg(&index_key)
            .arg(self.key(USAGE_CLEANUP_WATERMARK_KEY))
            .arg(self.key(USAGE_DERIVED_CACHE_INVALIDATED_KEY))
            .arg(USAGE_RECORDS_TTL_SECS)
            .arg(&member)
            .arg(created_at.timestamp_millis())
            .arg(cutoff_ms)
            .arg(max_cached.max(1))
            .arg(trim_batch.max(1))
            .arg(item_key_prefix)
            .arg(encoded)
            .arg(local_cutoff)
            .arg(created_at.timestamp_micros().max(0))
            .query_async(&mut manager)
            .await?;
        self.usage_cleanup_watermark_micros
            .fetch_max(effective_cutoff, Ordering::AcqRel);
        Ok(accepted != 0)
    }

    pub async fn usage_records_page(
        &self,
        query: UsageRecordQuery,
        page: usize,
        limit: usize,
    ) -> anyhow::Result<Option<UsageRecordsPageResult>> {
        self.ensure_observability_usage_store("read usage record snapshots")?;
        if self.usage_derived_cache_is_invalidated().await? {
            return Ok(None);
        }
        let page = page.max(1);
        let limit = if limit == 0 { 20 } else { limit.min(1000) };
        if let Some(request_id) = query
            .request_id
            .as_deref()
            .map(str::trim)
            .filter(|request_id| !request_id.is_empty())
        {
            if page > 1 {
                return Ok(Some(UsageRecordsPageResult {
                    page,
                    limit,
                    has_next: false,
                    records: Vec::new(),
                }));
            }
            let mut manager = self.manager.clone();
            let value: Option<String> = manager.get(self.key(usage_record_key(request_id))).await?;
            let mut records = Vec::new();
            if let Some(value) = value {
                if let Ok(record) = serde_json::from_str::<UsageRecord>(&value) {
                    if usage_record_matches_query(&record, &query) {
                        records.push(record);
                    }
                }
            }
            return Ok(Some(UsageRecordsPageResult {
                page,
                limit,
                has_next: false,
                records,
            }));
        }
        let target_start = page.saturating_sub(1).saturating_mul(limit);
        let target_len = limit.saturating_add(1);
        let target_matches = target_start.saturating_add(target_len);

        let mut manager = self.manager.clone();
        let index_key = self.key(USAGE_RECORDS_INDEX_KEY);
        let total_indexed: usize = manager.zcard(&index_key).await?;
        if total_indexed == 0 {
            return Ok(None);
        }
        if target_start >= total_indexed {
            return Ok(None);
        }

        let mut offset = 0usize;
        let mut scanned = 0usize;
        let mut matched = Vec::with_capacity(target_matches.min(256));
        while offset < total_indexed
            && matched.len() < target_matches
            && scanned < USAGE_RECORDS_QUERY_SCAN_LIMIT
        {
            let batch_size = (target_matches.saturating_sub(matched.len()))
                .max(limit)
                .min(250);
            let stop = offset
                .saturating_add(batch_size)
                .saturating_sub(1)
                .min(total_indexed.saturating_sub(1));
            let members: Vec<String> = manager
                .zrevrange(&index_key, offset as isize, stop as isize)
                .await?;
            if members.is_empty() {
                break;
            }
            scanned = scanned.saturating_add(members.len());

            let mut pipe = redis::pipe();
            for member in &members {
                pipe.cmd("GET").arg(self.key(usage_record_key(member)));
            }
            let values: Vec<Option<String>> = pipe.query_async(&mut manager).await?;
            for value in values.into_iter().flatten() {
                let Ok(record) = serde_json::from_str::<UsageRecord>(&value) else {
                    continue;
                };
                if usage_record_matches_query(&record, &query) {
                    matched.push(record);
                    if matched.len() >= target_matches {
                        break;
                    }
                }
            }

            offset = stop.saturating_add(1);
        }

        if matched.len() < target_matches
            && offset < total_indexed
            && scanned >= USAGE_RECORDS_QUERY_SCAN_LIMIT
        {
            return Ok(None);
        }

        let mut records = matched
            .into_iter()
            .skip(target_start)
            .take(target_len)
            .collect::<Vec<_>>();
        let has_next = records.len() > limit;
        if has_next {
            records.truncate(limit);
        }

        Ok(Some(UsageRecordsPageResult {
            page,
            limit,
            has_next,
            records,
        }))
    }

    pub async fn usage_summary(
        &self,
        high_cache_threshold: i32,
    ) -> anyhow::Result<Option<UsageSummary>> {
        self.ensure_observability_usage_store("read usage summary")?;
        let Some(totals) = self.usage_totals_if_valid().await? else {
            return Ok(None);
        };

        let Some(high_cache_requests) = self.high_cache_requests(high_cache_threshold).await?
        else {
            tracing::warn!(
                high_cache_threshold,
                "Redis usage summary cache-read bucket cardinality exceeds inline limit; falling back to PgSQL rollup"
            );
            return Ok(None);
        };

        let realtime = self.usage_realtime_stats().await?;
        let top_credentials = self
            .usage_top_aggregates(USAGE_SUMMARY_TOP_CREDENTIALS_KEY, "credential")
            .await?;
        let top_conversations = self
            .usage_top_aggregates(USAGE_SUMMARY_TOP_CONVERSATIONS_KEY, "conversation")
            .await?;

        Ok(Some(UsageSummary {
            total_requests: usage_usize(&totals, "total_requests"),
            success_requests: usage_usize(&totals, "success_requests"),
            error_requests: usage_usize(&totals, "error_requests"),
            high_cache_requests,
            total_input_tokens: usage_i64(&totals, "total_input_tokens"),
            total_output_tokens: usage_i64(&totals, "total_output_tokens"),
            total_cache_read_input_tokens: usage_i64(&totals, "total_cache_read_input_tokens"),
            total_cache_creation_input_tokens: usage_i64(
                &totals,
                "total_cache_creation_input_tokens",
            ),
            total_estimated_cost_usd: usage_f64(&totals, "total_estimated_cost_usd"),
            total_original_cost_usd: usage_f64(&totals, "total_original_cost_usd"),
            priced_requests: usage_usize(&totals, "priced_requests"),
            unpriced_requests: usage_usize(&totals, "unpriced_requests"),
            local_prompt_cache_requests: usage_usize(&totals, "local_prompt_cache_requests"),
            local_prompt_cache_input_tokens: usage_i64(&totals, "local_prompt_cache_input_tokens"),
            local_prompt_cache_read_input_tokens: usage_i64(
                &totals,
                "local_prompt_cache_read_input_tokens",
            ),
            local_prompt_cache_creation_input_tokens: usage_i64(
                &totals,
                "local_prompt_cache_creation_input_tokens",
            ),
            simulated_requests: usage_usize(&totals, "simulated_requests"),
            upstream_metadata_requests: usage_usize(&totals, "upstream_metadata_requests"),
            external_pool_billing: UsageExternalPoolBillingSummary {
                requests: usage_usize(&totals, "external_pool_requests"),
                priced_requests: usage_usize(&totals, "external_pool_priced_requests"),
                unpriced_requests: usage_usize(&totals, "external_pool_unpriced_requests"),
                cost_floor_applied_requests: usage_usize(
                    &totals,
                    "external_pool_cost_floor_applied_requests",
                ),
                raw_cost_usd: usage_f64(&totals, "external_pool_raw_cost_usd"),
                shaped_cost_usd: usage_f64(&totals, "external_pool_shaped_cost_usd"),
                uplifted_cost_usd: usage_f64(&totals, "external_pool_uplifted_cost_usd"),
                profit_usd: usage_f64(&totals, "external_pool_profit_usd"),
                reported_cost_usd: usage_f64(&totals, "external_pool_reported_cost_usd"),
                billable_cost_usd: usage_f64(&totals, "external_pool_billable_cost_usd"),
                cost_floor_delta_usd: usage_f64(&totals, "external_pool_cost_floor_delta_usd"),
            },
            realtime,
            top_credentials,
            top_conversations,
        }))
    }

    async fn high_cache_requests(
        &self,
        high_cache_threshold: i32,
    ) -> anyhow::Result<Option<usize>> {
        let mut manager = self.manager.clone();
        let cache_read_key = self.key(USAGE_SUMMARY_CACHE_READ_KEY);
        let cache_read_bucket_count: usize = manager.hlen(&cache_read_key).await?;
        if cache_read_bucket_count == 0 {
            return Ok(Some(0));
        }
        if cache_read_bucket_count > USAGE_CACHE_READ_INLINE_BUCKET_LIMIT {
            tracing::warn!(
                high_cache_threshold,
                cache_read_buckets = cache_read_bucket_count,
                inline_bucket_limit = USAGE_CACHE_READ_INLINE_BUCKET_LIMIT,
                "Redis usage high-cache summary read skipped due to high cardinality"
            );
            return Ok(None);
        }

        let started_at = std::time::Instant::now();
        let cache_read_totals: HashMap<String, String> = manager.hgetall(&cache_read_key).await?;
        let high_cache_requests = cache_read_totals
            .iter()
            .filter_map(|(tokens, requests)| {
                let tokens = tokens.parse::<i32>().ok()?;
                let requests = requests.parse::<usize>().ok()?;
                (tokens >= high_cache_threshold).then_some(requests)
            })
            .sum();

        let elapsed_ms = started_at.elapsed().as_millis();
        if elapsed_ms >= 10 || cache_read_bucket_count >= USAGE_CACHE_READ_INLINE_WARN_BUCKET_LIMIT
        {
            tracing::debug!(
                high_cache_threshold,
                cache_read_buckets = cache_read_bucket_count,
                inline_bucket_limit = USAGE_CACHE_READ_INLINE_BUCKET_LIMIT,
                elapsed_ms = elapsed_ms.min(u128::from(u64::MAX)) as u64,
                "Redis usage high-cache summary read"
            );
        }
        Ok(Some(high_cache_requests))
    }

    #[cfg(test)]
    pub async fn usage_dashboard(
        &self,
        timezone: Option<&str>,
        high_cache_threshold: i32,
    ) -> anyhow::Result<Option<UsageDashboardResponse>> {
        self.ensure_observability_usage_store("read usage dashboard")?;
        let started_at = std::time::Instant::now();
        let Some(totals) = self.usage_totals_if_valid().await? else {
            return Ok(None);
        };
        let lifetime_requests = usage_usize(&totals, "total_requests");

        let now = Utc::now();
        let (timezone, offset) = usage_dashboard_timezone(timezone);
        let window_specs = usage_dashboard_windows(now, offset);
        let hourly_specs = usage_dashboard_hourly_windows(now, offset);
        let daily_specs = usage_dashboard_daily_windows(now, offset);
        let external_pool_index = self.dashboard_external_pool_index().await?;
        let bucket_cache = self
            .dashboard_bucket_cache(
                &window_specs,
                &hourly_specs,
                &daily_specs,
                &external_pool_index,
            )
            .await?;
        let mut windows = Vec::with_capacity(window_specs.len());
        for spec in &window_specs {
            windows.push(self.dashboard_window_from_cache(
                spec,
                high_cache_threshold,
                &external_pool_index,
                &bucket_cache,
            ));
        }

        let has_window_data = windows
            .iter()
            .any(|window| window.summary.total_requests > 0);
        if lifetime_requests > 0 && !has_window_data {
            return Ok(None);
        }

        let top = UsageDashboardTop {
            window_key: "lifetime".to_string(),
            models: self
                .dashboard_top_aggregates(USAGE_DASHBOARD_TOP_MODELS_KEY, "model")
                .await?,
            credentials: self
                .dashboard_top_aggregates(USAGE_DASHBOARD_TOP_CREDENTIALS_KEY, "credential")
                .await?,
            endpoints: self
                .dashboard_top_aggregates(USAGE_DASHBOARD_TOP_ENDPOINTS_KEY, "endpoint")
                .await?,
            errors: self
                .dashboard_top_aggregates(USAGE_DASHBOARD_TOP_ERRORS_KEY, "error")
                .await?,
        };

        let elapsed_ms = started_at.elapsed().as_millis();
        if elapsed_ms >= 250 {
            tracing::warn!(
                elapsed_ms = elapsed_ms.min(u128::from(u64::MAX)) as u64,
                external_pool_count = external_pool_index.len(),
                "Redis usage dashboard read was slow"
            );
        } else {
            tracing::debug!(
                elapsed_ms = elapsed_ms.min(u128::from(u64::MAX)) as u64,
                external_pool_count = external_pool_index.len(),
                "Redis usage dashboard read"
            );
        }

        Ok(Some(UsageDashboardResponse {
            generated_at: now.to_rfc3339(),
            timezone,
            windows,
            series: UsageDashboardSeries {
                hourly_24h: self.dashboard_series_from_cache(&hourly_specs, &bucket_cache),
                daily_7d: self.dashboard_series_from_cache(&daily_specs, &bucket_cache),
            },
            top,
        }))
    }

    pub async fn usage_dashboard_series_only(
        &self,
        timezone: Option<&str>,
    ) -> anyhow::Result<Option<(String, String, UsageDashboardSeries)>> {
        self.ensure_observability_usage_store("read usage dashboard series")?;
        let Some(_totals) = self.usage_totals_if_valid().await? else {
            return Ok(None);
        };

        let now = Utc::now();
        let (timezone, offset) = usage_dashboard_timezone(timezone);
        let hourly_specs = usage_dashboard_hourly_windows(now, offset);
        let daily_specs = usage_dashboard_daily_windows(now, offset);
        let hourly_cache = self.dashboard_series_cache(&hourly_specs).await?;
        let daily_cache = self.dashboard_series_cache(&daily_specs).await?;
        Ok(Some((
            now.to_rfc3339(),
            timezone,
            UsageDashboardSeries {
                hourly_24h: self.dashboard_series_from_cache(&hourly_specs, &hourly_cache),
                daily_7d: self.dashboard_series_from_cache(&daily_specs, &daily_cache),
            },
        )))
    }

    pub async fn usage_dashboard_top_only(
        &self,
    ) -> anyhow::Result<Option<(String, UsageDashboardTop)>> {
        self.ensure_observability_usage_store("read usage dashboard top")?;
        let Some(_totals) = self.usage_totals_if_valid().await? else {
            return Ok(None);
        };

        let now = Utc::now();
        Ok(Some((
            now.to_rfc3339(),
            UsageDashboardTop {
                window_key: "lifetime".to_string(),
                models: self
                    .dashboard_top_aggregates(USAGE_DASHBOARD_TOP_MODELS_KEY, "model")
                    .await?,
                credentials: self
                    .dashboard_top_aggregates(USAGE_DASHBOARD_TOP_CREDENTIALS_KEY, "credential")
                    .await?,
                endpoints: self
                    .dashboard_top_aggregates(USAGE_DASHBOARD_TOP_ENDPOINTS_KEY, "endpoint")
                    .await?,
                errors: self
                    .dashboard_top_aggregates(USAGE_DASHBOARD_TOP_ERRORS_KEY, "error")
                    .await?,
            },
        )))
    }

    pub async fn clear_usage_summary_aggregates_bounded(
        &self,
        cancel: Option<&AtomicBool>,
    ) -> anyhow::Result<RedisPatternDeleteStats> {
        self.ensure_observability_usage_store("clear usage summary aggregates")?;
        let mut stats = self
            .delete_pattern_bounded("usage:summary:*", cancel)
            .await?;
        stats.merge(
            self.delete_pattern_bounded("usage:dashboard:*", cancel)
                .await?,
        );
        Ok(stats)
    }

    #[cfg(test)]
    pub async fn clear_usage_summary(&self) -> anyhow::Result<usize> {
        let mut stats = self.clear_usage_summary_aggregates_bounded(None).await?;
        stats.merge(self.clear_usage_record_snapshots_bounded(None).await?);
        if stats.pass_limit_reached {
            anyhow::bail!(
                "Redis usage summary cleanup did not converge after {} passes",
                REDIS_PATTERN_DELETE_MAX_PASSES
            );
        }
        Ok(stats.deleted_keys)
    }

    pub async fn clear_usage_record_snapshots_bounded(
        &self,
        cancel: Option<&AtomicBool>,
    ) -> anyhow::Result<RedisPatternDeleteStats> {
        self.ensure_observability_usage_store("clear usage record snapshots")?;
        let (initial_index_deleted, initial_fallback) =
            self.unlink_count(USAGE_RECORDS_INDEX_KEY).await?;
        let mut stats = self
            .delete_pattern_bounded("usage:records:item:*", cancel)
            .await?;
        let (final_index_deleted, final_fallback) =
            self.unlink_count(USAGE_RECORDS_INDEX_KEY).await?;
        stats.deleted_keys = stats
            .deleted_keys
            .saturating_add(initial_index_deleted)
            .saturating_add(final_index_deleted);
        stats.delete_commands = stats
            .delete_commands
            .saturating_add(usize::from(initial_index_deleted > 0))
            .saturating_add(usize::from(final_index_deleted > 0));
        stats.max_command_keys = stats.max_command_keys.max(usize::from(
            initial_index_deleted > 0 || final_index_deleted > 0,
        ));
        stats.used_del_fallback |= initial_fallback || final_fallback;
        Ok(stats)
    }

    async fn usage_realtime_stats(&self) -> anyhow::Result<UsageRealtimeStats> {
        let now = Utc::now().timestamp();
        let mut pipe = redis::pipe();
        for offset in 0..REALTIME_USAGE_WINDOW_SECS as i64 {
            pipe.cmd("HGETALL")
                .arg(self.key(usage_realtime_bucket_key(now - offset)));
        }
        let mut manager = self.manager.clone();
        let buckets: Vec<HashMap<String, String>> = pipe.query_async(&mut manager).await?;
        let mut requests = 0usize;
        let mut input_tokens = 0i64;
        let mut output_tokens = 0i64;
        let mut billable_input_tokens = 0i64;
        for bucket in buckets {
            requests += usage_usize(&bucket, "requests");
            input_tokens += usage_i64(&bucket, "input_tokens");
            output_tokens += usage_i64(&bucket, "output_tokens");
            billable_input_tokens += usage_i64(&bucket, "billable_input_tokens");
        }
        Ok(UsageRealtimeStats::from_totals(
            REALTIME_USAGE_WINDOW_SECS,
            requests,
            input_tokens,
            output_tokens,
            billable_input_tokens,
        ))
    }

    async fn usage_top_aggregates(
        &self,
        index_key: &str,
        dimension: &str,
    ) -> anyhow::Result<Vec<UsageAggregate>> {
        let mut manager = self.manager.clone();
        let keys: Vec<String> = manager.zrevrange(self.key(index_key), 0, 9).await?;
        if keys.is_empty() {
            return Ok(Vec::new());
        }

        let mut pipe = redis::pipe();
        for key in &keys {
            pipe.cmd("HGETALL")
                .arg(self.key(usage_top_metrics_key(dimension, key)));
        }
        let metrics_list: Vec<HashMap<String, String>> = pipe.query_async(&mut manager).await?;
        let mut items = Vec::with_capacity(keys.len());
        for (key, metrics) in keys.into_iter().zip(metrics_list) {
            if metrics.is_empty() {
                continue;
            }
            items.push(UsageAggregate {
                key: metrics.get("key").cloned().unwrap_or(key),
                label: metrics
                    .get("label")
                    .filter(|value| !value.is_empty())
                    .cloned(),
                requests: usage_usize(&metrics, "requests"),
                cache_read_input_tokens: usage_i64(&metrics, "cache_read_input_tokens"),
                cache_creation_input_tokens: usage_i64(&metrics, "cache_creation_input_tokens"),
                estimated_cost_usd: usage_f64(&metrics, "estimated_cost_usd"),
                original_cost_usd: usage_f64(&metrics, "original_cost_usd"),
            });
        }
        Ok(items)
    }

    #[cfg(test)]
    fn dashboard_window_from_cache(
        &self,
        spec: &UsageDashboardWindowSpec,
        high_cache_threshold: i32,
        external_pool_index: &[RedisExternalPoolIndexItem],
        cache: &DashboardBucketCache,
    ) -> UsageDashboardWindow {
        let totals = cache.sum_bucket("global", "all", spec);
        let high_cache_requests = cache.high_cache_requests(spec, high_cache_threshold);
        let total_requests = usage_usize(&totals, "total_requests");
        let mut summary = dashboard_summary_from_values(&totals, high_cache_requests);
        summary.status_breakdown = self.dashboard_breakdown_from_cache(
            spec,
            "status",
            &USAGE_STATUS_VALUES,
            usage_status_label,
            total_requests,
            cache,
        );
        summary.usage_source_breakdown = self.dashboard_breakdown_from_cache(
            spec,
            "usage_source",
            &USAGE_SOURCE_VALUES,
            usage_source_label,
            total_requests,
            cache,
        );
        summary.external_pool_billing_by_pool = self
            .dashboard_external_pool_billing_by_pool_from_cache(spec, external_pool_index, cache);

        UsageDashboardWindow {
            key: spec.key.clone(),
            label: spec.label.clone(),
            from: spec.from.to_rfc3339(),
            to: spec.to.to_rfc3339(),
            summary,
        }
    }

    #[cfg(test)]
    async fn dashboard_external_pool_index(
        &self,
    ) -> anyhow::Result<Vec<RedisExternalPoolIndexItem>> {
        let mut manager = self.manager.clone();
        let pool_ids: Vec<String> = manager
            .zrevrange(
                self.key(USAGE_DASHBOARD_TOP_EXTERNAL_POOLS_KEY),
                0,
                USAGE_DASHBOARD_EXTERNAL_POOL_LIMIT,
            )
            .await?;
        if pool_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut pipe = redis::pipe();
        for pool_id in pool_ids {
            pipe.cmd("HGETALL")
                .arg(self.key(usage_dashboard_top_metrics_key("external_pool", &pool_id)));
        }
        let metrics_list: Vec<HashMap<String, String>> = pipe.query_async(&mut manager).await?;
        Ok(metrics_list
            .into_iter()
            .map(|metrics| {
                let id = metrics.get("key").cloned().unwrap_or_default();
                let label = metrics
                    .get("label")
                    .filter(|value| !value.trim().is_empty())
                    .cloned()
                    .unwrap_or_else(|| format!("#{}", id));
                RedisExternalPoolIndexItem { id, label }
            })
            .filter(|item| !item.id.trim().is_empty())
            .collect())
    }

    async fn dashboard_top_aggregates(
        &self,
        index_key: &str,
        dimension: &str,
    ) -> anyhow::Result<Vec<UsageTopAggregate>> {
        let mut manager = self.manager.clone();
        let keys: Vec<String> = manager.zrevrange(self.key(index_key), 0, 9).await?;
        if keys.is_empty() {
            return Ok(Vec::new());
        }

        let mut pipe = redis::pipe();
        for key in &keys {
            pipe.cmd("HGETALL")
                .arg(self.key(usage_dashboard_top_metrics_key(dimension, key)));
        }
        let metrics_list: Vec<HashMap<String, String>> = pipe.query_async(&mut manager).await?;
        let mut items = Vec::with_capacity(keys.len());
        for (key, metrics) in keys.into_iter().zip(metrics_list) {
            if metrics.is_empty() {
                continue;
            }
            items.push(UsageTopAggregate {
                key: metrics.get("key").cloned().unwrap_or(key),
                label: metrics
                    .get("label")
                    .filter(|value| !value.is_empty())
                    .cloned(),
                requests: usage_usize(&metrics, "requests"),
                error_requests: usage_usize(&metrics, "error_requests"),
                total_input_tokens: usage_i64(&metrics, "total_input_tokens"),
                billable_input_tokens: usage_i64(&metrics, "billable_input_tokens"),
                total_output_tokens: usage_i64(&metrics, "total_output_tokens"),
                total_cache_read_input_tokens: usage_i64(&metrics, "total_cache_read_input_tokens"),
                total_cache_creation_input_tokens: usage_i64(
                    &metrics,
                    "total_cache_creation_input_tokens",
                ),
                total_estimated_cost_usd: usage_f64(&metrics, "total_estimated_cost_usd"),
                total_original_cost_usd: usage_f64(&metrics, "total_original_cost_usd"),
            });
        }
        items.sort_by_key(|item| {
            (
                std::cmp::Reverse((item.total_estimated_cost_usd * 1_000_000.0).round() as i64),
                std::cmp::Reverse(item.requests),
                std::cmp::Reverse(item.total_input_tokens),
            )
        });
        items.truncate(10);
        Ok(items)
    }

    #[allow(dead_code)]
    pub async fn get_session_binding(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<SchedulerSessionBinding>> {
        self.get_json(session_binding_key(session_id)).await
    }

    pub async fn set_session_binding(
        &self,
        session_id: &str,
        binding: &SchedulerSessionBinding,
        ttl_secs: usize,
    ) -> anyhow::Result<SchedulerSessionBinding> {
        let encoded = serde_json::to_string(binding)?;
        let session_hash = session_hash(session_id);
        let script = r#"
            local old = redis.call('GET', KEYS[1])
            local next = cjson.decode(ARGV[3])
            if old then
                local ok, parsed = pcall(cjson.decode, old)
                if ok and parsed['credential_id'] then
                    local old_id = tostring(parsed['credential_id'])
                    if old_id ~= ARGV[2] then
                        redis.call('SREM', ARGV[5] .. old_id, ARGV[1])
                    else
                        next['soft_failure_count'] = tonumber(parsed['soft_failure_count'] or '0')
                    end
                end
            end
            local next_encoded = cjson.encode(next)
            redis.call('SET', KEYS[1], next_encoded, 'EX', ARGV[4])
            redis.call('SADD', ARGV[5] .. ARGV[2], ARGV[1])
            redis.call('EXPIRE', ARGV[5] .. ARGV[2], ARGV[4])
            return next_encoded
        "#;
        let mut manager = self.scheduler_manager();
        let actual: String = redis::cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(self.key(session_binding_key(session_id)))
            .arg(&session_hash)
            .arg(binding.credential_id.to_string())
            .arg(encoded)
            .arg(ttl_secs.max(1))
            .arg(self.key("scheduler:sessions_by_credential:"))
            .query_async(&mut manager)
            .await?;
        Ok(serde_json::from_str(&actual)?)
    }

    pub async fn delete_session_binding_if_bound_to(
        &self,
        session_id: &str,
        credential_id: u64,
    ) -> anyhow::Result<bool> {
        let session_hash = session_hash(session_id);
        let script = r#"
            local old = redis.call('GET', KEYS[1])
            if not old then
                return 0
            end
            local ok, parsed = pcall(cjson.decode, old)
            if not ok or not parsed['credential_id'] then
                return 0
            end
            if tostring(parsed['credential_id']) ~= ARGV[2] then
                return 0
            end
            redis.call('DEL', KEYS[1])
            redis.call('SREM', ARGV[3] .. ARGV[2], ARGV[1])
            return 1
        "#;
        let mut manager = self.scheduler_manager();
        let deleted: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(self.key(session_binding_key(session_id)))
            .arg(&session_hash)
            .arg(credential_id.to_string())
            .arg(self.key("scheduler:sessions_by_credential:"))
            .query_async(&mut manager)
            .await?;
        Ok(deleted == 1)
    }

    pub async fn delete_sessions_for_credential(
        &self,
        credential_id: u64,
    ) -> anyhow::Result<usize> {
        let set_key = self.key(sessions_by_credential_key(credential_id));
        let mut manager = self.scheduler_manager();
        let session_hashes: Vec<String> = manager.smembers(&set_key).await?;
        let mut deleted = 0usize;
        let script = r#"
            local raw = redis.call('GET', KEYS[1])
            if not raw then
                redis.call('SREM', KEYS[2], ARGV[1])
                return 0
            end
            local ok, parsed = pcall(cjson.decode, raw)
            if not ok or tostring(parsed['credential_id']) ~= ARGV[2] then
                redis.call('SREM', KEYS[2], ARGV[1])
                return 0
            end
            redis.call('DEL', KEYS[1])
            redis.call('SREM', KEYS[2], ARGV[1])
            return 1
        "#;
        for session_hash in &session_hashes {
            let removed: i64 = redis::cmd("EVAL")
                .arg(script)
                .arg(2)
                .arg(self.key(format!("scheduler:session:{}", session_hash)))
                .arg(&set_key)
                .arg(session_hash)
                .arg(credential_id.to_string())
                .query_async(&mut manager)
                .await?;
            if removed > 0 {
                deleted += 1;
            }
        }
        Ok(deleted)
    }

    pub async fn record_session_soft_failure_with_state(
        &self,
        session_id: &str,
        credential_id: u64,
        ttl_secs: usize,
    ) -> anyhow::Result<Option<SchedulerSessionBinding>> {
        let session_hash = session_hash(session_id);
        let script = r#"
            local raw = redis.call('GET', KEYS[1])
            if not raw then
                return nil
            end
            local ok, parsed = pcall(cjson.decode, raw)
            if not ok or not parsed['credential_id'] then
                return nil
            end
            if tostring(parsed['credential_id']) ~= ARGV[2] then
                return nil
            end
            parsed['soft_failure_count'] = tonumber(parsed['soft_failure_count'] or '0') + 1
            parsed['last_used_at'] = ARGV[3]
            local encoded = cjson.encode(parsed)
            redis.call('SET', KEYS[1], encoded, 'EX', ARGV[4])
            redis.call('SADD', ARGV[5] .. ARGV[2], ARGV[1])
            redis.call('EXPIRE', ARGV[5] .. ARGV[2], ARGV[4])
            return encoded
        "#;
        let mut manager = self.scheduler_manager();
        let encoded: Option<String> = redis::cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(self.key(session_binding_key(session_id)))
            .arg(&session_hash)
            .arg(credential_id.to_string())
            .arg(Utc::now().to_rfc3339())
            .arg(ttl_secs.max(1))
            .arg(self.key("scheduler:sessions_by_credential:"))
            .query_async(&mut manager)
            .await?;
        encoded
            .map(|encoded| serde_json::from_str(&encoded).map_err(anyhow::Error::from))
            .transpose()
    }

    #[cfg(test)]
    pub async fn record_session_soft_failure(
        &self,
        session_id: &str,
        credential_id: u64,
        threshold: u32,
        ttl_secs: usize,
    ) -> anyhow::Result<bool> {
        Ok(self
            .record_session_soft_failure_with_state(session_id, credential_id, ttl_secs)
            .await?
            .is_some_and(|binding| binding.soft_failure_count >= threshold))
    }

    pub async fn clear_session_soft_failure_with_state(
        &self,
        session_id: &str,
        credential_id: u64,
        ttl_secs: usize,
    ) -> anyhow::Result<Option<SchedulerSessionBinding>> {
        let session_hash = session_hash(session_id);
        let script = r#"
            local raw = redis.call('GET', KEYS[1])
            if not raw then
                return nil
            end
            local ok, parsed = pcall(cjson.decode, raw)
            if not ok or not parsed['credential_id'] then
                return nil
            end
            if tostring(parsed['credential_id']) ~= ARGV[2] then
                return nil
            end
            parsed['soft_failure_count'] = 0
            parsed['last_used_at'] = ARGV[3]
            local encoded = cjson.encode(parsed)
            redis.call('SET', KEYS[1], encoded, 'EX', ARGV[4])
            redis.call('SADD', ARGV[5] .. ARGV[2], ARGV[1])
            redis.call('EXPIRE', ARGV[5] .. ARGV[2], ARGV[4])
            return encoded
        "#;
        let mut manager = self.scheduler_manager();
        let encoded: Option<String> = redis::cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(self.key(session_binding_key(session_id)))
            .arg(&session_hash)
            .arg(credential_id.to_string())
            .arg(Utc::now().to_rfc3339())
            .arg(ttl_secs.max(1))
            .arg(self.key("scheduler:sessions_by_credential:"))
            .query_async(&mut manager)
            .await?;
        encoded
            .map(|encoded| serde_json::from_str(&encoded).map_err(anyhow::Error::from))
            .transpose()
    }

    #[cfg(test)]
    pub async fn clear_session_soft_failure(
        &self,
        session_id: &str,
        credential_id: u64,
        ttl_secs: usize,
    ) -> anyhow::Result<()> {
        self.clear_session_soft_failure_with_state(session_id, credential_id, ttl_secs)
            .await?;
        Ok(())
    }

    #[allow(dead_code)]
    pub async fn set_scheduler_cooldown(
        &self,
        credential_id: u64,
        duration: StdDuration,
        reason: Option<String>,
    ) -> anyhow::Result<SchedulerCooldownState> {
        let duration = duration.max(StdDuration::from_secs(1));
        let now = now_ms();
        let state = SchedulerCooldownState {
            until_ms: now + duration.as_millis() as i64,
            reason,
            model: None,
        };
        let encoded = serde_json::to_string(&state)?;
        let ttl_ms = (state.until_ms - now).max(1);
        let script = r#"
            local existing = redis.call('GET', KEYS[1])
            if existing then
                local ok, existing_data = pcall(cjson.decode, existing)
                if ok and existing_data and existing_data.until_ms then
                    local existing_until = tonumber(existing_data.until_ms)
                    local new_until = tonumber(ARGV[1])
                    if existing_until and new_until and existing_until >= new_until then
                        return existing
                    end
                end
            end
            redis.call('SET', KEYS[1], ARGV[2], 'PX', ARGV[3])
            return ARGV[2]
        "#;
        let mut manager = self.scheduler_manager();
        let stored: String = redis::cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(self.key(scheduler_cooldown_key(credential_id)))
            .arg(state.until_ms)
            .arg(&encoded)
            .arg(ttl_ms)
            .query_async(&mut manager)
            .await?;
        Ok(serde_json::from_str(&stored)?)
    }

    #[allow(dead_code)]
    pub async fn get_scheduler_cooldown(
        &self,
        credential_id: u64,
    ) -> anyhow::Result<Option<SchedulerCooldownState>> {
        let key = scheduler_cooldown_key(credential_id);
        let state = self.get_json::<SchedulerCooldownState>(&key).await?;
        if state
            .as_ref()
            .is_some_and(|state| state.until_ms <= now_ms())
        {
            self.del(key).await?;
            return Ok(None);
        }
        Ok(state)
    }

    pub async fn clear_scheduler_cooldown(&self, credential_id: u64) -> anyhow::Result<()> {
        let mut manager = self.scheduler_manager();
        let index_key = scheduler_model_index_key(credential_id);
        let models: HashMap<String, String> = manager
            .hgetall(self.key(&index_key))
            .await
            .unwrap_or_default();
        let mut pipe = redis::pipe();
        pipe.cmd("DEL")
            .arg(self.key(scheduler_cooldown_key(credential_id)));
        for hash in models.keys() {
            pipe.cmd("DEL")
                .arg(self.key(scheduler_model_cooldown_key(credential_id, hash)));
        }
        let _: () = pipe.query_async(&mut manager).await?;
        Ok(())
    }

    pub async fn record_scheduler_transient_failure(
        &self,
        credential_id: u64,
        model: Option<&str>,
        kind: &str,
        reason: &str,
        retry_after: Option<StdDuration>,
        base_cooldown: StdDuration,
        max_cooldown: StdDuration,
        backoff_multiplier: f64,
        jitter_factor: f64,
        probation: StdDuration,
        ewma_alpha: f64,
        coalesce_window: StdDuration,
    ) -> anyhow::Result<(SchedulerCooldownState, SchedulerHealthState)> {
        let now = now_ms();
        let retry_after_ms = retry_after
            .map(|duration| duration.as_millis().max(1) as i64)
            .unwrap_or(-1);
        let model = model.map(str::trim).filter(|value| !value.is_empty());
        let model_hash = model.map(scheduler_model_hash);
        let cooldown_key = match model_hash.as_deref() {
            Some(hash) => scheduler_model_cooldown_key(credential_id, hash),
            None => scheduler_cooldown_key(credential_id),
        };
        let health_key = match model_hash.as_deref() {
            Some(hash) => scheduler_model_health_key(credential_id, hash),
            None => scheduler_health_key(credential_id),
        };
        let script = r#"
            local now = tonumber(ARGV[1])
            local kind = ARGV[2]
            local reason = ARGV[3]
            local retry_after_ms = tonumber(ARGV[4])
            local base_ms = tonumber(ARGV[5])
            local max_ms = tonumber(ARGV[6])
            local multiplier = tonumber(ARGV[7])
            local jitter = tonumber(ARGV[8])
            local probation_ms = tonumber(ARGV[9])
            local alpha = tonumber(ARGV[10])
            local health_ttl = tonumber(ARGV[11])
            local model = ARGV[12]
            local model_hash = ARGV[13]
            local coalesce_window_ms = tonumber(ARGV[14])

            local health = {}
            local health_raw = redis.call('GET', KEYS[2])
            if health_raw then
                local ok, parsed = pcall(cjson.decode, health_raw)
                if ok and parsed then health = parsed end
            end
            local cooldown = nil
            local existing_until = 0
            local cooldown_raw = redis.call('GET', KEYS[1])
            if cooldown_raw then
                local ok, parsed = pcall(cjson.decode, cooldown_raw)
                if ok and parsed then
                    cooldown = parsed
                    existing_until = tonumber(parsed['until_ms'] or '0')
                end
            end
            local last_error_at = tonumber(health['last_error_at_ms'] or '-1')
            local duplicate_wave = retry_after_ms < 0
                and existing_until > now
                and health['last_error_kind'] == kind
                and last_error_at >= 0
                and now >= last_error_at
                and (now - last_error_at) <= coalesce_window_ms
            local streak = tonumber(health['transient_failure_streak'] or '0')
            if duplicate_wave then
                streak = math.max(streak, 1)
            else
                streak = streak + 1
            end
            local previous_error_rate = tonumber(health['recent_error_rate'] or '0')
            health['transient_failure_streak'] = streak
            health['recent_error_rate'] = previous_error_rate + alpha * (1 - previous_error_rate)
            health['last_error_kind'] = kind
            health['last_error_reason'] = reason
            health['last_error_at_ms'] = now

            local requested
            if retry_after_ms >= 0 then
                requested = retry_after_ms
            else
                requested = base_ms * (multiplier ^ math.max(streak - 1, 0)) * jitter
            end
            local duration_ms = math.max(1000, math.min(max_ms, math.floor(requested + 0.5)))
            local candidate_until = now + duration_ms

            if duplicate_wave and cooldown then
                -- Keep the current cooldown deadline for concurrent failures in the same burst.
            else
                local candidate_cooldown = {until_ms = candidate_until, reason = reason}
                if model ~= '' then candidate_cooldown['model'] = model end
                if cooldown and existing_until >= candidate_until then
                    -- Keep the longer existing cooldown.
                else
                    cooldown = candidate_cooldown
                end
            end

            local current_probation = tonumber(health['probation_until_ms'] or '0')
            health['probation_until_ms'] = math.max(current_probation, tonumber(cooldown['until_ms']) + probation_ms)
            local health_encoded = cjson.encode(health)
            local cooldown_encoded = cjson.encode(cooldown)
            redis.call('SET', KEYS[2], health_encoded, 'EX', health_ttl)
            redis.call('SET', KEYS[1], cooldown_encoded, 'PX', math.max(1, tonumber(cooldown['until_ms']) - now))
            if model ~= '' then
                redis.call('HSET', KEYS[3], model_hash, model)
                redis.call('EXPIRE', KEYS[3], health_ttl)
            end
            return {cooldown_encoded, health_encoded}
        "#;
        let health_ttl_secs = 30 * 24 * 60 * 60;
        let mut manager = self.scheduler_manager();
        let result: Vec<String> = redis::cmd("EVAL")
            .arg(script)
            .arg(3)
            .arg(self.key(cooldown_key))
            .arg(self.key(health_key))
            .arg(self.key(scheduler_model_index_key(credential_id)))
            .arg(now)
            .arg(kind)
            .arg(reason)
            .arg(retry_after_ms)
            .arg(base_cooldown.as_millis().max(1) as i64)
            .arg(max_cooldown.as_millis().max(1) as i64)
            .arg(backoff_multiplier.max(1.0))
            .arg(jitter_factor.max(0.01))
            .arg(probation.as_millis() as i64)
            .arg(ewma_alpha.clamp(0.01, 1.0))
            .arg(health_ttl_secs)
            .arg(model.unwrap_or(""))
            .arg(model_hash.as_deref().unwrap_or(""))
            .arg(coalesce_window.as_millis().clamp(1, i64::MAX as u128) as i64)
            .query_async(&mut manager)
            .await?;
        let cooldown = serde_json::from_str(
            result
                .first()
                .ok_or_else(|| anyhow::anyhow!("Redis 未返回调度冷却结果"))?,
        )?;
        let health = serde_json::from_str(
            result
                .get(1)
                .ok_or_else(|| anyhow::anyhow!("Redis 未返回调度健康结果"))?,
        )?;
        Ok((cooldown, health))
    }

    pub async fn record_scheduler_success(
        &self,
        credential_id: u64,
        model: Option<&str>,
        latency: Option<StdDuration>,
        ewma_alpha: f64,
    ) -> anyhow::Result<SchedulerHealthState> {
        let model = model.map(str::trim).filter(|value| !value.is_empty());
        let model_hash = model.map(scheduler_model_hash);
        let health_key = match model_hash.as_deref() {
            Some(hash) => scheduler_model_health_key(credential_id, hash),
            None => scheduler_health_key(credential_id),
        };
        let script = r#"
            local alpha = tonumber(ARGV[1])
            local latency_ms = tonumber(ARGV[2])
            local ttl = tonumber(ARGV[3])
            local model = ARGV[4]
            local model_hash = ARGV[5]
            local health = {}
            local raw = redis.call('GET', KEYS[1])
            if raw then
                local ok, parsed = pcall(cjson.decode, raw)
                if ok and parsed then health = parsed end
            end
            local previous_error_rate = tonumber(health['recent_error_rate'] or '0')
            health['recent_error_rate'] = previous_error_rate * (1 - alpha)
            health['transient_failure_streak'] = math.max(0, tonumber(health['transient_failure_streak'] or '0') - 1)
            if latency_ms >= 0 then
                local previous_latency = tonumber(health['latency_ewma_ms'])
                if previous_latency then
                    health['latency_ewma_ms'] = previous_latency + alpha * (latency_ms - previous_latency)
                else
                    health['latency_ewma_ms'] = latency_ms
                end
            end
            local encoded = cjson.encode(health)
            redis.call('SET', KEYS[1], encoded, 'EX', ttl)
            if model ~= '' then
                redis.call('HSET', KEYS[2], model_hash, model)
                redis.call('EXPIRE', KEYS[2], ttl)
            end
            return encoded
        "#;
        let latency_ms = latency
            .map(|duration| duration.as_millis() as i64)
            .unwrap_or(-1);
        let mut manager = self.scheduler_manager();
        let encoded: String = redis::cmd("EVAL")
            .arg(script)
            .arg(2)
            .arg(self.key(health_key))
            .arg(self.key(scheduler_model_index_key(credential_id)))
            .arg(ewma_alpha.clamp(0.01, 1.0))
            .arg(latency_ms)
            .arg(30 * 24 * 60 * 60)
            .arg(model.unwrap_or(""))
            .arg(model_hash.as_deref().unwrap_or(""))
            .query_async(&mut manager)
            .await?;
        Ok(serde_json::from_str(&encoded)?)
    }

    pub async fn record_scheduler_selection(
        &self,
        credential_id: u64,
        rpm: u32,
        weight_units: u32,
    ) -> anyhow::Result<SchedulerHealthState> {
        let now = now_ms();
        let weight_units = weight_units.clamp(1, 64);
        let script = r#"
            local ttl = tonumber(ARGV[1])
            local now = tonumber(ARGV[2])
            local window_10s = tonumber(ARGV[3])
            local window_60s = tonumber(ARGV[4])
            local window_5m = tonumber(ARGV[5])
            local rpm = tonumber(ARGV[6])
            local weight_units = tonumber(ARGV[7])
            local health = {}
            local raw = redis.call('GET', KEYS[1])
            if raw then
                local ok, parsed = pcall(cjson.decode, raw)
                if ok and parsed then health = parsed end
            end
            redis.call('ZREMRANGEBYSCORE', KEYS[2], '-inf', now - window_5m)
            for i = 1, weight_units do
                local sequence = redis.call('INCR', KEYS[3])
                local member = tostring(now) .. '-' .. tostring(sequence)
                redis.call('ZADD', KEYS[2], now, member)
            end
            redis.call('EXPIRE', KEYS[2], ttl)
            health['selection_count'] = tonumber(health['selection_count'] or '0') + 1
            health['recent_selection_count_10s'] = redis.call('ZCOUNT', KEYS[2], now - window_10s, '+inf')
            health['recent_selection_count_60s'] = redis.call('ZCOUNT', KEYS[2], now - window_60s, '+inf')
            health['recent_selection_count_5m'] = redis.call('ZCOUNT', KEYS[2], now - window_5m, '+inf')
            if rpm > 0 and tonumber(health['recent_selection_count_60s']) >= rpm then
                local oldest = redis.call('ZRANGEBYSCORE', KEYS[2], now - window_60s, '+inf', 'WITHSCORES', 'LIMIT', 0, 1)
                local next_at = now + window_60s
                if oldest and oldest[2] then
                    next_at = tonumber(oldest[2]) + window_60s
                end
                if next_at > now then
                    redis.call('SET', KEYS[4], tostring(next_at), 'PX', math.max(1, next_at - now))
                else
                    redis.call('DEL', KEYS[4])
                end
            else
                redis.call('DEL', KEYS[4])
            end
            redis.call('SET', KEYS[1], cjson.encode(health), 'EX', ttl)
            return cjson.encode(health)
        "#;
        let mut manager = self.scheduler_manager();
        let encoded: String = redis::cmd("EVAL")
            .arg(script)
            .arg(4)
            .arg(self.key(scheduler_health_key(credential_id)))
            .arg(self.key(scheduler_selection_window_key(credential_id)))
            .arg(self.key("scheduler:selection:sequence"))
            .arg(self.key(scheduler_rate_limit_key(credential_id)))
            .arg(30 * 24 * 60 * 60)
            .arg(now)
            .arg(10_000)
            .arg(60_000)
            .arg(5 * 60_000)
            .arg(rpm)
            .arg(weight_units)
            .query_async(&mut manager)
            .await?;
        Ok(serde_json::from_str(&encoded)?)
    }

    pub async fn try_record_scheduler_selection(
        &self,
        credential_id: u64,
        rpm: u32,
        weight_units: u32,
    ) -> anyhow::Result<SchedulerSelectionReservation> {
        let now = now_ms();
        let weight_units = weight_units.clamp(1, 64);
        let script = r#"
            local ttl = tonumber(ARGV[1])
            local now = tonumber(ARGV[2])
            local window_10s = tonumber(ARGV[3])
            local window_60s = tonumber(ARGV[4])
            local window_5m = tonumber(ARGV[5])
            local rpm = tonumber(ARGV[6])
            local weight_units = tonumber(ARGV[7])
            local health = {}
            local raw = redis.call('GET', KEYS[1])
            if raw then
                local ok, parsed = pcall(cjson.decode, raw)
                if ok and parsed then health = parsed end
            end
            redis.call('ZREMRANGEBYSCORE', KEYS[2], '-inf', now - window_5m)
            local existing = tonumber(redis.call('GET', KEYS[4]) or '0')
            if existing > now then
                return cjson.encode({
                    status = 'rate_limited',
                    retryAfterMs = existing - now,
                    rateLimitAvailableAtMs = existing,
                })
            end
            local recent_60s = tonumber(redis.call('ZCOUNT', KEYS[2], now - window_60s, '+inf'))
            if rpm > 0 and recent_60s >= rpm then
                local oldest = redis.call('ZRANGEBYSCORE', KEYS[2], now - window_60s, '+inf', 'WITHSCORES', 'LIMIT', 0, 1)
                local next_at = now + window_60s
                if oldest and oldest[2] then
                    next_at = tonumber(oldest[2]) + window_60s
                end
                if next_at > now then
                    redis.call('SET', KEYS[4], tostring(next_at), 'PX', math.max(1, next_at - now))
                    return cjson.encode({
                        status = 'rate_limited',
                        retryAfterMs = next_at - now,
                        rateLimitAvailableAtMs = next_at,
                    })
                end
            end
            for i = 1, weight_units do
                local sequence = redis.call('INCR', KEYS[3])
                local member = tostring(now) .. '-' .. tostring(sequence)
                redis.call('ZADD', KEYS[2], now, member)
            end
            redis.call('EXPIRE', KEYS[2], ttl)
            health['selection_count'] = tonumber(health['selection_count'] or '0') + 1
            health['recent_selection_count_10s'] = redis.call('ZCOUNT', KEYS[2], now - window_10s, '+inf')
            health['recent_selection_count_60s'] = redis.call('ZCOUNT', KEYS[2], now - window_60s, '+inf')
            health['recent_selection_count_5m'] = redis.call('ZCOUNT', KEYS[2], now - window_5m, '+inf')
            local rate_limit_available_at_ms = nil
            if rpm > 0 and tonumber(health['recent_selection_count_60s']) >= rpm then
                local oldest = redis.call('ZRANGEBYSCORE', KEYS[2], now - window_60s, '+inf', 'WITHSCORES', 'LIMIT', 0, 1)
                local next_at = now + window_60s
                if oldest and oldest[2] then
                    next_at = tonumber(oldest[2]) + window_60s
                end
                if next_at > now then
                    redis.call('SET', KEYS[4], tostring(next_at), 'PX', math.max(1, next_at - now))
                    rate_limit_available_at_ms = next_at
                else
                    redis.call('DEL', KEYS[4])
                end
            else
                redis.call('DEL', KEYS[4])
            end
            redis.call('SET', KEYS[1], cjson.encode(health), 'EX', ttl)
            return cjson.encode({
                status = 'recorded',
                health = health,
                rateLimitAvailableAtMs = rate_limit_available_at_ms,
            })
        "#;
        let mut manager = self.scheduler_manager();
        let encoded: String = redis::cmd("EVAL")
            .arg(script)
            .arg(4)
            .arg(self.key(scheduler_health_key(credential_id)))
            .arg(self.key(scheduler_selection_window_key(credential_id)))
            .arg(self.key("scheduler:selection:sequence"))
            .arg(self.key(scheduler_rate_limit_key(credential_id)))
            .arg(30 * 24 * 60 * 60)
            .arg(now)
            .arg(10_000)
            .arg(60_000)
            .arg(5 * 60_000)
            .arg(rpm)
            .arg(weight_units)
            .query_async(&mut manager)
            .await?;
        let wire: SchedulerSelectionReservationWire = serde_json::from_str(&encoded)?;
        match wire.status.as_str() {
            "recorded" => Ok(SchedulerSelectionReservation::Recorded {
                health: wire.health.unwrap_or_default(),
                rate_limit_available_at_ms: wire.rate_limit_available_at_ms,
            }),
            "rate_limited" => Ok(SchedulerSelectionReservation::RateLimited {
                retry_after_ms: wire.retry_after_ms.unwrap_or(1).max(1),
                rate_limit_available_at_ms: wire
                    .rate_limit_available_at_ms
                    .unwrap_or(now + 1)
                    .max(now + 1),
            }),
            other => anyhow::bail!("unexpected scheduler selection reservation status: {other}"),
        }
    }

    pub async fn clear_scheduler_health(&self, credential_id: u64) -> anyhow::Result<()> {
        let mut manager = self.scheduler_manager();
        let index_key = scheduler_model_index_key(credential_id);
        let models: HashMap<String, String> = manager
            .hgetall(self.key(&index_key))
            .await
            .unwrap_or_default();
        let mut pipe = redis::pipe();
        pipe.atomic()
            .cmd("DEL")
            .arg(self.key(scheduler_health_key(credential_id)))
            .cmd("DEL")
            .arg(self.key(scheduler_selection_window_key(credential_id)))
            .cmd("DEL")
            .arg(self.key(&index_key));
        for hash in models.keys() {
            pipe.cmd("DEL")
                .arg(self.key(scheduler_model_health_key(credential_id, hash)))
                .cmd("DEL")
                .arg(self.key(scheduler_model_cooldown_key(credential_id, hash)));
        }
        let _: () = pipe.query_async(&mut manager).await?;
        Ok(())
    }

    #[cfg(test)]
    pub async fn bump_rate_limit_available_at(
        &self,
        credential_id: u64,
        interval: StdDuration,
    ) -> anyhow::Result<i64> {
        let interval_ms = interval.as_millis().max(1) as i64;
        let now = now_ms();
        let script = r#"
            local current = tonumber(redis.call('GET', KEYS[1]) or '0')
            local now = tonumber(ARGV[1])
            local interval = tonumber(ARGV[2])
            local next_at = math.max(current, now) + interval
            local ttl = math.max(1, math.ceil((next_at - now) / 1000))
            redis.call('SET', KEYS[1], tostring(next_at), 'EX', ttl)
            return next_at
        "#;
        let mut manager = self.scheduler_capacity_manager();
        let next_at: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(self.key(scheduler_rate_limit_key(credential_id)))
            .arg(now)
            .arg(interval_ms)
            .query_async(&mut manager)
            .await?;
        Ok(next_at)
    }

    #[allow(dead_code)]
    pub async fn get_rate_limit_available_at(
        &self,
        credential_id: u64,
    ) -> anyhow::Result<Option<i64>> {
        let key = scheduler_rate_limit_key(credential_id);
        let mut manager = self.scheduler_manager();
        let value: Option<String> = manager.get(self.key(&key)).await?;
        let Some(value) = value else {
            return Ok(None);
        };
        let until_ms = value.parse::<i64>()?;
        if until_ms <= now_ms() {
            let _: () = manager.del(self.key(&key)).await?;
            return Ok(None);
        }
        Ok(Some(until_ms))
    }

    pub async fn clear_rate_limit(&self, credential_id: u64) -> anyhow::Result<()> {
        self.del(scheduler_rate_limit_key(credential_id)).await
    }

    #[allow(dead_code)]
    pub async fn next_in_flight_lease_id(&self) -> anyhow::Result<u64> {
        let mut manager = self.scheduler_capacity_manager();
        let id: u64 = manager
            .incr(self.key("scheduler:inflight:lease_sequence"), 1u64)
            .await?;
        Ok(id)
    }

    #[allow(dead_code)]
    pub async fn acquire_in_flight_lease(
        &self,
        credential_id: u64,
        lease_id: u64,
        max_concurrent_requests: u32,
        max_age: Option<StdDuration>,
        kind: &str,
    ) -> anyhow::Result<Option<usize>> {
        self.acquire_dispatch_lease(
            credential_id,
            lease_id,
            max_concurrent_requests,
            0,
            1,
            max_age,
            kind,
        )
        .await
    }

    pub async fn acquire_dispatch_lease(
        &self,
        credential_id: u64,
        lease_id: u64,
        max_concurrent_requests: u32,
        global_max_concurrent_requests: u32,
        request_weight_units: u32,
        max_age: Option<StdDuration>,
        kind: &str,
    ) -> anyhow::Result<Option<usize>> {
        let now = now_ms();
        let request_weight_units = request_weight_units.clamp(1, 64);
        let max_age_ms = max_age.map(|age| age.as_millis() as i64).unwrap_or(0);
        let ttl_secs = max_age
            .map(|age| age.as_secs().saturating_mul(2).max(60) as i64)
            .unwrap_or(0);
        let script = r#"
            local now = tonumber(ARGV[1])
            local max_age_ms = tonumber(ARGV[2])
            local max_count = tonumber(ARGV[3])
            local global_max_count = tonumber(ARGV[4])
            local lease_id = ARGV[5]
            local kind = ARGV[6]
            local ttl_secs = tonumber(ARGV[7])
            local request_weight = tonumber(ARGV[8])

            if redis.call('SISMEMBER', KEYS[11], lease_id) == 1 then
                return {0, -1}
            end

            if max_age_ms > 0 then
                local expired = redis.call('ZRANGEBYSCORE', KEYS[1], '-inf', now - max_age_ms)
                for _, member in ipairs(expired) do
                    local weight = tonumber(redis.call('HGET', KEYS[4], member) or '1')
                    redis.call('ZREM', KEYS[1], member)
                    local acquired_removed = redis.call('ZREM', KEYS[2], member)
                    redis.call('HDEL', KEYS[3], member)
                    redis.call('HDEL', KEYS[4], member)
                    if acquired_removed > 0 then
                        local next_count = redis.call('DECRBY', KEYS[5], weight)
                        if tonumber(next_count) < 0 then
                            redis.call('SET', KEYS[5], 0)
                        end
                    end
                end
                local global_expired = redis.call('ZRANGEBYSCORE', KEYS[6], '-inf', now - max_age_ms)
                for _, member in ipairs(global_expired) do
                    local weight = tonumber(redis.call('HGET', KEYS[9], member) or '1')
                    redis.call('ZREM', KEYS[6], member)
                    local acquired_removed = redis.call('ZREM', KEYS[7], member)
                    redis.call('HDEL', KEYS[8], member)
                    redis.call('HDEL', KEYS[9], member)
                    if acquired_removed > 0 then
                        local next_count = redis.call('DECRBY', KEYS[10], weight)
                        if tonumber(next_count) < 0 then
                            redis.call('SET', KEYS[10], 0)
                        end
                    end
                end
            end

            local count = tonumber(redis.call('GET', KEYS[5]))
            if not count then
                count = redis.call('ZCARD', KEYS[2])
                redis.call('SET', KEYS[5], count)
            end
            if count < 0 then count = 0 end
            local effective_weight = request_weight
            if max_count > 0 and effective_weight > max_count then
                effective_weight = max_count
            end

            local global_count = tonumber(redis.call('GET', KEYS[10]))
            if not global_count then
                global_count = redis.call('ZCARD', KEYS[7])
                redis.call('SET', KEYS[10], global_count)
            end
            if global_count < 0 then global_count = 0 end
            if global_max_count > 0 and effective_weight > global_max_count then
                effective_weight = global_max_count
            end

            if max_count > 0 and (count + effective_weight) > max_count then
                return {0, count}
            end

            if global_max_count > 0 and (global_count + effective_weight) > global_max_count then
                return {0, global_count}
            end

            if redis.call('SISMEMBER', KEYS[11], lease_id) == 1 then
                return {0, -1}
            end

            redis.call('ZADD', KEYS[1], now, lease_id)
            redis.call('ZADD', KEYS[2], now, lease_id)
            redis.call('HSET', KEYS[3], lease_id, kind)
            redis.call('HSET', KEYS[4], lease_id, effective_weight)
            redis.call('INCRBY', KEYS[5], effective_weight)
            redis.call('ZADD', KEYS[6], now, lease_id)
            redis.call('ZADD', KEYS[7], now, lease_id)
            redis.call('HSET', KEYS[8], lease_id, kind)
            redis.call('HSET', KEYS[9], lease_id, effective_weight)
            redis.call('INCRBY', KEYS[10], effective_weight)
            if ttl_secs > 0 then
                redis.call('EXPIRE', KEYS[1], ttl_secs)
                redis.call('EXPIRE', KEYS[2], ttl_secs)
                redis.call('EXPIRE', KEYS[3], ttl_secs)
                redis.call('EXPIRE', KEYS[4], ttl_secs)
                redis.call('EXPIRE', KEYS[5], ttl_secs)
                redis.call('EXPIRE', KEYS[6], ttl_secs)
                redis.call('EXPIRE', KEYS[7], ttl_secs)
                redis.call('EXPIRE', KEYS[8], ttl_secs)
                redis.call('EXPIRE', KEYS[9], ttl_secs)
                redis.call('EXPIRE', KEYS[10], ttl_secs)
            end
            return {1, count + effective_weight}
        "#;
        let keys = in_flight_keys(credential_id);
        let global_keys = global_in_flight_keys();
        let mut manager = self.scheduler_capacity_manager();
        let result: Vec<i64> = redis::cmd("EVAL")
            .arg(script)
            .arg(11)
            .arg(self.key(&keys.last_seen))
            .arg(self.key(&keys.acquired))
            .arg(self.key(&keys.kind))
            .arg(self.key(&keys.weight))
            .arg(self.key(&keys.count))
            .arg(self.key(&global_keys.last_seen))
            .arg(self.key(&global_keys.acquired))
            .arg(self.key(&global_keys.kind))
            .arg(self.key(&global_keys.weight))
            .arg(self.key(&global_keys.count))
            .arg(self.key(&keys.released))
            .arg(now)
            .arg(max_age_ms)
            .arg(max_concurrent_requests)
            .arg(global_max_concurrent_requests)
            .arg(lease_id.to_string())
            .arg(kind)
            .arg(ttl_secs)
            .arg(request_weight_units)
            .query_async(&mut manager)
            .await?;
        if result.first().copied().unwrap_or(0) == 1 {
            Ok(Some(result.get(1).copied().unwrap_or(1).max(0) as usize))
        } else {
            Ok(None)
        }
    }

    /// External pool coordination uses atomic multi-key Lua scripts and is
    /// intentionally supported only on standalone Redis. Redis Cluster would
    /// require a shared hash tag and a cluster-aware client, neither of which
    /// is part of the current storage contract.
    pub async fn external_pool_redis_run_id(&self) -> anyhow::Result<String> {
        let mut manager = self.scheduler_capacity_manager();
        #[cfg(test)]
        self.external_pool_hot_path_round_trips
            .fetch_add(1, Ordering::Relaxed);
        let (server_info, cluster_info): (String, String) = redis::pipe()
            .cmd("INFO")
            .arg("server")
            .cmd("INFO")
            .arg("cluster")
            .query_async(&mut manager)
            .await?;
        let cluster_enabled = cluster_info.lines().any(|line| {
            line.trim_end_matches('\r')
                .split_once(':')
                .is_some_and(|(key, value)| key == "cluster_enabled" && value.trim() == "1")
        });
        if cluster_enabled {
            anyhow::bail!(
                "external pool coordination supports standalone Redis only; Redis Cluster is not supported"
            );
        }
        server_info
            .lines()
            .find_map(|line| {
                line.trim_end_matches('\r')
                    .split_once(':')
                    .filter(|(key, _)| *key == "run_id")
                    .map(|(_, value)| value.trim().to_string())
            })
            .filter(|run_id| !run_id.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Redis INFO server did not include run_id"))
    }

    pub async fn external_pool_coordinator_guard_state_for_epoch(
        &self,
        expected_epoch: &str,
    ) -> anyhow::Result<ExternalPoolCoordinatorGuardState> {
        let script = r#"
            local stored = redis.pcall('GET', KEYS[1])
            if type(stored) == 'table' and stored.err then
                return {2, '', 0}
            end
            if not stored or stored ~= ARGV[1] then
                return {2, stored or '', 0}
            end
            local recovery_ttl = redis.call('PTTL', KEYS[2])
            if recovery_ttl > 0 then
                return {3, stored, recovery_ttl}
            end
            if recovery_ttl == -1 then
                return {2, stored, 0}
            end
            return {1, stored, 0}
        "#;
        let mut manager = self.scheduler_capacity_manager();
        #[cfg(test)]
        self.external_pool_hot_path_round_trips
            .fetch_add(1, Ordering::Relaxed);
        let values: Vec<redis::Value> = redis::cmd("EVAL")
            .arg(script)
            .arg(2)
            .arg(self.key(EXTERNAL_POOL_COORDINATOR_EPOCH_KEY))
            .arg(self.key(EXTERNAL_POOL_COORDINATOR_RECOVERY_KEY))
            .arg(expected_epoch)
            .query_async(&mut manager)
            .await?;
        decode_external_pool_coordinator_guard_state(&values)
    }

    pub async fn install_external_pool_coordinator_guard(
        &self,
        coordination_epoch: &str,
        recovery_grace: StdDuration,
    ) -> anyhow::Result<ExternalPoolCoordinatorGuardState> {
        let recovery_grace_ms = recovery_grace.as_millis().min(i64::MAX as u128) as i64;
        let script = r#"
            local redis_time = redis.call('TIME')
            local now = tonumber(redis_time[1]) * 1000 + math.floor(tonumber(redis_time[2]) / 1000)
            local recovery_grace_ms = tonumber(ARGV[2]) or 0
            redis.call('SET', KEYS[1], ARGV[1])
            if recovery_grace_ms > 0 then
                local recovery_until = now + recovery_grace_ms
                redis.call('SET', KEYS[2], recovery_until)
                redis.call('PEXPIREAT', KEYS[2], recovery_until)
                return {3, ARGV[1], recovery_grace_ms}
            end
            redis.call('DEL', KEYS[2])
            return {1, ARGV[1], 0}
        "#;
        let mut manager = self.scheduler_capacity_manager();
        #[cfg(test)]
        self.external_pool_hot_path_round_trips
            .fetch_add(1, Ordering::Relaxed);
        let values: Vec<redis::Value> = redis::cmd("EVAL")
            .arg(script)
            .arg(2)
            .arg(self.key(EXTERNAL_POOL_COORDINATOR_EPOCH_KEY))
            .arg(self.key(EXTERNAL_POOL_COORDINATOR_RECOVERY_KEY))
            .arg(coordination_epoch)
            .arg(recovery_grace_ms)
            .query_async(&mut manager)
            .await?;
        decode_external_pool_coordinator_guard_state(&values)
    }

    pub async fn acquire_external_pool_lease(
        &self,
        pool_id: u64,
        lease_id: &str,
        coordination_epoch: &str,
        max_concurrent_requests: u32,
        global_max_concurrent_requests: u32,
        max_age: Option<StdDuration>,
        cooldown_keys: &[String],
    ) -> anyhow::Result<ExternalPoolLeaseAcquireResult> {
        if cooldown_keys.is_empty() {
            anyhow::bail!("external pool lease acquire requires the pool cooldown key");
        }
        let max_age_ms = max_age.map(|age| age.as_millis() as i64).unwrap_or(0);
        let ttl_secs = max_age
            .map(|age| age.as_secs().saturating_mul(2).max(60) as i64)
            .unwrap_or(0);
        let script = r#"
            local redis_time = redis.call('TIME')
            local now = tonumber(redis_time[1]) * 1000 + math.floor(tonumber(redis_time[2]) / 1000)
            local max_age_ms = tonumber(ARGV[1])
            local max_count = tonumber(ARGV[2])
            local global_max_count = tonumber(ARGV[3])
            local ttl_secs = tonumber(ARGV[4])
            local lease_id = ARGV[5]
            local expected_epoch = ARGV[6]

            local stored_epoch = redis.pcall('GET', KEYS[1])
            if type(stored_epoch) == 'table' and stored_epoch.err then
                return {6, 0, 0, 0, ''}
            end
            if not stored_epoch or stored_epoch ~= expected_epoch then
                return {6, 0, 0, 0, stored_epoch or ''}
            end
            local recovery_ttl = redis.call('PTTL', KEYS[2])
            if recovery_ttl > 0 then
                return {7, recovery_ttl, 0, 0, stored_epoch}
            end
            if recovery_ttl == -1 then
                return {6, 0, 0, 0, stored_epoch}
            end

            if max_age_ms > 0 then
                local expired = redis.call('ZRANGEBYSCORE', KEYS[3], '-inf', now - max_age_ms)
                for _, member in ipairs(expired) do
                    redis.call('ZREM', KEYS[3], member)
                    redis.call('ZREM', KEYS[4], member)
                end
                local global_expired = redis.call('ZRANGEBYSCORE', KEYS[5], '-inf', now - max_age_ms)
                for _, member in ipairs(global_expired) do
                    redis.call('ZREM', KEYS[5], member)
                    redis.call('ZREM', KEYS[6], member)
                end
            end

            local count = redis.call('ZCARD', KEYS[3])
            local global_count = redis.call('ZCARD', KEYS[5])
            redis.call('ZREMRANGEBYSCORE', KEYS[7], '-inf', now)

            if redis.call('ZSCORE', KEYS[7], lease_id) then
                return {5, 0, count, global_count}
            end

            for index = 8, #KEYS do
                if redis.call('EXISTS', KEYS[index]) == 1 then
                    return {2, redis.call('PTTL', KEYS[index]), count, global_count, index - 7}
                end
            end

            if max_count > 0 and count >= max_count then
                return {3, 0, count, global_count}
            end

            if global_max_count > 0 and global_count >= global_max_count then
                return {4, 0, count, global_count}
            end

            redis.call('ZADD', KEYS[3], now, lease_id)
            redis.call('ZADD', KEYS[4], now, lease_id)
            redis.call('ZADD', KEYS[5], now, lease_id)
            redis.call('ZADD', KEYS[6], now, lease_id)
            if ttl_secs > 0 then
                redis.call('EXPIRE', KEYS[3], ttl_secs)
                redis.call('EXPIRE', KEYS[4], ttl_secs)
                redis.call('EXPIRE', KEYS[5], ttl_secs)
                redis.call('EXPIRE', KEYS[6], ttl_secs)
            end
            return {1, 0, count + 1, global_count + 1}
        "#;
        let keys = external_pool_in_flight_keys(pool_id);
        let global_keys = external_pool_global_in_flight_keys();
        let mut manager = self.scheduler_capacity_manager();
        #[cfg(test)]
        self.external_pool_hot_path_round_trips
            .fetch_add(1, Ordering::Relaxed);
        let mut command = redis::cmd("EVAL");
        command
            .arg(script)
            .arg(7usize.saturating_add(cooldown_keys.len()))
            .arg(self.key(EXTERNAL_POOL_COORDINATOR_EPOCH_KEY))
            .arg(self.key(EXTERNAL_POOL_COORDINATOR_RECOVERY_KEY))
            .arg(self.key(&keys.last_seen))
            .arg(self.key(&keys.acquired))
            .arg(self.key(&global_keys.last_seen))
            .arg(self.key(&global_keys.acquired))
            .arg(self.key(&keys.released));
        for key in cooldown_keys {
            command.arg(self.key(key));
        }
        let result: Vec<redis::Value> = command
            .arg(max_age_ms)
            .arg(max_concurrent_requests)
            .arg(global_max_concurrent_requests)
            .arg(ttl_secs)
            .arg(lease_id)
            .arg(coordination_epoch)
            .query_async(&mut manager)
            .await?;
        let status = redis_value_i64(&result, 0)?.unwrap_or_default();
        let pool_in_flight_requests = redis_value_i64(&result, 2)?
            .unwrap_or_default()
            .clamp(0, u32::MAX as i64) as u32;
        let global_in_flight_requests = redis_value_i64(&result, 3)?
            .unwrap_or_default()
            .clamp(0, u32::MAX as i64) as u32;
        match status {
            1 => Ok(ExternalPoolLeaseAcquireResult::Acquired {
                lease_id: lease_id.to_string(),
                pool_in_flight_requests,
                global_in_flight_requests,
            }),
            2 => {
                let remaining_ms = redis_value_i64(&result, 1)?.unwrap_or(-1);
                let remaining = (remaining_ms >= 0)
                    .then(|| StdDuration::from_millis(remaining_ms.max(1) as u64));
                if redis_value_i64(&result, 4)?.unwrap_or(1) == 1 {
                    Ok(ExternalPoolLeaseAcquireResult::PoolCooldown { remaining })
                } else {
                    Ok(ExternalPoolLeaseAcquireResult::ModelCooldown { remaining })
                }
            }
            3 => Ok(ExternalPoolLeaseAcquireResult::PoolCapacityFull {
                in_flight_requests: pool_in_flight_requests,
            }),
            4 => Ok(ExternalPoolLeaseAcquireResult::GlobalCapacityFull {
                in_flight_requests: global_in_flight_requests,
            }),
            5 => Ok(ExternalPoolLeaseAcquireResult::Released),
            6 => Ok(ExternalPoolLeaseAcquireResult::CoordinatorEpochMismatch {
                coordination_epoch: redis_value_string(&result, 4)?.unwrap_or_default(),
            }),
            7 => Ok(ExternalPoolLeaseAcquireResult::CoordinatorRecovering {
                coordination_epoch: redis_value_string(&result, 4)?.unwrap_or_default(),
                remaining: StdDuration::from_millis(
                    redis_value_i64(&result, 1)?.unwrap_or(1).max(1) as u64,
                ),
            }),
            _ => anyhow::bail!("Redis returned an invalid external pool lease acquire status"),
        }
    }

    #[cfg(test)]
    pub async fn release_in_flight_lease(
        &self,
        credential_id: u64,
        lease_id: u64,
    ) -> anyhow::Result<bool> {
        self.release_in_flight_lease_inner(credential_id, lease_id, false, None)
            .await
    }

    #[cfg(test)]
    pub async fn release_in_flight_lease_with_tombstone(
        &self,
        credential_id: u64,
        lease_id: u64,
    ) -> anyhow::Result<bool> {
        self.release_in_flight_lease_inner(credential_id, lease_id, true, None)
            .await
    }

    pub async fn release_in_flight_lease_and_publish_wakeup(
        &self,
        credential_id: u64,
        lease_id: u64,
        tombstone: bool,
        wakeup_payload: &str,
    ) -> anyhow::Result<bool> {
        self.release_in_flight_lease_inner(credential_id, lease_id, tombstone, Some(wakeup_payload))
            .await
    }

    async fn release_in_flight_lease_inner(
        &self,
        credential_id: u64,
        lease_id: u64,
        tombstone: bool,
        wakeup_payload: Option<&str>,
    ) -> anyhow::Result<bool> {
        let keys = in_flight_keys(credential_id);
        let global_keys = global_in_flight_keys();
        let lease_id = lease_id.to_string();
        let mut manager = self.scheduler_capacity_manager();
        let script = r#"
            local lease_id = ARGV[1]
            local tombstone = tonumber(ARGV[2])
            local ttl_secs = tonumber(ARGV[3])

            if tombstone == 1 then
                redis.call('SADD', KEYS[11], lease_id)
                if ttl_secs > 0 then
                    redis.call('EXPIRE', KEYS[11], ttl_secs)
                end
            end

            local local_weight = tonumber(redis.call('HGET', KEYS[4], lease_id) or '1')
            local global_weight = tonumber(redis.call('HGET', KEYS[9], lease_id) or '1')
            local removed = redis.call('ZREM', KEYS[1], lease_id)
            local local_acquired_removed = redis.call('ZREM', KEYS[2], lease_id)
            removed = removed + local_acquired_removed
            removed = removed + redis.call('HDEL', KEYS[3], lease_id)
            removed = removed + redis.call('HDEL', KEYS[4], lease_id)
            if local_acquired_removed > 0 then
                local next_count = redis.call('DECRBY', KEYS[5], local_weight)
                if tonumber(next_count) < 0 then
                    redis.call('SET', KEYS[5], 0)
                end
            end
            removed = removed + redis.call('ZREM', KEYS[6], lease_id)
            local global_acquired_removed = redis.call('ZREM', KEYS[7], lease_id)
            removed = removed + global_acquired_removed
            removed = removed + redis.call('HDEL', KEYS[8], lease_id)
            removed = removed + redis.call('HDEL', KEYS[9], lease_id)
            if global_acquired_removed > 0 then
                local next_global_count = redis.call('DECRBY', KEYS[10], global_weight)
                if tonumber(next_global_count) < 0 then
                    redis.call('SET', KEYS[10], 0)
                end
            end
            if removed > 0 and ARGV[4] ~= '' then
                redis.call('PUBLISH', KEYS[12], ARGV[4])
            end
            return removed
        "#;
        let tombstone_ttl_secs = 120i64;
        let removed: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(12)
            .arg(self.key(&keys.last_seen))
            .arg(self.key(&keys.acquired))
            .arg(self.key(&keys.kind))
            .arg(self.key(&keys.weight))
            .arg(self.key(&keys.count))
            .arg(self.key(&global_keys.last_seen))
            .arg(self.key(&global_keys.acquired))
            .arg(self.key(&global_keys.kind))
            .arg(self.key(&global_keys.weight))
            .arg(self.key(&global_keys.count))
            .arg(self.key(&keys.released))
            .arg(self.dispatch_wakeup_channel())
            .arg(&lease_id)
            .arg(if tombstone { 1 } else { 0 })
            .arg(tombstone_ttl_secs)
            .arg(wakeup_payload.unwrap_or_default())
            .query_async(&mut manager)
            .await?;
        Ok(removed > 0)
    }

    #[cfg(test)]
    pub async fn release_external_pool_confirmed_lease(
        &self,
        pool_id: u64,
        lease_id: &str,
    ) -> anyhow::Result<bool> {
        let result = self
            .release_external_pool_leases_batch(&[ExternalPoolLeaseReleaseRequest {
                pool_id,
                lease_id: lease_id.to_string(),
                pending: false,
            }])
            .await?;
        Ok(result.first().is_some_and(|result| result.removed))
    }

    #[cfg(test)]
    pub async fn release_external_pool_pending_lease(
        &self,
        pool_id: u64,
        lease_id: &str,
    ) -> anyhow::Result<bool> {
        let result = self
            .release_external_pool_leases_batch(&[ExternalPoolLeaseReleaseRequest {
                pool_id,
                lease_id: lease_id.to_string(),
                pending: true,
            }])
            .await?;
        Ok(result.first().is_some_and(|result| result.removed))
    }

    pub async fn release_external_pool_leases_batch(
        &self,
        requests: &[ExternalPoolLeaseReleaseRequest],
    ) -> anyhow::Result<Vec<ExternalPoolLeaseReleaseResult>> {
        if requests.is_empty() {
            return Ok(Vec::new());
        }
        let script = r#"
            local request_count = tonumber(ARGV[1])
            local tombstone_ttl_ms = tonumber(ARGV[2])
            local redis_time = redis.call('TIME')
            local now = tonumber(redis_time[1]) * 1000 + math.floor(tonumber(redis_time[2]) / 1000)
            local tombstone_expires_at = now + tombstone_ttl_ms
            local result = {}

            local function command_failed(value)
                return type(value) == 'table' and value.err
            end

            for request_index = 1, request_count do
                local key_index = (request_index - 1) * 5
                local arg_index = 3 + (request_index - 1) * 2
                local pending = tonumber(ARGV[arg_index]) == 1
                local lease_id = ARGV[arg_index + 1]
                local completed = 1
                local removed = 0

                if pending then
                    local pruned = redis.pcall('ZREMRANGEBYSCORE', KEYS[key_index + 5], '-inf', now)
                    if command_failed(pruned) then
                        completed = 0
                    end
                    if completed == 1 then
                        local added = redis.pcall('ZADD', KEYS[key_index + 5], tombstone_expires_at, lease_id)
                        if command_failed(added) then
                            completed = 0
                        end
                    end
                    if completed == 1 then
                        local latest = redis.pcall('ZREVRANGE', KEYS[key_index + 5], 0, 0, 'WITHSCORES')
                        if command_failed(latest) then
                            completed = 0
                        elseif latest[2] then
                            local expiry = redis.pcall('PEXPIREAT', KEYS[key_index + 5], math.ceil(tonumber(latest[2])))
                            if command_failed(expiry) then
                                completed = 0
                            end
                        end
                    end
                end

                if completed == 1 then
                    for offset = 1, 4 do
                        local command_result = redis.pcall('ZREM', KEYS[key_index + offset], lease_id)
                        if command_failed(command_result) then
                            completed = 0
                        else
                            removed = removed + tonumber(command_result)
                        end
                    end
                end

                table.insert(result, completed)
                table.insert(result, removed)
            end

            return result
        "#;
        let global_keys = external_pool_global_in_flight_keys();
        let mut command = redis::cmd("EVAL");
        command.arg(script).arg(requests.len().saturating_mul(5));
        for request in requests {
            let keys = external_pool_in_flight_keys(request.pool_id);
            command
                .arg(self.key(&keys.last_seen))
                .arg(self.key(&keys.acquired))
                .arg(self.key(&global_keys.last_seen))
                .arg(self.key(&global_keys.acquired))
                .arg(self.key(&keys.released));
        }
        command
            .arg(requests.len())
            .arg(EXTERNAL_POOL_PENDING_LEASE_TOMBSTONE_TTL_MILLIS);
        for request in requests {
            command
                .arg(if request.pending { 1 } else { 0 })
                .arg(&request.lease_id);
        }
        let mut manager = self.scheduler_capacity_manager();
        let values: Vec<i64> = command.query_async(&mut manager).await?;
        if values.len() != requests.len().saturating_mul(2) {
            anyhow::bail!("Redis returned an incomplete external pool lease release batch");
        }
        Ok(values
            .chunks_exact(2)
            .map(|values| ExternalPoolLeaseReleaseResult {
                completed: values[0] == 1,
                removed: values[1] > 0,
            })
            .collect())
    }

    pub async fn touch_external_pool_lease(
        &self,
        pool_id: u64,
        lease_id: &str,
        ttl_secs: usize,
        coordination_epoch: &str,
    ) -> anyhow::Result<bool> {
        let keys = external_pool_in_flight_keys(pool_id);
        let global_keys = external_pool_global_in_flight_keys();
        let script = r#"
            local lease_id = ARGV[1]
            local expected_epoch = ARGV[3]
            local stored_epoch = redis.pcall('GET', KEYS[1])
            if type(stored_epoch) == 'table' and stored_epoch.err then
                return 0
            end
            if not stored_epoch or stored_epoch ~= expected_epoch then
                return 0
            end
            local recovery_ttl = redis.call('PTTL', KEYS[2])
            if recovery_ttl > 0 or recovery_ttl == -1 then
                return 0
            end
            local redis_time = redis.call('TIME')
            local now = tonumber(redis_time[1]) * 1000 + math.floor(tonumber(redis_time[2]) / 1000)
            local ttl_secs = tonumber(ARGV[2])

            if not redis.call('ZSCORE', KEYS[4], lease_id) then
                return 0
            end
            if not redis.call('ZSCORE', KEYS[6], lease_id) then
                return 0
            end

            redis.call('ZADD', KEYS[3], now, lease_id)
            redis.call('ZADD', KEYS[5], now, lease_id)
            if ttl_secs > 0 then
                redis.call('EXPIRE', KEYS[3], ttl_secs)
                redis.call('EXPIRE', KEYS[4], ttl_secs)
                redis.call('EXPIRE', KEYS[5], ttl_secs)
                redis.call('EXPIRE', KEYS[6], ttl_secs)
            end
            return 1
        "#;
        let mut manager = self.scheduler_capacity_manager();
        let touched: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(6)
            .arg(self.key(EXTERNAL_POOL_COORDINATOR_EPOCH_KEY))
            .arg(self.key(EXTERNAL_POOL_COORDINATOR_RECOVERY_KEY))
            .arg(self.key(&keys.last_seen))
            .arg(self.key(&keys.acquired))
            .arg(self.key(&global_keys.last_seen))
            .arg(self.key(&global_keys.acquired))
            .arg(lease_id)
            .arg(ttl_secs.max(1))
            .arg(coordination_epoch)
            .query_async(&mut manager)
            .await?;
        Ok(touched == 1)
    }

    #[cfg(test)]
    pub async fn external_pool_coordinator_snapshot(
        &self,
        pool_id: u64,
        max_age: Option<StdDuration>,
        cooldown_keys: &[String],
        coordination_epoch: &str,
    ) -> anyhow::Result<ExternalPoolCoordinatorSnapshot> {
        let mut snapshots = self
            .external_pool_coordinator_snapshots(
                &[ExternalPoolCoordinatorSnapshotRequest {
                    pool_id,
                    cooldown_keys: cooldown_keys.to_vec(),
                }],
                max_age,
                coordination_epoch,
            )
            .await?;
        snapshots
            .pop()
            .ok_or_else(|| anyhow::anyhow!("Redis returned no external pool coordinator snapshot"))
    }

    pub async fn external_pool_coordinator_snapshots(
        &self,
        requests: &[ExternalPoolCoordinatorSnapshotRequest],
        max_age: Option<StdDuration>,
        coordination_epoch: &str,
    ) -> anyhow::Result<Vec<ExternalPoolCoordinatorSnapshot>> {
        if requests.is_empty() {
            return Ok(Vec::new());
        }
        let max_age_ms = max_age.map(|age| age.as_millis() as i64).unwrap_or(0);
        let script = r#"
            local redis_time = redis.call('TIME')
            local now = tonumber(redis_time[1]) * 1000 + math.floor(tonumber(redis_time[2]) / 1000)
            local max_age_ms = tonumber(ARGV[1])
            local pool_count = tonumber(ARGV[2])
            local expected_epoch = ARGV[3 + pool_count]

            local stored_epoch = redis.pcall('GET', KEYS[1])
            if type(stored_epoch) == 'table' and stored_epoch.err then
                return {2, '', 0}
            end
            if not stored_epoch or stored_epoch ~= expected_epoch then
                return {2, stored_epoch or '', 0}
            end
            local recovery_ttl = redis.call('PTTL', KEYS[2])
            if recovery_ttl > 0 then
                return {3, stored_epoch, recovery_ttl}
            end
            if recovery_ttl == -1 then
                return {2, stored_epoch, 0}
            end
            if max_age_ms > 0 then
                local expired = redis.call('ZRANGEBYSCORE', KEYS[3], '-inf', now - max_age_ms)
                for _, member in ipairs(expired) do
                    redis.call('ZREM', KEYS[3], member)
                    redis.call('ZREM', KEYS[4], member)
                end
            end

            local global_count = redis.call('ZCARD', KEYS[3])
            local result = {1, stored_epoch, 0}
            local key_index = 5
            for pool_index = 1, pool_count do
                local cooldown_count = tonumber(ARGV[2 + pool_index])
                if max_age_ms > 0 then
                    local expired = redis.call('ZRANGEBYSCORE', KEYS[key_index], '-inf', now - max_age_ms)
                    for _, member in ipairs(expired) do
                        redis.call('ZREM', KEYS[key_index], member)
                        redis.call('ZREM', KEYS[key_index + 1], member)
                    end
                end
                table.insert(result, redis.call('ZCARD', KEYS[key_index]))
                table.insert(result, global_count)
                key_index = key_index + 2
                for _ = 1, cooldown_count do
                    local cooldown_value = redis.pcall('GET', KEYS[key_index])
                    if type(cooldown_value) == 'table' and cooldown_value.err then
                        table.insert(result, '__kiro_rs_invalid_redis_type__')
                    else
                        table.insert(result, cooldown_value or '')
                    end
                    table.insert(result, redis.call('PTTL', KEYS[key_index]))
                    key_index = key_index + 1
                end
            end
            return result
        "#;
        let global_keys = external_pool_global_in_flight_keys();
        let mut manager = self.scheduler_capacity_manager();
        let redis_key_count = requests.iter().fold(4usize, |count, request| {
            count.saturating_add(2usize.saturating_add(request.cooldown_keys.len()))
        });
        let mut command = redis::cmd("EVAL");
        command
            .arg(script)
            .arg(redis_key_count)
            .arg(self.key(EXTERNAL_POOL_COORDINATOR_EPOCH_KEY))
            .arg(self.key(EXTERNAL_POOL_COORDINATOR_RECOVERY_KEY))
            .arg(self.key(&global_keys.last_seen))
            .arg(self.key(&global_keys.acquired));
        for request in requests {
            let keys = external_pool_in_flight_keys(request.pool_id);
            command
                .arg(self.key(&keys.last_seen))
                .arg(self.key(&keys.acquired));
            for key in &request.cooldown_keys {
                command.arg(self.key(key));
            }
        }
        command.arg(max_age_ms).arg(requests.len());
        for request in requests {
            command.arg(request.cooldown_keys.len());
        }
        command.arg(coordination_epoch);
        #[cfg(test)]
        self.external_pool_hot_path_round_trips
            .fetch_add(1, Ordering::Relaxed);
        let result: Vec<redis::Value> = command.query_async(&mut manager).await?;
        let guard_state = decode_external_pool_coordinator_guard_state(&result)?;
        match guard_state {
            ExternalPoolCoordinatorGuardState::Ready { .. } => {}
            state => {
                return Err(anyhow::Error::new(ExternalPoolCoordinatorGuardError {
                    state,
                }));
            }
        }
        let expected_values = requests.iter().fold(3usize, |count, request| {
            count.saturating_add(
                2usize.saturating_add(request.cooldown_keys.len().saturating_mul(2)),
            )
        });
        if result.len() != expected_values {
            anyhow::bail!("Redis returned an incomplete external pool coordinator snapshot batch");
        }

        let mut cursor = 3usize;
        let mut snapshots = Vec::with_capacity(requests.len());
        for request in requests {
            let pool_in_flight_requests =
                redis::from_redis_value::<i64>(&result[cursor])?.max(0) as u32;
            cursor += 1;
            let global_in_flight_requests =
                redis::from_redis_value::<i64>(&result[cursor])?.max(0) as u32;
            cursor += 1;
            let mut cooldown_values = Vec::with_capacity(request.cooldown_keys.len());
            let mut cooldown_ttls = Vec::with_capacity(request.cooldown_keys.len());
            for _ in &request.cooldown_keys {
                let value = redis::from_redis_value::<String>(&result[cursor])?;
                cursor += 1;
                let ttl_ms = redis::from_redis_value::<i64>(&result[cursor])?;
                cursor += 1;
                cooldown_values.push((!value.is_empty()).then_some(value));
                cooldown_ttls
                    .push((ttl_ms >= 0).then(|| StdDuration::from_millis(ttl_ms.max(1) as u64)));
            }
            snapshots.push(ExternalPoolCoordinatorSnapshot {
                capacity: ExternalPoolCapacityState {
                    pool_in_flight_requests,
                    global_in_flight_requests,
                },
                cooldown_values,
                cooldown_ttls,
            });
        }
        Ok(snapshots)
    }

    #[cfg(test)]
    pub fn reset_external_pool_hot_path_round_trips(&self) {
        self.external_pool_hot_path_round_trips
            .store(0, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub fn external_pool_hot_path_round_trips(&self) -> u64 {
        self.external_pool_hot_path_round_trips
            .load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub fn reset_usage_summary_write_round_trips(&self) {
        self.usage_summary_write_round_trips
            .store(0, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub fn usage_summary_write_round_trips(&self) -> u64 {
        self.usage_summary_write_round_trips.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub async fn set_raw_string_for_test(
        &self,
        key: impl AsRef<str>,
        value: impl AsRef<str>,
    ) -> anyhow::Result<()> {
        let mut manager = self.manager.clone();
        let _: () = manager.set(self.key(key), value.as_ref()).await?;
        Ok(())
    }

    #[cfg(test)]
    pub fn reset_scheduler_state_round_trips(&self) {
        self.scheduler_state_round_trips.store(0, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub fn scheduler_state_round_trips(&self) -> u64 {
        self.scheduler_state_round_trips.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub fn set_scheduler_state_delay_millis(&self, delay_millis: u64) {
        self.scheduler_state_delay_millis
            .store(delay_millis, Ordering::Release);
    }

    #[cfg(test)]
    pub async fn set_scheduler_release_wrongtype_for_test(
        &self,
        credential_id: u64,
        poisoned: bool,
    ) -> anyhow::Result<()> {
        let key = self.key(&in_flight_keys(credential_id).weight);
        let mut manager = self.scheduler_capacity_manager();
        if poisoned {
            redis::cmd("SET")
                .arg(key)
                .arg("wrongtype")
                .query_async::<()>(&mut manager)
                .await?;
        } else {
            redis::cmd("DEL")
                .arg(key)
                .query_async::<()>(&mut manager)
                .await?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub async fn set_external_release_wrongtype_for_test(
        &self,
        pool_id: u64,
        poisoned: bool,
    ) -> anyhow::Result<()> {
        let key = self.key(&external_pool_in_flight_keys(pool_id).last_seen);
        let mut manager = self.scheduler_capacity_manager();
        if poisoned {
            redis::cmd("SET")
                .arg(key)
                .arg("wrongtype")
                .query_async::<()>(&mut manager)
                .await?;
        } else {
            redis::cmd("DEL")
                .arg(key)
                .query_async::<()>(&mut manager)
                .await?;
        }
        Ok(())
    }

    pub async fn record_local_pool_circuit_failure(
        &self,
        credential_id: Option<u64>,
        reason: &str,
        window: StdDuration,
        open_after_failures: u32,
        require_distinct_credentials: u32,
        open_for: StdDuration,
    ) -> anyhow::Result<LocalPoolCircuitState> {
        let now = now_ms();
        let window_ms = window.as_millis().max(1) as i64;
        let open_for_ms = open_for.as_millis().max(1) as i64;
        let credential_member = credential_id
            .map(|id| format!("credential:{}", id))
            .unwrap_or_else(|| "unknown".to_string());
        let script = r#"
            local now = tonumber(ARGV[1])
            local window_ms = tonumber(ARGV[2])
            local credential = ARGV[3]
            local reason = ARGV[4]
            local open_after = tonumber(ARGV[5])
            local required_distinct = tonumber(ARGV[6])
            local open_for_ms = tonumber(ARGV[7])
            local ttl_secs = tonumber(ARGV[8])

            redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', now - window_ms)
            local seq = redis.call('INCR', KEYS[2])
            redis.call('PEXPIRE', KEYS[2], (ttl_secs * 1000))
            redis.call('ZADD', KEYS[1], now, 'failure:' .. tostring(seq) .. ':' .. credential)
            redis.call('ZADD', KEYS[1], now, credential)

            local failure_members = redis.call('ZRANGEBYSCORE', KEYS[1], now - window_ms + 1, now)
            local failures = 0
            local distinct_credentials = {}
            for _, member in ipairs(failure_members) do
                if string.sub(member, 1, 8) == 'failure:' then
                    failures = failures + 1
                else
                    distinct_credentials[member] = true
                end
            end
            local distinct = 0
            for _, _ in pairs(distinct_credentials) do
                distinct = distinct + 1
            end
            local open_until = tonumber(redis.call('GET', KEYS[3]) or '0')
            local reported_reason = ''
            local opened = 0
            if open_until > now then
                opened = 1
                reported_reason = redis.call('GET', KEYS[4]) or reason
            elseif failures >= open_after and distinct >= required_distinct then
                open_until = now + open_for_ms
                redis.call('SET', KEYS[3], open_until, 'PX', open_for_ms)
                redis.call('SET', KEYS[4], reason, 'PX', open_for_ms)
                opened = 1
                reported_reason = reason
            else
                open_until = 0
            end

            redis.call('EXPIRE', KEYS[1], ttl_secs)
            return {opened, open_until, reported_reason, failures, distinct}
        "#;
        let ttl_secs = window.as_secs().saturating_add(open_for.as_secs()).max(1) as usize;
        let mut manager = self.scheduler_manager();
        let result: Vec<redis::Value> = redis::cmd("EVAL")
            .arg(script)
            .arg(4)
            .arg(self.key(local_pool_circuit_failures_key()))
            .arg(self.key(local_pool_circuit_sequence_key()))
            .arg(self.key(local_pool_circuit_open_until_key()))
            .arg(self.key(local_pool_circuit_reason_key()))
            .arg(now)
            .arg(window_ms)
            .arg(credential_member)
            .arg(reason)
            .arg(open_after_failures.max(1))
            .arg(require_distinct_credentials.max(1))
            .arg(open_for_ms)
            .arg(ttl_secs)
            .query_async(&mut manager)
            .await?;
        Ok(LocalPoolCircuitState {
            open: redis::from_redis_value::<i64>(result.first().unwrap_or(&redis::Value::Nil))
                .unwrap_or(0)
                == 1,
            open_until_ms: redis::from_redis_value::<i64>(
                result.get(1).unwrap_or(&redis::Value::Nil),
            )
            .ok()
            .filter(|value| *value > now),
            reason: redis::from_redis_value::<String>(result.get(2).unwrap_or(&redis::Value::Nil))
                .ok()
                .filter(|value| !value.is_empty()),
            recent_failures: redis::from_redis_value::<i64>(
                result.get(3).unwrap_or(&redis::Value::Nil),
            )
            .unwrap_or(0)
            .max(0) as u32,
            distinct_credentials: redis::from_redis_value::<i64>(
                result.get(4).unwrap_or(&redis::Value::Nil),
            )
            .unwrap_or(0)
            .max(0) as u32,
        })
    }

    pub async fn local_pool_circuit_state(
        &self,
        window: StdDuration,
    ) -> anyhow::Result<LocalPoolCircuitState> {
        let now = now_ms();
        let window_ms = window.as_millis().max(1) as i64;
        let mut manager = self.scheduler_manager();
        let script = r#"
            local now = tonumber(ARGV[1])
            redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', now - tonumber(ARGV[2]))

            local members = redis.call('ZRANGE', KEYS[1], 0, -1)
            local failures = 0
            local distinct_credentials = {}
            for _, member in ipairs(members) do
                if string.sub(member, 1, 8) == 'failure:' then
                    failures = failures + 1
                else
                    distinct_credentials[member] = true
                end
            end
            local distinct = 0
            for _, _ in pairs(distinct_credentials) do
                distinct = distinct + 1
            end

            return {
                redis.call('GET', KEYS[2]) or false,
                redis.call('GET', KEYS[3]) or false,
                failures,
                distinct
            }
        "#;
        let result: Vec<redis::Value> = redis::cmd("EVAL")
            .arg(script)
            .arg(3)
            .arg(self.key(local_pool_circuit_failures_key()))
            .arg(self.key(local_pool_circuit_open_until_key()))
            .arg(self.key(local_pool_circuit_reason_key()))
            .arg(now)
            .arg(window_ms)
            .query_async(&mut manager)
            .await?;
        let open_until =
            redis::from_redis_value::<Option<i64>>(result.first().unwrap_or(&redis::Value::Nil))
                .unwrap_or(None);
        let reason =
            redis::from_redis_value::<Option<String>>(result.get(1).unwrap_or(&redis::Value::Nil))
                .unwrap_or(None);
        let recent_failures =
            redis::from_redis_value::<i64>(result.get(2).unwrap_or(&redis::Value::Nil))
                .unwrap_or(0);
        let distinct = redis::from_redis_value::<i64>(result.get(3).unwrap_or(&redis::Value::Nil))
            .unwrap_or(0);
        let open_until_ms = open_until.filter(|until| *until > now);
        if open_until.is_some() && open_until_ms.is_none() {
            let _: () = redis::pipe()
                .cmd("DEL")
                .arg(self.key(local_pool_circuit_open_until_key()))
                .cmd("DEL")
                .arg(self.key(local_pool_circuit_reason_key()))
                .query_async(&mut manager)
                .await
                .unwrap_or(());
        }
        let open = open_until_ms.is_some();
        let reason = if open { reason } else { None };
        Ok(LocalPoolCircuitState {
            open,
            open_until_ms,
            reason,
            recent_failures: recent_failures.max(0) as u32,
            distinct_credentials: distinct.max(0) as u32,
        })
    }

    pub async fn touch_in_flight_lease(
        &self,
        credential_id: u64,
        lease_id: u64,
    ) -> anyhow::Result<()> {
        let keys = in_flight_keys(credential_id);
        let global_keys = global_in_flight_keys();
        let mut manager = self.scheduler_capacity_manager();
        let lease_id = lease_id.to_string();
        let script = r#"
            local now = tonumber(ARGV[1])
            local lease_id = ARGV[2]

            if not redis.call('ZSCORE', KEYS[2], lease_id)
                or not redis.call('ZSCORE', KEYS[4], lease_id)
            then
                redis.call('ZREM', KEYS[1], lease_id)
                redis.call('ZREM', KEYS[3], lease_id)
                return 0
            end

            redis.call('ZADD', KEYS[1], now, lease_id)
            redis.call('ZADD', KEYS[3], now, lease_id)
            return 1
        "#;
        let _: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(4)
            .arg(self.key(&keys.last_seen))
            .arg(self.key(&keys.acquired))
            .arg(self.key(&global_keys.last_seen))
            .arg(self.key(&global_keys.acquired))
            .arg(now_ms())
            .arg(&lease_id)
            .query_async(&mut manager)
            .await?;
        Ok(())
    }

    pub async fn set_in_flight_lease_kind(
        &self,
        credential_id: u64,
        lease_id: u64,
        kind: &str,
    ) -> anyhow::Result<()> {
        let keys = in_flight_keys(credential_id);
        let global_keys = global_in_flight_keys();
        let lease_id = lease_id.to_string();
        let mut manager = self.scheduler_capacity_manager();
        let script = r#"
            local now = tonumber(ARGV[1])
            local lease_id = ARGV[2]
            local kind = ARGV[3]

            if not redis.call('ZSCORE', KEYS[2], lease_id)
                or not redis.call('ZSCORE', KEYS[5], lease_id)
            then
                redis.call('ZREM', KEYS[1], lease_id)
                redis.call('HDEL', KEYS[3], lease_id)
                redis.call('ZREM', KEYS[4], lease_id)
                redis.call('HDEL', KEYS[6], lease_id)
                return 0
            end

            redis.call('ZADD', KEYS[1], now, lease_id)
            redis.call('HSET', KEYS[3], lease_id, kind)
            redis.call('ZADD', KEYS[4], now, lease_id)
            redis.call('HSET', KEYS[6], lease_id, kind)
            return 1
        "#;
        let _: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(6)
            .arg(self.key(&keys.last_seen))
            .arg(self.key(&keys.acquired))
            .arg(self.key(&keys.kind))
            .arg(self.key(&global_keys.last_seen))
            .arg(self.key(&global_keys.acquired))
            .arg(self.key(&global_keys.kind))
            .arg(now_ms())
            .arg(&lease_id)
            .arg(kind)
            .query_async(&mut manager)
            .await?;
        Ok(())
    }

    pub async fn cleanup_expired_in_flight_leases(
        &self,
        credential_ids: &[u64],
        max_age: StdDuration,
    ) -> anyhow::Result<usize> {
        let now = now_ms();
        let max_age_ms = max_age.as_millis() as i64;
        let script = r#"
            local now = tonumber(ARGV[1])
            local max_age_ms = tonumber(ARGV[2])
            local expired = redis.call('ZRANGEBYSCORE', KEYS[1], '-inf', now - max_age_ms)
            for _, member in ipairs(expired) do
                local local_weight = tonumber(redis.call('HGET', KEYS[4], member) or '1')
                local global_weight = tonumber(redis.call('HGET', KEYS[9], member) or '1')
                redis.call('ZREM', KEYS[1], member)
                local local_acquired_removed = redis.call('ZREM', KEYS[2], member)
                redis.call('HDEL', KEYS[3], member)
                redis.call('HDEL', KEYS[4], member)
                if local_acquired_removed > 0 then
                    local next_count = redis.call('DECRBY', KEYS[5], local_weight)
                    if tonumber(next_count) < 0 then
                        redis.call('SET', KEYS[5], 0)
                    end
                end
                redis.call('ZREM', KEYS[6], member)
                local global_acquired_removed = redis.call('ZREM', KEYS[7], member)
                redis.call('HDEL', KEYS[8], member)
                redis.call('HDEL', KEYS[9], member)
                if global_acquired_removed > 0 then
                    local next_global_count = redis.call('DECRBY', KEYS[10], global_weight)
                    if tonumber(next_global_count) < 0 then
                        redis.call('SET', KEYS[10], 0)
                    end
                end
            end
            return #expired
        "#;
        let mut manager = self.scheduler_capacity_manager();
        let mut cleaned = 0usize;
        let global_keys = global_in_flight_keys();
        for credential_id in credential_ids {
            let keys = in_flight_keys(*credential_id);
            let removed: i64 = redis::cmd("EVAL")
                .arg(script)
                .arg(10)
                .arg(self.key(&keys.last_seen))
                .arg(self.key(&keys.acquired))
                .arg(self.key(&keys.kind))
                .arg(self.key(&keys.weight))
                .arg(self.key(&keys.count))
                .arg(self.key(&global_keys.last_seen))
                .arg(self.key(&global_keys.acquired))
                .arg(self.key(&global_keys.kind))
                .arg(self.key(&global_keys.weight))
                .arg(self.key(&global_keys.count))
                .arg(now)
                .arg(max_age_ms)
                .query_async(&mut manager)
                .await?;
            cleaned += removed.max(0) as usize;
        }
        Ok(cleaned)
    }

    pub async fn clear_in_flight_leases(
        &self,
        credential_id: u64,
        min_idle: Option<StdDuration>,
    ) -> anyhow::Result<usize> {
        let keys = in_flight_keys(credential_id);
        let global_keys = global_in_flight_keys();
        let mut manager = self.scheduler_capacity_manager();
        if let Some(min_idle) = min_idle {
            let cutoff = now_ms() - min_idle.as_millis() as i64;
            let script = r#"
                local expired = redis.call('ZRANGEBYSCORE', KEYS[1], '-inf', ARGV[1])
                for _, member in ipairs(expired) do
                    local local_weight = tonumber(redis.call('HGET', KEYS[4], member) or '1')
                    local global_weight = tonumber(redis.call('HGET', KEYS[9], member) or '1')
                    redis.call('ZREM', KEYS[1], member)
                    local local_acquired_removed = redis.call('ZREM', KEYS[2], member)
                    redis.call('HDEL', KEYS[3], member)
                    redis.call('HDEL', KEYS[4], member)
                    if local_acquired_removed > 0 then
                        local next_count = redis.call('DECRBY', KEYS[5], local_weight)
                        if tonumber(next_count) < 0 then
                            redis.call('SET', KEYS[5], 0)
                        end
                    end
                    redis.call('ZREM', KEYS[6], member)
                    local global_acquired_removed = redis.call('ZREM', KEYS[7], member)
                    redis.call('HDEL', KEYS[8], member)
                    redis.call('HDEL', KEYS[9], member)
                    if global_acquired_removed > 0 then
                        local next_global_count = redis.call('DECRBY', KEYS[10], global_weight)
                        if tonumber(next_global_count) < 0 then
                            redis.call('SET', KEYS[10], 0)
                        end
                    end
                end
                return #expired
            "#;
            let removed: i64 = redis::cmd("EVAL")
                .arg(script)
                .arg(10)
                .arg(self.key(&keys.last_seen))
                .arg(self.key(&keys.acquired))
                .arg(self.key(&keys.kind))
                .arg(self.key(&keys.weight))
                .arg(self.key(&keys.count))
                .arg(self.key(&global_keys.last_seen))
                .arg(self.key(&global_keys.acquired))
                .arg(self.key(&global_keys.kind))
                .arg(self.key(&global_keys.weight))
                .arg(self.key(&global_keys.count))
                .arg(cutoff)
                .query_async(&mut manager)
                .await?;
            return Ok(removed.max(0) as usize);
        }

        let count: i64 = manager.zcard(self.key(&keys.last_seen)).await.unwrap_or(0);
        let script = r#"
            local leases = redis.call('ZRANGE', KEYS[1], 0, -1)
            for _, member in ipairs(leases) do
                local global_weight = tonumber(redis.call('HGET', KEYS[9], member) or '1')
                redis.call('ZREM', KEYS[6], member)
                local global_acquired_removed = redis.call('ZREM', KEYS[7], member)
                redis.call('HDEL', KEYS[8], member)
                redis.call('HDEL', KEYS[9], member)
                if global_acquired_removed > 0 then
                    local next_global_count = redis.call('DECRBY', KEYS[10], global_weight)
                    if tonumber(next_global_count) < 0 then
                        redis.call('SET', KEYS[10], 0)
                    end
                end
            end
            redis.call('DEL', KEYS[1], KEYS[2], KEYS[3], KEYS[4], KEYS[5])
            return #leases
        "#;
        let _: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(10)
            .arg(self.key(&keys.last_seen))
            .arg(self.key(&keys.acquired))
            .arg(self.key(&keys.kind))
            .arg(self.key(&keys.weight))
            .arg(self.key(&keys.count))
            .arg(self.key(&global_keys.last_seen))
            .arg(self.key(&global_keys.acquired))
            .arg(self.key(&global_keys.kind))
            .arg(self.key(&global_keys.weight))
            .arg(self.key(&global_keys.count))
            .query_async(&mut manager)
            .await?;
        Ok(count.max(0) as usize)
    }

    pub async fn scheduler_state_for_credentials(
        &self,
        credential_ids: &[u64],
    ) -> anyhow::Result<HashMap<u64, SchedulerCredentialState>> {
        let mut states = HashMap::with_capacity(credential_ids.len());
        if credential_ids.is_empty() {
            return Ok(states);
        }

        let query_now = now_ms();
        let mut manager = self.scheduler_manager();
        let mut values = Vec::with_capacity(credential_ids.len() * 11);
        for batch in credential_ids.chunks(SCHEDULER_STATE_BATCH_SIZE) {
            let mut command = redis::cmd("EVAL");
            command
                .arg(SCHEDULER_STATE_BATCH_SCRIPT)
                .arg(batch.len() * 9);
            for credential_id in batch {
                let keys = in_flight_keys(*credential_id);
                command
                    .arg(self.key(scheduler_cooldown_key(*credential_id)))
                    .arg(self.key(scheduler_health_key(*credential_id)))
                    .arg(self.key(scheduler_rate_limit_key(*credential_id)))
                    .arg(self.key(&keys.last_seen))
                    .arg(self.key(&keys.acquired))
                    .arg(self.key(&keys.kind))
                    .arg(self.key(&keys.weight))
                    .arg(self.key(scheduler_selection_window_key(*credential_id)))
                    .arg(self.key(scheduler_model_index_key(*credential_id)));
            }
            command.arg(query_now);
            #[cfg(test)]
            {
                self.scheduler_state_round_trips
                    .fetch_add(1, Ordering::Relaxed);
                let delay_millis = self.scheduler_state_delay_millis.load(Ordering::Acquire);
                if delay_millis > 0 {
                    tokio::time::sleep(StdDuration::from_millis(delay_millis)).await;
                }
            }
            let batch_values: Vec<redis::Value> = command.query_async(&mut manager).await?;
            if batch_values.len() != batch.len() * 11 {
                anyhow::bail!(
                    "Redis 调度状态批量读取返回数量异常：期望 {}，实际 {}",
                    batch.len() * 11,
                    batch_values.len()
                );
            }
            values.extend(batch_values);
            tokio::task::yield_now().await;
        }
        let now = now_ms();
        let mut keys_to_delete = Vec::new();
        let mut indexed_models: Vec<(u64, String, String)> = Vec::new();
        for (index, credential_id) in credential_ids.iter().enumerate() {
            let base = index * 11;
            let cooldown_raw: Option<String> = redis::from_redis_value(&values[base])?;
            let health_raw: Option<String> = redis::from_redis_value(&values[base + 1])?;
            let rate_raw: Option<String> = redis::from_redis_value(&values[base + 2])?;
            let last_seen: Vec<(String, f64)> = redis::from_redis_value(&values[base + 3])?;
            let acquired: Vec<(String, f64)> = redis::from_redis_value(&values[base + 4])?;
            let kinds: HashMap<String, String> = redis::from_redis_value(&values[base + 5])?;
            let weights: HashMap<String, String> = redis::from_redis_value(&values[base + 6])?;
            let recent_10s: i64 = redis::from_redis_value(&values[base + 7])?;
            let recent_60s: i64 = redis::from_redis_value(&values[base + 8])?;
            let recent_5m: i64 = redis::from_redis_value(&values[base + 9])?;
            let model_index: HashMap<String, String> = redis::from_redis_value(&values[base + 10])?;
            for (hash, model) in model_index {
                if !hash.is_empty() && !model.trim().is_empty() {
                    indexed_models.push((*credential_id, hash, model));
                }
            }

            let cooldown = cooldown_raw
                .as_deref()
                .and_then(|raw| serde_json::from_str::<SchedulerCooldownState>(raw).ok())
                .and_then(|state| {
                    if state.until_ms <= now {
                        keys_to_delete.push(scheduler_cooldown_key(*credential_id));
                        None
                    } else {
                        Some(state)
                    }
                });
            let rate_limit_available_at_ms = rate_raw
                .as_deref()
                .and_then(|raw| raw.parse::<i64>().ok())
                .and_then(|until_ms| {
                    if until_ms <= now {
                        keys_to_delete.push(scheduler_rate_limit_key(*credential_id));
                        None
                    } else {
                        Some(until_ms)
                    }
                });
            let mut health = health_raw
                .as_deref()
                .and_then(|raw| serde_json::from_str::<SchedulerHealthState>(raw).ok())
                .unwrap_or_default();
            health.recent_selection_count_10s = recent_10s.max(0).min(u32::MAX as i64) as u32;
            health.recent_selection_count_60s = recent_60s.max(0).min(u32::MAX as i64) as u32;
            health.recent_selection_count_5m = recent_5m.max(0).min(u32::MAX as i64) as u32;
            let acquired_map: HashMap<String, i64> = acquired
                .into_iter()
                .map(|(member, score)| (member, score as i64))
                .collect();
            let in_flight_leases = last_seen
                .into_iter()
                .filter_map(|(member, last_seen_score)| {
                    let id = member.parse::<u64>().ok()?;
                    let acquired_at_ms = acquired_map.get(&member).copied()?;
                    Some(SchedulerInFlightLease {
                        id,
                        acquired_at_ms,
                        last_seen_at_ms: last_seen_score as i64,
                        kind: kinds
                            .get(&member)
                            .cloned()
                            .unwrap_or_else(|| "api".to_string()),
                        weight_units: weights
                            .get(&member)
                            .and_then(|value| value.parse::<u32>().ok())
                            .unwrap_or(1)
                            .clamp(1, 64),
                    })
                })
                .collect();
            states.insert(
                *credential_id,
                SchedulerCredentialState {
                    cooldown,
                    health,
                    model_states: Vec::new(),
                    rate_limit_available_at_ms,
                    in_flight_leases,
                },
            );
        }
        if !indexed_models.is_empty() {
            let mut model_pipe = redis::pipe();
            for (credential_id, hash, _) in &indexed_models {
                model_pipe
                    .cmd("GET")
                    .arg(self.key(scheduler_model_cooldown_key(*credential_id, hash)))
                    .cmd("GET")
                    .arg(self.key(scheduler_model_health_key(*credential_id, hash)));
            }
            let model_values: Vec<redis::Value> = model_pipe.query_async(&mut manager).await?;
            for (index, (credential_id, hash, model)) in indexed_models.into_iter().enumerate() {
                let base = index * 2;
                let cooldown_raw: Option<String> = redis::from_redis_value(&model_values[base])?;
                let health_raw: Option<String> = redis::from_redis_value(&model_values[base + 1])?;
                let cooldown = cooldown_raw
                    .as_deref()
                    .and_then(|raw| serde_json::from_str::<SchedulerCooldownState>(raw).ok())
                    .and_then(|mut state| {
                        if state.until_ms <= now {
                            keys_to_delete.push(scheduler_model_cooldown_key(credential_id, &hash));
                            None
                        } else {
                            if state.model.is_none() {
                                state.model = Some(model.clone());
                            }
                            Some(state)
                        }
                    });
                let health = health_raw
                    .as_deref()
                    .and_then(|raw| serde_json::from_str::<SchedulerHealthState>(raw).ok())
                    .unwrap_or_default();
                if let Some(state) = states.get_mut(&credential_id) {
                    state.model_states.push(SchedulerModelState {
                        model,
                        cooldown,
                        health,
                    });
                }
            }
        }
        if !keys_to_delete.is_empty() {
            let mut manager = self.scheduler_manager();
            let full_keys: Vec<String> = keys_to_delete
                .into_iter()
                .map(|key| self.key(key))
                .collect();
            let _: () = manager.del(full_keys).await?;
        }
        Ok(states)
    }

    #[allow(dead_code)]
    pub async fn global_capacity_state(&self) -> anyhow::Result<SchedulerGlobalCapacityState> {
        let keys = global_in_flight_keys();
        let mut manager = self.scheduler_capacity_manager();
        let (weighted_in_flight, zcard_in_flight, queued): (Option<i64>, i64, i64) = redis::pipe()
            .cmd("GET")
            .arg(self.key(&keys.count))
            .cmd("ZCARD")
            .arg(self.key(&keys.acquired))
            .cmd("EVAL")
            .arg(DISPATCH_QUEUE_PRUNE_AND_COUNT_SCRIPT)
            .arg(1)
            .arg(self.key(scheduler_global_queue_key()))
            .query_async(&mut manager)
            .await?;
        let in_flight = weighted_in_flight.unwrap_or(zcard_in_flight);
        Ok(SchedulerGlobalCapacityState {
            in_flight_requests: in_flight.max(0) as u32,
            queued_requests: queued.max(0) as u32,
        })
    }

    pub async fn try_enter_dispatch_queue(
        &self,
        lease_id: &str,
        max_queued: u32,
        ttl_secs: u64,
    ) -> anyhow::Result<bool> {
        let mut manager = self.scheduler_capacity_manager();
        let admitted: i64 = redis::cmd("EVAL")
            .arg(DISPATCH_QUEUE_ADMIT_SCRIPT)
            .arg(1)
            .arg(self.key(scheduler_global_queue_key()))
            .arg(max_queued)
            .arg(ttl_secs.max(60))
            .arg(lease_id)
            .query_async(&mut manager)
            .await?;
        Ok(admitted == 1)
    }

    pub async fn leave_dispatch_queue(&self, lease_id: &str) -> anyhow::Result<bool> {
        let mut manager = self.scheduler_capacity_manager();
        let removed: i64 = redis::cmd("EVAL")
            .arg(DISPATCH_QUEUE_RELEASE_SCRIPT)
            .arg(1)
            .arg(self.key(scheduler_global_queue_key()))
            .arg(lease_id)
            .query_async(&mut manager)
            .await?;
        Ok(removed > 0)
    }

    pub async fn renew_dispatch_queue(
        &self,
        lease_id: &str,
        ttl_secs: u64,
    ) -> anyhow::Result<bool> {
        let mut manager = self.scheduler_capacity_manager();
        let renewed: i64 = redis::cmd("EVAL")
            .arg(DISPATCH_QUEUE_RENEW_SCRIPT)
            .arg(1)
            .arg(self.key(scheduler_global_queue_key()))
            .arg(ttl_secs.max(60))
            .arg(lease_id)
            .query_async(&mut manager)
            .await?;
        Ok(renewed == 1)
    }

    #[cfg(test)]
    pub async fn dispatch_queue_lease_deadlines_ms(&self) -> anyhow::Result<Vec<(String, f64)>> {
        let mut manager = self.scheduler_capacity_manager();
        redis::cmd("ZRANGE")
            .arg(self.key(scheduler_global_queue_key()))
            .arg(0)
            .arg(-1)
            .arg("WITHSCORES")
            .query_async(&mut manager)
            .await
            .map_err(Into::into)
    }

    #[cfg(test)]
    pub async fn server_time_ms(&self) -> anyhow::Result<u64> {
        let mut manager = self.scheduler_capacity_manager();
        let (seconds, micros): (u64, u64) = redis::cmd("TIME").query_async(&mut manager).await?;
        Ok(seconds.saturating_mul(1_000).saturating_add(micros / 1_000))
    }

    pub async fn try_enter_external_pool_dispatch_queue(
        &self,
        lease_id: &str,
        max_queued: u32,
        ttl_secs: u64,
    ) -> anyhow::Result<bool> {
        let mut manager = self.scheduler_capacity_manager();
        let admitted: i64 = redis::cmd("EVAL")
            .arg(DISPATCH_QUEUE_ADMIT_SCRIPT)
            .arg(1)
            .arg(self.key(external_pool_global_queue_key()))
            .arg(max_queued)
            .arg(ttl_secs.max(60))
            .arg(lease_id)
            .query_async(&mut manager)
            .await?;
        Ok(admitted == 1)
    }

    pub async fn renew_external_pool_dispatch_queue(
        &self,
        lease_id: &str,
        ttl_secs: u64,
    ) -> anyhow::Result<bool> {
        let mut manager = self.scheduler_capacity_manager();
        let renewed: i64 = redis::cmd("EVAL")
            .arg(DISPATCH_QUEUE_RENEW_SCRIPT)
            .arg(1)
            .arg(self.key(external_pool_global_queue_key()))
            .arg(ttl_secs.max(60))
            .arg(lease_id)
            .query_async(&mut manager)
            .await?;
        Ok(renewed == 1)
    }

    pub async fn leave_external_pool_dispatch_queue(&self, lease_id: &str) -> anyhow::Result<bool> {
        let mut manager = self.scheduler_capacity_manager();
        let removed: i64 = redis::cmd("EVAL")
            .arg(DISPATCH_QUEUE_RELEASE_SCRIPT)
            .arg(1)
            .arg(self.key(external_pool_global_queue_key()))
            .arg(lease_id)
            .query_async(&mut manager)
            .await?;
        Ok(removed > 0)
    }

    #[cfg(test)]
    pub async fn external_pool_dispatch_queue_size(&self) -> anyhow::Result<u32> {
        let mut manager = self.scheduler_capacity_manager();
        let count: i64 = redis::cmd("EVAL")
            .arg(DISPATCH_QUEUE_PRUNE_AND_COUNT_SCRIPT)
            .arg(1)
            .arg(self.key(external_pool_global_queue_key()))
            .query_async(&mut manager)
            .await?;
        Ok(count.max(0) as u32)
    }

    pub async fn acquire_refresh_lock(
        &self,
        credential_id: u64,
        ttl_secs: usize,
    ) -> anyhow::Result<Option<String>> {
        let token = uuid::Uuid::new_v4().to_string();
        let acquired = self
            .set_nx_ex(scheduler_refresh_lock_key(credential_id), &token, ttl_secs)
            .await?;
        Ok(acquired.then_some(token))
    }

    pub async fn release_refresh_lock(
        &self,
        credential_id: u64,
        token: &str,
    ) -> anyhow::Result<bool> {
        self.release_lock(scheduler_refresh_lock_key(credential_id), token)
            .await
    }

    pub(crate) async fn begin_token_refresh(
        &self,
        credential_id: u64,
        identity: [u8; 32],
        caller_can_claim_health: bool,
    ) -> anyhow::Result<RedisRefreshBegin> {
        let owner = uuid::Uuid::new_v4().to_string();
        let health_claim_token = uuid::Uuid::new_v4().to_string();
        let mut manager = self.scheduler_capacity_manager();
        let values: Vec<String> = redis::cmd("EVAL")
            .arg(TOKEN_REFRESH_BEGIN_SCRIPT)
            .arg(2)
            .arg(self.key(scheduler_refresh_lock_key(credential_id)))
            .arg(self.key(scheduler_refresh_outcome_key(credential_id)))
            .arg(&owner)
            .arg(refresh_identity_hex(&identity))
            .arg(TOKEN_REFRESH_LOCK_TTL_MS)
            .arg(if caller_can_claim_health { 1 } else { 0 })
            .arg(&health_claim_token)
            .arg(TOKEN_REFRESH_HEALTH_CLAIM_TTL_MS)
            .arg(TOKEN_REFRESH_POLL_AFTER_MS)
            .arg(TOKEN_REFRESH_NEGATIVE_STREAK_RESET_MS)
            .query_async(&mut manager)
            .await?;
        decode_refresh_begin(
            &values,
            credential_id,
            identity,
            &owner,
            &health_claim_token,
        )
    }

    pub(crate) async fn complete_token_refresh_failure(
        &self,
        lease: &RedisRefreshLease,
        failure: &RedisRefreshFailure,
        leader_can_claim_health: bool,
    ) -> anyhow::Result<Option<RedisRefreshFailureCommit>> {
        let (consecutive_failures, delay) = refresh_failure_delay(lease, failure.retry_after);
        let delay_ms = delay.as_millis().min(u128::from(u64::MAX)) as u64;
        let outcome_ttl_ms = delay_ms
            .saturating_add(TOKEN_REFRESH_FAILURE_RETENTION_MS)
            .min(TOKEN_REFRESH_OUTCOME_MAX_TTL_MS)
            .max(1);
        let health_claim_token = uuid::Uuid::new_v4().to_string();
        let failure_fence_token = uuid::Uuid::new_v4().to_string();
        let retry_after_ms = failure
            .retry_after
            .map(|duration| {
                duration
                    .as_millis()
                    .min(u128::from(TOKEN_REFRESH_NEGATIVE_RETRY_AFTER_MAX_MS))
                    as u64
            })
            .map(|value| value.to_string())
            .unwrap_or_default();
        let status = failure
            .status
            .map(|value| value.to_string())
            .unwrap_or_default();
        let mut manager = self.scheduler_capacity_manager();
        let values: Vec<String> = redis::cmd("EVAL")
            .arg(TOKEN_REFRESH_COMPLETE_FAILURE_SCRIPT)
            .arg(2)
            .arg(self.key(scheduler_refresh_lock_key(lease.credential_id)))
            .arg(self.key(scheduler_refresh_outcome_key(lease.credential_id)))
            .arg(&lease.owner)
            .arg(lease.generation)
            .arg(refresh_identity_hex(&lease.identity))
            .arg(failure.stage.as_str())
            .arg(failure.kind.as_str())
            .arg(status)
            .arg(retry_after_ms)
            .arg(if failure.send_committed { 1 } else { 0 })
            .arg(if failure.health_action_required { 1 } else { 0 })
            .arg(if leader_can_claim_health { 1 } else { 0 })
            .arg(&health_claim_token)
            .arg(delay_ms)
            .arg(outcome_ttl_ms)
            .arg(TOKEN_REFRESH_HEALTH_CLAIM_TTL_MS)
            .arg(failure_fence_token)
            .arg(consecutive_failures)
            .query_async(&mut manager)
            .await?;
        if values.len() == 1 && values[0] == "stale" {
            return Ok(None);
        }
        decode_refresh_failure_outcome(
            &values,
            "committed",
            Some(lease.generation),
            &health_claim_token,
        )
        .map(Some)
    }

    pub(crate) async fn complete_token_refresh_success(
        &self,
        lease: &RedisRefreshLease,
        storage_revision: u64,
    ) -> anyhow::Result<bool> {
        if storage_revision == 0 {
            anyhow::bail!("token refresh success requires a positive PostgreSQL storage revision");
        }
        let mut manager = self.scheduler_capacity_manager();
        let committed: i64 = redis::cmd("EVAL")
            .arg(TOKEN_REFRESH_COMPLETE_SUCCESS_SCRIPT)
            .arg(2)
            .arg(self.key(scheduler_refresh_lock_key(lease.credential_id)))
            .arg(self.key(scheduler_refresh_outcome_key(lease.credential_id)))
            .arg(&lease.owner)
            .arg(lease.generation)
            .arg(refresh_identity_hex(&lease.identity))
            .arg(storage_revision)
            .arg(TOKEN_REFRESH_SUCCESS_TTL_MS)
            .query_async(&mut manager)
            .await?;
        Ok(committed == 1)
    }

    pub(crate) async fn cancel_token_refresh(
        &self,
        lease: &RedisRefreshLease,
    ) -> anyhow::Result<bool> {
        let mut manager = self.scheduler_capacity_manager();
        let cancelled: i64 = redis::cmd("EVAL")
            .arg(TOKEN_REFRESH_CANCEL_SCRIPT)
            .arg(2)
            .arg(self.key(scheduler_refresh_lock_key(lease.credential_id)))
            .arg(self.key(scheduler_refresh_outcome_key(lease.credential_id)))
            .arg(&lease.owner)
            .arg(lease.generation)
            .arg(refresh_identity_hex(&lease.identity))
            .query_async(&mut manager)
            .await?;
        Ok(cancelled == 1)
    }

    pub(crate) async fn ack_token_refresh_health_claim(
        &self,
        credential_id: u64,
        claim: &RedisRefreshHealthClaim,
    ) -> anyhow::Result<bool> {
        let mut manager = self.scheduler_capacity_manager();
        let acknowledged: i64 = redis::cmd("EVAL")
            .arg(TOKEN_REFRESH_ACK_HEALTH_SCRIPT)
            .arg(1)
            .arg(self.key(scheduler_refresh_outcome_key(credential_id)))
            .arg(claim.generation)
            .arg(&claim.token)
            .query_async(&mut manager)
            .await?;
        Ok(acknowledged == 1)
    }

    pub(crate) async fn reserve_token_refresh_send(
        &self,
        max_rpm: u32,
        burst: u32,
        limits_fingerprint: u64,
    ) -> anyhow::Result<TokenRefreshBucketDecision> {
        if !(MIN_TOKEN_REFRESH_MAX_RPM..=MAX_TOKEN_REFRESH_MAX_RPM).contains(&max_rpm) {
            anyhow::bail!("token refresh RPM limit must be between 1 and 6000");
        }
        if !(MIN_TOKEN_REFRESH_BURST..=MAX_TOKEN_REFRESH_BURST).contains(&burst) {
            anyhow::bail!("token refresh burst limit must be between 1 and 256");
        }
        let bucket_ttl_ms = token_refresh_bucket_ttl_ms(max_rpm, burst);
        let mut manager = self.scheduler_capacity_manager();
        let values: Vec<i64> = redis::cmd("EVAL")
            .arg(TOKEN_REFRESH_BUCKET_SCRIPT)
            .arg(1)
            .arg(self.key(token_refresh_bucket_key()))
            .arg(max_rpm)
            .arg(burst)
            .arg(TOKEN_REFRESH_BUCKET_TOKEN_UNITS)
            .arg(limits_fingerprint)
            .arg(bucket_ttl_ms)
            .query_async(&mut manager)
            .await?;
        decode_token_refresh_bucket_decision(&values, burst)
    }
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn usage_realtime_bucket_key(epoch_sec: i64) -> String {
    format!("usage:summary:rt:{}", epoch_sec)
}

fn usage_top_metrics_key(dimension: &str, key: &str) -> String {
    format!(
        "usage:summary:top:{}:{}",
        dimension,
        usage_dimension_hash(key)
    )
}

fn usage_dashboard_bucket_key(dimension: &str, key: &str, hour_epoch: i64) -> String {
    format!(
        "usage:dashboard:hour:{}:{}:{}",
        dimension,
        usage_dimension_hash(key),
        hour_epoch
    )
}

fn usage_dashboard_cache_read_bucket_key(hour_epoch: i64) -> String {
    format!("usage:dashboard:hour:cache_read:{}", hour_epoch)
}

fn usage_dashboard_cache_read_bucket_suffix(suffix: &str) -> bool {
    suffix.starts_with("usage:dashboard:hour:cache_read:")
}

fn usage_dashboard_top_metrics_key(dimension: &str, key: &str) -> String {
    format!(
        "usage:dashboard:top:{}:{}",
        dimension,
        usage_dimension_hash(key)
    )
}

fn usage_record_key(member: &str) -> String {
    format!("usage:records:item:{}", member)
}

fn usage_dimension_hash(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn usage_dashboard_hour_start(created_at: DateTime<Utc>) -> DateTime<Utc> {
    created_at
        .with_minute(0)
        .and_then(|value| value.with_second(0))
        .and_then(|value| value.with_nanosecond(0))
        .unwrap_or(created_at)
}

fn usage_dashboard_hour_epochs(from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<i64> {
    if to <= from {
        return Vec::new();
    }
    let start = usage_dashboard_hour_start(from);
    let inclusive_to = to - ChronoDuration::seconds(1);
    let end = usage_dashboard_hour_start(inclusive_to.max(from));
    let mut epochs = Vec::new();
    let mut cursor = start;
    while cursor <= end {
        epochs.push(cursor.timestamp());
        cursor += ChronoDuration::hours(1);
    }
    epochs
}

fn append_usage_dashboard_rollups(
    pipe: &mut GuardedUsagePipeline,
    store: &RedisStore,
    record: &UsageRecord,
    created_at: DateTime<Utc>,
) {
    let hour_epoch = usage_dashboard_hour_start(created_at).timestamp();
    append_usage_dashboard_bucket_aggregate(
        pipe,
        &store.key(usage_dashboard_bucket_key("global", "all", hour_epoch)),
        record,
    );
    append_usage_dashboard_bucket_aggregate(
        pipe,
        &store.key(usage_dashboard_bucket_key(
            "status",
            usage_status_value(record.status),
            hour_epoch,
        )),
        record,
    );
    append_usage_dashboard_bucket_aggregate(
        pipe,
        &store.key(usage_dashboard_bucket_key(
            "usage_source",
            usage_source_value(record.usage_source),
            hour_epoch,
        )),
        record,
    );
    if let Some(external_pool_id) = record.external_pool_id {
        append_usage_dashboard_bucket_aggregate(
            pipe,
            &store.key(usage_dashboard_bucket_key(
                "external_pool",
                &external_pool_id.to_string(),
                hour_epoch,
            )),
            record,
        );
    }

    let cache_read_key = store.key(usage_dashboard_cache_read_bucket_key(hour_epoch));
    pipe.cmd("HINCRBY")
        .arg(&cache_read_key)
        .arg(record.cache_read_input_tokens.max(0).to_string())
        .arg(1i64)
        .cmd("EXPIRE")
        .arg(&cache_read_key)
        .arg(USAGE_DASHBOARD_BUCKET_TTL_SECS);
}

fn append_usage_dashboard_bucket_aggregate(
    pipe: &mut GuardedUsagePipeline,
    key: &str,
    record: &UsageRecord,
) {
    let success = record.status == UsageRecordStatus::Success;
    pipe.cmd("HINCRBY")
        .arg(key)
        .arg("total_requests")
        .arg(1i64)
        .cmd("HINCRBY")
        .arg(key)
        .arg(if success {
            "success_requests"
        } else {
            "error_requests"
        })
        .arg(1i64)
        .cmd("HINCRBY")
        .arg(key)
        .arg(if record.stream {
            "stream_requests"
        } else {
            "non_stream_requests"
        })
        .arg(1i64)
        .cmd("HINCRBY")
        .arg(key)
        .arg("total_input_tokens")
        .arg(record.total_input_tokens as i64)
        .cmd("HINCRBY")
        .arg(key)
        .arg("billable_input_tokens")
        .arg(record.billable_input_tokens as i64)
        .cmd("HINCRBY")
        .arg(key)
        .arg("total_output_tokens")
        .arg(record.output_tokens as i64)
        .cmd("HINCRBY")
        .arg(key)
        .arg("total_cache_read_input_tokens")
        .arg(record.cache_read_input_tokens as i64)
        .cmd("HINCRBY")
        .arg(key)
        .arg("total_cache_creation_input_tokens")
        .arg(record.cache_creation_input_tokens as i64)
        .cmd("HINCRBYFLOAT")
        .arg(key)
        .arg("total_estimated_cost_usd")
        .arg(record.estimated_cost_usd)
        .cmd("HINCRBYFLOAT")
        .arg(key)
        .arg("total_original_cost_usd")
        .arg(record.original_cost_usd)
        .cmd("HINCRBY")
        .arg(key)
        .arg(if record.pricing_available {
            "priced_requests"
        } else {
            "unpriced_requests"
        })
        .arg(1i64)
        .cmd("HINCRBY")
        .arg(key)
        .arg("sticky_bound_requests")
        .arg(if record.sticky_bound { 1i64 } else { 0i64 })
        .cmd("HINCRBY")
        .arg(key)
        .arg("fallback_from_sticky_requests")
        .arg(if record.fallback_from_sticky {
            1i64
        } else {
            0i64
        })
        .cmd("HINCRBY")
        .arg(key)
        .arg("simulated_requests")
        .arg(if record.simulated { 1i64 } else { 0i64 })
        .cmd("HINCRBY")
        .arg(key)
        .arg("upstream_metadata_requests")
        .arg(if record.usage_source == UsageSource::UpstreamMetadata {
            1i64
        } else {
            0i64
        })
        .cmd("HINCRBY")
        .arg(key)
        .arg("duration_ms_sum")
        .arg(record.duration_ms.min(i64::MAX as u64) as i64)
        .cmd("HINCRBY")
        .arg(key)
        .arg("duration_ms_count")
        .arg(1i64);

    append_external_pool_usage_summary(pipe, key, record);

    pipe.cmd("__USAGE_DURATION_MAX__")
        .arg(key)
        .arg(record.duration_ms.min(i64::MAX as u64) as i64)
        .arg(USAGE_DASHBOARD_BUCKET_TTL_SECS);
}

fn append_external_pool_usage_summary(
    pipe: &mut GuardedUsagePipeline,
    totals_key: &str,
    record: &UsageRecord,
) {
    if record.route_kind != Some(UsageRouteKind::ExternalPool) {
        return;
    }

    pipe.cmd("HINCRBY")
        .arg(totals_key)
        .arg("external_pool_requests")
        .arg(1i64);
    if let Some(billing) = &record.external_pool_billing {
        pipe.cmd("HINCRBY")
            .arg(totals_key)
            .arg(if billing.pricing_available {
                "external_pool_priced_requests"
            } else {
                "external_pool_unpriced_requests"
            })
            .arg(1i64)
            .cmd("HINCRBY")
            .arg(totals_key)
            .arg("external_pool_cost_floor_applied_requests")
            .arg(if billing.cost_floor_applied {
                1i64
            } else {
                0i64
            })
            .cmd("HINCRBYFLOAT")
            .arg(totals_key)
            .arg("external_pool_raw_cost_usd")
            .arg(billing.raw_cost_usd)
            .cmd("HINCRBYFLOAT")
            .arg(totals_key)
            .arg("external_pool_shaped_cost_usd")
            .arg(billing.effective_shaped_cost_usd())
            .cmd("HINCRBYFLOAT")
            .arg(totals_key)
            .arg("external_pool_uplifted_cost_usd")
            .arg(billing.effective_uplifted_cost_usd())
            .cmd("HINCRBYFLOAT")
            .arg(totals_key)
            .arg("external_pool_profit_usd")
            .arg(billing.effective_profit_usd())
            .cmd("HINCRBYFLOAT")
            .arg(totals_key)
            .arg("external_pool_reported_cost_usd")
            .arg(billing.reported_cost_usd)
            .cmd("HINCRBYFLOAT")
            .arg(totals_key)
            .arg("external_pool_billable_cost_usd")
            .arg(billing.billable_cost_usd)
            .cmd("HINCRBYFLOAT")
            .arg(totals_key)
            .arg("external_pool_cost_floor_delta_usd")
            .arg(billing.cost_floor_delta_usd);
    } else {
        pipe.cmd("HINCRBY")
            .arg(totals_key)
            .arg("external_pool_unpriced_requests")
            .arg(1i64);
    }
}

fn append_usage_dashboard_top_aggregate(
    pipe: &mut GuardedUsagePipeline,
    index_key: &str,
    _dimension: &str,
    key: Option<String>,
    label: Option<String>,
    record: &UsageRecord,
    metrics_key: impl FnOnce(&str) -> String,
) {
    let Some(key) = key
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
    else {
        return;
    };
    let metrics_key = metrics_key(&key);
    pipe.cmd("ZINCRBY")
        .arg(index_key)
        .arg(record.estimated_cost_usd)
        .arg(&key)
        .cmd("HSET")
        .arg(&metrics_key)
        .arg("key")
        .arg(&key)
        .cmd("HINCRBY")
        .arg(&metrics_key)
        .arg("requests")
        .arg(1i64)
        .cmd("HINCRBY")
        .arg(&metrics_key)
        .arg("error_requests")
        .arg(if record.status == UsageRecordStatus::Success {
            0i64
        } else {
            1i64
        })
        .cmd("HINCRBY")
        .arg(&metrics_key)
        .arg("total_input_tokens")
        .arg(record.total_input_tokens as i64)
        .cmd("HINCRBY")
        .arg(&metrics_key)
        .arg("billable_input_tokens")
        .arg(record.billable_input_tokens as i64)
        .cmd("HINCRBY")
        .arg(&metrics_key)
        .arg("total_output_tokens")
        .arg(record.output_tokens as i64)
        .cmd("HINCRBY")
        .arg(&metrics_key)
        .arg("total_cache_read_input_tokens")
        .arg(record.cache_read_input_tokens as i64)
        .cmd("HINCRBY")
        .arg(&metrics_key)
        .arg("total_cache_creation_input_tokens")
        .arg(record.cache_creation_input_tokens as i64)
        .cmd("HINCRBYFLOAT")
        .arg(&metrics_key)
        .arg("total_estimated_cost_usd")
        .arg(record.estimated_cost_usd)
        .cmd("HINCRBYFLOAT")
        .arg(&metrics_key)
        .arg("total_original_cost_usd")
        .arg(record.original_cost_usd);
    if let Some(label) = label.filter(|label| !label.trim().is_empty()) {
        pipe.cmd("HSET").arg(&metrics_key).arg("label").arg(label);
    }
}

fn append_usage_top_aggregate(
    pipe: &mut GuardedUsagePipeline,
    index_key: &str,
    key: Option<String>,
    label: Option<String>,
    record: &UsageRecord,
    metrics_key: impl FnOnce(&str) -> String,
) {
    let Some(key) = key
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
    else {
        return;
    };
    let metrics_key = metrics_key(&key);
    pipe.cmd("ZINCRBY")
        .arg(index_key)
        .arg(record.estimated_cost_usd)
        .arg(&key)
        .cmd("HSET")
        .arg(&metrics_key)
        .arg("key")
        .arg(&key)
        .cmd("HINCRBY")
        .arg(&metrics_key)
        .arg("requests")
        .arg(1i64)
        .cmd("HINCRBY")
        .arg(&metrics_key)
        .arg("cache_read_input_tokens")
        .arg(record.cache_read_input_tokens as i64)
        .cmd("HINCRBY")
        .arg(&metrics_key)
        .arg("cache_creation_input_tokens")
        .arg(record.cache_creation_input_tokens as i64)
        .cmd("HINCRBYFLOAT")
        .arg(&metrics_key)
        .arg("estimated_cost_usd")
        .arg(record.estimated_cost_usd)
        .cmd("HINCRBYFLOAT")
        .arg(&metrics_key)
        .arg("original_cost_usd")
        .arg(record.original_cost_usd);
    if let Some(label) = label.filter(|label| !label.trim().is_empty()) {
        pipe.cmd("HSET").arg(&metrics_key).arg("label").arg(label);
    }
}

#[cfg(test)]
fn dashboard_summary_from_values(
    values: &HashMap<String, String>,
    high_cache_requests: usize,
) -> UsageDashboardSummary {
    let total_requests = usage_usize(values, "total_requests");
    let error_requests = usage_usize(values, "error_requests");
    let total_input_tokens = usage_i64(values, "total_input_tokens");
    let total_cache_read_input_tokens = usage_i64(values, "total_cache_read_input_tokens");
    let duration_count = usage_i64(values, "duration_ms_count");
    let average_duration_ms = if duration_count > 0 {
        usage_i64(values, "duration_ms_sum") as f64 / duration_count as f64
    } else {
        0.0
    };

    UsageDashboardSummary {
        total_requests,
        success_requests: usage_usize(values, "success_requests"),
        error_requests,
        error_rate: usage_ratio(error_requests, total_requests),
        stream_requests: usage_usize(values, "stream_requests"),
        non_stream_requests: usage_usize(values, "non_stream_requests"),
        high_cache_requests,
        total_input_tokens,
        billable_input_tokens: usage_i64(values, "billable_input_tokens"),
        total_output_tokens: usage_i64(values, "total_output_tokens"),
        total_cache_read_input_tokens,
        total_cache_creation_input_tokens: usage_i64(values, "total_cache_creation_input_tokens"),
        cache_read_ratio: token_ratio(total_cache_read_input_tokens, total_input_tokens),
        total_estimated_cost_usd: usage_f64(values, "total_estimated_cost_usd"),
        total_original_cost_usd: usage_f64(values, "total_original_cost_usd"),
        priced_requests: usage_usize(values, "priced_requests"),
        unpriced_requests: usage_usize(values, "unpriced_requests"),
        average_duration_ms,
        p95_duration_ms: usage_i64(values, "duration_ms_max") as u64,
        sticky_bound_requests: usage_usize(values, "sticky_bound_requests"),
        fallback_from_sticky_requests: usage_usize(values, "fallback_from_sticky_requests"),
        simulated_requests: usage_usize(values, "simulated_requests"),
        upstream_metadata_requests: usage_usize(values, "upstream_metadata_requests"),
        external_pool_billing: UsageExternalPoolBillingSummary {
            requests: usage_usize(values, "external_pool_requests"),
            priced_requests: usage_usize(values, "external_pool_priced_requests"),
            unpriced_requests: usage_usize(values, "external_pool_unpriced_requests"),
            cost_floor_applied_requests: usage_usize(
                values,
                "external_pool_cost_floor_applied_requests",
            ),
            raw_cost_usd: usage_f64(values, "external_pool_raw_cost_usd"),
            shaped_cost_usd: usage_f64(values, "external_pool_shaped_cost_usd"),
            uplifted_cost_usd: usage_f64(values, "external_pool_uplifted_cost_usd"),
            profit_usd: usage_f64(values, "external_pool_profit_usd"),
            reported_cost_usd: usage_f64(values, "external_pool_reported_cost_usd"),
            billable_cost_usd: usage_f64(values, "external_pool_billable_cost_usd"),
            cost_floor_delta_usd: usage_f64(values, "external_pool_cost_floor_delta_usd"),
        },
        external_pool_billing_by_pool: Vec::new(),
        status_breakdown: Vec::new(),
        usage_source_breakdown: Vec::new(),
    }
}

fn sum_usage_hash_refs<'a>(
    buckets: impl Iterator<Item = &'a HashMap<String, String>>,
) -> HashMap<String, String> {
    let mut totals: HashMap<String, String> = HashMap::new();
    for bucket in buckets {
        for (key, value) in bucket {
            if key == "duration_ms_max" {
                let current = usage_i64(&totals, key);
                let candidate = value.parse::<i64>().unwrap_or(0).max(0);
                totals.insert(key.clone(), current.max(candidate).to_string());
            } else if usage_hash_field_is_float(key) {
                let next = usage_f64(&totals, key) + value.parse::<f64>().unwrap_or(0.0);
                totals.insert(key.clone(), next.to_string());
            } else {
                let next = usage_i64(&totals, key) + value.parse::<i64>().unwrap_or(0);
                totals.insert(key.clone(), next.max(0).to_string());
            }
        }
    }
    totals
}

#[cfg(test)]
fn collect_dashboard_window_bucket_keys(
    suffixes: &mut Vec<String>,
    seen: &mut HashSet<String>,
    spec: &UsageDashboardWindowSpec,
    external_pool_index: &[RedisExternalPoolIndexItem],
) {
    for epoch in usage_dashboard_hour_epochs(spec.from, spec.to) {
        push_dashboard_bucket_suffix(
            suffixes,
            seen,
            usage_dashboard_bucket_key("global", "all", epoch),
        );
        push_dashboard_bucket_suffix(suffixes, seen, usage_dashboard_cache_read_bucket_key(epoch));
        for status in USAGE_STATUS_VALUES {
            push_dashboard_bucket_suffix(
                suffixes,
                seen,
                usage_dashboard_bucket_key("status", status, epoch),
            );
        }
        for source in USAGE_SOURCE_VALUES {
            push_dashboard_bucket_suffix(
                suffixes,
                seen,
                usage_dashboard_bucket_key("usage_source", source, epoch),
            );
        }
        for pool in external_pool_index {
            push_dashboard_bucket_suffix(
                suffixes,
                seen,
                usage_dashboard_bucket_key("external_pool", &pool.id, epoch),
            );
        }
    }
}

fn collect_dashboard_global_bucket_keys(
    suffixes: &mut Vec<String>,
    seen: &mut HashSet<String>,
    spec: &UsageDashboardWindowSpec,
) {
    for epoch in usage_dashboard_hour_epochs(spec.from, spec.to) {
        push_dashboard_bucket_suffix(
            suffixes,
            seen,
            usage_dashboard_bucket_key("global", "all", epoch),
        );
    }
}

fn push_dashboard_bucket_suffix(
    suffixes: &mut Vec<String>,
    seen: &mut HashSet<String>,
    suffix: String,
) {
    if seen.insert(suffix.clone()) {
        suffixes.push(suffix);
    }
}

fn usage_hash_field_is_float(key: &str) -> bool {
    key.ends_with("_usd") || key == "kiro_metering_usage"
}

#[cfg(test)]
fn usage_ratio(part: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        part as f64 / total as f64
    }
}

#[cfg(test)]
fn token_ratio(part: i64, total: i64) -> f64 {
    if total <= 0 {
        0.0
    } else {
        part.max(0) as f64 / total as f64
    }
}

fn non_empty_or_unknown(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        "unknown".to_string()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
const USAGE_STATUS_VALUES: [&str; 5] = [
    "success",
    "error",
    "stream_error",
    "upstream_timeout",
    "client_dropped",
];

#[cfg(test)]
const USAGE_SOURCE_VALUES: [&str; 5] = [
    "upstream_metadata",
    "local_prompt_cache",
    "context_estimate",
    "request_estimate",
    "none",
];

fn usage_status_value(status: UsageRecordStatus) -> &'static str {
    match status {
        UsageRecordStatus::Success => "success",
        UsageRecordStatus::Error => "error",
        UsageRecordStatus::StreamError => "stream_error",
        UsageRecordStatus::UpstreamTimeout => "upstream_timeout",
        UsageRecordStatus::ClientDropped => "client_dropped",
    }
}

fn usage_source_value(source: UsageSource) -> &'static str {
    match source {
        UsageSource::UpstreamMetadata => "upstream_metadata",
        UsageSource::LocalPromptCache => "local_prompt_cache",
        UsageSource::ContextEstimate => "context_estimate",
        UsageSource::RequestEstimate => "request_estimate",
        UsageSource::None => "none",
    }
}

#[cfg(test)]
fn usage_status_label(value: &str) -> String {
    match value {
        "success" => "成功",
        "error" => "错误",
        "stream_error" => "流错误",
        "upstream_timeout" => "上游超时",
        "client_dropped" => "客户端断开",
        _ => value,
    }
    .to_string()
}

#[cfg(test)]
fn usage_source_label(value: &str) -> String {
    match value {
        "upstream_metadata" => "上游 metadata",
        "local_prompt_cache" => "本地 prompt cache",
        "context_estimate" => "上下文估算",
        "request_estimate" => "请求估算",
        "none" => "无缓存",
        _ => value,
    }
    .to_string()
}

fn usage_record_matches_query(record: &UsageRecord, query: &UsageRecordQuery) -> bool {
    if let Some(request_id) = query.request_id.as_deref() {
        if record.id != request_id {
            return false;
        }
    }
    if let Some(q) = query.q.as_deref() {
        if !usage_record_matches_search(record, q) {
            return false;
        }
    }
    if let Some(endpoint) = query.endpoint.as_deref() {
        let endpoint = endpoint.trim().to_ascii_lowercase();
        if !endpoint.is_empty() && !record.endpoint.to_ascii_lowercase().contains(&endpoint) {
            return false;
        }
    }
    if let Some(conversation_id) = query.conversation_id.as_deref() {
        if record.conversation_id.as_deref() != Some(conversation_id) {
            return false;
        }
    }
    if let Some(request_api_key_id) = query.request_api_key_id.as_deref() {
        if record.request_api_key_id.as_deref() != Some(request_api_key_id) {
            return false;
        }
    }
    if let Some(credential_id) = query.credential_id {
        if record.credential_id != Some(credential_id) {
            return false;
        }
    }
    if let Some(external_pool_id) = query.external_pool_id {
        if record.external_pool_id != Some(external_pool_id) {
            return false;
        }
    }
    if let Some(route_kind) = query.route_kind {
        if record.route_kind != Some(route_kind) {
            return false;
        }
    }
    if let Some(model) = query.model.as_deref() {
        if record.model != model
            && record.upstream_model.as_deref() != Some(model)
            && record.external_outbound_model.as_deref() != Some(model)
        {
            return false;
        }
    }
    if let Some(status) = query.status {
        if record.status != status {
            return false;
        }
    }
    if let Some(source) = query.source {
        if record.usage_source != source {
            return false;
        }
    }
    if let Some(stream) = query.stream {
        if record.stream != stream {
            return false;
        }
    }
    if let Some(min_cache_read) = query.min_cache_read {
        if record.cache_read_input_tokens < min_cache_read {
            return false;
        }
    }
    if let Some(min_first_token_latency_ms) = query.min_first_token_latency_ms {
        if record
            .first_token_latency_ms
            .map_or(true, |value| value < min_first_token_latency_ms)
        {
            return false;
        }
    }
    if let Some(since) = query.since {
        let Some(created_at) = parse_usage_record_time(&record.created_at) else {
            return false;
        };
        if created_at < since {
            return false;
        }
    }
    if let Some(until) = query.until {
        let Some(created_at) = parse_usage_record_time(&record.created_at) else {
            return false;
        };
        if created_at > until {
            return false;
        }
    }
    true
}

fn usage_record_matches_search(record: &UsageRecord, q: &str) -> bool {
    let q = q.trim().to_ascii_lowercase();
    if q.is_empty() {
        return true;
    }

    let status = usage_status_value(record.status);
    let source = usage_source_value(record.usage_source);
    let credential_id = record.credential_id.map(|id| id.to_string());
    let external_pool_id = record.external_pool_id.map(|id| id.to_string());
    let estimated_cost = record.estimated_cost_usd.to_string();
    let kiro_metering_usage = record.kiro_metering_usage.to_string();

    [
        Some(record.id.as_str()),
        Some(record.created_at.as_str()),
        Some(record.endpoint.as_str()),
        Some(record.model.as_str()),
        record.upstream_model.as_deref(),
        record.external_outbound_model.as_deref(),
        record.model_resolution_source.as_deref(),
        record.model_resolution_note.as_deref(),
        record.conversation_id.as_deref(),
        record.request_api_key_id.as_deref(),
        external_pool_id.as_deref(),
        record.external_pool_name.as_deref(),
        record.credential_label.as_deref(),
        Some(status),
        Some(source),
        record.error_type.as_deref(),
        record.error_message.as_deref(),
        record.error_detail.as_deref(),
        record.pricing_model.as_deref(),
        Some(estimated_cost.as_str()),
        Some(kiro_metering_usage.as_str()),
        credential_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|value| value.to_ascii_lowercase().contains(&q))
}

fn parse_usage_record_time(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn usage_i64(values: &HashMap<String, String>, key: &str) -> i64 {
    values
        .get(key)
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0)
        .max(0)
}

fn usage_usize(values: &HashMap<String, String>, key: &str) -> usize {
    usage_i64(values, key) as usize
}

fn usage_f64(values: &HashMap<String, String>, key: &str) -> f64 {
    values
        .get(key)
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(0.0)
}

fn session_hash(session_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(session_id.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn session_binding_key(session_id: &str) -> String {
    format!("scheduler:session:{}", session_hash(session_id))
}

fn sessions_by_credential_key(credential_id: u64) -> String {
    format!("scheduler:sessions_by_credential:{}", credential_id)
}

fn scheduler_cooldown_key(credential_id: u64) -> String {
    format!("scheduler:cooldown:{}", credential_id)
}

fn scheduler_health_key(credential_id: u64) -> String {
    format!("scheduler:health:{}", credential_id)
}

fn scheduler_model_hash(model: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(model.trim().to_ascii_lowercase().as_bytes());
    format!("{:x}", hasher.finalize())
}

fn scheduler_model_index_key(credential_id: u64) -> String {
    format!("scheduler:models:{}", credential_id)
}

fn scheduler_model_cooldown_key(credential_id: u64, model_hash: &str) -> String {
    format!("scheduler:cooldown:{}:model:{}", credential_id, model_hash)
}

fn scheduler_model_health_key(credential_id: u64, model_hash: &str) -> String {
    format!("scheduler:health:{}:model:{}", credential_id, model_hash)
}

fn scheduler_selection_window_key(credential_id: u64) -> String {
    format!("scheduler:selection:{}", credential_id)
}

fn scheduler_rate_limit_key(credential_id: u64) -> String {
    format!("scheduler:rate_limit:{}", credential_id)
}

fn scheduler_global_queue_key() -> &'static str {
    "scheduler:global:queue_leases:v1"
}

fn external_pool_global_queue_key() -> &'static str {
    "external_pool:global:queue_leases:v1"
}

fn local_pool_circuit_failures_key() -> &'static str {
    "local_pool:circuit:failures"
}

fn local_pool_circuit_sequence_key() -> &'static str {
    "local_pool:circuit:sequence"
}

fn local_pool_circuit_open_until_key() -> &'static str {
    "local_pool:circuit:open_until"
}

fn local_pool_circuit_reason_key() -> &'static str {
    "local_pool:circuit:reason"
}

fn scheduler_refresh_lock_key(credential_id: u64) -> String {
    format!("scheduler:refresh_lock:{}", credential_id)
}

fn scheduler_refresh_outcome_key(credential_id: u64) -> String {
    format!("scheduler:refresh_outcome:v1:{}", credential_id)
}

fn token_refresh_bucket_key() -> &'static str {
    "auxiliary:token_refresh:bucket:v1"
}

fn redis_value_i64(values: &[redis::Value], index: usize) -> anyhow::Result<Option<i64>> {
    values
        .get(index)
        .map(redis::from_redis_value::<i64>)
        .transpose()
        .map_err(Into::into)
}

fn redis_value_string(values: &[redis::Value], index: usize) -> anyhow::Result<Option<String>> {
    values
        .get(index)
        .map(redis::from_redis_value::<String>)
        .transpose()
        .map_err(Into::into)
}

fn decode_external_pool_coordinator_guard_state(
    values: &[redis::Value],
) -> anyhow::Result<ExternalPoolCoordinatorGuardState> {
    let status = redis_value_i64(values, 0)?.ok_or_else(|| {
        anyhow::anyhow!("Redis returned no external pool coordinator guard status")
    })?;
    let coordination_epoch = redis_value_string(values, 1)?.unwrap_or_default();
    if coordination_epoch.is_empty() && status != 2 {
        anyhow::bail!("Redis returned no external pool coordinator epoch");
    }
    match status {
        1 => Ok(ExternalPoolCoordinatorGuardState::Ready { coordination_epoch }),
        2 => Ok(ExternalPoolCoordinatorGuardState::EpochMismatch { coordination_epoch }),
        3 => Ok(ExternalPoolCoordinatorGuardState::Recovering {
            coordination_epoch,
            remaining: StdDuration::from_millis(
                redis_value_i64(values, 2)?.unwrap_or(1).max(1) as u64
            ),
        }),
        _ => anyhow::bail!("Redis returned an invalid external pool coordinator guard status"),
    }
}

struct InFlightKeys {
    last_seen: String,
    acquired: String,
    kind: String,
    weight: String,
    count: String,
    released: String,
}

fn in_flight_keys(credential_id: u64) -> InFlightKeys {
    InFlightKeys {
        last_seen: format!("scheduler:inflight:{}:last_seen", credential_id),
        acquired: format!("scheduler:inflight:{}:acquired", credential_id),
        kind: format!("scheduler:inflight:{}:kind", credential_id),
        weight: format!("scheduler:inflight:{}:weight", credential_id),
        count: format!("scheduler:inflight:{}:weighted_count", credential_id),
        released: format!("scheduler:inflight:{}:released", credential_id),
    }
}

fn global_in_flight_keys() -> InFlightKeys {
    InFlightKeys {
        last_seen: "scheduler:global:inflight:last_seen".to_string(),
        acquired: "scheduler:global:inflight:acquired".to_string(),
        kind: "scheduler:global:inflight:kind".to_string(),
        weight: "scheduler:global:inflight:weight".to_string(),
        count: "scheduler:global:inflight:weighted_count".to_string(),
        released: "scheduler:global:inflight:released".to_string(),
    }
}

fn external_pool_in_flight_keys(pool_id: u64) -> InFlightKeys {
    InFlightKeys {
        last_seen: format!("external_pool:inflight:{}:last_seen", pool_id),
        acquired: format!("external_pool:inflight:{}:acquired", pool_id),
        kind: format!("external_pool:inflight:{}:kind", pool_id),
        weight: format!("external_pool:inflight:{}:weight", pool_id),
        count: format!("external_pool:inflight:{}:weighted_count", pool_id),
        released: format!("external_pool:inflight:{}:released", pool_id),
    }
}

fn external_pool_global_in_flight_keys() -> InFlightKeys {
    InFlightKeys {
        last_seen: "external_pool:global:inflight:last_seen".to_string(),
        acquired: "external_pool:global:inflight:acquired".to_string(),
        kind: "external_pool:global:inflight:kind".to_string(),
        weight: "external_pool:global:inflight:weight".to_string(),
        count: "external_pool:global:inflight:weighted_count".to_string(),
        released: "external_pool:global:inflight:released".to_string(),
    }
}

async fn unlink_keys_with_fallback(
    manager: &mut ConnectionManager,
    keys: &[String],
) -> anyhow::Result<(usize, bool)> {
    if keys.is_empty() {
        return Ok((0, false));
    }

    let unlink_result: redis::RedisResult<i64> =
        redis::cmd("UNLINK").arg(keys).query_async(manager).await;
    match unlink_result {
        Ok(removed) => Ok((removed.max(0) as usize, false)),
        Err(error) if redis_unlink_is_unsupported(&error) => {
            let removed: i64 = redis::cmd("DEL").arg(keys).query_async(manager).await?;
            Ok((removed.max(0) as usize, true))
        }
        Err(error) => Err(error.into()),
    }
}

fn redis_unlink_is_unsupported(error: &redis::RedisError) -> bool {
    let message = error.to_string().to_ascii_lowercase();
    message.contains("unknown command") && message.contains("unlink")
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::panic::AssertUnwindSafe;

    use futures::{FutureExt, StreamExt, future::join_all};
    use redis::AsyncCommands;
    use serde::{Deserialize, Serialize};

    use super::*;
    use crate::anthropic::usage::{
        ExternalPoolBilling, ExternalPoolUsageSnapshot, UsageRouteSubtype,
    };
    use crate::model::config::Config;

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    struct CachedValue {
        value: String,
    }

    const TEST_EXTERNAL_POOL_COORDINATOR_EPOCH: &str = "test-external-pool-epoch";

    #[test]
    fn guarded_usage_script_commits_snapshot_aggregate_and_seen_in_order_for_five_rounds() {
        for round in 1..=5 {
            let script = GUARDED_IDEMPOTENT_USAGE_PIPELINE_SCRIPT;
            let cap = script
                .find("if cache_read_bucket_limit > 0")
                .expect("cache-read cardinality guard");
            let snapshot = script
                .find("if not call_succeeded('SETEX'")
                .expect("snapshot commit");
            let aggregate = script
                .find("local command_count = tonumber(ARGV[14])")
                .expect("aggregate command loop");
            let seen = script
                .find("if not call_succeeded('SET', {KEYS[3], '1', 'EX', seen_ttl})")
                .expect("seen commit marker");

            assert!(cap < snapshot, "round {round}: cap precedes all writes");
            assert!(
                snapshot < aggregate,
                "round {round}: snapshot precedes aggregate"
            );
            assert!(
                aggregate < seen,
                "round {round}: seen is the final commit marker"
            );
            assert!(
                script[..seen].contains("return invalidate_and_fail(effective_cutoff)"),
                "round {round}: pre-marker failures invalidate derived cache"
            );
            assert_eq!(
                script.matches("{KEYS[3], '1', 'EX', seen_ttl}").count(),
                1,
                "round {round}: no early or duplicate seen write"
            );
        }
    }

    #[test]
    fn token_refresh_bucket_ttl_covers_a_full_refill_window_for_five_rounds() {
        for _ in 0..5 {
            assert_eq!(token_refresh_bucket_ttl_ms(60, 8), 68_000);
            assert_eq!(token_refresh_bucket_ttl_ms(1, 256), 15_420_000);
            assert!(token_refresh_bucket_ttl_ms(1, 256) > 120_000);
        }
    }

    #[test]
    fn token_refresh_coordination_closed_decode_is_stable_for_five_rounds() {
        for round in 1..=5 {
            let identity = [round as u8; 32];
            let owner = format!("owner-{round}");
            let claim = format!("claim-{round}");
            let leader = vec![
                "leader".to_string(),
                round.to_string(),
                owner.clone(),
                "".to_string(),
                "".to_string(),
                "".to_string(),
                "".to_string(),
                "".to_string(),
                "".to_string(),
                (round - 1).to_string(),
                "".to_string(),
                "".to_string(),
                "".to_string(),
                "".to_string(),
            ];
            assert!(matches!(
                decode_refresh_begin(&leader, 42, identity, &owner, &claim).unwrap(),
                RedisRefreshBegin::Leader(RedisRefreshLease {
                    credential_id: 42,
                    generation,
                    prior_failure_streak,
                    ..
                }) if generation == round && prior_failure_streak == (round - 1) as u8
            ));

            let replay = vec![
                "replay".to_string(),
                round.to_string(),
                "".to_string(),
                "response_status".to_string(),
                "rate_limited".to_string(),
                "429".to_string(),
                "1000".to_string(),
                "1".to_string(),
                "1900000000000".to_string(),
                round.to_string(),
                claim.clone(),
                "1900000005000".to_string(),
                "1".to_string(),
                "".to_string(),
            ];
            assert!(matches!(
                decode_refresh_begin(&replay, 42, identity, &owner, &claim).unwrap(),
                RedisRefreshBegin::Replay {
                    outcome: RedisRefreshFailureOutcome {
                        failure: RedisRefreshFailure {
                            kind: RedisRefreshFailureKind::RateLimited,
                            status: Some(429),
                            health_action_required: true,
                            ..
                        },
                        ..
                    },
                    health_claim: Some(_),
                }
            ));
        }
    }

    #[test]
    fn token_refresh_coordination_rejects_malformed_wire_for_five_rounds() {
        for round in 1..=5 {
            let identity = [round as u8; 32];
            let malformed = vec![
                "replay".to_string(),
                round.to_string(),
                "".to_string(),
                "private_stage".to_string(),
                "rate_limited".to_string(),
                "429".to_string(),
                "60001".to_string(),
                "1".to_string(),
                "1900000000000".to_string(),
                "1".to_string(),
                "".to_string(),
                "0".to_string(),
                "1".to_string(),
                "".to_string(),
            ];
            let error = decode_refresh_begin(&malformed, 42, identity, "owner", "claim")
                .expect_err("unknown closed enum values must fail closed");
            let public = error.to_string();
            assert!(!public.contains("private_stage"));
            assert!(!public.contains("rate_limited"));
        }
    }

    #[test]
    fn token_refresh_failure_backoff_is_bounded_and_deterministic_for_five_rounds() {
        for round in 1..=5 {
            let lease = RedisRefreshLease {
                credential_id: 7,
                generation: round,
                owner: format!("owner-{round}"),
                identity: [round as u8; 32],
                prior_failure_streak: (round - 1) as u8,
            };
            let (streak, first) = refresh_failure_delay(&lease, None);
            let (_, repeated) = refresh_failure_delay(&lease, None);
            assert_eq!(streak, round as u8);
            assert_eq!(first, repeated);
            assert!(
                first > StdDuration::from_millis(TOKEN_REFRESH_POLL_AFTER_MS),
                "round {round}: failure replay window must outlive waiter polling"
            );
            assert!(first <= StdDuration::from_secs(30));

            let (_, retry_after) = refresh_failure_delay(&lease, Some(StdDuration::from_secs(600)));
            assert_eq!(retry_after, StdDuration::from_secs(60));
            let (_, tiny_retry_after) =
                refresh_failure_delay(&lease, Some(StdDuration::from_millis(1)));
            assert!(
                tiny_retry_after > StdDuration::from_millis(TOKEN_REFRESH_POLL_AFTER_MS),
                "round {round}: tiny Retry-After must still protect Redis waiters"
            );
        }
    }

    #[test]
    fn token_refresh_bucket_decode_is_closed_and_bounded_for_five_rounds() {
        for round in 1_u32..=5 {
            let admitted =
                decode_token_refresh_bucket_decision(&[1, 0, i64::from(round * 60)], 8).unwrap();
            assert!(admitted.admitted);
            assert_eq!(admitted.retry_after, None);
            assert_eq!(admitted.remaining_milli_tokens, u64::from(round));

            let rejected = decode_token_refresh_bucket_decision(&[0, 1_000, 0], 8).unwrap();
            assert!(!rejected.admitted);
            assert_eq!(rejected.retry_after, Some(StdDuration::from_secs(1)));
            assert!(decode_token_refresh_bucket_decision(&[2, 0, 0], 8).is_err());
            assert!(decode_token_refresh_bucket_decision(&[1, 1, 0], 8).is_err());
            assert!(decode_token_refresh_bucket_decision(&[0, 0, 0], 8).is_err());
        }
    }

    #[test]
    fn token_refresh_redis_contract_stays_secret_free_and_bounded_for_five_rounds() {
        for round in 1..=5 {
            let identity = [round as u8; 32];
            let encoded = refresh_identity_hex(&identity);
            assert_eq!(encoded.len(), 64);
            assert!(encoded.bytes().all(|byte| byte.is_ascii_hexdigit()));
            for script in [
                TOKEN_REFRESH_BEGIN_SCRIPT,
                TOKEN_REFRESH_COMPLETE_FAILURE_SCRIPT,
                TOKEN_REFRESH_COMPLETE_SUCCESS_SCRIPT,
                TOKEN_REFRESH_ACK_HEALTH_SCRIPT,
                TOKEN_REFRESH_BUCKET_SCRIPT,
            ] {
                assert!(!script.contains("refresh_token"));
                assert!(!script.contains("client_secret"));
                assert!(!script.contains("response_body"));
                assert!(!script.contains("endpoint"));
            }
        }
    }

    async fn initialize_test_external_pool_coordinator(store: &RedisStore) {
        assert!(matches!(
            store
                .install_external_pool_coordinator_guard(
                    TEST_EXTERNAL_POOL_COORDINATOR_EPOCH,
                    StdDuration::ZERO,
                )
                .await
                .unwrap(),
            ExternalPoolCoordinatorGuardState::Ready { .. }
        ));
    }

    fn test_config() -> Option<Config> {
        let url = crate::storage::integration_test_url("KIRO_RS_TEST_REDIS_URL")?;
        let mut config = Config::default();
        config.redis.url = Some(url);
        config.redis.key_prefix = format!("kiro_rs:test:{}", uuid::Uuid::new_v4());
        Some(config)
    }

    fn usage_record(
        id: &str,
        status: UsageRecordStatus,
        source: UsageSource,
        cache_read_input_tokens: i32,
        estimated_cost_usd: f64,
        duration_ms: u64,
    ) -> UsageRecord {
        UsageRecord {
            id: id.to_string(),
            created_at: Utc::now().to_rfc3339(),
            endpoint: "/v1/messages".to_string(),
            stream: status == UsageRecordStatus::Success,
            model: "claude-sonnet-4-5".to_string(),
            requested_max_tokens: None,
            downstream_stop_reason: None,
            upstream_model: None,
            external_outbound_model: None,
            model_resolution_source: None,
            model_resolution_note: None,
            conversation_id: Some("redis-dashboard-session".to_string()),
            request_api_key_id: Some("request-key-redis".to_string()),
            credential_id: Some(if status == UsageRecordStatus::Success {
                7
            } else {
                8
            }),
            credential_label: Some(if status == UsageRecordStatus::Success {
                "success@example.com".to_string()
            } else {
                "error@example.com".to_string()
            }),
            status,
            usage_source: source,
            raw_usage: None,
            total_input_tokens: 100,
            compat_input_tokens: 90,
            billable_input_tokens: 95,
            output_tokens: 20,
            cache_read_input_tokens,
            cache_creation_input_tokens: 5,
            cache_creation_5m_input_tokens: 5,
            cache_creation_1h_input_tokens: 0,
            estimated_cost_usd,
            original_cost_usd: estimated_cost_usd,
            kiro_metering_usage: 0.0,
            pricing_available: status == UsageRecordStatus::Success,
            pricing_model: Some("claude-sonnet-4-5".to_string()),
            duration_ms,
            first_token_latency_ms: Some(duration_ms / 2),
            response_latency_ms: Some(duration_ms),
            latency_trace: None,
            simulated: source.is_simulated(),
            sticky_bound: status == UsageRecordStatus::Success,
            fallback_from_sticky: status != UsageRecordStatus::Success,
            credential_attempts: Vec::new(),
            route_kind: None,
            route_subtype: None,
            fallback_reason: None,
            direct_policy_reason: None,
            local_attempted: None,
            local_preflight: None,
            external_pool_id: None,
            external_pool_name: None,
            external_attempts: Vec::new(),
            usage_projection_applied: None,
            external_pool_billing: None,
            error_type: (status != UsageRecordStatus::Success).then(|| "rate_limit".to_string()),
            error_message: (status != UsageRecordStatus::Success).then(|| "429".to_string()),
            error_detail: None,
            error_status_code: (status != UsageRecordStatus::Success).then_some(429),
            error_source: (status != UsageRecordStatus::Success)
                .then(|| "local_account".to_string()),
            error_id: None,
            error_metadata: None,
            public_error_status_code: None,
            public_error_type: None,
            public_error_message: None,
            payload_breakdown: None,
            payload_guard_report: None,
        }
    }

    fn assert_f64_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 0.000_000_001,
            "expected {expected}, got {actual}"
        );
    }

    async fn redis_matching_key_count(store: &RedisStore, pattern: &str) -> usize {
        let mut manager = store.manager.clone();
        let mut cursor = 0u64;
        let mut count = 0usize;
        loop {
            let (next_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(store.key(pattern))
                .arg("COUNT")
                .arg(64)
                .query_async(&mut manager)
                .await
                .unwrap();
            count = count.saturating_add(keys.len());
            cursor = next_cursor;
            if cursor == 0 {
                return count;
            }
        }
    }

    async fn run_isolated_redis_fixture<F, Fut>(body: F)
    where
        F: FnOnce(Arc<RedisStore>) -> Fut,
        Fut: Future<Output = ()>,
    {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = Arc::new(RedisStore::connect(&config).await.unwrap());
        let outcome = AssertUnwindSafe(body(store.clone())).catch_unwind().await;
        let cleanup = store.delete_pattern_bounded("*", None).await.unwrap();
        assert!(
            !cleanup.cancelled,
            "isolated Redis fixture cleanup was cancelled"
        );
        assert!(
            !cleanup.pass_limit_reached,
            "isolated Redis fixture cleanup did not converge"
        );
        assert_eq!(
            redis_matching_key_count(&store, "*").await,
            0,
            "isolated Redis fixture must remove its unique namespace"
        );
        if let Err(payload) = outcome {
            std::panic::resume_unwind(payload);
        }
    }

    async fn clear_usage_derived_cache_invalidation_for_test(store: &RedisStore) {
        let mut manager = store.manager.clone();
        let _: usize = manager
            .del(store.key(USAGE_DERIVED_CACHE_INVALIDATED_KEY))
            .await
            .unwrap();
    }

    fn refresh_test_failure() -> RedisRefreshFailure {
        RedisRefreshFailure {
            stage: RedisRefreshFailureStage::ResponseStatus,
            kind: RedisRefreshFailureKind::RateLimited,
            status: Some(429),
            retry_after: Some(StdDuration::from_secs(1)),
            send_committed: true,
            health_action_required: true,
        }
    }

    #[test]
    fn redis_unlink_fallback_only_accepts_explicit_unknown_command() {
        let unsupported = redis::RedisError::from((
            redis::ErrorKind::ResponseError,
            "ERR",
            "unknown command 'UNLINK'".to_string(),
        ));
        let transient = redis::RedisError::from((
            redis::ErrorKind::IoError,
            "connection reset while running UNLINK",
        ));

        assert!(redis_unlink_is_unsupported(&unsupported));
        assert!(!redis_unlink_is_unsupported(&transient));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn token_refresh_redis_concurrent_begin_elects_one_leader_for_five_rounds() {
        run_isolated_redis_fixture(|store| async move {
            const CALLERS: usize = 16;
            for round in 1_u64..=5 {
                let credential_id = 10_000 + round;
                let identity = [round as u8; 32];
                let barrier = Arc::new(tokio::sync::Barrier::new(CALLERS));
                let calls = (0..CALLERS)
                    .map(|_| {
                        let store = store.clone();
                        let barrier = barrier.clone();
                        tokio::spawn(async move {
                            barrier.wait().await;
                            store
                                .begin_token_refresh(credential_id, identity, false)
                                .await
                        })
                    })
                    .collect::<Vec<_>>();

                let mut leader = None;
                let mut waiters = 0;
                for result in join_all(calls).await {
                    match result
                        .expect("token refresh begin task must not panic")
                        .expect("token refresh begin must succeed")
                    {
                        RedisRefreshBegin::Leader(lease) => {
                            assert!(leader.replace(lease).is_none(), "round {round}");
                        }
                        RedisRefreshBegin::Wait {
                            generation,
                            poll_after,
                        } => {
                            assert_eq!(generation, Some(1), "round {round}");
                            assert!(
                                (StdDuration::from_millis(1)
                                    ..=StdDuration::from_millis(TOKEN_REFRESH_POLL_AFTER_MS))
                                    .contains(&poll_after),
                                "round {round}: {poll_after:?}"
                            );
                            waiters += 1;
                        }
                        other => panic!("round {round}: unexpected begin result {other:?}"),
                    }
                }

                let leader = leader.expect("one caller must own the refresh lease");
                assert_eq!(waiters, CALLERS - 1, "round {round}");
                assert!(store.cancel_token_refresh(&leader).await.unwrap());
            }
        })
        .await;
    }

    #[tokio::test]
    async fn token_refresh_redis_failure_replay_health_claim_and_identity_are_fenced_for_five_rounds()
     {
        run_isolated_redis_fixture(|store| async move {
            for round in 1_u64..=5 {
                let credential_id = 20_000 + round;
                let identity = [round as u8; 32];
                let leader = match store
                    .begin_token_refresh(credential_id, identity, false)
                    .await
                    .unwrap()
                {
                    RedisRefreshBegin::Leader(lease) => lease,
                    other => panic!("round {round}: expected leader, got {other:?}"),
                };
                let committed = store
                    .complete_token_refresh_failure(&leader, &refresh_test_failure(), false)
                    .await
                    .unwrap()
                    .expect("current leader must commit its failure");
                assert_eq!(committed.outcome.generation, leader.generation);
                assert!(committed.health_claim.is_none());

                let (replayed_generation, claim) = match store
                    .begin_token_refresh(credential_id, identity, true)
                    .await
                    .unwrap()
                {
                    RedisRefreshBegin::Replay {
                        outcome,
                        health_claim,
                    } => {
                        assert_eq!(outcome.failure, refresh_test_failure(), "round {round}");
                        (outcome.generation, health_claim.expect("first replayer claims health"))
                    }
                    other => panic!("round {round}: expected failure replay, got {other:?}"),
                };
                assert_eq!(replayed_generation, leader.generation);
                assert!(
                    store
                        .ack_token_refresh_health_claim(credential_id, &claim)
                        .await
                        .unwrap(),
                    "round {round}: first consume must apply"
                );
                assert!(
                    !store
                        .ack_token_refresh_health_claim(credential_id, &claim)
                        .await
                        .unwrap(),
                    "round {round}: a health claim is consumed at most once"
                );
                match store
                    .begin_token_refresh(credential_id, identity, true)
                    .await
                    .unwrap()
                {
                    RedisRefreshBegin::Replay {
                        outcome,
                        health_claim,
                    } => {
                        assert_eq!(outcome.generation, leader.generation);
                        assert!(health_claim.is_none(), "round {round}");
                    }
                    other => panic!("round {round}: expected replay after ack, got {other:?}"),
                }

                let replacement_identity = [round as u8 + 32; 32];
                let replacement = match store
                    .begin_token_refresh(credential_id, replacement_identity, true)
                    .await
                    .unwrap()
                {
                    RedisRefreshBegin::Leader(lease) => lease,
                    other => panic!(
                        "round {round}: a different identity must not replay the old failure: {other:?}"
                    ),
                };
                assert!(replacement.generation > leader.generation, "round {round}");
                assert!(store.cancel_token_refresh(&replacement).await.unwrap());
            }
        })
        .await;
    }

    #[tokio::test]
    async fn token_refresh_redis_stale_leader_cannot_overwrite_success_for_five_rounds() {
        run_isolated_redis_fixture(|store| async move {
            for round in 1_u64..=5 {
                let credential_id = 30_000 + round;
                let identity = [round as u8 + 64; 32];
                let stale = match store
                    .begin_token_refresh(credential_id, identity, false)
                    .await
                    .unwrap()
                {
                    RedisRefreshBegin::Leader(lease) => lease,
                    other => panic!("round {round}: expected first leader, got {other:?}"),
                };

                let mut manager = store.scheduler_capacity_manager();
                let removed: i64 = redis::cmd("DEL")
                    .arg(store.key(scheduler_refresh_lock_key(credential_id)))
                    .query_async(&mut manager)
                    .await
                    .unwrap();
                assert_eq!(removed, 1, "round {round}: simulate lease expiry");

                let current = match store
                    .begin_token_refresh(credential_id, identity, false)
                    .await
                    .unwrap()
                {
                    RedisRefreshBegin::Leader(lease) => lease,
                    other => panic!("round {round}: expected replacement leader, got {other:?}"),
                };
                assert!(current.generation > stale.generation, "round {round}");
                assert!(
                    !store
                        .complete_token_refresh_success(&stale, 900 + round)
                        .await
                        .unwrap(),
                    "round {round}: stale success must be rejected"
                );
                assert!(
                    store
                        .complete_token_refresh_failure(&stale, &refresh_test_failure(), true)
                        .await
                        .unwrap()
                        .is_none(),
                    "round {round}: stale failure must be rejected"
                );

                let storage_revision = 1_000 + round;
                assert!(
                    store
                        .complete_token_refresh_success(&current, storage_revision)
                        .await
                        .unwrap(),
                    "round {round}: current leader must commit success"
                );
                assert!(
                    !store
                        .complete_token_refresh_success(&stale, 2_000 + round)
                        .await
                        .unwrap(),
                    "round {round}: stale success cannot overwrite committed success"
                );
                assert!(
                    store
                        .complete_token_refresh_failure(&stale, &refresh_test_failure(), true)
                        .await
                        .unwrap()
                        .is_none(),
                    "round {round}: stale failure cannot overwrite committed success"
                );
                assert!(matches!(
                    store
                        .begin_token_refresh(credential_id, identity, true)
                        .await
                        .unwrap(),
                    RedisRefreshBegin::Succeeded {
                        generation,
                        storage_revision: revision,
                    } if generation == current.generation && revision == storage_revision
                ));
            }
        })
        .await;
    }

    #[tokio::test]
    async fn token_refresh_redis_cancel_before_send_allows_immediate_new_leader_for_five_rounds() {
        run_isolated_redis_fixture(|store| async move {
            for round in 1_u64..=5 {
                let credential_id = 40_000 + round;
                let identity = [round as u8 + 96; 32];
                let cancelled = match store
                    .begin_token_refresh(credential_id, identity, false)
                    .await
                    .unwrap()
                {
                    RedisRefreshBegin::Leader(lease) => lease,
                    other => panic!("round {round}: expected cancellable leader, got {other:?}"),
                };
                assert!(store.cancel_token_refresh(&cancelled).await.unwrap());

                let replacement = match store
                    .begin_token_refresh(credential_id, identity, false)
                    .await
                    .unwrap()
                {
                    RedisRefreshBegin::Leader(lease) => lease,
                    other => panic!(
                        "round {round}: pre-send cancellation must immediately release leadership: {other:?}"
                    ),
                };
                assert_ne!(replacement.owner, cancelled.owner, "round {round}");
                assert!(!store.cancel_token_refresh(&cancelled).await.unwrap());
                assert!(store.cancel_token_refresh(&replacement).await.unwrap());
            }
        })
        .await;
    }

    #[tokio::test]
    async fn token_refresh_redis_bucket_ttl_refill_and_version_switch_hold_for_five_rounds() {
        run_isolated_redis_fixture(|store| async move {
            for round in 1_u64..=5 {
                store.del(token_refresh_bucket_key()).await.unwrap();
                let slow_version = 50_000 + round;
                let first = store
                    .reserve_token_refresh_send(1, 2, slow_version)
                    .await
                    .unwrap();
                let second = store
                    .reserve_token_refresh_send(1, 2, slow_version)
                    .await
                    .unwrap();
                let exhausted = store
                    .reserve_token_refresh_send(1, 2, slow_version)
                    .await
                    .unwrap();
                assert!(first.admitted, "round {round}");
                assert!(second.admitted, "round {round}");
                assert!(!exhausted.admitted, "round {round}");
                assert!(
                    exhausted.retry_after.is_some_and(|delay| {
                        (StdDuration::from_secs(55)..=StdDuration::from_secs(60)).contains(&delay)
                    }),
                    "round {round}: {:?}",
                    exhausted.retry_after
                );

                let mut manager = store.scheduler_capacity_manager();
                let slow_ttl: i64 = redis::cmd("PTTL")
                    .arg(store.key(token_refresh_bucket_key()))
                    .query_async(&mut manager)
                    .await
                    .unwrap();
                let expected_slow_ttl = token_refresh_bucket_ttl_ms(1, 2) as i64;
                assert!(
                    (expected_slow_ttl - 10_000..=expected_slow_ttl).contains(&slow_ttl),
                    "round {round}: slow bucket TTL {slow_ttl} outside expected bound"
                );

                let fast_version_a = 60_000 + round;
                let switched = store
                    .reserve_token_refresh_send(6_000, 1, fast_version_a)
                    .await
                    .unwrap();
                assert!(
                    !switched.admitted,
                    "round {round}: a version switch must not retroactively refill"
                );
                tokio::time::sleep(StdDuration::from_millis(20)).await;
                assert!(
                    store
                        .reserve_token_refresh_send(6_000, 1, fast_version_a)
                        .await
                        .unwrap()
                        .admitted,
                    "round {round}: same-version elapsed time must refill"
                );

                tokio::time::sleep(StdDuration::from_millis(20)).await;
                let fast_version_b = 70_000 + round;
                assert!(
                    !store
                        .reserve_token_refresh_send(6_000, 1, fast_version_b)
                        .await
                        .unwrap()
                        .admitted,
                    "round {round}: the new version must discard elapsed-time refill"
                );
                tokio::time::sleep(StdDuration::from_millis(20)).await;
                assert!(
                    store
                        .reserve_token_refresh_send(6_000, 1, fast_version_b)
                        .await
                        .unwrap()
                        .admitted,
                    "round {round}: the new version must refill after its own timestamp"
                );

                let fast_ttl: i64 = redis::cmd("PTTL")
                    .arg(store.key(token_refresh_bucket_key()))
                    .query_async(&mut manager)
                    .await
                    .unwrap();
                let expected_fast_ttl = token_refresh_bucket_ttl_ms(6_000, 1) as i64;
                assert!(
                    (expected_fast_ttl - 5_000..=expected_fast_ttl).contains(&fast_ttl),
                    "round {round}: fast bucket TTL {fast_ttl} outside expected bound"
                );
                store.del(token_refresh_bucket_key()).await.unwrap();
            }
        })
        .await;
    }

    #[tokio::test]
    async fn redis_pattern_delete_is_bounded_and_cancellable() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let mut manager = store.manager.clone();
        for round in 0..3usize {
            let key_count = 321 + round * 17;
            for index in 0..key_count {
                let _: () = manager
                    .set(
                        store.key(format!("bounded-delete:{round}:{index}")),
                        "value",
                    )
                    .await
                    .unwrap();
            }

            let stats = store
                .delete_pattern_bounded("bounded-delete:*", None)
                .await
                .unwrap();
            assert_eq!(stats.deleted_keys, key_count, "round {round}");
            assert!(stats.scan_calls >= 1);
            assert!(stats.scan_passes >= 2);
            assert!(stats.delete_commands >= 6);
            assert!(stats.max_command_keys <= REDIS_PATTERN_DELETE_COMMAND_KEY_LIMIT);
            assert!(!stats.cancelled);
            assert!(!stats.pass_limit_reached);
            assert_eq!(
                redis_matching_key_count(&store, "bounded-delete:*").await,
                0,
                "round {round} must leave no matching keys"
            );
        }

        let cancelled = AtomicBool::new(true);
        let _: () = manager
            .set(store.key(USAGE_RECORDS_INDEX_KEY), "index")
            .await
            .unwrap();
        let _: () = manager
            .set(store.key("usage:records:item:cancelled"), "value")
            .await
            .unwrap();
        let cancelled_stats = store
            .clear_usage_record_snapshots_bounded(Some(&cancelled))
            .await
            .unwrap();
        assert!(cancelled_stats.cancelled);
        assert_eq!(cancelled_stats.scan_calls, 0);
        let index_exists: bool = manager
            .exists(store.key(USAGE_RECORDS_INDEX_KEY))
            .await
            .unwrap();
        let item_exists: bool = manager
            .exists(store.key("usage:records:item:cancelled"))
            .await
            .unwrap();
        assert!(!index_exists);
        assert!(item_exists);
        store
            .delete_pattern_bounded("usage:records:item:cancelled", None)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn redis_json_round_trip_and_delete() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let value = CachedValue {
            value: "ok".to_string(),
        };

        store.set_json("sample", &value, 60).await.unwrap();
        assert_eq!(
            store.get_json::<CachedValue>("sample").await.unwrap(),
            Some(value)
        );

        store.del("sample").await.unwrap();
        assert_eq!(store.get_json::<CachedValue>("sample").await.unwrap(), None);
    }

    #[tokio::test]
    async fn redis_usage_summary_and_dashboard_are_materialized() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        store.clear_usage_summary().await.unwrap();

        let success = usage_record(
            "redis-usage-success",
            UsageRecordStatus::Success,
            UsageSource::UpstreamMetadata,
            1_000,
            0.10,
            20,
        );
        let mut external = usage_record(
            "redis-usage-external",
            UsageRecordStatus::Success,
            UsageSource::UpstreamMetadata,
            0,
            0.42,
            30,
        );
        external.route_kind = Some(UsageRouteKind::ExternalPool);
        external.route_subtype = Some(UsageRouteSubtype::ExternalFallbackAfterLocalAttempts);
        external.credential_id = None;
        external.credential_label = None;
        external.external_pool_id = Some(42);
        external.external_pool_name = Some("backup-a".to_string());
        external.external_pool_billing = Some(ExternalPoolBilling {
            request_input_tokens: None,
            raw_usage: ExternalPoolUsageSnapshot::default(),
            shaped_usage: ExternalPoolUsageSnapshot::default(),
            reported_usage: ExternalPoolUsageSnapshot::default(),
            usage_projection_applied: true,
            raw_cost_usd: 0.10,
            shaped_cost_usd: 0.20,
            uplifted_cost_usd: 0.42,
            profit_usd: 0.32,
            reported_cost_usd: 0.42,
            billable_cost_usd: 0.42,
            cost_floor_delta_usd: 0.0,
            cost_floor_applied: false,
            pricing_available: true,
            pricing_model: Some("claude-sonnet-4-5".to_string()),
            usage_projection_mode: "current_path_policy".to_string(),
            stream_response_mode: None,
            usage_estimated: false,
            usage_estimate_reason: None,
            usage_candidate_path: None,
            body_usage_projection_applied: true,
        });
        let mut error = usage_record(
            "redis-usage-error",
            UsageRecordStatus::Error,
            UsageSource::LocalPromptCache,
            100,
            0.20,
            50,
        );
        error.request_api_key_id = Some("request-key-error".to_string());

        store.record_usage_summary(&success).await.unwrap();
        store.record_usage_summary(&external).await.unwrap();
        store.record_usage_summary(&error).await.unwrap();
        store.record_usage_summary(&error).await.unwrap();

        let summary = store.usage_summary(500).await.unwrap().unwrap();
        assert_eq!(summary.total_requests, 3);
        assert_eq!(summary.success_requests, 2);
        assert_eq!(summary.error_requests, 1);
        assert_eq!(summary.high_cache_requests, 1);
        assert_eq!(summary.total_input_tokens, 300);
        assert_eq!(summary.top_credentials.len(), 2);

        let dashboard = store
            .usage_dashboard(Some("UTC"), 500)
            .await
            .unwrap()
            .unwrap();
        let last24h = dashboard
            .windows
            .iter()
            .find(|window| window.key == "last24h")
            .unwrap();
        assert_eq!(last24h.summary.total_requests, 3);
        assert_eq!(last24h.summary.success_requests, 2);
        assert_eq!(last24h.summary.error_requests, 1);
        assert_eq!(last24h.summary.high_cache_requests, 1);
        assert_eq!(last24h.summary.p95_duration_ms, 50);
        assert_eq!(last24h.summary.external_pool_billing.requests, 1);
        assert_f64_close(last24h.summary.external_pool_billing.raw_cost_usd, 0.10);
        assert_f64_close(
            last24h.summary.external_pool_billing.uplifted_cost_usd,
            0.42,
        );
        assert_f64_close(last24h.summary.external_pool_billing.profit_usd, 0.32);
        assert_eq!(last24h.summary.external_pool_billing_by_pool.len(), 1);
        let pool_billing = &last24h.summary.external_pool_billing_by_pool[0];
        assert_eq!(pool_billing.pool_id, 42);
        assert_eq!(pool_billing.pool_name, "backup-a");
        assert_eq!(pool_billing.requests, 1);
        assert_f64_close(pool_billing.raw_cost_usd, 0.10);
        assert_f64_close(pool_billing.uplifted_cost_usd, 0.42);
        assert_f64_close(pool_billing.profit_usd, 0.32);
        assert_eq!(last24h.summary.status_breakdown.len(), 2);
        assert!(
            last24h
                .summary
                .usage_source_breakdown
                .iter()
                .any(|item| item.key == "local_prompt_cache")
        );
        assert_eq!(dashboard.top.models[0].requests, 3);
        assert_eq!(dashboard.top.errors[0].key, "rate_limit");
        assert!(
            dashboard
                .series
                .hourly_24h
                .iter()
                .any(|point| point.requests == 3)
        );

        let records = store
            .usage_records_page(UsageRecordQuery::default(), 1, 10)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(records.records.len(), 3);
        assert!(!records.has_next);

        let filtered = store
            .usage_records_page(
                UsageRecordQuery {
                    status: Some(UsageRecordStatus::Error),
                    ..Default::default()
                },
                1,
                10,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(filtered.records.len(), 1);
        assert_eq!(filtered.records[0].id, "redis-usage-error");

        let request_key_filtered = store
            .usage_records_page(
                UsageRecordQuery {
                    request_api_key_id: Some("request-key-error".to_string()),
                    ..Default::default()
                },
                1,
                10,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(request_key_filtered.records.len(), 1);
        assert_eq!(request_key_filtered.records[0].id, "redis-usage-error");

        store.clear_usage_summary().await.unwrap();
        assert!(store.usage_summary(500).await.unwrap().is_none());
        assert!(
            store
                .usage_dashboard(Some("UTC"), 500)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .usage_records_page(UsageRecordQuery::default(), 1, 10)
                .await
                .unwrap()
                .is_none()
        );
    }

    async fn assert_redis_usage_cleanup_authorities_empty(store: &RedisStore, context: &str) {
        assert!(
            store.usage_summary(500).await.unwrap().is_none(),
            "{context}: summary"
        );
        assert!(
            store
                .usage_dashboard(Some("UTC"), 500)
                .await
                .unwrap()
                .is_none(),
            "{context}: dashboard"
        );
        assert!(
            store
                .usage_records_page(UsageRecordQuery::default(), 1, 10)
                .await
                .unwrap()
                .is_none(),
            "{context}: detail snapshots"
        );
    }

    #[tokio::test]
    async fn redis_cleanup_watermark_rejects_old_and_accepts_new_for_three_rounds() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };
        let store = RedisStore::connect(&config).await.unwrap();
        store.clear_usage_summary().await.unwrap();

        for round in 0..3 {
            let mut old = usage_record(
                &format!("redis-cleanup-old-round-{round}"),
                UsageRecordStatus::Success,
                UsageSource::UpstreamMetadata,
                600,
                0.1,
                20,
            );
            old.created_at = Utc::now().to_rfc3339();
            store.reset_usage_summary_write_round_trips();
            assert!(store.record_usage_summary(&old).await.unwrap());
            assert_eq!(
                store.usage_summary_write_round_trips(),
                1,
                "round {round}: snapshot, aggregate and seen marker share one Lua RTT"
            );

            tokio::time::sleep(StdDuration::from_millis(2)).await;
            let cutoff = Utc::now();
            store.advance_usage_cleanup_watermark(cutoff).await.unwrap();
            store.clear_usage_summary().await.unwrap();

            store.reset_usage_summary_write_round_trips();
            assert!(!store.record_usage_summary(&old).await.unwrap());
            assert_eq!(
                store.usage_summary_write_round_trips(),
                1,
                "round {round}: an old replay is rejected by the combined Lua RTT"
            );
            assert_redis_usage_cleanup_authorities_empty(
                &store,
                &format!("old replay round {round}"),
            )
            .await;

            let mut new = usage_record(
                &format!("redis-cleanup-new-round-{round}"),
                UsageRecordStatus::Success,
                UsageSource::UpstreamMetadata,
                600,
                0.1,
                20,
            );
            new.created_at = Utc::now().to_rfc3339();
            store.reset_usage_summary_write_round_trips();
            assert!(store.record_usage_summary(&new).await.unwrap());
            assert_eq!(store.usage_summary_write_round_trips(), 1, "round {round}");
            assert_eq!(
                store
                    .usage_summary(500)
                    .await
                    .unwrap()
                    .unwrap()
                    .total_requests,
                1,
                "round {round}: newer usage remains visible"
            );
            store.clear_usage_summary().await.unwrap();
        }
    }

    #[tokio::test]
    async fn redis_cleanup_watermark_is_shared_across_two_instances_for_three_rounds() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };
        let store_a = RedisStore::connect(&config).await.unwrap();
        let store_b = RedisStore::connect(&config).await.unwrap();
        store_a.clear_usage_summary().await.unwrap();

        for round in 0..3 {
            let mut old = usage_record(
                &format!("redis-two-instance-old-round-{round}"),
                UsageRecordStatus::Success,
                UsageSource::UpstreamMetadata,
                100,
                0.1,
                20,
            );
            old.created_at = Utc::now().to_rfc3339();
            tokio::time::sleep(StdDuration::from_millis(2)).await;
            let cutoff = Utc::now();
            store_a
                .advance_usage_cleanup_watermark(cutoff)
                .await
                .unwrap();
            store_a.clear_usage_summary().await.unwrap();

            store_b.reset_usage_summary_write_round_trips();
            assert!(
                !store_b.record_usage_summary(&old).await.unwrap(),
                "round {round}: instance B must honor instance A's watermark"
            );
            assert_eq!(
                store_b.usage_summary_write_round_trips(),
                1,
                "round {round}"
            );
            assert_redis_usage_cleanup_authorities_empty(
                &store_b,
                &format!("two instance round {round}"),
            )
            .await;
        }
    }

    #[tokio::test]
    async fn redis_guarded_summary_commit_closes_midflight_cleanup_race_for_three_rounds() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };
        let store = RedisStore::connect(&config).await.unwrap();
        store.clear_usage_summary().await.unwrap();

        for round in 0..3 {
            let mut old = usage_record(
                &format!("redis-midflight-old-round-{round}"),
                UsageRecordStatus::Success,
                UsageSource::UpstreamMetadata,
                100,
                0.1,
                20,
            );
            old.created_at = Utc::now().to_rfc3339();
            let created_at = DateTime::parse_from_rfc3339(&old.created_at)
                .unwrap()
                .with_timezone(&Utc);
            assert!(
                store
                    .record_usage_record_snapshot(&old, created_at)
                    .await
                    .unwrap(),
                "round {round}: snapshot starts before cleanup"
            );

            tokio::time::sleep(StdDuration::from_millis(2)).await;
            let cutoff = Utc::now();
            store.advance_usage_cleanup_watermark(cutoff).await.unwrap();
            store.clear_usage_summary().await.unwrap();

            let totals_key = store.key(USAGE_SUMMARY_TOTALS_KEY);
            let mut guarded = GuardedUsagePipeline::default();
            guarded
                .cmd("HINCRBY")
                .arg(&totals_key)
                .arg("total_requests")
                .arg(1i64);
            let mut manager = store.manager.clone();
            let (accepted, _) = guarded
                .query_guarded(
                    &mut manager,
                    &store.key(USAGE_CLEANUP_WATERMARK_KEY),
                    &store.key(USAGE_DERIVED_CACHE_INVALIDATED_KEY),
                    created_at.timestamp_micros(),
                    0,
                )
                .await
                .unwrap();
            assert!(
                !accepted,
                "round {round}: a pre-cleanup snapshot cannot commit summary after cleanup"
            );
            assert_redis_usage_cleanup_authorities_empty(
                &store,
                &format!("midflight round {round}"),
            )
            .await;
        }
    }

    #[tokio::test]
    async fn redis_usage_summary_cache_read_cardinality_is_hard_capped_for_five_rounds() {
        run_isolated_redis_fixture(|store| async move {
            const TEST_BUCKET_LIMIT: usize = 8;

            for round in 1..=5 {
                store.clear_usage_summary().await.unwrap();
                clear_usage_derived_cache_invalidation_for_test(&store).await;
                for bucket in 0..TEST_BUCKET_LIMIT {
                let record = usage_record(
                    &format!("redis-cache-cardinality-{round}-{bucket}"),
                    UsageRecordStatus::Success,
                    UsageSource::UpstreamMetadata,
                    bucket as i32,
                    0.01,
                    20,
                );
                assert!(
                    store
                        .record_usage_summary_with_cache_read_limit(&record, TEST_BUCKET_LIMIT)
                        .await
                        .unwrap(),
                    "round {round}, bucket {bucket}"
                );
                }

            let overflow = usage_record(
                &format!("redis-cache-cardinality-{round}-overflow"),
                UsageRecordStatus::Success,
                UsageSource::UpstreamMetadata,
                TEST_BUCKET_LIMIT as i32,
                0.01,
                20,
            );
            assert!(
                !store
                    .record_usage_summary_with_cache_read_limit(&overflow, TEST_BUCKET_LIMIT)
                    .await
                    .unwrap(),
                "round {round}: a new exact-token bucket beyond the cap must invalidate the cache"
            );

            let created_at = DateTime::parse_from_rfc3339(&overflow.created_at)
                .unwrap()
                .with_timezone(&Utc);
            let dashboard_key = store.key(usage_dashboard_cache_read_bucket_key(
                usage_dashboard_hour_start(created_at).timestamp(),
            ));
            let mut manager = store.manager.clone();
            let global_fields: usize = manager
                .hlen(store.key(USAGE_SUMMARY_CACHE_READ_KEY))
                .await
                .unwrap();
            let dashboard_fields: usize = manager.hlen(dashboard_key).await.unwrap();
            assert_eq!(global_fields, TEST_BUCKET_LIMIT, "round {round}");
            assert_eq!(dashboard_fields, TEST_BUCKET_LIMIT, "round {round}");
            assert!(
                manager
                    .exists::<_, bool>(store.key(USAGE_DERIVED_CACHE_INVALIDATED_KEY))
                    .await
                    .unwrap(),
                "round {round}"
            );
            assert!(
                store.usage_summary(500).await.unwrap().is_none(),
                "round {round}"
            );
            }
        })
        .await;
    }

    #[tokio::test]
    async fn redis_usage_summary_partial_command_error_never_sets_seen_for_five_rounds() {
        run_isolated_redis_fixture(|store| async move {
            for round in 1..=5 {
                store.clear_usage_summary().await.unwrap();
                clear_usage_derived_cache_invalidation_for_test(&store).await;
                let record = usage_record(
                    &format!("redis-partial-summary-{round}"),
                    UsageRecordStatus::Success,
                    UsageSource::UpstreamMetadata,
                    600,
                    0.01,
                    20,
                );
                let created_at = DateTime::parse_from_rfc3339(&record.created_at)
                    .unwrap()
                    .with_timezone(&Utc);
                let realtime_key = store.key(usage_realtime_bucket_key(created_at.timestamp()));
                let seen_key = store.key(format!(
                    "usage:summary:seen:{}",
                    usage_dimension_hash(&record.id)
                ));
                let totals_key = store.key(USAGE_SUMMARY_TOTALS_KEY);
                let mut manager = store.manager.clone();
                let _: () = redis::cmd("SET")
                    .arg(&realtime_key)
                    .arg("wrong-type")
                    .query_async(&mut manager)
                    .await
                    .unwrap();

                assert!(
                    store.record_usage_summary(&record).await.is_err(),
                    "round {round}: a late WRONGTYPE must fail the derived summary commit"
                );
                assert!(
                    manager
                        .exists::<_, bool>(store.key(USAGE_DERIVED_CACHE_INVALIDATED_KEY))
                        .await
                        .unwrap(),
                    "round {round}"
                );
                assert!(
                    !manager.exists::<_, bool>(&seen_key).await.unwrap(),
                    "round {round}: a partial pipeline must remain retryable"
                );
                assert_eq!(
                    manager
                        .hget::<_, _, Option<i64>>(&totals_key, "total_requests")
                        .await
                        .unwrap(),
                    Some(1),
                    "round {round}: the fixture must fail after at least one derived write"
                );
                assert!(
                    store.usage_summary(500).await.unwrap().is_none(),
                    "round {round}"
                );

                store.clear_usage_summary().await.unwrap();
                clear_usage_derived_cache_invalidation_for_test(&store).await;
                assert!(
                    store.record_usage_summary(&record).await.unwrap(),
                    "round {round}"
                );
                assert_eq!(
                    store
                        .usage_summary(500)
                        .await
                        .unwrap()
                        .unwrap()
                        .total_requests,
                    1,
                    "round {round}: retry after invalidated-cache cleanup counts exactly once"
                );
            }
        })
        .await;
    }

    fn latency_percentile_micros(values: &mut [u64], percentile: f64) -> u64 {
        values.sort_unstable();
        let index = ((values.len().saturating_sub(1)) as f64 * percentile).ceil() as usize;
        values[index.min(values.len().saturating_sub(1))]
    }

    fn process_rss_kib() -> Option<u64> {
        let output = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            .ok()?;
        String::from_utf8(output.stdout).ok()?.trim().parse().ok()
    }

    fn open_fd_count() -> Option<usize> {
        std::fs::read_dir("/dev/fd")
            .ok()
            .map(|entries| entries.count())
    }

    async fn dispatch_queue_latency_sample(
        store: &RedisStore,
        round: usize,
        sample_count: usize,
    ) -> Vec<u64> {
        let mut latencies = Vec::with_capacity(sample_count);
        for index in 0..sample_count {
            let lease_id = format!("usage-redis-latency-round-{round}-lease-{index}");
            let started = std::time::Instant::now();
            assert!(
                store
                    .try_enter_dispatch_queue(&lease_id, 10_000, 60)
                    .await
                    .unwrap()
            );
            assert!(store.leave_dispatch_queue(&lease_id).await.unwrap());
            latencies.push(started.elapsed().as_micros() as u64);
        }
        latencies
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn redis_usage_writer_burst_keeps_scheduler_latency_bounded_for_three_rounds() {
        run_isolated_redis_fixture(|store| async move {
            store.clear_usage_summary().await.unwrap();
            let usage_summary_write_permits = store.usage_summary_write_gate.available_permits();
            let rss_start = process_rss_kib();
            let fd_start = open_fd_count();
            let mut rss_peak = rss_start;
            let mut fd_peak = fd_start;

            for round in 0..3 {
            let mut baseline = dispatch_queue_latency_sample(&store, round, 100).await;
            let baseline_p50 = latency_percentile_micros(&mut baseline, 0.50);
            let baseline_p95 = latency_percentile_micros(&mut baseline, 0.95);
            let baseline_p99 = latency_percentile_micros(&mut baseline, 0.99);

            let start_barrier = Arc::new(tokio::sync::Barrier::new(5));
            let usage_store = store.clone();
            let usage_start_barrier = start_barrier.clone();
            let usage_burst = tokio::spawn(async move {
                let mut workers = Vec::new();
                for worker in 0..4 {
                    let worker_store = usage_store.clone();
                    let worker_start_barrier = usage_start_barrier.clone();
                    workers.push(tokio::spawn(async move {
                        worker_start_barrier.wait().await;
                        let worker_started = std::time::Instant::now();
                        let mut latencies = Vec::with_capacity(25);
                        for index in 0..25 {
                            let record = usage_record(
                                &format!(
                                    "usage-scheduler-latency-round-{round}-worker-{worker}-{index}"
                                ),
                                UsageRecordStatus::Success,
                                UsageSource::UpstreamMetadata,
                                600 + index,
                                0.01,
                                20,
                            );
                            let started = std::time::Instant::now();
                            assert!(worker_store.record_usage_summary(&record).await.unwrap());
                            latencies.push(started.elapsed().as_micros() as u64);
                        }
                        (latencies, worker_started, std::time::Instant::now())
                    }));
                }
                let mut writer_latencies = Vec::with_capacity(100);
                let mut writer_started = None;
                let mut writer_finished = None;
                for worker in workers {
                    let (mut latencies, started, finished) = worker.await.unwrap();
                    writer_latencies.append(&mut latencies);
                    writer_started = Some(
                        writer_started
                            .map(|current: std::time::Instant| current.min(started))
                            .unwrap_or(started),
                    );
                    writer_finished = Some(
                        writer_finished
                            .map(|current: std::time::Instant| current.max(finished))
                            .unwrap_or(finished),
                    );
                }
                let writer_elapsed = writer_finished.unwrap() - writer_started.unwrap();
                (writer_latencies, writer_elapsed)
            });
            let scheduler_store = store.clone();
            let scheduler_start_barrier = start_barrier.clone();
            let scheduler = tokio::spawn(async move {
                scheduler_start_barrier.wait().await;
                dispatch_queue_latency_sample(&scheduler_store, round + 100, 100).await
            });
            let (mut writer_latencies, writer_elapsed) = usage_burst.await.unwrap();
            let mut loaded = scheduler.await.unwrap();
            let writer_p50 = latency_percentile_micros(&mut writer_latencies, 0.50);
            let writer_p95 = latency_percentile_micros(&mut writer_latencies, 0.95);
            let writer_p99 = latency_percentile_micros(&mut writer_latencies, 0.99);
            let writer_throughput_per_sec =
                writer_latencies.len() as f64 / writer_elapsed.as_secs_f64().max(f64::EPSILON);
            let loaded_p50 = latency_percentile_micros(&mut loaded, 0.50);
            let loaded_p95 = latency_percentile_micros(&mut loaded, 0.95);
            let loaded_p99 = latency_percentile_micros(&mut loaded, 0.99);
            let mut recovery = dispatch_queue_latency_sample(&store, round + 200, 100).await;
            let recovery_p50 = latency_percentile_micros(&mut recovery, 0.50);
            let recovery_p95 = latency_percentile_micros(&mut recovery, 0.95);
            let recovery_p99 = latency_percentile_micros(&mut recovery, 0.99);
            let rss_now = process_rss_kib();
            let fd_now = open_fd_count();
            rss_peak = match (rss_peak, rss_now) {
                (Some(previous), Some(current)) => Some(previous.max(current)),
                (previous, None) => previous,
                (None, current) => current,
            };
            fd_peak = match (fd_peak, fd_now) {
                (Some(previous), Some(current)) => Some(previous.max(current)),
                (previous, None) => previous,
                (None, current) => current,
            };
            eprintln!(
                "Redis usage/scheduler round {round}: permits={usage_summary_write_permits} writer_count={} writer_wall_us={} writer_throughput_per_sec={writer_throughput_per_sec:.2} writer_p50_us={writer_p50} writer_p95_us={writer_p95} writer_p99_us={writer_p99} baseline_p50_us={baseline_p50} baseline_p95_us={baseline_p95} baseline_p99_us={baseline_p99} loaded_p50_us={loaded_p50} loaded_p95_us={loaded_p95} loaded_p99_us={loaded_p99} recovery_p50_us={recovery_p50} recovery_p95_us={recovery_p95} recovery_p99_us={recovery_p99} rss_kib={rss_now:?} fd={fd_now:?}",
                writer_latencies.len(),
                writer_elapsed.as_micros(),
            );
            assert!(
                loaded_p99 < 250_000,
                "round {round}: scheduler p99 exceeded the 250ms capacity hot-path budget under usage burst: {loaded_p99}us"
            );
            assert!(
                recovery_p99 < 250_000,
                "round {round}: scheduler p99 did not recover below the 250ms capacity hot-path budget: {recovery_p99}us"
            );
                store.clear_usage_summary().await.unwrap();
            }

            let rss_end = process_rss_kib();
            let fd_end = open_fd_count();
            if let (Some(start), Some(end)) = (rss_start, rss_end) {
                assert!(
                    end <= start.saturating_add(32 * 1024),
                    "usage/scheduler burst RSS did not recover within 32 MiB: start={start}, peak={rss_peak:?}, end={end}"
                );
            }
            if let (Some(start), Some(end)) = (fd_start, fd_end) {
                assert!(
                    end <= start.saturating_add(8),
                    "usage/scheduler burst leaked file descriptors: start={start}, peak={fd_peak:?}, end={end}"
                );
            }
            eprintln!(
                "Redis usage/scheduler resources: permits={usage_summary_write_permits} rss_start_kib={rss_start:?} rss_peak_kib={rss_peak:?} rss_end_kib={rss_end:?} fd_start={fd_start:?} fd_peak={fd_peak:?} fd_end={fd_end:?}"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn redis_usage_record_snapshot_trims_orphan_items_with_index() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        store.clear_usage_summary().await.unwrap();
        let base = Utc::now();

        for index in 0..5 {
            let record = usage_record(
                &format!("redis-usage-trim-{index}"),
                UsageRecordStatus::Success,
                UsageSource::UpstreamMetadata,
                index * 10,
                0.01,
                10,
            );
            store
                .record_usage_record_snapshot_with_limits(
                    &record,
                    base + ChronoDuration::milliseconds(index as i64),
                    3,
                    8,
                )
                .await
                .unwrap();
        }

        let index_key = store.key(USAGE_RECORDS_INDEX_KEY);
        let mut manager = store.manager.clone();
        let indexed: usize = manager.zcard(&index_key).await.unwrap();
        assert_eq!(indexed, 3);

        for index in 0..2 {
            let member = usage_dimension_hash(&format!("redis-usage-trim-{index}"));
            let exists: bool = manager
                .exists(store.key(usage_record_key(&member)))
                .await
                .unwrap();
            assert!(!exists, "trimmed record item key should be deleted");
        }
        for index in 2..5 {
            let member = usage_dimension_hash(&format!("redis-usage-trim-{index}"));
            let exists: bool = manager
                .exists(store.key(usage_record_key(&member)))
                .await
                .unwrap();
            assert!(exists, "cached record item key should remain");
        }

        let page = store
            .usage_records_page(UsageRecordQuery::default(), 1, 10)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            page.records
                .iter()
                .map(|record| record.id.as_str())
                .collect::<Vec<_>>(),
            vec![
                "redis-usage-trim-4",
                "redis-usage-trim-3",
                "redis-usage-trim-2"
            ]
        );

        store.clear_usage_summary().await.unwrap();
    }

    #[tokio::test]
    async fn redis_usage_summary_reads_existing_hash_data_without_index_rebuild() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        store.clear_usage_summary().await.unwrap();

        let totals_key = store.key(USAGE_SUMMARY_TOTALS_KEY);
        let cache_read_key = store.key(USAGE_SUMMARY_CACHE_READ_KEY);
        let cache_read_index_key = store.key(USAGE_SUMMARY_CACHE_READ_INDEX_KEY);
        let mut manager = store.manager.clone();
        let _: () = redis::pipe()
            .atomic()
            .cmd("HSET")
            .arg(&totals_key)
            .arg("total_requests")
            .arg(6i64)
            .cmd("HSET")
            .arg(&cache_read_key)
            .arg("0")
            .arg(1i64)
            .arg("500")
            .arg(2i64)
            .arg("1000")
            .arg(3i64)
            .cmd("DEL")
            .arg(&cache_read_index_key)
            .query_async(&mut manager)
            .await
            .unwrap();

        let summary = store.usage_summary(500).await.unwrap().unwrap();
        assert_eq!(summary.total_requests, 6);
        assert_eq!(summary.high_cache_requests, 5);

        let mut manager = store.manager.clone();
        let indexed: usize = manager.zcard(&cache_read_index_key).await.unwrap();
        assert_eq!(indexed, 0);
        let summary = store.usage_summary(1000).await.unwrap().unwrap();
        assert_eq!(summary.high_cache_requests, 3);

        store.clear_usage_summary().await.unwrap();
    }

    #[tokio::test]
    async fn redis_usage_summary_and_dashboard_skip_high_cardinality_cache_read_buckets() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        store.clear_usage_summary().await.unwrap();

        let totals_key = store.key(USAGE_SUMMARY_TOTALS_KEY);
        let cache_read_key = store.key(USAGE_SUMMARY_CACHE_READ_KEY);
        let hour = usage_dashboard_hour_start(Utc::now()).timestamp();
        let dashboard_cache_read_key = store.key(usage_dashboard_cache_read_bucket_key(hour));
        let mut pipe = redis::pipe();
        pipe.atomic()
            .cmd("HSET")
            .arg(&totals_key)
            .arg("total_requests")
            .arg((USAGE_CACHE_READ_INLINE_BUCKET_LIMIT + 1) as i64);
        for index in 0..=USAGE_CACHE_READ_INLINE_BUCKET_LIMIT {
            pipe.cmd("HSET")
                .arg(&cache_read_key)
                .arg(index.to_string())
                .arg(1i64)
                .cmd("HSET")
                .arg(&dashboard_cache_read_key)
                .arg(index.to_string())
                .arg(1i64);
        }
        let mut manager = store.manager.clone();
        let _: () = pipe.query_async(&mut manager).await.unwrap();

        assert!(store.usage_summary(500).await.unwrap().is_none());
        assert!(store.usage_dashboard(Some("UTC"), 500).await.is_err());

        store.clear_usage_summary().await.unwrap();
    }

    #[tokio::test]
    async fn redis_usage_dashboard_stale_summary_without_window_buckets_returns_none() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        store.clear_usage_summary().await.unwrap();

        let mut manager = store.manager.clone();
        let inserted: i64 = redis::cmd("HSET")
            .arg(store.key(USAGE_SUMMARY_TOTALS_KEY))
            .arg("total_requests")
            .arg(12i64)
            .query_async(&mut manager)
            .await
            .unwrap();
        assert_eq!(inserted, 1);

        assert!(
            store
                .usage_dashboard(Some("UTC"), 500)
                .await
                .unwrap()
                .is_none()
        );
        store.clear_usage_summary().await.unwrap();
    }

    #[tokio::test]
    async fn redis_scheduler_session_binding_round_trip_and_soft_failure() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let binding = SchedulerSessionBinding {
            credential_id: 7,
            last_used_at: Utc::now(),
            soft_failure_count: 0,
        };
        store
            .set_session_binding("session-a", &binding, 60)
            .await
            .unwrap();
        let loaded = store
            .get_session_binding("session-a")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded.credential_id, 7);
        assert_eq!(loaded.soft_failure_count, 0);

        let rebound = SchedulerSessionBinding {
            credential_id: 8,
            last_used_at: Utc::now(),
            soft_failure_count: 0,
        };
        store
            .set_session_binding("session-a", &rebound, 60)
            .await
            .unwrap();
        assert_eq!(store.delete_sessions_for_credential(7).await.unwrap(), 0);
        let loaded = store
            .get_session_binding("session-a")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded.credential_id, 8);
        assert_eq!(loaded.soft_failure_count, 0);

        assert!(
            !store
                .record_session_soft_failure("session-a", 8, 2, 60)
                .await
                .unwrap()
        );
        assert!(
            store
                .record_session_soft_failure("session-a", 8, 2, 60)
                .await
                .unwrap()
        );
        let same_credential = SchedulerSessionBinding {
            credential_id: 8,
            last_used_at: Utc::now(),
            soft_failure_count: 0,
        };
        let preserved = store
            .set_session_binding("session-a", &same_credential, 60)
            .await
            .unwrap();
        assert_eq!(
            preserved.soft_failure_count, 2,
            "rebinding to the same credential must not overwrite the atomic failure count"
        );
        store
            .clear_session_soft_failure("session-a", 8, 60)
            .await
            .unwrap();
        let loaded = store
            .get_session_binding("session-a")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded.soft_failure_count, 0);

        assert_eq!(store.delete_sessions_for_credential(8).await.unwrap(), 1);
        assert!(
            store
                .get_session_binding("session-a")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn redis_runtime_event_pubsub_round_trip() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let mut pubsub = store.subscribe_runtime_events().await.unwrap();
        let expected_channel = store.runtime_config_changed_channel();
        let mut stream = pubsub.on_message();

        store
            .publish_runtime_config_changed(r#"{"kind":"runtime_config_changed"}"#)
            .await
            .unwrap();

        let message = tokio::time::timeout(StdDuration::from_secs(2), stream.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.get_channel_name(), expected_channel);
        assert_eq!(
            message.get_payload::<String>().unwrap(),
            r#"{"kind":"runtime_config_changed"}"#
        );
    }

    #[tokio::test]
    async fn redis_scheduler_cooldown_and_rate_limit_round_trip() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        store.clear_scheduler_cooldown(3).await.unwrap();
        let cooldown = store
            .set_scheduler_cooldown(3, StdDuration::from_secs(30), Some("429".to_string()))
            .await
            .unwrap();
        assert!(cooldown.until_ms > now_ms());
        let loaded = store.get_scheduler_cooldown(3).await.unwrap().unwrap();
        assert_eq!(loaded.reason.as_deref(), Some("429"));
        let shorter = store
            .set_scheduler_cooldown(3, StdDuration::from_secs(1), Some("short".to_string()))
            .await
            .unwrap();
        assert_eq!(shorter, cooldown);
        store.clear_scheduler_cooldown(3).await.unwrap();
        assert!(store.get_scheduler_cooldown(3).await.unwrap().is_none());

        let first = store
            .bump_rate_limit_available_at(3, StdDuration::from_millis(50))
            .await
            .unwrap();
        let second = store
            .bump_rate_limit_available_at(3, StdDuration::from_millis(50))
            .await
            .unwrap();
        assert!(second > first);
        assert_eq!(
            store.get_rate_limit_available_at(3).await.unwrap(),
            Some(second)
        );
        store.clear_rate_limit(3).await.unwrap();
        assert!(
            store
                .get_rate_limit_available_at(3)
                .await
                .unwrap()
                .is_none()
        );

        let reservation_credential_id = 33;
        store
            .clear_scheduler_health(reservation_credential_id)
            .await
            .unwrap();
        store
            .clear_rate_limit(reservation_credential_id)
            .await
            .unwrap();
        let reserved_once = store
            .try_record_scheduler_selection(reservation_credential_id, 2, 1)
            .await
            .unwrap();
        assert!(
            matches!(
                &reserved_once,
                SchedulerSelectionReservation::Recorded {
                    rate_limit_available_at_ms: None,
                    ..
                }
            ),
            "first reservation should remain below a 2 RPM window: {reserved_once:?}"
        );
        let reserved_twice = store
            .try_record_scheduler_selection(reservation_credential_id, 2, 1)
            .await
            .unwrap();
        let SchedulerSelectionReservation::Recorded {
            rate_limit_available_at_ms: Some(second_deadline),
            ..
        } = reserved_twice
        else {
            panic!("second reservation should set the shared RPM deadline: {reserved_twice:?}");
        };
        let third = store
            .try_record_scheduler_selection(reservation_credential_id, 2, 1)
            .await
            .unwrap();
        let SchedulerSelectionReservation::RateLimited {
            retry_after_ms,
            rate_limit_available_at_ms,
        } = third
        else {
            panic!("third reservation should be rejected by the shared RPM deadline: {third:?}");
        };
        assert_eq!(rate_limit_available_at_ms, second_deadline);
        assert!(retry_after_ms > 1_000);
        store
            .clear_scheduler_health(reservation_credential_id)
            .await
            .unwrap();
        store
            .clear_rate_limit(reservation_credential_id)
            .await
            .unwrap();

        let (_, failure_health) = store
            .record_scheduler_transient_failure(
                3,
                None,
                "rate_limit",
                "429",
                None,
                StdDuration::from_secs(1),
                StdDuration::from_secs(30),
                2.0,
                1.0,
                StdDuration::from_secs(5),
                0.2,
                StdDuration::from_secs(1),
            )
            .await
            .unwrap();
        assert_eq!(failure_health.transient_failure_streak, 1);
        assert!(failure_health.recent_error_rate > 0.0);
        let success_health = store
            .record_scheduler_success(3, None, Some(StdDuration::from_millis(120)), 0.2)
            .await
            .unwrap();
        assert_eq!(success_health.transient_failure_streak, 0);
        assert_eq!(success_health.latency_ewma_ms, Some(120.0));

        store.clear_scheduler_health(4).await.unwrap();
        store.clear_scheduler_cooldown(4).await.unwrap();
        let (model_cooldown, model_failure_health) = store
            .record_scheduler_transient_failure(
                4,
                Some("claude-opus-4.8"),
                "rate_limit",
                "429 opus",
                Some(StdDuration::from_secs(10)),
                StdDuration::from_secs(1),
                StdDuration::from_secs(30),
                2.0,
                1.0,
                StdDuration::from_secs(5),
                0.2,
                StdDuration::from_secs(1),
            )
            .await
            .unwrap();
        assert_eq!(model_cooldown.model.as_deref(), Some("claude-opus-4.8"));
        assert_eq!(model_failure_health.transient_failure_streak, 1);
        let model_states = store.scheduler_state_for_credentials(&[4]).await.unwrap();
        let credential_state = model_states.get(&4).unwrap();
        assert!(credential_state.cooldown.is_none());
        let opus_state = credential_state
            .model_states
            .iter()
            .find(|state| state.model == "claude-opus-4.8")
            .unwrap();
        assert_eq!(
            opus_state
                .cooldown
                .as_ref()
                .and_then(|cooldown| cooldown.reason.as_deref()),
            Some("429 opus")
        );
        let model_success_health = store
            .record_scheduler_success(
                4,
                Some("claude-opus-4.8"),
                Some(StdDuration::from_millis(88)),
                0.2,
            )
            .await
            .unwrap();
        assert_eq!(model_success_health.transient_failure_streak, 0);
        assert!(store.get_scheduler_cooldown(4).await.unwrap().is_none());

        let selected_once = store.record_scheduler_selection(3, 60, 1).await.unwrap();
        assert_eq!(selected_once.selection_count, 1);
        assert_eq!(selected_once.recent_selection_count_10s, 1);
        assert_eq!(selected_once.recent_selection_count_60s, 1);
        assert_eq!(selected_once.recent_selection_count_5m, 1);
        let selected_twice = store.record_scheduler_selection(3, 60, 1).await.unwrap();
        assert_eq!(selected_twice.selection_count, 2);
        assert_eq!(
            store
                .scheduler_state_for_credentials(&[3])
                .await
                .unwrap()
                .get(&3)
                .unwrap()
                .health
                .recent_selection_count_60s,
            2
        );
        let selected_weighted = store.record_scheduler_selection(5, 60, 4).await.unwrap();
        assert_eq!(selected_weighted.selection_count, 1);
        assert_eq!(selected_weighted.recent_selection_count_10s, 4);
        assert_eq!(selected_weighted.recent_selection_count_60s, 4);
        assert_eq!(selected_weighted.recent_selection_count_5m, 4);
        store.clear_scheduler_health(3).await.unwrap();
        store.clear_scheduler_health(4).await.unwrap();
        store.clear_scheduler_health(5).await.unwrap();
    }

    #[tokio::test]
    async fn redis_local_pool_circuit_uses_sliding_window_and_distinct_credentials() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let window = StdDuration::from_millis(80);
        let open_for = StdDuration::from_secs(2);

        let state = store.local_pool_circuit_state(window).await.unwrap();
        assert!(!state.open);
        assert_eq!(state.recent_failures, 0);
        assert_eq!(state.distinct_credentials, 0);

        let state = store
            .record_local_pool_circuit_failure(
                Some(1),
                "local_transient_exhausted",
                window,
                3,
                2,
                open_for,
            )
            .await
            .unwrap();
        assert!(!state.open);
        assert_eq!(state.recent_failures, 1);
        assert_eq!(state.distinct_credentials, 1);
        assert_eq!(state.reason, None);

        let state = store
            .record_local_pool_circuit_failure(
                Some(1),
                "local_transient_exhausted",
                window,
                3,
                2,
                open_for,
            )
            .await
            .unwrap();
        assert!(!state.open);
        assert_eq!(state.recent_failures, 2);
        assert_eq!(state.distinct_credentials, 1);

        let state = store
            .record_local_pool_circuit_failure(
                Some(2),
                "local_transient_exhausted",
                window,
                3,
                2,
                open_for,
            )
            .await
            .unwrap();
        assert!(state.open);
        assert!(state.open_until_ms.is_some());
        assert_eq!(state.recent_failures, 3);
        assert_eq!(state.distinct_credentials, 2);
        assert_eq!(state.reason.as_deref(), Some("local_transient_exhausted"));

        tokio::time::sleep(StdDuration::from_millis(110)).await;
        let state = store.local_pool_circuit_state(window).await.unwrap();
        assert!(state.open);
        assert_eq!(state.recent_failures, 0);
        assert_eq!(state.distinct_credentials, 0);
        assert_eq!(state.reason.as_deref(), Some("local_transient_exhausted"));
    }

    #[tokio::test]
    async fn redis_scheduler_in_flight_acquire_release_and_cleanup() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let lease_a = store.next_in_flight_lease_id().await.unwrap();
        assert!(
            store
                .acquire_in_flight_lease(9, lease_a, 1, Some(StdDuration::from_secs(60)), "api",)
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(
            store
                .global_capacity_state()
                .await
                .unwrap()
                .in_flight_requests,
            1
        );
        let lease_b = store.next_in_flight_lease_id().await.unwrap();
        assert!(
            store
                .acquire_in_flight_lease(9, lease_b, 1, Some(StdDuration::from_secs(60)), "api",)
                .await
                .unwrap()
                .is_none()
        );

        let state = store.scheduler_state_for_credentials(&[9]).await.unwrap();
        assert_eq!(state.get(&9).unwrap().in_flight_leases.len(), 1);
        assert!(store.release_in_flight_lease(9, lease_a).await.unwrap());
        assert_eq!(
            store
                .global_capacity_state()
                .await
                .unwrap()
                .in_flight_requests,
            0
        );
        let state = store.scheduler_state_for_credentials(&[9]).await.unwrap();
        assert_eq!(state.get(&9).unwrap().in_flight_leases.len(), 0);

        assert!(
            store
                .acquire_in_flight_lease(
                    9,
                    lease_b,
                    1,
                    Some(StdDuration::from_millis(1)),
                    "stream",
                )
                .await
                .unwrap()
                .is_some()
        );
        tokio::time::sleep(StdDuration::from_millis(5)).await;
        assert_eq!(
            store
                .cleanup_expired_in_flight_leases(&[9], StdDuration::from_millis(1))
                .await
                .unwrap(),
            1
        );
        let state = store.scheduler_state_for_credentials(&[9]).await.unwrap();
        assert_eq!(state.get(&9).unwrap().in_flight_leases.len(), 0);
    }

    #[tokio::test]
    async fn redis_dispatch_queue_leases_enforce_cross_manager_limit_and_idempotence() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store_a = RedisStore::connect(&config).await.unwrap();
        let store_b = RedisStore::connect(&config).await.unwrap();
        assert!(
            store_a
                .try_enter_dispatch_queue("manager-a:lease-1", 1, 60)
                .await
                .unwrap()
        );
        assert!(
            store_b
                .try_enter_dispatch_queue("manager-a:lease-1", 1, 60)
                .await
                .unwrap(),
            "retrying the same lease must remain admitted even at the queue limit"
        );
        assert!(
            !store_b
                .try_enter_dispatch_queue("manager-b:lease-1", 1, 60)
                .await
                .unwrap(),
            "a different manager must observe the shared queue limit"
        );
        assert!(
            !store_b.leave_dispatch_queue("missing-lease").await.unwrap(),
            "releasing an unknown lease must not decrement another waiter"
        );
        assert_eq!(
            store_a
                .global_capacity_state()
                .await
                .unwrap()
                .queued_requests,
            1
        );

        assert!(
            store_a
                .leave_dispatch_queue("manager-a:lease-1")
                .await
                .unwrap()
        );
        assert!(
            !store_a
                .leave_dispatch_queue("manager-a:lease-1")
                .await
                .unwrap(),
            "queue lease release must be idempotent"
        );
        assert!(
            store_b
                .try_enter_dispatch_queue("manager-b:lease-1", 1, 60)
                .await
                .unwrap()
        );
        assert!(
            store_b
                .leave_dispatch_queue("manager-b:lease-1")
                .await
                .unwrap()
        );
        assert_eq!(
            store_a
                .global_capacity_state()
                .await
                .unwrap()
                .queued_requests,
            0
        );
    }

    #[tokio::test]
    async fn redis_dispatch_queue_prunes_stale_leases_before_admission_and_counting() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let queue_key = store.key(scheduler_global_queue_key());
        let mut manager = store.scheduler_capacity_manager();
        let _: i64 = redis::cmd("ZADD")
            .arg(&queue_key)
            .arg(1)
            .arg("expired-lease")
            .query_async(&mut manager)
            .await
            .unwrap();

        assert!(
            store
                .try_enter_dispatch_queue("live-lease", 1, 60)
                .await
                .unwrap(),
            "an expired lease must not consume the queue limit"
        );
        assert_eq!(
            store.global_capacity_state().await.unwrap().queued_requests,
            1
        );
        let expired_score: Option<f64> = redis::cmd("ZSCORE")
            .arg(&queue_key)
            .arg("expired-lease")
            .query_async(&mut manager)
            .await
            .unwrap();
        assert!(expired_score.is_none());
        assert!(store.leave_dispatch_queue("live-lease").await.unwrap());
    }

    #[tokio::test]
    async fn redis_dispatch_queue_commit_unknown_cleanup_only_removes_its_lease() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let _unknown_result = store
            .try_enter_dispatch_queue("commit-unknown", 2, 60)
            .await
            .unwrap();
        assert!(
            store
                .try_enter_dispatch_queue("unrelated-waiter", 2, 60)
                .await
                .unwrap()
        );

        assert!(store.leave_dispatch_queue("commit-unknown").await.unwrap());
        assert!(!store.leave_dispatch_queue("commit-unknown").await.unwrap());
        assert_eq!(
            store.global_capacity_state().await.unwrap().queued_requests,
            1,
            "commit-unknown cleanup must not pollute the global count"
        );
        assert!(
            store
                .leave_dispatch_queue("unrelated-waiter")
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn redis_dispatch_queue_admission_and_renewal_only_update_their_own_lease() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let queue_key = store.key(scheduler_global_queue_key());
        assert!(
            store
                .try_enter_dispatch_queue("earlier-lease", 2, 60)
                .await
                .unwrap()
        );
        let mut manager = store.scheduler_capacity_manager();
        let before: f64 = redis::cmd("ZSCORE")
            .arg(&queue_key)
            .arg("earlier-lease")
            .query_async(&mut manager)
            .await
            .unwrap();

        assert!(
            store
                .try_enter_dispatch_queue("later-lease", 2, 120)
                .await
                .unwrap()
        );
        let after: f64 = redis::cmd("ZSCORE")
            .arg(&queue_key)
            .arg("earlier-lease")
            .query_async(&mut manager)
            .await
            .unwrap();
        let later: f64 = redis::cmd("ZSCORE")
            .arg(&queue_key)
            .arg("later-lease")
            .query_async(&mut manager)
            .await
            .unwrap();

        assert_eq!(before, after);
        assert!(later > after);
        assert!(
            store
                .renew_dispatch_queue("earlier-lease", 180)
                .await
                .unwrap()
        );
        let renewed: f64 = redis::cmd("ZSCORE")
            .arg(&queue_key)
            .arg("earlier-lease")
            .query_async(&mut manager)
            .await
            .unwrap();
        let later_after_renewal: f64 = redis::cmd("ZSCORE")
            .arg(&queue_key)
            .arg("later-lease")
            .query_async(&mut manager)
            .await
            .unwrap();
        assert!(renewed > later);
        assert_eq!(later, later_after_renewal);
        assert!(
            !store
                .renew_dispatch_queue("missing-lease", 180)
                .await
                .unwrap(),
            "renewal must not recreate an expired or released queue lease"
        );
        assert!(store.leave_dispatch_queue("earlier-lease").await.unwrap());
        assert!(store.leave_dispatch_queue("later-lease").await.unwrap());
    }

    #[tokio::test]
    async fn redis_external_pool_snapshot_and_acquire_are_atomic_across_managers() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store_a = RedisStore::connect(&config).await.unwrap();
        let store_b = RedisStore::connect(&config).await.unwrap();
        initialize_test_external_pool_coordinator(&store_a).await;
        let pool_id = 77;
        let other_pool_id = 78;
        let pool_cooldown_key = format!("external_pool:{pool_id}:cooldown");
        let model_cooldown_key = format!("external_pool:{pool_id}:model_cooldown:test");
        let cooldown_keys = vec![pool_cooldown_key.clone(), model_cooldown_key.clone()];
        let other_cooldown_keys = vec![format!("external_pool:{other_pool_id}:cooldown")];
        store_a
            .set_json(
                &pool_cooldown_key,
                &CachedValue {
                    value: "pool".to_string(),
                },
                60,
            )
            .await
            .unwrap();
        store_a
            .set_json(
                &model_cooldown_key,
                &CachedValue {
                    value: "model".to_string(),
                },
                60,
            )
            .await
            .unwrap();

        let snapshot = store_b
            .external_pool_coordinator_snapshot(
                pool_id,
                Some(StdDuration::from_secs(60)),
                &cooldown_keys,
                TEST_EXTERNAL_POOL_COORDINATOR_EPOCH,
            )
            .await
            .unwrap();
        assert_eq!(snapshot.capacity, ExternalPoolCapacityState::default());
        assert_eq!(snapshot.cooldown_values.len(), 2);
        assert_eq!(
            serde_json::from_str::<CachedValue>(snapshot.cooldown_values[0].as_deref().unwrap())
                .unwrap()
                .value,
            "pool"
        );
        assert_eq!(
            serde_json::from_str::<CachedValue>(snapshot.cooldown_values[1].as_deref().unwrap())
                .unwrap()
                .value,
            "model"
        );

        let blocked = store_b
            .acquire_external_pool_lease(
                pool_id,
                "lease-cooldown-blocked",
                TEST_EXTERNAL_POOL_COORDINATOR_EPOCH,
                1,
                2,
                Some(StdDuration::from_secs(60)),
                &cooldown_keys,
            )
            .await
            .unwrap();
        assert!(matches!(
            blocked,
            ExternalPoolLeaseAcquireResult::PoolCooldown { remaining: Some(_) }
        ));
        store_a.del(&pool_cooldown_key).await.unwrap();
        store_a.del(&model_cooldown_key).await.unwrap();

        for round in 0..3 {
            let before_model_race = store_b
                .external_pool_coordinator_snapshot(
                    pool_id,
                    Some(StdDuration::from_secs(60)),
                    &cooldown_keys,
                    TEST_EXTERNAL_POOL_COORDINATOR_EPOCH,
                )
                .await
                .unwrap();
            assert_eq!(
                before_model_race.cooldown_values,
                vec![None, None],
                "round {round}: selection snapshot should precede the simulated cross-instance model cooldown write"
            );
            store_a
                .set_json(
                    &model_cooldown_key,
                    &CachedValue {
                        value: format!("model-race-{round}"),
                    },
                    60,
                )
                .await
                .unwrap();
            let model_blocked = store_b
                .acquire_external_pool_lease(
                    pool_id,
                    "lease-model-cooldown-blocked",
                    TEST_EXTERNAL_POOL_COORDINATOR_EPOCH,
                    1,
                    2,
                    Some(StdDuration::from_secs(60)),
                    &cooldown_keys,
                )
                .await
                .unwrap();
            assert!(matches!(
                model_blocked,
                ExternalPoolLeaseAcquireResult::ModelCooldown { remaining: Some(_) }
            ));
            assert_eq!(
                store_a
                    .external_pool_coordinator_snapshot(
                        pool_id,
                        Some(StdDuration::from_secs(60)),
                        &cooldown_keys,
                        TEST_EXTERNAL_POOL_COORDINATOR_EPOCH,
                    )
                    .await
                    .unwrap()
                    .capacity,
                ExternalPoolCapacityState::default(),
                "round {round}: model cooldown race rejection must not occupy a pool or global lease"
            );
            store_a.del(&model_cooldown_key).await.unwrap();
        }

        let first_lease_id = match store_a
            .acquire_external_pool_lease(
                pool_id,
                "lease-first",
                TEST_EXTERNAL_POOL_COORDINATOR_EPOCH,
                1,
                2,
                Some(StdDuration::from_secs(60)),
                &cooldown_keys,
            )
            .await
            .unwrap()
        {
            ExternalPoolLeaseAcquireResult::Acquired {
                lease_id,
                pool_in_flight_requests: 1,
                global_in_flight_requests: 1,
            } => lease_id,
            outcome => panic!("first lease should be acquired: {outcome:?}"),
        };
        assert!(matches!(
            store_b
                .acquire_external_pool_lease(
                    pool_id,
                    "lease-pool-capacity-blocked",
                    TEST_EXTERNAL_POOL_COORDINATOR_EPOCH,
                    1,
                    2,
                    Some(StdDuration::from_secs(60)),
                    &cooldown_keys,
                )
                .await
                .unwrap(),
            ExternalPoolLeaseAcquireResult::PoolCapacityFull {
                in_flight_requests: 1
            }
        ));
        assert!(matches!(
            store_b
                .acquire_external_pool_lease(
                    other_pool_id,
                    "lease-global-capacity-blocked",
                    TEST_EXTERNAL_POOL_COORDINATOR_EPOCH,
                    1,
                    1,
                    Some(StdDuration::from_secs(60)),
                    &other_cooldown_keys,
                )
                .await
                .unwrap(),
            ExternalPoolLeaseAcquireResult::GlobalCapacityFull {
                in_flight_requests: 1
            }
        ));
        assert!(
            store_b
                .release_external_pool_confirmed_lease(pool_id, &first_lease_id)
                .await
                .unwrap()
        );
        assert!(
            !store_a
                .release_external_pool_confirmed_lease(pool_id, &first_lease_id)
                .await
                .unwrap(),
            "lease release must remain idempotent across managers"
        );

        let second_lease_id = match store_b
            .acquire_external_pool_lease(
                pool_id,
                "lease-second",
                TEST_EXTERNAL_POOL_COORDINATOR_EPOCH,
                1,
                1,
                Some(StdDuration::from_secs(60)),
                &cooldown_keys,
            )
            .await
            .unwrap()
        {
            ExternalPoolLeaseAcquireResult::Acquired { lease_id, .. } => lease_id,
            outcome => panic!("capacity should recover after release: {outcome:?}"),
        };
        assert!(
            second_lease_id != first_lease_id,
            "request-scoped lease IDs must remain unique across managers"
        );
        assert!(
            store_a
                .release_external_pool_confirmed_lease(pool_id, &second_lease_id)
                .await
                .unwrap()
        );

        let keys = external_pool_in_flight_keys(pool_id);
        let global_keys = external_pool_global_in_flight_keys();
        let stale_at = now_ms().saturating_sub(10_000);
        let mut manager = store_a.scheduler_capacity_manager();
        let _: () = redis::pipe()
            .cmd("ZADD")
            .arg(store_a.key(&keys.last_seen))
            .arg(stale_at)
            .arg("stale-lease")
            .cmd("ZADD")
            .arg(store_a.key(&keys.acquired))
            .arg(stale_at)
            .arg("stale-lease")
            .cmd("ZADD")
            .arg(store_a.key(&global_keys.last_seen))
            .arg(stale_at)
            .arg("stale-lease")
            .cmd("ZADD")
            .arg(store_a.key(&global_keys.acquired))
            .arg(stale_at)
            .arg("stale-lease")
            .query_async(&mut manager)
            .await
            .unwrap();
        let recovered_lease_id = match store_a
            .acquire_external_pool_lease(
                pool_id,
                "lease-recovered",
                TEST_EXTERNAL_POOL_COORDINATOR_EPOCH,
                1,
                1,
                Some(StdDuration::from_millis(1)),
                &cooldown_keys,
            )
            .await
            .unwrap()
        {
            ExternalPoolLeaseAcquireResult::Acquired {
                lease_id,
                pool_in_flight_requests: 1,
                global_in_flight_requests: 1,
            } => lease_id,
            outcome => panic!("expired leases should be pruned atomically: {outcome:?}"),
        };
        assert!(
            store_b
                .release_external_pool_confirmed_lease(pool_id, &recovered_lease_id)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn redis_external_pool_commit_unknown_cleanup_tombstone_blocks_late_acquire() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store_a = RedisStore::connect(&config).await.unwrap();
        let store_b = RedisStore::connect(&config).await.unwrap();
        initialize_test_external_pool_coordinator(&store_a).await;
        let pool_id = 79;
        let cooldown_keys = vec![format!("external_pool:{pool_id}:cooldown")];

        for round in 0..3 {
            let cancelled_lease_id = format!("commit-unknown-{round}");
            assert!(
                !store_a
                    .release_external_pool_pending_lease(pool_id, &cancelled_lease_id)
                    .await
                    .unwrap(),
                "round {round}: preemptive cleanup has no committed lease to remove"
            );
            assert!(matches!(
                store_b
                    .acquire_external_pool_lease(
                        pool_id,
                        &cancelled_lease_id,
                        TEST_EXTERNAL_POOL_COORDINATOR_EPOCH,
                        1,
                        1,
                        Some(StdDuration::from_secs(60)),
                        &cooldown_keys,
                    )
                    .await
                    .unwrap(),
                ExternalPoolLeaseAcquireResult::Released
            ));
            assert_eq!(
                store_b
                    .external_pool_coordinator_snapshot(
                        pool_id,
                        Some(StdDuration::from_secs(60)),
                        &cooldown_keys,
                        TEST_EXTERNAL_POOL_COORDINATOR_EPOCH,
                    )
                    .await
                    .unwrap()
                    .capacity,
                ExternalPoolCapacityState::default(),
                "round {round}: a late acquire must not consume pool or global capacity"
            );

            let recovered_lease_id = format!("recovered-after-commit-unknown-{round}");
            assert!(matches!(
                store_b
                    .acquire_external_pool_lease(
                        pool_id,
                        &recovered_lease_id,
                        TEST_EXTERNAL_POOL_COORDINATOR_EPOCH,
                        1,
                        1,
                        Some(StdDuration::from_secs(60)),
                        &cooldown_keys,
                    )
                    .await
                    .unwrap(),
                ExternalPoolLeaseAcquireResult::Acquired { .. }
            ));
            assert!(
                store_a
                    .release_external_pool_confirmed_lease(pool_id, &recovered_lease_id)
                    .await
                    .unwrap(),
                "round {round}: capacity must remain releasable after recovery"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn redis_external_pool_atomic_acquire_never_oversells_across_managers() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store_a = RedisStore::connect(&config).await.unwrap();
        let store_b = RedisStore::connect(&config).await.unwrap();
        initialize_test_external_pool_coordinator(&store_a).await;
        let pool_id = 80;
        let cooldown_keys = vec![format!("external_pool:{pool_id}:cooldown")];

        for round in 0..50 {
            let lease_a = format!("manager-a-{round}");
            let lease_b = format!("manager-b-{round}");
            let (result_a, result_b) = tokio::join!(
                store_a.acquire_external_pool_lease(
                    pool_id,
                    &lease_a,
                    TEST_EXTERNAL_POOL_COORDINATOR_EPOCH,
                    1,
                    1,
                    Some(StdDuration::from_secs(60)),
                    &cooldown_keys,
                ),
                store_b.acquire_external_pool_lease(
                    pool_id,
                    &lease_b,
                    TEST_EXTERNAL_POOL_COORDINATOR_EPOCH,
                    1,
                    1,
                    Some(StdDuration::from_secs(60)),
                    &cooldown_keys,
                ),
            );
            let result_a = result_a.unwrap();
            let result_b = result_b.unwrap();
            let acquired = [(&lease_a, &result_a), (&lease_b, &result_b)]
                .into_iter()
                .filter_map(|(lease_id, result)| {
                    matches!(result, ExternalPoolLeaseAcquireResult::Acquired { .. })
                        .then_some(lease_id)
                })
                .collect::<Vec<_>>();
            assert_eq!(
                acquired.len(),
                1,
                "round {round}: exactly one manager may hold capacity: {result_a:?}, {result_b:?}"
            );
            assert!(
                [&result_a, &result_b].into_iter().any(|result| matches!(
                    result,
                    ExternalPoolLeaseAcquireResult::PoolCapacityFull {
                        in_flight_requests: 1
                    }
                )),
                "round {round}: the losing manager must observe the shared pool limit"
            );
            assert!(
                store_a
                    .release_external_pool_confirmed_lease(pool_id, acquired[0])
                    .await
                    .unwrap(),
                "round {round}: winning lease must be releasable"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn redis_external_pool_two_managers_sixty_pools_never_oversell_across_10k_competitions() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let stores = [
            Arc::new(RedisStore::connect(&config).await.unwrap()),
            Arc::new(RedisStore::connect(&config).await.unwrap()),
        ];
        initialize_test_external_pool_coordinator(&stores[0]).await;
        let pool_ids = (1_000u64..1_060u64).collect::<Vec<_>>();
        let per_pool_limit = 1u32;
        let global_limit = 30u32;
        let requests_per_batch = 120usize;
        let batches = 84usize;
        let mut total_competitions = 0usize;

        for batch in 0..batches {
            let results = futures::future::join_all((0..requests_per_batch).map(|slot| {
                let store_index = (batch + slot) % stores.len();
                let store = stores[store_index].clone();
                let pool_id = pool_ids[(batch + slot) % pool_ids.len()];
                let lease_id = format!("manager-{store_index}-batch-{batch}-slot-{slot}");
                async move {
                    let cooldown_keys = vec![format!("external_pool:{pool_id}:cooldown")];
                    let result = store
                        .acquire_external_pool_lease(
                            pool_id,
                            &lease_id,
                            TEST_EXTERNAL_POOL_COORDINATOR_EPOCH,
                            per_pool_limit,
                            global_limit,
                            Some(StdDuration::from_secs(60)),
                            &cooldown_keys,
                        )
                        .await;
                    (store_index, pool_id, lease_id, result)
                }
            }))
            .await;
            total_competitions = total_competitions.saturating_add(results.len());

            let mut acquired = Vec::new();
            let mut acquired_per_pool = std::collections::HashMap::<u64, usize>::new();
            for (store_index, pool_id, lease_id, result) in results {
                match result.unwrap() {
                    ExternalPoolLeaseAcquireResult::Acquired {
                        pool_in_flight_requests,
                        global_in_flight_requests,
                        ..
                    } => {
                        assert!(
                            pool_in_flight_requests <= per_pool_limit,
                            "batch {batch}: Redis reported per-pool oversell"
                        );
                        assert!(
                            global_in_flight_requests <= global_limit,
                            "batch {batch}: Redis reported global oversell"
                        );
                        *acquired_per_pool.entry(pool_id).or_default() += 1;
                        acquired.push((store_index, pool_id, lease_id));
                    }
                    ExternalPoolLeaseAcquireResult::PoolCapacityFull { .. }
                    | ExternalPoolLeaseAcquireResult::GlobalCapacityFull { .. } => {}
                    other => panic!("batch {batch}: unexpected acquire result: {other:?}"),
                }
            }
            assert!(
                acquired.len() <= global_limit as usize,
                "batch {batch}: global capacity was oversold"
            );
            assert!(
                acquired_per_pool.values().all(|count| *count <= 1),
                "batch {batch}: a pool capacity was oversold: {acquired_per_pool:?}"
            );

            let releases = futures::future::join_all(acquired.into_iter().map(
                |(store_index, pool_id, lease_id)| {
                    let store = stores[store_index].clone();
                    async move {
                        store
                            .release_external_pool_confirmed_lease(pool_id, &lease_id)
                            .await
                    }
                },
            ))
            .await;
            assert!(
                releases
                    .into_iter()
                    .all(|released| matches!(released, Ok(true))),
                "batch {batch}: every confirmed lease must be released exactly once"
            );
        }

        assert!(total_competitions >= 10_000);
        let requests = pool_ids
            .iter()
            .map(|pool_id| ExternalPoolCoordinatorSnapshotRequest {
                pool_id: *pool_id,
                cooldown_keys: vec![format!("external_pool:{pool_id}:cooldown")],
            })
            .collect::<Vec<_>>();
        let snapshots = stores[0]
            .external_pool_coordinator_snapshots(
                &requests,
                Some(StdDuration::from_secs(60)),
                TEST_EXTERNAL_POOL_COORDINATOR_EPOCH,
            )
            .await
            .unwrap();
        assert_eq!(snapshots.len(), 60);
        assert!(
            snapshots
                .iter()
                .all(|snapshot| snapshot.capacity == ExternalPoolCapacityState::default()),
            "all confirmed leases must be cleared after 10k competitions"
        );

        let mut manager = stores[0].scheduler_capacity_manager();
        for pool_id in pool_ids {
            let keys = external_pool_in_flight_keys(pool_id);
            let tombstones: i64 = redis::cmd("ZCARD")
                .arg(stores[0].key(&keys.released))
                .query_async(&mut manager)
                .await
                .unwrap();
            assert_eq!(
                tombstones, 0,
                "confirmed release must not create pending tombstones for pool {pool_id}"
            );
        }
    }

    #[tokio::test]
    async fn redis_external_pool_confirmed_release_keeps_tombstones_empty_for_10k_rounds() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        initialize_test_external_pool_coordinator(&store).await;
        let pool_id = 81;
        let cooldown_keys = vec![format!("external_pool:{pool_id}:cooldown")];
        for round in 0..10_000 {
            let lease_id = format!("confirmed-{round}");
            assert!(matches!(
                store
                    .acquire_external_pool_lease(
                        pool_id,
                        &lease_id,
                        TEST_EXTERNAL_POOL_COORDINATOR_EPOCH,
                        1,
                        1,
                        Some(StdDuration::from_secs(60)),
                        &cooldown_keys,
                    )
                    .await
                    .unwrap(),
                ExternalPoolLeaseAcquireResult::Acquired { .. }
            ));
            assert!(
                store
                    .release_external_pool_confirmed_lease(pool_id, &lease_id)
                    .await
                    .unwrap()
            );
        }

        let keys = external_pool_in_flight_keys(pool_id);
        let mut manager = store.scheduler_capacity_manager();
        let tombstones: i64 = redis::cmd("ZCARD")
            .arg(store.key(&keys.released))
            .query_async(&mut manager)
            .await
            .unwrap();
        assert_eq!(tombstones, 0);
        let snapshot = store
            .external_pool_coordinator_snapshot(
                pool_id,
                Some(StdDuration::from_secs(60)),
                &cooldown_keys,
                TEST_EXTERNAL_POOL_COORDINATOR_EPOCH,
            )
            .await
            .unwrap();
        assert_eq!(snapshot.capacity, ExternalPoolCapacityState::default());
    }

    #[tokio::test]
    async fn redis_external_pool_pending_tombstone_expiry_never_shortens_newer_score() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let pool_id = 82;
        let keys = external_pool_in_flight_keys(pool_id);
        let released_key = store.key(&keys.released);
        let mut manager = store.scheduler_capacity_manager();
        let redis_time: (i64, i64) = redis::cmd("TIME").query_async(&mut manager).await.unwrap();
        let now_ms = redis_time.0 * 1_000 + redis_time.1 / 1_000;
        let later_expiry = now_ms + 10 * 60 * 1_000;
        let _: () = redis::pipe()
            .cmd("ZADD")
            .arg(&released_key)
            .arg(later_expiry)
            .arg("newer-pending")
            .cmd("PEXPIREAT")
            .arg(&released_key)
            .arg(later_expiry)
            .query_async(&mut manager)
            .await
            .unwrap();

        assert!(
            !store
                .release_external_pool_pending_lease(pool_id, "older-pending")
                .await
                .unwrap()
        );
        let ttl_ms: i64 = redis::cmd("PTTL")
            .arg(&released_key)
            .query_async(&mut manager)
            .await
            .unwrap();
        assert!(
            ttl_ms > 9 * 60 * 1_000,
            "an older pending cleanup shortened the newer tombstone TTL: {ttl_ms}ms"
        );
        let tombstones: i64 = redis::cmd("ZCARD")
            .arg(&released_key)
            .query_async(&mut manager)
            .await
            .unwrap();
        assert_eq!(tombstones, 2);
    }

    #[tokio::test]
    async fn redis_external_pool_queue_leases_enforce_cross_manager_limit_and_idempotence() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store_a = RedisStore::connect(&config).await.unwrap();
        let store_b = RedisStore::connect(&config).await.unwrap();
        assert!(
            store_a
                .try_enter_external_pool_dispatch_queue("manager-a:waiter-1", 1, 60)
                .await
                .unwrap()
        );
        assert!(
            store_b
                .try_enter_external_pool_dispatch_queue("manager-a:waiter-1", 1, 60)
                .await
                .unwrap(),
            "retrying the same external waiter must remain admitted at the limit"
        );
        assert!(
            !store_b
                .try_enter_external_pool_dispatch_queue("manager-b:waiter-1", 1, 60)
                .await
                .unwrap(),
            "another manager must observe the shared external queue limit"
        );
        assert!(
            !store_b
                .leave_external_pool_dispatch_queue("missing-waiter")
                .await
                .unwrap(),
            "an unknown release must not remove another external waiter"
        );
        assert_eq!(
            store_a.external_pool_dispatch_queue_size().await.unwrap(),
            1
        );
        assert!(
            store_a
                .leave_external_pool_dispatch_queue("manager-a:waiter-1")
                .await
                .unwrap()
        );
        assert!(
            !store_a
                .leave_external_pool_dispatch_queue("manager-a:waiter-1")
                .await
                .unwrap(),
            "external queue release must be idempotent"
        );
        assert_eq!(
            store_a.external_pool_dispatch_queue_size().await.unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn redis_external_pool_queue_prunes_stale_leases_before_admission() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let queue_key = store.key(external_pool_global_queue_key());
        let mut manager = store.scheduler_capacity_manager();
        let _: i64 = redis::cmd("ZADD")
            .arg(&queue_key)
            .arg(1)
            .arg("expired-waiter")
            .query_async(&mut manager)
            .await
            .unwrap();

        assert!(
            store
                .try_enter_external_pool_dispatch_queue("live-waiter", 1, 60)
                .await
                .unwrap(),
            "an expired external waiter must not consume the queue limit"
        );
        assert_eq!(store.external_pool_dispatch_queue_size().await.unwrap(), 1);
        let expired_score: Option<f64> = redis::cmd("ZSCORE")
            .arg(&queue_key)
            .arg("expired-waiter")
            .query_async(&mut manager)
            .await
            .unwrap();
        assert!(expired_score.is_none());
        assert!(
            store
                .leave_external_pool_dispatch_queue("live-waiter")
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn redis_external_pool_queue_commit_unknown_cleanup_only_removes_its_waiter() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let _unknown_result = store
            .try_enter_external_pool_dispatch_queue("commit-unknown", 2, 60)
            .await
            .unwrap();
        assert!(
            store
                .try_enter_external_pool_dispatch_queue("unrelated-waiter", 2, 60)
                .await
                .unwrap()
        );

        assert!(
            store
                .leave_external_pool_dispatch_queue("commit-unknown")
                .await
                .unwrap()
        );
        assert!(
            !store
                .leave_external_pool_dispatch_queue("commit-unknown")
                .await
                .unwrap()
        );
        assert_eq!(
            store.external_pool_dispatch_queue_size().await.unwrap(),
            1,
            "commit-unknown cleanup must preserve unrelated external waiters"
        );
        assert!(
            store
                .leave_external_pool_dispatch_queue("unrelated-waiter")
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn redis_external_pool_queue_admission_and_renewal_only_update_their_own_lease() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let queue_key = store.key(external_pool_global_queue_key());
        assert!(
            store
                .try_enter_external_pool_dispatch_queue("earlier-waiter", 2, 60)
                .await
                .unwrap()
        );
        let mut manager = store.scheduler_capacity_manager();
        let earlier_before: f64 = redis::cmd("ZSCORE")
            .arg(&queue_key)
            .arg("earlier-waiter")
            .query_async(&mut manager)
            .await
            .unwrap();

        assert!(
            store
                .try_enter_external_pool_dispatch_queue("later-waiter", 2, 120)
                .await
                .unwrap()
        );
        let earlier_after_admission: f64 = redis::cmd("ZSCORE")
            .arg(&queue_key)
            .arg("earlier-waiter")
            .query_async(&mut manager)
            .await
            .unwrap();
        let later_before_renewal: f64 = redis::cmd("ZSCORE")
            .arg(&queue_key)
            .arg("later-waiter")
            .query_async(&mut manager)
            .await
            .unwrap();
        assert_eq!(earlier_before, earlier_after_admission);
        assert!(later_before_renewal > earlier_after_admission);

        assert!(
            store
                .renew_external_pool_dispatch_queue("earlier-waiter", 180)
                .await
                .unwrap(),
            "an unbounded waiter must be able to renew its own lease"
        );
        let earlier_after_renewal: f64 = redis::cmd("ZSCORE")
            .arg(&queue_key)
            .arg("earlier-waiter")
            .query_async(&mut manager)
            .await
            .unwrap();
        let later_after_renewal: f64 = redis::cmd("ZSCORE")
            .arg(&queue_key)
            .arg("later-waiter")
            .query_async(&mut manager)
            .await
            .unwrap();
        assert!(earlier_after_renewal > earlier_after_admission);
        assert_eq!(later_before_renewal, later_after_renewal);
        assert!(
            !store
                .renew_external_pool_dispatch_queue("missing-waiter", 180)
                .await
                .unwrap()
        );

        assert!(
            store
                .leave_external_pool_dispatch_queue("earlier-waiter")
                .await
                .unwrap()
        );
        assert!(
            store
                .leave_external_pool_dispatch_queue("later-waiter")
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn redis_scheduler_weighted_in_flight_acquire_release_and_cleanup() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let credential_id = 90;
        let lease_a = store.next_in_flight_lease_id().await.unwrap();
        assert_eq!(
            store
                .acquire_dispatch_lease(
                    credential_id,
                    lease_a,
                    4,
                    4,
                    4,
                    Some(StdDuration::from_secs(60)),
                    "api",
                )
                .await
                .unwrap(),
            Some(4)
        );
        assert_eq!(
            store
                .global_capacity_state()
                .await
                .unwrap()
                .in_flight_requests,
            4
        );

        let lease_b = store.next_in_flight_lease_id().await.unwrap();
        assert!(
            store
                .acquire_dispatch_lease(
                    credential_id,
                    lease_b,
                    4,
                    4,
                    1,
                    Some(StdDuration::from_secs(60)),
                    "api",
                )
                .await
                .unwrap()
                .is_none()
        );

        assert!(
            store
                .release_in_flight_lease(credential_id, lease_a)
                .await
                .unwrap()
        );
        assert_eq!(
            store
                .global_capacity_state()
                .await
                .unwrap()
                .in_flight_requests,
            0
        );

        let lease_c = store.next_in_flight_lease_id().await.unwrap();
        assert!(
            store
                .acquire_dispatch_lease(
                    credential_id,
                    lease_c,
                    4,
                    4,
                    4,
                    Some(StdDuration::from_millis(1)),
                    "stream",
                )
                .await
                .unwrap()
                .is_some()
        );
        tokio::time::sleep(StdDuration::from_millis(5)).await;
        assert_eq!(
            store
                .cleanup_expired_in_flight_leases(&[credential_id], StdDuration::from_millis(1))
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .global_capacity_state()
                .await
                .unwrap()
                .in_flight_requests,
            0
        );
    }

    #[tokio::test]
    async fn redis_scheduler_clearing_one_weighted_credential_keeps_other_global_count() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let first_id = 91;
        let second_id = 92;
        let first_lease = store.next_in_flight_lease_id().await.unwrap();
        let second_lease = store.next_in_flight_lease_id().await.unwrap();
        assert!(
            store
                .acquire_dispatch_lease(
                    first_id,
                    first_lease,
                    8,
                    16,
                    4,
                    Some(StdDuration::from_secs(60)),
                    "api",
                )
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .acquire_dispatch_lease(
                    second_id,
                    second_lease,
                    8,
                    16,
                    2,
                    Some(StdDuration::from_secs(60)),
                    "api",
                )
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(
            store
                .global_capacity_state()
                .await
                .unwrap()
                .in_flight_requests,
            6
        );

        assert_eq!(
            store.clear_in_flight_leases(first_id, None).await.unwrap(),
            1
        );
        assert_eq!(
            store
                .global_capacity_state()
                .await
                .unwrap()
                .in_flight_requests,
            2
        );
        let states = store
            .scheduler_state_for_credentials(&[first_id, second_id])
            .await
            .unwrap();
        assert_eq!(states.get(&first_id).unwrap().in_flight_leases.len(), 0);
        assert_eq!(states.get(&second_id).unwrap().in_flight_leases.len(), 1);

        assert!(
            store
                .release_in_flight_lease(second_id, second_lease)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn redis_scheduler_late_touch_after_release_does_not_reoccupy_capacity() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let credential_id = 42;
        let lease_a = store.next_in_flight_lease_id().await.unwrap();
        assert!(
            store
                .acquire_in_flight_lease(
                    credential_id,
                    lease_a,
                    1,
                    Some(StdDuration::from_secs(60)),
                    "stream",
                )
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .release_in_flight_lease(credential_id, lease_a)
                .await
                .unwrap()
        );

        store
            .touch_in_flight_lease(credential_id, lease_a)
            .await
            .unwrap();
        store
            .set_in_flight_lease_kind(credential_id, lease_a, "stream")
            .await
            .unwrap();
        assert_eq!(
            store
                .global_capacity_state()
                .await
                .unwrap()
                .in_flight_requests,
            0
        );
        let state = store
            .scheduler_state_for_credentials(&[credential_id])
            .await
            .unwrap();
        assert_eq!(state.get(&credential_id).unwrap().in_flight_leases.len(), 0);

        let lease_b = store.next_in_flight_lease_id().await.unwrap();
        assert!(
            store
                .acquire_in_flight_lease(
                    credential_id,
                    lease_b,
                    1,
                    Some(StdDuration::from_secs(60)),
                    "stream",
                )
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn redis_scheduler_tombstone_blocks_late_acquire_after_release() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let credential_id = 43;
        let lease_id = store.next_in_flight_lease_id().await.unwrap();

        assert!(
            !store
                .release_in_flight_lease_with_tombstone(credential_id, lease_id)
                .await
                .unwrap(),
            "release may run before a timed-out Redis acquire has actually written the lease"
        );
        assert!(
            store
                .acquire_in_flight_lease(
                    credential_id,
                    lease_id,
                    1,
                    Some(StdDuration::from_secs(60)),
                    "stream",
                )
                .await
                .unwrap()
                .is_none(),
            "a late acquire for a released lease must not reoccupy Redis capacity"
        );
        assert_eq!(
            store
                .global_capacity_state()
                .await
                .unwrap()
                .in_flight_requests,
            0
        );
        let state = store
            .scheduler_state_for_credentials(&[credential_id])
            .await
            .unwrap();
        assert_eq!(state.get(&credential_id).unwrap().in_flight_leases.len(), 0);

        let next_lease_id = store.next_in_flight_lease_id().await.unwrap();
        assert!(
            store
                .acquire_in_flight_lease(
                    credential_id,
                    next_lease_id,
                    1,
                    Some(StdDuration::from_secs(60)),
                    "stream",
                )
                .await
                .unwrap()
                .is_some(),
            "tombstone must only block the released lease id"
        );
        assert!(
            store
                .release_in_flight_lease(credential_id, next_lease_id)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn redis_scheduler_refresh_lock_is_exclusive() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let lock = store.acquire_refresh_lock(11, 30).await.unwrap().unwrap();
        assert!(store.acquire_refresh_lock(11, 30).await.unwrap().is_none());
        assert!(store.release_refresh_lock(11, &lock).await.unwrap());
        assert!(store.acquire_refresh_lock(11, 30).await.unwrap().is_some());
    }
}
