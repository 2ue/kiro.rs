use super::*;
use crate::anthropic::model_capabilities::ModelResolutionSource;
use crate::anthropic::prompt_cache::PromptCacheTracker;
use crate::anthropic::prompt_cache_creation_control::PromptCacheCreationController;
use crate::anthropic::types::{Message, OutputConfig};
use crate::anthropic::usage::UsageRecorder;

fn messages_request_for_model(model: &str) -> MessagesRequest {
    MessagesRequest {
        model: model.to_string(),
        max_tokens: 16,
        messages: vec![Message {
            role: "user".to_string(),
            content: json!("hello"),
        }],
        stream: false,
        system: None,
        tools: None,
        tool_choice: None,
        thinking: None,
        output_config: None,
        output_format: None,
        metadata: None,
        stop_sequences: None,
    }
}

#[test]
fn defined_cache_route_requires_explicit_configuration() {
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        Arc::new(UsageRecorder::new(10)),
        Arc::new(PromptCacheTracker::default()),
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::HighCache,
        0.98,
        CompatProfile::ClaudeCode,
    )
    .with_defined_cache_routes(vec!["/dfcache/cc".to_string()]);

    assert_eq!(
        resolve_defined_cache_route(&state, "cc").unwrap(),
        "/dfcache/cc"
    );
    assert!(resolve_defined_cache_route(&state, "aa").is_err());
    assert!(resolve_defined_cache_route(&state, "aa/b").is_err());
}

#[test]
fn external_route_model_resolution_preserves_caller_model_for_external_mapping() {
    let resolved = external_request_model_resolution("claude-opus-4-6-thinking");

    assert_eq!(resolved.source, ModelResolutionSource::PassThrough);
    assert_eq!(
        resolved.upstream_model.as_deref(),
        Some("claude-opus-4-6-thinking")
    );
    assert!(resolved.note.is_none());
}

#[test]
fn models_response_is_a_superset_of_the_claude_code_models_shape() {
    let models = crate::anthropic::model_capabilities::ModelCapabilitiesCatalog::default()
        .anthropic_models();
    let value = serde_json::to_value(ModelsResponse::from_models(models)).unwrap();
    assert_eq!(value["object"], "list");
    assert_eq!(value["has_more"], false);
    let data = value["data"].as_array().expect("data");
    assert!(!data.is_empty());
    assert_eq!(value["first_id"], data[0]["id"]);
    assert_eq!(value["last_id"], data[data.len() - 1]["id"]);
    for model in data {
        assert_eq!(model["type"], "model", "{model}");
        assert!(model["display_name"].is_string(), "{model}");
        let created_at = model["created_at"].as_str().expect("created_at");
        assert!(
            chrono::DateTime::parse_from_rfc3339(created_at).is_ok(),
            "{model}"
        );
    }
    let again = serde_json::to_value(ModelsResponse::from_models(
        crate::anthropic::model_capabilities::ModelCapabilitiesCatalog::default()
            .anthropic_models(),
    ))
    .unwrap();
    assert_eq!(value, again, "model metadata must be stable across calls");
}

#[tokio::test]
async fn oversized_current_image_response_is_official_400_without_retry() {
    let violation = crate::anthropic::payload_guard::CurrentImageTooLarge {
        path: "messages.0.content.1.image.source.base64".to_string(),
        bytes: 5_763_023,
        max_bytes: 5_242_880,
    };
    let response = current_image_too_large_response(&violation);

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response
            .headers()
            .get("x-should-retry")
            .and_then(|value| value.to_str().ok()),
        Some("false")
    );
    let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("body");
    let body: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(
        body["error"]["message"],
        "messages.0.content.1.image.source.base64: image exceeds 5 MB maximum: 5763023 bytes > 5242880 bytes"
    );
}

#[tokio::test]
async fn parse_messages_payload_rejects_empty_model_before_routing() {
    for model in ["", "   "] {
        let body = Bytes::from(
            json!({
                "model": model,
                "max_tokens": 16,
                "messages": [{"role": "user", "content": "hello"}]
            })
            .to_string(),
        );

        let response = request_entry::parse_messages_payload(&body, "req_test_empty_model")
            .expect_err("empty model rejected")
            .to_response(&envelope::request_id());

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read error body");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("json envelope");
        assert_eq!(value["error"]["type"], "invalid_request_error");
        assert_eq!(
            value["error"]["message"],
            "model: field is required and cannot be empty"
        );
        assert!(
            value["request_id"]
                .as_str()
                .is_some_and(|request_id| request_id.starts_with("req_01"))
        );
    }
}

