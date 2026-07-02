//! Kiro IDE 端点
//!
//! 对应 Kiro IDE 客户端目前使用的 AWS CodeWhisperer 端点：
//! - API: `https://q.{api_region}.amazonaws.com/generateAssistantResponse`
//! - MCP: `https://q.{api_region}.amazonaws.com/mcp`
//!
//! 请求头使用 aws-sdk-js User-Agent 标识。请求体会在根对象上注入 `profileArn`。

use reqwest::RequestBuilder;
use uuid::Uuid;

use super::{KiroEndpoint, RequestContext};
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
        let body = inject_ide_thinking_fields(body);
        inject_profile_arn(
            &body,
            &resolve_streaming_profile_arn(ctx.credentials, ctx.config),
        )
    }
}

/// 将 profile_arn 注入到请求体 JSON 根对象
fn inject_profile_arn(request_body: &str, profile_arn: &Option<String>) -> String {
    if let Some(arn) = profile_arn {
        if let Ok(mut json) = serde_json::from_str::<serde_json::Value>(request_body) {
            json["profileArn"] = serde_json::Value::String(arn.clone());
            if let Ok(body) = serde_json::to_string(&json) {
                return body;
            }
        }
    }
    request_body.to_string()
}

fn inject_ide_thinking_fields(request_body: &str) -> String {
    let Ok(mut json) = serde_json::from_str::<serde_json::Value>(request_body) else {
        return request_body.to_string();
    };

    let Some(fields) = json
        .get_mut("additionalModelRequestFields")
        .and_then(|v| v.as_object_mut())
    else {
        return request_body.to_string();
    };

    if !fields.contains_key("output_config") || fields.contains_key("thinking") {
        return request_body.to_string();
    }

    fields.insert(
        "thinking".to_string(),
        serde_json::json!({
            "type": "adaptive",
            "display": "summarized"
        }),
    );
    serde_json::to_string(&json).unwrap_or_else(|_| request_body.to_string())
}

#[cfg(test)]
mod tests {
    use super::{IdeEndpoint, inject_ide_thinking_fields, inject_profile_arn};
    use crate::kiro::endpoint::{KiroEndpoint, RequestContext};
    use crate::kiro::model::credentials::KiroCredentials;
    use crate::kiro::protocol::{KIRO_BUILDER_ID_PLACEHOLDER_ARN, KIRO_SOCIAL_PROFILE_ARN};
    use crate::model::config::{Config, KiroAgentModeStrategy};
    use reqwest::Client;
    use serde_json::Value;

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
    fn test_ide_injects_thinking_for_output_config_effort() {
        let body = r#"{"conversationState":{},"additionalModelRequestFields":{"output_config":{"effort":"xhigh"}}}"#;
        let result = inject_ide_thinking_fields(body);
        let json: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(
            json["additionalModelRequestFields"]["thinking"]["type"],
            "adaptive"
        );
        assert_eq!(
            json["additionalModelRequestFields"]["thinking"]["display"],
            "summarized"
        );
        assert_eq!(
            json["additionalModelRequestFields"]["output_config"]["effort"],
            "xhigh"
        );
    }

    #[test]
    fn test_ide_preserves_existing_thinking_field() {
        let body = r#"{"additionalModelRequestFields":{"thinking":{"type":"disabled"},"output_config":{"effort":"low"}}}"#;
        let result = inject_ide_thinking_fields(body);
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
