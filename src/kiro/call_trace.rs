use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionFailureStage {
    RouteValidation,
    ModelEligibility,
    AccountEligibility,
    RpmLimit,
    AccountConcurrency,
    GlobalConcurrency,
    DispatchQueue,
    DispatchWait,
    Cooldown,
    StickyBinding,
    UpstreamPreflight,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountRejectReason {
    NoAccounts,
    Disabled,
    MissingAuth,
    ModelNotSupported,
    RouteNotAllowed,
    ProxyUnavailable,
    RpmLimited,
    AccountConcurrencyFull,
    GlobalConcurrencyFull,
    CooldownActive,
    HealthBlocked,
    StickyTargetUnavailable,
    RefreshInProgress,
    RefreshFailed,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RejectedAccountSample {
    pub account_id: u64,
    pub reason: AccountRejectReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpm_limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_flight: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_concurrent: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cooldown_remaining_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SelectionFailureSummary {
    pub request_id: String,
    pub route: String,
    pub model: String,
    pub stage: SelectionFailureStage,
    pub primary_reason: AccountRejectReason,
    pub rejected_account_count: usize,
    pub waitable_account_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
    pub reason_counts: BTreeMap<AccountRejectReason, usize>,
    pub sampled_accounts: Vec<RejectedAccountSample>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dispatch_wait_ms: Option<u64>,
    pub queue_depth: u32,
    pub global_in_flight: u32,
}

/// 单个下游请求在 Kiro provider 内部的一次凭据尝试。
///
/// 该结构只用于观测，不参与调度、计费或缓存计算。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct KiroCredentialAttempt {
    pub attempt: u32,
    pub credential_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_text: Option<String>,
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    pub duration_ms: u64,
}

impl KiroCredentialAttempt {
    pub fn new(
        attempt: usize,
        credential_id: u64,
        credential_label: Option<String>,
        status: Option<reqwest::StatusCode>,
        action: impl Into<String>,
        error_type: Option<impl Into<String>>,
        error_message: Option<impl Into<String>>,
        duration_ms: u64,
    ) -> Self {
        Self {
            attempt: attempt.saturating_add(1) as u32,
            credential_id,
            credential_label,
            status: status.map(|status| status.as_u16()),
            status_text: status.map(|status| status.to_string()),
            action: action.into(),
            model: None,
            error_type: error_type.map(Into::into),
            error_message: error_message.map(Into::into),
            duration_ms,
        }
    }

    pub fn with_model(mut self, model: Option<&str>) -> Self {
        self.model = model.map(str::to_string);
        self
    }

    fn compact(&self) -> String {
        let label = format!("#{}", self.credential_id);
        let outcome = self
            .status
            .map(|status| status.to_string())
            .or_else(|| self.error_type.clone())
            .unwrap_or_else(|| self.action.clone());
        format!("{}({})", label, outcome)
    }
}

pub fn summarize_attempts(attempts: &[KiroCredentialAttempt]) -> String {
    attempts
        .iter()
        .map(KiroCredentialAttempt::compact)
        .collect::<Vec<_>>()
        .join(">")
}

#[derive(Debug, Clone)]
pub struct KiroCallError {
    message: String,
    attempts: Vec<KiroCredentialAttempt>,
    selection_failure: Option<SelectionFailureSummary>,
}

impl KiroCallError {
    pub fn new(message: impl Into<String>, attempts: Vec<KiroCredentialAttempt>) -> Self {
        Self {
            message: message.into(),
            attempts,
            selection_failure: None,
        }
    }

    pub fn attempts(&self) -> &[KiroCredentialAttempt] {
        &self.attempts
    }

    pub fn with_selection_failure(mut self, summary: Option<SelectionFailureSummary>) -> Self {
        self.selection_failure = summary;
        self
    }

    pub fn selection_failure(&self) -> Option<&SelectionFailureSummary> {
        self.selection_failure.as_ref()
    }
}

impl fmt::Display for KiroCallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for KiroCallError {}
