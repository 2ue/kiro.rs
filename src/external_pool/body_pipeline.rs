use super::{model_pipeline, *};
use crate::anthropic::body_capabilities::{
    ExternalBodyBytesPlan, ExternalBodyPlan, PayloadGuardStagePlan, RawModelStagePlan,
};
use std::collections::{HashMap, VecDeque};

pub(super) struct PreparedExternalRequest {
    pub(super) body: Bytes,
    pub(super) outbound_model: Option<String>,
}

pub(super) struct NormalizedRequestBase {
    body: Bytes,
    probe: RawMessagesBodyProbe,
    report: Option<PayloadGuardReport>,
    guard_applied: bool,
    guard_fallback_error: Option<PayloadGuardError>,
}

pub(super) fn prepare_request(
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
) -> Result<PreparedExternalRequest, ExternalPoolError> {
    let plan = plan_for_pool(route, pool);
    tracing::trace!(
        request_id = %route.request_id,
        pool_id = pool.id,
        profile = plan.profile.as_str(),
        usage_projection_enabled = plan.usage_projection.is_enabled(),
        "preparing external request body with capability plan"
    );

    let (payload_guard, model, thinking_normalization) = match plan.bytes {
        ExternalBodyBytesPlan::RawPassthrough { model } => {
            return prepare_raw_request(route, pool, model);
        }
        ExternalBodyBytesPlan::Normalized {
            payload_guard,
            model,
            thinking_normalization,
        } => (payload_guard, model, thinking_normalization),
    };

    let raw_projection_payload = if route.payload.is_none() {
        Some(external_route_raw_projection_payload(route).ok_or_else(|| ExternalPoolError {
            status: Some(StatusCode::BAD_REQUEST),
            message: format!(
                "external pool #{} requires normalized request body but raw route could not be parsed",
                pool.id
            ),
            retryable: false,
            auto_disable_reason: None,
            cooldown: None,
            protocol_error: None,
            raw_upstream_error: None,
        })?)
    } else {
        None
    };
    let payload = route
        .payload
        .as_ref()
        .or(raw_projection_payload.as_deref())
        .expect("raw projection payload is parsed when typed payload is missing");

    let normalized_base = route.preparation_cache.normalized_base.get_or_init(|| {
        route.preparation_cache.record_normalized_base_build();
        build_normalized_request_base(
            route,
            payload,
            payload_guard,
            thinking_normalization.is_enabled(),
        )
        .map(Arc::new)
    });
    let normalized_base = match normalized_base {
        Ok(base) => base,
        Err(error) => return Err(payload_guard_error(pool, error.clone())),
    };
    if let Some(report) = normalized_base.report.as_ref() {
        tracing::debug!(
            request_id = %route.request_id,
            pool_id = pool.id,
            guard_applied = normalized_base.guard_applied,
            modified = report.was_modified(),
            original_bytes = report.original_bytes,
            final_bytes = report.final_bytes,
            "external pool normalized body payload guard applied"
        );
    }
    if let Some(error) = normalized_base.guard_fallback_error.as_ref() {
        tracing::warn!(
            request_id = %route.request_id,
            pool_id = pool.id,
            error = %error,
            endpoint = route.endpoint,
            model = %payload.model,
            "external pool normalized payload guard failed; using the request-scoped safety-sanitized body"
        );
    }

    let outbound_model = if model.is_enabled() {
        model_pipeline::outbound_model_for_raw(route, pool, normalized_base.probe.model.as_deref())?
    } else {
        None
    };
    let body = match outbound_model.as_deref() {
        Some(outbound_model) => rewrite_raw_top_level_model_with_probe(
            &normalized_base.body,
            &normalized_base.probe,
            outbound_model,
        )
        .map_err(|message| ExternalPoolError {
            status: Some(StatusCode::INTERNAL_SERVER_ERROR),
            message: format!(
                "external pool #{} normalized model rewrite failed: {}",
                pool.id, message
            ),
            retryable: false,
            auto_disable_reason: None,
            cooldown: None,
            protocol_error: None,
            raw_upstream_error: None,
        })?,
        None => normalized_base.body.clone(),
    };
    Ok(PreparedExternalRequest {
        body,
        outbound_model,
    })
}

