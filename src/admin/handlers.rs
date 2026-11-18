//! Admin API HTTP 处理器

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::Deserialize;

use super::{middleware::AdminState, types::*};
use crate::account::{CreateAccountRequest, SetAccountEnabledRequest, UpdateAccountRequest};
use crate::anthropic::usage::{
    UsageAccountRiskQuery, UsageRecordQuery, UsageRecordStatus, UsageRouteKind, UsageSource,
    usage_dashboard_timezone, usage_dashboard_window_spec_for_key,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditLogsQueryParams {
    pub page: Option<usize>,
    pub limit: Option<usize>,
    /// `true` lists only account change events.
    pub account_events: Option<bool>,
    pub account_event: Option<String>,
    pub account_source: Option<String>,
    pub object_type: Option<String>,
    pub object_id: Option<String>,
    /// Account label substring or id.
    pub account: Option<String>,
    pub action: Option<String>,
    /// Comma-separated action list; the console sends a category expanded to its actions.
    pub actions: Option<String>,
    pub actor: Option<String>,
    pub success: Option<bool>,
    /// RFC3339 lower bound (inclusive).
    pub since: Option<String>,
    /// RFC3339 upper bound (exclusive).
    pub until: Option<String>,
    /// Free-text search across action, actor, object and error message.
    pub q: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountEventSummaryQueryParams {
    /// Window in hours back from now, 1 to 720. Defaults to 24.
    pub hours: Option<u32>,
    pub account_source: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecordsQueryParams {
    pub limit: Option<usize>,
    #[serde(alias = "request_id", alias = "reqId", alias = "req_id")]
    pub request_id: Option<String>,
    pub q: Option<String>,
    pub endpoint: Option<String>,
    /// 路由前缀（默认路由传 `/`），精确匹配 `{前缀}/v1/` 开头的入口。
    pub route: Option<String>,
    pub conversation_id: Option<String>,
    pub request_api_key_id: Option<String>,
    pub credential_id: Option<u64>,
    pub account_id: Option<u64>,
    pub route_kind: Option<String>,
    pub model: Option<String>,
    pub status: Option<String>,
    pub source: Option<String>,
    pub stream: Option<bool>,
    pub min_cache_read: Option<i32>,
    pub min_first_token_latency_ms: Option<u64>,
    pub since: Option<String>,
    pub until: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecordsPageQueryParams {
    pub page: Option<usize>,
    pub limit: Option<usize>,
    #[serde(alias = "request_id", alias = "reqId", alias = "req_id")]
    pub request_id: Option<String>,
    pub q: Option<String>,
    pub endpoint: Option<String>,
    /// 路由前缀（默认路由传 `/`），精确匹配 `{前缀}/v1/` 开头的入口。
    pub route: Option<String>,
    pub conversation_id: Option<String>,
    pub request_api_key_id: Option<String>,
    pub credential_id: Option<u64>,
    pub account_id: Option<u64>,
    pub route_kind: Option<String>,
    pub model: Option<String>,
    pub status: Option<String>,
    pub source: Option<String>,
    pub stream: Option<bool>,
    pub min_cache_read: Option<i32>,
    pub min_first_token_latency_ms: Option<u64>,
    pub since: Option<String>,
    pub until: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDashboardQueryParams {
    pub timezone: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageOverviewQueryParams {
    pub timezone: Option<String>,
    pub range: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub granularity: Option<String>,
    pub top_n: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageAccountLatencyQueryParams {
    pub timezone: Option<String>,
    pub range: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub account_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountQualityHistoryQueryParams {
    /// 回看小时数，1 到 168，默认 24。
    pub hours: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDashboardWindowQueryParams {
    pub timezone: Option<String>,
    pub window_key: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageAccountRiskQueryParams {
    pub timezone: Option<String>,
    pub window_key: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub warning_threshold_tokens: Option<i64>,
    pub critical_threshold_tokens: Option<i64>,
    pub account_id: Option<u64>,
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub stream: Option<bool>,
    pub limit: Option<usize>,
}

/// GET /api/admin/system/version
pub async fn get_system_version() -> Json<SystemVersionResponse> {
    Json(SystemVersionResponse {
        version: env!("CARGO_PKG_VERSION"),
    })
}

impl UsageRecordsQueryParams {
    fn into_query(self) -> Result<UsageRecordQuery, String> {
        let mut request_id = non_blank(self.request_id);
        let mut q = non_blank(self.q);
        if request_id.is_none() {
            if let Some(extracted) = q.as_deref().and_then(extract_request_id_token) {
                request_id = Some(extracted);
                q = None;
            }
        }
        Ok(UsageRecordQuery {
            limit: self.limit.unwrap_or_default(),
            request_id,
            q,
            endpoint: non_blank(self.endpoint),
            route: non_blank(self.route),
            conversation_id: non_blank(self.conversation_id),
            request_api_key_id: parse_optional_request_api_key_id(self.request_api_key_id)?,
            credential_id: self.credential_id,
            account_id: self.account_id,
            route_kind: parse_optional_usage_route_kind(self.route_kind)?,
            model: non_blank(self.model),
            status: parse_optional_usage_status(self.status)?,
            source: parse_optional_usage_source(self.source)?,
            stream: self.stream,
            min_cache_read: self.min_cache_read,
            min_first_token_latency_ms: self.min_first_token_latency_ms,
            since: parse_optional_time(self.since)?,
            until: parse_optional_time(self.until)?,
        })
    }
}

impl UsageRecordsPageQueryParams {
    fn into_query(self) -> Result<(UsageRecordQuery, usize, usize), String> {
        let page = self.page.unwrap_or_default();
        let limit = self.limit.unwrap_or_default();
        let mut request_id = non_blank(self.request_id);
        let mut q = non_blank(self.q);
        if request_id.is_none() {
            if let Some(extracted) = q.as_deref().and_then(extract_request_id_token) {
                request_id = Some(extracted);
                q = None;
            }
        }
        let query = UsageRecordQuery {
            limit,
            request_id,
            q,
            endpoint: non_blank(self.endpoint),
            route: non_blank(self.route),
            conversation_id: non_blank(self.conversation_id),
            request_api_key_id: parse_optional_request_api_key_id(self.request_api_key_id)?,
            credential_id: self.credential_id,
            account_id: self.account_id,
            route_kind: parse_optional_usage_route_kind(self.route_kind)?,
            model: non_blank(self.model),
            status: parse_optional_usage_status(self.status)?,
            source: parse_optional_usage_source(self.source)?,
            stream: self.stream,
            min_cache_read: self.min_cache_read,
            min_first_token_latency_ms: self.min_first_token_latency_ms,
            since: parse_optional_time(self.since)?,
            until: parse_optional_time(self.until)?,
        };
        Ok((query, page, limit))
    }
}

impl UsageAccountRiskQueryParams {
    fn into_query(self) -> Result<UsageAccountRiskQuery, String> {
        const DEFAULT_WARNING_THRESHOLD_TOKENS: i64 = 800_000;
        const DEFAULT_CRITICAL_THRESHOLD_TOKENS: i64 = 1_000_000;
        const DEFAULT_SAMPLE_LIMIT: usize = 50;
        const MAX_SAMPLE_LIMIT: usize = 200;
        const MAX_CUSTOM_WINDOW_HOURS: i64 = 24 * 7;

        let now = Utc::now();
        let (timezone, offset) = usage_dashboard_timezone(self.timezone.as_deref());
        let custom_from = parse_optional_time(self.since)?;
        let custom_to = parse_optional_time(self.until)?;
        let window_key_value = non_blank(self.window_key).unwrap_or_else(|| "last24h".to_string());
        let (window_key, window_label, from, to) =
            if window_key_value == "custom" || custom_from.is_some() || custom_to.is_some() {
                let Some(from) = custom_from else {
                    return Err("自定义时间范围必须同时提供 since 和 until".to_string());
                };
                let Some(to) = custom_to else {
                    return Err("自定义时间范围必须同时提供 since 和 until".to_string());
                };
                if from >= to {
                    return Err("since 必须早于 until".to_string());
                }
                let to = to.min(now);
                if from >= to {
                    return Err("since 必须早于 until，且 until 不能全部落在未来".to_string());
                }
                let max_from = to - ChronoDuration::hours(MAX_CUSTOM_WINDOW_HOURS);
                if from < max_from {
                    return Err("自定义时间范围不能超过 7 天".to_string());
                }
                ("custom".to_string(), "自定义".to_string(), from, to)
            } else {
                let spec = usage_dashboard_window_spec_for_key(now, offset, &window_key_value);
                (spec.key, spec.label, spec.from, spec.to)
            };

        let warning_threshold_tokens = self
            .warning_threshold_tokens
            .unwrap_or(DEFAULT_WARNING_THRESHOLD_TOKENS)
            .max(0);
        let critical_threshold_tokens = self
            .critical_threshold_tokens
            .unwrap_or(DEFAULT_CRITICAL_THRESHOLD_TOKENS)
            .max(warning_threshold_tokens);
        let limit = self
            .limit
            .unwrap_or(DEFAULT_SAMPLE_LIMIT)
            .clamp(1, MAX_SAMPLE_LIMIT);

        Ok(UsageAccountRiskQuery {
            timezone,
            window_key,
            window_label,
            from,
            to,
            warning_threshold_tokens,
            critical_threshold_tokens,
            pool_id: self.account_id,
            endpoint: non_blank(self.endpoint),
            model: non_blank(self.model),
            stream: self.stream,
            limit,
        })
    }
}

fn non_blank(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn parse_optional_request_api_key_id(value: Option<String>) -> Result<Option<String>, String> {
    let Some(value) = non_blank(value) else {
        return Ok(None);
    };
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("无效 requestApiKeyId：必须为 64 位 SHA-256 十六进制摘要".to_string());
    }
    Ok(Some(value.to_ascii_lowercase()))
}

fn extract_request_id_token(value: &str) -> Option<String> {
    let start = value.find("req_")?;
    let token = value[start..]
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_' || *ch == '-')
        .collect::<String>();
    (token.len() > "req_".len()).then_some(token)
}

fn parse_optional_usage_status(value: Option<String>) -> Result<Option<UsageRecordStatus>, String> {
    let value = non_blank(value);
    value.as_deref().map(parse_usage_status).transpose()
}

fn parse_optional_usage_source(value: Option<String>) -> Result<Option<UsageSource>, String> {
    let value = non_blank(value);
    value.as_deref().map(parse_usage_source).transpose()
}

fn parse_optional_usage_route_kind(
    value: Option<String>,
) -> Result<Option<UsageRouteKind>, String> {
    let value = non_blank(value);
    value.as_deref().map(parse_usage_route_kind).transpose()
}

fn parse_optional_time(value: Option<String>) -> Result<Option<DateTime<Utc>>, String> {
    let value = non_blank(value);
    value.as_deref().map(parse_time).transpose()
}

fn parse_usage_status(value: &str) -> Result<UsageRecordStatus, String> {
    UsageRecordStatus::parse(value).ok_or_else(|| format!("无效 status: {}", value))
}

fn parse_usage_source(value: &str) -> Result<UsageSource, String> {
    UsageSource::parse(value).ok_or_else(|| format!("无效 source: {}", value))
}

fn parse_usage_route_kind(value: &str) -> Result<UsageRouteKind, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "account" | "external" => Ok(UsageRouteKind::Account),
        _ => Err(format!("无效 routeKind: {}", value)),
    }
}

fn parse_time(value: &str) -> Result<DateTime<Utc>, String> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|_| format!("无效时间: {}", value))
}

/// POST /api/admin/accounts/:id/supported-models
/// 设置账号支持模型列表
pub async fn set_account_supported_models(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetSupportedModelsRequest>,
) -> impl IntoResponse {
    match state.service.set_account_supported_models(id, payload) {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/accounts/:id/supported-models/sync
/// 使用账号自身的兼容 /v1/models 接口同步并写回支持模型列表
pub async fn sync_account_supported_models(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<DiscoverAccountSupportedModelsRequest>,
) -> impl IntoResponse {
    match state
        .service
        .sync_account_supported_models(id, payload)
        .await
    {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/accounts/supported-models/discover
/// 使用未保存的账号配置发现支持模型，不写回。
pub async fn discover_account_supported_models_from_request(
    State(state): State<AdminState>,
    Json(payload): Json<DiscoverAccountSupportedModelsRequest>,
) -> impl IntoResponse {
    match state
        .service
        .discover_account_supported_models(None, payload)
        .await
    {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/accounts/:id/supported-models/discover
/// 使用已保存账号配置发现支持模型，不写回。
pub async fn discover_account_supported_models(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<DiscoverAccountSupportedModelsRequest>,
) -> impl IntoResponse {
    match state
        .service
        .discover_account_supported_models(Some(id), payload)
        .await
    {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/security/keys
/// 获取页面可复制的请求 Key 和当前后台登录 Key。
pub async fn get_access_keys(State(state): State<AdminState>) -> impl IntoResponse {
    Json(
        state
            .service
            .get_access_keys(&state.current_admin_api_key()),
    )
}

/// POST /api/admin/security/request-keys
/// 新增一个客户端调用 API Key。未传 apiKey 时由服务端生成。
pub async fn create_request_api_key(
    State(state): State<AdminState>,
    Json(payload): Json<CreateRequestApiKeyRequest>,
) -> impl IntoResponse {
    match state
        .service
        .create_request_api_key(payload, &state.current_admin_api_key())
    {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// PUT /api/admin/security/request-keys/:id
/// 替换一个客户端调用 API Key。
pub async fn update_request_api_key(
    State(state): State<AdminState>,
    Path(id): Path<String>,
    Json(payload): Json<UpdateRequestApiKeyRequest>,
) -> impl IntoResponse {
    match state
        .service
        .update_request_api_key(&id, payload, &state.current_admin_api_key())
    {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/security/request-keys/usage-limits
/// 每个设置了用量限额的 Key 当前已用量。
pub async fn get_request_key_usage_limits(State(state): State<AdminState>) -> impl IntoResponse {
    match state.service.get_request_key_usage_limits().await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/security/request-keys/:id/usage-limits/reset
/// 把某个 Key 当前周期的用量计数清零。
pub async fn reset_request_key_usage(
    State(state): State<AdminState>,
    Path(id): Path<String>,
    Json(payload): Json<ResetRequestKeyUsageRequest>,
) -> impl IntoResponse {
    match state
        .service
        .reset_request_key_usage(&id, payload.period)
        .await
    {
        Ok(()) => Json(serde_json::json!({ "success": true })).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// DELETE /api/admin/security/request-keys/:id
/// 删除一个客户端调用 API Key。最后一个 Key 不允许删除。
pub async fn delete_request_api_key(
    State(state): State<AdminState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state
        .service
        .delete_request_api_key(&id, &state.current_admin_api_key())
    {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

pub async fn get_accounts(State(state): State<AdminState>) -> impl IntoResponse {
    match state.service.list_accounts() {
        Ok(accounts) => Json(serde_json::json!({ "accounts": accounts })).into_response(),
        Err(err) => (err.status_code(), Json(err.into_response())).into_response(),
    }
}

pub async fn create_account(
    State(state): State<AdminState>,
    Json(payload): Json<CreateAccountRequest>,
) -> impl IntoResponse {
    match state.service.create_account(payload) {
        Ok(pool) => Json(pool).into_response(),
        Err(err) => (err.status_code(), Json(err.into_response())).into_response(),
    }
}

pub async fn copy_account(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    // 请求体可省略：无 body、空 body 都按默认参数处理；有内容则必须是合法 JSON。
    let payload = if body.iter().all(u8::is_ascii_whitespace) {
        CopyAccountRequest::default()
    } else {
        match serde_json::from_slice::<CopyAccountRequest>(&body) {
            Ok(payload) => payload,
            Err(err) => {
                let err = super::error::AdminServiceError::InvalidRequest(format!(
                    "请求体不是合法 JSON: {err}"
                ));
                return (err.status_code(), Json(err.into_response())).into_response();
            }
        }
    };
    match state.service.copy_account(id, payload) {
        Ok(pool) => Json(pool).into_response(),
        Err(err) => (err.status_code(), Json(err.into_response())).into_response(),
    }
}

pub async fn update_account(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<UpdateAccountRequest>,
) -> impl IntoResponse {
    match state.service.update_account(id, payload) {
        Ok(pool) => Json(pool).into_response(),
        Err(err) => (err.status_code(), Json(err.into_response())).into_response(),
    }
}

pub async fn delete_account(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.delete_account(id) {
        Ok(()) => Json(SuccessResponse::new("账号已删除")).into_response(),
        Err(err) => (err.status_code(), Json(err.into_response())).into_response(),
    }
}

pub async fn set_account_enabled(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetAccountEnabledRequest>,
) -> impl IntoResponse {
    match state.service.set_account_enabled(id, payload) {
        Ok(pool) => Json(pool).into_response(),
        Err(err) => (err.status_code(), Json(err.into_response())).into_response(),
    }
}

pub async fn clear_account_auto_disabled(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.clear_account_auto_disabled(id) {
        Ok(pool) => Json(pool).into_response(),
        Err(err) => (err.status_code(), Json(err.into_response())).into_response(),
    }
}

pub async fn clear_account_cooldown(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.clear_account_cooldown(id) {
        Ok(pool) => Json(pool).into_response(),
        Err(err) => (err.status_code(), Json(err.into_response())).into_response(),
    }
}

pub async fn get_account_status(State(state): State<AdminState>) -> impl IntoResponse {
    match state.service.get_account_status() {
        Ok(status) => Json(status).into_response(),
        Err(err) => (err.status_code(), Json(err.into_response())).into_response(),
    }
}

pub async fn test_account(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    payload: Option<Json<AccountTestRequest>>,
) -> impl IntoResponse {
    match state
        .service
        .test_account(id, payload.map(|Json(payload)| payload))
    {
        Ok(result) => Json(result).into_response(),
        Err(err) => (err.status_code(), Json(err.into_response())).into_response(),
    }
}

/// PUT /api/admin/security/admin-key
/// 修改后台登录 Key。成功后当前进程的 Admin API 认证立即切到新 Key。
pub async fn update_admin_api_key(
    State(state): State<AdminState>,
    Json(payload): Json<UpdateAdminApiKeyRequest>,
) -> impl IntoResponse {
    match state.service.update_admin_api_key(payload) {
        Ok(response) => {
            state.set_admin_api_key(response.admin_api_key.clone());
            Json(response).into_response()
        }
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/config/runtime
/// 获取运行时全局配置
pub async fn get_runtime_config(State(state): State<AdminState>) -> impl IntoResponse {
    Json(state.service.get_runtime_config())
}

/// PUT /api/admin/config/runtime
/// 更新运行时全局配置
pub async fn update_runtime_config(
    State(state): State<AdminState>,
    Json(payload): Json<UpdateRuntimeConfigRequest>,
) -> impl IntoResponse {
    match state.service.update_runtime_config(payload) {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/prompt-injection/catalog
/// 全部注入项的说明、内置文本、建议和关闭后的影响
pub async fn get_prompt_injection_catalog(State(state): State<AdminState>) -> impl IntoResponse {
    Json(state.service.prompt_injection_catalog())
}

/// POST /api/admin/prompt-injection/preview
/// 按路由和请求条件预览实际注入的内容；可以带未保存的配置草稿，不会保存。
pub async fn preview_prompt_injection(
    State(state): State<AdminState>,
    Json(payload): Json<PromptInjectionPreviewBody>,
) -> impl IntoResponse {
    match state.service.preview_prompt_injection(payload) {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/model-pricing
/// 获取模型价格目录状态
pub async fn get_model_pricing(State(state): State<AdminState>) -> impl IntoResponse {
    Json(state.service.get_model_pricing())
}

/// POST /api/admin/model-pricing/sync
/// 手动同步模型价格目录
pub async fn sync_model_pricing(State(state): State<AdminState>) -> impl IntoResponse {
    Json(state.service.sync_model_pricing().await)
}

/// GET /api/admin/model-capabilities
/// 获取模型能力目录状态
pub async fn get_model_capabilities(State(state): State<AdminState>) -> impl IntoResponse {
    Json(state.service.get_model_capabilities())
}

/// POST /api/admin/model-capabilities/manual
/// 添加或更新手动模型补充
pub async fn upsert_manual_model(
    State(state): State<AdminState>,
    Json(payload): Json<UpsertManualModelRequest>,
) -> impl IntoResponse {
    match state.service.upsert_manual_model(payload).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/model-capabilities/discover
/// 从公信力来源拉取候选模型（不写入）
pub async fn discover_model_catalog(State(state): State<AdminState>) -> impl IntoResponse {
    match state.service.discover_model_catalog().await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/model-capabilities/adopt
/// 把用户确认的候选加入目录
pub async fn adopt_model_catalog(
    State(state): State<AdminState>,
    Json(payload): Json<ModelCatalogAdoptRequest>,
) -> impl IntoResponse {
    match state.service.adopt_model_catalog(payload).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// DELETE /api/admin/model-capabilities/manual/:model
/// 删除手动模型补充
pub async fn delete_manual_model(
    State(state): State<AdminState>,
    Path(model): Path<String>,
) -> impl IntoResponse {
    match state.service.delete_manual_model(model).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/usage-records
/// 查询请求级 usage 记录
pub async fn get_usage_records(
    State(state): State<AdminState>,
    Query(params): Query<UsageRecordsQueryParams>,
) -> impl IntoResponse {
    match params.into_query() {
        Ok(query) => Json(state.service.get_usage_records(query)).into_response(),
        Err(message) => (
            StatusCode::BAD_REQUEST,
            Json(AdminErrorResponse::invalid_request(message)),
        )
            .into_response(),
    }
}

/// GET /api/admin/usage-records-paged
/// 分页查询请求级 usage 记录
pub async fn get_usage_records_page(
    State(state): State<AdminState>,
    Query(params): Query<UsageRecordsPageQueryParams>,
) -> impl IntoResponse {
    match params.into_query() {
        Ok((query, page, limit)) => {
            Json(state.service.get_usage_records_page(query, page, limit)).into_response()
        }
        Err(message) => (
            StatusCode::BAD_REQUEST,
            Json(AdminErrorResponse::invalid_request(message)),
        )
            .into_response(),
    }
}

/// GET /api/admin/usage-summary
/// 获取请求级 usage 汇总
pub async fn get_usage_summary(State(state): State<AdminState>) -> impl IntoResponse {
    Json(state.service.get_usage_summary())
}

/// GET /api/admin/usage-dashboard
/// 获取 PgSQL 聚合的 usage 仪表盘数据。
pub async fn get_usage_dashboard(
    State(state): State<AdminState>,
    Query(params): Query<UsageDashboardQueryParams>,
) -> impl IntoResponse {
    match state.service.get_usage_dashboard(params.timezone) {
        Ok(data) => Json(data).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/usage-dashboard/overview/summary
pub async fn get_usage_dashboard_overview_summary(
    State(state): State<AdminState>,
    Query(params): Query<UsageOverviewQueryParams>,
) -> impl IntoResponse {
    match state.service.get_usage_dashboard_overview_summary(
        params.timezone,
        params.range,
        params.from,
        params.to,
        params.granularity,
        params.top_n,
    ) {
        Ok(data) => Json(data).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/usage-dashboard/overview/series
pub async fn get_usage_dashboard_overview_series(
    State(state): State<AdminState>,
    Query(params): Query<UsageOverviewQueryParams>,
) -> impl IntoResponse {
    match state.service.get_usage_dashboard_overview_series(
        params.timezone,
        params.range,
        params.from,
        params.to,
        params.granularity,
        params.top_n,
    ) {
        Ok(data) => Json(data).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/usage-dashboard/overview/rankings
pub async fn get_usage_dashboard_overview_rankings(
    State(state): State<AdminState>,
    Query(params): Query<UsageOverviewQueryParams>,
) -> impl IntoResponse {
    match state.service.get_usage_dashboard_overview_rankings(
        params.timezone,
        params.range,
        params.from,
        params.to,
        params.granularity,
        params.top_n,
    ) {
        Ok(data) => Json(data).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/usage-dashboard/windows
/// 获取 usage 仪表盘窗口汇总。
pub async fn get_usage_dashboard_windows(
    State(state): State<AdminState>,
    Query(params): Query<UsageDashboardQueryParams>,
) -> impl IntoResponse {
    match state.service.get_usage_dashboard_windows(params.timezone) {
        Ok(data) => Json(data).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/usage-dashboard/series
/// 获取 usage 仪表盘趋势数据。
pub async fn get_usage_dashboard_series(
    State(state): State<AdminState>,
    Query(params): Query<UsageDashboardQueryParams>,
) -> impl IntoResponse {
    match state.service.get_usage_dashboard_series(params.timezone) {
        Ok(data) => Json(data).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/usage-dashboard/top
/// 获取 usage 仪表盘排行数据。
pub async fn get_usage_dashboard_top(
    State(state): State<AdminState>,
    Query(params): Query<UsageDashboardWindowQueryParams>,
) -> impl IntoResponse {
    match state
        .service
        .get_usage_dashboard_top(params.timezone, params.window_key)
    {
        Ok(data) => Json(data).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/usage-dashboard/breakdown
/// 获取 usage 仪表盘单个窗口的状态和来源分布。
pub async fn get_usage_dashboard_breakdown(
    State(state): State<AdminState>,
    Query(params): Query<UsageDashboardWindowQueryParams>,
) -> impl IntoResponse {
    let window_key = params.window_key.unwrap_or_else(|| "today".to_string());
    match state
        .service
        .get_usage_dashboard_breakdown(params.timezone, window_key)
    {
        Ok(data) => Json(data).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/usage-dashboard/account-billing
/// 获取 usage 仪表盘单个窗口的账号费用明细。
pub async fn get_usage_dashboard_account_billing(
    State(state): State<AdminState>,
    Query(params): Query<UsageDashboardWindowQueryParams>,
) -> impl IntoResponse {
    let window_key = params.window_key.unwrap_or_else(|| "today".to_string());
    match state
        .service
        .get_usage_dashboard_account_billing(params.timezone, window_key)
    {
        Ok(data) => Json(data).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/usage-dashboard/account-risk
/// 获取账号 usage 风控统计。
pub async fn get_usage_dashboard_account_risk(
    State(state): State<AdminState>,
    Query(params): Query<UsageAccountRiskQueryParams>,
) -> impl IntoResponse {
    match params.into_query() {
        Ok(query) => match state.service.get_usage_dashboard_account_risk(query) {
            Ok(data) => Json(data).into_response(),
            Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
        },
        Err(message) => (
            StatusCode::BAD_REQUEST,
            Json(AdminErrorResponse::invalid_request(message)),
        )
            .into_response(),
    }
}

/// GET /api/admin/usage-dashboard/account-latency
/// 按账号的总耗时 / 首字延迟 p50 / p95 / p99（扫明细，范围最长 8 天）。
pub async fn get_usage_dashboard_account_latency(
    State(state): State<AdminState>,
    Query(params): Query<UsageAccountLatencyQueryParams>,
) -> impl IntoResponse {
    match state.service.get_usage_dashboard_account_latency(
        params.timezone,
        params.range,
        params.from,
        params.to,
        params.account_id,
    ) {
        Ok(data) => Json(data).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/usage-dashboard/route-latency?range=today
/// 按路由前缀的总耗时 / 首字延迟分位数；范围最长 8 天。
pub async fn get_usage_dashboard_route_latency(
    State(state): State<AdminState>,
    Query(params): Query<UsageOverviewQueryParams>,
) -> impl IntoResponse {
    match state.service.get_usage_dashboard_route_latency(
        params.timezone,
        params.range,
        params.from,
        params.to,
    ) {
        Ok(data) => Json(data).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/accounts/{id}/quality-history?hours=24
/// 账号质量分历史（错误率 / 首字 / 耗时 EWMA 快照），按时间升序。
pub async fn get_account_quality_history(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Query(params): Query<AccountQualityHistoryQueryParams>,
) -> impl IntoResponse {
    match state
        .service
        .get_account_quality_history(id, params.hours.unwrap_or(24))
    {
        Ok(data) => Json(data).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/usage-writer-stats
/// 获取 usage 持久化 writer 观测状态。
pub async fn get_usage_writer_stats(State(state): State<AdminState>) -> impl IntoResponse {
    Json(state.service.get_usage_writer_stats())
}

/// POST /api/admin/usage-records/clear
/// 清空请求级 usage 记录
pub async fn clear_usage_records(
    State(state): State<AdminState>,
    payload: Option<Json<UsageCleanupRequest>>,
) -> impl IntoResponse {
    match state
        .service
        .clear_usage_records(payload.map(|Json(payload)| payload))
    {
        Ok(data) => (StatusCode::ACCEPTED, Json(data)).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/usage-records/cleanup/preview
/// 预估手动分批清理会影响的 usage 记录数量。
pub async fn preview_usage_cleanup(
    State(state): State<AdminState>,
    Json(request): Json<UsageCleanupRequest>,
) -> impl IntoResponse {
    match state.service.preview_usage_cleanup(request) {
        Ok(data) => Json(data).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/usage-records/cleanup/start
/// 启动一次手动后台分批清理任务。
pub async fn start_usage_cleanup(
    State(state): State<AdminState>,
    Json(request): Json<UsageCleanupRequest>,
) -> impl IntoResponse {
    match state.service.start_usage_cleanup(request) {
        Ok(data) => (StatusCode::ACCEPTED, Json(data)).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/usage-records/cleanup/resume
/// 恢复一个失败或已取消的持久化 usage 清理任务。
pub async fn resume_usage_cleanup(
    State(state): State<AdminState>,
    Json(request): Json<UsageCleanupResumeRequest>,
) -> impl IntoResponse {
    match state.service.resume_usage_cleanup(request) {
        Ok(data) => (StatusCode::ACCEPTED, Json(data)).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/usage-records/cleanup/status
/// 获取当前手动 usage 清理任务状态。
pub async fn get_usage_cleanup_status(State(state): State<AdminState>) -> impl IntoResponse {
    Json(state.service.get_usage_cleanup_status())
}

/// POST /api/admin/usage-records/cleanup/cancel
/// 请求取消当前手动 usage 清理任务。
pub async fn cancel_usage_cleanup(State(state): State<AdminState>) -> impl IntoResponse {
    match state.service.cancel_usage_cleanup() {
        Ok(data) => Json(data).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/audit-logs
/// 分页查询 Admin 审计日志。
pub async fn get_audit_logs(
    State(state): State<AdminState>,
    Query(params): Query<AuditLogsQueryParams>,
) -> impl IntoResponse {
    let parse_time = |value: &Option<String>,
                      name: &str|
     -> Result<
        Option<chrono::DateTime<chrono::Utc>>,
        crate::admin::error::AdminServiceError,
    > {
        match value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            None => Ok(None),
            Some(value) => chrono::DateTime::parse_from_rfc3339(value)
                .map(|time| Some(time.with_timezone(&chrono::Utc)))
                .map_err(|_| {
                    crate::admin::error::AdminServiceError::InvalidCredential(format!(
                        "{name} 必须是 RFC3339 时间"
                    ))
                }),
        }
    };
    let filter = match (
        parse_time(&params.since, "since"),
        parse_time(&params.until, "until"),
    ) {
        (Ok(since), Ok(until)) => crate::storage::postgres::AdminAuditLogFilter {
            account_events_only: params.account_events.unwrap_or(false),
            account_event: params.account_event,
            account_source: params.account_source,
            object_type: params.object_type,
            object_id: params.object_id,
            account_query: params.account,
            action: params.action,
            actions: params.actions.map(|value| {
                value
                    .split(',')
                    .map(|item| item.trim().to_string())
                    .filter(|item| !item.is_empty())
                    .collect()
            }),
            actor: params.actor,
            success: params.success,
            since,
            until,
            text_query: params.q,
        },
        (Err(err), _) | (_, Err(err)) => {
            return (err.status_code(), Json(err.into_response())).into_response();
        }
    };
    Json(state.service.get_audit_logs_filtered(
        params.page.unwrap_or(1),
        params.limit.unwrap_or(20),
        filter,
    ))
    .into_response()
}

/// GET /api/admin/audit-logs/account-summary
/// 账号异动按类型统计（默认最近 24 小时）。
pub async fn get_account_event_summary(
    State(state): State<AdminState>,
    Query(params): Query<AccountEventSummaryQueryParams>,
) -> impl IntoResponse {
    let hours = params.hours.unwrap_or(24).clamp(1, 720);
    Json(
        state
            .service
            .get_account_event_summary(hours, params.account_source),
    )
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_records_query_ignores_blank_text_filters() {
        let query = UsageRecordsQueryParams {
            limit: Some(25),
            request_id: None,
            q: Some("   ".to_string()),
            conversation_id: Some("   ".to_string()),
            request_api_key_id: Some("   ".to_string()),
            credential_id: Some(7),
            account_id: None,
            route_kind: Some("external".to_string()),
            model: Some("".to_string()),
            status: Some("".to_string()),
            source: Some("   ".to_string()),
            endpoint: Some("   ".to_string()),
            route: None,
            stream: Some(true),
            min_cache_read: Some(10_000),
            min_first_token_latency_ms: Some(10_000),
            since: None,
            until: None,
        }
        .into_query()
        .expect("valid query");

        assert_eq!(query.limit, 25);
        assert_eq!(query.q, None);
        assert_eq!(query.conversation_id, None);
        assert_eq!(query.request_api_key_id, None);
        assert_eq!(query.credential_id, Some(7));
        assert_eq!(query.route_kind, Some(UsageRouteKind::Account));
        assert_eq!(query.model, None);
        assert_eq!(query.status, None);
        assert_eq!(query.source, None);
        assert_eq!(query.endpoint, None);
        assert_eq!(query.stream, Some(true));
        assert_eq!(query.min_cache_read, Some(10_000));
        assert_eq!(query.min_first_token_latency_ms, Some(10_000));
    }

    #[test]
    fn usage_records_query_extracts_request_id_from_search_text() {
        let query = UsageRecordsQueryParams {
            limit: None,
            request_id: None,
            q: Some("请求 ID: req_01abcXYZ-123，模型 opus".to_string()),
            conversation_id: None,
            request_api_key_id: None,
            credential_id: None,
            account_id: None,
            route_kind: None,
            model: None,
            status: None,
            source: None,
            endpoint: None,
            route: None,
            stream: None,
            min_cache_read: None,
            min_first_token_latency_ms: None,
            since: None,
            until: None,
        }
        .into_query()
        .expect("valid query");

        assert_eq!(query.request_id.as_deref(), Some("req_01abcXYZ-123"));
        assert_eq!(query.q, None);
    }

    #[test]
    fn usage_records_page_query_accepts_http_request_id_names() {
        let params: UsageRecordsPageQueryParams = serde_json::from_value(serde_json::json!({
            "page": 1,
            "limit": 20,
            "requestId": " req_01camel "
        }))
        .expect("camelCase request id should deserialize");
        let (query, page, limit) = params.into_query().expect("valid query");

        assert_eq!(page, 1);
        assert_eq!(limit, 20);
        assert_eq!(query.request_id.as_deref(), Some("req_01camel"));

        let params: UsageRecordsPageQueryParams = serde_json::from_value(serde_json::json!({
            "req_id": "req_01snake"
        }))
        .expect("snake_case alias should deserialize");
        let (query, _, _) = params.into_query().expect("valid query");

        assert_eq!(query.request_id.as_deref(), Some("req_01snake"));
    }

    #[test]
    fn usage_records_query_keeps_explicit_request_id_over_search_text() {
        let query = UsageRecordsQueryParams {
            limit: None,
            request_id: Some("  req_01explicit  ".to_string()),
            q: Some("req_01fromq".to_string()),
            conversation_id: None,
            request_api_key_id: None,
            credential_id: None,
            account_id: None,
            route_kind: None,
            model: None,
            status: None,
            source: None,
            endpoint: None,
            route: None,
            stream: None,
            min_cache_read: None,
            min_first_token_latency_ms: None,
            since: None,
            until: None,
        }
        .into_query()
        .expect("valid query");

        assert_eq!(query.request_id.as_deref(), Some("req_01explicit"));
        assert_eq!(query.q.as_deref(), Some("req_01fromq"));
    }

    #[test]
    fn usage_records_query_rejects_invalid_enums_and_time() {
        let bad_status = UsageRecordsQueryParams {
            limit: None,
            request_id: None,
            q: None,
            conversation_id: None,
            request_api_key_id: None,
            credential_id: None,
            account_id: None,
            route_kind: None,
            model: None,
            status: Some("ok".to_string()),
            source: None,
            endpoint: None,
            route: None,
            stream: None,
            min_cache_read: None,
            min_first_token_latency_ms: None,
            since: None,
            until: None,
        }
        .into_query()
        .unwrap_err();
        assert!(bad_status.contains("无效 status"));

        let bad_source = UsageRecordsQueryParams {
            limit: None,
            request_id: None,
            q: None,
            conversation_id: None,
            request_api_key_id: None,
            credential_id: None,
            account_id: None,
            route_kind: None,
            model: None,
            status: None,
            source: Some("cache".to_string()),
            endpoint: None,
            route: None,
            stream: None,
            min_cache_read: None,
            min_first_token_latency_ms: None,
            since: None,
            until: None,
        }
        .into_query()
        .unwrap_err();
        assert!(bad_source.contains("无效 source"));

        let bad_time = UsageRecordsQueryParams {
            limit: None,
            request_id: None,
            q: None,
            conversation_id: None,
            request_api_key_id: None,
            credential_id: None,
            account_id: None,
            route_kind: None,
            model: None,
            status: None,
            source: None,
            endpoint: None,
            route: None,
            stream: None,
            min_cache_read: None,
            min_first_token_latency_ms: None,
            since: Some("not-a-time".to_string()),
            until: None,
        }
        .into_query()
        .unwrap_err();
        assert!(bad_time.contains("无效时间"));

        let invalid_request_key: UsageRecordsQueryParams =
            serde_json::from_value(serde_json::json!({
                "requestApiKeyId": "sk-raw-key-must-not-be-queryable"
            }))
            .unwrap();
        let invalid_request_key = invalid_request_key.into_query().unwrap_err();
        assert!(invalid_request_key.contains("无效 requestApiKeyId"));
    }

    #[test]
    fn usage_records_page_query_keeps_pagination_separate_from_filters() {
        let (query, page, limit) = UsageRecordsPageQueryParams {
            page: Some(3),
            limit: Some(50),
            request_id: None,
            q: Some("sonnet".to_string()),
            conversation_id: Some("session-a".to_string()),
            request_api_key_id: Some("A".repeat(64)),
            credential_id: None,
            account_id: None,
            route_kind: None,
            model: None,
            status: None,
            source: None,
            endpoint: Some("/ha".to_string()),
            route: None,
            stream: None,
            min_cache_read: None,
            min_first_token_latency_ms: None,
            since: None,
            until: None,
        }
        .into_query()
        .expect("valid page query");

        assert_eq!(page, 3);
        assert_eq!(limit, 50);
        assert_eq!(query.limit, 50);
        assert_eq!(query.q.as_deref(), Some("sonnet"));
        assert_eq!(query.conversation_id.as_deref(), Some("session-a"));
        assert_eq!(query.request_api_key_id, Some("a".repeat(64)));
        assert_eq!(query.endpoint.as_deref(), Some("/ha"));
    }
}

pub async fn promote_account_models(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> axum::response::Response {
    match state.service.promote_account_models(id).await {
        Ok(response) => Json(response).into_response(),
        Err(error) => (error.status_code(), Json(error.into_response())).into_response(),
    }
}
