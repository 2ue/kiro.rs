//! Anthropic → Kiro 协议转换器
//!
//! 负责将 Anthropic API 请求格式转换为 Kiro API 请求格式

use std::collections::HashMap;

#[cfg(test)]
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::anthropic::body_capabilities::KiroConverterPlan;
use crate::anthropic::model_capabilities::ModelResolution;
use crate::anthropic::prompt_cache::canonicalize_cache_value;
use crate::anthropic::tool_schema_keys::ToolSchemaKeyMap;
#[cfg(test)]
use crate::kiro::model::requests::conversation::{
    AssistantMessage, HistoryAssistantMessage, HistoryUserMessage, Message, UserMessage,
};
use crate::kiro::model::requests::conversation::{
    ConversationState, CurrentMessage, UserInputMessage, UserInputMessageContext,
};
use crate::kiro::model::requests::kiro::AdditionalModelRequestFields;
#[cfg(test)]
use crate::kiro::model::requests::tool::ToolResult;
use crate::model::config::{CompatProfile, PromptCacheSimulationMode, PromptSteeringConfig};

#[cfg(test)]
use super::types::ContentBlock;
use super::types::MessagesRequest;

#[path = "converter/content.rs"]
mod content;
#[path = "converter/history.rs"]
mod history;
#[path = "converter/model.rs"]
mod model;
#[path = "converter/schema.rs"]
mod schema;
#[path = "converter/thinking.rs"]
mod thinking;
#[path = "converter/tool_pairing.rs"]
mod tool_pairing;
#[path = "converter/tools.rs"]
mod tools;

use content::process_message_content;
#[cfg(test)]
use content::sanitize_tool_use_id;
pub(crate) use content::{infer_document_media_type_from_url, infer_image_format_from_url};
use history::build_history;
#[cfg(test)]
use history::{convert_assistant_message, merge_assistant_messages};
use model::build_additional_model_request_fields;
pub use model::{get_context_window_size, map_model};
#[cfg(test)]
use schema::normalize_json_schema;
#[cfg(test)]
use tool_pairing::kiro_tool_result_to_text;
use tool_pairing::{
    append_orphan_tool_result_texts, remove_orphaned_tool_uses, validate_tool_pairing,
};
#[cfg(test)]
use tools::{
    SYSTEM_CHUNKED_POLICY, TOOL_HASH_MARKER, TOOL_NAME_MAX_LEN, map_tool_name, shorten_tool_name,
};
use tools::{collect_history_tool_names, convert_tools, create_placeholder_tool};

const TOOL_RESULTS_PROVIDED_PLACEHOLDER: &str = "Continue";
const EMPTY_TOOL_RESULT_CONTENT_PLACEHOLDER: &str = "Tool result content was empty.";

/// 转换结果
#[derive(Debug)]
pub struct ConversionResult {
    /// 转换后的 Kiro 请求
    pub conversation_state: ConversationState,
    /// 最终 Kiro tools 数组里需要在对应工具后插入 cachePoint 的工具下标。
    pub tool_cache_point_insert_after: Vec<usize>,
    /// 是否把 cachePoint 插入计划记录到 payload diagnostics。
    pub cache_point_plan_recording_enabled: bool,
    /// 工具名称映射（短名称 → 原始名称），仅当存在超长工具名时非空
    pub tool_name_map: HashMap<String, String>,
    /// 工具 input_schema property key 映射（上游工具名 → 清洗 key 到原始 key），仅在本次请求内使用。
    pub tool_schema_key_map: ToolSchemaKeyMap,
    /// 本次请求声明并实际发给上游的工具名集合，包含原始名和因长度限制生成的短名。
    ///
    /// 仅用于下游响应容错：当上游把工具调用泄漏为字面 `<invoke>` 文本时，只有工具名命中
    /// 这个集合才允许恢复成结构化 `tool_use`，避免误执行正文中展示的 XML。
    pub known_tool_names: std::collections::HashSet<String>,
    /// 代理对入参的隐式改写汇总（兜底动作的统计），用于可选的 `x-kiro-rs-warnings` 响应头。
    pub warnings: ProxyWarnings,
    /// Kiro 原生模型扩展字段，例如 reasoning effort。
    pub additional_model_request_fields: Option<AdditionalModelRequestFields>,
}

#[derive(Debug, Clone)]
pub struct ConverterOptions {
    pub compat_profile: CompatProfile,
    pub conversion: KiroConverterPlan,
    pub prompt_cache_simulation_mode: PromptCacheSimulationMode,
    pub kiro_cache_point_enabled: bool,
    pub kiro_cache_point_tools_only: bool,
    pub kiro_cache_point_record_plan: bool,
    pub force_visible_thinking: bool,
    pub prompt_steering: PromptSteeringConfig,
}

impl Default for ConverterOptions {
    fn default() -> Self {
        Self {
            compat_profile: CompatProfile::ClaudeCode,
            conversion: KiroConverterPlan::default(),
            prompt_cache_simulation_mode: PromptCacheSimulationMode::HighCache,
            kiro_cache_point_enabled: false,
            kiro_cache_point_tools_only: true,
            kiro_cache_point_record_plan: true,
            force_visible_thinking: false,
            prompt_steering: PromptSteeringConfig::default(),
        }
    }
}

impl ConverterOptions {
    fn is_strict(&self) -> bool {
        self.compat_profile.is_strict()
    }

    fn inject_chunked_policy(&self) -> bool {
        let prompt_steering = self.prompt_steering.clone().normalized();
        !self.is_strict()
            && prompt_steering.enabled
            && prompt_steering.chunked_write.enabled
            && prompt_steering.chunked_write.system_prompt_enabled
            && self.conversion.chunked_tool_policy.is_enabled()
    }

    fn inject_chunked_tool_descriptions(&self) -> bool {
        let prompt_steering = self.prompt_steering.clone().normalized();
        !self.is_strict()
            && prompt_steering.enabled
            && prompt_steering.chunked_write.enabled
            && prompt_steering.chunked_write.tool_description_enabled
            && self.conversion.chunked_tool_policy.is_enabled()
    }

    fn inject_thinking_prefix(&self) -> bool {
        let prompt_steering = self.prompt_steering.clone().normalized();
        self.conversion.thinking_prompt_controls.is_enabled()
            && prompt_steering.enabled
            && prompt_steering.thinking.enabled
            && (self.force_visible_thinking || !self.is_strict())
    }

    fn inject_tool_choice_prefix(&self) -> bool {
        let prompt_steering = self.prompt_steering.clone().normalized();
        !self.is_strict()
            && prompt_steering.enabled
            && prompt_steering.tool_choice.enabled
            && self.conversion.tool_choice_steering.is_enabled()
    }

    fn tool_choice_steering_enabled(&self) -> bool {
        let prompt_steering = self.prompt_steering.clone().normalized();
        prompt_steering.enabled
            && prompt_steering.tool_choice.enabled
            && self.conversion.tool_choice_steering.is_enabled()
    }
}

/// 代理在请求转换过程中执行的兜底改写计数
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ProxyWarnings {
    /// 末尾 assistant 消息（prefill）被丢弃的次数
    pub prefill_dropped: u32,
    /// 因找不到对应 tool_use 而被跳过的当前轮 tool_result
    pub orphan_tool_results: u32,
    /// 孤立 tool_result 被转成普通文本保留的次数
    pub orphan_tool_results_textified: u32,
    /// 因找不到对应 tool_result 而被从历史移除的 tool_use
    pub orphan_tool_uses: u32,
    /// 历史中重复出现的 tool_result（已配对过）被跳过
    pub duplicate_tool_results: u32,
    /// 重复 tool_result 被转成普通文本保留的次数
    pub duplicate_tool_results_textified: u32,
    /// user 消息只有 tool_result 且文本为空时补了 Kiro content 占位
    pub tool_result_content_placeholders: u32,
    /// user 消息没有文本也没有 tool_result 时补了 Continue 占位
    pub empty_content_placeholders: u32,
}

impl ProxyWarnings {
    /// 编码为 `x-kiro-rs-warnings` 头值（仅包含计数 > 0 的项）。
    pub fn encode_header(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if self.prefill_dropped > 0 {
            parts.push(format!("prefill-dropped={}", self.prefill_dropped));
        }
        if self.orphan_tool_results > 0 {
            parts.push(format!("orphan-tool-result={}", self.orphan_tool_results));
        }
        if self.orphan_tool_results_textified > 0 {
            parts.push(format!(
                "orphan-tool-result-textified={}",
                self.orphan_tool_results_textified
            ));
        }
        if self.orphan_tool_uses > 0 {
            parts.push(format!("orphan-tool-use={}", self.orphan_tool_uses));
        }
        if self.duplicate_tool_results > 0 {
            parts.push(format!(
                "duplicate-tool-result={}",
                self.duplicate_tool_results
            ));
        }
        if self.duplicate_tool_results_textified > 0 {
            parts.push(format!(
                "duplicate-tool-result-textified={}",
                self.duplicate_tool_results_textified
            ));
        }
        if self.tool_result_content_placeholders > 0 {
            parts.push(format!(
                "tool-result-content-placeholder={}",
                self.tool_result_content_placeholders
            ));
        }
        if self.empty_content_placeholders > 0 {
            parts.push(format!(
                "empty-content-placeholder={}",
                self.empty_content_placeholders
            ));
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(","))
        }
    }
}

/// 转换错误
#[derive(Debug)]
pub enum ConversionError {
    UnsupportedModel(String),
    EmptyMessages,
    UnsupportedContent(String),
}

impl std::fmt::Display for ConversionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConversionError::UnsupportedModel(model) => write!(f, "模型不支持: {}", model),
            ConversionError::EmptyMessages => write!(f, "消息列表为空"),
            ConversionError::UnsupportedContent(message) => write!(f, "内容块不支持: {}", message),
        }
    }
}

impl std::error::Error for ConversionError {}

/// 从 metadata.user_id 中提取 session UUID
///
/// 支持两种格式:
/// 1. 字符串格式: user_xxx_account__session_0b4445e1-f5be-49e1-87ce-62bbc28ad705
/// 2. JSON 格式: {"device_id":"...","account_uuid":"...","session_id":"UUID"}
///
/// 提取 session UUID 作为 conversationId
fn extract_session_id(user_id: &str) -> Option<String> {
    // 先尝试 JSON 解析
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(user_id) {
        if let Some(session_id) = json.get("session_id").and_then(|v| v.as_str()) {
            if is_valid_uuid(session_id) {
                return Some(session_id.to_string());
            }
        }
    }

    // 回退到字符串格式: 查找 "session_" 后面的内容
    if let Some(pos) = user_id.find("session_") {
        let session_part = &user_id[pos + 8..]; // "session_" 长度为 8
        if session_part.len() >= 36 {
            let uuid_str = &session_part[..36];
            if is_valid_uuid(uuid_str) {
                return Some(uuid_str.to_string());
            }
        }
    }
    None
}

pub(crate) fn extract_metadata_conversation_id(req: &MessagesRequest) -> Option<String> {
    req.metadata
        .as_ref()
        .and_then(|m| m.user_id.as_ref())
        .and_then(|user_id| extract_session_id(user_id))
}

pub(crate) fn extract_stable_conversation_id(req: &MessagesRequest) -> Option<String> {
    extract_metadata_conversation_id(req).or_else(|| derive_fallback_conversation_id(req))
}

fn conversation_id_for_options(
    req: &MessagesRequest,
    options: &ConverterOptions,
) -> Option<String> {
    match options.prompt_cache_simulation_mode {
        PromptCacheSimulationMode::HighCache => extract_stable_conversation_id(req),
        PromptCacheSimulationMode::Disabled => extract_metadata_conversation_id(req),
    }
}

fn derive_fallback_conversation_id(req: &MessagesRequest) -> Option<String> {
    let seed = if let Some(first_user_message) =
        req.messages.iter().find(|message| message.role == "user")
    {
        serde_json::json!({
            "system": &req.system,
            "tools": &req.tools,
            "first_user_message": first_user_message,
        })
    } else {
        serde_json::json!({
            "system": &req.system,
            "tools": &req.tools,
            "messages": &req.messages,
        })
    };

    Some(deterministic_conversation_id(&canonicalize_cache_value(
        &seed,
    )))
}

fn deterministic_conversation_id(seed: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"kiro.rs:anthropic:conversation-id:v1:");
    hasher.update(seed.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes).to_string()
}

/// 简单验证 UUID 格式（36 字符，包含 4 个连字符）
fn is_valid_uuid(s: &str) -> bool {
    s.len() == 36 && s.chars().filter(|c| *c == '-').count() == 4
}

/// 将 Anthropic 请求转换为 Kiro 请求
#[allow(dead_code)]
pub fn convert_request(req: &MessagesRequest) -> Result<ConversionResult, ConversionError> {
    convert_request_with_options(req, ConverterOptions::default())
}

/// 将 Anthropic 请求转换为 Kiro 请求，并按兼容 profile 控制代理侧改写。
pub fn convert_request_with_options(
    req: &MessagesRequest,
    options: ConverterOptions,
) -> Result<ConversionResult, ConversionError> {
    let model_id = map_model(&req.model)
        .ok_or_else(|| ConversionError::UnsupportedModel(req.model.clone()))?;
    convert_request_with_model_id(req, options, model_id)
}

