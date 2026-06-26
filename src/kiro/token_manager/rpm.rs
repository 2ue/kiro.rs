use std::time::{Duration as StdDuration, Instant};

use super::account_state::CredentialEntry;

pub(super) fn entry_rate_limit_remaining(
    entry: &CredentialEntry,
    now: Instant,
) -> Option<StdDuration> {
    entry
        .rate_limit_available_at
        .and_then(|until| until.checked_duration_since(now))
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
