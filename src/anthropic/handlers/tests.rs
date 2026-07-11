use super::*;
use crate::anthropic::cache::{self, CacheUsage};
use crate::anthropic::pricing::PricingCatalog;
use crate::anthropic::prompt_cache::PromptCacheTracker;
use crate::anthropic::prompt_cache_creation_control::PromptCacheCreationController;
use crate::anthropic::types::{Message, Metadata, SystemMessage};
use crate::anthropic::usage::{UsageRecordQuery, UsageRecorder};
use crate::kiro::call_trace::{
    AccountRejectReason, KiroCallError, SelectionFailureStage, SelectionFailureSummary,
};
use crate::kiro::model::events::MetadataTokenUsage;
use crate::model::config::{
    CachePointPolicyPatch, CachePolicyConfig, CacheRoutePolicyPatch, CacheSimulationPolicyPatch,
    PromptCacheCreationControlConfig, ReportedUsageFieldPolicy, ReportedUsagePathPolicy,
};
use serde_json::json;
use std::collections::BTreeMap;

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
        metadata: None,
    }
}

#[test]
fn anthropic_content_summary_skips_tool_result_text_for_latest_user() {
    let content = json!([
        {"type": "text", "text": "please answer this"},
        {
            "type": "tool_result",
            "tool_use_id": "toolu_1",
            "content": [{"type": "text", "text": "large command output"}]
        }
    ]);

    let summary = summarize_anthropic_content(&content);

    assert_eq!(summary.kind, "array");
    assert_eq!(summary.text.bytes, "please answer this".len());
    assert_eq!(summary.text.chars, "please answer this".chars().count());
    assert_eq!(summary.text.segments, 1);
    assert_eq!(
        summary.text.hash,
        Some(short_text_hash("please answer this"))
    );
    assert_eq!(summary.tool_result_count, 1);
}

#[test]
fn count_tool_use_blocks_counts_assistant_tool_uses_without_text_hashing() {
    let content = json!([
        {"type": "text", "text": "I will call a tool."},
        {"type": "tool_use", "id": "toolu_1", "name": "Read", "input": {"file_path": "README.md"}},
        {"type": "tool_use", "id": "toolu_2", "name": "Grep", "input": {"pattern": "kiro"}}
    ]);

    assert_eq!(count_tool_use_blocks(&content), 2);
}

#[test]
fn json_stream_sniffer_detects_request_body_invalid_exception() {
    let mut sniffer = JsonStreamErrorSniffer::new(Some("application/json; charset=utf-8"));

    let result = sniffer.inspect(Bytes::from_static(
        br#"{"message":"Invalid tool use format.","reason":"REQUEST_BODY_INVALID"}"#,
    ));

    match result {
        JsonStreamSniffResult::Error(error) => {
            assert_eq!(error.error_type, "invalid_request_error");
            assert!(error.internal_detail.contains("REQUEST_BODY_INVALID"));
            assert!(error.internal_detail.contains("Invalid tool use format."));
            assert!(error.body_preview.contains("Invalid tool use format."));
        }
        _ => panic!("expected JSON stream error"),
    }
}

#[test]
fn json_stream_sniffer_passes_binary_eventstream_mislabeled_as_json() {
    let mut sniffer = JsonStreamErrorSniffer::new(Some("application/json"));
    let chunk = Bytes::from_static(&[0, 0, 0, 16, 0, 0, 0, 0]);

    match sniffer.inspect(chunk.clone()) {
        JsonStreamSniffResult::Pass(passed) => assert_eq!(passed, chunk),
        _ => panic!("expected binary eventstream chunk to pass through"),
    }
}

#[test]
fn json_stream_sniffer_accumulates_split_json_exception() {
    let mut sniffer = JsonStreamErrorSniffer::new(Some("application/json"));

    assert!(matches!(
        sniffer.inspect(Bytes::from_static(br#"{"message":"Too many"#)),
        JsonStreamSniffResult::Pending
    ));

    match sniffer.inspect(Bytes::from_static(
        br#" requests","code":"ThrottlingException"}"#,
    )) {
        JsonStreamSniffResult::Error(error) => {
            assert_eq!(error.error_type, "rate_limit_error");
            assert!(error.internal_detail.contains("ThrottlingException"));
            assert!(error.internal_detail.contains("Too many requests"));
        }
        _ => panic!("expected split JSON stream error"),
    }
}

#[test]
fn claude_code_noop_delta_keepalive_is_version_gated() {
    assert_eq!(
        extract_claude_code_cli_version("claude-cli/2.1.197 (external, cli)"),
        Some("2.1.197")
    );
    assert!(should_use_claude_code_noop_delta_keepalive(Some(
        "claude-cli/2.1.193 (external, cli)"
    )));
    assert!(should_use_claude_code_noop_delta_keepalive(Some(
        "Claude-CLI/2.1.197 (Claude Code)"
    )));
    assert!(!should_use_claude_code_noop_delta_keepalive(Some(
        "claude-cli/2.1.192 (external, cli)"
    )));
    assert!(!should_use_claude_code_noop_delta_keepalive(Some(
        "curl/8.0"
    )));
    assert!(!should_use_claude_code_noop_delta_keepalive(None));
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

        let response = parse_messages_payload(&body).expect_err("empty model rejected");

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
fn defined_cache_route_requires_explicit_configuration() {
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        true,
        Arc::new(UsageRecorder::new(10)),
        Arc::new(PromptCacheTracker::default()),
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::HighCache,
        0.98,
        CompatProfile::ClaudeCode,
        false,
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
fn raw_external_route_request_is_preparse_raw_only() {
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        true,
        Arc::new(UsageRecorder::new(10)),
        Arc::new(PromptCacheTracker::default()),
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::HighCache,
        0.98,
        CompatProfile::ClaudeCode,
        false,
    );
    let runtime_config = RequestRuntimeConfig::from_app_state(&state);
    let cache_route = runtime_config.cache_policy_for_path("/cc/v1/messages");
    let raw_body = Bytes::from_static(
        br#"{"model":"client-model","stream":true,"messages":[{"role":"user","content":"hello"}]}"#,
    );

    let route = raw_external_route_request(
        &state,
        &runtime_config,
        &cache_route,
        HeaderMap::new(),
        raw_body.clone(),
        "/cc/v1/messages",
        "req_raw_preparse_test".to_string(),
        UsageRouteSubtype::ExternalFallbackPreflight,
        Some("local_capacity_full".to_string()),
        None,
        Some(json!({"preflightStage":"before_parse"})),
    );

    assert_eq!(route.raw_body, raw_body);
    assert!(route.payload.is_none());
    assert_eq!(
        route.body_mode_filter,
        Some(ExternalPoolRequestBodyMode::RawPassthrough)
    );
    assert_eq!(route.model_hint.as_deref(), Some("client-model"));
    assert_eq!(route.stream_hint, Some(true));
    assert_eq!(route.request_input_tokens, 0);
    assert!(!route.payload_guard_external_enabled);
    assert_eq!(
        route
            .local_preflight
            .as_ref()
            .and_then(|value| value.get("preflightStage"))
            .and_then(|value| value.as_str()),
        Some("before_parse")
    );
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
        true,
        Arc::new(UsageRecorder::new(10)),
        Arc::new(PromptCacheTracker::default()),
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::HighCache,
        0.98,
        CompatProfile::ClaudeCode,
        false,
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

    let non_stream_route = raw_external_route_request(
            &state,
            &runtime_config,
            &cache_route,
            HeaderMap::new(),
            Bytes::from_static(
                br#"{"model":"client-model","stream":false,"messages":[{"role":"user","content":"hello"}]}"#,
            ),
            "/cc/v1/messages",
            "req_raw_non_stream_skip".to_string(),
            UsageRouteSubtype::ExternalDirectPolicy,
            None,
            Some("direct_policy".to_string()),
            None,
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

    let stream_route = raw_external_route_request(
            &state,
            &runtime_config,
            &cache_route,
            HeaderMap::new(),
            Bytes::from_static(
                br#"{"model":"client-model","stream":true,"messages":[{"role":"user","content":"hello"}]}"#,
            ),
            "/cc/v1/messages",
            "req_raw_stream_skip".to_string(),
            UsageRouteSubtype::ExternalDirectPolicy,
            None,
            Some("direct_policy".to_string()),
            None,
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

fn runtime_config_for_payload_guard(
    mode: PayloadGuardMode,
    enabled: bool,
    max_bytes: usize,
) -> RequestRuntimeConfig {
    RequestRuntimeConfig {
        extract_thinking: true,
        thinking_trigger_mode: ThinkingTriggerMode::RealRequest,
        prompt_cache_simulation_mode: PromptCacheSimulationMode::HighCache,
        prompt_cache_target_read_ratio: 0.98,
        prompt_cache_token_scale: 1.0,
        prompt_cache_max_simulated_input_tokens: 0,
        prompt_cache_cap_jitter_min_tokens: 0,
        prompt_cache_cap_jitter_max_tokens: 0,
        prompt_cache_scale_min_input_tokens: 0,
        prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
        prompt_cache_bounds: PromptCacheBounds::default(),
        reported_usage: ReportedUsageConfig::default(),
        cache_policy: CachePolicyConfig::default(),
        defined_cache_routes: Vec::new(),
        compat_profile: CompatProfile::ClaudeCode,
        model_resolution_mode: ModelResolutionMode::Compatible,
        model_mapping: ModelMappingConfig::default(),
        expose_proxy_warnings: false,
        payload_guard_enabled: enabled,
        payload_guard_mode: mode,
        payload_guard_max_bytes: max_bytes,
        payload_guard_safety_margin_bytes: 0,
        payload_guard_trim_history: true,
        payload_guard_external_enabled: true,
        kiro_cache_point_enabled: false,
        kiro_cache_point_tools_only: true,
        kiro_cache_point_record_plan: true,
        kiro_upstream_stream_idle_timeout_secs: 180,
        image_processing: ImageProcessingConfig::default(),
        body_conversion: BodyConversionConfig::default(),
        missing_max_tokens: MissingMaxTokensConfig::default(),
        payload_shaping: PayloadShapingConfig::default(),
        external_pools: ExternalPoolsConfig::default(),
    }
}

#[test]
fn on_too_long_initial_guard_repairs_without_size_trimming() {
    let runtime_config =
        runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);

    let initial = runtime_config.initial_payload_guard_config();

    assert!(initial.enabled);
    assert_eq!(initial.max_bytes, 0);
    assert!(!initial.trim_history);
    assert!(runtime_config.too_long_retry_enabled());
    assert_eq!(runtime_config.payload_guard_config().max_bytes, 460_800);
    assert!(runtime_config.payload_guard_config().trim_history);
}

#[test]
fn payload_guard_safety_margin_reduces_effective_size_target() {
    let mut runtime_config =
        runtime_config_for_payload_guard(PayloadGuardMode::Preemptive, true, 460_800);
    runtime_config.payload_guard_safety_margin_bytes = 32 * 1024;

    assert_eq!(runtime_config.payload_guard_config().max_bytes, 428_032);

    runtime_config.payload_guard_max_bytes = 0;
    assert_eq!(runtime_config.payload_guard_config().max_bytes, 0);
}

#[test]
fn on_too_long_retry_requires_enabled_guard_and_positive_limit() {
    assert!(
        !runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, false, 460_800)
            .too_long_retry_enabled()
    );
    assert!(
        !runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 0)
            .too_long_retry_enabled()
    );
    assert!(
        !runtime_config_for_payload_guard(PayloadGuardMode::Preemptive, true, 460_800)
            .too_long_retry_enabled()
    );
}

#[test]
fn payload_guard_retry_treats_large_improper_request_as_possible_size_error() {
    assert!(should_retry_payload_guard_after_error(
        r#"400 Bad Request {"message":"Input is too long.","reason":"CONTENT_LENGTH_EXCEEDS_THRESHOLD"}"#,
        100,
        460_800,
    ));
    assert!(should_retry_payload_guard_after_error(
        r#"400 Bad Request {"message":"Improperly formed request.","reason":null}"#,
        700_000,
        460_800,
    ));
    assert!(!should_retry_payload_guard_after_error(
        r#"400 Bad Request {"message":"Improperly formed request.","reason":null}"#,
        100_000,
        460_800,
    ));
    assert!(!should_retry_payload_guard_after_error(
        r#"400 Bad Request {"message":"Improperly formed request.","reason":null}"#,
        700_000,
        0,
    ));
}

#[test]
fn request_body_invalid_tool_format_is_bad_request_diagnostic_error() {
    let message =
        r#"400 Bad Request {"message":"Invalid tool use format.","reason":"REQUEST_BODY_INVALID"}"#;

    assert!(is_upstream_bad_request_error(message));
    assert!(is_upstream_tool_use_format_error(message));
    assert!(!should_retry_payload_guard_after_error(
        message, 100_000, 460_800,
    ));
}

#[test]
fn thinking_suffix_opus_4_7_uses_adaptive_by_default() {
    let mut payload = messages_request_for_model("claude-opus-4-7-thinking");

    override_thinking_from_model_name(&mut payload);

    let thinking = payload.thinking.expect("thinking should be set");
    assert_eq!(thinking.thinking_type, "adaptive");
    assert_eq!(
        payload
            .output_config
            .expect("output_config should be filled")
            .effort,
        "xhigh"
    );
}

#[test]
fn thinking_suffix_opus_alias_uses_adaptive_by_default() {
    let mut payload = messages_request_for_model("opus-thinking");

    override_thinking_from_model_name(&mut payload);

    let thinking = payload.thinking.expect("thinking should be set");
    assert_eq!(thinking.thinking_type, "adaptive");
    assert_eq!(
        payload
            .output_config
            .expect("output_config should be filled")
            .effort,
        "xhigh"
    );
}

#[test]
fn thinking_suffix_sonnet_4_6_uses_adaptive_by_default() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6-thinking");

    override_thinking_from_model_name(&mut payload);

    let thinking = payload.thinking.expect("thinking should be set");
    assert_eq!(thinking.thinking_type, "adaptive");
    assert_eq!(
        payload
            .output_config
            .expect("output_config should be filled")
            .effort,
        "high"
    );
}

#[test]
fn thinking_suffix_sonnet_alias_uses_adaptive_by_default() {
    let mut payload = messages_request_for_model("sonnet-thinking");

    override_thinking_from_model_name(&mut payload);

    let thinking = payload.thinking.expect("thinking should be set");
    assert_eq!(thinking.thinking_type, "adaptive");
    assert_eq!(
        payload
            .output_config
            .expect("output_config should be filled")
            .effort,
        "high"
    );
}

#[test]
fn thinking_suffix_sonnet_4_5_uses_enabled_by_default() {
    let mut payload = messages_request_for_model("claude-sonnet-4-5-thinking");

    override_thinking_from_model_name(&mut payload);

    let thinking = payload.thinking.expect("thinking should be set");
    assert_eq!(thinking.thinking_type, "enabled");
    assert_eq!(thinking.budget_tokens, 20000);
    assert!(payload.output_config.is_none());
}

#[test]
fn thinking_suffix_preserves_explicit_enabled_without_effort() {
    let mut payload = messages_request_for_model("claude-opus-4-7-thinking");
    payload.thinking = Some(Thinking {
        thinking_type: "enabled".to_string(),
        budget_tokens: 4096,
    });

    override_thinking_from_model_name(&mut payload);

    let thinking = payload.thinking.expect("thinking should be preserved");
    assert_eq!(thinking.thinking_type, "enabled");
    assert_eq!(thinking.budget_tokens, 4096);
    assert!(payload.output_config.is_none());
}

#[test]
fn thinking_suffix_fills_effort_for_explicit_adaptive() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6-thinking");
    payload.thinking = Some(Thinking {
        thinking_type: "adaptive".to_string(),
        budget_tokens: 4096,
    });

    override_thinking_from_model_name(&mut payload);

    let thinking = payload.thinking.expect("thinking should be preserved");
    assert_eq!(thinking.thinking_type, "adaptive");
    assert_eq!(thinking.budget_tokens, 4096);
    assert_eq!(
        payload
            .output_config
            .expect("output_config should be filled")
            .effort,
        "high"
    );
}

#[test]
fn thinking_trigger_real_request_preserves_empty_payload() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    let runtime_config =
        runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);

    apply_thinking_trigger_mode(&mut payload, &runtime_config);

    assert!(payload.thinking.is_none());
    assert!(payload.output_config.is_none());
    assert!(!should_force_visible_thinking(&payload, &runtime_config));
}

