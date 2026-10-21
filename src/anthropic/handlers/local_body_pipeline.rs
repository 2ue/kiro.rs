use super::*;
use crate::anthropic::body_capabilities::{BodyStageState, LocalKiroBodyPlan};
use crate::anthropic::language_preference::{
    ClientLanguagePreference, detect_client_language_preference,
};
use crate::anthropic::payload_guard::sanitize_anthropic_messages_for_external_forwarding;
use crate::anthropic::prompt_steering::language_turn_reminder_applies;
use crate::anthropic::tool_schema_keys::ToolSchemaKeyMap;
use crate::anthropic::tool_use_policy::ResponseToolPolicy;
use crate::model::config::PayloadShapingConfig;

pub(super) struct PreparedLocalKiroBody {
    pub(super) request_body: String,
    pub(super) kiro_request: KiroRequest,
    pub(super) conversation_id: String,
    pub(super) input_tokens: i32,
    pub(super) payload_breakdown: Option<PayloadByteBreakdown>,
    pub(super) payload_guard_report: Option<PayloadGuardReport>,
    pub(super) payload_guard_elapsed: Option<Duration>,
    pub(super) thinking_enabled: bool,
    pub(super) tool_name_map: HashMap<String, String>,
    pub(super) tool_schema_key_map: ToolSchemaKeyMap,
    pub(super) known_tool_names: HashSet<String>,
    pub(super) response_tool_policy: ResponseToolPolicy,
    pub(super) warnings_header: Option<String>,
    pub(super) extract_xml_thinking: bool,
    pub(super) too_long_retry: Option<PayloadTooLongRetryRequest>,
    pub(super) cache_point_retry: Option<CachePointRetryRequest>,
}

pub(super) struct LocalBodyPrepareError {
    pub(super) response: Response,
    pub(super) error_metadata: serde_json::Value,
}

impl LocalBodyPrepareError {
    fn conversion(error: &ConversionError) -> Self {
        Self {
            response: conversion_error_response(error),
            error_metadata: serde_json::json!({
                "localBodyPrepareKind": "conversion_error",
                "localBodyPrepareCategory": conversion_error_category(error),
                "localBodyPrepareDiagnostic": bounded_diagnostic_text(conversion_error_public_message(error)),
            }),
        }
    }

    fn payload_guard(error: PayloadGuardError) -> Self {
        let kind = payload_guard_error_kind(&error);
        Self {
            response: payload_guard_error_response(error),
            error_metadata: serde_json::json!({
                "localBodyPrepareKind": "payload_guard_error",
                "localBodyPrepareCategory": kind,
                "localBodyPrepareDiagnostic": kind,
            }),
        }
    }
}

fn conversion_error_category(error: &ConversionError) -> &'static str {
    match error {
        ConversionError::UnsupportedModel(_) => "unsupported_model",
        ConversionError::EmptyMessages => "empty_messages",
        ConversionError::UnsupportedContent(message) => {
            if message.starts_with("unsupported image media_type:") {
                "unsupported_image_media_type"
            } else if message.starts_with("invalid image data for media_type:") {
                "invalid_image_data"
            } else if message.starts_with("Image data cannot be empty.") {
                "empty_image_data"
            } else if message.contains("image") {
                "image_content"
            } else {
                "unsupported_content"
            }
        }
    }
}

fn should_retry_local_conversion_after_reasoning_shaping(error: &ConversionError) -> bool {
    let ConversionError::UnsupportedContent(message) = error else {
        return false;
    };
    matches!(
        message.as_str(),
        "consecutive assistant messages contain multiple native reasoning blocks and cannot be merged losslessly"
            | "assistant history contains multiple or mixed native reasoning blocks; only one reasoning block per assistant message is supported"
            | "assistant history mixes native signed/redacted reasoning with unsigned thinking and cannot be represented losslessly"
    )
}

fn local_reasoning_fallback_shaping_config(
    mut shaping: PayloadShapingConfig,
) -> PayloadShapingConfig {
    shaping.enabled = true;
    shaping.discard_historical_thinking = true;
    shaping
}

fn conversion_error_public_message(error: &ConversionError) -> String {
    match error {
        ConversionError::UnsupportedModel(model) => {
            format!("The requested model is not available: {}", model)
        }
        ConversionError::EmptyMessages => "messages: at least one message is required".to_string(),
        ConversionError::UnsupportedContent(message) => message.clone(),
    }
}

