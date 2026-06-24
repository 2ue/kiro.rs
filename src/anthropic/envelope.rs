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
    "The request could not be completed right now. Please retry later.";
pub(crate) const PUBLIC_PROCESSING_FAILED_MESSAGE: &str =
    "Request processing failed. Please retry later.";
pub(crate) const PUBLIC_ACCOUNT_UNAVAILABLE_MESSAGE: &str =
    "No account is currently available. Please retry later or contact the administrator.";
pub(crate) const PUBLIC_PROVIDER_NOT_READY_MESSAGE: &str =
    "Requests cannot be processed right now. Please retry later or contact the administrator.";
pub(crate) const PUBLIC_MODEL_UNAVAILABLE_MESSAGE: &str =
    "The requested model is not available. Select a supported model and retry.";
pub(crate) const PUBLIC_INVALID_REQUEST_MESSAGE: &str =
    "Invalid request. Simplify the message, tools, tool results, files, or images and retry.";

pub(crate) fn public_message_with_error_id(message: &str, error_id: &str) -> String {
    format!("{message} If this continues, contact the administrator with error ID: {error_id}")
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
}
