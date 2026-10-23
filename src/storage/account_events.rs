//! Account change log: quota exhausted, auto-disabled, risk control, recovered and manual
//! enable/disable for local credentials and external pools.
//!
//! Events are rows in `admin_audit_logs` whose `detail` carries an `accountEvent` kind, so the
//! audit page can show them as their own view. Recording never blocks or fails the caller: the
//! write goes to the bounded best-effort storage queue and a full queue or a failed insert only
//! logs a warning.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::kiro::token_manager::storage_task::spawn_best_effort_storage_task;
use crate::model::config::{MAX_AUDIT_LOG_RETENTION_DAYS, MIN_AUDIT_LOG_RETENTION_DAYS};
use crate::storage::postgres::{AccountQuotaFigures, PostgresStore};

/// Kind of account change, stored as `detail.accountEvent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountEventKind {
    /// Base quota used up; the account stops taking requests.
    QuotaExhausted,
    /// Quota available again after a refresh of the account info.
    QuotaRecovered,
    /// Usage went past the plan limit and overage started.
    OverageStarted,
    /// Disabled automatically for failures, an invalid token or configuration.
    AutoDisabled,
    /// Disabled because upstream reported risk control, suspension or a lock.
    RiskControlled,
    /// Re-enabled automatically.
    AutoEnabled,
    /// Disabled by an operator.
    ManualDisabled,
    /// Enabled or reset by an operator.
    ManualEnabled,
    /// Deleted by an operator.
    Deleted,
}

impl AccountEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::QuotaExhausted => "quota_exhausted",
            Self::QuotaRecovered => "quota_recovered",
            Self::OverageStarted => "overage_started",
            Self::AutoDisabled => "auto_disabled",
            Self::RiskControlled => "risk_controlled",
            Self::AutoEnabled => "auto_enabled",
            Self::ManualDisabled => "manual_disabled",
            Self::ManualEnabled => "manual_enabled",
            Self::Deleted => "deleted",
        }
    }
}

/// Which pool the account belongs to, stored as `detail.accountSource`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountSource {
    Local,
    External,
}

