//! Token 管理模块
//!
//! 负责 Token 过期检测和刷新，支持 Social 和 IdC 认证方式
//! 支持多凭据 (MultiTokenManager) 管理

use chrono::{DateTime, Duration, Utc};
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex as TokioMutex, Notify};

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration as StdDuration, Instant};

use crate::http_client::ProxyConfig;
use crate::kiro::call_trace::{
    AccountRejectReason, RejectedAccountSample, SelectionFailureStage, SelectionFailureSummary,
};
use crate::kiro::machine_id;
use crate::kiro::model::credentials::KiroCredentials;
use crate::kiro::model::usage_limits::UsageLimitsResponse;
use crate::model::config::Config;
use crate::storage::postgres::{
    CredentialRuntimeStateRow, CredentialRuntimeStateSnapshot, CredentialStatsDeltaRow,
    PostgresStore,
};
use crate::storage::redis_cache::{
    RedisStore, SchedulerCredentialState, SchedulerGlobalCapacityState, SchedulerHealthState,
};

use super::account_state::{
    CredentialEntry, CredentialModelCooldown, CredentialRiskControlReason, DisabledReason,
    InFlightLease, ProxyResourceAvailability, ProxyResourceRuntime, SessionBinding,
};
use super::admin_snapshot::{
    CredentialBaseSnapshot, CredentialEntrySnapshot, ManagerBaseSnapshot, ManagerRuntimeSnapshot,
    ManagerSnapshot, ManagerSummarySnapshot,
};
use super::capacity::{
    credential_is_dispatch_candidate, credential_is_dispatchable,
    credential_is_temporarily_available, credential_is_usable_for_model,
    credential_proxy_availability, credential_proxy_is_dispatchable,
    effective_max_concurrent_requests, entry_has_concurrency_capacity,
    global_has_concurrency_capacity, is_opus_model, proxy_unavailable_error,
};
use super::concurrency::{DispatchQueueGuard, InFlightLeaseGuard};
use super::cooldown::{
    entry_any_cooldown_remaining, entry_cooldown_remaining, entry_cooldown_snapshots,
    model_state_key,
};
use super::queue::{
    concurrency_blocked_count, effective_concurrency_range_for_candidates,
    format_effective_concurrency_range, min_dispatch_wait,
};
use super::refresh::{
    RefreshTokenInvalidError, get_usage_limits, is_token_expired, is_token_expiring_soon,
    refresh_token, validate_refresh_token,
};
use super::route_state::{CachedLocalPoolRouteState, LocalPoolRouteState, LocalPoolRouteStateKind};
use super::rpm::{effective_rpm, entry_rate_limit_remaining, rate_limit_interval_for_rpm};
use super::sticky::{
    bind_session_to_credential as bind_sticky_session_to_credential,
    bound_credential_id as sticky_bound_credential_id,
    clear_session_soft_failure as clear_sticky_session_soft_failure, prune_session_bindings_locked,
    record_session_soft_failure as record_sticky_session_soft_failure,
    unbind_session as unbind_sticky_session,
    unbind_session_if_bound_to as unbind_sticky_session_if_bound_to,
    unbind_sessions_for_credential as unbind_sticky_sessions_for_credential,
};
use super::storage_task::{block_on_storage, spawn_best_effort_storage_task};
use super::strategy::{
    balanced_selection_key, entry_effective_health_mut, priority_selection_key,
    record_local_selection, refresh_local_selection_windows_locked, scheduler_score_with_config,
    select_health_weighted, selection_pressure_from_totals, should_select_warming_from_totals,
};
use super::types::{
    AcquireMode, CallContext, CredentialAuthUpdate, EXTERNAL_CREDENTIAL_CONTEXT_ID, InFlightKind,
    TransientFailureKind,
};

#[cfg(test)]
use super::refresh::{
    is_invalid_grant_response, usage_limits_amz_user_agent, usage_limits_user_agent,
};

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    format!("{:x}", result)
}

/// 生成 API Key 脱敏展示(前 4 + ... + 后 4,长度不足或非 ASCII 回退 ***)
fn mask_api_key(key: &str) -> String {
    if key.is_ascii() && key.len() > 16 {
        format!("{}...{}", &key[..4], &key[key.len() - 4..])
    } else {
        "***".to_string()
    }
}

