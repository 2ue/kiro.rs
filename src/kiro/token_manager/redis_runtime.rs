use chrono::Utc;
use parking_lot::Mutex;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant};

use crate::model::config::Config;
use crate::storage::postgres::PostgresStore;
use crate::storage::redis_cache::{RedisStore, SchedulerCredentialState, SchedulerHealthState};

use super::account_state::{CredentialEntry, CredentialModelCooldown, InFlightLease};
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

fn redis_in_flight_lease_is_fresh(
    last_seen_at_ms: i64,
    now_ms: i64,
    max_age: Option<StdDuration>,
) -> bool {
    let Some(max_age) = max_age else {
        return true;
    };
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
    in_flight_max_age: Option<StdDuration>,
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
    entry.rate_limit_available_at =
        if rate_limit_interval_for_rpm(effective_rpm(entry, global_rpm)).is_some() {
            let redis_available_at = state
                .rate_limit_available_at_ms
                .and_then(|until_ms| instant_from_epoch_ms(until_ms, now_ms, now));
            match (entry.rate_limit_available_at, redis_available_at) {
                (Some(local), Some(redis)) => Some(local.max(redis)),
                (Some(local), None) if local > now => Some(local),
                (_, redis) => redis,
            }
        } else {
            None
        };
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
    let (global_rpm, in_flight_max_age) = {
        let config = config.lock();
        (
            config.credential_rpm.unwrap_or(0),
            (config.credential_in_flight_lease_max_secs > 0)
                .then(|| StdDuration::from_secs(config.credential_in_flight_lease_max_secs)),
        )
    };
    apply_scheduler_states_with_global_rpm(entries, global_rpm, in_flight_max_age, states);
}

pub(super) fn apply_scheduler_states_with_global_rpm(
    entries: &Mutex<Vec<CredentialEntry>>,
    global_rpm: u32,
    in_flight_max_age: Option<StdDuration>,
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
    let (global_rpm, in_flight_max_age) = {
        let config = config.lock();
        (
            config.credential_rpm.unwrap_or(0),
            (config.credential_in_flight_lease_max_secs > 0)
                .then(|| StdDuration::from_secs(config.credential_in_flight_lease_max_secs)),
        )
    };
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
        redis.delete_sessions_for_credential(id).await?;
        if clear_in_flight {
            redis.clear_in_flight_leases(id, None).await?;
        }
        Ok(())
    });
}
