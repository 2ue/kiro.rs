//! Admin API 业务逻辑服务

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::error::AdminServiceError;
use super::types::{
    AccessKeysResponse, AddCredentialRequest, AddCredentialResponse, BalanceResponse,
    BatchCredentialImportDefaults, BatchCredentialImportDuplicateMode, BatchCredentialImportItem,
    BatchCredentialImportRequest, BatchCredentialImportResponse, ClearInFlightRequest,
    CreateProxyResourceRequest, CredentialAccountInfo, CredentialInfoRefreshItem,
    CredentialInfoRefreshResponse, CredentialStatusItem, CredentialValidationGroup,
    CredentialValidationInfo, CredentialValidationItem, CredentialValidationResponse,
    CredentialsPageResponse, CredentialsStatusResponse, ExternalPoolTestRequest,
    LoadBalancingModeResponse, ManualModelResponse, ProxyResourceResponse, ProxyResourcesResponse,
    RefreshCredentialInfoRequest, RuntimeConfigResponse, SetCredentialConcurrencyRequest,
    SetCredentialProxyRequest, SetLoadBalancingModeRequest, SetWarmupRequest,
    TestCredentialRequest, TestCredentialResponse, UpdateAdminApiKeyRequest,
    UpdateCredentialAuthRequest, UpdateProxyResourceRequest, UpdateRuntimeConfigRequest,
    UpsertManualModelRequest, UsageCleanupJobStatus, UsageCleanupMode, UsageCleanupPreviewResponse,
    UsageCleanupRequest, UsageCleanupStatusResponse, ValidateExistingCredentialsRequest,
    ValidateExternalCredentialsRequest,
};
use crate::anthropic::{
    model_capabilities::{
        MANUAL_SOURCE, ModelCapabilitiesCatalog, ModelCapabilitiesStatus, ModelCapabilityItem,
        ModelResolutionSource, normalize_model_id, normalize_supported_input_types,
    },
    pricing::{PricingCatalog, PricingStatus},
    prompt_cache::PromptCacheTracker,
    prompt_cache_creation_control::PromptCacheCreationController,
    usage::{
        UsageDashboardResponse, UsageRecordQuery, UsageRecorder, UsageRecorderStats,
        UsageRecordsPageResult, UsageRecordsResult, UsageSummary,
    },
};
use crate::external_pool::{
    CreateExternalPoolRequest, ExternalPool, ExternalPoolManager, ExternalPoolTestResponse,
    ExternalPoolsStatusResponse, SetExternalPoolEnabledRequest, UpdateExternalPoolRequest,
    external_pool_messages_url, external_pool_models_url,
};
use crate::kiro::model::credentials::KiroCredentials;
use crate::kiro::model::events::Event;
use crate::kiro::model::requests::{
    ConversationState, CurrentMessage, KiroRequest, UserInputMessage,
};
use crate::kiro::model::usage_limits::UsageLimitsResponse;
use crate::kiro::parser::decoder::EventStreamDecoder;
use crate::kiro::provider::KiroProvider;
use crate::kiro::token_manager::{CredentialAuthUpdate, MultiTokenManager};
use crate::model::config::ExternalPoolsConfig;
use crate::storage::postgres::{
    AdminAuditLogPage, CreateProxyResourceRow, CredentialAccountInfoRow, PostgresStore,
    PostgresUsageStore, ProxyResourceRow, UpdateProxyResourceRow,
};
use crate::storage::redis_cache::RedisStore;

/// 账号信息缓存过期时间（秒），5 分钟
const BALANCE_CACHE_TTL_SECS: i64 = 300;
const DEFAULT_CREDENTIALS_PAGE_LIMIT: usize = 12;
const MAX_CREDENTIALS_PAGE_LIMIT: usize = 500;
const MAX_MANUAL_MODEL_ID_LEN: usize = 160;
const USAGE_CLEANUP_DEFAULT_MAX_BATCHES: usize = 10_000;
const USAGE_CLEANUP_MAX_BATCHES: usize = 10_000;

#[derive(Debug, Clone, Default)]
pub struct CredentialListQuery {
    pub q: Option<String>,
    pub status: Option<String>,
    pub auth_method: Option<String>,
    pub subscription: Option<String>,
    pub proxy_resource_id: Option<u64>,
}

#[derive(Debug, Clone, Copy)]
struct CredentialStatusBuildOptions {
    include_account_info: bool,
    include_cost_summary: bool,
}

impl CredentialStatusBuildOptions {
    const LIGHT: Self = Self {
        include_account_info: false,
        include_cost_summary: false,
    };
}

/// 缓存的账号信息条目（含时间戳）
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedBalance {
    /// 缓存时间（Unix 秒）
    cached_at: f64,
    /// 缓存的账号信息数据
    data: BalanceResponse,
}

fn balance_cache_key(id: u64) -> String {
    format!("balance:{}", id)
}

fn mask_secret(value: &str) -> String {
    if value.is_empty() {
        return "未配置".to_string();
    }
    if value.len() <= 10 {
        let head = value.chars().take(2).collect::<String>();
        let tail = value
            .chars()
            .rev()
            .take(2)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>();
        return format!("{}...{}", head, tail);
    }
    let head = value.chars().take(6).collect::<String>();
    let tail = value
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("{}...{}", head, tail)
}

fn access_keys_response(request_api_key: &str, admin_api_key: &str) -> AccessKeysResponse {
    AccessKeysResponse {
        request_api_key: request_api_key.to_string(),
        masked_request_api_key: mask_secret(request_api_key),
        admin_api_key: admin_api_key.to_string(),
        masked_admin_api_key: mask_secret(admin_api_key),
    }
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
    request_api_key: String,
    /// 已注册的端点名称集合（用于 add_credential 校验）
    known_endpoints: HashSet<String>,
    usage_recorder: Arc<UsageRecorder>,
    prompt_cache: Arc<PromptCacheTracker>,
    prompt_cache_creation_controller: Arc<PromptCacheCreationController>,
    pricing_catalog: Arc<PricingCatalog>,
    model_capabilities: Arc<ModelCapabilitiesCatalog>,
    kiro_provider: Arc<KiroProvider>,
    external_pool_manager: Arc<ExternalPoolManager>,
    usage_cleanup: Arc<Mutex<UsageCleanupRuntime>>,
}

#[derive(Debug, Clone)]
struct UsageCleanupPlan {
    mode: UsageCleanupMode,
    cutoff: DateTime<Utc>,
    batch_size: usize,
    max_batches: usize,
    pause_ms_between_batches: u64,
}

#[derive(Debug)]
struct UsageCleanupRuntime {
    status: UsageCleanupStatusResponse,
    cancel: Option<Arc<AtomicBool>>,
}

impl Default for UsageCleanupRuntime {
    fn default() -> Self {
        Self {
            status: UsageCleanupStatusResponse {
                job_id: None,
                status: UsageCleanupJobStatus::Idle,
                mode: None,
                cutoff_at: None,
                batch_size: 0,
                max_batches: 0,
                pause_ms_between_batches: 0,
                matched_rows: None,
                remaining_rows: None,
                processed_rows: 0,
                last_batch_rows: 0,
                batches: 0,
                cancel_requested: false,
                stop_reason: None,
                started_at: None,
                updated_at: None,
                finished_at: None,
                last_error: None,
            },
            cancel: None,
        }
    }
}

impl AdminService {
    pub fn new(
        token_manager: Arc<MultiTokenManager>,
        known_endpoints: impl IntoIterator<Item = String>,
        usage_recorder: Arc<UsageRecorder>,
        prompt_cache: Arc<PromptCacheTracker>,
        prompt_cache_creation_controller: Arc<PromptCacheCreationController>,
        pricing_catalog: Arc<PricingCatalog>,
        model_capabilities: Arc<ModelCapabilitiesCatalog>,
        kiro_provider: Arc<KiroProvider>,
        postgres_store: Arc<PostgresStore>,
        redis_store: Arc<RedisStore>,
        request_api_key: impl Into<String>,
        external_pool_manager: Arc<ExternalPoolManager>,
    ) -> Self {
        Self {
            token_manager,
            postgres_store,
            redis_store,
            request_api_key: request_api_key.into(),
            known_endpoints: known_endpoints.into_iter().collect(),
            usage_recorder,
            prompt_cache,
            prompt_cache_creation_controller,
            pricing_catalog,
            model_capabilities,
            kiro_provider,
            external_pool_manager,
            usage_cleanup: Arc::new(Mutex::new(UsageCleanupRuntime::default())),
        }
    }

    fn invalidate_balance_cache(&self, id: u64) {
        let redis = self.redis_store.clone();
        tokio::spawn(async move {
            if let Err(err) = redis.del(balance_cache_key(id)).await {
                tracing::warn!("清理 Redis 账号信息缓存失败: {}", err);
            }
        });
    }

    pub fn get_access_keys(&self, admin_api_key: &str) -> AccessKeysResponse {
        access_keys_response(&self.request_api_key, admin_api_key)
    }

    pub fn list_external_pools(&self) -> Result<Vec<ExternalPool>, AdminServiceError> {
        let store = self.postgres_store.clone();
        block_on_admin_store(async move { store.list_external_pools(true).await })
            .map_err(|err| AdminServiceError::InternalError(err.to_string()))
    }

    pub fn create_external_pool(
        &self,
        request: CreateExternalPoolRequest,
    ) -> Result<ExternalPool, AdminServiceError> {
        let store = self.postgres_store.clone();
        let pool = block_on_admin_store(async move { store.create_external_pool(request).await })
            .map_err(|err| AdminServiceError::InvalidCredential(err.to_string()))?;
        self.audit(
            "create_external_pool",
            "external_pool",
            Some(pool.id.to_string()),
            true,
            None,
            json!({ "name": pool.name, "baseUrl": pool.base_url }),
        );
        Ok(pool)
    }

