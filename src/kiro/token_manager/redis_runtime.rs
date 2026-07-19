use chrono::Utc;
use parking_lot::Mutex;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant};

use crate::model::config::Config;
use crate::storage::postgres::PostgresStore;
use crate::storage::redis_cache::{RedisStore, SchedulerCredentialState, SchedulerHealthState};

use super::account_state::{CredentialEntry, CredentialModelCooldown, InFlightLease};
use super::concurrency::distributed_in_flight_lease_max_age;
use super::cooldown::model_state_key;
use super::rpm::{effective_rpm, rate_limit_interval_for_rpm};
use super::storage_task::spawn_best_effort_storage_task;
use super::types::InFlightKind;

fn instant_from_epoch_ms(target_ms: i64, now_ms: i64, now: Instant) -> Option<Instant> {
    (target_ms > now_ms)
        .then(|| now + std::time::Duration::from_millis((target_ms - now_ms) as u64))
}

fn instant_from_elapsed_epoch_ms(target_ms: i64, now_ms: i64, now: Instant) -> Instant {
    if target_ms >= now_ms {
        now
    } else {
        now.checked_sub(std::time::Duration::from_millis(
            (now_ms - target_ms) as u64,
        ))
        .unwrap_or(now)
    }
}

fn redis_in_flight_lease_is_fresh(last_seen_at_ms: i64, now_ms: i64, max_age: StdDuration) -> bool {
    if last_seen_at_ms >= now_ms {
        return true;
    }
    let idle_ms = (now_ms - last_seen_at_ms) as u128;
    idle_ms <= max_age.as_millis()
}

fn runtime_event_payload(kind: &str, version: Option<i64>, reason: &str) -> String {
    serde_json::json!({
        "kind": kind,
        "version": version,
        "reason": reason,
        "changedAt": Utc::now().to_rfc3339(),
    })
    .to_string()
}

pub(super) fn publish_runtime_config_changed(
    redis_store: Option<&Arc<RedisStore>>,
    version: Option<i64>,
    reason: &str,
) {
    let Some(redis) = redis_store else {
        return;
    };
    let redis = redis.clone();
    let payload = runtime_event_payload("runtime_config_changed", version, reason);
    spawn_best_effort_storage_task("发布 Redis 运行配置变更通知", async move {
        redis.publish_runtime_config_changed(payload).await
    });
}

pub(super) fn publish_credentials_changed(
    postgres_store: Option<&Arc<PostgresStore>>,
    redis_store: Option<&Arc<RedisStore>>,
    reason: &str,
) {
    if let Some(store) = postgres_store {
        let store = store.clone();
        let reason_owned = reason.to_string();
        spawn_best_effort_storage_task("记录凭据事件到 PgSQL", async move {
            store
                .record_credential_event(
                    None,
                    "credentials_changed",
                    Some(&reason_owned),
                    serde_json::json!({ "reason": reason_owned }),
                )
                .await
        });
    }
    let Some(redis) = redis_store else {
        return;
    };
    let redis = redis.clone();
    let payload = runtime_event_payload("credentials_changed", None, reason);
    spawn_best_effort_storage_task("发布 Redis 凭据变更通知", async move {
        redis.publish_credentials_changed(payload).await
    });
}

