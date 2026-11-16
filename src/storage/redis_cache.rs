const DISPATCH_QUEUE_PRUNE_AND_COUNT_SCRIPT: &str = r#"
    local now = redis.call('TIME')
    local now_ms = tonumber(now[1]) * 1000 + math.floor(tonumber(now[2]) / 1000)
    redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', now_ms)
    return redis.call('ZCARD', KEYS[1])
"#;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration as ChronoDuration, Timelike, Utc};
use redis::aio::{ConnectionManager, PubSub};
use redis::{AsyncCommands, ToRedisArgs};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use crate::anthropic::usage::{
    REALTIME_USAGE_WINDOW_SECS, UsageAccountBillingByPool, UsageAccountBillingSummary,
    UsageAggregate, UsageBreakdownItem, UsageDashboardResponse, UsageDashboardSeries,
    UsageDashboardSummary, UsageDashboardTop, UsageDashboardWindow, UsageDashboardWindowSpec,
    UsageRealtimeStats, UsageRecord, UsageRecordQuery, UsageRecordStatus, UsageRecordsPageResult,
    UsageRouteKind, UsageSeriesPoint, UsageSource, UsageSummary, UsageTopAggregate,
    usage_dashboard_daily_windows, usage_dashboard_hourly_windows, usage_dashboard_timezone,
    usage_dashboard_windows,
};
use crate::model::config::{Config, RedisConfig};

const REDIS_PATTERN_DELETE_SCAN_COUNT: usize = 128;
const REDIS_PATTERN_DELETE_COMMAND_KEY_LIMIT: usize = 64;
const REDIS_PATTERN_DELETE_MAX_PASSES: usize = 8;
const ACCOUNT_PENDING_LEASE_TOMBSTONE_TTL_MILLIS: i64 = 5 * 60 * 1_000;
const ACCOUNT_COORDINATOR_EPOCH_KEY: &str = "account:coordinator:coordination_epoch";
const ACCOUNT_COORDINATOR_RECOVERY_KEY: &str = "account:coordinator:recovery_until";
const ACCOUNT_DATA_GENERATION_KEY: &str = "account:data:generation";
/// Lua 侧在遇到非预期 Redis 类型时回填的哨兵值。
const INVALID_REDIS_TYPE_SENTINEL: &str = "__claude_pool_invalid_redis_type__";

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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountCapacityState {
    pub pool_in_flight_requests: u32,
    pub global_in_flight_requests: u32,
}

/// 账号质量采样的调参项。
///
/// 这三个值都来自运行时配置而非单次请求，打包传递可以让采样入口的签名
/// 只保留「这次请求发生了什么」，避免调用点把一串裸标量排错顺序。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AccountQualitySampleTuning {
    /// 失败率/延迟 EWMA 的平滑系数。
    pub alpha: f64,
    /// 质量状态在 Redis 中的存活时长。
    pub ttl: StdDuration,
    /// 劣化判定窗口；`ZERO` 表示不维护独立窗口。
    pub degrade_window: StdDuration,
}

/// 账号被动质量采样状态。
///
/// 全部字段都来自真实用户请求，不做任何主动探测。该状态只在 Redis 中保存
/// 短窗口数据，实例重启或 Redis 清空后重新累积；调度器读取失败时必须退化为
/// 中性值，不得阻塞主请求。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AccountQualityState {
    /// 失败率 EWMA，范围 `0.0..=1.0`。
    pub recent_error_rate: f64,
    /// 首字延迟 EWMA（毫秒）。仅流式请求有值。
    pub ttft_ewma_ms: Option<f64>,
    /// 总耗时 EWMA（毫秒）。
    pub latency_ewma_ms: Option<f64>,
    /// 累计样本数，用于 `quality_min_samples` 门槛判断。
    pub sample_count: u64,
    /// 临时避让到期时间（epoch 毫秒）。
    pub probation_until_ms: Option<i64>,
    /// 连续进入避让的次数，用于指数退避。
    pub probation_level: u32,
    /// 避让到期时间戳（epoch 毫秒），用于恢复爬坡线性衰减。
    pub probation_cleared_at_ms: Option<i64>,
    /// 当前劣化判定窗口内的样本数。
    ///
    /// 该窗口与质量样本 TTL 分离：TTL 决定调度评分数据保留多久，
    /// 窗口只用于判断“失败是否持续发生”，避免很久以前的失败在新一轮
    /// 请求到来时立即触发再次避让。
    pub degrade_window_sample_count: u64,
    /// 当前劣化判定窗口内的失败数。
    pub degrade_window_failure_count: u64,
    /// 当前劣化判定窗口起点（epoch 毫秒）。
    pub degrade_window_started_at_ms: Option<i64>,
}

impl AccountQualityState {
    /// 样本是否足够参与质量评分。
    pub fn has_enough_samples(&self, min_samples: u64) -> bool {
        self.sample_count >= min_samples.max(1)
    }

    /// 当前是否处于临时避让。
    pub fn is_in_probation(&self, now_ms: i64) -> bool {
        self.probation_until_ms
            .is_some_and(|until_ms| until_ms > now_ms)
    }