#[test]
fn thinking_trigger_real_request_uses_claude_code_ultrathink_signal() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    payload.messages[0].content = json!("ultrathink analyze this issue");
    let runtime_config =
        runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);

    apply_thinking_trigger_mode(&mut payload, &runtime_config);

    let thinking = payload
        .thinking
        .as_ref()
        .expect("ultrathink should inject adaptive thinking");
    assert_eq!(thinking.thinking_type, "adaptive");
    assert_eq!(thinking.budget_tokens, 0);
    assert_eq!(
        payload
            .output_config
            .as_ref()
            .expect("output_config should be filled")
            .effort,
        "high"
    );
    assert!(should_force_visible_thinking(&payload, &runtime_config));
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
    let runtime_config =
        runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);

    apply_thinking_trigger_mode(&mut payload, &runtime_config);

    assert_eq!(
        payload
            .thinking
            .as_ref()
            .expect("Claude Code wrapper should inject thinking")
            .thinking_type,
        "adaptive"
    );
    assert!(should_force_visible_thinking(&payload, &runtime_config));
}

#[test]
fn thinking_trigger_real_request_does_not_treat_think_hard_as_cli_keyword() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    payload.messages[0].content = json!("think hard about this issue");
    let runtime_config =
        runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);

    apply_thinking_trigger_mode(&mut payload, &runtime_config);

    assert!(payload.thinking.is_none());
    assert!(payload.output_config.is_none());
    assert!(!should_force_visible_thinking(&payload, &runtime_config));
}

#[test]
fn thinking_trigger_real_request_respects_explicit_disabled_over_cli_signal() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    payload.messages[0].content = json!("ultrathink analyze this issue");
    payload.thinking = Some(Thinking {
        thinking_type: "disabled".to_string(),
        budget_tokens: 4096,
    });
    let runtime_config =
        runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);

    apply_thinking_trigger_mode(&mut payload, &runtime_config);

    let thinking = payload
        .thinking
        .as_ref()
        .expect("disabled thinking should be preserved");
    assert_eq!(thinking.thinking_type, "disabled");
    assert_eq!(thinking.budget_tokens, 4096);
    assert!(payload.output_config.is_none());
    assert!(!should_force_visible_thinking(&payload, &runtime_config));
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
    let runtime_config =
        runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);

    apply_thinking_trigger_mode(&mut payload, &runtime_config);

    assert!(payload.thinking.is_none());
    assert!(payload.output_config.is_none());
    assert!(!should_force_visible_thinking(&payload, &runtime_config));
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
    let runtime_config =
        runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);

    apply_thinking_trigger_mode(&mut payload, &runtime_config);

    assert_eq!(
        payload
            .thinking
            .as_ref()
            .expect("current turn signal should survive tool-result continuation")
            .thinking_type,
        "adaptive"
    );
    assert!(should_force_visible_thinking(&payload, &runtime_config));
}

#[test]
fn thinking_trigger_always_adds_adaptive_and_effort() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    let mut runtime_config =
        runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);
    runtime_config.thinking_trigger_mode = ThinkingTriggerMode::Always;

    apply_thinking_trigger_mode(&mut payload, &runtime_config);

    let thinking = payload
        .thinking
        .as_ref()
        .expect("thinking should be injected");
    assert_eq!(thinking.thinking_type, "adaptive");
    assert_eq!(thinking.budget_tokens, 0);
    assert_eq!(
        payload
            .output_config
            .as_ref()
            .expect("output_config should be filled")
            .effort,
        "high"
    );
    assert!(should_force_visible_thinking(&payload, &runtime_config));
}

#[test]
fn thinking_trigger_always_preserves_disabled() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    payload.thinking = Some(Thinking {
        thinking_type: "disabled".to_string(),
        budget_tokens: 4096,
    });
    let mut runtime_config =
        runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);
    runtime_config.thinking_trigger_mode = ThinkingTriggerMode::Always;

    apply_thinking_trigger_mode(&mut payload, &runtime_config);

    let thinking = payload
        .thinking
        .as_ref()
        .expect("thinking should be preserved");
    assert_eq!(thinking.thinking_type, "disabled");
    assert_eq!(thinking.budget_tokens, 4096);
    assert!(payload.output_config.is_none());
    assert!(!should_force_visible_thinking(&payload, &runtime_config));
}

#[test]
fn thinking_trigger_always_preserves_enabled_and_output_config() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    payload.thinking = Some(Thinking {
        thinking_type: "enabled".to_string(),
        budget_tokens: 4096,
    });
    payload.output_config = Some(OutputConfig {
        effort: "low".to_string(),
    });
    let mut runtime_config =
        runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);
    runtime_config.thinking_trigger_mode = ThinkingTriggerMode::Always;

    apply_thinking_trigger_mode(&mut payload, &runtime_config);

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
            .effort,
        "low"
    );
    assert!(should_force_visible_thinking(&payload, &runtime_config));
}

#[test]
fn thinking_trigger_always_rewrites_unknown_type_to_adaptive() {
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    payload.thinking = Some(Thinking {
        thinking_type: "mystery".to_string(),
        budget_tokens: 4096,
    });
    let mut runtime_config =
        runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);
    runtime_config.thinking_trigger_mode = ThinkingTriggerMode::Always;

    apply_thinking_trigger_mode(&mut payload, &runtime_config);

    let thinking = payload
        .thinking
        .as_ref()
        .expect("thinking should be rewritten");
    assert_eq!(thinking.thinking_type, "adaptive");
    assert_eq!(thinking.budget_tokens, 0);
    assert_eq!(
        payload
            .output_config
            .as_ref()
            .expect("output_config should be filled")
            .effort,
        "high"
    );
    assert!(should_force_visible_thinking(&payload, &runtime_config));
}

#[test]
fn path_reported_usage_policy_samples_natural_usage() {
    let reported_usage_config = ReportedUsageConfig::default();
    let usage = CacheUsage {
        total_input_tokens: 100_000,
        input_tokens: 50_000,
        output_tokens: 1,
        cache_creation_input_tokens: 50_000,
        cache_read_input_tokens: 0,
        cache_creation_5m_input_tokens: 50_000,
        cache_creation_1h_input_tokens: 0,
    };
    let values: Vec<i32> = (0..24)
        .map(|seed| {
            let policy = reported_cache_usage_policy_for_path(
                "/cc/v1/messages",
                PromptCacheSimulationMode::HighCache,
                &reported_usage_config,
                seed,
            )
            .expect("policy should apply");
            usage
                .with_reported_cache_usage_policy(policy)
                .cache_creation_input_tokens
        })
        .collect();

    assert!(values.iter().all(|value| (1..=3_600).contains(value)));
    assert!(values.windows(2).any(|pair| pair[1] < pair[0]));
    assert!(values.iter().any(|value| value % 10 != 0));

    let reported = usage.with_reported_cache_usage_policy(
        reported_cache_usage_policy_for_path(
            "/cc/v1/messages",
            PromptCacheSimulationMode::HighCache,
            &reported_usage_config,
            9,
        )
        .expect("policy should apply"),
    );
    assert_eq!(reported.input_tokens, usage.input_tokens);
    assert_eq!(reported.cache_read_input_tokens, 0);
    assert!(reported.cache_creation_input_tokens > 0);
    assert_eq!(reported.output_tokens, 1);

    let raw_reported = usage.with_reported_cache_usage_policy_and_raw(
        reported_cache_usage_policy_for_path(
            "/cc/v1/messages",
            PromptCacheSimulationMode::HighCache,
            &reported_usage_config,
            9,
        )
        .expect("policy should apply"),
        cache::RawUsage::uncached(100_000, 1),
    );
    assert_eq!(raw_reported.input_tokens, 100_000);
    assert_eq!(raw_reported.cache_read_input_tokens, 0);
    assert!(raw_reported.cache_creation_input_tokens > 0);
}

#[test]
fn path_reported_usage_skip_non_stream_blocks_non_stream_only() {
    let policy = ReportedUsagePathPolicy {
        skip_non_stream_usage_projection: true,
        input: ReportedUsageFieldPolicy::sample_input_max(96),
        ..ReportedUsagePathPolicy::default()
    };

    let non_stream_policy = reported_cache_usage_policy_for_request(
        PromptCacheStrategyType::CurrentHighCache,
        PromptCacheSimulationMode::HighCache,
        &policy,
        7,
        false,
    );
    assert!(non_stream_policy.is_none());

    let stream_policy = reported_cache_usage_policy_for_request(
        PromptCacheStrategyType::CurrentHighCache,
        PromptCacheSimulationMode::HighCache,
        &policy,
        7,
        true,
    );
    assert!(stream_policy.is_some());
}

#[test]
fn path_reported_usage_skip_non_stream_disables_local_cache_route_only_for_non_stream() {
    let prompt_cache = Arc::new(PromptCacheTracker::default());
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let mut cache_policy = CachePolicyConfig::default();
    cache_policy.path_overrides.insert(
        "/kiro/v1/messages".to_string(),
        CacheRoutePolicyPatch {
            cache_type: Some(PromptCacheStrategyType::KiroRsTool),
            reported_usage: Some(ReportedUsagePathPolicy {
                skip_non_stream_usage_projection: true,
                ..ReportedUsagePathPolicy::default()
            }),
            ..CacheRoutePolicyPatch::default()
        },
    );
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        true,
        usage_recorder,
        prompt_cache,
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::Disabled,
        0.95,
        CompatProfile::ClaudeCode,
        false,
    )
    .with_cache_policy(cache_policy);

    let route =
        RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/kiro/v1/messages");
    assert_eq!(route.policy.cache_type, PromptCacheStrategyType::KiroRsTool);
    assert!(route.policy.reported_usage.skip_non_stream_usage_projection);

    let stream_route = cache_route_for_request_stream(route.clone(), true);
    assert_eq!(
        stream_route.policy.cache_type,
        PromptCacheStrategyType::KiroRsTool
    );
    assert!(
        stream_route
            .policy
            .reported_usage
            .skip_non_stream_usage_projection
    );

    let non_stream_route = cache_route_for_request_stream(route, false);
    assert_eq!(
        non_stream_route.policy.cache_type,
        PromptCacheStrategyType::NoCache
    );
    assert!(non_stream_route.namespace.is_none());
    assert!(!non_stream_route.policy.simulation.enabled);
    assert!(!non_stream_route.policy.creation_control.enabled);
    assert!(!non_stream_route.policy.cache_point.enabled);
    assert!(!non_stream_route.policy.reported_usage.enabled);
    assert!(
        non_stream_route
            .policy
            .reported_usage
            .skip_non_stream_usage_projection
    );
}

#[test]
fn reported_usage_rewrite_shapes_high_cache_downstream_usage() {
    let reported_usage_config = ReportedUsageConfig::default();
    let v1_policy = reported_cache_usage_policy_for_path(
        "/v1/messages",
        PromptCacheSimulationMode::HighCache,
        &reported_usage_config,
        0,
    )
    .expect("default policy should apply");
    let unchanged_usage = CacheUsage {
        total_input_tokens: 100_000,
        input_tokens: 10_000,
        output_tokens: 1,
        cache_creation_input_tokens: 50_000,
        cache_read_input_tokens: 40_000,
        cache_creation_5m_input_tokens: 50_000,
        cache_creation_1h_input_tokens: 0,
    };
    let v1_reported = unchanged_usage
        .with_reported_cache_usage_policy_and_raw(v1_policy, cache::RawUsage::uncached(100_000, 1));
    assert_eq!(v1_reported.input_tokens, 100_000);
    assert_eq!(v1_reported.output_tokens, 1);
    assert_eq!(
        v1_reported.cache_creation_input_tokens,
        unchanged_usage.cache_creation_input_tokens
    );
    assert_eq!(
        v1_reported.cache_read_input_tokens,
        unchanged_usage.cache_read_input_tokens
    );
    assert_eq!(
        reported_cache_usage_policy_for_path(
            "/cc/v1/messages",
            PromptCacheSimulationMode::Disabled,
            &reported_usage_config,
            0,
        ),
        None
    );

    let prompt_cache = Arc::new(PromptCacheTracker::default());
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let usage_context = RequestUsageContext {
        recorder: usage_recorder.clone(),
        tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
        prompt_cache,
        prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
        pricing_catalog: Arc::new(PricingCatalog::new()),
        request_id: "req_reported_limit".to_string(),
        error_id: "req_01reported_limit".to_string(),
        endpoint: "/cc/v1/messages".to_string(),
        stream: false,
        model: "claude-sonnet-4-6".to_string(),
        upstream_model: None,
        model_resolution_source: None,
        model_resolution_note: None,
        requested_max_tokens: 0,
        downstream_stop_reason: Arc::new(Mutex::new(None)),
        conversation_id: Some("session-limit".to_string()),
        prompt_cache_scope_conversation_id: Some("session-limit".to_string()),
        input_tokens: 100_000,
        context_window_tokens: 200_000,
        prompt_cache_profile: None,
        kiro_rs_tool_prompt_cache_plan: None,
        prompt_cache_route_namespace: None,
        prompt_cache_strategy_type: PromptCacheStrategyType::CurrentHighCache,
        simulation_mode: PromptCacheSimulationMode::HighCache,
        prompt_cache_target_read_ratio: 0.95,
        prompt_cache_token_scale: 1.0,
        prompt_cache_max_simulated_input_tokens: 0,
        prompt_cache_cap_jitter_min_tokens: 0,
        prompt_cache_cap_jitter_max_tokens: 0,
        prompt_cache_scale_min_input_tokens: 0,
        prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
        prompt_cache_bounds: PromptCacheBounds::default(),
        reported_cache_usage_policy: reported_cache_usage_policy_for_path(
            "/cc/v1/messages",
            PromptCacheSimulationMode::HighCache,
            &reported_usage_config,
            7,
        ),
        simulated_usage: None,
        simulated_source: Some(UsageSource::LocalPromptCache),
        payload_breakdown: None,
        payload_guard_report: None,
        route_subtype_override: None,
        fallback_reason: None,
        local_preflight: None,
        external_attempts: Vec::new(),
        started_at: Instant::now(),
        first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        capacity_weight_units: Arc::new(AtomicU32::new(1)),
        latency: RequestLatencyTraceState::new(),
    };
    let usage = CacheUsage {
        total_input_tokens: 100_000,
        input_tokens: 10_000,
        output_tokens: 1,
        cache_creation_input_tokens: 50_000,
        cache_read_input_tokens: 40_000,
        cache_creation_5m_input_tokens: 50_000,
        cache_creation_1h_input_tokens: 0,
    };

    let capped = usage_context.reported_usage_for_downstream(usage, UsageSource::LocalPromptCache);
    assert!((0..=3_300).contains(&capped.cache_creation_input_tokens));
    assert!((1..=96).contains(&capped.input_tokens));
    assert_eq!(
        capped.cache_read_input_tokens,
        usage.cache_read_input_tokens.saturating_add(
            usage_context
                .input_tokens
                .saturating_sub(capped.input_tokens)
        )
    );
    assert!(capped.cache_read_input_tokens > usage.cache_read_input_tokens);

    let upstream_metadata =
        usage_context.reported_usage_for_downstream(usage, UsageSource::UpstreamMetadata);
    assert_eq!(upstream_metadata.input_tokens, usage_context.input_tokens);
    assert_eq!(upstream_metadata.cache_creation_input_tokens, 0);
    assert_eq!(upstream_metadata.cache_read_input_tokens, 0);

    let upstream_metadata_with_raw = usage_context.reported_usage_for_downstream_with_raw(
        usage,
        UsageSource::UpstreamMetadata,
        raw_usage_to_reported_raw(usage),
    );
    assert!((0..=3_300).contains(&upstream_metadata_with_raw.cache_creation_input_tokens));
    assert!((1..=96).contains(&upstream_metadata_with_raw.input_tokens));
    assert_eq!(
        upstream_metadata_with_raw.cache_read_input_tokens,
        usage.cache_read_input_tokens.saturating_add(
            usage
                .input_tokens
                .saturating_sub(upstream_metadata_with_raw.input_tokens)
        )
    );
    assert!(upstream_metadata_with_raw.cache_read_input_tokens > usage.cache_read_input_tokens);
}

