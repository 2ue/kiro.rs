use super::*;
use crate::anthropic::body_capabilities::{BodyStageState, LocalKiroBodyPlan};
use crate::anthropic::tool_schema_keys::ToolSchemaKeyMap;

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
    pub(super) warnings_header: Option<String>,
    pub(super) extract_xml_thinking: bool,
    pub(super) too_long_retry: Option<PayloadTooLongRetryRequest>,
    pub(super) cache_point_retry: Option<CachePointRetryRequest>,
}

pub(super) fn prepare(
    endpoint: &str,
    payload: &MessagesRequest,
    runtime_config: &RequestRuntimeConfig,
    cache_route: &ResolvedCacheRoutePolicy,
    model_resolution: &ModelResolution,
) -> Result<PreparedLocalKiroBody, Response> {
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
        plan,
    )
}

pub(super) fn prepare_with_plan(
    endpoint: &str,
    payload: &MessagesRequest,
    runtime_config: &RequestRuntimeConfig,
    cache_route: &ResolvedCacheRoutePolicy,
    model_resolution: &ModelResolution,
    plan: LocalKiroBodyPlan,
) -> Result<PreparedLocalKiroBody, Response> {
    debug_assert_eq!(plan.profile.as_str(), "local_credential");
    debug_assert_eq!(plan.conversion, BodyStageState::Enabled);
    let converter_prompt_cache_mode = prompt_cache_converter_mode_for_policy(&cache_route.policy);
    let conversion_result = match convert_request_with_resolved_model(
        payload,
        ConverterOptions {
            compat_profile: runtime_config.compat_profile,
            conversion: plan.converter,
            prompt_cache_simulation_mode: converter_prompt_cache_mode,
            kiro_cache_point_enabled: cache_route.policy.cache_point.enabled,
            kiro_cache_point_tools_only: cache_route.policy.cache_point.tools_only,
            kiro_cache_point_record_plan: cache_route.policy.cache_point.record_plan,
            force_visible_thinking: should_force_visible_thinking(payload, runtime_config),
        },
        model_resolution,
    ) {
        Ok(result) => result,
        Err(e) => {
            tracing::warn!("请求转换失败: {}", e);
            return Err(conversion_error_response(&e));
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
            &kiro_request,
            runtime_config,
            endpoint,
            &payload.model,
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
            Err(err) => return Err(payload_guard_error_response(err)),
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
        "Kiro request prepared"
    );
    tracing::trace!(
        endpoint = endpoint,
        requested_model = %payload.model,
        upstream_model = ?model_resolution.upstream_model,
        conversation_id = %conversation_id,
        request_body = %request_body,
        "Kiro request body"
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
    let thinking_enabled = payload
        .thinking
        .as_ref()
        .map(|t| t.is_enabled())
        .unwrap_or(false);
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
        warnings_header,
        extract_xml_thinking,
        too_long_retry,
        cache_point_retry,
    })
}
