//! Admin API 业务逻辑服务

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::error::AdminServiceError;
use super::types::{
    AddCredentialRequest, AddCredentialResponse, BalanceResponse, ClearInFlightRequest,
    CreateProxyResourceRequest, CredentialAccountInfo, CredentialStatusItem,
    CredentialsPageResponse, CredentialsStatusResponse, LoadBalancingModeResponse,
    ProxyResourceResponse, ProxyResourcesResponse, RuntimeConfigResponse,
    SetCredentialProxyRequest, SetLoadBalancingModeRequest, SetWarmupRequest,
    TestCredentialRequest, TestCredentialResponse, UpdateProxyResourceRequest,
    UpdateRuntimeConfigRequest,
};
use crate::anthropic::{
    converter::map_model,
    model_capabilities::{ModelCapabilitiesCatalog, ModelCapabilitiesStatus},
    pricing::{PricingCatalog, PricingStatus},
    prompt_cache::PromptCacheTracker,
    usage::{
        UsageRecordQuery, UsageRecorder, UsageRecorderStats, UsageRecordsPageResult,
        UsageRecordsResult, UsageSummary,
    },
};
use crate::kiro::model::credentials::KiroCredentials;
use crate::kiro::model::events::Event;
use crate::kiro::model::requests::{
    ConversationState, CurrentMessage, KiroRequest, UserInputMessage,
};
use crate::kiro::parser::decoder::EventStreamDecoder;
use crate::kiro::provider::KiroProvider;
use crate::kiro::token_manager::MultiTokenManager;
use crate::storage::postgres::{
    AdminAuditLogPage, CreateProxyResourceRow, CredentialAccountInfoRow, PostgresStore,
    ProxyResourceRow, UpdateProxyResourceRow,
};
use crate::storage::redis_cache::RedisStore;

/// 余额缓存过期时间（秒），5 分钟
const BALANCE_CACHE_TTL_SECS: i64 = 300;
const DEFAULT_CREDENTIALS_PAGE_LIMIT: usize = 12;
const MAX_CREDENTIALS_PAGE_LIMIT: usize = 500;

/// 缓存的余额条目（含时间戳）
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedBalance {
    /// 缓存时间（Unix 秒）
    cached_at: f64,
    /// 缓存的余额数据
    data: BalanceResponse,
}

