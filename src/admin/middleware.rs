//! Admin API 中间件

use std::sync::Arc;

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Json, Response},
};

use super::service::AdminService;
use super::types::AdminErrorResponse;
use crate::anthropic::PromptCacheRuntimeConfig;
use crate::app_config::AppConfigService;
use crate::common::auth;
use crate::pricing::ModelPricingRegistry;

/// Admin API 共享状态
#[derive(Clone)]
pub struct AdminState {
    /// Admin API 密钥
    pub admin_api_key: String,
    /// Admin 服务
    pub service: Arc<AdminService>,
    /// 在线运行时配置
    pub app_config: Arc<AppConfigService>,
    /// 模型计价
    pub pricing: Arc<ModelPricingRegistry>,
    /// PG 句柄(用于聚合查询等)
    pub db: crate::storage::Db,
    /// Token 管理器(用于配置热更新等)
    pub token_manager: Arc<crate::kiro::token_manager::MultiTokenManager>,
    /// prompt-cache 运行时配置(用于配置热更新)
    pub prompt_cache_runtime_config: Arc<PromptCacheRuntimeConfig>,
}

impl AdminState {
    pub fn new(
        admin_api_key: impl Into<String>,
        service: AdminService,
        app_config: Arc<AppConfigService>,
        pricing: Arc<ModelPricingRegistry>,
        db: crate::storage::Db,
        token_manager: Arc<crate::kiro::token_manager::MultiTokenManager>,
        prompt_cache_runtime_config: Arc<PromptCacheRuntimeConfig>,
    ) -> Self {
        Self {
            admin_api_key: admin_api_key.into(),
            service: Arc::new(service),
            app_config,
            pricing,
            db,
            token_manager,
            prompt_cache_runtime_config,
        }
    }
}

/// Admin API 认证中间件
pub async fn admin_auth_middleware(
    State(state): State<AdminState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let api_key = auth::extract_api_key(&request);

    match api_key {
        Some(key) if auth::constant_time_eq(&key, &state.admin_api_key) => next.run(request).await,
        _ => {
            let error = AdminErrorResponse::authentication_error();
            (StatusCode::UNAUTHORIZED, Json(error)).into_response()
        }
    }
}