#[test]
fn raw_external_route_request_applies_non_stream_skip_cache_route() {
    let mut cache_policy = CachePolicyConfig::default();
    cache_policy.path_overrides.insert(
        "/cc".to_string(),
        CacheRoutePolicyPatch {
            cache_type: Some(PromptCacheStrategyType::CurrentHighCache),
            reported_usage: Some(ReportedUsagePathPolicy {
                skip_non_stream_usage_projection: true,
                input: ReportedUsageFieldPolicy::sample_input_max(1),
                ..ReportedUsagePathPolicy::default()
            }),
            ..CacheRoutePolicyPatch::default()
        },
    );
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        Arc::new(UsageRecorder::new(10)),
        Arc::new(PromptCacheTracker::default()),
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::HighCache,
        0.98,
        CompatProfile::ClaudeCode,
    )
    .with_cache_policy(cache_policy);
    let runtime_config = RequestRuntimeConfig::from_app_state(&state);
    let cache_route = runtime_config.cache_policy_for_path("/cc/v1/messages");
    assert_eq!(
        cache_route.policy.cache_type,
        PromptCacheStrategyType::CurrentHighCache
    );
    assert!(
        cache_route
            .policy
            .reported_usage
            .skip_non_stream_usage_projection
    );

    let non_stream_route = external_route_request_raw(
            &state,
            &runtime_config,
            &cache_route,
            HeaderMap::new(),
            Bytes::from_static(
                br#"{"model":"client-model","stream":false,"messages":[{"role":"user","content":"hello"}]}"#,
            ),
            "/cc/v1/messages",
            "req_raw_non_stream_skip".to_string(),
            Arc::new(InferenceAttemptBudget::new(4)),
            None,
        None,
        Arc::new(probe_raw_messages_body(&Bytes::from_static(br#"{"model":"client-model","stream":false,"messages":[{"role":"user","content":"hello"}]}"#))),
        );

    assert_eq!(
        non_stream_route.prompt_cache_strategy_type,
        PromptCacheStrategyType::NoCache
    );
    assert_eq!(
        non_stream_route.prompt_cache_simulation_mode,
        PromptCacheSimulationMode::Disabled
    );
    assert!(non_stream_route.prompt_cache_route_namespace.is_none());
    assert!(!non_stream_route.reported_usage.default.enabled);
    assert!(
        non_stream_route
            .reported_usage
            .default
            .skip_non_stream_usage_projection
    );

    let stream_route = external_route_request_raw(
            &state,
            &runtime_config,
            &cache_route,
            HeaderMap::new(),
            Bytes::from_static(
                br#"{"model":"client-model","stream":true,"messages":[{"role":"user","content":"hello"}]}"#,
            ),
            "/cc/v1/messages",
            "req_raw_stream_skip".to_string(),
            Arc::new(InferenceAttemptBudget::new(4)),
            None,
        None,
        Arc::new(probe_raw_messages_body(&Bytes::from_static(br#"{"model":"client-model","stream":true,"messages":[{"role":"user","content":"hello"}]}"#))),
        );

    assert_eq!(
        stream_route.prompt_cache_strategy_type,
        PromptCacheStrategyType::CurrentHighCache
    );
    assert!(stream_route.reported_usage.default.enabled);
    assert!(
        stream_route
            .reported_usage
            .default
            .skip_non_stream_usage_projection
    );
}

#[test]
fn raw_external_route_request_is_preparse_raw_only() {
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        Arc::new(UsageRecorder::new(10)),
        Arc::new(PromptCacheTracker::default()),
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::HighCache,
        0.98,
        CompatProfile::ClaudeCode,
    );
    let runtime_config = RequestRuntimeConfig::from_app_state(&state);
    let cache_route = runtime_config.cache_policy_for_path("/cc/v1/messages");
    let raw_body = Bytes::from_static(
        br#"{"model":"client-model","stream":true,"messages":[{"role":"user","content":"hello"}]}"#,
    );

    let route = external_route_request_raw(
        &state,
        &runtime_config,
        &cache_route,
        HeaderMap::new(),
        raw_body.clone(),
        "/cc/v1/messages",
        "req_raw_preparse_test".to_string(),
        Arc::new(InferenceAttemptBudget::new(4)),
        None,
        None,
        Arc::new(probe_raw_messages_body(&raw_body.clone())),
    );

    assert_eq!(route.raw_body, raw_body);
    assert_eq!(route.effective_raw_body, raw_body);
    assert!(route.payload.is_none());
    assert_eq!(
        route.body_mode_filter,
        Some(ExternalPoolRequestBodyMode::RawPassthrough)
    );
    assert_eq!(route.model_hint.as_deref(), Some("client-model"));
    assert_eq!(route.stream_hint, Some(true));
    assert_eq!(route.request_input_tokens, 0);
    assert!(!route.payload_guard_external_enabled);
}

#[test]
fn remote_url_safety_rejects_local_and_private_targets() {
    for url in [
        "http://localhost/image.png",
        "http://127.0.0.1/image.png",
        "http://10.0.0.5/image.png",
        "http://169.254.169.254/latest/meta-data",
        "http://[::1]/image.png",
    ] {
        assert!(
            body_processing::ensure_safe_remote_url(url).is_err(),
            "{url} should be blocked"
        );
    }
}

#[test]
fn synthetic_thinking_activation_never_forges_client_effort_five_rounds() {
    for round in 0..5 {
        for model in [
            "claude-opus-4-7-thinking",
            "claude-opus-4-8-thinking",
            "claude-opus-5-5-thinking",
            "claude-sonnet-5-5-thinking",
            "claude-haiku-5-5-thinking",
            "fable-thinking",
            "claude-fable-5-thinking",
            "claude-fable-5-1-thinking",
            "claude-sonnet-4-6-thinking",
            "claude-sonnet-5-thinking",
        ] {
            let mut payload = messages_request_for_model(model);
            override_thinking_from_model_name(&mut payload)
                .unwrap_or_else(|error| panic!("round {round}: {model}: {error}"));
            assert_eq!(
                payload
                    .thinking
                    .as_ref()
                    .map(|thinking| thinking.thinking_type.as_str()),
                Some("adaptive")
            );
            assert!(
                payload.output_config.is_none(),
                "round {round}: {model} omitted effort must remain omitted"
            );
        }

        let mut triggered = messages_request_for_model("claude-sonnet-4-6");
        triggered.messages[0].content = json!("ultrathink inspect this");
        let trigger_mode = ThinkingTriggerMode::RealRequest;
        apply_thinking_trigger_mode(&mut triggered, trigger_mode);
        assert_eq!(
            triggered
                .thinking
                .as_ref()
                .map(|thinking| thinking.thinking_type.as_str()),
            Some("adaptive")
        );
        assert!(triggered.output_config.is_none(), "round {round}");

        let mut explicit = messages_request_for_model("claude-opus-4-8-thinking");
        explicit.output_config = Some(OutputConfig {
            effort: Some("max".to_string()),
            format: None,
        });
        override_thinking_from_model_name(&mut explicit).expect("explicit effort override");
        assert_eq!(
            explicit
                .output_config
                .expect("explicit effort preserved")
                .effort
                .as_deref(),
            Some("max"),
            "round {round}"
        );
    }
}

#[test]
fn thinking_suffix_opus_4_7_uses_adaptive_by_default() {
    let mut payload = messages_request_for_model("claude-opus-4-7-thinking");

    override_thinking_from_model_name(&mut payload).expect("thinking model override");

    let thinking = payload.thinking.expect("thinking should be set");
    assert_eq!(thinking.thinking_type, "adaptive");
    assert!(payload.output_config.is_none());
}

#[test]
fn thinking_suffix_opus_alias_uses_adaptive_by_default() {
    let mut payload = messages_request_for_model("opus-thinking");

    override_thinking_from_model_name(&mut payload).expect("thinking model override");

    let thinking = payload.thinking.expect("thinking should be set");
    assert_eq!(thinking.thinking_type, "adaptive");
    assert!(payload.output_config.is_none());
}

#[test]
fn thinking_suffix_preserves_explicit_enabled_without_effort() {
    let mut payload = messages_request_for_model("claude-opus-4-7-thinking");
    payload.thinking = Some(Thinking {
        thinking_type: "enabled".to_string(),
        budget_tokens: 4096,
        display: None,
    });

    override_thinking_from_model_name(&mut payload).expect("thinking model override");

    let thinking = payload.thinking.expect("thinking should be preserved");
    assert_eq!(thinking.thinking_type, "enabled");
    assert_eq!(thinking.budget_tokens, 4096);
    assert!(payload.output_config.is_none());
}

#[test]
fn thinking_suffix_preserves_omitted_effort_for_explicit_adaptive() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6-thinking");
    payload.thinking = Some(Thinking {
        thinking_type: "adaptive".to_string(),
        budget_tokens: 4096,
        display: None,
    });

    override_thinking_from_model_name(&mut payload).expect("thinking model override");

    let thinking = payload.thinking.expect("thinking should be preserved");
    assert_eq!(thinking.thinking_type, "adaptive");
    assert_eq!(thinking.budget_tokens, 4096);
    assert!(payload.output_config.is_none());
}

#[test]
fn thinking_suffix_public_5x_models_use_adaptive_by_default() {
    for model in [
        "claude-sonnet-5-thinking",
        "claude-sonnet-5-5-thinking",
        "claude-opus-5-thinking",
        "claude-opus-5-5-thinking",
        "claude-opus-5.5[1m]-thinking",
        "claude-haiku-5-5-thinking",
        "fable-thinking",
        "claude-fable-5-thinking",
        "claude-fable-5-1-thinking",
        "claude-fable-5.1[1m]-thinking",
    ] {
        let mut payload = messages_request_for_model(model);

        override_thinking_from_model_name(&mut payload)
            .unwrap_or_else(|error| panic!("{model}: {error}"));

        let thinking = payload
            .thinking
            .unwrap_or_else(|| panic!("{model}: thinking should be set"));
        assert_eq!(thinking.thinking_type, "adaptive", "{model}");
        assert_eq!(thinking.budget_tokens, 0, "{model}");
        assert!(payload.output_config.is_none(), "{model}");
    }
}

#[test]
fn thinking_suffix_sonnet_4_5_rejects_when_minimum_budget_cannot_fit() {
    for max_tokens in [1, 1_024] {
        let mut payload = messages_request_for_model("claude-sonnet-4-5-thinking");
        payload.max_tokens = max_tokens;

        let error = override_thinking_from_model_name(&mut payload)
            .expect_err("enabled thinking requires room above the minimum budget");
        assert!(
            error.contains("greater than 1024"),
            "max_tokens={max_tokens}"
        );
        assert!(payload.thinking.is_none(), "max_tokens={max_tokens}");
    }

    let mut boundary = messages_request_for_model("claude-sonnet-4-5-thinking");
    boundary.max_tokens = 1_025;
    override_thinking_from_model_name(&mut boundary).expect("minimum thinking budget fits");
    assert_eq!(
        boundary.thinking.expect("boundary thinking").budget_tokens,
        1_024
    );
}

#[test]
fn thinking_suffix_sonnet_4_5_uses_enabled_by_default() {
    let mut payload = messages_request_for_model("claude-sonnet-4-5-thinking");
    payload.max_tokens = 32_768;

    override_thinking_from_model_name(&mut payload).expect("thinking model override");

    let thinking = payload.thinking.expect("thinking should be set");
    assert_eq!(thinking.thinking_type, "enabled");
    assert_eq!(thinking.budget_tokens, 20000);
    assert!(payload.output_config.is_none());
}

#[test]
fn thinking_suffix_sonnet_4_6_uses_adaptive_by_default() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6-thinking");

    override_thinking_from_model_name(&mut payload).expect("thinking model override");

    let thinking = payload.thinking.expect("thinking should be set");
    assert_eq!(thinking.thinking_type, "adaptive");
    assert!(payload.output_config.is_none());
}

#[test]
fn thinking_suffix_sonnet_alias_uses_adaptive_by_default() {
    let mut payload = messages_request_for_model("sonnet-thinking");

    override_thinking_from_model_name(&mut payload).expect("thinking model override");

    let thinking = payload.thinking.expect("thinking should be set");
    assert_eq!(thinking.thinking_type, "adaptive");
    assert!(payload.output_config.is_none());
}

#[test]
fn thinking_trigger_always_adds_adaptive_without_forging_explicit_effort() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    let trigger_mode = ThinkingTriggerMode::Always;

    apply_thinking_trigger_mode(&mut payload, trigger_mode);

    let thinking = payload
        .thinking
        .as_ref()
        .expect("thinking should be injected");
    assert_eq!(thinking.thinking_type, "adaptive");
    assert_eq!(thinking.budget_tokens, 0);
    assert!(payload.output_config.is_none());
}

#[test]
fn thinking_trigger_always_preserves_disabled() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    payload.thinking = Some(Thinking {
        thinking_type: "disabled".to_string(),
        budget_tokens: 4096,
        display: None,
    });
    let trigger_mode = ThinkingTriggerMode::Always;

    apply_thinking_trigger_mode(&mut payload, trigger_mode);

    let thinking = payload
        .thinking
        .as_ref()
        .expect("thinking should be preserved");
    assert_eq!(thinking.thinking_type, "disabled");
    assert_eq!(thinking.budget_tokens, 4096);
    assert!(payload.output_config.is_none());
}

