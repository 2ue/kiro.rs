use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration as StdDuration, Instant};

use parking_lot::Mutex;
use reqwest::Client;
use tokio::sync::OnceCell;

use crate::http_client::{ProxyConfig, build_client};
use crate::model::config::{
    Config, MAX_TOKEN_REFRESH_BURST, MAX_TOKEN_REFRESH_MAX_RPM, MIN_TOKEN_REFRESH_BURST,
    MIN_TOKEN_REFRESH_MAX_RPM, TlsBackend,
};
use crate::storage::redis_cache::{RedisStore, TokenRefreshBucketDecision};

const REFRESH_CLIENT_TIMEOUT_SECS: u64 = 60;
const REFRESH_CLIENT_CACHE_MAX_ENTRIES: usize = 256;
const TOKEN_REFRESH_REDIS_ADMISSION_TIMEOUT: StdDuration = StdDuration::from_millis(250);
const TOKEN_REFRESH_COORDINATION_RETRY_AFTER: StdDuration = StdDuration::from_secs(1);
const TOKEN_REFRESH_BUCKET_TOKEN_UNITS: u64 = 60_000;
const TOKEN_REFRESH_REDIS_BACKOFF_MAX: StdDuration = StdDuration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuxiliaryConcurrencyKind {
    TokenRefresh,
    ProfileDiscovery,
    ModelDiscovery,
}

impl AuxiliaryConcurrencyKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::TokenRefresh => "token_refresh",
            Self::ProfileDiscovery => "profile_discovery",
            Self::ModelDiscovery => "model_discovery",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AuxiliaryConcurrencySaturated {
    pub(crate) kind: AuxiliaryConcurrencyKind,
    pub(crate) limit: u32,
    pub(crate) in_flight: u32,
}

impl std::fmt::Display for AuxiliaryConcurrencySaturated {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "auxiliary upstream concurrency saturated for {} ({}/{})",
            self.kind.as_str(),
            self.in_flight,
            self.limit
        )
    }
}

impl std::error::Error for AuxiliaryConcurrencySaturated {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AuxiliaryConcurrencySnapshot {
    pub(crate) limit: u32,
    pub(crate) in_flight: u32,
    pub(crate) peak_in_flight: u32,
    pub(crate) rejected: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenRefreshAdmissionAuthority {
    ProcessLocal,
    RedisGlobal,
    RedisGlobalDegraded,
}

impl TokenRefreshAdmissionAuthority {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ProcessLocal => "process_local",
            Self::RedisGlobal => "redis_global",
            Self::RedisGlobalDegraded => "redis_global_degraded",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenRefreshAdmissionRejectionKind {
    RateLimited,
    CoordinationUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TokenRefreshAdmissionRejected {
    pub(crate) kind: TokenRefreshAdmissionRejectionKind,
    pub(crate) authority: TokenRefreshAdmissionAuthority,
    pub(crate) retry_after: StdDuration,
}

impl std::fmt::Display for TokenRefreshAdmissionRejected {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self.kind {
            TokenRefreshAdmissionRejectionKind::RateLimited => "rate_limited",
            TokenRefreshAdmissionRejectionKind::CoordinationUnavailable => {
                "coordination_unavailable"
            }
        };
        write!(
            formatter,
            "token refresh admission rejected: kind={} authority={} retry_after_ms={}",
            kind,
            self.authority.as_str(),
            self.retry_after.as_millis()
        )
    }
}

impl std::error::Error for TokenRefreshAdmissionRejected {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TokenRefreshAdmissionReceipt {
    pub(crate) authority: TokenRefreshAdmissionAuthority,
    pub(crate) remaining_milli_tokens: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TokenRefreshAdmissionSnapshot {
    pub(crate) authority: TokenRefreshAdmissionAuthority,
    pub(crate) breaker_state: &'static str,
    pub(crate) next_probe_after_ms: u64,
    pub(crate) configured_rpm: u32,
    pub(crate) configured_burst: u32,
    pub(crate) admitted: u64,
    pub(crate) rate_limited: u64,
    pub(crate) coordination_rejected: u64,
    pub(crate) redis_errors: u64,
    pub(crate) last_retry_after_ms: u64,
    pub(crate) remaining_milli_tokens: u64,
}

#[derive(Debug, Clone, Copy)]
enum TokenRefreshRedisAdmissionPhase {
    Closed,
    Open { retry_at: Instant },
    HalfOpen,
}

#[derive(Debug)]
struct TokenRefreshRedisAdmissionState {
    generation: u64,
    consecutive_failures: u8,
    phase: TokenRefreshRedisAdmissionPhase,
}

impl Default for TokenRefreshRedisAdmissionState {
    fn default() -> Self {
        Self {
            generation: 0,
            consecutive_failures: 0,
            phase: TokenRefreshRedisAdmissionPhase::Closed,
        }
    }
}

#[derive(Debug)]
struct LocalTokenRefreshBucket {
    units: u64,
    last_refill: Instant,
}

impl LocalTokenRefreshBucket {
    fn new(burst: u32) -> Self {
        Self {
            units: u64::from(burst).saturating_mul(TOKEN_REFRESH_BUCKET_TOKEN_UNITS),
            last_refill: Instant::now(),
        }
    }

