//! 对话类型定义
//!
//! 定义 Kiro API 中对话相关的类型，包括消息、历史记录等

use serde::{Deserialize, Serialize};

use super::tool::{Tool, ToolResult, ToolUseEntry};

/// 对话状态
///
/// Kiro API 请求中的核心结构，包含当前消息和历史记录
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationState {
    /// 代理延续 ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_continuation_id: Option<String>,
    /// 代理任务类型（通常为 "vibe"）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_task_type: Option<String>,
    /// 聊天触发类型（"MANUAL" 或 "AUTO"）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_trigger_type: Option<String>,
    /// 当前消息
    pub current_message: CurrentMessage,
    /// 会话 ID
    pub conversation_id: String,
    /// 历史消息列表
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<Message>,
}

impl ConversationState {
    /// 创建新的对话状态
    pub fn new(conversation_id: impl Into<String>) -> Self {
        Self {
            agent_continuation_id: None,
            agent_task_type: None,
            chat_trigger_type: None,
            current_message: CurrentMessage::default(),
            conversation_id: conversation_id.into(),
            history: Vec::new(),
        }
    }

    /// 设置代理延续 ID
    pub fn with_agent_continuation_id(mut self, id: impl Into<String>) -> Self {
        self.agent_continuation_id = Some(id.into());
        self
    }

    /// 设置代理任务类型
    pub fn with_agent_task_type(mut self, task_type: impl Into<String>) -> Self {
        self.agent_task_type = Some(task_type.into());
        self
    }

    /// 设置聊天触发类型
    pub fn with_chat_trigger_type(mut self, trigger_type: impl Into<String>) -> Self {
        self.chat_trigger_type = Some(trigger_type.into());
        self
    }

    /// 设置当前消息
    pub fn with_current_message(mut self, message: CurrentMessage) -> Self {
        self.current_message = message;
        self
    }

    /// 添加历史消息
    pub fn with_history(mut self, history: Vec<Message>) -> Self {
        self.history = history;
        self
    }

    /// Whether any assistant history entry carries Kiro-native reasoning.
    pub fn has_history_reasoning_content(&self) -> bool {
        self.history.iter().any(|message| {
            matches!(
                message,
                Message::Assistant(assistant)
                    if assistant.assistant_response_message.reasoning_content.is_some()
            )
        })
    }

    /// Remove all Kiro-native reasoning blocks from assistant history.
    ///
    /// This is intentionally narrower than removing an assistant entry: visible
    /// content and tool uses remain byte-for-byte equivalent after serialization.
    pub fn clear_history_reasoning_content(&mut self) -> usize {
        self.history
            .iter_mut()
            .filter_map(|message| match message {
                Message::Assistant(assistant) => assistant
                    .assistant_response_message
                    .reasoning_content
                    .take(),
                Message::User(_) => None,
            })
            .count()
    }
}

/// 当前消息容器
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentMessage {
    /// 用户输入消息
    pub user_input_message: UserInputMessage,
}

impl CurrentMessage {
    /// 创建新的当前消息
    pub fn new(user_input_message: UserInputMessage) -> Self {
        Self { user_input_message }
    }
}

/// 用户输入消息
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputMessage {
    /// 用户输入消息上下文
    pub user_input_message_context: UserInputMessageContext,
    /// 消息内容
    pub content: String,
    /// 模型 ID
    pub model_id: String,
    /// 图片列表
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<KiroImage>,
    /// 消息来源（通常为 "AI_EDITOR"）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

impl UserInputMessage {
    /// 创建新的用户输入消息
    pub fn new(content: impl Into<String>, model_id: impl Into<String>) -> Self {
        Self {
            user_input_message_context: UserInputMessageContext::default(),
            content: content.into(),
            model_id: model_id.into(),
            images: Vec::new(),
            origin: Some("AI_EDITOR".to_string()),
        }
    }

    /// 设置消息上下文
    pub fn with_context(mut self, context: UserInputMessageContext) -> Self {
        self.user_input_message_context = context;
        self
    }

    /// 添加图片
    pub fn with_images(mut self, images: Vec<KiroImage>) -> Self {
        self.images = images;
        self
    }

    /// 设置来源
    pub fn with_origin(mut self, origin: impl Into<String>) -> Self {
        self.origin = Some(origin.into());
        self
    }
}

/// 用户输入消息上下文
///
/// 包含工具定义和工具执行结果
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputMessageContext {
    /// 工具执行结果列表
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_results: Vec<ToolResult>,
    /// 可用工具列表
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,
}

impl UserInputMessageContext {
    /// 创建新的消息上下文
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置工具列表
    pub fn with_tools(mut self, tools: Vec<Tool>) -> Self {
        self.tools = tools;
        self
    }

    /// 设置工具结果
    pub fn with_tool_results(mut self, results: Vec<ToolResult>) -> Self {
        self.tool_results = results;
        self
    }
}

/// Kiro 图片
///
/// API 中使用的图片格式
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KiroImage {
    /// 图片格式（"jpeg", "png", "gif", "webp"）
    pub format: String,
    /// 图片数据源
    pub source: KiroImageSource,
}

impl KiroImage {
    /// 从 base64 数据创建图片
    pub fn from_base64(format: impl Into<String>, data: impl Into<String>) -> Self {
        Self {
            format: format.into(),
            source: KiroImageSource::from_bytes(data),
        }
    }
}

/// Kiro 图片数据源
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KiroImageSource {
    /// base64 编码的图片数据
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<String>,
}

impl KiroImageSource {
    pub fn from_bytes(data: impl Into<String>) -> Self {
        Self {
            bytes: Some(data.into()),
        }
    }
}

/// 历史消息
///
/// 可以是用户消息或助手消息
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Message {
    /// 用户消息
    User(HistoryUserMessage),
    /// 助手消息
    Assistant(HistoryAssistantMessage),
}

/// 历史用户消息
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryUserMessage {
    /// 用户输入消息
    pub user_input_message: UserMessage,
}

impl HistoryUserMessage {
    /// 创建新的历史用户消息
    pub fn new(content: impl Into<String>, model_id: impl Into<String>) -> Self {
        Self {
            user_input_message: UserMessage::new(content, model_id),
        }
    }
}

/// 用户消息（历史记录中使用）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMessage {
    /// 消息内容
    pub content: String,
    /// 模型 ID
    pub model_id: String,
    /// 消息来源
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// 图片列表
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<KiroImage>,
    /// 用户输入消息上下文
    #[serde(default, skip_serializing_if = "is_default_context")]
    pub user_input_message_context: UserInputMessageContext,
}

