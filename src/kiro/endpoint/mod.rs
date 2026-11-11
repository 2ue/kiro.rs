//! Kiro 端点抽象
//!
//! 不同 Kiro 端点（如 `ide` / `cli`）在 URL、请求头、请求体上存在差异，
//! 但共享凭据池、Token 刷新、重试逻辑和 AWS event-stream 响应解码。
//!
//! [`KiroEndpoint`] 抽象了请求侧的差异点；`KiroProvider` 持有一个 endpoint 注册表，
//! 按凭据的 `endpoint` 字段选择对应实现。

use std::collections::HashSet;

use reqwest::{Method, RequestBuilder};

use crate::kiro::model::credentials::KiroCredentials;
use crate::model::config::Config;

pub mod amazonq;
pub mod cli;
pub mod codewhisperer;
mod compat;
pub mod ide;
pub mod runtime;

pub use amazonq::AmazonqEndpoint;
pub use cli::CliEndpoint;
pub use codewhisperer::CodewhispererEndpoint;
pub use ide::IdeEndpoint;
pub use runtime::RuntimeEndpoint;

/// 端点候选链使用的凭据认证类型。
///
/// 端点轮换只能在同一认证类型的实现之间进行。该类型描述的是请求所携带的
/// Kiro 身份，不是 refresh 操作使用的具体 OAuth provider。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EndpointAuthKind {
    Social,
    Idc,
    ExternalIdp,
    ApiKey,
    Unknown,
}

/// Protocol operation being routed through an endpoint implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EndpointOperation {
    Inference,
    Models,
    Mcp,
}

/// 从凭据字段归一化端点候选链所需的认证类型。
pub fn endpoint_auth_kind(credentials: &KiroCredentials) -> EndpointAuthKind {
    if credentials.is_api_key_credential() {
        return EndpointAuthKind::ApiKey;
    }
    if crate::kiro::protocol::is_builder_id_credentials(credentials) {
        return EndpointAuthKind::Idc;
    }
    if credentials.is_external_idp_refresh_credential() {
        return EndpointAuthKind::ExternalIdp;
    }
    if credentials.is_idc_refresh_credential() {
        return EndpointAuthKind::Idc;
    }

    match credentials.auth_method.as_deref() {
        None => EndpointAuthKind::Social,
        Some(value) if compact_endpoint_value(value) == "social" => EndpointAuthKind::Social,
        Some(_) => EndpointAuthKind::Unknown,
    }
}

fn compact_endpoint_value(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// 端点候选链运行时策略。
///
/// 默认关闭，且不携带任何候选项，因此调用方可以先接入该结构而不改变现有的
/// `credentials.endpoint > config.defaultEndpoint` 选择行为。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointRoutingPlan {
    enabled: bool,
    preferred_endpoint: String,
    fallback_order: Vec<String>,
}

impl Default for EndpointRoutingPlan {
    fn default() -> Self {
        Self::disabled()
    }
}

impl EndpointRoutingPlan {
    pub fn disabled() -> Self {
        Self::new(
            crate::kiro::endpoint::ide::IDE_ENDPOINT_NAME,
            false,
            Vec::new(),
        )
    }

    pub fn new(
        preferred_endpoint: impl Into<String>,
        enabled: bool,
        fallback_order: Vec<String>,
    ) -> Self {
        Self {
            enabled,
            preferred_endpoint: preferred_endpoint.into(),
            fallback_order,
        }
    }

    /// 从运行时配置构建候选计划；`default_endpoint` 是全局 preferred endpoint，
    /// 凭据显式 endpoint 仍由调用方优先传入 candidate resolver。
    pub fn from_config(config: &Config) -> Self {
        Self::new(
            config.default_endpoint.clone(),
            config.kiro_endpoint_fallback_enabled,
            config.kiro_endpoint_fallback_order.clone(),
        )
    }

