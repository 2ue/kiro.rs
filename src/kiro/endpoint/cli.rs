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

use super::{KiroEndpoint, RequestContext};
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
        format!("https://{}/", self.runtime_host(ctx))
    }

    fn mcp_url(&self, ctx: &RequestContext<'_>) -> String {
        format!("{}/mcp", self.q_base_url(ctx))
    }

    fn models_url(&self, ctx: &RequestContext<'_>, next_token: Option<&str>) -> String {
        let mut params = vec!["origin=KIRO_CLI".to_string()];
        if let Some(profile_arn) = resolve_profile_arn(ctx.credentials, ctx.config) {
            params.push(format!("profileArn={}", urlencoding::encode(&profile_arn)));
        }
        if let Some(next_token) = next_token {
            params.push(format!("nextToken={}", urlencoding::encode(next_token)));
        }
        format!("{}/?{}", self.management_base_url(ctx), params.join("&"))
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
        let body = rewrite_cli_body(body);
        inject_profile_arn(
            &body,
            &resolve_streaming_profile_arn(ctx.credentials, ctx.config),
        )
    }
}

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

fn rewrite_cli_body(body: &str) -> String {
    let Ok(mut json) = serde_json::from_str::<serde_json::Value>(body) else {
        return body.to_string();
    };

    rewrite_origin(&mut json);
    strip_unsupported_cli_model_fields(&mut json);
    serde_json::to_string(&json).unwrap_or_else(|_| body.to_string())
}

fn strip_unsupported_cli_model_fields(json: &mut serde_json::Value) {
    let Some(fields) = json
        .get_mut("additionalModelRequestFields")
        .and_then(|v| v.as_object_mut())
    else {
        return;
    };

    fields.remove("thinking");
    if fields.is_empty() {
        if let Some(root) = json.as_object_mut() {
            root.remove("additionalModelRequestFields");
        }
    }
}

fn set_user_input_for_cli(uim: &mut serde_json::Value) {
    let Some(obj) = uim.as_object_mut() else {
        return;
    };
    if obj.contains_key("origin") {
        obj.insert(
            "origin".to_string(),
            serde_json::Value::String("KIRO_CLI".to_string()),
        );
    }
}

fn rewrite_origin(json: &mut serde_json::Value) {
    let Some(state) = json
        .get_mut("conversationState")
        .and_then(|v| v.as_object_mut())
    else {
        return;
    };
    if let Some(uim) = state
        .get_mut("currentMessage")
        .and_then(|v| v.get_mut("userInputMessage"))
    {
        set_user_input_for_cli(uim);
    }

    if let Some(history) = state.get_mut("history").and_then(|v| v.as_array_mut()) {
        for msg in history {
            if let Some(user_input) = msg.get_mut("userInputMessage") {
                set_user_input_for_cli(user_input);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kiro::model::credentials::KiroCredentials;
    use crate::model::config::Config;

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
    fn cli_rewrite_removes_ide_thinking_wrapper() {
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
