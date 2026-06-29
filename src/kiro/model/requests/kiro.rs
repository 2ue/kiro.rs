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

fn default_cache_point_plan_recording_enabled() -> bool {
    true
}

impl KiroRequest {
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
}
