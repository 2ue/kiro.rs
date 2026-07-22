//! Kiro CLI endpoint.
//!
//! This matches the Amazon Q/Kiro CLI runtime protocol:
//! - API: `https://runtime.{api_region}.kiro.dev/`
//! - content type: `application/x-amz-json-1.0`
//! - target header: `AmazonCodeWhispererStreamingService.GenerateAssistantResponse`
//! - request body origin: `KIRO_CLI`

use reqwest::{Method, RequestBuilder};
use serde_json::json;
use uuid::Uuid;

use super::{
    KiroEndpoint, RequestContext, configured_upstream_url, contains_json_object_key,
    serialize_json_with_capacity,
};
use crate::kiro::protocol::{
    is_external_idp_credentials, resolve_profile_arn, resolve_streaming_profile_arn,
};

pub const CLI_ENDPOINT_NAME: &str = "cli";

pub struct CliEndpoint;

impl CliEndpoint {
    pub fn new() -> Self {
        Self
    }

    fn api_region<'a>(&self, ctx: &'a RequestContext<'_>) -> &'a str {
        ctx.credentials.effective_api_region(ctx.config)
    }

    fn runtime_host(&self, ctx: &RequestContext<'_>) -> String {
        format!("runtime.{}.kiro.dev", self.api_region(ctx))
    }

    fn q_host(&self, ctx: &RequestContext<'_>) -> String {
        format!("q.{}.amazonaws.com", self.api_region(ctx))
    }

    fn management_host(&self, ctx: &RequestContext<'_>) -> String {
        format!("management.{}.kiro.dev", self.api_region(ctx))
    }

    fn q_base_url(&self, ctx: &RequestContext<'_>) -> String {
        format!("https://{}", self.q_host(ctx))
    }

    fn management_base_url(&self, ctx: &RequestContext<'_>) -> String {
        format!("https://{}", self.management_host(ctx))
    }

    fn user_agent(&self, ctx: &RequestContext<'_>) -> String {
        format!(
            "aws-sdk-rust/1.3.15 ua/2.1 api/codewhispererstreaming/0.1.16551 os/{} lang/rust/1.92.0 md/appVersion-{} app/AmazonQ-For-CLI",
            ctx.config.system_version, ctx.config.kiro_version,
        )
    }

    fn x_amz_user_agent(&self, ctx: &RequestContext<'_>) -> String {
        format!(
            "aws-sdk-rust/1.3.15 ua/2.1 api/codewhispererstreaming/0.1.16551 os/{} lang/rust/1.92.0 m/F app/AmazonQ-For-CLI",
            ctx.config.system_version,
        )
    }

    fn management_user_agent(&self, ctx: &RequestContext<'_>) -> String {
        format!(
            "aws-sdk-rust/1.3.15 ua/2.1 api/codewhispererruntime/0.1.16551 os/{} lang/rust/1.92.0 md/appVersion-{} app/AmazonQ-For-CLI",
            ctx.config.system_version, ctx.config.kiro_version,
        )
    }

    fn management_x_amz_user_agent(&self, ctx: &RequestContext<'_>) -> String {
        format!(
            "aws-sdk-rust/1.3.15 ua/2.1 api/codewhispererruntime/0.1.16551 os/{} lang/rust/1.92.0 m/F,C app/AmazonQ-For-CLI",
            ctx.config.system_version,
        )
    }
}

impl Default for CliEndpoint {
    fn default() -> Self {
        Self::new()
    }
}

