//! How late the tokio runtime is running tasks.
//!
//! A background task asks to wake up every [`PROBE_INTERVAL`] and records how much later than
//! asked it actually ran. When workers are saturated (lock contention, a flood of cheap
//! rejections, blocking calls) that delay grows for every task in the process, and every
//! operation with a short deadline (Redis scheduler calls, lease renewals) starts timing out even
//! though the remote side is fine. Callers use [`RuntimeLag::recent_max`] to tell "the
//! dependency is slow" from "we are slow".

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

const PROBE_INTERVAL: Duration = Duration::from_millis(20);
/// Number of buckets in the rolling window; each covers `BUCKET_SPAN`.
const BUCKETS: usize = 4;
const BUCKET_SPAN: Duration = Duration::from_millis(500);

#[derive(Debug)]
pub(crate) struct RuntimeLag {
    origin: Instant,
    /// Per bucket: the bucket index it was last written for, and the max lag seen in it (µs).
    bucket_index: [AtomicU64; BUCKETS],
    bucket_max_micros: [AtomicU64; BUCKETS],
    /// When the probe's current sleep began (µs since `origin`, plus one so zero means "not
    /// sleeping"). A stalled runtime has not woken the probe yet, so the buckets alone would
    /// report the stall only after it ended; the overdue part of the current sleep covers that.
    sleeping_since_micros: AtomicU64,
    started: AtomicU64,
}

impl Default for RuntimeLag {
    fn default() -> Self {
        Self {
            origin: Instant::now(),
            bucket_index: Default::default(),
            bucket_max_micros: Default::default(),
            sleeping_since_micros: AtomicU64::new(0),
            started: AtomicU64::new(0),
        }
    }
}

static GLOBAL: LazyLock<Arc<RuntimeLag>> = LazyLock::new(|| Arc::new(RuntimeLag::default()));

/// The process-wide probe. Its numbers are only meaningful once [`spawn_probe`] runs.
pub(crate) fn global() -> &'static Arc<RuntimeLag> {
    &GLOBAL
}

/// Starts the probe task on the current runtime. Idempotent.
pub(crate) fn spawn_probe() {
    let lag = global().clone();
    if lag.started.swap(1, Ordering::AcqRel) == 1 {
        return;
    }
    tokio::spawn(async move {
        loop {
            let asked_at = Instant::now();
            lag.sleeping_since_micros
                .store(lag.micros_since_origin(asked_at) + 1, Ordering::Release);
            tokio::time::sleep(PROBE_INTERVAL).await;
            lag.sleeping_since_micros.store(0, Ordering::Release);
            let late = asked_at.elapsed().saturating_sub(PROBE_INTERVAL);
            lag.record(Instant::now(), late);
        }
    });
}

impl RuntimeLag {
    fn micros_since_origin(&self, at: Instant) -> u64 {
        at.saturating_duration_since(self.origin)
            .as_micros()
            .min(u64::MAX as u128 - 1) as u64
    }

    fn bucket_of(&self, at: Instant) -> u64 {
        (at.saturating_duration_since(self.origin).as_millis() / BUCKET_SPAN.as_millis()) as u64
    }

    pub(crate) fn record(&self, at: Instant, late: Duration) {
        let index = self.bucket_of(at);
        let slot = (index as usize) % BUCKETS;
        let micros = late.as_micros().min(u64::MAX as u128) as u64;
        if self.bucket_index[slot].swap(index, Ordering::AcqRel) != index {
            self.bucket_max_micros[slot].store(micros, Ordering::Release);
        } else {
            self.bucket_max_micros[slot].fetch_max(micros, Ordering::AcqRel);
        }
    }

    /// Largest scheduling delay seen over the last `BUCKETS * BUCKET_SPAN` (2 s), including how
    /// overdue the probe's current sleep already is, or zero when the probe is not running.
    pub(crate) fn recent_max(&self) -> Duration {
        self.recent_max_at(Instant::now())
    }

    fn recent_max_at(&self, now: Instant) -> Duration {
        let current = self.bucket_of(now);
        let oldest = current.saturating_sub(BUCKETS as u64 - 1);
        let mut max = 0u64;
        let sleeping_since = self.sleeping_since_micros.load(Ordering::Acquire);
        if sleeping_since > 0 {
            let overdue = self
                .micros_since_origin(now)
                .saturating_sub(sleeping_since - 1)
                .saturating_sub(PROBE_INTERVAL.as_micros() as u64);
            max = overdue;
        }
        for slot in 0..BUCKETS {
            let index = self.bucket_index[slot].load(Ordering::Acquire);
            if index >= oldest && index <= current {
                max = max.max(self.bucket_max_micros[slot].load(Ordering::Acquire));
            }
        }
        Duration::from_micros(max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_the_largest_recent_delay_and_forgets_old_ones() {
        let lag = RuntimeLag::default();
        let t0 = lag.origin;
        lag.record(t0, Duration::from_millis(3));
        lag.record(t0 + Duration::from_millis(600), Duration::from_millis(80));
        lag.record(t0 + Duration::from_millis(1100), Duration::from_millis(5));
        assert_eq!(
            lag.recent_max_at(t0 + Duration::from_millis(1200)),
            Duration::from_millis(80)
        );
        // 2 s after the 80 ms sample's bucket it no longer counts.
        assert_eq!(
            lag.recent_max_at(t0 + Duration::from_millis(2600)),
            Duration::from_millis(5)
        );
        assert_eq!(
            lag.recent_max_at(t0 + Duration::from_secs(10)),
            Duration::ZERO
        );
    }

    #[test]
    fn a_probe_that_has_not_woken_up_yet_counts_as_lag() {
        let lag = RuntimeLag::default();
        let t0 = lag.origin;
        lag.sleeping_since_micros
            .store(lag.micros_since_origin(t0) + 1, Ordering::Release);
        assert_eq!(lag.recent_max_at(t0 + PROBE_INTERVAL), Duration::ZERO);
        assert_eq!(
            lag.recent_max_at(t0 + PROBE_INTERVAL + Duration::from_millis(300)),
            Duration::from_millis(300)
        );
    }

    #[test]
    fn a_reused_slot_starts_from_the_new_sample() {
        let lag = RuntimeLag::default();
        let t0 = lag.origin;
        lag.record(t0, Duration::from_millis(90));
        // Same slot, four buckets later.
        lag.record(t0 + BUCKET_SPAN * BUCKETS as u32, Duration::from_millis(1));
        assert_eq!(
            lag.recent_max_at(t0 + BUCKET_SPAN * BUCKETS as u32),
            Duration::from_millis(1)
        );
    }
}
