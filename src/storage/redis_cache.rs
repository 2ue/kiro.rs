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
    UsageRouteKind, UsageSeriesPoint, UsageSource, UsageSummary, UsageTopAggregate,
    usage_dashboard_daily_windows, usage_dashboard_hourly_windows, usage_dashboard_timezone,
    usage_dashboard_windows,
};
use crate::model::config::Config;

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
    key_prefix: String,
}

const USAGE_SUMMARY_TOTALS_KEY: &str = "usage:summary:totals";
const USAGE_SUMMARY_CACHE_READ_KEY: &str = "usage:summary:cache_read";
const USAGE_SUMMARY_TOP_CREDENTIALS_KEY: &str = "usage:summary:top:credentials";
const USAGE_SUMMARY_TOP_CONVERSATIONS_KEY: &str = "usage:summary:top:conversations";
const USAGE_REALTIME_BUCKET_TTL_SECS: usize = REALTIME_USAGE_WINDOW_SECS as usize * 3;
const USAGE_SUMMARY_SEEN_TTL_SECS: usize = 35 * 24 * 60 * 60;
const USAGE_DASHBOARD_BUCKET_TTL_SECS: usize = 35 * 24 * 60 * 60;
const USAGE_DASHBOARD_TOP_MODELS_KEY: &str = "usage:dashboard:top:models";
const USAGE_DASHBOARD_TOP_CREDENTIALS_KEY: &str = "usage:dashboard:top:credentials";
const USAGE_DASHBOARD_TOP_ENDPOINTS_KEY: &str = "usage:dashboard:top:endpoints";
const USAGE_DASHBOARD_TOP_ERRORS_KEY: &str = "usage:dashboard:top:errors";
const USAGE_DASHBOARD_TOP_EXTERNAL_POOLS_KEY: &str = "usage:dashboard:top:external_pools";
const USAGE_RECORDS_INDEX_KEY: &str = "usage:records:index";
const USAGE_RECORDS_TTL_SECS: usize = 35 * 24 * 60 * 60;
const USAGE_RECORDS_MAX_CACHED: usize = 100_000;
const USAGE_RECORDS_QUERY_SCAN_LIMIT: usize = 5_000;
const USAGE_DASHBOARD_DURATION_MAX_SCRIPT: &str = r#"
    local current = tonumber(redis.call('HGET', KEYS[1], 'duration_ms_max') or '0')
    local candidate = tonumber(ARGV[1]) or 0
    if candidate > current then
        redis.call('HSET', KEYS[1], 'duration_ms_max', candidate)
    end
    redis.call('EXPIRE', KEYS[1], ARGV[2])
    return 1
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
        Ok(Self {
            client,
            manager,
            key_prefix: config.redis.key_prefix.trim_end_matches(':').to_string(),
        })
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

    pub fn dispatch_wakeup_channel(&self) -> String {
        self.key("events:dispatch_wakeup")
    }

    pub async fn subscribe_runtime_events(&self) -> anyhow::Result<PubSub> {
        let mut pubsub = self.client.get_async_pubsub().await?;
        pubsub
            .subscribe(self.runtime_config_changed_channel())
            .await?;
        pubsub.subscribe(self.credentials_changed_channel()).await?;
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

    pub async fn record_usage_summary(&self, record: &UsageRecord) -> anyhow::Result<()> {
        let created_at = DateTime::parse_from_rfc3339(&record.created_at)
            .map(|created_at| created_at.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());
        self.record_usage_record_snapshot(record, created_at)
            .await?;

        let seen_key = self.key(format!(
            "usage:summary:seen:{}",
            usage_dimension_hash(&record.id)
        ));
        let mut manager = self.manager.clone();
        let inserted: Option<String> = redis::cmd("SET")
            .arg(&seen_key)
            .arg("1")
            .arg("NX")
            .arg("EX")
            .arg(USAGE_SUMMARY_SEEN_TTL_SECS)
            .query_async(&mut manager)
            .await?;
        if inserted.as_deref() != Some("OK") {
            return Ok(());
        }

        let realtime_key = usage_realtime_bucket_key(created_at.timestamp());
        let totals_key = self.key(USAGE_SUMMARY_TOTALS_KEY);
        let cache_read_key = self.key(USAGE_SUMMARY_CACHE_READ_KEY);
        let realtime_key = self.key(realtime_key);

        let mut pipe = redis::pipe();
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
            .arg(record.cache_read_input_tokens.to_string())
            .arg(1i64)
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

        let _: () = pipe.query_async(&mut manager).await?;
        Ok(())
    }

    async fn record_usage_record_snapshot(
        &self,
        record: &UsageRecord,
        created_at: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        let member = usage_dimension_hash(&record.id);
        let record_key = self.key(usage_record_key(&member));
        let index_key = self.key(USAGE_RECORDS_INDEX_KEY);
        let encoded = serde_json::to_string(record)?;
        let cutoff_ms = Utc::now()
            .timestamp_millis()
            .saturating_sub((USAGE_RECORDS_TTL_SECS as i64).saturating_mul(1000));
        let mut manager = self.manager.clone();
        let mut pipe = redis::pipe();
        pipe.atomic()
            .cmd("SETEX")
            .arg(&record_key)
            .arg(USAGE_RECORDS_TTL_SECS)
            .arg(encoded)
            .cmd("ZADD")
            .arg(&index_key)
            .arg(created_at.timestamp_millis())
            .arg(&member)
            .cmd("EXPIRE")
            .arg(&index_key)
            .arg(USAGE_RECORDS_TTL_SECS)
            .cmd("ZREMRANGEBYSCORE")
            .arg(&index_key)
            .arg("-inf")
            .arg(cutoff_ms)
            .cmd("ZREMRANGEBYRANK")
            .arg(&index_key)
            .arg(0)
            .arg(-((USAGE_RECORDS_MAX_CACHED as isize) + 1));
        let _: () = pipe.query_async(&mut manager).await?;
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

        let cache_read_totals: HashMap<String, String> = manager
            .hgetall(self.key(USAGE_SUMMARY_CACHE_READ_KEY))
            .await?;
        let high_cache_requests = cache_read_totals
            .iter()
            .filter_map(|(tokens, requests)| {
                let tokens = tokens.parse::<i32>().ok()?;
                let requests = requests.parse::<usize>().ok()?;
                (tokens >= high_cache_threshold).then_some(requests)
            })
            .sum();

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

    pub async fn usage_dashboard(
        &self,
        timezone: Option<&str>,
        high_cache_threshold: i32,
    ) -> anyhow::Result<Option<UsageDashboardResponse>> {
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

    pub async fn clear_usage_summary(&self) -> anyhow::Result<usize> {
        let summary_deleted = self.del_pattern("usage:summary:*").await?;
        let dashboard_deleted = self.del_pattern("usage:dashboard:*").await?;
        let record_deleted = self.del_pattern("usage:records:*").await?;
        Ok(summary_deleted
            .saturating_add(dashboard_deleted)
            .saturating_add(record_deleted))
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
            .zrevrange(self.key(USAGE_DASHBOARD_TOP_EXTERNAL_POOLS_KEY), 0, -1)
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
    ) -> anyhow::Result<()> {
        let encoded = serde_json::to_string(binding)?;
        let session_hash = session_hash(session_id);
        let script = r#"
            local old = redis.call('GET', KEYS[1])
            if old then
                local ok, parsed = pcall(cjson.decode, old)
                if ok and parsed['credential_id'] then
                    local old_id = tostring(parsed['credential_id'])
                    if old_id ~= ARGV[2] then
                        redis.call('SREM', ARGV[5] .. old_id, ARGV[1])
                    end
                end
            end
            redis.call('SET', KEYS[1], ARGV[3], 'EX', ARGV[4])
            redis.call('SADD', ARGV[5] .. ARGV[2], ARGV[1])
            redis.call('EXPIRE', ARGV[5] .. ARGV[2], ARGV[4])
            return 1
        "#;
        let mut manager = self.manager.clone();
        let _: i64 = redis::cmd("EVAL")
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
        Ok(())
    }

    pub async fn delete_session_binding(&self, session_id: &str) -> anyhow::Result<()> {
        let session_hash = session_hash(session_id);
        let script = r#"
            local old = redis.call('GET', KEYS[1])
            redis.call('DEL', KEYS[1])
            if old then
                local ok, parsed = pcall(cjson.decode, old)
                if ok and parsed['credential_id'] then
                    redis.call('SREM', ARGV[2] .. tostring(parsed['credential_id']), ARGV[1])
                end
            end
            return 1
        "#;
        let mut manager = self.manager.clone();
        let _: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(self.key(session_binding_key(session_id)))
            .arg(&session_hash)
            .arg(self.key("scheduler:sessions_by_credential:"))
            .query_async(&mut manager)
            .await?;
        Ok(())
    }

    pub async fn delete_sessions_for_credential(
        &self,
        credential_id: u64,
    ) -> anyhow::Result<usize> {
        let set_key = self.key(sessions_by_credential_key(credential_id));
        let mut manager = self.manager.clone();
        let session_hashes: Vec<String> = manager.smembers(&set_key).await?;
        let mut deleted = 0usize;
        for session_hash in &session_hashes {
            let removed: i64 = manager
                .del(self.key(format!("scheduler:session:{}", session_hash)))
                .await?;
            if removed > 0 {
                deleted += 1;
            }
        }
        let _: () = manager.del(set_key).await?;
        Ok(deleted)
    }

    pub async fn record_session_soft_failure(
        &self,
        session_id: &str,
        credential_id: u64,
        threshold: u32,
        ttl_secs: usize,
    ) -> anyhow::Result<bool> {
        let session_hash = session_hash(session_id);
        let script = r#"
            local raw = redis.call('GET', KEYS[1])
            if not raw then
                return 0
            end
            local ok, parsed = pcall(cjson.decode, raw)
            if not ok or not parsed['credential_id'] then
                return 0
            end
            if tostring(parsed['credential_id']) ~= ARGV[2] then
                return 0
            end
            local count = tonumber(parsed['soft_failure_count'] or '0') + 1
            parsed['soft_failure_count'] = count
            parsed['last_used_at'] = ARGV[4]
            redis.call('SET', KEYS[1], cjson.encode(parsed), 'EX', ARGV[5])
            redis.call('SADD', ARGV[6] .. ARGV[2], ARGV[1])
            redis.call('EXPIRE', ARGV[6] .. ARGV[2], ARGV[5])
            if count >= tonumber(ARGV[3]) then
                return 1
            end
            return 0
        "#;
        let mut manager = self.manager.clone();
        let should_fallback: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(self.key(session_binding_key(session_id)))
            .arg(&session_hash)
            .arg(credential_id.to_string())
            .arg(threshold)
            .arg(Utc::now().to_rfc3339())
            .arg(ttl_secs.max(1))
            .arg(self.key("scheduler:sessions_by_credential:"))
            .query_async(&mut manager)
            .await?;
        Ok(should_fallback == 1)
    }

    pub async fn clear_session_soft_failure(
        &self,
        session_id: &str,
        credential_id: u64,
        ttl_secs: usize,
    ) -> anyhow::Result<()> {
        let session_hash = session_hash(session_id);
        let script = r#"
            local raw = redis.call('GET', KEYS[1])
            if not raw then
                return 0
            end
            local ok, parsed = pcall(cjson.decode, raw)
            if not ok or not parsed['credential_id'] then
                return 0
            end
            if tostring(parsed['credential_id']) ~= ARGV[2] then
                return 0
            end
            parsed['soft_failure_count'] = 0
            parsed['last_used_at'] = ARGV[3]
            redis.call('SET', KEYS[1], cjson.encode(parsed), 'EX', ARGV[4])
            redis.call('SADD', ARGV[5] .. ARGV[2], ARGV[1])
            redis.call('EXPIRE', ARGV[5] .. ARGV[2], ARGV[4])
            return 1
        "#;
        let mut manager = self.manager.clone();
        let _: i64 = redis::cmd("EVAL")
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
        let mut manager = self.manager.clone();
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
        let mut manager = self.manager.clone();
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

            local health = {}
            local health_raw = redis.call('GET', KEYS[2])
            if health_raw then
                local ok, parsed = pcall(cjson.decode, health_raw)
                if ok and parsed then health = parsed end
            end
            local streak = tonumber(health['transient_failure_streak'] or '0') + 1
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

            local cooldown = {until_ms = candidate_until, reason = reason}
            if model ~= '' then cooldown['model'] = model end
            local cooldown_raw = redis.call('GET', KEYS[1])
            if cooldown_raw then
                local ok, parsed = pcall(cjson.decode, cooldown_raw)
                if ok and parsed and tonumber(parsed['until_ms'] or '0') >= candidate_until then
                    cooldown = parsed
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
        let mut manager = self.manager.clone();
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
        let mut manager = self.manager.clone();
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
    ) -> anyhow::Result<SchedulerHealthState> {
        let now = now_ms();
        let script = r#"
            local ttl = tonumber(ARGV[1])
            local now = tonumber(ARGV[2])
            local window_10s = tonumber(ARGV[3])
            local window_60s = tonumber(ARGV[4])
            local window_5m = tonumber(ARGV[5])
            local health = {}
            local raw = redis.call('GET', KEYS[1])
            if raw then
                local ok, parsed = pcall(cjson.decode, raw)
                if ok and parsed then health = parsed end
            end
            local sequence = redis.call('INCR', KEYS[3])
            local member = tostring(now) .. '-' .. tostring(sequence)
            redis.call('ZREMRANGEBYSCORE', KEYS[2], '-inf', now - window_5m)
            redis.call('ZADD', KEYS[2], now, member)
            redis.call('EXPIRE', KEYS[2], ttl)
            health['selection_count'] = tonumber(health['selection_count'] or '0') + 1
            health['recent_selection_count_10s'] = redis.call('ZCOUNT', KEYS[2], now - window_10s, '+inf')
            health['recent_selection_count_60s'] = redis.call('ZCOUNT', KEYS[2], now - window_60s, '+inf')
            health['recent_selection_count_5m'] = redis.call('ZCOUNT', KEYS[2], now - window_5m, '+inf')
            redis.call('SET', KEYS[1], cjson.encode(health), 'EX', ttl)
            return cjson.encode(health)
        "#;
        let mut manager = self.manager.clone();
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
            .query_async(&mut manager)
            .await?;
        Ok(serde_json::from_str(&encoded)?)
    }

    pub async fn clear_scheduler_health(&self, credential_id: u64) -> anyhow::Result<()> {
        let mut manager = self.manager.clone();
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
        let mut manager = self.manager.clone();
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
        let mut manager = self.manager.clone();
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

    pub async fn next_in_flight_lease_id(&self) -> anyhow::Result<u64> {
        let mut manager = self.manager.clone();
        let id: u64 = manager
            .incr(self.key("scheduler:inflight:lease_sequence"), 1u64)
            .await?;
        Ok(id)
    }

    pub async fn next_external_pool_lease_id(&self) -> anyhow::Result<u64> {
        let mut manager = self.manager.clone();
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
        max_age: Option<StdDuration>,
        kind: &str,
    ) -> anyhow::Result<Option<usize>> {
        let now = now_ms();
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

            if max_age_ms > 0 then
                local expired = redis.call('ZRANGEBYSCORE', KEYS[1], '-inf', now - max_age_ms)
                for _, member in ipairs(expired) do
                    redis.call('ZREM', KEYS[1], member)
                    redis.call('ZREM', KEYS[2], member)
                    redis.call('HDEL', KEYS[3], member)
                end
                local global_expired = redis.call('ZRANGEBYSCORE', KEYS[4], '-inf', now - max_age_ms)
                for _, member in ipairs(global_expired) do
                    redis.call('ZREM', KEYS[4], member)
                    redis.call('ZREM', KEYS[5], member)
                    redis.call('HDEL', KEYS[6], member)
                end
            end

            local count = redis.call('ZCARD', KEYS[1])
            if max_count > 0 and count >= max_count then
                return {0, count}
            end

            local global_count = redis.call('ZCARD', KEYS[4])
            if global_max_count > 0 and global_count >= global_max_count then
                return {0, global_count}
            end

            redis.call('ZADD', KEYS[1], now, lease_id)
            redis.call('ZADD', KEYS[2], now, lease_id)
            redis.call('HSET', KEYS[3], lease_id, kind)
            redis.call('ZADD', KEYS[4], now, lease_id)
            redis.call('ZADD', KEYS[5], now, lease_id)
            redis.call('HSET', KEYS[6], lease_id, kind)
            if ttl_secs > 0 then
                redis.call('EXPIRE', KEYS[1], ttl_secs)
                redis.call('EXPIRE', KEYS[2], ttl_secs)
                redis.call('EXPIRE', KEYS[3], ttl_secs)
                redis.call('EXPIRE', KEYS[4], ttl_secs)
                redis.call('EXPIRE', KEYS[5], ttl_secs)
                redis.call('EXPIRE', KEYS[6], ttl_secs)
            end
            return {1, count + 1}
        "#;
        let keys = in_flight_keys(credential_id);
        let global_keys = global_in_flight_keys();
        let mut manager = self.manager.clone();
        let result: Vec<i64> = redis::cmd("EVAL")
            .arg(script)
            .arg(6)
            .arg(self.key(&keys.last_seen))
            .arg(self.key(&keys.acquired))
            .arg(self.key(&keys.kind))
            .arg(self.key(&global_keys.last_seen))
            .arg(self.key(&global_keys.acquired))
            .arg(self.key(&global_keys.kind))
            .arg(now)
            .arg(max_age_ms)
            .arg(max_concurrent_requests)
            .arg(global_max_concurrent_requests)
            .arg(lease_id.to_string())
            .arg(kind)
            .arg(ttl_secs)
            .query_async(&mut manager)
            .await?;
        if result.first().copied().unwrap_or(0) == 1 {
            Ok(Some(result.get(1).copied().unwrap_or(1).max(0) as usize))
        } else {
            Ok(None)
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
        let now = now_ms();
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
            local ttl_secs = tonumber(ARGV[6])

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
        let mut manager = self.manager.clone();
        let result: Vec<i64> = redis::cmd("EVAL")
            .arg(script)
            .arg(4)
            .arg(self.key(&keys.last_seen))
            .arg(self.key(&keys.acquired))
            .arg(self.key(&global_keys.last_seen))
            .arg(self.key(&global_keys.acquired))
            .arg(now)
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

    pub async fn release_in_flight_lease(
        &self,
        credential_id: u64,
        lease_id: u64,
    ) -> anyhow::Result<bool> {
        let keys = in_flight_keys(credential_id);
        let global_keys = global_in_flight_keys();
        let lease_id = lease_id.to_string();
        let mut manager = self.manager.clone();
        let removed: i64 = redis::pipe()
            .atomic()
            .cmd("ZREM")
            .arg(self.key(&keys.last_seen))
            .arg(&lease_id)
            .cmd("ZREM")
            .arg(self.key(&keys.acquired))
            .arg(&lease_id)
            .cmd("HDEL")
            .arg(self.key(&keys.kind))
            .arg(&lease_id)
            .cmd("ZREM")
            .arg(self.key(&global_keys.last_seen))
            .arg(&lease_id)
            .cmd("ZREM")
            .arg(self.key(&global_keys.acquired))
            .arg(&lease_id)
            .cmd("HDEL")
            .arg(self.key(&global_keys.kind))
            .arg(&lease_id)
            .query_async::<(i64, i64, i64, i64, i64, i64)>(&mut manager)
            .await
            .map(|(a, b, c, d, e, f)| a + b + c + d + e + f)?;
        Ok(removed > 0)
    }

    pub async fn release_external_pool_lease(
        &self,
        pool_id: u64,
        lease_id: u64,
    ) -> anyhow::Result<bool> {
        let keys = external_pool_in_flight_keys(pool_id);
        let global_keys = external_pool_global_in_flight_keys();
        let lease_id = lease_id.to_string();
        let mut manager = self.manager.clone();
        let removed: i64 = redis::pipe()
            .atomic()
            .cmd("ZREM")
            .arg(self.key(&keys.last_seen))
            .arg(&lease_id)
            .cmd("ZREM")
            .arg(self.key(&keys.acquired))
            .arg(&lease_id)
            .cmd("ZREM")
            .arg(self.key(&global_keys.last_seen))
            .arg(&lease_id)
            .cmd("ZREM")
            .arg(self.key(&global_keys.acquired))
            .arg(&lease_id)
            .query_async::<(i64, i64, i64, i64)>(&mut manager)
            .await
            .map(|(a, b, c, d)| a + b + c + d)?;
        Ok(removed > 0)
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
        let now = now_ms();
        let script = r#"
            local lease_id = ARGV[1]
            local now = tonumber(ARGV[2])
            local ttl_secs = tonumber(ARGV[3])

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
        let mut manager = self.manager.clone();
        let touched: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(4)
            .arg(self.key(&keys.last_seen))
            .arg(self.key(&keys.acquired))
            .arg(self.key(&global_keys.last_seen))
            .arg(self.key(&global_keys.acquired))
            .arg(lease_id)
            .arg(now)
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
        let now = now_ms();
        let max_age_ms = max_age.map(|age| age.as_millis() as i64).unwrap_or(0);
        let script = r#"
            local now = tonumber(ARGV[1])
            local max_age_ms = tonumber(ARGV[2])
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
        let mut manager = self.manager.clone();
        let result: Vec<i64> = redis::cmd("EVAL")
            .arg(script)
            .arg(4)
            .arg(self.key(&keys.last_seen))
            .arg(self.key(&keys.acquired))
            .arg(self.key(&global_keys.last_seen))
            .arg(self.key(&global_keys.acquired))
            .arg(now)
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
        let mut manager = self.manager.clone();
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
        let mut manager = self.manager.clone();
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
        let mut manager = self.manager.clone();
        let lease_id = lease_id.to_string();
        let now = now_ms() as f64;
        let _: (i64, i64) = redis::pipe()
            .atomic()
            .cmd("ZADD")
            .arg(self.key(&keys.last_seen))
            .arg(now)
            .arg(&lease_id)
            .cmd("ZADD")
            .arg(self.key(&global_keys.last_seen))
            .arg(now)
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
        let mut manager = self.manager.clone();
        let _: (i64, i64, i64, i64) = redis::pipe()
            .atomic()
            .cmd("HSET")
            .arg(self.key(&keys.kind))
            .arg(&lease_id)
            .arg(kind)
            .cmd("ZADD")
            .arg(self.key(&keys.last_seen))
            .arg(now_ms() as f64)
            .arg(&lease_id)
            .cmd("HSET")
            .arg(self.key(&global_keys.kind))
            .arg(&lease_id)
            .arg(kind)
            .cmd("ZADD")
            .arg(self.key(&global_keys.last_seen))
            .arg(now_ms() as f64)
            .arg(&lease_id)
            .query_async::<(i64, i64, i64, i64)>(&mut manager)
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
                redis.call('ZREM', KEYS[1], member)
                redis.call('ZREM', KEYS[2], member)
                redis.call('HDEL', KEYS[3], member)
                redis.call('ZREM', KEYS[4], member)
                redis.call('ZREM', KEYS[5], member)
                redis.call('HDEL', KEYS[6], member)
            end
            return #expired
        "#;
        let mut manager = self.manager.clone();
        let mut cleaned = 0usize;
        let global_keys = global_in_flight_keys();
        for credential_id in credential_ids {
            let keys = in_flight_keys(*credential_id);
            let removed: i64 = redis::cmd("EVAL")
                .arg(script)
                .arg(6)
                .arg(self.key(&keys.last_seen))
                .arg(self.key(&keys.acquired))
                .arg(self.key(&keys.kind))
                .arg(self.key(&global_keys.last_seen))
                .arg(self.key(&global_keys.acquired))
                .arg(self.key(&global_keys.kind))
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
        let mut manager = self.manager.clone();
        if let Some(min_idle) = min_idle {
            let cutoff = now_ms() - min_idle.as_millis() as i64;
            let script = r#"
                local expired = redis.call('ZRANGEBYSCORE', KEYS[1], '-inf', ARGV[1])
                for _, member in ipairs(expired) do
                    redis.call('ZREM', KEYS[1], member)
                    redis.call('ZREM', KEYS[2], member)
                    redis.call('HDEL', KEYS[3], member)
                    redis.call('ZREM', KEYS[4], member)
                    redis.call('ZREM', KEYS[5], member)
                    redis.call('HDEL', KEYS[6], member)
                end
                return #expired
            "#;
            let removed: i64 = redis::cmd("EVAL")
                .arg(script)
                .arg(6)
                .arg(self.key(&keys.last_seen))
                .arg(self.key(&keys.acquired))
                .arg(self.key(&keys.kind))
                .arg(self.key(&global_keys.last_seen))
                .arg(self.key(&global_keys.acquired))
                .arg(self.key(&global_keys.kind))
                .arg(cutoff)
                .query_async(&mut manager)
                .await?;
            return Ok(removed.max(0) as usize);
        }

        let count: i64 = manager.zcard(self.key(&keys.last_seen)).await.unwrap_or(0);
        let script = r#"
            local leases = redis.call('ZRANGE', KEYS[1], 0, -1)
            for _, member in ipairs(leases) do
                redis.call('ZREM', KEYS[4], member)
                redis.call('ZREM', KEYS[5], member)
                redis.call('HDEL', KEYS[6], member)
            end
            redis.call('DEL', KEYS[1], KEYS[2], KEYS[3])
            return #leases
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
        let mut pipe = redis::pipe();
        for credential_id in credential_ids {
            let keys = in_flight_keys(*credential_id);
            pipe.cmd("GET")
                .arg(self.key(scheduler_cooldown_key(*credential_id)))
                .cmd("GET")
                .arg(self.key(scheduler_health_key(*credential_id)))
                .cmd("GET")
                .arg(self.key(scheduler_rate_limit_key(*credential_id)))
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

        let mut manager = self.manager.clone();
        let values: Vec<redis::Value> = pipe.query_async(&mut manager).await?;
        let now = now_ms();
        let mut keys_to_delete = Vec::new();
        let mut indexed_models: Vec<(u64, String, String)> = Vec::new();
        for (index, credential_id) in credential_ids.iter().enumerate() {
            let base = index * 10;
            let cooldown_raw: Option<String> = redis::from_redis_value(&values[base])?;
            let health_raw: Option<String> = redis::from_redis_value(&values[base + 1])?;
            let rate_raw: Option<String> = redis::from_redis_value(&values[base + 2])?;
            let last_seen: Vec<(String, f64)> = redis::from_redis_value(&values[base + 3])?;
            let acquired: Vec<(String, f64)> = redis::from_redis_value(&values[base + 4])?;
            let kinds: HashMap<String, String> = redis::from_redis_value(&values[base + 5])?;
            let recent_10s: i64 = redis::from_redis_value(&values[base + 6])?;
            let recent_60s: i64 = redis::from_redis_value(&values[base + 7])?;
            let recent_5m: i64 = redis::from_redis_value(&values[base + 8])?;
            let model_index: HashMap<String, String> = redis::from_redis_value(&values[base + 9])?;
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
            let mut manager = self.manager.clone();
            let full_keys: Vec<String> = keys_to_delete
                .into_iter()
                .map(|key| self.key(key))
                .collect();
            let _: () = manager.del(full_keys).await?;
        }
        Ok(states)
    }

    pub async fn global_capacity_state(&self) -> anyhow::Result<SchedulerGlobalCapacityState> {
        let keys = global_in_flight_keys();
        let mut manager = self.manager.clone();
        let (in_flight, queued): (i64, Option<i64>) = redis::pipe()
            .cmd("ZCARD")
            .arg(self.key(&keys.last_seen))
            .cmd("GET")
            .arg(self.key(scheduler_global_queue_key()))
            .query_async(&mut manager)
            .await?;
        Ok(SchedulerGlobalCapacityState {
            in_flight_requests: in_flight.max(0) as u32,
            queued_requests: queued.unwrap_or(0).max(0) as u32,
        })
    }

    pub async fn try_enter_dispatch_queue(&self, max_queued: u32) -> anyhow::Result<bool> {
        let script = r#"
            local max_queued = tonumber(ARGV[1])
            local count = tonumber(redis.call('GET', KEYS[1]) or '0')
            if max_queued > 0 and count >= max_queued then
                return 0
            end
            redis.call('INCR', KEYS[1])
            redis.call('EXPIRE', KEYS[1], ARGV[2])
            return 1
        "#;
        let mut manager = self.manager.clone();
        let admitted: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(self.key(scheduler_global_queue_key()))
            .arg(max_queued)
            .arg(3600)
            .query_async(&mut manager)
            .await?;
        Ok(admitted == 1)
    }

    pub async fn leave_dispatch_queue(&self) -> anyhow::Result<()> {
        let script = r#"
            local count = tonumber(redis.call('GET', KEYS[1]) or '0')
            if count <= 1 then
                redis.call('DEL', KEYS[1])
            else
                redis.call('DECR', KEYS[1])
            end
            return 1
        "#;
        let mut manager = self.manager.clone();
        let _: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(self.key(scheduler_global_queue_key()))
            .query_async(&mut manager)
            .await?;
        Ok(())
    }

    pub async fn try_enter_external_pool_dispatch_queue(
        &self,
        max_queued: u32,
    ) -> anyhow::Result<bool> {
        let script = r#"
            local max_queued = tonumber(ARGV[1])
            local count = tonumber(redis.call('GET', KEYS[1]) or '0')
            if max_queued > 0 and count >= max_queued then
                return 0
            end
            redis.call('INCR', KEYS[1])
            redis.call('EXPIRE', KEYS[1], ARGV[2])
            return 1
        "#;
        let mut manager = self.manager.clone();
        let admitted: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(self.key(external_pool_global_queue_key()))
            .arg(max_queued)
            .arg(3600)
            .query_async(&mut manager)
            .await?;
        Ok(admitted == 1)
    }

    pub async fn leave_external_pool_dispatch_queue(&self) -> anyhow::Result<()> {
        let script = r#"
            local count = tonumber(redis.call('GET', KEYS[1]) or '0')
            if count <= 1 then
                redis.call('DEL', KEYS[1])
            else
                redis.call('DECR', KEYS[1])
            end
            return 1
        "#;
        let mut manager = self.manager.clone();
        let _: i64 = redis::cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(self.key(external_pool_global_queue_key()))
            .query_async(&mut manager)
            .await?;
        Ok(())
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
    pipe: &mut redis::Pipeline,
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
    pipe: &mut redis::Pipeline,
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

    pipe.cmd("EVAL")
        .arg(USAGE_DASHBOARD_DURATION_MAX_SCRIPT)
        .arg(1)
        .arg(key)
        .arg(record.duration_ms.min(i64::MAX as u64) as i64)
        .arg(USAGE_DASHBOARD_BUCKET_TTL_SECS);
}

fn append_external_pool_usage_summary(
    pipe: &mut redis::Pipeline,
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
    pipe: &mut redis::Pipeline,
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
        .arg(record.estimated_cost_usd);
    if let Some(label) = label.filter(|label| !label.trim().is_empty()) {
        pipe.cmd("HSET").arg(&metrics_key).arg("label").arg(label);
    }
}

fn append_usage_top_aggregate(
    pipe: &mut redis::Pipeline,
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
        .arg(record.estimated_cost_usd);
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
    if let Some(q) = query.q.as_deref() {
        if !usage_record_matches_search(record, q) {
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
    if let Some(model) = query.model.as_deref() {
        if record.model != model {
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

    [
        Some(record.id.as_str()),
        Some(record.created_at.as_str()),
        Some(record.endpoint.as_str()),
        Some(record.model.as_str()),
        record.upstream_model.as_deref(),
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
    "scheduler:global:queued"
}

fn external_pool_global_queue_key() -> &'static str {
    "external_pool:global:queued"
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
}

fn in_flight_keys(credential_id: u64) -> InFlightKeys {
    InFlightKeys {
        last_seen: format!("scheduler:inflight:{}:last_seen", credential_id),
        acquired: format!("scheduler:inflight:{}:acquired", credential_id),
        kind: format!("scheduler:inflight:{}:kind", credential_id),
    }
}

fn global_in_flight_keys() -> InFlightKeys {
    InFlightKeys {
        last_seen: "scheduler:global:inflight:last_seen".to_string(),
        acquired: "scheduler:global:inflight:acquired".to_string(),
        kind: "scheduler:global:inflight:kind".to_string(),
    }
}

fn external_pool_in_flight_keys(pool_id: u64) -> InFlightKeys {
    InFlightKeys {
        last_seen: format!("external_pool:inflight:{}:last_seen", pool_id),
        acquired: format!("external_pool:inflight:{}:acquired", pool_id),
        kind: format!("external_pool:inflight:{}:kind", pool_id),
    }
}

fn external_pool_global_in_flight_keys() -> InFlightKeys {
    InFlightKeys {
        last_seen: "external_pool:global:inflight:last_seen".to_string(),
        acquired: "external_pool:global:inflight:acquired".to_string(),
        kind: "external_pool:global:inflight:kind".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
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

    fn test_config() -> Option<Config> {
        let url = std::env::var("KIRO_RS_TEST_REDIS_URL").ok()?;
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
            upstream_model: None,
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
            total_input_tokens: 100,
            compat_input_tokens: 90,
            billable_input_tokens: 95,
            output_tokens: 20,
            cache_read_input_tokens,
            cache_creation_input_tokens: 5,
            cache_creation_5m_input_tokens: 5,
            cache_creation_1h_input_tokens: 0,
            estimated_cost_usd,
            pricing_available: status == UsageRecordStatus::Success,
            pricing_model: Some("claude-sonnet-4-5".to_string()),
            duration_ms,
            first_token_latency_ms: Some(duration_ms / 2),
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
        });
        let error = usage_record(
            "redis-usage-error",
            UsageRecordStatus::Error,
            UsageSource::LocalPromptCache,
            100,
            0.20,
            50,
        );

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

        let selected_once = store.record_scheduler_selection(3).await.unwrap();
        assert_eq!(selected_once.selection_count, 1);
        assert_eq!(selected_once.recent_selection_count_10s, 1);
        assert_eq!(selected_once.recent_selection_count_60s, 1);
        assert_eq!(selected_once.recent_selection_count_5m, 1);
        let selected_twice = store.record_scheduler_selection(3).await.unwrap();
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
        store.clear_scheduler_health(3).await.unwrap();
        store.clear_scheduler_health(4).await.unwrap();
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
