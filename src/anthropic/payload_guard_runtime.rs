use std::time::{Duration, Instant};

use bytes::Bytes;

use crate::{
    anthropic::{
        payload_guard::{
            PayloadGuardConfig, PayloadGuardError, PayloadGuardReport,
            guard_anthropic_messages_request_reusing_body, guard_kiro_request,
            serialize_kiro_request,
        },
        types::MessagesRequest,
    },
    kiro::model::requests::kiro::KiroRequest,
};

pub(crate) struct PreparedKiroRequestBody {
    pub body: String,
    pub report: Option<PayloadGuardReport>,
    pub guard_elapsed: Option<Duration>,
}

pub(crate) fn prepare_kiro_request_body(
    request: &mut KiroRequest,
    config: PayloadGuardConfig,
) -> Result<PreparedKiroRequestBody, PayloadGuardError> {
    if !config.enabled {
        return Ok(PreparedKiroRequestBody {
            body: serialize_kiro_request(request)?,
            report: None,
            guard_elapsed: None,
        });
    }

    let started_at = Instant::now();
    let (body, report) = guard_kiro_request(request, config)?;
    Ok(PreparedKiroRequestBody {
        body,
        report: Some(report),
        guard_elapsed: Some(started_at.elapsed()),
    })
}

pub(crate) struct PreparedExternalMessagesPayload {
    pub raw_body: Bytes,
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
            raw_body: raw_body.clone(),
            payload: payload.clone(),
            report: None,
            guard_applied: false,
        });
    }

    let mut prepared_payload = payload.clone();
    let (guarded_body, report) =
        guard_anthropic_messages_request_reusing_body(&mut prepared_payload, config, raw_body)?;
    let should_send_serialized = report.was_modified()
        || (config.max_bytes > 0
            && raw_body.len() > config.max_bytes
            && guarded_body.len() <= raw_body.len());
    let raw_body = if should_send_serialized {
        guarded_body
    } else {
        raw_body.clone()
    };

    Ok(PreparedExternalMessagesPayload {
        raw_body,
        payload: prepared_payload,
        report: Some(report),
        guard_applied: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        anthropic::types::Message as AnthropicMessage,
        kiro::model::requests::conversation::{
            ConversationState, CurrentMessage, HistoryUserMessage, UserInputMessage,
        },
        model::config::PayloadShapingConfig,
    };
    use serde_json::json;

    fn guard_config(enabled: bool, max_bytes: usize) -> PayloadGuardConfig {
        PayloadGuardConfig {
            enabled,
            max_bytes,
            trim_history: true,
            shaping: PayloadShapingConfig::default(),
        }
    }

    fn kiro_request() -> KiroRequest {
        KiroRequest {
            conversation_state: ConversationState::new("conv-runtime-test")
                .with_current_message(CurrentMessage::new(UserInputMessage::new(
                    "current",
                    "test-model",
                )))
                .with_history(vec![
                    crate::kiro::model::requests::conversation::Message::User(
                        HistoryUserMessage::new("old history", "test-model"),
                    ),
                ]),
            profile_arn: None,
            additional_model_request_fields: None,
            tool_cache_point_insert_after: Vec::new(),
            cache_point_plan_recording_enabled: true,
        }
    }

    fn anthropic_request() -> MessagesRequest {
        MessagesRequest {
            model: "claude-sonnet-4-20250514".to_string(),
            max_tokens: 128,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: json!([{"type":"text","text":"hello"}]),
            }],
            stream: true,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        }
    }

    #[test]
    fn disabled_local_guard_serializes_without_report() {
        let mut request = kiro_request();

        let prepared =
            prepare_kiro_request_body(&mut request, guard_config(false, 1024)).expect("prepare");

        assert!(prepared.body.contains("current"));
        assert!(prepared.report.is_none());
        assert!(prepared.guard_elapsed.is_none());
    }

    #[test]
    fn disabled_external_guard_reuses_raw_body_without_report() {
        let request = anthropic_request();
        let raw_body = Bytes::from(serde_json::to_vec(&request).expect("serialize"));

        let prepared =
            prepare_external_messages_payload(&request, &raw_body, true, guard_config(false, 1024))
                .expect("prepare");

        assert_eq!(prepared.raw_body, raw_body);
        assert_eq!(prepared.payload.messages.len(), 1);
        assert!(prepared.report.is_none());
        assert!(!prepared.guard_applied);
    }

    #[test]
    fn external_guard_flag_off_reuses_raw_body_without_report() {
        let request = anthropic_request();
        let raw_body = Bytes::from(serde_json::to_vec(&request).expect("serialize"));

        let prepared =
            prepare_external_messages_payload(&request, &raw_body, false, guard_config(true, 1024))
                .expect("prepare");

        assert_eq!(prepared.raw_body, raw_body);
        assert!(prepared.report.is_none());
        assert!(!prepared.guard_applied);
    }
}
