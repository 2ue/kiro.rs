//! Anthropic API 类型定义

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
///
/// 序列化结果是 Claude Code 协议 `/v1/models` 形态（`type: "model"`、`display_name`、
/// RFC3339 `created_at`）的超集，保留旧的 `object`/`created`/`owned_by` 字段兼容已有客户端。
#[derive(Debug, Serialize, Clone)]
pub struct Model {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub created_at: String,
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

/// 模型列表响应（Claude Code 协议分页字段 + 旧 `object` 字段）
#[derive(Debug, Serialize)]
pub struct ModelsResponse {
    pub object: String,
    pub data: Vec<Model>,
    pub has_more: bool,
    pub first_id: Option<String>,
    pub last_id: Option<String>,
}

impl ModelsResponse {
    pub fn from_models(data: Vec<Model>) -> Self {
        Self {
            object: "list".to_string(),
            first_id: data.first().map(|model| model.id.clone()),
            last_id: data.last().map(|model| model.id.clone()),
            has_more: false,
            data,
        }
    }
}

/// RFC3339 form of a Unix timestamp for the `created_at` model field.
pub fn model_created_at_rfc3339(created: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(created, 0)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

// === Messages 端点类型 ===

pub const THINKING_EFFORT_VALUES: &[&str] = &["low", "medium", "high", "xhigh", "max"];

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
    /// `thinking.display`（`summarized` / `omitted`）。保留原始 JSON 值，
    /// 非法值不报错，按默认处理（见 [`Thinking::display_mode`]）。
    #[serde(default)]
    pub display: Option<serde_json::Value>,
}

/// `thinking.display` 的解析结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThinkingDisplay {
    /// 默认：正常下发 thinking 文本。
    #[default]
    Summarized,
    /// thinking 块只下发空文本和 signature。
    Omitted,
}

impl ThinkingDisplay {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Summarized => "summarized",
            Self::Omitted => "omitted",
        }
    }
}

impl Thinking {
    /// 是否启用了 thinking（enabled 或 adaptive）
    pub fn is_enabled(&self) -> bool {
        self.thinking_type == "enabled" || self.thinking_type == "adaptive"
    }

    /// 解析 `thinking.display`；缺省、`null` 与非法值都返回 `None`。
    pub fn parsed_display(&self) -> Option<ThinkingDisplay> {
        match self.display.as_ref()?.as_str()? {
            "summarized" => Some(ThinkingDisplay::Summarized),
            "omitted" => Some(ThinkingDisplay::Omitted),
            _ => None,
        }
    }

    /// 宽容解析 `thinking.display`：非法值记录日志并按默认（summarized）处理。
    pub fn display_mode(&self) -> ThinkingDisplay {
        match (self.display.as_ref(), self.parsed_display()) {
            (_, Some(display)) => display,
            (None | Some(serde_json::Value::Null), None) => ThinkingDisplay::default(),
            (Some(value), None) => {
                tracing::warn!(
                    thinking_display = %value,
                    "ignoring unsupported thinking.display value; using default display"
                );
                ThinkingDisplay::default()
            }
        }
    }
}

