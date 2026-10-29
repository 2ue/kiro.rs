//! OpenAI 兼容接口：Chat Completions（`POST /v1/chat/completions`）和 Responses
//! （`POST /v1/responses`），各前缀下的同名路径同样可用。
//!
//! 请求在 HTTP 层翻译成 Anthropic Messages 请求体，交给消息入口处理，鉴权、准入、调度、
//! 提示词注入、缓存、用量记录和外部池兜底全部复用；响应再翻译回 OpenAI 格式。
//! 用量记录的入口路径是 `{前缀}/v1/chat/completions` 或 `{前缀}/v1/responses`，路由策略按前缀生效。
//!
//! 不转发的字段：`temperature`、`top_p` 等采样参数（Kiro 不支持，Anthropic 在开启思考时会拒绝），
//! 以及 `logprobs`、`seed`、`user`、`store` 等 Anthropic 没有对应的字段。

mod request;
mod response;
mod responses_request;
mod responses_response;

use axum::{
    body::Body,
    extract::{Extension, Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Json, Response},
};
use bytes::Bytes;
use futures::{StreamExt, stream::BoxStream};
use serde_json::Value;

use super::envelope;
use super::handlers::{
    missing_max_tokens_default_value, model_uses_adaptive_thinking, post_messages_for_endpoint,
    record_pre_usage_rejection, resolve_defined_cache_route,
};
use super::middleware::AppState;
use super::request_admission::{RequestRejectionAttribution, RequestRejectionReason};
use super::request_body::MessagesBody;
use response::{SseParser, StreamTranslator};
use responses_response::{ResponseShell, ResponsesStreamTranslator};

/// 消息入口返回的响应都已有大小上限，这里只是兜底。
const MAX_TRANSLATED_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

/// POST /v1/chat/completions
pub async fn post_chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    attribution: Option<Extension<RequestRejectionAttribution>>,
    body: MessagesBody,
) -> Response {
    chat_completions(state, headers, attribution, body, "/v1/chat/completions").await
}

/// POST /na/v1/chat/completions
pub async fn post_chat_completions_na(
    State(state): State<AppState>,
    headers: HeaderMap,
    attribution: Option<Extension<RequestRejectionAttribution>>,
    body: MessagesBody,
) -> Response {
    chat_completions(state, headers, attribution, body, "/na/v1/chat/completions").await
}

/// POST /cc/v1/chat/completions
pub async fn post_chat_completions_cc(
    State(state): State<AppState>,
    headers: HeaderMap,
    attribution: Option<Extension<RequestRejectionAttribution>>,
    body: MessagesBody,
) -> Response {
    chat_completions(state, headers, attribution, body, "/cc/v1/chat/completions").await
}

/// POST /ha/v1/chat/completions
pub async fn post_chat_completions_ha(
    State(state): State<AppState>,
    headers: HeaderMap,
    attribution: Option<Extension<RequestRejectionAttribution>>,
    body: MessagesBody,
) -> Response {
    chat_completions(state, headers, attribution, body, "/ha/v1/chat/completions").await
}

/// POST /dfcache/:route/v1/chat/completions
pub async fn post_chat_completions_dfcache(
    State(state): State<AppState>,
    Path(route): Path<String>,
    headers: HeaderMap,
    attribution: Option<Extension<RequestRejectionAttribution>>,
    body: MessagesBody,
) -> Response {
    let prefix = match resolve_defined_cache_route(&state, &route) {
        Ok(prefix) => prefix,
        Err(response) => {
            record_pre_usage_rejection(
                attribution.as_ref().map(|Extension(value)| value),
                RequestRejectionReason::DfcacheRouteInvalid,
                &format!("/dfcache/{route}/v1/chat/completions"),
                &response,
            );
            return translate_error_response(response).await;
        }
    };
    let endpoint = format!("{prefix}/v1/chat/completions");
    chat_completions(state, headers, attribution, body, &endpoint).await
}

