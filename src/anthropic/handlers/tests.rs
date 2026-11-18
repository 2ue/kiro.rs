use super::*;
use crate::account::*;
use crate::anthropic::model_capabilities::ModelCapabilitiesCatalog;
use crate::anthropic::model_capabilities::ModelResolutionSource;
use crate::anthropic::pricing::PricingCatalog;
use crate::anthropic::prompt_cache::PromptCacheTracker;
use crate::anthropic::prompt_cache_creation_control::PromptCacheCreationController;
use crate::anthropic::request_admission::RequestAdmissionController;
use crate::anthropic::router::{AnthropicRouterDependencies, create_router};
use crate::anthropic::types::{Message, OutputConfig};
use crate::anthropic::usage::{
    UsageRecord, UsageRecordQuery, UsageRecordStatus, UsageRecorder, UsageRouteKind,
};
use crate::common::auth::RequestApiKeyStore;
use crate::storage::{postgres::PostgresStore, redis_cache::RedisStore};
use axum::{Router, body::Body, http::Request, routing::post};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use std::{
    collections::HashMap,
    future::Future,
    sync::{
        Mutex as StdMutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tower::ServiceExt;

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
    );
    state.runtime.install_local(|config| {
        config.routes.route_mut("/dfcache/cc").cache_policy = CacheRoutePolicyPatch {
            cache_type: Some(PromptCacheStrategyType::CurrentHighCache),
            route_namespace: Some(true),
            ..Default::default()
        };
    });

    assert_eq!(
        resolve_defined_cache_route(&state, "cc").unwrap(),
        "/dfcache/cc"
    );
    assert!(resolve_defined_cache_route(&state, "aa").is_err());
    assert!(resolve_defined_cache_route(&state, "aa/b").is_err());
}

#[test]
fn admin_route_update_is_visible_to_request_path_config_snapshot() {
    let state = AppState::new(
        Arc::new(RequestApiKeyStore::new(["test-key"])),
        Arc::new(UsageRecorder::new(10)),
        Arc::new(PromptCacheTracker::default()),
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::HighCache,
    );
    let previous = request_runtime_config(&state);
    state
        .runtime
        .update(|config| {
            config.prompt_cache_target_read_ratio = 0.75;
            config.prompt_cache_token_scale = 2.0;
            config.accounts.request_timeout_secs = 42;
            config.routes.route_mut("/cc").cache_policy.cache_type =
                Some(PromptCacheStrategyType::NoCache);
        })
        .unwrap();

    let current = request_runtime_config(&state);
    assert!(!Arc::ptr_eq(&previous, &current));
    for runtime_config in [
        current,
        Arc::new(RequestRuntimeConfig::from_app_state(&state)),
    ] {
        assert_eq!(runtime_config.prompt_cache_target_read_ratio, 0.75);
        assert_eq!(runtime_config.prompt_cache_token_scale, 2.0);
        assert_eq!(runtime_config.accounts.request_timeout_secs, 42);
        assert_eq!(
            runtime_config
                .cache_policy_for_path("/cc/v1/messages")
                .policy
                .cache_type,
            PromptCacheStrategyType::NoCache
        );
    }
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

        let response = parse_messages_payload(&body).expect_err("empty model rejected");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let error_id = response.headers()["x-error-id"]
            .to_str()
            .unwrap()
            .to_string();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read error body");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("json envelope");
        assert_eq!(value["error"]["type"], "invalid_request_error");
        assert_eq!(
            value["error"]["message"],
            envelope::public_message_with_error_id(
                "model: field is required and cannot be empty",
                &error_id,
            )
        );
        assert_eq!(value["request_id"], error_id);
        assert!(
            value["request_id"]
                .as_str()
                .is_some_and(|request_id| request_id.starts_with("req_01"))
        );
    }
}

