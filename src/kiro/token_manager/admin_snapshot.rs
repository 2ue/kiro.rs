use serde::Serialize;

use std::collections::HashMap;
use std::time::{Duration as StdDuration, Instant};

use crate::http_client::ProxyConfig;
use crate::kiro::model::credentials::KiroCredentials;
use crate::model::config::Config;

use super::account_state::{CredentialEntry, ProxyResourceRuntime};
use super::capacity::effective_max_concurrent_requests;
use super::cooldown::{entry_any_cooldown_remaining, entry_cooldown_snapshots};
use super::rpm::{effective_rpm, entry_rate_limit_remaining};
use super::strategy::{scheduler_score_with_config, selection_pressure_from_totals};

/// 凭据条目快照（用于 Admin API 读取）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialEntrySnapshot {
    /// 凭据唯一 ID
    pub id: u64,
    /// 凭据创建时间（RFC3339 格式）
    pub created_at: Option<String>,
    /// 凭据更新时间（RFC3339 格式）
    pub updated_at: Option<String>,
    /// 优先级
    pub priority: u32,
    /// 是否被禁用
    pub disabled: bool,
    /// 连续失败次数
    pub failure_count: u32,
    /// 认证方式
    pub auth_method: Option<String>,
    /// 上游身份提供方
    pub provider: Option<String>,
    /// 凭据级兼容 Region（主要作为 Auth Region 的旧字段/回退字段）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// 凭据级 Auth Region 覆盖值。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_region: Option<String>,
    /// 凭据级 API Region 覆盖值。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_region: Option<String>,
    /// 实际生效的 Auth Region。
    pub effective_auth_region: String,
    /// 实际生效的 API Region。
    pub effective_api_region: String,
    /// 是否有 Profile ARN
    pub has_profile_arn: bool,
    /// Token 过期时间
    pub expires_at: Option<String>,
    /// refreshToken 的 SHA-256 哈希（仅 OAuth 凭据，用于前端去重）
    pub refresh_token_hash: Option<String>,
    /// kiroApiKey 的 SHA-256 哈希（仅 API Key 凭据，用于前端去重）
    pub api_key_hash: Option<String>,
    /// kiroApiKey 的脱敏展示（仅 API Key 凭据，用于前端显示）
    pub masked_api_key: Option<String>,
    /// 用户邮箱（用于前端显示）
    pub email: Option<String>,
    /// 订阅等级（KIRO PRO+ / KIRO FREE 等）
    pub subscription_title: Option<String>,
    /// API 调用成功次数
    pub success_count: u64,
    /// 调度器实际选中该凭据的总次数。
    pub total_selection_count: u64,
    /// 最后一次 API 调用时间（RFC3339 格式）
    pub last_used_at: Option<String>,
    /// 是否配置了凭据级代理
    pub has_proxy: bool,
    /// 代理 URL（用于前端展示）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,
    /// 凭据级直接代理账号。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_username: Option<String>,
    /// 凭据级直接代理密码。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_password: Option<String>,
    /// 绑定的代理/家宽资源 ID。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_resource_id: Option<u64>,
    /// 绑定的代理/家宽资源名称。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_resource_name: Option<String>,
    /// 实际生效的代理 URL（直接代理、绑定资源或全局代理）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_proxy_url: Option<String>,
    /// 实际代理来源：direct / resource / global / none。
    pub effective_proxy_source: String,
    /// Token 刷新连续失败次数
    pub refresh_failure_count: u32,
    /// 禁用原因
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
    /// 端点名称（未显式配置时返回 None，由 Admin 层回退到默认值）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// 是否处于临时冷却。
    pub cooled_down: bool,
    /// 临时冷却剩余秒数。
    pub cooldown_remaining_secs: u64,
    /// 临时冷却原因。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cooldown_reason: Option<String>,
    /// 当前所有活动冷却项，包含全局冷却和模型专属冷却。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cooldowns: Vec<CredentialCooldownSnapshot>,
    /// 是否因本地速率限制暂不可用。
    pub rate_limited: bool,
    /// 本地速率限制剩余秒数。
    pub rate_limit_remaining_secs: u64,
    /// 当前正在使用该凭据的请求数。
    pub in_flight_requests: u32,
    /// 最老并发占用已经持续的秒数。
    pub oldest_in_flight_age_secs: u64,
    /// 最近活跃并发占用距离现在的秒数。
    pub newest_in_flight_idle_secs: u64,
    /// 当前生效的单凭据最大并发请求数。0 表示不限制。
    pub max_concurrent_requests: u32,
    /// 凭据级最大并发覆盖值。None 表示继承全局；Some(0) 表示该凭据不限并发。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_concurrent_requests_override: Option<u32>,
    /// 当前生效的凭据每分钟请求数。0 表示不限制。
    pub rpm: u32,
    /// 凭据级 RPM 覆盖值。None 表示继承全局；Some(0) 表示该凭据不限 RPM。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpm_override: Option<u32>,
    /// 并发占用 lease 自动回收阈值。0 表示关闭自动回收。
    pub in_flight_lease_max_secs: u64,
    /// 预热剩余请求数。
    pub warmup_remaining: u32,
    /// 连续瞬态错误次数。
    pub transient_failure_streak: u32,
    /// 近期错误率 EWMA，范围 0..=1。
    pub recent_error_rate: f64,
    /// 成功调用总耗时 EWMA（毫秒）。
    pub latency_ewma_ms: Option<f64>,
    /// 最近瞬态错误类型。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error_kind: Option<String>,
    /// 最近瞬态错误原因。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error_reason: Option<String>,
    /// 最近瞬态错误时间（Unix 毫秒）。
    pub last_error_at_ms: Option<i64>,
    /// 是否处于冷却结束后的降权观察窗口。
    pub in_probation: bool,
    /// 降权观察窗口剩余秒数。
    pub probation_remaining_secs: u64,
    /// 调度选中该凭据次数。
    pub scheduler_selection_count: u64,
    /// 10 秒内调度选中次数。
    pub recent_scheduler_selection_count_10s: u32,
    /// 60 秒内调度选中次数。
    pub recent_scheduler_selection_count_60s: u32,
    /// 5 分钟内调度选中次数。
    pub recent_scheduler_selection_count_5m: u32,
    /// 近期调度压力，1 表示约等于平均份额，越高表示近期被选中过多。
    pub scheduler_selection_pressure: f64,
    /// 当前健康评分；越低越优先，仅健康均衡模式有实际决策意义。
    pub scheduler_score: f64,
}

