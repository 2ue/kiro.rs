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
    kiro::model::requests::{
        conversation::{Message, ReasoningContent},
        kiro::KiroRequest,
    },
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
    let removed_synthetic_reasoning = remove_synthetic_history_reasoning_content(request);
    if removed_synthetic_reasoning > 0 {
        tracing::debug!(
            removed_synthetic_reasoning,
            "removed proxy synthetic reasoning before Kiro serialization"
        );
    }
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

/// Synthetic thinking signatures are an Anthropic compatibility marker. They must never cross
/// the Kiro native reasoning boundary, where only provider-issued opaque signatures are valid.
/// Keep real Kiro signatures and redacted reasoning untouched.
fn remove_synthetic_history_reasoning_content(request: &mut KiroRequest) -> usize {
    request
        .conversation_state
        .history
        .iter_mut()
        .filter_map(|message| match message {
            Message::Assistant(assistant) => {
                Some(&mut assistant.assistant_response_message.reasoning_content)
            }
            Message::User(_) => None,
        })
        .filter(|reasoning_content| {
            matches!(
                reasoning_content.as_ref(),
                Some(ReasoningContent::ReasoningText(content))
                    if crate::anthropic::signature::is_synthetic_thinking_signature(
                        &content.reasoning_text.signature
                    )
            )
        })
        .map(|reasoning_content| {
            *reasoning_content = None;
        })
        .count()
}

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
            max_kiro_weight: max_bytes,
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
            output_format: None,
            metadata: None,
            stop_sequences: None,
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
    fn local_boundary_removes_synthetic_reasoning_but_preserves_native_signature() {
        use crate::anthropic::signature::synthetic_thinking_signature;
        use crate::kiro::model::requests::conversation::{
            AssistantMessage, HistoryAssistantMessage, ReasoningContent,
        };

        let mut request = kiro_request();
        let synthetic = synthetic_thinking_signature("compatibility thought");
        request.conversation_state.history = vec![
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: AssistantMessage::new("synthetic")
                    .with_reasoning_content(ReasoningContent::reasoning_text(
                        "compatibility thought",
                        synthetic,
                    )),
            }),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: AssistantMessage::new("native").with_reasoning_content(
                    ReasoningContent::reasoning_text(
                        "provider thought",
                        "provider-issued-opaque-signature",
                    ),
                ),
            }),
        ];

        let prepared =
            prepare_kiro_request_body(&mut request, guard_config(false, 1024)).expect("prepare");

        assert!(!prepared.body.contains("compatibility thought"));
        assert!(prepared.body.contains("provider-issued-opaque-signature"));
        assert_eq!(
            request
                .conversation_state
                .history
                .iter()
                .filter(|message| matches!(
                    message,
                    Message::Assistant(assistant)
                        if assistant.assistant_response_message.reasoning_content.is_some()
                ))
                .count(),
            1
        );
    }

    #[test]
    fn disabled_external_guard_reuses_raw_body_without_report() {
        let request = anthropic_request();
        let raw_body = Bytes::from(serde_json::to_vec(&request).expect("serialize"));

        let prepared =
            prepare_external_messages_payload(&request, &raw_body, true, guard_config(false, 1024))
                .expect("prepare");

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

        assert_eq!(prepared.payload.messages.len(), 1);
        assert!(prepared.report.is_none());
        assert!(!prepared.guard_applied);
    }
}
