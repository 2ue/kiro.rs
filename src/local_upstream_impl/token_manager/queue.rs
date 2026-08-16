use std::collections::{HashMap, HashSet};
use std::time::{Duration as StdDuration, Instant};

use super::account_state::{CredentialEntry, ProxyResourceRuntime};
use super::capacity::{
    credential_is_dispatch_candidate, effective_max_concurrent_requests,
    entry_has_concurrency_capacity,
};
use super::cooldown::entry_cooldown_remaining;
use super::rpm::entry_rate_limit_remaining;

pub(super) fn min_dispatch_wait(
    entries: &[CredentialEntry],
    proxy_resources: &HashMap<u64, ProxyResourceRuntime>,
    model: Option<&str>,
    excluded_ids: &HashSet<u64>,
    now: Instant,
    global_rpm: u32,
) -> Option<StdDuration> {
    entries
        .iter()
        .filter(|entry| {
            credential_is_dispatch_candidate(proxy_resources, entry, model, excluded_ids)
        })
        .filter_map(|entry| {
            match (
                entry_cooldown_remaining(entry, model, now),
                entry_rate_limit_remaining(entry, global_rpm, now),
            ) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            }
        })
        .min()
}

pub(super) fn concurrency_blocked_count(
    entries: &[CredentialEntry],
    proxy_resources: &HashMap<u64, ProxyResourceRuntime>,
    model: Option<&str>,
    excluded_ids: &HashSet<u64>,
    now: Instant,
    max_concurrent_requests: u32,
    global_rpm: u32,
    global_has_capacity: bool,
    request_weight_units: u32,
) -> usize {
    entries
        .iter()
        .filter(|entry| {
            credential_is_dispatch_candidate(proxy_resources, entry, model, excluded_ids)
                && entry_cooldown_remaining(entry, model, now).is_none()
                && entry_rate_limit_remaining(entry, global_rpm, now).is_none()
                && (!global_has_capacity
                    || !entry_has_concurrency_capacity(
                        entry,
                        max_concurrent_requests,
                        request_weight_units,
                    ))
        })
        .count()
}

pub(super) fn rate_limit_blocked_count(
    entries: &[CredentialEntry],
    proxy_resources: &HashMap<u64, ProxyResourceRuntime>,
    model: Option<&str>,
    excluded_ids: &HashSet<u64>,
    now: Instant,
    global_rpm: u32,
) -> usize {
    entries
        .iter()
        .filter(|entry| {
            credential_is_dispatch_candidate(proxy_resources, entry, model, excluded_ids)
                && entry_cooldown_remaining(entry, model, now).is_none()
                && entry_rate_limit_remaining(entry, global_rpm, now).is_some()
        })
        .count()
}

pub(super) fn effective_concurrency_range_for_candidates(
    entries: &[CredentialEntry],
    proxy_resources: &HashMap<u64, ProxyResourceRuntime>,
    model: Option<&str>,
    excluded_ids: &HashSet<u64>,
    global_max_concurrent_requests: u32,
) -> Option<(u32, u32)> {
    entries
        .iter()
        .filter(|entry| {
            credential_is_dispatch_candidate(proxy_resources, entry, model, excluded_ids)
        })
        .map(|entry| effective_max_concurrent_requests(entry, global_max_concurrent_requests))
        .fold(None, |range, value| {
            Some(match range {
                Some((min, max)) => (min.min(value), max.max(value)),
                None => (value, value),
            })
        })
}

pub(super) fn format_effective_concurrency_range(range: Option<(u32, u32)>) -> String {
    match range {
        Some((min, max)) if min == max => min.to_string(),
        Some((min, max)) => format!("{min}..{max}"),
        None => "none".to_string(),
    }
}
