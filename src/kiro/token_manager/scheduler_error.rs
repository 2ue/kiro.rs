use std::fmt;

use anyhow::Error;

use crate::kiro::call_trace::KiroCallError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalSchedulerErrorKind {
    AllDisabled,
    NoModelCompatible,
    ProxyBlocked,
    AllCoolingDown,
    RequestExcludedAll,
    CapacityUnavailable,
    QueueFull,
    QueueTimeout,
    TokenUnavailable,
    RiskCircuitOpen,
}

#[derive(Debug, Clone)]
pub struct LocalSchedulerError {
    kind: LocalSchedulerErrorKind,
    message: String,
}

impl LocalSchedulerError {
    pub fn new(kind: LocalSchedulerErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn kind(&self) -> LocalSchedulerErrorKind {
        self.kind
    }
}

impl fmt::Display for LocalSchedulerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for LocalSchedulerError {}

pub fn local_scheduler_error_kind(err: &Error) -> Option<LocalSchedulerErrorKind> {
    if let Some(error) = err.downcast_ref::<LocalSchedulerError>() {
        return Some(error.kind());
    }
    if let Some(error) = err.downcast_ref::<KiroCallError>()
        && let Some(kind) = error.scheduler_error_kind()
    {
        return Some(kind);
    }
    for cause in err.chain() {
        if let Some(error) = cause.downcast_ref::<LocalSchedulerError>() {
            return Some(error.kind());
        }
        if let Some(error) = cause.downcast_ref::<KiroCallError>()
            && let Some(kind) = error.scheduler_error_kind()
        {
            return Some(kind);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{LocalSchedulerError, LocalSchedulerErrorKind, local_scheduler_error_kind};

    fn kinds() -> [LocalSchedulerErrorKind; 10] {
        [
            LocalSchedulerErrorKind::AllDisabled,
            LocalSchedulerErrorKind::NoModelCompatible,
            LocalSchedulerErrorKind::ProxyBlocked,
            LocalSchedulerErrorKind::AllCoolingDown,
            LocalSchedulerErrorKind::RequestExcludedAll,
            LocalSchedulerErrorKind::CapacityUnavailable,
            LocalSchedulerErrorKind::QueueFull,
            LocalSchedulerErrorKind::QueueTimeout,
            LocalSchedulerErrorKind::TokenUnavailable,
            LocalSchedulerErrorKind::RiskCircuitOpen,
        ]
    }

    #[test]
    fn display_preserves_message_for_every_kind() {
        for kind in kinds() {
            let message = format!("message for {kind:?}");
            let error = LocalSchedulerError::new(kind, message.clone());
            assert_eq!(error.to_string(), message);
            assert_eq!(error.kind(), kind);
        }
    }

    #[test]
    fn kind_is_found_through_direct_context_and_kiro_call_error() {
        let kind = LocalSchedulerErrorKind::QueueTimeout;
        let direct: anyhow::Error = LocalSchedulerError::new(kind, "direct").into();
        assert_eq!(local_scheduler_error_kind(&direct), Some(kind));

        let contextual: anyhow::Error = anyhow::Context::context(
            Err::<(), _>(LocalSchedulerError::new(kind, "contextual")),
            "x",
        )
        .expect_err("contextual error");
        assert_eq!(local_scheduler_error_kind(&contextual), Some(kind));

        let wrapped: anyhow::Error =
            crate::kiro::call_trace::KiroCallError::new("wrapped", Vec::new())
                .with_scheduler_error_kind(kind)
                .into();
        assert_eq!(local_scheduler_error_kind(&wrapped), Some(kind));
    }

    #[test]
    fn ordinary_anyhow_error_has_no_scheduler_kind() {
        let error = anyhow::anyhow!("ordinary");
        assert_eq!(local_scheduler_error_kind(&error), None);
    }
}