#[test]
fn raw_external_route_request_applies_non_stream_skip_cache_route() {
    let mut routes = crate::model::routes::RoutesConfig::default();
    routes.route_mut(&"/cc".to_string()).cache_policy = CacheRoutePolicyPatch {
        cache_type: Some(PromptCacheStrategyType::CurrentHighCache),
        reported_usage: Some(ReportedUsagePathPolicy {
            skip_non_stream_usage_projection: true,
            input: ReportedUsageFieldPolicy::sample_input_max(1),
            ..ReportedUsagePathPolicy::default()
        }),
        ..CacheRoutePolicyPatch::default()
    };
    let state = AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
        Arc::new(UsageRecorder::new(10)),
        Arc::new(PromptCacheTracker::default()),
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::HighCache,
    );
    state.runtime.install_local(|config| config.routes = routes);
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
async fn messages_without_external_manager_return_529() {
    {
        let state = AppState::new(
            Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
            Arc::new(UsageRecorder::new(10)),
            Arc::new(PromptCacheTracker::default()),
            Arc::new(PromptCacheCreationController::default()),
            PromptCacheSimulationMode::Disabled,
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
            None,
        )
        .await;
        assert_eq!(response.status().as_u16(), 529);
        let body = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["type"], "overloaded_error");
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

#[test]
fn all_multimodal_handlers_reject_21_remote_sources_before_upstream_for_five_rounds() {
    run_handler_fixture_on_large_stack_thread("multimodal-handler-matrix", || async {
        const MESSAGE_PATHS: [&str; 5] = [
            "/v1/messages",
            "/cc/v1/messages",
            "/na/v1/messages",
            "/ha/v1/messages",
            "/dfcache/demo/v1/messages",
        ];
        const COUNT_TOKEN_PATHS: [&str; 5] = [
            "/v1/messages/count_tokens",
            "/cc/v1/messages/count_tokens",
            "/na/v1/messages/count_tokens",
            "/ha/v1/messages/count_tokens",
            "/dfcache/demo/v1/messages/count_tokens",
        ];
        let upstream = MultimodalHandlerUpstream::start().await;
        let Some(app) = multimodal_handler_test_router(&upstream.base_url).await else {
            return;
        };

        for round in 1..=5 {
            for path in MESSAGE_PATHS {
                let response = tokio::time::timeout(
                    Duration::from_secs(1),
                    app.clone().oneshot(multimodal_handler_request(
                        path,
                        remote_multimodal_limit_body(true),
                    )),
                )
                .await
                .unwrap_or_else(|_| panic!("round {round} path {path}: handler waited on I/O"))
                .expect("message handler response");
                assert_remote_multimodal_limit_response(response, path, round).await;
            }
            for path in COUNT_TOKEN_PATHS {
                let response = tokio::time::timeout(
                    Duration::from_secs(1),
                    app.clone().oneshot(multimodal_handler_request(
                        path,
                        remote_multimodal_limit_body(false),
                    )),
                )
                .await
                .unwrap_or_else(|_| panic!("round {round} path {path}: handler waited on I/O"))
                .expect("count_tokens handler response");
                assert_remote_multimodal_limit_response(response, path, round).await;
            }
        }

        assert_eq!(
            upstream.hits(),
            0,
            "remote source preflight must run before inference HTTP"
        );

        for round in 1..=5 {
            for path in COUNT_TOKEN_PATHS {
                let response = tokio::time::timeout(
                    Duration::from_secs(1),
                    app.clone().oneshot(multimodal_handler_request(
                        path,
                        inline_multimodal_body(false),
                    )),
                )
                .await
                .unwrap_or_else(|_| panic!("inline round {round} path {path}: handler timed out"))
                .expect("inline count_tokens response");
                assert_eq!(
                    response.status(),
                    StatusCode::OK,
                    "inline round {round} path {path}"
                );
                let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
                    .await
                    .expect("read inline count_tokens response");
                let value: Value = serde_json::from_slice(&body).expect("count_tokens JSON");
                assert!(
                    value["input_tokens"]
                        .as_i64()
                        .is_some_and(|tokens| tokens > 0),
                    "inline round {round} path {path}: {value}"
                );
            }
        }
        assert_eq!(
            upstream.hits(),
            0,
            "count_tokens inline controls must remain local"
        );

        for round in 1..=5 {
            for path in MESSAGE_PATHS {
                let response = tokio::time::timeout(
                    Duration::from_secs(2),
                    app.clone().oneshot(multimodal_handler_request(
                        path,
                        inline_multimodal_body(true),
                    )),
                )
                .await
                .unwrap_or_else(|_| {
                    panic!("inline message round {round} path {path}: handler timed out")
                })
                .expect("inline message response");
                assert_eq!(
                    response.status(),
                    StatusCode::OK,
                    "inline message round {round} path {path}"
                );
                let body = axum::body::to_bytes(response.into_body(), 256 * 1024)
                    .await
                    .expect("read inline message response");
                let value: Value = serde_json::from_slice(&body).expect("inline message JSON");
                assert_eq!(value["content"][0]["type"], "text");
                assert_eq!(value["content"][0]["text"], "inline-ok");
            }
        }
        assert_eq!(
            upstream.hits(),
            25,
            "five rounds across five inline message routes should each reach inference exactly once"
        );
    });
}

async fn assert_remote_multimodal_limit_response(response: Response, path: &str, round: usize) {
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "round {round} path {path}"
    );
    let request_id = response
        .headers()
        .get("request-id")
        .and_then(|value| value.to_str().ok())
        .expect("remote source limit response carries request-id")
        .to_string();
    assert_eq!(
        response
            .headers()
            .get("anthropic-request-id")
            .and_then(|value| value.to_str().ok()),
        Some(request_id.as_str()),
        "round {round} path {path}"
    );
    let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("read remote source limit response");
    let value: Value = serde_json::from_slice(&body).expect("Anthropic error JSON");
    assert_eq!(value["type"], "error", "round {round} path {path}");
    assert_eq!(
        value["error"]["type"], "invalid_request_error",
        "round {round} path {path}"
    );
    assert_eq!(value["request_id"], request_id, "round {round} path {path}");
    let message = value["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message
            .starts_with("remote image/document source count 21 exceeds the request limit of 20"),
        "round {round} path {path}: {message}"
    );
    // 入口拒绝统一带请求编号后缀，便于按日志定位。
    assert!(
        message.ends_with(&format!("(request-id: {request_id})")),
        "round {round} path {path}: {message}"
    );
    assert!(
        !String::from_utf8_lossy(&body).contains("b07-source-"),
        "round {round} path {path}: public error must not echo source URLs"
    );
}

