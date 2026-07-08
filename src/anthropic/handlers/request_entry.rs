use super::*;

pub(super) async fn handle_messages_endpoint(
    state: AppState,
    headers: HeaderMap,
    mut raw_body: Bytes,
    endpoint: String,
) -> Response {
    let started_at = Instant::now();
    let runtime_config = state
        .kiro_provider
        .as_ref()
        .map(|provider| request_runtime_config(&state, provider))
        .unwrap_or_else(|| RequestRuntimeConfig::from_app_state(&state));
    let mut defaulted_max_tokens = None;
    if let Err(error) = apply_missing_max_tokens_policy(
        &mut raw_body,
        runtime_config.missing_max_tokens,
        &mut defaulted_max_tokens,
    ) {
        let request_id = envelope::request_id();
        record_entry_request_error(
            &state,
            &endpoint,
            &request_id,
            &raw_body,
            &error,
            started_at,
            runtime_config.missing_max_tokens,
            defaulted_max_tokens,
        );
        return error.to_response(&request_id);
    }

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
    let request_id = envelope::request_id();
    let payload = match parse_messages_payload(&raw_body, &request_id) {
        Ok(payload) => payload,
        Err(error) => {
            record_entry_request_error(
                &state,
                &endpoint,
                &request_id,
                &raw_body,
                &error,
                started_at,
                runtime_config.missing_max_tokens,
                defaulted_max_tokens,
            );
            return error.to_response(&request_id);
        }
    };
    post_messages_inner(state, headers, raw_body, payload, endpoint).await
}

#[derive(Debug, Clone)]
pub(super) struct EntryRequestError {
    status: StatusCode,
    error_type: &'static str,
    message: String,
    reason: &'static str,
}

impl EntryRequestError {
    fn invalid(message: impl Into<String>, reason: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            error_type: "invalid_request_error",
            message: message.into(),
            reason,
        }
    }

    pub(super) fn to_response(&self, request_id: &str) -> Response {
        envelope::error_response_with_id(
            self.status,
            self.error_type,
            self.message.clone(),
            request_id,
        )
    }
}

fn apply_missing_max_tokens_policy(
    raw_body: &mut Bytes,
    config: MissingMaxTokensConfig,
    defaulted_max_tokens: &mut Option<i32>,
) -> Result<(), EntryRequestError> {
    let probe = probe_raw_messages_body(raw_body);
    if probe.max_tokens_present || !probe.complete_top_level_object {
        return Ok(());
    }

    match config.normalized().policy {
        MissingMaxTokensPolicy::Reject => Err(EntryRequestError::invalid(
            "max_tokens: field is required",
            "missing_max_tokens",
        )),
        MissingMaxTokensPolicy::DefaultValue => {
            let default_value = config.normalized().default_value;
            match rewrite_raw_missing_top_level_max_tokens_with_probe(
                raw_body,
                &probe,
                default_value,
            ) {
                Ok(Some(rewritten)) => {
                    *raw_body = rewritten;
                    *defaulted_max_tokens = Some(default_value);
                    Ok(())
                }
                Ok(None) => Ok(()),
                Err(err) => Err(EntryRequestError::invalid(
                    format!("Invalid JSON body: {}", err),
                    "missing_max_tokens_rewrite_failed",
                )),
            }
        }
    }
}

pub(super) fn parse_messages_payload(
    raw_body: &Bytes,
    _request_id: &str,
) -> Result<MessagesRequest, EntryRequestError> {
    let payload = serde_json::from_slice::<MessagesRequest>(raw_body).map_err(|err| {
        EntryRequestError::invalid(format!("Invalid JSON body: {}", err), "invalid_json_body")
    })?;
    if payload.model.trim().is_empty() {
        return Err(EntryRequestError::invalid(
            "model: field is required and cannot be empty",
            "empty_model",
        ));
    }
    Ok(payload)
}

