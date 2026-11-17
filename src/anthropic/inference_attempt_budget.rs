use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub(crate) const MIN_INFERENCE_UPSTREAM_MAX_ATTEMPTS: u32 = 1;
pub(crate) const MAX_INFERENCE_UPSTREAM_MAX_ATTEMPTS: u32 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InferenceAttemptKind {
    Account,
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
    pub external_attempts: u32,
    pub exhausted: bool,
    pub downstream_committed: bool,
}

#[derive(Debug)]
pub(crate) struct InferenceAttemptBudget {
    /// 请求级发送上限。
    ///
    /// 常规路径构造后即固定在 `1..=10`；狂暴模式会通过
    /// [`raise_max_attempts_for_berserk`] 一次性抬高到 `账号×端点×轮数`，
    /// 因此这里必须是原子的。抬高只放宽计数上限，`reserve` 的
    /// 「下游已提交」「为 fallback 预留」两条语义完全不受影响。
    max_attempts: AtomicU32,
    state: AtomicU32,
    external_attempts: AtomicU32,
    exhausted: AtomicBool,
    started_at: Instant,
    dispatch_deadline: OnceLock<Instant>,
    /// Request entry route (e.g. `/cc/v1/messages`) used to apply per-credential route
    /// rules on every local dispatch of this request, including stream retries and MCP calls.
    dispatch_route: OnceLock<String>,
    /// 本请求内已经空回（只有 usage/metadata、零帧或空响应体）的本地账号。同一请求后续派发
    /// 跳过它们；是否另外冷却由全局开关 `credentialEmptyTurnCooldownEnabled` 决定。
    credential_dispatch_elapsed_ms: AtomicU64,
    upstream_header_wait_ms: AtomicU64,
}

const DOWNSTREAM_COMMITTED_BIT: u32 = 1 << 31;
const CONSUMED_MASK: u32 = DOWNSTREAM_COMMITTED_BIT - 1;

impl InferenceAttemptBudget {
    pub(crate) fn new(max_attempts: u32) -> Self {
        Self {
            max_attempts: AtomicU32::new(max_attempts.clamp(
                MIN_INFERENCE_UPSTREAM_MAX_ATTEMPTS,
                MAX_INFERENCE_UPSTREAM_MAX_ATTEMPTS,
            )),
            state: AtomicU32::new(0),
            external_attempts: AtomicU32::new(0),
            exhausted: AtomicBool::new(false),
            started_at: Instant::now(),
            dispatch_deadline: OnceLock::new(),
            dispatch_route: OnceLock::new(),
            credential_dispatch_elapsed_ms: AtomicU64::new(0),
            upstream_header_wait_ms: AtomicU64::new(0),
        }
    }

    #[cfg(test)]
    pub(crate) fn record_credential_dispatch_elapsed(&self, elapsed: Duration) {
        saturating_add_ms(&self.credential_dispatch_elapsed_ms, elapsed);
    }

    pub(crate) fn record_upstream_header_wait(&self, elapsed: Duration) {
        saturating_add_ms(&self.upstream_header_wait_ms, elapsed);
    }

    pub(crate) fn credential_dispatch_elapsed_ms(&self) -> Option<u64> {
        nonzero_ms(&self.credential_dispatch_elapsed_ms)
    }

    pub(crate) fn upstream_header_wait_ms(&self) -> Option<u64> {
        nonzero_ms(&self.upstream_header_wait_ms)
    }

    pub(crate) fn max_attempts(&self) -> u32 {
        self.max_attempts.load(Ordering::Acquire)
    }

    /// The request start is shared by local and external routing phases.  A
    /// fallback route must not receive a fresh queue budget merely because it
    /// was entered later in the request lifecycle.
    pub(crate) fn started_at(&self) -> Instant {
        self.started_at
    }

    /// Installs the request-wide dispatch deadline once at admission time.
    /// Tests that construct a budget directly intentionally leave it unset and
    /// retain the legacy per-stage wait behavior.
    pub(crate) fn set_dispatch_deadline_after(&self, duration: Duration) {
        let _ = self
            .dispatch_deadline
            .set(self.started_at + duration.max(Duration::from_millis(1)));
    }