fn apply_scheduler_state_to_entry(
    entry: &mut CredentialEntry,
    state: SchedulerCredentialState,
    global_rpm: u32,
    in_flight_max_age: StdDuration,
    now_ms: i64,
    now: Instant,
) {
    entry.cooldown_until = state
        .cooldown
        .as_ref()
        .and_then(|cooldown| instant_from_epoch_ms(cooldown.until_ms, now_ms, now));
    entry.cooldown_reason = state.cooldown.and_then(|cooldown| cooldown.reason);
    entry.model_cooldowns.clear();
    entry.model_health.clear();
    for model_state in state.model_states {
        let key = model_state_key(&model_state.model);
        if let Some(cooldown) = model_state.cooldown {
            if let Some(until) = instant_from_epoch_ms(cooldown.until_ms, now_ms, now) {
                entry.model_cooldowns.insert(
                    key.clone(),
                    CredentialModelCooldown {
                        model: model_state.model.clone(),
                        until,
                        reason: cooldown.reason,
                    },
                );
            }
        }
        entry.model_health.insert(key, model_state.health);
    }
    let current_rpm = effective_rpm(entry, global_rpm);
    let redis_reservation = (rate_limit_interval_for_rpm(current_rpm).is_some()
        && state.rate_limit_rpm == Some(current_rpm))
    .then(|| {
        state
            .rate_limit_available_at_ms
            .zip(state.rate_limit_remaining_ms)
            .map(|(redis_deadline_ms, remaining_ms)| {
                (
                    now + StdDuration::from_millis(remaining_ms.max(1)),
                    state.rate_limit_owner_lease_id,
                    redis_deadline_ms,
                )
            })
    })
    .flatten();
    let local_reservation = (entry.rate_limit_rpm == Some(current_rpm))
        .then_some(entry.rate_limit_available_at)
        .flatten()
        .filter(|available_at| *available_at > now)
        .zip(entry.rate_limit_redis_deadline_ms)
        .map(|(available_at, redis_deadline_ms)| {
            (
                available_at,
                entry.rate_limit_owner_lease_id,
                redis_deadline_ms,
            )
        });
    let reservation = match (local_reservation, redis_reservation) {
        (Some(local), Some(redis)) if local.2 > redis.2 => Some(local),
        (Some(local), Some(redis)) if local.2 == redis.2 => {
            Some((local.0.max(redis.0), local.1.or(redis.1), redis.2))
        }
        (Some(local), None) => Some(local),
        (_, redis) => redis,
    };
    entry.rate_limit_available_at = reservation.map(|(available_at, _, _)| available_at);
    entry.rate_limit_rpm = reservation.map(|_| current_rpm);
    entry.rate_limit_owner_lease_id = reservation.and_then(|(_, owner, _)| owner);
    entry.rate_limit_redis_deadline_ms =
        reservation.map(|(_, _, redis_deadline_ms)| redis_deadline_ms);
    let previous_local_leases = std::mem::take(&mut entry.in_flight_leases);
    let mut seen_lease_ids = HashSet::new();
    let mut merged_in_flight_leases: Vec<InFlightLease> = state
        .in_flight_leases
        .into_iter()
        .filter(|lease| {
            redis_in_flight_lease_is_fresh(lease.last_seen_at_ms, now_ms, in_flight_max_age)
        })
        .map(|lease| {
            seen_lease_ids.insert(lease.id);
            InFlightLease {
                id: lease.id,
                acquired_at: instant_from_elapsed_epoch_ms(lease.acquired_at_ms, now_ms, now),
                last_seen_at: instant_from_elapsed_epoch_ms(lease.last_seen_at_ms, now_ms, now),
                kind: InFlightKind::from_str(&lease.kind),
                weight_units: lease.weight_units.max(1),
                locally_owned: previous_local_leases
                    .iter()
                    .any(|local| local.id == lease.id && local.locally_owned),
            }
        })
        .collect();
    for lease in previous_local_leases {
        if lease.locally_owned && seen_lease_ids.insert(lease.id) {
            merged_in_flight_leases.push(lease);
        }
    }
    entry.in_flight_requests = merged_in_flight_leases.iter().fold(0u32, |sum, lease| {
        sum.saturating_add(lease.weight_units.max(1))
    });
    entry.in_flight_leases = merged_in_flight_leases;
    entry.health = state.health;
}

