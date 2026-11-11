//! CodeWhisperer-compatible Kiro streaming route.

use reqwest::RequestBuilder;

use super::compat::{CompatEndpoint, CompatRoute};
use super::{EndpointAuthKind, EndpointOperation, KiroEndpoint, RequestContext};

pub const CODEWHISPERER_ENDPOINT_NAME: &str = "codewhisperer";

pub struct CodewhispererEndpoint {
    inner: CompatEndpoint,
}

impl CodewhispererEndpoint {
    pub fn new() -> Self {
        Self {
            inner: CompatEndpoint::new(CompatRoute::Codewhisperer),
        }
    }
}

impl Default for CodewhispererEndpoint {
    fn default() -> Self {
        Self::new()
    }
}

impl KiroEndpoint for CodewhispererEndpoint {
    fn name(&self) -> &'static str {
        CODEWHISPERER_ENDPOINT_NAME
    }
    fn supports_auth_kind(&self, auth_kind: EndpointAuthKind) -> bool {
        self.inner.supports_auth_kind(auth_kind)
    }
    fn supports_operation(&self, operation: EndpointOperation) -> bool {
        self.inner.supports_operation(operation)
    }
    fn api_url(&self, ctx: &RequestContext<'_>) -> String {
        self.inner.api_url(ctx)
    }
    fn mcp_url(&self, ctx: &RequestContext<'_>) -> String {
        self.inner.mcp_url(ctx)
    }
    fn models_url(&self, ctx: &RequestContext<'_>, next_token: Option<&str>) -> String {
        self.inner.models_url(ctx, next_token)
    }
    fn decorate_api(&self, req: RequestBuilder, ctx: &RequestContext<'_>) -> RequestBuilder {
        self.inner.decorate_api(req, ctx)
    }
    fn decorate_mcp(&self, req: RequestBuilder, ctx: &RequestContext<'_>) -> RequestBuilder {
        self.inner.decorate_mcp(req, ctx)
    }
    fn decorate_models(&self, req: RequestBuilder, ctx: &RequestContext<'_>) -> RequestBuilder {
        self.inner.decorate_models(req, ctx)
    }
    fn transform_api_body(&self, body: &str, ctx: &RequestContext<'_>) -> String {
        self.inner.transform_api_body(body, ctx)
    }
}
