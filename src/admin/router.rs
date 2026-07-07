//! Admin API 路由配置

use axum::{
    Router, middleware,
    routing::{delete, get, patch, post, put},
};

use super::{
    handlers::{
        add_credential, batch_import_credentials, batch_update_credentials, cancel_usage_cleanup,
        clear_credential_in_flight, clear_external_pool_auto_disabled, clear_usage_records,
        create_external_pool, create_proxy_resource, create_request_api_key, delete_credential,
        delete_disabled_credentials, delete_external_pool, delete_manual_model,
        delete_proxy_resource, delete_request_api_key, export_credentials, force_refresh_token,
        get_access_keys, get_all_credentials, get_audit_logs, get_credential_balance,
        get_credential_credit_summary, get_credential_info, get_credentials_account_info,
        get_credentials_list, get_credentials_page, get_credentials_runtime,
        get_credentials_summary, get_credentials_usage_summary, get_external_pool_status,
        get_external_pools, get_load_balancing_mode, get_model_capabilities, get_model_pricing,
        get_proxy_resources, get_runtime_config, get_system_version, get_usage_cleanup_status,
        get_usage_dashboard, get_usage_dashboard_breakdown,
        get_usage_dashboard_external_pool_billing, get_usage_dashboard_series,
        get_usage_dashboard_top, get_usage_dashboard_windows, get_usage_records,
        get_usage_records_page, get_usage_summary, get_usage_writer_stats, preview_usage_cleanup,
        refresh_credentials_info, reset_failure_count, set_credential_concurrency,
        set_credential_disabled, set_credential_priority, set_credential_proxy,
        set_credential_regions, set_credential_rpm, set_credential_supported_models,
        set_credential_warmup, set_external_pool_enabled, set_external_pool_supported_models,
        set_load_balancing_mode, start_usage_cleanup, sync_credential_supported_models,
        sync_external_pool_supported_models, sync_model_capabilities, sync_model_pricing,
        test_credential, test_external_pool, test_proxy_resource, test_proxy_resource_config,
        update_admin_api_key, update_credential_auth, update_external_pool, update_proxy_resource,
        update_request_api_key, update_runtime_config, upsert_manual_model,
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
        .route("/credentials/import", post(batch_import_credentials))
        .route("/credentials/batch-update", post(batch_update_credentials))
        .route("/credentials/export", get(export_credentials))
        .route(
            "/credentials/credit-summary",
            get(get_credential_credit_summary),
        )
        .route("/credentials/summary", get(get_credentials_summary))
        .route("/credentials/runtime", get(get_credentials_runtime))
        .route(
            "/credentials/account-info",
            get(get_credentials_account_info),
        )
        .route(
            "/credentials/usage-summary",
            get(get_credentials_usage_summary),
        )
        .route("/credentials/list", get(get_credentials_list))
        .route("/credentials-list", get(get_credentials_list))
        .route("/credentials/disabled", delete(delete_disabled_credentials))
        .route("/credentials-paged", get(get_credentials_page))
        .route("/credentials/{id}", delete(delete_credential))
        .route("/credentials/{id}/auth", patch(update_credential_auth))
        .route("/credentials/{id}/disabled", post(set_credential_disabled))
        .route("/credentials/{id}/priority", post(set_credential_priority))
        .route(
            "/credentials/{id}/concurrency",
            post(set_credential_concurrency),
        )
        .route("/credentials/{id}/rpm", post(set_credential_rpm))
        .route(
            "/credentials/{id}/supported-models",
            post(set_credential_supported_models),
        )
        .route(
            "/credentials/{id}/supported-models/sync",
            post(sync_credential_supported_models),
        )
        .route("/credentials/{id}/regions", post(set_credential_regions))
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
        .route("/proxy-resources/test", post(test_proxy_resource_config))
        .route("/proxy-resources/{id}/test", post(test_proxy_resource))
        .route(
            "/proxy-resources/{id}",
            put(update_proxy_resource).delete(delete_proxy_resource),
        )
        .route(
            "/external-pools",
            get(get_external_pools).post(create_external_pool),
        )
        .route("/external-pools/status", get(get_external_pool_status))
        .route(
            "/external-pools/{id}",
            put(update_external_pool).delete(delete_external_pool),
        )
        .route(
            "/external-pools/{id}/enabled",
            post(set_external_pool_enabled),
        )
        .route(
            "/external-pools/{id}/supported-models",
            post(set_external_pool_supported_models),
        )
        .route(
            "/external-pools/{id}/supported-models/sync",
            post(sync_external_pool_supported_models),
        )
        .route(
            "/external-pools/{id}/auto-disabled/clear",
            post(clear_external_pool_auto_disabled),
        )
        .route("/external-pools/{id}/test", post(test_external_pool))
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
        .route("/usage-dashboard/windows", get(get_usage_dashboard_windows))
        .route("/usage-dashboard/series", get(get_usage_dashboard_series))
        .route("/usage-dashboard/top", get(get_usage_dashboard_top))
        .route(
            "/usage-dashboard/breakdown",
            get(get_usage_dashboard_breakdown),
        )
        .route(
            "/usage-dashboard/external-pool-billing",
            get(get_usage_dashboard_external_pool_billing),
        )
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
        .route("/system/version", get(get_system_version))
        .route("/security/keys", get(get_access_keys))
        .route("/security/admin-key", put(update_admin_api_key))
        .route("/security/request-keys", post(create_request_api_key))
        .route(
            "/security/request-keys/{id}",
            put(update_request_api_key).delete(delete_request_api_key),
        )
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
