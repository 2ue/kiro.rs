use std::collections::VecDeque;
use std::time::{Duration as StdDuration, Instant};

use crate::model::config::Config;
use crate::storage::redis_cache::SchedulerHealthState;

use super::account_state::CredentialEntry;
use super::capacity::effective_max_concurrent_requests;
use super::cooldown::model_state_key;

const SELECTION_WINDOW_10S: StdDuration = StdDuration::from_secs(10);
const SELECTION_WINDOW_60S: StdDuration = StdDuration::from_secs(60);
const SELECTION_WINDOW_5M: StdDuration = StdDuration::from_secs(5 * 60);

pub(super) fn entry_effective_health<'a>(
    entry: &'a CredentialEntry,
    model: Option<&str>,
) -> &'a SchedulerHealthState {
    if entry.model_health.is_empty() {
        return &entry.health;
    }
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
    entry.health.recent_selection_count_10s =
        selections_within(&entry.selection_events, now, SELECTION_WINDOW_10S);
    entry.health.recent_selection_count_60s =
        selections_within(&entry.selection_events, now, SELECTION_WINDOW_60S);
    entry.health.recent_selection_count_5m =
        entry.selection_events.len().min(u32::MAX as usize) as u32;
}

/// Selections in the last `window`; `events` is in time order.
fn selections_within(events: &VecDeque<Instant>, now: Instant, window: StdDuration) -> u32 {
    let first =
        events.partition_point(|selected_at| now.saturating_duration_since(*selected_at) > window);
    (events.len() - first).min(u32::MAX as usize) as u32
}

pub(super) fn record_local_selection(entry: &mut CredentialEntry, now: Instant, weight_units: u32) {
    entry.total_selection_count = entry.total_selection_count.saturating_add(1);
    entry.health.selection_count = entry.health.selection_count.saturating_add(1);
    // Callers read `now` before taking the entries lock, so two dispatches can arrive here out
    // of order. Keep the events sorted: the window checks binary-search them.
    let at = entry
        .selection_events
        .back()
        .map_or(now, |last| (*last).max(now));
    for _ in 0..weight_units.clamp(1, 64) {
        entry.selection_events.push_back(at);
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

/// How far warmup may push the ready credentials above their fair share of the traffic (the
/// share they would carry if every candidate were ready). Holding warming credentials back moves
/// their traffic onto the ready ones; when few credentials are ready, e.g. after a global capacity
/// change reset the warmup of the whole pool, that would turn the first credentials to finish
/// warmup into hot spots. Warmup is relaxed instead, as far as needed to stay within this factor.
pub(super) const WARMUP_READY_OVERLOAD_LIMIT: f64 = 2.0;

/// Traffic weight of a warming credential relative to an average ready one, in `[0, 1]`.
///
/// A warming credential starts at `credential_warmup_selection_percent` of an average ready
/// credential's traffic and ramps linearly to the full share as its remaining warmup requests
/// count down. The ramp spans `max(credential_warmup_requests, warmup_remaining)`, so a value set
/// by hand above the configured count starts at the bottom instead of past the end. A ready
/// credential (`warmup_remaining == 0`) has weight 1.
pub(super) fn warmup_weight(config: &Config, warmup_remaining: u32) -> f64 {
    if warmup_remaining == 0 {
        return 1.0;
    }
    let start = config.credential_warmup_selection_percent.min(100) as f64 / 100.0;
    let span = config.credential_warmup_requests.max(warmup_remaining);
    let progress = (span - warmup_remaining) as f64 / span as f64;
    start + (1.0 - start) * progress
}

/// Lowest warmup weight that keeps `ready_count` ready credentials within
/// [`WARMUP_READY_OVERLOAD_LIMIT`] of their fair share while `warming_count` credentials warm
/// up. Zero (no floor) while the ready credentials outnumber the warming ones enough.
pub(super) fn warmup_weight_floor(ready_count: usize, warming_count: usize) -> f64 {
    if ready_count == 0 || warming_count == 0 {
        return 0.0;
    }
    let ready = ready_count as f64;
    let warming = warming_count as f64;
    // Ready share 1 / (ready + warming * w) <= limit / (ready + warming).
    let floor = ((ready + warming) / WARMUP_READY_OVERLOAD_LIMIT - ready) / warming;
    floor.clamp(0.0, 1.0)
}

/// [`warmup_weight`] raised to `floor` (see [`warmup_weight_floor`]).
pub(super) fn effective_warmup_weight(config: &Config, warmup_remaining: u32, floor: f64) -> f64 {
    warmup_weight(config, warmup_remaining).max(floor)
}

/// Traffic a credential has taken recently, as counted by the warmup quota: selections in the
/// last five minutes (long enough to resolve shares well below one request a minute) plus
/// requests in flight. In-flight requests are counted because a selection is only recorded
/// after the token is ready: without them, concurrent requests arriving in that gap would all
/// see an idle warming credential and pile onto it.
pub(super) fn warmup_load(entry: &CredentialEntry) -> f64 {
    entry.health.recent_selection_count_5m as f64 + entry.in_flight_requests as f64
}

/// Whether a warming credential may take another request while ready credentials have room:
/// its recent load must stay below `weight` times the load of an average ready credential it
/// competes with. An idle warming credential (no selection in the window and nothing in
/// flight) is always below a positive quota, so warmup keeps progressing, one request per
/// window at least, when traffic is too sparse for the quota to reach a whole request, and on
/// an idle pool. A zero weight leaves the credential to overflow traffic only.
pub(super) fn warming_below_quota(
    entry: &CredentialEntry,
    weight: f64,
    average_ready_load: f64,
) -> bool {
    let load = warmup_load(entry);
    weight > 0.0 && (load <= 0.0 || load < weight * average_ready_load)
}

/// Whether the warming credentials together already carry their maximum share of the recent
/// load: `credential_warmup_max_selection_percent`, raised as far as
/// [`WARMUP_READY_OVERLOAD_LIMIT`] requires when few credentials are ready. A zero cap leaves
/// warming credentials to overflow traffic only.
pub(super) fn warming_group_at_cap(
    config: &Config,
    ready_count: usize,
    warming_count: usize,
    warming_load: f64,
    total_load: f64,
) -> bool {
    let configured = config.credential_warmup_max_selection_percent.min(100) as f64 / 100.0;
    if configured <= 0.0 {
        return true;
    }
    let candidates = (ready_count + warming_count) as f64;
    let overload_floor = 1.0 - WARMUP_READY_OVERLOAD_LIMIT * ready_count as f64 / candidates;
    let max_share = configured.max(overload_floor);
    total_load > 0.0 && warming_load >= max_share * total_load
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
