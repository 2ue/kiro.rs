//! Compatibility facade for the current legacy local-upstream implementation.
//!
//! New Anthropic/account-runtime boundaries should import these account-neutral
//! names while the concrete implementation is still backed by the legacy local
//! provider module.

pub(crate) mod call_trace {
    #[cfg(test)]
    pub(crate) use crate::local_upstream_impl::call_trace::SelectionFailureSummary;
    pub(crate) use crate::local_upstream_impl::call_trace::summarize_attempts as summarize_local_upstream_attempts;
    pub(crate) use crate::local_upstream_impl::call_trace::{
        AccountRejectReason, SelectionFailureStage,
    };
    #[cfg(test)]
    pub(crate) type LocalUpstreamCallError =
        crate::local_upstream_impl::call_trace::LocalUpstreamCallError;
    pub(crate) type LocalUpstreamCallFailureKind =
        crate::local_upstream_impl::call_trace::LocalUpstreamCallFailureKind;
    pub(crate) type LocalUpstreamCredentialAttempt =
        crate::local_upstream_impl::call_trace::LocalUpstreamCredentialAttempt;
    pub(crate) type LocalAuxiliaryMcpAttributionSink =
        crate::local_upstream_impl::call_trace::McpCallAttributionSink;
}

pub(crate) mod credentials {
    pub(crate) use crate::local_upstream_impl::model::credentials::split_local_upstream_api_key_and_region;
    pub(crate) type LocalUpstreamCredentials =
        crate::local_upstream_impl::model::credentials::LocalUpstreamCredentials;
    pub(crate) type LocalUpstreamCredentialsConfig =
        crate::local_upstream_impl::model::credentials::CredentialsConfig;
}

pub(crate) mod dispatch {
    pub(crate) type LocalUpstreamAcquireMode =
        crate::local_upstream_impl::token_manager::AcquireMode;
    pub(crate) type LocalUpstreamRouteState =
        crate::local_upstream_impl::token_manager::AccountRouteState;
    pub(crate) type LocalUpstreamRouteStateKind =
        crate::local_upstream_impl::token_manager::AccountRouteStateKind;
}

pub(crate) mod endpoint {
    #[cfg(test)]
    pub(crate) use crate::local_upstream_impl::endpoint::DEFAULT_LOCAL_UPSTREAM_ENDPOINT_NAME as LOCAL_UPSTREAM_IDE_ENDPOINT_NAME;
    #[cfg(test)]
    pub(crate) use crate::local_upstream_impl::endpoint::IdeEndpoint as LocalUpstreamIdeEndpoint;
    #[cfg(test)]
    pub(crate) use crate::local_upstream_impl::endpoint::LocalUpstreamEndpoint as LocalUpstreamEndpointTrait;
    #[cfg(test)]
    pub(crate) type LocalUpstreamEndpoint = dyn LocalUpstreamEndpointTrait;
}

#[cfg(test)]
pub(crate) mod provider {
    pub(crate) type LocalAuxiliaryMcpAttribution =
        crate::local_upstream_impl::provider::McpCallAttribution;
    pub(crate) type LocalAuxiliaryMcpFailureKind =
        crate::local_upstream_impl::provider::McpCallFailureKind;
    pub(crate) type LocalUpstreamApiResponse =
        crate::local_upstream_impl::provider::LocalUpstreamApiResponse;
    pub(crate) type LocalUpstreamProvider =
        crate::local_upstream_impl::provider::LocalUpstreamProvider;
    pub(crate) type LocalUpstreamStreamCompletion =
        crate::local_upstream_impl::provider::LocalUpstreamStreamCompletion;
    pub(crate) type LocalUpstreamStreamResponse =
        crate::local_upstream_impl::provider::LocalUpstreamStreamResponse;
}

#[cfg(test)]
pub(crate) mod event {
    #[cfg(test)]
    pub(crate) type LocalUpstreamAssistantResponseEvent =
        crate::local_upstream_impl::model::events::AssistantResponseEvent;
    #[cfg(test)]
    pub(crate) type LocalUpstreamCodeEvent = crate::local_upstream_impl::model::events::CodeEvent;
    #[cfg(test)]
    pub(crate) type LocalUpstreamContextUsageEvent =
        crate::local_upstream_impl::model::events::ContextUsageEvent;
    pub(crate) type LocalUpstreamEvent = crate::local_upstream_impl::model::events::Event;
    #[cfg(test)]
    pub(crate) type LocalUpstreamInvalidStateEvent =
        crate::local_upstream_impl::model::events::InvalidStateEvent;
    #[cfg(test)]
    pub(crate) type LocalUpstreamMessageMetadataEvent =
        crate::local_upstream_impl::model::events::MessageMetadataEvent;
    #[cfg(test)]
    pub(crate) type LocalUpstreamMetadataEvent =
        crate::local_upstream_impl::model::events::MetadataEvent;
    pub(crate) type LocalUpstreamMetadataTokenUsage =
        crate::local_upstream_impl::model::events::MetadataTokenUsage;
    #[cfg(test)]
    pub(crate) type LocalUpstreamMeteringEvent =
        crate::local_upstream_impl::model::events::MeteringEvent;
    pub(crate) type LocalUpstreamReasoningContentEvent =
        crate::local_upstream_impl::model::events::ReasoningContentEvent;
    pub(crate) type LocalUpstreamToolUseEvent =
        crate::local_upstream_impl::model::events::ToolUseEvent;
}

