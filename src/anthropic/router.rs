//! Anthropic API 路由配置

use std::sync::Arc;

use axum::{
    Router,
    extract::DefaultBodyLimit,
    http::StatusCode,
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
};

use crate::common::auth::RequestApiKeyStore;
use crate::external_pool::ExternalPoolManager;
use crate::model::config::{
    CachePolicyConfig, CompatProfile, Config, ExternalPoolsConfig, ImageProcessingConfig,
    MissingMaxTokensConfig, ModelMappingConfig, ModelResolutionMode, PayloadGuardMode,
    PayloadShapingConfig, PromptCacheCreationControlConfig, PromptCacheSimulationMode,
    PromptInjectionConfig, ReportedUsageConfig, RouteFeaturesConfig,
};

use super::{
    envelope,
    handlers::{
        count_tokens, count_tokens_cc, count_tokens_dfcache, count_tokens_ha, count_tokens_na,
        get_model, get_model_dfcache, get_models, get_models_dfcache, post_messages,
        post_messages_cc, post_messages_dfcache, post_messages_ha, post_messages_na,
    },
    middleware::{AppState, auth_middleware, capture_http_ingress, cors_layer},
    model_capabilities::ModelCapabilitiesCatalog,
    pricing::PricingCatalog,
    prompt_cache::{PromptCacheBounds, PromptCacheTracker},
    prompt_cache_creation_control::PromptCacheCreationController,
    request_admission::{
        RequestAdmissionController, RequestAdmissionMiddlewareState, request_admission_middleware,
    },
    request_body::MAX_MESSAGES_BODY_SIZE,
    usage::UsageRecorder,
};

pub struct AnthropicRouterDependencies {
    pub request_api_keys: Arc<RequestApiKeyStore>,
    pub request_admission: Arc<RequestAdmissionController>,
    pub usage_recorder: Arc<UsageRecorder>,
    pub prompt_cache: Arc<PromptCacheTracker>,
    pub prompt_cache_creation_controller: Arc<PromptCacheCreationController>,
    pub pricing_catalog: Arc<PricingCatalog>,
    pub model_capabilities: Arc<ModelCapabilitiesCatalog>,
    pub external_pool_manager: Option<Arc<ExternalPoolManager>>,
}

pub struct AnthropicRouterConfig {
    prompt_cache_target_read_ratio: f64,
    prompt_cache_token_scale: f64,
    prompt_cache_max_simulated_input_tokens: i32,
    prompt_cache_cap_jitter_min_tokens: i32,
    prompt_cache_cap_jitter_max_tokens: i32,
    prompt_cache_scale_min_input_tokens: i32,
    prompt_cache_creation_control: PromptCacheCreationControlConfig,
    prompt_cache_bounds: PromptCacheBounds,
    reported_usage: ReportedUsageConfig,
    cache_policy: CachePolicyConfig,
    defined_cache_routes: Vec<String>,
    compat_profile: CompatProfile,
    prompt_injection: PromptInjectionConfig,
    route_features: RouteFeaturesConfig,
    model_resolution_mode: ModelResolutionMode,
    model_mapping: ModelMappingConfig,
    payload_guard_enabled: bool,
    payload_guard_mode: PayloadGuardMode,
    payload_guard_max_bytes: usize,
    payload_guard_safety_margin_bytes: usize,
    payload_guard_trim_history: bool,
    payload_guard_external_enabled: bool,
    image_processing: ImageProcessingConfig,
    missing_max_tokens: MissingMaxTokensConfig,
    payload_shaping: PayloadShapingConfig,
    external_pools: ExternalPoolsConfig,
}