#[test]
fn thinking_trigger_always_preserves_enabled_and_output_config() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    payload.thinking = Some(Thinking {
        thinking_type: "enabled".to_string(),
        budget_tokens: 4096,
        display: None,
    });
    payload.output_config = Some(OutputConfig {
        effort: Some("low".to_string()),
        format: None,
    });
    let trigger_mode = ThinkingTriggerMode::Always;

    apply_thinking_trigger_mode(&mut payload, trigger_mode);

    let thinking = payload
        .thinking
        .as_ref()
        .expect("thinking should be preserved");
    assert_eq!(thinking.thinking_type, "enabled");
    assert_eq!(thinking.budget_tokens, 4096);
    assert_eq!(
        payload
            .output_config
            .as_ref()
            .expect("output_config should be preserved")
            .effort
            .as_deref(),
        Some("low")
    );
}

#[test]
fn thinking_trigger_always_preserves_unknown_type_without_implicit_activation() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    payload.thinking = Some(Thinking {
        thinking_type: "mystery".to_string(),
        budget_tokens: 4096,
        display: None,
    });
    let trigger_mode = ThinkingTriggerMode::Always;

    apply_thinking_trigger_mode(&mut payload, trigger_mode);

    let thinking = payload
        .thinking
        .as_ref()
        .expect("thinking should be preserved for the entry validator to reject");
    assert_eq!(thinking.thinking_type, "mystery");
    assert_eq!(thinking.budget_tokens, 4096);
    assert!(payload.output_config.is_none());
}

