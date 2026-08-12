//! Per-request-API-key admission for inference requests.
//!
//! The hot path is process-local and intentionally independent from Redis. A
//! multi-instance deployment therefore has an aggregate limit equal to the sum
//! of each instance's configured limit.

use std::{
    collections::{BTreeSet, HashMap},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};

use axum::{
    body::{Body, Bytes, HttpBody},
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
};
use http_body::{Frame, SizeHint};
use parking_lot::{Mutex, RwLock};
use tokio::sync::Notify;

use crate::{common::auth::RequestApiKeyIdentity, model::config::RequestAdmissionConfig};

use super::{
    envelope,
    usage::{UsageRecorder, sampled_request_rejection_usage_record_with_metadata},
};

const STATE_SHARDS: usize = 16;
const DEFAULT_MAX_TRACKED_KEYS: usize = 4_096;
const DEFAULT_STATE_TTL: Duration = Duration::from_secs(10 * 60);
const DEFAULT_RPM_WINDOW: Duration = Duration::from_secs(60);
const CLEANUP_INTERVAL: usize = 64;
const MAX_RPM_BURST: u32 = 32;
const REJECTION_LOG_BURST_CAPACITY: u32 = 64;
const REJECTION_LOG_REFILL_PER_SECOND: f64 = 8.0;
const REJECTION_LOG_SUMMARY_INTERVAL: Duration = Duration::from_secs(30);
const REQUEST_REJECTION_REASON_COUNT: usize = RequestRejectionReason::COUNT;
const LOCAL_TEMPORARY_BACKOFF_MIN: Duration = Duration::from_secs(1);
const LOCAL_TEMPORARY_BACKOFF_MAX: Duration = Duration::from_secs(8);

#[derive(Debug, Default)]
struct ConcurrencyGate {
    active: u32,
    queued: u32,
    next_ticket: u64,
    serving_ticket: u64,
    cancelled_tickets: BTreeSet<u64>,
}

#[derive(Debug)]
struct RpmBucket {
    last_refill: Instant,
    tokens: f64,
    limit: u32,
}

#[derive(Debug)]
struct KeyState {
    gate: Mutex<ConcurrencyGate>,
    notify: Notify,
    rpm: Mutex<RpmBucket>,
    rejection_counts: [AtomicU64; REQUEST_REJECTION_REASON_COUNT],
    last_seen_ms: AtomicU64,
    local_temporary_backoff_until_ms: AtomicU64,
}

impl KeyState {
    fn new(now: Instant, last_seen_ms: u64) -> Self {
        Self {
            gate: Mutex::new(ConcurrencyGate::default()),
            notify: Notify::new(),
            rpm: Mutex::new(RpmBucket {
                last_refill: now,
                tokens: 0.0,
                limit: 0,
            }),
            rejection_counts: std::array::from_fn(|_| AtomicU64::new(0)),
            last_seen_ms: AtomicU64::new(last_seen_ms),
            local_temporary_backoff_until_ms: AtomicU64::new(0),
        }
    }

