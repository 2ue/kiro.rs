//! Account change log: quota exhausted, auto-disabled, risk control, recovered and manual
//! enable/disable for accounts.
//!
//! Events are rows in `admin_audit_logs` whose `detail` carries an `accountEvent` kind, so the
//! audit page can show them as their own view. Recording never blocks or fails the caller: the
//! write goes to the bounded best-effort storage queue and a full queue or a failed insert only
//! logs a warning.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::common::storage_task::spawn_best_effort_storage_task;
use crate::model::config::{MAX_AUDIT_LOG_RETENTION_DAYS, MIN_AUDIT_LOG_RETENTION_DAYS};
use crate::storage::postgres::PostgresStore;

/// Kind of account change, stored as `detail.accountEvent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountEventKind {
    /// Base quota used up; the account stops taking requests.
    QuotaExhausted,
    /// Disabled automatically for failures, an invalid token or configuration.
    AutoDisabled,
    /// Disabled because upstream reported risk control, suspension or a lock.
    RiskControlled,
    /// Disabled by an operator.
    ManualDisabled,
    /// Enabled or reset by an operator.
    ManualEnabled,
    /// Deleted by an operator.
    Deleted,
    /// The scheduler put the account (or some of its models) into cooldown after an upstream
    /// failure; it is skipped until the cooldown expires.
    CooldownStarted,
    /// Relative quality degradation put the account into probation (scored down for a while).
    ProbationStarted,
}

impl AccountEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::QuotaExhausted => "quota_exhausted",
            Self::AutoDisabled => "auto_disabled",
            Self::RiskControlled => "risk_controlled",
            Self::ManualDisabled => "manual_disabled",
            Self::ManualEnabled => "manual_enabled",
            Self::Deleted => "deleted",
            Self::CooldownStarted => "cooldown_started",
            Self::ProbationStarted => "probation_started",
        }
    }
}

/// Per-process throttle for high-frequency account events (cooldowns fire on every failed
/// attempt): the same account + reason is recorded at most once per window. Bounded; when
/// full it is cleared rather than grown.
pub struct AccountEventThrottle {
    recent: parking_lot::Mutex<std::collections::HashMap<(u64, String), std::time::Instant>>,
    window: Duration,
    capacity: usize,
}

pub const ACCOUNT_EVENT_THROTTLE_WINDOW: Duration = Duration::from_secs(60);
/// 冷却事件的节流窗口更长：模型级冷却默认只有 10 秒，持续命中时按 60 秒记一条，
/// 审计表（清理默认关闭）会持续膨胀；5 分钟一条足够看出趋势。
pub const ACCOUNT_COOLDOWN_EVENT_THROTTLE_WINDOW: Duration = Duration::from_secs(300);
pub const ACCOUNT_EVENT_THROTTLE_CAPACITY: usize = 4096;

impl Default for AccountEventThrottle {
    fn default() -> Self {
        Self::new(
            ACCOUNT_EVENT_THROTTLE_WINDOW,
            ACCOUNT_EVENT_THROTTLE_CAPACITY,
        )
    }
}

impl AccountEventThrottle {
    pub fn new(window: Duration, capacity: usize) -> Self {
        Self {
            recent: parking_lot::Mutex::new(std::collections::HashMap::new()),
            window,
            capacity: capacity.max(1),
        }
    }

    /// `true` when this account + reason has not been recorded within the window; marks it.
    pub fn should_record(&self, account_id: u64, reason: &str) -> bool {
        self.should_record_at(account_id, reason, std::time::Instant::now())
    }

    pub fn should_record_at(&self, account_id: u64, reason: &str, now: std::time::Instant) -> bool {
        self.should_record_with_window_at(account_id, reason, self.window, now)
    }

    /// 同 `should_record`，但用调用方给的窗口（冷却事件用更长的窗口）。
    pub fn should_record_with_window(&self, account_id: u64, key: &str, window: Duration) -> bool {
        self.should_record_with_window_at(account_id, key, window, std::time::Instant::now())
    }

    pub fn should_record_with_window_at(
        &self,
        account_id: u64,
        reason: &str,
        window: Duration,
        now: std::time::Instant,
    ) -> bool {
        let mut recent = self.recent.lock();
        let key = (account_id, reason.to_string());
        if let Some(last) = recent.get(&key)
            && now.saturating_duration_since(*last) < window
        {
            return false;
        }
        if recent.len() >= self.capacity && !recent.contains_key(&key) {
            recent.clear();
        }
        recent.insert(key, now);
        true
    }
}

