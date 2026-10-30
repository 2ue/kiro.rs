//! Admin API 路由配置

use axum::{
    Router, middleware,
    routing::{delete, get, patch, post, put},
};
use tower_http::compression::{
    CompressionLayer,
    predicate::{And, DefaultPredicate, NotForContentType, Predicate},
};

use super::{
    capture_handlers::{download_capture, get_capture_overview, start_capture, stop_capture},
    handlers::{
        add_credential, batch_delete_credentials, batch_import_credentials,
        batch_update_credentials, cancel_usage_cleanup, clear_credential_in_flight,
        clear_external_pool_auto_disabled, clear_external_pool_cooldown, clear_usage_records,
        create_external_pool, create_proxy_resource, create_request_api_key, delete_credential,
        delete_disabled_credentials, delete_external_pool, delete_manual_model,
        delete_proxy_resource, delete_request_api_key, discover_credential_supported_models,
        discover_external_pool_supported_models,
        discover_external_pool_supported_models_from_request, export_credentials,
        force_refresh_token, get_access_keys, get_account_event_summary, get_all_credentials,
        get_audit_logs, get_credential_balance, get_credential_credit_summary,
        get_credential_diagnostics, get_credential_info, get_credentials_account_info,
        get_credentials_list, get_credentials_page, get_credentials_runtime,
        get_credentials_summary, get_credentials_usage_summary, get_external_pool_status,
        get_external_pools, get_load_balancing_mode, get_model_capabilities, get_model_pricing,
        get_prompt_injection_catalog, get_proxy_resources, get_request_key_usage_limits,
        get_runtime_config, get_system_version, get_usage_cleanup_status, get_usage_dashboard,
        get_usage_dashboard_accounts, get_usage_dashboard_breakdown,
        get_usage_dashboard_external_pool_billing, get_usage_dashboard_external_pool_risk,
        get_usage_dashboard_overview, get_usage_dashboard_overview_external,
        get_usage_dashboard_overview_local, get_usage_dashboard_overview_rankings,
        get_usage_dashboard_overview_series, get_usage_dashboard_overview_summary,
        get_usage_dashboard_series, get_usage_dashboard_top, get_usage_dashboard_windows,
        get_usage_records, get_usage_records_page, get_usage_summary, get_usage_writer_stats,
        import_proxy_resources, preview_prompt_injection, preview_usage_cleanup,
        refresh_credentials_info, reset_failure_count, reset_request_key_usage,
        resume_usage_cleanup, set_credential_concurrency, set_credential_disabled,
        set_credential_fast, set_credential_overage, set_credential_priority, set_credential_proxy,
        set_credential_rate_limit_auto_disable, set_credential_regions,
        set_credential_route_policy, set_credential_rpm, set_credential_supported_models,
        set_credential_warmup, set_external_pool_enabled, set_external_pool_supported_models,
        set_load_balancing_mode, start_usage_cleanup, sync_credential_supported_models,
        sync_external_pool_supported_models, sync_model_capabilities, sync_model_pricing,
        test_all_proxy_resources, test_credential, test_external_pool, test_proxy_resource,
        test_proxy_resource_config, update_admin_api_key, update_credential_auth,
        update_external_pool, update_proxy_resource, update_request_api_key, update_runtime_config,
        upsert_manual_model, validate_existing_credentials, validate_external_credentials,
    },
    inventory::http as inventory_http,
    middleware::{AdminState, admin_auth_middleware},
    public_pool::{get_public_pool_config, set_public_pool_config},
    route_scenarios::http as route_scenario_http,
};

