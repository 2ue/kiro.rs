//! Admin API HTTP 处理器

use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::Deserialize;

use super::{
    middleware::AdminState,
    types::{
        AddCredentialRequest, AdminErrorResponse, BatchCredentialImportRequest,
        BatchUpdateCredentialsRequest, ClearInFlightRequest, CreateProxyResourceRequest,
        CreateRequestApiKeyRequest, DiscoverExternalPoolSupportedModelsRequest,
        ExportCredentialsQuery, ExternalPoolTestRequest, ProxyResourceTestRequest,
        RefreshCredentialInfoRequest, SetCredentialConcurrencyRequest, SetCredentialOverageRequest,
        SetCredentialProxyRequest, SetCredentialRateLimitAutoDisableRequest,
        SetCredentialRegionsRequest, SetCredentialRpmRequest, SetDisabledRequest,
        SetLoadBalancingModeRequest, SetPriorityRequest, SetSupportedModelsRequest,
        SetWarmupRequest, SuccessResponse, SystemVersionResponse, TestCredentialRequest,
        UpdateAdminApiKeyRequest, UpdateCredentialAuthRequest, UpdateProxyResourceRequest,
        UpdateRequestApiKeyRequest, UpdateRuntimeConfigRequest, UpsertManualModelRequest,
        UsageCleanupRequest, UsageCleanupResumeRequest, ValidateExistingCredentialsRequest,
        ValidateExternalCredentialsRequest,
    },
};
use crate::anthropic::usage::{
    UsageExternalPoolRiskQuery, UsageRecordQuery, UsageRecordStatus, UsageRouteKind, UsageSource,
    usage_dashboard_timezone, usage_dashboard_window_spec_for_key,
};
use crate::external_pool::{
    CreateExternalPoolRequest, SetExternalPoolEnabledRequest, UpdateExternalPoolRequest,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialsPageQueryParams {
    pub page: Option<usize>,
    pub limit: Option<usize>,
    pub q: Option<String>,
    #[serde(alias = "id")]
    pub credential_id: Option<u64>,
    #[serde(alias = "email")]
    pub account: Option<String>,
    pub region: Option<String>,
    pub model: Option<String>,
    pub endpoint: Option<String>,
    pub priority: Option<u32>,
    pub rpm: Option<u32>,
    pub concurrency: Option<u32>,
    pub status: Option<String>,
    pub auth_method: Option<String>,
    pub subscription: Option<String>,
    pub proxy_resource_id: Option<u64>,
    pub sort_by: Option<String>,
    pub sort_order: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialInfoQueryParams {
    pub force: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialsIdsQueryParams {
    pub ids: Option<String>,
}

impl CredentialsIdsQueryParams {
    fn ids(&self) -> Result<Vec<u64>, String> {
        let Some(ids) = self.ids.as_deref() else {
            return Ok(Vec::new());
        };
        ids.split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| {
                value
                    .parse::<u64>()
                    .map_err(|_| format!("无效凭据 ID: {}", value))
            })
            .collect()
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditLogsQueryParams {
    pub page: Option<usize>,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecordsQueryParams {
    pub limit: Option<usize>,
    #[serde(alias = "request_id", alias = "reqId", alias = "req_id")]
    pub request_id: Option<String>,
    pub q: Option<String>,
    pub endpoint: Option<String>,
    pub conversation_id: Option<String>,
    pub request_api_key_id: Option<String>,
    pub credential_id: Option<u64>,
    pub external_pool_id: Option<u64>,
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
    pub conversation_id: Option<String>,
    pub request_api_key_id: Option<String>,
    pub credential_id: Option<u64>,
    pub external_pool_id: Option<u64>,
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
pub struct UsageDashboardWindowQueryParams {
    pub timezone: Option<String>,
    pub window_key: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDashboardAccountsQueryParams {
    pub timezone: Option<String>,
    pub window_key: Option<String>,
    pub page: Option<usize>,
    pub page_size: Option<usize>,
    pub q: Option<String>,
    pub status: Option<String>,
    pub sort_by: Option<String>,
    pub sort_order: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageExternalPoolRiskQueryParams {
    pub timezone: Option<String>,
    pub window_key: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub warning_threshold_tokens: Option<i64>,
    pub critical_threshold_tokens: Option<i64>,
    pub external_pool_id: Option<u64>,
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
            conversation_id: non_blank(self.conversation_id),
            request_api_key_id: parse_optional_request_api_key_id(self.request_api_key_id)?,
            credential_id: self.credential_id,
            external_pool_id: self.external_pool_id,
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
            conversation_id: non_blank(self.conversation_id),
            request_api_key_id: parse_optional_request_api_key_id(self.request_api_key_id)?,
            credential_id: self.credential_id,
            external_pool_id: self.external_pool_id,
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

impl UsageExternalPoolRiskQueryParams {
    fn into_query(self) -> Result<UsageExternalPoolRiskQuery, String> {
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

        Ok(UsageExternalPoolRiskQuery {
            timezone,
            window_key,
            window_label,
            from,
            to,
            warning_threshold_tokens,
            critical_threshold_tokens,
            pool_id: self.external_pool_id,
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
        "local_credential" | "local-credential" | "local" => Ok(UsageRouteKind::LocalCredential),
        "external_pool" | "external-pool" | "external" => Ok(UsageRouteKind::ExternalPool),
        _ => Err(format!("无效 routeKind: {}", value)),
    }
}

fn parse_time(value: &str) -> Result<DateTime<Utc>, String> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|_| format!("无效时间: {}", value))
}

/// GET /api/admin/credentials
/// 获取所有凭据状态
pub async fn get_all_credentials(State(state): State<AdminState>) -> impl IntoResponse {
    let response = state.service.get_all_credentials();
    Json(response)
}

/// GET /api/admin/credentials-paged
/// 分页获取凭据状态
pub async fn get_credentials_page(
    State(state): State<AdminState>,
    Query(params): Query<CredentialsPageQueryParams>,
) -> impl IntoResponse {
    Json(state.service.get_credentials_page(
        params.page.unwrap_or_default(),
        params.limit.unwrap_or_default(),
        super::service::CredentialListQuery {
            q: non_blank(params.q),
            credential_id: params.credential_id,
            account: non_blank(params.account),
            region: non_blank(params.region),
            model: non_blank(params.model),
            endpoint: non_blank(params.endpoint),
            priority: params.priority,
            rpm: params.rpm,
            concurrency: params.concurrency,
            status: non_blank(params.status),
            auth_method: non_blank(params.auth_method),
            subscription: non_blank(params.subscription),
            proxy_resource_id: params.proxy_resource_id,
            sort_by: non_blank(params.sort_by),
            sort_order: non_blank(params.sort_order),
        },
    ))
}

/// GET /api/admin/credentials-list
/// 轻量分页获取凭据基础字段
pub async fn get_credentials_list(
    State(state): State<AdminState>,
    Query(params): Query<CredentialsPageQueryParams>,
) -> impl IntoResponse {
    Json(state.service.get_credentials_list(
        params.page.unwrap_or_default(),
        params.limit.unwrap_or_default(),
        super::service::CredentialListQuery {
            q: non_blank(params.q),
            credential_id: params.credential_id,
            account: non_blank(params.account),
            region: non_blank(params.region),
            model: non_blank(params.model),
            endpoint: non_blank(params.endpoint),
            priority: params.priority,
            rpm: params.rpm,
            concurrency: params.concurrency,
            status: non_blank(params.status),
            auth_method: non_blank(params.auth_method),
            subscription: non_blank(params.subscription),
            proxy_resource_id: params.proxy_resource_id,
            sort_by: non_blank(params.sort_by),
            sort_order: non_blank(params.sort_order),
        },
    ))
}

/// GET /api/admin/credentials/summary
pub async fn get_credentials_summary(State(state): State<AdminState>) -> impl IntoResponse {
    Json(state.service.get_credentials_summary())
}

/// GET /api/admin/credentials/runtime
pub async fn get_credentials_runtime(
    State(state): State<AdminState>,
    Query(params): Query<CredentialsIdsQueryParams>,
) -> impl IntoResponse {
    match params.ids() {
        Ok(ids) => Json(state.service.get_credentials_runtime(&ids)).into_response(),
        Err(message) => (
            StatusCode::BAD_REQUEST,
            Json(AdminErrorResponse::invalid_request(message)),
        )
            .into_response(),
    }
}

/// GET /api/admin/credentials/account-info
pub async fn get_credentials_account_info(
    State(state): State<AdminState>,
    Query(params): Query<CredentialsIdsQueryParams>,
) -> impl IntoResponse {
    match params.ids() {
        Ok(ids) => Json(state.service.get_credentials_account_info(&ids).await).into_response(),
        Err(message) => (
            StatusCode::BAD_REQUEST,
            Json(AdminErrorResponse::invalid_request(message)),
        )
            .into_response(),
    }
}

/// GET /api/admin/credentials/usage-summary
pub async fn get_credentials_usage_summary(
    State(state): State<AdminState>,
    Query(params): Query<CredentialsIdsQueryParams>,
) -> impl IntoResponse {
    match params.ids() {
        Ok(ids) => Json(state.service.get_credentials_usage_summary(&ids)).into_response(),
        Err(message) => (
            StatusCode::BAD_REQUEST,
            Json(AdminErrorResponse::invalid_request(message)),
        )
            .into_response(),
    }
}

/// DELETE /api/admin/credentials/disabled
pub async fn delete_disabled_credentials(State(state): State<AdminState>) -> impl IntoResponse {
    match state.service.delete_disabled_credentials() {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/credentials/credit-summary
/// 获取本地凭据积分快照统计，不触发上游查询
pub async fn get_credential_credit_summary(State(state): State<AdminState>) -> impl IntoResponse {
    match state.service.get_credential_credit_summary().await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/disabled
/// 设置凭据禁用状态
pub async fn set_credential_disabled(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetDisabledRequest>,
) -> impl IntoResponse {
    match state.service.set_disabled(id, payload.disabled) {
        Ok(_) => {
            let action = if payload.disabled { "禁用" } else { "启用" };
            Json(SuccessResponse::new(format!("凭据 #{} 已{}", id, action))).into_response()
        }
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/priority
/// 设置凭据优先级
pub async fn set_credential_priority(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetPriorityRequest>,
) -> impl IntoResponse {
    match state.service.set_priority(id, payload.priority) {
        Ok(_) => Json(SuccessResponse::new(format!(
            "凭据 #{} 优先级已设置为 {}",
            id, payload.priority
        )))
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/concurrency
/// 设置凭据级最大并发覆盖
pub async fn set_credential_concurrency(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetCredentialConcurrencyRequest>,
) -> impl IntoResponse {
    match state.service.set_credential_concurrency(id, payload) {
        Ok(_) => Json(SuccessResponse::new(format!("凭据 #{} 并发限制已更新", id))).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/rpm
/// 设置凭据级 RPM 覆盖
pub async fn set_credential_rpm(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetCredentialRpmRequest>,
) -> impl IntoResponse {
    match state.service.set_credential_rpm(id, payload) {
        Ok(_) => Json(SuccessResponse::new(format!("凭据 #{} RPM 限制已更新", id))).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/rate-limit-auto-disable
/// 设置 429 临时风控自动禁用开关
pub async fn set_credential_rate_limit_auto_disable(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetCredentialRateLimitAutoDisableRequest>,
) -> impl IntoResponse {
    match state
        .service
        .set_credential_rate_limit_auto_disable(id, payload)
    {
        Ok(_) => Json(SuccessResponse::new(format!(
            "凭据 #{} 429 自动禁用开关已更新",
            id
        )))
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/supported-models
/// 设置凭据支持模型列表
pub async fn set_credential_supported_models(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetSupportedModelsRequest>,
) -> impl IntoResponse {
    match state.service.set_credential_supported_models(id, payload) {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/supported-models/sync
/// 使用该凭据同步并写回支持模型列表
pub async fn sync_credential_supported_models(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.sync_credential_supported_models(id).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/supported-models/discover
/// 使用该凭据拉取上游模型并生成可编辑的 Claude/Claude Code 支持模型建议，不写回。
pub async fn discover_credential_supported_models(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.discover_credential_supported_models(id).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/external-pools/:id/supported-models
/// 设置外部池支持模型列表
pub async fn set_external_pool_supported_models(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetSupportedModelsRequest>,
) -> impl IntoResponse {
    match state
        .service
        .set_external_pool_supported_models(id, payload)
    {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/external-pools/:id/supported-models/sync
/// 使用外部池自身的兼容 /v1/models 接口同步并写回支持模型列表
pub async fn sync_external_pool_supported_models(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<DiscoverExternalPoolSupportedModelsRequest>,
) -> impl IntoResponse {
    match state
        .service
        .sync_external_pool_supported_models(id, payload)
        .await
    {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/external-pools/supported-models/discover
/// 使用未保存的外部池配置发现支持模型，不写回。
pub async fn discover_external_pool_supported_models_from_request(
    State(state): State<AdminState>,
    Json(payload): Json<DiscoverExternalPoolSupportedModelsRequest>,
) -> impl IntoResponse {
    match state
        .service
        .discover_external_pool_supported_models(None, payload)
        .await
    {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/external-pools/:id/supported-models/discover
/// 使用已保存外部池配置发现支持模型，不写回。
pub async fn discover_external_pool_supported_models(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<DiscoverExternalPoolSupportedModelsRequest>,
) -> impl IntoResponse {
    match state
        .service
        .discover_external_pool_supported_models(Some(id), payload)
        .await
    {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/regions
/// 设置凭据级 Region 覆盖
pub async fn set_credential_regions(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetCredentialRegionsRequest>,
) -> impl IntoResponse {
    match state.service.set_credential_regions(id, payload) {
        Ok(_) => Json(SuccessResponse::new(format!("凭据 #{} Region 已更新", id))).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/warmup
/// 设置凭据预热剩余请求数
pub async fn set_credential_warmup(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetWarmupRequest>,
) -> impl IntoResponse {
    match state.service.set_warmup(id, payload) {
        Ok(_) => Json(SuccessResponse::new(format!("凭据 #{} 预热状态已更新", id))).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/in-flight/clear
/// 清理凭据并发占用
pub async fn clear_credential_in_flight(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<ClearInFlightRequest>,
) -> impl IntoResponse {
    match state.service.clear_in_flight(id, payload) {
        Ok(count) => Json(SuccessResponse::new(format!(
            "凭据 #{} 已清理 {} 个并发占用",
            id, count
        )))
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/reset
/// 重置失败计数并重新启用
pub async fn reset_failure_count(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.reset_and_enable(id) {
        Ok(_) => Json(SuccessResponse::new(format!(
            "凭据 #{} 失败计数已重置并重新启用",
            id
        )))
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/credentials/:id/balance
/// 获取指定凭据的账号信息（兼容旧路径）
pub async fn get_credential_balance(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.get_account_info(id, false).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/credentials/:id/info
/// 查询指定凭据的账号信息
pub async fn get_credential_info(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Query(params): Query<CredentialInfoQueryParams>,
) -> impl IntoResponse {
    match state
        .service
        .get_account_info(id, params.force.unwrap_or(false))
        .await
    {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/overage
/// 设置指定凭据的上游 Overages 开关，并返回刷新后的账号信息。
pub async fn set_credential_overage(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetCredentialOverageRequest>,
) -> impl IntoResponse {
    match state.service.set_credential_overage(id, payload).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/info/refresh
/// 批量查询凭据账号信息
pub async fn refresh_credentials_info(
    State(state): State<AdminState>,
    Json(payload): Json<RefreshCredentialInfoRequest>,
) -> impl IntoResponse {
    match state.service.refresh_credentials_info(payload).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credential-validation/existing
/// 校验系统已有凭据订阅信息
pub async fn validate_existing_credentials(
    State(state): State<AdminState>,
    Json(payload): Json<ValidateExistingCredentialsRequest>,
) -> impl IntoResponse {
    match state.service.validate_existing_credentials(payload).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credential-validation/external
/// 校验外部 JSON 凭据订阅信息
pub async fn validate_external_credentials(
    State(state): State<AdminState>,
    Json(payload): Json<ValidateExternalCredentialsRequest>,
) -> impl IntoResponse {
    match state.service.validate_external_credentials(payload).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/test
/// 使用指定凭据发起一次模型调用测试
pub async fn test_credential(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<TestCredentialRequest>,
) -> impl IntoResponse {
    match state.service.test_credential(id, payload).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/proxy-resources
/// 获取代理/家宽资源列表
pub async fn get_proxy_resources(State(state): State<AdminState>) -> impl IntoResponse {
    match state.service.list_proxy_resources() {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/proxy-resources
/// 新增代理/家宽资源
pub async fn create_proxy_resource(
    State(state): State<AdminState>,
    Json(payload): Json<CreateProxyResourceRequest>,
) -> impl IntoResponse {
    match state.service.create_proxy_resource(payload) {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/proxy-resources/test
/// 测试未保存的代理配置
pub async fn test_proxy_resource_config(
    State(state): State<AdminState>,
    Json(payload): Json<ProxyResourceTestRequest>,
) -> impl IntoResponse {
    match state.service.test_proxy_resource_config(payload).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/proxy-resources/:id/test
/// 测试已保存的代理资源
pub async fn test_proxy_resource(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<ProxyResourceTestRequest>,
) -> impl IntoResponse {
    match state.service.test_proxy_resource(id, payload).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// PUT /api/admin/proxy-resources/:id
/// 更新代理/家宽资源
pub async fn update_proxy_resource(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<UpdateProxyResourceRequest>,
) -> impl IntoResponse {
    match state.service.update_proxy_resource(id, payload) {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// DELETE /api/admin/proxy-resources/:id
/// 删除代理/家宽资源
pub async fn delete_proxy_resource(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.delete_proxy_resource(id) {
        Ok(_) => Json(SuccessResponse::new(format!("代理资源 #{} 已删除", id))).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/proxy
/// 设置凭据代理绑定
pub async fn set_credential_proxy(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetCredentialProxyRequest>,
) -> impl IntoResponse {
    match state.service.set_credential_proxy(id, payload) {
        Ok(_) => Json(SuccessResponse::new(format!("凭据 #{} 代理已更新", id))).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials
/// 添加新凭据
pub async fn add_credential(
    State(state): State<AdminState>,
    Json(payload): Json<AddCredentialRequest>,
) -> impl IntoResponse {
    match state.service.add_credential(payload).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

pub async fn batch_import_credentials(
    State(state): State<AdminState>,
    Json(payload): Json<BatchCredentialImportRequest>,
) -> impl IntoResponse {
    match state.service.batch_import_credentials(payload).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

pub async fn batch_update_credentials(
    State(state): State<AdminState>,
    Json(payload): Json<BatchUpdateCredentialsRequest>,
) -> impl IntoResponse {
    match state.service.batch_update_credentials(payload) {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

pub async fn update_credential_auth(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<UpdateCredentialAuthRequest>,
) -> impl IntoResponse {
    match state.service.update_credential_auth(id, payload) {
        Ok(_) => Json(SuccessResponse::new(format!("凭据 #{} 认证信息已更新", id))).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// DELETE /api/admin/credentials/:id
/// 删除凭据
pub async fn delete_credential(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.delete_credential(id) {
        Ok(_) => Json(SuccessResponse::new(format!("凭据 #{} 已删除", id))).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/refresh
/// 强制刷新凭据 Token
pub async fn force_refresh_token(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.force_refresh_token(id).await {
        Ok(_) => Json(SuccessResponse::new(format!(
            "凭据 #{} Token 已强制刷新",
            id
        )))
        .into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/config/load-balancing
/// 获取负载均衡模式
pub async fn get_load_balancing_mode(State(state): State<AdminState>) -> impl IntoResponse {
    let response = state.service.get_load_balancing_mode();
    Json(response)
}

/// PUT /api/admin/config/load-balancing
/// 设置负载均衡模式
pub async fn set_load_balancing_mode(
    State(state): State<AdminState>,
    Json(payload): Json<SetLoadBalancingModeRequest>,
) -> impl IntoResponse {
    match state.service.set_load_balancing_mode(payload) {
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

pub async fn get_external_pools(State(state): State<AdminState>) -> impl IntoResponse {
    match state.service.list_external_pools() {
        Ok(pools) => Json(serde_json::json!({ "pools": pools })).into_response(),
        Err(err) => (err.status_code(), Json(err.into_response())).into_response(),
    }
}

pub async fn create_external_pool(
    State(state): State<AdminState>,
    Json(payload): Json<CreateExternalPoolRequest>,
) -> impl IntoResponse {
    match state.service.create_external_pool(payload) {
        Ok(pool) => Json(pool).into_response(),
        Err(err) => (err.status_code(), Json(err.into_response())).into_response(),
    }
}

pub async fn update_external_pool(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<UpdateExternalPoolRequest>,
) -> impl IntoResponse {
    match state.service.update_external_pool(id, payload) {
        Ok(pool) => Json(pool).into_response(),
        Err(err) => (err.status_code(), Json(err.into_response())).into_response(),
    }
}

pub async fn delete_external_pool(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.delete_external_pool(id) {
        Ok(()) => Json(SuccessResponse::new("外部池已删除")).into_response(),
        Err(err) => (err.status_code(), Json(err.into_response())).into_response(),
    }
}

pub async fn set_external_pool_enabled(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<SetExternalPoolEnabledRequest>,
) -> impl IntoResponse {
    match state.service.set_external_pool_enabled(id, payload) {
        Ok(pool) => Json(pool).into_response(),
        Err(err) => (err.status_code(), Json(err.into_response())).into_response(),
    }
}

pub async fn clear_external_pool_auto_disabled(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.clear_external_pool_auto_disabled(id) {
        Ok(pool) => Json(pool).into_response(),
        Err(err) => (err.status_code(), Json(err.into_response())).into_response(),
    }
}

pub async fn clear_external_pool_cooldown(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.clear_external_pool_cooldown(id) {
        Ok(pool) => Json(pool).into_response(),
        Err(err) => (err.status_code(), Json(err.into_response())).into_response(),
    }
}

pub async fn get_external_pool_status(State(state): State<AdminState>) -> impl IntoResponse {
    match state.service.get_external_pool_status() {
        Ok(status) => Json(status).into_response(),
        Err(err) => (err.status_code(), Json(err.into_response())).into_response(),
    }
}

pub async fn test_external_pool(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    payload: Option<Json<ExternalPoolTestRequest>>,
) -> impl IntoResponse {
    match state
        .service
        .test_external_pool(id, payload.map(|Json(payload)| payload))
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
/// 获取 Kiro 模型能力同步状态
pub async fn get_model_capabilities(State(state): State<AdminState>) -> impl IntoResponse {
    Json(state.service.get_model_capabilities())
}

/// POST /api/admin/model-capabilities/sync
/// 手动同步 Kiro 模型能力
pub async fn sync_model_capabilities(State(state): State<AdminState>) -> impl IntoResponse {
    Json(state.service.sync_model_capabilities().await)
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

fn sensitive_export_response_builder() -> axum::http::response::Builder {
    Response::builder()
        .header(header::CACHE_CONTROL, "no-store, private")
        .header(header::PRAGMA, "no-cache")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
}

fn parse_export_credential_ids(raw: Option<&str>) -> Result<Vec<u64>, String> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    let mut ids = Vec::new();
    for part in trimmed.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let id = part
            .parse::<u64>()
            .map_err(|_| format!("无效的导出凭据 ID: {}", part))?;
        ids.push(id);
    }
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

/// GET /api/admin/credentials/export?format=json|backup-json|jsonl&ids=1,2,3
/// 导出完整凭据。ids 为空时导出全部；ids 非空时仅导出选中凭据。
pub async fn export_credentials(
    State(state): State<AdminState>,
    Query(query): Query<ExportCredentialsQuery>,
) -> Response {
    let format = query.format.as_deref().unwrap_or("json");
    let selected_ids = match parse_export_credential_ids(query.ids.as_deref()) {
        Ok(ids) => ids,
        Err(message) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(AdminErrorResponse::invalid_request(message)),
            )
                .into_response();
        }
    };
    match state.service.export_credentials(format, &selected_ids) {
        Ok((body, filename)) => {
            let content_type = if filename.ends_with(".jsonl") {
                "application/x-ndjson; charset=utf-8"
            } else {
                "application/json; charset=utf-8"
            };
            sensitive_export_response_builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, content_type)
                .header(
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{}\"", filename),
                )
                .body(Body::from(body))
                .expect("valid credentials export response")
        }
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

/// GET /api/admin/usage-dashboard/accounts
/// 获取全量本地账号窗口统计与运行态快照（分页）。
pub async fn get_usage_dashboard_accounts(
    State(state): State<AdminState>,
    Query(params): Query<UsageDashboardAccountsQueryParams>,
) -> impl IntoResponse {
    Json(state.service.get_usage_dashboard_accounts(
        params.timezone,
        params.window_key,
        params.page.unwrap_or(1),
        params.page_size.unwrap_or(50),
        params.q,
        params.status,
        params.sort_by,
        params.sort_order,
    ))
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

/// GET /api/admin/usage-dashboard/external-pool-billing
/// 获取 usage 仪表盘单个窗口的外部池费用明细。
pub async fn get_usage_dashboard_external_pool_billing(
    State(state): State<AdminState>,
    Query(params): Query<UsageDashboardWindowQueryParams>,
) -> impl IntoResponse {
    let window_key = params.window_key.unwrap_or_else(|| "today".to_string());
    match state
        .service
        .get_usage_dashboard_external_pool_billing(params.timezone, window_key)
    {
        Ok(data) => Json(data).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// GET /api/admin/usage-dashboard/external-pool-risk
/// 获取外部池 usage 风控统计。
pub async fn get_usage_dashboard_external_pool_risk(
    State(state): State<AdminState>,
    Query(params): Query<UsageExternalPoolRiskQueryParams>,
) -> impl IntoResponse {
    match params.into_query() {
        Ok(query) => match state.service.get_usage_dashboard_external_pool_risk(query) {
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
    Json(
        state
            .service
            .get_audit_logs(params.page.unwrap_or(1), params.limit.unwrap_or(20)),
    )
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
            external_pool_id: None,
            route_kind: Some("external".to_string()),
            model: Some("".to_string()),
            status: Some("".to_string()),
            source: Some("   ".to_string()),
            endpoint: Some("   ".to_string()),
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
        assert_eq!(query.route_kind, Some(UsageRouteKind::ExternalPool));
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
            external_pool_id: None,
            route_kind: None,
            model: None,
            status: None,
            source: None,
            endpoint: None,
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
            external_pool_id: None,
            route_kind: None,
            model: None,
            status: None,
            source: None,
            endpoint: None,
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
            external_pool_id: None,
            route_kind: None,
            model: None,
            status: Some("ok".to_string()),
            source: None,
            endpoint: None,
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
            external_pool_id: None,
            route_kind: None,
            model: None,
            status: None,
            source: Some("cache".to_string()),
            endpoint: None,
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
            external_pool_id: None,
            route_kind: None,
            model: None,
            status: None,
            source: None,
            endpoint: None,
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
            external_pool_id: None,
            route_kind: None,
            model: None,
            status: None,
            source: None,
            endpoint: Some("/ha".to_string()),
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

    #[test]
    fn sensitive_credential_export_is_not_cacheable_or_content_sniffable() {
        let response = sensitive_export_response_builder()
            .status(StatusCode::OK)
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store, private")
        );
        assert_eq!(
            response
                .headers()
                .get(header::PRAGMA)
                .and_then(|value| value.to_str().ok()),
            Some("no-cache")
        );
        assert_eq!(
            response
                .headers()
                .get(header::X_CONTENT_TYPE_OPTIONS)
                .and_then(|value| value.to_str().ok()),
            Some("nosniff")
        );
    }

    #[test]
    fn export_credential_ids_query_parses_and_deduplicates_ids() {
        assert_eq!(
            parse_export_credential_ids(None).unwrap(),
            Vec::<u64>::new()
        );
        assert_eq!(
            parse_export_credential_ids(Some("")).unwrap(),
            Vec::<u64>::new()
        );
        assert_eq!(
            parse_export_credential_ids(Some("3, 1,3,2")).unwrap(),
            vec![1, 2, 3]
        );
        assert!(parse_export_credential_ids(Some("1,nope")).is_err());
    }
}