#[test]
fn upstream_metadata_raw_usage_is_shaped_by_high_cache_reported_usage() {
    let usage_context = RequestUsageContext {
        recorder: Arc::new(UsageRecorder::new(10)),
        tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
        prompt_cache: Arc::new(PromptCacheTracker::default()),
        prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
        pricing_catalog: Arc::new(PricingCatalog::new()),
        request_id: "req_upstream_raw_reported_limit".to_string(),
        error_id: "req_01upstream_raw_reported_limit".to_string(),
        endpoint: "/ha/v1/messages".to_string(),
        stream: false,
        model: "claude-haiku-4-5".to_string(),
        upstream_model: None,
        model_resolution_source: None,
        model_resolution_note: None,
        requested_max_tokens: 0,
        downstream_stop_reason: Arc::new(Mutex::new(None)),
        conversation_id: Some("session-upstream-raw-limit".to_string()),
        prompt_cache_scope_conversation_id: Some("session-upstream-raw-limit".to_string()),
        input_tokens: 1_234,
        context_window_tokens: 200_000,
        prompt_cache_profile: None,
        kiro_rs_tool_prompt_cache_plan: None,
        prompt_cache_route_namespace: None,
        prompt_cache_strategy_type: PromptCacheStrategyType::CurrentHighCache,
        simulation_mode: PromptCacheSimulationMode::HighCache,
        prompt_cache_target_read_ratio: 0.95,
        prompt_cache_token_scale: 1.0,
        prompt_cache_max_simulated_input_tokens: 0,
        prompt_cache_cap_jitter_min_tokens: 0,
        prompt_cache_cap_jitter_max_tokens: 0,
        prompt_cache_scale_min_input_tokens: 0,
        prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
        prompt_cache_bounds: PromptCacheBounds::default(),
        reported_cache_usage_policy: reported_cache_usage_policy(
            PromptCacheStrategyType::CurrentHighCache,
            PromptCacheSimulationMode::HighCache,
            &ReportedUsagePathPolicy {
                input: ReportedUsageFieldPolicy::sample_input_max(500),
                ..ReportedUsagePathPolicy::default()
            },
            11,
        ),
        simulated_usage: None,
        simulated_source: None,
        payload_breakdown: None,
        payload_guard_report: None,
        route_subtype_override: None,
        fallback_reason: None,
        local_preflight: None,
        external_attempts: Vec::new(),
        started_at: Instant::now(),
        first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        capacity_weight_units: Arc::new(AtomicU32::new(1)),
        latency: RequestLatencyTraceState::new(),
    };
    let raw_usage = CacheUsage {
        total_input_tokens: 1_234,
        input_tokens: 1_234,
        output_tokens: 7,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        cache_creation_5m_input_tokens: 0,
        cache_creation_1h_input_tokens: 0,
    };

    let reported =
        usage_context.reported_usage_for_downstream(raw_usage, UsageSource::UpstreamMetadata);

    assert_eq!(reported.input_tokens, raw_usage.input_tokens);
    assert_eq!(reported.cache_read_input_tokens, 0);
    assert_eq!(reported.cache_creation_input_tokens, 0);
    assert_eq!(reported.output_tokens, 7);
    assert_eq!(
        reported.total_input_tokens,
        reported
            .input_tokens
            .saturating_add(reported.cache_read_input_tokens)
            .saturating_add(reported.cache_creation_input_tokens)
    );
}

#[test]
fn cc_local_prompt_cache_stream_reported_usage_caps_prod_like_input() {
    let reported_usage_config = ReportedUsageConfig::default();
    let request_input_tokens = 17_241;
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let usage_context = RequestUsageContext {
        recorder: usage_recorder.clone(),
        tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
        prompt_cache: Arc::new(PromptCacheTracker::default()),
        prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
        pricing_catalog: Arc::new(PricingCatalog::new()),
        request_id: "req_prod_like_cc_reported_usage".to_string(),
        error_id: "req_01prod_like_cc_reported_usage".to_string(),
        endpoint: "/cc/v1/messages".to_string(),
        stream: true,
        model: "claude-opus-4-6".to_string(),
        upstream_model: Some("claude-opus-4.6".to_string()),
        model_resolution_source: None,
        model_resolution_note: None,
        requested_max_tokens: 0,
        downstream_stop_reason: Arc::new(Mutex::new(None)),
        conversation_id: Some("conversation-prod-like".to_string()),
        prompt_cache_scope_conversation_id: Some("conversation-prod-like".to_string()),
        input_tokens: request_input_tokens,
        context_window_tokens: 200_000,
        prompt_cache_profile: None,
        kiro_rs_tool_prompt_cache_plan: None,
        prompt_cache_route_namespace: None,
        prompt_cache_strategy_type: PromptCacheStrategyType::CurrentHighCache,
        simulation_mode: PromptCacheSimulationMode::HighCache,
        prompt_cache_target_read_ratio: 0.99,
        prompt_cache_token_scale: 2.0,
        prompt_cache_max_simulated_input_tokens: 300_000,
        prompt_cache_cap_jitter_min_tokens: 0,
        prompt_cache_cap_jitter_max_tokens: 0,
        prompt_cache_scale_min_input_tokens: 20_000,
        prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
        prompt_cache_bounds: PromptCacheBounds::default(),
        reported_cache_usage_policy: reported_cache_usage_policy_for_path(
            "/cc/v1/messages",
            PromptCacheSimulationMode::HighCache,
            &reported_usage_config,
            7,
        ),
        simulated_usage: Some(cache::CacheSimulation {
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 36_109,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
            target_cache_ratio: Some(0.99),
            amplification: None,
            split_cached_input: false,
            ..Default::default()
        }),
        simulated_source: Some(UsageSource::LocalPromptCache),
        payload_breakdown: None,
        payload_guard_report: None,
        route_subtype_override: None,
        fallback_reason: None,
        local_preflight: None,
        external_attempts: Vec::new(),
        started_at: Instant::now(),
        first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        capacity_weight_units: Arc::new(AtomicU32::new(1)),
        latency: RequestLatencyTraceState::new(),
    };
    let credential_usage =
        usage_context.attach_credential(Some(131), None, false, false, Vec::new());
    let prod_like_usage = CacheUsage {
        total_input_tokens: 57_499,
        input_tokens: 21_390,
        output_tokens: 6,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 36_109,
        cache_creation_5m_input_tokens: 0,
        cache_creation_1h_input_tokens: 0,
    };

    let reported = credential_usage.final_reported_usage_for_stream(
        prod_like_usage,
        None,
        true,
        request_input_tokens,
    );

    assert!((1..=96).contains(&reported.input_tokens));
    assert_eq!(reported.output_tokens, 6);
    assert_eq!(reported.cache_creation_input_tokens, 0);
    assert_eq!(
        reported.cache_read_input_tokens,
        prod_like_usage
            .cache_read_input_tokens
            .saturating_add(request_input_tokens.saturating_sub(reported.input_tokens))
    );
    let raw_usage = raw_usage_from_metadata_or_estimate(
        None,
        request_input_tokens,
        prod_like_usage.output_tokens,
    );

    credential_usage.record_success_reported(
        reported,
        UsageSource::LocalPromptCache,
        Some(raw_usage),
    );
    let records = usage_recorder.query(UsageRecordQuery::default());
    assert_eq!(records.total, 1);
    let record = records.records.first().expect("usage record should exist");
    assert_eq!(record.compat_input_tokens, reported.input_tokens);
    assert_eq!(record.output_tokens, 6);
    assert_eq!(
        record.cache_creation_input_tokens,
        reported.cache_creation_input_tokens
    );
    assert_eq!(
        record.cache_read_input_tokens,
        reported.cache_read_input_tokens
    );
    let raw_usage = record.raw_usage.expect("raw usage should be retained");
    assert_eq!(raw_usage.total_input_tokens, request_input_tokens);
    assert_eq!(raw_usage.input_tokens, request_input_tokens);
    assert_eq!(raw_usage.output_tokens, prod_like_usage.output_tokens);
    assert_eq!(raw_usage.cache_creation_input_tokens, 0);
    assert_eq!(raw_usage.cache_read_input_tokens, 0);
}

#[test]
fn success_usage_record_uses_raw_usage_for_actual_input_diagnostic() {
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let usage_context = RequestUsageContext {
        recorder: usage_recorder.clone(),
        tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
        prompt_cache: Arc::new(PromptCacheTracker::default()),
        prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
        pricing_catalog: Arc::new(PricingCatalog::new()),
        request_id: "req_context_actual_input".to_string(),
        error_id: "req_01context_actual_input".to_string(),
        endpoint: "/cc/v1/messages".to_string(),
        stream: false,
        model: "claude-sonnet-4-6".to_string(),
        upstream_model: None,
        model_resolution_source: None,
        model_resolution_note: None,
        requested_max_tokens: 0,
        downstream_stop_reason: Arc::new(Mutex::new(None)),
        conversation_id: Some("context-estimate-session".to_string()),
        prompt_cache_scope_conversation_id: None,
        input_tokens: 141,
        context_window_tokens: 200_000,
        prompt_cache_profile: None,
        kiro_rs_tool_prompt_cache_plan: None,
        prompt_cache_route_namespace: None,
        prompt_cache_strategy_type: PromptCacheStrategyType::NoCache,
        simulation_mode: PromptCacheSimulationMode::Disabled,
        prompt_cache_target_read_ratio: 0.95,
        prompt_cache_token_scale: 1.0,
        prompt_cache_max_simulated_input_tokens: 0,
        prompt_cache_cap_jitter_min_tokens: 0,
        prompt_cache_cap_jitter_max_tokens: 0,
        prompt_cache_scale_min_input_tokens: 0,
        prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
        prompt_cache_bounds: PromptCacheBounds::default(),
        reported_cache_usage_policy: None,
        simulated_usage: None,
        simulated_source: None,
        payload_breakdown: None,
        payload_guard_report: None,
        route_subtype_override: None,
        fallback_reason: None,
        local_preflight: None,
        external_attempts: Vec::new(),
        started_at: Instant::now(),
        first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        capacity_weight_units: Arc::new(AtomicU32::new(1)),
        latency: RequestLatencyTraceState::new(),
    };
    let credential_usage = usage_context.attach_credential(Some(9), None, false, false, Vec::new());
    let usage = CacheUsage {
        total_input_tokens: 4_275,
        input_tokens: 4_275,
        output_tokens: 1,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        cache_creation_5m_input_tokens: 0,
        cache_creation_1h_input_tokens: 0,
    };

    credential_usage.record_success_reported(usage, UsageSource::ContextEstimate, Some(usage));

    let records = usage_recorder.query(UsageRecordQuery::default());
    assert_eq!(records.total, 1);
    let record = records.records.first().expect("usage record should exist");
    assert_eq!(record.total_input_tokens, 4_275);
    assert_eq!(record.compat_input_tokens, 4_275);
    assert_eq!(record.cache_creation_input_tokens, 0);
    assert_eq!(record.cache_read_input_tokens, 0);
    let raw_usage = record.raw_usage.expect("raw usage should be retained");
    assert_eq!(raw_usage.total_input_tokens, 4_275);
}

#[test]
fn kiro_rs_tool_local_prompt_cache_uses_strategy_usage_without_legacy_reported_usage() {
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let usage_context = RequestUsageContext {
        recorder: usage_recorder.clone(),
        tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
        prompt_cache: Arc::new(PromptCacheTracker::default()),
        prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
        pricing_catalog: Arc::new(PricingCatalog::new()),
        request_id: "req_kiro_strategy_reported_usage".to_string(),
        error_id: "req_01kiro_strategy_reported_usage".to_string(),
        endpoint: "/kiro/v1/messages".to_string(),
        stream: false,
        model: "claude-sonnet-4-6".to_string(),
        upstream_model: None,
        model_resolution_source: None,
        model_resolution_note: None,
        requested_max_tokens: 0,
        downstream_stop_reason: Arc::new(Mutex::new(None)),
        conversation_id: Some("conversation-kiro-strategy".to_string()),
        prompt_cache_scope_conversation_id: Some("conversation-kiro-strategy".to_string()),
        input_tokens: 100_000,
        context_window_tokens: 200_000,
        prompt_cache_profile: None,
        kiro_rs_tool_prompt_cache_plan: None,
        prompt_cache_route_namespace: Some("/kiro".to_string()),
        prompt_cache_strategy_type: PromptCacheStrategyType::KiroRsTool,
        simulation_mode: PromptCacheSimulationMode::Disabled,
        prompt_cache_target_read_ratio: 0.5,
        prompt_cache_token_scale: 3.0,
        prompt_cache_max_simulated_input_tokens: 300_000,
        prompt_cache_cap_jitter_min_tokens: 0,
        prompt_cache_cap_jitter_max_tokens: 0,
        prompt_cache_scale_min_input_tokens: 0,
        prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
        prompt_cache_bounds: PromptCacheBounds::default(),
        reported_cache_usage_policy: reported_cache_usage_policy(
            PromptCacheStrategyType::KiroRsTool,
            PromptCacheSimulationMode::Disabled,
            &ReportedUsagePathPolicy {
                input: ReportedUsageFieldPolicy::sample_input_max(96),
                ..ReportedUsagePathPolicy::default()
            },
            7,
        ),
        simulated_usage: Some(cache::CacheSimulation {
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 60_000,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
            target_cache_ratio: None,
            amplification: None,
            split_cached_input: true,
            ..Default::default()
        }),
        simulated_source: Some(UsageSource::LocalPromptCache),
        payload_breakdown: None,
        payload_guard_report: None,
        route_subtype_override: None,
        fallback_reason: None,
        local_preflight: None,
        external_attempts: Vec::new(),
        started_at: Instant::now(),
        first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        capacity_weight_units: Arc::new(AtomicU32::new(1)),
        latency: RequestLatencyTraceState::new(),
    };

    assert!(usage_context.reported_cache_usage_policy().is_none());
    let usage = cache::build_usage_with_simulation_policy(
        None,
        100_000,
        6,
        usage_context.simulated_usage,
        should_build_local_prompt_cache_usage(
            usage_context.prompt_cache_strategy_type,
            usage_context.simulation_mode,
        ),
    );
    assert_eq!(usage.input_tokens, 40_000);
    assert_eq!(usage.cache_read_input_tokens, 60_000);

    let credential_usage =
        usage_context.attach_credential(Some(131), None, false, false, Vec::new());
    let reported =
        credential_usage.final_reported_usage_for_success(usage, UsageSource::LocalPromptCache);

    assert_eq!(reported.input_tokens, 40_000);
    assert_eq!(reported.cache_read_input_tokens, 60_000);
    assert_eq!(reported.output_tokens, 6);

    let raw_usage = raw_usage_from_metadata_or_estimate(None, 100_000, 6);
    credential_usage.record_success_reported(
        reported,
        UsageSource::LocalPromptCache,
        Some(raw_usage),
    );
    let records = usage_recorder.query(UsageRecordQuery::default());
    assert_eq!(records.total, 1);
    let record = records.records.first().expect("usage record should exist");
    assert_eq!(record.compat_input_tokens, 40_000);
    assert_eq!(record.cache_read_input_tokens, 60_000);
    let raw_usage = record.raw_usage.expect("raw usage should be retained");
    assert_eq!(raw_usage.input_tokens, 100_000);
    assert_eq!(raw_usage.cache_read_input_tokens, 0);
}