    fn reserve(&mut self, now: Instant, max_rpm: u32, burst: u32) -> TokenRefreshBucketDecision {
        let capacity = u64::from(burst).saturating_mul(TOKEN_REFRESH_BUCKET_TOKEN_UNITS);
        let elapsed_ms = now
            .saturating_duration_since(self.last_refill)
            .as_millis()
            .min(86_400_000) as u64;
        self.units = self
            .units
            .saturating_add(elapsed_ms.saturating_mul(u64::from(max_rpm)))
            .min(capacity);
        self.last_refill = now;
        if self.units >= TOKEN_REFRESH_BUCKET_TOKEN_UNITS {
            self.units -= TOKEN_REFRESH_BUCKET_TOKEN_UNITS;
            TokenRefreshBucketDecision {
                admitted: true,
                retry_after: None,
                remaining_milli_tokens: self.units / (TOKEN_REFRESH_BUCKET_TOKEN_UNITS / 1_000),
            }
        } else {
            let missing = TOKEN_REFRESH_BUCKET_TOKEN_UNITS - self.units;
            let rpm = u64::from(max_rpm.max(1));
            let retry_after_ms = missing.saturating_add(rpm - 1) / rpm;
            TokenRefreshBucketDecision {
                admitted: false,
                retry_after: Some(StdDuration::from_millis(retry_after_ms.max(1))),
                remaining_milli_tokens: self.units / (TOKEN_REFRESH_BUCKET_TOKEN_UNITS / 1_000),
            }
        }
    }

    fn reconfigure(&mut self, burst: u32) {
        self.units = self
            .units
            .min(u64::from(burst).saturating_mul(TOKEN_REFRESH_BUCKET_TOKEN_UNITS));
        // Do not apply elapsed time from the old configuration at the new refill rate.
        self.last_refill = Instant::now();
    }
}

fn pack_token_refresh_limits(max_rpm: u32, burst: u32) -> u64 {
    (u64::from(max_rpm) << 32) | u64::from(burst)
}

fn unpack_token_refresh_limits(packed: u64) -> (u32, u32) {
    ((packed >> 32) as u32, packed as u32)
}

pub(crate) struct TokenRefreshAdmissionController {
    limits: AtomicU64,
    local: Mutex<LocalTokenRefreshBucket>,
    redis_store: Mutex<Option<Arc<RedisStore>>>,
    redis_state: Mutex<TokenRefreshRedisAdmissionState>,
    redis_degraded: AtomicBool,
    admitted: AtomicU64,
    rate_limited: AtomicU64,
    coordination_rejected: AtomicU64,
    redis_errors: AtomicU64,
    last_retry_after_ms: AtomicU64,
    remaining_milli_tokens: AtomicU64,
}

impl TokenRefreshAdmissionController {
    fn new(max_rpm: u32, burst: u32) -> Arc<Self> {
        debug_assert!((MIN_TOKEN_REFRESH_MAX_RPM..=MAX_TOKEN_REFRESH_MAX_RPM).contains(&max_rpm));
        debug_assert!((MIN_TOKEN_REFRESH_BURST..=MAX_TOKEN_REFRESH_BURST).contains(&burst));
        Arc::new(Self {
            limits: AtomicU64::new(pack_token_refresh_limits(max_rpm, burst)),
            local: Mutex::new(LocalTokenRefreshBucket::new(burst)),
            redis_store: Mutex::new(None),
            redis_state: Mutex::new(TokenRefreshRedisAdmissionState::default()),
            redis_degraded: AtomicBool::new(false),
            admitted: AtomicU64::new(0),
            rate_limited: AtomicU64::new(0),
            coordination_rejected: AtomicU64::new(0),
            redis_errors: AtomicU64::new(0),
            last_retry_after_ms: AtomicU64::new(0),
            remaining_milli_tokens: AtomicU64::new(u64::from(burst) * 1_000),
        })
    }

    fn set_redis_store(&self, redis_store: Option<Arc<RedisStore>>) {
        *self.redis_store.lock() = redis_store;
        let mut state = self.redis_state.lock();
        state.generation = state.generation.wrapping_add(1);
        state.consecutive_failures = 0;
        state.phase = TokenRefreshRedisAdmissionPhase::Closed;
        self.redis_degraded.store(false, Ordering::Release);
    }

