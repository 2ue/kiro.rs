//! WebSearch 工具处理模块
//!
//! 实现 Anthropic WebSearch 请求到 Kiro MCP 的转换和响应生成

use std::{convert::Infallible, sync::Arc};

use axum::{body::Body, http::StatusCode, response::Response};
use bytes::Bytes;
use futures::{Stream, stream};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use super::envelope;
use super::inference_attempt_budget::InferenceAttemptBudget;
use super::stream::SseEvent;
use super::types::MessagesRequest;
use crate::http_client::{HttpSendError, response_bytes_with_limit_and_body_timeout};
use crate::kiro::call_trace::McpCallAttributionSink;
use crate::kiro::provider::{McpCallAttribution, McpCallFailureKind};

const MAX_MCP_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const WEB_SEARCH_QUERY_PREFIX: &str = "Perform a web search for the query:";
const KNOWN_NATIVE_WEB_SEARCH_TOOL_TYPES: &[&str] = &[
    "web_search_20250305",
    "web_search_20260209",
    "web_search_20260318",
];

/// MCP 请求
#[derive(Debug, Serialize)]
pub struct McpRequest {
    pub id: String,
    pub jsonrpc: String,
    pub method: String,
    pub params: McpParams,
}

/// MCP 请求参数
#[derive(Debug, Serialize)]
pub struct McpParams {
    pub name: String,
    pub arguments: McpArguments,
}

/// MCP 参数
#[derive(Debug, Serialize)]
pub struct McpArguments {
    pub query: String,
}

/// MCP 响应
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct McpResponse {
    pub error: Option<Value>,
    pub id: String,
    pub jsonrpc: String,
    pub result: Option<McpResult>,
}

/// MCP 结果
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct McpResult {
    pub content: Vec<McpContent>,
    #[serde(rename = "isError")]
    pub is_error: bool,
}

/// MCP 内容
#[derive(Debug, Deserialize)]
pub struct McpContent {
    #[serde(rename = "type")]
    pub content_type: String,
    pub text: String,
}

/// WebSearch 搜索结果
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct WebSearchResults {
    pub results: Vec<WebSearchResult>,
    #[serde(rename = "totalResults")]
    pub total_results: Option<i32>,
    pub query: Option<String>,
    pub error: Option<String>,
}

/// 单个搜索结果
#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct WebSearchResult {
    pub title: String,
    pub url: String,
    pub snippet: Option<String>,
    #[serde(rename = "publishedDate")]
    pub published_date: Option<i64>,
    pub id: Option<String>,
    pub domain: Option<String>,
    #[serde(rename = "maxVerbatimWordLimit")]
    pub max_verbatim_word_limit: Option<i32>,
    #[serde(rename = "publicDomain")]
    pub public_domain: Option<bool>,
}

pub enum WebSearchOutcome {
    Success {
        response: Response,
        output_tokens: i32,
        attribution: McpCallAttribution,
    },
    Failure {
        response: Response,
        error_type: &'static str,
        internal_reason: &'static str,
        attribution: McpCallAttribution,
        tool_error: Option<WebSearchToolErrorOutcome>,
    },
}

pub struct WebSearchToolErrorOutcome {
    pub response: Response,
    pub output_tokens: i32,
    pub error_code: &'static str,
    pub internal_reason: &'static str,
}

pub struct WebSearchRequestContext<'a> {
    pub provider: Arc<crate::kiro::provider::KiroProvider>,
    pub payload: &'a MessagesRequest,
    pub input_tokens: i32,
    pub inference_attempt_budget: Arc<InferenceAttemptBudget>,
    pub attribution_sink: Arc<McpCallAttributionSink>,
    pub request_id: &'a str,
    pub error_id: &'a str,
}

struct WebSearchResponseContext<'a> {
    model: &'a str,
    query: &'a str,
    tool_use_id: &'a str,
    summary: &'a str,
    input_tokens: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WebSearchFailureKind {
    Scheduler,
    InvalidRequest,
    RateLimit,
    Timeout,
    AttemptLimit,
    Upstream,
    Protocol,
}

#[derive(Debug, Clone)]
struct WebSearchFailure {
    kind: WebSearchFailureKind,
    internal_reason: &'static str,
    attribution: Box<McpCallAttribution>,
}

impl WebSearchFailure {
    fn new(kind: WebSearchFailureKind, internal_reason: &'static str) -> Self {
        Self {
            kind,
            internal_reason,
            attribution: Box::default(),
        }
    }

    fn with_attribution(mut self, attribution: McpCallAttribution) -> Self {
        self.attribution = Box::new(attribution);
        self
    }

    fn from_provider_error(error: &anyhow::Error) -> Self {
        let failure = match crate::kiro::provider::KiroProvider::mcp_failure_kind_from_error(error)
        {
            Some(McpCallFailureKind::Scheduler) => Self::new(
                WebSearchFailureKind::Scheduler,
                "websearch_mcp_scheduler_unavailable",
            ),
            Some(McpCallFailureKind::InvalidRequest) => Self::new(
                WebSearchFailureKind::InvalidRequest,
                "websearch_mcp_invalid_request",
            ),
            Some(McpCallFailureKind::RateLimit) => {
                Self::new(WebSearchFailureKind::RateLimit, "websearch_mcp_rate_limit")
            }
            Some(McpCallFailureKind::Timeout) => {
                Self::new(WebSearchFailureKind::Timeout, "websearch_mcp_timeout")
            }
            Some(McpCallFailureKind::ResponseTooLarge) => Self::new(
                WebSearchFailureKind::Protocol,
                "websearch_mcp_response_too_large",
            ),
            Some(McpCallFailureKind::BodyRead) => {
                Self::new(WebSearchFailureKind::Upstream, "websearch_mcp_body_read")
            }
            Some(McpCallFailureKind::Protocol) => Self::new(
                WebSearchFailureKind::Protocol,
                "websearch_mcp_protocol_error",
            ),
            Some(McpCallFailureKind::AttemptLimit) => Self::new(
                WebSearchFailureKind::AttemptLimit,
                "websearch_mcp_attempt_limit",
            ),
            Some(McpCallFailureKind::AuxiliaryAttemptLimit) => Self::new(
                WebSearchFailureKind::AttemptLimit,
                "websearch_auxiliary_attempt_limit",
            ),
            Some(McpCallFailureKind::AuxiliaryConcurrency) => Self::new(
                WebSearchFailureKind::AttemptLimit,
                "websearch_auxiliary_concurrency",
            ),
            Some(McpCallFailureKind::Upstream) | None => Self::new(
                WebSearchFailureKind::Upstream,
                "websearch_mcp_upstream_error",
            ),
        };
        failure.with_attribution(
            crate::kiro::provider::KiroProvider::mcp_attribution_from_error(error),
        )
    }

    fn into_outcome(self, request_id: &str, error_id: &str) -> WebSearchOutcome {
        self.into_outcome_with_tool_error(request_id, error_id, None)
    }