#[test]
fn thinking_trigger_off_never_synthesizes_or_forces_thinking() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6-thinking");
    payload.messages[0].content = json!("ultrathink analyze this issue");
    let trigger_mode = ThinkingTriggerMode::Off;

    apply_thinking_trigger_mode(&mut payload, trigger_mode);

    assert!(
        payload.thinking.is_none(),
        "trigger off must not synthesize thinking from the prompt signal"
    );
}

#[test]
fn thinking_trigger_real_request_does_not_treat_think_hard_as_cli_keyword() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    payload.messages[0].content = json!("think hard about this issue");
    let trigger_mode = ThinkingTriggerMode::RealRequest;

    apply_thinking_trigger_mode(&mut payload, trigger_mode);

    assert!(payload.thinking.is_none());
    assert!(payload.output_config.is_none());
}

#[test]
fn thinking_trigger_real_request_ignores_old_user_signal_after_new_turn() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    payload.messages = vec![
        Message {
            role: "user".to_string(),
            content: json!("ultrathink analyze the old turn"),
        },
        Message {
            role: "assistant".to_string(),
            content: json!([{ "type": "text", "text": "done" }]),
        },
        Message {
            role: "user".to_string(),
            content: json!("new plain turn"),
        },
    ];
    let trigger_mode = ThinkingTriggerMode::RealRequest;

    apply_thinking_trigger_mode(&mut payload, trigger_mode);

    assert!(payload.thinking.is_none());
    assert!(payload.output_config.is_none());
}

