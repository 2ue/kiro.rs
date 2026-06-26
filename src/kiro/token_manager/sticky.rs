use chrono::Utc;
use parking_lot::Mutex;

use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::redis_cache::{RedisStore, SchedulerSessionBinding};

use super::account_state::SessionBinding;
use super::storage_task::spawn_best_effort_storage_task;

/// 会话绑定最长保留时间，避免长期运行进程无限增长。
const SESSION_BINDING_TTL_SECS: i64 = 6 * 60 * 60;
/// 会话绑定表上限。
const MAX_SESSION_BINDINGS: usize = 10_000;
/// 同一会话绑定账号连续软失败达到该阈值后，本次请求允许临时 fallback。
const MAX_SESSION_SOFT_FAILURES: u32 = 2;

pub(super) fn prune_session_bindings_locked(bindings: &mut HashMap<String, SessionBinding>) {
    let now = Utc::now();
    bindings.retain(|_, binding| {
        now.signed_duration_since(binding.last_used_at)
            .num_seconds()
            <= SESSION_BINDING_TTL_SECS
    });

    if bindings.len() <= MAX_SESSION_BINDINGS {
        return;
    }

    let mut sessions_by_age: Vec<_> = bindings
        .iter()
        .map(|(session_id, binding)| (session_id.clone(), binding.last_used_at))
        .collect();
    sessions_by_age.sort_by_key(|(_, last_used_at)| *last_used_at);

    let remove_count = bindings.len() - MAX_SESSION_BINDINGS;
    for (session_id, _) in sessions_by_age.into_iter().take(remove_count) {
        bindings.remove(&session_id);
    }
}

pub(super) fn bound_credential_id(
    session_bindings: &Mutex<HashMap<String, SessionBinding>>,
    session_id: &str,
) -> Option<u64> {
    session_bindings
        .lock()
        .get(session_id)
        .map(|binding| binding.credential_id)
}

pub(super) fn bind_session_to_credential(
    session_bindings: &Mutex<HashMap<String, SessionBinding>>,
    redis_store: Option<&Arc<RedisStore>>,
    session_id: &str,
    credential_id: u64,
) {
    let soft_failure_count = {
        let mut bindings = session_bindings.lock();
        prune_session_bindings_locked(&mut bindings);
        match bindings.get_mut(session_id) {
            Some(binding) if binding.credential_id == credential_id => {
                binding.last_used_at = Utc::now();
                binding.soft_failure_count
            }
            _ => {
                bindings.insert(
                    session_id.to_string(),
                    SessionBinding {
                        credential_id,
                        last_used_at: Utc::now(),
                        soft_failure_count: 0,
                    },
                );
                0
            }
        }
    };
    if let Some(redis) = redis_store {
        let redis = redis.clone();
        let session_id = session_id.to_string();
        spawn_best_effort_storage_task("写入 Redis 会话绑定", async move {
            let binding = SchedulerSessionBinding {
                credential_id,
                last_used_at: Utc::now(),
                soft_failure_count,
            };
            redis
                .set_session_binding(&session_id, &binding, SESSION_BINDING_TTL_SECS as usize)
                .await
        });
    }
}

pub(super) fn unbind_session(
    session_bindings: &Mutex<HashMap<String, SessionBinding>>,
    redis_store: Option<&Arc<RedisStore>>,
    session_id: &str,
) {
    session_bindings.lock().remove(session_id);
    if let Some(redis) = redis_store {
        let redis = redis.clone();
        let session_id = session_id.to_string();
        spawn_best_effort_storage_task("删除 Redis 会话绑定", async move {
            redis.delete_session_binding(&session_id).await
        });
    }
}

pub(super) fn unbind_session_if_bound_to(
    session_bindings: &Mutex<HashMap<String, SessionBinding>>,
    redis_store: Option<&Arc<RedisStore>>,
    session_id: &str,
    credential_id: u64,
) {
    let mut bindings = session_bindings.lock();
    if bindings
        .get(session_id)
        .is_some_and(|binding| binding.credential_id == credential_id)
    {
        bindings.remove(session_id);
        drop(bindings);
        if let Some(redis) = redis_store {
            let redis = redis.clone();
            let session_id = session_id.to_string();
            spawn_best_effort_storage_task("删除 Redis 会话绑定", async move {
                redis.delete_session_binding(&session_id).await
            });
        }
    }
}

pub(super) fn unbind_sessions_for_credential(
    session_bindings: &Mutex<HashMap<String, SessionBinding>>,
    redis_store: Option<&Arc<RedisStore>>,
    credential_id: u64,
) {
    session_bindings
        .lock()
        .retain(|_, binding| binding.credential_id != credential_id);
    if let Some(redis) = redis_store {
        let redis = redis.clone();
        spawn_best_effort_storage_task("删除 Redis 凭据会话绑定", async move {
            redis.delete_sessions_for_credential(credential_id).await?;
            Ok(())
        });
    }
}

pub(super) fn record_session_soft_failure(
    session_bindings: &Mutex<HashMap<String, SessionBinding>>,
    redis_store: Option<&Arc<RedisStore>>,
    session_id: &str,
    credential_id: u64,
) -> bool {
    let should_fallback = {
        let mut bindings = session_bindings.lock();
        if let Some(binding) = bindings.get_mut(session_id) {
            if binding.credential_id == credential_id {
                binding.last_used_at = Utc::now();
                binding.soft_failure_count = binding.soft_failure_count.saturating_add(1);
                binding.soft_failure_count >= MAX_SESSION_SOFT_FAILURES
            } else {
                false
            }
        } else {
            false
        }
    };
    if let Some(redis) = redis_store {
        let redis = redis.clone();
        let session_id = session_id.to_string();
        spawn_best_effort_storage_task("记录 Redis 会话软失败", async move {
            redis
                .record_session_soft_failure(
                    &session_id,
                    credential_id,
                    MAX_SESSION_SOFT_FAILURES,
                    SESSION_BINDING_TTL_SECS as usize,
                )
                .await?;
            Ok(())
        });
    }
    should_fallback
}

pub(super) fn clear_session_soft_failure(
    session_bindings: &Mutex<HashMap<String, SessionBinding>>,
    redis_store: Option<&Arc<RedisStore>>,
    session_id: &str,
    credential_id: u64,
) {
    {
        let mut bindings = session_bindings.lock();
        if let Some(binding) = bindings.get_mut(session_id) {
            if binding.credential_id == credential_id {
                binding.last_used_at = Utc::now();
                binding.soft_failure_count = 0;
            }
        }
    }
    if let Some(redis) = redis_store {
        let redis = redis.clone();
        let session_id = session_id.to_string();
        spawn_best_effort_storage_task("清理 Redis 会话软失败", async move {
            redis
                .clear_session_soft_failure(
                    &session_id,
                    credential_id,
                    SESSION_BINDING_TTL_SECS as usize,
                )
                .await
        });
    }
}