impl AccountSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::External => "external",
        }
    }

    fn object_type(self) -> &'static str {
        match self {
            Self::Local => "credential",
            Self::External => "external_pool",
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
            AccountSource::Local => "credential_account_event",
            AccountSource::External => "external_pool_account_event",
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

/// Quota change between two account-info snapshots, if any. The first snapshot of an account
/// is not a change and yields nothing, so syncing a pool for the first time does not flood the
/// log. Overage takes precedence: going past the limit also means the plan quota is used up.
pub fn quota_transition(
    previous: Option<AccountQuotaFigures>,
    next: AccountQuotaFigures,
) -> Option<AccountEventKind> {
    let previous = previous?;
    let used_up = |f: &AccountQuotaFigures| f.usage_limit > 0.0 && f.current_usage >= f.usage_limit;
    let in_overage = |f: &AccountQuotaFigures| {
        f.usage_limit > 0.0 && (f.current_usage > f.usage_limit || f.current_overages > 0.0)
    };
    if !in_overage(&previous) && in_overage(&next) {
        Some(AccountEventKind::OverageStarted)
    } else if !used_up(&previous) && used_up(&next) {
        Some(AccountEventKind::QuotaExhausted)
    } else if used_up(&previous) && !used_up(&next) {
        Some(AccountEventKind::QuotaRecovered)
    } else {
        None
    }
}

/// Event kind and label for an external pool auto-disable reason.
pub fn external_pool_disable_event(reason: &str) -> (AccountEventKind, &'static str) {
    match reason {
        "quota_exhausted" => (AccountEventKind::QuotaExhausted, "额度耗尽"),
        "security_lock" => (AccountEventKind::RiskControlled, "安全锁定"),
        "auth_error" => (AccountEventKind::AutoDisabled, "鉴权失败"),
        "misconfigured_endpoint" => (AccountEventKind::AutoDisabled, "端点配置错误"),
        "channel_disabled" => (AccountEventKind::AutoDisabled, "上游渠道已禁用"),
        _ => (AccountEventKind::AutoDisabled, "连续失败"),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn audit_parts_carry_fixed_keys_and_keep_extra_detail() {
        let event = AccountEvent {
            kind: AccountEventKind::QuotaExhausted,
            source: AccountSource::Local,
            account_id: 7,
            account_label: "a@example.com".to_string(),
            reason: "upstream_quota_exhausted".to_string(),
            reason_label: "额度耗尽".to_string(),
            detail: json!({ "currentUsage": 1000.0, "reason": "overwritten" }),
        };
        let (action, object_type, object_id, detail) = event.into_audit_parts();
        assert_eq!(action, "credential_account_event");
        assert_eq!(object_type, "credential");
        assert_eq!(object_id, "7");
        assert_eq!(detail["accountEvent"], "quota_exhausted");
        assert_eq!(detail["accountSource"], "local");
        assert_eq!(detail["accountLabel"], "a@example.com");
        assert_eq!(detail["reason"], "upstream_quota_exhausted");
        assert_eq!(detail["reasonLabel"], "额度耗尽");
        assert_eq!(detail["currentUsage"], 1000.0);
    }

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
        assert_eq!(action, "external_pool_account_event");
        assert_eq!(object_type, "external_pool");
        assert_eq!(detail["accountSource"], "external");
    }

    fn figures(current_usage: f64, usage_limit: f64, current_overages: f64) -> AccountQuotaFigures {
        AccountQuotaFigures {
            current_usage,
            usage_limit,
            current_overages,
        }
    }

    #[test]
    fn quota_transitions_follow_the_snapshots() {
        use AccountEventKind::*;
        // The first snapshot is not a change.
        assert_eq!(quota_transition(None, figures(1000.0, 1000.0, 0.0)), None);
        assert_eq!(
            quota_transition(
                Some(figures(900.0, 1000.0, 0.0)),
                figures(1000.0, 1000.0, 0.0)
            ),
            Some(QuotaExhausted)
        );
        assert_eq!(
            quota_transition(
                Some(figures(900.0, 1000.0, 0.0)),
                figures(1200.0, 1000.0, 0.0)
            ),
            Some(OverageStarted)
        );
        assert_eq!(
            quota_transition(
                Some(figures(1000.0, 1000.0, 0.0)),
                figures(1000.0, 1000.0, 5.0)
            ),
            Some(OverageStarted)
        );
        assert_eq!(
            quota_transition(
                Some(figures(1000.0, 1000.0, 0.0)),
                figures(0.0, 1000.0, 0.0)
            ),
            Some(QuotaRecovered)
        );
        // Monthly reset out of overage is a recovery too.
        assert_eq!(
            quota_transition(
                Some(figures(1300.0, 1000.0, 300.0)),
                figures(10.0, 1000.0, 0.0)
            ),
            Some(QuotaRecovered)
        );
        // No change of state, no event.
        assert_eq!(
            quota_transition(
                Some(figures(100.0, 1000.0, 0.0)),
                figures(500.0, 1000.0, 0.0)
            ),
            None
        );
        assert_eq!(
            quota_transition(
                Some(figures(1200.0, 1000.0, 0.0)),
                figures(1500.0, 1000.0, 0.0)
            ),
            None
        );
        // An account without a limit never transitions.
        assert_eq!(
            quota_transition(Some(figures(0.0, 0.0, 0.0)), figures(5.0, 0.0, 0.0)),
            None
        );
    }

    #[test]
    fn external_pool_reasons_map_to_kinds() {
        assert_eq!(
            external_pool_disable_event("quota_exhausted").0,
            AccountEventKind::QuotaExhausted
        );
        assert_eq!(
            external_pool_disable_event("security_lock").0,
            AccountEventKind::RiskControlled
        );
        assert_eq!(
            external_pool_disable_event("auth_error"),
            (AccountEventKind::AutoDisabled, "鉴权失败")
        );
        assert_eq!(
            external_pool_disable_event("unexpected").0,
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