#[test]
fn builtin_routes_follow_runtime_cache_and_prompt_config_matrix() {
    run_handler_fixture_on_large_stack_thread("route-policy-config-matrix", || async {
        let upstream = MultimodalHandlerUpstream::start().await;
        let config = route_policy_matrix_config(&upstream.base_url);
        let Some((app, usage_recorder)) =
            multimodal_handler_test_router_from_config(config.clone(), &upstream.base_url).await
        else {
            return;
        };

        let cc_policy = config.cache_policy_for_path("/cc/v1/messages");
        assert_eq!(
            cc_policy.policy.cache_type,
            PromptCacheStrategyType::NoCache
        );
        assert_eq!(cc_policy.namespace, None);
        let na_policy = config.cache_policy_for_path("/na/v1/messages");
        assert_eq!(
            na_policy.policy.cache_type,
            PromptCacheStrategyType::CurrentHighCache
        );
        assert_eq!(na_policy.namespace, None);
        let ha_policy = config.cache_policy_for_path("/ha/v1/messages");
        assert_eq!(
            ha_policy.policy.cache_type,
            PromptCacheStrategyType::CurrentHighCache
        );
        assert_eq!(ha_policy.namespace.as_deref(), Some("/ha"));

        let (v1_body, v1_record, v1_upstream_body) = route_policy_matrix_message_request(
            &app,
            &usage_recorder,
            &upstream,
            "/v1/messages",
            "shared",
        )
        .await;
        assert!(
            !v1_upstream_body.contains(ROUTE_POLICY_MATRIX_PROMPT_MARKER),
            "/v1 should not receive /ha prompt steering"
        );
        assert!(usage_field(&v1_body, "cache_creation_input_tokens") > 0);
        assert_eq!(usage_field(&v1_body, "cache_read_input_tokens"), 0);
        assert!(v1_record.cache_creation_input_tokens > 0);
        assert_eq!(v1_record.cache_read_input_tokens, 0);

        let (cc_body, cc_record, cc_upstream_body) = route_policy_matrix_message_request(
            &app,
            &usage_recorder,
            &upstream,
            "/cc/v1/messages",
            "shared",
        )
        .await;
        assert!(
            !cc_upstream_body.contains(ROUTE_POLICY_MATRIX_PROMPT_MARKER),
            "/cc should not receive /ha prompt steering"
        );
        assert_eq!(usage_field(&cc_body, "cache_creation_input_tokens"), 0);
        assert_eq!(usage_field(&cc_body, "cache_read_input_tokens"), 0);
        assert_eq!(cc_record.cache_creation_input_tokens, 0);
        assert_eq!(cc_record.cache_read_input_tokens, 0);

        let (na_body, na_record, na_upstream_body) = route_policy_matrix_message_request(
            &app,
            &usage_recorder,
            &upstream,
            "/na/v1/messages",
            "shared",
        )
        .await;
        assert!(
            !na_upstream_body.contains(ROUTE_POLICY_MATRIX_PROMPT_MARKER),
            "/na should not receive /ha prompt steering"
        );
        assert!(
            usage_field(&na_body, "cache_read_input_tokens") > 0,
            "/na is configured high-cache with shared namespace, so it should read the /v1 cache entry"
        );
        assert!(na_record.cache_read_input_tokens > 0);

        let (ha_first_body, ha_first_record, ha_first_upstream_body) =
            route_policy_matrix_message_request(
                &app,
                &usage_recorder,
                &upstream,
                "/ha/v1/messages",
                "shared",
            )
            .await;
        assert!(
            ha_first_upstream_body.contains(ROUTE_POLICY_MATRIX_PROMPT_MARKER),
            "/ha should receive configured prompt steering"
        );
        assert_eq!(
            usage_field(&ha_first_body, "cache_read_input_tokens"),
            0,
            "/ha is configured with independent namespace, so it must not read /v1 or /na cache entries"
        );
        assert!(usage_field(&ha_first_body, "cache_creation_input_tokens") > 0);
        assert_eq!(ha_first_record.cache_read_input_tokens, 0);
        assert!(ha_first_record.cache_creation_input_tokens > 0);

        let (ha_second_body, ha_second_record, ha_second_upstream_body) =
            route_policy_matrix_message_request(
                &app,
                &usage_recorder,
                &upstream,
                "/ha/v1/messages",
                "shared",
            )
            .await;
        assert!(ha_second_upstream_body.contains(ROUTE_POLICY_MATRIX_PROMPT_MARKER));
        assert!(
            usage_field(&ha_second_body, "cache_read_input_tokens") > 0,
            "second /ha request should read from its own independent namespace"
        );
        assert!(ha_second_record.cache_read_input_tokens > 0);

        let hits_before_count_tokens = upstream.hits();
        let v1_count = route_policy_matrix_count_tokens(&app, "/v1/messages/count_tokens").await;
        let cc_count = route_policy_matrix_count_tokens(&app, "/cc/v1/messages/count_tokens").await;
        let na_count = route_policy_matrix_count_tokens(&app, "/na/v1/messages/count_tokens").await;
        let ha_count = route_policy_matrix_count_tokens(&app, "/ha/v1/messages/count_tokens").await;
        assert_eq!(
            upstream.hits(),
            hits_before_count_tokens,
            "count_tokens must stay local for all built-in routes"
        );
        assert_eq!(
            cc_count, v1_count,
            "/cc should not receive /ha prompt steering"
        );
        assert_eq!(
            na_count, v1_count,
            "/na should not receive /ha prompt steering"
        );
        assert!(
            ha_count > v1_count,
            "/ha count_tokens should include configured prompt steering: ha={ha_count}, v1={v1_count}"
        );
    });
}

/// 逐项关闭：每次只关一个注入项，真实请求里只有这一项消失，其余文本逐字节不变。
#[test]
fn each_prompt_injection_item_switches_off_alone_in_the_real_request() {
    use crate::model::config::InjectionId;
    use crate::model::prompt_injection::{ItemSetting, TextKey};
    run_handler_fixture_on_large_stack_thread("prompt-injection-independence", || async {
        if crate::storage::integration_test_url("CLAUDE_POOL_TEST_POSTGRES_URL").is_none()
            || crate::storage::integration_test_url("CLAUDE_POOL_TEST_REDIS_URL").is_none()
        {
            return;
        }
        let upstream = MultimodalHandlerUpstream::start().await;
        let base = {
            let mut config = route_policy_matrix_config(&upstream.base_url);
            let injection = &mut config.prompt_injection;
            *injection = crate::model::config::PromptInjectionConfig::default();
            for id in InjectionId::ALL {
                injection.defaults.insert(id, ItemSetting { enabled: true });
            }
            injection
                .texts
                .insert(TextKey::Custom, "independence custom text".to_string());
            config
        };
        let request: super::injection_preview::PromptInjectionPreviewRequest =
            serde_json::from_value(json!({
                "route": "/v1",
                "model": "claude-sonnet-4-20250514",
                "toolChoice": "any",
            }))
            .unwrap();
        let payload =
            serde_json::to_string(&super::injection_preview::preview_payload(&request)).unwrap();
        let send = |config: Config| {
            let upstream = &upstream;
            let payload = payload.clone();
            async move {
                let (app, _usage) =
                    multimodal_handler_test_router_from_config(config, &upstream.base_url)
                        .await
                        .expect("handler test storage available");
                let index = upstream.hits();
                let response = app
                    .oneshot(multimodal_handler_request("/v1/messages", payload))
                    .await
                    .expect("message response");
                assert_eq!(response.status(), StatusCode::OK);
                upstream.bodies_snapshot()[index].clone()
            }
        };
        let all_on = send(base.clone()).await;
        // 这些项在这个请求里会出现，关掉后它们的文本必须从请求里消失。
        let markers = [
            (InjectionId::TaskQuality, "<task_quality_policy>"),
            (InjectionId::LanguageConstraint, "<language_constraint>"),
            (InjectionId::Custom, "independence custom text"),
            (InjectionId::IdentityPrompt, "<assistant_identity"),
        ];
        for (_, marker) in markers {
            assert!(
                all_on.contains(marker),
                "all-on request must contain {marker}"
            );
        }
        for (off, off_marker) in markers {
            let mut config = base.clone();
            config
                .prompt_injection
                .defaults
                .insert(off, ItemSetting { enabled: false });
            let body = send(config).await;
            assert!(
                !body.contains(off_marker),
                "{off:?} off: {off_marker} still injected"
            );
            for (other, marker) in markers {
                if other != off {
                    assert!(body.contains(marker), "{off:?} off removed {other:?}");
                }
            }
        }
    });
}