#[test]
fn first_token_detection_ignores_initial_empty_blocks() {
    assert!(!is_first_token_output_event(&SseEvent::new(
        "message_start",
        json!({"type": "message_start"})
    )));
    assert!(!is_first_token_output_event(&SseEvent::new(
        "content_block_start",
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "text", "text": ""}
        })
    )));
    assert!(is_first_token_output_event(&SseEvent::new(
        "content_block_delta",
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": "hello"}
        })
    )));
    assert!(is_first_token_output_event(&SseEvent::new(
        "content_block_start",
        json!({
            "type": "content_block_start",
            "index": 1,
            "content_block": {"type": "tool_use", "id": "toolu_1", "name": "read", "input": {}}
        })
    )));
}

#[test]
fn local_latency_trace_records_markers_without_changing_first_output_semantics() {
    let mut usage_context = RequestUsageContext {
        recorder: Arc::new(UsageRecorder::new(10)),
        tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
        prompt_cache: Arc::new(PromptCacheTracker::default()),
        prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
        pricing_catalog: Arc::new(PricingCatalog::new()),
        request_id: "req_latency_trace".to_string(),
        error_id: "req_01latency_trace".to_string(),
        endpoint: "/cc/v1/messages".to_string(),
        stream: true,
        model: "claude-opus-4-8".to_string(),
        upstream_model: Some("claude-opus-4.8".to_string()),
        model_resolution_source: None,
        model_resolution_note: None,
        requested_max_tokens: 0,
        downstream_stop_reason: Arc::new(Mutex::new(None)),
        conversation_id: Some("session-latency".to_string()),
        prompt_cache_scope_conversation_id: Some("session-latency".to_string()),
        input_tokens: 100,
        context_window_tokens: 200_000,
        prompt_cache_profile: None,
        kiro_rs_tool_prompt_cache_plan: None,
        prompt_cache_route_namespace: None,
        prompt_cache_strategy_type: PromptCacheStrategyType::CurrentHighCache,
        simulation_mode: PromptCacheSimulationMode::Disabled,
        prompt_cache_target_read_ratio: 0.95,
        prompt_cache_token_scale: 1.0,
        prompt_cache_max_simulated_input_tokens: 0,
        prompt_cache_cap_jitter_min_tokens: 0,
        prompt_cache_cap_jitter_max_tokens: 0,
        prompt_cache_scale_min_input_tokens: 0,
        prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
        prompt_cache_bounds: PromptCacheBounds::default(),
        reported_cache_usage_policy: None,
        simulated_usage: None,
        simulated_source: None,
        payload_breakdown: None,
        payload_guard_report: None,
        route_subtype_override: None,
        fallback_reason: None,
        local_preflight: None,
        external_attempts: Vec::new(),
        started_at: Instant::now(),
        first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        capacity_weight_units: Arc::new(AtomicU32::new(1)),
        latency: RequestLatencyTraceState::new(),
    };

    usage_context.mark_payload_guard_latency(Duration::from_millis(3));
    usage_context.mark_upstream_header();
    usage_context.mark_first_upstream_chunk();
    usage_context.mark_upstream_bytes_before_first_output(128);
    usage_context.mark_upstream_pending_chunk_before_first_output();
    usage_context.mark_upstream_frame_before_first_output();
    usage_context.mark_upstream_event_before_first_output(
        &Event::Metadata(crate::kiro::model::events::MetadataEvent::default()),
        0,
    );
    usage_context.mark_upstream_frame_before_first_output();
    usage_context.mark_upstream_frame_decode_error_before_first_output();
    usage_context.mark_upstream_event_parse_error_before_first_output();
    usage_context.mark_stream_events(&[
        SseEvent::new("message_start", json!({"type": "message_start"})),
        SseEvent::new(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {"type": "text", "text": ""}
            }),
        ),
    ]);
    assert!(usage_context.first_token_latency_ms().is_none());

    usage_context.mark_first_upstream_chunk();
    usage_context.mark_stream_events(&[
        SseEvent::new("ping", json!({"type": "ping"})),
        SseEvent::new(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "thinking_delta", "thinking": "thinking"}
            }),
        ),
    ]);
    usage_context.mark_stream_events(&[SseEvent::new(
        "content_block_delta",
        json!({
            "type": "content_block_delta",
            "index": 1,
            "delta": {"type": "text_delta", "text": "hello"}
        }),
    )]);
    usage_context.mark_stream_terminal(StreamTerminalReason::Completed);

    let trace = usage_context.latency_trace().expect("latency trace");
    assert_eq!(trace.payload_guard_ms, Some(3));
    assert!(trace.upstream_header_ms.is_some());
    assert!(trace.first_upstream_chunk_ms.is_some());
    assert_eq!(
        trace.first_output_delta_ms,
        usage_context.first_token_latency_ms()
    );
    assert_eq!(trace.first_thinking_delta_ms, trace.first_output_delta_ms);
    assert!(trace.first_visible_text_delta_ms.is_some());
    assert_eq!(trace.chunks_before_first_output, Some(1));
    assert_eq!(trace.events_before_first_output, Some(3));
    assert_eq!(trace.upstream_bytes_before_first_output, Some(128));
    assert_eq!(trace.upstream_frames_before_first_output, Some(2));
    assert_eq!(trace.upstream_events_before_first_output, Some(1));
    assert_eq!(
        trace.upstream_frames_without_downstream_events_before_first_output,
        Some(1)
    );
    assert_eq!(trace.upstream_pending_chunks_before_first_output, Some(1));
    assert_eq!(
        trace.upstream_frame_decode_errors_before_first_output,
        Some(1)
    );
    assert_eq!(
        trace.upstream_event_parse_errors_before_first_output,
        Some(1)
    );
    assert_eq!(
        trace
            .upstream_event_types_before_first_output
            .as_ref()
            .and_then(|counts| counts.get("metadata")),
        Some(&1)
    );
    assert!(trace.stream_gap_to_first_output_ms.is_some());
    assert_eq!(trace.terminal_reason, Some(StreamTerminalReason::Completed));
}

#[test]
fn stream_success_records_requested_max_tokens_and_downstream_stop_reason() {
    let prompt_cache = Arc::new(PromptCacheTracker::default());
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        true,
        usage_recorder.clone(),
        prompt_cache,
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::Disabled,
        0.95,
        CompatProfile::ClaudeCode,
        false,
    );
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    payload.stream = true;
    payload.max_tokens = 100;

    let usage_context = prepare_usage_context(
        &state,
        RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/cc/v1/messages"),
        "/cc/v1/messages",
        true,
        &payload,
        None,
        Some("conv-stop-reason".to_string()),
        Some("conv-stop-reason".to_string()),
        50,
    );
    let credential_usage = attach_test_credential_usage(usage_context, 1);

    let mut stream_context = StreamContext::new_with_simulation(
        &payload.model,
        50,
        200_000,
        false,
        false,
        HashMap::new(),
        None,
        PromptCacheSimulationMode::Disabled,
    );
    stream_context.set_requested_max_tokens(payload.max_tokens);
    let _initial_events = stream_context.generate_initial_events();
    let mut events = Vec::new();
    let mut assistant_response = crate::kiro::model::events::AssistantResponseEvent::default();
    assistant_response.content = "near token limit".to_string();
    events.extend(stream_context.process_kiro_event(&Event::AssistantResponse(assistant_response)));
    events.extend(stream_context.process_kiro_event(&Event::MessageMetadata(
        crate::kiro::model::events::MessageMetadataEvent {
            conversation_id: Some("conv-stop-reason".to_string()),
            utterance_id: Some("utt-stop-reason".to_string()),
            token_usage: Some(MetadataTokenUsage {
                uncached_input_tokens: 50,
                output_tokens: 95,
                total_tokens: 145,
                cache_read_input_tokens: 0,
                cache_write_input_tokens: 0,
            }),
        },
    )));
    events.extend(stream_context.generate_final_events());
    assert!(events.iter().any(|event| {
        event.event == "message_delta" && event.data["delta"]["stop_reason"] == "max_tokens"
    }));

    credential_usage.record_success_from_stream(&stream_context);

    let records = usage_recorder.query(UsageRecordQuery::default());
    assert_eq!(records.total, 1);
    let record = records.records.first().expect("usage record should exist");
    assert_eq!(record.requested_max_tokens, Some(100));
    assert_eq!(record.downstream_stop_reason.as_deref(), Some("max_tokens"));
}

#[test]
fn stream_zero_context_and_metadata_record_request_estimate_consistently() {
    use crate::kiro::model::events::{AssistantResponseEvent, ContextUsageEvent, MetadataEvent};

    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        true,
        usage_recorder.clone(),
        Arc::new(PromptCacheTracker::default()),
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::Disabled,
        0.95,
        CompatProfile::ClaudeCode,
        false,
    );
    let mut payload = messages_request_for_model("claude-sonnet-4-6");
    payload.stream = true;

    let usage_context = prepare_usage_context(
        &state,
        RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/cc/v1/messages"),
        "/cc/v1/messages",
        true,
        &payload,
        None,
        Some("conv-zero-context".to_string()),
        Some("conv-zero-context".to_string()),
        4_096,
    );
    let credential_usage = attach_test_credential_usage(usage_context, 1);
    let mut stream_context = StreamContext::new_with_simulation(
        &payload.model,
        4_096,
        200_000,
        false,
        false,
        HashMap::new(),
        None,
        PromptCacheSimulationMode::Disabled,
    );
    let _initial_events = stream_context.generate_initial_events();
    let mut assistant_response = AssistantResponseEvent::default();
    assistant_response.content = "fake response".to_string();
    let mut events =
        stream_context.process_kiro_event(&Event::AssistantResponse(assistant_response));
    events.extend(
        stream_context.process_kiro_event(&Event::ContextUsage(ContextUsageEvent {
            context_usage_percentage: 0.0,
        })),
    );
    events.extend(
        stream_context.process_kiro_event(&Event::Metadata(MetadataEvent {
            token_usage: Some(MetadataTokenUsage::default()),
        })),
    );
    events.extend(stream_context.generate_final_events());

    let downstream_usage = &events
        .iter()
        .find(|event| event.event == "message_delta")
        .expect("message_delta should exist")
        .data["usage"];
    assert_eq!(downstream_usage["input_tokens"], 4_096);
    assert!(
        downstream_usage["output_tokens"]
            .as_i64()
            .is_some_and(|tokens| tokens > 0)
    );

    credential_usage.record_success_from_stream(&stream_context);

    let records = usage_recorder.query(UsageRecordQuery::default());
    assert_eq!(records.total, 1);
    let record = records.records.first().expect("usage record should exist");
    assert_eq!(record.usage_source, UsageSource::RequestEstimate);
    assert_eq!(record.total_input_tokens, 4_096);
    assert_eq!(record.compat_input_tokens, 4_096);
    assert_eq!(
        record.output_tokens,
        downstream_usage["output_tokens"].as_i64().unwrap() as i32
    );
    assert_eq!(record.cache_read_input_tokens, 0);
    assert_eq!(record.cache_creation_input_tokens, 0);
    let raw_usage = record.raw_usage.expect("raw usage should be retained");
    assert_eq!(raw_usage.input_tokens, 4_096);
    assert_eq!(raw_usage.total_input_tokens, 4_096);
    assert!(raw_usage.output_tokens > 0);
}

