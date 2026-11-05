//! Credential selection policy.
//!
//! The outer acquire loop in `manager.rs` owns locking, slot reservation, Redis leases, RPM
//! reservations and token refresh. This module only decides *which* dispatchable credential a
//! request should take, so every load-balancing mode is expressed as a pure function over the
//! candidate list and can be tested without the acquire machinery.

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use crate::kiro::model::credentials::KiroCredentials;
use crate::model::config::Config;

use super::account_state::CredentialEntry;
use super::capacity::{credential_catalog_lists_model, effective_max_concurrent_requests};
use super::degrade::is_suspended;
use super::strategy::{select_health_weighted, should_select_warming_from_totals};

/// Load-balancing mode. Persisted as its snake_case name; unknown names are rejected when the
/// configuration is validated instead of silently falling back to `priority`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum LoadBalancingMode {
    /// Lowest priority value first; inside a tier, least loaded first.
    #[default]
    Priority,
    /// Same ordering as `Priority`; kept as a separate name for existing configurations.
    Balanced,
    /// Score-weighted random choice among the best `scheduler_top_k` candidates.
    HealthBalanced,
    /// Same scoring as `HealthBalanced`; kept as a separate name for existing configurations.
    WeightedLeastInflight,
}

impl LoadBalancingMode {
    pub(crate) const ALL: [Self; 4] = [
        Self::Priority,
        Self::Balanced,
        Self::HealthBalanced,
        Self::WeightedLeastInflight,
    ];

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Priority => "priority",
            Self::Balanced => "balanced",
            Self::HealthBalanced => "health_balanced",
            Self::WeightedLeastInflight => "weighted_least_inflight",
        }
    }

    /// Priority mode fills the most preferred priority tier before using the next one, both on
    /// the first pick and after losing a slot race.
    pub(crate) fn fills_priority_tiers_in_order(self) -> bool {
        matches!(self, Self::Priority)
    }

    fn uses_scores(self) -> bool {
        matches!(self, Self::HealthBalanced | Self::WeightedLeastInflight)
    }
}

impl fmt::Display for LoadBalancingMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for LoadBalancingMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|mode| mode.as_str() == value)
            .ok_or_else(|| {
                format!(
                    "无效的负载均衡模式: {value}（可选: priority、balanced、health_balanced、weighted_least_inflight）"
                )
            })
    }
}

/// Picks one credential from `dispatchable`, which must already satisfy every dispatch safety
/// rule (enabled, quota, model, proxy, cooldown, RPM, concurrency, request exclusions).
///
/// Order of decisions:
/// 1. Catalog preference: when any candidate's catalog lists the model, unknown-catalog
///    candidates are dropped.
/// 2. Fast: only the highest Fast tier competes.
/// 3. Priority mode: only the preferred priority tier (the lowest priority value that still
///    has a dispatchable credential) competes. A full tier is simply absent from
///    `dispatchable`, so traffic overflows to the next tier in the same pick.
/// 4. Warmup share: the warming or the ready group is chosen by target share, inside the
///    tier chosen above, so warming credentials never pull traffic across tiers.
/// 5. The mode orders the chosen group.
///
/// `bound_sessions` maps a credential id to the number of sessions bound to it that were
/// active in the last [`ACTIVE_SESSION_WINDOW_SECS`].
pub(crate) fn select_candidate<'a>(
    mode: LoadBalancingMode,
    dispatchable: Vec<&'a CredentialEntry>,
    model: Option<&str>,
    now_ms: i64,
    config: &Config,
    bound_sessions: &HashMap<u64, u32>,
) -> Option<&'a CredentialEntry> {
    let mut available = prefer_catalog_listed(dispatchable, model);
    if available.is_empty() {
        return None;
    }
    // 劣化摘除发生在档位筛选之前：被摘除的账号不再参与后面的 max(fast) 计算，
    // 于是流量自然溢出到同档位或其他档位的健康账号，而不是继续被它霸占。
    retain_non_degraded(&mut available, model, now_ms, config);
    retain_top_fast_tier(&mut available);
    if mode.fills_priority_tiers_in_order() {
        retain_preferred_priority_tier(&mut available);
    }
    let candidates = warmup_group(available, config);
    order_by_mode(mode, &candidates, model, now_ms, config, bound_sessions)
}

