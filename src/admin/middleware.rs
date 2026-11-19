//! Admin API 中间件

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Json, Response},
};
use parking_lot::{Mutex, RwLock};
use serde_json::json;

use super::service::AdminService;
use super::types::AdminErrorResponse;
use crate::common::auth;
use crate::common::log_throttle::LogThrottle;

/// 认证失败日志：每 10 秒最多一条，附带被压掉的条数。
static ADMIN_AUTH_FAILURE_LOG_THROTTLE: LazyLock<LogThrottle> =
    LazyLock::new(|| LogThrottle::new(Duration::from_secs(10)));

/// 每个来源每分钟最多写这么多条认证失败审计。
const ADMIN_AUTH_FAILURE_AUDITS_PER_MINUTE: u32 = 10;
/// 所有来源合计每分钟最多写这么多条；伪造来源头换 IP 也绕不过去。
const ADMIN_AUTH_FAILURE_AUDITS_PER_MINUTE_GLOBAL: u32 = 60;
const ADMIN_AUTH_FAILURE_WINDOW: Duration = Duration::from_secs(60);
/// 单个窗口里单独计数的来源上限；超出的新来源共用一个溢出桶，表不会无限增长，也不会被清空。
const ADMIN_AUTH_FAILURE_MAX_TRACKED_SOURCES: usize = 1024;
const ADMIN_AUTH_FAILURE_OVERFLOW_SOURCE: &str = "__overflow__";

#[derive(Default)]
struct AuthFailureWindow {
    started_at: Option<Instant>,
    total: u32,
    sources: HashMap<String, u32>,
    /// 被限速压掉、还没写进审计的失败次数；下一条落库的审计带上它。
    suppressed: u64,
}

/// 认证失败审计限速器：固定一分钟窗口，按来源和全局两级计数。
#[derive(Default)]
pub(super) struct AdminAuthFailureAuditLimiter {
    window: Mutex<AuthFailureWindow>,
}

impl AdminAuthFailureAuditLimiter {
    /// 这次失败能否写审计；能写时返回此前被压掉的次数（并清零），不能写返回 `None`。
    pub(super) fn admit(&self, source: &str, now: Instant) -> Option<u64> {
        let mut window = self.window.lock();
        let expired = window.started_at.is_none_or(|started| {
            now.saturating_duration_since(started) >= ADMIN_AUTH_FAILURE_WINDOW
        });
        if expired {
            window.started_at = Some(now);
            window.total = 0;
            window.sources.clear();
        }
        if window.total >= ADMIN_AUTH_FAILURE_AUDITS_PER_MINUTE_GLOBAL {
            window.suppressed = window.suppressed.saturating_add(1);
            return None;
        }
        let key = if window.sources.contains_key(source)
            || window.sources.len() < ADMIN_AUTH_FAILURE_MAX_TRACKED_SOURCES
        {
            source
        } else {
            ADMIN_AUTH_FAILURE_OVERFLOW_SOURCE
        };
        let count = window.sources.entry(key.to_string()).or_insert(0);
        if *count >= ADMIN_AUTH_FAILURE_AUDITS_PER_MINUTE {
            window.suppressed = window.suppressed.saturating_add(1);
            return None;
        }
        *count += 1;
        window.total += 1;
        Some(std::mem::take(&mut window.suppressed))
    }
}

static ADMIN_AUTH_FAILURE_AUDIT_LIMITER: LazyLock<AdminAuthFailureAuditLimiter> =
    LazyLock::new(AdminAuthFailureAuditLimiter::default);

/// 来源 IP：优先 `x-forwarded-for` 第一段，其次 `x-real-ip`，都没有就返回 `None`。
pub(super) fn client_source_ip(headers: &HeaderMap) -> Option<String> {
    let forwarded = headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(ip) = forwarded {
        return Some(ip.chars().take(64).collect());
    }
    headers
        .get("x-real-ip")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(64).collect())
}

/// Admin API 共享状态
#[derive(Clone)]
pub struct AdminState {
    /// Admin API 密钥
    admin_api_key: AdminApiKeyStore,
    /// Admin 服务
    pub service: Arc<AdminService>,
}

#[derive(Clone, Default)]
pub struct AdminApiKeyStore {
    key: Arc<RwLock<String>>,
}