#[test]
fn path_overrides_independently_control_reported_usage_fields() {
    let reported_usage_config = ReportedUsageConfig::default();
    let usage = CacheUsage {
        total_input_tokens: 100_000,
        input_tokens: 10_000,
        output_tokens: 1,
        cache_creation_input_tokens: 50_000,
        cache_read_input_tokens: 40_000,
        cache_creation_5m_input_tokens: 50_000,
        cache_creation_1h_input_tokens: 0,
    };
    let prompt_cache = Arc::new(PromptCacheTracker::default());
    let usage_recorder = Arc::new(UsageRecorder::new(10));

    let v1_context = RequestUsageContext {
        recorder: usage_recorder.clone(),
        tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
        prompt_cache: prompt_cache.clone(),
        prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
        pricing_catalog: Arc::new(PricingCatalog::new()),
        request_id: "req_v1_policy".to_string(),
        error_id: "req_01v1_policy".to_string(),
        endpoint: "/v1/messages".to_string(),
        stream: false,
        model: "claude-sonnet-4-6".to_string(),
        upstream_model: None,
        model_resolution_source: None,
        model_resolution_note: None,
        requested_max_tokens: 0,
        downstream_stop_reason: Arc::new(Mutex::new(None)),
        conversation_id: Some("session-policy".to_string()),
        prompt_cache_scope_conversation_id: Some("session-policy".to_string()),
        input_tokens: 100_000,
        context_window_tokens: 200_000,
        prompt_cache_profile: None,
        kiro_rs_tool_prompt_cache_plan: None,
        prompt_cache_route_namespace: None,
        prompt_cache_strategy_type: PromptCacheStrategyType::CurrentHighCache,
        simulation_mode: PromptCacheSimulationMode::HighCache,
        prompt_cache_target_read_ratio: 0.95,
        prompt_cache_token_scale: 1.0,
        prompt_cache_max_simulated_input_tokens: 0,
        prompt_cache_cap_jitter_min_tokens: 0,
        prompt_cache_cap_jitter_max_tokens: 0,
        prompt_cache_scale_min_input_tokens: 0,
        prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
        prompt_cache_bounds: PromptCacheBounds::default(),
        reported_cache_usage_policy: reported_cache_usage_policy_for_path(
            "/v1/messages",
            PromptCacheSimulationMode::HighCache,
            &reported_usage_config,
            7,
        ),
        simulated_usage: None,
        simulated_source: Some(UsageSource::LocalPromptCache),
        payload_breakdown: None,
        payload_guard_report: None,
        route_subtype_override: None,
        fallback_reason: None,
        local_preflight: None,
        external_attempts: Vec::new(),
        started_at: Instant::now(),
        first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        capacity_weight_units: Arc::new(AtomicU32::new(1)),
        latency: RequestLatencyTraceState::new(),
    };
    let cc_context = RequestUsageContext {
        endpoint: "/cc/v1/messages".to_string(),
        request_id: "req_cc_policy".to_string(),
        error_id: "req_01cc_policy".to_string(),
        reported_cache_usage_policy: reported_cache_usage_policy_for_path(
            "/cc/v1/messages",
            PromptCacheSimulationMode::HighCache,
            &reported_usage_config,
            7,
        ),
        ..v1_context.clone()
    };
    let ha_context = RequestUsageContext {
        endpoint: "/ha/v1/messages".to_string(),
        request_id: "req_ha_policy".to_string(),
        error_id: "req_01ha_policy".to_string(),
        reported_cache_usage_policy: reported_cache_usage_policy_for_path(
            "/ha/v1/messages",
            PromptCacheSimulationMode::HighCache,
            &reported_usage_config,
            7,
        ),
        ..v1_context.clone()
    };
    let na_context = RequestUsageContext {
        endpoint: "/na/v1/messages".to_string(),
        request_id: "req_na_policy".to_string(),
        error_id: "req_01na_policy".to_string(),
        prompt_cache_strategy_type: PromptCacheStrategyType::NoCache,
        simulation_mode: PromptCacheSimulationMode::Disabled,
        reported_cache_usage_policy: reported_cache_usage_policy(
            PromptCacheStrategyType::NoCache,
            PromptCacheSimulationMode::Disabled,
            &reported_usage_config.policy_for_path("/na/v1/messages"),
            7,
        ),
        simulated_source: None,
        ..v1_context.clone()
    };

    assert!(v1_context.reported_cache_usage_policy().is_some());
    assert!(cc_context.reported_cache_usage_policy().is_some());
    assert!(ha_context.reported_cache_usage_policy().is_some());
    assert!(na_context.reported_cache_usage_policy().is_none());

    let v1_reported =
        v1_context.reported_usage_for_downstream(usage, UsageSource::LocalPromptCache);
    assert_eq!(v1_reported.input_tokens, v1_context.input_tokens);
    assert_eq!(v1_reported.output_tokens, usage.output_tokens);
    assert_eq!(
        v1_reported.cache_creation_input_tokens,
        usage.cache_creation_input_tokens
    );
    assert_eq!(
        v1_reported.cache_read_input_tokens,
        usage.cache_read_input_tokens
    );

    let cc_reported =
        cc_context.reported_usage_for_downstream(usage, UsageSource::LocalPromptCache);
    assert!((1..=96).contains(&cc_reported.input_tokens));
    assert!((0..=3_300).contains(&cc_reported.cache_creation_input_tokens));
    assert_eq!(
        cc_reported.cache_read_input_tokens,
        usage.cache_read_input_tokens.saturating_add(
            cc_context
                .input_tokens
                .saturating_sub(cc_reported.input_tokens)
        )
    );
    assert_eq!(cc_reported.output_tokens, usage.output_tokens);

    let ha_reported =
        ha_context.reported_usage_for_downstream(usage, UsageSource::LocalPromptCache);
    assert!((1..=96).contains(&ha_reported.input_tokens));
    assert_eq!(
        ha_reported.cache_creation_input_tokens,
        usage.cache_creation_input_tokens
    );
    assert_eq!(
        ha_reported.cache_creation_5m_input_tokens,
        usage.cache_creation_5m_input_tokens
    );
    assert_eq!(
        ha_reported.cache_creation_1h_input_tokens,
        usage.cache_creation_1h_input_tokens
    );
    assert_eq!(
        ha_reported.cache_read_input_tokens,
        usage.cache_read_input_tokens.saturating_add(
            ha_context
                .input_tokens
                .saturating_sub(ha_reported.input_tokens)
        )
    );
    assert_eq!(ha_reported.output_tokens, usage.output_tokens);

    let na_raw = cache::RawUsage::uncached(12_345, usage.output_tokens);
    let na_reported = na_context.reported_usage_for_downstream_with_raw(
        usage,
        UsageSource::UpstreamMetadata,
        na_raw,
    );
    assert_eq!(na_reported.input_tokens, 12_345);
    assert_eq!(na_reported.output_tokens, usage.output_tokens);
    assert_eq!(na_reported.cache_creation_input_tokens, 0);
    assert_eq!(na_reported.cache_read_input_tokens, 0);
    assert_eq!(na_reported.total_input_tokens, 12_345);
}

#[test]
fn creation_control_preserves_reported_usage_input_policy() {
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let usage_context = RequestUsageContext {
        recorder: usage_recorder,
        tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
        prompt_cache: Arc::new(PromptCacheTracker::default()),
        prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
        pricing_catalog: Arc::new(PricingCatalog::new()),
        request_id: "req_creation_reported_policy".to_string(),
        error_id: "req_01creation_reported_policy".to_string(),
        endpoint: "/ha/v1/messages".to_string(),
        stream: true,
        model: "claude-sonnet-4-6".to_string(),
        upstream_model: None,
        model_resolution_source: None,
        model_resolution_note: None,
        requested_max_tokens: 0,
        downstream_stop_reason: Arc::new(Mutex::new(None)),
        conversation_id: Some("session-creation-policy".to_string()),
        prompt_cache_scope_conversation_id: Some("session-creation-policy".to_string()),
        input_tokens: 100_000,
        context_window_tokens: 200_000,
        prompt_cache_profile: None,
        kiro_rs_tool_prompt_cache_plan: None,
        prompt_cache_route_namespace: None,
        prompt_cache_strategy_type: PromptCacheStrategyType::CurrentHighCache,
        simulation_mode: PromptCacheSimulationMode::HighCache,
        prompt_cache_target_read_ratio: 0.95,
        prompt_cache_token_scale: 1.0,
        prompt_cache_max_simulated_input_tokens: 0,
        prompt_cache_cap_jitter_min_tokens: 0,
        prompt_cache_cap_jitter_max_tokens: 0,
        prompt_cache_scale_min_input_tokens: 0,
        prompt_cache_creation_control: PromptCacheCreationControlConfig {
            max_creation_tokens_per_event: 30_000,
            ..PromptCacheCreationControlConfig::default()
        },
        prompt_cache_bounds: PromptCacheBounds::default(),
        reported_cache_usage_policy: reported_cache_usage_policy(
            PromptCacheStrategyType::CurrentHighCache,
            PromptCacheSimulationMode::HighCache,
            &ReportedUsagePathPolicy {
                input: ReportedUsageFieldPolicy::sample_input_max(96),
                ..ReportedUsagePathPolicy::default()
            },
            7,
        ),
        simulated_usage: None,
        simulated_source: Some(UsageSource::LocalPromptCache),
        payload_breakdown: None,
        payload_guard_report: None,
        route_subtype_override: None,
        fallback_reason: None,
        local_preflight: None,
        external_attempts: Vec::new(),
        started_at: Instant::now(),
        first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        capacity_weight_units: Arc::new(AtomicU32::new(1)),
        latency: RequestLatencyTraceState::new(),
    };
    let credential_usage = usage_context.attach_credential(Some(1), None, false, false, Vec::new());
    let usage = CacheUsage {
        total_input_tokens: 150_000,
        input_tokens: 100_000,
        output_tokens: 9,
        cache_creation_input_tokens: 50_000,
        cache_read_input_tokens: 0,
        cache_creation_5m_input_tokens: 50_000,
        cache_creation_1h_input_tokens: 0,
    };

    let reported =
        credential_usage.canonical_reported_usage_for_success(usage, UsageSource::LocalPromptCache);

    assert_eq!(reported.input_tokens, usage.input_tokens);
    assert!((26_400..30_000).contains(&reported.cache_creation_input_tokens));
    assert_eq!(reported.cache_read_input_tokens, 0);
    assert_eq!(
        reported.total_input_tokens,
        reported
            .input_tokens
            .saturating_add(reported.cache_read_input_tokens)
            .saturating_add(reported.cache_creation_input_tokens)
    );
}

#[test]
fn provider_error_hint_extracts_credential_for_failure_records() {
    let hint = extract_credential_error_hint(
        "非流式 API 请求失败（凭据 #2 IlmiMiazzi@gmail.com）: 429 Too Many Requests",
    )
    .expect("credential hint");
    assert_eq!(hint.id, 2);
    assert_eq!(hint.label.as_deref(), Some("IlmiMiazzi@gmail.com"));
    assert_eq!(hint.display_label(), "#2 IlmiMiazzi@gmail.com");

    let prompt_cache = Arc::new(PromptCacheTracker::default());
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let usage_context = RequestUsageContext {
        recorder: usage_recorder.clone(),
        tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
        prompt_cache,
        prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
        pricing_catalog: Arc::new(PricingCatalog::new()),
        request_id: "req_error_hint".to_string(),
        error_id: "req_01error_hint".to_string(),
        endpoint: "/v1/messages".to_string(),
        stream: false,
        model: "claude-sonnet-4-6".to_string(),
        upstream_model: None,
        model_resolution_source: None,
        model_resolution_note: None,
        requested_max_tokens: 0,
        downstream_stop_reason: Arc::new(Mutex::new(None)),
        conversation_id: Some("session-error".to_string()),
        prompt_cache_scope_conversation_id: Some("session-error".to_string()),
        input_tokens: 4096,
        context_window_tokens: 200_000,
        prompt_cache_profile: None,
        kiro_rs_tool_prompt_cache_plan: None,
        prompt_cache_route_namespace: None,
        prompt_cache_strategy_type: PromptCacheStrategyType::CurrentHighCache,
        simulation_mode: PromptCacheSimulationMode::HighCache,
        prompt_cache_target_read_ratio: 0.95,
        prompt_cache_token_scale: 1.0,
        prompt_cache_max_simulated_input_tokens: 0,
        prompt_cache_cap_jitter_min_tokens: 0,
        prompt_cache_cap_jitter_max_tokens: 0,
        prompt_cache_scale_min_input_tokens: 0,
        prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
        prompt_cache_bounds: PromptCacheBounds::default(),
        reported_cache_usage_policy: None,
        simulated_usage: None,
        simulated_source: None,
        payload_breakdown: None,
        payload_guard_report: None,
        route_subtype_override: None,
        fallback_reason: None,
        local_preflight: None,
        external_attempts: Vec::new(),
        started_at: Instant::now(),
        first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        capacity_weight_units: Arc::new(AtomicU32::new(1)),
        latency: RequestLatencyTraceState::new(),
    };
    usage_context
        .attach_credential(Some(hint.id), hint.label, false, false, Vec::new())
        .with_error_metadata(Some(json!({
            "selectionFailure": {
                "stage": "rpm_limit",
                "primaryReason": "rpm_limited"
            }
        })))
        .record_failure(UsageRecordStatus::Error, "api_error", "upstream failed");

    let records = usage_recorder.query(Default::default()).records;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].credential_id, Some(2));
    assert_eq!(
        records[0].credential_label.as_deref(),
        Some("IlmiMiazzi@gmail.com")
    );
    assert_eq!(records[0].error_id.as_deref(), Some("req_01error_hint"));
    assert_eq!(records[0].error_source.as_deref(), Some("local_account"));
    assert_eq!(
        records[0]
            .error_metadata
            .as_ref()
            .and_then(|value| value.pointer("/selectionFailure/primaryReason"))
            .and_then(|value| value.as_str()),
        Some("rpm_limited")
    );
}

#[test]
fn provider_error_metadata_wraps_selection_failure_without_error_id_duplication() {
    let mut reason_counts = BTreeMap::new();
    reason_counts.insert(AccountRejectReason::RpmLimited, 3);
    let summary = SelectionFailureSummary {
        request_id: "req_selection".to_string(),
        route: "/cc/v1/messages".to_string(),
        model: "claude-sonnet-4-6".to_string(),
        stage: SelectionFailureStage::RpmLimit,
        primary_reason: AccountRejectReason::RpmLimited,
        rejected_account_count: 3,
        waitable_account_count: 3,
        retry_after_ms: Some(1000),
        reason_counts,
        sampled_accounts: Vec::new(),
        dispatch_wait_ms: None,
        queue_depth: 0,
        global_in_flight: 0,
    };
    let err: Error = KiroCallError::new("local selection failed", Vec::new())
        .with_selection_failure(Some(summary))
        .into();

    let metadata = provider_error_metadata(&err).expect("selection failure metadata");

    assert_eq!(
        metadata
            .pointer("/selectionFailure/primaryReason")
            .and_then(|value| value.as_str()),
        Some("rpm_limited")
    );
    assert!(metadata.pointer("/errorId").is_none());
    assert!(metadata.pointer("/selectionFailure/errorId").is_none());
}

#[tokio::test]
async fn content_length_threshold_error_is_not_reported_as_context_window_full() {
    let response = map_provider_error(
        anyhow::anyhow!(
            "{}",
            r#"流式 API 请求失败（凭据 #1 test@example.com）: 400 Bad Request {"message":"Input is too long.","reason":"CONTENT_LENGTH_EXCEEDS_THRESHOLD"}"#
        ),
        Some("req_test_content_length"),
        None,
        None,
    );

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    let message = value
        .pointer("/error/message")
        .and_then(|v| v.as_str())
        .expect("error message");

    assert!(message.contains("input content length exceeded"));
    assert!(message.contains("separate from the model context window"));
    assert!(!message.contains("Context window is full"));
}

#[tokio::test]
async fn malformed_upstream_error_uses_generic_user_message() {
    let response = map_provider_error(
        anyhow::anyhow!(
            "{}",
            r#"流式 API 请求失败（凭据 #1 test@example.com，请求无效）: 400 Bad Request {"message":"Improperly formed request.","reason":null}"#
        ),
        Some("req_test_malformed"),
        None,
        None,
    );

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    let message = value
        .pointer("/error/message")
        .and_then(|v| v.as_str())
        .expect("error message");

    assert_eq!(message, UPSTREAM_INVALID_REQUEST_MESSAGE);
    assert!(!message.contains("tool_use"));
    assert!(!message.contains("转换"));
}

#[tokio::test]
async fn opaque_400_bad_request_maps_to_invalid_request_not_gateway() {
    let response = map_provider_error(
        anyhow::anyhow!(
            "{}",
            "流式 API 请求失败（凭据 #6 ***，请求无效）: 400 Bad Request <failed to read response body: error decoding response body>"
        ),
        Some("req_test_opaque_bad_request"),
        None,
        None,
    );

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(
        value.pointer("/error/type").and_then(|v| v.as_str()),
        Some("invalid_request_error")
    );
    assert_eq!(
        value.pointer("/error/message").and_then(|v| v.as_str()),
        Some(UPSTREAM_INVALID_REQUEST_MESSAGE)
    );
}

#[tokio::test]
async fn no_available_credentials_error_uses_public_account_message() {
    let response = map_provider_error(
        anyhow::anyhow!("所有凭据均已禁用（0/26）"),
        Some("req_no_account"),
        None,
        None,
    );

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(
        value.pointer("/error/type").and_then(|v| v.as_str()),
        Some("api_error")
    );
    let message = value
        .pointer("/error/message")
        .and_then(|v| v.as_str())
        .expect("error message");
    assert_eq!(message, envelope::PUBLIC_ACCOUNT_UNAVAILABLE_MESSAGE);
    assert_public_error_message_is_normalized(message);
    assert!(message.contains("account"));
    assert!(!message.contains("0/26"));
    assert_eq!(value["request_id"], "req_no_account");
}

#[tokio::test]
async fn generic_provider_error_masks_raw_internal_details() {
    let response = map_provider_error(
        anyhow::anyhow!(
            "流式 API 请求失败（凭据 #37 shadow，请求失败）: 502 Bad Gateway raw upstream body"
        ),
        Some("req_generic_provider"),
        None,
        None,
    );

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(
        value.pointer("/error/type").and_then(|v| v.as_str()),
        Some("api_error")
    );
    let message = value
        .pointer("/error/message")
        .and_then(|v| v.as_str())
        .expect("error message");
    assert_eq!(message, envelope::PUBLIC_PROCESSING_FAILED_MESSAGE);
    assert_public_error_message_is_normalized(message);
    assert!(!message.contains("shadow"));
    assert_eq!(value["request_id"], "req_generic_provider");
}