fn inline_multimodal_body(include_max_tokens: bool) -> String {
    const ONE_PIXEL_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
    let mut content = Vec::new();
    for index in 0..21 {
        content.push(json!({
            "type": "image",
            "source": {
                "type": "url",
                "url": format!("data:image/png;base64,{ONE_PIXEL_PNG}")
            }
        }));
        content.push(json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": "image/png",
                "data": ONE_PIXEL_PNG
            }
        }));
        content.push(json!({"type": "text", "text": format!("inline image pair {index}")}));
    }
    let mut body = json!({
        "model": "claude-sonnet-4-20250514",
        "messages": [{"role": "user", "content": content}]
    });
    if include_max_tokens {
        body["max_tokens"] = json!(16);
        body["stream"] = json!(false);
    }
    body.to_string()
}

/// The `usage` object of the first `message_start` event in an SSE body.
fn message_start_usage(body: &str) -> serde_json::Value {
    body.split("\n\n")
        .find(|chunk| chunk.starts_with("event: message_start"))
        .and_then(|chunk| chunk.lines().find_map(|line| line.strip_prefix("data: ")))
        .and_then(|data| serde_json::from_str::<serde_json::Value>(data).ok())
        .map(|event| event["message"]["usage"].clone())
        .expect("message_start with usage")
}

fn multimodal_handler_request(path: &str, body: String) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-api-key", "b07-handler-key")
        .body(Body::from(body))
        .expect("build multimodal handler request")
}

async fn multimodal_handler_test_router(base_url: &str) -> Option<Router> {
    Some(multimodal_handler_test_router_with_usage(base_url).await?.0)
}

async fn multimodal_handler_test_router_with_usage(
    base_url: &str,
) -> Option<(Router, Arc<UsageRecorder>)> {
    let mut config = Config::default();
    // 远程图片/文档源的物化与数量上限属于 imageProcessing 路由功能，默认关闭（透传）；
    // 这些多模态前置检查用例显式打开它。
    config
        .route_features
        .defaults
        .image_processing
        .get_or_insert_with(Default::default);

    config.routes.route_mut("/dfcache/demo").cache_policy = CacheRoutePolicyPatch {
        cache_type: Some(PromptCacheStrategyType::CurrentHighCache),
        route_namespace: Some(true),
        ..Default::default()
    };

    multimodal_handler_test_router_from_config(config, base_url).await
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
fn oversized_current_image_is_resized_when_switched_on() {
    run_handler_fixture_on_large_stack_thread(
        "oversized-image-resize",
        run_oversized_current_image_is_resized_when_switched_on,
    );
}

/// A noisy PNG over the 5 MB inline limit (noise does not compress, so the PNG stays large).
fn oversized_noise_png_base64() -> String {
    let (width, height) = (1600u32, 1200u32);
    let mut state = 0x2545_f491_u32;
    let image = image::RgbImage::from_fn(width, height, |_, _| {
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state & 0xff) as u8
        };
        image::Rgb([next(), next(), next()])
    });
    let mut png = Vec::new();
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("encode noise png");
    let encoded = BASE64_STANDARD.encode(png);
    assert!(encoded.len() > 5 * 1024 * 1024, "fixture must exceed 5 MB");
    encoded
}

fn parse_test_sse(body: &[u8]) -> Vec<(String, Value)> {
    String::from_utf8_lossy(body)
        .split("\n\n")
        .filter_map(|block| {
            let mut name = None;
            let mut data = None;
            for line in block.lines() {
                if let Some(value) = line.strip_prefix("event:") {
                    name = Some(value.trim().to_string());
                } else if let Some(value) = line.strip_prefix("data:") {
                    data = serde_json::from_str::<Value>(value.trim()).ok();
                }
            }
            Some((name?, data?))
        })
        .collect()
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

fn remote_multimodal_limit_body(include_max_tokens: bool) -> String {
    let content = (0..21)
        .map(|index| {
            let block_type = if index % 2 == 0 { "image" } else { "document" };
            json!({
                "type": block_type,
                "source": {
                    "type": "url",
                    "url": format!("https://b07-source-{index}.invalid/resource")
                }
            })
        })
        .collect::<Vec<_>>();
    let mut body = json!({
        "model": "claude-sonnet-4-20250514",
        "messages": [{"role": "user", "content": content}]
    });
    if include_max_tokens {
        body["max_tokens"] = json!(16);
        body["stream"] = json!(false);
    }
    body.to_string()
}

fn response_request_id(response: &Response) -> String {
    response
        .headers()
        .get("request-id")
        .and_then(|value| value.to_str().ok())
        .expect("response request-id")
        .to_string()
}

fn route_policy_matrix_config(_base_url: &str) -> Config {
    let mut config = Config::default();

    config.routes.route_mut(&"/cc".to_string()).cache_policy = CacheRoutePolicyPatch {
        cache_type: Some(PromptCacheStrategyType::NoCache),
        ..CacheRoutePolicyPatch::default()
    };
    config.routes.route_mut(&"/na".to_string()).cache_policy = CacheRoutePolicyPatch {
        cache_type: Some(PromptCacheStrategyType::CurrentHighCache),
        route_namespace: Some(false),
        ..CacheRoutePolicyPatch::default()
    };
    config.routes.route_mut(&"/ha".to_string()).cache_policy = CacheRoutePolicyPatch {
        cache_type: Some(PromptCacheStrategyType::CurrentHighCache),
        route_namespace: Some(true),
        ..CacheRoutePolicyPatch::default()
    };

    // 只有 /ha 显式开启自定义注入项；其他注入项全部关闭。
    {
        use crate::model::config::InjectionId;
        use crate::model::prompt_injection::{ItemOverride, ItemSetting, TextKey};
        let injection = &mut config.prompt_injection;
        for id in InjectionId::ALL {
            injection
                .defaults
                .insert(id, ItemSetting { enabled: false });
        }
        config.routes.route_mut(&"/ha".to_string()).prompt_injection = [(
            InjectionId::Custom,
            ItemOverride {
                enabled: Some(true),
            },
        )]
        .into();
        injection.texts.insert(
            TextKey::Custom,
            format!(
                "{ROUTE_POLICY_MATRIX_PROMPT_MARKER} {}",
                "extra route policy prompt tokens ".repeat(50)
            ),
        );
    }

    config
}

async fn route_policy_matrix_count_tokens(app: &Router, path: &str) -> i64 {
    let response = app
        .clone()
        .oneshot(multimodal_handler_request(
            path,
            route_policy_matrix_count_tokens_body(),
        ))
        .await
        .expect("route policy matrix count_tokens response");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{path} count_tokens status"
    );
    let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("read route policy matrix count_tokens response");
    let value: Value = serde_json::from_slice(&body).expect("route policy count_tokens JSON");
    value["input_tokens"]
        .as_i64()
        .unwrap_or_else(|| panic!("{path}: missing input_tokens: {value}"))
}

