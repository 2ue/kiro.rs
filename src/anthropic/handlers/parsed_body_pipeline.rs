use crate::anthropic::body_capabilities::{MultimodalStageKind, ParsedAnthropicBodyPlan};

use super::*;

#[derive(Debug, Default)]
pub(super) struct ParsedAnthropicBodyReport {
    pub(super) thinking_model_name_override: bool,
    pub(super) thinking_trigger_mode: bool,
    pub(super) thinking_trace: bool,
    pub(super) multimodal: Option<body_processing::BodyProcessingReport>,
}

pub(super) async fn prepare(
    state: &AppState,
    headers: &HeaderMap,
    endpoint: &str,
    payload: &mut MessagesRequest,
    runtime_config: &RequestRuntimeConfig,
    plan: ParsedAnthropicBodyPlan,
) -> Result<ParsedAnthropicBodyReport, Response> {
    tracing::trace!(
        endpoint,
        profile = plan.profile.as_str(),
        multimodal_enabled = plan.multimodal.is_enabled(),
        "preparing parsed Anthropic body with capability plan"
    );

    let mut report = ParsedAnthropicBodyReport::default();

    let prompt_additions_enabled = runtime_config.prompt_steering.enabled;

    if prompt_additions_enabled && plan.thinking.model_name_override.is_enabled() {
        if let Err(message) = override_thinking_from_model_name(payload) {
            let request_id = envelope::request_id();
            return Err(envelope::error_response_with_id(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                message,
                &request_id,
            ));
        }
        report.thinking_model_name_override = true;
    }
    if prompt_additions_enabled && plan.thinking.trigger_mode.is_enabled() {
        apply_thinking_trigger_mode(payload, runtime_config);
        report.thinking_trigger_mode = true;
    }
    if plan.thinking.trace.is_enabled() {
        log_thinking_request_trace(endpoint, payload, runtime_config);
        report.thinking_trace = true;
    }

    match plan.multimodal {
        MultimodalStageKind::Disabled => {}
        MultimodalStageKind::Configured(config) => {
            let caller_ua = headers
                .get(header::USER_AGENT)
                .and_then(|v| v.to_str().ok());
            let multimodal = body_processing::prepare_multimodal_sources(
                &state.file_store,
                payload,
                caller_ua,
                config,
            )
            .await
            .map_err(multimodal_preprocessing_error_response)?;
            report.multimodal = Some(multimodal);
        }
    }

    Ok(report)
}