#[tokio::test]
async fn provider_error_response_exposes_matching_public_error_id() {
    let response = map_provider_error(
        anyhow::anyhow!(
            "流式 API 请求失败（凭据 #37 shadow，请求失败）: 502 Bad Gateway raw upstream body"
        ),
        Some("req_public_error_id"),
        Some("req_01public_error_id"),
        None,
    );

    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        response
            .headers()
            .get("x-kiro-rs-error-id")
            .and_then(|value| value.to_str().ok()),
        Some("req_01public_error_id")
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(
        value.pointer("/error/type").and_then(|v| v.as_str()),
        Some("api_error")
    );
    let message = value
        .pointer("/error/message")
        .and_then(|v| v.as_str())
        .expect("error message");
    assert!(message.contains(envelope::PUBLIC_PROCESSING_FAILED_MESSAGE));
    assert!(message.contains("error ID: req_01public_error_id"));
    assert!(!message.contains("shadow"));
    assert_public_error_message_is_normalized(message);
}

fn assert_public_error_message_is_normalized(message: &str) {
    let lower = message.to_ascii_lowercase();
    for forbidden in [
        "credential",
        "external pool",
        "external_pool",
        "fallback",
        "preflight",
        "upstream",
        "备用池",
        "外部池",
        "凭据",
    ] {
        assert!(
            !lower.contains(forbidden),
            "public message leaked internal term {forbidden:?}: {message}"
        );
    }
}

#[test]
fn local_prompt_cache_updates_even_when_context_tokens_are_estimated() {
    let prompt_cache = Arc::new(PromptCacheTracker::default());
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let payload = MessagesRequest {
        model: "claude-sonnet-4-6".to_string(),
        max_tokens: 16,
        messages: vec![Message {
            role: "user".to_string(),
            content: json!([
                {
                    "type": "text",
                    "text": "cacheable prompt block ".repeat(700),
                    "cache_control": {"type": "ephemeral"}
                }
            ]),
        }],
        stream: true,
        system: None,
        tools: None,
        tool_choice: None,
        thinking: None,
        output_config: None,
        metadata: None,
    };
    let profile = prompt_cache.build_profile(&payload, 4096);
    let usage_context = RequestUsageContext {
        recorder: usage_recorder,
        tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
        prompt_cache: prompt_cache.clone(),
        prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
        pricing_catalog: Arc::new(PricingCatalog::new()),
        request_id: "req_test".to_string(),
        error_id: "req_01test".to_string(),
        endpoint: "/v1/messages".to_string(),
        stream: true,
        model: payload.model.clone(),
        upstream_model: None,
        model_resolution_source: None,
        model_resolution_note: None,
        requested_max_tokens: 0,
        downstream_stop_reason: Arc::new(Mutex::new(None)),
        conversation_id: Some("session-a".to_string()),
        prompt_cache_scope_conversation_id: Some("session-a".to_string()),
        input_tokens: 4096,
        context_window_tokens: 200_000,
        prompt_cache_profile: profile.clone(),
        kiro_rs_tool_prompt_cache_plan: None,
        prompt_cache_route_namespace: None,
        prompt_cache_strategy_type: PromptCacheStrategyType::CurrentHighCache,
        simulation_mode: PromptCacheSimulationMode::HighCache,
        prompt_cache_target_read_ratio: 0.85,
        prompt_cache_token_scale: 1.0,
        prompt_cache_max_simulated_input_tokens: 0,
        prompt_cache_cap_jitter_min_tokens: 0,
        prompt_cache_cap_jitter_max_tokens: 0,
        prompt_cache_scale_min_input_tokens: 0,
        prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
        prompt_cache_bounds: PromptCacheBounds::default(),
        reported_cache_usage_policy: None,
        simulated_usage: None,
        simulated_source: Some(UsageSource::LocalPromptCache),
        payload_breakdown: None,
        payload_guard_report: None,
        route_subtype_override: None,
        fallback_reason: None,
        local_preflight: None,
        external_attempts: Vec::new(),
        started_at: Instant::now(),
        first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        capacity_weight_units: Arc::new(AtomicU32::new(1)),
        latency: RequestLatencyTraceState::new(),
    }
    .attach_credential(Some(1), None, false, false, Vec::new());
    let usage = CacheUsage {
        total_input_tokens: 4096,
        input_tokens: 128,
        output_tokens: 1,
        cache_creation_input_tokens: 3968,
        cache_read_input_tokens: 0,
        cache_creation_5m_input_tokens: 3968,
        cache_creation_1h_input_tokens: 0,
    };

    usage_context.record_success(usage, UsageSource::LocalPromptCache, true);

    let scope = PromptCacheScope {
        conversation_id: "session-a".to_string(),
        route_namespace: None,
    };
    let second = prompt_cache.compute(Some(scope), profile.as_ref(), 0.85);
    assert!(second.cache_read_input_tokens > 0);
}

#[test]
fn high_cache_zero_metadata_fallback_updates_local_prompt_cache() {
    let prompt_cache = Arc::new(PromptCacheTracker::default());
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let mut payload = MessagesRequest {
        model: "claude-sonnet-4-6".to_string(),
        max_tokens: 16,
        messages: vec![Message {
            role: "user".to_string(),
            content: json!("hello"),
        }],
        stream: true,
        system: Some(vec![SystemMessage {
            text: "cacheable prompt block ".repeat(700),
            cache_control: None,
        }]),
        tools: None,
        tool_choice: None,
        thinking: None,
        output_config: None,
        metadata: None,
    };
    let profile = prompt_cache.build_high_cache_profile(&payload, 4096);
    let usage_context = RequestUsageContext {
        recorder: usage_recorder,
        tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
        prompt_cache: prompt_cache.clone(),
        prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
        pricing_catalog: Arc::new(PricingCatalog::new()),
        request_id: "req_high_cache".to_string(),
        error_id: "req_01high_cache".to_string(),
        endpoint: "/v1/messages".to_string(),
        stream: true,
        model: payload.model.clone(),
        upstream_model: None,
        model_resolution_source: None,
        model_resolution_note: None,
        requested_max_tokens: 0,
        downstream_stop_reason: Arc::new(Mutex::new(None)),
        conversation_id: Some("session-high-cache".to_string()),
        prompt_cache_scope_conversation_id: Some("session-high-cache".to_string()),
        input_tokens: 4096,
        context_window_tokens: 200_000,
        prompt_cache_profile: profile.clone(),
        kiro_rs_tool_prompt_cache_plan: None,
        prompt_cache_route_namespace: None,
        prompt_cache_strategy_type: PromptCacheStrategyType::CurrentHighCache,
        simulation_mode: PromptCacheSimulationMode::HighCache,
        prompt_cache_target_read_ratio: 0.95,
        prompt_cache_token_scale: 1.0,
        prompt_cache_max_simulated_input_tokens: 0,
        prompt_cache_cap_jitter_min_tokens: 0,
        prompt_cache_cap_jitter_max_tokens: 0,
        prompt_cache_scale_min_input_tokens: 0,
        prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
        prompt_cache_bounds: PromptCacheBounds::default(),
        reported_cache_usage_policy: None,
        simulated_usage: Some(cache::CacheSimulation {
            cache_creation_input_tokens: 3968,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 3968,
            cache_creation_1h_input_tokens: 0,
            target_cache_ratio: Some(0.95),
            amplification: None,
            split_cached_input: false,
            ..Default::default()
        }),
        simulated_source: Some(UsageSource::LocalPromptCache),
        payload_breakdown: None,
        payload_guard_report: None,
        route_subtype_override: None,
        fallback_reason: None,
        local_preflight: None,
        external_attempts: Vec::new(),
        started_at: Instant::now(),
        first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        capacity_weight_units: Arc::new(AtomicU32::new(1)),
        latency: RequestLatencyTraceState::new(),
    }
    .attach_credential(Some(1), None, false, false, Vec::new());
    let metadata = MetadataTokenUsage {
        uncached_input_tokens: 4096,
        output_tokens: 1,
        total_tokens: 4097,
        cache_read_input_tokens: 0,
        cache_write_input_tokens: 0,
    };
    let usage = cache::build_usage_with_simulation_policy(
        Some(&metadata),
        4096,
        1,
        usage_context.request.simulated_usage,
        true,
    );

    let source = usage_context.usage_source(&usage, Some(&metadata), false);
    assert_eq!(source, UsageSource::LocalPromptCache);
    usage_context.record_success(usage, source, false);

    let scope = PromptCacheScope {
        conversation_id: "session-high-cache".to_string(),
        route_namespace: None,
    };
    payload.messages.extend([
        Message {
            role: "assistant".to_string(),
            content: json!("ready"),
        },
        Message {
            role: "user".to_string(),
            content: json!("continue the same session"),
        },
    ]);
    let second_profile = prompt_cache.build_high_cache_profile(&payload, 8192);
    let second = prompt_cache.compute(Some(scope), second_profile.as_ref(), 0.95);
    assert!(second.cache_read_input_tokens > 0);
}

#[test]
fn high_cache_missing_metadata_fallback_conversation_reads_second_turn() {
    let prompt_cache = Arc::new(PromptCacheTracker::default());
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        true,
        usage_recorder,
        prompt_cache,
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::HighCache,
        0.95,
        CompatProfile::ClaudeCode,
        false,
    );
    let first_payload = MessagesRequest {
        model: "claude-sonnet-4-6".to_string(),
        max_tokens: 16,
        messages: vec![Message {
            role: "user".to_string(),
            content: json!("start high cache session"),
        }],
        stream: false,
        system: Some(vec![SystemMessage {
            text: "stable high cache system prompt ".repeat(700),
            cache_control: None,
        }]),
        tools: None,
        tool_choice: None,
        thinking: None,
        output_config: None,
        metadata: None,
    };
    let first_conversation_id =
        extract_stable_conversation_id(&first_payload).expect("fallback id");
    let first_context = prepare_usage_context(
        &state,
        RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/v1/messages"),
        "/v1/messages",
        false,
        &first_payload,
        None,
        Some(first_conversation_id.clone()),
        Some(first_conversation_id.clone()),
        4096,
    );
    let first_usage = attach_test_credential_usage(first_context, 1);
    let first_usage_body = cache::build_usage_with_simulation_policy(
        None,
        4096,
        1,
        first_usage.request.simulated_usage,
        true,
    );
    assert!(first_usage_body.cache_creation_input_tokens > 0);
    assert_eq!(first_usage_body.cache_read_input_tokens, 0);
    let first_metadata = MetadataTokenUsage {
        uncached_input_tokens: 4096,
        output_tokens: 1,
        total_tokens: 4097,
        cache_read_input_tokens: 0,
        cache_write_input_tokens: 0,
    };
    let first_source = first_usage.usage_source(&first_usage_body, Some(&first_metadata), false);
    assert_eq!(first_source, UsageSource::LocalPromptCache);
    first_usage.record_success(first_usage_body, first_source, false);

    let second_payload = MessagesRequest {
        model: "claude-sonnet-4-6".to_string(),
        max_tokens: 16,
        messages: vec![
            Message {
                role: "user".to_string(),
                content: json!("start high cache session"),
            },
            Message {
                role: "assistant".to_string(),
                content: json!("ready"),
            },
            Message {
                role: "user".to_string(),
                content: json!("continue the same session"),
            },
        ],
        stream: false,
        system: Some(vec![SystemMessage {
            text: "stable high cache system prompt ".repeat(700),
            cache_control: None,
        }]),
        tools: None,
        tool_choice: None,
        thinking: None,
        output_config: None,
        metadata: None,
    };
    let second_conversation_id =
        extract_stable_conversation_id(&second_payload).expect("fallback id");
    assert_eq!(first_conversation_id, second_conversation_id);

    let second_context = prepare_usage_context(
        &state,
        RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/v1/messages"),
        "/v1/messages",
        false,
        &second_payload,
        None,
        Some(second_conversation_id.clone()),
        Some(second_conversation_id),
        8192,
    );
    let second_usage = attach_test_credential_usage(second_context, 1);
    let second_usage_body = cache::build_usage_with_simulation_policy(
        None,
        8192,
        1,
        second_usage.request.simulated_usage,
        true,
    );

    assert!(second_usage_body.cache_read_input_tokens > 0);
    assert_eq!(
        second_usage.usage_source(&second_usage_body, None, false),
        UsageSource::LocalPromptCache
    );
}

#[test]
fn kiro_rs_tool_route_strategy_misses_first_then_reads_after_success() {
    let prompt_cache = Arc::new(PromptCacheTracker::default());
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let mut cache_policy = CachePolicyConfig::default();
    cache_policy.path_overrides.insert(
        "/kiro/v1/messages".to_string(),
        CacheRoutePolicyPatch {
            cache_type: Some(PromptCacheStrategyType::KiroRsTool),
            ..CacheRoutePolicyPatch::default()
        },
    );
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        true,
        usage_recorder,
        prompt_cache,
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::Disabled,
        0.95,
        CompatProfile::ClaudeCode,
        false,
    )
    .with_cache_policy(cache_policy);
    let session_id = "8bb5523b-ec7c-4540-a9ca-beb6d79f1552";
    let mut first_payload = MessagesRequest {
        model: "claude-sonnet-4-6".to_string(),
        max_tokens: 16,
        messages: vec![Message {
            role: "user".to_string(),
            content: json!("start kiro strategy session"),
        }],
        stream: false,
        system: Some(vec![SystemMessage {
            text: "stable kiro strategy system prompt ".repeat(700),
            cache_control: Some(json!({"type": "ephemeral"})),
        }]),
        tools: None,
        tool_choice: None,
        thinking: None,
        output_config: None,
        metadata: Some(Metadata {
            user_id: Some(format!("user_test_account__session_{session_id}")),
        }),
    };
    let cache_route =
        RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/kiro/v1/messages");
    assert_eq!(
        cache_route.policy.cache_type,
        PromptCacheStrategyType::KiroRsTool
    );
    let first_context = prepare_usage_context(
        &state,
        cache_route,
        "/kiro/v1/messages",
        false,
        &first_payload,
        None,
        Some(session_id.to_string()),
        prompt_cache_scope_conversation_id(
            PromptCacheStrategyType::KiroRsTool,
            PromptCacheSimulationMode::Disabled,
            &first_payload,
        ),
        4096,
    );

    assert_eq!(
        first_context.simulation_mode,
        PromptCacheSimulationMode::Disabled
    );
    assert_eq!(
        first_context.prompt_cache_scope_conversation_id.as_deref(),
        Some(session_id)
    );
    assert!(first_context.kiro_rs_tool_prompt_cache_plan.is_some());
    let first_simulation = first_context
        .simulated_usage
        .expect("first kiro request should project cache creation");
    assert!(first_simulation.cache_creation_input_tokens > 0);
    assert_eq!(first_simulation.cache_read_input_tokens, 0);
    let first_usage = first_context.attach_credential(Some(1), None, false, false, Vec::new());
    let first_usage_body = cache::build_usage_with_simulation_policy(
        None,
        4096,
        1,
        first_usage.request.simulated_usage,
        true,
    );
    assert!(first_usage_body.cache_creation_input_tokens > 0);
    assert_eq!(first_usage_body.cache_read_input_tokens, 0);
    assert!((32..=4_096).contains(&first_usage_body.input_tokens));
    let first_source = first_usage.usage_source(&first_usage_body, None, false);
    assert_eq!(first_source, UsageSource::LocalPromptCache);
    first_usage.record_success(first_usage_body, first_source, false);

    first_payload.messages.extend([
        Message {
            role: "assistant".to_string(),
            content: json!("ready"),
        },
        Message {
            role: "user".to_string(),
            content: json!("continue the same kiro strategy session"),
        },
    ]);
    let second_context = prepare_usage_context(
        &state,
        RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/kiro/v1/messages"),
        "/kiro/v1/messages",
        false,
        &first_payload,
        None,
        Some(session_id.to_string()),
        prompt_cache_scope_conversation_id(
            PromptCacheStrategyType::KiroRsTool,
            PromptCacheSimulationMode::Disabled,
            &first_payload,
        ),
        8192,
    );
    assert!(second_context.kiro_rs_tool_prompt_cache_plan.is_some());
    let second_simulation = second_context
        .simulated_usage
        .expect("second kiro request should project a cache read");
    assert!(second_simulation.cache_read_input_tokens > 0);
    let second_usage =
        cache::build_usage_with_simulation_policy(None, 8192, 1, Some(second_simulation), true);
    assert!(second_usage.cache_read_input_tokens > 0);
    assert!((32..=4_096).contains(&second_usage.input_tokens));
    assert_eq!(
        second_usage.input_tokens
            + second_usage.cache_creation_input_tokens
            + second_usage.cache_read_input_tokens,
        second_usage.total_input_tokens
    );
    assert_eq!(second_usage.total_input_tokens, 8192);
}

