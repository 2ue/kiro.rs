use super::*;

pub(super) async fn handle_messages_endpoint(
    state: AppState,
    headers: HeaderMap,
    mut raw_body: Bytes,
    endpoint: String,
    request_api_key_id: Option<String>,
    attribution: Option<RequestRejectionAttribution>,
) -> Response {
    let runtime_config = state
        .kiro_provider
        .as_ref()
        .map(|provider| request_runtime_config(&state, provider))
        .unwrap_or_else(|| RequestRuntimeConfig::from_app_state(&state));
    let inference_attempt_budget = Arc::new(InferenceAttemptBudget::with_auxiliary_max_attempts(
        runtime_config.inference_upstream_max_attempts,
        runtime_config.auxiliary_upstream_max_attempts,
    ));
    let mut defaulted_max_tokens = None;
    let mut raw_probe = probe_raw_messages_body(&raw_body);
    if let Err(error) = apply_missing_max_tokens_policy_with_probe(
        &mut raw_body,
        &raw_probe,
        runtime_config.missing_max_tokens,
        &mut defaulted_max_tokens,
    ) {
        let request_id = envelope::request_id();
        record_entry_request_error(attribution.as_ref(), &endpoint, &request_id, &error);
        return error.to_response(&request_id);
    }
    if defaulted_max_tokens.is_some() {
        raw_probe = probe_raw_messages_body(&raw_body);
    }
    if let Some(probe_error) = raw_probe.scan_error() {
        let request_id = envelope::request_id();
        let error = entry_error_from_raw_probe(probe_error);
        record_entry_request_error(attribution.as_ref(), &endpoint, &request_id, &error);
        return error.to_response(&request_id);
    }
    // Raw external routes preserve the effective client request after the
    // configured missing-max-tokens policy, before any compatibility cleanup.
    let effective_raw_body = raw_body.clone();

    if let Err(message) =
        validate_raw_reasoning_protocol_with_probe(&effective_raw_body, &raw_probe)
    {
        let request_id = envelope::request_id();
        let error = EntryRequestError::invalid(message, "invalid_reasoning_protocol");
        record_entry_request_error(attribution.as_ref(), &endpoint, &request_id, &error);
        return error.to_response(&request_id);
    }

    let mut request_history_contaminated = false;
    let raw_history_sanitization =
        super::super::transcript_sanitizer::sanitize_raw_request_assistant_history_with_probe(
            &raw_body, &raw_probe,
        );
    let raw_history_sanitization = match raw_history_sanitization {
        Ok(sanitization) => sanitization,
        Err(error) => {
            let request_id = envelope::request_id();
            let error = EntryRequestError::invalid(
                format!("Invalid JSON body: {error}"),
                "request_history_inspection_failed",
            );
            record_entry_request_error(attribution.as_ref(), &endpoint, &request_id, &error);
            return error.to_response(&request_id);
        }
    };
    let effective_raw_probe = Arc::new(raw_probe);
    let mut parsed_raw_probe = effective_raw_probe.clone();
    if let Some((sanitized_body, _report)) = raw_history_sanitization {
        if runtime_config.compat_profile.is_strict() {
            let request_id = envelope::request_id();
            let error = EntryRequestError::invalid(
                envelope::PUBLIC_INVALID_REQUEST_MESSAGE,
                "strict_request_protocol_contamination",
            );
            record_entry_request_error(attribution.as_ref(), &endpoint, &request_id, &error);
            return envelope::error_response_with_id_and_headers(
                error.status,
                error.error_type,
                envelope::public_message_with_error_id(&error.message, &request_id),
                &request_id,
                [("x-error-id", request_id.clone())],
            );
        }
        raw_body = Bytes::from(sanitized_body);
        parsed_raw_probe = Arc::new(probe_raw_messages_body(&raw_body));
        request_history_contaminated = true;
    }

    if should_try_raw_external_routes(request_history_contaminated) {
        if let Some(response) = maybe_raw_external_direct_response(
            &state,
            headers.clone(),
            effective_raw_body.clone(),
            &endpoint,
            inference_attempt_budget.clone(),
            request_api_key_id.clone(),
            effective_raw_probe.clone(),
        )
        .await
        {
            return response;
        }
        if let Some(response) = maybe_raw_external_preflight_response(
            &state,
            headers.clone(),
            effective_raw_body.clone(),
            &endpoint,
            inference_attempt_budget.clone(),
            request_api_key_id.clone(),
            effective_raw_probe.clone(),
        )
        .await
        {
            return response;
        }
    }
    if let Some(response) = maybe_local_pool_unavailable_fast_fail_response(
        &state,
        &runtime_config,
        &endpoint,
        attribution.as_ref(),
        &parsed_raw_probe,
    ) {
        return response;
    }
    let request_id = envelope::request_id();
    let payload = match parse_messages_payload_with_probe(&raw_body, &parsed_raw_probe, &request_id)
    {
        Ok(payload) => payload,
        Err(error) => {
            record_entry_request_error(attribution.as_ref(), &endpoint, &request_id, &error);
            return error.to_response(&request_id);
        }
    };
    post_messages_inner(
        state,
        headers,
        effective_raw_body,
        effective_raw_probe,
        raw_body,
        payload,
        endpoint,
        inference_attempt_budget,
        request_api_key_id,
        request_history_contaminated,
        attribution,
    )
    .await
}

