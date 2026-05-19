//! Admin API 业务逻辑服务

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::anthropic::{
    PromptCacheRuntimeConfig, map_model,
    prompt_cache::PromptCacheTracker,
    usage::{
        UsageRecordQuery, UsageRecorder, UsageRecordsPageResult, UsageRecordsResult, UsageSummary,
    },
};
use crate::app_config::AppConfigService;
use crate::kiro::model::credentials::KiroCredentials;
use crate::kiro::model::events::Event;
use crate::kiro::model::requests::{
    conversation::{ConversationState, CurrentMessage, UserInputMessage},
    kiro::KiroRequest,
};
use crate::kiro::parser::decoder::EventStreamDecoder;
use crate::kiro::provider::KiroProvider;
use crate::kiro::token_manager::MultiTokenManager;
use crate::storage::{Db, RedisPool};

use super::error::AdminServiceError;
use super::types::{
    AddCredentialRequest, AddCredentialResponse, BalanceResponse, CredentialStatusItem,
    CredentialTestRequest, CredentialTestResponse, CredentialsPageResponse,
    CredentialsStatusResponse, LoadBalancingModeResponse, SetLoadBalancingModeRequest,
};

const DEFAULT_BALANCE_CACHE_TTL_SECS: i64 = 300;
const DEFAULT_CREDENTIALS_PAGE_LIMIT: usize = 12;
const MAX_CREDENTIALS_PAGE_LIMIT: usize = 500;
const DEFAULT_CREDENTIAL_TEST_PROMPT: &str = "Reply with OK.";
const MAX_TEST_PREVIEW_CHARS: usize = 4000;

