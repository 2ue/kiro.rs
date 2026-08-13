use super::*;

pub(super) fn should_retry_payload_guard(
    route: &ExternalRouteRequest,
    err: &ExternalPoolError,
) -> bool {
    if route.payload_guard_retry_config.is_none() {
        return false;
    }
    if err.status != Some(StatusCode::BAD_REQUEST) {
        return false;
    }
    payload_too_long_message(&err.message)
}

pub(super) fn payload_guard_retry_route(
    route: &ExternalRouteRequest,
) -> Option<ExternalRouteRequest> {
    let config = route.payload_guard_retry_config?;
    let mut payload = route.payload.clone()?;
    let (body, report) = match guard_anthropic_messages_request_reusing_body(
        &mut payload,
        config,
        &route.raw_body,
    ) {
        Ok(result) => result,
        Err(err) => {
            tracing::warn!(
                request_id = %route.request_id,
                error = %err,
                "external pool payload guard retry failed to build trimmed request"
            );
            return None;
        }
    };
    let breakdown = breakdown_anthropic_messages_request(&payload, body.len());
    let request_input_tokens = count_external_route_input_tokens(&payload);
    let mut next = route.clone();
    next.reset_preparation_cache();
    next.raw_body = body;
    next.payload = Some(payload);
    next.request_input_tokens = request_input_tokens;
    next.body_mode_filter = Some(ExternalPoolRequestBodyMode::Normalized);
    next.payload_breakdown = Some(breakdown);
    next.payload_guard_report = Some(report);
    next.payload_guard_retry_config = None;
    Some(next)
}

pub(super) fn should_retry_same_pool(
    config: &ExternalPoolsConfig,
    err: &ExternalPoolError,
) -> bool {
    if !err.retryable {
        return false;
    }
    // These errors should leave the current candidate for this request instead
    // of repeatedly sending to the same pool. They are still treated as
    // recoverable health signals by the pool scheduler.
    if err.auto_disable_reason.is_some()
        || err
            .cooldown
            .as_ref()
            .is_some_and(|(_, reason)| reason == "model_unavailable")
    {
        return false;
    }
    let Some(status) = err.status else {
        return false;
    };
    retry_status_matches(status, &config.same_pool_retry_status_codes())
}

pub(super) fn same_pool_retry_limit(config: &ExternalPoolsConfig, err: &ExternalPoolError) -> u32 {
    if !should_retry_same_pool(config, err) {
        return 0;
    }
    // Keep same-pool replay bounded to a single retry. More than one retry on the
    // same pool tends to amplify one upstream fault into repeated failures for
    // the same request, while the scheduler-level transient penalty already
    // handles future requests.
    config.external_pool_same_pool_retry_count.min(1)
}

pub(super) fn should_retry_cross_pool(
    config: &ExternalPoolsConfig,
    err: &ExternalPoolError,
) -> bool {
    if !err.retryable {
        return false;
    }
    if err.auto_disable_reason.is_some() {
        return true;
    }
    if err.protocol_error.is_some() {
        return config.external_pool_retry_on_protocol_error;
    }
    let Some(status) = err.status else {
        return config.external_pool_retry_on_network_error;
    };
    retry_status_matches(status, &config.retry_status_codes())
}

pub(super) fn same_pool_retry_delay(config: &ExternalPoolsConfig) -> Option<Duration> {
    let delay_ms = config.external_pool_same_pool_retry_delay_ms;
    (delay_ms > 0).then(|| Duration::from_millis(delay_ms))
}

fn retry_status_matches(status: StatusCode, configured: &std::collections::BTreeSet<u16>) -> bool {
    let code = status.as_u16();
    configured.contains(&code) || (status.is_server_error() && configured.contains(&500))
}

fn payload_too_long_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("context window is full")
        || lower.contains("input is too long")
        || lower.contains("prompt is too long")
        || lower.contains("content_length_exceeds_threshold")
        || lower.contains("request payload is too large")
        || lower.contains("payload is too large")
}
