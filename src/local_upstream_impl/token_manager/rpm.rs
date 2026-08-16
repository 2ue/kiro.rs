use std::time::{Duration as StdDuration, Instant};

use super::account_state::CredentialEntry;

const RPM_WINDOW: StdDuration = StdDuration::from_secs(60);

pub(super) fn entry_rate_limit_remaining(
    entry: &CredentialEntry,
    global_rpm: u32,
    now: Instant,
) -> Option<StdDuration> {
    let explicit_remaining = entry
        .rate_limit_available_at
        .and_then(|until| until.checked_duration_since(now));
    let window_remaining = entry_rate_limit_window_remaining(entry, global_rpm, now);

    match (explicit_remaining, window_remaining) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

pub(super) fn entry_rate_limit_window_remaining(
    entry: &CredentialEntry,
    global_rpm: u32,
    now: Instant,
) -> Option<StdDuration> {
    let rpm = effective_rpm(entry, global_rpm);
    if rpm == 0 {
        return None;
    }

    let mut count = 0_u32;
    let mut oldest: Option<Instant> = None;
    for selected_at in &entry.selection_events {
        if now.saturating_duration_since(*selected_at) > RPM_WINDOW {
            continue;
        }
        count = count.saturating_add(1);
        oldest = Some(oldest.map_or(*selected_at, |current| current.min(*selected_at)));
    }

    if count < rpm {
        return None;
    }

    oldest
        .and_then(|selected_at| selected_at.checked_add(RPM_WINDOW))
        .and_then(|available_at| available_at.checked_duration_since(now))
}

pub(super) fn effective_rpm(entry: &CredentialEntry, global_rpm: u32) -> u32 {
    entry.credentials.rpm.unwrap_or(global_rpm)
}

pub(super) fn rate_limit_interval_for_rpm(rpm: u32) -> Option<StdDuration> {
    if rpm == 0 {
        return None;
    }

    let millis = (60_000u64 / rpm as u64).max(1);
    Some(StdDuration::from_millis(millis))
}