fn payload_guard_error_kind(error: &PayloadGuardError) -> &'static str {
    match error {
        PayloadGuardError::Serialize(_) => "serialize",
        PayloadGuardError::OversizedImage { .. } => "oversized_image",
        PayloadGuardError::ToolPairingInvariant { .. } => "tool_pairing_invariant",
    }
}

fn bounded_diagnostic_text(value: String) -> String {
    const MAX_DIAGNOSTIC_CHARS: usize = 256;
    let mut sanitized = String::with_capacity(value.len().min(MAX_DIAGNOSTIC_CHARS));
    for (chars, ch) in value.chars().enumerate() {
        if chars >= MAX_DIAGNOSTIC_CHARS {
            sanitized.push_str("...");
            return sanitized;
        }
        if ch.is_control() {
            sanitized.push(' ');
        } else {
            sanitized.push(ch);
        }
    }
    sanitized
}

/// 超过该大小的请求体在转换、PDF 抽取和 payload guard 反复序列化上会占用可观的 CPU 时间。
/// 小请求保持原路径，避免 `block_in_place` 移交 worker 的额外开销。
const CPU_HEAVY_BODY_BYTES: usize = 256 * 1024;

/// 本地 body 准备是纯同步 CPU 工作；大请求或带 document 块（可能触发 PDF 抽取）的请求在
/// 多线程运行时里用 `block_in_place` 执行，让当前 worker 上排队的其他任务（含 SSE keepalive）
/// 迁移到其他线程继续运行。current_thread 运行时（以及无运行时）不支持 `block_in_place`，
/// 直接同步执行。
pub(super) fn run_cpu_heavy_in_place<T>(
    body_bytes: usize,
    has_documents: bool,
    work: impl FnOnce() -> T,
) -> T {
    if body_bytes < CPU_HEAVY_BODY_BYTES && !has_documents {
        return work();
    }
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(work)
        }
        _ => work(),
    }
}

/// 请求里是否带 document 块。PDF 抽取只发生在 document 块上，单个 PDF 抽取可能耗时数百毫秒，
/// 即使请求体不大也不应占用 tokio worker。
pub(super) fn payload_has_documents(payload: &MessagesRequest) -> bool {
    payload
        .messages
        .iter()
        .any(|message| match &message.content {
            serde_json::Value::Array(blocks) => blocks.iter().any(|block| {
                block.get("type").and_then(serde_json::Value::as_str) == Some("document")
            }),
            _ => false,
        })
}

/// Prompt steering for one local request with the identity settings resolved for its route, so
/// the converter's identity turn reminder only applies on routes identity masking is bound to.
fn prompt_steering_for_route(
    config: &crate::model::config::PromptSteeringConfig,
    endpoint: &str,
) -> crate::model::config::PromptSteeringConfig {
    let mut config = config.clone().normalized();
    config.identity = config.identity.for_route(endpoint);
    config
}

fn language_turn_reminder_for_payload(
    endpoint: &str,
    compat_profile: CompatProfile,
    config: &PromptSteeringConfig,
    payload: &MessagesRequest,
) -> Option<ClientLanguagePreference> {
    // count_tokens 使用本地 token 计数，不经过 converter 注入。
    language_turn_reminder_applies(endpoint, compat_profile, config)
        .then(|| detect_client_language_preference(payload))
}

fn language_reminder_log_value(preference: Option<&ClientLanguagePreference>) -> &'static str {
    match preference {
        Some(ClientLanguagePreference::Explicit(_)) => "explicit",
        Some(ClientLanguagePreference::Detected(_)) => "detected",
        Some(ClientLanguagePreference::FollowUser) => "follow_user",
        None => "off",
    }
}

pub(super) fn prepare(
    endpoint: &str,
    payload: &MessagesRequest,
    runtime_config: &RequestRuntimeConfig,
    cache_route: &ResolvedCacheRoutePolicy,
    model_resolution: &ModelResolution,
    native_reasoning_capability: KiroReasoningCapabilityState,
) -> Result<PreparedLocalKiroBody, LocalBodyPrepareError> {
    let plan = LocalKiroBodyPlan::compatible_with_config(
        runtime_config.initial_payload_guard_config(),
        runtime_config.body_conversion.clone(),
    );
    prepare_with_plan(
        endpoint,
        payload,
        runtime_config,
        cache_route,
        model_resolution,
        native_reasoning_capability,
        plan,
    )
}

