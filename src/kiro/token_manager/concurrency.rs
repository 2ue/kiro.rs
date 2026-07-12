use chrono::Utc;
use parking_lot::Mutex;
use tokio::sync::Notify;

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration as StdDuration, Instant};

use crate::storage::redis_cache::{RedisStore, SchedulerCredentialState};

use super::account_state::CredentialEntry;
use super::route_state::CachedLocalPoolRouteState;
use super::storage_task::{
    block_on_storage, spawn_best_effort_storage_task, spawn_critical_storage_task,
};
use super::types::InFlightKind;

const RELEASED_IN_FLIGHT_LEASE_TOMBSTONE_TTL: StdDuration = StdDuration::from_secs(15 * 60);
const RELEASED_IN_FLIGHT_LEASE_TOMBSTONE_PRUNE_THRESHOLD: usize = 4096;
const RELEASED_IN_FLIGHT_LEASE_TOMBSTONE_HARD_LIMIT: usize = 200_000;
const REDIS_CRITICAL_OPERATION_TIMEOUT: StdDuration = StdDuration::from_secs(2);
const REDIS_CRITICAL_RETRY_DELAY: StdDuration = StdDuration::from_millis(50);
const DISPATCH_QUEUE_LEASE_RENEW_INTERVAL_MAX_SECS: u64 = 20;

pub(super) type ReleasedInFlightLeaseTombstones = Arc<Mutex<HashMap<(u64, u64), Instant>>>;

pub(super) async fn release_redis_in_flight_lease_and_wakeup(
    redis: Arc<RedisStore>,
    credential_id: u64,
    lease_id: u64,
    tombstone: bool,
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

pub(super) fn release_redis_dispatch_queue_lease_reliably(
    redis: Arc<RedisStore>,
    lease_id: String,
) {
    let fallback_redis = redis.clone();
    let fallback_lease_id = lease_id.clone();
    let admitted = spawn_critical_storage_task("释放 Redis 调度排队 lease", async move {
        release_redis_dispatch_queue_lease_with_retry(redis, lease_id, 2).await
    });
    if !admitted {
        if let Err(err) = block_on_storage(
            "关键队列拒绝后同步释放 Redis 调度排队 lease",
            async move {
                release_redis_dispatch_queue_lease_with_retry(fallback_redis, fallback_lease_id, 2)
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
        released_lease_tombstones: Option<ReleasedInFlightLeaseTombstones>,
        in_flight_notify: Arc<Notify>,
        local_pool_route_state_cache: Arc<Mutex<HashMap<String, CachedLocalPoolRouteState>>>,
        credential_id: u64,
        lease_id: u64,
        weight_units: u32,
        tombstone_redis_release: bool,
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
            released: AtomicBool::new(false),
        }
    }

    #[cfg(test)]
    pub(crate) fn id(&self) -> u64 {
        self.lease_id
    }

    pub fn release(&self) {
        if self.released.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(tombstones) = &self.released_lease_tombstones {
            record_released_in_flight_lease_tombstone(
                tombstones,
                self.credential_id,
                self.lease_id,
            );
        }
        let released_locally =
            release_in_flight_lease_from_entries(&self.entries, self.credential_id, self.lease_id);
        if released_locally {
            self.local_pool_route_state_cache.lock().clear();
            self.in_flight_notify.notify_waiters();
        }
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            let credential_id = self.credential_id;
            let lease_id = self.lease_id;
            let tombstone_release = self.tombstone_redis_release;
            let retry_redis = redis.clone();
            let fallback_redis = redis.clone();
            if let Err(err) = block_on_storage("释放 Redis 并发 lease 并唤醒调度", async move {
                release_redis_in_flight_lease_and_wakeup(
                    redis,
                    credential_id,
                    lease_id,
                    tombstone_release,
                    1,
                )
                .await
            }) {
                tracing::warn!(
                    credential_id,
                    lease_id,
                    "同步释放 Redis 并发 lease 失败，将后台重试: {}",
                    err
                );
                let admitted =
                    spawn_critical_storage_task("重试释放 Redis 并发 lease", async move {
                        release_redis_in_flight_lease_and_wakeup(
                            retry_redis,
                            credential_id,
                            lease_id,
                            tombstone_release,
                            2,
                        )
                        .await
                    });
                if !admitted {
                    if let Err(retry_err) = block_on_storage(
                        "关键队列拒绝后同步重试释放 Redis 并发 lease",
                        async move {
                            release_redis_in_flight_lease_and_wakeup(
                                fallback_redis,
                                credential_id,
                                lease_id,
                                tombstone_release,
                                2,
                            )
                            .await
                        },
                    ) {
                        tracing::error!(
                            credential_id,
                            lease_id,
                            "Redis 并发 lease 有界重试仍失败，将由 lease TTL 回收: {}",
                            retry_err
                        );
                    }
                }
            }
        }
    }

    pub fn touch(&self) {
        touch_in_flight_lease_from_entries(&self.entries, self.credential_id, self.lease_id);
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            let credential_id = self.credential_id;
            let lease_id = self.lease_id;
            spawn_best_effort_storage_task("更新 Redis 并发 lease 活跃时间", async move {
                redis.touch_in_flight_lease(credential_id, lease_id).await
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
        self.local_pool_route_state_cache.lock().clear();
        if let (Some(redis), Some(lease_id)) = (&self.redis_store, &self.redis_lease_id) {
            release_redis_dispatch_queue_lease_reliably(redis.clone(), lease_id.clone());
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
) -> bool {
    let mut entries = entries.lock();
    let Some(entry) = entries.iter_mut().find(|e| e.id == credential_id) else {
        return false;
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
        true
    } else {
        false
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
