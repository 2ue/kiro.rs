//! Admin API 路由配置

use axum::{
    Router, middleware,
    routing::{delete, get, post},
};

use super::{
    handlers::{
        add_credential, clear_usage_records, delete_credential, force_refresh_token,
        get_all_credentials, get_credential_balance, get_credentials_page, get_load_balancing_mode,
        get_usage_records, get_usage_records_page, get_usage_stats, get_usage_summary,
        list_admin_actions, list_app_config, list_pricing, list_quota_events, reset_failure_count,
        set_credential_disabled, set_credential_priority, set_load_balancing_mode, sync_pricing,
        test_credential, update_app_config,
    },
    middleware::{AdminState, admin_auth_middleware},
};

/// 创建 Admin API 路由
pub fn create_admin_router(state: AdminState) -> Router {
    Router::new()
        .route(
            "/credentials",
            get(get_all_credentials).post(add_credential),
        )
        .route("/credentials-paged", get(get_credentials_page))
        .route("/credentials/{id}", delete(delete_credential))
        .route("/credentials/{id}/disabled", post(set_credential_disabled))
        .route("/credentials/{id}/priority", post(set_credential_priority))
        .route("/credentials/{id}/reset", post(reset_failure_count))
        .route("/credentials/{id}/refresh", post(force_refresh_token))
        .route("/credentials/{id}/balance", get(get_credential_balance))
        .route("/credentials/{id}/test", post(test_credential))
        .route("/usage-records", get(get_usage_records))
        .route("/usage-records-paged", get(get_usage_records_page))
        .route("/usage-records/clear", post(clear_usage_records))
        .route("/usage-summary", get(get_usage_summary))
        .route("/usage-stats", get(get_usage_stats))
        .route("/quota-events", get(list_quota_events))
        .route("/admin-actions", get(list_admin_actions))
        .route(
            "/config/load-balancing",
            get(get_load_balancing_mode).put(set_load_balancing_mode),
        )
        .route("/config", get(list_app_config).put(update_app_config))
        .route("/pricing", get(list_pricing))
        .route("/pricing/sync", post(sync_pricing))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            admin_auth_middleware,
        ))
        .with_state(state)
}
