//! 事件基础定义
//!
//! 定义事件类型枚举、trait 和统一事件结构

use crate::kiro::parser::error::{ParseError, ParseResult};
use crate::kiro::parser::frame::Frame;

/// 事件类型枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventType {
    /// 助手响应事件
    AssistantResponse,
    /// 工具使用事件
    ToolUse,
    /// 原生 reasoning/thinking 事件
    ReasoningContent,
    /// 元数据事件（包含 token usage）
    Metadata,
    /// 计费事件
    Metering,
    /// 代码内容事件（Amazon Q CLI 风格流）
    Code,
    /// 上下文使用率事件
    ContextUsage,
    /// 消息元数据事件
    MessageMetadata,
    /// 无效会话/状态事件
    InvalidState,
    /// 未知事件类型
    Unknown,
}

impl EventType {
    /// 从事件类型字符串解析
    pub fn from_str(s: &str) -> Self {
        match s {
            "assistantResponseEvent" => Self::AssistantResponse,
            "toolUseEvent" => Self::ToolUse,
            "reasoningContentEvent" => Self::ReasoningContent,
            "metadataEvent" => Self::Metadata,
            "meteringEvent" => Self::Metering,
            "codeEvent" => Self::Code,
            "contextUsageEvent" => Self::ContextUsage,
            "messageMetadataEvent" => Self::MessageMetadata,
            "invalidStateEvent" => Self::InvalidState,
            _ => Self::Unknown,
        }
    }

    /// 转换为事件类型字符串
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::AssistantResponse => "assistantResponseEvent",
            Self::ToolUse => "toolUseEvent",
            Self::ReasoningContent => "reasoningContentEvent",
            Self::Metadata => "metadataEvent",
            Self::Metering => "meteringEvent",
            Self::Code => "codeEvent",
            Self::ContextUsage => "contextUsageEvent",
            Self::MessageMetadata => "messageMetadataEvent",
            Self::InvalidState => "invalidStateEvent",
            Self::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for EventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// 事件 payload trait
///
/// 所有具体事件类型都需要实现此 trait
pub trait EventPayload: Sized {
    /// 从帧解析事件负载
    fn from_frame(frame: &Frame) -> ParseResult<Self>;
}

/// 统一事件枚举
///
/// 封装所有可能的事件类型
#[derive(Debug, Clone)]
pub enum Event {
    /// 助手响应
    AssistantResponse(super::AssistantResponseEvent),
    /// 工具使用
    ToolUse(super::ToolUseEvent),
    /// 原生 reasoning/thinking 内容
    ReasoningContent(super::ReasoningContentEvent),
    /// 元数据
    Metadata(super::MetadataEvent),
    /// 计费
    Metering(super::MeteringEvent),
    /// 代码内容
    Code(super::CodeEvent),
    /// 上下文使用率
    ContextUsage(super::ContextUsageEvent),
    /// 消息元数据
    MessageMetadata(super::MessageMetadataEvent),
    /// 无效状态
    InvalidState(super::InvalidStateEvent),
    /// 未知事件 (保留原始帧数据)
    Unknown {},
    /// 服务端错误
    Error {
        /// 错误代码
        error_code: String,
        /// 错误消息
        error_message: String,
    },
    /// 服务端异常
    Exception {
        /// 异常类型
        exception_type: String,
        /// 异常消息
        message: String,
    },
}

impl Event {
    /// 从帧解析事件
    pub fn from_frame(frame: Frame) -> ParseResult<Self> {
        let message_type = frame.message_type().unwrap_or("event");

        match message_type {
            "event" => Self::parse_event(frame),
            "error" => Self::parse_error(frame),
            "exception" => Self::parse_exception(frame),
            other => Err(ParseError::InvalidMessageType(other.to_string())),
        }
    }