/// Redis 中存储的余额条目(JSON)
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedBalance {
    /// 缓存时间(Unix 秒,用于前端展示"几分钟前查询")
    cached_at: f64,
    /// 缓存的余额数据
    data: BalanceResponse,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaEventItem {
    pub id: i64,
    pub credential_id: i64,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    pub kind: String,
    pub reason: Option<String>,
    pub upstream_status: Option<i16>,
    pub cooldown_until: Option<chrono::DateTime<chrono::Utc>>,
    pub note: Option<String>,
}

fn balance_cache_key(id: u64) -> String {
    format!("kiro_rs:balance:{}", id)
}

/// Admin 服务
///
/// 封装所有 Admin API 的业务逻辑
pub struct AdminService {
    token_manager: Arc<MultiTokenManager>,
    kiro_provider: Arc<KiroProvider>,
    /// Redis 句柄,用于 balance 缓存
    redis: RedisPool,
    /// app_config 句柄,用于读取 balance_cache_ttl_seconds
    app_config: Arc<AppConfigService>,
    /// PG 句柄,用于 quota_events 查询
    db: Db,
    /// 已注册的端点名称集合(用于 add_credential 校验)
    known_endpoints: HashSet<String>,
    usage_recorder: Arc<UsageRecorder>,
    prompt_cache: Arc<PromptCacheTracker>,
    prompt_cache_runtime_config: Arc<PromptCacheRuntimeConfig>,
}

impl AdminService {
    pub fn new(
        token_manager: Arc<MultiTokenManager>,
        kiro_provider: Arc<KiroProvider>,
        known_endpoints: impl IntoIterator<Item = String>,
        usage_recorder: Arc<UsageRecorder>,
        prompt_cache: Arc<PromptCacheTracker>,
        prompt_cache_runtime_config: Arc<PromptCacheRuntimeConfig>,
        app_config: Arc<AppConfigService>,
        redis: RedisPool,
        db: Db,
    ) -> Self {
        Self {
            token_manager,
            kiro_provider,
            redis,
            app_config,
            db,
            known_endpoints: known_endpoints.into_iter().collect(),
            usage_recorder,
            prompt_cache,
            prompt_cache_runtime_config,
        }
    }

    /// 失效指定凭据的余额缓存(spawn 一个后台任务做 DEL,失败仅打日志)
    fn invalidate_balance_cache(&self, id: u64) {
        let me_redis = self.redis.clone();
        tokio::spawn(async move {
            let key = balance_cache_key(id);
            match me_redis.get().await {
                Ok(mut conn) => {
                    let res: Result<(), _> =
                        ::redis::cmd("DEL").arg(&key).query_async(&mut *conn).await;
                    if let Err(err) = res {
                        tracing::warn!("Redis DEL {} 失败: {}", key, err);
                    }
                }
                Err(err) => tracing::warn!("Redis 取连接失败,跳过缓存失效: {}", err),
            }
        });
    }

    /// 获取所有凭据状态
    pub fn get_all_credentials(&self) -> CredentialsStatusResponse {
        let (total, available, current_id, credentials) = self.credential_status_items();

        CredentialsStatusResponse {
            total,
            available,
            current_id,
            credentials,
        }
    }

    /// 分页获取凭据状态。
    pub fn get_credentials_page(&self, page: usize, limit: usize) -> CredentialsPageResponse {
        let page = normalize_page(page);
        let limit = normalize_credentials_limit(limit);
        let (total, available, current_id, credentials) = self.credential_status_items();
        let total_pages = total_pages(total, limit);
        let start = page.saturating_sub(1).saturating_mul(limit);
        let credentials = credentials.into_iter().skip(start).take(limit).collect();

        CredentialsPageResponse {
            total,
            available,
            current_id,
            page,
            limit,
            total_pages,
            credentials,
        }
    }

    fn credential_status_items(&self) -> (usize, usize, u64, Vec<CredentialStatusItem>) {
        let snapshot = self.token_manager.snapshot();
        let default_endpoint = self.token_manager.config().default_endpoint.clone();

        let mut credentials: Vec<CredentialStatusItem> = snapshot
            .entries
            .into_iter()
            .map(|entry| CredentialStatusItem {
                id: entry.id,
                priority: entry.priority,
                disabled: entry.disabled,
                failure_count: entry.failure_count,
                is_current: entry.id == snapshot.current_id,
                expires_at: entry.expires_at,
                auth_method: entry.auth_method,
                has_profile_arn: entry.has_profile_arn,
                refresh_token_hash: entry.refresh_token_hash,
                api_key_hash: entry.api_key_hash,
                masked_api_key: entry.masked_api_key,
                email: entry.email,
                success_count: entry.success_count,
                last_used_at: entry.last_used_at.clone(),
                has_proxy: entry.has_proxy,
                proxy_url: entry.proxy_url,
                refresh_failure_count: entry.refresh_failure_count,
                disabled_reason: entry.disabled_reason,
                endpoint: entry.endpoint.unwrap_or_else(|| default_endpoint.clone()),
                scheduling_status: entry.scheduling_status,
                scheduling_reason: entry.scheduling_reason,
                scheduling_until: entry.scheduling_until,
                last_upstream_status: entry.last_upstream_status,
                rate_limited_count: entry.rate_limited_count,
            })
            .collect();

        // 按优先级排序（数字越小优先级越高）
        credentials.sort_by_key(|c| c.priority);

        (
            snapshot.total,
            snapshot.available,
            snapshot.current_id,
            credentials,
        )
    }

    /// 设置凭据禁用状态
    pub fn set_disabled(&self, id: u64, disabled: bool) -> Result<(), AdminServiceError> {
        // 先获取当前凭据 ID，用于判断是否需要切换
        let snapshot = self.token_manager.snapshot();
        let current_id = snapshot.current_id;

        self.token_manager
            .set_disabled(id, disabled)
            .map_err(|e| self.classify_error(e, id))?;
        self.invalidate_balance_cache(id);
        if disabled {
            self.prompt_cache.clear_credential(id);
        }

        // 只有禁用的是当前凭据时才尝试切换到下一个
        if disabled && id == current_id {
            let _ = self.token_manager.switch_to_next();
        }
        Ok(())
    }

    /// 设置凭据优先级
    pub fn set_priority(&self, id: u64, priority: u32) -> Result<(), AdminServiceError> {
        self.token_manager
            .set_priority(id, priority)
            .map_err(|e| self.classify_error(e, id))?;
        self.invalidate_balance_cache(id);
        Ok(())
    }

    /// 重置失败计数并重新启用
    pub fn reset_and_enable(&self, id: u64) -> Result<(), AdminServiceError> {
        self.token_manager
            .reset_and_enable(id)
            .map_err(|e| self.classify_error(e, id))?;
        self.invalidate_balance_cache(id);
        Ok(())
    }

    /// 获取凭据余额(Redis 缓存,TTL 由 app_config.balance_cache_ttl_seconds 控制)
    pub async fn get_balance(&self, id: u64) -> Result<BalanceResponse, AdminServiceError> {
        let key = balance_cache_key(id);
        let ttl_secs: i64 = self
            .app_config
            .get_as::<i64>("balance_cache_ttl_seconds")
            .unwrap_or(DEFAULT_BALANCE_CACHE_TTL_SECS)
            .max(1);

        // 1) 先查 Redis
        if let Ok(mut conn) = self.redis.get().await {
            let cached: Result<Option<String>, _> =
                ::redis::cmd("GET").arg(&key).query_async(&mut *conn).await;
            if let Ok(Some(json)) = cached {
                if let Ok(entry) = serde_json::from_str::<CachedBalance>(&json) {
                    tracing::debug!("凭据 #{} 余额命中 Redis 缓存", id);
                    return Ok(entry.data);
                }
            }
        }

        // 2) 缓存未命中,从上游获取
        let balance = self.fetch_balance(id).await?;

        // 3) 写回 Redis(SETEX)
        let entry = CachedBalance {
            cached_at: chrono::Utc::now().timestamp() as f64,
            data: balance.clone(),
        };
        if let Ok(json) = serde_json::to_string(&entry) {
            match self.redis.get().await {
                Ok(mut conn) => {
                    let res: Result<(), _> = ::redis::cmd("SET")
                        .arg(&key)
                        .arg(&json)
                        .arg("EX")
                        .arg(ttl_secs)
                        .query_async(&mut *conn)
                        .await;
                    if let Err(err) = res {
                        tracing::warn!("Redis SETEX {} 失败: {}", key, err);
                    }
                }
                Err(err) => tracing::warn!("Redis 取连接失败,跳过缓存写入: {}", err),
            }
        }

        Ok(balance)
    }

    /// 从上游获取余额（无缓存）
    async fn fetch_balance(&self, id: u64) -> Result<BalanceResponse, AdminServiceError> {
        let usage = self
            .token_manager
            .get_usage_limits_for(id)
            .await
            .map_err(|e| self.classify_balance_error(e, id))?;

        let current_usage = usage.current_usage();
        let usage_limit = usage.usage_limit();
        let remaining = (usage_limit - current_usage).max(0.0);
        let usage_percentage = if usage_limit > 0.0 {
            (current_usage / usage_limit * 100.0).min(100.0)
        } else {
            0.0
        };

        Ok(BalanceResponse {
            id,
            subscription_title: usage.subscription_title().map(|s| s.to_string()),
            current_usage,
            usage_limit,
            remaining,
            usage_percentage,
            next_reset_at: usage.next_date_reset,
        })
    }

    /// 使用指定凭据和指定模型发起一次最小化测试调用。
    pub async fn test_credential(
        &self,
        id: u64,
        req: CredentialTestRequest,
    ) -> Result<CredentialTestResponse, AdminServiceError> {
        let model = req.model.trim();
        if model.is_empty() {
            return Err(AdminServiceError::InvalidCredential(
                "模型不能为空".to_string(),
            ));
        }

        let mapped_model = map_model(model).ok_or_else(|| {
            AdminServiceError::InvalidCredential(format!("模型不支持: {}", model))
        })?;
        let prompt = req
            .prompt
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(DEFAULT_CREDENTIAL_TEST_PROMPT);

        let kiro_request = build_credential_test_request(prompt, &mapped_model);
        let request_body = serde_json::to_string(&kiro_request)
            .map_err(|err| AdminServiceError::InternalError(err.to_string()))?;

        let started = Instant::now();
        let call = self
            .kiro_provider
            .test_api_with_credential(id, &request_body)
            .await
            .map_err(|err| self.classify_test_error(err, id))?;
        let duration_ms = started.elapsed().as_millis();

        if call.status.is_success() {
            let output_text = extract_test_output(&call.body);
            self.kiro_provider
                .report_success_for_context(call.credential_id, None);
            return Ok(CredentialTestResponse {
                success: true,
                credential_id: call.credential_id,
                model: model.to_string(),
                status_code: Some(call.status.as_u16()),
                output_text: Some(if output_text.is_empty() {
                    "上游返回成功，但未解析到文本输出。".to_string()
                } else {
                    output_text
                }),
                error_type: None,
                error_message: None,
                duration_ms,
                content_type: call.content_type,
                raw_preview: None,
            });
        }

        let raw_preview = text_preview(&call.body, MAX_TEST_PREVIEW_CHARS);
        Ok(CredentialTestResponse {
            success: false,
            credential_id: call.credential_id,
            model: model.to_string(),
            status_code: Some(call.status.as_u16()),
            output_text: None,
            error_type: Some(status_error_type(call.status.as_u16()).to_string()),
            error_message: Some(if raw_preview.is_empty() {
                format!("上游返回 HTTP {}", call.status.as_u16())
            } else {
                raw_preview.clone()
            }),
            duration_ms,
            content_type: call.content_type,
            raw_preview: Some(raw_preview),
        })
    }

    /// 添加新凭据
    pub async fn add_credential(
        &self,
        req: AddCredentialRequest,
    ) -> Result<AddCredentialResponse, AdminServiceError> {
        // 校验端点名：未指定则默认合法，指定则必须已注册
        if let Some(ref name) = req.endpoint {
            if !self.known_endpoints.contains(name) {
                let mut known: Vec<&str> =
                    self.known_endpoints.iter().map(|s| s.as_str()).collect();
                known.sort();
                return Err(AdminServiceError::InvalidCredential(format!(
                    "未知端点 \"{}\"，已注册端点: {:?}",
                    name, known
                )));
            }
        }

        // 构建凭据对象
        let email = req.email.clone();
        let new_cred = KiroCredentials {
            id: None,
            access_token: None,
            refresh_token: req.refresh_token,
            profile_arn: None,
            expires_at: None,
            auth_method: Some(req.auth_method),
            client_id: req.client_id,
            client_secret: req.client_secret,
            priority: req.priority,
            region: req.region,
            auth_region: req.auth_region,
            api_region: req.api_region,
            machine_id: req.machine_id,
            email: req.email,
            subscription_title: None, // 将在首次获取使用额度时自动更新
            proxy_url: req.proxy_url,
            proxy_username: req.proxy_username,
            proxy_password: req.proxy_password,
            disabled: false, // 新添加的凭据默认启用
            kiro_api_key: req.kiro_api_key,
            endpoint: req.endpoint,
        };

        // 调用 token_manager 添加凭据
        let credential_id = self
            .token_manager
            .add_credential(new_cred)
            .await
            .map_err(|e| self.classify_add_error(e))?;

        // 主动获取订阅等级，避免首次请求时 Free 账号绕过 Opus 模型过滤
        if let Err(e) = self.token_manager.get_usage_limits_for(credential_id).await {
            tracing::warn!("添加凭据后获取订阅等级失败（不影响凭据添加）: {}", e);
        }
        self.invalidate_balance_cache(credential_id);

        Ok(AddCredentialResponse {
            success: true,
            message: format!("凭据添加成功，ID: {}", credential_id),
            credential_id,
            email,
        })
    }

    /// 删除凭据
    pub fn delete_credential(&self, id: u64) -> Result<(), AdminServiceError> {
        self.token_manager
            .delete_credential(id)
            .map_err(|e| self.classify_delete_error(e, id))?;
        self.prompt_cache.clear_credential(id);
        self.invalidate_balance_cache(id);
        Ok(())
    }

    /// 查询请求级 usage 记录(优先 PG,失败回退内存)。
    pub async fn get_usage_records(&self, query: UsageRecordQuery) -> UsageRecordsResult {
        self.usage_recorder.query_async(query).await
    }

    /// 分页查询请求级 usage 记录(优先 PG,失败回退内存)。
    pub async fn get_usage_records_page(
        &self,
        query: UsageRecordQuery,
        page: usize,
        limit: usize,
    ) -> UsageRecordsPageResult {
        self.usage_recorder
            .query_page_async(query, page, limit)
            .await
    }

    /// 获取 usage 汇总。
    pub fn get_usage_summary(&self) -> UsageSummary {
        self.usage_recorder.summary(
            self.prompt_cache_runtime_config
                .snapshot()
                .high_cache_threshold,
        )
    }

    /// 清空 usage 记录。
    pub async fn clear_usage_records(&self) -> anyhow::Result<u64> {
        self.usage_recorder.clear_all().await
    }

    /// PG 聚合统计(today / 累计 / 按模型 / 按凭据,带与 usage 列表一致的过滤参数)。
    pub async fn get_usage_stats(
        &self,
        db: &crate::storage::Db,
        filter: &crate::anthropic::usage::UsageStatsFilter,
    ) -> anyhow::Result<crate::anthropic::usage::UsageStats> {
        crate::anthropic::usage::query_usage_stats(db, filter).await
    }

    /// 获取负载均衡模式
    pub fn get_load_balancing_mode(&self) -> LoadBalancingModeResponse {
        LoadBalancingModeResponse {
            mode: self.token_manager.get_load_balancing_mode(),
        }
    }

    /// 设置负载均衡模式
    pub fn set_load_balancing_mode(
        &self,
        req: SetLoadBalancingModeRequest,
    ) -> Result<LoadBalancingModeResponse, AdminServiceError> {
        // 验证模式值
        if req.mode != "priority" && req.mode != "balanced" {
            return Err(AdminServiceError::InvalidCredential(
                "mode 必须是 'priority' 或 'balanced'".to_string(),
            ));
        }

        self.token_manager
            .set_load_balancing_mode(req.mode.clone())
            .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;

        Ok(LoadBalancingModeResponse { mode: req.mode })
    }

    /// 强制刷新指定凭据的 Token
    pub async fn force_refresh_token(&self, id: u64) -> Result<(), AdminServiceError> {
        self.token_manager
            .force_refresh_token_for(id)
            .await
            .map_err(|e| self.classify_balance_error(e, id))?;
        self.invalidate_balance_cache(id);
        Ok(())
    }

    /// 查询最近 N 条配额事件(供前端凭据详情查看)
    pub async fn list_quota_events(
        &self,
        credential_id: Option<u64>,
        limit: i64,
    ) -> anyhow::Result<Vec<QuotaEventItem>> {
        use sqlx::Row;
        let limit = limit.clamp(1, 500);
        let rows = if let Some(id) = credential_id {
            sqlx::query(
                "SELECT id, credential_id, occurred_at, kind, reason, upstream_status, \
                        cooldown_until, note \
                 FROM quota_events WHERE credential_id = $1 \
                 ORDER BY occurred_at DESC LIMIT $2",
            )
            .bind(id as i64)
            .bind(limit)
            .fetch_all(&self.db)
            .await?
        } else {
            sqlx::query(
                "SELECT id, credential_id, occurred_at, kind, reason, upstream_status, \
                        cooldown_until, note \
                 FROM quota_events ORDER BY occurred_at DESC LIMIT $1",
            )
            .bind(limit)
            .fetch_all(&self.db)
            .await?
        };
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push(QuotaEventItem {
                id: r.try_get::<i64, _>("id")?,
                credential_id: r.try_get::<i64, _>("credential_id")?,
                occurred_at: r.try_get::<chrono::DateTime<chrono::Utc>, _>("occurred_at")?,
                kind: r.try_get("kind")?,
                reason: r.try_get("reason").ok(),
                upstream_status: r
                    .try_get::<Option<i16>, _>("upstream_status")
                    .ok()
                    .flatten(),
                cooldown_until: r
                    .try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("cooldown_until")
                    .ok()
                    .flatten(),
                note: r.try_get("note").ok(),
            });
        }
        Ok(out)
    }

    // ============ 错误分类 ============

    /// 分类简单操作错误（set_disabled, set_priority, reset_and_enable）
    fn classify_error(&self, e: anyhow::Error, id: u64) -> AdminServiceError {
        let msg = e.to_string();
        if msg.contains("不存在") {
            AdminServiceError::NotFound { id }
        } else {
            AdminServiceError::InternalError(msg)
        }
    }

    /// 分类余额查询错误（可能涉及上游 API 调用）
    fn classify_balance_error(&self, e: anyhow::Error, id: u64) -> AdminServiceError {
        let msg = e.to_string();

        // 1. 凭据不存在
        if msg.contains("不存在") {
            return AdminServiceError::NotFound { id };
        }

        // 2. API Key 凭据不支持刷新：客户端请求错误，映射为 400
        if msg.contains("API Key 凭据不支持刷新") {
            return AdminServiceError::InvalidCredential(msg);
        }

        // 3. 上游服务错误特征：HTTP 响应错误或网络错误
        let is_upstream_error =
            // HTTP 响应错误（来自 refresh_*_token 的错误消息）
            msg.contains("凭证已过期或无效") ||
            msg.contains("权限不足") ||
            msg.contains("已被限流") ||
            msg.contains("服务器错误") ||
            msg.contains("Token 刷新失败") ||
            msg.contains("暂时不可用") ||
            // 网络错误（reqwest 错误）
            msg.contains("error trying to connect") ||
            msg.contains("connection") ||
            msg.contains("timeout") ||
            msg.contains("timed out");

        if is_upstream_error {
            AdminServiceError::UpstreamError(msg)
        } else {
            // 4. 默认归类为内部错误（本地验证失败、配置错误等）
            // 包括：缺少 refreshToken、refreshToken 已被截断、无法生成 machineId 等
            AdminServiceError::InternalError(msg)
        }
    }

    /// 分类添加凭据错误
    fn classify_add_error(&self, e: anyhow::Error) -> AdminServiceError {
        let msg = e.to_string();

        // 凭据验证失败（refreshToken 无效、格式错误等）
        let is_invalid_credential = msg.contains("缺少 refreshToken")
            || msg.contains("refreshToken 为空")
            || msg.contains("refreshToken 已被截断")
            || msg.contains("凭据已存在")
            || msg.contains("refreshToken 重复")
            || msg.contains("kiroApiKey 重复")
            || msg.contains("缺少 kiroApiKey")
            || msg.contains("kiroApiKey 为空")
            || msg.contains("凭证已过期或无效")
            || msg.contains("权限不足")
            || msg.contains("已被限流");

        if is_invalid_credential {
            AdminServiceError::InvalidCredential(msg)
        } else if msg.contains("error trying to connect")
            || msg.contains("connection")
            || msg.contains("timeout")
        {
            AdminServiceError::UpstreamError(msg)
        } else {
            AdminServiceError::InternalError(msg)
        }
    }

    /// 分类删除凭据错误
    fn classify_delete_error(&self, e: anyhow::Error, id: u64) -> AdminServiceError {
        let msg = e.to_string();
        if msg.contains("不存在") {
            AdminServiceError::NotFound { id }
        } else if msg.contains("只能删除已禁用的凭据") || msg.contains("请先禁用凭据")
        {
            AdminServiceError::InvalidCredential(msg)
        } else {
            AdminServiceError::InternalError(msg)
        }
    }

    fn classify_test_error(&self, e: anyhow::Error, id: u64) -> AdminServiceError {
        let msg = e.to_string();
        if msg.contains("不存在") {
            AdminServiceError::NotFound { id }
        } else if msg.contains("缺少 refreshToken")
            || msg.contains("缺少 kiroApiKey")
            || msg.contains("Token")
            || msg.contains("accessToken")
            || msg.contains("refreshToken")
        {
            AdminServiceError::InvalidCredential(msg)
        } else if msg.contains("error trying to connect")
            || msg.contains("connection")
            || msg.contains("timeout")
            || msg.contains("timed out")
        {
            AdminServiceError::UpstreamError(msg)
        } else {
            AdminServiceError::InternalError(msg)
        }
    }
}

