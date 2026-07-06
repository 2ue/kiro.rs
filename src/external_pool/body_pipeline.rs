use super::{model_pipeline, *};

pub(super) struct PreparedExternalRequest {
    pub(super) body: Bytes,
    pub(super) outbound_model: Option<String>,
}

pub(super) fn prepare_request(
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
) -> Result<PreparedExternalRequest, ExternalPoolError> {
    if pool.request_body_mode == ExternalPoolRequestBodyMode::RawPassthrough {
        return prepare_raw_request(route, pool);
    }

    let Some(payload) = route.payload.as_ref() else {
        return Err(ExternalPoolError {
            status: None,
            message: format!(
                "external pool #{} requires normalized request body but raw route has no parsed payload",
                pool.id
            ),
            retryable: false,
            auto_disable_reason: None,
            cooldown: Some((Duration::ZERO, "body_mode_mismatch".to_string())),
            response_body: None,
        });
    };

    let prepared_payload = prepare_normalized_payload(route, pool, payload)?;
    let payload = &prepared_payload.payload;
    if let Some(report) = prepared_payload.report.as_ref() {
        tracing::debug!(
            request_id = %route.request_id,
            pool_id = pool.id,
            guard_applied = prepared_payload.guard_applied,
            modified = report.was_modified(),
            original_bytes = report.original_bytes,
            final_bytes = report.final_bytes,
            "external pool normalized body payload guard applied"
        );
    }

    let mut value = match serde_json::to_value(payload) {
        Ok(value) => value,
        Err(_) => match serde_json::from_slice::<serde_json::Value>(&prepared_payload.raw_body) {
            Ok(value) => value,
            Err(_) => {
                return Ok(PreparedExternalRequest {
                    body: prepared_payload.raw_body,
                    outbound_model: None,
                });
            }
        },
    };

    let outbound_model = model_pipeline::outbound_model_for_value(route, pool, &value)?;
    if let Some(outbound_model) = outbound_model.as_deref() {
        if value.get("model").and_then(|model| model.as_str()) != Some(outbound_model) {
            value["model"] = serde_json::Value::String(outbound_model.to_string());
        }
    }
    super::normalize_external_pool_thinking_value(&mut value);

    let body = serde_json::to_vec(&value)
        .map(Bytes::from)
        .unwrap_or(prepared_payload.raw_body);
    Ok(PreparedExternalRequest {
        body,
        outbound_model,
    })
}

fn prepare_raw_request(
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
) -> Result<PreparedExternalRequest, ExternalPoolError> {
    match pool.raw_model_mode {
        ExternalPoolRawModelMode::None => Ok(PreparedExternalRequest {
            body: route.raw_body.clone(),
            outbound_model: None,
        }),
        ExternalPoolRawModelMode::ProbeOnly => {
            let probe = probe_raw_messages_body(&route.raw_body);
            let outbound_model =
                model_pipeline::outbound_model_for_raw(route, pool, probe.model.as_deref())?;
            Ok(PreparedExternalRequest {
                body: route.raw_body.clone(),
                outbound_model,
            })
        }
        ExternalPoolRawModelMode::RewriteTopLevel => {
            let probe = probe_raw_messages_body(&route.raw_body);
            let outbound_model =
                model_pipeline::outbound_model_for_raw(route, pool, probe.model.as_deref())?;
            let Some(outbound_model_value) = outbound_model.as_deref() else {
                return Ok(PreparedExternalRequest {
                    body: route.raw_body.clone(),
                    outbound_model,
                });
            };
            let body = rewrite_raw_top_level_model(&route.raw_body, outbound_model_value).map_err(
                |message| ExternalPoolError {
                    status: None,
                    message: format!(
                        "external pool #{} raw model rewrite failed: {}",
                        pool.id, message
                    ),
                    retryable: false,
                    auto_disable_reason: None,
                    cooldown: Some((Duration::ZERO, "model_rewrite_failed".to_string())),
                    response_body: None,
                },
            )?;
            Ok(PreparedExternalRequest {
                body,
                outbound_model,
            })
        }
    }
}

fn prepare_normalized_payload(
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
    payload: &MessagesRequest,
) -> Result<PreparedExternalMessagesPayload, ExternalPoolError> {
    let guard_config = route.payload_guard_initial_config;
    match prepare_external_messages_payload(
        payload,
        &route.raw_body,
        route.payload_guard_external_enabled,
        guard_config,
    ) {
        Ok(prepared) => Ok(prepared),
        Err(err) => {
            if matches!(err, PayloadGuardError::OversizedImage { .. }) {
                return Err(payload_guard_error(pool, err));
            }
            let mut payload = payload.clone();
            let sanitized = guard_config.shaping.enabled
                && sanitize_anthropic_messages_for_external_forwarding(
                    &mut payload,
                    guard_config.shaping,
                );
            let raw_body = if sanitized {
                serialize_external_messages_request_body(&payload)
            } else {
                route.raw_body.clone()
            };
            tracing::warn!(
                request_id = %route.request_id,
                pool_id = pool.id,
                error = %err,
                endpoint = route.endpoint,
                model = %payload.model,
                sanitized,
                "external pool normalized payload guard failed; forwarding safety-sanitized request body when possible"
            );
            Ok(PreparedExternalMessagesPayload {
                raw_body,
                payload,
                report: None,
                guard_applied: false,
            })
        }
    }
}

fn serialize_external_messages_request_body(payload: &MessagesRequest) -> Bytes {
    serde_json::to_vec(payload)
        .map(Bytes::from)
        .unwrap_or_default()
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
        response_body: None,
    }
}
