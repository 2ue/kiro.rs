use chrono::Utc;
use futures::future::join_all;
use parking_lot::Mutex;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration as StdDuration, Instant};

use crate::common::capacity_signal::CapacitySignal;
use crate::storage::redis_cache::{RedisStore, SchedulerCredentialState};

use super::account_state::CredentialEntry;
use super::storage_task::spawn_best_effort_storage_task;
use super::types::InFlightKind;

const RELEASED_IN_FLIGHT_LEASE_TOMBSTONE_TTL: StdDuration = StdDuration::from_secs(15 * 60);
const RELEASED_IN_FLIGHT_LEASE_TOMBSTONE_PRUNE_THRESHOLD: usize = 4096;
const RELEASED_IN_FLIGHT_LEASE_TOMBSTONE_HARD_LIMIT: usize = 200_000;
const REDIS_CRITICAL_OPERATION_TIMEOUT: StdDuration = StdDuration::from_secs(2);
const REDIS_CRITICAL_RETRY_DELAY: StdDuration = StdDuration::from_millis(50);
const DISPATCH_QUEUE_LEASE_RENEW_INTERVAL_MAX_SECS: u64 = 20;
const SCHEDULER_REDIS_RELEASE_CAPACITY: usize = 65_536;
const SCHEDULER_REDIS_RELEASE_BATCH_SIZE: usize = 16;
const SCHEDULER_REDIS_RELEASE_RETRY_MAX_DELAY: StdDuration = StdDuration::from_secs(30);
const SCHEDULER_REDIS_LEASE_TOUCH_INTERVAL_MAX: StdDuration = StdDuration::from_secs(30);
const SCHEDULER_REDIS_LEASE_TOUCH_INTERVAL_MIN: StdDuration = StdDuration::from_millis(250);

pub(super) type ReleasedInFlightLeaseTombstones = Arc<Mutex<HashMap<(u64, u64), Instant>>>;

pub(super) async fn release_redis_in_flight_lease_and_wakeup(
    redis: Arc<RedisStore>,
    source_instance_id: Arc<str>,
    credential_id: u64,
    lease_id: u64,
    tombstone: bool,
    attempts: usize,
) -> anyhow::Result<()> {
    let payload = serde_json::json!({
        "kind": "dispatch_wakeup",
        "credentialId": credential_id,
        "leaseId": lease_id,
        "sourceInstanceId": source_instance_id,
        "changedAt": Utc::now().to_rfc3339(),
    })
    .to_string();
    let attempts = attempts.max(1);
    let mut last_error = None;
    for attempt in 0..attempts {
        match tokio::time::timeout(
            REDIS_CRITICAL_OPERATION_TIMEOUT,
            redis.release_in_flight_lease_and_publish_wakeup(
                credential_id,
                lease_id,
                tombstone,
                &payload,
            ),
        )
        .await
        {
            Ok(Ok(_)) => return Ok(()),
            Ok(Err(err)) => last_error = Some(err),
            Err(_) => {
                last_error = Some(anyhow::anyhow!(
                    "Redis lease release timed out after {}ms",
                    REDIS_CRITICAL_OPERATION_TIMEOUT.as_millis()
                ));
            }
        }
        if attempt + 1 < attempts {
            tokio::time::sleep(REDIS_CRITICAL_RETRY_DELAY).await;
        }
    }
    Err(last_error.expect("at least one Redis release attempt must run"))
}