fn is_default_context(ctx: &UserInputMessageContext) -> bool {
    ctx.tools.is_empty() && ctx.tool_results.is_empty()
}

impl UserMessage {
    /// 创建新的用户消息
    pub fn new(content: impl Into<String>, model_id: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            model_id: model_id.into(),
            origin: Some("AI_EDITOR".to_string()),
            images: Vec::new(),
            user_input_message_context: UserInputMessageContext::default(),
        }
    }

    /// 设置图片
    pub fn with_images(mut self, images: Vec<KiroImage>) -> Self {
        self.images = images;
        self
    }

    /// 设置上下文
    pub fn with_context(mut self, context: UserInputMessageContext) -> Self {
        self.user_input_message_context = context;
        self
    }
}

/// 历史助手消息
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryAssistantMessage {
    /// 助手响应消息
    pub assistant_response_message: AssistantMessage,
}

impl HistoryAssistantMessage {
    /// 创建新的历史助手消息
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            assistant_response_message: AssistantMessage::new(content),
        }
    }
}

/// 助手消息（历史记录中使用）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessage {
    /// 响应内容
    pub content: String,
    /// 工具使用列表
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_uses: Option<Vec<ToolUseEntry>>,
    /// Kiro-native signed or redacted reasoning history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<ReasoningContent>,
}

impl AssistantMessage {
    /// 创建新的助手消息
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            tool_uses: None,
            reasoning_content: None,
        }
    }

    /// 设置工具使用
    pub fn with_tool_uses(mut self, tool_uses: Vec<ToolUseEntry>) -> Self {
        self.tool_uses = Some(tool_uses);
        self
    }

    /// Set a single Kiro-native reasoning union value.
    pub fn with_reasoning_content(mut self, reasoning_content: ReasoningContent) -> Self {
        self.reasoning_content = Some(reasoning_content);
        self
    }
}

/// Kiro assistant history accepts exactly one native reasoning union member.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ReasoningContent {
    /// Signed reasoning text returned by the same model.
    ReasoningText(ReasoningTextContent),
    /// Opaque canonical-base64 reasoning content.
    RedactedContent(RedactedReasoningContent),
}

impl ReasoningContent {
    pub fn reasoning_text(text: impl Into<String>, signature: impl Into<String>) -> Self {
        Self::ReasoningText(ReasoningTextContent {
            reasoning_text: ReasoningText {
                text: text.into(),
                signature: signature.into(),
            },
        })
    }

