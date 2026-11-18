//! Admin API 路由配置

use axum::{
    Router, middleware,
    routing::{delete, get, post, put},
};
use tower_http::compression::{
    CompressionLayer,
    predicate::{And, DefaultPredicate, NotForContentType, Predicate},
};

use super::{
    capture_handlers::{download_capture, get_capture_overview, start_capture, stop_capture},
    handlers::*,
    middleware::{AdminState, admin_auth_middleware},
    route_scenarios::http as route_scenario_http,
};

/// 创建 Admin API 路由
///
/// # 端点
/// 账号、路由策略、运行配置、usage 和诊断采集管理。
///
/// # 认证
/// 需要 Admin API Key 认证，支持：
/// - `x-api-key` header
/// - `Authorization: Bearer <token>` header
pub fn create_admin_router(state: AdminState) -> Router {
    Router::new()
        .route(
            "/routes",
            get(super::service::management::routes).put(super::service::management::put_routes),
        )
        .route(
            "/routes/effective",
            get(super::service::management::route_effective),
        )
        .route(
            "/accounts/{id}/effective",
            get(super::service::management::account_effective),
        )
        .route(
            "/model-capabilities/{model}/listed",
            axum::routing::patch(super::service::management::model_listed),
        )
        .route(
            "/config/export",
            get(super::service::management::export_config),
        )
        .route(
            "/config/import",
            post(super::service::management::import_config),
        )
        .route(
            "/route-scenarios",
            get(route_scenario_http::list).post(route_scenario_http::create),
        )
        .route(
            "/route-scenarios/options",
            get(route_scenario_http::options),
        )
        .route(
            "/route-scenarios/current",
            get(route_scenario_http::current),
        )
        .route(
            "/route-scenarios/{id}",
            put(route_scenario_http::update).delete(route_scenario_http::delete),
        )
        .route(
            "/route-scenarios/{id}/preview",
            get(route_scenario_http::preview),
        )
        .route(
            "/route-scenarios/{id}/apply",
            post(route_scenario_http::apply),
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
            "/accounts/{id}/models/promote",
            post(promote_account_models),
        )
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
        .route("/accounts/{id}/copy", post(copy_account))
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
        .route(
            "/usage-dashboard/overview/summary",
            get(get_usage_dashboard_overview_summary),
        )
        .route(
            "/usage-dashboard/overview/series",
            get(get_usage_dashboard_overview_series),
        )
        .route(
            "/usage-dashboard/overview",
            get(super::service::management::usage_overview),
        )
        .route(
            "/usage-dashboard/overview/rankings",
            get(get_usage_dashboard_overview_rankings),
        )
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
            "/audit-logs/account-summary",
            get(get_account_event_summary),
        )
        .route(
            "/config/runtime",
            get(get_runtime_config).put(update_runtime_config),
        )
        .route(
            "/prompt-injection/catalog",
            get(get_prompt_injection_catalog),
        )
        .route("/prompt-injection/preview", post(preview_prompt_injection))
        .route("/system/version", get(get_system_version))
        .route("/diagnostics/capture", get(get_capture_overview))
        .route("/diagnostics/capture/start", post(start_capture))
        .route("/diagnostics/capture/stop", post(stop_capture))
        .route("/diagnostics/capture/{id}/download", get(download_capture))
        .route("/security/keys", get(get_access_keys))
        .route("/security/admin-key", put(update_admin_api_key))
        .route("/security/request-keys", post(create_request_api_key))
        .route(
            "/security/request-keys/usage-limits",
            get(get_request_key_usage_limits),
        )
        .route(
            "/security/request-keys/{id}/usage-limits/reset",
            post(reset_request_key_usage),
        )
        .route(
            "/security/request-keys/{id}",
            put(update_request_api_key).delete(delete_request_api_key),
        )
        .route("/model-pricing", get(get_model_pricing))
        .route("/model-pricing/sync", post(sync_model_pricing))
        .route("/model-capabilities", get(get_model_capabilities))
        .route("/model-capabilities/manual", post(upsert_manual_model))
        .route("/model-capabilities/discover", post(discover_model_catalog))
        .route("/model-capabilities/adopt", post(adopt_model_catalog))
        .route(
            "/model-capabilities/manual/{model}",
            delete(delete_manual_model),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            admin_auth_middleware,
        ))
        .layer(admin_compression_layer())
        .with_state(state)
}

/// Account, proxy and usage lists are large, repetitive JSON; gzip cuts what a slow link has to
/// carry by most of it. Applied only when the client sends `Accept-Encoding: gzip`; capture
/// downloads are already zip archives and pass through as they are.
fn admin_compression_layer() -> CompressionLayer<And<DefaultPredicate, NotForContentType>> {
    CompressionLayer::new()
        .gzip(true)
        .no_br()
        .no_deflate()
        .no_zstd()
        .compress_when(DefaultPredicate::new().and(NotForContentType::const_new("application/zip")))
}

#[cfg(test)]
mod tests {
    use super::admin_compression_layer;
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{Request, header},
        response::IntoResponse,
        routing::get,
    };
    use std::io::Read;
    use tower::ServiceExt;

    fn app() -> Router {
        let json = "{\"id\":1,\"email\":\"user@example.com\"}".repeat(200);
        Router::new()
            .route(
                "/json",
                get(move || async move {
                    ([(header::CONTENT_TYPE, "application/json")], json).into_response()
                }),
            )
            .route(
                "/zip",
                get(|| async {
                    (
                        [(header::CONTENT_TYPE, "application/zip")],
                        vec![b'z'; 4096],
                    )
                        .into_response()
                }),
            )
            .layer(admin_compression_layer())
    }

    async fn get_with(path: &str, accept: Option<&str>) -> axum::response::Response {
        let mut request = Request::builder().uri(path);
        if let Some(accept) = accept {
            request = request.header(header::ACCEPT_ENCODING, accept);
        }
        app()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn json_is_gzipped_when_the_client_accepts_it() {
        let response = get_with("/json", Some("gzip, deflate, br")).await;
        assert_eq!(
            response.headers().get(header::CONTENT_ENCODING).unwrap(),
            "gzip"
        );
        let compressed = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let mut plain = String::new();
        flate2::read::GzDecoder::new(&compressed[..])
            .read_to_string(&mut plain)
            .unwrap();
        assert!(plain.starts_with("{\"id\":1"));
        assert!(
            compressed.len() * 5 < plain.len(),
            "repetitive JSON should shrink well"
        );
    }

    #[tokio::test]
    async fn responses_stay_plain_without_accept_encoding() {
        let response = get_with("/json", None).await;
        assert!(response.headers().get(header::CONTENT_ENCODING).is_none());
    }

    #[tokio::test]
    async fn zip_downloads_are_not_compressed_again() {
        let response = get_with("/zip", Some("gzip")).await;
        assert!(response.headers().get(header::CONTENT_ENCODING).is_none());
        assert_eq!(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .len(),
            4096
        );
    }
}