/// 将 Anthropic 请求转换为 Kiro 请求，并使用已经按当前 Kiro 上游目录解析过的模型 ID。
pub fn convert_request_with_resolved_model(
    req: &MessagesRequest,
    options: ConverterOptions,
    resolution: &ModelResolution,
) -> Result<ConversionResult, ConversionError> {
    let model_id = resolution
        .upstream_model
        .clone()
        .ok_or_else(|| ConversionError::UnsupportedModel(req.model.clone()))?;
    convert_request_with_model_id(req, options, model_id)
}

fn convert_request_with_model_id(
    req: &MessagesRequest,
    options: ConverterOptions,
    model_id: String,
) -> Result<ConversionResult, ConversionError> {
    let mut warnings = ProxyWarnings::default();

    // 2. 检查消息列表
    if req.messages.is_empty() {
        return Err(ConversionError::EmptyMessages);
    }

    // 2.5. 预处理 prefill：如果末尾不是 user，静默丢弃尾部 prefill 并截断到最后一条 user
    // Claude 4.x 已弃用 assistant prefill，Kiro API 也不接受 assistant 作为最终消息
    let messages: &[_] = if req.messages.last().is_some_and(|m| m.role != "user") {
        warnings.prefill_dropped += 1;
        tracing::info!("检测到末尾非 user 消息（prefill），静默丢弃");
        let last_user_idx = req
            .messages
            .iter()
            .rposition(|m| m.role == "user")
            .ok_or(ConversionError::EmptyMessages)?;
        &req.messages[..=last_user_idx]
    } else {
        &req.messages
    };

    // 3. 生成会话 ID 和代理 ID
    // High-cache 模式下缺失 metadata 时从稳定请求锚点派生确定性 UUID；
    // 其他模式保持旧语义，只信任显式 metadata session。
    let conversation_id =
        conversation_id_for_options(req, &options).unwrap_or_else(|| Uuid::new_v4().to_string());
    let agent_continuation_id = Uuid::new_v4().to_string();

    // 4. 确定触发类型
    let chat_trigger_type = determine_chat_trigger_type(req);

    // 5. 处理最后一条消息作为 current_message（经过 prefill 预处理，末尾必为 user）
    let last_message = messages.last().unwrap();
    let (text_content, images, tool_results) = process_message_content(&last_message.content)?;

    // 6. 转换工具定义（超长名称自动缩短并记录映射）
    let mut tool_name_map = HashMap::new();
    let converted_tools = convert_tools(
        &req.tools,
        &req.tool_choice,
        &mut tool_name_map,
        options.clone(),
    )?;
    let mut tools = converted_tools.tools;
    let mut known_tool_names: std::collections::HashSet<String> = tools
        .iter()
        .map(|tool| tool.tool_specification.name.clone())
        .collect();
    for original_name in tool_name_map.values() {
        known_tool_names.insert(original_name.clone());
    }

    // 7. 构建历史消息（需要先构建，以便收集历史中使用的工具）
    let mut history = build_history(
        req,
        messages,
        &model_id,
        &mut tool_name_map,
        options.clone(),
    )?;

    // 8. 验证并过滤 tool_use/tool_result 配对
    // 移除孤立的 tool_result（没有对应的 tool_use）
    // 同时返回孤立的 tool_use_id 集合，用于后续清理
    let repair_tool_pairing = options.conversion.tool_pairing_repair.is_enabled();
    let (validated_tool_results, orphaned_tool_use_ids, orphan_tool_result_texts) =
        if repair_tool_pairing || options.is_strict() {
            validate_tool_pairing(&history, &tool_results, &mut warnings)
        } else {
            (
                tool_results.clone(),
                std::collections::HashSet::new(),
                Vec::new(),
            )
        };

    if options.is_strict()
        && (warnings.orphan_tool_results > 0
            || warnings.orphan_tool_uses > 0
            || warnings.duplicate_tool_results > 0
            || !orphaned_tool_use_ids.is_empty())
    {
        return Err(ConversionError::UnsupportedContent(
            "tool_use/tool_result history is not strictly paired".to_string(),
        ));
    }

    // 9. 从历史中移除孤立的 tool_use（Kiro API 要求 tool_use 必须有对应的 tool_result）
    if !options.is_strict() && repair_tool_pairing {
        remove_orphaned_tool_uses(&mut history, &orphaned_tool_use_ids);
    }

    // 10. 收集历史中使用的工具名称，为缺失的工具生成占位符定义
    // Kiro API 要求：历史消息中引用的工具必须在 tools 列表中有定义
    // 注意：Kiro 匹配工具名称时忽略大小写，所以这里也需要忽略大小写比较
    let history_tool_names = collect_history_tool_names(&history);
    let mut existing_tool_names: std::collections::HashSet<_> = tools
        .iter()
        .map(|t| t.tool_specification.name.to_lowercase())
        .collect();

    if options.conversion.history_placeholder_tools.is_enabled() || options.is_strict() {
        for tool_name in history_tool_names {
            let tool_name_lower = tool_name.to_lowercase();
            if !existing_tool_names.contains(&tool_name_lower) {
                if options.is_strict() {
                    return Err(ConversionError::UnsupportedContent(format!(
                        "tool {} appears in history but is missing from tools",
                        tool_name
                    )));
                }
                known_tool_names.insert(tool_name.clone());
                tools.push(create_placeholder_tool(&tool_name, options.clone()));
                existing_tool_names.insert(tool_name_lower);
            }
        }
    }

    // 11. 构建 UserInputMessageContext
    let mut context = UserInputMessageContext::new();
    if !tools.is_empty() {
        context = context.with_tools(tools);
    }
    if !validated_tool_results.is_empty() {
        context = context.with_tool_results(validated_tool_results);
    }

    // 12. 构建当前消息
    // 保留文本内容，即使有工具结果也不丢弃用户文本
    let mut content = text_content;
    if !options.is_strict() && repair_tool_pairing {
        append_orphan_tool_result_texts(&mut content, &orphan_tool_result_texts);
    }
    if content.trim().is_empty() {
        if !context.tool_results.is_empty() {
            content = TOOL_RESULTS_PROVIDED_PLACEHOLDER.to_string();
            warnings.tool_result_content_placeholders += 1;
        } else {
            content = "Continue".to_string();
            warnings.empty_content_placeholders += 1;
        }
    }

    let mut user_input = UserInputMessage::new(content, &model_id)
        .with_context(context)
        .with_origin("AI_EDITOR");

    if !images.is_empty() {
        user_input = user_input.with_images(images);
    }
    let current_message = CurrentMessage::new(user_input);

    // 13. 构建 ConversationState
    let conversation_state = ConversationState::new(conversation_id)
        .with_agent_continuation_id(agent_continuation_id)
        .with_agent_task_type("vibe")
        .with_chat_trigger_type(chat_trigger_type)
        .with_current_message(current_message)
        .with_history(history);

    if !tool_name_map.is_empty() {
        tracing::info!("工具名称映射: {} 个超长名称已缩短", tool_name_map.len());
    }
    let mapped_schema_key_count = converted_tools.tool_schema_key_map.len();
    if mapped_schema_key_count > 0 {
        tracing::info!(
            mapped_schema_key_count,
            "工具 schema property key 映射已启用"
        );
    }

    let additional_model_request_fields = build_additional_model_request_fields(
        req,
        &model_id,
        options.conversion.native_reasoning_fields.is_enabled(),
    );
    if additional_model_request_fields.is_none() {
        if let Some(oc) = &req.output_config {
            if !oc.effort.trim().is_empty() {
                tracing::debug!(
                    model_id = %model_id,
                    "skipping unsupported additionalModelRequestFields for model"
                );
            }
        }
    }

    Ok(ConversionResult {
        conversation_state,
        tool_cache_point_insert_after: converted_tools.tool_cache_point_insert_after,
        cache_point_plan_recording_enabled: options.kiro_cache_point_record_plan,
        tool_name_map,
        tool_schema_key_map: converted_tools.tool_schema_key_map,
        known_tool_names,
        warnings,
        additional_model_request_fields,
    })
}

