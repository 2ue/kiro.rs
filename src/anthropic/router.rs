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

use crate::account::AccountManager;
use crate::common::auth::RequestApiKeyStore;
use crate::model::config::PromptCacheSimulationMode;
use crate::model::runtime::RuntimeConfigStore;

use super::{
    envelope,
    handlers::{
        count_tokens, count_tokens_cc, count_tokens_dfcache, count_tokens_ha, count_tokens_na,
        get_model, get_models, post_messages, post_messages_cc, post_messages_dfcache,
        post_messages_ha, post_messages_na,
    },
    middleware::{AppState, auth_middleware, capture_http_ingress, cors_layer},
    model_capabilities::ModelCapabilitiesCatalog,
    pricing::PricingCatalog,
    prompt_cache::PromptCacheTracker,
    prompt_cache_creation_control::PromptCacheCreationController,
    request_admission::{
        RequestAdmissionController, RequestAdmissionMiddlewareState, request_admission_middleware,
    },
    request_body::MAX_MESSAGES_BODY_SIZE,
    usage::UsageRecorder,
};

pub struct AnthropicRouterDependencies {
    pub runtime: Arc<RuntimeConfigStore>,
    pub request_api_keys: Arc<RequestApiKeyStore>,
    pub request_admission: Arc<RequestAdmissionController>,
    pub usage_recorder: Arc<UsageRecorder>,
    pub prompt_cache: Arc<PromptCacheTracker>,
    pub prompt_cache_creation_controller: Arc<PromptCacheCreationController>,
    pub pricing_catalog: Arc<PricingCatalog>,
    pub model_capabilities: Arc<ModelCapabilitiesCatalog>,
    pub account_manager: Option<Arc<AccountManager>>,
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
pub fn create_router(dependencies: AnthropicRouterDependencies) -> Router {
    let AnthropicRouterDependencies {
        runtime,
        request_api_keys,
        request_admission,
        usage_recorder,
        prompt_cache,
        prompt_cache_creation_controller,
        pricing_catalog,
        model_capabilities,
        account_manager,
    } = dependencies;
    let request_admission_state =
        RequestAdmissionMiddlewareState::new(request_admission, usage_recorder.clone());
    let mut base_state = AppState::new(
        request_api_keys,
        usage_recorder,
        prompt_cache,
        prompt_cache_creation_controller,
        PromptCacheSimulationMode::HighCache,
    )
    .with_runtime(runtime)
    .with_pricing_catalog(pricing_catalog)
    .with_model_capabilities(model_capabilities);

    if let Some(manager) = account_manager {
        base_state = base_state.with_account_manager(manager);
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
            super::middleware::route_middleware,
        ))
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
            super::middleware::route_middleware,
        ))
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
            super::middleware::route_middleware,
        ))
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
            super::middleware::route_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            ha_v1_state.clone(),
            auth_middleware,
        ))
        .with_state(ha_v1_state);

    // 自定义入口与兼容的 /dfcache/{route}/v1 入口共用处理器。
    // 前缀必须在 routes 中显式定义。
    let dfcache_routes = Router::new()
        .route("/{route}/v1/models", get(get_models))
        .route("/{route}/v1/models/{model_id}", get(get_model))
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
            super::middleware::route_middleware,
        ))
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
        .nest("/dfcache", dfcache_routes.clone())
        .merge(dfcache_routes)
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

    use crate::model::config::{Config, RequestAdmissionConfig};

    use super::*;

    fn base_state(mode: PromptCacheSimulationMode) -> AppState {
        AppState::new(
            Arc::new(RequestApiKeyStore::new(["test-key"])),
            Arc::new(UsageRecorder::new(10)),
            Arc::new(PromptCacheTracker::default()),
            Arc::new(PromptCacheCreationController::default()),
            mode,
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
            config.routes.route_mut("/dfcache/demo").cache_policy =
                crate::model::config::CacheRoutePolicyPatch {
                    cache_type: Some(
                        crate::model::config::PromptCacheStrategyType::CurrentHighCache,
                    ),
                    route_namespace: Some(true),
                    ..Default::default()
                };
            let app = create_router(AnthropicRouterDependencies {
                runtime: RuntimeConfigStore::new(config, None),
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
                prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
                pricing_catalog: Arc::new(PricingCatalog::new()),
                model_capabilities: Arc::new(ModelCapabilitiesCatalog::new()),
                account_manager: None,
            });

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
        config.routes.route_mut("/dfcache/demo").cache_policy =
            crate::model::config::CacheRoutePolicyPatch {
                cache_type: Some(crate::model::config::PromptCacheStrategyType::CurrentHighCache),
                route_namespace: Some(true),
                ..Default::default()
            };
        let app = create_router(AnthropicRouterDependencies {
            runtime: RuntimeConfigStore::new(config, None),
            request_api_keys: Arc::new(RequestApiKeyStore::new(["body-limit-key"])),
            request_admission: Arc::new(RequestAdmissionController::new(
                RequestAdmissionConfig::disabled(),
            )),

            usage_recorder: Arc::new(UsageRecorder::new(10)),
            prompt_cache: Arc::new(PromptCacheTracker::default()),
            prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
            pricing_catalog: Arc::new(PricingCatalog::new()),
            model_capabilities: Arc::new(ModelCapabilitiesCatalog::new()),
            account_manager: None,
        });
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
        config.routes.route_mut("/dfcache/demo").cache_policy =
            crate::model::config::CacheRoutePolicyPatch {
                cache_type: Some(crate::model::config::PromptCacheStrategyType::CurrentHighCache),
                route_namespace: Some(true),
                ..Default::default()
            };
        test_router_with_runtime(keys, RuntimeConfigStore::new(config, None))
    }

    fn test_router_with_runtime(keys: &[&str], runtime: Arc<RuntimeConfigStore>) -> Router {
        create_router(AnthropicRouterDependencies {
            runtime,
            request_api_keys: Arc::new(RequestApiKeyStore::new(keys)),
            request_admission: Arc::new(RequestAdmissionController::new(
                RequestAdmissionConfig::disabled(),
            )),

            usage_recorder: Arc::new(UsageRecorder::new(10)),
            prompt_cache: Arc::new(PromptCacheTracker::default()),
            prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
            pricing_catalog: Arc::new(PricingCatalog::new()),
            model_capabilities: Arc::new(ModelCapabilitiesCatalog::new()),
            account_manager: None,
        })
    }

    async fn message_status(app: &Router, endpoint: &str) -> StatusCode {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(endpoint)
                    .header("content-type", "application/json")
                    .header("x-api-key", "runtime-update-key")
                    .body(Body::from(
                        r#"{"model":"claude-sonnet-4-6","max_tokens":16,"messages":[{"role":"user","content":"ping"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        status
    }

    #[tokio::test]
    async fn admin_route_update_is_visible_to_request_path() {
        let runtime = RuntimeConfigStore::new(Config::default(), None);
        let app = test_router_with_runtime(&["runtime-update-key"], runtime.clone());
        assert_eq!(message_status(&app, "/cc/v1/messages").await.as_u16(), 529);

        runtime
            .update(|config| config.routes.route_mut("/cc").enabled = false)
            .unwrap();
        assert_eq!(
            message_status(&app, "/cc/v1/messages").await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(message_status(&app, "/na/v1/messages").await.as_u16(), 529);

        runtime
            .update(|config| config.routes.route_mut("/cc").enabled = true)
            .unwrap();
        assert_eq!(message_status(&app, "/cc/v1/messages").await.as_u16(), 529);
    }

    #[tokio::test]
    async fn admin_route_update_is_visible_to_request_path_allowed_routes() {
        let mut config = Config::default();
        config.set_request_api_key_policies([crate::model::config::RequestApiKeyPolicy {
            api_key: "runtime-update-key".into(),
            name: "runtime update".into(),
            enabled: true,
            request_admission: None,
            usage_limits: vec![],
            allowed_routes: vec![],
        }]);
        let runtime = RuntimeConfigStore::new(config, None);
        let app = test_router_with_runtime(&["runtime-update-key"], runtime.clone());
        assert_eq!(message_status(&app, "/cc/v1/messages").await.as_u16(), 529);

        runtime
            .update(|config| {
                config.request_api_key_policies[0].allowed_routes = vec!["/na".into()];
            })
            .unwrap();
        assert_eq!(
            message_status(&app, "/cc/v1/messages").await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(message_status(&app, "/na/v1/messages").await.as_u16(), 529);

        runtime
            .update(|config| config.request_api_key_policies[0].allowed_routes.clear())
            .unwrap();
        assert_eq!(message_status(&app, "/cc/v1/messages").await.as_u16(), 529);
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
