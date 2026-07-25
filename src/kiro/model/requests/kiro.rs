//! Kiro 请求类型定义
//!
//! 定义 Kiro API 的主请求结构

use serde::{Deserialize, Serialize};

use super::conversation::ConversationState;

/// Kiro API 请求
///
/// 用于构建发送给 Kiro API 的请求
///
/// # 示例
///
/// ```rust
/// use kiro_rs::kiro::model::requests::{
///     KiroRequest, ConversationState, CurrentMessage, UserInputMessage, Tool
/// };
///
/// // 创建简单请求
/// let state = ConversationState::new("conv-123")
///     .with_agent_task_type("vibe")
///     .with_current_message(CurrentMessage::new(
///         UserInputMessage::new("Hello", "claude-3-5-sonnet")
///     ));
///
/// let request = KiroRequest::new(state);
/// let json = request.to_json().unwrap();
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KiroRequest {
    /// 对话状态
    pub conversation_state: ConversationState,
    /// Profile ARN（可选）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_arn: Option<String>,
    /// Kiro 模型原生扩展字段，例如 reasoning effort。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub additional_model_request_fields: Option<AdditionalModelRequestFields>,
    /// 仅代理运行期使用：在最终 JSON 的 tools 数组中指定哪些工具后插入 Kiro cachePoint。
    ///
    /// Kiro 的 tools 数组是 `toolSpecification | cachePoint` 联合结构；Rust 侧仍保持
    /// `Tool` 强类型，最后序列化时再按这个计划插入 cachePoint，避免破坏现有工具诊断。
    #[serde(default, skip)]
    pub tool_cache_point_insert_after: Vec<usize>,
    /// 是否把 cachePoint 插入计划写入 payload diagnostics。
    #[serde(default = "default_cache_point_plan_recording_enabled", skip)]
    pub cache_point_plan_recording_enabled: bool,
}

/// Kiro `additionalModelRequestFields` 容器。
///
/// 注意：外层 `KiroRequest` 使用 camelCase，因此字段名会是
/// `additionalModelRequestFields`；但这里的内层字段按真实 wire format 保持
/// `output_config` 这种 snake_case。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AdditionalModelRequestFields {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<KiroThinkingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_config: Option<KiroOutputConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<KiroReasoningConfig>,
}

impl AdditionalModelRequestFields {
    pub fn normalize_output_config_thinking_compatibility(&mut self) -> bool {
        if self.output_config.is_some()
            && self
                .thinking
                .as_ref()
                .is_some_and(|thinking| thinking.thinking_type != "adaptive")
        {
            self.thinking = None;
            return true;
        }
        false
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KiroThinkingConfig {
    #[serde(rename = "type")]
    pub thinking_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KiroOutputConfig {
    pub effort: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KiroReasoningConfig {
    pub effort: String,
}

fn default_cache_point_plan_recording_enabled() -> bool {
    true
}

impl KiroRequest {
    /// Normalize Kiro-native reasoning fields to the upstream wire contract.
    ///
    /// Kiro accepts `additionalModelRequestFields.output_config` only when the sibling
    /// `thinking` field is either omitted or explicitly `{"type":"adaptive"}`. The Anthropic
    /// ingress protocol may legitimately use `thinking.type=disabled` together with an
    /// `output_config.effort`; by the time we send Kiro-native fields upstream, the disabled
    /// client preference has already been applied to downstream visibility, so the safest Kiro
    /// wire representation is to omit the incompatible sibling `thinking` field.
    pub fn normalize_output_config_thinking_compatibility(&mut self) -> bool {
        self.additional_model_request_fields.as_mut().is_some_and(
            AdditionalModelRequestFields::normalize_output_config_thinking_compatibility,
        )
    }

    pub fn has_tool_cache_point_plan(&self) -> bool {
        !self.tool_cache_point_insert_after.is_empty()
    }

    pub fn clear_tool_cache_point_plan(&mut self) -> usize {
        let planned = self.tool_cache_point_insert_after.len();
        self.tool_cache_point_insert_after.clear();
        planned
    }
}
#[cfg(test)]
mod tests {
    use super::super::conversation::{CurrentMessage, UserInputMessage};
    use super::*;
    #[test]
    fn test_kiro_request_deserialize() {
        let json = r#"{
            "conversationState": {
                "conversationId": "conv-456",
                "currentMessage": {
                    "userInputMessage": {
                        "content": "Test message",
                        "modelId": "claude-3-5-sonnet",
                        "userInputMessageContext": {}
                    }
                }
            }
        }"#;

        let request: KiroRequest = serde_json::from_str(json).unwrap();
        assert_eq!(request.conversation_state.conversation_id, "conv-456");
        assert_eq!(
            request
                .conversation_state
                .current_message
                .user_input_message
                .content,
            "Test message"
        );
    }

    #[test]
    fn test_additional_model_request_fields_wire_format() {
        let state = ConversationState::new("conv").with_current_message(CurrentMessage::new(
            UserInputMessage::new("hi", "claude-opus-4.7"),
        ));
        let request = KiroRequest {
            conversation_state: state,
            profile_arn: None,
            additional_model_request_fields: Some(AdditionalModelRequestFields {
                thinking: None,
                output_config: Some(KiroOutputConfig {
                    effort: "xhigh".to_string(),
                }),
                reasoning: None,
            }),
            tool_cache_point_insert_after: Vec::new(),
            cache_point_plan_recording_enabled: true,
        };

        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(
            value["additionalModelRequestFields"]["output_config"]["effort"],
            "xhigh"
        );
        assert!(
            value["additionalModelRequestFields"]
                .get("outputConfig")
                .is_none()
        );
        assert!(value.get("toolCachePointInsertAfter").is_none());
    }

    #[test]
    fn output_config_thinking_compatibility_normalizer_drops_non_adaptive_thinking_for_five_rounds()
    {
        for round in 0..5 {
            let state = ConversationState::new("conv").with_current_message(CurrentMessage::new(
                UserInputMessage::new("hi", "claude-opus-4.7"),
            ));
            let mut request = KiroRequest {
                conversation_state: state,
                profile_arn: None,
                additional_model_request_fields: Some(AdditionalModelRequestFields {
                    thinking: Some(KiroThinkingConfig {
                        thinking_type: "disabled".to_string(),
                        display: None,
                    }),
                    output_config: Some(KiroOutputConfig {
                        effort: "max".to_string(),
                    }),
                    reasoning: None,
                }),
                tool_cache_point_insert_after: Vec::new(),
                cache_point_plan_recording_enabled: true,
            };

            assert!(
                request.normalize_output_config_thinking_compatibility(),
                "round {round}"
            );
            let value = serde_json::to_value(&request).unwrap();
            assert!(
                value["additionalModelRequestFields"]
                    .get("thinking")
                    .is_none(),
                "round {round}"
            );
            assert_eq!(
                value["additionalModelRequestFields"]["output_config"]["effort"], "max",
                "round {round}"
            );
        }
    }
}