    pub fn update_external_pool(
        &self,
        id: u64,
        request: UpdateExternalPoolRequest,
    ) -> Result<ExternalPool, AdminServiceError> {
        let store = self.postgres_store.clone();
        let pool =
            block_on_admin_store(async move { store.update_external_pool(id, request).await })
                .map_err(|err| AdminServiceError::InvalidCredential(err.to_string()))?
                .ok_or(AdminServiceError::NotFound { id })?;
        self.audit(
            "update_external_pool",
            "external_pool",
            Some(id.to_string()),
            true,
            None,
            json!({ "name": pool.name, "baseUrl": pool.base_url }),
        );
        Ok(pool)
    }

    pub fn delete_external_pool(&self, id: u64) -> Result<(), AdminServiceError> {
        let store = self.postgres_store.clone();
        let deleted =
            block_on_admin_store(async move { store.soft_delete_external_pool(id).await })
                .map_err(|err| AdminServiceError::InternalError(err.to_string()))?;
        if !deleted {
            return Err(AdminServiceError::NotFound { id });
        }
        self.audit(
            "delete_external_pool",
            "external_pool",
            Some(id.to_string()),
            true,
            None,
            json!({}),
        );
        Ok(())
    }

    pub fn set_external_pool_enabled(
        &self,
        id: u64,
        request: SetExternalPoolEnabledRequest,
    ) -> Result<ExternalPool, AdminServiceError> {
        let store = self.postgres_store.clone();
        let pool = block_on_admin_store(async move {
            store.set_external_pool_enabled(id, request.enabled).await
        })
        .map_err(|err| AdminServiceError::InternalError(err.to_string()))?
        .ok_or(AdminServiceError::NotFound { id })?;
        self.audit(
            "set_external_pool_enabled",
            "external_pool",
            Some(id.to_string()),
            true,
            None,
            json!({ "enabled": request.enabled }),
        );
        Ok(pool)
    }

    pub fn clear_external_pool_auto_disabled(
        &self,
        id: u64,
    ) -> Result<ExternalPool, AdminServiceError> {
        let store = self.postgres_store.clone();
        let pool =
            block_on_admin_store(async move { store.clear_external_pool_auto_disabled(id).await })
                .map_err(|err| AdminServiceError::InternalError(err.to_string()))?
                .ok_or(AdminServiceError::NotFound { id })?;
        self.audit(
            "clear_external_pool_auto_disabled",
            "external_pool",
            Some(id.to_string()),
            true,
            None,
            json!({}),
        );
        Ok(pool)
    }

    pub fn get_external_pool_status(
        &self,
    ) -> Result<ExternalPoolsStatusResponse, AdminServiceError> {
        let manager = self.external_pool_manager.clone();
        let config = self.token_manager.runtime_config().external_pools;
        let pools = block_on_admin_store(async move { manager.status(&config).await })
            .map_err(|err| AdminServiceError::InternalError(err.to_string()))?;
        Ok(ExternalPoolsStatusResponse { pools })
    }

    pub fn test_external_pool(
        &self,
        id: u64,
        req: Option<ExternalPoolTestRequest>,
    ) -> Result<ExternalPoolTestResponse, AdminServiceError> {
        let store = self.postgres_store.clone();
        let pool = block_on_admin_store(async move { store.get_external_pool(id, false).await })
            .map_err(|err| AdminServiceError::InternalError(err.to_string()))?
            .ok_or(AdminServiceError::NotFound { id })?;
        block_on_admin_store(async move {
            let model = req
                .as_ref()
                .map(|req| req.model.trim().to_string())
                .filter(|model| !model.is_empty());
            let url = if model.is_some() {
                external_pool_messages_url(&pool.base_url)?
            } else {
                external_pool_models_url(&pool.base_url)?
            };
            let client = reqwest::Client::builder()
                .timeout(StdDuration::from_secs(15))
                .build()?;
            let mut request = if let Some(model) = model.as_deref() {
                let prompt = req
                    .as_ref()
                    .and_then(|req| req.prompt.as_deref())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .unwrap_or("hi");
                client.post(url).json(&json!({
                    "model": model,
                    "max_tokens": 32,
                    "messages": [
                        {"role": "user", "content": prompt}
                    ],
                    "stream": false
                }))
            } else {
                client.get(url)
            };
            match pool.auth_type {
                crate::external_pool::ExternalPoolAuthType::Bearer => {
                    request = request.bearer_auth(pool.api_key.unwrap_or_default());
                }
                crate::external_pool::ExternalPoolAuthType::XApiKey => {
                    request = request.header("x-api-key", pool.api_key.unwrap_or_default());
                }
            }
            let result = request.send().await;
            Ok::<ExternalPoolTestResponse, anyhow::Error>(match result {
                Ok(response) => {
                    let status = response.status();
                    let body = response.text().await.unwrap_or_default();
                    let response_text = if status.is_success() {
                        extract_external_pool_test_response_text(&body)
                    } else {
                        None
                    };
                    ExternalPoolTestResponse {
                        ok: status.is_success(),
                        status: Some(status.as_u16()),
                        message: if status.is_success() {
                            if model.is_some() {
                                "外部池模型调用测试通过".to_string()
                            } else {
                                "外部池模型列表测试通过".to_string()
                            }
                        } else {
                            let suffix = body.chars().take(300).collect::<String>();
                            if suffix.is_empty() {
                                format!("外部池测试失败: {}", status)
                            } else {
                                format!("外部池测试失败: {}; {}", status, suffix)
                            }
                        },
                        model,
                        response: response_text,
                    }
                }
                Err(err) => ExternalPoolTestResponse {
                    ok: false,
                    status: None,
                    message: err.to_string(),
                    model,
                    response: None,
                },
            })
        })
        .map_err(|err| AdminServiceError::InternalError(err.to_string()))
    }

    pub fn update_admin_api_key(
        &self,
        req: UpdateAdminApiKeyRequest,
    ) -> Result<AccessKeysResponse, AdminServiceError> {
        let next_admin_api_key = req.admin_api_key.trim();
        if next_admin_api_key.is_empty() {
            return Err(AdminServiceError::InvalidCredential(
                "adminApiKey 不能为空".to_string(),
            ));
        }
        if next_admin_api_key.len() < 8 {
            return Err(AdminServiceError::InvalidCredential(
                "adminApiKey 至少需要 8 个字符".to_string(),
            ));
        }

        let next_admin_api_key = next_admin_api_key.to_string();
        self.token_manager
            .update_runtime_config(|config| {
                config.admin_api_key = Some(next_admin_api_key.clone());
            })
            .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;
        self.audit(
            "update_admin_api_key",
            "security_keys",
            Some("adminApiKey".to_string()),
            true,
            None,
            json!({
                "maskedAdminApiKey": mask_secret(&next_admin_api_key),
            }),
        );

        Ok(self.get_access_keys(&next_admin_api_key))
    }

    fn credential_lookup(&self) -> HashMap<u64, CredentialStatusItem> {
        let (_, _, _, _, _, _, _, credentials) =
            self.credential_status_items(CredentialStatusBuildOptions::LIGHT);
        credentials
            .into_iter()
            .map(|credential| (credential.id, credential))
            .collect()
    }

    fn credential_lookup_by_email(&self) -> HashMap<String, CredentialStatusItem> {
        let (_, _, _, _, _, _, _, credentials) =
            self.credential_status_items(CredentialStatusBuildOptions::LIGHT);
        credentials
            .into_iter()
            .filter_map(|credential| {
                let email = credential.email.as_deref()?;
                Some((email_key(email), credential))
            })
            .collect()
    }

