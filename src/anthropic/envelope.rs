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
// 所有返回给调用方的错误文案都在这里定义，不做成配置，只用英文。每条都说清楚三件事：发生了什么、
// 是不是调用方要处理、能不能直接重试。文案不提及内部组件、账号、渠道或服务链路；排查编号
// 由 `public_message_with_error_id` 统一附在末尾。对外说明见 docs/errors.md。

/// 临时失败（网络、超时、连接中断），重试通常能成功。
pub(crate) const PUBLIC_TEMPORARY_FAILURE_MESSAGE: &str =
    "The model service did not respond in time. Please try again.";
/// 请求已发出但没有完成，原因不属于调用方。
pub(crate) const PUBLIC_PROCESSING_FAILED_MESSAGE: &str =
    "The request could not be completed. Please try again.";
/// 空回：模型这次没有返回任何内容，已自动重试仍失败。

/// 输出过程中连接中断（已有部分内容）。

/// 当前没有空闲的处理容量。
pub(crate) const PUBLIC_ACCOUNT_UNAVAILABLE_MESSAGE: &str =
    "The service is busy right now. Please try again shortly.";
/// 服务刚启动或正在准备，暂时无法开始处理。
pub(crate) const PUBLIC_PROVIDER_NOT_READY_MESSAGE: &str =
    "The service is starting up. Please try again shortly.";
/// 请求的模型在这个接口不可用。
pub(crate) const PUBLIC_MODEL_UNAVAILABLE_MESSAGE: &str =
    "The requested model is not available on this endpoint. Please choose another model.";
/// 请求体格式不被接受，需要调用方调整。
pub(crate) const PUBLIC_INVALID_REQUEST_MESSAGE: &str = "The request could not be processed. Please check the message history, tool results, files, or images and try again.";
/// 单张图片过大。
pub(crate) const PUBLIC_IMAGE_TOO_LARGE_MESSAGE: &str =
    "One or more images exceed the 5 MB size limit. Please resize or remove them and try again.";
/// 图片数量过多。

/// 图片格式不支持。

/// 联网搜索工具失败。

/// 请求频率过高。
pub(crate) const PUBLIC_RATE_LIMIT_MESSAGE: &str =
    "Too many requests right now. Please try again shortly.";

/// 文案末尾附上请求编号（英文），便于反馈问题时定位日志。
pub(crate) fn public_message_with_error_id(message: &str, error_id: &str) -> String {
    format!("{message} (request-id: {error_id})")
}

pub(crate) fn public_rate_limit_message(retry_after_secs: Option<u64>) -> String {
    match retry_after_secs {
        Some(seconds) if seconds > 0 => format!(
            "Too many requests right now. Please try again in {seconds} second{}.",
            if seconds == 1 { "" } else { "s" }
        ),
        _ => PUBLIC_RATE_LIMIT_MESSAGE.to_string(),
    }
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
                "pool",
                "credential",
                "account",
                "account",
                "fallback",
                "backup",
                "scheduler",
                "lease",
                "capacity snapshot",
                "service unavailable",
                "upstream",
                "备用",
                "账号",
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
}
