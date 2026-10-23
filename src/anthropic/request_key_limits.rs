//! Per request-API-key usage limits: billed amount (USD), Kiro credits and tokens, per day,
//! per month or in total.
//!
//! Counters live in Redis so every instance sees the same totals. A request is checked before
//! dispatch and rejected with 429 once any limit is reached; usage is added after the request
//! finishes, so requests already in flight can push a key slightly past its limit. Keys without
//! limits never touch Redis. When Redis cannot answer, a key that has limits is rejected (fail
//! closed) rather than allowed to run without accounting.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Datelike, FixedOffset, TimeZone, Utc};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::storage::redis_cache::RedisStore;

/// What a limit counts. Each maps to a standard usage field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyLimitMetric {
    /// Billed amount in USD: `estimatedCostUsd`.
    CostUsd,
    /// Kiro credits consumed on local accounts: `kiroMeteringUsage`.
    Credits,
    /// Input + output + cache read + cache creation tokens.
    Tokens,
}

impl KeyLimitMetric {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CostUsd => "cost_usd",
            Self::Credits => "credits",
            Self::Tokens => "tokens",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::CostUsd => "计费金额",
            Self::Credits => "积分",
            Self::Tokens => "Token",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyLimitPeriod {
    Daily,
    Monthly,
    Total,
}

impl KeyLimitPeriod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Monthly => "monthly",
            Self::Total => "total",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Daily => "每天",
            Self::Monthly => "每月",
            Self::Total => "累计",
        }
    }
}

/// One limit on a request API key.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyUsageLimit {
    pub metric: KeyLimitMetric,
    pub period: KeyLimitPeriod,
    /// Requests are rejected once usage in the period reaches this value.
    pub limit: f64,
}

// Saved limits are validated to be finite, so equality is total; the key policy that holds
// them is compared with `Eq` to detect config changes.
impl Eq for KeyUsageLimit {}

pub const MAX_LIMITS_PER_KEY: usize = 9;

/// Validates a key's limit list: positive finite caps, no duplicate metric + period pair.
pub fn validate_key_limits(limits: &[KeyUsageLimit]) -> Result<(), String> {
    if limits.len() > MAX_LIMITS_PER_KEY {
        return Err(format!("每个 Key 最多设置 {MAX_LIMITS_PER_KEY} 条用量限制"));
    }
    let mut seen = std::collections::HashSet::new();
    for limit in limits {
        if !limit.limit.is_finite() || limit.limit <= 0.0 {
            return Err(format!(
                "{}{}限制必须是大于 0 的数字",
                limit.period.label(),
                limit.metric.label()
            ));
        }
        if limit.metric == KeyLimitMetric::Tokens && limit.limit.fract() != 0.0 {
            return Err("Token 限制必须是整数".to_string());
        }
        if !seen.insert((limit.metric, limit.period)) {
            return Err(format!(
                "{}{}限制重复",
                limit.period.label(),
                limit.metric.label()
            ));
        }
    }
    Ok(())
}

/// Usage of one finished request, in the three measured units.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct KeyUsageDelta {
    pub cost_usd: f64,
    pub credits: f64,
    pub tokens: f64,
}

impl KeyUsageDelta {
    pub fn get(&self, metric: KeyLimitMetric) -> f64 {
        match metric {
            KeyLimitMetric::CostUsd => self.cost_usd,
            KeyLimitMetric::Credits => self.credits,
            KeyLimitMetric::Tokens => self.tokens,
        }
    }

    pub fn is_empty(&self) -> bool {
        [self.cost_usd, self.credits, self.tokens]
            .iter()
            .all(|value| !value.is_finite() || *value <= 0.0)
    }
}

/// Period windows are in China Standard Time, matching the usage dashboard default.
pub fn limits_timezone() -> FixedOffset {
    FixedOffset::east_opt(8 * 3600).expect("UTC+8 is a valid offset")
}