impl AdminApiKeyStore {
    pub fn new(admin_api_key: impl Into<String>) -> Self {
        Self {
            key: Arc::new(RwLock::new(admin_api_key.into())),
        }
    }

    pub fn current(&self) -> String {
        self.key.read().clone()
    }

    pub fn replace(&self, admin_api_key: impl Into<String>) {
        *self.key.write() = admin_api_key.into();
    }
}

impl AdminState {
    #[allow(dead_code)]
    pub fn new(admin_api_key: impl Into<String>, service: AdminService) -> Self {
        Self::with_key_store(AdminApiKeyStore::new(admin_api_key), service)
    }

    pub fn with_key_store(admin_api_key: AdminApiKeyStore, service: AdminService) -> Self {
        Self {
            admin_api_key,
            service: Arc::new(service),
        }
    }

    pub fn current_admin_api_key(&self) -> String {
        self.admin_api_key.current()
    }

    pub fn set_admin_api_key(&self, admin_api_key: impl Into<String>) {
        self.admin_api_key.replace(admin_api_key);
    }
}

fn admin_auth_keys_match(presented: Option<&str>, configured: &str) -> bool {
    matches!(
        presented,
        Some(key)
            if !key.is_empty()
                && !configured.is_empty()
                && auth::constant_time_eq(key, configured)
    )
}

/// Admin API 认证中间件
pub async fn admin_auth_middleware(
    State(state): State<AdminState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let api_key = auth::extract_api_key(&request);

    if admin_auth_keys_match(api_key.as_deref(), &state.current_admin_api_key()) {
        next.run(request).await
    } else {
        record_admin_auth_failure(&state, &request);
        let error = AdminErrorResponse::authentication_error();
        (StatusCode::UNAUTHORIZED, Json(error)).into_response()
    }
}

