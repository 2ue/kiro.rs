//! Anthropic-compatible response envelope helpers.

use axum::{
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Json, Response},
};
use serde_json::Value;

use super::types::ErrorResponse;

const ID_ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
const ID_RANDOM_LEN: usize = 22;

// ============ 对外错误文案（唯一来源） ============
//
// 所有返回给调用方的错误文案都在这里定义，不做成配置。每条都说清楚三件事：发生了什么、
// 是不是调用方要处理、能不能直接重试。文案不提及内部组件、账号、渠道或服务链路；排查编号
// 由 `public_message_with_error_id` 统一附在末尾。对外说明见 docs/errors.md。

/// 临时失败（网络、超时、连接中断），重试通常能成功。
pub(crate) const PUBLIC_TEMPORARY_FAILURE_MESSAGE: &str =
    "模型服务暂时没有响应，请稍后重试。The model service did not respond in time. Please retry.";
/// 请求已发出但没有完成，原因不属于调用方。
pub(crate) const PUBLIC_PROCESSING_FAILED_MESSAGE: &str =
    "本次请求处理失败，请稍后重试。The request failed to complete. Please retry.";
/// 空回：模型这次没有返回任何内容，已自动重试仍失败。
pub(crate) const PUBLIC_EMPTY_RESPONSE_MESSAGE: &str = "模型这次没有返回任何内容，已自动重试仍未成功，请重试本轮对话。The model returned no content for this turn even after automatic retries. Please retry.";
/// 输出过程中连接中断（已有部分内容）。
pub(crate) const PUBLIC_STREAM_INTERRUPTED_MESSAGE: &str = "回复在输出过程中中断，请重试本轮对话。The response was interrupted before it finished. Please retry this turn.";
/// 当前没有空闲的处理容量。
pub(crate) const PUBLIC_ACCOUNT_UNAVAILABLE_MESSAGE: &str = "当前请求较多，暂时没有可用的处理容量，请稍后重试。The service is busy right now. Please retry shortly.";
/// 服务刚启动或正在准备，暂时无法开始处理。
pub(crate) const PUBLIC_PROVIDER_NOT_READY_MESSAGE: &str = "服务正在准备中，暂时无法处理请求，请稍后重试。The service is still starting up. Please retry shortly.";
/// 请求的模型在这个接口不可用。
pub(crate) const PUBLIC_MODEL_UNAVAILABLE_MESSAGE: &str = "请求的模型在当前接口不可用，请更换模型。The requested model is not available on this endpoint. Choose another model.";
/// 请求体格式不被接受，需要调用方调整。
pub(crate) const PUBLIC_INVALID_REQUEST_MESSAGE: &str = "请求内容无法被模型接受，常见原因是历史消息、工具调用结果或附件格式有误。请精简内容或新开对话后重试。The request content was rejected. Check the message history, tool results, files, or images, or start a new conversation.";
/// 单张图片过大。
pub(crate) const PUBLIC_IMAGE_TOO_LARGE_MESSAGE: &str = "有图片超过 5 MB 的大小上限，请压缩或移除后重试。One or more images exceed the 5 MB size limit. Resize or remove them and retry.";
/// 图片数量过多。
pub(crate) const PUBLIC_IMAGE_COUNT_EXCEEDED_MESSAGE: &str = "本次请求携带的图片超过 100 张的上限，请新开对话或减少截图后重试。This request carries more than 100 images. Start a new conversation or send fewer images.";
/// 图片格式不支持。
pub(crate) const PUBLIC_IMAGE_FORMAT_UNSUPPORTED_MESSAGE: &str = "有图片的格式无法识别，请转换为 PNG、JPEG、GIF 或 WebP 后重试。An image format is not supported. Convert it to PNG, JPEG, GIF, or WebP and retry.";
/// 联网搜索工具失败。
pub(crate) const PUBLIC_WEB_SEARCH_FAILED_MESSAGE: &str = "联网搜索没有成功，请稍后重试，或关闭联网搜索后再试。Web search failed. Retry later or disable web search.";
/// 请求频率过高。
pub(crate) const PUBLIC_RATE_LIMIT_MESSAGE: &str =
    "当前请求较多，请稍后重试。Too many requests right now. Please retry shortly.";