    /// 解析管理配置中常见的逗号分隔候选链。
    ///
    /// 空白项会被丢弃，但保留首次出现顺序；真正的端点存在性和认证兼容性在
    /// [`resolve_endpoint_candidates`] 中依据当前 registry 再校验。
    #[cfg(test)]
    pub fn parse(preferred_endpoint: impl Into<String>, enabled: bool, value: &str) -> Self {
        let fallback_order = value
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect();
        Self::new(preferred_endpoint, enabled, fallback_order)
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn preferred_endpoint(&self) -> &str {
        &self.preferred_endpoint
    }

    pub fn fallback_order(&self) -> &[String] {
        &self.fallback_order
    }
}

/// 根据当前凭据和注册表生成有序 endpoint 候选名。
///
/// 主端点始终位于首位，即使策略关闭、候选不存在或主端点的能力声明暂未完善。
/// 这保证了策略层接入前后的默认行为一致。只有开启策略后，候选端点才需要同时
/// 满足：已注册、不是主端点、未重复，并声明支持当前认证类型。
pub fn resolve_endpoint_candidates<'a, I>(
    credentials: &KiroCredentials,
    registered: I,
    plan: &EndpointRoutingPlan,
    operation: EndpointOperation,
) -> Vec<String>
where
    I: IntoIterator<Item = &'a dyn KiroEndpoint>,
{
    let primary_endpoint = credentials
        .endpoint
        .as_deref()
        .unwrap_or(plan.preferred_endpoint())
        .trim();
    let mut candidates = vec![primary_endpoint.to_string()];
    if !plan.enabled() {
        return candidates;
    }

    let auth_kind = endpoint_auth_kind(credentials);
    let registered = registered
        .into_iter()
        .map(|endpoint| (endpoint.name(), endpoint))
        .collect::<std::collections::HashMap<_, _>>();
    let mut seen = HashSet::from([primary_endpoint.to_string()]);

    for name in plan.fallback_order() {
        let name = name.trim();
        if name.is_empty() || !seen.insert(name.to_string()) {
            continue;
        }
        let Some(endpoint) = registered.get(name) else {
            continue;
        };
        if endpoint.supports_auth_kind(auth_kind) && endpoint.supports_operation(operation) {
            candidates.push(name.to_string());
        }
    }
    candidates
}

/// Resolve a local/staging upstream override while preserving endpoint-specific Host headers.
///
/// The override changes only the transport destination. Callers must still decorate the request
/// with the logical AWS/Kiro host derived from the credential region.
pub(crate) fn configured_upstream_url(config: &Config, suffix: &str) -> Option<String> {
    let base = config
        .kiro_upstream_base_url
        .as_deref()
        .map(str::trim)
        .filter(|base| !base.is_empty())?
        .trim_end_matches('/');
    let suffix = suffix.trim_start_matches('/');
    Some(if suffix.is_empty() {
        format!("{base}/")
    } else {
        format!("{base}/{suffix}")
    })
}

/// Detect semantic ASCII object keys without parsing or allocating a JSON value tree.
///
/// Endpoint marker fast paths call this only when a raw `"key"` search missed and the
/// body contains a backslash, so escaped keys such as `"orig\u0069n"` cannot bypass a
/// required transform. Invalid JSON may produce a conservative marker hit, but the
/// subsequent real parse still fails closed and returns the original body unchanged.
pub(super) fn contains_json_object_key(body: &str, targets: &[&str]) -> bool {
    debug_assert!(targets.iter().all(|target| target.is_ascii()));
    let bytes = body.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'"' {
            index += 1;
            continue;
        }

        let string_start = index + 1;
        let Some(string_end) = json_string_end(bytes, string_start) else {
            return false;
        };
        index = string_end + 1;
        let mut next = index;
        while next < bytes.len() && matches!(bytes[next], b' ' | b'\t' | b'\r' | b'\n') {
            next += 1;
        }
        if next < bytes.len()
            && bytes[next] == b':'
            && targets.iter().any(|target| {
                json_string_matches_ascii(&bytes[string_start..string_end], target.as_bytes())
            })
        {
            return true;
        }
    }
    false
}

pub(super) fn serialize_json_with_capacity(
    value: &serde_json::Value,
    minimum_capacity: usize,
) -> Option<String> {
    let mut output = Vec::with_capacity(minimum_capacity);
    serde_json::to_writer(&mut output, value).ok()?;
    String::from_utf8(output).ok()
}

