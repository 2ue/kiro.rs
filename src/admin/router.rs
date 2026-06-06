//! Admin API 路由配置

use axum::{
    Router, middleware,
    routing::{delete, get, post, put},
};

use super::{
    handlers::{
        add_credential, cancel_usage_cleanup, clear_credential_in_flight, clear_usage_records,
        create_proxy_resource, delete_credential, delete_manual_model, delete_proxy_resource,
        export_credentials, force_refresh_token, get_access_keys, get_all_credentials,
        get_audit_logs, get_credential_balance, get_credential_info, get_credentials_page,
        get_load_balancing_mode, get_model_capabilities, get_model_pricing, get_proxy_resources,
        get_runtime_config, get_usage_cleanup_status, get_usage_dashboard, get_usage_records,
        get_usage_records_page, get_usage_summary, get_usage_writer_stats, preview_usage_cleanup,
        refresh_credentials_info, reset_failure_count, set_credential_concurrency,
        set_credential_disabled, set_credential_priority, set_credential_proxy,
        set_credential_warmup, set_load_balancing_mode, start_usage_cleanup,
        sync_model_capabilities, sync_model_pricing, test_credential, update_admin_api_key,
        update_proxy_resource, update_runtime_config, upsert_manual_model,
        validate_existing_credentials, validate_external_credentials,
    },
    middleware::{AdminState, admin_auth_middleware},
};

/// 创建 Admin API 路由
///
/// # 端点
/// - `GET /credentials` - 获取所有凭据状态
/// - `GET /credentials-paged` - 分页获取凭据状态
/// - `POST /credentials` - 添加新凭据
/// - `DELETE /credentials/:id` - 删除凭据
/// - `POST /credentials/:id/disabled` - 设置凭据禁用状态
/// - `POST /credentials/:id/priority` - 设置凭据优先级
/// - `POST /credentials/:id/reset` - 重置失败计数
/// - `POST /credentials/:id/refresh` - 强制刷新 Token
/// - `GET /credentials/:id/info` - 查询凭据账号信息
/// - `GET /credentials/:id/balance` - 获取凭据账号信息（兼容旧路径）
/// - `POST /credentials/:id/test` - 测试指定凭据的模型调用
/// - `GET /config/load-balancing` - 获取负载均衡模式
/// - `PUT /config/load-balancing` - 设置负载均衡模式
///
/// # 认证
/// 需要 Admin API Key 认证，支持：
/// - `x-api-key` header
/// - `Authorization: Bearer <token>` header
pub fn create_admin_router(state: AdminState) -> Router {
    Router::new()
        .route(
            "/credentials",
            get(get_all_credentials).post(add_credential),
        )
        .route("/credentials/export", get(export_credentials))
        .route("/credentials-paged", get(get_credentials_page))
        .route("/credentials/{id}", delete(delete_credential))
        .route("/credentials/{id}/disabled", post(set_credential_disabled))
        .route("/credentials/{id}/priority", post(set_credential_priority))
        .route(
            "/credentials/{id}/concurrency",
            post(set_credential_concurrency),
        )
        .route("/credentials/{id}/warmup", post(set_credential_warmup))
        .route(
            "/credentials/{id}/in-flight/clear",
            post(clear_credential_in_flight),
        )
        .route("/credentials/{id}/reset", post(reset_failure_count))
        .route("/credentials/{id}/refresh", post(force_refresh_token))
        .route("/credentials/{id}/balance", get(get_credential_balance))
        .route("/credentials/{id}/info", get(get_credential_info))
        .route("/credentials/info/refresh", post(refresh_credentials_info))
        .route("/credentials/{id}/test", post(test_credential))
        .route("/credentials/{id}/proxy", post(set_credential_proxy))
        .route(
            "/credential-validation/existing",
            post(validate_existing_credentials),
        )
        .route(
            "/credential-validation/external",
            post(validate_external_credentials),
        )
        .route(
            "/proxy-resources",
            get(get_proxy_resources).post(create_proxy_resource),
        )
        .route(
            "/proxy-resources/{id}",
            put(update_proxy_resource).delete(delete_proxy_resource),
        )
        .route("/usage-records", get(get_usage_records))
        .route("/usage-records-paged", get(get_usage_records_page))
        .route("/usage-records/clear", post(clear_usage_records))
        .route(
            "/usage-records/cleanup/preview",
            post(preview_usage_cleanup),
        )
        .route("/usage-records/cleanup/start", post(start_usage_cleanup))
        .route(
            "/usage-records/cleanup/status",
            get(get_usage_cleanup_status),
        )
        .route("/usage-records/cleanup/cancel", post(cancel_usage_cleanup))
        .route("/usage-summary", get(get_usage_summary))
        .route("/usage-dashboard", get(get_usage_dashboard))
        .route("/usage-writer-stats", get(get_usage_writer_stats))
        .route("/audit-logs", get(get_audit_logs))
        .route(
            "/config/load-balancing",
            get(get_load_balancing_mode).put(set_load_balancing_mode),
        )
        .route(
            "/config/runtime",
            get(get_runtime_config).put(update_runtime_config),
        )
        .route("/security/keys", get(get_access_keys))
        .route("/security/admin-key", put(update_admin_api_key))
        .route("/model-pricing", get(get_model_pricing))
        .route("/model-pricing/sync", post(sync_model_pricing))
        .route("/model-capabilities", get(get_model_capabilities))
        .route("/model-capabilities/sync", post(sync_model_capabilities))
        .route("/model-capabilities/manual", post(upsert_manual_model))
        .route(
            "/model-capabilities/manual/{model}",
            delete(delete_manual_model),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            admin_auth_middleware,
        ))
        .with_state(state)
}