async fn chat_completions(
    state: AppState,
    mut headers: HeaderMap,
    attribution: Option<Extension<RequestRejectionAttribution>>,
    MessagesBody(raw_body, request_api_key_id, ingress_id): MessagesBody,
    endpoint: &str,
) -> Response {
    let attribution = attribution.map(|Extension(value)| value);
    let translated = request::translate_request(
        &raw_body,
        missing_max_tokens_default_value(&state),
        model_uses_adaptive_thinking,
    );
    let translated = match translated {
        Ok(translated) => translated,
        Err(message) => {
            let response = openai_json_response(
                StatusCode::BAD_REQUEST,
                response::openai_error("invalid_request_error", &message),
                &envelope::request_id(),
            );
            record_pre_usage_rejection(
                attribution.as_ref(),
                RequestRejectionReason::RequestEntryInvalid,
                endpoint,
                &response,
            );
            return response;
        }
    };
    let requested_model = translated.body["model"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let body = match serde_json::to_vec(&translated.body) {
        Ok(body) => Bytes::from(body),
        Err(error) => {
            return openai_json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                response::openai_error("api_error", &format!("failed to encode request: {error}")),
                &envelope::request_id(),
            );
        }
    };
    headers.remove(header::CONTENT_LENGTH);
    let response = post_messages_for_endpoint(
        state,
        headers,
        body,
        endpoint.to_string(),
        request_api_key_id,
        ingress_id,
        attribution,
    )
    .await;
    let created = chrono::Utc::now().timestamp();
    if !response.status().is_success() {
        return translate_error_response(response).await;
    }
    if is_event_stream(response.headers()) {
        return translate_stream_response(
            response,
            StreamTranslator::new(&requested_model, created, translated.include_usage),
        );
    }
    let (mut parts, body) = response.into_parts();
    let message = axum::body::to_bytes(body, MAX_TRANSLATED_RESPONSE_BYTES)
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .filter(|message| message["type"] == "message");
    let Some(message) = message else {
        return openai_json_response(
            StatusCode::BAD_GATEWAY,
            response::openai_error("api_error", "upstream returned an unreadable response"),
            &response_request_id(&parts.headers),
        );
    };
    let completion = response::translate_message(&message, &requested_model, created);
    parts.headers.remove(header::CONTENT_LENGTH);
    parts.headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    let body = serde_json::to_vec(&completion).unwrap_or_default();
    Response::from_parts(parts, Body::from(body))
}

/// POST /v1/responses
pub async fn post_responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    attribution: Option<Extension<RequestRejectionAttribution>>,
    body: MessagesBody,
) -> Response {
    responses(state, headers, attribution, body, "/v1/responses").await
}

/// POST /na/v1/responses
pub async fn post_responses_na(
    State(state): State<AppState>,
    headers: HeaderMap,
    attribution: Option<Extension<RequestRejectionAttribution>>,
    body: MessagesBody,
) -> Response {
    responses(state, headers, attribution, body, "/na/v1/responses").await
}

/// POST /cc/v1/responses
pub async fn post_responses_cc(
    State(state): State<AppState>,
    headers: HeaderMap,
    attribution: Option<Extension<RequestRejectionAttribution>>,
    body: MessagesBody,
) -> Response {
    responses(state, headers, attribution, body, "/cc/v1/responses").await
}

/// POST /ha/v1/responses
pub async fn post_responses_ha(
    State(state): State<AppState>,
    headers: HeaderMap,
    attribution: Option<Extension<RequestRejectionAttribution>>,
    body: MessagesBody,
) -> Response {
    responses(state, headers, attribution, body, "/ha/v1/responses").await
}