pub(super) fn apply_scheduler_states(
    entries: &Mutex<Vec<CredentialEntry>>,
    config: &Mutex<Config>,
    states: HashMap<u64, SchedulerCredentialState>,
) {
    let (global_rpm, configured_in_flight_max_age_secs) = {
        let config = config.lock();
        (
            config.credential_rpm.unwrap_or(0),
            config.credential_in_flight_lease_max_secs,
        )
    };
    let in_flight_max_age = distributed_in_flight_lease_max_age(configured_in_flight_max_age_secs);
    apply_scheduler_states_with_global_rpm(entries, global_rpm, in_flight_max_age, states);
}

pub(super) fn apply_scheduler_states_with_global_rpm(
    entries: &Mutex<Vec<CredentialEntry>>,
    global_rpm: u32,
    in_flight_max_age: StdDuration,
    states: HashMap<u64, SchedulerCredentialState>,
) {
    let now_ms = Utc::now().timestamp_millis();
    let now = Instant::now();
    let mut entries = entries.lock();
    for entry in entries.iter_mut() {
        let state = states.get(&entry.id).cloned().unwrap_or_default();
        apply_scheduler_state_to_entry(entry, state, global_rpm, in_flight_max_age, now_ms, now);
    }
}

pub(super) fn apply_scheduler_states_for_ids(
    entries: &Mutex<Vec<CredentialEntry>>,
    config: &Mutex<Config>,
    states: HashMap<u64, SchedulerCredentialState>,
) {
    if states.is_empty() {
        return;
    }
    let now_ms = Utc::now().timestamp_millis();
    let now = Instant::now();
    let (global_rpm, configured_in_flight_max_age_secs) = {
        let config = config.lock();
        (
            config.credential_rpm.unwrap_or(0),
            config.credential_in_flight_lease_max_secs,
        )
    };
    let in_flight_max_age = distributed_in_flight_lease_max_age(configured_in_flight_max_age_secs);
    let mut entries = entries.lock();
    for entry in entries.iter_mut() {
        if let Some(state) = states.get(&entry.id).cloned() {
            apply_scheduler_state_to_entry(
                entry,
                state,
                global_rpm,
                in_flight_max_age,
                now_ms,
                now,
            );
        }
    }
}

pub(super) fn clear_scheduler_state_for_credential_local(
    entries: &Mutex<Vec<CredentialEntry>>,
    id: u64,
    clear_in_flight: bool,
) {
    let mut entries = entries.lock();
    if let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) {
        entry.cooldown_until = None;
        entry.cooldown_reason = None;
        entry.model_cooldowns.clear();
        entry.rate_limit_available_at = None;
        entry.rate_limit_rpm = None;
        entry.rate_limit_owner_lease_id = None;
        entry.rate_limit_redis_deadline_ms = None;
        entry.health = SchedulerHealthState::default();
        entry.model_health.clear();
        if clear_in_flight {
            entry.in_flight_requests = 0;
            entry.in_flight_leases.clear();
        }
    }
}

pub(super) fn clear_scheduler_state_for_credential_redis(
    redis_store: Option<&Arc<RedisStore>>,
    id: u64,
    clear_in_flight: bool,
) {
    let Some(redis) = redis_store else {
        return;
    };
    let redis = redis.clone();
    spawn_best_effort_storage_task("清理 Redis 凭据调度状态", async move {
        redis.clear_scheduler_cooldown(id).await?;
        redis.clear_scheduler_health(id).await?;
        redis.clear_rate_limit(id).await?;
        if clear_in_flight {
            redis.clear_in_flight_leases(id, None).await?;
        }
        Ok(())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_config_snapshot_age_expires_old_distributed_lease() {
        let max_age = distributed_in_flight_lease_max_age(0);
        let now_ms = 1_000_000i64;
        assert!(redis_in_flight_lease_is_fresh(
            now_ms - max_age.as_millis() as i64,
            now_ms,
            max_age,
        ));
        assert!(!redis_in_flight_lease_is_fresh(
            now_ms - max_age.as_millis() as i64 - 1,
            now_ms,
            max_age,
        ));
    }
}