pub(super) fn prepare_with_plan(
    endpoint: &str,
    payload: &MessagesRequest,
    runtime_config: &RequestRuntimeConfig,
    cache_route: &ResolvedCacheRoutePolicy,
    model_resolution: &ModelResolution,
    native_reasoning_capability: KiroReasoningCapabilityState,
    plan: LocalKiroBodyPlan,
) -> Result<PreparedLocalKiroBody, LocalBodyPrepareError> {
    debug_assert_eq!(plan.profile.as_str(), "local_credential");
    debug_assert_eq!(plan.conversion, BodyStageState::Enabled);
    // Structured output is served through an internal tool, so every later stage (conversion,
    // retries, token counting) sees the request that is actually sent to Kiro.
    let structured_output = crate::anthropic::structured_output::plan(payload);
    let payload = structured_output
        .as_ref()
        .map(|(request, _)| request)
        .unwrap_or(payload);
    let converter_prompt_cache_mode = prompt_cache_converter_mode_for_policy(&cache_route.policy);
    let discovered_reasoning_capability = matches!(
        &native_reasoning_capability,
        KiroReasoningCapabilityState::Supported(_)
    );
    // 语言识别必须基于进入转换前的完整 payload，不能使用后续裁剪后的副本。
    let language_turn_reminder = language_turn_reminder_for_payload(
        endpoint,
        runtime_config.compat_profile,
        &runtime_config.prompt_steering,
        payload,
    );
    let converter_options = ConverterOptions {
        compat_profile: runtime_config.compat_profile,
        conversion: plan.converter,
        prompt_cache_simulation_mode: converter_prompt_cache_mode,
        kiro_cache_point_enabled: cache_route.policy.cache_point.enabled,
        kiro_cache_point_tools_only: cache_route.policy.cache_point.tools_only,
        kiro_cache_point_record_plan: cache_route.policy.cache_point.record_plan,
        force_visible_thinking: should_force_visible_thinking(payload, runtime_config),
        explicit_reasoning_request: false,
        native_reasoning_capability,
        prompt_steering: prompt_steering_for_route(&runtime_config.prompt_steering, endpoint),
        language_turn_reminder: language_turn_reminder.clone(),
    };
    // Historical thinking is discarded before conversion for every request when the configured
    // shaping allows it, matching the external-pool path. The protected tool-continuation
    // assistant keeps its Kiro-native signed reasoning. Without this, the Kiro-layer discard only
    // runs inside the oversize branch, so first sends replay every historical signature and hit
    // avoidable THINKING_SIGNATURE_INVALID retries or single-union merge failures.
    let pre_conversion_shaping = plan.payload_guard.config.shaping;
    let mut pre_shaped_payload = None;
    if plan.payload_guard.config.enabled
        && pre_conversion_shaping.enabled
        && pre_conversion_shaping.discard_historical_thinking
    {
        let mut shaped_payload = payload.clone();
        if sanitize_anthropic_messages_for_external_forwarding(
            &mut shaped_payload,
            pre_conversion_shaping,
        ) {
            tracing::debug!(
                endpoint,
                model = %payload.model,
                upstream_model = ?model_resolution.upstream_model,
                "applied local pre-conversion historical thinking shaping"
            );
            pre_shaped_payload = Some(shaped_payload);
        }
    }
    let conversion_payload = pre_shaped_payload.as_ref().unwrap_or(payload);

    let conversion_result = match convert_request_with_resolved_model(
        conversion_payload,
        converter_options.clone(),
        model_resolution,
    ) {
        Ok(result) => result,
        Err(first_error)
            if pre_shaped_payload.is_none()
                && should_retry_local_conversion_after_reasoning_shaping(&first_error) =>
        {
            let mut shaped_payload = payload.clone();
            let fallback_shaping =
                local_reasoning_fallback_shaping_config(plan.payload_guard.config.shaping);
            if sanitize_anthropic_messages_for_external_forwarding(
                &mut shaped_payload,
                fallback_shaping,
            ) {
                tracing::info!(
                    endpoint,
                    model = %payload.model,
                    upstream_model = ?model_resolution.upstream_model,
                    "applied local conversion fallback Anthropic payload shaping"
                );
                match convert_request_with_resolved_model(
                    &shaped_payload,
                    converter_options,
                    model_resolution,
                ) {
                    Ok(result) => result,
                    Err(_) => return Err(LocalBodyPrepareError::conversion(&first_error)),
                }
            } else {
                return Err(LocalBodyPrepareError::conversion(&first_error));
            }
        }
        Err(error) => {
            return Err(LocalBodyPrepareError::conversion(&error));
        }
    };

    let mut kiro_request = KiroRequest {
        conversation_state: conversion_result.conversation_state,
        profile_arn: None,
        additional_model_request_fields: conversion_result.additional_model_request_fields,
        tool_cache_point_insert_after: conversion_result.tool_cache_point_insert_after.clone(),
        cache_point_plan_recording_enabled: conversion_result.cache_point_plan_recording_enabled,
    };
    let conversation_id = kiro_request.conversation_state.conversation_id.clone();

    let too_long_retry = if plan.retry_payloads.is_enabled() {
        PayloadTooLongRetryRequest::new(
            payload,
            &kiro_request,
            runtime_config,
            endpoint,
            model_resolution.upstream_model.as_deref(),
            &conversation_id,
            should_expose_proxy_warnings(runtime_config)
                .then(|| conversion_result.warnings.encode_header())
                .flatten(),
        )
    } else {
        None
    };
    let prepared_payload =
        match prepare_kiro_request_body(&mut kiro_request, plan.payload_guard.config) {
            Ok(result) => result,
            Err(err) => return Err(LocalBodyPrepareError::payload_guard(err)),
        };
    let request_body = prepared_payload.body;
    let payload_guard_report = prepared_payload.report;
    if let Some(report) = payload_guard_report.as_ref() {
        log_payload_guard_report(
            report,
            endpoint,
            &payload.model,
            model_resolution.upstream_model.as_deref(),
            Some(&conversation_id),
        );
    }
    let payload_breakdown = if plan.diagnostics.is_enabled() {
        payload_guard_report.as_ref().and_then(|report| {
            should_log_payload_byte_breakdown(report)
                .then(|| breakdown_kiro_request(&kiro_request, &request_body))
        })
    } else {
        None
    };
    if let Some(report) = payload_guard_report.as_ref() {
        log_payload_byte_breakdown(
            payload_breakdown,
            report,
            endpoint,
            &payload.model,
            model_resolution.upstream_model.as_deref(),
            Some(&conversation_id),
        );
    }
    log_kiro_conversion_summary(
        endpoint,
        payload,
        model_resolution,
        &kiro_request,
        request_body.len(),
        payload_guard_report.as_ref(),
        &conversion_result.warnings,
        discovered_reasoning_capability,
    );
    if model_resolution.is_remapped() {
        tracing::info!(
            endpoint,
            requested_model = %model_resolution.requested_model,
            upstream_model = ?model_resolution.upstream_model,
            resolution = %model_resolution.source.as_str(),
            note = ?model_resolution.note,
            conversation_id = %conversation_id,
            "Kiro upstream model mapping applied to request payload"
        );
    };

    tracing::debug!(
        endpoint = endpoint,
        requested_model = %payload.model,
        upstream_model = ?model_resolution.upstream_model,
        conversation_id = %conversation_id,
        request_bytes = request_body.len(),
        history_entries = payload_guard_report
            .as_ref()
            .map(|report| report.final_history_entries)
            .unwrap_or_else(|| kiro_request.conversation_state.history.len()),
        current_tool_count = kiro_request.conversation_state.current_message.user_input_message.user_input_message_context.tools.len(),
        current_tool_result_count = kiro_request.conversation_state.current_message.user_input_message.user_input_message_context.tool_results.len(),
        current_image_count = kiro_request.conversation_state.current_message.user_input_message.images.len(),
        language_reminder = language_reminder_log_value(language_turn_reminder.as_ref()),
        "Kiro request prepared"
    );
    let input_tokens = if plan.token_counting.is_enabled() {
        token::count_all_tokens(
            &payload.model,
            payload.system.as_deref(),
            &payload.messages,
            payload.tools.as_deref(),
        ) as i32
    } else {
        0
    };
    let thinking_enabled = should_expose_downstream_thinking(payload, &kiro_request);
    let warnings_header = if should_expose_proxy_warnings(runtime_config) {
        merge_warning_headers(
            conversion_result.warnings.encode_header(),
            payload_guard_report.as_ref(),
        )
    } else {
        None
    };
    let extract_xml_thinking = runtime_config.compat_profile.allows_unsigned_thinking();
    let cache_point_retry = if plan.retry_payloads.is_enabled() {
        CachePointRetryRequest::new(
            &kiro_request,
            endpoint,
            &payload.model,
            model_resolution.upstream_model.as_deref(),
            &conversation_id,
        )
    } else {
        None
    };

    Ok(PreparedLocalKiroBody {
        request_body,
        kiro_request,
        conversation_id,
        input_tokens,
        payload_breakdown,
        payload_guard_report,
        payload_guard_elapsed: prepared_payload.guard_elapsed,
        thinking_enabled,
        tool_name_map: conversion_result.tool_name_map,
        tool_schema_key_map: conversion_result.tool_schema_key_map,
        known_tool_names: conversion_result.known_tool_names,
        response_tool_policy: {
            let mut policy = conversion_result.response_tool_policy;
            if let Some((_, target)) = structured_output {
                policy.set_structured_output(target);
            }
            policy
        },
        warnings_header,
        extract_xml_thinking,
        too_long_retry,
        cache_point_retry,
    })
}

