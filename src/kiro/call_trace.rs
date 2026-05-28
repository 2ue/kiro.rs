use serde::{Deserialize, Serialize};
use std::fmt;

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
}

impl KiroCallError {
    pub fn new(message: impl Into<String>, attempts: Vec<KiroCredentialAttempt>) -> Self {
        Self {
            message: message.into(),
            attempts,
        }
    }

    pub fn attempts(&self) -> &[KiroCredentialAttempt] {
        &self.attempts
    }
}

impl fmt::Display for KiroCallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for KiroCallError {}