fn should_try_raw_external_routes(request_history_contaminated: bool) -> bool {
    !request_history_contaminated
}

fn maybe_local_pool_unavailable_fast_fail_response(
    state: &AppState,
    runtime_config: &RequestRuntimeConfig,
    endpoint: &str,
    attribution: Option<&RequestRejectionAttribution>,
    raw_probe: &RawMessagesBodyProbe,
) -> Option<Response> {
    let provider = state.kiro_provider.as_ref()?;
    let model = raw_probe
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())?;

    // If external pools are globally enabled and wired, the normalized external fallback path may
    // still be eligible after typed parsing. Do not preempt it with a local-only response.
    if runtime_config.external_pools.external_pools_enabled && state.external_pool_manager.is_some()
    {
        return None;
    }

    let local_state = provider.local_pool_route_state_cached(Some(model));
    let (status, error_type, message, reason, retry_after_secs) =
        local_pool_fast_fail_response_parts(local_state.kind, local_state.retry_after_secs)?;

    if let (Some(attribution), Some(retry_after_secs)) = (attribution, retry_after_secs) {
        attribution.apply_local_temporary_backoff(retry_after_secs);
    }

    let request_id = envelope::request_id();
    tracing::warn!(
        request_id,
        endpoint,
        model,
        local_state = ?local_state.kind,
        local_total = local_state.total,
        local_available = local_state.available,
        local_dispatchable = local_state.dispatchable,
        local_usable = local_state.usable,
        retry_after_secs = ?retry_after_secs,
        "local credential pool is unavailable and no external pool takeover is available; rejecting before full body processing"
    );
    let response = match retry_after_secs {
        Some(retry_after_secs) => envelope::error_response_with_id_and_headers(
            status,
            error_type,
            message,
            &request_id,
            [("retry-after", retry_after_secs.to_string())],
        ),
        None => envelope::error_response_with_id(status, error_type, message, &request_id),
    };
    record_pre_usage_rejection(attribution, reason, endpoint, &response);
    Some(response)
}

fn local_pool_fast_fail_response_parts(
    kind: LocalPoolRouteStateKind,
    retry_after_secs: Option<u64>,
) -> Option<(
    StatusCode,
    &'static str,
    String,
    RequestRejectionReason,
    Option<u64>,
)> {
    Some(match kind {
        LocalPoolRouteStateKind::NoCredentials
        | LocalPoolRouteStateKind::AllDisabled
        | LocalPoolRouteStateKind::ProxyBlocked => (
            StatusCode::SERVICE_UNAVAILABLE,
            "api_error",
            envelope::PUBLIC_ACCOUNT_UNAVAILABLE_MESSAGE.to_string(),
            RequestRejectionReason::LocalPoolUnavailable,
            None,
        ),
        LocalPoolRouteStateKind::SchedulerRedisDegraded => {
            let retry_after_secs = retry_after_secs.unwrap_or(1).max(1);
            (
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limit_error",
                envelope::public_rate_limit_message(Some(retry_after_secs)),
                RequestRejectionReason::LocalPoolTemporaryUnavailable,
                Some(retry_after_secs),
            )
        }
        LocalPoolRouteStateKind::RiskCircuitOpen => {
            let retry_after_secs = retry_after_secs.unwrap_or(1).max(1);
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "api_error",
                envelope::PUBLIC_TEMPORARY_FAILURE_MESSAGE.to_string(),
                RequestRejectionReason::LocalPoolTemporaryUnavailable,
                Some(retry_after_secs),
            )
        }
        LocalPoolRouteStateKind::Ready
        | LocalPoolRouteStateKind::NoModelCompatible
        | LocalPoolRouteStateKind::AllCoolingDown
        | LocalPoolRouteStateKind::CapacityFull => return None,
    })
}