impl AnthropicRouterConfig {
    pub fn from_runtime_config(config: &Config) -> Self {
        Self {
            prompt_cache_target_read_ratio: config.prompt_cache_target_read_ratio,
            prompt_cache_token_scale: config.prompt_cache_token_scale,
            prompt_cache_max_simulated_input_tokens: config.prompt_cache_max_simulated_input_tokens,
            prompt_cache_cap_jitter_min_tokens: config.prompt_cache_cap_jitter_min_tokens,
            prompt_cache_cap_jitter_max_tokens: config.prompt_cache_cap_jitter_max_tokens,
            prompt_cache_scale_min_input_tokens: config.prompt_cache_scale_min_input_tokens,
            prompt_cache_creation_control: config.prompt_cache_creation_control.normalized(),
            prompt_cache_bounds: PromptCacheBounds::from_config(
                config.prompt_cache_max_entries_per_account,
                config.prompt_cache_max_entries_global,
                config.prompt_cache_entry_ttl_secs,
                config.prompt_cache_estimated_bytes_limit,
            ),
            reported_usage: config.reported_usage.clone(),
            cache_policy: config.cache_policy.clone(),
            defined_cache_routes: config.defined_cache_routes.clone(),
            compat_profile: config.compat_profile,
            prompt_injection: config.prompt_injection.clone().normalized(),
            route_features: config.route_features.clone().normalized(),
            model_resolution_mode: config.model_resolution_mode,
            model_mapping: config.model_mapping.clone().normalized(),
            payload_guard_enabled: config
                .route_features
                .defaults
                .payload_guard
                .map(|guard| guard.enabled)
                .unwrap_or(false),
            payload_guard_mode: config
                .route_features
                .defaults
                .payload_guard
                .map(|guard| guard.mode)
                .unwrap_or(Default::default()),
            payload_guard_max_bytes: config
                .route_features
                .defaults
                .payload_guard
                .map(|guard| guard.max_bytes)
                .unwrap_or(0),
            payload_guard_safety_margin_bytes: config
                .route_features
                .defaults
                .payload_guard
                .map(|guard| guard.safety_margin_bytes)
                .unwrap_or(0),
            payload_guard_trim_history: config
                .route_features
                .defaults
                .payload_guard
                .map(|guard| guard.trim_history)
                .unwrap_or(false),
            payload_guard_external_enabled: config
                .route_features
                .defaults
                .payload_guard
                .map(|guard| guard.external_enabled)
                .unwrap_or(false),
            image_processing: config
                .route_features
                .defaults
                .image_processing
                .unwrap_or_default()
                .normalized(),
            missing_max_tokens: config
                .route_features
                .defaults
                .missing_max_tokens
                .unwrap_or_default()
                .normalized(),
            payload_shaping: config
                .route_features
                .defaults
                .payload_shaping
                .unwrap_or_default(),
            external_pools: config.external_pools.clone(),
        }
    }
}

