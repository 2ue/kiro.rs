use super::{model_pipeline, *};

pub(super) struct PreparedAccountRequest {
    pub(super) body: Bytes,
    pub(super) outbound_model: Option<String>,
}

struct ResolvedRawRequestBody {
    body: Bytes,
    probe: RawMessagesBodyProbe,
}

#[allow(clippy::result_large_err)]
pub(super) fn prepare_request(
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
) -> Result<PreparedAccountRequest, ExternalPoolError> {
    tracing::trace!(
        request_id = %route.request_id,
        pool_id = pool.id,
        raw_model_mode = pool.raw_model_mode.as_str(),
        "preparing account request body as raw passthrough"
    );

    prepare_raw_request(route, pool)
}

#[allow(clippy::result_large_err)]
fn prepare_raw_request(
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
) -> Result<PreparedAccountRequest, ExternalPoolError> {
    let ResolvedRawRequestBody { body, probe } = resolve_raw_request_body(route, pool)?;
    if pool.raw_model_mode == ExternalPoolRawModelMode::None {
        return Ok(PreparedAccountRequest {
            body,
            outbound_model: None,
        });
    }

    let outbound_model =
        model_pipeline::account_outbound_model_for_raw(route, pool, probe.model.as_deref())?;
    if pool.raw_model_mode == ExternalPoolRawModelMode::ProbeOnly {
        return Ok(PreparedAccountRequest {
            body,
            outbound_model,
        });
    }

    let body = if let (Some(raw_model), Some(outbound_model_value)) =
        (probe.model.as_deref(), outbound_model.as_deref())
    {
        if raw_model == outbound_model_value {
            body
        } else {
            rewrite_raw_top_level_model_with_probe(&body, &probe, outbound_model_value).map_err(
                |message| ExternalPoolError {
                    status: None,
                    message: format!("account #{} raw model rewrite failed: {}", pool.id, message),
                    retryable: false,
                    auto_disable_reason: None,
                    cooldown: Some((Duration::ZERO, "model_rewrite_failed".to_string())),
                    protocol_error: None,
                    raw_upstream_error: None,
                },
            )?
        }
    } else {
        body
    };
    Ok(PreparedAccountRequest {
        body,
        outbound_model,
    })
}

#[allow(clippy::result_large_err)]
fn resolve_raw_request_body(
    route: &ExternalRouteRequest,
    pool: &ExternalPool,
) -> Result<ResolvedRawRequestBody, ExternalPoolError> {
    let body = if !route.effective_raw_body.is_empty() {
        route.effective_raw_body.clone()
    } else {
        #[cfg(test)]
        {
            if let Some(payload) = route.payload.as_ref() {
                serde_json::to_vec(payload)
                    .map(Bytes::from)
                    .map_err(|error| ExternalPoolError {
                        status: Some(StatusCode::INTERNAL_SERVER_ERROR),
                        message: format!(
                            "account #{} raw payload serialization failed: {}",
                            pool.id, error
                        ),
                        retryable: false,
                        auto_disable_reason: None,
                        cooldown: None,
                        protocol_error: None,
                        raw_upstream_error: None,
                    })?
            } else if !route.raw_body.is_empty() {
                route.raw_body.clone()
            } else {
                Bytes::new()
            }
        }
        #[cfg(not(test))]
        {
            route.raw_body.clone()
        }
    };
    if body.is_empty() {
        return Err(ExternalPoolError {
            status: Some(StatusCode::INTERNAL_SERVER_ERROR),
            message: format!("account #{} raw request body snapshot is missing", pool.id),
            retryable: false,
            auto_disable_reason: None,
            cooldown: None,
            protocol_error: None,
            raw_upstream_error: None,
        });
    }
    let probe = match route.effective_raw_probe.as_deref() {
        Some(probe) if probe.matches_body(&body) => probe.clone(),
        _ => probe_raw_messages_body(&body),
    };
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
    Ok(ResolvedRawRequestBody { body, probe })
}
