use crate::anthropic::body_capabilities::{MultimodalStageKind, ParsedAnthropicBodyPlan};

use super::*;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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

    if plan.thinking.model_name_override.is_enabled() {
        override_thinking_from_model_name(payload);
        report.thinking_model_name_override = true;
    }
    if plan.thinking.trigger_mode.is_enabled() {
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
            .map_err(|message| {
                tracing::warn!("多模态 source 处理失败: {}", message);
                envelope::error_response(StatusCode::BAD_REQUEST, "invalid_request_error", message)
            })?;
            report.multimodal = Some(multimodal);
        }
    }

    Ok(report)
}