/// 认证失败：限速日志 + 按来源限速的 `admin_auth_failed` 审计（actor 为 `anonymous`）。
/// 审计写入和其它管理操作一样走有界存储队列，不会因为暴力尝试把任务堆满。
fn record_admin_auth_failure(state: &AdminState, request: &Request<Body>) {
    let path = request.uri().path().to_string();
    // 限速按真实连接地址：代理头可以随意伪造，只作为参考写进审计。
    let peer_ip = request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|info| info.0.ip().to_string());
    let forwarded_for = client_source_ip(request.headers());
    let presented = auth::extract_api_key(request).is_some();
    if let Some(skipped) = ADMIN_AUTH_FAILURE_LOG_THROTTLE.admit() {
        tracing::warn!(
            path = %path,
            peer_ip = peer_ip.as_deref().unwrap_or("unknown"),
            forwarded_for = forwarded_for.as_deref().unwrap_or(""),
            key_presented = presented,
            skipped_since_last_log = skipped,
            "Admin API 认证失败"
        );
    }
    let bucket = peer_ip
        .clone()
        .or_else(|| forwarded_for.clone())
        .unwrap_or_else(|| "unknown".to_string());
    let Some(suppressed) = ADMIN_AUTH_FAILURE_AUDIT_LIMITER.admit(&bucket, Instant::now()) else {
        return;
    };
    let mut detail = json!({
        "path": path,
        "method": request.method().as_str(),
        "keyPresented": presented,
    });
    if let Some(ip) = peer_ip.as_deref().or(forwarded_for.as_deref()) {
        detail["sourceIp"] = json!(ip);
    }
    if let Some(forwarded) = forwarded_for.as_deref()
        && peer_ip.is_some()
    {
        detail["forwardedFor"] = json!(forwarded);
    }
    if suppressed > 0 {
        detail["suppressedSinceLast"] = json!(suppressed);
    }
    state.service.audit_as(
        "anonymous",
        "admin_auth_failed",
        "admin_auth",
        None,
        false,
        Some("Invalid or missing admin API key".to_string()),
        detail,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admin_auth_rejects_empty_presented_or_configured_keys() {
        assert!(!admin_auth_keys_match(Some(""), ""));
        assert!(!admin_auth_keys_match(Some(""), "configured-secret"));
        assert!(!admin_auth_keys_match(Some("configured-secret"), ""));
        assert!(!admin_auth_keys_match(None, "configured-secret"));
        assert!(admin_auth_keys_match(
            Some("configured-secret"),
            "configured-secret"
        ));
    }

    #[test]
    fn admin_auth_failure_source_ip_prefers_forwarded_for_then_real_ip() {
        let mut headers = HeaderMap::new();
        assert_eq!(client_source_ip(&headers), None);
        headers.insert("x-real-ip", "10.0.0.9".parse().unwrap());
        assert_eq!(client_source_ip(&headers).as_deref(), Some("10.0.0.9"));
        headers.insert("x-forwarded-for", "203.0.113.7, 10.0.0.1".parse().unwrap());
        assert_eq!(client_source_ip(&headers).as_deref(), Some("203.0.113.7"));
        headers.insert("x-forwarded-for", " , ".parse().unwrap());
        assert_eq!(client_source_ip(&headers).as_deref(), Some("10.0.0.9"));
    }

    #[test]
    fn admin_auth_failure_audit_is_limited_per_source_per_minute() {
        let limiter = AdminAuthFailureAuditLimiter::default();
        let start = Instant::now();
        for _ in 0..ADMIN_AUTH_FAILURE_AUDITS_PER_MINUTE {
            assert_eq!(limiter.admit("203.0.113.7", start), Some(0));
        }
        assert_eq!(
            limiter.admit("203.0.113.7", start + Duration::from_secs(30)),
            None
        );
        // 其它来源不受影响，并带上此前被压掉的次数。
        assert_eq!(
            limiter.admit("198.51.100.2", start + Duration::from_secs(30)),
            Some(1)
        );
        // 窗口过后重新放行。
        assert_eq!(
            limiter.admit("203.0.113.7", start + ADMIN_AUTH_FAILURE_WINDOW),
            Some(0)
        );
    }

    #[test]
    fn admin_auth_failure_audit_has_a_global_cap_and_overflow_bucket() {
        let limiter = AdminAuthFailureAuditLimiter::default();
        let now = Instant::now();
        // 每次换一个来源（伪造代理头的打法）也只能写到全局上限。
        let mut admitted = 0;
        for index in 0..500 {
            if limiter
                .admit(&format!("10.0.{}.{}", index / 256, index % 256), now)
                .is_some()
            {
                admitted += 1;
            }
        }
        assert_eq!(admitted, ADMIN_AUTH_FAILURE_AUDITS_PER_MINUTE_GLOBAL);
        assert!(limiter.window.lock().sources.len() <= ADMIN_AUTH_FAILURE_MAX_TRACKED_SOURCES);
        // 下一个窗口的第一条审计带上被压掉的次数。
        let next = now + ADMIN_AUTH_FAILURE_WINDOW;
        assert_eq!(
            limiter.admit("192.0.2.1", next),
            Some(500 - u64::from(ADMIN_AUTH_FAILURE_AUDITS_PER_MINUTE_GLOBAL))
        );

        // 来源表满了不清空：新来源共用溢出桶，原有来源的计数保留。
        let limiter = AdminAuthFailureAuditLimiter::default();
        {
            let mut window = limiter.window.lock();
            window.started_at = Some(now);
            for index in 0..ADMIN_AUTH_FAILURE_MAX_TRACKED_SOURCES {
                window.sources.insert(format!("tracked-{index}"), 1);
            }
        }
        for _ in 0..ADMIN_AUTH_FAILURE_AUDITS_PER_MINUTE {
            assert!(limiter.admit("new-source", now).is_some());
        }
        assert!(limiter.admit("another-new-source", now).is_none());
        let window = limiter.window.lock();
        assert_eq!(
            window.sources.len(),
            ADMIN_AUTH_FAILURE_MAX_TRACKED_SOURCES + 1
        );
        assert_eq!(window.sources.get("tracked-0"), Some(&1));
    }

    #[test]
    fn cloned_admin_key_store_observes_rotation_and_empty_replacement() {
        let listener_store = AdminApiKeyStore::new("old-secret");
        let admin_state_store = listener_store.clone();

        listener_store.replace("new-secret");
        assert_eq!(admin_state_store.current(), "new-secret");
        assert!(!admin_auth_keys_match(
            Some("old-secret"),
            &admin_state_store.current()
        ));
        assert!(admin_auth_keys_match(
            Some("new-secret"),
            &admin_state_store.current()
        ));

        listener_store.replace("");
        assert!(!admin_auth_keys_match(
            Some(""),
            &admin_state_store.current()
        ));
    }
}