#[test]
fn thinking_trigger_real_request_keeps_signal_across_tool_result_continuation() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    payload.messages = vec![
        Message {
            role: "user".to_string(),
            content: json!("ultrathink inspect the file then answer"),
        },
        Message {
            role: "assistant".to_string(),
            content: json!([{
                "type": "tool_use",
                "id": "toolu_1",
                "name": "Read",
                "input": {"file_path": "src/main.rs"}
            }]),
        },
        Message {
            role: "user".to_string(),
            content: json!([{
                "type": "tool_result",
                "tool_use_id": "toolu_1",
                "content": "file contents"
            }]),
        },
    ];
    let trigger_mode = ThinkingTriggerMode::RealRequest;

    apply_thinking_trigger_mode(&mut payload, trigger_mode);

    assert_eq!(
        payload
            .thinking
            .as_ref()
            .expect("current turn signal should survive tool-result continuation")
            .thinking_type,
        "adaptive"
    );
}

#[test]
fn thinking_trigger_real_request_preserves_empty_payload() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    let trigger_mode = ThinkingTriggerMode::RealRequest;

    apply_thinking_trigger_mode(&mut payload, trigger_mode);

    assert!(payload.thinking.is_none());
    assert!(payload.output_config.is_none());
}

#[test]
fn thinking_trigger_real_request_respects_explicit_disabled_over_cli_signal() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    payload.messages[0].content = json!("ultrathink analyze this issue");
    payload.thinking = Some(Thinking {
        thinking_type: "disabled".to_string(),
        budget_tokens: 4096,
        display: None,
    });
    let trigger_mode = ThinkingTriggerMode::RealRequest;

    apply_thinking_trigger_mode(&mut payload, trigger_mode);

    let thinking = payload
        .thinking
        .as_ref()
        .expect("disabled thinking should be preserved");
    assert_eq!(thinking.thinking_type, "disabled");
    assert_eq!(thinking.budget_tokens, 4096);
    assert!(payload.output_config.is_none());
}