/// Window id used in the counter key and when the window ends (for retry-after). `Total` never
/// resets and has no end.
pub fn period_window(
    period: KeyLimitPeriod,
    now: DateTime<Utc>,
) -> (String, Option<DateTime<Utc>>) {
    let tz = limits_timezone();
    let local = now.with_timezone(&tz);
    match period {
        KeyLimitPeriod::Daily => {
            let next = local.date_naive().succ_opt().unwrap_or(local.date_naive());
            let end = tz
                .from_local_datetime(&next.and_hms_opt(0, 0, 0).expect("midnight"))
                .single()
                .map(|time| time.with_timezone(&Utc));
            (local.format("%Y%m%d").to_string(), end)
        }
        KeyLimitPeriod::Monthly => {
            let (year, month) = if local.month() == 12 {
                (local.year() + 1, 1)
            } else {
                (local.year(), local.month() + 1)
            };
            let end = tz
                .with_ymd_and_hms(year, month, 1, 0, 0, 0)
                .single()
                .map(|time| time.with_timezone(&Utc));
            (local.format("%Y%m").to_string(), end)
        }
        KeyLimitPeriod::Total => ("all".to_string(), None),
    }
}

/// Start of the current window (UTC). `Total` has none.
pub fn period_start(period: KeyLimitPeriod, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let tz = limits_timezone();
    let local = now.with_timezone(&tz);
    let start = match period {
        KeyLimitPeriod::Daily => tz
            .from_local_datetime(&local.date_naive().and_hms_opt(0, 0, 0)?)
            .single()?,
        KeyLimitPeriod::Monthly => tz
            .with_ymd_and_hms(local.year(), local.month(), 1, 0, 0, 0)
            .single()?,
        KeyLimitPeriod::Total => return None,
    };
    Some(start.with_timezone(&Utc))
}

/// Redis key of one counter hash: one hash per key id and window, a field per metric.
pub fn counter_key(key_id: &str, period: KeyLimitPeriod, window: &str) -> String {
    format!("request_key_limits:{key_id}:{}:{window}", period.as_str())
}

/// Counters outlive their window by a margin so late writes still land; `Total` never expires.
fn counter_ttl_secs(period: KeyLimitPeriod) -> Option<u64> {
    match period {
        KeyLimitPeriod::Daily => Some(3 * 24 * 3600),
        KeyLimitPeriod::Monthly => Some(62 * 24 * 3600),
        KeyLimitPeriod::Total => None,
    }
}

const REDIS_TIMEOUT: Duration = Duration::from_millis(500);
const ALL_METRICS: [KeyLimitMetric; 3] = [
    KeyLimitMetric::CostUsd,
    KeyLimitMetric::Credits,
    KeyLimitMetric::Tokens,
];
/// Hash field present on every counter window. A window that exists is authoritative: live
/// increments, history seeding and resets all set it, and history is only ever written into a
/// window that does not exist yet.
const COUNTED_MARKER: &str = "_counted";

/// Seeds every limited window of `keys` from the usage rollups, in the background. Used when
/// limits are saved and at startup (after a Redis flush the counters come back from history).
pub fn spawn_seed_from_history(
    limiter: Arc<RequestKeyLimiter>,
    store: Arc<crate::storage::postgres::PostgresStore>,
    keys: Vec<(String, Vec<KeyLimitPeriod>)>,
) {
    if keys.is_empty() {
        return;
    }
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    runtime.spawn(async move {
        let now = Utc::now();
        for (key_id, periods) in keys {
            for period in periods {
                let result = async {
                    let totals = store
                        .request_key_usage_since(&key_id, period_start(period, now))
                        .await?;
                    limiter
                        .seed_from_history(
                            &key_id,
                            period,
                            KeyUsageDelta {
                                cost_usd: totals.cost_usd,
                                credits: totals.credits,
                                tokens: totals.tokens,
                            },
                        )
                        .await
                }
                .await;
                if let Err(err) = result {
                    tracing::warn!(key_id = %key_id, period = period.as_str(), "按历史用量初始化 Key 限额计数失败: {}", err);
                }
            }
        }
    });
}

/// Limited key ids with the periods they are limited on, for seeding.
pub fn limited_periods(
    policies: &[crate::model::config::RequestApiKeyPolicy],
) -> Vec<(String, Vec<KeyLimitPeriod>)> {
    policies
        .iter()
        .filter(|policy| policy.enabled && !policy.usage_limits.is_empty())
        .map(|policy| {
            let mut periods: Vec<KeyLimitPeriod> = policy
                .usage_limits
                .iter()
                .map(|limit| limit.period)
                .collect();
            periods.sort_by_key(|period| period.as_str());
            periods.dedup();
            (
                crate::common::auth::request_api_key_id(&policy.api_key),
                periods,
            )
        })
        .collect()
}