#[derive(Debug, Clone)]
pub(super) struct EntryRequestError {
    status: StatusCode,
    error_type: &'static str,
    message: String,
    #[cfg_attr(not(test), allow(dead_code))]
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

    fn rejection_reason(&self) -> RequestRejectionReason {
        match self.reason {
            "strict_request_protocol_contamination" => {
                RequestRejectionReason::StrictRequestProtocolContamination
            }
            _ => RequestRejectionReason::RequestEntryInvalid,
        }
    }
}

#[cfg(test)]
fn apply_missing_max_tokens_policy(
    raw_body: &mut Bytes,
    config: MissingMaxTokensConfig,
    defaulted_max_tokens: &mut Option<i32>,
) -> Result<(), EntryRequestError> {
    let probe = probe_raw_messages_body(raw_body);
    apply_missing_max_tokens_policy_with_probe(raw_body, &probe, config, defaulted_max_tokens)
}

fn apply_missing_max_tokens_policy_with_probe(
    raw_body: &mut Bytes,
    probe: &RawMessagesBodyProbe,
    config: MissingMaxTokensConfig,
    defaulted_max_tokens: &mut Option<i32>,
) -> Result<(), EntryRequestError> {
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
                probe,
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

fn entry_error_from_raw_probe(
    error: super::super::request_facts::RawMessagesBodyProbeError,
) -> EntryRequestError {
    let reason = match error {
        super::super::request_facts::RawMessagesBodyProbeError::NestingTooDeep => {
            "json_nesting_too_deep"
        }
        super::super::request_facts::RawMessagesBodyProbeError::BodyTooLarge => {
            "json_body_too_large"
        }
        super::super::request_facts::RawMessagesBodyProbeError::ModelTooLong => "model_too_long",
        super::super::request_facts::RawMessagesBodyProbeError::WorkLimitExceeded => {
            "json_probe_work_limit"
        }
        super::super::request_facts::RawMessagesBodyProbeError::DuplicateObjectKey => {
            "duplicate_json_object_key"
        }
        super::super::request_facts::RawMessagesBodyProbeError::MalformedTopLevelJson => {
            "invalid_json_body"
        }
    };
    EntryRequestError::invalid(error.to_string(), reason)
}

#[cfg(test)]
pub(super) fn parse_messages_payload(
    raw_body: &Bytes,
    request_id: &str,
) -> Result<MessagesRequest, EntryRequestError> {
    let probe = probe_raw_messages_body(raw_body);
    parse_messages_payload_with_probe(raw_body, &probe, request_id)
}

fn parse_messages_payload_with_probe(
    raw_body: &Bytes,
    probe: &RawMessagesBodyProbe,
    _request_id: &str,
) -> Result<MessagesRequest, EntryRequestError> {
    if !probe.matches_body(raw_body) {
        return Err(EntryRequestError::invalid(
            "raw request probe does not match the request body snapshot",
            "raw_probe_body_mismatch",
        ));
    }
    if let Some(error) = probe.scan_error() {
        return Err(entry_error_from_raw_probe(error));
    }
    let payload = deserialize_messages_request_with_probe(raw_body, probe)
        .map_err(|message| EntryRequestError::invalid(message, "invalid_json_body"))?;
    if payload.model.trim().is_empty() {
        return Err(EntryRequestError::invalid(
            "model: field is required and cannot be empty",
            "empty_model",
        ));
    }
    validate_typed_reasoning_protocol(&payload)?;
    Ok(payload)
}

fn validate_typed_reasoning_protocol(payload: &MessagesRequest) -> Result<(), EntryRequestError> {
    if payload
        .output_config
        .as_ref()
        .and_then(|output_config| output_config.effort.as_deref())
        .is_some_and(|effort| super::super::types::parse_thinking_effort(effort).is_none())
    {
        return Err(EntryRequestError::invalid(
            format!(
                "output_config.effort must be one of: {}",
                super::super::types::THINKING_EFFORT_VALUES.join(", ")
            ),
            "invalid_thinking_effort",
        ));
    }
    let Some(thinking) = payload.thinking.as_ref() else {
        return Ok(());
    };
    if !matches!(
        thinking.thinking_type.as_str(),
        "enabled" | "adaptive" | "disabled"
    ) {
        return Err(EntryRequestError::invalid(
            "thinking.type must be one of: enabled, adaptive, disabled",
            "invalid_thinking_type",
        ));
    }
    if thinking.thinking_type == "enabled" {
        if thinking.budget_tokens < 1_024 {
            return Err(EntryRequestError::invalid(
                "thinking.budget_tokens is required and must be at least 1024",
                "invalid_thinking_budget",
            ));
        }
        if thinking.budget_tokens >= payload.max_tokens {
            return Err(EntryRequestError::invalid(
                "thinking.budget_tokens must be less than max_tokens",
                "invalid_thinking_budget",
            ));
        }
    }
    Ok(())
}

fn record_entry_request_error(
    attribution: Option<&RequestRejectionAttribution>,
    endpoint: &str,
    request_id: &str,
    error: &EntryRequestError,
) {
    if let Some(attribution) = attribution {
        attribution.record(
            error.rejection_reason(),
            "request_entry",
            error.status,
            request_id,
            endpoint,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::prompt_cache::PromptCacheTracker;
    use crate::anthropic::prompt_cache_creation_control::PromptCacheCreationController;
    use crate::anthropic::request_admission::RequestAdmissionController;
    use crate::anthropic::usage::{UsageRecordQuery, UsageRecorder};
    use crate::common::auth::RequestApiKeyStore;
    use crate::model::config::{DEFAULT_MISSING_MAX_TOKENS_VALUE, RequestAdmissionConfig};

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

    fn test_attribution(recorder: Arc<UsageRecorder>) -> RequestRejectionAttribution {
        let store = RequestApiKeyStore::new(["test-key"]);
        let identity = store.authenticate("test-key").unwrap();
        RequestRejectionAttribution::detached(
            Arc::new(RequestAdmissionController::new(
                RequestAdmissionConfig::disabled(),
            )),
            recorder,
            identity,
        )
    }

    fn nested_json_value(depth: usize) -> Value {
        let mut value = json!({"leaf": true});
        for level in (0..depth).rev() {
            value = json!({"level": level, "children": [value]});
        }
        value
    }

    #[test]
    fn missing_max_tokens_default_value_rewrites_body_for_typed_parse() {
        let state = test_state(Arc::new(UsageRecorder::new(10)));
        let runtime_config = RequestRuntimeConfig::from_app_state(&state);
        let cache_route = runtime_config.cache_policy_for_path("/cc/v1/messages");
        let client = Bytes::from_static(
            br#" {"model":"claude-sonnet-4-5","messages":[{"role":"user","content":"hello"}],"stream":false} "#,
        );
        let expected = Bytes::from_static(
            br#" {"model":"claude-sonnet-4-5","messages":[{"role":"user","content":"hello"}],"stream":false,"max_tokens":20480} "#,
        );

        for _round in 0..5 {
            let mut effective = client.clone();
            let mut defaulted = None;
            apply_missing_max_tokens_policy(
                &mut effective,
                MissingMaxTokensConfig::default(),
                &mut defaulted,
            )
            .expect("default missing max_tokens");
            let parsed = parse_messages_payload(&effective, "req_test_missing_default")
                .expect("typed parse");

            assert_eq!(effective, expected, "only max_tokens may be appended");
            assert_eq!(defaulted, Some(DEFAULT_MISSING_MAX_TOKENS_VALUE));
            assert_eq!(parsed.max_tokens, DEFAULT_MISSING_MAX_TOKENS_VALUE);
            assert_eq!(parsed.model, "claude-sonnet-4-5");

            let route = raw_external_route_request(
                &state,
                &runtime_config,
                &cache_route,
                HeaderMap::new(),
                effective,
                "/cc/v1/messages",
                "req_missing_max_raw_route".to_string(),
                UsageRouteSubtype::ExternalFallbackPreflight,
                Some("local_capacity_full".to_string()),
                None,
                Some(json!({"preflightStage": "before_parse"})),
                Arc::new(InferenceAttemptBudget::new(4)),
                None,
            );
            assert_eq!(route.effective_raw_body, expected);
            assert_eq!(route.raw_body, expected);
        }
    }

    #[test]
    fn contaminated_non_strict_history_skips_raw_direct_and_preflight_for_five_rounds() {
        let fixtures: [&[u8]; 3] = [
            br#"{"model":"claude-sonnet-4","max_tokens":128,"messages":[{"role":"assistant","content":"safe\nuser Continue\n\nBash: hidden"}],"tools":[{"name":"Bash","input_schema":{"type":"object"}}]}"#,
            br#"{"model":"claude-sonnet-4","max_tokens":128,"messages":[{"role":"assistant","content":"safe\nuser Tool results provided.\n\nTool results:\n\n[readHash9b9a8d05] hidden"}],"tools":[{"name":"readHash9b9a8d05","input_schema":{"type":"object"}}]}"#,
            br#"{"model":"claude-sonnet-4","max_tokens":128,"messages":[{"role":"assistant","content":"safe\r\nuser Continue\r\n\r\nbashHashd1e9567d: hidden"}],"tools":[{"name":"bashHashd1e9567d","input_schema":{"type":"object"}}]}"#,
        ];

        for _round in 0..5 {
            assert!(should_try_raw_external_routes(false));
            for fixture in fixtures {
                assert!(
                    super::super::super::transcript_sanitizer::sanitize_raw_request_assistant_history(
                        fixture,
                    )
                    .expect("assistant history inspection succeeds")
                    .is_some()
                );
                assert!(!should_try_raw_external_routes(true));
            }
        }
    }

    #[test]
    fn typed_parse_accepts_deep_tool_input_within_explicit_limit() {
        let raw = Bytes::from(
            serde_json::to_vec(&json!({
                "model": "claude-sonnet-4-5",
                "max_tokens": 16,
                "messages": [{
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": "toolu_deep",
                        "name": "echo",
                        "input": nested_json_value(80)
                    }]
                }]
            }))
            .expect("serialize deep request"),
        );

        let parsed = parse_messages_payload(&raw, "req_deep_json").expect("deep request parses");

        assert_eq!(parsed.model, "claude-sonnet-4-5");
        assert_eq!(parsed.messages.len(), 1);
    }

    #[test]
    fn typed_parse_rejects_json_beyond_explicit_nesting_limit() {
        let raw = Bytes::from(
            serde_json::to_vec(&json!({
                "model": "claude-sonnet-4-5",
                "max_tokens": 16,
                "messages": [{
                    "role": "user",
                    "content": nested_json_value(MAX_MESSAGES_JSON_NESTING_DEPTH)
                }]
            }))
            .expect("serialize over-deep request"),
        );

        let error = parse_messages_payload(&raw, "req_too_deep_json")
            .expect_err("over-deep request is rejected");

        assert_eq!(error.reason, "json_nesting_too_deep");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn missing_max_tokens_reject_records_entry_error_without_body_content() {
        let recorder = Arc::new(UsageRecorder::new(10));
        let attribution = test_attribution(recorder.clone());
        let error =
            EntryRequestError::invalid("max_tokens: field is required", "missing_max_tokens");

        for count in 1..=5 {
            record_entry_request_error(
                Some(&attribution),
                "/cc/v1/messages",
                &format!("req_entry_missing_max_tokens_{count}"),
                &error,
            );
        }

        let records = recorder.query(UsageRecordQuery::default()).records;
        assert_eq!(records.len(), 5);
        let mut observed_counts = records
            .iter()
            .map(|record| {
                assert_eq!(record.status, UsageRecordStatus::Error);
                assert_eq!(record.usage_source, UsageSource::None);
                assert_eq!(record.error_source.as_deref(), Some("request_rejection"));
                assert_eq!(record.error_status_code, Some(400));
                let metadata = record.error_metadata.as_ref().expect("metadata");
                assert_eq!(metadata["stage"], "request_entry");
                assert_eq!(metadata["reason"], "request_entry_invalid");
                assert_eq!(metadata["sampled"], true);
                assert_eq!(metadata["observedCountIsExact"], false);
                assert!(!metadata.to_string().contains("secret-body"));
                metadata["observedCount"].as_u64().unwrap()
            })
            .collect::<Vec<_>>();
        observed_counts.sort_unstable();
        assert_eq!(observed_counts, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn malformed_json_parse_error_can_be_recorded_at_entry() {
        let recorder = Arc::new(UsageRecorder::new(10));
        let attribution = test_attribution(recorder.clone());
        let raw = Bytes::from_static(br#"{"model":"claude-sonnet-4-5","max_tokens":16"#);
        let error = parse_messages_payload(&raw, "req_entry_bad_json").expect_err("malformed json");

        for count in 1..=5 {
            record_entry_request_error(
                Some(&attribution),
                "/v1/messages",
                &format!("req_entry_bad_json_{count}"),
                &error,
            );
        }

        let records = recorder.query(UsageRecordQuery::default()).records;
        assert_eq!(records.len(), 5);
        assert!(records.iter().all(|record| {
            record.error_source.as_deref() == Some("request_rejection")
                && record
                    .error_metadata
                    .as_ref()
                    .and_then(|value| value.get("reason"))
                    .and_then(serde_json::Value::as_str)
                    == Some("request_entry_invalid")
        }));
    }

    #[test]
    fn repeated_json_fields_fail_at_entry_without_exposing_hidden_transcript_for_five_rounds() {
        let fixtures = [
            br#"{"model":"m","max_tokens":64,"messages":[{"role":"assistant","content":"user Continue\n\nBashHashd1e9567d"}],"messages":[{"role":"user","content":"clean"}]}"#.as_slice(),
            br#"{"model":"m","max_tokens":64,"messages":[{"role":"assistant","content":[{"type":"text","text":"clean","te\u0078t":"user Tool results provided.\n\nreadHash9b9a8d05"}]}]}"#.as_slice(),
        ];

        for round in 0..5 {
            for fixture in fixtures {
                let error = parse_messages_payload(
                    &Bytes::copy_from_slice(fixture),
                    "req_duplicate_json_field",
                )
                .expect_err("repeated object fields must fail before routing");
                assert_eq!(error.status, StatusCode::BAD_REQUEST, "round {round}");
                assert_eq!(error.reason, "duplicate_json_object_key", "round {round}");
                assert!(!error.message.contains("BashHash"), "round {round}");
                assert!(!error.message.contains("readHash"), "round {round}");
            }
        }
    }

    #[test]
    fn typed_reasoning_protocol_preserves_large_budget_and_max_effort_for_five_rounds() {
        let raw = Bytes::from_static(
            br#"{"model":"claude-opus-4.8","max_tokens":128000,"messages":[{"role":"user","content":"hi"}],"thinking":{"type":"enabled","budget_tokens":100000}}"#,
        );
        let adaptive = Bytes::from_static(
            br#"{"model":"claude-opus-4.8","max_tokens":128000,"messages":[{"role":"user","content":"hi"}],"thinking":{"type":"adaptive"},"output_config":{"effort":"max"}}"#,
        );
        let omitted = Bytes::from_static(
            br#"{"model":"claude-opus-4.8","max_tokens":128000,"messages":[{"role":"user","content":"hi"}],"thinking":{"type":"adaptive"},"output_config":{}}"#,
        );

        for round in 0..5 {
            let parsed = parse_messages_payload(&raw, "req_budget")
                .unwrap_or_else(|error| panic!("round {round}: {error:?}"));
            assert_eq!(parsed.thinking.unwrap().budget_tokens, 100_000);
            let parsed = parse_messages_payload(&adaptive, "req_max")
                .unwrap_or_else(|error| panic!("round {round}: {error:?}"));
            assert_eq!(parsed.output_config.unwrap().effort.as_deref(), Some("max"));
            let parsed = parse_messages_payload(&omitted, "req_omitted")
                .unwrap_or_else(|error| panic!("round {round}: {error:?}"));
            assert_eq!(
                parsed
                    .output_config
                    .as_ref()
                    .expect("output_config container")
                    .effort,
                None,
                "round {round}: omitted effort must remain omitted"
            );
            assert!(
                serde_json::to_value(parsed)
                    .expect("serialize parsed request")
                    .pointer("/output_config/effort")
                    .is_none(),
                "round {round}: typed serialization must not add effort"
            );
        }
    }

    #[test]
    fn omitted_output_effort_keeps_enabled_and_disabled_thinking_authoritative_five_rounds() {
        let enabled = Bytes::from_static(
            br#"{"model":"claude-opus-4.8","max_tokens":64000,"messages":[{"role":"user","content":"hi"}],"thinking":{"type":"enabled","budget_tokens":32000},"output_config":{}}"#,
        );
        let disabled = Bytes::from_static(
            br#"{"model":"claude-opus-4.8","max_tokens":4096,"messages":[{"role":"user","content":"hi"}],"thinking":{"type":"disabled"},"output_config":{}}"#,
        );
        let disabled_with_effort = Bytes::from_static(
            br#"{"model":"claude-opus-4.8","max_tokens":4096,"messages":[{"role":"user","content":"hi"}],"thinking":{"type":"disabled"},"output_config":{"effort":"max"}}"#,
        );
        let enabled_with_effort = Bytes::from_static(
            br#"{"model":"claude-opus-4.5","max_tokens":4096,"messages":[{"role":"user","content":"hi"}],"thinking":{"type":"enabled","budget_tokens":2048},"output_config":{"effort":"high"}}"#,
        );

        for round in 0..5 {
            let parsed = parse_messages_payload(&enabled, "req_enabled_omitted")
                .unwrap_or_else(|error| panic!("round {round}: {error:?}"));
            assert_eq!(
                parsed
                    .thinking
                    .as_ref()
                    .map(|thinking| thinking.thinking_type.as_str()),
                Some("enabled"),
                "round {round}"
            );
            assert_eq!(
                parsed
                    .output_config
                    .as_ref()
                    .and_then(|config| config.effort.as_deref()),
                None,
                "round {round}"
            );

            let parsed = parse_messages_payload(&disabled, "req_disabled_omitted")
                .unwrap_or_else(|error| panic!("round {round}: {error:?}"));
            assert_eq!(
                parsed
                    .thinking
                    .as_ref()
                    .map(|thinking| thinking.thinking_type.as_str()),
                Some("disabled"),
                "round {round}"
            );
            assert_eq!(
                parsed
                    .output_config
                    .as_ref()
                    .and_then(|config| config.effort.as_deref()),
                None,
                "round {round}"
            );

            let parsed = parse_messages_payload(&disabled_with_effort, "req_disabled_effort")
                .unwrap_or_else(|error| panic!("round {round}: {error:?}"));
            assert_eq!(
                parsed
                    .thinking
                    .as_ref()
                    .map(|thinking| thinking.thinking_type.as_str()),
                Some("disabled"),
                "round {round}"
            );
            assert_eq!(
                parsed
                    .output_config
                    .as_ref()
                    .and_then(|config| config.effort.as_deref()),
                Some("max"),
                "round {round}: Claude CLI disabled thinking must not discard explicit effort"
            );

            let parsed = parse_messages_payload(&enabled_with_effort, "req_enabled_effort")
                .unwrap_or_else(|error| panic!("round {round}: {error:?}"));
            assert_eq!(
                parsed
                    .thinking
                    .as_ref()
                    .map(|thinking| thinking.thinking_type.as_str()),
                Some("enabled"),
                "round {round}"
            );
            assert_eq!(
                parsed
                    .output_config
                    .as_ref()
                    .and_then(|config| config.effort.as_deref()),
                Some("high"),
                "round {round}: valid budget plus explicit effort is a protocol-level supported form"
            );
        }
    }

    #[test]
    fn typed_reasoning_protocol_rejects_unknown_and_ambiguous_controls_for_five_rounds() {
        let fixtures = [
            br#"{"model":"m","max_tokens":4096,"messages":[],"thinking":{"type":"mystery"}}"#.as_slice(),
            br#"{"model":"m","max_tokens":4096,"messages":[],"thinking":{"type":"enabled","budget_tokens":1023}}"#.as_slice(),
            br#"{"model":"m","max_tokens":4096,"messages":[],"thinking":{"type":"enabled"}}"#.as_slice(),
            br#"{"model":"m","max_tokens":4096,"messages":[],"thinking":{"type":"enabled","budget_tokens":4096}}"#.as_slice(),
            br#"{"model":"m","max_tokens":4096,"messages":[],"thinking":{"type":"enabled","budget_tokens":4096},"output_config":{"effort":"low"}}"#.as_slice(),
            br#"{"model":"m","max_tokens":4096,"messages":[],"output_config":{"effort":"MAX"}}"#.as_slice(),
            br#"{"model":"m","max_tokens":4096,"messages":[],"output_config":{"effort":"unknown"}}"#.as_slice(),
        ];
        for round in 0..5 {
            for fixture in fixtures {
                let error = parse_messages_payload(&Bytes::copy_from_slice(fixture), "req_invalid")
                    .expect_err("invalid reasoning controls must be rejected");
                assert_eq!(error.status, StatusCode::BAD_REQUEST, "round {round}");
            }
        }
    }

    #[test]
    fn local_pool_fast_fail_maps_only_terminal_or_temporary_pool_states_for_five_rounds() {
        for round in 0..5 {
            for kind in [
                LocalPoolRouteStateKind::NoCredentials,
                LocalPoolRouteStateKind::AllDisabled,
                LocalPoolRouteStateKind::ProxyBlocked,
            ] {
                let (status, error_type, message, reason, retry_after) =
                    local_pool_fast_fail_response_parts(kind, None)
                        .unwrap_or_else(|| panic!("round {round}: {kind:?} should fast-fail"));
                assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "round {round}");
                assert_eq!(error_type, "api_error", "round {round}");
                assert_eq!(
                    message,
                    envelope::PUBLIC_ACCOUNT_UNAVAILABLE_MESSAGE,
                    "round {round}"
                );
                assert_eq!(
                    reason,
                    RequestRejectionReason::LocalPoolUnavailable,
                    "round {round}"
                );
                assert_eq!(retry_after, None, "round {round}");
            }

            let (status, error_type, message, reason, retry_after) =
                local_pool_fast_fail_response_parts(
                    LocalPoolRouteStateKind::SchedulerRedisDegraded,
                    Some(4),
                )
                .expect("scheduler degraded should fast-fail");
            assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "round {round}");
            assert_eq!(error_type, "rate_limit_error", "round {round}");
            assert!(
                message.contains("Retry after 4 seconds."),
                "round {round}: {message}"
            );
            assert_eq!(
                reason,
                RequestRejectionReason::LocalPoolTemporaryUnavailable,
                "round {round}"
            );
            assert_eq!(retry_after, Some(4), "round {round}");

            let (status, error_type, message, reason, retry_after) =
                local_pool_fast_fail_response_parts(LocalPoolRouteStateKind::RiskCircuitOpen, None)
                    .expect("risk circuit should fast-fail");
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "round {round}");
            assert_eq!(error_type, "api_error", "round {round}");
            assert_eq!(
                message,
                envelope::PUBLIC_TEMPORARY_FAILURE_MESSAGE,
                "round {round}"
            );
            assert_eq!(
                reason,
                RequestRejectionReason::LocalPoolTemporaryUnavailable,
                "round {round}"
            );
            assert_eq!(retry_after, Some(1), "round {round}");
        }
    }

    #[test]
    fn local_pool_fast_fail_does_not_preempt_waitable_or_model_states_for_five_rounds() {
        for round in 0..5 {
            for kind in [
                LocalPoolRouteStateKind::Ready,
                LocalPoolRouteStateKind::NoModelCompatible,
                LocalPoolRouteStateKind::AllCoolingDown,
                LocalPoolRouteStateKind::CapacityFull,
            ] {
                assert!(
                    local_pool_fast_fail_response_parts(kind, Some(2)).is_none(),
                    "round {round}: {kind:?} must continue through normal parsing/routing"
                );
            }
        }
    }
}