    fn update_limits(&self, max_rpm: u32, burst: u32) {
        debug_assert!((MIN_TOKEN_REFRESH_MAX_RPM..=MAX_TOKEN_REFRESH_MAX_RPM).contains(&max_rpm));
        debug_assert!((MIN_TOKEN_REFRESH_BURST..=MAX_TOKEN_REFRESH_BURST).contains(&burst));
        let mut local = self.local.lock();
        local.reconfigure(burst);
        self.limits
            .store(pack_token_refresh_limits(max_rpm, burst), Ordering::Release);
        drop(local);
        let mut state = self.redis_state.lock();
        state.generation = state.generation.wrapping_add(1);
        state.consecutive_failures = 0;
        state.phase = TokenRefreshRedisAdmissionPhase::Closed;
        self.redis_degraded.store(false, Ordering::Release);
    }

    fn limits_snapshot(&self) -> (u32, u32, u64) {
        let packed = self.limits.load(Ordering::Acquire);
        let (max_rpm, burst) = unpack_token_refresh_limits(packed);
        // This deterministic value is shared by every instance with the same config. A
        // process-local update counter would make peers continually reset one Redis bucket.
        (max_rpm, burst, packed)
    }

    fn begin_redis_attempt(&self) -> Result<u64, StdDuration> {
        let now = Instant::now();
        let mut state = self.redis_state.lock();
        match state.phase {
            TokenRefreshRedisAdmissionPhase::Closed => Ok(state.generation),
            TokenRefreshRedisAdmissionPhase::Open { retry_at } if now < retry_at => Err(retry_at
                .saturating_duration_since(now)
                .max(StdDuration::from_millis(1))),
            TokenRefreshRedisAdmissionPhase::Open { .. } => {
                state.phase = TokenRefreshRedisAdmissionPhase::HalfOpen;
                Ok(state.generation)
            }
            TokenRefreshRedisAdmissionPhase::HalfOpen => {
                Err(TOKEN_REFRESH_COORDINATION_RETRY_AFTER)
            }
        }
    }

    fn complete_redis_success(&self, generation: u64) {
        let mut state = self.redis_state.lock();
        if state.generation != generation {
            return;
        }
        state.consecutive_failures = 0;
        state.phase = TokenRefreshRedisAdmissionPhase::Closed;
        self.redis_degraded.store(false, Ordering::Release);
    }

    fn complete_redis_failure(&self, generation: u64) -> StdDuration {
        let mut state = self.redis_state.lock();
        if state.generation != generation {
            return TOKEN_REFRESH_COORDINATION_RETRY_AFTER;
        }
        state.generation = state.generation.wrapping_add(1);
        state.consecutive_failures = state.consecutive_failures.saturating_add(1).min(16);
        let shift = u32::from(state.consecutive_failures.saturating_sub(1)).min(5);
        let backoff = TOKEN_REFRESH_COORDINATION_RETRY_AFTER
            .saturating_mul(1_u32 << shift)
            .min(TOKEN_REFRESH_REDIS_BACKOFF_MAX);
        state.phase = TokenRefreshRedisAdmissionPhase::Open {
            retry_at: Instant::now() + backoff,
        };
        self.redis_degraded.store(true, Ordering::Release);
        backoff
    }

