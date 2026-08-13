//! Anthropic API 类型定义

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use serde::{Deserialize, Serialize, ser::SerializeStruct};
use std::collections::HashMap;

// === 错误响应 ===

/// API 错误响应
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    #[serde(rename = "type")]
    pub response_type: &'static str,
    pub error: ErrorDetail,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

/// 错误详情
#[derive(Debug, Serialize)]
pub struct ErrorDetail {
    #[serde(rename = "type")]
    pub error_type: String,
    pub message: String,
}

impl ErrorResponse {
    /// 创建新的错误响应
    pub fn new(error_type: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            response_type: "error",
            error: ErrorDetail {
                error_type: error_type.into(),
                message: message.into(),
            },
            request_id: None,
        }
    }

    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = Some(request_id.into());
        self
    }

    /// 创建认证错误响应
    #[allow(dead_code)]
    pub fn authentication_error() -> Self {
        Self::new("authentication_error", "Invalid API key")
    }
}

// === Models 端点类型 ===

/// 模型信息
#[derive(Debug, Serialize, Clone)]
pub struct Model {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub owned_by: String,
    pub display_name: String,
    #[serde(rename = "type")]
    pub model_type: String,
    pub max_tokens: i32,
    #[serde(rename = "maxInputTokens", skip_serializing_if = "Option::is_none")]
    pub max_input_tokens: Option<i32>,
    #[serde(rename = "contextWindow", skip_serializing_if = "Option::is_none")]
    pub context_window: Option<i32>,
}

/// 模型列表响应
#[derive(Debug, Serialize)]
pub struct ModelsResponse {
    pub object: String,
    pub data: Vec<Model>,
}

// === Messages 端点类型 ===

pub const THINKING_EFFORT_VALUES: &[&str] = &["low", "medium", "high", "xhigh", "max"];
// Canonical base64 for this decoded limit fits within the 1 MiB atomic
// reasoning block bound used by the response pipelines.
pub const MAX_REDACTED_THINKING_DECODED_BYTES: usize = 768 * 1024;

pub fn validate_redacted_thinking_data(data: &str) -> Result<usize, &'static str> {
    if data.is_empty() {
        return Err("redacted_thinking.data must not be empty");
    }
    let max_encoded_bytes = MAX_REDACTED_THINKING_DECODED_BYTES
        .div_ceil(3)
        .saturating_mul(4);
    if data.len() > max_encoded_bytes {
        return Err("redacted_thinking.data exceeds the decoded size limit");
    }
    let decoded = BASE64_STANDARD
        .decode(data)
        .map_err(|_| "redacted_thinking.data must be canonical base64")?;
    if decoded.is_empty() || decoded.len() > MAX_REDACTED_THINKING_DECODED_BYTES {
        return Err("redacted_thinking.data exceeds the decoded size limit");
    }
    if BASE64_STANDARD.encode(&decoded) != data {
        return Err("redacted_thinking.data must be canonical base64");
    }
    Ok(decoded.len())
}

/// Thinking 配置
#[derive(Debug, Deserialize, Clone)]
pub struct Thinking {
    #[serde(rename = "type")]
    pub thinking_type: String,
    #[serde(
        default = "default_budget_tokens",
        deserialize_with = "deserialize_budget_tokens"
    )]
    pub budget_tokens: i32,
}

impl Thinking {
    /// 是否启用了 thinking（enabled 或 adaptive）
    pub fn is_enabled(&self) -> bool {
        self.thinking_type == "enabled" || self.thinking_type == "adaptive"
    }
}

impl Serialize for Thinking {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let field_count = if self.thinking_type == "enabled" {
            2
        } else {
            1
        };
        let mut state = serializer.serialize_struct("Thinking", field_count)?;
        state.serialize_field("type", &self.thinking_type)?;
        if self.thinking_type == "enabled" {
            state.serialize_field("budget_tokens", &self.budget_tokens)?;
        }
        state.end()
    }
}

fn default_budget_tokens() -> i32 {
    0
}
fn deserialize_budget_tokens<'de, D>(deserializer: D) -> Result<i32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    i32::deserialize(deserializer)
}

