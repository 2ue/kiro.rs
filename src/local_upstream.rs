//! Compatibility facade for the current legacy local-upstream implementation.
//!
//! New Anthropic/account-runtime boundaries should import these account-neutral
//! names while the concrete implementation is still backed by the legacy local
//! provider module.

pub(crate) mod call_trace {
    #[cfg(test)]
    pub(crate) use crate::kiro::call_trace::SelectionFailureSummary;
    pub(crate) use crate::kiro::call_trace::{AccountRejectReason, SelectionFailureStage};
    #[cfg(test)]
    pub(crate) type LocalUpstreamCallError = crate::kiro::call_trace::KiroCallError;
    pub(crate) type LocalUpstreamCallFailureKind = crate::kiro::call_trace::KiroCallFailureKind;
    pub(crate) type LocalUpstreamCredentialAttempt = crate::kiro::call_trace::KiroCredentialAttempt;
    pub(crate) type LocalAuxiliaryMcpAttributionSink =
        crate::kiro::call_trace::McpCallAttributionSink;
}

pub(crate) mod provider {
    pub(crate) type LocalAuxiliaryMcpAttribution = crate::kiro::provider::McpCallAttribution;
    pub(crate) type LocalAuxiliaryMcpFailureKind = crate::kiro::provider::McpCallFailureKind;
    pub(crate) type LocalUpstreamApiResponse = crate::kiro::provider::KiroApiResponse;
    pub(crate) type LocalUpstreamProvider = crate::kiro::provider::KiroProvider;
    pub(crate) type LocalUpstreamStreamCompletion = crate::kiro::provider::KiroStreamCompletion;
    pub(crate) type LocalUpstreamStreamResponse = crate::kiro::provider::KiroStreamResponse;
}

pub(crate) mod event {
    #[cfg(test)]
    pub(crate) type LocalUpstreamAssistantResponseEvent =
        crate::kiro::model::events::AssistantResponseEvent;
    #[cfg(test)]
    pub(crate) type LocalUpstreamContextUsageEvent = crate::kiro::model::events::ContextUsageEvent;
    pub(crate) type LocalUpstreamEvent = crate::kiro::model::events::Event;
    #[cfg(test)]
    pub(crate) type LocalUpstreamMessageMetadataEvent =
        crate::kiro::model::events::MessageMetadataEvent;
    #[cfg(test)]
    pub(crate) type LocalUpstreamMetadataEvent = crate::kiro::model::events::MetadataEvent;
    pub(crate) type LocalUpstreamMetadataTokenUsage =
        crate::kiro::model::events::MetadataTokenUsage;
    pub(crate) type LocalUpstreamReasoningContentEvent =
        crate::kiro::model::events::ReasoningContentEvent;
    pub(crate) type LocalUpstreamToolUseEvent = crate::kiro::model::events::ToolUseEvent;
}

pub(crate) mod request {
    #[cfg(test)]
    pub(crate) type LocalUpstreamAssistantMessage =
        crate::kiro::model::requests::conversation::AssistantMessage;
    pub(crate) type LocalUpstreamConversationMessage =
        crate::kiro::model::requests::conversation::Message;
    #[cfg(test)]
    pub(crate) type LocalUpstreamConversationState =
        crate::kiro::model::requests::conversation::ConversationState;
    #[cfg(test)]
    pub(crate) type LocalUpstreamCurrentMessage =
        crate::kiro::model::requests::conversation::CurrentMessage;
    #[cfg(test)]
    pub(crate) type LocalUpstreamHistoryAssistantMessage =
        crate::kiro::model::requests::conversation::HistoryAssistantMessage;
    #[cfg(test)]
    pub(crate) type LocalUpstreamHistoryUserMessage =
        crate::kiro::model::requests::conversation::HistoryUserMessage;
    pub(crate) type LocalUpstreamRequest = crate::kiro::model::requests::kiro::KiroRequest;
    pub(crate) type LocalUpstreamToolResult = crate::kiro::model::requests::tool::ToolResult;
    pub(crate) type LocalUpstreamToolUseEntry = crate::kiro::model::requests::tool::ToolUseEntry;
    #[cfg(test)]
    pub(crate) type LocalUpstreamUserInputMessageContext =
        crate::kiro::model::requests::conversation::UserInputMessageContext;
    #[cfg(test)]
    pub(crate) type LocalUpstreamUserInputMessage =
        crate::kiro::model::requests::conversation::UserInputMessage;
}

pub(crate) mod stream {
    pub(crate) type LocalUpstreamEventStreamDecoder =
        crate::kiro::parser::decoder::EventStreamDecoder;
}