async fn release_redis_dispatch_queue_lease_with_retry(
    redis: Arc<RedisStore>,
    lease_id: String,
    attempts: usize,
) -> anyhow::Result<()> {
    let attempts = attempts.max(1);
    let mut last_error = None;
    for attempt in 0..attempts {
        match tokio::time::timeout(
            REDIS_CRITICAL_OPERATION_TIMEOUT,
            redis.leave_dispatch_queue(&lease_id),
        )
        .await
        {
            Ok(Ok(_)) => return Ok(()),
            Ok(Err(err)) => last_error = Some(err),
            Err(_) => {
                last_error = Some(anyhow::anyhow!(
                    "Redis dispatch queue release timed out after {}ms",
                    REDIS_CRITICAL_OPERATION_TIMEOUT.as_millis()
                ));
            }
        }
        if attempt + 1 < attempts {
            tokio::time::sleep(REDIS_CRITICAL_RETRY_DELAY).await;
        }
    }
    Err(last_error.expect("at least one Redis queue release attempt must run"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RedisLeaseAcquisitionState {
    NotStarted,
    CommitUnknown,
    Definitive { acquired: bool },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum SchedulerRedisReleaseIntent {
    InFlight {
        credential_id: u64,
        lease_id: u64,
        tombstone: bool,
    },
    DispatchQueue {
        lease_id: String,
    },
}

#[derive(Default)]
struct SchedulerRedisReleaseDispatcherState {
    pending: HashMap<SchedulerRedisReleaseIntent, SchedulerRedisPendingRelease>,
    order: VecDeque<SchedulerRedisReleaseIntent>,
    system_failures: u32,
    system_retry_at: Option<Instant>,
    worker_running: bool,
}

struct SchedulerRedisPendingRelease {
    _permit: OwnedSemaphorePermit,
    failures: u32,
    next_attempt_at: Instant,
}

impl SchedulerRedisReleaseDispatcherState {
    fn next_ready_batch(
        &mut self,
        now: Instant,
        limit: usize,
    ) -> (Vec<SchedulerRedisReleaseIntent>, Option<Instant>) {
        if let Some(retry_at) = self.system_retry_at {
            if retry_at > now {
                return (Vec::new(), Some(retry_at));
            }
            self.system_retry_at = None;
        }
        let candidates = self.order.len();
        let mut batch = Vec::with_capacity(limit);
        let mut next_attempt_at: Option<Instant> = None;
        for _ in 0..candidates {
            if batch.len() >= limit {
                break;
            }
            let Some(intent) = self.order.pop_front() else {
                break;
            };
            if let Some(next_attempt) = self
                .pending
                .get(&intent)
                .map(|pending| pending.next_attempt_at)
            {
                self.order.push_back(intent.clone());
                if next_attempt <= now {
                    batch.push(intent);
                } else {
                    next_attempt_at = Some(
                        next_attempt_at
                            .map(|next| next.min(next_attempt))
                            .unwrap_or(next_attempt),
                    );
                }
            }
        }
        (batch, next_attempt_at)
    }
}

#[derive(Debug, Default)]
struct SchedulerRedisReleaseDispatcherStats {
    enqueued: AtomicU64,
    completed: AtomicU64,
    retries: AtomicU64,
    worker_starts: AtomicU64,
    spawn_failures: AtomicU64,
    saturated: AtomicU64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct SchedulerRedisReleaseDispatcherSnapshot {
    pub(super) pending: usize,
    pub(super) capacity_available: usize,
    pub(super) enqueued: u64,
    pub(super) completed: u64,
    pub(super) retries: u64,
    pub(super) worker_starts: u64,
    pub(super) spawn_failures: u64,
    pub(super) saturated: u64,
}

pub(super) struct SchedulerRedisReleaseReservation(Option<OwnedSemaphorePermit>);

impl SchedulerRedisReleaseReservation {
    fn take(&mut self) -> OwnedSemaphorePermit {
        self.0
            .take()
            .expect("scheduler Redis release reservation may only be consumed once")
    }
}

pub(super) struct SchedulerRedisReleaseDispatcher {
    redis: Arc<RedisStore>,
    source_instance_id: Arc<str>,
    capacity: Arc<Semaphore>,
    drained_notify: Notify,
    work_notify: Notify,
    state: Mutex<SchedulerRedisReleaseDispatcherState>,
    stats: SchedulerRedisReleaseDispatcherStats,
}

impl SchedulerRedisReleaseDispatcher {
    pub(super) fn new(redis: Arc<RedisStore>, source_instance_id: Arc<str>) -> Arc<Self> {
        Arc::new(Self {
            redis,
            source_instance_id,
            capacity: Arc::new(Semaphore::new(SCHEDULER_REDIS_RELEASE_CAPACITY)),
            drained_notify: Notify::new(),
            work_notify: Notify::new(),
            state: Mutex::new(SchedulerRedisReleaseDispatcherState::default()),
            stats: SchedulerRedisReleaseDispatcherStats::default(),
        })
    }

    pub(super) fn try_reserve(&self) -> Option<SchedulerRedisReleaseReservation> {
        match self.capacity.clone().try_acquire_owned() {
            Ok(permit) => Some(SchedulerRedisReleaseReservation(Some(permit))),
            Err(_) => {
                let saturated = self.stats.saturated.fetch_add(1, Ordering::Relaxed) + 1;
                if saturated == 1 || saturated.is_power_of_two() {
                    tracing::warn!(
                        saturated,
                        capacity = SCHEDULER_REDIS_RELEASE_CAPACITY,
                        pending = self.pending_len(),
                        "Redis scheduler release reconciliation 容量已满，拒绝新的分布式 lease"
                    );
                }
                None
            }
        }
    }

    fn enqueue(
        self: &Arc<Self>,
        intent: SchedulerRedisReleaseIntent,
        mut reservation: SchedulerRedisReleaseReservation,
    ) {
        let should_start = {
            let mut state = self.state.lock();
            if !state.pending.contains_key(&intent) {
                state.order.push_back(intent.clone());
                state.pending.insert(
                    intent,
                    SchedulerRedisPendingRelease {
                        _permit: reservation.take(),
                        failures: 0,
                        next_attempt_at: Instant::now(),
                    },
                );
                self.stats.enqueued.fetch_add(1, Ordering::Relaxed);
            }
            if state.worker_running {
                false
            } else {
                state.worker_running = true;
                true
            }
        };
        if !should_start {
            self.work_notify.notify_one();
            return;
        }

        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            self.state.lock().worker_running = false;
            let failures = self.stats.spawn_failures.fetch_add(1, Ordering::Relaxed) + 1;
            tracing::error!(
                failures,
                pending = self.pending_len(),
                "Redis scheduler release worker 无可用 Tokio runtime，保留 intent 等待后续唤醒"
            );
            return;
        };
        self.stats.worker_starts.fetch_add(1, Ordering::Relaxed);
        let dispatcher = self.clone();
        handle.spawn(async move {
            dispatcher.run().await;
        });
    }

    async fn run(self: Arc<Self>) {
        loop {
            let work_available = self.work_notify.notified();
            tokio::pin!(work_available);
            work_available.as_mut().enable();
            let (batch, next_attempt_at) = {
                let mut state = self.state.lock();
                if state.pending.is_empty() {
                    state.order.clear();
                    state.worker_running = false;
                    self.drained_notify.notify_waiters();
                    return;
                }
                state.next_ready_batch(Instant::now(), SCHEDULER_REDIS_RELEASE_BATCH_SIZE)
            };
            if batch.is_empty() {
                match next_attempt_at {
                    Some(next_attempt_at) => {
                        tokio::select! {
                            _ = tokio::time::sleep_until(next_attempt_at.into()) => {}
                            _ = work_available.as_mut() => {}
                        }
                    }
                    None => tokio::task::yield_now().await,
                }
                continue;
            }
            let results = join_all(batch.iter().cloned().map(|intent| {
                let redis = self.redis.clone();
                let source_instance_id = self.source_instance_id.clone();
                async move {
                    let result = match &intent {
                        SchedulerRedisReleaseIntent::InFlight {
                            credential_id,
                            lease_id,
                            tombstone,
                        } => {
                            release_redis_in_flight_lease_and_wakeup(
                                redis,
                                source_instance_id,
                                *credential_id,
                                *lease_id,
                                *tombstone,
                                1,
                            )
                            .await
                        }
                        SchedulerRedisReleaseIntent::DispatchQueue { lease_id } => {
                            release_redis_dispatch_queue_lease_with_retry(
                                redis,
                                lease_id.clone(),
                                1,
                            )
                            .await
                        }
                    };
                    (intent, result)
                }
            }))
            .await;

            let mut completed = 0usize;
            let mut failed = 0usize;
            let mut system_failed = false;
            {
                let mut state = self.state.lock();
                for (intent, result) in results {
                    match result {
                        Ok(()) if state.pending.remove(&intent).is_some() => {
                            completed += 1;
                        }
                        Ok(()) => {}
                        Err(error) => {
                            failed += 1;
                            if scheduler_release_error_is_intent_scoped(&error) {
                                if let Some(pending) = state.pending.get_mut(&intent) {
                                    pending.failures = pending.failures.saturating_add(1);
                                    pending.next_attempt_at = Instant::now()
                                        + scheduler_release_retry_delay(pending.failures);
                                }
                            } else {
                                system_failed = true;
                            }
                        }
                    }
                }
                if system_failed {
                    state.system_failures = state.system_failures.saturating_add(1);
                    state.system_retry_at =
                        Some(Instant::now() + scheduler_release_retry_delay(state.system_failures));
                } else if completed > 0 {
                    state.system_failures = 0;
                    state.system_retry_at = None;
                }
            }
            if completed > 0 {
                self.stats
                    .completed
                    .fetch_add(completed as u64, Ordering::Relaxed);
            }
            if failed == 0 {
                tokio::task::yield_now().await;
                continue;
            }

            let retries = self.stats.retries.fetch_add(1, Ordering::Relaxed) + 1;
            if retries == 1 || retries.is_power_of_two() {
                tracing::warn!(
                    retries,
                    pending = self.pending_len(),
                    batch_size = batch.len(),
                    completed,
                    failed,
                    "Redis scheduler release reconciliation 未完整完成，保留 intent 并重试"
                );
            }
        }
    }

    fn pending_len(&self) -> usize {
        self.state.lock().pending.len()
    }

    pub(super) fn snapshot(&self) -> SchedulerRedisReleaseDispatcherSnapshot {
        SchedulerRedisReleaseDispatcherSnapshot {
            pending: self.pending_len(),
            capacity_available: self.capacity.available_permits(),
            enqueued: self.stats.enqueued.load(Ordering::Relaxed),
            completed: self.stats.completed.load(Ordering::Relaxed),
            retries: self.stats.retries.load(Ordering::Relaxed),
            worker_starts: self.stats.worker_starts.load(Ordering::Relaxed),
            spawn_failures: self.stats.spawn_failures.load(Ordering::Relaxed),
            saturated: self.stats.saturated.load(Ordering::Relaxed),
        }
    }

    pub(super) async fn drain(&self, timeout: StdDuration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let notified = self.drained_notify.notified();
            {
                let state = self.state.lock();
                if state.pending.is_empty() && !state.worker_running {
                    return true;
                }
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                let state = self.state.lock();
                return state.pending.is_empty() && !state.worker_running;
            }
        }
    }
}

fn stable_release_retry_jitter(base: StdDuration, attempt: u64) -> StdDuration {
    let base_millis = base.as_millis().max(1);
    let spread = (base_millis / 10).max(1);
    let mixed = attempt
        .wrapping_add(0x9e37_79b9_7f4a_7c15)
        .wrapping_mul(0xbf58_476d_1ce4_e5b9)
        ^ attempt.rotate_left(27);
    let reduction = mixed as u128 % spread.saturating_add(1);
    StdDuration::from_millis(base_millis.saturating_sub(reduction).min(u64::MAX as u128) as u64)
}

fn scheduler_release_retry_delay(failures: u32) -> StdDuration {
    let multiplier = 1u32
        .checked_shl(failures.saturating_sub(1).min(16))
        .unwrap_or(u32::MAX);
    let base = REDIS_CRITICAL_RETRY_DELAY
        .saturating_mul(multiplier)
        .min(SCHEDULER_REDIS_RELEASE_RETRY_MAX_DELAY);
    stable_release_retry_jitter(base, u64::from(failures))
}

fn scheduler_release_error_is_intent_scoped(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.to_string().contains("WRONGTYPE"))
}

fn scheduler_redis_lease_touch_interval(max_age: Option<StdDuration>) -> StdDuration {
    max_age
        .and_then(|max_age| max_age.checked_div(3))
        .unwrap_or(SCHEDULER_REDIS_LEASE_TOUCH_INTERVAL_MAX)
        .clamp(
            SCHEDULER_REDIS_LEASE_TOUCH_INTERVAL_MIN,
            SCHEDULER_REDIS_LEASE_TOUCH_INTERVAL_MAX,
        )
}

fn reserve_periodic_work(
    next_at_millis: &AtomicU64,
    now_millis: u64,
    interval_millis: u64,
) -> bool {
    let interval_millis = interval_millis.max(1);
    loop {
        let next = next_at_millis.load(Ordering::Acquire);
        if now_millis < next {
            return false;
        }
        let following = now_millis.saturating_add(interval_millis);
        if next_at_millis
            .compare_exchange(next, following, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return true;
        }
    }
}

struct RedisTouchInFlightReset(Arc<AtomicBool>);

impl Drop for RedisTouchInFlightReset {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn prune_released_in_flight_lease_tombstones_locked(
    tombstones: &mut HashMap<(u64, u64), Instant>,
    now: Instant,
) {
    tombstones.retain(|_, released_at| {
        now.saturating_duration_since(*released_at) <= RELEASED_IN_FLIGHT_LEASE_TOMBSTONE_TTL
    });
    if tombstones.len() > RELEASED_IN_FLIGHT_LEASE_TOMBSTONE_HARD_LIMIT {
        tombstones.clear();
        tracing::warn!(
            hard_limit = RELEASED_IN_FLIGHT_LEASE_TOMBSTONE_HARD_LIMIT,
            "本地已释放并发 lease tombstone 超过硬上限，已清空以避免内存增长"
        );
    }
}

pub(super) fn record_released_in_flight_lease_tombstone(
    tombstones: &Mutex<HashMap<(u64, u64), Instant>>,
    credential_id: u64,
    lease_id: u64,
) {
    let now = Instant::now();
    let mut tombstones = tombstones.lock();
    tombstones.insert((credential_id, lease_id), now);
    if tombstones.len() >= RELEASED_IN_FLIGHT_LEASE_TOMBSTONE_PRUNE_THRESHOLD {
        prune_released_in_flight_lease_tombstones_locked(&mut tombstones, now);
    }
}

pub(super) fn filter_released_in_flight_leases_from_scheduler_states(
    tombstones: &Mutex<HashMap<(u64, u64), Instant>>,
    states: &mut HashMap<u64, SchedulerCredentialState>,
) {
    if states.is_empty() {
        return;
    }
    let now = Instant::now();
    let mut tombstones = tombstones.lock();
    prune_released_in_flight_lease_tombstones_locked(&mut tombstones, now);
    if tombstones.is_empty() {
        return;
    }
    for (credential_id, state) in states.iter_mut() {
        state
            .in_flight_leases
            .retain(|lease| !tombstones.contains_key(&(*credential_id, lease.id)));
    }
}

pub struct InFlightLeaseGuard {
    entries: Arc<Mutex<Vec<CredentialEntry>>>,
    redis_store: Option<Arc<RedisStore>>,
    release_dispatcher: Option<Arc<SchedulerRedisReleaseDispatcher>>,
    release_reservation: Mutex<Option<SchedulerRedisReleaseReservation>>,
    released_lease_tombstones: Option<ReleasedInFlightLeaseTombstones>,
    capacity_signal: Arc<CapacitySignal>,
    credential_id: u64,
    lease_id: u64,
    weight_units: u32,
    redis_acquisition_state: RedisLeaseAcquisitionState,
    redis_touch_epoch: Instant,
    redis_touch_interval_millis: AtomicU64,
    next_redis_touch_at_millis: AtomicU64,
    redis_touch_in_flight: Arc<AtomicBool>,
    released: AtomicBool,
}

impl fmt::Debug for InFlightLeaseGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InFlightLeaseGuard")
            .field("credential_id", &self.credential_id)
            .field("lease_id", &self.lease_id)
            .field("weight_units", &self.weight_units)
            .finish_non_exhaustive()
    }
}

impl InFlightLeaseGuard {
    pub(super) fn new(
        entries: Arc<Mutex<Vec<CredentialEntry>>>,
        redis_store: Option<Arc<RedisStore>>,
        release_dispatcher: Option<Arc<SchedulerRedisReleaseDispatcher>>,
        release_reservation: Option<SchedulerRedisReleaseReservation>,
        released_lease_tombstones: Option<ReleasedInFlightLeaseTombstones>,
        capacity_signal: Arc<CapacitySignal>,
        credential_id: u64,
        lease_id: u64,
        weight_units: u32,
    ) -> Self {
        let redis_touch_interval = scheduler_redis_lease_touch_interval(None);
        let redis_touch_interval_millis =
            redis_touch_interval.as_millis().min(u64::MAX as u128) as u64;
        Self {
            entries,
            redis_store,
            release_dispatcher,
            release_reservation: Mutex::new(release_reservation),
            released_lease_tombstones,
            capacity_signal,
            credential_id,
            lease_id,
            weight_units: weight_units.max(1),
            redis_acquisition_state: RedisLeaseAcquisitionState::NotStarted,
            redis_touch_epoch: Instant::now(),
            redis_touch_interval_millis: AtomicU64::new(redis_touch_interval_millis),
            next_redis_touch_at_millis: AtomicU64::new(redis_touch_interval_millis),
            redis_touch_in_flight: Arc::new(AtomicBool::new(false)),
            released: AtomicBool::new(false),
        }
    }

    pub(super) fn credential_id(&self) -> u64 {
        self.credential_id
    }

    pub(super) fn id(&self) -> u64 {
        self.lease_id
    }

    /// Redis 调用即将开始；取消或超时必须按 commit-unknown tombstone 对账。
    pub(super) fn arm_redis_commit_unknown(&mut self) {
        debug_assert_eq!(
            self.redis_acquisition_state,
            RedisLeaseAcquisitionState::NotStarted
        );
        self.redis_acquisition_state = RedisLeaseAcquisitionState::CommitUnknown;
    }

    /// Redis 已确认创建 lease；后续释放使用普通删除语义。
    pub(super) fn confirm_redis_acquired(&mut self) {
        debug_assert_eq!(
            self.redis_acquisition_state,
            RedisLeaseAcquisitionState::CommitUnknown
        );
        self.redis_acquisition_state = RedisLeaseAcquisitionState::Definitive { acquired: true };
    }

    pub(super) fn configure_redis_touch_interval(&self, max_age: Option<StdDuration>) {
        let interval = scheduler_redis_lease_touch_interval(max_age);
        let interval_millis = interval.as_millis().min(u64::MAX as u128) as u64;
        self.redis_touch_interval_millis
            .store(interval_millis, Ordering::Release);
        self.next_redis_touch_at_millis
            .store(interval_millis, Ordering::Release);
    }

    /// Redis 未开始或已明确拒绝创建 lease；只回滚本地 provisional reservation。
    pub(super) fn confirm_redis_not_acquired(&mut self) {
        self.redis_acquisition_state = RedisLeaseAcquisitionState::Definitive { acquired: false };
        self.release_reservation.lock().take();
        self.released_lease_tombstones = None;
    }

    pub fn release(&self) {
        if self.released.swap(true, Ordering::AcqRel) {
            return;
        }
        let release_kind = match self.redis_acquisition_state {
            RedisLeaseAcquisitionState::NotStarted
            | RedisLeaseAcquisitionState::Definitive { acquired: false } => None,
            RedisLeaseAcquisitionState::CommitUnknown => Some(true),
            RedisLeaseAcquisitionState::Definitive { acquired: true } => Some(false),
        };
        if release_kind.is_some() {
            if let Some(tombstones) = &self.released_lease_tombstones {
                record_released_in_flight_lease_tombstone(
                    tombstones,
                    self.credential_id,
                    self.lease_id,
                );
            }
        }
        let released_weight =
            release_in_flight_lease_from_entries(&self.entries, self.credential_id, self.lease_id);
        if let Some(released_weight) = released_weight {
            self.capacity_signal
                .capacity_released(released_weight as usize);
        }
        if let (Some(tombstone), Some(dispatcher), Some(reservation)) = (
            release_kind,
            &self.release_dispatcher,
            self.release_reservation.lock().take(),
        ) {
            dispatcher.enqueue(
                SchedulerRedisReleaseIntent::InFlight {
                    credential_id: self.credential_id,
                    lease_id: self.lease_id,
                    tombstone,
                },
                reservation,
            );
        }
    }

    pub fn touch(&self) {
        touch_in_flight_lease_from_entries(&self.entries, self.credential_id, self.lease_id);
        let Some(redis) = &self.redis_store else {
            return;
        };
        let elapsed_millis = self
            .redis_touch_epoch
            .elapsed()
            .as_millis()
            .min(u64::MAX as u128) as u64;
        let interval_millis = self.redis_touch_interval_millis.load(Ordering::Acquire);
        if !reserve_periodic_work(
            &self.next_redis_touch_at_millis,
            elapsed_millis,
            interval_millis,
        ) || self
            .redis_touch_in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        let redis = redis.clone();
        let credential_id = self.credential_id;
        let lease_id = self.lease_id;
        let in_flight = self.redis_touch_in_flight.clone();
        let reset = RedisTouchInFlightReset(in_flight);
        spawn_best_effort_storage_task("更新 Redis 并发 lease 活跃时间", async move {
            let _reset = reset;
            redis.touch_in_flight_lease(credential_id, lease_id).await
        });
    }

    pub fn set_kind(&self, kind: InFlightKind) {
        set_in_flight_lease_kind_from_entries(
            &self.entries,
            self.credential_id,
            self.lease_id,
            kind,
        );
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            let credential_id = self.credential_id;
            let lease_id = self.lease_id;
            let kind_name = kind.as_str().to_string();
            spawn_best_effort_storage_task("更新 Redis 并发 lease 类型", async move {
                redis
                    .set_in_flight_lease_kind(credential_id, lease_id, &kind_name)
                    .await
            });
        }
    }
}

