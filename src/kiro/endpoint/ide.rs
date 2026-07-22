//! Kiro IDE 端点
//!
//! 对应 Kiro IDE 客户端目前使用的 AWS CodeWhisperer 端点：
//! - API: `https://q.{api_region}.amazonaws.com/generateAssistantResponse`
//! - MCP: `https://q.{api_region}.amazonaws.com/mcp`
//!
//! 请求头使用 aws-sdk-js User-Agent 标识。请求体会在根对象上注入 `profileArn`。

use reqwest::RequestBuilder;
use uuid::Uuid;

use super::{KiroEndpoint, RequestContext, serialize_json_with_capacity};
use crate::kiro::protocol::{
    is_external_idp_credentials, resolve_agent_mode, resolve_profile_arn,
    resolve_streaming_profile_arn,
};

/// Kiro IDE 端点名称
pub const IDE_ENDPOINT_NAME: &str = "ide";

/// Kiro IDE 端点
pub struct IdeEndpoint;

impl IdeEndpoint {
    pub fn new() -> Self {
        Self
    }

    fn api_region<'a>(&self, ctx: &'a RequestContext<'_>) -> &'a str {
        ctx.credentials.effective_api_region(ctx.config)
    }

    fn host(&self, ctx: &RequestContext<'_>) -> String {
        format!("q.{}.amazonaws.com", self.api_region(ctx))
    }

    fn base_url(&self, ctx: &RequestContext<'_>) -> String {
        ctx.config
            .kiro_upstream_base_url
            .as_deref()
            .map(str::trim)
            .filter(|base| !base.is_empty())
            .map(|base| base.trim_end_matches('/').to_string())
            .unwrap_or_else(|| format!("https://{}", self.host(ctx)))
    }

    fn x_amz_user_agent(&self, ctx: &RequestContext<'_>) -> String {
        format!(
            "aws-sdk-js/1.0.34 KiroIDE-{}-{}",
            ctx.config.kiro_version, ctx.machine_id
        )
    }

    fn user_agent(&self, ctx: &RequestContext<'_>) -> String {
        format!(
            "aws-sdk-js/1.0.34 ua/2.1 os/{} lang/js md/nodejs#{} api/codewhispererstreaming#1.0.34 m/E KiroIDE-{}-{}",
            ctx.config.system_version,
            ctx.config.node_version,
            ctx.config.kiro_version,
            ctx.machine_id
        )
    }
}

impl Default for IdeEndpoint {
    fn default() -> Self {
        Self::new()
    }
}

impl KiroEndpoint for IdeEndpoint {
    fn name(&self) -> &'static str {
        IDE_ENDPOINT_NAME
    }

    fn api_url(&self, ctx: &RequestContext<'_>) -> String {
        format!("{}/generateAssistantResponse", self.base_url(ctx))
    }

    fn mcp_url(&self, ctx: &RequestContext<'_>) -> String {
        format!("{}/mcp", self.base_url(ctx))
    }

    fn models_url(&self, ctx: &RequestContext<'_>, next_token: Option<&str>) -> String {
        let mut params = vec!["origin=AI_EDITOR".to_string(), "maxResults=50".to_string()];
        if let Some(profile_arn) = resolve_profile_arn(ctx.credentials, ctx.config) {
            params.push(format!("profileArn={}", urlencoding::encode(&profile_arn)));
        }
        if let Some(next_token) = next_token {
            params.push(format!("nextToken={}", urlencoding::encode(next_token)));
        }
        format!(
            "{}/ListAvailableModels?{}",
            self.base_url(ctx),
            params.join("&")
        )
    }

    fn decorate_api(&self, req: RequestBuilder, ctx: &RequestContext<'_>) -> RequestBuilder {
        let mut req = req
            .header("x-amzn-codewhisperer-optout", "true")
            .header(
                "x-amzn-kiro-agent-mode",
                resolve_agent_mode(ctx.credentials, ctx.config),
            )
            .header("x-amz-user-agent", self.x_amz_user_agent(ctx))
            .header("user-agent", self.user_agent(ctx))
            .header("host", self.host(ctx))
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
            .header("host", self.host(ctx))
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
            .header("accept", "application/json")
            .header("x-amzn-codewhisperer-optout", "true")
            .header("x-amz-user-agent", self.x_amz_user_agent(ctx))
            .header("user-agent", self.user_agent(ctx))
            .header("host", self.host(ctx))
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
        transform_ide_api_body(
            body,
            &resolve_streaming_profile_arn(ctx.credentials, ctx.config),
        )
    }
}

