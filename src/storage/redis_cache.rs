use std::collections::{HashMap, HashSet};
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration as ChronoDuration, Timelike, Utc};
use redis::AsyncCommands;
use redis::aio::{ConnectionManager, PubSub};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::anthropic::usage::{
    REALTIME_USAGE_WINDOW_SECS, UsageAggregate, UsageBreakdownItem, UsageDashboardResponse,
    UsageDashboardSeries, UsageDashboardSummary, UsageDashboardTop, UsageDashboardWindow,
    UsageDashboardWindowSpec, UsageExternalPoolBillingByPool, UsageExternalPoolBillingSummary,
    UsageRealtimeStats, UsageRecord, UsageRecordQuery, UsageRecordStatus, UsageRecordsPageResult,
    UsageSeriesPoint, UsageSource, UsageSummary, UsageTopAggregate, usage_dashboard_daily_windows,
    usage_dashboard_hourly_windows, usage_dashboard_timezone, usage_dashboard_window_spec_for_key,
    usage_dashboard_windows,
};
use crate::model::config::Config;

pub(crate) const SCHEDULER_DISTRIBUTED_LEASE_SAFETY_SECS: u64 = 15 * 60;
const EXTERNAL_POOL_RELEASE_TOMBSTONE_TTL_SECS: u64 = 15 * 60;

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
    pub rate_limit_remaining_ms: Option<u64>,
    pub rate_limit_rpm: Option<u32>,
    pub rate_limit_owner_lease_id: Option<u64>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerDispatchAdmission {
    Acquired {
        in_flight_count: usize,
        rate_limit_available_at_ms: Option<i64>,
        rate_limit_remaining_ms: Option<u64>,
        rate_limit_rpm: Option<u32>,
        rate_limit_owner_lease_id: Option<u64>,
    },
    RateLimited {
        available_at_ms: i64,
        remaining_ms: u64,
        rpm: u32,
        owner_lease_id: Option<u64>,
    },
    CredentialCapacityFull,
    GlobalCapacityFull,
    LeaseCancelled,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExternalPoolCapacityState {
    pub pool_in_flight_requests: u32,
    pub global_in_flight_requests: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalPoolCircuitState {
    pub open: bool,
    pub open_until_ms: Option<i64>,
    pub reason: Option<String>,
    pub recent_failures: u32,
    pub distinct_credentials: u32,
}

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
    scheduler_admission_manager: ConnectionManager,
    scheduler_capacity_manager: ConnectionManager,
    key_prefix: String,
    #[cfg(test)]
    scheduler_admission_pre_eval_delay_ms: std::sync::Arc<std::sync::atomic::AtomicU64>,
    #[cfg(test)]
    scheduler_admission_post_eval_delay_ms: std::sync::Arc<std::sync::atomic::AtomicU64>,
    #[cfg(test)]
    scheduler_admission_eval_count: std::sync::Arc<std::sync::atomic::AtomicU64>,
    #[cfg(test)]
    scheduler_state_snapshot_count: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

const USAGE_SUMMARY_TOTALS_KEY: &str = "usage:summary:totals";
const USAGE_SUMMARY_CACHE_READ_KEY: &str = "usage:summary:cache_read";
const USAGE_SUMMARY_CACHE_READ_INDEX_KEY: &str = "usage:summary:cache_read:index";
const USAGE_SUMMARY_TOP_CREDENTIALS_KEY: &str = "usage:summary:top:credentials";
const USAGE_SUMMARY_TOP_CONVERSATIONS_KEY: &str = "usage:summary:top:conversations";
const USAGE_DASHBOARD_TOP_MODELS_KEY: &str = "usage:dashboard:top:models";
const USAGE_DASHBOARD_TOP_CREDENTIALS_KEY: &str = "usage:dashboard:top:credentials";
const USAGE_DASHBOARD_TOP_ENDPOINTS_KEY: &str = "usage:dashboard:top:endpoints";
const USAGE_DASHBOARD_TOP_ERRORS_KEY: &str = "usage:dashboard:top:errors";
const USAGE_DASHBOARD_TOP_EXTERNAL_POOLS_KEY: &str = "usage:dashboard:top:external_pools";
const USAGE_DASHBOARD_EXTERNAL_POOL_LIMIT: isize = 19;
const USAGE_RECORDS_INDEX_KEY: &str = "usage:records:index";
const USAGE_RECORDS_TTL_SECS: usize = 35 * 24 * 60 * 60;
const USAGE_RECORDS_MAX_CACHED: usize = 100_000;
const USAGE_RECORDS_TRIM_BATCH: usize = 512;
const USAGE_RECORDS_QUERY_SCAN_LIMIT: usize = 5_000;
const USAGE_CACHE_READ_INLINE_BUCKET_LIMIT: usize = 4_096;
const USAGE_CACHE_READ_INLINE_WARN_BUCKET_LIMIT: usize = 1_024;
const SESSION_CLEANUP_BATCH_SIZE: usize = 64;
const SESSION_BINDING_REVISION_TOMBSTONE_TTL_SECS: usize = 6 * 60 * 60;
const SCHEDULER_RATE_LIMIT_PHASE_CREDIT_MAX_MS: i64 = 125;
const SCHEDULER_RATE_LIMIT_PHASE_HISTORY_MAX_MS: i64 = 500;
const USAGE_RECORD_SNAPSHOT_SCRIPT: &str = r#"
    local ttl = tonumber(ARGV[1])
    local member = ARGV[2]
    local score = tonumber(ARGV[3])
    local cutoff_ms = tonumber(ARGV[4])
    local max_cached = tonumber(ARGV[5])
    local trim_batch = tonumber(ARGV[6])
    local item_key_prefix = ARGV[7]
    local encoded = ARGV[8]

    redis.call('SETEX', KEYS[1], ttl, encoded)
    redis.call('ZADD', KEYS[2], score, member)
    redis.call('EXPIRE', KEYS[2], ttl)

    local expired = redis.call('ZRANGEBYSCORE', KEYS[2], '-inf', cutoff_ms, 'LIMIT', 0, trim_batch)
    if #expired > 0 then
        for _, old_member in ipairs(expired) do
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
            for _, old_member in ipairs(old_members) do
                redis.call('DEL', item_key_prefix .. old_member)
            end
            redis.call('ZREM', KEYS[2], unpack(old_members))
        end
    end

    return 1
"#;
const DISPATCH_QUEUE_PRUNE_AND_COUNT_SCRIPT: &str = r#"
    local now = redis.call('TIME')
    local now_ms = tonumber(now[1]) * 1000 + math.floor(tonumber(now[2]) / 1000)
    redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', now_ms)
    return redis.call('ZCARD', KEYS[1])
"#;
const EXTERNAL_DISPATCH_QUEUE_ADMIT_SCRIPT: &str = r#"
    local max_queued = tonumber(ARGV[1])
    local ttl_ms = tonumber(ARGV[2]) * 1000
    local lease_id = ARGV[3]
    local now = redis.call('TIME')
    local now_ms = tonumber(now[1]) * 1000 + math.floor(tonumber(now[2]) / 1000)

    redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', now_ms)
    local expired_tombstones = redis.call(
        'ZRANGEBYSCORE', KEYS[2], '-inf', now_ms, 'LIMIT', 0, 256
    )
    if #expired_tombstones > 0 then
        redis.call('ZREM', KEYS[2], unpack(expired_tombstones))
    end
    if redis.call('ZSCORE', KEYS[2], lease_id) then
        return -1
    end
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
const EXTERNAL_DISPATCH_QUEUE_RELEASE_SCRIPT: &str = r#"
    local tombstone_ttl_ms = tonumber(ARGV[1]) * 1000
    local now = redis.call('TIME')
    local now_ms = tonumber(now[1]) * 1000 + math.floor(tonumber(now[2]) / 1000)
    local tombstone_expires_at_ms = now_ms + tombstone_ttl_ms

    local expired_tombstones = redis.call(
        'ZRANGEBYSCORE', KEYS[2], '-inf', now_ms, 'LIMIT', 0, 256
    )
    if #expired_tombstones > 0 then
        redis.call('ZREM', KEYS[2], unpack(expired_tombstones))
    end

    local lease_ids = {}
    local zadd_args = {'ZADD', KEYS[2]}
    for index = 2, #ARGV do
        local lease_id = ARGV[index]
        lease_ids[#lease_ids + 1] = lease_id
        zadd_args[#zadd_args + 1] = tombstone_expires_at_ms
        zadd_args[#zadd_args + 1] = lease_id
    end
    redis.call(unpack(zadd_args))
    local latest_tombstone = redis.call('ZREVRANGE', KEYS[2], 0, 0, 'WITHSCORES')
    if latest_tombstone[2] then
        redis.call('PEXPIREAT', KEYS[2], math.ceil(tonumber(latest_tombstone[2])))
    end

    local removed = redis.call('ZREM', KEYS[1], unpack(lease_ids))
    if redis.call('ZCARD', KEYS[1]) == 0 then
        redis.call('DEL', KEYS[1])
    end
    return removed
"#;
const EXTERNAL_POOL_LEASE_RELEASE_SCRIPT: &str = r#"
    local tombstone_ttl_ms = tonumber(ARGV[1]) * 1000
    local now = redis.call('TIME')
    local now_ms = tonumber(now[1]) * 1000 + math.floor(tonumber(now[2]) / 1000)
    local tombstone_expires_at_ms = now_ms + tombstone_ttl_ms

    local expired_tombstones = redis.call(
        'ZRANGEBYSCORE', KEYS[5], '-inf', now_ms, 'LIMIT', 0, 256
    )
    if #expired_tombstones > 0 then
        redis.call('ZREM', KEYS[5], unpack(expired_tombstones))
    end

    local lease_ids = {}
    local zadd_args = {'ZADD', KEYS[5]}
    for index = 2, #ARGV do
        local lease_id = ARGV[index]
        lease_ids[#lease_ids + 1] = lease_id
        zadd_args[#zadd_args + 1] = tombstone_expires_at_ms
        zadd_args[#zadd_args + 1] = lease_id
    end
    redis.call(unpack(zadd_args))
    local latest_tombstone = redis.call('ZREVRANGE', KEYS[5], 0, 0, 'WITHSCORES')
    if latest_tombstone[2] then
        redis.call('PEXPIREAT', KEYS[5], math.ceil(tonumber(latest_tombstone[2])))
    end

    redis.call('ZREM', KEYS[1], unpack(lease_ids))
    local pool_acquired_removed = redis.call('ZREM', KEYS[2], unpack(lease_ids))
    redis.call('ZREM', KEYS[3], unpack(lease_ids))
    redis.call('ZREM', KEYS[4], unpack(lease_ids))
    return pool_acquired_removed
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
const LOCAL_DISPATCH_QUEUE_ADMIT_SCRIPT: &str = r#"
    local max_queued = tonumber(ARGV[1])
    local ttl_ms = tonumber(ARGV[2]) * 1000
    local lease_id = ARGV[3]
    local now = redis.call('TIME')
    local now_ms = tonumber(now[1]) * 1000 + math.floor(tonumber(now[2]) / 1000)

    redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', now_ms)
    redis.call('ZREMRANGEBYSCORE', KEYS[2], '-inf', now_ms)
    if redis.call('ZSCORE', KEYS[2], lease_id) then
        return -1
    end
    if redis.call('ZSCORE', KEYS[1], lease_id) then
        return 1
    end

    local count = redis.call('ZCARD', KEYS[1])
    if max_queued > 0 and count >= max_queued then
        return 0
    end
    if redis.call('ZSCORE', KEYS[2], lease_id) then
        return -1
    end

    local expires_at_ms = now_ms + ttl_ms
    redis.call('ZADD', KEYS[1], expires_at_ms, lease_id)
    local latest = redis.call('ZREVRANGE', KEYS[1], 0, 0, 'WITHSCORES')
    if latest[2] then
        redis.call('PEXPIREAT', KEYS[1], math.ceil(tonumber(latest[2])))
    end
    return 1
"#;
const LOCAL_DISPATCH_QUEUE_RELEASE_SCRIPT: &str = r#"
    local lease_id = ARGV[1]
    local tombstone_ttl_ms = tonumber(ARGV[2]) * 1000
    local now = redis.call('TIME')
    local now_ms = tonumber(now[1]) * 1000 + math.floor(tonumber(now[2]) / 1000)
    local tombstone_expires_at_ms = now_ms + tombstone_ttl_ms

    redis.call('ZREMRANGEBYSCORE', KEYS[2], '-inf', now_ms)
    redis.call('ZADD', KEYS[2], tombstone_expires_at_ms, lease_id)
    local latest_tombstone = redis.call('ZREVRANGE', KEYS[2], 0, 0, 'WITHSCORES')
    if latest_tombstone[2] then
        redis.call('PEXPIREAT', KEYS[2], math.ceil(tonumber(latest_tombstone[2])))
    end

    local removed = redis.call('ZREM', KEYS[1], lease_id)
    if redis.call('ZCARD', KEYS[1]) == 0 then
        redis.call('DEL', KEYS[1])
    end
    return removed
"#;
impl RedisStore {
    pub async fn connect(config: &Config) -> anyhow::Result<Self> {
        let url = config
            .redis
            .url
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("必须配置 redis.url"))?;
        let client = redis::Client::open(url)?;
        let manager = client.get_connection_manager().await?;
        let scheduler_manager = client.get_connection_manager().await?;
        let scheduler_admission_manager = client.get_connection_manager().await?;
        let scheduler_capacity_manager = client.get_connection_manager().await?;
        Ok(Self {
            client,
            manager,
            scheduler_manager,
            scheduler_admission_manager,
            scheduler_capacity_manager,
            key_prefix: config.redis.key_prefix.trim_end_matches(':').to_string(),
            #[cfg(test)]
            scheduler_admission_pre_eval_delay_ms: std::sync::Arc::new(
                std::sync::atomic::AtomicU64::new(0),
            ),
            #[cfg(test)]
            scheduler_admission_post_eval_delay_ms: std::sync::Arc::new(
                std::sync::atomic::AtomicU64::new(0),
            ),
            #[cfg(test)]
            scheduler_admission_eval_count: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(
                0,
            )),
            #[cfg(test)]
            scheduler_state_snapshot_count: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(
                0,
            )),
        })
    }

    #[cfg(test)]
    pub(crate) fn delay_next_scheduler_admission_before_eval(&self, delay: StdDuration) {
        self.scheduler_admission_pre_eval_delay_ms.store(
            delay.as_millis().min(u64::MAX as u128) as u64,
            std::sync::atomic::Ordering::Release,
        );
    }

    #[cfg(test)]
    pub(crate) fn delay_next_scheduler_admission_after_eval(&self, delay: StdDuration) {
        self.scheduler_admission_post_eval_delay_ms.store(
            delay.as_millis().min(u64::MAX as u128) as u64,
            std::sync::atomic::Ordering::Release,
        );
    }

    #[cfg(test)]
    pub(crate) fn scheduler_admission_eval_count(&self) -> u64 {
        self.scheduler_admission_eval_count
            .load(std::sync::atomic::Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn scheduler_state_snapshot_count(&self) -> u64 {
        self.scheduler_state_snapshot_count
            .load(std::sync::atomic::Ordering::Acquire)
    }

    fn scheduler_manager(&self) -> ConnectionManager {
        self.scheduler_manager.clone()
    }

    fn scheduler_admission_manager(&self) -> ConnectionManager {
        self.scheduler_admission_manager.clone()
    }

    fn scheduler_capacity_manager(&self) -> ConnectionManager {
        self.scheduler_capacity_manager.clone()
    }

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

    async fn dashboard_window_summary_cache(
        &self,
        specs: &[UsageDashboardWindowSpec],
    ) -> anyhow::Result<DashboardBucketCache> {
        let mut suffixes = Vec::new();
        let mut seen = HashSet::new();
        for spec in specs {
            collect_dashboard_global_bucket_keys(&mut suffixes, &mut seen, spec);
            collect_dashboard_cache_read_bucket_keys(&mut suffixes, &mut seen, spec);
        }
        self.dashboard_bucket_cache_for_suffixes(suffixes).await
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

    async fn dashboard_breakdown_cache(
        &self,
        spec: &UsageDashboardWindowSpec,
    ) -> anyhow::Result<DashboardBucketCache> {
        let mut suffixes = Vec::new();
        let mut seen = HashSet::new();
        collect_dashboard_global_bucket_keys(&mut suffixes, &mut seen, spec);
        collect_dashboard_dimension_bucket_keys(
            &mut suffixes,
            &mut seen,
            spec,
            "status",
            &USAGE_STATUS_VALUES,
        );
        collect_dashboard_dimension_bucket_keys(
            &mut suffixes,
            &mut seen,
            spec,
            "usage_source",
            &USAGE_SOURCE_VALUES,
        );
        self.dashboard_bucket_cache_for_suffixes(suffixes).await
    }

    async fn dashboard_external_pool_billing_cache(
        &self,
        spec: &UsageDashboardWindowSpec,
        external_pool_index: &[RedisExternalPoolIndexItem],
    ) -> anyhow::Result<DashboardBucketCache> {
        let mut suffixes = Vec::new();
        let mut seen = HashSet::new();
        collect_dashboard_external_pool_bucket_keys(
            &mut suffixes,
            &mut seen,
            spec,
            external_pool_index,
        );
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

    pub fn runtime_config_changed_channel(&self) -> String {
        self.key("events:runtime_config_changed")
    }

    pub fn credentials_changed_channel(&self) -> String {
        self.key("events:credentials_changed")
    }

    pub fn external_pools_changed_channel(&self) -> String {
        self.key("events:external_pools_changed")
    }

    pub fn dispatch_wakeup_channel(&self) -> String {
        self.key("events:dispatch_wakeup")
    }

    pub async fn subscribe_runtime_events(&self) -> anyhow::Result<PubSub> {
        let mut pubsub = self.client.get_async_pubsub().await?;
        pubsub
            .subscribe(self.runtime_config_changed_channel())
            .await?;
        pubsub.subscribe(self.credentials_changed_channel()).await?;
        pubsub
            .subscribe(self.external_pools_changed_channel())
            .await?;
        pubsub.subscribe(self.dispatch_wakeup_channel()).await?;
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

    pub async fn publish_external_pools_changed(
        &self,
        payload: impl AsRef<str>,
    ) -> anyhow::Result<()> {
        self.publish_event(self.external_pools_changed_channel(), payload)
            .await
    }

    pub async fn invalidate_external_pool_admin_cache_and_publish(
        &self,
        payload: impl AsRef<str>,
    ) -> anyhow::Result<()> {
        let script = r#"
            redis.call('DEL', KEYS[1], KEYS[2])
            return redis.call('PUBLISH', KEYS[3], ARGV[1])
        "#;
        let mut manager = self.manager.clone();
        let _: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(3)
            .arg(self.key("admin_cache:external_pools:status"))
            .arg(self.key("admin_cache:external_pools:list"))
            .arg(self.external_pools_changed_channel())
            .arg(payload.as_ref())
            .query_async(&mut manager)
            .await?;
        Ok(())
    }

    pub async fn publish_dispatch_wakeup(&self, payload: impl AsRef<str>) -> anyhow::Result<()> {
        self.publish_event(self.dispatch_wakeup_channel(), payload)
            .await
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

    pub async fn del_count(&self, key: impl AsRef<str>) -> anyhow::Result<usize> {
        let mut manager = self.manager.clone();
        let removed: i64 = manager.del(self.key(key)).await?;
        Ok(removed.max(0) as usize)
    }

    pub async fn del_pattern(&self, pattern: impl AsRef<str>) -> anyhow::Result<usize> {
        let full_pattern = self.key(pattern);
        let mut manager = self.manager.clone();
        let mut cursor = 0u64;
        let mut deleted = 0usize;
        loop {
            let (next_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&full_pattern)
                .arg("COUNT")
                .arg(1000)
                .query_async(&mut manager)
                .await?;
            if !keys.is_empty() {
                let removed: i64 = redis::cmd("DEL")
                    .arg(keys)
                    .query_async(&mut manager)
                    .await?;
                deleted = deleted.saturating_add(removed.max(0) as usize);
            }
            cursor = next_cursor;
            if cursor == 0 {
                break;
            }
        }
        Ok(deleted)
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

    pub async fn record_usage_record_snapshot(&self, record: &UsageRecord) -> anyhow::Result<()> {
        let created_at = DateTime::parse_from_rfc3339(&record.created_at)
            .map(|created_at| created_at.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());
        self.record_usage_record_snapshot_with_limits(
            record,
            created_at,
            USAGE_RECORDS_MAX_CACHED,
            USAGE_RECORDS_TRIM_BATCH,
        )
        .await
    }

    async fn record_usage_record_snapshot_with_limits(
        &self,
        record: &UsageRecord,
        created_at: DateTime<Utc>,
        max_cached: usize,
        trim_batch: usize,
    ) -> anyhow::Result<()> {
        let mut pipe = redis::pipe();
        self.append_usage_record_snapshot_command(
            &mut pipe, record, created_at, max_cached, trim_batch,
        )?;
        let mut manager = self.manager.clone();
        let result: Vec<i64> = pipe.query_async(&mut manager).await?;
        if result.first().copied() != Some(1) {
            anyhow::bail!("Redis usage record snapshot returned an invalid result");
        }
        Ok(())
    }

    fn append_usage_record_snapshot_command(
        &self,
        pipe: &mut redis::Pipeline,
        record: &UsageRecord,
        created_at: DateTime<Utc>,
        max_cached: usize,
        trim_batch: usize,
    ) -> anyhow::Result<()> {
        let member = usage_dimension_hash(&record.id);
        let record_key = self.key(usage_record_key(&member));
        let index_key = self.key(USAGE_RECORDS_INDEX_KEY);
        let item_key_prefix = self.key("usage:records:item:");
        let encoded = serde_json::to_string(record)?;
        let cutoff_ms = Utc::now()
            .timestamp_millis()
            .saturating_sub((USAGE_RECORDS_TTL_SECS as i64).saturating_mul(1000));
        pipe.cmd("EVAL")
            .arg(USAGE_RECORD_SNAPSHOT_SCRIPT)
            .arg(2)
            .arg(&record_key)
            .arg(&index_key)
            .arg(USAGE_RECORDS_TTL_SECS)
            .arg(&member)
            .arg(created_at.timestamp_millis())
            .arg(cutoff_ms)
            .arg(max_cached.max(1))
            .arg(trim_batch.max(1))
            .arg(item_key_prefix)
            .arg(encoded);
        Ok(())
    }

    pub async fn usage_records_page(
        &self,
        query: UsageRecordQuery,
        page: usize,
        limit: usize,
    ) -> anyhow::Result<Option<UsageRecordsPageResult>> {
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
        let mut manager = self.manager.clone();
        let totals: HashMap<String, String> =
            manager.hgetall(self.key(USAGE_SUMMARY_TOTALS_KEY)).await?;
        if totals.is_empty() {
            return Ok(None);
        }

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

    pub async fn usage_dashboard(
        &self,
        timezone: Option<&str>,
        high_cache_threshold: i32,
    ) -> anyhow::Result<Option<UsageDashboardResponse>> {
        let started_at = std::time::Instant::now();
        let mut manager = self.manager.clone();
        let totals: HashMap<String, String> =
            manager.hgetall(self.key(USAGE_SUMMARY_TOTALS_KEY)).await?;
        if totals.is_empty() {
            return Ok(None);
        }
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

    pub async fn usage_dashboard_windows_only(
        &self,
        timezone: Option<&str>,
        high_cache_threshold: i32,
    ) -> anyhow::Result<Option<(String, String, Vec<UsageDashboardWindow>)>> {
        let mut manager = self.manager.clone();
        let totals: HashMap<String, String> =
            manager.hgetall(self.key(USAGE_SUMMARY_TOTALS_KEY)).await?;
        if totals.is_empty() {
            return Ok(None);
        }
        let lifetime_requests = usage_usize(&totals, "total_requests");

        let now = Utc::now();
        let (timezone, offset) = usage_dashboard_timezone(timezone);
        let window_specs = usage_dashboard_windows(now, offset);
        let bucket_cache = self.dashboard_window_summary_cache(&window_specs).await?;
        let windows = window_specs
            .iter()
            .map(|spec| {
                self.dashboard_window_from_cache(spec, high_cache_threshold, &[], &bucket_cache)
            })
            .collect::<Vec<_>>();
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
        let mut manager = self.manager.clone();
        let totals: HashMap<String, String> =
            manager.hgetall(self.key(USAGE_SUMMARY_TOTALS_KEY)).await?;
        if totals.is_empty() {
            return Ok(None);
        }

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
        let mut manager = self.manager.clone();
        let totals: HashMap<String, String> =
            manager.hgetall(self.key(USAGE_SUMMARY_TOTALS_KEY)).await?;
        if totals.is_empty() {
            return Ok(None);
        }

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

    pub async fn usage_dashboard_breakdown_only(
        &self,
        timezone: Option<&str>,
        window_key: &str,
    ) -> anyhow::Result<
        Option<(
            String,
            String,
            String,
            Vec<UsageBreakdownItem>,
            Vec<UsageBreakdownItem>,
        )>,
    > {
        let mut manager = self.manager.clone();
        let totals: HashMap<String, String> =
            manager.hgetall(self.key(USAGE_SUMMARY_TOTALS_KEY)).await?;
        if totals.is_empty() {
            return Ok(None);
        }

        let now = Utc::now();
        let (timezone, offset) = usage_dashboard_timezone(timezone);
        let spec = usage_dashboard_window_spec_for_key(now, offset, window_key);
        let bucket_cache = self.dashboard_breakdown_cache(&spec).await?;
        let total_requests = usage_usize(
            &bucket_cache.sum_bucket("global", "all", &spec),
            "total_requests",
        );
        let status_breakdown = self.dashboard_breakdown_from_cache(
            &spec,
            "status",
            &USAGE_STATUS_VALUES,
            usage_status_label,
            total_requests,
            &bucket_cache,
        );
        let usage_source_breakdown = self.dashboard_breakdown_from_cache(
            &spec,
            "usage_source",
            &USAGE_SOURCE_VALUES,
            usage_source_label,
            total_requests,
            &bucket_cache,
        );
        Ok(Some((
            now.to_rfc3339(),
            timezone,
            spec.key,
            status_breakdown,
            usage_source_breakdown,
        )))
    }

    pub async fn usage_dashboard_external_pool_billing_only(
        &self,
        timezone: Option<&str>,
        window_key: &str,
    ) -> anyhow::Result<Option<(String, String, String, Vec<UsageExternalPoolBillingByPool>)>> {
        let mut manager = self.manager.clone();
        let totals: HashMap<String, String> =
            manager.hgetall(self.key(USAGE_SUMMARY_TOTALS_KEY)).await?;
        if totals.is_empty() {
            return Ok(None);
        }

        let now = Utc::now();
        let (timezone, offset) = usage_dashboard_timezone(timezone);
        let spec = usage_dashboard_window_spec_for_key(now, offset, window_key);
        let external_pool_index = self.dashboard_external_pool_index().await?;
        let bucket_cache = self
            .dashboard_external_pool_billing_cache(&spec, &external_pool_index)
            .await?;
        let billing = self.dashboard_external_pool_billing_by_pool_from_cache(
            &spec,
            &external_pool_index,
            &bucket_cache,
        );
        Ok(Some((now.to_rfc3339(), timezone, spec.key, billing)))
    }

    pub async fn clear_usage_summary(&self) -> anyhow::Result<usize> {
        let summary_deleted = self.del_pattern("usage:summary:*").await?;
        let dashboard_deleted = self.del_pattern("usage:dashboard:*").await?;
        let record_deleted = self.clear_usage_record_snapshots().await?;
        Ok(summary_deleted
            .saturating_add(dashboard_deleted)
            .saturating_add(record_deleted))
    }

    pub async fn clear_usage_record_snapshots(&self) -> anyhow::Result<usize> {
        let initial_index_deleted = self.del_count(USAGE_RECORDS_INDEX_KEY).await?;
        let item_deleted = self.del_pattern("usage:records:item:*").await?;
        let final_index_deleted = self.del_count(USAGE_RECORDS_INDEX_KEY).await?;
        Ok(initial_index_deleted
            .saturating_add(item_deleted)
            .saturating_add(final_index_deleted))
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
    ) -> anyhow::Result<Option<SchedulerSessionBinding>> {
        let encoded = serde_json::to_string(binding)?;
        let session_hash = session_hash(session_id);
        let script = r#"
            local old = redis.call('GET', KEYS[1])
            local next = cjson.decode(ARGV[3])
            local current_revision = tonumber(redis.call('GET', KEYS[2]) or '-1')
            local next_revision = tonumber(ARGV[6])
            if current_revision >= next_revision then
                return old
            end
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
            redis.call('SET', KEYS[2], ARGV[6], 'EX', ARGV[4])
            redis.call('SADD', ARGV[5] .. ARGV[2], ARGV[1])
            redis.call('EXPIRE', ARGV[5] .. ARGV[2], ARGV[4])
            return next_encoded
        "#;
        let mut manager = self.scheduler_manager();
        let actual: Option<String> = redis::cmd("EVAL")
            .arg(script)
            .arg(2)
            .arg(self.key(session_binding_key(session_id)))
            .arg(self.key(session_binding_revision_key(session_id)))
            .arg(&session_hash)
            .arg(binding.credential_id.to_string())
            .arg(encoded)
            .arg(ttl_secs.max(1))
            .arg(self.key("scheduler:sessions_by_credential:"))
            .arg(binding.last_used_at.timestamp_micros())
            .query_async(&mut manager)
            .await?;
        actual
            .map(|actual| serde_json::from_str(&actual).map_err(anyhow::Error::from))
            .transpose()
    }

    pub async fn delete_session_binding(&self, session_id: &str) -> anyhow::Result<()> {
        let session_hash = session_hash(session_id);
        let script = r#"
            local old = redis.call('GET', KEYS[1])
            local now = redis.call('TIME')
            local redis_revision_raw = now[1] .. string.format('%06d', tonumber(now[2]))
            local current_revision_raw = redis.call('GET', KEYS[2]) or '-1'
            local revision_raw = current_revision_raw
            if tonumber(redis_revision_raw) > tonumber(current_revision_raw) then
                revision_raw = redis_revision_raw
            end
            redis.call('DEL', KEYS[1])
            redis.call('SET', KEYS[2], revision_raw, 'EX', ARGV[3])
            if old then
                local ok, parsed = pcall(cjson.decode, old)
                if ok and parsed['credential_id'] then
                    redis.call('SREM', ARGV[2] .. tostring(parsed['credential_id']), ARGV[1])
                end
            end
            return 1
        "#;
        let mut manager = self.scheduler_manager();
        let _: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(2)
            .arg(self.key(session_binding_key(session_id)))
            .arg(self.key(session_binding_revision_key(session_id)))
            .arg(&session_hash)
            .arg(self.key("scheduler:sessions_by_credential:"))
            .arg(SESSION_BINDING_REVISION_TOMBSTONE_TTL_SECS)
            .query_async(&mut manager)
            .await?;
        Ok(())
    }

    pub async fn delete_session_binding_if_bound_to(
        &self,
        session_id: &str,
        credential_id: u64,
    ) -> anyhow::Result<bool> {
        let session_hash = session_hash(session_id);
        let script = r#"
            local old = redis.call('GET', KEYS[1])
            local now = redis.call('TIME')
            local redis_revision_raw = now[1] .. string.format('%06d', tonumber(now[2]))
            local current_revision_raw = redis.call('GET', KEYS[2]) or '-1'
            local revision_raw = current_revision_raw
            if tonumber(redis_revision_raw) > tonumber(current_revision_raw) then
                revision_raw = redis_revision_raw
            end
            if not old then
                redis.call('SET', KEYS[2], revision_raw, 'EX', ARGV[4])
                redis.call('SREM', ARGV[3] .. ARGV[2], ARGV[1])
                return 1
            end
            local ok, parsed = pcall(cjson.decode, old)
            if not ok or not parsed['credential_id'] then
                redis.call('SET', KEYS[2], revision_raw, 'EX', ARGV[4])
                return 0
            end
            if tostring(parsed['credential_id']) ~= ARGV[2] then
                redis.call('SET', KEYS[2], revision_raw, 'EX', ARGV[4])
                return 0
            end
            redis.call('DEL', KEYS[1])
            redis.call('SET', KEYS[2], revision_raw, 'EX', ARGV[4])
            redis.call('SREM', ARGV[3] .. ARGV[2], ARGV[1])
            return 1
        "#;
        let mut manager = self.scheduler_manager();
        let deleted: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(2)
            .arg(self.key(session_binding_key(session_id)))
            .arg(self.key(session_binding_revision_key(session_id)))
            .arg(&session_hash)
            .arg(credential_id.to_string())
            .arg(self.key("scheduler:sessions_by_credential:"))
            .arg(SESSION_BINDING_REVISION_TOMBSTONE_TTL_SECS)
            .query_async(&mut manager)
            .await?;
        Ok(deleted == 1)
    }

    pub(crate) async fn delete_sessions_for_credential_batch(
        &self,
        credential_id: u64,
    ) -> anyhow::Result<(usize, bool)> {
        let set_key = self.key(sessions_by_credential_key(credential_id));
        let mut manager = self.scheduler_manager();
        let script = r#"
            local session_hashes = redis.call('SPOP', KEYS[1], tonumber(ARGV[1]))
            local now = redis.call('TIME')
            local redis_revision_raw = now[1] .. string.format('%06d', tonumber(now[2]))
            local deleted = 0
            for _, session_hash in ipairs(session_hashes) do
                local session_key = ARGV[3] .. session_hash
                local raw = redis.call('GET', session_key)
                if raw then
                    local ok, parsed = pcall(cjson.decode, raw)
                    if ok and type(parsed) == 'table' and parsed['credential_id'] ~= nil
                        and tostring(parsed['credential_id']) == ARGV[2] then
                        local revision_key = ARGV[4] .. session_hash
                        local current_revision_raw = redis.call('GET', revision_key) or '-1'
                        local revision_raw = current_revision_raw
                        if tonumber(redis_revision_raw) > tonumber(current_revision_raw) then
                            revision_raw = redis_revision_raw
                        end
                        redis.call('DEL', session_key)
                        redis.call(
                            'SET', revision_key, revision_raw, 'EX', ARGV[5]
                        )
                        deleted = deleted + 1
                    end
                end
            end
            return {#session_hashes, deleted}
        "#;
        let result: Vec<usize> = redis::cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(&set_key)
            .arg(SESSION_CLEANUP_BATCH_SIZE)
            .arg(credential_id.to_string())
            .arg(self.key("scheduler:session:"))
            .arg(self.key("scheduler:session_revision:"))
            .arg(SESSION_BINDING_REVISION_TOMBSTONE_TTL_SECS)
            .query_async(&mut manager)
            .await?;
        if result.len() != 2 {
            anyhow::bail!("Redis 凭据会话清理返回了无效结果");
        }
        Ok((result[1], result[0] == SESSION_CLEANUP_BATCH_SIZE))
    }

    pub async fn delete_sessions_for_credential(
        &self,
        credential_id: u64,
    ) -> anyhow::Result<usize> {
        let mut deleted = 0usize;
        loop {
            let (batch_deleted, may_have_more) = self
                .delete_sessions_for_credential_batch(credential_id)
                .await?;
            deleted = deleted.saturating_add(batch_deleted);
            if !may_have_more {
                break;
            }
            tokio::task::yield_now().await;
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
            local current_revision = tonumber(redis.call('GET', KEYS[2]) or '-1')
            local next_revision = tonumber(ARGV[6])
            if current_revision >= next_revision then
                return raw
            end
            parsed['soft_failure_count'] = tonumber(parsed['soft_failure_count'] or '0') + 1
            parsed['last_used_at'] = ARGV[3]
            local encoded = cjson.encode(parsed)
            redis.call('SET', KEYS[1], encoded, 'EX', ARGV[4])
            redis.call('SET', KEYS[2], ARGV[6], 'EX', ARGV[4])
            redis.call('SADD', ARGV[5] .. ARGV[2], ARGV[1])
            redis.call('EXPIRE', ARGV[5] .. ARGV[2], ARGV[4])
            return encoded
        "#;
        let now = Utc::now();
        let mut manager = self.scheduler_manager();
        let encoded: Option<String> = redis::cmd("EVAL")
            .arg(script)
            .arg(2)
            .arg(self.key(session_binding_key(session_id)))
            .arg(self.key(session_binding_revision_key(session_id)))
            .arg(&session_hash)
            .arg(credential_id.to_string())
            .arg(now.to_rfc3339())
            .arg(ttl_secs.max(1))
            .arg(self.key("scheduler:sessions_by_credential:"))
            .arg(now.timestamp_micros())
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
            local current_revision = tonumber(redis.call('GET', KEYS[2]) or '-1')
            local next_revision = tonumber(ARGV[6])
            if current_revision >= next_revision then
                return raw
            end
            parsed['soft_failure_count'] = 0
            parsed['last_used_at'] = ARGV[3]
            local encoded = cjson.encode(parsed)
            redis.call('SET', KEYS[1], encoded, 'EX', ARGV[4])
            redis.call('SET', KEYS[2], ARGV[6], 'EX', ARGV[4])
            redis.call('SADD', ARGV[5] .. ARGV[2], ARGV[1])
            redis.call('EXPIRE', ARGV[5] .. ARGV[2], ARGV[4])
            return encoded
        "#;
        let now = Utc::now();
        let mut manager = self.scheduler_manager();
        let encoded: Option<String> = redis::cmd("EVAL")
            .arg(script)
            .arg(2)
            .arg(self.key(session_binding_key(session_id)))
            .arg(self.key(session_binding_revision_key(session_id)))
            .arg(&session_hash)
            .arg(credential_id.to_string())
            .arg(now.to_rfc3339())
            .arg(ttl_secs.max(1))
            .arg(self.key("scheduler:sessions_by_credential:"))
            .arg(now.timestamp_micros())
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
            local weight_units = tonumber(ARGV[6])
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
            redis.call('SET', KEYS[1], cjson.encode(health), 'EX', ttl)
            return cjson.encode(health)
        "#;
        let mut manager = self.scheduler_manager();
        let encoded: String = redis::cmd("EVAL")
            .arg(script)
            .arg(3)
            .arg(self.key(scheduler_health_key(credential_id)))
            .arg(self.key(scheduler_selection_window_key(credential_id)))
            .arg(self.key("scheduler:selection:sequence"))
            .arg(30 * 24 * 60 * 60)
            .arg(now)
            .arg(10_000)
            .arg(60_000)
            .arg(5 * 60_000)
            .arg(weight_units)
            .query_async(&mut manager)
            .await?;
        Ok(serde_json::from_str(&encoded)?)
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
            drop(manager);
            self.clear_rate_limit(credential_id).await?;
            return Ok(None);
        }
        Ok(Some(until_ms))
    }

    pub async fn clear_rate_limit(&self, credential_id: u64) -> anyhow::Result<()> {
        let mut manager = self.scheduler_manager();
        let _: usize = manager
            .del(vec![
                self.key(scheduler_rate_limit_key(credential_id)),
                self.key(scheduler_rate_limit_owner_key(credential_id)),
                self.key(scheduler_rate_limit_rpm_key(credential_id)),
                self.key(scheduler_rate_limit_phase_key(credential_id)),
            ])
            .await?;
        Ok(())
    }

    #[allow(dead_code)]
    pub async fn next_in_flight_lease_id(&self) -> anyhow::Result<u64> {
        let mut manager = self.scheduler_capacity_manager();
        let id: u64 = manager
            .incr(self.key("scheduler:inflight:lease_sequence"), 1u64)
            .await?;
        Ok(id)
    }

    pub async fn next_external_pool_lease_id(&self) -> anyhow::Result<u64> {
        let mut manager = self.scheduler_capacity_manager();
        let id: u64 = manager
            .incr(self.key("external_pool:inflight:lease_sequence"), 1u64)
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
        Ok(
            match self
                .acquire_dispatch_lease_with_rate_limit(
                    credential_id,
                    lease_id,
                    max_concurrent_requests,
                    global_max_concurrent_requests,
                    request_weight_units,
                    0,
                    max_age,
                    kind,
                )
                .await?
            {
                SchedulerDispatchAdmission::Acquired {
                    in_flight_count, ..
                } => Some(in_flight_count),
                SchedulerDispatchAdmission::RateLimited { .. }
                | SchedulerDispatchAdmission::CredentialCapacityFull
                | SchedulerDispatchAdmission::GlobalCapacityFull
                | SchedulerDispatchAdmission::LeaseCancelled => None,
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn acquire_dispatch_lease_with_rate_limit(
        &self,
        credential_id: u64,
        lease_id: u64,
        max_concurrent_requests: u32,
        global_max_concurrent_requests: u32,
        request_weight_units: u32,
        rpm: u32,
        max_age: Option<StdDuration>,
        kind: &str,
    ) -> anyhow::Result<SchedulerDispatchAdmission> {
        let request_weight_units = request_weight_units.clamp(1, 64);
        let max_age_ms = max_age.map(|age| age.as_millis() as i64).unwrap_or(0);
        let ttl_secs = max_age
            .map(|age| age.as_secs().saturating_mul(2).max(60) as i64)
            .unwrap_or(0);
        let script = r#"
            local redis_time = redis.call('TIME')
            local now = tonumber(redis_time[1]) * 1000 + math.floor(tonumber(redis_time[2]) / 1000)
            local max_age_ms = tonumber(ARGV[2])
            local max_count = tonumber(ARGV[3])
            local global_max_count = tonumber(ARGV[4])
            local lease_id = ARGV[5]
            local kind = ARGV[6]
            local ttl_secs = tonumber(ARGV[7])
            local request_weight = tonumber(ARGV[8])
            local rpm = tonumber(ARGV[9])
            local phase_credit_max_ms = tonumber(ARGV[10])
            local phase_history_max_ms = tonumber(ARGV[11])
            local pacing_weight = request_weight

            if redis.call('SISMEMBER', KEYS[11], lease_id) == 1 then
                return {0, -1}
            end

            if max_age_ms > 0 then
                local expired = redis.call(
                    'ZRANGEBYSCORE', KEYS[1], '-inf', now - max_age_ms, 'LIMIT', 0, 64
                )
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
                local global_expired = redis.call(
                    'ZRANGEBYSCORE', KEYS[6], '-inf', now - max_age_ms, 'LIMIT', 0, 64
                )
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

            if global_max_count > 0 and (global_count + effective_weight) > global_max_count then
                return {4, global_count}
            end

            if max_count > 0 and (count + effective_weight) > max_count then
                return {3, count}
            end

            local interval_ms = 0
            local phase_at = 0
            local phase_credit_ms = 0
            local phase_history_ms = 0
            if rpm > 0 then
                interval_ms = math.max(1, math.floor(60000 / rpm))
                phase_credit_ms = math.min(
                    phase_credit_max_ms,
                    math.max(1, math.floor(interval_ms / 8))
                )
                phase_history_ms = math.min(
                    phase_history_max_ms,
                    math.max(1, math.floor(interval_ms / 2))
                )
                local available_at = tonumber(redis.call('GET', KEYS[12]) or '0')
                local stored_rpm = tonumber(redis.call('GET', KEYS[14]) or '0')
                if available_at > now then
                    if stored_rpm ~= rpm then
                        redis.call('DEL', KEYS[12], KEYS[13], KEYS[14], KEYS[15])
                    else
                        local finished_time = redis.call('TIME')
                        local finished_at = tonumber(finished_time[1]) * 1000
                            + math.floor(tonumber(finished_time[2]) / 1000)
                        return {2, available_at, math.max(1, available_at - finished_at), rpm, 0}
                    end
                else
                    redis.call('DEL', KEYS[12], KEYS[13], KEYS[14])
                end
                local phase_raw = redis.call('GET', KEYS[15]) or ''
                local phase_rpm_raw, phase_at_raw = string.match(phase_raw, '^(%d+):(%d+)$')
                if tonumber(phase_rpm_raw or '0') == rpm then
                    phase_at = tonumber(phase_at_raw or '0')
                else
                    redis.call('DEL', KEYS[15])
                end
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
            local next_at = 0
            if interval_ms > 0 then
                local pacing_span_ms = interval_ms * pacing_weight
                next_at = now + pacing_span_ms
                if phase_at > 0 and phase_at <= now then
                    local lateness_ms = now - phase_at
                    local phase_next_at = phase_at + pacing_span_ms
                    if lateness_ms <= phase_history_ms then
                        local minimum_next_at = now + math.max(1, pacing_span_ms - phase_credit_ms)
                        next_at = math.max(phase_next_at, minimum_next_at)
                    end
                end
                local remaining_ms = math.max(1, next_at - now)
                redis.call('SET', KEYS[12], tostring(next_at), 'PX', remaining_ms)
                redis.call('SET', KEYS[13], lease_id, 'PX', remaining_ms)
                redis.call('SET', KEYS[14], tostring(rpm), 'PX', remaining_ms)
                redis.call(
                    'SET',
                    KEYS[15],
                    tostring(rpm) .. ':' .. string.format('%.0f', next_at),
                    'PX',
                    remaining_ms + phase_history_ms
                )
                local finished_time = redis.call('TIME')
                local finished_at = tonumber(finished_time[1]) * 1000
                    + math.floor(tonumber(finished_time[2]) / 1000)
                return {
                    1,
                    count + effective_weight,
                    next_at,
                    math.max(1, next_at - finished_at),
                    rpm,
                    0
                }
            else
                redis.call('DEL', KEYS[12], KEYS[13], KEYS[14], KEYS[15])
            end
            return {1, count + effective_weight, 0, 0, 0, 0}
        "#;
        let keys = in_flight_keys(credential_id);
        let global_keys = global_in_flight_keys();
        let mut manager = self.scheduler_admission_manager();
        #[cfg(test)]
        {
            let delay_ms = self
                .scheduler_admission_pre_eval_delay_ms
                .swap(0, std::sync::atomic::Ordering::AcqRel);
            if delay_ms > 0 {
                tokio::time::sleep(StdDuration::from_millis(delay_ms)).await;
            }
        }
        #[cfg(test)]
        self.scheduler_admission_eval_count
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        let result: Vec<i64> = redis::cmd("EVAL")
            .arg(script)
            .arg(15)
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
            .arg(self.key(scheduler_rate_limit_key(credential_id)))
            .arg(self.key(scheduler_rate_limit_owner_key(credential_id)))
            .arg(self.key(scheduler_rate_limit_rpm_key(credential_id)))
            .arg(self.key(scheduler_rate_limit_phase_key(credential_id)))
            .arg(0i64)
            .arg(max_age_ms)
            .arg(max_concurrent_requests)
            .arg(global_max_concurrent_requests)
            .arg(lease_id.to_string())
            .arg(kind)
            .arg(ttl_secs)
            .arg(request_weight_units)
            .arg(rpm)
            .arg(SCHEDULER_RATE_LIMIT_PHASE_CREDIT_MAX_MS)
            .arg(SCHEDULER_RATE_LIMIT_PHASE_HISTORY_MAX_MS)
            .query_async(&mut manager)
            .await?;
        #[cfg(test)]
        {
            let delay_ms = self
                .scheduler_admission_post_eval_delay_ms
                .swap(0, std::sync::atomic::Ordering::AcqRel);
            if delay_ms > 0 {
                tokio::time::sleep(StdDuration::from_millis(delay_ms)).await;
            }
        }
        match result.first().copied().unwrap_or(0) {
            1 => Ok(SchedulerDispatchAdmission::Acquired {
                in_flight_count: result.get(1).copied().unwrap_or(1).max(0) as usize,
                rate_limit_available_at_ms: result.get(2).copied().filter(|value| *value > 0),
                rate_limit_remaining_ms: result
                    .get(3)
                    .copied()
                    .filter(|value| *value > 0)
                    .map(|value| value as u64),
                rate_limit_rpm: result
                    .get(4)
                    .copied()
                    .filter(|value| *value > 0)
                    .map(|value| value.min(u32::MAX as i64) as u32),
                rate_limit_owner_lease_id: result
                    .get(4)
                    .copied()
                    .filter(|value| *value > 0)
                    .map(|_| lease_id),
            }),
            2 => Ok(SchedulerDispatchAdmission::RateLimited {
                available_at_ms: result.get(1).copied().unwrap_or(0).max(0),
                remaining_ms: result.get(2).copied().unwrap_or(1).max(1) as u64,
                rpm: result
                    .get(3)
                    .copied()
                    .unwrap_or(rpm as i64)
                    .clamp(1, u32::MAX as i64) as u32,
                owner_lease_id: None,
            }),
            0 => Ok(SchedulerDispatchAdmission::LeaseCancelled),
            3 => Ok(SchedulerDispatchAdmission::CredentialCapacityFull),
            4 => Ok(SchedulerDispatchAdmission::GlobalCapacityFull),
            code => anyhow::bail!("unexpected Redis scheduler admission result code: {code}"),
        }
    }

    pub async fn acquire_external_pool_lease(
        &self,
        pool_id: u64,
        lease_id: u64,
        max_concurrent_requests: u32,
        global_max_concurrent_requests: u32,
        max_age: Option<StdDuration>,
    ) -> anyhow::Result<Option<usize>> {
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
            local lease_id = ARGV[4]
            local ttl_secs = tonumber(ARGV[5])

            local expired_tombstones = redis.call(
                'ZRANGEBYSCORE', KEYS[5], '-inf', now, 'LIMIT', 0, 256
            )
            if #expired_tombstones > 0 then
                redis.call('ZREM', KEYS[5], unpack(expired_tombstones))
            end
            if redis.call('ZSCORE', KEYS[5], lease_id) then
                return {0, redis.call('ZCARD', KEYS[1])}
            end

            if max_age_ms > 0 then
                local expired = redis.call(
                    'ZRANGEBYSCORE', KEYS[1], '-inf', now - max_age_ms, 'LIMIT', 0, 64
                )
                for _, member in ipairs(expired) do
                    redis.call('ZREM', KEYS[1], member)
                    redis.call('ZREM', KEYS[2], member)
                end
                local global_expired = redis.call(
                    'ZRANGEBYSCORE', KEYS[3], '-inf', now - max_age_ms, 'LIMIT', 0, 64
                )
                for _, member in ipairs(global_expired) do
                    redis.call('ZREM', KEYS[3], member)
                    redis.call('ZREM', KEYS[4], member)
                end
            end

            local count = redis.call('ZCARD', KEYS[1])
            if max_count > 0 and count >= max_count then
                return {0, count}
            end

            local global_count = redis.call('ZCARD', KEYS[3])
            if global_max_count > 0 and global_count >= global_max_count then
                return {0, global_count}
            end

            redis.call('ZADD', KEYS[1], now, lease_id)
            redis.call('ZADD', KEYS[2], now, lease_id)
            redis.call('ZADD', KEYS[3], now, lease_id)
            redis.call('ZADD', KEYS[4], now, lease_id)
            if ttl_secs > 0 then
                redis.call('EXPIRE', KEYS[1], ttl_secs)
                redis.call('EXPIRE', KEYS[2], ttl_secs)
                redis.call('EXPIRE', KEYS[3], ttl_secs)
                redis.call('EXPIRE', KEYS[4], ttl_secs)
            end
            return {1, count + 1}
        "#;
        let keys = external_pool_in_flight_keys(pool_id);
        let global_keys = external_pool_global_in_flight_keys();
        let mut manager = self.scheduler_capacity_manager();
        let result: Vec<i64> = redis::cmd("EVAL")
            .arg(script)
            .arg(5)
            .arg(self.key(&keys.last_seen))
            .arg(self.key(&keys.acquired))
            .arg(self.key(&global_keys.last_seen))
            .arg(self.key(&global_keys.acquired))
            .arg(self.key(&global_keys.released))
            .arg(max_age_ms)
            .arg(max_concurrent_requests)
            .arg(global_max_concurrent_requests)
            .arg(lease_id.to_string())
            .arg(ttl_secs)
            .query_async(&mut manager)
            .await?;
        if result.first().copied().unwrap_or(0) == 1 {
            Ok(Some(result.get(1).copied().unwrap_or(1).max(0) as usize))
        } else {
            Ok(None)
        }
    }

    #[cfg(test)]
    pub async fn release_in_flight_lease(
        &self,
        credential_id: u64,
        lease_id: u64,
    ) -> anyhow::Result<bool> {
        self.release_in_flight_lease_inner(credential_id, lease_id, false, false, None)
            .await
    }

    #[cfg(test)]
    pub async fn release_in_flight_lease_with_tombstone(
        &self,
        credential_id: u64,
        lease_id: u64,
    ) -> anyhow::Result<bool> {
        self.release_in_flight_lease_inner(credential_id, lease_id, true, false, None)
            .await
    }

    pub async fn release_in_flight_lease_and_publish_wakeup(
        &self,
        credential_id: u64,
        lease_id: u64,
        tombstone: bool,
        wakeup_payload: &str,
    ) -> anyhow::Result<bool> {
        self.release_in_flight_lease_inner(
            credential_id,
            lease_id,
            tombstone,
            false,
            Some(wakeup_payload),
        )
        .await
    }

    pub async fn rollback_dispatch_admission_and_publish_wakeup(
        &self,
        credential_id: u64,
        lease_id: u64,
        tombstone: bool,
        wakeup_payload: &str,
    ) -> anyhow::Result<bool> {
        self.release_in_flight_lease_inner(
            credential_id,
            lease_id,
            tombstone,
            true,
            Some(wakeup_payload),
        )
        .await
    }

    async fn release_in_flight_lease_inner(
        &self,
        credential_id: u64,
        lease_id: u64,
        tombstone: bool,
        rollback_rate_limit: bool,
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
            local rollback_rate_limit = tonumber(ARGV[5])

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
            local rate_limit_removed = 0
            if rollback_rate_limit == 1 and redis.call('GET', KEYS[14]) == lease_id then
                rate_limit_removed = redis.call('DEL', KEYS[13], KEYS[14], KEYS[15], KEYS[16])
            end
            if (removed > 0 or rate_limit_removed > 0) and ARGV[4] ~= '' then
                redis.call('PUBLISH', KEYS[12], ARGV[4])
            end
            return removed + rate_limit_removed
        "#;
        let tombstone_ttl_secs = SCHEDULER_DISTRIBUTED_LEASE_SAFETY_SECS as i64;
        let removed: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(16)
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
            .arg(self.key(scheduler_rate_limit_key(credential_id)))
            .arg(self.key(scheduler_rate_limit_owner_key(credential_id)))
            .arg(self.key(scheduler_rate_limit_rpm_key(credential_id)))
            .arg(self.key(scheduler_rate_limit_phase_key(credential_id)))
            .arg(&lease_id)
            .arg(if tombstone { 1 } else { 0 })
            .arg(tombstone_ttl_secs)
            .arg(wakeup_payload.unwrap_or_default())
            .arg(if rollback_rate_limit { 1 } else { 0 })
            .query_async(&mut manager)
            .await?;
        Ok(removed > 0)
    }

    pub async fn release_external_pool_lease(
        &self,
        pool_id: u64,
        lease_id: u64,
    ) -> anyhow::Result<bool> {
        Ok(self
            .release_external_pool_leases(pool_id, &[lease_id])
            .await?
            > 0)
    }

    pub async fn release_external_pool_leases(
        &self,
        pool_id: u64,
        lease_ids: &[u64],
    ) -> anyhow::Result<usize> {
        if lease_ids.is_empty() {
            return Ok(0);
        }
        let keys = external_pool_in_flight_keys(pool_id);
        let global_keys = external_pool_global_in_flight_keys();
        let lease_ids = lease_ids.iter().map(u64::to_string).collect::<Vec<_>>();
        let mut manager = self.scheduler_capacity_manager();
        let removed: i64 = redis::cmd("EVAL")
            .arg(EXTERNAL_POOL_LEASE_RELEASE_SCRIPT)
            .arg(5)
            .arg(self.key(&keys.last_seen))
            .arg(self.key(&keys.acquired))
            .arg(self.key(&global_keys.last_seen))
            .arg(self.key(&global_keys.acquired))
            .arg(self.key(&global_keys.released))
            .arg(EXTERNAL_POOL_RELEASE_TOMBSTONE_TTL_SECS)
            .arg(&lease_ids)
            .query_async(&mut manager)
            .await?;
        Ok(removed.max(0) as usize)
    }

    pub async fn touch_external_pool_lease(
        &self,
        pool_id: u64,
        lease_id: u64,
        ttl_secs: usize,
    ) -> anyhow::Result<bool> {
        let keys = external_pool_in_flight_keys(pool_id);
        let global_keys = external_pool_global_in_flight_keys();
        let lease_id = lease_id.to_string();
        let script = r#"
            local lease_id = ARGV[1]
            local redis_time = redis.call('TIME')
            local now = tonumber(redis_time[1]) * 1000 + math.floor(tonumber(redis_time[2]) / 1000)
            local ttl_secs = tonumber(ARGV[2])

            if not redis.call('ZSCORE', KEYS[2], lease_id) then
                return 0
            end
            if not redis.call('ZSCORE', KEYS[4], lease_id) then
                return 0
            end

            redis.call('ZADD', KEYS[1], now, lease_id)
            redis.call('ZADD', KEYS[3], now, lease_id)
            if ttl_secs > 0 then
                redis.call('EXPIRE', KEYS[1], ttl_secs)
                redis.call('EXPIRE', KEYS[2], ttl_secs)
                redis.call('EXPIRE', KEYS[3], ttl_secs)
                redis.call('EXPIRE', KEYS[4], ttl_secs)
            end
            return 1
        "#;
        let mut manager = self.scheduler_capacity_manager();
        let touched: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(4)
            .arg(self.key(&keys.last_seen))
            .arg(self.key(&keys.acquired))
            .arg(self.key(&global_keys.last_seen))
            .arg(self.key(&global_keys.acquired))
            .arg(lease_id)
            .arg(ttl_secs.max(1))
            .query_async(&mut manager)
            .await?;
        Ok(touched == 1)
    }

    pub async fn external_pool_capacity_state(
        &self,
        pool_id: u64,
        max_age: Option<StdDuration>,
    ) -> anyhow::Result<ExternalPoolCapacityState> {
        let max_age_ms = max_age.map(|age| age.as_millis() as i64).unwrap_or(0);
        let script = r#"
            local redis_time = redis.call('TIME')
            local now = tonumber(redis_time[1]) * 1000 + math.floor(tonumber(redis_time[2]) / 1000)
            local max_age_ms = tonumber(ARGV[1])
            if max_age_ms > 0 then
                local expired = redis.call('ZRANGEBYSCORE', KEYS[1], '-inf', now - max_age_ms)
                for _, member in ipairs(expired) do
                    redis.call('ZREM', KEYS[1], member)
                    redis.call('ZREM', KEYS[2], member)
                end
                local global_expired = redis.call('ZRANGEBYSCORE', KEYS[3], '-inf', now - max_age_ms)
                for _, member in ipairs(global_expired) do
                    redis.call('ZREM', KEYS[3], member)
                    redis.call('ZREM', KEYS[4], member)
                end
            end
            return {redis.call('ZCARD', KEYS[1]), redis.call('ZCARD', KEYS[3])}
        "#;
        let keys = external_pool_in_flight_keys(pool_id);
        let global_keys = external_pool_global_in_flight_keys();
        let mut manager = self.scheduler_capacity_manager();
        let result: Vec<i64> = redis::cmd("EVAL")
            .arg(script)
            .arg(4)
            .arg(self.key(&keys.last_seen))
            .arg(self.key(&keys.acquired))
            .arg(self.key(&global_keys.last_seen))
            .arg(self.key(&global_keys.acquired))
            .arg(max_age_ms)
            .query_async(&mut manager)
            .await?;
        Ok(ExternalPoolCapacityState {
            pool_in_flight_requests: result.first().copied().unwrap_or(0).max(0) as u32,
            global_in_flight_requests: result.get(1).copied().unwrap_or(0).max(0) as u32,
        })
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
        ttl_secs: u64,
    ) -> anyhow::Result<()> {
        let keys = in_flight_keys(credential_id);
        let global_keys = global_in_flight_keys();
        let mut manager = self.scheduler_capacity_manager();
        let lease_id = lease_id.to_string();
        let script = r#"
            local redis_time = redis.call('TIME')
            local now = tonumber(redis_time[1]) * 1000 + math.floor(tonumber(redis_time[2]) / 1000)
            local lease_id = ARGV[1]
            local ttl_secs = tonumber(ARGV[2])

            if not redis.call('ZSCORE', KEYS[2], lease_id)
                or not redis.call('ZSCORE', KEYS[7], lease_id)
            then
                redis.call('ZREM', KEYS[1], lease_id)
                redis.call('ZREM', KEYS[6], lease_id)
                return 0
            end

            redis.call('ZADD', KEYS[1], now, lease_id)
            redis.call('ZADD', KEYS[6], now, lease_id)
            if ttl_secs > 0 then
                for index = 1, 10 do
                    redis.call('EXPIRE', KEYS[index], ttl_secs)
                end
            end
            return 1
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
            .arg(&lease_id)
            .arg(ttl_secs)
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
            local redis_time = redis.call('TIME')
            local now = tonumber(redis_time[1]) * 1000 + math.floor(tonumber(redis_time[2]) / 1000)
            local lease_id = ARGV[1]
            local kind = ARGV[2]

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
        let max_age_ms = max_age.as_millis() as i64;
        let script = r#"
            local redis_time = redis.call('TIME')
            local now = tonumber(redis_time[1]) * 1000 + math.floor(tonumber(redis_time[2]) / 1000)
            local max_age_ms = tonumber(ARGV[1])
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
            let min_idle_ms = min_idle.as_millis() as i64;
            let script = r#"
                local redis_time = redis.call('TIME')
                local now = tonumber(redis_time[1]) * 1000 + math.floor(tonumber(redis_time[2]) / 1000)
                local cutoff = now - tonumber(ARGV[1])
                local expired = redis.call('ZRANGEBYSCORE', KEYS[1], '-inf', cutoff)
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
                .arg(min_idle_ms)
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
        #[cfg(test)]
        self.scheduler_state_snapshot_count
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        let mut states = HashMap::with_capacity(credential_ids.len());
        if credential_ids.is_empty() {
            return Ok(states);
        }

        let query_now = now_ms();
        let mut pipe = redis::pipe();
        for credential_id in credential_ids {
            let keys = in_flight_keys(*credential_id);
            pipe.cmd("GET")
                .arg(self.key(scheduler_cooldown_key(*credential_id)))
                .cmd("GET")
                .arg(self.key(scheduler_health_key(*credential_id)))
                .cmd("MGET")
                .arg(&[
                    self.key(scheduler_rate_limit_key(*credential_id)),
                    self.key(scheduler_rate_limit_rpm_key(*credential_id)),
                    self.key(scheduler_rate_limit_owner_key(*credential_id)),
                ])
                .cmd("ZRANGE")
                .arg(self.key(&keys.last_seen))
                .arg(0)
                .arg(-1)
                .arg("WITHSCORES")
                .cmd("ZRANGE")
                .arg(self.key(&keys.acquired))
                .arg(0)
                .arg(-1)
                .arg("WITHSCORES")
                .cmd("HGETALL")
                .arg(self.key(&keys.kind))
                .cmd("HGETALL")
                .arg(self.key(&keys.weight))
                .cmd("ZCOUNT")
                .arg(self.key(scheduler_selection_window_key(*credential_id)))
                .arg(query_now - 10_000)
                .arg("+inf")
                .cmd("ZCOUNT")
                .arg(self.key(scheduler_selection_window_key(*credential_id)))
                .arg(query_now - 60_000)
                .arg("+inf")
                .cmd("ZCOUNT")
                .arg(self.key(scheduler_selection_window_key(*credential_id)))
                .arg(query_now - 5 * 60_000)
                .arg("+inf")
                .cmd("HGETALL")
                .arg(self.key(scheduler_model_index_key(*credential_id)));
        }
        pipe.cmd("TIME");

        let mut manager = self.scheduler_manager();
        let values: Vec<redis::Value> = pipe.query_async(&mut manager).await?;
        let snapshot_processing_started_at = std::time::Instant::now();
        let redis_time: Vec<String> = redis::from_redis_value(
            values
                .last()
                .ok_or_else(|| anyhow::anyhow!("Redis 调度快照返回为空"))?,
        )?;
        let redis_now = redis_time
            .first()
            .and_then(|seconds| seconds.parse::<i64>().ok())
            .and_then(|seconds| {
                redis_time
                    .get(1)
                    .and_then(|micros| micros.parse::<i64>().ok())
                    .map(|micros| seconds.saturating_mul(1_000) + micros / 1_000)
            })
            .ok_or_else(|| anyhow::anyhow!("Redis TIME 返回格式无效"))?;
        let mut values_to_compare_delete = Vec::new();
        let mut rate_limits_to_delete = Vec::new();
        let mut indexed_models: Vec<(u64, String, String)> = Vec::new();
        for (index, credential_id) in credential_ids.iter().enumerate() {
            let base = index * 11;
            let cooldown_raw: Option<String> = redis::from_redis_value(&values[base])?;
            let health_raw: Option<String> = redis::from_redis_value(&values[base + 1])?;
            let rate_limit_values: Vec<Option<String>> =
                redis::from_redis_value(&values[base + 2])?;
            let rate_raw = rate_limit_values.first().cloned().flatten();
            let rate_rpm_raw = rate_limit_values.get(1).cloned().flatten();
            let rate_owner_raw = rate_limit_values.get(2).cloned().flatten();
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

            let cooldown = cooldown_raw.as_deref().and_then(|raw| {
                serde_json::from_str::<SchedulerCooldownState>(raw)
                    .ok()
                    .and_then(|state| {
                        if state.until_ms <= redis_now {
                            values_to_compare_delete
                                .push((scheduler_cooldown_key(*credential_id), raw.to_string()));
                            None
                        } else {
                            Some(state)
                        }
                    })
            });
            let parsed_rate_limit_available_at_ms =
                rate_raw.as_deref().and_then(|raw| raw.parse::<i64>().ok());
            let parsed_rate_limit_rpm = rate_rpm_raw
                .as_deref()
                .and_then(|raw| raw.parse::<u32>().ok())
                .filter(|rpm| *rpm > 0);
            let parsed_rate_limit_owner_lease_id = rate_owner_raw
                .as_deref()
                .and_then(|raw| raw.parse::<u64>().ok());
            let rate_limit_is_valid = parsed_rate_limit_available_at_ms
                .is_some_and(|until_ms| until_ms > redis_now)
                && parsed_rate_limit_rpm.is_some()
                && parsed_rate_limit_owner_lease_id.is_some();
            let (
                rate_limit_available_at_ms,
                rate_limit_remaining_ms,
                rate_limit_rpm,
                rate_limit_owner_lease_id,
            ) = if rate_limit_is_valid {
                let redis_until = parsed_rate_limit_available_at_ms.expect("validated above");
                let remaining_ms = redis_until.saturating_sub(redis_now);
                (
                    Some(redis_until),
                    Some(remaining_ms as u64),
                    parsed_rate_limit_rpm,
                    parsed_rate_limit_owner_lease_id,
                )
            } else {
                if rate_raw.is_some() || rate_rpm_raw.is_some() || rate_owner_raw.is_some() {
                    rate_limits_to_delete.push((
                        *credential_id,
                        rate_raw.unwrap_or_default(),
                        rate_rpm_raw.unwrap_or_default(),
                        rate_owner_raw.unwrap_or_default(),
                    ));
                }
                (None, None, None, None)
            };
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
                    rate_limit_remaining_ms,
                    rate_limit_rpm,
                    rate_limit_owner_lease_id,
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
                let cooldown = cooldown_raw.as_deref().and_then(|raw| {
                    serde_json::from_str::<SchedulerCooldownState>(raw)
                        .ok()
                        .and_then(|mut state| {
                            if state.until_ms <= redis_now {
                                values_to_compare_delete.push((
                                    scheduler_model_cooldown_key(credential_id, &hash),
                                    raw.to_string(),
                                ));
                                None
                            } else {
                                if state.model.is_none() {
                                    state.model = Some(model.clone());
                                }
                                Some(state)
                            }
                        })
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
        if !rate_limits_to_delete.is_empty() {
            let script = r#"
                local removed = 0
                for key_index = 1, #KEYS, 3 do
                    local expected_deadline = ARGV[key_index]
                    local expected_rpm = ARGV[key_index + 1]
                    local expected_owner = ARGV[key_index + 2]
                    local current_deadline = redis.call('GET', KEYS[key_index]) or ''
                    local current_rpm = redis.call('GET', KEYS[key_index + 1]) or ''
                    local current_owner = redis.call('GET', KEYS[key_index + 2]) or ''
                    if current_deadline == expected_deadline
                        and current_rpm == expected_rpm
                        and current_owner == expected_owner then
                        removed = removed + redis.call(
                            'DEL',
                            KEYS[key_index],
                            KEYS[key_index + 1],
                            KEYS[key_index + 2]
                        )
                    end
                end
                return removed
            "#;
            let mut command = redis::cmd("EVAL");
            command.arg(script).arg(rate_limits_to_delete.len() * 3);
            for (credential_id, _, _, _) in &rate_limits_to_delete {
                command
                    .arg(self.key(scheduler_rate_limit_key(*credential_id)))
                    .arg(self.key(scheduler_rate_limit_rpm_key(*credential_id)))
                    .arg(self.key(scheduler_rate_limit_owner_key(*credential_id)));
            }
            for (_, deadline, rpm, owner) in &rate_limits_to_delete {
                command.arg(deadline).arg(rpm).arg(owner);
            }
            let _: i64 = command.query_async(&mut manager).await?;
        }
        if !values_to_compare_delete.is_empty() {
            let script = r#"
                local removed = 0
                for index = 1, #KEYS do
                    if redis.call('GET', KEYS[index]) == ARGV[index] then
                        removed = removed + redis.call('DEL', KEYS[index])
                    end
                end
                return removed
            "#;
            let mut command = redis::cmd("EVAL");
            command.arg(script).arg(values_to_compare_delete.len());
            for (key, _) in &values_to_compare_delete {
                command.arg(self.key(key));
            }
            for (_, expected_value) in &values_to_compare_delete {
                command.arg(expected_value);
            }
            let _: i64 = command.query_async(&mut manager).await?;
        }
        let snapshot_elapsed_ms = snapshot_processing_started_at
            .elapsed()
            .as_millis()
            .min(u64::MAX as u128) as u64;
        for state in states.values_mut() {
            state.rate_limit_remaining_ms = state
                .rate_limit_remaining_ms
                .map(|remaining_ms| remaining_ms.saturating_sub(snapshot_elapsed_ms));
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
        let mut manager = self.scheduler_admission_manager();
        let admitted: i64 = redis::cmd("EVAL")
            .arg(LOCAL_DISPATCH_QUEUE_ADMIT_SCRIPT)
            .arg(2)
            .arg(self.key(scheduler_global_queue_key()))
            .arg(self.key(scheduler_global_queue_released_key()))
            .arg(max_queued)
            .arg(ttl_secs.max(60))
            .arg(lease_id)
            .query_async(&mut manager)
            .await?;
        Ok(admitted == 1)
    }

    #[cfg(test)]
    pub async fn leave_dispatch_queue(&self, lease_id: &str) -> anyhow::Result<bool> {
        self.leave_dispatch_queue_with_tombstone(lease_id, 120)
            .await
    }

    pub async fn leave_dispatch_queue_with_tombstone(
        &self,
        lease_id: &str,
        ttl_secs: u64,
    ) -> anyhow::Result<bool> {
        let mut manager = self.scheduler_capacity_manager();
        let removed: i64 = redis::cmd("EVAL")
            .arg(LOCAL_DISPATCH_QUEUE_RELEASE_SCRIPT)
            .arg(2)
            .arg(self.key(scheduler_global_queue_key()))
            .arg(self.key(scheduler_global_queue_released_key()))
            .arg(lease_id)
            .arg(ttl_secs.max(60))
            .query_async(&mut manager)
            .await?;
        Ok(removed > 0)
    }

    pub async fn renew_dispatch_queue(
        &self,
        lease_id: &str,
        ttl_secs: u64,
    ) -> anyhow::Result<bool> {
        let mut manager = self.scheduler_admission_manager();
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

    pub async fn try_enter_external_pool_dispatch_queue(
        &self,
        lease_id: &str,
        max_queued: u32,
        ttl_secs: u64,
    ) -> anyhow::Result<bool> {
        let mut manager = self.scheduler_capacity_manager();
        let admitted: i64 = redis::cmd("EVAL")
            .arg(EXTERNAL_DISPATCH_QUEUE_ADMIT_SCRIPT)
            .arg(2)
            .arg(self.key(external_pool_global_queue_key()))
            .arg(self.key(external_pool_global_queue_released_key()))
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
        Ok(self
            .leave_external_pool_dispatch_queue_leases(&[lease_id])
            .await?
            > 0)
    }

    pub async fn leave_external_pool_dispatch_queue_leases(
        &self,
        lease_ids: &[&str],
    ) -> anyhow::Result<usize> {
        if lease_ids.is_empty() {
            return Ok(0);
        }
        let mut manager = self.scheduler_capacity_manager();
        let removed: i64 = redis::cmd("EVAL")
            .arg(EXTERNAL_DISPATCH_QUEUE_RELEASE_SCRIPT)
            .arg(2)
            .arg(self.key(external_pool_global_queue_key()))
            .arg(self.key(external_pool_global_queue_released_key()))
            .arg(EXTERNAL_POOL_RELEASE_TOMBSTONE_TTL_SECS)
            .arg(lease_ids)
            .query_async(&mut manager)
            .await?;
        Ok(removed.max(0) as usize)
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

fn collect_dashboard_cache_read_bucket_keys(
    suffixes: &mut Vec<String>,
    seen: &mut HashSet<String>,
    spec: &UsageDashboardWindowSpec,
) {
    for epoch in usage_dashboard_hour_epochs(spec.from, spec.to) {
        push_dashboard_bucket_suffix(suffixes, seen, usage_dashboard_cache_read_bucket_key(epoch));
    }
}

fn collect_dashboard_dimension_bucket_keys(
    suffixes: &mut Vec<String>,
    seen: &mut HashSet<String>,
    spec: &UsageDashboardWindowSpec,
    dimension: &str,
    keys: &[&str],
) {
    for epoch in usage_dashboard_hour_epochs(spec.from, spec.to) {
        for key in keys {
            push_dashboard_bucket_suffix(
                suffixes,
                seen,
                usage_dashboard_bucket_key(dimension, key, epoch),
            );
        }
    }
}

fn collect_dashboard_external_pool_bucket_keys(
    suffixes: &mut Vec<String>,
    seen: &mut HashSet<String>,
    spec: &UsageDashboardWindowSpec,
    external_pool_index: &[RedisExternalPoolIndexItem],
) {
    for epoch in usage_dashboard_hour_epochs(spec.from, spec.to) {
        for pool in external_pool_index {
            push_dashboard_bucket_suffix(
                suffixes,
                seen,
                usage_dashboard_bucket_key("external_pool", &pool.id, epoch),
            );
        }
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

fn session_binding_revision_key(session_id: &str) -> String {
    format!("scheduler:session_revision:{}", session_hash(session_id))
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

fn scheduler_rate_limit_owner_key(credential_id: u64) -> String {
    format!("scheduler:rate_limit_owner:{}", credential_id)
}

fn scheduler_rate_limit_rpm_key(credential_id: u64) -> String {
    format!("scheduler:rate_limit_rpm:{}", credential_id)
}

fn scheduler_rate_limit_phase_key(credential_id: u64) -> String {
    format!("scheduler:rate_limit_phase:{}", credential_id)
}

fn scheduler_global_queue_key() -> &'static str {
    "scheduler:global:queue_leases:v1"
}

fn scheduler_global_queue_released_key() -> &'static str {
    "scheduler:global:queue_released:v1"
}

fn external_pool_global_queue_key() -> &'static str {
    "external_pool:global:queue_leases:v1"
}

fn external_pool_global_queue_released_key() -> &'static str {
    "external_pool:global:queue_released:v1"
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

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use redis::AsyncCommands;
    use serde::{Deserialize, Serialize};

    use super::*;
    use crate::model::config::Config;

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    struct CachedValue {
        value: String,
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
    async fn redis_usage_writer_materializes_snapshots_without_aggregates() {
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
        let mut error = usage_record(
            "redis-usage-error",
            UsageRecordStatus::Error,
            UsageSource::LocalPromptCache,
            100,
            0.20,
            50,
        );

        store.record_usage_record_snapshot(&success).await.unwrap();
        store.record_usage_record_snapshot(&error).await.unwrap();
        error.duration_ms = 75;
        store.record_usage_record_snapshot(&error).await.unwrap();

        assert!(store.usage_summary(500).await.unwrap().is_none());
        assert!(
            store
                .usage_dashboard(Some("UTC"), 500)
                .await
                .unwrap()
                .is_none()
        );
        let mut manager = store.manager.clone();
        let summary_exists: bool = manager
            .exists(store.key(USAGE_SUMMARY_TOTALS_KEY))
            .await
            .unwrap();
        assert!(!summary_exists);
        let legacy_seen_exists: bool = manager
            .exists(store.key(format!(
                "usage:summary:seen:{}",
                usage_dimension_hash(&error.id)
            )))
            .await
            .unwrap();
        assert!(!legacy_seen_exists);
        let dashboard_exists: bool = manager
            .exists(store.key(usage_dashboard_bucket_key(
                "global",
                "all",
                usage_dashboard_hour_start(Utc::now()).timestamp(),
            )))
            .await
            .unwrap();
        assert!(!dashboard_exists);

        let records = store
            .usage_records_page(UsageRecordQuery::default(), 1, 10)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(records.records.len(), 2);
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
        assert_eq!(filtered.records[0].duration_ms, 75);

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
    async fn redis_scheduler_session_binding_rejects_stale_cross_instance_write() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store_a = RedisStore::connect(&config).await.unwrap();
        let store_b = RedisStore::connect(&config).await.unwrap();
        let session_id = format!("session-cas-{}", uuid::Uuid::new_v4());
        let newer_at = Utc::now();
        let newer = SchedulerSessionBinding {
            credential_id: 8,
            last_used_at: newer_at,
            soft_failure_count: 0,
        };
        let stale = SchedulerSessionBinding {
            credential_id: 7,
            last_used_at: newer_at - ChronoDuration::seconds(1),
            soft_failure_count: 0,
        };

        store_b
            .set_session_binding(&session_id, &newer, 60)
            .await
            .unwrap();
        let authoritative = store_a
            .set_session_binding(&session_id, &stale, 60)
            .await
            .unwrap()
            .expect("newer binding should remain authoritative");
        assert_eq!(authoritative.credential_id, newer.credential_id);
        assert_eq!(authoritative.last_used_at, newer.last_used_at);
        assert_eq!(
            store_a
                .get_session_binding(&session_id)
                .await
                .unwrap()
                .unwrap(),
            newer
        );

        let session_hash = session_hash(&session_id);
        let mut manager = store_a.scheduler_manager();
        let stale_indexed: bool = manager
            .sismember(
                store_a.key(sessions_by_credential_key(stale.credential_id)),
                &session_hash,
            )
            .await
            .unwrap();
        let current_indexed: bool = manager
            .sismember(
                store_a.key(sessions_by_credential_key(newer.credential_id)),
                &session_hash,
            )
            .await
            .unwrap();
        assert!(!stale_indexed);
        assert!(current_indexed);
        let future_revision = (newer_at + ChronoDuration::seconds(30)).timestamp_micros();
        let revision_key = store_a.key(session_binding_revision_key(&session_id));
        let _: () = manager
            .set_ex(&revision_key, future_revision, 60)
            .await
            .unwrap();
        store_a.delete_session_binding(&session_id).await.unwrap();
        let tombstone_revision: i64 = manager.get(&revision_key).await.unwrap();
        assert_eq!(
            tombstone_revision, future_revision,
            "a delete tombstone must not move an existing revision backwards"
        );
        assert!(
            store_b
                .set_session_binding(&session_id, &stale, 60)
                .await
                .unwrap()
                .is_none(),
            "a delayed write must not resurrect a binding after a newer delete tombstone"
        );
        assert!(
            store_a
                .get_session_binding(&session_id)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn redis_scheduler_conditional_delete_fences_absent_binding() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let session_id = format!("session-conditional-fence-{}", uuid::Uuid::new_v4());
        let credential_id = 17;
        let stale = SchedulerSessionBinding {
            credential_id,
            last_used_at: Utc::now() - ChronoDuration::seconds(1),
            soft_failure_count: 0,
        };

        assert!(
            store
                .get_session_binding(&session_id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .delete_session_binding_if_bound_to(&session_id, credential_id)
                .await
                .unwrap(),
            "an absent binding must still be fenced against an older queued write"
        );

        let revision_key = store.key(session_binding_revision_key(&session_id));
        let mut manager = store.scheduler_manager();
        let tombstone_revision: i64 = manager.get(&revision_key).await.unwrap();
        assert!(tombstone_revision > stale.last_used_at.timestamp_micros());
        let tombstone_ttl_secs: i64 = manager.ttl(&revision_key).await.unwrap();
        assert!(
            tombstone_ttl_secs > 0,
            "conditional delete must retain a revision tombstone"
        );
        assert!(
            store
                .set_session_binding(&session_id, &stale, 60)
                .await
                .unwrap()
                .is_none(),
            "a delayed write must not create a binding after an absent conditional delete"
        );
        assert!(
            store
                .get_session_binding(&session_id)
                .await
                .unwrap()
                .is_none()
        );
        let indexed: bool = manager
            .sismember(
                store.key(sessions_by_credential_key(credential_id)),
                session_hash(&session_id),
            )
            .await
            .unwrap();
        assert!(!indexed);

        let future_revision = tombstone_revision + 30_000_000;
        let _: () = manager
            .set_ex(&revision_key, future_revision, 60)
            .await
            .unwrap();
        assert!(
            store
                .delete_session_binding_if_bound_to(&session_id, credential_id)
                .await
                .unwrap()
        );
        let preserved_revision: i64 = manager.get(&revision_key).await.unwrap();
        assert_eq!(
            preserved_revision, future_revision,
            "an absent conditional delete must not move an existing revision backwards"
        );

        let mismatch_session = format!("session-conditional-other-{}", uuid::Uuid::new_v4());
        let authoritative = SchedulerSessionBinding {
            credential_id: 23,
            last_used_at: Utc::now() - ChronoDuration::seconds(2),
            soft_failure_count: 0,
        };
        let queued_target = SchedulerSessionBinding {
            credential_id,
            last_used_at: Utc::now() - ChronoDuration::seconds(1),
            soft_failure_count: 0,
        };
        store
            .set_session_binding(&mismatch_session, &authoritative, 60)
            .await
            .unwrap();
        assert!(
            !store
                .delete_session_binding_if_bound_to(&mismatch_session, credential_id)
                .await
                .unwrap(),
            "conditional delete must preserve a binding owned by another credential"
        );
        assert_eq!(
            store
                .set_session_binding(&mismatch_session, &queued_target, 60)
                .await
                .unwrap(),
            Some(authoritative.clone()),
            "the mismatch branch must fence a target write queued before the delete"
        );
        assert_eq!(
            store.get_session_binding(&mismatch_session).await.unwrap(),
            Some(authoritative)
        );
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
        let malformed_session_id = "session-malformed-scalar";
        let malformed_session_hash = session_hash(malformed_session_id);
        let malformed_session_key = store.key(session_binding_key(malformed_session_id));
        let old_reverse_index = store.key(sessions_by_credential_key(7));
        let mut manager = store.scheduler_manager();
        let _: () = manager
            .set_ex(&malformed_session_key, r#""malformed""#, 60)
            .await
            .unwrap();
        let _: usize = manager
            .sadd(&old_reverse_index, &malformed_session_hash)
            .await
            .unwrap();
        assert_eq!(store.delete_sessions_for_credential(7).await.unwrap(), 0);
        let malformed_still_exists: bool = manager.exists(&malformed_session_key).await.unwrap();
        assert!(
            malformed_still_exists,
            "cleanup must not delete a syntactically valid scalar binding with no credential id"
        );
        let old_reverse_index_size: usize = manager.scard(&old_reverse_index).await.unwrap();
        assert_eq!(old_reverse_index_size, 0);
        let _: usize = manager.del(&malformed_session_key).await.unwrap();
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
            .unwrap()
            .expect("same-credential binding should remain present");
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

        let expected_channel = store.external_pools_changed_channel();
        store
            .publish_external_pools_changed(r#"{"kind":"external_pools_changed"}"#)
            .await
            .unwrap();
        let message = tokio::time::timeout(StdDuration::from_secs(2), stream.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.get_channel_name(), expected_channel);
        assert_eq!(
            message.get_payload::<String>().unwrap(),
            r#"{"kind":"external_pools_changed"}"#
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

        let selected_once = store.record_scheduler_selection(3, 1).await.unwrap();
        assert_eq!(selected_once.selection_count, 1);
        assert_eq!(selected_once.recent_selection_count_10s, 1);
        assert_eq!(selected_once.recent_selection_count_60s, 1);
        assert_eq!(selected_once.recent_selection_count_5m, 1);
        let selected_twice = store.record_scheduler_selection(3, 1).await.unwrap();
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
        let selected_weighted = store.record_scheduler_selection(5, 4).await.unwrap();
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
    async fn redis_local_admission_isolated_from_capacity_maintenance_connection() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let blocking_key = store.key("test:capacity-maintenance-block");
        let mut maintenance = store.scheduler_capacity_manager();
        let blocked = tokio::spawn(async move {
            let _: Option<(String, String)> = redis::cmd("BLPOP")
                .arg(blocking_key)
                .arg(1)
                .query_async(&mut maintenance)
                .await
                .unwrap();
        });
        tokio::time::sleep(StdDuration::from_millis(50)).await;

        let credential_id = 9_001;
        let lease_id = 7_001;
        let acquired = tokio::time::timeout(
            StdDuration::from_millis(500),
            store.acquire_dispatch_lease(
                credential_id,
                lease_id,
                1,
                1,
                1,
                Some(StdDuration::from_secs(60)),
                "api",
            ),
        )
        .await
        .expect("local admission must not queue behind capacity maintenance")
        .unwrap();
        assert!(acquired.is_some());

        assert!(
            store
                .release_in_flight_lease(credential_id, lease_id)
                .await
                .unwrap()
        );
        blocked.await.unwrap();
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
    async fn redis_dispatch_queue_cleanup_tombstone_blocks_late_admission_commit() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let blocking_key = store.key(format!(
            "test:queue-admission-block:{}",
            uuid::Uuid::new_v4()
        ));
        let mut admission = store.scheduler_admission_manager();
        let blocked = tokio::spawn(async move {
            let _: Option<(String, String)> = redis::cmd("BLPOP")
                .arg(blocking_key)
                .arg(1)
                .query_async(&mut admission)
                .await
                .unwrap();
        });
        tokio::time::sleep(StdDuration::from_millis(50)).await;

        let delayed_store = store.clone();
        let delayed_admission = tokio::spawn(async move {
            delayed_store
                .try_enter_dispatch_queue("late-commit", 1, 60)
                .await
        });
        tokio::time::sleep(StdDuration::from_millis(50)).await;

        assert!(
            !store
                .leave_dispatch_queue_with_tombstone("late-commit", 60)
                .await
                .unwrap(),
            "cleanup runs before the delayed admission command reaches Redis"
        );
        blocked.await.unwrap();
        assert!(
            !delayed_admission.await.unwrap().unwrap(),
            "a cleanup tombstone must reject the late admission commit"
        );
        assert_eq!(
            store.global_capacity_state().await.unwrap().queued_requests,
            0
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
    async fn redis_external_pool_batch_release_is_grouped_and_idempotent() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        for lease_id in [101, 102] {
            assert!(
                store
                    .acquire_external_pool_lease(
                        11,
                        lease_id,
                        10,
                        10,
                        Some(StdDuration::from_secs(60)),
                    )
                    .await
                    .unwrap()
                    .is_some()
            );
        }
        assert!(
            store
                .acquire_external_pool_lease(22, 201, 10, 10, Some(StdDuration::from_secs(60)),)
                .await
                .unwrap()
                .is_some()
        );

        assert_eq!(
            store
                .release_external_pool_leases(11, &[101, 102])
                .await
                .unwrap(),
            2
        );
        let first_pool = store.external_pool_capacity_state(11, None).await.unwrap();
        let second_pool = store.external_pool_capacity_state(22, None).await.unwrap();
        assert_eq!(first_pool.pool_in_flight_requests, 0);
        assert_eq!(first_pool.global_in_flight_requests, 1);
        assert_eq!(second_pool.pool_in_flight_requests, 1);
        assert_eq!(second_pool.global_in_flight_requests, 1);
        assert_eq!(
            store
                .release_external_pool_leases(11, &[101, 102])
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .release_external_pool_leases(22, &[201])
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .external_pool_capacity_state(22, None)
                .await
                .unwrap()
                .global_in_flight_requests,
            0
        );
    }

    #[tokio::test]
    async fn redis_external_pool_release_tombstone_blocks_late_acquire() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let pool_id = 31;
        let released_lease_id = 301;
        assert_eq!(
            store
                .release_external_pool_leases(pool_id, &[released_lease_id])
                .await
                .unwrap(),
            0,
            "release-before-acquire must remain idempotent"
        );
        let mut manager = store.scheduler_capacity_manager();
        let tombstone_ttl_secs: i64 = manager
            .ttl(store.key(&external_pool_global_in_flight_keys().released))
            .await
            .unwrap();
        assert!(
            tombstone_ttl_secs >= EXTERNAL_POOL_RELEASE_TOMBSTONE_TTL_SECS as i64 - 5,
            "external release tombstone must cover the late-command window: ttl={tombstone_ttl_secs}"
        );

        assert!(
            store
                .acquire_external_pool_lease(
                    pool_id,
                    released_lease_id,
                    1,
                    1,
                    Some(StdDuration::from_secs(60)),
                )
                .await
                .unwrap()
                .is_none(),
            "a late acquire must not resurrect a released external lease"
        );
        let state = store
            .external_pool_capacity_state(pool_id, None)
            .await
            .unwrap();
        assert_eq!(state.pool_in_flight_requests, 0);
        assert_eq!(state.global_in_flight_requests, 0);

        let fresh_lease_id = released_lease_id + 1;
        assert!(
            store
                .acquire_external_pool_lease(
                    pool_id,
                    fresh_lease_id,
                    1,
                    1,
                    Some(StdDuration::from_secs(60)),
                )
                .await
                .unwrap()
                .is_some(),
            "a tombstone must only reject its exact lease ID"
        );
        assert_eq!(
            store
                .release_external_pool_leases(pool_id, &[fresh_lease_id])
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn redis_external_pool_queue_tombstone_blocks_late_admission() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let released_lease_id = "late-external-queue-admission";
        assert_eq!(
            store
                .leave_external_pool_dispatch_queue_leases(&[released_lease_id])
                .await
                .unwrap(),
            0,
            "release-before-admit must remain idempotent"
        );
        let mut manager = store.scheduler_capacity_manager();
        let tombstone_ttl_secs: i64 = manager
            .ttl(store.key(external_pool_global_queue_released_key()))
            .await
            .unwrap();
        assert!(
            tombstone_ttl_secs >= EXTERNAL_POOL_RELEASE_TOMBSTONE_TTL_SECS as i64 - 5,
            "external queue tombstone must cover the late-command window: ttl={tombstone_ttl_secs}"
        );

        assert!(
            !store
                .try_enter_external_pool_dispatch_queue(released_lease_id, 1, 60)
                .await
                .unwrap(),
            "a late admission must not resurrect a released queue lease"
        );
        assert_eq!(store.external_pool_dispatch_queue_size().await.unwrap(), 0);

        let fresh_lease_id = "fresh-external-queue-admission";
        assert!(
            store
                .try_enter_external_pool_dispatch_queue(fresh_lease_id, 1, 60)
                .await
                .unwrap(),
            "a tombstone must only reject its exact queue lease ID"
        );
        assert_eq!(store.external_pool_dispatch_queue_size().await.unwrap(), 1);
        assert_eq!(
            store
                .leave_external_pool_dispatch_queue_leases(&[fresh_lease_id])
                .await
                .unwrap(),
            1
        );
        assert_eq!(store.external_pool_dispatch_queue_size().await.unwrap(), 0);
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn redis_scheduler_rate_limit_pacing_is_atomic_across_managers() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store_a = RedisStore::connect(&config).await.unwrap();
        let store_b = RedisStore::connect(&config).await.unwrap();
        let credential_id = 98_001;
        let first_lease = 98_100;
        let first = store_a
            .acquire_dispatch_lease_with_rate_limit(
                credential_id,
                first_lease,
                64,
                64,
                1,
                60,
                Some(StdDuration::from_secs(60)),
                "api",
            )
            .await
            .unwrap();
        let (available_at_ms, first_remaining_ms) = match first {
            SchedulerDispatchAdmission::Acquired {
                rate_limit_available_at_ms: Some(available_at_ms),
                rate_limit_remaining_ms: Some(remaining_ms),
                ..
            } => (available_at_ms, remaining_ms),
            other => panic!("first pacing admission should succeed, got {other:?}"),
        };
        assert!(
            (990..=1_000).contains(&first_remaining_ms),
            "Redis TIME may advance while the admission script returns: {first_remaining_ms}"
        );
        assert!(
            store_a
                .release_in_flight_lease(credential_id, first_lease)
                .await
                .unwrap()
        );

        let mut attempts = Vec::new();
        for offset in 0..16_u64 {
            let store = store_b.clone();
            attempts.push(tokio::spawn(async move {
                store
                    .acquire_dispatch_lease_with_rate_limit(
                        credential_id,
                        98_200 + offset,
                        64,
                        64,
                        1,
                        60,
                        Some(StdDuration::from_secs(60)),
                        "api",
                    )
                    .await
                    .unwrap()
            }));
        }
        for attempt in attempts {
            match attempt.await.unwrap() {
                SchedulerDispatchAdmission::RateLimited {
                    available_at_ms: observed_available_at_ms,
                    remaining_ms,
                    rpm,
                    ..
                } => {
                    assert_eq!(observed_available_at_ms, available_at_ms);
                    assert!(remaining_ms > 0);
                    assert_eq!(rpm, 60);
                }
                other => panic!("concurrent pacing admission should wait, got {other:?}"),
            }
        }
        assert_eq!(
            store_a
                .global_capacity_state()
                .await
                .unwrap()
                .in_flight_requests,
            0,
            "rejected pacing contenders must not occupy concurrency"
        );

        tokio::time::sleep(StdDuration::from_millis(first_remaining_ms + 20)).await;
        let recovered_lease = 98_300;
        assert!(matches!(
            store_b
                .acquire_dispatch_lease_with_rate_limit(
                    credential_id,
                    recovered_lease,
                    64,
                    64,
                    1,
                    60,
                    Some(StdDuration::from_secs(60)),
                    "api",
                )
                .await
                .unwrap(),
            SchedulerDispatchAdmission::Acquired { .. }
        ));
        assert!(
            store_b
                .release_in_flight_lease(credential_id, recovered_lease)
                .await
                .unwrap()
        );
        store_b.clear_rate_limit(credential_id).await.unwrap();
    }

    #[tokio::test]
    async fn redis_scheduler_pacing_preserves_only_bounded_phase_credit() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let credential_id = 98_301;
        let first_lease = 98_302;
        assert!(matches!(
            store
                .acquire_dispatch_lease_with_rate_limit(
                    credential_id,
                    first_lease,
                    8,
                    8,
                    1,
                    60,
                    Some(StdDuration::from_secs(60)),
                    "api",
                )
                .await
                .unwrap(),
            SchedulerDispatchAdmission::Acquired { .. }
        ));
        assert!(
            store
                .release_in_flight_lease(credential_id, first_lease)
                .await
                .unwrap()
        );

        let deadline_key = store.key(scheduler_rate_limit_key(credential_id));
        let owner_key = store.key(scheduler_rate_limit_owner_key(credential_id));
        let rpm_key = store.key(scheduler_rate_limit_rpm_key(credential_id));
        let phase_key = store.key(scheduler_rate_limit_phase_key(credential_id));
        let mut manager = store.scheduler_manager();
        let script = r#"
            local redis_time = redis.call('TIME')
            local now = tonumber(redis_time[1]) * 1000
                + math.floor(tonumber(redis_time[2]) / 1000)
            redis.call('DEL', KEYS[1], KEYS[2], KEYS[3])
            redis.call(
                'SET', KEYS[4], '60:' .. string.format('%.0f', now - tonumber(ARGV[1])),
                'PX', 2000
            )
            return now
        "#;
        let simulated_now: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(4)
            .arg(&deadline_key)
            .arg(&owner_key)
            .arg(&rpm_key)
            .arg(&phase_key)
            .arg(80)
            .query_async(&mut manager)
            .await
            .unwrap();

        let second_lease = 98_303;
        let (second_deadline, second_remaining) = match store
            .acquire_dispatch_lease_with_rate_limit(
                credential_id,
                second_lease,
                8,
                8,
                1,
                60,
                Some(StdDuration::from_secs(60)),
                "api",
            )
            .await
            .unwrap()
        {
            SchedulerDispatchAdmission::Acquired {
                rate_limit_available_at_ms: Some(deadline),
                rate_limit_remaining_ms: Some(remaining),
                ..
            } => (deadline, remaining),
            other => panic!("bounded phase admission should succeed, got {other:?}"),
        };
        assert!(
            (simulated_now + 875..=simulated_now + 1_000).contains(&second_deadline),
            "bounded phase deadline should retain only the recent timing credit: {second_deadline}"
        );
        assert!(
            (850..1_000).contains(&second_remaining),
            "an 80ms late arrival should preserve phase without a full reset: {second_remaining}"
        );
        assert!(
            store
                .release_in_flight_lease(credential_id, second_lease)
                .await
                .unwrap()
        );

        let reset_now: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(4)
            .arg(&deadline_key)
            .arg(&owner_key)
            .arg(&rpm_key)
            .arg(&phase_key)
            .arg(501)
            .query_async(&mut manager)
            .await
            .unwrap();
        let reset_lease = 98_304;
        let (reset_deadline, reset_remaining) = match store
            .acquire_dispatch_lease_with_rate_limit(
                credential_id,
                reset_lease,
                8,
                8,
                1,
                60,
                Some(StdDuration::from_secs(60)),
                "api",
            )
            .await
            .unwrap()
        {
            SchedulerDispatchAdmission::Acquired {
                rate_limit_available_at_ms: Some(deadline),
                rate_limit_remaining_ms: Some(remaining),
                ..
            } => (deadline, remaining),
            other => panic!("late phase reset admission should succeed, got {other:?}"),
        };
        assert!(
            (reset_now + 1_000..=reset_now + 1_050).contains(&reset_deadline),
            "phase older than the history window must reset from current Redis time: {reset_deadline}"
        );
        assert!(reset_remaining <= 1_000 && reset_remaining >= 950);
        assert!(
            store
                .rollback_dispatch_admission_and_publish_wakeup(
                    credential_id,
                    reset_lease,
                    false,
                    r#"{"kind":"test_wakeup"}"#,
                )
                .await
                .unwrap()
        );
        let phase_after_rollback: Option<String> = manager.get(&phase_key).await.unwrap();
        assert!(phase_after_rollback.is_none());

        store.clear_rate_limit(credential_id).await.unwrap();
    }

    #[tokio::test]
    async fn redis_scheduler_weighted_pacing_uses_original_request_weight_and_snapshot_tags() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let credential_id = 98_401;
        let lease_id = 98_402;
        let admission = store
            .acquire_dispatch_lease_with_rate_limit(
                credential_id,
                lease_id,
                15,
                15,
                64,
                60,
                Some(StdDuration::from_secs(60)),
                "api",
            )
            .await
            .unwrap();
        match admission {
            SchedulerDispatchAdmission::Acquired {
                in_flight_count,
                rate_limit_remaining_ms,
                rate_limit_rpm,
                rate_limit_owner_lease_id,
                ..
            } => {
                assert_eq!(
                    in_flight_count, 15,
                    "concurrency weight remains capped at 15"
                );
                assert!(
                    rate_limit_remaining_ms
                        .is_some_and(|remaining| (63_990..=64_000).contains(&remaining))
                );
                assert_eq!(rate_limit_rpm, Some(60));
                assert_eq!(rate_limit_owner_lease_id, Some(lease_id));
            }
            other => panic!("weighted pacing admission should succeed, got {other:?}"),
        }

        let state = store
            .scheduler_state_for_credentials(&[credential_id])
            .await
            .unwrap()
            .remove(&credential_id)
            .unwrap();
        assert_eq!(state.rate_limit_rpm, Some(60));
        assert_eq!(state.rate_limit_owner_lease_id, Some(lease_id));
        assert!(state.rate_limit_available_at_ms.is_some());
        assert!(
            state
                .rate_limit_remaining_ms
                .is_some_and(|remaining| { remaining > 0 && remaining <= 64_000 })
        );

        let payload = r#"{"kind":"test_wakeup"}"#;
        assert!(
            store
                .rollback_dispatch_admission_and_publish_wakeup(
                    credential_id,
                    lease_id,
                    false,
                    payload,
                )
                .await
                .unwrap()
        );
        let state = store
            .scheduler_state_for_credentials(&[credential_id])
            .await
            .unwrap()
            .remove(&credential_id)
            .unwrap();
        assert!(state.rate_limit_available_at_ms.is_none());
        assert!(state.in_flight_leases.is_empty());
    }

    #[tokio::test]
    async fn redis_scheduler_old_lease_rollback_cannot_clear_new_pacing_reservation() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let credential_id = 98_501;
        let old_lease_id = 98_502;
        let first = store
            .acquire_dispatch_lease_with_rate_limit(
                credential_id,
                old_lease_id,
                8,
                8,
                1,
                60,
                Some(StdDuration::from_secs(60)),
                "api",
            )
            .await
            .unwrap();
        let first_remaining_ms = match first {
            SchedulerDispatchAdmission::Acquired {
                rate_limit_remaining_ms: Some(remaining_ms),
                ..
            } => remaining_ms,
            other => panic!("first pacing admission should succeed, got {other:?}"),
        };
        assert!(
            store
                .release_in_flight_lease(credential_id, old_lease_id)
                .await
                .unwrap()
        );
        tokio::time::sleep(StdDuration::from_millis(first_remaining_ms + 20)).await;

        let new_lease_id = 98_503;
        assert!(matches!(
            store
                .acquire_dispatch_lease_with_rate_limit(
                    credential_id,
                    new_lease_id,
                    8,
                    8,
                    1,
                    60,
                    Some(StdDuration::from_secs(60)),
                    "api",
                )
                .await
                .unwrap(),
            SchedulerDispatchAdmission::Acquired { .. }
        ));

        let phase_key = store.key(scheduler_rate_limit_phase_key(credential_id));
        let mut manager = store.scheduler_manager();
        let phase_before_rollback: Option<String> = manager.get(&phase_key).await.unwrap();
        assert!(phase_before_rollback.is_some());

        let payload = r#"{"kind":"test_wakeup"}"#;
        assert!(
            !store
                .rollback_dispatch_admission_and_publish_wakeup(
                    credential_id,
                    old_lease_id,
                    false,
                    payload,
                )
                .await
                .unwrap(),
            "the old lease was already released and must not remove the new reservation"
        );
        let phase_after_rollback: Option<String> = manager.get(&phase_key).await.unwrap();
        assert_eq!(phase_after_rollback, phase_before_rollback);
        let state = store
            .scheduler_state_for_credentials(&[credential_id])
            .await
            .unwrap()
            .remove(&credential_id)
            .unwrap();
        assert_eq!(state.rate_limit_owner_lease_id, Some(new_lease_id));
        assert_eq!(state.rate_limit_rpm, Some(60));

        assert!(
            store
                .rollback_dispatch_admission_and_publish_wakeup(
                    credential_id,
                    new_lease_id,
                    false,
                    payload,
                )
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn redis_scheduler_snapshot_rejects_and_cleans_partial_pacing_triple() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let credential_id = 98_601;
        let deadline_key = store.key(scheduler_rate_limit_key(credential_id));
        let rpm_key = store.key(scheduler_rate_limit_rpm_key(credential_id));
        let owner_key = store.key(scheduler_rate_limit_owner_key(credential_id));
        let mut manager = store.scheduler_manager();
        let _: () = redis::pipe()
            .atomic()
            .cmd("SET")
            .arg(&deadline_key)
            .arg(now_ms() + 60_000)
            .cmd("SET")
            .arg(&rpm_key)
            .arg(60)
            .cmd("DEL")
            .arg(&owner_key)
            .query_async(&mut manager)
            .await
            .unwrap();

        let state = store
            .scheduler_state_for_credentials(&[credential_id])
            .await
            .unwrap()
            .remove(&credential_id)
            .unwrap();
        assert!(state.rate_limit_available_at_ms.is_none());
        let (deadline, rpm, owner): (Option<String>, Option<String>, Option<String>) =
            redis::pipe()
                .cmd("GET")
                .arg(deadline_key)
                .cmd("GET")
                .arg(rpm_key)
                .cmd("GET")
                .arg(owner_key)
                .query_async(&mut manager)
                .await
                .unwrap();
        assert_eq!((deadline, rpm, owner), (None, None, None));
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
    async fn redis_scheduler_touch_renews_all_local_and_global_lease_keys() {
        let Some(config) = test_config() else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let store = RedisStore::connect(&config).await.unwrap();
        let credential_id = 9_777;
        let lease_id = store.next_in_flight_lease_id().await.unwrap();
        assert!(
            store
                .acquire_dispatch_lease(
                    credential_id,
                    lease_id,
                    4,
                    8,
                    1,
                    Some(StdDuration::from_secs(30)),
                    "stream",
                )
                .await
                .unwrap()
                .is_some()
        );

        let local = in_flight_keys(credential_id);
        let global = global_in_flight_keys();
        let keys = [
            local.last_seen,
            local.acquired,
            local.kind,
            local.weight,
            local.count,
            global.last_seen,
            global.acquired,
            global.kind,
            global.weight,
            global.count,
        ];
        let full_keys: Vec<_> = keys.iter().map(|key| store.key(key)).collect();
        let mut manager = store.scheduler_capacity_manager();
        let mut expire_pipe = redis::pipe();
        for key in &full_keys {
            expire_pipe.cmd("EXPIRE").arg(key).arg(1);
        }
        let _: Vec<i64> = expire_pipe.query_async(&mut manager).await.unwrap();

        store
            .touch_in_flight_lease(credential_id, lease_id, 60)
            .await
            .unwrap();
        let mut ttl_pipe = redis::pipe();
        for key in &full_keys {
            ttl_pipe.cmd("TTL").arg(key);
        }
        let ttls: Vec<i64> = ttl_pipe.query_async(&mut manager).await.unwrap();
        assert!(
            ttls.iter().all(|ttl| *ttl >= 55),
            "touch must renew every lease key TTL, got {ttls:?}"
        );
        assert_eq!(
            store
                .global_capacity_state()
                .await
                .unwrap()
                .in_flight_requests,
            1
        );
        assert!(
            store
                .release_in_flight_lease(credential_id, lease_id)
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
            .touch_in_flight_lease(credential_id, lease_a, 60)
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
        let mut manager = store.scheduler_capacity_manager();
        let tombstone_ttl_secs: i64 = manager
            .ttl(store.key(&in_flight_keys(credential_id).released))
            .await
            .unwrap();
        assert!(
            tombstone_ttl_secs >= SCHEDULER_DISTRIBUTED_LEASE_SAFETY_SECS as i64 - 5,
            "rollback tombstone TTL must cover the distributed lease safety window: ttl={tombstone_ttl_secs}"
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
