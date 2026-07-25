//! Kiro 端点抽象
//!
//! 不同 Kiro 端点（如 `ide` / `cli`）在 URL、请求头、请求体上存在差异，
//! 但共享凭据池、Token 刷新、重试逻辑和 AWS event-stream 响应解码。
//!
//! [`KiroEndpoint`] 抽象了请求侧的差异点；`KiroProvider` 持有一个 endpoint 注册表，
//! 按凭据的 `endpoint` 字段选择对应实现。

use reqwest::{Method, RequestBuilder};

use crate::kiro::model::credentials::KiroCredentials;
use crate::model::config::Config;

pub mod cli;
pub mod ide;

pub use cli::CliEndpoint;
pub use ide::IdeEndpoint;

/// Resolve a local/staging upstream override while preserving endpoint-specific Host headers.
///
/// The override changes only the transport destination. Callers must still decorate the request
/// with the logical AWS/Kiro host derived from the credential region.
pub(crate) fn configured_upstream_url(config: &Config, suffix: &str) -> Option<String> {
    let base = config
        .kiro_upstream_base_url
        .as_deref()
        .map(str::trim)
        .filter(|base| !base.is_empty())?
        .trim_end_matches('/');
    let suffix = suffix.trim_start_matches('/');
    Some(if suffix.is_empty() {
        format!("{base}/")
    } else {
        format!("{base}/{suffix}")
    })
}

/// Detect semantic ASCII object keys without parsing or allocating a JSON value tree.
///
/// Endpoint marker fast paths call this only when a raw `"key"` search missed and the
/// body contains a backslash, so escaped keys such as `"orig\u0069n"` cannot bypass a
/// required transform. Invalid JSON may produce a conservative marker hit, but the
/// subsequent real parse still fails closed and returns the original body unchanged.
pub(super) fn contains_json_object_key(body: &str, targets: &[&str]) -> bool {
    debug_assert!(targets.iter().all(|target| target.is_ascii()));
    let bytes = body.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'"' {
            index += 1;
            continue;
        }

        let string_start = index + 1;
        let Some(string_end) = json_string_end(bytes, string_start) else {
            return false;
        };
        index = string_end + 1;
        let mut next = index;
        while next < bytes.len() && matches!(bytes[next], b' ' | b'\t' | b'\r' | b'\n') {
            next += 1;
        }
        if next < bytes.len()
            && bytes[next] == b':'
            && targets.iter().any(|target| {
                json_string_matches_ascii(&bytes[string_start..string_end], target.as_bytes())
            })
        {
            return true;
        }
    }
    false
}

pub(super) fn serialize_json_with_capacity(
    value: &serde_json::Value,
    minimum_capacity: usize,
) -> Option<String> {
    let mut output = Vec::with_capacity(minimum_capacity);
    serde_json::to_writer(&mut output, value).ok()?;
    String::from_utf8(output).ok()
}

pub(super) fn body_may_need_output_config_thinking_normalization(body: &str) -> bool {
    let plain_markers = body.contains("\"additionalModelRequestFields\"")
        && body.contains("\"output_config\"")
        && body.contains("\"thinking\"");
    plain_markers
        || (body.as_bytes().contains(&b'\\')
            && contains_json_object_key(
                body,
                &["additionalModelRequestFields", "output_config", "thinking"],
            ))
}

pub(super) fn normalize_output_config_thinking_compatibility_json(
    json: &mut serde_json::Value,
) -> bool {
    let Some(fields) = json
        .get_mut("additionalModelRequestFields")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return false;
    };
    if fields
        .get("output_config")
        .is_none_or(serde_json::Value::is_null)
    {
        return false;
    }
    let incompatible_thinking = match fields.get("thinking") {
        Some(serde_json::Value::Object(thinking)) => thinking
            .get("type")
            .and_then(serde_json::Value::as_str)
            .is_none_or(|thinking_type| thinking_type != "adaptive"),
        Some(_) => true,
        None => false,
    };
    if incompatible_thinking {
        fields.remove("thinking");
        return true;
    }
    false
}

fn json_string_end(bytes: &[u8], mut index: usize) -> Option<usize> {
    while index < bytes.len() {
        match bytes[index] {
            b'"' => return Some(index),
            b'\\' => {
                index = index.checked_add(2)?;
            }
            _ => index += 1,
        }
    }
    None
}

