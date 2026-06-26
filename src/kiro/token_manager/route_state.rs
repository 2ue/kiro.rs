use serde::Serialize;
use std::time::Instant;

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
