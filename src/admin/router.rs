//! Admin API 路由配置

use axum::{
    Router, middleware,
    routing::{delete, get, post, put},
};

use super::{
    handlers::{
        cancel_usage_cleanup, clear_account_auto_disabled, clear_account_cooldown,
        clear_usage_records, create_account, create_proxy_resource, create_request_api_key,
        delete_account, delete_manual_model, delete_proxy_resource, delete_request_api_key,
        discover_account_supported_models, discover_account_supported_models_from_request,
        get_access_keys, get_account_status, get_accounts, get_audit_logs, get_load_balancing_mode,
        get_model_capabilities, get_model_pricing, get_proxy_resources, get_runtime_config,
        get_system_version, get_usage_cleanup_status, get_usage_dashboard,
        get_usage_dashboard_account_billing, get_usage_dashboard_account_risk,
        get_usage_dashboard_breakdown, get_usage_dashboard_series, get_usage_dashboard_top,
        get_usage_dashboard_windows, get_usage_records, get_usage_records_page, get_usage_summary,
        get_usage_writer_stats, preview_usage_cleanup, resume_usage_cleanup, set_account_enabled,
        set_account_supported_models, set_load_balancing_mode, start_usage_cleanup,
        sync_account_supported_models, sync_model_capabilities, sync_model_pricing, test_account,
        test_proxy_resource, test_proxy_resource_config, update_account, update_admin_api_key,
        update_proxy_resource, update_request_api_key, update_runtime_config, upsert_manual_model,
    },
    middleware::{AdminState, admin_auth_middleware},
};

/// 创建 Admin API 路由。
pub fn create_admin_router(state: AdminState) -> Router {
    Router::new()
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
        .route("/accounts", get(get_accounts).post(create_account))
        .route("/accounts/status", get(get_account_status))
        .route(
            "/accounts/supported-models/discover",
            post(discover_account_supported_models_from_request),
        )
        .route("/accounts/{id}", put(update_account).delete(delete_account))
        .route("/accounts/{id}/enabled", post(set_account_enabled))
        .route(
            "/accounts/{id}/supported-models",
            post(set_account_supported_models),
        )
        .route(
            "/accounts/{id}/supported-models/sync",
            post(sync_account_supported_models),
        )
        .route(
            "/accounts/{id}/supported-models/discover",
            post(discover_account_supported_models),
        )
        .route(
            "/accounts/{id}/auto-disabled/clear",
            post(clear_account_auto_disabled),
        )
        .route(
            "/accounts/{id}/cooldown/clear",
            post(clear_account_cooldown),
        )
        .route("/accounts/{id}/test", post(test_account))
        .route("/usage-records", get(get_usage_records))
        .route("/usage-records-paged", get(get_usage_records_page))
        .route("/usage-records/clear", post(clear_usage_records))
        .route(
            "/usage-records/cleanup/preview",
            post(preview_usage_cleanup),
        )
        .route("/usage-records/cleanup/start", post(start_usage_cleanup))
        .route("/usage-records/cleanup/resume", post(resume_usage_cleanup))
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
            "/usage-dashboard/account-billing",
            get(get_usage_dashboard_account_billing),
        )
        .route(
            "/usage-dashboard/account-risk",
            get(get_usage_dashboard_account_risk),
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