fn deserialize_nullable_map<'de, D>(
    deserializer: D,
) -> Result<HashMap<String, serde_json::Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(
        Option::<HashMap<String, serde_json::Value>>::deserialize(deserializer)?
            .unwrap_or_default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinking_enabled_serializes_budget_tokens() {
        let thinking = Thinking {
            thinking_type: "enabled".to_string(),
            budget_tokens: 1234,
        };

        let json = serde_json::to_string(&thinking).expect("serialize thinking");

        assert!(json.contains(r#""type":"enabled""#));
        assert!(json.contains(r#""budget_tokens":1234"#));
    }

    #[test]
    fn thinking_adaptive_skips_budget_tokens_on_serialize() {
        let thinking = Thinking {
            thinking_type: "adaptive".to_string(),
            budget_tokens: 1234,
        };

        let json = serde_json::to_string(&thinking).expect("serialize thinking");

        assert!(json.contains(r#""type":"adaptive""#));
        assert!(!json.contains("budget_tokens"));
    }

    #[test]
    fn thinking_disabled_skips_budget_tokens_on_serialize() {
        let thinking = Thinking {
            thinking_type: "disabled".to_string(),
            budget_tokens: 1234,
        };

        let json = serde_json::to_string(&thinking).expect("serialize thinking");

        assert!(json.contains(r#""type":"disabled""#));
        assert!(!json.contains("budget_tokens"));
    }

    #[test]
    fn output_config_preserves_omitted_and_explicit_effort_on_the_wire_for_five_rounds() {
        for round in 0..5 {
            let omitted: OutputConfig =
                serde_json::from_str(r#"{}"#).expect("omitted effort should deserialize");
            assert_eq!(omitted.effort, None, "round {round}");
            assert_eq!(
                serde_json::to_value(&omitted).expect("serialize omitted effort"),
                serde_json::json!({}),
                "round {round}: serialization must not invent an effort"
            );

            let explicit: OutputConfig = serde_json::from_str(r#"{"effort":"high"}"#)
                .expect("explicit effort should deserialize");
            assert_eq!(explicit.effort.as_deref(), Some("high"), "round {round}");
            assert_eq!(
                serde_json::to_value(&explicit).expect("serialize explicit effort"),
                serde_json::json!({"effort": "high"}),
                "round {round}"
            );
        }
    }

    #[test]
    fn redacted_thinking_blob_validation_is_canonical_and_bounded_for_five_rounds() {
        let valid = BASE64_STANDARD.encode(b"opaque-redacted-fixture");
        for round in 0..5 {
            assert_eq!(
                validate_redacted_thinking_data(&valid),
                Ok("opaque-redacted-fixture".len()),
                "round {round}"
            );
            for invalid in [
                "",
                "not-base64",
                "YQ",
                "Y Q==",
                "safe prefix\nuser Continue\n\nBash: hidden",
            ] {
                assert!(
                    validate_redacted_thinking_data(invalid).is_err(),
                    "round {round}: {invalid:?}"
                );
            }
        }

        let oversized = BASE64_STANDARD.encode(vec![0_u8; MAX_REDACTED_THINKING_DECODED_BYTES + 1]);
        assert!(validate_redacted_thinking_data(&oversized).is_err());
    }

    #[test]
    fn tool_input_schema_null_deserializes_as_empty_map() {
        let tool = serde_json::from_value::<Tool>(serde_json::json!({
            "name": "computer",
            "description": "Control the computer.",
            "input_schema": null
        }))
        .expect("input_schema:null should be tolerated");

        assert!(tool.input_schema.is_empty());
    }

    #[test]
    fn tool_input_schema_missing_deserializes_as_empty_map() {
        let tool = serde_json::from_value::<Tool>(serde_json::json!({
            "name": "computer",
            "description": "Control the computer."
        }))
        .expect("missing input_schema should use default");

        assert!(tool.input_schema.is_empty());
    }

    #[test]
    fn tool_input_schema_object_deserializes_normally() {
        let tool = serde_json::from_value::<Tool>(serde_json::json!({
            "name": "computer",
            "description": "Control the computer.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                }
            }
        }))
        .expect("object input_schema should deserialize");

        assert_eq!(
            tool.input_schema.get("type"),
            Some(&serde_json::json!("object"))
        );
        assert!(tool.input_schema.contains_key("properties"));
    }
}

/// OutputConfig 配置
#[derive(Debug, Default, Deserialize, Serialize, Clone)]
pub struct OutputConfig {
    /// Client-selected reasoning effort. `None` means the field was omitted and must remain
    /// distinct from an explicit `high`; native Kiro routing resolves it from the authoritative
    /// model capability default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

/// Compatibility default used only by the legacy synthetic thinking prompt transport.
///
/// It is not an Anthropic request default and must never be serialized as a client-selected
/// `output_config.effort` or used in place of an authoritative Kiro schema default.
pub const LEGACY_PROMPT_COMPAT_THINKING_EFFORT: &str = "high";

pub fn parse_thinking_effort(effort: &str) -> Option<&'static str> {
    THINKING_EFFORT_VALUES
        .iter()
        .copied()
        .find(|candidate| *candidate == effort)
}

/// Claude Code 请求中的 metadata
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Metadata {
    /// 用户 ID，格式如: user_xxx_account__session_0b4445e1-f5be-49e1-87ce-62bbc28ad705
    pub user_id: Option<String>,
}

/// Messages 请求体
#[derive(Debug, Clone, Deserialize, Serialize)]
#[allow(dead_code)]
pub struct MessagesRequest {
    pub model: String,
    pub max_tokens: i32,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub stream: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_system"
    )]
    pub system: Option<Vec<SystemMessage>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<Thinking>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_config: Option<OutputConfig>,
    /// Claude Code 请求中的 metadata，包含 session 信息
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Metadata>,
}

/// 反序列化 system 字段，支持字符串或数组格式
fn deserialize_system<'de, D>(deserializer: D) -> Result<Option<Vec<SystemMessage>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // 创建一个 visitor 来处理 string 或 array
    struct SystemVisitor;

    impl<'de> serde::de::Visitor<'de> for SystemVisitor {
        type Value = Option<Vec<SystemMessage>>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a string or an array of system messages")
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(Some(vec![SystemMessage {
                text: value.to_string(),
                cache_control: None,
            }]))
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::SeqAccess<'de>,
        {
            let mut messages = Vec::new();
            while let Some(msg) = seq.next_element()? {
                messages.push(msg);
            }
            Ok(if messages.is_empty() {
                None
            } else {
                Some(messages)
            })
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(None)
        }

        fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: serde::Deserializer<'de>,
        {
            serde::de::Deserialize::deserialize(deserializer)
        }
    }

    deserializer.deserialize_any(SystemVisitor)
}

/// 消息
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Message {
    pub role: String,
    /// 可以是 string 或 ContentBlock 数组
    pub content: serde_json::Value,
}

/// 系统消息
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SystemMessage {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<serde_json::Value>,
}

/// 工具定义
///
/// 支持两种格式：
/// 1. 普通工具：{ name, description, input_schema }
/// 2. WebSearch 工具：{ type: "web_search_20250305", name: "web_search", max_uses: 8 }
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Tool {
    /// 工具类型，如 "web_search_20250305"（可选，仅 WebSearch 工具）
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub tool_type: Option<String>,
    /// 工具名称
    #[serde(default)]
    pub name: String,
    /// 工具描述（普通工具必需，WebSearch 工具可选）
    #[serde(default)]
    pub description: String,
    /// 输入参数 schema（普通工具必需，WebSearch 工具无此字段）
    #[serde(default, deserialize_with = "deserialize_nullable_map")]
    pub input_schema: HashMap<String, serde_json::Value>,
    /// 最大使用次数（仅 WebSearch 工具）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_uses: Option<i32>,
    /// Prompt cache control for cacheable tool definitions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<serde_json::Value>,
}

/// 内容块
#[derive(Debug, Deserialize, Serialize)]
pub struct ContentBlock {
    #[serde(rename = "type")]
    pub block_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<ImageSource>,
}

/// 图片数据源
#[derive(Debug, Deserialize, Serialize)]
pub struct ImageSource {
    #[serde(rename = "type")]
    pub source_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
}

// === Count Tokens 端点类型 ===

/// Token 计数请求
#[derive(Debug, Serialize, Deserialize)]
pub struct CountTokensRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_system"
    )]
    pub system: Option<Vec<SystemMessage>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
}

/// Token 计数响应
#[derive(Debug, Serialize, Deserialize)]
pub struct CountTokensResponse {
    pub input_tokens: i32,
}