    /// Installs the request route once; later calls keep the first value.
    pub(crate) fn set_dispatch_route(&self, route: &str) {
        let _ = self.dispatch_route.set(route.to_string());
    }

    pub(crate) fn dispatch_deadline(&self) -> Option<Instant> {
        self.dispatch_deadline.get().copied()
    }

    #[cfg(test)]
    pub(crate) fn dispatch_remaining(&self) -> Option<Duration> {
        self.dispatch_deadline()
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }

    /// Reserves one real inference HTTP send. `preserve_attempts` is a routing
    /// policy limit, used to keep capacity for a later channel without
    /// pre-consuming a synthetic attempt.
    pub(crate) fn reserve(
        &self,
        kind: InferenceAttemptKind,
        preserve_attempts: u32,
    ) -> Result<u32, InferenceAttemptRejection> {
        let max_attempts = self.max_attempts();
        let effective_limit =
            max_attempts.saturating_sub(preserve_attempts.min(max_attempts.saturating_sub(1)));
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
                } else if state & CONSUMED_MASK < max_attempts {
                    InferenceAttemptRejection::ReservedForFallback
                } else {
                    self.exhausted.store(true, Ordering::Release);
                    InferenceAttemptRejection::Exhausted
                }
            })?;

        match kind {
            InferenceAttemptKind::Account => {
                self.external_attempts.fetch_add(1, Ordering::Relaxed);
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
        self.max_attempts()
            .saturating_sub(preserve_attempts.min(self.max_attempts().saturating_sub(1)))
            .saturating_sub(consumed)
    }

    pub(crate) fn snapshot(&self) -> InferenceAttemptSnapshot {
        let state = self.state.load(Ordering::Acquire);
        let consumed = state & CONSUMED_MASK;
        InferenceAttemptSnapshot {
            max_attempts: self.max_attempts(),
            consumed,
            external_attempts: self.external_attempts.load(Ordering::Acquire),
            exhausted: consumed >= self.max_attempts() || self.exhausted.load(Ordering::Acquire),
            downstream_committed: state & DOWNSTREAM_COMMITTED_BIT != 0,
        }
    }
}

fn saturating_add_ms(target: &AtomicU64, elapsed: Duration) {
    let millis = elapsed.as_millis().min(u128::from(u64::MAX)) as u64;
    let _ = target.fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
        Some(current.saturating_add(millis))
    });
}

fn nonzero_ms(target: &AtomicU64) -> Option<u64> {
    let value = target.load(Ordering::Acquire);
    (value > 0).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_deadline_is_shared_and_only_installed_once() {
        let budget = InferenceAttemptBudget::new(4);
        budget.set_dispatch_deadline_after(Duration::from_secs(5));
        let first = budget
            .dispatch_deadline()
            .expect("request deadline should be installed");
        budget.set_dispatch_deadline_after(Duration::from_secs(60));
        let second = budget
            .dispatch_deadline()
            .expect("request deadline should remain available");
        assert_eq!(first, second);
        assert!(budget.dispatch_remaining().is_some());
        assert!(budget.started_at() <= first);
    }

    #[test]
    fn latency_attribution_accumulates_dispatch_and_header_wait_without_touching_attempt_budget() {
        let budget = InferenceAttemptBudget::new(4);
        budget.record_credential_dispatch_elapsed(Duration::from_millis(17));
        budget.record_credential_dispatch_elapsed(Duration::from_millis(23));
        budget.record_upstream_header_wait(Duration::from_millis(5));
        budget.record_upstream_header_wait(Duration::from_millis(11));

        assert_eq!(budget.credential_dispatch_elapsed_ms(), Some(40));
        assert_eq!(budget.upstream_header_wait_ms(), Some(16));
        assert_eq!(budget.snapshot().consumed, 0);
    }

    #[test]
    fn latency_attribution_omits_zero_values_for_legacy_records() {
        let budget = InferenceAttemptBudget::new(4);
        assert_eq!(budget.credential_dispatch_elapsed_ms(), None);
        assert_eq!(budget.upstream_header_wait_ms(), None);
    }
}