fn route_policy_matrix_count_tokens_body() -> String {
    json!({
        "model": "claude-sonnet-4-20250514",
        "system": "stable route policy count tokens system prompt ".repeat(20),
        "messages": [{"role": "user", "content": "same count_tokens payload"}]
    })
    .to_string()
}

async fn route_policy_matrix_message_request(
    app: &Router,
    usage_recorder: &UsageRecorder,
    upstream: &MultimodalHandlerUpstream,
    path: &str,
    label: &str,
) -> (Value, UsageRecord, String) {
    let body_index = upstream.hits();
    let response = app
        .clone()
        .oneshot(multimodal_handler_request(
            path,
            route_policy_matrix_messages_body(label),
        ))
        .await
        .expect("route policy matrix message response");
    assert_eq!(response.status(), StatusCode::OK, "{path} message status");
    let request_id = response_request_id(&response);
    let body = axum::body::to_bytes(response.into_body(), 256 * 1024)
        .await
        .expect("read route policy matrix message response");
    let value: Value = serde_json::from_slice(&body).expect("route policy matrix message JSON");
    assert_eq!(value["content"][0]["text"], "inline-ok", "{path}");
    let record = usage_record_for_request(usage_recorder, &request_id);
    assert_eq!(record.endpoint, path, "{path} usage endpoint");
    assert_eq!(
        record.route_kind,
        Some(UsageRouteKind::Account),
        "{path} route kind"
    );
    let bodies = upstream.bodies_snapshot();
    let upstream_body = bodies
        .get(body_index)
        .unwrap_or_else(|| panic!("{path}: upstream body at index {body_index} missing"))
        .clone();
    (value, record, upstream_body)
}

fn route_policy_matrix_messages_body(label: &str) -> String {
    json!({
        "model": "claude-sonnet-4-20250514",
        "max_tokens": 32,
        "stream": false,
        "system": [{
            "type": "text",
            "text": "stable route policy cache system prompt ".repeat(700),
            "cache_control": {"type": "ephemeral"}
        }],
        "metadata": {
            "user_id": r#"{"session_id":"11111111-2222-4333-8444-555555555555"}"#
        },
        "messages": [{"role": "user", "content": format!("route policy matrix {label}")}]
    })
    .to_string()
}

async fn run_strict_request_protocol_contamination_fails_closed_before_upstream_for_five_rounds() {
    let upstream = MultimodalHandlerUpstream::start().await;
    let mut config = Config::default();

    config.compat_profile = CompatProfile::AnthropicStrict;
    config.route_features.defaults.history_sanitize = true;
    let (app, usage_recorder) = handler_test_router(config, None);

    for round in 1..=5 {
        let hidden = format!("credential-like-output-{round}");
        let response = app
            .clone()
            .oneshot(multimodal_handler_request(
                "/v1/messages",
                json!({
                    "model": "claude-sonnet-4-20250514",
                    "max_tokens": 32,
                    "stream": false,
                    "messages": [{
                        "role": "assistant",
                        "content": format!("safe prefix\nuser Continue\n\nbashHashd1e9567d: {hidden}")
                    }],
                    "tools": [{"name": "bashHashd1e9567d", "input_schema": {"type": "object"}}]
                })
                .to_string(),
            ))
            .await
            .expect("strict contamination response");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "round {round}");
        let request_id = response
            .headers()
            .get("request-id")
            .and_then(|value| value.to_str().ok())
            .expect("request-id")
            .to_string();
        assert_eq!(
            response
                .headers()
                .get("anthropic-request-id")
                .and_then(|value| value.to_str().ok()),
            Some(request_id.as_str())
        );
        assert_eq!(
            response
                .headers()
                .get("x-error-id")
                .and_then(|value| value.to_str().ok()),
            Some(request_id.as_str())
        );
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("strict contamination body");
        let body_text = String::from_utf8(body.to_vec()).expect("utf-8 error body");
        let value: Value = serde_json::from_str(&body_text).expect("error JSON");
        assert_eq!(value["request_id"], request_id);
        assert!(
            value["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains(&request_id))
        );
        assert!(!body_text.contains(&hidden));
        assert!(!body_text.contains("bashHashd1e9567d"));

        let records = usage_recorder.query(UsageRecordQuery {
            request_id: Some(request_id.clone()),
            ..UsageRecordQuery::default()
        });
        assert_eq!(records.records.len(), 1, "round {round}");
        let record = records.records.first().expect("strict error usage record");
        assert_eq!(record.status, UsageRecordStatus::Error);
        assert_eq!(record.error_source.as_deref(), Some("request_rejection"));
        assert_eq!(record.error_status_code, Some(400));
        assert_eq!(record.error_id.as_deref(), Some(request_id.as_str()));
        assert_eq!(record.total_input_tokens, 0);
        assert_eq!(record.output_tokens, 0);
        let metadata = record.error_metadata.as_ref().expect("error metadata");
        assert_eq!(metadata["stage"], "request_entry");
        assert_eq!(metadata["sampled"], true);
        assert_eq!(metadata["observedCountIsExact"], false);
        assert_eq!(metadata["observedCount"], round);
        assert_eq!(
            metadata.get("reason").and_then(Value::as_str),
            Some("strict_request_protocol_contamination")
        );
        let serialized_record = serde_json::to_string(record).expect("serialize usage record");
        assert!(!serialized_record.contains(&hidden));
        assert!(!serialized_record.contains("bashHashd1e9567d"));
    }

    assert_eq!(
        upstream.hits(),
        0,
        "strict contamination must never hit upstream"
    );
}

