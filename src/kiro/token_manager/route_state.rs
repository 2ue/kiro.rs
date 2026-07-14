use parking_lot::Mutex;
use serde::Serialize;

use std::collections::HashMap;
use std::time::{Duration as StdDuration, Instant};

const LOCAL_POOL_ROUTE_STATE_CACHE_TTL: StdDuration = StdDuration::from_millis(250);
const LOCAL_POOL_ROUTE_STATE_CACHE_MAX_KEYS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalPoolRouteStateKind {
    Ready,
    NoCredentials,
    AllDisabled,
    NoModelCompatible,
    ProxyBlocked,
    AllCoolingDown,
    CapacityFull,
    SchedulerRedisDegraded,
}

impl LocalPoolRouteStateKind {
    pub fn should_route_external(self) -> bool {
        !matches!(self, Self::Ready)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalPoolRouteState {
    pub kind: LocalPoolRouteStateKind,
    pub total: usize,
    pub available: usize,
    pub model_usable: usize,
    pub usable: usize,
    pub dispatchable: usize,
    pub proxy_blocked: usize,
    pub cooldown_blocked: usize,
    pub rate_limit_blocked: usize,
    pub concurrency_blocked: usize,
    pub global_in_flight_requests: u32,
    pub global_max_concurrent_requests: u32,
    pub global_credential_max_concurrent_requests: u32,
    pub effective_credential_max_concurrent_requests: String,
    pub queued_requests: u32,
    pub max_queued_requests: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_secs: Option<u64>,
}

#[derive(Debug, Clone)]
pub(super) struct CachedLocalPoolRouteState {
    pub(super) state: LocalPoolRouteState,
    pub(super) expires_at: Instant,
}

pub(super) fn local_pool_route_state_cache_key(model: Option<&str>) -> String {
    model
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
        .unwrap_or_else(|| "*".to_string())
}

pub(super) fn cached_local_pool_route_state(
    cache: &Mutex<HashMap<String, CachedLocalPoolRouteState>>,
    key: &str,
    now: Instant,
) -> Option<LocalPoolRouteState> {
    cache
        .lock()
        .get(key)
        .filter(|cached| cached.expires_at > now)
        .map(|cached| cached.state.clone())
}

pub(super) fn store_local_pool_route_state_cache(
    cache: &Mutex<HashMap<String, CachedLocalPoolRouteState>>,
    key: String,
    state: LocalPoolRouteState,
    now: Instant,
) {
    let mut cache = cache.lock();
    if cache.len() >= LOCAL_POOL_ROUTE_STATE_CACHE_MAX_KEYS && !cache.contains_key(&key) {
        cache.retain(|_, cached| cached.expires_at > now);
        if cache.len() >= LOCAL_POOL_ROUTE_STATE_CACHE_MAX_KEYS {
            cache.clear();
        }
    }
    cache.insert(
        key,
        CachedLocalPoolRouteState {
            state,
            expires_at: now + LOCAL_POOL_ROUTE_STATE_CACHE_TTL,
        },
    );
}

pub(super) fn invalidate_local_pool_route_state_cache(
    cache: &Mutex<HashMap<String, CachedLocalPoolRouteState>>,
) {
    cache.lock().clear();
}