pub(super) fn body_may_need_output_config_thinking_normalization(body: &str) -> bool {
    let plain_markers = body.contains("\"additionalModelRequestFields\"")
        && body.contains("\"output_config\"")
        && body.contains("\"thinking\"");
    plain_markers
        || (body.as_bytes().contains(&b'\\')
            && contains_json_object_key(
                body,
                &["additionalModelRequestFields", "output_config", "thinking"],
            ))
}

pub(super) fn normalize_output_config_thinking_compatibility_json(
    json: &mut serde_json::Value,
) -> bool {
    let Some(fields) = json
        .get_mut("additionalModelRequestFields")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return false;
    };
    if fields
        .get("output_config")
        .is_none_or(serde_json::Value::is_null)
    {
        return false;
    }
    let incompatible_thinking = match fields.get("thinking") {
        Some(serde_json::Value::Object(thinking)) => thinking
            .get("type")
            .and_then(serde_json::Value::as_str)
            .is_none_or(|thinking_type| thinking_type != "adaptive"),
        Some(_) => true,
        None => false,
    };
    if incompatible_thinking {
        fields.remove("thinking");
        return true;
    }
    false
}

fn json_string_end(bytes: &[u8], mut index: usize) -> Option<usize> {
    while index < bytes.len() {
        match bytes[index] {
            b'"' => return Some(index),
            b'\\' => {
                index = index.checked_add(2)?;
            }
            _ => index += 1,
        }
    }
    None
}

fn json_string_matches_ascii(encoded: &[u8], target: &[u8]) -> bool {
    let mut input_index = 0;
    let mut target_index = 0;
    while input_index < encoded.len() {
        let semantic = if encoded[input_index] == b'\\' {
            input_index += 1;
            let Some(escape) = encoded.get(input_index).copied() else {
                return false;
            };
            input_index += 1;
            match escape {
                b'"' | b'\\' | b'/' => escape,
                b'b' => 0x08,
                b'f' => 0x0c,
                b'n' => b'\n',
                b'r' => b'\r',
                b't' => b'\t',
                b'u' => {
                    let Some(hex) = encoded.get(input_index..input_index.saturating_add(4)) else {
                        return false;
                    };
                    let Some(codepoint) = decode_json_hex4(hex) else {
                        return false;
                    };
                    input_index += 4;
                    let Ok(ascii) = u8::try_from(codepoint) else {
                        return false;
                    };
                    if !ascii.is_ascii() {
                        return false;
                    }
                    ascii
                }
                _ => return false,
            }
        } else {
            let byte = encoded[input_index];
            input_index += 1;
            if !byte.is_ascii() || byte < 0x20 {
                return false;
            }
            byte
        };
        if target.get(target_index).copied() != Some(semantic) {
            return false;
        }
        target_index += 1;
    }
    target_index == target.len()
}

fn decode_json_hex4(hex: &[u8]) -> Option<u16> {
    if hex.len() != 4 {
        return None;
    }
    hex.iter().try_fold(0_u16, |value, byte| {
        let digit = match byte {
            b'0'..=b'9' => u16::from(byte - b'0'),
            b'a'..=b'f' => u16::from(byte - b'a' + 10),
            b'A'..=b'F' => u16::from(byte - b'A' + 10),
            _ => return None,
        };
        Some((value << 4) | digit)
    })
}

/// Kiro 端点
///
/// 同一个 `KiroProvider` 可持有多个 endpoint 实现，按凭据级字段切换。
pub trait KiroEndpoint: Send + Sync {
    /// 端点名称（对应 credentials.endpoint / config.defaultEndpoint 的取值）
    fn name(&self) -> &'static str;

    /// 声明该实现接受的凭据认证类型。
    ///
    /// 新端点未声明能力时不会进入 fallback 候选链；primary 仍保持旧路由行为。
    fn supports_auth_kind(&self, _auth_kind: EndpointAuthKind) -> bool {
        false
    }