#[test]
fn strict_request_protocol_contamination_fails_closed_before_upstream_for_five_rounds() {
    run_handler_fixture_on_large_stack_thread("strict-contamination-matrix", || async {
        run_strict_request_protocol_contamination_fails_closed_before_upstream_for_five_rounds()
            .await;
    });
}

fn usage_field(value: &Value, field: &str) -> i64 {
    value["usage"][field]
        .as_i64()
        .unwrap_or_else(|| panic!("missing usage.{field}: {value}"))
}

fn usage_numbers(value: &serde_json::Value) -> Vec<i64> {
    match value {
        serde_json::Value::Number(number) => vec![number.as_i64().unwrap_or(-1)],
        serde_json::Value::Object(map) => map.values().flat_map(usage_numbers).collect(),
        _ => Vec::new(),
    }
}

fn usage_record_for_request(recorder: &UsageRecorder, request_id: &str) -> UsageRecord {
    let records = recorder.query(UsageRecordQuery {
        request_id: Some(request_id.to_string()),
        ..UsageRecordQuery::default()
    });
    assert_eq!(records.records.len(), 1, "one usage record per request");
    records.records.into_iter().next().unwrap()
}

fn parse_messages_payload(raw_body: &Bytes) -> Result<MessagesRequest, Response> {
    let request_id = envelope::request_id();
    request_entry::parse_messages_payload(raw_body, &request_id)
        .map_err(|error| error.to_response(&request_id))
}

const ROUTE_POLICY_MATRIX_PROMPT_MARKER: &str = "ROUTE_POLICY_MATRIX_PROMPT_20260803";

fn run_handler_fixture_on_large_stack_thread<F, Fut>(thread_name: &'static str, fixture: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + 'static,
{
    std::thread::Builder::new()
        .name(thread_name.to_string())
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build handler fixture runtime");
            runtime.block_on(fixture());
        })
        .expect("spawn handler fixture thread")
        .join()
        .expect("run handler fixture thread");
}

/// With image resizing on, a current-turn image over the 5 MB limit is shrunk and sent instead
/// of being rejected; with it off the request is still the official 400 and never reaches the
/// upstream.
async fn run_oversized_current_image_is_resized_when_switched_on() {
    let data = oversized_noise_png_base64();
    // (imageProcessing 功能是否开启, resize 是否开启)。功能关闭时按协议透传，上游说了算。
    for (feature_on, resize) in [(false, false), (true, false), (true, true)] {
        let upstream = MultimodalHandlerUpstream::start().await;
        let mut config = Config::default();
        if feature_on {
            config
                .route_features
                .defaults
                .image_processing
                .get_or_insert_with(Default::default)
                .resize_enabled = resize;
        }
        let Some((app, _usage_recorder)) =
            multimodal_handler_test_router_from_config(config, &upstream.base_url).await
        else {
            return;
        };
        let request = multimodal_handler_request(
            "/v1/messages",
            json!({
                "model": "claude-sonnet-4-20250514",
                "max_tokens": 256,
                "messages": [{"role": "user", "content": [
                    {"type": "text", "text": "describe"},
                    {"type": "image", "source": {
                        "type": "base64", "media_type": "image/png", "data": data
                    }}
                ]}]
            })
            .to_string(),
        );
        let response = tokio::time::timeout(Duration::from_secs(60), app.oneshot(request))
            .await
            .expect("oversized image response timed out")
            .expect("oversized image response");
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("read body");
        let body = String::from_utf8_lossy(&body);
        if !feature_on {
            // 透传：不改写也不拦截，原图原样到达上游。
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(upstream.hits(), 1);
            let sent = upstream.bodies_snapshot()[0].to_string();
            assert!(
                sent.len() > 5 * 1024 * 1024,
                "passthrough body is {} bytes",
                sent.len()
            );
        } else if resize {
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(upstream.hits(), 1);
            let sent = upstream.bodies_snapshot()[0].to_string();
            assert!(
                sent.len() < 5 * 1024 * 1024,
                "resized body is {} bytes",
                sent.len()
            );
        } else {
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert!(body.contains("image exceeds 5 MB maximum"), "{body}");
            assert_eq!(upstream.hits(), 0);
        }
    }
}

fn runtime_config_for_payload_guard(
    mode: PayloadGuardMode,
    enabled: bool,
    max_bytes: usize,
) -> RequestRuntimeConfig {
    let state = AppState::new(
        Arc::new(RequestApiKeyStore::new(["test-key"])),
        Arc::new(UsageRecorder::new(10)),
        Arc::new(PromptCacheTracker::default()),
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::HighCache,
    );
    let mut runtime_config = RequestRuntimeConfig::from_app_state(&state);
    runtime_config.payload_guard_enabled = enabled;
    runtime_config.payload_guard_mode = mode;
    runtime_config.payload_guard_max_bytes = max_bytes;
    runtime_config.payload_guard_safety_margin_bytes = 0;
    runtime_config.payload_guard_trim_history = true;
    runtime_config.payload_guard_external_enabled = true;
    runtime_config
}

#[derive(Clone, Default)]
struct MultimodalHandlerUpstreamState {
    hits: Arc<AtomicUsize>,
    bodies: Arc<StdMutex<Vec<String>>>,
}

struct MultimodalHandlerUpstream {
    base_url: String,
    state: MultimodalHandlerUpstreamState,
    task: tokio::task::JoinHandle<()>,
}