/// Current usage and limit of one rule, for checks and for the admin page.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyLimitStatus {
    pub metric: KeyLimitMetric,
    pub period: KeyLimitPeriod,
    pub limit: f64,
    pub used: f64,
    pub exceeded: bool,
    /// When the window resets (RFC3339); absent for `total`.
    pub resets_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum KeyLimitDecision {
    Allowed,
    /// A limit is reached; `retry_after_secs` is time until its window resets, if it does.
    Exceeded {
        status: KeyLimitStatus,
        retry_after_secs: Option<u64>,
    },
    /// Redis could not answer. The key has limits, so the request is rejected.
    Unavailable(String),
}

/// Pure decision over current usage: the first reached limit wins, ordered so a window that
/// never resets is reported before one that does.
pub fn decide(statuses: &[KeyLimitStatus], now: DateTime<Utc>) -> KeyLimitDecision {
    let mut exceeded: Vec<&KeyLimitStatus> =
        statuses.iter().filter(|status| status.exceeded).collect();
    exceeded.sort_by_key(|status| match status.period {
        KeyLimitPeriod::Total => 0,
        KeyLimitPeriod::Monthly => 1,
        KeyLimitPeriod::Daily => 2,
    });
    match exceeded.first() {
        None => KeyLimitDecision::Allowed,
        Some(status) => {
            let retry_after_secs = status
                .resets_at
                .as_deref()
                .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                .map(|end| (end.with_timezone(&Utc) - now).num_seconds().max(1) as u64);
            KeyLimitDecision::Exceeded {
                status: (*status).clone(),
                retry_after_secs,
            }
        }
    }
}

/// Shared limit table and counters. Cloned into the admission middleware and the usage recorder.
pub struct RequestKeyLimiter {
    redis: Option<Arc<RedisStore>>,
    /// Limits per key id; keys absent here are unlimited.
    limits: RwLock<Arc<HashMap<String, Vec<KeyUsageLimit>>>>,
}

impl RequestKeyLimiter {
    pub fn new(redis: Option<Arc<RedisStore>>) -> Self {
        Self {
            redis,
            limits: RwLock::new(Arc::new(HashMap::new())),
        }
    }

    /// Replaces the limit table; keys with an empty list are dropped.
    pub fn update_limits(&self, limits: impl IntoIterator<Item = (String, Vec<KeyUsageLimit>)>) {
        let table = limits
            .into_iter()
            .filter(|(_, limits)| !limits.is_empty())
            .collect::<HashMap<_, _>>();
        *self.limits.write() = Arc::new(table);
    }

    pub fn limits_for(&self, key_id: &str) -> Option<Vec<KeyUsageLimit>> {
        self.limits.read().get(key_id).cloned()
    }

