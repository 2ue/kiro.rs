use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(crate) const RAW_UPSTREAM_ERROR_BODY_MAX_BYTES: usize = 2 * 1024;
const RAW_UPSTREAM_ERROR_JSON_PARSE_MAX_BYTES: usize = 64 * 1024;
const RAW_UPSTREAM_ERROR_MAX_LINES: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RawUpstreamError {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    pub body: String,
    pub body_bytes: usize,
    pub truncated: bool,
}

impl RawUpstreamError {
    pub fn from_bytes(
        source: impl Into<String>,
        status_code: Option<u16>,
        content_type: Option<&str>,
        body: &[u8],
    ) -> Self {
        let body_bytes = body.len();
        let content_type = content_type
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let (mut bounded, fragment_truncated) =
            upstream_error_fragment(body, content_type.as_deref());
        let truncated = body.len() > bounded.len() || fragment_truncated;
        if truncated {
            bounded.push_str("\n...[truncated]");
        }
        Self {
            source: source.into(),
            status_code,
            content_type,
            body: bounded,
            body_bytes,
            truncated,
        }
    }

    pub fn from_text(
        source: impl Into<String>,
        status_code: Option<u16>,
        content_type: Option<&str>,
        body: &str,
    ) -> Self {
        Self::from_bytes(source, status_code, content_type, body.as_bytes())
    }

    pub fn normalize(mut self) -> Self {
        let normalized = Self::from_text(
            self.source.clone(),
            self.status_code,
            self.content_type.as_deref(),
            &self.body,
        );
        self.body = normalized.body;
        self.truncated |= normalized.truncated;
        self.body_bytes = self.body_bytes.max(normalized.body_bytes);
        self.content_type = normalized.content_type;
        self
    }
}

fn upstream_error_fragment(body: &[u8], content_type: Option<&str>) -> (String, bool) {
    if body.is_empty() {
        return (String::new(), false);
    }
    if let Some(fragment) = upstream_error_looks_like_json(body, content_type)
        .then(|| upstream_json_error_fragment(body))
        .flatten()
    {
        return bounded_utf8_fragment(fragment.as_bytes(), RAW_UPSTREAM_ERROR_BODY_MAX_BYTES);
    }
    bounded_utf8_fragment(body, RAW_UPSTREAM_ERROR_BODY_MAX_BYTES)
}

fn upstream_error_looks_like_json(body: &[u8], content_type: Option<&str>) -> bool {
    content_type
        .map(|value| value.to_ascii_lowercase().contains("json"))
        .unwrap_or(false)
        || body
            .iter()
            .copied()
            .find(|byte| !byte.is_ascii_whitespace())
            .is_some_and(|byte| byte == b'{' || byte == b'[')
}

fn upstream_json_error_fragment(body: &[u8]) -> Option<String> {
    let parse_len = utf8_boundary_len(
        body,
        body.len().min(RAW_UPSTREAM_ERROR_JSON_PARSE_MAX_BYTES),
    );
    let text = std::str::from_utf8(&body[..parse_len]).ok()?;
    let value: Value = serde_json::from_str(text).ok()?;
    let mut lines = Vec::new();
    collect_json_error_lines(&value, "$", false, 0, &mut lines);
    (!lines.is_empty()).then(|| lines.join("\n"))
}

fn collect_json_error_lines(
    value: &Value,
    path: &str,
    in_error_context: bool,
    depth: usize,
    lines: &mut Vec<String>,
) {
    if lines.len() >= RAW_UPSTREAM_ERROR_MAX_LINES || depth > 8 {
        return;
    }
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if lines.len() >= RAW_UPSTREAM_ERROR_MAX_LINES {
                    return;
                }
                let child_path = format_json_path(path, key);
                let key_is_error = json_error_key(key);
                let next_in_error_context = in_error_context || key_is_error;
                if next_in_error_context && child.is_primitive() {
                    lines.push(format!("{child_path}: {}", json_scalar_fragment(child)));
                    continue;
                }
                collect_json_error_lines(
                    child,
                    &child_path,
                    next_in_error_context,
                    depth + 1,
                    lines,
                );
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().take(5).enumerate() {
                if lines.len() >= RAW_UPSTREAM_ERROR_MAX_LINES {
                    return;
                }
                collect_json_error_lines(
                    child,
                    &format!("{path}[{index}]"),
                    in_error_context,
                    depth + 1,
                    lines,
                );
            }
        }
        primitive if in_error_context => {
            lines.push(format!("{path}: {}", json_scalar_fragment(primitive)));
        }
        _ => {}
    }
}

fn json_error_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "error"
            | "errors"
            | "message"
            | "error_message"
            | "error_type"
            | "type"
            | "code"
            | "status"
            | "detail"
            | "details"
            | "reason"
            | "title"
            | "description"
            | "param"
            | "request_id"
            | "requestid"
            | "trace_id"
            | "traceid"
    )
}

fn format_json_path(parent: &str, key: &str) -> String {
    if key
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        format!("{parent}.{key}")
    } else {
        format!(
            "{parent}[{}]",
            serde_json::to_string(key).unwrap_or_default()
        )
    }
}

fn json_scalar_fragment(value: &Value) -> String {
    match value {
        Value::String(text) => bounded_utf8_fragment(text.as_bytes(), 512).0,
        Value::Null => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        _ => value.to_string(),
    }
}

fn bounded_utf8_fragment(body: &[u8], max_bytes: usize) -> (String, bool) {
    let end = utf8_boundary_len(body, body.len().min(max_bytes));
    let bounded = String::from_utf8_lossy(&body[..end]).into_owned();
    (bounded, body.len() > end)
}

fn utf8_boundary_len(body: &[u8], mut end: usize) -> usize {
    while end > 0 && std::str::from_utf8(&body[..end]).is_err() {
        end -= 1;
    }
    end
}

trait JsonPrimitive {
    fn is_primitive(&self) -> bool;
}

impl JsonPrimitive for Value {
    fn is_primitive(&self) -> bool {
        matches!(
            self,
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_upstream_error_is_utf8_safe_and_bounded() {
        let body = "错".repeat(RAW_UPSTREAM_ERROR_BODY_MAX_BYTES);
        let error = RawUpstreamError::from_text(
            "external_pool",
            Some(400),
            Some("application/json"),
            &body,
        );

        assert!(error.truncated);
        assert!(error.body.len() <= RAW_UPSTREAM_ERROR_BODY_MAX_BYTES + 32);
        assert!(error.body.ends_with("...[truncated]"));
        assert_eq!(error.body_bytes, body.len());
    }

    #[test]
    fn raw_upstream_error_extracts_json_error_fields_instead_of_full_body() {
        let body = serde_json::json!({
            "error": {
                "type": "invalid_request_error",
                "message": "request is invalid",
                "code": "bad_request"
            },
            "largePayloadEcho": "x".repeat(4096),
            "unrelated": { "body": "should not be copied" }
        })
        .to_string();
        let error = RawUpstreamError::from_text(
            "external_pool",
            Some(400),
            Some("application/json"),
            &body,
        );

        assert!(error.body.contains("$.error.type: invalid_request_error"));
        assert!(error.body.contains("$.error.message: request is invalid"));
        assert!(!error.body.contains("largePayloadEcho"));
        assert!(!error.body.contains("should not be copied"));
        assert_eq!(error.body_bytes, body.len());
    }
}