impl MultimodalHandlerUpstream {
    async fn start() -> Self {
        let state = MultimodalHandlerUpstreamState::default();
        let app = Router::new()
            .route("/v1/messages", post(multimodal_handler_upstream))
            // 透传用例会把 >5MB 的内联图片原样发到这里，超过 axum 默认 2MB 的 body 上限。
            .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind multimodal handler upstream probe");
        let address = listener
            .local_addr()
            .expect("multimodal handler upstream probe address");
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve multimodal handler upstream probe");
        });
        Self {
            base_url: format!("http://{address}"),
            state,
            task,
        }
    }

    fn hits(&self) -> usize {
        self.state.hits.load(Ordering::Acquire)
    }

    fn bodies_snapshot(&self) -> Vec<String> {
        self.state
            .bodies
            .lock()
            .expect("upstream bodies lock")
            .clone()
    }
}

impl Drop for MultimodalHandlerUpstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn multimodal_handler_upstream(
    State(state): State<MultimodalHandlerUpstreamState>,
    body: Bytes,
) -> Response {
    state.hits.fetch_add(1, Ordering::AcqRel);
    state
        .bodies
        .lock()
        .expect("upstream bodies lock")
        .push(String::from_utf8_lossy(&body).to_string());
    Json(json!({
        "id": "msg_inline", "type": "message", "role": "assistant", "model": "claude-sonnet-4-20250514",
        "content": [{"type": "text", "text": "inline-ok"}], "stop_reason": "end_turn", "stop_sequence": null,
        "usage": {"input_tokens": 100, "output_tokens": 3, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}
    })).into_response()
}

async fn multimodal_handler_test_router_from_config(
    mut config: Config,
    base_url: &str,
) -> Option<(Router, Arc<UsageRecorder>)> {
    let manager = test_account_manager_for_handlers(base_url).await?;
    config.accounts.enabled = true;
    config.accounts.same_pool_retry_count = 0;
    Some(handler_test_router(config, Some(manager)))
}

fn handler_test_router(
    config: Config,
    manager: Option<Arc<AccountManager>>,
) -> (Router, Arc<UsageRecorder>) {
    let usage_recorder = Arc::new(UsageRecorder::new(1_000));
    let router = create_router(AnthropicRouterDependencies {
        runtime: crate::model::runtime::RuntimeConfigStore::new(config, None),
        request_api_keys: Arc::new(RequestApiKeyStore::new(["b07-handler-key"])),
        request_admission: Arc::new(RequestAdmissionController::new(
            RequestAdmissionConfig::disabled(),
        )),
        usage_recorder: usage_recorder.clone(),
        prompt_cache: Arc::new(PromptCacheTracker::default()),
        prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
        pricing_catalog: Arc::new(PricingCatalog::new()),
        model_capabilities: Arc::new(ModelCapabilitiesCatalog::new()),
        account_manager: manager,
    });
    (router, usage_recorder)
}

async fn test_account_manager_for_handlers(base_url: &str) -> Option<Arc<AccountManager>> {
    let Some(postgres_url) = crate::storage::integration_test_url("CLAUDE_POOL_TEST_POSTGRES_URL")
    else {
        eprintln!("跳过 账号池入口 集成测试：未设置 CLAUDE_POOL_TEST_POSTGRES_URL");
        return None;
    };
    let Some(redis_url) = crate::storage::integration_test_url("CLAUDE_POOL_TEST_REDIS_URL") else {
        eprintln!("跳过 账号池入口 集成测试：未设置 CLAUDE_POOL_TEST_REDIS_URL");
        return None;
    };

    let mut postgres_config = Config::default();
    postgres_config.postgres.url = Some(postgres_url);
    postgres_config.postgres.max_connections = 2;
    let postgres = Arc::new(
        PostgresStore::connect_test(&postgres_config)
            .await
            .expect("connect handler account pool test Postgres"),
    );

    let mut redis_config = Config::default();
    redis_config.redis.url = Some(redis_url);
    redis_config.redis.key_prefix = format!(
        "claude_pool:test:handlers:messages:{}",
        uuid::Uuid::new_v4()
    );
    let redis = Arc::new(
        RedisStore::connect(&redis_config)
            .await
            .expect("connect handler account pool test Redis"),
    );

    let manager = Arc::new(AccountManager::new(postgres.clone(), redis));
    postgres
        .create_account(CreateAccountRequest {
            tags: Vec::new(),
            proxy_url: None,
            cost_multiplier: 1.0,
            overrides: Default::default(),
            name: "handler-messages".to_string(),
            base_url: base_url.to_string(),
            api_key: "sk-handler-external-test".to_string(),
            auth_type: AccountAuthType::XApiKey,
            header_profile: AccountHeaderProfile::Generic,
            append_beta_query: false,
            header_overrides: HashMap::new(),
            wire_profile: AccountWireProfile::Default,
            tls_profile: AccountTlsProfile::Default,
            enabled: true,
            priority: 1,
            max_concurrent_requests: 10,
            usage_projection_mode: UsageProjectionMode::CurrentPathPolicy,
            stream_response_mode: None,

            auto_disable_policy: AutoDisablePolicy::Inherit,
            pre_output_stream_retry_mode: StreamRetryMode::Inherit,

            normalize_model_version_dots: false,
            model_mapping_mode: ModelMappingMode::PassthroughMapping,
            model_mapping_require_match: false,
            model_mapping_rules: Vec::new(),
            supported_models: Vec::new(),
            route_mode: crate::model::config::RouteRuleMode::AllowAll,
            route_rules: Vec::new(),
            notes: None,
        })
        .await
        .expect("create handler account pool pool");
    manager.invalidate_static_pool_snapshot();
    Some(manager)
}

#[tokio::test]
async fn content_length_threshold_error_is_not_reported_as_context_window_full() {
    let response = AccountFinalError {
        status: StatusCode::BAD_REQUEST,
        response_error_type: "invalid_request_error".to_string(),
        route_error_type: "bad_request".to_string(),
        message: r#"流式 API 请求失败（凭据 #1 test@example.com）: 400 Bad Request {"message":"Input is too long.","reason":"CONTENT_LENGTH_EXCEEDS_THRESHOLD"}"#.to_string(),
        error_id: "err_test_payload_limit".to_string(),
        retryable: false,
        attempts: Vec::new(),
        pool_id: None,
        pool_name: None,
    }.into_response("req_test_payload_limit");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    let message = value
        .pointer("/error/message")
        .and_then(|v| v.as_str())
        .expect("error message");

    assert!(message.starts_with("prompt is too long"), "{message}");
    // 账号池路径的公开文案（原 上游 路径写的是 "content length limit"）。
    assert!(message.contains("content length exceeded"), "{message}");
    assert!(message.contains("separate from the model context window"));
    assert!(!message.contains("Context window is full"));
}

