//! Local upstream request payload types.
//!
//! These structs describe the current local-upstream wire payload used by the
//! legacy implementation while the runtime is being migrated to account-owned
//! upstream execution.

use serde::{Deserialize, Serialize};

use super::conversation::ConversationState;

/// Local-upstream request payload.
///
/// Used to build the JSON request body sent to the current local upstream
/// implementation.
///
/// # 示例
///
/// ```rust
/// use crate::local_upstream_impl::model::requests::{
///     LocalUpstreamRequest, ConversationState, CurrentMessage, UserInputMessage, Tool
/// };
///
/// // 创建简单请求
/// let state = ConversationState::new("conv-123")
///     .with_agent_task_type("vibe")
///     .with_current_message(CurrentMessage::new(
///         UserInputMessage::new("Hello", "claude-3-5-sonnet")
///     ));
///
/// let request = LocalUpstreamRequest::new(state);
/// let json = request.to_json().unwrap();
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalUpstreamRequest {
    /// 对话状态
    pub conversation_state: ConversationState,
    /// Optional upstream identity selector retained only in test builds.
    #[cfg(test)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_arn: Option<String>,
    /// Local-upstream native model extension fields, such as reasoning effort.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub additional_model_request_fields: Option<LocalUpstreamAdditionalModelRequestFields>,
    /// Runtime-only plan for inserting upstream cachePoint entries after selected tools.
    ///
    /// The upstream tools array is a `toolSpecification | cachePoint` union. Rust keeps
    /// `Tool` strongly typed and applies this insertion plan at final serialization time
    /// so existing tool diagnostics can stay type-safe.
    #[serde(default, skip)]
    pub tool_cache_point_insert_after: Vec<usize>,
    /// 是否把 cachePoint 插入计划写入 payload diagnostics。
    #[serde(default = "default_cache_point_plan_recording_enabled", skip)]
    pub cache_point_plan_recording_enabled: bool,
}

/// Local-upstream `additionalModelRequestFields` container.
///
/// 注意：外层 `LocalUpstreamRequest` 使用 camelCase，因此字段名会是
/// `additionalModelRequestFields`；但这里的内层字段按真实 wire format 保持
/// `output_config` 这种 snake_case。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LocalUpstreamAdditionalModelRequestFields {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<LocalUpstreamThinkingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_config: Option<LocalUpstreamOutputConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<LocalUpstreamReasoningConfig>,
}

impl LocalUpstreamAdditionalModelRequestFields {
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
pub struct LocalUpstreamThinkingConfig {
    #[serde(rename = "type")]
    pub thinking_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalUpstreamOutputConfig {
    pub effort: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalUpstreamReasoningConfig {
    pub effort: String,
}

fn default_cache_point_plan_recording_enabled() -> bool {
    true
}

impl LocalUpstreamRequest {
    /// Normalize local-upstream native reasoning fields to the upstream wire contract.
    ///
    /// The current upstream accepts `additionalModelRequestFields.output_config` only when the sibling
    /// `thinking` field is either omitted or explicitly `{"type":"adaptive"}`. The Anthropic
    /// ingress protocol may legitimately use `thinking.type=disabled` together with an
    /// `output_config.effort`; by the time we send native fields upstream, the disabled
    /// client preference has already been applied to downstream visibility, so the safest
    /// wire representation is to omit the incompatible sibling `thinking` field.
    pub fn normalize_output_config_thinking_compatibility(&mut self) -> bool {
        self.additional_model_request_fields.as_mut().is_some_and(
            LocalUpstreamAdditionalModelRequestFields::normalize_output_config_thinking_compatibility,
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
    fn local_upstream_request_deserializes() {
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

        let request: LocalUpstreamRequest = serde_json::from_str(json).unwrap();
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
        let request = LocalUpstreamRequest {
            conversation_state: state,
            profile_arn: None,
            additional_model_request_fields: Some(LocalUpstreamAdditionalModelRequestFields {
                thinking: None,
                output_config: Some(LocalUpstreamOutputConfig {
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
            let mut request = LocalUpstreamRequest {
                conversation_state: state,
                profile_arn: None,
                additional_model_request_fields: Some(LocalUpstreamAdditionalModelRequestFields {
                    thinking: Some(LocalUpstreamThinkingConfig {
                        thinking_type: "disabled".to_string(),
                        display: None,
                    }),
                    output_config: Some(LocalUpstreamOutputConfig {
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