/// 凭据基础字段快照（用于 Admin 轻量列表）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialBaseSnapshot {
    pub id: u64,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub priority: u32,
    pub disabled: bool,
    pub disabled_reason: Option<String>,
    pub auth_method: Option<String>,
    pub provider: Option<String>,
    pub region: Option<String>,
    pub auth_region: Option<String>,
    pub api_region: Option<String>,
    pub effective_auth_region: String,
    pub effective_api_region: String,
    pub has_profile_arn: bool,
    pub refresh_token_hash: Option<String>,
    pub api_key_hash: Option<String>,
    pub masked_api_key: Option<String>,
    pub email: Option<String>,
    pub subscription_title: Option<String>,
    pub has_proxy: bool,
    pub proxy_url: Option<String>,
    pub proxy_username: Option<String>,
    pub proxy_password: Option<String>,
    pub proxy_resource_id: Option<u64>,
    pub proxy_resource_name: Option<String>,
    pub effective_proxy_url: Option<String>,
    pub effective_proxy_source: String,
    pub endpoint: Option<String>,
    pub max_concurrent_requests: u32,
    pub max_concurrent_requests_override: Option<u32>,
    pub rpm: u32,
    pub rpm_override: Option<u32>,
    pub warmup_remaining: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagerBaseSnapshot {
    pub entries: Vec<CredentialBaseSnapshot>,
    pub current_id: u64,
    pub total: usize,
    pub available: usize,
    pub global_in_flight_requests: u32,
    pub queued_requests: u32,
    pub global_max_concurrent_requests: u32,
    pub max_queued_requests: u32,
    pub runtime_fresh: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagerSummarySnapshot {
    pub current_id: u64,
    pub total: usize,
    pub available: usize,
    pub global_in_flight_requests: u32,
    pub queued_requests: u32,
    pub global_max_concurrent_requests: u32,
    pub max_queued_requests: u32,
    pub runtime_fresh: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagerRuntimeSnapshot {
    pub entries: Vec<CredentialEntrySnapshot>,
    pub current_id: u64,
    pub total: usize,
    pub available: usize,
    pub runtime_fresh: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialCooldownSnapshot {
    pub model: Option<String>,
    pub global: bool,
    pub remaining_secs: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// 凭据管理器状态快照
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagerSnapshot {
    /// 凭据条目列表
    pub entries: Vec<CredentialEntrySnapshot>,
    /// 当前活跃凭据 ID
    pub current_id: u64,
    /// 总凭据数量
    pub total: usize,
    /// 可用凭据数量
    pub available: usize,
    /// 全局正在处理的调度请求数量。
    pub global_in_flight_requests: u32,
    /// 全局等待调度容量的请求数量。
    pub queued_requests: u32,
    /// 全局最大并发限制。0 表示不限。
    pub global_max_concurrent_requests: u32,
    /// 全局等待队列上限。0 表示不限。
    pub max_queued_requests: u32,
}

fn effective_proxy_display_with_resources(
    creds: &KiroCredentials,
    resources: &HashMap<u64, ProxyResourceRuntime>,
    global_proxy: Option<&ProxyConfig>,
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
            match global_proxy {
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

pub(super) fn base_snapshot_from_entry(
    entry: &CredentialEntry,
    config: &Config,
    resources: &HashMap<u64, ProxyResourceRuntime>,
    global_proxy: Option<&ProxyConfig>,
    hash_secret: fn(&str) -> String,
    mask_secret: fn(&str) -> String,
) -> CredentialBaseSnapshot {
    let (effective_proxy_url, effective_proxy_source) =
        effective_proxy_display_with_resources(&entry.credentials, resources, global_proxy);
    let proxy_resource_id = entry.credentials.proxy_resource_id;
    CredentialBaseSnapshot {
        id: entry.id,
        created_at: entry.credentials.created_at.clone(),
        updated_at: entry.credentials.updated_at.clone(),
        priority: entry.credentials.priority,
        disabled: entry.disabled,
        disabled_reason: entry.disabled_reason.map(|r| r.as_str().to_string()),
        auth_method: normalized_auth_method(&entry.credentials),
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
            entry.credentials.refresh_token.as_deref().map(hash_secret)
        },
        api_key_hash: if entry.credentials.is_api_key_credential() {
            entry.credentials.kiro_api_key.as_deref().map(hash_secret)
        } else {
            None
        },
        masked_api_key: if entry.credentials.is_api_key_credential() {
            entry.credentials.kiro_api_key.as_deref().map(mask_secret)
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
        proxy_resource_name: proxy_resource_name_from_resources(resources, proxy_resource_id),
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

pub(super) fn runtime_snapshot_from_entry(
    entry: &CredentialEntry,
    config: &Config,
    resources: &HashMap<u64, ProxyResourceRuntime>,
    global_proxy: Option<&ProxyConfig>,
    max_concurrent_requests: u32,
    lease_max_age: Option<StdDuration>,
    now: Instant,
    now_ms: i64,
    score_total_recent: u64,
    score_candidate_count: usize,
    hash_secret: fn(&str) -> String,
    mask_secret: fn(&str) -> String,
) -> CredentialEntrySnapshot {
    let (effective_proxy_url, effective_proxy_source) =
        effective_proxy_display_with_resources(&entry.credentials, resources, global_proxy);
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
        auth_method: normalized_auth_method(&entry.credentials),
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
            entry.credentials.refresh_token.as_deref().map(hash_secret)
        },
        api_key_hash: if entry.credentials.is_api_key_credential() {
            entry.credentials.kiro_api_key.as_deref().map(hash_secret)
        } else {
            None
        },
        masked_api_key: if entry.credentials.is_api_key_credential() {
            entry.credentials.kiro_api_key.as_deref().map(mask_secret)
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
        proxy_resource_name: proxy_resource_name_from_resources(resources, proxy_resource_id),
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
        max_concurrent_requests: effective_max_concurrent_requests(entry, max_concurrent_requests),
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
