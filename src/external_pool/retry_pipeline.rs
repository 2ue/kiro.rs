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
    let original: serde_json::Value = serde_json::from_slice(&route.effective_raw_body).ok()?;
    let messages = crate::anthropic::payload_guard::trim_route_messages(&original, config)?;
    let mut patches = crate::anthropic::body_patch::BodyPatchSet::default();
    patches.set("/messages", messages, "payload_guard");
    let mut next = route.clone();
    next.reset_preparation_cache();
    next.effective_raw_body = patches.apply(&route.effective_raw_body);
    next.effective_raw_probe = Some(Arc::new(probe_raw_messages_body(&next.effective_raw_body)));
    next.payload = serde_json::from_slice(&next.effective_raw_body).ok();
    next.request_input_tokens = next
        .payload
        .as_ref()
        .map(count_external_route_input_tokens)
        .unwrap_or(0);
    let breakdown = next.payload.as_ref().map(|payload| {
        breakdown_anthropic_messages_request(payload, next.effective_raw_body.len())
    });
    next.payload_breakdown = breakdown;
    let after = serde_json::from_slice(&next.effective_raw_body).ok()?;
    next.payload_guard_report = Some(crate::anthropic::payload_guard::route_trim_report(
        &original, &after, config,
    ));
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
    if err.auto_disable_reason.is_some() || is_pool_or_model_route_error(err) {
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
    // Mapping misses and model-unavailable responses are local to the selected
    // pool/model route. Replaying the same pool cannot fix them, but a
    // different eligible pool may still serve the Claude Code model.
    if is_pool_or_model_route_error(err) {
        return true;
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

fn is_pool_or_model_route_error(err: &ExternalPoolError) -> bool {
    if err.cooldown.as_ref().is_some_and(|(_, reason)| {
        matches!(reason.as_str(), "model_mapping_miss" | "model_unavailable")
    }) {
        return true;
    }

    // Model-unavailable cooldown can be disabled. The classifier emits this
    // exact safe message in that mode, so preserve cross-pool behavior without
    // broad keyword matching.
    err.message.trim() == "external upstream model is unavailable"
}

pub(super) fn payload_too_long_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("context window is full")
        || lower.contains("input is too long")
        || lower.contains("prompt is too long")
        || lower.contains("content_length_exceeds_threshold")
        || lower.contains("request payload is too large")
        || lower.contains("payload is too large")
        || lower.contains("request body is too large")
        || lower.contains("content length exceeded")
}