/// 将 profile_arn 注入到请求体 JSON 根对象
#[cfg(test)]
fn inject_profile_arn(request_body: &str, profile_arn: &Option<String>) -> String {
    let Ok(mut json) = serde_json::from_str::<serde_json::Value>(request_body) else {
        return request_body.to_string();
    };
    if !set_profile_arn(&mut json, profile_arn) {
        return request_body.to_string();
    }
    serde_json::to_string(&json).unwrap_or_else(|_| request_body.to_string())
}

fn transform_ide_api_body(body: &str, profile_arn: &Option<String>) -> String {
    if profile_arn.is_none() {
        return body.to_string();
    }
    let Ok(mut json) = serde_json::from_str::<serde_json::Value>(body) else {
        return body.to_string();
    };
    let profile_changed = set_profile_arn(&mut json, profile_arn);
    if !profile_changed {
        return body.to_string();
    }
    let profile_allowance = profile_arn
        .as_deref()
        .map_or(0, |arn| arn.len().saturating_mul(6).saturating_add(32));
    let minimum_capacity = body
        .len()
        .saturating_add(profile_allowance)
        .saturating_add(32);
    serialize_json_with_capacity(&json, minimum_capacity).unwrap_or_else(|| body.to_string())
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

#[cfg(test)]
mod tests {
    use super::{IdeEndpoint, inject_profile_arn, transform_ide_api_body};
    use crate::http_client::allocation_probe;
    use crate::kiro::endpoint::{KiroEndpoint, RequestContext};
    use crate::kiro::model::credentials::KiroCredentials;
    use crate::kiro::protocol::{KIRO_BUILDER_ID_PLACEHOLDER_ARN, KIRO_SOCIAL_PROFILE_ARN};
    use crate::model::config::{Config, KiroAgentModeStrategy};
    use reqwest::Client;
    use serde_json::Value;
    use std::time::Instant;

    fn ide_perf_body(target_size: usize, mode: &str) -> String {
        let suffix = match mode {
            "no-marker" => r#"","unknown":true}"#,
            "escaped-no-marker" => r#"\n","unknown":true}"#,
            "mutation" => {
                r#"","additionalModelRequestFields":{"output\u005fconfig":{"effort":"xhigh"}}}"#
            }
            _ => panic!("unknown IDE perf mode {mode}"),
        };
        let prefix = r#"{"payload":""#;
        let payload_len = target_size.saturating_sub(prefix.len() + suffix.len());
        let mut body = String::with_capacity(prefix.len() + payload_len + suffix.len());
        body.push_str(prefix);
        body.extend(std::iter::repeat_n('x', payload_len));
        body.push_str(suffix);
        body
    }

    fn ide_percentile(sorted: &[u128], percentile: usize) -> u128 {
        let index = ((sorted.len() * percentile).saturating_sub(1) / 100).min(sorted.len() - 1);
        sorted[index]
    }

    #[test]
    fn test_inject_profile_arn_with_some() {
        let body = r#"{"conversationState":{"conversationId":"c1"}}"#;
        let arn = Some("arn:aws:codewhisperer:us-east-1:123:profile/ABC".to_string());
        let result = inject_profile_arn(body, &arn);
        let json: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(
            json["profileArn"],
            "arn:aws:codewhisperer:us-east-1:123:profile/ABC"
        );
        assert_eq!(json["conversationState"]["conversationId"], "c1");
    }

    #[test]
    fn test_inject_profile_arn_with_none() {
        let body = r#"{"conversationState":{"conversationId":"c1"}}"#;
        let result = inject_profile_arn(body, &None);
        let json: Value = serde_json::from_str(&result).unwrap();
        assert!(json.get("profileArn").is_none());
        assert_eq!(json["conversationState"]["conversationId"], "c1");
    }

    #[test]
    fn test_inject_profile_arn_overwrites_existing() {
        let body = r#"{"conversationState":{},"profileArn":"old-arn"}"#;
        let arn = Some("new-arn".to_string());
        let result = inject_profile_arn(body, &arn);
        let json: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(json["profileArn"], "new-arn");
    }

    #[test]
    fn test_inject_profile_arn_invalid_json() {
        let body = "not-valid-json";
        let arn = Some("arn:test".to_string());
        let result = inject_profile_arn(body, &arn);
        assert_eq!(result, "not-valid-json");
    }

    #[test]
    fn test_ide_does_not_invent_thinking_for_output_config_effort() {
        let body = r#"{"conversationState":{},"additionalModelRequestFields":{"output_config":{"effort":"xhigh"}}}"#;
        let result = transform_ide_api_body(body, &None);
        let json: Value = serde_json::from_str(&result).unwrap();
        assert!(
            json["additionalModelRequestFields"]
                .get("thinking")
                .is_none()
        );
        assert_eq!(
            json["additionalModelRequestFields"]["output_config"]["effort"],
            "xhigh"
        );
    }

    #[test]
    fn test_ide_preserves_existing_schema_owned_thinking_field() {
        let body = r#"{"additionalModelRequestFields":{"thinking":{"type":"disabled"},"output_config":{"effort":"low"}}}"#;
        let result = transform_ide_api_body(body, &None);
        let json: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(
            json["additionalModelRequestFields"]["thinking"]["type"],
            "disabled"
        );
        assert_eq!(
            json["additionalModelRequestFields"]["output_config"]["effort"],
            "low"
        );
    }

    #[test]
    fn test_ide_transform_adds_only_profile_without_other_semantic_changes() {
        let body = r#"{
            "conversationState": {
                "future": {"z": 1.0, "a": 100.0},
                "content": "keep  spaces\n\u00e9"
            },
            "additionalModelRequestFields": {
                "output_config": {"effort":"xhigh"},
                "unknown": [true, null]
            },
            "unknownRoot": {"nested":"value"}
        }"#;
        let mut expected: Value = serde_json::from_str(body).unwrap();
        expected["profileArn"] = serde_json::json!("arn:test:profile/combined");

        for round in 0..5 {
            let transformed =
                transform_ide_api_body(body, &Some("arn:test:profile/combined".to_string()));
            let actual: Value = serde_json::from_str(&transformed).unwrap();
            assert_eq!(actual, expected, "round {round}");
        }
    }

    #[test]
    fn ide_existing_thinking_and_profile_are_byte_identical_for_five_rounds() {
        let profile = "arn:test:profile/already-normalized";
        let body = " {\n  \"additionalModelRequestFields\" : {\n    \"output_config\" : {\"effort\" : \"xhigh\"},\n    \"thinking\" : {\"type\" : \"adaptive\", \"display\" : \"summarized\"}\n  },\n  \"profileArn\" : \"arn:test:profile/already-normalized\",\n  \"unknown\" : {\"z\" : 1.0, \"a\" : 1e+02}\n} \n";

        for round in 0..5 {
            assert_eq!(
                transform_ide_api_body(body, &Some(profile.to_string())),
                body,
                "round {round}: existing thinking/profile must not trigger reserialization"
            );
        }
    }

    #[test]
    fn ide_transform_preserves_escaped_output_config_key_for_five_rounds() {
        let body = r#"{
            "conversationState": {},
            "additionalModelRequestFields": {
                "output\u005fconfig": {"effort":"xhigh"},
                "unknown": [true, null]
            }
        }"#;
        let nested_no_op = r#"{
            "schema": {"properties": {"output\u005fconfig": {"type":"object"}}},
            "content": "the text output_config is not a model field",
            "future": {"z":1.0,"a":1e+02}
        }"#;

        for round in 0..5 {
            let result = transform_ide_api_body(body, &None);
            assert_eq!(
                result, body,
                "round {round}: no profile means byte identity"
            );
            assert_eq!(
                transform_ide_api_body(nested_no_op, &None),
                nested_no_op,
                "round {round}: an escaped target-shaped nested schema key must remain exact when no declared path changes"
            );
        }
    }

    #[test]
    fn ide_transform_deep_or_malformed_input_is_identity_and_recovers_for_five_rounds() {
        let deep = format!(
            "{{ \"payload\" : {}0{} }}",
            "[".repeat(256),
            "]".repeat(256)
        );
        let malformed = r#"{ "output_config" : { "effort" : "xhigh" }, "broken" : ] }"#;
        let profile = Some("arn:test:profile/deep".to_string());
        let recovery = r#"{"additionalModelRequestFields":{"output_config":{"effort":"xhigh"}}}"#;

        for round in 0..5 {
            assert_eq!(
                transform_ide_api_body(&deep, &profile),
                deep,
                "round {round}: recursion-limit input must fail safe without rewriting"
            );
            assert_eq!(
                transform_ide_api_body(malformed, &profile),
                malformed,
                "round {round}: malformed input must remain byte-identical"
            );
            let recovered: Value =
                serde_json::from_str(&transform_ide_api_body(recovery, &None)).unwrap();
            assert!(
                recovered["additionalModelRequestFields"]
                    .get("thinking")
                    .is_none(),
                "round {round}: endpoint must not synthesize model fields"
            );
        }
    }

    #[test]
    #[ignore = "run in release as an isolated endpoint allocation/latency/RSS probe"]
    fn ide_transform_release_perf_probe() {
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
        let body = ide_perf_body(target_size, &mode);
        let mut latencies_us = Vec::with_capacity(rounds);
        let mut allocation_ops = Vec::with_capacity(rounds);
        let mut allocated_bytes = Vec::with_capacity(rounds);
        let mut peak_live_bytes = Vec::with_capacity(rounds);
        let mut end_live_bytes = Vec::with_capacity(rounds);

        for round in 0..rounds {
            let started = Instant::now();
            let (output, stats) = allocation_probe::measure(|| {
                transform_ide_api_body(std::hint::black_box(&body), &None)
            });
            latencies_us.push(started.elapsed().as_micros());
            assert_eq!(output, body, "round {round}: no-op path changed bytes");
            allocation_ops.push(stats.allocation_ops);
            allocated_bytes.push(stats.allocated_bytes);
            peak_live_bytes.push(stats.peak_live_bytes);
            end_live_bytes.push(stats.end_live_bytes);
        }
        latencies_us.sort_unstable();
        println!(
            "IDE_ENDPOINT_BODY_PERF mode={} input_bytes={} rounds={} latency_us_p50={} latency_us_p95={} latency_us_p99={} allocation_ops={:?} allocated_bytes={:?} peak_live_bytes={:?} end_live_bytes={:?}",
            mode,
            body.len(),
            rounds,
            ide_percentile(&latencies_us, 50),
            ide_percentile(&latencies_us, 95),
            ide_percentile(&latencies_us, 99),
            allocation_ops,
            allocated_bytes,
            peak_live_bytes,
            end_live_bytes,
        );
    }

    #[test]
    fn test_models_url_skips_builder_id_placeholder_for_idc_credentials() {
        let endpoint = IdeEndpoint::new();
        let credentials = KiroCredentials {
            auth_method: Some("builder-id".to_string()),
            client_id: Some("client".to_string()),
            client_secret: Some("secret".to_string()),
            ..Default::default()
        };
        let config = Config::default();
        let ctx = RequestContext {
            credentials: &credentials,
            token: "token",
            machine_id: "machine",
            config: &config,
        };

        let url = endpoint.models_url(&ctx, Some("next-token"));
        assert!(!url.contains("profileArn="));
        assert!(url.contains("nextToken=next-token"));
    }

    #[test]
    fn test_models_url_uses_social_profile_for_social_credentials() {
        let endpoint = IdeEndpoint::new();
        let credentials = KiroCredentials {
            auth_method: Some("social".to_string()),
            provider: Some("Github".to_string()),
            ..Default::default()
        };
        let config = Config::default();
        let ctx = RequestContext {
            credentials: &credentials,
            token: "token",
            machine_id: "machine",
            config: &config,
        };

        let url = endpoint.models_url(&ctx, None);
        assert!(url.contains(&urlencoding::encode(KIRO_SOCIAL_PROFILE_ARN).to_string()));
    }

    #[test]
    fn test_models_url_skips_enterprise_fallback_for_external_idp_credentials() {
        let endpoint = IdeEndpoint::new();
        let credentials = KiroCredentials {
            auth_method: Some("external_idp".to_string()),
            provider: Some("Enterprise".to_string()),
            api_region: Some("eu-west-1".to_string()),
            ..Default::default()
        };
        let config = Config::default();
        let ctx = RequestContext {
            credentials: &credentials,
            token: "token",
            machine_id: "machine",
            config: &config,
        };

        let url = endpoint.models_url(&ctx, None);
        assert!(!url.contains("profileArn="));
    }

    #[test]
    fn test_streaming_body_keeps_builder_id_placeholder() {
        let endpoint = IdeEndpoint::new();
        let credentials = KiroCredentials {
            auth_method: Some("builder-id".to_string()),
            client_id: Some("client".to_string()),
            client_secret: Some("secret".to_string()),
            ..Default::default()
        };
        let config = Config::default();
        let ctx = RequestContext {
            credentials: &credentials,
            token: "token",
            machine_id: "machine",
            config: &config,
        };

        let body = endpoint.transform_api_body(r#"{"conversationState":{}}"#, &ctx);
        let json: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["profileArn"], KIRO_BUILDER_ID_PLACEHOLDER_ARN);
    }

    #[test]
    fn test_streaming_body_uses_enterprise_fallback_without_model_header_leak() {
        let endpoint = IdeEndpoint::new();
        let credentials = KiroCredentials {
            auth_method: Some("external_idp".to_string()),
            provider: Some("Enterprise".to_string()),
            api_region: Some("eu-west-1".to_string()),
            ..Default::default()
        };
        let config = Config::default();
        let ctx = RequestContext {
            credentials: &credentials,
            token: "token",
            machine_id: "machine",
            config: &config,
        };

        let body = endpoint.transform_api_body(r#"{"conversationState":{}}"#, &ctx);
        let json: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            json["profileArn"],
            "arn:aws:codewhisperer:eu-central-1:610548660232:profile/VNECVYCYYAWN"
        );

        let models_req = endpoint
            .decorate_models(Client::new().get("https://example.com"), &ctx)
            .build()
            .unwrap();
        assert!(
            models_req
                .headers()
                .get("x-amzn-kiro-profile-arn")
                .is_none()
        );
    }

    #[test]
    fn test_decorate_api_applies_agent_mode_and_token_type_headers() {
        let endpoint = IdeEndpoint::new();
        let credentials = KiroCredentials {
            auth_method: Some("External IDP".to_string()),
            provider: Some("Enterprise".to_string()),
            api_region: Some("us-east-1".to_string()),
            ..Default::default()
        };
        let mut config = Config::default();
        config.kiro_agent_mode_strategy = KiroAgentModeStrategy::Auto;
        let ctx = RequestContext {
            credentials: &credentials,
            token: "token",
            machine_id: "machine",
            config: &config,
        };

        let req = endpoint
            .decorate_api(Client::new().post("https://example.com"), &ctx)
            .build()
            .unwrap();
        let headers = req.headers();

        assert_eq!(
            headers
                .get("x-amzn-kiro-agent-mode")
                .and_then(|v| v.to_str().ok()),
            Some("vibe")
        );
        assert_eq!(
            headers.get("TokenType").and_then(|v| v.to_str().ok()),
            Some("EXTERNAL_IDP")
        );
        let expected_x_amz_user_agent =
            format!("aws-sdk-js/1.0.34 KiroIDE-{}-machine", config.kiro_version);
        assert_eq!(
            headers
                .get("x-amz-user-agent")
                .and_then(|v| v.to_str().ok()),
            Some(expected_x_amz_user_agent.as_str())
        );
        let expected_user_agent = format!(
            "aws-sdk-js/1.0.34 ua/2.1 os/{} lang/js md/nodejs#{} api/codewhispererstreaming#1.0.34 m/E KiroIDE-{}-machine",
            config.system_version, config.node_version, config.kiro_version
        );
        assert_eq!(
            headers.get("user-agent").and_then(|v| v.to_str().ok()),
            Some(expected_user_agent.as_str())
        );
    }

    #[test]
    fn test_decorate_models_and_mcp_do_not_attach_profile_arn_for_api_key() {
        let endpoint = IdeEndpoint::new();
        let credentials = KiroCredentials {
            auth_method: Some("api key".to_string()),
            kiro_api_key: Some("ksk_test".to_string()),
            provider: Some("Enterprise".to_string()),
            profile_arn: Some("arn:aws:codewhisperer:us-east-1:123:profile/STALE".to_string()),
            ..Default::default()
        };
        let config = Config::default();
        let ctx = RequestContext {
            credentials: &credentials,
            token: "token",
            machine_id: "machine",
            config: &config,
        };

        let models_req = endpoint
            .decorate_models(Client::new().get("https://example.com"), &ctx)
            .build()
            .unwrap();
        assert!(
            models_req
                .headers()
                .get("x-amzn-kiro-profile-arn")
                .is_none()
        );
        assert_eq!(
            models_req
                .headers()
                .get("tokentype")
                .and_then(|v| v.to_str().ok()),
            Some("API_KEY")
        );

        let mcp_req = endpoint
            .decorate_mcp(Client::new().post("https://example.com"), &ctx)
            .build()
            .unwrap();
        assert!(mcp_req.headers().get("x-amzn-kiro-profile-arn").is_none());
        assert_eq!(
            mcp_req
                .headers()
                .get("tokentype")
                .and_then(|v| v.to_str().ok()),
            Some("API_KEY")
        );

        let body = endpoint.transform_api_body(r#"{"conversationState":{}}"#, &ctx);
        let json: Value = serde_json::from_str(&body).unwrap();
        assert!(json.get("profileArn").is_none());
    }

    #[test]
    fn test_api_key_noop_transform_is_byte_identical_for_five_rounds() {
        let endpoint = IdeEndpoint::new();
        let credentials = KiroCredentials {
            auth_method: Some("api key".to_string()),
            kiro_api_key: Some("ksk_test".to_string()),
            ..Default::default()
        };
        let config = Config::default();
        let ctx = RequestContext {
            credentials: &credentials,
            token: "token",
            machine_id: "machine",
            config: &config,
        };
        let body = " {\n  \"conversationState\": { \"futureField\" : { \"z\" : 1.0, \"a\" : 1e+02 } },\n  \"unknownRoot\": \"keep  spaces\\n\\u00e9\"\n} \n";

        for round in 0..5 {
            assert_eq!(
                endpoint.transform_api_body(body, &ctx),
                body,
                "round {round}: an IDE no-op transform must preserve exact bytes"
            );
        }
    }

    #[test]
    fn test_kiro_upstream_base_url_override_only_changes_target_url() {
        let endpoint = IdeEndpoint::new();
        let credentials = KiroCredentials::default();
        let mut config = Config::default();
        config.kiro_upstream_base_url = Some("http://127.0.0.1:39090/mock/".to_string());
        let ctx = RequestContext {
            credentials: &credentials,
            token: "token",
            machine_id: "machine",
            config: &config,
        };

        assert_eq!(
            endpoint.api_url(&ctx),
            "http://127.0.0.1:39090/mock/generateAssistantResponse"
        );
        assert_eq!(endpoint.mcp_url(&ctx), "http://127.0.0.1:39090/mock/mcp");
        assert!(
            endpoint
                .models_url(&ctx, None)
                .starts_with("http://127.0.0.1:39090/mock/ListAvailableModels?")
        );

        let req = endpoint
            .decorate_api(Client::new().post(endpoint.api_url(&ctx)), &ctx)
            .build()
            .unwrap();
        assert_eq!(
            req.headers()
                .get("host")
                .and_then(|value| value.to_str().ok()),
            Some("q.us-east-1.amazonaws.com")
        );
    }
}