    /// 声明该实现支持的操作；未声明的实现仅可作为显式 primary，不能进入 fallback。
    fn supports_operation(&self, _operation: EndpointOperation) -> bool {
        false
    }

    /// API endpoint URL
    fn api_url(&self, ctx: &RequestContext<'_>) -> String;

    /// API/MCP request content type.
    ///
    /// IDE uses normal JSON. CLI runtime uses AWS JSON 1.0.
    fn content_type(&self) -> &'static str {
        "application/json"
    }

    /// MCP uses JSON-RPC JSON even when the endpoint's inference protocol uses AWS JSON.
    fn mcp_content_type(&self) -> &'static str {
        "application/json"
    }

    /// MCP endpoint URL
    fn mcp_url(&self, ctx: &RequestContext<'_>) -> String;

    /// ListAvailableModels endpoint URL.
    fn models_url(&self, ctx: &RequestContext<'_>, next_token: Option<&str>) -> String;

    /// ListAvailableModels HTTP method.
    ///
    /// IDE-compatible endpoints use the legacy GET form. The Kiro CLI management
    /// endpoint uses AWS JSON 1.0 POST with a JSON body.
    fn models_method(&self, _ctx: &RequestContext<'_>) -> Method {
        Method::GET
    }

    /// Optional ListAvailableModels JSON body.
    fn models_body(
        &self,
        _ctx: &RequestContext<'_>,
        _next_token: Option<&str>,
    ) -> Option<serde_json::Value> {
        None
    }

    /// 装饰 API 请求的端点特有 header
    ///
    /// Provider 已经设置好 URL、content-type、Connection 和 body；
    /// 实现负责追加 Authorization、host、user-agent 等端点相关头。
    fn decorate_api(&self, req: RequestBuilder, ctx: &RequestContext<'_>) -> RequestBuilder;

    /// 装饰 MCP 请求的端点特有 header
    fn decorate_mcp(&self, req: RequestBuilder, ctx: &RequestContext<'_>) -> RequestBuilder;

    /// 装饰 ListAvailableModels 请求的端点特有 header
    fn decorate_models(&self, req: RequestBuilder, ctx: &RequestContext<'_>) -> RequestBuilder;

    /// 对已序列化的 API 请求体做端点特有加工（如注入 profileArn）
    fn transform_api_body(&self, body: &str, ctx: &RequestContext<'_>) -> String;

    /// 对已序列化的 MCP 请求体做端点特有加工（默认不变）
    fn transform_mcp_body(&self, body: &str, _ctx: &RequestContext<'_>) -> String {
        body.to_string()
    }

    /// 判断响应体是否表示"额度/透支请求额度用尽"（禁用凭据并转移）
    fn is_monthly_request_limit(&self, body: &str) -> bool {
        default_is_monthly_request_limit(body)
    }

    /// 判断响应体是否表示"上游 bearer token 失效"（触发强制刷新）
    fn is_bearer_token_invalid(&self, body: &str) -> bool {
        default_is_bearer_token_invalid(body)
    }
}

/// 装饰请求时可用的上下文
///
/// 包含单次调用已确定的所有运行时信息。引用形式避免无谓 clone。
pub struct RequestContext<'a> {
    /// 当前凭据
    pub credentials: &'a KiroCredentials,
    /// 有效的 access token（API Key 凭据下即 kiroApiKey）
    pub token: &'a str,
    /// 当前凭据对应的 machineId
    pub machine_id: &'a str,
    /// 全局配置
    pub config: &'a Config,
    /// 本次尝试的 region 覆盖（狂暴 / region 轮换用）。
    ///
    /// `None` 表示沿用凭据自身的 [`KiroCredentials::effective_api_region`]，
    /// 即引入 region 轮换之前的行为。`Some(region)` 时端点实现必须优先使用它
    /// 拼 `https://q.{region}.amazonaws.com`，从而在同一个账号上切换上游地址。
    ///
    /// 注意该覆盖只影响 **API region**（上游调用地址），不影响 auth region
    /// （token 刷新），避免轮换把 token 刷新也打到错误的区域。
    pub region_override: Option<&'a str>,
}

impl<'a> RequestContext<'a> {
    /// 解析本次尝试实际使用的 API region。
    ///
    /// 优先级：轮换覆盖 > 凭据自身解析链（credentials.api_region > profileArn
    /// 内嵌 region > 全局配置）。
    pub fn effective_api_region(&self) -> &str {
        self.region_override
            .map(str::trim)
            .filter(|region| !region.is_empty())
            .unwrap_or_else(|| self.credentials.effective_api_region(self.config))
    }
}

/// 默认的额度用尽判断逻辑
///
/// 同时识别顶层 `reason` 字段、嵌套 `error.reason` 字段和 Kiro overage 限制。
pub fn default_is_quota_exhausted(body: &str) -> bool {
    if body.contains("MONTHLY_REQUEST_COUNT") || body.contains("OVERAGE_REQUEST_LIMIT_EXCEEDED") {
        return true;
    }

    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };

    if value
        .get("reason")
        .and_then(|v| v.as_str())
        .is_some_and(|v| {
            matches!(
                v,
                "MONTHLY_REQUEST_COUNT" | "OVERAGE_REQUEST_LIMIT_EXCEEDED"
            )
        })
    {
        return true;
    }

    value
        .pointer("/error/reason")
        .and_then(|v| v.as_str())
        .is_some_and(|v| {
            matches!(
                v,
                "MONTHLY_REQUEST_COUNT" | "OVERAGE_REQUEST_LIMIT_EXCEEDED"
            )
        })
}

/// 向后兼容旧名称：语义已扩展为额度用尽。
pub fn default_is_monthly_request_limit(body: &str) -> bool {
    default_is_quota_exhausted(body)
}

/// 400 响应体是否是额度用尽。
///
/// cli 端点（api_key 账号）额度用尽时返回 400 `ServiceQuotaExceededException`，
/// 带 `reason: MONTHLY_REQUEST_COUNT`，而不是 402。400 大多是请求本身的问题，
/// 所以这里只认结构化的 `reason` 字段，不做子串匹配，避免把回显了这些字样的
/// 请求错误误判成账号额度用尽。
pub fn is_quota_exhausted_bad_request(body: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };
    ["/reason", "/error/reason"].into_iter().any(|pointer| {
        value
            .pointer(pointer)
            .and_then(|v| v.as_str())
            .is_some_and(|v| {
                matches!(
                    v,
                    "MONTHLY_REQUEST_COUNT" | "OVERAGE_REQUEST_LIMIT_EXCEEDED"
                )
            })
    })
}

/// 默认的 bearer token 失效判断逻辑
pub fn default_is_bearer_token_invalid(body: &str) -> bool {
    body.contains("The bearer token included in the request is invalid")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registered_endpoints() -> [Box<dyn KiroEndpoint>; 5] {
        [
            Box::new(IdeEndpoint::new()),
            Box::new(CliEndpoint::new()),
            Box::new(CodewhispererEndpoint::new()),
            Box::new(AmazonqEndpoint::new()),
            Box::new(RuntimeEndpoint::new()),
        ]
    }

    fn registered_refs(endpoints: &[Box<dyn KiroEndpoint>]) -> Vec<&dyn KiroEndpoint> {
        endpoints.iter().map(Box::as_ref).collect()
    }

    #[test]
    fn endpoint_routing_plan_is_disabled_by_default() {
        let plan = EndpointRoutingPlan::default();
        let endpoints = registered_endpoints();
        let credentials = KiroCredentials {
            auth_method: Some("social".to_string()),
            endpoint: None,
            ..Default::default()
        };

        assert_eq!(endpoint_auth_kind(&credentials), EndpointAuthKind::Social);
        assert_eq!(
            resolve_endpoint_candidates(
                &credentials,
                registered_refs(&endpoints),
                &plan,
                EndpointOperation::Inference,
            ),
            vec!["ide"],
            "默认关闭时只返回原 primary endpoint"
        );
    }

    #[test]
    fn endpoint_routing_plan_deduplicates_unknown_and_blank_names() {
        let plan = EndpointRoutingPlan::parse("cli", true, " cli, ,ide,cli,missing ");
        assert_eq!(
            plan.fallback_order(),
            &["cli", "ide", "cli", "missing"],
            "解析只负责清理空白，候选存在性由 resolver 判断"
        );

        let endpoints = registered_endpoints();
        let credentials = KiroCredentials {
            auth_method: Some("api_key".to_string()),
            endpoint: None,
            kiro_api_key: Some("ksk_test".to_string()),
            ..Default::default()
        };
        assert_eq!(
            resolve_endpoint_candidates(
                &credentials,
                registered_refs(&endpoints),
                &plan,
                EndpointOperation::Inference,
            ),
            vec!["cli"],
            "primary 和重复候选不能重复出现，未知项必须丢弃"
        );
    }

    #[test]
    fn endpoint_routing_plan_rejects_cross_auth_ide_cli_candidates() {
        let endpoints = registered_endpoints();
        let social = KiroCredentials {
            auth_method: Some("social".to_string()),
            endpoint: None,
            ..Default::default()
        };
        let api_key = KiroCredentials {
            auth_method: Some("api_key".to_string()),
            endpoint: None,
            kiro_api_key: Some("ksk_test".to_string()),
            ..Default::default()
        };
        let api_key_plan = EndpointRoutingPlan::new("cli", true, vec!["ide".to_string()]);
        let social_plan = EndpointRoutingPlan::new("ide", true, vec!["cli".to_string()]);

        assert_eq!(
            resolve_endpoint_candidates(
                &social,
                registered_refs(&endpoints),
                &social_plan,
                EndpointOperation::Inference,
            ),
            vec!["ide"],
            "social/OAuth 请求不能回退到 CLI API-key 协议"
        );
        assert_eq!(
            resolve_endpoint_candidates(
                &api_key,
                registered_refs(&endpoints),
                &api_key_plan,
                EndpointOperation::Inference,
            ),
            vec!["cli"],
            "API-key 请求不能回退到 IDE OAuth 协议"
        );
    }

    #[test]
    fn endpoint_auth_kind_preserves_idc_and_external_idp_distinction() {
        let idc = KiroCredentials {
            auth_method: Some("builder-id".to_string()),
            client_id: Some("client".to_string()),
            client_secret: Some("secret".to_string()),
            ..Default::default()
        };
        let external_idp = KiroCredentials {
            auth_method: Some("external_idp".to_string()),
            refresh_token: Some("refresh".to_string()),
            token_endpoint: Some("https://example.invalid/token".to_string()),
            ..Default::default()
        };

        assert_eq!(endpoint_auth_kind(&idc), EndpointAuthKind::Idc);
        assert_eq!(
            endpoint_auth_kind(&external_idp),
            EndpointAuthKind::ExternalIdp
        );

        let provider_only_builder_id = KiroCredentials {
            provider: Some("BuilderId".to_string()),
            ..Default::default()
        };
        assert_eq!(
            endpoint_auth_kind(&provider_only_builder_id),
            EndpointAuthKind::Idc
        );
    }

    #[test]
    fn oauth_fallback_order_accepts_registered_compat_routes() {
        let endpoints = registered_endpoints();
        let credentials = KiroCredentials {
            auth_method: Some("social".to_string()),
            ..Default::default()
        };
        let plan = EndpointRoutingPlan::new(
            "ide",
            true,
            vec![
                "codewhisperer".to_string(),
                "amazonq".to_string(),
                "runtime".to_string(),
                "cli".to_string(),
            ],
        );
        assert_eq!(
            resolve_endpoint_candidates(
                &credentials,
                registered_refs(&endpoints),
                &plan,
                EndpointOperation::Inference,
            ),
            vec!["ide", "codewhisperer", "amazonq", "runtime"],
            "OAuth fallback 不能跨到 API key CLI 协议"
        );
    }

    #[test]
    fn escaped_json_object_key_detector_covers_valid_and_adversarial_shapes_for_five_rounds() {
        let positives = [
            r#"{"orig\u0069n":"AI_EDITOR"}"#,
            r#"{"ORIGIN":"other","orig\u0069n":"AI_EDITOR"}"#,
            r#"{"additionalModelRequest\u0046ields":{}}"#,
            r#"{"additionalModelRequest\u0046ield\u0073":{}}"#,
            r#"{"output\u005Fconfig":{}}"#,
            r#"{"nested":{"orig\u0069n":"AI_EDITOR"}}"#,
        ];
        let negatives = [
            r#"{"text":"orig\u0069n"}"#,
            r#"{"text":"\"orig\u0069n\": value text"}"#,
            r#"{"orig\\u0069n":"literal backslash"}"#,
            r#"{"orig\u006An":"different key"}"#,
            r#"{"orig\uD800n":"surrogate key"}"#,
            r#"{"原点":"origin"}"#,
            r#"{"unterminated":"value}"#,
            r#"{"bad\u00xz":"value"}"#,
        ];
        let targets = ["origin", "additionalModelRequestFields", "output_config"];

        for round in 0..5 {
            for body in positives {
                assert!(
                    contains_json_object_key(body, &targets),
                    "round {round}: expected semantic key in {body:?}"
                );
            }
            for body in negatives {
                assert!(
                    !contains_json_object_key(body, &targets),
                    "round {round}: must not treat value/text/invalid escape as a target key in {body:?}"
                );
            }
        }
    }

    #[test]
    fn test_default_monthly_request_limit_detects_reason() {
        let body = r#"{"message":"You have reached the limit.","reason":"MONTHLY_REQUEST_COUNT"}"#;
        assert!(default_is_monthly_request_limit(body));
    }

    #[test]
    fn test_default_monthly_request_limit_nested_reason() {
        let body = r#"{"error":{"reason":"MONTHLY_REQUEST_COUNT"}}"#;
        assert!(default_is_monthly_request_limit(body));
    }

    #[test]
    fn test_default_monthly_request_limit_false() {
        let body = r#"{"message":"nope","reason":"DAILY_REQUEST_COUNT"}"#;
        assert!(!default_is_monthly_request_limit(body));
    }

    #[test]
    fn test_default_quota_exhausted_detects_overage_limit() {
        let body = r#"{"message":"You have reached the limit for overages.","reason":"OVERAGE_REQUEST_LIMIT_EXCEEDED"}"#;
        assert!(default_is_monthly_request_limit(body));
    }

    #[test]
    fn test_default_quota_exhausted_detects_nested_overage_limit() {
        let body = r#"{"error":{"reason":"OVERAGE_REQUEST_LIMIT_EXCEEDED"}}"#;
        assert!(default_is_quota_exhausted(body));
    }

    /// 2026-11-08 59138：cli 端点 api_key 账号额度用尽返回的是 400。
    #[test]
    fn test_quota_exhausted_bad_request_requires_reason_field() {
        let body = r#"{"__type":"com.amazon.kiro.runtimeservice#ServiceQuotaExceededException","message":"You have reached the limit.","reason":"MONTHLY_REQUEST_COUNT"}"#;
        assert!(is_quota_exhausted_bad_request(body));
        assert!(is_quota_exhausted_bad_request(
            r#"{"error":{"reason":"OVERAGE_REQUEST_LIMIT_EXCEEDED"}}"#
        ));
        // 请求错误里回显了这些字样，不算额度用尽。
        assert!(!is_quota_exhausted_bad_request(
            r#"{"message":"Improperly formed request: MONTHLY_REQUEST_COUNT","reason":null}"#
        ));
        assert!(!is_quota_exhausted_bad_request(
            "MONTHLY_REQUEST_COUNT in plain text"
        ));
        assert!(!is_quota_exhausted_bad_request(
            r#"{"message":"Input content length exceeds threshold.","reason":"CONTENT_LENGTH_EXCEEDS_THRESHOLD"}"#
        ));
    }

    #[test]
    fn test_default_bearer_token_invalid() {
        assert!(default_is_bearer_token_invalid(
            "The bearer token included in the request is invalid"
        ));
        assert!(!default_is_bearer_token_invalid("unrelated error"));
    }
}