/// 文案末尾附上排查编号。编号只用于定位日志，不暗示调用方需要联系谁。
pub(crate) fn public_message_with_error_id(message: &str, error_id: &str) -> String {
    format!("{message}（请求编号 request id: {error_id}）")
}

pub(crate) fn public_rate_limit_message(retry_after_secs: Option<u64>) -> String {
    match retry_after_secs {
        Some(seconds) if seconds > 0 => format!(
            "当前请求较多，请 {seconds} 秒后重试。Too many requests right now. Retry after {seconds} second{}.",
            if seconds == 1 { "" } else { "s" }
        ),
        _ => PUBLIC_RATE_LIMIT_MESSAGE.to_string(),
    }
}

/// Public text for an upstream error body.
///
/// Only the human-readable `message` is relayed, with known provider prefixes removed. Upstream
/// `reason` / `code` values are channel-specific identifiers (for example
/// `CONTENT_LENGTH_EXCEEDS_THRESHOLD`), so they stay in the logs and are never appended.
pub(crate) fn kiro_official_upstream_message(raw: &str) -> Option<String> {
    let value = extract_json_value(raw)?;
    let message = json_string_at(
        &value,
        &[
            "/message",
            "/Message",
            "/error/message",
            "/error/Message",
            "/error",
        ],
    )?;
    let message = normalize_public_upstream_text(strip_provider_message_prefix(message))?;
    Some(truncate_public_upstream_message(&message))
}

/// Provider wrappers that prefix the real message, e.g. `Bedrock error message: ...`.
fn strip_provider_message_prefix(message: &str) -> &str {
    const PREFIXES: &[&str] = &["bedrock error message:", "bedrock error:"];
    let trimmed = message.trim_start();
    PREFIXES
        .iter()
        .find_map(|prefix| {
            trimmed
                .get(..prefix.len())
                .filter(|head| head.eq_ignore_ascii_case(prefix))
                .map(|_| trimmed[prefix.len()..].trim_start())
        })
        .unwrap_or(trimmed)
}