/// 创建 Anthropic API 路由
///
/// # 端点
/// - `GET /v1/models` - 获取可用模型列表
/// - `POST /v1/messages` - 创建消息（对话）
/// - `POST /v1/messages/count_tokens` - 计算 token 数量
/// - `GET /na/v1/models` - 获取可用模型列表
/// - `POST /na/v1/messages` - 创建消息
/// - `POST /na/v1/messages/count_tokens` - 计算 token 数量
/// - `GET /ha/v1/models` - 获取可用模型列表
/// - `POST /ha/v1/messages` - 创建消息
/// - `POST /ha/v1/messages/count_tokens` - 计算 token 数量
///
/// # 认证
/// 所有 `/v1` 路径需要 API Key 认证，支持：
/// - `x-api-key` header
/// - `Authorization: Bearer <token>` header
///
pub fn create_router(
    dependencies: AnthropicRouterDependencies,
    config: AnthropicRouterConfig,
) -> Router {
    let AnthropicRouterDependencies {
        request_api_keys,
        request_admission,
        usage_recorder,
        prompt_cache,
        prompt_cache_creation_controller,
        pricing_catalog,
        model_capabilities,
        external_pool_manager,
    } = dependencies;
    let AnthropicRouterConfig {
        prompt_cache_target_read_ratio,
        prompt_cache_token_scale,
        prompt_cache_max_simulated_input_tokens,
        prompt_cache_cap_jitter_min_tokens,
        prompt_cache_cap_jitter_max_tokens,
        prompt_cache_scale_min_input_tokens,
        prompt_cache_creation_control,
        prompt_cache_bounds,
        reported_usage,
        cache_policy,
        defined_cache_routes,
        compat_profile,
        prompt_injection,
        route_features,
        model_resolution_mode,
        model_mapping,
        payload_guard_enabled,
        payload_guard_mode,
        payload_guard_max_bytes,
        payload_guard_safety_margin_bytes,
        payload_guard_trim_history,
        payload_guard_external_enabled,
        image_processing,
        missing_max_tokens,
        payload_shaping,
        external_pools,
    } = config;
    let request_admission_state =
        RequestAdmissionMiddlewareState::new(request_admission, usage_recorder.clone());
    let configured_features = route_features.clone();
    let mut base_state = AppState::new(
        request_api_keys,
        usage_recorder,
        prompt_cache,
        prompt_cache_creation_controller,
        PromptCacheSimulationMode::HighCache,
        prompt_cache_target_read_ratio,
        compat_profile,
    )
    .with_prompt_cache_amplification(
        prompt_cache_token_scale,
        prompt_cache_max_simulated_input_tokens,
        prompt_cache_cap_jitter_min_tokens,
        prompt_cache_cap_jitter_max_tokens,
        prompt_cache_scale_min_input_tokens,
    )
    .with_prompt_cache_creation_control(prompt_cache_creation_control)
    .with_prompt_cache_bounds(prompt_cache_bounds)
    .with_reported_usage(reported_usage)
    .with_cache_policy(cache_policy)
    .with_defined_cache_routes(defined_cache_routes)
    .with_prompt_injection(prompt_injection, route_features)
    .with_model_resolution_mode(model_resolution_mode)
    .with_model_mapping(model_mapping)
    .with_payload_guard(
        payload_guard_enabled,
        payload_guard_mode,
        payload_guard_max_bytes,
        payload_guard_safety_margin_bytes,
        payload_guard_trim_history,
        payload_guard_external_enabled,
        image_processing,
        payload_shaping,
    )
    .with_missing_max_tokens(missing_max_tokens)
    .with_external_pools(external_pools)
    .with_pricing_catalog(pricing_catalog)
    .with_model_capabilities(model_capabilities);

    base_state
        .runtime
        .install_local(|config| config.route_features = configured_features);

    if let Some(manager) = external_pool_manager {
        base_state = base_state.with_external_pool_manager(manager);
    }

    let (v1_state, na_v1_state, cc_v1_state, ha_v1_state) = route_prompt_cache_states(base_state);
    let define_cache_state = v1_state.clone();

    // 需要认证的内置 /v1 路由。运行策略由运行配置按入口路径解析。
    let v1_routes = Router::new()
        .route("/models", get(get_models))
        .route("/models/{model_id}", get(get_model))
        .route(
            "/messages",
            post(post_messages).layer(middleware::from_fn_with_state(
                request_admission_state.clone(),
                request_admission_middleware,
            )),
        )
        .route("/messages/count_tokens", post(count_tokens))
        // Set before the auth layer so method probing still requires a key.
        .method_not_allowed_fallback(method_not_allowed)
        .layer(middleware::from_fn_with_state(
            v1_state.clone(),
            auth_middleware,
        ))
        .with_state(v1_state);

    // 需要认证的内置 /na/v1 路由。运行策略由运行配置按入口路径解析。
    let na_v1_routes = Router::new()
        .route("/models", get(get_models))
        .route("/models/{model_id}", get(get_model))
        .route(
            "/messages",
            post(post_messages_na).layer(middleware::from_fn_with_state(
                request_admission_state.clone(),
                request_admission_middleware,
            )),
        )
        .route("/messages/count_tokens", post(count_tokens_na))
        // Set before the auth layer so method probing still requires a key.
        .method_not_allowed_fallback(method_not_allowed)
        .layer(middleware::from_fn_with_state(
            na_v1_state.clone(),
            auth_middleware,
        ))
        .with_state(na_v1_state);

    // 需要认证的内置 /cc/v1 路由。运行策略由运行配置按入口路径解析。
    let cc_v1_routes = Router::new()
        .route("/models", get(get_models))
        .route("/models/{model_id}", get(get_model))
        .route(
            "/messages",
            post(post_messages_cc).layer(middleware::from_fn_with_state(
                request_admission_state.clone(),
                request_admission_middleware,
            )),
        )
        .route("/messages/count_tokens", post(count_tokens_cc))
        // Set before the auth layer so method probing still requires a key.
        .method_not_allowed_fallback(method_not_allowed)
        .layer(middleware::from_fn_with_state(
            cc_v1_state.clone(),
            auth_middleware,
        ))
        .with_state(cc_v1_state);

    // 需要认证的内置 /ha/v1 路由。运行策略由运行配置按入口路径解析。
    let ha_v1_routes = Router::new()
        .route("/models", get(get_models))
        .route("/models/{model_id}", get(get_model))
        .route(
            "/messages",
            post(post_messages_ha).layer(middleware::from_fn_with_state(
                request_admission_state.clone(),
                request_admission_middleware,
            )),
        )
        .route("/messages/count_tokens", post(count_tokens_ha))
        // Set before the auth layer so method probing still requires a key.
        .method_not_allowed_fallback(method_not_allowed)
        .layer(middleware::from_fn_with_state(
            ha_v1_state.clone(),
            auth_middleware,
        ))
        .with_state(ha_v1_state);

    // 需要认证的 /dfcache/{route}/v1 路由。
    // route 必须在 definedCacheRoutes 中显式定义，避免未知路径被默认放行。
    let dfcache_routes = Router::new()
        .route("/{route}/v1/models", get(get_models_dfcache))
        .route("/{route}/v1/models/{model_id}", get(get_model_dfcache))
        .route(
            "/{route}/v1/messages",
            post(post_messages_dfcache).layer(middleware::from_fn_with_state(
                request_admission_state.clone(),
                request_admission_middleware,
            )),
        )
        .route(
            "/{route}/v1/messages/count_tokens",
            post(count_tokens_dfcache),
        )
        // Set before the auth layer so method probing still requires a key.
        .method_not_allowed_fallback(method_not_allowed)
        .layer(middleware::from_fn_with_state(
            define_cache_state.clone(),
            auth_middleware,
        ))
        .with_state(define_cache_state);

    let mut router = Router::new();
    for path in API_HELLO_PATHS {
        router = router.route(path, get(api_hello));
    }
    router
        .nest("/v1", v1_routes)
        .nest("/na/v1", na_v1_routes)
        .nest("/cc/v1", cc_v1_routes)
        .nest("/ha/v1", ha_v1_routes)
        .nest("/dfcache", dfcache_routes)
        .fallback(route_not_found)
        .layer(middleware::map_response(mark_non_retryable_client_errors))
        .layer(cors_layer())
        .layer(DefaultBodyLimit::max(MAX_MESSAGES_BODY_SIZE))
        .layer(middleware::from_fn(capture_http_ingress))
}