/// POST /dfcache/:route/v1/responses
pub async fn post_responses_dfcache(
    State(state): State<AppState>,
    Path(route): Path<String>,
    headers: HeaderMap,
    attribution: Option<Extension<RequestRejectionAttribution>>,
    body: MessagesBody,
) -> Response {
    let prefix = match resolve_defined_cache_route(&state, &route) {
        Ok(prefix) => prefix,
        Err(response) => {
            record_pre_usage_rejection(
                attribution.as_ref().map(|Extension(value)| value),
                RequestRejectionReason::DfcacheRouteInvalid,
                &format!("/dfcache/{route}/v1/responses"),
                &response,
            );
            return translate_error_response(response).await;
        }
    };
    let endpoint = format!("{prefix}/v1/responses");
    responses(state, headers, attribution, body, &endpoint).await
}

async fn responses(
    state: AppState,
    mut headers: HeaderMap,
    attribution: Option<Extension<RequestRejectionAttribution>>,
    MessagesBody(raw_body, request_api_key_id, ingress_id): MessagesBody,
    endpoint: &str,
) -> Response {
    let attribution = attribution.map(|Extension(value)| value);
    let translated = responses_request::translate_request(
        &raw_body,
        missing_max_tokens_default_value(&state),
        model_uses_adaptive_thinking,
    );
    let translated = match translated {
        Ok(translated) => translated,
        Err(message) => {
            let response = openai_json_response(
                StatusCode::BAD_REQUEST,
                response::openai_error("invalid_request_error", &message),
                &envelope::request_id(),
            );
            record_pre_usage_rejection(
                attribution.as_ref(),
                RequestRejectionReason::RequestEntryInvalid,
                endpoint,
                &response,
            );
            return response;
        }
    };
    let stream = translated.body["stream"] == true;
    let body = match serde_json::to_vec(&translated.body) {
        Ok(body) => Bytes::from(body),
        Err(error) => {
            return openai_json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                response::openai_error("api_error", &format!("failed to encode request: {error}")),
                &envelope::request_id(),
            );
        }
    };
    let shell = ResponseShell {
        id: response::prefixed_id("resp_", None),
        created_at: chrono::Utc::now().timestamp(),
        model: translated.body["model"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        echo: translated.echo,
    };
    let custom_tools = translated.custom_tools.into_iter().collect();
    headers.remove(header::CONTENT_LENGTH);
    let response = post_messages_for_endpoint(
        state,
        headers,
        body,
        endpoint.to_string(),
        request_api_key_id,
        ingress_id,
        attribution,
    )
    .await;
    if !response.status().is_success() {
        return translate_error_response(response).await;
    }
    if stream && is_event_stream(response.headers()) {
        return translate_stream_response(
            response,
            ResponsesStreamTranslator::new(shell, custom_tools),
        );
    }
    let (mut parts, body) = response.into_parts();
    let message = axum::body::to_bytes(body, MAX_TRANSLATED_RESPONSE_BYTES)
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .filter(|message| message["type"] == "message");
    let Some(message) = message else {
        return openai_json_response(
            StatusCode::BAD_GATEWAY,
            response::openai_error("api_error", "upstream returned an unreadable response"),
            &response_request_id(&parts.headers),
        );
    };
    let translated = responses_response::translate_message(&message, shell, &custom_tools);
    parts.headers.remove(header::CONTENT_LENGTH);
    parts.headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    let body = serde_json::to_vec(&translated).unwrap_or_default();
    Response::from_parts(parts, Body::from(body))
}

fn is_event_stream(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/event-stream"))
}

