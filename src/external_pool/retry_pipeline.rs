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

fn payload_too_long_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("context window is full")
        || lower.contains("input is too long")
        || lower.contains("prompt is too long")
        || lower.contains("content_length_exceeds_threshold")
        || lower.contains("request payload is too large")
        || lower.contains("payload is too large")
}
