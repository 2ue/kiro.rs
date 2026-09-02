use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};

use tokio::sync::Notify;

#[derive(Debug, Default)]
pub(crate) struct CapacitySignal {
    waiters: AtomicUsize,
    credits: AtomicUsize,
    capacity_changed: Notify,
    state_generation: AtomicU64,
    state_changed: Notify,
}

impl CapacitySignal {
    pub(crate) fn register(self: &Arc<Self>) -> CapacityWaiter {
        self.waiters.fetch_add(1, Ordering::AcqRel);
        CapacityWaiter {
            signal: self.clone(),
            observed_state_generation: self.state_generation.load(Ordering::Acquire),
            active: true,
        }
    }

    pub(crate) fn capacity_released(&self, units: usize) {
        if units == 0 {
            return;
        }
        let added = loop {
            let waiters = self.waiters.load(Ordering::Acquire);
            let current = self.credits.load(Ordering::Acquire);
            let target = current.saturating_add(units).min(waiters);
            if target == current {
                return;
            }
            if self
                .credits
                .compare_exchange(current, target, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break target - current;
            }
        };
        for _ in 0..added {
            self.capacity_changed.notify_one();
        }
    }

    pub(crate) fn notify_state_changed(&self) {
        self.state_generation.fetch_add(1, Ordering::AcqRel);
        self.state_changed.notify_waiters();
    }

    fn consume_credit(&self) -> bool {
        let mut current = self.credits.load(Ordering::Acquire);
        while current > 0 {
            match self.credits.compare_exchange_weak(
                current,
                current - 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(actual) => current = actual,
            }
        }
        false
    }

    fn unregister(&self) {
        // A waiter owns exactly one registration, but cancellation and task
        // teardown can race with the caller's final state transition. Use a
        // saturating CAS instead of `fetch_sub`: a duplicate teardown must not
        // wrap the counter (or panic in debug builds), which would corrupt
        // future capacity-credit accounting for the whole scheduler.
        let mut current = self.waiters.load(Ordering::Acquire);
        let remaining = loop {
            if current == 0 {
                tracing::debug!("capacity waiter unregister observed an already-empty waiter set");
                return;
            }
            let remaining = current - 1;
            match self.waiters.compare_exchange_weak(
                current,
                remaining,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break remaining,
                Err(actual) => current = actual,
            }
        };
        let _ = self
            .credits
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |credits| {
                (credits > remaining).then_some(remaining)
            });
    }

    #[cfg(test)]
    pub(crate) fn test_snapshot(&self) -> (usize, usize, u64) {
        (
            self.waiters.load(Ordering::Acquire),
            self.credits.load(Ordering::Acquire),
            self.state_generation.load(Ordering::Acquire),
        )
    }
}

pub(crate) struct CapacityWaiter {
    signal: Arc<CapacitySignal>,
    observed_state_generation: u64,
    active: bool,
}

impl CapacityWaiter {
    pub(crate) async fn wait_for_change(&mut self) {
        loop {
            if self.signal.consume_credit() {
                return;
            }
            let generation = self.signal.state_generation.load(Ordering::Acquire);
            if generation != self.observed_state_generation {
                self.observed_state_generation = generation;
                return;
            }

            let capacity_changed = self.signal.capacity_changed.notified();
            let state_changed = self.signal.state_changed.notified();
            tokio::pin!(capacity_changed);
            tokio::pin!(state_changed);
            capacity_changed.as_mut().enable();
            state_changed.as_mut().enable();

            if self.signal.consume_credit() {
                return;
            }
            let generation = self.signal.state_generation.load(Ordering::Acquire);
            if generation != self.observed_state_generation {
                self.observed_state_generation = generation;
                return;
            }

            tokio::select! {
                _ = capacity_changed.as_mut() => {}
                _ = state_changed.as_mut() => {}
            }
        }
    }

    pub(crate) fn finish_acquired(&mut self) {
        if !self.active {
            return;
        }
        let _ = self.signal.consume_credit();
        self.signal.unregister();
        self.active = false;
    }
}

impl Drop for CapacityWaiter {
    fn drop(&mut self) {
        if self.active {
            self.signal.unregister();
            self.active = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn capacity_credits_do_not_collapse_before_waiters_poll_for_five_rounds() {
        for round in 0..5 {
            let signal = Arc::new(CapacitySignal::default());
            let mut waiters = (0..16).map(|_| signal.register()).collect::<Vec<_>>();
            signal.capacity_released(16);

            tokio::time::timeout(Duration::from_millis(100), async {
                futures::future::join_all(waiters.iter_mut().map(CapacityWaiter::wait_for_change))
                    .await;
            })
            .await
            .unwrap_or_else(|_| panic!("round {round}: all sixteen credits must remain distinct"));
            assert_eq!(signal.test_snapshot(), (16, 0, 0), "round {round}");
        }
    }

    #[tokio::test]
    async fn state_generation_closes_register_to_wait_race_for_five_rounds() {
        for round in 0..5 {
            let signal = Arc::new(CapacitySignal::default());
            let mut waiter = signal.register();
            signal.notify_state_changed();
            tokio::time::timeout(Duration::from_millis(100), waiter.wait_for_change())
                .await
                .unwrap_or_else(|_| panic!("round {round}: generation change must not be lost"));
            assert_eq!(signal.test_snapshot(), (1, 0, 1), "round {round}");
        }
    }

    #[tokio::test]
    async fn releases_without_waiters_do_not_create_stale_spin_credits() {
        let signal = Arc::new(CapacitySignal::default());
        signal.capacity_released(10_000);
        assert_eq!(signal.test_snapshot(), (0, 0, 0));

        let mut waiter = signal.register();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), waiter.wait_for_change())
                .await
                .is_err()
        );
    }

    #[test]
    fn finishing_or_dropping_waiters_trims_unclaimed_credits() {
        let signal = Arc::new(CapacitySignal::default());
        let mut first = signal.register();
        let second = signal.register();
        signal.capacity_released(2);
        first.finish_acquired();
        drop(second);
        assert_eq!(signal.test_snapshot(), (0, 0, 0));
    }

    #[test]
    fn duplicate_unregister_is_saturating_and_does_not_corrupt_future_waiters() {
        let signal = Arc::new(CapacitySignal::default());
        signal.unregister();
        assert_eq!(signal.test_snapshot(), (0, 0, 0));

        let waiter = signal.register();
        drop(waiter);
        signal.unregister();
        assert_eq!(signal.test_snapshot(), (0, 0, 0));
    }
}
