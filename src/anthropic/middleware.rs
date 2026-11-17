//! Anthropic API 中间件

use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, Request, StatusCode},
    middleware::Next,
    response::Response,
};
use bytes::{Bytes, BytesMut};
use chrono::{DateTime, Utc};
use http_body::{Body as HttpBody, Frame, SizeHint};
use uuid::Uuid;

use crate::account::AccountManager;
use crate::common::auth;
use crate::common::auth::RequestApiKeyStore;
use crate::model::config::PromptCacheSimulationMode;
use crate::model::runtime::RuntimeConfigStore;

use super::request_body::MAX_MESSAGES_BODY_SIZE;
use super::{
    envelope, model_capabilities::ModelCapabilitiesCatalog, pricing::PricingCatalog,
    prompt_cache::PromptCacheTracker, prompt_cache_creation_control::PromptCacheCreationController,
    usage::UsageRecorder,
};

/// 入口层明文采集。仅在采集会话运行时旁路复制请求体，并把入口关联 id 传给下游。
pub async fn capture_http_ingress(mut request: Request<Body>, next: Next) -> Response {
    if !crate::diagnostics::capture::is_active() {
        return next.run(request).await;
    }

    let ingress_id = format!("ingress-{}", Uuid::new_v4().simple());
    let method = request.method().clone();
    let uri = request.uri().clone();
    let headers = request.headers().clone();
    let content_length = headers
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    let body_limit = ingress_body_limit(uri.path());
    request
        .extensions_mut()
        .insert(crate::diagnostics::capture::IngressId(ingress_id.clone()));
    let started_at = Utc::now();

    // 已知超限时不要读 body，交给原有 DefaultBodyLimit 保持 413 行为。
    if content_length.is_some_and(|length| length > body_limit as u64) {
        crate::diagnostics::capture::record("http_ingress", None, || {
            serde_json::json!({
                "ingressId": ingress_id,
                "method": method.as_str(),
                "path": uri.path(),
                "query": uri.query(),
                "headers": crate::diagnostics::capture::http_headers_json(&headers),
                "startedAt": started_at.to_rfc3339(),
                "bodyTruncated": true,
                "bodyBytes": content_length,
                "bodyReadError": null,
            })
        });
        let response = next.run(request).await;
        return record_http_egress(response, &ingress_id);
    }

    let (parts, body) = request.into_parts();
    let capture_body = IngressCaptureBody::new(
        body,
        IngressCaptureState {
            ingress_id: ingress_id.clone(),
            method: method.as_str().to_string(),
            path: uri.path().to_string(),
            query: uri.query().map(str::to_string),
            headers,
            started_at,
            body_limit,
            body_bytes: 0,
            body_truncated: false,
            body: BytesMut::new(),
            recorded: false,
        },
    );
    let request = Request::from_parts(parts, Body::new(capture_body));
    let response = next.run(request).await;
    record_http_egress(response, &ingress_id)
}

fn ingress_body_limit(_path: &str) -> usize {
    MAX_MESSAGES_BODY_SIZE
}

struct IngressCaptureBody {
    inner: Body,
    state: IngressCaptureState,
}

impl IngressCaptureBody {
    fn new(inner: Body, state: IngressCaptureState) -> Self {
        Self { inner, state }
    }
}