fn trimmed_optional(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn apply_optional_string(target: &mut Option<String>, value: Option<String>) {
    if let Some(value) = value {
        *target = trimmed_optional(value);
    }
}

fn apply_credential_auth_update(credential: &mut KiroCredentials, update: CredentialAuthUpdate) {
    let mut clear_access_token = false;

    if let Some(api_key) = update.kiro_api_key {
        credential.kiro_api_key = trimmed_optional(api_key);
        credential.refresh_token = None;
        credential.provider = None;
        credential.client_id = None;
        credential.client_secret = None;
        credential.auth_method = Some("api_key".to_string());
        clear_access_token = true;
    }

    if let Some(refresh_token) = update.refresh_token {
        credential.refresh_token = trimmed_optional(refresh_token);
        credential.kiro_api_key = None;
        if credential
            .auth_method
            .as_deref()
            .is_none_or(|method| method.eq_ignore_ascii_case("api_key"))
        {
            credential.auth_method = Some("social".to_string());
        }
        clear_access_token = true;
    }

    if let Some(auth_method) = update.auth_method {
        if let Some(auth_method) = trimmed_optional(auth_method) {
            credential.auth_method = Some(auth_method);
            clear_access_token = true;
        }
    }
    if update.provider.is_some() {
        apply_optional_string(&mut credential.provider, update.provider);
    }
    if update.client_id.is_some() {
        apply_optional_string(&mut credential.client_id, update.client_id);
        clear_access_token = true;
    }
    if update.client_secret.is_some() {
        apply_optional_string(&mut credential.client_secret, update.client_secret);
        clear_access_token = true;
    }
    if update.region.is_some() {
        apply_optional_string(&mut credential.region, update.region);
        clear_access_token = true;
    }
    if update.auth_region.is_some() {
        apply_optional_string(&mut credential.auth_region, update.auth_region);
        clear_access_token = true;
    }
    apply_optional_string(&mut credential.api_region, update.api_region);
    apply_optional_string(&mut credential.machine_id, update.machine_id);
    apply_optional_string(&mut credential.email, update.email);
    apply_optional_string(&mut credential.endpoint, update.endpoint);

    if clear_access_token {
        credential.access_token = None;
        credential.expires_at = None;
        credential.profile_arn = None;
        credential.subscription_title = None;
    }
}

// ============================================================================
// 多凭据 Token 管理器
// ============================================================================

fn instant_from_epoch_ms(target_ms: i64, now_ms: i64, now: Instant) -> Option<Instant> {
    (target_ms > now_ms).then(|| now + StdDuration::from_millis((target_ms - now_ms) as u64))
}

fn instant_from_elapsed_epoch_ms(target_ms: i64, now_ms: i64, now: Instant) -> Instant {
    if target_ms >= now_ms {
        now
    } else {
        now.checked_sub(StdDuration::from_millis((now_ms - target_ms) as u64))
            .unwrap_or(now)
    }
}

fn truncate_for_audit(value: &str, max_chars: usize) -> String {
    let mut truncated = value.chars().take(max_chars).collect::<String>();
    if value.chars().count() > max_chars {
        truncated.push_str("...");
    }
    truncated
}

fn merge_json_object(base: &mut serde_json::Value, extra: serde_json::Value) {
    let (Some(base), serde_json::Value::Object(extra)) = (base.as_object_mut(), extra) else {
        return;
    };
    for (key, value) in extra {
        base.insert(key, value);
    }
}

/// 多凭据 Token 管理器
///
/// 支持多个凭据的管理，实现固定优先级 + 故障转移策略
/// 故障统计基于 API 调用结果，而非 Token 刷新结果
pub struct MultiTokenManager {
    config: Mutex<Config>,
    proxy: Option<ProxyConfig>,
    /// 凭据条目列表
    entries: Arc<Mutex<Vec<CredentialEntry>>>,
    /// 当前活动凭据 ID
    current_id: Mutex<u64>,
    /// Token 刷新锁按凭据隔离，避免单个坏凭据刷新等待阻塞其他凭据。
    refresh_locks: Mutex<HashMap<u64, Arc<TokioMutex<()>>>>,
    /// PgSQL 存储后端。生产运行必须配置；测试可使用 None 避免依赖外部数据库。
    postgres_store: Option<Arc<PostgresStore>>,
    /// Redis 运行态存储后端。生产用于跨实例调度状态；测试可使用 None 走本地内存。
    redis_store: Option<Arc<RedisStore>>,
    /// 代理/家宽资源快照。凭据直接代理优先，未设置时可通过 proxy_resource_id 绑定这里的资源。
    proxy_resources: Arc<Mutex<HashMap<u64, ProxyResourceRuntime>>>,
    /// 负载均衡模式（运行时可修改）
    load_balancing_mode: Mutex<String>,
    /// 最近一次统计持久化时间（用于 debounce）
    last_stats_save_at: Mutex<Option<Instant>>,
    /// 最近一次从 Redis 全量同步调度状态的时间，用于避免每个请求重复拉取所有凭据状态。
    last_scheduler_redis_sync_at: Mutex<Option<Instant>>,
    /// 最近一次执行 Redis 超时 lease 清理的时间。清理是全局操作，不能放在请求热路径每轮执行。
    last_scheduler_redis_cleanup_at: Mutex<Option<Instant>>,
    /// Redis 调度热路径最近一次超时/失败后的退避截止时间。退避期间请求只用本机内存态调度。
    scheduler_redis_degraded_until: Mutex<Option<Instant>>,
    /// 统计数据是否有未落盘更新
    stats_dirty: AtomicBool,
    /// 未落盘的统计增量。请求热路径只合并内存 delta，后台任务定期写入 PgSQL。
    pending_stats_deltas: Mutex<HashMap<u64, CredentialStatsDeltaRow>>,
    /// 未落盘的运行态快照。成功路径只合并最新快照，后台任务定期写入 PgSQL。
    pending_runtime_state_snapshots: Mutex<HashMap<u64, CredentialRuntimeStateSnapshot>>,
    /// 会话粘性绑定：conversationId -> credential id
    session_bindings: Mutex<HashMap<String, SessionBinding>>,
    /// 凭据并发槽释放通知，用于所有凭据占满时排队等待。
    in_flight_notify: Arc<Notify>,
    next_in_flight_lease_id: AtomicU64,
    queued_requests: Arc<AtomicU32>,
    /// 短 TTL 派生缓存，只用于避免高并发下本地池预检重复全量扫描。
    local_pool_route_state_cache: Arc<Mutex<HashMap<String, CachedLocalPoolRouteState>>>,
}

/// 每个凭据最大 API 调用失败次数
const MAX_FAILURES_PER_CREDENTIAL: u32 = 3;
/// 并发排队等待的周期性唤醒间隔，避免极端竞态下丢失通知后永久睡眠。
const CONCURRENCY_WAIT_WAKEUP_SECS: u64 = 30;
const SCHEDULER_REDIS_SYNC_MIN_INTERVAL: StdDuration = StdDuration::from_secs(1);
const SCHEDULER_REDIS_CLEANUP_MIN_INTERVAL: StdDuration = StdDuration::from_secs(5);
const SCHEDULER_REDIS_HOT_OP_TIMEOUT: StdDuration = StdDuration::from_millis(75);
const SCHEDULER_REDIS_DEGRADED_BACKOFF: StdDuration = StdDuration::from_secs(2);
const LOCAL_POOL_ROUTE_STATE_CACHE_TTL: StdDuration = StdDuration::from_millis(250);
const LOCAL_POOL_ROUTE_STATE_CACHE_MAX_KEYS: usize = 128;
const SELECTION_FAILURE_SAMPLE_LIMIT: usize = 20;
const CREDENTIAL_STATS_FLUSH_MIN_INTERVAL: StdDuration = StdDuration::from_secs(5);

impl MultiTokenManager {
    /// 创建多凭据 Token 管理器
    ///
    /// # Arguments
    /// * `config` - 应用配置
    /// * `credentials` - 凭据列表
    /// * `proxy` - 可选的代理配置
    /// * `credentials_path` - 已废弃，保留参数用于测试和调用兼容；生产不写凭据文件
    /// * `is_multiple_format` - 已废弃，保留参数用于测试和调用兼容
    #[cfg(test)]
    pub fn new(
        config: Config,
        credentials: Vec<KiroCredentials>,
        proxy: Option<ProxyConfig>,
        credentials_path: Option<PathBuf>,
        is_multiple_format: bool,
    ) -> anyhow::Result<Self> {
        let _ = (credentials_path, is_multiple_format);
        Self::new_with_postgres_store(config, credentials, proxy, None, false, None)
    }

    #[cfg(test)]
    pub fn new_with_postgres_store(
        config: Config,
        credentials: Vec<KiroCredentials>,
        proxy: Option<ProxyConfig>,
        credentials_path: Option<PathBuf>,
        is_multiple_format: bool,
        postgres_store: Option<Arc<PostgresStore>>,
    ) -> anyhow::Result<Self> {
        Self::new_with_stores(
            config,
            credentials,
            proxy,
            credentials_path,
            is_multiple_format,
            postgres_store,
            None,
        )
    }

    pub fn new_with_stores(
        config: Config,
        mut credentials: Vec<KiroCredentials>,
        proxy: Option<ProxyConfig>,
        credentials_path: Option<PathBuf>,
        is_multiple_format: bool,
        postgres_store: Option<Arc<PostgresStore>>,
        redis_store: Option<Arc<RedisStore>>,
    ) -> anyhow::Result<Self> {
        let _ = (credentials_path, is_multiple_format);
        if credentials.iter().any(|credential| credential.id.is_none()) {
            if let Some(store) = &postgres_store {
                let store = store.clone();
                credentials = block_on_storage("通过 PgSQL 为无 ID 凭据分配 ID", async move {
                    let mut resolved = Vec::with_capacity(credentials.len());
                    for credential in credentials {
                        if credential.id.is_some() {
                            resolved.push(credential);
                        } else {
                            resolved.push(store.insert_credential(&credential).await?);
                        }
                    }
                    Ok(resolved)
                })?;
            }
        }

        // 计算当前最大 ID，为没有 ID 的凭据分配新 ID
        let max_existing_id = credentials.iter().filter_map(|c| c.id).max().unwrap_or(0);
        let mut next_id = max_existing_id + 1;
        let mut has_new_ids = false;
        let mut has_new_machine_ids = false;
        let config_ref = &config;

        let entries: Vec<CredentialEntry> = credentials
            .into_iter()
            .map(|mut cred| {
                cred.canonicalize_auth_method();
                let id = cred.id.unwrap_or_else(|| {
                    let id = next_id;
                    next_id += 1;
                    cred.id = Some(id);
                    has_new_ids = true;
                    id
                });
                if cred.machine_id.is_none() {
                    cred.machine_id =
                        Some(machine_id::generate_from_credentials(&cred, config_ref));
                    has_new_machine_ids = true;
                }
                CredentialEntry {
                    id,
                    credentials: cred.clone(),
                    failure_count: 0,
                    refresh_failure_count: 0,
                    disabled: cred.disabled, // 从配置文件读取 disabled 状态
                    disabled_reason: if cred.disabled {
                        Some(DisabledReason::Manual)
                    } else {
                        None
                    },
                    success_count: 0,
                    total_selection_count: 0,
                    last_used_at: None,
                    cooldown_until: None,
                    cooldown_reason: None,
                    model_cooldowns: HashMap::new(),
                    rate_limit_available_at: None,
                    in_flight_requests: 0,
                    in_flight_leases: Vec::new(),
                    warmup_remaining: 0,
                    health: SchedulerHealthState::default(),
                    model_health: HashMap::new(),
                    selection_events: VecDeque::new(),
                }
            })
            .collect();

        // 校验 API Key 凭据配置完整性：authMethod=api_key 时必须提供 kiroApiKey
        let mut entries = entries;
        let mut invalid_config_credential_ids = Vec::new();
        for entry in &mut entries {
            if entry.credentials.kiro_api_key.is_none()
                && entry
                    .credentials
                    .auth_method
                    .as_deref()
                    .map(|m| m.eq_ignore_ascii_case("api_key") || m.eq_ignore_ascii_case("apikey"))
                    .unwrap_or(false)
            {
                tracing::warn!(
                    "凭据 #{} 配置了 authMethod=api_key 但缺少 kiroApiKey 字段，已自动禁用",
                    entry.id
                );
                entry.disabled = true;
                entry.disabled_reason = Some(DisabledReason::InvalidConfig);
                invalid_config_credential_ids.push(entry.id);
            }
        }

        // 检测重复 ID
        let mut seen_ids = std::collections::HashSet::new();
        let mut duplicate_ids = Vec::new();
        for entry in &entries {
            if !seen_ids.insert(entry.id) {
                duplicate_ids.push(entry.id);
            }
        }
        if !duplicate_ids.is_empty() {
            anyhow::bail!("检测到重复的凭据 ID: {:?}", duplicate_ids);
        }

        // 选择初始凭据：优先级最高（priority 最小）的可用凭据，无可用凭据时为 0
        let initial_id = entries
            .iter()
            .filter(|e| !e.disabled)
            .min_by_key(|e| e.credentials.priority)
            .map(|e| e.id)
            .unwrap_or(0);

        let load_balancing_mode = config.load_balancing_mode.clone();
        let proxy_resources = if let Some(store) = &postgres_store {
            let store = store.clone();
            match block_on_storage("从 PgSQL 加载代理资源", async move {
                store.load_proxy_resources().await
            }) {
                Ok(resources) => resources
                    .into_iter()
                    .map(ProxyResourceRuntime::from)
                    .map(|resource| (resource.id, resource))
                    .collect(),
                Err(err) => {
                    tracing::warn!("加载代理资源失败: {}", err);
                    HashMap::new()
                }
            }
        } else {
            HashMap::new()
        };
        let manager = Self {
            config: Mutex::new(config),
            proxy,
            entries: Arc::new(Mutex::new(entries)),
            current_id: Mutex::new(initial_id),
            refresh_locks: Mutex::new(HashMap::new()),
            postgres_store,
            redis_store,
            proxy_resources: Arc::new(Mutex::new(proxy_resources)),
            load_balancing_mode: Mutex::new(load_balancing_mode),
            last_stats_save_at: Mutex::new(None),
            last_scheduler_redis_sync_at: Mutex::new(None),
            last_scheduler_redis_cleanup_at: Mutex::new(None),
            scheduler_redis_degraded_until: Mutex::new(None),
            stats_dirty: AtomicBool::new(false),
            pending_stats_deltas: Mutex::new(HashMap::new()),
            pending_runtime_state_snapshots: Mutex::new(HashMap::new()),
            session_bindings: Mutex::new(HashMap::new()),
            in_flight_notify: Arc::new(Notify::new()),
            next_in_flight_lease_id: AtomicU64::new(1),
            queued_requests: Arc::new(AtomicU32::new(0)),
            local_pool_route_state_cache: Arc::new(Mutex::new(HashMap::new())),
        };

        // 如果有新分配的 ID 或新生成的 machineId，立即持久化到 PgSQL。
        if has_new_ids || has_new_machine_ids {
            if let Err(e) = manager.persist_credentials() {
                tracing::warn!("补全凭据 ID/machineId 后持久化失败: {}", e);
            } else {
                tracing::info!("已补全凭据 ID/machineId 并写入数据库");
            }
        }

        // 加载持久化的统计数据（success_count, last_used_at）
        manager.load_stats();
        manager.load_runtime_state();
        manager.refresh_scheduler_state_from_redis_force_best_effort();
        for id in invalid_config_credential_ids {
            manager.record_scheduler_credential_audit(
                "auto_disable_credential",
                id,
                DisabledReason::InvalidConfig,
                "startup_invalid_config",
                "凭据配置无效，启动时已自动禁用",
                serde_json::json!({
                    "configError": "api_key_auth_without_kiro_api_key",
                }),
            );
        }

        Ok(manager)
    }

    /// 获取当前运行时配置快照。
    pub fn runtime_config(&self) -> Config {
        self.config.lock().clone()
    }

    pub fn current_id(&self) -> u64 {
        *self.current_id.lock()
    }

    fn credential_audit_label(entry: &CredentialEntry) -> String {
        let prefix = format!("#{}", entry.id);
        let label = entry
            .credentials
            .email
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| entry.credentials.kiro_api_key.as_deref().map(mask_api_key))
            .or_else(|| entry.credentials.endpoint.clone());

        match label {
            Some(label) if label.starts_with(&prefix) => label,
            Some(label) => format!("{} {}", prefix, label),
            None => prefix,
        }
    }

    fn record_scheduler_credential_audit(
        &self,
        action: &'static str,
        id: u64,
        reason: DisabledReason,
        trigger: &'static str,
        message: &'static str,
        extra_detail: serde_json::Value,
    ) {
        let Some(store) = self.postgres_store.clone() else {
            return;
        };

        let current_id = *self.current_id.lock();
        let (
            label,
            auth_method,
            endpoint,
            disabled,
            failure_count,
            refresh_failure_count,
            available,
        ) = {
            let entries = self.entries.lock();
            let available = entries.iter().filter(|entry| !entry.disabled).count();
            match entries.iter().find(|entry| entry.id == id) {
                Some(entry) => (
                    Self::credential_audit_label(entry),
                    entry.credentials.auth_method.clone(),
                    entry.credentials.endpoint.clone(),
                    entry.disabled,
                    entry.failure_count,
                    entry.refresh_failure_count,
                    available,
                ),
                None => (format!("#{}", id), None, None, false, 0, 0, available),
            }
        };

        let mut detail = serde_json::json!({
            "credentialId": id,
            "credentialLabel": label,
            "reason": reason.as_str(),
            "reasonLabel": reason.label(),
            "trigger": trigger,
            "message": message,
            "disabled": disabled,
            "failureCount": failure_count,
            "refreshFailureCount": refresh_failure_count,
            "maxFailures": MAX_FAILURES_PER_CREDENTIAL,
            "availableCredentials": available,
            "currentCredentialId": current_id,
            "source": "scheduler",
        });
        if let Some(auth_method) = auth_method {
            detail["authMethod"] = serde_json::Value::String(auth_method);
        }
        if let Some(endpoint) = endpoint {
            detail["endpoint"] = serde_json::Value::String(endpoint);
        }
        merge_json_object(&mut detail, extra_detail);

        let object_id = id.to_string();
        spawn_best_effort_storage_task("记录调度审计日志", async move {
            store
                .record_admin_audit_log(
                    "system-scheduler",
                    action,
                    "credential",
                    Some(&object_id),
                    true,
                    None,
                    detail,
                )
                .await
        });
    }

    /// 更新当前运行时配置并写入 PgSQL。
    pub fn update_runtime_config(&self, update: impl FnOnce(&mut Config)) -> anyhow::Result<()> {
        let mut updated = self.runtime_config();
        update(&mut updated);
        updated.set_config_path_for_runtime(None);

        let mut saved_version: Option<i64> = None;
        if let Some(store) = &self.postgres_store {
            let store = store.clone();
            let to_save = updated.clone();
            saved_version = Some(block_on_storage("持久化运行时配置到 PgSQL", async move {
                store.save_runtime_config_returning_version(&to_save).await
            })?);
        }

        {
            let mut config = self.config.lock();
            *config = updated;
        }
        self.invalidate_local_pool_route_state_cache();
        self.update_credential_rpm_from_config();
        self.notify_dispatch_state_changed();
        self.publish_runtime_config_changed(saved_version, "runtime_config_updated");

        Ok(())
    }

    /// 从 PgSQL 重新加载运行配置。用于 Redis pub/sub 通知或定时兜底检查。
    pub fn reload_runtime_config_from_postgres(&self) -> anyhow::Result<bool> {
        let Some(store) = &self.postgres_store else {
            return Ok(false);
        };
        let store = store.clone();
        let Some(mut config) = block_on_storage("从 PgSQL 重新加载运行时配置", async move {
            store.load_runtime_config().await
        })?
        else {
            return Ok(false);
        };
        config.set_config_path_for_runtime(None);
        {
            let mut current = self.config.lock();
            *current = config.clone();
        }
        *self.load_balancing_mode.lock() = config.load_balancing_mode.clone();
        self.invalidate_local_pool_route_state_cache();
        self.update_credential_rpm_from_config();
        self.notify_dispatch_state_changed();
        Ok(true)
    }

    pub fn update_load_balancing_mode_in_config(&self, mode: &str) {
        self.config.lock().load_balancing_mode = mode.to_string();
    }

    /// 获取凭据总数
    pub fn total_count(&self) -> usize {
        self.entries.lock().len()
    }

    /// 获取可用凭据数量
    pub fn available_count(&self) -> usize {
        self.entries.lock().iter().filter(|e| !e.disabled).count()
    }

    fn local_pool_route_state_cache_key(model: Option<&str>) -> String {
        model
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_ascii_lowercase)
            .unwrap_or_else(|| "*".to_string())
    }

    fn cached_local_pool_route_state(
        &self,
        key: &str,
        now: Instant,
    ) -> Option<LocalPoolRouteState> {
        self.local_pool_route_state_cache
            .lock()
            .get(key)
            .filter(|cached| cached.expires_at > now)
            .map(|cached| cached.state.clone())
    }

    fn store_local_pool_route_state_cache(
        &self,
        key: String,
        state: LocalPoolRouteState,
        now: Instant,
    ) {
        let mut cache = self.local_pool_route_state_cache.lock();
        if cache.len() >= LOCAL_POOL_ROUTE_STATE_CACHE_MAX_KEYS && !cache.contains_key(&key) {
            cache.retain(|_, cached| cached.expires_at > now);
            if cache.len() >= LOCAL_POOL_ROUTE_STATE_CACHE_MAX_KEYS {
                cache.clear();
            }
        }
        cache.insert(
            key,
            CachedLocalPoolRouteState {
                state,
                expires_at: now + LOCAL_POOL_ROUTE_STATE_CACHE_TTL,
            },
        );
    }

    fn invalidate_local_pool_route_state_cache(&self) {
        self.local_pool_route_state_cache.lock().clear();
    }

    pub fn local_pool_route_state(&self, model: Option<&str>) -> LocalPoolRouteState {
        let cache_key = Self::local_pool_route_state_cache_key(model);
        let now = Instant::now();
        if let Some(state) = self.cached_local_pool_route_state(&cache_key, now) {
            return state;
        }

        let mut state = self.compute_local_pool_route_state(model);
        if state.kind.should_route_external() && self.auto_heal_too_many_failures_if_applicable() {
            state = self.compute_local_pool_route_state(model);
        }
        self.store_local_pool_route_state_cache(cache_key, state.clone(), Instant::now());
        state
    }

    pub fn selection_failure_summary(
        &self,
        request_id: impl Into<String>,
        route: impl Into<String>,
        model: Option<&str>,
        error_message: &str,
    ) -> SelectionFailureSummary {
        let state = self.compute_local_pool_route_state(model);
        let (stage, primary_reason) =
            Self::selection_failure_stage_and_reason(&state, error_message);
        let (reason_counts, sampled_accounts, rejected_account_count, waitable_account_count) =
            self.selection_failure_account_breakdown(model);
        let retry_after_ms = state.retry_after_secs.map(|secs| secs.saturating_mul(1000));

        SelectionFailureSummary {
            request_id: request_id.into(),
            route: route.into(),
            model: model.unwrap_or("").to_string(),
            stage,
            primary_reason,
            rejected_account_count,
            waitable_account_count,
            retry_after_ms,
            reason_counts,
            sampled_accounts,
            dispatch_wait_ms: None,
            queue_depth: state.queued_requests,
            global_in_flight: state.global_in_flight_requests,
        }
    }

    fn selection_failure_stage_and_reason(
        state: &LocalPoolRouteState,
        error_message: &str,
    ) -> (SelectionFailureStage, AccountRejectReason) {
        if error_message.contains("等待队列已满") {
            return (
                SelectionFailureStage::DispatchQueue,
                AccountRejectReason::GlobalConcurrencyFull,
            );
        }
        if error_message.contains("排队等待超时") {
            return (
                SelectionFailureStage::DispatchWait,
                if state.global_max_concurrent_requests > 0
                    && state.global_in_flight_requests >= state.global_max_concurrent_requests
                {
                    AccountRejectReason::GlobalConcurrencyFull
                } else {
                    AccountRejectReason::AccountConcurrencyFull
                },
            );
        }
        if error_message.contains("临时排除") {
            return (
                SelectionFailureStage::StickyBinding,
                AccountRejectReason::StickyTargetUnavailable,
            );
        }

        match state.kind {
            LocalPoolRouteStateKind::NoCredentials => (
                SelectionFailureStage::AccountEligibility,
                AccountRejectReason::NoAccounts,
            ),
            LocalPoolRouteStateKind::AllDisabled => (
                SelectionFailureStage::AccountEligibility,
                AccountRejectReason::Disabled,
            ),
            LocalPoolRouteStateKind::NoModelCompatible => (
                SelectionFailureStage::ModelEligibility,
                AccountRejectReason::ModelNotSupported,
            ),
            LocalPoolRouteStateKind::ProxyBlocked => (
                SelectionFailureStage::RouteValidation,
                AccountRejectReason::ProxyUnavailable,
            ),
            LocalPoolRouteStateKind::AllCoolingDown => {
                if state.rate_limit_blocked >= state.cooldown_blocked {
                    (
                        SelectionFailureStage::RpmLimit,
                        AccountRejectReason::RpmLimited,
                    )
                } else {
                    (
                        SelectionFailureStage::Cooldown,
                        AccountRejectReason::CooldownActive,
                    )
                }
            }
            LocalPoolRouteStateKind::CapacityFull => {
                if state.global_max_concurrent_requests > 0
                    && state.global_in_flight_requests >= state.global_max_concurrent_requests
                {
                    (
                        SelectionFailureStage::GlobalConcurrency,
                        AccountRejectReason::GlobalConcurrencyFull,
                    )
                } else {
                    (
                        SelectionFailureStage::AccountConcurrency,
                        AccountRejectReason::AccountConcurrencyFull,
                    )
                }
            }
            LocalPoolRouteStateKind::Ready => (
                SelectionFailureStage::AccountEligibility,
                AccountRejectReason::Unknown,
            ),
        }
    }

    fn selection_failure_account_breakdown(
        &self,
        model: Option<&str>,
    ) -> (
        BTreeMap<AccountRejectReason, usize>,
        Vec<RejectedAccountSample>,
        usize,
        usize,
    ) {
        let entries = self.entries.lock();
        let proxy_resources = self.proxy_resources.lock();
        let config = self.config.lock().clone();
        let now = Instant::now();
        let global_in_flight = entries
            .iter()
            .map(|entry| entry.in_flight_requests)
            .sum::<u32>();
        let global_has_capacity = global_has_concurrency_capacity(
            global_in_flight,
            config.dispatch_global_max_concurrent_requests,
        );
        let global_rpm = config.credential_rpm.unwrap_or(0);
        let mut reason_counts: BTreeMap<AccountRejectReason, usize> = BTreeMap::new();
        let mut sampled_accounts = Vec::new();
        let mut rejected_account_count = 0usize;
        let mut waitable_account_count = 0usize;

        for entry in entries.iter() {
            let (reason, cooldown_remaining) = if entry.disabled {
                (AccountRejectReason::Disabled, None)
            } else if is_opus_model(model) && !entry.credentials.supports_opus() {
                (AccountRejectReason::ModelNotSupported, None)
            } else if !credential_proxy_is_dispatchable(&entry.credentials, &proxy_resources) {
                (AccountRejectReason::ProxyUnavailable, None)
            } else if let Some(remaining) = entry_cooldown_remaining(entry, model, now) {
                (AccountRejectReason::CooldownActive, Some(remaining))
            } else if entry_rate_limit_remaining(entry, now).is_some() {
                waitable_account_count = waitable_account_count.saturating_add(1);
                (AccountRejectReason::RpmLimited, None)
            } else if !global_has_capacity {
                waitable_account_count = waitable_account_count.saturating_add(1);
                (AccountRejectReason::GlobalConcurrencyFull, None)
            } else if !entry_has_concurrency_capacity(
                entry,
                config.credential_max_concurrent_requests,
            ) {
                waitable_account_count = waitable_account_count.saturating_add(1);
                (AccountRejectReason::AccountConcurrencyFull, None)
            } else {
                continue;
            };

            rejected_account_count = rejected_account_count.saturating_add(1);
            *reason_counts.entry(reason).or_insert(0) += 1;

            if sampled_accounts.len() < SELECTION_FAILURE_SAMPLE_LIMIT {
                sampled_accounts.push(RejectedAccountSample {
                    account_id: entry.id,
                    reason,
                    rpm_limit: Some(effective_rpm(entry, global_rpm)),
                    in_flight: Some(entry.in_flight_requests),
                    max_concurrent: Some(effective_max_concurrent_requests(
                        entry,
                        config.credential_max_concurrent_requests,
                    )),
                    cooldown_remaining_ms: cooldown_remaining
                        .map(|duration| duration.as_millis().min(u64::MAX as u128) as u64),
                });
            }
        }

        (
            reason_counts,
            sampled_accounts,
            rejected_account_count,
            waitable_account_count,
        )
    }

    fn compute_local_pool_route_state(&self, model: Option<&str>) -> LocalPoolRouteState {
        if let Err(err) = self.refresh_scheduler_state_from_redis() {
            tracing::warn!("本地池路由预检同步 Redis 调度状态失败: {}", err);
        }
        self.cleanup_expired_in_flight_leases_local_first();

        let entries = self.entries.lock();
        let proxy_resources = self.proxy_resources.lock();
        let config = self.config.lock().clone();
        let now = Instant::now();
        let total = entries.len();
        let available = entries.iter().filter(|entry| !entry.disabled).count();
        let max_concurrent_requests = config.credential_max_concurrent_requests;
        let global_in_flight_requests = entries
            .iter()
            .map(|entry| entry.in_flight_requests)
            .sum::<u32>();
        let global_has_capacity = global_has_concurrency_capacity(
            global_in_flight_requests,
            config.dispatch_global_max_concurrent_requests,
        );
        let mut model_usable = 0usize;
        let mut usable = 0usize;
        let mut proxy_blocked = 0usize;
        let mut dispatchable = 0usize;
        let mut cooldown_blocked = 0usize;
        let mut rate_limit_blocked = 0usize;
        let mut concurrency_blocked = 0usize;
        let mut wait_for: Option<StdDuration> = None;
        let mut effective_concurrency_range: Option<(u32, u32)> = None;

        for entry in entries.iter() {
            if !credential_is_usable_for_model(entry, model) {
                continue;
            }
            model_usable += 1;

            if !credential_proxy_is_dispatchable(&entry.credentials, &proxy_resources) {
                proxy_blocked += 1;
                continue;
            }

            usable += 1;
            let effective_concurrency =
                effective_max_concurrent_requests(entry, max_concurrent_requests);
            effective_concurrency_range = Some(match effective_concurrency_range {
                Some((min, max)) => (
                    min.min(effective_concurrency),
                    max.max(effective_concurrency),
                ),
                None => (effective_concurrency, effective_concurrency),
            });

            let cooldown_remaining = entry_cooldown_remaining(entry, model, now);
            let rate_limit_remaining = entry_rate_limit_remaining(entry, now);

            if let Some(remaining) = cooldown_remaining {
                cooldown_blocked += 1;
                let blocking_wait = rate_limit_remaining
                    .map(|rate_limit| remaining.max(rate_limit))
                    .unwrap_or(remaining);
                wait_for =
                    Some(wait_for.map_or(blocking_wait, |current| current.min(blocking_wait)));
                continue;
            }
            if let Some(remaining) = rate_limit_remaining {
                rate_limit_blocked += 1;
                wait_for = Some(wait_for.map_or(remaining, |current| current.min(remaining)));
                continue;
            }

            if !global_has_capacity
                || !entry_has_concurrency_capacity(entry, max_concurrent_requests)
            {
                concurrency_blocked += 1;
                continue;
            }

            dispatchable += 1;
        }

        let dispatch_candidate_count = usable;
        let retry_after_secs = wait_for
            .map(|duration| duration.as_secs().saturating_add(1))
            .filter(|value| *value > 0);
        let effective_credential_max_concurrent_requests =
            format_effective_concurrency_range(effective_concurrency_range);

        let kind = if total == 0 {
            LocalPoolRouteStateKind::NoCredentials
        } else if available == 0 {
            LocalPoolRouteStateKind::AllDisabled
        } else if model.is_some() && model_usable == 0 {
            LocalPoolRouteStateKind::NoModelCompatible
        } else if model_usable > 0 && usable == 0 && proxy_blocked >= model_usable {
            LocalPoolRouteStateKind::ProxyBlocked
        } else if dispatchable > 0 {
            LocalPoolRouteStateKind::Ready
        } else if dispatch_candidate_count > 0
            && cooldown_blocked.saturating_add(rate_limit_blocked) >= dispatch_candidate_count
        {
            LocalPoolRouteStateKind::AllCoolingDown
        } else {
            LocalPoolRouteStateKind::CapacityFull
        };

        LocalPoolRouteState {
            kind,
            total,
            available,
            model_usable,
            usable,
            dispatchable,
            proxy_blocked,
            cooldown_blocked,
            rate_limit_blocked,
            concurrency_blocked,
            global_in_flight_requests,
            global_max_concurrent_requests: config.dispatch_global_max_concurrent_requests,
            global_credential_max_concurrent_requests: max_concurrent_requests,
            effective_credential_max_concurrent_requests,
            queued_requests: self.queued_requests.load(Ordering::Relaxed),
            max_queued_requests: config.dispatch_max_queued_requests,
            retry_after_secs,
        }
    }

    fn auto_heal_too_many_failures_if_applicable(&self) -> bool {
        let healed_ids = {
            let mut entries = self.entries.lock();
            if !entries.iter().any(|entry| {
                entry.disabled && entry.disabled_reason == Some(DisabledReason::TooManyFailures)
            }) {
                return false;
            }

            tracing::warn!(
                "所有可调度凭据均不可用且存在连续失败自动禁用凭据，执行自愈：重置失败计数并重新启用"
            );

            let mut healed_ids = Vec::new();
            for entry in entries.iter_mut() {
                if entry.disabled_reason == Some(DisabledReason::TooManyFailures) {
                    entry.disabled = false;
                    entry.disabled_reason = None;
                    entry.failure_count = 0;
                    healed_ids.push(entry.id);
                }
            }
            healed_ids
        };

        if healed_ids.is_empty() {
            return false;
        }

        for healed_id in healed_ids {
            self.queue_runtime_state_flush(healed_id, None, Utc::now());
            self.save_runtime_state_for(healed_id);
            self.record_scheduler_credential_audit(
                "auto_enable_credential",
                healed_id,
                DisabledReason::TooManyFailures,
                "auto_heal_all_too_many_failures",
                "所有可调度凭据均因连续失败自动禁用，调度器已自动恢复该凭据",
                serde_json::json!({
                    "previousReason": DisabledReason::TooManyFailures.as_str(),
                }),
            );
        }
        self.invalidate_local_pool_route_state_cache();
        self.notify_dispatch_state_changed();
        self.publish_credentials_changed("auto_heal_too_many_failures");
        true
    }

    fn max_concurrent_requests(&self) -> u32 {
        self.config.lock().credential_max_concurrent_requests
    }

    fn effective_max_concurrent_requests_for_id(
        &self,
        id: u64,
        global_max_concurrent_requests: u32,
    ) -> u32 {
        let entries = self.entries.lock();
        entries
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| effective_max_concurrent_requests(entry, global_max_concurrent_requests))
            .unwrap_or(global_max_concurrent_requests)
    }

    fn global_max_concurrent_requests(&self) -> u32 {
        self.config.lock().dispatch_global_max_concurrent_requests
    }

    fn max_queued_requests(&self) -> u32 {
        self.config.lock().dispatch_max_queued_requests
    }

    fn scheduler_redis_hot_path_allowed(&self) -> bool {
        if self.redis_store.is_none() {
            return false;
        }
        let now = Instant::now();
        let mut degraded_until = self.scheduler_redis_degraded_until.lock();
        match *degraded_until {
            Some(until) if until > now => false,
            Some(_) => {
                *degraded_until = None;
                true
            }
            None => true,
        }
    }

    fn mark_scheduler_redis_degraded(&self, operation: &'static str, err: &anyhow::Error) {
        *self.scheduler_redis_degraded_until.lock() =
            Some(Instant::now() + SCHEDULER_REDIS_DEGRADED_BACKOFF);
        tracing::warn!(
            operation,
            timeout_ms = SCHEDULER_REDIS_HOT_OP_TIMEOUT.as_millis() as u64,
            backoff_ms = SCHEDULER_REDIS_DEGRADED_BACKOFF.as_millis() as u64,
            "Redis 调度热路径不可用，本进程暂时降级为本地调度: {}",
            err
        );
    }

    fn block_on_scheduler_redis_hot<T>(
        &self,
        operation: &'static str,
        future: impl Future<Output = anyhow::Result<T>>,
    ) -> Option<T> {
        if !self.scheduler_redis_hot_path_allowed() {
            return None;
        }
        let result = block_on_storage(operation, async move {
            match tokio::time::timeout(SCHEDULER_REDIS_HOT_OP_TIMEOUT, future).await {
                Ok(result) => result,
                Err(_) => anyhow::bail!(
                    "{}超过 {}ms",
                    operation,
                    SCHEDULER_REDIS_HOT_OP_TIMEOUT.as_millis()
                ),
            }
        });
        match result {
            Ok(value) => {
                *self.scheduler_redis_degraded_until.lock() = None;
                Some(value)
            }
            Err(err) => {
                self.mark_scheduler_redis_degraded(operation, &err);
                None
            }
        }
    }

    fn refresh_lock_for_credential(&self, id: u64) -> Arc<TokioMutex<()>> {
        let mut locks = self.refresh_locks.lock();
        locks
            .entry(id)
            .or_insert_with(|| Arc::new(TokioMutex::new(())))
            .clone()
    }

    fn retain_refresh_locks_for_ids(&self, ids: &HashSet<u64>) {
        self.refresh_locks.lock().retain(|id, _| ids.contains(id));
    }

    fn remove_refresh_lock_for_credential(&self, id: u64) {
        self.refresh_locks.lock().remove(&id);
    }

    fn local_global_capacity_state(&self) -> SchedulerGlobalCapacityState {
        let in_flight_requests = self
            .entries
            .lock()
            .iter()
            .map(|entry| entry.in_flight_requests)
            .sum();
        SchedulerGlobalCapacityState {
            in_flight_requests,
            queued_requests: self.queued_requests.load(Ordering::Acquire),
        }
    }

    fn acquire_local_in_flight_slot_with_id(
        &self,
        id: u64,
        lease_id: u64,
        now: Instant,
        max_concurrent_requests: u32,
        global_max_concurrent_requests: u32,
    ) -> bool {
        let mut entries = self.entries.lock();
        let global_in_flight: u32 = entries.iter().map(|entry| entry.in_flight_requests).sum();
        if global_max_concurrent_requests > 0 && global_in_flight >= global_max_concurrent_requests
        {
            return false;
        }
        if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
            if !entry_has_concurrency_capacity(entry, max_concurrent_requests) {
                return false;
            }
            entry.in_flight_requests = entry.in_flight_requests.saturating_add(1);
            entry.in_flight_leases.push(InFlightLease {
                id: lease_id,
                acquired_at: now,
                last_seen_at: now,
                kind: InFlightKind::Api,
            });
            return true;
        }
        false
    }

    fn try_enter_local_dispatch_queue(&self, max_queued: u32) -> bool {
        loop {
            let queued = self.queued_requests.load(Ordering::Acquire);
            if max_queued > 0 && queued >= max_queued {
                return false;
            }
            if self
                .queued_requests
                .compare_exchange(queued, queued + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return true;
            }
        }
    }

    fn in_flight_lease_max_age(&self) -> Option<StdDuration> {
        let secs = self.config.lock().credential_in_flight_lease_max_secs;
        (secs > 0).then(|| StdDuration::from_secs(secs))
    }

    fn transient_failure_settings(
        &self,
        kind: TransientFailureKind,
    ) -> (StdDuration, StdDuration, f64, f64, StdDuration, f64) {
        let config = self.config.lock();
        let base = match kind {
            TransientFailureKind::RateLimit => config.credential_rate_limit_cooldown_secs,
            TransientFailureKind::Server => config.credential_server_error_cooldown_secs,
            TransientFailureKind::Network => config.credential_network_error_cooldown_secs,
            TransientFailureKind::Stream => config.credential_stream_error_cooldown_secs,
            TransientFailureKind::Protocol => config.credential_protocol_error_cooldown_secs,
            TransientFailureKind::Auth => config.credential_auth_error_cooldown_secs,
        }
        .max(1);
        let max = config.credential_max_cooldown_secs.max(1);
        let multiplier = config.credential_cooldown_backoff_multiplier.max(1.0);
        let jitter_percent = config.credential_cooldown_jitter_percent.min(100) as f64 / 100.0;
        let offset = if jitter_percent == 0.0 {
            0.0
        } else {
            fastrand::f64() * jitter_percent * 2.0 - jitter_percent
        };
        (
            StdDuration::from_secs(base),
            StdDuration::from_secs(max),
            multiplier,
            1.0 + offset,
            StdDuration::from_secs(config.credential_probation_secs),
            config.scheduler_error_ewma_alpha.clamp(0.01, 1.0),
        )
    }

    fn local_cooldown_duration(
        retry_after: Option<StdDuration>,
        base: StdDuration,
        max: StdDuration,
        multiplier: f64,
        jitter: f64,
        streak: u32,
    ) -> StdDuration {
        let requested = retry_after.unwrap_or_else(|| {
            let millis = base.as_millis() as f64
                * multiplier.powi(streak.saturating_sub(1).min(20) as i32)
                * jitter;
            StdDuration::from_millis(millis.max(1.0).round() as u64)
        });
        requested.clamp(StdDuration::from_secs(1), max)
    }

    fn update_credential_rpm_from_config(&self) {
        let global_rpm = self.config.lock().credential_rpm.unwrap_or(0);
        let ids: Vec<u64> = {
            let mut entries = self.entries.lock();
            entries
                .iter_mut()
                .filter_map(|entry| {
                    let rpm = effective_rpm(entry, global_rpm);
                    if rate_limit_interval_for_rpm(rpm).is_some() {
                        return None;
                    }
                    if entry.rate_limit_available_at.is_none() {
                        return None;
                    }
                    let id = entry.id;
                    entry.rate_limit_available_at = None;
                    Some(id)
                })
                .collect()
        };
        if ids.is_empty() {
            return;
        }
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            spawn_best_effort_storage_task("清理 Redis 凭据限流状态", async move {
                for id in ids {
                    redis.clear_rate_limit(id).await?;
                }
                Ok(())
            });
        }
    }

    fn clear_rate_limit_for_credential(&self, id: u64) {
        {
            let mut entries = self.entries.lock();
            if let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) {
                entry.rate_limit_available_at = None;
            }
        }
        self.invalidate_local_pool_route_state_cache();
        let Some(redis) = &self.redis_store else {
            return;
        };
        let redis = redis.clone();
        spawn_best_effort_storage_task("清理 Redis 凭据限流状态", async move {
            redis.clear_rate_limit(id).await?;
            Ok(())
        });
    }

    fn mark_rate_limited_at(&self, id: u64, now: Instant) -> anyhow::Result<()> {
        let global_rpm = self.config.lock().credential_rpm.unwrap_or(0);
        let interval = {
            let mut entries = self.entries.lock();
            let Some(entry) = entries.iter_mut().find(|e| e.id == id) else {
                return Ok(());
            };
            let rpm = effective_rpm(entry, global_rpm);
            let Some(interval) = rate_limit_interval_for_rpm(rpm) else {
                entry.rate_limit_available_at = None;
                return Ok(());
            };
            let base = entry
                .rate_limit_available_at
                .filter(|at| *at > now)
                .unwrap_or(now);
            entry.rate_limit_available_at = Some(base + interval);
            interval
        };
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            let _ = self.block_on_scheduler_redis_hot("更新 Redis 凭据限流状态", async move {
                redis.bump_rate_limit_available_at(id, interval).await
            });
        }
        Ok(())
    }

    fn acquire_in_flight_slot(&self, id: u64) -> anyhow::Result<Option<InFlightLeaseGuard>> {
        self.cleanup_expired_in_flight_leases_local_first();
        let max_concurrent_requests = self.max_concurrent_requests();
        let effective_max_concurrent_requests =
            self.effective_max_concurrent_requests_for_id(id, max_concurrent_requests);
        let global_max_concurrent_requests = self.global_max_concurrent_requests();
        let now = Instant::now();

        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            let max_age = self.in_flight_lease_max_age();
            let redis_acquire =
                self.block_on_scheduler_redis_hot("占用 Redis 凭据并发槽", async move {
                    redis
                        .acquire_dispatch_lease_with_new_id(
                            id,
                            effective_max_concurrent_requests,
                            global_max_concurrent_requests,
                            max_age,
                            InFlightKind::Api.as_str(),
                        )
                        .await
                });
            match redis_acquire {
                Some(Some((lease_id, _count))) => {
                    if self.acquire_local_in_flight_slot_with_id(
                        id,
                        lease_id,
                        now,
                        max_concurrent_requests,
                        global_max_concurrent_requests,
                    ) {
                        self.invalidate_local_pool_route_state_cache();
                        return Ok(Some(InFlightLeaseGuard::new(
                            self.entries.clone(),
                            self.redis_store.clone(),
                            self.in_flight_notify.clone(),
                            self.local_pool_route_state_cache.clone(),
                            id,
                            lease_id,
                        )));
                    }
                    if let Some(redis) = &self.redis_store {
                        let redis = redis.clone();
                        spawn_best_effort_storage_task(
                            "释放本地竞争失败的 Redis 并发 lease",
                            async move {
                                redis.release_in_flight_lease(id, lease_id).await?;
                                Ok(())
                            },
                        );
                    }
                    return Ok(None);
                }
                Some(None) => return Ok(None),
                None => {}
            }
        }

        let lease_id = self.next_in_flight_lease_id.fetch_add(1, Ordering::Relaxed);
        if self.acquire_local_in_flight_slot_with_id(
            id,
            lease_id,
            now,
            max_concurrent_requests,
            global_max_concurrent_requests,
        ) {
            self.invalidate_local_pool_route_state_cache();
            return Ok(Some(InFlightLeaseGuard::new(
                self.entries.clone(),
                None,
                self.in_flight_notify.clone(),
                self.local_pool_route_state_cache.clone(),
                id,
                lease_id,
            )));
        }
        Ok(None)
    }

    #[cfg(test)]
    pub(crate) fn acquire_in_flight_lease_for_test(&self, id: u64) -> Option<InFlightLeaseGuard> {
        self.acquire_in_flight_slot(id).unwrap()
    }

    #[cfg(test)]
    pub(crate) fn age_in_flight_lease_for_test(
        &self,
        credential_id: u64,
        lease_id: u64,
        age: StdDuration,
    ) {
        let mut entries = self.entries.lock();
        if let Some(entry) = entries.iter_mut().find(|e| e.id == credential_id) {
            if let Some(lease) = entry
                .in_flight_leases
                .iter_mut()
                .find(|lease| lease.id == lease_id)
            {
                let now = Instant::now();
                lease.acquired_at = now - age;
                lease.last_seen_at = now - age;
            }
        }
    }

    #[cfg(test)]
    pub fn cleanup_expired_in_flight_leases(&self) -> usize {
        match self.cleanup_expired_in_flight_leases_result() {
            Ok(cleaned) => cleaned,
            Err(err) => {
                tracing::warn!("清理超时并发 lease 失败: {}", err);
                0
            }
        }
    }

    #[cfg(test)]
    fn cleanup_expired_in_flight_leases_result(&self) -> anyhow::Result<usize> {
        let Some(max_age) = self.in_flight_lease_max_age() else {
            return Ok(0);
        };
        if let Some(redis) = &self.redis_store {
            let ids: Vec<u64> = {
                let entries = self.entries.lock();
                entries.iter().map(|entry| entry.id).collect()
            };
            let redis = redis.clone();
            let cleaned = block_on_storage("清理 Redis 超时并发 lease", async move {
                redis.cleanup_expired_in_flight_leases(&ids, max_age).await
            })?;
            if cleaned > 0 {
                tracing::warn!(
                    removed = cleaned,
                    max_age_secs = max_age.as_secs(),
                    "清理超时未释放的 Redis 凭据并发占用 lease"
                );
                self.notify_dispatch_state_changed();
                self.refresh_scheduler_state_from_redis_force()?;
            }
            return Ok(cleaned);
        }
        Ok(self.cleanup_expired_local_in_flight_leases(max_age, Instant::now()))
    }

    fn cleanup_expired_local_in_flight_leases(&self, max_age: StdDuration, now: Instant) -> usize {
        let mut cleaned = 0usize;
        {
            let mut entries = self.entries.lock();
            for entry in entries.iter_mut() {
                let before = entry.in_flight_leases.len();
                entry
                    .in_flight_leases
                    .retain(|lease| now.saturating_duration_since(lease.last_seen_at) <= max_age);
                let removed = before.saturating_sub(entry.in_flight_leases.len());
                if removed > 0 {
                    cleaned += removed;
                    entry.in_flight_requests =
                        entry.in_flight_requests.saturating_sub(removed as u32);
                    tracing::warn!(
                        credential_id = entry.id,
                        removed,
                        max_age_secs = max_age.as_secs(),
                        "清理超时未释放的凭据并发占用 lease"
                    );
                }
            }
        }

        if cleaned > 0 {
            self.notify_dispatch_state_changed();
        }
        cleaned
    }

    fn spawn_redis_expired_in_flight_cleanup(&self, max_age: StdDuration) {
        let Some(redis) = &self.redis_store else {
            return;
        };
        let redis = redis.clone();
        let ids: Vec<u64> = {
            let entries = self.entries.lock();
            entries.iter().map(|entry| entry.id).collect()
        };
        spawn_best_effort_storage_task("清理 Redis 超时并发 lease", async move {
            let removed = redis
                .cleanup_expired_in_flight_leases(&ids, max_age)
                .await?;
            if removed > 0 {
                let payload = serde_json::json!({
                    "kind": "dispatch_wakeup",
                    "removed": removed,
                    "changedAt": Utc::now().to_rfc3339(),
                })
                .to_string();
                redis.publish_dispatch_wakeup(payload).await?;
            }
            Ok(())
        });
    }

    fn cleanup_expired_in_flight_leases_throttled(&self) -> anyhow::Result<usize> {
        let now = Instant::now();
        {
            let mut last_cleanup_at = self.last_scheduler_redis_cleanup_at.lock();
            if last_cleanup_at.is_some_and(|last| {
                now.saturating_duration_since(last) < SCHEDULER_REDIS_CLEANUP_MIN_INTERVAL
            }) {
                return Ok(0);
            }
            *last_cleanup_at = Some(now);
        }

        let mut cleaned = 0usize;
        if let Some(max_age) = self.in_flight_lease_max_age() {
            cleaned = self.cleanup_expired_local_in_flight_leases(max_age, now);
            self.spawn_redis_expired_in_flight_cleanup(max_age);
        }
        Ok(cleaned)
    }

    fn cleanup_expired_in_flight_leases_local_first(&self) {
        if let Some(max_age) = self.in_flight_lease_max_age() {
            let cleaned = self.cleanup_expired_local_in_flight_leases(max_age, Instant::now());
            if cleaned > 0 {
                self.spawn_redis_expired_in_flight_cleanup(max_age);
                return;
            }
        }
        if let Err(err) = self.cleanup_expired_in_flight_leases_throttled() {
            tracing::warn!("快照读取前清理超时并发 lease 失败: {}", err);
        }
    }

    pub fn clear_in_flight_leases(
        &self,
        credential_id: u64,
        min_idle: Option<StdDuration>,
    ) -> usize {
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            match block_on_storage("清理 Redis 凭据并发占用", async move {
                redis.clear_in_flight_leases(credential_id, min_idle).await
            }) {
                Ok(cleared) => {
                    self.refresh_scheduler_state_from_redis_force_best_effort();
                    if cleared > 0 {
                        self.notify_dispatch_state_changed();
                    }
                    return cleared;
                }
                Err(err) => tracing::warn!(credential_id, "清理 Redis 凭据并发占用失败: {}", err),
            }
            return 0;
        }
        let now = Instant::now();
        let mut cleared = 0usize;
        {
            let mut entries = self.entries.lock();
            if let Some(entry) = entries.iter_mut().find(|e| e.id == credential_id) {
                let before = entry.in_flight_leases.len();
                match min_idle {
                    Some(min_idle) => {
                        entry.in_flight_leases.retain(|lease| {
                            now.saturating_duration_since(lease.last_seen_at) < min_idle
                        });
                    }
                    None => {
                        entry.in_flight_leases.clear();
                    }
                }
                cleared = before.saturating_sub(entry.in_flight_leases.len());
                if cleared > 0 {
                    entry.in_flight_requests =
                        entry.in_flight_requests.saturating_sub(cleared as u32);
                }
            }
        }
        if cleared > 0 {
            self.notify_dispatch_state_changed();
        }
        cleared
    }

    async fn wait_for_dispatch_capacity(
        &self,
        wait_for: Option<StdDuration>,
        max_wait_remaining: Option<StdDuration>,
    ) {
        let fallback = if self.redis_store.is_some() {
            StdDuration::from_secs(1)
        } else {
            StdDuration::from_secs(CONCURRENCY_WAIT_WAKEUP_SECS)
        };
        let mut wakeup = wait_for.unwrap_or(fallback).min(fallback);
        if let Some(remaining) = max_wait_remaining {
            wakeup = wakeup.min(remaining);
        }
        if wakeup.is_zero() {
            return;
        }
        let _ = tokio::time::timeout(wakeup, self.in_flight_notify.notified()).await;
    }

    fn try_enter_dispatch_queue(&self) -> anyhow::Result<Option<DispatchQueueGuard>> {
        let max_queued = self.max_queued_requests();
        if !self.try_enter_local_dispatch_queue(max_queued) {
            return Ok(None);
        }
        self.invalidate_local_pool_route_state_cache();
        let mut redis_admitted = false;
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            let ttl_secs = self
                .config
                .lock()
                .credential_dispatch_max_wait_secs
                .saturating_add(60)
                .max(60);
            if let Some(admitted) = self
                .block_on_scheduler_redis_hot("占用 Redis 调度排队名额", async move {
                    redis.try_enter_dispatch_queue(max_queued, ttl_secs).await
                })
            {
                if !admitted {
                    self.queued_requests.fetch_sub(1, Ordering::AcqRel);
                    return Ok(None);
                }
                redis_admitted = true;
            }
        }
        Ok(Some(DispatchQueueGuard::new(
            self.redis_store.clone(),
            self.queued_requests.clone(),
            self.in_flight_notify.clone(),
            self.local_pool_route_state_cache.clone(),
            redis_admitted,
        )))
    }

    fn global_capacity_state(&self) -> SchedulerGlobalCapacityState {
        self.local_global_capacity_state()
    }

    fn dispatch_max_wait(&self, acquire_mode: AcquireMode) -> Option<StdDuration> {
        if let Some(max_wait) = acquire_mode.max_wait_override() {
            return Some(max_wait);
        }
        let secs = self.config.lock().credential_dispatch_max_wait_secs;
        (secs > 0).then(|| StdDuration::from_secs(secs))
    }

    fn dispatch_wait_exceeded(
        &self,
        acquire_mode: AcquireMode,
        started_at: Instant,
        now: Instant,
    ) -> Option<(StdDuration, StdDuration)> {
        let max_wait = self.dispatch_max_wait(acquire_mode)?;
        let waited = now.saturating_duration_since(started_at);
        (waited >= max_wait).then_some((waited, max_wait))
    }

    fn dispatch_wait_remaining(
        &self,
        acquire_mode: AcquireMode,
        started_at: Instant,
        now: Instant,
    ) -> Option<StdDuration> {
        let max_wait = self.dispatch_max_wait(acquire_mode)?;
        Some(max_wait.saturating_sub(now.saturating_duration_since(started_at)))
    }

    /// 判断排除当前凭据后，本次请求是否还有其他可用凭据可 fallback。
    pub fn has_alternate_usable_credential(
        &self,
        model: Option<&str>,
        excluded_ids: &HashSet<u64>,
        current_id: u64,
    ) -> bool {
        if let Err(err) = self.refresh_scheduler_state_from_redis() {
            tracing::warn!("判断备选凭据前同步 Redis 调度状态失败: {}", err);
            return false;
        }
        let mut entries = self.entries.lock();
        let now = Instant::now();
        let max_concurrent_requests = self.max_concurrent_requests();
        if self.redis_store.is_none() {
            refresh_local_selection_windows_locked(&mut entries, now);
        }
        let proxy_resources = self.proxy_resources.lock();
        entries.iter().any(|entry| {
            entry.id != current_id
                && !excluded_ids.contains(&entry.id)
                && credential_is_dispatchable(
                    &proxy_resources,
                    entry,
                    model,
                    now,
                    max_concurrent_requests,
                )
        })
    }

    /// 根据负载均衡模式选择下一个凭据，并排除本次请求已临时失败的凭据。
    fn select_next_credential_excluding(
        &self,
        model: Option<&str>,
        excluded_ids: &HashSet<u64>,
    ) -> Option<(u64, KiroCredentials)> {
        if let Err(err) = self.refresh_scheduler_state_from_redis() {
            tracing::warn!("选择凭据前同步 Redis 调度状态失败: {}", err);
            return None;
        }
        let entries = self.entries.lock();
        let proxy_resources = self.proxy_resources.lock();
        let now = Instant::now();
        let config = self.config.lock().clone();
        let max_concurrent_requests = config.credential_max_concurrent_requests;
        let global_in_flight = entries
            .iter()
            .map(|entry| entry.in_flight_requests)
            .sum::<u32>();
        let global_has_capacity = global_has_concurrency_capacity(
            global_in_flight,
            config.dispatch_global_max_concurrent_requests,
        );

        let mut available = Vec::new();
        let mut ready = Vec::new();
        let mut warming = Vec::new();
        let mut total_recent = 0u64;
        let mut warming_recent = 0u64;

        if global_has_capacity {
            for entry in entries.iter() {
                if excluded_ids.contains(&entry.id)
                    || !credential_is_dispatchable(
                        &proxy_resources,
                        entry,
                        model,
                        now,
                        max_concurrent_requests,
                    )
                {
                    continue;
                }

                let recent = entry.health.recent_selection_count_60s as u64;
                total_recent = total_recent.saturating_add(recent);
                available.push(entry);
                if entry.warmup_remaining > 0 {
                    warming_recent = warming_recent.saturating_add(recent);
                    warming.push(entry);
                } else {
                    ready.push(entry);
                }
            }
        }

        if available.is_empty() {
            return None;
        }

        let select_warming = should_select_warming_from_totals(
            &config,
            ready.len(),
            warming.len(),
            total_recent,
            warming_recent,
        );
        let candidates = if select_warming { &warming } else { &ready };
        let mode = self.load_balancing_mode.lock().clone();

        match mode.as_str() {
            "health_balanced" => {
                // 预热不是后台主动打流量，而是在真实业务请求中按目标份额参与调度；
                // 份额按预热账号数量放大并受最大预热份额限制，避免批量导入后长期吃不到流量。
                let entry = select_health_weighted(
                    candidates,
                    model,
                    Utc::now().timestamp_millis(),
                    &config,
                )?;

                Some((entry.id, entry.credentials.clone()))
            }
            "balanced" => {
                let entry = candidates
                    .iter()
                    .min_by_key(|entry| balanced_selection_key(entry))?;
                Some((entry.id, entry.credentials.clone()))
            }
            _ => {
                // priority 模式（默认）：优先级仍是第一排序，但同优先级账号优先选低并发。
                let entry = available.iter().min_by_key(|e| priority_selection_key(e))?;
                Some((entry.id, entry.credentials.clone()))
            }
        }
    }

    fn get_bound_credential(
        &self,
        session_id: &str,
        model: Option<&str>,
        excluded_ids: &HashSet<u64>,
    ) -> Option<(u64, KiroCredentials)> {
        if let Err(err) = self.refresh_scheduler_state_from_redis() {
            tracing::warn!("读取会话绑定前同步 Redis 调度状态失败: {}", err);
        }
        let bound_id = {
            let mut bindings = self.session_bindings.lock();
            prune_session_bindings_locked(&mut bindings);
            bindings
                .get(session_id)
                .map(|binding| binding.credential_id)
        }?;

        if excluded_ids.contains(&bound_id) {
            return None;
        }

        let entries = self.entries.lock();
        let proxy_resources = self.proxy_resources.lock();
        let now = Instant::now();
        let max_concurrent_requests = self.max_concurrent_requests();
        entries
            .iter()
            .find(|e| {
                e.id == bound_id
                    && credential_is_dispatchable(
                        &proxy_resources,
                        e,
                        model,
                        now,
                        max_concurrent_requests,
                    )
            })
            .map(|e| (e.id, e.credentials.clone()))
    }

    fn bound_credential_id(&self, session_id: &str) -> Option<u64> {
        sticky_bound_credential_id(&self.session_bindings, session_id)
    }

    fn bound_credential_exists_but_unusable(&self, session_id: &str, model: Option<&str>) -> bool {
        if let Err(err) = self.refresh_scheduler_state_from_redis() {
            tracing::warn!("检查会话绑定可用性前同步 Redis 调度状态失败: {}", err);
        }
        let Some(bound_id) = self.bound_credential_id(session_id) else {
            return false;
        };

        let entries = self.entries.lock();
        let proxy_resources = self.proxy_resources.lock();
        let now = Instant::now();
        entries
            .iter()
            .find(|e| e.id == bound_id)
            .is_none_or(|e| !credential_is_temporarily_available(&proxy_resources, e, model, now))
    }

    fn bind_session_to_credential(&self, session_id: &str, credential_id: u64) {
        bind_sticky_session_to_credential(
            &self.session_bindings,
            self.redis_store.as_ref(),
            session_id,
            credential_id,
        );
    }

    /// 清理指定会话的粘性绑定。
    pub fn unbind_session(&self, session_id: &str) {
        unbind_sticky_session(
            &self.session_bindings,
            self.redis_store.as_ref(),
            session_id,
        );
    }

    /// 仅当指定会话当前绑定到该凭据时清理绑定。
    pub fn unbind_session_if_bound_to(&self, session_id: &str, credential_id: u64) {
        unbind_sticky_session_if_bound_to(
            &self.session_bindings,
            self.redis_store.as_ref(),
            session_id,
            credential_id,
        );
    }

    /// 清理某个凭据关联的所有会话绑定。
    pub fn unbind_sessions_for_credential(&self, credential_id: u64) {
        unbind_sticky_sessions_for_credential(
            &self.session_bindings,
            self.redis_store.as_ref(),
            credential_id,
        );
    }

    /// 记录绑定账号的一次软失败。返回 true 表示本次请求可以临时 fallback。
    pub fn record_session_soft_failure(&self, session_id: &str, credential_id: u64) -> bool {
        record_sticky_session_soft_failure(
            &self.session_bindings,
            self.redis_store.as_ref(),
            session_id,
            credential_id,
        )
    }

    /// 清理绑定账号的软失败计数。
    pub fn clear_session_soft_failure(&self, session_id: &str, credential_id: u64) {
        clear_sticky_session_soft_failure(
            &self.session_bindings,
            self.redis_store.as_ref(),
            session_id,
            credential_id,
        );
    }

    /// 获取 API 调用上下文
    ///
    /// 返回绑定了 id、credentials 和 token 的调用上下文
    /// 确保整个 API 调用过程中使用一致的凭据信息
    ///
    /// 如果 Token 过期或即将过期，会自动刷新
    /// Token 刷新失败会累计到当前凭据，达到阈值后禁用并切换
    ///
    /// # 参数
    /// - `model`: 可选的模型名称，用于过滤支持该模型的凭据（如 opus 模型需要付费订阅）
    #[cfg(test)]
    pub async fn acquire_context(&self, model: Option<&str>) -> anyhow::Result<CallContext> {
        self.acquire_context_for_session(model, None, &HashSet::new())
            .await
    }

    /// 获取指定凭据的 API 调用上下文，不参与负载均衡或会话绑定。
    ///
    /// 仅用于 Admin 手动模型调用测试；即使凭据已被禁用，也允许验证一次上游模型调用。
    pub async fn acquire_context_for_credential(&self, id: u64) -> anyhow::Result<CallContext> {
        let credentials = {
            let entries = self.entries.lock();
            let entry = entries
                .iter()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            entry.credentials.clone()
        };

        self.try_ensure_token(id, &credentials, false).await
    }

    /// 获取 API 调用上下文，优先保持同一会话使用同一个凭据。
    ///
    /// `excluded_ids` 只作用于本次请求，用于 sticky-aware retry 的临时 fallback。
    pub async fn acquire_context_for_session(
        &self,
        model: Option<&str>,
        session_id: Option<&str>,
        excluded_ids: &HashSet<u64>,
    ) -> anyhow::Result<CallContext> {
        self.acquire_context_for_session_with_mode(
            model,
            session_id,
            excluded_ids,
            AcquireMode::WaitForCapacity,
        )
        .await
    }

    /// 获取 API 调用上下文，可选择在本地容量不足时立即返回。
    ///
    /// `FailFastOnCapacity` 仅用于外部备用池预检：如果无法立即拿到本地凭据并发
    /// lease，不进入本地等待队列，让上层可以直接路由到外部池。默认调用仍应使用
    /// [`Self::acquire_context_for_session`]，保持原有等待/排队语义。
    pub async fn acquire_context_for_session_with_mode(
        &self,
        model: Option<&str>,
        session_id: Option<&str>,
        excluded_ids: &HashSet<u64>,
        acquire_mode: AcquireMode,
    ) -> anyhow::Result<CallContext> {
        enum AcquireDecision {
            Selected(u64, KiroCredentials, bool, bool),
            WaitForDispatch {
                available: usize,
                total: usize,
                global_credential_max_concurrent_requests: u32,
                effective_credential_max_concurrent_requests: String,
                wait_for: Option<StdDuration>,
            },
        }

        let total = self.total_count();
        let max_attempts = (total * MAX_FAILURES_PER_CREDENTIAL as usize).max(1);
        let mut attempt_count = 0;
        let dispatch_wait_started_at = Instant::now();
        let mut queue_guard: Option<DispatchQueueGuard> = None;
        let mut local_excluded_ids = excluded_ids.clone();
        let mut slot_race_excluded_count = 0usize;

        loop {
            self.refresh_scheduler_state_from_redis()?;
            self.cleanup_expired_in_flight_leases_local_first();
            if attempt_count >= max_attempts {
                anyhow::bail!(
                    "所有凭据均无法获取有效 Token（可用: {}/{}）",
                    self.available_count(),
                    total
                );
            }

            let decision = {
                let existing_bound_id = session_id.and_then(|sid| self.bound_credential_id(sid));
                let bound_hit = session_id
                    .and_then(|sid| self.get_bound_credential(sid, model, &local_excluded_ids));

                if let Some(hit) = bound_hit {
                    AcquireDecision::Selected(hit.0, hit.1, true, false)
                } else {
                    let fallback_from_sticky = existing_bound_id.is_some();
                    {
                        // 根据负载均衡策略选择；priority 模式也会在同优先级账号之间优先低并发。
                        let mut best =
                            self.select_next_credential_excluding(model, &local_excluded_ids);

                        // 没有可用凭据：如果是"自动禁用导致全灭"，做一次类似重启的自愈
                        if best.is_none() {
                            if self.auto_heal_too_many_failures_if_applicable() {
                                best = self
                                    .select_next_credential_excluding(model, &local_excluded_ids);
                            }
                        }

                        if let Some((new_id, new_creds)) = best {
                            // 更新 current_id
                            let mut current_id = self.current_id.lock();
                            *current_id = new_id;
                            AcquireDecision::Selected(
                                new_id,
                                new_creds,
                                false,
                                fallback_from_sticky,
                            )
                        } else {
                            let entries = self.entries.lock();
                            let proxy_resources = self.proxy_resources.lock();
                            let config = self.config.lock().clone();
                            // 注意：必须在 bail! 之前计算 available_count，
                            // 因为 available_count() 会尝试获取 entries 锁，
                            // 而此时我们已经持有该锁，会导致死锁
                            let available = entries.iter().filter(|e| !e.disabled).count();
                            let now = Instant::now();
                            let max_concurrent_requests = config.credential_max_concurrent_requests;
                            let global_in_flight = entries
                                .iter()
                                .map(|entry| entry.in_flight_requests)
                                .sum::<u32>();
                            let global_has_capacity = global_has_concurrency_capacity(
                                global_in_flight,
                                config.dispatch_global_max_concurrent_requests,
                            );
                            let model_usable = entries
                                .iter()
                                .filter(|e| credential_is_usable_for_model(e, model))
                                .count();
                            let usable = entries
                                .iter()
                                .filter(|e| {
                                    credential_is_usable_for_model(e, model)
                                        && credential_proxy_is_dispatchable(
                                            &e.credentials,
                                            &proxy_resources,
                                        )
                                })
                                .count();
                            let proxy_blocked = entries
                                .iter()
                                .filter(|e| {
                                    credential_is_usable_for_model(e, model)
                                        && !credential_proxy_is_dispatchable(
                                            &e.credentials,
                                            &proxy_resources,
                                        )
                                })
                                .count();
                            let dispatchable = if global_has_capacity {
                                entries
                                    .iter()
                                    .filter(|e| {
                                        !local_excluded_ids.contains(&e.id)
                                            && credential_is_dispatchable(
                                                &proxy_resources,
                                                e,
                                                model,
                                                now,
                                                max_concurrent_requests,
                                            )
                                    })
                                    .count()
                            } else {
                                0
                            };
                            let effective_concurrency_range = format_effective_concurrency_range(
                                effective_concurrency_range_for_candidates(
                                    &entries,
                                    &proxy_resources,
                                    model,
                                    &local_excluded_ids,
                                    max_concurrent_requests,
                                ),
                            );
                            let excluded_usable = entries
                                .iter()
                                .filter(|e| {
                                    local_excluded_ids.contains(&e.id)
                                        && credential_is_usable_for_model(e, model)
                                        && credential_proxy_is_dispatchable(
                                            &e.credentials,
                                            &proxy_resources,
                                        )
                                })
                                .count();
                            if usable > 0 && excluded_usable >= usable {
                                if acquire_mode.is_fail_fast() && slot_race_excluded_count > 0 {
                                    anyhow::bail!(
                                        "本地凭据调度容量暂不可用（本次请求因并发槽竞争临时排除了所有可用凭据，可用: {}/{}, global_credential_max_concurrent_requests={}, effective_credential_max_concurrent_requests={}）",
                                        available,
                                        total,
                                        max_concurrent_requests,
                                        effective_concurrency_range
                                    );
                                }
                                anyhow::bail!(
                                    "本次请求临时排除了所有可用凭据（可用: {}/{}, 临时排除: {}）",
                                    available,
                                    total,
                                    excluded_usable
                                );
                            }
                            if available > 0 && model_usable == 0 && model.is_some() {
                                anyhow::bail!(
                                    "没有支持当前模型的可用凭据（可用: {}/{}）",
                                    available,
                                    total
                                );
                            }
                            if model_usable > 0 && usable == 0 && proxy_blocked >= model_usable {
                                anyhow::bail!(
                                    "所有可用凭据均因代理资源不可用而不可调度（可用: {}/{}, 代理不可用: {}）",
                                    available,
                                    total,
                                    proxy_blocked
                                );
                            }
                            if usable > 0 && dispatchable > 0 {
                                tracing::debug!(
                                    available,
                                    total,
                                    usable,
                                    dispatchable,
                                    "调度候选在重检时恢复可用，重新选择凭据"
                                );
                                continue;
                            }
                            if usable > 0 && dispatchable == 0 {
                                let dispatch_candidate_count = entries
                                    .iter()
                                    .filter(|e| {
                                        credential_is_dispatch_candidate(
                                            &proxy_resources,
                                            e,
                                            model,
                                            &local_excluded_ids,
                                        )
                                    })
                                    .count();
                                let cooldown_blocked = entries
                                    .iter()
                                    .filter(|e| {
                                        credential_is_dispatch_candidate(
                                            &proxy_resources,
                                            e,
                                            model,
                                            &local_excluded_ids,
                                        ) && entry_cooldown_remaining(e, model, now).is_some()
                                    })
                                    .count();
                                let wait_for = min_dispatch_wait(
                                    &entries,
                                    &proxy_resources,
                                    model,
                                    &local_excluded_ids,
                                    now,
                                );
                                if dispatch_candidate_count > 0
                                    && cooldown_blocked >= dispatch_candidate_count
                                {
                                    let retry_after_secs = wait_for
                                        .map(|duration| duration.as_secs().saturating_add(1))
                                        .unwrap_or(1)
                                        .max(1);
                                    anyhow::bail!(
                                        "所有可用凭据均处于上游临时冷却（可用: {}/{}, 临时可调度: 0, global_credential_max_concurrent_requests={}, effective_credential_max_concurrent_requests={}, retry_after_secs={}）",
                                        available,
                                        total,
                                        max_concurrent_requests,
                                        effective_concurrency_range,
                                        retry_after_secs
                                    );
                                }
                                let concurrency_blocked = concurrency_blocked_count(
                                    &entries,
                                    &proxy_resources,
                                    model,
                                    &local_excluded_ids,
                                    now,
                                    max_concurrent_requests,
                                    global_has_capacity,
                                );
                                if concurrency_blocked > 0
                                    && concurrency_blocked >= dispatch_candidate_count
                                {
                                    AcquireDecision::WaitForDispatch {
                                        available,
                                        total,
                                        global_credential_max_concurrent_requests:
                                            max_concurrent_requests,
                                        effective_credential_max_concurrent_requests:
                                            effective_concurrency_range,
                                        wait_for: None,
                                    }
                                } else {
                                    AcquireDecision::WaitForDispatch {
                                        available,
                                        total,
                                        global_credential_max_concurrent_requests:
                                            max_concurrent_requests,
                                        effective_credential_max_concurrent_requests:
                                            effective_concurrency_range,
                                        wait_for,
                                    }
                                }
                            } else {
                                anyhow::bail!("所有凭据均已禁用（{}/{}）", available, total);
                            }
                        }
                    }
                }
            };

            let (id, credentials, sticky_bound, fallback_from_sticky) = match decision {
                AcquireDecision::Selected(id, credentials, sticky_bound, fallback_from_sticky) => {
                    (id, credentials, sticky_bound, fallback_from_sticky)
                }
                AcquireDecision::WaitForDispatch {
                    available,
                    total,
                    global_credential_max_concurrent_requests,
                    effective_credential_max_concurrent_requests,
                    wait_for,
                } => {
                    if acquire_mode.is_fail_fast() {
                        let retry_after_secs = wait_for
                            .map(|duration| duration.as_secs().saturating_add(1))
                            .unwrap_or(1)
                            .max(1);
                        anyhow::bail!(
                            "本地凭据调度容量暂不可用（可用: {}/{}, 临时可调度: 0, global_credential_max_concurrent_requests={}, effective_credential_max_concurrent_requests={}, retry_after_secs={}）",
                            available,
                            total,
                            global_credential_max_concurrent_requests,
                            effective_credential_max_concurrent_requests,
                            retry_after_secs
                        );
                    }
                    if queue_guard.is_none() {
                        queue_guard = self.try_enter_dispatch_queue()?;
                        if queue_guard.is_none() {
                            anyhow::bail!(
                                "凭据调度等待队列已满（max_queued_requests={}, global_max_concurrent_requests={}）",
                                self.max_queued_requests(),
                                self.global_max_concurrent_requests()
                            );
                        }
                    }
                    let now = Instant::now();
                    let retry_after_secs = wait_for
                        .map(|duration| duration.as_secs().saturating_add(1))
                        .unwrap_or(0);
                    if let Some((waited, max_wait)) =
                        self.dispatch_wait_exceeded(acquire_mode, dispatch_wait_started_at, now)
                    {
                        anyhow::bail!(
                            "凭据调度排队等待超时（可用: {}/{}, 临时可调度: 0, global_credential_max_concurrent_requests={}, effective_credential_max_concurrent_requests={}, waited_secs={}, max_wait_secs={}, retry_after_secs={}）",
                            available,
                            total,
                            global_credential_max_concurrent_requests,
                            effective_credential_max_concurrent_requests,
                            waited.as_secs(),
                            max_wait.as_secs(),
                            retry_after_secs.max(1)
                        );
                    }
                    tracing::debug!(
                        available,
                        total,
                        global_credential_max_concurrent_requests,
                        effective_credential_max_concurrent_requests,
                        retry_after_secs,
                        "所有可用凭据暂不可调度，进入排队等待"
                    );
                    self.wait_for_dispatch_capacity(
                        wait_for,
                        self.dispatch_wait_remaining(acquire_mode, dispatch_wait_started_at, now),
                    )
                    .await;
                    continue;
                }
            };

            let Some(in_flight_lease) = self.acquire_in_flight_slot(id)? else {
                if acquire_mode.is_fail_fast() {
                    local_excluded_ids.insert(id);
                    attempt_count += 1;
                    slot_race_excluded_count += 1;
                    tracing::debug!(
                        credential_id = id,
                        excluded_count = local_excluded_ids.len(),
                        "fail-fast 预检选中凭据后并发槽已满，本次请求临时排除并重选"
                    );
                    continue;
                }
                if queue_guard.is_none() {
                    queue_guard = self.try_enter_dispatch_queue()?;
                    if queue_guard.is_none() {
                        anyhow::bail!(
                            "凭据调度等待队列已满（max_queued_requests={}, global_max_concurrent_requests={}）",
                            self.max_queued_requests(),
                            self.global_max_concurrent_requests()
                        );
                    }
                }
                let now = Instant::now();
                if let Some((waited, max_wait)) =
                    self.dispatch_wait_exceeded(acquire_mode, dispatch_wait_started_at, now)
                {
                    let global_credential_max_concurrent_requests = self.max_concurrent_requests();
                    let effective_credential_max_concurrent_requests = {
                        let entries = self.entries.lock();
                        let proxy_resources = self.proxy_resources.lock();
                        format_effective_concurrency_range(
                            effective_concurrency_range_for_candidates(
                                &entries,
                                &proxy_resources,
                                model,
                                &local_excluded_ids,
                                global_credential_max_concurrent_requests,
                            ),
                        )
                    };
                    anyhow::bail!(
                        "凭据调度排队等待超时（可用: {}/{}, 临时可调度: 0, global_credential_max_concurrent_requests={}, effective_credential_max_concurrent_requests={}, waited_secs={}, max_wait_secs={}, retry_after_secs=1）",
                        self.available_count(),
                        total,
                        global_credential_max_concurrent_requests,
                        effective_credential_max_concurrent_requests,
                        waited.as_secs(),
                        max_wait.as_secs()
                    );
                }
                tracing::debug!(
                    credential_id = id,
                    "选中凭据后并发槽已被其他请求占用，进入排队等待"
                );
                self.wait_for_dispatch_capacity(
                    None,
                    self.dispatch_wait_remaining(acquire_mode, dispatch_wait_started_at, now),
                )
                .await;
                continue;
            };
            drop(queue_guard.take());

            // 尝试获取/刷新 Token
            match self.try_ensure_token(id, &credentials, true).await {
                Ok(ctx) => {
                    if let Err(err) = self.mark_rate_limited_at(ctx.id, Instant::now()) {
                        drop(in_flight_lease);
                        return Err(err);
                    }
                    if let Some(sid) = session_id {
                        if self.bound_credential_exists_but_unusable(sid, model) {
                            self.unbind_session(sid);
                        }
                        let should_bind = self
                            .bound_credential_id(sid)
                            .is_none_or(|bound_id| bound_id == ctx.id);
                        if should_bind {
                            self.bind_session_to_credential(sid, ctx.id);
                        }
                    }
                    self.record_scheduler_selection(ctx.id);
                    return Ok(CallContext {
                        sticky_bound,
                        fallback_from_sticky,
                        in_flight_lease: Some(in_flight_lease),
                        ..ctx
                    });
                }
                Err(e) => {
                    // refreshToken 永久失效 → 立即禁用，不累计重试
                    let has_available = if e.downcast_ref::<RefreshTokenInvalidError>().is_some() {
                        tracing::warn!("凭据 #{} refreshToken 永久失效: {}", id, e);
                        self.report_refresh_token_invalid(id)
                    } else {
                        tracing::warn!("凭据 #{} Token 刷新失败: {}", id, e);
                        self.report_transient_failure_kind(
                            id,
                            model,
                            TransientFailureKind::Auth,
                            None,
                            format!("token_refresh_failure {}", e),
                        )?;
                        self.report_refresh_failure(id)
                    };
                    drop(in_flight_lease);
                    attempt_count += 1;
                    if !has_available {
                        anyhow::bail!("所有凭据均已禁用（0/{}）", total);
                    }
                }
            }
        }
    }

    /// 选择优先级最高的未禁用凭据作为当前凭据（内部方法）
    ///
    /// 纯粹按优先级选择，不排除当前凭据，用于优先级变更后立即生效
    fn select_highest_priority(&self) {
        let entries = self.entries.lock();
        let mut current_id = self.current_id.lock();

        // 选择优先级最高的未禁用凭据（不排除当前凭据）
        if let Some(best) = entries
            .iter()
            .filter(|e| !e.disabled)
            .min_by_key(|e| e.credentials.priority)
        {
            if best.id != *current_id {
                tracing::info!(
                    "优先级变更后切换凭据: #{} -> #{}（优先级 {}）",
                    *current_id,
                    best.id,
                    best.credentials.priority
                );
                *current_id = best.id;
            }
        }
    }

    /// 尝试使用指定凭据获取有效 Token
    ///
    /// 使用双重检查锁定模式，确保同一时间只有一个刷新操作
    ///
    /// # Arguments
    /// * `id` - 凭据 ID，用于更新正确的条目
    /// * `credentials` - 凭据信息
    async fn try_ensure_token(
        &self,
        id: u64,
        credentials: &KiroCredentials,
        update_refresh_health: bool,
    ) -> anyhow::Result<CallContext> {
        // API Key 凭据直接使用 kiro_api_key 作为 Bearer Token，无需刷新
        if credentials.is_api_key_credential() {
            let credentials = self.resolve_proxy_for_credential(credentials.clone())?;
            let token = credentials
                .kiro_api_key
                .clone()
                .ok_or_else(|| anyhow::anyhow!("API Key 凭据缺少 kiroApiKey"))?;
            return Ok(CallContext {
                id,
                credentials,
                token,
                sticky_bound: false,
                fallback_from_sticky: false,
                in_flight_lease: None,
            });
        }

        // 第一次检查（无锁）：快速判断是否需要刷新
        let needs_refresh = is_token_expired(credentials) || is_token_expiring_soon(credentials);

        let creds = if needs_refresh {
            // 同一凭据只允许一个刷新任务；不同凭据互不阻塞。
            let refresh_lock = self.refresh_lock_for_credential(id);
            let _guard = refresh_lock.lock().await;

            // 第二次检查：获取锁后重新读取凭据，因为其他请求可能已经完成刷新
            let current_creds = {
                let entries = self.entries.lock();
                entries
                    .iter()
                    .find(|e| e.id == id)
                    .map(|e| e.credentials.clone())
                    .ok_or_else(|| anyhow::anyhow!("凭据 #{} 不存在", id))?
            };

            if is_token_expired(&current_creds) || is_token_expiring_soon(&current_creds) {
                let mut redis_refresh_lock: Option<(Arc<RedisStore>, String)> = None;
                if let Some(redis) = &self.redis_store {
                    let redis_for_lock = redis.clone();
                    match redis_for_lock.acquire_refresh_lock(id, 120).await {
                        Ok(Some(lock_token)) => {
                            redis_refresh_lock = Some((redis_for_lock, lock_token));
                        }
                        Ok(None) => {
                            tracing::debug!(
                                credential_id = id,
                                "其他实例正在刷新 Token，等待 PgSQL 凭据同步"
                            );
                            for _ in 0..30 {
                                tokio::time::sleep(StdDuration::from_millis(500)).await;
                                if let Err(err) = self.reload_credentials_from_postgres() {
                                    tracing::warn!(
                                        credential_id = id,
                                        "等待跨实例刷新时重新加载凭据失败: {}",
                                        err
                                    );
                                }
                                let latest = {
                                    let entries = self.entries.lock();
                                    entries
                                        .iter()
                                        .find(|e| e.id == id)
                                        .map(|e| e.credentials.clone())
                                        .ok_or_else(|| anyhow::anyhow!("凭据 #{} 不存在", id))?
                                };
                                if !is_token_expired(&latest) && !is_token_expiring_soon(&latest) {
                                    tracing::debug!(
                                        credential_id = id,
                                        "Token 已由其他实例刷新，跳过本实例刷新"
                                    );
                                    return self.token_context_from_credentials(
                                        id,
                                        latest,
                                        update_refresh_health,
                                    );
                                }
                            }
                            anyhow::bail!("等待其他实例刷新 Token 超时");
                        }
                        Err(err) => tracing::warn!(
                            credential_id = id,
                            "获取 Redis Token 刷新锁失败，使用本进程刷新锁继续: {}",
                            err
                        ),
                    }
                }

                // 确实需要刷新
                let refresh_result = match self.resolve_proxy_for_credential(current_creds.clone())
                {
                    Ok(current_creds_for_proxy) => {
                        let effective_proxy =
                            current_creds_for_proxy.effective_proxy(self.proxy.as_ref());
                        let config = self.runtime_config();
                        refresh_token(&current_creds_for_proxy, &config, effective_proxy.as_ref())
                            .await
                    }
                    Err(err) => Err(err),
                };
                if let Some((redis, lock_token)) = redis_refresh_lock {
                    if let Err(err) = redis.release_refresh_lock(id, &lock_token).await {
                        tracing::warn!(credential_id = id, "释放 Redis Token 刷新锁失败: {}", err);
                    }
                }
                let mut new_creds = refresh_result?;
                new_creds.proxy_url = current_creds.proxy_url.clone();
                new_creds.proxy_username = current_creds.proxy_username.clone();
                new_creds.proxy_password = current_creds.proxy_password.clone();
                new_creds.proxy_resource_id = current_creds.proxy_resource_id;

                if is_token_expired(&new_creds) {
                    anyhow::bail!("刷新后的 Token 仍然无效或已过期");
                }

                // 更新凭据
                {
                    let mut entries = self.entries.lock();
                    if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                        entry.credentials = new_creds.clone();
                    }
                }

                // 只回写当前凭据行，避免旧内存快照覆盖其他实例新增凭据；回写不阻塞本次请求。
                if self.persist_credential_entry_best_effort(id, "Token 刷新后持久化失败") {
                    self.publish_credentials_changed("token_refreshed");
                }

                new_creds
            } else {
                // 其他请求已经完成刷新，直接使用新凭据
                tracing::debug!("Token 已被其他请求刷新，跳过刷新");
                current_creds
            }
        } else {
            credentials.clone()
        };

        self.token_context_from_credentials(id, creds, update_refresh_health)
    }

    fn token_context_from_credentials(
        &self,
        id: u64,
        creds: KiroCredentials,
        update_refresh_health: bool,
    ) -> anyhow::Result<CallContext> {
        let creds = self.resolve_proxy_for_credential(creds)?;
        let token = creds
            .access_token
            .clone()
            .ok_or_else(|| anyhow::anyhow!("没有可用的 accessToken"))?;

        if update_refresh_health {
            let mut changed = false;
            {
                let mut entries = self.entries.lock();
                if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                    if entry.refresh_failure_count != 0 {
                        changed = true;
                    }
                    entry.refresh_failure_count = 0;
                }
            }
            if changed {
                self.queue_runtime_state_flush(id, None, Utc::now());
            }
        }

        Ok(CallContext {
            id,
            credentials: creds,
            token,
            sticky_bound: false,
            fallback_from_sticky: false,
            in_flight_lease: None,
        })
    }

    fn resolve_proxy_for_credential(
        &self,
        mut creds: KiroCredentials,
    ) -> anyhow::Result<KiroCredentials> {
        if creds.proxy_url.is_some() {
            return Ok(creds);
        }
        let availability = {
            let resources = self.proxy_resources.lock();
            credential_proxy_availability(&creds, &resources)
        };
        let Some(availability) = availability else {
            return Ok(creds);
        };

        match availability {
            ProxyResourceAvailability::Available(resource) => {
                creds.proxy_url = Some(resource.proxy_url);
                creds.proxy_username = resource.proxy_username;
                creds.proxy_password = resource.proxy_password;
                Ok(creds)
            }
            unavailable => Err(proxy_unavailable_error(creds.id, unavailable)),
        }
    }

    fn preserve_proxy_fields(
        mut credentials: KiroCredentials,
        source: &KiroCredentials,
    ) -> KiroCredentials {
        credentials.proxy_url = source.proxy_url.clone();
        credentials.proxy_username = source.proxy_username.clone();
        credentials.proxy_password = source.proxy_password.clone();
        credentials.proxy_resource_id = source.proxy_resource_id;
        credentials
    }

    fn effective_proxy_display_with_resources(
        &self,
        creds: &KiroCredentials,
        resources: &HashMap<u64, ProxyResourceRuntime>,
    ) -> (Option<String>, String) {
        match creds.proxy_url.as_deref() {
            Some(url) if url.eq_ignore_ascii_case(KiroCredentials::PROXY_DIRECT) => {
                (None, "direct".to_string())
            }
            Some(url) => (Some(url.to_string()), "credential".to_string()),
            None => {
                if let Some(resource_id) = creds.proxy_resource_id {
                    if let Some(resource) = resources.get(&resource_id) {
                        if resource.enabled {
                            return (Some(resource.proxy_url.clone()), "resource".to_string());
                        }
                        return (None, "resource_disabled".to_string());
                    }
                    return (None, "resource_missing".to_string());
                }
                match &self.proxy {
                    Some(proxy) => (Some(proxy.url.clone()), "global".to_string()),
                    None => (None, "none".to_string()),
                }
            }
        }
    }

    fn proxy_resource_name_from_resources(
        resources: &HashMap<u64, ProxyResourceRuntime>,
        resource_id: Option<u64>,
    ) -> Option<String> {
        let resource_id = resource_id?;
        resources
            .get(&resource_id)
            .map(|resource| resource.name.clone())
    }

    fn normalized_auth_method(credentials: &KiroCredentials) -> Option<String> {
        if credentials.is_api_key_credential() {
            return Some("api_key".to_string());
        }
        credentials.auth_method.as_deref().map(|method| {
            if method.eq_ignore_ascii_case("builder-id") || method.eq_ignore_ascii_case("iam") {
                "idc".to_string()
            } else if method.eq_ignore_ascii_case("external-idp")
                || method.eq_ignore_ascii_case("externalidp")
                || method.eq_ignore_ascii_case("enterprise")
            {
                "external_idp".to_string()
            } else {
                method.to_string()
            }
        })
    }

    fn base_snapshot_from_entry(
        &self,
        entry: &CredentialEntry,
        config: &Config,
        resources: &HashMap<u64, ProxyResourceRuntime>,
    ) -> CredentialBaseSnapshot {
        let (effective_proxy_url, effective_proxy_source) =
            self.effective_proxy_display_with_resources(&entry.credentials, resources);
        let proxy_resource_id = entry.credentials.proxy_resource_id;
        CredentialBaseSnapshot {
            id: entry.id,
            created_at: entry.credentials.created_at.clone(),
            updated_at: entry.credentials.updated_at.clone(),
            priority: entry.credentials.priority,
            disabled: entry.disabled,
            disabled_reason: entry.disabled_reason.map(|r| r.as_str().to_string()),
            auth_method: Self::normalized_auth_method(&entry.credentials),
            provider: entry.credentials.provider.clone(),
            region: entry.credentials.region.clone(),
            auth_region: entry.credentials.auth_region.clone(),
            api_region: entry.credentials.api_region.clone(),
            effective_auth_region: entry.credentials.effective_auth_region(config).to_string(),
            effective_api_region: entry.credentials.effective_api_region(config).to_string(),
            has_profile_arn: entry.credentials.profile_arn.is_some(),
            refresh_token_hash: if entry.credentials.is_api_key_credential() {
                None
            } else {
                entry.credentials.refresh_token.as_deref().map(sha256_hex)
            },
            api_key_hash: if entry.credentials.is_api_key_credential() {
                entry.credentials.kiro_api_key.as_deref().map(sha256_hex)
            } else {
                None
            },
            masked_api_key: if entry.credentials.is_api_key_credential() {
                entry.credentials.kiro_api_key.as_deref().map(mask_api_key)
            } else {
                None
            },
            email: entry.credentials.email.clone(),
            subscription_title: entry.credentials.subscription_title.clone(),
            has_proxy: effective_proxy_url.is_some(),
            proxy_url: entry.credentials.proxy_url.clone(),
            proxy_username: entry.credentials.proxy_username.clone(),
            proxy_password: entry.credentials.proxy_password.clone(),
            proxy_resource_id,
            proxy_resource_name: Self::proxy_resource_name_from_resources(
                resources,
                proxy_resource_id,
            ),
            effective_proxy_url,
            effective_proxy_source,
            endpoint: entry.credentials.endpoint.clone(),
            max_concurrent_requests: effective_max_concurrent_requests(
                entry,
                config.credential_max_concurrent_requests,
            ),
            max_concurrent_requests_override: entry.credentials.max_concurrent_requests,
            rpm: effective_rpm(entry, config.credential_rpm.unwrap_or(0)),
            rpm_override: entry.credentials.rpm,
            warmup_remaining: entry.warmup_remaining,
        }
    }

    fn runtime_snapshot_from_entry(
        &self,
        entry: &CredentialEntry,
        config: &Config,
        resources: &HashMap<u64, ProxyResourceRuntime>,
        max_concurrent_requests: u32,
        lease_max_age: Option<StdDuration>,
        now: Instant,
        now_ms: i64,
        score_total_recent: u64,
        score_candidate_count: usize,
    ) -> CredentialEntrySnapshot {
        let (effective_proxy_url, effective_proxy_source) =
            self.effective_proxy_display_with_resources(&entry.credentials, resources);
        let proxy_resource_id = entry.credentials.proxy_resource_id;
        let oldest_in_flight_age_secs = entry
            .in_flight_leases
            .iter()
            .map(|lease| now.saturating_duration_since(lease.acquired_at).as_secs())
            .max()
            .unwrap_or(0);
        let newest_in_flight_idle_secs = entry
            .in_flight_leases
            .iter()
            .map(|lease| now.saturating_duration_since(lease.last_seen_at).as_secs())
            .min()
            .unwrap_or(0);
        let cooldowns = entry_cooldown_snapshots(entry, now);
        let cooldown_reason = cooldowns
            .iter()
            .find_map(|cooldown| cooldown.reason.clone());
        let selection_pressure =
            selection_pressure_from_totals(entry, score_total_recent, score_candidate_count);

        CredentialEntrySnapshot {
            id: entry.id,
            created_at: entry.credentials.created_at.clone(),
            updated_at: entry.credentials.updated_at.clone(),
            priority: entry.credentials.priority,
            disabled: entry.disabled,
            failure_count: entry.failure_count,
            auth_method: Self::normalized_auth_method(&entry.credentials),
            provider: entry.credentials.provider.clone(),
            region: entry.credentials.region.clone(),
            auth_region: entry.credentials.auth_region.clone(),
            api_region: entry.credentials.api_region.clone(),
            effective_auth_region: entry.credentials.effective_auth_region(config).to_string(),
            effective_api_region: entry.credentials.effective_api_region(config).to_string(),
            has_profile_arn: entry.credentials.profile_arn.is_some(),
            expires_at: if entry.credentials.is_api_key_credential() {
                None
            } else {
                entry.credentials.expires_at.clone()
            },
            refresh_token_hash: if entry.credentials.is_api_key_credential() {
                None
            } else {
                entry.credentials.refresh_token.as_deref().map(sha256_hex)
            },
            api_key_hash: if entry.credentials.is_api_key_credential() {
                entry.credentials.kiro_api_key.as_deref().map(sha256_hex)
            } else {
                None
            },
            masked_api_key: if entry.credentials.is_api_key_credential() {
                entry.credentials.kiro_api_key.as_deref().map(mask_api_key)
            } else {
                None
            },
            email: entry.credentials.email.clone(),
            subscription_title: entry.credentials.subscription_title.clone(),
            success_count: entry.success_count,
            total_selection_count: entry.total_selection_count,
            last_used_at: entry.last_used_at.clone(),
            has_proxy: effective_proxy_url.is_some(),
            proxy_url: entry.credentials.proxy_url.clone(),
            proxy_username: entry.credentials.proxy_username.clone(),
            proxy_password: entry.credentials.proxy_password.clone(),
            proxy_resource_id,
            proxy_resource_name: Self::proxy_resource_name_from_resources(
                resources,
                proxy_resource_id,
            ),
            effective_proxy_url,
            effective_proxy_source,
            refresh_failure_count: entry.refresh_failure_count,
            disabled_reason: entry.disabled_reason.map(|r| r.as_str().to_string()),
            endpoint: entry.credentials.endpoint.clone(),
            cooled_down: entry_any_cooldown_remaining(entry, now).is_some(),
            cooldown_remaining_secs: entry_any_cooldown_remaining(entry, now)
                .map(|duration| duration.as_secs().saturating_add(1))
                .unwrap_or(0),
            cooldown_reason,
            cooldowns,
            rate_limited: entry_rate_limit_remaining(entry, now).is_some(),
            rate_limit_remaining_secs: entry_rate_limit_remaining(entry, now)
                .map(|duration| duration.as_secs().saturating_add(1))
                .unwrap_or(0),
            in_flight_requests: entry.in_flight_requests,
            oldest_in_flight_age_secs,
            newest_in_flight_idle_secs,
            max_concurrent_requests: effective_max_concurrent_requests(
                entry,
                max_concurrent_requests,
            ),
            max_concurrent_requests_override: entry.credentials.max_concurrent_requests,
            rpm: effective_rpm(entry, config.credential_rpm.unwrap_or(0)),
            rpm_override: entry.credentials.rpm,
            in_flight_lease_max_secs: lease_max_age
                .map(|duration| duration.as_secs())
                .unwrap_or(0),
            warmup_remaining: entry.warmup_remaining,
            transient_failure_streak: entry.health.transient_failure_streak,
            recent_error_rate: entry.health.recent_error_rate,
            latency_ewma_ms: entry.health.latency_ewma_ms,
            last_error_kind: entry.health.last_error_kind.clone(),
            last_error_reason: entry.health.last_error_reason.clone(),
            last_error_at_ms: entry.health.last_error_at_ms,
            in_probation: entry
                .health
                .probation_until_ms
                .is_some_and(|until_ms| until_ms > now_ms),
            probation_remaining_secs: entry
                .health
                .probation_until_ms
                .filter(|until_ms| *until_ms > now_ms)
                .map(|until_ms| ((until_ms - now_ms) as u64).div_ceil(1000))
                .unwrap_or(0),
            scheduler_selection_count: entry.total_selection_count,
            recent_scheduler_selection_count_10s: entry.health.recent_selection_count_10s,
            recent_scheduler_selection_count_60s: entry.health.recent_selection_count_60s,
            recent_scheduler_selection_count_5m: entry.health.recent_selection_count_5m,
            scheduler_selection_pressure: selection_pressure,
            scheduler_score: scheduler_score_with_config(
                entry,
                None,
                now_ms,
                selection_pressure,
                config,
            ),
        }
    }

    fn credential_from_entry(entry: &CredentialEntry) -> KiroCredentials {
        let mut cred = entry.credentials.clone();
        cred.id = Some(entry.id);
        cred.disabled = entry.disabled;
        cred.canonicalize_auth_method();
        cred
    }

    fn runtime_state_from_entry(entry: &CredentialEntry) -> CredentialRuntimeStateRow {
        CredentialRuntimeStateRow {
            failure_count: entry.failure_count,
            refresh_failure_count: entry.refresh_failure_count,
            disabled_reason: entry
                .disabled_reason
                .map(|reason| reason.as_str().to_string()),
            warmup_remaining: entry.warmup_remaining,
        }
    }

    /// 非破坏性地将当前凭据快照 upsert 到 PgSQL。
    ///
    /// 该方法只用于旧数据补全、环境变量凭据导入等 bootstrap 场景。底层
    /// `save_credentials` 不再删除未出现在当前内存快照里的数据库凭据。
    fn persist_credentials(&self) -> anyhow::Result<bool> {
        let credentials: Vec<KiroCredentials> = {
            let entries = self.entries.lock();
            entries.iter().map(Self::credential_from_entry).collect()
        };

        let Some(store) = &self.postgres_store else {
            return Ok(false);
        };
        let store = store.clone();
        block_on_storage("保存凭据到 PgSQL", async move {
            store.save_credentials(&credentials).await
        })?;
        tracing::debug!("已保存凭据到 PgSQL");
        Ok(true)
    }

    fn persist_credential_entry(&self, id: u64) -> anyhow::Result<bool> {
        let credential = {
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|entry| entry.id == id)
                .map(Self::credential_from_entry)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?
        };
        self.persist_credential_value(&credential)
    }

    fn persist_credential_entry_best_effort(&self, id: u64, operation: &'static str) -> bool {
        let Some(store) = &self.postgres_store else {
            return false;
        };
        let credential = {
            let entries = self.entries.lock();
            let Some(entry) = entries.iter().find(|entry| entry.id == id) else {
                return false;
            };
            Self::credential_from_entry(entry)
        };
        let store = store.clone();
        let entries = self.entries.clone();
        spawn_best_effort_storage_task(operation, async move {
            let saved = store.upsert_credential(&credential).await?;
            if let Some(saved_id) = saved.id {
                let mut entries = entries.lock();
                if let Some(entry) = entries.iter_mut().find(|entry| entry.id == saved_id) {
                    entry.credentials.created_at = saved.created_at;
                    entry.credentials.updated_at = saved.updated_at;
                }
            }
            Ok(())
        });
        true
    }

    fn persist_credential_value(&self, credential: &KiroCredentials) -> anyhow::Result<bool> {
        let Some(store) = &self.postgres_store else {
            return Ok(false);
        };
        let store = store.clone();
        let credential = credential.clone();
        let saved = block_on_storage("保存凭据到 PgSQL", async move {
            store.upsert_credential(&credential).await
        })?;
        if let Some(id) = saved.id {
            let mut entries = self.entries.lock();
            if let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) {
                entry.credentials.created_at = saved.created_at;
                entry.credentials.updated_at = saved.updated_at;
            }
        }
        Ok(true)
    }

    pub fn reload_credentials_from_postgres(&self) -> anyhow::Result<bool> {
        let Some(store) = &self.postgres_store else {
            return Ok(false);
        };
        let proxy_changed = self.reload_proxy_resources_from_postgres()?;
        let store = store.clone();
        let credentials = block_on_storage("从 PgSQL 重新加载凭据", async move {
            store.load_credentials().await
        })?;
        let by_id: HashMap<u64, KiroCredentials> = credentials
            .into_iter()
            .filter_map(|credential| credential.id.map(|id| (id, credential)))
            .collect();
        let mut entries = self.entries.lock();
        let mut changed = false;
        let non_deleted_ids: HashSet<u64> = by_id.keys().copied().collect();
        let before_len = entries.len();
        entries.retain(|entry| non_deleted_ids.contains(&entry.id));
        if entries.len() != before_len {
            changed = true;
        }
        self.retain_refresh_locks_for_ids(&non_deleted_ids);
        let existing_ids: HashSet<u64> = entries.iter().map(|entry| entry.id).collect();
        for entry in entries.iter_mut() {
            if let Some(credential) = by_id.get(&entry.id) {
                let mut credential = credential.clone();
                credential.canonicalize_auth_method();
                if entry.credentials.disabled != credential.disabled
                    || !entry.credentials.same_dispatch_config(&credential)
                {
                    changed = true;
                }
                entry.disabled = credential.disabled;
                entry.disabled_reason = if credential.disabled {
                    Some(DisabledReason::Manual)
                } else {
                    entry.disabled_reason
                };
                entry.credentials = credential;
            }
        }
        for (id, mut credential) in by_id {
            if existing_ids.contains(&id) {
                continue;
            }
            credential.canonicalize_auth_method();
            entries.push(CredentialEntry {
                id,
                disabled: credential.disabled,
                disabled_reason: if credential.disabled {
                    Some(DisabledReason::Manual)
                } else {
                    None
                },
                credentials: credential,
                failure_count: 0,
                refresh_failure_count: 0,
                success_count: 0,
                total_selection_count: 0,
                last_used_at: None,
                cooldown_until: None,
                cooldown_reason: None,
                model_cooldowns: HashMap::new(),
                rate_limit_available_at: None,
                in_flight_requests: 0,
                in_flight_leases: Vec::new(),
                warmup_remaining: 0,
                health: SchedulerHealthState::default(),
                model_health: HashMap::new(),
                selection_events: VecDeque::new(),
            });
            changed = true;
        }
        if changed {
            entries.sort_by_key(|entry| (entry.credentials.priority, entry.id));
        }
        drop(entries);
        let runtime_changed = self.load_runtime_state();
        if changed {
            self.load_stats();
        }
        if changed || runtime_changed {
            self.select_highest_priority();
            self.invalidate_local_pool_route_state_cache();
        }
        Ok(changed || runtime_changed || proxy_changed)
    }

    pub fn reload_proxy_resources_from_postgres(&self) -> anyhow::Result<bool> {
        let Some(store) = &self.postgres_store else {
            return Ok(false);
        };
        let store = store.clone();
        let resources = block_on_storage("从 PgSQL 重新加载代理资源", async move {
            store.load_proxy_resources().await
        })?;
        let next: HashMap<u64, ProxyResourceRuntime> = resources
            .into_iter()
            .map(ProxyResourceRuntime::from)
            .map(|resource| (resource.id, resource))
            .collect();
        let mut current = self.proxy_resources.lock();
        let changed = current.len() != next.len()
            || next.iter().any(|(id, next_resource)| {
                current.get(id).is_none_or(|current_resource| {
                    current_resource.name != next_resource.name
                        || current_resource.proxy_url != next_resource.proxy_url
                        || current_resource.proxy_username != next_resource.proxy_username
                        || current_resource.proxy_password != next_resource.proxy_password
                        || current_resource.enabled != next_resource.enabled
                })
            });
        if changed {
            *current = next;
            tracing::info!("已重新加载 {} 个代理资源", current.len());
            self.invalidate_local_pool_route_state_cache();
        }
        Ok(changed)
    }

    fn redis_event_payload(&self, kind: &str, version: Option<i64>, reason: &str) -> String {
        serde_json::json!({
            "kind": kind,
            "version": version,
            "reason": reason,
            "changedAt": Utc::now().to_rfc3339(),
        })
        .to_string()
    }

    fn publish_runtime_config_changed(&self, version: Option<i64>, reason: &str) {
        self.invalidate_local_pool_route_state_cache();
        let Some(redis) = &self.redis_store else {
            return;
        };
        let redis = redis.clone();
        let payload = self.redis_event_payload("runtime_config_changed", version, reason);
        spawn_best_effort_storage_task("发布 Redis 运行配置变更通知", async move {
            redis.publish_runtime_config_changed(payload).await
        });
    }

    fn publish_credentials_changed(&self, reason: &str) {
        self.invalidate_local_pool_route_state_cache();
        if let Some(store) = &self.postgres_store {
            let store = store.clone();
            let reason_owned = reason.to_string();
            spawn_best_effort_storage_task("记录凭据事件到 PgSQL", async move {
                store
                    .record_credential_event(
                        None,
                        "credentials_changed",
                        Some(&reason_owned),
                        serde_json::json!({ "reason": reason_owned }),
                    )
                    .await
            });
        }
        let Some(redis) = &self.redis_store else {
            return;
        };
        let redis = redis.clone();
        let payload = self.redis_event_payload("credentials_changed", None, reason);
        spawn_best_effort_storage_task("发布 Redis 凭据变更通知", async move {
            redis.publish_credentials_changed(payload).await
        });
    }

    pub fn publish_admin_credentials_changed(&self, reason: &str) {
        self.publish_credentials_changed(reason);
    }

    pub(crate) fn notify_dispatch_state_changed(&self) {
        self.in_flight_notify.notify_waiters();
    }

    /// 从 PgSQL 加载统计数据并应用到当前条目。
    fn load_stats(&self) {
        self.load_stats_inner(true);
    }

    fn refresh_stats_from_postgres(&self) {
        self.load_stats_inner(false);
    }

    fn load_stats_inner(&self, log_info: bool) {
        let Some(store) = &self.postgres_store else {
            return;
        };
        let store = store.clone();
        let stats = match block_on_storage("从 PgSQL 加载凭据统计", async move {
            store.load_credential_stats().await
        }) {
            Ok(stats) => stats,
            Err(e) => {
                tracing::warn!("{}", e);
                return;
            }
        };

        let mut entries = self.entries.lock();
        for entry in entries.iter_mut() {
            if let Some(s) = stats.get(&entry.id) {
                entry.success_count = entry.success_count.max(s.success_count);
                entry.total_selection_count = entry.total_selection_count.max(s.selection_count);
                entry.last_used_at = s.last_used_at.clone();
            }
        }
        *self.last_stats_save_at.lock() = Some(Instant::now());
        self.stats_dirty.store(false, Ordering::Relaxed);
        if log_info {
            tracing::info!("已从 PgSQL 加载 {} 条凭据统计", stats.len());
        } else {
            tracing::debug!("已刷新 {} 条 PgSQL 凭据统计", stats.len());
        }
    }

    /// 将当前统计数据持久化到 PgSQL
    fn save_stats(&self) {
        let Some(store) = &self.postgres_store else {
            return;
        };
        let deltas: HashMap<u64, CredentialStatsDeltaRow> = {
            let mut pending = self.pending_stats_deltas.lock();
            std::mem::take(&mut *pending)
        };
        let runtime_snapshots: HashMap<u64, CredentialRuntimeStateSnapshot> = {
            let mut pending = self.pending_runtime_state_snapshots.lock();
            std::mem::take(&mut *pending)
        };
        if deltas.is_empty() && runtime_snapshots.is_empty() {
            self.stats_dirty.store(false, Ordering::Release);
            return;
        }

        if !deltas.is_empty() {
            let store = store.clone();
            let deltas_to_write = deltas.clone();
            if let Err(e) = block_on_storage("保存凭据统计增量到 PgSQL", async move {
                store.apply_credential_stats_deltas(&deltas_to_write).await
            }) {
                tracing::warn!("{}", e);
                self.requeue_stats_deltas(deltas);
                self.requeue_runtime_state_snapshots(runtime_snapshots);
                self.stats_dirty.store(true, Ordering::Release);
                return;
            }
        }

        if !runtime_snapshots.is_empty() {
            let store = store.clone();
            let runtime_snapshots_to_write = runtime_snapshots.clone();
            if let Err(e) = block_on_storage("保存凭据运行态快照到 PgSQL", async move {
                store
                    .apply_credential_runtime_state_snapshots(&runtime_snapshots_to_write)
                    .await
            }) {
                tracing::warn!("{}", e);
                self.requeue_runtime_state_snapshots(runtime_snapshots);
                self.stats_dirty.store(true, Ordering::Release);
                return;
            }
        }

        if self.pending_stats_deltas.lock().is_empty()
            && self.pending_runtime_state_snapshots.lock().is_empty()
        {
            *self.last_stats_save_at.lock() = Some(Instant::now());
            self.stats_dirty.store(false, Ordering::Release);
        } else {
            self.stats_dirty.store(true, Ordering::Release);
        }
    }

    fn requeue_stats_deltas(&self, deltas: HashMap<u64, CredentialStatsDeltaRow>) {
        if deltas.is_empty() {
            return;
        }
        let mut pending = self.pending_stats_deltas.lock();
        for (id, delta) in deltas {
            let entry = pending.entry(id).or_default();
            entry.success_delta = entry.success_delta.saturating_add(delta.success_delta);
            entry.selection_delta = entry.selection_delta.saturating_add(delta.selection_delta);
            if let Some(last_used_at) = delta.last_used_at {
                let replace = entry
                    .last_used_at
                    .as_ref()
                    .is_none_or(|existing| &last_used_at > existing);
                if replace {
                    entry.last_used_at = Some(last_used_at);
                }
            }
        }
    }

    fn requeue_runtime_state_snapshots(
        &self,
        snapshots: HashMap<u64, CredentialRuntimeStateSnapshot>,
    ) {
        if snapshots.is_empty() {
            return;
        }
        let mut pending = self.pending_runtime_state_snapshots.lock();
        for (id, snapshot) in snapshots {
            match pending.get_mut(&id) {
                Some(existing) if existing.updated_at > snapshot.updated_at => {}
                Some(existing) => *existing = snapshot,
                None => {
                    pending.insert(id, snapshot);
                }
            }
        }
    }

    fn mark_stats_dirty(&self) {
        self.stats_dirty.store(true, Ordering::Release);
    }

    pub fn spawn_stats_flush_worker(self: &Arc<Self>) {
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(CREDENTIAL_STATS_FLUSH_MIN_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                let _ = manager.cleanup_expired_in_flight_leases_throttled();
                manager.save_stats();
            }
        });
    }

    /// 从 PgSQL 加载凭据运行态（失败计数、禁用原因、预热次数）。
    fn load_runtime_state(&self) -> bool {
        let Some(store) = &self.postgres_store else {
            return false;
        };
        let store = store.clone();
        let states = match block_on_storage("从 PgSQL 加载凭据运行态", async move {
            store.load_credential_runtime_state().await
        }) {
            Ok(states) => states,
            Err(e) => {
                tracing::warn!("{}", e);
                return false;
            }
        };

        let mut entries = self.entries.lock();
        let mut changed = false;
        for entry in entries.iter_mut() {
            if let Some(state) = states.get(&entry.id) {
                let next_reason = state
                    .disabled_reason
                    .as_deref()
                    .and_then(DisabledReason::from_str);
                let next_disabled = entry.credentials.disabled || next_reason.is_some();
                let next_disabled_reason = if !next_disabled {
                    None
                } else if let Some(reason) = next_reason {
                    Some(reason)
                } else {
                    Some(DisabledReason::Manual)
                };

                changed |= entry.failure_count != state.failure_count
                    || entry.refresh_failure_count != state.refresh_failure_count
                    || entry.warmup_remaining != state.warmup_remaining
                    || entry.disabled != next_disabled
                    || entry.disabled_reason != next_disabled_reason;
                entry.failure_count = state.failure_count;
                entry.refresh_failure_count = state.refresh_failure_count;
                entry.warmup_remaining = state.warmup_remaining;
                entry.disabled = next_disabled;
                entry.disabled_reason = next_disabled_reason;
            }
        }
        tracing::info!("已从 PgSQL 加载 {} 条凭据运行态", states.len());
        changed
    }

    fn save_runtime_state_for(&self, id: u64) {
        let state = {
            let entries = self.entries.lock();
            let Some(entry) = entries.iter().find(|entry| entry.id == id) else {
                return;
            };
            Self::runtime_state_from_entry(entry)
        };
        if let Err(e) = self.persist_runtime_state_value(id, &state) {
            tracing::warn!("{}", e);
        }
    }

    fn persist_runtime_state_value(
        &self,
        id: u64,
        state: &CredentialRuntimeStateRow,
    ) -> anyhow::Result<bool> {
        let Some(store) = &self.postgres_store else {
            return Ok(false);
        };
        let store = store.clone();
        let snapshot = CredentialRuntimeStateSnapshot {
            state: state.clone(),
            updated_at: Utc::now(),
        };
        block_on_storage("保存凭据运行态到 PgSQL", async move {
            store
                .save_credential_runtime_state_snapshot(id, &snapshot)
                .await
        })?;
        Ok(true)
    }

    fn persist_success_state(
        &self,
        id: u64,
        last_used_at: &str,
        updated_at: chrono::DateTime<Utc>,
    ) {
        let runtime_state = {
            let entries = self.entries.lock();
            let Some(entry) = entries.iter().find(|entry| entry.id == id) else {
                return;
            };
            Self::runtime_state_from_entry(entry)
        };

        {
            let mut pending = self.pending_stats_deltas.lock();
            let entry = pending.entry(id).or_default();
            entry.success_delta = entry.success_delta.saturating_add(1);
            entry.last_used_at = Some(last_used_at.to_string());
        }
        {
            let mut pending = self.pending_runtime_state_snapshots.lock();
            let snapshot = CredentialRuntimeStateSnapshot {
                state: runtime_state,
                updated_at,
            };
            match pending.get_mut(&id) {
                Some(existing) if existing.updated_at > snapshot.updated_at => {}
                Some(existing) => *existing = snapshot,
                None => {
                    pending.insert(id, snapshot);
                }
            }
        }
        self.mark_stats_dirty();
    }

    fn queue_runtime_state_flush(
        &self,
        id: u64,
        last_used_at: Option<String>,
        updated_at: chrono::DateTime<Utc>,
    ) {
        let runtime_state = {
            let entries = self.entries.lock();
            let Some(entry) = entries.iter().find(|entry| entry.id == id) else {
                return;
            };
            Self::runtime_state_from_entry(entry)
        };

        if let Some(last_used_at) = last_used_at {
            let mut pending = self.pending_stats_deltas.lock();
            let entry = pending.entry(id).or_default();
            let replace = entry
                .last_used_at
                .as_ref()
                .is_none_or(|existing| &last_used_at > existing);
            if replace {
                entry.last_used_at = Some(last_used_at);
            }
        }
        {
            let mut pending = self.pending_runtime_state_snapshots.lock();
            let snapshot = CredentialRuntimeStateSnapshot {
                state: runtime_state,
                updated_at,
            };
            match pending.get_mut(&id) {
                Some(existing) if existing.updated_at > snapshot.updated_at => {}
                Some(existing) => *existing = snapshot,
                None => {
                    pending.insert(id, snapshot);
                }
            }
        }
        self.mark_stats_dirty();
    }

    fn delete_persisted_credential_state(&self, id: u64) -> anyhow::Result<bool> {
        let Some(store) = &self.postgres_store else {
            return Ok(false);
        };
        let store = store.clone();
        block_on_storage("删除凭据持久化状态", async move {
            store.soft_delete_credential(id).await?;
            store.delete_credential_stats_and_runtime(id).await
        })?;
        Ok(true)
    }

    fn refresh_scheduler_state_from_redis(&self) -> anyhow::Result<()> {
        if self.redis_store.is_none() {
            return Ok(());
        }

        let now = Instant::now();
        {
            let mut last_sync_at = self.last_scheduler_redis_sync_at.lock();
            if last_sync_at.is_some_and(|last| {
                now.saturating_duration_since(last) < SCHEDULER_REDIS_SYNC_MIN_INTERVAL
            }) {
                return Ok(());
            }
            *last_sync_at = Some(now);
        }

        self.refresh_scheduler_state_from_redis_force()
    }

    fn refresh_scheduler_state_from_redis_force(&self) -> anyhow::Result<()> {
        let Some(redis) = &self.redis_store else {
            return Ok(());
        };
        let ids: Vec<u64> = {
            let entries = self.entries.lock();
            entries.iter().map(|entry| entry.id).collect()
        };
        if ids.is_empty() {
            return Ok(());
        }

        let redis = redis.clone();
        if let Some(states) = self
            .block_on_scheduler_redis_hot("从 Redis 同步调度运行态", async move {
                redis.scheduler_state_for_credentials(&ids).await
            })
        {
            self.apply_scheduler_states(states);
            *self.last_scheduler_redis_sync_at.lock() = Some(Instant::now());
        }
        Ok(())
    }

    fn refresh_scheduler_state_from_redis_for_ids(&self, ids: &HashSet<u64>) -> anyhow::Result<()> {
        let Some(redis) = &self.redis_store else {
            return Ok(());
        };
        if ids.is_empty() {
            return Ok(());
        }
        let ids: Vec<u64> = {
            let entries = self.entries.lock();
            entries
                .iter()
                .filter(|entry| ids.contains(&entry.id))
                .map(|entry| entry.id)
                .collect()
        };
        if ids.is_empty() {
            return Ok(());
        }
        let redis = redis.clone();
        if let Some(states) = self
            .block_on_scheduler_redis_hot("从 Redis 同步指定凭据调度运行态", async move {
                redis.scheduler_state_for_credentials(&ids).await
            })
        {
            self.apply_scheduler_states(states);
            Ok(())
        } else {
            anyhow::bail!("Redis 调度运行态未在热路径超时内返回")
        }
    }

    fn refresh_scheduler_state_from_redis_best_effort(&self) {
        if let Err(err) = self.refresh_scheduler_state_from_redis() {
            tracing::warn!("从 Redis 同步调度运行态失败: {}", err);
        }
    }

    fn refresh_scheduler_state_from_redis_force_best_effort(&self) {
        if let Err(err) = self.refresh_scheduler_state_from_redis_force() {
            tracing::warn!("从 Redis 同步调度运行态失败: {}", err);
        }
    }

    fn apply_scheduler_states(&self, states: HashMap<u64, SchedulerCredentialState>) {
        let now_ms = Utc::now().timestamp_millis();
        let now = Instant::now();
        let global_rpm = self.config.lock().credential_rpm.unwrap_or(0);
        let mut entries = self.entries.lock();
        for entry in entries.iter_mut() {
            let state = states.get(&entry.id).cloned().unwrap_or_default();
            entry.cooldown_until = state
                .cooldown
                .as_ref()
                .and_then(|cooldown| instant_from_epoch_ms(cooldown.until_ms, now_ms, now));
            entry.cooldown_reason = state.cooldown.and_then(|cooldown| cooldown.reason);
            entry.model_cooldowns.clear();
            entry.model_health.clear();
            for model_state in state.model_states {
                let key = model_state_key(&model_state.model);
                if let Some(cooldown) = model_state.cooldown {
                    if let Some(until) = instant_from_epoch_ms(cooldown.until_ms, now_ms, now) {
                        entry.model_cooldowns.insert(
                            key.clone(),
                            CredentialModelCooldown {
                                model: model_state.model.clone(),
                                until,
                                reason: cooldown.reason,
                            },
                        );
                    }
                }
                entry.model_health.insert(key, model_state.health);
            }
            entry.rate_limit_available_at =
                if rate_limit_interval_for_rpm(effective_rpm(entry, global_rpm)).is_some() {
                    let redis_available_at = state
                        .rate_limit_available_at_ms
                        .and_then(|until_ms| instant_from_epoch_ms(until_ms, now_ms, now));
                    match (entry.rate_limit_available_at, redis_available_at) {
                        (Some(local), Some(redis)) => Some(local.max(redis)),
                        (Some(local), None) if local > now => Some(local),
                        (_, redis) => redis,
                    }
                } else {
                    None
                };
            entry.in_flight_leases = state
                .in_flight_leases
                .into_iter()
                .map(|lease| InFlightLease {
                    id: lease.id,
                    acquired_at: instant_from_elapsed_epoch_ms(lease.acquired_at_ms, now_ms, now),
                    last_seen_at: instant_from_elapsed_epoch_ms(lease.last_seen_at_ms, now_ms, now),
                    kind: InFlightKind::from_str(&lease.kind),
                })
                .collect();
            entry.in_flight_requests = entry.in_flight_leases.len() as u32;
            entry.health = state.health;
        }
        drop(entries);
        self.invalidate_local_pool_route_state_cache();
    }

    fn record_scheduler_selection(&self, id: u64) {
        let now = Instant::now();
        {
            let mut entries = self.entries.lock();
            if let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) {
                record_local_selection(entry, now);
            }
        }
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            spawn_best_effort_storage_task("记录 Redis 调度选中次数", async move {
                redis.record_scheduler_selection(id).await?;
                Ok(())
            });
        }
        let mut pending = self.pending_stats_deltas.lock();
        let entry = pending.entry(id).or_default();
        entry.selection_delta = entry.selection_delta.saturating_add(1);
        self.mark_stats_dirty();
    }

    fn clear_scheduler_state_for_credential(&self, id: u64, clear_in_flight: bool) {
        {
            let mut entries = self.entries.lock();
            if let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) {
                entry.cooldown_until = None;
                entry.cooldown_reason = None;
                entry.model_cooldowns.clear();
                entry.rate_limit_available_at = None;
                entry.health = SchedulerHealthState::default();
                entry.model_health.clear();
                if clear_in_flight {
                    entry.in_flight_requests = 0;
                    entry.in_flight_leases.clear();
                }
            }
        }
        self.invalidate_local_pool_route_state_cache();
        let Some(redis) = &self.redis_store else {
            return;
        };
        let redis = redis.clone();
        spawn_best_effort_storage_task("清理 Redis 凭据调度状态", async move {
            redis.clear_scheduler_cooldown(id).await?;
            redis.clear_scheduler_health(id).await?;
            redis.clear_rate_limit(id).await?;
            redis.delete_sessions_for_credential(id).await?;
            if clear_in_flight {
                redis.clear_in_flight_leases(id, None).await?;
            }
            Ok(())
        });
    }

    /// 报告指定凭据 API 调用成功
    ///
    /// 重置该凭据的失败计数
    ///
    /// # Arguments
    /// * `id` - 凭据 ID（来自 CallContext）
    #[allow(dead_code)]
    pub fn report_success(&self, id: u64) {
        self.report_success_with_latency(id, None, None);
    }

    pub fn report_success_with_latency(
        &self,
        id: u64,
        model: Option<&str>,
        latency: Option<StdDuration>,
    ) {
        let mut last_used_at: Option<String> = None;
        let mut success_at: Option<DateTime<Utc>> = None;
        let alpha = self
            .config
            .lock()
            .scheduler_error_ewma_alpha
            .clamp(0.01, 1.0);
        {
            let mut entries = self.entries.lock();
            if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                entry.failure_count = 0;
                entry.refresh_failure_count = 0;
                if entry.warmup_remaining > 0 {
                    entry.warmup_remaining -= 1;
                }
                entry.success_count += 1;
                let now_at = Utc::now();
                let now = now_at.to_rfc3339();
                entry.last_used_at = Some(now.clone());
                {
                    let health = entry_effective_health_mut(entry, model);
                    health.recent_error_rate *= 1.0 - alpha;
                    health.transient_failure_streak =
                        health.transient_failure_streak.saturating_sub(1);
                    if let Some(latency) = latency {
                        let latency_ms = latency.as_millis() as f64;
                        health.latency_ewma_ms = Some(
                            health
                                .latency_ewma_ms
                                .map(|previous| previous + alpha * (latency_ms - previous))
                                .unwrap_or(latency_ms),
                        );
                    }
                }
                last_used_at = Some(now);
                success_at = Some(now_at);
                tracing::debug!(
                    "凭据 #{} API 调用成功（累计 {} 次）",
                    id,
                    entry.success_count
                );
            }
        }
        if let (Some(last_used_at), Some(success_at)) = (last_used_at, success_at) {
            self.persist_success_state(id, &last_used_at, success_at);
        }
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            let model_for_redis = model.map(str::to_string);
            spawn_best_effort_storage_task("记录 Redis 调度成功健康状态", async move {
                redis
                    .record_scheduler_success(id, model_for_redis.as_deref(), latency, alpha)
                    .await?;
                Ok(())
            });
        }
        self.notify_dispatch_state_changed();
    }

    /// 报告指定凭据遇到上游瞬态错误，按 Retry-After 或默认值临时冷却。
    ///
    /// 不增加 failure_count，不禁用凭据；冷却到期后自动重新参与调度。
    #[allow(dead_code)]
    pub fn report_transient_failure(
        &self,
        id: u64,
        model: Option<&str>,
        retry_after: Option<StdDuration>,
        reason: impl Into<String>,
    ) -> anyhow::Result<bool> {
        self.report_transient_failure_kind(
            id,
            model,
            TransientFailureKind::Protocol,
            retry_after,
            reason,
        )
    }

    pub fn report_transient_failure_kind(
        &self,
        id: u64,
        model: Option<&str>,
        kind: TransientFailureKind,
        retry_after: Option<StdDuration>,
        reason: impl Into<String>,
    ) -> anyhow::Result<bool> {
        let reason = reason.into();
        let (base, max, multiplier, jitter, probation, alpha) =
            self.transient_failure_settings(kind);
        let now = Instant::now();
        let now_ms = Utc::now().timestamp_millis();

        {
            let mut entries = self.entries.lock();
            let Some(entry) = entries.iter_mut().find(|e| e.id == id) else {
                return Ok(entries.iter().any(|e| !e.disabled));
            };

            if entry.disabled {
                return Ok(entries.iter().any(|e| !e.disabled));
            }
        }

        let duration = {
            let mut entries = self.entries.lock();
            let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) else {
                return Ok(entries.iter().any(|entry| !entry.disabled));
            };
            let streak = {
                let health = entry_effective_health_mut(entry, model);
                health.transient_failure_streak = health.transient_failure_streak.saturating_add(1);
                health.recent_error_rate += alpha * (1.0 - health.recent_error_rate);
                health.last_error_kind = Some(kind.as_str().to_string());
                health.last_error_reason = Some(reason.clone());
                health.last_error_at_ms = Some(now_ms);
                health.transient_failure_streak
            };
            let duration =
                Self::local_cooldown_duration(retry_after, base, max, multiplier, jitter, streak);
            let until = now + duration;
            if let Some(model) = model.map(str::trim).filter(|value| !value.is_empty()) {
                let key = model_state_key(model);
                let should_update = entry
                    .model_cooldowns
                    .get(&key)
                    .is_none_or(|existing| until > existing.until);
                if should_update {
                    entry.model_cooldowns.insert(
                        key,
                        CredentialModelCooldown {
                            model: model.to_string(),
                            until,
                            reason: Some(reason.clone()),
                        },
                    );
                }
            } else if entry.cooldown_until.is_none_or(|existing| until > existing) {
                entry.cooldown_until = Some(until);
                entry.cooldown_reason = Some(reason.clone());
            }
            let health = entry_effective_health_mut(entry, model);
            health.probation_until_ms = Some(
                health
                    .probation_until_ms
                    .unwrap_or(0)
                    .max(now_ms + duration.as_millis() as i64 + probation.as_millis() as i64),
            );
            entry.last_used_at = Some(Utc::now().to_rfc3339());
            duration
        };

        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            let reason_for_redis = reason.clone();
            let model_for_redis = model.map(str::to_string);
            spawn_best_effort_storage_task("写入 Redis 凭据临时冷却与健康状态", async move {
                redis
                    .record_scheduler_transient_failure(
                        id,
                        model_for_redis.as_deref(),
                        kind.as_str(),
                        &reason_for_redis,
                        retry_after,
                        base,
                        max,
                        multiplier,
                        jitter,
                        probation,
                        alpha,
                    )
                    .await?;
                Ok(())
            });
        }

        tracing::warn!(
            "凭据 #{} 因 {} 瞬态错误进入临时冷却 {} 秒: {}",
            id,
            kind.as_str(),
            duration.as_secs(),
            reason
        );

        self.invalidate_local_pool_route_state_cache();
        let has_alternate = {
            let entries = self.entries.lock();
            let proxy_resources = self.proxy_resources.lock();
            let max_concurrent_requests = self.max_concurrent_requests();
            entries.iter().any(|e| {
                e.id != id
                    && credential_is_dispatchable(
                        &proxy_resources,
                        e,
                        model,
                        Instant::now(),
                        max_concurrent_requests,
                    )
            })
        };
        self.notify_dispatch_state_changed();
        Ok(has_alternate)
    }

    /// 报告指定会话在该凭据上的 API 调用成功，并清理 sticky 软失败计数。
    #[allow(dead_code)]
    pub fn report_success_for_session(&self, id: u64, session_id: Option<&str>) {
        self.report_success_for_session_with_latency(id, None, session_id, None);
    }

    pub fn report_success_for_session_with_latency(
        &self,
        id: u64,
        model: Option<&str>,
        session_id: Option<&str>,
        latency: Option<StdDuration>,
    ) {
        self.report_success_with_latency(id, model, latency);
        if let Some(sid) = session_id {
            self.clear_session_soft_failure(sid, id);
        }
    }

    /// 报告指定凭据 API 调用失败
    ///
    /// 增加失败计数，达到阈值时禁用凭据并切换到优先级最高的可用凭据
    /// 返回是否还有可用凭据可以重试
    ///
    /// # Arguments
    /// * `id` - 凭据 ID（来自 CallContext）
    pub fn report_failure(&self, id: u64) -> bool {
        let last_used_at: String;
        let _available_after_local_update = {
            let mut entries = self.entries.lock();
            let mut current_id = self.current_id.lock();

            let entry = match entries.iter_mut().find(|e| e.id == id) {
                Some(e) => e,
                None => return entries.iter().any(|e| !e.disabled),
            };

            if entry.disabled {
                return entries.iter().any(|e| !e.disabled);
            }

            entry.failure_count += 1;
            let now = Utc::now().to_rfc3339();
            entry.last_used_at = Some(now.clone());
            last_used_at = now;
            let failure_count = entry.failure_count;

            tracing::warn!(
                "凭据 #{} API 调用失败（{}/{}）",
                id,
                failure_count,
                MAX_FAILURES_PER_CREDENTIAL
            );

            if failure_count >= MAX_FAILURES_PER_CREDENTIAL {
                entry.disabled = true;
                entry.disabled_reason = Some(DisabledReason::TooManyFailures);
                tracing::error!("凭据 #{} 已连续失败 {} 次，已被禁用", id, failure_count);

                // 切换到优先级最高的可用凭据
                if let Some(next) = entries
                    .iter()
                    .filter(|e| !e.disabled)
                    .min_by_key(|e| e.credentials.priority)
                {
                    *current_id = next.id;
                    tracing::info!(
                        "已切换到凭据 #{}（优先级 {}）",
                        next.id,
                        next.credentials.priority
                    );
                } else {
                    tracing::error!("所有凭据均已禁用！");
                }
            }

            entries.iter().any(|e| !e.disabled)
        };
        let disabled = {
            let entries = self.entries.lock();
            entries.iter().any(|e| e.id == id && e.disabled)
        };
        if disabled {
            self.select_highest_priority();
            self.unbind_sessions_for_credential(id);
            self.clear_scheduler_state_for_credential(id, false);
        }
        self.queue_runtime_state_flush(id, Some(last_used_at.clone()), Utc::now());
        if disabled {
            self.save_runtime_state_for(id);
            self.record_scheduler_credential_audit(
                "auto_disable_credential",
                id,
                DisabledReason::TooManyFailures,
                "api_failure_threshold",
                "连续 API 调用失败达到阈值，已自动禁用凭据",
                serde_json::json!({}),
            );
        }
        let result = {
            let entries = self.entries.lock();
            entries.iter().any(|e| !e.disabled)
        };
        if disabled {
            self.publish_credentials_changed("credential_failure_reported");
        }
        self.notify_dispatch_state_changed();
        result
    }

    /// 报告指定凭据额度已用尽
    ///
    /// 用于处理 402 Payment Required 且 reason 表示额度用尽的场景：
    /// - 立即禁用该凭据（不等待连续失败阈值）
    /// - 切换到下一个可用凭据继续重试
    /// - 返回是否还有可用凭据
    pub fn report_quota_exhausted(&self, id: u64) -> bool {
        let last_used_at: String;
        let result = {
            let mut entries = self.entries.lock();
            let mut current_id = self.current_id.lock();

            let entry = match entries.iter_mut().find(|e| e.id == id) {
                Some(e) => e,
                None => return entries.iter().any(|e| !e.disabled),
            };

            if entry.disabled {
                return entries.iter().any(|e| !e.disabled);
            }

            entry.disabled = true;
            entry.disabled_reason = Some(DisabledReason::QuotaExceeded);
            let now = Utc::now().to_rfc3339();
            entry.last_used_at = Some(now.clone());
            last_used_at = now;
            // 设为阈值，便于在管理面板中直观看到该凭据已不可用
            entry.failure_count = MAX_FAILURES_PER_CREDENTIAL;

            tracing::error!("凭据 #{} 额度已用尽，已被禁用", id);

            // 切换到优先级最高的可用凭据
            if let Some(next) = entries
                .iter()
                .filter(|e| !e.disabled)
                .min_by_key(|e| e.credentials.priority)
            {
                *current_id = next.id;
                tracing::info!(
                    "已切换到凭据 #{}（优先级 {}）",
                    next.id,
                    next.credentials.priority
                );
                true
            } else {
                tracing::error!("所有凭据均已禁用！");
                false
            }
        };
        self.unbind_sessions_for_credential(id);
        self.clear_scheduler_state_for_credential(id, false);
        self.queue_runtime_state_flush(id, Some(last_used_at.clone()), Utc::now());
        self.save_runtime_state_for(id);
        self.record_scheduler_credential_audit(
            "auto_disable_credential",
            id,
            DisabledReason::QuotaExceeded,
            "upstream_quota_exhausted",
            "上游返回额度耗尽，已自动禁用凭据并切换调度",
            serde_json::json!({
                "failureCountSetTo": MAX_FAILURES_PER_CREDENTIAL,
            }),
        );
        self.publish_credentials_changed("credential_quota_exhausted");
        self.notify_dispatch_state_changed();
        result
    }

    /// 报告指定凭据被上游明确风控、暂停或锁定。
    ///
    /// 这类错误不是普通瞬态 429，也不是连续失败阈值问题；继续调度该凭据通常只会
    /// 放大风控。这里立即禁用并记录独立原因，后台可通过 reset/enable 人工恢复。
    pub fn report_risk_controlled(
        &self,
        id: u64,
        reason: CredentialRiskControlReason,
        detail: impl Into<String>,
    ) -> bool {
        let detail = detail.into();
        let detail_summary = truncate_for_audit(&detail, 500);
        let disabled_reason = reason.disabled_reason();
        let last_used_at: String;
        let result = {
            let mut entries = self.entries.lock();
            let mut current_id = self.current_id.lock();

            let entry = match entries.iter_mut().find(|e| e.id == id) {
                Some(e) => e,
                None => return entries.iter().any(|e| !e.disabled),
            };

            if entry.disabled {
                return entries.iter().any(|e| !e.disabled);
            }

            entry.disabled = true;
            entry.disabled_reason = Some(disabled_reason);
            let now = Utc::now().to_rfc3339();
            entry.last_used_at = Some(now.clone());
            last_used_at = now;
            entry.failure_count = MAX_FAILURES_PER_CREDENTIAL;

            tracing::error!(
                credential_id = id,
                reason = reason.event_reason(),
                detail = %detail,
                "凭据 #{} 命中上游{}，已被禁用",
                id,
                reason.label()
            );

            if let Some(next) = entries
                .iter()
                .filter(|e| !e.disabled)
                .min_by_key(|e| e.credentials.priority)
            {
                *current_id = next.id;
                tracing::info!(
                    "已切换到凭据 #{}（优先级 {}）",
                    next.id,
                    next.credentials.priority
                );
                true
            } else {
                tracing::error!("所有凭据均已禁用！");
                false
            }
        };
        self.unbind_sessions_for_credential(id);
        self.clear_scheduler_state_for_credential(id, false);
        self.queue_runtime_state_flush(id, Some(last_used_at.clone()), Utc::now());
        self.save_runtime_state_for(id);
        if let Some(store) = &self.postgres_store {
            let store = store.clone();
            let event_reason = reason.event_reason().to_string();
            let detail_value = serde_json::json!({
                "reason": event_reason,
                "detail": detail,
            });
            spawn_best_effort_storage_task("记录凭据风控事件到 PgSQL", async move {
                store
                    .record_credential_event(
                        Some(id),
                        "credential_risk_controlled",
                        Some(&event_reason),
                        detail_value,
                    )
                    .await
            });
        }
        self.record_scheduler_credential_audit(
            "auto_disable_credential",
            id,
            disabled_reason,
            "upstream_risk_controlled",
            "上游返回风控、暂停或锁定状态，已自动禁用凭据并切换调度",
            serde_json::json!({
                "riskReason": reason.event_reason(),
                "upstreamErrorSummary": detail_summary,
                "failureCountSetTo": MAX_FAILURES_PER_CREDENTIAL,
            }),
        );
        self.publish_credentials_changed("credential_risk_controlled");
        self.notify_dispatch_state_changed();
        result
    }

    /// 报告指定凭据刷新 Token 失败。
    ///
    /// 连续刷新失败达到阈值后禁用凭据并切换，阈值内保持当前凭据不切换，
    /// 与 API 401/403 的累计失败策略保持一致。
    pub fn report_refresh_failure(&self, id: u64) -> bool {
        let last_used_at: String;
        let _available_after_local_update = {
            let mut entries = self.entries.lock();
            let mut current_id = self.current_id.lock();

            let entry = match entries.iter_mut().find(|e| e.id == id) {
                Some(e) => e,
                None => return entries.iter().any(|e| !e.disabled),
            };

            if entry.disabled {
                return entries.iter().any(|e| !e.disabled);
            }

            let now = Utc::now().to_rfc3339();
            entry.last_used_at = Some(now.clone());
            last_used_at = now;
            entry.refresh_failure_count += 1;
            let refresh_failure_count = entry.refresh_failure_count;

            tracing::warn!(
                "凭据 #{} Token 刷新失败（{}/{}）",
                id,
                refresh_failure_count,
                MAX_FAILURES_PER_CREDENTIAL
            );

            if refresh_failure_count < MAX_FAILURES_PER_CREDENTIAL {
                return entries.iter().any(|e| !e.disabled);
            }

            entry.disabled = true;
            entry.disabled_reason = Some(DisabledReason::TooManyRefreshFailures);

            tracing::error!(
                "凭据 #{} Token 已连续刷新失败 {} 次，已被禁用",
                id,
                refresh_failure_count
            );

            if let Some(next) = entries
                .iter()
                .filter(|e| !e.disabled)
                .min_by_key(|e| e.credentials.priority)
            {
                *current_id = next.id;
                tracing::info!(
                    "已切换到凭据 #{}（优先级 {}）",
                    next.id,
                    next.credentials.priority
                );
                true
            } else {
                tracing::error!("所有凭据均已禁用！");
                false
            }
        };
        let disabled = {
            let entries = self.entries.lock();
            entries.iter().any(|e| e.id == id && e.disabled)
        };
        if disabled {
            self.select_highest_priority();
            self.unbind_sessions_for_credential(id);
            self.clear_scheduler_state_for_credential(id, false);
        }
        self.queue_runtime_state_flush(id, Some(last_used_at.clone()), Utc::now());
        if disabled {
            self.save_runtime_state_for(id);
            self.record_scheduler_credential_audit(
                "auto_disable_credential",
                id,
                DisabledReason::TooManyRefreshFailures,
                "token_refresh_failure_threshold",
                "连续 Token 刷新失败达到阈值，已自动禁用凭据",
                serde_json::json!({}),
            );
        }
        let result = {
            let entries = self.entries.lock();
            entries.iter().any(|e| !e.disabled)
        };
        if disabled {
            self.publish_credentials_changed("credential_refresh_failure_reported");
        }
        self.notify_dispatch_state_changed();
        result
    }

    /// 报告指定凭据的 refreshToken 永久失效（invalid_grant）。
    ///
    /// 立即禁用凭据，不累计、不重试。
    /// 返回是否还有可用凭据。
    pub fn report_refresh_token_invalid(&self, id: u64) -> bool {
        let last_used_at: String;
        let result = {
            let mut entries = self.entries.lock();
            let mut current_id = self.current_id.lock();

            let entry = match entries.iter_mut().find(|e| e.id == id) {
                Some(e) => e,
                None => return entries.iter().any(|e| !e.disabled),
            };

            if entry.disabled {
                return entries.iter().any(|e| !e.disabled);
            }

            let now = Utc::now().to_rfc3339();
            entry.last_used_at = Some(now.clone());
            last_used_at = now;
            entry.disabled = true;
            entry.disabled_reason = Some(DisabledReason::InvalidRefreshToken);

            tracing::error!(
                "凭据 #{} refreshToken 已失效 (invalid_grant)，已立即禁用",
                id
            );

            if let Some(next) = entries
                .iter()
                .filter(|e| !e.disabled)
                .min_by_key(|e| e.credentials.priority)
            {
                *current_id = next.id;
                tracing::info!(
                    "已切换到凭据 #{}（优先级 {}）",
                    next.id,
                    next.credentials.priority
                );
                true
            } else {
                tracing::error!("所有凭据均已禁用！");
                false
            }
        };
        self.unbind_sessions_for_credential(id);
        self.clear_scheduler_state_for_credential(id, false);
        self.queue_runtime_state_flush(id, Some(last_used_at.clone()), Utc::now());
        self.save_runtime_state_for(id);
        self.record_scheduler_credential_audit(
            "auto_disable_credential",
            id,
            DisabledReason::InvalidRefreshToken,
            "refresh_token_invalid_grant",
            "refreshToken 永久失效，已自动禁用凭据并切换调度",
            serde_json::json!({
                "upstreamReason": "invalid_grant",
            }),
        );
        self.publish_credentials_changed("credential_refresh_token_invalid");
        self.notify_dispatch_state_changed();
        result
    }

    /// 切换到优先级最高的可用凭据
    ///
    /// 返回是否成功切换
    pub fn switch_to_next(&self) -> bool {
        let entries = self.entries.lock();
        let mut current_id = self.current_id.lock();

        // 选择优先级最高的未禁用凭据（排除当前凭据）
        if let Some(next) = entries
            .iter()
            .filter(|e| !e.disabled && e.id != *current_id)
            .min_by_key(|e| e.credentials.priority)
        {
            *current_id = next.id;
            tracing::info!(
                "已切换到凭据 #{}（优先级 {}）",
                next.id,
                next.credentials.priority
            );
            true
        } else {
            // 没有其他可用凭据，检查当前凭据是否可用
            entries.iter().any(|e| e.id == *current_id && !e.disabled)
        }
    }

    // ========================================================================
    // Admin API 方法
    // ========================================================================

    /// 获取管理器状态快照（用于 Admin API）
    pub fn snapshot(&self) -> ManagerSnapshot {
        self.cleanup_expired_in_flight_leases_local_first();
        self.refresh_stats_from_postgres();
        self.refresh_scheduler_state_from_redis_best_effort();
        let config = self.config.lock().clone();
        let mut entries = self.entries.lock();
        if self.redis_store.is_none() {
            refresh_local_selection_windows_locked(&mut entries, Instant::now());
        }
        let current_id = *self.current_id.lock();
        let available = entries.iter().filter(|e| !e.disabled).count();
        let now = Instant::now();
        let now_ms = Utc::now().timestamp_millis();
        let max_concurrent_requests = config.credential_max_concurrent_requests;
        let lease_max_age = (config.credential_in_flight_lease_max_secs > 0)
            .then(|| StdDuration::from_secs(config.credential_in_flight_lease_max_secs));
        let local_global_in_flight = entries
            .iter()
            .map(|entry| entry.in_flight_requests)
            .sum::<u32>();
        let global_capacity = SchedulerGlobalCapacityState {
            in_flight_requests: local_global_in_flight,
            queued_requests: self.queued_requests.load(Ordering::Acquire),
        };
        let proxy_resources = self.proxy_resources.lock();
        let score_candidates: Vec<_> = entries
            .iter()
            .filter(|entry| {
                credential_is_dispatchable(
                    &proxy_resources,
                    entry,
                    None,
                    now,
                    max_concurrent_requests,
                )
            })
            .collect();
        let score_total_recent: u64 = score_candidates
            .iter()
            .map(|entry| entry.health.recent_selection_count_60s as u64)
            .sum();
        let score_candidate_count = score_candidates.len();
        let entries_snapshot = entries
            .iter()
            .map(|entry| {
                self.runtime_snapshot_from_entry(
                    entry,
                    &config,
                    &proxy_resources,
                    max_concurrent_requests,
                    lease_max_age,
                    now,
                    now_ms,
                    score_total_recent,
                    score_candidate_count,
                )
            })
            .collect();

        ManagerSnapshot {
            entries: entries_snapshot,
            current_id,
            total: entries.len(),
            available,
            global_in_flight_requests: global_capacity.in_flight_requests,
            queued_requests: global_capacity.queued_requests,
            global_max_concurrent_requests: config.dispatch_global_max_concurrent_requests,
            max_queued_requests: config.dispatch_max_queued_requests,
        }
    }

    /// 获取轻量凭据基础快照（用于 Admin 列表首屏）。
    ///
    /// 该路径不主动同步 PgSQL/Redis，也不计算调度评分，避免列表请求被外部存储拖慢。
    pub fn base_snapshot(&self) -> ManagerBaseSnapshot {
        self.cleanup_expired_in_flight_leases_local_first();
        let config = self.config.lock().clone();
        let entries = self.entries.lock();
        let current_id = *self.current_id.lock();
        let available = entries.iter().filter(|e| !e.disabled).count();
        let global_in_flight_requests = entries.iter().map(|entry| entry.in_flight_requests).sum();
        let proxy_resources = self.proxy_resources.lock();
        let entries_snapshot = entries
            .iter()
            .map(|entry| self.base_snapshot_from_entry(entry, &config, &proxy_resources))
            .collect();

        ManagerBaseSnapshot {
            entries: entries_snapshot,
            current_id,
            total: entries.len(),
            available,
            global_in_flight_requests,
            queued_requests: self.queued_requests.load(Ordering::Acquire),
            global_max_concurrent_requests: config.dispatch_global_max_concurrent_requests,
            max_queued_requests: config.dispatch_max_queued_requests,
            runtime_fresh: self.redis_store.is_none(),
        }
    }

    /// 获取凭据数量和全局调度容量快照。
    ///
    /// 该路径只读计数和全局容量，不构造每个凭据的运行态详情，供 Admin 顶部概览高频轮询使用。
    pub fn summary_snapshot(&self) -> ManagerSummarySnapshot {
        self.cleanup_expired_in_flight_leases_local_first();
        let config = self.config.lock().clone();
        let (current_id, total, available, local_global_in_flight) = {
            let entries = self.entries.lock();
            (
                *self.current_id.lock(),
                entries.len(),
                entries.iter().filter(|e| !e.disabled).count(),
                entries.iter().map(|entry| entry.in_flight_requests).sum(),
            )
        };
        let (global_capacity, runtime_fresh) = if self.redis_store.is_some() {
            (self.global_capacity_state(), true)
        } else {
            (
                SchedulerGlobalCapacityState {
                    in_flight_requests: local_global_in_flight,
                    queued_requests: self.queued_requests.load(Ordering::Acquire),
                },
                true,
            )
        };

        ManagerSummarySnapshot {
            current_id,
            total,
            available,
            global_in_flight_requests: global_capacity.in_flight_requests,
            queued_requests: global_capacity.queued_requests,
            global_max_concurrent_requests: config.dispatch_global_max_concurrent_requests,
            max_queued_requests: config.dispatch_max_queued_requests,
            runtime_fresh,
        }
    }

    /// 获取指定凭据的运行态快照（用于 Admin 当前页补充）。
    pub fn runtime_snapshot_for_ids(&self, ids: &[u64]) -> ManagerRuntimeSnapshot {
        self.cleanup_expired_in_flight_leases_local_first();
        let ids: HashSet<u64> = ids.iter().copied().collect();
        let mut runtime_fresh = self.redis_store.is_none();
        if self.redis_store.is_some() && !ids.is_empty() {
            runtime_fresh = match self.refresh_scheduler_state_from_redis_for_ids(&ids) {
                Ok(()) => true,
                Err(err) => {
                    tracing::warn!("从 Redis 同步指定凭据调度运行态失败: {}", err);
                    false
                }
            };
        }

        let config = self.config.lock().clone();
        let mut entries = self.entries.lock();
        let now = Instant::now();
        if self.redis_store.is_none() {
            refresh_local_selection_windows_locked(&mut entries, now);
        }
        let current_id = *self.current_id.lock();
        let available = entries.iter().filter(|e| !e.disabled).count();
        let now_ms = Utc::now().timestamp_millis();
        let max_concurrent_requests = config.credential_max_concurrent_requests;
        let lease_max_age = (config.credential_in_flight_lease_max_secs > 0)
            .then(|| StdDuration::from_secs(config.credential_in_flight_lease_max_secs));
        let proxy_resources = self.proxy_resources.lock();
        let score_candidates: Vec<_> = entries
            .iter()
            .filter(|entry| {
                credential_is_dispatchable(
                    &proxy_resources,
                    entry,
                    None,
                    now,
                    max_concurrent_requests,
                )
            })
            .collect();
        let score_total_recent: u64 = score_candidates
            .iter()
            .map(|entry| entry.health.recent_selection_count_60s as u64)
            .sum();
        let score_candidate_count = score_candidates.len();
        let entries_snapshot = entries
            .iter()
            .filter(|entry| ids.is_empty() || ids.contains(&entry.id))
            .map(|entry| {
                self.runtime_snapshot_from_entry(
                    entry,
                    &config,
                    &proxy_resources,
                    max_concurrent_requests,
                    lease_max_age,
                    now,
                    now_ms,
                    score_total_recent,
                    score_candidate_count,
                )
            })
            .collect();

        ManagerRuntimeSnapshot {
            entries: entries_snapshot,
            current_id,
            total: entries.len(),
            available,
            runtime_fresh,
        }
    }

    /// 导出完整凭据快照（包含 refreshToken / kiroApiKey 等敏感字段）。
    ///
    /// 仅供 Admin API 显式导出使用；不改变调度状态，也不触发持久化。
    pub fn export_credentials(&self) -> Vec<KiroCredentials> {
        let mut credentials: Vec<KiroCredentials> = {
            let entries = self.entries.lock();
            entries
                .iter()
                .map(|entry| {
                    let mut credentials = entry.credentials.clone();
                    credentials.canonicalize_auth_method();
                    credentials.disabled = entry.disabled;
                    credentials
                })
                .collect()
        };

        credentials.sort_by_key(|credential| (credential.priority, credential.id.unwrap_or(0)));
        credentials
    }

    /// 设置凭据禁用状态（Admin API）
    pub fn set_disabled(&self, id: u64, disabled: bool) -> anyhow::Result<()> {
        let (credential, runtime_state) = {
            let entries = self.entries.lock();
            let entry = entries
                .iter()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            let mut credential = Self::credential_from_entry(entry);
            let mut runtime_state = Self::runtime_state_from_entry(entry);
            credential.disabled = disabled;
            if disabled {
                runtime_state.disabled_reason = Some(DisabledReason::Manual.as_str().to_string());
            } else {
                runtime_state.failure_count = 0;
                runtime_state.refresh_failure_count = 0;
                runtime_state.disabled_reason = None;
            }
            (credential, runtime_state)
        };
        self.persist_credential_value(&credential)?;
        self.persist_runtime_state_value(id, &runtime_state)?;

        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            entry.disabled = disabled;
            if !disabled {
                // 启用时重置失败计数
                entry.failure_count = 0;
                entry.refresh_failure_count = 0;
                entry.disabled_reason = None;
                entry.cooldown_until = None;
                entry.cooldown_reason = None;
                entry.model_cooldowns.clear();
                entry.rate_limit_available_at = None;
            } else {
                entry.disabled_reason = Some(DisabledReason::Manual);
                entry.cooldown_until = None;
                entry.cooldown_reason = None;
                entry.model_cooldowns.clear();
                entry.rate_limit_available_at = None;
            }
        }
        if disabled {
            self.unbind_sessions_for_credential(id);
            self.clear_scheduler_state_for_credential(id, false);
        } else {
            self.clear_scheduler_state_for_credential(id, false);
            self.select_highest_priority();
        }
        self.notify_dispatch_state_changed();
        self.publish_credentials_changed("credential_disabled_updated");
        Ok(())
    }

    /// 设置凭据优先级（Admin API）
    ///
    /// 修改优先级后会立即按新优先级重新选择当前凭据。
    pub fn set_priority(&self, id: u64, priority: u32) -> anyhow::Result<()> {
        let credential = {
            let entries = self.entries.lock();
            let entry = entries
                .iter()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            let mut credential = Self::credential_from_entry(entry);
            credential.priority = priority;
            credential
        };
        self.persist_credential_value(&credential)?;

        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            entry.credentials.priority = priority;
        }
        // 立即按新优先级重新选择当前凭据（无论持久化是否成功）
        self.select_highest_priority();
        self.notify_dispatch_state_changed();
        self.publish_credentials_changed("credential_priority_updated");
        Ok(())
    }

    /// 设置凭据级最大并发覆盖（Admin API）。
    ///
    /// `None` 表示继承全局 `credentialMaxConcurrentRequests`；
    /// `Some(0)` 表示该凭据不限并发；
    /// `Some(n)` 表示该凭据最多同时处理 n 个请求。
    pub fn set_credential_max_concurrent_requests(
        &self,
        id: u64,
        max_concurrent_requests: Option<u32>,
    ) -> anyhow::Result<()> {
        let credential = {
            let entries = self.entries.lock();
            let entry = entries
                .iter()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            let mut credential = Self::credential_from_entry(entry);
            credential.max_concurrent_requests = max_concurrent_requests;
            credential
        };
        self.persist_credential_value(&credential)?;

        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            entry.credentials.max_concurrent_requests = credential.max_concurrent_requests;
        }

        self.notify_dispatch_state_changed();
        self.publish_credentials_changed("credential_concurrency_updated");
        Ok(())
    }

    /// 设置凭据级 RPM 覆盖（Admin API）。
    ///
    /// `None` 表示继承全局 `credentialRpm`；
    /// `Some(0)` 表示该凭据不做本地 RPM 限制；
    /// `Some(n)` 表示该凭据每分钟最多调度 n 次。
    pub fn set_credential_rpm(&self, id: u64, rpm: Option<u32>) -> anyhow::Result<()> {
        let credential = {
            let entries = self.entries.lock();
            let entry = entries
                .iter()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            let mut credential = Self::credential_from_entry(entry);
            credential.rpm = rpm;
            credential
        };
        self.persist_credential_value(&credential)?;

        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            entry.credentials.rpm = credential.rpm;
        }

        self.clear_rate_limit_for_credential(id);
        self.notify_dispatch_state_changed();
        self.publish_credentials_changed("credential_rpm_updated");
        Ok(())
    }

    /// 设置凭据 Region 覆盖值（Admin API）。
    ///
    /// `region` 是旧兼容字段，主要作为 Auth Region 回退；`auth_region`
    /// 控制 token 刷新；`api_region` 控制 q.{region}.amazonaws.com 请求。
    pub fn set_credential_regions(
        &self,
        id: u64,
        region: Option<Option<String>>,
        auth_region: Option<Option<String>>,
        api_region: Option<Option<String>>,
    ) -> anyhow::Result<()> {
        let credential = {
            let entries = self.entries.lock();
            let entry = entries
                .iter()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            let mut credential = Self::credential_from_entry(entry);
            let region_changed = region.is_some() || auth_region.is_some() || api_region.is_some();
            if let Some(value) = region {
                credential.region = value;
            }
            if let Some(value) = auth_region {
                credential.auth_region = value;
            }
            if let Some(value) = api_region {
                credential.api_region = value;
            }
            if region_changed && !credential.is_api_key_credential() {
                credential.access_token = None;
                credential.expires_at = None;
                credential.profile_arn = None;
                credential.subscription_title = None;
            }
            credential
        };
        self.persist_credential_value(&credential)?;

        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            entry.credentials.region = credential.region;
            entry.credentials.auth_region = credential.auth_region;
            entry.credentials.api_region = credential.api_region;
            if !entry.credentials.is_api_key_credential() {
                entry.credentials.access_token = None;
                entry.credentials.expires_at = None;
                entry.credentials.profile_arn = None;
                entry.credentials.subscription_title = None;
            }
        }

        self.publish_credentials_changed("credential_regions_updated");
        self.notify_dispatch_state_changed();
        Ok(())
    }

    pub fn update_credential_auth(
        &self,
        id: u64,
        update: CredentialAuthUpdate,
        reset_runtime_state: bool,
    ) -> anyhow::Result<()> {
        let mut credential = {
            let entries = self.entries.lock();
            let entry = entries
                .iter()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            Self::credential_from_entry(entry)
        };

        apply_credential_auth_update(&mut credential, update);
        credential.canonicalize_auth_method();
        if credential.is_api_key_credential() {
            let api_key = credential
                .kiro_api_key
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("API Key 凭据缺少 kiroApiKey"))?;
            if api_key.trim().is_empty() {
                anyhow::bail!("kiroApiKey 为空");
            }
        } else {
            validate_refresh_token(&credential)?;
        }

        {
            let entries = self.entries.lock();
            if credential.is_api_key_credential() {
                let new_hash = credential.kiro_api_key.as_deref().map(sha256_hex);
                if let Some(new_hash) = new_hash.as_deref() {
                    let duplicate = entries.iter().any(|entry| {
                        entry.id != id
                            && entry
                                .credentials
                                .kiro_api_key
                                .as_deref()
                                .map(sha256_hex)
                                .as_deref()
                                == Some(new_hash)
                    });
                    if duplicate {
                        anyhow::bail!("凭据已存在（kiroApiKey 重复）");
                    }
                }
            } else {
                let new_hash = credential.refresh_token.as_deref().map(sha256_hex);
                if let Some(new_hash) = new_hash.as_deref() {
                    let duplicate = entries.iter().any(|entry| {
                        entry.id != id
                            && entry
                                .credentials
                                .refresh_token
                                .as_deref()
                                .map(sha256_hex)
                                .as_deref()
                                == Some(new_hash)
                    });
                    if duplicate {
                        anyhow::bail!("凭据已存在（refreshToken 重复）");
                    }
                }
            }
        }

        self.persist_credential_value(&credential)?;

        let runtime_state = {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            entry.credentials = credential;
            if reset_runtime_state {
                entry.failure_count = 0;
                entry.refresh_failure_count = 0;
                entry.cooldown_until = None;
                entry.cooldown_reason = None;
                entry.model_cooldowns.clear();
                entry.rate_limit_available_at = None;
                entry.health = SchedulerHealthState::default();
                entry.model_health.clear();
                entry.selection_events.clear();
            }
            reset_runtime_state.then(|| Self::runtime_state_from_entry(entry))
        };
        if let Some(runtime_state) = runtime_state {
            self.persist_runtime_state_value(id, &runtime_state)?;
            self.clear_scheduler_state_for_credential(id, false);
        }

        self.publish_credentials_changed("credential_auth_updated");
        self.notify_dispatch_state_changed();
        Ok(())
    }

    pub fn update_credential_profile_arn(
        &self,
        id: u64,
        profile_arn: Option<String>,
    ) -> anyhow::Result<()> {
        let profile_arn = profile_arn
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let credential = {
            let entries = self.entries.lock();
            let entry = entries
                .iter()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            let mut credential = Self::credential_from_entry(entry);
            credential.profile_arn = profile_arn;
            credential
        };
        self.persist_credential_value(&credential)?;
        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            entry.credentials.profile_arn = credential.profile_arn.clone();
        }
        self.publish_credentials_changed("credential_profile_arn_updated");
        Ok(())
    }

    /// 重置凭据失败计数并重新启用（Admin API）
    pub fn reset_and_enable(&self, id: u64) -> anyhow::Result<()> {
        let (credential, runtime_state) = {
            let entries = self.entries.lock();
            let entry = entries
                .iter()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            if entry.disabled_reason == Some(DisabledReason::InvalidConfig) {
                anyhow::bail!("凭据 #{} 因配置无效被禁用，请修正配置后重启服务", id);
            }
            let mut credential = Self::credential_from_entry(entry);
            credential.disabled = false;
            let mut runtime_state = Self::runtime_state_from_entry(entry);
            runtime_state.failure_count = 0;
            runtime_state.refresh_failure_count = 0;
            runtime_state.disabled_reason = None;
            (credential, runtime_state)
        };
        self.persist_credential_value(&credential)?;
        self.persist_runtime_state_value(id, &runtime_state)?;

        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            if entry.disabled_reason == Some(DisabledReason::InvalidConfig) {
                anyhow::bail!("凭据 #{} 因配置无效被禁用，请修正配置后重启服务", id);
            }
            entry.failure_count = 0;
            entry.refresh_failure_count = 0;
            entry.disabled = false;
            entry.disabled_reason = None;
            entry.cooldown_until = None;
            entry.cooldown_reason = None;
            entry.model_cooldowns.clear();
            entry.rate_limit_available_at = None;
        }
        self.select_highest_priority();
        self.clear_scheduler_state_for_credential(id, false);
        self.notify_dispatch_state_changed();
        self.publish_credentials_changed("credential_reset_and_enabled");
        Ok(())
    }

    /// 获取指定凭据的使用额度（Admin API）
    pub async fn get_usage_limits_for(&self, id: u64) -> anyhow::Result<UsageLimitsResponse> {
        let ctx = self.acquire_context_for_credential(id).await?;
        let token = ctx.token;
        let credentials = ctx.credentials;

        let effective_proxy = credentials.effective_proxy(self.proxy.as_ref());
        let config = self.runtime_config();
        let usage_limits =
            get_usage_limits(&credentials, &config, &token, effective_proxy.as_ref()).await?;

        // 更新订阅等级到凭据（仅在发生变化时持久化）
        if let Some(subscription_title) = usage_limits.subscription_title() {
            let changed = {
                let mut entries = self.entries.lock();
                if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                    let old_title = entry.credentials.subscription_title.clone();
                    if old_title.as_deref() != Some(subscription_title) {
                        entry.credentials.subscription_title = Some(subscription_title.to_string());
                        tracing::info!(
                            "凭据 #{} 订阅等级已更新: {:?} -> {}",
                            id,
                            old_title,
                            subscription_title
                        );
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            };

            if changed {
                if let Err(e) = self.persist_credential_entry(id) {
                    tracing::warn!("订阅等级更新后持久化失败（不影响本次请求）: {}", e);
                } else {
                    self.publish_credentials_changed("subscription_title_updated");
                }
            }
        }

        Ok(usage_limits)
    }

    /// 使用一份外部凭据临时查询账号信息，不加入凭据池、不改变调度状态。
    ///
    /// 这个方法用于 Admin 的外部 JSON 订阅校验。它允许使用凭据绑定的代理资源，
    /// 但不会保存 token、不会启用/禁用任何系统凭据，也不会占用调度并发槽。
    pub async fn acquire_context_for_external_credentials(
        &self,
        mut credentials: KiroCredentials,
    ) -> anyhow::Result<CallContext> {
        credentials.canonicalize_auth_method();

        if credentials.is_api_key_credential() {
            let credentials = self.resolve_proxy_for_credential(credentials)?;
            let token = credentials
                .kiro_api_key
                .clone()
                .ok_or_else(|| anyhow::anyhow!("API Key 凭据缺少 kiroApiKey"))?;
            return Ok(CallContext {
                id: EXTERNAL_CREDENTIAL_CONTEXT_ID,
                credentials,
                token,
                sticky_bound: false,
                fallback_from_sticky: false,
                in_flight_lease: None,
            });
        }

        let source_credentials = credentials.clone();
        let credentials = self.resolve_proxy_for_credential(credentials)?;
        let effective_proxy = credentials.effective_proxy(self.proxy.as_ref());
        let config = self.runtime_config();
        let refreshed = refresh_token(&credentials, &config, effective_proxy.as_ref()).await?;
        let refreshed = Self::preserve_proxy_fields(refreshed, &source_credentials);
        self.token_context_from_credentials(EXTERNAL_CREDENTIAL_CONTEXT_ID, refreshed, false)
    }

    /// 使用一份外部凭据临时查询账号信息，不加入凭据池、不改变调度状态。
    ///
    /// 这个方法用于 Admin 的外部 JSON 订阅校验。它允许使用凭据绑定的代理资源，
    /// 但不会保存 token、不会启用/禁用任何系统凭据，也不会占用调度并发槽。
    pub async fn probe_usage_limits_for_credentials(
        &self,
        credentials: KiroCredentials,
    ) -> anyhow::Result<UsageLimitsResponse> {
        let ctx = self
            .acquire_context_for_external_credentials(credentials)
            .await?;
        let effective_proxy = ctx.credentials.effective_proxy(self.proxy.as_ref());
        let config = self.runtime_config();
        get_usage_limits(
            &ctx.credentials,
            &config,
            &ctx.token,
            effective_proxy.as_ref(),
        )
        .await
    }

    /// 添加新凭据（Admin API）
    ///
    /// # 流程
    /// 1. 验证凭据基本字段（API Key: kiroApiKey 不为空; OAuth: refreshToken 不为空）
    /// 2. 基于 kiroApiKey 或 refreshToken 的 SHA-256 哈希检测重复
    /// 3. OAuth: 尝试刷新 Token 验证凭据有效性; API Key: 跳过
    /// 4. 分配新 ID（PgSQL 模式由数据库序列生成；测试无 PgSQL 时回退内存 max + 1）
    /// 5. 添加到 entries 列表
    /// 6. 行级持久化到 PgSQL
    ///
    /// # 返回
    /// - `Ok(u64)` - 新凭据 ID
    /// - `Err(_)` - 验证失败或添加失败
    pub async fn add_credential(&self, new_cred: KiroCredentials) -> anyhow::Result<u64> {
        // 1. 基本验证
        if let Some(resource_id) = new_cred.proxy_resource_id {
            let exists = self.proxy_resources.lock().contains_key(&resource_id);
            if !exists {
                anyhow::bail!("代理资源不存在: {}", resource_id);
            }
        }

        if new_cred.is_api_key_credential() {
            let api_key = new_cred
                .kiro_api_key
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("API Key 凭据缺少 kiroApiKey"))?;
            if api_key.is_empty() {
                anyhow::bail!("kiroApiKey 为空");
            }
        } else {
            validate_refresh_token(&new_cred)?;
        }

        // 2. 基于哈希检测重复
        if new_cred.is_api_key_credential() {
            let new_api_key = new_cred
                .kiro_api_key
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("缺少 kiroApiKey"))?;
            let new_api_key_hash = sha256_hex(new_api_key);
            let duplicate_exists = {
                let entries = self.entries.lock();
                entries.iter().any(|entry| {
                    entry
                        .credentials
                        .kiro_api_key
                        .as_deref()
                        .map(sha256_hex)
                        .as_deref()
                        == Some(new_api_key_hash.as_str())
                })
            };
            if duplicate_exists {
                anyhow::bail!("凭据已存在（kiroApiKey 重复）");
            }
        } else {
            let new_refresh_token = new_cred
                .refresh_token
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("缺少 refreshToken"))?;
            let new_refresh_token_hash = sha256_hex(new_refresh_token);
            let duplicate_exists = {
                let entries = self.entries.lock();
                entries.iter().any(|entry| {
                    entry
                        .credentials
                        .refresh_token
                        .as_deref()
                        .map(sha256_hex)
                        .as_deref()
                        == Some(new_refresh_token_hash.as_str())
                })
            };
            if duplicate_exists {
                anyhow::bail!("凭据已存在（refreshToken 重复）");
            }
        }

        // 3. 验证凭据有效性（API Key 无需网络刷新）
        let mut validated_cred = if new_cred.is_api_key_credential() {
            new_cred.clone()
        } else {
            let new_cred_for_proxy = self.resolve_proxy_for_credential(new_cred.clone())?;
            let effective_proxy = new_cred_for_proxy.effective_proxy(self.proxy.as_ref());
            let config = self.runtime_config();
            refresh_token(&new_cred_for_proxy, &config, effective_proxy.as_ref()).await?
        };

        // 4. 保留用户输入的元数据
        validated_cred.priority = new_cred.priority;
        validated_cred.auth_method = new_cred.auth_method.map(|m| {
            if m.eq_ignore_ascii_case("builder-id") || m.eq_ignore_ascii_case("iam") {
                "idc".to_string()
            } else {
                m
            }
        });
        validated_cred.client_id = new_cred.client_id;
        validated_cred.client_secret = new_cred.client_secret;
        validated_cred.region = new_cred.region;
        validated_cred.auth_region = new_cred.auth_region;
        validated_cred.api_region = new_cred.api_region;
        validated_cred.machine_id = new_cred.machine_id;
        validated_cred.email = new_cred.email;
        validated_cred.proxy_url = new_cred.proxy_url;
        validated_cred.proxy_username = new_cred.proxy_username;
        validated_cred.proxy_password = new_cred.proxy_password;
        validated_cred.proxy_resource_id = new_cred.proxy_resource_id;
        validated_cred.kiro_api_key = new_cred.kiro_api_key;
        validated_cred.endpoint = new_cred.endpoint;
        if validated_cred.machine_id.is_none() {
            validated_cred.machine_id = Some(machine_id::generate_from_credentials(
                &validated_cred,
                &self.runtime_config(),
            ));
        }

        let initial_disabled = new_cred.disabled;
        let warmup_remaining = self.runtime_config().credential_warmup_requests;
        let new_id = if let Some(store) = &self.postgres_store {
            validated_cred.disabled = initial_disabled;
            let inserted = store.insert_credential(&validated_cred).await?;
            let id = inserted
                .id
                .ok_or_else(|| anyhow::anyhow!("PgSQL 新增凭据未返回 id"))?;
            validated_cred = inserted;
            id
        } else {
            let id = {
                let entries = self.entries.lock();
                entries.iter().map(|e| e.id).max().unwrap_or(0) + 1
            };
            validated_cred.id = Some(id);
            id
        };

        {
            let mut entries = self.entries.lock();
            entries.push(CredentialEntry {
                id: new_id,
                credentials: validated_cred,
                failure_count: 0,
                refresh_failure_count: 0,
                disabled: initial_disabled,
                disabled_reason: initial_disabled.then_some(DisabledReason::Manual),
                success_count: 0,
                total_selection_count: 0,
                last_used_at: None,
                cooldown_until: None,
                cooldown_reason: None,
                model_cooldowns: HashMap::new(),
                rate_limit_available_at: None,
                in_flight_requests: 0,
                in_flight_leases: Vec::new(),
                warmup_remaining,
                health: SchedulerHealthState::default(),
                model_health: HashMap::new(),
                selection_events: VecDeque::new(),
            });
        }

        // 6. 无 PgSQL 的测试模式需要在这里补持久化；PgSQL 模式上面已先写库再更新内存。
        if self.postgres_store.is_none() {
            self.persist_credentials()?;
        }
        self.save_runtime_state_for(new_id);
        self.publish_credentials_changed("credential_added");
        self.notify_dispatch_state_changed();

        tracing::info!("成功添加凭据 #{}", new_id);
        Ok(new_id)
    }

    /// 设置凭据预热剩余请求数。0 表示关闭预热。
    pub fn set_warmup_remaining(&self, id: u64, warmup_remaining: u32) -> anyhow::Result<()> {
        let runtime_state = {
            let entries = self.entries.lock();
            let entry = entries
                .iter()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            let mut runtime_state = Self::runtime_state_from_entry(entry);
            runtime_state.warmup_remaining = warmup_remaining;
            runtime_state
        };
        self.persist_runtime_state_value(id, &runtime_state)?;

        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            entry.warmup_remaining = warmup_remaining;
        }
        self.publish_credentials_changed("credential_warmup_updated");
        self.notify_dispatch_state_changed();
        Ok(())
    }

    pub fn set_credential_proxy(
        &self,
        id: u64,
        proxy_resource_id: Option<u64>,
        proxy_url: Option<String>,
        proxy_username: Option<String>,
        proxy_password: Option<String>,
    ) -> anyhow::Result<()> {
        if let Some(resource_id) = proxy_resource_id {
            let exists = self.proxy_resources.lock().contains_key(&resource_id);
            if !exists {
                anyhow::bail!("代理资源不存在: {}", resource_id);
            }
        }

        let credential = {
            let entries = self.entries.lock();
            let entry = entries
                .iter()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            let mut credential = Self::credential_from_entry(entry);
            credential.proxy_resource_id = proxy_resource_id;
            credential.proxy_url = proxy_url;
            credential.proxy_username = proxy_username;
            credential.proxy_password = proxy_password;
            credential
        };
        self.persist_credential_value(&credential)?;

        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            entry.credentials.proxy_resource_id = credential.proxy_resource_id;
            entry.credentials.proxy_url = credential.proxy_url;
            entry.credentials.proxy_username = credential.proxy_username;
            entry.credentials.proxy_password = credential.proxy_password;
        }
        self.publish_credentials_changed("credential_proxy_updated");
        self.notify_dispatch_state_changed();
        Ok(())
    }

    /// 删除凭据（Admin API）
    ///
    /// # 前置条件
    /// - 凭据必须已禁用（disabled = true）
    ///
    /// # 行为
    /// 1. 验证凭据存在
    /// 2. 验证凭据已禁用
    /// 3. 从 entries 移除
    /// 4. 如果删除的是当前凭据，切换到优先级最高的可用凭据
    /// 5. 如果删除后没有凭据，将 current_id 重置为 0
    /// 6. 软删除 PgSQL 中的凭据行并清理运行态
    ///
    /// # 返回
    /// - `Ok(())` - 删除成功
    /// - `Err(_)` - 凭据不存在、未禁用或持久化失败
    pub fn delete_credential(&self, id: u64) -> anyhow::Result<()> {
        {
            let entries = self.entries.lock();
            let entry = entries
                .iter()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;

            if !entry.disabled {
                anyhow::bail!("只能删除已禁用的凭据（请先禁用凭据 #{}）", id);
            }
        }

        // 行级软删除，并清理该凭据的统计/运行态残留。成功后再更新本进程快照。
        self.delete_persisted_credential_state(id)?;

        let was_current = {
            let mut entries = self.entries.lock();
            let current_id = *self.current_id.lock();
            let was_current = current_id == id;
            entries.retain(|e| e.id != id);
            was_current
        };

        // 如果删除的是当前凭据，切换到优先级最高的可用凭据
        if was_current {
            self.select_highest_priority();
        }
        self.unbind_sessions_for_credential(id);
        self.clear_scheduler_state_for_credential(id, true);
        self.remove_refresh_lock_for_credential(id);
        self.notify_dispatch_state_changed();

        // 如果删除后没有任何凭据，将 current_id 重置为 0（与初始化行为保持一致）
        {
            let entries = self.entries.lock();
            if entries.is_empty() {
                let mut current_id = self.current_id.lock();
                *current_id = 0;
                tracing::info!("所有凭据已删除，current_id 已重置为 0");
            }
        }

        self.publish_credentials_changed("credential_deleted");

        tracing::info!("已删除凭据 #{}", id);
        Ok(())
    }

    /// 强制刷新指定凭据的 Token（Admin API）
    ///
    /// 无条件调用上游 API 重新获取 access token，不检查是否过期。
    /// 适用于排查问题、Token 异常但未过期、主动更新凭据状态等场景。
    pub async fn force_refresh_token_for(&self, id: u64) -> anyhow::Result<()> {
        let credentials = {
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.credentials.clone())
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?
        };

        // 获取该凭据的刷新锁，防止同一账号并发刷新。
        let refresh_lock = self.refresh_lock_for_credential(id);
        let _guard = refresh_lock.lock().await;
        let mut redis_refresh_lock: Option<(Arc<RedisStore>, String)> = None;
        let mut redis_lock_failed_open = false;
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            for _ in 0..10 {
                match redis.acquire_refresh_lock(id, 120).await {
                    Ok(Some(lock_token)) => {
                        redis_refresh_lock = Some((redis.clone(), lock_token));
                        break;
                    }
                    Ok(None) => tokio::time::sleep(StdDuration::from_millis(500)).await,
                    Err(err) => {
                        tracing::warn!(
                            credential_id = id,
                            "强制刷新时获取 Redis Token 刷新锁失败，使用本进程刷新锁继续: {}",
                            err
                        );
                        redis_lock_failed_open = true;
                        break;
                    }
                }
            }
            if !redis_lock_failed_open && redis_refresh_lock.is_none() {
                anyhow::bail!("其他实例正在刷新凭据 #{}，请稍后再试", id);
            }
        }

        // 无条件调用 refresh_token
        let refresh_result = match self.resolve_proxy_for_credential(credentials.clone()) {
            Ok(credentials_for_proxy) => {
                let effective_proxy = credentials_for_proxy.effective_proxy(self.proxy.as_ref());
                let config = self.runtime_config();
                refresh_token(&credentials_for_proxy, &config, effective_proxy.as_ref()).await
            }
            Err(err) => Err(err),
        };
        if let Some((redis, lock_token)) = redis_refresh_lock {
            if let Err(err) = redis.release_refresh_lock(id, &lock_token).await {
                tracing::warn!(credential_id = id, "释放 Redis Token 刷新锁失败: {}", err);
            }
        }
        let mut new_creds = refresh_result?;
        new_creds.proxy_url = credentials.proxy_url;
        new_creds.proxy_username = credentials.proxy_username;
        new_creds.proxy_password = credentials.proxy_password;
        new_creds.proxy_resource_id = credentials.proxy_resource_id;

        // 更新 entries 中对应凭据
        {
            let mut entries = self.entries.lock();
            if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                entry.credentials = new_creds;
                entry.refresh_failure_count = 0;
            }
        }

        // 持久化当前凭据行
        if let Err(e) = self.persist_credential_entry(id) {
            tracing::warn!("强制刷新 Token 后持久化失败: {}", e);
        } else {
            self.publish_credentials_changed("credential_force_refreshed");
        }
        self.save_runtime_state_for(id);

        tracing::info!("凭据 #{} Token 已强制刷新", id);
        Ok(())
    }

    /// 获取负载均衡模式（Admin API）
    pub fn get_load_balancing_mode(&self) -> String {
        self.load_balancing_mode.lock().clone()
    }

    fn persist_load_balancing_mode(&self, mode: &str) -> anyhow::Result<()> {
        let mut config = self.runtime_config();
        config.load_balancing_mode = mode.to_string();
        config.set_config_path_for_runtime(None);
        let mut saved_version: Option<i64> = None;
        if let Some(store) = &self.postgres_store {
            let store = store.clone();
            saved_version = Some(block_on_storage(
                "持久化负载均衡模式到 PgSQL",
                async move { store.save_runtime_config_returning_version(&config).await },
            )?);
        }
        self.publish_runtime_config_changed(saved_version, "load_balancing_mode_updated");
        Ok(())
    }

    /// 设置负载均衡模式（Admin API）
    pub fn set_load_balancing_mode(&self, mode: String) -> anyhow::Result<()> {
        // 验证模式值
        if mode != "priority" && mode != "balanced" && mode != "health_balanced" {
            anyhow::bail!("无效的负载均衡模式: {}", mode);
        }

        let previous_mode = self.get_load_balancing_mode();
        if previous_mode == mode {
            return Ok(());
        }

        *self.load_balancing_mode.lock() = mode.clone();

        if let Err(err) = self.persist_load_balancing_mode(&mode) {
            *self.load_balancing_mode.lock() = previous_mode;
            return Err(err);
        }
        self.update_load_balancing_mode_in_config(&mode);
        self.notify_dispatch_state_changed();

        tracing::info!("负载均衡模式已设置为: {}", mode);
        Ok(())
    }
}

