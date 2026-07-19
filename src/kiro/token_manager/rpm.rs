use std::time::{Duration as StdDuration, Instant};

use super::account_state::CredentialEntry;

const RATE_LIMIT_PHASE_CREDIT_DIVISOR: u128 = 8;
const RATE_LIMIT_PHASE_CREDIT_MAX_MS: u128 = 125;
const RATE_LIMIT_PHASE_HISTORY_DIVISOR: u128 = 2;
const RATE_LIMIT_PHASE_HISTORY_MAX_MS: u128 = 500;

pub(super) fn entry_rate_limit_remaining(
    entry: &CredentialEntry,
    global_rpm: u32,
    now: Instant,
) -> Option<StdDuration> {
    let pending = entry
        .pending_redis_admission
        .and_then(|pending| pending.rate_limit_available_at)
        .and_then(|until| until.checked_duration_since(now));
    let rpm = effective_rpm(entry, global_rpm);
    if rpm == 0 || entry.rate_limit_rpm != Some(rpm) {
        return pending;
    }
    let confirmed = entry
        .rate_limit_available_at
        .and_then(|until| until.checked_duration_since(now));
    pending.into_iter().chain(confirmed).max()
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

fn rate_limit_phase_credit(interval: StdDuration) -> StdDuration {
    let credit_ms = (interval.as_millis() / RATE_LIMIT_PHASE_CREDIT_DIVISOR)
        .max(1)
        .min(RATE_LIMIT_PHASE_CREDIT_MAX_MS) as u64;
    StdDuration::from_millis(credit_ms)
}

fn rate_limit_phase_history(interval: StdDuration) -> StdDuration {
    let history_ms = (interval.as_millis() / RATE_LIMIT_PHASE_HISTORY_DIVISOR)
        .max(1)
        .min(RATE_LIMIT_PHASE_HISTORY_MAX_MS) as u64;
    StdDuration::from_millis(history_ms)
}

pub(super) fn next_rate_limit_available_at(
    previous_available_at: Option<Instant>,
    now: Instant,
    interval: StdDuration,
    weight_units: u32,
) -> Option<Instant> {
    let pacing_span = interval.checked_mul(weight_units.clamp(1, 64))?;
    let reset_deadline = now.checked_add(pacing_span)?;
    let Some(previous_deadline) = previous_available_at else {
        return Some(reset_deadline);
    };
    let Some(lateness) = now.checked_duration_since(previous_deadline) else {
        return Some(reset_deadline);
    };
    if lateness > rate_limit_phase_history(interval) {
        return Some(reset_deadline);
    }
    let phase_deadline = previous_deadline.checked_add(pacing_span)?;
    let minimum_span = pacing_span
        .saturating_sub(rate_limit_phase_credit(interval))
        .max(StdDuration::from_millis(1));
    let minimum_deadline = now.checked_add(minimum_span)?;
    Some(phase_deadline.max(minimum_deadline))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_deadline_preserves_small_arrival_phase_error() {
        let start = Instant::now();
        let interval = StdDuration::from_secs(1);
        let previous_deadline = start + interval;
        let next = next_rate_limit_available_at(
            Some(previous_deadline),
            previous_deadline + StdDuration::from_millis(80),
            interval,
            1,
        )
        .unwrap();

        assert_eq!(next, start + StdDuration::from_secs(2));
    }

    #[test]
    fn rate_limit_deadline_resets_after_large_delay() {
        let start = Instant::now();
        let interval = StdDuration::from_secs(1);
        let previous_deadline = start + interval;
        let now = previous_deadline + StdDuration::from_millis(501);
        let next = next_rate_limit_available_at(Some(previous_deadline), now, interval, 1).unwrap();

        assert_eq!(next, now + interval);
    }

    #[test]
    fn weighted_rate_limit_credit_is_not_multiplied_by_weight() {
        let start = Instant::now();
        let interval = StdDuration::from_secs(1);
        let previous_deadline = start + interval;
        let now = previous_deadline + StdDuration::from_millis(400);
        let next =
            next_rate_limit_available_at(Some(previous_deadline), now, interval, 64).unwrap();

        assert_eq!(next, now + StdDuration::from_millis(63_875));
    }
}