fn build_normalized_request_base(
    route: &ExternalRouteRequest,
    payload: &MessagesRequest,
    payload_guard: PayloadGuardStagePlan,
    normalize_thinking: bool,
) -> Result<NormalizedRequestBase, PayloadGuardError> {
    let (prepared_payload, guard_fallback_error) =
        prepare_normalized_payload(route, payload, payload_guard)?;
    if let Some(report) = prepared_payload.report.as_ref() {
        route
            .preparation_cache
            .record_payload_guard_serializations(report.guard_serializations);
    }
    let mut value = normalized_request_value(route, &prepared_payload.payload)
        .map_err(|error| PayloadGuardError::Serialize(error.to_string()))?;
    if normalize_thinking {
        super::normalize_external_pool_thinking_value(&mut value);
    }
    route
        .preparation_cache
        .record_normalized_json_serialization();
    let body = serde_json::to_vec(&value)
        .map(Bytes::from)
        .map_err(|error| PayloadGuardError::Serialize(error.to_string()))?;
    let probe = probe_raw_messages_body(&body);
    if let Some(error) = probe.scan_error() {
        return Err(PayloadGuardError::Serialize(error.to_string()));
    }
    Ok(NormalizedRequestBase {
        body,
        probe,
        report: prepared_payload.report,
        guard_applied: prepared_payload.guard_applied,
        guard_fallback_error,
    })
}

const NORMALIZED_TOP_LEVEL_FIELDS: &[&str] = &[
    "model",
    "max_tokens",
    "messages",
    "stream",
    "system",
    "tools",
    "tool_choice",
    "thinking",
    "output_config",
    "metadata",
];

fn normalized_request_value(
    route: &ExternalRouteRequest,
    payload: &MessagesRequest,
) -> Result<serde_json::Value, serde_json::Error> {
    route
        .preparation_cache
        .record_normalized_json_serialization();
    let typed = serde_json::to_value(payload)?;
    route
        .preparation_cache
        .record_normalized_original_value_parse();
    let Ok(mut original) = parse_json_value_unbounded(&route.effective_raw_body) else {
        return Ok(typed);
    };
    let (Some(original_object), Some(typed_object)) = (original.as_object_mut(), typed.as_object())
    else {
        return Ok(typed);
    };

    for field in NORMALIZED_TOP_LEVEL_FIELDS {
        let Some(typed_value) = typed_object.get(*field) else {
            original_object.remove(*field);
            continue;
        };
        let original_value = original_object.get(*field);
        let overlaid = match *field {
            "messages" => overlay_message_array(original_value, typed_value),
            "tools" => overlay_named_object_array(
                original_value,
                typed_value,
                "name",
                &[
                    "type",
                    "name",
                    "description",
                    "input_schema",
                    "max_uses",
                    "cache_control",
                ],
            ),
            "system" => overlay_named_object_array(
                original_value,
                typed_value,
                "text",
                &["text", "cache_control"],
            ),
            "metadata" => overlay_object_fields(original_value, typed_value, &["user_id"]),
            "thinking" => {
                overlay_object_fields(original_value, typed_value, &["type", "budget_tokens"])
            }
            "output_config" => overlay_object_fields(original_value, typed_value, &["effort"]),
            _ => typed_value.clone(),
        };
        original_object.insert((*field).to_string(), overlaid);
    }
    Ok(original)
}

fn parse_json_value_unbounded(bytes: &[u8]) -> Result<serde_json::Value, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    deserializer.disable_recursion_limit();
    let value = serde_json::Value::deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(value)
}

fn overlay_message_array(
    original: Option<&serde_json::Value>,
    typed: &serde_json::Value,
) -> serde_json::Value {
    let Some(typed_messages) = typed.as_array() else {
        return typed.clone();
    };
    let original_messages = original.and_then(serde_json::Value::as_array);
    let aligned = align_message_indices(original_messages, typed_messages);
    serde_json::Value::Array(
        typed_messages
            .iter()
            .enumerate()
            .map(|(index, typed_message)| {
                let original_message = original_messages
                    .and_then(|messages| aligned[index].and_then(|idx| messages.get(idx)));
                overlay_object_fields(original_message, typed_message, &["role", "content"])
            })
            .collect(),
    )
}