    pub(crate) async fn reserve(
        &self,
    ) -> Result<TokenRefreshAdmissionReceipt, TokenRefreshAdmissionRejected> {
        let redis_store = self.redis_store.lock().clone();
        let (authority, decision) = if let Some(redis) = redis_store {
            let (max_rpm, burst, limits_fingerprint) = self.limits_snapshot();
            let redis_generation = self.begin_redis_attempt().map_err(|retry_after| {
                self.coordination_rejected.fetch_add(1, Ordering::Relaxed);
                self.last_retry_after_ms
                    .store(retry_after.as_millis() as u64, Ordering::Release);
                TokenRefreshAdmissionRejected {
                    kind: TokenRefreshAdmissionRejectionKind::CoordinationUnavailable,
                    authority: TokenRefreshAdmissionAuthority::RedisGlobalDegraded,
                    retry_after,
                }
            })?;
            match tokio::time::timeout(
                TOKEN_REFRESH_REDIS_ADMISSION_TIMEOUT,
                redis.reserve_token_refresh_send(max_rpm, burst, limits_fingerprint),
            )
            .await
            {
                Ok(Ok(decision)) => {
                    self.complete_redis_success(redis_generation);
                    (TokenRefreshAdmissionAuthority::RedisGlobal, decision)
                }
                Ok(Err(_)) | Err(_) => {
                    let retry_after = self.complete_redis_failure(redis_generation);
                    self.redis_errors.fetch_add(1, Ordering::Relaxed);
                    self.coordination_rejected.fetch_add(1, Ordering::Relaxed);
                    self.last_retry_after_ms
                        .store(retry_after.as_millis() as u64, Ordering::Release);
                    return Err(TokenRefreshAdmissionRejected {
                        kind: TokenRefreshAdmissionRejectionKind::CoordinationUnavailable,
                        authority: TokenRefreshAdmissionAuthority::RedisGlobalDegraded,
                        retry_after,
                    });
                }
            }
        } else {
            let mut local = self.local.lock();
            let (max_rpm, burst, _) = self.limits_snapshot();
            let decision = local.reserve(Instant::now(), max_rpm, burst);
            (TokenRefreshAdmissionAuthority::ProcessLocal, decision)
        };
        self.remaining_milli_tokens
            .store(decision.remaining_milli_tokens, Ordering::Release);
        if decision.admitted {
            self.admitted.fetch_add(1, Ordering::Relaxed);
            self.last_retry_after_ms.store(0, Ordering::Release);
            Ok(TokenRefreshAdmissionReceipt {
                authority,
                remaining_milli_tokens: decision.remaining_milli_tokens,
            })
        } else {
            let retry_after = decision.retry_after.unwrap_or(StdDuration::from_millis(1));
            self.rate_limited.fetch_add(1, Ordering::Relaxed);
            self.last_retry_after_ms
                .store(retry_after.as_millis() as u64, Ordering::Release);
            Err(TokenRefreshAdmissionRejected {
                kind: TokenRefreshAdmissionRejectionKind::RateLimited,
                authority,
                retry_after,
            })
        }
    }

    fn snapshot(&self) -> TokenRefreshAdmissionSnapshot {
        let (configured_rpm, configured_burst, _) = self.limits_snapshot();
        let now = Instant::now();
        let (breaker_state, next_probe_after_ms) = match self.redis_state.lock().phase {
            TokenRefreshRedisAdmissionPhase::Closed => ("closed", 0),
            TokenRefreshRedisAdmissionPhase::HalfOpen => ("half_open", 0),
            TokenRefreshRedisAdmissionPhase::Open { retry_at } => (
                "open",
                retry_at.saturating_duration_since(now).as_millis() as u64,
            ),
        };
        let has_redis = self.redis_store.lock().is_some();
        let authority = if !has_redis {
            TokenRefreshAdmissionAuthority::ProcessLocal
        } else if self.redis_degraded.load(Ordering::Acquire) {
            TokenRefreshAdmissionAuthority::RedisGlobalDegraded
        } else {
            TokenRefreshAdmissionAuthority::RedisGlobal
        };
        TokenRefreshAdmissionSnapshot {
            authority,
            breaker_state,
            next_probe_after_ms,
            configured_rpm,
            configured_burst,
            admitted: self.admitted.load(Ordering::Acquire),
            rate_limited: self.rate_limited.load(Ordering::Acquire),
            coordination_rejected: self.coordination_rejected.load(Ordering::Acquire),
            redis_errors: self.redis_errors.load(Ordering::Acquire),
            last_retry_after_ms: self.last_retry_after_ms.load(Ordering::Acquire),
            remaining_milli_tokens: self.remaining_milli_tokens.load(Ordering::Acquire),
        }
    }
}

#[derive(Debug)]
pub(crate) struct AuxiliaryConcurrencyController {
    limit: AtomicU32,
    in_flight: AtomicU32,
    peak_in_flight: AtomicU32,
    rejected: AtomicU64,
}

impl AuxiliaryConcurrencyController {
    fn new(limit: u32) -> Arc<Self> {
        Arc::new(Self {
            limit: AtomicU32::new(limit.max(1)),
            in_flight: AtomicU32::new(0),
            peak_in_flight: AtomicU32::new(0),
            rejected: AtomicU64::new(0),
        })
    }

    pub(super) fn update_limit(&self, limit: u32) {
        self.limit.store(limit.max(1), Ordering::Release);
    }

    pub(crate) fn try_acquire(
        self: &Arc<Self>,
        kind: AuxiliaryConcurrencyKind,
    ) -> Result<AuxiliaryConcurrencyPermit, AuxiliaryConcurrencySaturated> {
        let acquired =
            self.in_flight
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |in_flight| {
                    let limit = self.limit.load(Ordering::Acquire);
                    (in_flight < limit).then_some(in_flight + 1)
                });
        match acquired {
            Ok(previous) => {
                let in_flight = previous + 1;
                self.peak_in_flight.fetch_max(in_flight, Ordering::Relaxed);
                Ok(AuxiliaryConcurrencyPermit {
                    controller: self.clone(),
                })
            }
            Err(in_flight) => {
                self.rejected.fetch_add(1, Ordering::Relaxed);
                Err(AuxiliaryConcurrencySaturated {
                    kind,
                    limit: self.limit.load(Ordering::Acquire),
                    in_flight,
                })
            }
        }
    }