fn json_string_matches_ascii(encoded: &[u8], target: &[u8]) -> bool {
    let mut input_index = 0;
    let mut target_index = 0;
    while input_index < encoded.len() {
        let semantic = if encoded[input_index] == b'\\' {
            input_index += 1;
            let Some(escape) = encoded.get(input_index).copied() else {
                return false;
            };
            input_index += 1;
            match escape {
                b'"' | b'\\' | b'/' => escape,
                b'b' => 0x08,
                b'f' => 0x0c,
                b'n' => b'\n',
                b'r' => b'\r',
                b't' => b'\t',
                b'u' => {
                    let Some(hex) = encoded.get(input_index..input_index.saturating_add(4)) else {
                        return false;
                    };
                    let Some(codepoint) = decode_json_hex4(hex) else {
                        return false;
                    };
                    input_index += 4;
                    let Ok(ascii) = u8::try_from(codepoint) else {
                        return false;
                    };
                    if !ascii.is_ascii() {
                        return false;
                    }
                    ascii
                }
                _ => return false,
            }
        } else {
            let byte = encoded[input_index];
            input_index += 1;
            if !byte.is_ascii() || byte < 0x20 {
                return false;
            }
            byte
        };
        if target.get(target_index).copied() != Some(semantic) {
            return false;
        }
        target_index += 1;
    }
    target_index == target.len()
}

fn decode_json_hex4(hex: &[u8]) -> Option<u16> {
    if hex.len() != 4 {
        return None;
    }
    hex.iter().try_fold(0_u16, |value, byte| {
        let digit = match byte {
            b'0'..=b'9' => u16::from(byte - b'0'),
            b'a'..=b'f' => u16::from(byte - b'a' + 10),
            b'A'..=b'F' => u16::from(byte - b'A' + 10),
            _ => return None,
        };
        Some((value << 4) | digit)
    })
}

/// Kiro 端点
///
/// 同一个 `KiroProvider` 可持有多个 endpoint 实现，按凭据级字段切换。
pub trait KiroEndpoint: Send + Sync {
    /// 端点名称（对应 credentials.endpoint / config.defaultEndpoint 的取值）
    fn name(&self) -> &'static str;

    /// API endpoint URL
    fn api_url(&self, ctx: &RequestContext<'_>) -> String;

    /// API/MCP request content type.
    ///
    /// IDE uses normal JSON. CLI runtime uses AWS JSON 1.0.
    fn content_type(&self) -> &'static str {
        "application/json"
    }