/// Claude Code preconnects with `HEAD <ANTHROPIC_BASE_URL>/api/hello` at startup and only needs
/// the endpoint to be reachable, so every API base path answers it without authentication.
const API_HELLO_PATHS: [&str; 5] = [
    "/api/hello",
    "/na/api/hello",
    "/cc/api/hello",
    "/ha/api/hello",
    "/dfcache/{route}/api/hello",
];

/// `GET` routes also answer `HEAD` in axum; the empty 200 carries a request id like other replies.
async fn api_hello() -> Response {
    let mut response = StatusCode::OK.into_response();
    envelope::insert_request_id_headers(response.headers_mut(), &envelope::request_id());
    response
}

async fn route_not_found() -> Response {
    envelope::error_response(StatusCode::NOT_FOUND, "not_found_error", "Not Found")
}

async fn method_not_allowed() -> Response {
    envelope::error_response(
        StatusCode::METHOD_NOT_ALLOWED,
        "invalid_request_error",
        "Method Not Allowed",
    )
}

/// Covers client errors built outside the envelope helpers (framework rejections, handler
/// shortcuts) so every non-retryable 4xx tells Claude Code not to retry.
async fn mark_non_retryable_client_errors(mut response: Response) -> Response {
    let status = response.status();
    envelope::insert_should_retry_header(response.headers_mut(), status);
    response
}

fn route_prompt_cache_states(base_state: AppState) -> (AppState, AppState, AppState, AppState) {
    (
        base_state.clone(),
        base_state.clone(),
        base_state.clone(),
        base_state,
    )
}

#[cfg(test)]
mod tests {
    use axum::{
        body::{Body, Bytes},
        http::Request,
    };
    use tower::ServiceExt;

    use crate::model::config::RequestAdmissionConfig;

    use super::*;

    fn base_state(mode: PromptCacheSimulationMode) -> AppState {
        AppState::new(
            Arc::new(RequestApiKeyStore::new(["test-key"])),
            Arc::new(UsageRecorder::new(10)),
            Arc::new(PromptCacheTracker::default()),
            Arc::new(PromptCacheCreationController::default()),
            mode,
            0.98,
            CompatProfile::ClaudeCode,
        )
    }

