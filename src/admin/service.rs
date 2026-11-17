//! Admin API 业务逻辑服务

#[path = "management.rs"]
pub(crate) mod management;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration as StdDuration, Instant};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use parking_lot::Mutex;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use super::error::AdminServiceError;
use super::types::*;
use crate::account::{
    Account, AccountAuthType, AccountManager, AccountTestResponse, AccountsStatusResponse,
    CreateAccountRequest, SetAccountEnabledRequest, UpdateAccountRequest, account_messages_url,
    account_models_url,
};
use crate::anthropic::request_key_limits::{
    KeyLimitPeriod, limited_periods, spawn_seed_from_history, validate_key_limits,
};
use crate::anthropic::{
    model_capabilities::{
        MANUAL_SOURCE, ModelCapabilitiesCatalog, ModelCapabilitiesStatus, ModelCapabilityItem,
        normalize_model_id, normalize_supported_input_types,
    },
    pricing::{PricingCatalog, PricingStatus},
    request_admission::RequestAdmissionController,
    usage::{
        UsageAccountRiskCostConfig, UsageAccountRiskQuery, UsageAccountRiskResponse,
        UsageDashboardResponse, UsageOverviewRankingsResponse, UsageOverviewResponse,
        UsageOverviewSeriesResponse, UsageOverviewSpec, UsageOverviewSummaryResponse,
        UsageRecordQuery, UsageRecorder, UsageRecorderStats, UsageRecordsPageResult,
        UsageRecordsResult, UsageSummary, resolve_usage_overview_spec,
    },
};
use crate::common::auth::{RequestApiKeyStore, request_api_key_id as stable_request_api_key_id};
use crate::http_client::{
    build_client, response_text_with_limit_and_body_timeout, send_with_response_header_timeout,
};
use crate::model::config::{AccountsConfig, Config, RequestAdmissionConfig, RequestApiKeyPolicy};
use crate::model::model_support::normalize_supported_models;
use crate::storage::account_events::{AccountEvent, AccountEventKind, AccountSource};
use crate::storage::postgres::{
    AccountEventSummaryResponse, AdminAuditLogFilter, AdminAuditLogPage, NewUsageCleanupJob,
    PostgresStore, PostgresUsageStore, UsageCleanupJobProgress, UsageCleanupJobRow,
};
use crate::storage::redis_cache::{RedisPatternDeleteStats, RedisStore};

/// 账号信息缓存过期时间（秒），5 分钟

/// 账号列表每页上限，与前端分页选项的最大值一致。

const MAX_MANUAL_MODEL_ID_LEN: usize = 160;
const USAGE_CLEANUP_DEFAULT_MAX_BATCHES: usize = 10_000;

const MAX_RUNTIME_CONFIG_AUDIT_CHANGES: usize = 200;
const MAX_RUNTIME_CONFIG_AUDIT_STRING_CHARS: usize = 160;
const MAX_RUNTIME_CONFIG_AUDIT_ARRAY_ITEMS: usize = 20;

fn claude_code_model_name(model: &str) -> String {
    let normalized = model.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return normalized;
    }

    let (base, suffix) = if let Some(base) = normalized.strip_suffix("[1m]") {
        (base, "[1m]")
    } else {
        (normalized.as_str(), "")
    };
    let (base, thinking) = if let Some(base) = base.strip_suffix("-thinking") {
        (base, "-thinking")
    } else {
        (base, "")
    };

    let parts = base.split('-').collect::<Vec<_>>();
    if parts.len() < 3 || parts[0] != "claude" {
        return normalized;
    }
    let family = parts[1];

    let all_digits = |value: &str| !value.is_empty() && value.chars().all(|ch| ch.is_ascii_digit());
    let is_date = |value: &str| value.len() == 8 && all_digits(value);

    // Legacy Anthropic IDs use either `claude-3-5-sonnet[-date]` or
    // `claude-3.5-sonnet`. Claude Code exposes these as `sonnet-3.5`
    // (and the corresponding `haiku-3.5`) commands.
    let legacy_family = match parts.as_slice() {
        ["claude", "3", "5", family] if matches!(*family, "sonnet" | "haiku") => Some(*family),
        ["claude", "3", "5", family, date]
            if matches!(*family, "sonnet" | "haiku") && is_date(date) =>
        {
            Some(*family)
        }
        ["claude", "3.5", family] if matches!(*family, "sonnet" | "haiku") => Some(*family),
        ["claude", "3.5", family, date]
            if matches!(*family, "sonnet" | "haiku") && is_date(date) =>
        {
            Some(*family)
        }
        _ => None,
    };
    if let Some(family) = legacy_family {
        return format!("{family}-3.5{thinking}{suffix}");
    }

    if !matches!(family, "opus" | "sonnet" | "haiku") {
        return normalized;
    }

    let version = match parts.as_slice() {
        ["claude", _, version]
            if version
                .split_once('.')
                .is_some_and(|(major, minor)| all_digits(major) && all_digits(minor))
                || all_digits(version) =>
        {
            version.to_string()
        }
        ["claude", _, major, third]
            if all_digits(major) && (all_digits(third) || is_date(third)) =>
        {
            if is_date(third) {
                major.to_string()
            } else {
                format!("{major}.{third}")
            }
        }
        ["claude", _, major, minor, date]
            if all_digits(major) && all_digits(minor) && is_date(date) =>
        {
            format!("{major}.{minor}")
        }
        _ => return normalized,
    };

    format!("{family}-{version}{thinking}{suffix}")
}
const USAGE_CLEANUP_MAX_BATCHES: usize = 10_000;
const USAGE_CLEANUP_DEFAULT_OLDER_THAN_DAYS: u32 = 0;
const USAGE_CLEANUP_DEFAULT_BATCH_SIZE: usize = 10_000;
const USAGE_CLEANUP_MAX_BATCH_SIZE: usize = 10_000;
const USAGE_CLEANUP_MAX_ROWS: u64 = 50_000_000;
const USAGE_CLEANUP_LEASE_SECS: u64 = 30;
const USAGE_CLEANUP_HEARTBEAT_ATTEMPT_TIMEOUT_MS: u64 = 2_000;
const USAGE_CLEANUP_HEARTBEAT_MAX_ATTEMPTS: usize = 3;
const USAGE_CLEANUP_HEARTBEAT_RETRY_MS: u64 = 100;
const USAGE_CLEANUP_RECOVERY_POLL_MIN_SECS: u64 = 1;
const USAGE_CLEANUP_RECOVERY_POLL_MAX_SECS: u64 = 15;
const USAGE_CLEANUP_LOCK_CONTENTION_MAX_RETRIES: usize = 5;
const USAGE_CLEANUP_LOCK_CONTENTION_RETRY_MS: u64 = 25;
const USAGE_CLEANUP_LOCK_CONTENTION_MAX_RETRY_MS: u64 = 400;
const ADMIN_USAGE_CACHE_TTL_SECS: usize = 2;
const ADMIN_ACCOUNT_STATUS_CACHE_TTL_SECS: usize = 2;
const ADMIN_CACHE_DEFAULT_LOCAL_TTL_SECS: usize = 2;
const ADMIN_CACHE_REDIS_READ_TIMEOUT: StdDuration = StdDuration::from_millis(250);
const ADMIN_CACHE_LOCAL_STALE_TTL_SECS: u64 = 30;

const ADMIN_ACCOUNT_TEST_RESPONSE_MAX_BYTES: usize = 64 * 1024;

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

fn admin_usage_overview_cache_key(spec: &UsageOverviewSpec) -> String {
    let timezone = spec.timezone.trim().replace([':', '/', ' '], "_");
    let range_identity = match spec.range_key.as_deref() {
        Some(key) => format!("preset:{}", key),
        None => format!(
            "custom:{}:{}",
            spec.requested_from.to_rfc3339(),
            spec.requested_to.to_rfc3339()
        ),
    };
    format!(
        "admin_cache:usage:overview:{}:{}:{}:{}:{}:{}",
        timezone,
        range_identity,
        spec.from.to_rfc3339(),
        spec.to.to_rfc3339(),
        spec.granularity,
        spec.top_n
    )
}

fn admin_account_list_cache_key() -> &'static str {
    "admin_cache:accounts:list"
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

fn request_api_key_items(
    keys: &[String],
    policies: &[RequestApiKeyPolicy],
) -> Vec<RequestApiKeyItem> {
    let policy_by_key = policies
        .iter()
        .map(|policy| (policy.api_key.as_str(), policy))
        .collect::<HashMap<_, _>>();
    keys.iter()
        .enumerate()
        .map(|(index, key)| {
            let policy = policy_by_key.get(key.as_str()).copied();
            RequestApiKeyItem {
                id: crate::common::auth::request_api_key_id(key),
                api_key: key.clone(),
                masked_api_key: mask_secret(key),
                primary: index == 0,
                name: policy
                    .map(|policy| policy.name.clone())
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| format!("请求 Key {}", index + 1)),
                enabled: policy.map(|policy| policy.enabled).unwrap_or(true),
                request_admission: policy.and_then(|policy| policy.request_admission),
                allowed_routes: policy.map(|p| p.allowed_routes.clone()).unwrap_or_default(),
                usage_limits: policy
                    .map(|policy| policy.usage_limits.clone())
                    .unwrap_or_default(),
            }
        })
        .collect()
}