/// Sessions count as active on a credential for this long after their last request.
pub(crate) const ACTIVE_SESSION_WINDOW_SECS: i64 = 10 * 60;

/// The preferred priority tier: the lowest priority value among the candidates.
pub(crate) fn preferred_priority<'a>(
    candidates: impl IntoIterator<Item = &'a CredentialEntry>,
) -> Option<u32> {
    candidates
        .into_iter()
        .map(|entry| entry.credentials.priority)
        .min()
}

/// The tier a sticky binding lives in, used to decide whether a session should move up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StickyTier {
    pub(crate) fast: u32,
    pub(crate) priority: u32,
}

impl StickyTier {
    pub(crate) fn of(credentials: &KiroCredentials) -> Self {
        Self {
            fast: credentials.fast,
            priority: credentials.priority,
        }
    }

    /// Fast always ranks above priority; priority only counts when the mode fills priority
    /// tiers in order.
    pub(crate) fn is_better_than(self, other: Self, compare_priority: bool) -> bool {
        self.fast > other.fast
            || (compare_priority && self.fast == other.fast && self.priority < other.priority)
    }
}

fn retain_preferred_priority_tier(available: &mut Vec<&CredentialEntry>) {
    if let Some(preferred) = preferred_priority(available.iter().copied()) {
        available.retain(|entry| entry.credentials.priority == preferred);
    }
}

fn prefer_catalog_listed<'a>(
    dispatchable: Vec<&'a CredentialEntry>,
    model: Option<&str>,
) -> Vec<&'a CredentialEntry> {
    // When a dispatchable credential's catalog lists the model, credentials with an empty
    // catalog (discovery failed or never ran) are not picked: an invalid-model answer from them
    // would burn an attempt. This only reorders: the filter applies to *dispatchable*
    // credentials, so once every listed credential is full the empty-catalog ones take the
    // traffic and no capacity is lost.
    let prefer_listed = dispatchable
        .iter()
        .any(|entry| credential_catalog_lists_model(entry, model));
    if !prefer_listed {
        return dispatchable;
    }
    dispatchable
        .into_iter()
        .filter(|entry| credential_catalog_lists_model(entry, model))
        .collect()
}

/// 暂时摘除劣化分已达阈值的账号，让流量溢出到其他账号。
///
/// 三条安全规则，缺一不可：
///
/// 1. **没有健康候选时全部放行**。摘除绝不能把候选集清空——否则请求会去排队，
///    把「质量劣化」变成「容量不足」，这正是 59137 上出现过的那类问题。
///    可用性优先于质量。
/// 2. **保留探测流量**。按 `credential_degrade_probe_percent` 抽样时不摘除任何账号，
///    让被摘除的账号还能拿到少量真实请求、产生成功样本，否则它永远无法恢复。
/// 3. **只摘除、不改变档位值**。摘除发生在 `retain_top_fast_tier` 之前，被摘除的账号
///    不参与 `max(fast)`；恢复后自动回到原档位。账号自身的 `fast` 与 `priority`
///    配置永远不被改写。
fn retain_non_degraded(
    available: &mut Vec<&CredentialEntry>,
    model: Option<&str>,
    now_ms: i64,
    config: &Config,
) {
    let threshold = config.credential_degrade_suspend_score;
    // 恢复线低于摘除线，两者之间的滞回带防止账号在「恢复—吃满流量—再失败—摘除」之间横跳。
    let resume_score =
        (threshold * config.credential_degrade_resume_ratio.clamp(0.0, 1.0)).max(0.0);
    if !config.credential_degrade_enabled || resume_score <= 0.0 || available.len() <= 1 {
        return;
    }
    let half_life_ms = (config.credential_degrade_half_life_secs.max(1) as f64) * 1000.0;
    let is_degraded = |entry: &CredentialEntry| {
        is_suspended(
            &entry.degrade_scores,
            model,
            now_ms,
            half_life_ms,
            resume_score,
        )
    };
    let healthy_count = available.iter().filter(|entry| !is_degraded(entry)).count();
    if healthy_count == 0 || healthy_count == available.len() {
        return;
    }
    let probe_percent = config.credential_degrade_probe_percent.min(100);
    if probe_percent > 0 && fastrand::u32(0..100) < probe_percent {
        return;
    }
    available.retain(|entry| !is_degraded(entry));
}

