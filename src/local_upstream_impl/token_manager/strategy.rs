use std::time::{Duration as StdDuration, Instant};

use crate::model::config::Config;
use crate::storage::redis_cache::SchedulerHealthState;

use super::account_state::CredentialEntry;
use super::cooldown::model_state_key;

const SELECTION_WINDOW_10S: StdDuration = StdDuration::from_secs(10);
const SELECTION_WINDOW_60S: StdDuration = StdDuration::from_secs(60);
const SELECTION_WINDOW_5M: StdDuration = StdDuration::from_secs(5 * 60);

pub(super) fn entry_effective_health<'a>(
    entry: &'a CredentialEntry,
    model: Option<&str>,
) -> &'a SchedulerHealthState {
    model
        .map(model_state_key)
        .and_then(|key| entry.model_health.get(&key))
        .unwrap_or(&entry.health)
}

pub(super) fn entry_effective_health_mut<'a>(
    entry: &'a mut CredentialEntry,
    model: Option<&str>,
) -> &'a mut SchedulerHealthState {
    match model.map(model_state_key) {
        Some(key) => entry.model_health.entry(key).or_default(),
        None => &mut entry.health,
    }
}

pub(super) fn scheduler_score_with_config(
    entry: &CredentialEntry,
    model: Option<&str>,
    now_ms: i64,
    selection_pressure: f64,
    config: &Config,
) -> f64 {
    let max_concurrent =
        effective_max_concurrent_requests(entry, config.credential_max_concurrent_requests);
    let load = if max_concurrent > 0 {
        entry.in_flight_requests as f64 / max_concurrent as f64
    } else {
        entry.in_flight_requests as f64
    };
    let health = entry_effective_health(entry, model);
    let probation = health
        .probation_until_ms
        .is_some_and(|until_ms| until_ms > now_ms) as u8 as f64;
    entry.credentials.priority as f64 * config.scheduler_priority_weight.max(0.0)
        + load * config.scheduler_load_weight.max(0.0)
        + health.recent_error_rate.clamp(0.0, 1.0) * config.scheduler_error_weight.max(0.0)
        + health.latency_ewma_ms.unwrap_or(0.0).max(0.0) * config.scheduler_latency_weight.max(0.0)
        + probation * config.scheduler_probation_weight.max(0.0)
        + selection_pressure.max(0.0) * config.scheduler_selection_pressure_weight.max(0.0)
        + (entry.total_selection_count as f64).ln_1p()
            * config.scheduler_total_selection_weight.max(0.0)
}

pub(super) fn prune_local_selection_events(entry: &mut CredentialEntry, now: Instant) {
    while entry.selection_events.front().is_some_and(|selected_at| {
        now.saturating_duration_since(*selected_at) > SELECTION_WINDOW_5M
    }) {
        entry.selection_events.pop_front();
    }
    entry.health.recent_selection_count_10s = entry
        .selection_events
        .iter()
        .filter(|selected_at| now.saturating_duration_since(**selected_at) <= SELECTION_WINDOW_10S)
        .count()
        .min(u32::MAX as usize) as u32;
    entry.health.recent_selection_count_60s = entry
        .selection_events
        .iter()
        .filter(|selected_at| now.saturating_duration_since(**selected_at) <= SELECTION_WINDOW_60S)
        .count()
        .min(u32::MAX as usize) as u32;
    entry.health.recent_selection_count_5m =
        entry.selection_events.len().min(u32::MAX as usize) as u32;
}

pub(super) fn record_local_selection(entry: &mut CredentialEntry, now: Instant, weight_units: u32) {
    entry.total_selection_count = entry.total_selection_count.saturating_add(1);
    entry.health.selection_count = entry.health.selection_count.saturating_add(1);
    for _ in 0..weight_units.clamp(1, 64) {
        entry.selection_events.push_back(now);
    }
    prune_local_selection_events(entry, now);
}

pub(super) fn refresh_local_selection_windows_locked(
    entries: &mut [CredentialEntry],
    now: Instant,
) {
    for entry in entries {
        prune_local_selection_events(entry, now);
    }
}

pub(super) fn selection_pressure_from_totals(
    entry: &CredentialEntry,
    total_recent: u64,
    candidate_count: usize,
) -> f64 {
    if candidate_count <= 1 || total_recent == 0 {
        return 0.0;
    }
    let share = entry.health.recent_selection_count_60s as f64 / total_recent as f64;
    let expected_share = 1.0 / candidate_count as f64;
    (share / expected_share - 1.0).max(0.0)
}

pub(super) fn warmup_target_share_with_config(config: &Config, warming_count: usize) -> f64 {
    if warming_count == 0 {
        return 0.0;
    }
    let per_warming = config.credential_warmup_selection_percent.min(100) as f64 / 100.0;
    let max_share = config.credential_warmup_max_selection_percent.min(100) as f64 / 100.0;
    (per_warming * warming_count as f64).min(max_share).min(1.0)
}

pub(super) fn should_select_warming_from_totals(
    config: &Config,
    ready_count: usize,
    warming_count: usize,
    total_recent: u64,
    warming_recent: u64,
) -> bool {
    if warming_count == 0 {
        return false;
    }
    if ready_count == 0 {
        return true;
    }
    let target_share = warmup_target_share_with_config(config, warming_count);
    if target_share <= 0.0 {
        return false;
    }
    if total_recent == 0 {
        return true;
    }
    let current_share = warming_recent as f64 / total_recent as f64;
    current_share < target_share
}

