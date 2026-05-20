//! Admin API HTTP 处理器

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;

use super::{
    middleware::AdminState,
    types::{
        AddCredentialRequest, AdminErrorResponse, CredentialTestRequest, SetDisabledRequest,
        SetLoadBalancingModeRequest, SetPriorityRequest, SuccessResponse,
    },
};
use crate::anthropic::usage::{UsageRecordQuery, UsageRecordStatus, UsageSource};

/// 写一条管理员操作审计(失败仅打日志,不影响主流程)
async fn record_admin_action(
    db: &crate::storage::Db,
    actor: &str,
    action: &str,
    target_type: Option<&str>,
    target_id: Option<String>,
    payload: Option<serde_json::Value>,
) {
    let res = sqlx::query(
        "INSERT INTO admin_actions (actor, action, target_type, target_id, payload) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(actor)
    .bind(action)
    .bind(target_type)
    .bind(target_id)
    .bind(payload)
    .execute(db)
    .await;
    if let Err(err) = res {
        tracing::warn!("写 admin_actions 失败: {:#}", err);
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialsPageQueryParams {
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
    pub model: Option<String>,
    pub status: Option<String>,
    pub source: Option<String>,
    pub stream: Option<bool>,
    pub min_cache_read: Option<i32>,
    pub since: Option<String>,
    pub until: Option<String>,
}

impl UsageRecordsQueryParams {
    fn into_query(self) -> Result<UsageRecordQuery, String> {
        Ok(UsageRecordQuery {
            limit: self.limit.unwrap_or_default(),
            q: non_blank(self.q),
            conversation_id: non_blank(self.conversation_id),
            credential_id: self.credential_id,
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
    let response = state.service.get_all_credentials().await;
    Json(response)
}

/// GET /api/admin/credentials-paged
/// 分页获取凭据状态
pub async fn get_credentials_page(
    State(state): State<AdminState>,
    Query(params): Query<CredentialsPageQueryParams>,
) -> impl IntoResponse {
    Json(
        state
            .service
            .get_credentials_page(
                params.page.unwrap_or_default(),
                params.limit.unwrap_or_default(),
            )
            .await,
    )
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
            record_admin_action(
                &state.db,
                "admin",
                if payload.disabled {
                    "credential_disable"
                } else {
                    "credential_enable"
                },
                Some("credential"),
                Some(id.to_string()),
                Some(serde_json::json!({ "disabled": payload.disabled })),
            )
            .await;
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
        Ok(_) => {
            record_admin_action(
                &state.db,
                "admin",
                "credential_set_priority",
                Some("credential"),
                Some(id.to_string()),
                Some(serde_json::json!({ "priority": payload.priority })),
            )
            .await;
            Json(SuccessResponse::new(format!(
                "凭据 #{} 优先级已设置为 {}",
                id, payload.priority
            )))
            .into_response()
        }
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
/// 获取指定凭据的余额
pub async fn get_credential_balance(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.get_balance(id).await {
        Ok(response) => Json(response).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

/// POST /api/admin/credentials/:id/test
/// 使用指定凭据和模型发起一次测试调用
pub async fn test_credential(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
    Json(payload): Json<CredentialTestRequest>,
) -> impl IntoResponse {
    let model = payload.model.clone();
    match state.service.test_credential(id, payload).await {
        Ok(response) => {
            record_admin_action(
                &state.db,
                "admin",
                "credential_test",
                Some("credential"),
                Some(id.to_string()),
                Some(serde_json::json!({
                    "model": model,
                    "success": response.success,
                    "statusCode": response.status_code,
                    "errorType": response.error_type,
                    "durationMs": response.duration_ms,
                })),
            )
            .await;
            Json(response).into_response()
        }
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

/// DELETE /api/admin/credentials/:id
/// 删除凭据
pub async fn delete_credential(
    State(state): State<AdminState>,
    Path(id): Path<u64>,
) -> impl IntoResponse {
    match state.service.delete_credential(id) {
        Ok(_) => {
            record_admin_action(
                &state.db,
                "admin",
                "credential_delete",
                Some("credential"),
                Some(id.to_string()),
                None,
            )
            .await;
            Json(SuccessResponse::new(format!("凭据 #{} 已删除", id))).into_response()
        }
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
        Ok(_) => {
            record_admin_action(
                &state.db,
                "admin",
                "credential_force_refresh",
                Some("credential"),
                Some(id.to_string()),
                None,
            )
            .await;
            Json(SuccessResponse::new(format!(
                "凭据 #{} Token 已强制刷新",
                id
            )))
            .into_response()
        }
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
    match state.service.set_load_balancing_mode(payload).await {
        Ok(response) => {
            record_admin_action(
                &state.db,
                "admin",
                "config_update",
                Some("app_config"),
                Some("load_balancing_mode".to_string()),
                Some(serde_json::json!({ "load_balancing_mode": response.mode })),
            )
            .await;
            Json(response).into_response()
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
        Ok(query) => Json(state.service.get_usage_records(query).await).into_response(),
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
        Ok((query, page, limit)) => Json(
            state
                .service
                .get_usage_records_page(query, page, limit)
                .await,
        )
        .into_response(),
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

/// POST /api/admin/usage-records/clear
/// 清空请求级 usage 记录
pub async fn clear_usage_records(State(state): State<AdminState>) -> impl IntoResponse {
    match state.service.clear_usage_records().await {
        Ok(deleted) => {
            record_admin_action(
                &state.db,
                "admin",
                "usage_clear",
                Some("usage"),
                None,
                Some(serde_json::json!({ "deletedUsageRecords": deleted })),
            )
            .await;
            Json(SuccessResponse::new(format!(
                "Usage 记录已清空，PG 删除 {} 条",
                deleted
            )))
            .into_response()
        }
        Err(err) => {
            tracing::error!("清空 usage_records 失败: {:#}", err);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AdminErrorResponse::internal_error("清空 Usage 记录失败")),
            )
                .into_response()
        }
    }
}

/// GET /api/admin/usage-stats
/// 后端 SQL 聚合的 today / 累计 / 范围内 / 时间序列 / 按模型 / 按账号 统计。
/// 支持过滤参数(q / model / credentialId / status / source / stream / minCacheRead / conversationId)
/// + 时间范围 since / until + bucket(hour / day);默认 since = 今日 0 点。
pub async fn get_usage_stats(
    State(state): State<AdminState>,
    Query(params): Query<UsageStatsQueryParams>,
) -> impl IntoResponse {
    use crate::anthropic::usage::UsageStatsFilter;
    let status = match parse_optional_usage_status(params.status.clone()) {
        Ok(v) => v,
        Err(msg) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(AdminErrorResponse::invalid_request(msg)),
            )
                .into_response();
        }
    };
    let source = match parse_optional_usage_source(params.source.clone()) {
        Ok(v) => v,
        Err(msg) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(AdminErrorResponse::invalid_request(msg)),
            )
                .into_response();
        }
    };
    let since = match parse_optional_time(params.since) {
        Ok(v) => v,
        Err(msg) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(AdminErrorResponse::invalid_request(msg)),
            )
                .into_response();
        }
    };
    let until = match parse_optional_time(params.until) {
        Ok(v) => v,
        Err(msg) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(AdminErrorResponse::invalid_request(msg)),
            )
                .into_response();
        }
    };
    let bucket = match params.bucket.as_deref() {
        None | Some("") => None,
        Some("hour") => Some("hour".to_string()),
        Some("day") => Some("day".to_string()),
        Some(other) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(AdminErrorResponse::invalid_request(format!(
                    "无效 bucket: {}(只接受 hour / day)",
                    other
                ))),
            )
                .into_response();
        }
    };
    let filter = UsageStatsFilter {
        q: non_blank(params.q),
        conversation_id: non_blank(params.conversation_id),
        credential_id: params.credential_id,
        model: non_blank(params.model),
        status: status.map(|s| {
            match s {
                UsageRecordStatus::Success => "success",
                UsageRecordStatus::Error => "error",
                UsageRecordStatus::StreamError => "stream_error",
                UsageRecordStatus::UpstreamTimeout => "upstream_timeout",
                UsageRecordStatus::ClientDropped => "client_dropped",
            }
            .to_string()
        }),
        source: source.map(|s| {
            match s {
                UsageSource::UpstreamMetadata => "upstream_metadata",
                UsageSource::LocalPromptCache => "local_prompt_cache",
                UsageSource::ContextEstimate => "context_estimate",
                UsageSource::RequestEstimate => "request_estimate",
                UsageSource::None => "none",
            }
            .to_string()
        }),
        stream: params.stream,
        min_cache_read: params.min_cache_read,
        since,
        until,
        bucket,
    };
    match state.service.get_usage_stats(&state.db, &filter).await {
        Ok(stats) => Json(stats).into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(AdminErrorResponse::internal_error(err.to_string())),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageStatsQueryParams {
    pub q: Option<String>,
    pub conversation_id: Option<String>,
    pub credential_id: Option<u64>,
    pub model: Option<String>,
    pub status: Option<String>,
    pub source: Option<String>,
    pub stream: Option<bool>,
    pub min_cache_read: Option<i32>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub bucket: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct QuotaEventsQuery {
    #[serde(default)]
    pub credential_id: Option<u64>,
    #[serde(default)]
    pub limit: Option<i64>,
}

/// GET /api/admin/quota-events
/// 配额事件历史(soft_402 / hard_disabled / cooldown_recovered / manual_reset)
pub async fn list_quota_events(
    State(state): State<AdminState>,
    Query(params): Query<QuotaEventsQuery>,
) -> impl IntoResponse {
    let limit = params.limit.unwrap_or(50);
    match state
        .service
        .list_quota_events(params.credential_id, limit)
        .await
    {
        Ok(items) => Json(items).into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(AdminErrorResponse::internal_error(err.to_string())),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AdminActionsQuery {
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub target_type: Option<String>,
}

/// GET /api/admin/admin-actions
/// 管理员操作审计历史(自 v2026.4)
pub async fn list_admin_actions(
    State(state): State<AdminState>,
    Query(params): Query<AdminActionsQuery>,
) -> impl IntoResponse {
    use sqlx::Row;
    let limit = params.limit.unwrap_or(100).clamp(1, 500);
    let action_filter = params.action.filter(|s| !s.trim().is_empty());
    let target_filter = params.target_type.filter(|s| !s.trim().is_empty());

    let res = sqlx::query(
        "SELECT id, occurred_at, actor, action, target_type, target_id, payload, note \
         FROM admin_actions \
         WHERE ($1::text IS NULL OR action = $1) \
           AND ($2::text IS NULL OR target_type = $2) \
         ORDER BY occurred_at DESC LIMIT $3",
    )
    .bind(&action_filter)
    .bind(&target_filter)
    .bind(limit)
    .fetch_all(&state.db)
    .await;

    match res {
        Ok(rows) => {
            let items: Vec<serde_json::Value> = rows
                .into_iter()
                .map(|r| {
                    serde_json::json!({
                        "id": r.try_get::<i64, _>("id").unwrap_or(0),
                        "occurredAt": r.try_get::<chrono::DateTime<chrono::Utc>, _>("occurred_at")
                            .map(|d| d.to_rfc3339())
                            .unwrap_or_default(),
                        "actor": r.try_get::<String, _>("actor").unwrap_or_default(),
                        "action": r.try_get::<String, _>("action").unwrap_or_default(),
                        "targetType": r.try_get::<Option<String>, _>("target_type").ok().flatten(),
                        "targetId": r.try_get::<Option<String>, _>("target_id").ok().flatten(),
                        "payload": r.try_get::<Option<serde_json::Value>, _>("payload").ok().flatten(),
                        "note": r.try_get::<Option<String>, _>("note").ok().flatten(),
                    })
                })
                .collect();
            Json(items).into_response()
        }
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(AdminErrorResponse::internal_error(err.to_string())),
        )
            .into_response(),
    }
}

// ============ 在线配置 (app_config) ============

/// GET /api/admin/config
/// 列出所有运行时配置项
pub async fn list_app_config(State(state): State<AdminState>) -> impl IntoResponse {
    match state.app_config.list().await {
        Ok(items) => Json(items).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(AdminErrorResponse::internal_error(e.to_string())),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct AppConfigUpdate {
    pub items: std::collections::HashMap<String, serde_json::Value>,
}

/// PUT /api/admin/config
/// 批量更新配置项(只允许白名单 key)
pub async fn update_app_config(
    State(state): State<AdminState>,
    Json(payload): Json<AppConfigUpdate>,
) -> impl IntoResponse {
    let items: Vec<(String, serde_json::Value)> = payload.items.into_iter().collect();
    for (key, value) in &items {
        match key.as_str() {
            "load_balancing_mode" => match value.as_str() {
                Some("priority" | "balanced") => {}
                _ => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(AdminErrorResponse::invalid_request(
                            "load_balancing_mode 必须是 priority 或 balanced",
                        )),
                    )
                        .into_response();
                }
            },
            "session_binding_ttl_minutes" => {
                if value.as_i64().is_none_or(|ttl| ttl < 1) {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(AdminErrorResponse::invalid_request(
                            "session_binding_ttl_minutes 必须是正整数",
                        )),
                    )
                        .into_response();
                }
            }
            _ => {}
        }
    }
    match state.app_config.set_many(&items, "admin").await {
        Ok(_) => {
            // 关键 key 同步推到运行时模块,避免改了不生效
            for (k, v) in &items {
                match k.as_str() {
                    "load_balancing_mode" => {
                        if let Some(m) = v.as_str() {
                            state.token_manager.override_load_balancing_mode(m);
                        }
                    }
                    "quota_soft_fail_limit" => {
                        let limit = v.as_u64().map(|x| x as u32).unwrap_or(3);
                        // cooldown 也一起拉一下,保持一致性
                        let cooldown = state
                            .app_config
                            .get_as::<i64>("quota_cooldown_minutes")
                            .unwrap_or(30);
                        state.token_manager.set_quota_settings(limit, cooldown);
                    }
                    "quota_cooldown_minutes" => {
                        let cooldown = v.as_i64().unwrap_or(30);
                        let limit = state
                            .app_config
                            .get_as::<u32>("quota_soft_fail_limit")
                            .unwrap_or(3);
                        state.token_manager.set_quota_settings(limit, cooldown);
                    }
                    "session_binding_ttl_minutes" => {
                        let ttl = v.as_i64().unwrap_or(30);
                        state.token_manager.set_session_binding_ttl_minutes(ttl);
                    }
                    _ => {}
                }
            }
            state
                .prompt_cache_runtime_config
                .reload_from_app_config(&state.app_config);
            // 审计
            record_admin_action(
                &state.db,
                "admin",
                "config_update",
                Some("config"),
                None,
                Some(serde_json::json!({ "items": items })),
            )
            .await;
            Json(SuccessResponse::new(format!(
                "已更新 {} 项配置",
                items.len()
            )))
            .into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(AdminErrorResponse::invalid_request(e.to_string())),
        )
            .into_response(),
    }
}

// ============ 模型计价 ============

/// GET /api/admin/pricing
/// 列出所有模型价格
pub async fn list_pricing(State(state): State<AdminState>) -> impl IntoResponse {
    match state.pricing.list().await {
        Ok(items) => Json(items).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(AdminErrorResponse::internal_error(e.to_string())),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SyncPricingRequest {
    /// 强制使用内置快照而不是 LiteLLM
    #[serde(default)]
    pub force_builtin: bool,
}

/// POST /api/admin/pricing/sync
/// 立即同步模型价格(从 LiteLLM 或内置快照)
pub async fn sync_pricing(
    State(state): State<AdminState>,
    payload: Option<Json<SyncPricingRequest>>,
) -> impl IntoResponse {
    let force_builtin = payload.map(|p| p.0.force_builtin).unwrap_or(false);
    match state.pricing.sync(force_builtin).await {
        Ok(summary) => {
            // 同步成功后标记 bootstrap 完成,前端可据此显示同步时间
            let _ = state
                .app_config
                .set(
                    "pricing_bootstrap_done",
                    serde_json::Value::Bool(true),
                    "admin",
                )
                .await;
            // 刷新内存缓存,后续 record 计算 cost_usd 立即生效
            if let Err(err) = state.pricing.warm_cache().await {
                tracing::warn!("warm pricing cache 失败: {:#}", err);
            }
            record_admin_action(
                &state.db,
                "admin",
                "pricing_sync",
                Some("pricing"),
                None,
                Some(serde_json::json!({
                    "source": summary.source,
                    "fetchedCount": summary.fetched_count,
                    "upserted": summary.upserted,
                    "usedFallback": summary.used_fallback,
                })),
            )
            .await;
            Json(summary).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(AdminErrorResponse::internal_error(e.to_string())),
        )
            .into_response(),
    }
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
