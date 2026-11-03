use chrono::Utc;
use parking_lot::Mutex;

use std::collections::HashMap;

use crate::storage::redis_cache::SchedulerSessionBinding;

use super::account_state::SessionBinding;

/// 会话绑定最长保留时间，避免长期运行进程无限增长。
pub(super) const SESSION_BINDING_TTL_SECS: i64 = 6 * 60 * 60;
/// 会话绑定表上限。
const MAX_SESSION_BINDINGS: usize = 10_000;
/// 过期会话绑定的清理间隔（秒）。
const SESSION_BINDING_PRUNE_INTERVAL_SECS: i64 = 60;
/// 同一会话绑定账号连续软失败达到该阈值后，本次请求允许临时 fallback。
pub(super) const MAX_SESSION_SOFT_FAILURES: u32 = 2;

/// Prunes expired bindings and enforces [`MAX_SESSION_BINDINGS`].
///
/// Called on every binding insert, which every sticky request does. Walking all bindings (up to
/// 10 000) each time is an O(n) step under this lock per request; expiry is on a scale of hours,
/// so the TTL sweep runs at most once per [`SESSION_BINDING_PRUNE_INTERVAL_SECS`] unless the
/// table is over its size limit.
pub(super) fn prune_session_bindings_locked(bindings: &mut HashMap<String, SessionBinding>) {
    use std::sync::atomic::{AtomicI64, Ordering};
    static LAST_PRUNE_AT: AtomicI64 = AtomicI64::new(i64::MIN);
    let now = Utc::now();
    let now_secs = now.timestamp();
    let last = LAST_PRUNE_AT.load(Ordering::Relaxed);
    if bindings.len() <= MAX_SESSION_BINDINGS
        && now_secs.saturating_sub(last) < SESSION_BINDING_PRUNE_INTERVAL_SECS
    {
        return;
    }
    LAST_PRUNE_AT.store(now_secs, Ordering::Relaxed);
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

pub(super) fn cache_redis_binding(
    session_bindings: &Mutex<HashMap<String, SessionBinding>>,
    session_id: &str,
    binding: Option<SchedulerSessionBinding>,
) {
    let mut bindings = session_bindings.lock();
    prune_session_bindings_locked(&mut bindings);
    match binding {
        Some(binding) => {
            bindings.insert(
                session_id.to_string(),
                SessionBinding {
                    credential_id: binding.credential_id,
                    last_used_at: binding.last_used_at,
                    soft_failure_count: binding.soft_failure_count,
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
) {
    let mut bindings = session_bindings.lock();
    prune_session_bindings_locked(&mut bindings);
    match bindings.get_mut(session_id) {
        Some(binding) if binding.credential_id == credential_id => {
            binding.last_used_at = Utc::now();
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
        }
    }
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
        }
    }
}