    fn credential_from_request(
        &self,
        req: AddCredentialRequest,
        disabled: bool,
    ) -> Result<KiroCredentials, AdminServiceError> {
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

        Ok(KiroCredentials {
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
            max_concurrent_requests: req.max_concurrent_requests,
            region: req.region,
            auth_region: req.auth_region,
            api_region: req.api_region,
            machine_id: req.machine_id,
            email: req.email,
            subscription_title: None,
            proxy_url: req.proxy_url,
            proxy_username: req.proxy_username,
            proxy_password: req.proxy_password,
            proxy_resource_id: req.proxy_resource_id,
            disabled,
            kiro_api_key: req.kiro_api_key,
            endpoint: req.endpoint,
        })
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
        ) = self.credential_status_items(CredentialStatusBuildOptions::LIGHT);

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
    pub fn get_credentials_page(
        &self,
        page: usize,
        limit: usize,
        query: CredentialListQuery,
    ) -> CredentialsPageResponse {
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
        ) = self.credential_status_items(CredentialStatusBuildOptions::LIGHT);
        let filtered: Vec<_> = credentials
            .into_iter()
            .filter(|credential| credential_matches_query(credential, &query))
            .collect();
        let filtered_total = filtered.len();
        let filtered_available = filtered
            .iter()
            .filter(|credential| !credential.disabled)
            .count();
        let total_pages = total_pages(filtered_total, limit);
        let start = page.saturating_sub(1).saturating_mul(limit);
        let credentials = filtered.into_iter().skip(start).take(limit).collect();

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
            filtered_total,
            filtered_available,
            credentials,
        }
    }

    fn credential_status_items(
        &self,
        options: CredentialStatusBuildOptions,
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
        let cost_summary = options
            .include_cost_summary
            .then(|| self.usage_recorder.credential_cost_summary());
        let account_info = if options.include_account_info {
            match block_on_admin_store({
                let store = self.postgres_store.clone();
                async move { store.load_credential_account_info().await }
            }) {
                Ok(info) => Some(info),
                Err(err) => {
                    tracing::warn!("加载凭据账号信息快照失败: {}", err);
                    None
                }
            }
        } else {
            None
        };

        let mut credentials: Vec<CredentialStatusItem> = snapshot
            .entries
            .into_iter()
            .map(|entry| {
                let cost = cost_summary
                    .as_ref()
                    .and_then(|summary| summary.get(&entry.id).copied())
                    .unwrap_or_default();
                let info = account_info
                    .as_ref()
                    .and_then(|info| info.get(&entry.id))
                    .map(account_info_from_row);
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
                    proxy_username: entry.proxy_username,
                    proxy_password: entry.proxy_password,
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
                    max_concurrent_requests_override: entry.max_concurrent_requests_override,
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

        sort_credentials_for_admin_display(&mut credentials);

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
            self.prompt_cache_creation_controller.clear_credential(id);
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

    /// 设置凭据级最大并发覆盖
    pub fn set_credential_concurrency(
        &self,
        id: u64,
        req: SetCredentialConcurrencyRequest,
    ) -> Result<(), AdminServiceError> {
        self.token_manager
            .set_credential_max_concurrent_requests(id, req.max_concurrent_requests)
            .map_err(|e| self.classify_error(e, id))?;
        self.audit(
            "set_credential_concurrency",
            "credential",
            Some(id.to_string()),
            true,
            None,
            json!({ "maxConcurrentRequests": req.max_concurrent_requests }),
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

    /// 获取凭据账号信息（带缓存）。force=true 时绕过 Redis 缓存，适合订阅复查。
    pub async fn get_account_info(
        &self,
        id: u64,
        force: bool,
    ) -> Result<BalanceResponse, AdminServiceError> {
        if !force {
            if let Ok(Some(cached)) = self
                .redis_store
                .get_json::<CachedBalance>(balance_cache_key(id))
                .await
            {
                let now = Utc::now().timestamp() as f64;
                if (now - cached.cached_at) < BALANCE_CACHE_TTL_SECS as f64 {
                    tracing::debug!("凭据 #{} 账号信息命中 Redis 缓存", id);
                    self.save_account_info_snapshot(&cached.data).await?;
                    return Ok(cached.data);
                }
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
            tracing::warn!("保存 Redis 账号信息缓存失败: {}", err);
        }

        Ok(balance)
    }

    /// 兼容旧调用名。
    pub async fn get_balance(&self, id: u64) -> Result<BalanceResponse, AdminServiceError> {
        self.get_account_info(id, false).await
    }

    pub async fn refresh_credentials_info(
        &self,
        req: RefreshCredentialInfoRequest,
    ) -> Result<CredentialInfoRefreshResponse, AdminServiceError> {
        let mut ids = req.ids;
        ids.sort_unstable();
        ids.dedup();
        if ids.is_empty() {
            return Err(AdminServiceError::InvalidCredential(
                "请至少选择一个凭据".to_string(),
            ));
        }
        if ids.len() > MAX_CREDENTIALS_PAGE_LIMIT {
            return Err(AdminServiceError::InvalidCredential(format!(
                "单次最多查询 {} 个凭据",
                MAX_CREDENTIALS_PAGE_LIMIT
            )));
        }

        let labels = self.credential_lookup();
        let mut items = Vec::with_capacity(ids.len());
        let mut success = 0;
        for id in ids {
            let label = labels.get(&id);
            match self.get_account_info(id, req.force).await {
                Ok(info) => {
                    success += 1;
                    items.push(CredentialInfoRefreshItem {
                        id,
                        email: label.and_then(|item| item.email.clone()),
                        disabled: label.map(|item| item.disabled).unwrap_or(false),
                        ok: true,
                        info: Some(info),
                        error: None,
                    });
                }
                Err(err) => items.push(CredentialInfoRefreshItem {
                    id,
                    email: label.and_then(|item| item.email.clone()),
                    disabled: label.map(|item| item.disabled).unwrap_or(false),
                    ok: false,
                    info: None,
                    error: Some(err.to_string()),
                }),
            }
        }
        let total = items.len();
        Ok(CredentialInfoRefreshResponse {
            total,
            success,
            failed: total.saturating_sub(success),
            items,
        })
    }

    pub async fn validate_existing_credentials(
        &self,
        req: ValidateExistingCredentialsRequest,
    ) -> Result<CredentialValidationResponse, AdminServiceError> {
        let previous = self
            .postgres_store
            .load_credential_account_info()
            .await
            .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;
        let requested_ids: HashSet<u64> = req.ids.into_iter().collect();
        let scope = req
            .scope
            .unwrap_or_else(|| "all".to_string())
            .to_lowercase();
        let (_, _, _, _, _, _, _, credentials) =
            self.credential_status_items(CredentialStatusBuildOptions::LIGHT);
        let candidates: Vec<_> = credentials
            .into_iter()
            .filter(|credential| {
                if !requested_ids.is_empty() {
                    return requested_ids.contains(&credential.id);
                }
                match scope.as_str() {
                    "enabled" => !credential.disabled,
                    "disabled" => credential.disabled,
                    "selected" => false,
                    _ => true,
                }
            })
            .collect();
        if candidates.is_empty() {
            return Err(AdminServiceError::InvalidCredential(
                "没有可校验的凭据".to_string(),
            ));
        }

        let mut items = Vec::with_capacity(candidates.len());
        for credential in candidates {
            let previous_info = previous.get(&credential.id).map(validation_info_from_row);
            match self.get_account_info(credential.id, req.force).await {
                Ok(current) => {
                    let current_info = validation_info_from_balance(&current);
                    let change_kind =
                        compare_subscription_change(previous_info.as_ref(), Some(&current_info));
                    let subscription_title = current_info
                        .subscription_title
                        .clone()
                        .unwrap_or_else(|| "未知".to_string());
                    let subscription_key = subscription_key(Some(&subscription_title));
                    items.push(CredentialValidationItem {
                        id: Some(credential.id),
                        index: None,
                        email: credential.email.clone(),
                        disabled: Some(credential.disabled),
                        ok: true,
                        previous: previous_info,
                        current: Some(current_info),
                        change_kind,
                        subscription_key,
                        subscription_title,
                        error: None,
                        matched_existing_credential_id: None,
                        existing_disabled: None,
                    });
                }
                Err(err) => items.push(CredentialValidationItem {
                    id: Some(credential.id),
                    index: None,
                    email: credential.email.clone(),
                    disabled: Some(credential.disabled),
                    ok: false,
                    previous: previous_info,
                    current: None,
                    change_kind: "failed".to_string(),
                    subscription_key: "failed".to_string(),
                    subscription_title: "查询失败".to_string(),
                    error: Some(err.to_string()),
                    matched_existing_credential_id: None,
                    existing_disabled: None,
                }),
            }
        }

        Ok(build_validation_response(items))
    }

    pub async fn validate_external_credentials(
        &self,
        req: ValidateExternalCredentialsRequest,
    ) -> Result<CredentialValidationResponse, AdminServiceError> {
        if req.credentials.is_empty() {
            return Err(AdminServiceError::InvalidCredential(
                "请先提供要校验的凭据 JSON".to_string(),
            ));
        }
        if req.credentials.len() > MAX_CREDENTIALS_PAGE_LIMIT {
            return Err(AdminServiceError::InvalidCredential(format!(
                "单次最多校验 {} 个凭据",
                MAX_CREDENTIALS_PAGE_LIMIT
            )));
        }

        let existing_by_email = self.credential_lookup_by_email();
        let mut items = Vec::with_capacity(req.credentials.len());
        for (index, req) in req.credentials.into_iter().enumerate() {
            let email = req.email.clone();
            let matched = email
                .as_deref()
                .and_then(|value| existing_by_email.get(&email_key(value)))
                .cloned();
            let credential = match self.credential_from_request(req, false) {
                Ok(credential) => credential,
                Err(err) => {
                    items.push(CredentialValidationItem {
                        id: None,
                        index: Some(index + 1),
                        email,
                        disabled: None,
                        ok: false,
                        previous: None,
                        current: None,
                        change_kind: "failed".to_string(),
                        subscription_key: "failed".to_string(),
                        subscription_title: "解析失败".to_string(),
                        error: Some(err.to_string()),
                        matched_existing_credential_id: matched.as_ref().map(|item| item.id),
                        existing_disabled: matched.as_ref().map(|item| item.disabled),
                    });
                    continue;
                }
            };

            match self
                .token_manager
                .probe_usage_limits_for_credentials(credential)
                .await
            {
                Ok(usage) => {
                    let current = validation_info_from_usage(&usage);
                    let subscription_title = current
                        .subscription_title
                        .clone()
                        .unwrap_or_else(|| "未知".to_string());
                    items.push(CredentialValidationItem {
                        id: None,
                        index: Some(index + 1),
                        email,
                        disabled: None,
                        ok: true,
                        previous: None,
                        current: Some(current),
                        change_kind: "external".to_string(),
                        subscription_key: subscription_key(Some(&subscription_title)),
                        subscription_title,
                        error: None,
                        matched_existing_credential_id: matched.as_ref().map(|item| item.id),
                        existing_disabled: matched.as_ref().map(|item| item.disabled),
                    });
                }
                Err(err) => items.push(CredentialValidationItem {
                    id: None,
                    index: Some(index + 1),
                    email,
                    disabled: None,
                    ok: false,
                    previous: None,
                    current: None,
                    change_kind: "failed".to_string(),
                    subscription_key: "failed".to_string(),
                    subscription_title: "查询失败".to_string(),
                    error: Some(err.to_string()),
                    matched_existing_credential_id: matched.as_ref().map(|item| item.id),
                    existing_disabled: matched.as_ref().map(|item| item.disabled),
                }),
            }
        }

        Ok(build_validation_response(items))
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

        let runtime_config = self.token_manager.runtime_config();
        let model_resolution = self.model_capabilities.resolve_model_with_mapping(
            model,
            runtime_config.model_resolution_mode,
            &runtime_config.model_mapping,
        );
        if model_resolution.source == ModelResolutionSource::Unsupported {
            return Err(AdminServiceError::InvalidCredential(format!(
                "不支持的测试模型: {}",
                model
            )));
        }
        let model_id = model_resolution.upstream_model.clone().ok_or_else(|| {
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

    /// 从上游获取账号信息（无缓存）
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
        let email = req.email.clone();
        let warmup_remaining = req.warmup_remaining;
        let disabled = req.disabled.unwrap_or(false);
        let new_cred = self.credential_from_request(req, disabled)?;

        let credential_id = self
            .token_manager
            .add_credential(new_cred)
            .await
            .map_err(|e| self.classify_add_error(e))?;
        if let Some(warmup_remaining) = warmup_remaining {
            self.token_manager
                .set_warmup_remaining(credential_id, warmup_remaining)
                .map_err(|e| self.classify_error(e, credential_id))?;
        }

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

    pub fn update_credential_auth(
        &self,
        id: u64,
        req: UpdateCredentialAuthRequest,
    ) -> Result<(), AdminServiceError> {
        if let Some(ref name) = req.endpoint {
            if !name.trim().is_empty() && !self.known_endpoints.contains(name) {
                let mut known: Vec<&str> =
                    self.known_endpoints.iter().map(|s| s.as_str()).collect();
                known.sort();
                return Err(AdminServiceError::InvalidCredential(format!(
                    "未知端点 \"{}\"，已注册端点: {:?}",
                    name, known
                )));
            }
        }

        let reset_runtime_state = req.reset_runtime_state;
        let update = CredentialAuthUpdate {
            refresh_token: req.refresh_token,
            auth_method: req.auth_method,
            client_id: req.client_id,
            client_secret: req.client_secret,
            kiro_api_key: req.kiro_api_key,
            auth_region: req.auth_region,
            api_region: req.api_region,
            machine_id: req.machine_id,
            email: req.email,
            endpoint: req.endpoint,
        };
        self.token_manager
            .update_credential_auth(id, update, reset_runtime_state)
            .map_err(|e| self.classify_error(e, id))?;
        self.invalidate_balance_cache(id);
        self.audit(
            "update_credential_auth",
            "credential",
            Some(id.to_string()),
            true,
            None,
            json!({ "resetRuntimeState": reset_runtime_state }),
        );
        Ok(())
    }

    pub async fn batch_import_credentials(
        &self,
        req: BatchCredentialImportRequest,
    ) -> Result<BatchCredentialImportResponse, AdminServiceError> {
        if req.credentials.is_empty() {
            return Err(AdminServiceError::InvalidCredential(
                "没有可导入的凭据".to_string(),
            ));
        }
        if req.credentials.len() > MAX_CREDENTIALS_PAGE_LIMIT {
            return Err(AdminServiceError::InvalidCredential(format!(
                "单次最多导入 {} 个凭据",
                MAX_CREDENTIALS_PAGE_LIMIT
            )));
        }

        let total = req.credentials.len();
        let mut items = Vec::with_capacity(total);
        for (index, credential) in req.credentials.into_iter().enumerate() {
            let import_index = index + 1;
            let credential = apply_batch_import_defaults(credential, &req.defaults);
            let email = credential.email.clone();
            match self.add_credential(credential).await {
                Ok(response) => items.push(BatchCredentialImportItem {
                    index: import_index,
                    ok: true,
                    skipped: false,
                    credential_id: Some(response.credential_id),
                    email: response.email.or(email),
                    error: None,
                }),
                Err(err)
                    if req.duplicate_mode == BatchCredentialImportDuplicateMode::Skip
                        && is_duplicate_credential_error(&err) =>
                {
                    items.push(BatchCredentialImportItem {
                        index: import_index,
                        ok: true,
                        skipped: true,
                        credential_id: None,
                        email,
                        error: Some(err.to_string()),
                    });
                }
                Err(err) => {
                    let message = err.to_string();
                    items.push(BatchCredentialImportItem {
                        index: import_index,
                        ok: false,
                        skipped: false,
                        credential_id: None,
                        email,
                        error: Some(message.clone()),
                    });
                    if !req.continue_on_error {
                        break;
                    }
                }
            }
        }

        let success = items.iter().filter(|item| item.ok && !item.skipped).count();
        let skipped = items.iter().filter(|item| item.skipped).count();
        let failed = items.iter().filter(|item| !item.ok).count();
        self.audit(
            "batch_import_credentials",
            "credential",
            None,
            failed == 0,
            (failed > 0).then(|| format!("{} 条导入失败", failed)),
            json!({ "total": total, "success": success, "skipped": skipped, "failed": failed }),
        );
        Ok(BatchCredentialImportResponse {
            total,
            success,
            skipped,
            failed,
            items,
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
        let delete_result = block_on_admin_store({
            let store = self.postgres_store.clone();
            async move { store.soft_delete_proxy_resource_if_unbound(id).await }
        })
        .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;
        match delete_result {
            None => return Err(AdminServiceError::NotFound { id }),
            Some(credential_count) if credential_count > 0 => {
                return Err(AdminServiceError::Conflict(format!(
                    "代理资源仍有 {} 个凭据绑定，请先解绑后再删除",
                    credential_count
                )));
            }
            Some(_) => {}
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
        self.prompt_cache_creation_controller.clear_credential(id);

        // 清理已删除凭据的账号信息缓存
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

    /// 获取 PgSQL 聚合的 usage 仪表盘数据。
    ///
    /// 该接口不走 UsageRecorder 的内存记录兜底，避免仪表盘统计受进程内缓存大小影响。
    pub fn get_usage_dashboard(
        &self,
        timezone: Option<String>,
    ) -> Result<UsageDashboardResponse, AdminServiceError> {
        let high_cache_threshold = self.token_manager.runtime_config().high_cache_threshold;
        let usage_store = PostgresUsageStore::new(self.postgres_store.clone());
        block_on_admin_store(async move {
            usage_store
                .dashboard(timezone.as_deref(), high_cache_threshold)
                .await
        })
        .map_err(|err| AdminServiceError::InternalError(err.to_string()))
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
        let capability_models = self
            .model_capabilities
            .status()
            .models
            .into_iter()
            .map(|item| item.model);
        let mut status = self
            .pricing_catalog
            .sync_for_models(capability_models)
            .await;
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

    /// 添加或更新手动模型补充。手动项参与模型解析和可选计价；后续上游同步同名模型会覆盖手动来源。
    pub async fn upsert_manual_model(
        &self,
        req: UpsertManualModelRequest,
    ) -> Result<ManualModelResponse, AdminServiceError> {
        let item = manual_model_item_from_request(req.clone())?;
        let clear_pricing = req.clear_pricing;
        let pricing = req
            .pricing
            .as_ref()
            .map(|pricing| {
                pricing.to_pricing().ok_or_else(|| {
                    AdminServiceError::InvalidCredential(
                        "手动模型价格必须是有效的非负数字".to_string(),
                    )
                })
            })
            .transpose()?;
        let has_pricing = pricing.is_some();
        self.postgres_store
            .save_manual_model(&item, pricing, clear_pricing)
            .await
            .map_err(|err| {
                AdminServiceError::InternalError(format!("保存手动模型失败: {}", err))
            })?;
        self.model_capabilities.upsert_manual_model(item.clone());

        if let Some(pricing) = pricing {
            self.pricing_catalog
                .upsert_manual_price(&item.model, pricing);
        } else if clear_pricing {
            self.pricing_catalog.delete_manual_price(&item.model);
        }

        self.audit(
            "upsert_manual_model",
            "model_capabilities",
            Some(item.model.clone()),
            true,
            None,
            json!({
                "model": item.model,
                "hasPricing": has_pricing,
                "clearPricing": clear_pricing,
                "source": MANUAL_SOURCE,
            }),
        );
        Ok(ManualModelResponse::new(item.model, "手动模型已保存"))
    }

    /// 删除手动模型补充。只允许删除 source=manual 的模型，避免误删上游同步模型。
    pub async fn delete_manual_model(
        &self,
        model: String,
    ) -> Result<ManualModelResponse, AdminServiceError> {
        let model = normalize_model_id(&model);
        validate_manual_model_id(&model)?;
        let removed = self
            .postgres_store
            .delete_manual_model(&model)
            .await
            .map_err(|err| {
                AdminServiceError::InternalError(format!("删除手动模型失败: {}", err))
            })?;
        self.model_capabilities.delete_manual_model(&model);
        self.pricing_catalog.delete_manual_price(&model);
        if !removed {
            return Err(AdminServiceError::InvalidCredential(format!(
                "手动模型不存在或该模型不是手动添加: {}",
                model
            )));
        }
        self.audit(
            "delete_manual_model",
            "model_capabilities",
            Some(model.clone()),
            true,
            None,
            json!({ "model": model, "source": MANUAL_SOURCE }),
        );
        Ok(ManualModelResponse::new(model, "手动模型已删除"))
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

    pub fn preview_usage_cleanup(
        &self,
        request: UsageCleanupRequest,
    ) -> Result<UsageCleanupPreviewResponse, AdminServiceError> {
        let plan = normalize_usage_cleanup_request(request)?;
        let store = PostgresUsageStore::new(self.postgres_store.clone());
        let preview = block_on_admin_store(async move {
            match plan.mode {
                UsageCleanupMode::SoftDelete => {
                    store.preview_soft_delete_cleanup(plan.cutoff).await
                }
                UsageCleanupMode::HardDelete => {
                    store.preview_hard_delete_cleanup(plan.cutoff).await
                }
            }
        })
        .map_err(|err| AdminServiceError::InternalError(err.to_string()))?;

        Ok(UsageCleanupPreviewResponse {
            mode: plan.mode,
            cutoff_at: plan.cutoff.to_rfc3339(),
            matched_rows: preview.matched_rows,
            oldest_created_at: preview.oldest_created_at.map(|value| value.to_rfc3339()),
            newest_created_at: preview.newest_created_at.map(|value| value.to_rfc3339()),
        })
    }

    pub fn start_usage_cleanup(
        &self,
        request: UsageCleanupRequest,
    ) -> Result<UsageCleanupStatusResponse, AdminServiceError> {
        let plan = normalize_usage_cleanup_request(request)?;
        let store = PostgresUsageStore::new(self.postgres_store.clone());
        let preview = block_on_admin_store({
            let store = store.clone();
            let plan = plan.clone();
            async move {
                match plan.mode {
                    UsageCleanupMode::SoftDelete => {
                        store.preview_soft_delete_cleanup(plan.cutoff).await
                    }
                    UsageCleanupMode::HardDelete => {
                        store.preview_hard_delete_cleanup(plan.cutoff).await
                    }
                }
            }
        })
        .map_err(|err| AdminServiceError::InternalError(err.to_string()))?;

        let now = Utc::now();
        let job_id = format!("usage-cleanup-{}", now.timestamp_millis());
        let cancel = Arc::new(AtomicBool::new(false));
        let status = UsageCleanupStatusResponse {
            job_id: Some(job_id.clone()),
            status: UsageCleanupJobStatus::Running,
            mode: Some(plan.mode),
            cutoff_at: Some(plan.cutoff.to_rfc3339()),
            batch_size: plan.batch_size,
            max_batches: plan.max_batches,
            pause_ms_between_batches: plan.pause_ms_between_batches,
            matched_rows: Some(preview.matched_rows),
            remaining_rows: Some(preview.matched_rows),
            processed_rows: 0,
            last_batch_rows: 0,
            batches: 0,
            cancel_requested: false,
            stop_reason: None,
            started_at: Some(now.to_rfc3339()),
            updated_at: Some(now.to_rfc3339()),
            finished_at: None,
            last_error: None,
        };

        {
            let mut runtime = self.usage_cleanup.lock();
            if runtime.status.status == UsageCleanupJobStatus::Running {
                return Err(AdminServiceError::Conflict(
                    "已有 usage 清理任务正在运行".to_string(),
                ));
            }
            runtime.status = status;
            runtime.cancel = Some(cancel.clone());
        }

        self.audit(
            "start_usage_cleanup",
            "usage_record",
            None,
            true,
            None,
            json!({
                "jobId": job_id,
                "mode": plan.mode,
                "cutoffAt": plan.cutoff.to_rfc3339(),
                "batchSize": plan.batch_size,
                "maxBatches": plan.max_batches,
                "matchedRows": preview.matched_rows,
            }),
        );

        let cleanup_state = self.usage_cleanup.clone();
        tokio::spawn(async move {
            run_usage_cleanup_job(job_id, store, cleanup_state, cancel, plan).await;
        });

        Ok(self.get_usage_cleanup_status())
    }

    pub fn get_usage_cleanup_status(&self) -> UsageCleanupStatusResponse {
        self.usage_cleanup.lock().status.clone()
    }

    pub fn cancel_usage_cleanup(&self) -> UsageCleanupStatusResponse {
        let mut runtime = self.usage_cleanup.lock();
        if runtime.status.status == UsageCleanupJobStatus::Running {
            if let Some(cancel) = &runtime.cancel {
                cancel.store(true, Ordering::Release);
            }
            runtime.status.cancel_requested = true;
            runtime.status.updated_at = Some(Utc::now().to_rfc3339());
        }
        runtime.status.clone()
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
            proxy_url: config.proxy_url.clone(),
            proxy_username: config.proxy_username.clone(),
            proxy_password: config.proxy_password.clone(),
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
            credential_retry_max_attempts: config.credential_retry_max_attempts,
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
            payload_guard_enabled: config.payload_guard_enabled,
            payload_guard_mode: config.payload_guard_mode,
            payload_guard_max_bytes: config.payload_guard_max_bytes as u64,
            payload_guard_safety_margin_bytes: config.payload_guard_safety_margin_bytes as u64,
            payload_guard_trim_history: config.payload_guard_trim_history,
            payload_shaping: config.payload_shaping,
            prompt_cache_target_read_ratio: config.prompt_cache_target_read_ratio,
            prompt_cache_token_scale: config.prompt_cache_token_scale,
            prompt_cache_max_simulated_input_tokens: config.prompt_cache_max_simulated_input_tokens,
            prompt_cache_cap_jitter_min_tokens: config.prompt_cache_cap_jitter_min_tokens,
            prompt_cache_cap_jitter_max_tokens: config.prompt_cache_cap_jitter_max_tokens,
            prompt_cache_scale_min_input_tokens: config.prompt_cache_scale_min_input_tokens,
            prompt_cache_creation_control: config.prompt_cache_creation_control.normalized(),
            reported_usage: config.reported_usage.normalized(),
            external_pools: config.external_pools.clone(),
            high_cache_threshold: config.high_cache_threshold,
            compat_profile: config.compat_profile,
            model_resolution_mode: config.model_resolution_mode,
            model_mapping: config.model_mapping.clone().normalized(),
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
        let credential_retry_max_attempts = req
            .credential_retry_max_attempts
            .unwrap_or(current_config.credential_retry_max_attempts);
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
        let payload_guard_enabled = req
            .payload_guard_enabled
            .unwrap_or(current_config.payload_guard_enabled);
        let payload_guard_mode = req
            .payload_guard_mode
            .unwrap_or(current_config.payload_guard_mode);
        let payload_guard_max_bytes = req
            .payload_guard_max_bytes
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(current_config.payload_guard_max_bytes);
        let payload_guard_safety_margin_bytes = req
            .payload_guard_safety_margin_bytes
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(current_config.payload_guard_safety_margin_bytes);
        let payload_guard_trim_history = req
            .payload_guard_trim_history
            .unwrap_or(current_config.payload_guard_trim_history);
        let payload_shaping = req
            .payload_shaping
            .map(|patch| patch.apply_to(current_config.payload_shaping))
            .unwrap_or(current_config.payload_shaping);
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
        let prompt_cache_creation_control = req
            .prompt_cache_creation_control
            .unwrap_or(current_config.prompt_cache_creation_control)
            .normalized();
        let reported_usage = req
            .reported_usage
            .clone()
            .unwrap_or_else(|| current_config.reported_usage.clone())
            .normalized();
        let external_pools = req
            .external_pools
            .clone()
            .unwrap_or_else(|| current_config.external_pools.clone());
        let high_cache_threshold = req
            .high_cache_threshold
            .unwrap_or(current_config.high_cache_threshold);
        let compat_profile = req.compat_profile.unwrap_or(current_config.compat_profile);
        let model_resolution_mode = req
            .model_resolution_mode
            .unwrap_or(current_config.model_resolution_mode);
        let model_mapping = req
            .model_mapping
            .clone()
            .unwrap_or_else(|| current_config.model_mapping.clone())
            .normalized();
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
        if credential_retry_max_attempts > 10_000 {
            return Err(AdminServiceError::InvalidCredential(
                "credentialRetryMaxAttempts 不能大于 10000".to_string(),
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
        if payload_guard_enabled
            && payload_guard_max_bytes > 0
            && payload_guard_max_bytes < 64 * 1024
        {
            return Err(AdminServiceError::InvalidCredential(
                "payloadGuardMaxBytes 启用时必须为 0 或不小于 65536".to_string(),
            ));
        }
        if payload_guard_enabled
            && payload_guard_max_bytes > 0
            && payload_guard_max_bytes.saturating_sub(payload_guard_safety_margin_bytes) < 64 * 1024
        {
            return Err(AdminServiceError::InvalidCredential(
                "payloadGuardSafetyMarginBytes 不能让实际裁剪目标小于 65536".to_string(),
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
        prompt_cache_creation_control
            .validate()
            .map_err(AdminServiceError::InvalidCredential)?;
        validate_external_pools_config(&external_pools)
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
                config.credential_retry_max_attempts = credential_retry_max_attempts;
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
                config.payload_guard_enabled = payload_guard_enabled;
                config.payload_guard_mode = payload_guard_mode;
                config.payload_guard_max_bytes = payload_guard_max_bytes;
                config.payload_guard_safety_margin_bytes = payload_guard_safety_margin_bytes;
                config.payload_guard_trim_history = payload_guard_trim_history;
                config.payload_shaping = payload_shaping;
                config.prompt_cache_target_read_ratio = prompt_cache_target_read_ratio;
                config.prompt_cache_token_scale = prompt_cache_token_scale;
                config.prompt_cache_max_simulated_input_tokens =
                    prompt_cache_max_simulated_input_tokens;
                config.prompt_cache_cap_jitter_min_tokens = prompt_cache_cap_jitter_min_tokens;
                config.prompt_cache_cap_jitter_max_tokens = prompt_cache_cap_jitter_max_tokens;
                config.prompt_cache_scale_min_input_tokens = prompt_cache_scale_min_input_tokens;
                config.prompt_cache_creation_control = prompt_cache_creation_control;
                config.reported_usage = reported_usage;
                config.external_pools = external_pools;
                config.high_cache_threshold = high_cache_threshold;
                config.compat_profile = compat_profile;
                config.model_resolution_mode = model_resolution_mode;
                config.model_mapping = model_mapping;
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

    /// 分类账号信息查询错误（可能涉及上游 API 调用）
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
            || msg.contains("代理资源不存在")
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

fn manual_model_item_from_request(
    req: UpsertManualModelRequest,
) -> Result<ModelCapabilityItem, AdminServiceError> {
    let model = normalize_model_id(&req.model);
    validate_manual_model_id(&model)?;
    let display_name = req
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(&model)
        .to_string();
    if let Some(value) = req.max_input_tokens {
        if value <= 0 {
            return Err(AdminServiceError::InvalidCredential(
                "输入上限必须大于 0，或留空".to_string(),
            ));
        }
    }
    if let Some(value) = req.max_output_tokens {
        if value <= 0 {
            return Err(AdminServiceError::InvalidCredential(
                "输出上限必须大于 0，或留空".to_string(),
            ));
        }
    }
    Ok(ModelCapabilityItem {
        model,
        display_name,
        description: req
            .description
            .and_then(|value| (!value.trim().is_empty()).then(|| value.trim().to_string())),
        max_input_tokens: req.max_input_tokens,
        max_output_tokens: req.max_output_tokens,
        supports_prompt_caching: req.supports_prompt_caching,
        supported_input_types: normalize_supported_input_types(req.supported_input_types),
        source: Some(MANUAL_SOURCE.to_string()),
    })
}

fn validate_manual_model_id(model: &str) -> Result<(), AdminServiceError> {
    if model.is_empty() {
        return Err(AdminServiceError::InvalidCredential(
            "模型 ID 不能为空".to_string(),
        ));
    }
    if model.len() > MAX_MANUAL_MODEL_ID_LEN {
        return Err(AdminServiceError::InvalidCredential(format!(
            "模型 ID 不能超过 {} 个字符",
            MAX_MANUAL_MODEL_ID_LEN
        )));
    }
    if !model.chars().all(|ch| {
        ch.is_ascii_lowercase()
            || ch.is_ascii_digit()
            || matches!(ch, '-' | '_' | '.' | ':' | '[' | ']')
    }) {
        return Err(AdminServiceError::InvalidCredential(
            "模型 ID 只能包含小写字母、数字、-、_、.、:、[、]".to_string(),
        ));
    }
    Ok(())
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
            "代理 URL 必须是 http://、https://、socks5:// 或 socks5h:// 开头的完整地址".to_string(),
        )
    })?;
    match parsed.scheme() {
        "http" | "https" | "socks5" | "socks5h" => Ok(value.to_string()),
        scheme => Err(AdminServiceError::InvalidCredential(format!(
            "不支持的代理协议: {}，仅支持 http/https/socks5/socks5h",
            scheme
        ))),
    }
}

fn validate_external_pools_config(config: &ExternalPoolsConfig) -> Result<(), String> {
    if config.external_pool_global_max_concurrent_requests > 100_000 {
        return Err("externalPoolGlobalMaxConcurrentRequests 不能大于 100000".to_string());
    }
    if config.external_pool_max_queued_requests > 100_000 {
        return Err("externalPoolMaxQueuedRequests 不能大于 100000".to_string());
    }
    if config.external_pool_dispatch_max_wait_secs > 86_400 {
        return Err("externalPoolDispatchMaxWaitSecs 不能大于 86400".to_string());
    }
    if config.external_pool_retry_max_attempts > 10_000 {
        return Err("externalPoolRetryMaxAttempts 不能大于 10000".to_string());
    }
    if config.external_pool_local_rescue_max_wait_secs > 300 {
        return Err("externalPoolLocalRescueMaxWaitSecs 不能大于 300".to_string());
    }
    if config.direct_external_model_rules.len() > 200 {
        return Err("directExternalModelRules 不能超过 200 条".to_string());
    }
    if config.direct_external_path_rules.len() > 200 {
        return Err("directExternalPathRules 不能超过 200 条".to_string());
    }
    if config
        .direct_external_model_rules
        .iter()
        .chain(config.direct_external_path_rules.iter())
        .any(|rule| rule.len() > 256)
    {
        return Err("directExternal 规则单条长度不能超过 256".to_string());
    }
    if config.local_pool_circuit_window_secs == 0
        || config.local_pool_circuit_window_secs > 24 * 60 * 60
    {
        return Err("localPoolCircuitWindowSecs 必须在 1 到 86400 之间".to_string());
    }
    if config.local_pool_circuit_open_after_failures == 0
        || config.local_pool_circuit_open_after_failures > 10_000
    {
        return Err("localPoolCircuitOpenAfterFailures 必须在 1 到 10000 之间".to_string());
    }
    if config.local_pool_circuit_require_distinct_credentials > 10_000 {
        return Err("localPoolCircuitRequireDistinctCredentials 不能大于 10000".to_string());
    }
    if config.local_pool_circuit_open_secs == 0
        || config.local_pool_circuit_open_secs > 24 * 60 * 60
    {
        return Err("localPoolCircuitOpenSecs 必须在 1 到 86400 之间".to_string());
    }
    if config.local_pool_circuit_half_open_max_probes == 0
        || config.local_pool_circuit_half_open_max_probes > 10_000
    {
        return Err("localPoolCircuitHalfOpenMaxProbes 必须在 1 到 10000 之间".to_string());
    }
    if config.external_pool_auto_disable_failure_threshold == 0
        || config.external_pool_auto_disable_failure_threshold > 10_000
    {
        return Err("externalPoolAutoDisableFailureThreshold 必须在 1 到 10000 之间".to_string());
    }
    if config.external_pool_auto_disable_window_secs == 0
        || config.external_pool_auto_disable_window_secs > 24 * 60 * 60
    {
        return Err("externalPoolAutoDisableWindowSecs 必须在 1 到 86400 之间".to_string());
    }
    if config.external_pool_auto_disable_duration_secs > 365 * 24 * 60 * 60 {
        return Err("externalPoolAutoDisableDurationSecs 不能超过 365 天".to_string());
    }
    if config.external_pool_rate_limit_cooldown_secs == 0
        || config.external_pool_server_error_cooldown_secs == 0
        || config.external_pool_network_error_cooldown_secs == 0
        || config.external_pool_protocol_error_cooldown_secs == 0
    {
        return Err("外部池错误冷却秒数必须大于 0".to_string());
    }
    if config.external_pool_request_timeout_secs > 86_400 {
        return Err("externalPoolRequestTimeoutSecs 不能大于 86400".to_string());
    }
    if config.external_pool_stream_request_timeout_secs > 86_400 {
        return Err("externalPoolStreamRequestTimeoutSecs 不能大于 86400".to_string());
    }
    if config.external_pool_stream_idle_timeout_secs > 86_400 {
        return Err("externalPoolStreamIdleTimeoutSecs 不能大于 86400".to_string());
    }
    if config.external_pool_usage_projection_uplift_percent > 200 {
        return Err("externalPoolUsageProjectionUpliftPercent 不能大于 200".to_string());
    }
    if config.external_pool_usage_projection_output_uplift_min_tokens < 0 {
        return Err("externalPoolUsageProjectionOutputUpliftMinTokens 不能小于 0".to_string());
    }
    if config.external_pool_usage_projection_output_uplift_percent > 200 {
        return Err("externalPoolUsageProjectionOutputUpliftPercent 不能大于 200".to_string());
    }
    Ok(())
}

fn proxy_resource_response(row: ProxyResourceRow) -> ProxyResourceResponse {
    let has_password = row
        .proxy_password
        .as_deref()
        .map(|value| !value.is_empty())
        .unwrap_or(false);
    ProxyResourceResponse {
        id: row.id,
        name: row.name,
        proxy_url: row.proxy_url,
        proxy_username: row.proxy_username,
        proxy_password: row.proxy_password,
        has_password,
        enabled: row.enabled,
        notes: row.notes,
        created_at: row.created_at,
        updated_at: row.updated_at,
        credential_count: row.credential_count,
    }
}

fn apply_batch_import_defaults(
    mut credential: AddCredentialRequest,
    defaults: &BatchCredentialImportDefaults,
) -> AddCredentialRequest {
    if credential.disabled.is_none() {
        credential.disabled = defaults.disabled;
    }
    if credential.priority == 0 {
        if let Some(priority) = defaults.priority {
            credential.priority = priority;
        }
    }
    if credential.max_concurrent_requests.is_none() {
        if let Some(max_concurrent_requests) = defaults.max_concurrent_requests {
            credential.max_concurrent_requests = max_concurrent_requests;
        }
    }
    if credential.auth_region.as_deref().is_none_or(str::is_empty) {
        credential.auth_region = defaults.auth_region.clone();
    }
    if credential.api_region.as_deref().is_none_or(str::is_empty) {
        credential.api_region = defaults.api_region.clone();
    }
    if credential.proxy_url.as_deref().is_none_or(str::is_empty) {
        credential.proxy_url = defaults.proxy_url.clone();
    }
    if credential
        .proxy_username
        .as_deref()
        .is_none_or(str::is_empty)
    {
        credential.proxy_username = defaults.proxy_username.clone();
    }
    if credential
        .proxy_password
        .as_deref()
        .is_none_or(str::is_empty)
    {
        credential.proxy_password = defaults.proxy_password.clone();
    }
    if credential.proxy_resource_id.is_none() {
        if let Some(proxy_resource_id) = defaults.proxy_resource_id {
            credential.proxy_resource_id = proxy_resource_id;
        }
    }
    if credential.endpoint.as_deref().is_none_or(str::is_empty) {
        credential.endpoint = defaults.endpoint.clone();
    }
    if credential.warmup_remaining.is_none() {
        credential.warmup_remaining = defaults.warmup_remaining;
    }
    credential
}

fn is_duplicate_credential_error(err: &AdminServiceError) -> bool {
    let message = err.to_string();
    message.contains("凭据已存在")
        || message.contains("kiroApiKey 重复")
        || message.contains("refreshToken 重复")
}

fn extract_external_pool_test_response_text(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    value
        .get("content")
        .and_then(|content| content.as_array())
        .and_then(|items| {
            items.iter().find_map(|item| {
                item.get("text")
                    .and_then(|text| text.as_str())
                    .map(|text| text.trim().to_string())
                    .filter(|text| !text.is_empty())
            })
        })
        .or_else(|| {
            value
                .get("choices")
                .and_then(|choices| choices.as_array())
                .and_then(|choices| choices.first())
                .and_then(|choice| choice.get("message"))
                .and_then(|message| message.get("content"))
                .and_then(|content| content.as_str())
                .map(|text| text.trim().to_string())
                .filter(|text| !text.is_empty())
        })
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

fn sort_credentials_for_admin_display(credentials: &mut [CredentialStatusItem]) {
    credentials.sort_by(|a, b| {
        a.disabled
            .cmp(&b.disabled)
            .then_with(|| match (&a.created_at, &b.created_at) {
                (Some(a_created), Some(b_created)) => b_created.cmp(a_created),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            })
            .then_with(|| b.id.cmp(&a.id))
    });
}

fn credential_matches_query(
    credential: &CredentialStatusItem,
    query: &CredentialListQuery,
) -> bool {
    if let Some(proxy_resource_id) = query.proxy_resource_id {
        if credential.proxy_resource_id != Some(proxy_resource_id) {
            return false;
        }
    }

    if let Some(auth_method) = query.auth_method.as_deref() {
        let expected = auth_method.trim().to_lowercase();
        if !expected.is_empty()
            && expected != "all"
            && credential
                .auth_method
                .as_deref()
                .map(|value| value.to_lowercase())
                .as_deref()
                != Some(expected.as_str())
        {
            return false;
        }
    }

    if let Some(status) = query.status.as_deref() {
        let status = status.trim().to_lowercase();
        let matched = match status.as_str() {
            "" | "all" => true,
            "enabled" => !credential.disabled,
            "disabled" => credential.disabled,
            "current" => credential.is_current,
            "cooldown" => credential.cooled_down,
            "rate_limited" | "rate-limited" => credential.rate_limited,
            "proxy_blocked" | "proxy-blocked" => matches!(
                credential.effective_proxy_source.as_str(),
                "resource_disabled" | "resource_missing"
            ),
            "error" => {
                credential.failure_count > 0
                    || credential.refresh_failure_count > 0
                    || credential.last_error_kind.is_some()
            }
            "unknown_subscription" | "unknown-subscription" => {
                subscription_key(credential_subscription_title(credential).as_deref()) == "unknown"
            }
            _ => true,
        };
        if !matched {
            return false;
        }
    }

    if let Some(subscription) = query.subscription.as_deref() {
        let expected = subscription.trim().to_lowercase();
        if !expected.is_empty() && expected != "all" {
            let title = credential_subscription_title(credential);
            let key = subscription_key(title.as_deref());
            let title_match = title
                .as_deref()
                .map(|value| value.to_lowercase().contains(&expected))
                .unwrap_or(false);
            if key != expected && !title_match {
                return false;
            }
        }
    }

    if let Some(q) = query.q.as_deref() {
        let q = q.trim().to_lowercase();
        if !q.is_empty() && !credential_search_text(credential).contains(&q) {
            return false;
        }
    }

    true
}

fn credential_subscription_title(credential: &CredentialStatusItem) -> Option<String> {
    credential
        .account_info
        .as_ref()
        .and_then(|info| info.subscription_title.clone())
        .or_else(|| credential.subscription_title.clone())
}

fn credential_search_text(credential: &CredentialStatusItem) -> String {
    [
        Some(credential.id.to_string()),
        credential.email.clone(),
        credential.masked_api_key.clone(),
        credential.refresh_token_hash.clone(),
        credential.api_key_hash.clone(),
        credential_subscription_title(credential),
        credential.proxy_resource_name.clone(),
        credential.proxy_url.clone(),
        credential.effective_proxy_url.clone(),
        Some(credential.effective_proxy_source.clone()),
        credential.disabled_reason.clone(),
        credential.cooldown_reason.clone(),
        credential.last_error_kind.clone(),
        credential.last_error_reason.clone(),
        Some(credential.endpoint.clone()),
        credential.auth_method.clone(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ")
    .to_lowercase()
}

fn validation_info_from_row(row: &CredentialAccountInfoRow) -> CredentialValidationInfo {
    CredentialValidationInfo {
        subscription_title: row.subscription_title.clone(),
        current_usage: row.current_usage,
        usage_limit: row.usage_limit,
        usage_percentage: row.usage_percentage,
        checked_at: row.checked_at.clone(),
    }
}

fn validation_info_from_balance(balance: &BalanceResponse) -> CredentialValidationInfo {
    CredentialValidationInfo {
        subscription_title: balance.subscription_title.clone(),
        current_usage: balance.current_usage,
        usage_limit: balance.usage_limit,
        usage_percentage: balance.usage_percentage,
        checked_at: balance.checked_at.clone(),
    }
}

fn validation_info_from_usage(usage: &UsageLimitsResponse) -> CredentialValidationInfo {
    let current_usage = usage.current_usage();
    let usage_limit = usage.usage_limit();
    let usage_percentage = if usage_limit > 0.0 {
        (current_usage / usage_limit * 100.0).min(100.0)
    } else {
        0.0
    };
    CredentialValidationInfo {
        subscription_title: usage.subscription_title().map(|value| value.to_string()),
        current_usage,
        usage_limit,
        usage_percentage,
        checked_at: Utc::now().to_rfc3339(),
    }
}

fn compare_subscription_change(
    previous: Option<&CredentialValidationInfo>,
    current: Option<&CredentialValidationInfo>,
) -> String {
    let Some(previous) = previous else {
        return "unknown".to_string();
    };
    let Some(current) = current else {
        return "unknown".to_string();
    };
    let previous_rank = subscription_rank(previous.subscription_title.as_deref());
    let current_rank = subscription_rank(current.subscription_title.as_deref());
    if previous_rank == 0 || current_rank == 0 {
        return "unknown".to_string();
    }
    if current_rank < previous_rank {
        "downgraded".to_string()
    } else if current_rank > previous_rank {
        "upgraded".to_string()
    } else {
        "unchanged".to_string()
    }
}

fn subscription_key(title: Option<&str>) -> String {
    let Some(title) = title else {
        return "unknown".to_string();
    };
    let lower = title.to_lowercase();
    if lower.contains("pro+")
        || lower.contains("pro plus")
        || lower.contains("pro_plus")
        || lower.contains("pro-plus")
    {
        "pro_plus".to_string()
    } else if lower.contains("trial") || lower.contains("试用") {
        "trial".to_string()
    } else if lower.contains("free") || lower.contains("免费") {
        "free".to_string()
    } else if lower.contains("pro") {
        "pro".to_string()
    } else {
        "unknown".to_string()
    }
}

fn subscription_rank(title: Option<&str>) -> u8 {
    match subscription_key(title).as_str() {
        "free" => 1,
        "trial" => 2,
        "pro" => 3,
        "pro_plus" => 4,
        _ => 0,
    }
}

fn validation_group_key(item: &CredentialValidationItem) -> String {
    match item.change_kind.as_str() {
        "failed" | "downgraded" | "upgraded" => item.change_kind.clone(),
        _ => item.subscription_key.clone(),
    }
}

fn validation_group_title(key: &str) -> String {
    match key {
        "failed" => "查询失败".to_string(),
        "downgraded" => "疑似订阅掉级".to_string(),
        "upgraded" => "订阅升级".to_string(),
        "pro_plus" => "Pro+".to_string(),
        "pro" => "Pro".to_string(),
        "trial" => "试用".to_string(),
        "free" => "Free".to_string(),
        "unknown" => "未知订阅".to_string(),
        other => other.to_string(),
    }
}

fn build_validation_response(items: Vec<CredentialValidationItem>) -> CredentialValidationResponse {
    let total = items.len();
    let success = items.iter().filter(|item| item.ok).count();
    let failed = total.saturating_sub(success);
    let downgraded = items
        .iter()
        .filter(|item| item.change_kind == "downgraded")
        .count();
    let upgraded = items
        .iter()
        .filter(|item| item.change_kind == "upgraded")
        .count();
    let unchanged = items
        .iter()
        .filter(|item| item.change_kind == "unchanged")
        .count();
    let mut grouped: BTreeMap<String, Vec<CredentialValidationItem>> = BTreeMap::new();
    for item in items {
        grouped
            .entry(validation_group_key(&item))
            .or_default()
            .push(item);
    }
    let preferred = [
        "downgraded",
        "failed",
        "upgraded",
        "pro_plus",
        "pro",
        "trial",
        "free",
        "unknown",
    ];
    let mut groups = Vec::new();
    for key in preferred {
        if let Some(items) = grouped.remove(key) {
            groups.push(CredentialValidationGroup {
                key: key.to_string(),
                title: validation_group_title(key),
                count: items.len(),
                items,
            });
        }
    }
    for (key, items) in grouped {
        groups.push(CredentialValidationGroup {
            title: validation_group_title(&key),
            count: items.len(),
            key,
            items,
        });
    }

    CredentialValidationResponse {
        total,
        success,
        failed,
        downgraded,
        upgraded,
        unchanged,
        groups,
    }
}

fn email_key(email: &str) -> String {
    email.trim().to_lowercase()
}

fn normalize_usage_cleanup_request(
    request: UsageCleanupRequest,
) -> Result<UsageCleanupPlan, AdminServiceError> {
    let cutoff = if let Some(value) = request
        .cutoff_before
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        DateTime::parse_from_rfc3339(value)
            .map_err(|err| {
                AdminServiceError::InvalidCredential(format!(
                    "cutoffBefore 不是有效 RFC3339 时间: {}",
                    err
                ))
            })?
            .with_timezone(&Utc)
    } else {
        let days = request.older_than_days.unwrap_or(7);
        if days == 0 {
            return Err(AdminServiceError::InvalidCredential(
                "olderThanDays 必须大于 0".to_string(),
            ));
        }
        if days > 3650 {
            return Err(AdminServiceError::InvalidCredential(
                "olderThanDays 不能超过 3650".to_string(),
            ));
        }
        Utc::now() - ChronoDuration::days(days as i64)
    };

    if cutoff >= Utc::now() {
        return Err(AdminServiceError::InvalidCredential(
            "cutoffBefore 必须早于当前时间".to_string(),
        ));
    }

    let batch_size = request.batch_size.unwrap_or(1000);
    if batch_size == 0 || batch_size > 5000 {
        return Err(AdminServiceError::InvalidCredential(
            "batchSize 必须在 1..=5000 之间".to_string(),
        ));
    }

    let max_batches = request
        .max_batches
        .filter(|value| *value > 0)
        .unwrap_or(USAGE_CLEANUP_DEFAULT_MAX_BATCHES);
    if max_batches > USAGE_CLEANUP_MAX_BATCHES {
        return Err(AdminServiceError::InvalidCredential(
            "maxBatches 必须在 1..=10000 之间".to_string(),
        ));
    }

    let pause_ms_between_batches = request.pause_ms_between_batches.unwrap_or(100);
    if pause_ms_between_batches > 10_000 {
        return Err(AdminServiceError::InvalidCredential(
            "pauseMsBetweenBatches 不能超过 10000".to_string(),
        ));
    }

    Ok(UsageCleanupPlan {
        mode: request.mode,
        cutoff,
        batch_size,
        max_batches,
        pause_ms_between_batches,
    })
}

async fn run_usage_cleanup_job(
    job_id: String,
    store: PostgresUsageStore,
    cleanup_state: Arc<Mutex<UsageCleanupRuntime>>,
    cancel: Arc<AtomicBool>,
    plan: UsageCleanupPlan,
) {
    let mut processed_rows = 0u64;
    let mut batches = 0usize;

    let (final_status, stop_reason, last_error) = loop {
        if cancel.load(Ordering::Acquire) {
            break (
                UsageCleanupJobStatus::Cancelled,
                Some("cancel_requested".to_string()),
                None,
            );
        }
        if batches >= plan.max_batches {
            break (
                UsageCleanupJobStatus::Completed,
                Some("max_batches_reached".to_string()),
                None,
            );
        }

        let batch_result = match plan.mode {
            UsageCleanupMode::SoftDelete => {
                store
                    .soft_delete_cleanup_batch(plan.cutoff, plan.batch_size)
                    .await
            }
            UsageCleanupMode::HardDelete => {
                store
                    .hard_delete_cleanup_batch(plan.cutoff, plan.batch_size)
                    .await
            }
        };

        let batch_rows = match batch_result {
            Ok(rows) => rows,
            Err(err) => {
                break (
                    UsageCleanupJobStatus::Failed,
                    Some("batch_failed".to_string()),
                    Some(err.to_string()),
                );
            }
        };

        if batch_rows == 0 {
            break (
                UsageCleanupJobStatus::Completed,
                Some("no_more_rows".to_string()),
                None,
            );
        }

        batches += 1;
        processed_rows = processed_rows.saturating_add(batch_rows);
        update_usage_cleanup_progress(
            &cleanup_state,
            &job_id,
            UsageCleanupJobStatus::Running,
            processed_rows,
            batch_rows,
            batches,
            None,
            None,
            None,
        );

        if batch_rows < plan.batch_size as u64 {
            break (
                UsageCleanupJobStatus::Completed,
                Some("no_more_rows".to_string()),
                None,
            );
        }

        if plan.pause_ms_between_batches > 0 {
            tokio::time::sleep(StdDuration::from_millis(plan.pause_ms_between_batches)).await;
        }
    };

    let remaining_rows = match plan.mode {
        UsageCleanupMode::SoftDelete => store
            .preview_soft_delete_cleanup(plan.cutoff)
            .await
            .map(|preview| preview.matched_rows)
            .ok(),
        UsageCleanupMode::HardDelete => store
            .preview_hard_delete_cleanup(plan.cutoff)
            .await
            .map(|preview| preview.matched_rows)
            .ok(),
    };

    update_usage_cleanup_progress(
        &cleanup_state,
        &job_id,
        final_status,
        processed_rows,
        0,
        batches,
        remaining_rows,
        stop_reason,
        last_error,
    );
}

#[allow(clippy::too_many_arguments)]
fn update_usage_cleanup_progress(
    cleanup_state: &Arc<Mutex<UsageCleanupRuntime>>,
    job_id: &str,
    status: UsageCleanupJobStatus,
    processed_rows: u64,
    last_batch_rows: u64,
    batches: usize,
    remaining_rows: Option<u64>,
    stop_reason: Option<String>,
    last_error: Option<String>,
) {
    let now = Utc::now().to_rfc3339();
    let mut runtime = cleanup_state.lock();
    if runtime.status.job_id.as_deref() != Some(job_id) {
        return;
    }
    runtime.status.status = status;
    runtime.status.processed_rows = processed_rows;
    runtime.status.last_batch_rows = last_batch_rows;
    runtime.status.batches = batches;
    runtime.status.updated_at = Some(now.clone());
    if let Some(remaining_rows) = remaining_rows {
        runtime.status.remaining_rows = Some(remaining_rows);
    } else if let Some(matched_rows) = runtime.status.matched_rows {
        runtime.status.remaining_rows = Some(matched_rows.saturating_sub(processed_rows));
    }
    if stop_reason.is_some() {
        runtime.status.stop_reason = stop_reason;
    }
    if last_error.is_some() {
        runtime.status.last_error = last_error;
    }
    if status != UsageCleanupJobStatus::Running {
        runtime.status.finished_at = Some(now);
        runtime.cancel = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cleanup_request() -> UsageCleanupRequest {
        UsageCleanupRequest {
            mode: UsageCleanupMode::SoftDelete,
            older_than_days: None,
            cutoff_before: None,
            batch_size: None,
            max_batches: None,
            pause_ms_between_batches: None,
        }
    }

    #[test]
    fn usage_cleanup_request_uses_safe_manual_defaults() {
        let before = Utc::now();
        let plan = normalize_usage_cleanup_request(cleanup_request()).expect("valid request");
        let after = Utc::now();

        assert_eq!(plan.mode, UsageCleanupMode::SoftDelete);
        assert_eq!(plan.batch_size, 1000);
        assert_eq!(plan.max_batches, USAGE_CLEANUP_DEFAULT_MAX_BATCHES);
        assert_eq!(plan.pause_ms_between_batches, 100);
        assert!(plan.cutoff >= before - ChronoDuration::days(7) - ChronoDuration::seconds(1));
        assert!(plan.cutoff <= after - ChronoDuration::days(7) + ChronoDuration::seconds(1));
    }

    #[test]
    fn usage_cleanup_request_cutoff_before_overrides_days() {
        let cutoff = Utc::now() - ChronoDuration::days(30);
        let mut request = cleanup_request();
        request.mode = UsageCleanupMode::HardDelete;
        request.older_than_days = Some(1);
        request.cutoff_before = Some(cutoff.to_rfc3339());
        request.batch_size = Some(5000);
        request.max_batches = Some(10_000);
        request.pause_ms_between_batches = Some(0);

        let plan = normalize_usage_cleanup_request(request).expect("valid request");

        assert_eq!(plan.mode, UsageCleanupMode::HardDelete);
        assert_eq!(plan.cutoff, cutoff);
        assert_eq!(plan.batch_size, 5000);
        assert_eq!(plan.max_batches, 10_000);
        assert_eq!(plan.pause_ms_between_batches, 0);
    }

    #[test]
    fn usage_cleanup_request_rejects_unsafe_bounds() {
        let mut zero_days = cleanup_request();
        zero_days.older_than_days = Some(0);
        assert!(matches!(
            normalize_usage_cleanup_request(zero_days),
            Err(AdminServiceError::InvalidCredential(_))
        ));

        let mut large_batch = cleanup_request();
        large_batch.batch_size = Some(5001);
        assert!(matches!(
            normalize_usage_cleanup_request(large_batch),
            Err(AdminServiceError::InvalidCredential(_))
        ));

        let mut too_many_batches = cleanup_request();
        too_many_batches.max_batches = Some(USAGE_CLEANUP_MAX_BATCHES + 1);
        assert!(matches!(
            normalize_usage_cleanup_request(too_many_batches),
            Err(AdminServiceError::InvalidCredential(_))
        ));

        let mut future_cutoff = cleanup_request();
        future_cutoff.cutoff_before = Some((Utc::now() + ChronoDuration::minutes(1)).to_rfc3339());
        assert!(matches!(
            normalize_usage_cleanup_request(future_cutoff),
            Err(AdminServiceError::InvalidCredential(_))
        ));
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
