//! Admin API 业务逻辑服务

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration as StdDuration, Instant};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::error::AdminServiceError;
use super::types::{
    AccessKeysResponse, AddCredentialRequest, AddCredentialResponse, BalanceResponse,
    BatchCredentialImportDefaults, BatchCredentialImportDuplicateMode, BatchCredentialImportItem,
    BatchCredentialImportRequest, BatchCredentialImportResponse, BatchUpdateCredentialItem,
    BatchUpdateCredentialsRequest, BatchUpdateCredentialsResponse, BulkCredentialActionError,
    BulkCredentialActionResponse, ClearInFlightRequest, CreateProxyResourceRequest,
    CreateRequestApiKeyRequest, CredentialAccountInfo, CredentialAccountInfoItem,
    CredentialAccountInfoListResponse, CredentialCooldown, CredentialCreditSummaryResponse,
    CredentialInfoRefreshItem, CredentialInfoRefreshResponse, CredentialListItem,
    CredentialListResponse, CredentialRuntimeItem, CredentialRuntimeResponse, CredentialStatusItem,
    CredentialSummaryResponse, CredentialUsageSummaryItem, CredentialUsageSummaryResponse,
    CredentialValidationGroup, CredentialValidationInfo, CredentialValidationItem,
    CredentialValidationResponse, CredentialsPageResponse, CredentialsStatusResponse,
    ExternalPoolTestRequest, LoadBalancingModeResponse, ManualModelResponse, ProxyResourceResponse,
    ProxyResourcesResponse, RefreshCredentialInfoRequest, RequestApiKeyItem, RuntimeConfigResponse,
    SetCredentialConcurrencyRequest, SetCredentialProxyRequest, SetCredentialRegionsRequest,
    SetLoadBalancingModeRequest, SetWarmupRequest, TestCredentialRequest, TestCredentialResponse,
    UpdateAdminApiKeyRequest, UpdateCredentialAuthRequest, UpdateProxyResourceRequest,
    UpdateRequestApiKeyRequest, UpdateRuntimeConfigRequest, UpsertManualModelRequest,
    UsageCleanupJobStatus, UsageCleanupMode, UsageCleanupPreviewResponse, UsageCleanupRequest,
    UsageCleanupStatusResponse, ValidateExistingCredentialsRequest,
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
use crate::common::auth::RequestApiKeyStore;
use crate::external_pool::{
    CreateExternalPoolRequest, ExternalPool, ExternalPoolManager, ExternalPoolTestResponse,
    ExternalPoolsStatusResponse, SetExternalPoolEnabledRequest, UpdateExternalPoolRequest,
    external_pool_messages_url, external_pool_models_url,
};
use crate::http_client::response_bytes_with_body_timeout;
use crate::kiro::model::credentials::KiroCredentials;
use crate::kiro::model::events::Event;
use crate::kiro::model::requests::{
    ConversationState, CurrentMessage, KiroRequest, UserInputMessage,
};
use crate::kiro::model::usage_limits::UsageLimitsResponse;
use crate::kiro::parser::decoder::EventStreamDecoder;
use crate::kiro::provider::KiroProvider;
use crate::kiro::token_manager::{
    CredentialAuthUpdate, CredentialBaseSnapshot, CredentialEntrySnapshot, MultiTokenManager,
};
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
const ADMIN_USAGE_CACHE_TTL_SECS: usize = 2;
const ADMIN_EXTERNAL_POOL_STATUS_CACHE_TTL_SECS: usize = 2;
const ADMIN_CACHE_DEFAULT_LOCAL_TTL_SECS: usize = 2;
const ADMIN_CACHE_REDIS_READ_TIMEOUT: StdDuration = StdDuration::from_millis(250);
const ADMIN_CACHE_LOCAL_STALE_TTL_SECS: u64 = 30;

#[derive(Debug, Clone, Default)]
pub struct CredentialListQuery {
    pub q: Option<String>,
    pub status: Option<String>,
    pub auth_method: Option<String>,
    pub subscription: Option<String>,
    pub proxy_resource_id: Option<u64>,
    pub sort_by: Option<String>,
    pub sort_order: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CredentialSortBy {
    Default,
    Id,
    CreatedAt,
    UpdatedAt,
    Priority,
    LastUsedAt,
    SuccessCount,
    FailureCount,
    RefreshFailureCount,
    EstimatedCost,
    UsagePercentage,
    RemainingQuota,
    InFlightRequests,
    SchedulerScore,
}

impl CredentialSortBy {
    fn parse(value: Option<&str>) -> Self {
        match value
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "id" => Self::Id,
            "created_at" | "created-at" | "createdat" => Self::CreatedAt,
            "updated_at" | "updated-at" | "updatedat" => Self::UpdatedAt,
            "priority" => Self::Priority,
            "last_used_at" | "last-used-at" | "lastusedat" => Self::LastUsedAt,
            "success_count" | "success-count" | "successcount" => Self::SuccessCount,
            "failure_count" | "failure-count" | "failurecount" => Self::FailureCount,
            "refresh_failure_count" | "refresh-failure-count" | "refreshfailurecount" => {
                Self::RefreshFailureCount
            }
            "estimated_cost" | "estimated-cost" | "estimatedcost" | "cost" => Self::EstimatedCost,
            "usage_percentage" | "usage-percentage" | "usagepercentage" | "usage" => {
                Self::UsagePercentage
            }
            "remaining_quota" | "remaining-quota" | "remainingquota" | "remaining" => {
                Self::RemainingQuota
            }
            "in_flight_requests" | "in-flight-requests" | "inflightrequests" | "in_flight" => {
                Self::InFlightRequests
            }
            "scheduler_score" | "scheduler-score" | "schedulerscore" => Self::SchedulerScore,
            _ => Self::Default,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CredentialSortOrder {
    Asc,
    Desc,
}

impl CredentialSortOrder {
    fn parse(value: Option<&str>, sort_by: CredentialSortBy) -> Self {
        match value
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "asc" | "ascending" => Self::Asc,
            "desc" | "descending" => Self::Desc,
            _ => default_sort_order(sort_by),
        }
    }

    fn apply(self, ordering: std::cmp::Ordering) -> std::cmp::Ordering {
        match self {
            Self::Asc => ordering,
            Self::Desc => ordering.reverse(),
        }
    }
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

    const WITH_ACCOUNT_INFO_AND_COST_SUMMARY: Self = Self {
        include_account_info: true,
        include_cost_summary: true,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CredentialCreditTier {
    Free,
    Pro,
    ProPlus,
    Power,
    ProMax,
}

#[derive(Debug, Clone, Copy)]
struct CredentialCreditSnapshot {
    limit: f64,
    remaining: f64,
    base: f64,
    bonus: f64,
}

fn credit_snapshot_for_subscription(
    subscription_title: Option<&str>,
    current_usage: f64,
) -> CredentialCreditSnapshot {
    let Some(title) = subscription_title
        .map(str::trim)
        .filter(|title| !title.is_empty())
    else {
        return CredentialCreditSnapshot {
            limit: 0.0,
            remaining: 0.0,
            base: 0.0,
            bonus: 0.0,
        };
    };
    let tier = credential_credit_tier(Some(title));
    let Some(tier) = tier else {
        return CredentialCreditSnapshot {
            limit: 0.0,
            remaining: 0.0,
            base: 0.0,
            bonus: 0.0,
        };
    };
    let base = credential_credit_base(tier);
    let bonus = if matches!(
        tier,
        CredentialCreditTier::Free | CredentialCreditTier::Power
    ) {
        0.0
    } else {
        10_000.0
    };
    let limit = base + bonus;
    CredentialCreditSnapshot {
        limit,
        remaining: (limit - current_usage).max(0.0),
        base,
        bonus,
    }
}

fn credit_snapshot_from_persisted_fields(
    subscription_title: Option<&str>,
    current_usage: f64,
    credit_limit: f64,
    credit_remaining: f64,
    credit_base: f64,
    credit_bonus: f64,
) -> CredentialCreditSnapshot {
    if credit_limit > 0.0 || credit_remaining > 0.0 || credit_base > 0.0 || credit_bonus > 0.0 {
        return CredentialCreditSnapshot {
            limit: credit_limit,
            remaining: credit_remaining,
            base: credit_base,
            bonus: credit_bonus,
        };
    }
    credit_snapshot_for_subscription(subscription_title, current_usage)
}

fn credential_credit_base(tier: CredentialCreditTier) -> f64 {
    match tier {
        CredentialCreditTier::Free => 0.0,
        CredentialCreditTier::Pro => 1_000.0,
        CredentialCreditTier::ProPlus => 2_000.0,
        CredentialCreditTier::Power => 10_000.0,
        CredentialCreditTier::ProMax => 5_000.0,
    }
}

fn credential_credit_tier(subscription_title: Option<&str>) -> Option<CredentialCreditTier> {
    let title = subscription_title?.trim().to_ascii_lowercase();
    if title.is_empty() {
        return None;
    }
    let compact: String = title
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect();
    if title.contains("free") {
        return Some(CredentialCreditTier::Free);
    }
    if title.contains("power") {
        return Some(CredentialCreditTier::Power);
    }
    if compact.contains("promax") {
        return Some(CredentialCreditTier::ProMax);
    }
    if title.contains("pro+") || compact.contains("proplus") {
        return Some(CredentialCreditTier::ProPlus);
    }
    if compact.contains("pro") {
        return Some(CredentialCreditTier::Pro);
    }
    None
}

fn balance_cache_key(id: u64) -> String {
    format!("balance:{}", id)
}

fn admin_usage_summary_cache_key(high_cache_threshold: i32) -> String {
    format!("admin_cache:usage:summary:{}", high_cache_threshold)
}

fn admin_usage_dashboard_cache_key(timezone: Option<&str>, high_cache_threshold: i32) -> String {
    let timezone = timezone
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("default")
        .replace(':', "_");
    format!(
        "admin_cache:usage:dashboard:{}:{}",
        timezone, high_cache_threshold
    )
}

fn admin_external_pool_status_cache_key() -> &'static str {
    "admin_cache:external_pools:status"
}

fn admin_external_pool_list_cache_key() -> &'static str {
    "admin_cache:external_pools:list"
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

fn request_api_key_id(key: &str) -> String {
    hex::encode(Sha256::digest(key.as_bytes()))
}

fn request_api_key_items(keys: &[String]) -> Vec<RequestApiKeyItem> {
    keys.iter()
        .enumerate()
        .map(|(index, key)| RequestApiKeyItem {
            id: request_api_key_id(key),
            api_key: key.clone(),
            masked_api_key: mask_secret(key),
            primary: index == 0,
        })
        .collect()
}

fn access_keys_response(request_api_keys: &[String], admin_api_key: &str) -> AccessKeysResponse {
    let primary = request_api_keys.first().cloned().unwrap_or_default();
    AccessKeysResponse {
        request_api_key: primary.clone(),
        masked_request_api_key: mask_secret(&primary),
        request_api_keys: request_api_key_items(request_api_keys),
        admin_api_key: admin_api_key.to_string(),
        masked_admin_api_key: mask_secret(admin_api_key),
    }
}

fn deserialize_admin_cache_value<T: DeserializeOwned>(value: &Value) -> Option<T> {
    match serde_json::from_value(value.clone()) {
        Ok(value) => Some(value),
        Err(err) => {
            tracing::warn!("反序列化 Admin 本地缓存失败: {}", err);
            None
        }
    }
}

fn generate_request_api_key() -> String {
    let mut bytes = [0u8; 32];
    bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    format!("sk-kiro-rs-{}", URL_SAFE_NO_PAD.encode(bytes))
}

fn remove_request_api_key_by_id(
    keys: &mut Vec<String>,
    key_id: &str,
) -> Result<String, AdminServiceError> {
    let index = keys
        .iter()
        .position(|key| request_api_key_id(key) == key_id)
        .ok_or_else(|| AdminServiceError::InvalidCredential("调用 API Key 不存在".to_string()))?;
    if keys.len() <= 1 {
        return Err(AdminServiceError::Conflict(
            "至少需要保留一个调用 API Key".to_string(),
        ));
    }
    Ok(keys.remove(index))
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
    request_api_key_store: Arc<RequestApiKeyStore>,
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
    admin_cache_shadow: Arc<Mutex<HashMap<String, AdminCacheEntry>>>,
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

#[derive(Debug, Clone)]
struct AdminCacheEntry {
    value: Value,
    fresh_until: Instant,
    stale_until: Instant,
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
        request_api_key_store: Arc<RequestApiKeyStore>,
        external_pool_manager: Arc<ExternalPoolManager>,
    ) -> Self {
        Self {
            token_manager,
            postgres_store,
            redis_store,
            request_api_key_store,
            known_endpoints: known_endpoints.into_iter().collect(),
            usage_recorder,
            prompt_cache,
            prompt_cache_creation_controller,
            pricing_catalog,
            model_capabilities,
            kiro_provider,
            external_pool_manager,
            usage_cleanup: Arc::new(Mutex::new(UsageCleanupRuntime::default())),
            admin_cache_shadow: Arc::new(Mutex::new(HashMap::new())),
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

    fn read_admin_cache<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        let now = Instant::now();
        let stale_value = {
            let mut shadow = self.admin_cache_shadow.lock();
            match shadow.get(key) {
                Some(entry) if entry.fresh_until > now => {
                    return deserialize_admin_cache_value(&entry.value);
                }
                Some(entry) if entry.stale_until > now => Some(entry.value.clone()),
                Some(_) => {
                    shadow.remove(key);
                    None
                }
                None => None,
            }
        };

        let redis = self.redis_store.clone();
        let key = key.to_string();
        let redis_key = key.clone();
        match block_on_admin_store(async move {
            tokio::time::timeout(
                ADMIN_CACHE_REDIS_READ_TIMEOUT,
                redis.get_json::<Value>(redis_key),
            )
            .await
            .map_err(|_| anyhow::anyhow!("Redis Admin 缓存读取超时"))?
        }) {
            Ok(Some(value)) => {
                self.write_admin_cache_shadow(
                    key.clone(),
                    value.clone(),
                    ADMIN_CACHE_DEFAULT_LOCAL_TTL_SECS,
                );
                deserialize_admin_cache_value(&value)
            }
            Ok(None) => stale_value.and_then(|value| deserialize_admin_cache_value(&value)),
            Err(err) => {
                tracing::warn!("读取 Redis Admin 缓存失败: {}", err);
                stale_value.and_then(|value| deserialize_admin_cache_value(&value))
            }
        }
    }

    fn write_admin_cache<T>(&self, key: String, value: T, ttl_secs: usize)
    where
        T: Serialize,
    {
        let value = match serde_json::to_value(&value) {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!("序列化 Admin 缓存失败: {}", err);
                return;
            }
        };
        self.write_admin_cache_shadow(key.clone(), value.clone(), ttl_secs);
        let redis = self.redis_store.clone();
        tokio::spawn(async move {
            if let Err(err) = redis.set_json(key, &value, ttl_secs).await {
                tracing::warn!("写入 Redis Admin 缓存失败: {}", err);
            }
        });
    }

    fn write_admin_cache_shadow(&self, key: String, value: Value, ttl_secs: usize) {
        let now = Instant::now();
        let fresh_ttl = StdDuration::from_secs(ttl_secs.max(1) as u64);
        let stale_ttl = StdDuration::from_secs(ADMIN_CACHE_LOCAL_STALE_TTL_SECS);
        self.admin_cache_shadow.lock().insert(
            key,
            AdminCacheEntry {
                value,
                fresh_until: now + fresh_ttl,
                stale_until: now + fresh_ttl + stale_ttl,
            },
        );
    }

    fn invalidate_admin_cache_pattern(&self, pattern: &'static str) {
        {
            let mut shadow = self.admin_cache_shadow.lock();
            if let Some(prefix) = pattern.strip_suffix('*') {
                shadow.retain(|key, _| !key.starts_with(prefix));
            } else {
                shadow.remove(pattern);
            }
        }
        let redis = self.redis_store.clone();
        tokio::spawn(async move {
            if let Err(err) = redis.del_pattern(pattern).await {
                tracing::warn!("清理 Redis Admin 缓存失败: {}", err);
            }
        });
    }

    fn invalidate_usage_admin_cache(&self) {
        self.invalidate_admin_cache_pattern("admin_cache:usage:*");
    }

    fn invalidate_external_pool_admin_cache(&self) {
        self.invalidate_admin_cache_pattern("admin_cache:external_pools:*");
    }

    pub fn get_access_keys(&self, admin_api_key: &str) -> AccessKeysResponse {
        access_keys_response(
            &self.token_manager.runtime_config().request_api_keys(),
            admin_api_key,
        )
    }

    fn persist_request_api_keys(
        &self,
        keys: Vec<String>,
        action: &'static str,
        detail: serde_json::Value,
    ) -> Result<(), AdminServiceError> {
        if keys.is_empty() {
            return Err(AdminServiceError::InvalidCredential(
                "至少需要保留一个调用 API Key".to_string(),
            ));
        }

        self.token_manager
            .update_runtime_config(|config| {
                config.set_request_api_keys(keys.clone());
            })
            .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;
        self.request_api_key_store.replace_keys(keys);
        self.audit(
            action,
            "security_keys",
            Some("apiKeys".to_string()),
            true,
            None,
            detail,
        );
        Ok(())
    }

    pub fn create_request_api_key(
        &self,
        req: CreateRequestApiKeyRequest,
        admin_api_key: &str,
    ) -> Result<AccessKeysResponse, AdminServiceError> {
        let next_key = req
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(str::to_string)
            .unwrap_or_else(generate_request_api_key);
        if next_key.len() < 8 {
            return Err(AdminServiceError::InvalidCredential(
                "调用 API Key 至少需要 8 个字符".to_string(),
            ));
        }

        let mut keys = self.token_manager.runtime_config().request_api_keys();
        if keys.iter().any(|key| key == &next_key) {
            return Err(AdminServiceError::Conflict(
                "调用 API Key 已存在".to_string(),
            ));
        }
        keys.push(next_key.clone());
        self.persist_request_api_keys(
            keys,
            "create_request_api_key",
            json!({
                "keyId": request_api_key_id(&next_key),
                "maskedApiKey": mask_secret(&next_key),
            }),
        )?;
        Ok(self.get_access_keys(admin_api_key))
    }

    pub fn update_request_api_key(
        &self,
        key_id: &str,
        req: UpdateRequestApiKeyRequest,
        admin_api_key: &str,
    ) -> Result<AccessKeysResponse, AdminServiceError> {
        let next_key = req
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .ok_or_else(|| {
                AdminServiceError::InvalidCredential("调用 API Key 不能为空".to_string())
            })?
            .to_string();
        if next_key.len() < 8 {
            return Err(AdminServiceError::InvalidCredential(
                "调用 API Key 至少需要 8 个字符".to_string(),
            ));
        }

        let mut keys = self.token_manager.runtime_config().request_api_keys();
        let index = keys
            .iter()
            .position(|key| request_api_key_id(key) == key_id)
            .ok_or_else(|| {
                AdminServiceError::InvalidCredential("调用 API Key 不存在".to_string())
            })?;
        if keys
            .iter()
            .enumerate()
            .any(|(existing_index, key)| existing_index != index && key == &next_key)
        {
            return Err(AdminServiceError::Conflict(
                "调用 API Key 已存在".to_string(),
            ));
        }
        keys[index] = next_key.clone();
        self.persist_request_api_keys(
            keys,
            "update_request_api_key",
            json!({
                "keyId": key_id,
                "nextKeyId": request_api_key_id(&next_key),
                "maskedApiKey": mask_secret(&next_key),
            }),
        )?;
        Ok(self.get_access_keys(admin_api_key))
    }

    pub fn delete_request_api_key(
        &self,
        key_id: &str,
        admin_api_key: &str,
    ) -> Result<AccessKeysResponse, AdminServiceError> {
        let mut keys = self.token_manager.runtime_config().request_api_keys();
        remove_request_api_key_by_id(&mut keys, key_id)?;
        self.persist_request_api_keys(keys, "delete_request_api_key", json!({ "keyId": key_id }))?;
        Ok(self.get_access_keys(admin_api_key))
    }

    pub fn list_external_pools(&self) -> Result<Vec<ExternalPool>, AdminServiceError> {
        let cache_key = admin_external_pool_list_cache_key();
        if let Some(cached) = self.read_admin_cache::<Vec<ExternalPool>>(cache_key) {
            return Ok(cached);
        }

        let store = self.postgres_store.clone();
        let pools = block_on_admin_store(async move { store.list_external_pools(true).await })
            .map_err(|err| AdminServiceError::InternalError(err.to_string()))?;
        self.write_admin_cache(
            cache_key.to_string(),
            pools.clone(),
            ADMIN_EXTERNAL_POOL_STATUS_CACHE_TTL_SECS,
        );
        Ok(pools)
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
        self.invalidate_external_pool_admin_cache();
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
        self.invalidate_external_pool_admin_cache();
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
        self.invalidate_external_pool_admin_cache();
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
        self.invalidate_external_pool_admin_cache();
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
        self.invalidate_external_pool_admin_cache();
        Ok(pool)
    }

    pub fn get_external_pool_status(
        &self,
    ) -> Result<ExternalPoolsStatusResponse, AdminServiceError> {
        let cache_key = admin_external_pool_status_cache_key();
        if let Some(cached) = self.read_admin_cache::<ExternalPoolsStatusResponse>(cache_key) {
            return Ok(cached);
        }

        let manager = self.external_pool_manager.clone();
        let config = self.token_manager.runtime_config().external_pools;
        let pools = block_on_admin_store(async move { manager.status(&config).await })
            .map_err(|err| AdminServiceError::InternalError(err.to_string()))?;
        let response = ExternalPoolsStatusResponse { pools };
        self.write_admin_cache(
            cache_key.to_string(),
            response.clone(),
            ADMIN_EXTERNAL_POOL_STATUS_CACHE_TTL_SECS,
        );
        Ok(response)
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
            profile_arn: req.profile_arn,
            expires_at: None,
            auth_method: Some(req.auth_method),
            provider: req.provider,
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
        ) = self.credential_status_items(
            CredentialStatusBuildOptions::WITH_ACCOUNT_INFO_AND_COST_SUMMARY,
        );

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
        ) = self.credential_status_items(
            CredentialStatusBuildOptions::WITH_ACCOUNT_INFO_AND_COST_SUMMARY,
        );
        let mut filtered: Vec<_> = credentials
            .into_iter()
            .filter(|credential| credential_matches_query(credential, &query))
            .collect();
        sort_credentials_for_admin_display_with_query(&mut filtered, &query);
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

    /// 轻量分页获取凭据基础字段。
    pub fn get_credentials_list(
        &self,
        page: usize,
        limit: usize,
        query: CredentialListQuery,
    ) -> CredentialListResponse {
        let page = normalize_page(page);
        let limit = normalize_credentials_limit(limit);
        let snapshot = self.token_manager.base_snapshot();
        let default_endpoint = self.token_manager.runtime_config().default_endpoint;
        let mut filtered: Vec<_> = snapshot
            .entries
            .into_iter()
            .map(|entry| credential_list_item_from_base(entry, &default_endpoint))
            .filter(|credential| credential_base_matches_query(credential, &query))
            .collect();
        sort_credential_list_items_for_admin_display_with_query(&mut filtered, &query);
        let filtered_total = filtered.len();
        let filtered_available = filtered
            .iter()
            .filter(|credential| !credential.disabled)
            .count();
        let total_pages = total_pages(filtered_total, limit);
        let start = page.saturating_sub(1).saturating_mul(limit);
        let items = filtered.into_iter().skip(start).take(limit).collect();

        CredentialListResponse {
            page,
            limit,
            total: snapshot.total,
            available: snapshot.available,
            filtered_total,
            filtered_available,
            total_pages,
            items,
        }
    }

    /// 获取凭据数量和全局调度容量概览。
    pub fn get_credentials_summary(&self) -> CredentialSummaryResponse {
        let snapshot = self.token_manager.base_snapshot();
        CredentialSummaryResponse {
            total: snapshot.total,
            available: snapshot.available,
            disabled: snapshot.total.saturating_sub(snapshot.available),
            current_id: (snapshot.current_id > 0).then_some(snapshot.current_id),
            global_in_flight_requests: snapshot.global_in_flight_requests,
            queued_requests: snapshot.queued_requests,
            global_max_concurrent_requests: snapshot.global_max_concurrent_requests,
            max_queued_requests: snapshot.max_queued_requests,
            updated_at: Utc::now().to_rfc3339(),
            runtime_fresh: snapshot.runtime_fresh,
        }
    }

    pub fn get_credentials_runtime(&self, ids: &[u64]) -> CredentialRuntimeResponse {
        if ids.is_empty() {
            return CredentialRuntimeResponse {
                items: Vec::new(),
                updated_at: Utc::now().to_rfc3339(),
                fresh: true,
            };
        }

        let snapshot = self.token_manager.runtime_snapshot_for_ids(ids);
        CredentialRuntimeResponse {
            items: snapshot
                .entries
                .into_iter()
                .map(|entry| credential_runtime_item_from_snapshot(entry, snapshot.current_id))
                .collect(),
            updated_at: Utc::now().to_rfc3339(),
            fresh: snapshot.runtime_fresh,
        }
    }

    pub async fn get_credentials_account_info(
        &self,
        ids: &[u64],
    ) -> CredentialAccountInfoListResponse {
        let info = if ids.is_empty() {
            HashMap::new()
        } else {
            match block_on_admin_store({
                let store = self.postgres_store.clone();
                let ids = ids.to_vec();
                async move { store.load_credential_account_info_for_ids(&ids).await }
            }) {
                Ok(info) => info,
                Err(err) => {
                    tracing::warn!("按 ID 加载凭据账号信息失败: {}", err);
                    HashMap::new()
                }
            }
        };
        CredentialAccountInfoListResponse {
            items: ids
                .iter()
                .filter_map(|id| {
                    info.get(id)
                        .map(|row| credential_account_info_item_from_row(*id, row))
                })
                .collect(),
            updated_at: Utc::now().to_rfc3339(),
            fresh: true,
        }
    }

    pub fn get_credentials_usage_summary(&self, ids: &[u64]) -> CredentialUsageSummaryResponse {
        let summaries = self.usage_recorder.credential_cost_summary_for_ids(ids);
        CredentialUsageSummaryResponse {
            items: ids
                .iter()
                .filter_map(|id| {
                    summaries.get(id).map(|summary| CredentialUsageSummaryItem {
                        id: *id,
                        estimated_cost_usd: summary.estimated_cost_usd,
                        priced_requests: summary.priced_requests,
                        unpriced_requests: summary.unpriced_requests,
                    })
                })
                .collect(),
            updated_at: Utc::now().to_rfc3339(),
            fresh: true,
        }
    }

    pub async fn get_credential_credit_summary(
        &self,
    ) -> Result<CredentialCreditSummaryResponse, AdminServiceError> {
        let account_info = self
            .postgres_store
            .load_credential_account_info()
            .await
            .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;
        let (_, _, _, _, _, _, _, credentials) =
            self.credential_status_items(CredentialStatusBuildOptions::LIGHT);

        let total_credentials = credentials.len();
        let enabled_credentials = credentials
            .iter()
            .filter(|credential| !credential.disabled)
            .count();
        let disabled_credentials = total_credentials.saturating_sub(enabled_credentials);
        let mut known_credentials = 0;
        let mut total_credit_limit = 0.0;
        let mut total_credit_remaining = 0.0;
        let mut total_current_usage = 0.0;
        let mut enabled_credit_limit = 0.0;
        let mut enabled_credit_remaining = 0.0;
        let mut disabled_credit_limit = 0.0;
        let mut disabled_credit_remaining = 0.0;
        let mut last_checked_at: Option<String> = None;

        for credential in &credentials {
            let Some(info) = account_info.get(&credential.id).map(account_info_from_row) else {
                continue;
            };
            known_credentials += 1;
            total_credit_limit += info.credit_limit;
            total_credit_remaining += info.credit_remaining;
            total_current_usage += info.current_usage;
            if credential.disabled {
                disabled_credit_limit += info.credit_limit;
                disabled_credit_remaining += info.credit_remaining;
            } else {
                enabled_credit_limit += info.credit_limit;
                enabled_credit_remaining += info.credit_remaining;
            }
            if last_checked_at
                .as_ref()
                .map(|current| info.checked_at > *current)
                .unwrap_or(true)
            {
                last_checked_at = Some(info.checked_at.clone());
            }
        }

        Ok(CredentialCreditSummaryResponse {
            total_credentials,
            enabled_credentials,
            disabled_credentials,
            known_credentials,
            unknown_credentials: total_credentials.saturating_sub(known_credentials),
            total_credit_limit,
            total_credit_remaining,
            total_current_usage,
            enabled_credit_limit,
            enabled_credit_remaining,
            disabled_credit_limit,
            disabled_credit_remaining,
            last_checked_at,
        })
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
                let mut item = credential_status_item_from_snapshot(
                    entry,
                    &default_endpoint,
                    snapshot.current_id,
                );
                item.account_info = info;
                item.estimated_cost_usd = cost.estimated_cost_usd;
                item.priced_requests = cost.priced_requests;
                item.unpriced_requests = cost.unpriced_requests;
                item
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

    pub fn set_credential_regions(
        &self,
        id: u64,
        req: SetCredentialRegionsRequest,
    ) -> Result<(), AdminServiceError> {
        let region = normalize_optional_update(req.region);
        let auth_region = normalize_optional_update(req.auth_region);
        let api_region = normalize_optional_update(req.api_region);
        self.token_manager
            .set_credential_regions(id, region.clone(), auth_region.clone(), api_region.clone())
            .map_err(|e| self.classify_error(e, id))?;
        self.invalidate_balance_cache(id);
        self.audit(
            "set_credential_regions",
            "credential",
            Some(id.to_string()),
            true,
            None,
            json!({
                "region": region,
                "authRegion": auth_region,
                "apiRegion": api_region,
            }),
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
                    let cached_data = normalize_balance_credit_snapshot(&cached.data);
                    self.save_account_info_snapshot(&cached_data).await?;
                    return Ok(cached_data);
                }
            }
        }

        // 缓存未命中或已过期，从上游获取
        let balance = normalize_balance_credit_snapshot(&self.fetch_balance(id).await?);
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
        let body_bytes = match response_bytes_with_body_timeout(
            response,
            runtime_config.kiro_upstream_response_timeout_secs,
        )
        .await
        {
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
        let credit = credit_snapshot_for_subscription(usage.subscription_title(), current_usage);

        Ok(BalanceResponse {
            id,
            checked_at: Utc::now().to_rfc3339(),
            subscription_title: usage.subscription_title().map(|s| s.to_string()),
            current_usage,
            usage_limit,
            remaining,
            usage_percentage,
            credit_limit: credit.limit,
            credit_remaining: credit.remaining,
            credit_base: credit.base,
            credit_bonus: credit.bonus,
            next_reset_at: usage.next_date_reset,
        })
    }

    async fn save_account_info_snapshot(
        &self,
        balance: &BalanceResponse,
    ) -> Result<(), AdminServiceError> {
        let balance = normalize_balance_credit_snapshot(balance);
        let info = CredentialAccountInfoRow {
            subscription_title: balance.subscription_title.clone(),
            current_usage: balance.current_usage,
            usage_limit: balance.usage_limit,
            remaining: balance.remaining,
            usage_percentage: balance.usage_percentage,
            credit_limit: balance.credit_limit,
            credit_remaining: balance.credit_remaining,
            credit_base: balance.credit_base,
            credit_bonus: balance.credit_bonus,
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
            provider: req.provider,
            client_id: req.client_id,
            client_secret: req.client_secret,
            kiro_api_key: req.kiro_api_key,
            region: req.region,
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
        let proxy_url = normalize_proxy_url(req.proxy_url)?;
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

    pub fn batch_update_credentials(
        &self,
        req: BatchUpdateCredentialsRequest,
    ) -> Result<BatchUpdateCredentialsResponse, AdminServiceError> {
        if req.ids.is_empty() {
            return Err(AdminServiceError::InvalidCredential(
                "没有选择要修改的凭据".to_string(),
            ));
        }
        if req.ids.len() > MAX_CREDENTIALS_PAGE_LIMIT {
            return Err(AdminServiceError::InvalidCredential(format!(
                "单次最多修改 {} 个凭据",
                MAX_CREDENTIALS_PAGE_LIMIT
            )));
        }
        if req.regions.is_none() && req.concurrency.is_none() && req.proxy.is_none() {
            return Err(AdminServiceError::InvalidCredential(
                "没有选择任何要修改的字段".to_string(),
            ));
        }

        let mut seen = HashSet::new();
        let ids: Vec<u64> = req.ids.into_iter().filter(|id| seen.insert(*id)).collect();
        let regions = req.regions.map(|regions| {
            (
                normalize_optional_update(regions.region),
                normalize_optional_update(regions.auth_region),
                normalize_optional_update(regions.api_region),
            )
        });
        let concurrency = req.concurrency.map(|value| value.max_concurrent_requests);
        let proxy = match req.proxy {
            Some(proxy) => Some((
                proxy.proxy_resource_id,
                normalize_proxy_url(proxy.proxy_url)?,
                optional_trimmed(proxy.proxy_username),
                optional_trimmed(proxy.proxy_password),
            )),
            None => None,
        };

        let mut items = Vec::with_capacity(ids.len());
        let mut success = 0usize;
        for id in ids {
            let mut error: Option<String> = None;

            if let Some((region, auth_region, api_region)) = &regions {
                if let Err(err) = self
                    .token_manager
                    .set_credential_regions(
                        id,
                        region.clone(),
                        auth_region.clone(),
                        api_region.clone(),
                    )
                    .map_err(|e| self.classify_error(e, id))
                {
                    error = Some(err.to_string());
                }
            }

            if error.is_none() {
                if let Some(max_concurrent_requests) = concurrency {
                    if let Err(err) = self
                        .token_manager
                        .set_credential_max_concurrent_requests(id, max_concurrent_requests)
                        .map_err(|e| self.classify_error(e, id))
                    {
                        error = Some(err.to_string());
                    }
                }
            }

            if error.is_none() {
                if let Some((proxy_resource_id, proxy_url, proxy_username, proxy_password)) = &proxy
                {
                    if let Err(err) = self
                        .token_manager
                        .set_credential_proxy(
                            id,
                            *proxy_resource_id,
                            proxy_url.clone(),
                            proxy_username.clone(),
                            proxy_password.clone(),
                        )
                        .map_err(|e| self.classify_error(e, id))
                    {
                        error = Some(err.to_string());
                    }
                }
            }

            if error.is_none() {
                self.invalidate_balance_cache(id);
                success += 1;
            }
            items.push(BatchUpdateCredentialItem {
                id,
                ok: error.is_none(),
                error,
            });
        }

        let failed = items.len().saturating_sub(success);
        self.audit(
            "batch_update_credentials",
            "credential",
            None,
            failed == 0,
            (failed > 0).then(|| format!("{} 个凭据修改失败", failed)),
            json!({
                "total": items.len(),
                "success": success,
                "failed": failed,
                "regions": regions.is_some(),
                "concurrency": concurrency.is_some(),
                "proxy": proxy.is_some(),
            }),
        );
        Ok(BatchUpdateCredentialsResponse {
            total: items.len(),
            success,
            failed,
            items,
        })
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

    /// 服务端一次性删除全部已禁用凭据，避免前端为确认弹窗加载完整重列表。
    pub fn delete_disabled_credentials(
        &self,
    ) -> Result<BulkCredentialActionResponse, AdminServiceError> {
        let disabled_ids: Vec<u64> = self
            .token_manager
            .base_snapshot()
            .entries
            .into_iter()
            .filter(|credential| credential.disabled)
            .map(|credential| credential.id)
            .collect();

        let total_matched = disabled_ids.len();
        let mut success = 0usize;
        let mut errors = Vec::new();

        for id in disabled_ids {
            match self.delete_credential(id) {
                Ok(()) => {
                    success += 1;
                }
                Err(err) => {
                    errors.push(BulkCredentialActionError {
                        id,
                        message: err.to_string(),
                    });
                }
            }
        }

        let failed = errors.len();
        self.audit(
            "delete_disabled_credentials",
            "credential",
            None,
            failed == 0,
            (failed > 0).then(|| format!("{} 个已禁用凭据删除失败", failed)),
            json!({
                "totalMatched": total_matched,
                "success": success,
                "failed": failed,
            }),
        );

        Ok(BulkCredentialActionResponse {
            total_matched,
            total_attempted: total_matched,
            success,
            failed,
            skipped: 0,
            errors,
        })
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
        let cache_key = admin_usage_summary_cache_key(high_cache_threshold);
        if let Some(cached) = self.read_admin_cache::<UsageSummary>(&cache_key) {
            return cached;
        }

        let summary = self.usage_recorder.summary(high_cache_threshold);
        self.write_admin_cache(cache_key, summary.clone(), ADMIN_USAGE_CACHE_TTL_SECS);
        summary
    }

    /// 获取 Redis-first 聚合的 usage 仪表盘数据。
    ///
    /// Redis 为空或未初始化时回退 PgSQL rollup，避免冷启动没有历史窗口数据。
    pub fn get_usage_dashboard(
        &self,
        timezone: Option<String>,
    ) -> Result<UsageDashboardResponse, AdminServiceError> {
        let high_cache_threshold = self.token_manager.runtime_config().high_cache_threshold;
        let cache_key = admin_usage_dashboard_cache_key(timezone.as_deref(), high_cache_threshold);
        if let Some(cached) = self.read_admin_cache::<UsageDashboardResponse>(&cache_key) {
            return Ok(cached);
        }

        let dashboard = self
            .usage_recorder
            .dashboard(timezone.as_deref(), high_cache_threshold)
            .map_err(|err| AdminServiceError::InternalError(err.to_string()))?;
        self.write_admin_cache(cache_key, dashboard.clone(), ADMIN_USAGE_CACHE_TTL_SECS);
        Ok(dashboard)
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
        self.invalidate_usage_admin_cache();
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
            kiro_upstream_response_timeout_secs: config.kiro_upstream_response_timeout_secs,
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
            payload_guard_external_enabled: config.payload_guard_external_enabled,
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
            kiro_agent_mode_strategy: config.kiro_agent_mode_strategy,
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
        let kiro_upstream_response_timeout_secs = req
            .kiro_upstream_response_timeout_secs
            .unwrap_or(current_config.kiro_upstream_response_timeout_secs);
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
        let payload_guard_external_enabled = req
            .payload_guard_external_enabled
            .unwrap_or(current_config.payload_guard_external_enabled);
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
        let kiro_agent_mode_strategy = req
            .kiro_agent_mode_strategy
            .unwrap_or(current_config.kiro_agent_mode_strategy);
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
        if kiro_upstream_response_timeout_secs > 86_400 {
            return Err(AdminServiceError::InvalidCredential(
                "kiroUpstreamResponseTimeoutSecs 不能大于 86400".to_string(),
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
                config.kiro_upstream_response_timeout_secs = kiro_upstream_response_timeout_secs;
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
                config.payload_guard_external_enabled = payload_guard_external_enabled;
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
                config.kiro_agent_mode_strategy = kiro_agent_mode_strategy;
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
        self.invalidate_external_pool_admin_cache();

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

fn normalize_optional_update(value: Option<Option<String>>) -> Option<Option<String>> {
    value.map(|value| {
        value.and_then(|value| {
            let value = value.trim().to_string();
            (!value.is_empty()).then_some(value)
        })
    })
}

fn normalize_proxy_url(value: Option<String>) -> Result<Option<String>, AdminServiceError> {
    match value {
        Some(value) => {
            let value = value.trim();
            if value.is_empty() {
                Ok(None)
            } else if value.eq_ignore_ascii_case(KiroCredentials::PROXY_DIRECT) {
                Ok(Some(KiroCredentials::PROXY_DIRECT.to_string()))
            } else {
                validate_proxy_url(value).map(Some)
            }
        }
        None => Ok(None),
    }
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
    if credential.provider.as_deref().is_none_or(str::is_empty) {
        credential.provider = defaults.provider.clone();
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
    let credit = credit_snapshot_from_persisted_fields(
        row.subscription_title.as_deref(),
        row.current_usage,
        row.credit_limit,
        row.credit_remaining,
        row.credit_base,
        row.credit_bonus,
    );
    CredentialAccountInfo {
        subscription_title: row.subscription_title.clone(),
        current_usage: row.current_usage,
        usage_limit: row.usage_limit,
        remaining: row.remaining,
        usage_percentage: row.usage_percentage,
        credit_limit: credit.limit,
        credit_remaining: credit.remaining,
        credit_base: credit.base,
        credit_bonus: credit.bonus,
        next_reset_at: row.next_reset_at,
        checked_at: row.checked_at.clone(),
    }
}

fn credential_status_item_from_snapshot(
    entry: CredentialEntrySnapshot,
    default_endpoint: &str,
    current_id: u64,
) -> CredentialStatusItem {
    CredentialStatusItem {
        id: entry.id,
        created_at: entry.created_at,
        updated_at: entry.updated_at,
        priority: entry.priority,
        disabled: entry.disabled,
        failure_count: entry.failure_count,
        is_current: entry.id == current_id,
        expires_at: entry.expires_at,
        auth_method: entry.auth_method,
        provider: entry.provider,
        region: entry.region,
        auth_region: entry.auth_region,
        api_region: entry.api_region,
        effective_auth_region: entry.effective_auth_region,
        effective_api_region: entry.effective_api_region,
        has_profile_arn: entry.has_profile_arn,
        refresh_token_hash: entry.refresh_token_hash,
        api_key_hash: entry.api_key_hash,
        masked_api_key: entry.masked_api_key,
        email: entry.email,
        subscription_title: entry.subscription_title,
        account_info: None,
        success_count: entry.success_count,
        last_used_at: entry.last_used_at,
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
        endpoint: entry
            .endpoint
            .unwrap_or_else(|| default_endpoint.to_string()),
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
        recent_scheduler_selection_count_10s: entry.recent_scheduler_selection_count_10s,
        recent_scheduler_selection_count_60s: entry.recent_scheduler_selection_count_60s,
        recent_scheduler_selection_count_5m: entry.recent_scheduler_selection_count_5m,
        scheduler_selection_pressure: entry.scheduler_selection_pressure,
        scheduler_score: entry.scheduler_score,
        estimated_cost_usd: 0.0,
        priced_requests: 0,
        unpriced_requests: 0,
    }
}

fn credential_list_item_from_base(
    credential: CredentialBaseSnapshot,
    default_endpoint: &str,
) -> CredentialListItem {
    CredentialListItem {
        id: credential.id,
        created_at: credential.created_at,
        updated_at: credential.updated_at,
        priority: credential.priority,
        disabled: credential.disabled,
        disabled_reason: credential.disabled_reason,
        auth_method: credential.auth_method,
        provider: credential.provider,
        region: credential.region,
        auth_region: credential.auth_region,
        api_region: credential.api_region,
        effective_auth_region: credential.effective_auth_region,
        effective_api_region: credential.effective_api_region,
        has_profile_arn: credential.has_profile_arn,
        refresh_token_hash: credential.refresh_token_hash,
        api_key_hash: credential.api_key_hash,
        masked_api_key: credential.masked_api_key,
        email: credential.email,
        subscription_title: credential.subscription_title,
        has_proxy: credential.has_proxy,
        proxy_url: credential.proxy_url,
        proxy_username: credential.proxy_username,
        proxy_password: credential.proxy_password,
        proxy_resource_id: credential.proxy_resource_id,
        proxy_resource_name: credential.proxy_resource_name,
        effective_proxy_url: credential.effective_proxy_url,
        effective_proxy_source: credential.effective_proxy_source,
        endpoint: credential
            .endpoint
            .unwrap_or_else(|| default_endpoint.to_string()),
        max_concurrent_requests_override: credential.max_concurrent_requests_override,
        warmup_remaining: credential.warmup_remaining,
    }
}

fn credential_runtime_item_from_snapshot(
    credential: CredentialEntrySnapshot,
    current_id: u64,
) -> CredentialRuntimeItem {
    CredentialRuntimeItem {
        id: credential.id,
        is_current: credential.id == current_id,
        failure_count: credential.failure_count,
        refresh_failure_count: credential.refresh_failure_count,
        success_count: credential.success_count,
        last_used_at: credential.last_used_at,
        expires_at: credential.expires_at,
        cooled_down: credential.cooled_down,
        cooldown_remaining_secs: credential.cooldown_remaining_secs,
        cooldown_reason: credential.cooldown_reason,
        cooldowns: credential
            .cooldowns
            .into_iter()
            .map(|cooldown| CredentialCooldown {
                model: cooldown.model,
                global: cooldown.global,
                remaining_secs: cooldown.remaining_secs,
                reason: cooldown.reason,
            })
            .collect(),
        rate_limited: credential.rate_limited,
        rate_limit_remaining_secs: credential.rate_limit_remaining_secs,
        in_flight_requests: credential.in_flight_requests,
        oldest_in_flight_age_secs: credential.oldest_in_flight_age_secs,
        newest_in_flight_idle_secs: credential.newest_in_flight_idle_secs,
        max_concurrent_requests: credential.max_concurrent_requests,
        in_flight_lease_max_secs: credential.in_flight_lease_max_secs,
        transient_failure_streak: credential.transient_failure_streak,
        recent_error_rate: credential.recent_error_rate,
        latency_ewma_ms: credential.latency_ewma_ms,
        last_error_kind: credential.last_error_kind,
        last_error_reason: credential.last_error_reason,
        last_error_at_ms: credential.last_error_at_ms,
        in_probation: credential.in_probation,
        probation_remaining_secs: credential.probation_remaining_secs,
        scheduler_selection_count: credential.scheduler_selection_count,
        recent_scheduler_selection_count_10s: credential.recent_scheduler_selection_count_10s,
        recent_scheduler_selection_count_60s: credential.recent_scheduler_selection_count_60s,
        recent_scheduler_selection_count_5m: credential.recent_scheduler_selection_count_5m,
        scheduler_selection_pressure: credential.scheduler_selection_pressure,
        scheduler_score: credential.scheduler_score,
    }
}

fn credential_account_info_item_from_row(
    id: u64,
    row: &CredentialAccountInfoRow,
) -> CredentialAccountInfoItem {
    let info = account_info_from_row(row);
    CredentialAccountInfoItem {
        id,
        subscription_title: info.subscription_title,
        current_usage: info.current_usage,
        usage_limit: info.usage_limit,
        remaining: info.remaining,
        usage_percentage: info.usage_percentage,
        credit_limit: info.credit_limit,
        credit_remaining: info.credit_remaining,
        credit_base: info.credit_base,
        credit_bonus: info.credit_bonus,
        next_reset_at: info.next_reset_at,
        checked_at: info.checked_at,
    }
}

fn normalize_balance_credit_snapshot(balance: &BalanceResponse) -> BalanceResponse {
    let credit = credit_snapshot_from_persisted_fields(
        balance.subscription_title.as_deref(),
        balance.current_usage,
        balance.credit_limit,
        balance.credit_remaining,
        balance.credit_base,
        balance.credit_bonus,
    );
    BalanceResponse {
        id: balance.id,
        checked_at: balance.checked_at.clone(),
        subscription_title: balance.subscription_title.clone(),
        current_usage: balance.current_usage,
        usage_limit: balance.usage_limit,
        remaining: balance.remaining,
        usage_percentage: balance.usage_percentage,
        credit_limit: credit.limit,
        credit_remaining: credit.remaining,
        credit_base: credit.base,
        credit_bonus: credit.bonus,
        next_reset_at: balance.next_reset_at,
    }
}

fn default_sort_order(sort_by: CredentialSortBy) -> CredentialSortOrder {
    match sort_by {
        CredentialSortBy::Priority | CredentialSortBy::SchedulerScore => CredentialSortOrder::Asc,
        _ => CredentialSortOrder::Desc,
    }
}

fn sort_credentials_for_admin_display(credentials: &mut [CredentialStatusItem]) {
    credentials.sort_by(compare_credentials_default);
}

fn sort_credentials_for_admin_display_with_query(
    credentials: &mut [CredentialStatusItem],
    query: &CredentialListQuery,
) {
    let sort_by = CredentialSortBy::parse(query.sort_by.as_deref());
    if sort_by == CredentialSortBy::Default {
        sort_credentials_for_admin_display(credentials);
        return;
    }

    let sort_order = CredentialSortOrder::parse(query.sort_order.as_deref(), sort_by);
    credentials.sort_by(|a, b| {
        compare_credentials_by(a, b, sort_by, sort_order)
            .then_with(|| compare_credentials_default(a, b))
    });
}

fn sort_credential_list_items_for_admin_display_with_query(
    credentials: &mut [CredentialListItem],
    query: &CredentialListQuery,
) {
    let sort_by = CredentialSortBy::parse(query.sort_by.as_deref());
    if sort_by == CredentialSortBy::Default {
        credentials.sort_by(compare_credential_list_default);
        return;
    }

    let sort_order = CredentialSortOrder::parse(query.sort_order.as_deref(), sort_by);
    credentials.sort_by(|a, b| {
        compare_credential_list_by(a, b, sort_by, sort_order)
            .then_with(|| compare_credential_list_default(a, b))
    });
}

fn compare_credential_list_default(
    a: &CredentialListItem,
    b: &CredentialListItem,
) -> std::cmp::Ordering {
    a.disabled
        .cmp(&b.disabled)
        .then_with(|| {
            compare_option_ord_desc_some_first(a.created_at.as_ref(), b.created_at.as_ref())
        })
        .then_with(|| b.id.cmp(&a.id))
}

fn compare_credential_list_by(
    a: &CredentialListItem,
    b: &CredentialListItem,
    sort_by: CredentialSortBy,
    sort_order: CredentialSortOrder,
) -> std::cmp::Ordering {
    match sort_by {
        CredentialSortBy::Default => compare_credential_list_default(a, b),
        CredentialSortBy::Id => sort_order.apply(a.id.cmp(&b.id)),
        CredentialSortBy::CreatedAt => {
            compare_option_ord_some_first(a.created_at.as_ref(), b.created_at.as_ref(), sort_order)
        }
        CredentialSortBy::UpdatedAt => {
            compare_option_ord_some_first(a.updated_at.as_ref(), b.updated_at.as_ref(), sort_order)
        }
        CredentialSortBy::Priority => sort_order.apply(a.priority.cmp(&b.priority)),
        _ => compare_credential_list_default(a, b),
    }
}

fn compare_credentials_default(
    a: &CredentialStatusItem,
    b: &CredentialStatusItem,
) -> std::cmp::Ordering {
    a.disabled
        .cmp(&b.disabled)
        .then_with(|| {
            compare_option_ord_desc_some_first(a.created_at.as_ref(), b.created_at.as_ref())
        })
        .then_with(|| b.id.cmp(&a.id))
}

fn compare_credentials_by(
    a: &CredentialStatusItem,
    b: &CredentialStatusItem,
    sort_by: CredentialSortBy,
    sort_order: CredentialSortOrder,
) -> std::cmp::Ordering {
    match sort_by {
        CredentialSortBy::Default => compare_credentials_default(a, b),
        CredentialSortBy::Id => sort_order.apply(a.id.cmp(&b.id)),
        CredentialSortBy::CreatedAt => {
            compare_option_ord_some_first(a.created_at.as_ref(), b.created_at.as_ref(), sort_order)
        }
        CredentialSortBy::UpdatedAt => {
            compare_option_ord_some_first(a.updated_at.as_ref(), b.updated_at.as_ref(), sort_order)
        }
        CredentialSortBy::Priority => sort_order.apply(a.priority.cmp(&b.priority)),
        CredentialSortBy::LastUsedAt => compare_option_ord_some_first(
            a.last_used_at.as_ref(),
            b.last_used_at.as_ref(),
            sort_order,
        ),
        CredentialSortBy::SuccessCount => sort_order.apply(a.success_count.cmp(&b.success_count)),
        CredentialSortBy::FailureCount => sort_order.apply(a.failure_count.cmp(&b.failure_count)),
        CredentialSortBy::RefreshFailureCount => {
            sort_order.apply(a.refresh_failure_count.cmp(&b.refresh_failure_count))
        }
        CredentialSortBy::EstimatedCost => {
            compare_f64(a.estimated_cost_usd, b.estimated_cost_usd, sort_order)
        }
        CredentialSortBy::UsagePercentage => compare_option_f64_some_first(
            a.account_info.as_ref().map(|info| info.usage_percentage),
            b.account_info.as_ref().map(|info| info.usage_percentage),
            sort_order,
        ),
        CredentialSortBy::RemainingQuota => compare_option_f64_some_first(
            a.account_info.as_ref().map(|info| info.remaining),
            b.account_info.as_ref().map(|info| info.remaining),
            sort_order,
        ),
        CredentialSortBy::InFlightRequests => {
            sort_order.apply(a.in_flight_requests.cmp(&b.in_flight_requests))
        }
        CredentialSortBy::SchedulerScore => {
            compare_f64(a.scheduler_score, b.scheduler_score, sort_order)
        }
    }
}

fn compare_option_ord_desc_some_first<T: Ord>(a: Option<&T>, b: Option<&T>) -> std::cmp::Ordering {
    compare_option_ord_some_first(a, b, CredentialSortOrder::Desc)
}

fn compare_option_ord_some_first<T: Ord>(
    a: Option<&T>,
    b: Option<&T>,
    sort_order: CredentialSortOrder,
) -> std::cmp::Ordering {
    match (a, b) {
        (Some(a), Some(b)) => sort_order.apply(a.cmp(b)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

fn compare_option_f64_some_first(
    a: Option<f64>,
    b: Option<f64>,
    sort_order: CredentialSortOrder,
) -> std::cmp::Ordering {
    match (a, b) {
        (Some(a), Some(b)) => compare_f64(a, b, sort_order),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

fn compare_f64(a: f64, b: f64, sort_order: CredentialSortOrder) -> std::cmp::Ordering {
    sort_order.apply(a.total_cmp(&b))
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

fn credential_base_matches_query(
    credential: &CredentialListItem,
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
            "proxy_blocked" | "proxy-blocked" => matches!(
                credential.effective_proxy_source.as_str(),
                "resource_disabled" | "resource_missing"
            ),
            "unknown_subscription" | "unknown-subscription" => {
                subscription_key(credential.subscription_title.as_deref()) == "unknown"
            }
            // Runtime-owned statuses are hydrated separately; keep the base list broad.
            "current" | "cooldown" | "rate_limited" | "rate-limited" | "error" => true,
            _ => true,
        };
        if !matched {
            return false;
        }
    }

    if let Some(subscription) = query.subscription.as_deref() {
        let expected = subscription.trim().to_lowercase();
        if !expected.is_empty() && expected != "all" {
            let key = subscription_key(credential.subscription_title.as_deref());
            let title_match = credential
                .subscription_title
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
        if !q.is_empty() && !credential_base_search_text(credential).contains(&q) {
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

fn credential_base_search_text(credential: &CredentialListItem) -> String {
    [
        Some(credential.id.to_string()),
        credential.email.clone(),
        credential.masked_api_key.clone(),
        credential.refresh_token_hash.clone(),
        credential.api_key_hash.clone(),
        credential.subscription_title.clone(),
        credential.proxy_resource_name.clone(),
        credential.proxy_url.clone(),
        credential.effective_proxy_url.clone(),
        Some(credential.effective_proxy_source.clone()),
        credential.disabled_reason.clone(),
        Some(credential.endpoint.clone()),
        credential.auth_method.clone(),
        credential.provider.clone(),
        credential.region.clone(),
        credential.auth_region.clone(),
        credential.api_region.clone(),
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
    let now = Utc::now();
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
        if days > 3650 {
            return Err(AdminServiceError::InvalidCredential(
                "olderThanDays 不能超过 3650".to_string(),
            ));
        }
        now - ChronoDuration::days(days as i64)
    };

    if cutoff > now {
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
    fn credential_admin_list_options_include_account_info_snapshot() {
        let options = CredentialStatusBuildOptions::WITH_ACCOUNT_INFO_AND_COST_SUMMARY;

        assert!(options.include_account_info);
        assert!(options.include_cost_summary);
    }

    fn credential_item(
        id: u64,
        disabled: bool,
        created_at: Option<&str>,
        success_count: u64,
        estimated_cost_usd: f64,
        usage_percentage: Option<f64>,
    ) -> CredentialStatusItem {
        CredentialStatusItem {
            id,
            created_at: created_at.map(str::to_string),
            updated_at: None,
            priority: id as u32,
            disabled,
            failure_count: 0,
            is_current: false,
            expires_at: None,
            auth_method: None,
            provider: None,
            region: None,
            auth_region: None,
            api_region: None,
            effective_auth_region: "us-east-1".to_string(),
            effective_api_region: "us-east-1".to_string(),
            has_profile_arn: false,
            refresh_token_hash: None,
            api_key_hash: None,
            masked_api_key: None,
            email: Some(format!("user{}@example.com", id)),
            subscription_title: None,
            account_info: usage_percentage.map(|usage_percentage| CredentialAccountInfo {
                subscription_title: Some("Kiro Pro".to_string()),
                current_usage: usage_percentage,
                usage_limit: 100.0,
                remaining: 100.0 - usage_percentage,
                usage_percentage,
                credit_limit: 11_000.0,
                credit_remaining: (11_000.0 - usage_percentage).max(0.0),
                credit_base: 1_000.0,
                credit_bonus: 10_000.0,
                next_reset_at: None,
                checked_at: "2026-01-01T00:00:00Z".to_string(),
            }),
            success_count,
            last_used_at: None,
            has_proxy: false,
            proxy_url: None,
            proxy_username: None,
            proxy_password: None,
            proxy_resource_id: None,
            proxy_resource_name: None,
            effective_proxy_url: None,
            effective_proxy_source: "none".to_string(),
            refresh_failure_count: 0,
            disabled_reason: None,
            endpoint: "ide".to_string(),
            cooled_down: false,
            cooldown_remaining_secs: 0,
            cooldown_reason: None,
            rate_limited: false,
            rate_limit_remaining_secs: 0,
            in_flight_requests: 0,
            oldest_in_flight_age_secs: 0,
            newest_in_flight_idle_secs: 0,
            max_concurrent_requests: 0,
            max_concurrent_requests_override: None,
            in_flight_lease_max_secs: 0,
            warmup_remaining: 0,
            transient_failure_streak: 0,
            recent_error_rate: 0.0,
            latency_ewma_ms: None,
            last_error_kind: None,
            last_error_reason: None,
            last_error_at_ms: None,
            in_probation: false,
            probation_remaining_secs: 0,
            scheduler_selection_count: 0,
            recent_scheduler_selection_count_10s: 0,
            recent_scheduler_selection_count_60s: 0,
            recent_scheduler_selection_count_5m: 0,
            scheduler_selection_pressure: 0.0,
            scheduler_score: 0.0,
            estimated_cost_usd,
            priced_requests: 0,
            unpriced_requests: 0,
        }
    }

    #[test]
    fn credential_default_sort_keeps_enabled_then_newest_created() {
        let mut credentials = vec![
            credential_item(1, false, Some("2026-01-01T00:00:00Z"), 0, 0.0, None),
            credential_item(2, true, Some("2026-01-03T00:00:00Z"), 0, 0.0, None),
            credential_item(3, false, Some("2026-01-02T00:00:00Z"), 0, 0.0, None),
            credential_item(4, false, None, 0, 0.0, None),
        ];

        sort_credentials_for_admin_display(&mut credentials);

        let ids: Vec<u64> = credentials
            .into_iter()
            .map(|credential| credential.id)
            .collect();
        assert_eq!(ids, vec![3, 1, 4, 2]);
    }

    #[test]
    fn credential_custom_sort_runs_before_pagination_order() {
        let mut credentials = vec![
            credential_item(1, false, Some("2026-01-01T00:00:00Z"), 10, 0.2, Some(20.0)),
            credential_item(2, false, Some("2026-01-02T00:00:00Z"), 30, 0.1, Some(90.0)),
            credential_item(3, true, Some("2026-01-03T00:00:00Z"), 20, 0.3, None),
        ];
        let query = CredentialListQuery {
            sort_by: Some("success_count".to_string()),
            sort_order: Some("desc".to_string()),
            ..Default::default()
        };

        sort_credentials_for_admin_display_with_query(&mut credentials, &query);

        let ids: Vec<u64> = credentials
            .into_iter()
            .map(|credential| credential.id)
            .collect();
        assert_eq!(ids, vec![2, 3, 1]);
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
    fn usage_cleanup_request_zero_days_uses_execution_cutoff() {
        let mut zero_days = cleanup_request();
        zero_days.older_than_days = Some(0);
        let before = Utc::now();
        let plan = normalize_usage_cleanup_request(zero_days).expect("valid zero-day cleanup");
        let after = Utc::now();

        assert!(plan.cutoff >= before);
        assert!(plan.cutoff <= after);
    }

    #[test]
    fn usage_cleanup_request_rejects_unsafe_bounds() {
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

    #[test]
    fn remove_request_api_key_by_id_removes_requested_key() {
        let mut keys = vec!["sk-one".to_string(), "sk-two".to_string()];

        let removed = remove_request_api_key_by_id(&mut keys, &request_api_key_id("sk-one"))
            .expect("key should be removed");

        assert_eq!(removed, "sk-one");
        assert_eq!(keys, vec!["sk-two".to_string()]);
    }

    #[test]
    fn remove_request_api_key_by_id_rejects_missing_key() {
        let mut keys = vec!["sk-one".to_string(), "sk-two".to_string()];

        let err = remove_request_api_key_by_id(&mut keys, &request_api_key_id("sk-missing"))
            .expect_err("missing key should fail");

        assert!(matches!(err, AdminServiceError::InvalidCredential(_)));
        assert_eq!(keys, vec!["sk-one".to_string(), "sk-two".to_string()]);
    }

    #[test]
    fn remove_request_api_key_by_id_rejects_removing_last_key() {
        let mut keys = vec!["sk-one".to_string()];

        let err = remove_request_api_key_by_id(&mut keys, &request_api_key_id("sk-one"))
            .expect_err("last key should not be removable");

        assert!(matches!(err, AdminServiceError::Conflict(_)));
        assert_eq!(keys, vec!["sk-one".to_string()]);
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
