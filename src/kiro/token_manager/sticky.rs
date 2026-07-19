use chrono::Utc;
use parking_lot::Mutex;

use std::collections::HashMap;

use crate::storage::redis_cache::SchedulerSessionBinding;

use super::account_state::SessionBinding;

/// 会话绑定最长保留时间，避免长期运行进程无限增长。
pub(super) const SESSION_BINDING_TTL_SECS: i64 = 6 * 60 * 60;
/// 会话绑定表上限。
const MAX_SESSION_BINDINGS: usize = 10_000;
/// 同一会话绑定账号连续软失败达到该阈值后，本次请求允许临时 fallback。
pub(super) const MAX_SESSION_SOFT_FAILURES: u32 = 2;

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

pub(super) fn session_binding_snapshot(
    session_bindings: &Mutex<HashMap<String, SessionBinding>>,
    session_id: &str,
) -> Option<SessionBinding> {
    session_bindings.lock().get(session_id).cloned()
}

pub(super) fn cache_redis_binding_if_unchanged(
    session_bindings: &Mutex<HashMap<String, SessionBinding>>,
    session_id: &str,
    expected: &Option<SessionBinding>,
    incoming: Option<SchedulerSessionBinding>,
) -> bool {
    let mut bindings = session_bindings.lock();
    if bindings.get(session_id) != expected.as_ref() {
        return false;
    }
    match incoming {
        Some(binding) => {
            bindings.insert(
                session_id.to_string(),
                SessionBinding {
                    credential_id: binding.credential_id,
                    last_used_at: binding.last_used_at,
                    soft_failure_count: binding.soft_failure_count,
                    redis_persist_pending: false,
                },
            );
        }
        None => {
            bindings.remove(session_id);
        }
    }
    true
}

pub(super) fn cache_redis_binding(
    session_bindings: &Mutex<HashMap<String, SessionBinding>>,
    session_id: &str,
    binding: Option<SchedulerSessionBinding>,
) {
    let mut bindings = session_bindings.lock();
    prune_session_bindings_locked(&mut bindings);
    if bindings
        .get(session_id)
        .is_some_and(|binding| binding.redis_persist_pending)
    {
        return;
    }
    match binding {
        Some(binding) => {
            bindings.insert(
                session_id.to_string(),
                SessionBinding {
                    credential_id: binding.credential_id,
                    last_used_at: binding.last_used_at,
                    soft_failure_count: binding.soft_failure_count,
                    redis_persist_pending: false,
                },
            );
        }
        None => {
            bindings.remove(session_id);
        }
    }
}

pub(super) fn bind_session_to_credential(
    session_bindings: &Mutex<HashMap<String, SessionBinding>>,
    session_id: &str,
    credential_id: u64,
    redis_persist_pending: bool,
) -> SchedulerSessionBinding {
    let mut bindings = session_bindings.lock();
    prune_session_bindings_locked(&mut bindings);
    match bindings.get_mut(session_id) {
        Some(binding) if binding.credential_id == credential_id => {
            binding.last_used_at = Utc::now();
            binding.redis_persist_pending = redis_persist_pending;
        }
        _ => {
            bindings.insert(
                session_id.to_string(),
                SessionBinding {
                    credential_id,
                    last_used_at: Utc::now(),
                    soft_failure_count: 0,
                    redis_persist_pending,
                },
            );
        }
    }

    let binding = bindings
        .get(session_id)
        .expect("newly bound session must remain in the local cache");
    SchedulerSessionBinding {
        credential_id: binding.credential_id,
        last_used_at: binding.last_used_at,
        soft_failure_count: binding.soft_failure_count,
    }
}

pub(super) fn redis_binding_matches_local(
    session_bindings: &Mutex<HashMap<String, SessionBinding>>,
    session_id: &str,
    expected: &SchedulerSessionBinding,
) -> bool {
    session_bindings
        .lock()
        .get(session_id)
        .is_some_and(|binding| {
            binding.credential_id == expected.credential_id
                && binding.last_used_at == expected.last_used_at
                && binding.soft_failure_count == expected.soft_failure_count
                && binding.redis_persist_pending
        })
}

pub(super) fn cache_redis_binding_if_current(
    session_bindings: &Mutex<HashMap<String, SessionBinding>>,
    session_id: &str,
    expected: &SchedulerSessionBinding,
    actual: Option<SchedulerSessionBinding>,
) -> bool {
    let mut bindings = session_bindings.lock();
    let Some(binding) = bindings.get_mut(session_id) else {
        return false;
    };
    if binding.credential_id != expected.credential_id
        || binding.last_used_at != expected.last_used_at
        || binding.soft_failure_count != expected.soft_failure_count
        || !binding.redis_persist_pending
    {
        return false;
    }

    if let Some(actual) = actual {
        binding.credential_id = actual.credential_id;
        binding.last_used_at = actual.last_used_at;
        binding.soft_failure_count = actual.soft_failure_count;
        binding.redis_persist_pending = false;
    } else {
        bindings.remove(session_id);
    }
    true
}

pub(super) fn clear_redis_persist_pending_if_current(
    session_bindings: &Mutex<HashMap<String, SessionBinding>>,
    session_id: &str,
    expected: &SchedulerSessionBinding,
) -> bool {
    let mut bindings = session_bindings.lock();
    let Some(binding) = bindings.get_mut(session_id) else {
        return false;
    };
    if binding.credential_id != expected.credential_id
        || binding.last_used_at != expected.last_used_at
        || binding.soft_failure_count != expected.soft_failure_count
    {
        return false;
    }
    binding.redis_persist_pending = false;
    true
}

pub(super) fn unbind_session_if_bound_to(
    session_bindings: &Mutex<HashMap<String, SessionBinding>>,
    session_id: &str,
    credential_id: u64,
) {
    let mut bindings = session_bindings.lock();
    if bindings
        .get(session_id)
        .is_some_and(|binding| binding.credential_id == credential_id)
    {
        bindings.remove(session_id);
    }
}

pub(super) fn unbind_sessions_for_credential(
    session_bindings: &Mutex<HashMap<String, SessionBinding>>,
    credential_id: u64,
) {
    session_bindings
        .lock()
        .retain(|_, binding| binding.credential_id != credential_id);
}

pub(super) fn record_session_soft_failure(
    session_bindings: &Mutex<HashMap<String, SessionBinding>>,
    session_id: &str,
    credential_id: u64,
) -> bool {
    let mut bindings = session_bindings.lock();
    if let Some(binding) = bindings.get_mut(session_id) {
        if binding.credential_id == credential_id {
            binding.last_used_at = Utc::now();
            binding.soft_failure_count = binding.soft_failure_count.saturating_add(1);
            binding.redis_persist_pending = false;
            binding.soft_failure_count >= MAX_SESSION_SOFT_FAILURES
        } else {
            false
        }
    } else {
        false
    }
}

pub(super) fn clear_session_soft_failure(
    session_bindings: &Mutex<HashMap<String, SessionBinding>>,
    session_id: &str,
    credential_id: u64,
) {
    let mut bindings = session_bindings.lock();
    if let Some(binding) = bindings.get_mut(session_id) {
        if binding.credential_id == credential_id {
            binding.last_used_at = Utc::now();
            binding.soft_failure_count = 0;
            binding.redis_persist_pending = false;
        }
    }
}