    pub(super) fn snapshot(&self) -> AuxiliaryConcurrencySnapshot {
        AuxiliaryConcurrencySnapshot {
            limit: self.limit.load(Ordering::Acquire),
            in_flight: self.in_flight.load(Ordering::Acquire),
            peak_in_flight: self.peak_in_flight.load(Ordering::Acquire),
            rejected: self.rejected.load(Ordering::Acquire),
        }
    }
}

pub(crate) struct AuxiliaryConcurrencyPermit {
    controller: Arc<AuxiliaryConcurrencyController>,
}

impl Drop for AuxiliaryConcurrencyPermit {
    fn drop(&mut self) {
        self.controller.in_flight.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct RefreshClientKey {
    tls_backend: TlsBackend,
    proxy: Option<ProxyConfig>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RefreshClientCacheSnapshot {
    pub(crate) entries: usize,
    pub(crate) max_entries: usize,
    pub(crate) builds: u64,
    pub(crate) hits: u64,
    pub(crate) misses: u64,
    pub(crate) saturated: u64,
}

struct RefreshClientCache {
    entries: Mutex<HashMap<RefreshClientKey, Arc<OnceCell<Arc<Client>>>>>,
    builds: AtomicU64,
    hits: AtomicU64,
    misses: AtomicU64,
    saturated: AtomicU64,
}

impl RefreshClientCache {
    fn new(
        config: &Config,
        global_proxy: Option<&ProxyConfig>,
        prewarm: bool,
    ) -> anyhow::Result<Self> {
        let cache = Self {
            entries: Mutex::new(HashMap::new()),
            builds: AtomicU64::new(0),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            saturated: AtomicU64::new(0),
        };
        if prewarm {
            let key = RefreshClientKey {
                tls_backend: config.tls_backend,
                proxy: global_proxy.cloned(),
            };
            let client = Arc::new(build_client(
                key.proxy.as_ref(),
                REFRESH_CLIENT_TIMEOUT_SECS,
                key.tls_backend,
            )?);
            let cell = Arc::new(OnceCell::new());
            cell.set(client)
                .map_err(|_| anyhow::anyhow!("refresh client prewarm cell already initialized"))?;
            cache.entries.lock().insert(key, cell);
            cache.builds.store(1, Ordering::Release);
            cache.misses.store(1, Ordering::Release);
        }
        Ok(cache)
    }

    async fn client(
        &self,
        tls_backend: TlsBackend,
        proxy: Option<&ProxyConfig>,
    ) -> anyhow::Result<Arc<Client>> {
        let key = RefreshClientKey {
            tls_backend,
            proxy: proxy.cloned(),
        };
        let (cell, cached) = {
            let mut entries = self.entries.lock();
            if let Some(cell) = entries.get(&key) {
                (cell.clone(), true)
            } else if entries.len() < REFRESH_CLIENT_CACHE_MAX_ENTRIES {
                let cell = Arc::new(OnceCell::new());
                entries.insert(key.clone(), cell.clone());
                (cell, false)
            } else {
                self.saturated.fetch_add(1, Ordering::Relaxed);
                // Once a cell has no lookup/build waiters, removing it is safe: callers retain
                // their own Arc<Client>. Replacing an idle entry keeps the cache bounded without
                // rebuilding the same overflow key on every later refresh.
                let evicted_key = entries
                    .iter()
                    .find(|(_, cell)| Arc::strong_count(cell) == 1)
                    .map(|(key, _)| key.clone())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "refresh client cache is temporarily saturated with active lookups"
                        )
                    })?;
                entries.remove(&evicted_key);
                let cell = Arc::new(OnceCell::new());
                entries.insert(key.clone(), cell.clone());
                (cell, false)
            }
        };
        if cached {
            self.hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
        }

        let client = cell
            .get_or_try_init(|| async {
                self.builds.fetch_add(1, Ordering::Relaxed);
                let proxy = key.proxy.clone();
                tokio::task::spawn_blocking(move || {
                    build_client(proxy.as_ref(), REFRESH_CLIENT_TIMEOUT_SECS, key.tls_backend)
                        .map(Arc::new)
                })
                .await
                .map_err(|err| anyhow::anyhow!("refresh client build task failed: {err}"))?
            })
            .await?;
        Ok(client.clone())
    }