fn balance_cache_key(id: u64) -> String {
    format!("balance:{}", id)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CredentialsBackupExport {
    format: &'static str,
    exported_at: String,
    credentials: Vec<KiroCredentials>,
}

/// Admin 服务
///
/// 封装所有 Admin API 的业务逻辑
pub struct AdminService {
    token_manager: Arc<MultiTokenManager>,
    postgres_store: Arc<PostgresStore>,
    redis_store: Arc<RedisStore>,
    /// 已注册的端点名称集合（用于 add_credential 校验）
    known_endpoints: HashSet<String>,
    usage_recorder: Arc<UsageRecorder>,
    prompt_cache: Arc<PromptCacheTracker>,
    pricing_catalog: Arc<PricingCatalog>,
    model_capabilities: Arc<ModelCapabilitiesCatalog>,
    kiro_provider: Arc<KiroProvider>,
}

impl AdminService {
    pub fn new(
        token_manager: Arc<MultiTokenManager>,
        known_endpoints: impl IntoIterator<Item = String>,
        usage_recorder: Arc<UsageRecorder>,
        prompt_cache: Arc<PromptCacheTracker>,
        pricing_catalog: Arc<PricingCatalog>,
        model_capabilities: Arc<ModelCapabilitiesCatalog>,
        kiro_provider: Arc<KiroProvider>,
        postgres_store: Arc<PostgresStore>,
        redis_store: Arc<RedisStore>,
    ) -> Self {
        Self {
            token_manager,
            postgres_store,
            redis_store,
            known_endpoints: known_endpoints.into_iter().collect(),
            usage_recorder,
            prompt_cache,
            pricing_catalog,
            model_capabilities,
            kiro_provider,
        }
    }

    fn invalidate_balance_cache(&self, id: u64) {
        let redis = self.redis_store.clone();
        tokio::spawn(async move {
            if let Err(err) = redis.del(balance_cache_key(id)).await {
                tracing::warn!("清理 Redis 余额缓存失败: {}", err);
            }
        });
    }

    fn invalidate_all_credential_caches(&self) {
        let snapshot = self.token_manager.snapshot();
        for entry in snapshot.entries {
            self.invalidate_balance_cache(entry.id);
        }
    }

    fn reload_proxy_resources_after_admin_change(&self) {
        if let Err(err) = self.token_manager.reload_proxy_resources_from_postgres() {
            tracing::warn!("重新加载代理资源失败: {}", err);
        }
        if let Err(err) = self.token_manager.reload_credentials_from_postgres() {
            tracing::warn!("代理资源变更后重新加载凭据失败: {}", err);
        }
        self.token_manager
            .publish_admin_credentials_changed("proxy_resources_changed");
        self.token_manager.notify_dispatch_state_changed();
    }

    fn audit(
        &self,
        action: &'static str,
        object_type: &'static str,
        object_id: Option<String>,
        success: bool,
        error_message: Option<String>,
        detail: serde_json::Value,
    ) {
        let store = self.postgres_store.clone();
        tokio::spawn(async move {
            if let Err(err) = store
                .record_admin_audit_log(
                    "admin-api",
                    action,
                    object_type,
                    object_id.as_deref(),
                    success,
                    error_message.as_deref(),
                    detail,
                )
                .await
            {
                tracing::warn!("写入 Admin 审计日志失败: {}", err);
            }
        });
    }

    /// 获取所有凭据状态
    pub fn get_all_credentials(&self) -> CredentialsStatusResponse {
        let (
            total,
            available,
            current_id,
            global_in_flight_requests,
            queued_requests,
            global_max_concurrent_requests,
            max_queued_requests,
            credentials,
        ) = self.credential_status_items();

        CredentialsStatusResponse {
            total,
            available,
            current_id,
            global_in_flight_requests,
            queued_requests,
            global_max_concurrent_requests,
            max_queued_requests,
            credentials,
        }
    }

    /// 分页获取凭据状态。
    pub fn get_credentials_page(&self, page: usize, limit: usize) -> CredentialsPageResponse {
        let page = normalize_page(page);
        let limit = normalize_credentials_limit(limit);
        let (
            total,
            available,
            current_id,
            global_in_flight_requests,
            queued_requests,
            global_max_concurrent_requests,
            max_queued_requests,
            credentials,
        ) = self.credential_status_items();
        let total_pages = total_pages(total, limit);
        let start = page.saturating_sub(1).saturating_mul(limit);
        let credentials = credentials.into_iter().skip(start).take(limit).collect();

        CredentialsPageResponse {
            total,
            available,
            current_id,
            global_in_flight_requests,
            queued_requests,
            global_max_concurrent_requests,
            max_queued_requests,
            page,
            limit,
            total_pages,
            credentials,
        }
    }

    fn credential_status_items(
        &self,
    ) -> (
        usize,
        usize,
        u64,
        u32,
        u32,
        u32,
        u32,
        Vec<CredentialStatusItem>,
    ) {
        let snapshot = self.token_manager.snapshot();
        let default_endpoint = self.token_manager.runtime_config().default_endpoint;
        let cost_summary = self.usage_recorder.credential_cost_summary();
        let account_info = match block_on_admin_store({
            let store = self.postgres_store.clone();
            async move { store.load_credential_account_info().await }
        }) {
            Ok(info) => info,
            Err(err) => {
                tracing::warn!("加载凭据账号信息快照失败: {}", err);
                Default::default()
            }
        };

        let mut credentials: Vec<CredentialStatusItem> = snapshot
            .entries
            .into_iter()
            .map(|entry| {
                let cost = cost_summary.get(&entry.id).copied().unwrap_or_default();
                let info = account_info.get(&entry.id).map(account_info_from_row);
                CredentialStatusItem {
                    id: entry.id,
                    created_at: entry.created_at,
                    updated_at: entry.updated_at,
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
                    subscription_title: entry.subscription_title,
                    account_info: info,
                    success_count: entry.success_count,
                    last_used_at: entry.last_used_at.clone(),
                    has_proxy: entry.has_proxy,
                    proxy_url: entry.proxy_url,
                    proxy_resource_id: entry.proxy_resource_id,
                    proxy_resource_name: entry.proxy_resource_name,
                    effective_proxy_url: entry.effective_proxy_url,
                    effective_proxy_source: entry.effective_proxy_source,
                    refresh_failure_count: entry.refresh_failure_count,
                    disabled_reason: entry.disabled_reason,
                    endpoint: entry.endpoint.unwrap_or_else(|| default_endpoint.clone()),
                    cooled_down: entry.cooled_down,
                    cooldown_remaining_secs: entry.cooldown_remaining_secs,
                    cooldown_reason: entry.cooldown_reason,
                    rate_limited: entry.rate_limited,
                    rate_limit_remaining_secs: entry.rate_limit_remaining_secs,
                    in_flight_requests: entry.in_flight_requests,
                    oldest_in_flight_age_secs: entry.oldest_in_flight_age_secs,
                    newest_in_flight_idle_secs: entry.newest_in_flight_idle_secs,
                    max_concurrent_requests: entry.max_concurrent_requests,
                    in_flight_lease_max_secs: entry.in_flight_lease_max_secs,
                    warmup_remaining: entry.warmup_remaining,
                    transient_failure_streak: entry.transient_failure_streak,
                    recent_error_rate: entry.recent_error_rate,
                    latency_ewma_ms: entry.latency_ewma_ms,
                    last_error_kind: entry.last_error_kind,
                    last_error_reason: entry.last_error_reason,
                    last_error_at_ms: entry.last_error_at_ms,
                    in_probation: entry.in_probation,
                    probation_remaining_secs: entry.probation_remaining_secs,
                    scheduler_selection_count: entry.scheduler_selection_count,
                    recent_scheduler_selection_count_10s: entry
                        .recent_scheduler_selection_count_10s,
                    recent_scheduler_selection_count_60s: entry
                        .recent_scheduler_selection_count_60s,
                    recent_scheduler_selection_count_5m: entry.recent_scheduler_selection_count_5m,
                    scheduler_selection_pressure: entry.scheduler_selection_pressure,
                    scheduler_score: entry.scheduler_score,
                    estimated_cost_usd: cost.estimated_cost_usd,
                    priced_requests: cost.priced_requests,
                    unpriced_requests: cost.unpriced_requests,
                }
            })
            .collect();

        // 按优先级排序（数字越小优先级越高）
        credentials.sort_by_key(|c| c.priority);

        (
            snapshot.total,
            snapshot.available,
            snapshot.current_id,
            snapshot.global_in_flight_requests,
            snapshot.queued_requests,
            snapshot.global_max_concurrent_requests,
            snapshot.max_queued_requests,
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
        self.audit(
            "set_credential_disabled",
            "credential",
            Some(id.to_string()),
            true,
            None,
            json!({ "disabled": disabled }),
        );
        Ok(())
    }

    /// 设置凭据优先级
    pub fn set_priority(&self, id: u64, priority: u32) -> Result<(), AdminServiceError> {
        self.token_manager
            .set_priority(id, priority)
            .map_err(|e| self.classify_error(e, id))?;
        self.invalidate_balance_cache(id);
        self.audit(
            "set_credential_priority",
            "credential",
            Some(id.to_string()),
            true,
            None,
            json!({ "priority": priority }),
        );
        Ok(())
    }

    /// 重置失败计数并重新启用
    pub fn reset_and_enable(&self, id: u64) -> Result<(), AdminServiceError> {
        self.token_manager
            .reset_and_enable(id)
            .map_err(|e| self.classify_error(e, id))?;
        self.invalidate_balance_cache(id);
        self.audit(
            "reset_credential",
            "credential",
            Some(id.to_string()),
            true,
            None,
            json!({}),
        );
        Ok(())
    }

    /// 获取凭据余额（带缓存）
    pub async fn get_balance(&self, id: u64) -> Result<BalanceResponse, AdminServiceError> {
        if let Ok(Some(cached)) = self
            .redis_store
            .get_json::<CachedBalance>(balance_cache_key(id))
            .await
        {
            let now = Utc::now().timestamp() as f64;
            if (now - cached.cached_at) < BALANCE_CACHE_TTL_SECS as f64 {
                tracing::debug!("凭据 #{} 余额命中 Redis 缓存", id);
                self.save_account_info_snapshot(&cached.data).await?;
                return Ok(cached.data);
            }
        }

        // 缓存未命中或已过期，从上游获取
        let balance = self.fetch_balance(id).await?;
        self.save_account_info_snapshot(&balance).await?;

        let cached = CachedBalance {
            cached_at: Utc::now().timestamp() as f64,
            data: balance.clone(),
        };
        if let Err(err) = self
            .redis_store
            .set_json(
                balance_cache_key(id),
                &cached,
                BALANCE_CACHE_TTL_SECS as usize,
            )
            .await
        {
            tracing::warn!("保存 Redis 余额缓存失败: {}", err);
        }

        Ok(balance)
    }

    /// 使用指定凭据发起一次模型调用测试。
    pub async fn test_credential(
        &self,
        id: u64,
        req: TestCredentialRequest,
    ) -> Result<TestCredentialResponse, AdminServiceError> {
        let model = req.model.trim();
        if model.is_empty() {
            return Err(AdminServiceError::InvalidCredential(
                "请选择测试模型".to_string(),
            ));
        }
        let prompt = req.prompt.unwrap_or_else(|| "hi".to_string());
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Err(AdminServiceError::InvalidCredential(
                "测试消息不能为空".to_string(),
            ));
        }

        let model_id = map_model(model).ok_or_else(|| {
            AdminServiceError::InvalidCredential(format!("不支持的测试模型: {}", model))
        })?;

        let conversation_id = uuid::Uuid::new_v4().to_string();
        let agent_continuation_id = uuid::Uuid::new_v4().to_string();
        let user_input = UserInputMessage::new(prompt, &model_id).with_origin("AI_EDITOR");
        let conversation_state = ConversationState::new(conversation_id)
            .with_agent_continuation_id(agent_continuation_id)
            .with_agent_task_type("vibe")
            .with_chat_trigger_type("MANUAL")
            .with_current_message(CurrentMessage::new(user_input));
        let kiro_request = KiroRequest {
            conversation_state,
            profile_arn: None,
        };
        let request_body = serde_json::to_string(&kiro_request)
            .map_err(|e| AdminServiceError::InternalError(format!("序列化测试请求失败: {}", e)))?;

        let started_at = std::time::Instant::now();
        let api_response = self
            .kiro_provider
            .call_api_with_credential(id, &request_body)
            .await
            .map_err(|e| self.classify_test_error(e, id))?;
        let credential_id = api_response.credential_id();
        let (response, completion) = api_response.into_parts();
        let body_bytes = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(e) => {
                completion.release();
                return Err(AdminServiceError::UpstreamError(format!(
                    "读取测试响应失败: {}",
                    e
                )));
            }
        };
        let response_text = parse_model_test_response(&body_bytes)?;
        completion.release();

        Ok(TestCredentialResponse {
            success: true,
            credential_id,
            model: model.to_string(),
            model_id,
            prompt: prompt.to_string(),
            response: response_text,
            duration_ms: started_at.elapsed().as_millis() as u64,
        })
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
            checked_at: Utc::now().to_rfc3339(),
            subscription_title: usage.subscription_title().map(|s| s.to_string()),
            current_usage,
            usage_limit,
            remaining,
            usage_percentage,
            next_reset_at: usage.next_date_reset,
        })
    }

    async fn save_account_info_snapshot(
        &self,
        balance: &BalanceResponse,
    ) -> Result<(), AdminServiceError> {
        let info = CredentialAccountInfoRow {
            subscription_title: balance.subscription_title.clone(),
            current_usage: balance.current_usage,
            usage_limit: balance.usage_limit,
            remaining: balance.remaining,
            usage_percentage: balance.usage_percentage,
            next_reset_at: balance.next_reset_at,
            checked_at: balance.checked_at.clone(),
        };
        if let Err(err) = self
            .postgres_store
            .save_credential_account_info(balance.id, &info)
            .await
        {
            let message = format!("保存凭据账号信息快照失败: {}", err);
            tracing::warn!(credential_id = balance.id, "{}", message);
            return Err(AdminServiceError::InternalError(message));
        }
        Ok(())
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
            created_at: None,
            updated_at: None,
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
            proxy_resource_id: req.proxy_resource_id,
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

        // 主动获取订阅等级并保存账号信息快照，避免首次请求时 Free 账号绕过 Opus 模型过滤
        if let Err(e) = self.get_balance(credential_id).await {
            tracing::warn!("添加凭据后获取订阅等级失败（不影响凭据添加）: {}", e);
        }
        self.audit(
            "add_credential",
            "credential",
            Some(credential_id.to_string()),
            true,
            None,
            json!({ "email": email }),
        );

        Ok(AddCredentialResponse {
            success: true,
            message: format!("凭据添加成功，ID: {}", credential_id),
            credential_id,
            email,
        })
    }

    pub fn list_proxy_resources(&self) -> Result<ProxyResourcesResponse, AdminServiceError> {
        let resources = block_on_admin_store({
            let store = self.postgres_store.clone();
            async move { store.load_proxy_resources().await }
        })
        .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;
        Ok(ProxyResourcesResponse {
            resources: resources.into_iter().map(proxy_resource_response).collect(),
        })
    }

    pub fn create_proxy_resource(
        &self,
        req: CreateProxyResourceRequest,
    ) -> Result<ProxyResourceResponse, AdminServiceError> {
        let name = required_trimmed(req.name, "代理名称")?;
        let proxy_url = validate_proxy_url(&required_trimmed(req.proxy_url, "代理 URL")?)?;
        let row = CreateProxyResourceRow {
            name: name.clone(),
            proxy_url: proxy_url.clone(),
            proxy_username: optional_trimmed(req.proxy_username),
            proxy_password: optional_trimmed(req.proxy_password),
            enabled: req.enabled,
            notes: optional_trimmed(req.notes),
        };
        let created = block_on_admin_store({
            let store = self.postgres_store.clone();
            async move { store.insert_proxy_resource(&row).await }
        })
        .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;
        self.reload_proxy_resources_after_admin_change();
        self.audit(
            "create_proxy_resource",
            "proxy_resource",
            Some(created.id.to_string()),
            true,
            None,
            json!({ "name": name, "proxyUrl": proxy_url, "enabled": created.enabled }),
        );
        Ok(proxy_resource_response(created))
    }

    pub fn update_proxy_resource(
        &self,
        id: u64,
        req: UpdateProxyResourceRequest,
    ) -> Result<ProxyResourceResponse, AdminServiceError> {
        let name = match req.name {
            Some(value) => Some(required_trimmed(value, "代理名称")?),
            None => None,
        };
        let proxy_url = match req.proxy_url {
            Some(value) => Some(validate_proxy_url(&required_trimmed(value, "代理 URL")?)?),
            None => None,
        };
        let update = UpdateProxyResourceRow {
            name,
            proxy_url,
            proxy_username: if req.clear_username {
                Some(None)
            } else {
                req.proxy_username
                    .map(|value| optional_trimmed(Some(value)))
            },
            proxy_password: if req.clear_password {
                Some(None)
            } else {
                req.proxy_password
                    .map(|value| optional_trimmed(Some(value)))
            },
            enabled: req.enabled,
            notes: if req.clear_notes {
                Some(None)
            } else {
                req.notes.map(|value| optional_trimmed(Some(value)))
            },
        };
        let updated = block_on_admin_store({
            let store = self.postgres_store.clone();
            async move { store.update_proxy_resource(id, &update).await }
        })
        .map_err(|e| AdminServiceError::InternalError(e.to_string()))?
        .ok_or(AdminServiceError::NotFound { id })?;
        self.reload_proxy_resources_after_admin_change();
        self.audit(
            "update_proxy_resource",
            "proxy_resource",
            Some(id.to_string()),
            true,
            None,
            json!({ "enabled": updated.enabled }),
        );
        Ok(proxy_resource_response(updated))
    }

    pub fn delete_proxy_resource(&self, id: u64) -> Result<(), AdminServiceError> {
        let deleted = block_on_admin_store({
            let store = self.postgres_store.clone();
            async move { store.soft_delete_proxy_resource(id).await }
        })
        .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;
        if !deleted {
            return Err(AdminServiceError::NotFound { id });
        }
        self.invalidate_all_credential_caches();
        self.reload_proxy_resources_after_admin_change();
        self.audit(
            "delete_proxy_resource",
            "proxy_resource",
            Some(id.to_string()),
            true,
            None,
            json!({}),
        );
        Ok(())
    }

    pub fn set_credential_proxy(
        &self,
        id: u64,
        req: SetCredentialProxyRequest,
    ) -> Result<(), AdminServiceError> {
        let proxy_url = match req.proxy_url {
            Some(value) => {
                let value = required_trimmed(value, "代理 URL")?;
                if value.eq_ignore_ascii_case(KiroCredentials::PROXY_DIRECT) {
                    Some(KiroCredentials::PROXY_DIRECT.to_string())
                } else {
                    Some(validate_proxy_url(&value)?)
                }
            }
            None => None,
        };
        self.token_manager
            .set_credential_proxy(
                id,
                req.proxy_resource_id,
                proxy_url,
                optional_trimmed(req.proxy_username),
                optional_trimmed(req.proxy_password),
            )
            .map_err(|e| self.classify_error(e, id))?;
        self.invalidate_balance_cache(id);
        self.audit(
            "set_credential_proxy",
            "credential",
            Some(id.to_string()),
            true,
            None,
            json!({ "proxyResourceId": req.proxy_resource_id }),
        );
        Ok(())
    }

    /// 删除凭据
    pub fn delete_credential(&self, id: u64) -> Result<(), AdminServiceError> {
        self.token_manager
            .delete_credential(id)
            .map_err(|e| self.classify_delete_error(e, id))?;
        self.prompt_cache.clear_credential(id);

        // 清理已删除凭据的余额缓存
        self.invalidate_balance_cache(id);
        self.audit(
            "delete_credential",
            "credential",
            Some(id.to_string()),
            true,
            None,
            json!({}),
        );

        Ok(())
    }

    /// 查询请求级 usage 记录。
    pub fn get_usage_records(&self, query: UsageRecordQuery) -> UsageRecordsResult {
        self.usage_recorder.query(query)
    }

    /// 分页查询请求级 usage 记录。
    pub fn get_usage_records_page(
        &self,
        query: UsageRecordQuery,
        page: usize,
        limit: usize,
    ) -> UsageRecordsPageResult {
        self.usage_recorder.query_page(query, page, limit)
    }

    /// 获取 usage 汇总。
    pub fn get_usage_summary(&self) -> UsageSummary {
        let high_cache_threshold = self.token_manager.runtime_config().high_cache_threshold;
        self.usage_recorder.summary(high_cache_threshold)
    }

    /// 获取 usage 持久化 writer 状态。该状态只用于观测，不参与调度。
    pub fn get_usage_writer_stats(&self) -> UsageRecorderStats {
        self.usage_recorder.writer_stats()
    }

    /// 获取模型价格同步状态。
    pub fn get_model_pricing(&self) -> PricingStatus {
        self.pricing_catalog.status()
    }

    /// 手动同步模型价格。失败不影响调度，只体现在返回状态的 last_error。
    pub async fn sync_model_pricing(&self) -> PricingStatus {
        let mut status = self.pricing_catalog.sync().await;
        if let Err(err) = self.postgres_store.save_pricing_status(&status).await {
            tracing::warn!("保存手动同步后的模型价格到 PgSQL 失败: {}", err);
            if status.last_error.is_none() {
                status.last_error = Some(format!("价格已同步，但保存到 PgSQL 失败: {}", err));
            }
        }
        self.audit(
            "sync_model_pricing",
            "model_pricing",
            None,
            status.last_error.is_none(),
            status.last_error.clone(),
            json!({ "source": status.source, "modelCount": status.model_count }),
        );
        status
    }

    /// 获取 Kiro 模型能力同步状态。
    pub fn get_model_capabilities(&self) -> ModelCapabilitiesStatus {
        self.model_capabilities.status()
    }

    /// 手动同步 Kiro 模型能力。失败不影响调度，只体现在返回状态的 last_error。
    pub async fn sync_model_capabilities(&self) -> ModelCapabilitiesStatus {
        let status = match self.kiro_provider.list_available_models().await {
            Ok(models) => self.model_capabilities.sync_from_kiro_models(models),
            Err(err) => {
                tracing::warn!("同步 Kiro 模型能力失败，不影响请求调度: {}", err);
                self.model_capabilities.record_sync_error(err.to_string())
            }
        };
        let mut status = status;
        if let Err(err) = self
            .postgres_store
            .save_model_capabilities_status(&status)
            .await
        {
            tracing::warn!("保存模型能力到 PgSQL 失败: {}", err);
            if status.last_error.is_none() {
                status.last_error = Some(format!("模型能力已同步，但保存到 PgSQL 失败: {}", err));
            }
        }
        self.audit(
            "sync_model_capabilities",
            "model_capabilities",
            None,
            status.last_error.is_none(),
            status.last_error.clone(),
            json!({ "source": status.source, "modelCount": status.model_count }),
        );
        status
    }

    /// 导出完整凭据。仅格式化当前内存快照，不修改凭据状态。
    pub fn export_credentials(&self, format: &str) -> Result<(String, String), AdminServiceError> {
        let credentials = self.token_manager.export_credentials();
        let credential_count = credentials.len();
        let normalized = format.trim().to_ascii_lowercase();
        let result = match normalized.as_str() {
            "" | "json" => {
                let body = serde_json::to_string_pretty(&credentials).map_err(|e| {
                    AdminServiceError::InternalError(format!("序列化凭据失败: {}", e))
                })?;
                Ok((body, "kiro-credentials.json".to_string()))
            }
            "backup-json" | "wrapped-json" => {
                let export = CredentialsBackupExport {
                    format: "kiro-rs-credentials-backup",
                    exported_at: Utc::now().to_rfc3339(),
                    credentials,
                };
                let body = serde_json::to_string_pretty(&export).map_err(|e| {
                    AdminServiceError::InternalError(format!("序列化凭据失败: {}", e))
                })?;
                Ok((body, "kiro-credentials-backup.json".to_string()))
            }
            "jsonl" => {
                let mut lines = Vec::with_capacity(credentials.len());
                for credential in &credentials {
                    lines.push(serde_json::to_string(&credential).map_err(|e| {
                        AdminServiceError::InternalError(format!("序列化凭据失败: {}", e))
                    })?);
                }
                Ok((lines.join("\n"), "kiro-credentials.jsonl".to_string()))
            }
            _ => Err(AdminServiceError::InvalidCredential(format!(
                "不支持的导出格式: {}，可选 json、backup-json、jsonl",
                format
            ))),
        };
        self.audit(
            "export_credentials",
            "credential",
            None,
            result.is_ok(),
            result.as_ref().err().map(ToString::to_string),
            json!({ "format": normalized, "count": credential_count }),
        );
        result
    }

    /// 清空 usage 记录。
    pub fn clear_usage_records(&self) {
        self.usage_recorder.clear();
        self.audit(
            "clear_usage_records",
            "usage_record",
            None,
            true,
            None,
            json!({ "mode": "soft_delete" }),
        );
    }

    pub fn get_audit_logs(&self, page: usize, limit: usize) -> AdminAuditLogPage {
        let store = self.postgres_store.clone();
        block_on_admin_store(async move { store.query_admin_audit_logs(page, limit).await })
            .unwrap_or_else(|err| {
                tracing::warn!("查询 Admin 审计日志失败: {}", err);
                AdminAuditLogPage {
                    page: page.max(1),
                    limit: if limit == 0 { 20 } else { limit.min(200) },
                    has_next: false,
                    records: Vec::new(),
                }
            })
    }

    /// 获取运行时全局配置。
    pub fn get_runtime_config(&self) -> RuntimeConfigResponse {
        let config = self.token_manager.runtime_config();
        RuntimeConfigResponse {
            credential_rpm: config.credential_rpm.unwrap_or(0),
            credential_max_concurrent_requests: config.credential_max_concurrent_requests,
            credential_transient_cooldown_secs: config.credential_transient_cooldown_secs,
            credential_rate_limit_cooldown_secs: config.credential_rate_limit_cooldown_secs,
            credential_server_error_cooldown_secs: config.credential_server_error_cooldown_secs,
            credential_network_error_cooldown_secs: config.credential_network_error_cooldown_secs,
            credential_stream_error_cooldown_secs: config.credential_stream_error_cooldown_secs,
            credential_protocol_error_cooldown_secs: config.credential_protocol_error_cooldown_secs,
            credential_auth_error_cooldown_secs: config.credential_auth_error_cooldown_secs,
            credential_cooldown_backoff_multiplier: config.credential_cooldown_backoff_multiplier,
            credential_cooldown_jitter_percent: config.credential_cooldown_jitter_percent,
            credential_probation_secs: config.credential_probation_secs,
            credential_max_cooldown_secs: config.credential_max_cooldown_secs,
            credential_dispatch_max_wait_secs: config.credential_dispatch_max_wait_secs,
            credential_in_flight_lease_max_secs: config.credential_in_flight_lease_max_secs,
            dispatch_global_max_concurrent_requests: config.dispatch_global_max_concurrent_requests,
            dispatch_max_queued_requests: config.dispatch_max_queued_requests,
            credential_warmup_requests: config.credential_warmup_requests,
            credential_warmup_selection_percent: config.credential_warmup_selection_percent,
            credential_warmup_max_selection_percent: config.credential_warmup_max_selection_percent,
            scheduler_error_ewma_alpha: config.scheduler_error_ewma_alpha,
            scheduler_priority_weight: config.scheduler_priority_weight,
            scheduler_load_weight: config.scheduler_load_weight,
            scheduler_error_weight: config.scheduler_error_weight,
            scheduler_latency_weight: config.scheduler_latency_weight,
            scheduler_probation_weight: config.scheduler_probation_weight,
            scheduler_selection_pressure_weight: config.scheduler_selection_pressure_weight,
            scheduler_total_selection_weight: config.scheduler_total_selection_weight,
            scheduler_top_k: config.scheduler_top_k,
            compression_enabled: config.compression.enabled,
            whitespace_compression: config.compression.whitespace_compression,
            prompt_cache_target_read_ratio: config.prompt_cache_target_read_ratio,
            prompt_cache_token_scale: config.prompt_cache_token_scale,
            prompt_cache_max_simulated_input_tokens: config.prompt_cache_max_simulated_input_tokens,
            prompt_cache_cap_jitter_min_tokens: config.prompt_cache_cap_jitter_min_tokens,
            prompt_cache_cap_jitter_max_tokens: config.prompt_cache_cap_jitter_max_tokens,
            prompt_cache_scale_min_input_tokens: config.prompt_cache_scale_min_input_tokens,
            reported_usage: config.reported_usage.normalized(),
            high_cache_threshold: config.high_cache_threshold,
            compat_profile: config.compat_profile,
            extract_thinking: config.extract_thinking,
            expose_proxy_warnings: config.expose_proxy_warnings,
        }
    }

    /// 更新运行时全局配置，并写入 PgSQL。
    pub fn update_runtime_config(
        &self,
        req: UpdateRuntimeConfigRequest,
    ) -> Result<RuntimeConfigResponse, AdminServiceError> {
        let current_config = self.token_manager.runtime_config();
        let credential_dispatch_max_wait_secs = req
            .credential_dispatch_max_wait_secs
            .unwrap_or(current_config.credential_dispatch_max_wait_secs);
        let credential_in_flight_lease_max_secs = req
            .credential_in_flight_lease_max_secs
            .unwrap_or(current_config.credential_in_flight_lease_max_secs);
        let credential_rate_limit_cooldown_secs = req
            .credential_rate_limit_cooldown_secs
            .unwrap_or(current_config.credential_rate_limit_cooldown_secs);
        let credential_server_error_cooldown_secs = req
            .credential_server_error_cooldown_secs
            .unwrap_or(current_config.credential_server_error_cooldown_secs);
        let credential_network_error_cooldown_secs = req
            .credential_network_error_cooldown_secs
            .unwrap_or(current_config.credential_network_error_cooldown_secs);
        let credential_stream_error_cooldown_secs = req
            .credential_stream_error_cooldown_secs
            .unwrap_or(current_config.credential_stream_error_cooldown_secs);
        let credential_protocol_error_cooldown_secs = req
            .credential_protocol_error_cooldown_secs
            .unwrap_or(current_config.credential_protocol_error_cooldown_secs);
        let credential_auth_error_cooldown_secs = req
            .credential_auth_error_cooldown_secs
            .unwrap_or(current_config.credential_auth_error_cooldown_secs);
        let credential_cooldown_backoff_multiplier = req
            .credential_cooldown_backoff_multiplier
            .unwrap_or(current_config.credential_cooldown_backoff_multiplier);
        let credential_cooldown_jitter_percent = req
            .credential_cooldown_jitter_percent
            .unwrap_or(current_config.credential_cooldown_jitter_percent);
        let credential_probation_secs = req
            .credential_probation_secs
            .unwrap_or(current_config.credential_probation_secs);
        let dispatch_global_max_concurrent_requests = req
            .dispatch_global_max_concurrent_requests
            .unwrap_or(current_config.dispatch_global_max_concurrent_requests);
        let dispatch_max_queued_requests = req
            .dispatch_max_queued_requests
            .unwrap_or(current_config.dispatch_max_queued_requests);
        let warmup_selection_percent = req
            .credential_warmup_selection_percent
            .unwrap_or(current_config.credential_warmup_selection_percent);
        let warmup_max_selection_percent = req
            .credential_warmup_max_selection_percent
            .unwrap_or(current_config.credential_warmup_max_selection_percent);
        let scheduler_error_ewma_alpha = req
            .scheduler_error_ewma_alpha
            .unwrap_or(current_config.scheduler_error_ewma_alpha);
        let scheduler_priority_weight = req
            .scheduler_priority_weight
            .unwrap_or(current_config.scheduler_priority_weight);
        let scheduler_load_weight = req
            .scheduler_load_weight
            .unwrap_or(current_config.scheduler_load_weight);
        let scheduler_error_weight = req
            .scheduler_error_weight
            .unwrap_or(current_config.scheduler_error_weight);
        let scheduler_latency_weight = req
            .scheduler_latency_weight
            .unwrap_or(current_config.scheduler_latency_weight);
        let scheduler_probation_weight = req
            .scheduler_probation_weight
            .unwrap_or(current_config.scheduler_probation_weight);
        let scheduler_selection_pressure_weight = req
            .scheduler_selection_pressure_weight
            .unwrap_or(current_config.scheduler_selection_pressure_weight);
        let scheduler_total_selection_weight = req
            .scheduler_total_selection_weight
            .unwrap_or(current_config.scheduler_total_selection_weight);
        let scheduler_top_k = req
            .scheduler_top_k
            .unwrap_or(current_config.scheduler_top_k);
        let prompt_cache_target_read_ratio = req
            .prompt_cache_target_read_ratio
            .unwrap_or(current_config.prompt_cache_target_read_ratio);
        let prompt_cache_token_scale = req
            .prompt_cache_token_scale
            .unwrap_or(current_config.prompt_cache_token_scale);
        let prompt_cache_max_simulated_input_tokens = req
            .prompt_cache_max_simulated_input_tokens
            .unwrap_or(current_config.prompt_cache_max_simulated_input_tokens);
        let prompt_cache_cap_jitter_min_tokens = req
            .prompt_cache_cap_jitter_min_tokens
            .unwrap_or(current_config.prompt_cache_cap_jitter_min_tokens);
        let prompt_cache_cap_jitter_max_tokens = req
            .prompt_cache_cap_jitter_max_tokens
            .unwrap_or(current_config.prompt_cache_cap_jitter_max_tokens);
        let prompt_cache_scale_min_input_tokens = req
            .prompt_cache_scale_min_input_tokens
            .unwrap_or(current_config.prompt_cache_scale_min_input_tokens);
        let reported_usage = req
            .reported_usage
            .clone()
            .unwrap_or_else(|| current_config.reported_usage.clone())
            .normalized();
        let high_cache_threshold = req
            .high_cache_threshold
            .unwrap_or(current_config.high_cache_threshold);
        let compat_profile = req.compat_profile.unwrap_or(current_config.compat_profile);
        let extract_thinking = req
            .extract_thinking
            .unwrap_or(current_config.extract_thinking);
        let expose_proxy_warnings = req
            .expose_proxy_warnings
            .unwrap_or(current_config.expose_proxy_warnings);

        if req.credential_max_cooldown_secs == 0 {
            return Err(AdminServiceError::InvalidCredential(
                "credentialMaxCooldownSecs 必须大于 0".to_string(),
            ));
        }
        if req.credential_transient_cooldown_secs > req.credential_max_cooldown_secs {
            return Err(AdminServiceError::InvalidCredential(
                "credentialTransientCooldownSecs 不能大于 credentialMaxCooldownSecs".to_string(),
            ));
        }
        if [
            credential_rate_limit_cooldown_secs,
            credential_server_error_cooldown_secs,
            credential_network_error_cooldown_secs,
            credential_stream_error_cooldown_secs,
            credential_protocol_error_cooldown_secs,
            credential_auth_error_cooldown_secs,
        ]
        .into_iter()
        .any(|value| value == 0 || value > req.credential_max_cooldown_secs)
        {
            return Err(AdminServiceError::InvalidCredential(
                "各错误类型基础冷却秒数必须大于 0 且不能大于 credentialMaxCooldownSecs".to_string(),
            ));
        }
        if !credential_cooldown_backoff_multiplier.is_finite()
            || !(1.0..=10.0).contains(&credential_cooldown_backoff_multiplier)
        {
            return Err(AdminServiceError::InvalidCredential(
                "credentialCooldownBackoffMultiplier 必须在 1 到 10 之间".to_string(),
            ));
        }
        if credential_cooldown_jitter_percent > 100 {
            return Err(AdminServiceError::InvalidCredential(
                "credentialCooldownJitterPercent 不能大于 100".to_string(),
            ));
        }
        if !scheduler_error_ewma_alpha.is_finite()
            || !(0.01..=1.0).contains(&scheduler_error_ewma_alpha)
        {
            return Err(AdminServiceError::InvalidCredential(
                "schedulerErrorEwmaAlpha 必须在 0.01 到 1 之间".to_string(),
            ));
        }
        if [
            scheduler_priority_weight,
            scheduler_load_weight,
            scheduler_error_weight,
            scheduler_latency_weight,
            scheduler_probation_weight,
            scheduler_selection_pressure_weight,
            scheduler_total_selection_weight,
        ]
        .into_iter()
        .any(|value| !value.is_finite() || value < 0.0)
        {
            return Err(AdminServiceError::InvalidCredential(
                "调度评分权重必须为非负有限数字".to_string(),
            ));
        }
        if scheduler_top_k == 0 || scheduler_top_k > 100 {
            return Err(AdminServiceError::InvalidCredential(
                "schedulerTopK 必须在 1 到 100 之间".to_string(),
            ));
        }
        if warmup_selection_percent > 100 {
            return Err(AdminServiceError::InvalidCredential(
                "credentialWarmupSelectionPercent 不能大于 100".to_string(),
            ));
        }
        if warmup_max_selection_percent > 100 {
            return Err(AdminServiceError::InvalidCredential(
                "credentialWarmupMaxSelectionPercent 不能大于 100".to_string(),
            ));
        }
        if !(0.0..=0.99).contains(&prompt_cache_target_read_ratio)
            || !prompt_cache_target_read_ratio.is_finite()
        {
            return Err(AdminServiceError::InvalidCredential(
                "promptCacheTargetReadRatio 必须在 0 到 0.99 之间".to_string(),
            ));
        }
        if !(1.0..=3.0).contains(&prompt_cache_token_scale) || !prompt_cache_token_scale.is_finite()
        {
            return Err(AdminServiceError::InvalidCredential(
                "promptCacheTokenScale 必须在 1 到 3 之间".to_string(),
            ));
        }
        if prompt_cache_max_simulated_input_tokens < 0 {
            return Err(AdminServiceError::InvalidCredential(
                "promptCacheMaxSimulatedInputTokens 不能小于 0".to_string(),
            ));
        }
        if prompt_cache_cap_jitter_min_tokens < 0 || prompt_cache_cap_jitter_max_tokens < 0 {
            return Err(AdminServiceError::InvalidCredential(
                "promptCacheCapJitterMinTokens 和 promptCacheCapJitterMaxTokens 不能小于 0"
                    .to_string(),
            ));
        }
        if prompt_cache_cap_jitter_min_tokens > prompt_cache_cap_jitter_max_tokens {
            return Err(AdminServiceError::InvalidCredential(
                "promptCacheCapJitterMinTokens 不能大于 promptCacheCapJitterMaxTokens".to_string(),
            ));
        }
        if prompt_cache_scale_min_input_tokens < 0 {
            return Err(AdminServiceError::InvalidCredential(
                "promptCacheScaleMinInputTokens 不能小于 0".to_string(),
            ));
        }
        reported_usage
            .validate()
            .map_err(AdminServiceError::InvalidCredential)?;
        if high_cache_threshold < 0 {
            return Err(AdminServiceError::InvalidCredential(
                "highCacheThreshold 不能小于 0".to_string(),
            ));
        }

        let credential_rpm = (req.credential_rpm > 0).then_some(req.credential_rpm);
        let compression = req.compression();

        self.token_manager
            .update_runtime_config(|config| {
                config.credential_rpm = credential_rpm;
                config.credential_max_concurrent_requests = req.credential_max_concurrent_requests;
                config.credential_transient_cooldown_secs = req.credential_transient_cooldown_secs;
                config.credential_rate_limit_cooldown_secs = credential_rate_limit_cooldown_secs;
                config.credential_server_error_cooldown_secs =
                    credential_server_error_cooldown_secs;
                config.credential_network_error_cooldown_secs =
                    credential_network_error_cooldown_secs;
                config.credential_stream_error_cooldown_secs =
                    credential_stream_error_cooldown_secs;
                config.credential_protocol_error_cooldown_secs =
                    credential_protocol_error_cooldown_secs;
                config.credential_auth_error_cooldown_secs = credential_auth_error_cooldown_secs;
                config.credential_cooldown_backoff_multiplier =
                    credential_cooldown_backoff_multiplier;
                config.credential_cooldown_jitter_percent = credential_cooldown_jitter_percent;
                config.credential_probation_secs = credential_probation_secs;
                config.credential_max_cooldown_secs = req.credential_max_cooldown_secs;
                config.credential_dispatch_max_wait_secs = credential_dispatch_max_wait_secs;
                config.credential_in_flight_lease_max_secs = credential_in_flight_lease_max_secs;
                config.dispatch_global_max_concurrent_requests =
                    dispatch_global_max_concurrent_requests;
                config.dispatch_max_queued_requests = dispatch_max_queued_requests;
                config.credential_warmup_requests = req.credential_warmup_requests;
                config.credential_warmup_selection_percent = warmup_selection_percent;
                config.credential_warmup_max_selection_percent = warmup_max_selection_percent;
                config.scheduler_error_ewma_alpha = scheduler_error_ewma_alpha;
                config.scheduler_priority_weight = scheduler_priority_weight;
                config.scheduler_load_weight = scheduler_load_weight;
                config.scheduler_error_weight = scheduler_error_weight;
                config.scheduler_latency_weight = scheduler_latency_weight;
                config.scheduler_probation_weight = scheduler_probation_weight;
                config.scheduler_selection_pressure_weight = scheduler_selection_pressure_weight;
                config.scheduler_total_selection_weight = scheduler_total_selection_weight;
                config.scheduler_top_k = scheduler_top_k;
                config.compression = compression.clone();
                config.prompt_cache_target_read_ratio = prompt_cache_target_read_ratio;
                config.prompt_cache_token_scale = prompt_cache_token_scale;
                config.prompt_cache_max_simulated_input_tokens =
                    prompt_cache_max_simulated_input_tokens;
                config.prompt_cache_cap_jitter_min_tokens = prompt_cache_cap_jitter_min_tokens;
                config.prompt_cache_cap_jitter_max_tokens = prompt_cache_cap_jitter_max_tokens;
                config.prompt_cache_scale_min_input_tokens = prompt_cache_scale_min_input_tokens;
                config.reported_usage = reported_usage;
                config.high_cache_threshold = high_cache_threshold;
                config.compat_profile = compat_profile;
                config.extract_thinking = extract_thinking;
                config.expose_proxy_warnings = expose_proxy_warnings;
            })
            .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;
        self.audit(
            "update_runtime_config",
            "runtime_config",
            Some("default".to_string()),
            true,
            None,
            json!({}),
        );

        Ok(self.get_runtime_config())
    }

    /// 设置凭据预热剩余请求数。
    pub fn set_warmup(&self, id: u64, req: SetWarmupRequest) -> Result<(), AdminServiceError> {
        self.token_manager
            .set_warmup_remaining(id, req.warmup_remaining)
            .map_err(|e| AdminServiceError::InvalidCredential(e.to_string()))?;
        self.audit(
            "set_credential_warmup",
            "credential",
            Some(id.to_string()),
            true,
            None,
            json!({ "warmupRemaining": req.warmup_remaining }),
        );
        Ok(())
    }

    /// 清理指定凭据的并发占用 lease。
    pub fn clear_in_flight(
        &self,
        id: u64,
        req: ClearInFlightRequest,
    ) -> Result<usize, AdminServiceError> {
        let snapshot = self.token_manager.snapshot();
        if !snapshot.entries.iter().any(|entry| entry.id == id) {
            return Err(AdminServiceError::NotFound { id });
        }
        let min_idle = req.min_idle_secs.map(StdDuration::from_secs);
        let cleared = self.token_manager.clear_in_flight_leases(id, min_idle);
        self.audit(
            "clear_credential_in_flight",
            "credential",
            Some(id.to_string()),
            true,
            None,
            json!({ "minIdleSecs": req.min_idle_secs, "cleared": cleared }),
        );
        Ok(cleared)
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
        if req.mode != "priority" && req.mode != "balanced" && req.mode != "health_balanced" {
            return Err(AdminServiceError::InvalidCredential(
                "mode 必须是 'priority'、'balanced' 或 'health_balanced'".to_string(),
            ));
        }

        self.token_manager
            .set_load_balancing_mode(req.mode.clone())
            .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;
        self.audit(
            "set_load_balancing_mode",
            "runtime_config",
            Some("default".to_string()),
            true,
            None,
            json!({ "mode": req.mode }),
        );

        Ok(LoadBalancingModeResponse { mode: req.mode })
    }

    /// 强制刷新指定凭据的 Token
    pub async fn force_refresh_token(&self, id: u64) -> Result<(), AdminServiceError> {
        self.token_manager
            .force_refresh_token_for(id)
            .await
            .map_err(|e| self.classify_balance_error(e, id))?;
        self.invalidate_balance_cache(id);
        self.audit(
            "force_refresh_token",
            "credential",
            Some(id.to_string()),
            true,
            None,
            json!({}),
        );
        Ok(())
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

    fn classify_test_error(&self, e: anyhow::Error, id: u64) -> AdminServiceError {
        let msg = e.to_string();
        if msg.contains("不存在") {
            AdminServiceError::NotFound { id }
        } else if msg.contains("已禁用") || msg.contains("不支持") {
            AdminServiceError::InvalidCredential(msg)
        } else {
            AdminServiceError::UpstreamError(msg)
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
            || msg.contains("代理资源不存在或已禁用")
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
}

fn parse_model_test_response(body_bytes: &[u8]) -> Result<String, AdminServiceError> {
    let mut decoder = EventStreamDecoder::new();
    decoder
        .feed(body_bytes)
        .map_err(|e| AdminServiceError::UpstreamError(format!("解析测试响应失败: {}", e)))?;

    let mut text_content = String::new();
    let mut invalid_state: Option<String> = None;
    let mut error: Option<String> = None;
    let mut exception: Option<String> = None;
    for result in decoder.decode_iter() {
        match result {
            Ok(frame) => match Event::from_frame(frame) {
                Ok(Event::AssistantResponse(resp)) => {
                    text_content.push_str(&resp.content);
                }
                Ok(Event::InvalidState(invalid)) => {
                    invalid_state = Some(invalid.error_text());
                }
                Ok(Event::Error {
                    error_code,
                    error_message,
                }) => {
                    error = Some(format!("{}: {}", error_code, error_message));
                }
                Ok(Event::Exception {
                    exception_type,
                    message,
                }) => {
                    exception = Some(format!("{}: {}", exception_type, message));
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!("测试响应事件解析失败: {}", e);
                }
            },
            Err(e) => {
                tracing::warn!("测试响应解码失败: {}", e);
            }
        }
    }

    if let Some(message) = invalid_state {
        return Err(AdminServiceError::UpstreamError(message));
    }
    if let Some(message) = error {
        return Err(AdminServiceError::UpstreamError(message));
    }
    if let Some(message) = exception {
        return Err(AdminServiceError::UpstreamError(message));
    }
    if text_content.trim().is_empty() {
        return Err(AdminServiceError::UpstreamError(
            "模型调用成功但响应为空".to_string(),
        ));
    }

    Ok(text_content)
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

fn required_trimmed(value: String, label: &str) -> Result<String, AdminServiceError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(AdminServiceError::InvalidCredential(format!(
            "{}不能为空",
            label
        )));
    }
    Ok(value.to_string())
}

fn optional_trimmed(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn validate_proxy_url(value: &str) -> Result<String, AdminServiceError> {
    let parsed = url::Url::parse(value).map_err(|_| {
        AdminServiceError::InvalidCredential(
            "代理 URL 必须是 http://、https:// 或 socks5:// 开头的完整地址".to_string(),
        )
    })?;
    match parsed.scheme() {
        "http" | "https" | "socks5" => Ok(value.to_string()),
        scheme => Err(AdminServiceError::InvalidCredential(format!(
            "不支持的代理协议: {}，仅支持 http/https/socks5",
            scheme
        ))),
    }
}

fn proxy_resource_response(row: ProxyResourceRow) -> ProxyResourceResponse {
    ProxyResourceResponse {
        id: row.id,
        name: row.name,
        proxy_url: row.proxy_url,
        proxy_username: row.proxy_username,
        has_password: row
            .proxy_password
            .as_deref()
            .map(|value| !value.is_empty())
            .unwrap_or(false),
        enabled: row.enabled,
        notes: row.notes,
        created_at: row.created_at,
        updated_at: row.updated_at,
        credential_count: row.credential_count,
    }
}

fn account_info_from_row(row: &CredentialAccountInfoRow) -> CredentialAccountInfo {
    CredentialAccountInfo {
        subscription_title: row.subscription_title.clone(),
        current_usage: row.current_usage,
        usage_limit: row.usage_limit,
        remaining: row.remaining,
        usage_percentage: row.usage_percentage,
        next_reset_at: row.next_reset_at,
        checked_at: row.checked_at.clone(),
    }
}

fn block_on_admin_store<T>(
    future: impl std::future::Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(|| handle.block_on(future))
    } else {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(future)
    }
}