/// Fast tiers sit above every load-balancing mode: only the highest Fast tier among the
/// dispatchable candidates competes, so a tier is filled up to its safety limits before
/// traffic overflows to the next one.
fn retain_top_fast_tier(available: &mut Vec<&CredentialEntry>) {
    let top_fast = available
        .iter()
        .map(|entry| entry.credentials.fast)
        .max()
        .unwrap_or(0);
    if top_fast > 0 {
        available.retain(|entry| entry.credentials.fast == top_fast);
    }
}

/// Real requests sample warming credentials at the configured target share, scaled by the
/// number of warming credentials and capped by the maximum share.
fn warmup_group<'a>(
    available: Vec<&'a CredentialEntry>,
    config: &Config,
) -> Vec<&'a CredentialEntry> {
    let recent = |entry: &CredentialEntry| entry.health.recent_selection_count_60s as u64;
    let (warming, ready): (Vec<_>, Vec<_>) = available
        .into_iter()
        .partition(|entry| entry.warmup_remaining > 0);
    let warming_recent = warming.iter().map(|entry| recent(entry)).sum::<u64>();
    let total_recent = warming_recent + ready.iter().map(|entry| recent(entry)).sum::<u64>();
    if should_select_warming_from_totals(
        config,
        ready.len(),
        warming.len(),
        total_recent,
        warming_recent,
    ) {
        warming
    } else {
        ready
    }
}

fn order_by_mode<'a>(
    mode: LoadBalancingMode,
    candidates: &[&'a CredentialEntry],
    model: Option<&str>,
    now_ms: i64,
    config: &Config,
    bound_sessions: &HashMap<u64, u32>,
) -> Option<&'a CredentialEntry> {
    if mode.uses_scores() {
        return select_health_weighted(candidates, model, now_ms, config);
    }
    candidates.iter().copied().min_by_key(|entry| {
        spread_key(
            entry,
            config.credential_max_concurrent_requests,
            bound_sessions.get(&entry.id).copied().unwrap_or(0),
        )
    })
}

/// Ordering inside a tier, lowest first. New sessions go to the credential that is least
/// loaded right now, then to the one carrying the fewest active sessions, so bound sessions
/// even out across the tier over time instead of piling onto a few credentials:
/// 1. priority (only differs in `balanced` mode, where tiers are not pre-filtered);
/// 2. in-flight load as a share of the credential's own concurrency limit;
/// 3. active sessions bound to the credential;
/// 4. selections in the last 60 seconds;
/// 5. least recently selected first (never selected sorts first);
/// 6. id, for a stable order.
///
/// Lifetime success counts are deliberately not used: they made idle credentials with a long
/// history lose every tie, so old credentials stayed idle while new ones took the traffic.
fn spread_key(
    entry: &CredentialEntry,
    global_max_concurrent_requests: u32,
    bound_sessions: u32,
) -> (u32, u64, u32, u32, Option<std::time::Instant>, u64) {
    let max_concurrent = effective_max_concurrent_requests(entry, global_max_concurrent_requests);
    let load_per_mille = if max_concurrent > 0 {
        (entry.in_flight_requests as u64 * 1000) / max_concurrent as u64
    } else {
        0
    };
    (
        entry.credentials.priority,
        load_per_mille,
        bound_sessions,
        entry.health.recent_selection_count_60s,
        entry.selection_events.back().copied(),
        entry.id,
    )
}

#[cfg(test)]
mod tests {
    use super::super::degrade;
    use super::*;

