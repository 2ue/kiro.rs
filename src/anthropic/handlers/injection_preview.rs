//! 管理端"注入预览"：按给定的路由和请求条件构造一个合成请求，走和真实请求相同的注入与
//! 上游 转换代码，返回每个注入项的结果和发给上游的完整文本。不访问上游，不写缓存或 usage。

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::*;
use crate::model::config::{InjectionId, ResolvedInjections};
use crate::model::prompt_injection::BlockReason;

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PreviewToolChoice {
    Auto,
    Any,
    Tool,
    None,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PreviewThinking {
    Off,
    Enabled,
    Adaptive,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PreviewLanguage {
    /// 请求里没有可识别的语言信号。
    None,
    /// 客户端在 system 里声明了语言（例如 Claude Code 的 language 设置）。
    Declared,
}

fn default_model() -> String {
    "claude-sonnet-4-5".to_string()
}

/// 管理端会把它和 `draft` 平铺在同一个对象里（`#[serde(flatten)]`），所以这里不能拒绝未知字段。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PromptInjectionPreviewRequest {
    /// 路由前缀，例如 `/cc`、`/v1`、`/dfcache/team`。
    pub route: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_true")]
    pub with_system: bool,
    #[serde(default = "default_true")]
    pub with_write_edit: bool,
    #[serde(default)]
    pub disable_parallel_tool_use: bool,
    #[serde(default = "default_tool_choice")]
    pub tool_choice: PreviewToolChoice,
    #[serde(default = "default_thinking")]
    pub thinking: PreviewThinking,
    #[serde(default = "default_language")]
    pub language: PreviewLanguage,
    #[serde(default)]
    pub stream: bool,
}

fn default_true() -> bool {
    true
}

fn default_tool_choice() -> PreviewToolChoice {
    PreviewToolChoice::Auto
}
fn default_thinking() -> PreviewThinking {
    PreviewThinking::Off
}
fn default_language() -> PreviewLanguage {
    PreviewLanguage::None
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PreviewItem {
    pub id: InjectionId,
    /// `applied`：已注入；`not_triggered`：允许但这个请求不满足触发条件；`blocked`：开关或模式不允许。
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked: Option<BlockReason>,
    /// 决定开关的路由前缀；为空表示来自全局默认。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PreviewFeatures {
    pub thinking_model_name: bool,
    pub thinking_trigger: crate::model::config::ThinkingTriggerMode,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PromptInjectionPreviewResponse {
    pub endpoint: String,
    pub items: Vec<PreviewItem>,
    pub features: PreviewFeatures,
    /// 账号：发往外部上游的 system（Anthropic 格式）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_system: Option<Vec<String>>,
}

/// 合成请求的固定内容。用户文字用英文，避免语言识别把它当成语言信号。
const PREVIEW_SYSTEM: &str = "You are a coding assistant.";
const PREVIEW_DECLARED_LANGUAGE: &str = "\n# Language\nAlways respond in Chinese.";
const PREVIEW_USER_TEXT: &str = "Please update the file.";

pub(super) fn preview_payload(req: &PromptInjectionPreviewRequest) -> MessagesRequest {
    let mut tools = vec![json!({
        "name": "Read",
        "description": "Reads a file from the local filesystem.",
        "input_schema": {"type": "object", "properties": {"file_path": {"type": "string"}}, "required": ["file_path"]}
    })];
    if req.with_write_edit {
        tools.push(json!({
            "name": "Write",
            "description": "Writes a file to the local filesystem.",
            "input_schema": {"type": "object", "properties": {"file_path": {"type": "string"}, "content": {"type": "string"}}, "required": ["file_path", "content"]}
        }));
        tools.push(json!({
            "name": "Edit",
            "description": "Performs exact string replacements in files.",
            "input_schema": {"type": "object", "properties": {"file_path": {"type": "string"}, "old_string": {"type": "string"}, "new_string": {"type": "string"}}, "required": ["file_path", "old_string", "new_string"]}
        }));
    }
    let mut tool_choice = match req.tool_choice {
        PreviewToolChoice::Auto => json!({"type": "auto"}),
        PreviewToolChoice::Any => json!({"type": "any"}),
        PreviewToolChoice::Tool => json!({"type": "tool", "name": "Read"}),
        PreviewToolChoice::None => json!({"type": "none"}),
    };
    if req.disable_parallel_tool_use && req.tool_choice != PreviewToolChoice::None {
        tool_choice["disable_parallel_tool_use"] = json!(true);
    }
    let system = req.with_system.then(|| {
        let mut text = PREVIEW_SYSTEM.to_string();
        if req.language == PreviewLanguage::Declared {
            text.push_str(PREVIEW_DECLARED_LANGUAGE);
        }
        json!([{"type": "text", "text": text}])
    });
    let thinking = match req.thinking {
        PreviewThinking::Off => Value::Null,
        PreviewThinking::Enabled => json!({"type": "enabled", "budget_tokens": 4096}),
        PreviewThinking::Adaptive => json!({"type": "adaptive"}),
    };
    let mut body = json!({
        "model": req.model,
        "max_tokens": 8192,
        "stream": req.stream,
        "tools": tools,
        "tool_choice": tool_choice,
        "messages": [{"role": "user", "content": PREVIEW_USER_TEXT}],
    });
    if let Some(system) = system {
        body["system"] = system;
    }
    if !thinking.is_null() {
        body["thinking"] = thinking;
    }
    serde_json::from_value(body).expect("preview payload is a valid MessagesRequest")
}

fn normalize_route(route: &str) -> Result<String, String> {
    crate::model::config::normalize_reported_usage_path_prefix(route)
        .filter(|route| route != "/")
        .ok_or_else(|| format!("无效的路由：{route}"))
}

/// 路由对应的 messages 入口。内置 `/v1` 直接挂在 `/v1/messages`，其余路由是 `{route}/v1/messages`。
pub(super) fn messages_endpoint(route: &str) -> String {
    if route == "/v1" {
        "/v1/messages".to_string()
    } else {
        format!("{route}/v1/messages")
    }
}

fn item_statuses(injections: &ResolvedInjections, applied: &[InjectionId]) -> Vec<PreviewItem> {
    injections
        .items()
        .iter()
        .map(|item| PreviewItem {
            id: item.id,
            status: if applied.contains(&item.id) {
                "applied"
            } else if item.allowed {
                "not_triggered"
            } else {
                "blocked"
            },
            blocked: item.blocked,
            source: item.source.clone(),
        })
        .collect()
}

/// 运行预览。`config` 可以是还没保存的草稿。
pub(crate) fn preview_prompt_injection(
    config: &Config,
    req: PromptInjectionPreviewRequest,
) -> Result<PromptInjectionPreviewResponse, String> {
    let route = normalize_route(&req.route)?;
    let endpoint = messages_endpoint(&route);
    let mut runtime_config = RequestRuntimeConfig::from_config(config);
    runtime_config.prompt_injection = config.prompt_injection.clone().normalized();
    runtime_config.route_features = config.route_features.clone().normalized();
    let features = runtime_config.route_features_for(&endpoint);
    let preview_features = PreviewFeatures {
        thinking_model_name: features.thinking_model_name,
        thinking_trigger: features.thinking_trigger,
    };
    let mut payload = preview_payload(&req);

    let injections = runtime_config.injections_for(&endpoint);
    let applied =
        super::super::prompt_injection::apply_to_messages_request(&injections, &mut payload);
    Ok(PromptInjectionPreviewResponse {
        endpoint,
        items: item_statuses(&injections, &applied.items),
        features: preview_features,
        external_system: payload
            .system
            .map(|blocks| blocks.into_iter().map(|block| block.text).collect()),
    })
}
