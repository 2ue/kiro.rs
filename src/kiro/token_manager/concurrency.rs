use chrono::Utc;
use parking_lot::Mutex;
use tokio::sync::Notify;

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::time::{Duration as StdDuration, Instant};

use crate::storage::redis_cache::{
    RedisStore, SCHEDULER_DISTRIBUTED_LEASE_SAFETY_SECS, SchedulerCredentialState,
};

use super::account_state::CredentialEntry;
use super::route_state::CachedLocalPoolRouteState;
use super::storage_task::{
    block_on_storage, spawn_best_effort_storage_task, spawn_critical_storage_task,
};
use super::types::InFlightKind;

const RELEASED_IN_FLIGHT_LEASE_TOMBSTONE_TTL: StdDuration =
    StdDuration::from_secs(SCHEDULER_DISTRIBUTED_LEASE_SAFETY_SECS);
const RELEASED_IN_FLIGHT_LEASE_TOMBSTONE_PRUNE_THRESHOLD: usize = 4096;
const RELEASED_IN_FLIGHT_LEASE_TOMBSTONE_HARD_LIMIT: usize = 200_000;
const REDIS_CRITICAL_OPERATION_TIMEOUT: StdDuration = StdDuration::from_secs(2);
const REDIS_CRITICAL_RETRY_DELAY: StdDuration = StdDuration::from_millis(50);
const REDIS_IN_FLIGHT_TOUCH_INTERVAL_MIN: StdDuration = StdDuration::from_millis(100);
const REDIS_IN_FLIGHT_TOUCH_INTERVAL_MAX: StdDuration = StdDuration::from_secs(30);
const DISPATCH_QUEUE_LEASE_RENEW_INTERVAL_MAX_SECS: u64 = 20;
const IN_FLIGHT_LEASE_PENDING_DISPATCH: u8 = 0;
const IN_FLIGHT_LEASE_DISPATCHED: u8 = 1;
const IN_FLIGHT_LEASE_RELEASED: u8 = 2;

pub(super) type ReleasedInFlightLeaseTombstones = Arc<Mutex<HashMap<(u64, u64), Instant>>>;

pub(super) fn distributed_in_flight_lease_max_age(configured_secs: u64) -> StdDuration {
    StdDuration::from_secs(if configured_secs > 0 {
        configured_secs
    } else {
        SCHEDULER_DISTRIBUTED_LEASE_SAFETY_SECS
    })
}

pub(super) struct RedisAdmissionCleanupGuard {
    redis: Arc<RedisStore>,
    released_lease_tombstones: ReleasedInFlightLeaseTombstones,
    credential_id: u64,
    lease_id: u64,
    armed: bool,
}

impl RedisAdmissionCleanupGuard {
    pub(super) fn new(
        redis: Arc<RedisStore>,
        released_lease_tombstones: ReleasedInFlightLeaseTombstones,
        credential_id: u64,
        lease_id: u64,
    ) -> Self {
        Self {
            redis,
            released_lease_tombstones,
            credential_id,
            lease_id,
            armed: true,
        }
    }

    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for RedisAdmissionCleanupGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        record_released_in_flight_lease_tombstone(
            &self.released_lease_tombstones,
            self.credential_id,
            self.lease_id,
        );
        schedule_uncertain_redis_admission_cleanup(
            self.redis.clone(),
            self.credential_id,
            self.lease_id,
        );
    }
}