impl KiroEndpoint for CliEndpoint {
    fn name(&self) -> &'static str {
        CLI_ENDPOINT_NAME
    }

    fn content_type(&self) -> &'static str {
        "application/x-amz-json-1.0"
    }

    fn api_url(&self, ctx: &RequestContext<'_>) -> String {
        configured_upstream_url(ctx.config, "")
            .unwrap_or_else(|| format!("https://{}/", self.runtime_host(ctx)))
    }

    fn mcp_url(&self, ctx: &RequestContext<'_>) -> String {
        configured_upstream_url(ctx.config, "mcp")
            .unwrap_or_else(|| format!("{}/mcp", self.q_base_url(ctx)))
    }

    fn models_url(&self, ctx: &RequestContext<'_>, next_token: Option<&str>) -> String {
        let mut params = vec!["origin=KIRO_CLI".to_string()];
        if let Some(profile_arn) = resolve_profile_arn(ctx.credentials, ctx.config) {
            params.push(format!("profileArn={}", urlencoding::encode(&profile_arn)));
        }
        if let Some(next_token) = next_token {
            params.push(format!("nextToken={}", urlencoding::encode(next_token)));
        }
        let base = configured_upstream_url(ctx.config, "")
            .unwrap_or_else(|| format!("{}/", self.management_base_url(ctx)));
        format!("{}?{}", base, params.join("&"))
    }

    fn models_method(&self, _ctx: &RequestContext<'_>) -> Method {
        Method::POST
    }

    fn models_body(
        &self,
        ctx: &RequestContext<'_>,
        next_token: Option<&str>,
    ) -> Option<serde_json::Value> {
        let mut body = serde_json::Map::new();
        body.insert("origin".to_string(), json!("KIRO_CLI"));
        if let Some(profile_arn) = resolve_profile_arn(ctx.credentials, ctx.config) {
            body.insert("profileArn".to_string(), json!(profile_arn));
        }
        if let Some(next_token) = next_token {
            body.insert("nextToken".to_string(), json!(next_token));
        }
        Some(serde_json::Value::Object(body))
    }

    fn decorate_api(&self, req: RequestBuilder, ctx: &RequestContext<'_>) -> RequestBuilder {
        let mut req = req
            .header(
                "x-amz-target",
                "AmazonCodeWhispererStreamingService.GenerateAssistantResponse",
            )
            .header("x-amzn-codewhisperer-optout", "false")
            .header("x-amz-user-agent", self.x_amz_user_agent(ctx))
            .header("user-agent", self.user_agent(ctx))
            .header("host", self.runtime_host(ctx))
            .header("amz-sdk-invocation-id", Uuid::new_v4().to_string())
            .header("amz-sdk-request", "attempt=1; max=3")
            .header("Authorization", format!("Bearer {}", ctx.token));

        if ctx.credentials.is_api_key_credential() {
            req = req.header("tokentype", "API_KEY");
        }
        if is_external_idp_credentials(ctx.credentials) {
            req = req.header("TokenType", "EXTERNAL_IDP");
        }
        req
    }

    fn decorate_mcp(&self, req: RequestBuilder, ctx: &RequestContext<'_>) -> RequestBuilder {
        let mut req = req
            .header("x-amz-user-agent", self.x_amz_user_agent(ctx))
            .header("user-agent", self.user_agent(ctx))
            .header("host", self.q_host(ctx))
            .header("amz-sdk-invocation-id", Uuid::new_v4().to_string())
            .header("amz-sdk-request", "attempt=1; max=3")
            .header("Authorization", format!("Bearer {}", ctx.token));

        if let Some(arn) = resolve_profile_arn(ctx.credentials, ctx.config) {
            req = req.header("x-amzn-kiro-profile-arn", arn);
        }
        if ctx.credentials.is_api_key_credential() {
            req = req.header("tokentype", "API_KEY");
        }
        if is_external_idp_credentials(ctx.credentials) {
            req = req.header("TokenType", "EXTERNAL_IDP");
        }
        req
    }

    fn decorate_models(&self, req: RequestBuilder, ctx: &RequestContext<'_>) -> RequestBuilder {
        let mut req = req
            .header("accept", "*/*")
            .header("content-type", "application/x-amz-json-1.0")
            .header(
                "x-amz-target",
                "AmazonCodeWhispererService.ListAvailableModels",
            )
            .header("x-amzn-codewhisperer-optout", "false")
            .header("x-amz-user-agent", self.management_x_amz_user_agent(ctx))
            .header("user-agent", self.management_user_agent(ctx))
            .header("host", self.management_host(ctx))
            .header("amz-sdk-invocation-id", Uuid::new_v4().to_string())
            .header("amz-sdk-request", "attempt=1; max=3")
            .header("Authorization", format!("Bearer {}", ctx.token));

        if let Some(arn) = resolve_profile_arn(ctx.credentials, ctx.config) {
            req = req.header("x-amzn-kiro-profile-arn", arn);
        }
        if ctx.credentials.is_api_key_credential() {
            req = req.header("tokentype", "API_KEY");
        }
        if is_external_idp_credentials(ctx.credentials) {
            req = req.header("TokenType", "EXTERNAL_IDP");
        }
        req
    }

    fn transform_api_body(&self, body: &str, ctx: &RequestContext<'_>) -> String {
        transform_cli_api_body(
            body,
            &resolve_streaming_profile_arn(ctx.credentials, ctx.config),
        )
    }
}