impl Drop for InFlightLeaseGuard {
    fn drop(&mut self) {
        self.release();
    }
}

pub(super) struct DispatchQueueGuard {
    redis_store: Option<Arc<RedisStore>>,
    release_dispatcher: Option<Arc<SchedulerRedisReleaseDispatcher>>,
    release_reservation: Mutex<Option<SchedulerRedisReleaseReservation>>,
    local_queued: Arc<AtomicU32>,
    redis_lease_id: Option<String>,
    redis_acquisition_state: RedisLeaseAcquisitionState,
    redis_lease_ttl_secs: u64,
    redis_renewal_required: bool,
    next_redis_renew_at: Option<Instant>,
    released: AtomicBool,
}

impl DispatchQueueGuard {
    pub(super) fn new(
        redis_store: Option<Arc<RedisStore>>,
        release_dispatcher: Option<Arc<SchedulerRedisReleaseDispatcher>>,
        release_reservation: Option<SchedulerRedisReleaseReservation>,
        local_queued: Arc<AtomicU32>,
        redis_lease_id: Option<String>,
        redis_lease_ttl_secs: u64,
        redis_renewal_required: bool,
    ) -> Self {
        Self {
            redis_store,
            release_dispatcher,
            release_reservation: Mutex::new(release_reservation),
            local_queued,
            redis_lease_id,
            redis_acquisition_state: RedisLeaseAcquisitionState::NotStarted,
            redis_lease_ttl_secs,
            redis_renewal_required,
            next_redis_renew_at: None,
            released: AtomicBool::new(false),
        }
    }