fn response_request_id(headers: &HeaderMap) -> String {
    headers
        .get("request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
        .unwrap_or_else(envelope::request_id)
}

fn openai_json_response(status: StatusCode, body: Value, request_id: &str) -> Response {
    let mut response = (status, Json(body)).into_response();
    envelope::insert_request_id_headers(response.headers_mut(), request_id);
    response
}

/// 错误响应换成 OpenAI 信封，保留 `retry-after`、请求 id 等响应头。OpenAI 没有 529，换成 503。
async fn translate_error_response(response: Response) -> Response {
    let (mut parts, body) = response.into_parts();
    let bytes = axum::body::to_bytes(body, MAX_TRANSLATED_RESPONSE_BYTES)
        .await
        .unwrap_or_default();
    let error = serde_json::from_slice::<Value>(&bytes)
        .ok()
        .and_then(|value| response::openai_error_from_anthropic(&value))
        .unwrap_or_else(|| {
            let message = String::from_utf8_lossy(&bytes);
            let message = message.trim();
            response::openai_error(
                "api_error",
                if message.is_empty() {
                    "upstream error"
                } else {
                    message
                },
            )
        });
    if parts.status.as_u16() == 529 {
        parts.status = StatusCode::SERVICE_UNAVAILABLE;
    }
    parts.headers.remove(header::CONTENT_LENGTH);
    parts.headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    let body = serde_json::to_vec(&error).unwrap_or_default();
    Response::from_parts(parts, Body::from(body))
}

/// Chat Completions 与 Responses 两种流翻译器的共同接口。
trait AnthropicStreamTranslator: Send + 'static {
    fn on_event(&mut self, event: response::SseMessage) -> Vec<String>;
    /// 上游流结束（没有遇到 `message_stop` 时以错误收尾）。
    fn on_end(&mut self) -> Vec<String>;
    fn fail(&mut self, error_type: &str, message: &str) -> Vec<String>;
}

impl AnthropicStreamTranslator for StreamTranslator {
    fn on_event(&mut self, event: response::SseMessage) -> Vec<String> {
        StreamTranslator::on_event(self, event)
    }
    fn on_end(&mut self) -> Vec<String> {
        StreamTranslator::on_end(self)
    }
    fn fail(&mut self, error_type: &str, message: &str) -> Vec<String> {
        StreamTranslator::fail(self, error_type, message)
    }
}

impl AnthropicStreamTranslator for ResponsesStreamTranslator {
    fn on_event(&mut self, event: response::SseMessage) -> Vec<String> {
        ResponsesStreamTranslator::on_event(self, event)
    }
    fn on_end(&mut self) -> Vec<String> {
        ResponsesStreamTranslator::on_end(self)
    }
    fn fail(&mut self, error_type: &str, message: &str) -> Vec<String> {
        ResponsesStreamTranslator::fail(self, error_type, message)
    }
}

struct StreamState<T> {
    upstream: BoxStream<'static, Result<Bytes, axum::Error>>,
    parser: SseParser,
    translator: T,
    upstream_done: bool,
}

/// 翻译完 `message_stop` 后仍然读完上游流，让消息入口按正常结束记账，而不是当成客户端断开。
fn translate_stream_response<T: AnthropicStreamTranslator>(
    response: Response,
    translator: T,
) -> Response {
    let (mut parts, body) = response.into_parts();
    parts.headers.remove(header::CONTENT_LENGTH);
    let state = StreamState {
        upstream: body.into_data_stream().boxed(),
        parser: SseParser::default(),
        translator,
        upstream_done: false,
    };
    let stream = futures::stream::unfold(state, |mut state| async move {
        loop {
            if state.upstream_done {
                return None;
            }
            let output = match state.upstream.next().await {
                Some(Ok(chunk)) => match state.parser.feed(&chunk) {
                    Ok(events) => events
                        .into_iter()
                        .flat_map(|event| state.translator.on_event(event))
                        .collect(),
                    Err(message) => {
                        state.upstream_done = true;
                        state.translator.fail("api_error", &message)
                    }
                },
                Some(Err(error)) => {
                    state.upstream_done = true;
                    state.translator.fail(
                        "api_error",
                        &format!("upstream stream read failed: {error}"),
                    )
                }
                None => {
                    state.upstream_done = true;
                    let mut output: Vec<String> = state
                        .parser
                        .finish()
                        .map(|event| state.translator.on_event(event))
                        .unwrap_or_default();
                    output.extend(state.translator.on_end());
                    output
                }
            };
            if !output.is_empty() {
                return Some((
                    Ok::<_, std::convert::Infallible>(Bytes::from(output.concat())),
                    state,
                ));
            }
        }
    });
    Response::from_parts(parts, Body::from_stream(stream))
}