#[test]
fn thinking_trigger_real_request_uses_claude_code_deep_reasoning_wrapper() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    payload.messages[0].content = json!(
        r#"<system-reminder>
The user included the keyword "ultrathink", requesting deeper reasoning on this turn. Reason as thoroughly as the task warrants.
</system-reminder>

Return a fix plan."#
    );
    let trigger_mode = ThinkingTriggerMode::RealRequest;

    apply_thinking_trigger_mode(&mut payload, trigger_mode);

    assert_eq!(
        payload
            .thinking
            .as_ref()
            .expect("Claude Code wrapper should inject thinking")
            .thinking_type,
        "adaptive"
    );
}

#[test]
fn thinking_trigger_real_request_uses_claude_code_ultrathink_signal() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    payload.messages[0].content = json!("ultrathink analyze this issue");
    let trigger_mode = ThinkingTriggerMode::RealRequest;

    apply_thinking_trigger_mode(&mut payload, trigger_mode);

    let thinking = payload
        .thinking
        .as_ref()
        .expect("ultrathink should inject adaptive thinking");
    assert_eq!(thinking.thinking_type, "adaptive");
    assert_eq!(thinking.budget_tokens, 0);
    assert!(payload.output_config.is_none());
}

#[tokio::test]
async fn messages_without_external_manager_return_503_for_raw_and_normalized_paths() {
    for normalized in [false, true] {
        let state = AppState::new(
            Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
            Arc::new(UsageRecorder::new(10)),
            Arc::new(PromptCacheTracker::default()),
            Arc::new(PromptCacheCreationController::default()),
            PromptCacheSimulationMode::Disabled,
            0.98,
            CompatProfile::ClaudeCode,
        );
        let raw = Bytes::from_static(br#"{"model":"claude-sonnet-4-6","max_tokens":16,"messages":[{"role":"user","content":"hello"}]}"#);
        let probe = Arc::new(probe_raw_messages_body(&raw));
        let response = post_messages_inner(
            state,
            HeaderMap::new(),
            raw.clone(),
            probe,
            raw,
            "/v1/messages".to_string(),
            Arc::new(InferenceAttemptBudget::new(4)),
            None,
            None,
            normalized,
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["type"], "api_error");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains(envelope::PUBLIC_PROVIDER_NOT_READY_MESSAGE)
        );
    }
}

#[tokio::test]
async fn count_tokens_keeps_local_estimation_available_without_an_external_manager() {
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        Arc::new(UsageRecorder::new(10)),
        Arc::new(PromptCacheTracker::default()),
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::Disabled,
        0.98,
        CompatProfile::ClaudeCode,
    );
    let payload = CountTokensRequest {
        model: "claude-sonnet-4-6".to_string(),
        messages: vec![Message {
            role: "user".to_string(),
            content: json!("hello"),
        }],
        system: None,
        tools: None,
    };
    let response = count_tokens_for_endpoint(state, payload, "/v1/messages/count_tokens").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert!(body["input_tokens"].as_i64().unwrap() > 0);
}