/// 创建 Admin API 路由
///
/// # 端点
/// - `GET /credentials` - 获取所有凭据状态
/// - `GET /credentials-paged` - 分页获取凭据状态
/// - `POST /credentials` - 添加新凭据
/// - `DELETE /credentials/:id` - 删除凭据
/// - `POST /credentials/batch-delete` - 按 ID 批量删除已禁用凭据
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
        .route("/inventory", get(inventory_http::list))
        .route("/inventory/import", post(inventory_http::import))
        .route("/inventory/check", post(inventory_http::check))
        .route("/inventory/push", post(inventory_http::push))
        .route("/inventory/delete", post(inventory_http::delete))
        .route("/inventory/{id}", put(inventory_http::update))
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
        .route("/credentials/batch-update", post(batch_update_credentials))
        .route("/credentials/batch-delete", post(batch_delete_credentials))
        .route("/credentials/export", get(export_credentials))
        .route(
            "/credentials/credit-summary",
            get(get_credential_credit_summary),
        )
        .route("/credentials/summary", get(get_credentials_summary))
        .route("/credentials/runtime", get(get_credentials_runtime))
        .route(
            "/credentials/{id}/diagnostics",
            get(get_credential_diagnostics),
        )
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
        .route("/credentials/{id}/fast", post(set_credential_fast))
        .route(
            "/credentials/{id}/route-policy",
            post(set_credential_route_policy),
        )
        .route(
            "/credentials/{id}/concurrency",
            post(set_credential_concurrency),
        )
        .route("/credentials/{id}/rpm", post(set_credential_rpm))
        .route(
            "/credentials/{id}/rate-limit-auto-disable",
            post(set_credential_rate_limit_auto_disable),
        )
        .route(
            "/credentials/{id}/supported-models",
            post(set_credential_supported_models),
        )
        .route(
            "/credentials/{id}/supported-models/sync",
            post(sync_credential_supported_models),
        )
        .route(
            "/credentials/{id}/supported-models/discover",
            post(discover_credential_supported_models),
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
        .route("/credentials/{id}/overage", post(set_credential_overage))
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
        .route("/proxy-resources/import", post(import_proxy_resources))
        .route("/proxy-resources/test", post(test_proxy_resource_config))
        .route("/proxy-resources/test-all", post(test_all_proxy_resources))
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
            "/external-pools/supported-models/discover",
            post(discover_external_pool_supported_models_from_request),
        )
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
            "/external-pools/{id}/supported-models/discover",
            post(discover_external_pool_supported_models),
        )
        .route(
            "/external-pools/{id}/auto-disabled/clear",
            post(clear_external_pool_auto_disabled),
        )
        .route(
            "/external-pools/{id}/cooldown/clear",
            post(clear_external_pool_cooldown),
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
        .route("/usage-records/cleanup/resume", post(resume_usage_cleanup))
        .route(
            "/usage-records/cleanup/status",
            get(get_usage_cleanup_status),
        )
        .route("/usage-records/cleanup/cancel", post(cancel_usage_cleanup))
        .route("/usage-summary", get(get_usage_summary))
        .route("/usage-dashboard", get(get_usage_dashboard))
        .route(
            "/usage-dashboard/overview",
            get(get_usage_dashboard_overview),
        )
        .route(
            "/usage-dashboard/overview/summary",
            get(get_usage_dashboard_overview_summary),
        )
        .route(
            "/usage-dashboard/overview/series",
            get(get_usage_dashboard_overview_series),
        )
        .route(
            "/usage-dashboard/overview/local",
            get(get_usage_dashboard_overview_local),
        )
        .route(
            "/usage-dashboard/overview/external",
            get(get_usage_dashboard_overview_external),
        )
        .route(
            "/usage-dashboard/overview/rankings",
            get(get_usage_dashboard_overview_rankings),
        )
        .route("/usage-dashboard/windows", get(get_usage_dashboard_windows))
        .route("/usage-dashboard/series", get(get_usage_dashboard_series))
        .route("/usage-dashboard/top", get(get_usage_dashboard_top))
        .route(
            "/usage-dashboard/accounts",
            get(get_usage_dashboard_accounts),
        )
        .route(
            "/usage-dashboard/breakdown",
            get(get_usage_dashboard_breakdown),
        )
        .route(
            "/usage-dashboard/external-pool-billing",
            get(get_usage_dashboard_external_pool_billing),
        )
        .route(
            "/usage-dashboard/external-pool-risk",
            get(get_usage_dashboard_external_pool_risk),
        )
        .route("/usage-writer-stats", get(get_usage_writer_stats))
        .route("/audit-logs", get(get_audit_logs))
        .route(
            "/audit-logs/account-summary",
            get(get_account_event_summary),
        )
        .route(
            "/config/load-balancing",
            get(get_load_balancing_mode).put(set_load_balancing_mode),
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
        .route(
            "/public-pool",
            get(get_public_pool_config).put(set_public_pool_config),
        )
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