#[test]
fn kiro_rs_tool_route_strategy_commits_without_credential_id() {
    let prompt_cache = Arc::new(PromptCacheTracker::default());
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let mut cache_policy = CachePolicyConfig::default();
    cache_policy.path_overrides.insert(
        "/kiro/v1/messages".to_string(),
        CacheRoutePolicyPatch {
            cache_type: Some(PromptCacheStrategyType::KiroRsTool),
            ..CacheRoutePolicyPatch::default()
        },
    );
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        true,
        usage_recorder,
        prompt_cache,
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::Disabled,
        0.95,
        CompatProfile::ClaudeCode,
        false,
    )
    .with_cache_policy(cache_policy);
    let session_id = "8bb5523b-ec7c-4540-a9ca-beb6d79f1552";
    let mut payload = MessagesRequest {
        model: "claude-sonnet-4-6".to_string(),
        max_tokens: 16,
        messages: vec![Message {
            role: "user".to_string(),
            content: json!("start kiro no credential session"),
        }],
        stream: false,
        system: Some(vec![SystemMessage {
            text: "stable kiro no credential system prompt ".repeat(700),
            cache_control: Some(json!({"type": "ephemeral"})),
        }]),
        tools: None,
        tool_choice: None,
        thinking: None,
        output_config: None,
        metadata: Some(Metadata {
            user_id: Some(format!("user_test_account__session_{session_id}")),
        }),
    };

    let first_context = prepare_usage_context(
        &state,
        RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/kiro/v1/messages"),
        "/kiro/v1/messages",
        false,
        &payload,
        None,
        Some(session_id.to_string()),
        prompt_cache_scope_conversation_id(
            PromptCacheStrategyType::KiroRsTool,
            PromptCacheSimulationMode::Disabled,
            &payload,
        ),
        4096,
    );
    let first_usage_body = cache::build_usage_with_simulation_policy(
        None,
        4096,
        1,
        first_context.simulated_usage,
        true,
    );
    let first_usage = first_context.attach_credential(None, None, false, false, Vec::new());
    let first_source = first_usage.usage_source(&first_usage_body, None, false);
    assert_eq!(first_source, UsageSource::LocalPromptCache);
    first_usage.record_success(first_usage_body, first_source, false);

    payload.messages.extend([
        Message {
            role: "assistant".to_string(),
            content: json!("ready"),
        },
        Message {
            role: "user".to_string(),
            content: json!("continue no credential session"),
        },
    ]);
    let second_context = prepare_usage_context(
        &state,
        RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/kiro/v1/messages"),
        "/kiro/v1/messages",
        false,
        &payload,
        None,
        Some(session_id.to_string()),
        prompt_cache_scope_conversation_id(
            PromptCacheStrategyType::KiroRsTool,
            PromptCacheSimulationMode::Disabled,
            &payload,
        ),
        8192,
    );
    let second_simulation = second_context
        .simulated_usage
        .expect("second kiro request should read cache without credential id");
    assert!(second_simulation.cache_read_input_tokens > 0);
    let second_usage =
        cache::build_usage_with_simulation_policy(None, 8192, 1, Some(second_simulation), true);
    assert!((32..=4_096).contains(&second_usage.input_tokens));
}

#[test]
fn disabled_prompt_cache_does_not_simulate_without_stable_conversation_id() {
    let prompt_cache = Arc::new(PromptCacheTracker::default());
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        true,
        usage_recorder,
        prompt_cache.clone(),
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::Disabled,
        0.95,
        CompatProfile::ClaudeCode,
        false,
    );
    let payload = MessagesRequest {
        model: "claude-sonnet-4-6".to_string(),
        max_tokens: 16,
        messages: vec![Message {
            role: "user".to_string(),
            content: json!([
                {
                    "type": "text",
                    "text": "cacheable prompt block ".repeat(700),
                    "cache_control": {"type": "ephemeral"}
                }
            ]),
        }],
        stream: true,
        system: None,
        tools: None,
        tool_choice: None,
        thinking: None,
        output_config: None,
        metadata: None,
    };
    let (simulation, source) =
        build_simulated_usage(PromptCacheSimulationMode::Disabled, None, None);

    assert!(simulation.is_none());
    assert!(source.is_none());

    let context = prepare_usage_context(
        &state,
        RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/v1/messages"),
        "/v1/messages",
        true,
        &payload,
        None,
        Some("random-conversation".to_string()),
        prompt_cache_scope_conversation_id(
            PromptCacheStrategyType::CurrentHighCache,
            state.prompt_cache_simulation_mode,
            &payload,
        ),
        4096,
    );
    assert!(context.prompt_cache_profile.is_none());
    assert!(context.prompt_cache_scope_conversation_id.is_none());

    let credential_usage = attach_test_credential_usage(context, 1);
    assert!(credential_usage.request.simulated_usage.is_none());
    assert!(credential_usage.request.simulated_source.is_none());
}

#[test]
fn builtin_na_path_does_not_build_local_profile_or_reporting_policy() {
    let prompt_cache = Arc::new(PromptCacheTracker::default());
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        true,
        usage_recorder,
        prompt_cache,
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::Disabled,
        0.95,
        CompatProfile::ClaudeCode,
        false,
    );
    let payload = MessagesRequest {
        model: "claude-sonnet-4-6".to_string(),
        max_tokens: 16,
        messages: vec![Message {
            role: "user".to_string(),
            content: json!([
                {
                    "type": "text",
                    "text": "cacheable prompt block ".repeat(700),
                    "cache_control": {"type": "ephemeral"}
                }
            ]),
        }],
        stream: false,
        system: None,
        tools: None,
        tool_choice: None,
        thinking: None,
        output_config: None,
        metadata: None,
    };

    let context = prepare_usage_context(
        &state,
        RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/na/v1/messages"),
        "/na/v1/messages",
        false,
        &payload,
        None,
        Some("conversation-id".to_string()),
        prompt_cache_scope_conversation_id(
            PromptCacheStrategyType::NoCache,
            PromptCacheSimulationMode::Disabled,
            &payload,
        ),
        4096,
    );

    assert_eq!(
        context.prompt_cache_strategy_type,
        PromptCacheStrategyType::NoCache
    );
    assert_eq!(context.simulation_mode, PromptCacheSimulationMode::Disabled);
    assert!(context.prompt_cache_profile.is_none());
    assert!(context.kiro_rs_tool_prompt_cache_plan.is_none());
    assert!(context.prompt_cache_scope_conversation_id.is_none());
    assert_eq!(context.prompt_cache_route_namespace, None);
    assert!(context.simulated_usage.is_none());
    assert!(context.simulated_source.is_none());
    assert!(context.reported_cache_usage_policy.is_none());
}

#[test]
fn no_cache_route_does_not_build_cache_profile_plan_or_shape_reporting() {
    let prompt_cache = Arc::new(PromptCacheTracker::default());
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let mut cache_policy = CachePolicyConfig::default();
    cache_policy.path_overrides.insert(
        "/plain".to_string(),
        CacheRoutePolicyPatch {
            cache_type: Some(PromptCacheStrategyType::NoCache),
            simulation: Some(CacheSimulationPolicyPatch {
                enabled: Some(true),
                ..CacheSimulationPolicyPatch::default()
            }),
            creation_control: Some(PromptCacheCreationControlConfig::default()),
            reported_usage: Some(ReportedUsagePathPolicy {
                input: ReportedUsageFieldPolicy::sample_input_max(32),
                ..ReportedUsagePathPolicy::default()
            }),
            cache_point: Some(CachePointPolicyPatch {
                enabled: Some(true),
                ..CachePointPolicyPatch::default()
            }),
            ..CacheRoutePolicyPatch::default()
        },
    );
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        true,
        usage_recorder,
        prompt_cache,
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::HighCache,
        0.95,
        CompatProfile::ClaudeCode,
        false,
    )
    .with_cache_policy(cache_policy);
    let payload = MessagesRequest {
        model: "claude-sonnet-4-6".to_string(),
        max_tokens: 16,
        messages: vec![Message {
            role: "user".to_string(),
            content: json!("plain request should not enter prompt cache"),
        }],
        stream: false,
        system: Some(vec![SystemMessage {
            text: "cacheable prompt block ".repeat(700),
            cache_control: Some(json!({"type": "ephemeral"})),
        }]),
        tools: None,
        tool_choice: None,
        thinking: None,
        output_config: None,
        metadata: None,
    };

    let cache_route =
        RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/plain/v1/messages");
    assert_eq!(
        cache_route.policy.cache_type,
        PromptCacheStrategyType::NoCache
    );
    assert_eq!(cache_route.namespace, None);
    assert_eq!(
        prompt_cache_simulation_mode_for_policy(&cache_route.policy),
        PromptCacheSimulationMode::Disabled
    );
    assert_eq!(
        prompt_cache_converter_mode_for_policy(&cache_route.policy),
        PromptCacheSimulationMode::Disabled
    );
    assert!(!cache_route.policy.cache_point.enabled);

    let context = prepare_usage_context(
        &state,
        cache_route,
        "/plain/v1/messages",
        false,
        &payload,
        None,
        Some("plain-session".to_string()),
        prompt_cache_scope_conversation_id(
            PromptCacheStrategyType::NoCache,
            PromptCacheSimulationMode::Disabled,
            &payload,
        ),
        4096,
    );

    assert_eq!(
        context.prompt_cache_strategy_type,
        PromptCacheStrategyType::NoCache
    );
    assert_eq!(context.simulation_mode, PromptCacheSimulationMode::Disabled);
    assert!(context.prompt_cache_profile.is_none());
    assert!(context.kiro_rs_tool_prompt_cache_plan.is_none());
    assert!(context.prompt_cache_scope_conversation_id.is_none());
    assert_eq!(context.prompt_cache_route_namespace, None);
    assert!(context.simulated_usage.is_none());
    assert!(context.simulated_source.is_none());
    assert!(context.reported_cache_usage_policy.is_none());

    let upstream_raw = CacheUsage {
        total_input_tokens: 4_165,
        input_tokens: 4_165,
        output_tokens: 7,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        cache_creation_5m_input_tokens: 0,
        cache_creation_1h_input_tokens: 0,
    };
    let reported = context.reported_usage_for_downstream_with_raw(
        cache::CacheUsage {
            total_input_tokens: 125_000,
            input_tokens: 100_000,
            output_tokens: 7,
            cache_creation_input_tokens: 20_000,
            cache_read_input_tokens: 5_000,
            cache_creation_5m_input_tokens: 20_000,
            cache_creation_1h_input_tokens: 0,
        },
        UsageSource::UpstreamMetadata,
        raw_usage_to_reported_raw(upstream_raw),
    );
    assert_eq!(reported.input_tokens, 4_165);
    assert_eq!(reported.output_tokens, 7);
    assert_eq!(reported.cache_creation_input_tokens, 0);
    assert_eq!(reported.cache_read_input_tokens, 0);
}

#[test]
fn no_cache_disabled_reported_usage_preserves_upstream_metadata_usage() {
    let prompt_cache = Arc::new(PromptCacheTracker::default());
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let mut reported_usage = ReportedUsageConfig::default();
    reported_usage
        .path_overrides
        .insert("/na".to_string(), ReportedUsagePathPolicy::disabled());
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        true,
        usage_recorder,
        prompt_cache,
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::HighCache,
        0.95,
        CompatProfile::ClaudeCode,
        false,
    )
    .with_reported_usage(reported_usage);
    let payload = MessagesRequest {
        model: "claude-sonnet-4-6".to_string(),
        max_tokens: 16,
        messages: vec![Message {
            role: "user".to_string(),
            content: json!("no-cache request should preserve upstream metadata usage"),
        }],
        stream: true,
        system: Some(vec![SystemMessage {
            text: "large no-cache system prompt ".repeat(700),
            cache_control: Some(json!({"type": "ephemeral"})),
        }]),
        tools: None,
        tool_choice: None,
        thinking: None,
        output_config: None,
        metadata: None,
    };

    let context = prepare_usage_context(
        &state,
        RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/na/v1/messages"),
        "/na/v1/messages",
        true,
        &payload,
        None,
        Some("na-disabled-reported".to_string()),
        prompt_cache_scope_conversation_id(
            PromptCacheStrategyType::NoCache,
            PromptCacheSimulationMode::Disabled,
            &payload,
        ),
        32_000,
    );

    assert_eq!(
        context.prompt_cache_strategy_type,
        PromptCacheStrategyType::NoCache
    );
    assert!(context.reported_cache_usage_policy.is_none());

    let context_usage = CacheUsage {
        total_input_tokens: 32_000,
        input_tokens: 32_000,
        output_tokens: 3,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        cache_creation_5m_input_tokens: 0,
        cache_creation_1h_input_tokens: 0,
    };
    let upstream_raw = CacheUsage {
        total_input_tokens: 10,
        input_tokens: 10,
        output_tokens: 3,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        cache_creation_5m_input_tokens: 0,
        cache_creation_1h_input_tokens: 0,
    };

    let reported = context.reported_usage_for_downstream_with_raw(
        context_usage,
        UsageSource::UpstreamMetadata,
        raw_usage_to_reported_raw(upstream_raw),
    );

    assert_eq!(reported.input_tokens, 10);
    assert_eq!(reported.output_tokens, 3);
    assert_eq!(reported.cache_read_input_tokens, 0);
    assert_eq!(reported.cache_creation_input_tokens, 0);
}