#[tokio::test]
async fn prompt_too_long_error_maps_to_input_length_message() {
    let response = AccountFinalError {
        status: StatusCode::BAD_REQUEST,
        response_error_type: "invalid_request_error".to_string(),
        route_error_type: "bad_request".to_string(),
        message: r#"流式 API 请求失败（账号 #9 hidden）: 400 Bad Request {"error":{"message":"prompt is too long: > 1000000 maximum","type":"invalid_request_error"},"type":"error"}"#.to_string(),
        error_id: "err_test_payload_limit".to_string(),
        retryable: false,
        attempts: Vec::new(),
        pool_id: None,
        pool_name: None,
    }.into_response("req_test_payload_limit");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");
    let message = value
        .pointer("/error/message")
        .and_then(|v| v.as_str())
        .expect("error message");

    assert!(message.starts_with("prompt is too long"), "{message}");
    assert!(!message.contains("hidden"));
    assert!(!message.contains("1000000"));
}

#[tokio::test]
async fn account_endpoint_gate_applies_global_enable_and_route_policy() {
    let upstream = MultimodalHandlerUpstream::start().await;
    let Some(manager) = test_account_manager_for_handlers(&upstream.base_url).await else {
        return;
    };
    // 账号池是唯一上游路径，默认开启；显式关闭总开关后请求必须被拒。
    let mut config = AccountsConfig::default();
    assert!(config.enabled);
    assert!(account_enabled_for_endpoint(&config, "/cc/v1/messages", &manager).await);

    config.enabled = false;
    assert!(!account_enabled_for_endpoint(&config, "/cc/v1/messages", &manager).await);

    config.enabled = true;
    assert!(account_enabled_for_endpoint(&config, "/cc/v1/messages", &manager).await);

    config.routes.route_mut("/cc").account_rule = crate::model::routes::RouteRule {
        mode: crate::model::config::RouteRuleMode::DenyList,
        rules: vec!["*".into()],
    };
    assert!(!account_enabled_for_endpoint(&config, "/cc/v1/messages", &manager).await);
    assert!(account_enabled_for_endpoint(&config, "/v1/messages", &manager).await);

    for route in &mut config.routes.routes {
        route.account_rule = crate::model::routes::RouteRule {
            mode: crate::model::config::RouteRuleMode::DenyList,
            rules: vec!["*".into()],
        };
    }
    config.routes.route_mut("/dfcache/team-a").account_rule = Default::default();
    assert!(!account_enabled_for_endpoint(&config, "/v1/messages", &manager).await);
    assert!(account_enabled_for_endpoint(&config, "/dfcache/team-a/v1/messages", &manager).await);
}

async fn account_enabled_for_endpoint(
    accounts: &AccountsConfig,
    endpoint: &str,
    manager: &Arc<AccountManager>,
) -> bool {
    let mut config = Config::default();
    config.accounts = accounts.clone();
    config.routes = accounts.routes.clone();
    let (app, _usage) = handler_test_router(config, Some(manager.clone()));
    let response = app
        .oneshot(multimodal_handler_request(
            endpoint,
            json!({
                "model": "claude-sonnet-4-20250514",
                "max_tokens": 16,
                "messages": [{"role": "user", "content": "hello"}]
            })
            .to_string(),
        ))
        .await
        .expect("endpoint gate response");
    response.status() == StatusCode::OK
}

#[tokio::test]
async fn custom_route_http_entrypoints_enforce_definition_and_enabled_state() {
    let mut config = Config::default();
    config.routes.route_mut("/dfcache/team");
    config.routes.route_mut("/dfcache/disabled").enabled = false;
    config.routes.route_mut("/cc").enabled = false;
    let (app, _) = handler_test_router(config, None);
    for (prefix, enabled) in [
        ("/dfcache/team", true),
        // 根级前缀从来不是入口：即使想定义也会被校验拒绝，这里只能是 404。
        ("/team", false),
        ("/missing", false),
        ("/dfcache/missing", false),
        ("/dfcache/disabled", false),
        ("/cc", false),
    ] {
        for (method, suffix) in [
            ("GET", "models"),
            ("GET", "models/unknown"),
            ("POST", "messages"),
            ("POST", "messages/count_tokens"),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(format!("{prefix}/v1/{suffix}"))
                        .header("x-api-key", "b07-handler-key")
                        .header("content-type", "application/json")
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
                .await
                .unwrap();
            if enabled && method == "POST" {
                assert_ne!(status, StatusCode::NOT_FOUND, "{prefix}/{suffix}");
                continue;
            }
            let value: serde_json::Value = serde_json::from_slice(&body).unwrap_or_else(|error| {
                panic!(
                    "{prefix}/{suffix}: {status}: {error}: {}",
                    String::from_utf8_lossy(&body)
                )
            });
            if !enabled {
                assert_eq!(status, StatusCode::NOT_FOUND, "{prefix}/{suffix}");
                assert_eq!(value["error"]["type"], "not_found_error");
                // /dfcache 下未定义或已停用的名字走路由层，报“未配置或已停用”；根级未知前缀
                // 根本不进路由层，是普通 404。
                if prefix.starts_with("/dfcache/") || prefix == "/cc" {
                    assert!(
                        value["error"]["message"]
                            .as_str()
                            .unwrap()
                            .contains("route is not configured"),
                        "{prefix}/{suffix}: {}",
                        value["error"]["message"]
                    );
                }
            } else if suffix == "models" {
                assert_eq!(status, StatusCode::OK, "{prefix}");
                assert_eq!(value["data"], json!([]));
            } else if suffix == "models/unknown" {
                assert_eq!(status, StatusCode::NOT_FOUND);
                assert!(
                    value["error"]["message"]
                        .as_str()
                        .unwrap()
                        .contains("model: unknown")
                );
            } else {
                assert_ne!(status, StatusCode::NOT_FOUND, "{prefix}/{suffix}");
            }
        }
    }
}