#[cfg(test)]
fn rewrite_cli_body(body: &str) -> String {
    transform_cli_api_body(body, &None)
}

fn transform_cli_api_body(body: &str, profile_arn: &Option<String>) -> String {
    let has_escaped_target_key =
        body.as_bytes().contains(&b'\\') && contains_json_object_key(body, &["origin"]);
    if profile_arn.is_none() && !body.contains("\"origin\"") && !has_escaped_target_key {
        return body.to_string();
    }

    let Ok(mut json) = serde_json::from_str::<serde_json::Value>(body) else {
        return body.to_string();
    };

    let origin_changed = rewrite_origin(&mut json);
    let profile_changed = set_profile_arn(&mut json, profile_arn);
    if !origin_changed && !profile_changed {
        return body.to_string();
    }

    let profile_allowance = profile_arn
        .as_deref()
        .map_or(0, |arn| arn.len().saturating_mul(6).saturating_add(32));
    serialize_json_with_capacity(&json, body.len().saturating_add(profile_allowance))
        .unwrap_or_else(|| body.to_string())
}

fn set_profile_arn(json: &mut serde_json::Value, profile_arn: &Option<String>) -> bool {
    let Some(profile_arn) = profile_arn else {
        return false;
    };
    let Some(root) = json.as_object_mut() else {
        return false;
    };
    if root
        .get("profileArn")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|current| current == profile_arn)
    {
        return false;
    }
    root.insert(
        "profileArn".to_string(),
        serde_json::Value::String(profile_arn.clone()),
    );
    true
}

fn set_user_input_for_cli(uim: &mut serde_json::Value) -> bool {
    let Some(obj) = uim.as_object_mut() else {
        return false;
    };
    match obj.get("origin") {
        None => false,
        Some(serde_json::Value::String(origin)) if origin == "KIRO_CLI" => false,
        Some(_) => {
            obj.insert(
                "origin".to_string(),
                serde_json::Value::String("KIRO_CLI".to_string()),
            );
            true
        }
    }
}