impl Drop for MultiTokenManager {
    fn drop(&mut self) {
        if self.stats_dirty.load(Ordering::Relaxed) {
            self.save_stats();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const SONNET_MODEL: &str = "claude-sonnet-4.5";

    async fn test_redis_store() -> Option<Arc<RedisStore>> {
        let url = std::env::var("KIRO_RS_TEST_REDIS_URL").ok()?;
        let mut config = Config::default();
        config.redis.url = Some(url);
        config.redis.key_prefix = format!("kiro_rs:test:{}", uuid::Uuid::new_v4());
        Some(Arc::new(RedisStore::connect(&config).await.unwrap()))
    }

    async fn test_postgres_store() -> Option<Arc<PostgresStore>> {
        let url = std::env::var("KIRO_RS_TEST_POSTGRES_URL").ok()?;
        let mut config = Config::default();
        config.postgres.url = Some(url);
        config.postgres.max_connections = 2;
        Some(Arc::new(
            PostgresStore::connect_test(&config).await.unwrap(),
        ))
    }

    fn api_key_credential(token: &str) -> KiroCredentials {
        KiroCredentials {
            kiro_api_key: Some(token.to_string()),
            auth_method: Some("api_key".to_string()),
            ..Default::default()
        }
    }

    fn test_access_token_credential(token: &str, subscription_title: &str) -> KiroCredentials {
        let mut credential = KiroCredentials::default();
        credential.subscription_title = Some(subscription_title.to_string());
        credential.access_token = Some(token.to_string());
        credential.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        credential
    }

    #[test]
    fn test_is_token_expired_with_expired_token() {
        let mut credentials = KiroCredentials::default();
        credentials.expires_at = Some("2020-01-01T00:00:00Z".to_string());
        assert!(is_token_expired(&credentials));
    }

    #[test]
    fn test_is_token_expired_with_valid_token() {
        let mut credentials = KiroCredentials::default();
        let future = Utc::now() + Duration::hours(1);
        credentials.expires_at = Some(future.to_rfc3339());
        assert!(!is_token_expired(&credentials));
    }

    #[test]
    fn test_is_token_expired_within_5_minutes() {
        let mut credentials = KiroCredentials::default();
        let expires = Utc::now() + Duration::minutes(3);
        credentials.expires_at = Some(expires.to_rfc3339());
        assert!(is_token_expired(&credentials));
    }

    #[test]
    fn test_is_token_expired_no_expires_at() {
        let credentials = KiroCredentials::default();
        assert!(is_token_expired(&credentials));
    }

    #[test]
    fn test_is_token_expiring_soon_within_10_minutes() {
        let mut credentials = KiroCredentials::default();
        let expires = Utc::now() + Duration::minutes(8);
        credentials.expires_at = Some(expires.to_rfc3339());
        assert!(is_token_expiring_soon(&credentials));
    }

    #[test]
    fn test_is_token_expiring_soon_beyond_10_minutes() {
        let mut credentials = KiroCredentials::default();
        let expires = Utc::now() + Duration::minutes(15);
        credentials.expires_at = Some(expires.to_rfc3339());
        assert!(!is_token_expiring_soon(&credentials));
    }

    #[test]
    fn test_validate_refresh_token_missing() {
        let credentials = KiroCredentials::default();
        let result = validate_refresh_token(&credentials);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_refresh_token_valid() {
        let mut credentials = KiroCredentials::default();
        credentials.refresh_token = Some("a".repeat(150));
        let result = validate_refresh_token(&credentials);
        assert!(result.is_ok());
    }

    #[test]
    fn test_invalid_grant_resource_not_found_is_permanent_refresh_failure() {
        assert!(is_invalid_grant_response(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":"invalid_grant","error_description":"Resource not found"}"#
        ));
        assert!(!is_invalid_grant_response(
            reqwest::StatusCode::UNAUTHORIZED,
            r#"{"error":"invalid_grant","error_description":"Resource not found"}"#
        ));
        assert!(!is_invalid_grant_response(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":"slow_down"}"#
        ));
    }

    #[test]
    fn test_sha256_hex() {
        let result = sha256_hex("test");
        assert_eq!(
            result,
            "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
        );
    }

    #[test]
    fn usage_limits_user_agents_match_kiro_rest_shape() {
        assert_eq!(
            usage_limits_amz_user_agent("0.12.155", "machine"),
            "aws-sdk-js/1.0.0 KiroIDE 0.12.155 machine"
        );
        assert_eq!(
            usage_limits_user_agent("macos#23.4.0", "22.22.0", "0.12.155", "machine"),
            "aws-sdk-js/1.0.0 ua/2.1 os/macos#23.4.0 lang/js md/nodejs#22.22.0 api/codewhispererruntime#1.0.0 m/N,E KiroIDE-0.12.155-machine"
        );
    }

    #[tokio::test]
    async fn test_refresh_token_rejects_api_key_credential() {
        let config = Config::default();
        let mut credentials = KiroCredentials::default();
        credentials.kiro_api_key = Some("ksk_test_key_123".to_string());
        credentials.auth_method = Some("api_key".to_string());

        let result = refresh_token(&credentials, &config, None).await;

        assert!(result.is_err(), "API Key 凭据应被 refresh_token 拒绝");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("API Key 凭据不支持刷新"),
            "期望错误消息包含 'API Key 凭据不支持刷新'，实际: {}",
            err_msg
        );
    }

    #[tokio::test]
    async fn test_add_credential_reject_duplicate_refresh_token() {
        let config = Config::default();

        let mut existing = KiroCredentials::default();
        existing.refresh_token = Some("a".repeat(150));

        let manager = MultiTokenManager::new(config, vec![existing], None, None, false).unwrap();

        let mut duplicate = KiroCredentials::default();
        duplicate.refresh_token = Some("a".repeat(150));

        let result = manager.add_credential(duplicate).await;
        assert!(result.is_err());
        assert!(result.err().unwrap().to_string().contains("凭据已存在"));
    }

    #[tokio::test]
    async fn test_add_credential_api_key_success() {
        let config = Config::default();
        let manager = MultiTokenManager::new(config, vec![], None, None, false).unwrap();

        let mut api_key_cred = KiroCredentials::default();
        api_key_cred.kiro_api_key = Some("ksk_test_key_123".to_string());
        api_key_cred.auth_method = Some("api_key".to_string());

        let result = manager.add_credential(api_key_cred).await;
        assert!(result.is_ok());
        let id = result.unwrap();
        assert!(id > 0);
        assert_eq!(manager.total_count(), 1);
        assert_eq!(manager.available_count(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn postgres_row_level_update_does_not_delete_credentials_added_by_other_instance() {
        let Some(store) = test_postgres_store().await else {
            eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let mut first = api_key_credential("ksk_first_row_level");
        first.id = Some(1);
        first.priority = 1;
        store.save_credentials(&[first.clone()]).await.unwrap();

        let manager = MultiTokenManager::new_with_stores(
            Config::default(),
            vec![first],
            None,
            None,
            false,
            Some(store.clone()),
            None,
        )
        .unwrap();

        let second = store
            .insert_credential(&KiroCredentials {
                kiro_api_key: Some("ksk_second_other_instance".to_string()),
                auth_method: Some("api_key".to_string()),
                priority: 2,
                ..Default::default()
            })
            .await
            .unwrap();

        manager.set_priority(1, 5).unwrap();
        let loaded = store.load_credentials().await.unwrap();
        assert!(
            loaded
                .iter()
                .any(|credential| credential.id == Some(1) && credential.priority == 5),
            "当前实例更新的凭据应被行级保存"
        );
        assert!(
            loaded.iter().any(|credential| credential.id == second.id),
            "其他实例新增的凭据不应被旧内存快照软删除"
        );

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn postgres_failure_counts_are_atomic_across_managers() {
        let Some(store) = test_postgres_store().await else {
            eprintln!("跳过 PgSQL TokenManager 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };

        let mut credential = api_key_credential("ksk_atomic_failure_count");
        credential.id = Some(1);
        store.save_credentials(&[credential.clone()]).await.unwrap();

        let manager_a = MultiTokenManager::new_with_stores(
            Config::default(),
            vec![credential.clone()],
            None,
            None,
            false,
            Some(store.clone()),
            None,
        )
        .unwrap();
        let manager_b = MultiTokenManager::new_with_stores(
            Config::default(),
            vec![credential],
            None,
            None,
            false,
            Some(store.clone()),
            None,
        )
        .unwrap();

        assert!(manager_a.report_failure(1));
        assert!(manager_b.report_failure(1));
        assert!(!manager_a.report_failure(1));

        let runtime_state = store.load_credential_runtime_state().await.unwrap();
        let state = runtime_state.get(&1).unwrap();
        assert_eq!(state.failure_count, MAX_FAILURES_PER_CREDENTIAL);
        assert_eq!(
            state.disabled_reason.as_deref(),
            Some(DisabledReason::TooManyFailures.as_str())
        );
        let credentials = store.load_credentials().await.unwrap();
        assert!(
            credentials
                .iter()
                .any(|credential| { credential.id == Some(1) && credential.disabled })
        );
        let snapshot = manager_a.snapshot();
        assert!(snapshot.entries.iter().any(|entry| {
            entry.id == 1 && entry.disabled && entry.failure_count == MAX_FAILURES_PER_CREDENTIAL
        }));

        store.drop_test_schema().await.unwrap();
    }

    #[tokio::test]
    async fn test_add_credential_reject_duplicate_api_key() {
        let config = Config::default();

        let mut existing = KiroCredentials::default();
        existing.kiro_api_key = Some("ksk_existing_key".to_string());
        existing.auth_method = Some("api_key".to_string());

        let manager = MultiTokenManager::new(config, vec![existing], None, None, false).unwrap();

        let mut duplicate = KiroCredentials::default();
        duplicate.kiro_api_key = Some("ksk_existing_key".to_string());
        duplicate.auth_method = Some("api_key".to_string());

        let result = manager.add_credential(duplicate).await;
        assert!(result.is_err());
        assert!(
            result
                .err()
                .unwrap()
                .to_string()
                .contains("kiroApiKey 重复")
        );
    }

    #[tokio::test]
    async fn test_add_credential_api_key_empty_rejected() {
        let config = Config::default();
        let manager = MultiTokenManager::new(config, vec![], None, None, false).unwrap();

        let mut cred = KiroCredentials::default();
        cred.kiro_api_key = Some(String::new());
        cred.auth_method = Some("api_key".to_string());

        let result = manager.add_credential(cred).await;
        assert!(result.is_err());
        assert!(
            result
                .err()
                .unwrap()
                .to_string()
                .contains("kiroApiKey 为空")
        );
    }

    #[tokio::test]
    async fn test_add_credential_api_key_missing_key_rejected() {
        let config = Config::default();
        let manager = MultiTokenManager::new(config, vec![], None, None, false).unwrap();

        let mut cred = KiroCredentials::default();
        cred.auth_method = Some("api_key".to_string());
        // kiro_api_key is None

        let result = manager.add_credential(cred).await;
        assert!(result.is_err());
        assert!(
            result
                .err()
                .unwrap()
                .to_string()
                .contains("缺少 kiroApiKey")
        );
    }

    #[tokio::test]
    async fn test_add_credential_api_key_and_oauth_coexist() {
        let config = Config::default();

        let mut oauth_cred = KiroCredentials::default();
        oauth_cred.refresh_token = Some("a".repeat(150));

        let manager = MultiTokenManager::new(config, vec![oauth_cred], None, None, false).unwrap();

        let mut api_key_cred = KiroCredentials::default();
        api_key_cred.kiro_api_key = Some("ksk_new_key".to_string());
        api_key_cred.auth_method = Some("api_key".to_string());

        let result = manager.add_credential(api_key_cred).await;
        assert!(result.is_ok());
        assert_eq!(manager.total_count(), 2);
        assert_eq!(manager.available_count(), 2);
    }

    // MultiTokenManager 测试

    #[test]
    fn test_multi_token_manager_new() {
        let config = Config::default();
        let mut cred1 = KiroCredentials::default();
        cred1.priority = 0;
        let mut cred2 = KiroCredentials::default();
        cred2.priority = 1;

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();
        assert_eq!(manager.total_count(), 2);
        assert_eq!(manager.available_count(), 2);
    }

    #[test]
    fn test_multi_token_manager_empty_credentials() {
        let config = Config::default();
        let result = MultiTokenManager::new(config, vec![], None, None, false);
        // 支持 0 个凭据启动（可通过管理面板添加）
        assert!(result.is_ok());
        let manager = result.unwrap();
        assert_eq!(manager.total_count(), 0);
        assert_eq!(manager.available_count(), 0);
    }

    #[test]
    fn test_multi_token_manager_duplicate_ids() {
        let config = Config::default();
        let mut cred1 = KiroCredentials::default();
        cred1.id = Some(1);
        let mut cred2 = KiroCredentials::default();
        cred2.id = Some(1); // 重复 ID

        let result = MultiTokenManager::new(config, vec![cred1, cred2], None, None, false);
        assert!(result.is_err());
        let err_msg = result.err().unwrap().to_string();
        assert!(
            err_msg.contains("重复的凭据 ID"),
            "错误消息应包含 '重复的凭据 ID'，实际: {}",
            err_msg
        );
    }

    #[test]
    fn test_multi_token_manager_api_key_missing_kiro_api_key_auto_disabled() {
        let config = Config::default();

        // auth_method=api_key 但缺少 kiro_api_key → 应被自动禁用
        let mut bad_cred = KiroCredentials::default();
        bad_cred.auth_method = Some("api_key".to_string());
        // kiro_api_key 保持 None

        let mut good_cred = KiroCredentials::default();
        good_cred.refresh_token = Some("valid_token".to_string());

        let manager =
            MultiTokenManager::new(config, vec![bad_cred, good_cred], None, None, false).unwrap();
        assert_eq!(manager.total_count(), 2);
        assert_eq!(manager.available_count(), 1); // bad_cred 被禁用，只剩 1 个可用
    }

    #[test]
    fn test_multi_token_manager_api_key_with_kiro_api_key_not_disabled() {
        let config = Config::default();

        // auth_method=api_key 且有 kiro_api_key → 不应被禁用
        let mut cred = KiroCredentials::default();
        cred.auth_method = Some("api_key".to_string());
        cred.kiro_api_key = Some("ksk_test123".to_string());

        let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();
        assert_eq!(manager.total_count(), 1);
        assert_eq!(manager.available_count(), 1);
    }

    #[test]
    fn test_multi_token_manager_report_failure() {
        let config = Config::default();
        let cred1 = KiroCredentials::default();
        let cred2 = KiroCredentials::default();

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        // 凭据会自动分配 ID（从 1 开始）
        // 前两次失败不会禁用（使用 ID 1）
        assert!(manager.report_failure(1));
        assert!(manager.report_failure(1));
        assert_eq!(manager.available_count(), 2);

        // 第三次失败会禁用第一个凭据
        assert!(manager.report_failure(1));
        assert_eq!(manager.available_count(), 1);

        // 继续失败第二个凭据（使用 ID 2）
        assert!(manager.report_failure(2));
        assert!(manager.report_failure(2));
        assert!(!manager.report_failure(2)); // 所有凭据都禁用了
        assert_eq!(manager.available_count(), 0);
    }

    #[test]
    fn test_multi_token_manager_report_success() {
        let config = Config::default();
        let cred = KiroCredentials::default();

        let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();

        // 失败两次（使用 ID 1）
        manager.report_failure(1);
        manager.report_failure(1);

        // 成功后重置计数（使用 ID 1）
        manager.report_success(1);

        // 再失败两次不会禁用
        manager.report_failure(1);
        manager.report_failure(1);
        assert_eq!(manager.available_count(), 1);
    }

    #[test]
    fn test_multi_token_manager_switch_to_next() {
        let config = Config::default();
        let mut cred1 = KiroCredentials::default();
        cred1.refresh_token = Some("token1".to_string());
        let mut cred2 = KiroCredentials::default();
        cred2.refresh_token = Some("token2".to_string());

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        let initial_id = manager.snapshot().current_id;

        // 切换到下一个
        assert!(manager.switch_to_next());
        assert_ne!(manager.snapshot().current_id, initial_id);
    }

    #[test]
    fn test_set_load_balancing_mode_updates_runtime_memory_without_store() {
        let config = Config::default();
        let manager =
            MultiTokenManager::new(config, vec![KiroCredentials::default()], None, None, false)
                .unwrap();

        manager
            .set_load_balancing_mode("balanced".to_string())
            .unwrap();

        assert_eq!(manager.get_load_balancing_mode(), "balanced");
        assert_eq!(manager.runtime_config().load_balancing_mode, "balanced");
    }

    #[test]
    fn test_update_runtime_config_updates_runtime_memory_without_store() {
        use crate::model::config::{
            ReportedUsageConfig, ReportedUsageFieldPolicy, ReportedUsagePathPolicy,
        };

        let config = Config::default();
        let manager =
            MultiTokenManager::new(config, vec![KiroCredentials::default()], None, None, false)
                .unwrap();

        manager
            .update_runtime_config(|config| {
                config.credential_dispatch_max_wait_secs = 77;
                config.reported_usage = ReportedUsageConfig {
                    default: ReportedUsagePathPolicy::default(),
                    path_overrides: [(
                        "/custom".to_string(),
                        ReportedUsagePathPolicy {
                            input: ReportedUsageFieldPolicy::sample_input_max(42),
                            ..ReportedUsagePathPolicy::default()
                        },
                    )]
                    .into_iter()
                    .collect(),
                };
            })
            .unwrap();

        assert_eq!(
            manager
                .runtime_config()
                .reported_usage
                .policy_for_path("/custom/v1/messages")
                .input
                .max_tokens,
            42
        );
    }

    #[tokio::test]
    async fn test_multi_token_manager_acquire_context_auto_recovers_all_disabled() {
        let config = Config::default();
        let mut cred1 = KiroCredentials::default();
        cred1.access_token = Some("t1".to_string());
        cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let mut cred2 = KiroCredentials::default();
        cred2.access_token = Some("t2".to_string());
        cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        // 凭据会自动分配 ID（从 1 开始）
        for _ in 0..MAX_FAILURES_PER_CREDENTIAL {
            manager.report_failure(1);
        }
        for _ in 0..MAX_FAILURES_PER_CREDENTIAL {
            manager.report_failure(2);
        }

        assert_eq!(manager.available_count(), 0);

        // 应触发自愈：重置失败计数并重新启用，避免必须重启进程
        let mut ctx = manager.acquire_context(None).await.unwrap();
        assert!(ctx.token == "t1" || ctx.token == "t2");
        assert_eq!(manager.available_count(), 2);
        ctx.release_in_flight();
    }

    #[tokio::test]
    async fn test_multi_token_manager_acquire_context_balanced_retries_until_bad_credential_disabled()
     {
        let mut config = Config::default();
        config.load_balancing_mode = "balanced".to_string();

        let mut bad_cred = KiroCredentials::default();
        bad_cred.priority = 0;
        bad_cred.refresh_token = Some("bad".to_string());

        let mut good_cred = KiroCredentials::default();
        good_cred.priority = 1;
        good_cred.access_token = Some("good-token".to_string());
        good_cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            MultiTokenManager::new(config, vec![bad_cred, good_cred], None, None, false).unwrap();

        let mut ctx = manager.acquire_context(None).await.unwrap();
        assert_eq!(ctx.id, 2);
        assert_eq!(ctx.token, "good-token");
        ctx.release_in_flight();
    }

    #[tokio::test]
    async fn test_all_bad_refresh_tokens_are_bounded_by_auth_cooldown() {
        let mut config = Config::default();
        config.load_balancing_mode = "balanced".to_string();

        let mut first = KiroCredentials::default();
        first.refresh_token = Some("bad".to_string());
        let mut second = KiroCredentials::default();
        second.refresh_token = Some("also-bad".to_string());

        let manager =
            MultiTokenManager::new(config, vec![first, second], None, None, false).unwrap();
        let started = Instant::now();
        let err = manager
            .acquire_context(None)
            .await
            .err()
            .unwrap()
            .to_string();

        assert!(
            started.elapsed() < StdDuration::from_millis(500),
            "全部 refreshToken 无效时应按凭据数量和失败阈值有界结束，不应持续打"
        );
        assert!(
            err.contains("所有可用凭据均处于上游临时冷却"),
            "错误应明确结束调度并要求退避，实际: {}",
            err
        );
        let snapshot = manager.snapshot();
        assert_eq!(snapshot.available, 2);
        assert!(snapshot.entries.iter().all(|entry| !entry.disabled));
        assert!(
            snapshot
                .entries
                .iter()
                .all(|entry| entry.refresh_failure_count == 1)
        );
        assert!(snapshot.entries.iter().all(|entry| entry.cooled_down));
    }

    #[tokio::test]
    async fn test_acquire_context_sticks_same_session_to_same_credential_in_balanced_mode() {
        let mut config = Config::default();
        config.load_balancing_mode = "balanced".to_string();

        let mut cred1 = KiroCredentials::default();
        cred1.access_token = Some("t1".to_string());
        cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let mut cred2 = KiroCredentials::default();
        cred2.access_token = Some("t2".to_string());
        cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();
        let excluded = HashSet::new();

        let first = manager
            .acquire_context_for_session(None, Some("session-a"), &excluded)
            .await
            .unwrap();
        manager.report_success_for_session(first.id, Some("session-a"));

        let second = manager
            .acquire_context_for_session(None, Some("session-a"), &excluded)
            .await
            .unwrap();

        assert_eq!(first.id, second.id);
    }

    #[tokio::test]
    async fn test_model_specific_cooldown_only_blocks_same_model() {
        let mut config = Config::default();
        config.load_balancing_mode = "balanced".to_string();

        let mut cred1 = KiroCredentials::default();
        cred1.access_token = Some("t1".to_string());
        cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let mut cred2 = KiroCredentials::default();
        cred2.access_token = Some("t2".to_string());
        cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();
        manager
            .report_transient_failure_kind(
                1,
                Some("claude-opus-4.8"),
                TransientFailureKind::RateLimit,
                Some(StdDuration::from_secs(60)),
                "429",
            )
            .unwrap();

        let mut sonnet = manager
            .acquire_context_for_session(Some("claude-sonnet-4.6"), None, &HashSet::new())
            .await
            .unwrap();
        assert_eq!(sonnet.id, 1);
        sonnet.release_in_flight();

        let mut opus = manager
            .acquire_context_for_session(Some("claude-opus-4.8"), None, &HashSet::new())
            .await
            .unwrap();
        assert_eq!(opus.id, 2);
        opus.release_in_flight();

        let snapshot = manager.snapshot();
        let entry = snapshot.entries.iter().find(|entry| entry.id == 1).unwrap();
        assert!(entry.cooled_down);
        assert_eq!(entry.cooldowns.len(), 1);
        assert_eq!(entry.cooldowns[0].model.as_deref(), Some("claude-opus-4.8"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn test_model_scoped_429_high_concurrency_disabled_and_model_filters() {
        const OPUS_MODEL: &str = "claude-opus-4.8";
        const CREDENTIAL_COUNT: usize = 12;
        const REQUESTS_PER_MODEL: usize = 120;
        const PER_CREDENTIAL_LIMIT: u32 = 2;
        const GLOBAL_LIMIT: u32 = 12;

        let mut config = Config::default();
        config.load_balancing_mode = "balanced".to_string();
        config.credential_max_concurrent_requests = PER_CREDENTIAL_LIMIT;
        config.dispatch_global_max_concurrent_requests = GLOBAL_LIMIT;
        config.dispatch_max_queued_requests = (REQUESTS_PER_MODEL * 2) as u32;
        config.credential_dispatch_max_wait_secs = 5;
        config.credential_rate_limit_cooldown_secs = 30;
        config.credential_max_cooldown_secs = 30;
        config.credential_cooldown_jitter_percent = 0;

        let mut credentials = (1..=CREDENTIAL_COUNT)
            .map(|idx| {
                let subscription = if idx % 2 == 0 { "Pro" } else { "Free" };
                test_access_token_credential(&format!("token-{idx}"), subscription)
            })
            .collect::<Vec<_>>();
        credentials[10].disabled = true;
        credentials[11].disabled = true;

        let manager =
            Arc::new(MultiTokenManager::new(config, credentials, None, None, false).unwrap());

        for id in [2_u64, 4, 6] {
            manager
                .report_transient_failure_kind(
                    id,
                    Some(OPUS_MODEL),
                    TransientFailureKind::RateLimit,
                    Some(StdDuration::from_secs(30)),
                    "429 opus high concurrency",
                )
                .unwrap();
        }

        let start = Arc::new(tokio::sync::Barrier::new(REQUESTS_PER_MODEL * 2 + 1));
        let selected_sonnet = Arc::new(std::sync::Mutex::new(Vec::<u64>::new()));
        let selected_opus = Arc::new(std::sync::Mutex::new(Vec::<u64>::new()));
        let mut handles = Vec::with_capacity(REQUESTS_PER_MODEL * 2);

        for idx in 0..REQUESTS_PER_MODEL {
            let manager = manager.clone();
            let start = start.clone();
            let selected_sonnet = selected_sonnet.clone();
            handles.push(tokio::spawn(async move {
                start.wait().await;
                let mut ctx = manager.acquire_context(Some(SONNET_MODEL)).await.unwrap();
                let snapshot = manager.snapshot();
                assert!(
                    snapshot.global_in_flight_requests <= GLOBAL_LIMIT,
                    "全局并发超限: {} > {}",
                    snapshot.global_in_flight_requests,
                    GLOBAL_LIMIT
                );
                for entry in &snapshot.entries {
                    assert!(
                        entry.in_flight_requests <= entry.max_concurrent_requests,
                        "凭据 #{} 并发超限: {} > {}",
                        entry.id,
                        entry.in_flight_requests,
                        entry.max_concurrent_requests
                    );
                }
                tokio::time::sleep(StdDuration::from_millis(2 + (idx % 5) as u64)).await;
                let id = ctx.id;
                manager.report_success_with_latency(id, Some(SONNET_MODEL), None);
                ctx.release_in_flight();
                selected_sonnet.lock().unwrap().push(id);
            }));
        }

        for idx in 0..REQUESTS_PER_MODEL {
            let manager = manager.clone();
            let start = start.clone();
            let selected_opus = selected_opus.clone();
            handles.push(tokio::spawn(async move {
                start.wait().await;
                let mut ctx = manager.acquire_context(Some(OPUS_MODEL)).await.unwrap();
                assert!(
                    matches!(ctx.id, 8 | 10),
                    "opus 只能调度未冷却且支持 opus 的 Pro 凭据，实际 #{}",
                    ctx.id
                );
                tokio::time::sleep(StdDuration::from_millis(2 + (idx % 5) as u64)).await;
                let id = ctx.id;
                manager.report_success_with_latency(id, Some(OPUS_MODEL), None);
                ctx.release_in_flight();
                selected_opus.lock().unwrap().push(id);
            }));
        }

        start.wait().await;
        for handle in handles {
            tokio::time::timeout(StdDuration::from_secs(10), handle)
                .await
                .expect("混合模型高并发调度不应超时")
                .expect("混合模型高并发调度不应 panic");
        }

        let sonnet_ids = selected_sonnet.lock().unwrap().clone();
        let opus_ids = selected_opus.lock().unwrap().clone();
        assert_eq!(sonnet_ids.len(), REQUESTS_PER_MODEL);
        assert_eq!(opus_ids.len(), REQUESTS_PER_MODEL);
        assert!(
            [2_u64, 4, 6].iter().any(|id| sonnet_ids.contains(id)),
            "sonnet 应允许使用仅 opus 模型冷却的凭据，实际分布: {:?}",
            sonnet_ids
        );
        assert!(
            opus_ids.iter().all(|id| matches!(*id, 8 | 10)),
            "opus 不应使用 Free、禁用或 opus 冷却凭据，实际分布: {:?}",
            opus_ids
        );

        let snapshot = manager.snapshot();
        assert_eq!(snapshot.global_in_flight_requests, 0);
        assert_eq!(snapshot.queued_requests, 0);
        for id in [2_u64, 4, 6] {
            let entry = snapshot
                .entries
                .iter()
                .find(|entry| entry.id == id)
                .unwrap();
            assert!(entry.cooldowns.iter().any(|cooldown| {
                !cooldown.global && cooldown.model.as_deref() == Some(OPUS_MODEL)
            }));
        }
        for id in [11_u64, 12] {
            assert!(
                snapshot
                    .entries
                    .iter()
                    .find(|entry| entry.id == id)
                    .unwrap()
                    .disabled
            );
        }
    }

    #[tokio::test]
    async fn test_acquire_context_excluded_bound_session_can_fallback() {
        let mut config = Config::default();
        config.load_balancing_mode = "balanced".to_string();

        let mut cred1 = KiroCredentials::default();
        cred1.access_token = Some("t1".to_string());
        cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let mut cred2 = KiroCredentials::default();
        cred2.access_token = Some("t2".to_string());
        cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();
        let empty = HashSet::new();

        let first = manager
            .acquire_context_for_session(None, Some("session-b"), &empty)
            .await
            .unwrap();

        let mut excluded = HashSet::new();
        excluded.insert(first.id);
        let fallback = manager
            .acquire_context_for_session(None, Some("session-b"), &excluded)
            .await
            .unwrap();

        assert_ne!(first.id, fallback.id);

        let rebound = manager
            .acquire_context_for_session(None, Some("session-b"), &empty)
            .await
            .unwrap();
        assert_eq!(first.id, rebound.id);
    }

    #[tokio::test]
    async fn test_bound_session_falls_back_when_bound_credential_is_full() {
        let mut config = Config::default();
        config.load_balancing_mode = "balanced".to_string();
        config.credential_max_concurrent_requests = 1;

        let mut cred1 = KiroCredentials::default();
        cred1.access_token = Some("t1".to_string());
        cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let mut cred2 = KiroCredentials::default();
        cred2.access_token = Some("t2".to_string());
        cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();
        let empty = HashSet::new();

        let mut bound = manager
            .acquire_context_for_session(None, Some("sticky-full"), &empty)
            .await
            .unwrap();
        manager.report_success_for_session(bound.id, Some("sticky-full"));

        let mut fallback = manager
            .acquire_context_for_session(None, Some("sticky-full"), &empty)
            .await
            .unwrap();

        assert_ne!(
            bound.id, fallback.id,
            "同一 sticky 会话绑定账号并发已满时，应临时调度到其他可用账号，而不是等待绑定账号"
        );
        assert!(fallback.fallback_from_sticky);
        assert!(!fallback.sticky_bound);

        fallback.release_in_flight();
        bound.release_in_flight();

        let rebound = manager
            .acquire_context_for_session(None, Some("sticky-full"), &empty)
            .await
            .unwrap();
        assert_eq!(
            rebound.id, bound.id,
            "并发释放后 sticky 会话应回到原绑定账号，保持粘性"
        );
    }

    #[tokio::test]
    async fn test_transient_failure_cools_down_without_disabling_credential() {
        let mut config = Config::default();
        config.load_balancing_mode = "balanced".to_string();
        config.credential_transient_cooldown_secs = 60;

        let mut cred1 = KiroCredentials::default();
        cred1.access_token = Some("t1".to_string());
        cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let mut cred2 = KiroCredentials::default();
        cred2.access_token = Some("t2".to_string());
        cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        assert!(
            manager
                .report_transient_failure(1, None, Some(StdDuration::from_secs(20)), "429")
                .unwrap()
        );

        let snapshot = manager.snapshot();
        let first = snapshot.entries.iter().find(|entry| entry.id == 1).unwrap();
        assert!(!first.disabled);
        assert_eq!(first.failure_count, 0);
        assert!(first.cooled_down);
        assert!(first.cooldown_remaining_secs > 0);

        let mut ctx = manager.acquire_context(None).await.unwrap();
        assert_eq!(ctx.id, 2);
        assert_eq!(manager.available_count(), 2);
        ctx.release_in_flight();
    }

    #[tokio::test]
    async fn test_transient_failure_does_not_shorten_existing_cooldown() {
        let mut config = Config::default();
        config.credential_transient_cooldown_secs = 60;

        let mut cred1 = KiroCredentials::default();
        cred1.access_token = Some("t1".to_string());
        cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let mut cred2 = KiroCredentials::default();
        cred2.access_token = Some("t2".to_string());
        cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        manager
            .report_transient_failure(1, None, Some(StdDuration::from_secs(30)), "long")
            .unwrap();
        manager
            .report_transient_failure(1, None, Some(StdDuration::from_secs(1)), "short")
            .unwrap();

        let snapshot = manager.snapshot();
        let first = snapshot.entries.iter().find(|entry| entry.id == 1).unwrap();
        assert_eq!(first.cooldown_reason.as_deref(), Some("long"));
        assert!(first.cooldown_remaining_secs >= 20);
    }

    #[test]
    fn test_success_does_not_clear_active_transient_cooldown() {
        let mut config = Config::default();
        config.credential_transient_cooldown_secs = 60;

        let mut cred1 = KiroCredentials::default();
        cred1.access_token = Some("t1".to_string());
        cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let mut cred2 = KiroCredentials::default();
        cred2.access_token = Some("t2".to_string());
        cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        manager
            .report_transient_failure(1, None, Some(StdDuration::from_secs(30)), "429")
            .unwrap();
        manager.report_success(1);

        let snapshot = manager.snapshot();
        let first = snapshot.entries.iter().find(|entry| entry.id == 1).unwrap();
        assert!(first.cooled_down);
        assert_eq!(first.success_count, 1);
    }

    #[test]
    fn test_structured_transient_failure_updates_health_and_backoff() {
        let mut config = Config::default();
        config.credential_rate_limit_cooldown_secs = 1;
        config.credential_max_cooldown_secs = 10;
        config.credential_cooldown_backoff_multiplier = 2.0;
        config.credential_cooldown_jitter_percent = 0;
        let manager =
            MultiTokenManager::new(config, vec![KiroCredentials::default()], None, None, false)
                .unwrap();

        manager
            .report_transient_failure_kind(1, None, TransientFailureKind::RateLimit, None, "429")
            .unwrap();
        manager
            .report_transient_failure_kind(
                1,
                None,
                TransientFailureKind::RateLimit,
                None,
                "429 again",
            )
            .unwrap();

        let entry = &manager.snapshot().entries[0];
        assert_eq!(entry.transient_failure_streak, 2);
        assert!(entry.recent_error_rate > 0.0);
        assert_eq!(entry.last_error_kind.as_deref(), Some("rate_limit"));
        assert!(entry.cooldown_remaining_secs >= 2);
        assert!(entry.in_probation);
    }

    #[test]
    fn test_error_specific_cooldown_parameters_are_effective() {
        let cases: [(TransientFailureKind, fn(&mut Config), u64); 6] = [
            (
                TransientFailureKind::RateLimit,
                |config: &mut Config| config.credential_rate_limit_cooldown_secs = 2,
                2,
            ),
            (
                TransientFailureKind::Server,
                |config: &mut Config| config.credential_server_error_cooldown_secs = 3,
                3,
            ),
            (
                TransientFailureKind::Network,
                |config: &mut Config| config.credential_network_error_cooldown_secs = 4,
                4,
            ),
            (
                TransientFailureKind::Stream,
                |config: &mut Config| config.credential_stream_error_cooldown_secs = 5,
                5,
            ),
            (
                TransientFailureKind::Protocol,
                |config: &mut Config| config.credential_protocol_error_cooldown_secs = 6,
                6,
            ),
            (
                TransientFailureKind::Auth,
                |config: &mut Config| config.credential_auth_error_cooldown_secs = 7,
                7,
            ),
        ];

        for (kind, configure, expected_min_secs) in cases {
            let mut config = Config::default();
            config.credential_max_cooldown_secs = 30;
            config.credential_cooldown_backoff_multiplier = 1.0;
            config.credential_cooldown_jitter_percent = 0;
            configure(&mut config);

            let manager = MultiTokenManager::new(
                config,
                vec![test_access_token_credential("token", "Pro")],
                None,
                None,
                false,
            )
            .unwrap();

            manager
                .report_transient_failure_kind(1, None, kind, None, "synthetic")
                .unwrap();
            let entry = &manager.snapshot().entries[0];
            assert_eq!(entry.last_error_kind.as_deref(), Some(kind.as_str()));
            assert!(
                entry.cooldown_remaining_secs >= expected_min_secs,
                "{kind:?} 应使用对应配置冷却，实际 remaining={} expected_min={expected_min_secs}",
                entry.cooldown_remaining_secs
            );
        }
    }

    #[test]
    fn test_scheduler_error_ewma_alpha_changes_error_rate_update() {
        let manager_with_alpha = |alpha: f64| {
            let mut config = Config::default();
            config.scheduler_error_ewma_alpha = alpha;
            config.credential_rate_limit_cooldown_secs = 1;
            config.credential_max_cooldown_secs = 10;
            config.credential_cooldown_jitter_percent = 0;
            MultiTokenManager::new(
                config,
                vec![test_access_token_credential("token", "Pro")],
                None,
                None,
                false,
            )
            .unwrap()
        };

        let low_alpha = manager_with_alpha(0.1);
        let high_alpha = manager_with_alpha(0.9);
        low_alpha
            .report_transient_failure_kind(1, None, TransientFailureKind::RateLimit, None, "429")
            .unwrap();
        high_alpha
            .report_transient_failure_kind(1, None, TransientFailureKind::RateLimit, None, "429")
            .unwrap();

        let low_rate = low_alpha.snapshot().entries[0].recent_error_rate;
        let high_rate = high_alpha.snapshot().entries[0].recent_error_rate;
        assert!(
            high_rate > low_rate,
            "scheduler_error_ewma_alpha 应改变错误率 EWMA 更新幅度，low={low_rate}, high={high_rate}"
        );
        assert!((low_rate - 0.1).abs() < f64::EPSILON);
        assert!((high_rate - 0.9).abs() < f64::EPSILON);
    }

    #[test]
    fn test_health_balanced_score_parameters_are_effective() {
        let mut worse = CredentialEntry {
            id: 1,
            credentials: KiroCredentials {
                priority: 10,
                max_concurrent_requests: Some(2),
                ..Default::default()
            },
            failure_count: 0,
            refresh_failure_count: 0,
            disabled: false,
            disabled_reason: None,
            success_count: 0,
            total_selection_count: 100,
            last_used_at: None,
            cooldown_until: None,
            cooldown_reason: None,
            model_cooldowns: HashMap::new(),
            rate_limit_available_at: None,
            in_flight_requests: 1,
            in_flight_leases: Vec::new(),
            warmup_remaining: 0,
            health: SchedulerHealthState::default(),
            model_health: HashMap::new(),
            selection_events: VecDeque::new(),
        };
        worse.health.recent_error_rate = 0.5;
        worse.health.latency_ewma_ms = Some(1_000.0);
        let now_ms = Utc::now().timestamp_millis();
        worse.health.probation_until_ms = Some(now_ms + 60_000);

        let better = CredentialEntry {
            id: 2,
            credentials: KiroCredentials::default(),
            failure_count: 0,
            refresh_failure_count: 0,
            disabled: false,
            disabled_reason: None,
            success_count: 0,
            total_selection_count: 0,
            last_used_at: None,
            cooldown_until: None,
            cooldown_reason: None,
            model_cooldowns: HashMap::new(),
            rate_limit_available_at: None,
            in_flight_requests: 0,
            in_flight_leases: Vec::new(),
            warmup_remaining: 0,
            health: SchedulerHealthState::default(),
            model_health: HashMap::new(),
            selection_events: VecDeque::new(),
        };

        let mut config = Config::default();
        config.scheduler_priority_weight = 0.0;
        config.scheduler_load_weight = 0.0;
        config.scheduler_error_weight = 0.0;
        config.scheduler_latency_weight = 0.0;
        config.scheduler_probation_weight = 0.0;
        config.scheduler_selection_pressure_weight = 0.0;
        config.scheduler_total_selection_weight = 0.0;

        assert_eq!(
            scheduler_score_with_config(&worse, None, now_ms, 0.0, &config),
            scheduler_score_with_config(&better, None, now_ms, 0.0, &config)
        );

        let weight_setters: [fn(&mut Config); 7] = [
            |config: &mut Config| config.scheduler_priority_weight = 1.0,
            |config: &mut Config| config.scheduler_load_weight = 100.0,
            |config: &mut Config| config.scheduler_error_weight = 100.0,
            |config: &mut Config| config.scheduler_latency_weight = 0.01,
            |config: &mut Config| config.scheduler_probation_weight = 50.0,
            |config: &mut Config| config.scheduler_selection_pressure_weight = 25.0,
            |config: &mut Config| config.scheduler_total_selection_weight = 1.0,
        ];

        for enable_weight in weight_setters {
            let mut weighted = config.clone();
            enable_weight(&mut weighted);
            assert!(
                scheduler_score_with_config(&worse, None, now_ms, 1.0, &weighted)
                    > scheduler_score_with_config(&better, None, now_ms, 0.0, &weighted),
                "启用单个健康调度权重后，较差候选得分应更高"
            );
        }
    }

    #[test]
    fn test_success_updates_health_latency_without_clearing_cooldown() {
        let mut config = Config::default();
        config.credential_stream_error_cooldown_secs = 10;
        let manager =
            MultiTokenManager::new(config, vec![KiroCredentials::default()], None, None, false)
                .unwrap();
        manager
            .report_transient_failure_kind(
                1,
                None,
                TransientFailureKind::Stream,
                None,
                "stream idle timeout",
            )
            .unwrap();
        manager.report_success_with_latency(1, None, Some(StdDuration::from_millis(120)));

        let entry = &manager.snapshot().entries[0];
        assert!(entry.cooled_down);
        assert_eq!(entry.transient_failure_streak, 0);
        assert_eq!(entry.latency_ewma_ms, Some(120.0));
    }

    #[tokio::test]
    async fn test_health_balanced_mode_prefers_best_scored_candidate() {
        let mut config = Config::default();
        config.load_balancing_mode = "health_balanced".to_string();
        config.scheduler_top_k = 1;
        let mut first = KiroCredentials::default();
        first.access_token = Some("first".to_string());
        first.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let mut second = KiroCredentials::default();
        second.access_token = Some("second".to_string());
        second.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let manager =
            MultiTokenManager::new(config, vec![first, second], None, None, false).unwrap();
        {
            let mut entries = manager.entries.lock();
            entries[0].health.recent_error_rate = 1.0;
            entries[0].health.latency_ewma_ms = Some(10_000.0);
        }

        let mut ctx = manager.acquire_context(None).await.unwrap();
        assert_eq!(ctx.id, 2);
        ctx.release_in_flight();
    }

    #[tokio::test]
    async fn test_health_balanced_mode_penalizes_recent_selection_pressure() {
        let mut config = Config::default();
        config.load_balancing_mode = "health_balanced".to_string();
        config.scheduler_top_k = 1;
        config.scheduler_selection_pressure_weight = 100.0;
        let mut first = KiroCredentials::default();
        first.access_token = Some("first".to_string());
        first.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let mut second = KiroCredentials::default();
        second.access_token = Some("second".to_string());
        second.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let manager =
            MultiTokenManager::new(config, vec![first, second], None, None, false).unwrap();
        {
            let mut entries = manager.entries.lock();
            entries[0].health.recent_selection_count_60s = 100;
        }

        let mut ctx = manager.acquire_context(None).await.unwrap();
        assert_eq!(ctx.id, 2);
        ctx.release_in_flight();
    }

    #[tokio::test]
    async fn test_balanced_mode_rotates_all_warming_credentials_by_recent_selection() {
        let mut config = Config::default();
        config.load_balancing_mode = "balanced".to_string();
        let mut first = KiroCredentials::default();
        first.access_token = Some("first".to_string());
        first.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let mut second = KiroCredentials::default();
        second.access_token = Some("second".to_string());
        second.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let mut third = KiroCredentials::default();
        third.access_token = Some("third".to_string());
        third.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let manager =
            MultiTokenManager::new(config, vec![first, second, third], None, None, false).unwrap();
        manager.set_warmup_remaining(1, 3).unwrap();
        manager.set_warmup_remaining(2, 3).unwrap();
        manager.set_warmup_remaining(3, 3).unwrap();

        let mut seen = Vec::new();
        for _ in 0..3 {
            let mut ctx = manager.acquire_context(None).await.unwrap();
            seen.push(ctx.id);
            ctx.release_in_flight();
        }

        assert_eq!(seen, vec![1, 2, 3]);
        let snapshot = manager.snapshot();
        assert_eq!(
            snapshot
                .entries
                .iter()
                .map(|entry| entry.recent_scheduler_selection_count_60s)
                .collect::<Vec<_>>(),
            vec![1, 1, 1]
        );
    }

    #[tokio::test]
    async fn test_balanced_mode_gives_warming_group_scaled_target_share() {
        let mut config = Config::default();
        config.load_balancing_mode = "balanced".to_string();
        config.credential_warmup_selection_percent = 5;
        config.credential_warmup_max_selection_percent = 50;
        let mut ready = KiroCredentials::default();
        ready.access_token = Some("ready".to_string());
        ready.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let mut warming_a = KiroCredentials::default();
        warming_a.access_token = Some("warming-a".to_string());
        warming_a.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let mut warming_b = KiroCredentials::default();
        warming_b.access_token = Some("warming-b".to_string());
        warming_b.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let manager =
            MultiTokenManager::new(config, vec![ready, warming_a, warming_b], None, None, false)
                .unwrap();
        manager.set_warmup_remaining(2, 3).unwrap();
        manager.set_warmup_remaining(3, 3).unwrap();

        let mut ctx = manager.acquire_context(None).await.unwrap();
        assert_ne!(ctx.id, 1);
        ctx.release_in_flight();
    }

    #[tokio::test]
    async fn test_simulation_balanced_mode_spreads_new_warming_batch() {
        let mut config = Config::default();
        config.load_balancing_mode = "balanced".to_string();
        let credentials = (1_u64..=10)
            .map(|id| api_key_credential(&format!("ksk_warmup_{id}")))
            .collect::<Vec<_>>();
        let manager = MultiTokenManager::new(config, credentials, None, None, false).unwrap();
        for id in 1_u64..=10 {
            manager.set_warmup_remaining(id, 3).unwrap();
        }

        let mut first_round = Vec::new();
        for _ in 0..10 {
            let mut ctx = manager.acquire_context(None).await.unwrap();
            first_round.push(ctx.id);
            ctx.release_in_flight();
        }

        assert_eq!(first_round, (1_u64..=10).collect::<Vec<_>>());

        for _ in 0..40 {
            let mut ctx = manager.acquire_context(None).await.unwrap();
            ctx.release_in_flight();
        }

        let counts = manager
            .snapshot()
            .entries
            .iter()
            .map(|entry| entry.recent_scheduler_selection_count_60s)
            .collect::<Vec<_>>();
        let min = counts.iter().min().copied().unwrap();
        let max = counts.iter().max().copied().unwrap();
        assert!(
            max - min <= 1,
            "新导入预热账号应均衡参与调度，实际近期选中次数: {counts:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_scheduler_handles_500_daily_credentials_1000_rpm_simulation() {
        const CREDENTIAL_COUNT: usize = 500;
        const REQUEST_COUNT: usize = 1000;
        const MAX_ELAPSED: StdDuration = StdDuration::from_secs(5);

        let mut config = Config::default();
        config.load_balancing_mode = "balanced".to_string();
        config.credential_max_concurrent_requests = 0;
        config.dispatch_global_max_concurrent_requests = 0;
        config.dispatch_max_queued_requests = 0;

        let credentials = (1..=CREDENTIAL_COUNT)
            .map(|id| api_key_credential(&format!("ksk_daily_{id}")))
            .collect::<Vec<_>>();
        let manager =
            Arc::new(MultiTokenManager::new(config, credentials, None, None, false).unwrap());

        let started_at = Instant::now();
        let mut selection_counts: HashMap<u64, usize> = HashMap::new();
        for _ in 0..REQUEST_COUNT {
            let mut ctx =
                tokio::time::timeout(StdDuration::from_secs(1), manager.acquire_context(None))
                    .await
                    .expect("500 日抛凭据调度不应单次超时")
                    .expect("500 日抛凭据调度不应失败");
            *selection_counts.entry(ctx.id).or_insert(0) += 1;
            ctx.release_in_flight();
        }
        let elapsed = started_at.elapsed();

        assert!(
            elapsed <= MAX_ELAPSED,
            "500 日抛凭据、1000 RPM 等价调度耗时过高: {:?} > {:?}",
            elapsed,
            MAX_ELAPSED
        );
        assert_eq!(
            selection_counts.len(),
            CREDENTIAL_COUNT,
            "1000 次调度应覆盖全部 500 个凭据，实际覆盖 {} 个",
            selection_counts.len()
        );
        let min = selection_counts.values().min().copied().unwrap_or_default();
        let max = selection_counts.values().max().copied().unwrap_or_default();
        assert!(
            max - min <= 1,
            "balanced 模式在 500 凭据/1000 次调度下分布应接近均匀，实际 min={min}, max={max}"
        );

        let snapshot = manager.snapshot();
        assert_eq!(snapshot.global_in_flight_requests, 0);
        assert_eq!(snapshot.queued_requests, 0);
    }

    #[tokio::test]
    async fn test_simulation_mixed_large_requests_failures_and_disabled_accounts() {
        let mut config = Config::default();
        config.load_balancing_mode = "balanced".to_string();
        config.credential_max_concurrent_requests = 1;
        config.credential_rate_limit_cooldown_secs = 60;
        config.credential_server_error_cooldown_secs = 60;
        config.credential_max_cooldown_secs = 60;
        config.credential_cooldown_jitter_percent = 0;
        config.credential_dispatch_max_wait_secs = 2;

        let mut credentials = (1_u64..=8)
            .map(|id| api_key_credential(&format!("ksk_mixed_{id}")))
            .collect::<Vec<_>>();
        credentials[1].disabled = true;
        let manager =
            Arc::new(MultiTokenManager::new(config, credentials, None, None, false).unwrap());

        assert!(manager.report_quota_exhausted(3));
        assert!(manager.report_risk_controlled(
            4,
            CredentialRiskControlReason::TemporarilySuspended,
            "TEMPORARILY_SUSPENDED"
        ));
        assert!(
            manager
                .report_transient_failure_kind(
                    5,
                    None,
                    TransientFailureKind::RateLimit,
                    Some(StdDuration::from_secs(30)),
                    "429 Too Many Requests",
                )
                .unwrap()
        );
        assert!(
            manager
                .report_transient_failure_kind(
                    6,
                    None,
                    TransientFailureKind::Server,
                    Some(StdDuration::from_secs(30)),
                    "502 Bad Gateway",
                )
                .unwrap()
        );

        let snapshot = manager.snapshot();
        assert_eq!(snapshot.available, 5);
        for id in [2, 3, 4] {
            let entry = snapshot
                .entries
                .iter()
                .find(|entry| entry.id == id)
                .unwrap();
            assert!(entry.disabled, "凭据 #{id} 应被禁用");
        }
        for id in [5, 6] {
            let entry = snapshot
                .entries
                .iter()
                .find(|entry| entry.id == id)
                .unwrap();
            assert!(entry.cooled_down, "凭据 #{id} 应处于瞬态冷却");
        }

        let mut long_a = manager.acquire_context(None).await.unwrap();
        let mut long_b = manager.acquire_context(None).await.unwrap();
        let mut long_c = manager.acquire_context(None).await.unwrap();
        assert_eq!(vec![long_a.id, long_b.id, long_c.id], vec![1, 7, 8]);

        let waiting_manager = manager.clone();
        let waiting = tokio::spawn(async move { waiting_manager.acquire_context(None).await });
        tokio::time::sleep(StdDuration::from_millis(50)).await;
        assert!(
            !waiting.is_finished(),
            "健康账号都被大请求占满时，后续请求应排队等待"
        );

        long_b.release_in_flight();
        let mut recovered = tokio::time::timeout(StdDuration::from_secs(1), waiting)
            .await
            .expect("释放一个健康账号后等待请求应恢复")
            .expect("等待任务不应 panic")
            .expect("等待请求应成功获取凭据");
        assert_eq!(recovered.id, 7);

        recovered.release_in_flight();
        long_a.release_in_flight();
        long_c.release_in_flight();
        assert_eq!(manager.snapshot().global_in_flight_requests, 0);
    }

    #[tokio::test]
    async fn test_transient_failure_cools_down_only_usable_credential() {
        let mut config = Config::default();
        config.credential_transient_cooldown_secs = 1;
        config.credential_max_cooldown_secs = 1;

        let mut disabled = KiroCredentials::default();
        disabled.disabled = true;
        let mut active = KiroCredentials::default();
        active.access_token = Some("active-token".to_string());
        active.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager = Arc::new(
            MultiTokenManager::new(config, vec![disabled, active], None, None, false).unwrap(),
        );

        assert!(
            !manager
                .report_transient_failure(2, None, Some(StdDuration::from_millis(20)), "429")
                .unwrap()
        );

        let snapshot = manager.snapshot();
        let active = snapshot.entries.iter().find(|entry| entry.id == 2).unwrap();
        assert!(!active.disabled);
        assert_eq!(active.failure_count, 0);
        assert!(active.cooled_down);

        let started = Instant::now();
        let err = match manager.acquire_context(None).await {
            Ok(mut ctx) => {
                ctx.release_in_flight();
                panic!("唯一可用凭据处于上游冷却时应快速失败")
            }
            Err(err) => err.to_string(),
        };
        assert!(
            started.elapsed() < StdDuration::from_millis(200),
            "全部候选都处于上游冷却时不应排队等待"
        );
        assert!(
            err.contains("所有可用凭据均处于上游临时冷却"),
            "错误应明确提示全部处于上游临时冷却，实际: {}",
            err
        );
        assert!(
            err.contains("retry_after_secs="),
            "错误应携带 retry_after_secs 供下游快速重试退避，实际: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_rate_limiter_prefers_other_dispatchable_credential() {
        let mut config = Config::default();
        config.load_balancing_mode = "balanced".to_string();
        config.credential_rpm = Some(60);

        let mut cred1 = KiroCredentials::default();
        cred1.access_token = Some("t1".to_string());
        cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let mut cred2 = KiroCredentials::default();
        cred2.access_token = Some("t2".to_string());
        cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        let first = manager.acquire_context(None).await.unwrap();
        let second = manager.acquire_context(None).await.unwrap();

        assert_ne!(first.id, second.id);
        assert!(
            manager
                .snapshot()
                .entries
                .iter()
                .filter(|entry| entry.rate_limited)
                .count()
                >= 2
        );
    }

    #[tokio::test]
    async fn test_rate_limiter_waits_until_slot_is_dispatchable() {
        let mut config = Config::default();
        config.credential_rpm = Some(6_000);

        let mut cred = KiroCredentials::default();
        cred.access_token = Some("t1".to_string());
        cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            Arc::new(MultiTokenManager::new(config, vec![cred], None, None, false).unwrap());
        let mut first = manager.acquire_context(None).await.unwrap();
        first.release_in_flight();

        let started = Instant::now();
        let mut second =
            tokio::time::timeout(StdDuration::from_millis(500), manager.acquire_context(None))
                .await
                .expect("本地 RPM 限流恢复后应继续调度")
                .expect("等待请求应成功获取凭据");

        assert!(started.elapsed() >= StdDuration::from_millis(8));
        assert_eq!(second.id, first.id);
        second.release_in_flight();
    }

    #[tokio::test]
    async fn test_runtime_config_disabling_credential_rpm_clears_rate_limit_state() {
        let mut config = Config::default();
        config.credential_rpm = Some(60);

        let manager = MultiTokenManager::new(
            config,
            vec![test_access_token_credential("t1", "Pro")],
            None,
            None,
            false,
        )
        .unwrap();

        let mut first = manager.acquire_context(None).await.unwrap();
        first.release_in_flight();
        assert!(manager.snapshot().entries[0].rate_limited);

        manager
            .update_runtime_config(|config| {
                config.credential_rpm = None;
            })
            .unwrap();

        let snapshot = manager.snapshot();
        assert_eq!(snapshot.entries[0].rate_limited, false);
        assert_eq!(manager.runtime_config().credential_rpm, None);

        let mut second = manager.acquire_context(None).await.unwrap();
        assert_eq!(second.id, first.id);
        second.release_in_flight();
    }

    #[tokio::test]
    async fn test_credential_rpm_override_limits_when_global_unlimited() {
        let mut config = Config::default();
        config.credential_rpm = None;

        let mut cred = test_access_token_credential("t1", "Pro");
        cred.rpm = Some(60);

        let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();
        let mut first = manager.acquire_context(None).await.unwrap();
        first.release_in_flight();

        let snapshot = manager.snapshot();
        assert_eq!(snapshot.entries[0].rpm, 60);
        assert_eq!(snapshot.entries[0].rpm_override, Some(60));
        assert!(snapshot.entries[0].rate_limited);
    }

    #[tokio::test]
    async fn test_credential_rpm_override_zero_bypasses_global_limit() {
        let mut config = Config::default();
        config.credential_rpm = Some(60);

        let mut cred = test_access_token_credential("t1", "Pro");
        cred.rpm = Some(0);

        let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();
        let mut first = manager.acquire_context(None).await.unwrap();
        first.release_in_flight();

        let snapshot = manager.snapshot();
        assert_eq!(snapshot.entries[0].rpm, 0);
        assert_eq!(snapshot.entries[0].rpm_override, Some(0));
        assert!(!snapshot.entries[0].rate_limited);
    }

    #[tokio::test]
    async fn test_all_transient_cooldown_fails_fast() {
        let mut config = Config::default();
        config.credential_transient_cooldown_secs = 1;
        config.credential_max_cooldown_secs = 1;

        let mut cred1 = KiroCredentials::default();
        cred1.access_token = Some("t1".to_string());
        cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let mut cred2 = KiroCredentials::default();
        cred2.access_token = Some("t2".to_string());
        cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager = Arc::new(
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap(),
        );
        assert!(
            manager
                .report_transient_failure(1, None, Some(StdDuration::from_millis(20)), "429")
                .unwrap()
        );
        assert!(
            !manager
                .report_transient_failure(2, None, Some(StdDuration::from_millis(20)), "429")
                .unwrap()
        );

        let started = Instant::now();
        let err = match manager.acquire_context(None).await {
            Ok(mut ctx) => {
                ctx.release_in_flight();
                panic!("所有可用凭据都处于上游冷却时应快速失败")
            }
            Err(err) => err.to_string(),
        };

        assert!(
            started.elapsed() < StdDuration::from_millis(200),
            "全账号上游冷却时不应让请求排队等冷却恢复"
        );
        assert!(
            err.contains("所有可用凭据均处于上游临时冷却"),
            "错误应明确提示全部处于上游临时冷却，实际: {}",
            err
        );
        assert!(
            err.contains("retry_after_secs="),
            "错误应携带 retry_after_secs，实际: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_concurrency_limiter_prefers_other_dispatchable_credential() {
        let mut config = Config::default();
        config.load_balancing_mode = "balanced".to_string();
        config.credential_max_concurrent_requests = 1;

        let mut cred1 = KiroCredentials::default();
        cred1.access_token = Some("t1".to_string());
        cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let mut cred2 = KiroCredentials::default();
        cred2.access_token = Some("t2".to_string());
        cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        let mut first = manager.acquire_context(None).await.unwrap();
        let mut second = manager.acquire_context(None).await.unwrap();

        assert_ne!(first.id, second.id);
        let snapshot = manager.snapshot();
        assert_eq!(
            snapshot
                .entries
                .iter()
                .map(|entry| entry.in_flight_requests)
                .sum::<u32>(),
            2
        );

        first.release_in_flight();
        let snapshot = manager.snapshot();
        let released = snapshot
            .entries
            .iter()
            .find(|entry| entry.id == first.id)
            .unwrap();
        assert_eq!(released.in_flight_requests, 0);
        second.release_in_flight();
    }

    #[tokio::test]
    async fn test_priority_mode_prefers_lower_in_flight_with_same_priority() {
        let config = Config::default();

        let mut cred1 = KiroCredentials::default();
        cred1.access_token = Some("t1".to_string());
        cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let mut cred2 = KiroCredentials::default();
        cred2.access_token = Some("t2".to_string());
        cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        let mut first = manager.acquire_context(None).await.unwrap();
        let mut second = manager.acquire_context(None).await.unwrap();

        assert_ne!(first.id, second.id);

        first.release_in_flight();
        second.release_in_flight();
    }

    #[tokio::test]
    async fn test_global_capacity_limits_dispatch_and_bounds_wait_queue() {
        let mut config = Config::default();
        config.dispatch_global_max_concurrent_requests = 1;
        config.dispatch_max_queued_requests = 1;
        let mut first_cred = KiroCredentials::default();
        first_cred.access_token = Some("first".to_string());
        first_cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let mut second_cred = KiroCredentials::default();
        second_cred.access_token = Some("second".to_string());
        second_cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let manager = Arc::new(
            MultiTokenManager::new(config, vec![first_cred, second_cred], None, None, false)
                .unwrap(),
        );

        let mut first = manager.acquire_context(None).await.unwrap();
        assert_eq!(manager.snapshot().global_in_flight_requests, 1);

        let waiting_manager = manager.clone();
        let waiting = tokio::spawn(async move { waiting_manager.acquire_context(None).await });
        tokio::time::sleep(StdDuration::from_millis(30)).await;
        assert_eq!(manager.snapshot().queued_requests, 1);

        let rejected = match manager.acquire_context(None).await {
            Ok(mut ctx) => {
                ctx.release_in_flight();
                panic!("超过等待队列上限的请求不应获得调度上下文")
            }
            Err(err) => err,
        };
        assert!(rejected.to_string().contains("等待队列已满"));

        first.release_in_flight();
        let mut next = tokio::time::timeout(StdDuration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(manager.snapshot().queued_requests, 0);
        next.release_in_flight();
    }

    #[tokio::test]
    async fn test_fail_fast_global_capacity_full_returns_without_queueing() {
        let mut config = Config::default();
        config.dispatch_global_max_concurrent_requests = 1;
        config.dispatch_max_queued_requests = 10;

        let first_cred = test_access_token_credential("first", "Pro");
        let second_cred = test_access_token_credential("second", "Pro");
        let manager =
            MultiTokenManager::new(config, vec![first_cred, second_cred], None, None, false)
                .unwrap();

        let mut first = manager.acquire_context(None).await.unwrap();
        let err = manager
            .acquire_context_for_session_with_mode(
                None,
                None,
                &HashSet::new(),
                AcquireMode::FailFastOnCapacity,
            )
            .await
            .err()
            .unwrap()
            .to_string();

        assert!(
            err.contains("本地凭据调度容量暂不可用"),
            "fail-fast 模式全局容量满应直接返回容量错误，实际: {}",
            err
        );
        let snapshot = manager.snapshot();
        assert_eq!(snapshot.global_in_flight_requests, 1);
        assert_eq!(snapshot.queued_requests, 0);
        first.release_in_flight();
    }

    #[test]
    fn test_local_pool_route_state_reports_no_credentials_and_all_disabled() {
        let empty = MultiTokenManager::new(Config::default(), vec![], None, None, false).unwrap();
        let empty_state = empty.local_pool_route_state(None);
        assert_eq!(empty_state.kind, LocalPoolRouteStateKind::NoCredentials);
        assert_eq!(empty_state.total, 0);
        assert_eq!(empty_state.available, 0);

        let mut disabled = test_access_token_credential("disabled", "Pro");
        disabled.disabled = true;
        let disabled_manager =
            MultiTokenManager::new(Config::default(), vec![disabled], None, None, false).unwrap();
        let disabled_state = disabled_manager.local_pool_route_state(None);
        assert_eq!(disabled_state.kind, LocalPoolRouteStateKind::AllDisabled);
        assert_eq!(disabled_state.total, 1);
        assert_eq!(disabled_state.available, 0);
    }

    #[tokio::test]
    async fn test_local_pool_route_state_reports_capacity_full_without_queueing() {
        let mut config = Config::default();
        config.credential_max_concurrent_requests = 1;
        config.dispatch_max_queued_requests = 10;
        let manager = MultiTokenManager::new(
            config,
            vec![test_access_token_credential("first", "Pro")],
            None,
            None,
            false,
        )
        .unwrap();

        let ready = manager.local_pool_route_state(None);
        assert_eq!(ready.kind, LocalPoolRouteStateKind::Ready);
        assert_eq!(ready.dispatchable, 1);

        let mut ctx = manager.acquire_context(None).await.unwrap();
        let full = manager.local_pool_route_state(None);
        assert_eq!(full.kind, LocalPoolRouteStateKind::CapacityFull);
        assert_eq!(full.dispatchable, 0);
        assert_eq!(full.concurrency_blocked, 1);
        assert_eq!(full.queued_requests, 0);

        ctx.release_in_flight();
        let ready_again = manager.local_pool_route_state(None);
        assert_eq!(ready_again.kind, LocalPoolRouteStateKind::Ready);
        assert_eq!(ready_again.dispatchable, 1);
    }

    #[tokio::test]
    async fn selection_failure_summary_records_concurrency_full_accounts() {
        let mut config = Config::default();
        config.credential_max_concurrent_requests = 1;
        let manager = MultiTokenManager::new(
            config,
            vec![test_access_token_credential("first", "Pro")],
            None,
            None,
            false,
        )
        .unwrap();

        let mut ctx = manager.acquire_context(None).await.unwrap();
        let summary = manager.selection_failure_summary(
            "req_concurrency",
            "/cc/v1/messages",
            None,
            "凭据调度排队等待超时",
        );

        assert_eq!(summary.stage, SelectionFailureStage::DispatchWait);
        assert_eq!(
            summary.primary_reason,
            AccountRejectReason::AccountConcurrencyFull
        );
        assert_eq!(
            summary
                .reason_counts
                .get(&AccountRejectReason::AccountConcurrencyFull),
            Some(&1)
        );
        assert_eq!(summary.sampled_accounts.len(), 1);
        assert_eq!(
            summary.sampled_accounts[0].reason,
            AccountRejectReason::AccountConcurrencyFull
        );

        ctx.release_in_flight();
    }

    #[tokio::test]
    async fn selection_failure_summary_records_rpm_limited_accounts() {
        let mut config = Config::default();
        config.credential_rpm = Some(60);
        let manager = MultiTokenManager::new(
            config,
            vec![test_access_token_credential("first", "Pro")],
            None,
            None,
            false,
        )
        .unwrap();

        let mut ctx = manager.acquire_context(None).await.unwrap();
        ctx.release_in_flight();
        let summary = manager.selection_failure_summary(
            "req_rpm",
            "/cc/v1/messages",
            None,
            "本地限流 retry_after_secs=1",
        );

        assert_eq!(summary.stage, SelectionFailureStage::RpmLimit);
        assert_eq!(summary.primary_reason, AccountRejectReason::RpmLimited);
        assert_eq!(
            summary.reason_counts.get(&AccountRejectReason::RpmLimited),
            Some(&1)
        );
        assert_eq!(summary.waitable_account_count, 1);
        assert!(summary.retry_after_ms.is_some());
    }

    #[tokio::test]
    async fn selection_failure_summary_records_model_not_supported() {
        let mut free = api_key_credential("ksk_selection_free");
        free.subscription_title = Some("Free".to_string());
        let manager =
            MultiTokenManager::new(Config::default(), vec![free], None, None, false).unwrap();

        let summary = manager.selection_failure_summary(
            "req_model",
            "/cc/v1/messages",
            Some("claude-opus-4-8"),
            "没有支持当前模型的可用凭据",
        );

        assert_eq!(summary.stage, SelectionFailureStage::ModelEligibility);
        assert_eq!(
            summary.primary_reason,
            AccountRejectReason::ModelNotSupported
        );
        assert_eq!(
            summary
                .reason_counts
                .get(&AccountRejectReason::ModelNotSupported),
            Some(&1)
        );
    }

    #[test]
    fn selection_failure_summary_enforces_sample_limit_and_omits_secrets() {
        let credentials = (0..100)
            .map(|idx| {
                let mut credential = api_key_credential(&format!("ksk_secret_selection_{idx:03}"));
                credential.disabled = true;
                credential
            })
            .collect::<Vec<_>>();
        let manager =
            MultiTokenManager::new(Config::default(), credentials, None, None, false).unwrap();

        let summary = manager.selection_failure_summary(
            "req_disabled",
            "/cc/v1/messages",
            None,
            "所有凭据均已禁用",
        );

        assert_eq!(summary.rejected_account_count, 100);
        assert_eq!(
            summary.sampled_accounts.len(),
            SELECTION_FAILURE_SAMPLE_LIMIT
        );
        let serialized = serde_json::to_string(&summary).unwrap();
        assert!(!serialized.contains("ksk_secret_selection"));
        assert!(!serialized.contains("access_token"));
        assert!(!serialized.contains("refresh_token"));
    }

    #[tokio::test]
    async fn test_local_pool_route_state_sees_added_credential_after_empty_pool() {
        let manager = MultiTokenManager::new(Config::default(), vec![], None, None, false).unwrap();
        let empty_state = manager.local_pool_route_state(None);
        assert_eq!(empty_state.kind, LocalPoolRouteStateKind::NoCredentials);

        manager
            .add_credential(api_key_credential("ksk_dynamic_added"))
            .await
            .unwrap();

        let ready = manager.local_pool_route_state(None);
        assert_eq!(ready.kind, LocalPoolRouteStateKind::Ready);
        assert_eq!(ready.total, 1);
        assert_eq!(ready.dispatchable, 1);
    }

    #[test]
    fn test_local_pool_route_state_sees_manual_enable_after_all_disabled() {
        let mut disabled = api_key_credential("ksk_dynamic_disabled");
        disabled.disabled = true;
        let manager =
            MultiTokenManager::new(Config::default(), vec![disabled], None, None, false).unwrap();

        let disabled_state = manager.local_pool_route_state(None);
        assert_eq!(disabled_state.kind, LocalPoolRouteStateKind::AllDisabled);

        manager.set_disabled(1, false).unwrap();

        let ready = manager.local_pool_route_state(None);
        assert_eq!(ready.kind, LocalPoolRouteStateKind::Ready);
        assert_eq!(ready.available, 1);
        assert_eq!(ready.dispatchable, 1);
    }

    #[tokio::test]
    async fn test_local_pool_route_state_sees_model_compatible_credential_added() {
        let mut free = api_key_credential("ksk_model_free");
        free.subscription_title = Some("Free".to_string());
        let manager =
            MultiTokenManager::new(Config::default(), vec![free], None, None, false).unwrap();

        let unsupported = manager.local_pool_route_state(Some("claude-opus-4-8"));
        assert_eq!(unsupported.kind, LocalPoolRouteStateKind::NoModelCompatible);

        let mut pro = api_key_credential("ksk_model_pro");
        pro.subscription_title = Some("Pro".to_string());
        manager.add_credential(pro).await.unwrap();

        let ready = manager.local_pool_route_state(Some("claude-opus-4-8"));
        assert_eq!(ready.kind, LocalPoolRouteStateKind::Ready);
        assert_eq!(ready.model_usable, 1);
        assert_eq!(ready.dispatchable, 1);
    }

    #[test]
    fn test_local_pool_route_state_auto_heals_too_many_failures() {
        let manager = MultiTokenManager::new(
            Config::default(),
            vec![api_key_credential("ksk_auto_heal_preflight")],
            None,
            None,
            false,
        )
        .unwrap();

        assert!(manager.report_failure(1));
        assert!(manager.report_failure(1));
        assert!(!manager.report_failure(1));
        let disabled = manager.snapshot().entries.into_iter().next().unwrap();
        assert!(disabled.disabled);
        assert_eq!(
            disabled.disabled_reason.as_deref(),
            Some(DisabledReason::TooManyFailures.as_str())
        );

        let ready = manager.local_pool_route_state(None);
        assert_eq!(ready.kind, LocalPoolRouteStateKind::Ready);
        assert_eq!(ready.available, 1);
        assert_eq!(ready.dispatchable, 1);

        let healed = manager.snapshot().entries.into_iter().next().unwrap();
        assert!(!healed.disabled);
        assert_eq!(healed.failure_count, 0);
        assert!(healed.disabled_reason.is_none());
    }

    #[tokio::test]
    async fn test_local_pool_route_state_proxy_blocked_recovers_after_resource_enabled() {
        let mut credential = api_key_credential("ksk_proxy_dynamic");
        credential.proxy_resource_id = Some(7);
        let manager =
            MultiTokenManager::new(Config::default(), vec![credential], None, None, false).unwrap();
        manager.proxy_resources.lock().insert(
            7,
            ProxyResourceRuntime {
                id: 7,
                name: "residential".to_string(),
                proxy_url: "http://127.0.0.1:8080".to_string(),
                proxy_username: None,
                proxy_password: None,
                enabled: false,
            },
        );
        manager.invalidate_local_pool_route_state_cache();

        let blocked = manager.local_pool_route_state(None);
        assert_eq!(blocked.kind, LocalPoolRouteStateKind::ProxyBlocked);
        assert_eq!(blocked.proxy_blocked, 1);

        manager.proxy_resources.lock().get_mut(&7).unwrap().enabled = true;
        manager.invalidate_local_pool_route_state_cache();

        let ready = manager.local_pool_route_state(None);
        assert_eq!(ready.kind, LocalPoolRouteStateKind::Ready);
        assert_eq!(ready.dispatchable, 1);

        let mut ctx = manager.acquire_context(None).await.unwrap();
        assert_eq!(ctx.id, 1);
        ctx.release_in_flight();
    }

    #[tokio::test]
    async fn test_concurrency_limiter_skips_disabled_credentials_and_queues_on_only_active() {
        let mut config = Config::default();
        config.load_balancing_mode = "balanced".to_string();
        config.credential_max_concurrent_requests = 1;

        let mut disabled1 = KiroCredentials::default();
        disabled1.disabled = true;
        let mut disabled2 = KiroCredentials::default();
        disabled2.disabled = true;
        let mut active = KiroCredentials::default();
        active.access_token = Some("active-token".to_string());
        active.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager = Arc::new(
            MultiTokenManager::new(
                config,
                vec![disabled1, disabled2, active],
                None,
                None,
                false,
            )
            .unwrap(),
        );
        assert_eq!(manager.available_count(), 1);

        let mut first = manager.acquire_context(None).await.unwrap();
        assert_eq!(first.id, 3);

        let waiting_manager = manager.clone();
        let waiting = tokio::spawn(async move { waiting_manager.acquire_context(None).await });
        tokio::time::sleep(StdDuration::from_millis(50)).await;
        assert!(
            !waiting.is_finished(),
            "只剩一个启用凭据且并发占满时，后续请求应排队等待"
        );

        first.release_in_flight();
        let mut second = tokio::time::timeout(StdDuration::from_secs(1), waiting)
            .await
            .expect("释放唯一启用凭据后等待请求应被唤醒")
            .expect("等待任务不应 panic")
            .expect("等待请求应成功获取唯一启用凭据");

        assert_eq!(second.id, 3);
        second.release_in_flight();

        let snapshot = manager.snapshot();
        assert_eq!(
            snapshot
                .entries
                .iter()
                .map(|entry| entry.in_flight_requests)
                .sum::<u32>(),
            0
        );
    }

    #[tokio::test]
    async fn test_concurrency_limiter_multiple_waiters_are_served_serially_on_one_credential() {
        let mut config = Config::default();
        config.credential_max_concurrent_requests = 1;

        let mut cred = KiroCredentials::default();
        cred.access_token = Some("t1".to_string());
        cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            Arc::new(MultiTokenManager::new(config, vec![cred], None, None, false).unwrap());
        let mut first = manager.acquire_context(None).await.unwrap();

        let (acquired_tx, mut acquired_rx) = tokio::sync::mpsc::channel::<(&'static str, u64)>(2);
        let (release_a_tx, release_a_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_b_tx, release_b_rx) = tokio::sync::oneshot::channel::<()>();
        let mut release_a_tx = Some(release_a_tx);
        let mut release_b_tx = Some(release_b_tx);

        let waiting_a_manager = manager.clone();
        let waiting_a_tx = acquired_tx.clone();
        let waiting_a = tokio::spawn(async move {
            let mut ctx = waiting_a_manager.acquire_context(None).await.unwrap();
            waiting_a_tx.send(("a", ctx.id)).await.unwrap();
            let _ = release_a_rx.await;
            ctx.release_in_flight();
        });
        let waiting_b_manager = manager.clone();
        let waiting_b_tx = acquired_tx;
        let waiting_b = tokio::spawn(async move {
            let mut ctx = waiting_b_manager.acquire_context(None).await.unwrap();
            waiting_b_tx.send(("b", ctx.id)).await.unwrap();
            let _ = release_b_rx.await;
            ctx.release_in_flight();
        });

        tokio::time::sleep(StdDuration::from_millis(50)).await;
        assert!(
            matches!(
                acquired_rx.try_recv(),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            ),
            "首个占用未释放前，两个等待者都不应获得并发槽"
        );

        first.release_in_flight();

        let (second_label, second_id) =
            tokio::time::timeout(StdDuration::from_secs(1), acquired_rx.recv())
                .await
                .expect("第一个等待者应在释放后获得并发槽")
                .expect("等待者应发送获取结果");
        assert_eq!(second_id, 1);
        assert_eq!(manager.snapshot().entries[0].in_flight_requests, 1);

        tokio::time::sleep(StdDuration::from_millis(50)).await;
        assert!(
            matches!(
                acquired_rx.try_recv(),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            ),
            "第二个等待者应继续排队，不能和第一个等待者同时占用同一凭据"
        );

        if second_label == "a" {
            release_a_tx.take().unwrap().send(()).unwrap();
        } else {
            release_b_tx.take().unwrap().send(()).unwrap();
        }

        let (third_label, third_id) =
            tokio::time::timeout(StdDuration::from_secs(1), acquired_rx.recv())
                .await
                .expect("第二个等待者应在前一个请求释放后获得并发槽")
                .expect("等待者应发送获取结果");
        assert_eq!(third_id, 1);

        if third_label == "a" {
            release_a_tx.take().unwrap().send(()).unwrap();
        } else {
            release_b_tx.take().unwrap().send(()).unwrap();
        }

        tokio::time::timeout(StdDuration::from_secs(1), waiting_a)
            .await
            .expect("等待任务 a 应正常结束")
            .expect("等待任务 a 不应 panic");
        tokio::time::timeout(StdDuration::from_secs(1), waiting_b)
            .await
            .expect("等待任务 b 应正常结束")
            .expect("等待任务 b 不应 panic");

        assert_eq!(manager.snapshot().entries[0].in_flight_requests, 0);
    }

    #[tokio::test]
    async fn test_acquire_context_all_manually_disabled_fails_without_queueing() {
        let mut config = Config::default();
        config.credential_max_concurrent_requests = 1;
        config.credential_dispatch_max_wait_secs = 1;

        let mut disabled1 = KiroCredentials::default();
        disabled1.disabled = true;
        let mut disabled2 = KiroCredentials::default();
        disabled2.disabled = true;

        let manager =
            MultiTokenManager::new(config, vec![disabled1, disabled2], None, None, false).unwrap();
        assert_eq!(manager.available_count(), 0);

        let started = Instant::now();
        let err = manager
            .acquire_context(None)
            .await
            .err()
            .unwrap()
            .to_string();

        assert!(
            started.elapsed() < StdDuration::from_millis(200),
            "全部手动禁用不是临时调度阻塞，不应进入并发排队等待"
        );
        assert!(
            err.contains("所有凭据均已禁用"),
            "错误应明确提示全部禁用，实际: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_all_model_incompatible_credentials_fail_fast_without_queueing() {
        let mut config = Config::default();
        config.credential_max_concurrent_requests = 1;
        config.credential_dispatch_max_wait_secs = 1;

        let free_a = test_access_token_credential("free-a", "Free");
        let free_b = test_access_token_credential("free-b", "Free");
        let manager =
            MultiTokenManager::new(config, vec![free_a, free_b], None, None, false).unwrap();

        let started = Instant::now();
        let err = manager
            .acquire_context(Some("claude-opus-4"))
            .await
            .err()
            .unwrap()
            .to_string();

        assert!(
            started.elapsed() < StdDuration::from_millis(200),
            "模型不兼容不是临时容量阻塞，不应进入等待队列"
        );
        assert!(
            err.contains("没有支持当前模型的可用凭据"),
            "错误应明确提示模型不兼容，实际: {}",
            err
        );
        assert_eq!(manager.snapshot().queued_requests, 0);
    }

    #[tokio::test]
    async fn test_concurrency_limiter_waits_until_slot_released() {
        let mut config = Config::default();
        config.credential_max_concurrent_requests = 1;

        let mut cred = KiroCredentials::default();
        cred.access_token = Some("t1".to_string());
        cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            Arc::new(MultiTokenManager::new(config, vec![cred], None, None, false).unwrap());
        let mut first = manager.acquire_context(None).await.unwrap();

        let waiting_manager = manager.clone();
        let waiting = tokio::spawn(async move { waiting_manager.acquire_context(None).await });
        tokio::time::sleep(StdDuration::from_millis(50)).await;
        assert!(
            !waiting.is_finished(),
            "并发占满时请求应排队等待，而不是立即返回"
        );

        first.release_in_flight();
        let mut second = tokio::time::timeout(StdDuration::from_secs(1), waiting)
            .await
            .expect("释放并发槽后等待请求应被唤醒")
            .expect("等待任务不应 panic")
            .expect("等待请求应成功获取凭据");

        assert_eq!(second.id, first.id);
        second.release_in_flight();
    }

    #[tokio::test]
    async fn test_fail_fast_slot_race_reselects_other_available_credential() {
        let mut config = Config::default();
        config.credential_max_concurrent_requests = 1;

        let mut first_cred = KiroCredentials::default();
        first_cred.access_token = Some("t1".to_string());
        first_cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        first_cred.priority = 0;

        let mut second_cred = KiroCredentials::default();
        second_cred.access_token = Some("t2".to_string());
        second_cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        second_cred.priority = 0;

        let manager =
            MultiTokenManager::new(config, vec![first_cred, second_cred], None, None, false)
                .unwrap();
        let mut first = manager.acquire_context(None).await.unwrap();
        assert_eq!(first.id, 1);

        let mut second = manager
            .acquire_context_for_session_with_mode(
                None,
                None,
                &HashSet::new(),
                AcquireMode::FailFastOnCapacity,
            )
            .await
            .expect("fail-fast should reselect another credential when the selected slot is full");

        assert_eq!(second.id, 2);
        let snapshot = manager.snapshot();
        assert_eq!(snapshot.entries[0].in_flight_requests, 1);
        assert_eq!(snapshot.entries[1].in_flight_requests, 1);

        first.release_in_flight();
        second.release_in_flight();
    }

    #[tokio::test]
    async fn test_credential_concurrency_override_limits_when_global_unlimited() {
        let mut config = Config::default();
        config.credential_max_concurrent_requests = 0;

        let mut cred = KiroCredentials::default();
        cred.access_token = Some("t1".to_string());
        cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        cred.max_concurrent_requests = Some(1);

        let manager =
            Arc::new(MultiTokenManager::new(config, vec![cred], None, None, false).unwrap());
        let mut first = manager.acquire_context(None).await.unwrap();
        let snapshot = manager.snapshot();
        assert_eq!(snapshot.entries[0].max_concurrent_requests, 1);
        assert_eq!(
            snapshot.entries[0].max_concurrent_requests_override,
            Some(1)
        );

        let waiting_manager = manager.clone();
        let waiting = tokio::spawn(async move { waiting_manager.acquire_context(None).await });
        tokio::time::sleep(StdDuration::from_millis(50)).await;
        assert!(
            !waiting.is_finished(),
            "账号级并发覆盖为 1 时，即使全局不限，也应排队等待"
        );

        first.release_in_flight();
        let mut second = tokio::time::timeout(StdDuration::from_secs(1), waiting)
            .await
            .expect("释放账号级并发槽后等待请求应恢复")
            .expect("等待任务不应 panic")
            .expect("等待请求应成功获取凭据");
        second.release_in_flight();
    }

    #[tokio::test]
    async fn test_credential_concurrency_override_zero_bypasses_global_limit() {
        let mut config = Config::default();
        config.credential_max_concurrent_requests = 1;

        let mut cred = KiroCredentials::default();
        cred.access_token = Some("t1".to_string());
        cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        cred.max_concurrent_requests = Some(0);

        let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();
        let mut first = manager.acquire_context(None).await.unwrap();
        let mut second = manager.acquire_context(None).await.unwrap();
        let snapshot = manager.snapshot();
        assert_eq!(snapshot.entries[0].in_flight_requests, 2);
        assert_eq!(snapshot.entries[0].max_concurrent_requests, 0);
        assert_eq!(
            snapshot.entries[0].max_concurrent_requests_override,
            Some(0)
        );
        first.release_in_flight();
        second.release_in_flight();
    }

    #[tokio::test]
    async fn test_credential_concurrency_override_exceeds_global_default() {
        let mut config = Config::default();
        config.credential_max_concurrent_requests = 5;

        let mut cred = KiroCredentials::default();
        cred.access_token = Some("t1".to_string());
        cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        cred.max_concurrent_requests = Some(200);

        let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();
        let mut leases = Vec::new();
        for _ in 0..6 {
            leases.push(manager.acquire_context(None).await.unwrap());
        }

        let runtime_snapshot = manager.snapshot();
        assert_eq!(runtime_snapshot.entries[0].in_flight_requests, 6);
        assert_eq!(runtime_snapshot.entries[0].max_concurrent_requests, 200);
        assert_eq!(
            runtime_snapshot.entries[0].max_concurrent_requests_override,
            Some(200)
        );

        let base_snapshot = manager.base_snapshot();
        assert_eq!(base_snapshot.entries[0].max_concurrent_requests, 200);
        assert_eq!(
            base_snapshot.entries[0].max_concurrent_requests_override,
            Some(200)
        );

        for mut lease in leases {
            lease.release_in_flight();
        }
    }

    #[tokio::test]
    async fn test_concurrency_limiter_times_out_after_dispatch_wait_limit() {
        let mut config = Config::default();
        config.credential_max_concurrent_requests = 1;
        config.credential_dispatch_max_wait_secs = 1;
        config.credential_in_flight_lease_max_secs = 0;

        let mut cred = KiroCredentials::default();
        cred.access_token = Some("t1".to_string());
        cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            Arc::new(MultiTokenManager::new(config, vec![cred], None, None, false).unwrap());
        let mut first = manager.acquire_context(None).await.unwrap();

        let started = Instant::now();
        let err = manager
            .acquire_context(None)
            .await
            .err()
            .unwrap()
            .to_string();

        assert!(
            started.elapsed() >= StdDuration::from_millis(900),
            "排队等待上限生效前不应提前失败"
        );
        assert!(
            err.contains("凭据调度排队等待超时"),
            "错误应提示调度排队超时，实际: {}",
            err
        );
        assert!(
            err.contains("max_wait_secs=1"),
            "错误应包含配置的等待上限，实际: {}",
            err
        );

        first.release_in_flight();
    }

    #[tokio::test]
    async fn test_in_flight_lease_guard_drop_releases_slot() {
        let mut config = Config::default();
        config.credential_max_concurrent_requests = 1;

        let mut cred = KiroCredentials::default();
        cred.access_token = Some("t1".to_string());
        cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();
        {
            let ctx = manager.acquire_context(None).await.unwrap();
            assert_eq!(ctx.in_flight_lease_id(), Some(1));
            let snapshot = manager.snapshot();
            assert_eq!(snapshot.entries[0].in_flight_requests, 1);
        }

        let snapshot = manager.snapshot();
        assert_eq!(snapshot.entries[0].in_flight_requests, 0);
    }

    #[tokio::test]
    async fn test_expired_leaked_in_flight_lease_wakes_waiting_request() {
        let mut config = Config::default();
        config.credential_max_concurrent_requests = 1;
        config.credential_in_flight_lease_max_secs = 1;

        let mut cred = KiroCredentials::default();
        cred.access_token = Some("t1".to_string());
        cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            Arc::new(MultiTokenManager::new(config, vec![cred], None, None, false).unwrap());
        let lease = manager.acquire_in_flight_lease_for_test(1).unwrap();
        manager.age_in_flight_lease_for_test(1, lease.id(), StdDuration::from_secs(2));
        std::mem::forget(lease);

        let waiting_manager = manager.clone();
        let waiting = tokio::spawn(async move { waiting_manager.acquire_context(None).await });

        let mut ctx = tokio::time::timeout(StdDuration::from_secs(1), waiting)
            .await
            .expect("等待请求应触发超时 lease 清理并恢复调度")
            .expect("等待任务不应 panic")
            .expect("等待请求应成功获取凭据");

        assert_eq!(ctx.id, 1);
        assert_eq!(manager.snapshot().entries[0].in_flight_requests, 1);
        ctx.release_in_flight();
        assert_eq!(manager.snapshot().entries[0].in_flight_requests, 0);
    }

    #[tokio::test]
    async fn test_manual_clear_in_flight_leases_wakes_waiting_request() {
        let mut config = Config::default();
        config.credential_max_concurrent_requests = 1;
        config.credential_in_flight_lease_max_secs = 0;

        let mut cred = KiroCredentials::default();
        cred.access_token = Some("t1".to_string());
        cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            Arc::new(MultiTokenManager::new(config, vec![cred], None, None, false).unwrap());
        let lease = manager.acquire_in_flight_lease_for_test(1).unwrap();
        let leaked_lease_id = lease.id();
        std::mem::forget(lease);

        let waiting_manager = manager.clone();
        let waiting = tokio::spawn(async move { waiting_manager.acquire_context(None).await });
        tokio::time::sleep(StdDuration::from_millis(50)).await;
        assert!(
            !waiting.is_finished(),
            "关闭自动回收且 lease 泄漏时，等待请求应保持排队"
        );

        manager.age_in_flight_lease_for_test(1, leaked_lease_id, StdDuration::from_secs(5));
        assert_eq!(
            manager.clear_in_flight_leases(1, Some(StdDuration::from_secs(3))),
            1
        );

        let mut ctx = tokio::time::timeout(StdDuration::from_secs(1), waiting)
            .await
            .expect("手动清理异常占用后等待请求应被唤醒")
            .expect("等待任务不应 panic")
            .expect("等待请求应成功获取凭据");
        assert_eq!(ctx.id, 1);
        ctx.release_in_flight();
    }

    #[tokio::test]
    async fn test_expired_in_flight_lease_is_cleaned_and_dispatch_recovers() {
        let mut config = Config::default();
        config.credential_max_concurrent_requests = 1;
        config.credential_in_flight_lease_max_secs = 1;

        let mut cred = KiroCredentials::default();
        cred.access_token = Some("t1".to_string());
        cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();
        let lease = manager.acquire_in_flight_lease_for_test(1).unwrap();
        manager.age_in_flight_lease_for_test(1, lease.id(), StdDuration::from_secs(2));
        std::mem::forget(lease);

        assert_eq!(manager.cleanup_expired_in_flight_leases(), 1);
        let mut ctx = manager.acquire_context(None).await.unwrap();
        assert_eq!(ctx.id, 1);
        ctx.release_in_flight();
    }

    #[tokio::test]
    async fn test_summary_snapshot_cleans_expired_in_flight_lease() {
        let mut config = Config::default();
        config.credential_max_concurrent_requests = 1;
        config.credential_in_flight_lease_max_secs = 1;

        let mut cred = KiroCredentials::default();
        cred.access_token = Some("t1".to_string());
        cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager = MultiTokenManager::new(config, vec![cred], None, None, false).unwrap();
        let lease = manager.acquire_in_flight_lease_for_test(1).unwrap();
        manager.age_in_flight_lease_for_test(1, lease.id(), StdDuration::from_secs(2));
        std::mem::forget(lease);

        let summary = manager.summary_snapshot();
        assert_eq!(summary.global_in_flight_requests, 0);
        assert_eq!(manager.snapshot().entries[0].in_flight_requests, 0);
    }

    #[tokio::test]
    async fn test_added_credential_warmup_does_not_fake_success_count() {
        let mut config = Config::default();
        config.load_balancing_mode = "balanced".to_string();
        config.credential_warmup_requests = 2;
        config.credential_warmup_selection_percent = 0;

        let mut existing = KiroCredentials::default();
        existing.access_token = Some("existing".to_string());
        existing.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager = MultiTokenManager::new(config, vec![existing], None, None, false).unwrap();

        let mut new_cred = KiroCredentials::default();
        new_cred.kiro_api_key = Some("ksk_new_key".to_string());
        new_cred.auth_method = Some("api_key".to_string());
        let new_id = manager.add_credential(new_cred).await.unwrap();

        let snapshot = manager.snapshot();
        let added = snapshot
            .entries
            .iter()
            .find(|entry| entry.id == new_id)
            .unwrap();
        assert_eq!(added.success_count, 0);
        assert_eq!(added.warmup_remaining, 2);

        let mut ctx = manager.acquire_context(None).await.unwrap();
        assert_ne!(ctx.id, new_id);
        manager.report_success(ctx.id);
        ctx.release_in_flight();

        manager.set_warmup_remaining(new_id, 0).unwrap();
        let mut ctx = manager.acquire_context(None).await.unwrap();
        assert_eq!(ctx.id, new_id);
        manager.report_success(ctx.id);
        ctx.release_in_flight();

        let snapshot = manager.snapshot();
        let added = snapshot
            .entries
            .iter()
            .find(|entry| entry.id == new_id)
            .unwrap();
        assert_eq!(added.success_count, 1);
        assert_eq!(added.warmup_remaining, 0);
    }

    #[tokio::test]
    async fn test_warmup_selection_percent_allows_real_request_sampling() {
        let mut config = Config::default();
        config.load_balancing_mode = "balanced".to_string();
        config.credential_warmup_requests = 2;
        config.credential_warmup_selection_percent = 100;

        let mut ready = KiroCredentials::default();
        ready.access_token = Some("ready".to_string());
        ready.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let mut warming = KiroCredentials::default();
        warming.access_token = Some("warming".to_string());
        warming.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            MultiTokenManager::new(config, vec![ready, warming], None, None, false).unwrap();
        manager.set_warmup_remaining(2, 2).unwrap();

        let mut ctx = manager.acquire_context(None).await.unwrap();
        assert_eq!(ctx.id, 2);
        manager.report_success(ctx.id);
        ctx.release_in_flight();

        let snapshot = manager.snapshot();
        let warming = snapshot.entries.iter().find(|entry| entry.id == 2).unwrap();
        assert_eq!(warming.success_count, 1);
        assert_eq!(warming.warmup_remaining, 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn redis_backed_in_flight_limit_is_shared_between_managers() {
        let Some(redis_store) = test_redis_store().await else {
            eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let mut config = Config::default();
        config.credential_max_concurrent_requests = 1;
        config.credential_dispatch_max_wait_secs = 2;

        let manager_a = Arc::new(
            MultiTokenManager::new_with_stores(
                config.clone(),
                vec![api_key_credential("a")],
                None,
                None,
                false,
                None,
                Some(redis_store.clone()),
            )
            .unwrap(),
        );
        let manager_b = Arc::new(
            MultiTokenManager::new_with_stores(
                config,
                vec![api_key_credential("a")],
                None,
                None,
                false,
                None,
                Some(redis_store),
            )
            .unwrap(),
        );

        let mut first = manager_a.acquire_context(None).await.unwrap();
        let waiting_manager = manager_b.clone();
        let waiting = tokio::spawn(async move { waiting_manager.acquire_context(None).await });
        tokio::time::sleep(StdDuration::from_millis(100)).await;
        assert!(
            !waiting.is_finished(),
            "另一个 manager 应看到 Redis 中的并发占用并排队"
        );

        first.release_in_flight();
        let mut second = tokio::time::timeout(StdDuration::from_secs(2), waiting)
            .await
            .expect("释放 Redis 并发槽后等待请求应恢复")
            .expect("等待任务不应 panic")
            .expect("等待请求应成功");
        second.release_in_flight();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn redis_backed_session_binding_and_cooldown_are_shared_between_managers() {
        let Some(redis_store) = test_redis_store().await else {
            eprintln!("跳过 Redis TokenManager 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };

        let mut config = Config::default();
        config.load_balancing_mode = "balanced".to_string();
        config.credential_transient_cooldown_secs = 60;

        let manager_a = MultiTokenManager::new_with_stores(
            config.clone(),
            vec![api_key_credential("a"), api_key_credential("b")],
            None,
            None,
            false,
            None,
            Some(redis_store.clone()),
        )
        .unwrap();
        let manager_b = MultiTokenManager::new_with_stores(
            config,
            vec![api_key_credential("a"), api_key_credential("b")],
            None,
            None,
            false,
            None,
            Some(redis_store),
        )
        .unwrap();
        let empty = HashSet::new();

        let mut first = manager_a
            .acquire_context_for_session(None, Some("shared-session"), &empty)
            .await
            .unwrap();
        let first_id = first.id;
        first.release_in_flight();

        let mut rebound = manager_b
            .acquire_context_for_session(None, Some("shared-session"), &empty)
            .await
            .unwrap();
        assert_eq!(rebound.id, first_id);
        rebound.release_in_flight();

        assert!(!manager_a.record_session_soft_failure("shared-session", first_id));
        let mut rebound_after_soft_failure = manager_b
            .acquire_context_for_session(None, Some("shared-session"), &empty)
            .await
            .unwrap();
        assert_eq!(rebound_after_soft_failure.id, first_id);
        rebound_after_soft_failure.release_in_flight();
        assert!(
            manager_b.record_session_soft_failure("shared-session", first_id),
            "同凭据重新绑定不应清空 Redis 中已有软失败计数"
        );
        manager_b.clear_session_soft_failure("shared-session", first_id);

        assert!(
            manager_a
                .report_transient_failure(first_id, None, Some(StdDuration::from_secs(30)), "429")
                .unwrap()
        );

        let mut after_cooldown = manager_b.acquire_context(None).await.unwrap();
        assert_ne!(after_cooldown.id, first_id);
        after_cooldown.release_in_flight();
    }

    #[tokio::test]
    async fn test_only_available_credential_is_not_an_alternate_after_soft_failure() {
        let mut config = Config::default();
        config.load_balancing_mode = "balanced".to_string();

        let mut disabled1 = KiroCredentials::default();
        disabled1.disabled = true;
        let mut disabled2 = KiroCredentials::default();
        disabled2.disabled = true;
        let mut active = KiroCredentials::default();
        active.access_token = Some("active-token".to_string());
        active.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager = MultiTokenManager::new(
            config,
            vec![disabled1, disabled2, active],
            None,
            None,
            false,
        )
        .unwrap();
        let empty = HashSet::new();
        let ctx = manager
            .acquire_context_for_session(None, Some("session-only"), &empty)
            .await
            .unwrap();

        assert_eq!(ctx.id, 3);
        assert!(!manager.has_alternate_usable_credential(None, &empty, ctx.id));
        assert!(!manager.record_session_soft_failure("session-only", ctx.id));
        assert!(manager.record_session_soft_failure("session-only", ctx.id));
        assert!(!manager.has_alternate_usable_credential(None, &empty, ctx.id));
    }

    #[tokio::test]
    async fn test_excluding_only_available_credential_reports_temporary_exclusion() {
        let mut config = Config::default();
        config.load_balancing_mode = "balanced".to_string();

        let mut disabled = KiroCredentials::default();
        disabled.disabled = true;
        let mut active = KiroCredentials::default();
        active.access_token = Some("active-token".to_string());
        active.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            MultiTokenManager::new(config, vec![disabled, active], None, None, false).unwrap();
        let mut excluded = HashSet::new();
        excluded.insert(2);

        let err = manager
            .acquire_context_for_session(None, Some("session-excluded"), &excluded)
            .await
            .err()
            .unwrap()
            .to_string();

        assert!(
            err.contains("本次请求临时排除了所有可用凭据"),
            "错误应提示临时排除，实际: {}",
            err
        );
        assert!(
            !err.contains("所有凭据均已禁用"),
            "错误不应误报所有凭据禁用，实际: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_bound_disabled_proxy_resource_is_not_dispatchable() {
        let mut config = Config::default();
        config.load_balancing_mode = "balanced".to_string();

        let mut blocked = KiroCredentials::default();
        blocked.access_token = Some("blocked-token".to_string());
        blocked.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        blocked.proxy_resource_id = Some(7);

        let mut active = KiroCredentials::default();
        active.access_token = Some("active-token".to_string());
        active.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            MultiTokenManager::new(config, vec![blocked, active], None, None, false).unwrap();
        manager.proxy_resources.lock().insert(
            7,
            ProxyResourceRuntime {
                id: 7,
                name: "disabled-proxy".to_string(),
                proxy_url: "socks5h://127.0.0.1:1080".to_string(),
                proxy_username: None,
                proxy_password: None,
                enabled: false,
            },
        );

        let ctx = manager.acquire_context(None).await.unwrap();
        assert_eq!(ctx.id, 2);

        let snapshot = manager.snapshot();
        let blocked = snapshot.entries.iter().find(|entry| entry.id == 1).unwrap();
        assert_eq!(blocked.effective_proxy_source, "resource_disabled");
        assert_eq!(blocked.effective_proxy_url, None);
    }

    #[tokio::test]
    async fn test_all_proxy_blocked_credentials_fail_fast_with_proxy_error() {
        let mut config = Config::default();
        config.proxy_url = Some("http://global-proxy:8080".to_string());
        config.credential_dispatch_max_wait_secs = 1;

        let mut missing_proxy = test_access_token_credential("missing-proxy", "Pro");
        missing_proxy.proxy_resource_id = Some(404);
        let mut disabled_proxy = test_access_token_credential("disabled-proxy", "Pro");
        disabled_proxy.proxy_resource_id = Some(7);

        let manager = MultiTokenManager::new(
            config,
            vec![missing_proxy, disabled_proxy],
            None,
            None,
            false,
        )
        .unwrap();
        manager.proxy_resources.lock().insert(
            7,
            ProxyResourceRuntime {
                id: 7,
                name: "disabled-proxy".to_string(),
                proxy_url: "socks5h://127.0.0.1:1080".to_string(),
                proxy_username: None,
                proxy_password: None,
                enabled: false,
            },
        );

        let started = Instant::now();
        let err = manager
            .acquire_context(None)
            .await
            .err()
            .unwrap()
            .to_string();

        assert!(
            started.elapsed() < StdDuration::from_millis(200),
            "全部凭据代理资源不可用时应快速失败，不应进入容量等待"
        );
        assert!(
            err.contains("代理资源不可用"),
            "错误应明确提示代理资源不可用，实际: {}",
            err
        );
        assert!(
            !err.contains("所有凭据均已禁用") && !err.contains("没有支持当前模型"),
            "代理不可用不应误报禁用或模型不兼容，实际: {}",
            err
        );
        assert_eq!(manager.snapshot().queued_requests, 0);
    }

    #[tokio::test]
    async fn test_bound_missing_proxy_resource_does_not_fallback_to_global_proxy() {
        let mut config = Config::default();
        config.proxy_url = Some("http://global-proxy:8080".to_string());

        let mut credential = KiroCredentials::default();
        credential.access_token = Some("token".to_string());
        credential.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        credential.proxy_resource_id = Some(404);

        let manager = MultiTokenManager::new(config, vec![credential], None, None, false).unwrap();
        let err = manager
            .acquire_context_for_credential(1)
            .await
            .err()
            .unwrap()
            .to_string();

        assert!(
            err.contains("代理资源 #404 不存在"),
            "应返回代理资源缺失错误，实际: {}",
            err
        );
        assert!(
            err.contains("阻止回退"),
            "不应静默回退到全局代理，实际: {}",
            err
        );

        let snapshot = manager.snapshot();
        let entry = snapshot.entries.iter().find(|entry| entry.id == 1).unwrap();
        assert_eq!(entry.effective_proxy_source, "resource_missing");
        assert_eq!(entry.effective_proxy_url, None);
    }

    #[test]
    fn test_external_import_refresh_preserves_bound_proxy_resource() {
        let mut config = Config::default();
        config.proxy_url = Some("http://global-proxy:8080".to_string());

        let manager = MultiTokenManager::new(config, vec![], None, None, false).unwrap();
        manager.proxy_resources.lock().insert(
            7,
            ProxyResourceRuntime {
                id: 7,
                name: "import-proxy".to_string(),
                proxy_url: "socks5h://127.0.0.1:1080".to_string(),
                proxy_username: Some("user".to_string()),
                proxy_password: Some("pass".to_string()),
                enabled: true,
            },
        );

        let mut source = KiroCredentials::default();
        source.proxy_resource_id = Some(7);
        let mut refreshed = KiroCredentials::default();
        refreshed.access_token = Some("refreshed-token".to_string());
        refreshed.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let preserved = MultiTokenManager::preserve_proxy_fields(refreshed, &source);
        assert_eq!(preserved.proxy_resource_id, Some(7));

        let resolved = manager.resolve_proxy_for_credential(preserved).unwrap();
        assert_eq!(
            resolved.proxy_url.as_deref(),
            Some("socks5h://127.0.0.1:1080")
        );
        assert_eq!(resolved.proxy_username.as_deref(), Some("user"));
        assert_eq!(resolved.proxy_password.as_deref(), Some("pass"));
        assert_eq!(
            resolved
                .effective_proxy(manager.proxy.as_ref())
                .unwrap()
                .url,
            "socks5h://127.0.0.1:1080"
        );
    }

    #[tokio::test]
    async fn test_unbind_session_if_bound_to_does_not_clear_original_binding() {
        let mut config = Config::default();
        config.load_balancing_mode = "balanced".to_string();

        let mut cred1 = KiroCredentials::default();
        cred1.access_token = Some("t1".to_string());
        cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let mut cred2 = KiroCredentials::default();
        cred2.access_token = Some("t2".to_string());
        cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();
        let empty = HashSet::new();

        let bound = manager
            .acquire_context_for_session(None, Some("session-c"), &empty)
            .await
            .unwrap();

        let mut excluded = HashSet::new();
        excluded.insert(bound.id);
        let fallback = manager
            .acquire_context_for_session(None, Some("session-c"), &excluded)
            .await
            .unwrap();
        assert_ne!(bound.id, fallback.id);

        manager.unbind_session_if_bound_to("session-c", fallback.id);

        let rebound = manager
            .acquire_context_for_session(None, Some("session-c"), &empty)
            .await
            .unwrap();
        assert_eq!(bound.id, rebound.id);
    }

    #[tokio::test]
    async fn test_current_id_respects_opus_model_filter() {
        let mut free = KiroCredentials::default();
        free.priority = 0;
        free.subscription_title = Some("Free".to_string());
        free.access_token = Some("free-token".to_string());
        free.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let mut pro = KiroCredentials::default();
        pro.priority = 1;
        pro.subscription_title = Some("Pro".to_string());
        pro.access_token = Some("pro-token".to_string());
        pro.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            MultiTokenManager::new(Config::default(), vec![free, pro], None, None, false).unwrap();

        let ctx = manager
            .acquire_context(Some("claude-opus-4"))
            .await
            .unwrap();
        assert_eq!(ctx.id, 2);
        assert_eq!(ctx.token, "pro-token");
    }

    #[tokio::test]
    async fn test_sonnet_model_can_use_free_credentials() {
        let mut free = KiroCredentials::default();
        free.priority = 0;
        free.subscription_title = Some("Free".to_string());
        free.access_token = Some("free-token".to_string());
        free.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let mut pro = KiroCredentials::default();
        pro.priority = 1;
        pro.subscription_title = Some("Pro".to_string());
        pro.access_token = Some("pro-token".to_string());
        pro.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            MultiTokenManager::new(Config::default(), vec![free, pro], None, None, false).unwrap();

        let mut ctx = manager.acquire_context(Some(SONNET_MODEL)).await.unwrap();
        assert_eq!(ctx.id, 1);
        assert_eq!(ctx.token, "free-token");
        ctx.release_in_flight();
    }

    #[tokio::test]
    async fn test_sonnet_bound_session_falls_back_when_bound_credential_is_full() {
        let mut config = Config::default();
        config.load_balancing_mode = "balanced".to_string();
        config.credential_max_concurrent_requests = 1;

        let mut cred1 = KiroCredentials::default();
        cred1.subscription_title = Some("Free".to_string());
        cred1.access_token = Some("t1".to_string());
        cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let mut cred2 = KiroCredentials::default();
        cred2.subscription_title = Some("Free".to_string());
        cred2.access_token = Some("t2".to_string());
        cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();
        let empty = HashSet::new();

        let mut bound = manager
            .acquire_context_for_session(Some(SONNET_MODEL), Some("sonnet-sticky-full"), &empty)
            .await
            .unwrap();
        manager.report_success_for_session(bound.id, Some("sonnet-sticky-full"));

        let mut fallback = manager
            .acquire_context_for_session(Some(SONNET_MODEL), Some("sonnet-sticky-full"), &empty)
            .await
            .unwrap();

        assert_ne!(bound.id, fallback.id);
        assert!(fallback.fallback_from_sticky);
        assert!(!fallback.sticky_bound);

        fallback.release_in_flight();
        bound.release_in_flight();

        let mut rebound = manager
            .acquire_context_for_session(Some(SONNET_MODEL), Some("sonnet-sticky-full"), &empty)
            .await
            .unwrap();
        assert_eq!(rebound.id, bound.id);
        rebound.release_in_flight();
    }

    #[tokio::test]
    async fn test_sonnet_rate_limiter_prefers_other_dispatchable_credential() {
        let mut config = Config::default();
        config.load_balancing_mode = "balanced".to_string();
        config.credential_rpm = Some(60);

        let mut cred1 = KiroCredentials::default();
        cred1.subscription_title = Some("Free".to_string());
        cred1.access_token = Some("t1".to_string());
        cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let mut cred2 = KiroCredentials::default();
        cred2.subscription_title = Some("Free".to_string());
        cred2.access_token = Some("t2".to_string());
        cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        let mut first = manager.acquire_context(Some(SONNET_MODEL)).await.unwrap();
        let mut second = manager.acquire_context(Some(SONNET_MODEL)).await.unwrap();

        assert_ne!(first.id, second.id);
        first.release_in_flight();
        second.release_in_flight();
    }

    #[tokio::test]
    async fn test_sonnet_rate_limit_cooldown_skips_limited_credential() {
        let mut config = Config::default();
        config.load_balancing_mode = "balanced".to_string();
        config.credential_rate_limit_cooldown_secs = 60;
        config.credential_max_cooldown_secs = 60;
        config.credential_cooldown_jitter_percent = 0;

        let mut cred1 = KiroCredentials::default();
        cred1.subscription_title = Some("Free".to_string());
        cred1.access_token = Some("t1".to_string());
        cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let mut cred2 = KiroCredentials::default();
        cred2.subscription_title = Some("Free".to_string());
        cred2.access_token = Some("t2".to_string());
        cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        assert!(
            manager
                .report_transient_failure_kind(
                    1,
                    Some(SONNET_MODEL),
                    TransientFailureKind::RateLimit,
                    None,
                    "429 Too Many Requests"
                )
                .unwrap()
        );

        let mut ctx = manager.acquire_context(Some(SONNET_MODEL)).await.unwrap();
        assert_eq!(ctx.id, 2);
        ctx.release_in_flight();
    }

    #[tokio::test]
    async fn test_sonnet_pool_uses_available_credentials_when_one_is_cooldown_and_sticky_is_full() {
        let mut config = Config::default();
        config.load_balancing_mode = "balanced".to_string();
        config.credential_max_concurrent_requests = 1;
        config.credential_auth_error_cooldown_secs = 60;
        config.credential_max_cooldown_secs = 60;
        config.credential_cooldown_jitter_percent = 0;

        let credentials = (1..=4)
            .map(|idx| {
                let mut cred = KiroCredentials::default();
                cred.subscription_title = Some("Free".to_string());
                cred.access_token = Some(format!("t{idx}"));
                cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
                cred
            })
            .collect();

        let manager = MultiTokenManager::new(config, credentials, None, None, false).unwrap();
        let empty = HashSet::new();

        assert!(
            manager
                .report_transient_failure_kind(
                    1,
                    Some(SONNET_MODEL),
                    TransientFailureKind::Auth,
                    None,
                    "403 Forbidden user is not authorized"
                )
                .unwrap()
        );

        let mut bound = manager
            .acquire_context_for_session(Some(SONNET_MODEL), Some("sonnet-pool-session"), &empty)
            .await
            .unwrap();
        assert_ne!(bound.id, 1);
        manager.report_success_for_session(bound.id, Some("sonnet-pool-session"));

        let mut fallback = manager
            .acquire_context_for_session(Some(SONNET_MODEL), Some("sonnet-pool-session"), &empty)
            .await
            .unwrap();
        assert_ne!(fallback.id, 1);
        assert_ne!(fallback.id, bound.id);
        assert!(fallback.fallback_from_sticky);
        assert!(!fallback.sticky_bound);

        let mut unbound = manager.acquire_context(Some(SONNET_MODEL)).await.unwrap();
        assert_ne!(unbound.id, 1);
        assert_ne!(unbound.id, bound.id);
        assert_ne!(unbound.id, fallback.id);

        unbound.release_in_flight();
        fallback.release_in_flight();
        bound.release_in_flight();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn test_sonnet_high_concurrency_dispatch_respects_limits_and_spreads_load() {
        const CREDENTIAL_COUNT: usize = 24;
        const COOLED_DOWN_CREDENTIALS: u64 = 4;
        const AVAILABLE_CREDENTIALS: usize = CREDENTIAL_COUNT - COOLED_DOWN_CREDENTIALS as usize;
        const REQUEST_COUNT: usize = 600;
        const PER_CREDENTIAL_LIMIT: u32 = 3;
        const GLOBAL_LIMIT: u32 = 48;

        let mut config = Config::default();
        config.load_balancing_mode = "balanced".to_string();
        config.credential_max_concurrent_requests = PER_CREDENTIAL_LIMIT;
        config.dispatch_global_max_concurrent_requests = GLOBAL_LIMIT;
        config.dispatch_max_queued_requests = REQUEST_COUNT as u32;
        config.credential_dispatch_max_wait_secs = 5;
        config.credential_rate_limit_cooldown_secs = 30;
        config.credential_auth_error_cooldown_secs = 30;
        config.credential_max_cooldown_secs = 30;
        config.credential_cooldown_jitter_percent = 0;

        let credentials = (1..=CREDENTIAL_COUNT)
            .map(|idx| {
                let mut cred = KiroCredentials::default();
                cred.subscription_title = Some("Free".to_string());
                cred.email = Some(format!("sonnet-free-{idx}@example.test"));
                cred.access_token = Some(format!("t{idx}"));
                cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
                cred
            })
            .collect();
        let manager =
            Arc::new(MultiTokenManager::new(config, credentials, None, None, false).unwrap());

        for id in 1..=COOLED_DOWN_CREDENTIALS {
            let kind = if id % 2 == 0 {
                TransientFailureKind::RateLimit
            } else {
                TransientFailureKind::Auth
            };
            assert!(
                manager
                    .report_transient_failure_kind(
                        id,
                        Some(SONNET_MODEL),
                        kind,
                        None,
                        "preload high-concurrency cooldown"
                    )
                    .unwrap()
            );
        }

        let start = Arc::new(tokio::sync::Barrier::new(REQUEST_COUNT + 1));
        let mut handles = Vec::with_capacity(REQUEST_COUNT);

        for idx in 0..REQUEST_COUNT {
            let manager = manager.clone();
            let start = start.clone();
            handles.push(tokio::spawn(async move {
                start.wait().await;
                let mut ctx = manager.acquire_context(Some(SONNET_MODEL)).await.unwrap();
                assert!(
                    ctx.id > COOLED_DOWN_CREDENTIALS,
                    "冷却凭据不应被调度，实际选中 #{}",
                    ctx.id
                );

                let snapshot = manager.snapshot();
                assert!(
                    snapshot.global_in_flight_requests <= GLOBAL_LIMIT,
                    "全局并发超限: {} > {}",
                    snapshot.global_in_flight_requests,
                    GLOBAL_LIMIT
                );
                assert!(
                    snapshot.queued_requests <= REQUEST_COUNT as u32,
                    "等待队列超出测试配置: {}",
                    snapshot.queued_requests
                );
                for entry in &snapshot.entries {
                    if entry.id <= COOLED_DOWN_CREDENTIALS {
                        assert_eq!(
                            entry.in_flight_requests, 0,
                            "冷却凭据 #{} 不应持有 in-flight",
                            entry.id
                        );
                    }
                    if entry.max_concurrent_requests > 0 {
                        assert!(
                            entry.in_flight_requests <= entry.max_concurrent_requests,
                            "凭据 #{} 并发超限: {} > {}",
                            entry.id,
                            entry.in_flight_requests,
                            entry.max_concurrent_requests
                        );
                    }
                }

                tokio::time::sleep(StdDuration::from_millis(3 + (idx % 7) as u64)).await;
                manager.report_success(ctx.id);
                let id = ctx.id;
                ctx.release_in_flight();
                id
            }));
        }

        let started_at = Instant::now();
        start.wait().await;

        let mut selection_counts: HashMap<u64, usize> = HashMap::new();
        for handle in handles {
            let selected_id = tokio::time::timeout(StdDuration::from_secs(10), handle)
                .await
                .expect("高并发调度任务不应超时")
                .expect("高并发调度任务不应 panic");
            *selection_counts.entry(selected_id).or_insert(0) += 1;
        }

        let elapsed = started_at.elapsed();
        let snapshot = manager.snapshot();
        assert_eq!(snapshot.global_in_flight_requests, 0);
        assert_eq!(snapshot.queued_requests, 0);
        assert_eq!(
            selection_counts.len(),
            AVAILABLE_CREDENTIALS,
            "所有非冷却凭据都应在高并发下被使用，实际分布: {:?}",
            selection_counts
        );
        assert!(
            selection_counts
                .keys()
                .all(|id| *id > COOLED_DOWN_CREDENTIALS),
            "冷却凭据不应出现在最终调度分布中: {:?}",
            selection_counts
        );

        let min_selected = selection_counts.values().copied().min().unwrap_or(0);
        let max_selected = selection_counts.values().copied().max().unwrap_or(0);
        let mut distribution: Vec<_> = selection_counts
            .iter()
            .map(|(id, count)| (*id, *count))
            .collect();
        distribution.sort_by_key(|(id, _)| *id);
        println!(
            "sonnet high concurrency dispatch: requests={}, total_credentials={}, cooled_down={}, used_credentials={}, global_limit={}, per_credential_limit={}, elapsed_ms={}, min_selected={}, max_selected={}, distribution={:?}",
            REQUEST_COUNT,
            CREDENTIAL_COUNT,
            COOLED_DOWN_CREDENTIALS,
            selection_counts.len(),
            GLOBAL_LIMIT,
            PER_CREDENTIAL_LIMIT,
            elapsed.as_millis(),
            min_selected,
            max_selected,
            distribution
        );
        assert!(
            max_selected <= min_selected * 2 + 10,
            "balanced 高并发分布过度倾斜: min={}, max={}, elapsed={:?}, counts={:?}",
            min_selected,
            max_selected,
            elapsed,
            selection_counts
        );
    }

    #[test]
    fn test_multi_token_manager_report_refresh_failure() {
        let config = Config::default();
        let cred1 = KiroCredentials::default();
        let cred2 = KiroCredentials::default();

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        assert_eq!(manager.available_count(), 2);
        for _ in 0..(MAX_FAILURES_PER_CREDENTIAL - 1) {
            assert!(manager.report_refresh_failure(1));
        }
        assert_eq!(manager.available_count(), 2);

        assert!(manager.report_refresh_failure(1));
        assert_eq!(manager.available_count(), 1);

        let snapshot = manager.snapshot();
        let first = snapshot.entries.iter().find(|e| e.id == 1).unwrap();
        assert!(first.disabled);
        assert_eq!(first.refresh_failure_count, MAX_FAILURES_PER_CREDENTIAL);
        assert_eq!(snapshot.current_id, 2);
    }

    #[tokio::test]
    async fn test_multi_token_manager_refresh_failure_disabled_is_not_auto_recovered() {
        let config = Config::default();
        let cred1 = KiroCredentials::default();
        let cred2 = KiroCredentials::default();

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        for _ in 0..MAX_FAILURES_PER_CREDENTIAL {
            manager.report_refresh_failure(1);
            manager.report_refresh_failure(2);
        }
        assert_eq!(manager.available_count(), 0);

        let err = manager
            .acquire_context(None)
            .await
            .err()
            .unwrap()
            .to_string();
        assert!(
            err.contains("所有凭据均已禁用"),
            "错误应提示所有凭据禁用，实际: {}",
            err
        );
    }

    #[test]
    fn test_multi_token_manager_report_quota_exhausted() {
        let config = Config::default();
        let cred1 = KiroCredentials::default();
        let cred2 = KiroCredentials::default();

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        // 凭据会自动分配 ID（从 1 开始）
        assert_eq!(manager.available_count(), 2);
        assert!(manager.report_quota_exhausted(1));
        assert_eq!(manager.available_count(), 1);

        // 再禁用第二个后，无可用凭据
        assert!(!manager.report_quota_exhausted(2));
        assert_eq!(manager.available_count(), 0);
    }

    #[test]
    fn test_report_risk_controlled_disables_with_specific_reason() {
        let config = Config::default();
        let cred1 = KiroCredentials::default();
        let cred2 = KiroCredentials::default();

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        assert!(manager.report_risk_controlled(
            1,
            CredentialRiskControlReason::TemporarilySuspended,
            "TEMPORARILY_SUSPENDED"
        ));

        let snapshot = manager.snapshot();
        assert_eq!(snapshot.available, 1);
        assert_eq!(snapshot.current_id, 2);
        let disabled = snapshot.entries.iter().find(|entry| entry.id == 1).unwrap();
        assert!(disabled.disabled);
        assert_eq!(
            disabled.disabled_reason.as_deref(),
            Some("TemporarilySuspended")
        );
        assert_eq!(disabled.failure_count, MAX_FAILURES_PER_CREDENTIAL);
    }

    #[tokio::test]
    async fn test_multi_token_manager_quota_disabled_is_not_auto_recovered() {
        let config = Config::default();
        let cred1 = KiroCredentials::default();
        let cred2 = KiroCredentials::default();

        let manager =
            MultiTokenManager::new(config, vec![cred1, cred2], None, None, false).unwrap();

        manager.report_quota_exhausted(1);
        manager.report_quota_exhausted(2);
        assert_eq!(manager.available_count(), 0);

        let err = manager
            .acquire_context(None)
            .await
            .err()
            .unwrap()
            .to_string();
        assert!(
            err.contains("所有凭据均已禁用"),
            "错误应提示所有凭据禁用，实际: {}",
            err
        );
        assert_eq!(manager.available_count(), 0);
    }

    // ============ 凭据级 Region 优先级测试 ============

    #[test]
    fn test_credential_region_priority_uses_credential_auth_region() {
        // 凭据配置了 auth_region 时，应使用凭据的 auth_region
        let mut config = Config::default();
        config.region = "us-west-2".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.auth_region = Some("eu-west-1".to_string());

        let region = credentials.effective_auth_region(&config);
        assert_eq!(region, "eu-west-1");
    }

    #[test]
    fn test_credential_region_priority_fallback_to_credential_region() {
        // 凭据未配置 auth_region 但配置了 region 时，应回退到凭据.region
        let mut config = Config::default();
        config.region = "us-west-2".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.region = Some("eu-central-1".to_string());

        let region = credentials.effective_auth_region(&config);
        assert_eq!(region, "eu-central-1");
    }

    #[test]
    fn test_credential_region_priority_fallback_to_config() {
        // 凭据未配置 auth_region 和 region 时，应回退到 config
        let mut config = Config::default();
        config.region = "us-west-2".to_string();

        let credentials = KiroCredentials::default();
        assert!(credentials.auth_region.is_none());
        assert!(credentials.region.is_none());

        let region = credentials.effective_auth_region(&config);
        assert_eq!(region, "us-west-2");
    }

    #[test]
    fn test_multiple_credentials_use_respective_regions() {
        // 多凭据场景下，不同凭据使用各自的 auth_region
        let mut config = Config::default();
        config.region = "ap-northeast-1".to_string();

        let mut cred1 = KiroCredentials::default();
        cred1.auth_region = Some("us-east-1".to_string());

        let mut cred2 = KiroCredentials::default();
        cred2.region = Some("eu-west-1".to_string());

        let cred3 = KiroCredentials::default(); // 无 region，使用 config

        assert_eq!(cred1.effective_auth_region(&config), "us-east-1");
        assert_eq!(cred2.effective_auth_region(&config), "eu-west-1");
        assert_eq!(cred3.effective_auth_region(&config), "ap-northeast-1");
    }

    #[test]
    fn test_idc_oidc_endpoint_uses_credential_auth_region() {
        // 验证 IdC OIDC endpoint URL 使用凭据 auth_region
        let mut config = Config::default();
        config.region = "us-west-2".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.auth_region = Some("eu-central-1".to_string());

        let region = credentials.effective_auth_region(&config);
        let refresh_url = format!("https://oidc.{}.amazonaws.com/token", region);

        assert_eq!(refresh_url, "https://oidc.eu-central-1.amazonaws.com/token");
    }

    #[test]
    fn test_social_refresh_endpoint_uses_credential_auth_region() {
        // 验证 Social refresh endpoint URL 使用凭据 auth_region
        let mut config = Config::default();
        config.region = "us-west-2".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.auth_region = Some("ap-southeast-1".to_string());

        let region = credentials.effective_auth_region(&config);
        let refresh_url = format!("https://prod.{}.auth.desktop.kiro.dev/refreshToken", region);

        assert_eq!(
            refresh_url,
            "https://prod.ap-southeast-1.auth.desktop.kiro.dev/refreshToken"
        );
    }

    #[test]
    fn test_api_call_uses_effective_api_region() {
        // 验证 API 调用使用 effective_api_region
        let mut config = Config::default();
        config.region = "us-west-2".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.region = Some("eu-west-1".to_string());

        // 凭据.region 不参与 api_region 回退链
        let api_region = credentials.effective_api_region(&config);
        let api_host = format!("q.{}.amazonaws.com", api_region);

        assert_eq!(api_host, "q.us-west-2.amazonaws.com");
    }

    #[test]
    fn test_api_call_uses_credential_api_region() {
        // 凭据配置了 api_region 时，API 调用应使用凭据的 api_region
        let mut config = Config::default();
        config.region = "us-west-2".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.api_region = Some("eu-central-1".to_string());

        let api_region = credentials.effective_api_region(&config);
        let api_host = format!("q.{}.amazonaws.com", api_region);

        assert_eq!(api_host, "q.eu-central-1.amazonaws.com");
    }

    #[test]
    fn test_credential_region_empty_string_treated_as_set() {
        // 空字符串 auth_region 被视为已设置（虽然不推荐，但行为应一致）
        let mut config = Config::default();
        config.region = "us-west-2".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.auth_region = Some("".to_string());

        let region = credentials.effective_auth_region(&config);
        // 空字符串被视为已设置，不会回退到 config
        assert_eq!(region, "");
    }

    #[test]
    fn test_auth_and_api_region_independent() {
        // auth_region 和 api_region 互不影响
        let mut config = Config::default();
        config.region = "default".to_string();

        let mut credentials = KiroCredentials::default();
        credentials.auth_region = Some("auth-only".to_string());
        credentials.api_region = Some("api-only".to_string());

        assert_eq!(credentials.effective_auth_region(&config), "auth-only");
        assert_eq!(credentials.effective_api_region(&config), "api-only");
    }
}
