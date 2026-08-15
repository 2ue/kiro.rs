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