pub(crate) mod model_catalog {
    #[cfg(test)]
    pub(crate) type LocalUpstreamAvailableModel =
        crate::local_upstream_impl::model::available_models::LocalUpstreamAvailableModel;
    #[cfg(test)]
    pub(crate) type LocalUpstreamAvailableModelCatalog =
        crate::local_upstream_impl::model::available_models::LocalUpstreamAvailableModelCatalog;
    pub(crate) type LocalUpstreamModelCapabilityCohortKey =
        crate::local_upstream_impl::model::available_models::LocalUpstreamModelCapabilityCohortKey;
    #[cfg(test)]
    pub(crate) type LocalUpstreamModelTokenLimits =
        crate::local_upstream_impl::model::available_models::LocalUpstreamModelTokenLimits;
}

pub(crate) mod manager {
    pub(crate) type LocalUpstreamCredentialAuthUpdate =
        crate::local_upstream_impl::token_manager::CredentialAuthUpdate;
    pub(crate) type LocalUpstreamCredentialBaseSnapshot =
        crate::local_upstream_impl::token_manager::CredentialBaseSnapshot;
    pub(crate) type LocalUpstreamCredentialEntrySnapshot =
        crate::local_upstream_impl::token_manager::CredentialEntrySnapshot;
    pub(crate) type LocalUpstreamCredentialManager =
        crate::local_upstream_impl::token_manager::MultiTokenManager;
}

pub(crate) mod request {
    pub(crate) type LocalUpstreamAdditionalModelRequestFields =
        crate::local_upstream_impl::model::requests::upstream::LocalUpstreamAdditionalModelRequestFields;
    pub(crate) type LocalUpstreamAssistantMessage =
        crate::local_upstream_impl::model::requests::conversation::AssistantMessage;
    pub(crate) type LocalUpstreamConversationMessage =
        crate::local_upstream_impl::model::requests::conversation::Message;
    pub(crate) type LocalUpstreamConversationState =
        crate::local_upstream_impl::model::requests::conversation::ConversationState;
    pub(crate) type LocalUpstreamCurrentMessage =
        crate::local_upstream_impl::model::requests::conversation::CurrentMessage;
    pub(crate) type LocalUpstreamHistoryAssistantMessage =
        crate::local_upstream_impl::model::requests::conversation::HistoryAssistantMessage;
    pub(crate) type LocalUpstreamHistoryUserMessage =
        crate::local_upstream_impl::model::requests::conversation::HistoryUserMessage;
    pub(crate) type LocalUpstreamImage =
        crate::local_upstream_impl::model::requests::conversation::LocalUpstreamImage;
    pub(crate) type LocalUpstreamInputSchema =
        crate::local_upstream_impl::model::requests::tool::InputSchema;
    pub(crate) type LocalUpstreamOutputConfig =
        crate::local_upstream_impl::model::requests::upstream::LocalUpstreamOutputConfig;
    pub(crate) type LocalUpstreamReasoningConfig =
        crate::local_upstream_impl::model::requests::upstream::LocalUpstreamReasoningConfig;
    pub(crate) type LocalUpstreamReasoningContent =
        crate::local_upstream_impl::model::requests::conversation::ReasoningContent;
    pub(crate) type LocalUpstreamRequest =
        crate::local_upstream_impl::model::requests::upstream::LocalUpstreamRequest;
    pub(crate) type LocalUpstreamThinkingConfig =
        crate::local_upstream_impl::model::requests::upstream::LocalUpstreamThinkingConfig;
    pub(crate) type LocalUpstreamTool = crate::local_upstream_impl::model::requests::tool::Tool;
    pub(crate) type LocalUpstreamToolResult =
        crate::local_upstream_impl::model::requests::tool::ToolResult;
    pub(crate) type LocalUpstreamToolSpecification =
        crate::local_upstream_impl::model::requests::tool::ToolSpecification;
    pub(crate) type LocalUpstreamToolUseEntry =
        crate::local_upstream_impl::model::requests::tool::ToolUseEntry;
    pub(crate) type LocalUpstreamUserInputMessageContext =
        crate::local_upstream_impl::model::requests::conversation::UserInputMessageContext;
    pub(crate) type LocalUpstreamUserInputMessage =
        crate::local_upstream_impl::model::requests::conversation::UserInputMessage;
    pub(crate) type LocalUpstreamUserMessage =
        crate::local_upstream_impl::model::requests::conversation::UserMessage;
}

#[cfg(test)]
pub(crate) mod stream {
    #[cfg(test)]
    pub(crate) use crate::local_upstream_impl::parser::crc::crc32 as local_upstream_eventstream_crc32;
    pub(crate) type LocalUpstreamEventStreamDecoder =
        crate::local_upstream_impl::parser::decoder::EventStreamDecoder;
}

pub(crate) mod storage_task {
    pub(crate) use crate::local_upstream_impl::token_manager::storage_task::{
        best_effort_storage_task_stats as local_upstream_storage_task_stats,
        drain_best_effort_storage_tasks as drain_local_upstream_storage_tasks,
        shutdown_best_effort_storage_tasks as shutdown_local_upstream_storage_tasks,
        spawn_critical_storage_task as spawn_local_upstream_critical_storage_task,
    };
}
