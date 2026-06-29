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
use super::storage_task::spawn_best_effort_storage_task;
use super::types::InFlightKind;

const RELEASED_IN_FLIGHT_LEASE_TOMBSTONE_TTL: StdDuration = StdDuration::from_secs(30);
const RELEASED_IN_FLIGHT_LEASE_TOMBSTONE_PRUNE_THRESHOLD: usize = 4096;
const RELEASED_IN_FLIGHT_LEASE_TOMBSTONE_HARD_LIMIT: usize = 200_000;

pub(super) type ReleasedInFlightLeaseTombstones = Arc<Mutex<HashMap<(u64, u64), Instant>>>;

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
    tombstone_redis_release: bool,
    released: AtomicBool,
}

impl fmt::Debug for InFlightLeaseGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InFlightLeaseGuard")
            .field("credential_id", &self.credential_id)
            .field("lease_id", &self.lease_id)
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
            spawn_best_effort_storage_task("释放 Redis 并发 lease", async move {
                let released = if tombstone_release {
                    redis
                        .release_in_flight_lease_with_tombstone(credential_id, lease_id)
                        .await?
                } else {
                    redis
                        .release_in_flight_lease(credential_id, lease_id)
                        .await?
                };
                if released {
                    let payload = serde_json::json!({
                        "kind": "dispatch_wakeup",
                        "credentialId": credential_id,
                        "leaseId": lease_id,
                        "changedAt": Utc::now().to_rfc3339(),
                    })
                    .to_string();
                    redis.publish_dispatch_wakeup(payload).await?;
                }
                Ok(())
            });
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
    redis_admitted: bool,
    released: AtomicBool,
}

impl DispatchQueueGuard {
    pub(super) fn new(
        redis_store: Option<Arc<RedisStore>>,
        local_queued: Arc<AtomicU32>,
        in_flight_notify: Arc<Notify>,
        local_pool_route_state_cache: Arc<Mutex<HashMap<String, CachedLocalPoolRouteState>>>,
        redis_admitted: bool,
    ) -> Self {
        Self {
            redis_store,
            local_queued,
            in_flight_notify,
            local_pool_route_state_cache,
            redis_admitted,
            released: AtomicBool::new(false),
        }
    }

    fn release(&self) {
        if self.released.swap(true, Ordering::AcqRel) {
            return;
        }
        self.local_queued.fetch_sub(1, Ordering::AcqRel);
        self.local_pool_route_state_cache.lock().clear();
        if self.redis_admitted {
            if let Some(redis) = &self.redis_store {
                let redis = redis.clone();
                spawn_best_effort_storage_task("释放 Redis 调度排队占位", async move {
                    redis.leave_dispatch_queue().await
                });
            }
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
        entry.in_flight_leases.remove(index);
        if entry.in_flight_requests > 0 {
            entry.in_flight_requests -= 1;
        } else {
            tracing::debug!(
                "凭据 #{} 并发 lease #{} 释放时计数已为空",
                credential_id,
                lease_id
            );
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
