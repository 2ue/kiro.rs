use super::*;
use crate::anthropic::cache::{self, CacheUsage};
use crate::anthropic::model_capabilities::ModelCapabilitiesCatalog;
use crate::anthropic::pricing::PricingCatalog;
use crate::anthropic::prompt_cache::PromptCacheTracker;
use crate::anthropic::prompt_cache_creation_control::PromptCacheCreationController;
use crate::anthropic::request_admission::RequestAdmissionController;
use crate::anthropic::router::{
    AnthropicRouterConfig, AnthropicRouterDependencies, create_router_with_provider,
};
use crate::anthropic::types::{Message, Metadata, SystemMessage};
use crate::anthropic::usage::{
    UsageRecord, UsageRecordQuery, UsageRecorder, UsageRouteKind, UsageRouteSubtype,
};
use crate::common::auth::RequestApiKeyStore;
use crate::external_pool::{
    CreateExternalPoolRequest, ExternalPoolAuthType, ExternalPoolAutoDisablePolicy,
    ExternalPoolManager, ExternalPoolModelMappingMode, ExternalPoolRawModelMode,
    ExternalPoolRequestBodyMode, ExternalPoolStreamRetryMode, ExternalPoolUsageProjectionMode,
};
use crate::kiro::call_trace::{
    AccountRejectReason, KiroCallError, SelectionFailureStage, SelectionFailureSummary,
};
use crate::kiro::endpoint::{IdeEndpoint, KiroEndpoint};
use crate::kiro::model::credentials::KiroCredentials;
use crate::kiro::model::events::MetadataTokenUsage;
use crate::kiro::token_manager::MultiTokenManager;
use crate::model::config::{
    CachePointPolicyPatch, CachePolicyConfig, CacheRoutePolicyPatch, CacheSimulationPolicyPatch,
    PromptCacheCreationControlConfig, PromptSteeringRouteMode, PromptSteeringScope,
    ReportedUsageFieldPolicy, ReportedUsagePathPolicy, RequestAdmissionConfig,
};
use crate::storage::{postgres::PostgresStore, redis_cache::RedisStore};
use axum::{
    Router,
    body::{Body, Bytes},
    http::Request,
    routing::post,
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use serde_json::json;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Mutex as StdMutex, atomic::AtomicUsize};
use tower::ServiceExt;

fn eventstream_test_frame(event_type: &str, payload: serde_json::Value) -> Vec<u8> {
    fn push_string_header(headers: &mut Vec<u8>, name: &str, value: &str) {
        headers.push(name.len() as u8);
        headers.extend_from_slice(name.as_bytes());
        headers.push(7);
        headers.extend_from_slice(&(value.len() as u16).to_be_bytes());
        headers.extend_from_slice(value.as_bytes());
    }

    let mut headers = Vec::new();
    push_string_header(&mut headers, ":message-type", "event");
    push_string_header(&mut headers, ":event-type", event_type);
    push_string_header(&mut headers, ":content-type", "application/json");
    let payload = serde_json::to_vec(&payload).expect("test event payload serializes");
    let total_length = 12 + headers.len() + payload.len() + 4;

    let mut frame = Vec::with_capacity(total_length);
    frame.extend_from_slice(&(total_length as u32).to_be_bytes());
    frame.extend_from_slice(&(headers.len() as u32).to_be_bytes());
    let prelude_crc = crate::kiro::parser::crc::crc32(&frame[..8]);
    frame.extend_from_slice(&prelude_crc.to_be_bytes());
    frame.extend_from_slice(&headers);
    frame.extend_from_slice(&payload);
    let message_crc = crate::kiro::parser::crc::crc32(&frame);
    frame.extend_from_slice(&message_crc.to_be_bytes());
    frame
}

#[derive(Clone, Default)]
struct MultimodalHandlerUpstreamState {
    hits: Arc<AtomicUsize>,
    bodies: Arc<StdMutex<Vec<String>>>,
}

struct MultimodalHandlerUpstream {
    base_url: String,
    state: MultimodalHandlerUpstreamState,
    task: tokio::task::JoinHandle<()>,
}

impl MultimodalHandlerUpstream {
    async fn start() -> Self {
        let state = MultimodalHandlerUpstreamState::default();
        let app = Router::new()
            .route(
                "/generateAssistantResponse",
                post(multimodal_handler_upstream),
            )
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind multimodal handler upstream probe");
        let address = listener
            .local_addr()
            .expect("multimodal handler upstream probe address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve multimodal handler upstream probe");
        });
        Self {
            base_url: format!("http://{address}"),
            state,
            task,
        }
    }

    fn hits(&self) -> usize {
        self.state.hits.load(Ordering::Acquire)
    }

    fn bodies_snapshot(&self) -> Vec<String> {
        self.state
            .bodies
            .lock()
            .expect("upstream bodies lock")
            .clone()
    }
}

impl Drop for MultimodalHandlerUpstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

const TEST_MCP_RESPONSE_MAX_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Default)]
struct WebSearchHandlerUpstreamState {
    mcp_hits: Arc<AtomicUsize>,
    normal_hits: Arc<AtomicUsize>,
    queries: Arc<StdMutex<Vec<String>>>,
}

impl WebSearchHandlerUpstreamState {
    fn mcp_hits(&self) -> usize {
        self.mcp_hits.load(Ordering::Acquire)
    }

    fn normal_hits(&self) -> usize {
        self.normal_hits.load(Ordering::Acquire)
    }

    fn queries(&self) -> Vec<String> {
        self.queries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

struct WebSearchHandlerUpstream {
    base_url: String,
    state: WebSearchHandlerUpstreamState,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Clone, Default)]
struct ExternalMessagesUpstreamState {
    hits: Arc<AtomicUsize>,
    bodies: Arc<StdMutex<Vec<String>>>,
}

impl ExternalMessagesUpstreamState {
    fn hits(&self) -> usize {
        self.hits.load(Ordering::Acquire)
    }

    fn bodies(&self) -> Vec<String> {
        self.bodies
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

struct ExternalMessagesUpstream {
    base_url: String,
    state: ExternalMessagesUpstreamState,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Clone, Default)]
struct CapturedTestLogs(Arc<StdMutex<Vec<u8>>>);

struct CapturedTestLogWriter(Arc<StdMutex<Vec<u8>>>);

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedTestLogs {
    type Writer = CapturedTestLogWriter;

    fn make_writer(&'a self) -> Self::Writer {
        CapturedTestLogWriter(self.0.clone())
    }
}

impl std::io::Write for CapturedTestLogWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl CapturedTestLogs {
    fn snapshot(&self) -> String {
        String::from_utf8_lossy(
            &self
                .0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
        .into_owned()
    }
}

impl WebSearchHandlerUpstream {
    async fn start() -> Self {
        let state = WebSearchHandlerUpstreamState::default();
        let app = Router::new()
            .route("/mcp", post(websearch_handler_mcp_upstream))
            .route(
                "/generateAssistantResponse",
                post(websearch_handler_normal_upstream),
            )
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake WebSearch/MCP upstream");
        let address = listener.local_addr().expect("fake WebSearch/MCP address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve fake WebSearch/MCP upstream");
        });
        Self {
            base_url: format!("http://{address}"),
            state,
            task,
        }
    }
}

impl Drop for WebSearchHandlerUpstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl ExternalMessagesUpstream {
    async fn start() -> Self {
        let state = ExternalMessagesUpstreamState::default();
        let app = Router::new()
            .route("/v1/messages", post(external_messages_upstream))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake external messages upstream");
        let address = listener
            .local_addr()
            .expect("fake external messages upstream address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve fake external messages upstream");
        });
        Self {
            base_url: format!("http://{address}"),
            state,
            task,
        }
    }
}

impl Drop for ExternalMessagesUpstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn valid_mcp_search_response(request_id: &str, query: &str) -> Value {
    let result = json!({
        "results": [{
            "title": format!("result for {query}"),
            "url": "https://fixture.example.invalid/result",
            "snippet": format!("WEBSEARCH_RAW_RESULT_MARKER snippet for {query}")
        }],
        "totalResults": 1,
        "query": query
    });
    json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "result": {
            "content": [{"type": "text", "text": result.to_string()}],
            "isError": false
        }
    })
}

fn zero_result_mcp_search_response(request_id: &str, query: &str) -> Value {
    let result = json!({
        "results": [],
        "totalResults": 0,
        "query": query
    });
    json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "result": {
            "content": [{"type": "text", "text": result.to_string()}],
            "isError": false
        }
    })
}

async fn websearch_handler_mcp_upstream(
    State(state): State<WebSearchHandlerUpstreamState>,
    body: Bytes,
) -> Response {
    state.mcp_hits.fetch_add(1, Ordering::AcqRel);
    let request = serde_json::from_slice::<Value>(&body).unwrap_or_default();
    let request_id = request
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("missing-id")
        .to_string();
    let query = request
        .pointer("/params/arguments/query")
        .and_then(Value::as_str)
        .unwrap_or("missing-query")
        .to_string();
    state
        .queries
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(query.clone());

    match query.as_str() {
        value if value.starts_with("http-400") => (
            StatusCode::BAD_REQUEST,
            Json(json!({"message": "private-400-marker falsely says 429 timeout"})),
        )
            .into_response(),
        value if value.starts_with("http-429") => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"message": "private-429-marker falsely says 400 timeout"})),
        )
            .into_response(),
        value if value.starts_with("http-500") => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"message": "private-500-marker falsely says 429 timeout 400"})),
        )
            .into_response(),
        value if value.starts_with("header-timeout") => {
            tokio::time::sleep(Duration::from_secs(2)).await;
            Json(valid_mcp_search_response(&request_id, &query)).into_response()
        }
        value if value.starts_with("body-timeout") => {
            let response = valid_mcp_search_response(&request_id, &query).to_string();
            let body = Body::from_stream(stream::once(async move {
                tokio::time::sleep(Duration::from_secs(2)).await;
                Ok::<_, Infallible>(Bytes::from(response))
            }));
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/json")
                .body(body)
                .expect("body-timeout response")
        }
        value if value.starts_with("disconnect") => {
            let body = Body::from_stream(stream::iter([
                Ok::<_, std::io::Error>(Bytes::from_static(b"{")),
                Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "controlled MCP disconnect",
                )),
            ]));
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/json")
                .body(body)
                .expect("disconnect response")
        }
        value if value.starts_with("malformed") => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            "not-json",
        )
            .into_response(),
        value if value.starts_with("jsonrpc-error") => Json(json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "error": {"code": -32000, "message": "private-jsonrpc-marker"}
        }))
        .into_response(),
        value if value.starts_with("is-error") => Json(json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {
                "content": [{"type": "text", "text": "private-is-error-marker"}],
                "isError": true
            }
        }))
        .into_response(),
        value if value.starts_with("non-text-content") => Json(json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {
                "content": [{"type": "image", "text": "private-non-text-marker"}],
                "isError": false
            }
        }))
        .into_response(),
        value if value.starts_with("mismatched-id") => {
            Json(valid_mcp_search_response("different-request-id", &query)).into_response()
        }
        value if value.starts_with("content-length-over-limit") => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/json")
            .header(
                header::CONTENT_LENGTH,
                (TEST_MCP_RESPONSE_MAX_BYTES + 1).to_string(),
            )
            .body(Body::empty())
            .expect("declared over-limit response"),
        value if value.starts_with("chunked-over-limit") => {
            let chunks = (0..17).map(|_| Ok::<_, Infallible>(Bytes::from(vec![b'x'; 256 * 1024])));
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from_stream(stream::iter(chunks)))
                .expect("chunked over-limit response")
        }
        value if value.starts_with("zero-results") => {
            Json(zero_result_mcp_search_response(&request_id, &query)).into_response()
        }
        _ => Json(valid_mcp_search_response(&request_id, &query)).into_response(),
    }
}

async fn websearch_handler_normal_upstream(
    State(state): State<WebSearchHandlerUpstreamState>,
) -> Response {
    state.normal_hits.fetch_add(1, Ordering::AcqRel);
    let mut body = eventstream_test_frame(
        "assistantResponseEvent",
        json!({"content":"normal-tool-path","messageStatus":"COMPLETED"}),
    );
    body.extend(eventstream_test_frame(
        "metadataEvent",
        json!({
            "tokenUsage": {
                "uncachedInputTokens": 10,
                "outputTokens": 2,
                "totalTokens": 12,
                "cacheReadInputTokens": 0,
                "cacheWriteInputTokens": 0
            }
        }),
    ));
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/vnd.amazon.eventstream")],
        body,
    )
        .into_response()
}

async fn external_messages_upstream(
    State(state): State<ExternalMessagesUpstreamState>,
    body: Bytes,
) -> Response {
    state.hits.fetch_add(1, Ordering::AcqRel);
    let body_text = String::from_utf8_lossy(&body).into_owned();
    state
        .bodies
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(body_text.clone());
    let stream = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|value| value.get("stream").and_then(Value::as_bool))
        .unwrap_or(false);
    if stream {
        let events = [
            (
                "message_start",
                json!({
                    "type": "message_start",
                    "message": {
                        "id": "msg_fake_external_websearch",
                        "type": "message",
                        "role": "assistant",
                        "model": "claude-opus-5",
                        "content": [],
                        "stop_reason": null,
                        "stop_sequence": null,
                        "usage": {
                            "input_tokens": 11,
                            "cache_creation_input_tokens": 0,
                            "cache_read_input_tokens": 0,
                            "output_tokens": 1
                        }
                    }
                }),
            ),
            (
                "content_block_start",
                json!({
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": {"type": "text", "text": ""}
                }),
            ),
            (
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {
                        "type": "text_delta",
                        "text": "fake-normalized-external-ok"
                    }
                }),
            ),
            (
                "content_block_stop",
                json!({"type": "content_block_stop", "index": 0}),
            ),
            (
                "message_delta",
                json!({
                    "type": "message_delta",
                    "delta": {"stop_reason": "end_turn", "stop_sequence": null},
                    "usage": {
                        "input_tokens": 11,
                        "cache_creation_input_tokens": 0,
                        "cache_read_input_tokens": 0,
                        "output_tokens": 4
                    }
                }),
            ),
            ("message_stop", json!({"type": "message_stop"})),
        ];
        let mut sse = String::new();
        for (event, payload) in events {
            sse.push_str("event: ");
            sse.push_str(event);
            sse.push_str("\ndata: ");
            sse.push_str(&payload.to_string());
            sse.push_str("\n\n");
        }
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/event-stream")],
            sse,
        )
            .into_response();
    }

    Json(json!({
        "id": "msg_fake_external_websearch",
        "type": "message",
        "role": "assistant",
        "model": "claude-opus-5",
        "content": [{"type": "text", "text": "fake-normalized-external-ok"}],
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {"input_tokens": 11, "output_tokens": 4}
    }))
    .into_response()
}

async fn multimodal_handler_upstream(
    State(state): State<MultimodalHandlerUpstreamState>,
    body: Bytes,
) -> Response {
    state.hits.fetch_add(1, Ordering::AcqRel);
    state
        .bodies
        .lock()
        .expect("upstream bodies lock")
        .push(String::from_utf8_lossy(&body).to_string());
    let mut body = eventstream_test_frame(
        "assistantResponseEvent",
        json!({"content":"inline-ok","messageStatus":"COMPLETED"}),
    );
    body.extend(eventstream_test_frame(
        "metadataEvent",
        json!({
            "tokenUsage": {
                "uncachedInputTokens": 100,
                "outputTokens": 3,
                "totalTokens": 103,
                "cacheReadInputTokens": 0,
                "cacheWriteInputTokens": 0
            }
        }),
    ));
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/vnd.amazon.eventstream")],
        body,
    )
        .into_response()
}

fn multimodal_handler_test_provider(config: Config) -> Arc<KiroProvider> {
    let credentials = vec![KiroCredentials {
        id: Some(1),
        access_token: Some("multimodal-handler-test-token".to_string()),
        profile_arn: Some(
            "arn:aws:codewhisperer:us-east-1:123456789012:profile/HANDLER_TEST".to_string(),
        ),
        expires_at: Some((Utc::now() + chrono::Duration::hours(1)).to_rfc3339()),
        auth_method: Some("social".to_string()),
        rate_limit_auto_disable_enabled: Some(false),
        ..Default::default()
    }];
    let manager = Arc::new(
        MultiTokenManager::new(config, credentials, None, None, false)
            .expect("build multimodal handler token manager"),
    );
    let mut endpoints: HashMap<String, Arc<dyn KiroEndpoint>> = HashMap::new();
    endpoints.insert("ide".to_string(), Arc::new(IdeEndpoint));
    Arc::new(KiroProvider::with_proxy(
        manager,
        None,
        endpoints,
        "ide".to_string(),
    ))
}

fn multimodal_handler_test_router(base_url: &str) -> Router {
    multimodal_handler_test_router_with_usage(base_url).0
}

fn multimodal_handler_test_router_with_usage(base_url: &str) -> (Router, Arc<UsageRecorder>) {
    let mut config = Config::default();
    config.kiro_upstream_base_url = Some(base_url.to_string());
    config.kiro_upstream_response_timeout_secs = 2;
    config.credential_retry_max_attempts = 1;
    config.defined_cache_routes = vec!["/dfcache/demo".to_string()];
    multimodal_handler_test_router_from_config(config)
}

fn multimodal_handler_test_router_from_config(config: Config) -> (Router, Arc<UsageRecorder>) {
    let provider = multimodal_handler_test_provider(config.clone());
    let usage_recorder = Arc::new(UsageRecorder::new(1_000));
    let router = create_router_with_provider(
        AnthropicRouterDependencies {
            request_api_keys: Arc::new(RequestApiKeyStore::new(["b07-handler-key"])),
            request_admission: Arc::new(RequestAdmissionController::new(
                RequestAdmissionConfig::disabled(),
            )),
            kiro_provider: Some(provider),
            usage_recorder: usage_recorder.clone(),
            prompt_cache: Arc::new(PromptCacheTracker::default()),
            prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
            pricing_catalog: Arc::new(PricingCatalog::new()),
            model_capabilities: Arc::new(ModelCapabilitiesCatalog::new()),
            external_pool_manager: None,
        },
        AnthropicRouterConfig::from_runtime_config(&config),
    );
    (router, usage_recorder)
}

async fn run_strict_request_protocol_contamination_fails_closed_before_upstream_for_five_rounds() {
    let upstream = MultimodalHandlerUpstream::start().await;
    let mut config = Config::default();
    config.kiro_upstream_base_url = Some(upstream.base_url.clone());
    config.kiro_upstream_response_timeout_secs = 2;
    config.credential_retry_max_attempts = 1;
    config.compat_profile = CompatProfile::AnthropicStrict;
    let (app, usage_recorder) = multimodal_handler_test_router_from_config(config);

    for round in 1..=5 {
        let hidden = format!("credential-like-output-{round}");
        let response = app
            .clone()
            .oneshot(multimodal_handler_request(
                "/v1/messages",
                json!({
                    "model": "claude-sonnet-4-20250514",
                    "max_tokens": 32,
                    "stream": false,
                    "messages": [{
                        "role": "assistant",
                        "content": format!("safe prefix\nuser Continue\n\nbashHashd1e9567d: {hidden}")
                    }],
                    "tools": [{"name": "bashHashd1e9567d", "input_schema": {"type": "object"}}]
                })
                .to_string(),
            ))
            .await
            .expect("strict contamination response");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "round {round}");
        let request_id = response
            .headers()
            .get("request-id")
            .and_then(|value| value.to_str().ok())
            .expect("request-id")
            .to_string();
        assert_eq!(
            response
                .headers()
                .get("anthropic-request-id")
                .and_then(|value| value.to_str().ok()),
            Some(request_id.as_str())
        );
        assert_eq!(
            response
                .headers()
                .get("x-error-id")
                .and_then(|value| value.to_str().ok()),
            Some(request_id.as_str())
        );
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("strict contamination body");
        let body_text = String::from_utf8(body.to_vec()).expect("utf-8 error body");
        let value: Value = serde_json::from_str(&body_text).expect("error JSON");
        assert_eq!(value["request_id"], request_id);
        assert!(
            value["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains(&request_id))
        );
        assert!(!body_text.contains(&hidden));
        assert!(!body_text.contains("bashHashd1e9567d"));

        let records = usage_recorder.query(UsageRecordQuery {
            request_id: Some(request_id.clone()),
            ..UsageRecordQuery::default()
        });
        assert_eq!(records.records.len(), 1, "round {round}");
        let record = records.records.first().expect("strict error usage record");
        assert_eq!(record.status, UsageRecordStatus::Error);
        assert_eq!(record.error_source.as_deref(), Some("request_rejection"));
        assert_eq!(record.error_status_code, Some(400));
        assert_eq!(record.error_id.as_deref(), Some(request_id.as_str()));
        assert_eq!(record.total_input_tokens, 0);
        assert_eq!(record.output_tokens, 0);
        let metadata = record.error_metadata.as_ref().expect("error metadata");
        assert_eq!(metadata["stage"], "request_entry");
        assert_eq!(metadata["sampled"], true);
        assert_eq!(metadata["observedCountIsExact"], false);
        assert_eq!(metadata["observedCount"], round);
        assert_eq!(
            metadata.get("reason").and_then(Value::as_str),
            Some("strict_request_protocol_contamination")
        );
        let serialized_record = serde_json::to_string(record).expect("serialize usage record");
        assert!(!serialized_record.contains(&hidden));
        assert!(!serialized_record.contains("bashHashd1e9567d"));
    }

    assert_eq!(
        upstream.hits(),
        0,
        "strict contamination must never hit upstream"
    );
}

#[test]
fn strict_request_protocol_contamination_fails_closed_before_upstream_for_five_rounds() {
    run_handler_fixture_on_four_mib_thread("strict-contamination-matrix", || async {
        run_strict_request_protocol_contamination_fails_closed_before_upstream_for_five_rounds()
            .await;
    });
}

async fn run_local_non_stream_success_commits_shared_attempt_budget_before_usage_for_five_rounds() {
    let upstream = MultimodalHandlerUpstream::start().await;
    let (router, usage_recorder) = multimodal_handler_test_router_with_usage(&upstream.base_url);

    for round in 1..=5 {
        let response = router
            .clone()
            .oneshot(multimodal_handler_request(
                "/v1/messages",
                json!({
                    "model": "claude-sonnet-4-20250514",
                    "max_tokens": 32,
                    "stream": false,
                    "messages": [{"role": "user", "content": format!("round {round}")}]
                })
                .to_string(),
            ))
            .await
            .expect("local non-stream request");
        assert_eq!(response.status(), StatusCode::OK, "round {round}");
        let request_id = response
            .headers()
            .get("request-id")
            .and_then(|value| value.to_str().ok())
            .expect("non-stream response request id")
            .to_string();
        axum::body::to_bytes(response.into_body(), 256 * 1024)
            .await
            .expect("read local non-stream response");

        let records = usage_recorder.query(UsageRecordQuery {
            request_id: Some(request_id),
            ..UsageRecordQuery::default()
        });
        let record = records
            .records
            .first()
            .expect("non-stream success usage record");
        let attempts = record
            .latency_trace
            .as_ref()
            .and_then(|trace| trace.inference_attempts)
            .expect("non-stream inference attempt snapshot");
        assert_eq!(attempts.consumed, 1, "round {round}");
        assert!(attempts.downstream_committed, "round {round}");
    }
    assert_eq!(upstream.hits(), 5);
}

#[test]
fn local_non_stream_success_commits_shared_attempt_budget_before_usage_for_five_rounds() {
    run_handler_fixture_on_four_mib_thread("local-non-stream-matrix", || async {
        run_local_non_stream_success_commits_shared_attempt_budget_before_usage_for_five_rounds()
            .await;
    });
}

fn websearch_handler_test_router(base_url: &str) -> (Router, Arc<UsageRecorder>) {
    let mut config = Config::default();
    config.kiro_upstream_base_url = Some(base_url.to_string());
    config.kiro_upstream_response_timeout_secs = 1;
    config.credential_retry_max_attempts = 1;
    config.credential_transient_cooldown_secs = 0;
    config.credential_rate_limit_cooldown_secs = 0;
    config.credential_server_error_cooldown_secs = 0;
    config.credential_network_error_cooldown_secs = 0;
    config.credential_stream_error_cooldown_secs = 0;
    config.credential_protocol_error_cooldown_secs = 0;
    config.credential_auth_error_cooldown_secs = 0;
    config.credential_cooldown_backoff_multiplier = 1.0;
    config.credential_cooldown_jitter_percent = 0;
    config.credential_max_cooldown_secs = 0;
    let credentials = (1..=80)
        .map(|id| KiroCredentials {
            id: Some(id),
            access_token: Some(format!("websearch-handler-test-token-{id}")),
            profile_arn: Some(
                "arn:aws:codewhisperer:us-east-1:123456789012:profile/WEBSEARCH_TEST".to_string(),
            ),
            expires_at: Some((Utc::now() + chrono::Duration::hours(1)).to_rfc3339()),
            auth_method: Some("social".to_string()),
            rate_limit_auto_disable_enabled: Some(false),
            ..Default::default()
        })
        .collect();
    let manager = Arc::new(
        MultiTokenManager::new(config.clone(), credentials, None, None, false)
            .expect("build WebSearch handler token manager"),
    );
    let mut endpoints: HashMap<String, Arc<dyn KiroEndpoint>> = HashMap::new();
    endpoints.insert("ide".to_string(), Arc::new(IdeEndpoint));
    let provider = Arc::new(KiroProvider::with_proxy(
        manager,
        None,
        endpoints,
        "ide".to_string(),
    ));
    let usage_recorder = Arc::new(UsageRecorder::new(1_000));
    let router = create_router_with_provider(
        AnthropicRouterDependencies {
            request_api_keys: Arc::new(RequestApiKeyStore::new(["b07-handler-key"])),
            request_admission: Arc::new(RequestAdmissionController::new(
                RequestAdmissionConfig::disabled(),
            )),
            kiro_provider: Some(provider),
            usage_recorder: usage_recorder.clone(),
            prompt_cache: Arc::new(PromptCacheTracker::default()),
            prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
            pricing_catalog: Arc::new(PricingCatalog::new()),
            model_capabilities: Arc::new(ModelCapabilitiesCatalog::new()),
            external_pool_manager: None,
        },
        AnthropicRouterConfig::from_runtime_config(&config),
    );
    (router, usage_recorder)
}

async fn test_external_pool_manager_for_handlers(
    base_url: &str,
    body_mode: ExternalPoolRequestBodyMode,
) -> Option<Arc<ExternalPoolManager>> {
    let Some(postgres_url) = crate::storage::integration_test_url("KIRO_RS_TEST_POSTGRES_URL")
    else {
        eprintln!("跳过 WebSearch external fallback 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
        return None;
    };
    let Some(redis_url) = crate::storage::integration_test_url("KIRO_RS_TEST_REDIS_URL") else {
        eprintln!("跳过 WebSearch external fallback 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
        return None;
    };

    let mut postgres_config = Config::default();
    postgres_config.postgres.url = Some(postgres_url);
    postgres_config.postgres.max_connections = 2;
    let postgres = Arc::new(
        PostgresStore::connect_test(&postgres_config)
            .await
            .expect("connect handler external fallback test Postgres"),
    );

    let mut redis_config = Config::default();
    redis_config.redis.url = Some(redis_url);
    redis_config.redis.key_prefix =
        format!("kiro_rs:test:handlers:websearch:{}", uuid::Uuid::new_v4());
    let redis = Arc::new(
        RedisStore::connect(&redis_config)
            .await
            .expect("connect handler external fallback test Redis"),
    );

    let manager = Arc::new(ExternalPoolManager::new(postgres.clone(), redis));
    postgres
        .create_external_pool(CreateExternalPoolRequest {
            name: format!("handler-websearch-{body_mode:?}"),
            base_url: base_url.to_string(),
            api_key: "sk-handler-external-test".to_string(),
            auth_type: ExternalPoolAuthType::XApiKey,
            enabled: true,
            priority: 1,
            max_concurrent_requests: 10,
            usage_projection_mode: ExternalPoolUsageProjectionMode::PassThrough,
            stream_response_mode: None,
            request_body_mode: body_mode,
            raw_model_mode: ExternalPoolRawModelMode::None,
            auto_disable_policy: ExternalPoolAutoDisablePolicy::Inherit,
            pre_output_stream_retry_mode: ExternalPoolStreamRetryMode::Inherit,
            preserve_path: true,
            normalize_model_version_dots: false,
            model_mapping_mode: ExternalPoolModelMappingMode::ProcessedMapping,
            model_mapping_require_match: false,
            model_mapping_rules: Vec::new(),
            supported_models: Vec::new(),
            notes: None,
        })
        .await
        .expect("create handler external fallback pool");
    manager.invalidate_static_pool_snapshot();
    Some(manager)
}

fn websearch_handler_test_router_with_external(
    kiro_base_url: &str,
    external_pool_manager: Arc<ExternalPoolManager>,
) -> (Router, Arc<UsageRecorder>) {
    websearch_handler_test_router_with_external_options(
        kiro_base_url,
        external_pool_manager,
        Vec::new(),
        true,
        false,
    )
}

fn websearch_handler_test_router_with_external_options(
    kiro_base_url: &str,
    external_pool_manager: Arc<ExternalPoolManager>,
    credentials: Vec<KiroCredentials>,
    local_pool_preflight_enabled: bool,
    external_direct_policy_enabled: bool,
) -> (Router, Arc<UsageRecorder>) {
    let mut config = Config::default();
    config.kiro_upstream_base_url = Some(kiro_base_url.to_string());
    config.kiro_upstream_response_timeout_secs = 1;
    config.credential_retry_max_attempts = 0;
    config.external_pools.external_pools_enabled = true;
    config.external_pools.external_direct_policy_enabled = external_direct_policy_enabled;
    config.external_pools.local_pool_preflight_enabled = local_pool_preflight_enabled;
    config.external_pools.fallback_on_no_available_credentials = true;
    config.external_pools.fallback_on_local_capacity_exhausted = true;
    config.external_pools.fallback_on_scheduler_redis_degraded = true;
    config.external_pools.fallback_on_local_transient_exhausted = true;
    config
        .external_pools
        .external_pool_global_max_concurrent_requests = 20;
    config.external_pools.external_pool_max_queued_requests = 0;
    config.external_pools.external_pool_retry_max_attempts = 0;
    config.external_pools.external_pool_request_timeout_secs = 10;
    config
        .external_pools
        .external_pool_stream_request_timeout_secs = 10;
    config.external_pools.external_pool_stream_idle_timeout_secs = 10;

    let manager = Arc::new(
        MultiTokenManager::new(config.clone(), credentials, None, None, false)
            .expect("build WebSearch external fallback token manager"),
    );
    let mut endpoints: HashMap<String, Arc<dyn KiroEndpoint>> = HashMap::new();
    endpoints.insert("ide".to_string(), Arc::new(IdeEndpoint));
    let provider = Arc::new(KiroProvider::with_proxy(
        manager,
        None,
        endpoints,
        "ide".to_string(),
    ));
    let usage_recorder = Arc::new(UsageRecorder::new(1_000));
    let router = create_router_with_provider(
        AnthropicRouterDependencies {
            request_api_keys: Arc::new(RequestApiKeyStore::new(["b07-handler-key"])),
            request_admission: Arc::new(RequestAdmissionController::new(
                RequestAdmissionConfig::disabled(),
            )),
            kiro_provider: Some(provider),
            usage_recorder: usage_recorder.clone(),
            prompt_cache: Arc::new(PromptCacheTracker::default()),
            prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
            pricing_catalog: Arc::new(PricingCatalog::new()),
            model_capabilities: Arc::new(ModelCapabilitiesCatalog::new()),
            external_pool_manager: Some(external_pool_manager),
        },
        AnthropicRouterConfig::from_runtime_config(&config),
    );
    (router, usage_recorder)
}

fn websearch_messages_body(
    messages: Vec<Value>,
    stream: bool,
    tool_type: Option<&str>,
    include_second_tool: bool,
) -> String {
    let mut tools = vec![json!({
        "name": "web_search",
        "type": tool_type,
        "max_uses": 8,
        "description": "same name custom tool",
        "input_schema": {"type": "object"}
    })];
    if include_second_tool {
        tools.push(json!({
            "name": "fixture",
            "description": "ordinary tool",
            "input_schema": {"type": "object"}
        }));
    }
    json!({
        "model": "claude-sonnet-4-20250514",
        "max_tokens": 256,
        "stream": stream,
        "messages": messages,
        "tools": tools
    })
    .to_string()
}

fn single_query_websearch_body(query: &str, stream: bool) -> String {
    websearch_messages_body(
        vec![json!({"role": "user", "content": query})],
        stream,
        Some("web_search_20250305"),
        false,
    )
}

fn response_request_id(response: &Response) -> String {
    response
        .headers()
        .get("request-id")
        .and_then(|value| value.to_str().ok())
        .expect("response request-id")
        .to_string()
}

fn usage_record_for_request(recorder: &UsageRecorder, request_id: &str) -> UsageRecord {
    let records = recorder.query(UsageRecordQuery {
        request_id: Some(request_id.to_string()),
        ..UsageRecordQuery::default()
    });
    assert_eq!(records.records.len(), 1, "one usage record per request");
    records.records.into_iter().next().unwrap()
}

fn assert_websearch_usage_attribution(
    record: &UsageRecord,
    context: &str,
    forbidden_markers: &[&str],
) {
    let request_api_key_id = record
        .request_api_key_id
        .as_deref()
        .expect("WebSearch usage must retain the stable request-key channel ID");
    assert_eq!(request_api_key_id.len(), 64, "{context}");
    assert!(
        request_api_key_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit()),
        "{context}: request-key channel ID must be a hexadecimal digest"
    );
    assert_ne!(request_api_key_id, "b07-handler-key", "{context}");
    assert_eq!(
        record.route_kind,
        Some(crate::anthropic::usage::UsageRouteKind::LocalCredential),
        "{context}"
    );
    let credential_id = record
        .credential_id
        .expect("WebSearch usage must retain the selected credential");
    assert!(
        (1..=80).contains(&credential_id),
        "{context}: unexpected credential id {credential_id}"
    );
    assert!(
        !record.credential_attempts.is_empty(),
        "{context}: WebSearch usage lost all MCP attempts"
    );
    assert!(
        record.credential_attempts.len() <= 4,
        "{context}: MCP attempt chain exceeded the shared hard budget: {:?}",
        record.credential_attempts
    );
    let final_attempt = record
        .credential_attempts
        .last()
        .expect("non-empty WebSearch attempt chain");
    assert_eq!(
        final_attempt.credential_id, credential_id,
        "{context}: selected credential must match the final MCP attempt"
    );
    assert_eq!(
        final_attempt.credential_label.as_deref(),
        record.credential_label.as_deref(),
        "{context}: selected credential label must match the final MCP attempt"
    );

    for (index, attempt) in record.credential_attempts.iter().enumerate() {
        assert_eq!(attempt.attempt as usize, index + 1, "{context}");
        assert!(
            matches!(attempt.action.as_str(), "success" | "retry" | "fail"),
            "{context}: unstable MCP attempt action {:?}",
            attempt.action
        );
        for (field, marker) in [
            ("error_type", attempt.error_type.as_deref()),
            ("error_message", attempt.error_message.as_deref()),
        ] {
            let Some(marker) = marker else {
                continue;
            };
            assert!(
                marker.len() <= 64
                    && marker.bytes().all(|byte| byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || byte == b'_'),
                "{context}: {field} must be a fixed taxonomy marker, got {marker:?}"
            );
        }
    }

    let attempts = record
        .latency_trace
        .as_ref()
        .and_then(|trace| trace.inference_attempts)
        .expect("WebSearch usage inference attempt snapshot");
    assert_eq!(
        attempts.consumed as usize,
        record.credential_attempts.len(),
        "{context}: real MCP sends and attributed attempts must agree"
    );
    assert!(attempts.consumed <= attempts.max_attempts, "{context}");
    assert!(attempts.consumed <= 4, "{context}");
    assert_eq!(attempts.local_attempts, 0, "{context}");
    assert_eq!(attempts.external_attempts, 0, "{context}");
    assert_eq!(
        attempts.mcp_attempts, attempts.consumed,
        "{context}: every real MCP send must use the explicit MCP channel"
    );

    let attribution = serde_json::to_string(&(
        record.credential_id,
        &record.credential_label,
        &record.credential_attempts,
    ))
    .expect("serialize WebSearch attribution");
    for marker in forbidden_markers {
        assert!(
            !attribution.contains(marker),
            "{context}: WebSearch attribution captured raw marker {marker:?}: {attribution}"
        );
    }
}

async fn run_websearch_canonical_detection_and_current_long_history_query_are_exact_for_five_rounds()
 {
    let upstream = WebSearchHandlerUpstream::start().await;
    let (router, _usage_recorder) = websearch_handler_test_router(&upstream.base_url);

    for round in 1..=5 {
        let canonical_query = format!("canonical-current-{round}");
        let canonical = router
            .clone()
            .oneshot(multimodal_handler_request(
                "/cc/v1/messages",
                websearch_messages_body(
                    vec![
                        json!({"role": "user", "content": format!("stale-{round}")}),
                        json!({"role": "assistant", "content": "old answer"}),
                        json!({
                            "role": "user",
                            "content": [{
                                "type": "text",
                                "text": format!("Perform a web search for the query: {canonical_query}")
                            }]
                        }),
                    ],
                    false,
                    Some("web_search_20250305"),
                    false,
                ),
            ))
            .await
            .expect("canonical WebSearch response");
        assert_eq!(canonical.status(), StatusCode::OK, "round {round}");
        axum::body::to_bytes(canonical.into_body(), 256 * 1024)
            .await
            .expect("canonical response body");

        let custom = router
            .clone()
            .oneshot(multimodal_handler_request(
                "/cc/v1/messages",
                websearch_messages_body(
                    vec![json!({"role": "user", "content": format!("custom-{round}")})],
                    false,
                    None,
                    false,
                ),
            ))
            .await
            .expect("same-name custom tool response");
        assert_eq!(custom.status(), StatusCode::OK, "round {round}");
        axum::body::to_bytes(custom.into_body(), 256 * 1024)
            .await
            .expect("same-name custom body");

        let mixed = router
            .clone()
            .oneshot(multimodal_handler_request(
                "/cc/v1/messages",
                websearch_messages_body(
                    vec![json!({"role": "user", "content": format!("mixed-{round}")})],
                    false,
                    Some("web_search_20250305"),
                    true,
                ),
            ))
            .await
            .expect("mixed tools response");
        assert_eq!(mixed.status(), StatusCode::OK, "round {round}");
        let mixed_body = axum::body::to_bytes(mixed.into_body(), 256 * 1024)
            .await
            .expect("mixed tools body");
        let mixed_body = String::from_utf8_lossy(&mixed_body);
        assert!(
            mixed_body.contains(r#""type":"web_search_tool_result""#),
            "round {round}: {mixed_body}"
        );
    }

    assert_eq!(upstream.state.mcp_hits(), 10);
    assert_eq!(upstream.state.normal_hits(), 5);
    assert_eq!(
        upstream.state.queries(),
        (1..=5)
            .flat_map(|round| {
                [
                    format!("canonical-current-{round}"),
                    format!("mixed-{round}"),
                ]
            })
            .collect::<Vec<_>>()
    );

    for tool_cycles in [20, 100] {
        for round in 1..=5 {
            let mut messages = vec![json!({"role": "user", "content": "stale query"})];
            for cycle in 0..tool_cycles {
                messages.push(json!({
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": format!("tool-{cycle}"),
                        "name": "fixture",
                        "input": {"cycle": cycle}
                    }]
                }));
                messages.push(json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": format!("tool-{cycle}"),
                        "content": format!("result-{cycle}")
                    }]
                }));
            }
            let current = format!("long-current-{tool_cycles}-{round}");
            messages.push(json!({"role": "user", "content": current}));
            let response = router
                .clone()
                .oneshot(multimodal_handler_request(
                    "/cc/v1/messages",
                    websearch_messages_body(messages, false, Some("web_search_20250305"), false),
                ))
                .await
                .expect("long history WebSearch response");
            assert_eq!(response.status(), StatusCode::OK);
            axum::body::to_bytes(response.into_body(), 256 * 1024)
                .await
                .expect("long history body");
        }
    }
    let queries = upstream.state.queries();
    for tool_cycles in [20, 100] {
        for round in 1..=5 {
            assert!(
                queries.contains(&format!("long-current-{tool_cycles}-{round}")),
                "captured current query for {tool_cycles} cycles round {round}"
            );
        }
    }
}

#[test]
fn websearch_canonical_detection_and_current_long_history_query_are_exact_for_five_rounds() {
    run_handler_fixture_on_four_mib_thread("websearch-routing-matrix", || async {
        run_websearch_canonical_detection_and_current_long_history_query_are_exact_for_five_rounds(
        )
        .await;
    });
}

async fn run_native_websearch_current_official_and_future_version_formats_route_to_mcp() {
    let upstream = WebSearchHandlerUpstream::start().await;
    let (router, _usage_recorder) = websearch_handler_test_router(&upstream.base_url);

    for tool_type in [
        "web_search_20250305",
        "web_search_20260209",
        "web_search_20260318",
        "web_search_20270101",
    ] {
        let query = format!("official-version-{tool_type}");
        let response = router
            .clone()
            .oneshot(multimodal_handler_request(
                "/cc/v1/messages",
                websearch_messages_body(
                    vec![json!({"role": "user", "content": query})],
                    false,
                    Some(tool_type),
                    false,
                ),
            ))
            .await
            .expect("official WebSearch version response");
        assert_eq!(response.status(), StatusCode::OK, "{tool_type}");
        let body = axum::body::to_bytes(response.into_body(), 256 * 1024)
            .await
            .expect("official WebSearch body");
        assert!(
            String::from_utf8_lossy(&body).contains(r#""type":"web_search_tool_result""#),
            "{tool_type}"
        );
    }

    assert_eq!(upstream.state.mcp_hits(), 4);
    assert_eq!(upstream.state.normal_hits(), 0);
    assert_eq!(
        upstream.state.queries(),
        vec![
            "official-version-web_search_20250305".to_string(),
            "official-version-web_search_20260209".to_string(),
            "official-version-web_search_20260318".to_string(),
            "official-version-web_search_20270101".to_string(),
        ]
    );
}

#[test]
fn native_websearch_current_official_and_future_version_formats_route_to_mcp() {
    run_handler_fixture_on_four_mib_thread("websearch-official-versions", || async {
        run_native_websearch_current_official_and_future_version_formats_route_to_mcp().await;
    });
}

async fn run_native_websearch_normalized_external_preflight_precedes_mcp_for_five_rounds() {
    let mcp_upstream = WebSearchHandlerUpstream::start().await;
    let external_upstream = ExternalMessagesUpstream::start().await;
    let Some(external_pool_manager) = test_external_pool_manager_for_handlers(
        &external_upstream.base_url,
        ExternalPoolRequestBodyMode::Normalized,
    )
    .await
    else {
        return;
    };
    let (router, usage_recorder) =
        websearch_handler_test_router_with_external(&mcp_upstream.base_url, external_pool_manager);

    for stream in [false, true] {
        for round in 1..=5 {
            let query = format!("normalized-external-websearch-{stream}-{round}");
            let response = router
                .clone()
                .oneshot(multimodal_handler_request(
                    "/ha/v1/messages",
                    single_query_websearch_body(&query, stream),
                ))
                .await
                .expect("normalized external WebSearch response");
            let request_id = response_request_id(&response);
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "stream={stream} round={round}"
            );
            let body = axum::body::to_bytes(response.into_body(), 256 * 1024)
                .await
                .expect("normalized external WebSearch body");
            let body = String::from_utf8(body.to_vec()).expect("external response UTF-8");
            assert!(
                body.contains("fake-normalized-external-ok"),
                "stream={stream} round={round} body={body}"
            );
            let record = usage_record_for_request(&usage_recorder, &request_id);
            assert_eq!(
                record.status,
                UsageRecordStatus::Success,
                "stream={stream} round={round}"
            );
            assert_eq!(
                record.route_kind,
                Some(UsageRouteKind::ExternalPool),
                "stream={stream} round={round}"
            );
            assert_eq!(
                record.route_subtype,
                Some(UsageRouteSubtype::ExternalFallbackPreflight),
                "stream={stream} round={round}"
            );
            assert_eq!(
                record.fallback_reason.as_deref(),
                Some("local_no_credentials"),
                "stream={stream} round={round}"
            );
            assert_eq!(
                record.local_attempted,
                Some(false),
                "stream={stream} round={round}"
            );
            let attempts = record
                .latency_trace
                .as_ref()
                .and_then(|trace| trace.inference_attempts)
                .expect("normalized external attempt trace");
            assert_eq!(attempts.mcp_attempts, 0, "stream={stream} round={round}");
            assert_eq!(
                attempts.external_attempts, 1,
                "stream={stream} round={round}"
            );
        }
    }

    assert_eq!(
        external_upstream.state.hits(),
        10,
        "every normalized fallback request must reach external pool"
    );
    assert_eq!(
        mcp_upstream.state.mcp_hits(),
        0,
        "native WebSearch must not call MCP when normalized external fallback is eligible"
    );
    let bodies = external_upstream.state.bodies();
    assert_eq!(bodies.len(), 10);
    assert!(
        bodies
            .iter()
            .all(|body| body.contains("web_search_20250305")),
        "external fallback must preserve official WebSearch tool payload: {bodies:?}"
    );
}

#[test]
fn native_websearch_normalized_external_preflight_precedes_mcp_for_five_rounds() {
    run_handler_fixture_on_four_mib_thread("websearch-normalized-external", || async {
        run_native_websearch_normalized_external_preflight_precedes_mcp_for_five_rounds().await;
    });
}

async fn run_normalized_external_direct_policy_skips_raw_preparse_without_raw_pool() {
    let kiro_upstream = WebSearchHandlerUpstream::start().await;
    let external_upstream = ExternalMessagesUpstream::start().await;
    let Some(external_pool_manager) = test_external_pool_manager_for_handlers(
        &external_upstream.base_url,
        ExternalPoolRequestBodyMode::Normalized,
    )
    .await
    else {
        return;
    };
    let (router, usage_recorder) = websearch_handler_test_router_with_external_options(
        &kiro_upstream.base_url,
        external_pool_manager,
        Vec::new(),
        false,
        true,
    );

    for stream in [false, true] {
        let response = router
            .clone()
            .oneshot(multimodal_handler_request(
                "/cc/v1/messages",
                json!({
                    "model": "claude-opus-4-6-thinking",
                    "max_tokens": 32,
                    "stream": stream,
                    "messages": [{"role": "user", "content": format!("hi stream={stream}")}]
                })
                .to_string(),
            ))
            .await
            .expect("normalized direct external response");
        let request_id = response_request_id(&response);
        assert_eq!(response.status(), StatusCode::OK, "stream={stream}");
        let body = axum::body::to_bytes(response.into_body(), 256 * 1024)
            .await
            .expect("normalized direct external body");
        let body = String::from_utf8(body.to_vec()).expect("external response UTF-8");
        assert!(
            body.contains("fake-normalized-external-ok"),
            "stream={stream} normalized direct body={body}"
        );

        let record = usage_record_for_request(&usage_recorder, &request_id);
        assert_eq!(record.status, UsageRecordStatus::Success, "stream={stream}");
        assert_eq!(
            record.route_kind,
            Some(UsageRouteKind::ExternalPool),
            "stream={stream}"
        );
        assert_eq!(
            record.route_subtype,
            Some(UsageRouteSubtype::ExternalDirectPolicy),
            "stream={stream}"
        );
        assert_eq!(
            record.direct_policy_reason.as_deref(),
            Some("explicit_direct"),
            "stream={stream}"
        );
        assert_eq!(record.model, "claude-opus-4-6-thinking", "stream={stream}");
        assert_eq!(
            record.upstream_model.as_deref(),
            Some("claude-opus-4.6"),
            "stream={stream}"
        );
        assert_eq!(
            record.external_outbound_model.as_deref(),
            Some("claude-opus-4.6"),
            "stream={stream}"
        );
        assert!(record.model_resolution_source.is_some(), "stream={stream}");
        let attempts = record
            .latency_trace
            .as_ref()
            .and_then(|trace| trace.inference_attempts)
            .expect("normalized direct external attempt trace");
        assert_eq!(attempts.external_attempts, 1, "stream={stream}");
    }
    assert_eq!(
        external_upstream.state.hits(),
        2,
        "normalized direct stream and non-stream requests must reach external pool"
    );
    let bodies = external_upstream.state.bodies();
    assert_eq!(bodies.len(), 2);
    for body in bodies {
        let outbound: Value = serde_json::from_str(&body).expect("external body json");
        assert_eq!(outbound["model"], "claude-opus-4.6");
    }
    assert_eq!(
        kiro_upstream.state.normal_hits(),
        0,
        "direct external policy must not call local Kiro upstream for stream or non-stream"
    );
}

#[test]
fn normalized_external_direct_policy_skips_raw_preparse_without_raw_pool() {
    run_handler_fixture_on_four_mib_thread("normalized-external-direct-raw-guard", || async {
        run_normalized_external_direct_policy_skips_raw_preparse_without_raw_pool().await;
    });
}

async fn run_native_websearch_scheduler_failure_falls_back_to_external_after_mcp_path_for_five_rounds()
 {
    let mcp_upstream = WebSearchHandlerUpstream::start().await;
    let external_upstream = ExternalMessagesUpstream::start().await;
    let Some(external_pool_manager) = test_external_pool_manager_for_handlers(
        &external_upstream.base_url,
        ExternalPoolRequestBodyMode::Normalized,
    )
    .await
    else {
        return;
    };
    let (router, usage_recorder) = websearch_handler_test_router_with_external_options(
        &mcp_upstream.base_url,
        external_pool_manager,
        Vec::new(),
        false,
        false,
    );

    for stream in [false, true] {
        for round in 1..=5 {
            let query = format!("scheduler-after-mcp-websearch-{stream}-{round}");
            let response = router
                .clone()
                .oneshot(multimodal_handler_request(
                    "/ha/v1/messages",
                    single_query_websearch_body(&query, stream),
                ))
                .await
                .expect("post-MCP external fallback WebSearch response");
            let request_id = response_request_id(&response);
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "stream={stream} round={round}"
            );
            let body = axum::body::to_bytes(response.into_body(), 256 * 1024)
                .await
                .expect("post-MCP external fallback body");
            let body = String::from_utf8(body.to_vec()).expect("external response UTF-8");
            assert!(
                body.contains("fake-normalized-external-ok"),
                "stream={stream} round={round} body={body}"
            );

            let record = usage_record_for_request(&usage_recorder, &request_id);
            assert_eq!(
                record.status,
                UsageRecordStatus::Success,
                "stream={stream} round={round}"
            );
            assert_eq!(
                record.route_kind,
                Some(UsageRouteKind::ExternalPool),
                "stream={stream} round={round}"
            );
            assert_eq!(
                record.fallback_reason.as_deref(),
                Some("local_no_credentials"),
                "stream={stream} round={round}: scheduler failure must keep the real local-pool reason"
            );
            assert_eq!(
                record.local_attempted,
                Some(true),
                "stream={stream} round={round}: fallback must record that the local MCP path was attempted"
            );
            let attempts = record
                .latency_trace
                .as_ref()
                .and_then(|trace| trace.inference_attempts)
                .expect("post-MCP fallback attempt trace");
            assert_eq!(attempts.mcp_attempts, 0, "stream={stream} round={round}");
            assert_eq!(
                attempts.external_attempts, 1,
                "stream={stream} round={round}"
            );
        }
    }

    assert_eq!(
        external_upstream.state.hits(),
        10,
        "every post-MCP fallback request must reach external pool"
    );
    assert_eq!(
        mcp_upstream.state.mcp_hits(),
        0,
        "scheduler failure before credential acquisition must not send MCP HTTP traffic"
    );
}

#[test]
fn native_websearch_scheduler_failure_falls_back_to_external_after_mcp_path_for_five_rounds() {
    run_handler_fixture_on_four_mib_thread("websearch-after-mcp-external", || async {
        run_native_websearch_scheduler_failure_falls_back_to_external_after_mcp_path_for_five_rounds(
        )
        .await;
    });
}

async fn run_websearch_latest_non_text_or_blank_user_turn_rejects_without_mcp_for_five_rounds() {
    let upstream = WebSearchHandlerUpstream::start().await;
    let (router, usage_recorder) = websearch_handler_test_router(&upstream.base_url);
    let expected_key_id = crate::common::auth::request_api_key_id("b07-handler-key");

    for round in 1..=5 {
        for current_content in [
            json!([{
                "type": "tool_result",
                "tool_use_id": format!("tool-{round}"),
                "content": "current turn has no query"
            }]),
            json!([{"type": "text", "text": "   \n\t  "}]),
        ] {
            let stale_query = format!("STALE_WEBSEARCH_QUERY_MARKER_{round}");
            let response = router
                .clone()
                .oneshot(multimodal_handler_request(
                    "/cc/v1/messages",
                    websearch_messages_body(
                        vec![
                            json!({"role": "user", "content": stale_query}),
                            json!({"role": "assistant", "content": "old answer"}),
                            json!({"role": "user", "content": current_content}),
                        ],
                        round % 2 == 0,
                        Some("web_search_20250305"),
                        false,
                    ),
                ))
                .await
                .expect("invalid current-turn WebSearch response");
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "round {round}");
            let request_id = response_request_id(&response);
            let error_id = response
                .headers()
                .get("x-error-id")
                .and_then(|value| value.to_str().ok())
                .expect("invalid current-turn error-id")
                .to_string();
            let body = axum::body::to_bytes(response.into_body(), 128 * 1024)
                .await
                .expect("invalid current-turn body");
            let body = String::from_utf8(body.to_vec()).expect("invalid body UTF-8");
            assert!(body.contains(&error_id));
            assert!(!body.contains(&stale_query));

            let record = usage_record_for_request(&usage_recorder, &request_id);
            assert_eq!(record.status, UsageRecordStatus::Error);
            assert_eq!(record.public_error_status_code, Some(400));
            assert_eq!(
                record.public_error_type.as_deref(),
                Some("invalid_request_error")
            );
            assert_eq!(record.error_id.as_deref(), Some(error_id.as_str()));
            assert_eq!(
                record.request_api_key_id.as_deref(),
                Some(expected_key_id.as_str())
            );
            assert_eq!(record.credential_id, None);
            assert!(record.credential_attempts.is_empty());
            let attempts = record
                .latency_trace
                .as_ref()
                .and_then(|trace| trace.inference_attempts)
                .expect("invalid current-turn attempt snapshot");
            assert_eq!(attempts.consumed, 0);
            assert_eq!(attempts.local_attempts, 0);
            assert_eq!(attempts.external_attempts, 0);
            assert_eq!(attempts.mcp_attempts, 0);
            assert!(!attempts.downstream_committed);
            let serialized = serde_json::to_string(&record).expect("serialize invalid usage");
            assert!(!serialized.contains(&stale_query));
        }
    }

    assert_eq!(upstream.state.mcp_hits(), 0);
    assert_eq!(upstream.state.normal_hits(), 0);
    assert!(upstream.state.queries().is_empty());
}

#[test]
fn websearch_latest_non_text_or_blank_user_turn_rejects_without_mcp_for_five_rounds() {
    run_handler_fixture_on_four_mib_thread("websearch-empty-query-matrix", || async {
        run_websearch_latest_non_text_or_blank_user_turn_rejects_without_mcp_for_five_rounds()
            .await;
    });
}

async fn run_websearch_valid_zero_results_keep_stream_and_non_stream_success_for_five_rounds() {
    let upstream = WebSearchHandlerUpstream::start().await;
    let (router, usage_recorder) = websearch_handler_test_router(&upstream.base_url);

    for stream in [true, false] {
        for round in 1..=5 {
            let query = format!("zero-results-{stream}-{round}");
            let response = router
                .clone()
                .oneshot(multimodal_handler_request(
                    "/cc/v1/messages",
                    single_query_websearch_body(&query, stream),
                ))
                .await
                .expect("zero-result WebSearch response");
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "stream={stream} round={round}"
            );
            let content_type = response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .expect("zero-result content type");
            if stream {
                assert_eq!(content_type, "text/event-stream");
            } else {
                assert!(content_type.starts_with("application/json"));
            }
            let request_id = response_request_id(&response);
            let body = axum::body::to_bytes(response.into_body(), 512 * 1024)
                .await
                .expect("zero-result body");
            let body = String::from_utf8(body.to_vec()).expect("zero-result UTF-8");
            assert!(body.contains("No results found."));
            assert!(body.contains(r#""type":"web_search_tool_result""#));
            assert!(body.contains(r#""content":[]"#));
            if stream {
                assert!(body.contains("event: message_stop"));
            } else {
                let value: Value = serde_json::from_str(&body).expect("zero-result message JSON");
                assert_eq!(value["type"], "message");
                assert_eq!(value["stop_reason"], "end_turn");
            }

            let record = usage_record_for_request(&usage_recorder, &request_id);
            assert_eq!(record.status, UsageRecordStatus::Success);
            assert_websearch_usage_attribution(
                &record,
                &format!("zero-result stream={stream} round={round}"),
                &[query.as_str(), "WEBSEARCH_RAW_RESULT_MARKER"],
            );
            assert_eq!(record.downstream_stop_reason.as_deref(), Some("end_turn"));
            assert!(record.output_tokens > 0);
        }
    }

    assert_eq!(upstream.state.mcp_hits(), 10);
    assert_eq!(upstream.state.normal_hits(), 0);
}

#[test]
fn websearch_valid_zero_results_keep_stream_and_non_stream_success_for_five_rounds() {
    run_handler_fixture_on_four_mib_thread("websearch-zero-results-matrix", || async {
        run_websearch_valid_zero_results_keep_stream_and_non_stream_success_for_five_rounds().await;
    });
}

async fn run_websearch_stream_completion_and_client_drop_usage_ownership_hold_for_five_rounds() {
    let upstream = WebSearchHandlerUpstream::start().await;
    let (router, usage_recorder) = websearch_handler_test_router(&upstream.base_url);
    let expected_key_id = crate::common::auth::request_api_key_id("b07-handler-key");

    for round in 1..=5 {
        let full_query = format!("full-stream-{round}");
        let response = router
            .clone()
            .oneshot(multimodal_handler_request(
                "/cc/v1/messages",
                single_query_websearch_body(&full_query, true),
            ))
            .await
            .expect("full stream response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/event-stream")
        );
        let request_id = response_request_id(&response);
        let body = axum::body::to_bytes(response.into_body(), 512 * 1024)
            .await
            .expect("consume complete WebSearch stream");
        let body = String::from_utf8(body.to_vec()).expect("SSE body UTF-8");
        assert!(body.contains("event: message_start"));
        assert!(body.contains("event: message_stop"));

        let record = usage_record_for_request(&usage_recorder, &request_id);
        assert_eq!(record.status, UsageRecordStatus::Success);
        assert_websearch_usage_attribution(
            &record,
            &format!("full stream round {round}"),
            &[full_query.as_str(), "WEBSEARCH_RAW_RESULT_MARKER"],
        );
        assert_eq!(
            record.request_api_key_id.as_deref(),
            Some(expected_key_id.as_str())
        );
        assert_ne!(
            record.request_api_key_id.as_deref(),
            Some("b07-handler-key")
        );
        assert_eq!(record.downstream_stop_reason.as_deref(), Some("end_turn"));
        assert!(record.output_tokens > 0);
        let trace = record.latency_trace.expect("full stream latency trace");
        let attempts = trace.inference_attempts.expect("full stream attempts");
        assert_eq!(attempts.consumed, 1);
        assert!(attempts.downstream_committed);
        assert_eq!(trace.terminal_reason, Some(StreamTerminalReason::Completed));

        let never_polled_query = format!("never-polled-{round}");
        let response = router
            .clone()
            .oneshot(multimodal_handler_request(
                "/cc/v1/messages",
                single_query_websearch_body(&never_polled_query, true),
            ))
            .await
            .expect("never-polled stream response");
        let request_id = response_request_id(&response);
        drop(response);
        let record = usage_record_for_request(&usage_recorder, &request_id);
        assert_eq!(record.status, UsageRecordStatus::ClientDropped);
        assert_websearch_usage_attribution(
            &record,
            &format!("never-polled stream round {round}"),
            &[never_polled_query.as_str(), "WEBSEARCH_RAW_RESULT_MARKER"],
        );
        let trace = record.latency_trace.expect("never-polled latency trace");
        let attempts = trace.inference_attempts.expect("never-polled attempts");
        assert_eq!(attempts.consumed, 1);
        assert!(!attempts.downstream_committed);
        assert_eq!(
            trace.terminal_reason,
            Some(StreamTerminalReason::ClientDropped)
        );

        let partial_drop_query = format!("partial-drop-{round}");
        let response = router
            .clone()
            .oneshot(multimodal_handler_request(
                "/cc/v1/messages",
                single_query_websearch_body(&partial_drop_query, true),
            ))
            .await
            .expect("partial stream response");
        let request_id = response_request_id(&response);
        let mut data_stream = response.into_body().into_data_stream();
        let first = data_stream
            .next()
            .await
            .expect("first WebSearch chunk")
            .expect("first WebSearch chunk succeeds");
        assert!(!first.is_empty());
        drop(data_stream);
        let record = usage_record_for_request(&usage_recorder, &request_id);
        assert_eq!(record.status, UsageRecordStatus::ClientDropped);
        assert_websearch_usage_attribution(
            &record,
            &format!("partial-drop stream round {round}"),
            &[partial_drop_query.as_str(), "WEBSEARCH_RAW_RESULT_MARKER"],
        );
        let trace = record.latency_trace.expect("partial-drop latency trace");
        let attempts = trace.inference_attempts.expect("partial-drop attempts");
        assert_eq!(attempts.consumed, 1);
        assert!(attempts.downstream_committed);
        assert_eq!(
            trace.terminal_reason,
            Some(StreamTerminalReason::ClientDropped)
        );
    }
}

#[test]
fn websearch_stream_completion_and_client_drop_usage_ownership_hold_for_five_rounds() {
    run_handler_fixture_on_four_mib_thread("websearch-stream-drop-matrix", || async {
        run_websearch_stream_completion_and_client_drop_usage_ownership_hold_for_five_rounds()
            .await;
    });
}

async fn run_websearch_client_cancel_during_mcp_body_keeps_usage_ownership_for_five_rounds() {
    let upstream = WebSearchHandlerUpstream::start().await;
    let (router, usage_recorder) = websearch_handler_test_router(&upstream.base_url);
    let expected_key_id = crate::common::auth::request_api_key_id("b07-handler-key");

    // Keep the historical test filter stable while covering both pre-header and body-read drops.
    for cancel_phase in ["header-timeout", "body-timeout"] {
        for round in 1..=5 {
            let records_before = usage_recorder
                .query(UsageRecordQuery::default())
                .records
                .len();
            let hits_before = upstream.state.mcp_hits();
            let task = tokio::spawn(router.clone().oneshot(multimodal_handler_request(
                "/cc/v1/messages",
                single_query_websearch_body(&format!("{cancel_phase}-cancel-{round}"), true),
            )));
            tokio::time::timeout(Duration::from_secs(1), async {
                while upstream.state.mcp_hits() == hits_before {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("cancel fixture must reach the MCP upstream");
            task.abort();
            let _ = task.await;
            tokio::task::yield_now().await;
            assert_eq!(
                upstream.state.mcp_hits(),
                hits_before + 1,
                "{cancel_phase} round {round}: cancellation must not trigger another MCP send"
            );

            let records = usage_recorder.query(UsageRecordQuery::default()).records;
            assert_eq!(
                records.len(),
                records_before + 1,
                "{cancel_phase} round {round}: cancelling during MCP validation lost usage"
            );
            let record = &records[0];
            assert_eq!(
                record.status,
                UsageRecordStatus::ClientDropped,
                "{cancel_phase} round {round}"
            );
            assert_eq!(
                record.request_api_key_id.as_deref(),
                Some(expected_key_id.as_str()),
                "{cancel_phase} round {round}"
            );
            let attempts = record
                .latency_trace
                .as_ref()
                .and_then(|trace| trace.inference_attempts)
                .expect("cancelled WebSearch attempt snapshot");
            assert_eq!(attempts.consumed, 1, "{cancel_phase} round {round}");
            assert_eq!(attempts.local_attempts, 0, "{cancel_phase} round {round}");
            assert_eq!(
                attempts.external_attempts, 0,
                "{cancel_phase} round {round}"
            );
            assert_eq!(attempts.mcp_attempts, 1, "{cancel_phase} round {round}");
            assert!(
                !attempts.downstream_committed,
                "{cancel_phase} round {round}"
            );
            assert!(
                record.credential_id.is_some(),
                "{cancel_phase} round {round}"
            );
            assert_eq!(
                record.credential_attempts.len(),
                1,
                "{cancel_phase} round {round}"
            );
            assert_eq!(
                record.credential_attempts[0].attempt, 1,
                "{cancel_phase} round {round}"
            );
            assert_eq!(
                record.credential_attempts[0].action, "fail",
                "{cancel_phase} round {round}"
            );
            assert_eq!(
                record.credential_attempts[0].error_type.as_deref(),
                Some("client_dropped"),
                "{cancel_phase} round {round}"
            );
            assert_eq!(
                record.credential_attempts[0].error_message.as_deref(),
                Some("mcp_client_cancelled"),
                "{cancel_phase} round {round}"
            );
        }
    }
}

#[test]
fn websearch_client_cancel_during_mcp_body_keeps_usage_ownership_for_five_rounds() {
    run_handler_fixture_on_four_mib_thread("websearch-cancel-matrix", || async {
        run_websearch_client_cancel_during_mcp_body_keeps_usage_ownership_for_five_rounds().await;
    });
}

async fn run_websearch_non_stream_success_has_json_shape_usage_and_stable_key_for_five_rounds() {
    let upstream = WebSearchHandlerUpstream::start().await;
    let (router, usage_recorder) = websearch_handler_test_router(&upstream.base_url);
    let expected_key_id = crate::common::auth::request_api_key_id("b07-handler-key");

    for round in 1..=5 {
        let query = format!("non-stream-{round}");
        let response = router
            .clone()
            .oneshot(multimodal_handler_request(
                "/cc/v1/messages",
                single_query_websearch_body(&query, false),
            ))
            .await
            .expect("non-stream WebSearch response");
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.starts_with("application/json"))
        );
        let request_id = response_request_id(&response);
        let body = axum::body::to_bytes(response.into_body(), 512 * 1024)
            .await
            .expect("non-stream WebSearch body");
        let value: Value = serde_json::from_slice(&body).expect("non-stream message JSON");
        assert_eq!(value["type"], "message");
        assert_eq!(value["stop_reason"], "end_turn");
        assert!(
            value["content"]
                .as_array()
                .is_some_and(|blocks| blocks.iter().any(|block| {
                    block["type"] == "server_tool_use" && block["input"]["query"] == query
                }))
        );

        let record = usage_record_for_request(&usage_recorder, &request_id);
        assert_eq!(record.status, UsageRecordStatus::Success);
        assert_websearch_usage_attribution(
            &record,
            &format!("non-stream success round {round}"),
            &[query.as_str(), "WEBSEARCH_RAW_RESULT_MARKER"],
        );
        assert_eq!(
            record.request_api_key_id.as_deref(),
            Some(expected_key_id.as_str())
        );
        assert_eq!(record.downstream_stop_reason.as_deref(), Some("end_turn"));
        let attempts = record
            .latency_trace
            .and_then(|trace| trace.inference_attempts)
            .expect("non-stream attempts");
        assert_eq!(attempts.consumed, 1);
        assert!(attempts.downstream_committed);
    }
}

#[test]
fn websearch_non_stream_success_has_json_shape_usage_and_stable_key_for_five_rounds() {
    run_handler_fixture_on_four_mib_thread("websearch-non-stream-matrix", || async {
        run_websearch_non_stream_success_has_json_shape_usage_and_stable_key_for_five_rounds()
            .await;
    });
}

async fn run_websearch_debug_logs_and_usage_never_capture_raw_query_or_result_markers_for_five_rounds()
 {
    let captured = CapturedTestLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(captured.clone())
        .finish();
    let _subscriber_guard = tracing::subscriber::set_default(subscriber);
    let upstream = WebSearchHandlerUpstream::start().await;
    let (router, usage_recorder) = websearch_handler_test_router(&upstream.base_url);

    for round in 1..=5 {
        let query_marker = format!("WEBSEARCH_RAW_QUERY_MARKER_{round}");
        let response = router
            .clone()
            .oneshot(multimodal_handler_request(
                "/cc/v1/messages",
                single_query_websearch_body(&query_marker, round % 2 == 0),
            ))
            .await
            .expect("privacy marker WebSearch response");
        assert_eq!(response.status(), StatusCode::OK);
        let request_id = response_request_id(&response);
        axum::body::to_bytes(response.into_body(), 512 * 1024)
            .await
            .expect("consume privacy marker response");

        let record = usage_record_for_request(&usage_recorder, &request_id);
        assert_websearch_usage_attribution(
            &record,
            &format!("privacy round {round}"),
            &[query_marker.as_str(), "WEBSEARCH_RAW_RESULT_MARKER"],
        );
        let serialized = serde_json::to_string(&record).expect("serialize privacy usage record");
        assert!(!serialized.contains(&query_marker));
        assert!(!serialized.contains("WEBSEARCH_RAW_RESULT_MARKER"));
    }

    let logs = captured.snapshot();
    for round in 1..=5 {
        assert!(
            !logs.contains(&format!("WEBSEARCH_RAW_QUERY_MARKER_{round}")),
            "round {round}: DEBUG logs captured raw query"
        );
    }
    assert!(
        !logs.contains("WEBSEARCH_RAW_RESULT_MARKER"),
        "DEBUG logs captured raw MCP result"
    );
    assert!(logs.contains("query_bytes"));
    assert!(logs.contains("native WebSearch"));
}

#[test]
fn websearch_debug_logs_and_usage_never_capture_raw_query_or_result_markers_for_five_rounds() {
    run_handler_fixture_on_four_mib_thread("websearch-privacy-matrix", || async {
        run_websearch_debug_logs_and_usage_never_capture_raw_query_or_result_markers_for_five_rounds()
            .await;
    });
}

async fn run_websearch_mcp_error_resource_and_recovery_matrix_is_fail_closed_for_five_rounds() {
    let captured = CapturedTestLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(captured.clone())
        .finish();
    let _subscriber_guard = tracing::subscriber::set_default(subscriber);
    let upstream = WebSearchHandlerUpstream::start().await;
    let (router, usage_recorder) = websearch_handler_test_router(&upstream.base_url);
    let cases = [
        ("http-400", StatusCode::BAD_REQUEST, "invalid_request_error"),
        (
            "http-429",
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_error",
        ),
        ("http-500", StatusCode::BAD_GATEWAY, "api_error"),
        ("header-timeout", StatusCode::GATEWAY_TIMEOUT, "api_error"),
        ("body-timeout", StatusCode::GATEWAY_TIMEOUT, "api_error"),
        ("disconnect", StatusCode::BAD_GATEWAY, "api_error"),
        ("malformed", StatusCode::BAD_GATEWAY, "api_error"),
        ("jsonrpc-error", StatusCode::BAD_GATEWAY, "api_error"),
        ("is-error", StatusCode::BAD_GATEWAY, "api_error"),
        ("non-text-content", StatusCode::BAD_GATEWAY, "api_error"),
        ("mismatched-id", StatusCode::BAD_GATEWAY, "api_error"),
        (
            "content-length-over-limit",
            StatusCode::BAD_GATEWAY,
            "api_error",
        ),
        ("chunked-over-limit", StatusCode::BAD_GATEWAY, "api_error"),
    ];
    let private_markers = [
        "private-400-marker",
        "private-429-marker",
        "private-500-marker",
        "private-jsonrpc-marker",
        "private-is-error-marker",
        "private-non-text-marker",
    ];

    for (scenario, expected_status, expected_error_type) in cases {
        for round in 1..=5 {
            let query = format!("{scenario}-{round}");
            let response = router
                .clone()
                .oneshot(multimodal_handler_request(
                    "/cc/v1/messages",
                    single_query_websearch_body(&query, round % 2 == 0),
                ))
                .await
                .expect("WebSearch error response");
            assert_eq!(
                response.status(),
                expected_status,
                "{scenario} round {round}"
            );
            let request_id = response_request_id(&response);
            let error_id = response
                .headers()
                .get("x-error-id")
                .and_then(|value| value.to_str().ok())
                .expect("normalized WebSearch error-id")
                .to_string();
            let body = axum::body::to_bytes(response.into_body(), 128 * 1024)
                .await
                .expect("normalized WebSearch error body");
            let body = String::from_utf8(body.to_vec()).expect("error body UTF-8");
            let value: Value = serde_json::from_str(&body).expect("error response JSON");
            assert_eq!(value["type"], "error");
            assert_eq!(value["error"]["type"], expected_error_type);
            assert!(
                value["error"]["message"]
                    .as_str()
                    .is_some_and(|message| message.contains(&error_id)),
                "{scenario} round {round}"
            );
            assert_eq!(value["request_id"], request_id);
            assert!(!body.contains(&query));
            for marker in private_markers {
                assert!(!body.contains(marker), "{scenario} round {round}: {marker}");
            }

            let record = usage_record_for_request(&usage_recorder, &request_id);
            assert_eq!(record.status, UsageRecordStatus::Error);
            assert_websearch_usage_attribution(
                &record,
                &format!("{scenario} round {round}"),
                &[query.as_str(), "private-", "WEBSEARCH_RAW_RESULT_MARKER"],
            );
            assert_eq!(record.error_id.as_deref(), Some(error_id.as_str()));
            assert_eq!(
                record.public_error_status_code,
                Some(expected_status.as_u16())
            );
            assert_eq!(
                record.public_error_type.as_deref(),
                Some(expected_error_type)
            );
            assert_eq!(record.output_tokens, 0);
            let serialized = serde_json::to_string(&record).expect("serialize error usage");
            assert!(!serialized.contains(&query));
            for marker in private_markers {
                assert!(!serialized.contains(marker));
            }
            let attempts = record
                .latency_trace
                .and_then(|trace| trace.inference_attempts)
                .expect("error attempt snapshot");
            assert_eq!(attempts.consumed, 1, "{scenario} round {round}");
            assert!(!attempts.downstream_committed);
        }
    }

    for round in 1..=5 {
        let recovery_query = format!("recovery-normal-{round}");
        let response = router
            .clone()
            .oneshot(multimodal_handler_request(
                "/cc/v1/messages",
                single_query_websearch_body(&recovery_query, false),
            ))
            .await
            .expect("recovery response");
        assert_eq!(response.status(), StatusCode::OK, "recovery round {round}");
        let request_id = response_request_id(&response);
        axum::body::to_bytes(response.into_body(), 512 * 1024)
            .await
            .expect("recovery body");
        let record = usage_record_for_request(&usage_recorder, &request_id);
        assert_eq!(record.status, UsageRecordStatus::Success);
        assert_websearch_usage_attribution(
            &record,
            &format!("recovery round {round}"),
            &[recovery_query.as_str(), "WEBSEARCH_RAW_RESULT_MARKER"],
        );
    }

    let logs = captured.snapshot();
    for (scenario, _, _) in cases {
        for round in 1..=5 {
            assert!(
                !logs.contains(&format!("{scenario}-{round}")),
                "{scenario} round {round}: DEBUG logs captured raw query"
            );
        }
    }
    for marker in private_markers {
        assert!(
            !logs.contains(marker),
            "DEBUG logs captured private MCP marker {marker}"
        );
    }
    assert!(logs.contains("native WebSearch MCP request failed"));
}

#[test]
fn websearch_mcp_error_resource_and_recovery_matrix_is_fail_closed_for_five_rounds() {
    run_handler_fixture_on_four_mib_thread("websearch-error-matrix", || async {
        run_websearch_mcp_error_resource_and_recovery_matrix_is_fail_closed_for_five_rounds().await;
    });
}

fn remote_multimodal_limit_body(include_max_tokens: bool) -> String {
    let content = (0..21)
        .map(|index| {
            let block_type = if index % 2 == 0 { "image" } else { "document" };
            json!({
                "type": block_type,
                "source": {
                    "type": "url",
                    "url": format!("https://b07-source-{index}.invalid/resource")
                }
            })
        })
        .collect::<Vec<_>>();
    let mut body = json!({
        "model": "claude-sonnet-4-20250514",
        "messages": [{"role": "user", "content": content}]
    });
    if include_max_tokens {
        body["max_tokens"] = json!(16);
        body["stream"] = json!(false);
    }
    body.to_string()
}

fn inline_multimodal_body(include_max_tokens: bool) -> String {
    const ONE_PIXEL_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
    let mut content = Vec::new();
    for index in 0..21 {
        content.push(json!({
            "type": "image",
            "source": {
                "type": "url",
                "url": format!("data:image/png;base64,{ONE_PIXEL_PNG}")
            }
        }));
        content.push(json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": "image/png",
                "data": ONE_PIXEL_PNG
            }
        }));
        content.push(json!({"type": "text", "text": format!("inline image pair {index}")}));
    }
    let mut body = json!({
        "model": "claude-sonnet-4-20250514",
        "messages": [{"role": "user", "content": content}]
    });
    if include_max_tokens {
        body["max_tokens"] = json!(16);
        body["stream"] = json!(false);
    }
    body.to_string()
}

const ROUTE_POLICY_MATRIX_PROMPT_MARKER: &str = "ROUTE_POLICY_MATRIX_PROMPT_20260803";

fn route_policy_matrix_messages_body(label: &str) -> String {
    json!({
        "model": "claude-sonnet-4-20250514",
        "max_tokens": 32,
        "stream": false,
        "system": "stable route policy cache system prompt ".repeat(700),
        "metadata": {
            "user_id": r#"{"session_id":"11111111-2222-4333-8444-555555555555"}"#
        },
        "messages": [{"role": "user", "content": format!("route policy matrix {label}")}]
    })
    .to_string()
}

fn route_policy_matrix_count_tokens_body() -> String {
    json!({
        "model": "claude-sonnet-4-20250514",
        "system": "stable route policy count tokens system prompt ".repeat(20),
        "messages": [{"role": "user", "content": "same count_tokens payload"}]
    })
    .to_string()
}

fn multimodal_handler_request(path: &str, body: String) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-api-key", "b07-handler-key")
        .body(Body::from(body))
        .expect("build multimodal handler request")
}

fn route_policy_matrix_config(base_url: &str) -> Config {
    let mut config = Config::default();
    config.kiro_upstream_base_url = Some(base_url.to_string());
    config.kiro_upstream_response_timeout_secs = 2;
    config.credential_retry_max_attempts = 1;

    config.cache_policy.path_overrides.insert(
        "/cc".to_string(),
        CacheRoutePolicyPatch {
            cache_type: Some(PromptCacheStrategyType::NoCache),
            ..CacheRoutePolicyPatch::default()
        },
    );
    config.cache_policy.path_overrides.insert(
        "/na".to_string(),
        CacheRoutePolicyPatch {
            cache_type: Some(PromptCacheStrategyType::CurrentHighCache),
            route_namespace: Some(false),
            ..CacheRoutePolicyPatch::default()
        },
    );
    config.cache_policy.path_overrides.insert(
        "/ha".to_string(),
        CacheRoutePolicyPatch {
            cache_type: Some(PromptCacheStrategyType::CurrentHighCache),
            route_namespace: Some(true),
            ..CacheRoutePolicyPatch::default()
        },
    );

    config.prompt_steering.scope = PromptSteeringScope::RouteRules;
    config.prompt_steering.route_mode = PromptSteeringRouteMode::AllowList;
    config.prompt_steering.route_rules = vec!["/ha".to_string()];
    config.prompt_steering.language_constraint.enabled = false;
    config.prompt_steering.task_quality.enabled = false;
    config.prompt_steering.tool_choice.enabled = false;
    config.prompt_steering.chunked_write.enabled = false;
    config.prompt_steering.thinking.enabled = false;
    config.prompt_steering.custom.enabled = true;
    config.prompt_steering.custom.prompt = format!(
        "{ROUTE_POLICY_MATRIX_PROMPT_MARKER} {}",
        "extra route policy prompt tokens ".repeat(50)
    );

    config
}

fn usage_field(value: &Value, field: &str) -> i64 {
    value["usage"][field]
        .as_i64()
        .unwrap_or_else(|| panic!("missing usage.{field}: {value}"))
}

async fn route_policy_matrix_message_request(
    app: &Router,
    usage_recorder: &UsageRecorder,
    upstream: &MultimodalHandlerUpstream,
    path: &str,
    label: &str,
) -> (Value, UsageRecord, String) {
    let body_index = upstream.hits();
    let response = app
        .clone()
        .oneshot(multimodal_handler_request(
            path,
            route_policy_matrix_messages_body(label),
        ))
        .await
        .expect("route policy matrix message response");
    assert_eq!(response.status(), StatusCode::OK, "{path} message status");
    let request_id = response_request_id(&response);
    let body = axum::body::to_bytes(response.into_body(), 256 * 1024)
        .await
        .expect("read route policy matrix message response");
    let value: Value = serde_json::from_slice(&body).expect("route policy matrix message JSON");
    assert_eq!(value["content"][0]["text"], "inline-ok", "{path}");
    let record = usage_record_for_request(usage_recorder, &request_id);
    assert_eq!(record.endpoint, path, "{path} usage endpoint");
    assert_eq!(
        record.route_kind,
        Some(UsageRouteKind::LocalCredential),
        "{path} route kind"
    );
    let bodies = upstream.bodies_snapshot();
    let upstream_body = bodies
        .get(body_index)
        .unwrap_or_else(|| panic!("{path}: upstream body at index {body_index} missing"))
        .clone();
    (value, record, upstream_body)
}

async fn route_policy_matrix_count_tokens(app: &Router, path: &str) -> i64 {
    let response = app
        .clone()
        .oneshot(multimodal_handler_request(
            path,
            route_policy_matrix_count_tokens_body(),
        ))
        .await
        .expect("route policy matrix count_tokens response");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{path} count_tokens status"
    );
    let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("read route policy matrix count_tokens response");
    let value: Value = serde_json::from_slice(&body).expect("route policy count_tokens JSON");
    value["input_tokens"]
        .as_i64()
        .unwrap_or_else(|| panic!("{path}: missing input_tokens: {value}"))
}

async fn assert_remote_multimodal_limit_response(response: Response, path: &str, round: usize) {
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "round {round} path {path}"
    );
    let request_id = response
        .headers()
        .get("request-id")
        .and_then(|value| value.to_str().ok())
        .expect("remote source limit response carries request-id")
        .to_string();
    assert_eq!(
        response
            .headers()
            .get("anthropic-request-id")
            .and_then(|value| value.to_str().ok()),
        Some(request_id.as_str()),
        "round {round} path {path}"
    );
    let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("read remote source limit response");
    let value: Value = serde_json::from_slice(&body).expect("Anthropic error JSON");
    assert_eq!(value["type"], "error", "round {round} path {path}");
    assert_eq!(
        value["error"]["type"], "invalid_request_error",
        "round {round} path {path}"
    );
    assert_eq!(value["request_id"], request_id, "round {round} path {path}");
    assert_eq!(
        value["error"]["message"],
        "remote image/document source count 21 exceeds the request limit of 20",
        "round {round} path {path}"
    );
    assert!(
        !String::from_utf8_lossy(&body).contains("b07-source-"),
        "round {round} path {path}: public error must not echo source URLs"
    );
}

#[test]
fn all_multimodal_handlers_reject_21_remote_sources_before_upstream_for_five_rounds() {
    run_handler_fixture_on_four_mib_thread("multimodal-handler-matrix", || async {
        const MESSAGE_PATHS: [&str; 5] = [
            "/v1/messages",
            "/cc/v1/messages",
            "/na/v1/messages",
            "/ha/v1/messages",
            "/dfcache/demo/v1/messages",
        ];
        const COUNT_TOKEN_PATHS: [&str; 5] = [
            "/v1/messages/count_tokens",
            "/cc/v1/messages/count_tokens",
            "/na/v1/messages/count_tokens",
            "/ha/v1/messages/count_tokens",
            "/dfcache/demo/v1/messages/count_tokens",
        ];
        let upstream = MultimodalHandlerUpstream::start().await;
        let app = multimodal_handler_test_router(&upstream.base_url);

        for round in 1..=5 {
            for path in MESSAGE_PATHS {
                let response = tokio::time::timeout(
                    Duration::from_secs(1),
                    app.clone().oneshot(multimodal_handler_request(
                        path,
                        remote_multimodal_limit_body(true),
                    )),
                )
                .await
                .unwrap_or_else(|_| panic!("round {round} path {path}: handler waited on I/O"))
                .expect("message handler response");
                assert_remote_multimodal_limit_response(response, path, round).await;
            }
            for path in COUNT_TOKEN_PATHS {
                let response = tokio::time::timeout(
                    Duration::from_secs(1),
                    app.clone().oneshot(multimodal_handler_request(
                        path,
                        remote_multimodal_limit_body(false),
                    )),
                )
                .await
                .unwrap_or_else(|_| panic!("round {round} path {path}: handler waited on I/O"))
                .expect("count_tokens handler response");
                assert_remote_multimodal_limit_response(response, path, round).await;
            }
        }

        assert_eq!(
            upstream.hits(),
            0,
            "remote source preflight must run before inference HTTP"
        );

        for round in 1..=5 {
            for path in COUNT_TOKEN_PATHS {
                let response = tokio::time::timeout(
                    Duration::from_secs(1),
                    app.clone().oneshot(multimodal_handler_request(
                        path,
                        inline_multimodal_body(false),
                    )),
                )
                .await
                .unwrap_or_else(|_| panic!("inline round {round} path {path}: handler timed out"))
                .expect("inline count_tokens response");
                assert_eq!(
                    response.status(),
                    StatusCode::OK,
                    "inline round {round} path {path}"
                );
                let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
                    .await
                    .expect("read inline count_tokens response");
                let value: Value = serde_json::from_slice(&body).expect("count_tokens JSON");
                assert!(
                    value["input_tokens"]
                        .as_i64()
                        .is_some_and(|tokens| tokens > 0),
                    "inline round {round} path {path}: {value}"
                );
            }
        }
        assert_eq!(
            upstream.hits(),
            0,
            "count_tokens inline controls must remain local"
        );

        for round in 1..=5 {
            for path in MESSAGE_PATHS {
                let response = tokio::time::timeout(
                    Duration::from_secs(2),
                    app.clone().oneshot(multimodal_handler_request(
                        path,
                        inline_multimodal_body(true),
                    )),
                )
                .await
                .unwrap_or_else(|_| {
                    panic!("inline message round {round} path {path}: handler timed out")
                })
                .expect("inline message response");
                assert_eq!(
                    response.status(),
                    StatusCode::OK,
                    "inline message round {round} path {path}"
                );
                let body = axum::body::to_bytes(response.into_body(), 256 * 1024)
                    .await
                    .expect("read inline message response");
                let value: Value = serde_json::from_slice(&body).expect("inline message JSON");
                assert_eq!(value["content"][0]["type"], "text");
                assert_eq!(value["content"][0]["text"], "inline-ok");
            }
        }
        assert_eq!(
            upstream.hits(),
            25,
            "five rounds across five inline message routes should each reach inference exactly once"
        );
    });
}

#[test]
fn builtin_routes_follow_runtime_cache_and_prompt_config_matrix() {
    run_handler_fixture_on_four_mib_thread("route-policy-config-matrix", || async {
        let upstream = MultimodalHandlerUpstream::start().await;
        let config = route_policy_matrix_config(&upstream.base_url);
        let (app, usage_recorder) = multimodal_handler_test_router_from_config(config.clone());

        let cc_policy = config.cache_policy_for_path("/cc/v1/messages");
        assert_eq!(
            cc_policy.policy.cache_type,
            PromptCacheStrategyType::NoCache
        );
        assert_eq!(cc_policy.namespace, None);
        let na_policy = config.cache_policy_for_path("/na/v1/messages");
        assert_eq!(
            na_policy.policy.cache_type,
            PromptCacheStrategyType::CurrentHighCache
        );
        assert_eq!(na_policy.namespace, None);
        let ha_policy = config.cache_policy_for_path("/ha/v1/messages");
        assert_eq!(
            ha_policy.policy.cache_type,
            PromptCacheStrategyType::CurrentHighCache
        );
        assert_eq!(ha_policy.namespace.as_deref(), Some("/ha"));

        let (v1_body, v1_record, v1_upstream_body) = route_policy_matrix_message_request(
            &app,
            &usage_recorder,
            &upstream,
            "/v1/messages",
            "shared",
        )
        .await;
        assert!(
            !v1_upstream_body.contains(ROUTE_POLICY_MATRIX_PROMPT_MARKER),
            "/v1 should not receive /ha prompt steering"
        );
        assert!(usage_field(&v1_body, "cache_creation_input_tokens") > 0);
        assert_eq!(usage_field(&v1_body, "cache_read_input_tokens"), 0);
        assert!(v1_record.cache_creation_input_tokens > 0);
        assert_eq!(v1_record.cache_read_input_tokens, 0);

        let (cc_body, cc_record, cc_upstream_body) = route_policy_matrix_message_request(
            &app,
            &usage_recorder,
            &upstream,
            "/cc/v1/messages",
            "shared",
        )
        .await;
        assert!(
            !cc_upstream_body.contains(ROUTE_POLICY_MATRIX_PROMPT_MARKER),
            "/cc should not receive /ha prompt steering"
        );
        assert_eq!(usage_field(&cc_body, "cache_creation_input_tokens"), 0);
        assert_eq!(usage_field(&cc_body, "cache_read_input_tokens"), 0);
        assert_eq!(cc_record.cache_creation_input_tokens, 0);
        assert_eq!(cc_record.cache_read_input_tokens, 0);

        let (na_body, na_record, na_upstream_body) = route_policy_matrix_message_request(
            &app,
            &usage_recorder,
            &upstream,
            "/na/v1/messages",
            "shared",
        )
        .await;
        assert!(
            !na_upstream_body.contains(ROUTE_POLICY_MATRIX_PROMPT_MARKER),
            "/na should not receive /ha prompt steering"
        );
        assert!(
            usage_field(&na_body, "cache_read_input_tokens") > 0,
            "/na is configured high-cache with shared namespace, so it should read the /v1 cache entry"
        );
        assert!(na_record.cache_read_input_tokens > 0);

        let (ha_first_body, ha_first_record, ha_first_upstream_body) =
            route_policy_matrix_message_request(
                &app,
                &usage_recorder,
                &upstream,
                "/ha/v1/messages",
                "shared",
            )
            .await;
        assert!(
            ha_first_upstream_body.contains(ROUTE_POLICY_MATRIX_PROMPT_MARKER),
            "/ha should receive configured prompt steering"
        );
        assert_eq!(
            usage_field(&ha_first_body, "cache_read_input_tokens"),
            0,
            "/ha is configured with independent namespace, so it must not read /v1 or /na cache entries"
        );
        assert!(usage_field(&ha_first_body, "cache_creation_input_tokens") > 0);
        assert_eq!(ha_first_record.cache_read_input_tokens, 0);
        assert!(ha_first_record.cache_creation_input_tokens > 0);

        let (ha_second_body, ha_second_record, ha_second_upstream_body) =
            route_policy_matrix_message_request(
                &app,
                &usage_recorder,
                &upstream,
                "/ha/v1/messages",
                "shared",
            )
            .await;
        assert!(ha_second_upstream_body.contains(ROUTE_POLICY_MATRIX_PROMPT_MARKER));
        assert!(
            usage_field(&ha_second_body, "cache_read_input_tokens") > 0,
            "second /ha request should read from its own independent namespace"
        );
        assert!(ha_second_record.cache_read_input_tokens > 0);

        let hits_before_count_tokens = upstream.hits();
        let v1_count = route_policy_matrix_count_tokens(&app, "/v1/messages/count_tokens").await;
        let cc_count = route_policy_matrix_count_tokens(&app, "/cc/v1/messages/count_tokens").await;
        let na_count = route_policy_matrix_count_tokens(&app, "/na/v1/messages/count_tokens").await;
        let ha_count = route_policy_matrix_count_tokens(&app, "/ha/v1/messages/count_tokens").await;
        assert_eq!(
            upstream.hits(),
            hits_before_count_tokens,
            "count_tokens must stay local for all built-in routes"
        );
        assert_eq!(
            cc_count, v1_count,
            "/cc should not receive /ha prompt steering"
        );
        assert_eq!(
            na_count, v1_count,
            "/na should not receive /ha prompt steering"
        );
        assert!(
            ha_count > v1_count,
            "/ha count_tokens should include configured prompt steering: ha={ha_count}, v1={v1_count}"
        );
    });
}

fn messages_request_for_model(model: &str) -> MessagesRequest {
    MessagesRequest {
        model: model.to_string(),
        max_tokens: 16,
        messages: vec![Message {
            role: "user".to_string(),
            content: json!("hello"),
        }],
        stream: false,
        system: None,
        tools: None,
        tool_choice: None,
        thinking: None,
        output_config: None,
        metadata: None,
    }
}

#[test]
fn local_stream_protocol_contamination_retry_uses_status_policy_and_explicit_terminal() {
    let enabled = LocalStreamRetryConfig {
        enabled: true,
        max_attempts: 2,
        on_idle_timeout: false,
        on_read_error: false,
        on_status_error: true,
    };
    assert!(enabled.allows(StreamRetryReason::ProtocolContamination));
    assert_eq!(
        StreamRetryReason::ProtocolContamination.as_str(),
        "protocol_contamination"
    );

    let disabled = LocalStreamRetryConfig {
        on_status_error: false,
        ..enabled
    };
    assert!(!disabled.allows(StreamRetryReason::ProtocolContamination));
    assert_eq!(
        serde_json::to_value(StreamTerminalReason::ProtocolContamination).unwrap(),
        json!("protocol_contamination")
    );
}

#[derive(Debug, Clone, Copy)]
enum HandlerEventStreamFault {
    JsonExceptionBeforeOutput,
    BinaryEventStreamWithJsonContentType,
    JsonBodyWithEventStreamContentType,
    SignatureInvalidThenJsonLabeledEventStreamSuccess,
    SignatureInvalidThenJsonErrorEnvelope,
    ReadErrorBeforeOutput,
    IdleBeforeOutput,
    BadCrcBeforeOutput,
    TruncatedFrameBeforeOutput,
    IncompleteStatusBeforeOutput,
    ProtocolContaminationBeforeOutput,
    UnknownEventOnly,
    MissingCompletionAfterText,
    LegacyTextWithMetadataNoStatus,
    TextWithMeteringNoStatus,
    UsageOnlyMeteringNoStatus,
    CompleteToolWithoutStatus,
    IncompleteToolWithoutStatus,
    TextThenReadError,
    ThinkingThenReadError,
    ToolThenReadError,
    NonStreamContentLengthOverLimit,
    NonStreamChunkedOverLimit,
    NonStreamExactLimit,
    NonStreamSmallBody,
}

#[derive(Clone)]
struct HandlerEventStreamFaultState {
    fault: HandlerEventStreamFault,
    hits: Arc<AtomicUsize>,
    json_secret_marker: String,
}

struct HandlerEventStreamFaultUpstream {
    base_url: String,
    state: HandlerEventStreamFaultState,
    task: tokio::task::JoinHandle<()>,
}

impl HandlerEventStreamFaultUpstream {
    async fn start(fault: HandlerEventStreamFault) -> Self {
        Self::start_with_json_secret_marker(fault, "private fault fixture detail".to_string()).await
    }

    async fn start_with_json_secret_marker(
        fault: HandlerEventStreamFault,
        json_secret_marker: String,
    ) -> Self {
        let state = HandlerEventStreamFaultState {
            fault,
            hits: Arc::new(AtomicUsize::new(0)),
            json_secret_marker,
        };
        let app = Router::new()
            .route(
                "/generateAssistantResponse",
                post(handler_eventstream_fault_upstream),
            )
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind EventStream fault upstream");
        let address = listener
            .local_addr()
            .expect("EventStream fault upstream address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve EventStream fault upstream");
        });
        Self {
            base_url: format!("http://{address}"),
            state,
            task,
        }
    }

    fn hits(&self) -> usize {
        self.state.hits.load(Ordering::Acquire)
    }
}

impl Drop for HandlerEventStreamFaultUpstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn handler_eventstream_normal_body() -> Vec<u8> {
    let mut body = eventstream_test_frame(
        "assistantResponseEvent",
        json!({"content":"recovered-ok","messageStatus":"COMPLETED"}),
    );
    body.extend(eventstream_test_frame(
        "metadataEvent",
        json!({
            "tokenUsage": {
                "uncachedInputTokens": 100,
                "outputTokens": 4,
                "totalTokens": 104,
                "cacheReadInputTokens": 0,
                "cacheWriteInputTokens": 0
            }
        }),
    ));
    body
}

fn handler_eventstream_exact_non_stream_limit_body() -> Vec<u8> {
    let terminal = eventstream_test_frame(
        "assistantResponseEvent",
        json!({"content":"exact-limit-ok","messageStatus":"COMPLETED"}),
    );
    let empty_padding = eventstream_test_frame("futurePaddingEvent", json!({"padding":""}));
    let padding_bytes = LOCAL_NON_STREAM_RESPONSE_MAX_BYTES
        .checked_sub(terminal.len() + empty_padding.len())
        .expect("16 MiB limit leaves room for fixture framing");
    let padding = eventstream_test_frame(
        "futurePaddingEvent",
        json!({"padding":"x".repeat(padding_bytes)}),
    );
    let mut body = Vec::with_capacity(LOCAL_NON_STREAM_RESPONSE_MAX_BYTES);
    body.extend(padding);
    body.extend(terminal);
    assert_eq!(body.len(), LOCAL_NON_STREAM_RESPONSE_MAX_BYTES);
    body
}

fn handler_eventstream_bytes_response(bytes: Vec<u8>) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/vnd.amazon.eventstream")
        .body(Body::from(bytes))
        .expect("build EventStream bytes response")
}

fn handler_eventstream_json_labeled_bytes_response(bytes: Vec<u8>) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(bytes))
        .expect("build JSON-labeled EventStream bytes response")
}

fn handler_eventstream_chunked_response(
    chunks: Vec<(Duration, Result<Bytes, std::io::Error>)>,
) -> Response {
    let body_stream =
        futures::StreamExt::then(futures::stream::iter(chunks), |(delay, chunk)| async move {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            chunk
        });
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/vnd.amazon.eventstream")
        .body(Body::from_stream(body_stream))
        .expect("build EventStream chunked response")
}

async fn handler_eventstream_fault_upstream(
    State(state): State<HandlerEventStreamFaultState>,
) -> Response {
    let hit = state.hits.fetch_add(1, Ordering::AcqRel) + 1;
    if hit > 1 {
        return match state.fault {
            HandlerEventStreamFault::SignatureInvalidThenJsonLabeledEventStreamSuccess => {
                handler_eventstream_json_labeled_bytes_response(handler_eventstream_normal_body())
            }
            HandlerEventStreamFault::SignatureInvalidThenJsonErrorEnvelope => (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/json")],
                Json(json!({
                    "__type": "ThrottlingException",
                    "message": state.json_secret_marker
                })),
            )
                .into_response(),
            _ => handler_eventstream_bytes_response(handler_eventstream_normal_body()),
        };
    }

    match state.fault {
        HandlerEventStreamFault::SignatureInvalidThenJsonLabeledEventStreamSuccess
        | HandlerEventStreamFault::SignatureInvalidThenJsonErrorEnvelope => (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "application/json")],
            Json(json!({
                "reason": "THINKING_SIGNATURE_INVALID"
            })),
        )
            .into_response(),
        HandlerEventStreamFault::JsonExceptionBeforeOutput => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            Json(json!({
                "__type": "ThrottlingException",
                "message": state.json_secret_marker
            })),
        )
            .into_response(),
        HandlerEventStreamFault::BinaryEventStreamWithJsonContentType => {
            handler_eventstream_json_labeled_bytes_response(handler_eventstream_normal_body())
        }
        HandlerEventStreamFault::JsonBodyWithEventStreamContentType => {
            handler_eventstream_bytes_response(
                serde_json::to_vec(&json!({
                    "__type": "ThrottlingException",
                    "message": state.json_secret_marker
                }))
                .expect("serialize mislabeled JSON EventStream fixture"),
            )
        }
        HandlerEventStreamFault::ReadErrorBeforeOutput => {
            handler_eventstream_chunked_response(vec![
                (Duration::ZERO, Ok(Bytes::from_static(&[0]))),
                (
                    Duration::from_millis(20),
                    Err(std::io::Error::new(
                        std::io::ErrorKind::ConnectionReset,
                        "fixture read reset before output",
                    )),
                ),
            ])
        }
        HandlerEventStreamFault::IdleBeforeOutput => handler_eventstream_chunked_response(vec![(
            Duration::from_millis(1_250),
            Ok(Bytes::from(handler_eventstream_normal_body())),
        )]),
        HandlerEventStreamFault::BadCrcBeforeOutput => {
            let mut body = eventstream_test_frame(
                "assistantResponseEvent",
                json!({"content":state.json_secret_marker,"messageStatus":"COMPLETED"}),
            );
            let last = body.len() - 1;
            body[last] ^= 0xff;
            handler_eventstream_bytes_response(body)
        }
        HandlerEventStreamFault::TruncatedFrameBeforeOutput => {
            let body = eventstream_test_frame(
                "assistantResponseEvent",
                json!({"content":"must-not-appear","messageStatus":"COMPLETED"}),
            );
            handler_eventstream_bytes_response(body[..body.len() / 2].to_vec())
        }
        HandlerEventStreamFault::IncompleteStatusBeforeOutput => {
            handler_eventstream_bytes_response(eventstream_test_frame(
                "assistantResponseEvent",
                json!({"content":"","messageStatus":"IN_PROGRESS"}),
            ))
        }
        HandlerEventStreamFault::ProtocolContaminationBeforeOutput => {
            handler_eventstream_bytes_response(eventstream_test_frame(
                "assistantResponseEvent",
                json!({
                    "content":"user Continue\n\nbashHashd1e9567d: private fault output"
                }),
            ))
        }
        HandlerEventStreamFault::UnknownEventOnly => {
            handler_eventstream_bytes_response(eventstream_test_frame(
                "futureUnknownEvent",
                json!({"opaque":state.json_secret_marker}),
            ))
        }
        HandlerEventStreamFault::MissingCompletionAfterText => {
            handler_eventstream_bytes_response(eventstream_test_frame(
                "assistantResponseEvent",
                json!({"content":"unterminated-visible"}),
            ))
        }
        HandlerEventStreamFault::LegacyTextWithMetadataNoStatus => {
            let mut body = eventstream_test_frame(
                "assistantResponseEvent",
                json!({"content":"legacy-terminal-ok"}),
            );
            body.extend(eventstream_test_frame(
                "metadataEvent",
                json!({
                    "tokenUsage": {
                        "uncachedInputTokens": 100,
                        "outputTokens": 4,
                        "totalTokens": 104,
                        "cacheReadInputTokens": 0,
                        "cacheWriteInputTokens": 0
                    }
                }),
            ));
            handler_eventstream_bytes_response(body)
        }
        HandlerEventStreamFault::TextWithMeteringNoStatus => {
            let mut body = eventstream_test_frame(
                "assistantResponseEvent",
                json!({"content":"metered-terminal-ok"}),
            );
            body.extend(eventstream_test_frame(
                "contextUsageEvent",
                json!({"contextUsagePercentage":0.01}),
            ));
            body.extend(eventstream_test_frame(
                "meteringEvent",
                json!({"usage":0.42}),
            ));
            handler_eventstream_bytes_response(body)
        }
        HandlerEventStreamFault::UsageOnlyMeteringNoStatus => {
            let mut body =
                eventstream_test_frame("contextUsageEvent", json!({"contextUsagePercentage":0.01}));
            body.extend(eventstream_test_frame(
                "meteringEvent",
                json!({"usage":0.24,"inputTokens":123,"outputTokens":0}),
            ));
            handler_eventstream_bytes_response(body)
        }
        HandlerEventStreamFault::CompleteToolWithoutStatus => {
            handler_eventstream_bytes_response(eventstream_test_frame(
                "toolUseEvent",
                json!({
                    "name":"Bash",
                    "toolUseId":"toolu_legacy_terminal",
                    "input":"{\"command\":\"printf legacy-tool-ok\"}",
                    "stop":true
                }),
            ))
        }
        HandlerEventStreamFault::IncompleteToolWithoutStatus => {
            handler_eventstream_bytes_response(eventstream_test_frame(
                "toolUseEvent",
                json!({
                    "name":"Bash",
                    "toolUseId":"toolu_legacy_terminal",
                    "input":"{\"command\":\"printf legacy-tool-ok\"}",
                    "stop":false
                }),
            ))
        }
        HandlerEventStreamFault::TextThenReadError => handler_eventstream_chunked_response(vec![
            (
                Duration::ZERO,
                Ok(Bytes::from(eventstream_test_frame(
                    "assistantResponseEvent",
                    json!({"content":"postcommit-visible"}),
                ))),
            ),
            (
                Duration::from_millis(20),
                Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "fixture read reset after text",
                )),
            ),
        ]),
        HandlerEventStreamFault::ThinkingThenReadError => {
            handler_eventstream_chunked_response(vec![
                (
                    Duration::ZERO,
                    Ok(Bytes::from(eventstream_test_frame(
                        "reasoningContentEvent",
                        json!({"text":"postcommit reasoning","signature":"fixture-signature"}),
                    ))),
                ),
                (
                    Duration::from_millis(5),
                    Ok(Bytes::from(eventstream_test_frame(
                        "assistantResponseEvent",
                        json!({"content":"post-thinking-visible"}),
                    ))),
                ),
                (
                    Duration::from_millis(20),
                    Err(std::io::Error::new(
                        std::io::ErrorKind::ConnectionReset,
                        "fixture read reset after thinking",
                    )),
                ),
            ])
        }
        HandlerEventStreamFault::ToolThenReadError => handler_eventstream_chunked_response(vec![
            (
                Duration::ZERO,
                Ok(Bytes::from(eventstream_test_frame(
                    "toolUseEvent",
                    json!({
                        "name":"bashHashd1e9567d",
                        "toolUseId":"toolu_fault_1",
                        "input":"{\"command\":\"printf fault-ok\"}",
                        "stop":true
                    }),
                ))),
            ),
            (
                Duration::from_millis(20),
                Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "fixture read reset after tool",
                )),
            ),
        ]),
        HandlerEventStreamFault::NonStreamContentLengthOverLimit => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/vnd.amazon.eventstream")
            .header(
                header::CONTENT_LENGTH,
                (LOCAL_NON_STREAM_RESPONSE_MAX_BYTES + 1).to_string(),
            )
            .body(Body::empty())
            .expect("build declared over-limit response"),
        HandlerEventStreamFault::NonStreamChunkedOverLimit => {
            handler_eventstream_chunked_response(vec![
                (
                    Duration::ZERO,
                    Ok(Bytes::from(vec![b'x'; LOCAL_NON_STREAM_RESPONSE_MAX_BYTES])),
                ),
                (Duration::ZERO, Ok(Bytes::from_static(b"x"))),
            ])
        }
        HandlerEventStreamFault::NonStreamExactLimit => {
            handler_eventstream_bytes_response(handler_eventstream_exact_non_stream_limit_body())
        }
        HandlerEventStreamFault::NonStreamSmallBody => {
            handler_eventstream_bytes_response(handler_eventstream_normal_body())
        }
    }
}

fn handler_eventstream_fault_router_with_credential_count(
    base_url: &str,
    credential_count: u64,
) -> (Router, Arc<UsageRecorder>) {
    handler_eventstream_fault_router_with_limits(base_url, credential_count, 1)
}

fn handler_eventstream_fault_router_with_limits(
    base_url: &str,
    credential_count: u64,
    credential_retry_max_attempts: u32,
) -> (Router, Arc<UsageRecorder>) {
    assert!(credential_count > 0);
    let mut config = Config::default();
    config.kiro_upstream_base_url = Some(base_url.to_string());
    // The non-stream exact-limit fixture intentionally reads and decodes a 16 MiB
    // EventStream body.  Under the full all-target test tree, CPU contention can
    // make that boundary-control path exceed the previous 3s fixture timeout and
    // turn a limit test into a false upstream body-timeout 502.  The caller-side
    // test timeout remains 5s, so real stalls still fail the fixture promptly.
    config.kiro_upstream_response_timeout_secs = 10;
    config.kiro_upstream_stream_idle_timeout_secs = 1;
    config.kiro_upstream_stream_retry_enabled = true;
    config.kiro_upstream_stream_retry_max_attempts = 2;
    config.kiro_upstream_stream_retry_on_idle_timeout = true;
    config.kiro_upstream_stream_retry_on_read_error = true;
    config.kiro_upstream_stream_retry_on_status_error = true;
    config.credential_retry_max_attempts = credential_retry_max_attempts;
    config.inference_upstream_max_attempts = 4;
    let credentials = (1..=credential_count)
        .map(|id| KiroCredentials {
            id: Some(id),
            access_token: Some(format!("handler-eventstream-fault-token-{id}")),
            profile_arn: Some(format!(
                "arn:aws:codewhisperer:us-east-1:123456789012:profile/HANDLER_FAULT_{id}"
            )),
            expires_at: Some((Utc::now() + chrono::Duration::hours(1)).to_rfc3339()),
            auth_method: Some("social".to_string()),
            rate_limit_auto_disable_enabled: Some(false),
            ..Default::default()
        })
        .collect();
    let manager = Arc::new(
        MultiTokenManager::new(config.clone(), credentials, None, None, false)
            .expect("build EventStream fault token manager"),
    );
    let mut endpoints: HashMap<String, Arc<dyn KiroEndpoint>> = HashMap::new();
    endpoints.insert("ide".to_string(), Arc::new(IdeEndpoint));
    let provider = Arc::new(KiroProvider::with_proxy(
        manager,
        None,
        endpoints,
        "ide".to_string(),
    ));
    let usage_recorder = Arc::new(UsageRecorder::new(1_000));
    let router = create_router_with_provider(
        AnthropicRouterDependencies {
            request_api_keys: Arc::new(RequestApiKeyStore::new(["b07-handler-key"])),
            request_admission: Arc::new(RequestAdmissionController::new(
                RequestAdmissionConfig::disabled(),
            )),
            kiro_provider: Some(provider),
            usage_recorder: usage_recorder.clone(),
            prompt_cache: Arc::new(PromptCacheTracker::default()),
            prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
            pricing_catalog: Arc::new(PricingCatalog::new()),
            model_capabilities: Arc::new(ModelCapabilitiesCatalog::new()),
            external_pool_manager: None,
        },
        AnthropicRouterConfig::from_runtime_config(&config),
    );
    (router, usage_recorder)
}

fn handler_eventstream_fault_router(base_url: &str) -> (Router, Arc<UsageRecorder>) {
    handler_eventstream_fault_router_with_credential_count(base_url, 2)
}

fn handler_eventstream_fault_request(stream: bool) -> Request<Body> {
    multimodal_handler_request(
        "/cc/v1/messages",
        json!({
            "model":"claude-sonnet-4-20250514",
            "max_tokens":2048,
            "stream":stream,
            "messages":[{"role":"user","content":"exercise the EventStream fixture"}],
            "thinking":{"type":"enabled","budget_tokens":1024},
            "tools":[{
                "name":"Bash",
                "description":"run one command",
                "input_schema":{
                    "type":"object",
                    "properties":{"command":{"type":"string"}},
                    "required":["command"]
                }
            }]
        })
        .to_string(),
    )
}

fn handler_thinking_signature_retry_request(stream: bool) -> Request<Body> {
    multimodal_handler_request(
        "/cc/v1/messages",
        json!({
            "model":"claude-opus-4-8",
            "max_tokens":128,
            "stream":stream,
            "thinking":{"type":"adaptive"},
            "messages":[
                {"role":"user","content":"Say hello."},
                {"role":"assistant","content":[
                    {
                        "type":"thinking",
                        "thinking":"prior private reasoning",
                        "signature":"invalid-signature-fixture"
                    },
                    {"type":"text","text":"Hello."}
                ]},
                {"role":"user","content":"Reply exactly: recovered-ok"}
            ]
        })
        .to_string(),
    )
}

async fn call_handler_eventstream_fault(app: Router, stream: bool) -> (StatusCode, String, String) {
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        app.oneshot(handler_eventstream_fault_request(stream)),
    )
    .await
    .expect("fault handler response timed out")
    .expect("fault handler response");
    let status = response.status();
    let request_id = response
        .headers()
        .get("request-id")
        .and_then(|value| value.to_str().ok())
        .expect("fault response request-id")
        .to_string();
    let body = tokio::time::timeout(
        Duration::from_secs(5),
        axum::body::to_bytes(response.into_body(), 1024 * 1024),
    )
    .await
    .expect("fault response body timed out")
    .expect("read fault response body");
    (
        status,
        request_id,
        String::from_utf8(body.to_vec()).expect("fault response UTF-8"),
    )
}

async fn call_handler_thinking_signature_retry(
    app: Router,
    stream: bool,
) -> (StatusCode, String, String) {
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        app.oneshot(handler_thinking_signature_retry_request(stream)),
    )
    .await
    .expect("signature retry handler response timed out")
    .expect("signature retry handler response");
    let status = response.status();
    let request_id = response
        .headers()
        .get("request-id")
        .and_then(|value| value.to_str().ok())
        .expect("signature retry response request-id")
        .to_string();
    let body = tokio::time::timeout(
        Duration::from_secs(5),
        axum::body::to_bytes(response.into_body(), 1024 * 1024),
    )
    .await
    .expect("signature retry response body timed out")
    .expect("read signature retry response body");
    (
        status,
        request_id,
        String::from_utf8(body.to_vec()).expect("signature retry response UTF-8"),
    )
}

fn assert_fault_usage(
    usage_recorder: &UsageRecorder,
    request_id: &str,
    expected_status: UsageRecordStatus,
    expected_attempts: u32,
) {
    let records = usage_recorder.query(UsageRecordQuery {
        request_id: Some(request_id.to_string()),
        ..UsageRecordQuery::default()
    });
    assert_eq!(records.records.len(), 1, "request_id={request_id}");
    let record = &records.records[0];
    assert_eq!(record.status, expected_status, "request_id={request_id}");
    let attempts = record
        .latency_trace
        .as_ref()
        .and_then(|trace| trace.inference_attempts)
        .expect("fault usage inference attempt snapshot");
    assert_eq!(
        attempts.consumed, expected_attempts,
        "request_id={request_id}"
    );
    assert!(attempts.consumed <= 4, "request_id={request_id}");
}

async fn run_handler_thinking_signature_retry_accepts_json_labeled_eventstream_success_for_five_rounds()
 {
    for stream in [false, true] {
        for round in 1..=5 {
            let upstream = HandlerEventStreamFaultUpstream::start(
                HandlerEventStreamFault::SignatureInvalidThenJsonLabeledEventStreamSuccess,
            )
            .await;
            let (app, usage_recorder) =
                handler_eventstream_fault_router_with_limits(&upstream.base_url, 1, 1);
            let (status, request_id, body) =
                call_handler_thinking_signature_retry(app, stream).await;

            assert_eq!(
                status,
                StatusCode::OK,
                "stream={stream} round={round} body={body}"
            );
            assert!(
                body.contains("recovered-ok"),
                "stream={stream} round={round} body={body}"
            );
            assert_eq!(
                upstream.hits(),
                2,
                "stream={stream} round={round}: signature retry must use exactly two sends"
            );
            assert_fault_usage(&usage_recorder, &request_id, UsageRecordStatus::Success, 2);
        }
    }
}

async fn run_handler_thinking_signature_retry_rejects_json_error_envelope_for_five_rounds() {
    for stream in [false, true] {
        for round in 1..=5 {
            let marker = format!("SIGNATURE_RETRY_JSON_ERROR_PRIVATE_{stream}_{round}");
            let upstream = HandlerEventStreamFaultUpstream::start_with_json_secret_marker(
                HandlerEventStreamFault::SignatureInvalidThenJsonErrorEnvelope,
                marker.clone(),
            )
            .await;
            let (app, usage_recorder) =
                handler_eventstream_fault_router_with_limits(&upstream.base_url, 1, 1);
            let (status, request_id, body) =
                call_handler_thinking_signature_retry(app, stream).await;

            if stream {
                assert_eq!(
                    status,
                    StatusCode::OK,
                    "stream={stream} round={round} body={body}"
                );
                assert!(
                    body.contains(r#""type":"error""#),
                    "stream={stream} round={round} body={body}"
                );
                assert!(
                    !body.contains("event: message_stop"),
                    "stream={stream} round={round} body={body}"
                );
                assert_fault_usage(
                    &usage_recorder,
                    &request_id,
                    UsageRecordStatus::StreamError,
                    2,
                );
            } else {
                assert!(
                    status.is_client_error() || status.is_server_error(),
                    "stream={stream} round={round} status={status} body={body}"
                );
                assert!(body.contains(&request_id), "round={round} body={body}");
                assert_fault_usage(&usage_recorder, &request_id, UsageRecordStatus::Error, 2);
            }
            assert_eq!(
                upstream.hits(),
                2,
                "stream={stream} round={round}: signature retry should use exactly two sends"
            );
            assert!(
                !body.contains("recovered-ok"),
                "stream={stream} round={round} body={body}"
            );
            assert!(
                !body.contains(&marker),
                "stream={stream} round={round} body={body}"
            );
            let record = usage_record_for_request(&usage_recorder, &request_id);
            assert_ne!(
                record.status,
                UsageRecordStatus::Success,
                "stream={stream} round={round}"
            );
            let serialized =
                serde_json::to_string(&record).expect("serialize signature retry JSON error usage");
            assert!(
                !serialized.contains(&marker),
                "stream={stream} round={round}: usage leaked private JSON body"
            );
        }
    }
}

#[test]
fn handler_thinking_signature_retry_accepts_json_labeled_eventstream_success_for_five_rounds() {
    run_handler_fixture_on_four_mib_thread(
        "signature-retry-json-eventstream",
        run_handler_thinking_signature_retry_accepts_json_labeled_eventstream_success_for_five_rounds,
    );
}

#[test]
fn handler_thinking_signature_retry_rejects_json_error_envelope_for_five_rounds() {
    run_handler_fixture_on_four_mib_thread(
        "signature-retry-json-error-envelope",
        run_handler_thinking_signature_retry_rejects_json_error_envelope_for_five_rounds,
    );
}

fn expected_precommit_retry_reason(fault: HandlerEventStreamFault) -> &'static str {
    match fault {
        HandlerEventStreamFault::ReadErrorBeforeOutput => "read_error:sends=1",
        HandlerEventStreamFault::IdleBeforeOutput => "idle_timeout:sends=1",
        HandlerEventStreamFault::BadCrcBeforeOutput
        | HandlerEventStreamFault::TruncatedFrameBeforeOutput
        | HandlerEventStreamFault::IncompleteStatusBeforeOutput => "protocol_error:sends=1",
        HandlerEventStreamFault::ProtocolContaminationBeforeOutput => {
            "protocol_contamination:sends=1"
        }
        HandlerEventStreamFault::JsonBodyWithEventStreamContentType => "protocol_error:sends=1",
        other => panic!("not a precommit retry fixture: {other:?}"),
    }
}

async fn assert_handler_eventstream_precommit_retry(fault: HandlerEventStreamFault, round: usize) {
    let upstream = HandlerEventStreamFaultUpstream::start(fault).await;
    let (app, usage_recorder) = handler_eventstream_fault_router(&upstream.base_url);
    let (status, request_id, body) = call_handler_eventstream_fault(app, true).await;
    let usage_debug = serde_json::to_string(
        &usage_recorder
            .query(UsageRecordQuery {
                request_id: Some(request_id.clone()),
                ..UsageRecordQuery::default()
            })
            .records,
    )
    .expect("serialize precommit retry usage evidence");

    assert_eq!(
        status,
        StatusCode::OK,
        "fault={fault:?} round={round} hits={} body={body} usage={usage_debug}",
        upstream.hits()
    );
    assert!(
        body.contains("recovered-ok"),
        "fault={fault:?} round={round} hits={} body={body}",
        upstream.hits()
    );
    assert_eq!(
        body.matches("event: message_stop").count(),
        1,
        "fault={fault:?} round={round}"
    );
    assert!(!body.contains(r#""type":"error""#));
    assert!(!body.contains("private fault fixture detail"));
    assert!(!body.contains("private fault output"));
    assert!(!body.contains("must-not-appear"));
    assert_eq!(upstream.hits(), 2, "fault={fault:?} round={round}");
    assert_fault_usage(&usage_recorder, &request_id, UsageRecordStatus::Success, 2);

    let record = usage_record_for_request(&usage_recorder, &request_id);
    let trace = record
        .latency_trace
        .as_ref()
        .expect("precommit retry latency trace");
    assert_eq!(trace.stream_retry_attempts, Some(1));
    assert_eq!(trace.stream_retry_dispatch_failures, None);
    assert_eq!(
        trace.stream_retry_reasons.as_deref(),
        Some(&[expected_precommit_retry_reason(fault).to_string()][..])
    );
}

fn boxed_precommit_retry_matrix() -> Pin<Box<dyn Future<Output = ()> + Send>> {
    Box::pin(async {
        for fault in [
            HandlerEventStreamFault::ReadErrorBeforeOutput,
            HandlerEventStreamFault::IdleBeforeOutput,
            HandlerEventStreamFault::BadCrcBeforeOutput,
            HandlerEventStreamFault::TruncatedFrameBeforeOutput,
            HandlerEventStreamFault::IncompleteStatusBeforeOutput,
            HandlerEventStreamFault::ProtocolContaminationBeforeOutput,
        ] {
            for round in 1..=5 {
                assert_handler_eventstream_precommit_retry(fault, round).await;
            }
        }
    })
}

fn run_handler_fixture_on_four_mib_thread<F, Fut>(thread_name: &'static str, fixture: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + 'static,
{
    std::thread::Builder::new()
        .name(thread_name.to_string())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build handler fixture runtime");
            runtime.block_on(fixture());
        })
        .expect("spawn handler fixture thread")
        .join()
        .expect("run handler fixture thread");
}

#[test]
fn handler_eventstream_precommit_faults_retry_once_and_recover_for_five_rounds() {
    std::thread::Builder::new()
        .name("precommit-matrix-constructor".to_string())
        .stack_size(4 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build debug-safe precommit matrix runtime");
            runtime.block_on(boxed_precommit_retry_matrix());
        })
        .expect("spawn precommit matrix thread")
        .join()
        .expect("run precommit matrix thread");
}

async fn run_provider_json_exception_retry_and_single_credential_failure_are_private_for_five_rounds()
 {
    let captured = CapturedTestLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(captured.clone())
        .finish();
    let _subscriber_guard = tracing::subscriber::set_default(subscriber);

    for round in 1..=5 {
        let marker = format!("PROVIDER_JSON_RETRY_PRIVATE_MARKER_{round}");
        let upstream = HandlerEventStreamFaultUpstream::start_with_json_secret_marker(
            HandlerEventStreamFault::JsonExceptionBeforeOutput,
            marker.clone(),
        )
        .await;
        let (app, usage_recorder) =
            handler_eventstream_fault_router_with_limits(&upstream.base_url, 2, 2);
        let (status, request_id, body) = call_handler_eventstream_fault(app, true).await;

        assert_eq!(status, StatusCode::OK, "round={round} body={body}");
        assert!(body.contains("recovered-ok"), "round={round} body={body}");
        assert_eq!(body.matches("event: message_stop").count(), 1);
        assert!(!body.contains(&marker));
        assert_eq!(upstream.hits(), 2, "round={round}");
        assert_fault_usage(&usage_recorder, &request_id, UsageRecordStatus::Success, 2);
        let record = usage_record_for_request(&usage_recorder, &request_id);
        assert_eq!(record.credential_attempts.len(), 2, "round={round}");
        assert_ne!(
            record.credential_attempts[0].credential_id,
            record.credential_attempts[1].credential_id,
            "round={round}: JSON stream retry must move to the alternate credential"
        );
        let trace = record
            .latency_trace
            .as_ref()
            .expect("JSON stream retry trace");
        assert_eq!(trace.stream_retry_attempts, Some(1));
        assert_eq!(trace.stream_retry_dispatch_failures, None);
        assert_eq!(
            trace.stream_retry_reasons.as_deref(),
            Some(&["status_error:sends=1".to_string()][..])
        );
        let serialized = serde_json::to_string(&record).expect("serialize provider retry usage");
        assert!(!serialized.contains(&marker));
    }

    for round in 1..=5 {
        let marker = format!("PROVIDER_JSON_SINGLE_PRIVATE_MARKER_{round}");
        let upstream = HandlerEventStreamFaultUpstream::start_with_json_secret_marker(
            HandlerEventStreamFault::JsonExceptionBeforeOutput,
            marker.clone(),
        )
        .await;
        let (app, usage_recorder) =
            handler_eventstream_fault_router_with_limits(&upstream.base_url, 1, 1);
        let (status, request_id, body) = call_handler_eventstream_fault(app, true).await;

        assert_eq!(status, StatusCode::OK, "round={round} body={body}");
        assert_eq!(
            body.matches("event: message_start").count(),
            1,
            "round={round} body={body}"
        );
        assert_eq!(
            body.matches("event: error").count(),
            1,
            "round={round} body={body}"
        );
        assert!(body.contains(r#""type":"rate_limit_error""#));
        assert!(!body.contains("event: message_stop"));
        assert!(!body.contains("recovered-ok"));
        assert!(!body.contains(&marker));
        assert_eq!(upstream.hits(), 1, "round={round}");
        assert_fault_usage(
            &usage_recorder,
            &request_id,
            UsageRecordStatus::StreamError,
            1,
        );
        let record = usage_record_for_request(&usage_recorder, &request_id);
        assert_eq!(record.credential_attempts.len(), 1);
        let trace = record
            .latency_trace
            .as_ref()
            .expect("single JSON stream failure trace");
        assert_eq!(trace.stream_retry_attempts, None);
        assert_eq!(trace.stream_retry_dispatch_failures, Some(1));
        assert_eq!(
            trace.stream_retry_reasons.as_deref(),
            Some(&["status_error:dispatch_failed_without_send".to_string()][..])
        );
        let error_id = record
            .error_id
            .as_deref()
            .expect("single JSON stream error-id");
        assert!(body.contains(error_id), "round={round} body={body}");
        let serialized =
            serde_json::to_string(&record).expect("serialize provider JSON failure usage");
        assert!(!serialized.contains(&marker));
    }

    let logs = captured.snapshot();
    for round in 1..=5 {
        assert!(!logs.contains(&format!("PROVIDER_JSON_RETRY_PRIVATE_MARKER_{round}")));
        assert!(!logs.contains(&format!("PROVIDER_JSON_SINGLE_PRIVATE_MARKER_{round}")));
    }
}

#[test]
fn provider_json_exception_retry_and_single_credential_failure_are_private_for_five_rounds() {
    run_handler_fixture_on_four_mib_thread("provider-json-fault-matrix", || async {
        run_provider_json_exception_retry_and_single_credential_failure_are_private_for_five_rounds()
            .await;
    });
}

#[test]
fn eventstream_content_type_with_json_bytes_uses_protocol_retry_for_five_rounds() {
    std::thread::Builder::new()
        .name("mislabeled-json-eventstream-fixture".to_string())
        .stack_size(4 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build mislabeled JSON EventStream runtime");
            runtime.block_on(async {
                for round in 1..=5 {
                    assert_handler_eventstream_precommit_retry(
                        HandlerEventStreamFault::JsonBodyWithEventStreamContentType,
                        round,
                    )
                    .await;
                }
            });
        })
        .expect("spawn mislabeled JSON EventStream fixture")
        .join()
        .expect("run mislabeled JSON EventStream fixture");
}

#[cfg(not(debug_assertions))]
fn boxed_precommit_retry_heap_control(rounds: usize) -> Pin<Box<dyn Future<Output = ()> + Send>> {
    Box::pin(async move {
        for round in 1..=rounds {
            assert_handler_eventstream_precommit_retry(
                HandlerEventStreamFault::BadCrcBeforeOutput,
                round,
            )
            .await;
        }
    })
}

#[cfg(not(debug_assertions))]
#[test]
fn handler_precommit_retry_runs_on_two_mib_worker_stack_for_one_minimal_case() {
    let future = std::thread::Builder::new()
        .name("precommit-fixture-constructor".to_string())
        .stack_size(4 * 1024 * 1024)
        .spawn(|| boxed_precommit_retry_heap_control(1))
        .expect("spawn bounded fixture constructor")
        .join()
        .expect("construct boxed precommit retry future");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_stack_size(2 * 1024 * 1024)
        .enable_all()
        .build()
        .expect("build two-MiB worker runtime");
    runtime.block_on(async move {
        tokio::spawn(future)
            .await
            .expect("heap-spawned precommit retry task");
    });
}

#[test]
fn handler_precommit_retry_future_sizes_remain_below_four_mib() {
    fn two_arg_future_size<A, B, F: Future>(_: fn(A, B) -> F) -> usize {
        std::mem::size_of::<F>()
    }
    fn one_arg_future_size<A, F: Future>(_: fn(A) -> F) -> usize {
        std::mem::size_of::<F>()
    }

    let case_future = two_arg_future_size(assert_handler_eventstream_precommit_retry);
    let handler_call_future = two_arg_future_size(call_handler_eventstream_fault);
    let upstream_start_future = one_arg_future_size(HandlerEventStreamFaultUpstream::start);
    eprintln!(
        "precommit future sizes: case={case_future} handler_call={handler_call_future} upstream_start={upstream_start_future}"
    );
    assert!(case_future < 4 * 1024 * 1024);
    assert!(handler_call_future < 4 * 1024 * 1024);
    assert!(upstream_start_future < 4 * 1024 * 1024);
}

async fn run_single_credential_precommit_retry_matrix() {
    let captured = CapturedTestLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(captured.clone())
        .finish();
    let _subscriber_guard = tracing::subscriber::set_default(subscriber);

    for round in 1..=5 {
        let marker = format!("SINGLE_CREDENTIAL_PROTOCOL_SECRET_MARKER_{round}");
        let upstream = HandlerEventStreamFaultUpstream::start_with_json_secret_marker(
            HandlerEventStreamFault::BadCrcBeforeOutput,
            marker.clone(),
        )
        .await;
        let (app, usage_recorder) =
            handler_eventstream_fault_router_with_credential_count(&upstream.base_url, 1);
        let (status, request_id, body) = call_handler_eventstream_fault(app, true).await;

        assert_eq!(status, StatusCode::OK, "round={round} body={body}");
        assert_eq!(
            body.matches("event: message_start").count(),
            1,
            "round={round} body={body}"
        );
        assert_eq!(
            body.matches("event: error").count(),
            1,
            "round={round} body={body}"
        );
        assert!(body.contains(r#""type":"api_error""#));
        assert!(!body.contains("event: message_stop"));
        assert!(!body.contains("recovered-ok"));
        assert!(!body.contains(&marker));
        assert_eq!(upstream.hits(), 1, "round={round} body={body}");

        assert_fault_usage(
            &usage_recorder,
            &request_id,
            UsageRecordStatus::StreamError,
            1,
        );
        let record = usage_record_for_request(&usage_recorder, &request_id);
        let trace = record
            .latency_trace
            .as_ref()
            .expect("single-credential latency trace");
        let attempts = trace
            .inference_attempts
            .expect("single-credential inference attempt snapshot");
        assert_eq!(attempts.local_attempts, 1, "round={round}");
        assert_eq!(attempts.external_attempts, 0, "round={round}");
        assert!(attempts.consumed <= attempts.max_attempts, "round={round}");
        assert_eq!(trace.stream_retry_attempts, None, "round={round}");
        assert_eq!(
            trace.stream_retry_dispatch_failures,
            Some(1),
            "round={round}"
        );
        assert_eq!(
            trace.stream_retry_reasons.as_deref(),
            Some(&["protocol_error:dispatch_failed_without_send".to_string()][..]),
            "round={round}"
        );
        assert!(record.credential_attempts.len() <= 1, "round={round}");
        let error_id = record
            .error_id
            .as_deref()
            .expect("single-credential stream error-id");
        assert!(body.contains(error_id), "round={round} body={body}");
        let serialized = serde_json::to_string(&record).expect("serialize single-credential usage");
        assert!(!serialized.contains(&marker), "round={round}: {serialized}");
    }

    let logs = captured.snapshot();
    assert_eq!(
        logs.matches("本地 Kiro 流式响应在首个下游事件前失败，准备换号重试")
            .count(),
        5,
        "logs={logs}"
    );
    assert_eq!(
        logs.matches("本地 Kiro 流式首输出前重试失败").count(),
        5,
        "logs={logs}"
    );
    for round in 1..=5 {
        assert!(!logs.contains(&format!("SINGLE_CREDENTIAL_PROTOCOL_SECRET_MARKER_{round}")));
    }
}

#[test]
fn handler_single_credential_precommit_retry_is_bounded_and_fails_closed_for_five_rounds() {
    std::thread::Builder::new()
        .name("single-credential-precommit-fixture".to_string())
        .stack_size(4 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build single-credential precommit runtime");
            runtime.block_on(run_single_credential_precommit_retry_matrix());
        })
        .expect("spawn single-credential precommit fixture")
        .join()
        .expect("run single-credential precommit fixture");
}

async fn run_handler_eventstream_postcommit_faults_matrix() {
    let cases = [
        (
            HandlerEventStreamFault::TextThenReadError,
            "postcommit-visible",
        ),
        (
            HandlerEventStreamFault::ThinkingThenReadError,
            "thinking_delta",
        ),
        (
            HandlerEventStreamFault::ToolThenReadError,
            r#""type":"tool_use""#,
        ),
    ];

    for (fault, expected_visible) in cases {
        for round in 1..=5 {
            let upstream = HandlerEventStreamFaultUpstream::start(fault).await;
            let (app, usage_recorder) = handler_eventstream_fault_router(&upstream.base_url);
            let (status, request_id, body) = call_handler_eventstream_fault(app, true).await;

            assert_eq!(status, StatusCode::OK, "fault={fault:?} round={round}");
            assert!(
                body.contains(expected_visible),
                "fault={fault:?} round={round} body={body}"
            );
            assert!(body.contains(r#""type":"error""#));
            assert!(body.contains(r#""type":"api_error""#));
            assert!(!body.contains("event: message_stop"));
            assert!(!body.contains("upstream stream read error"));
            assert_eq!(upstream.hits(), 1, "fault={fault:?} round={round}");
            assert_fault_usage(
                &usage_recorder,
                &request_id,
                UsageRecordStatus::StreamError,
                1,
            );
            let record = usage_record_for_request(&usage_recorder, &request_id);
            assert_eq!(
                record.public_error_type.as_deref(),
                Some("api_error"),
                "fault={fault:?} round={round}"
            );
            let error_id = record
                .error_id
                .as_deref()
                .expect("postcommit stream error usage error-id");
            assert!(
                body.contains(error_id),
                "fault={fault:?} round={round} request_id={request_id} error_id={error_id} hits={} body={body}",
                upstream.hits()
            );
        }
    }
}

#[test]
fn handler_eventstream_postcommit_faults_never_retry_or_fake_success_for_five_rounds() {
    run_handler_fixture_on_four_mib_thread(
        "postcommit-eventstream-fixture",
        run_handler_eventstream_postcommit_faults_matrix,
    );
}

async fn run_handler_non_stream_eventstream_faults_matrix() {
    let cases = [
        HandlerEventStreamFault::JsonExceptionBeforeOutput,
        HandlerEventStreamFault::BadCrcBeforeOutput,
        HandlerEventStreamFault::TruncatedFrameBeforeOutput,
        HandlerEventStreamFault::IncompleteStatusBeforeOutput,
    ];

    for fault in cases {
        for round in 1..=5 {
            let upstream = HandlerEventStreamFaultUpstream::start(fault).await;
            let (app, usage_recorder) = handler_eventstream_fault_router(&upstream.base_url);
            let (status, request_id, body) = call_handler_eventstream_fault(app, false).await;

            assert!(status.is_client_error() || status.is_server_error());
            assert!(body.contains(&request_id));
            assert!(!body.contains("private fault fixture detail"));
            assert!(!body.contains("must-not-appear"));
            assert_eq!(upstream.hits(), 1, "fault={fault:?} round={round}");
            assert_fault_usage(&usage_recorder, &request_id, UsageRecordStatus::Error, 1);
        }
    }
}

#[test]
fn handler_non_stream_eventstream_faults_fail_closed_for_five_rounds() {
    run_handler_fixture_on_four_mib_thread(
        "non-stream-eventstream-fixture",
        run_handler_non_stream_eventstream_faults_matrix,
    );
}

async fn run_handler_binary_eventstream_with_json_content_type_matrix() {
    for stream in [true, false] {
        for round in 1..=5 {
            let upstream = HandlerEventStreamFaultUpstream::start(
                HandlerEventStreamFault::BinaryEventStreamWithJsonContentType,
            )
            .await;
            let (app, usage_recorder) = handler_eventstream_fault_router(&upstream.base_url);
            let (status, request_id, body) = call_handler_eventstream_fault(app, stream).await;

            assert_eq!(
                status,
                StatusCode::OK,
                "stream={stream} round={round} body={body}"
            );
            assert!(
                body.contains("recovered-ok"),
                "stream={stream} round={round} body={body}"
            );
            assert!(
                !body.contains(r#""type":"error""#),
                "stream={stream} round={round} body={body}"
            );
            if stream {
                assert_eq!(
                    body.matches("event: message_stop").count(),
                    1,
                    "stream={stream} round={round} body={body}"
                );
            }
            assert_eq!(upstream.hits(), 1, "stream={stream} round={round}");
            assert_fault_usage(&usage_recorder, &request_id, UsageRecordStatus::Success, 1);
        }
    }
}

#[test]
fn handler_binary_eventstream_with_json_content_type_is_body_sniffed_for_five_rounds() {
    run_handler_fixture_on_four_mib_thread(
        "json-labeled-binary-eventstream-fixture",
        run_handler_binary_eventstream_with_json_content_type_matrix,
    );
}

async fn run_handler_json_stream_secret_markers_matrix() {
    let captured = CapturedTestLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .without_time()
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(captured.clone())
        .finish();
    let _subscriber_guard = tracing::subscriber::set_default(subscriber);

    for round in 1..=5 {
        let marker = format!("JSON_STREAM_PRIVATE_SECRET_MARKER_{round}");
        let upstream = HandlerEventStreamFaultUpstream::start_with_json_secret_marker(
            HandlerEventStreamFault::JsonExceptionBeforeOutput,
            marker.clone(),
        )
        .await;
        let (app, usage_recorder) = handler_eventstream_fault_router(&upstream.base_url);
        let (status, request_id, body) = call_handler_eventstream_fault(app, false).await;

        assert_eq!(
            status,
            StatusCode::TOO_MANY_REQUESTS,
            "round={round} body={body}"
        );
        assert!(body.contains(&request_id), "round={round} body={body}");
        assert!(!body.contains(&marker), "round={round} body={body}");
        assert_eq!(upstream.hits(), 1, "round={round}");
        assert_fault_usage(&usage_recorder, &request_id, UsageRecordStatus::Error, 1);

        let record = usage_record_for_request(&usage_recorder, &request_id);
        assert_eq!(record.public_error_status_code, Some(429), "round={round}");
        assert_eq!(
            record.public_error_type.as_deref(),
            Some("rate_limit_error"),
            "round={round}"
        );
        let serialized = serde_json::to_string(&record).expect("serialize JSON error usage");
        assert!(
            !serialized.contains(&marker),
            "round={round}: usage captured private JSON marker: {serialized}"
        );
    }

    let logs = captured.snapshot();
    for round in 1..=5 {
        let marker = format!("JSON_STREAM_PRIVATE_SECRET_MARKER_{round}");
        assert!(
            !logs.contains(&marker),
            "round={round}: DEBUG logs captured private JSON marker"
        );
    }
}

#[test]
fn handler_json_stream_secret_markers_never_reach_logs_or_usage_for_five_rounds() {
    run_handler_fixture_on_four_mib_thread(
        "json-stream-privacy-fixture",
        run_handler_json_stream_secret_markers_matrix,
    );
}

async fn run_handler_unknown_event_only_matrix() {
    for round in 1..=5 {
        let marker = format!("UNKNOWN_EVENT_PRIVATE_MARKER_{round}");
        let upstream = HandlerEventStreamFaultUpstream::start_with_json_secret_marker(
            HandlerEventStreamFault::UnknownEventOnly,
            marker.clone(),
        )
        .await;
        let (app, usage_recorder) = handler_eventstream_fault_router(&upstream.base_url);
        let (status, request_id, body) = call_handler_eventstream_fault(app, true).await;
        assert_eq!(status, StatusCode::OK, "unknown round={round}");
        assert!(
            body.contains("recovered-ok"),
            "unknown round={round} body={body}"
        );
        assert!(!body.contains(&marker), "unknown round={round} body={body}");
        assert_eq!(upstream.hits(), 2, "unknown round={round}");
        assert_fault_usage(&usage_recorder, &request_id, UsageRecordStatus::Success, 2);
        let record = usage_record_for_request(&usage_recorder, &request_id);
        let trace = record
            .latency_trace
            .as_ref()
            .expect("unknown-event retry latency trace");
        assert_eq!(trace.stream_retry_attempts, Some(1));
        assert_eq!(trace.stream_retry_dispatch_failures, None);
        assert_eq!(
            trace.stream_retry_reasons.as_deref(),
            Some(&["protocol_error:sends=1".to_string()][..])
        );
        let serialized = serde_json::to_string(&record).expect("serialize unknown-event usage");
        assert!(!serialized.contains(&marker), "unknown round={round}");
    }
}

#[test]
fn handler_unknown_event_only_retries_before_empty_success_for_five_rounds() {
    run_handler_fixture_on_four_mib_thread(
        "unknown-event-terminal-fixture",
        run_handler_unknown_event_only_matrix,
    );
}

async fn run_handler_missing_completion_after_text_matrix() {
    for round in 1..=5 {
        let upstream = HandlerEventStreamFaultUpstream::start(
            HandlerEventStreamFault::MissingCompletionAfterText,
        )
        .await;
        let (app, usage_recorder) = handler_eventstream_fault_router(&upstream.base_url);
        let (status, request_id, body) = call_handler_eventstream_fault(app, true).await;
        assert_eq!(status, StatusCode::OK, "missing completion round={round}");
        assert!(body.contains("unterminated-visible"));
        assert!(
            body.contains(r#""type":"error""#),
            "missing completion round={round} hits={} body={body}",
            upstream.hits()
        );
        assert!(!body.contains("event: message_stop"));
        assert!(!body.contains("upstream eventstream"));
        assert_eq!(upstream.hits(), 1, "missing completion round={round}");
        assert_fault_usage(
            &usage_recorder,
            &request_id,
            UsageRecordStatus::StreamError,
            1,
        );
        let record = usage_record_for_request(&usage_recorder, &request_id);
        let trace = record
            .latency_trace
            .as_ref()
            .expect("missing-completion latency trace");
        assert_eq!(trace.stream_retry_attempts, None);
        assert_eq!(trace.stream_retry_dispatch_failures, None);
        let error_id = record
            .error_id
            .as_deref()
            .expect("missing-completion stream error-id");
        assert!(body.contains(error_id), "round={round} body={body}");
    }
}

#[test]
fn handler_missing_completion_after_text_fails_closed_for_five_rounds() {
    run_handler_fixture_on_four_mib_thread(
        "missing-completion-terminal-fixture",
        run_handler_missing_completion_after_text_matrix,
    );
}

async fn run_handler_non_stream_untrusted_eof_matrix() {
    for (fault, forbidden) in [
        (
            HandlerEventStreamFault::UnknownEventOnly,
            "UNKNOWN_NON_STREAM_PRIVATE_MARKER",
        ),
        (
            HandlerEventStreamFault::MissingCompletionAfterText,
            "unterminated-visible",
        ),
    ] {
        for round in 1..=5 {
            let marker = format!("{forbidden}_{round}");
            let upstream = HandlerEventStreamFaultUpstream::start_with_json_secret_marker(
                fault,
                marker.clone(),
            )
            .await;
            let (app, usage_recorder) = handler_eventstream_fault_router(&upstream.base_url);
            let (status, request_id, body) = call_handler_eventstream_fault(app, false).await;

            assert_eq!(
                status,
                StatusCode::BAD_GATEWAY,
                "fault={fault:?} round={round} body={body}"
            );
            assert!(body.contains(&request_id));
            assert!(!body.contains("upstream eventstream"));
            assert!(!body.contains(&marker));
            assert!(!body.contains("unterminated-visible"));
            assert_eq!(upstream.hits(), 1, "fault={fault:?} round={round}");
            assert_fault_usage(&usage_recorder, &request_id, UsageRecordStatus::Error, 1);
            let record = usage_record_for_request(&usage_recorder, &request_id);
            assert_eq!(record.public_error_status_code, Some(502));
            assert_eq!(record.public_error_type.as_deref(), Some("api_error"));
            let serialized = serde_json::to_string(&record).expect("serialize untrusted EOF usage");
            assert!(!serialized.contains(&marker));
            assert!(!serialized.contains("unterminated-visible"));
        }
    }
}

#[test]
fn handler_non_stream_untrusted_eof_fails_closed_for_five_rounds() {
    run_handler_fixture_on_four_mib_thread(
        "non-stream-untrusted-eof-fixture",
        run_handler_non_stream_untrusted_eof_matrix,
    );
}

async fn run_handler_legacy_metadata_and_complete_tool_matrix() {
    for (fault, expected_content, expected_stop_reason, expected_kiro_metering_usage) in [
        (
            HandlerEventStreamFault::LegacyTextWithMetadataNoStatus,
            Some("legacy-terminal-ok"),
            "end_turn",
            0.0,
        ),
        (
            HandlerEventStreamFault::TextWithMeteringNoStatus,
            Some("metered-terminal-ok"),
            "end_turn",
            0.42,
        ),
        (
            HandlerEventStreamFault::UsageOnlyMeteringNoStatus,
            None,
            "end_turn",
            0.24,
        ),
        (
            HandlerEventStreamFault::CompleteToolWithoutStatus,
            Some(r#""name":"Bash""#),
            "tool_use",
            0.0,
        ),
        (
            HandlerEventStreamFault::IncompleteToolWithoutStatus,
            Some(r#""name":"Bash""#),
            "tool_use",
            0.0,
        ),
    ] {
        for stream in [true, false] {
            for round in 1..=5 {
                let upstream = HandlerEventStreamFaultUpstream::start(fault).await;
                let (app, usage_recorder) = handler_eventstream_fault_router(&upstream.base_url);
                let (status, request_id, body) = call_handler_eventstream_fault(app, stream).await;

                assert_eq!(
                    status,
                    StatusCode::OK,
                    "fault={fault:?} stream={stream} round={round} body={body}"
                );
                if let Some(expected_content) = expected_content {
                    assert!(
                        body.contains(expected_content),
                        "fault={fault:?} stream={stream} round={round} body={body}"
                    );
                }
                assert!(
                    body.contains(&format!(r#""stop_reason":"{expected_stop_reason}""#)),
                    "fault={fault:?} stream={stream} round={round} body={body}"
                );
                assert!(!body.contains(r#""type":"error""#));
                if stream {
                    assert_eq!(body.matches("event: message_stop").count(), 1);
                }
                assert_eq!(upstream.hits(), 1, "fault={fault:?} round={round}");
                assert_fault_usage(&usage_recorder, &request_id, UsageRecordStatus::Success, 1);
                let record = usage_record_for_request(&usage_recorder, &request_id);
                assert_eq!(
                    record.downstream_stop_reason.as_deref(),
                    Some(expected_stop_reason)
                );
                assert!(
                    (record.kiro_metering_usage - expected_kiro_metering_usage).abs() < 0.000_001,
                    "fault={fault:?} stream={stream} round={round} kiro_metering_usage={}",
                    record.kiro_metering_usage
                );
                if matches!(fault, HandlerEventStreamFault::UsageOnlyMeteringNoStatus) {
                    assert_eq!(record.output_tokens, 0);
                    assert!(
                        record.total_input_tokens > 0 || record.compat_input_tokens > 0,
                        "usage-only metering should preserve an input-token signal: {record:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn handler_legacy_metadata_metering_and_complete_tool_are_trusted_terminals_for_five_rounds() {
    run_handler_fixture_on_four_mib_thread(
        "legacy-terminal-positive-fixture",
        run_handler_legacy_metadata_and_complete_tool_matrix,
    );
}

async fn run_handler_non_stream_response_body_limit_matrix() {
    for fault in [
        HandlerEventStreamFault::NonStreamContentLengthOverLimit,
        HandlerEventStreamFault::NonStreamChunkedOverLimit,
    ] {
        for round in 1..=5 {
            let upstream = HandlerEventStreamFaultUpstream::start(fault).await;
            let (app, usage_recorder) = handler_eventstream_fault_router(&upstream.base_url);
            let (status, request_id, body) =
                call_handler_eventstream_fault(app.clone(), false).await;
            assert_eq!(
                status,
                StatusCode::BAD_GATEWAY,
                "fault={fault:?} round={round} body={body}"
            );
            assert!(body.contains(&request_id));
            assert!(!body.contains("upstream response body"));
            assert_eq!(upstream.hits(), 1, "fault={fault:?} round={round}");
            assert_fault_usage(&usage_recorder, &request_id, UsageRecordStatus::Error, 1);

            let (recovery_status, recovery_request_id, recovery_body) =
                call_handler_eventstream_fault(app, false).await;
            assert_eq!(
                recovery_status,
                StatusCode::OK,
                "recovery fault={fault:?} round={round} body={recovery_body}"
            );
            assert!(recovery_body.contains("recovered-ok"));
            assert_eq!(upstream.hits(), 2, "recovery fault={fault:?} round={round}");
            assert_fault_usage(
                &usage_recorder,
                &recovery_request_id,
                UsageRecordStatus::Success,
                1,
            );
        }
    }

    for fault in [
        HandlerEventStreamFault::NonStreamExactLimit,
        HandlerEventStreamFault::NonStreamSmallBody,
    ] {
        for round in 1..=5 {
            let upstream = HandlerEventStreamFaultUpstream::start(fault).await;
            let (app, usage_recorder) = handler_eventstream_fault_router(&upstream.base_url);
            let (status, request_id, body) = call_handler_eventstream_fault(app, false).await;
            assert_eq!(
                status,
                StatusCode::OK,
                "control fault={fault:?} round={round} body={body}"
            );
            let expected = if matches!(fault, HandlerEventStreamFault::NonStreamExactLimit) {
                "exact-limit-ok"
            } else {
                "recovered-ok"
            };
            assert!(body.contains(expected));
            assert_eq!(upstream.hits(), 1, "control fault={fault:?} round={round}");
            assert_fault_usage(&usage_recorder, &request_id, UsageRecordStatus::Success, 1);
        }
    }
}

#[test]
fn handler_non_stream_response_body_limit_and_recovery_hold_for_five_rounds() {
    run_handler_fixture_on_four_mib_thread(
        "non-stream-response-limit-fixture",
        run_handler_non_stream_response_body_limit_matrix,
    );
}

#[test]
fn anthropic_content_summary_skips_tool_result_text_for_latest_user() {
    let content = json!([
        {"type": "text", "text": "please answer this"},
        {
            "type": "tool_result",
            "tool_use_id": "toolu_1",
            "content": [{"type": "text", "text": "large command output"}]
        }
    ]);

    let summary = summarize_anthropic_content(&content);

    assert_eq!(summary.kind, "array");
    assert_eq!(summary.text.bytes, "please answer this".len());
    assert_eq!(summary.text.chars, "please answer this".chars().count());
    assert_eq!(summary.text.segments, 1);
    assert_eq!(
        summary.text.hash,
        Some(short_text_hash("please answer this"))
    );
    assert_eq!(summary.tool_result_count, 1);
}

#[test]
fn count_tool_use_blocks_counts_assistant_tool_uses_without_text_hashing() {
    let content = json!([
        {"type": "text", "text": "I will call a tool."},
        {"type": "tool_use", "id": "toolu_1", "name": "Read", "input": {"file_path": "README.md"}},
        {"type": "tool_use", "id": "toolu_2", "name": "Grep", "input": {"pattern": "kiro"}}
    ]);

    assert_eq!(count_tool_use_blocks(&content), 2);
}

#[test]
fn non_stream_redacted_thinking_keeps_following_visible_text() {
    let mut content = Vec::new();
    let mut thinking_sanitizer = super::super::transcript_sanitizer::ToolTranscriptSanitizer::new(
        std::iter::empty::<String>(),
    );
    let redacted = BASE64_STANDARD.encode(b"opaque-redacted-data");
    append_non_stream_reasoning_and_text(
        &mut content,
        true,
        true,
        Some(&redacted),
        "",
        None,
        "visible answer",
        &HashSet::new(),
        &HashMap::new(),
        &ToolSchemaKeyMap::default(),
        &mut HashSet::new(),
        &mut thinking_sanitizer,
    )
    .expect("canonical redacted blob");

    assert_eq!(content.len(), 2);
    assert_eq!(content[0]["type"], "redacted_thinking");
    assert_eq!(content[0]["data"], redacted);
    assert_eq!(
        content[1],
        json!({"type": "text", "text": "visible answer"})
    );
}

#[test]
fn non_stream_native_reasoning_is_independent_from_xml_extraction_for_five_rounds() {
    let redacted = BASE64_STANDARD.encode(b"opaque-strict-redacted");
    for round in 0..5 {
        let mut signed = Vec::new();
        let mut signed_sanitizer =
            super::super::transcript_sanitizer::ToolTranscriptSanitizer::new(std::iter::empty::<
                String,
            >());
        append_non_stream_reasoning_and_text(
            &mut signed,
            true,
            false,
            None,
            "native reasoning",
            Some("opaque-signature"),
            "visible",
            &HashSet::new(),
            &HashMap::new(),
            &ToolSchemaKeyMap::default(),
            &mut HashSet::new(),
            &mut signed_sanitizer,
        )
        .unwrap_or_else(|error| panic!("round {round}: {error}"));
        assert_eq!(signed[0]["type"], "thinking");
        assert_eq!(signed[0]["signature"], "opaque-signature");

        let mut opaque = Vec::new();
        let mut opaque_sanitizer =
            super::super::transcript_sanitizer::ToolTranscriptSanitizer::new(std::iter::empty::<
                String,
            >());
        append_non_stream_reasoning_and_text(
            &mut opaque,
            true,
            false,
            Some(&redacted),
            "",
            None,
            "visible",
            &HashSet::new(),
            &HashMap::new(),
            &ToolSchemaKeyMap::default(),
            &mut HashSet::new(),
            &mut opaque_sanitizer,
        )
        .unwrap_or_else(|error| panic!("round {round}: {error}"));
        assert_eq!(opaque[0]["type"], "redacted_thinking");
        assert_eq!(opaque[0]["data"], redacted);
    }
}

#[test]
fn non_stream_thinking_policy_sanitizes_unsigned_and_drops_atomic_blocks() {
    let polluted = "safe prefix\nuser Continue\n\nBash: hidden";
    for _round in 0..5 {
        let known = HashSet::from(["Bash".to_string()]);

        let mut unsigned = Vec::new();
        let mut unsigned_sanitizer =
            super::super::transcript_sanitizer::ToolTranscriptSanitizer::new(known.clone());
        append_non_stream_reasoning_and_text(
            &mut unsigned,
            true,
            true,
            None,
            polluted,
            None,
            "visible",
            &known,
            &HashMap::new(),
            &ToolSchemaKeyMap::default(),
            &mut HashSet::new(),
            &mut unsigned_sanitizer,
        )
        .expect("unsigned thinking");
        assert_eq!(unsigned[0]["type"], "thinking");
        assert_eq!(unsigned[0]["thinking"], "safe prefix\n");
        assert_eq!(unsigned[1]["text"], "visible");

        let mut signed = Vec::new();
        let mut signed_sanitizer =
            super::super::transcript_sanitizer::ToolTranscriptSanitizer::new(known.clone());
        append_non_stream_reasoning_and_text(
            &mut signed,
            true,
            true,
            None,
            polluted,
            Some("opaque-signature"),
            "visible",
            &known,
            &HashMap::new(),
            &ToolSchemaKeyMap::default(),
            &mut HashSet::new(),
            &mut signed_sanitizer,
        )
        .expect("signed thinking");
        assert_eq!(signed, vec![json!({"type":"text","text":"visible"})]);

        let mut redacted = Vec::new();
        let mut redacted_sanitizer =
            super::super::transcript_sanitizer::ToolTranscriptSanitizer::new(known.clone());
        let error = append_non_stream_reasoning_and_text(
            &mut redacted,
            true,
            true,
            Some(polluted),
            "",
            None,
            "visible",
            &known,
            &HashMap::new(),
            &ToolSchemaKeyMap::default(),
            &mut HashSet::new(),
            &mut redacted_sanitizer,
        )
        .expect_err("plaintext redacted data must be rejected");
        assert!(error.contains("canonical base64"));
        assert!(redacted.is_empty());
    }
}

#[test]
fn json_stream_sniffer_detects_request_body_invalid_exception() {
    for round in 1..=5 {
        let marker = format!("JSON_STREAM_INVALID_SECRET_MARKER_{round}");
        let raw = serde_json::to_vec(&json!({
            "message": format!("Invalid tool use format. {marker}"),
            "reason": "REQUEST_BODY_INVALID"
        }))
        .expect("serialize invalid request fixture");
        let mut sniffer = JsonStreamErrorSniffer::new(Some("application/json; charset=utf-8"));

        match sniffer.inspect(Bytes::from(raw.clone())) {
            JsonStreamSniffResult::Error(error) => {
                assert_eq!(error.error_type, "invalid_request_error");
                assert_eq!(
                    error.internal_detail,
                    "json_stream_exception classified_as=invalid_request_error code_present=false reason_present=true message_present=true"
                );
                assert_eq!(error.body_bytes, raw.len());
                assert!(!error.internal_detail.contains(&marker));
                let diagnostics = error.diagnostics.as_ref().expect("body diagnostics");
                assert_eq!(diagnostics["bodyKind"], "json_error_envelope");
                assert_eq!(diagnostics["firstNonWhitespace"], "json_object");
                assert_eq!(diagnostics["jsonErrorFields"]["reasonPresent"], true);
                assert_eq!(diagnostics["jsonErrorFields"]["messagePresent"], true);
                assert!(
                    diagnostics["bodyFingerprint"]
                        .as_str()
                        .unwrap()
                        .starts_with("sha256:")
                );
                assert!(
                    !diagnostics.to_string().contains(&marker),
                    "diagnostics must not contain raw body values"
                );
            }
            _ => panic!("round {round}: expected JSON stream error"),
        }
    }
}

#[test]
fn json_stream_sniffer_passes_binary_eventstream_mislabeled_as_json() {
    let mut sniffer = JsonStreamErrorSniffer::new(Some("application/json"));
    let chunk = Bytes::from_static(&[0, 0, 0, 16, 0, 0, 0, 0]);

    match sniffer.inspect(chunk.clone()) {
        JsonStreamSniffResult::Pass(passed) => {
            assert_eq!(passed, chunk);
            assert_eq!(
                passed.as_ptr(),
                chunk.as_ptr(),
                "binary fast path is zero-copy"
            );
        }
        _ => panic!("expected binary eventstream chunk to pass through"),
    }
}

#[test]
fn json_stream_sniffer_accumulates_split_json_exception() {
    for round in 1..=5 {
        let marker = format!("JSON_STREAM_SPLIT_SECRET_MARKER_{round}");
        let first = Bytes::from_static(br#"{"message":"Too many"#);
        let second = Bytes::from(format!(
            " requests {marker}\",\"code\":\"ThrottlingException\"}}"
        ));
        let expected_body_bytes = first.len() + second.len();
        let mut sniffer = JsonStreamErrorSniffer::new(Some("application/json"));

        assert!(matches!(
            sniffer.inspect(first),
            JsonStreamSniffResult::Pending
        ));

        match sniffer.inspect(second) {
            JsonStreamSniffResult::Error(error) => {
                assert_eq!(error.error_type, "rate_limit_error");
                assert_eq!(
                    error.internal_detail,
                    "json_stream_exception classified_as=rate_limit_error code_present=true reason_present=false message_present=true"
                );
                assert_eq!(error.body_bytes, expected_body_bytes);
                assert!(!error.internal_detail.contains(&marker));
                let diagnostics = error.diagnostics.as_ref().expect("body diagnostics");
                assert_eq!(diagnostics["bodyKind"], "json_error_envelope");
                assert_eq!(diagnostics["jsonErrorFields"]["codePresent"], true);
                assert!(
                    !diagnostics.to_string().contains(&marker),
                    "diagnostics must not contain raw body values"
                );
            }
            _ => panic!("round {round}: expected split JSON stream error"),
        }
    }
}

#[test]
fn complete_upstream_body_rejects_json_exception_without_copying_eventstream() {
    for round in 1..=5 {
        let marker = format!("JSON_STREAM_COMPLETE_SECRET_MARKER_{round}");
        let raw = serde_json::to_vec(&json!({
            "__type": "ThrottlingException",
            "message": format!("Rate exceeded {marker}")
        }))
        .expect("serialize complete JSON exception fixture");
        let error =
            inspect_complete_upstream_body(Some("application/json"), Bytes::from(raw.clone()))
                .expect_err("JSON exception must not be treated as an EventStream body");
        assert_eq!(error.error_type, "rate_limit_error");
        assert_eq!(
            error.internal_detail,
            "json_stream_exception classified_as=rate_limit_error code_present=true reason_present=false message_present=true"
        );
        assert_eq!(error.body_bytes, raw.len());
        assert!(!error.internal_detail.contains(&marker));
        let diagnostics = error.diagnostics.as_ref().expect("body diagnostics");
        assert_eq!(diagnostics["bodyKind"], "json_error_envelope");
        assert_eq!(diagnostics["jsonErrorFields"]["codePresent"], true);
        assert!(
            !diagnostics.to_string().contains(&marker),
            "diagnostics must not contain raw body values"
        );
    }

    let frame = eventstream_test_frame(
        "assistantResponseEvent",
        json!({"content":"exact body","messageStatus":"COMPLETED"}),
    );
    let passed =
        inspect_complete_upstream_body(Some("application/json"), Bytes::from(frame.clone()))
            .expect("binary EventStream mislabeled as JSON remains byte-exact");
    assert_eq!(passed.as_ref(), frame.as_slice());
}

#[test]
fn complete_eventstream_decoder_accepts_only_complete_valid_frames() {
    let frame = eventstream_test_frame(
        "assistantResponseEvent",
        json!({"content":"visible answer","messageStatus":"COMPLETED"}),
    );

    for _ in 0..5 {
        let events = decode_complete_eventstream(&frame).expect("valid frame decodes");
        assert_eq!(events.len(), 1);
        match &events[0] {
            Event::AssistantResponse(response) => {
                assert_eq!(response.content, "visible answer");
                assert_eq!(response.message_status.as_deref(), Some("COMPLETED"));
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    let empty = decode_complete_eventstream(&[]).expect_err("empty body is not success");
    assert!(empty.contains("without any frames"));

    for cut in [1, 4, 8, frame.len() / 2, frame.len() - 1] {
        let error = decode_complete_eventstream(&frame[..cut])
            .expect_err("truncated frame must not be accepted");
        assert!(error.contains("undecoded bytes"), "{error}");
    }
}

#[test]
fn complete_eventstream_decoder_rejects_crc_and_payload_corruption() {
    let frame = eventstream_test_frame(
        "assistantResponseEvent",
        json!({"content":"visible answer","messageStatus":"COMPLETED"}),
    );
    for _ in 0..5 {
        let mut bad_crc = frame.clone();
        let last = bad_crc.len() - 1;
        bad_crc[last] ^= 0xff;
        let error = decode_complete_eventstream(&bad_crc)
            .expect_err("bad message CRC must not be accepted");
        assert!(error.contains("CRC"), "{error}");
    }

    let invalid_payload = eventstream_test_frame(
        "assistantResponseEvent",
        serde_json::Value::String("not an assistant event object".to_string()),
    );
    let error = decode_complete_eventstream(&invalid_payload)
        .expect_err("invalid event payload must not be accepted");
    assert!(error.contains("payload parse error"), "{error}");
}

#[test]
fn claude_code_noop_delta_keepalive_is_version_gated() {
    assert_eq!(
        extract_claude_code_cli_version("claude-cli/2.1.197 (external, cli)"),
        Some("2.1.197")
    );
    assert!(should_use_claude_code_noop_delta_keepalive(Some(
        "claude-cli/2.1.193 (external, cli)"
    )));
    assert!(should_use_claude_code_noop_delta_keepalive(Some(
        "Claude-CLI/2.1.197 (Claude Code)"
    )));
    assert!(!should_use_claude_code_noop_delta_keepalive(Some(
        "claude-cli/2.1.192 (external, cli)"
    )));
    assert!(!should_use_claude_code_noop_delta_keepalive(Some(
        "curl/8.0"
    )));
    assert!(!should_use_claude_code_noop_delta_keepalive(None));
}

#[tokio::test]
async fn parse_messages_payload_rejects_empty_model_before_routing() {
    for model in ["", "   "] {
        let body = Bytes::from(
            json!({
                "model": model,
                "max_tokens": 16,
                "messages": [{"role": "user", "content": "hello"}]
            })
            .to_string(),
        );

        let response = parse_messages_payload(&body).expect_err("empty model rejected");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read error body");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("json envelope");
        assert_eq!(value["error"]["type"], "invalid_request_error");
        assert_eq!(
            value["error"]["message"],
            "model: field is required and cannot be empty"
        );
        assert!(
            value["request_id"]
                .as_str()
                .is_some_and(|request_id| request_id.starts_with("req_01"))
        );
    }
}

#[test]
fn defined_cache_route_requires_explicit_configuration() {
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        true,
        Arc::new(UsageRecorder::new(10)),
        Arc::new(PromptCacheTracker::default()),
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::HighCache,
        0.98,
        CompatProfile::ClaudeCode,
        false,
    )
    .with_defined_cache_routes(vec!["/dfcache/cc".to_string()]);

    assert_eq!(
        resolve_defined_cache_route(&state, "cc").unwrap(),
        "/dfcache/cc"
    );
    assert!(resolve_defined_cache_route(&state, "aa").is_err());
    assert!(resolve_defined_cache_route(&state, "aa/b").is_err());
}

#[test]
fn raw_external_route_request_is_preparse_raw_only() {
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        true,
        Arc::new(UsageRecorder::new(10)),
        Arc::new(PromptCacheTracker::default()),
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::HighCache,
        0.98,
        CompatProfile::ClaudeCode,
        false,
    );
    let runtime_config = RequestRuntimeConfig::from_app_state(&state);
    let cache_route = runtime_config.cache_policy_for_path("/cc/v1/messages");
    let raw_body = Bytes::from_static(
        br#"{"model":"client-model","stream":true,"messages":[{"role":"user","content":"hello"}]}"#,
    );

    let route = raw_external_route_request(
        &state,
        &runtime_config,
        &cache_route,
        HeaderMap::new(),
        raw_body.clone(),
        "/cc/v1/messages",
        "req_raw_preparse_test".to_string(),
        UsageRouteSubtype::ExternalFallbackPreflight,
        Some("local_capacity_full".to_string()),
        None,
        Some(json!({"preflightStage":"before_parse"})),
        Arc::new(InferenceAttemptBudget::new(4)),
        None,
    );

    assert_eq!(route.raw_body, raw_body);
    assert_eq!(route.effective_raw_body, raw_body);
    assert!(route.payload.is_none());
    assert_eq!(
        route.body_mode_filter,
        Some(ExternalPoolRequestBodyMode::RawPassthrough)
    );
    assert_eq!(route.model_hint.as_deref(), Some("client-model"));
    assert_eq!(route.stream_hint, Some(true));
    assert_eq!(route.request_input_tokens, 0);
    assert!(!route.payload_guard_external_enabled);
    assert_eq!(
        route
            .local_preflight
            .as_ref()
            .and_then(|value| value.get("preflightStage"))
            .and_then(|value| value.as_str()),
        Some("before_parse")
    );
}

#[test]
fn contaminated_fallback_requires_normalized_pool_for_five_rounds() {
    for _round in 0..5 {
        assert_eq!(external_fallback_body_mode_filter(false), None);
        assert_eq!(
            external_fallback_body_mode_filter(true),
            Some(ExternalPoolRequestBodyMode::Normalized)
        );
    }
}

#[test]
fn all_parsed_external_fallback_entrypoints_share_model_only_eligibility() {
    let source = include_str!("../handlers.rs");
    assert_eq!(
        source
            .matches("fn has_eligible_external_pool_for_model")
            .count(),
        1
    );
    assert_eq!(
        source
            .matches("fn has_immediately_available_external_pool_for_model")
            .count(),
        1
    );
    assert_eq!(
        source
            .matches(".external_pool_ready_for_route_reason(")
            .count(),
        2,
        "parsed preflight and local-error fallback must share the route-reason availability gate"
    );
    assert!(
        source.contains(".has_cached_immediately_available_pool_for_model("),
        "local attempt policy must only switch to fail-fast when cached external capacity is immediately available"
    );
    assert!(
        source.contains(
            "body_mode_filter: external_fallback_body_mode_filter(self.requires_normalized_body)"
        ),
        "body mode remains a route body-processing hint, not an external-pool candidate filter"
    );
    assert!(
        !source.contains("has_eligible_pool_for_body_mode_and_model"),
        "parsed external fallback eligibility must be model-based, not body-mode-based"
    );
    assert!(
        !source.contains("has_immediately_available_pool_for_body_mode_and_model"),
        "parsed immediate external fallback readiness must be model-based, not body-mode-based"
    );
}

#[test]
fn direct_external_policy_resolves_model_before_route_request() {
    let source = include_str!("../handlers.rs");

    assert!(
        source.contains(
            "let direct_model_resolution = state.model_capabilities.resolve_model_with_mapping("
        ),
        "direct external policy must compute 模型（本地解析） before bypassing local credentials"
    );
    assert!(
        source.contains(".direct_policy_response(&request_id, direct_model_resolution)"),
        "direct external policy must pass 模型（本地解析） into the external route"
    );
    assert!(
        source.contains("external.model_resolution = model_resolution;"),
        "external direct route must retain 模型（本地解析） for external 模型处理"
    );
    assert!(
        source.contains("external_route_model_resolution(direct_model_resolution)"),
        "direct external policy must use the same processed 模型（上游） that local Kiro dispatch would use"
    );
}

#[test]
fn external_route_model_resolution_prefers_local_processed_model_for_cc_aliases() {
    let resolved = external_route_model_resolution(ModelResolution::exact(
        "claude-opus-4-6-thinking".to_string(),
    ));

    assert_eq!(resolved.source, ModelResolutionSource::Alias);
    assert_eq!(resolved.upstream_model.as_deref(), Some("claude-opus-4.6"));
    assert_eq!(
        resolved.note.as_deref(),
        Some("claude-opus-4-6-thinking -> claude-opus-4.6")
    );
}

#[test]
fn native_websearch_runs_local_pool_preflight_before_mcp_intercept() {
    let source = include_str!("../handlers.rs");
    let websearch_block = source
        .split("if websearch::has_native_web_search_tool(&payload)")
        .nth(1)
        .expect("native WebSearch block exists");
    let preflight = websearch_block
        .find("maybe_local_pool_preflight_external_response")
        .expect("native WebSearch must run local-pool preflight before MCP");
    let mcp_call = websearch_block
        .find("websearch::handle_websearch_request")
        .expect("native WebSearch MCP handler exists");
    assert!(
        preflight < mcp_call,
        "local-pool preflight must happen before native MCP intercept so external-only deployments do not fail with websearch_mcp_scheduler_unavailable"
    );
}

#[test]
fn raw_external_route_request_applies_non_stream_skip_cache_route() {
    let mut cache_policy = CachePolicyConfig::default();
    cache_policy.path_overrides.insert(
        "/cc".to_string(),
        CacheRoutePolicyPatch {
            cache_type: Some(PromptCacheStrategyType::CurrentHighCache),
            reported_usage: Some(ReportedUsagePathPolicy {
                skip_non_stream_usage_projection: true,
                input: ReportedUsageFieldPolicy::sample_input_max(1),
                ..ReportedUsagePathPolicy::default()
            }),
            ..CacheRoutePolicyPatch::default()
        },
    );
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        true,
        Arc::new(UsageRecorder::new(10)),
        Arc::new(PromptCacheTracker::default()),
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::HighCache,
        0.98,
        CompatProfile::ClaudeCode,
        false,
    )
    .with_cache_policy(cache_policy);
    let runtime_config = RequestRuntimeConfig::from_app_state(&state);
    let cache_route = runtime_config.cache_policy_for_path("/cc/v1/messages");
    assert_eq!(
        cache_route.policy.cache_type,
        PromptCacheStrategyType::CurrentHighCache
    );
    assert!(
        cache_route
            .policy
            .reported_usage
            .skip_non_stream_usage_projection
    );

    let non_stream_route = raw_external_route_request(
            &state,
            &runtime_config,
            &cache_route,
            HeaderMap::new(),
            Bytes::from_static(
                br#"{"model":"client-model","stream":false,"messages":[{"role":"user","content":"hello"}]}"#,
            ),
            "/cc/v1/messages",
            "req_raw_non_stream_skip".to_string(),
            UsageRouteSubtype::ExternalDirectPolicy,
            None,
            Some("direct_policy".to_string()),
            None,
            Arc::new(InferenceAttemptBudget::new(4)),
            None,
        );

    assert_eq!(
        non_stream_route.prompt_cache_strategy_type,
        PromptCacheStrategyType::NoCache
    );
    assert_eq!(
        non_stream_route.prompt_cache_simulation_mode,
        PromptCacheSimulationMode::Disabled
    );
    assert!(non_stream_route.prompt_cache_route_namespace.is_none());
    assert!(!non_stream_route.reported_usage.default.enabled);
    assert!(
        non_stream_route
            .reported_usage
            .default
            .skip_non_stream_usage_projection
    );

    let stream_route = raw_external_route_request(
            &state,
            &runtime_config,
            &cache_route,
            HeaderMap::new(),
            Bytes::from_static(
                br#"{"model":"client-model","stream":true,"messages":[{"role":"user","content":"hello"}]}"#,
            ),
            "/cc/v1/messages",
            "req_raw_stream_skip".to_string(),
            UsageRouteSubtype::ExternalDirectPolicy,
            None,
            Some("direct_policy".to_string()),
            None,
            Arc::new(InferenceAttemptBudget::new(4)),
            None,
        );

    assert_eq!(
        stream_route.prompt_cache_strategy_type,
        PromptCacheStrategyType::CurrentHighCache
    );
    assert!(stream_route.reported_usage.default.enabled);
    assert!(
        stream_route
            .reported_usage
            .default
            .skip_non_stream_usage_projection
    );
}

fn runtime_config_for_payload_guard(
    mode: PayloadGuardMode,
    enabled: bool,
    max_bytes: usize,
) -> RequestRuntimeConfig {
    RequestRuntimeConfig {
        extract_thinking: true,
        thinking_trigger_mode: ThinkingTriggerMode::RealRequest,
        prompt_cache_simulation_mode: PromptCacheSimulationMode::HighCache,
        prompt_cache_target_read_ratio: 0.98,
        prompt_cache_token_scale: 1.0,
        prompt_cache_max_simulated_input_tokens: 0,
        prompt_cache_cap_jitter_min_tokens: 0,
        prompt_cache_cap_jitter_max_tokens: 0,
        prompt_cache_scale_min_input_tokens: 0,
        prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
        prompt_cache_bounds: PromptCacheBounds::default(),
        reported_usage: ReportedUsageConfig::default(),
        cache_policy: CachePolicyConfig::default(),
        defined_cache_routes: Vec::new(),
        compat_profile: CompatProfile::ClaudeCode,
        model_resolution_mode: ModelResolutionMode::Compatible,
        model_mapping: ModelMappingConfig::default(),
        expose_proxy_warnings: false,
        payload_guard_enabled: enabled,
        payload_guard_mode: mode,
        payload_guard_max_bytes: max_bytes,
        payload_guard_safety_margin_bytes: 0,
        payload_guard_trim_history: true,
        payload_guard_external_enabled: true,
        kiro_cache_point_enabled: false,
        kiro_cache_point_tools_only: true,
        kiro_cache_point_record_plan: true,
        kiro_upstream_stream_idle_timeout_secs: 180,
        kiro_upstream_stream_retry_enabled: true,
        kiro_upstream_stream_retry_max_attempts: 2,
        inference_upstream_max_attempts: 4,
        auxiliary_upstream_max_attempts: 2,
        kiro_upstream_stream_retry_on_idle_timeout: true,
        kiro_upstream_stream_retry_on_read_error: true,
        kiro_upstream_stream_retry_on_status_error: true,
        image_processing: ImageProcessingConfig::default(),
        body_conversion: BodyConversionConfig::default(),
        prompt_steering: PromptSteeringConfig::default(),
        missing_max_tokens: MissingMaxTokensConfig::default(),
        payload_shaping: PayloadShapingConfig::default(),
        external_pools: ExternalPoolsConfig::default(),
    }
}

fn thinking_signature_retry_kiro_fixture() -> KiroRequest {
    let mut request = serde_json::from_value::<KiroRequest>(json!({
        "conversationState": {
            "conversationId": "thinking-signature-handler-fixture",
            "history": [
                {
                    "assistantResponseMessage": {
                        "content": "signed-visible-content",
                        "toolUses": [{
                            "toolUseId": "tool-signed",
                            "name": "read",
                            "input": {"path": "README.md"}
                        }],
                        "reasoningContent": {
                            "reasoningText": {
                                "text": "private-signed-thought",
                                "signature": "private-signature"
                            }
                        }
                    }
                },
                {
                    "userInputMessage": {
                        "content": "continue after signed block",
                        "modelId": "claude-sonnet-4"
                    }
                },
                {
                    "assistantResponseMessage": {
                        "content": "redacted-visible-content",
                        "reasoningContent": {
                            "redactedContent": "cHJpdmF0ZS1yZWRhY3RlZA=="
                        }
                    }
                }
            ],
            "currentMessage": {
                "userInputMessage": {
                    "content": "current-visible-content",
                    "modelId": "claude-sonnet-4",
                    "userInputMessageContext": {
                        "tools": [{
                            "toolSpecification": {
                                "name": "read",
                                "description": "Read a file",
                                "inputSchema": {
                                    "json": {
                                        "type": "object",
                                        "properties": {"path": {"type": "string"}},
                                        "required": ["path"]
                                    }
                                }
                            }
                        }]
                    }
                }
            }
        },
        "additionalModelRequestFields": {
            "thinking": {"type": "adaptive"},
            "output_config": {"effort": "max"}
        }
    }))
    .expect("deserialize thinking signature retry handler fixture");
    request.tool_cache_point_insert_after = vec![0];
    request
}

fn count_serialized_cache_points(value: &serde_json::Value) -> usize {
    value
        .pointer("/conversationState/currentMessage/userInputMessage/userInputMessageContext/tools")
        .and_then(serde_json::Value::as_array)
        .map(|tools| {
            tools
                .iter()
                .filter(|tool| tool.get("cachePoint").is_some())
                .count()
        })
        .unwrap_or(0)
}

#[test]
fn thinking_signature_retry_body_removes_only_native_reasoning_five_rounds() {
    for round in 1..=5 {
        let request = thinking_signature_retry_kiro_fixture();
        let original_body = serialize_kiro_request(&request).expect("serialize original fixture");
        let original: serde_json::Value =
            serde_json::from_str(&original_body).expect("original fixture JSON");
        let retry_body = build_thinking_signature_retry_body(&request)
            .unwrap_or_else(|error| panic!("round {round}: {error}"));
        let retry: serde_json::Value =
            serde_json::from_str(&retry_body).expect("retry fixture JSON");

        assert!(
            original
                .pointer(
                    "/conversationState/history/0/assistantResponseMessage/reasoningContent/reasoningText/signature"
                )
                .is_some()
        );
        assert!(
            original
                .pointer(
                    "/conversationState/history/2/assistantResponseMessage/reasoningContent/redactedContent"
                )
                .is_some()
        );
        assert!(
            retry
                .pointer("/conversationState/history/0/assistantResponseMessage/reasoningContent")
                .is_none(),
            "round {round}: signed reasoning removed"
        );
        assert!(
            retry
                .pointer("/conversationState/history/2/assistantResponseMessage/reasoningContent")
                .is_none(),
            "round {round}: redacted reasoning removed"
        );
        for pointer in [
            "/conversationState/history/0/assistantResponseMessage/content",
            "/conversationState/history/0/assistantResponseMessage/toolUses",
            "/conversationState/history/1/userInputMessage/content",
            "/conversationState/history/2/assistantResponseMessage/content",
            "/conversationState/currentMessage/userInputMessage/content",
            "/additionalModelRequestFields",
        ] {
            assert_eq!(
                original.pointer(pointer),
                retry.pointer(pointer),
                "round {round}: non-reasoning field changed at {pointer}"
            );
        }
        assert_eq!(count_serialized_cache_points(&original), 1);
        assert_eq!(count_serialized_cache_points(&retry), 1);
        assert!(!retry_body.contains("private-signature"));
        assert!(!retry_body.contains("private-signed-thought"));
        assert!(!retry_body.contains("cHJpdmF0ZS1yZWRhY3RlZA=="));
        assert_eq!(
            serialize_kiro_request(&request).expect("reserialize original fixture"),
            original_body,
            "round {round}: lazy retry clone must not mutate original request"
        );
    }
}

#[test]
fn cache_point_then_signature_retry_never_reintroduces_cache_point_five_rounds() {
    for round in 1..=5 {
        let mut cache_retry_request = thinking_signature_retry_kiro_fixture();
        assert_eq!(cache_retry_request.clear_tool_cache_point_plan(), 1);
        let cache_retry_body =
            serialize_kiro_request(&cache_retry_request).expect("serialize cache retry fixture");
        let cache_retry: serde_json::Value =
            serde_json::from_str(&cache_retry_body).expect("cache retry JSON");
        assert_eq!(count_serialized_cache_points(&cache_retry), 0);
        assert!(
            cache_retry_request
                .conversation_state
                .has_history_reasoning_content()
        );

        let signature_retry_body = build_thinking_signature_retry_body(&cache_retry_request)
            .unwrap_or_else(|error| panic!("round {round}: {error}"));
        let signature_retry: serde_json::Value =
            serde_json::from_str(&signature_retry_body).expect("signature retry JSON");
        assert_eq!(
            count_serialized_cache_points(&signature_retry),
            0,
            "round {round}: signature retry must preserve cleared cachePoint plan"
        );
        assert!(
            signature_retry
                .pointer("/conversationState/history/0/assistantResponseMessage/reasoningContent")
                .is_none()
        );
        assert_eq!(
            cache_retry.pointer(
                "/conversationState/currentMessage/userInputMessage/userInputMessageContext/tools/0/toolSpecification"
            ),
            signature_retry.pointer(
                "/conversationState/currentMessage/userInputMessage/userInputMessageContext/tools/0/toolSpecification"
            )
        );
    }
}

#[test]
fn payload_guard_then_signature_retry_preserves_actual_trimmed_history_five_rounds() {
    for round in 1..=5 {
        let old_marker = format!("OLD_HISTORY_MUST_STAY_TRIMMED_{round}_{}", "x".repeat(4096));
        let mut request = serde_json::from_value::<KiroRequest>(json!({
            "conversationState": {
                "conversationId": format!("guard-signature-{round}"),
                "history": [
                    {
                        "userInputMessage": {
                            "content": old_marker,
                            "modelId": "claude-sonnet-4"
                        }
                    },
                    {
                        "assistantResponseMessage": {
                            "content": format!("old-answer-{}", "y".repeat(4096))
                        }
                    },
                    {
                        "userInputMessage": {
                            "content": "RECENT_USER_MUST_REMAIN",
                            "modelId": "claude-sonnet-4"
                        }
                    },
                    {
                        "assistantResponseMessage": {
                            "content": "RECENT_ASSISTANT_MUST_REMAIN",
                            "reasoningContent": {
                                "reasoningText": {
                                    "text": "recent-private-thought",
                                    "signature": "recent-private-signature"
                                }
                            }
                        }
                    }
                ],
                "currentMessage": {
                    "userInputMessage": {
                        "content": "CURRENT_MUST_REMAIN",
                        "modelId": "claude-sonnet-4",
                        "userInputMessageContext": {}
                    }
                }
            }
        }))
        .expect("deserialize payload guard signature fixture");
        let mut expected_trimmed = request.clone();
        expected_trimmed.conversation_state.history.drain(0..2);
        let expected_trimmed_len = serialize_kiro_request(&expected_trimmed)
            .expect("serialize expected trimmed fixture")
            .len();
        let mut shaping = PayloadShapingConfig::default();
        shaping.enabled = false;
        let (guarded_body, report) = guard_kiro_request(
            &mut request,
            PayloadGuardConfig {
                enabled: true,
                max_bytes: expected_trimmed_len,
                trim_history: true,
                shaping,
            },
        )
        .unwrap_or_else(|error| panic!("round {round}: {error}"));

        assert_eq!(report.trimmed_history_entries, 2, "round {round}");
        assert_eq!(request.conversation_state.history.len(), 2, "round {round}");
        assert!(!guarded_body.contains("OLD_HISTORY_MUST_STAY_TRIMMED"));
        assert!(guarded_body.contains("RECENT_USER_MUST_REMAIN"));
        assert!(guarded_body.contains("RECENT_ASSISTANT_MUST_REMAIN"));
        assert!(request.conversation_state.has_history_reasoning_content());

        let signature_retry_body = build_thinking_signature_retry_body(&request)
            .unwrap_or_else(|error| panic!("round {round}: {error}"));
        assert!(!signature_retry_body.contains("OLD_HISTORY_MUST_STAY_TRIMMED"));
        assert!(!signature_retry_body.contains("recent-private-thought"));
        assert!(!signature_retry_body.contains("recent-private-signature"));
        assert!(signature_retry_body.contains("RECENT_USER_MUST_REMAIN"));
        assert!(signature_retry_body.contains("RECENT_ASSISTANT_MUST_REMAIN"));
        assert!(signature_retry_body.contains("CURRENT_MUST_REMAIN"));
    }
}

#[test]
fn thinking_signature_typed_failures_never_enter_external_fallback_five_rounds() {
    let mut config = ExternalPoolsConfig::default();
    config.fallback_on_local_capacity_exhausted = true;
    config.fallback_on_no_available_credentials = true;
    config.fallback_on_local_transient_exhausted = true;
    config.fallback_on_scheduler_redis_degraded = true;
    config.fallback_on_unsupported_model = true;
    for round in 1..=5 {
        for kind in [
            KiroCallFailureKind::ThinkingSignatureInvalid,
            KiroCallFailureKind::ThinkingSignatureRetryFailed,
        ] {
            for misleading_message in [
                "429 rate limit capacity exhausted",
                "500 timeout network error",
                "all credentials disabled; scheduler Redis degraded",
                "invalid model not available",
            ] {
                assert_eq!(
                    classify_local_error_for_external_fallback_with_kind(
                        misleading_message,
                        &[],
                        &config,
                        Some(kind),
                    ),
                    None,
                    "round {round}: typed signature failure {kind:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn thinking_signature_typed_failures_map_to_stable_public_errors_five_rounds() {
    for round in 1..=5 {
        for (kind, expected_status, expected_type, expected_message) in [
            (
                KiroCallFailureKind::ThinkingSignatureInvalid,
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                UPSTREAM_INVALID_REQUEST_MESSAGE,
            ),
            (
                KiroCallFailureKind::ThinkingSignatureRetryFailed,
                StatusCode::BAD_GATEWAY,
                "api_error",
                envelope::PUBLIC_TEMPORARY_FAILURE_MESSAGE,
            ),
        ] {
            let private_marker = format!("PRIVATE_SIGNATURE_HANDLER_ERROR_{round}_{kind:?}");
            let error: anyhow::Error =
                crate::kiro::call_trace::KiroCallError::new(private_marker.clone(), Vec::new())
                    .with_failure_kind(kind)
                    .into();
            let response = map_provider_error(
                error,
                Some("req_signature_handler"),
                Some("err_signature_handler"),
                None,
            );
            assert_eq!(response.status(), expected_status);
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("read signature public error body");
            let value: serde_json::Value =
                serde_json::from_slice(&body).expect("signature public error JSON");
            assert_eq!(value["error"]["type"], expected_type);
            assert_eq!(
                value["error"]["message"],
                envelope::public_message_with_error_id(expected_message, "err_signature_handler")
            );
            assert!(!String::from_utf8_lossy(&body).contains(&private_marker));
        }
    }
}

#[test]
fn signature_error_token_cannot_trigger_cache_point_retry_five_rounds() {
    for round in 1..=5 {
        for value in [
            "upstream_failure reason=THINKING_SIGNATURE_INVALID",
            "400 Bad Request reason=THINKING_SIGNATURE_INVALID Invalid tool use format",
            "bad_request improperly formed reason=THINKING_SIGNATURE_INVALID",
        ] {
            assert!(
                !should_retry_without_cache_point_after_error(value),
                "round {round}: signature retry terminal result must not stack cachePoint retry"
            );
        }
    }
}

#[tokio::test]
async fn auxiliary_focus_attempt_limits_map_to_public_temporary_failure_without_internal_terms() {
    for _ in 0..5 {
        for failure_kind in [
            KiroCallFailureKind::InferenceAttemptsExhausted,
            KiroCallFailureKind::InferenceAttemptReservedForFallback,
            KiroCallFailureKind::DownstreamCommitted,
            KiroCallFailureKind::AuxiliaryAttemptsExhausted,
            KiroCallFailureKind::AuxiliaryConcurrencySaturated,
            KiroCallFailureKind::LocalPoolRiskCircuitOpen,
        ] {
            let error_message = if failure_kind == KiroCallFailureKind::LocalPoolRiskCircuitOpen {
                "本地账号池风险保护已打开（retry_after_secs=7）"
            } else {
                "local inference attempt reserved for fallback"
            };
            let err: anyhow::Error =
                crate::kiro::call_trace::KiroCallError::new(error_message, Vec::new())
                    .with_failure_kind(failure_kind)
                    .into();
            let response = map_provider_error(
                err,
                Some("req_attempt_limit_test"),
                Some("req_attempt_limit_error"),
                None,
            );
            let expected_status = if failure_kind == KiroCallFailureKind::DownstreamCommitted {
                StatusCode::BAD_GATEWAY
            } else {
                StatusCode::SERVICE_UNAVAILABLE
            };
            assert_eq!(response.status(), expected_status);
            if failure_kind == KiroCallFailureKind::LocalPoolRiskCircuitOpen {
                assert_eq!(
                    response
                        .headers()
                        .get("retry-after")
                        .and_then(|value| value.to_str().ok()),
                    Some("7")
                );
            }
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("read public error response");
            let body = String::from_utf8(body.to_vec()).expect("utf-8 public error response");
            assert!(body.contains(envelope::PUBLIC_TEMPORARY_FAILURE_MESSAGE));
            assert!(body.contains("req_attempt_limit_error"));
            for internal in ["budget", "upstream", "pool", "credential", "reserved"] {
                assert!(!body.to_ascii_lowercase().contains(internal));
            }
        }
    }
}

#[test]
fn fallback_reservation_has_a_dedicated_internal_classification() {
    let reason = classify_local_error_for_external_fallback(
        "local inference attempt reserved for fallback",
        &[],
        &ExternalPoolsConfig::default(),
    );
    assert_eq!(
        reason.as_deref(),
        Some("local_attempt_reserved_for_fallback")
    );
}

#[test]
fn auxiliary_focus_typed_failures_use_local_transient_fallback_policy_for_five_rounds() {
    for _ in 0..5 {
        let mut config = ExternalPoolsConfig::default();
        config.fallback_on_local_transient_exhausted = true;
        for (kind, expected) in [
            (
                KiroCallFailureKind::AuxiliaryAttemptsExhausted,
                "local_auxiliary_attempts_exhausted",
            ),
            (
                KiroCallFailureKind::AuxiliaryConcurrencySaturated,
                "local_auxiliary_concurrency_saturated",
            ),
        ] {
            let reason = classify_local_error_for_external_fallback_with_kind(
                "misleading deterministic request body error",
                &[],
                &config,
                Some(kind),
            );
            assert_eq!(reason.as_deref(), Some(expected));

            config.fallback_on_local_transient_exhausted = false;
            assert_eq!(
                classify_local_error_for_external_fallback_with_kind(
                    "misleading 503 transient text",
                    &[],
                    &config,
                    Some(kind),
                ),
                None
            );
            config.fallback_on_local_transient_exhausted = true;
        }
    }
}

#[test]
fn on_too_long_initial_guard_repairs_without_size_trimming() {
    let runtime_config =
        runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);

    let initial = runtime_config.initial_payload_guard_config();

    assert!(initial.enabled);
    assert_eq!(initial.max_bytes, 0);
    assert!(!initial.trim_history);
    assert!(runtime_config.too_long_retry_enabled());
    assert_eq!(runtime_config.payload_guard_config().max_bytes, 460_800);
    assert!(runtime_config.payload_guard_config().trim_history);
}

#[test]
fn payload_guard_safety_margin_reduces_effective_size_target() {
    let mut runtime_config =
        runtime_config_for_payload_guard(PayloadGuardMode::Preemptive, true, 460_800);
    runtime_config.payload_guard_safety_margin_bytes = 32 * 1024;

    assert_eq!(runtime_config.payload_guard_config().max_bytes, 428_032);

    runtime_config.payload_guard_max_bytes = 0;
    assert_eq!(runtime_config.payload_guard_config().max_bytes, 0);
}

#[test]
fn on_too_long_retry_requires_enabled_guard_and_positive_limit() {
    assert!(
        !runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, false, 460_800)
            .too_long_retry_enabled()
    );
    assert!(
        !runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 0)
            .too_long_retry_enabled()
    );
    assert!(
        !runtime_config_for_payload_guard(PayloadGuardMode::Preemptive, true, 460_800)
            .too_long_retry_enabled()
    );
}

#[test]
fn payload_guard_retry_treats_large_improper_request_as_possible_size_error() {
    assert!(should_retry_payload_guard_after_error(
        r#"400 Bad Request {"message":"Input is too long.","reason":"CONTENT_LENGTH_EXCEEDS_THRESHOLD"}"#,
        100,
        460_800,
    ));
    assert!(should_retry_payload_guard_after_error(
        r#"400 Bad Request {"message":"Improperly formed request.","reason":null}"#,
        700_000,
        460_800,
    ));
    assert!(!should_retry_payload_guard_after_error(
        r#"400 Bad Request {"message":"Improperly formed request.","reason":null}"#,
        100_000,
        460_800,
    ));
    assert!(!should_retry_payload_guard_after_error(
        r#"400 Bad Request {"message":"Improperly formed request.","reason":null}"#,
        700_000,
        0,
    ));
}

#[test]
fn request_body_invalid_tool_format_is_bad_request_diagnostic_error() {
    let message =
        r#"400 Bad Request {"message":"Invalid tool use format.","reason":"REQUEST_BODY_INVALID"}"#;

    assert!(is_upstream_bad_request_error(message));
    assert!(is_upstream_tool_use_format_error(message));
    assert!(!should_retry_payload_guard_after_error(
        message, 100_000, 460_800,
    ));
}

#[test]
fn thinking_suffix_opus_4_7_uses_adaptive_by_default() {
    let mut payload = messages_request_for_model("claude-opus-4-7-thinking");

    override_thinking_from_model_name(&mut payload).expect("thinking model override");

    let thinking = payload.thinking.expect("thinking should be set");
    assert_eq!(thinking.thinking_type, "adaptive");
    assert!(payload.output_config.is_none());
}

#[test]
fn thinking_suffix_opus_alias_uses_adaptive_by_default() {
    let mut payload = messages_request_for_model("opus-thinking");

    override_thinking_from_model_name(&mut payload).expect("thinking model override");

    let thinking = payload.thinking.expect("thinking should be set");
    assert_eq!(thinking.thinking_type, "adaptive");
    assert!(payload.output_config.is_none());
}

#[test]
fn thinking_suffix_sonnet_4_6_uses_adaptive_by_default() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6-thinking");

    override_thinking_from_model_name(&mut payload).expect("thinking model override");

    let thinking = payload.thinking.expect("thinking should be set");
    assert_eq!(thinking.thinking_type, "adaptive");
    assert!(payload.output_config.is_none());
}

#[test]
fn thinking_suffix_sonnet_alias_uses_adaptive_by_default() {
    let mut payload = messages_request_for_model("sonnet-thinking");

    override_thinking_from_model_name(&mut payload).expect("thinking model override");

    let thinking = payload.thinking.expect("thinking should be set");
    assert_eq!(thinking.thinking_type, "adaptive");
    assert!(payload.output_config.is_none());
}

#[test]
fn thinking_suffix_sonnet_4_5_uses_enabled_by_default() {
    let mut payload = messages_request_for_model("claude-sonnet-4-5-thinking");
    payload.max_tokens = 32_768;

    override_thinking_from_model_name(&mut payload).expect("thinking model override");

    let thinking = payload.thinking.expect("thinking should be set");
    assert_eq!(thinking.thinking_type, "enabled");
    assert_eq!(thinking.budget_tokens, 20000);
    assert!(payload.output_config.is_none());
}

#[test]
fn thinking_suffix_sonnet_4_5_rejects_when_minimum_budget_cannot_fit() {
    for max_tokens in [1, 1_024] {
        let mut payload = messages_request_for_model("claude-sonnet-4-5-thinking");
        payload.max_tokens = max_tokens;

        let error = override_thinking_from_model_name(&mut payload)
            .expect_err("enabled thinking requires room above the minimum budget");
        assert!(
            error.contains("greater than 1024"),
            "max_tokens={max_tokens}"
        );
        assert!(payload.thinking.is_none(), "max_tokens={max_tokens}");
    }

    let mut boundary = messages_request_for_model("claude-sonnet-4-5-thinking");
    boundary.max_tokens = 1_025;
    override_thinking_from_model_name(&mut boundary).expect("minimum thinking budget fits");
    assert_eq!(
        boundary.thinking.expect("boundary thinking").budget_tokens,
        1_024
    );
}

#[test]
fn thinking_suffix_preserves_explicit_enabled_without_effort() {
    let mut payload = messages_request_for_model("claude-opus-4-7-thinking");
    payload.thinking = Some(Thinking {
        thinking_type: "enabled".to_string(),
        budget_tokens: 4096,
    });

    override_thinking_from_model_name(&mut payload).expect("thinking model override");

    let thinking = payload.thinking.expect("thinking should be preserved");
    assert_eq!(thinking.thinking_type, "enabled");
    assert_eq!(thinking.budget_tokens, 4096);
    assert!(payload.output_config.is_none());
}

#[test]
fn thinking_suffix_preserves_omitted_effort_for_explicit_adaptive() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6-thinking");
    payload.thinking = Some(Thinking {
        thinking_type: "adaptive".to_string(),
        budget_tokens: 4096,
    });

    override_thinking_from_model_name(&mut payload).expect("thinking model override");

    let thinking = payload.thinking.expect("thinking should be preserved");
    assert_eq!(thinking.thinking_type, "adaptive");
    assert_eq!(thinking.budget_tokens, 4096);
    assert!(payload.output_config.is_none());
}

#[test]
fn thinking_trigger_real_request_preserves_empty_payload() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    let runtime_config =
        runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);

    apply_thinking_trigger_mode(&mut payload, &runtime_config);

    assert!(payload.thinking.is_none());
    assert!(payload.output_config.is_none());
    assert!(!should_force_visible_thinking(&payload, &runtime_config));
}

#[test]
fn disabled_prompt_master_suppresses_automatic_thinking_additions() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6-thinking");
    payload.messages[0].content = json!("ultrathink analyze this issue");
    let mut runtime_config =
        runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);
    runtime_config.prompt_steering.enabled = false;
    runtime_config.thinking_trigger_mode = ThinkingTriggerMode::Always;

    apply_thinking_trigger_mode(&mut payload, &runtime_config);

    assert!(
        payload.thinking.is_none(),
        "master-off must not synthesize thinking from model suffix or prompt signal"
    );
    assert!(!should_force_visible_thinking(&payload, &runtime_config));
}

#[test]
fn thinking_trigger_real_request_uses_claude_code_ultrathink_signal() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    payload.messages[0].content = json!("ultrathink analyze this issue");
    let runtime_config =
        runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);

    apply_thinking_trigger_mode(&mut payload, &runtime_config);

    let thinking = payload
        .thinking
        .as_ref()
        .expect("ultrathink should inject adaptive thinking");
    assert_eq!(thinking.thinking_type, "adaptive");
    assert_eq!(thinking.budget_tokens, 0);
    assert!(payload.output_config.is_none());
    assert!(should_force_visible_thinking(&payload, &runtime_config));
}

#[test]
fn thinking_trigger_real_request_uses_claude_code_deep_reasoning_wrapper() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    payload.messages[0].content = json!(
        r#"<system-reminder>
The user included the keyword "ultrathink", requesting deeper reasoning on this turn. Reason as thoroughly as the task warrants.
</system-reminder>

Return a fix plan."#
    );
    let runtime_config =
        runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);

    apply_thinking_trigger_mode(&mut payload, &runtime_config);

    assert_eq!(
        payload
            .thinking
            .as_ref()
            .expect("Claude Code wrapper should inject thinking")
            .thinking_type,
        "adaptive"
    );
    assert!(should_force_visible_thinking(&payload, &runtime_config));
}

#[test]
fn thinking_trigger_real_request_does_not_treat_think_hard_as_cli_keyword() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    payload.messages[0].content = json!("think hard about this issue");
    let runtime_config =
        runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);

    apply_thinking_trigger_mode(&mut payload, &runtime_config);

    assert!(payload.thinking.is_none());
    assert!(payload.output_config.is_none());
    assert!(!should_force_visible_thinking(&payload, &runtime_config));
}

#[test]
fn thinking_trigger_real_request_respects_explicit_disabled_over_cli_signal() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    payload.messages[0].content = json!("ultrathink analyze this issue");
    payload.thinking = Some(Thinking {
        thinking_type: "disabled".to_string(),
        budget_tokens: 4096,
    });
    let runtime_config =
        runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);

    apply_thinking_trigger_mode(&mut payload, &runtime_config);

    let thinking = payload
        .thinking
        .as_ref()
        .expect("disabled thinking should be preserved");
    assert_eq!(thinking.thinking_type, "disabled");
    assert_eq!(thinking.budget_tokens, 4096);
    assert!(payload.output_config.is_none());
    assert!(!should_force_visible_thinking(&payload, &runtime_config));
}

#[test]
fn thinking_trigger_real_request_ignores_old_user_signal_after_new_turn() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    payload.messages = vec![
        Message {
            role: "user".to_string(),
            content: json!("ultrathink analyze the old turn"),
        },
        Message {
            role: "assistant".to_string(),
            content: json!([{ "type": "text", "text": "done" }]),
        },
        Message {
            role: "user".to_string(),
            content: json!("new plain turn"),
        },
    ];
    let runtime_config =
        runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);

    apply_thinking_trigger_mode(&mut payload, &runtime_config);

    assert!(payload.thinking.is_none());
    assert!(payload.output_config.is_none());
    assert!(!should_force_visible_thinking(&payload, &runtime_config));
}

#[test]
fn thinking_trigger_real_request_keeps_signal_across_tool_result_continuation() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    payload.messages = vec![
        Message {
            role: "user".to_string(),
            content: json!("ultrathink inspect the file then answer"),
        },
        Message {
            role: "assistant".to_string(),
            content: json!([{
                "type": "tool_use",
                "id": "toolu_1",
                "name": "Read",
                "input": {"file_path": "src/main.rs"}
            }]),
        },
        Message {
            role: "user".to_string(),
            content: json!([{
                "type": "tool_result",
                "tool_use_id": "toolu_1",
                "content": "file contents"
            }]),
        },
    ];
    let runtime_config =
        runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);

    apply_thinking_trigger_mode(&mut payload, &runtime_config);

    assert_eq!(
        payload
            .thinking
            .as_ref()
            .expect("current turn signal should survive tool-result continuation")
            .thinking_type,
        "adaptive"
    );
    assert!(should_force_visible_thinking(&payload, &runtime_config));
}

#[test]
fn thinking_trigger_always_adds_adaptive_without_forging_explicit_effort() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    let mut runtime_config =
        runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);
    runtime_config.thinking_trigger_mode = ThinkingTriggerMode::Always;

    apply_thinking_trigger_mode(&mut payload, &runtime_config);

    let thinking = payload
        .thinking
        .as_ref()
        .expect("thinking should be injected");
    assert_eq!(thinking.thinking_type, "adaptive");
    assert_eq!(thinking.budget_tokens, 0);
    assert!(payload.output_config.is_none());
    assert!(should_force_visible_thinking(&payload, &runtime_config));
}

#[test]
fn synthetic_thinking_activation_never_forges_client_effort_five_rounds() {
    for round in 0..5 {
        for model in [
            "claude-opus-4-7-thinking",
            "claude-opus-4-8-thinking",
            "claude-sonnet-4-6-thinking",
        ] {
            let mut payload = messages_request_for_model(model);
            override_thinking_from_model_name(&mut payload)
                .unwrap_or_else(|error| panic!("round {round}: {model}: {error}"));
            assert_eq!(
                payload
                    .thinking
                    .as_ref()
                    .map(|thinking| thinking.thinking_type.as_str()),
                Some("adaptive")
            );
            assert!(
                payload.output_config.is_none(),
                "round {round}: {model} omitted effort must remain omitted"
            );
        }

        let mut triggered = messages_request_for_model("claude-sonnet-4-6");
        triggered.messages[0].content = json!("ultrathink inspect this");
        let runtime_config =
            runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);
        apply_thinking_trigger_mode(&mut triggered, &runtime_config);
        assert_eq!(
            triggered
                .thinking
                .as_ref()
                .map(|thinking| thinking.thinking_type.as_str()),
            Some("adaptive")
        );
        assert!(triggered.output_config.is_none(), "round {round}");

        let mut explicit = messages_request_for_model("claude-opus-4-8-thinking");
        explicit.output_config = Some(OutputConfig {
            effort: Some("max".to_string()),
        });
        override_thinking_from_model_name(&mut explicit).expect("explicit effort override");
        assert_eq!(
            explicit
                .output_config
                .expect("explicit effort preserved")
                .effort
                .as_deref(),
            Some("max"),
            "round {round}"
        );
    }
}

#[test]
fn thinking_trigger_always_preserves_disabled() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    payload.thinking = Some(Thinking {
        thinking_type: "disabled".to_string(),
        budget_tokens: 4096,
    });
    let mut runtime_config =
        runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);
    runtime_config.thinking_trigger_mode = ThinkingTriggerMode::Always;

    apply_thinking_trigger_mode(&mut payload, &runtime_config);

    let thinking = payload
        .thinking
        .as_ref()
        .expect("thinking should be preserved");
    assert_eq!(thinking.thinking_type, "disabled");
    assert_eq!(thinking.budget_tokens, 4096);
    assert!(payload.output_config.is_none());
    assert!(!should_force_visible_thinking(&payload, &runtime_config));
}

#[test]
fn thinking_trigger_always_preserves_enabled_and_output_config() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    payload.thinking = Some(Thinking {
        thinking_type: "enabled".to_string(),
        budget_tokens: 4096,
    });
    payload.output_config = Some(OutputConfig {
        effort: Some("low".to_string()),
    });
    let mut runtime_config =
        runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);
    runtime_config.thinking_trigger_mode = ThinkingTriggerMode::Always;

    apply_thinking_trigger_mode(&mut payload, &runtime_config);

    let thinking = payload
        .thinking
        .as_ref()
        .expect("thinking should be preserved");
    assert_eq!(thinking.thinking_type, "enabled");
    assert_eq!(thinking.budget_tokens, 4096);
    assert_eq!(
        payload
            .output_config
            .as_ref()
            .expect("output_config should be preserved")
            .effort
            .as_deref(),
        Some("low")
    );
    assert!(should_force_visible_thinking(&payload, &runtime_config));
}

#[test]
fn thinking_trigger_always_preserves_unknown_type_without_implicit_activation() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    payload.thinking = Some(Thinking {
        thinking_type: "mystery".to_string(),
        budget_tokens: 4096,
    });
    let mut runtime_config =
        runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);
    runtime_config.thinking_trigger_mode = ThinkingTriggerMode::Always;

    apply_thinking_trigger_mode(&mut payload, &runtime_config);

    let thinking = payload
        .thinking
        .as_ref()
        .expect("thinking should be preserved for the entry validator to reject");
    assert_eq!(thinking.thinking_type, "mystery");
    assert_eq!(thinking.budget_tokens, 4096);
    assert!(payload.output_config.is_none());
    assert!(!should_force_visible_thinking(&payload, &runtime_config));
}

#[test]
fn preflight_ready_acquire_full_race_uses_bounded_local_wait_for_five_rounds() {
    let mut config = ExternalPoolsConfig::default();
    config.external_pools_enabled = true;
    config.local_pool_preflight_enabled = true;
    config.fallback_on_local_capacity_exhausted = true;
    config.external_pool_dispatch_max_wait_secs = 7;

    for round in 1..=5 {
        assert_eq!(
            local_pool_acquire_mode(&config),
            AcquireMode::FailFastOnCapacityWaitForRedis(Duration::from_secs(7)),
            "round {round}: external eligibility was already established, so local capacity races still fail fast for reselect/fallback, while Redis degraded uses the external fallback's bounded wait window"
        );

        config.fallback_on_local_capacity_exhausted = false;
        assert_eq!(
            local_pool_acquire_mode(&config),
            AcquireMode::WaitForCapacity,
            "round {round}: the capacity fallback toggle remains authoritative"
        );
        config.fallback_on_local_capacity_exhausted = true;

        config.local_pool_preflight_enabled = false;
        assert_eq!(
            local_pool_acquire_mode(&config),
            AcquireMode::WaitForCapacity,
            "round {round}: the operator preflight switch remains authoritative"
        );
        config.local_pool_preflight_enabled = true;
    }
}

#[test]
fn local_acquire_mode_is_clamped_to_shared_request_deadline() {
    let budget = InferenceAttemptBudget::new(4);
    budget.set_dispatch_deadline_after(Duration::from_millis(25));
    let mode = clamp_acquire_mode_to_dispatch_deadline(
        AcquireMode::WaitForCapacityMax(Duration::from_secs(7)),
        &budget,
    );
    match mode {
        AcquireMode::WaitForCapacityMax(wait) => {
            assert!(wait <= Duration::from_millis(25));
            assert!(wait < Duration::from_secs(7));
        }
        other => panic!("unexpected acquire mode: {other:?}"),
    }
}

#[test]
fn path_reported_usage_policy_samples_natural_usage() {
    let reported_usage_config = ReportedUsageConfig::default();
    let usage = CacheUsage {
        total_input_tokens: 100_000,
        input_tokens: 50_000,
        output_tokens: 1,
        cache_creation_input_tokens: 50_000,
        cache_read_input_tokens: 0,
        cache_creation_5m_input_tokens: 50_000,
        cache_creation_1h_input_tokens: 0,
    };
    let values: Vec<i32> = (0..24)
        .map(|seed| {
            let policy = reported_cache_usage_policy_for_path(
                "/cc/v1/messages",
                PromptCacheSimulationMode::HighCache,
                &reported_usage_config,
                seed,
            )
            .expect("policy should apply");
            usage
                .with_reported_cache_usage_policy(policy)
                .cache_creation_input_tokens
        })
        .collect();

    assert!(values.iter().all(|value| (49_905..=53_599).contains(value)));
    assert!(values.windows(2).any(|pair| pair[1] < pair[0]));
    assert!(values.iter().any(|value| value % 10 != 0));

    let reported = usage.with_reported_cache_usage_policy(
        reported_cache_usage_policy_for_path(
            "/cc/v1/messages",
            PromptCacheSimulationMode::HighCache,
            &reported_usage_config,
            9,
        )
        .expect("policy should apply"),
    );
    assert!((1..=96).contains(&reported.input_tokens));
    assert_eq!(reported.cache_read_input_tokens, 0);
    assert!(
        reported.cache_creation_input_tokens
            >= usage.input_tokens.saturating_sub(reported.input_tokens)
    );
    assert_eq!(reported.output_tokens, 1);

    let raw_reported = usage.with_reported_cache_usage_policy_and_raw(
        reported_cache_usage_policy_for_path(
            "/cc/v1/messages",
            PromptCacheSimulationMode::HighCache,
            &reported_usage_config,
            9,
        )
        .expect("policy should apply"),
        cache::RawUsage::uncached(100_000, 1),
    );
    assert!((1..=96).contains(&raw_reported.input_tokens));
    assert_eq!(raw_reported.cache_read_input_tokens, 0);
    assert!(
        raw_reported.cache_creation_input_tokens
            >= 100_000_i32.saturating_sub(raw_reported.input_tokens)
    );
}

#[test]
fn path_reported_usage_skip_non_stream_blocks_non_stream_only() {
    let policy = ReportedUsagePathPolicy {
        skip_non_stream_usage_projection: true,
        input: ReportedUsageFieldPolicy::sample_input_max(96),
        ..ReportedUsagePathPolicy::default()
    };

    let non_stream_policy = reported_cache_usage_policy_for_request(
        PromptCacheStrategyType::CurrentHighCache,
        PromptCacheSimulationMode::HighCache,
        &policy,
        7,
        false,
    );
    assert!(non_stream_policy.is_none());

    let stream_policy = reported_cache_usage_policy_for_request(
        PromptCacheStrategyType::CurrentHighCache,
        PromptCacheSimulationMode::HighCache,
        &policy,
        7,
        true,
    );
    assert!(stream_policy.is_some());
}

#[test]
fn path_reported_usage_skip_non_stream_disables_local_cache_route_only_for_non_stream() {
    let prompt_cache = Arc::new(PromptCacheTracker::default());
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let mut cache_policy = CachePolicyConfig::default();
    cache_policy.path_overrides.insert(
        "/kiro/v1/messages".to_string(),
        CacheRoutePolicyPatch {
            cache_type: Some(PromptCacheStrategyType::KiroRsTool),
            reported_usage: Some(ReportedUsagePathPolicy {
                skip_non_stream_usage_projection: true,
                ..ReportedUsagePathPolicy::default()
            }),
            ..CacheRoutePolicyPatch::default()
        },
    );
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        true,
        usage_recorder,
        prompt_cache,
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::Disabled,
        0.95,
        CompatProfile::ClaudeCode,
        false,
    )
    .with_cache_policy(cache_policy);

    let route =
        RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/kiro/v1/messages");
    assert_eq!(route.policy.cache_type, PromptCacheStrategyType::KiroRsTool);
    assert!(route.policy.reported_usage.skip_non_stream_usage_projection);

    let stream_route = cache_route_for_request_stream(route.clone(), true);
    assert_eq!(
        stream_route.policy.cache_type,
        PromptCacheStrategyType::KiroRsTool
    );
    assert!(
        stream_route
            .policy
            .reported_usage
            .skip_non_stream_usage_projection
    );

    let non_stream_route = cache_route_for_request_stream(route, false);
    assert_eq!(
        non_stream_route.policy.cache_type,
        PromptCacheStrategyType::NoCache
    );
    assert!(non_stream_route.namespace.is_none());
    assert!(!non_stream_route.policy.simulation.enabled);
    assert!(!non_stream_route.policy.creation_control.enabled);
    assert!(!non_stream_route.policy.cache_point.enabled);
    assert!(!non_stream_route.policy.reported_usage.enabled);
    assert!(
        non_stream_route
            .policy
            .reported_usage
            .skip_non_stream_usage_projection
    );
}

#[test]
fn reported_usage_rewrite_shapes_high_cache_downstream_usage() {
    let reported_usage_config = ReportedUsageConfig::default();
    let v1_policy = reported_cache_usage_policy_for_path(
        "/v1/messages",
        PromptCacheSimulationMode::HighCache,
        &reported_usage_config,
        0,
    )
    .expect("default policy should apply");
    let unchanged_usage = CacheUsage {
        total_input_tokens: 100_000,
        input_tokens: 10_000,
        output_tokens: 1,
        cache_creation_input_tokens: 50_000,
        cache_read_input_tokens: 40_000,
        cache_creation_5m_input_tokens: 50_000,
        cache_creation_1h_input_tokens: 0,
    };
    let v1_reported = unchanged_usage
        .with_reported_cache_usage_policy_and_raw(v1_policy, cache::RawUsage::uncached(100_000, 1));
    assert_eq!(v1_reported.input_tokens, 100_000);
    assert_eq!(v1_reported.output_tokens, 1);
    assert_eq!(
        v1_reported.cache_creation_input_tokens,
        unchanged_usage.cache_creation_input_tokens
    );
    assert_eq!(
        v1_reported.cache_read_input_tokens,
        unchanged_usage.cache_read_input_tokens
    );
    assert_eq!(
        reported_cache_usage_policy_for_path(
            "/cc/v1/messages",
            PromptCacheSimulationMode::Disabled,
            &reported_usage_config,
            0,
        ),
        None
    );

    let prompt_cache = Arc::new(PromptCacheTracker::default());
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let usage_context = RequestUsageContext {
        recorder: usage_recorder.clone(),
        tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
        prompt_cache,
        prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
        pricing_catalog: Arc::new(PricingCatalog::new()),
        request_id: "req_reported_limit".to_string(),
        error_id: "req_01reported_limit".to_string(),
        endpoint: "/cc/v1/messages".to_string(),
        stream: false,
        model: "claude-sonnet-4-6".to_string(),
        upstream_model: None,
        model_resolution_source: None,
        model_resolution_note: None,
        requested_max_tokens: 0,
        downstream_stop_reason: Arc::new(Mutex::new(None)),
        conversation_id: Some("session-limit".to_string()),
        request_api_key_id: None,
        prompt_cache_scope_conversation_id: Some("session-limit".to_string()),
        input_tokens: 100_000,
        context_window_tokens: 200_000,
        prompt_cache_profile: None,
        kiro_rs_tool_prompt_cache_plan: None,
        prompt_cache_route_namespace: None,
        prompt_cache_strategy_type: PromptCacheStrategyType::CurrentHighCache,
        simulation_mode: PromptCacheSimulationMode::HighCache,
        prompt_cache_target_read_ratio: 0.95,
        prompt_cache_token_scale: 1.0,
        prompt_cache_max_simulated_input_tokens: 0,
        prompt_cache_cap_jitter_min_tokens: 0,
        prompt_cache_cap_jitter_max_tokens: 0,
        prompt_cache_scale_min_input_tokens: 0,
        prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
        prompt_cache_bounds: PromptCacheBounds::default(),
        reported_cache_usage_policy: reported_cache_usage_policy_for_path(
            "/cc/v1/messages",
            PromptCacheSimulationMode::HighCache,
            &reported_usage_config,
            7,
        ),
        simulated_usage: None,
        simulated_source: Some(UsageSource::LocalPromptCache),
        payload_breakdown: None,
        payload_guard_report: None,
        error_metadata: Arc::new(Mutex::new(None)),
        route_subtype_override: None,
        fallback_reason: None,
        local_preflight: None,
        external_attempts: Vec::new(),
        started_at: Instant::now(),
        first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        capacity_weight_units: Arc::new(AtomicU32::new(1)),
        latency: RequestLatencyTraceState::new(),
    };
    let usage = CacheUsage {
        total_input_tokens: 100_000,
        input_tokens: 10_000,
        output_tokens: 1,
        cache_creation_input_tokens: 50_000,
        cache_read_input_tokens: 40_000,
        cache_creation_5m_input_tokens: 50_000,
        cache_creation_1h_input_tokens: 0,
    };

    let capped = usage_context.reported_usage_for_downstream(usage, UsageSource::LocalPromptCache);
    assert!((0..=3_300).contains(&capped.cache_creation_input_tokens));
    assert!((1..=96).contains(&capped.input_tokens));
    assert_eq!(
        capped.cache_read_input_tokens,
        usage.cache_read_input_tokens.saturating_add(
            usage_context
                .input_tokens
                .saturating_sub(capped.input_tokens)
        )
    );
    assert!(capped.cache_read_input_tokens > usage.cache_read_input_tokens);

    let upstream_metadata =
        usage_context.reported_usage_for_downstream(usage, UsageSource::UpstreamMetadata);
    assert!((1..=96).contains(&upstream_metadata.input_tokens));
    assert_eq!(upstream_metadata.cache_read_input_tokens, 0);
    assert_eq!(
        upstream_metadata.cache_creation_input_tokens,
        usage_context
            .input_tokens
            .saturating_sub(upstream_metadata.input_tokens)
    );

    let upstream_metadata_with_raw = usage_context.reported_usage_for_downstream_with_raw(
        usage,
        UsageSource::UpstreamMetadata,
        raw_usage_to_reported_raw(usage),
    );
    assert!((0..=3_300).contains(&upstream_metadata_with_raw.cache_creation_input_tokens));
    assert!((1..=96).contains(&upstream_metadata_with_raw.input_tokens));
    assert_eq!(
        upstream_metadata_with_raw.cache_read_input_tokens,
        usage.cache_read_input_tokens.saturating_add(
            usage
                .input_tokens
                .saturating_sub(upstream_metadata_with_raw.input_tokens)
        )
    );
    assert!(upstream_metadata_with_raw.cache_read_input_tokens > usage.cache_read_input_tokens);
}

#[test]
fn unreported_kiro_rs_tool_usage_caps_standard_cache_fields_only_for_local_cache() {
    let usage_context = RequestUsageContext {
        recorder: Arc::new(UsageRecorder::new(10)),
        tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
        prompt_cache: Arc::new(PromptCacheTracker::default()),
        prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
        pricing_catalog: Arc::new(PricingCatalog::new()),
        request_id: "req_dfcache_tool_standard_guard".to_string(),
        error_id: "req_01dfcache_tool_standard_guard".to_string(),
        endpoint: "/dfcache/team/v1/messages".to_string(),
        stream: false,
        model: "claude-sonnet-4-6".to_string(),
        upstream_model: None,
        model_resolution_source: None,
        model_resolution_note: None,
        requested_max_tokens: 0,
        downstream_stop_reason: Arc::new(Mutex::new(None)),
        conversation_id: Some("session-dfcache-tool".to_string()),
        request_api_key_id: None,
        prompt_cache_scope_conversation_id: Some("session-dfcache-tool".to_string()),
        input_tokens: 304_883,
        context_window_tokens: 1_000_000,
        prompt_cache_profile: None,
        kiro_rs_tool_prompt_cache_plan: None,
        prompt_cache_route_namespace: Some("/dfcache/team".to_string()),
        prompt_cache_strategy_type: PromptCacheStrategyType::KiroRsTool,
        simulation_mode: PromptCacheSimulationMode::Disabled,
        prompt_cache_target_read_ratio: 0.0,
        prompt_cache_token_scale: 1.0,
        prompt_cache_max_simulated_input_tokens: 0,
        prompt_cache_cap_jitter_min_tokens: 0,
        prompt_cache_cap_jitter_max_tokens: 0,
        prompt_cache_scale_min_input_tokens: 0,
        prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
        prompt_cache_bounds: PromptCacheBounds::default(),
        reported_cache_usage_policy: None,
        simulated_usage: None,
        simulated_source: Some(UsageSource::LocalPromptCache),
        payload_breakdown: None,
        payload_guard_report: None,
        error_metadata: Arc::new(Mutex::new(None)),
        route_subtype_override: None,
        fallback_reason: None,
        local_preflight: None,
        external_attempts: Vec::new(),
        started_at: Instant::now(),
        first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        capacity_weight_units: Arc::new(AtomicU32::new(1)),
        latency: RequestLatencyTraceState::new(),
    };
    let usage = CacheUsage {
        total_input_tokens: 5_292_349,
        input_tokens: 349,
        output_tokens: 17,
        cache_creation_input_tokens: 2_645_419,
        cache_read_input_tokens: 2_646_152,
        cache_creation_5m_input_tokens: 1_800_000,
        cache_creation_1h_input_tokens: 845_419,
    };

    let local = usage_context.reported_usage_for_downstream(usage, UsageSource::LocalPromptCache);
    assert_eq!(local.input_tokens, usage.input_tokens);
    assert_eq!(local.output_tokens, usage.output_tokens);
    assert!(local.cache_read_input_tokens <= 700_000);
    assert!(local.cache_creation_input_tokens <= 400_000);
    assert_eq!(
        local.total_input_tokens,
        local
            .input_tokens
            .saturating_add(local.cache_creation_input_tokens)
            .saturating_add(local.cache_read_input_tokens)
    );
    assert!(
        local.cache_creation_5m_input_tokens + local.cache_creation_1h_input_tokens
            <= local.cache_creation_input_tokens
    );

    let upstream =
        usage_context.reported_usage_for_downstream(usage, UsageSource::UpstreamMetadata);
    assert_eq!(upstream.input_tokens, usage_context.input_tokens);
    assert_eq!(upstream.output_tokens, usage.output_tokens);
    assert_eq!(upstream.cache_creation_input_tokens, 0);
    assert_eq!(upstream.cache_read_input_tokens, 0);
}

#[test]
fn upstream_metadata_raw_usage_is_shaped_by_high_cache_reported_usage() {
    let usage_context = RequestUsageContext {
        recorder: Arc::new(UsageRecorder::new(10)),
        tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
        prompt_cache: Arc::new(PromptCacheTracker::default()),
        prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
        pricing_catalog: Arc::new(PricingCatalog::new()),
        request_id: "req_upstream_raw_reported_limit".to_string(),
        error_id: "req_01upstream_raw_reported_limit".to_string(),
        endpoint: "/ha/v1/messages".to_string(),
        stream: false,
        model: "claude-haiku-4-5".to_string(),
        upstream_model: None,
        model_resolution_source: None,
        model_resolution_note: None,
        requested_max_tokens: 0,
        downstream_stop_reason: Arc::new(Mutex::new(None)),
        conversation_id: Some("session-upstream-raw-limit".to_string()),
        request_api_key_id: None,
        prompt_cache_scope_conversation_id: Some("session-upstream-raw-limit".to_string()),
        input_tokens: 1_234,
        context_window_tokens: 200_000,
        prompt_cache_profile: None,
        kiro_rs_tool_prompt_cache_plan: None,
        prompt_cache_route_namespace: None,
        prompt_cache_strategy_type: PromptCacheStrategyType::CurrentHighCache,
        simulation_mode: PromptCacheSimulationMode::HighCache,
        prompt_cache_target_read_ratio: 0.95,
        prompt_cache_token_scale: 1.0,
        prompt_cache_max_simulated_input_tokens: 0,
        prompt_cache_cap_jitter_min_tokens: 0,
        prompt_cache_cap_jitter_max_tokens: 0,
        prompt_cache_scale_min_input_tokens: 0,
        prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
        prompt_cache_bounds: PromptCacheBounds::default(),
        reported_cache_usage_policy: reported_cache_usage_policy(
            PromptCacheStrategyType::CurrentHighCache,
            PromptCacheSimulationMode::HighCache,
            &ReportedUsagePathPolicy {
                input: ReportedUsageFieldPolicy::sample_input_max(500),
                ..ReportedUsagePathPolicy::default()
            },
            11,
        ),
        simulated_usage: None,
        simulated_source: None,
        payload_breakdown: None,
        payload_guard_report: None,
        error_metadata: Arc::new(Mutex::new(None)),
        route_subtype_override: None,
        fallback_reason: None,
        local_preflight: None,
        external_attempts: Vec::new(),
        started_at: Instant::now(),
        first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        capacity_weight_units: Arc::new(AtomicU32::new(1)),
        latency: RequestLatencyTraceState::new(),
    };
    let raw_usage = CacheUsage {
        total_input_tokens: 1_234,
        input_tokens: 1_234,
        output_tokens: 7,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        cache_creation_5m_input_tokens: 0,
        cache_creation_1h_input_tokens: 0,
    };

    let reported =
        usage_context.reported_usage_for_downstream(raw_usage, UsageSource::UpstreamMetadata);

    assert!((1..=500).contains(&reported.input_tokens));
    assert_eq!(reported.cache_read_input_tokens, 0);
    assert_eq!(
        reported.cache_creation_input_tokens,
        raw_usage.input_tokens.saturating_sub(reported.input_tokens)
    );
    assert_eq!(reported.output_tokens, 7);
    assert_eq!(
        reported.total_input_tokens,
        reported
            .input_tokens
            .saturating_add(reported.cache_read_input_tokens)
            .saturating_add(reported.cache_creation_input_tokens)
    );
}

#[test]
fn cc_local_prompt_cache_stream_reported_usage_caps_prod_like_input() {
    let reported_usage_config = ReportedUsageConfig::default();
    let request_input_tokens = 17_241;
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let usage_context = RequestUsageContext {
        recorder: usage_recorder.clone(),
        tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
        prompt_cache: Arc::new(PromptCacheTracker::default()),
        prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
        pricing_catalog: Arc::new(PricingCatalog::new()),
        request_id: "req_prod_like_cc_reported_usage".to_string(),
        error_id: "req_01prod_like_cc_reported_usage".to_string(),
        endpoint: "/cc/v1/messages".to_string(),
        stream: true,
        model: "claude-opus-4-6".to_string(),
        upstream_model: Some("claude-opus-4.6".to_string()),
        model_resolution_source: None,
        model_resolution_note: None,
        requested_max_tokens: 0,
        downstream_stop_reason: Arc::new(Mutex::new(None)),
        conversation_id: Some("conversation-prod-like".to_string()),
        request_api_key_id: None,
        prompt_cache_scope_conversation_id: Some("conversation-prod-like".to_string()),
        input_tokens: request_input_tokens,
        context_window_tokens: 200_000,
        prompt_cache_profile: None,
        kiro_rs_tool_prompt_cache_plan: None,
        prompt_cache_route_namespace: None,
        prompt_cache_strategy_type: PromptCacheStrategyType::CurrentHighCache,
        simulation_mode: PromptCacheSimulationMode::HighCache,
        prompt_cache_target_read_ratio: 0.99,
        prompt_cache_token_scale: 2.0,
        prompt_cache_max_simulated_input_tokens: 300_000,
        prompt_cache_cap_jitter_min_tokens: 0,
        prompt_cache_cap_jitter_max_tokens: 0,
        prompt_cache_scale_min_input_tokens: 20_000,
        prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
        prompt_cache_bounds: PromptCacheBounds::default(),
        reported_cache_usage_policy: reported_cache_usage_policy_for_path(
            "/cc/v1/messages",
            PromptCacheSimulationMode::HighCache,
            &reported_usage_config,
            7,
        ),
        simulated_usage: Some(cache::CacheSimulation {
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 36_109,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
            target_cache_ratio: Some(0.99),
            amplification: None,
            split_cached_input: false,
            ..Default::default()
        }),
        simulated_source: Some(UsageSource::LocalPromptCache),
        payload_breakdown: None,
        payload_guard_report: None,
        error_metadata: Arc::new(Mutex::new(None)),
        route_subtype_override: None,
        fallback_reason: None,
        local_preflight: None,
        external_attempts: Vec::new(),
        started_at: Instant::now(),
        first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        capacity_weight_units: Arc::new(AtomicU32::new(1)),
        latency: RequestLatencyTraceState::new(),
    };
    let credential_usage =
        usage_context.attach_credential(Some(131), None, false, false, Vec::new());
    let prod_like_usage = CacheUsage {
        total_input_tokens: 57_499,
        input_tokens: 21_390,
        output_tokens: 6,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 36_109,
        cache_creation_5m_input_tokens: 0,
        cache_creation_1h_input_tokens: 0,
    };

    let reported = credential_usage.final_reported_usage_for_stream(
        prod_like_usage,
        None,
        true,
        request_input_tokens,
    );

    assert!((1..=96).contains(&reported.input_tokens));
    assert_eq!(reported.output_tokens, 6);
    assert_eq!(reported.cache_creation_input_tokens, 0);
    assert_eq!(
        reported.cache_read_input_tokens,
        prod_like_usage
            .cache_read_input_tokens
            .saturating_add(request_input_tokens.saturating_sub(reported.input_tokens))
    );
    let raw_usage = raw_usage_from_metadata_or_estimate(
        None,
        request_input_tokens,
        prod_like_usage.output_tokens,
    );

    credential_usage.record_success_reported(
        reported,
        UsageSource::LocalPromptCache,
        Some(raw_usage),
    );
    let records = usage_recorder.query(UsageRecordQuery::default());
    assert_eq!(records.total, 1);
    let record = records.records.first().expect("usage record should exist");
    assert_eq!(record.compat_input_tokens, reported.input_tokens);
    assert_eq!(record.output_tokens, 6);
    assert_eq!(
        record.cache_creation_input_tokens,
        reported.cache_creation_input_tokens
    );
    assert_eq!(
        record.cache_read_input_tokens,
        reported.cache_read_input_tokens
    );
    let raw_usage = record.raw_usage.expect("raw usage should be retained");
    assert_eq!(raw_usage.total_input_tokens, request_input_tokens);
    assert_eq!(raw_usage.input_tokens, request_input_tokens);
    assert_eq!(raw_usage.output_tokens, prod_like_usage.output_tokens);
    assert_eq!(raw_usage.cache_creation_input_tokens, 0);
    assert_eq!(raw_usage.cache_read_input_tokens, 0);
}

#[test]
fn success_usage_record_uses_raw_usage_for_actual_input_diagnostic() {
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let usage_context = RequestUsageContext {
        recorder: usage_recorder.clone(),
        tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
        prompt_cache: Arc::new(PromptCacheTracker::default()),
        prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
        pricing_catalog: Arc::new(PricingCatalog::new()),
        request_id: "req_context_actual_input".to_string(),
        error_id: "req_01context_actual_input".to_string(),
        endpoint: "/cc/v1/messages".to_string(),
        stream: false,
        model: "claude-sonnet-4-6".to_string(),
        upstream_model: None,
        model_resolution_source: None,
        model_resolution_note: None,
        requested_max_tokens: 0,
        downstream_stop_reason: Arc::new(Mutex::new(None)),
        conversation_id: Some("context-estimate-session".to_string()),
        request_api_key_id: None,
        prompt_cache_scope_conversation_id: None,
        input_tokens: 141,
        context_window_tokens: 200_000,
        prompt_cache_profile: None,
        kiro_rs_tool_prompt_cache_plan: None,
        prompt_cache_route_namespace: None,
        prompt_cache_strategy_type: PromptCacheStrategyType::NoCache,
        simulation_mode: PromptCacheSimulationMode::Disabled,
        prompt_cache_target_read_ratio: 0.95,
        prompt_cache_token_scale: 1.0,
        prompt_cache_max_simulated_input_tokens: 0,
        prompt_cache_cap_jitter_min_tokens: 0,
        prompt_cache_cap_jitter_max_tokens: 0,
        prompt_cache_scale_min_input_tokens: 0,
        prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
        prompt_cache_bounds: PromptCacheBounds::default(),
        reported_cache_usage_policy: None,
        simulated_usage: None,
        simulated_source: None,
        payload_breakdown: None,
        payload_guard_report: None,
        error_metadata: Arc::new(Mutex::new(None)),
        route_subtype_override: None,
        fallback_reason: None,
        local_preflight: None,
        external_attempts: Vec::new(),
        started_at: Instant::now(),
        first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        capacity_weight_units: Arc::new(AtomicU32::new(1)),
        latency: RequestLatencyTraceState::new(),
    };
    let credential_usage = usage_context.attach_credential(Some(9), None, false, false, Vec::new());
    let usage = CacheUsage {
        total_input_tokens: 4_275,
        input_tokens: 4_275,
        output_tokens: 1,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        cache_creation_5m_input_tokens: 0,
        cache_creation_1h_input_tokens: 0,
    };

    credential_usage.record_success_reported(usage, UsageSource::ContextEstimate, Some(usage));

    let records = usage_recorder.query(UsageRecordQuery::default());
    assert_eq!(records.total, 1);
    let record = records.records.first().expect("usage record should exist");
    assert_eq!(record.total_input_tokens, 4_275);
    assert_eq!(record.compat_input_tokens, 4_275);
    assert_eq!(record.cache_creation_input_tokens, 0);
    assert_eq!(record.cache_read_input_tokens, 0);
    let raw_usage = record.raw_usage.expect("raw usage should be retained");
    assert_eq!(raw_usage.total_input_tokens, 4_275);
}

#[test]
fn kiro_rs_tool_local_prompt_cache_uses_strategy_usage_without_legacy_reported_usage() {
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let usage_context = RequestUsageContext {
        recorder: usage_recorder.clone(),
        tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
        prompt_cache: Arc::new(PromptCacheTracker::default()),
        prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
        pricing_catalog: Arc::new(PricingCatalog::new()),
        request_id: "req_kiro_strategy_reported_usage".to_string(),
        error_id: "req_01kiro_strategy_reported_usage".to_string(),
        endpoint: "/kiro/v1/messages".to_string(),
        stream: false,
        model: "claude-sonnet-4-6".to_string(),
        upstream_model: None,
        model_resolution_source: None,
        model_resolution_note: None,
        requested_max_tokens: 0,
        downstream_stop_reason: Arc::new(Mutex::new(None)),
        conversation_id: Some("conversation-kiro-strategy".to_string()),
        request_api_key_id: None,
        prompt_cache_scope_conversation_id: Some("conversation-kiro-strategy".to_string()),
        input_tokens: 100_000,
        context_window_tokens: 200_000,
        prompt_cache_profile: None,
        kiro_rs_tool_prompt_cache_plan: None,
        prompt_cache_route_namespace: Some("/kiro".to_string()),
        prompt_cache_strategy_type: PromptCacheStrategyType::KiroRsTool,
        simulation_mode: PromptCacheSimulationMode::Disabled,
        prompt_cache_target_read_ratio: 0.5,
        prompt_cache_token_scale: 3.0,
        prompt_cache_max_simulated_input_tokens: 300_000,
        prompt_cache_cap_jitter_min_tokens: 0,
        prompt_cache_cap_jitter_max_tokens: 0,
        prompt_cache_scale_min_input_tokens: 0,
        prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
        prompt_cache_bounds: PromptCacheBounds::default(),
        reported_cache_usage_policy: reported_cache_usage_policy(
            PromptCacheStrategyType::KiroRsTool,
            PromptCacheSimulationMode::Disabled,
            &ReportedUsagePathPolicy {
                input: ReportedUsageFieldPolicy::sample_input_max(96),
                ..ReportedUsagePathPolicy::default()
            },
            7,
        ),
        simulated_usage: Some(cache::CacheSimulation {
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 60_000,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
            target_cache_ratio: None,
            amplification: None,
            split_cached_input: true,
            ..Default::default()
        }),
        simulated_source: Some(UsageSource::LocalPromptCache),
        payload_breakdown: None,
        payload_guard_report: None,
        error_metadata: Arc::new(Mutex::new(None)),
        route_subtype_override: None,
        fallback_reason: None,
        local_preflight: None,
        external_attempts: Vec::new(),
        started_at: Instant::now(),
        first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        capacity_weight_units: Arc::new(AtomicU32::new(1)),
        latency: RequestLatencyTraceState::new(),
    };

    assert!(usage_context.reported_cache_usage_policy().is_none());
    let usage = cache::build_usage_with_simulation_policy(
        None,
        100_000,
        6,
        usage_context.simulated_usage,
        should_build_local_prompt_cache_usage(
            usage_context.prompt_cache_strategy_type,
            usage_context.simulation_mode,
        ),
    );
    assert_eq!(usage.input_tokens, 40_000);
    assert_eq!(usage.cache_read_input_tokens, 60_000);

    let credential_usage =
        usage_context.attach_credential(Some(131), None, false, false, Vec::new());
    let reported =
        credential_usage.final_reported_usage_for_success(usage, UsageSource::LocalPromptCache);

    assert_eq!(reported.input_tokens, 40_000);
    assert_eq!(reported.cache_read_input_tokens, 60_000);
    assert_eq!(reported.output_tokens, 6);

    let raw_usage = raw_usage_from_metadata_or_estimate(None, 100_000, 6);
    credential_usage.record_success_reported(
        reported,
        UsageSource::LocalPromptCache,
        Some(raw_usage),
    );
    let records = usage_recorder.query(UsageRecordQuery::default());
    assert_eq!(records.total, 1);
    let record = records.records.first().expect("usage record should exist");
    assert_eq!(record.compat_input_tokens, 40_000);
    assert_eq!(record.cache_read_input_tokens, 60_000);
    let raw_usage = record.raw_usage.expect("raw usage should be retained");
    assert_eq!(raw_usage.input_tokens, 100_000);
    assert_eq!(raw_usage.cache_read_input_tokens, 0);
}

#[test]
fn first_token_detection_ignores_initial_empty_blocks() {
    assert!(!is_first_token_output_event(&SseEvent::new(
        "message_start",
        json!({"type": "message_start"})
    )));
    assert!(!is_first_token_output_event(&SseEvent::new(
        "content_block_start",
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "text", "text": ""}
        })
    )));
    assert!(is_first_token_output_event(&SseEvent::new(
        "content_block_delta",
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": "hello"}
        })
    )));
    assert!(is_first_token_output_event(&SseEvent::new(
        "content_block_start",
        json!({
            "type": "content_block_start",
            "index": 1,
            "content_block": {"type": "tool_use", "id": "toolu_1", "name": "read", "input": {}}
        })
    )));
}

#[test]
fn local_latency_trace_records_markers_without_changing_first_output_semantics() {
    let mut usage_context = RequestUsageContext {
        recorder: Arc::new(UsageRecorder::new(10)),
        tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
        prompt_cache: Arc::new(PromptCacheTracker::default()),
        prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
        pricing_catalog: Arc::new(PricingCatalog::new()),
        request_id: "req_latency_trace".to_string(),
        error_id: "req_01latency_trace".to_string(),
        endpoint: "/cc/v1/messages".to_string(),
        stream: true,
        model: "claude-opus-4-8".to_string(),
        upstream_model: Some("claude-opus-4.8".to_string()),
        model_resolution_source: None,
        model_resolution_note: None,
        requested_max_tokens: 0,
        downstream_stop_reason: Arc::new(Mutex::new(None)),
        conversation_id: Some("session-latency".to_string()),
        request_api_key_id: None,
        prompt_cache_scope_conversation_id: Some("session-latency".to_string()),
        input_tokens: 100,
        context_window_tokens: 200_000,
        prompt_cache_profile: None,
        kiro_rs_tool_prompt_cache_plan: None,
        prompt_cache_route_namespace: None,
        prompt_cache_strategy_type: PromptCacheStrategyType::CurrentHighCache,
        simulation_mode: PromptCacheSimulationMode::Disabled,
        prompt_cache_target_read_ratio: 0.95,
        prompt_cache_token_scale: 1.0,
        prompt_cache_max_simulated_input_tokens: 0,
        prompt_cache_cap_jitter_min_tokens: 0,
        prompt_cache_cap_jitter_max_tokens: 0,
        prompt_cache_scale_min_input_tokens: 0,
        prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
        prompt_cache_bounds: PromptCacheBounds::default(),
        reported_cache_usage_policy: None,
        simulated_usage: None,
        simulated_source: None,
        payload_breakdown: None,
        payload_guard_report: None,
        error_metadata: Arc::new(Mutex::new(None)),
        route_subtype_override: None,
        fallback_reason: None,
        local_preflight: None,
        external_attempts: Vec::new(),
        started_at: Instant::now(),
        first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        capacity_weight_units: Arc::new(AtomicU32::new(1)),
        latency: RequestLatencyTraceState::new(),
    };

    usage_context.mark_payload_guard_latency(Duration::from_millis(3));
    usage_context.mark_upstream_header();
    usage_context.mark_first_upstream_chunk();
    usage_context.mark_upstream_bytes_before_first_output(128);
    usage_context.mark_upstream_pending_chunk_before_first_output();
    usage_context.mark_upstream_frame_before_first_output();
    usage_context.mark_upstream_event_before_first_output(
        &Event::Metadata(crate::kiro::model::events::MetadataEvent::default()),
        0,
    );
    usage_context.mark_upstream_frame_before_first_output();
    usage_context.mark_upstream_frame_decode_error_before_first_output();
    usage_context.mark_upstream_event_parse_error_before_first_output();
    usage_context.mark_stream_events(&[
        SseEvent::new("message_start", json!({"type": "message_start"})),
        SseEvent::new(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {"type": "text", "text": ""}
            }),
        ),
    ]);
    assert!(usage_context.first_token_latency_ms().is_none());

    usage_context.mark_first_upstream_chunk();
    usage_context.mark_stream_events(&[
        SseEvent::new("ping", json!({"type": "ping"})),
        SseEvent::new(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "thinking_delta", "thinking": "thinking"}
            }),
        ),
    ]);
    usage_context.mark_stream_events(&[SseEvent::new(
        "content_block_delta",
        json!({
            "type": "content_block_delta",
            "index": 1,
            "delta": {"type": "text_delta", "text": "hello"}
        }),
    )]);
    usage_context.mark_stream_terminal(StreamTerminalReason::Completed);
    usage_context.mark_suppressed_tool_context_leak(
        2,
        321,
        vec!["continue_tool_result".to_string()],
    );

    let trace = usage_context.latency_trace().expect("latency trace");
    assert_eq!(trace.payload_guard_ms, Some(3));
    assert!(trace.upstream_header_ms.is_some());
    assert!(trace.first_upstream_chunk_ms.is_some());
    assert_eq!(
        trace.first_output_delta_ms,
        usage_context.first_token_latency_ms()
    );
    assert_eq!(trace.first_thinking_delta_ms, trace.first_output_delta_ms);
    assert!(trace.first_visible_text_delta_ms.is_some());
    assert_eq!(trace.chunks_before_first_output, Some(1));
    assert_eq!(trace.events_before_first_output, Some(3));
    assert_eq!(trace.upstream_bytes_before_first_output, Some(128));
    assert_eq!(trace.upstream_frames_before_first_output, Some(2));
    assert_eq!(trace.upstream_events_before_first_output, Some(1));
    assert_eq!(
        trace.upstream_frames_without_downstream_events_before_first_output,
        Some(1)
    );
    assert_eq!(trace.upstream_pending_chunks_before_first_output, Some(1));
    assert_eq!(
        trace.upstream_frame_decode_errors_before_first_output,
        Some(1)
    );
    assert_eq!(
        trace.upstream_event_parse_errors_before_first_output,
        Some(1)
    );
    assert_eq!(
        trace
            .upstream_event_types_before_first_output
            .as_ref()
            .and_then(|counts| counts.get("metadata")),
        Some(&1)
    );
    assert!(trace.stream_gap_to_first_output_ms.is_some());
    assert_eq!(trace.terminal_reason, Some(StreamTerminalReason::Completed));
    assert_eq!(trace.suppressed_tool_context_leak_blocks, Some(2));
    assert_eq!(trace.suppressed_tool_context_leak_chars, Some(321));
    assert_eq!(
        trace.suppressed_tool_context_leak_kinds,
        Some(vec!["continue_tool_result".to_string()])
    );
}

#[test]
fn stream_success_records_requested_max_tokens_and_downstream_stop_reason() {
    let prompt_cache = Arc::new(PromptCacheTracker::default());
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        true,
        usage_recorder.clone(),
        prompt_cache,
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::Disabled,
        0.95,
        CompatProfile::ClaudeCode,
        false,
    );
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    payload.stream = true;
    payload.max_tokens = 100;

    let usage_context = prepare_usage_context(
        &state,
        RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/cc/v1/messages"),
        "/cc/v1/messages",
        true,
        &payload,
        None,
        Some("conv-stop-reason".to_string()),
        Some("conv-stop-reason".to_string()),
        50,
    );
    let credential_usage = attach_test_credential_usage(usage_context, 1);

    let mut stream_context = StreamContext::new_with_simulation(
        &payload.model,
        50,
        200_000,
        false,
        false,
        HashMap::new(),
        None,
        PromptCacheSimulationMode::Disabled,
    );
    stream_context.set_requested_max_tokens(payload.max_tokens);
    let _initial_events = stream_context.generate_initial_events();
    let mut events = Vec::new();
    let assistant_response = crate::kiro::model::events::AssistantResponseEvent {
        content: "near token limit".to_string(),
        ..Default::default()
    };
    events.extend(stream_context.process_kiro_event(&Event::AssistantResponse(assistant_response)));
    events.extend(stream_context.process_kiro_event(&Event::MessageMetadata(
        crate::kiro::model::events::MessageMetadataEvent {
            conversation_id: Some("conv-stop-reason".to_string()),
            utterance_id: Some("utt-stop-reason".to_string()),
            token_usage: Some(MetadataTokenUsage {
                uncached_input_tokens: 50,
                output_tokens: 95,
                total_tokens: 145,
                cache_read_input_tokens: 0,
                cache_write_input_tokens: 0,
            }),
        },
    )));
    events.extend(stream_context.generate_final_events());
    assert!(events.iter().any(|event| {
        event.event == "message_delta" && event.data["delta"]["stop_reason"] == "max_tokens"
    }));

    credential_usage.record_success_from_stream(&stream_context);

    let records = usage_recorder.query(UsageRecordQuery::default());
    assert_eq!(records.total, 1);
    let record = records.records.first().expect("usage record should exist");
    assert_eq!(record.requested_max_tokens, Some(100));
    assert_eq!(record.downstream_stop_reason.as_deref(), Some("max_tokens"));
}

#[test]
fn stream_zero_context_and_metadata_record_request_estimate_consistently() {
    use crate::kiro::model::events::{AssistantResponseEvent, ContextUsageEvent, MetadataEvent};

    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        true,
        usage_recorder.clone(),
        Arc::new(PromptCacheTracker::default()),
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::Disabled,
        0.95,
        CompatProfile::ClaudeCode,
        false,
    );
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    payload.stream = true;

    let usage_context = prepare_usage_context(
        &state,
        RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/cc/v1/messages"),
        "/cc/v1/messages",
        true,
        &payload,
        None,
        Some("conv-zero-context".to_string()),
        Some("conv-zero-context".to_string()),
        4_096,
    );
    let credential_usage = attach_test_credential_usage(usage_context, 1);
    let mut stream_context = StreamContext::new_with_simulation(
        &payload.model,
        4_096,
        200_000,
        false,
        false,
        HashMap::new(),
        None,
        PromptCacheSimulationMode::Disabled,
    );
    let _initial_events = stream_context.generate_initial_events();
    let assistant_response = AssistantResponseEvent {
        content: "fake response".to_string(),
        ..Default::default()
    };
    let mut events =
        stream_context.process_kiro_event(&Event::AssistantResponse(assistant_response));
    events.extend(
        stream_context.process_kiro_event(&Event::ContextUsage(ContextUsageEvent {
            context_usage_percentage: 0.0,
        })),
    );
    events.extend(
        stream_context.process_kiro_event(&Event::Metadata(MetadataEvent {
            token_usage: Some(MetadataTokenUsage::default()),
        })),
    );
    events.extend(stream_context.generate_final_events());

    let downstream_usage = &events
        .iter()
        .find(|event| event.event == "message_delta")
        .expect("message_delta should exist")
        .data["usage"];
    assert_eq!(downstream_usage["input_tokens"], 4_096);
    assert!(
        downstream_usage["output_tokens"]
            .as_i64()
            .is_some_and(|tokens| tokens > 0)
    );

    credential_usage.record_success_from_stream(&stream_context);

    let records = usage_recorder.query(UsageRecordQuery::default());
    assert_eq!(records.total, 1);
    let record = records.records.first().expect("usage record should exist");
    assert_eq!(record.usage_source, UsageSource::RequestEstimate);
    assert_eq!(record.total_input_tokens, 4_096);
    assert_eq!(record.compat_input_tokens, 4_096);
    assert_eq!(
        record.output_tokens,
        downstream_usage["output_tokens"].as_i64().unwrap() as i32
    );
    assert_eq!(record.cache_read_input_tokens, 0);
    assert_eq!(record.cache_creation_input_tokens, 0);
    let raw_usage = record.raw_usage.expect("raw usage should be retained");
    assert_eq!(raw_usage.input_tokens, 4_096);
    assert_eq!(raw_usage.total_input_tokens, 4_096);
    assert!(raw_usage.output_tokens > 0);
}

#[test]
fn path_overrides_independently_control_reported_usage_fields() {
    let reported_usage_config = ReportedUsageConfig::default();
    let usage = CacheUsage {
        total_input_tokens: 100_000,
        input_tokens: 10_000,
        output_tokens: 1,
        cache_creation_input_tokens: 50_000,
        cache_read_input_tokens: 40_000,
        cache_creation_5m_input_tokens: 50_000,
        cache_creation_1h_input_tokens: 0,
    };
    let prompt_cache = Arc::new(PromptCacheTracker::default());
    let usage_recorder = Arc::new(UsageRecorder::new(10));

    let v1_context = RequestUsageContext {
        recorder: usage_recorder.clone(),
        tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
        prompt_cache: prompt_cache.clone(),
        prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
        pricing_catalog: Arc::new(PricingCatalog::new()),
        request_id: "req_v1_policy".to_string(),
        error_id: "req_01v1_policy".to_string(),
        endpoint: "/v1/messages".to_string(),
        stream: false,
        model: "claude-sonnet-4-6".to_string(),
        upstream_model: None,
        model_resolution_source: None,
        model_resolution_note: None,
        requested_max_tokens: 0,
        downstream_stop_reason: Arc::new(Mutex::new(None)),
        conversation_id: Some("session-policy".to_string()),
        request_api_key_id: None,
        prompt_cache_scope_conversation_id: Some("session-policy".to_string()),
        input_tokens: 100_000,
        context_window_tokens: 200_000,
        prompt_cache_profile: None,
        kiro_rs_tool_prompt_cache_plan: None,
        prompt_cache_route_namespace: None,
        prompt_cache_strategy_type: PromptCacheStrategyType::CurrentHighCache,
        simulation_mode: PromptCacheSimulationMode::HighCache,
        prompt_cache_target_read_ratio: 0.95,
        prompt_cache_token_scale: 1.0,
        prompt_cache_max_simulated_input_tokens: 0,
        prompt_cache_cap_jitter_min_tokens: 0,
        prompt_cache_cap_jitter_max_tokens: 0,
        prompt_cache_scale_min_input_tokens: 0,
        prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
        prompt_cache_bounds: PromptCacheBounds::default(),
        reported_cache_usage_policy: reported_cache_usage_policy_for_path(
            "/v1/messages",
            PromptCacheSimulationMode::HighCache,
            &reported_usage_config,
            7,
        ),
        simulated_usage: None,
        simulated_source: Some(UsageSource::LocalPromptCache),
        payload_breakdown: None,
        payload_guard_report: None,
        error_metadata: Arc::new(Mutex::new(None)),
        route_subtype_override: None,
        fallback_reason: None,
        local_preflight: None,
        external_attempts: Vec::new(),
        started_at: Instant::now(),
        first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        capacity_weight_units: Arc::new(AtomicU32::new(1)),
        latency: RequestLatencyTraceState::new(),
    };
    let cc_context = RequestUsageContext {
        endpoint: "/cc/v1/messages".to_string(),
        request_id: "req_cc_policy".to_string(),
        error_id: "req_01cc_policy".to_string(),
        reported_cache_usage_policy: reported_cache_usage_policy_for_path(
            "/cc/v1/messages",
            PromptCacheSimulationMode::HighCache,
            &reported_usage_config,
            7,
        ),
        ..v1_context.clone()
    };
    let ha_context = RequestUsageContext {
        endpoint: "/ha/v1/messages".to_string(),
        request_id: "req_ha_policy".to_string(),
        error_id: "req_01ha_policy".to_string(),
        reported_cache_usage_policy: reported_cache_usage_policy_for_path(
            "/ha/v1/messages",
            PromptCacheSimulationMode::HighCache,
            &reported_usage_config,
            7,
        ),
        ..v1_context.clone()
    };
    let na_context = RequestUsageContext {
        endpoint: "/na/v1/messages".to_string(),
        request_id: "req_na_policy".to_string(),
        error_id: "req_01na_policy".to_string(),
        prompt_cache_strategy_type: PromptCacheStrategyType::NoCache,
        simulation_mode: PromptCacheSimulationMode::Disabled,
        reported_cache_usage_policy: reported_cache_usage_policy(
            PromptCacheStrategyType::NoCache,
            PromptCacheSimulationMode::Disabled,
            &reported_usage_config.policy_for_path("/na/v1/messages"),
            7,
        ),
        simulated_source: None,
        ..v1_context.clone()
    };

    assert!(v1_context.reported_cache_usage_policy().is_some());
    assert!(cc_context.reported_cache_usage_policy().is_some());
    assert!(ha_context.reported_cache_usage_policy().is_some());
    assert!(na_context.reported_cache_usage_policy().is_none());

    let v1_reported =
        v1_context.reported_usage_for_downstream(usage, UsageSource::LocalPromptCache);
    assert_eq!(v1_reported.input_tokens, v1_context.input_tokens);
    assert_eq!(v1_reported.output_tokens, usage.output_tokens);
    assert_eq!(
        v1_reported.cache_creation_input_tokens,
        usage.cache_creation_input_tokens
    );
    assert_eq!(
        v1_reported.cache_read_input_tokens,
        usage.cache_read_input_tokens
    );

    let cc_reported =
        cc_context.reported_usage_for_downstream(usage, UsageSource::LocalPromptCache);
    assert!((1..=96).contains(&cc_reported.input_tokens));
    assert!((0..=3_300).contains(&cc_reported.cache_creation_input_tokens));
    assert_eq!(
        cc_reported.cache_read_input_tokens,
        usage.cache_read_input_tokens.saturating_add(
            cc_context
                .input_tokens
                .saturating_sub(cc_reported.input_tokens)
        )
    );
    assert_eq!(cc_reported.output_tokens, usage.output_tokens);

    let ha_reported =
        ha_context.reported_usage_for_downstream(usage, UsageSource::LocalPromptCache);
    assert!((1..=96).contains(&ha_reported.input_tokens));
    assert_eq!(
        ha_reported.cache_creation_input_tokens,
        usage.cache_creation_input_tokens
    );
    assert_eq!(
        ha_reported.cache_creation_5m_input_tokens,
        usage.cache_creation_5m_input_tokens
    );
    assert_eq!(
        ha_reported.cache_creation_1h_input_tokens,
        usage.cache_creation_1h_input_tokens
    );
    assert_eq!(
        ha_reported.cache_read_input_tokens,
        usage.cache_read_input_tokens.saturating_add(
            ha_context
                .input_tokens
                .saturating_sub(ha_reported.input_tokens)
        )
    );
    assert_eq!(ha_reported.output_tokens, usage.output_tokens);

    let na_raw = cache::RawUsage::uncached(12_345, usage.output_tokens);
    let na_reported = na_context.reported_usage_for_downstream_with_raw(
        usage,
        UsageSource::UpstreamMetadata,
        na_raw,
    );
    assert_eq!(na_reported.input_tokens, 12_345);
    assert_eq!(na_reported.output_tokens, usage.output_tokens);
    assert_eq!(na_reported.cache_creation_input_tokens, 0);
    assert_eq!(na_reported.cache_read_input_tokens, 0);
    assert_eq!(na_reported.total_input_tokens, 12_345);
}

#[test]
fn creation_control_preserves_reported_usage_input_policy() {
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let usage_context = RequestUsageContext {
        recorder: usage_recorder,
        tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
        prompt_cache: Arc::new(PromptCacheTracker::default()),
        prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
        pricing_catalog: Arc::new(PricingCatalog::new()),
        request_id: "req_creation_reported_policy".to_string(),
        error_id: "req_01creation_reported_policy".to_string(),
        endpoint: "/ha/v1/messages".to_string(),
        stream: true,
        model: "claude-sonnet-4-6".to_string(),
        upstream_model: None,
        model_resolution_source: None,
        model_resolution_note: None,
        requested_max_tokens: 0,
        downstream_stop_reason: Arc::new(Mutex::new(None)),
        conversation_id: Some("session-creation-policy".to_string()),
        request_api_key_id: None,
        prompt_cache_scope_conversation_id: Some("session-creation-policy".to_string()),
        input_tokens: 100_000,
        context_window_tokens: 200_000,
        prompt_cache_profile: None,
        kiro_rs_tool_prompt_cache_plan: None,
        prompt_cache_route_namespace: None,
        prompt_cache_strategy_type: PromptCacheStrategyType::CurrentHighCache,
        simulation_mode: PromptCacheSimulationMode::HighCache,
        prompt_cache_target_read_ratio: 0.95,
        prompt_cache_token_scale: 1.0,
        prompt_cache_max_simulated_input_tokens: 0,
        prompt_cache_cap_jitter_min_tokens: 0,
        prompt_cache_cap_jitter_max_tokens: 0,
        prompt_cache_scale_min_input_tokens: 0,
        prompt_cache_creation_control: PromptCacheCreationControlConfig {
            max_creation_tokens_per_event: 30_000,
            ..PromptCacheCreationControlConfig::default()
        },
        prompt_cache_bounds: PromptCacheBounds::default(),
        reported_cache_usage_policy: reported_cache_usage_policy(
            PromptCacheStrategyType::CurrentHighCache,
            PromptCacheSimulationMode::HighCache,
            &ReportedUsagePathPolicy {
                input: ReportedUsageFieldPolicy::sample_input_max(96),
                ..ReportedUsagePathPolicy::default()
            },
            7,
        ),
        simulated_usage: None,
        simulated_source: Some(UsageSource::LocalPromptCache),
        payload_breakdown: None,
        payload_guard_report: None,
        error_metadata: Arc::new(Mutex::new(None)),
        route_subtype_override: None,
        fallback_reason: None,
        local_preflight: None,
        external_attempts: Vec::new(),
        started_at: Instant::now(),
        first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        capacity_weight_units: Arc::new(AtomicU32::new(1)),
        latency: RequestLatencyTraceState::new(),
    };
    let credential_usage = usage_context.attach_credential(Some(1), None, false, false, Vec::new());
    let usage = CacheUsage {
        total_input_tokens: 150_000,
        input_tokens: 100_000,
        output_tokens: 9,
        cache_creation_input_tokens: 50_000,
        cache_read_input_tokens: 0,
        cache_creation_5m_input_tokens: 50_000,
        cache_creation_1h_input_tokens: 0,
    };

    let reported =
        credential_usage.canonical_reported_usage_for_success(usage, UsageSource::LocalPromptCache);

    assert!((1..=96).contains(&reported.input_tokens));
    assert_eq!(reported.cache_read_input_tokens, 0);
    let input_delta = usage.input_tokens.saturating_sub(reported.input_tokens);
    assert!(
        (input_delta.saturating_add(26_400)..input_delta.saturating_add(30_000))
            .contains(&reported.cache_creation_input_tokens)
    );
    assert_eq!(
        reported.total_input_tokens,
        reported
            .input_tokens
            .saturating_add(reported.cache_read_input_tokens)
            .saturating_add(reported.cache_creation_input_tokens)
    );
}

#[test]
fn provider_error_hint_extracts_credential_for_failure_records() {
    let hint = extract_credential_error_hint(
        "非流式 API 请求失败（凭据 #2 IlmiMiazzi@gmail.com）: 429 Too Many Requests",
    )
    .expect("credential hint");
    assert_eq!(hint.id, 2);
    assert_eq!(hint.label.as_deref(), Some("IlmiMiazzi@gmail.com"));
    assert_eq!(hint.display_label(), "#2 IlmiMiazzi@gmail.com");

    let prompt_cache = Arc::new(PromptCacheTracker::default());
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let usage_context = RequestUsageContext {
        recorder: usage_recorder.clone(),
        tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
        prompt_cache,
        prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
        pricing_catalog: Arc::new(PricingCatalog::new()),
        request_id: "req_error_hint".to_string(),
        error_id: "req_01error_hint".to_string(),
        endpoint: "/v1/messages".to_string(),
        stream: false,
        model: "claude-sonnet-4-6".to_string(),
        upstream_model: None,
        model_resolution_source: None,
        model_resolution_note: None,
        requested_max_tokens: 0,
        downstream_stop_reason: Arc::new(Mutex::new(None)),
        conversation_id: Some("session-error".to_string()),
        request_api_key_id: None,
        prompt_cache_scope_conversation_id: Some("session-error".to_string()),
        input_tokens: 4096,
        context_window_tokens: 200_000,
        prompt_cache_profile: None,
        kiro_rs_tool_prompt_cache_plan: None,
        prompt_cache_route_namespace: None,
        prompt_cache_strategy_type: PromptCacheStrategyType::CurrentHighCache,
        simulation_mode: PromptCacheSimulationMode::HighCache,
        prompt_cache_target_read_ratio: 0.95,
        prompt_cache_token_scale: 1.0,
        prompt_cache_max_simulated_input_tokens: 0,
        prompt_cache_cap_jitter_min_tokens: 0,
        prompt_cache_cap_jitter_max_tokens: 0,
        prompt_cache_scale_min_input_tokens: 0,
        prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
        prompt_cache_bounds: PromptCacheBounds::default(),
        reported_cache_usage_policy: None,
        simulated_usage: None,
        simulated_source: None,
        payload_breakdown: None,
        payload_guard_report: None,
        error_metadata: Arc::new(Mutex::new(None)),
        route_subtype_override: None,
        fallback_reason: None,
        local_preflight: None,
        external_attempts: Vec::new(),
        started_at: Instant::now(),
        first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        capacity_weight_units: Arc::new(AtomicU32::new(1)),
        latency: RequestLatencyTraceState::new(),
    };
    usage_context
        .attach_credential(Some(hint.id), hint.label, false, false, Vec::new())
        .with_error_metadata(Some(json!({
            "selectionFailure": {
                "stage": "rpm_limit",
                "primaryReason": "rpm_limited"
            }
        })))
        .record_failure(UsageRecordStatus::Error, "api_error", "upstream failed");

    let records = usage_recorder.query(Default::default()).records;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].credential_id, Some(2));
    assert_eq!(
        records[0].credential_label.as_deref(),
        Some("IlmiMiazzi@gmail.com")
    );
    assert_eq!(records[0].error_id.as_deref(), Some("req_01error_hint"));
    assert_eq!(records[0].error_source.as_deref(), Some("local_account"));
    assert_eq!(
        records[0]
            .error_metadata
            .as_ref()
            .and_then(|value| value.pointer("/selectionFailure/primaryReason"))
            .and_then(|value| value.as_str()),
        Some("rpm_limited")
    );
}

#[test]
fn failure_usage_record_keeps_large_request_estimate_out_of_standard_fields() {
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let request_input_tokens = 2_648_439;
    let usage_context = RequestUsageContext {
        recorder: usage_recorder.clone(),
        tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
        prompt_cache: Arc::new(PromptCacheTracker::default()),
        prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
        pricing_catalog: Arc::new(PricingCatalog::new()),
        request_id: "req_large_failure_estimate".to_string(),
        error_id: "req_01large_failure_estimate".to_string(),
        endpoint: "/dfcache/team/v1/messages".to_string(),
        stream: false,
        model: "claude-sonnet-4-6".to_string(),
        upstream_model: None,
        model_resolution_source: None,
        model_resolution_note: None,
        requested_max_tokens: 0,
        downstream_stop_reason: Arc::new(Mutex::new(None)),
        conversation_id: Some("session-large-failure".to_string()),
        request_api_key_id: None,
        prompt_cache_scope_conversation_id: Some("session-large-failure".to_string()),
        input_tokens: request_input_tokens,
        context_window_tokens: 1_000_000,
        prompt_cache_profile: None,
        kiro_rs_tool_prompt_cache_plan: None,
        prompt_cache_route_namespace: Some("/dfcache/team".to_string()),
        prompt_cache_strategy_type: PromptCacheStrategyType::KiroRsTool,
        simulation_mode: PromptCacheSimulationMode::Disabled,
        prompt_cache_target_read_ratio: 0.0,
        prompt_cache_token_scale: 1.0,
        prompt_cache_max_simulated_input_tokens: 0,
        prompt_cache_cap_jitter_min_tokens: 0,
        prompt_cache_cap_jitter_max_tokens: 0,
        prompt_cache_scale_min_input_tokens: 0,
        prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
        prompt_cache_bounds: PromptCacheBounds::default(),
        reported_cache_usage_policy: None,
        simulated_usage: None,
        simulated_source: None,
        payload_breakdown: None,
        payload_guard_report: None,
        error_metadata: Arc::new(Mutex::new(None)),
        route_subtype_override: None,
        fallback_reason: None,
        local_preflight: None,
        external_attempts: Vec::new(),
        started_at: Instant::now(),
        first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        capacity_weight_units: Arc::new(AtomicU32::new(1)),
        latency: RequestLatencyTraceState::new(),
    };

    usage_context
        .attach_credential(
            Some(1141),
            Some("ksk".to_string()),
            false,
            false,
            Vec::new(),
        )
        .record_failure(
            UsageRecordStatus::Error,
            "rate_limit_error",
            "upstream rate limit",
        );

    let records = usage_recorder.query(Default::default()).records;
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.status, UsageRecordStatus::Error);
    assert_eq!(record.total_input_tokens, request_input_tokens);
    assert_eq!(
        record
            .raw_usage
            .as_ref()
            .map(|usage| usage.total_input_tokens),
        Some(request_input_tokens)
    );
    assert_eq!(record.compat_input_tokens, 0);
    assert_eq!(record.billable_input_tokens, 0);
    assert_eq!(record.output_tokens, 0);
    assert_eq!(record.cache_read_input_tokens, 0);
    assert_eq!(record.cache_creation_input_tokens, 0);
    assert_eq!(record.estimated_cost_usd, 0.0);
}

#[test]
fn provider_error_metadata_wraps_selection_failure_without_error_id_duplication() {
    let mut reason_counts = BTreeMap::new();
    reason_counts.insert(AccountRejectReason::RpmLimited, 3);
    let summary = SelectionFailureSummary {
        request_id: "req_selection".to_string(),
        route: "/cc/v1/messages".to_string(),
        model: "claude-sonnet-4-6".to_string(),
        stage: SelectionFailureStage::RpmLimit,
        primary_reason: AccountRejectReason::RpmLimited,
        rejected_account_count: 3,
        waitable_account_count: 3,
        retry_after_ms: Some(1000),
        reason_counts,
        sampled_accounts: Vec::new(),
        dispatch_wait_ms: None,
        queue_depth: 0,
        global_in_flight: 0,
    };
    let err: Error = KiroCallError::new("local selection failed", Vec::new())
        .with_selection_failure(Some(summary))
        .into();

    let metadata = provider_error_metadata(&err).expect("selection failure metadata");

    assert_eq!(
        metadata
            .pointer("/selectionFailure/primaryReason")
            .and_then(|value| value.as_str()),
        Some("rpm_limited")
    );
    assert!(metadata.pointer("/errorId").is_none());
    assert!(metadata.pointer("/selectionFailure/errorId").is_none());
}

#[test]
fn local_scheduler_error_enables_admission_backoff_classification() {
    let err =
        anyhow::anyhow!("本地账号调度容量暂不可用（Redis 调度协调状态不可用，retry_after_secs=4）");

    assert_eq!(local_temporary_admission_backoff_secs(&err, None), Some(4));
}

#[test]
fn upstream_rate_limit_does_not_enable_local_admission_backoff() {
    let err = anyhow::anyhow!(
        "{}",
        r#"Kiro API 请求失败: 429 Too Many Requests {"error":"rate_limit"}"#
    );

    assert_eq!(local_temporary_admission_backoff_secs(&err, None), None);
}

#[tokio::test]
async fn content_length_threshold_error_is_not_reported_as_context_window_full() {
    let response = map_provider_error(
        anyhow::anyhow!(
            "{}",
            r#"流式 API 请求失败（凭据 #1 test@example.com）: 400 Bad Request {"message":"Input is too long.","reason":"CONTENT_LENGTH_EXCEEDS_THRESHOLD"}"#
        ),
        Some("req_test_content_length"),
        None,
        None,
    );

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    let message = value
        .pointer("/error/message")
        .and_then(|v| v.as_str())
        .expect("error message");

    assert!(message.contains("input content length exceeded"));
    assert!(message.contains("separate from the model context window"));
    assert!(!message.contains("Context window is full"));
}

#[tokio::test]
async fn prompt_too_long_error_maps_to_input_length_message() {
    let response = map_provider_error(
        anyhow::anyhow!(
            "{}",
            r#"流式 API 请求失败（账号 #9 hidden）: 400 Bad Request {"error":{"message":"prompt is too long: > 1000000 maximum","type":"invalid_request_error"},"type":"error"}"#
        ),
        Some("req_test_prompt_too_long"),
        None,
        None,
    );

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    let message = value
        .pointer("/error/message")
        .and_then(|v| v.as_str())
        .expect("error message");

    assert!(message.contains("input content length exceeded"));
    assert!(!message.contains("hidden"));
    assert!(!message.contains("1000000"));
}

#[tokio::test]
async fn official_kiro_upstream_400_message_is_exposed_without_internal_prefix() {
    let response = map_provider_error(
        anyhow::anyhow!(
            "{}",
            r#"流式 API 请求失败（账号 #7 secret-user）: 400 Bad Request {"message":"Invalid tool use format.","reason":"REQUEST_BODY_INVALID"}"#
        ),
        Some("req_test_official_upstream"),
        Some("req_01official"),
        None,
    );

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response
            .headers()
            .get("x-error-id")
            .and_then(|value| value.to_str().ok()),
        Some("req_01official")
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(
        value.pointer("/error/type").and_then(|v| v.as_str()),
        Some("invalid_request_error")
    );
    let message = value
        .pointer("/error/message")
        .and_then(|v| v.as_str())
        .expect("error message");

    assert!(message.contains("Invalid tool use format."));
    assert!(message.contains("REQUEST_BODY_INVALID"));
    assert!(message.contains("error ID: req_01official"));
    assert!(!message.contains("账号"));
    assert!(!message.contains("secret-user"));
}

#[tokio::test]
async fn malformed_upstream_error_exposes_safe_official_message() {
    let response = map_provider_error(
        anyhow::anyhow!(
            "{}",
            r#"流式 API 请求失败（凭据 #1 test@example.com，请求无效）: 400 Bad Request {"message":"Improperly formed request.","reason":null}"#
        ),
        Some("req_test_malformed"),
        None,
        None,
    );

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    let message = value
        .pointer("/error/message")
        .and_then(|v| v.as_str())
        .expect("error message");

    assert_eq!(message, "Improperly formed request.");
    assert!(!message.contains("tool_use"));
    assert!(!message.contains("转换"));
    assert!(!message.contains("test@example.com"));
    assert!(!message.contains("凭据"));
}

#[tokio::test]
async fn official_kiro_bad_request_message_is_exposed_when_safe() {
    let response = map_provider_error(
        anyhow::anyhow!(
            "{}",
            r#"流式 API 请求失败（凭据 #1 hidden@example.com，请求无效）: 400 Bad Request {"message":"Bedrock error message: Could not process image","reason":"IMAGE_FORMAT_UNSUPPORTED"}"#
        ),
        Some("req_test_official_bad_request"),
        Some("req_01official_bad_request"),
        None,
    );

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(
        value.pointer("/error/type").and_then(|v| v.as_str()),
        Some("invalid_request_error")
    );
    let message = value
        .pointer("/error/message")
        .and_then(|v| v.as_str())
        .expect("error message");

    assert!(message.contains("Bedrock error message: Could not process image"));
    assert!(message.contains("IMAGE_FORMAT_UNSUPPORTED"));
    assert!(message.contains("error ID: req_01official_bad_request"));
    assert!(!message.contains("hidden@example.com"));
    assert!(!message.contains("凭据"));
}

#[tokio::test]
async fn official_kiro_high_load_message_is_exposed_when_safe() {
    let response = map_provider_error(
        anyhow::anyhow!(
            "{}",
            r#"流式 API 请求失败（账号 #157 hidden）: 500 Internal Server Error {"message":"Encountered unexpectedly high load when processing the request, please try again.","reason":"MODEL_TEMPORARILY_UNAVAILABLE"}"#
        ),
        Some("req_test_official_high_load"),
        Some("req_01official_high_load"),
        None,
    );

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(
        value.pointer("/error/type").and_then(|v| v.as_str()),
        Some("api_error")
    );
    let message = value
        .pointer("/error/message")
        .and_then(|v| v.as_str())
        .expect("error message");

    assert!(message.contains("Encountered unexpectedly high load"));
    assert!(message.contains("MODEL_TEMPORARILY_UNAVAILABLE"));
    assert!(message.contains("error ID: req_01official_high_load"));
    assert!(!message.contains("账号 #157"));
    assert!(!message.contains("hidden"));
}

#[tokio::test]
async fn official_kiro_upstream_message_with_kiro_term_is_masked() {
    let response = map_provider_error(
        anyhow::anyhow!(
            "{}",
            r#"流式 API 请求失败（账号 #7 secret-user）: 500 Internal Server Error {"message":"Kiro service rejected the request","reason":"MODEL_TEMPORARILY_UNAVAILABLE"}"#
        ),
        Some("req_test_official_kiro_term"),
        Some("req_01official_term"),
        None,
    );

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(
        value.pointer("/error/type").and_then(|v| v.as_str()),
        Some("api_error")
    );
    let message = value
        .pointer("/error/message")
        .and_then(|v| v.as_str())
        .expect("error message");

    assert!(message.contains(envelope::PUBLIC_PROCESSING_FAILED_MESSAGE));
    assert!(message.contains("error ID: req_01official_term"));
    assert_public_error_message_is_normalized(message);
}

#[tokio::test]
async fn opaque_400_bad_request_maps_to_invalid_request_not_gateway() {
    let response = map_provider_error(
        anyhow::anyhow!(
            "{}",
            "流式 API 请求失败（凭据 #6 ***，请求无效）: 400 Bad Request <failed to read response body: error decoding response body>"
        ),
        Some("req_test_opaque_bad_request"),
        None,
        None,
    );

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(
        value.pointer("/error/type").and_then(|v| v.as_str()),
        Some("invalid_request_error")
    );
    assert_eq!(
        value.pointer("/error/message").and_then(|v| v.as_str()),
        Some(UPSTREAM_INVALID_REQUEST_MESSAGE)
    );
}

#[tokio::test]
async fn model_unavailable_400_maps_to_public_model_unavailable_message() {
    let response = map_provider_error(
        anyhow::anyhow!(
            "{}",
            r#"流式 API 请求失败（账号 #7 secret-user，模型不可用）: 400 Bad Request {"message":"The requested model is not available for this endpoint. If this continues, contact the administrator with error ID: req_01raw"}"#
        ),
        Some("req_test_model_unavailable"),
        Some("req_01public_model_unavailable"),
        None,
    );

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(
        value.pointer("/error/type").and_then(|v| v.as_str()),
        Some("invalid_request_error")
    );
    let message = value
        .pointer("/error/message")
        .and_then(|v| v.as_str())
        .expect("error message");

    assert!(message.contains(envelope::PUBLIC_MODEL_UNAVAILABLE_MESSAGE));
    assert!(message.contains("error ID: req_01public_model_unavailable"));
    assert!(!message.contains("req_01raw"));
    assert_public_error_message_is_normalized(message);
}

#[tokio::test]
async fn no_available_credentials_error_uses_public_account_message() {
    let response = map_provider_error(
        anyhow::anyhow!("所有凭据均已禁用（0/26）"),
        Some("req_no_account"),
        None,
        None,
    );

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(
        value.pointer("/error/type").and_then(|v| v.as_str()),
        Some("api_error")
    );
    let message = value
        .pointer("/error/message")
        .and_then(|v| v.as_str())
        .expect("error message");
    assert_eq!(message, envelope::PUBLIC_ACCOUNT_UNAVAILABLE_MESSAGE);
    assert_public_error_message_is_normalized(message);
    assert!(message.contains("account"));
    assert!(!message.contains("0/26"));
    assert_eq!(value["request_id"], "req_no_account");
}

#[tokio::test]
async fn generic_provider_error_masks_raw_internal_details() {
    let response = map_provider_error(
        anyhow::anyhow!(
            "流式 API 请求失败（凭据 #37 shadow，请求失败）: 502 Bad Gateway raw upstream body"
        ),
        Some("req_generic_provider"),
        None,
        None,
    );

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(
        value.pointer("/error/type").and_then(|v| v.as_str()),
        Some("api_error")
    );
    let message = value
        .pointer("/error/message")
        .and_then(|v| v.as_str())
        .expect("error message");
    assert_eq!(message, envelope::PUBLIC_PROCESSING_FAILED_MESSAGE);
    assert_public_error_message_is_normalized(message);
    assert!(!message.contains("shadow"));
    assert_eq!(value["request_id"], "req_generic_provider");
}

#[tokio::test]
async fn provider_error_response_exposes_matching_public_error_id() {
    let response = map_provider_error(
        anyhow::anyhow!(
            "流式 API 请求失败（凭据 #37 shadow，请求失败）: 502 Bad Gateway raw upstream body"
        ),
        Some("req_public_error_id"),
        Some("req_01public_error_id"),
        None,
    );

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        response
            .headers()
            .get("x-error-id")
            .and_then(|value| value.to_str().ok()),
        Some("req_01public_error_id")
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(
        value.pointer("/error/type").and_then(|v| v.as_str()),
        Some("api_error")
    );
    let message = value
        .pointer("/error/message")
        .and_then(|v| v.as_str())
        .expect("error message");
    assert!(message.contains(envelope::PUBLIC_PROCESSING_FAILED_MESSAGE));
    assert!(message.contains("error ID: req_01public_error_id"));
    assert!(!message.contains("shadow"));
    assert_public_error_message_is_normalized(message);
}

fn assert_public_error_message_is_normalized(message: &str) {
    let lower = message.to_ascii_lowercase();
    for forbidden in [
        "kiro",
        "credential",
        "external pool",
        "external_pool",
        "fallback",
        "preflight",
        "upstream",
        "备用池",
        "外部池",
        "凭据",
    ] {
        assert!(
            !lower.contains(forbidden),
            "public message leaked internal term {forbidden:?}: {message}"
        );
    }
}

#[test]
fn local_prompt_cache_updates_even_when_context_tokens_are_estimated() {
    let prompt_cache = Arc::new(PromptCacheTracker::default());
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let payload = MessagesRequest {
        model: "claude-sonnet-4-6".to_string(),
        max_tokens: 16,
        messages: vec![Message {
            role: "user".to_string(),
            content: json!([
                {
                    "type": "text",
                    "text": "cacheable prompt block ".repeat(700),
                    "cache_control": {"type": "ephemeral"}
                }
            ]),
        }],
        stream: true,
        system: None,
        tools: None,
        tool_choice: None,
        thinking: None,
        output_config: None,
        metadata: None,
    };
    let profile = prompt_cache.build_profile(&payload, 4096);
    let usage_context = RequestUsageContext {
        recorder: usage_recorder,
        tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
        prompt_cache: prompt_cache.clone(),
        prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
        pricing_catalog: Arc::new(PricingCatalog::new()),
        request_id: "req_test".to_string(),
        error_id: "req_01test".to_string(),
        endpoint: "/v1/messages".to_string(),
        stream: true,
        model: payload.model.clone(),
        upstream_model: None,
        model_resolution_source: None,
        model_resolution_note: None,
        requested_max_tokens: 0,
        downstream_stop_reason: Arc::new(Mutex::new(None)),
        conversation_id: Some("session-a".to_string()),
        request_api_key_id: None,
        prompt_cache_scope_conversation_id: Some("session-a".to_string()),
        input_tokens: 4096,
        context_window_tokens: 200_000,
        prompt_cache_profile: profile.clone(),
        kiro_rs_tool_prompt_cache_plan: None,
        prompt_cache_route_namespace: None,
        prompt_cache_strategy_type: PromptCacheStrategyType::CurrentHighCache,
        simulation_mode: PromptCacheSimulationMode::HighCache,
        prompt_cache_target_read_ratio: 0.85,
        prompt_cache_token_scale: 1.0,
        prompt_cache_max_simulated_input_tokens: 0,
        prompt_cache_cap_jitter_min_tokens: 0,
        prompt_cache_cap_jitter_max_tokens: 0,
        prompt_cache_scale_min_input_tokens: 0,
        prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
        prompt_cache_bounds: PromptCacheBounds::default(),
        reported_cache_usage_policy: None,
        simulated_usage: None,
        simulated_source: Some(UsageSource::LocalPromptCache),
        payload_breakdown: None,
        payload_guard_report: None,
        error_metadata: Arc::new(Mutex::new(None)),
        route_subtype_override: None,
        fallback_reason: None,
        local_preflight: None,
        external_attempts: Vec::new(),
        started_at: Instant::now(),
        first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        capacity_weight_units: Arc::new(AtomicU32::new(1)),
        latency: RequestLatencyTraceState::new(),
    }
    .attach_credential(Some(1), None, false, false, Vec::new());
    let usage = CacheUsage {
        total_input_tokens: 4096,
        input_tokens: 128,
        output_tokens: 1,
        cache_creation_input_tokens: 3968,
        cache_read_input_tokens: 0,
        cache_creation_5m_input_tokens: 3968,
        cache_creation_1h_input_tokens: 0,
    };

    usage_context.record_success(usage, UsageSource::LocalPromptCache, true);

    let scope = PromptCacheScope {
        conversation_id: "session-a".to_string(),
        route_namespace: None,
    };
    let second = prompt_cache.compute(Some(scope), profile.as_ref(), 0.85);
    assert!(second.cache_read_input_tokens > 0);
}

#[test]
fn high_cache_zero_metadata_fallback_updates_local_prompt_cache() {
    let prompt_cache = Arc::new(PromptCacheTracker::default());
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let mut payload = MessagesRequest {
        model: "claude-sonnet-4-6".to_string(),
        max_tokens: 16,
        messages: vec![Message {
            role: "user".to_string(),
            content: json!("hello"),
        }],
        stream: true,
        system: Some(vec![SystemMessage {
            text: "cacheable prompt block ".repeat(700),
            cache_control: None,
        }]),
        tools: None,
        tool_choice: None,
        thinking: None,
        output_config: None,
        metadata: None,
    };
    let profile = prompt_cache.build_high_cache_profile(&payload, 4096);
    let usage_context = RequestUsageContext {
        recorder: usage_recorder,
        tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
        prompt_cache: prompt_cache.clone(),
        prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
        pricing_catalog: Arc::new(PricingCatalog::new()),
        request_id: "req_high_cache".to_string(),
        error_id: "req_01high_cache".to_string(),
        endpoint: "/v1/messages".to_string(),
        stream: true,
        model: payload.model.clone(),
        upstream_model: None,
        model_resolution_source: None,
        model_resolution_note: None,
        requested_max_tokens: 0,
        downstream_stop_reason: Arc::new(Mutex::new(None)),
        conversation_id: Some("session-high-cache".to_string()),
        request_api_key_id: None,
        prompt_cache_scope_conversation_id: Some("session-high-cache".to_string()),
        input_tokens: 4096,
        context_window_tokens: 200_000,
        prompt_cache_profile: profile.clone(),
        kiro_rs_tool_prompt_cache_plan: None,
        prompt_cache_route_namespace: None,
        prompt_cache_strategy_type: PromptCacheStrategyType::CurrentHighCache,
        simulation_mode: PromptCacheSimulationMode::HighCache,
        prompt_cache_target_read_ratio: 0.95,
        prompt_cache_token_scale: 1.0,
        prompt_cache_max_simulated_input_tokens: 0,
        prompt_cache_cap_jitter_min_tokens: 0,
        prompt_cache_cap_jitter_max_tokens: 0,
        prompt_cache_scale_min_input_tokens: 0,
        prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
        prompt_cache_bounds: PromptCacheBounds::default(),
        reported_cache_usage_policy: None,
        simulated_usage: Some(cache::CacheSimulation {
            cache_creation_input_tokens: 3968,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 3968,
            cache_creation_1h_input_tokens: 0,
            target_cache_ratio: Some(0.95),
            amplification: None,
            split_cached_input: false,
            ..Default::default()
        }),
        simulated_source: Some(UsageSource::LocalPromptCache),
        payload_breakdown: None,
        payload_guard_report: None,
        error_metadata: Arc::new(Mutex::new(None)),
        route_subtype_override: None,
        fallback_reason: None,
        local_preflight: None,
        external_attempts: Vec::new(),
        started_at: Instant::now(),
        first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        capacity_weight_units: Arc::new(AtomicU32::new(1)),
        latency: RequestLatencyTraceState::new(),
    }
    .attach_credential(Some(1), None, false, false, Vec::new());
    let metadata = MetadataTokenUsage {
        uncached_input_tokens: 4096,
        output_tokens: 1,
        total_tokens: 4097,
        cache_read_input_tokens: 0,
        cache_write_input_tokens: 0,
    };
    let usage = cache::build_usage_with_simulation_policy(
        Some(&metadata),
        4096,
        1,
        usage_context.request.simulated_usage,
        true,
    );

    let source = usage_context.usage_source(&usage, Some(&metadata), false);
    assert_eq!(source, UsageSource::LocalPromptCache);
    usage_context.record_success(usage, source, false);

    let scope = PromptCacheScope {
        conversation_id: "session-high-cache".to_string(),
        route_namespace: None,
    };
    payload.messages.extend([
        Message {
            role: "assistant".to_string(),
            content: json!("ready"),
        },
        Message {
            role: "user".to_string(),
            content: json!("continue the same session"),
        },
    ]);
    let second_profile = prompt_cache.build_high_cache_profile(&payload, 8192);
    let second = prompt_cache.compute(Some(scope), second_profile.as_ref(), 0.95);
    assert!(second.cache_read_input_tokens > 0);
}

#[test]
fn high_cache_missing_metadata_fallback_conversation_reads_second_turn() {
    let prompt_cache = Arc::new(PromptCacheTracker::default());
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        true,
        usage_recorder,
        prompt_cache,
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::HighCache,
        0.95,
        CompatProfile::ClaudeCode,
        false,
    );
    let first_payload = MessagesRequest {
        model: "claude-sonnet-4-6".to_string(),
        max_tokens: 16,
        messages: vec![Message {
            role: "user".to_string(),
            content: json!("start high cache session"),
        }],
        stream: false,
        system: Some(vec![SystemMessage {
            text: "stable high cache system prompt ".repeat(700),
            cache_control: None,
        }]),
        tools: None,
        tool_choice: None,
        thinking: None,
        output_config: None,
        metadata: None,
    };
    let first_conversation_id =
        extract_stable_conversation_id(&first_payload).expect("fallback id");
    let first_context = prepare_usage_context(
        &state,
        RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/v1/messages"),
        "/v1/messages",
        false,
        &first_payload,
        None,
        Some(first_conversation_id.clone()),
        Some(first_conversation_id.clone()),
        4096,
    );
    let first_usage = attach_test_credential_usage(first_context, 1);
    let first_usage_body = cache::build_usage_with_simulation_policy(
        None,
        4096,
        1,
        first_usage.request.simulated_usage,
        true,
    );
    assert!(first_usage_body.cache_creation_input_tokens > 0);
    assert_eq!(first_usage_body.cache_read_input_tokens, 0);
    let first_metadata = MetadataTokenUsage {
        uncached_input_tokens: 4096,
        output_tokens: 1,
        total_tokens: 4097,
        cache_read_input_tokens: 0,
        cache_write_input_tokens: 0,
    };
    let first_source = first_usage.usage_source(&first_usage_body, Some(&first_metadata), false);
    assert_eq!(first_source, UsageSource::LocalPromptCache);
    first_usage.record_success(first_usage_body, first_source, false);

    let second_payload = MessagesRequest {
        model: "claude-sonnet-4-6".to_string(),
        max_tokens: 16,
        messages: vec![
            Message {
                role: "user".to_string(),
                content: json!("start high cache session"),
            },
            Message {
                role: "assistant".to_string(),
                content: json!("ready"),
            },
            Message {
                role: "user".to_string(),
                content: json!("continue the same session"),
            },
        ],
        stream: false,
        system: Some(vec![SystemMessage {
            text: "stable high cache system prompt ".repeat(700),
            cache_control: None,
        }]),
        tools: None,
        tool_choice: None,
        thinking: None,
        output_config: None,
        metadata: None,
    };
    let second_conversation_id =
        extract_stable_conversation_id(&second_payload).expect("fallback id");
    assert_eq!(first_conversation_id, second_conversation_id);

    let second_context = prepare_usage_context(
        &state,
        RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/v1/messages"),
        "/v1/messages",
        false,
        &second_payload,
        None,
        Some(second_conversation_id.clone()),
        Some(second_conversation_id),
        8192,
    );
    let second_usage = attach_test_credential_usage(second_context, 1);
    let second_usage_body = cache::build_usage_with_simulation_policy(
        None,
        8192,
        1,
        second_usage.request.simulated_usage,
        true,
    );

    assert!(second_usage_body.cache_read_input_tokens > 0);
    assert_eq!(
        second_usage.usage_source(&second_usage_body, None, false),
        UsageSource::LocalPromptCache
    );
}

#[test]
fn kiro_rs_tool_route_strategy_misses_first_then_reads_after_success() {
    let prompt_cache = Arc::new(PromptCacheTracker::default());
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let mut cache_policy = CachePolicyConfig::default();
    cache_policy.path_overrides.insert(
        "/kiro/v1/messages".to_string(),
        CacheRoutePolicyPatch {
            cache_type: Some(PromptCacheStrategyType::KiroRsTool),
            ..CacheRoutePolicyPatch::default()
        },
    );
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        true,
        usage_recorder,
        prompt_cache,
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::Disabled,
        0.95,
        CompatProfile::ClaudeCode,
        false,
    )
    .with_cache_policy(cache_policy);
    let session_id = "8bb5523b-ec7c-4540-a9ca-beb6d79f1552";
    let mut first_payload = MessagesRequest {
        model: "claude-sonnet-4-6".to_string(),
        max_tokens: 16,
        messages: vec![Message {
            role: "user".to_string(),
            content: json!("start kiro strategy session"),
        }],
        stream: false,
        system: Some(vec![SystemMessage {
            text: "stable kiro strategy system prompt ".repeat(700),
            cache_control: Some(json!({"type": "ephemeral"})),
        }]),
        tools: None,
        tool_choice: None,
        thinking: None,
        output_config: None,
        metadata: Some(Metadata {
            user_id: Some(format!("user_test_account__session_{session_id}")),
        }),
    };
    let cache_route =
        RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/kiro/v1/messages");
    assert_eq!(
        cache_route.policy.cache_type,
        PromptCacheStrategyType::KiroRsTool
    );
    let first_context = prepare_usage_context(
        &state,
        cache_route,
        "/kiro/v1/messages",
        false,
        &first_payload,
        None,
        Some(session_id.to_string()),
        prompt_cache_scope_conversation_id(
            PromptCacheStrategyType::KiroRsTool,
            PromptCacheSimulationMode::Disabled,
            &first_payload,
        ),
        4096,
    );

    assert_eq!(
        first_context.simulation_mode,
        PromptCacheSimulationMode::Disabled
    );
    assert_eq!(
        first_context.prompt_cache_scope_conversation_id.as_deref(),
        Some(session_id)
    );
    assert!(first_context.kiro_rs_tool_prompt_cache_plan.is_some());
    let first_simulation = first_context
        .simulated_usage
        .expect("first kiro request should project cache creation");
    assert!(first_simulation.cache_creation_input_tokens > 0);
    assert_eq!(first_simulation.cache_read_input_tokens, 0);
    let first_usage = first_context.attach_credential(Some(1), None, false, false, Vec::new());
    let first_usage_body = cache::build_usage_with_simulation_policy(
        None,
        4096,
        1,
        first_usage.request.simulated_usage,
        true,
    );
    assert!(first_usage_body.cache_creation_input_tokens > 0);
    assert_eq!(first_usage_body.cache_read_input_tokens, 0);
    assert!((32..=4_096).contains(&first_usage_body.input_tokens));
    let first_source = first_usage.usage_source(&first_usage_body, None, false);
    assert_eq!(first_source, UsageSource::LocalPromptCache);
    first_usage.record_success(first_usage_body, first_source, false);

    first_payload.messages.extend([
        Message {
            role: "assistant".to_string(),
            content: json!("ready"),
        },
        Message {
            role: "user".to_string(),
            content: json!("continue the same kiro strategy session"),
        },
    ]);
    let second_context = prepare_usage_context(
        &state,
        RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/kiro/v1/messages"),
        "/kiro/v1/messages",
        false,
        &first_payload,
        None,
        Some(session_id.to_string()),
        prompt_cache_scope_conversation_id(
            PromptCacheStrategyType::KiroRsTool,
            PromptCacheSimulationMode::Disabled,
            &first_payload,
        ),
        8192,
    );
    assert!(second_context.kiro_rs_tool_prompt_cache_plan.is_some());
    let second_simulation = second_context
        .simulated_usage
        .expect("second kiro request should project a cache read");
    assert!(second_simulation.cache_read_input_tokens > 0);
    let second_usage =
        cache::build_usage_with_simulation_policy(None, 8192, 1, Some(second_simulation), true);
    assert!(second_usage.cache_read_input_tokens > 0);
    assert!((32..=4_096).contains(&second_usage.input_tokens));
    assert_eq!(
        second_usage.input_tokens
            + second_usage.cache_creation_input_tokens
            + second_usage.cache_read_input_tokens,
        second_usage.total_input_tokens
    );
    assert_eq!(second_usage.total_input_tokens, 8192);
}

#[test]
fn kiro_rs_tool_route_strategy_commits_without_credential_id() {
    let prompt_cache = Arc::new(PromptCacheTracker::default());
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let mut cache_policy = CachePolicyConfig::default();
    cache_policy.path_overrides.insert(
        "/kiro/v1/messages".to_string(),
        CacheRoutePolicyPatch {
            cache_type: Some(PromptCacheStrategyType::KiroRsTool),
            ..CacheRoutePolicyPatch::default()
        },
    );
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        true,
        usage_recorder,
        prompt_cache,
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::Disabled,
        0.95,
        CompatProfile::ClaudeCode,
        false,
    )
    .with_cache_policy(cache_policy);
    let session_id = "8bb5523b-ec7c-4540-a9ca-beb6d79f1552";
    let mut payload = MessagesRequest {
        model: "claude-sonnet-4-6".to_string(),
        max_tokens: 16,
        messages: vec![Message {
            role: "user".to_string(),
            content: json!("start kiro no credential session"),
        }],
        stream: false,
        system: Some(vec![SystemMessage {
            text: "stable kiro no credential system prompt ".repeat(700),
            cache_control: Some(json!({"type": "ephemeral"})),
        }]),
        tools: None,
        tool_choice: None,
        thinking: None,
        output_config: None,
        metadata: Some(Metadata {
            user_id: Some(format!("user_test_account__session_{session_id}")),
        }),
    };

    let first_context = prepare_usage_context(
        &state,
        RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/kiro/v1/messages"),
        "/kiro/v1/messages",
        false,
        &payload,
        None,
        Some(session_id.to_string()),
        prompt_cache_scope_conversation_id(
            PromptCacheStrategyType::KiroRsTool,
            PromptCacheSimulationMode::Disabled,
            &payload,
        ),
        4096,
    );
    let first_usage_body = cache::build_usage_with_simulation_policy(
        None,
        4096,
        1,
        first_context.simulated_usage,
        true,
    );
    let first_usage = first_context.attach_credential(None, None, false, false, Vec::new());
    let first_source = first_usage.usage_source(&first_usage_body, None, false);
    assert_eq!(first_source, UsageSource::LocalPromptCache);
    first_usage.record_success(first_usage_body, first_source, false);

    payload.messages.extend([
        Message {
            role: "assistant".to_string(),
            content: json!("ready"),
        },
        Message {
            role: "user".to_string(),
            content: json!("continue no credential session"),
        },
    ]);
    let second_context = prepare_usage_context(
        &state,
        RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/kiro/v1/messages"),
        "/kiro/v1/messages",
        false,
        &payload,
        None,
        Some(session_id.to_string()),
        prompt_cache_scope_conversation_id(
            PromptCacheStrategyType::KiroRsTool,
            PromptCacheSimulationMode::Disabled,
            &payload,
        ),
        8192,
    );
    let second_simulation = second_context
        .simulated_usage
        .expect("second kiro request should read cache without credential id");
    assert!(second_simulation.cache_read_input_tokens > 0);
    let second_usage =
        cache::build_usage_with_simulation_policy(None, 8192, 1, Some(second_simulation), true);
    assert!((32..=4_096).contains(&second_usage.input_tokens));
}

#[test]
fn disabled_prompt_cache_does_not_simulate_without_stable_conversation_id() {
    let prompt_cache = Arc::new(PromptCacheTracker::default());
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        true,
        usage_recorder,
        prompt_cache.clone(),
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::Disabled,
        0.95,
        CompatProfile::ClaudeCode,
        false,
    );
    let payload = MessagesRequest {
        model: "claude-sonnet-4-6".to_string(),
        max_tokens: 16,
        messages: vec![Message {
            role: "user".to_string(),
            content: json!([
                {
                    "type": "text",
                    "text": "cacheable prompt block ".repeat(700),
                    "cache_control": {"type": "ephemeral"}
                }
            ]),
        }],
        stream: true,
        system: None,
        tools: None,
        tool_choice: None,
        thinking: None,
        output_config: None,
        metadata: None,
    };
    let (simulation, source) =
        build_simulated_usage(PromptCacheSimulationMode::Disabled, None, None);

    assert!(simulation.is_none());
    assert!(source.is_none());

    let context = prepare_usage_context(
        &state,
        RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/v1/messages"),
        "/v1/messages",
        true,
        &payload,
        None,
        Some("random-conversation".to_string()),
        prompt_cache_scope_conversation_id(
            PromptCacheStrategyType::CurrentHighCache,
            state.prompt_cache_simulation_mode,
            &payload,
        ),
        4096,
    );
    assert!(context.prompt_cache_profile.is_none());
    assert!(context.prompt_cache_scope_conversation_id.is_none());

    let credential_usage = attach_test_credential_usage(context, 1);
    assert!(credential_usage.request.simulated_usage.is_none());
    assert!(credential_usage.request.simulated_source.is_none());
}

#[test]
fn builtin_na_path_does_not_build_local_profile_or_reporting_policy() {
    let prompt_cache = Arc::new(PromptCacheTracker::default());
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        true,
        usage_recorder,
        prompt_cache,
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::Disabled,
        0.95,
        CompatProfile::ClaudeCode,
        false,
    );
    let payload = MessagesRequest {
        model: "claude-sonnet-4-6".to_string(),
        max_tokens: 16,
        messages: vec![Message {
            role: "user".to_string(),
            content: json!([
                {
                    "type": "text",
                    "text": "cacheable prompt block ".repeat(700),
                    "cache_control": {"type": "ephemeral"}
                }
            ]),
        }],
        stream: false,
        system: None,
        tools: None,
        tool_choice: None,
        thinking: None,
        output_config: None,
        metadata: None,
    };

    let context = prepare_usage_context(
        &state,
        RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/na/v1/messages"),
        "/na/v1/messages",
        false,
        &payload,
        None,
        Some("conversation-id".to_string()),
        prompt_cache_scope_conversation_id(
            PromptCacheStrategyType::NoCache,
            PromptCacheSimulationMode::Disabled,
            &payload,
        ),
        4096,
    );

    assert_eq!(
        context.prompt_cache_strategy_type,
        PromptCacheStrategyType::NoCache
    );
    assert_eq!(context.simulation_mode, PromptCacheSimulationMode::Disabled);
    assert!(context.prompt_cache_profile.is_none());
    assert!(context.kiro_rs_tool_prompt_cache_plan.is_none());
    assert!(context.prompt_cache_scope_conversation_id.is_none());
    assert_eq!(context.prompt_cache_route_namespace, None);
    assert!(context.simulated_usage.is_none());
    assert!(context.simulated_source.is_none());
    assert!(context.reported_cache_usage_policy.is_none());
}

#[test]
fn no_cache_route_does_not_build_cache_profile_plan_or_shape_reporting() {
    let prompt_cache = Arc::new(PromptCacheTracker::default());
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let mut cache_policy = CachePolicyConfig::default();
    cache_policy.path_overrides.insert(
        "/plain".to_string(),
        CacheRoutePolicyPatch {
            cache_type: Some(PromptCacheStrategyType::NoCache),
            simulation: Some(CacheSimulationPolicyPatch {
                enabled: Some(true),
                ..CacheSimulationPolicyPatch::default()
            }),
            creation_control: Some(PromptCacheCreationControlConfig::default()),
            reported_usage: Some(ReportedUsagePathPolicy {
                input: ReportedUsageFieldPolicy::sample_input_max(32),
                ..ReportedUsagePathPolicy::default()
            }),
            cache_point: Some(CachePointPolicyPatch {
                enabled: Some(true),
                ..CachePointPolicyPatch::default()
            }),
            ..CacheRoutePolicyPatch::default()
        },
    );
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        true,
        usage_recorder,
        prompt_cache,
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::HighCache,
        0.95,
        CompatProfile::ClaudeCode,
        false,
    )
    .with_cache_policy(cache_policy);
    let payload = MessagesRequest {
        model: "claude-sonnet-4-6".to_string(),
        max_tokens: 16,
        messages: vec![Message {
            role: "user".to_string(),
            content: json!("plain request should not enter prompt cache"),
        }],
        stream: false,
        system: Some(vec![SystemMessage {
            text: "cacheable prompt block ".repeat(700),
            cache_control: Some(json!({"type": "ephemeral"})),
        }]),
        tools: None,
        tool_choice: None,
        thinking: None,
        output_config: None,
        metadata: None,
    };

    let cache_route =
        RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/plain/v1/messages");
    assert_eq!(
        cache_route.policy.cache_type,
        PromptCacheStrategyType::NoCache
    );
    assert_eq!(cache_route.namespace, None);
    assert_eq!(
        prompt_cache_simulation_mode_for_policy(&cache_route.policy),
        PromptCacheSimulationMode::Disabled
    );
    assert_eq!(
        prompt_cache_converter_mode_for_policy(&cache_route.policy),
        PromptCacheSimulationMode::Disabled
    );
    assert!(!cache_route.policy.cache_point.enabled);

    let context = prepare_usage_context(
        &state,
        cache_route,
        "/plain/v1/messages",
        false,
        &payload,
        None,
        Some("plain-session".to_string()),
        prompt_cache_scope_conversation_id(
            PromptCacheStrategyType::NoCache,
            PromptCacheSimulationMode::Disabled,
            &payload,
        ),
        4096,
    );

    assert_eq!(
        context.prompt_cache_strategy_type,
        PromptCacheStrategyType::NoCache
    );
    assert_eq!(context.simulation_mode, PromptCacheSimulationMode::Disabled);
    assert!(context.prompt_cache_profile.is_none());
    assert!(context.kiro_rs_tool_prompt_cache_plan.is_none());
    assert!(context.prompt_cache_scope_conversation_id.is_none());
    assert_eq!(context.prompt_cache_route_namespace, None);
    assert!(context.simulated_usage.is_none());
    assert!(context.simulated_source.is_none());
    assert!(context.reported_cache_usage_policy.is_none());

    let upstream_raw = CacheUsage {
        total_input_tokens: 4_165,
        input_tokens: 4_165,
        output_tokens: 7,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        cache_creation_5m_input_tokens: 0,
        cache_creation_1h_input_tokens: 0,
    };
    let reported = context.reported_usage_for_downstream_with_raw(
        cache::CacheUsage {
            total_input_tokens: 125_000,
            input_tokens: 100_000,
            output_tokens: 7,
            cache_creation_input_tokens: 20_000,
            cache_read_input_tokens: 5_000,
            cache_creation_5m_input_tokens: 20_000,
            cache_creation_1h_input_tokens: 0,
        },
        UsageSource::UpstreamMetadata,
        raw_usage_to_reported_raw(upstream_raw),
    );
    assert_eq!(reported.input_tokens, 4_165);
    assert_eq!(reported.output_tokens, 7);
    assert_eq!(reported.cache_creation_input_tokens, 0);
    assert_eq!(reported.cache_read_input_tokens, 0);
}

#[test]
fn no_cache_disabled_reported_usage_preserves_upstream_metadata_usage() {
    let prompt_cache = Arc::new(PromptCacheTracker::default());
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let mut reported_usage = ReportedUsageConfig::default();
    reported_usage
        .path_overrides
        .insert("/na".to_string(), ReportedUsagePathPolicy::disabled());
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        true,
        usage_recorder,
        prompt_cache,
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::HighCache,
        0.95,
        CompatProfile::ClaudeCode,
        false,
    )
    .with_reported_usage(reported_usage);
    let payload = MessagesRequest {
        model: "claude-sonnet-4-6".to_string(),
        max_tokens: 16,
        messages: vec![Message {
            role: "user".to_string(),
            content: json!("no-cache request should preserve upstream metadata usage"),
        }],
        stream: true,
        system: Some(vec![SystemMessage {
            text: "large no-cache system prompt ".repeat(700),
            cache_control: Some(json!({"type": "ephemeral"})),
        }]),
        tools: None,
        tool_choice: None,
        thinking: None,
        output_config: None,
        metadata: None,
    };

    let context = prepare_usage_context(
        &state,
        RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/na/v1/messages"),
        "/na/v1/messages",
        true,
        &payload,
        None,
        Some("na-disabled-reported".to_string()),
        prompt_cache_scope_conversation_id(
            PromptCacheStrategyType::NoCache,
            PromptCacheSimulationMode::Disabled,
            &payload,
        ),
        32_000,
    );

    assert_eq!(
        context.prompt_cache_strategy_type,
        PromptCacheStrategyType::NoCache
    );
    assert!(context.reported_cache_usage_policy.is_none());

    let context_usage = CacheUsage {
        total_input_tokens: 32_000,
        input_tokens: 32_000,
        output_tokens: 3,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        cache_creation_5m_input_tokens: 0,
        cache_creation_1h_input_tokens: 0,
    };
    let upstream_raw = CacheUsage {
        total_input_tokens: 10,
        input_tokens: 10,
        output_tokens: 3,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        cache_creation_5m_input_tokens: 0,
        cache_creation_1h_input_tokens: 0,
    };

    let reported = context.reported_usage_for_downstream_with_raw(
        context_usage,
        UsageSource::UpstreamMetadata,
        raw_usage_to_reported_raw(upstream_raw),
    );

    assert_eq!(reported.input_tokens, 10);
    assert_eq!(reported.output_tokens, 3);
    assert_eq!(reported.cache_read_input_tokens, 0);
    assert_eq!(reported.cache_creation_input_tokens, 0);
}

#[test]
fn no_cache_canonical_record_keeps_upstream_raw_when_default_reporting_exists() {
    let prompt_cache = Arc::new(PromptCacheTracker::default());
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        true,
        usage_recorder,
        prompt_cache,
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::HighCache,
        0.95,
        CompatProfile::ClaudeCode,
        false,
    );
    let payload = MessagesRequest {
        model: "claude-sonnet-4-6".to_string(),
        max_tokens: 16,
        messages: vec![Message {
            role: "user".to_string(),
            content: json!("no-cache request should not be expanded by record shaping"),
        }],
        stream: true,
        system: Some(vec![SystemMessage {
            text: "large no-cache system prompt ".repeat(700),
            cache_control: Some(json!({"type": "ephemeral"})),
        }]),
        tools: None,
        tool_choice: None,
        thinking: None,
        output_config: None,
        metadata: None,
    };

    let context = prepare_usage_context(
        &state,
        RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/na/v1/messages"),
        "/na/v1/messages",
        true,
        &payload,
        None,
        Some("na-default-reported".to_string()),
        prompt_cache_scope_conversation_id(
            PromptCacheStrategyType::NoCache,
            PromptCacheSimulationMode::Disabled,
            &payload,
        ),
        32_000,
    );

    assert_eq!(
        context.prompt_cache_strategy_type,
        PromptCacheStrategyType::NoCache
    );
    assert!(context.reported_cache_usage_policy.is_none());

    let context_usage = CacheUsage {
        total_input_tokens: 32_000,
        input_tokens: 32_000,
        output_tokens: 3,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        cache_creation_5m_input_tokens: 0,
        cache_creation_1h_input_tokens: 0,
    };
    let upstream_raw = CacheUsage {
        total_input_tokens: 10,
        input_tokens: 10,
        output_tokens: 3,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        cache_creation_5m_input_tokens: 0,
        cache_creation_1h_input_tokens: 0,
    };
    let credential_context = context.attach_credential(Some(1), None, false, false, Vec::new());

    let reported = credential_context.canonical_reported_usage_for_success_with_raw(
        context_usage,
        UsageSource::UpstreamMetadata,
        upstream_raw,
    );

    assert_eq!(reported.input_tokens, 10);
    assert_eq!(reported.output_tokens, 3);
    assert_eq!(reported.cache_read_input_tokens, 0);
    assert_eq!(reported.cache_creation_input_tokens, 0);
}

fn attach_test_credential_usage(
    mut usage_context: RequestUsageContext,
    credential_id: u64,
) -> CredentialUsageContext {
    let scope = usage_context
        .prompt_cache_scope_conversation_id
        .as_ref()
        .map(|conversation_id| {
            let _ = credential_id;
            PromptCacheScope::new(conversation_id.clone(), None)
        });
    let prompt_usage = usage_context.prompt_cache.compute_with_bounds(
        scope,
        usage_context.prompt_cache_profile.as_ref(),
        usage_context.prompt_cache_target_read_ratio,
        usage_context.prompt_cache_bounds,
    );
    usage_context.simulated_usage =
        cache::CacheSimulation::from_prompt_cache_with_ratio_and_amplification(
            prompt_usage,
            usage_context.prompt_cache_target_read_ratio,
            usage_context.cache_amplification(),
        );
    usage_context.simulated_source = usage_context
        .simulated_usage
        .map(|_| UsageSource::LocalPromptCache);
    usage_context.attach_credential(Some(credential_id), None, false, false, Vec::new())
}

#[test]
fn strict_profile_suppresses_proxy_warning_header() {
    let prompt_cache = Arc::new(PromptCacheTracker::default());
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        true,
        usage_recorder,
        prompt_cache,
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::Disabled,
        0.85,
        CompatProfile::AnthropicStrict,
        true,
    );

    assert!(!should_expose_proxy_warnings(
        &RequestRuntimeConfig::from_app_state(&state)
    ));
}

#[test]
fn external_fallback_classifier_rejects_request_errors() {
    let config = ExternalPoolsConfig::default();

    assert_eq!(
        classify_local_error_for_external_fallback(
            r#"400 Bad Request {"message":"Input is too long.","reason":"CONTENT_LENGTH_EXCEEDS_THRESHOLD"}"#,
            &[],
            &config,
        ),
        None
    );
    assert_eq!(
        classify_local_error_for_external_fallback(
            "JSON schema is invalid for tool input_schema",
            &[],
            &config,
        ),
        None
    );

    let attempts = vec![KiroCredentialAttempt::new(
        0,
        1,
        None,
        Some(StatusCode::BAD_REQUEST),
        "fail",
        Some("client_error"),
        Some("bad request"),
        10,
    )];
    assert_eq!(
        classify_local_error_for_external_fallback("429 Too Many Requests", &attempts, &config),
        None
    );
}

#[test]
fn external_fallback_classifier_allows_capacity_and_transient_errors() {
    let config = ExternalPoolsConfig::default();

    assert_eq!(
        classify_local_error_for_external_fallback(
            "本地凭据调度容量暂不可用，并发槽位已满",
            &[],
            &config,
        )
        .as_deref(),
        Some("local_capacity_exhausted")
    );
    assert_eq!(
        classify_local_error_for_external_fallback("429 Too Many Requests", &[], &config)
            .as_deref(),
        Some("local_transient_exhausted")
    );
}

#[test]
fn external_fallback_classifier_can_use_retry_stage_attempts_after_payload_guard_retry() {
    let config = ExternalPoolsConfig::default();
    let prior_too_long_attempt = KiroCredentialAttempt::new(
        0,
        63,
        Some("account@example.com".to_string()),
        Some(StatusCode::BAD_REQUEST),
        "fail",
        Some("client_error"),
        Some(
            r#"400 Bad Request {"message":"Input is too long.","reason":"CONTENT_LENGTH_EXCEEDS_THRESHOLD"}"#,
        ),
        1200,
    );
    let capacity_message = "本地账号调度容量暂不可用（可用: 7/29, 临时可调度: 0, global_credential_max_concurrent_requests=10, effective_credential_max_concurrent_requests=50, retry_after_secs=1）";

    let diagnostic_attempts =
        merge_credential_attempts(vec![prior_too_long_attempt.clone()], Vec::new());
    assert_eq!(
        classify_local_error_for_external_fallback(capacity_message, &diagnostic_attempts, &config,),
        None
    );

    let retry_stage_attempts = Vec::new();
    assert_eq!(
        classify_local_error_for_external_fallback(
            capacity_message,
            &retry_stage_attempts,
            &config,
        )
        .as_deref(),
        Some("local_capacity_exhausted")
    );

    let retry_bad_request_attempt = vec![KiroCredentialAttempt::new(
        0,
        64,
        Some("retry@example.com".to_string()),
        Some(StatusCode::BAD_REQUEST),
        "fail",
        Some("client_error"),
        Some("bad request"),
        10,
    )];
    assert_eq!(
        classify_local_error_for_external_fallback(
            capacity_message,
            &retry_bad_request_attempt,
            &config,
        ),
        None
    );
}

#[test]
fn external_fallback_classifier_respects_scheduler_fallback_toggles() {
    let mut config = ExternalPoolsConfig {
        fallback_on_local_capacity_exhausted: false,
        fallback_on_scheduler_redis_degraded: false,
        ..Default::default()
    };

    assert_eq!(
        classify_local_error_for_external_fallback(
            "本地凭据调度容量暂不可用，并发槽位已满",
            &[],
            &config,
        ),
        None
    );
    assert_eq!(
        classify_local_error_for_external_fallback(
            "本地账号调度容量暂不可用（Redis 调度协调状态不可用，retry_after_secs=2）",
            &[],
            &config,
        ),
        None
    );

    config = ExternalPoolsConfig::default();
    assert_eq!(
        classify_local_error_for_external_fallback(
            "本地账号调度容量暂不可用（Redis 调度协调状态不可用，retry_after_secs=2）",
            &[],
            &config,
        )
        .as_deref(),
        Some("local_scheduler_redis_degraded")
    );
    config.fallback_on_scheduler_redis_degraded = false;
    assert_eq!(
        classify_local_error_for_external_fallback(
            "本地账号调度容量暂不可用（Redis 调度协调状态不可用，retry_after_secs=2）",
            &[],
            &config,
        ),
        None
    );

    config = ExternalPoolsConfig::default();
    config.fallback_on_local_transient_exhausted = false;
    assert_eq!(
        classify_local_error_for_external_fallback("429 Too Many Requests", &[], &config),
        None
    );
    assert_eq!(
        classify_local_error_for_external_fallback(
            "upstream server_error",
            &[KiroCredentialAttempt::new(
                0,
                1,
                None,
                Some(StatusCode::BAD_GATEWAY),
                "retry",
                Some("server_error"),
                Some("502"),
                10,
            )],
            &config,
        ),
        None
    );

    config = ExternalPoolsConfig::default();
    config.fallback_on_no_available_credentials = false;
    assert_eq!(
        classify_local_error_for_external_fallback("所有凭据均已禁用（0/2）", &[], &config),
        None
    );

    config.fallback_on_no_available_credentials = true;
    assert_eq!(
        classify_local_error_for_external_fallback("所有凭据均已禁用（0/2）", &[], &config)
            .as_deref(),
        Some("no_available_credentials")
    );
}

#[test]
fn local_pool_preflight_reason_respects_scheduler_fallback_toggles() {
    let mut config = ExternalPoolsConfig::default();

    assert!(local_pool_capacity_fail_fast_enabled(&config));
    assert_eq!(
        local_pool_route_fallback_reason(LocalPoolRouteStateKind::NoCredentials, &config),
        Some("local_no_credentials")
    );
    assert_eq!(
        local_pool_route_fallback_reason(LocalPoolRouteStateKind::AllDisabled, &config),
        Some("local_all_disabled")
    );
    assert_eq!(
        local_pool_route_fallback_reason(LocalPoolRouteStateKind::ProxyBlocked, &config),
        Some("local_proxy_blocked")
    );
    assert_eq!(
        local_pool_route_fallback_reason(LocalPoolRouteStateKind::AllCoolingDown, &config),
        Some("local_all_cooling_down")
    );
    assert_eq!(
        local_pool_route_fallback_reason(LocalPoolRouteStateKind::CapacityFull, &config),
        Some("local_capacity_full")
    );
    assert_eq!(
        local_pool_route_fallback_reason(LocalPoolRouteStateKind::SchedulerRedisDegraded, &config),
        Some("local_scheduler_redis_degraded")
    );
    assert_eq!(
        local_pool_route_fallback_reason(LocalPoolRouteStateKind::NoModelCompatible, &config),
        None
    );

    config.fallback_on_no_available_credentials = false;
    assert_eq!(
        local_pool_route_fallback_reason(LocalPoolRouteStateKind::NoCredentials, &config),
        None
    );
    assert_eq!(
        local_pool_route_fallback_reason(LocalPoolRouteStateKind::AllDisabled, &config),
        None
    );
    assert_eq!(
        local_pool_route_fallback_reason(LocalPoolRouteStateKind::ProxyBlocked, &config),
        None
    );

    config = ExternalPoolsConfig::default();
    config.fallback_on_local_transient_exhausted = false;
    assert_eq!(
        local_pool_route_fallback_reason(LocalPoolRouteStateKind::AllCoolingDown, &config),
        None
    );

    config = ExternalPoolsConfig::default();
    config.fallback_on_local_capacity_exhausted = false;
    assert!(!local_pool_capacity_fail_fast_enabled(&config));
    assert_eq!(
        local_pool_route_fallback_reason(LocalPoolRouteStateKind::CapacityFull, &config),
        None
    );

    config = ExternalPoolsConfig::default();
    config.fallback_on_scheduler_redis_degraded = false;
    assert_eq!(
        local_pool_route_fallback_reason(LocalPoolRouteStateKind::SchedulerRedisDegraded, &config),
        None
    );

    config = ExternalPoolsConfig::default();
    config.fallback_on_unsupported_model = true;
    assert_eq!(
        local_pool_route_fallback_reason(LocalPoolRouteStateKind::NoModelCompatible, &config),
        Some("local_no_model_compatible")
    );

    config.local_pool_preflight_enabled = false;
    assert!(!local_pool_capacity_fail_fast_enabled(&config));
}

#[test]
fn local_external_fallback_capacity_gate_reason_matrix_is_explicit() {
    for reason in [
        "local_capacity_full",
        "local_scheduler_redis_degraded",
        "local_all_cooling_down",
        "local_pool_risk_circuit_open",
        "local_transient_exhausted",
        "local_auxiliary_attempts_exhausted",
        "local_auxiliary_concurrency_saturated",
        "local_attempt_reserved_for_fallback",
    ] {
        assert!(
            local_route_reason_requires_immediate_external_capacity(reason),
            "{reason} must not push local requests into external fallback unless an external pool can immediately accept"
        );
    }

    for reason in [
        "local_no_credentials",
        "local_all_disabled",
        "local_proxy_blocked",
        "local_no_model_compatible",
        "no_available_credentials",
        "unsupported_model",
    ] {
        assert!(
            !local_route_reason_requires_immediate_external_capacity(reason),
            "{reason} has no viable local route, so external fallback may use the external pool's own capacity policy"
        );
    }
}

#[test]
fn fresh_local_pool_state_blocks_external_while_dispatchable_except_degraded_states() {
    let mut config = ExternalPoolsConfig::default();

    assert_eq!(
        local_pool_fallback_reason_for_fresh_state(LocalPoolRouteStateKind::Ready, 1, &config,),
        None
    );
    assert_eq!(
        local_pool_fallback_reason_for_fresh_state(
            LocalPoolRouteStateKind::AllCoolingDown,
            1,
            &config,
        ),
        None
    );
    assert_eq!(
        local_pool_fallback_reason_for_fresh_state(
            LocalPoolRouteStateKind::CapacityFull,
            1,
            &config,
        ),
        None
    );

    assert_eq!(
        local_pool_fallback_reason_for_fresh_state(
            LocalPoolRouteStateKind::AllCoolingDown,
            0,
            &config,
        ),
        Some("local_all_cooling_down")
    );
    assert_eq!(
        local_pool_fallback_reason_for_fresh_state(
            LocalPoolRouteStateKind::CapacityFull,
            0,
            &config,
        ),
        Some("local_capacity_full")
    );
    assert_eq!(
        local_pool_fallback_reason_for_fresh_state(
            LocalPoolRouteStateKind::SchedulerRedisDegraded,
            0,
            &config,
        ),
        Some("local_scheduler_redis_degraded")
    );
    assert_eq!(
        local_pool_fallback_reason_for_fresh_state(
            LocalPoolRouteStateKind::SchedulerRedisDegraded,
            1,
            &config,
        ),
        Some("local_scheduler_redis_degraded"),
        "Redis scheduler degraded means distributed lease state is not trustworthy; stale in-memory dispatchable capacity must not suppress external fallback"
    );

    config.fallback_on_unsupported_model = true;
    assert_eq!(
        local_pool_fallback_reason_for_fresh_state(
            LocalPoolRouteStateKind::NoModelCompatible,
            0,
            &config,
        ),
        Some("local_no_model_compatible")
    );

    config.fallback_on_scheduler_redis_degraded = false;
    assert_eq!(
        local_pool_fallback_reason_for_fresh_state(
            LocalPoolRouteStateKind::SchedulerRedisDegraded,
            1,
            &config,
        ),
        None
    );
}

#[test]
fn classified_scheduler_degraded_fallback_is_not_suppressed_by_stale_ready_snapshot() {
    assert_eq!(
        classified_local_error_route_reason("local_scheduler_redis_degraded"),
        Some("local_scheduler_redis_degraded")
    );
    assert_eq!(
        classified_local_error_route_reason("local_attempt_reserved_for_fallback"),
        Some("local_attempt_reserved_for_fallback")
    );
    assert_eq!(
        classified_local_error_route_reason("unsupported_model"),
        Some("unsupported_model")
    );
    assert_eq!(
        classified_local_error_route_reason("local_capacity_exhausted"),
        None,
        "capacity fallback still uses fresh route state to preserve strict local-first"
    );
    assert_eq!(
        local_pool_fallback_reason_for_fresh_state(
            LocalPoolRouteStateKind::Ready,
            1,
            &ExternalPoolsConfig::default(),
        ),
        None,
        "a fresh Ready snapshot alone must not trigger external fallback"
    );
}

#[test]
fn external_fallback_classifier_gates_unsupported_model() {
    let mut config = ExternalPoolsConfig {
        fallback_on_unsupported_model: false,
        ..Default::default()
    };
    assert_eq!(
        classify_local_error_for_external_fallback("模型不支持: claude-future", &[], &config,),
        None
    );

    config.fallback_on_unsupported_model = true;
    assert_eq!(
        classify_local_error_for_external_fallback("模型不支持: claude-future", &[], &config,)
            .as_deref(),
        Some("unsupported_model")
    );
    assert_eq!(
            classify_local_error_for_external_fallback(
                r#"非流式 API 请求失败: 400 Bad Request {"message":"Invalid model. Please select a different model to continue.","reason":"INVALID_MODEL_ID"}"#,
                &[KiroCredentialAttempt::new(
                    0,
                    1,
                    None,
                    Some(StatusCode::BAD_REQUEST),
                    "fail",
                    Some("client_error"),
                    Some("bad request"),
                    10,
                )],
                &config,
            )
            .as_deref(),
            Some("unsupported_model")
        );
}

#[test]
fn external_local_rescue_classifier_respects_error_type_and_toggles() {
    let config = ExternalPoolsConfig::default();
    let rate_limit = ExternalPoolFinalError {
            status: StatusCode::TOO_MANY_REQUESTS,
            response_error_type: "rate_limit_error".to_string(),
            route_error_type: "rate_limit".to_string(),
            message:
                r#"{"message":"Too many requests, please wait before trying again.","reason":"SERVICE_REQUEST_RATE_EXCEEDED"}"#
                    .to_string(),
            error_id: "req_01rate_limit".to_string(),
            retryable: true,
            attempts: Vec::new(),
            pool_id: Some(1),
            pool_name: Some("backup".to_string()),
        };
    assert_eq!(
        local_rescue_reason_after_external_error(
            &config,
            &rate_limit,
            Some("local_capacity_full"),
            Some(1),
        ),
        Some("external_rate_limit")
    );
    assert_eq!(
        local_rescue_reason_after_external_error(
            &config,
            &rate_limit,
            Some("no_available_credentials"),
            Some(1),
        ),
        None
    );
    assert_eq!(
        local_rescue_reason_after_external_error(
            &config,
            &rate_limit,
            Some("local_capacity_exhausted"),
            Some(0),
        ),
        None
    );
    assert_eq!(
        local_rescue_reason_after_external_error(
            &config,
            &rate_limit,
            Some("local_attempt_reserved_for_fallback"),
            Some(1),
        ),
        Some("external_rate_limit")
    );

    let timeout = ExternalPoolFinalError {
        status: StatusCode::BAD_GATEWAY,
        response_error_type: "api_error".to_string(),
        route_error_type: "network_error".to_string(),
        message: "stream idle timeout".to_string(),
        error_id: "req_01timeout".to_string(),
        retryable: true,
        attempts: Vec::new(),
        pool_id: Some(1),
        pool_name: Some("backup".to_string()),
    };
    assert_eq!(
        local_rescue_reason_after_external_error(
            &config,
            &timeout,
            Some("local_capacity_full"),
            Some(1),
        ),
        Some("external_timeout")
    );

    let capacity = ExternalPoolFinalError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        response_error_type: "api_error".to_string(),
        route_error_type: "external_pool_capacity_full".to_string(),
        message: "Request capacity is full".to_string(),
        error_id: "req_01capacity".to_string(),
        retryable: true,
        attempts: Vec::new(),
        pool_id: None,
        pool_name: None,
    };
    assert_eq!(
        local_rescue_reason_after_external_error(
            &config,
            &capacity,
            Some("local_capacity_full"),
            Some(1),
        ),
        Some("external_capacity")
    );

    let bad_request = ExternalPoolFinalError {
        status: StatusCode::BAD_REQUEST,
        response_error_type: "invalid_request_error".to_string(),
        route_error_type: "client_error".to_string(),
        message: "Improperly formed request".to_string(),
        error_id: "req_01bad_request".to_string(),
        retryable: false,
        attempts: Vec::new(),
        pool_id: Some(1),
        pool_name: Some("backup".to_string()),
    };
    assert_eq!(
        local_rescue_reason_after_external_error(
            &config,
            &bad_request,
            Some("local_capacity_full"),
            Some(1),
        ),
        Some("external_bad_request")
    );

    let mut disabled = config.clone();
    disabled.external_pool_local_rescue_enabled = false;
    assert_eq!(
        local_rescue_reason_after_external_error(&disabled, &rate_limit, None, Some(1)),
        None
    );

    let mut direct = config.clone();
    direct.external_direct_policy_enabled = true;
    assert_eq!(
        local_rescue_reason_after_external_error(
            &direct,
            &rate_limit,
            Some("local_capacity_full"),
            Some(1),
        ),
        None
    );

    let mut no_rate_limit = config;
    no_rate_limit.external_pool_local_rescue_on_rate_limit = false;
    assert_eq!(
        local_rescue_reason_after_external_error(
            &no_rate_limit,
            &rate_limit,
            Some("local_capacity_full"),
            Some(1),
        ),
        None
    );

    let mut no_capacity = no_rate_limit;
    no_capacity.external_pool_local_rescue_on_capacity = false;
    assert_eq!(
        local_rescue_reason_after_external_error(
            &no_capacity,
            &capacity,
            Some("local_capacity_full"),
            Some(1),
        ),
        None
    );

    let server_error = ExternalPoolFinalError {
        status: StatusCode::BAD_GATEWAY,
        response_error_type: "api_error".to_string(),
        route_error_type: "server_error".to_string(),
        message: "external upstream failed".to_string(),
        error_id: "req_01server".to_string(),
        retryable: true,
        attempts: Vec::new(),
        pool_id: Some(1),
        pool_name: Some("backup".to_string()),
    };
    assert_eq!(
        local_rescue_reason_after_external_error(
            &no_capacity,
            &server_error,
            Some("local_transient_exhausted"),
            Some(1),
        ),
        None
    );
    assert_eq!(
        local_rescue_reason_after_external_error(
            &no_capacity,
            &server_error,
            Some("local_capacity_exhausted"),
            Some(0),
        ),
        None
    );
}

#[test]
fn external_local_rescue_is_blocked_after_terminal_local_route_reasons() {
    let config = ExternalPoolsConfig::default();
    let capacity = ExternalPoolFinalError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        response_error_type: "api_error".to_string(),
        route_error_type: "external_pool_capacity_full".to_string(),
        message: "No available external fallback pools".to_string(),
        error_id: "req_terminal_local_reason".to_string(),
        retryable: true,
        attempts: Vec::new(),
        pool_id: None,
        pool_name: None,
    };

    for reason in [
        "local_no_credentials",
        "local_all_disabled",
        "local_proxy_blocked",
        "local_no_model_compatible",
        "local_all_cooling_down",
        "local_scheduler_redis_degraded",
        "local_pool_risk_circuit_open",
        "local_transient_exhausted",
        "no_available_credentials",
        "unsupported_model",
    ] {
        assert_eq!(
            local_rescue_reason_after_external_error(&config, &capacity, Some(reason), Some(1),),
            None,
            "terminal local reason {reason} must not be rescued back to local"
        );
    }
}

#[test]
fn external_local_rescue_requires_current_capacity_for_capacity_based_local_fallbacks() {
    let config = ExternalPoolsConfig::default();
    let rate_limit = ExternalPoolFinalError {
        status: StatusCode::TOO_MANY_REQUESTS,
        response_error_type: "rate_limit_error".to_string(),
        route_error_type: "rate_limit".to_string(),
        message: "external rate limit".to_string(),
        error_id: "req_capacity_guard_rate".to_string(),
        retryable: true,
        attempts: Vec::new(),
        pool_id: Some(1),
        pool_name: Some("backup".to_string()),
    };

    assert_eq!(
        local_rescue_reason_after_external_error(
            &config,
            &rate_limit,
            Some("local_capacity_full"),
            Some(1),
        ),
        Some("external_rate_limit")
    );
    assert_eq!(
        local_rescue_reason_after_external_error(
            &config,
            &rate_limit,
            Some("local_capacity_exhausted"),
            Some(1),
        ),
        Some("external_rate_limit")
    );
    assert_eq!(
        local_rescue_reason_after_external_error(
            &config,
            &rate_limit,
            Some("local_attempt_reserved_for_fallback"),
            Some(1),
        ),
        Some("external_rate_limit")
    );
    assert_eq!(
        local_rescue_reason_after_external_error(
            &config,
            &rate_limit,
            Some("local_capacity_full"),
            Some(0),
        ),
        None
    );
    assert_eq!(
        local_rescue_reason_after_external_error(
            &config,
            &rate_limit,
            Some("local_capacity_exhausted"),
            Some(0),
        ),
        None
    );
    assert_eq!(
        local_rescue_reason_after_external_error(
            &config,
            &rate_limit,
            Some("local_attempt_reserved_for_fallback"),
            Some(0),
        ),
        None
    );
}

#[test]
fn local_rescue_requires_remaining_shared_attempt_budget_for_five_rounds() {
    use crate::anthropic::inference_attempt_budget::InferenceAttemptKind;

    let config = ExternalPoolsConfig::default();
    let rate_limit = ExternalPoolFinalError {
        status: StatusCode::TOO_MANY_REQUESTS,
        response_error_type: "rate_limit_error".to_string(),
        route_error_type: "rate_limit".to_string(),
        message: "redacted external rate limit".to_string(),
        error_id: "req_rescue_budget".to_string(),
        retryable: true,
        attempts: Vec::new(),
        pool_id: Some(1),
        pool_name: Some("backup".to_string()),
    };

    for round in 1..=5 {
        let remaining = InferenceAttemptBudget::new(4);
        remaining
            .reserve(InferenceAttemptKind::LocalCredential, 0)
            .unwrap();
        remaining
            .reserve(InferenceAttemptKind::ExternalPool, 0)
            .unwrap();
        assert_eq!(
            budgeted_local_rescue_reason_after_external_error(
                &config,
                &rate_limit,
                Some("local_capacity_full"),
                Some(1),
                &remaining,
            ),
            Some("external_rate_limit"),
            "round {round}: two attempts remain for a one-send rescue"
        );

        let exhausted = InferenceAttemptBudget::new(4);
        for _ in 0..3 {
            exhausted
                .reserve(InferenceAttemptKind::LocalCredential, 0)
                .unwrap();
        }
        exhausted
            .reserve(InferenceAttemptKind::ExternalPool, 0)
            .unwrap();
        assert_eq!(
            budgeted_local_rescue_reason_after_external_error(
                &config,
                &rate_limit,
                Some("local_capacity_full"),
                Some(1),
                &exhausted,
            ),
            None,
            "round {round}: exhausted budget must skip rescue before logging or waiting"
        );
    }
}

#[test]
fn direct_external_policy_disables_local_rescue_for_all_error_classes_five_rounds() {
    use crate::anthropic::inference_attempt_budget::InferenceAttemptKind;

    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_direct_policy_enabled: true,
        ..Default::default()
    };

    let errors = [
        ExternalPoolFinalError {
            status: StatusCode::TOO_MANY_REQUESTS,
            response_error_type: "rate_limit_error".to_string(),
            route_error_type: "rate_limit".to_string(),
            message: "external rate limit".to_string(),
            error_id: "req_direct_rate".to_string(),
            retryable: true,
            attempts: Vec::new(),
            pool_id: Some(1),
            pool_name: Some("direct".to_string()),
        },
        ExternalPoolFinalError {
            status: StatusCode::BAD_GATEWAY,
            response_error_type: "api_error".to_string(),
            route_error_type: "network_error".to_string(),
            message: "external timeout".to_string(),
            error_id: "req_direct_timeout".to_string(),
            retryable: true,
            attempts: Vec::new(),
            pool_id: Some(1),
            pool_name: Some("direct".to_string()),
        },
        ExternalPoolFinalError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            response_error_type: "api_error".to_string(),
            route_error_type: "external_pool_capacity_full".to_string(),
            message: "external capacity full".to_string(),
            error_id: "req_direct_capacity".to_string(),
            retryable: true,
            attempts: Vec::new(),
            pool_id: None,
            pool_name: None,
        },
        ExternalPoolFinalError {
            status: StatusCode::BAD_REQUEST,
            response_error_type: "invalid_request_error".to_string(),
            route_error_type: "client_error".to_string(),
            message: "external bad request".to_string(),
            error_id: "req_direct_bad_request".to_string(),
            retryable: false,
            attempts: Vec::new(),
            pool_id: Some(1),
            pool_name: Some("direct".to_string()),
        },
        ExternalPoolFinalError {
            status: StatusCode::BAD_GATEWAY,
            response_error_type: "api_error".to_string(),
            route_error_type: "server_error".to_string(),
            message: "external server error".to_string(),
            error_id: "req_direct_server".to_string(),
            retryable: true,
            attempts: Vec::new(),
            pool_id: Some(1),
            pool_name: Some("direct".to_string()),
        },
    ];

    for round in 1..=5 {
        for err in &errors {
            assert_eq!(
                local_rescue_reason_after_external_error(
                    &config,
                    err,
                    Some("local_capacity_full"),
                    Some(1),
                ),
                None,
                "round {round}: direct external policy must not route external failures back to local"
            );

            let budget = InferenceAttemptBudget::new(4);
            budget
                .reserve(InferenceAttemptKind::ExternalPool, 0)
                .unwrap();
            assert_eq!(
                budgeted_local_rescue_reason_after_external_error(
                    &config,
                    err,
                    Some("local_capacity_full"),
                    Some(1),
                    &budget,
                ),
                None,
                "round {round}: direct external policy must ignore remaining attempt budget"
            );
        }
    }
}

#[test]
fn direct_external_route_subtype_blocks_local_rescue_even_without_global_direct_flag() {
    use crate::anthropic::inference_attempt_budget::InferenceAttemptKind;

    let config = ExternalPoolsConfig {
        external_pools_enabled: true,
        external_direct_policy_enabled: false,
        external_pool_local_rescue_enabled: true,
        ..Default::default()
    };
    let server_error = ExternalPoolFinalError {
        status: StatusCode::BAD_GATEWAY,
        response_error_type: "api_error".to_string(),
        route_error_type: "server_error".to_string(),
        message: "external upstream failed".to_string(),
        error_id: "req_direct_route_subtype".to_string(),
        retryable: true,
        attempts: Vec::new(),
        pool_id: Some(1),
        pool_name: Some("direct".to_string()),
    };

    for round in 1..=10 {
        assert_eq!(
            local_rescue_reason_after_external_route_error(
                UsageRouteSubtype::ExternalDirectPolicy,
                &config,
                &server_error,
                Some("local_capacity_full"),
                Some(4),
            ),
            None,
            "round {round}: route subtype external_direct_policy is an absolute local-rescue boundary"
        );

        let budget = InferenceAttemptBudget::new(8);
        budget
            .reserve(InferenceAttemptKind::ExternalPool, 0)
            .unwrap();
        assert_eq!(
            budgeted_local_rescue_reason_after_external_route_error(
                UsageRouteSubtype::ExternalDirectPolicy,
                &config,
                &server_error,
                Some("local_capacity_full"),
                Some(4),
                &budget,
            ),
            None,
            "round {round}: direct route subtype must ignore remaining local rescue budget"
        );

        assert_eq!(
            budgeted_local_rescue_reason_after_external_route_error(
                UsageRouteSubtype::ExternalFallbackAfterLocalAttempts,
                &config,
                &server_error,
                Some("local_capacity_full"),
                Some(4),
                &budget,
            ),
            Some("external_error"),
            "round {round}: local-first fallback route still allows bounded rescue when the fresh local pool is dispatchable"
        );
    }
}

#[test]
fn preflight_external_error_can_rescue_once_then_attempt_budget_blocks_cycle_five_rounds() {
    use crate::anthropic::inference_attempt_budget::InferenceAttemptKind;

    let config = ExternalPoolsConfig::default();
    let capacity = ExternalPoolFinalError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        response_error_type: "api_error".to_string(),
        route_error_type: "external_pool_capacity_full".to_string(),
        message: "No available external fallback pools".to_string(),
        error_id: "req_preflight_capacity".to_string(),
        retryable: true,
        attempts: Vec::new(),
        pool_id: None,
        pool_name: None,
    };

    for round in 1..=5 {
        let budget = InferenceAttemptBudget::new(2);
        budget
            .reserve(InferenceAttemptKind::ExternalPool, 0)
            .unwrap();
        assert_eq!(
            budgeted_local_rescue_reason_after_external_error(
                &config,
                &capacity,
                Some("local_capacity_full"),
                Some(1),
                &budget,
            ),
            Some("external_capacity"),
            "round {round}: preflight external capacity failure may wait for one local rescue"
        );

        budget
            .reserve(InferenceAttemptKind::LocalCredential, 0)
            .unwrap();
        assert_eq!(
            budgeted_local_rescue_reason_after_external_error(
                &config,
                &capacity,
                Some("local_capacity_full"),
                Some(1),
                &budget,
            ),
            None,
            "round {round}: once external+local rescue consumed the two-send budget, no second external/local cycle is permitted"
        );
    }
}

#[test]
fn external_pool_endpoint_gate_applies_global_enable_and_route_policy() {
    let mut config = ExternalPoolsConfig::default();

    assert!(!external_pool_enabled_for_endpoint(
        &config,
        "/cc/v1/messages"
    ));

    config.external_pools_enabled = true;
    assert!(external_pool_enabled_for_endpoint(
        &config,
        "/cc/v1/messages"
    ));

    config.external_pool_route_mode = crate::model::config::ExternalPoolRouteMode::DenyList;
    config.external_pool_route_rules = vec!["/cc".to_string()];
    assert!(!external_pool_enabled_for_endpoint(
        &config,
        "/cc/v1/messages"
    ));
    assert!(external_pool_enabled_for_endpoint(&config, "/v1/messages"));

    config.external_pool_route_mode = crate::model::config::ExternalPoolRouteMode::AllowList;
    config.external_pool_route_rules = vec!["/dfcache/team-a".to_string()];
    assert!(!external_pool_enabled_for_endpoint(&config, "/v1/messages"));
    assert!(external_pool_enabled_for_endpoint(
        &config,
        "/dfcache/team-a/v1/messages"
    ));
}

#[test]
fn remote_url_safety_rejects_local_and_private_targets() {
    for url in [
        "http://localhost/image.png",
        "http://127.0.0.1/image.png",
        "http://10.0.0.5/image.png",
        "http://169.254.169.254/latest/meta-data",
        "http://[::1]/image.png",
    ] {
        assert!(
            body_processing::ensure_safe_remote_url(url).is_err(),
            "{url} should be blocked"
        );
    }
}