    /// MCP endpoint URL
    fn mcp_url(&self, ctx: &RequestContext<'_>) -> String;

    /// ListAvailableModels endpoint URL.
    fn models_url(&self, ctx: &RequestContext<'_>, next_token: Option<&str>) -> String;

    /// ListAvailableModels HTTP method.
    ///
    /// IDE-compatible endpoints use the legacy GET form. The Kiro CLI management
    /// endpoint uses AWS JSON 1.0 POST with a JSON body.
    fn models_method(&self, _ctx: &RequestContext<'_>) -> Method {
        Method::GET
    }

    /// Optional ListAvailableModels JSON body.
    fn models_body(
        &self,
        _ctx: &RequestContext<'_>,
        _next_token: Option<&str>,
    ) -> Option<serde_json::Value> {
        None
    }

    /// 装饰 API 请求的端点特有 header
    ///
    /// Provider 已经设置好 URL、content-type、Connection 和 body；
    /// 实现负责追加 Authorization、host、user-agent 等端点相关头。
    fn decorate_api(&self, req: RequestBuilder, ctx: &RequestContext<'_>) -> RequestBuilder;

    /// 装饰 MCP 请求的端点特有 header
    fn decorate_mcp(&self, req: RequestBuilder, ctx: &RequestContext<'_>) -> RequestBuilder;

    /// 装饰 ListAvailableModels 请求的端点特有 header
    fn decorate_models(&self, req: RequestBuilder, ctx: &RequestContext<'_>) -> RequestBuilder;

    /// 对已序列化的 API 请求体做端点特有加工（如注入 profileArn）
    fn transform_api_body(&self, body: &str, ctx: &RequestContext<'_>) -> String;

    /// 对已序列化的 MCP 请求体做端点特有加工（默认不变）
    fn transform_mcp_body(&self, body: &str, _ctx: &RequestContext<'_>) -> String {
        body.to_string()
    }

    /// 判断响应体是否表示"额度/透支请求额度用尽"（禁用凭据并转移）
    fn is_monthly_request_limit(&self, body: &str) -> bool {
        default_is_monthly_request_limit(body)
    }

    /// 判断响应体是否表示"上游 bearer token 失效"（触发强制刷新）
    fn is_bearer_token_invalid(&self, body: &str) -> bool {
        default_is_bearer_token_invalid(body)
    }
}

/// 装饰请求时可用的上下文
///
/// 包含单次调用已确定的所有运行时信息。引用形式避免无谓 clone。
pub struct RequestContext<'a> {
    /// 当前凭据
    pub credentials: &'a KiroCredentials,
    /// 有效的 access token（API Key 凭据下即 kiroApiKey）
    pub token: &'a str,
    /// 当前凭据对应的 machineId
    pub machine_id: &'a str,
    /// 全局配置
    pub config: &'a Config,
}

/// 默认的额度用尽判断逻辑
///
/// 同时识别顶层 `reason` 字段、嵌套 `error.reason` 字段和 Kiro overage 限制。
pub fn default_is_quota_exhausted(body: &str) -> bool {
    if body.contains("MONTHLY_REQUEST_COUNT") || body.contains("OVERAGE_REQUEST_LIMIT_EXCEEDED") {
        return true;
    }

    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };

    if value
        .get("reason")
        .and_then(|v| v.as_str())
        .is_some_and(|v| {
            matches!(
                v,
                "MONTHLY_REQUEST_COUNT" | "OVERAGE_REQUEST_LIMIT_EXCEEDED"
            )
        })
    {
        return true;
    }

    value
        .pointer("/error/reason")
        .and_then(|v| v.as_str())
        .is_some_and(|v| {
            matches!(
                v,
                "MONTHLY_REQUEST_COUNT" | "OVERAGE_REQUEST_LIMIT_EXCEEDED"
            )
        })
}

/// 向后兼容旧名称：语义已扩展为额度用尽。
pub fn default_is_monthly_request_limit(body: &str) -> bool {
    default_is_quota_exhausted(body)
}

/// 默认的 bearer token 失效判断逻辑
pub fn default_is_bearer_token_invalid(body: &str) -> bool {
    body.contains("The bearer token included in the request is invalid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaped_json_object_key_detector_covers_valid_and_adversarial_shapes_for_five_rounds() {
        let positives = [
            r#"{"orig\u0069n":"AI_EDITOR"}"#,
            r#"{"ORIGIN":"other","orig\u0069n":"AI_EDITOR"}"#,
            r#"{"additionalModelRequest\u0046ields":{}}"#,
            r#"{"additionalModelRequest\u0046ield\u0073":{}}"#,
            r#"{"output\u005Fconfig":{}}"#,
            r#"{"nested":{"orig\u0069n":"AI_EDITOR"}}"#,
        ];
        let negatives = [
            r#"{"text":"orig\u0069n"}"#,
            r#"{"text":"\"orig\u0069n\": value text"}"#,
            r#"{"orig\\u0069n":"literal backslash"}"#,
            r#"{"orig\u006An":"different key"}"#,
            r#"{"orig\uD800n":"surrogate key"}"#,
            r#"{"原点":"origin"}"#,
            r#"{"unterminated":"value}"#,
            r#"{"bad\u00xz":"value"}"#,
        ];
        let targets = ["origin", "additionalModelRequestFields", "output_config"];

        for round in 0..5 {
            for body in positives {
                assert!(
                    contains_json_object_key(body, &targets),
                    "round {round}: expected semantic key in {body:?}"
                );
            }
            for body in negatives {
                assert!(
                    !contains_json_object_key(body, &targets),
                    "round {round}: must not treat value/text/invalid escape as a target key in {body:?}"
                );
            }
        }
    }

    #[test]
    fn test_default_monthly_request_limit_detects_reason() {
        let body = r#"{"message":"You have reached the limit.","reason":"MONTHLY_REQUEST_COUNT"}"#;
        assert!(default_is_monthly_request_limit(body));
    }

    #[test]
    fn test_default_monthly_request_limit_nested_reason() {
        let body = r#"{"error":{"reason":"MONTHLY_REQUEST_COUNT"}}"#;
        assert!(default_is_monthly_request_limit(body));
    }

    #[test]
    fn test_default_monthly_request_limit_false() {
        let body = r#"{"message":"nope","reason":"DAILY_REQUEST_COUNT"}"#;
        assert!(!default_is_monthly_request_limit(body));
    }

    #[test]
    fn test_default_quota_exhausted_detects_overage_limit() {
        let body = r#"{"message":"You have reached the limit for overages.","reason":"OVERAGE_REQUEST_LIMIT_EXCEEDED"}"#;
        assert!(default_is_monthly_request_limit(body));
    }

    #[test]
    fn test_default_quota_exhausted_detects_nested_overage_limit() {
        let body = r#"{"error":{"reason":"OVERAGE_REQUEST_LIMIT_EXCEEDED"}}"#;
        assert!(default_is_quota_exhausted(body));
    }

    #[test]
    fn test_default_bearer_token_invalid() {
        assert!(default_is_bearer_token_invalid(
            "The bearer token included in the request is invalid"
        ));
        assert!(!default_is_bearer_token_invalid("unrelated error"));
    }
}