    #[test]
    fn mode_names_round_trip_and_unknown_names_are_rejected() {
        for mode in LoadBalancingMode::ALL {
            assert_eq!(mode.as_str().parse::<LoadBalancingMode>(), Ok(mode));
        }
        for invalid in ["", "Priority", "round_robin", "priority "] {
            assert!(
                invalid.parse::<LoadBalancingMode>().is_err(),
                "{invalid:?} must not silently fall back to priority"
            );
        }
    }

    fn test_entry(id: u64, fast: u32, degrade_score: f64) -> CredentialEntry {
        let mut entry = CredentialEntry {
            id,
            credentials: KiroCredentials {
                fast,
                ..Default::default()
            },
            failure_count: 0,
            refresh_failure_count: 0,
            runtime_revision: 0,
            runtime_generation: 0,
            runtime_persistence_degraded: false,
            runtime_persistence_quarantined: false,
            disabled: false,
            disabled_reason: None,
            account_quota_blocked: false,
            account_quota_block_reason: None,
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
            health: Default::default(),
            model_health: HashMap::new(),
            selection_events: Default::default(),
            last_auto_heal_at: None,
            auto_heal_consecutive_rounds: 0,
            upstream_fault_streak: 0,
            upstream_fault_seq: 0,
            redis_full_until: None,
            last_upstream_fault_concentrated: false,
            degrade_scores: HashMap::new(),
        };
        if degrade_score > 0.0 {
            // 用默认阈值 6.0 打标记：分数跨过阈值时该条目标记为「已摘除」
            degrade::add_failure(
                &mut entry.degrade_scores,
                None,
                degrade_score,
                0,
                f64::MAX,
                Config::default().credential_degrade_suspend_score,
            );
        }
        entry
    }

    fn config_without_probe() -> Config {
        let mut config = Config::default();
        config.credential_degrade_probe_percent = 0;
        config
    }

    /// 整档劣化：同档位所有账号都被摘除时必须全部放行，否则请求会去排队，
    /// 把「质量劣化」变成「容量不足」。
    #[test]
    fn degrading_every_candidate_keeps_them_all() {
        let entries = [test_entry(1, 15, 10.0), test_entry(2, 15, 10.0)];
        let mut available: Vec<&CredentialEntry> = entries.iter().collect();
        retain_non_degraded(&mut available, None, 0, &config_without_probe());
        assert_eq!(available.len(), 2, "没有健康候选时必须全部放行");
    }

    /// 单个号劣化：只有它被摘除，同档位健康号不受影响。
    #[test]
    fn degrading_one_candidate_removes_only_it() {
        let entries = [
            test_entry(1, 15, 10.0),
            test_entry(2, 15, 0.0),
            test_entry(3, 10, 0.0),
        ];
        let mut available: Vec<&CredentialEntry> = entries.iter().collect();
        retain_non_degraded(&mut available, None, 0, &config_without_probe());
        assert_eq!(
            available.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            vec![2, 3]
        );
    }

    /// 2026-11-04 59137 的真实场景：fast 15 档整体劣化后，流量必须溢出到 fast 10，
    /// 而不是继续被 fast 15 的坏号独占。
    #[test]
    fn degraded_top_fast_tier_lets_the_next_tier_serve() {
        let entries = [
            test_entry(1, 15, 10.0),
            test_entry(2, 15, 10.0),
            test_entry(3, 10, 0.0),
            test_entry(4, 10, 0.0),
        ];
        let mut available: Vec<&CredentialEntry> = entries.iter().collect();
        retain_non_degraded(&mut available, None, 0, &config_without_probe());
        retain_top_fast_tier(&mut available);
        assert_eq!(
            available.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            vec![3, 4],
            "fast 15 全部劣化时 fast 10 必须接手"
        );
    }

    /// 探测比例 100% 时永不摘除，保证被摘除的账号还能拿到成功样本恢复。
    #[test]
    fn probe_share_never_suspends() {
        let mut config = Config::default();
        config.credential_degrade_probe_percent = 100;
        let entries = [test_entry(1, 15, 10.0), test_entry(2, 10, 0.0)];
        let mut available: Vec<&CredentialEntry> = entries.iter().collect();
        retain_non_degraded(&mut available, None, 0, &config);
        assert_eq!(available.len(), 2);
    }