    fn snapshot(&self) -> RefreshClientCacheSnapshot {
        RefreshClientCacheSnapshot {
            entries: self.entries.lock().len(),
            max_entries: REFRESH_CLIENT_CACHE_MAX_ENTRIES,
            builds: self.builds.load(Ordering::Acquire),
            hits: self.hits.load(Ordering::Acquire),
            misses: self.misses.load(Ordering::Acquire),
            saturated: self.saturated.load(Ordering::Acquire),
        }
    }
}

pub(super) struct AuxiliaryRuntime {
    controller: Arc<AuxiliaryConcurrencyController>,
    token_refresh_admission: Arc<TokenRefreshAdmissionController>,
    refresh_clients: RefreshClientCache,
}

impl AuxiliaryRuntime {
    pub(super) fn new(
        config: &Config,
        global_proxy: Option<&ProxyConfig>,
        prewarm_refresh_client: bool,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            controller: AuxiliaryConcurrencyController::new(
                config.auxiliary_upstream_max_concurrent_requests,
            ),
            token_refresh_admission: TokenRefreshAdmissionController::new(
                config.token_refresh_max_rpm,
                config.token_refresh_burst,
            ),
            refresh_clients: RefreshClientCache::new(config, global_proxy, prewarm_refresh_client)?,
        })
    }

    pub(super) fn controller(&self) -> Arc<AuxiliaryConcurrencyController> {
        self.controller.clone()
    }

    pub(super) fn update_limit(&self, limit: u32) {
        self.controller.update_limit(limit);
    }

    pub(super) fn set_token_refresh_redis_store(&self, redis_store: Option<Arc<RedisStore>>) {
        self.token_refresh_admission.set_redis_store(redis_store);
    }

    pub(super) fn update_token_refresh_limits(&self, max_rpm: u32, burst: u32) {
        self.token_refresh_admission.update_limits(max_rpm, burst);
    }

    pub(super) fn token_refresh_admission_controller(
        &self,
    ) -> Arc<TokenRefreshAdmissionController> {
        self.token_refresh_admission.clone()
    }

    pub(super) fn token_refresh_admission_snapshot(&self) -> TokenRefreshAdmissionSnapshot {
        self.token_refresh_admission.snapshot()
    }

    pub(super) async fn refresh_client(
        &self,
        tls_backend: TlsBackend,
        proxy: Option<&ProxyConfig>,
    ) -> anyhow::Result<Arc<Client>> {
        self.refresh_clients.client(tls_backend, proxy).await
    }

    pub(super) fn concurrency_snapshot(&self) -> AuxiliaryConcurrencySnapshot {
        self.controller.snapshot()
    }