fn rewrite_origin(json: &mut serde_json::Value) -> bool {
    let Some(state) = json
        .get_mut("conversationState")
        .and_then(|v| v.as_object_mut())
    else {
        return false;
    };
    let mut changed = false;
    if let Some(uim) = state
        .get_mut("currentMessage")
        .and_then(|v| v.get_mut("userInputMessage"))
    {
        changed |= set_user_input_for_cli(uim);
    }

    if let Some(history) = state.get_mut("history").and_then(|v| v.as_array_mut()) {
        for msg in history {
            if let Some(user_input) = msg.get_mut("userInputMessage") {
                changed |= set_user_input_for_cli(user_input);
            }
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http_client::allocation_probe;
    use crate::kiro::model::credentials::KiroCredentials;
    use crate::model::config::Config;
    use std::time::Instant;

    fn cli_perf_body(target_size: usize, mode: &str) -> String {
        let suffix = match mode {
            "no-marker" => r#"","unknown":true}"#,
            "escaped-no-marker" => r#"\n","unknown":true}"#,
            "mutation" => {
                r#"","conversationState":{"currentMessage":{"userInputMessage":{"orig\u0069n":"AI_EDITOR","modelId":"claude-sonnet-4"}}}}"#
            }
            _ => panic!("unknown CLI perf mode {mode}"),
        };
        let prefix = r#"{"payload":""#;
        let payload_len = target_size.saturating_sub(prefix.len() + suffix.len());
        let mut body = String::with_capacity(prefix.len() + payload_len + suffix.len());
        body.push_str(prefix);
        body.extend(std::iter::repeat_n('x', payload_len));
        body.push_str(suffix);
        body
    }

    fn cli_percentile(sorted: &[u128], percentile: usize) -> u128 {
        let index = ((sorted.len() * percentile).saturating_sub(1) / 100).min(sorted.len() - 1);
        sorted[index]
    }

    fn ctx<'a>(
        credentials: &'a KiroCredentials,
        config: &'a Config,
        token: &'a str,
    ) -> RequestContext<'a> {
        RequestContext {
            credentials,
            token,
            machine_id: "machine",
            config,
        }
    }

    #[test]
    fn cli_api_url_uses_runtime_kiro_dev() {
        let endpoint = CliEndpoint::new();
        let config = Config::default();
        let credentials = KiroCredentials::default();

        assert_eq!(
            endpoint.api_url(&ctx(&credentials, &config, "token")),
            "https://runtime.us-east-1.kiro.dev/"
        );
        assert_eq!(endpoint.content_type(), "application/x-amz-json-1.0");
    }

    #[test]
    fn cli_upstream_override_changes_transport_but_preserves_region_headers() {
        let endpoint = CliEndpoint::new();
        let mut config = Config::default();
        config.kiro_upstream_base_url = Some(" http://127.0.0.1:39091/aws-lifecycle/ ".to_string());
        let credentials = KiroCredentials {
            auth_method: Some("api_key".to_string()),
            kiro_api_key: Some("ksk_fake_lifecycle".to_string()),
            api_region: Some("eu-west-3".to_string()),
            ..Default::default()
        };
        let rctx = ctx(&credentials, &config, "ksk_fake_lifecycle");

        assert_eq!(
            endpoint.api_url(&rctx),
            "http://127.0.0.1:39091/aws-lifecycle/"
        );
        assert_eq!(
            endpoint.mcp_url(&rctx),
            "http://127.0.0.1:39091/aws-lifecycle/mcp"
        );
        assert_eq!(
            endpoint.models_url(&rctx, None),
            "http://127.0.0.1:39091/aws-lifecycle/?origin=KIRO_CLI"
        );

        let api = endpoint
            .decorate_api(reqwest::Client::new().post(endpoint.api_url(&rctx)), &rctx)
            .build()
            .unwrap();
        assert_eq!(
            api.headers()
                .get("host")
                .and_then(|value| value.to_str().ok()),
            Some("runtime.eu-west-3.kiro.dev")
        );
        assert_eq!(
            api.headers()
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            Some("Bearer ksk_fake_lifecycle")
        );
        assert_eq!(
            api.headers()
                .get("tokentype")
                .and_then(|value| value.to_str().ok()),
            Some("API_KEY")
        );

        let models = endpoint
            .decorate_models(
                reqwest::Client::new().post(endpoint.models_url(&rctx, None)),
                &rctx,
            )
            .build()
            .unwrap();
        assert_eq!(
            models
                .headers()
                .get("host")
                .and_then(|value| value.to_str().ok()),
            Some("management.eu-west-3.kiro.dev")
        );
        assert_eq!(
            models
                .headers()
                .get("tokentype")
                .and_then(|value| value.to_str().ok()),
            Some("API_KEY")
        );
    }

    #[test]
    fn cli_models_request_uses_management_post_protocol() {
        let endpoint = CliEndpoint::new();
        let config = Config::default();
        let credentials = KiroCredentials {
            profile_arn: Some("arn:aws:codewhisperer:us-east-1:123:profile/ABC".to_string()),
            ..Default::default()
        };
        let rctx = ctx(&credentials, &config, "token");

        let url = endpoint.models_url(&rctx, Some("next-token"));
        assert_eq!(
            url,
            "https://management.us-east-1.kiro.dev/?origin=KIRO_CLI&profileArn=arn%3Aaws%3Acodewhisperer%3Aus-east-1%3A123%3Aprofile%2FABC&nextToken=next-token"
        );
        assert_eq!(endpoint.models_method(&rctx), Method::POST);

        let body = endpoint.models_body(&rctx, Some("next-token")).unwrap();
        assert_eq!(body["origin"], "KIRO_CLI");
        assert_eq!(
            body["profileArn"],
            "arn:aws:codewhisperer:us-east-1:123:profile/ABC"
        );
        assert_eq!(body["nextToken"], "next-token");

        let request = endpoint
            .decorate_models(
                reqwest::Client::new()
                    .post("https://example.com")
                    .body(serde_json::to_vec(&body).unwrap()),
                &rctx,
            )
            .build()
            .unwrap();
        let headers = request.headers();
        assert_eq!(headers.get_all("content-type").iter().count(), 1);
        assert_eq!(
            headers.get("x-amz-target").and_then(|v| v.to_str().ok()),
            Some("AmazonCodeWhispererService.ListAvailableModels")
        );
        assert_eq!(
            headers.get("content-type").and_then(|v| v.to_str().ok()),
            Some("application/x-amz-json-1.0")
        );
        assert_eq!(
            headers.get("host").and_then(|v| v.to_str().ok()),
            Some("management.us-east-1.kiro.dev")
        );
        assert!(
            headers
                .get("x-amz-user-agent")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|value| {
                    value.contains("api/codewhispererruntime/")
                        && value.contains("m/F,C app/AmazonQ-For-CLI")
                })
        );
    }

    #[test]
    fn cli_rewrite_changes_only_user_input_origin() {
        let body = r#"{
            "conversationState": {
                "agentContinuationId": "keep-me",
                "currentMessage": {
                    "userInputMessage": {
                        "origin": "AI_EDITOR",
                        "modelId": "claude-opus-4.8",
                        "userInputMessageContext": {
                            "tools": [{
                                "toolSpecification": {
                                    "name": "test",
                                    "description": "test",
                                    "inputSchema": {
                                        "json": {
                                            "type": "object",
                                            "properties": {
                                                "origin": {"type": "string", "description": "AI_EDITOR"},
                                                "modelId": {"type": "string", "default": "claude-opus-4.8"}
                                            }
                                        }
                                    }
                                }
                            }]
                        }
                    }
                },
                "history": [
                    {"userInputMessage":{"origin":"AI_EDITOR","content":"old"}}
                ]
            }
        }"#;

        let result = rewrite_cli_body(body);
        let json: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(json["conversationState"]["agentContinuationId"], "keep-me");
        let uim = &json["conversationState"]["currentMessage"]["userInputMessage"];
        assert_eq!(uim["origin"], "KIRO_CLI");
        assert_eq!(uim["modelId"], "claude-opus-4.8");
        assert_eq!(
            json["conversationState"]["history"][0]["userInputMessage"]["origin"],
            "KIRO_CLI"
        );
        let props = &uim["userInputMessageContext"]["tools"][0]["toolSpecification"]["inputSchema"]
            ["json"]["properties"];
        assert_eq!(props["origin"]["description"], "AI_EDITOR");
        assert_eq!(props["modelId"]["default"], "claude-opus-4.8");
    }

    #[test]
    fn cli_rewrite_noop_is_byte_identical_for_five_rounds() {
        let body = " {\n  \"conversationState\": {\n    \"futureField\": { \"z\" : 1.0, \"a\" : 1e+02 },\n    \"history\": [{ \"assistantResponseMessage\" : { \"content\" : \"keep  spaces\\n\\u00e9\" } }]\n  },\n  \"unknownRoot\": [ true, null ]\n} \n";

        for round in 0..5 {
            assert_eq!(
                rewrite_cli_body(body),
                body,
                "round {round}: a no-op endpoint transform must preserve exact bytes"
            );
        }
    }

    #[test]
    fn cli_already_normalized_origin_and_profile_are_byte_identical_for_five_rounds() {
        let profile = "arn:test:profile/already-normalized";
        let body = " {\n  \"conversationState\" : {\n    \"currentMessage\" : {\"userInputMessage\" : {\"origin\" : \"KIRO_CLI\", \"content\" : \"keep  spaces\\n\\u00e9\"}}\n  },\n  \"profileArn\" : \"arn:test:profile/already-normalized\",\n  \"unknown\" : {\"z\" : 1.0, \"a\" : 1e+02}\n} \n";

        for round in 0..5 {
            assert_eq!(
                transform_cli_api_body(body, &Some(profile.to_string())),
                body,
                "round {round}: already-normalized origin/profile must not trigger reserialization"
            );
        }
    }

    #[test]
    fn cli_transform_handles_escaped_target_keys_for_five_rounds() {
        let body = r#"{
            "conversationState": {
                "currentMessage": {
                    "userInputMessage": {
                        "orig\u0069n": "AI_EDITOR",
                        "modelId": "claude-sonnet-4"
                    }
                }
            },
            "additionalModelRequest\u0046ields": {
                "thinking": {"type":"adaptive"},
                "output_config": {"effort":"xhigh"}
            }
        }"#;
        let nested_no_op = r#"{
            "schema": {"properties": {"orig\u0069n": {"type":"string"}}},
            "content": "the text orig\u0069n is not a request origin",
            "future": {"z":1.0,"a":1e+02}
        }"#;

        for round in 0..5 {
            let result = transform_cli_api_body(body, &None);
            let json: serde_json::Value = serde_json::from_str(&result).unwrap();
            assert_eq!(
                json["conversationState"]["currentMessage"]["userInputMessage"]["origin"],
                "KIRO_CLI",
                "round {round}: escaped semantic origin key must not bypass the transform"
            );
            assert_eq!(
                json["additionalModelRequestFields"]["thinking"]["type"], "adaptive",
                "round {round}: endpoint transform must preserve schema-owned fields"
            );
            assert_eq!(
                json["additionalModelRequestFields"]["output_config"]["effort"], "xhigh",
                "round {round}"
            );
            assert_eq!(
                transform_cli_api_body(nested_no_op, &None),
                nested_no_op,
                "round {round}: an escaped target-shaped nested schema key must remain exact when no declared path changes"
            );
        }
    }

    #[test]
    fn cli_transform_deep_or_malformed_input_is_identity_and_recovers_for_five_rounds() {
        let deep = format!(
            "{{ \"payload\" : {}0{} }}",
            "[".repeat(256),
            "]".repeat(256)
        );
        let malformed = r#"{ "origin" : "AI_EDITOR", "unterminated" : [ 1, 2 }"#;
        let profile = Some("arn:test:profile/deep".to_string());
        let recovery = r#"{"conversationState":{"currentMessage":{"userInputMessage":{"origin":"AI_EDITOR"}}}}"#;

        for round in 0..5 {
            assert_eq!(
                transform_cli_api_body(&deep, &profile),
                deep,
                "round {round}: recursion-limit input must fail safe without rewriting"
            );
            assert_eq!(
                transform_cli_api_body(malformed, &profile),
                malformed,
                "round {round}: malformed input must remain byte-identical"
            );
            let recovered: serde_json::Value =
                serde_json::from_str(&transform_cli_api_body(recovery, &None)).unwrap();
            assert_eq!(
                recovered["conversationState"]["currentMessage"]["userInputMessage"]["origin"],
                "KIRO_CLI",
                "round {round}: a rejected body must not poison the next transform"
            );
        }
    }

    #[test]
    #[ignore = "run in release as an isolated endpoint allocation/latency/RSS probe"]
    fn cli_transform_release_perf_probe() {
        let target_size = std::env::var("KIRO_BODY_PERF_SIZE_BYTES")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(1 << 20);
        let rounds = std::env::var("KIRO_BODY_PERF_ROUNDS")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(5);
        let mode = std::env::var("KIRO_ENDPOINT_BODY_PERF_MODE")
            .unwrap_or_else(|_| "escaped-no-marker".to_string());
        assert!(rounds >= 5);
        let body = cli_perf_body(target_size, &mode);
        let mut latencies_us = Vec::with_capacity(rounds);
        let mut allocation_ops = Vec::with_capacity(rounds);
        let mut allocated_bytes = Vec::with_capacity(rounds);
        let mut peak_live_bytes = Vec::with_capacity(rounds);
        let mut end_live_bytes = Vec::with_capacity(rounds);

        for round in 0..rounds {
            let started = Instant::now();
            let (output, stats) = allocation_probe::measure(|| {
                transform_cli_api_body(std::hint::black_box(&body), &None)
            });
            latencies_us.push(started.elapsed().as_micros());
            if mode == "mutation" {
                let json: serde_json::Value = serde_json::from_str(&output).unwrap();
                assert_eq!(
                    json["conversationState"]["currentMessage"]["userInputMessage"]["origin"],
                    "KIRO_CLI",
                    "round {round}"
                );
            } else {
                assert_eq!(output, body, "round {round}: no-op path changed bytes");
            }
            allocation_ops.push(stats.allocation_ops);
            allocated_bytes.push(stats.allocated_bytes);
            peak_live_bytes.push(stats.peak_live_bytes);
            end_live_bytes.push(stats.end_live_bytes);
        }
        latencies_us.sort_unstable();
        println!(
            "CLI_ENDPOINT_BODY_PERF mode={} input_bytes={} rounds={} latency_us_p50={} latency_us_p95={} latency_us_p99={} allocation_ops={:?} allocated_bytes={:?} peak_live_bytes={:?} end_live_bytes={:?}",
            mode,
            body.len(),
            rounds,
            cli_percentile(&latencies_us, 50),
            cli_percentile(&latencies_us, 95),
            cli_percentile(&latencies_us, 99),
            allocation_ops,
            allocated_bytes,
            peak_live_bytes,
            end_live_bytes,
        );
    }

    #[test]
    fn cli_rewrite_preserves_schema_owned_reasoning_fields() {
        let body = r#"{
            "conversationState": {
                "currentMessage": {
                    "userInputMessage": {
                        "origin": "AI_EDITOR",
                        "modelId": "claude-opus-4.8"
                    }
                }
            },
            "additionalModelRequestFields": {
                "thinking": {"type":"adaptive","display":"summarized"},
                "output_config": {"effort":"xhigh"}
            }
        }"#;

        let result = rewrite_cli_body(body);
        let json: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(
            json["additionalModelRequestFields"]["thinking"]["type"],
            "adaptive"
        );
        assert_eq!(
            json["additionalModelRequestFields"]["output_config"]["effort"],
            "xhigh"
        );
    }

    #[test]
    fn cli_transform_combines_origin_model_fields_and_profile_without_other_semantic_changes() {
        let body = r#"{
            "conversationState": {
                "currentMessage": {
                    "userInputMessage": {
                        "origin": "AI_EDITOR",
                        "content": "keep  spaces\n\u00e9",
                        "future": {"z": 1.0, "a": 100.0}
                    }
                }
            },
            "additionalModelRequestFields": {
                "thinking": {"type":"adaptive"},
                "output_config": {"effort":"xhigh"},
                "unknown": [true, null]
            },
            "unknownRoot": {"nested":"value"}
        }"#;
        let mut expected: serde_json::Value = serde_json::from_str(body).unwrap();
        expected["conversationState"]["currentMessage"]["userInputMessage"]["origin"] =
            serde_json::json!("KIRO_CLI");
        expected["profileArn"] = serde_json::json!("arn:test:profile/combined");

        for round in 0..5 {
            let transformed =
                transform_cli_api_body(body, &Some("arn:test:profile/combined".to_string()));
            let actual: serde_json::Value = serde_json::from_str(&transformed).unwrap();
            assert_eq!(actual, expected, "round {round}");
        }
    }

    #[test]
    fn cli_decorate_api_uses_runtime_headers() {
        let endpoint = CliEndpoint::new();
        let config = Config::default();
        let credentials = KiroCredentials {
            auth_method: Some("api key".to_string()),
            kiro_api_key: Some("ksk_test".to_string()),
            ..Default::default()
        };
        let request = endpoint
            .decorate_api(
                reqwest::Client::new().post("https://example.com"),
                &ctx(&credentials, &config, "token"),
            )
            .build()
            .unwrap();
        let headers = request.headers();

        assert_eq!(
            headers.get("x-amz-target").and_then(|v| v.to_str().ok()),
            Some("AmazonCodeWhispererStreamingService.GenerateAssistantResponse")
        );
        assert_eq!(
            headers.get("tokentype").and_then(|v| v.to_str().ok()),
            Some("API_KEY")
        );
        assert_eq!(
            headers.get("host").and_then(|v| v.to_str().ok()),
            Some("runtime.us-east-1.kiro.dev")
        );
    }
}