fn should_expose_downstream_thinking(
    payload: &MessagesRequest,
    kiro_request: &KiroRequest,
) -> bool {
    if payload
        .thinking
        .as_ref()
        .is_some_and(|thinking| thinking.thinking_type == "disabled")
    {
        return false;
    }
    payload
        .thinking
        .as_ref()
        .map(|t| t.is_enabled())
        .unwrap_or(false)
        || payload.output_config.is_some()
        || kiro_request
            .additional_model_request_fields
            .as_ref()
            .is_some_and(|fields| {
                fields.output_config.is_some()
                    || fields.reasoning.is_some()
                    || fields.thinking.is_some()
            })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::types::{Message as AnthropicMessage, OutputConfig, Thinking};
    use crate::kiro::model::requests::{
        conversation::{ConversationState, CurrentMessage, UserInputMessage},
        kiro::{AdditionalModelRequestFields, KiroOutputConfig, KiroThinkingConfig},
    };

    fn messages_request(
        thinking_type: Option<&str>,
        output_effort: Option<&str>,
    ) -> MessagesRequest {
        MessagesRequest {
            model: "claude-opus-4.8".to_string(),
            max_tokens: 64_000,
            messages: vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("hello"),
            }],
            stream: true,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: thinking_type.map(|thinking_type| Thinking {
                thinking_type: thinking_type.to_string(),
                budget_tokens: 0,
                display: None,
            }),
            output_config: output_effort.map(|effort| OutputConfig {
                effort: Some(effort.to_string()),
                format: None,
            }),
            output_format: None,
            metadata: None,
            stop_sequences: None,
        }
    }

    fn kiro_request_with_native_output_config() -> KiroRequest {
        KiroRequest {
            conversation_state: ConversationState::new("conv").with_current_message(
                CurrentMessage::new(UserInputMessage::new("hello", "claude-opus-4.8")),
            ),
            profile_arn: None,
            additional_model_request_fields: Some(AdditionalModelRequestFields {
                thinking: Some(KiroThinkingConfig {
                    thinking_type: "adaptive".to_string(),
                    display: None,
                }),
                output_config: Some(KiroOutputConfig {
                    effort: "max".to_string(),
                }),
                reasoning: None,
            }),
            tool_cache_point_insert_after: Vec::new(),
            cache_point_plan_recording_enabled: true,
        }
    }

    #[test]
    fn payload_document_detection_only_matches_document_blocks() {
        let mut payload = messages_request(None, None);
        assert!(!payload_has_documents(&payload));
        payload.messages[0].content = serde_json::json!([
            {"type": "text", "text": "summarize this"},
            {"type": "document", "source": {"type": "text", "media_type": "text/plain", "data": "x"}}
        ]);
        assert!(payload_has_documents(&payload));
        payload.messages[0].content = serde_json::json!("document as plain text");
        assert!(!payload_has_documents(&payload));
    }

    #[test]
    fn language_turn_reminder_uses_route_and_original_payload() {
        let mut config = PromptSteeringConfig::default();
        config.language_constraint.enabled = true;
        config.route_rules = vec!["/cc".to_string()];
        let payload = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!("# Language\nAlways respond in Korean."),
                },
                AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!("continue"),
                },
            ],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            output_format: None,
            metadata: None,
            stop_sequences: None,
        };

        let matched_preference = language_turn_reminder_for_payload(
            "/cc/v1/messages",
            CompatProfile::ClaudeCode,
            &config,
            &payload,
        );
        assert_eq!(
            matched_preference,
            Some(ClientLanguagePreference::Explicit("Korean".to_string()))
        );
        let matched_result = crate::anthropic::converter::convert_request_with_options(
            &payload,
            ConverterOptions {
                prompt_steering: config.clone(),
                language_turn_reminder: matched_preference,
                ..ConverterOptions::default()
            },
        )
        .unwrap();
        assert!(
            matched_result
                .conversation_state
                .current_message
                .user_input_message
                .content
                .contains("Visible reply language: Korean")
        );

        let unmatched_preference = language_turn_reminder_for_payload(
            "/v1/messages",
            CompatProfile::ClaudeCode,
            &config,
            &payload,
        );
        assert!(unmatched_preference.is_none());
        let unmatched_result = crate::anthropic::converter::convert_request_with_options(
            &payload,
            ConverterOptions {
                prompt_steering: config,
                language_turn_reminder: unmatched_preference,
                ..ConverterOptions::default()
            },
        )
        .unwrap();
        assert!(
            !unmatched_result
                .conversation_state
                .current_message
                .user_input_message
                .content
                .contains("<language_reminder>")
        );
    }

    #[test]
    fn language_reminder_log_value_reports_only_preference_kind() {
        assert_eq!(
            language_reminder_log_value(Some(&ClientLanguagePreference::Explicit(
                "Korean".to_string()
            ))),
            "explicit"
        );
        assert_eq!(
            language_reminder_log_value(Some(&ClientLanguagePreference::Detected(
                "Chinese".to_string()
            ))),
            "detected"
        );
        assert_eq!(
            language_reminder_log_value(Some(&ClientLanguagePreference::FollowUser)),
            "follow_user"
        );
        assert_eq!(language_reminder_log_value(None), "off");
    }

    #[test]
    fn cpu_heavy_helper_runs_inline_without_a_multi_thread_runtime() {
        assert_eq!(run_cpu_heavy_in_place(CPU_HEAVY_BODY_BYTES, false, || 7), 7);
        assert_eq!(run_cpu_heavy_in_place(0, true, || 8), 8);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("current-thread runtime");
        let value = runtime
            .block_on(async { run_cpu_heavy_in_place(CPU_HEAVY_BODY_BYTES * 4, true, || 11) });
        assert_eq!(value, 11);
    }

    #[test]
    fn cpu_heavy_helper_uses_block_in_place_only_for_large_bodies_on_multi_thread_runtime() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .build()
            .expect("multi-thread runtime");
        let (small, large) = runtime.block_on(async {
            tokio::spawn(async {
                let small = run_cpu_heavy_in_place(CPU_HEAVY_BODY_BYTES - 1, false, || 1);
                let large = run_cpu_heavy_in_place(CPU_HEAVY_BODY_BYTES, false, || 2);
                let small_with_document = run_cpu_heavy_in_place(1, true, || 3);
                (small, large + small_with_document - 3)
            })
            .await
            .expect("task should complete")
        });
        assert_eq!((small, large), (1, 2));
    }

    #[test]
    fn disabled_thinking_suppresses_downstream_thinking_even_with_native_effort_for_five_rounds() {
        let kiro_request = kiro_request_with_native_output_config();
        for round in 0..5 {
            assert!(
                !should_expose_downstream_thinking(
                    &messages_request(Some("disabled"), Some("max")),
                    &kiro_request,
                ),
                "round {round}: client disabled thinking must control downstream visibility"
            );
        }
    }

    #[test]
    fn adaptive_or_omitted_thinking_with_output_effort_exposes_downstream_thinking_for_five_rounds()
    {
        let kiro_request = kiro_request_with_native_output_config();
        for round in 0..5 {
            assert!(
                should_expose_downstream_thinking(
                    &messages_request(Some("adaptive"), Some("max")),
                    &kiro_request,
                ),
                "round {round}: adaptive thinking remains visible"
            );
            assert!(
                should_expose_downstream_thinking(
                    &messages_request(None, Some("max")),
                    &kiro_request
                ),
                "round {round}: omitted thinking with explicit effort keeps prior output_config behavior"
            );
        }
    }
}