    /// 解析事件类型消息
    fn parse_event(frame: Frame) -> ParseResult<Self> {
        let event_type_str = frame.event_type().unwrap_or("unknown");
        let event_type = EventType::from_str(event_type_str);

        match event_type {
            EventType::AssistantResponse => {
                let payload = super::AssistantResponseEvent::from_frame(&frame)?;
                Ok(Self::AssistantResponse(payload))
            }
            EventType::ToolUse => {
                let payload = super::ToolUseEvent::from_frame(&frame)?;
                Ok(Self::ToolUse(payload))
            }
            EventType::ReasoningContent => {
                let payload = super::ReasoningContentEvent::from_frame(&frame)?;
                Ok(Self::ReasoningContent(payload))
            }
            EventType::Metadata => {
                let payload = super::MetadataEvent::from_frame(&frame)?;
                Ok(Self::Metadata(payload))
            }
            EventType::Metering => {
                let payload = super::MeteringEvent::from_frame(&frame)?;
                Ok(Self::Metering(payload))
            }
            EventType::Code => {
                let payload = super::CodeEvent::from_frame(&frame)?;
                Ok(Self::Code(payload))
            }
            EventType::ContextUsage => {
                let payload = super::ContextUsageEvent::from_frame(&frame)?;
                Ok(Self::ContextUsage(payload))
            }
            EventType::MessageMetadata => {
                let payload = super::MessageMetadataEvent::from_frame(&frame)?;
                Ok(Self::MessageMetadata(payload))
            }
            EventType::InvalidState => {
                let payload = super::InvalidStateEvent::from_frame(&frame)?;
                Ok(Self::InvalidState(payload))
            }
            EventType::Unknown => Ok(Self::Unknown {}),
        }
    }

    /// 解析错误类型消息
    fn parse_error(frame: Frame) -> ParseResult<Self> {
        let error_code = frame
            .headers
            .error_code()
            .unwrap_or("UnknownError")
            .to_string();
        let error_message = frame.payload_as_str();

        Ok(Self::Error {
            error_code,
            error_message,
        })
    }

    /// 解析异常类型消息
    fn parse_exception(frame: Frame) -> ParseResult<Self> {
        let exception_type = frame
            .headers
            .exception_type()
            .unwrap_or("UnknownException")
            .to_string();
        let message = frame.payload_as_str();

        Ok(Self::Exception {
            exception_type,
            message,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_type_from_str() {
        assert_eq!(
            EventType::from_str("assistantResponseEvent"),
            EventType::AssistantResponse
        );
        assert_eq!(EventType::from_str("toolUseEvent"), EventType::ToolUse);
        assert_eq!(EventType::from_str("meteringEvent"), EventType::Metering);
        assert_eq!(EventType::from_str("codeEvent"), EventType::Code);
        assert_eq!(
            EventType::from_str("contextUsageEvent"),
            EventType::ContextUsage
        );
        assert_eq!(
            EventType::from_str("reasoningContentEvent"),
            EventType::ReasoningContent
        );
        assert_eq!(EventType::from_str("metadataEvent"), EventType::Metadata);
        assert_eq!(
            EventType::from_str("messageMetadataEvent"),
            EventType::MessageMetadata
        );
        assert_eq!(
            EventType::from_str("invalidStateEvent"),
            EventType::InvalidState
        );
        assert_eq!(EventType::from_str("unknown_type"), EventType::Unknown);
    }

    #[test]
    fn test_event_type_as_str() {
        assert_eq!(
            EventType::AssistantResponse.as_str(),
            "assistantResponseEvent"
        );
        assert_eq!(EventType::ToolUse.as_str(), "toolUseEvent");
        assert_eq!(
            EventType::ReasoningContent.as_str(),
            "reasoningContentEvent"
        );
        assert_eq!(EventType::Metadata.as_str(), "metadataEvent");
        assert_eq!(EventType::Metering.as_str(), "meteringEvent");
        assert_eq!(EventType::Code.as_str(), "codeEvent");
        assert_eq!(EventType::MessageMetadata.as_str(), "messageMetadataEvent");
        assert_eq!(EventType::InvalidState.as_str(), "invalidStateEvent");
    }
}
