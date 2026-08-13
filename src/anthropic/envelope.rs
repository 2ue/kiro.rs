//! Anthropic-compatible response envelope helpers.

use axum::{
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Json, Response},
};
use serde_json::Value;

use super::types::ErrorResponse;

const ID_ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
const ID_RANDOM_LEN: usize = 22;

pub(crate) const PUBLIC_TEMPORARY_FAILURE_MESSAGE: &str =
    "The request could not be completed right now. Please retry shortly.";
pub(crate) const PUBLIC_PROCESSING_FAILED_MESSAGE: &str =
    "The request could not be completed. Please retry shortly.";
pub(crate) const PUBLIC_ACCOUNT_UNAVAILABLE_MESSAGE: &str =
    "No account is ready for this request right now. Please retry shortly.";
pub(crate) const PUBLIC_PROVIDER_NOT_READY_MESSAGE: &str =
    "The request could not be started right now. Please retry shortly.";
pub(crate) const PUBLIC_MODEL_UNAVAILABLE_MESSAGE: &str =
    "The requested model is not available for this endpoint.";
pub(crate) const PUBLIC_INVALID_REQUEST_MESSAGE: &str = "The request body is invalid. Simplify the message, tools, tool results, files, or images and retry.";
pub(crate) const PUBLIC_RATE_LIMIT_MESSAGE: &str =
    "No account is ready for this request right now. Please retry shortly.";

pub(crate) fn public_message_with_error_id(message: &str, error_id: &str) -> String {
    format!("{message} If this continues, contact the administrator with error ID: {error_id}")
}

pub(crate) fn public_rate_limit_message(retry_after_secs: Option<u64>) -> String {
    match retry_after_secs {
        Some(seconds) if seconds > 0 => {
            let unit = if seconds == 1 { "second" } else { "seconds" };
            format!("No account is ready for this request right now. Retry after {seconds} {unit}.")
        }
        _ => PUBLIC_RATE_LIMIT_MESSAGE.to_string(),
    }
}

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
    let mut message = normalize_public_upstream_text(message)?;
    if let Some(reason) = json_string_at(
        &value,
        &[
            "/reason",
            "/Reason",
            "/error/reason",
            "/error/Reason",
            "/code",
            "/Code",
            "/error/code",
            "/error/Code",
        ],
    )
    .and_then(normalize_public_upstream_text)
    .filter(|reason| !message.contains(reason))
    {
        message.push_str(" (reason: ");
        message.push_str(&reason);
        message.push(')');
    }
    Some(truncate_public_upstream_message(&message))
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

fn public_upstream_message_has_forbidden_content(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    [
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
        "账号 #",
        "凭据",
        "凭证",
        "外部池",
        "调度",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
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
    let mut response = (
        status,
        Json(ErrorResponse::new(error_type, message).with_request_id(request_id)),
    )
        .into_response();
    insert_request_id_headers(response.headers_mut(), request_id);
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
    fn request_id_headers_are_inserted_without_overwriting() {
        let mut headers = HeaderMap::new();
        headers.insert("request-id", HeaderValue::from_static("req_existing"));

        insert_request_id_headers(&mut headers, "req_01abc");

        assert_eq!(headers["request-id"], "req_existing");
        assert_eq!(headers["anthropic-request-id"], "req_01abc");
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

        assert_eq!(
            message,
            "Bedrock error message: Could not process image (reason: IMAGE_FORMAT_UNSUPPORTED)"
        );
        assert!(!message.contains("账号"));
        assert!(!message.contains("test"));
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
