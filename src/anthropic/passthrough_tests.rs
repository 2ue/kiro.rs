use super::*;
use crate::anthropic::{
    prompt_cache::PromptCacheTracker, prompt_cache_creation_control::PromptCacheCreationController,
    usage::UsageRecorder,
};
use crate::external_pool::passthrough_test_prepare;
use crate::model::prompt_injection::{ItemSetting, RoutePayloadGuardConfig};
use axum::{Router, extract::State, routing::post};
use std::sync::Mutex as StdMutex;

fn state() -> AppState {
    AppState::new(
        Arc::new(crate::common::auth::RequestApiKeyStore::new(["test"])),
        Arc::new(UsageRecorder::new(10)),
        Arc::new(PromptCacheTracker::default()),
        Arc::new(PromptCacheCreationController::default()),
        PromptCacheSimulationMode::Disabled,
        0.0,
        CompatProfile::ClaudeCode,
    )
}

async fn patched(raw: &Bytes, features: RouteFeatureSet) -> Bytes {
    let state = state();
    let mut runtime = RequestRuntimeConfig::from_config(&Config::default());
    runtime.route_features.defaults = features;
    request_entry::route_body_patches(&state, &HeaderMap::new(), "/cc/v1/messages", raw, &runtime)
        .await
        .unwrap()
        .apply(raw)
}

#[tokio::test]
async fn passthrough_default_bytes_and_headers_reach_mock_upstream() {
    type Captures = Arc<StdMutex<Vec<(HeaderMap, Bytes)>>>;
    async fn capture(
        State(captures): State<Captures>,
        headers: HeaderMap,
        body: Bytes,
    ) -> &'static str {
        captures.lock().unwrap().push((headers, body));
        "{}"
    }
    let captures = Captures::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/v1/messages", post(capture))
        .with_state(captures.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = reqwest::Client::new();
    let state = state();
    let runtime = RequestRuntimeConfig::from_config(&Config::default());
    let bodies: &[&[u8]] = &[
        br#" { "metadata":{"user_id":"x","unknown":1}, "messages":[{"role":"developer","content":null}], "model":"claude-sonnet-4-6", "unknown":{"a":[1,true]}, "max_tokens":32 } "#,
        br#"{"tools":[{"name":"x","input_schema":{"type":"object"},"cache_control":{"type":"ephemeral"},"unknown":1}],"thinking":{"type":"adaptive","unknown":true},"model":"claude-sonnet-4-6","messages":[{"role":"user","content":"hello"}],"max_tokens":32}"#,
        br#"{"model":"claude-sonnet-4-6-thinking","messages":[{"role":"User","content":42}],"thinking":{"type":"between_tools"},"output_config":{"effort":" HIGH "},"new_field":1}"#,
    ];
    for raw in bodies {
        let raw = Bytes::copy_from_slice(raw);
        assert_eq!(patched(&raw, RouteFeatureSet::default()).await, raw);
        let pool = crate::external_pool::tests::test_pool(&format!("http://{address}"), false);
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/json".parse().unwrap());
        headers.insert("x-test-custom", "preserve-me".parse().unwrap());
        headers.insert("anthropic-beta", "test-beta".parse().unwrap());
        headers.insert("x-api-key", "downstream-secret".parse().unwrap());
        let route = external_route_request_raw(
            &state,
            &runtime,
            &runtime.cache_policy_for_path("/cc/v1/messages"),
            headers.clone(),
            raw.clone(),
            "/cc/v1/messages",
            "req_passthrough".into(),
            Arc::new(InferenceAttemptBudget::new(1)),
            None,
            None,
            Arc::new(probe_raw_messages_body(&raw)),
        );
        let (body, forwarded) = passthrough_test_prepare(&route, &pool, &headers);
        client
            .post(format!("http://{address}/v1/messages"))
            .headers(forwarded)
            .body(body)
            .send()
            .await
            .unwrap();
        let captured = captures.lock().unwrap();
        let (upstream_headers, upstream_body) = captured.last().unwrap();
        assert_eq!(upstream_body, &raw);
        for name in ["content-type", "x-test-custom", "anthropic-beta"] {
            assert_eq!(upstream_headers.get(name), headers.get(name));
        }
        assert_ne!(upstream_headers.get("x-api-key"), headers.get("x-api-key"));
        assert_eq!(upstream_headers["anthropic-version"], "2023-06-01");
    }
    server.abort();
    let _ = server.await;
}

