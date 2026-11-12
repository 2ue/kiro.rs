//! Kiro-Go compatible protocol routes used as explicit endpoint candidates.

use reqwest::RequestBuilder;
use uuid::Uuid;

use super::ide::transform_ide_api_body;
use super::{EndpointAuthKind, EndpointOperation, RequestContext, configured_upstream_url};
use crate::kiro::protocol::{
    is_external_idp_credentials, resolve_mcp_profile_arn, resolve_models_profile_arn,
    resolve_streaming_profile_arn,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompatRoute {
    Codewhisperer,
    Amazonq,
    Runtime,
}

pub(crate) struct CompatEndpoint {
    route: CompatRoute,
}

impl CompatEndpoint {
    pub(crate) const fn new(route: CompatRoute) -> Self {
        Self { route }
    }

    fn host(&self, ctx: &RequestContext<'_>) -> String {
        let region = ctx.effective_api_region();
        match self.route {
            CompatRoute::Codewhisperer if region == "us-east-1" => {
                format!("codewhisperer.{region}.amazonaws.com")
            }
            CompatRoute::Codewhisperer | CompatRoute::Amazonq => {
                format!("q.{region}.amazonaws.com")
            }
            CompatRoute::Runtime => format!("runtime.{region}.kiro.dev"),
        }
    }

    /// runtime 域名只承载对话和 MCP；模型列表与 IDE 端点一样固定走 q host。
    fn models_host(&self, ctx: &RequestContext<'_>) -> String {
        match self.route {
            CompatRoute::Runtime => format!("q.{}.amazonaws.com", ctx.effective_api_region()),
            _ => self.host(ctx),
        }
    }

    fn base_url_for_host(&self, ctx: &RequestContext<'_>, host: &str) -> String {
        configured_upstream_url(ctx.config, "")
            .map(|base| base.trim_end_matches('/').to_string())
            .unwrap_or_else(|| format!("https://{host}"))
    }

    fn base_url(&self, ctx: &RequestContext<'_>) -> String {
        self.base_url_for_host(ctx, &self.host(ctx))
    }

    fn target(&self) -> Option<&'static str> {
        match self.route {
            CompatRoute::Codewhisperer => {
                Some("AmazonCodeWhispererStreamingService.GenerateAssistantResponse")
            }
            CompatRoute::Amazonq => Some("AmazonQDeveloperStreamingService.SendMessage"),
            CompatRoute::Runtime => None,
        }
    }

    fn user_agent(&self, ctx: &RequestContext<'_>) -> String {
        let ide_version = ctx.config.effective_kiro_version();
        let streaming_sdk_version =
            crate::kiro::kiro_version::streaming_sdk_version_for_ide(&ide_version);
        format!(
            "aws-sdk-js/1.0.34 ua/2.1 os/{} lang/js md/nodejs#{} api/codewhispererstreaming#{} m/E KiroIDE-{}-{}",
            ctx.config.system_version,
            ctx.config.node_version,
            streaming_sdk_version,
            ide_version,
            ctx.machine_id,
        )
    }

    fn x_amz_user_agent(&self, ctx: &RequestContext<'_>) -> String {
        format!(
            "aws-sdk-js/1.0.34 KiroIDE-{}-{}",
            ctx.config.effective_kiro_version(),
            ctx.machine_id,
        )
    }

    fn decorate_token_headers(
        &self,
        mut req: RequestBuilder,
        ctx: &RequestContext<'_>,
    ) -> RequestBuilder {
        if ctx.credentials.is_api_key_credential() {
            req = req.header("tokentype", "API_KEY");
        }
        if is_external_idp_credentials(ctx.credentials) {
            req = req.header("TokenType", "EXTERNAL_IDP");
        }
        req
    }

    pub(crate) fn api_url(&self, ctx: &RequestContext<'_>) -> String {
        format!("{}/generateAssistantResponse", self.base_url(ctx))
    }

    pub(crate) fn mcp_url(&self, ctx: &RequestContext<'_>) -> String {
        format!("{}/mcp", self.base_url(ctx))
    }

    pub(crate) fn models_url(&self, ctx: &RequestContext<'_>, next_token: Option<&str>) -> String {
        let mut params = vec!["origin=AI_EDITOR".to_string(), "maxResults=50".to_string()];
        if let Some(profile_arn) = resolve_models_profile_arn(ctx.credentials, ctx.config) {
            params.push(format!("profileArn={}", urlencoding::encode(&profile_arn)));
        }
        if let Some(next_token) = next_token {
            params.push(format!("nextToken={}", urlencoding::encode(next_token)));
        }
        format!(
            "{}/ListAvailableModels?{}",
            self.base_url_for_host(ctx, &self.models_host(ctx)),
            params.join("&")
        )
    }

    pub(crate) fn decorate_api(
        &self,
        req: RequestBuilder,
        ctx: &RequestContext<'_>,
    ) -> RequestBuilder {
        let mut req = req
            .header("x-amzn-codewhisperer-optout", "true")
            .header("x-amzn-kiro-agent-mode", "vibe")
            .header("x-amz-user-agent", self.x_amz_user_agent(ctx))
            .header("user-agent", self.user_agent(ctx))
            .header("host", self.host(ctx))
            .header("amz-sdk-invocation-id", Uuid::new_v4().to_string())
            .header("amz-sdk-request", "attempt=1; max=3")
            .header("Authorization", format!("Bearer {}", ctx.token));
        if let Some(target) = self.target() {
            req = req.header("x-amz-target", target);
        }
        self.decorate_token_headers(req, ctx)
    }

    pub(crate) fn decorate_mcp(
        &self,
        req: RequestBuilder,
        ctx: &RequestContext<'_>,
    ) -> RequestBuilder {
        let mut req = req
            .header("x-amz-user-agent", self.x_amz_user_agent(ctx))
            .header("user-agent", self.user_agent(ctx))
            .header("host", self.host(ctx))
            .header("amz-sdk-invocation-id", Uuid::new_v4().to_string())
            .header("amz-sdk-request", "attempt=1; max=3")
            .header("Authorization", format!("Bearer {}", ctx.token));
        if let Some(arn) = resolve_mcp_profile_arn(ctx.credentials, ctx.config) {
            req = req.header("x-amzn-kiro-profile-arn", arn);
        }
        self.decorate_token_headers(req, ctx)
    }

    pub(crate) fn decorate_models(
        &self,
        req: RequestBuilder,
        ctx: &RequestContext<'_>,
    ) -> RequestBuilder {
        let mut req = req
            .header("accept", "application/json")
            .header("x-amzn-codewhisperer-optout", "true")
            .header("x-amz-user-agent", self.x_amz_user_agent(ctx))
            .header("user-agent", self.user_agent(ctx))
            .header("host", self.models_host(ctx))
            .header("amz-sdk-invocation-id", Uuid::new_v4().to_string())
            .header("amz-sdk-request", "attempt=1; max=3")
            .header("Authorization", format!("Bearer {}", ctx.token));
        if let Some(arn) = resolve_models_profile_arn(ctx.credentials, ctx.config) {
            req = req.header("x-amzn-kiro-profile-arn", arn);
        }
        self.decorate_token_headers(req, ctx)
    }

    pub(crate) fn transform_api_body(&self, body: &str, ctx: &RequestContext<'_>) -> String {
        transform_ide_api_body(
            body,
            &resolve_streaming_profile_arn(ctx.credentials, ctx.config),
        )
    }

    pub(crate) fn supports_auth_kind(&self, auth_kind: EndpointAuthKind) -> bool {
        matches!(
            auth_kind,
            EndpointAuthKind::Social | EndpointAuthKind::Idc | EndpointAuthKind::ExternalIdp
        )
    }

    pub(crate) fn supports_operation(&self, operation: EndpointOperation) -> bool {
        // runtime.{region}.kiro.dev 对 ListAvailableModels 返回 404 UnknownOperationException
        // （2026-11-11 FREE Social/IdC 实测）。runtime 作主端点时模型列表已改走 q host，
        // 作备用候选只会重复 amazonq 的同一 host，不再进入模型列表候选链。
        !(self.route == CompatRoute::Runtime && operation == EndpointOperation::Models)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kiro::endpoint::{
        AmazonqEndpoint, CodewhispererEndpoint, KiroEndpoint, RuntimeEndpoint,
    };
    use crate::kiro::model::credentials::KiroCredentials;
    use crate::model::config::Config;

    fn ctx<'a>(credentials: &'a KiroCredentials, config: &'a Config) -> RequestContext<'a> {
        RequestContext {
            credentials,
            token: "token",
            machine_id: "machine",
            config,
            region_override: None,
        }
    }

    #[test]
    fn routes_use_expected_host_families_and_targets() {
        let credentials = KiroCredentials::default();
        let config = Config::default();
        let context = ctx(&credentials, &config);
        let codewhisperer = CompatEndpoint::new(CompatRoute::Codewhisperer);
        let amazonq = CompatEndpoint::new(CompatRoute::Amazonq);
        let runtime = CompatEndpoint::new(CompatRoute::Runtime);
        assert_eq!(
            codewhisperer.host(&context),
            "codewhisperer.us-east-1.amazonaws.com"
        );
        assert_eq!(amazonq.host(&context), "q.us-east-1.amazonaws.com");
        assert_eq!(runtime.host(&context), "runtime.us-east-1.kiro.dev");
        assert_eq!(
            codewhisperer.target(),
            Some("AmazonCodeWhispererStreamingService.GenerateAssistantResponse")
        );
        assert_eq!(
            amazonq.target(),
            Some("AmazonQDeveloperStreamingService.SendMessage")
        );
        assert_eq!(runtime.target(), None);
    }

    #[test]
    fn runtime_route_lists_models_on_q_host() {
        let credentials = KiroCredentials::default();
        let config = Config::default();
        let context = ctx(&credentials, &config);
        let runtime = CompatEndpoint::new(CompatRoute::Runtime);
        assert!(
            runtime
                .models_url(&context, None)
                .starts_with("https://q.us-east-1.amazonaws.com/ListAvailableModels?")
        );
        let request = runtime
            .decorate_models(reqwest::Client::new().get("http://localhost"), &context)
            .build()
            .unwrap();
        assert_eq!(request.headers()["host"], "q.us-east-1.amazonaws.com");
        assert!(
            runtime
                .api_url(&context)
                .starts_with("https://runtime.us-east-1.kiro.dev/")
        );
    }

    #[test]
    fn runtime_route_is_not_a_model_catalog_candidate() {
        let runtime = CompatEndpoint::new(CompatRoute::Runtime);
        assert!(runtime.supports_operation(EndpointOperation::Inference));
        assert!(runtime.supports_operation(EndpointOperation::Mcp));
        assert!(!runtime.supports_operation(EndpointOperation::Models));
        for route in [CompatRoute::Codewhisperer, CompatRoute::Amazonq] {
            assert!(CompatEndpoint::new(route).supports_operation(EndpointOperation::Models));
        }
    }

    #[test]
    fn codewhisperer_uses_q_host_outside_us_east_one() {
        let credentials = KiroCredentials {
            api_region: Some("eu-central-1".to_string()),
            ..Default::default()
        };
        let config = Config::default();
        let context = ctx(&credentials, &config);
        let route = CompatEndpoint::new(CompatRoute::Codewhisperer);
        assert_eq!(route.host(&context), "q.eu-central-1.amazonaws.com");
    }

    #[test]
    fn api_decoration_keeps_route_target_and_same_account_identity() {
        let credentials = KiroCredentials {
            auth_method: Some("external_idp".to_string()),
            ..Default::default()
        };
        let config = Config::default();
        let context = ctx(&credentials, &config);
        let route = CompatEndpoint::new(CompatRoute::Codewhisperer);
        let request = route
            .decorate_api(
                reqwest::Client::new().post(route.api_url(&context)),
                &context,
            )
            .build()
            .unwrap();
        let headers = request.headers();
        assert_eq!(
            headers.get("x-amz-target").unwrap(),
            "AmazonCodeWhispererStreamingService.GenerateAssistantResponse"
        );
        assert_eq!(headers.get("authorization").unwrap(), "Bearer token");
        assert_eq!(headers.get("tokenType").unwrap(), "EXTERNAL_IDP");
    }

    #[test]
    fn mcp_decoration_sends_builder_id_profile_placeholder() {
        let credentials = KiroCredentials {
            auth_method: Some("idc".to_string()),
            client_id: Some("client".to_string()),
            client_secret: Some("secret".to_string()),
            ..Default::default()
        };
        let config = Config::default();
        let context = ctx(&credentials, &config);
        let route = CompatEndpoint::new(CompatRoute::Amazonq);
        let request = route
            .decorate_mcp(
                reqwest::Client::new().post(route.mcp_url(&context)),
                &context,
            )
            .build()
            .unwrap();

        assert_eq!(
            request.headers()["x-amzn-kiro-profile-arn"],
            crate::kiro::protocol::KIRO_BUILDER_ID_PLACEHOLDER_ARN
        );
    }

    #[test]
    fn compatible_mcp_routes_share_authentication_profile_rules() {
        let real_arn = "arn:aws:codewhisperer:us-east-1:123456789012:profile/REAL";
        let cases = [
            (
                "iam",
                KiroCredentials {
                    auth_method: Some("iam".to_string()),
                    client_id: Some("client".to_string()),
                    client_secret: Some("secret".to_string()),
                    ..Default::default()
                },
                Some(crate::kiro::protocol::KIRO_BUILDER_ID_PLACEHOLDER_ARN),
                None,
            ),
            (
                "idc",
                KiroCredentials {
                    auth_method: Some("idc".to_string()),
                    client_id: Some("client".to_string()),
                    client_secret: Some("secret".to_string()),
                    ..Default::default()
                },
                Some(crate::kiro::protocol::KIRO_BUILDER_ID_PLACEHOLDER_ARN),
                None,
            ),
            (
                "builderid",
                KiroCredentials {
                    auth_method: Some("builderid".to_string()),
                    client_id: Some("client".to_string()),
                    client_secret: Some("secret".to_string()),
                    ..Default::default()
                },
                Some(crate::kiro::protocol::KIRO_BUILDER_ID_PLACEHOLDER_ARN),
                None,
            ),
            (
                "organization idc with enterprise provider",
                KiroCredentials {
                    auth_method: Some("IdC".to_string()),
                    provider: Some("Enterprise".to_string()),
                    client_id: Some("client".to_string()),
                    client_secret: Some("secret".to_string()),
                    ..Default::default()
                },
                Some(crate::kiro::protocol::KIRO_BUILDER_ID_PLACEHOLDER_ARN),
                None,
            ),
            (
                "external idp without real profile",
                KiroCredentials {
                    auth_method: Some("external_idp".to_string()),
                    provider: Some("Enterprise".to_string()),
                    ..Default::default()
                },
                None,
                Some("EXTERNAL_IDP"),
            ),
            (
                "external idp with real profile",
                KiroCredentials {
                    auth_method: Some("external_idp".to_string()),
                    provider: Some("Enterprise".to_string()),
                    profile_arn: Some(real_arn.to_string()),
                    ..Default::default()
                },
                Some(real_arn),
                Some("EXTERNAL_IDP"),
            ),
            (
                "api key ignores stored profile",
                KiroCredentials {
                    auth_method: Some("api_key".to_string()),
                    kiro_api_key: Some("ksk_test".to_string()),
                    provider: Some("Enterprise".to_string()),
                    profile_arn: Some(real_arn.to_string()),
                    ..Default::default()
                },
                None,
                Some("API_KEY"),
            ),
        ];
        let routes: [(&str, Box<dyn KiroEndpoint>); 3] = [
            ("codewhisperer", Box::new(CodewhispererEndpoint::new())),
            ("amazonq", Box::new(AmazonqEndpoint::new())),
            ("runtime", Box::new(RuntimeEndpoint::new())),
        ];
        let config = Config::default();

        for (route_name, route) in routes {
            for (label, credentials, expected_arn, expected_token_type) in &cases {
                let context = ctx(credentials, &config);
                let request = route
                    .decorate_mcp(
                        reqwest::Client::new().post(route.mcp_url(&context)),
                        &context,
                    )
                    .build()
                    .unwrap();
                let headers = request.headers();

                assert_eq!(
                    headers
                        .get("x-amzn-kiro-profile-arn")
                        .and_then(|value| value.to_str().ok()),
                    *expected_arn,
                    "unexpected Profile ARN for {route_name} / {label}"
                );
                assert_eq!(
                    headers
                        .get("tokenType")
                        .and_then(|value| value.to_str().ok()),
                    *expected_token_type,
                    "unexpected TokenType for {route_name} / {label}"
                );
            }
        }
    }

    #[test]
    fn routes_preserve_streaming_profile_transform() {
        let credentials = KiroCredentials {
            auth_method: Some("social".to_string()),
            ..Default::default()
        };
        let config = Config::default();
        let context = ctx(&credentials, &config);
        let route = CompatEndpoint::new(CompatRoute::Amazonq);
        let body = route.transform_api_body(r#"{"conversationState":{}}"#, &context);
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            value["profileArn"],
            crate::kiro::protocol::KIRO_SOCIAL_PROFILE_ARN
        );
    }

    #[test]
    fn compatible_routes_use_the_sdk_paired_with_the_request_ide_version() {
        let credentials = KiroCredentials {
            auth_method: Some("social".to_string()),
            ..Default::default()
        };
        let mut config = Config::default();
        config.pin_kiro_version_for_request("1.4.121".to_string());
        let context = ctx(&credentials, &config);
        let route = CompatEndpoint::new(CompatRoute::Runtime);
        let request = route
            .decorate_api(reqwest::Client::new().post("https://example.com"), &context)
            .build()
            .unwrap();

        assert_eq!(
            request.headers()["user-agent"],
            format!(
                "aws-sdk-js/1.0.34 ua/2.1 os/{} lang/js md/nodejs#{} api/codewhispererstreaming#1.0.44 m/E KiroIDE-1.4.121-machine",
                config.system_version, config.node_version
            )
        );
        assert_eq!(
            request.headers()["x-amz-user-agent"],
            "aws-sdk-js/1.0.34 KiroIDE-1.4.121-machine"
        );
    }
}