#[test]
fn no_cache_canonical_record_keeps_upstream_raw_when_default_reporting_exists() {
    let prompt_cache = Arc::new(PromptCacheTracker::default());
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        true,
        usage_recorder,
        prompt_cache,
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::HighCache,
        0.95,
        CompatProfile::ClaudeCode,
        false,
    );
    let payload = MessagesRequest {
        model: "claude-sonnet-4-6".to_string(),
        max_tokens: 16,
        messages: vec![Message {
            role: "user".to_string(),
            content: json!("no-cache request should not be expanded by record shaping"),
        }],
        stream: true,
        system: Some(vec![SystemMessage {
            text: "large no-cache system prompt ".repeat(700),
            cache_control: Some(json!({"type": "ephemeral"})),
        }]),
        tools: None,
        tool_choice: None,
        thinking: None,
        output_config: None,
        metadata: None,
    };

    let context = prepare_usage_context(
        &state,
        RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/na/v1/messages"),
        "/na/v1/messages",
        true,
        &payload,
        None,
        Some("na-default-reported".to_string()),
        prompt_cache_scope_conversation_id(
            PromptCacheStrategyType::NoCache,
            PromptCacheSimulationMode::Disabled,
            &payload,
        ),
        32_000,
    );

    assert_eq!(
        context.prompt_cache_strategy_type,
        PromptCacheStrategyType::NoCache
    );
    assert!(context.reported_cache_usage_policy.is_none());

    let context_usage = CacheUsage {
        total_input_tokens: 32_000,
        input_tokens: 32_000,
        output_tokens: 3,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        cache_creation_5m_input_tokens: 0,
        cache_creation_1h_input_tokens: 0,
    };
    let upstream_raw = CacheUsage {
        total_input_tokens: 10,
        input_tokens: 10,
        output_tokens: 3,
        cache_creation_input_tokens: 0,
        cache_read_input_tokens: 0,
        cache_creation_5m_input_tokens: 0,
        cache_creation_1h_input_tokens: 0,
    };
    let credential_context = context.attach_credential(Some(1), None, false, false, Vec::new());

    let reported = credential_context.canonical_reported_usage_for_success_with_raw(
        context_usage,
        UsageSource::UpstreamMetadata,
        upstream_raw,
    );

    assert_eq!(reported.input_tokens, 10);
    assert_eq!(reported.output_tokens, 3);
    assert_eq!(reported.cache_read_input_tokens, 0);
    assert_eq!(reported.cache_creation_input_tokens, 0);
}

fn attach_test_credential_usage(
    mut usage_context: RequestUsageContext,
    credential_id: u64,
) -> CredentialUsageContext {
    let scope = usage_context
        .prompt_cache_scope_conversation_id
        .as_ref()
        .map(|conversation_id| {
            let _ = credential_id;
            PromptCacheScope::new(conversation_id.clone(), None)
        });
    let prompt_usage = usage_context.prompt_cache.compute_with_bounds(
        scope,
        usage_context.prompt_cache_profile.as_ref(),
        usage_context.prompt_cache_target_read_ratio,
        usage_context.prompt_cache_bounds,
    );
    usage_context.simulated_usage =
        cache::CacheSimulation::from_prompt_cache_with_ratio_and_amplification(
            prompt_usage,
            usage_context.prompt_cache_target_read_ratio,
            usage_context.cache_amplification(),
        );
    usage_context.simulated_source = usage_context
        .simulated_usage
        .map(|_| UsageSource::LocalPromptCache);
    usage_context.attach_credential(Some(credential_id), None, false, false, Vec::new())
}

#[test]
fn strict_profile_suppresses_proxy_warning_header() {
    let prompt_cache = Arc::new(PromptCacheTracker::default());
    let usage_recorder = Arc::new(UsageRecorder::new(10));
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        true,
        usage_recorder,
        prompt_cache,
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::Disabled,
        0.85,
        CompatProfile::AnthropicStrict,
        true,
    );

    assert!(!should_expose_proxy_warnings(
        &RequestRuntimeConfig::from_app_state(&state)
    ));
}

#[test]
fn external_fallback_classifier_rejects_request_errors() {
    let config = ExternalPoolsConfig::default();

    assert_eq!(
        classify_local_error_for_external_fallback(
            r#"400 Bad Request {"message":"Input is too long.","reason":"CONTENT_LENGTH_EXCEEDS_THRESHOLD"}"#,
            &[],
            &config,
        ),
        None
    );
    assert_eq!(
        classify_local_error_for_external_fallback(
            "JSON schema is invalid for tool input_schema",
            &[],
            &config,
        ),
        None
    );

    let attempts = vec![KiroCredentialAttempt::new(
        0,
        1,
        None,
        Some(StatusCode::BAD_REQUEST),
        "fail",
        Some("client_error"),
        Some("bad request"),
        10,
    )];
    assert_eq!(
        classify_local_error_for_external_fallback("429 Too Many Requests", &attempts, &config),
        None
    );
}

#[test]
fn external_fallback_classifier_allows_capacity_and_transient_errors() {
    let config = ExternalPoolsConfig::default();

    assert_eq!(
        classify_local_error_for_external_fallback(
            "本地凭据调度容量暂不可用，并发槽位已满",
            &[],
            &config,
        )
        .as_deref(),
        Some("local_capacity_exhausted")
    );
    assert_eq!(
        classify_local_error_for_external_fallback("429 Too Many Requests", &[], &config)
            .as_deref(),
        Some("local_transient_exhausted")
    );
}

#[test]
fn external_fallback_classifier_can_use_retry_stage_attempts_after_payload_guard_retry() {
    let config = ExternalPoolsConfig::default();
    let prior_too_long_attempt = KiroCredentialAttempt::new(
        0,
        63,
        Some("account@example.com".to_string()),
        Some(StatusCode::BAD_REQUEST),
        "fail",
        Some("client_error"),
        Some(
            r#"400 Bad Request {"message":"Input is too long.","reason":"CONTENT_LENGTH_EXCEEDS_THRESHOLD"}"#,
        ),
        1200,
    );
    let capacity_message = "本地账号调度容量暂不可用（可用: 7/29, 临时可调度: 0, global_credential_max_concurrent_requests=10, effective_credential_max_concurrent_requests=50, retry_after_secs=1）";

    let diagnostic_attempts =
        merge_credential_attempts(vec![prior_too_long_attempt.clone()], Vec::new());
    assert_eq!(
        classify_local_error_for_external_fallback(capacity_message, &diagnostic_attempts, &config,),
        None
    );

    let retry_stage_attempts = Vec::new();
    assert_eq!(
        classify_local_error_for_external_fallback(
            capacity_message,
            &retry_stage_attempts,
            &config,
        )
        .as_deref(),
        Some("local_capacity_exhausted")
    );

    let retry_bad_request_attempt = vec![KiroCredentialAttempt::new(
        0,
        64,
        Some("retry@example.com".to_string()),
        Some(StatusCode::BAD_REQUEST),
        "fail",
        Some("client_error"),
        Some("bad request"),
        10,
    )];
    assert_eq!(
        classify_local_error_for_external_fallback(
            capacity_message,
            &retry_bad_request_attempt,
            &config,
        ),
        None
    );
}

#[test]
fn external_fallback_classifier_respects_scheduler_fallback_toggles() {
    let mut config = ExternalPoolsConfig::default();

    config.fallback_on_local_capacity_exhausted = false;
    assert_eq!(
        classify_local_error_for_external_fallback(
            "本地凭据调度容量暂不可用，并发槽位已满",
            &[],
            &config,
        ),
        None
    );

    config = ExternalPoolsConfig::default();
    config.fallback_on_local_transient_exhausted = false;
    assert_eq!(
        classify_local_error_for_external_fallback("429 Too Many Requests", &[], &config),
        None
    );
    assert_eq!(
        classify_local_error_for_external_fallback(
            "upstream server_error",
            &[KiroCredentialAttempt::new(
                0,
                1,
                None,
                Some(StatusCode::BAD_GATEWAY),
                "retry",
                Some("server_error"),
                Some("502"),
                10,
            )],
            &config,
        ),
        None
    );

    config = ExternalPoolsConfig::default();
    config.fallback_on_no_available_credentials = false;
    assert_eq!(
        classify_local_error_for_external_fallback("所有凭据均已禁用（0/2）", &[], &config),
        None
    );

    config.fallback_on_no_available_credentials = true;
    assert_eq!(
        classify_local_error_for_external_fallback("所有凭据均已禁用（0/2）", &[], &config)
            .as_deref(),
        Some("no_available_credentials")
    );
}

#[test]
fn local_pool_preflight_reason_respects_scheduler_fallback_toggles() {
    let mut config = ExternalPoolsConfig::default();

    assert!(local_pool_capacity_fail_fast_enabled(&config));
    assert_eq!(
        local_pool_route_fallback_reason(LocalPoolRouteStateKind::NoCredentials, &config),
        Some("local_no_credentials")
    );
    assert_eq!(
        local_pool_route_fallback_reason(LocalPoolRouteStateKind::AllDisabled, &config),
        Some("local_all_disabled")
    );
    assert_eq!(
        local_pool_route_fallback_reason(LocalPoolRouteStateKind::ProxyBlocked, &config),
        Some("local_proxy_blocked")
    );
    assert_eq!(
        local_pool_route_fallback_reason(LocalPoolRouteStateKind::AllCoolingDown, &config),
        Some("local_all_cooling_down")
    );
    assert_eq!(
        local_pool_route_fallback_reason(LocalPoolRouteStateKind::CapacityFull, &config),
        Some("local_capacity_full")
    );
    assert_eq!(
        local_pool_route_fallback_reason(LocalPoolRouteStateKind::NoModelCompatible, &config),
        None
    );

    config.fallback_on_no_available_credentials = false;
    assert_eq!(
        local_pool_route_fallback_reason(LocalPoolRouteStateKind::NoCredentials, &config),
        None
    );
    assert_eq!(
        local_pool_route_fallback_reason(LocalPoolRouteStateKind::AllDisabled, &config),
        None
    );
    assert_eq!(
        local_pool_route_fallback_reason(LocalPoolRouteStateKind::ProxyBlocked, &config),
        None
    );

    config = ExternalPoolsConfig::default();
    config.fallback_on_local_transient_exhausted = false;
    assert_eq!(
        local_pool_route_fallback_reason(LocalPoolRouteStateKind::AllCoolingDown, &config),
        None
    );

    config = ExternalPoolsConfig::default();
    config.fallback_on_local_capacity_exhausted = false;
    assert!(!local_pool_capacity_fail_fast_enabled(&config));
    assert_eq!(
        local_pool_route_fallback_reason(LocalPoolRouteStateKind::CapacityFull, &config),
        None
    );

    config = ExternalPoolsConfig::default();
    config.fallback_on_unsupported_model = true;
    assert_eq!(
        local_pool_route_fallback_reason(LocalPoolRouteStateKind::NoModelCompatible, &config),
        Some("local_no_model_compatible")
    );

    config.local_pool_preflight_enabled = false;
    assert!(!local_pool_capacity_fail_fast_enabled(&config));
}

#[test]
fn external_fallback_classifier_gates_unsupported_model() {
    let mut config = ExternalPoolsConfig::default();
    config.fallback_on_unsupported_model = false;
    assert_eq!(
        classify_local_error_for_external_fallback("模型不支持: claude-future", &[], &config,),
        None
    );

    config.fallback_on_unsupported_model = true;
    assert_eq!(
        classify_local_error_for_external_fallback("模型不支持: claude-future", &[], &config,)
            .as_deref(),
        Some("unsupported_model")
    );
    assert_eq!(
            classify_local_error_for_external_fallback(
                r#"非流式 API 请求失败: 400 Bad Request {"message":"Invalid model. Please select a different model to continue.","reason":"INVALID_MODEL_ID"}"#,
                &[KiroCredentialAttempt::new(
                    0,
                    1,
                    None,
                    Some(StatusCode::BAD_REQUEST),
                    "fail",
                    Some("client_error"),
                    Some("bad request"),
                    10,
                )],
                &config,
            )
            .as_deref(),
            Some("unsupported_model")
        );
}

#[test]
fn external_local_rescue_classifier_respects_error_type_and_toggles() {
    let config = ExternalPoolsConfig::default();
    let rate_limit = ExternalPoolFinalError {
            status: StatusCode::TOO_MANY_REQUESTS,
            response_error_type: "rate_limit_error".to_string(),
            route_error_type: "rate_limit".to_string(),
            message:
                r#"{"message":"Too many requests, please wait before trying again.","reason":"SERVICE_REQUEST_RATE_EXCEEDED"}"#
                    .to_string(),
            error_id: "req_01rate_limit".to_string(),
            retryable: true,
            attempts: Vec::new(),
            pool_id: Some(1),
            pool_name: Some("backup".to_string()),
        };
    assert_eq!(
        local_rescue_reason_after_external_error(&config, &rate_limit, None),
        Some("external_rate_limit")
    );
    assert_eq!(
        local_rescue_reason_after_external_error(
            &config,
            &rate_limit,
            Some("no_available_credentials")
        ),
        Some("external_rate_limit")
    );
    assert_eq!(
        local_rescue_reason_after_external_error(
            &config,
            &rate_limit,
            Some("local_capacity_exhausted")
        ),
        Some("external_rate_limit")
    );

    let timeout = ExternalPoolFinalError {
        status: StatusCode::BAD_GATEWAY,
        response_error_type: "api_error".to_string(),
        route_error_type: "network_error".to_string(),
        message: "stream idle timeout".to_string(),
        error_id: "req_01timeout".to_string(),
        retryable: true,
        attempts: Vec::new(),
        pool_id: Some(1),
        pool_name: Some("backup".to_string()),
    };
    assert_eq!(
        local_rescue_reason_after_external_error(&config, &timeout, None),
        Some("external_timeout")
    );

    let capacity = ExternalPoolFinalError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        response_error_type: "api_error".to_string(),
        route_error_type: "external_pool_capacity_full".to_string(),
        message: "Request capacity is full".to_string(),
        error_id: "req_01capacity".to_string(),
        retryable: true,
        attempts: Vec::new(),
        pool_id: None,
        pool_name: None,
    };
    assert_eq!(
        local_rescue_reason_after_external_error(&config, &capacity, None),
        Some("external_capacity")
    );

    let bad_request = ExternalPoolFinalError {
        status: StatusCode::BAD_REQUEST,
        response_error_type: "invalid_request_error".to_string(),
        route_error_type: "client_error".to_string(),
        message: "Improperly formed request".to_string(),
        error_id: "req_01bad_request".to_string(),
        retryable: false,
        attempts: Vec::new(),
        pool_id: Some(1),
        pool_name: Some("backup".to_string()),
    };
    assert_eq!(
        local_rescue_reason_after_external_error(&config, &bad_request, None),
        Some("external_bad_request")
    );

    let mut disabled = config.clone();
    disabled.external_pool_local_rescue_enabled = false;
    assert_eq!(
        local_rescue_reason_after_external_error(&disabled, &rate_limit, None),
        None
    );

    let mut direct = config.clone();
    direct.external_direct_policy_enabled = true;
    assert_eq!(
        local_rescue_reason_after_external_error(&direct, &rate_limit, None),
        None
    );

    let mut no_rate_limit = config;
    no_rate_limit.external_pool_local_rescue_on_rate_limit = false;
    assert_eq!(
        local_rescue_reason_after_external_error(&no_rate_limit, &rate_limit, None),
        None
    );

    let mut no_capacity = no_rate_limit;
    no_capacity.external_pool_local_rescue_on_capacity = false;
    assert_eq!(
        local_rescue_reason_after_external_error(&no_capacity, &capacity, None),
        None
    );

    let server_error = ExternalPoolFinalError {
        status: StatusCode::BAD_GATEWAY,
        response_error_type: "api_error".to_string(),
        route_error_type: "server_error".to_string(),
        message: "external upstream failed".to_string(),
        error_id: "req_01server".to_string(),
        retryable: true,
        attempts: Vec::new(),
        pool_id: Some(1),
        pool_name: Some("backup".to_string()),
    };
    assert_eq!(
        local_rescue_reason_after_external_error(
            &no_capacity,
            &server_error,
            Some("local_transient_exhausted")
        ),
        Some("external_error")
    );
    assert_eq!(
        local_rescue_reason_after_external_error(
            &no_capacity,
            &server_error,
            Some("local_capacity_exhausted")
        ),
        Some("external_error")
    );
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