    pub(super) fn arm_redis_commit_unknown(&mut self) {
        debug_assert_eq!(
            self.redis_acquisition_state,
            RedisLeaseAcquisitionState::NotStarted
        );
        self.redis_acquisition_state = RedisLeaseAcquisitionState::CommitUnknown;
    }

    pub(super) fn confirm_redis_acquired(&mut self) {
        debug_assert_eq!(
            self.redis_acquisition_state,
            RedisLeaseAcquisitionState::CommitUnknown
        );
        self.redis_acquisition_state = RedisLeaseAcquisitionState::Definitive { acquired: true };
        self.next_redis_renew_at = self.redis_renewal_required.then(|| {
            let renew_interval_secs = (self.redis_lease_ttl_secs / 3)
                .clamp(1, DISPATCH_QUEUE_LEASE_RENEW_INTERVAL_MAX_SECS);
            Instant::now() + StdDuration::from_secs(renew_interval_secs)
        });
    }

    pub(super) fn confirm_redis_not_acquired(&mut self) {
        self.redis_acquisition_state = RedisLeaseAcquisitionState::Definitive { acquired: false };
        self.release_reservation.lock().take();
        self.next_redis_renew_at = None;
    }

    pub(super) fn redis_renewal_is_due(&self) -> bool {
        !self.released.load(Ordering::Acquire)
            && self
                .next_redis_renew_at
                .is_some_and(|renew_at| Instant::now() >= renew_at)
    }

