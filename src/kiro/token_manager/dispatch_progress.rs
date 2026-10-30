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
/// The latest dispatches are also measured on their own over this horizon, so a burst after a
/// quiet spell is not averaged down by the few dispatches that came before it.
const SHORT_RATE_HORIZON: Duration = Duration::from_secs(5);
/// Samples closer together than this are measured over this span, so a handful of dispatches
/// in the same instant does not read as an unbounded rate.
const MIN_RATE_SPAN: Duration = Duration::from_secs(1);
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

    /// Dispatches per second recently: the higher of the rate over the whole window and over the
    /// last few seconds, each measured over the time its samples actually span. Dividing by the
    /// full window would make a busy queue look many times slower than it is right after
    /// start-up, and a burst after a quiet spell would be averaged down by the quiet part. With
    /// no new dispatch both rates decay as `now` moves away from the samples.
    pub(crate) fn recent_rate_per_sec(&self, now: Instant) -> f64 {
        let mut samples = self.dispatched_at.lock();
        Self::prune(&mut samples, now);
        let rate_since = |count: usize, oldest: Option<Instant>| {
            oldest.map_or(0.0, |oldest| {
                let span = now.saturating_duration_since(oldest).max(MIN_RATE_SPAN);
                count as f64 / span.as_secs_f64()
            })
        };
        // Concurrent dispatches may record slightly out of order, so no sample order is assumed.
        let (mut oldest, mut short_count, mut short_oldest) = (None, 0, None);
        let earlier = |current: Option<Instant>, at: Instant| {
            Some(current.map_or(at, |current: Instant| current.min(at)))
        };
        for &at in samples.iter() {
            oldest = earlier(oldest, at);
            if now.saturating_duration_since(at) <= SHORT_RATE_HORIZON {
                short_count += 1;
                short_oldest = earlier(short_oldest, at);
            }
        }
        rate_since(samples.len(), oldest).max(rate_since(short_count, short_oldest))
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

    /// A burst on a freshly started (or long idle) instance: 15 slots filled within the first
    /// second and 20 more requests queued. The queue drains in about a second, not the 40 s a
    /// rate averaged over the whole 30 s window would suggest.
    #[test]
    fn a_burst_right_after_start_is_measured_over_the_history_it_has() {
        let progress = DispatchProgress::default();
        let now = Instant::now();
        for index in 0..15u64 {
            progress.record_dispatch(now - Duration::from_millis(index * 50));
        }
        assert_eq!(progress.queue_full_retry_after_secs(20, now), 2);

        // Three seconds of steady dispatching at 10 per second: 20 queued drain in 2 s.
        let progress = DispatchProgress::default();
        for index in 0..30u64 {
            progress.record_dispatch(now - Duration::from_millis(index * 100));
        }
        assert_eq!(progress.queue_full_retry_after_secs(20, now), 2);
        // The same 30 dispatches, but the last one was 10 s ago: the queue is slowing down.
        assert_eq!(
            progress.queue_full_retry_after_secs(20, now + Duration::from_secs(10)),
            9
        );

        // Two dispatches during a quiet spell, then the burst: the quiet part does not drag
        // the estimate to half a minute.
        let progress = DispatchProgress::default();
        progress.record_dispatch(now - Duration::from_secs(25));
        progress.record_dispatch(now - Duration::from_secs(20));
        for index in 0..15u64 {
            progress.record_dispatch(now - Duration::from_millis(index * 50));
        }
        assert_eq!(progress.queue_full_retry_after_secs(20, now), 2);
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
