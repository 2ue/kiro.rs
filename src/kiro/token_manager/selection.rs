//! Credential selection policy.
//!
//! The outer acquire loop in `manager.rs` owns locking, slot reservation, Redis leases, RPM
//! reservations and token refresh. This module only decides *which* dispatchable credential a
//! request should take, so every load-balancing mode is expressed as a pure function over the
//! candidate list and can be tested without the acquire machinery.

use std::fmt;
use std::str::FromStr;

use crate::model::config::Config;

use super::account_state::CredentialEntry;
use super::capacity::credential_catalog_lists_model;
use super::strategy::{
    priority_selection_key, select_health_weighted, should_select_warming_from_totals,
};

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
/// 3. Warmup share: the warming or the ready group is chosen by target share.
/// 4. The mode orders the chosen group.
pub(crate) fn select_candidate<'a>(
    mode: LoadBalancingMode,
    dispatchable: Vec<&'a CredentialEntry>,
    model: Option<&str>,
    now_ms: i64,
    config: &Config,
) -> Option<&'a CredentialEntry> {
    let mut available = prefer_catalog_listed(dispatchable, model);
    if available.is_empty() {
        return None;
    }
    retain_top_fast_tier(&mut available);
    let candidates = warmup_group(available, config);
    order_by_mode(mode, &candidates, model, now_ms, config)
}

fn prefer_catalog_listed<'a>(
    dispatchable: Vec<&'a CredentialEntry>,
    model: Option<&str>,
) -> Vec<&'a CredentialEntry> {
    // Credentials with an empty model catalog are only a fallback: when any dispatchable
    // credential's catalog lists the model, unknown-catalog credentials (failed discovery,
    // invalid keys) must not win the selection and burn an attempt.
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
) -> Option<&'a CredentialEntry> {
    if mode.uses_scores() {
        return select_health_weighted(candidates, model, now_ms, config);
    }
    candidates
        .iter()
        .copied()
        .min_by_key(|entry| priority_selection_key(entry))
}

#[cfg(test)]
mod tests {
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
