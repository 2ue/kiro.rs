//! Rate limit for log lines that repeat once per request during an overload.
//!
//! When the dispatch queue is full every arriving request is turned away, and a gateway in front
//! retries each one several times. Writing a WARN line per rejection then costs more than the
//! rejection itself: formatting plus a write under the global stdout lock, thousands of times a
//! second, on the same workers that have to move the queue forward. A [`LogThrottle`] lets the
//! first occurrence through, then at most one line per interval carrying how many were skipped.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub(crate) struct LogThrottle {
    interval: Duration,
    origin: Instant,
    /// Milliseconds since `origin` at which the next line may be written.
    next_allowed_ms: AtomicU64,
    suppressed: AtomicU64,
}

impl LogThrottle {
    pub(crate) fn new(interval: Duration) -> Self {
        Self {
            interval,
            origin: Instant::now(),
            next_allowed_ms: AtomicU64::new(0),
            suppressed: AtomicU64::new(0),
        }
    }

    /// `Some(skipped)` when this occurrence should be logged, where `skipped` is how many were
    /// dropped since the last logged one; `None` when it should be dropped.
    pub(crate) fn admit(&self) -> Option<u64> {
        self.admit_at(Instant::now())
    }

    fn admit_at(&self, now: Instant) -> Option<u64> {
        let now_ms = now.saturating_duration_since(self.origin).as_millis() as u64;
        let next = self.next_allowed_ms.load(Ordering::Acquire);
        if now_ms < next {
            self.suppressed.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        let interval_ms = self.interval.as_millis().max(1) as u64;
        if self
            .next_allowed_ms
            .compare_exchange(
                next,
                now_ms + interval_ms,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            // Another thread logged this window.
            self.suppressed.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        Some(self.suppressed.swap(0, Ordering::AcqRel))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_occurrence_is_logged_then_one_per_interval_with_the_skipped_count() {
        let throttle = LogThrottle::new(Duration::from_secs(1));
        let start = throttle.origin;
        assert_eq!(throttle.admit_at(start), Some(0));
        for offset in [1, 200, 999] {
            assert_eq!(
                throttle.admit_at(start + Duration::from_millis(offset)),
                None
            );
        }
        assert_eq!(
            throttle.admit_at(start + Duration::from_millis(1000)),
            Some(3)
        );
        assert_eq!(throttle.admit_at(start + Duration::from_millis(1500)), None);
        assert_eq!(
            throttle.admit_at(start + Duration::from_millis(2600)),
            Some(1)
        );
    }

    #[test]
    fn concurrent_callers_log_once_per_window() {
        let throttle = std::sync::Arc::new(LogThrottle::new(Duration::from_secs(60)));
        let admitted = std::sync::Arc::new(AtomicU64::new(0));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let throttle = throttle.clone();
                let admitted = admitted.clone();
                std::thread::spawn(move || {
                    for _ in 0..1000 {
                        if throttle.admit().is_some() {
                            admitted.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(admitted.load(Ordering::Relaxed), 1);
        assert_eq!(throttle.suppressed.load(Ordering::Relaxed), 7999);
    }
}