fn extract_json_value(raw: &str) -> Option<Value> {
    if let Ok(value) = serde_json::from_str::<Value>(raw.trim()) {
        return Some(value);
    }
    for (idx, ch) in raw.char_indices() {
        if ch != '{' {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(&raw[idx..]) {
            return Some(value);
        }
    }
    None
}

fn json_string_at<'a>(value: &'a Value, pointers: &[&str]) -> Option<&'a str> {
    pointers
        .iter()
        .find_map(|pointer| value.pointer(pointer).and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn normalize_public_upstream_text(raw: &str) -> Option<String> {
    let text = raw
        .chars()
        .map(|ch| {
            if ch.is_control() && ch != '\n' && ch != '\r' && ch != '\t' {
                ' '
            } else {
                ch
            }
        })
        .collect::<String>();
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() || public_upstream_message_has_forbidden_content(&text) {
        return None;
    }
    Some(text)
}

/// Whether proxy-generated text can be shown to clients verbatim; it must not name the serving
/// channel or internal machinery. Callers fall back to a generic public message otherwise.
pub(crate) fn public_text_is_safe(message: &str) -> bool {
    !public_upstream_message_has_forbidden_content(message)
}

fn public_upstream_message_has_forbidden_content(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    let substring_hit = [
        "kiro",
        "credential",
        "external pool",
        "external_pool",
        "fallback",
        "scheduler",
        "bearer token",
        "api key",
        "client secret",
        "refresh token",
        "access token",
        // Channel identity: the serving platform must not be inferable from relayed text.
        "amazon",
        "amazonaws",
        "bedrock",
        "codewhisperer",
        "q developer",
        "arn:",
        "profilearn",
        "profile_arn",
        "账号 #",
        "凭据",
        "凭证",
        "外部池",
        "调度",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    // Short or ambiguous terms are matched as whole words so ordinary text ("laws", "draws",
    // "q") is not rejected.
    substring_hit
        || lower
            .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
            .any(|word| matches!(word, "aws" | "sigv4" | "q_developer"))
        || message_contains_upstream_reason_code(message)
}

/// Upstream reason codes are SCREAMING_SNAKE_CASE identifiers such as
/// `MONTHLY_REQUEST_COUNT`; a message that embeds one is channel-identifying.
fn message_contains_upstream_reason_code(message: &str) -> bool {
    message
        .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
        .any(|word| {
            word.len() >= 8
                && word.contains('_')
                && word
                    .chars()
                    .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
                && word.chars().any(|ch| ch.is_ascii_uppercase())
        })
}

fn truncate_public_upstream_message(message: &str) -> String {
    const MAX_BYTES: usize = 1024;
    if message.len() <= MAX_BYTES {
        return message.to_string();
    }
    let mut out = String::with_capacity(MAX_BYTES + 3);
    for ch in message.chars() {
        if out.len() + ch.len_utf8() > MAX_BYTES {
            out.push_str("...");
            return out;
        }
        out.push(ch);
    }
    out
}

fn random_anthropic_id(prefix: &str) -> String {
    let mut id = String::with_capacity(prefix.len() + 3 + ID_RANDOM_LEN);
    id.push_str(prefix);
    id.push_str("_01");
    for _ in 0..ID_RANDOM_LEN {
        id.push(ID_ALPHABET[fastrand::usize(..ID_ALPHABET.len())] as char);
    }
    id
}

pub(crate) fn message_id() -> String {
    random_anthropic_id("msg")
}

pub(crate) fn tool_use_id() -> String {
    random_anthropic_id("toolu")
}

pub(crate) fn server_tool_use_id() -> String {
    random_anthropic_id("srvtoolu")
}

/// Client-facing id for an upstream tool call. Kiro ids (`tooluse_...`) are mapped to the
/// Anthropic shape (`toolu_01...`); the mapping is a hash so every chunk of one upstream call,
/// and a replayed or duplicated event, resolves to the same id. Ids already in the Anthropic
/// shape pass through. The client echoes the id back in `tool_result`, and Kiro accepts any
/// matching id pair in history, so no reverse mapping is kept.
pub(crate) fn public_tool_use_id(upstream_id: &str) -> String {
    use sha2::{Digest, Sha256};

    if upstream_id.starts_with("toolu_") {
        return upstream_id.to_string();
    }
    let digest = Sha256::digest(upstream_id.as_bytes());
    let mut id = String::with_capacity("toolu_01".len() + ID_RANDOM_LEN);
    id.push_str("toolu_01");
    for byte in digest.iter().take(ID_RANDOM_LEN) {
        id.push(ID_ALPHABET[usize::from(*byte) % ID_ALPHABET.len()] as char);
    }
    id
}

pub(crate) fn request_id() -> String {
    random_anthropic_id("req")
}

pub(crate) fn insert_request_id_headers(headers: &mut HeaderMap, request_id: &str) {
    let Ok(value) = HeaderValue::from_str(request_id) else {
        return;
    };
    if !headers.contains_key("request-id") {
        headers.insert("request-id", value.clone());
    }
    if !headers.contains_key("anthropic-request-id") {
        headers.insert("anthropic-request-id", value);
    }
}

/// Header the Anthropic API and its SDKs use to override the status-based retry decision.
pub(crate) const SHOULD_RETRY_HEADER: &str = "x-should-retry";

/// Client errors that repeat identically on retry. The Anthropic API marks these with
/// `x-should-retry: false`; without it Claude Code keeps retrying (for example a 401 for
/// about ten attempts). 408, 409, 429 and 5xx stay retryable and are left untouched.
pub(crate) fn status_is_non_retryable_client_error(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::BAD_REQUEST
            | StatusCode::UNAUTHORIZED
            | StatusCode::PAYMENT_REQUIRED
            | StatusCode::FORBIDDEN
            | StatusCode::NOT_FOUND
            | StatusCode::METHOD_NOT_ALLOWED
            | StatusCode::PAYLOAD_TOO_LARGE
            | StatusCode::UNSUPPORTED_MEDIA_TYPE
            | StatusCode::UNPROCESSABLE_ENTITY
    )
}

/// Adds `x-should-retry: false` to non-retryable client errors unless a handler already set it.
pub(crate) fn insert_should_retry_header(headers: &mut HeaderMap, status: StatusCode) {
    if status_is_non_retryable_client_error(status) && !headers.contains_key(SHOULD_RETRY_HEADER) {
        headers.insert(SHOULD_RETRY_HEADER, HeaderValue::from_static("false"));
    }
}

pub(crate) fn insert_optional_warnings_header(headers: &mut HeaderMap, warnings: Option<String>) {
    let Some(warnings) = warnings else {
        return;
    };
    if let Ok(value) = HeaderValue::from_str(&warnings) {
        headers.insert("x-kiro-rs-warnings", value);
    }
}

pub(crate) fn error_response_with_id(
    status: StatusCode,
    error_type: impl Into<String>,
    message: impl Into<String>,
    request_id: &str,
) -> Response {
    let body = ErrorResponse::new(error_type, message).with_request_id(request_id);
    crate::diagnostics::capture::record("client_response", Some(request_id), || {
        serde_json::json!({
            "kind": "error",
            "status": status.as_u16(),
            "body": serde_json::to_value(&body).unwrap_or(Value::Null),
        })
    });
    let mut response = (status, Json(body)).into_response();
    insert_request_id_headers(response.headers_mut(), request_id);
    insert_should_retry_header(response.headers_mut(), status);
    response
}

pub(crate) fn error_response_with_id_and_headers(
    status: StatusCode,
    error_type: impl Into<String>,
    message: impl Into<String>,
    request_id: &str,
    extra_headers: impl IntoIterator<Item = (&'static str, String)>,
) -> Response {
    let mut response = error_response_with_id(status, error_type, message, request_id);
    for (name, value) in extra_headers {
        if let Ok(value) = HeaderValue::from_str(&value) {
            response.headers_mut().insert(name, value);
        }
    }
    response
}

pub(crate) fn error_response(
    status: StatusCode,
    error_type: impl Into<String>,
    message: impl Into<String>,
) -> Response {
    let request_id = request_id();
    error_response_with_id(status, error_type, message, &request_id)
}

pub(crate) fn json_response_with_id(
    status: StatusCode,
    body: Value,
    request_id: &str,
    warnings: Option<String>,
) -> Response {
    crate::diagnostics::capture::record("client_response", Some(request_id), || {
        serde_json::json!({
            "kind": "json",
            "status": status.as_u16(),
            "warnings": warnings,
            "body": body,
        })
    });
    let mut response = (status, Json(body)).into_response();
    insert_request_id_headers(response.headers_mut(), request_id);
    insert_optional_warnings_header(response.headers_mut(), warnings);
    response
}

pub(crate) fn sse_builder_with_id(request_id: &str) -> axum::http::response::Builder {
    axum::http::Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::CONNECTION, "keep-alive")
        .header("x-accel-buffering", "no")
        .header("request-id", request_id)
        .header("anthropic-request-id", request_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_use_anthropic_compatible_shape_without_uuid_hex() {
        let msg = message_id();
        let req = request_id();

        assert!(msg.starts_with("msg_01"));
        assert!(req.starts_with("req_01"));
        assert_eq!(msg.len(), "msg_01".len() + ID_RANDOM_LEN);
        assert_eq!(req.len(), "req_01".len() + ID_RANDOM_LEN);
        assert!(!msg.contains('-'));
        assert!(!req.contains('-'));
    }

    #[test]
    fn tool_ids_use_anthropic_prefixes() {
        let tool = tool_use_id();
        let server = server_tool_use_id();
        assert!(tool.starts_with("toolu_01"));
        assert!(server.starts_with("srvtoolu_01"));
        assert_eq!(tool.len(), "toolu_01".len() + ID_RANDOM_LEN);
        assert_eq!(server.len(), "srvtoolu_01".len() + ID_RANDOM_LEN);
        assert!(
            tool.chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        );
    }

    #[test]
    fn upstream_tool_ids_map_to_stable_anthropic_ids() {
        let mapped = public_tool_use_id("tooluse_h1bbC3KF2KuOFbdG5EAQLC");
        assert!(mapped.starts_with("toolu_01"));
        assert_eq!(mapped.len(), "toolu_01".len() + ID_RANDOM_LEN);
        assert_eq!(mapped, public_tool_use_id("tooluse_h1bbC3KF2KuOFbdG5EAQLC"));
        assert_ne!(mapped, public_tool_use_id("tooluse_OpVN1F4YbO74BG70uFmV5r"));
        assert_eq!(public_tool_use_id("toolu_01abc"), "toolu_01abc");
    }

    #[test]
    fn request_id_headers_are_inserted_without_overwriting() {
        let mut headers = HeaderMap::new();
        headers.insert("request-id", HeaderValue::from_static("req_existing"));

        insert_request_id_headers(&mut headers, "req_01abc");

        assert_eq!(headers["request-id"], "req_existing");
        assert_eq!(headers["anthropic-request-id"], "req_01abc");
    }

    #[test]
    fn error_responses_mark_only_non_retryable_client_errors() {
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::NOT_FOUND,
            StatusCode::METHOD_NOT_ALLOWED,
            StatusCode::PAYLOAD_TOO_LARGE,
        ] {
            let response = error_response(status, "invalid_request_error", "nope");
            assert_eq!(
                response.headers()[SHOULD_RETRY_HEADER],
                "false",
                "{status} must not be retried"
            );
        }
        for status in [
            StatusCode::REQUEST_TIMEOUT,
            StatusCode::CONFLICT,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::BAD_GATEWAY,
            StatusCode::SERVICE_UNAVAILABLE,
            StatusCode::from_u16(529).unwrap(),
        ] {
            let response = error_response(status, "api_error", "later");
            assert!(
                !response.headers().contains_key(SHOULD_RETRY_HEADER),
                "{status} must stay retryable"
            );
        }
    }

    #[test]
    fn should_retry_header_keeps_explicit_handler_value() {
        let mut headers = HeaderMap::new();
        headers.insert(SHOULD_RETRY_HEADER, HeaderValue::from_static("true"));
        insert_should_retry_header(&mut headers, StatusCode::BAD_REQUEST);
        assert_eq!(headers[SHOULD_RETRY_HEADER], "true");
    }

    #[test]
    fn sse_builder_disables_proxy_buffering() {
        let response = sse_builder_with_id("req_01abc")
            .body(())
            .expect("SSE response should build");

        assert_eq!(response.headers()["x-accel-buffering"], "no");
    }

    #[test]
    fn public_error_messages_do_not_expose_internal_terms() {
        let retry_after_message = public_rate_limit_message(Some(3));
        for message in [
            PUBLIC_TEMPORARY_FAILURE_MESSAGE,
            PUBLIC_PROCESSING_FAILED_MESSAGE,
            PUBLIC_ACCOUNT_UNAVAILABLE_MESSAGE,
            PUBLIC_PROVIDER_NOT_READY_MESSAGE,
            PUBLIC_MODEL_UNAVAILABLE_MESSAGE,
            PUBLIC_INVALID_REQUEST_MESSAGE,
            PUBLIC_RATE_LIMIT_MESSAGE,
            retry_after_message.as_str(),
        ] {
            let lower = message.to_ascii_lowercase();
            for forbidden in [
                "kiro",
                "credential",
                "external pool",
                "external_pool",
                "fallback",
                "backup",
                "scheduler",
                "lease",
                "capacity snapshot",
                "service unavailable",
                "upstream",
                "备用",
                "外部池",
                "凭据",
                "凭证",
            ] {
                assert!(
                    !public_message_contains_forbidden_term(&lower, forbidden),
                    "public message leaked internal term {forbidden:?}: {message}"
                );
            }
        }
    }

    fn public_message_contains_forbidden_term(lower_message: &str, forbidden: &str) -> bool {
        if forbidden
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        {
            return lower_message
                .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
                .any(|part| part == forbidden);
        }
        lower_message.contains(forbidden)
    }

    #[test]
    fn retry_after_message_uses_singular_and_plural_units() {
        assert!(public_rate_limit_message(Some(1)).contains("1 second."));
        assert!(public_rate_limit_message(Some(2)).contains("2 seconds."));
        assert_eq!(
            public_rate_limit_message(Some(0)),
            PUBLIC_RATE_LIMIT_MESSAGE
        );
        assert_eq!(public_rate_limit_message(None), PUBLIC_RATE_LIMIT_MESSAGE);
    }

    #[test]
    fn kiro_official_upstream_message_extracts_json_body_without_internal_prefix() {
        let message = kiro_official_upstream_message(
            r#"流式 API 请求失败（账号 #170 test）: 400 Bad Request {"message":"Bedrock error message: Could not process image","reason":"IMAGE_FORMAT_UNSUPPORTED"}"#,
        )
        .expect("official message");

        assert_eq!(message, "Could not process image");
        assert!(!message.contains("账号"));
        assert!(!message.contains("test"));
        assert!(!message.to_ascii_lowercase().contains("bedrock"));
        assert!(!message.contains("IMAGE_FORMAT_UNSUPPORTED"));
    }

    #[test]
    fn kiro_official_upstream_message_hides_channel_identifying_text() {
        for raw in [
            r#"{"message":"Amazon Q could not complete the request"}"#,
            r#"{"message":"Request failed for arn:aws:codewhisperer:us-east-1:123:profile/X"}"#,
            r#"{"message":"Error calling https://q.us-east-1.amazonaws.com"}"#,
            r#"{"message":"CodeWhisperer streaming failed"}"#,
            r#"{"message":"Bedrock rejected this request"}"#,
            r#"{"message":"AWS service error"}"#,
            r#"{"message":"Input exceeds CONTENT_LENGTH_EXCEEDS_THRESHOLD"}"#,
            r#"{"message":"Monthly limit reached: MONTHLY_REQUEST_COUNT"}"#,
        ] {
            assert!(
                kiro_official_upstream_message(raw).is_none(),
                "channel-identifying upstream text must not be relayed: {raw}"
            );
        }
    }

    #[test]
    fn kiro_official_upstream_message_keeps_ordinary_wording() {
        for (raw, expected) in [
            (
                r#"{"message":"Improperly formed request.","reason":"INVALID_INPUT_FORMAT"}"#,
                "Improperly formed request.",
            ),
            (
                r#"{"message":"The draws and laws in this image could not be read"}"#,
                "The draws and laws in this image could not be read",
            ),
            (
                r#"{"message":"Bedrock error: Input is too long for requested model."}"#,
                "Input is too long for requested model.",
            ),
        ] {
            assert_eq!(
                kiro_official_upstream_message(raw).as_deref(),
                Some(expected),
                "{raw}"
            );
        }
    }

    #[test]
    fn kiro_official_upstream_message_rejects_sensitive_json_message() {
        assert!(
            kiro_official_upstream_message(
                r#"{"message":"The bearer token included in the request is invalid"}"#
            )
            .is_none()
        );
    }

    #[test]
    fn kiro_official_upstream_message_rejects_kiro_branded_json_message() {
        assert!(
            kiro_official_upstream_message(
                r#"{"message":"Kiro service rejected this model request"}"#
            )
            .is_none()
        );
    }

    #[test]
    fn kiro_official_upstream_message_drops_forbidden_reason_without_leaking_it() {
        let message = kiro_official_upstream_message(
            r#"{"message":"The requested model is temporarily unavailable.","reason":"KIRO_MODEL_GATEWAY"}"#,
        )
        .expect("safe message remains available");

        assert_eq!(message, "The requested model is temporarily unavailable.");
        assert!(!message.to_ascii_lowercase().contains("kiro"));
    }
}