    #[test]
    fn route_prompt_cache_states_keep_strategy_out_of_route_registration() {
        for base_mode in [
            PromptCacheSimulationMode::Disabled,
            PromptCacheSimulationMode::HighCache,
        ] {
            let (v1_state, na_v1_state, cc_v1_state, ha_v1_state) =
                route_prompt_cache_states(base_state(base_mode));

            assert_eq!(v1_state.prompt_cache_simulation_mode, base_mode);
            assert_eq!(na_v1_state.prompt_cache_simulation_mode, base_mode);
            assert_eq!(cc_v1_state.prompt_cache_simulation_mode, base_mode);
            assert_eq!(ha_v1_state.prompt_cache_simulation_mode, base_mode);
            assert!(Arc::ptr_eq(
                &v1_state.prompt_cache,
                &na_v1_state.prompt_cache
            ));
            assert!(Arc::ptr_eq(
                &v1_state.prompt_cache,
                &cc_v1_state.prompt_cache
            ));
            assert!(Arc::ptr_eq(
                &v1_state.prompt_cache,
                &ha_v1_state.prompt_cache
            ));
        }
    }

    #[tokio::test]
    async fn authentication_is_outer_to_message_admission_for_five_rounds() {
        let state = base_state(PromptCacheSimulationMode::HighCache);
        let controller = Arc::new(RequestAdmissionController::new(RequestAdmissionConfig {
            rpm: 0,
            max_concurrent_requests: 1,
            max_queued_requests: 0,
            queue_timeout_ms: 0,
        }));
        let admission_state =
            RequestAdmissionMiddlewareState::new(controller, state.usage_recorder.clone());
        let app = Router::new()
            .route(
                "/messages",
                post(|| async { "ok" }).layer(middleware::from_fn_with_state(
                    admission_state,
                    request_admission_middleware,
                )),
            )
            .layer(middleware::from_fn_with_state(
                state.clone(),
                auth_middleware,
            ))
            .with_state(state);

        for _ in 0..5 {
            let request = Request::builder()
                .method("POST")
                .uri("/messages")
                .header("x-api-key", "test-key")
                .body(Body::empty())
                .unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), axum::http::StatusCode::OK);
            axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap();
        }

        let unauthorized = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/messages")
                    .header("x-api-key", "wrong-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), axum::http::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn actual_anthropic_message_routes_reject_before_handler_for_five_rounds() {
        const PATHS: [&str; 5] = [
            "/v1/messages",
            "/na/v1/messages",
            "/cc/v1/messages",
            "/ha/v1/messages",
            "/dfcache/demo/v1/messages",
        ];
        let body = serde_json::json!({
            "model": "claude-sonnet-4-20250514",
            "max_tokens": 16,
            "messages": [{"role": "user", "content": "ping"}]
        })
        .to_string();

        for round in 0..5 {
            let keys = PATHS
                .iter()
                .enumerate()
                .map(|(index, _)| format!("actual-route-{round}-{index}"))
                .collect::<Vec<_>>();
            let mut config = Config::default();
            config.defined_cache_routes = vec!["/dfcache/demo".to_string()];
            let app = create_router(
                AnthropicRouterDependencies {
                    request_api_keys: Arc::new(RequestApiKeyStore::new(&keys)),
                    request_admission: Arc::new(RequestAdmissionController::new(
                        RequestAdmissionConfig {
                            rpm: 1,
                            max_concurrent_requests: 0,
                            max_queued_requests: 0,
                            queue_timeout_ms: 0,
                        },
                    )),

                    usage_recorder: Arc::new(UsageRecorder::new(10)),
                    prompt_cache: Arc::new(PromptCacheTracker::default()),
                    prompt_cache_creation_controller: Arc::new(
                        PromptCacheCreationController::default(),
                    ),
                    pricing_catalog: Arc::new(PricingCatalog::new()),
                    model_capabilities: Arc::new(ModelCapabilitiesCatalog::new()),
                    external_pool_manager: None,
                },
                AnthropicRouterConfig::from_runtime_config(&config),
            );

            for (path, key) in PATHS.iter().zip(&keys) {
                let make_request = || {
                    Request::builder()
                        .method("POST")
                        .uri(*path)
                        .header("content-type", "application/json")
                        .header("x-api-key", key)
                        .body(Body::from(body.clone()))
                        .unwrap()
                };
                let first = app.clone().oneshot(make_request()).await.unwrap();
                assert_ne!(first.status(), axum::http::StatusCode::TOO_MANY_REQUESTS);
                axum::body::to_bytes(first.into_body(), 64 * 1024)
                    .await
                    .unwrap();

                let rejected = app.clone().oneshot(make_request()).await.unwrap();
                assert_eq!(rejected.status(), axum::http::StatusCode::TOO_MANY_REQUESTS);
                assert!(
                    !rejected.headers().contains_key("x-should-retry"),
                    "rate limits must stay retryable"
                );
            }
        }
    }

    #[tokio::test]
    async fn all_message_routes_normalize_oversized_bodies_without_a_provider_for_five_rounds() {
        const PATHS: [&str; 5] = [
            "/v1/messages",
            "/na/v1/messages",
            "/cc/v1/messages",
            "/ha/v1/messages",
            "/dfcache/demo/v1/messages",
        ];
        let mut config = Config::default();
        config.defined_cache_routes = vec!["/dfcache/demo".to_string()];
        let app = create_router(
            AnthropicRouterDependencies {
                request_api_keys: Arc::new(RequestApiKeyStore::new(["body-limit-key"])),
                request_admission: Arc::new(RequestAdmissionController::new(
                    RequestAdmissionConfig::disabled(),
                )),

                usage_recorder: Arc::new(UsageRecorder::new(10)),
                prompt_cache: Arc::new(PromptCacheTracker::default()),
                prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
                pricing_catalog: Arc::new(PricingCatalog::new()),
                model_capabilities: Arc::new(ModelCapabilitiesCatalog::new()),
                external_pool_manager: None,
            },
            AnthropicRouterConfig::from_runtime_config(&config),
        );
        let oversized = Bytes::from(vec![b'x'; MAX_MESSAGES_BODY_SIZE + 1]);

        for _round in 0..5 {
            for path in PATHS {
                let response = app
                    .clone()
                    .oneshot(
                        Request::builder()
                            .method("POST")
                            .uri(path)
                            .header("content-type", "application/json")
                            .header("x-api-key", "body-limit-key")
                            .body(Body::from(oversized.clone()))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), axum::http::StatusCode::PAYLOAD_TOO_LARGE);
                assert_eq!(response.headers()["x-should-retry"], "false");
                let request_id = response
                    .headers()
                    .get("request-id")
                    .and_then(|value| value.to_str().ok())
                    .expect("oversized response must carry request-id")
                    .to_string();
                assert_eq!(
                    response
                        .headers()
                        .get("anthropic-request-id")
                        .and_then(|value| value.to_str().ok()),
                    Some(request_id.as_str())
                );
                let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
                    .await
                    .unwrap();
                let value: serde_json::Value = serde_json::from_slice(&body)
                    .expect("oversized response must use the Anthropic JSON envelope");
                assert_eq!(value["type"], "error");
                assert_eq!(value["error"]["type"], "request_too_large");
                assert_eq!(value["request_id"], request_id);
            }
        }
    }

    fn test_router(keys: &[&str]) -> Router {
        let mut config = Config::default();
        config.defined_cache_routes = vec!["/dfcache/demo".to_string()];
        create_router(
            AnthropicRouterDependencies {
                request_api_keys: Arc::new(RequestApiKeyStore::new(keys)),
                request_admission: Arc::new(RequestAdmissionController::new(
                    RequestAdmissionConfig::disabled(),
                )),

                usage_recorder: Arc::new(UsageRecorder::new(10)),
                prompt_cache: Arc::new(PromptCacheTracker::default()),
                prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
                pricing_catalog: Arc::new(PricingCatalog::new()),
                model_capabilities: Arc::new(ModelCapabilitiesCatalog::new()),
                external_pool_manager: None,
            },
            AnthropicRouterConfig::from_runtime_config(&config),
        )
    }

    #[tokio::test]
    async fn authentication_failures_tell_clients_not_to_retry_for_five_rounds() {
        const PATHS: [&str; 5] = [
            "/v1/messages",
            "/na/v1/messages",
            "/cc/v1/messages",
            "/ha/v1/messages",
            "/dfcache/demo/v1/messages",
        ];
        let app = test_router(&["should-retry-key"]);
        for _round in 0..5 {
            for path in PATHS {
                for key in [Some("wrong-key"), None] {
                    let mut request = Request::builder()
                        .method("POST")
                        .uri(path)
                        .header("content-type", "application/json");
                    if let Some(key) = key {
                        request = request.header("x-api-key", key);
                    }
                    let response = app
                        .clone()
                        .oneshot(request.body(Body::from("{}")).unwrap())
                        .await
                        .unwrap();
                    assert_eq!(response.status(), axum::http::StatusCode::UNAUTHORIZED);
                    assert_eq!(response.headers()["x-should-retry"], "false", "{path}");
                    let request_id = response.headers()["request-id"]
                        .to_str()
                        .unwrap()
                        .to_string();
                    let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
                        .await
                        .unwrap();
                    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    assert_eq!(value["error"]["type"], "authentication_error");
                    assert_eq!(value["request_id"], request_id);
                }
            }
        }
    }

    async fn json_error_body(response: Response) -> serde_json::Value {
        let request_id = response
            .headers()
            .get("request-id")
            .and_then(|value| value.to_str().ok())
            .expect("error response must carry request-id")
            .to_string();
        assert_eq!(response.headers()["x-should-retry"], "false");
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&body).expect("error must use the Anthropic JSON envelope");
        assert_eq!(value["type"], "error");
        assert_eq!(value["request_id"], request_id);
        value
    }

    #[tokio::test]
    async fn claude_code_preconnect_probe_is_reachable_without_auth_for_five_rounds() {
        let app = test_router(&["hello-key"]);
        for _round in 0..5 {
            for path in [
                "/api/hello",
                "/na/api/hello",
                "/cc/api/hello",
                "/ha/api/hello",
                "/dfcache/demo/api/hello",
            ] {
                for method in ["HEAD", "GET"] {
                    let response = app
                        .clone()
                        .oneshot(
                            Request::builder()
                                .method(method)
                                .uri(path)
                                .body(Body::empty())
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    assert_eq!(
                        response.status(),
                        axum::http::StatusCode::OK,
                        "{method} {path}"
                    );
                    assert!(response.headers().contains_key("request-id"));
                }
            }
        }
    }

    #[tokio::test]
    async fn unmatched_routes_and_methods_return_anthropic_error_envelopes_for_five_rounds() {
        let app = test_router(&["fallback-key"]);
        for _round in 0..5 {
            for (method, path) in [
                ("GET", "/cc"),
                ("HEAD", "/cc"),
                ("GET", "/v1/nope"),
                ("GET", "/v1/models/claude-sonnet-4-6/extra"),
                ("POST", "/dfcache/demo/v1/unknown"),
            ] {
                let response = app
                    .clone()
                    .oneshot(
                        Request::builder()
                            .method(method)
                            .uri(path)
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    axum::http::StatusCode::NOT_FOUND,
                    "{method} {path}"
                );
                if method == "HEAD" {
                    continue;
                }
                let value = json_error_body(response).await;
                assert_eq!(value["error"]["type"], "not_found_error", "{path}");
            }

            for path in ["/v1/messages", "/cc/v1/messages", "/cc/v1/models"] {
                let response = app
                    .clone()
                    .oneshot(
                        Request::builder()
                            .method("DELETE")
                            .uri(path)
                            .header("x-api-key", "fallback-key")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    axum::http::StatusCode::METHOD_NOT_ALLOWED,
                    "{path}"
                );
                let value = json_error_body(response).await;
                assert_eq!(value["error"]["type"], "invalid_request_error", "{path}");

                let unauthenticated = app
                    .clone()
                    .oneshot(
                        Request::builder()
                            .method("DELETE")
                            .uri(path)
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    unauthenticated.status(),
                    axum::http::StatusCode::UNAUTHORIZED,
                    "{path}: method probing still requires a key"
                );
            }
        }
    }

    fn multipart_file_body(boundary: &str, file_bytes: usize) -> Bytes {
        let prefix = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"fixture.bin\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        );
        let suffix = format!("\r\n--{boundary}--\r\n");
        let mut body = Vec::with_capacity(prefix.len() + file_bytes + suffix.len());
        body.extend_from_slice(prefix.as_bytes());
        body.resize(body.len() + file_bytes, b'x');
        body.extend_from_slice(suffix.as_bytes());
        Bytes::from(body)
    }
}