    /// 关闭开关或阈值 <= 0 时行为与改动前完全一致。
    #[test]
    fn degrade_disabled_changes_nothing() {
        let mut config = Config::default();
        config.credential_degrade_enabled = false;
        let entries = [test_entry(1, 15, 10.0), test_entry(2, 10, 0.0)];
        let mut available: Vec<&CredentialEntry> = entries.iter().collect();
        retain_non_degraded(&mut available, None, 0, &config);
        assert_eq!(available.len(), 2);
    }

    /// 时间衰减足够久后劣化分低于阈值，账号自动回到候选集。
    #[test]
    fn time_decay_restores_the_candidate() {
        let entries = [test_entry(1, 15, 6.0), test_entry(2, 10, 0.0)];
        let mut available: Vec<&CredentialEntry> = entries.iter().collect();
        let config = config_without_probe();
        retain_non_degraded(&mut available, None, 0, &config);
        assert_eq!(available.len(), 1, "刚越过阈值时被摘除");

        // 8 个半衰期后 6.0 → 0.023，远低于阈值 6.0
        let later_ms = 8 * (config.credential_degrade_half_life_secs as i64) * 1000;
        let mut available: Vec<&CredentialEntry> = entries.iter().collect();
        retain_non_degraded(&mut available, None, later_ms, &config);
        assert_eq!(available.len(), 2, "衰减后自动恢复");
    }

    /// 滞回：一次成功把 6.0 降到 3.0，仍高于恢复线 2.4，账号保持摘除；
    /// 第二次成功降到 1.5 才复权。没有滞回时它会立刻回来、吃满流量、再被摘掉，
    /// 形成高频抖动。
    #[test]
    fn one_success_does_not_immediately_restore_the_account() {
        let config = config_without_probe();
        let resume_score =
            config.credential_degrade_suspend_score * config.credential_degrade_resume_ratio;
        let decay = config.credential_degrade_success_decay;

        // 一次成功：6.0 → 3.0，仍高于恢复线 2.4
        let mut entry = test_entry(1, 15, 6.0);
        degrade::apply_success(&mut entry.degrade_scores, None, decay, 0, 0.0, resume_score);
        let other = test_entry(2, 10, 0.0);
        let entries = [entry, other];
        let mut available: Vec<&CredentialEntry> = entries.iter().collect();
        retain_non_degraded(&mut available, None, 0, &config);
        assert_eq!(available.len(), 1, "一次成功不足以复权");

        // 两次成功：6.0 → 3.0 → 1.5，低于恢复线 2.4
        let mut entry = test_entry(1, 15, 6.0);
        degrade::apply_success(&mut entry.degrade_scores, None, decay, 0, 0.0, resume_score);
        degrade::apply_success(&mut entry.degrade_scores, None, decay, 0, 0.0, resume_score);
        let other = test_entry(2, 10, 0.0);
        let entries = [entry, other];
        let mut available: Vec<&CredentialEntry> = entries.iter().collect();
        retain_non_degraded(&mut available, None, 0, &config);
        assert_eq!(available.len(), 2, "降到恢复线以下后复权");
    }

    #[test]
    fn manager_rejects_unknown_mode_at_construction_and_runtime_update() {
        let mut config = Config::default();
        config.load_balancing_mode = "round_robin".to_string();
        assert!(
            super::super::MultiTokenManager::new(config, Vec::new(), None, None, false).is_err()
        );

        let manager =
            super::super::MultiTokenManager::new(Config::default(), Vec::new(), None, None, false)
                .unwrap();
        assert!(
            manager
                .update_runtime_config(|config| config.load_balancing_mode = "bogus".to_string())
                .is_err()
        );
        assert!(
            manager
                .set_load_balancing_mode("bogus".to_string())
                .is_err()
        );
        assert_eq!(manager.get_load_balancing_mode(), "priority");
        manager
            .set_load_balancing_mode("health_balanced".to_string())
            .unwrap();
        assert_eq!(manager.get_load_balancing_mode(), "health_balanced");
    }
}