/// Which pool the account belongs to, stored as `detail.accountSource`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountSource {
    External,
}

impl AccountSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::External => "external",
        }
    }

    fn object_type(self) -> &'static str {
        match self {
            Self::External => "account",
        }
    }
}

/// One account change. `reason` is a short machine key; `reason_label` is what the page shows.
#[derive(Debug, Clone)]
pub struct AccountEvent {
    pub kind: AccountEventKind,
    pub source: AccountSource,
    pub account_id: u64,
    pub account_label: String,
    pub reason: String,
    pub reason_label: String,
    pub detail: Value,
}

impl AccountEvent {
    /// Audit `action` and `detail` for this event. Extra detail keys are kept unless they clash
    /// with the fixed account-event keys.
    pub fn into_audit_parts(self) -> (&'static str, &'static str, String, Value) {
        let action = match self.source {
            AccountSource::External => "account_account_event",
        };
        let mut detail = match self.detail {
            Value::Object(map) => map,
            Value::Null => Map::new(),
            other => {
                let mut map = Map::new();
                map.insert("extra".to_string(), other);
                map
            }
        };
        for (key, value) in [
            ("accountEvent", json!(self.kind.as_str())),
            ("accountSource", json!(self.source.as_str())),
            ("accountLabel", json!(self.account_label)),
            ("reason", json!(self.reason)),
            ("reasonLabel", json!(self.reason_label)),
        ] {
            detail.insert(key.to_string(), value);
        }
        (
            action,
            self.source.object_type(),
            self.account_id.to_string(),
            Value::Object(detail),
        )
    }
}

/// Queues the event for writing and returns at once. Returns `false` when the queue was full;
/// the event is then dropped and only a warning is logged.
pub fn record_account_event(
    store: Arc<PostgresStore>,
    actor: &'static str,
    event: AccountEvent,
) -> bool {
    let (action, object_type, object_id, detail) = event.into_audit_parts();
    spawn_best_effort_storage_task("记录账号异动日志", async move {
        store
            .record_admin_audit_log(
                actor,
                action,
                object_type,
                Some(&object_id),
                true,
                None,
                detail,
            )
            .await
    })
}

/// Event kind and label for an account auto-disable reason.
pub fn account_disable_event(reason: &str) -> (AccountEventKind, &'static str) {
    match reason {
        "quota_exhausted" => (AccountEventKind::QuotaExhausted, "额度耗尽"),
        "security_lock" => (AccountEventKind::RiskControlled, "安全锁定"),
        "auth_error" => (AccountEventKind::AutoDisabled, "鉴权失败"),
        "misconfigured_endpoint" => (AccountEventKind::AutoDisabled, "端点配置错误"),
        "channel_disabled" => (AccountEventKind::AutoDisabled, "上游渠道已禁用"),
        _ => (AccountEventKind::AutoDisabled, "连续失败"),
    }
}

/// Derived data: EWMA snapshots of account quality, kept for a fixed week regardless of the
/// usage retention switch (nothing in it is a record of a real request).
pub const ACCOUNT_QUALITY_SAMPLE_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const ACCOUNT_QUALITY_SAMPLE_RETENTION_BATCH: usize = 2_000;

/// Hourly cleanup of `account_quality_samples` older than a week. Own task, bounded per run.
pub fn spawn_account_quality_sample_retention_worker(
    store: Arc<PostgresStore>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        tokio::time::sleep(AUDIT_RETENTION_FIRST_RUN_DELAY).await;
        let mut interval = tokio::time::interval(AUDIT_RETENTION_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            let result = run_audit_retention_once(
                |limit| {
                    let store = store.clone();
                    async move {
                        store
                            .delete_expired_account_quality_samples(
                                ACCOUNT_QUALITY_SAMPLE_RETENTION,
                                limit,
                            )
                            .await
                    }
                },
                ACCOUNT_QUALITY_SAMPLE_RETENTION_BATCH,
                AUDIT_RETENTION_MAX_BATCHES,
                AUDIT_RETENTION_RUN_BUDGET,
                AUDIT_RETENTION_BATCH_PAUSE,
            )
            .await;
            match result {
                Ok(report) if report.removed > 0 || report.saturated => tracing::info!(
                    removed = report.removed,
                    batches = report.batches,
                    saturated = report.saturated,
                    "已清理过期账号质量采样"
                ),
                Ok(_) => {}
                Err(err) => tracing::warn!("清理过期账号质量采样失败，下次再试: {}", err),
            }
        }
    })
}