fn schedule_uncertain_redis_admission_cleanup(
    redis: Arc<RedisStore>,
    credential_id: u64,
    lease_id: u64,
) {
    let fallback_redis = redis.clone();
    let admitted = spawn_critical_storage_task("清理结果不确定的 Redis 凭据准入", async move {
        release_redis_in_flight_lease_and_wakeup(redis, credential_id, lease_id, true, true, 2)
            .await
    });
    if admitted {
        return;
    }
    let synchronous_fallback_redis = fallback_redis.clone();
    let admitted = spawn_best_effort_storage_task(
        "关键队列饱和后清理结果不确定的 Redis 凭据准入",
        async move {
            release_redis_in_flight_lease_and_wakeup(
                fallback_redis,
                credential_id,
                lease_id,
                true,
                true,
                2,
            )
            .await
        },
    );
    if !admitted {
        // Both background lanes are bounded. Keep the final fallback on this stack so cleanup
        // cannot be dropped or create an unbounded third task source under overload.
        if let Err(err) = block_on_storage("Redis 准入清理队列均饱和后的同步回滚", async move {
            release_redis_in_flight_lease_and_wakeup(
                synchronous_fallback_redis,
                credential_id,
                lease_id,
                true,
                true,
                2,
            )
            .await
        }) {
            tracing::error!(
                credential_id,
                lease_id,
                "Redis 准入清理有界同步回滚仍失败，将由 tombstone 与分布式 lease 最大年龄兜底: {}",
                err
            );
        }
    }
}