fn align_message_indices(
    original: Option<&Vec<serde_json::Value>>,
    typed: &[serde_json::Value],
) -> Vec<Option<usize>> {
    let Some(original) = original else {
        return vec![None; typed.len()];
    };
    if original.len() == typed.len() {
        return (0..typed.len()).map(Some).collect();
    }

    let mut aligned = vec![None; typed.len()];
    let mut cursor = original.len();
    for typed_index in (0..typed.len()).rev() {
        let typed_role = typed[typed_index]
            .get("role")
            .and_then(serde_json::Value::as_str);
        let matched = (0..cursor).rev().find(|original_index| {
            original[*original_index]
                .get("role")
                .and_then(serde_json::Value::as_str)
                == typed_role
        });
        if let Some(original_index) = matched {
            aligned[typed_index] = Some(original_index);
            cursor = original_index;
        } else {
            cursor = 0;
        }
    }
    aligned
}

fn overlay_named_object_array(
    original: Option<&serde_json::Value>,
    typed: &serde_json::Value,
    identity_field: &str,
    known_fields: &[&str],
) -> serde_json::Value {
    let Some(typed_items) = typed.as_array() else {
        return typed.clone();
    };
    let original_items = original.and_then(serde_json::Value::as_array);
    let mut originals_by_identity: HashMap<&str, VecDeque<&serde_json::Value>> = HashMap::new();
    if let Some(original_items) = original_items {
        for item in original_items {
            if let Some(identity) = item.get(identity_field).and_then(serde_json::Value::as_str) {
                originals_by_identity
                    .entry(identity)
                    .or_default()
                    .push_back(item);
            }
        }
    }
    serde_json::Value::Array(
        typed_items
            .iter()
            .enumerate()
            .map(|(index, typed_item)| {
                let identity = typed_item
                    .get(identity_field)
                    .and_then(serde_json::Value::as_str);
                let original_item = match identity {
                    Some(identity) => originals_by_identity
                        .get_mut(identity)
                        .and_then(VecDeque::pop_front),
                    None => original_items.and_then(|items| items.get(index)),
                };
                overlay_object_fields(original_item, typed_item, known_fields)
            })
            .collect(),
    )
}

fn overlay_object_fields(
    original: Option<&serde_json::Value>,
    typed: &serde_json::Value,
    known_fields: &[&str],
) -> serde_json::Value {
    let Some(typed_object) = typed.as_object() else {
        return typed.clone();
    };
    let mut merged = original
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    for field in known_fields {
        if let Some(value) = typed_object.get(*field) {
            merged.insert((*field).to_string(), value.clone());
        } else {
            merged.remove(*field);
        }
    }
    serde_json::Value::Object(merged)
}

fn plan_for_pool(route: &ExternalRouteRequest, pool: &ExternalPool) -> ExternalBodyPlan {
    match pool.request_body_mode {
        ExternalPoolRequestBodyMode::RawPassthrough => {
            ExternalBodyPlan::raw(raw_model_stage_plan(pool.raw_model_mode))
        }
        ExternalPoolRequestBodyMode::Normalized => {
            let mut guard_config = route.payload_guard_initial_config;
            guard_config.enabled = route.payload_guard_external_enabled && guard_config.enabled;
            ExternalBodyPlan::normalized(guard_config)
        }
    }
}

fn raw_model_stage_plan(mode: ExternalPoolRawModelMode) -> RawModelStagePlan {
    match mode {
        ExternalPoolRawModelMode::None => RawModelStagePlan::None,
        ExternalPoolRawModelMode::ProbeOnly => RawModelStagePlan::ProbeOnly,
        ExternalPoolRawModelMode::RewriteTopLevel => RawModelStagePlan::RewriteTopLevel,
    }
}