/// How often the retention job wakes up. The first run waits a few minutes after startup.
const AUDIT_RETENTION_INTERVAL: Duration = Duration::from_secs(60 * 60);
const AUDIT_RETENTION_FIRST_RUN_DELAY: Duration = Duration::from_secs(5 * 60);
/// Rows per DELETE. Small enough that each statement holds row locks only briefly.
const AUDIT_RETENTION_BATCH: usize = 1_000;
/// Pause between batches so cleanup yields the database to request traffic.
const AUDIT_RETENTION_BATCH_PAUSE: Duration = Duration::from_millis(200);
/// Upper bounds for one run; whatever is left goes to the next run.
const AUDIT_RETENTION_MAX_BATCHES: usize = 50;
const AUDIT_RETENTION_RUN_BUDGET: Duration = Duration::from_secs(60);

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AuditRetentionReport {
    pub removed: u64,
    pub batches: usize,
    /// The run stopped at the batch or time limit with rows possibly left.
    pub saturated: bool,
}

/// One retention pass: delete in batches until nothing old is left or a limit is hit. A failed
/// batch ends the pass; the error is returned for logging and nothing is retried here.
pub async fn run_audit_retention_once<F, Fut>(
    mut delete_batch: F,
    batch: usize,
    max_batches: usize,
    budget: Duration,
    pause: Duration,
) -> anyhow::Result<AuditRetentionReport>
where
    F: FnMut(usize) -> Fut,
    Fut: Future<Output = anyhow::Result<u64>>,
{
    let deadline = tokio::time::Instant::now() + budget;
    let mut report = AuditRetentionReport::default();
    while report.batches < max_batches {
        if tokio::time::Instant::now() >= deadline {
            report.saturated = true;
            return Ok(report);
        }
        let removed = delete_batch(batch).await?;
        report.batches += 1;
        report.removed = report.removed.saturating_add(removed);
        if removed < batch as u64 {
            return Ok(report);
        }
        tokio::time::sleep(pause).await;
    }
    report.saturated = true;
    Ok(report)
}

/// Background job that keeps `admin_audit_logs` (account changes included) to the configured
/// retention. It runs on its own task, never on the request path; every failure is logged and
/// the next run tries again. `retention_days` is read each run and returns `None` while cleanup
/// is switched off (the default), in which case nothing is deleted.
pub fn spawn_audit_retention_worker(
    store: Arc<PostgresStore>,
    retention_days: impl Fn() -> Option<u32> + Send + Sync + 'static,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        tokio::time::sleep(AUDIT_RETENTION_FIRST_RUN_DELAY).await;
        let mut interval = tokio::time::interval(AUDIT_RETENTION_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            // Off by default: nothing is deleted until an operator enables cleanup.
            let Some(days) = retention_days() else {
                continue;
            };
            let days = days.clamp(MIN_AUDIT_LOG_RETENTION_DAYS, MAX_AUDIT_LOG_RETENTION_DAYS);
            let retention = Duration::from_secs(u64::from(days) * 24 * 60 * 60);
            let result = run_audit_retention_once(
                |limit| {
                    let store = store.clone();
                    async move {
                        store
                            .delete_expired_admin_audit_logs(retention, limit)
                            .await
                    }
                },
                AUDIT_RETENTION_BATCH,
                AUDIT_RETENTION_MAX_BATCHES,
                AUDIT_RETENTION_RUN_BUDGET,
                AUDIT_RETENTION_BATCH_PAUSE,
            )
            .await;
            match result {
                Ok(report) if report.removed > 0 || report.saturated => tracing::info!(
                    retention_days = days,
                    removed = report.removed,
                    batches = report.batches,
                    saturated = report.saturated,
                    "已清理过期审计日志"
                ),
                Ok(_) => {}
                Err(err) => tracing::warn!(
                    retention_days = days,
                    "清理过期审计日志失败，下次再试，不影响调度: {}",
                    err
                ),
            }
        }
    })
}

