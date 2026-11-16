use super::{
    payload_guard::{
        PayloadGuardConfig, PayloadGuardError, PayloadGuardReport,
        guard_anthropic_messages_request_reusing_body,
    },
    types::MessagesRequest,
};
use bytes::Bytes;
pub(crate) struct PreparedExternalMessagesPayload {
    pub payload: MessagesRequest,
    pub report: Option<PayloadGuardReport>,
    pub guard_applied: bool,
}

pub(crate) fn prepare_external_messages_payload(
    payload: &MessagesRequest,
    raw_body: &Bytes,
    external_guard_enabled: bool,
    config: PayloadGuardConfig,
) -> Result<PreparedExternalMessagesPayload, PayloadGuardError> {
    if !external_guard_enabled || !config.enabled {
        return Ok(PreparedExternalMessagesPayload {
            payload: payload.clone(),
            report: None,
            guard_applied: false,
        });
    }

    let mut prepared_payload = payload.clone();
    let (_, report) =
        guard_anthropic_messages_request_reusing_body(&mut prepared_payload, config, raw_body)?;

    Ok(PreparedExternalMessagesPayload {
        payload: prepared_payload,
        report: Some(report),
        guard_applied: true,
    })
}