    /// Reads current usage for every limit of the key. `Ok(None)` when the key has no limits.
    pub async fn statuses(
        &self,
        key_id: &str,
        now: DateTime<Utc>,
    ) -> anyhow::Result<Option<Vec<KeyLimitStatus>>> {
        let Some(limits) = self.limits_for(key_id) else {
            return Ok(None);
        };
        let redis = self
            .redis
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("未配置 Redis，无法统计 Key 用量"))?;
        let mut statuses = Vec::with_capacity(limits.len());
        for limit in limits {
            let (window, resets_at) = period_window(limit.period, now);
            let key = counter_key(key_id, limit.period, &window);
            let used = tokio::time::timeout(
                REDIS_TIMEOUT,
                redis.hash_get_f64(&key, limit.metric.as_str()),
            )
            .await
            .map_err(|_| anyhow::anyhow!("读取 Key 用量超时"))??
            .unwrap_or(0.0);
            statuses.push(KeyLimitStatus {
                metric: limit.metric,
                period: limit.period,
                limit: limit.limit,
                used,
                exceeded: used >= limit.limit,
                resets_at: resets_at.map(|time| time.to_rfc3339()),
            });
        }
        Ok(Some(statuses))
    }

    /// Admission check. Keys without limits return `Allowed` without any Redis call.
    pub async fn check(&self, key_id: &str) -> KeyLimitDecision {
        let now = Utc::now();
        match self.statuses(key_id, now).await {
            Ok(None) => KeyLimitDecision::Allowed,
            Ok(Some(statuses)) => decide(&statuses, now),
            Err(err) => KeyLimitDecision::Unavailable(err.to_string()),
        }
    }

    /// Adds a finished request's usage to every window the key is limited on. Runs on its own
    /// task; a failure is logged and the usage is not retried.
    pub fn spawn_record(self: &Arc<Self>, key_id: String, delta: KeyUsageDelta) {
        if delta.is_empty() {
            return;
        }
        let Some(limits) = self.limits_for(&key_id) else {
            return;
        };
        let Some(redis) = self.redis.clone() else {
            return;
        };
        // Usage is recorded from request handlers; outside a runtime there is nothing to count.
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        runtime.spawn(async move {
            let now = Utc::now();
            // Every window the key is limited on tracks all three metrics, so a limit added
            // later on another metric of the same period already has its live count.
            let mut periods: Vec<KeyLimitPeriod> = limits.iter().map(|limit| limit.period).collect();
            periods.sort_by_key(|period| period.as_str());
            periods.dedup();
            let fields: Vec<(&'static str, f64)> = ALL_METRICS
                .into_iter()
                .map(|metric| (metric.as_str(), delta.get(metric)))
                .filter(|(_, value)| value.is_finite() && *value > 0.0)
                .collect();
            for period in periods {
                let (window, _) = period_window(period, now);
                let key = counter_key(&key_id, period, &window);
                let result = tokio::time::timeout(
                    REDIS_TIMEOUT,
                    redis.hash_incr_by_float(&key, COUNTED_MARKER, &fields, counter_ttl_secs(period)),
                )
                .await;
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(err)) => {
                        tracing::warn!(key_id = %key_id, period = period.as_str(), "累计 Key 用量失败: {}", err)
                    }
                    Err(_) => {
                        tracing::warn!(key_id = %key_id, period = period.as_str(), "累计 Key 用量超时")
                    }
                }
            }
        });
    }

    /// Starts the current window from usage recorded before the limit existed. Only a window
    /// that does not exist yet is written (a limit just added, or counters lost with Redis); a
    /// window that live requests, an earlier seed or a reset already created is left as is.
    pub async fn seed_from_history(
        &self,
        key_id: &str,
        period: KeyLimitPeriod,
        totals: KeyUsageDelta,
    ) -> anyhow::Result<bool> {
        let redis = self
            .redis
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("未配置 Redis"))?;
        let (window, _) = period_window(period, Utc::now());
        let key = counter_key(key_id, period, &window);
        let fields: Vec<(&'static str, f64)> = ALL_METRICS
            .into_iter()
            .map(|metric| (metric.as_str(), totals.get(metric).max(0.0)))
            .filter(|(_, value)| value.is_finite())
            .collect();
        redis
            .hash_init_if_absent(&key, COUNTED_MARKER, &fields, counter_ttl_secs(period))
            .await
    }

    /// Sets one window back to zero (admin reset). The window stays in place, so a later
    /// startup or limit change does not add history back into it.
    pub async fn reset(&self, key_id: &str, period: KeyLimitPeriod) -> anyhow::Result<()> {
        let redis = self
            .redis
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("未配置 Redis"))?;
        let (window, _) = period_window(period, Utc::now());
        let zeros: Vec<(&'static str, f64)> = ALL_METRICS
            .into_iter()
            .map(|metric| (metric.as_str(), 0.0))
            .collect();
        redis
            .hash_replace(
                counter_key(key_id, period, &window),
                COUNTED_MARKER,
                &zeros,
                counter_ttl_secs(period),
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn status(period: KeyLimitPeriod, used: f64, limit: f64, now: DateTime<Utc>) -> KeyLimitStatus {
        let (_, resets_at) = period_window(period, now);
        KeyLimitStatus {
            metric: KeyLimitMetric::CostUsd,
            period,
            limit,
            used,
            exceeded: used >= limit,
            resets_at: resets_at.map(|time| time.to_rfc3339()),
        }
    }

    #[test]
    fn windows_follow_china_time() {
        // 2026-10-23 16:30 UTC is 2026-10-24 00:30 in UTC+8.
        let now = at("2026-10-23T16:30:00Z");
        let (day, day_end) = period_window(KeyLimitPeriod::Daily, now);
        assert_eq!(day, "20261024");
        assert_eq!(day_end.unwrap(), at("2026-10-24T16:00:00Z"));
        let (month, month_end) = period_window(KeyLimitPeriod::Monthly, now);
        assert_eq!(month, "202610");
        assert_eq!(month_end.unwrap(), at("2026-10-31T16:00:00Z"));
        // December rolls into January of the next year.
        let (_, december_end) = period_window(KeyLimitPeriod::Monthly, at("2026-12-15T00:00:00Z"));
        assert_eq!(december_end.unwrap(), at("2026-12-31T16:00:00Z"));
        assert_eq!(
            period_window(KeyLimitPeriod::Total, now),
            ("all".to_string(), None)
        );
    }

    #[test]
    fn period_start_is_local_midnight_or_first_of_month() {
        let now = at("2026-10-23T16:30:00Z"); // 2026-10-24 00:30 UTC+8
        assert_eq!(
            period_start(KeyLimitPeriod::Daily, now).unwrap(),
            at("2026-10-23T16:00:00Z")
        );
        assert_eq!(
            period_start(KeyLimitPeriod::Monthly, now).unwrap(),
            at("2026-09-30T16:00:00Z")
        );
        assert_eq!(period_start(KeyLimitPeriod::Total, now), None);
    }

    #[test]
    fn validation_rejects_bad_limits() {
        let limit = |metric, period, limit| KeyUsageLimit {
            metric,
            period,
            limit,
        };
        assert!(
            validate_key_limits(&[limit(KeyLimitMetric::CostUsd, KeyLimitPeriod::Daily, 5.0)])
                .is_ok()
        );
        assert!(
            validate_key_limits(&[limit(KeyLimitMetric::CostUsd, KeyLimitPeriod::Daily, 0.0)])
                .is_err()
        );
        assert!(
            validate_key_limits(&[limit(
                KeyLimitMetric::Credits,
                KeyLimitPeriod::Daily,
                f64::NAN
            )])
            .is_err()
        );
        assert!(
            validate_key_limits(&[limit(KeyLimitMetric::Tokens, KeyLimitPeriod::Monthly, 10.5)])
                .is_err()
        );
        assert!(
            validate_key_limits(&[
                limit(KeyLimitMetric::Credits, KeyLimitPeriod::Daily, 1.0),
                limit(KeyLimitMetric::Credits, KeyLimitPeriod::Daily, 2.0),
            ])
            .is_err()
        );
        // The same metric on different periods is fine.
        assert!(
            validate_key_limits(&[
                limit(KeyLimitMetric::Credits, KeyLimitPeriod::Daily, 1.0),
                limit(KeyLimitMetric::Credits, KeyLimitPeriod::Monthly, 20.0),
            ])
            .is_ok()
        );
    }

    #[test]
    fn decision_reports_the_longest_window_and_time_to_reset() {
        let now = at("2026-10-23T04:00:00Z"); // 12:00 in UTC+8
        assert_eq!(
            decide(&[status(KeyLimitPeriod::Daily, 4.9, 5.0, now)], now),
            KeyLimitDecision::Allowed
        );
        match decide(&[status(KeyLimitPeriod::Daily, 5.0, 5.0, now)], now) {
            KeyLimitDecision::Exceeded {
                retry_after_secs, ..
            } => assert_eq!(retry_after_secs, Some(12 * 3600)),
            other => panic!("unexpected {other:?}"),
        }
        match decide(
            &[
                status(KeyLimitPeriod::Daily, 6.0, 5.0, now),
                status(KeyLimitPeriod::Total, 100.0, 50.0, now),
            ],
            now,
        ) {
            KeyLimitDecision::Exceeded {
                status,
                retry_after_secs,
            } => {
                assert_eq!(status.period, KeyLimitPeriod::Total);
                assert_eq!(retry_after_secs, None);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn delta_reports_empty_when_nothing_was_used() {
        assert!(KeyUsageDelta::default().is_empty());
        assert!(
            KeyUsageDelta {
                cost_usd: f64::NAN,
                ..Default::default()
            }
            .is_empty()
        );
        assert!(
            !KeyUsageDelta {
                tokens: 1.0,
                ..Default::default()
            }
            .is_empty()
        );
    }

    async fn redis_limiter() -> Option<(Arc<RedisStore>, Arc<RequestKeyLimiter>)> {
        let url = crate::storage::integration_test_url("KIRO_RS_TEST_REDIS_URL")?;
        let mut config = crate::model::config::Config::default();
        config.redis.url = Some(url);
        config.redis.key_prefix = format!("kiro_rs:test:{}", uuid::Uuid::new_v4());
        let redis = Arc::new(RedisStore::connect(&config).await.unwrap());
        Some((redis.clone(), Arc::new(RequestKeyLimiter::new(Some(redis)))))
    }

    /// Waits for the background counter write to land.
    async fn used(
        limiter: &RequestKeyLimiter,
        key: &str,
        metric: KeyLimitMetric,
        period: KeyLimitPeriod,
        expected: f64,
    ) -> f64 {
        let mut last = 0.0;
        for _ in 0..100 {
            let statuses = limiter.statuses(key, Utc::now()).await.unwrap().unwrap();
            last = statuses
                .iter()
                .find(|status| status.metric == metric && status.period == period)
                .unwrap()
                .used;
            if (last - expected).abs() < 1e-9 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        last
    }

    #[tokio::test]
    async fn limits_count_reject_and_reset_through_redis() {
        let Some((redis, limiter)) = redis_limiter().await else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };
        let key = "key-under-test";
        limiter.update_limits([(
            key.to_string(),
            vec![
                KeyUsageLimit {
                    metric: KeyLimitMetric::CostUsd,
                    period: KeyLimitPeriod::Daily,
                    limit: 1.0,
                },
                KeyUsageLimit {
                    metric: KeyLimitMetric::Credits,
                    period: KeyLimitPeriod::Monthly,
                    limit: 10.0,
                },
                KeyUsageLimit {
                    metric: KeyLimitMetric::Tokens,
                    period: KeyLimitPeriod::Total,
                    limit: 1_000.0,
                },
            ],
        )]);
        assert_eq!(limiter.check(key).await, KeyLimitDecision::Allowed);

        limiter.spawn_record(
            key.to_string(),
            KeyUsageDelta {
                cost_usd: 0.6,
                credits: 2.5,
                tokens: 300.0,
            },
        );
        assert!(
            (used(
                &limiter,
                key,
                KeyLimitMetric::CostUsd,
                KeyLimitPeriod::Daily,
                0.6
            )
            .await
                - 0.6)
                .abs()
                < 1e-9
        );
        assert!(
            (used(
                &limiter,
                key,
                KeyLimitMetric::Credits,
                KeyLimitPeriod::Monthly,
                2.5
            )
            .await
                - 2.5)
                .abs()
                < 1e-9
        );
        assert!(
            (used(
                &limiter,
                key,
                KeyLimitMetric::Tokens,
                KeyLimitPeriod::Total,
                300.0
            )
            .await
                - 300.0)
                .abs()
                < 1e-9
        );
        assert_eq!(limiter.check(key).await, KeyLimitDecision::Allowed);

        // Crossing the daily billed-amount cap rejects with time to the next local midnight.
        limiter.spawn_record(
            key.to_string(),
            KeyUsageDelta {
                cost_usd: 0.5,
                credits: 0.0,
                tokens: 10.0,
            },
        );
        used(
            &limiter,
            key,
            KeyLimitMetric::CostUsd,
            KeyLimitPeriod::Daily,
            1.1,
        )
        .await;
        match limiter.check(key).await {
            KeyLimitDecision::Exceeded {
                status,
                retry_after_secs,
            } => {
                assert_eq!(
                    (status.metric, status.period),
                    (KeyLimitMetric::CostUsd, KeyLimitPeriod::Daily)
                );
                let secs = retry_after_secs.unwrap();
                assert!((1..=24 * 3600).contains(&secs), "{secs}");
            }
            other => panic!("expected exceeded, got {other:?}"),
        }

        // Reset clears only that window.
        limiter.reset(key, KeyLimitPeriod::Daily).await.unwrap();
        assert_eq!(limiter.check(key).await, KeyLimitDecision::Allowed);
        assert!(
            (used(
                &limiter,
                key,
                KeyLimitMetric::Tokens,
                KeyLimitPeriod::Total,
                310.0
            )
            .await
                - 310.0)
                .abs()
                < 1e-9
        );

        // A window that already has counts never takes history: live counts are authoritative.
        let history = KeyUsageDelta {
            cost_usd: 0.25,
            credits: 1.0,
            tokens: 100.0,
        };
        assert!(
            !limiter
                .seed_from_history(key, KeyLimitPeriod::Total, history)
                .await
                .unwrap()
        );
        assert!(
            (used(
                &limiter,
                key,
                KeyLimitMetric::Tokens,
                KeyLimitPeriod::Total,
                310.0
            )
            .await
                - 310.0)
                .abs()
                < 1e-9
        );
        // A reset window stays at zero even when history is offered again (restart, saving limits).
        assert!(
            !limiter
                .seed_from_history(key, KeyLimitPeriod::Daily, history)
                .await
                .unwrap()
        );
        assert_eq!(
            used(
                &limiter,
                key,
                KeyLimitMetric::CostUsd,
                KeyLimitPeriod::Daily,
                0.0
            )
            .await,
            0.0
        );
        // A window that is gone (Redis lost it) is rebuilt from history, once.
        let (month, _) = period_window(KeyLimitPeriod::Monthly, Utc::now());
        redis
            .del(counter_key(key, KeyLimitPeriod::Monthly, &month))
            .await
            .unwrap();
        assert!(
            limiter
                .seed_from_history(key, KeyLimitPeriod::Monthly, history)
                .await
                .unwrap()
        );
        assert!(
            !limiter
                .seed_from_history(key, KeyLimitPeriod::Monthly, history)
                .await
                .unwrap()
        );
        assert!(
            (used(
                &limiter,
                key,
                KeyLimitMetric::Credits,
                KeyLimitPeriod::Monthly,
                1.0
            )
            .await
                - 1.0)
                .abs()
                < 1e-9
        );

        // Each window tracks every metric, even ones without a limit on it.
        limiter.spawn_record(
            key.to_string(),
            KeyUsageDelta {
                cost_usd: 0.0,
                credits: 0.0,
                tokens: 5.0,
            },
        );
        let monthly_tokens = {
            let (month, _) = period_window(KeyLimitPeriod::Monthly, Utc::now());
            let mut value = 0.0;
            for _ in 0..100 {
                value = redis
                    .hash_get_f64(counter_key(key, KeyLimitPeriod::Monthly, &month), "tokens")
                    .await
                    .unwrap()
                    .unwrap_or(0.0);
                if value > 0.0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            value
        };
        assert!(monthly_tokens >= 5.0, "{monthly_tokens}");

        // Daily windows expire; the lifetime window does not.
        let (day, _) = period_window(KeyLimitPeriod::Daily, Utc::now());
        let day_ttl = redis
            .ttl_secs_for_test(counter_key(key, KeyLimitPeriod::Daily, &day))
            .await
            .unwrap();
        assert!(day_ttl > 0 && day_ttl <= 3 * 24 * 3600, "{day_ttl}");
        let total_ttl = redis
            .ttl_secs_for_test(counter_key(key, KeyLimitPeriod::Total, "all"))
            .await
            .unwrap();
        assert_eq!(total_ttl, -1);

        redis.delete_pattern_bounded("*", None).await.unwrap();
    }

    #[tokio::test]
    async fn keys_without_limits_never_need_redis() {
        let limiter = RequestKeyLimiter::new(None);
        assert_eq!(limiter.check("free").await, KeyLimitDecision::Allowed);
        limiter.update_limits([(
            "capped".to_string(),
            vec![KeyUsageLimit {
                metric: KeyLimitMetric::Tokens,
                period: KeyLimitPeriod::Daily,
                limit: 10.0,
            }],
        )]);
        assert_eq!(limiter.check("free").await, KeyLimitDecision::Allowed);
        // A limited key with no Redis is rejected, never let through unaccounted.
        assert!(matches!(
            limiter.check("capped").await,
            KeyLimitDecision::Unavailable(_)
        ));
        // Empty lists are dropped.
        limiter.update_limits([("capped".to_string(), Vec::new())]);
        assert_eq!(limiter.check("capped").await, KeyLimitDecision::Allowed);
    }
}