    pub(super) fn has_redis_lease(&self) -> bool {
        self.redis_lease_id.is_some()
    }

    #[cfg(test)]
    pub(super) fn redis_renewal_is_armed(&self) -> bool {
        self.next_redis_renew_at.is_some()
    }

    pub(super) async fn renew_if_needed(&mut self) -> anyhow::Result<()> {
        if self.released.load(Ordering::Acquire)
            || self
                .next_redis_renew_at
                .is_none_or(|renew_at| Instant::now() < renew_at)
        {
            return Ok(());
        }
        let (Some(redis), Some(lease_id)) = (&self.redis_store, &self.redis_lease_id) else {
            self.next_redis_renew_at = None;
            return Ok(());
        };
        let renewed = tokio::time::timeout(
            REDIS_CRITICAL_OPERATION_TIMEOUT,
            redis.renew_dispatch_queue(lease_id, self.redis_lease_ttl_secs),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "Redis dispatch queue renewal timed out after {}ms",
                REDIS_CRITICAL_OPERATION_TIMEOUT.as_millis()
            )
        })??;
        if !renewed {
            anyhow::bail!("Redis dispatch queue lease expired before renewal");
        }
        let renew_interval_secs =
            (self.redis_lease_ttl_secs / 3).clamp(1, DISPATCH_QUEUE_LEASE_RENEW_INTERVAL_MAX_SECS);
        self.next_redis_renew_at =
            Some(Instant::now() + StdDuration::from_secs(renew_interval_secs));
        Ok(())
    }

    fn release(&self) {
        if self.released.swap(true, Ordering::AcqRel) {
            return;
        }
        self.local_queued.fetch_sub(1, Ordering::AcqRel);
        let should_release = matches!(
            self.redis_acquisition_state,
            RedisLeaseAcquisitionState::CommitUnknown
                | RedisLeaseAcquisitionState::Definitive { acquired: true }
        );
        if should_release {
            if let (Some(dispatcher), Some(lease_id), Some(reservation)) = (
                &self.release_dispatcher,
                &self.redis_lease_id,
                self.release_reservation.lock().take(),
            ) {
                dispatcher.enqueue(
                    SchedulerRedisReleaseIntent::DispatchQueue {
                        lease_id: lease_id.clone(),
                    },
                    reservation,
                );
            }
        }
    }
}