impl Serialize for Thinking {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // 只转发合法的 display 值，非法值已按默认处理。
        let display = self.parsed_display().filter(|_| self.is_enabled());
        let field_count = if self.thinking_type == "enabled" {
            2
        } else {
            1
        } + usize::from(display.is_some());
        let mut state = serializer.serialize_struct("Thinking", field_count)?;
        state.serialize_field("type", &self.thinking_type)?;
        if self.thinking_type == "enabled" {
            state.serialize_field("budget_tokens", &self.budget_tokens)?;
        }
        if let Some(display) = display {
            state.serialize_field("display", display.as_str())?;
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

/// 宽容解析 stop_sequences：单个字符串视为一项，数组中只取非空字符串，
/// 其余形态忽略并记日志，不因格式问题拒绝整个请求。
fn deserialize_stop_sequences<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(normalize_stop_sequences(value))
}

fn normalize_stop_sequences(value: Option<serde_json::Value>) -> Option<Vec<String>> {
    let sequences = match value? {
        serde_json::Value::Null => return None,
        serde_json::Value::String(sequence) => vec![sequence],
        serde_json::Value::Array(items) => {
            let total = items.len();
            let sequences: Vec<String> = items
                .into_iter()
                .filter_map(|item| match item {
                    serde_json::Value::String(sequence) => Some(sequence),
                    _ => None,
                })
                .collect();
            if sequences.len() != total {
                tracing::warn!(
                    ignored = total - sequences.len(),
                    "ignoring non-string stop_sequences entries"
                );
            }
            sequences
        }
        other => {
            tracing::warn!(
                kind = json_value_kind(&other),
                "ignoring stop_sequences that is neither a string nor an array"
            );
            return None;
        }
    };
    let sequences: Vec<String> = sequences.into_iter().filter(|s| !s.is_empty()).collect();
    (!sequences.is_empty()).then_some(sequences)
}

fn json_value_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
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
    fn stop_sequences_are_parsed_leniently() {
        let parse = |stop_sequences: serde_json::Value| -> Option<Vec<String>> {
            serde_json::from_value::<MessagesRequest>(serde_json::json!({
                "model": "claude-sonnet-4-5",
                "max_tokens": 16,
                "messages": [{"role": "user", "content": "hi"}],
                "stop_sequences": stop_sequences,
            }))
            .expect("request with any stop_sequences shape still parses")
            .stop_sequences
        };

        assert_eq!(
            parse(serde_json::json!(["END", "STOP"])),
            Some(vec!["END".to_string(), "STOP".to_string()])
        );
        assert_eq!(
            parse(serde_json::json!("END")),
            Some(vec!["END".to_string()])
        );
        assert_eq!(
            parse(serde_json::json!(["END", 3, null, "", {"a": 1}])),
            Some(vec!["END".to_string()])
        );
        assert_eq!(parse(serde_json::json!([1, 2])), None);
        assert_eq!(parse(serde_json::json!(42)), None);
        assert_eq!(parse(serde_json::json!({"a": "END"})), None);
        assert_eq!(parse(serde_json::Value::Null), None);
    }

    #[test]
    fn thinking_display_is_parsed_leniently() {
        let parse = |value: serde_json::Value| -> Thinking {
            serde_json::from_value(value).expect("thinking")
        };

        let omitted = parse(serde_json::json!({"type": "adaptive", "display": "omitted"}));
        assert_eq!(omitted.display_mode(), ThinkingDisplay::Omitted);
        let summarized = parse(serde_json::json!({"type": "adaptive", "display": "summarized"}));
        assert_eq!(summarized.display_mode(), ThinkingDisplay::Summarized);
        let missing = parse(serde_json::json!({"type": "adaptive"}));
        assert_eq!(missing.display_mode(), ThinkingDisplay::Summarized);
        // 非法字符串与非法类型都不报错，按默认处理。
        let bogus = parse(serde_json::json!({"type": "adaptive", "display": "bogus"}));
        assert_eq!(bogus.display_mode(), ThinkingDisplay::Summarized);
        let wrong_type = parse(serde_json::json!({"type": "adaptive", "display": 3}));
        assert_eq!(wrong_type.display_mode(), ThinkingDisplay::Summarized);

        assert_eq!(
            serde_json::to_value(&omitted).unwrap(),
            serde_json::json!({"type": "adaptive", "display": "omitted"})
        );
        assert_eq!(
            serde_json::to_value(&bogus).unwrap(),
            serde_json::json!({"type": "adaptive"})
        );
    }

    #[test]
    fn thinking_enabled_serializes_budget_tokens() {
        let thinking = Thinking {
            thinking_type: "enabled".to_string(),
            budget_tokens: 1234,
            display: None,
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
            display: None,
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
            display: None,
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
    /// 自定义停止序列。Kiro 无对应参数，由代理在输出侧检测并截断。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_stop_sequences"
    )]
    pub stop_sequences: Option<Vec<String>>,
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
    /// 仅返回这些域名（含子域名）的结果（仅 WebSearch 工具）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_domains: Option<Vec<String>>,
    /// 排除这些域名（含子域名）的结果（仅 WebSearch 工具）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_domains: Option<Vec<String>>,
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
