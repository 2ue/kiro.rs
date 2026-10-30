//! Dispatch progress: how fast queued requests are actually turning into upstream calls.
//!
//! Two consumers rely on it. A full queue answers with a `Retry-After` derived from the queue
//! depth and the recent dispatch rate, so clients back off for about as long as the queue needs
//! to drain instead of hammering it every second. The health endpoint reports a scheduler that
//! has queued requests but no dispatch for a while as stalled.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

/// Dispatches older than this no longer count towards the recent rate.
const RATE_WINDOW: Duration = Duration::from_secs(30);
/// Bounds the memory used by the window under very high request rates.
const MAX_SAMPLES: usize = 4096;
/// Never ask clients to wait less than this or more than this.
pub(crate) const QUEUE_FULL_RETRY_AFTER_MIN_SECS: u64 = 1;
pub(crate) const QUEUE_FULL_RETRY_AFTER_MAX_SECS: u64 = 60;

#[derive(Debug, Default)]
pub(crate) struct DispatchProgress {
    dispatched_at: Mutex<VecDeque<Instant>>,
    /// Kept outside the window so a stall longer than the window is still visible.
    last_dispatch_at: Mutex<Option<Instant>>,
}

impl DispatchProgress {
    pub(crate) fn record_dispatch(&self, now: Instant) {
        let mut samples = self.dispatched_at.lock();
        Self::prune(&mut samples, now);
        if samples.len() >= MAX_SAMPLES {
            samples.pop_front();
        }
        samples.push_back(now);
        drop(samples);
        let mut last = self.last_dispatch_at.lock();
        if last.is_none_or(|previous| previous < now) {
            *last = Some(now);
        }
    }

    pub(crate) fn last_dispatch_at(&self) -> Option<Instant> {
        *self.last_dispatch_at.lock()
    }

    /// Dispatches per second over the recent window.
    pub(crate) fn recent_rate_per_sec(&self, now: Instant) -> f64 {
        let mut samples = self.dispatched_at.lock();
        Self::prune(&mut samples, now);
        samples.len() as f64 / RATE_WINDOW.as_secs_f64()
    }

    /// How long a client turned away by a full queue should wait before retrying: the time the
    /// current queue needs to drain at the recent dispatch rate, clamped to a sane range. With
    /// no recent dispatch at all the queue is not moving, so the maximum is returned.
    pub(crate) fn queue_full_retry_after_secs(&self, queued: u32, now: Instant) -> u64 {
        let rate = self.recent_rate_per_sec(now);
        if rate <= 0.0 {
            return QUEUE_FULL_RETRY_AFTER_MAX_SECS;
        }
        let secs = (queued.max(1) as f64 / rate).ceil() as u64;
        secs.clamp(
            QUEUE_FULL_RETRY_AFTER_MIN_SECS,
            QUEUE_FULL_RETRY_AFTER_MAX_SECS,
        )
    }

    fn prune(samples: &mut VecDeque<Instant>, now: Instant) {
        while samples
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) > RATE_WINDOW)
        {
            samples.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_follows_queue_depth_over_recent_rate() {
        let progress = DispatchProgress::default();
        let now = Instant::now();
        // 300 dispatches in the last 30 seconds: 10 per second.
        for index in 0..300u64 {
            progress.record_dispatch(now - Duration::from_millis(index * 100));
        }
        assert_eq!(progress.queue_full_retry_after_secs(200, now), 20);
        assert_eq!(progress.queue_full_retry_after_secs(5, now), 1);
        assert_eq!(progress.queue_full_retry_after_secs(5_000, now), 60);
    }

    #[test]
    fn a_queue_that_is_not_moving_asks_for_the_maximum_wait() {
        let progress = DispatchProgress::default();
        let now = Instant::now();
        assert_eq!(progress.queue_full_retry_after_secs(10, now), 60);
        progress.record_dispatch(now - Duration::from_secs(45));
        assert_eq!(progress.queue_full_retry_after_secs(10, now), 60);
        assert!(progress.last_dispatch_at().is_some());
    }
}