/// Usage detail retention: hourly like the audit job, smaller batches because each delete
/// also touches the rollup commit guard. Off by default; nothing is deleted until an operator
/// enables it in the runtime config. Rollups are never touched, only `usage_records`.
const USAGE_RETENTION_BATCH: usize = 1_000;
const USAGE_RETENTION_MAX_BATCHES: usize = 30;
const USAGE_RETENTION_RUN_BUDGET: Duration = Duration::from_secs(60);
const USAGE_RETENTION_BATCH_PAUSE: Duration = Duration::from_millis(300);

/// Background job that keeps `usage_records` to the configured retention. Same shape as the
/// audit retention worker: own task, never on the request path, each run bounded, failures
/// logged and retried next hour. `retention_days` returns `None` while cleanup is off.
pub fn spawn_usage_retention_worker(
    store: Arc<PostgresStore>,
    retention_days: impl Fn() -> Option<u32> + Send + Sync + 'static,
) -> tokio::task::JoinHandle<()> {
    use crate::model::config::{MAX_USAGE_RECORD_RETENTION_DAYS, MIN_USAGE_RECORD_RETENTION_DAYS};
    use crate::storage::postgres::PostgresUsageStore;

    let usage_store = PostgresUsageStore::new(store);
    tokio::spawn(async move {
        tokio::time::sleep(AUDIT_RETENTION_FIRST_RUN_DELAY).await;
        let mut interval = tokio::time::interval(AUDIT_RETENTION_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            let Some(days) = retention_days() else {
                continue;
            };
            let days = days.clamp(
                MIN_USAGE_RECORD_RETENTION_DAYS,
                MAX_USAGE_RECORD_RETENTION_DAYS,
            );
            let retention = Duration::from_secs(u64::from(days) * 24 * 60 * 60);
            let result = run_audit_retention_once(
                |limit| {
                    let usage_store = usage_store.clone();
                    async move {
                        usage_store
                            .delete_expired_usage_records(retention, limit)
                            .await
                    }
                },
                USAGE_RETENTION_BATCH,
                USAGE_RETENTION_MAX_BATCHES,
                USAGE_RETENTION_RUN_BUDGET,
                USAGE_RETENTION_BATCH_PAUSE,
            )
            .await;
            match result {
                Ok(report) if report.removed > 0 || report.saturated => tracing::info!(
                    retention_days = days,
                    removed = report.removed,
                    batches = report.batches,
                    saturated = report.saturated,
                    "已清理过期用量明细（汇总与趋势不受影响）"
                ),
                Ok(_) => {}
                Err(err) => tracing::warn!(
                    retention_days = days,
                    "清理过期用量明细失败，下次再试，不影响调度: {}",
                    err
                ),
            }
            // 会话维度汇总只服务"会话排行"，一条会话一行且永不分桶；按最后活跃时间裁掉过期的，
            // 每轮一小批，剩下的下轮再来。
            match usage_store
                .delete_stale_conversation_rollups(retention, USAGE_RETENTION_BATCH)
                .await
            {
                Ok(removed) if removed > 0 => {
                    tracing::info!(retention_days = days, removed, "已裁剪过期会话维度汇总")
                }
                Ok(_) => {}
                Err(err) => tracing::warn!(
                    retention_days = days,
                    "裁剪过期会话维度汇总失败，下次再试: {}",
                    err
                ),
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn external_events_use_their_own_action_and_object_type() {
        let event = AccountEvent {
            kind: AccountEventKind::AutoDisabled,
            source: AccountSource::External,
            account_id: 3,
            account_label: "pool".to_string(),
            reason: "quota_exhausted".to_string(),
            reason_label: "额度耗尽".to_string(),
            detail: Value::Null,
        };
        let (action, object_type, _, detail) = event.into_audit_parts();
        assert_eq!(action, "account_account_event");
        assert_eq!(object_type, "account");
        assert_eq!(detail["accountSource"], "external");
    }

    #[test]
    fn throttle_records_same_account_and_reason_once_per_window() {
        let throttle = AccountEventThrottle::new(Duration::from_secs(60), 16);
        let start = std::time::Instant::now();
        assert!(throttle.should_record_at(7, "rate_limit", start));
        assert!(!throttle.should_record_at(7, "rate_limit", start + Duration::from_secs(30)));
        // 另一个原因、另一个账号互不影响。
        assert!(throttle.should_record_at(7, "overloaded", start + Duration::from_secs(30)));
        assert!(throttle.should_record_at(8, "rate_limit", start + Duration::from_secs(30)));
        // 窗口过后再次记录。
        assert!(throttle.should_record_at(7, "rate_limit", start + Duration::from_secs(60)));
    }

    #[test]
    fn cooldown_throttle_uses_its_own_longer_window() {
        let throttle = AccountEventThrottle::default();
        let start = std::time::Instant::now();
        let window = ACCOUNT_COOLDOWN_EVENT_THROTTLE_WINDOW;
        assert!(throttle.should_record_with_window_at(
            7,
            "cooldown:model_unavailable:a",
            window,
            start
        ));
        // 普通 60 秒窗口过了，冷却事件仍在自己的 5 分钟窗口里。
        assert!(!throttle.should_record_with_window_at(
            7,
            "cooldown:model_unavailable:a",
            window,
            start + Duration::from_secs(120)
        ));
        // 另一个模型的冷却单独记录。
        assert!(throttle.should_record_with_window_at(
            7,
            "cooldown:model_unavailable:b",
            window,
            start + Duration::from_secs(120)
        ));
        assert!(throttle.should_record_with_window_at(
            7,
            "cooldown:model_unavailable:a",
            window,
            start + window
        ));
    }

    #[test]
    fn throttle_clears_when_full_instead_of_growing() {
        let throttle = AccountEventThrottle::new(Duration::from_secs(60), 2);
        let now = std::time::Instant::now();
        assert!(throttle.should_record_at(1, "a", now));
        assert!(throttle.should_record_at(2, "a", now));
        assert_eq!(throttle.recent.lock().len(), 2);
        // 第三个键触发清空，之后只剩它自己。
        assert!(throttle.should_record_at(3, "a", now));
        assert_eq!(throttle.recent.lock().len(), 1);
        // 被清掉的键会再次放行（容量上限优先于精确去重）。
        assert!(throttle.should_record_at(1, "a", now));
    }

    #[test]
    fn account_reasons_map_to_kinds() {
        assert_eq!(
            account_disable_event("quota_exhausted").0,
            AccountEventKind::QuotaExhausted
        );
        assert_eq!(
            account_disable_event("security_lock").0,
            AccountEventKind::RiskControlled
        );
        assert_eq!(
            account_disable_event("auth_error"),
            (AccountEventKind::AutoDisabled, "鉴权失败")
        );
        assert_eq!(
            account_disable_event("unexpected").0,
            AccountEventKind::AutoDisabled
        );
    }

    #[tokio::test]
    async fn retention_stops_when_a_batch_comes_back_short() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let report = run_audit_retention_once(
            move |limit| {
                let call = counter.fetch_add(1, Ordering::SeqCst);
                async move { Ok(if call < 2 { limit as u64 } else { 7 }) }
            },
            100,
            50,
            Duration::from_secs(60),
            Duration::from_millis(1),
        )
        .await
        .unwrap();
        assert_eq!(
            report,
            AuditRetentionReport {
                removed: 207,
                batches: 3,
                saturated: false
            }
        );
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn retention_stops_at_the_batch_limit() {
        let report = run_audit_retention_once(
            |limit| async move { Ok(limit as u64) },
            10,
            4,
            Duration::from_secs(3600),
            Duration::from_millis(1),
        )
        .await
        .unwrap();
        assert_eq!(
            report,
            AuditRetentionReport {
                removed: 40,
                batches: 4,
                saturated: true
            }
        );
    }

    #[tokio::test]
    async fn retention_stops_once_the_time_budget_is_spent() {
        // A batch that outlasts the whole budget ends the run after it; a slower machine only
        // makes that more certain.
        let report = run_audit_retention_once(
            |limit| async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok(limit as u64)
            },
            10,
            50,
            Duration::from_millis(20),
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert_eq!(
            report,
            AuditRetentionReport {
                removed: 10,
                batches: 1,
                saturated: true
            }
        );
        let nothing = run_audit_retention_once(
            |limit| async move { Ok(limit as u64) },
            10,
            50,
            Duration::ZERO,
            Duration::ZERO,
        )
        .await
        .unwrap();
        assert_eq!(nothing.batches, 0);
        assert!(nothing.saturated);
    }

    #[tokio::test]
    async fn retention_failure_is_returned_not_retried() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let result = run_audit_retention_once(
            move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                async move { Err::<u64, _>(anyhow::anyhow!("lock timeout")) }
            },
            10,
            50,
            Duration::from_secs(60),
            Duration::from_millis(1),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