impl Drop for DispatchQueueGuard {
    fn drop(&mut self) {
        self.release();
    }
}

fn release_in_flight_lease_from_entries(
    entries: &Mutex<Vec<CredentialEntry>>,
    credential_id: u64,
    lease_id: u64,
) -> Option<u32> {
    let mut entries = entries.lock();
    let Some(entry) = entries.iter_mut().find(|e| e.id == credential_id) else {
        return None;
    };

    if let Some(index) = entry
        .in_flight_leases
        .iter()
        .position(|lease| lease.id == lease_id)
    {
        let lease = entry.in_flight_leases.remove(index);
        let weight_units = lease.weight_units.max(1);
        if entry.in_flight_requests >= weight_units {
            entry.in_flight_requests -= weight_units;
        } else {
            tracing::debug!(
                weight_units,
                in_flight_requests = entry.in_flight_requests,
                "凭据 #{} 并发 lease #{} 释放时计数小于权重，已归零",
                credential_id,
                lease_id
            );
            entry.in_flight_requests = 0;
        }
        Some(weight_units)
    } else {
        None
    }
}

fn touch_in_flight_lease_from_entries(
    entries: &Mutex<Vec<CredentialEntry>>,
    credential_id: u64,
    lease_id: u64,
) {
    let mut entries = entries.lock();
    if let Some(entry) = entries.iter_mut().find(|e| e.id == credential_id) {
        if let Some(lease) = entry
            .in_flight_leases
            .iter_mut()
            .find(|lease| lease.id == lease_id)
        {
            lease.last_seen_at = Instant::now();
        }
    }
}