fn record_entry_request_error(
    state: &AppState,
    endpoint: &str,
    request_id: &str,
    raw_body: &Bytes,
    error: &EntryRequestError,
    started_at: Instant,
    missing_max_tokens: MissingMaxTokensConfig,
    defaulted_max_tokens: Option<i32>,
) {
    let probe = probe_raw_messages_body(raw_body);
    let policy = missing_max_tokens.normalized();
    let duration_ms = started_at.elapsed().as_millis() as u64;
    tracing::warn!(
        request_id,
        endpoint,
        reason = error.reason,
        status = error.status.as_u16(),
        body_bytes = raw_body.len(),
        max_tokens_present = probe.max_tokens_present,
        defaulted_max_tokens = ?defaulted_max_tokens,
        "Anthropic Messages request rejected at entry"
    );
    state.usage_recorder.record(UsageRecord {
        id: request_id.to_string(),
        created_at: Utc::now().to_rfc3339(),
        endpoint: endpoint.to_string(),
        stream: probe.stream.unwrap_or(false),
        model: probe.model.unwrap_or_else(|| "unknown".to_string()),
        requested_max_tokens: None,
        downstream_stop_reason: None,
        upstream_model: None,
        external_outbound_model: None,
        model_resolution_source: None,
        model_resolution_note: None,
        conversation_id: None,
        credential_id: None,
        credential_label: None,
        status: UsageRecordStatus::Error,
        usage_source: UsageSource::None,
        raw_usage: None,
        total_input_tokens: 0,
        compat_input_tokens: 0,
        billable_input_tokens: 0,
        output_tokens: 0,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        cache_creation_5m_input_tokens: 0,
        cache_creation_1h_input_tokens: 0,
        estimated_cost_usd: 0.0,
        kiro_metering_usage: 0.0,
        pricing_available: false,
        pricing_model: None,
        duration_ms,
        first_token_latency_ms: None,
        response_latency_ms: Some(duration_ms),
        latency_trace: None,
        simulated: false,
        sticky_bound: false,
        fallback_from_sticky: false,
        credential_attempts: Vec::new(),
        route_kind: None,
        route_subtype: None,
        fallback_reason: None,
        direct_policy_reason: None,
        local_attempted: None,
        local_preflight: None,
        external_pool_id: None,
        external_pool_name: None,
        external_attempts: Vec::new(),
        usage_projection_applied: None,
        external_pool_billing: None,
        error_type: Some(error.error_type.to_string()),
        error_message: Some(error.message.clone()),
        error_detail: Some(error.message.clone()),
        error_status_code: Some(error.status.as_u16()),
        error_source: Some("request_entry".to_string()),
        error_id: Some(request_id.to_string()),
        error_metadata: Some(json!({
            "stage": "request_entry",
            "reason": error.reason,
            "bodyBytes": raw_body.len(),
            "maxTokensPresent": probe.max_tokens_present,
            "completeTopLevelObject": probe.complete_top_level_object,
            "missingMaxTokensPolicy": match policy.policy {
                MissingMaxTokensPolicy::Reject => "reject",
                MissingMaxTokensPolicy::DefaultValue => "default_value",
            },
            "defaultedMaxTokens": defaulted_max_tokens,
        })),
        public_error_status_code: Some(error.status.as_u16()),
        public_error_type: Some(error.error_type.to_string()),
        public_error_message: Some(error.message.clone()),
        payload_breakdown: None,
        payload_guard_report: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::prompt_cache::PromptCacheTracker;
    use crate::anthropic::prompt_cache_creation_control::PromptCacheCreationController;
    use crate::anthropic::usage::{UsageRecordQuery, UsageRecorder};
    use crate::common::auth::RequestApiKeyStore;
    use crate::model::config::DEFAULT_MISSING_MAX_TOKENS_VALUE;

    fn test_state(recorder: Arc<UsageRecorder>) -> AppState {
        AppState::new(
            Arc::new(RequestApiKeyStore::new(["test-key"])),
            true,
            recorder,
            Arc::new(PromptCacheTracker::default()),
            Arc::new(PromptCacheCreationController::default()),
            PromptCacheSimulationMode::HighCache,
            0.98,
            CompatProfile::ClaudeCode,
            false,
        )
    }

    #[test]
    fn missing_max_tokens_default_value_rewrites_body_for_typed_parse() {
        let mut raw = Bytes::from_static(
            br#"{"model":"claude-sonnet-4-5","messages":[{"role":"user","content":"hello"}],"stream":false}"#,
        );
        let mut defaulted = None;

        apply_missing_max_tokens_policy(
            &mut raw,
            MissingMaxTokensConfig::default(),
            &mut defaulted,
        )
        .expect("default missing max_tokens");
        let parsed = parse_messages_payload(&raw, "req_test_missing_default").expect("typed parse");

        assert_eq!(defaulted, Some(DEFAULT_MISSING_MAX_TOKENS_VALUE));
        assert_eq!(parsed.max_tokens, DEFAULT_MISSING_MAX_TOKENS_VALUE);
        assert_eq!(parsed.model, "claude-sonnet-4-5");
    }

    #[test]
    fn missing_max_tokens_reject_records_entry_error_without_body_content() {
        let recorder = Arc::new(UsageRecorder::new(10));
        let state = test_state(recorder.clone());
        let raw = Bytes::from_static(
            br#"{"model":"claude-sonnet-4-5","messages":[{"role":"user","content":"secret-body"}],"stream":true}"#,
        );
        let error =
            EntryRequestError::invalid("max_tokens: field is required", "missing_max_tokens");

        record_entry_request_error(
            &state,
            "/cc/v1/messages",
            "req_entry_missing_max_tokens",
            &raw,
            &error,
            Instant::now(),
            MissingMaxTokensConfig {
                policy: MissingMaxTokensPolicy::Reject,
                default_value: DEFAULT_MISSING_MAX_TOKENS_VALUE,
            },
            None,
        );

        let records = recorder.query(UsageRecordQuery::default()).records;
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.id, "req_entry_missing_max_tokens");
        assert_eq!(record.status, UsageRecordStatus::Error);
        assert_eq!(record.usage_source, UsageSource::None);
        assert_eq!(record.error_source.as_deref(), Some("request_entry"));
        assert_eq!(record.error_status_code, Some(400));
        assert_eq!(
            record.error_id.as_deref(),
            Some("req_entry_missing_max_tokens")
        );
        assert_eq!(record.model, "claude-sonnet-4-5");
        assert!(record.stream);
        let metadata = record.error_metadata.as_ref().expect("metadata");
        assert_eq!(metadata["stage"], "request_entry");
        assert_eq!(metadata["reason"], "missing_max_tokens");
        assert_eq!(metadata["missingMaxTokensPolicy"], "reject");
        let metadata_text = metadata.to_string();
        assert!(metadata_text.contains("bodyBytes"));
        assert!(!metadata_text.contains("secret-body"));
    }

    #[test]
    fn malformed_json_parse_error_can_be_recorded_at_entry() {
        let recorder = Arc::new(UsageRecorder::new(10));
        let state = test_state(recorder.clone());
        let raw = Bytes::from_static(br#"{"model":"claude-sonnet-4-5","max_tokens":16"#);
        let error = parse_messages_payload(&raw, "req_entry_bad_json").expect_err("malformed json");

        record_entry_request_error(
            &state,
            "/v1/messages",
            "req_entry_bad_json",
            &raw,
            &error,
            Instant::now(),
            MissingMaxTokensConfig::default(),
            None,
        );

        let records = recorder.query(UsageRecordQuery::default()).records;
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.id, "req_entry_bad_json");
        assert_eq!(record.status, UsageRecordStatus::Error);
        assert_eq!(record.error_source.as_deref(), Some("request_entry"));
        assert_eq!(
            record
                .error_metadata
                .as_ref()
                .and_then(|value| value.get("reason"))
                .and_then(serde_json::Value::as_str),
            Some("invalid_json_body")
        );
    }
}
