use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use parking_lot::Mutex;

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
    RiskCircuitOpen,
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

#[derive(Debug, Clone, Default)]
pub struct McpCallAttributionSnapshot {
    pub credential_id: Option<u64>,
    pub credential_label: Option<String>,
    pub attempts: Vec<KiroCredentialAttempt>,
}

#[derive(Debug, Clone, Default)]
pub struct McpCallAttributionSink {
    state: Arc<Mutex<McpCallAttributionSnapshot>>,
}

impl McpCallAttributionSink {
    pub fn begin_send(&self, attempt: usize, credential_id: u64, credential_label: &str) {
        let mut state = self.state.lock();
        state.credential_id = Some(credential_id);
        state.credential_label = Some(credential_label.to_string());
        let pending = KiroCredentialAttempt::new(
            attempt,
            credential_id,
            Some(credential_label.to_string()),
            None,
            "pending",
            None::<String>,
            None::<String>,
            0,
        );
        if let Some(existing) = state
            .attempts
            .iter_mut()
            .find(|existing| existing.attempt == pending.attempt)
        {
            *existing = pending;
        } else {
            state.attempts.push(pending);
            state.attempts.sort_by_key(|attempt| attempt.attempt);
        }
    }

    pub fn replace(
        &self,
        credential_id: Option<u64>,
        credential_label: Option<String>,
        attempts: Vec<KiroCredentialAttempt>,
    ) {
        *self.state.lock() = McpCallAttributionSnapshot {
            credential_id,
            credential_label,
            attempts,
        };
    }

    pub fn snapshot(&self) -> McpCallAttributionSnapshot {
        self.state.lock().clone()
    }

    pub fn snapshot_for_client_drop(&self) -> McpCallAttributionSnapshot {
        let mut state = self.state.lock();
        for attempt in &mut state.attempts {
            if attempt.action == "pending" {
                attempt.action = "fail".to_string();
                attempt.error_type = Some("client_dropped".to_string());
                attempt.error_message = Some("mcp_client_cancelled".to_string());
            }
        }
        state.clone()
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
    failure_kind: Option<KiroCallFailureKind>,
    error_metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KiroCallFailureKind {
    InferenceAttemptsExhausted,
    InferenceAttemptReservedForFallback,
    DownstreamCommitted,
    AuxiliaryAttemptsExhausted,
    AuxiliaryConcurrencySaturated,
    LocalPoolRiskCircuitOpen,
    ThinkingSignatureInvalid,
    ThinkingSignatureRetryFailed,
}

impl KiroCallError {
    pub fn new(message: impl Into<String>, attempts: Vec<KiroCredentialAttempt>) -> Self {
        Self {
            message: message.into(),
            attempts,
            selection_failure: None,
            failure_kind: None,
            error_metadata: None,
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

    pub fn with_failure_kind(mut self, failure_kind: KiroCallFailureKind) -> Self {
        self.failure_kind = Some(failure_kind);
        self
    }

    pub fn failure_kind(&self) -> Option<KiroCallFailureKind> {
        self.failure_kind
    }

    pub fn with_error_metadata(mut self, metadata: Option<serde_json::Value>) -> Self {
        self.error_metadata = metadata;
        self
    }

    pub fn error_metadata(&self) -> Option<&serde_json::Value> {
        self.error_metadata.as_ref()
    }
}

impl fmt::Display for KiroCallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for KiroCallError {}

#[cfg(test)]
mod tests {
    use super::McpCallAttributionSink;

    #[test]
    fn mcp_attribution_sink_finalizes_pending_send_on_client_drop_for_five_rounds() {
        for round in 1..=5 {
            let sink = McpCallAttributionSink::default();
            sink.begin_send(0, round, &format!("credential-{round}"));

            let pending = sink.snapshot();
            assert_eq!(pending.credential_id, Some(round));
            assert_eq!(pending.attempts.len(), 1);
            assert_eq!(pending.attempts[0].action, "pending");

            let dropped = sink.snapshot_for_client_drop();
            assert_eq!(dropped.attempts.len(), 1);
            assert_eq!(dropped.attempts[0].attempt, 1);
            assert_eq!(dropped.attempts[0].credential_id, round);
            assert_eq!(dropped.attempts[0].action, "fail");
            assert_eq!(
                dropped.attempts[0].error_type.as_deref(),
                Some("client_dropped")
            );
            assert_eq!(
                dropped.attempts[0].error_message.as_deref(),
                Some("mcp_client_cancelled")
            );
        }
    }
}