pub(super) fn select_health_weighted<'a>(
    candidates: &[&'a CredentialEntry],
    model: Option<&str>,
    now_ms: i64,
    config: &Config,
) -> Option<&'a CredentialEntry> {
    let candidate_count = candidates.len();
    let total_recent: u64 = candidates
        .iter()
        .map(|candidate| candidate.health.recent_selection_count_60s as u64)
        .sum();
    let top_k = (config.scheduler_top_k.max(1) as usize).min(candidate_count);
    let mut top: Vec<(&CredentialEntry, f64)> = Vec::with_capacity(top_k);
    for entry in candidates.iter().copied() {
        let pressure = selection_pressure_from_totals(entry, total_recent, candidate_count);
        let score = scheduler_score_with_config(entry, model, now_ms, pressure, config);
        let insert_at = top
            .iter()
            .position(|(existing_entry, existing_score)| {
                score < *existing_score
                    || (score == *existing_score && entry.id < existing_entry.id)
            })
            .unwrap_or(top.len());
        if insert_at < top_k {
            top.insert(insert_at, (entry, score));
            if top.len() > top_k {
                top.pop();
            }
        }
    }
    let worst_score = top.last()?.1;
    let total_weight: f64 = top
        .iter()
        .map(|(_, score)| (worst_score - score + 1.0).max(0.01))
        .sum();
    let mut roll = fastrand::f64() * total_weight;
    for (entry, score) in &top {
        roll -= (worst_score - score + 1.0).max(0.01);
        if roll <= 0.0 {
            return Some(*entry);
        }
    }
    top.last().map(|(entry, _)| *entry)
}

pub(super) fn select_weighted_least_inflight<'a>(
    candidates: &[&'a CredentialEntry],
    model: Option<&str>,
    now_ms: i64,
    config: &Config,
) -> Option<&'a CredentialEntry> {
    let candidate_count = candidates.len();
    if candidate_count == 0 {
        return None;
    }
    let total_recent: u64 = candidates
        .iter()
        .map(|candidate| candidate.health.recent_selection_count_60s as u64)
        .sum();
    let top_k = (config.scheduler_top_k.max(1) as usize).min(candidate_count);
    let mut top: Vec<(&CredentialEntry, f64)> = Vec::with_capacity(top_k);
    for entry in candidates.iter().copied() {
        let score = weighted_least_inflight_score(
            entry,
            model,
            now_ms,
            selection_pressure_from_totals(entry, total_recent, candidate_count),
            config,
        );
        let insert_at = top
            .iter()
            .position(|(existing_entry, existing_score)| {
                score < *existing_score
                    || (score == *existing_score && entry.id < existing_entry.id)
            })
            .unwrap_or(top.len());
        if insert_at < top_k {
            top.insert(insert_at, (entry, score));
            if top.len() > top_k {
                top.pop();
            }
        }
    }

    let worst_score = top.last()?.1;
    let total_weight: f64 = top
        .iter()
        .map(|(_, score)| (worst_score - score + 1.0).max(0.01))
        .sum();
    let mut roll = fastrand::f64() * total_weight;
    for (entry, score) in &top {
        roll -= (worst_score - score + 1.0).max(0.01);
        if roll <= 0.0 {
            return Some(*entry);
        }
    }
    top.last().map(|(entry, _)| *entry)
}

fn weighted_least_inflight_score(
    entry: &CredentialEntry,
    model: Option<&str>,
    now_ms: i64,
    selection_pressure: f64,
    config: &Config,
) -> f64 {
    let max_concurrent =
        effective_max_concurrent_requests(entry, config.credential_max_concurrent_requests);
    let inflight_component = if max_concurrent > 0 {
        entry.in_flight_requests as f64 / max_concurrent as f64
    } else {
        entry.in_flight_requests as f64
    };
    let health = entry_effective_health(entry, model);
    let probation_component = health
        .probation_until_ms
        .is_some_and(|until_ms| until_ms > now_ms) as u8 as f64;

    entry.credentials.priority as f64 * config.scheduler_priority_weight.max(0.0)
        + inflight_component * config.scheduler_load_weight.max(0.0)
        + health.recent_error_rate.clamp(0.0, 1.0) * config.scheduler_error_weight.max(0.0)
        + health.latency_ewma_ms.unwrap_or(0.0).max(0.0) * config.scheduler_latency_weight.max(0.0)
        + probation_component * config.scheduler_probation_weight.max(0.0)
        + selection_pressure.max(0.0) * config.scheduler_selection_pressure_weight.max(0.0)
        + (entry.total_selection_count as f64).ln_1p()
            * config.scheduler_total_selection_weight.max(0.0)
}

pub(super) fn balanced_selection_key(
    entry: &CredentialEntry,
) -> (u32, u32, u32, u64, u32, u64, u64) {
    (
        entry.in_flight_requests,
        entry.health.recent_selection_count_10s,
        entry.health.recent_selection_count_60s,
        entry.success_count,
        entry.credentials.priority,
        entry.total_selection_count,
        entry.id,
    )
}

pub(super) fn priority_selection_key(
    entry: &CredentialEntry,
) -> (u32, u32, u32, u32, u64, u64, u64) {
    (
        entry.credentials.priority,
        entry.in_flight_requests,
        entry.health.recent_selection_count_10s,
        entry.health.recent_selection_count_60s,
        entry.success_count,
        entry.total_selection_count,
        entry.id,
    )
}

fn effective_max_concurrent_requests(
    entry: &CredentialEntry,
    global_max_concurrent_requests: u32,
) -> u32 {
    entry
        .credentials
        .max_concurrent_requests
        .unwrap_or(global_max_concurrent_requests)
}
