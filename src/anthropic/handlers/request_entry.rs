use super::*;

pub(super) async fn handle_messages_endpoint(
    state: AppState,
    headers: HeaderMap,
    raw_body: Bytes,
    endpoint: String,
) -> Response {
    if let Some(response) =
        maybe_raw_external_direct_response(&state, headers.clone(), raw_body.clone(), &endpoint)
            .await
    {
        return response;
    }
    if let Some(response) =
        maybe_raw_external_preflight_response(&state, headers.clone(), raw_body.clone(), &endpoint)
            .await
    {
        return response;
    }
    let payload = match parse_messages_payload(&raw_body) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    post_messages_inner(state, headers, raw_body, payload, endpoint).await
}

pub(super) fn parse_messages_payload(raw_body: &Bytes) -> Result<MessagesRequest, Response> {
    let payload = serde_json::from_slice::<MessagesRequest>(raw_body).map_err(|err| {
        envelope::error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            format!("Invalid JSON body: {}", err),
        )
    })?;
    if payload.model.trim().is_empty() {
        return Err(envelope::error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "model: field is required and cannot be empty",
        ));
    }
    Ok(payload)
}
