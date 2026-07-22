use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use serde::{Deserialize, Serialize};

pub(crate) const DEFAULT_INFERENCE_UPSTREAM_MAX_ATTEMPTS: u32 = 4;
pub(crate) const MIN_INFERENCE_UPSTREAM_MAX_ATTEMPTS: u32 = 1;
pub(crate) const MAX_INFERENCE_UPSTREAM_MAX_ATTEMPTS: u32 = 10;
pub(crate) const DEFAULT_AUXILIARY_UPSTREAM_MAX_ATTEMPTS: u32 = 2;
pub(crate) const MIN_AUXILIARY_UPSTREAM_MAX_ATTEMPTS: u32 = 1;
pub(crate) const MAX_AUXILIARY_UPSTREAM_MAX_ATTEMPTS: u32 = 10;
pub(crate) const DEFAULT_AUXILIARY_UPSTREAM_MAX_CONCURRENT_REQUESTS: u32 = 16;
pub(crate) const MIN_AUXILIARY_UPSTREAM_MAX_CONCURRENT_REQUESTS: u32 = 1;
pub(crate) const MAX_AUXILIARY_UPSTREAM_MAX_CONCURRENT_REQUESTS: u32 = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InferenceAttemptKind {
    LocalCredential,
    ExternalPool,
    Mcp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InferenceAttemptRejection {
    Exhausted,
    ReservedForFallback,
    DownstreamCommitted,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InferenceAttemptSnapshot {
    pub max_attempts: u32,
    pub consumed: u32,
    pub local_attempts: u32,
    pub external_attempts: u32,
    pub mcp_attempts: u32,
    pub exhausted: bool,
    pub downstream_committed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuxiliaryAttemptKind {
    TokenRefresh,
    ProfileDiscovery,
}

impl AuxiliaryAttemptKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::TokenRefresh => "token_refresh",
            Self::ProfileDiscovery => "profile_discovery",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AuxiliaryAttemptSnapshot {
    pub max_attempts: u32,
    pub consumed: u32,
    pub token_refresh_attempts: u32,
    pub profile_discovery_attempts: u32,
    pub exhausted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AuxiliaryAttemptBudgetExhausted {
    pub(crate) kind: AuxiliaryAttemptKind,
    pub(crate) max_attempts: u32,
    pub(crate) consumed: u32,
}

impl std::fmt::Display for AuxiliaryAttemptBudgetExhausted {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "auxiliary upstream attempt budget exhausted for {} ({}/{})",
            self.kind.as_str(),
            self.consumed,
            self.max_attempts
        )
    }
}

impl std::error::Error for AuxiliaryAttemptBudgetExhausted {}

#[derive(Debug)]
pub(crate) struct AuxiliaryAttemptBudget {
    max_attempts: u32,
    consumed: AtomicU32,
    token_refresh_attempts: AtomicU32,
    profile_discovery_attempts: AtomicU32,
}

impl AuxiliaryAttemptBudget {
    pub(crate) fn new(max_attempts: u32) -> Self {
        Self {
            max_attempts: max_attempts.clamp(
                MIN_AUXILIARY_UPSTREAM_MAX_ATTEMPTS,
                MAX_AUXILIARY_UPSTREAM_MAX_ATTEMPTS,
            ),
            consumed: AtomicU32::new(0),
            token_refresh_attempts: AtomicU32::new(0),
            profile_discovery_attempts: AtomicU32::new(0),
        }
    }

    /// Reserve immediately before one real auxiliary HTTP send. A rejected
    /// reservation never changes channel counters or credential health.
    pub(crate) fn reserve(
        &self,
        kind: AuxiliaryAttemptKind,
    ) -> Result<u32, AuxiliaryAttemptBudgetExhausted> {
        let next = self
            .consumed
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |consumed| {
                (consumed < self.max_attempts).then_some(consumed + 1)
            })
            .map(|previous| previous + 1)
            .map_err(|consumed| AuxiliaryAttemptBudgetExhausted {
                kind,
                max_attempts: self.max_attempts,
                consumed,
            })?;
        match kind {
            AuxiliaryAttemptKind::TokenRefresh => {
                self.token_refresh_attempts.fetch_add(1, Ordering::Relaxed);
            }
            AuxiliaryAttemptKind::ProfileDiscovery => {
                self.profile_discovery_attempts
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
        Ok(next)
    }

    /// Reject work before it acquires process resources when no auxiliary send remains.
    ///
    /// This is only an advisory precheck. Concurrent callers must still call `reserve`
    /// immediately before the real HTTP send; that atomic reservation remains authoritative.
    pub(crate) fn ensure_available(
        &self,
        kind: AuxiliaryAttemptKind,
    ) -> Result<(), AuxiliaryAttemptBudgetExhausted> {
        let consumed = self.consumed.load(Ordering::Acquire);
        if consumed >= self.max_attempts {
            return Err(AuxiliaryAttemptBudgetExhausted {
                kind,
                max_attempts: self.max_attempts,
                consumed,
            });
        }
        Ok(())
    }

    pub(crate) fn available_attempts(&self) -> u32 {
        self.max_attempts
            .saturating_sub(self.consumed.load(Ordering::Acquire))
    }

    pub(crate) fn snapshot(&self) -> AuxiliaryAttemptSnapshot {
        let consumed = self.consumed.load(Ordering::Acquire);
        AuxiliaryAttemptSnapshot {
            max_attempts: self.max_attempts,
            consumed,
            token_refresh_attempts: self.token_refresh_attempts.load(Ordering::Acquire),
            profile_discovery_attempts: self.profile_discovery_attempts.load(Ordering::Acquire),
            exhausted: consumed >= self.max_attempts,
        }
    }
}

#[derive(Debug)]
pub(crate) struct InferenceAttemptBudget {
    max_attempts: u32,
    state: AtomicU32,
    local_attempts: AtomicU32,
    external_attempts: AtomicU32,
    mcp_attempts: AtomicU32,
    exhausted: AtomicBool,
    auxiliary: Arc<AuxiliaryAttemptBudget>,
}

const DOWNSTREAM_COMMITTED_BIT: u32 = 1 << 31;
const CONSUMED_MASK: u32 = DOWNSTREAM_COMMITTED_BIT - 1;

impl InferenceAttemptBudget {
    #[cfg(test)]
    pub(crate) fn new(max_attempts: u32) -> Self {
        Self::with_auxiliary_max_attempts(max_attempts, DEFAULT_AUXILIARY_UPSTREAM_MAX_ATTEMPTS)
    }

    pub(crate) fn with_auxiliary_max_attempts(
        max_attempts: u32,
        auxiliary_max_attempts: u32,
    ) -> Self {
        Self {
            max_attempts: max_attempts.clamp(
                MIN_INFERENCE_UPSTREAM_MAX_ATTEMPTS,
                MAX_INFERENCE_UPSTREAM_MAX_ATTEMPTS,
            ),
            state: AtomicU32::new(0),
            local_attempts: AtomicU32::new(0),
            external_attempts: AtomicU32::new(0),
            mcp_attempts: AtomicU32::new(0),
            exhausted: AtomicBool::new(false),
            auxiliary: Arc::new(AuxiliaryAttemptBudget::new(auxiliary_max_attempts)),
        }
    }

    pub(crate) fn auxiliary_budget(&self) -> Arc<AuxiliaryAttemptBudget> {
        self.auxiliary.clone()
    }

    pub(crate) fn auxiliary_snapshot(&self) -> AuxiliaryAttemptSnapshot {
        self.auxiliary.snapshot()
    }

    /// Reserves one real inference HTTP send. `preserve_attempts` is a routing
    /// policy limit, used to keep capacity for a later channel without
    /// pre-consuming a synthetic attempt.
    pub(crate) fn reserve(
        &self,
        kind: InferenceAttemptKind,
        preserve_attempts: u32,
    ) -> Result<u32, InferenceAttemptRejection> {
        let effective_limit = self
            .max_attempts
            .saturating_sub(preserve_attempts.min(self.max_attempts.saturating_sub(1)));
        let next = self
            .state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                if state & DOWNSTREAM_COMMITTED_BIT != 0 {
                    return None;
                }
                let consumed = state & CONSUMED_MASK;
                (consumed < effective_limit).then_some(consumed + 1)
            })
            .map(|previous| (previous & CONSUMED_MASK) + 1)
            .map_err(|state| {
                if state & DOWNSTREAM_COMMITTED_BIT != 0 {
                    InferenceAttemptRejection::DownstreamCommitted
                } else if state & CONSUMED_MASK < self.max_attempts {
                    InferenceAttemptRejection::ReservedForFallback
                } else {
                    self.exhausted.store(true, Ordering::Release);
                    InferenceAttemptRejection::Exhausted
                }
            })?;

        match kind {
            InferenceAttemptKind::LocalCredential => {
                self.local_attempts.fetch_add(1, Ordering::Relaxed);
            }
            InferenceAttemptKind::ExternalPool => {
                self.external_attempts.fetch_add(1, Ordering::Relaxed);
            }
            InferenceAttemptKind::Mcp => {
                self.mcp_attempts.fetch_add(1, Ordering::Relaxed);
            }
        }
        Ok(next)
    }

    pub(crate) fn mark_downstream_committed(&self) {
        self.state
            .fetch_or(DOWNSTREAM_COMMITTED_BIT, Ordering::AcqRel);
    }

    pub(crate) fn available_attempts(&self, preserve_attempts: u32) -> u32 {
        let state = self.state.load(Ordering::Acquire);
        if state & DOWNSTREAM_COMMITTED_BIT != 0 {
            return 0;
        }
        let consumed = state & CONSUMED_MASK;
        self.max_attempts
            .saturating_sub(preserve_attempts.min(self.max_attempts.saturating_sub(1)))
            .saturating_sub(consumed)
    }

    pub(crate) fn snapshot(&self) -> InferenceAttemptSnapshot {
        let state = self.state.load(Ordering::Acquire);
        let consumed = state & CONSUMED_MASK;
        InferenceAttemptSnapshot {
            max_attempts: self.max_attempts,
            consumed,
            local_attempts: self.local_attempts.load(Ordering::Acquire),
            external_attempts: self.external_attempts.load(Ordering::Acquire),
            mcp_attempts: self.mcp_attempts.load(Ordering::Acquire),
            exhausted: consumed >= self.max_attempts || self.exhausted.load(Ordering::Acquire),
            downstream_committed: state & DOWNSTREAM_COMMITTED_BIT != 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn counts_channels_without_exceeding_limit_for_five_rounds() {
        for _ in 0..5 {
            let budget = InferenceAttemptBudget::new(4);
            assert_eq!(
                budget.reserve(InferenceAttemptKind::LocalCredential, 0),
                Ok(1)
            );
            assert_eq!(budget.reserve(InferenceAttemptKind::Mcp, 0), Ok(2));
            assert_eq!(budget.reserve(InferenceAttemptKind::ExternalPool, 0), Ok(3));
            assert_eq!(budget.reserve(InferenceAttemptKind::ExternalPool, 0), Ok(4));
            assert_eq!(
                budget.reserve(InferenceAttemptKind::LocalCredential, 0),
                Err(InferenceAttemptRejection::Exhausted)
            );
            assert_eq!(
                budget.snapshot(),
                InferenceAttemptSnapshot {
                    max_attempts: 4,
                    consumed: 4,
                    local_attempts: 1,
                    mcp_attempts: 1,
                    external_attempts: 2,
                    exhausted: true,
                    downstream_committed: false,
                }
            );
        }
    }

    #[test]
    fn mcp_sends_have_a_distinct_counter_inside_the_shared_budget_for_five_rounds() {
        for _ in 0..5 {
            let budget = InferenceAttemptBudget::new(4);
            assert_eq!(budget.reserve(InferenceAttemptKind::Mcp, 0), Ok(1));
            let snapshot = budget.snapshot();
            assert_eq!(snapshot.consumed, 1);
            assert_eq!(snapshot.local_attempts, 0);
            assert_eq!(snapshot.external_attempts, 0);
            assert_eq!(snapshot.mcp_attempts, 1);
            assert_eq!(
                snapshot.local_attempts + snapshot.external_attempts + snapshot.mcp_attempts,
                snapshot.consumed
            );
        }
    }

    #[test]
    fn policy_reservation_does_not_consume_a_synthetic_attempt() {
        let budget = InferenceAttemptBudget::new(4);
        assert_eq!(
            budget.reserve(InferenceAttemptKind::LocalCredential, 1),
            Ok(1)
        );
        assert_eq!(
            budget.reserve(InferenceAttemptKind::LocalCredential, 1),
            Ok(2)
        );
        assert_eq!(
            budget.reserve(InferenceAttemptKind::LocalCredential, 1),
            Ok(3)
        );
        assert_eq!(
            budget.reserve(InferenceAttemptKind::LocalCredential, 1),
            Err(InferenceAttemptRejection::ReservedForFallback)
        );
        assert_eq!(budget.snapshot().consumed, 3);
        assert!(!budget.snapshot().exhausted);
        assert_eq!(budget.reserve(InferenceAttemptKind::ExternalPool, 0), Ok(4));
        assert!(budget.snapshot().exhausted);
    }

    #[test]
    fn downstream_commit_rejects_all_later_sends_for_five_rounds() {
        for _ in 0..5 {
            let budget = InferenceAttemptBudget::new(4);
            assert_eq!(
                budget.reserve(InferenceAttemptKind::LocalCredential, 0),
                Ok(1)
            );
            budget.mark_downstream_committed();
            assert_eq!(
                budget.reserve(InferenceAttemptKind::LocalCredential, 0),
                Err(InferenceAttemptRejection::DownstreamCommitted)
            );
            assert_eq!(
                budget.reserve(InferenceAttemptKind::ExternalPool, 0),
                Err(InferenceAttemptRejection::DownstreamCommitted)
            );
            let snapshot = budget.snapshot();
            assert_eq!(snapshot.consumed, 1);
            assert!(!snapshot.exhausted);
            assert!(snapshot.downstream_committed);
        }
    }

    #[test]
    fn concurrent_reservations_never_cross_hard_limit_for_five_rounds() {
        for max_attempts in [1, 4, 10] {
            for _ in 0..5 {
                let budget = Arc::new(InferenceAttemptBudget::new(max_attempts));
                let mut threads = Vec::new();
                for index in 0..64 {
                    let budget = budget.clone();
                    threads.push(std::thread::spawn(move || {
                        let kind = match index % 3 {
                            0 => InferenceAttemptKind::LocalCredential,
                            1 => InferenceAttemptKind::ExternalPool,
                            _ => InferenceAttemptKind::Mcp,
                        };
                        budget.reserve(kind, 0).is_ok()
                    }));
                }
                let successes = threads
                    .into_iter()
                    .map(|thread| thread.join().expect("reservation thread"))
                    .filter(|reserved| *reserved)
                    .count() as u32;
                let snapshot = budget.snapshot();
                assert_eq!(successes, max_attempts);
                assert_eq!(snapshot.consumed, max_attempts);
                assert!(snapshot.exhausted);
                assert_eq!(
                    snapshot.local_attempts + snapshot.external_attempts + snapshot.mcp_attempts,
                    max_attempts
                );
            }
        }
    }

    #[test]
    fn zero_remaining_budget_is_exhausted_without_an_extra_rejected_reserve_for_five_rounds() {
        for max_attempts in [1, 4, 10] {
            for _ in 0..5 {
                let budget = InferenceAttemptBudget::new(max_attempts);
                for expected in 1..=max_attempts {
                    let kind = match expected % 3 {
                        0 => InferenceAttemptKind::Mcp,
                        1 => InferenceAttemptKind::LocalCredential,
                        _ => InferenceAttemptKind::ExternalPool,
                    };
                    assert_eq!(budget.reserve(kind, 0), Ok(expected));
                }

                assert_eq!(budget.available_attempts(0), 0);
                let snapshot = budget.snapshot();
                assert_eq!(snapshot.consumed, max_attempts);
                assert_eq!(
                    snapshot.local_attempts + snapshot.external_attempts + snapshot.mcp_attempts,
                    max_attempts
                );
                assert!(snapshot.exhausted);
                assert!(!snapshot.downstream_committed);
            }
        }
    }

    #[test]
    fn exact_concurrent_limit_is_exhausted_without_a_rejected_reservation_for_five_rounds() {
        for max_attempts in [1, 4, 10] {
            for _ in 0..5 {
                let budget = Arc::new(InferenceAttemptBudget::new(max_attempts));
                let barrier = Arc::new(std::sync::Barrier::new(max_attempts as usize));
                let threads = (0..max_attempts)
                    .map(|index| {
                        let budget = budget.clone();
                        let barrier = barrier.clone();
                        std::thread::spawn(move || {
                            barrier.wait();
                            let kind = match index % 3 {
                                0 => InferenceAttemptKind::LocalCredential,
                                1 => InferenceAttemptKind::ExternalPool,
                                _ => InferenceAttemptKind::Mcp,
                            };
                            budget.reserve(kind, 0)
                        })
                    })
                    .collect::<Vec<_>>();

                for thread in threads {
                    assert!(thread.join().expect("reservation thread").is_ok());
                }
                let snapshot = budget.snapshot();
                assert_eq!(snapshot.consumed, max_attempts);
                assert_eq!(
                    snapshot.local_attempts + snapshot.external_attempts + snapshot.mcp_attempts,
                    max_attempts
                );
                assert!(snapshot.exhausted);
            }
        }
    }

    #[test]
    fn configured_zero_clamps_to_one_and_only_exhausts_after_consumption_for_five_rounds() {
        for _ in 0..5 {
            let budget = InferenceAttemptBudget::new(0);
            let initial = budget.snapshot();
            assert_eq!(initial.max_attempts, 1);
            assert_eq!(initial.consumed, 0);
            assert!(!initial.exhausted);

            assert_eq!(
                budget.reserve(InferenceAttemptKind::LocalCredential, 0),
                Ok(1)
            );
            assert_eq!(budget.available_attempts(0), 0);
            assert!(budget.snapshot().exhausted);
        }
    }

    #[test]
    fn remaining_capacity_is_not_reported_as_exhausted_for_five_rounds() {
        for _ in 0..5 {
            let budget = InferenceAttemptBudget::new(4);
            for expected in 1..=3 {
                assert_eq!(
                    budget.reserve(InferenceAttemptKind::LocalCredential, 0),
                    Ok(expected)
                );
            }
            assert_eq!(budget.available_attempts(0), 1);
            let snapshot = budget.snapshot();
            assert_eq!(snapshot.consumed, 3);
            assert!(!snapshot.exhausted);
        }
    }

    #[test]
    fn max_one_keeps_local_first_without_creating_a_zero_attempt_path() {
        for _ in 0..5 {
            let budget = InferenceAttemptBudget::new(1);
            assert_eq!(budget.available_attempts(1), 1);
            assert_eq!(
                budget.reserve(InferenceAttemptKind::LocalCredential, 1),
                Ok(1)
            );
            assert_eq!(
                budget.reserve(InferenceAttemptKind::ExternalPool, 0),
                Err(InferenceAttemptRejection::Exhausted)
            );
            let snapshot = budget.snapshot();
            assert_eq!(snapshot.local_attempts, 1);
            assert_eq!(snapshot.external_attempts, 0);
            assert!(snapshot.exhausted);
        }
    }

    #[test]
    fn unavailable_fallback_does_not_reduce_local_attempts() {
        for _ in 0..5 {
            let budget = InferenceAttemptBudget::new(4);
            assert_eq!(budget.available_attempts(0), 4);
            for expected in 1..=4 {
                assert_eq!(
                    budget.reserve(InferenceAttemptKind::LocalCredential, 0),
                    Ok(expected)
                );
            }
            assert_eq!(budget.snapshot().local_attempts, 4);
        }
    }

    #[test]
    fn successful_local_send_does_not_touch_external_counter() {
        let budget = InferenceAttemptBudget::new(4);
        assert_eq!(
            budget.reserve(InferenceAttemptKind::LocalCredential, 1),
            Ok(1)
        );
        assert_eq!(
            budget.snapshot(),
            InferenceAttemptSnapshot {
                max_attempts: 4,
                consumed: 1,
                local_attempts: 1,
                external_attempts: 0,
                mcp_attempts: 0,
                exhausted: false,
                downstream_committed: false,
            }
        );
    }

    #[test]
    fn cancellation_after_reservation_does_not_refund_a_possible_send() {
        for _ in 0..5 {
            let budget = InferenceAttemptBudget::new(4);
            let reservation = budget.reserve(InferenceAttemptKind::LocalCredential, 0);
            assert_eq!(reservation, Ok(1));
            assert_eq!(budget.snapshot().consumed, 1);
            assert_eq!(budget.available_attempts(0), 3);
        }
    }

    #[test]
    fn auxiliary_focus_channels_are_bounded_and_do_not_touch_inference_for_five_rounds() {
        for _ in 0..5 {
            let request_budget = InferenceAttemptBudget::with_auxiliary_max_attempts(4, 2);
            let auxiliary = request_budget.auxiliary_budget();
            assert_eq!(auxiliary.reserve(AuxiliaryAttemptKind::TokenRefresh), Ok(1));
            assert_eq!(
                auxiliary.reserve(AuxiliaryAttemptKind::ProfileDiscovery),
                Ok(2)
            );
            assert_eq!(
                auxiliary.reserve(AuxiliaryAttemptKind::TokenRefresh),
                Err(AuxiliaryAttemptBudgetExhausted {
                    kind: AuxiliaryAttemptKind::TokenRefresh,
                    max_attempts: 2,
                    consumed: 2,
                })
            );
            assert_eq!(request_budget.snapshot().consumed, 0);
            assert_eq!(
                request_budget.auxiliary_snapshot(),
                AuxiliaryAttemptSnapshot {
                    max_attempts: 2,
                    consumed: 2,
                    token_refresh_attempts: 1,
                    profile_discovery_attempts: 1,
                    exhausted: true,
                }
            );
        }
    }

    #[test]
    fn auxiliary_focus_concurrent_reservations_never_cross_limit_for_five_rounds() {
        for max_attempts in [1_u32, 2, 10] {
            for _ in 0..5 {
                let budget = Arc::new(AuxiliaryAttemptBudget::new(max_attempts));
                let threads = (0..64)
                    .map(|index| {
                        let budget = budget.clone();
                        std::thread::spawn(move || {
                            let kind = if index % 2 == 0 {
                                AuxiliaryAttemptKind::TokenRefresh
                            } else {
                                AuxiliaryAttemptKind::ProfileDiscovery
                            };
                            budget.reserve(kind).is_ok()
                        })
                    })
                    .collect::<Vec<_>>();
                let reserved = threads
                    .into_iter()
                    .map(|thread| thread.join().expect("auxiliary reservation thread"))
                    .filter(|reserved| *reserved)
                    .count() as u32;
                let snapshot = budget.snapshot();
                assert_eq!(reserved, max_attempts);
                assert_eq!(snapshot.consumed, max_attempts);
                assert_eq!(
                    snapshot.token_refresh_attempts + snapshot.profile_discovery_attempts,
                    max_attempts
                );
                assert!(snapshot.exhausted);
            }
        }
    }

    #[test]
    fn auxiliary_focus_default_and_clamps_are_independent_from_inference() {
        let defaulted = InferenceAttemptBudget::new(4);
        assert_eq!(
            defaulted.auxiliary_snapshot().max_attempts,
            DEFAULT_AUXILIARY_UPSTREAM_MAX_ATTEMPTS
        );
        let clamped_low = InferenceAttemptBudget::with_auxiliary_max_attempts(4, 0);
        assert_eq!(clamped_low.auxiliary_snapshot().max_attempts, 1);
        let clamped_high = InferenceAttemptBudget::with_auxiliary_max_attempts(1, u32::MAX);
        assert_eq!(clamped_high.auxiliary_snapshot().max_attempts, 10);
        assert_eq!(clamped_high.snapshot().max_attempts, 1);
    }

    #[test]
    fn auxiliary_focus_availability_precheck_is_non_consuming_and_reserve_stays_authoritative() {
        for _ in 0..5 {
            let budget = AuxiliaryAttemptBudget::new(1);
            assert_eq!(
                budget.ensure_available(AuxiliaryAttemptKind::ProfileDiscovery),
                Ok(())
            );
            assert_eq!(budget.snapshot().consumed, 0);
            assert_eq!(budget.reserve(AuxiliaryAttemptKind::TokenRefresh), Ok(1));
            let error = budget
                .ensure_available(AuxiliaryAttemptKind::ProfileDiscovery)
                .unwrap_err();
            assert_eq!(error.consumed, 1);
            assert_eq!(budget.snapshot().profile_discovery_attempts, 0);
        }
    }
}