fn prepare_raw_request(
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
    model_stage: RawModelStagePlan,
) -> Result<PreparedExternalRequest, ExternalPoolError> {
    match model_stage {
        RawModelStagePlan::None => Ok(PreparedExternalRequest {
            body: route.effective_raw_body.clone(),
            outbound_model: None,
        }),
        RawModelStagePlan::ProbeOnly => {
            let probe = effective_raw_probe(route, pool)?;
            let outbound_model =
                model_pipeline::outbound_model_for_raw(route, pool, probe.model.as_deref())?;
            Ok(PreparedExternalRequest {
                body: route.effective_raw_body.clone(),
                outbound_model,
            })
        }
        RawModelStagePlan::RewriteTopLevel => {
            let probe = effective_raw_probe(route, pool)?;
            let outbound_model =
                model_pipeline::outbound_model_for_raw(route, pool, probe.model.as_deref())?;
            let Some(outbound_model_value) = outbound_model.as_deref() else {
                return Ok(PreparedExternalRequest {
                    body: route.effective_raw_body.clone(),
                    outbound_model,
                });
            };
            let body = rewrite_raw_top_level_model_with_probe(
                &route.effective_raw_body,
                probe,
                outbound_model_value,
            )
            .map_err(|message| ExternalPoolError {
                status: None,
                message: format!(
                    "external pool #{} raw model rewrite failed: {}",
                    pool.id, message
                ),
                retryable: false,
                auto_disable_reason: None,
                cooldown: Some((Duration::ZERO, "model_rewrite_failed".to_string())),
                protocol_error: None,
                raw_upstream_error: None,
            })?;
            Ok(PreparedExternalRequest {
                body,
                outbound_model,
            })
        }
    }
}

fn effective_raw_probe<'a>(
    route: &'a ExternalRouteRequest,
    pool: &ExternalPool,
) -> Result<&'a RawMessagesBodyProbe, ExternalPoolError> {
    let probe = route
        .effective_raw_probe
        .as_deref()
        .filter(|probe| probe.matches_body(&route.effective_raw_body))
        .ok_or_else(|| ExternalPoolError {
            status: Some(StatusCode::INTERNAL_SERVER_ERROR),
            message: format!(
                "external pool #{} raw request probe is missing or does not match the body snapshot",
                pool.id
            ),
            retryable: false,
            auto_disable_reason: None,
            cooldown: None,
            protocol_error: None,
            raw_upstream_error: None,
        })?;
    if let Some(error) = probe.scan_error() {
        return Err(ExternalPoolError {
            status: Some(StatusCode::BAD_REQUEST),
            message: error.to_string(),
            retryable: false,
            auto_disable_reason: None,
            cooldown: None,
            protocol_error: None,
            raw_upstream_error: None,
        });
    }
    Ok(probe)
}

fn prepare_normalized_payload(
    route: &ExternalRouteRequest,
    payload: &MessagesRequest,
    payload_guard: PayloadGuardStagePlan,
) -> Result<(PreparedExternalMessagesPayload, Option<PayloadGuardError>), PayloadGuardError> {
    match prepare_external_messages_payload(
        payload,
        &route.raw_body,
        payload_guard.state.is_enabled(),
        payload_guard.config,
    ) {
        Ok(prepared) => Ok((prepared, None)),
        Err(err) => {
            if matches!(err, PayloadGuardError::OversizedImage { .. }) {
                return Err(err);
            }
            let mut payload = payload.clone();
            if payload_guard.config.shaping.enabled {
                let _ = sanitize_anthropic_messages_for_external_forwarding(
                    &mut payload,
                    payload_guard.config.shaping,
                );
            }
            Ok((
                PreparedExternalMessagesPayload {
                    payload,
                    report: None,
                    guard_applied: false,
                },
                Some(err),
            ))
        }
    }
}

fn payload_guard_error(pool: &ExternalPool, err: PayloadGuardError) -> ExternalPoolError {
    let (status, message) = match err {
        PayloadGuardError::Serialize(message) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("external pool #{} payload guard serialize failed: {}", pool.id, message),
        ),
        PayloadGuardError::OversizedImage { .. } => (
            StatusCode::BAD_REQUEST,
            "One or more images exceed the upstream 5 MB image size limit. Remove or resize the oversized image and retry."
                .to_string(),
        ),
    };
    ExternalPoolError {
        status: Some(status),
        message,
        retryable: false,
        auto_disable_reason: None,
        cooldown: None,
        protocol_error: None,
        raw_upstream_error: None,
    }
}
