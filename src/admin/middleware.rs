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

/// 每个来源 IP 每分钟最多写这么多条认证失败审计，超出只累计不落库。
const ADMIN_AUTH_FAILURE_AUDITS_PER_MINUTE: u32 = 10;
const ADMIN_AUTH_FAILURE_WINDOW: Duration = Duration::from_secs(60);
/// 来源 IP 计数表上限；满了整表清空，宁可少记也不让它无限增长。
const ADMIN_AUTH_FAILURE_MAX_TRACKED_SOURCES: usize = 1024;

/// 认证失败审计的按来源限速器：`(窗口起点, 窗口内已记录条数)`。
#[derive(Default)]
pub(super) struct AdminAuthFailureAuditLimiter {
    sources: Mutex<HashMap<String, (Instant, u32)>>,
}

impl AdminAuthFailureAuditLimiter {
    /// 这次失败是否还能写审计。来源未知（没有代理头）时统一记到 `unknown` 桶。
    pub(super) fn admit(&self, source: &str, now: Instant) -> bool {
        let mut sources = self.sources.lock();
        if sources.len() >= ADMIN_AUTH_FAILURE_MAX_TRACKED_SOURCES && !sources.contains_key(source)
        {
            sources.clear();
        }
        let entry = sources.entry(source.to_string()).or_insert((now, 0));
        if now.saturating_duration_since(entry.0) >= ADMIN_AUTH_FAILURE_WINDOW {
            *entry = (now, 0);
        }
        if entry.1 >= ADMIN_AUTH_FAILURE_AUDITS_PER_MINUTE {
            return false;
        }
        entry.1 += 1;
        true
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
    let source_ip = client_source_ip(request.headers());
    let presented = auth::extract_api_key(request).is_some();
    if let Some(skipped) = ADMIN_AUTH_FAILURE_LOG_THROTTLE.admit() {
        tracing::warn!(
            path = %path,
            source_ip = source_ip.as_deref().unwrap_or("unknown"),
            key_presented = presented,
            skipped_since_last_log = skipped,
            "Admin API 认证失败"
        );
    }
    let bucket = source_ip.clone().unwrap_or_else(|| "unknown".to_string());
    if !ADMIN_AUTH_FAILURE_AUDIT_LIMITER.admit(&bucket, Instant::now()) {
        return;
    }
    let mut detail = json!({
        "path": path,
        "method": request.method().as_str(),
        "keyPresented": presented,
    });
    if let Some(ip) = source_ip {
        detail["sourceIp"] = json!(ip);
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
            assert!(limiter.admit("203.0.113.7", start));
        }
        assert!(!limiter.admit("203.0.113.7", start + Duration::from_secs(30)));
        // 其它来源不受影响。
        assert!(limiter.admit("198.51.100.2", start + Duration::from_secs(30)));
        // 窗口过后重新放行。
        assert!(limiter.admit("203.0.113.7", start + ADMIN_AUTH_FAILURE_WINDOW));
    }

    #[test]
    fn admin_auth_failure_audit_limiter_clears_when_too_many_sources_are_tracked() {
        let limiter = AdminAuthFailureAuditLimiter::default();
        let now = Instant::now();
        for index in 0..ADMIN_AUTH_FAILURE_MAX_TRACKED_SOURCES {
            assert!(limiter.admit(&format!("10.0.{}.{}", index / 256, index % 256), now));
        }
        assert_eq!(
            limiter.sources.lock().len(),
            ADMIN_AUTH_FAILURE_MAX_TRACKED_SOURCES
        );
        assert!(limiter.admit("192.0.2.1", now));
        assert_eq!(limiter.sources.lock().len(), 1);
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