impl HttpBody for IngressCaptureBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let result = Pin::new(&mut self.inner).poll_frame(cx);
        match result {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    self.state.observe(data);
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(error))) => {
                let message = error.to_string();
                self.state.record(Some(message), false);
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(None) => {
                self.state.record(None, true);
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl Drop for IngressCaptureBody {
    fn drop(&mut self) {
        self.state
            .record(Some("body dropped before EOF".to_string()), false);
    }
}

struct IngressCaptureState {
    ingress_id: String,
    method: String,
    path: String,
    query: Option<String>,
    headers: HeaderMap,
    started_at: DateTime<Utc>,
    body_limit: usize,
    body_bytes: usize,
    body_truncated: bool,
    body: BytesMut,
    recorded: bool,
}

impl IngressCaptureState {
    fn observe(&mut self, bytes: &Bytes) {
        self.body_bytes = self.body_bytes.saturating_add(bytes.len());
        if self.body.len() < self.body_limit {
            let remaining = self.body_limit - self.body.len();
            let keep = remaining.min(bytes.len());
            self.body.extend_from_slice(&bytes[..keep]);
            if keep < bytes.len() {
                self.body_truncated = true;
            }
        } else if !bytes.is_empty() {
            self.body_truncated = true;
        }
        if self.body_bytes > self.body_limit {
            self.body_truncated = true;
        }
    }

    fn record(&mut self, body_read_error: Option<String>, complete: bool) {
        if self.recorded {
            return;
        }
        self.recorded = true;
        let body_truncated = self.body_truncated || !complete;
        let body = if !body_truncated {
            crate::diagnostics::capture::body_json(&self.body)
        } else {
            serde_json::Value::Null
        };
        crate::diagnostics::capture::record("http_ingress", None, || {
            serde_json::json!({
                "ingressId": self.ingress_id,
                "method": self.method,
                "path": self.path,
                "query": self.query,
                "headers": crate::diagnostics::capture::http_headers_json(&self.headers),
                "startedAt": self.started_at.to_rfc3339(),
                "bodyTruncated": body_truncated,
                "bodyBytes": self.body_bytes,
                "bodyReadError": body_read_error,
                "body": body,
            })
        });
        self.body.clear();
    }
}

fn record_http_egress(response: Response, ingress_id: &str) -> Response {
    if !crate::diagnostics::capture::is_active() {
        return response;
    }
    let request_id = response
        .extensions()
        .get::<crate::diagnostics::capture::CaptureRequestId>()
        .map(|value| value.0.clone())
        .or_else(|| {
            response
                .headers()
                .get("request-id")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        });
    let status = response.status();
    let headers = crate::diagnostics::capture::http_headers_json(response.headers());
    crate::diagnostics::capture::record("http_egress", request_id.as_deref(), || {
        serde_json::json!({
            "ingressId": ingress_id,
            "requestId": request_id,
            "status": status.as_u16(),
            "headers": headers,
        })
    });
    response
}

/// 应用共享状态
#[derive(Clone)]
pub struct AppState {
    pub runtime: Arc<RuntimeConfigStore>,
    /// 客户端请求 API Key 内存索引。
    pub request_api_keys: Arc<RequestApiKeyStore>,
    /// 请求级 usage 记录器。
    pub usage_recorder: Arc<UsageRecorder>,
    /// 模型价格目录。仅用于统计计价，失败不影响请求。
    pub pricing_catalog: Arc<PricingCatalog>,
    /// 模型能力目录。仅用于 /models 和后台观测，失败不影响请求调度。
    pub model_capabilities: Arc<ModelCapabilitiesCatalog>,
    /// 本地 prompt-cache tracker。
    pub prompt_cache: Arc<PromptCacheTracker>,
    /// 本地 prompt-cache creation 上报频次控制器。
    pub prompt_cache_creation_controller: Arc<PromptCacheCreationController>,
    /// 旧缓存策略的模拟模式；Config 中没有对应字段。
    pub prompt_cache_simulation_mode: PromptCacheSimulationMode,
    /// 账号池管理器。
    pub account_manager: Option<Arc<AccountManager>>,
}

impl AppState {
    /// 创建新的应用状态。
    pub fn new(
        request_api_keys: Arc<RequestApiKeyStore>,
        usage_recorder: Arc<UsageRecorder>,
        prompt_cache: Arc<PromptCacheTracker>,
        prompt_cache_creation_controller: Arc<PromptCacheCreationController>,
        prompt_cache_simulation_mode: PromptCacheSimulationMode,
    ) -> Self {
        Self {
            runtime: RuntimeConfigStore::new(crate::model::config::Config::default(), None),
            request_api_keys,
            usage_recorder,
            pricing_catalog: Arc::new(PricingCatalog::new()),
            model_capabilities: Arc::new(ModelCapabilitiesCatalog::new()),
            prompt_cache,
            prompt_cache_creation_controller,
            prompt_cache_simulation_mode,
            account_manager: None,
        }
    }

    pub fn with_runtime(mut self, runtime: Arc<RuntimeConfigStore>) -> Self {
        self.runtime = runtime;
        self
    }

    pub fn with_pricing_catalog(mut self, pricing_catalog: Arc<PricingCatalog>) -> Self {
        self.pricing_catalog = pricing_catalog;
        self
    }

    pub fn with_model_capabilities(
        mut self,
        model_capabilities: Arc<ModelCapabilitiesCatalog>,
    ) -> Self {
        self.model_capabilities = model_capabilities;
        self
    }

    pub fn with_account_manager(mut self, account_manager: Arc<AccountManager>) -> Self {
        self.account_manager = Some(account_manager);
        self
    }
}

pub async fn route_middleware(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let endpoint = request
        .extensions()
        .get::<axum::extract::OriginalUri>()
        .map(|uri| uri.0.path())
        .unwrap_or_else(|| request.uri().path());
    if !state
        .runtime
        .snapshot()
        .compiled_routes
        .as_ref()
        .is_some_and(|routes| routes.endpoint_enabled(endpoint))
    {
        return envelope::error_response(
            StatusCode::NOT_FOUND,
            "not_found_error",
            format!("route is not configured or disabled: {endpoint}"),
        );
    }
    next.run(request).await
}

/// API Key 认证中间件
pub async fn auth_middleware(
    State(state): State<AppState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let presented_key = auth::extract_api_key(&request);
    let key_present = presented_key.is_some();
    let identity = presented_key.and_then(|key| state.request_api_keys.authenticate(&key));
    match identity {
        Some(identity) => {
            let config = state.runtime.snapshot();
            let endpoint = request
                .extensions()
                .get::<axum::extract::OriginalUri>()
                .map(|uri| uri.0.path())
                .unwrap_or_else(|| request.uri().path());
            if let Some(policy) = config
                .request_api_key_policies()
                .iter()
                .find(|policy| auth::request_api_key_id(&policy.api_key) == identity.stable_id())
                .filter(|policy| !policy.allowed_routes.is_empty())
            {
                let prefix = config
                    .compiled_routes
                    .as_ref()
                    .and_then(|routes| routes.match_endpoint(endpoint))
                    .map(|route| route.config.prefix.as_str());
                let allowed = prefix.is_some_and(|prefix| {
                    policy
                        .allowed_routes
                        .iter()
                        .any(|rule| crate::model::routes::normalize_route_prefix(rule) == prefix)
                });
                if !allowed {
                    return envelope::error_response(
                        StatusCode::FORBIDDEN,
                        "permission_error",
                        "API key is not allowed to access this route",
                    );
                }
            }
            request.extensions_mut().insert(identity);
            let mut response = next.run(request).await;
            if !response.headers().contains_key("request-id") {
                let request_id = envelope::request_id();
                envelope::insert_request_id_headers(response.headers_mut(), &request_id);
            }
            response
        }
        _ => {
            let request_id = envelope::request_id();
            // Only record whether a key was presented; the key itself never reaches the log.
            tracing::warn!(
                request_id = %request_id,
                method = %request.method(),
                path = %request.uri().path(),
                key_present,
                "请求 API Key 认证失败"
            );
            envelope::error_response_with_id(
                StatusCode::UNAUTHORIZED,
                "authentication_error",
                "Invalid API key",
                &request_id,
            )
        }
    }
}

/// CORS 中间件层
///
/// **安全说明**：当前配置允许所有来源（Any），这是为了支持公开 API 服务。
/// 如果需要更严格的安全控制，请根据实际需求配置具体的允许来源、方法和头信息。
///
/// # 配置说明
/// - `allow_origin(Any)`: 允许任何来源的请求
/// - `allow_methods(Any)`: 允许任何 HTTP 方法
/// - `allow_headers(Any)`: 允许任何请求头
pub fn cors_layer() -> tower_http::cors::CorsLayer {
    use tower_http::cors::{Any, CorsLayer};

    CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
}

#[cfg(test)]
mod tests {
    use axum::{Router, routing::get};
    use tower::ServiceExt;

    use super::*;

    const JSON_413_BODY: &str = r#"{"source":"upstream","kind":"json-413"}"#;
    const TEXT_413_BODY: &str = "upstream plaintext 413";
    const UNTYPED_413_BODY: &str = "handler untyped 413";

    fn test_state() -> AppState {
        AppState::new(
            Arc::new(RequestApiKeyStore::new(["middleware-413-key"])),
            Arc::new(UsageRecorder::new(10)),
            Arc::new(PromptCacheTracker::default()),
            Arc::new(PromptCacheCreationController::default()),
            PromptCacheSimulationMode::HighCache,
        )
    }

    #[tokio::test]
    async fn admin_allowed_routes_rejects_disallowed_route_with_permission_error() {
        let state = test_state();
        state.runtime.install_local(|config| {
            config.request_api_key_policies = vec![crate::model::config::RequestApiKeyPolicy {
                api_key: "middleware-413-key".into(),
                name: "restricted".into(),
                enabled: true,
                request_admission: None,
                usage_limits: vec![],
                allowed_routes: vec!["cc/".into()],
            }];
        });
        let app = Router::new()
            .route("/cc/v1/messages", get(|| async { "ok" }))
            .route("/ha/v1/messages", get(|| async { "ok" }))
            .route("/v1/messages", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(state, auth_middleware));
        for (path, expected) in [
            ("/cc/v1/messages", StatusCode::OK),
            ("/ha/v1/messages", StatusCode::FORBIDDEN),
            ("/v1/messages", StatusCode::FORBIDDEN),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header("x-api-key", "middleware-413-key")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
            if expected == StatusCode::FORBIDDEN {
                let body = axum::body::to_bytes(response.into_body(), 4096)
                    .await
                    .unwrap();
                let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(body["error"]["type"], "permission_error");
            }
        }
    }

    async fn json_413() -> Response {
        Response::builder()
            .status(StatusCode::PAYLOAD_TOO_LARGE)
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .header("x-origin-marker", "json")
            .body(Body::from(JSON_413_BODY))
            .unwrap()
    }

    async fn plaintext_413() -> Response {
        Response::builder()
            .status(StatusCode::PAYLOAD_TOO_LARGE)
            .header(
                axum::http::header::CONTENT_TYPE,
                "text/plain; charset=utf-8",
            )
            .header("x-origin-marker", "text")
            .body(Body::from(TEXT_413_BODY))
            .unwrap()
    }

    async fn untyped_413() -> Response {
        Response::builder()
            .status(StatusCode::PAYLOAD_TOO_LARGE)
            .header("x-origin-marker", "untyped")
            .body(Body::from(UNTYPED_413_BODY))
            .unwrap()
    }

    #[tokio::test]
    async fn auth_middleware_preserves_downstream_413_bodies_for_five_rounds() {
        let state = test_state();
        let app = Router::new()
            .route("/json", get(json_413))
            .route("/text", get(plaintext_413))
            .route("/untyped", get(untyped_413))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                auth_middleware,
            ))
            .with_state(state);

        for _ in 0..5 {
            for (path, expected_content_type, expected_marker, expected_body) in [
                ("/json", Some("application/json"), "json", JSON_413_BODY),
                (
                    "/text",
                    Some("text/plain; charset=utf-8"),
                    "text",
                    TEXT_413_BODY,
                ),
                ("/untyped", None, "untyped", UNTYPED_413_BODY),
            ] {
                let response = app
                    .clone()
                    .oneshot(
                        Request::builder()
                            .uri(path)
                            .header("x-api-key", "middleware-413-key")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
                assert_eq!(
                    response
                        .headers()
                        .get(axum::http::header::CONTENT_TYPE)
                        .and_then(|value| value.to_str().ok()),
                    expected_content_type
                );
                assert_eq!(
                    response
                        .headers()
                        .get("x-origin-marker")
                        .and_then(|value| value.to_str().ok()),
                    Some(expected_marker)
                );
                let request_id = response
                    .headers()
                    .get("request-id")
                    .and_then(|value| value.to_str().ok())
                    .expect("middleware must add request-id")
                    .to_string();
                assert_eq!(
                    response
                        .headers()
                        .get("anthropic-request-id")
                        .and_then(|value| value.to_str().ok()),
                    Some(request_id.as_str())
                );
                let body = axum::body::to_bytes(response.into_body(), 1024)
                    .await
                    .unwrap();
                assert_eq!(body.as_ref(), expected_body.as_bytes());
            }
        }
    }
}