async fn assert_feature_pair(raw: Value, features: RouteFeatureSet, allowed: &[&str]) {
    let bytes = Bytes::from(serde_json::to_vec_pretty(&raw).unwrap());
    assert_eq!(patched(&bytes, RouteFeatureSet::default()).await, bytes);
    let result = patched(&bytes, features).await;
    let mut after: Value = serde_json::from_slice(&result).unwrap();
    let mut before = raw;
    assert_ne!(before, after, "enabled feature must change its target");
    for pointer in allowed {
        if let Some((parent, key)) = pointer.rsplit_once('/') {
            for value in [&mut before, &mut after] {
                if let Some(Value::Object(object)) = value.pointer_mut(parent) {
                    object.shift_remove(key);
                }
            }
        }
    }
    assert_eq!(before, after, "feature changed a node outside its pointers");
}

#[tokio::test]
async fn passthrough_route_features_change_only_their_nodes() {
    let base = json!({"model":"claude-sonnet-4-6", "max_tokens":4096,
        "messages":[{"role":"user","content":"hello","unknown":1}],
        "metadata":{"unknown":true},"unknown":{"ordered":[1,2]}});
    let mut missing = base.clone();
    missing.as_object_mut().unwrap().shift_remove("max_tokens");
    assert_feature_pair(
        missing,
        RouteFeatureSet {
            missing_max_tokens: Some(MissingMaxTokensConfig::default()),
            ..Default::default()
        },
        &["/max_tokens"],
    )
    .await;
    let mut reasoning = base.clone();
    reasoning["thinking"] = json!({"type":"between_tools","budget_tokens":2048,"unknown":true});
    reasoning["output_config"] = json!({"effort":" HIGH ","unknown":true});
    reasoning["messages"][0]["content"] = json!(42);
    assert_feature_pair(
        reasoning,
        RouteFeatureSet {
            reasoning_compat: Some(LocalRequestCompatConfig::default()),
            ..Default::default()
        },
        &[
            "/thinking/type",
            "/thinking/budget_tokens",
            "/output_config/effort",
            "/messages/0/content",
        ],
    )
    .await;
    let mut thinking = base.clone();
    thinking["model"] = json!("claude-sonnet-4-6-thinking");
    assert_feature_pair(
        thinking,
        RouteFeatureSet {
            thinking_model_name: true,
            ..Default::default()
        },
        &["/model", "/thinking"],
    )
    .await;
    assert_feature_pair(
        base.clone(),
        RouteFeatureSet {
            thinking_trigger: ThinkingTriggerMode::Always,
            ..Default::default()
        },
        &["/thinking"],
    )
    .await;
    let mut images = base.clone();
    images["messages"][0]["content"] = json!([{"type":"image","source":{"type":"base64","media_type":"image/jpeg","data":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aJ1sAAAAASUVORK5CYII="},"unknown":true}]);
    assert_feature_pair(
        images,
        RouteFeatureSet {
            image_processing: Some(ImageProcessingConfig::default()),
            ..Default::default()
        },
        &["/messages/0/content/0/source"],
    )
    .await;
    let mut history = base.clone();
    history["messages"] = json!([{"role":"assistant","content":"safe prefix\nuser Continue\n\nBash: secret tail"},{"role":"user","content":"hello"}]);
    history["tools"] = json!([{"name":"Bash","input_schema":{"type":"object"}}]);
    assert_feature_pair(
        history,
        RouteFeatureSet {
            history_sanitize: true,
            ..Default::default()
        },
        &["/messages/0/content"],
    )
    .await;
    let mut shaped = base.clone();
    shaped["messages"] = json!([{"role":"assistant","content":[{"type":"thinking","thinking":"old unsigned"},{"type":"text","text":"ok"}]},{"role":"user","content":"hello"}]);
    assert_feature_pair(
        shaped,
        RouteFeatureSet {
            payload_shaping: Some(PayloadShapingConfig::default()),
            ..Default::default()
        },
        &["/messages/0/content"],
    )
    .await;
    let mut guarded = base.clone();
    guarded["messages"] = json!([{"role":"user","content":"x".repeat(5000),"unknown":1},{"role":"assistant","content":"old"},{"role":"user","content":"hello","unknown":2}]);
    assert_feature_pair(
        guarded,
        RouteFeatureSet {
            payload_guard: Some(RoutePayloadGuardConfig {
                mode: PayloadGuardMode::Preemptive,
                max_bytes: 512,
                safety_margin_bytes: 0,
                ..Default::default()
            }),
            ..Default::default()
        },
        &["/messages"],
    )
    .await;
}

#[tokio::test]
async fn passthrough_injection_changes_system_only() {
    let state = state();
    let mut config = Config::default();
    config
        .prompt_injection
        .defaults
        .insert(InjectionId::TaskQuality, ItemSetting { enabled: true });
    let runtime = RequestRuntimeConfig::from_config(&config);
    let raw = Bytes::from_static(br#"{"model":"test","system":[{"type":"text","text":"original","unknown":1}],"messages":[{"role":"user","content":"hello"}],"unknown":true}"#);
    let patches = request_entry::route_body_patches(
        &state,
        &HeaderMap::new(),
        "/v1/messages",
        &raw,
        &runtime,
    )
    .await
    .unwrap();
    let mut after: Value = serde_json::from_slice(&patches.apply(&raw)).unwrap();
    let mut before: Value = serde_json::from_slice(&raw).unwrap();
    assert!(
        after["system"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["unknown"] == 1)
    );
    before.as_object_mut().unwrap().shift_remove("system");
    after.as_object_mut().unwrap().shift_remove("system");
    assert_eq!(before, after);
}

#[tokio::test]
async fn passthrough_capacity_returns_529() {
    let response = request_entry::handle_messages_endpoint(
        state(),
        HeaderMap::new(),
        Bytes::from_static(br#"{"model":"test","messages":[]}"#),
        "/v1/messages".into(),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(response.status().as_u16(), 529);
    let error_id = response.headers()["x-error-id"]
        .to_str()
        .unwrap()
        .to_string();
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["error"]["type"], "overloaded_error");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains(&error_id)
    );
}

#[tokio::test]
async fn passthrough_disabled_features_do_not_parse_json() {
    let raw = Bytes::from_static(b"{ deliberately invalid JSON");
    let result = patched(&raw, RouteFeatureSet::default()).await;
    assert_eq!(result.as_ptr(), raw.as_ptr());
}

#[tokio::test]
async fn passthrough_invalid_json_error_has_public_id() {
    let response = request_entry::handle_messages_endpoint(
        state(),
        HeaderMap::new(),
        Bytes::from_static(b"{ invalid"),
        "/v1/messages".into(),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let error_id = response.headers()["x-error-id"]
        .to_str()
        .unwrap()
        .to_string();
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["error"]["type"], "invalid_request_error");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains(&error_id)
    );
}

#[tokio::test]
async fn passthrough_count_tokens_image_processing_uses_route_feature() {
    let state = state();
    state.runtime.install_local(|config| {
        config.route_features.path_overrides.insert(
            "/images".into(),
            crate::model::prompt_injection::RouteFeatureOverride {
                image_processing: Some(Some(ImageProcessingConfig::default())),
                ..Default::default()
            },
        );
    });
    for (endpoint, expected_status) in [
        ("/v1/messages/count_tokens", StatusCode::OK),
        ("/images/v1/messages/count_tokens", StatusCode::BAD_REQUEST),
    ] {
        let payload = serde_json::from_value(json!({
            "model":"claude-sonnet-4-6",
            "messages":[{"role":"user","content":[{
                "type":"image","source":{"type":"url","url":"http://127.0.0.1/image.png"}
            }]}]
        }))
        .unwrap();
        let response = count_tokens_for_endpoint(state.clone(), payload, endpoint).await;
        assert_eq!(response.status(), expected_status, "{endpoint}");
    }
}

#[tokio::test]
async fn passthrough_reasoning_patch_preserves_remaining_field_order() {
    let raw = Bytes::from_static(br#"{"model":"test","max_tokens":4096,"output_config":{"first":1,"effort":"none","second":2,"third":3},"thinking":{"type":"adaptive","budget_tokens":2048,"second":2,"third":3},"messages":[]}"#);
    let result = patched(
        &raw,
        RouteFeatureSet {
            reasoning_compat: Some(LocalRequestCompatConfig::default()),
            ..Default::default()
        },
    )
    .await;
    let value: Value = serde_json::from_slice(&result).unwrap();
    assert_eq!(
        value["output_config"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["first", "second", "third"]
    );
    assert_eq!(
        value["thinking"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["type", "second", "third"]
    );
}

#[tokio::test]
async fn passthrough_account_mapping_changes_only_model_bytes() {
    use crate::external_pool::ExternalPoolModelMappingMode;
    use crate::model::config::ModelMappingRule;
    let raw = Bytes::from_static(br#" { "messages" : [{"role":"user","content":"old"}], "model" : "old", "metadata":{"model":"old"}, "unknown": 1 } "#);
    let mut route = crate::external_pool::tests::test_route("old");
    route.effective_raw_body = raw.clone();
    route.effective_raw_probe = Some(Arc::new(probe_raw_messages_body(&raw)));
    let mut pool = crate::external_pool::tests::test_pool("https://example.invalid", false);
    pool.model_mapping_mode = ExternalPoolModelMappingMode::PassthroughMapping;
    pool.model_mapping_rules = vec![ModelMappingRule {
        enabled: true,
        source: "old".into(),
        target: "new".into(),
        kind: Default::default(),
        note: None,
    }];
    let (body, _) = passthrough_test_prepare(&route, &pool, &HeaderMap::new());
    let expected =
        String::from_utf8_lossy(&raw).replace("\"model\" : \"old\"", "\"model\" : \"new\"");
    assert_eq!(body.as_ref(), expected.as_bytes());
}