    fn into_outcome_with_tool_error(
        self,
        request_id: &str,
        error_id: &str,
        tool_error: Option<WebSearchToolErrorOutcome>,
    ) -> WebSearchOutcome {
        let (status, error_type, message, retry_after) = match self.kind {
            WebSearchFailureKind::Scheduler => (
                StatusCode::SERVICE_UNAVAILABLE,
                "api_error",
                envelope::PUBLIC_TEMPORARY_FAILURE_MESSAGE,
                Some("1".to_string()),
            ),
            WebSearchFailureKind::InvalidRequest => (
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                envelope::PUBLIC_INVALID_REQUEST_MESSAGE,
                None,
            ),
            WebSearchFailureKind::RateLimit => (
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limit_error",
                envelope::PUBLIC_RATE_LIMIT_MESSAGE,
                Some("1".to_string()),
            ),
            WebSearchFailureKind::Timeout => (
                StatusCode::GATEWAY_TIMEOUT,
                "api_error",
                envelope::PUBLIC_TEMPORARY_FAILURE_MESSAGE,
                None,
            ),
            WebSearchFailureKind::AttemptLimit => (
                StatusCode::SERVICE_UNAVAILABLE,
                "api_error",
                envelope::PUBLIC_TEMPORARY_FAILURE_MESSAGE,
                Some("1".to_string()),
            ),
            WebSearchFailureKind::Upstream | WebSearchFailureKind::Protocol => (
                StatusCode::BAD_GATEWAY,
                "api_error",
                envelope::PUBLIC_PROCESSING_FAILED_MESSAGE,
                None,
            ),
        };
        let public_message = envelope::public_message_with_error_id(message, error_id);
        let mut headers = vec![("x-error-id", error_id.to_string())];
        if let Some(retry_after) = retry_after {
            headers.push(("retry-after", retry_after));
        }
        WebSearchOutcome::Failure {
            response: envelope::error_response_with_id_and_headers(
                status,
                error_type,
                public_message,
                request_id,
                headers,
            ),
            error_type,
            internal_reason: self.internal_reason,
            attribution: *self.attribution,
            tool_error,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum WebSearchToolErrorCode {
    TooManyRequests,
    InvalidToolInput,
    MaxUsesExceeded,
    QueryTooLong,
    RequestTooLarge,
    Unavailable,
}

impl WebSearchToolErrorCode {
    fn as_str(self) -> &'static str {
        match self {
            Self::TooManyRequests => "too_many_requests",
            Self::InvalidToolInput => "invalid_tool_input",
            Self::MaxUsesExceeded => "max_uses_exceeded",
            Self::QueryTooLong => "query_too_long",
            Self::RequestTooLarge => "request_too_large",
            Self::Unavailable => "unavailable",
        }
    }
}

fn websearch_tool_error_code_for_failure(failure: &WebSearchFailure) -> WebSearchToolErrorCode {
    match failure.kind {
        WebSearchFailureKind::InvalidRequest => WebSearchToolErrorCode::InvalidToolInput,
        WebSearchFailureKind::RateLimit => WebSearchToolErrorCode::TooManyRequests,
        WebSearchFailureKind::Scheduler
        | WebSearchFailureKind::Timeout
        | WebSearchFailureKind::AttemptLimit
        | WebSearchFailureKind::Upstream
        | WebSearchFailureKind::Protocol => WebSearchToolErrorCode::Unavailable,
    }
}

fn should_continue_after_websearch_failure(failure: &WebSearchFailure) -> bool {
    !matches!(
        failure.internal_reason,
        "websearch_mcp_malformed_json"
            | "websearch_mcp_invalid_envelope"
            | "websearch_mcp_missing_result"
            | "websearch_mcp_non_utf8_response"
            | "websearch_mcp_response_too_large"
            | "websearch_mcp_invalid_search_result"
            | "websearch_mcp_protocol_error"
    )
}

fn is_known_native_web_search_tool_type(tool_type: &str) -> bool {
    KNOWN_NATIVE_WEB_SEARCH_TOOL_TYPES.contains(&tool_type)
}

fn is_versioned_web_search_tool_type(tool_type: &str) -> bool {
    let Some(version) = tool_type.strip_prefix("web_search_") else {
        return false;
    };
    version.len() == 8 && version.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_native_web_search_tool(tool: &crate::anthropic::types::Tool) -> bool {
    tool.name == "web_search"
        && tool
            .tool_type
            .as_deref()
            .is_some_and(is_versioned_web_search_tool_type)
}

pub fn has_unlisted_native_web_search_tool_type(req: &MessagesRequest) -> bool {
    req.tools.as_ref().is_some_and(|tools| {
        tools
            .iter()
            .filter(|tool| tool.name == "web_search")
            .filter_map(|tool| tool.tool_type.as_deref())
            .any(|tool_type| {
                is_versioned_web_search_tool_type(tool_type)
                    && !is_known_native_web_search_tool_type(tool_type)
            })
    })
}

pub fn has_misnamed_native_web_search_tool(req: &MessagesRequest) -> bool {
    req.tools.as_ref().is_some_and(|tools| {
        tools.iter().any(|tool| {
            tool.name != "web_search"
                && tool
                    .tool_type
                    .as_deref()
                    .is_some_and(is_versioned_web_search_tool_type)
        })
    })
}

/// 检查请求是否声明了 Anthropic 原生 WebSearch 工具。
pub fn has_native_web_search_tool(req: &MessagesRequest) -> bool {
    req.tools
        .as_ref()
        .is_some_and(|tools| tools.iter().any(is_native_web_search_tool))
}

/// 检查请求是否为纯 WebSearch 请求
///
/// 条件：tools 有且只有一个，且是 Anthropic 原生 WebSearch 类型。
pub fn has_web_search_tool(req: &MessagesRequest) -> bool {
    req.tools.as_ref().is_some_and(|tools| {
        tools.len() == 1 && tools.first().is_some_and(is_native_web_search_tool)
    })
}

/// 检查请求是否混用了 Anthropic 原生 WebSearch 和普通客户端工具。
///
/// 当前原生 WebSearch 由服务端 MCP 路径执行；普通工具由客户端执行。混用时如果继续
/// 走普通工具转换，会把 `web_search` 当成普通 `tool_use` 返回给没有执行器的客户端。
pub fn has_mixed_native_web_search_tool(req: &MessagesRequest) -> bool {
    req.tools
        .as_ref()
        .is_some_and(|tools| tools.len() > 1 && tools.iter().any(is_native_web_search_tool))
}

/// 从消息中提取搜索查询
///
/// 只读取最后一个 user turn 内的有效 text，并去除 Claude 的固定搜索前缀。
pub fn extract_search_query(req: &MessagesRequest) -> Option<String> {
    let current_user_message = req
        .messages
        .iter()
        .rev()
        .find(|message| message.role == "user")?;
    let text = match &current_user_message.content {
        Value::String(text) => non_empty_trimmed(text),
        Value::Array(blocks) => blocks.iter().rev().find_map(|block| {
            (block.get("type").and_then(Value::as_str) == Some("text"))
                .then(|| block.get("text").and_then(Value::as_str))
                .flatten()
                .and_then(non_empty_trimmed)
        }),
        _ => None,
    }?;
    let query = text
        .strip_prefix(WEB_SEARCH_QUERY_PREFIX)
        .unwrap_or(&text)
        .trim();
    (!query.is_empty()).then(|| query.to_string())
}

fn non_empty_trimmed(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// 发给 Kiro MCP web_search 的 query 最大字符数。
///
/// 上游未公开限制：实测约 560 字符的 query 返回 `invalid_tool_input`。这里取保守值 200，
/// 常规搜索词远小于此长度，超长 query 截断后仍保留主要关键词。
const MAX_WEB_SEARCH_QUERY_CHARS: usize = 200;

/// 把 query 截断到 `max_chars` 个字符以内，优先在词边界截断。
///
/// 截断点落在单词中间时回退到前一个空白；若回退会丢掉一半以上内容
/// （如无空格的中文长句），则直接按字符截断。
fn truncate_search_query(query: &str, max_chars: usize) -> String {
    let Some((cut, next)) = query.char_indices().nth(max_chars) else {
        return query.to_string();
    };
    let head = &query[..cut];
    let mid_word = !next.is_whitespace() && !head.ends_with(char::is_whitespace);
    let head = if mid_word {
        match head.rfind(char::is_whitespace) {
            Some(space) if head[..space].chars().count() >= max_chars / 2 => &head[..space],
            _ => head,
        }
    } else {
        head
    };
    head.trim_end().to_string()
}

/// 加入 query 的 `site:` 提示最多覆盖的 allowed 域名数，避免 query 过长。
const MAX_SITE_HINT_DOMAINS: usize = 3;

/// 原生 WebSearch 工具上的 `allowed_domains` / `blocked_domains`。
///
/// Kiro MCP web_search 只接受 query，因此代理在结果侧按域名（含子域名）过滤，
/// 并把 allowed 域名以 `site:` 形式辅助加入 query，提高命中率。
/// 两者同时出现时都生效：先按 allowed 保留，再排除 blocked。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebSearchDomainFilter {
    allowed: Vec<String>,
    blocked: Vec<String>,
}

impl WebSearchDomainFilter {
    pub fn from_request(req: &MessagesRequest) -> Self {
        let Some(tool) = req
            .tools
            .as_ref()
            .and_then(|tools| tools.iter().find(|tool| is_native_web_search_tool(tool)))
        else {
            return Self::default();
        };
        let filter = Self {
            allowed: normalize_domains(tool.allowed_domains.as_deref()),
            blocked: normalize_domains(tool.blocked_domains.as_deref()),
        };
        if !filter.allowed.is_empty() && !filter.blocked.is_empty() {
            tracing::info!(
                allowed_domains = ?filter.allowed,
                blocked_domains = ?filter.blocked,
                "native WebSearch declares both allowed_domains and blocked_domains; applying allowed first, then excluding blocked"
            );
        }
        filter
    }

    pub fn is_empty(&self) -> bool {
        self.allowed.is_empty() && self.blocked.is_empty()
    }

    /// 结果 URL（或 MCP 返回的 domain 字段）是否满足过滤条件。
    fn permits(&self, result: &WebSearchResult) -> bool {
        let Some(host) = result_host(result) else {
            // 无法判定域名时：有 allowed 限制则排除，否则保留。
            return self.allowed.is_empty();
        };
        let allowed = self.allowed.is_empty()
            || self
                .allowed
                .iter()
                .any(|domain| host_matches_domain(&host, domain));
        allowed
            && !self
                .blocked
                .iter()
                .any(|domain| host_matches_domain(&host, domain))
    }

    /// 过滤搜索结果，返回被移除的条数。
    fn apply(&self, results: &mut WebSearchResults) -> usize {
        if self.is_empty() {
            return 0;
        }
        let before = results.results.len();
        results.results.retain(|result| self.permits(result));
        before - results.results.len()
    }

    /// 发给 MCP 的 query：allowed 域名较少、且附加后不超过 query 长度上限时，
    /// 附加 `site:` 提示；否则只依赖结果侧过滤。
    fn mcp_query(&self, query: &str) -> String {
        if self.allowed.is_empty() || self.allowed.len() > MAX_SITE_HINT_DOMAINS {
            return query.to_string();
        }
        let sites = self
            .allowed
            .iter()
            .map(|domain| format!("site:{domain}"))
            .collect::<Vec<_>>()
            .join(" OR ");
        let combined = format!("{query} {sites}");
        if combined.chars().count() > MAX_WEB_SEARCH_QUERY_CHARS {
            return query.to_string();
        }
        combined
    }
}

/// `max_uses` ≤ 0 不是可服务的限制值：按未设置处理（使用默认行为）并记日志，不报错。
/// 在路由分流前对请求统一改写，本地与外部池看到的是同一份规范化请求。
pub fn normalize_web_search_max_uses(req: &mut MessagesRequest) -> usize {
    let mut normalized = 0;
    for tool in req.tools.iter_mut().flatten() {
        if !is_native_web_search_tool(tool) {
            continue;
        }
        if let Some(max_uses) = tool.max_uses.filter(|max_uses| *max_uses <= 0) {
            tracing::warn!(
                max_uses,
                "ignoring non-positive web_search max_uses; treating it as unset"
            );
            tool.max_uses = None;
            normalized += 1;
        }
    }
    normalized
}

fn normalize_domains(domains: Option<&[String]>) -> Vec<String> {
    let mut normalized: Vec<String> = domains
        .unwrap_or_default()
        .iter()
        .filter_map(|domain| normalize_domain(domain))
        .collect();
    normalized.sort();
    normalized.dedup();
    normalized
}

/// 把 `https://Sub.Example.com/path`、`*.example.com` 等写法统一为 `sub.example.com`。
fn normalize_domain(value: &str) -> Option<String> {
    let lower = value.trim().to_ascii_lowercase();
    let without_scheme = lower
        .split_once("://")
        .map_or(lower.as_str(), |(_, rest)| rest);
    let authority = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = host.split(':').next().unwrap_or_default();
    let host = host.trim_start_matches("*.").trim_matches('.');
    (!host.is_empty()).then(|| host.to_string())
}

fn result_host(result: &WebSearchResult) -> Option<String> {
    url::Url::parse(&result.url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .or_else(|| result.domain.as_deref().and_then(normalize_domain))
        .map(|host| host.trim_matches('.').to_string())
        .filter(|host| !host.is_empty())
}

fn host_matches_domain(host: &str, domain: &str) -> bool {
    host == domain
        || host
            .strip_suffix(domain)
            .is_some_and(|prefix| prefix.ends_with('.'))
}

/// 生成22位大小写字母和数字的随机字符串
fn generate_random_id_22() -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    (0..22)
        .map(|_| {
            let idx = fastrand::usize(..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

/// 生成8位小写字母和数字的随机字符串
fn generate_random_id_8() -> String {
    const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    (0..8)
        .map(|_| {
            let idx = fastrand::usize(..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

/// 创建 MCP 请求
///
/// ID 格式: web_search_tooluse_{22位随机}_{毫秒时间戳}_{8位随机}
pub fn create_mcp_request(query: &str) -> (String, McpRequest) {
    let random_22 = generate_random_id_22();
    let timestamp = chrono::Utc::now().timestamp_millis();
    let random_8 = generate_random_id_8();

    let request_id = format!(
        "web_search_tooluse_{}_{}_{}",
        random_22, timestamp, random_8
    );

    // tool_use_id 使用相同格式
    let tool_use_id = format!("srvtoolu_{}", Uuid::new_v4().simple());

    let request = McpRequest {
        id: request_id,
        jsonrpc: "2.0".to_string(),
        method: "tools/call".to_string(),
        params: McpParams {
            name: "web_search".to_string(),
            arguments: McpArguments {
                query: query.to_string(),
            },
        },
    };

    (tool_use_id, request)
}

/// 解析 MCP 响应中的搜索结果
pub fn parse_search_results(mcp_response: &McpResponse) -> Option<WebSearchResults> {
    if mcp_response.error.is_some() {
        return None;
    }
    let result = mcp_response.result.as_ref()?;
    if result.is_error {
        return None;
    }
    let content = result.content.first()?;

    if content.content_type != "text" {
        return None;
    }

    let parsed = serde_json::from_str::<WebSearchResults>(&content.text).ok()?;
    parsed
        .error
        .as_deref()
        .map(str::trim)
        .is_none_or(str::is_empty)
        .then_some(parsed)
}

fn parse_mcp_search_response(
    body: &str,
    expected_request_id: &str,
) -> Result<WebSearchResults, WebSearchFailure> {
    let response = serde_json::from_str::<McpResponse>(body).map_err(|_| {
        WebSearchFailure::new(
            WebSearchFailureKind::Protocol,
            "websearch_mcp_malformed_json",
        )
    })?;
    if response.jsonrpc != "2.0" || response.id != expected_request_id {
        return Err(WebSearchFailure::new(
            WebSearchFailureKind::Protocol,
            "websearch_mcp_invalid_envelope",
        ));
    }
    if response.error.is_some() {
        if response
            .error
            .as_ref()
            .and_then(|error| error.get("code"))
            .and_then(Value::as_i64)
            == Some(-32602)
        {
            return Err(WebSearchFailure::new(
                WebSearchFailureKind::InvalidRequest,
                "websearch_mcp_invalid_tool_input",
            ));
        }
        return Err(WebSearchFailure::new(
            WebSearchFailureKind::Upstream,
            "websearch_mcp_rpc_error",
        ));
    }
    let result = response.result.as_ref().ok_or_else(|| {
        WebSearchFailure::new(
            WebSearchFailureKind::Protocol,
            "websearch_mcp_missing_result",
        )
    })?;
    if result.is_error {
        return Err(WebSearchFailure::new(
            WebSearchFailureKind::Upstream,
            "websearch_mcp_tool_error",
        ));
    }
    parse_search_results(&response).ok_or_else(|| {
        WebSearchFailure::new(
            WebSearchFailureKind::Protocol,
            "websearch_mcp_invalid_search_result",
        )
    })
}

/// 生成 WebSearch SSE 响应流
pub fn create_websearch_sse_stream(
    model: String,
    query: String,
    tool_use_id: String,
    search_results: Option<WebSearchResults>,
    summary: String,
    input_tokens: i32,
) -> impl Stream<Item = Result<Bytes, Infallible>> {
    let events = generate_websearch_events(
        &model,
        &query,
        &tool_use_id,
        search_results,
        &summary,
        input_tokens,
    );

    stream::iter(
        events
            .into_iter()
            .map(|e| Ok(Bytes::from(e.to_sse_string()))),
    )
}

fn websearch_result_content(search_results: &Option<WebSearchResults>) -> Value {
    Value::Array(
        search_results
            .as_ref()
            .map(|results| {
                results
                    .results
                    .iter()
                    .map(|result| {
                        let page_age = result.published_date.and_then(|milliseconds| {
                            chrono::DateTime::from_timestamp_millis(milliseconds)
                                .map(|date| date.format("%B %-d, %Y").to_string())
                        });
                        json!({
                            "type": "web_search_result",
                            "title": result.title,
                            "url": result.url,
                            "encrypted_content": result.snippet.clone().unwrap_or_default(),
                            "page_age": page_age
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
    )
}

fn websearch_error_content(error_code: WebSearchToolErrorCode) -> Value {
    json!({
        "type": "web_search_tool_result_error",
        "error_code": error_code.as_str()
    })
}

fn estimated_output_tokens(summary: &str) -> i32 {
    ((summary.chars().count() as i32).saturating_add(3) / 4).max(1)
}

fn generate_websearch_tool_error_summary(
    query: &str,
    error_code: WebSearchToolErrorCode,
) -> String {
    let reason = match error_code {
        WebSearchToolErrorCode::TooManyRequests => "the search service is currently rate limited",
        WebSearchToolErrorCode::InvalidToolInput => {
            "the search request was rejected as invalid tool input"
        }
        WebSearchToolErrorCode::MaxUsesExceeded => "the search use limit was reached",
        WebSearchToolErrorCode::QueryTooLong => "the search query is too long",
        WebSearchToolErrorCode::RequestTooLarge => "the search request is too large",
        WebSearchToolErrorCode::Unavailable => "the search service is temporarily unavailable",
    };
    format!(
        "I couldn't complete the web search for \"{}\" because {}. I can continue without live search results, but the answer may not include current web evidence.",
        query, reason
    )
}

fn generate_websearch_message_with_content(
    context: &WebSearchResponseContext<'_>,
    search_content: Value,
) -> Value {
    let output_tokens = estimated_output_tokens(context.summary);
    json!({
        "id": envelope::message_id(),
        "type": "message",
        "role": "assistant",
        "model": context.model,
        "content": [
            {
                "type": "text",
                "text": format!("I'll search for \"{}\".", context.query)
            },
            {
                "id": context.tool_use_id,
                "type": "server_tool_use",
                "name": "web_search",
                "input": {"query": context.query}
            },
            {
                "type": "web_search_tool_result",
                "tool_use_id": context.tool_use_id,
                "content": search_content
            },
            {
                "type": "text",
                "text": context.summary
            }
        ],
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {
            "input_tokens": context.input_tokens.max(0),
            "output_tokens": output_tokens,
            "cache_creation_input_tokens": 0,
            "cache_read_input_tokens": 0
        }
    })
}

fn generate_websearch_message(
    model: &str,
    query: &str,
    tool_use_id: &str,
    search_results: Option<WebSearchResults>,
    summary: &str,
    input_tokens: i32,
) -> Value {
    let context = WebSearchResponseContext {
        model,
        query,
        tool_use_id,
        summary,
        input_tokens,
    };
    generate_websearch_message_with_content(&context, websearch_result_content(&search_results))
}

/// 生成 WebSearch SSE 事件序列
fn generate_websearch_events_with_content(
    context: &WebSearchResponseContext<'_>,
    search_content: Value,
) -> Vec<SseEvent> {
    let mut events = Vec::new();
    let message_id = envelope::message_id();

    // 1. message_start
    events.push(SseEvent::new(
        "message_start",
        json!({
            "type": "message_start",
            "message": {
                "id": message_id,
                "type": "message",
                "role": "assistant",
                "model": context.model,
                "content": [],
                "stop_reason": null,
                "usage": {
                    "input_tokens": context.input_tokens,
                    "output_tokens": 0,
                    "cache_creation_input_tokens": 0,
                    "cache_read_input_tokens": 0
                }
            }
        }),
    ));

    // 2. content_block_start (text - 搜索决策说明, index 0)
    let decision_text = format!("I'll search for \"{}\".", context.query);
    events.push(SseEvent::new(
        "content_block_start",
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {
                "type": "text",
                "text": ""
            }
        }),
    ));

    events.push(SseEvent::new(
        "content_block_delta",
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {
                "type": "text_delta",
                "text": decision_text
            }
        }),
    ));

    events.push(SseEvent::new(
        "content_block_stop",
        json!({
            "type": "content_block_stop",
            "index": 0
        }),
    ));

    // 3. content_block_start (server_tool_use, index 1)
    // server_tool_use 是服务端工具，input 在 content_block_start 中一次性完整发送，
    // 不像客户端 tool_use 需要通过 input_json_delta 增量传输。
    events.push(SseEvent::new(
        "content_block_start",
        json!({
            "type": "content_block_start",
            "index": 1,
            "content_block": {
                "id": context.tool_use_id,
                "type": "server_tool_use",
                "name": "web_search",
                "input": {"query": context.query}
            }
        }),
    ));

    // 4. content_block_stop (server_tool_use)
    events.push(SseEvent::new(
        "content_block_stop",
        json!({
            "type": "content_block_stop",
            "index": 1
        }),
    ));

    // 5. content_block_start (web_search_tool_result, index 2)
    events.push(SseEvent::new(
        "content_block_start",
        json!({
            "type": "content_block_start",
            "index": 2,
            "content_block": {
                "type": "web_search_tool_result",
                "tool_use_id": context.tool_use_id,
                "content": search_content
            }
        }),
    ));

    // 6. content_block_stop (web_search_tool_result)
    events.push(SseEvent::new(
        "content_block_stop",
        json!({
            "type": "content_block_stop",
            "index": 2
        }),
    ));

    // 7. content_block_start (text, index 3)
    events.push(SseEvent::new(
        "content_block_start",
        json!({
            "type": "content_block_start",
            "index": 3,
            "content_block": {
                "type": "text",
                "text": ""
            }
        }),
    ));

    // 分块发送文本
    let mut chars = context.summary.chars();
    loop {
        let text = chars.by_ref().take(100).collect::<String>();
        if text.is_empty() {
            break;
        }
        events.push(SseEvent::new(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": 3,
                "delta": {
                    "type": "text_delta",
                    "text": text
                }
            }),
        ));
    }

    // 9. content_block_stop (text)
    events.push(SseEvent::new(
        "content_block_stop",
        json!({
            "type": "content_block_stop",
            "index": 3
        }),
    ));

    // 10. message_delta
    // 官方 API 的 message_delta.delta 中没有 stop_sequence 字段
    let output_tokens = estimated_output_tokens(context.summary);
    events.push(SseEvent::new(
        "message_delta",
        json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": "end_turn"
            },
            "usage": {
                "input_tokens": context.input_tokens,
                "output_tokens": output_tokens,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0
            }
        }),
    ));

    // 11. message_stop
    events.push(SseEvent::new(
        "message_stop",
        json!({
            "type": "message_stop"
        }),
    ));

    events
}

/// 生成 WebSearch SSE 事件序列
fn generate_websearch_events(
    model: &str,
    query: &str,
    tool_use_id: &str,
    search_results: Option<WebSearchResults>,
    summary: &str,
    input_tokens: i32,
) -> Vec<SseEvent> {
    let context = WebSearchResponseContext {
        model,
        query,
        tool_use_id,
        summary,
        input_tokens,
    };
    generate_websearch_events_with_content(&context, websearch_result_content(&search_results))
}

fn create_websearch_tool_error_outcome(
    context: &WebSearchResponseContext<'_>,
    error_code: WebSearchToolErrorCode,
    internal_reason: &'static str,
    stream_response: bool,
    request_id: &str,
) -> WebSearchToolErrorOutcome {
    let output_tokens = estimated_output_tokens(context.summary);
    let search_content = websearch_error_content(error_code);
    let response = if stream_response {
        let events = generate_websearch_events_with_content(context, search_content);
        envelope::sse_builder_with_id(request_id)
            .body(Body::from_stream(stream::iter(events.into_iter().map(
                |event| Ok::<_, Infallible>(Bytes::from(event.to_sse_string())),
            ))))
            .unwrap()
    } else {
        envelope::json_response_with_id(
            StatusCode::OK,
            generate_websearch_message_with_content(context, search_content),
            request_id,
            None,
        )
    };
    WebSearchToolErrorOutcome {
        response,
        output_tokens,
        error_code: error_code.as_str(),
        internal_reason,
    }
}

/// 生成搜索结果摘要
fn generate_search_summary(query: &str, results: &Option<WebSearchResults>) -> String {
    let mut summary = format!("Here are the search results for \"{}\":\n\n", query);

    if let Some(results) = results {
        if results.results.is_empty() {
            summary.push_str("No results found.\n");
        }
        for (i, result) in results.results.iter().enumerate() {
            summary.push_str(&format!("{}. **{}**\n", i + 1, result.title));
            if let Some(ref snippet) = result.snippet {
                // 截断过长的摘要（安全处理 UTF-8 多字节字符）
                let truncated = match snippet.char_indices().nth(200) {
                    Some((idx, _)) => format!("{}...", &snippet[..idx]),
                    None => snippet.clone(),
                };
                summary.push_str(&format!("   {}\n", truncated));
            }
            summary.push_str(&format!("   Source: {}\n\n", result.url));
        }
    } else {
        summary.push_str("No results found.\n");
    }

    summary.push_str("\nPlease note that these are web search results and may not be fully accurate or up-to-date.");

    summary
}

/// 处理 WebSearch 请求
pub async fn handle_websearch_request(context: WebSearchRequestContext<'_>) -> WebSearchOutcome {
    let payload = context.payload;
    let request_id = context.request_id;
    let error_id = context.error_id;
    let input_tokens = context.input_tokens;

    // 1. 提取搜索查询
    let query = match extract_search_query(payload) {
        Some(q) => q,
        None => {
            return WebSearchFailure::new(
                WebSearchFailureKind::InvalidRequest,
                "websearch_missing_user_query",
            )
            .into_outcome(request_id, error_id);
        }
    };

    // Kiro MCP 对过长 query 返回 invalid_tool_input，先截断到安全长度。
    let original_query_chars = query.chars().count();
    let query = truncate_search_query(&query, MAX_WEB_SEARCH_QUERY_CHARS);
    if query.chars().count() < original_query_chars {
        tracing::warn!(
            request_id,
            original_query_chars,
            truncated_query_chars = query.chars().count(),
            max_query_chars = MAX_WEB_SEARCH_QUERY_CHARS,
            "native WebSearch query truncated to upstream-safe length"
        );
    }

    tracing::info!(
        request_id,
        query_bytes = query.len(),
        "handling native WebSearch request"
    );

    // 2. 创建 MCP 请求（allowed_domains 以 site: 提示辅助加入 query）
    let domain_filter = WebSearchDomainFilter::from_request(payload);
    let (tool_use_id, mcp_request) = create_mcp_request(&domain_filter.mcp_query(&query));

    // 3. 调用 Kiro MCP API
    let (mut search_results, attribution) = match call_mcp_api(
        &context.provider,
        &mcp_request,
        context.inference_attempt_budget,
        context.attribution_sink,
        request_id,
    )
    .await
    {
        Ok(result) => result,
        Err(failure) => {
            tracing::warn!(
                request_id,
                failure = failure.internal_reason,
                "native WebSearch MCP request failed"
            );
            let tool_error = should_continue_after_websearch_failure(&failure).then(|| {
                let error_code = websearch_tool_error_code_for_failure(&failure);
                let summary = generate_websearch_tool_error_summary(&query, error_code);
                let response_context = WebSearchResponseContext {
                    model: &payload.model,
                    query: &query,
                    tool_use_id: &tool_use_id,
                    summary: &summary,
                    input_tokens,
                };
                create_websearch_tool_error_outcome(
                    &response_context,
                    error_code,
                    failure.internal_reason,
                    payload.stream,
                    request_id,
                )
            });
            return failure.into_outcome_with_tool_error(request_id, error_id, tool_error);
        }
    };

    // 4. 按 allowed_domains / blocked_domains 过滤结果（含子域名）。
    let filtered_out = domain_filter.apply(&mut search_results);
    if !domain_filter.is_empty() {
        tracing::info!(
            request_id,
            allowed_domains = domain_filter.allowed.len(),
            blocked_domains = domain_filter.blocked.len(),
            filtered_out,
            remaining = search_results.results.len(),
            "applied native WebSearch domain filters to MCP results"
        );
    }

    // 5. 按 Anthropic `stream` 语义生成响应。
    let model = payload.model.clone();
    let search_results = Some(search_results);
    let summary = generate_search_summary(&query, &search_results);
    let output_tokens = estimated_output_tokens(&summary);
    let response = if payload.stream {
        let stream = create_websearch_sse_stream(
            model,
            query,
            tool_use_id,
            search_results,
            summary,
            input_tokens,
        );
        envelope::sse_builder_with_id(request_id)
            .body(Body::from_stream(stream))
            .unwrap()
    } else {
        envelope::json_response_with_id(
            StatusCode::OK,
            generate_websearch_message(
                &model,
                &query,
                &tool_use_id,
                search_results,
                &summary,
                input_tokens,
            ),
            request_id,
            None,
        )
    };
    WebSearchOutcome::Success {
        response,
        output_tokens,
        attribution,
    }
}

/// 调用 Kiro MCP API
async fn call_mcp_api(
    provider: &crate::kiro::provider::KiroProvider,
    request: &McpRequest,
    inference_attempt_budget: Arc<InferenceAttemptBudget>,
    attribution_sink: Arc<McpCallAttributionSink>,
    request_id: &str,
) -> Result<(WebSearchResults, McpCallAttribution), WebSearchFailure> {
    let request_body = serde_json::to_string(request).map_err(|_| {
        WebSearchFailure::new(
            WebSearchFailureKind::InvalidRequest,
            "websearch_mcp_request_serialization",
        )
    })?;
    let response = provider
        .call_mcp_with_attribution_sink(
            &request_body,
            inference_attempt_budget,
            attribution_sink,
            Some(request_id),
        )
        .await
        .map_err(|error| WebSearchFailure::from_provider_error(&error))?;
    let (response, completion) = response.into_parts();

    let body = match response_bytes_with_limit_and_body_timeout(
        response,
        provider
            .runtime_config()
            .kiro_upstream_response_timeout_secs,
        MAX_MCP_RESPONSE_BYTES,
    )
    .await
    {
        Ok(body) => body,
        Err(error) => {
            let (completion_kind, failure) = match error {
                HttpSendError::ResponseBodyTimeout { .. }
                | HttpSendError::ResponseHeaderTimeout { .. } => (
                    McpCallFailureKind::Timeout,
                    WebSearchFailure::new(
                        WebSearchFailureKind::Timeout,
                        "websearch_mcp_body_timeout",
                    ),
                ),
                HttpSendError::ResponseBodyTooLarge { .. } => (
                    McpCallFailureKind::ResponseTooLarge,
                    WebSearchFailure::new(
                        WebSearchFailureKind::Protocol,
                        "websearch_mcp_response_too_large",
                    ),
                ),
                HttpSendError::Request(_) => (
                    McpCallFailureKind::BodyRead,
                    WebSearchFailure::new(
                        WebSearchFailureKind::Upstream,
                        "websearch_mcp_body_read",
                    ),
                ),
            };
            completion.report_failure(completion_kind);
            return Err(failure.with_attribution(completion.attribution()));
        }
    };
    let body = match std::str::from_utf8(&body) {
        Ok(body) => body,
        Err(_) => {
            completion.report_failure(McpCallFailureKind::Protocol);
            return Err(WebSearchFailure::new(
                WebSearchFailureKind::Protocol,
                "websearch_mcp_non_utf8_response",
            )
            .with_attribution(completion.attribution()));
        }
    };
    match parse_mcp_search_response(body, &request.id) {
        Ok(results) => {
            completion.report_success();
            Ok((results, completion.attribution()))
        }
        Err(failure) => {
            let completion_kind = match failure.kind {
                WebSearchFailureKind::Scheduler => McpCallFailureKind::Scheduler,
                WebSearchFailureKind::InvalidRequest => McpCallFailureKind::InvalidRequest,
                WebSearchFailureKind::RateLimit => McpCallFailureKind::RateLimit,
                WebSearchFailureKind::Timeout => McpCallFailureKind::Timeout,
                WebSearchFailureKind::AttemptLimit => McpCallFailureKind::AttemptLimit,
                WebSearchFailureKind::Upstream => McpCallFailureKind::Upstream,
                WebSearchFailureKind::Protocol => McpCallFailureKind::Protocol,
            };
            completion.report_failure(completion_kind);
            Err(failure.with_attribution(completion.attribution()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::types::{Message, Tool};
    use axum::body::to_bytes;

    fn request_with(
        messages: Vec<Message>,
        tool_type: Option<&str>,
        stream: bool,
    ) -> MessagesRequest {
        MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages,
            stream,
            system: None,
            tools: Some(vec![Tool {
                tool_type: tool_type.map(str::to_string),
                name: "web_search".to_string(),
                description: String::new(),
                input_schema: Default::default(),
                max_uses: Some(8),
                cache_control: None,
                allowed_domains: None,
                blocked_domains: None,
            }]),
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
            stop_sequences: None,
        }
    }

    #[test]
    fn test_has_web_search_tool_only_one() {
        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![Message {
                role: "user".to_string(),
                content: serde_json::json!("test"),
            }],
            stream: true,
            system: None,
            tools: Some(vec![Tool {
                tool_type: Some("web_search_20250305".to_string()),
                name: "web_search".to_string(),
                description: String::new(),
                input_schema: Default::default(),
                max_uses: Some(8),
                cache_control: None,
                allowed_domains: None,
                blocked_domains: None,
            }]),
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
            stop_sequences: None,
        };

        assert!(has_web_search_tool(&req));
        assert!(has_native_web_search_tool(&req));
    }

    fn domain_request(allowed: Option<&[&str]>, blocked: Option<&[&str]>) -> MessagesRequest {
        let mut req = request_with(
            vec![Message {
                role: "user".to_string(),
                content: json!("query"),
            }],
            Some("web_search_20250305"),
            false,
        );
        let tool = &mut req.tools.as_mut().expect("tools")[0];
        let to_vec = |domains: &[&str]| domains.iter().map(|d| d.to_string()).collect();
        tool.allowed_domains = allowed.map(to_vec);
        tool.blocked_domains = blocked.map(to_vec);
        req
    }

    fn search_result(url: &str, domain: Option<&str>) -> WebSearchResult {
        WebSearchResult {
            title: url.to_string(),
            url: url.to_string(),
            snippet: None,
            published_date: None,
            id: None,
            domain: domain.map(str::to_string),
            max_verbatim_word_limit: None,
            public_domain: None,
        }
    }

    fn results_of(urls: &[&str]) -> WebSearchResults {
        WebSearchResults {
            results: urls.iter().map(|url| search_result(url, None)).collect(),
            total_results: None,
            query: None,
            error: None,
        }
    }

    #[test]
    fn allowed_domains_keep_matching_hosts_and_subdomains_only() {
        let req = domain_request(Some(&["https://Example.com/docs", "*.rust-lang.org"]), None);
        let filter = WebSearchDomainFilter::from_request(&req);
        let mut results = results_of(&[
            "https://example.com/a",
            "https://docs.example.com/b",
            "https://notexample.com/c",
            "https://doc.rust-lang.org/std",
            "https://evil.com/?q=example.com",
            "not a url",
        ]);

        assert_eq!(filter.apply(&mut results), 3);
        let urls: Vec<&str> = results.results.iter().map(|r| r.url.as_str()).collect();
        assert_eq!(
            urls,
            vec![
                "https://example.com/a",
                "https://docs.example.com/b",
                "https://doc.rust-lang.org/std"
            ]
        );
        assert_eq!(
            filter.mcp_query("rust news"),
            "rust news site:example.com OR site:rust-lang.org"
        );
    }

    #[test]
    fn blocked_domains_remove_matching_hosts_and_subdomains() {
        let req = domain_request(None, Some(&["reddit.com"]));
        let filter = WebSearchDomainFilter::from_request(&req);
        let mut results = results_of(&[
            "https://www.reddit.com/r/rust",
            "https://reddit.com/",
            "https://notreddit.com/",
        ]);
        results
            .results
            .push(search_result("", Some("old.reddit.com")));

        assert_eq!(filter.apply(&mut results), 3);
        assert_eq!(results.results.len(), 1);
        assert_eq!(results.results[0].url, "https://notreddit.com/");
        // blocked 不改写 query。
        assert_eq!(filter.mcp_query("rust"), "rust");
    }

    #[test]
    fn allowed_and_blocked_domains_both_apply_when_declared_together() {
        let req = domain_request(Some(&["example.com"]), Some(&["ads.example.com"]));
        let filter = WebSearchDomainFilter::from_request(&req);
        let mut results = results_of(&[
            "https://example.com/a",
            "https://docs.example.com/b",
            "https://ads.example.com/c",
            "https://x.ads.example.com/d",
            "https://other.com/e",
        ]);

        assert_eq!(filter.apply(&mut results), 3);
        let urls: Vec<&str> = results.results.iter().map(|r| r.url.as_str()).collect();
        assert_eq!(
            urls,
            vec!["https://example.com/a", "https://docs.example.com/b"]
        );
        assert_eq!(filter.mcp_query("q"), "q site:example.com");
    }

    #[test]
    fn non_positive_max_uses_is_treated_as_unset() {
        for max_uses in [0, -3] {
            let mut req = domain_request(None, None);
            req.tools.as_mut().expect("tools")[0].max_uses = Some(max_uses);

            assert_eq!(normalize_web_search_max_uses(&mut req), 1);
            assert_eq!(req.tools.as_ref().expect("tools")[0].max_uses, None);
            assert!(has_web_search_tool(&req));
        }

        let mut req = domain_request(None, None);
        req.tools.as_mut().expect("tools")[0].max_uses = Some(5);
        assert_eq!(normalize_web_search_max_uses(&mut req), 0);
        assert_eq!(req.tools.as_ref().expect("tools")[0].max_uses, Some(5));
    }

    #[test]
    fn empty_domain_filters_are_a_no_op() {
        let filter = WebSearchDomainFilter::from_request(&domain_request(None, None));
        assert!(filter.is_empty());
        let mut results = results_of(&["https://x.com"]);
        assert_eq!(filter.apply(&mut results), 0);
        assert_eq!(filter.mcp_query("q"), "q");
    }

    #[test]
    fn long_search_query_is_truncated_on_word_boundary() {
        let short = "tokio spawn_blocking";
        assert_eq!(
            truncate_search_query(short, MAX_WEB_SEARCH_QUERY_CHARS),
            short
        );

        let long = format!(
            "tokio spawn_blocking thread pool{}",
            " with detailed explanation of blocking tasks".repeat(12)
        );
        assert!(long.chars().count() > 500);
        let truncated = truncate_search_query(&long, MAX_WEB_SEARCH_QUERY_CHARS);
        assert!(truncated.chars().count() <= MAX_WEB_SEARCH_QUERY_CHARS);
        assert!(long.starts_with(&truncated));
        // 截断点之后紧跟空白，说明没有切断单词。
        assert!(long[truncated.len()..].starts_with(' '));
        assert!(!truncated.ends_with(' '));
    }

    #[test]
    fn long_query_without_spaces_is_truncated_by_chars() {
        let long = "中文长查询".repeat(60);
        let truncated = truncate_search_query(&long, MAX_WEB_SEARCH_QUERY_CHARS);
        assert_eq!(truncated.chars().count(), MAX_WEB_SEARCH_QUERY_CHARS);
        assert!(long.starts_with(&truncated));
    }

    #[test]
    fn site_hint_is_skipped_when_it_would_exceed_query_limit() {
        let req = domain_request(Some(&["example.com"]), None);
        let filter = WebSearchDomainFilter::from_request(&req);
        let query = "q".repeat(MAX_WEB_SEARCH_QUERY_CHARS - 5);
        assert_eq!(filter.mcp_query(&query), query);
    }

    #[test]
    fn many_allowed_domains_skip_site_hint_but_still_filter() {
        let req = domain_request(Some(&["a.com", "b.com", "c.com", "d.com"]), None);
        let filter = WebSearchDomainFilter::from_request(&req);
        assert_eq!(filter.mcp_query("q"), "q");
        let mut results = results_of(&["https://d.com/x", "https://e.com/y"]);
        assert_eq!(filter.apply(&mut results), 1);
    }

    #[test]
    fn native_websearch_detection_accepts_current_official_versions() {
        for tool_type in [
            "web_search_20250305",
            "web_search_20260209",
            "web_search_20260318",
        ] {
            let req = request_with(
                vec![Message {
                    role: "user".to_string(),
                    content: json!("query"),
                }],
                Some(tool_type),
                true,
            );
            assert!(has_web_search_tool(&req), "{tool_type}");
            assert!(has_native_web_search_tool(&req), "{tool_type}");
            assert!(!has_unlisted_native_web_search_tool_type(&req));
        }
    }

    #[test]
    fn native_websearch_detection_accepts_future_version_format_as_basic_websearch() {
        let req = request_with(
            vec![Message {
                role: "user".to_string(),
                content: json!("query"),
            }],
            Some("web_search_20270101"),
            true,
        );
        assert!(has_web_search_tool(&req));
        assert!(has_native_web_search_tool(&req));
        assert!(has_unlisted_native_web_search_tool_type(&req));
    }

    #[test]
    fn native_websearch_detection_never_hijacks_same_named_custom_tool_for_five_rounds() {
        for _ in 0..5 {
            let custom = request_with(
                vec![Message {
                    role: "user".to_string(),
                    content: json!("query"),
                }],
                None,
                true,
            );
            let native = request_with(
                vec![Message {
                    role: "user".to_string(),
                    content: json!("query"),
                }],
                Some("web_search_20250305"),
                true,
            );

            assert!(!has_web_search_tool(&custom));
            assert!(!has_native_web_search_tool(&custom));
            assert!(has_web_search_tool(&native));
            assert!(has_native_web_search_tool(&native));
        }
    }

    #[test]
    fn test_has_web_search_tool_multiple_tools() {
        use crate::anthropic::types::{Message, Tool};

        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![Message {
                role: "user".to_string(),
                content: serde_json::json!("test"),
            }],
            stream: true,
            system: None,
            tools: Some(vec![
                Tool {
                    tool_type: Some("web_search_20250305".to_string()),
                    name: "web_search".to_string(),
                    description: String::new(),
                    input_schema: Default::default(),
                    max_uses: Some(8),
                    cache_control: None,
                    allowed_domains: None,
                    blocked_domains: None,
                },
                Tool {
                    tool_type: None,
                    name: "other_tool".to_string(),
                    description: "Other tool".to_string(),
                    input_schema: Default::default(),
                    max_uses: None,
                    cache_control: None,
                    allowed_domains: None,
                    blocked_domains: None,
                },
            ]),
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
            stop_sequences: None,
        };

        // 多个工具时不应该被识别为纯 websearch 请求
        assert!(!has_web_search_tool(&req));
        assert!(has_native_web_search_tool(&req));
        assert!(has_mixed_native_web_search_tool(&req));
    }

    #[test]
    fn mixed_native_websearch_detection_never_hijacks_same_named_custom_tool() {
        let custom_with_peer = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![Message {
                role: "user".to_string(),
                content: serde_json::json!("test"),
            }],
            stream: true,
            system: None,
            tools: Some(vec![
                Tool {
                    tool_type: None,
                    name: "web_search".to_string(),
                    description: "Client-side same-name custom tool".to_string(),
                    input_schema: Default::default(),
                    max_uses: None,
                    cache_control: None,
                    allowed_domains: None,
                    blocked_domains: None,
                },
                Tool {
                    tool_type: None,
                    name: "fixture".to_string(),
                    description: "Other tool".to_string(),
                    input_schema: Default::default(),
                    max_uses: None,
                    cache_control: None,
                    allowed_domains: None,
                    blocked_domains: None,
                },
            ]),
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
            stop_sequences: None,
        };

        assert!(!has_web_search_tool(&custom_with_peer));
        assert!(!has_native_web_search_tool(&custom_with_peer));
        assert!(!has_mixed_native_web_search_tool(&custom_with_peer));
    }

    #[test]
    fn test_extract_search_query_with_prefix() {
        use crate::anthropic::types::Message;

        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![Message {
                role: "user".to_string(),
                content: serde_json::json!([{
                    "type": "text",
                    "text": "Perform a web search for the query: rust latest version 2026"
                }]),
            }],
            stream: true,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
            stop_sequences: None,
        };

        let query = extract_search_query(&req);
        // 前缀应该被去除
        assert_eq!(query, Some("rust latest version 2026".to_string()));
    }

    #[test]
    fn test_extract_search_query_plain_text() {
        use crate::anthropic::types::Message;

        let req = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages: vec![Message {
                role: "user".to_string(),
                content: serde_json::json!("What is the weather today?"),
            }],
            stream: true,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
            stop_sequences: None,
        };

        let query = extract_search_query(&req);
        assert_eq!(query, Some("What is the weather today?".to_string()));
    }

    #[test]
    fn extracts_last_valid_user_text_instead_of_first_message_for_five_rounds() {
        for _ in 0..5 {
            let req = request_with(
                vec![
                    Message {
                        role: "user".to_string(),
                        content: json!("stale query"),
                    },
                    Message {
                        role: "assistant".to_string(),
                        content: json!("assistant text must not become the query"),
                    },
                    Message {
                        role: "user".to_string(),
                        content: json!([
                            {"type": "tool_result", "tool_use_id": "tool-1", "content": "ignored"},
                            {"type": "text", "text": "  Perform a web search for the query: current query  "}
                        ]),
                    },
                    Message {
                        role: "assistant".to_string(),
                        content: json!("trailing assistant text"),
                    },
                ],
                Some("web_search_20250305"),
                true,
            );

            assert_eq!(extract_search_query(&req).as_deref(), Some("current query"));
        }
    }

    #[test]
    fn latest_user_turn_without_text_never_reuses_a_stale_query_for_five_rounds() {
        for round in 1..=5 {
            let req = request_with(
                vec![
                    Message {
                        role: "user".to_string(),
                        content: json!(format!("stale-query-{round}")),
                    },
                    Message {
                        role: "assistant".to_string(),
                        content: json!("old answer"),
                    },
                    Message {
                        role: "user".to_string(),
                        content: json!([{
                            "type": "tool_result",
                            "tool_use_id": format!("tool-{round}"),
                            "content": "current turn has no search query"
                        }]),
                    },
                ],
                Some("web_search_20250305"),
                round % 2 == 0,
            );

            assert_eq!(
                extract_search_query(&req),
                None,
                "round {round}: current user turn without text must not fall back to stale history"
            );
        }
    }

    #[test]
    fn extracts_current_query_after_twenty_and_one_hundred_tool_cycles_for_five_rounds() {
        for tool_cycles in [20, 100] {
            for round in 1..=5 {
                let mut messages = Vec::with_capacity(tool_cycles * 2 + 2);
                messages.push(Message {
                    role: "user".to_string(),
                    content: json!(format!("stale-first-query-{round}")),
                });
                for cycle in 0..tool_cycles {
                    messages.push(Message {
                        role: "assistant".to_string(),
                        content: json!([{
                            "type": "tool_use",
                            "id": format!("tool-{cycle}"),
                            "name": "fixture",
                            "input": {"cycle": cycle}
                        }]),
                    });
                    messages.push(Message {
                        role: "user".to_string(),
                        content: json!([{
                            "type": "tool_result",
                            "tool_use_id": format!("tool-{cycle}"),
                            "content": format!("result-{cycle}")
                        }]),
                    });
                }
                let expected = format!("current-query-{tool_cycles}-{round}");
                messages.push(Message {
                    role: "user".to_string(),
                    content: json!([{
                        "type": "text",
                        "text": format!("{WEB_SEARCH_QUERY_PREFIX} {expected}")
                    }]),
                });

                let request = request_with(messages, Some("web_search_20250305"), true);
                assert_eq!(
                    extract_search_query(&request).as_deref(),
                    Some(expected.as_str())
                );
            }
        }
    }

    #[test]
    fn mcp_error_and_malformed_payloads_are_not_empty_search_success_for_five_rounds() {
        let payloads = [
            r#"{"jsonrpc":"2.0","id":"x","result":{"content":[],"isError":true}}"#,
            r#"{"jsonrpc":"2.0","id":"x","result":{"content":[{"type":"text","text":"not-json"}],"isError":false}}"#,
            r#"{"jsonrpc":"2.0","id":"x","error":{"code":-32000,"message":"private upstream detail"}}"#,
            r#"{"jsonrpc":"2.0","id":"x","result":null}"#,
            "not-json",
        ];

        for _ in 0..5 {
            for payload in payloads {
                assert!(
                    parse_mcp_search_response(payload, "x").is_err(),
                    "payload must not be converted into an empty successful result: {payload}"
                );
            }
        }
    }

    #[test]
    fn mcp_response_must_match_jsonrpc_version_and_request_id_for_five_rounds() {
        let valid_result = r#"{"results":[],"totalResults":0}"#;
        for _ in 0..5 {
            let valid = json!({
                "jsonrpc": "2.0",
                "id": "expected",
                "result": {
                    "content": [{"type": "text", "text": valid_result}],
                    "isError": false
                }
            })
            .to_string();
            assert!(parse_mcp_search_response(&valid, "expected").is_ok());

            for invalid in [
                valid.replace("\"id\":\"expected\"", "\"id\":\"other\""),
                valid.replace("\"jsonrpc\":\"2.0\"", "\"jsonrpc\":\"1.0\""),
                valid.replace("\"id\":\"expected\"", "\"id\":\"\""),
            ] {
                let failure = parse_mcp_search_response(&invalid, "expected")
                    .expect_err("mismatched JSON-RPC envelope must fail closed");
                assert_eq!(failure.kind, WebSearchFailureKind::Protocol);
                assert_eq!(failure.internal_reason, "websearch_mcp_invalid_envelope");
            }
        }
    }

    #[test]
    fn mcp_invalid_params_maps_to_invalid_tool_input_for_five_rounds() {
        for _ in 0..5 {
            let payload = json!({
                "jsonrpc": "2.0",
                "id": "expected",
                "error": {
                    "code": -32602,
                    "message": "private upstream invalid params detail"
                }
            })
            .to_string();
            let failure = parse_mcp_search_response(&payload, "expected")
                .expect_err("MCP invalid params must not parse as success");
            assert_eq!(failure.kind, WebSearchFailureKind::InvalidRequest);
            assert_eq!(failure.internal_reason, "websearch_mcp_invalid_tool_input");
            assert_eq!(
                websearch_tool_error_code_for_failure(&failure),
                WebSearchToolErrorCode::InvalidToolInput
            );
        }
    }

    #[test]
    fn only_recoverable_websearch_failures_emit_tool_errors_for_five_rounds() {
        let recoverable = [
            "websearch_mcp_invalid_request",
            "websearch_mcp_invalid_tool_input",
            "websearch_mcp_rate_limit",
            "websearch_mcp_timeout",
            "websearch_mcp_body_timeout",
            "websearch_mcp_body_read",
            "websearch_mcp_upstream_error",
            "websearch_mcp_rpc_error",
            "websearch_mcp_tool_error",
            "websearch_mcp_scheduler_unavailable",
            "websearch_mcp_attempt_limit",
        ];
        let hard_failures = [
            "websearch_mcp_malformed_json",
            "websearch_mcp_invalid_envelope",
            "websearch_mcp_missing_result",
            "websearch_mcp_non_utf8_response",
            "websearch_mcp_response_too_large",
            "websearch_mcp_invalid_search_result",
            "websearch_mcp_protocol_error",
        ];
        for _ in 0..5 {
            for reason in recoverable {
                let failure = WebSearchFailure::new(WebSearchFailureKind::Upstream, reason);
                assert!(
                    should_continue_after_websearch_failure(&failure),
                    "{reason}"
                );
            }
            for reason in hard_failures {
                let failure = WebSearchFailure::new(WebSearchFailureKind::Protocol, reason);
                assert!(
                    !should_continue_after_websearch_failure(&failure),
                    "{reason}"
                );
            }
        }
    }

    #[test]
    fn auxiliary_focus_provider_typed_failures_ignore_misleading_error_text_for_five_rounds() {
        let cases = [
            (
                McpCallFailureKind::Scheduler,
                "private-response contains scheduler internals",
                WebSearchFailureKind::Scheduler,
            ),
            (
                McpCallFailureKind::InvalidRequest,
                "private-response falsely says 429 timeout",
                WebSearchFailureKind::InvalidRequest,
            ),
            (
                McpCallFailureKind::RateLimit,
                "private-response falsely says 400 timeout",
                WebSearchFailureKind::RateLimit,
            ),
            (
                McpCallFailureKind::Upstream,
                "private-response falsely says 429 timeout 400",
                WebSearchFailureKind::Upstream,
            ),
            (
                McpCallFailureKind::Timeout,
                "private-response falsely says 400 and 429",
                WebSearchFailureKind::Timeout,
            ),
            (
                McpCallFailureKind::ResponseTooLarge,
                "private-response falsely says valid small body",
                WebSearchFailureKind::Protocol,
            ),
            (
                McpCallFailureKind::BodyRead,
                "private-response falsely says invalid request",
                WebSearchFailureKind::Upstream,
            ),
            (
                McpCallFailureKind::Protocol,
                "private-response contains raw search result",
                WebSearchFailureKind::Protocol,
            ),
            (
                McpCallFailureKind::AttemptLimit,
                "private-response falsely says success",
                WebSearchFailureKind::AttemptLimit,
            ),
            (
                McpCallFailureKind::AuxiliaryAttemptLimit,
                "private-response contains auxiliary budget details",
                WebSearchFailureKind::AttemptLimit,
            ),
            (
                McpCallFailureKind::AuxiliaryConcurrency,
                "private-response contains auxiliary concurrency details",
                WebSearchFailureKind::AttemptLimit,
            ),
        ];

        for _ in 0..5 {
            for (kind, message, expected) in cases {
                let error = crate::kiro::provider::KiroProvider::mcp_failure_error(kind, message);
                let failure = WebSearchFailure::from_provider_error(&error);
                assert_eq!(failure.kind, expected);
                assert!(!failure.internal_reason.contains("private-response"));
            }
        }
    }

    #[tokio::test]
    async fn public_failures_are_normalized_and_redacted_for_five_rounds() {
        let cases = [
            (
                WebSearchFailureKind::Scheduler,
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                WebSearchFailureKind::InvalidRequest,
                StatusCode::BAD_REQUEST,
            ),
            (
                WebSearchFailureKind::RateLimit,
                StatusCode::TOO_MANY_REQUESTS,
            ),
            (WebSearchFailureKind::Timeout, StatusCode::GATEWAY_TIMEOUT),
            (
                WebSearchFailureKind::AttemptLimit,
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (WebSearchFailureKind::Upstream, StatusCode::BAD_GATEWAY),
            (WebSearchFailureKind::Protocol, StatusCode::BAD_GATEWAY),
        ];

        for round in 0..5 {
            for (kind, expected_status) in cases {
                let outcome = WebSearchFailure::new(kind, "fixed_internal_reason")
                    .into_outcome("req_websearch_test", "err_websearch_test");
                let WebSearchOutcome::Failure { response, .. } = outcome else {
                    panic!("round {round}: failures must not become success");
                };
                assert_eq!(response.status(), expected_status, "round {round}");
                assert_eq!(
                    response
                        .headers()
                        .get("request-id")
                        .and_then(|value| value.to_str().ok()),
                    Some("req_websearch_test")
                );
                let body = to_bytes(response.into_body(), 64 * 1024)
                    .await
                    .expect("read normalized error body");
                let body = String::from_utf8(body.to_vec()).expect("utf8 error body");
                assert!(body.contains("err_websearch_test"));
                assert!(!body.contains("fixed_internal_reason"));
                assert!(!body.contains("private-response"));
            }
        }
    }

    #[test]
    fn stream_and_non_stream_success_shapes_match_anthropic_protocol_for_five_rounds() {
        for _ in 0..5 {
            let results = Some(WebSearchResults {
                results: vec![],
                total_results: Some(0),
                query: None,
                error: None,
            });
            let summary = generate_search_summary("query", &results);
            let non_stream = generate_websearch_message(
                "test-model",
                "query",
                "tool-1",
                results.clone(),
                &summary,
                123,
            );
            assert_eq!(non_stream["type"], "message");
            assert_eq!(non_stream["role"], "assistant");
            assert_eq!(non_stream["stop_reason"], "end_turn");
            assert!(
                non_stream["content"]
                    .as_array()
                    .is_some_and(|blocks| !blocks.is_empty())
            );
            assert!(
                non_stream["usage"]["output_tokens"]
                    .as_i64()
                    .unwrap_or_default()
                    > 0
            );

            let events =
                generate_websearch_events("test-model", "query", "tool-1", results, &summary, 123);
            assert_eq!(
                events.first().map(|event| event.event.as_str()),
                Some("message_start")
            );
            assert_eq!(
                events.last().map(|event| event.event.as_str()),
                Some("message_stop")
            );

            let content = non_stream["content"]
                .as_array()
                .expect("non-stream content blocks");
            let decision_text = events
                .iter()
                .find(|event| event.event == "content_block_delta" && event.data["index"] == 0)
                .and_then(|event| event.data["delta"]["text"].as_str())
                .expect("stream decision text");
            assert_eq!(content[0]["text"], decision_text);

            for (content_index, stream_index) in [(1, 1), (2, 2)] {
                let streamed_block = events
                    .iter()
                    .find(|event| {
                        event.event == "content_block_start" && event.data["index"] == stream_index
                    })
                    .map(|event| &event.data["content_block"])
                    .expect("stream content block");
                assert_eq!(&content[content_index], streamed_block);
            }

            let streamed_summary = events
                .iter()
                .filter(|event| event.event == "content_block_delta" && event.data["index"] == 3)
                .filter_map(|event| event.data["delta"]["text"].as_str())
                .collect::<String>();
            assert_eq!(content[3]["text"], streamed_summary);

            let final_usage = events
                .iter()
                .find(|event| event.event == "message_delta")
                .map(|event| &event.data["usage"])
                .expect("stream final usage");
            assert_eq!(&non_stream["usage"], final_usage);
        }
    }

    #[test]
    fn stream_and_non_stream_tool_error_shapes_match_anthropic_protocol_for_five_rounds() {
        for _ in 0..5 {
            let search_content = websearch_error_content(WebSearchToolErrorCode::InvalidToolInput);
            let summary = generate_websearch_tool_error_summary(
                "query",
                WebSearchToolErrorCode::InvalidToolInput,
            );
            let context = WebSearchResponseContext {
                model: "test-model",
                query: "query",
                tool_use_id: "tool-1",
                summary: &summary,
                input_tokens: 123,
            };
            let non_stream =
                generate_websearch_message_with_content(&context, search_content.clone());
            let blocks = non_stream["content"]
                .as_array()
                .expect("tool-error content blocks");
            let server_tool = blocks
                .iter()
                .find(|block| block["type"] == "server_tool_use")
                .expect("server tool block");
            let result = blocks
                .iter()
                .find(|block| block["type"] == "web_search_tool_result")
                .expect("tool result block");
            assert_eq!(server_tool["id"], "tool-1");
            assert_eq!(result["tool_use_id"], "tool-1");
            assert_eq!(
                result["content"],
                json!({
                    "type": "web_search_tool_result_error",
                    "error_code": "invalid_tool_input"
                })
            );
            assert_eq!(non_stream["stop_reason"], "end_turn");
            assert!(
                non_stream["usage"]["output_tokens"]
                    .as_i64()
                    .unwrap_or_default()
                    > 0
            );

            let events = generate_websearch_events_with_content(&context, search_content);
            let streamed_result = events
                .iter()
                .find(|event| event.event == "content_block_start" && event.data["index"] == 2)
                .map(|event| &event.data["content_block"])
                .expect("streamed tool result block");
            assert_eq!(streamed_result, result);
            assert_eq!(
                events.last().map(|event| event.event.as_str()),
                Some("message_stop")
            );
        }
    }

    #[test]
    fn test_create_mcp_request() {
        let (tool_use_id, request) = create_mcp_request("test query");

        assert!(tool_use_id.starts_with("srvtoolu_"));
        assert_eq!(request.jsonrpc, "2.0");
        assert_eq!(request.method, "tools/call");
        assert_eq!(request.params.name, "web_search");
        assert_eq!(request.params.arguments.query, "test query");

        // 验证 ID 格式: web_search_tooluse_{22位}_{时间戳}_{8位}
        assert!(request.id.starts_with("web_search_tooluse_"));
    }

    #[test]
    fn test_mcp_request_id_format() {
        let (_, request) = create_mcp_request("test");

        // 格式: web_search_tooluse_{22位}_{毫秒时间戳}_{8位}
        let id = &request.id;
        assert!(id.starts_with("web_search_tooluse_"));

        let suffix = &id["web_search_tooluse_".len()..];
        let parts: Vec<&str> = suffix.split('_').collect();
        assert_eq!(parts.len(), 3, "应该有3个部分: 22位随机_时间戳_8位随机");

        // 第一部分: 22位大小写字母和数字
        assert_eq!(parts[0].len(), 22);
        assert!(parts[0].chars().all(|c| c.is_ascii_alphanumeric()));

        // 第二部分: 毫秒时间戳
        assert!(parts[1].parse::<i64>().is_ok());

        // 第三部分: 8位小写字母和数字
        assert_eq!(parts[2].len(), 8);
        assert!(
            parts[2]
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        );
    }

    #[test]
    fn test_parse_search_results() {
        let response = McpResponse {
            error: None,
            id: "test_id".to_string(),
            jsonrpc: "2.0".to_string(),
            result: Some(McpResult {
                content: vec![McpContent {
                    content_type: "text".to_string(),
                    text: r#"{"results":[{"title":"Test","url":"https://example.com","snippet":"Test snippet"}],"totalResults":1}"#.to_string(),
                }],
                is_error: false,
            }),
        };

        let results = parse_search_results(&response);
        assert!(results.is_some());
        let results = results.unwrap();
        assert_eq!(results.results.len(), 1);
        assert_eq!(results.results[0].title, "Test");
    }

    #[test]
    fn test_generate_search_summary() {
        let results = WebSearchResults {
            results: vec![WebSearchResult {
                title: "Test Result".to_string(),
                url: "https://example.com".to_string(),
                snippet: Some("This is a test snippet".to_string()),
                published_date: None,
                id: None,
                domain: None,
                max_verbatim_word_limit: None,
                public_domain: None,
            }],
            total_results: Some(1),
            query: Some("test".to_string()),
            error: None,
        };

        let summary = generate_search_summary("test", &Some(results));

        assert!(summary.contains("Test Result"));
        assert!(summary.contains("https://example.com"));
        assert!(summary.contains("This is a test snippet"));
    }

    #[test]
    fn test_websearch_usage_is_sub2api_compatible() {
        let summary = generate_search_summary("test query", &None);
        let events =
            generate_websearch_events("test-model", "test query", "tool-1", None, &summary, 1234);

        let message_start = events
            .iter()
            .find(|event| event.event == "message_start")
            .expect("message_start should exist");
        let start_usage = &message_start.data["message"]["usage"];
        assert_eq!(start_usage["input_tokens"], 1234);
        assert_eq!(start_usage["output_tokens"], 0);
        assert_eq!(start_usage["cache_creation_input_tokens"], 0);
        assert_eq!(start_usage["cache_read_input_tokens"], 0);
        assert!(start_usage.get("server_tool_use").is_none());
        assert!(start_usage.get("cache_creation_5m_input_tokens").is_none());
        assert!(start_usage.get("cache_creation_1h_input_tokens").is_none());

        let message_delta = events
            .iter()
            .find(|event| event.event == "message_delta")
            .expect("message_delta should exist");
        let final_usage = &message_delta.data["usage"];
        assert_eq!(final_usage["input_tokens"], 1234);
        assert!(final_usage["output_tokens"].as_i64().unwrap_or_default() > 0);
        assert_eq!(final_usage["cache_creation_input_tokens"], 0);
        assert_eq!(final_usage["cache_read_input_tokens"], 0);
        assert!(final_usage.get("server_tool_use").is_none());
        assert!(final_usage.get("cache_creation_5m_input_tokens").is_none());
        assert!(final_usage.get("cache_creation_1h_input_tokens").is_none());
    }
}