    pub(super) fn refresh_client_cache_snapshot(&self) -> RefreshClientCacheSnapshot {
        self.refresh_clients.snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refresh_test_tls_backend() -> TlsBackend {
        #[cfg(feature = "native-tls")]
        {
            TlsBackend::NativeTls
        }
        #[cfg(not(feature = "native-tls"))]
        {
            TlsBackend::Rustls
        }
    }

    #[test]
    fn token_refresh_local_bucket_refill_is_integer_and_stable_for_five_rounds() {
        for _ in 0..5 {
            let mut bucket = LocalTokenRefreshBucket::new(1);
            let now = Instant::now();
            assert!(bucket.reserve(now, 60, 1).admitted);
            let rejected = bucket.reserve(now, 60, 1);
            assert!(!rejected.admitted);
            assert_eq!(rejected.retry_after, Some(StdDuration::from_secs(1)));
            assert!(
                bucket
                    .reserve(now + StdDuration::from_secs(1), 60, 1)
                    .admitted
            );
        }
    }

    #[tokio::test]
    async fn token_refresh_process_local_burst_is_hard_bounded_for_five_rounds() {
        for _ in 0..5 {
            let controller = TokenRefreshAdmissionController::new(60, 8);
            let mut admitted = 0;
            let mut rejected = 0;
            for _ in 0..128 {
                match controller.reserve().await {
                    Ok(receipt) => {
                        admitted += 1;
                        assert_eq!(
                            receipt.authority,
                            TokenRefreshAdmissionAuthority::ProcessLocal
                        );
                    }
                    Err(error) => {
                        rejected += 1;
                        assert_eq!(error.kind, TokenRefreshAdmissionRejectionKind::RateLimited);
                        assert!(error.retry_after <= StdDuration::from_secs(1));
                    }
                }
            }
            assert_eq!(admitted, 8);
            assert_eq!(rejected, 120);
            let snapshot = controller.snapshot();
            assert_eq!(snapshot.admitted, 8);
            assert_eq!(snapshot.rate_limited, 120);
            assert_eq!(snapshot.coordination_rejected, 0);
            assert_eq!(snapshot.redis_errors, 0);
        }
    }

    #[test]
    fn token_refresh_limit_updates_are_atomic_and_do_not_retroactively_refill_for_five_rounds() {
        for _ in 0..5 {
            let controller = TokenRefreshAdmissionController::new(120, 16);
            {
                let mut local = controller.local.lock();
                local.units = 0;
                local.last_refill = Instant::now() - StdDuration::from_secs(60);
            }
            controller.update_limits(60, 8);
            let snapshot = controller.snapshot();
            assert_eq!(snapshot.configured_rpm, 60);
            assert_eq!(snapshot.configured_burst, 8);
            assert_eq!(controller.local.lock().units, 0);
            let rejected = controller.local.lock().reserve(
                Instant::now(),
                snapshot.configured_rpm,
                snapshot.configured_burst,
            );
            assert!(!rejected.admitted);
            assert_eq!(
                controller.limits.load(Ordering::Acquire),
                pack_token_refresh_limits(60, 8)
            );
        }
    }

    #[test]
    fn token_refresh_redis_breaker_has_one_half_open_probe_and_stale_success_is_fenced() {
        for _ in 0..5 {
            let controller = TokenRefreshAdmissionController::new(60, 8);
            assert_eq!(controller.begin_redis_attempt(), Ok(0));
            assert_eq!(
                controller.complete_redis_failure(0),
                StdDuration::from_secs(1)
            );
            controller.complete_redis_success(0);
            assert!(controller.redis_degraded.load(Ordering::Acquire));
            {
                let mut state = controller.redis_state.lock();
                state.phase = TokenRefreshRedisAdmissionPhase::Open {
                    retry_at: Instant::now(),
                };
            }
            let mut probes = 0;
            for _ in 0..32 {
                if controller.begin_redis_attempt().is_ok() {
                    probes += 1;
                }
            }
            assert_eq!(probes, 1);
        }
    }

    #[test]
    fn token_refresh_limit_fingerprint_is_deterministic_across_instances() {
        for _ in 0..5 {
            let first = TokenRefreshAdmissionController::new(120, 16);
            let second = TokenRefreshAdmissionController::new(120, 16);
            assert_eq!(first.limits_snapshot(), second.limits_snapshot());
            assert_eq!(
                first.limits_snapshot().2,
                (u64::from(120_u32) << 32) | u64::from(16_u32)
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn token_refresh_local_reconfigure_and_reserve_keep_one_atomic_limit_snapshot() {
        for round in 1..=5 {
            let controller = TokenRefreshAdmissionController::new(60, 8);
            let mut tasks = Vec::new();
            for index in 0..32 {
                let controller = controller.clone();
                tasks.push(tokio::spawn(async move {
                    if index % 2 == 0 {
                        controller.update_limits(120, 16);
                    } else {
                        controller.update_limits(60, 8);
                    }
                    let _ = controller.reserve().await;
                }));
            }
            for task in tasks {
                task.await.expect("local admission task");
            }
            let (_, burst, _) = controller.limits_snapshot();
            assert!(matches!(burst, 8 | 16), "round {round}");
            assert!(
                controller.local.lock().units
                    <= u64::from(burst) * TOKEN_REFRESH_BUCKET_TOKEN_UNITS,
                "round {round}"
            );
        }
    }

    #[test]
    fn token_refresh_rejection_display_is_low_cardinality_for_five_rounds() {
        for _ in 0..5 {
            let error = TokenRefreshAdmissionRejected {
                kind: TokenRefreshAdmissionRejectionKind::CoordinationUnavailable,
                authority: TokenRefreshAdmissionAuthority::RedisGlobalDegraded,
                retry_after: StdDuration::from_secs(1),
            };
            assert_eq!(
                error.to_string(),
                "token refresh admission rejected: kind=coordination_unavailable authority=redis_global_degraded retry_after_ms=1000"
            );
        }
    }

    #[test]
    fn auxiliary_focus_controller_is_fail_fast_dynamic_and_drop_safe_for_five_rounds() {
        for _ in 0..5 {
            let controller = AuxiliaryConcurrencyController::new(2);
            let first = controller
                .try_acquire(AuxiliaryConcurrencyKind::TokenRefresh)
                .unwrap();
            let second = controller
                .try_acquire(AuxiliaryConcurrencyKind::ProfileDiscovery)
                .unwrap();
            assert!(matches!(
                controller.try_acquire(AuxiliaryConcurrencyKind::TokenRefresh),
                Err(AuxiliaryConcurrencySaturated {
                    limit: 2,
                    in_flight: 2,
                    ..
                })
            ));
            controller.update_limit(1);
            drop(first);
            assert!(
                controller
                    .try_acquire(AuxiliaryConcurrencyKind::TokenRefresh)
                    .is_err()
            );
            drop(second);
            let recovered = controller
                .try_acquire(AuxiliaryConcurrencyKind::TokenRefresh)
                .unwrap();
            assert_eq!(controller.snapshot().in_flight, 1);
            drop(recovered);
            let snapshot = controller.snapshot();
            assert_eq!(snapshot.limit, 1);
            assert_eq!(snapshot.in_flight, 0);
            assert_eq!(snapshot.peak_in_flight, 2);
            assert_eq!(snapshot.rejected, 2);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn auxiliary_focus_same_refresh_client_key_builds_once_under_concurrency_for_five_rounds()
    {
        for _ in 0..5 {
            let mut config = Config::default();
            config.tls_backend = refresh_test_tls_backend();
            let runtime = Arc::new(AuxiliaryRuntime::new(&config, None, false).unwrap());
            let barrier = Arc::new(tokio::sync::Barrier::new(32));
            let tasks = (0..32)
                .map(|_| {
                    let runtime = runtime.clone();
                    let barrier = barrier.clone();
                    tokio::spawn(async move {
                        barrier.wait().await;
                        runtime
                            .refresh_client(refresh_test_tls_backend(), None)
                            .await
                    })
                })
                .collect::<Vec<_>>();
            for task in tasks {
                task.await.unwrap().unwrap();
            }
            let snapshot = runtime.refresh_client_cache_snapshot();
            assert_eq!(snapshot.entries, 1);
            assert_eq!(snapshot.builds, 1);
            assert_eq!(snapshot.misses, 1);
            assert_eq!(snapshot.hits, 31);
            assert_eq!(snapshot.saturated, 0);
        }
    }

    #[tokio::test]
    async fn auxiliary_focus_distinct_proxy_cache_pressure_stays_bounded_and_reuses_overflow_key_for_five_rounds()
     {
        for round in 0..5 {
            let mut config = Config::default();
            config.tls_backend = TlsBackend::Rustls;
            let runtime = AuxiliaryRuntime::new(&config, None, false).unwrap();
            let prebuilt_client = Arc::new(
                build_client(None, REFRESH_CLIENT_TIMEOUT_SECS, TlsBackend::Rustls).unwrap(),
            );
            {
                let mut entries = runtime.refresh_clients.entries.lock();
                for index in 0..REFRESH_CLIENT_CACHE_MAX_ENTRIES {
                    let proxy = ProxyConfig::new(format!(
                        "http://127.0.0.1:{}",
                        10_000 + round * 1_000 + index
                    ));
                    let cell = Arc::new(OnceCell::new());
                    cell.set(prebuilt_client.clone()).unwrap();
                    entries.insert(
                        RefreshClientKey {
                            tls_backend: TlsBackend::Rustls,
                            proxy: Some(proxy),
                        },
                        cell,
                    );
                }
            }
            runtime
                .refresh_clients
                .builds
                .store(REFRESH_CLIENT_CACHE_MAX_ENTRIES as u64, Ordering::Release);
            runtime
                .refresh_clients
                .misses
                .store(REFRESH_CLIENT_CACHE_MAX_ENTRIES as u64, Ordering::Release);

            let overflow_proxy = ProxyConfig::new(format!(
                "http://127.0.0.1:{}",
                10_000 + round * 1_000 + REFRESH_CLIENT_CACHE_MAX_ENTRIES
            ));
            runtime
                .refresh_client(TlsBackend::Rustls, Some(&overflow_proxy))
                .await
                .unwrap();

            let after_pressure = runtime.refresh_client_cache_snapshot();
            assert_eq!(after_pressure.entries, REFRESH_CLIENT_CACHE_MAX_ENTRIES);
            assert_eq!(
                after_pressure.builds,
                (REFRESH_CLIENT_CACHE_MAX_ENTRIES + 1) as u64
            );
            assert_eq!(after_pressure.saturated, 1);

            runtime
                .refresh_client(TlsBackend::Rustls, Some(&overflow_proxy))
                .await
                .unwrap();
            let after_reuse = runtime.refresh_client_cache_snapshot();
            assert_eq!(after_reuse.entries, REFRESH_CLIENT_CACHE_MAX_ENTRIES);
            assert_eq!(after_reuse.builds, after_pressure.builds);
            assert_eq!(after_reuse.hits, after_pressure.hits + 1);
            assert_eq!(after_reuse.saturated, after_pressure.saturated);
        }
    }
}