    pub fn redacted_content(content: impl Into<String>) -> Self {
        Self::RedactedContent(RedactedReasoningContent {
            redacted_content: content.into(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReasoningTextContent {
    pub reasoning_text: ReasoningText,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReasoningText {
    pub text: String,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RedactedReasoningContent {
    pub redacted_content: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_conversation_state_new() {
        let state = ConversationState::new("conv-123")
            .with_agent_task_type("vibe")
            .with_chat_trigger_type("MANUAL");

        assert_eq!(state.conversation_id, "conv-123");
        assert_eq!(state.agent_task_type, Some("vibe".to_string()));
        assert_eq!(state.chat_trigger_type, Some("MANUAL".to_string()));
    }

    #[test]
    fn test_user_input_message() {
        let msg = UserInputMessage::new("Hello", "claude-3-5-sonnet").with_origin("AI_EDITOR");

        assert_eq!(msg.content, "Hello");
        assert_eq!(msg.model_id, "claude-3-5-sonnet");
        assert_eq!(msg.origin, Some("AI_EDITOR".to_string()));
    }

    #[test]
    fn test_history_serialize() {
        let history = vec![
            Message::User(HistoryUserMessage::new("Hello", "claude-3-5-sonnet")),
            Message::Assistant(HistoryAssistantMessage::new("Hi! How can I help you?")),
        ];

        let json = serde_json::to_string(&history).unwrap();
        assert!(json.contains("userInputMessage"));
        assert!(json.contains("assistantResponseMessage"));
    }

    #[test]
    fn test_conversation_state_serialize() {
        let state = ConversationState::new("conv-123")
            .with_agent_task_type("vibe")
            .with_current_message(CurrentMessage::new(UserInputMessage::new(
                "Hello",
                "claude-3-5-sonnet",
            )));

        let json = serde_json::to_string(&state).unwrap();
        assert!(json.contains("\"conversationId\":\"conv-123\""));
        assert!(json.contains("\"agentTaskType\":\"vibe\""));
        assert!(json.contains("\"content\":\"Hello\""));
    }

    #[test]
    fn test_image_source_serialize() {
        let msg = UserInputMessage::new("Analyze attachments", "claude-opus-4.7")
            .with_images(vec![KiroImage::from_base64("png", "aW1hZ2U=")]);

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"images\""));
        assert!(json.contains("\"bytes\":\"aW1hZ2U=\""));
    }

    #[test]
    fn assistant_reasoning_content_serializes_as_exact_kiro_union() {
        let signed = AssistantMessage::new("answer")
            .with_reasoning_content(ReasoningContent::reasoning_text("thought", "sig"));
        assert_eq!(
            serde_json::to_value(signed).unwrap(),
            serde_json::json!({
                "content": "answer",
                "reasoningContent": {
                    "reasoningText": {
                        "text": "thought",
                        "signature": "sig"
                    }
                }
            })
        );

        let redacted = AssistantMessage::new(" ")
            .with_reasoning_content(ReasoningContent::redacted_content("b3BhcXVl"));
        assert_eq!(
            serde_json::to_value(redacted).unwrap(),
            serde_json::json!({
                "content": " ",
                "reasoningContent": {"redactedContent": "b3BhcXVl"}
            })
        );
    }

    #[test]
    fn reasoning_content_deserialization_rejects_non_union_objects() {
        for invalid in [
            serde_json::json!({
                "reasoningText": {"text": "thought", "signature": "sig"},
                "redactedContent": "b3BhcXVl"
            }),
            serde_json::json!({"reasoningText": {"text": "thought"}}),
            serde_json::json!({"redactedContent": "b3BhcXVl", "extra": true}),
        ] {
            assert!(serde_json::from_value::<ReasoningContent>(invalid).is_err());
        }
    }

    #[test]
    fn conversation_state_clears_native_history_reasoning_only_for_five_rounds() {
        for round in 0..5 {
            let signed = AssistantMessage::new(format!("signed answer {round}"))
                .with_tool_uses(vec![ToolUseEntry::new("tool-1", "read")])
                .with_reasoning_content(ReasoningContent::reasoning_text(
                    format!("thought {round}"),
                    format!("sig-{round}"),
                ));
            let redacted = AssistantMessage::new(format!("redacted answer {round}"))
                .with_reasoning_content(ReasoningContent::redacted_content("b3BhcXVl"));
            let mut state = ConversationState::new(format!("conv-{round}")).with_history(vec![
                Message::User(HistoryUserMessage::new("question", "model")),
                Message::Assistant(HistoryAssistantMessage {
                    assistant_response_message: signed,
                }),
                Message::Assistant(HistoryAssistantMessage {
                    assistant_response_message: redacted,
                }),
                Message::Assistant(HistoryAssistantMessage::new("plain answer")),
            ]);

            assert!(state.has_history_reasoning_content(), "round {round}");
            assert_eq!(state.clear_history_reasoning_content(), 2, "round {round}");
            assert!(!state.has_history_reasoning_content(), "round {round}");
            assert_eq!(state.clear_history_reasoning_content(), 0, "round {round}");

            let serialized = serde_json::to_string(&state.history).unwrap();
            assert!(serialized.contains(&format!("signed answer {round}")));
            assert!(serialized.contains(&format!("redacted answer {round}")));
            assert!(serialized.contains("plain answer"));
            assert!(serialized.contains("tool-1"));
            assert!(!serialized.contains("reasoningContent"));
            assert!(!serialized.contains(&format!("thought {round}")));
            assert!(!serialized.contains(&format!("sig-{round}")));
        }
    }
}