fn build_credential_test_request(prompt: &str, mapped_model: &str) -> KiroRequest {
    let user_input = UserInputMessage::new(prompt, mapped_model).with_origin("AI_EDITOR");
    let conversation_state = ConversationState::new(uuid::Uuid::new_v4().to_string())
        .with_agent_continuation_id(uuid::Uuid::new_v4().to_string())
        .with_agent_task_type("vibe")
        .with_chat_trigger_type("MANUAL")
        .with_current_message(CurrentMessage::new(user_input));

    KiroRequest {
        conversation_state,
        profile_arn: None,
    }
}

fn extract_test_output(body: &[u8]) -> String {
    let mut decoder = EventStreamDecoder::new();
    if decoder.feed(body).is_err() {
        return text_preview(body, MAX_TEST_PREVIEW_CHARS);
    }

    let mut text = String::new();
    let mut tool_names: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    for result in decoder.decode_iter() {
        let Ok(frame) = result else {
            continue;
        };
        let Ok(event) = Event::from_frame(frame) else {
            continue;
        };
        match event {
            Event::AssistantResponse(resp) => text.push_str(&resp.content),
            Event::ReasoningContent(reasoning) => {
                if text.is_empty() && !reasoning.text.is_empty() {
                    text.push_str(&reasoning.text);
                }
            }
            Event::ToolUse(tool_use) if tool_use.stop => tool_names.push(tool_use.name),
            Event::InvalidState(invalid) => errors.push(invalid.error_text()),
            Event::Error {
                error_code,
                error_message,
            } => errors.push(format!("{}: {}", error_code, error_message)),
            Event::Exception {
                exception_type,
                message,
            } => errors.push(format!("{}: {}", exception_type, message)),
            _ => {}
        }
    }

    if !text.trim().is_empty() {
        return truncate_chars(text.trim(), MAX_TEST_PREVIEW_CHARS);
    }
    if !tool_names.is_empty() {
        return truncate_chars(
            &format!("上游返回工具调用: {}", tool_names.join(", ")),
            MAX_TEST_PREVIEW_CHARS,
        );
    }
    if !errors.is_empty() {
        return truncate_chars(&errors.join("\n"), MAX_TEST_PREVIEW_CHARS);
    }

    text_preview(body, MAX_TEST_PREVIEW_CHARS)
}

fn text_preview(body: &[u8], max_chars: usize) -> String {
    truncate_chars(String::from_utf8_lossy(body).trim(), max_chars)
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let mut out: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        out.push_str("...");
    }
    out
}

fn status_error_type(status: u16) -> &'static str {
    match status {
        400 => "invalid_request_error",
        401 => "authentication_error",
        402 => "quota_exhausted",
        403 => "permission_error",
        408 => "upstream_timeout",
        429 => "rate_limited",
        500..=599 => "upstream_error",
        _ => "api_error",
    }
}

fn normalize_page(page: usize) -> usize {
    page.max(1)
}

fn normalize_credentials_limit(limit: usize) -> usize {
    if limit == 0 {
        DEFAULT_CREDENTIALS_PAGE_LIMIT
    } else {
        limit.min(MAX_CREDENTIALS_PAGE_LIMIT)
    }
}

fn total_pages(total: usize, limit: usize) -> usize {
    if total == 0 { 0 } else { total.div_ceil(limit) }
}