/// 确定聊天触发类型
/// "AUTO" 模式可能会导致 400 Bad Request 错误
fn determine_chat_trigger_type(_req: &MessagesRequest) -> String {
    "MANUAL".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_PNG_1X1_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/p9sAAAAASUVORK5CYII=";

    fn valid_jpeg_base64() -> String {
        BASE64_STANDARD.encode([0xff, 0xd8, 0xff, 0xdb, 0xff, 0xd9])
    }

    #[test]
    fn test_map_model_sonnet() {
        assert!(
            map_model("claude-sonnet-4-20250514")
                .unwrap()
                .contains("sonnet")
        );
        assert!(
            map_model("claude-3-5-sonnet-20241022")
                .unwrap()
                .contains("sonnet")
        );
    }

    #[test]
    fn test_map_model_opus() {
        assert!(
            map_model("claude-opus-4-20250514")
                .unwrap()
                .contains("opus")
        );
    }

    #[test]
    fn test_map_model_haiku() {
        assert!(
            map_model("claude-haiku-4-20250514")
                .unwrap()
                .contains("haiku")
        );
    }

    #[test]
    fn test_map_model_unsupported() {
        assert!(map_model("gpt-4").is_none());
    }

    #[test]
    fn test_map_model_claude_code_aliases() {
        assert_eq!(map_model("opus"), Some("claude-opus-4.7".to_string()));
        assert_eq!(map_model("opusplan"), Some("claude-opus-4.7".to_string()));
        assert_eq!(map_model("best"), Some("claude-opus-4.7".to_string()));
        assert_eq!(map_model("default"), Some("claude-opus-4.7".to_string()));
        assert_eq!(map_model("sonnet"), Some("claude-sonnet-4.6".to_string()));
        assert_eq!(
            map_model("claude-opus-4-7[1m]"),
            Some("claude-opus-4.7".to_string())
        );
    }

    #[test]
    fn test_map_model_future_claude_models_pass_through() {
        assert_eq!(
            map_model("claude-sonnet-4-9-20270101"),
            Some("claude-sonnet-4-9-20270101".to_string())
        );
        assert_eq!(
            map_model("claude-opus-5-20270101"),
            Some("claude-opus-5-20270101".to_string())
        );
        assert_eq!(
            map_model("claude-haiku-4-7-20270101"),
            Some("claude-haiku-4-7-20270101".to_string())
        );
        assert_eq!(
            map_model("Claude-Sonnet-4-9-20270101-thinking[1m]"),
            Some("claude-sonnet-4-9-20270101".to_string())
        );
    }

    #[test]
    fn test_content_block_preserves_thinking_signature_and_redacted_data() {
        let thinking: ContentBlock = serde_json::from_value(serde_json::json!({
            "type": "thinking",
            "thinking": "reasoning",
            "signature": "sig"
        }))
        .unwrap();
        assert_eq!(thinking.thinking.as_deref(), Some("reasoning"));
        assert_eq!(thinking.signature.as_deref(), Some("sig"));

        let redacted: ContentBlock = serde_json::from_value(serde_json::json!({
            "type": "redacted_thinking",
            "data": "opaque"
        }))
        .unwrap();
        assert_eq!(redacted.data.as_deref(), Some("opaque"));
    }

    #[test]
    fn test_process_message_content_accepts_base64_image_source() {
        let content = serde_json::json!([
            {"type": "text", "text": "describe"},
            {
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/png",
                    "data": VALID_PNG_1X1_BASE64
                }
            }
        ]);

        let (text, images, tool_results) = process_message_content(&content).unwrap();
        assert_eq!(text, "describe");
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].format, "png");
        assert_eq!(
            images[0].source.bytes.as_deref(),
            Some(VALID_PNG_1X1_BASE64)
        );
        assert!(tool_results.is_empty());
    }

    #[test]
    fn test_process_message_content_accepts_image_data_url_source() {
        let content = serde_json::json!([
            {
                "type": "image",
                "source": {
                    "type": "url",
                    "url": format!("data:image/png;base64,{}", VALID_PNG_1X1_BASE64)
                }
            }
        ]);

        let (_, images, _) = process_message_content(&content).unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].format, "png");
        assert_eq!(
            images[0].source.bytes.as_deref(),
            Some(VALID_PNG_1X1_BASE64)
        );
    }

    #[test]
    fn test_process_message_content_rejects_fake_declared_image() {
        let fake_png = BASE64_STANDARD.encode(b"not-an-image-at-all");
        let content = serde_json::json!([
            {
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/png",
                    "data": fake_png
                }
            }
        ]);

        let err = process_message_content(&content).expect_err("fake image should be rejected");
        assert!(err.to_string().contains("invalid image data"));
    }

    #[test]
    fn test_process_message_content_rejects_truncated_png() {
        let truncated_png = BASE64_STANDARD.encode([
            0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0x00, 0x00, 0x00, 0x0d, b'I', b'H',
            b'D', b'R',
        ]);
        let content = serde_json::json!([
            {
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/png",
                    "data": truncated_png
                }
            }
        ]);

        let err = process_message_content(&content).expect_err("truncated png should be rejected");
        assert!(err.to_string().contains("invalid image data"));
    }

    #[test]
    fn test_process_message_content_extracts_text_document_block() {
        let content = serde_json::json!([
            {
                "type": "document",
                "source": {
                    "type": "base64",
                    "media_type": "text/plain",
                    "data": "a2lyby1kb2MtdGVzdA=="
                }
            }
        ]);

        let (text, images, _) = process_message_content(&content).unwrap();
        assert!(images.is_empty());
        assert!(text.contains("kiro-doc-test"));
        assert!(text.contains("media_type=\"text/plain\""));
    }

    #[test]
    fn test_process_message_content_extracts_simple_pdf_text() {
        let pdf = b"%PDF-1.1\nBT /F1 12 Tf 20 100 Td (kiro-pdf-test) Tj ET\n%%EOF";
        let data = BASE64_STANDARD.encode(pdf);
        let content = serde_json::json!([
            {
                "type": "document",
                "source": {
                    "type": "base64",
                    "media_type": "application/pdf",
                    "data": data
                }
            }
        ]);

        let (text, images, _) = process_message_content(&content).unwrap();
        assert!(images.is_empty());
        assert!(text.contains("kiro-pdf-test"));
        assert!(text.contains("media_type=\"application/pdf\""));
    }

    #[test]
    fn test_map_model_thinking_suffix_sonnet() {
        // thinking 后缀不应影响 sonnet 模型映射
        let result = map_model("claude-sonnet-4-5-20250929-thinking");
        assert_eq!(result, Some("claude-sonnet-4.5".to_string()));
    }

    #[test]
    fn test_map_model_thinking_suffix_opus_4_5() {
        // thinking 后缀不应影响 opus 4.5 模型映射
        let result = map_model("claude-opus-4-5-20251101-thinking");
        assert_eq!(result, Some("claude-opus-4.5".to_string()));
    }

    #[test]
    fn test_map_model_thinking_suffix_opus_4_6() {
        // thinking 后缀不应影响 opus 4.6 模型映射
        let result = map_model("claude-opus-4-6-thinking");
        assert_eq!(result, Some("claude-opus-4.6".to_string()));
    }

    #[test]
    fn test_map_model_thinking_suffix_haiku() {
        // thinking 后缀不应影响 haiku 模型映射
        let result = map_model("claude-haiku-4-5-20251001-thinking");
        assert_eq!(result, Some("claude-haiku-4.5".to_string()));
    }

    #[test]
    fn test_context_window_size_for_kiro_auto_and_dash_variants() {
        assert_eq!(get_context_window_size("auto"), 1_000_000);
        assert_eq!(get_context_window_size("sonnet"), 200_000);
        assert_eq!(get_context_window_size("opus"), 200_000);
        assert_eq!(get_context_window_size("claude-opus-4.8"), 1_000_000);
        assert_eq!(get_context_window_size("claude-opus-4.7"), 1_000_000);
        assert_eq!(get_context_window_size("claude-sonnet-4.6"), 1_000_000);
        assert_eq!(
            get_context_window_size("claude-opus-4.7-thinking[1m]"),
            1_000_000
        );
        assert_eq!(get_context_window_size("claude-sonnet-4-6[1m]"), 1_000_000);
        assert_eq!(get_context_window_size("claude-opus-4-7"), 200_000);
        assert_eq!(get_context_window_size("claude-sonnet-4-6"), 200_000);
    }

    #[test]
    fn test_determine_chat_trigger_type() {
        // 无工具时返回 MANUAL
        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };
        assert_eq!(determine_chat_trigger_type(&req), "MANUAL");
    }

    #[test]
    fn test_collect_history_tool_names() {
        use crate::kiro::model::requests::tool::ToolUseEntry;

        // 创建包含工具使用的历史消息
        let mut assistant_msg = AssistantMessage::new("I'll read the file.");
        assistant_msg = assistant_msg.with_tool_uses(vec![
            ToolUseEntry::new("tool-1", "read")
                .with_input(serde_json::json!({"path": "/test.txt"})),
            ToolUseEntry::new("tool-2", "write")
                .with_input(serde_json::json!({"path": "/out.txt"})),
        ]);

        let history = vec![
            Message::User(HistoryUserMessage::new(
                "Read the file",
                "claude-sonnet-4.5",
            )),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: assistant_msg,
            }),
        ];

        let tool_names = collect_history_tool_names(&history);
        assert_eq!(tool_names.len(), 2);
        assert!(tool_names.contains(&"read".to_string()));
        assert!(tool_names.contains(&"write".to_string()));
    }

    #[test]
    fn test_collect_history_tool_names_dedupes_case_insensitive() {
        use crate::kiro::model::requests::tool::ToolUseEntry;

        let mut assistant_msg = AssistantMessage::new("Using tools");
        assistant_msg = assistant_msg.with_tool_uses(vec![
            ToolUseEntry::new("tool-1", "Read"),
            ToolUseEntry::new("tool-2", "read"),
        ]);
        let history = vec![Message::Assistant(HistoryAssistantMessage {
            assistant_response_message: assistant_msg,
        })];

        let tool_names = collect_history_tool_names(&history);

        assert_eq!(tool_names, vec!["Read".to_string()]);
    }

    #[test]
    fn test_create_placeholder_tool() {
        let tool = create_placeholder_tool("my_custom_tool", ConverterOptions::default());

        assert_eq!(tool.tool_specification.name, "my_custom_tool");
        assert!(!tool.tool_specification.description.is_empty());

        // 验证 JSON 序列化正确
        let json = serde_json::to_string(&tool).unwrap();
        assert!(json.contains("\"name\":\"my_custom_tool\""));
        assert!(!json.contains("additionalProperties"));
        assert!(!json.contains("required"));
        assert!(!json.contains("$schema"));
    }

    #[test]
    fn test_normalize_json_schema_recursively_removes_kiro_rejected_fields() {
        let schema = serde_json::json!({
            "type": "object",
            "additionalProperties": true,
            "required": [],
            "properties": {
                "path": {
                    "type": "string",
                    "required": null,
                    "additionalProperties": false
                },
                "mode": {
                    "type": "object",
                    "required": ["kind", 7, null],
                    "additionalProperties": {"type": "string"},
                    "properties": {
                        "kind": {"type": "string"},
                        "nested": {
                            "type": "object",
                            "required": [],
                            "additionalProperties": true
                        }
                    }
                }
            }
        });

        let normalized = normalize_json_schema(schema);
        assert_eq!(normalized["type"], "object");
        assert_eq!(
            normalized["properties"]["mode"]["required"],
            serde_json::json!(["kind"])
        );
        assert!(normalized.get("required").is_none());
        assert!(normalized.get("additionalProperties").is_none());
        assert!(
            normalized["properties"]["path"]
                .get("additionalProperties")
                .is_none()
        );
        assert!(
            normalized["properties"]["mode"]
                .get("additionalProperties")
                .is_none()
        );
        assert!(
            normalized["properties"]["mode"]["properties"]["nested"]
                .get("required")
                .is_none()
        );
        assert!(
            normalized["properties"]["mode"]["properties"]["nested"]
                .get("additionalProperties")
                .is_none()
        );
    }

    #[test]
    fn test_normalize_json_schema_sanitizes_openapi_and_shorthand_schema() {
        let schema = serde_json::json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "$vocabulary": {"https://json-schema.org/draft/2020-12/vocab/core": true},
            "type": "object",
            "$id": "",
            "$anchor": " ",
            "nullable": true,
            "additionalProperties": false,
            "x-mcp-source": "test",
            "discriminator": {"propertyName": "kind"},
            "xml": {"name": "toolInput"},
            "externalDocs": {"url": "https://example.com"},
            "required": ["path", "missing", 7, "path"],
            "properties": {
                "path": "str",
                "enabled": {"type": "bool", "nullable": true},
                "count": {"type": ["int", null, "bad", "integer"]},
                "tags": {"type": "list", "items": "string"},
                "tuple": {"type": "array", "items": ["string", {"type": "int"}, 3]},
                "choice": {"oneOf": {"type": "str"}},
                "mode": {"type": "string", "enum": "read"},
                "bad": null,
                "constant": true,
                "title": {"text": "not a valid title"},
                "emptyPattern": {"type": "string", "pattern": ""},
                "emptyFormat": {"type": "string", "format": " "},
                "bounded": {
                    "type": "number",
                    "minimum": "zero",
                    "maximum": 10,
                    "multipleOf": 0
                }
            }
        });

        let normalized = normalize_json_schema(schema);

        assert_eq!(normalized["type"], "object");
        assert_eq!(normalized["required"], serde_json::json!(["path"]));
        assert!(normalized.get("$schema").is_none());
        assert!(normalized.get("$vocabulary").is_none());
        assert!(normalized.get("nullable").is_none());
        assert!(normalized.get("additionalProperties").is_none());
        assert!(normalized.get("x-mcp-source").is_none());
        assert!(normalized.get("discriminator").is_none());
        assert!(normalized.get("xml").is_none());
        assert!(normalized.get("externalDocs").is_none());
        assert!(normalized.get("$id").is_none());
        assert!(normalized.get("$anchor").is_none());

        let props = &normalized["properties"];
        assert_eq!(props["path"], serde_json::json!({"type": "string"}));
        assert_eq!(
            props["enabled"]["type"],
            serde_json::json!(["boolean", "null"])
        );
        assert_eq!(props["count"]["type"], serde_json::json!("integer"));
        assert_eq!(props["tags"]["type"], serde_json::json!("array"));
        assert_eq!(
            props["tags"]["items"],
            serde_json::json!({"type": "string"})
        );
        assert_eq!(
            props["tuple"]["prefixItems"],
            serde_json::json!([{"type": "string"}, {"type": "integer"}])
        );
        assert_eq!(
            props["choice"]["oneOf"],
            serde_json::json!([{"type": "string"}])
        );
        assert_eq!(props["mode"]["enum"], serde_json::json!(["read"]));
        assert_eq!(props["bad"], serde_json::json!({}));
        assert_eq!(props["constant"], serde_json::json!(true));
        assert!(props["title"].get("title").is_none());
        assert!(props["emptyPattern"].get("pattern").is_none());
        assert!(props["emptyFormat"].get("format").is_none());
        assert!(props["bounded"].get("minimum").is_none());
        assert_eq!(props["bounded"]["maximum"], serde_json::json!(10));
        assert!(props["bounded"].get("multipleOf").is_none());
    }

    #[test]
    fn test_normalize_json_schema_flattens_root_union_combinators() {
        let schema = serde_json::json!({
            "oneOf": [
                {
                    "type": "object",
                    "required": ["path", "mode"],
                    "properties": {
                        "path": {"type": "string"},
                        "mode": {"type": "string"}
                    }
                },
                {
                    "type": "object",
                    "required": ["path", "query"],
                    "properties": {
                        "path": {"type": "string"},
                        "query": {"type": "string"}
                    }
                }
            ]
        });

        let normalized = normalize_json_schema(schema);

        assert_eq!(normalized["type"], "object");
        assert!(normalized.get("oneOf").is_none());
        assert!(normalized.get("anyOf").is_none());
        assert!(normalized.get("allOf").is_none());
        assert_eq!(
            normalized["properties"]["path"],
            serde_json::json!({"type": "string"})
        );
        assert_eq!(
            normalized["properties"]["mode"],
            serde_json::json!({"type": "string"})
        );
        assert_eq!(
            normalized["properties"]["query"],
            serde_json::json!({"type": "string"})
        );
        assert_eq!(normalized["required"], serde_json::json!(["path"]));
    }

    #[test]
    fn test_normalize_json_schema_flattens_root_all_of_required_union() {
        let schema = serde_json::json!({
            "allOf": [
                {
                    "type": "object",
                    "required": ["path"],
                    "properties": {
                        "path": {"type": "string"}
                    }
                },
                {
                    "type": "object",
                    "required": ["recursive"],
                    "properties": {
                        "recursive": {"type": "boolean"}
                    }
                }
            ]
        });

        let normalized = normalize_json_schema(schema);

        assert!(normalized.get("allOf").is_none());
        assert_eq!(normalized["properties"]["path"]["type"], "string");
        assert_eq!(normalized["properties"]["recursive"]["type"], "boolean");
        assert_eq!(
            normalized["required"],
            serde_json::json!(["path", "recursive"])
        );
    }

    #[test]
    fn test_normalize_json_schema_converts_legacy_definition_keywords() {
        let schema = serde_json::json!({
            "type": "object",
            "definitions": {
                "file": {
                    "type": "object",
                    "required": ["path", "unused"],
                    "properties": {
                        "path": {"type": "str"},
                        "size": {"type": "int"}
                    }
                }
            },
            "properties": {
                "file": {"$ref": "#/definitions/file"},
                "owner": {"type": "string"}
            },
            "dependencies": {
                "file": ["owner", 1, "owner"],
                "owner": {"properties": {"team": "string"}, "required": ["team"]}
            },
            "dependentRequired": {
                "owner": ["file", null]
            }
        });

        let normalized = normalize_json_schema(schema);

        assert!(normalized.get("definitions").is_none());
        assert_eq!(normalized["properties"]["file"]["$ref"], "#/$defs/file");
        assert_eq!(
            normalized["$defs"]["file"]["properties"]["path"],
            serde_json::json!({"type": "string"})
        );
        assert_eq!(
            normalized["$defs"]["file"]["required"],
            serde_json::json!(["path"])
        );
        assert_eq!(
            normalized["dependentRequired"]["file"],
            serde_json::json!(["owner"])
        );
        assert_eq!(
            normalized["dependentRequired"]["owner"],
            serde_json::json!(["file"])
        );
        assert_eq!(
            normalized["dependentSchemas"]["owner"]["properties"]["team"],
            serde_json::json!({"type": "string"})
        );
    }

    #[test]
    fn test_shorten_tool_name_deterministic() {
        let long_name =
            "mcp__some_very_long_server_name__some_very_long_tool_name_that_exceeds_limit";
        assert!(long_name.len() > TOOL_NAME_MAX_LEN);

        let short1 = shorten_tool_name(long_name, long_name);
        let short2 = shorten_tool_name(long_name, long_name);
        assert_eq!(short1, short2, "相同输入应产生相同的短名称");
        assert!(
            short1.len() <= TOOL_NAME_MAX_LEN,
            "短名称长度应 <= 63，实际 {}",
            short1.len()
        );
    }

    #[test]
    fn test_shorten_tool_name_uniqueness() {
        let name_a = "mcp__server_alpha__tool_name_that_is_very_long_and_exceeds_the_limit_a";
        let name_b = "mcp__server_alpha__tool_name_that_is_very_long_and_exceeds_the_limit_b";
        let short_a = shorten_tool_name(name_a, name_a);
        let short_b = shorten_tool_name(name_b, name_b);
        assert_ne!(short_a, short_b, "不同输入应产生不同的短名称");
    }

    #[test]
    fn test_map_tool_name_short_passthrough() {
        let mut map = HashMap::new();
        let result = map_tool_name("shortName", &mut map, ConverterOptions::default());
        assert_eq!(result, "shortName");
        assert!(map.is_empty(), "Kiro-safe 短名称不应产生映射");
    }

    #[test]
    fn test_map_tool_name_sanitizes_separators_and_records_mapping() {
        let mut map = HashMap::new();
        let result = map_tool_name(
            "mcp__server-name__read_file",
            &mut map,
            ConverterOptions::default(),
        );
        assert!(result.len() <= TOOL_NAME_MAX_LEN);
        assert!(result.chars().all(|ch| ch.is_ascii_alphanumeric()));
        assert!(result.contains(TOOL_HASH_MARKER));
        assert_eq!(
            map.get(&result),
            Some(&"mcp__server-name__read_file".to_string())
        );
    }

    #[test]
    fn test_map_tool_name_avoids_collisions_after_sanitizing() {
        let mut map = HashMap::new();
        let dash = map_tool_name("foo-bar", &mut map, ConverterOptions::default());
        let underscore = map_tool_name("foo_bar", &mut map, ConverterOptions::default());
        assert_ne!(dash, underscore);
        assert_eq!(map.get(&dash), Some(&"foo-bar".to_string()));
        assert_eq!(map.get(&underscore), Some(&"foo_bar".to_string()));
    }

    #[test]
    fn test_map_tool_name_long_creates_mapping() {
        let mut map = HashMap::new();
        let long_name = "mcp__plugin_very_long_server_name__extremely_long_tool_name_exceeds_63";
        let result = map_tool_name(long_name, &mut map, ConverterOptions::default());
        assert!(result.len() <= TOOL_NAME_MAX_LEN);
        assert!(result.chars().all(|ch| ch.is_ascii_alphanumeric()));
        assert_eq!(map.get(&result), Some(&long_name.to_string()));
    }

    #[test]
    fn test_tool_name_mapping_can_be_disabled_by_conversion_plan() {
        use crate::anthropic::body_capabilities::BodyStageState;

        let mut map = HashMap::new();
        let mut options = ConverterOptions::default();
        options.conversion.tool_name_mapping = BodyStageState::Disabled;

        let result = map_tool_name("mcp__server-name__read_file", &mut map, options);

        assert_eq!(result, "mcp__server-name__read_file");
        assert!(map.is_empty());
    }

    #[test]
    fn test_tool_choice_steering_can_be_disabled_by_conversion_plan() {
        use crate::anthropic::body_capabilities::BodyStageState;

        let req = base_tool_choice_request(serde_json::json!({"type": "none"}));
        let mut options = ConverterOptions::default();
        options.conversion.tool_choice_steering = BodyStageState::Disabled;

        let result = convert_request_with_options(&req, options).unwrap();
        let context = &result
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context;

        assert_eq!(context.tools.len(), 2);
        assert!(
            result
                .conversation_state
                .history
                .iter()
                .all(|message| !matches!(
                    message,
                    Message::User(user)
                        if user.user_input_message.content.contains("<tool_choice>")
                ))
        );
    }

    #[test]
    fn test_tool_name_mapping_in_convert_request() {
        use super::super::types::{Message as AnthropicMessage, Tool as AnthropicTool};

        let long_tool_name =
            "mcp__plugin_very_long_server_name__extremely_long_tool_name_exceeds_63";
        assert!(long_tool_name.len() > TOOL_NAME_MAX_LEN);

        let mut schema = std::collections::HashMap::new();
        schema.insert("type".to_string(), serde_json::json!("object"));
        schema.insert("properties".to_string(), serde_json::json!({}));

        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("test"),
            }],
            system: None,
            stream: false,
            tools: Some(vec![AnthropicTool {
                name: long_tool_name.to_string(),
                description: "A test tool".to_string(),
                input_schema: schema,
                tool_type: None,
                max_uses: None,
                cache_control: None,
            }]),
            thinking: None,
            tool_choice: None,
            output_config: None,
            metadata: None,
        };

        let result = convert_request(&req).unwrap();

        // 应该有映射
        assert_eq!(result.tool_name_map.len(), 1);

        // 映射中的值应该是原始名称
        let (short, original) = result.tool_name_map.iter().next().unwrap();
        assert_eq!(original, long_tool_name);
        assert!(short.len() <= TOOL_NAME_MAX_LEN);

        // Kiro 请求中的工具名应该是短名称
        let tools = &result
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context
            .tools;
        assert_eq!(tools[0].tool_specification.name, *short);
    }

    fn schema_key_mapping_request(
        properties: serde_json::Value,
        required: serde_json::Value,
    ) -> MessagesRequest {
        use super::super::types::{Message as AnthropicMessage, Tool as AnthropicTool};

        let mut schema = std::collections::HashMap::new();
        schema.insert("type".to_string(), serde_json::json!("object"));
        schema.insert("properties".to_string(), properties);
        schema.insert("required".to_string(), required);

        MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("call probe"),
            }],
            system: None,
            stream: false,
            tools: Some(vec![AnthropicTool {
                name: "probe".to_string(),
                description: "A probe tool".to_string(),
                input_schema: schema,
                tool_type: None,
                max_uses: None,
                cache_control: None,
            }]),
            thinking: None,
            tool_choice: None,
            output_config: None,
            metadata: None,
        }
    }

    #[test]
    fn test_schema_key_mapping_sanitizes_only_invalid_keys_and_reverses_input() {
        let req = schema_key_mapping_request(
            serde_json::json!({
                "valid_key": {"type": "string"},
                "bad key": {
                    "type": "object",
                    "properties": {
                        "nested/key": {"type": "string"}
                    },
                    "required": ["nested/key"]
                }
            }),
            serde_json::json!(["valid_key", "bad key"]),
        );

        let result = convert_request_with_options(&req, ConverterOptions::default()).unwrap();
        let tool = &result
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context
            .tools[0];
        let schema = &tool.tool_specification.input_schema.json;
        let properties = schema["properties"].as_object().unwrap();
        assert!(properties.contains_key("valid_key"));
        assert!(!properties.contains_key("bad key"));

        let sanitized_bad_key = properties
            .keys()
            .find(|key| key.as_str() != "valid_key")
            .expect("sanitized bad key")
            .clone();
        assert!(
            sanitized_bad_key.starts_with("key")
                && sanitized_bad_key.len() == "key".len() + 16
                && sanitized_bad_key["key".len()..]
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit()),
            "invalid schema key should be mapped to a hash-only id, got {sanitized_bad_key}"
        );
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .contains(&serde_json::Value::String(sanitized_bad_key.clone()))
        );

        let nested_properties = properties[&sanitized_bad_key]["properties"]
            .as_object()
            .unwrap();
        assert!(!nested_properties.contains_key("nested/key"));
        let sanitized_nested_key = nested_properties.keys().next().unwrap().clone();

        let restored = result.tool_schema_key_map.reverse_tool_input(
            "probe",
            serde_json::json!({
                "valid_key": "kept",
                sanitized_bad_key: {
                    sanitized_nested_key: "restored"
                }
            }),
        );
        assert_eq!(restored["valid_key"], "kept");
        assert_eq!(restored["bad key"]["nested/key"], "restored");
    }

    #[test]
    fn test_schema_key_mapping_reject_mode_errors_without_sanitizing() {
        use crate::model::config::ToolSchemaKeyMappingMode;

        let req = schema_key_mapping_request(
            serde_json::json!({
                "bad key": {"type": "string"}
            }),
            serde_json::json!(["bad key"]),
        );
        let mut options = ConverterOptions::default();
        options.conversion.tool_schema_key_mapping = ToolSchemaKeyMappingMode::Reject;

        let err = convert_request_with_options(&req, options).unwrap_err();
        assert!(err.to_string().contains("bad key"));
        assert!(err.to_string().contains("does not match"));
    }

    #[test]
    fn test_schema_key_mapping_disabled_preserves_invalid_keys() {
        use crate::model::config::ToolSchemaKeyMappingMode;

        let req = schema_key_mapping_request(
            serde_json::json!({
                "bad key": {"type": "string"}
            }),
            serde_json::json!(["bad key"]),
        );
        let mut options = ConverterOptions::default();
        options.conversion.tool_schema_key_mapping = ToolSchemaKeyMappingMode::Disabled;

        let result = convert_request_with_options(&req, options).unwrap();
        let schema = &result
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context
            .tools[0]
            .tool_specification
            .input_schema
            .json;
        assert!(
            schema["properties"]
                .as_object()
                .unwrap()
                .contains_key("bad key")
        );
        assert!(!result.tool_schema_key_map.has_tool("probe"));
    }

    #[test]
    fn test_schema_key_mapping_uses_configured_regex() {
        let req = schema_key_mapping_request(
            serde_json::json!({
                "camelCase": {"type": "string"}
            }),
            serde_json::json!(["camelCase"]),
        );
        let mut options = ConverterOptions::default();
        options.conversion.tool_schema_key_validation_regex = "^[a-z_][a-z0-9_]{0,63}$".to_string();

        let result = convert_request_with_options(&req, options).unwrap();
        let schema = &result
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context
            .tools[0]
            .tool_specification
            .input_schema
            .json;
        assert!(
            !schema["properties"]
                .as_object()
                .unwrap()
                .contains_key("camelCase")
        );
        assert!(result.tool_schema_key_map.has_tool("probe"));
    }

    #[test]
    fn test_empty_tool_description_gets_non_empty_placeholder() {
        use super::super::types::{Message as AnthropicMessage, Tool as AnthropicTool};

        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("test"),
            }],
            stream: false,
            system: None,
            tools: Some(vec![
                AnthropicTool {
                    name: "computer".to_string(),
                    description: "".to_string(),
                    input_schema: HashMap::new(),
                    tool_type: None,
                    max_uses: None,
                    cache_control: None,
                },
                AnthropicTool {
                    name: "blank".to_string(),
                    description: "   ".to_string(),
                    input_schema: HashMap::new(),
                    tool_type: None,
                    max_uses: None,
                    cache_control: None,
                },
            ]),
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let result = convert_request(&req).unwrap();
        let tools = &result
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context
            .tools;

        assert_eq!(tools.len(), 2);
        assert!(
            tools
                .iter()
                .all(|tool| !tool.tool_specification.description.trim().is_empty())
        );
        assert!(tools[0].tool_specification.description.contains("computer"));
    }

    #[test]
    fn test_non_empty_tool_description_is_preserved() {
        use super::super::types::{Message as AnthropicMessage, Tool as AnthropicTool};

        let mut schema = std::collections::HashMap::new();
        schema.insert("type".to_string(), serde_json::json!("object"));

        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("test"),
            }],
            stream: false,
            system: None,
            tools: Some(vec![AnthropicTool {
                name: "probe".to_string(),
                description: "Probe tool.".to_string(),
                input_schema: schema,
                tool_type: None,
                max_uses: None,
                cache_control: None,
            }]),
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let result = convert_request(&req).unwrap();
        let tools = &result
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context
            .tools;

        assert_eq!(tools[0].tool_specification.description, "Probe tool.");
    }

    #[test]
    fn test_tool_name_mapping_in_history() {
        use super::super::types::{Message as AnthropicMessage, Tool as AnthropicTool};

        let long_tool_name =
            "mcp__plugin_very_long_server_name__extremely_long_tool_name_exceeds_63";

        let mut schema = std::collections::HashMap::new();
        schema.insert("type".to_string(), serde_json::json!("object"));
        schema.insert("properties".to_string(), serde_json::json!({}));

        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!("use the tool"),
                },
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: serde_json::json!([
                        {"type": "text", "text": "calling tool"},
                        {"type": "tool_use", "id": "toolu_01", "name": long_tool_name, "input": {}}
                    ]),
                },
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!([
                        {"type": "tool_result", "tool_use_id": "toolu_01", "content": "done"}
                    ]),
                },
            ],
            system: None,
            stream: false,
            tools: Some(vec![AnthropicTool {
                name: long_tool_name.to_string(),
                description: "A test tool".to_string(),
                input_schema: schema,
                tool_type: None,
                max_uses: None,
                cache_control: None,
            }]),
            thinking: None,
            tool_choice: None,
            output_config: None,
            metadata: None,
        };

        let result = convert_request(&req).unwrap();
        let short_name = result.tool_name_map.iter().next().unwrap().0.clone();

        // 历史中 assistant 消息的 tool_use name 也应该被映射
        let history = &result.conversation_state.history;
        let mut found = false;
        for msg in history {
            if let Message::Assistant(a) = msg {
                if let Some(ref tool_uses) = a.assistant_response_message.tool_uses {
                    for tu in tool_uses {
                        if tu.tool_use_id == "toolu_01" {
                            assert_eq!(tu.name, short_name, "历史中的 tool_use name 应该是短名称");
                            found = true;
                        }
                    }
                }
            }
        }
        assert!(found, "应该在历史中找到 tool_use");
    }

    #[test]
    fn test_history_tools_added_to_tools_list() {
        use super::super::types::Message as AnthropicMessage;

        // 创建一个请求，历史中有工具使用，但 tools 列表为空
        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!("Read the file"),
                },
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: serde_json::json!([
                        {"type": "text", "text": "I'll read the file."},
                        {"type": "tool_use", "id": "tool-1", "name": "read", "input": {"path": "/test.txt"}}
                    ]),
                },
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!([
                        {"type": "tool_result", "tool_use_id": "tool-1", "content": "file content"}
                    ]),
                },
            ],
            stream: false,
            system: None,
            tools: None, // 没有提供工具定义
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let result = convert_request(&req).unwrap();

        // 验证 tools 列表中包含了历史中使用的工具的占位符定义
        let tools = &result
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context
            .tools;

        assert!(!tools.is_empty(), "tools 列表不应为空");
        assert!(
            tools.iter().any(|t| t.tool_specification.name == "read"),
            "tools 列表应包含 'read' 工具的占位符定义"
        );
    }

    #[test]
    fn test_duplicate_declared_tools_are_deduped_before_kiro_request() {
        use super::super::types::{Message as AnthropicMessage, Tool as AnthropicTool};

        let mut schema = std::collections::HashMap::new();
        schema.insert("type".to_string(), serde_json::json!("object"));
        schema.insert("properties".to_string(), serde_json::json!({}));

        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("test"),
            }],
            stream: false,
            system: None,
            tools: Some(vec![
                AnthropicTool {
                    name: "read".to_string(),
                    description: "A test tool".to_string(),
                    input_schema: schema.clone(),
                    tool_type: None,
                    max_uses: None,
                    cache_control: None,
                },
                AnthropicTool {
                    name: "read".to_string(),
                    description: "Duplicate tool".to_string(),
                    input_schema: schema,
                    tool_type: None,
                    max_uses: None,
                    cache_control: None,
                },
            ]),
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let result = convert_request(&req).unwrap();
        let tools = &result
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context
            .tools;

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].tool_specification.name, "read");
    }

    #[test]
    fn current_tool_result_only_message_gets_content_placeholder() {
        use super::super::types::Message as AnthropicMessage;

        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!("Read the file"),
                },
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: serde_json::json!([
                        {"type": "tool_use", "id": "tool-1", "name": "read", "input": {"path": "/test.txt"}}
                    ]),
                },
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!([
                        {"type": "tool_result", "tool_use_id": "tool-1", "content": "file content"}
                    ]),
                },
            ],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let result = convert_request(&req).unwrap();
        let current = &result.conversation_state.current_message.user_input_message;

        assert_eq!(current.content, TOOL_RESULTS_PROVIDED_PLACEHOLDER);
        assert_eq!(current.user_input_message_context.tool_results.len(), 1);
        assert_eq!(result.warnings.tool_result_content_placeholders, 1);
    }

    #[test]
    fn current_empty_user_message_gets_continue_placeholder() {
        use super::super::types::Message as AnthropicMessage;

        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!(""),
            }],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let result = convert_request(&req).unwrap();
        let current = &result.conversation_state.current_message.user_input_message;

        assert_eq!(current.content, "Continue");
        assert!(current.user_input_message_context.tool_results.is_empty());
        assert_eq!(result.warnings.empty_content_placeholders, 1);
        assert_eq!(result.warnings.tool_result_content_placeholders, 0);
    }

    #[test]
    fn history_tool_result_only_message_gets_content_placeholder() {
        use super::super::types::Message as AnthropicMessage;

        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!("Read the file"),
                },
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: serde_json::json!([
                        {"type": "tool_use", "id": "tool-1", "name": "read", "input": {"path": "/test.txt"}}
                    ]),
                },
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!([
                        {"type": "tool_result", "tool_use_id": "tool-1", "content": "file content"}
                    ]),
                },
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: serde_json::json!("The file contains content."),
                },
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!("Continue"),
                },
            ],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let result = convert_request(&req).unwrap();
        let tool_result_user = result
            .conversation_state
            .history
            .iter()
            .find_map(|message| match message {
                Message::User(user)
                    if !user
                        .user_input_message
                        .user_input_message_context
                        .tool_results
                        .is_empty() =>
                {
                    Some(&user.user_input_message)
                }
                _ => None,
            })
            .expect("history should contain the tool_result user message");

        assert_eq!(tool_result_user.content, TOOL_RESULTS_PROVIDED_PLACEHOLDER);
        assert_eq!(
            tool_result_user
                .user_input_message_context
                .tool_results
                .len(),
            1
        );
    }

    #[test]
    fn test_extract_session_id_valid() {
        // 测试有效的 user_id 格式
        let user_id = "user_0dede55c6dcc4a11a30bbb5e7f22e6fdf86cdeba3820019cc27612af4e1243cd_account__session_8bb5523b-ec7c-4540-a9ca-beb6d79f1552";
        let session_id = extract_session_id(user_id);
        assert_eq!(
            session_id,
            Some("8bb5523b-ec7c-4540-a9ca-beb6d79f1552".to_string())
        );
    }

    #[test]
    fn test_extract_session_id_json_format() {
        // 测试 JSON 格式的 user_id
        let user_id = r#"{"device_id":"0dede55c6dcc4a11a30bbb5e7f22e6fdf86cdeba3820019cc27612af4e1243cd","account_uuid":"","session_id":"8bb5523b-ec7c-4540-a9ca-beb6d79f1552"}"#;
        let session_id = extract_session_id(user_id);
        assert_eq!(
            session_id,
            Some("8bb5523b-ec7c-4540-a9ca-beb6d79f1552".to_string())
        );
    }

    #[test]
    fn test_extract_session_id_json_invalid_session() {
        // 测试 JSON 格式但 session_id 不是有效 UUID
        let user_id = r#"{"device_id":"abc","session_id":"not-a-uuid"}"#;
        let session_id = extract_session_id(user_id);
        assert_eq!(session_id, None);
    }

    #[test]
    fn test_extract_session_id_no_session() {
        // 测试没有 session 的 user_id
        let user_id = "user_0dede55c6dcc4a11a30bbb5e7f22e6fdf86cdeba3820019cc27612af4e1243cd";
        let session_id = extract_session_id(user_id);
        assert_eq!(session_id, None);
    }

    #[test]
    fn test_extract_session_id_invalid_uuid() {
        // 测试无效的 UUID 格式
        let user_id = "user_xxx_session_invalid-uuid";
        let session_id = extract_session_id(user_id);
        assert_eq!(session_id, None);
    }

    #[test]
    fn test_convert_request_with_session_metadata() {
        use super::super::types::{Message as AnthropicMessage, Metadata};

        // 测试带有 metadata 的请求，应该使用 session UUID 作为 conversationId
        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("Hello"),
            }],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: Some(Metadata {
                user_id: Some(
                    "user_0dede55c6dcc4a11a30bbb5e7f22e6fdf86cdeba3820019cc27612af4e1243cd_account__session_a0662283-7fd3-4399-a7eb-52b9a717ae88".to_string(),
                ),
            }),
        };

        let result = convert_request(&req).unwrap();
        assert_eq!(
            result.conversation_state.conversation_id,
            "a0662283-7fd3-4399-a7eb-52b9a717ae88"
        );
    }

    #[test]
    fn test_convert_request_without_metadata_is_stable_across_turns() {
        use super::super::types::{Message as AnthropicMessage, SystemMessage};

        let first_req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("Hello"),
            }],
            stream: false,
            system: Some(vec![SystemMessage {
                text: "You are a helpful coding assistant.".to_string(),
                cache_control: Some(serde_json::json!({"type": "ephemeral"})),
            }]),
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let second_req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!("Hello"),
                },
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: serde_json::json!("Sure."),
                },
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!("Add tests for it."),
                },
            ],
            stream: false,
            system: Some(vec![SystemMessage {
                text: "You are a helpful coding assistant.".to_string(),
                cache_control: None,
            }]),
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let first_result = convert_request(&first_req).unwrap();
        let second_result = convert_request(&second_req).unwrap();

        assert_eq!(first_result.conversation_state.conversation_id.len(), 36);
        assert_eq!(
            first_result.conversation_state.conversation_id,
            second_result.conversation_state.conversation_id
        );
    }

    #[test]
    fn test_convert_request_without_metadata_is_not_stabilized_when_high_cache_disabled() {
        use super::super::types::{Message as AnthropicMessage, SystemMessage};

        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("Hello"),
            }],
            stream: false,
            system: Some(vec![SystemMessage {
                text: "You are a helpful coding assistant.".to_string(),
                cache_control: Some(serde_json::json!({"type": "ephemeral"})),
            }]),
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let first_result = convert_request_with_options(
            &req,
            ConverterOptions {
                prompt_cache_simulation_mode: PromptCacheSimulationMode::Disabled,
                ..ConverterOptions::default()
            },
        )
        .unwrap();
        let second_result = convert_request_with_options(
            &req,
            ConverterOptions {
                prompt_cache_simulation_mode: PromptCacheSimulationMode::Disabled,
                ..ConverterOptions::default()
            },
        )
        .unwrap();

        assert_ne!(
            first_result.conversation_state.conversation_id,
            second_result.conversation_state.conversation_id
        );
    }

    #[test]
    fn test_anthropic_strict_avoids_chunk_policy_and_thinking_prefix() {
        use super::super::types::{Message as AnthropicMessage, SystemMessage, Thinking};

        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("Hello"),
            }],
            stream: false,
            system: Some(vec![SystemMessage {
                text: "Reply tersely.".to_string(),
                cache_control: None,
            }]),
            tools: None,
            tool_choice: None,
            thinking: Some(Thinking {
                thinking_type: "enabled".to_string(),
                budget_tokens: 20000,
            }),
            output_config: None,
            metadata: None,
        };

        let result = convert_request_with_options(
            &req,
            ConverterOptions {
                compat_profile: CompatProfile::AnthropicStrict,
                ..ConverterOptions::default()
            },
        )
        .unwrap();

        let first_user = result
            .conversation_state
            .history
            .iter()
            .find_map(|message| match message {
                Message::User(user) => Some(&user.user_input_message.content),
                _ => None,
            })
            .expect("system should be represented as first user history message");

        assert_eq!(first_user, "Reply tersely.");
        assert!(!first_user.contains(SYSTEM_CHUNKED_POLICY));
        assert!(!first_user.contains("<thinking_mode>"));
        assert!(!first_user.contains("<thinking_output_policy>"));
    }

    #[test]
    fn test_resolved_base_model_keeps_enabled_thinking_prefix() {
        use crate::anthropic::model_capabilities::ModelResolutionSource;

        use super::super::types::{Message as AnthropicMessage, OutputConfig, Thinking};

        let req = MessagesRequest {
            model: "claude-sonnet-4-6-thinking".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("Hello"),
            }],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: Some(Thinking {
                thinking_type: "enabled".to_string(),
                budget_tokens: 20000,
            }),
            output_config: None,
            metadata: None,
        };
        let resolution = ModelResolution::resolved(
            "claude-sonnet-4-6-thinking".to_string(),
            "claude-sonnet-4.5".to_string(),
            ModelResolutionSource::FamilyNormalized,
        );

        let result =
            convert_request_with_resolved_model(&req, ConverterOptions::default(), &resolution)
                .expect("thinking request should convert through resolved base model");

        assert_eq!(
            result
                .conversation_state
                .current_message
                .user_input_message
                .model_id,
            "claude-sonnet-4.5"
        );
        let first_history_user = result
            .conversation_state
            .history
            .iter()
            .find_map(|message| match message {
                Message::User(user) => Some(&user.user_input_message),
                _ => None,
            })
            .expect("thinking controls should be injected as synthetic history");

        assert_eq!(first_history_user.model_id, "claude-sonnet-4.5");
        assert!(
            first_history_user
                .content
                .contains("<thinking_mode>enabled</thinking_mode>")
        );
        assert!(
            first_history_user
                .content
                .contains("<max_thinking_length>20000</max_thinking_length>")
        );
        assert!(
            first_history_user
                .content
                .contains("<thinking_output_policy>")
        );

        let adaptive_req = MessagesRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("Hello"),
            }],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: Some(Thinking {
                thinking_type: "adaptive".to_string(),
                budget_tokens: 0,
            }),
            output_config: Some(OutputConfig {
                effort: "high".to_string(),
            }),
            metadata: None,
        };
        let adaptive_resolution = ModelResolution::resolved(
            "claude-sonnet-4-6".to_string(),
            "claude-sonnet-4.5".to_string(),
            ModelResolutionSource::FamilyNormalized,
        );
        let adaptive_result = convert_request_with_resolved_model(
            &adaptive_req,
            ConverterOptions::default(),
            &adaptive_resolution,
        )
        .expect("adaptive request should convert");
        let adaptive_first_history_user = adaptive_result
            .conversation_state
            .history
            .iter()
            .find_map(|message| match message {
                Message::User(user) => Some(&user.user_input_message),
                _ => None,
            })
            .expect("adaptive thinking controls should be injected");
        assert!(
            adaptive_first_history_user
                .content
                .contains("<thinking_mode>adaptive</thinking_mode>")
        );
        assert!(
            !adaptive_first_history_user
                .content
                .contains("<thinking_output_policy>")
        );
    }

    #[test]
    fn test_native_reasoning_fields_emit_for_supported_models_without_prompt_tags() {
        use super::super::types::{Message as AnthropicMessage, OutputConfig, Thinking};

        let req = MessagesRequest {
            model: "claude-opus-4-7-thinking".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("Hello"),
            }],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: Some(Thinking {
                thinking_type: "adaptive".to_string(),
                budget_tokens: 0,
            }),
            output_config: Some(OutputConfig {
                effort: "xhigh".to_string(),
            }),
            metadata: None,
        };

        let result = convert_request_with_options(&req, ConverterOptions::default())
            .expect("supported native reasoning request should convert");

        let fields = result
            .additional_model_request_fields
            .expect("supported model should emit native reasoning fields");
        assert!(fields.thinking.is_none());
        assert_eq!(fields.output_config.unwrap().effort, "xhigh");
        assert!(
            result
                .conversation_state
                .history
                .iter()
                .all(|message| match message {
                    Message::User(user) =>
                        !user.user_input_message.content.contains("<thinking_mode>"),
                    _ => true,
                })
        );
    }

    #[test]
    fn test_sonnet_4_6_xhigh_downgrades_to_max_for_native_schema() {
        use super::super::types::{Message as AnthropicMessage, OutputConfig, Thinking};

        let req = MessagesRequest {
            model: "claude-sonnet-4-6-thinking".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("Hello"),
            }],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: Some(Thinking {
                thinking_type: "adaptive".to_string(),
                budget_tokens: 0,
            }),
            output_config: Some(OutputConfig {
                effort: "xhigh".to_string(),
            }),
            metadata: None,
        };

        let result = convert_request_with_options(&req, ConverterOptions::default())
            .expect("sonnet native reasoning request should convert");

        let fields = result
            .additional_model_request_fields
            .expect("sonnet 4.6 should emit native reasoning fields");
        assert_eq!(fields.output_config.unwrap().effort, "max");
    }

    #[test]
    fn test_native_reasoning_fields_can_be_disabled_by_conversion_plan() {
        use crate::anthropic::body_capabilities::BodyStageState;

        use super::super::types::{Message as AnthropicMessage, OutputConfig, Thinking};

        let req = MessagesRequest {
            model: "claude-opus-4-7-thinking".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("Hello"),
            }],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: Some(Thinking {
                thinking_type: "adaptive".to_string(),
                budget_tokens: 0,
            }),
            output_config: Some(OutputConfig {
                effort: "xhigh".to_string(),
            }),
            metadata: None,
        };
        let mut options = ConverterOptions::default();
        options.conversion.native_reasoning_fields = BodyStageState::Disabled;

        let result = convert_request_with_options(&req, options)
            .expect("request should convert without native reasoning fields");

        assert!(result.additional_model_request_fields.is_none());
        assert!(result.conversation_state.history.iter().any(|message| {
            match message {
                Message::User(user) => user
                    .user_input_message
                    .content
                    .contains("<thinking_mode>adaptive"),
                _ => false,
            }
        }));
    }

    #[test]
    fn test_force_visible_thinking_adds_policy_for_adaptive_request() {
        use super::super::types::{Message as AnthropicMessage, OutputConfig, Thinking};

        let req = MessagesRequest {
            model: "claude-sonnet-4-5".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("Hello"),
            }],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: Some(Thinking {
                thinking_type: "adaptive".to_string(),
                budget_tokens: 0,
            }),
            output_config: Some(OutputConfig {
                effort: "high".to_string(),
            }),
            metadata: None,
        };

        let result = convert_request_with_options(
            &req,
            ConverterOptions {
                force_visible_thinking: true,
                ..ConverterOptions::default()
            },
        )
        .expect("adaptive request should convert");
        let first_history_user = result
            .conversation_state
            .history
            .iter()
            .find_map(|message| match message {
                Message::User(user) => Some(&user.user_input_message.content),
                _ => None,
            })
            .expect("thinking controls should be injected");

        assert!(first_history_user.contains("<thinking_mode>adaptive</thinking_mode>"));
        assert!(first_history_user.contains("<thinking_effort>high</thinking_effort>"));
        assert!(first_history_user.contains("<thinking_output_policy>"));
    }

    #[test]
    fn test_force_visible_thinking_overrides_strict_prefix_suppression() {
        use super::super::types::{
            Message as AnthropicMessage, OutputConfig, SystemMessage, Thinking,
        };

        let req = MessagesRequest {
            model: "claude-sonnet-4-5".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("Hello"),
            }],
            stream: false,
            system: Some(vec![SystemMessage {
                text: "Reply tersely.".to_string(),
                cache_control: None,
            }]),
            tools: None,
            tool_choice: None,
            thinking: Some(Thinking {
                thinking_type: "adaptive".to_string(),
                budget_tokens: 0,
            }),
            output_config: Some(OutputConfig {
                effort: "high".to_string(),
            }),
            metadata: None,
        };

        let result = convert_request_with_options(
            &req,
            ConverterOptions {
                compat_profile: CompatProfile::AnthropicStrict,
                force_visible_thinking: true,
                ..ConverterOptions::default()
            },
        )
        .expect("strict adaptive request should convert with forced visible thinking");
        let first_history_user = result
            .conversation_state
            .history
            .iter()
            .find_map(|message| match message {
                Message::User(user) => Some(&user.user_input_message.content),
                _ => None,
            })
            .expect("system should be represented as first user history message");

        assert!(first_history_user.contains("<thinking_mode>adaptive</thinking_mode>"));
        assert!(first_history_user.contains("<thinking_output_policy>"));
        assert!(first_history_user.contains("Reply tersely."));
        assert!(!first_history_user.contains(SYSTEM_CHUNKED_POLICY));
    }

    #[test]
    fn test_anthropic_strict_drops_prefill_like_claude_code() {
        use super::super::types::Message as AnthropicMessage;

        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!("Hello"),
                },
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: serde_json::json!("prefill"),
                },
            ],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let result = convert_request_with_options(
            &req,
            ConverterOptions {
                compat_profile: CompatProfile::AnthropicStrict,
                ..ConverterOptions::default()
            },
        )
        .expect("strict profile should still sanitize terminal prefill");

        assert_eq!(result.warnings.prefill_dropped, 1);
        assert_eq!(
            result
                .conversation_state
                .current_message
                .user_input_message
                .content,
            "Hello"
        );
    }

    fn test_tool(name: &str) -> super::super::types::Tool {
        super::super::types::Tool {
            tool_type: None,
            name: name.to_string(),
            description: format!("{} description", name),
            input_schema: HashMap::from([
                ("type".to_string(), serde_json::json!("object")),
                ("properties".to_string(), serde_json::json!({})),
            ]),
            max_uses: None,
            cache_control: None,
        }
    }

    fn base_tool_choice_request(tool_choice: serde_json::Value) -> MessagesRequest {
        use super::super::types::Message as AnthropicMessage;

        MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("use the appropriate tool"),
            }],
            stream: false,
            system: None,
            tools: Some(vec![test_tool("read_file"), test_tool("write_file")]),
            tool_choice: Some(tool_choice),
            thinking: None,
            output_config: None,
            metadata: None,
        }
    }

    #[test]
    fn test_tool_choice_none_omits_current_tools() {
        let req = base_tool_choice_request(serde_json::json!({"type": "none"}));

        let result = convert_request_with_options(&req, ConverterOptions::default()).unwrap();
        let context = &result
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context;

        assert!(context.tools.is_empty());
        assert!(
            result
                .conversation_state
                .history
                .iter()
                .any(|message| matches!(
                    message,
                    Message::User(user)
                        if user.user_input_message.content.contains("<tool_choice>none</tool_choice>")
                )),
            "compat mode should steer Kiro away from tool calls when tool_choice is none"
        );
    }

    #[test]
    fn test_tool_choice_named_tool_filters_current_tools() {
        let req = base_tool_choice_request(serde_json::json!({
            "type": "tool",
            "name": "read_file"
        }));

        let result = convert_request_with_options(&req, ConverterOptions::default()).unwrap();
        let context = &result
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context;

        assert_eq!(context.tools.len(), 1);
        let kiro_tool_name = &context.tools[0].tool_specification.name;
        assert_eq!(
            result.tool_name_map.get(kiro_tool_name),
            Some(&"read_file".to_string())
        );
        assert!(
            result
                .conversation_state
                .history
                .iter()
                .any(|message| matches!(
                    message,
                    Message::User(user)
                        if user.user_input_message.content.contains("<tool_choice_name>read_file</tool_choice_name>")
                )),
            "compat mode should add a Kiro-facing forced-tool steering prefix"
        );
    }

    #[test]
    fn test_anthropic_strict_filters_tool_choice_without_prompt_steering() {
        let req = base_tool_choice_request(serde_json::json!({
            "type": "tool",
            "name": "read_file"
        }));

        let result = convert_request_with_options(
            &req,
            ConverterOptions {
                compat_profile: CompatProfile::AnthropicStrict,
                ..ConverterOptions::default()
            },
        )
        .unwrap();
        let context = &result
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context;

        assert_eq!(context.tools.len(), 1);
        assert!(
            result.conversation_state.history.is_empty(),
            "strict profile should avoid synthetic prompt steering"
        );
    }

    #[test]
    fn cache_point_disabled_by_default() {
        let mut req = base_tool_choice_request(serde_json::json!({"type": "auto"}));
        if let Some(tools) = req.tools.as_mut() {
            tools[0].cache_control = Some(serde_json::json!({"type": "ephemeral"}));
        }

        let result = convert_request_with_options(&req, ConverterOptions::default()).unwrap();

        assert!(result.tool_cache_point_insert_after.is_empty());
        assert!(result.cache_point_plan_recording_enabled);
    }

    #[test]
    fn cache_point_tools_only_records_selected_tool_indices() {
        let mut req = base_tool_choice_request(serde_json::json!({"type": "auto"}));
        if let Some(tools) = req.tools.as_mut() {
            tools[0].cache_control = Some(serde_json::json!({"type": "ephemeral"}));
            tools[1].cache_control = Some(serde_json::json!({"type": "ephemeral"}));
        }

        let result = convert_request_with_options(
            &req,
            ConverterOptions {
                kiro_cache_point_enabled: true,
                ..ConverterOptions::default()
            },
        )
        .unwrap();

        assert_eq!(result.tool_cache_point_insert_after, vec![0, 1]);
    }

    #[test]
    fn cache_point_respects_tool_choice_filtering() {
        let mut req = base_tool_choice_request(serde_json::json!({
            "type": "tool",
            "name": "write_file"
        }));
        if let Some(tools) = req.tools.as_mut() {
            tools[0].cache_control = Some(serde_json::json!({"type": "ephemeral"}));
            tools[1].cache_control = Some(serde_json::json!({"type": "ephemeral"}));
        }

        let result = convert_request_with_options(
            &req,
            ConverterOptions {
                kiro_cache_point_enabled: true,
                ..ConverterOptions::default()
            },
        )
        .unwrap();

        assert_eq!(result.tool_cache_point_insert_after, vec![0]);
        let context = &result
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context;
        assert_eq!(context.tools.len(), 1);
        assert_eq!(
            result
                .tool_name_map
                .get(&context.tools[0].tool_specification.name),
            Some(&"write_file".to_string())
        );
    }

    #[test]
    fn test_validate_tool_pairing_orphaned_result() {
        // 测试孤立的 tool_result 被过滤
        // 历史中没有 tool_use，但 tool_results 中有 tool_result
        let history = vec![
            Message::User(HistoryUserMessage::new("Hello", "claude-sonnet-4.5")),
            Message::Assistant(HistoryAssistantMessage::new("Hi there!")),
        ];

        let tool_results = vec![ToolResult::success("orphan-123", "some result")];

        let (filtered, _, orphan_texts) =
            validate_tool_pairing(&history, &tool_results, &mut ProxyWarnings::default());

        // 孤立的 tool_result 应该被过滤掉
        assert!(filtered.is_empty(), "孤立的 tool_result 应该被过滤");
        assert_eq!(orphan_texts.len(), 1);
        assert!(orphan_texts[0].contains("some result"));
    }

    #[test]
    fn test_validate_tool_pairing_orphaned_use() {
        use crate::kiro::model::requests::tool::ToolUseEntry;

        // 测试孤立的 tool_use（有 tool_use 但没有对应的 tool_result）
        let mut assistant_msg = AssistantMessage::new("I'll read the file.");
        assistant_msg = assistant_msg.with_tool_uses(vec![
            ToolUseEntry::new("tool-orphan", "read")
                .with_input(serde_json::json!({"path": "/test.txt"})),
        ]);

        let history = vec![
            Message::User(HistoryUserMessage::new(
                "Read the file",
                "claude-sonnet-4.5",
            )),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: assistant_msg,
            }),
        ];

        // 没有 tool_result
        let tool_results: Vec<ToolResult> = vec![];

        let (filtered, orphaned, _) =
            validate_tool_pairing(&history, &tool_results, &mut ProxyWarnings::default());

        // 结果应该为空（因为没有 tool_result）
        // 同时应该返回孤立的 tool_use_id
        assert!(filtered.is_empty());
        assert!(orphaned.contains("tool-orphan"));
    }

    #[test]
    fn test_validate_tool_pairing_valid() {
        use crate::kiro::model::requests::tool::ToolUseEntry;

        // 测试正常配对的情况
        let mut assistant_msg = AssistantMessage::new("I'll read the file.");
        assistant_msg = assistant_msg.with_tool_uses(vec![
            ToolUseEntry::new("tool-1", "read")
                .with_input(serde_json::json!({"path": "/test.txt"})),
        ]);

        let history = vec![
            Message::User(HistoryUserMessage::new(
                "Read the file",
                "claude-sonnet-4.5",
            )),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: assistant_msg,
            }),
        ];

        let tool_results = vec![ToolResult::success("tool-1", "file content")];

        let (filtered, orphaned, _) =
            validate_tool_pairing(&history, &tool_results, &mut ProxyWarnings::default());

        // 配对成功，应该保留，无孤立
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].tool_use_id, "tool-1");
        assert!(orphaned.is_empty());
    }

    #[test]
    fn test_validate_tool_pairing_mixed() {
        use crate::kiro::model::requests::tool::ToolUseEntry;

        // 测试混合情况：部分配对成功，部分孤立
        let mut assistant_msg = AssistantMessage::new("I'll use two tools.");
        assistant_msg = assistant_msg.with_tool_uses(vec![
            ToolUseEntry::new("tool-1", "read").with_input(serde_json::json!({})),
            ToolUseEntry::new("tool-2", "write").with_input(serde_json::json!({})),
        ]);

        let history = vec![
            Message::User(HistoryUserMessage::new("Do something", "claude-sonnet-4.5")),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: assistant_msg,
            }),
        ];

        // tool_results: tool-1 配对，tool-3 孤立
        let tool_results = vec![
            ToolResult::success("tool-1", "result 1"),
            ToolResult::success("tool-3", "orphan result"), // 孤立
        ];

        let (filtered, orphaned, orphan_texts) =
            validate_tool_pairing(&history, &tool_results, &mut ProxyWarnings::default());

        // 只有 tool-1 应该保留
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].tool_use_id, "tool-1");
        // tool-2 是孤立的 tool_use（无 result），tool-3 是孤立的 tool_result
        assert!(orphaned.contains("tool-2"));
        assert_eq!(orphan_texts.len(), 1);
        assert!(orphan_texts[0].contains("orphan result"));
    }

    #[test]
    fn test_validate_tool_pairing_history_already_paired() {
        use crate::kiro::model::requests::tool::ToolUseEntry;

        // 测试历史中已配对的 tool_use 不应该被报告为孤立
        // 场景：多轮对话中，之前的 tool_use 已经在历史中有对应的 tool_result
        let mut assistant_msg1 = AssistantMessage::new("I'll read the file.");
        assistant_msg1 = assistant_msg1.with_tool_uses(vec![
            ToolUseEntry::new("tool-1", "read")
                .with_input(serde_json::json!({"path": "/test.txt"})),
        ]);

        // 构建历史中的 user 消息，包含 tool_result
        let mut user_msg_with_result = UserMessage::new("", "claude-sonnet-4.5");
        let mut ctx = UserInputMessageContext::new();
        ctx = ctx.with_tool_results(vec![ToolResult::success("tool-1", "file content")]);
        user_msg_with_result = user_msg_with_result.with_context(ctx);

        let history = vec![
            // 第一轮：用户请求
            Message::User(HistoryUserMessage::new(
                "Read the file",
                "claude-sonnet-4.5",
            )),
            // 第一轮：assistant 使用工具
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: assistant_msg1,
            }),
            // 第二轮：用户返回工具结果（历史中已配对）
            Message::User(HistoryUserMessage {
                user_input_message: user_msg_with_result,
            }),
            // 第二轮：assistant 响应
            Message::Assistant(HistoryAssistantMessage::new("The file contains...")),
        ];

        // 当前消息没有 tool_results（用户只是继续对话）
        let tool_results: Vec<ToolResult> = vec![];

        let (filtered, orphaned, _) =
            validate_tool_pairing(&history, &tool_results, &mut ProxyWarnings::default());

        // 结果应该为空，且不应该有孤立 tool_use
        // 因为 tool-1 已经在历史中配对了
        assert!(filtered.is_empty());
        assert!(orphaned.is_empty());
    }

    #[test]
    fn test_validate_tool_pairing_duplicate_result() {
        use crate::kiro::model::requests::tool::ToolUseEntry;

        // 测试重复的 tool_result（历史中已配对，当前消息又发送了相同的 tool_result）
        let mut assistant_msg = AssistantMessage::new("I'll read the file.");
        assistant_msg = assistant_msg.with_tool_uses(vec![
            ToolUseEntry::new("tool-1", "read")
                .with_input(serde_json::json!({"path": "/test.txt"})),
        ]);

        // 历史中已有 tool_result
        let mut user_msg_with_result = UserMessage::new("", "claude-sonnet-4.5");
        let mut ctx = UserInputMessageContext::new();
        ctx = ctx.with_tool_results(vec![ToolResult::success("tool-1", "file content")]);
        user_msg_with_result = user_msg_with_result.with_context(ctx);

        let history = vec![
            Message::User(HistoryUserMessage::new(
                "Read the file",
                "claude-sonnet-4.5",
            )),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: assistant_msg,
            }),
            Message::User(HistoryUserMessage {
                user_input_message: user_msg_with_result,
            }),
            Message::Assistant(HistoryAssistantMessage::new("Done")),
        ];

        // 当前消息又发送了相同的 tool_result（重复）
        let tool_results = vec![ToolResult::success("tool-1", "file content again")];

        let (filtered, _, _) =
            validate_tool_pairing(&history, &tool_results, &mut ProxyWarnings::default());

        // 重复的 tool_result 应该被过滤掉
        assert!(filtered.is_empty(), "重复的 tool_result 应该被过滤");
    }

    #[test]
    fn test_validate_tool_pairing_textifies_duplicate_current_result() {
        use crate::kiro::model::requests::tool::ToolUseEntry;

        let mut assistant_msg = AssistantMessage::new("I'll read the file.");
        assistant_msg = assistant_msg.with_tool_uses(vec![
            ToolUseEntry::new("tool-1", "read")
                .with_input(serde_json::json!({"path": "/test.txt"})),
        ]);

        let history = vec![
            Message::User(HistoryUserMessage::new(
                "Read the file",
                "claude-sonnet-4.5",
            )),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: assistant_msg,
            }),
        ];

        let tool_results = vec![
            ToolResult::success("tool-1", "first result"),
            ToolResult::success("tool-1", "duplicate result"),
        ];
        let mut warnings = ProxyWarnings::default();

        let (filtered, orphaned, textified) =
            validate_tool_pairing(&history, &tool_results, &mut warnings);

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].tool_use_id, "tool-1");
        assert!(orphaned.is_empty());
        assert_eq!(textified.len(), 1);
        assert!(textified[0].contains("duplicate result"));
        assert_eq!(warnings.duplicate_tool_results, 1);
        assert_eq!(warnings.duplicate_tool_results_textified, 1);
    }

    #[test]
    fn test_validate_tool_pairing_allows_current_result_for_reused_last_tool_use_id() {
        use crate::kiro::model::requests::tool::ToolUseEntry;

        let mut first_assistant = AssistantMessage::new("First read.");
        first_assistant = first_assistant.with_tool_uses(vec![
            ToolUseEntry::new("tool-1", "read").with_input(serde_json::json!({"path": "/a"})),
        ]);

        let mut first_result_user = UserMessage::new(" ", "claude-sonnet-4.5");
        let mut first_ctx = UserInputMessageContext::new();
        first_ctx = first_ctx.with_tool_results(vec![ToolResult::success("tool-1", "first")]);
        first_result_user = first_result_user.with_context(first_ctx);

        let mut second_assistant = AssistantMessage::new("Second read.");
        second_assistant = second_assistant.with_tool_uses(vec![
            ToolUseEntry::new("tool-1", "read").with_input(serde_json::json!({"path": "/b"})),
        ]);

        let history = vec![
            Message::User(HistoryUserMessage::new("Read A", "claude-sonnet-4.5")),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: first_assistant,
            }),
            Message::User(HistoryUserMessage {
                user_input_message: first_result_user,
            }),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: second_assistant,
            }),
        ];

        let tool_results = vec![ToolResult::success("tool-1", "second")];

        let (filtered, orphaned, orphan_texts) =
            validate_tool_pairing(&history, &tool_results, &mut ProxyWarnings::default());

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].tool_use_id, "tool-1");
        assert!(orphaned.is_empty());
        assert!(orphan_texts.is_empty());
    }

    #[test]
    fn test_validate_tool_pairing_textifies_result_for_non_adjacent_tool_use() {
        use crate::kiro::model::requests::tool::ToolUseEntry;

        let mut first_assistant = AssistantMessage::new("First read.");
        first_assistant = first_assistant.with_tool_uses(vec![
            ToolUseEntry::new("tool-1", "read").with_input(serde_json::json!({"path": "/a"})),
        ]);

        let mut first_result_user = UserMessage::new(" ", "claude-sonnet-4.5");
        let mut first_ctx = UserInputMessageContext::new();
        first_ctx = first_ctx.with_tool_results(vec![ToolResult::success("tool-1", "first")]);
        first_result_user = first_result_user.with_context(first_ctx);

        let mut second_assistant = AssistantMessage::new("Second read.");
        second_assistant = second_assistant.with_tool_uses(vec![
            ToolUseEntry::new("tool-2", "read").with_input(serde_json::json!({"path": "/b"})),
        ]);

        let history = vec![
            Message::User(HistoryUserMessage::new("Read A", "claude-sonnet-4.5")),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: first_assistant,
            }),
            Message::User(HistoryUserMessage {
                user_input_message: first_result_user,
            }),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: second_assistant,
            }),
        ];

        let tool_results = vec![ToolResult::success("tool-1", "stale repeat")];

        let (filtered, orphaned, orphan_texts) =
            validate_tool_pairing(&history, &tool_results, &mut ProxyWarnings::default());

        assert!(filtered.is_empty());
        assert!(orphaned.contains("tool-2"));
        assert_eq!(orphan_texts.len(), 1);
        assert!(orphan_texts[0].contains("stale repeat"));
    }

    #[test]
    fn test_convert_assistant_message_tool_use_only() {
        use super::super::types::Message as AnthropicMessage;

        // 测试仅包含 tool_use 的 assistant 消息（无 text 块）
        // Kiro API 要求 content 字段不能为空
        let msg = AnthropicMessage {
            role: "assistant".to_string(),
            content: serde_json::json!([
                {"type": "tool_use", "id": "toolu_01ABC", "name": "read_file", "input": {"path": "/test.txt"}}
            ]),
        };

        let mut tool_name_map = HashMap::new();
        let result =
            convert_assistant_message(&msg, &mut tool_name_map, ConverterOptions::default())
                .expect("应该成功转换");

        // 验证 content 不为空（使用占位符）
        assert!(
            !result.assistant_response_message.content.is_empty(),
            "content 不应为空"
        );
        assert_eq!(
            result.assistant_response_message.content, " ",
            "仅 tool_use 时应使用 ' ' 占位符"
        );

        // 验证 tool_uses 被正确保留
        let tool_uses = result
            .assistant_response_message
            .tool_uses
            .expect("应该有 tool_uses");
        assert_eq!(tool_uses.len(), 1);
        assert_eq!(tool_uses[0].tool_use_id, "toolu_01ABC");
        assert_ne!(tool_uses[0].name, "read_file");
        assert!(
            tool_uses[0]
                .name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric())
        );
        assert_eq!(
            tool_name_map.get(&tool_uses[0].name),
            Some(&"read_file".to_string())
        );
    }

    #[test]
    fn test_convert_assistant_message_ignores_empty_tool_use_identity() {
        let msg = super::super::types::Message {
            role: "assistant".to_string(),
            content: serde_json::json!([
                {"type": "tool_use", "id": "   ", "name": "read_file", "input": {"path": "/test.txt"}},
                {"type": "tool_use", "id": "toolu_valid", "name": "   ", "input": {"path": "/test.txt"}},
                {"type": "tool_use", "id": "toolu_ok", "name": "read_file", "input": {"path": "/test.txt"}}
            ]),
        };

        let mut tool_name_map = HashMap::new();
        let result =
            convert_assistant_message(&msg, &mut tool_name_map, ConverterOptions::default())
                .expect("convert");
        let tool_uses = result
            .assistant_response_message
            .tool_uses
            .expect("valid tool use should remain");

        assert_eq!(tool_uses.len(), 1);
        assert_eq!(tool_uses[0].tool_use_id, "toolu_ok");
    }

    #[test]
    fn test_tool_use_ids_are_sanitized_consistently() {
        let raw_id = "toolu:01/ABC";
        let sanitized = sanitize_tool_use_id(raw_id).expect("sanitized id");
        assert!(
            sanitized
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
        );
        assert_ne!(sanitized, raw_id);

        let assistant = super::super::types::Message {
            role: "assistant".to_string(),
            content: serde_json::json!([
                {"type": "tool_use", "id": raw_id, "name": "read_file", "input": {"path": "/test.txt"}}
            ]),
        };
        let user_content = serde_json::json!([
            {"type": "tool_result", "tool_use_id": raw_id, "content": "done"}
        ]);

        let mut tool_name_map = HashMap::new();
        let assistant =
            convert_assistant_message(&assistant, &mut tool_name_map, ConverterOptions::default())
                .expect("convert");
        let (_, _, tool_results) = process_message_content(&user_content).expect("process");

        assert_eq!(
            assistant
                .assistant_response_message
                .tool_uses
                .as_ref()
                .expect("tool use")[0]
                .tool_use_id,
            sanitized
        );
        assert_eq!(tool_results[0].tool_use_id, sanitized);
    }

    #[test]
    fn test_convert_assistant_message_wraps_non_object_tool_input() {
        let msg = super::super::types::Message {
            role: "assistant".to_string(),
            content: serde_json::json!([
                {"type": "tool_use", "id": "toolu_scalar", "name": "run", "input": "raw input"}
            ]),
        };

        let mut tool_name_map = HashMap::new();
        let result =
            convert_assistant_message(&msg, &mut tool_name_map, ConverterOptions::default())
                .expect("convert");
        let tool_uses = result
            .assistant_response_message
            .tool_uses
            .expect("tool use should remain");

        assert_eq!(
            tool_uses[0].input,
            serde_json::json!({"value": "raw input"})
        );
    }

    #[test]
    fn test_process_message_content_ignores_empty_tool_result_id() {
        let content = serde_json::json!([
            {"type": "tool_result", "tool_use_id": " ", "content": "ignored"},
            {"type": "tool_result", "tool_use_id": "toolu_ok", "content": "kept"}
        ]);

        let (_, _, tool_results) = process_message_content(&content).expect("process");

        assert_eq!(tool_results.len(), 1);
        assert_eq!(tool_results[0].tool_use_id, "toolu_ok");
    }

    #[test]
    fn test_process_message_content_replaces_empty_tool_result_content() {
        let content = serde_json::json!([
            {"type": "tool_result", "tool_use_id": "toolu_ok", "content": []}
        ]);

        let (_, _, tool_results) = process_message_content(&content).expect("process");
        let text = tool_results[0].content[0]
            .get("text")
            .and_then(|value| value.as_str())
            .expect("tool result text");

        assert_eq!(text, EMPTY_TOOL_RESULT_CONTENT_PLACEHOLDER);
    }

    #[test]
    fn test_process_message_content_extracts_images_from_tool_results() {
        let content = serde_json::json!([
            {
                "type": "tool_result",
                "tool_use_id": "toolu_ok",
                "content": [
                    {"type": "text", "text": "plain"},
                    {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": VALID_PNG_1X1_BASE64}}
                ]
            }
        ]);

        let (_, images, tool_results) = process_message_content(&content).expect("process");
        let text = kiro_tool_result_to_text(&tool_results[0]).expect("text");

        assert_eq!(images.len(), 1);
        assert_eq!(images[0].format, "png");
        assert_eq!(
            images[0].source.bytes.as_deref(),
            Some(VALID_PNG_1X1_BASE64)
        );
        assert!(text.contains("plain"));
        assert!(text.contains("[image attached]"));
        assert!(!text.contains("iVBOR"));
    }

    #[test]
    fn test_base64_image_uses_detected_format_over_declared_media_type() {
        let jpeg = valid_jpeg_base64();
        let content = serde_json::json!([
            {
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/png",
                    "data": jpeg
                }
            }
        ]);

        let (_, images, _) = process_message_content(&content).expect("process");

        assert_eq!(images.len(), 1);
        assert_eq!(images[0].format, "jpeg");
    }

    #[test]
    fn test_base64_image_accepts_data_url_in_data_field() {
        let jpeg = valid_jpeg_base64();
        let content = serde_json::json!([
            {
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/png",
                    "data": format!("data:image/jpeg;base64,{}", jpeg)
                }
            }
        ]);

        let (_, images, _) = process_message_content(&content).expect("process");

        assert_eq!(images.len(), 1);
        assert_eq!(images[0].format, "jpeg");
        assert_eq!(images[0].source.bytes.as_deref(), Some(jpeg.as_str()));
    }

    #[test]
    fn test_base64_image_data_url_can_supply_media_type() {
        let png = VALID_PNG_1X1_BASE64;
        let content = serde_json::json!([
            {
                "type": "image",
                "source": {
                    "type": "base64",
                    "data": format!("data:image/png;base64,{}", png)
                }
            }
        ]);

        let (_, images, _) = process_message_content(&content).expect("process");

        assert_eq!(images.len(), 1);
        assert_eq!(images[0].format, "png");
        assert_eq!(images[0].source.bytes.as_deref(), Some(png));
    }

    #[test]
    fn test_base64_image_strips_whitespace_before_kiro_conversion() {
        let png = VALID_PNG_1X1_BASE64;
        let spaced_png = png
            .as_bytes()
            .chunks(12)
            .map(|chunk| std::str::from_utf8(chunk).expect("valid b64 chunk"))
            .collect::<Vec<_>>()
            .join("\n ");
        let content = serde_json::json!([
            {
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/png",
                    "data": spaced_png
                }
            }
        ]);

        let (_, images, _) = process_message_content(&content).expect("process");

        assert_eq!(images.len(), 1);
        assert_eq!(images[0].source.bytes.as_deref(), Some(png));
    }

    #[test]
    fn test_data_url_image_uses_detected_format_over_declared_media_type() {
        let jpeg = valid_jpeg_base64();
        let content = serde_json::json!([
            {
                "type": "image",
                "source": {
                    "type": "url",
                    "url": format!("data:image/png;charset=utf-8;base64,{}", jpeg)
                }
            }
        ]);

        let (_, images, _) = process_message_content(&content).expect("process");

        assert_eq!(images.len(), 1);
        assert_eq!(images[0].format, "jpeg");
    }

    #[test]
    fn test_convert_assistant_message_with_text_and_tool_use() {
        use super::super::types::Message as AnthropicMessage;

        // 测试同时包含 text 和 tool_use 的 assistant 消息
        let msg = AnthropicMessage {
            role: "assistant".to_string(),
            content: serde_json::json!([
                {"type": "text", "text": "Let me read that file for you."},
                {"type": "tool_use", "id": "toolu_02XYZ", "name": "read_file", "input": {"path": "/data.json"}}
            ]),
        };

        let mut tool_name_map = HashMap::new();
        let result =
            convert_assistant_message(&msg, &mut tool_name_map, ConverterOptions::default())
                .expect("应该成功转换");

        // 验证 content 使用原始文本（不是占位符）
        assert_eq!(
            result.assistant_response_message.content,
            "Let me read that file for you."
        );

        // 验证 tool_uses 被正确保留
        let tool_uses = result
            .assistant_response_message
            .tool_uses
            .expect("应该有 tool_uses");
        assert_eq!(tool_uses.len(), 1);
        assert_eq!(tool_uses[0].tool_use_id, "toolu_02XYZ");
        assert_eq!(
            tool_name_map.get(&tool_uses[0].name),
            Some(&"read_file".to_string())
        );
    }

    #[test]
    fn test_remove_orphaned_tool_uses() {
        use crate::kiro::model::requests::tool::ToolUseEntry;

        // 测试从历史中移除孤立的 tool_use
        let mut assistant_msg = AssistantMessage::new("I'll use multiple tools.");
        assistant_msg = assistant_msg.with_tool_uses(vec![
            ToolUseEntry::new("tool-1", "read").with_input(serde_json::json!({})),
            ToolUseEntry::new("tool-2", "write").with_input(serde_json::json!({})),
            ToolUseEntry::new("tool-3", "delete").with_input(serde_json::json!({})),
        ]);

        let mut history = vec![
            Message::User(HistoryUserMessage::new("Do something", "claude-sonnet-4.5")),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: assistant_msg,
            }),
        ];

        // 移除 tool-1 和 tool-3
        let mut orphaned = std::collections::HashSet::new();
        orphaned.insert("tool-1".to_string());
        orphaned.insert("tool-3".to_string());

        remove_orphaned_tool_uses(&mut history, &orphaned);

        // 验证只剩下 tool-2
        if let Message::Assistant(ref assistant_msg) = history[1] {
            let tool_uses = assistant_msg
                .assistant_response_message
                .tool_uses
                .as_ref()
                .expect("应该还有 tool_uses");
            assert_eq!(tool_uses.len(), 1);
            assert_eq!(tool_uses[0].tool_use_id, "tool-2");
        } else {
            panic!("应该是 Assistant 消息");
        }
    }

    #[test]
    fn test_remove_orphaned_tool_uses_all_removed() {
        use crate::kiro::model::requests::tool::ToolUseEntry;

        // 测试移除所有 tool_use 后，tool_uses 变为 None
        let mut assistant_msg = AssistantMessage::new("I'll use a tool.");
        assistant_msg = assistant_msg.with_tool_uses(vec![
            ToolUseEntry::new("tool-1", "read").with_input(serde_json::json!({})),
        ]);

        let mut history = vec![
            Message::User(HistoryUserMessage::new("Do something", "claude-sonnet-4.5")),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: assistant_msg,
            }),
        ];

        let mut orphaned = std::collections::HashSet::new();
        orphaned.insert("tool-1".to_string());

        remove_orphaned_tool_uses(&mut history, &orphaned);

        // 验证 tool_uses 变为 None
        if let Message::Assistant(ref assistant_msg) = history[1] {
            assert!(
                assistant_msg.assistant_response_message.tool_uses.is_none(),
                "移除所有 tool_use 后应为 None"
            );
        } else {
            panic!("应该是 Assistant 消息");
        }
    }

    #[test]
    fn test_merge_consecutive_assistant_messages() {
        // 测试连续 assistant 消息被正确合并（Issue #79）
        use super::super::types::Message as AnthropicMessage;

        let msg1 = AnthropicMessage {
            role: "assistant".to_string(),
            content: serde_json::json!([
                {"type": "thinking", "thinking": "Let me think about this..."},
                {"type": "text", "text": " "}
            ]),
        };

        let msg2 = AnthropicMessage {
            role: "assistant".to_string(),
            content: serde_json::json!([
                {"type": "thinking", "thinking": "I should read the file."},
                {"type": "text", "text": "Let me read that file."},
                {"type": "tool_use", "id": "toolu_01ABC", "name": "read_file", "input": {"path": "/test.txt"}}
            ]),
        };

        let messages: Vec<&AnthropicMessage> = vec![&msg1, &msg2];
        let result =
            merge_assistant_messages(&messages, &mut HashMap::new(), ConverterOptions::default())
                .expect("合并应成功");

        let content = &result.assistant_response_message.content;
        assert!(content.contains("<thinking>"), "应包含 thinking 标签");
        assert!(
            content.contains("Let me read that file"),
            "应包含第二条消息的 text 内容"
        );

        let tool_uses = result
            .assistant_response_message
            .tool_uses
            .expect("应有 tool_uses");
        assert_eq!(tool_uses.len(), 1);
        assert_eq!(tool_uses[0].tool_use_id, "toolu_01ABC");
    }

    #[test]
    fn test_consecutive_assistant_with_tool_use_result_pairing() {
        // 测试 Issue #79 的完整场景
        use super::super::types::Message as AnthropicMessage;

        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!("Read the config file"),
                },
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: serde_json::json!([
                        {"type": "thinking", "thinking": "I need to read the file..."},
                        {"type": "text", "text": " "}
                    ]),
                },
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: serde_json::json!([
                        {"type": "thinking", "thinking": "Let me read the config."},
                        {"type": "text", "text": "I'll read the config file for you."},
                        {"type": "tool_use", "id": "toolu_01XYZ", "name": "read_file", "input": {"path": "/config.json"}}
                    ]),
                },
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!([
                        {"type": "tool_result", "tool_use_id": "toolu_01XYZ", "content": "{\"key\": \"value\"}"}
                    ]),
                },
            ],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let result = convert_request(&req);
        assert!(
            result.is_ok(),
            "连续 assistant 消息场景不应报错: {:?}",
            result.err()
        );

        let state = result.unwrap().conversation_state;
        let mut found_tool_use = false;
        for msg in &state.history {
            if let Message::Assistant(assistant_msg) = msg {
                if let Some(ref tool_uses) = assistant_msg.assistant_response_message.tool_uses {
                    if tool_uses.iter().any(|t| t.tool_use_id == "toolu_01XYZ") {
                        found_tool_use = true;
                        break;
                    }
                }
            }
        }
        assert!(found_tool_use, "合并后的 assistant 消息应包含 tool_use");
    }

    #[test]
    fn test_convert_request_attaches_tool_result_image_to_current_message() {
        use super::super::types::Message as AnthropicMessage;

        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 128,
            messages: vec![
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!("Inspect the image"),
                },
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: serde_json::json!([{
                        "type": "tool_use",
                        "id": "toolu_image",
                        "name": "Read",
                        "input": {"file_path": "fixture.png"}
                    }]),
                },
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!([{
                            "type": "tool_result",
                            "tool_use_id": "toolu_image",
                            "content": [{
                                "type": "image",
                                "source": {
                                "type": "base64",
                                "media_type": "image/png",
                                "data": VALID_PNG_1X1_BASE64
                            }
                        }]
                    }]),
                },
            ],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let result = convert_request(&req).expect("tool-result image should convert");
        let current = result.conversation_state.current_message.user_input_message;

        assert_eq!(current.images.len(), 1);
        assert_eq!(current.images[0].format, "png");
        assert_eq!(
            current.images[0].source.bytes.as_deref(),
            Some(VALID_PNG_1X1_BASE64)
        );
        assert_eq!(current.user_input_message_context.tool_results.len(), 1);
        assert_eq!(
            current.user_input_message_context.tool_results[0].content[0]
                .get("text")
                .and_then(serde_json::Value::as_str),
            Some("[image attached]")
        );
    }
}