fn set_in_flight_lease_kind_from_entries(
    entries: &Mutex<Vec<CredentialEntry>>,
    credential_id: u64,
    lease_id: u64,
    kind: InFlightKind,
) {
    let mut entries = entries.lock();
    if let Some(entry) = entries.iter_mut().find(|e| e.id == credential_id) {
        if let Some(lease) = entry
            .in_flight_leases
            .iter_mut()
            .find(|lease| lease.id == lease_id)
        {
            lease.kind = kind;
            lease.last_seen_at = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classification_guard(tombstones: ReleasedInFlightLeaseTombstones) -> InFlightLeaseGuard {
        InFlightLeaseGuard::new(
            Arc::new(Mutex::new(Vec::new())),
            None,
            None,
            None,
            Some(tombstones),
            Arc::new(CapacitySignal::default()),
            7,
            11,
            1,
        )
    }

    #[test]
    fn dispatch_queue_guard_releases_local_count_only_once() {
        let queued = Arc::new(AtomicU32::new(1));
        let guard = DispatchQueueGuard::new(None, None, None, queued.clone(), None, 60, false);

        guard.release();
        guard.release();
        drop(guard);

        assert_eq!(queued.load(Ordering::Acquire), 0);
    }

    #[test]
    fn finite_dispatch_queue_guards_do_not_arm_redis_renewal_for_500_waiters() {
        let queued = Arc::new(AtomicU32::new(500));
        let mut guards = Vec::with_capacity(500);

        for index in 0..500 {
            let mut guard = DispatchQueueGuard::new(
                None,
                None,
                None,
                queued.clone(),
                Some(format!("finite-{index}")),
                180,
                false,
            );
            guard.arm_redis_commit_unknown();
            guard.confirm_redis_acquired();
            assert!(!guard.redis_renewal_is_armed());
            assert!(!guard.redis_renewal_is_due());
            guards.push(guard);
        }

        drop(guards);
        assert_eq!(queued.load(Ordering::Acquire), 0);
    }

    #[test]
    fn unlimited_dispatch_queue_guard_keeps_redis_renewal_armed() {
        let queued = Arc::new(AtomicU32::new(1));
        let mut guard = DispatchQueueGuard::new(
            None,
            None,
            None,
            queued.clone(),
            Some("unlimited".to_string()),
            60,
            true,
        );
        guard.arm_redis_commit_unknown();
        guard.confirm_redis_acquired();

        assert!(guard.redis_renewal_is_armed());
        assert!(!guard.redis_renewal_is_due());
        drop(guard);
        assert_eq!(queued.load(Ordering::Acquire), 0);
    }

    #[test]
    fn redis_acquire_failure_cleanup_distinguishes_not_started_commit_unknown_and_definitive() {
        let not_started_tombstones = Arc::new(Mutex::new(HashMap::new()));
        classification_guard(not_started_tombstones.clone()).release();
        assert!(not_started_tombstones.lock().is_empty());

        let commit_unknown_tombstones = Arc::new(Mutex::new(HashMap::new()));
        let mut commit_unknown = classification_guard(commit_unknown_tombstones.clone());
        commit_unknown.arm_redis_commit_unknown();
        commit_unknown.release();
        assert!(commit_unknown_tombstones.lock().contains_key(&(7, 11)));

        let definitive_rejection_tombstones = Arc::new(Mutex::new(HashMap::new()));
        let mut definitive_rejection =
            classification_guard(definitive_rejection_tombstones.clone());
        definitive_rejection.arm_redis_commit_unknown();
        definitive_rejection.confirm_redis_not_acquired();
        definitive_rejection.release();
        assert!(definitive_rejection_tombstones.lock().is_empty());
    }

    #[test]
    fn release_retry_jitter_is_stable_and_bounded() {
        for attempt in 1..=512 {
            let base = StdDuration::from_millis((attempt.min(20) * 50) as u64);
            let first = stable_release_retry_jitter(base, attempt);
            let second = stable_release_retry_jitter(base, attempt);
            assert_eq!(first, second);
            assert!(first <= base);
            assert!(first >= base.saturating_mul(9) / 10);
        }
    }

    #[test]
    fn scheduler_redis_touch_interval_tracks_lease_ttl_without_chunk_coupling() {
        assert_eq!(
            scheduler_redis_lease_touch_interval(None),
            StdDuration::from_secs(30)
        );
        assert_eq!(
            scheduler_redis_lease_touch_interval(Some(StdDuration::from_secs(900))),
            StdDuration::from_secs(30)
        );
        assert_eq!(
            scheduler_redis_lease_touch_interval(Some(StdDuration::from_secs(1))),
            StdDuration::from_secs(1).checked_div(3).unwrap()
        );
    }

    #[test]
    fn periodic_reservation_is_constant_for_many_chunks_in_one_interval() {
        let next = AtomicU64::new(100);
        for now in 0..100 {
            assert!(!reserve_periodic_work(&next, now, 100));
        }
        assert!(reserve_periodic_work(&next, 100, 100));
        for _ in 0..10_000 {
            assert!(!reserve_periodic_work(&next, 100, 100));
        }
        assert_eq!(next.load(Ordering::Acquire), 200);
        assert!(reserve_periodic_work(&next, 10_000, 100));
        assert_eq!(next.load(Ordering::Acquire), 10_100);
    }

    #[test]
    fn delayed_poison_release_intents_cannot_hide_ready_work_for_five_rounds() {
        for round in 0..5u64 {
            let semaphore = Arc::new(Semaphore::new(128));
            let mut state = SchedulerRedisReleaseDispatcherState::default();
            let now = Instant::now();
            for lease_id in 0..64u64 {
                let intent = SchedulerRedisReleaseIntent::InFlight {
                    credential_id: round + 1,
                    lease_id,
                    tombstone: false,
                };
                state.order.push_back(intent.clone());
                state.pending.insert(
                    intent,
                    SchedulerRedisPendingRelease {
                        _permit: semaphore.clone().try_acquire_owned().unwrap(),
                        failures: 20,
                        next_attempt_at: now + StdDuration::from_secs(30),
                    },
                );
            }
            let healthy = SchedulerRedisReleaseIntent::InFlight {
                credential_id: round + 100,
                lease_id: 10_000 + round,
                tombstone: false,
            };
            state.order.push_back(healthy.clone());
            state.pending.insert(
                healthy.clone(),
                SchedulerRedisPendingRelease {
                    _permit: semaphore.clone().try_acquire_owned().unwrap(),
                    failures: 0,
                    next_attempt_at: now,
                },
            );

            let (batch, next_attempt_at) =
                state.next_ready_batch(now, SCHEDULER_REDIS_RELEASE_BATCH_SIZE);
            assert_eq!(batch, vec![healthy], "round {round}");
            assert_eq!(next_attempt_at, Some(now + StdDuration::from_secs(30)));
        }
    }

    #[test]
    fn transport_backoff_blocks_redis_calls_without_discarding_new_work() {
        let semaphore = Arc::new(Semaphore::new(1));
        let now = Instant::now();
        let retry_at = now + StdDuration::from_secs(30);
        let intent = SchedulerRedisReleaseIntent::DispatchQueue {
            lease_id: "queued-during-outage".to_string(),
        };
        let mut state = SchedulerRedisReleaseDispatcherState {
            system_failures: 10,
            system_retry_at: Some(retry_at),
            ..Default::default()
        };
        state.order.push_back(intent.clone());
        state.pending.insert(
            intent,
            SchedulerRedisPendingRelease {
                _permit: semaphore.try_acquire_owned().unwrap(),
                failures: 0,
                next_attempt_at: now,
            },
        );
        assert_eq!(
            state.next_ready_batch(now, SCHEDULER_REDIS_RELEASE_BATCH_SIZE),
            (Vec::new(), Some(retry_at))
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn real_redis_wrongtype_poison_does_not_block_new_local_release_for_five_rounds() {
        let Some(url) = crate::storage::integration_test_url("KIRO_RS_TEST_REDIS_URL") else {
            eprintln!("跳过 Redis local release poison 测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        for round in 0..5u64 {
            let mut config = crate::model::config::Config::default();
            config.redis.url = Some(url.clone());
            config.redis.key_prefix = format!("kiro_rs:test:{}", uuid::Uuid::new_v4());
            let redis = Arc::new(RedisStore::connect(&config).await.unwrap());
            let poison_id = 10_000 + round;
            redis
                .set_scheduler_release_wrongtype_for_test(poison_id, true)
                .await
                .unwrap();
            let dispatcher = SchedulerRedisReleaseDispatcher::new(
                redis.clone(),
                Arc::from(format!("local-poison-{round}")),
            );
            let poison_reservation = dispatcher.try_reserve().unwrap();
            dispatcher.enqueue(
                SchedulerRedisReleaseIntent::InFlight {
                    credential_id: poison_id,
                    lease_id: 1,
                    tombstone: false,
                },
                poison_reservation,
            );
            tokio::time::timeout(StdDuration::from_secs(1), async {
                while dispatcher.snapshot().retries == 0 {
                    tokio::time::sleep(StdDuration::from_millis(5)).await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("round {round}: poison intent did not enter retry"));

            let completed_before = dispatcher.snapshot().completed;
            let healthy_reservation = dispatcher.try_reserve().unwrap();
            dispatcher.enqueue(
                SchedulerRedisReleaseIntent::InFlight {
                    credential_id: poison_id + 100,
                    lease_id: 2,
                    tombstone: false,
                },
                healthy_reservation,
            );
            tokio::time::timeout(StdDuration::from_millis(500), async {
                while dispatcher.snapshot().completed == completed_before {
                    tokio::time::sleep(StdDuration::from_millis(5)).await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("round {round}: healthy release was blocked by poison"));
            assert_eq!(dispatcher.snapshot().pending, 1, "round {round}");

            redis
                .set_scheduler_release_wrongtype_for_test(poison_id, false)
                .await
                .unwrap();
            assert!(
                dispatcher.drain(StdDuration::from_secs(2)).await,
                "round {round}"
            );
        }
    }
}