pub(super) async fn release_redis_in_flight_lease_and_wakeup(
    redis: Arc<RedisStore>,
    credential_id: u64,
    lease_id: u64,
    tombstone: bool,
    rollback_rate_limit: bool,
    attempts: usize,
) -> anyhow::Result<()> {
    let payload = serde_json::json!({
        "kind": "dispatch_wakeup",
        "credentialId": credential_id,
        "leaseId": lease_id,
        "changedAt": Utc::now().to_rfc3339(),
    })
    .to_string();
    let attempts = attempts.max(1);
    let mut last_error = None;
    for attempt in 0..attempts {
        match tokio::time::timeout(REDIS_CRITICAL_OPERATION_TIMEOUT, async {
            if rollback_rate_limit {
                redis
                    .rollback_dispatch_admission_and_publish_wakeup(
                        credential_id,
                        lease_id,
                        tombstone,
                        &payload,
                    )
                    .await
            } else {
                redis
                    .release_in_flight_lease_and_publish_wakeup(
                        credential_id,
                        lease_id,
                        tombstone,
                        &payload,
                    )
                    .await
            }
        })
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

fn submit_critical_release_or_run_sync_fallback(
    submit_critical: impl FnOnce() -> bool,
    sync_fallback: impl FnOnce(),
) {
    if !submit_critical() {
        sync_fallback();
    }
}

fn release_redis_in_flight_lease_reliably(
    redis: Arc<RedisStore>,
    credential_id: u64,
    lease_id: u64,
    tombstone: bool,
    rollback_rate_limit: bool,
) {
    let fallback_redis = redis.clone();
    // The Redis release script publishes only when it removes this lease or its owned pacing
    // reservation, so retrying here cannot emit duplicate wakeups after a committed first try.
    submit_critical_release_or_run_sync_fallback(
        move || {
            spawn_critical_storage_task("释放 Redis 并发 lease 并唤醒调度", async move {
                release_redis_in_flight_lease_and_wakeup(
                    redis,
                    credential_id,
                    lease_id,
                    tombstone,
                    rollback_rate_limit,
                    2,
                )
                .await
            })
        },
        move || {
            if let Err(err) =
                block_on_storage("关键队列拒绝后同步释放 Redis 并发 lease", async move {
                    release_redis_in_flight_lease_and_wakeup(
                        fallback_redis,
                        credential_id,
                        lease_id,
                        tombstone,
                        rollback_rate_limit,
                        2,
                    )
                    .await
                })
            {
                tracing::error!(
                    credential_id,
                    lease_id,
                    "Redis 并发 lease 有界重试仍失败，将由 tombstone 与 lease TTL 回收: {}",
                    err
                );
            }
        },
    );
}

async fn release_redis_dispatch_queue_lease_with_retry(
    redis: Arc<RedisStore>,
    lease_id: String,
    ttl_secs: u64,
    attempts: usize,
) -> anyhow::Result<()> {
    let attempts = attempts.max(1);
    let mut last_error = None;
    for attempt in 0..attempts {
        match tokio::time::timeout(
            REDIS_CRITICAL_OPERATION_TIMEOUT,
            redis.leave_dispatch_queue_with_tombstone(&lease_id, ttl_secs),
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

pub(super) fn release_redis_dispatch_queue_lease_reliably(
    redis: Arc<RedisStore>,
    lease_id: String,
    ttl_secs: u64,
) {
    let fallback_redis = redis.clone();
    let fallback_lease_id = lease_id.clone();
    let admitted = spawn_critical_storage_task("释放 Redis 调度排队 lease", async move {
        release_redis_dispatch_queue_lease_with_retry(redis, lease_id, ttl_secs, 2).await
    });
    if !admitted {
        if let Err(err) = block_on_storage(
            "关键队列拒绝后同步释放 Redis 调度排队 lease",
            async move {
                release_redis_dispatch_queue_lease_with_retry(
                    fallback_redis,
                    fallback_lease_id,
                    ttl_secs,
                    2,
                )
                .await
            },
        ) {
            tracing::error!(
                "Redis 调度排队 lease 有界重试仍失败，将由 lease TTL 回收: {}",
                err
            );
        }
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
    released_lease_tombstones: Option<ReleasedInFlightLeaseTombstones>,
    in_flight_notify: Arc<Notify>,
    local_pool_route_state_cache: Arc<Mutex<HashMap<String, CachedLocalPoolRouteState>>>,
    credential_id: u64,
    lease_id: u64,
    weight_units: u32,
    tombstone_redis_release: bool,
    state: AtomicU8,
    redis_touch_interval: Option<StdDuration>,
    redis_lease_ttl_secs: u64,
    last_redis_touch_at: Arc<Mutex<Instant>>,
}

pub(super) struct LocalRedisAdmissionReservationGuard {
    entries: Arc<Mutex<Vec<CredentialEntry>>>,
    in_flight_notify: Arc<Notify>,
    local_pool_route_state_cache: Arc<Mutex<HashMap<String, CachedLocalPoolRouteState>>>,
    credential_id: u64,
    lease_id: u64,
    armed: bool,
}

impl LocalRedisAdmissionReservationGuard {
    pub(super) fn new(
        entries: Arc<Mutex<Vec<CredentialEntry>>>,
        in_flight_notify: Arc<Notify>,
        local_pool_route_state_cache: Arc<Mutex<HashMap<String, CachedLocalPoolRouteState>>>,
        credential_id: u64,
        lease_id: u64,
    ) -> Self {
        Self {
            entries,
            in_flight_notify,
            local_pool_route_state_cache,
            credential_id,
            lease_id,
            armed: true,
        }
    }

    pub(super) fn commit(&mut self) {
        self.armed = false;
    }

    pub(super) fn release(&mut self) {
        if !self.armed {
            return;
        }
        self.armed = false;
        if release_in_flight_lease_from_entries(
            &self.entries,
            self.credential_id,
            self.lease_id,
            true,
        ) {
            self.local_pool_route_state_cache.lock().clear();
            self.in_flight_notify.notify_waiters();
        }
    }
}

impl Drop for LocalRedisAdmissionReservationGuard {
    fn drop(&mut self) {
        self.release();
    }
}

fn redis_touch_interval_for_max_age(max_age: Option<StdDuration>) -> Option<StdDuration> {
    let max_age = max_age.filter(|duration| !duration.is_zero())?;
    Some(
        (max_age / 3)
            .max(REDIS_IN_FLIGHT_TOUCH_INTERVAL_MIN)
            .min(REDIS_IN_FLIGHT_TOUCH_INTERVAL_MAX),
    )
}

fn redis_lease_ttl_secs_for_max_age(max_age: Option<StdDuration>) -> u64 {
    max_age
        .filter(|duration| !duration.is_zero())
        .map(|duration| duration.as_secs().saturating_mul(2).max(60))
        .unwrap_or(0)
}

fn rollback_redis_touch_reservation(
    last_touch_at: &Mutex<Instant>,
    attempted_at: Instant,
    previous_at: Instant,
) {
    let mut last_touch = last_touch_at.lock();
    if *last_touch == attempted_at {
        *last_touch = previous_at;
    }
}

struct RedisTouchReservation {
    last_touch_at: Arc<Mutex<Instant>>,
    attempted_at: Instant,
    previous_at: Instant,
    committed: bool,
}

impl RedisTouchReservation {
    fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for RedisTouchReservation {
    fn drop(&mut self) {
        if !self.committed {
            rollback_redis_touch_reservation(
                &self.last_touch_at,
                self.attempted_at,
                self.previous_at,
            );
        }
    }
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
        released_lease_tombstones: Option<ReleasedInFlightLeaseTombstones>,
        in_flight_notify: Arc<Notify>,
        local_pool_route_state_cache: Arc<Mutex<HashMap<String, CachedLocalPoolRouteState>>>,
        credential_id: u64,
        lease_id: u64,
        weight_units: u32,
        tombstone_redis_release: bool,
        redis_lease_max_age: Option<StdDuration>,
    ) -> Self {
        Self {
            entries,
            redis_store,
            released_lease_tombstones,
            in_flight_notify,
            local_pool_route_state_cache,
            credential_id,
            lease_id,
            weight_units: weight_units.max(1),
            tombstone_redis_release,
            state: AtomicU8::new(IN_FLIGHT_LEASE_PENDING_DISPATCH),
            redis_touch_interval: redis_touch_interval_for_max_age(redis_lease_max_age),
            redis_lease_ttl_secs: redis_lease_ttl_secs_for_max_age(redis_lease_max_age),
            last_redis_touch_at: Arc::new(Mutex::new(Instant::now())),
        }
    }

    #[cfg(test)]
    pub(crate) fn id(&self) -> u64 {
        self.lease_id
    }

    pub fn release(&self) {
        let previous_state = self.state.swap(IN_FLIGHT_LEASE_RELEASED, Ordering::AcqRel);
        if previous_state == IN_FLIGHT_LEASE_RELEASED {
            return;
        }
        if let Some(tombstones) = &self.released_lease_tombstones {
            record_released_in_flight_lease_tombstone(
                tombstones,
                self.credential_id,
                self.lease_id,
            );
        }
        let rollback_rate_limit = previous_state == IN_FLIGHT_LEASE_PENDING_DISPATCH;
        let released_locally = release_in_flight_lease_from_entries(
            &self.entries,
            self.credential_id,
            self.lease_id,
            rollback_rate_limit,
        );
        if released_locally {
            self.local_pool_route_state_cache.lock().clear();
            self.in_flight_notify.notify_waiters();
        }
        if let Some(redis) = &self.redis_store {
            release_redis_in_flight_lease_reliably(
                redis.clone(),
                self.credential_id,
                self.lease_id,
                self.tombstone_redis_release,
                rollback_rate_limit,
            );
        }
    }

    pub(crate) fn mark_upstream_dispatch_started(&self) {
        let _ = self.state.compare_exchange(
            IN_FLIGHT_LEASE_PENDING_DISPATCH,
            IN_FLIGHT_LEASE_DISPATCHED,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    pub fn touch(&self) {
        touch_in_flight_lease_from_entries(&self.entries, self.credential_id, self.lease_id);
        let touch_reservation = self.redis_touch_interval.and_then(|interval| {
            let now = Instant::now();
            let mut last_touch = self.last_redis_touch_at.lock();
            if now.saturating_duration_since(*last_touch) < interval {
                return None;
            }
            let previous = *last_touch;
            *last_touch = now;
            Some(RedisTouchReservation {
                last_touch_at: self.last_redis_touch_at.clone(),
                attempted_at: now,
                previous_at: previous,
                committed: false,
            })
        });
        if let Some(mut reservation) = touch_reservation {
            let Some(redis) = &self.redis_store else {
                return;
            };
            let redis = redis.clone();
            let credential_id = self.credential_id;
            let lease_id = self.lease_id;
            let ttl_secs = self.redis_lease_ttl_secs;
            spawn_critical_storage_task("更新 Redis 并发 lease 活跃时间", async move {
                tokio::time::timeout(
                    REDIS_CRITICAL_OPERATION_TIMEOUT,
                    redis.touch_in_flight_lease(credential_id, lease_id, ttl_secs),
                )
                .await
                .map_err(|_| {
                    anyhow::anyhow!(
                        "Redis lease touch timed out after {}ms",
                        REDIS_CRITICAL_OPERATION_TIMEOUT.as_millis()
                    )
                })??;
                reservation.commit();
                Ok(())
            });
        }
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
    local_queued: Arc<AtomicU32>,
    in_flight_notify: Arc<Notify>,
    local_pool_route_state_cache: Arc<Mutex<HashMap<String, CachedLocalPoolRouteState>>>,
    redis_lease_id: Option<String>,
    redis_lease_ttl_secs: u64,
    next_redis_renew_at: Option<Instant>,
    released: AtomicBool,
}

impl DispatchQueueGuard {
    pub(super) fn new(
        redis_store: Option<Arc<RedisStore>>,
        local_queued: Arc<AtomicU32>,
        in_flight_notify: Arc<Notify>,
        local_pool_route_state_cache: Arc<Mutex<HashMap<String, CachedLocalPoolRouteState>>>,
        redis_lease_id: Option<String>,
        redis_lease_ttl_secs: u64,
    ) -> Self {
        let renew_interval_secs =
            (redis_lease_ttl_secs / 3).clamp(1, DISPATCH_QUEUE_LEASE_RENEW_INTERVAL_MAX_SECS);
        let next_redis_renew_at = redis_lease_id
            .as_ref()
            .map(|_| Instant::now() + StdDuration::from_secs(renew_interval_secs));
        Self {
            redis_store,
            local_queued,
            in_flight_notify,
            local_pool_route_state_cache,
            redis_lease_id,
            redis_lease_ttl_secs,
            next_redis_renew_at,
            released: AtomicBool::new(false),
        }
    }

    pub(super) fn redis_renewal_due(&self) -> bool {
        !self.released.load(Ordering::Acquire)
            && self
                .next_redis_renew_at
                .is_some_and(|renew_at| Instant::now() >= renew_at)
    }

    pub(super) async fn renew_if_needed(&mut self) -> anyhow::Result<bool> {
        if self.released.load(Ordering::Acquire)
            || self
                .next_redis_renew_at
                .is_none_or(|renew_at| Instant::now() < renew_at)
        {
            return Ok(true);
        }
        let (Some(redis), Some(lease_id)) = (&self.redis_store, &self.redis_lease_id) else {
            self.next_redis_renew_at = None;
            return Ok(true);
        };
        let renewed = redis
            .renew_dispatch_queue(lease_id, self.redis_lease_ttl_secs)
            .await?;
        if !renewed {
            return Ok(false);
        }
        let renew_interval_secs =
            (self.redis_lease_ttl_secs / 3).clamp(1, DISPATCH_QUEUE_LEASE_RENEW_INTERVAL_MAX_SECS);
        self.next_redis_renew_at =
            Some(Instant::now() + StdDuration::from_secs(renew_interval_secs));
        Ok(true)
    }

    fn release(&self) {
        if self.released.swap(true, Ordering::AcqRel) {
            return;
        }
        self.local_queued.fetch_sub(1, Ordering::AcqRel);
        self.local_pool_route_state_cache.lock().clear();
        if let (Some(redis), Some(lease_id)) = (&self.redis_store, &self.redis_lease_id) {
            release_redis_dispatch_queue_lease_reliably(
                redis.clone(),
                lease_id.clone(),
                self.redis_lease_ttl_secs,
            );
        }
        self.in_flight_notify.notify_waiters();
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
    rollback_rate_limit: bool,
) -> bool {
    let mut entries = entries.lock();
    let Some(entry) = entries.iter_mut().find(|e| e.id == credential_id) else {
        return false;
    };

    let mut changed = false;
    if entry
        .pending_redis_admission
        .is_some_and(|pending| pending.lease_id == lease_id)
    {
        entry.pending_redis_admission = None;
        changed = true;
    }
    if rollback_rate_limit && entry.rate_limit_owner_lease_id == Some(lease_id) {
        entry.rate_limit_available_at = None;
        entry.rate_limit_rpm = None;
        entry.rate_limit_owner_lease_id = None;
        entry.rate_limit_redis_deadline_ms = None;
        changed = true;
    }

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
        changed = true;
    }
    changed
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
    use std::collections::VecDeque;

    use crate::kiro::model::credentials::KiroCredentials;
    use crate::storage::redis_cache::SchedulerHealthState;

    use super::super::account_state::InFlightLease;

    fn credential_entry_with_lease(
        lease_id: u64,
        rate_limit_owner_lease_id: Option<u64>,
    ) -> CredentialEntry {
        let now = Instant::now();
        CredentialEntry {
            id: 1,
            credentials: KiroCredentials::default(),
            failure_count: 0,
            refresh_failure_count: 0,
            runtime_revision: 0,
            runtime_generation: 0,
            runtime_persistence_degraded: false,
            disabled: false,
            disabled_reason: None,
            success_count: 0,
            total_selection_count: 0,
            last_used_at: None,
            cooldown_until: None,
            cooldown_reason: None,
            model_cooldowns: HashMap::new(),
            rate_limit_available_at: Some(now + StdDuration::from_secs(1)),
            rate_limit_rpm: Some(60),
            rate_limit_owner_lease_id,
            rate_limit_redis_deadline_ms: Some(1),
            pending_redis_admission: None,
            in_flight_requests: 3,
            in_flight_leases: vec![InFlightLease {
                id: lease_id,
                acquired_at: now,
                last_seen_at: now,
                kind: InFlightKind::Api,
                weight_units: 3,
                locally_owned: true,
            }],
            warmup_remaining: 0,
            health: SchedulerHealthState::default(),
            model_health: HashMap::new(),
            selection_events: VecDeque::new(),
        }
    }

    #[test]
    fn critical_release_submission_runs_sync_fallback_only_when_rejected() {
        let submissions = AtomicU32::new(0);
        let fallbacks = AtomicU32::new(0);
        submit_critical_release_or_run_sync_fallback(
            || {
                submissions.fetch_add(1, Ordering::Relaxed);
                true
            },
            || {
                fallbacks.fetch_add(1, Ordering::Relaxed);
            },
        );
        assert_eq!(submissions.load(Ordering::Relaxed), 1);
        assert_eq!(fallbacks.load(Ordering::Relaxed), 0);

        submit_critical_release_or_run_sync_fallback(
            || {
                submissions.fetch_add(1, Ordering::Relaxed);
                false
            },
            || {
                fallbacks.fetch_add(1, Ordering::Relaxed);
            },
        );
        assert_eq!(submissions.load(Ordering::Relaxed), 2);
        assert_eq!(fallbacks.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn local_lease_release_is_idempotent_and_only_owner_rolls_back_pacing() {
        let lease_id = 41;
        let entries = Mutex::new(vec![credential_entry_with_lease(lease_id, Some(lease_id))]);

        assert!(release_in_flight_lease_from_entries(
            &entries, 1, lease_id, true
        ));
        {
            let entries = entries.lock();
            let entry = &entries[0];
            assert_eq!(entry.in_flight_requests, 0);
            assert!(entry.in_flight_leases.is_empty());
            assert!(entry.rate_limit_available_at.is_none());
            assert!(entry.rate_limit_rpm.is_none());
            assert!(entry.rate_limit_owner_lease_id.is_none());
            assert!(entry.rate_limit_redis_deadline_ms.is_none());
        }
        assert!(!release_in_flight_lease_from_entries(
            &entries, 1, lease_id, true
        ));

        let newer_owner = lease_id + 1;
        let entries = Mutex::new(vec![credential_entry_with_lease(
            lease_id,
            Some(newer_owner),
        )]);
        assert!(release_in_flight_lease_from_entries(
            &entries, 1, lease_id, true
        ));
        let entries = entries.lock();
        let entry = &entries[0];
        assert_eq!(entry.in_flight_requests, 0);
        assert!(entry.in_flight_leases.is_empty());
        assert!(entry.rate_limit_available_at.is_some());
        assert_eq!(entry.rate_limit_rpm, Some(60));
        assert_eq!(entry.rate_limit_owner_lease_id, Some(newer_owner));
        assert_eq!(entry.rate_limit_redis_deadline_ms, Some(1));
    }

    #[test]
    fn distributed_lease_max_age_uses_safety_default_for_zero() {
        assert_eq!(
            distributed_in_flight_lease_max_age(0),
            StdDuration::from_secs(SCHEDULER_DISTRIBUTED_LEASE_SAFETY_SECS)
        );
        assert_eq!(
            distributed_in_flight_lease_max_age(37),
            StdDuration::from_secs(37)
        );
    }

    #[test]
    fn redis_touch_interval_tracks_lease_age_and_is_bounded() {
        assert_eq!(redis_touch_interval_for_max_age(None), None);
        assert_eq!(
            redis_touch_interval_for_max_age(Some(StdDuration::ZERO)),
            None
        );
        assert_eq!(
            redis_touch_interval_for_max_age(Some(StdDuration::from_secs(30))),
            Some(StdDuration::from_secs(10))
        );
        assert_eq!(
            redis_touch_interval_for_max_age(Some(StdDuration::from_secs(900))),
            Some(REDIS_IN_FLIGHT_TOUCH_INTERVAL_MAX)
        );
        assert_eq!(redis_lease_ttl_secs_for_max_age(None), 0);
        assert_eq!(
            redis_lease_ttl_secs_for_max_age(Some(StdDuration::from_secs(30))),
            60
        );
    }

    #[test]
    fn failed_redis_touch_reservation_is_retryable_without_overwriting_newer_touch() {
        let previous = Instant::now() - StdDuration::from_secs(20);
        let attempted = Instant::now() - StdDuration::from_secs(10);
        let last_touch = Mutex::new(attempted);
        rollback_redis_touch_reservation(&last_touch, attempted, previous);
        assert_eq!(*last_touch.lock(), previous);

        let newer = Instant::now();
        *last_touch.lock() = newer;
        rollback_redis_touch_reservation(&last_touch, attempted, previous);
        assert_eq!(*last_touch.lock(), newer);
    }

    #[test]
    fn redis_touch_reservation_rolls_back_on_drop_unless_committed() {
        let previous = Instant::now() - StdDuration::from_secs(20);
        let attempted = Instant::now() - StdDuration::from_secs(10);
        let last_touch = Arc::new(Mutex::new(attempted));
        {
            let _reservation = RedisTouchReservation {
                last_touch_at: last_touch.clone(),
                attempted_at: attempted,
                previous_at: previous,
                committed: false,
            };
        }
        assert_eq!(*last_touch.lock(), previous);

        *last_touch.lock() = attempted;
        {
            let mut reservation = RedisTouchReservation {
                last_touch_at: last_touch.clone(),
                attempted_at: attempted,
                previous_at: previous,
                committed: false,
            };
            reservation.commit();
        }
        assert_eq!(*last_touch.lock(), attempted);
    }

    #[test]
    fn dispatch_queue_guard_releases_local_count_only_once() {
        let queued = Arc::new(AtomicU32::new(1));
        let guard = DispatchQueueGuard::new(
            None,
            queued.clone(),
            Arc::new(Notify::new()),
            Arc::new(Mutex::new(HashMap::new())),
            None,
            60,
        );

        guard.release();
        guard.release();
        drop(guard);

        assert_eq!(queued.load(Ordering::Acquire), 0);
    }
}