fn access_keys_response(
    request_api_keys: &[String],
    policies: &[RequestApiKeyPolicy],
    default_request_admission: RequestAdmissionConfig,
    admin_api_key: &str,
) -> AccessKeysResponse {
    let primary = request_api_keys.first().cloned().unwrap_or_default();
    AccessKeysResponse {
        request_api_key: primary.clone(),
        masked_request_api_key: mask_secret(&primary),
        request_api_keys: request_api_key_items(request_api_keys, policies),
        default_request_admission,
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
    format!("sk-claude-pool-{}", URL_SAFE_NO_PAD.encode(bytes))
}

fn remove_request_api_key_by_id(
    keys: &mut Vec<String>,
    key_id: &str,
) -> Result<String, AdminServiceError> {
    let index = keys
        .iter()
        .position(|key| crate::common::auth::request_api_key_id(key) == key_id)
        .ok_or_else(|| AdminServiceError::InvalidCredential("调用 API Key 不存在".to_string()))?;
    if keys.len() <= 1 {
        return Err(AdminServiceError::Conflict(
            "至少需要保留一个调用 API Key".to_string(),
        ));
    }
    Ok(keys.remove(index))
}

/// Admin 服务
///
/// 封装所有 Admin API 的业务逻辑
pub struct AdminService {
    runtime: Arc<crate::model::runtime::RuntimeConfigStore>,
    postgres_store: Arc<PostgresStore>,
    /// Optional independent Redis for usage/statistics/Admin caches and cleanup only.
    observability_redis_store: Option<Arc<RedisStore>>,
    request_api_key_store: Arc<RequestApiKeyStore>,
    request_admission: Arc<RequestAdmissionController>,
    usage_recorder: Arc<UsageRecorder>,
    pricing_catalog: Arc<PricingCatalog>,
    model_capabilities: Arc<ModelCapabilitiesCatalog>,
    account_manager: Arc<AccountManager>,
    usage_cleanup: Arc<Mutex<UsageCleanupRuntime>>,
    admin_cache_shadow: Arc<Mutex<HashMap<String, AdminCacheEntry>>>,
}

pub struct AdminServiceDependencies {
    pub runtime: Arc<crate::model::runtime::RuntimeConfigStore>,
    pub usage_recorder: Arc<UsageRecorder>,
    pub pricing_catalog: Arc<PricingCatalog>,
    pub model_capabilities: Arc<ModelCapabilitiesCatalog>,
    pub postgres_store: Arc<PostgresStore>,
    pub observability_redis_store: Option<Arc<RedisStore>>,
    pub request_api_key_store: Arc<RequestApiKeyStore>,
    pub request_admission: Arc<RequestAdmissionController>,
    pub account_manager: Arc<AccountManager>,
}

#[derive(Debug, Clone)]
struct UsageCleanupPlan {
    mode: UsageCleanupMode,
    include_summary: bool,
    cutoff: DateTime<Utc>,
    batch_size: usize,
    max_batches: usize,
    max_rows: Option<u64>,
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
                phase: "idle".to_string(),
                mode: None,
                include_summary: false,
                cutoff_at: None,
                batch_size: 0,
                max_batches: 0,
                max_rows: None,
                pause_ms_between_batches: 0,
                matched_rows: None,
                remaining_rows: None,
                processed_rows: 0,
                last_batch_rows: 0,
                batches: 0,
                redis_deleted_keys: 0,
                redis_delete_commands: 0,
                redis_max_command_keys: 0,
                redis_scan_passes: 0,
                redis_used_del_fallback: false,
                redis_pass_limit_reached: false,
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
    pub fn new(dependencies: AdminServiceDependencies) -> Self {
        let AdminServiceDependencies {
            runtime,
            usage_recorder,
            pricing_catalog,
            model_capabilities,
            postgres_store,
            observability_redis_store,
            request_api_key_store,
            request_admission,
            account_manager,
        } = dependencies;
        assert!(
            observability_redis_store
                .as_ref()
                .is_none_or(|redis| redis.is_observability()),
            "Admin observability caches must not receive the business Redis store"
        );
        let service = Self {
            runtime,
            postgres_store,
            observability_redis_store,
            request_api_key_store,
            request_admission,
            usage_recorder,
            pricing_catalog,
            model_capabilities,
            account_manager,
            usage_cleanup: Arc::new(Mutex::new(UsageCleanupRuntime::default())),
            admin_cache_shadow: Arc::new(Mutex::new(HashMap::new())),
        };
        service.resume_persisted_usage_cleanup();
        service
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

        let Some(redis) = self.observability_redis_store.clone() else {
            return stale_value.and_then(|value| deserialize_admin_cache_value(&value));
        };
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
                tracing::warn!("读取 observability Redis Admin 缓存失败: {}", err);
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
        let Some(redis) = self.observability_redis_store.clone() else {
            return;
        };
        tokio::spawn(async move {
            if let Err(err) = redis.set_json(key, &value, ttl_secs).await {
                tracing::warn!("写入 observability Redis Admin 缓存失败: {}", err);
            }
        });
    }

    /// Usage summary/dashboard cache writes must complete before the caller returns.
    ///
    /// Cleanup invalidates the same Redis keys in a later phase. Fire-and-forget writes can
    /// otherwise finish after that invalidation and resurrect a pre-cleanup aggregate.
    fn write_usage_admin_cache<T>(&self, key: String, value: T, ttl_secs: usize)
    where
        T: Serialize,
    {
        let value = match serde_json::to_value(&value) {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!("序列化 Usage Admin 缓存失败: {}", err);
                return;
            }
        };
        self.write_admin_cache_shadow(key.clone(), value.clone(), ttl_secs);
        let Some(redis) = self.observability_redis_store.clone() else {
            return;
        };
        if let Err(err) =
            block_on_admin_store(async move { redis.set_json(key, &value, ttl_secs).await })
        {
            tracing::warn!("写入 observability Redis Usage Admin 缓存失败: {}", err);
        }
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
        invalidate_admin_cache_shadow(&self.admin_cache_shadow, pattern);
        let Some(redis) = self.observability_redis_store.clone() else {
            return;
        };
        tokio::spawn(async move {
            if let Err(err) = redis.del_pattern(pattern).await {
                tracing::warn!("清理 observability Redis Admin 缓存失败: {}", err);
            }
        });
    }

    fn invalidate_account_admin_cache_with_pool(&self, reason: &'static str, pool: &Account) {
        self.account_manager
            .notify_account_data_changed_with_local_pool(reason, pool);
        self.invalidate_admin_cache_pattern("admin_cache:accounts:*");
    }

    fn invalidate_account_admin_cache_for_delete(&self, reason: &'static str, pool_id: u64) {
        self.account_manager.notify_account_deleted(reason, pool_id);
        self.invalidate_admin_cache_pattern("admin_cache:accounts:*");
    }

    pub fn get_access_keys(&self, admin_api_key: &str) -> AccessKeysResponse {
        let config = self.runtime.config();
        access_keys_response(
            &config.request_api_keys(),
            &config.request_api_key_policies(),
            config.request_admission.normalized(),
            admin_api_key,
        )
    }

    fn persist_request_api_keys(
        &self,
        keys: Vec<String>,
        mut policies: Vec<RequestApiKeyPolicy>,
        action: &'static str,
        detail: serde_json::Value,
    ) -> Result<(), AdminServiceError> {
        if keys.is_empty() {
            return Err(AdminServiceError::InvalidCredential(
                "至少需要保留一个调用 API Key".to_string(),
            ));
        }
        let routes = self.runtime.config().routes;
        for policy in &mut policies {
            policy.allowed_routes = policy
                .allowed_routes
                .iter()
                .map(|prefix| crate::model::routes::normalize_route_prefix(prefix))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            if policy
                .allowed_routes
                .iter()
                .any(|prefix| !routes.routes.iter().any(|route| &route.prefix == prefix))
            {
                return Err(AdminServiceError::InvalidCredential(
                    "允许路由中包含未定义的前缀".into(),
                ));
            }
        }
        validate_request_api_key_policies(&keys, &policies)
            .map_err(AdminServiceError::InvalidCredential)?;

        self.runtime
            .update(|config| {
                config.set_request_api_keys(keys.clone());
                config.set_request_api_key_policies(policies.clone());
            })
            .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;
        let config = self.runtime.config();
        self.refresh_request_api_key_runtime(&config);
        self.seed_request_key_usage_counters(&config);
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

    /// Adds usage already recorded in the period to each limited window, once per window, so a
    /// new limit counts what was spent before it was set. Runs in the background.
    fn seed_request_key_usage_counters(&self, config: &Config) {
        spawn_seed_from_history(
            self.request_admission.usage_limits().clone(),
            self.postgres_store.clone(),
            limited_periods(&config.request_api_key_policies()),
        );
    }

    /// Current usage against each limit of every request key that has limits.
    pub async fn get_request_key_usage_limits(
        &self,
    ) -> Result<RequestKeyUsageLimitsResponse, AdminServiceError> {
        let config = self.runtime.config();
        let limiter = self.request_admission.usage_limits().clone();
        let now = chrono::Utc::now();
        let mut items = Vec::new();
        for policy in config.request_api_key_policies() {
            if policy.usage_limits.is_empty() {
                continue;
            }
            let key_id = stable_request_api_key_id(&policy.api_key);
            let (statuses, error) = match limiter.statuses(&key_id, now).await {
                Ok(statuses) => (statuses.unwrap_or_default(), None),
                Err(err) => (Vec::new(), Some(err.to_string())),
            };
            items.push(RequestKeyUsageLimitItem {
                key_id,
                enabled: policy.enabled,
                statuses,
                error,
            });
        }
        Ok(RequestKeyUsageLimitsResponse {
            timezone: "Asia/Shanghai".to_string(),
            items,
        })
    }

    /// Clears the current window of one limit period for a key, so it starts from zero.
    pub async fn reset_request_key_usage(
        &self,
        key_id: &str,
        period: KeyLimitPeriod,
    ) -> Result<(), AdminServiceError> {
        let config = self.runtime.config();
        if !config
            .request_api_keys()
            .iter()
            .any(|key| stable_request_api_key_id(key) == key_id)
        {
            return Err(AdminServiceError::InvalidCredential(
                "调用 API Key 不存在".to_string(),
            ));
        }
        self.request_admission
            .usage_limits()
            .reset(key_id, period)
            .await
            .map_err(|err| AdminServiceError::InternalError(format!("重置 Key 用量失败: {err}")))?;
        self.audit(
            "reset_request_key_usage",
            "security_keys",
            Some(key_id.to_string()),
            true,
            None,
            json!({ "period": period.as_str() }),
        );
        Ok(())
    }

    fn refresh_request_api_key_runtime(&self, config: &Config) {
        let policies = config.request_api_key_policies();
        let disabled = policies
            .iter()
            .filter(|policy| !policy.enabled)
            .map(|policy| policy.api_key.as_str())
            .collect::<Vec<_>>();
        self.request_api_key_store
            .replace_keys_with_disabled(config.request_api_keys(), disabled);
        let overrides = policies
            .iter()
            .filter_map(|policy| {
                let admission = policy.request_admission?;
                let identity = self.request_api_key_store.authenticate(&policy.api_key)?;
                Some((identity, admission))
            })
            .collect::<Vec<_>>();
        self.request_admission
            .update_policies(config.request_admission, overrides);
        self.request_admission.usage_limits().update_limits(
            policies
                .iter()
                .filter(|policy| policy.enabled)
                .map(|policy| {
                    (
                        stable_request_api_key_id(&policy.api_key),
                        policy.usage_limits.clone(),
                    )
                }),
        );
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

        let current_config = self.runtime.config();
        let mut keys = current_config.request_api_keys();
        let mut policies = current_config.request_api_key_policies();
        if keys.iter().any(|key| key == &next_key) {
            return Err(AdminServiceError::Conflict(
                "调用 API Key 已存在".to_string(),
            ));
        }
        let admission = req
            .request_admission
            .unwrap_or(current_config.request_admission);
        admission
            .validate()
            .map_err(AdminServiceError::InvalidCredential)?;
        let usage_limits = req.usage_limits.unwrap_or_default();
        validate_key_limits(&usage_limits).map_err(AdminServiceError::InvalidCredential)?;
        keys.push(next_key.clone());
        policies.push(RequestApiKeyPolicy {
            api_key: next_key.clone(),
            name: req.name.unwrap_or_default(),
            enabled: req.enabled.unwrap_or(true),
            request_admission: Some(admission),
            usage_limits,
            allowed_routes: req.allowed_routes.unwrap_or_default(),
        });
        self.persist_request_api_keys(
            keys,
            policies,
            "create_request_api_key",
            json!({
                "keyId": stable_request_api_key_id(&next_key),
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
        let current_config = self.runtime.config();
        let mut keys = current_config.request_api_keys();
        let mut policies = current_config.request_api_key_policies();
        let index = keys
            .iter()
            .position(|key| stable_request_api_key_id(key) == key_id)
            .ok_or_else(|| {
                AdminServiceError::InvalidCredential("调用 API Key 不存在".to_string())
            })?;
        let next_key = match req.api_key.as_deref() {
            Some(key) if key.trim().is_empty() => {
                return Err(AdminServiceError::InvalidCredential(
                    "调用 API Key 不能为空".to_string(),
                ));
            }
            Some(key) => key.trim().to_string(),
            None => keys[index].clone(),
        };
        if next_key.len() < 8 {
            return Err(AdminServiceError::InvalidCredential(
                "调用 API Key 至少需要 8 个字符".to_string(),
            ));
        }
        if let Some(admission) = req.request_admission {
            admission
                .validate()
                .map_err(AdminServiceError::InvalidCredential)?;
        }
        if let Some(usage_limits) = req.usage_limits.as_ref() {
            validate_key_limits(usage_limits).map_err(AdminServiceError::InvalidCredential)?;
        }
        if keys
            .iter()
            .enumerate()
            .any(|(existing_index, key)| existing_index != index && key == &next_key)
        {
            return Err(AdminServiceError::Conflict(
                "调用 API Key 已存在".to_string(),
            ));
        }
        let previous_key = keys[index].clone();
        keys[index] = next_key.clone();
        if let Some(policy) = policies
            .iter_mut()
            .find(|policy| policy.api_key == previous_key)
        {
            policy.api_key = next_key.clone();
            if let Some(name) = req.name.as_ref() {
                policy.name = name.clone();
            }
            if let Some(enabled) = req.enabled {
                policy.enabled = enabled;
            }
            if let Some(admission) = req.request_admission {
                policy.request_admission = Some(admission);
            }
            if let Some(routes) = req.allowed_routes.clone() {
                policy.allowed_routes = routes;
            }
            if let Some(usage_limits) = req.usage_limits.clone() {
                policy.usage_limits = usage_limits;
            }
        } else if req.name.is_some()
            || req.enabled.is_some()
            || req.request_admission.is_some()
            || req.usage_limits.is_some()
            || req.allowed_routes.is_some()
        {
            policies.push(RequestApiKeyPolicy {
                api_key: next_key.clone(),
                name: req.name.unwrap_or_default(),
                enabled: req.enabled.unwrap_or(true),
                request_admission: req.request_admission,
                usage_limits: req.usage_limits.clone().unwrap_or_default(),
                allowed_routes: req.allowed_routes.clone().unwrap_or_default(),
            });
        }
        self.persist_request_api_keys(
            keys,
            policies,
            "update_request_api_key",
            json!({
                "keyId": key_id,
                "nextKeyId": stable_request_api_key_id(&next_key),
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
        let current_config = self.runtime.config();
        let mut keys = current_config.request_api_keys();
        let mut policies = current_config.request_api_key_policies();
        remove_request_api_key_by_id(&mut keys, key_id)?;
        policies.retain(|policy| stable_request_api_key_id(&policy.api_key) != key_id);
        self.persist_request_api_keys(
            keys,
            policies,
            "delete_request_api_key",
            json!({ "keyId": key_id }),
        )?;
        Ok(self.get_access_keys(admin_api_key))
    }

    pub fn list_accounts(&self) -> Result<Vec<Account>, AdminServiceError> {
        let cache_key = admin_account_list_cache_key();
        if let Some(cached) = self.read_admin_cache::<Vec<Account>>(cache_key) {
            return Ok(cached.into_iter().map(management::mask_account).collect());
        }

        let store = self.postgres_store.clone();
        let pools = block_on_admin_store(async move { store.list_accounts(true).await })
            .map_err(|err| AdminServiceError::InternalError(err.to_string()))?;
        let pools = pools
            .into_iter()
            .map(management::mask_account)
            .collect::<Vec<_>>();
        self.write_admin_cache(
            cache_key.to_string(),
            pools.clone(),
            ADMIN_ACCOUNT_STATUS_CACHE_TTL_SECS,
        );
        Ok(pools)
    }

    pub fn create_account(
        &self,
        mut request: CreateAccountRequest,
    ) -> Result<Account, AdminServiceError> {
        request.supported_models =
            Self::normalize_account_supported_models(request.supported_models);
        let store = self.postgres_store.clone();
        let pool =
            block_on_admin_store(async move { store.create_account_unmasked(request).await })
                .map_err(|err| AdminServiceError::InvalidCredential(err.to_string()))?;
        self.audit(
            "create_account",
            "account",
            Some(pool.id.to_string()),
            true,
            None,
            json!({ "name": pool.name, "baseUrl": pool.base_url }),
        );
        self.invalidate_account_admin_cache_with_pool("create", &pool);
        Ok(management::mask_account(pool.masked_for_admin_response()))
    }

    pub fn update_account(
        &self,
        id: u64,
        mut request: UpdateAccountRequest,
    ) -> Result<Account, AdminServiceError> {
        request.supported_models = request
            .supported_models
            .map(Self::normalize_account_supported_models);
        let store = self.postgres_store.clone();
        let pool =
            block_on_admin_store(async move { store.update_account_unmasked(id, request).await })
                .map_err(|err| AdminServiceError::InvalidCredential(err.to_string()))?
                .ok_or(AdminServiceError::NotFound { id })?;
        self.audit(
            "update_account",
            "account",
            Some(id.to_string()),
            true,
            None,
            json!({ "name": pool.name, "baseUrl": pool.base_url }),
        );
        self.invalidate_account_admin_cache_with_pool("update", &pool);
        Ok(management::mask_account(pool.masked_for_admin_response()))
    }

    pub fn set_account_supported_models(
        &self,
        id: u64,
        request: SetSupportedModelsRequest,
    ) -> Result<SupportedModelsResponse, AdminServiceError> {
        let supported_models = Self::normalize_account_supported_models(request.supported_models);
        let store = self.postgres_store.clone();
        let pool = block_on_admin_store(async move {
            store
                .set_account_supported_models_unmasked(id, supported_models)
                .await
        })
        .map_err(|err| AdminServiceError::InvalidCredential(err.to_string()))?
        .ok_or(AdminServiceError::NotFound { id })?;
        self.audit(
            "set_account_supported_models",
            "account",
            Some(id.to_string()),
            true,
            None,
            json!({ "supportedModels": pool.supported_models.clone() }),
        );
        self.invalidate_account_admin_cache_with_pool("supported_models", &pool);
        Ok(SupportedModelsResponse {
            count: pool.supported_models.len(),
            supported_models: pool.supported_models,
        })
    }

    pub async fn sync_account_supported_models(
        &self,
        id: u64,
        request: DiscoverAccountSupportedModelsRequest,
    ) -> Result<SupportedModelsResponse, AdminServiceError> {
        let supported_models = self
            .discover_account_supported_model_ids(Some(id), request)
            .await?;
        let store = self.postgres_store.clone();
        let supported_models_for_store = supported_models.clone();
        let pool = block_on_admin_store(async move {
            store
                .set_account_supported_models_unmasked(id, supported_models_for_store)
                .await
        })
        .map_err(|err| AdminServiceError::InvalidCredential(err.to_string()))?
        .ok_or(AdminServiceError::NotFound { id })?;
        self.audit(
            "sync_account_supported_models",
            "account",
            Some(id.to_string()),
            true,
            None,
            json!({
                "supportedModels": pool.supported_models.clone(),
            }),
        );
        self.invalidate_account_admin_cache_with_pool("supported_models_sync", &pool);
        Ok(SupportedModelsResponse {
            count: pool.supported_models.len(),
            supported_models: pool.supported_models,
        })
    }

    pub async fn promote_account_models(
        &self,
        id: u64,
    ) -> Result<SupportedModelsResponse, AdminServiceError> {
        let pool = self
            .postgres_store
            .get_account(id, false)
            .await
            .map_err(|err| AdminServiceError::InternalError(err.to_string()))?
            .ok_or_else(|| AdminServiceError::InvalidCredential("账号不存在".into()))?;
        let promoted = self
            .postgres_store
            .promote_account_models(&pool.supported_models)
            .await
            .map_err(|err| AdminServiceError::InternalError(err.to_string()))?;
        if let Some(status) = self
            .postgres_store
            .load_model_capabilities_status()
            .await
            .map_err(|err| AdminServiceError::InternalError(err.to_string()))?
        {
            self.model_capabilities.load_persisted_status(status);
        }
        Ok(SupportedModelsResponse {
            count: promoted.len(),
            supported_models: promoted,
        })
    }

    /// 使用账号自身的兼容 /v1/models 接口发现模型，只返回可编辑建议，不写回。
    pub async fn discover_account_supported_models(
        &self,
        id: Option<u64>,
        request: DiscoverAccountSupportedModelsRequest,
    ) -> Result<SupportedModelsResponse, AdminServiceError> {
        let supported_models = self
            .discover_account_supported_model_ids(id, request)
            .await?;
        Ok(SupportedModelsResponse {
            count: supported_models.len(),
            supported_models,
        })
    }

    async fn discover_account_supported_model_ids(
        &self,
        id: Option<u64>,
        request: DiscoverAccountSupportedModelsRequest,
    ) -> Result<Vec<String>, AdminServiceError> {
        let saved_pool = if let Some(id) = id {
            let store = self.postgres_store.clone();
            Some(
                block_on_admin_store(async move { store.get_account(id, false).await })
                    .map_err(|err| AdminServiceError::InternalError(err.to_string()))?
                    .ok_or(AdminServiceError::NotFound { id })?,
            )
        } else {
            None
        };

        let base_url = request
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| saved_pool.as_ref().map(|pool| pool.base_url.clone()))
            .ok_or_else(|| {
                AdminServiceError::InvalidCredential("账号 Base URL 不能为空".to_string())
            })?;
        let api_key = request
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| saved_pool.as_ref().and_then(|pool| pool.api_key.clone()))
            .ok_or_else(|| AdminServiceError::InvalidCredential("账号 Key 不能为空".to_string()))?;
        let auth_type = request
            .auth_type
            .or_else(|| saved_pool.as_ref().map(|pool| pool.auth_type))
            .unwrap_or(AccountAuthType::Bearer);
        let url = account_models_url(&base_url).map_err(|err| {
            AdminServiceError::InvalidCredential(format!("账号模型列表 URL 无效: {err}"))
        })?;
        let config = self.runtime.config();
        let timeout_secs = config.accounts.request_timeout_secs.clamp(1, 60);
        let client = build_client(None, timeout_secs, config.tls_backend)
            .map_err(|err| AdminServiceError::InternalError(err.to_string()))?;
        let mut request_builder = client
            .get(url)
            .header("accept", "application/json")
            .header("anthropic-version", "2023-06-01");
        match auth_type {
            AccountAuthType::Bearer => {
                request_builder = request_builder.bearer_auth(api_key);
            }
            AccountAuthType::XApiKey => {
                request_builder = request_builder.header("x-api-key", api_key);
            }
        }
        let response = send_with_response_header_timeout(request_builder, timeout_secs)
            .await
            .map_err(|err| {
                AdminServiceError::InvalidCredential(format!("账号模型列表请求失败: {err}"))
            })?;
        let status = response.status();
        let body =
            response_text_with_limit_and_body_timeout(response, timeout_secs, 4 * 1024 * 1024)
                .await
                .map_err(|err| {
                    AdminServiceError::InvalidCredential(format!("读取账号模型列表失败: {err}"))
                })?;
        if !status.is_success() {
            let suffix = body.chars().take(500).collect::<String>();
            return Err(AdminServiceError::InvalidCredential(format!(
                "账号模型列表请求失败: {} {}",
                status, suffix
            )));
        }
        let supported_models =
            Self::normalize_account_supported_models(extract_model_ids_from_models_response(&body));
        if supported_models.is_empty() {
            return Err(AdminServiceError::InvalidCredential(
                "账号模型列表响应中没有可识别的模型 ID".to_string(),
            ));
        }
        Ok(supported_models)
    }

    pub fn delete_account(&self, id: u64) -> Result<(), AdminServiceError> {
        let store = self.postgres_store.clone();
        let deleted = block_on_admin_store(async move { store.soft_delete_account(id).await })
            .map_err(|err| AdminServiceError::InternalError(err.to_string()))?;
        if !deleted {
            return Err(AdminServiceError::NotFound { id });
        }
        self.audit_account_change(
            "delete_account",
            AccountEventKind::Deleted,
            AccountSource::External,
            id,
            format!("#{id}"),
            "删除账号",
            json!({}),
        );
        self.invalidate_account_admin_cache_for_delete("delete", id);
        Ok(())
    }

    pub fn set_account_enabled(
        &self,
        id: u64,
        request: SetAccountEnabledRequest,
    ) -> Result<Account, AdminServiceError> {
        let store = self.postgres_store.clone();
        let pool = block_on_admin_store(async move {
            store
                .set_account_enabled_unmasked(id, request.enabled)
                .await
        })
        .map_err(|err| AdminServiceError::InternalError(err.to_string()))?
        .ok_or(AdminServiceError::NotFound { id })?;
        let (kind, reason_label) = if request.enabled {
            (AccountEventKind::ManualEnabled, "手动启用")
        } else {
            (AccountEventKind::ManualDisabled, "手动禁用")
        };
        self.audit_account_change(
            "set_account_enabled",
            kind,
            AccountSource::External,
            id,
            pool.name.clone(),
            reason_label,
            json!({ "enabled": request.enabled }),
        );
        self.invalidate_account_admin_cache_with_pool("enabled", &pool);
        Ok(management::mask_account(pool.masked_for_admin_response()))
    }

    pub fn clear_account_auto_disabled(&self, id: u64) -> Result<Account, AdminServiceError> {
        let store = self.postgres_store.clone();
        let pool =
            block_on_admin_store(
                async move { store.clear_account_auto_disabled_unmasked(id).await },
            )
            .map_err(|err| AdminServiceError::InternalError(err.to_string()))?
            .ok_or(AdminServiceError::NotFound { id })?;
        self.audit_account_change(
            "clear_account_auto_disabled",
            AccountEventKind::ManualEnabled,
            AccountSource::External,
            id,
            pool.name.clone(),
            "清除自动禁用",
            json!({}),
        );
        self.invalidate_account_admin_cache_with_pool("clear_auto_disabled", &pool);
        Ok(management::mask_account(pool.masked_for_admin_response()))
    }

    pub fn clear_account_cooldown(&self, id: u64) -> Result<Account, AdminServiceError> {
        let store = self.postgres_store.clone();
        let pool = block_on_admin_store(async move { store.get_account(id, true).await })
            .map_err(|err| AdminServiceError::InternalError(err.to_string()))?
            .ok_or(AdminServiceError::NotFound { id })?;
        let manager = self.account_manager.clone();
        let deleted = block_on_admin_store(async move { manager.clear_pool_cooldowns(id).await })
            .map_err(|err| AdminServiceError::InternalError(err.to_string()))?;
        self.audit(
            "clear_account_cooldown",
            "account",
            Some(id.to_string()),
            true,
            None,
            json!({ "deletedKeys": deleted }),
        );
        self.invalidate_admin_cache_pattern("admin_cache:accounts:*");
        Ok(management::mask_account(pool.masked_for_admin_response()))
    }

    pub fn get_account_status(&self) -> Result<AccountsStatusResponse, AdminServiceError> {
        let manager = self.account_manager.clone();
        let config = self.runtime.config().accounts;
        let mut response = block_on_admin_store(async move {
            let accounts = manager.status(&config).await?;
            let summary = manager.status_summary(&accounts).await;
            Ok::<_, anyhow::Error>(AccountsStatusResponse { accounts, summary })
        })
        .map_err(|err| AdminServiceError::InternalError(err.to_string()))?;
        for status in &mut response.accounts {
            status.account = management::mask_account(status.account.clone());
        }
        Ok(response)
    }

    pub fn test_account(
        &self,
        id: u64,
        req: Option<AccountTestRequest>,
    ) -> Result<AccountTestResponse, AdminServiceError> {
        let store = self.postgres_store.clone();
        let pool = block_on_admin_store(async move { store.get_account(id, false).await })
            .map_err(|err| AdminServiceError::InternalError(err.to_string()))?
            .ok_or(AdminServiceError::NotFound { id })?;
        block_on_admin_store(async move {
            let model = req
                .as_ref()
                .map(|req| req.model.trim().to_string())
                .filter(|model| !model.is_empty());
            let url = if model.is_some() {
                account_messages_url(&pool.base_url)?
            } else {
                account_models_url(&pool.base_url)?
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
                crate::account::AccountAuthType::Bearer => {
                    request = request.bearer_auth(pool.api_key.unwrap_or_default());
                }
                crate::account::AccountAuthType::XApiKey => {
                    request = request.header("x-api-key", pool.api_key.unwrap_or_default());
                }
            }
            let result = request.send().await;
            Ok::<AccountTestResponse, anyhow::Error>(match result {
                Ok(response) => {
                    let status = response.status();
                    let body = match response_text_with_limit_and_body_timeout(
                        response,
                        15,
                        ADMIN_ACCOUNT_TEST_RESPONSE_MAX_BYTES,
                    )
                    .await
                    {
                        Ok(body) => body,
                        Err(error) => {
                            return Ok(AccountTestResponse {
                                ok: false,
                                status: Some(status.as_u16()),
                                message: format!("读取账号测试响应失败: {error}"),
                                model,
                                response: None,
                            });
                        }
                    };
                    let response_text = if status.is_success() {
                        extract_account_test_response_text(&body)
                    } else {
                        None
                    };
                    AccountTestResponse {
                        ok: status.is_success(),
                        status: Some(status.as_u16()),
                        message: if status.is_success() {
                            if model.is_some() {
                                "账号模型调用测试通过".to_string()
                            } else {
                                "账号模型列表测试通过".to_string()
                            }
                        } else {
                            let suffix = body.chars().take(300).collect::<String>();
                            if suffix.is_empty() {
                                format!("账号测试失败: {}", status)
                            } else {
                                format!("账号测试失败: {}; {}", status, suffix)
                            }
                        },
                        model,
                        response: response_text,
                    }
                }
                Err(err) => AccountTestResponse {
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
        self.runtime
            .update(|config| {
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

    /// Admin audit row that also shows up in the account change view.
    fn audit_account_change(
        &self,
        action: &'static str,
        kind: AccountEventKind,
        source: AccountSource,
        account_id: u64,
        account_label: String,
        reason_label: &'static str,
        detail: serde_json::Value,
    ) {
        let (_, object_type, object_id, detail) = AccountEvent {
            kind,
            source,
            account_id,
            account_label,
            reason: action.to_string(),
            reason_label: reason_label.to_string(),
            detail,
        }
        .into_audit_parts();
        self.audit(action, object_type, Some(object_id), true, None, detail);
    }

    fn runtime_config_audit_detail(before: &Config, after: &Config) -> Value {
        let Ok(before) = serde_json::to_value(before) else {
            return json!({
                "kind": "runtime_config_change",
                "changedCount": 0,
                "changes": [],
                "truncated": false,
                "error": "无法序列化保存前配置",
            });
        };
        let Ok(after) = serde_json::to_value(after) else {
            return json!({
                "kind": "runtime_config_change",
                "changedCount": 0,
                "changes": [],
                "truncated": false,
                "error": "无法序列化保存后配置",
            });
        };

        let mut changes = Vec::new();
        Self::append_runtime_config_changes("", &before, &after, &mut changes);
        let truncated = changes.len() >= MAX_RUNTIME_CONFIG_AUDIT_CHANGES;
        json!({
            "kind": "runtime_config_change",
            "changedCount": changes.len(),
            "changes": changes,
            "truncated": truncated,
        })
    }

    fn append_runtime_config_changes(
        path: &str,
        before: &Value,
        after: &Value,
        changes: &mut Vec<Value>,
    ) {
        if before == after || changes.len() >= MAX_RUNTIME_CONFIG_AUDIT_CHANGES {
            return;
        }
        if Self::runtime_config_audit_sensitive_path(path) {
            changes.push(json!({
                "path": path,
                "before": "[已隐藏]",
                "after": "[已隐藏]",
                "redacted": true,
            }));
            return;
        }

        match (before, after) {
            (Value::Object(before), Value::Object(after)) => {
                let mut keys = BTreeSet::new();
                keys.extend(before.keys());
                keys.extend(after.keys());
                for key in keys {
                    let child_path = if path.is_empty() {
                        key.to_string()
                    } else {
                        format!("{path}.{key}")
                    };
                    let missing = Value::Null;
                    Self::append_runtime_config_changes(
                        &child_path,
                        before.get(key).unwrap_or(&missing),
                        after.get(key).unwrap_or(&missing),
                        changes,
                    );
                    if changes.len() >= MAX_RUNTIME_CONFIG_AUDIT_CHANGES {
                        return;
                    }
                }
            }
            (Value::Array(before), Value::Array(after)) => {
                changes.push(json!({
                    "path": path,
                    "before": Self::summarize_runtime_config_array(before),
                    "after": Self::summarize_runtime_config_array(after),
                }));
            }
            _ => {
                changes.push(json!({
                    "path": path,
                    "before": Self::summarize_runtime_config_value(before),
                    "after": Self::summarize_runtime_config_value(after),
                }));
            }
        }
    }

    fn runtime_config_audit_sensitive_path(path: &str) -> bool {
        path.split('.')
            .map(|segment| segment.split('[').next().unwrap_or(segment))
            .any(|segment| {
                matches!(
                    segment,
                    "apiKey"
                        | "apiKeys"
                        | "adminApiKey"
                        | "countTokensApiKey"
                        | "accessToken"
                        | "apiToken"
                        | "authorization"
                        | "clientSecret"
                        | "upstreamApiKey"
                        | "proxyPassword"
                        | "password"
                        | "refreshToken"
                        | "secret"
                        | "token"
                )
            })
    }

    fn summarize_runtime_config_value(value: &Value) -> Value {
        match value {
            Value::String(value) => {
                let truncated: String = value
                    .chars()
                    .take(MAX_RUNTIME_CONFIG_AUDIT_STRING_CHARS)
                    .collect();
                if truncated.chars().count() < value.chars().count() {
                    Value::String(format!("{truncated}…"))
                } else {
                    Value::String(truncated)
                }
            }
            Value::Object(object) => json!({
                "type": "object",
                "keys": object.len(),
            }),
            Value::Array(array) => Self::summarize_runtime_config_array(array),
            value => value.clone(),
        }
    }

    fn summarize_runtime_config_array(array: &[Value]) -> Value {
        let mut summary = serde_json::Map::new();
        summary.insert("count".to_string(), json!(array.len()));
        if array.len() <= MAX_RUNTIME_CONFIG_AUDIT_ARRAY_ITEMS
            && array.iter().all(|item| {
                matches!(
                    item,
                    Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)
                )
            })
        {
            summary.insert(
                "items".to_string(),
                Value::Array(
                    array
                        .iter()
                        .map(Self::summarize_runtime_config_value)
                        .collect(),
                ),
            );
        }
        Value::Object(summary)
    }

    fn normalize_audit_detail(value: Value) -> Value {
        Self::normalize_audit_value("", value)
    }

    fn normalize_audit_value(path: &str, value: Value) -> Value {
        if Self::runtime_config_audit_sensitive_path(path) {
            return Value::String("[已隐藏]".to_string());
        }
        match value {
            Value::String(value) => {
                let truncated: String = value
                    .chars()
                    .take(MAX_RUNTIME_CONFIG_AUDIT_STRING_CHARS)
                    .collect();
                if truncated.chars().count() < value.chars().count() {
                    Value::String(format!("{truncated}…"))
                } else {
                    Value::String(truncated)
                }
            }
            Value::Array(values) => {
                let count = values.len();
                let item_limit = if path.ends_with("changes") {
                    MAX_RUNTIME_CONFIG_AUDIT_CHANGES
                } else {
                    MAX_RUNTIME_CONFIG_AUDIT_ARRAY_ITEMS
                };
                let items = values
                    .into_iter()
                    .take(item_limit)
                    .enumerate()
                    .map(|(index, item)| {
                        Self::normalize_audit_value(&format!("{path}[{index}]"), item)
                    })
                    .collect::<Vec<_>>();
                if path.ends_with("changes") || count <= item_limit {
                    return Value::Array(items);
                }
                let mut summary = serde_json::Map::new();
                summary.insert("count".to_string(), json!(count));
                summary.insert("items".to_string(), Value::Array(items));
                summary.insert("truncated".to_string(), Value::Bool(true));
                Value::Object(summary)
            }
            Value::Object(values) => {
                let mut normalized = serde_json::Map::new();
                for (key, item) in values.into_iter().take(MAX_RUNTIME_CONFIG_AUDIT_CHANGES) {
                    let child_path = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    normalized.insert(key, Self::normalize_audit_value(&child_path, item));
                }
                Value::Object(normalized)
            }
            value => value,
        }
    }

    pub(super) fn audit(
        &self,
        action: &'static str,
        object_type: &'static str,
        object_id: Option<String>,
        success: bool,
        error_message: Option<String>,
        detail: serde_json::Value,
    ) {
        let detail = Self::normalize_audit_detail(detail);
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

    /// External-pool allowlists are exposed in Claude Code command syntax.
    /// Keep arbitrary provider-specific IDs intact, but collapse official
    /// Claude dated/dashed IDs to a stable family/version command.
    fn normalize_account_supported_models(model_ids: Vec<String>) -> Vec<String> {
        let mut normalized = Vec::new();
        for model in normalize_supported_models(model_ids) {
            let canonical = claude_code_model_name(&model);
            if !normalized.iter().any(|existing| existing == &canonical) {
                normalized.push(canonical);
            }
        }
        normalized
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
        let high_cache_threshold = self.runtime.config().high_cache_threshold;
        let cache_key = admin_usage_summary_cache_key(high_cache_threshold);
        if let Some(cached) = self.read_admin_cache::<UsageSummary>(&cache_key) {
            return cached;
        }

        let summary = self.usage_recorder.summary(high_cache_threshold);
        self.write_usage_admin_cache(cache_key, summary.clone(), ADMIN_USAGE_CACHE_TTL_SECS);
        summary
    }

    /// 获取 Redis-first 聚合的 usage 仪表盘数据。
    ///
    /// Redis 为空或未初始化时回退 PgSQL rollup，避免冷启动没有历史窗口数据。
    pub fn get_usage_dashboard(
        &self,
        timezone: Option<String>,
    ) -> Result<UsageDashboardResponse, AdminServiceError> {
        let high_cache_threshold = self.runtime.config().high_cache_threshold;
        let cache_key = admin_usage_dashboard_cache_key(timezone.as_deref(), high_cache_threshold);
        if let Some(cached) = self.read_admin_cache::<UsageDashboardResponse>(&cache_key) {
            return Ok(cached);
        }

        let dashboard = self
            .usage_recorder
            .dashboard(timezone.as_deref(), high_cache_threshold)
            .map_err(|err| AdminServiceError::InternalError(err.to_string()))?;
        self.write_usage_admin_cache(cache_key, dashboard.clone(), ADMIN_USAGE_CACHE_TTL_SECS);
        Ok(dashboard)
    }

    pub fn get_usage_dashboard_overview(
        &self,
        timezone: Option<String>,
        range: Option<String>,
        from: Option<String>,
        to: Option<String>,
        granularity: Option<String>,
        top_n: Option<usize>,
    ) -> Result<UsageOverviewResponse, AdminServiceError> {
        let spec = self.resolve_usage_overview_spec(
            timezone.as_deref(),
            range.as_deref(),
            from.as_deref(),
            to.as_deref(),
            granularity.as_deref(),
            top_n,
        )?;
        self.get_cached_usage_dashboard_overview(spec)
    }

    fn get_cached_usage_dashboard_overview(
        &self,
        spec: UsageOverviewSpec,
    ) -> Result<UsageOverviewResponse, AdminServiceError> {
        let cache_key = admin_usage_overview_cache_key(&spec);
        if let Some(cached) = self.read_admin_cache::<UsageOverviewResponse>(&cache_key) {
            return Ok(cached);
        }

        let overview = self
            .usage_recorder
            .dashboard_overview(spec)
            .map_err(|err| AdminServiceError::InternalError(err.to_string()))?;
        self.write_usage_admin_cache(cache_key, overview.clone(), ADMIN_USAGE_CACHE_TTL_SECS);
        Ok(overview)
    }

    fn resolve_usage_overview_spec(
        &self,
        timezone: Option<&str>,
        range: Option<&str>,
        from: Option<&str>,
        to: Option<&str>,
        granularity: Option<&str>,
        top_n: Option<usize>,
    ) -> Result<crate::anthropic::usage::UsageOverviewSpec, AdminServiceError> {
        resolve_usage_overview_spec(Utc::now(), timezone, range, from, to, granularity, top_n)
            .map_err(|err| AdminServiceError::InvalidCredential(err.to_string()))
    }

    pub fn get_usage_dashboard_overview_summary(
        &self,
        timezone: Option<String>,
        range: Option<String>,
        from: Option<String>,
        to: Option<String>,
        granularity: Option<String>,
        top_n: Option<usize>,
    ) -> Result<UsageOverviewSummaryResponse, AdminServiceError> {
        let spec = self.resolve_usage_overview_spec(
            timezone.as_deref(),
            range.as_deref(),
            from.as_deref(),
            to.as_deref(),
            granularity.as_deref(),
            top_n,
        )?;
        let overview = self.get_cached_usage_dashboard_overview(spec)?;
        Ok(UsageOverviewSummaryResponse {
            generated_at: overview.generated_at.clone(),
            timezone: overview.timezone.clone(),
            range: overview.range.clone(),
            totals: overview.totals,
            external: crate::anthropic::usage::UsageOverviewExternalSummary {
                raw_cost_usd: overview.external.raw_cost_usd,
                shaped_cost_usd: overview.external.shaped_cost_usd,
                uplifted_cost_usd: overview.external.uplifted_cost_usd,
                reported_cost_usd: overview.external.reported_cost_usd,
                billable_cost_usd: overview.external.billable_cost_usd,
                profit_usd: overview.external.profit_usd,
                cost_floor_delta_usd: overview.external.cost_floor_delta_usd,
                cost_floor_applied_requests: overview.external.cost_floor_applied_requests,
            },
        })
    }

    pub fn get_usage_dashboard_overview_series(
        &self,
        timezone: Option<String>,
        range: Option<String>,
        from: Option<String>,
        to: Option<String>,
        granularity: Option<String>,
        top_n: Option<usize>,
    ) -> Result<UsageOverviewSeriesResponse, AdminServiceError> {
        let spec = self.resolve_usage_overview_spec(
            timezone.as_deref(),
            range.as_deref(),
            from.as_deref(),
            to.as_deref(),
            granularity.as_deref(),
            top_n,
        )?;
        let overview = self.get_cached_usage_dashboard_overview(spec)?;
        Ok(UsageOverviewSeriesResponse {
            generated_at: overview.generated_at,
            timezone: overview.timezone,
            range: overview.range,
            series: overview.series,
            series_groups: overview.series_groups,
        })
    }

    pub fn get_usage_dashboard_overview_rankings(
        &self,
        timezone: Option<String>,
        range: Option<String>,
        from: Option<String>,
        to: Option<String>,
        granularity: Option<String>,
        top_n: Option<usize>,
    ) -> Result<UsageOverviewRankingsResponse, AdminServiceError> {
        let spec = self.resolve_usage_overview_spec(
            timezone.as_deref(),
            range.as_deref(),
            from.as_deref(),
            to.as_deref(),
            granularity.as_deref(),
            top_n,
        )?;
        let overview = self.get_cached_usage_dashboard_overview(spec)?;
        Ok(UsageOverviewRankingsResponse {
            generated_at: overview.generated_at,
            timezone: overview.timezone,
            range: overview.range,
            total_requests: overview.totals.all.requests,
            total_errors: overview.totals.all.error_requests,
            top_models: overview.top_models,
            top_accounts: overview.top_accounts,
            top_keys: overview.top_keys,
            top_paths: overview.top_paths,
            top_errors: overview.top_errors,
        })
    }

    pub fn get_usage_dashboard_windows(
        &self,
        timezone: Option<String>,
    ) -> Result<crate::anthropic::usage::UsageDashboardWindowsResponse, AdminServiceError> {
        let high_cache_threshold = self.runtime.config().high_cache_threshold;
        self.usage_recorder
            .dashboard_windows(timezone.as_deref(), high_cache_threshold)
            .map_err(|err| AdminServiceError::InternalError(err.to_string()))
    }

    pub fn get_usage_dashboard_series(
        &self,
        timezone: Option<String>,
    ) -> Result<crate::anthropic::usage::UsageDashboardSeriesResponse, AdminServiceError> {
        self.usage_recorder
            .dashboard_series(timezone.as_deref())
            .map_err(|err| AdminServiceError::InternalError(err.to_string()))
    }

    pub fn get_usage_dashboard_top(
        &self,
        timezone: Option<String>,
        window_key: Option<String>,
    ) -> Result<crate::anthropic::usage::UsageDashboardTopResponse, AdminServiceError> {
        self.usage_recorder
            .dashboard_top(timezone.as_deref(), window_key.as_deref())
            .map_err(|err| AdminServiceError::InternalError(err.to_string()))
    }

    pub fn get_usage_dashboard_breakdown(
        &self,
        timezone: Option<String>,
        window_key: String,
    ) -> Result<crate::anthropic::usage::UsageDashboardBreakdownResponse, AdminServiceError> {
        self.usage_recorder
            .dashboard_breakdown(timezone.as_deref(), &window_key)
            .map_err(|err| AdminServiceError::InternalError(err.to_string()))
    }

    pub fn get_usage_dashboard_account_billing(
        &self,
        timezone: Option<String>,
        window_key: String,
    ) -> Result<crate::anthropic::usage::UsageDashboardAccountBillingResponse, AdminServiceError>
    {
        self.usage_recorder
            .dashboard_account_billing(timezone.as_deref(), &window_key)
            .map_err(|err| AdminServiceError::InternalError(err.to_string()))
    }

    pub fn get_usage_dashboard_account_risk(
        &self,
        query: UsageAccountRiskQuery,
    ) -> Result<UsageAccountRiskResponse, AdminServiceError> {
        let config = self.runtime.config();
        let cost_config = UsageAccountRiskCostConfig {
            cost_floor_enabled: config.accounts.usage_projection_cost_floor_enabled,
            cost_floor_margin_percent: config.accounts.usage_projection_cost_floor_margin_percent,
        };
        self.usage_recorder
            .account_usage_risk(query, cost_config)
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

    /// 获取 上游 模型能力同步状态。
    pub fn get_model_capabilities(&self) -> ModelCapabilitiesStatus {
        self.model_capabilities.status()
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

    /// 兼容旧清空入口：提交一个有界、可审计的全量明细软删除任务。
    /// 若传入参数，则直接按参数执行；否则保持旧的全量清空语义。
    pub fn clear_usage_records(
        &self,
        request: Option<UsageCleanupRequest>,
    ) -> Result<UsageCleanupStatusResponse, AdminServiceError> {
        self.start_usage_cleanup(request.unwrap_or(UsageCleanupRequest {
            mode: UsageCleanupMode::SoftDelete,
            include_summary: false,
            older_than_days: Some(0),
            cutoff_before: None,
            batch_size: Some(USAGE_CLEANUP_DEFAULT_BATCH_SIZE),
            max_batches: None,
            max_rows: None,
            pause_ms_between_batches: None,
        }))
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
            include_summary: plan.include_summary,
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
        let now = Utc::now();
        let job_id = format!("usage-cleanup-{}", uuid::Uuid::new_v4().simple());
        let inserted = block_on_admin_store({
            let store = store.clone();
            let job_id = job_id.clone();
            let plan = plan.clone();
            async move {
                store
                    .create_cleanup_job(NewUsageCleanupJob {
                        job_id: &job_id,
                        mode: usage_cleanup_mode_value(plan.mode),
                        include_summary: plan.include_summary,
                        cutoff_at: plan.cutoff,
                        batch_size: plan.batch_size,
                        max_batches: plan.max_batches,
                        max_rows: plan.max_rows,
                        pause_ms_between_batches: plan.pause_ms_between_batches,
                    })
                    .await
            }
        })
        .map_err(|err| AdminServiceError::InternalError(err.to_string()))?;
        if !inserted {
            return Err(AdminServiceError::Conflict(
                "已有 usage 清理任务正在排队或运行".to_string(),
            ));
        }

        let cancel = Arc::new(AtomicBool::new(false));
        let status = UsageCleanupStatusResponse {
            job_id: Some(job_id.clone()),
            status: UsageCleanupJobStatus::Queued,
            phase: "postgres".to_string(),
            mode: Some(plan.mode),
            include_summary: plan.include_summary,
            cutoff_at: Some(plan.cutoff.to_rfc3339()),
            batch_size: plan.batch_size,
            max_batches: plan.max_batches,
            max_rows: plan.max_rows,
            pause_ms_between_batches: plan.pause_ms_between_batches,
            matched_rows: None,
            remaining_rows: None,
            processed_rows: 0,
            last_batch_rows: 0,
            batches: 0,
            redis_deleted_keys: 0,
            redis_delete_commands: 0,
            redis_max_command_keys: 0,
            redis_scan_passes: 0,
            redis_used_del_fallback: false,
            redis_pass_limit_reached: false,
            cancel_requested: false,
            stop_reason: None,
            started_at: Some(now.to_rfc3339()),
            updated_at: Some(now.to_rfc3339()),
            finished_at: None,
            last_error: None,
        };

        {
            let mut runtime = self.usage_cleanup.lock();
            runtime.status = status.clone();
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
                "includeSummary": plan.include_summary,
                "cutoffAt": plan.cutoff.to_rfc3339(),
                "batchSize": plan.batch_size,
                "maxBatches": plan.max_batches,
                "maxRows": plan.max_rows,
                "submissionMode": "bounded_background_job",
            }),
        );
        self.spawn_usage_cleanup_job(job_id, cancel);
        Ok(status)
    }

    pub fn get_usage_cleanup_status(&self) -> UsageCleanupStatusResponse {
        let store = PostgresUsageStore::new(self.postgres_store.clone());
        match block_on_admin_store(async move { store.latest_cleanup_job().await }) {
            Ok(Some(job)) => {
                let status = usage_cleanup_status_from_row(&job);
                self.usage_cleanup.lock().status = status.clone();
                status
            }
            Ok(None) => self.usage_cleanup.lock().status.clone(),
            Err(err) => {
                tracing::warn!("读取 usage cleanup 持久状态失败，回退进程内状态: {}", err);
                self.usage_cleanup.lock().status.clone()
            }
        }
    }

    pub fn cancel_usage_cleanup(&self) -> Result<UsageCleanupStatusResponse, AdminServiceError> {
        let current = self.get_usage_cleanup_status();
        let Some(job_id) = current.job_id.clone() else {
            return Ok(current);
        };
        {
            let mut runtime = self.usage_cleanup.lock();
            if let Some(cancel) = &runtime.cancel {
                cancel.store(true, Ordering::Release);
            }
            runtime.status.cancel_requested = true;
            runtime.status.updated_at = Some(Utc::now().to_rfc3339());
        }
        let store = PostgresUsageStore::new(self.postgres_store.clone());
        let persisted = block_on_admin_store({
            let job_id = job_id.clone();
            async move { store.request_cleanup_cancel(&job_id).await }
        })
        .map_err(|err| AdminServiceError::InternalError(err.to_string()))?;
        let status = persisted
            .as_ref()
            .map(usage_cleanup_status_from_row)
            .unwrap_or_else(|| self.get_usage_cleanup_status());
        self.usage_cleanup.lock().status = status.clone();
        self.audit(
            "cancel_usage_cleanup",
            "usage_record",
            Some(job_id.clone()),
            persisted.is_some(),
            persisted
                .is_none()
                .then(|| "任务已经结束或不存在".to_string()),
            json!({ "jobId": job_id }),
        );
        Ok(status)
    }

    pub fn resume_usage_cleanup(
        &self,
        request: UsageCleanupResumeRequest,
    ) -> Result<UsageCleanupStatusResponse, AdminServiceError> {
        let job_id = request.job_id.trim();
        if job_id.is_empty() {
            return Err(AdminServiceError::InvalidCredential(
                "jobId 不能为空".to_string(),
            ));
        }
        let store = PostgresUsageStore::new(self.postgres_store.clone());
        let resumed = block_on_admin_store({
            let job_id = job_id.to_string();
            async move { store.requeue_cleanup_job(&job_id).await }
        })
        .map_err(|err| AdminServiceError::InternalError(err.to_string()))?
        .ok_or_else(|| {
            AdminServiceError::Conflict(
                "只有已暂停、失败或已取消的 usage 清理任务可以恢复".to_string(),
            )
        })?;
        let status = usage_cleanup_status_from_row(&resumed);
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut runtime = self.usage_cleanup.lock();
            runtime.status = status.clone();
            runtime.cancel = Some(cancel.clone());
        }
        self.audit(
            "resume_usage_cleanup",
            "usage_record",
            Some(job_id.to_string()),
            true,
            None,
            json!({ "jobId": job_id, "phase": resumed.phase }),
        );
        self.spawn_usage_cleanup_job(job_id.to_string(), cancel);
        Ok(status)
    }

    fn spawn_usage_cleanup_job(&self, job_id: String, cancel: Arc<AtomicBool>) {
        let store = PostgresUsageStore::new(self.postgres_store.clone());
        let postgres = self.postgres_store.clone();
        let observability_redis = self.observability_redis_store.clone();
        let usage_recorder = self.usage_recorder.clone();
        let admin_cache_shadow = self.admin_cache_shadow.clone();
        let cleanup_state = self.usage_cleanup.clone();
        tokio::spawn(async move {
            run_usage_cleanup_job(
                job_id,
                store,
                postgres,
                observability_redis,
                usage_recorder,
                admin_cache_shadow,
                cleanup_state,
                cancel,
            )
            .await;
        });
    }

    fn resume_persisted_usage_cleanup(&self) {
        let store = PostgresUsageStore::new(self.postgres_store.clone());
        let postgres = self.postgres_store.clone();
        let observability_redis = self.observability_redis_store.clone();
        let usage_recorder = self.usage_recorder.clone();
        let admin_cache_shadow = self.admin_cache_shadow.clone();
        let cleanup_state = Arc::downgrade(&self.usage_cleanup);
        tokio::spawn(async move {
            let mut poll_delay = StdDuration::from_secs(USAGE_CLEANUP_RECOVERY_POLL_MIN_SECS);
            loop {
                let Some(cleanup_state) = cleanup_state.upgrade() else {
                    return;
                };
                match store.recoverable_cleanup_job().await {
                    Ok(Some(job)) => {
                        tracing::info!(
                            job_id = %job.job_id,
                            status = %job.status,
                            lease_until = ?job.lease_until,
                            "usage cleanup supervisor 发现可领取任务"
                        );
                        let cancel = Arc::new(AtomicBool::new(job.cancel_requested));
                        {
                            let mut runtime = cleanup_state.lock();
                            runtime.status = usage_cleanup_status_from_row(&job);
                            runtime.cancel = Some(cancel.clone());
                        }
                        run_usage_cleanup_job(
                            job.job_id,
                            store.clone(),
                            postgres.clone(),
                            observability_redis.clone(),
                            usage_recorder.clone(),
                            admin_cache_shadow.clone(),
                            cleanup_state,
                            cancel,
                        )
                        .await;
                        poll_delay = StdDuration::from_secs(USAGE_CLEANUP_RECOVERY_POLL_MIN_SECS);
                    }
                    Ok(None) => {
                        poll_delay = (poll_delay * 2)
                            .min(StdDuration::from_secs(USAGE_CLEANUP_RECOVERY_POLL_MAX_SECS));
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "读取待恢复 usage cleanup job 失败");
                        poll_delay = (poll_delay * 2)
                            .min(StdDuration::from_secs(USAGE_CLEANUP_RECOVERY_POLL_MAX_SECS));
                    }
                }
                tokio::time::sleep(poll_delay).await;
            }
        });
    }

    pub fn get_audit_logs_filtered(
        &self,
        page: usize,
        limit: usize,
        filter: AdminAuditLogFilter,
    ) -> AdminAuditLogPage {
        let store = self.postgres_store.clone();
        block_on_admin_store(async move {
            store
                .query_admin_audit_logs_filtered(page, limit, &filter)
                .await
        })
        .unwrap_or_else(|err| {
            tracing::warn!("查询 Admin 审计日志失败: {}", err);
            AdminAuditLogPage {
                page: page.max(1),
                limit: if limit == 0 { 20 } else { limit.min(1000) },
                has_next: false,
                records: Vec::new(),
            }
        })
    }

    pub fn get_account_event_summary(
        &self,
        hours: u32,
        account_source: Option<String>,
    ) -> AccountEventSummaryResponse {
        let since = chrono::Utc::now() - chrono::Duration::hours(i64::from(hours));
        let source = account_source
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let store = self.postgres_store.clone();
        let query_source = source.clone();
        let items = block_on_admin_store(async move {
            store
                .summarize_account_events(since, query_source.as_deref())
                .await
        })
        .unwrap_or_else(|err| {
            tracing::warn!("统计账号异动失败: {}", err);
            Vec::new()
        });
        AccountEventSummaryResponse {
            hours,
            since: since.to_rfc3339(),
            account_source: source,
            items,
        }
    }

    /// 获取运行时全局配置。
    pub fn get_runtime_config(&self) -> RuntimeConfigResponse {
        let config = self.runtime.config();
        RuntimeConfigResponse {
            proxy_url: config.proxy_url.clone(),
            proxy_username: config.proxy_username.clone(),
            proxy_password: config.proxy_password.clone(),
            request_admission: config.request_admission.clone(),
            stream_pre_output_hold_secs: config.stream_pre_output_hold_secs.clone(),
            inference_upstream_max_attempts: config.inference_upstream_max_attempts.clone(),
            prompt_injection: config.prompt_injection.clone(),
            route_features: config.route_features.clone(),
            prompt_cache_target_read_ratio: config.prompt_cache_target_read_ratio.clone(),
            prompt_cache_token_scale: config.prompt_cache_token_scale.clone(),
            prompt_cache_max_simulated_input_tokens: config
                .prompt_cache_max_simulated_input_tokens
                .clone(),
            prompt_cache_cap_jitter_min_tokens: config.prompt_cache_cap_jitter_min_tokens.clone(),
            prompt_cache_cap_jitter_max_tokens: config.prompt_cache_cap_jitter_max_tokens.clone(),
            prompt_cache_scale_min_input_tokens: config.prompt_cache_scale_min_input_tokens.clone(),
            prompt_cache_creation_control: config.prompt_cache_creation_control.clone(),
            prompt_cache_max_entries_per_account: config
                .prompt_cache_max_entries_per_account
                .clone(),
            prompt_cache_max_entries_global: config.prompt_cache_max_entries_global.clone(),
            prompt_cache_entry_ttl_secs: config.prompt_cache_entry_ttl_secs.clone(),
            prompt_cache_estimated_bytes_limit: config.prompt_cache_estimated_bytes_limit.clone(),
            reported_usage: config.reported_usage.clone(),
            cache_policy: config.cache_policy.clone(),
            accounts: config.accounts.clone(),
            high_cache_threshold: config.high_cache_threshold.clone(),
            compat_profile: config.compat_profile.clone(),
            model_resolution_mode: config.model_resolution_mode.clone(),
            model_mapping: config.model_mapping.clone(),
            audit_log_retention_enabled: config.audit_log_retention_enabled.clone(),
            audit_log_retention_days: config.audit_log_retention_days.clone(),
            tls_backend: config.tls_backend,
            usage_record_limit: config.usage_record_limit,
        }
    }

    pub fn prompt_injection_catalog(&self) -> super::types::PromptInjectionCatalogResponse {
        use crate::model::prompt_injection::{MAX_TEXT_CHARS, MAX_TOTAL_TEXT_CHARS, catalog};
        super::types::PromptInjectionCatalogResponse {
            items: catalog::entries(),
            fixed: catalog::fixed_injections(),
            max_text_chars: MAX_TEXT_CHARS,
            max_total_text_chars: MAX_TOTAL_TEXT_CHARS,
        }
    }

    /// 用当前运行配置（或请求里的草稿）预览注入结果。草稿只用于这次调用，不会保存。
    pub fn preview_prompt_injection(
        &self,
        body: super::types::PromptInjectionPreviewBody,
    ) -> Result<crate::anthropic::PromptInjectionPreviewResponse, AdminServiceError> {
        let mut config = self.runtime.config();
        if let Some(draft) = body.draft {
            if let Some(prompt_injection) = draft.prompt_injection {
                let prompt_injection = prompt_injection.normalized();
                prompt_injection
                    .validate()
                    .map_err(AdminServiceError::InvalidCredential)?;
                config.prompt_injection = prompt_injection;
            }
            if let Some(route_features) = draft.route_features {
                config.route_features = route_features.normalized();
            }
        }
        crate::anthropic::preview_prompt_injection(&config, body.request)
            .map_err(AdminServiceError::InvalidCredential)
    }

    /// 更新运行时全局配置，并写入 PgSQL。
    pub fn update_runtime_config(
        &self,
        req: UpdateRuntimeConfigRequest,
    ) -> Result<RuntimeConfigResponse, AdminServiceError> {
        let current_config = self.runtime.config();
        let mut candidate = current_config.clone();
        {
            let config = &mut candidate;
            if let Some(value) = req.request_admission {
                config.request_admission = value;
            }
            if let Some(value) = req.stream_pre_output_hold_secs {
                config.stream_pre_output_hold_secs = value;
            }
            if let Some(value) = req.inference_upstream_max_attempts {
                config.inference_upstream_max_attempts = value;
            }
            if let Some(value) = req.prompt_injection {
                config.prompt_injection = value;
            }
            if let Some(value) = req.route_features {
                config.route_features = value;
            }
            if let Some(value) = req.prompt_cache_target_read_ratio {
                config.prompt_cache_target_read_ratio = value;
            }
            if let Some(value) = req.prompt_cache_token_scale {
                config.prompt_cache_token_scale = value;
            }
            if let Some(value) = req.prompt_cache_max_simulated_input_tokens {
                config.prompt_cache_max_simulated_input_tokens = value;
            }
            if let Some(value) = req.prompt_cache_cap_jitter_min_tokens {
                config.prompt_cache_cap_jitter_min_tokens = value;
            }
            if let Some(value) = req.prompt_cache_cap_jitter_max_tokens {
                config.prompt_cache_cap_jitter_max_tokens = value;
            }
            if let Some(value) = req.prompt_cache_scale_min_input_tokens {
                config.prompt_cache_scale_min_input_tokens = value;
            }
            if let Some(value) = req.prompt_cache_creation_control {
                config.prompt_cache_creation_control = value;
            }
            if let Some(value) = req.prompt_cache_max_entries_per_account {
                config.prompt_cache_max_entries_per_account = value;
            }
            if let Some(value) = req.prompt_cache_max_entries_global {
                config.prompt_cache_max_entries_global = value;
            }
            if let Some(value) = req.prompt_cache_entry_ttl_secs {
                config.prompt_cache_entry_ttl_secs = value;
            }
            if let Some(value) = req.prompt_cache_estimated_bytes_limit {
                config.prompt_cache_estimated_bytes_limit = value;
            }
            if let Some(value) = req.reported_usage {
                config.reported_usage = value;
            }
            if let Some(value) = req.cache_policy {
                config.cache_policy = value;
            }
            if let Some(value) = req.accounts {
                config.accounts = value;
            }
            if let Some(value) = req.high_cache_threshold {
                config.high_cache_threshold = value;
            }
            if let Some(value) = req.compat_profile {
                config.compat_profile = value;
            }
            if let Some(value) = req.model_resolution_mode {
                config.model_resolution_mode = value;
            }
            if let Some(value) = req.model_mapping {
                config.model_mapping = value;
            }
            if let Some(value) = req.audit_log_retention_enabled {
                config.audit_log_retention_enabled = value;
            }
            if let Some(value) = req.audit_log_retention_days {
                config.audit_log_retention_days = value;
            }
        }
        if let Some(value) = req.proxy_url {
            candidate.proxy_url = value;
        }
        if let Some(value) = req.proxy_username {
            candidate.proxy_username = value;
        }
        if let Some(value) = req.proxy_password {
            candidate.proxy_password = value;
        }
        if let Some(value) = req.tls_backend {
            candidate.tls_backend = value;
        }
        if let Some(value) = req.usage_record_limit {
            candidate.usage_record_limit = value;
        }
        normalize_and_validate_runtime_config(&mut candidate)
            .map_err(AdminServiceError::InvalidCredential)?;
        self.runtime
            .update(|config| *config = candidate)
            .map_err(|e| AdminServiceError::InternalError(e.to_string()))?;
        let updated = self.runtime.config();
        self.request_admission
            .update_config(updated.request_admission);
        self.refresh_request_api_key_runtime(&updated);
        self.account_manager.invalidate_account_policy_state();
        self.invalidate_admin_cache_pattern("admin_cache:accounts:*");
        self.audit(
            "update_runtime_config",
            "runtime_config",
            Some("default".to_string()),
            true,
            None,
            Self::runtime_config_audit_detail(&current_config, &updated),
        );
        Ok(self.get_runtime_config())
    }

    // ============ 错误分类 ============
}

fn normalize_and_validate_runtime_config(config: &mut Config) -> Result<(), String> {
    config.request_admission.validate()?;
    config.route_features.validate()?;
    config.prompt_injection.validate()?;
    config.reported_usage.validate()?;
    config.prompt_cache_creation_control.validate()?;
    config
        .cache_policy
        .validate(config.legacy_cache_route_policy_default())?;
    validate_accounts_config(&config.accounts)?;
    if !(1..=10).contains(&config.inference_upstream_max_attempts) {
        return Err("inferenceUpstreamMaxAttempts 必须在 1 到 10 之间".to_string());
    }
    if config.stream_pre_output_hold_secs > crate::model::config::STREAM_PRE_OUTPUT_HOLD_MAX_SECS {
        return Err(format!(
            "streamPreOutputHoldSecs 不能大于 {}",
            crate::model::config::STREAM_PRE_OUTPUT_HOLD_MAX_SECS
        ));
    }
    if !(crate::model::config::MIN_AUDIT_LOG_RETENTION_DAYS
        ..=crate::model::config::MAX_AUDIT_LOG_RETENTION_DAYS)
        .contains(&config.audit_log_retention_days)
    {
        return Err(format!(
            "auditLogRetentionDays 必须在 {} 到 {} 天之间",
            crate::model::config::MIN_AUDIT_LOG_RETENTION_DAYS,
            crate::model::config::MAX_AUDIT_LOG_RETENTION_DAYS
        ));
    }
    if config.high_cache_threshold < 0 {
        return Err("highCacheThreshold 不能小于 0".to_string());
    }
    if !(0.0..=0.99).contains(&config.prompt_cache_target_read_ratio)
        || !config.prompt_cache_target_read_ratio.is_finite()
    {
        return Err("promptCacheTargetReadRatio 必须在 0 到 0.99 之间".to_string());
    }
    if !(1.0..=3.0).contains(&config.prompt_cache_token_scale)
        || !config.prompt_cache_token_scale.is_finite()
    {
        return Err("promptCacheTokenScale 必须在 1 到 3 之间".to_string());
    }
    if config.prompt_cache_max_simulated_input_tokens < 0 {
        return Err("promptCacheMaxSimulatedInputTokens 不能小于 0".to_string());
    }
    if config.prompt_cache_cap_jitter_min_tokens < 0
        || config.prompt_cache_cap_jitter_max_tokens < 0
    {
        return Err(
            "promptCacheCapJitterMinTokens 和 promptCacheCapJitterMaxTokens 不能小于 0".to_string(),
        );
    }
    if config.prompt_cache_cap_jitter_min_tokens > config.prompt_cache_cap_jitter_max_tokens {
        return Err(
            "promptCacheCapJitterMinTokens 不能大于 promptCacheCapJitterMaxTokens".to_string(),
        );
    }
    if config.prompt_cache_scale_min_input_tokens < 0 {
        return Err("promptCacheScaleMinInputTokens 不能小于 0".to_string());
    }
    config.reported_usage.validate()?;
    config.prompt_cache_creation_control.validate()?;
    if config.prompt_cache_max_entries_global > 0
        && config.prompt_cache_max_entries_per_account > config.prompt_cache_max_entries_global
    {
        return Err(
            "promptCacheMaxEntriesPerAccount 不能大于 promptCacheMaxEntriesGlobal".to_string(),
        );
    }
    if config.prompt_cache_entry_ttl_secs == 0 {
        return Err("promptCacheEntryTtlSecs 必须大于 0".to_string());
    }

    config.routes.validate()?;
    let route_base = config.legacy_cache_route_policy_default();
    for route in &mut config.routes.routes {
        route.prefix = crate::model::routes::normalize_route_prefix(&route.prefix);
        route
            .cache_policy
            .validate(&format!("路由 {}", route.prefix), route_base.clone())?;
        if let Some(reported) = &route.reported_usage {
            reported.validate("路由上报策略")?;
        }
        crate::model::config::RouteFeaturesConfig {
            defaults: route.features.apply(config.route_features.defaults),
        }
        .validate()?;
    }
    config.cache_policy = config.cache_policy.normalized();
    config.request_admission = config.request_admission.normalized();
    config.prompt_injection = config.prompt_injection.clone().normalized();
    config.route_features = config.route_features.clone().normalized();
    config.reported_usage = config.reported_usage.normalized();
    config.prompt_cache_creation_control = config.prompt_cache_creation_control.normalized();
    config.model_mapping = config.model_mapping.clone().normalized();
    Ok(())
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
    if matches!(req.max_input_tokens, Some(value) if value <= 0) {
        return Err(AdminServiceError::InvalidCredential(
            "输入上限必须大于 0，或留空".to_string(),
        ));
    }
    if matches!(req.max_output_tokens, Some(value) if value <= 0) {
        return Err(AdminServiceError::InvalidCredential(
            "输出上限必须大于 0，或留空".to_string(),
        ));
    }
    Ok(ModelCapabilityItem {
        listed: req.listed,
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

fn validate_request_api_key_policies(
    keys: &[String],
    policies: &[RequestApiKeyPolicy],
) -> Result<(), String> {
    let mut seen = HashSet::new();
    if policies.len() > 10_000 {
        return Err("requestApiKeyPolicies 不能超过 10000 个".to_string());
    }
    for policy in policies {
        let key = policy.api_key.trim();
        if key.len() < 8 {
            return Err("managed request API Key 至少需要 8 个字符".to_string());
        }
        if !seen.insert(key.to_string()) {
            return Err("requestApiKeyPolicies 不能包含重复 Key".to_string());
        }
        if let Some(admission) = policy.request_admission {
            admission.validate()?;
        }
        validate_key_limits(&policy.usage_limits)?;
        if !keys.iter().any(|configured| configured == key) {
            return Err("managed request API Key 必须同时存在于 apiKey/apiKeys 列表".to_string());
        }
    }
    if !keys.iter().any(|key| {
        policies
            .iter()
            .find(|policy| policy.api_key.trim() == key)
            .map(|policy| policy.enabled)
            .unwrap_or(true)
    }) {
        return Err("至少需要保留一个启用的请求 API Key".to_string());
    }
    Ok(())
}

fn extract_account_test_response_text(body: &str) -> Option<String> {
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

fn normalize_usage_cleanup_request(
    request: UsageCleanupRequest,
) -> Result<UsageCleanupPlan, AdminServiceError> {
    let now = Utc::now();
    let requested_cutoff = if let Some(value) = request
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
        let days = request
            .older_than_days
            .unwrap_or(USAGE_CLEANUP_DEFAULT_OLDER_THAN_DAYS);
        if days > 3650 {
            return Err(AdminServiceError::InvalidCredential(
                "olderThanDays 不能超过 3650".to_string(),
            ));
        }
        now - ChronoDuration::days(days as i64)
    };

    if requested_cutoff > now {
        return Err(AdminServiceError::InvalidCredential(
            "cutoffBefore 必须早于当前时间".to_string(),
        ));
    }

    // The explicit summary option is deliberately a simple, all-history action.
    // It is not combined with a partial date range or a row cap, which avoids
    // leaving the operator with a partially rebuilt set of aggregates.
    let cutoff = if request.include_summary {
        now
    } else {
        requested_cutoff
    };

    let batch_size = request
        .batch_size
        .unwrap_or(USAGE_CLEANUP_DEFAULT_BATCH_SIZE);
    if batch_size == 0 || batch_size > USAGE_CLEANUP_MAX_BATCH_SIZE {
        return Err(AdminServiceError::InvalidCredential(format!(
            "每批数量 batchSize 必须在 1..={} 之间",
            USAGE_CLEANUP_MAX_BATCH_SIZE
        )));
    }

    let max_rows = request
        .max_rows
        .filter(|value| *value > 0)
        .map(|value| {
            if value > USAGE_CLEANUP_MAX_ROWS {
                Err(AdminServiceError::InvalidCredential(format!(
                    "maxRows 不能超过 {}",
                    USAGE_CLEANUP_MAX_ROWS
                )))
            } else {
                Ok(value)
            }
        })
        .transpose()?;

    let max_rows = if request.include_summary {
        None
    } else {
        max_rows
    };

    let max_batches = request
        .max_batches
        .filter(|value| *value > 0)
        .unwrap_or(USAGE_CLEANUP_DEFAULT_MAX_BATCHES);
    if max_batches > USAGE_CLEANUP_MAX_BATCHES {
        return Err(AdminServiceError::InvalidCredential(
            "maxBatches 必须在 1..=10000 之间".to_string(),
        ));
    }

    let pause_ms_between_batches = request.pause_ms_between_batches.unwrap_or(10);
    if pause_ms_between_batches > 10_000 {
        return Err(AdminServiceError::InvalidCredential(
            "pauseMsBetweenBatches 不能超过 10000".to_string(),
        ));
    }

    let mode = if request.include_summary {
        UsageCleanupMode::SoftDelete
    } else {
        request.mode
    };

    Ok(UsageCleanupPlan {
        mode,
        include_summary: request.include_summary,
        cutoff,
        batch_size,
        max_batches,
        max_rows,
        pause_ms_between_batches,
    })
}

fn usage_cleanup_mode_value(mode: UsageCleanupMode) -> &'static str {
    match mode {
        UsageCleanupMode::SoftDelete => "soft_delete",
        UsageCleanupMode::HardDelete => "hard_delete",
    }
}

fn usage_cleanup_mode_from_value(value: &str) -> Option<UsageCleanupMode> {
    match value {
        "soft_delete" => Some(UsageCleanupMode::SoftDelete),
        "hard_delete" => Some(UsageCleanupMode::HardDelete),
        _ => None,
    }
}

fn usage_cleanup_status_value(status: UsageCleanupJobStatus) -> &'static str {
    match status {
        UsageCleanupJobStatus::Idle => "idle",
        UsageCleanupJobStatus::Queued => "queued",
        UsageCleanupJobStatus::Running => "running",
        UsageCleanupJobStatus::Paused => "paused",
        UsageCleanupJobStatus::Completed => "completed",
        UsageCleanupJobStatus::Cancelled => "cancelled",
        UsageCleanupJobStatus::Failed => "failed",
    }
}

fn usage_cleanup_status_from_value(value: &str) -> UsageCleanupJobStatus {
    match value {
        "queued" => UsageCleanupJobStatus::Queued,
        "running" => UsageCleanupJobStatus::Running,
        "paused" => UsageCleanupJobStatus::Paused,
        "completed" => UsageCleanupJobStatus::Completed,
        "cancelled" => UsageCleanupJobStatus::Cancelled,
        "failed" => UsageCleanupJobStatus::Failed,
        _ => UsageCleanupJobStatus::Failed,
    }
}

fn usage_cleanup_status_from_row(job: &UsageCleanupJobRow) -> UsageCleanupStatusResponse {
    UsageCleanupStatusResponse {
        job_id: Some(job.job_id.clone()),
        status: usage_cleanup_status_from_value(&job.status),
        phase: job.phase.clone(),
        mode: usage_cleanup_mode_from_value(&job.mode),
        include_summary: job.include_summary,
        cutoff_at: Some(job.cutoff_at.to_rfc3339()),
        batch_size: job.batch_size,
        max_batches: job.max_batches,
        max_rows: job.max_rows,
        pause_ms_between_batches: job.pause_ms_between_batches,
        matched_rows: job.matched_rows,
        remaining_rows: job.remaining_rows,
        processed_rows: job.processed_rows,
        last_batch_rows: job.last_batch_rows,
        batches: job.batches,
        redis_deleted_keys: job.redis_deleted_keys,
        redis_delete_commands: job.redis_delete_commands,
        redis_max_command_keys: job.redis_max_command_keys,
        redis_scan_passes: job.redis_scan_passes,
        redis_used_del_fallback: job.redis_used_del_fallback,
        redis_pass_limit_reached: job.redis_pass_limit_reached,
        cancel_requested: job.cancel_requested,
        stop_reason: job.stop_reason.clone(),
        started_at: Some(job.started_at.to_rfc3339()),
        updated_at: Some(job.updated_at.to_rfc3339()),
        finished_at: job.finished_at.map(|value| value.to_rfc3339()),
        last_error: job.last_error.clone(),
    }
}

fn usage_cleanup_run_batch_limit_reached(
    total_batches: usize,
    run_start_batches: usize,
    max_batches_per_run: usize,
) -> bool {
    total_batches.saturating_sub(run_start_batches) >= max_batches_per_run
}

fn usage_cleanup_run_rows_remaining(
    total_rows: u64,
    run_start_rows: u64,
    max_rows_per_run: u64,
) -> u64 {
    max_rows_per_run.saturating_sub(total_rows.saturating_sub(run_start_rows))
}

fn usage_cleanup_lock_contention_backoff(retry: usize) -> StdDuration {
    let multiplier = 1_u64 << retry.saturating_sub(1).min(8);
    StdDuration::from_millis(
        USAGE_CLEANUP_LOCK_CONTENTION_RETRY_MS
            .saturating_mul(multiplier)
            .min(USAGE_CLEANUP_LOCK_CONTENTION_MAX_RETRY_MS),
    )
}

async fn usage_cleanup_has_remaining(
    store: &PostgresUsageStore,
    plan: &UsageCleanupPlan,
) -> anyhow::Result<bool> {
    match plan.mode {
        UsageCleanupMode::SoftDelete => store.soft_delete_cleanup_has_remaining(plan.cutoff).await,
        UsageCleanupMode::HardDelete => store.hard_delete_cleanup_has_remaining(plan.cutoff).await,
    }
}

#[derive(Debug, Clone, Copy)]
struct UsageCleanupHeartbeatPolicy {
    lease_secs: u64,
    attempt_timeout: StdDuration,
    max_attempts: usize,
    retry_delay: StdDuration,
}

impl Default for UsageCleanupHeartbeatPolicy {
    fn default() -> Self {
        Self {
            lease_secs: USAGE_CLEANUP_LEASE_SECS,
            attempt_timeout: StdDuration::from_millis(USAGE_CLEANUP_HEARTBEAT_ATTEMPT_TIMEOUT_MS),
            max_attempts: USAGE_CLEANUP_HEARTBEAT_MAX_ATTEMPTS,
            retry_delay: StdDuration::from_millis(USAGE_CLEANUP_HEARTBEAT_RETRY_MS),
        }
    }
}

async fn renew_usage_cleanup_lease_with_retry(
    store: &PostgresUsageStore,
    job_id: &str,
    worker_id: &str,
    policy: UsageCleanupHeartbeatPolicy,
) -> anyhow::Result<Option<bool>> {
    let max_attempts = policy.max_attempts.max(1);
    let mut last_error = None;
    for attempt in 1..=max_attempts {
        match tokio::time::timeout(
            policy.attempt_timeout,
            store.renew_cleanup_job_lease(job_id, worker_id, policy.lease_secs),
        )
        .await
        {
            Ok(Ok(result)) => return Ok(result),
            Ok(Err(err)) => last_error = Some(err.to_string()),
            Err(_) => {
                last_error = Some(format!(
                    "lease renewal attempt timed out after {} ms",
                    policy.attempt_timeout.as_millis()
                ));
            }
        }
        if attempt < max_attempts {
            let multiplier = 1_u32 << (attempt.saturating_sub(1).min(8) as u32);
            tokio::time::sleep(policy.retry_delay.saturating_mul(multiplier)).await;
        }
    }
    anyhow::bail!(
        "usage cleanup lease renewal failed after {max_attempts} attempts: {}",
        last_error.unwrap_or_else(|| "unknown renewal failure".to_string())
    )
}

struct UsageCleanupLeaseHeartbeat {
    task: tokio::task::JoinHandle<()>,
}

impl Drop for UsageCleanupLeaseHeartbeat {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn spawn_usage_cleanup_lease_heartbeat(
    store: PostgresUsageStore,
    job_id: String,
    worker_id: String,
    cancel: Arc<AtomicBool>,
    lease_lost: Arc<AtomicBool>,
) -> UsageCleanupLeaseHeartbeat {
    let interval = StdDuration::from_secs((USAGE_CLEANUP_LEASE_SECS / 3).max(1));
    let policy = UsageCleanupHeartbeatPolicy::default();
    let task = tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            match renew_usage_cleanup_lease_with_retry(&store, &job_id, &worker_id, policy).await {
                Ok(Some(cancel_requested)) => {
                    if cancel_requested {
                        cancel.store(true, Ordering::Release);
                    }
                }
                Ok(None) => {
                    lease_lost.store(true, Ordering::Release);
                    cancel.store(true, Ordering::Release);
                    tracing::warn!(job_id, "usage cleanup heartbeat 检测到 lease 已丢失");
                    return;
                }
                Err(err) => {
                    lease_lost.store(true, Ordering::Release);
                    cancel.store(true, Ordering::Release);
                    tracing::warn!(job_id, error = %err, "usage cleanup heartbeat 续租失败");
                    return;
                }
            }
        }
    });
    UsageCleanupLeaseHeartbeat { task }
}

#[allow(clippy::too_many_arguments)]
async fn run_usage_cleanup_job(
    job_id: String,
    store: PostgresUsageStore,
    postgres: Arc<PostgresStore>,
    observability_redis: Option<Arc<RedisStore>>,
    usage_recorder: Arc<UsageRecorder>,
    admin_cache_shadow: Arc<Mutex<HashMap<String, AdminCacheEntry>>>,
    cleanup_state: Arc<Mutex<UsageCleanupRuntime>>,
    cancel: Arc<AtomicBool>,
) {
    let worker_id = format!("usage-cleanup-worker-{}", uuid::Uuid::new_v4().simple());
    let job = match store
        .claim_cleanup_job(&job_id, &worker_id, USAGE_CLEANUP_LEASE_SECS)
        .await
    {
        Ok(Some(job)) => job,
        Ok(None) => {
            match store.cleanup_job(&job_id).await {
                Ok(Some(job)) if !matches!(job.status.as_str(), "queued" | "running") => {
                    let mut runtime = cleanup_state.lock();
                    runtime.status = usage_cleanup_status_from_row(&job);
                    runtime.cancel = None;
                }
                Ok(Some(_)) => {
                    tracing::debug!(job_id, "usage cleanup job 已由其他 worker 领取");
                }
                Ok(None) => tracing::warn!(job_id, "usage cleanup job 在领取前已不存在"),
                Err(err) => tracing::warn!(job_id, error = %err, "读取 usage cleanup job 失败"),
            }
            return;
        }
        Err(err) => {
            tracing::warn!(job_id, error = %err, "领取 usage cleanup job 失败，交由 supervisor 稍后重试");
            return;
        }
    };

    cancel.store(job.cancel_requested, Ordering::Release);
    {
        let mut runtime = cleanup_state.lock();
        runtime.status = usage_cleanup_status_from_row(&job);
        runtime.cancel = Some(cancel.clone());
    }

    let mut processed_rows = job.processed_rows;
    // Like the batch limit, the row limit is a per-run budget: a resumed job gets a fresh one
    // instead of stopping at once because the rows of earlier runs already used it up.
    let run_start_rows = processed_rows;
    let mut batches = job.batches;
    let run_start_batches = batches;
    let mut phase = job.phase.clone();
    let mut redis_stats = RedisPatternDeleteStats {
        deleted_keys: job.redis_deleted_keys,
        scan_calls: 0,
        delete_commands: job.redis_delete_commands,
        max_command_keys: job.redis_max_command_keys,
        scan_passes: job.redis_scan_passes,
        used_del_fallback: job.redis_used_del_fallback,
        cancelled: false,
        pass_limit_reached: job.redis_pass_limit_reached,
    };
    let lease_lost = Arc::new(AtomicBool::new(false));
    let _heartbeat = spawn_usage_cleanup_lease_heartbeat(
        store.clone(),
        job_id.clone(),
        worker_id.clone(),
        cancel.clone(),
        lease_lost.clone(),
    );

    let Some(mode) = usage_cleanup_mode_from_value(&job.mode) else {
        let error = format!("invalid persisted cleanup mode: {}", job.mode);
        let _ = persist_usage_cleanup_progress(
            &store,
            &cleanup_state,
            &job_id,
            &worker_id,
            UsageCleanupJobStatus::Failed,
            &phase,
            processed_rows,
            0,
            batches,
            job.remaining_rows,
            Some("invalid_persisted_mode"),
            Some(&error),
            &redis_stats,
            true,
        )
        .await;
        tracing::error!(job_id, mode = %job.mode, "持久化 usage cleanup mode 非法");
        return;
    };
    let plan = UsageCleanupPlan {
        mode,
        include_summary: job.include_summary,
        cutoff: job.cutoff_at,
        batch_size: job.batch_size,
        max_batches: job.max_batches,
        max_rows: job.max_rows,
        pause_ms_between_batches: job.pause_ms_between_batches,
    };
    let cleanup_watermark = match plan.mode {
        UsageCleanupMode::SoftDelete => store
            .advance_soft_delete_cleanup_watermark(plan.cutoff)
            .await
            .map(Some),
        UsageCleanupMode::HardDelete => store.soft_delete_cleanup_watermark().await,
    };
    let cleanup_watermark = match cleanup_watermark {
        Ok(cutoff) => cutoff,
        Err(err) => {
            let error = err.to_string();
            let _ = persist_usage_cleanup_progress(
                &store,
                &cleanup_state,
                &job_id,
                &worker_id,
                UsageCleanupJobStatus::Failed,
                &phase,
                processed_rows,
                0,
                batches,
                job.remaining_rows,
                Some("postgres_cleanup_watermark_failed"),
                Some(&error),
                &redis_stats,
                true,
            )
            .await;
            return;
        }
    };
    if let Some(cutoff) = cleanup_watermark {
        if let Err(err) = usage_recorder.advance_cleanup_watermark(cutoff).await {
            let error = err.to_string();
            let _ = persist_usage_cleanup_progress(
                &store,
                &cleanup_state,
                &job_id,
                &worker_id,
                UsageCleanupJobStatus::Failed,
                &phase,
                processed_rows,
                0,
                batches,
                job.remaining_rows,
                Some("redis_cleanup_watermark_failed"),
                Some(&error),
                &redis_stats,
                true,
            )
            .await;
            return;
        }
    }
    if let Some(redis) = observability_redis.as_ref() {
        if let Err(err) = redis.invalidate_usage_derived_cache().await {
            let error = err.to_string();
            let _ = persist_usage_cleanup_progress(
                &store,
                &cleanup_state,
                &job_id,
                &worker_id,
                UsageCleanupJobStatus::Failed,
                &phase,
                processed_rows,
                0,
                batches,
                job.remaining_rows,
                Some("redis_usage_cache_invalidation_failed"),
                Some(&error),
                &redis_stats,
                true,
            )
            .await;
            return;
        }
    }
    let mut final_status = UsageCleanupJobStatus::Completed;
    let mut stop_reason = Some("no_more_rows".to_string());
    let mut last_error = None;
    let mut remaining_rows = None;
    let mut postgres_complete = phase != "postgres";

    if phase == "postgres" {
        let mut lock_contention_retries = 0_usize;
        loop {
            if cancel.load(Ordering::Acquire) {
                final_status = UsageCleanupJobStatus::Cancelled;
                stop_reason = Some("cancel_requested".to_string());
                break;
            }
            if usage_cleanup_run_batch_limit_reached(batches, run_start_batches, plan.max_batches) {
                match usage_cleanup_has_remaining(&store, &plan).await {
                    Ok(false) => {
                        remaining_rows = Some(0);
                        postgres_complete = true;
                    }
                    Ok(true) => {
                        final_status = UsageCleanupJobStatus::Paused;
                        stop_reason = Some("max_batches_reached".to_string());
                    }
                    Err(err) => {
                        final_status = UsageCleanupJobStatus::Failed;
                        stop_reason = Some("postgres_completion_probe_failed".to_string());
                        last_error = Some(err.to_string());
                    }
                }
                break;
            }

            if let Some(max_rows) = plan.max_rows {
                let remaining_budget =
                    usage_cleanup_run_rows_remaining(processed_rows, run_start_rows, max_rows);
                if remaining_budget == 0 {
                    match usage_cleanup_has_remaining(&store, &plan).await {
                        Ok(false) => {
                            remaining_rows = Some(0);
                            postgres_complete = true;
                        }
                        Ok(true) => {
                            final_status = UsageCleanupJobStatus::Paused;
                            stop_reason = Some("max_rows_reached".to_string());
                        }
                        Err(err) => {
                            final_status = UsageCleanupJobStatus::Failed;
                            stop_reason = Some("postgres_completion_probe_failed".to_string());
                            last_error = Some(err.to_string());
                        }
                    }
                    break;
                }
            }

            let batch_limit = plan
                .max_rows
                .map(|max_rows| {
                    usage_cleanup_run_rows_remaining(processed_rows, run_start_rows, max_rows)
                        .min(plan.batch_size as u64) as usize
                })
                .filter(|limit| *limit > 0)
                .unwrap_or(plan.batch_size);
            let batch_result = match plan.mode {
                UsageCleanupMode::SoftDelete => {
                    store
                        .soft_delete_cleanup_batch_preserving_summary(plan.cutoff, batch_limit)
                        .await
                }
                UsageCleanupMode::HardDelete => {
                    store
                        .hard_delete_cleanup_batch_preserving_summary(plan.cutoff, batch_limit)
                        .await
                }
            };
            let batch_result = match batch_result {
                Ok(result) => result,
                Err(err) => {
                    final_status = UsageCleanupJobStatus::Failed;
                    stop_reason = Some("postgres_batch_failed".to_string());
                    last_error = Some(err.to_string());
                    break;
                }
            };
            let batch_rows = batch_result.processed_rows;
            if batch_rows == 0 {
                match batch_result.has_remaining {
                    Some(false) => {
                        remaining_rows = Some(0);
                        postgres_complete = true;
                        break;
                    }
                    Some(true) => {
                        lock_contention_retries = lock_contention_retries.saturating_add(1);
                        if lock_contention_retries >= USAGE_CLEANUP_LOCK_CONTENTION_MAX_RETRIES {
                            final_status = UsageCleanupJobStatus::Paused;
                            stop_reason = Some("postgres_lock_contention".to_string());
                            remaining_rows = None;
                            break;
                        }
                        tokio::time::sleep(usage_cleanup_lock_contention_backoff(
                            lock_contention_retries,
                        ))
                        .await;
                        continue;
                    }
                    None => {
                        final_status = UsageCleanupJobStatus::Failed;
                        stop_reason = Some("postgres_batch_contract_invalid".to_string());
                        last_error = Some(
                            "empty cleanup batch did not include a completion probe".to_string(),
                        );
                        break;
                    }
                }
            }

            lock_contention_retries = 0;
            batches = batches.saturating_add(1);
            processed_rows = processed_rows.saturating_add(batch_rows);
            let persist_result = persist_usage_cleanup_progress(
                &store,
                &cleanup_state,
                &job_id,
                &worker_id,
                UsageCleanupJobStatus::Running,
                &phase,
                processed_rows,
                batch_rows,
                batches,
                None,
                None,
                None,
                &redis_stats,
                false,
            )
            .await;
            match persist_result {
                Ok(Some(cancel_requested)) => {
                    if cancel_requested {
                        cancel.store(true, Ordering::Release);
                    }
                }
                Ok(None) => {
                    tracing::warn!(job_id, "usage cleanup lease 已丢失，当前 worker 停止");
                    return;
                }
                Err(err) => {
                    tracing::warn!(job_id, error = %err, "持久化 usage cleanup 批次进度失败");
                    return;
                }
            }
            if batch_result.has_remaining == Some(false) {
                remaining_rows = Some(0);
                postgres_complete = true;
                break;
            }
            tokio::task::yield_now().await;
            if plan.pause_ms_between_batches > 0 {
                tokio::time::sleep(StdDuration::from_millis(plan.pause_ms_between_batches)).await;
            }
        }
    }

    if plan.include_summary && postgres_complete && final_status == UsageCleanupJobStatus::Completed
    {
        if let Err(err) = store.reset_usage_rollups_from_active_records().await {
            final_status = UsageCleanupJobStatus::Failed;
            stop_reason = Some("postgres_summary_cleanup_failed".to_string());
            last_error = Some(err.to_string());
            postgres_complete = false;
        } else {
            usage_recorder.invalidate_derived_caches();
        }
    }

    let needs_cache_cleanup = processed_rows > 0 || phase != "postgres" || plan.include_summary;
    if needs_cache_cleanup {
        if matches!(
            plan.mode,
            UsageCleanupMode::SoftDelete | UsageCleanupMode::HardDelete
        ) {
            usage_recorder.remove_memory_records_before(plan.cutoff);
        }
        if cancel.load(Ordering::Acquire) && final_status == UsageCleanupJobStatus::Completed {
            final_status = UsageCleanupJobStatus::Cancelled;
            stop_reason = Some("cancel_requested".to_string());
            remaining_rows = None;
        }

        // The in-process Admin cache is always invalidated. Redis cleanup is optional and must
        // never fall back to the business scheduler Redis when the observability store is absent.
        invalidate_admin_cache_shadow(&admin_cache_shadow, "admin_cache:usage:*");

        if let Some(redis) = observability_redis.as_deref() {
            if phase == "postgres" || phase == "redis_admin_cache" {
                phase = "redis_admin_cache".to_string();
                match invalidate_usage_admin_cache_after_cleanup(
                    redis,
                    &admin_cache_shadow,
                    Some(cancel.as_ref()),
                )
                .await
                {
                    Ok(stats) => {
                        let cancelled = stats.cancelled;
                        let pass_limit_reached = stats.pass_limit_reached;
                        merge_redis_delete_stats(&mut redis_stats, stats);
                        if lease_lost.load(Ordering::Acquire) {
                            return;
                        }
                        if pass_limit_reached {
                            final_status = UsageCleanupJobStatus::Failed;
                            stop_reason = Some("redis_admin_cache_pass_limit_reached".to_string());
                            last_error = Some(
                                "Redis admin cache cleanup did not converge within the pass limit"
                                    .to_string(),
                            );
                        } else if cancelled {
                            final_status = UsageCleanupJobStatus::Cancelled;
                            stop_reason = Some("cancel_requested_during_redis_cleanup".to_string());
                            remaining_rows = None;
                        } else {
                            phase = "redis_snapshots".to_string();
                            match persist_usage_cleanup_progress(
                                &store,
                                &cleanup_state,
                                &job_id,
                                &worker_id,
                                UsageCleanupJobStatus::Running,
                                &phase,
                                processed_rows,
                                0,
                                batches,
                                remaining_rows,
                                stop_reason.as_deref(),
                                last_error.as_deref(),
                                &redis_stats,
                                false,
                            )
                            .await
                            {
                                Ok(Some(cancel_requested)) => {
                                    if cancel_requested {
                                        cancel.store(true, Ordering::Release);
                                    }
                                }
                                Ok(None) => return,
                                Err(err) => {
                                    tracing::warn!(job_id, error = %err, "持久化 Redis admin cache 清理进度失败");
                                    return;
                                }
                            }
                        }
                    }
                    Err(err) => {
                        final_status = UsageCleanupJobStatus::Failed;
                        stop_reason = Some("redis_admin_cache_cleanup_failed".to_string());
                        last_error = Some(err.to_string());
                    }
                }
            }

            let force_snapshot_index_only = redis_stats.pass_limit_reached
                || (final_status == UsageCleanupJobStatus::Failed && phase == "redis_admin_cache");
            let must_invalidate_snapshot_index = phase == "redis_snapshots"
                || (processed_rows > 0
                    && (cancel.load(Ordering::Acquire) || force_snapshot_index_only));
            if must_invalidate_snapshot_index {
                let interrupted_phase = phase.clone();
                let index_only_cancel = AtomicBool::new(true);
                let snapshot_cancel = if force_snapshot_index_only {
                    &index_only_cancel
                } else {
                    cancel.as_ref()
                };
                match redis
                    .clear_usage_record_snapshots_bounded(Some(snapshot_cancel))
                    .await
                {
                    Ok(stats) => {
                        let cancelled = stats.cancelled;
                        let pass_limit_reached = stats.pass_limit_reached;
                        merge_redis_delete_stats(&mut redis_stats, stats);
                        if lease_lost.load(Ordering::Acquire) {
                            return;
                        }
                        if pass_limit_reached {
                            final_status = UsageCleanupJobStatus::Failed;
                            stop_reason = Some("redis_snapshot_pass_limit_reached".to_string());
                            last_error = Some(
                            "Redis usage snapshot cleanup did not converge within the pass limit"
                                .to_string(),
                        );
                        } else if cancelled && force_snapshot_index_only {
                            phase = interrupted_phase;
                        } else if cancelled {
                            final_status = UsageCleanupJobStatus::Cancelled;
                            stop_reason = Some("cancel_requested_during_redis_cleanup".to_string());
                            remaining_rows = None;
                            phase = if postgres_complete {
                                interrupted_phase
                            } else {
                                "postgres".to_string()
                            };
                        } else {
                            phase = if postgres_complete
                                && final_status == UsageCleanupJobStatus::Completed
                            {
                                "complete".to_string()
                            } else {
                                "postgres".to_string()
                            };
                        }
                    }
                    Err(err) => {
                        final_status = UsageCleanupJobStatus::Failed;
                        stop_reason = Some("redis_snapshot_cleanup_failed".to_string());
                        last_error = Some(err.to_string());
                    }
                }
            }
        } else {
            phase = if postgres_complete && final_status == UsageCleanupJobStatus::Completed {
                "complete".to_string()
            } else {
                "postgres".to_string()
            };
        }
    } else if postgres_complete && final_status == UsageCleanupJobStatus::Completed {
        phase = "complete".to_string();
    }

    if lease_lost.load(Ordering::Acquire) {
        return;
    }

    let final_persist = persist_usage_cleanup_progress(
        &store,
        &cleanup_state,
        &job_id,
        &worker_id,
        final_status,
        &phase,
        processed_rows,
        0,
        batches,
        remaining_rows,
        stop_reason.as_deref(),
        last_error.as_deref(),
        &redis_stats,
        true,
    )
    .await;
    match final_persist {
        Ok(Some(_)) => {}
        Ok(None) => return,
        Err(err) => {
            tracing::warn!(job_id, error = %err, "持久化 usage cleanup 最终状态失败");
            return;
        }
    }

    let success = final_status != UsageCleanupJobStatus::Failed;
    if let Err(err) = postgres
        .record_admin_audit_log(
            "usage-cleanup-worker",
            "finish_usage_cleanup",
            "usage_record",
            Some(&job_id),
            success,
            last_error.as_deref(),
            json!({
                "jobId": job_id,
                "status": usage_cleanup_status_value(final_status),
                "phase": phase,
                "processedRows": processed_rows,
                "batches": batches,
                "stopReason": stop_reason,
                "redisDeletedKeys": redis_stats.deleted_keys,
                "redisDeleteCommands": redis_stats.delete_commands,
                "redisMaxCommandKeys": redis_stats.max_command_keys,
                "redisScanPasses": redis_stats.scan_passes,
                "redisUsedDelFallback": redis_stats.used_del_fallback,
                "redisPassLimitReached": redis_stats.pass_limit_reached,
            }),
        )
        .await
    {
        tracing::warn!(job_id, error = %err, "写入 usage cleanup 完成审计失败");
    }
}

fn invalidate_admin_cache_shadow(
    admin_cache_shadow: &Arc<Mutex<HashMap<String, AdminCacheEntry>>>,
    pattern: &str,
) {
    let mut shadow = admin_cache_shadow.lock();
    if let Some(prefix) = pattern.strip_suffix('*') {
        shadow.retain(|key, _| !key.starts_with(prefix));
    } else {
        shadow.remove(pattern);
    }
}

async fn invalidate_usage_admin_cache_after_cleanup(
    redis: &RedisStore,
    admin_cache_shadow: &Arc<Mutex<HashMap<String, AdminCacheEntry>>>,
    cancel: Option<&AtomicBool>,
) -> anyhow::Result<RedisPatternDeleteStats> {
    invalidate_admin_cache_shadow(admin_cache_shadow, "admin_cache:usage:*");
    let mut stats = redis
        .delete_pattern_bounded("admin_cache:usage:*", cancel)
        .await?;
    let usage_summary_stats = redis.clear_usage_summary_aggregates_bounded(cancel).await?;
    merge_redis_delete_stats(&mut stats, usage_summary_stats);
    Ok(stats)
}

#[allow(clippy::too_many_arguments)]
async fn persist_usage_cleanup_progress(
    store: &PostgresUsageStore,
    cleanup_state: &Arc<Mutex<UsageCleanupRuntime>>,
    job_id: &str,
    worker_id: &str,
    status: UsageCleanupJobStatus,
    phase: &str,
    processed_rows: u64,
    last_batch_rows: u64,
    batches: usize,
    remaining_rows: Option<u64>,
    stop_reason: Option<&str>,
    last_error: Option<&str>,
    redis_stats: &RedisPatternDeleteStats,
    finished: bool,
) -> anyhow::Result<Option<bool>> {
    let cancel_requested = store
        .update_cleanup_job_progress(
            UsageCleanupJobProgress {
                job_id,
                worker_id,
                status: usage_cleanup_status_value(status),
                phase,
                processed_rows,
                last_batch_rows,
                batches,
                remaining_rows,
                stop_reason,
                last_error,
                redis_deleted_keys: redis_stats.deleted_keys,
                redis_delete_commands: redis_stats.delete_commands,
                redis_max_command_keys: redis_stats.max_command_keys,
                redis_scan_passes: redis_stats.scan_passes,
                redis_used_del_fallback: redis_stats.used_del_fallback,
                redis_pass_limit_reached: redis_stats.pass_limit_reached,
                finished,
            },
            USAGE_CLEANUP_LEASE_SECS,
        )
        .await?;
    if cancel_requested.is_none() {
        return Ok(None);
    }

    let now = Utc::now().to_rfc3339();
    let mut runtime = cleanup_state.lock();
    if runtime.status.job_id.as_deref() != Some(job_id) {
        return Ok(cancel_requested);
    }
    runtime.status.status = status;
    runtime.status.phase = phase.to_string();
    runtime.status.processed_rows = processed_rows;
    runtime.status.last_batch_rows = last_batch_rows;
    runtime.status.batches = batches;
    runtime.status.redis_deleted_keys = redis_stats.deleted_keys;
    runtime.status.redis_delete_commands = redis_stats.delete_commands;
    runtime.status.redis_max_command_keys = redis_stats.max_command_keys;
    runtime.status.redis_scan_passes = redis_stats.scan_passes;
    runtime.status.redis_used_del_fallback = redis_stats.used_del_fallback;
    runtime.status.redis_pass_limit_reached = redis_stats.pass_limit_reached;
    runtime.status.cancel_requested = cancel_requested.unwrap_or(false);
    runtime.status.updated_at = Some(now.clone());
    if let Some(remaining_rows) = remaining_rows {
        runtime.status.remaining_rows = Some(remaining_rows);
    } else if let Some(matched_rows) = runtime.status.matched_rows {
        runtime.status.remaining_rows = Some(matched_rows.saturating_sub(processed_rows));
    }
    if let Some(stop_reason) = stop_reason {
        runtime.status.stop_reason = Some(stop_reason.to_string());
    }
    if let Some(last_error) = last_error {
        runtime.status.last_error = Some(last_error.to_string());
    }
    if finished {
        runtime.status.finished_at = Some(now);
        runtime.cancel = None;
    }
    Ok(cancel_requested)
}

/// Disable reasons that mean the account itself is unusable (rejected token, invalid config,
/// suspended or locked), whatever its quota snapshot says.

fn merge_redis_delete_stats(
    aggregate: &mut RedisPatternDeleteStats,
    current: RedisPatternDeleteStats,
) {
    aggregate.deleted_keys = aggregate.deleted_keys.saturating_add(current.deleted_keys);
    aggregate.scan_calls = aggregate.scan_calls.saturating_add(current.scan_calls);
    aggregate.delete_commands = aggregate
        .delete_commands
        .saturating_add(current.delete_commands);
    aggregate.max_command_keys = aggregate.max_command_keys.max(current.max_command_keys);
    aggregate.scan_passes = aggregate.scan_passes.saturating_add(current.scan_passes);
    aggregate.used_del_fallback |= current.used_del_fallback;
    aggregate.cancelled |= current.cancelled;
    aggregate.pass_limit_reached |= current.pass_limit_reached;
}

fn extract_model_ids_from_models_response(body: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    collect_model_ids_from_value(&value, &mut out);
    out
}

fn collect_model_ids_from_value(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_model_id_item(item, out);
            }
        }
        Value::Object(map) => {
            for key in ["data", "models", "modelList", "items"] {
                if let Some(items) = map.get(key).and_then(|value| value.as_array()) {
                    for item in items {
                        collect_model_id_item(item, out);
                    }
                }
            }
            if let Some(default_model) = map.get("defaultModel") {
                collect_model_id_item(default_model, out);
            }
        }
        _ => {}
    }
}

fn collect_model_id_item(value: &Value, out: &mut Vec<String>) {
    if let Some(model) = value.as_str() {
        out.push(model.to_string());
        return;
    }
    let Some(map) = value.as_object() else {
        return;
    };
    for key in ["id", "model", "modelId", "model_id"] {
        if let Some(model) = map.get(key).and_then(|value| value.as_str()) {
            out.push(model.to_string());
            return;
        }
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
pub(super) mod tests;

// ============ 路由场景辅助 ============

impl AdminService {}

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

// Seams for the account inventory (`inventory.rs`). The inventory keeps its own table and worker;
// it only borrows the shared pool, runtime config, proxy snapshot and the pool import below.
impl AdminService {}

// Seams for the read-only public pool view (`public_pool.rs`). The view keeps its own router and
// response types; it only borrows the account snapshot, the status rules and the runtime switch.
impl AdminService {}

pub(super) fn block_on_admin_store_public<T>(
    future: impl std::future::Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    block_on_admin_store(future)
}

impl AdminService {
    pub(super) fn route_scenario_store(&self) -> Arc<PostgresStore> {
        self.postgres_store.clone()
    }
    pub(super) fn route_scenario_runtime_config(&self) -> crate::model::config::Config {
        self.runtime.config()
    }
    pub(super) fn route_scenario_routes(&self) -> Vec<String> {
        let config = self.runtime.config();
        let mut routes: std::collections::BTreeSet<String> = ["/v1", "/cc", "/ha", "/na"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let normalize =
            |path: &str| crate::model::config::normalize_route_rules(&[path.to_string()]);
        for route in &config.routes.routes {
            routes.extend(normalize(&route.prefix));
        }
        routes.into_iter().collect()
    }
    pub(super) fn set_route_scenario_active_id(&self, id: Option<u64>) {
        if let Err(err) = self
            .runtime
            .update(|config| config.active_route_scenario_id = id)
        {
            tracing::warn!(error = %err, "记录当前路由场景失败");
        }
    }

    pub(super) fn route_scenario_active_id(&self) -> Option<u64> {
        self.runtime.config().active_route_scenario_id
    }
    pub(super) fn set_route_scenario_definitions(
        &self,
        routes: &crate::model::routes::RoutesConfig,
    ) -> Result<(), AdminServiceError> {
        routes
            .validate_with_config(&self.runtime.config())
            .map_err(AdminServiceError::InvalidCredential)?;
        self.runtime
            .update(|config| config.routes = routes.clone())
            .map_err(|error| AdminServiceError::InternalError(error.to_string()))?;
        self.invalidate_admin_cache_pattern("admin_cache:accounts:*");
        Ok(())
    }
    pub(super) fn route_scenario_audit(
        &self,
        action: &'static str,
        id: u64,
        detail: serde_json::Value,
    ) {
        self.audit(
            action,
            "route_scenario",
            Some(id.to_string()),
            true,
            None,
            detail,
        );
    }
}

pub(crate) fn validate_accounts_config(config: &AccountsConfig) -> Result<(), String> {
    if config.global_max_concurrent_requests > 100_000 {
        return Err("globalMaxConcurrentRequests 不能大于 100000".to_string());
    }
    if config.max_queued_requests > 100_000 {
        return Err("maxQueuedRequests 不能大于 100000".to_string());
    }
    if config.dispatch_max_wait_secs > 86_400 {
        return Err("dispatchMaxWaitSecs 不能大于 86400".to_string());
    }
    if config.retry_max_attempts > 10_000 {
        return Err("retryMaxAttempts 不能大于 10000".to_string());
    }
    if config
        .retry_status_codes
        .iter()
        .any(|code| !(100..=599).contains(code))
    {
        return Err("retryStatusCodes 只能包含 100 到 599 的 HTTP 状态码".to_string());
    }
    if config.retry_status_codes.len() > 100 {
        return Err("retryStatusCodes 不能超过 100 个".to_string());
    }
    if config.same_pool_retry_count > 10 {
        return Err("samePoolRetryCount 不能大于 10".to_string());
    }
    if config.same_pool_retry_delay_ms > 60_000 {
        return Err("samePoolRetryDelayMs 不能大于 60000".to_string());
    }
    if config
        .same_pool_retry_status_codes
        .iter()
        .any(|code| !(100..=599).contains(code))
    {
        return Err("samePoolRetryStatusCodes 只能包含 100 到 599 的 HTTP 状态码".to_string());
    }
    if config.same_pool_retry_status_codes.len() > 100 {
        return Err("samePoolRetryStatusCodes 不能超过 100 个".to_string());
    }
    if config.transient_failure_priority_penalty > 10_000 {
        return Err("transientFailurePriorityPenalty 不能大于 10000".to_string());
    }

    if !(0.01..=1.0).contains(&config.quality_ewma_alpha) {
        return Err("qualityEwmaAlpha 必须在 0.01 到 1 之间".to_string());
    }
    for (label, weight) in [
        ("qualityPriorityWeight", config.quality_priority_weight),
        ("qualityLoadWeight", config.quality_load_weight),
        ("qualityErrorWeight", config.quality_error_weight),
        ("qualityLatencyWeight", config.quality_latency_weight),
        ("qualityProbationWeight", config.quality_probation_weight),
    ] {
        if !weight.is_finite() || weight < 0.0 {
            return Err(format!("{label} 必须是大于等于 0 的有限数"));
        }
        if weight > 1_000_000.0 {
            return Err(format!("{label} 不能大于 1000000"));
        }
    }
    if config.quality_top_k > 100 {
        return Err("qualityTopK 不能大于 100".to_string());
    }
    if config.quality_min_samples > 10_000 {
        return Err("qualityMinSamples 不能大于 10000".to_string());
    }
    if config.quality_sample_ttl_secs > 86_400 {
        return Err("qualitySampleTtlSecs 不能大于 86400".to_string());
    }
    if config.degrade_window_secs > 86_400 {
        return Err("degradeWindowSecs 不能大于 86400".to_string());
    }
    if !(0.0..=1.0).contains(&config.degrade_error_rate_threshold) {
        return Err("degradeErrorRateThreshold 必须在 0 到 1 之间".to_string());
    }
    if config.degrade_probation_secs > 86_400 {
        return Err("degradeProbationSecs 不能大于 86400".to_string());
    }
    if config.max_probation_secs > 86_400 {
        return Err("maxProbationSecs 不能大于 86400".to_string());
    }
    // 避让上限必须不小于单次避让时长，否则指数退避的上限语义自相矛盾。
    if config.max_probation_secs < config.degrade_probation_secs {
        return Err("maxProbationSecs 不能小于 degradeProbationSecs".to_string());
    }
    if config.probe_share_percent > 100 {
        return Err("probeSharePercent 不能大于 100".to_string());
    }
    if config.recovery_ramp_secs > 86_400 {
        return Err("recoveryRampSecs 不能大于 86400".to_string());
    }

    if config.auto_disable_failure_threshold == 0 || config.auto_disable_failure_threshold > 10_000
    {
        return Err("autoDisableFailureThreshold 必须在 1 到 10000 之间".to_string());
    }
    if config.auto_disable_window_secs == 0 || config.auto_disable_window_secs > 24 * 60 * 60 {
        return Err("autoDisableWindowSecs 必须在 1 到 86400 之间".to_string());
    }
    if config.auto_disable_duration_secs > 365 * 24 * 60 * 60 {
        return Err("autoDisableDurationSecs 不能超过 365 天".to_string());
    }
    if config.rate_limit_cooldown_secs == 0
        || config.server_error_cooldown_secs == 0
        || config.network_error_cooldown_secs == 0
        || config.protocol_error_cooldown_secs == 0
        || config.model_unavailable_cooldown_secs == 0
    {
        return Err("账号错误冷却秒数必须大于 0".to_string());
    }
    if config.request_timeout_secs > 86_400 {
        return Err("requestTimeoutSecs 不能大于 86400".to_string());
    }
    if config.stream_request_timeout_secs > 86_400 {
        return Err("streamRequestTimeoutSecs 不能大于 86400".to_string());
    }
    if config.stream_idle_timeout_secs > 86_400 {
        return Err("streamIdleTimeoutSecs 不能大于 86400".to_string());
    }
    if config.stream_first_output_timeout_secs > 86_400 {
        return Err("streamFirstOutputTimeoutSecs 不能大于 86400".to_string());
    }
    if config.usage_projection_uplift_percent > 200 {
        return Err("usageProjectionUpliftPercent 不能大于 200".to_string());
    }
    if config.usage_projection_cost_floor_margin_percent > 200 {
        return Err("usageProjectionCostFloorMarginPercent 不能大于 200".to_string());
    }
    if config.usage_projection_output_uplift_min_tokens < 0 {
        return Err("usageProjectionOutputUpliftMinTokens 不能小于 0".to_string());
    }
    if config.usage_projection_output_uplift_percent > 200 {
        return Err("usageProjectionOutputUpliftPercent 不能大于 200".to_string());
    }
    if config.usage_debug_dir.chars().count() > 512 {
        return Err("usageDebugDir 不能超过 512 个字符".to_string());
    }
    if config.usage_debug_max_body_bytes > 1024 * 1024 {
        return Err("usageDebugMaxBodyBytes 不能大于 1048576".to_string());
    }
    if config.usage_debug_max_files > 100_000 {
        return Err("usageDebugMaxFiles 不能大于 100000".to_string());
    }
    if config.wire_debug_dir.chars().count() > 512 {
        return Err("wireDebugDir 不能超过 512 个字符".to_string());
    }
    if config.wire_debug_max_body_bytes > 1024 * 1024 {
        return Err("wireDebugMaxBodyBytes 不能大于 1048576".to_string());
    }
    if config.wire_debug_max_files > 100_000 {
        return Err("wireDebugMaxFiles 不能大于 100000".to_string());
    }
    Ok(())
}
