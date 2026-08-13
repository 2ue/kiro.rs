use serde::{Deserialize, Serialize};

use super::account::AccountId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountAttemptAction {
    Selected,
    Sent,
    Success,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryEvidence {
    NotSent,
    SendStarted,
    RequestSent,
    ResponseHeadersReceived,
    ResponseBodyStarted,
    Completed,
    UnknownMayHaveExecuted,
}

impl DeliveryEvidence {
    pub fn may_have_executed(self) -> bool {
        !matches!(self, Self::NotSent)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpstreamErrorClass {
    InvalidRequest,
    Authentication,
    Permission,
    RateLimit,
    Quota,
    Network,
    Timeout,
    Server,
    Protocol,
    Cancelled,
    Unknown,
}

impl UpstreamErrorClass {
    pub fn is_retry_candidate(self) -> bool {
        matches!(
            self,
            Self::RateLimit | Self::Network | Self::Timeout | Self::Server | Self::Protocol
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountAttemptTrace {
    pub attempt: u32,
    pub account_id: AccountId,
    pub account_name: String,
    pub action: AccountAttemptAction,
    pub delivery_evidence: DeliveryEvidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_class: Option<UpstreamErrorClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    pub duration_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_not_sent_is_non_executed_evidence() {
        assert!(!DeliveryEvidence::NotSent.may_have_executed());
        assert!(DeliveryEvidence::SendStarted.may_have_executed());
        assert!(DeliveryEvidence::UnknownMayHaveExecuted.may_have_executed());
    }

    #[test]
    fn transient_error_classes_are_retry_candidates() {
        assert!(UpstreamErrorClass::RateLimit.is_retry_candidate());
        assert!(UpstreamErrorClass::Network.is_retry_candidate());
        assert!(UpstreamErrorClass::Timeout.is_retry_candidate());
        assert!(!UpstreamErrorClass::Authentication.is_retry_candidate());
        assert!(!UpstreamErrorClass::InvalidRequest.is_retry_candidate());
    }
}
