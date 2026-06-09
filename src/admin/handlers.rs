//! Admin API HTTP 处理器

use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::{
    middleware::AdminState,
    types::{
        AddCredentialRequest, AdminErrorResponse, BatchCredentialImportRequest,
        ClearInFlightRequest, CreateProxyResourceRequest, ExportCredentialsQuery,
        ExternalPoolTestRequest, RefreshCredentialInfoRequest, SetCredentialConcurrencyRequest,
        SetCredentialProxyRequest, SetDisabledRequest, SetLoadBalancingModeRequest,
        SetPriorityRequest, SetWarmupRequest, SuccessResponse, TestCredentialRequest,
        UpdateAdminApiKeyRequest, UpdateCredentialAuthRequest, UpdateProxyResourceRequest,
        UpdateRuntimeConfigRequest, UpsertManualModelRequest, UsageCleanupRequest,
        ValidateExistingCredentialsRequest, ValidateExternalCredentialsRequest,
    },
};
use crate::anthropic::usage::{UsageRecordQuery, UsageRecordStatus, UsageSource};
use crate::external_pool::{
    CreateExternalPoolRequest, SetExternalPoolEnabledRequest, UpdateExternalPoolRequest,
};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialsPageQueryParams {
    pub page: Option<usize>,
    pub limit: Option<usize>,
    pub q: Option<String>,
    pub status: Option<String>,
    pub auth_method: Option<String>,
    pub subscription: Option<String>,
    pub proxy_resource_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialInfoQueryParams {
    pub force: Option<bool>,
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
    pub q: Option<String>,
    pub conversation_id: Option<String>,
    pub credential_id: Option<u64>,
    pub external_pool_id: Option<u64>,
    pub model: Option<String>,
    pub status: Option<String>,
    pub source: Option<String>,
    pub stream: Option<bool>,
    pub min_cache_read: Option<i32>,
    pub since: Option<String>,
    pub until: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecordsPageQueryParams {
    pub page: Option<usize>,
    pub limit: Option<usize>,
    pub q: Option<String>,
    pub conversation_id: Option<String>,
    pub credential_id: Option<u64>,
    pub external_pool_id: Option<u64>,
    pub model: Option<String>,
    pub status: Option<String>,
    pub source: Option<String>,
    pub stream: Option<bool>,
    pub min_cache_read: Option<i32>,
    pub since: Option<String>,
    pub until: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDashboardQueryParams {
    pub timezone: Option<String>,
}

impl UsageRecordsQueryParams {
    fn into_query(self) -> Result<UsageRecordQuery, String> {
        Ok(UsageRecordQuery {
            limit: self.limit.unwrap_or_default(),
            q: non_blank(self.q),
            conversation_id: non_blank(self.conversation_id),
            credential_id: self.credential_id,
            external_pool_id: self.external_pool_id,
            model: non_blank(self.model),
            status: parse_optional_usage_status(self.status)?,
            source: parse_optional_usage_source(self.source)?,
            stream: self.stream,
            min_cache_read: self.min_cache_read,
            since: parse_optional_time(self.since)?,
            until: parse_optional_time(self.until)?,
        })
    }
}

impl UsageRecordsPageQueryParams {
    fn into_query(self) -> Result<(UsageRecordQuery, usize, usize), String> {
        let page = self.page.unwrap_or_default();
        let limit = self.limit.unwrap_or_default();
        let query = UsageRecordQuery {
            limit,
            q: non_blank(self.q),
            conversation_id: non_blank(self.conversation_id),
            credential_id: self.credential_id,
            external_pool_id: self.external_pool_id,
            model: non_blank(self.model),
            status: parse_optional_usage_status(self.status)?,
            source: parse_optional_usage_source(self.source)?,
            stream: self.stream,
            min_cache_read: self.min_cache_read,
            since: parse_optional_time(self.since)?,
            until: parse_optional_time(self.until)?,
        };
        Ok((query, page, limit))
    }
}

fn non_blank(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

fn parse_optional_usage_status(value: Option<String>) -> Result<Option<UsageRecordStatus>, String> {
    let value = non_blank(value);
    value.as_deref().map(parse_usage_status).transpose()
}

fn parse_optional_usage_source(value: Option<String>) -> Result<Option<UsageSource>, String> {
    let value = non_blank(value);
    value.as_deref().map(parse_usage_source).transpose()
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
            status: non_blank(params.status),
            auth_method: non_blank(params.auth_method),
            subscription: non_blank(params.subscription),
            proxy_resource_id: params.proxy_resource_id,
        },
    ))
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

/// GET /api/admin/credentials/export?format=json|backup-json|jsonl
/// 导出完整凭据。
pub async fn export_credentials(
    State(state): State<AdminState>,
    Query(query): Query<ExportCredentialsQuery>,
) -> Response {
    let format = query.format.as_deref().unwrap_or("json");
    match state.service.export_credentials(format) {
        Ok((body, filename)) => {
            let content_type = if filename.ends_with(".jsonl") {
                "application/x-ndjson; charset=utf-8"
            } else {
                "application/json; charset=utf-8"
            };
            Response::builder()
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

/// GET /api/admin/usage-writer-stats
/// 获取 usage 持久化 writer 观测状态。
pub async fn get_usage_writer_stats(State(state): State<AdminState>) -> impl IntoResponse {
    Json(state.service.get_usage_writer_stats())
}

/// POST /api/admin/usage-records/clear
/// 清空请求级 usage 记录
pub async fn clear_usage_records(State(state): State<AdminState>) -> impl IntoResponse {
    state.service.clear_usage_records();
    Json(SuccessResponse::new("Usage 记录已清空"))
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
        Ok(data) => Json(data).into_response(),
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
    Json(state.service.cancel_usage_cleanup())
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
            q: Some("   ".to_string()),
            conversation_id: Some("   ".to_string()),
            credential_id: Some(7),
            external_pool_id: None,
            model: Some("".to_string()),
            status: Some("".to_string()),
            source: Some("   ".to_string()),
            stream: Some(true),
            min_cache_read: Some(10_000),
            since: None,
            until: None,
        }
        .into_query()
        .expect("valid query");

        assert_eq!(query.limit, 25);
        assert_eq!(query.q, None);
        assert_eq!(query.conversation_id, None);
        assert_eq!(query.credential_id, Some(7));
        assert_eq!(query.model, None);
        assert_eq!(query.status, None);
        assert_eq!(query.source, None);
        assert_eq!(query.stream, Some(true));
        assert_eq!(query.min_cache_read, Some(10_000));
    }

    #[test]
    fn usage_records_query_rejects_invalid_enums_and_time() {
        let bad_status = UsageRecordsQueryParams {
            limit: None,
            q: None,
            conversation_id: None,
            credential_id: None,
            external_pool_id: None,
            model: None,
            status: Some("ok".to_string()),
            source: None,
            stream: None,
            min_cache_read: None,
            since: None,
            until: None,
        }
        .into_query()
        .unwrap_err();
        assert!(bad_status.contains("无效 status"));

        let bad_source = UsageRecordsQueryParams {
            limit: None,
            q: None,
            conversation_id: None,
            credential_id: None,
            external_pool_id: None,
            model: None,
            status: None,
            source: Some("cache".to_string()),
            stream: None,
            min_cache_read: None,
            since: None,
            until: None,
        }
        .into_query()
        .unwrap_err();
        assert!(bad_source.contains("无效 source"));

        let bad_time = UsageRecordsQueryParams {
            limit: None,
            q: None,
            conversation_id: None,
            credential_id: None,
            external_pool_id: None,
            model: None,
            status: None,
            source: None,
            stream: None,
            min_cache_read: None,
            since: Some("not-a-time".to_string()),
            until: None,
        }
        .into_query()
        .unwrap_err();
        assert!(bad_time.contains("无效时间"));
    }

    #[test]
    fn usage_records_page_query_keeps_pagination_separate_from_filters() {
        let (query, page, limit) = UsageRecordsPageQueryParams {
            page: Some(3),
            limit: Some(50),
            q: Some("sonnet".to_string()),
            conversation_id: Some("session-a".to_string()),
            credential_id: None,
            external_pool_id: None,
            model: None,
            status: None,
            source: None,
            stream: None,
            min_cache_read: None,
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
    }
}
