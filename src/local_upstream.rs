//! Compatibility facade for the current legacy local-upstream implementation.
//!
//! New Anthropic/account-runtime boundaries should import these account-neutral
//! names while the concrete implementation is still backed by the legacy local
//! provider module.

pub(crate) mod call_trace {
    #[cfg(test)]
    pub(crate) use crate::kiro::call_trace::SelectionFailureSummary;
    pub(crate) use crate::kiro::call_trace::summarize_attempts as summarize_local_upstream_attempts;
    pub(crate) use crate::kiro::call_trace::{AccountRejectReason, SelectionFailureStage};
    #[cfg(test)]
    pub(crate) type LocalUpstreamCallError = crate::kiro::call_trace::KiroCallError;
    pub(crate) type LocalUpstreamCallFailureKind = crate::kiro::call_trace::KiroCallFailureKind;
    pub(crate) type LocalUpstreamCredentialAttempt = crate::kiro::call_trace::KiroCredentialAttempt;
    pub(crate) type LocalAuxiliaryMcpAttributionSink =
        crate::kiro::call_trace::McpCallAttributionSink;
}

pub(crate) mod dispatch {
    pub(crate) type LocalUpstreamAcquireMode = crate::kiro::token_manager::AcquireMode;
    pub(crate) type LocalUpstreamRouteState = crate::kiro::token_manager::LocalPoolRouteState;
    pub(crate) type LocalUpstreamRouteStateKind =
        crate::kiro::token_manager::LocalPoolRouteStateKind;
}

pub(crate) mod endpoint {
    pub(crate) use crate::kiro::endpoint::KiroEndpoint as LocalUpstreamEndpointTrait;
    pub(crate) use crate::kiro::endpoint::ide::IDE_ENDPOINT_NAME as LOCAL_UPSTREAM_IDE_ENDPOINT_NAME;
    pub(crate) type LocalUpstreamCliEndpoint = crate::kiro::endpoint::CliEndpoint;
    pub(crate) type LocalUpstreamEndpoint = dyn LocalUpstreamEndpointTrait;
    pub(crate) type LocalUpstreamIdeEndpoint = crate::kiro::endpoint::IdeEndpoint;
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
    pub(crate) type LocalUpstreamCodeEvent = crate::kiro::model::events::CodeEvent;
    #[cfg(test)]
    pub(crate) type LocalUpstreamContextUsageEvent = crate::kiro::model::events::ContextUsageEvent;
    pub(crate) type LocalUpstreamEvent = crate::kiro::model::events::Event;
    #[cfg(test)]
    pub(crate) type LocalUpstreamInvalidStateEvent = crate::kiro::model::events::InvalidStateEvent;
    #[cfg(test)]
    pub(crate) type LocalUpstreamMessageMetadataEvent =
        crate::kiro::model::events::MessageMetadataEvent;
    #[cfg(test)]
    pub(crate) type LocalUpstreamMetadataEvent = crate::kiro::model::events::MetadataEvent;
    pub(crate) type LocalUpstreamMetadataTokenUsage =
        crate::kiro::model::events::MetadataTokenUsage;
    #[cfg(test)]
    pub(crate) type LocalUpstreamMeteringEvent = crate::kiro::model::events::MeteringEvent;
    pub(crate) type LocalUpstreamReasoningContentEvent =
        crate::kiro::model::events::ReasoningContentEvent;
    pub(crate) type LocalUpstreamToolUseEvent = crate::kiro::model::events::ToolUseEvent;
}

pub(crate) mod model_catalog {
    pub(crate) type LocalUpstreamAvailableModel =
        crate::kiro::model::available_models::KiroAvailableModel;
    pub(crate) type LocalUpstreamAvailableModelCatalog =
        crate::kiro::model::available_models::KiroAvailableModelCatalog;
    pub(crate) type LocalUpstreamModelCapabilityCohortKey =
        crate::kiro::model::available_models::KiroModelCapabilityCohortKey;
    #[cfg(test)]
    pub(crate) type LocalUpstreamModelTokenLimits =
        crate::kiro::model::available_models::KiroModelTokenLimits;
}

pub(crate) mod request {
    pub(crate) type LocalUpstreamAdditionalModelRequestFields =
        crate::kiro::model::requests::kiro::AdditionalModelRequestFields;
    pub(crate) type LocalUpstreamAssistantMessage =
        crate::kiro::model::requests::conversation::AssistantMessage;
    pub(crate) type LocalUpstreamConversationMessage =
        crate::kiro::model::requests::conversation::Message;
    pub(crate) type LocalUpstreamConversationState =
        crate::kiro::model::requests::conversation::ConversationState;
    pub(crate) type LocalUpstreamCurrentMessage =
        crate::kiro::model::requests::conversation::CurrentMessage;
    pub(crate) type LocalUpstreamHistoryAssistantMessage =
        crate::kiro::model::requests::conversation::HistoryAssistantMessage;
    pub(crate) type LocalUpstreamHistoryUserMessage =
        crate::kiro::model::requests::conversation::HistoryUserMessage;
    pub(crate) type LocalUpstreamImage = crate::kiro::model::requests::conversation::KiroImage;
    pub(crate) type LocalUpstreamInputSchema = crate::kiro::model::requests::tool::InputSchema;
    pub(crate) type LocalUpstreamOutputConfig =
        crate::kiro::model::requests::kiro::KiroOutputConfig;
    pub(crate) type LocalUpstreamReasoningConfig =
        crate::kiro::model::requests::kiro::KiroReasoningConfig;
    pub(crate) type LocalUpstreamReasoningContent =
        crate::kiro::model::requests::conversation::ReasoningContent;
    pub(crate) type LocalUpstreamRequest = crate::kiro::model::requests::kiro::KiroRequest;
    pub(crate) type LocalUpstreamThinkingConfig =
        crate::kiro::model::requests::kiro::KiroThinkingConfig;
    pub(crate) type LocalUpstreamTool = crate::kiro::model::requests::tool::Tool;
    pub(crate) type LocalUpstreamToolResult = crate::kiro::model::requests::tool::ToolResult;
    pub(crate) type LocalUpstreamToolSpecification =
        crate::kiro::model::requests::tool::ToolSpecification;
    pub(crate) type LocalUpstreamToolUseEntry = crate::kiro::model::requests::tool::ToolUseEntry;
    pub(crate) type LocalUpstreamUserInputMessageContext =
        crate::kiro::model::requests::conversation::UserInputMessageContext;
    pub(crate) type LocalUpstreamUserInputMessage =
        crate::kiro::model::requests::conversation::UserInputMessage;
    pub(crate) type LocalUpstreamUserMessage =
        crate::kiro::model::requests::conversation::UserMessage;
}

pub(crate) mod stream {
    pub(crate) type LocalUpstreamEventStreamDecoder =
        crate::kiro::parser::decoder::EventStreamDecoder;
}
