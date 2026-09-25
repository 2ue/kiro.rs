//! Admin API 中间件

use std::sync::Arc;

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Json, Response},
};
use parking_lot::RwLock;

use super::service::AdminService;
use super::types::AdminErrorResponse;
use crate::common::auth;

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
        let error = AdminErrorResponse::authentication_error();
        (StatusCode::UNAUTHORIZED, Json(error)).into_response()
    }
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