    fn is_idle(&self) -> bool {
        let gate = self.gate.lock();
        gate.active == 0 && gate.queued == 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdmissionRejectionKind {
    Rpm,
    ConcurrencyFull,
    QueueFull,
    QueueTimeout,
    StateCapacity,
    LocalTemporaryBackoff,
}

impl AdmissionRejectionKind {
    const fn reason(self) -> Option<RequestRejectionReason> {
        match self {
            Self::Rpm => Some(RequestRejectionReason::AdmissionRpm),
            Self::ConcurrencyFull => Some(RequestRejectionReason::AdmissionConcurrencyFull),
            Self::QueueFull => Some(RequestRejectionReason::AdmissionQueueFull),
            Self::QueueTimeout => Some(RequestRejectionReason::AdmissionQueueTimeout),
            Self::StateCapacity => None,
            Self::LocalTemporaryBackoff => {
                Some(RequestRejectionReason::AdmissionLocalTemporaryBackoff)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RequestRejectionReason {
    AdmissionRpm,
    AdmissionConcurrencyFull,
    AdmissionQueueFull,
    AdmissionQueueTimeout,
    AdmissionStateCapacity,
    BodyTooLarge,
    BodyReadFailed,
    RequestEntryInvalid,
    StrictRequestProtocolContamination,
    DfcacheRouteInvalid,
    ProviderNotReady,
    MultimodalInvalid,
    ModelUnsupported,
    WebSearchUnsupported,
    LocalBodyPrepare,
    LocalPoolUnavailable,
    LocalPoolTemporaryUnavailable,
    AdmissionLocalTemporaryBackoff,
}

impl RequestRejectionReason {
    const COUNT: usize = Self::AdmissionLocalTemporaryBackoff as usize + 1;

    const fn index(self) -> usize {
        self as usize
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::AdmissionRpm => "admission_rpm",
            Self::AdmissionConcurrencyFull => "admission_concurrency_full",
            Self::AdmissionQueueFull => "admission_queue_full",
            Self::AdmissionQueueTimeout => "admission_queue_timeout",
            Self::AdmissionStateCapacity => "admission_state_capacity",
            Self::BodyTooLarge => "body_too_large",
            Self::BodyReadFailed => "body_read_failed",
            Self::RequestEntryInvalid => "request_entry_invalid",
            Self::StrictRequestProtocolContamination => "strict_request_protocol_contamination",
            Self::DfcacheRouteInvalid => "dfcache_route_invalid",
            Self::ProviderNotReady => "provider_not_ready",
            Self::MultimodalInvalid => "multimodal_invalid",
            Self::ModelUnsupported => "model_unsupported",
            Self::WebSearchUnsupported => "websearch_unsupported",
            Self::LocalBodyPrepare => "local_body_prepare",
            Self::LocalPoolUnavailable => "local_pool_unavailable",
            Self::LocalPoolTemporaryUnavailable => "local_pool_temporary_unavailable",
            Self::AdmissionLocalTemporaryBackoff => "admission_local_temporary_backoff",
        }
    }
}

#[derive(Debug, Clone)]
struct AdmissionRejection {
    kind: AdmissionRejectionKind,
    retry_after_secs: u64,
    key_state: Option<Arc<KeyState>>,
}

impl AdmissionRejection {
    fn for_key(
        kind: AdmissionRejectionKind,
        retry_after_secs: u64,
        key_state: Arc<KeyState>,
    ) -> Self {
        debug_assert!(kind.reason().is_some());
        Self {
            kind,
            retry_after_secs: retry_after_secs.max(1),
            key_state: Some(key_state),
        }
    }

    fn state_capacity(retry_after_secs: u64) -> Self {
        Self {
            kind: AdmissionRejectionKind::StateCapacity,
            retry_after_secs: retry_after_secs.max(1),
            key_state: None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct RejectionLogPolicy {
    capacity: u32,
    refill_per_second: f64,
    summary_interval: Duration,
}

impl Default for RejectionLogPolicy {
    fn default() -> Self {
        Self {
            capacity: REJECTION_LOG_BURST_CAPACITY,
            refill_per_second: REJECTION_LOG_REFILL_PER_SECOND,
            summary_interval: REJECTION_LOG_SUMMARY_INTERVAL,
        }
    }
}

impl RejectionLogPolicy {
    fn normalized(self) -> Self {
        Self {
            capacity: self.capacity.max(1),
            refill_per_second: if self.refill_per_second.is_finite() {
                self.refill_per_second.max(0.0)
            } else {
                0.0
            },
            summary_interval: self.summary_interval.max(Duration::from_millis(1)),
        }
    }
}

#[derive(Debug)]
struct RejectionLogTokenBucket {
    tokens: f64,
    last_refill: Instant,
}

impl RejectionLogTokenBucket {
    fn new(now: Instant, capacity: u32) -> Self {
        Self {
            tokens: capacity as f64,
            last_refill: now,
        }
    }

    fn try_take(&mut self, now: Instant, policy: RejectionLogPolicy) -> bool {
        let elapsed = now
            .saturating_duration_since(self.last_refill)
            .as_secs_f64();
        self.tokens =
            (self.tokens + elapsed * policy.refill_per_second).min(policy.capacity as f64);
        self.last_refill = now;
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}

/// Admission controller shared by all `/messages` route variants.
pub struct RequestAdmissionController {
    config: RwLock<RequestAdmissionConfig>,
    shards: Box<[Mutex<HashMap<[u8; 32], Arc<KeyState>>>]>,
    tracked_keys: AtomicUsize,
    operations: AtomicUsize,
    started_at: Instant,
    rpm_window: Duration,
    state_ttl: Duration,
    max_tracked_keys: usize,
    state_capacity_rejection_count: AtomicU64,
    rejection_log_policy: RejectionLogPolicy,
    rejection_log_bucket: Mutex<RejectionLogTokenBucket>,
    rejection_log_suppressed: AtomicU64,
    rejection_log_next_summary_ms: AtomicU64,
    rejection_log_detailed_emitted: AtomicU64,
    rejection_log_summaries_emitted: AtomicU64,
    rejection_log_budget_lock_acquisitions: AtomicU64,
}

#[derive(Clone)]
pub(crate) struct RequestAdmissionMiddlewareState {
    controller: Arc<RequestAdmissionController>,
    recorder: Arc<UsageRecorder>,
}

impl RequestAdmissionMiddlewareState {
    pub(crate) fn new(
        controller: Arc<RequestAdmissionController>,
        recorder: Arc<UsageRecorder>,
    ) -> Self {
        Self {
            controller,
            recorder,
        }
    }
}

#[derive(Clone)]
pub(crate) struct RequestRejectionAttribution {
    controller: Arc<RequestAdmissionController>,
    recorder: Arc<UsageRecorder>,
    identity: RequestApiKeyIdentity,
    key_state: Option<Arc<KeyState>>,
}

impl RequestRejectionAttribution {
    fn new(
        controller: Arc<RequestAdmissionController>,
        recorder: Arc<UsageRecorder>,
        identity: RequestApiKeyIdentity,
        key_state: Option<Arc<KeyState>>,
    ) -> Self {
        Self {
            controller,
            recorder,
            identity,
            key_state,
        }
    }

    #[cfg(test)]
    pub(crate) fn detached(
        controller: Arc<RequestAdmissionController>,
        recorder: Arc<UsageRecorder>,
        identity: RequestApiKeyIdentity,
    ) -> Self {
        Self::new(controller, recorder, identity, None)
    }

    pub(crate) fn request_api_key_id(&self) -> String {
        self.identity.stable_id()
    }

    pub(crate) fn apply_local_temporary_backoff(&self, retry_after_secs: u64) -> bool {
        self.controller.apply_local_temporary_backoff(
            self.identity,
            self.key_state.clone(),
            Duration::from_secs(retry_after_secs.max(1)),
        )
    }

    pub(crate) fn record(
        &self,
        reason: RequestRejectionReason,
        stage: &'static str,
        status: StatusCode,
        request_id: &str,
        endpoint: &str,
    ) -> bool {
        self.record_with_metadata(reason, stage, status, request_id, endpoint, None)
    }

    pub(crate) fn record_with_metadata(
        &self,
        reason: RequestRejectionReason,
        stage: &'static str,
        status: StatusCode,
        request_id: &str,
        endpoint: &str,
        extra_metadata: Option<serde_json::Value>,
    ) -> bool {
        let key_state = if reason == RequestRejectionReason::AdmissionStateCapacity {
            None
        } else {
            self.key_state
                .clone()
                .or_else(|| self.controller.state_for(self.identity).ok())
        };
        let Some(sample) = self.controller.record_attributed_rejection(
            self.identity,
            key_state.as_ref(),
            reason,
            stage,
            status,
            request_id,
            endpoint,
        ) else {
            return false;
        };
        self.recorder
            .record(sampled_request_rejection_usage_record_with_metadata(
                request_id,
                endpoint,
                Some(sample.request_api_key_id),
                reason.as_str(),
                stage,
                status,
                sample.observed_count,
                extra_metadata,
            ));
        true
    }
}

struct SampledRequestRejection {
    request_api_key_id: String,
    observed_count: u64,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RejectionLogStats {
    detailed_emitted: u64,
    summaries_emitted: u64,
    suppressed_pending: u64,
    budget_lock_acquisitions: u64,
}

impl RequestAdmissionController {
    pub(crate) fn new(config: RequestAdmissionConfig) -> Self {
        Self::with_runtime_bounds(
            config,
            DEFAULT_RPM_WINDOW,
            DEFAULT_STATE_TTL,
            DEFAULT_MAX_TRACKED_KEYS,
        )
    }

    fn with_runtime_bounds(
        config: RequestAdmissionConfig,
        rpm_window: Duration,
        state_ttl: Duration,
        max_tracked_keys: usize,
    ) -> Self {
        Self::with_runtime_bounds_and_log_policy(
            config,
            rpm_window,
            state_ttl,
            max_tracked_keys,
            RejectionLogPolicy::default(),
        )
    }

    fn with_runtime_bounds_and_log_policy(
        config: RequestAdmissionConfig,
        rpm_window: Duration,
        state_ttl: Duration,
        max_tracked_keys: usize,
        rejection_log_policy: RejectionLogPolicy,
    ) -> Self {
        let now = Instant::now();
        let rejection_log_policy = rejection_log_policy.normalized();
        let summary_interval_ms = duration_millis_u64(rejection_log_policy.summary_interval);
        let shards = (0..STATE_SHARDS)
            .map(|_| Mutex::new(HashMap::new()))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            config: RwLock::new(config.normalized()),
            shards,
            tracked_keys: AtomicUsize::new(0),
            operations: AtomicUsize::new(0),
            started_at: now,
            rpm_window: rpm_window.max(Duration::from_millis(1)),
            state_ttl,
            max_tracked_keys: max_tracked_keys.max(1),
            state_capacity_rejection_count: AtomicU64::new(0),
            rejection_log_policy,
            rejection_log_bucket: Mutex::new(RejectionLogTokenBucket::new(
                now,
                rejection_log_policy.capacity,
            )),
            rejection_log_suppressed: AtomicU64::new(0),
            rejection_log_next_summary_ms: AtomicU64::new(summary_interval_ms),
            rejection_log_detailed_emitted: AtomicU64::new(0),
            rejection_log_summaries_emitted: AtomicU64::new(0),
            rejection_log_budget_lock_acquisitions: AtomicU64::new(0),
        }
    }

    pub(crate) fn update_config(&self, config: RequestAdmissionConfig) {
        *self.config.write() = config.normalized();
        for shard in self.shards.iter() {
            for state in shard.lock().values() {
                state.notify.notify_waiters();
            }
        }
    }

    pub(crate) fn config(&self) -> RequestAdmissionConfig {
        *self.config.read()
    }

    async fn acquire(
        &self,
        identity: RequestApiKeyIdentity,
    ) -> Result<RequestAdmissionPermit, AdmissionRejection> {
        let initial_config = self.config();
        if !initial_config.enabled() {
            return Ok(RequestAdmissionPermit::disabled());
        }

        let state = self.state_for(identity)?;
        if let Some(rejection) = self.local_temporary_backoff_rejection(&state) {
            return Err(rejection);
        }
        if initial_config.rpm > 0 {
            self.reserve_rpm(&state, initial_config.rpm)?;
        }

        let counted_concurrency = if initial_config.max_concurrent_requests > 0 {
            self.acquire_concurrency(state.clone(), initial_config)
                .await?
        } else {
            false
        };
        Ok(RequestAdmissionPermit {
            state: Some(state),
            counted_concurrency,
        })
    }

    fn state_for(
        &self,
        identity: RequestApiKeyIdentity,
    ) -> Result<Arc<KeyState>, AdmissionRejection> {
        let digest = identity.digest();
        let shard_index = digest[0] as usize % self.shards.len();
        let now = Instant::now();
        let now_ms = self.monotonic_millis(now);

        let operation = self.operations.fetch_add(1, Ordering::Relaxed);
        if operation % CLEANUP_INTERVAL == 0 {
            self.cleanup_shard(
                (shard_index + operation / CLEANUP_INTERVAL) % self.shards.len(),
                now_ms,
            );
        }

        {
            let shard = self.shards[shard_index].lock();
            if let Some(state) = shard.get(&digest) {
                state.last_seen_ms.store(now_ms, Ordering::Relaxed);
                return Ok(state.clone());
            }
        }

        if !self.reserve_state_slot() {
            self.cleanup_all(now_ms);
            if !self.reserve_state_slot() {
                return Err(AdmissionRejection::state_capacity(1));
            }
        }

        let mut shard = self.shards[shard_index].lock();
        if let Some(state) = shard.get(&digest) {
            self.tracked_keys.fetch_sub(1, Ordering::AcqRel);
            state.last_seen_ms.store(now_ms, Ordering::Relaxed);
            return Ok(state.clone());
        }
        let state = Arc::new(KeyState::new(now, now_ms));
        shard.insert(digest, state.clone());
        Ok(state)
    }

    fn reserve_state_slot(&self) -> bool {
        self.tracked_keys
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < self.max_tracked_keys).then_some(current + 1)
            })
            .is_ok()
    }

    fn cleanup_all(&self, now_ms: u64) {
        for shard_index in 0..self.shards.len() {
            self.cleanup_shard(shard_index, now_ms);
        }
    }

    fn cleanup_shard(&self, shard_index: usize, now_ms: u64) {
        let ttl_ms = self.state_ttl.as_millis().min(u64::MAX as u128) as u64;
        let mut shard = self.shards[shard_index].lock();
        let before = shard.len();
        shard.retain(|_, state| {
            let idle_for = now_ms.saturating_sub(state.last_seen_ms.load(Ordering::Relaxed));
            idle_for < ttl_ms || Arc::strong_count(state) > 1 || !state.is_idle()
        });
        let removed = before.saturating_sub(shard.len());
        if removed > 0 {
            self.tracked_keys.fetch_sub(removed, Ordering::AcqRel);
        }
    }

    fn monotonic_millis(&self, now: Instant) -> u64 {
        now.duration_since(self.started_at)
            .as_millis()
            .min(u64::MAX as u128) as u64
    }

    fn local_temporary_backoff_rejection(
        &self,
        state: &Arc<KeyState>,
    ) -> Option<AdmissionRejection> {
        let now_ms = self.monotonic_millis(Instant::now());
        let until_ms = state
            .local_temporary_backoff_until_ms
            .load(Ordering::Acquire);
        if until_ms <= now_ms {
            return None;
        }
        Some(AdmissionRejection::for_key(
            AdmissionRejectionKind::LocalTemporaryBackoff,
            duration_ceil_secs(Duration::from_millis(until_ms.saturating_sub(now_ms))),
            state.clone(),
        ))
    }

    fn apply_local_temporary_backoff(
        &self,
        identity: RequestApiKeyIdentity,
        key_state: Option<Arc<KeyState>>,
        retry_after: Duration,
    ) -> bool {
        if !self.config().enabled() {
            return false;
        }
        let state = match key_state {
            Some(state) => state,
            None => match self.state_for(identity) {
                Ok(state) => state,
                Err(_) => return false,
            },
        };
        let bounded = retry_after.clamp(LOCAL_TEMPORARY_BACKOFF_MIN, LOCAL_TEMPORARY_BACKOFF_MAX);
        let now_ms = self.monotonic_millis(Instant::now());
        let until_ms = now_ms.saturating_add(duration_millis_u64(bounded));
        let mut current = state
            .local_temporary_backoff_until_ms
            .load(Ordering::Acquire);
        while until_ms > current {
            match state.local_temporary_backoff_until_ms.compare_exchange(
                current,
                until_ms,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    state.notify.notify_waiters();
                    return true;
                }
                Err(actual) => current = actual,
            }
        }
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn record_attributed_rejection(
        &self,
        identity: RequestApiKeyIdentity,
        key_state: Option<&Arc<KeyState>>,
        reason: RequestRejectionReason,
        stage: &'static str,
        status: StatusCode,
        request_id: &str,
        endpoint: &str,
    ) -> Option<SampledRequestRejection> {
        let count = match key_state {
            Some(state) => state.rejection_counts[reason.index()]
                .fetch_add(1, Ordering::Relaxed)
                .saturating_add(1),
            None => self
                .state_capacity_rejection_count
                .fetch_add(1, Ordering::Relaxed)
                .saturating_add(1),
        };
        let now = Instant::now();
        self.maybe_emit_rejection_log_summary(self.monotonic_millis(now));
        if count > 8 && !count.is_power_of_two() {
            return None;
        }

        self.rejection_log_budget_lock_acquisitions
            .fetch_add(1, Ordering::Relaxed);
        if !self
            .rejection_log_bucket
            .lock()
            .try_take(now, self.rejection_log_policy)
        {
            self.rejection_log_suppressed
                .fetch_add(1, Ordering::Relaxed);
            return None;
        }

        self.rejection_log_detailed_emitted
            .fetch_add(1, Ordering::Relaxed);
        let request_api_key_id = identity.stable_id();
        tracing::warn!(
            request_api_key_id = %request_api_key_id,
            reason = reason.as_str(),
            stage,
            status = status.as_u16(),
            rejected_count = count,
            request_id,
            endpoint,
            "authenticated inference request rejected before upstream dispatch"
        );
        Some(SampledRequestRejection {
            request_api_key_id,
            observed_count: count,
        })
    }

    fn maybe_emit_rejection_log_summary(&self, now_ms: u64) {
        if self.rejection_log_suppressed.load(Ordering::Relaxed) == 0 {
            return;
        }
        let next_summary_ms = self.rejection_log_next_summary_ms.load(Ordering::Acquire);
        if now_ms < next_summary_ms {
            return;
        }
        let next = now_ms.saturating_add(duration_millis_u64(
            self.rejection_log_policy.summary_interval,
        ));
        if self
            .rejection_log_next_summary_ms
            .compare_exchange(next_summary_ms, next, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let suppressed = self.rejection_log_suppressed.swap(0, Ordering::AcqRel);
        if suppressed == 0 {
            return;
        }
        self.rejection_log_summaries_emitted
            .fetch_add(1, Ordering::Relaxed);
        tracing::warn!(
            suppressed_count = suppressed,
            summary_interval_ms = duration_millis_u64(self.rejection_log_policy.summary_interval),
            "request API key admission rejection detail logs suppressed by global budget"
        );
    }

    fn reserve_rpm(&self, state: &Arc<KeyState>, limit: u32) -> Result<(), AdmissionRejection> {
        let now = Instant::now();
        let mut bucket = state.rpm.lock();
        let capacity = limit.min(MAX_RPM_BURST) as f64;
        if bucket.limit != limit {
            if bucket.limit == 0 {
                bucket.tokens = capacity;
            } else {
                bucket.tokens = bucket.tokens.min(capacity);
            }
            bucket.limit = limit;
            bucket.last_refill = now;
        } else {
            let elapsed = now
                .saturating_duration_since(bucket.last_refill)
                .as_secs_f64();
            let refill_per_second = limit as f64 / self.rpm_window.as_secs_f64();
            bucket.tokens = (bucket.tokens + elapsed * refill_per_second).min(capacity);
            bucket.last_refill = now;
        }
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            return Ok(());
        }

        let refill_per_second = limit as f64 / self.rpm_window.as_secs_f64();
        let wait_seconds = (1.0 - bucket.tokens).max(0.0) / refill_per_second;
        Err(AdmissionRejection::for_key(
            AdmissionRejectionKind::Rpm,
            duration_ceil_secs(Duration::from_secs_f64(wait_seconds)),
            state.clone(),
        ))
    }

    async fn acquire_concurrency(
        &self,
        state: Arc<KeyState>,
        initial_config: RequestAdmissionConfig,
    ) -> Result<bool, AdmissionRejection> {
        let queue_enabled =
            initial_config.max_queued_requests > 0 && initial_config.queue_timeout_ms > 0;
        let deadline =
            tokio::time::Instant::now() + Duration::from_millis(initial_config.queue_timeout_ms);
        let mut registration = QueueRegistration::new(state.clone());

        loop {
            let notified = state.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            let current_config = self.config();
            if current_config.max_concurrent_requests == 0 {
                let mut gate = state.gate.lock();
                registration.complete_locked(&mut gate);
                return Ok(false);
            }
            if let Some(rejection) = self.local_temporary_backoff_rejection(&state) {
                return Err(rejection);
            }
            {
                let mut gate = state.gate.lock();
                let queue_turn = registration
                    .ticket
                    .map(|ticket| ticket == gate.serving_ticket)
                    .unwrap_or(gate.queued == 0);
                if gate.active < current_config.max_concurrent_requests && queue_turn {
                    let was_queued = registration.is_registered();
                    gate.active += 1;
                    registration.complete_locked(&mut gate);
                    if was_queued {
                        state.notify.notify_waiters();
                    }
                    return Ok(true);
                }
                if registration.is_registered()
                    && current_config.max_queued_requests < initial_config.max_queued_requests
                {
                    return Err(AdmissionRejection::for_key(
                        AdmissionRejectionKind::QueueFull,
                        1,
                        state.clone(),
                    ));
                }
                if registration.is_registered()
                    && current_config.queue_timeout_ms < initial_config.queue_timeout_ms
                {
                    return Err(AdmissionRejection::for_key(
                        AdmissionRejectionKind::QueueTimeout,
                        1,
                        state.clone(),
                    ));
                }
                if !registration.is_registered() {
                    if !queue_enabled {
                        return Err(AdmissionRejection::for_key(
                            AdmissionRejectionKind::ConcurrencyFull,
                            1,
                            state.clone(),
                        ));
                    }
                    if gate.queued >= initial_config.max_queued_requests {
                        return Err(AdmissionRejection::for_key(
                            AdmissionRejectionKind::QueueFull,
                            1,
                            state.clone(),
                        ));
                    }
                    registration.register_locked(&mut gate);
                }
            }

            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return Err(AdmissionRejection::for_key(
                    AdmissionRejectionKind::QueueTimeout,
                    1,
                    state.clone(),
                ));
            }
        }
    }

    #[cfg(test)]
    fn tracked_key_count(&self) -> usize {
        self.tracked_keys.load(Ordering::Acquire)
    }

    #[cfg(test)]
    fn queued_for(&self, identity: RequestApiKeyIdentity) -> u32 {
        let digest = identity.digest();
        let shard = self.shards[digest[0] as usize % self.shards.len()].lock();
        shard
            .get(&digest)
            .map(|state| state.gate.lock().queued)
            .unwrap_or(0)
    }

    #[cfg(test)]
    fn rejection_count_for(
        &self,
        identity: RequestApiKeyIdentity,
        kind: AdmissionRejectionKind,
    ) -> u64 {
        let Some(reason) = kind.reason() else {
            return self.state_capacity_rejection_count.load(Ordering::Acquire);
        };
        let digest = identity.digest();
        let shard = self.shards[digest[0] as usize % self.shards.len()].lock();
        shard
            .get(&digest)
            .map(|state| state.rejection_counts[reason.index()].load(Ordering::Acquire))
            .unwrap_or(0)
    }

    #[cfg(test)]
    fn rejection_log_stats(&self) -> RejectionLogStats {
        RejectionLogStats {
            detailed_emitted: self.rejection_log_detailed_emitted.load(Ordering::Acquire),
            summaries_emitted: self.rejection_log_summaries_emitted.load(Ordering::Acquire),
            suppressed_pending: self.rejection_log_suppressed.load(Ordering::Acquire),
            budget_lock_acquisitions: self
                .rejection_log_budget_lock_acquisitions
                .load(Ordering::Acquire),
        }
    }
}

struct QueueRegistration {
    state: Arc<KeyState>,
    ticket: Option<u64>,
}

impl QueueRegistration {
    fn new(state: Arc<KeyState>) -> Self {
        Self {
            state,
            ticket: None,
        }
    }

    fn is_registered(&self) -> bool {
        self.ticket.is_some()
    }

    fn register_locked(&mut self, gate: &mut ConcurrencyGate) {
        debug_assert!(self.ticket.is_none());
        let ticket = gate.next_ticket;
        gate.next_ticket = gate.next_ticket.saturating_add(1);
        gate.queued = gate.queued.saturating_add(1);
        self.ticket = Some(ticket);
    }

    fn complete_locked(&mut self, gate: &mut ConcurrencyGate) {
        let Some(ticket) = self.ticket.take() else {
            return;
        };
        release_queue_ticket(gate, ticket);
    }
}

fn release_queue_ticket(gate: &mut ConcurrencyGate, ticket: u64) {
    gate.queued = gate.queued.saturating_sub(1);
    if ticket == gate.serving_ticket {
        gate.serving_ticket = gate.serving_ticket.saturating_add(1);
        while gate.cancelled_tickets.remove(&gate.serving_ticket) {
            gate.serving_ticket = gate.serving_ticket.saturating_add(1);
        }
    } else if ticket > gate.serving_ticket {
        gate.cancelled_tickets.insert(ticket);
    }
    if gate.queued == 0 {
        gate.next_ticket = 0;
        gate.serving_ticket = 0;
        gate.cancelled_tickets.clear();
    }
}

impl Drop for QueueRegistration {
    fn drop(&mut self) {
        let Some(ticket) = self.ticket.take() else {
            return;
        };
        let mut gate = self.state.gate.lock();
        release_queue_ticket(&mut gate, ticket);
        drop(gate);
        self.state.notify.notify_waiters();
    }
}

#[derive(Debug)]
struct RequestAdmissionPermit {
    state: Option<Arc<KeyState>>,
    counted_concurrency: bool,
}

impl RequestAdmissionPermit {
    fn disabled() -> Self {
        Self {
            state: None,
            counted_concurrency: false,
        }
    }

    fn holds_concurrency(&self) -> bool {
        self.counted_concurrency
    }

    fn key_state(&self) -> Option<Arc<KeyState>> {
        self.state.clone()
    }
}

impl Drop for RequestAdmissionPermit {
    fn drop(&mut self) {
        if !self.counted_concurrency {
            return;
        }
        let Some(state) = self.state.take() else {
            return;
        };
        let mut gate = state.gate.lock();
        gate.active = gate.active.saturating_sub(1);
        drop(gate);
        state.notify.notify_waiters();
    }
}

fn duration_ceil_secs(duration: Duration) -> u64 {
    duration
        .as_secs()
        .saturating_add(u64::from(duration.subsec_nanos() > 0))
        .max(1)
}

fn duration_millis_u64(duration: Duration) -> u64 {
    duration.as_millis().min(u64::MAX as u128) as u64
}

struct PermitBody {
    inner: Body,
    permit: Option<RequestAdmissionPermit>,
}

impl HttpBody for PermitBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let result = Pin::new(&mut self.inner).poll_frame(context);
        if matches!(result, Poll::Ready(None) | Poll::Ready(Some(Err(_)))) {
            self.permit.take();
        }
        result
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

fn response_holding_permit(response: Response, permit: RequestAdmissionPermit) -> Response {
    if !permit.holds_concurrency() {
        return response;
    }

    let (parts, body) = response.into_parts();
    Response::from_parts(
        parts,
        Body::new(PermitBody {
            inner: body,
            permit: Some(permit),
        }),
    )
}

/// Admission middleware for inference routes only. Authentication must be the
/// outer layer so the request carries a digest-only identity extension.
pub(crate) async fn request_admission_middleware(
    State(state): State<RequestAdmissionMiddlewareState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let Some(identity) = request.extensions().get::<RequestApiKeyIdentity>().copied() else {
        return envelope::error_response(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "Invalid API key",
        );
    };

    match state.controller.acquire(identity).await {
        Ok(permit) => {
            request
                .extensions_mut()
                .insert(RequestRejectionAttribution::new(
                    state.controller.clone(),
                    state.recorder.clone(),
                    identity,
                    permit.key_state(),
                ));
            response_holding_permit(next.run(request).await, permit)
        }
        Err(rejection) => {
            let request_id = envelope::request_id();
            let endpoint = request.uri().path().to_string();
            let reason = rejection
                .kind
                .reason()
                .unwrap_or(RequestRejectionReason::AdmissionStateCapacity);
            RequestRejectionAttribution::new(
                state.controller,
                state.recorder,
                identity,
                rejection.key_state.clone(),
            )
            .record(
                reason,
                "admission",
                StatusCode::TOO_MANY_REQUESTS,
                &request_id,
                &endpoint,
            );
            envelope::error_response_with_id_and_headers(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limit_error",
                "Request rate limit exceeded. Please retry shortly.",
                &request_id,
                [("retry-after", rejection.retry_after_secs.to_string())],
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        convert::Infallible,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use axum::{
        Router,
        body::{Body, Bytes, to_bytes},
        extract::State,
        http::{Request, StatusCode},
        middleware,
        response::Response,
        routing::{get, post},
    };
    use futures::stream;
    use serde_json::Value;
    use tokio::sync::mpsc;
    use tower::ServiceExt;

    use crate::common::auth::RequestApiKeyStore;

    use super::*;

    fn identity(key: &str) -> RequestApiKeyIdentity {
        RequestApiKeyStore::new([key]).authenticate(key).unwrap()
    }

    fn config(rpm: u32, concurrent: u32, queued: u32, timeout_ms: u64) -> RequestAdmissionConfig {
        RequestAdmissionConfig {
            rpm,
            max_concurrent_requests: concurrent,
            max_queued_requests: queued,
            queue_timeout_ms: timeout_ms,
        }
    }

    fn test_controller(config: RequestAdmissionConfig) -> Arc<RequestAdmissionController> {
        Arc::new(RequestAdmissionController::with_runtime_bounds(
            config,
            Duration::from_millis(20),
            Duration::from_millis(20),
            32,
        ))
    }

    fn test_controller_with_log_policy(
        config: RequestAdmissionConfig,
        max_tracked_keys: usize,
        capacity: u32,
        refill_per_second: f64,
        summary_interval: Duration,
    ) -> Arc<RequestAdmissionController> {
        Arc::new(
            RequestAdmissionController::with_runtime_bounds_and_log_policy(
                config,
                Duration::from_secs(60 * 60),
                Duration::from_secs(60 * 60),
                max_tracked_keys,
                RejectionLogPolicy {
                    capacity,
                    refill_per_second,
                    summary_interval,
                },
            ),
        )
    }

    #[tokio::test]
    async fn single_key_rpm_limit_is_stable_for_five_rounds() {
        for round in 0..5 {
            let controller = test_controller(config(3, 0, 0, 0));
            let key = identity(&format!("single-{round}"));
            for _ in 0..3 {
                assert!(controller.acquire(key).await.is_ok());
            }
            for _ in 0..5 {
                let rejection = controller.acquire(key).await.unwrap_err();
                assert_eq!(rejection.kind, AdmissionRejectionKind::Rpm);
                assert!(rejection.retry_after_secs >= 1);
            }
        }
    }

    #[tokio::test]
    async fn rpm_windows_roll_over_for_five_rounds() {
        let controller = test_controller(config(1, 0, 0, 0));
        let key = identity("rollover-key");
        for _ in 0..5 {
            assert!(controller.acquire(key).await.is_ok());
            assert_eq!(
                controller.acquire(key).await.unwrap_err().kind,
                AdmissionRejectionKind::Rpm
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    #[tokio::test]
    async fn local_temporary_backoff_rejects_before_rpm_for_five_rounds() {
        for round in 0..5 {
            let controller = test_controller(config(2, 0, 0, 0));
            let key = identity(&format!("local-temporary-backoff-{round}"));
            let permit = controller.acquire(key).await.unwrap();
            drop(permit);
            let state = controller.state_for(key).unwrap();
            let tokens_before = rpm_tokens(&controller, key);

            assert!(controller.apply_local_temporary_backoff(
                key,
                Some(state),
                Duration::from_secs(2),
            ));
            let rejection = controller.acquire(key).await.unwrap_err();

            assert_eq!(
                rejection.kind,
                AdmissionRejectionKind::LocalTemporaryBackoff
            );
            assert!(rejection.retry_after_secs >= 1);
            assert_eq!(rpm_tokens(&controller, key), tokens_before);
        }
    }

    #[tokio::test]
    async fn local_temporary_backoff_retry_after_is_bounded() {
        let controller = test_controller(config(100, 0, 0, 0));
        let key = identity("local-temporary-backoff-bounded");
        let state = controller.state_for(key).unwrap();

        assert!(controller.apply_local_temporary_backoff(
            key,
            Some(state),
            Duration::from_secs(60),
        ));
        let rejection = controller.acquire(key).await.unwrap_err();

        assert_eq!(
            rejection.kind,
            AdmissionRejectionKind::LocalTemporaryBackoff
        );
        assert!(
            rejection.retry_after_secs <= LOCAL_TEMPORARY_BACKOFF_MAX.as_secs(),
            "retry-after must be capped so one local scheduler failure cannot suppress a key for too long"
        );
    }

    #[tokio::test]
    async fn local_temporary_backoff_wakes_and_rejects_queued_waiters() {
        let controller = test_controller(config(0, 1, 2, 5_000));
        let key = identity("local-temporary-backoff-queue");
        let permit = controller.acquire(key).await.unwrap();
        let waiter_controller = controller.clone();
        let waiter = tokio::spawn(async move { waiter_controller.acquire(key).await });

        wait_for_queue_count(&controller, key, 1).await;
        let state = controller.state_for(key).unwrap();
        assert!(
            controller.apply_local_temporary_backoff(key, Some(state), Duration::from_secs(2),)
        );
        let rejection = tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("queued waiter must be woken by local temporary backoff")
            .expect("queued waiter task joins")
            .expect_err("queued waiter must be rejected by local temporary backoff");

        assert_eq!(
            rejection.kind,
            AdmissionRejectionKind::LocalTemporaryBackoff
        );
        assert_eq!(controller.queued_for(key), 0);
        drop(permit);
    }

    #[tokio::test]
    async fn local_temporary_backoff_is_disabled_when_admission_is_disabled() {
        let controller = test_controller(RequestAdmissionConfig::disabled());
        let key = identity("local-temporary-backoff-disabled");

        assert!(!controller.apply_local_temporary_backoff(key, None, Duration::from_secs(2)));
        assert!(controller.acquire(key).await.is_ok());
    }

    #[tokio::test]
    async fn concurrency_is_isolated_between_keys_for_five_rounds() {
        for round in 0..5 {
            let controller = test_controller(config(0, 1, 0, 0));
            let key_a = identity(&format!("key-a-{round}"));
            let key_b = identity(&format!("key-b-{round}"));
            let permit_a = controller.acquire(key_a).await.unwrap();
            let permit_b = controller.acquire(key_b).await.unwrap();
            assert_eq!(
                controller.acquire(key_a).await.unwrap_err().kind,
                AdmissionRejectionKind::ConcurrencyFull
            );
            drop(permit_a);
            assert!(controller.acquire(key_a).await.is_ok());
            drop(permit_b);
        }
    }

    async fn wait_for_queue_count(
        controller: &RequestAdmissionController,
        key: RequestApiKeyIdentity,
        expected: u32,
    ) {
        tokio::time::timeout(Duration::from_secs(1), async {
            while controller.queued_for(key) < expected {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct GateSnapshot {
        active: u32,
        queued: u32,
        next_ticket: u64,
        serving_ticket: u64,
        cancelled_tickets: usize,
    }

    fn gate_snapshot(
        controller: &RequestAdmissionController,
        key: RequestApiKeyIdentity,
    ) -> GateSnapshot {
        let digest = key.digest();
        let shard = controller.shards[digest[0] as usize % controller.shards.len()].lock();
        let state = shard.get(&digest).expect("admission key state");
        let gate = state.gate.lock();
        GateSnapshot {
            active: gate.active,
            queued: gate.queued,
            next_ticket: gate.next_ticket,
            serving_ticket: gate.serving_ticket,
            cancelled_tickets: gate.cancelled_tickets.len(),
        }
    }

    fn rpm_tokens(controller: &RequestAdmissionController, key: RequestApiKeyIdentity) -> f64 {
        let digest = key.digest();
        let shard = controller.shards[digest[0] as usize % controller.shards.len()].lock();
        let state = shard.get(&digest).expect("admission key state");
        let tokens = state.rpm.lock().tokens;
        tokens
    }

    fn idle_gate_snapshot() -> GateSnapshot {
        GateSnapshot {
            active: 0,
            queued: 0,
            next_ticket: 0,
            serving_ticket: 0,
            cancelled_tickets: 0,
        }
    }

    #[tokio::test]
    async fn queue_is_bounded_and_wakes_after_release_for_five_rounds() {
        for round in 0..5 {
            let controller = test_controller(config(0, 1, 1, 100));
            let key = identity(&format!("queue-{round}"));
            let first = controller.acquire(key).await.unwrap();
            let queued_controller = controller.clone();
            let queued = tokio::spawn(async move { queued_controller.acquire(key).await });
            wait_for_queue_count(&controller, key, 1).await;
            assert_eq!(
                controller.acquire(key).await.unwrap_err().kind,
                AdmissionRejectionKind::QueueFull
            );
            drop(first);
            assert!(queued.await.unwrap().is_ok());
        }
    }

    #[tokio::test]
    async fn queued_requests_are_fifo_and_newcomers_cannot_bypass_for_five_rounds() {
        for round in 0..5 {
            let controller = test_controller(config(0, 1, 2, 1_000));
            let key = identity(&format!("fifo-{round}"));
            let first = controller.acquire(key).await.unwrap();
            let (acquired_tx, mut acquired_rx) = mpsc::unbounded_channel();
            let first_release = Arc::new(Notify::new());
            let second_release = Arc::new(Notify::new());

            let queued_controller = controller.clone();
            let queued_tx = acquired_tx.clone();
            let queued_release = first_release.clone();
            let first_waiter = tokio::spawn(async move {
                let permit = queued_controller.acquire(key).await.unwrap();
                queued_tx.send(1u8).unwrap();
                queued_release.notified().await;
                drop(permit);
            });
            wait_for_queue_count(&controller, key, 1).await;

            let queued_controller = controller.clone();
            let queued_tx = acquired_tx.clone();
            let queued_release = second_release.clone();
            let second_waiter = tokio::spawn(async move {
                let permit = queued_controller.acquire(key).await.unwrap();
                queued_tx.send(2u8).unwrap();
                queued_release.notified().await;
                drop(permit);
            });
            wait_for_queue_count(&controller, key, 2).await;

            drop(first);
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(1), acquired_rx.recv())
                    .await
                    .unwrap(),
                Some(1)
            );
            assert!(acquired_rx.try_recv().is_err());
            first_release.notify_one();
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(1), acquired_rx.recv())
                    .await
                    .unwrap(),
                Some(2)
            );
            second_release.notify_one();
            first_waiter.await.unwrap();
            second_waiter.await.unwrap();
            assert_eq!(controller.queued_for(key), 0);
        }
    }

    #[tokio::test]
    async fn queue_timeout_and_cancellation_cleanup_are_bounded_for_five_rounds() {
        for round in 0..5 {
            let controller = test_controller(config(0, 1, 1, 5));
            let key = identity(&format!("timeout-{round}"));
            let first = controller.acquire(key).await.unwrap();
            assert_eq!(
                controller.acquire(key).await.unwrap_err().kind,
                AdmissionRejectionKind::QueueTimeout
            );
            assert_eq!(controller.queued_for(key), 0);

            let queued_controller = controller.clone();
            let queued = tokio::spawn(async move { queued_controller.acquire(key).await });
            wait_for_queue_count(&controller, key, 1).await;
            queued.abort();
            assert!(queued.await.unwrap_err().is_cancelled());
            assert_eq!(controller.queued_for(key), 0);
            drop(first);
        }
    }

    #[tokio::test]
    async fn lowering_queue_config_cancels_existing_waiters_for_five_rounds() {
        for round in 0..5 {
            let controller = test_controller(config(0, 1, 4, 1_000));
            let key = identity(&format!("queue-update-{round}"));
            let first = controller.acquire(key).await.unwrap();
            let queued_controller = controller.clone();
            let queued = tokio::spawn(async move { queued_controller.acquire(key).await });
            wait_for_queue_count(&controller, key, 1).await;

            controller.update_config(config(0, 1, 2, 500));
            let rejection = tokio::time::timeout(Duration::from_millis(50), queued)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err();
            assert!(matches!(
                rejection.kind,
                AdmissionRejectionKind::QueueFull | AdmissionRejectionKind::QueueTimeout
            ));
            assert_eq!(controller.queued_for(key), 0);
            drop(first);
        }
    }

    #[tokio::test]
    async fn disabled_controller_creates_no_key_state_for_five_rounds() {
        for _ in 0..5 {
            let controller = test_controller(RequestAdmissionConfig::disabled());
            for index in 0..100 {
                assert!(
                    controller
                        .acquire(identity(&format!("disabled-{index}")))
                        .await
                        .is_ok()
                );
            }
            assert_eq!(controller.tracked_key_count(), 0);
        }
    }

    #[tokio::test]
    async fn key_churn_state_is_evicted_and_hard_bounded_for_five_rounds() {
        for round in 0..5 {
            let controller = Arc::new(RequestAdmissionController::with_runtime_bounds(
                config(1, 0, 0, 0),
                Duration::from_secs(1),
                Duration::from_secs(60),
                8,
            ));
            for index in 0..8 {
                controller
                    .acquire(identity(&format!("old-{round}-{index}")))
                    .await
                    .unwrap();
            }
            assert_eq!(controller.tracked_key_count(), 8);
            assert_eq!(
                controller
                    .acquire(identity(&format!("overflow-{round}")))
                    .await
                    .unwrap_err()
                    .kind,
                AdmissionRejectionKind::StateCapacity
            );
            controller.cleanup_all(u64::MAX);
            for index in 0..8 {
                controller
                    .acquire(identity(&format!("new-{round}-{index}")))
                    .await
                    .unwrap();
                assert!(controller.tracked_key_count() <= 8);
            }
        }
    }

    #[derive(Clone, Copy)]
    enum BodyMode {
        Complete,
        Pending,
        Error,
    }

    #[derive(Clone)]
    struct MockState {
        hits: Arc<AtomicUsize>,
        body_mode: BodyMode,
    }

    async fn counted_handler(State(state): State<MockState>) -> Response {
        state.hits.fetch_add(1, Ordering::SeqCst);
        let body = match state.body_mode {
            BodyMode::Complete => Body::from("ok"),
            BodyMode::Pending => Body::from_stream(stream::pending::<Result<Bytes, Infallible>>()),
            BodyMode::Error => Body::from_stream(stream::once(async {
                Err::<Bytes, _>(std::io::Error::other("synthetic body error"))
            })),
        };
        Response::new(body)
    }

    fn middleware_router(
        controller: Arc<RequestAdmissionController>,
        hits: Arc<AtomicUsize>,
        body_mode: BodyMode,
    ) -> Router {
        let state = MockState { hits, body_mode };
        let middleware_state =
            RequestAdmissionMiddlewareState::new(controller, Arc::new(UsageRecorder::new(1_024)));
        Router::new()
            .route(
                "/messages",
                post(counted_handler).layer(middleware::from_fn_with_state(
                    middleware_state,
                    request_admission_middleware,
                )),
            )
            .route("/models", get(counted_handler))
            .route("/messages/count_tokens", post(counted_handler))
            .with_state(state)
    }

    fn request(path: &str, identity: RequestApiKeyIdentity) -> Request<Body> {
        let mut request = Request::builder()
            .method(if path == "/models" { "GET" } else { "POST" })
            .uri(path)
            .body(Body::empty())
            .unwrap();
        request.extensions_mut().insert(identity);
        request
    }

    #[tokio::test]
    async fn combined_rpm_fifo_cancel_timeout_and_recovery_are_isolated_for_five_rounds() {
        for round in 0..5 {
            let controller = Arc::new(RequestAdmissionController::with_runtime_bounds(
                config(4, 1, 2, 250),
                Duration::from_secs(60 * 60),
                Duration::from_secs(60 * 60),
                8,
            ));
            let hits = Arc::new(AtomicUsize::new(0));
            let app = middleware_router(controller.clone(), hits.clone(), BodyMode::Pending);
            let noisy = identity(&format!("combined-noisy-{round}"));
            let quiet = identity(&format!("combined-quiet-{round}"));

            let holder = app
                .clone()
                .oneshot(request("/messages", noisy))
                .await
                .unwrap();
            assert_eq!(holder.status(), StatusCode::OK);
            assert_eq!(hits.load(Ordering::SeqCst), 1);
            assert_eq!(
                gate_snapshot(&controller, noisy),
                GateSnapshot {
                    active: 1,
                    ..idle_gate_snapshot()
                }
            );

            let quiet_response = app
                .clone()
                .oneshot(request("/messages", quiet))
                .await
                .unwrap();
            assert_eq!(quiet_response.status(), StatusCode::OK);
            assert_eq!(hits.load(Ordering::SeqCst), 2);
            drop(quiet_response);
            assert_eq!(gate_snapshot(&controller, quiet), idle_gate_snapshot());

            let first_waiter = tokio::spawn({
                let app = app.clone();
                async move { app.oneshot(request("/messages", noisy)).await.unwrap() }
            });
            wait_for_queue_count(&controller, noisy, 1).await;

            let second_waiter = tokio::spawn({
                let app = app.clone();
                async move { app.oneshot(request("/messages", noisy)).await.unwrap() }
            });
            wait_for_queue_count(&controller, noisy, 2).await;
            assert_eq!(
                gate_snapshot(&controller, noisy),
                GateSnapshot {
                    active: 1,
                    queued: 2,
                    next_ticket: 2,
                    serving_ticket: 0,
                    cancelled_tickets: 0,
                }
            );

            second_waiter.abort();
            assert!(second_waiter.await.unwrap_err().is_cancelled());
            assert_eq!(
                gate_snapshot(&controller, noisy),
                GateSnapshot {
                    active: 1,
                    queued: 1,
                    next_ticket: 2,
                    serving_ticket: 0,
                    cancelled_tickets: 1,
                }
            );

            let timed_out = tokio::time::timeout(Duration::from_secs(1), first_waiter)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(timed_out.status(), StatusCode::TOO_MANY_REQUESTS);
            assert_eq!(hits.load(Ordering::SeqCst), 2);
            assert_eq!(
                controller.rejection_count_for(noisy, AdmissionRejectionKind::QueueTimeout),
                1
            );
            assert_eq!(
                gate_snapshot(&controller, noisy),
                GateSnapshot {
                    active: 1,
                    ..idle_gate_snapshot()
                }
            );

            drop(holder);
            assert_eq!(gate_snapshot(&controller, noisy), idle_gate_snapshot());

            let recovered = app
                .clone()
                .oneshot(request("/messages", noisy))
                .await
                .unwrap();
            assert_eq!(recovered.status(), StatusCode::OK);
            assert_eq!(hits.load(Ordering::SeqCst), 3);
            drop(recovered);
            assert_eq!(gate_snapshot(&controller, noisy), idle_gate_snapshot());

            let rpm_rejected = app
                .clone()
                .oneshot(request("/messages", noisy))
                .await
                .unwrap();
            assert_eq!(rpm_rejected.status(), StatusCode::TOO_MANY_REQUESTS);
            assert_eq!(hits.load(Ordering::SeqCst), 3);
            assert_eq!(
                controller.rejection_count_for(noisy, AdmissionRejectionKind::Rpm),
                1
            );
            assert!(
                rpm_tokens(&controller, noisy) < 1.0,
                "timed-out and cancelled authenticated attempts must not refund RPM"
            );

            let quiet_recovered = app
                .clone()
                .oneshot(request("/messages", quiet))
                .await
                .unwrap();
            assert_eq!(quiet_recovered.status(), StatusCode::OK);
            assert_eq!(hits.load(Ordering::SeqCst), 4);
            drop(quiet_recovered);
            assert_eq!(gate_snapshot(&controller, quiet), idle_gate_snapshot());
            assert_eq!(controller.tracked_key_count(), 2);
        }
    }

    #[tokio::test]
    async fn rejection_micropressure_is_bounded_and_power_of_two_sampled_for_five_rounds() {
        for rejection_volume in [10_000_u64, 100_000] {
            for round in 0..5 {
                let controller = Arc::new(RequestAdmissionController::with_runtime_bounds(
                    config(1, 0, 0, 0),
                    Duration::from_secs(60 * 60),
                    Duration::from_secs(60 * 60),
                    4,
                ));
                let noisy = identity(&format!("pressure-noisy-{rejection_volume}-{round}"));
                let quiet = identity(&format!("pressure-quiet-{rejection_volume}-{round}"));
                let recorder = Arc::new(UsageRecorder::new(128));
                let initial = controller.acquire(noisy).await.unwrap();
                let noisy_attribution = RequestRejectionAttribution::new(
                    controller.clone(),
                    recorder.clone(),
                    noisy,
                    initial.key_state(),
                );
                drop(initial);

                let started_at = Instant::now();
                let mut sampled = 0_u64;
                for index in 0..rejection_volume {
                    let rejection = controller.acquire(noisy).await.unwrap_err();
                    assert_eq!(rejection.kind, AdmissionRejectionKind::Rpm);
                    sampled += u64::from(noisy_attribution.record(
                        RequestRejectionReason::AdmissionRpm,
                        "admission",
                        StatusCode::TOO_MANY_REQUESTS,
                        &format!("req_pressure_noisy_{index}"),
                        "/messages",
                    ));
                }
                let elapsed = started_at.elapsed();
                let expected_samples =
                    u64::from(u64::BITS.saturating_sub(rejection_volume.leading_zeros())) + 4;
                assert_eq!(sampled, expected_samples);
                assert_eq!(
                    controller.rejection_count_for(noisy, AdmissionRejectionKind::Rpm),
                    rejection_volume
                );

                let initial = controller.acquire(quiet).await.unwrap();
                let quiet_attribution = RequestRejectionAttribution::new(
                    controller.clone(),
                    recorder.clone(),
                    quiet,
                    initial.key_state(),
                );
                drop(initial);
                let quiet_rejection = controller.acquire(quiet).await.unwrap_err();
                assert_eq!(quiet_rejection.kind, AdmissionRejectionKind::Rpm);
                let quiet_first_rejection_sampled = quiet_attribution.record(
                    RequestRejectionReason::AdmissionRpm,
                    "admission",
                    StatusCode::TOO_MANY_REQUESTS,
                    "req_pressure_quiet_first",
                    "/messages",
                );
                assert!(
                    quiet_first_rejection_sampled,
                    "a noisy key must not hide a quiet key's first rejection"
                );
                assert_eq!(
                    controller.rejection_count_for(quiet, AdmissionRejectionKind::Rpm),
                    1
                );

                assert_eq!(controller.tracked_key_count(), 2);
                assert_eq!(gate_snapshot(&controller, noisy), idle_gate_snapshot());
                assert_eq!(gate_snapshot(&controller, quiet), idle_gate_snapshot());
                assert!(elapsed < Duration::from_secs(30));
                let log_stats = controller.rejection_log_stats();
                assert_eq!(log_stats.detailed_emitted, expected_samples + 1);
                assert_eq!(log_stats.summaries_emitted, 0);
                assert_eq!(log_stats.suppressed_pending, 0);
                assert_eq!(log_stats.budget_lock_acquisitions, expected_samples + 1);
                let records = recorder
                    .query(super::super::usage::UsageRecordQuery::default())
                    .records;
                assert_eq!(records.len() as u64, expected_samples + 1);
                assert!(records.iter().all(|record| {
                    record.total_input_tokens == 0
                        && record.output_tokens == 0
                        && record.error_source.as_deref() == Some("request_rejection")
                }));
                eprintln!(
                    "request-admission-rejection-pressure volume={rejection_volume} round={} elapsed_ms={} tracked_keys={} detailed_logs={} budget_locks={} quiet_first_sampled={quiet_first_rejection_sampled}",
                    round + 1,
                    elapsed.as_millis(),
                    controller.tracked_key_count(),
                    log_stats.detailed_emitted,
                    log_stats.budget_lock_acquisitions,
                );
            }
        }
    }

    #[test]
    fn rejection_sampling_isolated_by_key_and_reason_for_five_rounds() {
        const KEY_REASONS: [AdmissionRejectionKind; 4] = [
            AdmissionRejectionKind::Rpm,
            AdmissionRejectionKind::ConcurrencyFull,
            AdmissionRejectionKind::QueueFull,
            AdmissionRejectionKind::QueueTimeout,
        ];

        for round in 0..5 {
            let controller = test_controller_with_log_policy(
                config(1, 1, 1, 100),
                8,
                32,
                0.0,
                Duration::from_secs(60),
            );
            let noisy = identity(&format!("reason-noisy-{round}"));
            let quiet = identity(&format!("reason-quiet-{round}"));
            let recorder = Arc::new(UsageRecorder::new(32));
            let noisy_state = controller.state_for(noisy).unwrap();
            let quiet_state = controller.state_for(quiet).unwrap();

            for (key, state) in [(noisy, noisy_state), (quiet, quiet_state)] {
                let attribution = RequestRejectionAttribution::new(
                    controller.clone(),
                    recorder.clone(),
                    key,
                    Some(state.clone()),
                );
                for kind in KEY_REASONS {
                    let rejection = AdmissionRejection::for_key(kind, 1, state.clone());
                    assert_eq!(rejection.kind, kind);
                    assert!(attribution.record(
                        kind.reason().unwrap(),
                        "admission",
                        StatusCode::TOO_MANY_REQUESTS,
                        &format!(
                            "req_multi_{}_{}",
                            &key.stable_id()[..8],
                            kind.reason().unwrap().as_str()
                        ),
                        "/messages",
                    ));
                    assert_eq!(controller.rejection_count_for(key, kind), 1);
                }
            }

            let overflow_attribution =
                RequestRejectionAttribution::new(controller.clone(), recorder.clone(), noisy, None);
            for index in 0..2 {
                let rejection = AdmissionRejection::state_capacity(1);
                assert_eq!(rejection.kind, AdmissionRejectionKind::StateCapacity);
                assert!(overflow_attribution.record(
                    RequestRejectionReason::AdmissionStateCapacity,
                    "admission",
                    StatusCode::TOO_MANY_REQUESTS,
                    &format!("req_state_capacity_{index}"),
                    "/messages",
                ));
            }
            assert_eq!(
                controller.rejection_count_for(noisy, AdmissionRejectionKind::StateCapacity),
                2
            );
            assert_eq!(
                controller.rejection_count_for(quiet, AdmissionRejectionKind::StateCapacity),
                2
            );
            assert_eq!(
                controller.rejection_log_stats(),
                RejectionLogStats {
                    detailed_emitted: 10,
                    summaries_emitted: 0,
                    suppressed_pending: 0,
                    budget_lock_acquisitions: 10,
                }
            );
            assert_eq!(controller.tracked_key_count(), 2);
            assert_eq!(
                recorder
                    .query(super::super::usage::UsageRecordQuery::default())
                    .records
                    .len(),
                10
            );
        }
    }

    #[test]
    fn request_rejection_reason_count_covers_every_index() {
        const ALL_REASONS: [RequestRejectionReason; REQUEST_REJECTION_REASON_COUNT] = [
            RequestRejectionReason::AdmissionRpm,
            RequestRejectionReason::AdmissionConcurrencyFull,
            RequestRejectionReason::AdmissionQueueFull,
            RequestRejectionReason::AdmissionQueueTimeout,
            RequestRejectionReason::AdmissionStateCapacity,
            RequestRejectionReason::BodyTooLarge,
            RequestRejectionReason::BodyReadFailed,
            RequestRejectionReason::RequestEntryInvalid,
            RequestRejectionReason::StrictRequestProtocolContamination,
            RequestRejectionReason::DfcacheRouteInvalid,
            RequestRejectionReason::ProviderNotReady,
            RequestRejectionReason::MultimodalInvalid,
            RequestRejectionReason::ModelUnsupported,
            RequestRejectionReason::WebSearchUnsupported,
            RequestRejectionReason::LocalBodyPrepare,
            RequestRejectionReason::LocalPoolUnavailable,
            RequestRejectionReason::LocalPoolTemporaryUnavailable,
            RequestRejectionReason::AdmissionLocalTemporaryBackoff,
        ];

        for (expected_index, reason) in ALL_REASONS.iter().copied().enumerate() {
            assert_eq!(reason.index(), expected_index);
            assert!(reason.index() < REQUEST_REJECTION_REASON_COUNT);
            assert!(!reason.as_str().is_empty());
        }
    }

    #[tokio::test]
    async fn high_cardinality_rejection_logs_are_globally_bounded_for_five_rounds() {
        const LOG_CAPACITY: u64 = 8;

        for round in 0..5 {
            let controller = test_controller_with_log_policy(
                config(1, 0, 0, 0),
                DEFAULT_MAX_TRACKED_KEYS,
                LOG_CAPACITY as u32,
                0.0,
                Duration::from_millis(5),
            );
            let started_at = Instant::now();
            let recorder = Arc::new(UsageRecorder::new(32));
            let mut detailed = 0_u64;
            let mut first_attribution = None;
            for key_index in 0..DEFAULT_MAX_TRACKED_KEYS {
                let key = identity(&format!("cardinality-{round}-{key_index}"));
                let permit = controller.acquire(key).await.unwrap();
                let attribution = RequestRejectionAttribution::new(
                    controller.clone(),
                    recorder.clone(),
                    key,
                    permit.key_state(),
                );
                drop(permit);
                if key_index == 0 {
                    first_attribution = Some(attribution.clone());
                }
                let rejection = controller.acquire(key).await.unwrap_err();
                assert_eq!(rejection.kind, AdmissionRejectionKind::Rpm);
                detailed += u64::from(attribution.record(
                    RequestRejectionReason::AdmissionRpm,
                    "admission",
                    StatusCode::TOO_MANY_REQUESTS,
                    &format!("req_cardinality_{key_index}"),
                    "/messages",
                ));
            }
            assert_eq!(detailed, LOG_CAPACITY);
            assert_eq!(controller.tracked_key_count(), DEFAULT_MAX_TRACKED_KEYS);

            let overflow = identity(&format!("cardinality-overflow-{round}"));
            let state_capacity = controller.acquire(overflow).await.unwrap_err();
            assert_eq!(state_capacity.kind, AdmissionRejectionKind::StateCapacity);
            let overflow_attribution = RequestRejectionAttribution::new(
                controller.clone(),
                recorder.clone(),
                overflow,
                None,
            );
            assert!(!overflow_attribution.record(
                RequestRejectionReason::AdmissionStateCapacity,
                "admission",
                StatusCode::TOO_MANY_REQUESTS,
                "req_cardinality_overflow",
                "/messages",
            ));
            assert_eq!(controller.tracked_key_count(), DEFAULT_MAX_TRACKED_KEYS);

            tokio::time::sleep(Duration::from_millis(6)).await;
            let first_key = identity(&format!("cardinality-{round}-0"));
            let second_rejection = controller.acquire(first_key).await.unwrap_err();
            assert_eq!(second_rejection.kind, AdmissionRejectionKind::Rpm);
            assert!(!first_attribution.unwrap().record(
                RequestRejectionReason::AdmissionRpm,
                "admission",
                StatusCode::TOO_MANY_REQUESTS,
                "req_cardinality_second",
                "/messages",
            ));

            let stats = controller.rejection_log_stats();
            let elapsed = started_at.elapsed();
            let max_summaries = duration_millis_u64(elapsed) / 5 + 1;
            assert_eq!(stats.detailed_emitted, LOG_CAPACITY);
            assert!(stats.summaries_emitted >= 1);
            assert!(stats.summaries_emitted <= max_summaries);
            assert_eq!(stats.suppressed_pending, 1);
            assert_eq!(
                recorder
                    .query(super::super::usage::UsageRecordQuery::default())
                    .records
                    .len(),
                LOG_CAPACITY as usize
            );
            assert_eq!(
                stats.budget_lock_acquisitions,
                DEFAULT_MAX_TRACKED_KEYS as u64 + 2
            );
            assert_eq!(
                controller.rejection_count_for(first_key, AdmissionRejectionKind::Rpm),
                2
            );
            assert_eq!(
                controller.rejection_count_for(overflow, AdmissionRejectionKind::StateCapacity),
                1
            );
            assert_eq!(gate_snapshot(&controller, first_key), idle_gate_snapshot());
            assert!(elapsed < Duration::from_secs(30));
            eprintln!(
                "request-admission-high-cardinality round={} keys={} elapsed_ms={} detailed_logs={} summaries={} suppressed_pending={} budget_locks={}",
                round + 1,
                controller.tracked_key_count(),
                elapsed.as_millis(),
                stats.detailed_emitted,
                stats.summaries_emitted,
                stats.suppressed_pending,
                stats.budget_lock_acquisitions,
            );
        }
    }

    #[tokio::test]
    async fn rejected_requests_are_anthropic_429_and_never_hit_handler_for_five_rounds() {
        for round in 0..5 {
            let hits = Arc::new(AtomicUsize::new(0));
            let controller = test_controller(config(1, 0, 0, 0));
            let app = middleware_router(controller.clone(), hits.clone(), BodyMode::Complete);
            let key = identity(&format!("reject-{round}"));
            let accepted = app
                .clone()
                .oneshot(request("/messages", key))
                .await
                .unwrap();
            assert_eq!(accepted.status(), StatusCode::OK);

            let rejected = app
                .clone()
                .oneshot(request("/messages", key))
                .await
                .unwrap();
            assert_eq!(rejected.status(), StatusCode::TOO_MANY_REQUESTS);
            assert!(rejected.headers().contains_key("retry-after"));
            assert!(rejected.headers().contains_key("request-id"));
            assert!(rejected.headers().contains_key("anthropic-request-id"));
            let response_request_id = rejected.headers()["request-id"]
                .to_str()
                .unwrap()
                .to_string();
            let body = to_bytes(rejected.into_body(), 64 * 1024).await.unwrap();
            let json: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["error"]["type"], "rate_limit_error");
            assert_eq!(json["request_id"], response_request_id);
            assert_eq!(hits.load(Ordering::SeqCst), 1);
            assert_eq!(
                controller.rejection_count_for(key, AdmissionRejectionKind::Rpm),
                1
            );
        }
    }

    #[tokio::test]
    async fn token_bucket_caps_boundary_bursts_for_five_rounds() {
        for round in 0..5 {
            // Use the production one-minute window here. The short 20 ms test
            // window is reserved for refill tests and can legitimately refill
            // while a busy parallel test runner issues the initial burst.
            let controller = Arc::new(RequestAdmissionController::new(config(300, 0, 0, 0)));
            let key = identity(&format!("burst-{round}"));
            for _ in 0..MAX_RPM_BURST {
                assert!(controller.acquire(key).await.is_ok());
            }
            assert_eq!(
                controller.acquire(key).await.unwrap_err().kind,
                AdmissionRejectionKind::Rpm
            );
        }
    }

    #[tokio::test]
    async fn permit_lives_until_body_eof_error_or_drop_for_five_rounds() {
        for round in 0..5 {
            let key = identity(&format!("body-{round}"));

            let hits = Arc::new(AtomicUsize::new(0));
            let complete = middleware_router(
                test_controller(config(0, 1, 0, 0)),
                hits,
                BodyMode::Complete,
            );
            let first = complete
                .clone()
                .oneshot(request("/messages", key))
                .await
                .unwrap();
            assert_eq!(
                complete
                    .clone()
                    .oneshot(request("/messages", key))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::TOO_MANY_REQUESTS
            );
            assert_eq!(to_bytes(first.into_body(), 1024).await.unwrap(), "ok");
            assert_eq!(
                complete
                    .clone()
                    .oneshot(request("/messages", key))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::OK
            );

            let pending = middleware_router(
                test_controller(config(0, 1, 0, 0)),
                Arc::new(AtomicUsize::new(0)),
                BodyMode::Pending,
            );
            let first = pending
                .clone()
                .oneshot(request("/messages", key))
                .await
                .unwrap();
            assert_eq!(
                pending
                    .clone()
                    .oneshot(request("/messages", key))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::TOO_MANY_REQUESTS
            );
            drop(first);
            assert_eq!(
                pending
                    .clone()
                    .oneshot(request("/messages", key))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::OK
            );

            let error = middleware_router(
                test_controller(config(0, 1, 0, 0)),
                Arc::new(AtomicUsize::new(0)),
                BodyMode::Error,
            );
            let first = error
                .clone()
                .oneshot(request("/messages", key))
                .await
                .unwrap();
            assert_eq!(
                error
                    .clone()
                    .oneshot(request("/messages", key))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::TOO_MANY_REQUESTS
            );
            assert!(to_bytes(first.into_body(), 1024).await.is_err());
            assert_eq!(
                error
                    .clone()
                    .oneshot(request("/messages", key))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::OK
            );
        }
    }

    #[tokio::test]
    async fn models_and_count_tokens_do_not_consume_message_admission_for_five_rounds() {
        for round in 0..5 {
            let hits = Arc::new(AtomicUsize::new(0));
            let app = middleware_router(
                test_controller(config(1, 1, 0, 0)),
                hits.clone(),
                BodyMode::Complete,
            );
            let key = identity(&format!("bypass-{round}"));
            for _ in 0..5 {
                assert_eq!(
                    app.clone()
                        .oneshot(request("/models", key))
                        .await
                        .unwrap()
                        .status(),
                    StatusCode::OK
                );
                assert_eq!(
                    app.clone()
                        .oneshot(request("/messages/count_tokens", key))
                        .await
                        .unwrap()
                        .status(),
                    StatusCode::OK
                );
            }
            assert_eq!(hits.load(Ordering::SeqCst), 10);
        }
    }
}