    /// 恢复爬坡进度：`0.0` 表示刚恢复，`1.0` 表示完全恢复。
    ///
    /// 避让到期后不立刻全量回流，避免高优先级池恢复后瞬间被打满。
    pub fn recovery_progress(&self, now_ms: i64, ramp_ms: i64) -> f64 {
        if ramp_ms <= 0 || self.is_in_probation(now_ms) {
            return 1.0;
        }
        match self.probation_cleared_at_ms {
            Some(cleared_at_ms) => {
                let elapsed = now_ms.saturating_sub(cleared_at_ms);
                if elapsed >= ramp_ms {
                    1.0
                } else {
                    (elapsed as f64 / ramp_ms as f64).clamp(0.0, 1.0)
                }
            }
            None => 1.0,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AccountCoordinatorSnapshot {
    pub capacity: AccountCapacityState,
    pub cooldown_values: Vec<Option<String>>,
    pub cooldown_ttls: Vec<Option<StdDuration>>,
    pub transient_failure_streak: u32,
    pub transient_failure_ttl: Option<StdDuration>,
    /// 被动质量采样状态；Redis 中无记录或解析失败时为 `None`（中性值）。
    pub quality: Option<AccountQualityState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountCoordinatorSnapshotRequest {
    pub pool_id: u64,
    pub cooldown_keys: Vec<String>,
    pub transient_failure_key: String,
    /// 质量状态键；为空表示本次不读取质量数据（例如测试或诊断调用）。
    pub quality_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountLeaseAcquireResult {
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
pub struct AccountLeaseReleaseRequest {
    pub pool_id: u64,
    pub lease_id: String,
    pub pending: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AccountLeaseReleaseResult {
    pub completed: bool,
    pub removed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountCoordinatorGuardState {
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
pub struct AccountCoordinatorGuardError {
    pub state: AccountCoordinatorGuardState,
}

impl std::fmt::Display for AccountCoordinatorGuardError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.state {
            AccountCoordinatorGuardState::Ready { .. } => {
                formatter.write_str("account coordinator guard is ready")
            }
            AccountCoordinatorGuardState::EpochMismatch { .. } => {
                formatter.write_str("account coordinator Redis epoch requires reconciliation")
            }
            AccountCoordinatorGuardState::Recovering { remaining, .. } => write!(
                formatter,
                "account coordinator is recovering for {}ms",
                remaining.as_millis().max(1)
            ),
        }
    }
}

impl std::error::Error for AccountCoordinatorGuardError {}

#[derive(Debug, Clone)]
struct RedisAccountIndexItem {
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
    scheduler_capacity_manager: ConnectionManager,
    quality_sample_manager: ConnectionManager,
    role: RedisStoreRole,
    usage_summary_write_gate: Arc<Semaphore>,
    key_prefix: String,
    usage_cleanup_watermark_micros: Arc<AtomicI64>,
    #[cfg(test)]
    account_hot_path_round_trips: Arc<AtomicU64>,
    #[cfg(test)]
    usage_summary_write_round_trips: Arc<AtomicU64>,
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
const USAGE_DASHBOARD_TOP_ACCOUNTS_KEY: &str = "usage:dashboard:top:accounts";
const USAGE_DASHBOARD_ACCOUNT_LIMIT: isize = 19;
const USAGE_RECORDS_INDEX_KEY: &str = "usage:records:index";
const USAGE_RECORDS_TTL_SECS: usize = 35 * 24 * 60 * 60;
const USAGE_RECORDS_MAX_CACHED: usize = 100_000;
const DEFAULT_USAGE_SUMMARY_WRITE_PERMITS: usize = 1;

const USAGE_CLEANUP_WATERMARK_KEY: &str = "usage:cleanup:soft_delete_cutoff_micros";
const USAGE_DERIVED_CACHE_INVALIDATED_KEY: &str = "usage:cleanup:derived_cache_invalidated";
const USAGE_RECORDS_TRIM_BATCH: usize = 512;
const USAGE_RECORDS_QUERY_SCAN_LIMIT: usize = 5_000;
const USAGE_CACHE_READ_INLINE_BUCKET_LIMIT: usize = 4_096;
const USAGE_CACHE_READ_INLINE_WARN_BUCKET_LIMIT: usize = 1_024;

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

impl RedisStore {
    #[cfg(test)]
    pub async fn account_dispatch_queue_size(&self) -> anyhow::Result<u32> {
        let mut manager = self.scheduler_capacity_manager();
        let count: i64 = redis::cmd("EVAL")
            .arg(DISPATCH_QUEUE_PRUNE_AND_COUNT_SCRIPT)
            .arg(1)
            .arg(self.key(account_global_queue_key()))
            .query_async(&mut manager)
            .await?;
        Ok(count.max(0) as u32)
    }

    #[cfg(test)]
    pub async fn record_account_quality_sample(
        &self,
        pool_id: u64,
        success: bool,
        ttft_ms: Option<u64>,
        latency_ms: Option<u64>,
        alpha: f64,
        ttl: StdDuration,
    ) -> anyhow::Result<AccountQualityState> {
        self.record_account_quality_sample_with_window(
            pool_id,
            success,
            ttft_ms,
            latency_ms,
            AccountQualitySampleTuning {
                alpha,
                ttl,
                degrade_window: StdDuration::ZERO,
            },
        )
        .await
    }

    #[cfg(test)]
    pub fn account_hot_path_round_trips(&self) -> u64 {
        self.account_hot_path_round_trips.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub fn reset_account_hot_path_round_trips(&self) {
        self.account_hot_path_round_trips
            .store(0, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub async fn account_coordinator_snapshot(
        &self,
        pool_id: u64,
        max_age: Option<StdDuration>,
        cooldown_keys: &[String],
        coordination_epoch: &str,
    ) -> anyhow::Result<AccountCoordinatorSnapshot> {
        let mut snapshots = self
            .account_coordinator_snapshots(
                &[AccountCoordinatorSnapshotRequest {
                    pool_id,
                    cooldown_keys: cooldown_keys.to_vec(),
                    transient_failure_key: format!("account:{pool_id}:transient_failures"),
                    // 单池路径只用于测试与诊断；调度热路径统一走批量接口，
                    // 质量数据在那里读取。
                    quality_key: None,
                }],
                max_age,
                coordination_epoch,
            )
            .await?;
        snapshots
            .pop()
            .ok_or_else(|| anyhow::anyhow!("Redis returned no account coordinator snapshot"))
    }

    #[cfg(test)]
    pub fn key_prefix_for_test(&self) -> String {
        self.key_prefix.clone()
    }

    #[cfg(test)]
    pub async fn set_external_release_wrongtype_for_test(
        &self,
        pool_id: u64,
        poisoned: bool,
    ) -> anyhow::Result<()> {
        let key = self.key(&account_in_flight_keys(pool_id).last_seen);
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
    pub async fn release_account_confirmed_lease(
        &self,
        pool_id: u64,
        lease_id: &str,
    ) -> anyhow::Result<bool> {
        let result = self
            .release_account_leases_batch(&[AccountLeaseReleaseRequest {
                pool_id,
                lease_id: lease_id.to_string(),
                pending: false,
            }])
            .await?;
        Ok(result.first().is_some_and(|result| result.removed))
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

    #[cfg(test)]
    pub(crate) async fn ttl_secs_for_test(&self, key: impl AsRef<str>) -> anyhow::Result<i64> {
        let mut manager = self.manager.clone();
        Ok(redis::cmd("TTL")
            .arg(self.key(key))
            .query_async(&mut manager)
            .await?)
    }

    pub async fn del(&self, key: impl AsRef<str>) -> anyhow::Result<()> {
        let mut manager = self.manager.clone();
        let _: () = manager.del(self.key(key)).await?;
        Ok(())
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
        let (scheduler_capacity_manager, quality_sample_manager) = match role {
            RedisStoreRole::Business => (
                client.get_connection_manager().await?,
                client.get_connection_manager().await?,
            ),
            RedisStoreRole::Observability => (manager.clone(), manager.clone()),
        };
        #[cfg(test)]
        let usage_summary_write_permits =
            std::env::var("CLAUDE_POOL_TEST_USAGE_SUMMARY_WRITE_PERMITS")
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                .map(|value| value.clamp(1, 4))
                .unwrap_or(DEFAULT_USAGE_SUMMARY_WRITE_PERMITS);
        #[cfg(not(test))]
        let usage_summary_write_permits = DEFAULT_USAGE_SUMMARY_WRITE_PERMITS;
        Ok(Self {
            client,
            manager,
            scheduler_capacity_manager,
            quality_sample_manager,
            role,
            usage_summary_write_gate: Arc::new(Semaphore::new(usage_summary_write_permits)),
            key_prefix: config.key_prefix.trim_end_matches(':').to_string(),
            usage_cleanup_watermark_micros: Arc::new(AtomicI64::new(0)),
            #[cfg(test)]
            account_hot_path_round_trips: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            usage_summary_write_round_trips: Arc::new(AtomicU64::new(0)),
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

    fn scheduler_capacity_manager(&self) -> ConnectionManager {
        self.scheduler_capacity_manager.clone()
    }

    fn quality_sample_manager(&self) -> ConnectionManager {
        self.quality_sample_manager.clone()
    }

    async fn dashboard_bucket_cache(
        &self,
        window_specs: &[UsageDashboardWindowSpec],
        hourly_specs: &[UsageDashboardWindowSpec],
        daily_specs: &[UsageDashboardWindowSpec],
        account_index: &[RedisAccountIndexItem],
    ) -> anyhow::Result<DashboardBucketCache> {
        let mut suffixes = Vec::new();
        let mut seen = HashSet::new();
        for spec in window_specs {
            collect_dashboard_window_bucket_keys(&mut suffixes, &mut seen, spec, account_index);
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

    fn dashboard_account_billing_by_pool_from_cache(
        &self,
        spec: &UsageDashboardWindowSpec,
        account_index: &[RedisAccountIndexItem],
        cache: &DashboardBucketCache,
    ) -> Vec<UsageAccountBillingByPool> {
        if account_index.is_empty() {
            return Vec::new();
        }

        let mut items = Vec::with_capacity(account_index.len());
        for pool in account_index {
            let totals = cache.sum_bucket("account", &pool.id, spec);
            let requests = usage_usize(&totals, "account_requests");
            if requests == 0 {
                continue;
            }
            items.push(UsageAccountBillingByPool {
                pool_id: pool.id.parse::<u64>().unwrap_or(0),
                pool_name: pool.label.clone(),
                requests,
                priced_requests: usage_usize(&totals, "account_priced_requests"),
                unpriced_requests: usage_usize(&totals, "account_unpriced_requests"),
                cost_floor_applied_requests: usage_usize(
                    &totals,
                    "account_cost_floor_applied_requests",
                ),
                raw_cost_usd: usage_f64(&totals, "account_raw_cost_usd"),
                shaped_cost_usd: usage_f64(&totals, "account_shaped_cost_usd"),
                uplifted_cost_usd: usage_f64(&totals, "account_uplifted_cost_usd"),
                profit_usd: usage_f64(&totals, "account_profit_usd"),
                reported_cost_usd: usage_f64(&totals, "account_reported_cost_usd"),
                billable_cost_usd: usage_f64(&totals, "account_billable_cost_usd"),
                cost_floor_delta_usd: usage_f64(&totals, "account_cost_floor_delta_usd"),
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

    pub fn runtime_config_changed_channel(&self) -> String {
        self.key("events:runtime_config_changed")
    }

    pub fn dispatch_wakeup_channel(&self) -> String {
        self.key("events:dispatch_wakeup")
    }

    pub fn account_data_changed_channel(&self) -> String {
        self.key("events:account_data_changed")
    }

    pub async fn subscribe_runtime_events(&self) -> anyhow::Result<PubSub> {
        let mut pubsub = self.client.get_async_pubsub().await?;
        pubsub
            .subscribe(self.runtime_config_changed_channel())
            .await?;
        pubsub.subscribe(self.dispatch_wakeup_channel()).await?;
        pubsub
            .subscribe(self.account_data_changed_channel())
            .await?;
        Ok(pubsub)
    }

    pub async fn publish_account_data_changed(
        &self,
        reason: &str,
        pool_id: Option<u64>,
    ) -> anyhow::Result<u64> {
        self.publish_account_data_changed_with_origin(reason, pool_id, None)
            .await
    }

    pub async fn publish_account_data_changed_with_origin(
        &self,
        reason: &str,
        pool_id: Option<u64>,
        origin: Option<&str>,
    ) -> anyhow::Result<u64> {
        const SCRIPT: &str = r#"
            local generation = redis.call('INCR', KEYS[1])
            local payload = cjson.encode({
                generation = generation,
                reason = ARGV[1],
                poolId = ARGV[2],
                origin = ARGV[3]
            })
            redis.call('PUBLISH', KEYS[2], payload)
            return generation
        "#;
        let mut manager = self.manager.clone();
        let generation: u64 = redis::cmd("EVAL")
            .arg(SCRIPT)
            .arg(2)
            .arg(self.key(ACCOUNT_DATA_GENERATION_KEY))
            .arg(self.account_data_changed_channel())
            .arg(reason)
            .arg(pool_id.map(|id| id.to_string()).unwrap_or_default())
            .arg(origin.unwrap_or_default())
            .query_async(&mut manager)
            .await?;
        Ok(generation)
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

    /// Reads one float field of a hash; `None` when the hash or field is absent.
    pub async fn hash_get_f64(
        &self,
        key: impl AsRef<str>,
        field: &str,
    ) -> anyhow::Result<Option<f64>> {
        let mut manager = self.manager.clone();
        let value: Option<String> = redis::cmd("HGET")
            .arg(self.key(key))
            .arg(field)
            .query_async(&mut manager)
            .await?;
        Ok(value.and_then(|value| value.parse::<f64>().ok()))
    }

    /// Adds to float fields of a hash and marks it with `marker`, atomically; sets the expiry on
    /// first write.
    pub async fn hash_incr_by_float(
        &self,
        key: impl AsRef<str>,
        marker: &str,
        fields: &[(&str, f64)],
        ttl_secs: Option<u64>,
    ) -> anyhow::Result<()> {
        let script = r#"
            for i = 1, #ARGV - 1, 2 do
                redis.call('HINCRBYFLOAT', KEYS[1], ARGV[i], ARGV[i + 1])
            end
            redis.call('HSETNX', KEYS[1], KEYS[2], 1)
            local ttl = tonumber(KEYS[3])
            if ttl > 0 and redis.call('TTL', KEYS[1]) < 0 then
                redis.call('EXPIRE', KEYS[1], ttl)
            end
            return 1
        "#;
        let mut command = redis::cmd("EVAL");
        command
            .arg(script)
            .arg(3)
            .arg(self.key(key))
            .arg(marker)
            .arg(ttl_secs.unwrap_or(0));
        for (field, value) in fields {
            command.arg(*field).arg(value.to_string());
        }
        let mut manager = self.manager.clone();
        let _: i64 = command.query_async(&mut manager).await?;
        Ok(())
    }

    /// Writes `fields` and `marker` into a hash only if the hash does not exist yet; an existing
    /// hash is left untouched. Returns whether it wrote.
    pub async fn hash_init_if_absent(
        &self,
        key: impl AsRef<str>,
        marker: &str,
        fields: &[(&str, f64)],
        ttl_secs: Option<u64>,
    ) -> anyhow::Result<bool> {
        let script = r#"
            if redis.call('EXISTS', KEYS[1]) == 1 then
                return 0
            end
            for i = 1, #ARGV - 1, 2 do
                redis.call('HSET', KEYS[1], ARGV[i], ARGV[i + 1])
            end
            redis.call('HSET', KEYS[1], KEYS[2], 1)
            local ttl = tonumber(KEYS[3])
            if ttl > 0 then
                redis.call('EXPIRE', KEYS[1], ttl)
            end
            return 1
        "#;
        let mut command = redis::cmd("EVAL");
        command
            .arg(script)
            .arg(3)
            .arg(self.key(key))
            .arg(marker)
            .arg(ttl_secs.unwrap_or(0));
        for (field, value) in fields {
            command.arg(*field).arg(value.to_string());
        }
        let mut manager = self.manager.clone();
        let written: i64 = command.query_async(&mut manager).await?;
        Ok(written == 1)
    }

    /// Replaces a hash with `fields` plus `marker` and sets its expiry.
    pub async fn hash_replace(
        &self,
        key: impl AsRef<str>,
        marker: &str,
        fields: &[(&str, f64)],
        ttl_secs: Option<u64>,
    ) -> anyhow::Result<()> {
        let script = r#"
            redis.call('DEL', KEYS[1])
            for i = 1, #ARGV - 1, 2 do
                redis.call('HSET', KEYS[1], ARGV[i], ARGV[i + 1])
            end
            redis.call('HSET', KEYS[1], KEYS[2], 1)
            local ttl = tonumber(KEYS[3])
            if ttl > 0 then
                redis.call('EXPIRE', KEYS[1], ttl)
            end
            return 1
        "#;
        let mut command = redis::cmd("EVAL");
        command
            .arg(script)
            .arg(3)
            .arg(self.key(key))
            .arg(marker)
            .arg(ttl_secs.unwrap_or(0));
        for (field, value) in fields {
            command.arg(*field).arg(value.to_string());
        }
        let mut manager = self.manager.clone();
        let _: i64 = command.query_async(&mut manager).await?;
        Ok(())
    }

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
    pub async fn claim_account_auto_disable_transition(
        &self,
        pool_id: u64,
        pool_revision: u64,
        reason: &str,
        threshold: u64,
        window_secs: usize,
        claim_ttl_secs: usize,
    ) -> anyhow::Result<(u64, bool)> {
        let counter_key = format!("account:{}:auto_disable_failures:{}", pool_id, reason);
        let claim_key = format!(
            "account:{}:auto_disable_transition:{}:{}",
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
            .ok_or_else(|| anyhow::anyhow!("Redis 未返回账号自动禁用失败计数"))?
            .max(0) as u64;
        let claimed = result
            .get(1)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("Redis 未返回账号自动禁用 transition claim"))?
            != 0;
        Ok((count, claimed))
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
            .arg(if record.status == UsageRecordStatus::Success {
                "success_requests"
            } else {
                "error_requests"
            })
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

        append_account_usage_summary(&mut pipe, &totals_key, record);
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
            &self.key(USAGE_DASHBOARD_TOP_ACCOUNTS_KEY),
            "account",
            record.account_id.map(|id| id.to_string()),
            record.account_name.clone(),
            record,
            |key| self.key(usage_dashboard_top_metrics_key("account", key)),
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
            account_billing: UsageAccountBillingSummary {
                requests: usage_usize(&totals, "account_requests"),
                priced_requests: usage_usize(&totals, "account_priced_requests"),
                unpriced_requests: usage_usize(&totals, "account_unpriced_requests"),
                cost_floor_applied_requests: usage_usize(
                    &totals,
                    "account_cost_floor_applied_requests",
                ),
                raw_cost_usd: usage_f64(&totals, "account_raw_cost_usd"),
                shaped_cost_usd: usage_f64(&totals, "account_shaped_cost_usd"),
                uplifted_cost_usd: usage_f64(&totals, "account_uplifted_cost_usd"),
                profit_usd: usage_f64(&totals, "account_profit_usd"),
                reported_cost_usd: usage_f64(&totals, "account_reported_cost_usd"),
                billable_cost_usd: usage_f64(&totals, "account_billable_cost_usd"),
                cost_floor_delta_usd: usage_f64(&totals, "account_cost_floor_delta_usd"),
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
        let account_index = self.dashboard_account_index().await?;
        let bucket_cache = self
            .dashboard_bucket_cache(&window_specs, &hourly_specs, &daily_specs, &account_index)
            .await?;
        let mut windows = Vec::with_capacity(window_specs.len());
        for spec in &window_specs {
            windows.push(self.dashboard_window_from_cache(
                spec,
                high_cache_threshold,
                &account_index,
                &bucket_cache,
            ));
        }

        let has_window_data = windows
            .iter()
            .any(|window| window.summary.total_requests > 0);
        if lifetime_requests > 0 && !has_window_data {
            return Ok(None);
        }

        let (models, models_total) = self
            .dashboard_top_aggregates(USAGE_DASHBOARD_TOP_MODELS_KEY, "model")
            .await?;
        let (credentials, credentials_total) = self
            .dashboard_top_aggregates(USAGE_DASHBOARD_TOP_CREDENTIALS_KEY, "credential")
            .await?;
        let (endpoints, endpoints_total) = self
            .dashboard_top_aggregates(USAGE_DASHBOARD_TOP_ENDPOINTS_KEY, "endpoint")
            .await?;
        let (errors, errors_total) = self
            .dashboard_top_aggregates(USAGE_DASHBOARD_TOP_ERRORS_KEY, "error")
            .await?;
        let mut top = UsageDashboardTop::empty("lifetime");
        top.models = models;
        top.credentials = credentials;
        top.endpoints = endpoints;
        top.errors = errors;
        top.models_total = models_total;
        top.credentials_total = credentials_total;
        top.endpoints_total = endpoints_total;
        top.errors_total = errors_total;
        top.models_truncated = models_total > top.models.len();
        top.credentials_truncated = credentials_total > top.credentials.len();
        top.endpoints_truncated = endpoints_total > top.endpoints.len();
        top.errors_truncated = errors_total > top.errors.len();

        let elapsed_ms = started_at.elapsed().as_millis();
        if elapsed_ms >= 250 {
            tracing::warn!(
                elapsed_ms = elapsed_ms.min(u128::from(u64::MAX)) as u64,
                account_count = account_index.len(),
                "Redis usage dashboard read was slow"
            );
        } else {
            tracing::debug!(
                elapsed_ms = elapsed_ms.min(u128::from(u64::MAX)) as u64,
                account_count = account_index.len(),
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

    pub async fn usage_dashboard_windows_only(
        &self,
        timezone: Option<&str>,
        high_cache_threshold: i32,
    ) -> anyhow::Result<Option<(String, String, Vec<UsageDashboardWindow>)>> {
        self.ensure_observability_usage_store("read usage dashboard windows")?;
        let Some(totals) = self.usage_totals_if_valid().await? else {
            return Ok(None);
        };
        let lifetime_requests = usage_usize(&totals, "total_requests");

        let now = Utc::now();
        let (timezone, offset) = usage_dashboard_timezone(timezone);
        let window_specs = usage_dashboard_windows(now, offset);
        let account_index = self.dashboard_account_index().await?;
        let bucket_cache = self
            .dashboard_bucket_cache(&window_specs, &[], &[], &account_index)
            .await?;
        let mut windows = Vec::with_capacity(window_specs.len());
        for spec in &window_specs {
            windows.push(self.dashboard_window_from_cache(
                spec,
                high_cache_threshold,
                &account_index,
                &bucket_cache,
            ));
        }

        let has_window_data = windows
            .iter()
            .any(|window| window.summary.total_requests > 0);
        if lifetime_requests > 0 && !has_window_data {
            return Ok(None);
        }

        Ok(Some((now.to_rfc3339(), timezone, windows)))
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
        Ok(Some((now.to_rfc3339(), {
            let mut top = UsageDashboardTop::empty("lifetime");
            let (models, models_total) = self
                .dashboard_top_aggregates(USAGE_DASHBOARD_TOP_MODELS_KEY, "model")
                .await?;
            let (credentials, credentials_total) = self
                .dashboard_top_aggregates(USAGE_DASHBOARD_TOP_CREDENTIALS_KEY, "credential")
                .await?;
            let (endpoints, endpoints_total) = self
                .dashboard_top_aggregates(USAGE_DASHBOARD_TOP_ENDPOINTS_KEY, "endpoint")
                .await?;
            let (errors, errors_total) = self
                .dashboard_top_aggregates(USAGE_DASHBOARD_TOP_ERRORS_KEY, "error")
                .await?;
            top.models = models;
            top.credentials = credentials;
            top.endpoints = endpoints;
            top.errors = errors;
            top.models_total = models_total;
            top.credentials_total = credentials_total;
            top.endpoints_total = endpoints_total;
            top.errors_total = errors_total;
            top.models_truncated = models_total > top.models.len();
            top.credentials_truncated = credentials_total > top.credentials.len();
            top.endpoints_truncated = endpoints_total > top.endpoints.len();
            top.errors_truncated = errors_total > top.errors.len();
            top
        })))
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
        let mut success_requests = 0usize;
        let mut error_requests = 0usize;
        let mut input_tokens = 0i64;
        let mut output_tokens = 0i64;
        let mut billable_input_tokens = 0i64;
        for bucket in buckets {
            requests += usage_usize(&bucket, "requests");
            success_requests += usage_usize(&bucket, "success_requests");
            error_requests += usage_usize(&bucket, "error_requests");
            input_tokens += usage_i64(&bucket, "input_tokens");
            output_tokens += usage_i64(&bucket, "output_tokens");
            billable_input_tokens += usage_i64(&bucket, "billable_input_tokens");
        }
        Ok(UsageRealtimeStats::from_totals_with_status(
            REALTIME_USAGE_WINDOW_SECS,
            requests,
            success_requests,
            error_requests,
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

    fn dashboard_window_from_cache(
        &self,
        spec: &UsageDashboardWindowSpec,
        high_cache_threshold: i32,
        account_index: &[RedisAccountIndexItem],
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
        summary.account_billing_by_pool =
            self.dashboard_account_billing_by_pool_from_cache(spec, account_index, cache);

        UsageDashboardWindow {
            key: spec.key.clone(),
            label: spec.label.clone(),
            from: spec.from.to_rfc3339(),
            to: spec.to.to_rfc3339(),
            summary,
        }
    }

    async fn dashboard_account_index(&self) -> anyhow::Result<Vec<RedisAccountIndexItem>> {
        let mut manager = self.manager.clone();
        let pool_ids: Vec<String> = manager
            .zrevrange(
                self.key(USAGE_DASHBOARD_TOP_ACCOUNTS_KEY),
                0,
                USAGE_DASHBOARD_ACCOUNT_LIMIT,
            )
            .await?;
        if pool_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut pipe = redis::pipe();
        for pool_id in pool_ids {
            pipe.cmd("HGETALL")
                .arg(self.key(usage_dashboard_top_metrics_key("account", &pool_id)));
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
                RedisAccountIndexItem { id, label }
            })
            .filter(|item| !item.id.trim().is_empty())
            .collect())
    }

    async fn dashboard_top_aggregates(
        &self,
        index_key: &str,
        dimension: &str,
    ) -> anyhow::Result<(Vec<UsageTopAggregate>, usize)> {
        let mut manager = self.manager.clone();
        let total: usize = manager.zcard(self.key(index_key)).await?;
        let keys: Vec<String> = manager.zrevrange(self.key(index_key), 0, 9).await?;
        if keys.is_empty() {
            return Ok((Vec::new(), total));
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
        Ok((items, total))
    }

    /// Account coordination uses atomic multi-key Lua scripts and is
    /// intentionally supported only on standalone Redis. Redis Cluster would
    /// require a shared hash tag and a cluster-aware client, neither of which
    /// is part of the current storage contract.
    pub async fn account_redis_run_id(&self) -> anyhow::Result<String> {
        let mut manager = self.scheduler_capacity_manager();
        #[cfg(test)]
        self.account_hot_path_round_trips
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
                "account coordination supports standalone Redis only; Redis Cluster is not supported"
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

    pub async fn account_coordinator_guard_state_for_epoch(
        &self,
        expected_epoch: &str,
    ) -> anyhow::Result<AccountCoordinatorGuardState> {
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
        self.account_hot_path_round_trips
            .fetch_add(1, Ordering::Relaxed);
        let values: Vec<redis::Value> = redis::cmd("EVAL")
            .arg(script)
            .arg(2)
            .arg(self.key(ACCOUNT_COORDINATOR_EPOCH_KEY))
            .arg(self.key(ACCOUNT_COORDINATOR_RECOVERY_KEY))
            .arg(expected_epoch)
            .query_async(&mut manager)
            .await?;
        decode_account_coordinator_guard_state(&values)
    }

    pub async fn install_account_coordinator_guard(
        &self,
        coordination_epoch: &str,
        recovery_grace: StdDuration,
    ) -> anyhow::Result<AccountCoordinatorGuardState> {
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
        self.account_hot_path_round_trips
            .fetch_add(1, Ordering::Relaxed);
        let values: Vec<redis::Value> = redis::cmd("EVAL")
            .arg(script)
            .arg(2)
            .arg(self.key(ACCOUNT_COORDINATOR_EPOCH_KEY))
            .arg(self.key(ACCOUNT_COORDINATOR_RECOVERY_KEY))
            .arg(coordination_epoch)
            .arg(recovery_grace_ms)
            .query_async(&mut manager)
            .await?;
        decode_account_coordinator_guard_state(&values)
    }

    pub async fn acquire_account_lease(
        &self,
        pool_id: u64,
        lease_id: &str,
        coordination_epoch: &str,
        max_concurrent_requests: u32,
        global_max_concurrent_requests: u32,
        max_age: Option<StdDuration>,
        cooldown_keys: &[String],
    ) -> anyhow::Result<AccountLeaseAcquireResult> {
        if cooldown_keys.is_empty() {
            anyhow::bail!("account lease acquire requires the pool cooldown key");
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
        let keys = account_in_flight_keys(pool_id);
        let global_keys = account_global_in_flight_keys();
        let mut manager = self.scheduler_capacity_manager();
        #[cfg(test)]
        self.account_hot_path_round_trips
            .fetch_add(1, Ordering::Relaxed);
        let mut command = redis::cmd("EVAL");
        command
            .arg(script)
            .arg(7usize.saturating_add(cooldown_keys.len()))
            .arg(self.key(ACCOUNT_COORDINATOR_EPOCH_KEY))
            .arg(self.key(ACCOUNT_COORDINATOR_RECOVERY_KEY))
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
            1 => Ok(AccountLeaseAcquireResult::Acquired {
                lease_id: lease_id.to_string(),
                pool_in_flight_requests,
                global_in_flight_requests,
            }),
            2 => {
                let remaining_ms = redis_value_i64(&result, 1)?.unwrap_or(-1);
                let remaining = (remaining_ms >= 0)
                    .then(|| StdDuration::from_millis(remaining_ms.max(1) as u64));
                if redis_value_i64(&result, 4)?.unwrap_or(1) == 1 {
                    Ok(AccountLeaseAcquireResult::PoolCooldown { remaining })
                } else {
                    Ok(AccountLeaseAcquireResult::ModelCooldown { remaining })
                }
            }
            3 => Ok(AccountLeaseAcquireResult::PoolCapacityFull {
                in_flight_requests: pool_in_flight_requests,
            }),
            4 => Ok(AccountLeaseAcquireResult::GlobalCapacityFull {
                in_flight_requests: global_in_flight_requests,
            }),
            5 => Ok(AccountLeaseAcquireResult::Released),
            6 => Ok(AccountLeaseAcquireResult::CoordinatorEpochMismatch {
                coordination_epoch: redis_value_string(&result, 4)?.unwrap_or_default(),
            }),
            7 => Ok(AccountLeaseAcquireResult::CoordinatorRecovering {
                coordination_epoch: redis_value_string(&result, 4)?.unwrap_or_default(),
                remaining: StdDuration::from_millis(
                    redis_value_i64(&result, 1)?.unwrap_or(1).max(1) as u64,
                ),
            }),
            _ => anyhow::bail!("Redis returned an invalid account lease acquire status"),
        }
    }

    pub async fn release_account_leases_batch(
        &self,
        requests: &[AccountLeaseReleaseRequest],
    ) -> anyhow::Result<Vec<AccountLeaseReleaseResult>> {
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
        let global_keys = account_global_in_flight_keys();
        let mut command = redis::cmd("EVAL");
        command.arg(script).arg(requests.len().saturating_mul(5));
        for request in requests {
            let keys = account_in_flight_keys(request.pool_id);
            command
                .arg(self.key(&keys.last_seen))
                .arg(self.key(&keys.acquired))
                .arg(self.key(&global_keys.last_seen))
                .arg(self.key(&global_keys.acquired))
                .arg(self.key(&keys.released));
        }
        command
            .arg(requests.len())
            .arg(ACCOUNT_PENDING_LEASE_TOMBSTONE_TTL_MILLIS);
        for request in requests {
            command
                .arg(if request.pending { 1 } else { 0 })
                .arg(&request.lease_id);
        }
        let mut manager = self.scheduler_capacity_manager();
        let values: Vec<i64> = command.query_async(&mut manager).await?;
        if values.len() != requests.len().saturating_mul(2) {
            anyhow::bail!("Redis returned an incomplete account lease release batch");
        }
        Ok(values
            .chunks_exact(2)
            .map(|values| AccountLeaseReleaseResult {
                completed: values[0] == 1,
                removed: values[1] > 0,
            })
            .collect())
    }

    pub async fn touch_account_lease(
        &self,
        pool_id: u64,
        lease_id: &str,
        ttl_secs: usize,
        coordination_epoch: &str,
    ) -> anyhow::Result<bool> {
        let keys = account_in_flight_keys(pool_id);
        let global_keys = account_global_in_flight_keys();
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
            .arg(self.key(ACCOUNT_COORDINATOR_EPOCH_KEY))
            .arg(self.key(ACCOUNT_COORDINATOR_RECOVERY_KEY))
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

    pub async fn account_coordinator_snapshots(
        &self,
        requests: &[AccountCoordinatorSnapshotRequest],
        max_age: Option<StdDuration>,
        coordination_epoch: &str,
    ) -> anyhow::Result<Vec<AccountCoordinatorSnapshot>> {
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
                        table.insert(result, '__claude_pool_invalid_redis_type__')
                    else
                        table.insert(result, cooldown_value or '')
                    end
                    table.insert(result, redis.call('PTTL', KEYS[key_index]))
                    key_index = key_index + 1
                end
                local transient_value = redis.pcall('GET', KEYS[key_index])
                if type(transient_value) == 'table' and transient_value.err then
                    table.insert(result, '__claude_pool_invalid_redis_type__')
                else
                    table.insert(result, transient_value or '')
                end
                table.insert(result, redis.call('PTTL', KEYS[key_index]))
                key_index = key_index + 1
                -- 质量状态与并发/冷却在同一次往返中读取。
                -- 账号选择运行态快照 TTL 只有 100ms，是真正的热路径，
                -- 不允许为质量数据新增第二次 Redis 往返。
                local quality_requested = tonumber(ARGV[3 + pool_count + pool_index])
                if quality_requested == 1 then
                    local quality_value = redis.pcall('GET', KEYS[key_index])
                    if type(quality_value) == 'table' and quality_value.err then
                        table.insert(result, '__claude_pool_invalid_redis_type__')
                    else
                        table.insert(result, quality_value or '')
                    end
                    key_index = key_index + 1
                else
                    table.insert(result, '')
                end
            end
            return result
        "#;
        let global_keys = account_global_in_flight_keys();
        let mut manager = self.scheduler_capacity_manager();
        let redis_key_count = requests.iter().fold(4usize, |count, request| {
            count.saturating_add(
                3usize
                    .saturating_add(request.cooldown_keys.len())
                    .saturating_add(usize::from(request.quality_key.is_some())),
            )
        });
        let mut command = redis::cmd("EVAL");
        command
            .arg(script)
            .arg(redis_key_count)
            .arg(self.key(ACCOUNT_COORDINATOR_EPOCH_KEY))
            .arg(self.key(ACCOUNT_COORDINATOR_RECOVERY_KEY))
            .arg(self.key(&global_keys.last_seen))
            .arg(self.key(&global_keys.acquired));
        for request in requests {
            let keys = account_in_flight_keys(request.pool_id);
            command
                .arg(self.key(&keys.last_seen))
                .arg(self.key(&keys.acquired));
            for key in &request.cooldown_keys {
                command.arg(self.key(key));
            }
            command.arg(self.key(&request.transient_failure_key));
            if let Some(quality_key) = &request.quality_key {
                command.arg(self.key(quality_key));
            }
        }
        command.arg(max_age_ms).arg(requests.len());
        for request in requests {
            command.arg(request.cooldown_keys.len());
        }
        command.arg(coordination_epoch);
        for request in requests {
            command.arg(i64::from(request.quality_key.is_some()));
        }
        #[cfg(test)]
        self.account_hot_path_round_trips
            .fetch_add(1, Ordering::Relaxed);
        let result: Vec<redis::Value> = command.query_async(&mut manager).await?;
        let guard_state = decode_account_coordinator_guard_state(&result)?;
        match guard_state {
            AccountCoordinatorGuardState::Ready { .. } => {}
            state => {
                return Err(anyhow::Error::new(AccountCoordinatorGuardError { state }));
            }
        }
        // 每个池固定返回：pool_in_flight、global_in_flight、每个 cooldown 的
        // (value, ttl)、transient (value, ttl)，再加一个质量槽位。质量槽位
        // 无论是否请求都会占位（未请求时为空串），保证游标算术恒定。
        let expected_values = requests.iter().fold(3usize, |count, request| {
            count.saturating_add(
                5usize.saturating_add(request.cooldown_keys.len().saturating_mul(2)),
            )
        });
        if result.len() != expected_values {
            anyhow::bail!("Redis returned an incomplete account coordinator snapshot batch");
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
            let transient_value = redis::from_redis_value::<String>(&result[cursor])?;
            cursor += 1;
            let transient_ttl_ms = redis::from_redis_value::<i64>(&result[cursor])?;
            cursor += 1;
            let transient_failure_streak = transient_value
                .parse::<u64>()
                .ok()
                .filter(|_| transient_ttl_ms >= 0)
                .unwrap_or_default()
                .min(u64::from(u32::MAX)) as u32;
            let transient_failure_ttl = (transient_ttl_ms >= 0)
                .then(|| StdDuration::from_millis(transient_ttl_ms.max(1) as u64));
            let quality_value = redis::from_redis_value::<String>(&result[cursor])?;
            cursor += 1;
            // 质量数据损坏或缺失时静默退化为 None（中性值），
            // 绝不能因为质量数据解析失败而让整个调度快照报错。
            let quality = (!quality_value.is_empty()
                && quality_value != INVALID_REDIS_TYPE_SENTINEL)
                .then(|| decode_account_quality_state(&quality_value))
                .flatten();
            snapshots.push(AccountCoordinatorSnapshot {
                capacity: AccountCapacityState {
                    pool_in_flight_requests,
                    global_in_flight_requests,
                },
                cooldown_values,
                cooldown_ttls,
                transient_failure_streak,
                transient_failure_ttl,
                quality,
            });
        }
        Ok(snapshots)
    }

    /// 记录质量样本，并维护独立的劣化判定窗口。
    ///
    /// 保留不带窗口参数的 `record_account_quality_sample` 作为兼容入口，
    /// 让现有诊断/测试调用仍保持原有语义；调度主路径传入配置的窗口时长。
    pub async fn record_account_quality_sample_with_window(
        &self,
        pool_id: u64,
        success: bool,
        ttft_ms: Option<u64>,
        latency_ms: Option<u64>,
        tuning: AccountQualitySampleTuning,
    ) -> anyhow::Result<AccountQualityState> {
        let AccountQualitySampleTuning {
            alpha,
            ttl,
            degrade_window,
        } = tuning;
        let script = r#"
            local raw = redis.pcall('GET', KEYS[1])
            if type(raw) == 'table' and raw.err then
                raw = nil
            end
            local state = {}
            if raw then
                local ok, decoded = pcall(cjson.decode, raw)
                if ok and type(decoded) == 'table' then
                    state = decoded
                end
            end
            local success = tonumber(ARGV[1])
            local ttft_ms = tonumber(ARGV[2])
            local latency_ms = tonumber(ARGV[3])
            local alpha = tonumber(ARGV[4])
            local ttl_ms = tonumber(ARGV[5])
            local degrade_window_ms = tonumber(ARGV[6]) or 0
            -- 采样发生在避让期内时，不得用较短的样本 TTL 抹掉
            -- `mark_account_probation` 写入的避让/恢复状态。
            local existing_ttl_ms = redis.call('PTTL', KEYS[1])
            local redis_time = redis.call('TIME')
            local now_ms = tonumber(redis_time[1]) * 1000
                + math.floor(tonumber(redis_time[2]) / 1000)

            local error_rate = tonumber(state['recent_error_rate']) or 0.0
            local sample = 0.0
            if success == 0 then
                sample = 1.0
            end
            error_rate = error_rate + alpha * (sample - error_rate)
            if error_rate < 0 then error_rate = 0 end
            if error_rate > 1 then error_rate = 1 end
            state['recent_error_rate'] = error_rate

            -- 延迟只在成功样本上累计。失败请求的耗时通常是超时或立即拒绝，
            -- 把它混进延迟曲线会同时污染"快"和"慢"两个方向的判断。
            if success == 1 then
                if ttft_ms >= 0 then
                    local prev = tonumber(state['ttft_ewma_ms'])
                    if prev then
                        state['ttft_ewma_ms'] = prev + alpha * (ttft_ms - prev)
                    else
                        state['ttft_ewma_ms'] = ttft_ms
                    end
                end
                if latency_ms >= 0 then
                    local prev = tonumber(state['latency_ewma_ms'])
                    if prev then
                        state['latency_ewma_ms'] = prev + alpha * (latency_ms - prev)
                    else
                        state['latency_ewma_ms'] = latency_ms
                    end
                end
            end

            state['sample_count'] = (tonumber(state['sample_count']) or 0) + 1
            local window_sample_count = tonumber(state['degrade_window_sample_count']) or 0
            local window_failure_count = tonumber(state['degrade_window_failure_count']) or 0
            local window_started_at_ms = tonumber(state['degrade_window_started_at_ms'])
            -- 使用固定窗口而不是永久累加：持续有流量时也必须淘汰旧失败
            -- 证据，否则一次早期故障会在数小时后仍触发避让。
            if degrade_window_ms > 0 and window_started_at_ms
                and now_ms >= window_started_at_ms + degrade_window_ms then
                window_sample_count = 0
                window_failure_count = 0
                window_started_at_ms = now_ms
            elseif degrade_window_ms > 0 and not window_started_at_ms then
                window_started_at_ms = now_ms
            end
            window_sample_count = window_sample_count + 1
            if success == 0 then
                window_failure_count = window_failure_count + 1
            end
            state['degrade_window_sample_count'] = window_sample_count
            state['degrade_window_failure_count'] = window_failure_count
            state['degrade_window_started_at_ms'] = window_started_at_ms
            local write_ttl_ms = ttl_ms
            if existing_ttl_ms > write_ttl_ms then
                write_ttl_ms = existing_ttl_ms
            end
            redis.call('SET', KEYS[1], cjson.encode(state), 'PX', write_ttl_ms)
            return cjson.encode(state)
        "#;
        // Quality sampling is deliberately isolated from the scheduler capacity
        // connection. A burst of detached EWMA writes must not queue behind
        // lease/snapshot commands on the dispatch hot path.
        let mut manager = self.quality_sample_manager();
        let encoded: String = redis::cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(self.key(account_quality_key(pool_id)))
            .arg(i64::from(success))
            .arg(ttft_ms.map(|value| value as i64).unwrap_or(-1))
            .arg(latency_ms.map(|value| value as i64).unwrap_or(-1))
            .arg(alpha.clamp(0.01, 1.0))
            .arg(ttl.as_millis().max(1) as i64)
            .arg(degrade_window.as_millis() as i64)
            .query_async(&mut manager)
            .await?;
        Ok(decode_account_quality_state(&encoded).unwrap_or_default())
    }

    /// 写入账号临时避让状态，并在已完成恢复爬坡时显式重置退避层级。
    pub async fn mark_account_probation_with_reset(
        &self,
        pool_id: u64,
        probation_until_ms: i64,
        probation_level: u32,
        ttl: StdDuration,
        reset_level: bool,
    ) -> anyhow::Result<()> {
        let script = r#"
            local raw = redis.pcall('GET', KEYS[1])
            if type(raw) == 'table' and raw.err then
                raw = nil
            end
            local state = {}
            if raw then
                local ok, decoded = pcall(cjson.decode, raw)
                if ok and type(decoded) == 'table' then
                    state = decoded
                end
            end
            local until_ms = tonumber(ARGV[1])
            local level = tonumber(ARGV[2])
            local reset_level = tonumber(ARGV[4]) or 0
            local existing = tonumber(state['probation_until_ms']) or 0
            local existing_level = tonumber(state['probation_level']) or 0
            if until_ms > existing then
                state['probation_until_ms'] = until_ms
                state['probation_cleared_at_ms'] = until_ms
                if reset_level == 1 then
                    state['probation_level'] = level
                    existing_level = level
                end
            end
            if level > existing_level then
                state['probation_level'] = level
            end
            redis.call('SET', KEYS[1], cjson.encode(state), 'PX', tonumber(ARGV[3]))
            return 1
        "#;
        let mut manager = self.quality_sample_manager();
        let _: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(self.key(account_quality_key(pool_id)))
            .arg(probation_until_ms)
            .arg(i64::from(probation_level))
            .arg(ttl.as_millis().max(1) as i64)
            .arg(i64::from(reset_level))
            .query_async(&mut manager)
            .await?;
        Ok(())
    }

    pub async fn try_enter_account_dispatch_queue(
        &self,
        lease_id: &str,
        max_queued: u32,
        ttl_secs: u64,
    ) -> anyhow::Result<bool> {
        let mut manager = self.scheduler_capacity_manager();
        let admitted: i64 = redis::cmd("EVAL")
            .arg(DISPATCH_QUEUE_ADMIT_SCRIPT)
            .arg(1)
            .arg(self.key(account_global_queue_key()))
            .arg(max_queued)
            .arg(ttl_secs.max(60))
            .arg(lease_id)
            .query_async(&mut manager)
            .await?;
        Ok(admitted == 1)
    }

    pub async fn renew_account_dispatch_queue(
        &self,
        lease_id: &str,
        ttl_secs: u64,
    ) -> anyhow::Result<bool> {
        let mut manager = self.scheduler_capacity_manager();
        let renewed: i64 = redis::cmd("EVAL")
            .arg(DISPATCH_QUEUE_RENEW_SCRIPT)
            .arg(1)
            .arg(self.key(account_global_queue_key()))
            .arg(ttl_secs.max(60))
            .arg(lease_id)
            .query_async(&mut manager)
            .await?;
        Ok(renewed == 1)
    }

    pub async fn leave_account_dispatch_queue(&self, lease_id: &str) -> anyhow::Result<bool> {
        let mut manager = self.scheduler_capacity_manager();
        let removed: i64 = redis::cmd("EVAL")
            .arg(DISPATCH_QUEUE_RELEASE_SCRIPT)
            .arg(1)
            .arg(self.key(account_global_queue_key()))
            .arg(lease_id)
            .query_async(&mut manager)
            .await?;
        Ok(removed > 0)
    }
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
    if let Some(account_id) = record.account_id {
        append_usage_dashboard_bucket_aggregate(
            pipe,
            &store.key(usage_dashboard_bucket_key(
                "account",
                &account_id.to_string(),
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

    append_account_usage_summary(pipe, key, record);

    pipe.cmd("__USAGE_DURATION_MAX__")
        .arg(key)
        .arg(record.duration_ms.min(i64::MAX as u64) as i64)
        .arg(USAGE_DASHBOARD_BUCKET_TTL_SECS);
}

fn append_account_usage_summary(
    pipe: &mut GuardedUsagePipeline,
    totals_key: &str,
    record: &UsageRecord,
) {
    if record.route_kind != Some(UsageRouteKind::Account) {
        return;
    }

    pipe.cmd("HINCRBY")
        .arg(totals_key)
        .arg("account_requests")
        .arg(1i64);
    if let Some(billing) = &record.account_billing {
        pipe.cmd("HINCRBY")
            .arg(totals_key)
            .arg(if billing.pricing_available {
                "account_priced_requests"
            } else {
                "account_unpriced_requests"
            })
            .arg(1i64)
            .cmd("HINCRBY")
            .arg(totals_key)
            .arg("account_cost_floor_applied_requests")
            .arg(if billing.cost_floor_applied {
                1i64
            } else {
                0i64
            })
            .cmd("HINCRBYFLOAT")
            .arg(totals_key)
            .arg("account_raw_cost_usd")
            .arg(billing.raw_cost_usd)
            .cmd("HINCRBYFLOAT")
            .arg(totals_key)
            .arg("account_shaped_cost_usd")
            .arg(billing.effective_shaped_cost_usd())
            .cmd("HINCRBYFLOAT")
            .arg(totals_key)
            .arg("account_uplifted_cost_usd")
            .arg(billing.effective_uplifted_cost_usd())
            .cmd("HINCRBYFLOAT")
            .arg(totals_key)
            .arg("account_profit_usd")
            .arg(billing.effective_profit_usd())
            .cmd("HINCRBYFLOAT")
            .arg(totals_key)
            .arg("account_reported_cost_usd")
            .arg(billing.reported_cost_usd)
            .cmd("HINCRBYFLOAT")
            .arg(totals_key)
            .arg("account_billable_cost_usd")
            .arg(billing.billable_cost_usd)
            .cmd("HINCRBYFLOAT")
            .arg(totals_key)
            .arg("account_cost_floor_delta_usd")
            .arg(billing.cost_floor_delta_usd);
    } else {
        pipe.cmd("HINCRBY")
            .arg(totals_key)
            .arg("account_unpriced_requests")
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

        simulated_requests: usage_usize(values, "simulated_requests"),
        upstream_metadata_requests: usage_usize(values, "upstream_metadata_requests"),
        account_billing: UsageAccountBillingSummary {
            requests: usage_usize(values, "account_requests"),
            priced_requests: usage_usize(values, "account_priced_requests"),
            unpriced_requests: usage_usize(values, "account_unpriced_requests"),
            cost_floor_applied_requests: usage_usize(values, "account_cost_floor_applied_requests"),
            raw_cost_usd: usage_f64(values, "account_raw_cost_usd"),
            shaped_cost_usd: usage_f64(values, "account_shaped_cost_usd"),
            uplifted_cost_usd: usage_f64(values, "account_uplifted_cost_usd"),
            profit_usd: usage_f64(values, "account_profit_usd"),
            reported_cost_usd: usage_f64(values, "account_reported_cost_usd"),
            billable_cost_usd: usage_f64(values, "account_billable_cost_usd"),
            cost_floor_delta_usd: usage_f64(values, "account_cost_floor_delta_usd"),
        },
        account_billing_by_pool: Vec::new(),
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

fn collect_dashboard_window_bucket_keys(
    suffixes: &mut Vec<String>,
    seen: &mut HashSet<String>,
    spec: &UsageDashboardWindowSpec,
    account_index: &[RedisAccountIndexItem],
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
        for pool in account_index {
            push_dashboard_bucket_suffix(
                suffixes,
                seen,
                usage_dashboard_bucket_key("account", &pool.id, epoch),
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
    key.ends_with("_usd")
}

fn usage_ratio(part: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        part as f64 / total as f64
    }
}

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

const USAGE_STATUS_VALUES: [&str; 5] = [
    "success",
    "error",
    "stream_error",
    "upstream_timeout",
    "client_dropped",
];

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
    if let Some(account_id) = query.account_id {
        if record.account_id != Some(account_id) {
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
    let account_id = record.account_id.map(|id| id.to_string());
    let estimated_cost = record.estimated_cost_usd.to_string();

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
        account_id.as_deref(),
        record.account_name.as_deref(),
        record.credential_label.as_deref(),
        Some(status),
        Some(source),
        record.error_type.as_deref(),
        record.error_message.as_deref(),
        record.error_detail.as_deref(),
        record.pricing_model.as_deref(),
        Some(estimated_cost.as_str()),
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

fn account_global_queue_key() -> &'static str {
    "account:global:queue_leases:v1"
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

fn decode_account_coordinator_guard_state(
    values: &[redis::Value],
) -> anyhow::Result<AccountCoordinatorGuardState> {
    let status = redis_value_i64(values, 0)?
        .ok_or_else(|| anyhow::anyhow!("Redis returned no account coordinator guard status"))?;
    let coordination_epoch = redis_value_string(values, 1)?.unwrap_or_default();
    if coordination_epoch.is_empty() && status != 2 {
        anyhow::bail!("Redis returned no account coordinator epoch");
    }
    match status {
        1 => Ok(AccountCoordinatorGuardState::Ready { coordination_epoch }),
        2 => Ok(AccountCoordinatorGuardState::EpochMismatch { coordination_epoch }),
        3 => Ok(AccountCoordinatorGuardState::Recovering {
            coordination_epoch,
            remaining: StdDuration::from_millis(
                redis_value_i64(values, 2)?.unwrap_or(1).max(1) as u64
            ),
        }),
        _ => anyhow::bail!("Redis returned an invalid account coordinator guard status"),
    }
}

struct InFlightKeys {
    last_seen: String,
    acquired: String,
    released: String,
}

fn account_in_flight_keys(pool_id: u64) -> InFlightKeys {
    InFlightKeys {
        last_seen: format!("account:inflight:{}:last_seen", pool_id),
        acquired: format!("account:inflight:{}:acquired", pool_id),
        released: format!("account:inflight:{}:released", pool_id),
    }
}

fn account_global_in_flight_keys() -> InFlightKeys {
    InFlightKeys {
        last_seen: "account:global:inflight:last_seen".to_string(),
        acquired: "account:global:inflight:acquired".to_string(),
        released: "account:global:inflight:released".to_string(),
    }
}

/// 账号被动质量采样状态的 Redis 键。
pub fn account_quality_key(pool_id: u64) -> String {
    format!("account:{pool_id}:quality")
}

/// 解析 Redis 中的质量状态 JSON。
///
/// 任何解析失败都返回 `None`，由调用方退化为中性值——质量数据是调度优化项，
/// 不是可用性依赖项，绝不能因为它的损坏而影响主请求。
fn decode_account_quality_state(raw: &str) -> Option<AccountQualityState> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let object = value.as_object()?;
    let read_f64 = |key: &str| object.get(key).and_then(serde_json::Value::as_f64);
    let positive_f64 = |key: &str| read_f64(key).filter(|value| value.is_finite() && *value >= 0.0);
    Some(AccountQualityState {
        recent_error_rate: read_f64("recent_error_rate")
            .filter(|value| value.is_finite())
            .unwrap_or(0.0)
            .clamp(0.0, 1.0),
        ttft_ewma_ms: positive_f64("ttft_ewma_ms"),
        latency_ewma_ms: positive_f64("latency_ewma_ms"),
        sample_count: read_f64("sample_count")
            .filter(|value| value.is_finite() && *value >= 0.0)
            .unwrap_or(0.0) as u64,
        probation_until_ms: read_f64("probation_until_ms")
            .filter(|value| value.is_finite() && *value > 0.0)
            .map(|value| value as i64),
        probation_level: read_f64("probation_level")
            .filter(|value| value.is_finite() && *value >= 0.0)
            .unwrap_or(0.0) as u32,
        probation_cleared_at_ms: read_f64("probation_cleared_at_ms")
            .filter(|value| value.is_finite() && *value > 0.0)
            .map(|value| value as i64),
        degrade_window_sample_count: read_f64("degrade_window_sample_count")
            .filter(|value| value.is_finite() && *value >= 0.0)
            .unwrap_or(0.0) as u64,
        degrade_window_failure_count: read_f64("degrade_window_failure_count")
            .filter(|value| value.is_finite() && *value >= 0.0)
            .unwrap_or(0.0) as u64,
        degrade_window_started_at_ms: read_f64("degrade_window_started_at_ms")
            .filter(|value| value.is_finite() && *value > 0.0)
            .map(|value| value as i64),
    })
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

    use redis::AsyncCommands;

    use super::*;
    use crate::model::config::Config;

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

    fn test_config() -> Option<Config> {
        let url = crate::storage::integration_test_url("CLAUDE_POOL_TEST_REDIS_URL")?;
        let mut config = Config::default();
        config.redis.url = Some(url);
        config.redis.key_prefix = format!("claude_pool:test:{}", uuid::Uuid::new_v4());
        Some(config)
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

    #[tokio::test]
    async fn redis_pattern_delete_is_bounded_and_cancellable() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 CLAUDE_POOL_TEST_REDIS_URL");
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

    // ---- 账号被动质量采样：纯逻辑单元测试（不依赖 Redis） ----

    #[test]
    fn account_quality_decode_rejects_corrupt_payloads_as_neutral() {
        // 质量数据是调度优化项而非可用性依赖项，任何损坏都必须退化为 None，
        // 由调用方按中性值处理，绝不能让它影响主请求。
        for raw in [
            "",
            "not json",
            "[1,2,3]",
            "\"a string\"",
            "123",
            "null",
            "{\"recent_error_rate\":",
        ] {
            assert!(
                decode_account_quality_state(raw).is_none(),
                "损坏载荷必须解析为 None: {raw:?}"
            );
        }
    }

    #[test]
    fn account_quality_decode_clamps_and_drops_invalid_fields() {
        // 越界失败率被夹紧，负数/非有限延迟被丢弃为 None 而不是变成 0——
        // 0 毫秒延迟会被评分器当成"极快"，是危险的误判方向。
        let state = decode_account_quality_state(
            r#"{
                "recent_error_rate": 7.5,
                "ttft_ewma_ms": -3.0,
                "latency_ewma_ms": 1800.5,
                "sample_count": 42,
                "probation_until_ms": 0,
                "probation_level": 2,
                "degrade_window_sample_count": 12,
                "degrade_window_failure_count": 7,
                "degrade_window_started_at_ms": 1000
            }"#,
        )
        .expect("合法 JSON 对象应能解析");

        assert_eq!(state.recent_error_rate, 1.0, "失败率必须夹紧到 1.0");
        assert_eq!(state.ttft_ewma_ms, None, "负首字必须丢弃而非置 0");
        assert_eq!(state.latency_ewma_ms, Some(1800.5));
        assert_eq!(state.sample_count, 42);
        assert_eq!(state.probation_until_ms, None, "0 不是有效避让时间戳");
        assert_eq!(state.probation_level, 2);
        assert_eq!(state.degrade_window_sample_count, 12);
        assert_eq!(state.degrade_window_failure_count, 7);
        assert_eq!(state.degrade_window_started_at_ms, Some(1000));

        let negative = decode_account_quality_state(r#"{"recent_error_rate": -2.0}"#)
            .expect("合法 JSON 对象应能解析");
        assert_eq!(negative.recent_error_rate, 0.0);

        // 缺失字段一律走中性默认值，不应报错。
        let empty = decode_account_quality_state("{}").expect("空对象应能解析");
        assert_eq!(empty, AccountQualityState::default());
    }

    #[test]
    fn account_quality_sample_threshold_guards_cold_start() {
        // 样本不足时不得参与质量评分，否则一两次抖动就能把账号打下去。
        let state = AccountQualityState {
            sample_count: 4,
            ..Default::default()
        };
        assert!(!state.has_enough_samples(5));
        assert!(state.has_enough_samples(4));

        // min_samples 配成 0 时按 1 处理：零样本永远不参与评分。
        let cold = AccountQualityState::default();
        assert!(!cold.has_enough_samples(0));
        assert!(
            AccountQualityState {
                sample_count: 1,
                ..Default::default()
            }
            .has_enough_samples(0)
        );
    }

    #[test]
    fn account_quality_probation_window_is_exclusive_at_expiry() {
        let state = AccountQualityState {
            probation_until_ms: Some(1_000),
            ..Default::default()
        };
        assert!(state.is_in_probation(999));
        assert!(!state.is_in_probation(1_000), "到期瞬间即视为已恢复");
        assert!(!state.is_in_probation(1_001));
        assert!(!AccountQualityState::default().is_in_probation(1_000));
    }

    #[test]
    fn account_quality_recovery_ramp_is_linear_and_saturating() {
        // 避让到期后线性回流，避免高优先级账号恢复瞬间被重新打满。
        let state = AccountQualityState {
            probation_until_ms: Some(1_000),
            probation_cleared_at_ms: Some(1_000),
            ..Default::default()
        };

        assert_eq!(state.recovery_progress(999, 100), 1.0, "避让期内不参与爬坡");
        assert_eq!(state.recovery_progress(1_000, 100), 0.0);
        assert!((state.recovery_progress(1_050, 100) - 0.5).abs() < 1e-9);
        assert_eq!(state.recovery_progress(1_100, 100), 1.0);
        assert_eq!(state.recovery_progress(9_999, 100), 1.0, "爬坡完成后不回退");

        // ramp 关闭（<=0）时直接全量恢复。
        assert_eq!(state.recovery_progress(1_000, 0), 1.0);
        assert_eq!(state.recovery_progress(1_000, -5), 1.0);

        // 从未避让过的账号不应被爬坡罚分。
        assert_eq!(
            AccountQualityState::default().recovery_progress(1_000, 100),
            1.0
        );
    }

    #[test]
    fn account_quality_key_is_namespaced_per_pool() {
        assert_eq!(account_quality_key(7), "account:7:quality");
        assert_ne!(account_quality_key(7), account_quality_key(8));
    }

    // ---- 账号被动质量采样：Redis Lua 往返测试 ----

    #[tokio::test]
    async fn redis_account_queue_admission_and_renewal_only_update_their_own_lease() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 CLAUDE_POOL_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let queue_key = store.key(account_global_queue_key());
        assert!(
            store
                .try_enter_account_dispatch_queue("earlier-waiter", 2, 60)
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
                .try_enter_account_dispatch_queue("later-waiter", 2, 120)
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
                .renew_account_dispatch_queue("earlier-waiter", 180)
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
                .renew_account_dispatch_queue("missing-waiter", 180)
                .await
                .unwrap()
        );

        assert!(
            store
                .leave_account_dispatch_queue("earlier-waiter")
                .await
                .unwrap()
        );
        assert!(
            store
                .leave_account_dispatch_queue("later-waiter")
                .await
                .unwrap()
        );
    }
}
