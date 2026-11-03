//! 免登录的只读号池视图。
//!
//! `GET /api/public/pool` 只返回展示账号状态、额度和调度负载所需的字段：不含 token 及其哈希、
//! 代理、完整邮箱、错误原文、任何美元费用和单个账号的使用积分。开关 `publicPoolViewEnabled`
//! 默认关闭，关闭时返回 404；开关只能通过需要 Admin Key 的 `/api/admin/public-pool` 修改。
//!
//! Free 账号按 `publicPoolFreeAccounts` 展示：显示成某个付费档位（默认 Power），或者整个不出现。
//! 这些换算只在本模块里做，现有管理接口和调度不受影响。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    Json, Router,
    extract::{Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::middleware::AdminState;
use super::service::{AdminService, credential_matches_status};
use super::types::{
    AdminErrorResponse, CredentialAccountInfo, CredentialCooldown, CredentialStatusItem,
};
use crate::model::config::PublicPoolFreeAccounts;
use crate::model::model_support::subscription_tier_key;

/// A rebuild reads every account's quota snapshot and usage totals from PgSQL, so anonymous
/// viewers share one build for this long.
const SNAPSHOT_TTL: Duration = Duration::from_secs(5);
const DEFAULT_PAGE_LIMIT: usize = 20;
const MAX_PAGE_LIMIT: usize = 1000;

/// The page's status tabs, with the account page's rules.
const STATUS_FILTERS: [&str; 5] = [
    "normal",
    "cooling",
    "quota_exhausted",
    "account_error",
    "disabled",
];

/// Disable reasons are fixed codes; nothing else is passed on.
const DISABLED_REASONS: [&str; 9] = [
    "Manual",
    "TooManyFailures",
    "TooManyRefreshFailures",
    "QuotaExceeded",
    "InvalidRefreshToken",
    "InvalidConfig",
    "TemporarilySuspended",
    "AccountSuspended",
    "AccountLocked",
];

const AUTH_METHODS: [&str; 4] = ["social", "idc", "api_key", "external_idp"];

// ============ 响应类型 ============

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicPoolResponse {
    pub overview: PublicPoolOverview,
    pub page: usize,
    pub limit: usize,
    pub total_pages: usize,
    pub filtered_total: usize,
    pub items: Vec<PublicPoolAccount>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicPoolOverview {
    pub total: usize,
    pub enabled: usize,
    pub disabled: usize,
    /// Enabled accounts not in an account-wide cooldown.
    pub schedulable: usize,
    pub cooling_down: usize,
    pub in_use: usize,
    pub failing: usize,
    pub normal: usize,
    pub cooling: usize,
    pub quota_exhausted: usize,
    pub account_error: usize,
    pub global_in_flight_requests: u32,
    pub global_max_concurrent_requests: u32,
    pub queued_requests: u32,
    pub realtime: PublicPoolRealtime,
    /// Credits used by all accounts over the recorded history.
    pub consumed_credits: f64,
    /// When the newest quota snapshot was taken.
    pub last_checked_at: Option<String>,
    pub load_balancing_mode: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicPoolRealtime {
    pub window_seconds: u32,
    pub requests: usize,
    pub error_requests: usize,
    pub rpm: f64,
    pub total_tpm: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicPoolAccount {
    pub id: u64,
    /// The email with the middle of its name masked, e.g. `ab***@gmail.com`; absent for API Key accounts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscription_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_method: Option<&'static str>,
    pub endpoint: String,
    pub fast: u32,
    pub priority: u32,
    pub disabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<&'static str>,
    pub is_current: bool,
    pub cooled_down: bool,
    pub cooldown_remaining_secs: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cooldowns: Vec<PublicPoolCooldown>,
    pub rate_limited: bool,
    pub rate_limit_remaining_secs: u64,
    pub in_flight_requests: u32,
    pub max_concurrent_requests: u32,
    pub rpm: u32,
    pub recent_scheduler_selection_count_60s: u32,
    pub success_count: u64,
    pub scheduler_selection_count: u64,
    pub in_probation: bool,
    pub probation_remaining_secs: u64,
    pub warmup_remaining: u32,
    pub transient_failure_streak: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quota: Option<PublicPoolQuota>,
    pub created_at: Option<String>,
    pub last_used_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicPoolCooldown {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub global: bool,
    pub remaining_secs: u64,
}

/// The last quota snapshot, in credits.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicPoolQuota {
    pub current_usage: f64,
    pub usage_limit: f64,
    pub current_overages: f64,
    pub overage_enabled: bool,
    pub checked_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicPoolQuery {
    pub page: Option<usize>,
    pub limit: Option<usize>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicPoolConfig {
    pub enabled: bool,
    pub free_accounts: PublicPoolFreeAccounts,
}

/// `freeAccounts` may be left out to keep the current choice.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicPoolConfigUpdate {
    pub enabled: bool,
    #[serde(default)]
    pub free_accounts: Option<PublicPoolFreeAccounts>,
}

// ============ Free 账号 ============

/// The paid tier a Free account is shown as: its subscription title and its credits per period.
fn disguise_tier(mode: PublicPoolFreeAccounts) -> Option<(&'static str, f64)> {
    match mode {
        PublicPoolFreeAccounts::Power => Some(("KIRO POWER", 10_000.0)),
        PublicPoolFreeAccounts::ProMax => Some(("KIRO PRO MAX", 5_000.0)),
        PublicPoolFreeAccounts::ProPlus => Some(("KIRO PRO+", 2_000.0)),
        PublicPoolFreeAccounts::Hide => None,
    }
}

fn is_free_account(credential: &CredentialStatusItem) -> bool {
    let title = credential
        .account_info
        .as_ref()
        .and_then(|info| info.subscription_title.as_deref())
        .or(credential.subscription_title.as_deref());
    subscription_tier_key(title) == "free"
}

/// A Free account as the page shows it: the paid tier's title and limit, used credits being the
/// credits it has used here wrapped by that limit, no overage. Status tabs then follow the shown
/// quota, so a Free account that ran out does not sit under "额度用尽" with a nearly empty bar.
fn disguise_free_account(
    mut credential: CredentialStatusItem,
    title: &str,
    limit: f64,
) -> CredentialStatusItem {
    let used = if credential.kiro_metering_usage.is_finite() {
        credential.kiro_metering_usage.max(0.0) % limit
    } else {
        0.0
    };
    credential.subscription_title = Some(title.to_string());
    if credential.disabled_reason.as_deref() == Some("QuotaExceeded") {
        credential.disabled_reason = None;
    }
    if let Some(info) = credential.account_info.as_mut() {
        *info = CredentialAccountInfo {
            subscription_title: Some(title.to_string()),
            current_usage: used,
            usage_limit: limit,
            remaining: limit - used,
            usage_percentage: used / limit * 100.0,
            credit_limit: limit,
            credit_remaining: limit - used,
            credit_base: limit,
            credit_bonus: 0.0,
            overage_status: Some("DISABLED".to_string()),
            overage_capability: None,
            overage_cap: 0.0,
            overage_rate: 0.0,
            current_overages: 0.0,
            next_reset_at: None,
            checked_at: info.checked_at.clone(),
        };
    }
    credential
}

/// The accounts the page shows, Free ones hidden or disguised by `mode`.
fn visible_credentials(
    credentials: Vec<CredentialStatusItem>,
    mode: PublicPoolFreeAccounts,
) -> Vec<CredentialStatusItem> {
    credentials
        .into_iter()
        .filter_map(|credential| {
            if !is_free_account(&credential) {
                return Some(credential);
            }
            disguise_tier(mode)
                .map(|(title, limit)| disguise_free_account(credential, title, limit))
        })
        .collect()
}

// ============ 脱敏 ============

/// `ab***@gmail.com`: up to two leading characters of the name, then `***`, then the domain.
/// Short names keep fewer characters, so the name is never shown whole.
fn mask_email(email: &str) -> Option<String> {
    let email = email.trim();
    if email.is_empty() {
        return None;
    }
    let (name, domain) = match email.rsplit_once('@') {
        Some((name, domain)) => (name, Some(domain)),
        None => (email, None),
    };
    let keep = match name.chars().count() {
        0 | 1 => 0,
        2..=3 => 1,
        _ => 2,
    };
    let head: String = name.chars().take(keep).collect();
    Some(match domain {
        Some(domain) => format!("{head}***@{domain}"),
        None => format!("{head}***"),
    })
}

fn known_value(value: Option<&str>, known: &[&'static str]) -> Option<&'static str> {
    let value = value?.trim();
    known.iter().copied().find(|item| *item == value)
}

impl PublicPoolAccount {
    fn from_status(credential: &CredentialStatusItem, cooldowns: &[CredentialCooldown]) -> Self {
        let info = credential.account_info.as_ref();
        Self {
            id: credential.id,
            label: credential.email.as_deref().and_then(mask_email),
            subscription_title: info
                .and_then(|info| info.subscription_title.clone())
                .or_else(|| credential.subscription_title.clone()),
            auth_method: known_value(credential.auth_method.as_deref(), &AUTH_METHODS),
            endpoint: credential.endpoint.clone(),
            fast: credential.fast,
            priority: credential.priority,
            disabled: credential.disabled,
            // An unrecognised reason still shows as disabled, without its text.
            disabled_reason: credential
                .disabled
                .then(|| known_value(credential.disabled_reason.as_deref(), &DISABLED_REASONS))
                .flatten(),
            is_current: credential.is_current,
            cooled_down: credential.cooled_down,
            cooldown_remaining_secs: credential.cooldown_remaining_secs,
            // Model and time only; the reason can quote the upstream response.
            cooldowns: cooldowns
                .iter()
                .map(|cooldown| PublicPoolCooldown {
                    model: cooldown.model.clone(),
                    global: cooldown.global,
                    remaining_secs: cooldown.remaining_secs,
                })
                .collect(),
            rate_limited: credential.rate_limited,
            rate_limit_remaining_secs: credential.rate_limit_remaining_secs,
            in_flight_requests: credential.in_flight_requests,
            max_concurrent_requests: credential.max_concurrent_requests,
            rpm: credential.rpm,
            recent_scheduler_selection_count_60s: credential.recent_scheduler_selection_count_60s,
            success_count: credential.success_count,
            scheduler_selection_count: credential.scheduler_selection_count,
            in_probation: credential.in_probation,
            probation_remaining_secs: credential.probation_remaining_secs,
            warmup_remaining: credential.warmup_remaining,
            transient_failure_streak: credential.transient_failure_streak,
            quota: info.map(|info| PublicPoolQuota {
                current_usage: info.current_usage,
                usage_limit: info.usage_limit,
                current_overages: info.current_overages,
                overage_enabled: info
                    .overage_status
                    .as_deref()
                    .is_some_and(|status| status.eq_ignore_ascii_case("ENABLED")),
                checked_at: info.checked_at.clone(),
            }),
            created_at: credential.created_at.clone(),
            last_used_at: credential.last_used_at.clone(),
        }
    }
}

// ============ 快照 ============

/// One build of the whole pool: masked accounts in display order, each with the status tab it
/// falls under.
#[derive(Debug, Clone)]
struct PublicPoolSnapshot {
    free_accounts: PublicPoolFreeAccounts,
    overview: PublicPoolOverview,
    accounts: Vec<(PublicPoolAccount, Vec<&'static str>)>,
}

impl PublicPoolSnapshot {
    fn build(service: &AdminService, free_accounts: PublicPoolFreeAccounts) -> Self {
        let credentials = service.public_pool_credentials();
        let ids: Vec<u64> = credentials.iter().map(|credential| credential.id).collect();
        let cooldowns = service.public_pool_cooldowns(&ids);
        let summary = service.public_pool_dispatch_summary();
        let realtime = service.get_usage_summary().realtime;
        let mode = service.get_load_balancing_mode().mode;
        Self::from_parts(
            credentials,
            free_accounts,
            &cooldowns,
            summary,
            realtime,
            mode,
        )
    }

    fn from_parts(
        credentials: Vec<CredentialStatusItem>,
        free_accounts: PublicPoolFreeAccounts,
        cooldowns: &HashMap<u64, Vec<CredentialCooldown>>,
        summary: crate::kiro::token_manager::ManagerSummarySnapshot,
        realtime: crate::anthropic::usage::UsageRealtimeStats,
        load_balancing_mode: String,
    ) -> Self {
        let credentials = visible_credentials(credentials, free_accounts);
        let accounts: Vec<(PublicPoolAccount, Vec<&'static str>)> = credentials
            .iter()
            .map(|credential| {
                let statuses = STATUS_FILTERS
                    .iter()
                    .copied()
                    .filter(|status| credential_matches_status(credential, status))
                    .collect();
                let cooldowns = cooldowns
                    .get(&credential.id)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                (
                    PublicPoolAccount::from_status(credential, cooldowns),
                    statuses,
                )
            })
            .collect();
        let count = |status: &str| {
            accounts
                .iter()
                .filter(|(_, statuses)| statuses.contains(&status))
                .count()
        };
        // Counted over the shown accounts with the manager summary's rules, so hidden accounts
        // leave no trace in the totals. Queue and limits are pool-wide and stay as they are.
        let enabled: Vec<&CredentialStatusItem> = credentials
            .iter()
            .filter(|credential| !credential.disabled)
            .collect();
        let global_cooldown = |credential: &CredentialStatusItem| {
            cooldowns
                .get(&credential.id)
                .is_some_and(|items| items.iter().any(|cooldown| cooldown.global))
        };
        let overview = PublicPoolOverview {
            total: credentials.len(),
            enabled: enabled.len(),
            disabled: credentials.len() - enabled.len(),
            schedulable: enabled
                .iter()
                .filter(|credential| !global_cooldown(credential))
                .count(),
            cooling_down: enabled
                .iter()
                .filter(|credential| credential.cooled_down)
                .count(),
            in_use: enabled
                .iter()
                .filter(|credential| credential.in_flight_requests > 0)
                .count(),
            failing: enabled
                .iter()
                .filter(|credential| {
                    credential.failure_count > 0 || credential.refresh_failure_count > 0
                })
                .count(),
            normal: count("normal"),
            cooling: count("cooling"),
            quota_exhausted: count("quota_exhausted"),
            account_error: count("account_error"),
            global_in_flight_requests: credentials
                .iter()
                .map(|credential| credential.in_flight_requests)
                .sum(),
            global_max_concurrent_requests: summary.global_max_concurrent_requests,
            queued_requests: summary.queued_requests,
            realtime: PublicPoolRealtime {
                window_seconds: realtime.window_seconds,
                requests: realtime.requests,
                error_requests: realtime.error_requests,
                rpm: realtime.rpm,
                total_tpm: realtime.total_tpm,
            },
            consumed_credits: credentials
                .iter()
                .map(|credential| credential.kiro_metering_usage)
                .sum(),
            last_checked_at: credentials
                .iter()
                .filter_map(|credential| credential.account_info.as_ref())
                .map(|info| info.checked_at.clone())
                .max(),
            load_balancing_mode,
            updated_at: Utc::now().to_rfc3339(),
        };
        Self {
            free_accounts,
            overview,
            accounts,
        }
    }

    fn page(&self, query: &PublicPoolQuery) -> PublicPoolResponse {
        let status = query
            .status
            .as_deref()
            .map(str::trim)
            .and_then(|status| STATUS_FILTERS.iter().copied().find(|item| *item == status));
        let matching: Vec<&PublicPoolAccount> = self
            .accounts
            .iter()
            .filter(|(_, statuses)| status.is_none_or(|status| statuses.contains(&status)))
            .map(|(account, _)| account)
            .collect();
        let page = query.page.unwrap_or(1).max(1);
        let limit = match query.limit.unwrap_or(DEFAULT_PAGE_LIMIT) {
            0 => DEFAULT_PAGE_LIMIT,
            limit => limit.min(MAX_PAGE_LIMIT),
        };
        let filtered_total = matching.len();
        PublicPoolResponse {
            overview: self.overview.clone(),
            page,
            limit,
            total_pages: filtered_total.div_ceil(limit),
            filtered_total,
            items: matching
                .into_iter()
                .skip(page.saturating_sub(1).saturating_mul(limit))
                .take(limit)
                .cloned()
                .collect(),
        }
    }
}

#[derive(Default)]
struct SnapshotCache {
    built: Option<(Instant, Arc<PublicPoolSnapshot>)>,
}

#[derive(Clone)]
struct PublicPoolState {
    service: Arc<AdminService>,
    cache: Arc<Mutex<SnapshotCache>>,
}

impl PublicPoolState {
    /// Holding the lock across a rebuild makes concurrent viewers wait for one build instead of
    /// each starting their own. A failed build serves the previous one while it exists. A change
    /// of the Free account setting rebuilds at once, so a cached build never shows what was hidden.
    async fn snapshot(&self) -> Option<Arc<PublicPoolSnapshot>> {
        let free_accounts = self.service.public_pool_free_accounts();
        let mut cache = self.cache.lock().await;
        if let Some((built_at, snapshot)) = &cache.built
            && built_at.elapsed() < SNAPSHOT_TTL
            && snapshot.free_accounts == free_accounts
        {
            return Some(snapshot.clone());
        }
        let service = self.service.clone();
        match tokio::task::spawn_blocking(move || {
            PublicPoolSnapshot::build(&service, free_accounts)
        })
        .await
        {
            Ok(snapshot) => {
                let snapshot = Arc::new(snapshot);
                cache.built = Some((Instant::now(), snapshot.clone()));
                Some(snapshot)
            }
            Err(err) => {
                tracing::warn!(error = %err, "构建只读号池快照失败");
                cache
                    .built
                    .as_ref()
                    .map(|(_, snapshot)| snapshot.clone())
                    .filter(|snapshot| snapshot.free_accounts == free_accounts)
            }
        }
    }
}

// ============ 处理函数与路由 ============

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(AdminErrorResponse::not_found("Not Found")),
    )
        .into_response()
}

/// GET /api/public/pool
async fn get_public_pool(
    State(state): State<PublicPoolState>,
    Query(query): Query<PublicPoolQuery>,
) -> Response {
    if !state.service.public_pool_enabled() {
        return not_found();
    }
    let Some(snapshot) = state.snapshot().await else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(AdminErrorResponse::api_error("Service Unavailable")),
        )
            .into_response();
    };
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(snapshot.page(&query)),
    )
        .into_response()
}

/// Mounted at `/api/public` without admin auth. GET only, so nothing here can change state.
pub fn create_public_pool_router(service: Arc<AdminService>) -> Router {
    Router::new()
        .route("/pool", get(get_public_pool))
        .fallback(|| async { not_found() })
        .with_state(PublicPoolState {
            service,
            cache: Arc::new(Mutex::new(SnapshotCache::default())),
        })
}

/// GET /api/admin/public-pool
pub(super) async fn get_public_pool_config(State(state): State<AdminState>) -> impl IntoResponse {
    Json(PublicPoolConfig {
        enabled: state.service.public_pool_enabled(),
        free_accounts: state.service.public_pool_free_accounts(),
    })
}

/// PUT /api/admin/public-pool
pub(super) async fn set_public_pool_config(
    State(state): State<AdminState>,
    Json(payload): Json<PublicPoolConfigUpdate>,
) -> Response {
    let config = PublicPoolConfig {
        enabled: payload.enabled,
        free_accounts: payload
            .free_accounts
            .unwrap_or_else(|| state.service.public_pool_free_accounts()),
    };
    match state
        .service
        .set_public_pool_config(config.enabled, config.free_accounts)
    {
        Ok(()) => Json(config).into_response(),
        Err(e) => (e.status_code(), Json(e.into_response())).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admin::service::tests::credential_item;
    use crate::anthropic::usage::UsageRealtimeStats;
    use crate::kiro::token_manager::{ManagerAccountStateCounts, ManagerSummarySnapshot};

    fn summary(total: usize, available: usize) -> ManagerSummarySnapshot {
        ManagerSummarySnapshot {
            current_id: 1,
            total,
            available,
            global_in_flight_requests: 3,
            queued_requests: 1,
            global_max_concurrent_requests: 64,
            max_queued_requests: 30,
            runtime_fresh: true,
            account_states: ManagerAccountStateCounts {
                schedulable: available,
                cooling_down: 0,
                in_use: 1,
                failing: 0,
            },
        }
    }

    /// An account carrying every secret the admin API returns, so the tests can check none of it
    /// reaches the public response.
    fn loaded_credential(id: u64) -> CredentialStatusItem {
        let mut item = credential_item(
            id,
            false,
            Some("2026-10-01T08:00:00Z"),
            42,
            12.5,
            Some(30.0),
        );
        item.email = Some("alice.wonder@gmail.com".to_string());
        item.auth_method = Some("social".to_string());
        item.refresh_token_hash = Some("refresh-hash-secret".to_string());
        item.api_key_hash = Some("api-key-hash-secret".to_string());
        item.masked_api_key = Some("ksk_ab***yz".to_string());
        item.has_proxy = true;
        item.proxy_url = Some("http://10.0.0.8:3128".to_string());
        item.proxy_username = Some("proxy-user-secret".to_string());
        item.proxy_password = Some("proxy-pass-secret".to_string());
        item.proxy_resource_name = Some("home-broadband-secret".to_string());
        item.effective_proxy_url = Some("http://10.0.0.8:3128".to_string());
        item.cooldown_reason = Some("upstream said: account flagged".to_string());
        item.last_error_kind = Some("auth".to_string());
        item.last_error_reason = Some("last-error-secret".to_string());
        item.tags = vec!["tag-secret".to_string()];
        item.supported_models = vec!["claude-opus-5-5".to_string()];
        item.route_rules = vec!["/route-secret".to_string()];
        item.estimated_cost_usd = 12.5;
        item.kiro_metering_usage = 7.25;
        item
    }

    fn snapshot(credentials: Vec<CredentialStatusItem>) -> PublicPoolSnapshot {
        snapshot_with(credentials, PublicPoolFreeAccounts::Power)
    }

    fn snapshot_with(
        credentials: Vec<CredentialStatusItem>,
        free_accounts: PublicPoolFreeAccounts,
    ) -> PublicPoolSnapshot {
        let total = credentials.len();
        let available = credentials.iter().filter(|item| !item.disabled).count();
        let cooldowns = HashMap::from([(
            1,
            vec![CredentialCooldown {
                model: Some("claude-opus-5-5".to_string()),
                global: false,
                remaining_secs: 30,
                reason: Some("cooldown-reason-secret".to_string()),
            }],
        )]);
        PublicPoolSnapshot::from_parts(
            credentials,
            free_accounts,
            &cooldowns,
            summary(total, available),
            UsageRealtimeStats::empty(60),
            "priority".to_string(),
        )
    }

    fn query(page: Option<usize>, limit: Option<usize>, status: Option<&str>) -> PublicPoolQuery {
        PublicPoolQuery {
            page,
            limit,
            status: status.map(str::to_string),
        }
    }

    #[test]
    fn emails_keep_at_most_two_characters_of_the_name() {
        assert_eq!(
            mask_email("alice.wonder@gmail.com").as_deref(),
            Some("al***@gmail.com")
        );
        assert_eq!(
            mask_email("bob@example.com").as_deref(),
            Some("b***@example.com")
        );
        assert_eq!(
            mask_email("ab@example.com").as_deref(),
            Some("a***@example.com")
        );
        assert_eq!(
            mask_email("x@example.com").as_deref(),
            Some("***@example.com")
        );
        assert_eq!(mask_email("no-at-sign").as_deref(), Some("no***"));
        assert_eq!(mask_email("  ").as_deref(), None);
    }

    #[test]
    fn public_response_carries_no_secret_cost_or_raw_error() {
        let response = snapshot(vec![loaded_credential(1)]).page(&query(None, None, None));
        let json = serde_json::to_string(&response).unwrap();
        for secret in [
            "alice.wonder",
            "refresh-hash-secret",
            "api-key-hash-secret",
            "ksk_",
            "10.0.0.8",
            "proxy-user-secret",
            "proxy-pass-secret",
            "home-broadband-secret",
            "account flagged",
            "cooldown-reason-secret",
            "last-error-secret",
            "tag-secret",
            "/route-secret",
            "machine",
            "identity-hash",
        ] {
            assert!(!json.contains(secret), "{secret} leaked: {json}");
        }
        // No dollar amount of any kind: costs, overage price or cap.
        for field in [
            "Cost",
            "cost",
            "Usd",
            "usd",
            "overageRate",
            "overageCap",
            "creditRemaining",
        ] {
            assert!(!json.contains(field), "{field} leaked: {json}");
        }
        // Credits used per account are recorded by this system, not queried, so they stay out.
        assert!(!json.contains("kiroMeteringUsage"), "{json}");
        let account = &response.items[0];
        assert_eq!(account.label.as_deref(), Some("al***@gmail.com"));
        assert_eq!(response.overview.consumed_credits, 7.25);
        let quota = account.quota.as_ref().unwrap();
        assert_eq!((quota.current_usage, quota.usage_limit), (30.0, 100.0));
        assert!(quota.overage_enabled);
        assert_eq!(account.cooldowns.len(), 1);
    }

    #[test]
    fn unknown_disable_reasons_and_auth_methods_are_dropped() {
        let mut disabled = credential_item(2, true, None, 0, 0.0, None);
        disabled.disabled_reason = Some("upstream body: {\"secret\":1}".to_string());
        disabled.auth_method = Some("weird-method".to_string());
        let mut quota = credential_item(3, true, None, 0, 0.0, None);
        quota.disabled_reason = Some("QuotaExceeded".to_string());
        let response = snapshot(vec![disabled, quota]).page(&query(None, None, None));
        let by_id = |id| response.items.iter().find(|item| item.id == id).unwrap();
        assert_eq!(by_id(2).disabled_reason, None);
        assert_eq!(by_id(2).auth_method, None);
        assert_eq!(by_id(3).disabled_reason, Some("QuotaExceeded"));
        let json = serde_json::to_string(&response).unwrap();
        assert!(
            !json.contains("secret") && !json.contains("weird-method"),
            "{json}"
        );
    }

    #[test]
    fn status_tabs_page_and_count_with_the_account_page_rules() {
        let mut credentials: Vec<_> = (1..=5)
            .map(|id| credential_item(id, false, None, 0, 0.0, Some(10.0)))
            .collect();
        credentials.push(credential_item(6, true, None, 0, 0.0, None));
        let mut exhausted = credential_item(7, false, None, 0, 0.0, Some(100.0));
        exhausted.account_info.as_mut().unwrap().current_usage = 100.0;
        credentials.push(exhausted);
        let snapshot = snapshot(credentials);

        assert_eq!(snapshot.overview.total, 7);
        assert_eq!(snapshot.overview.disabled, 1);
        assert_eq!(snapshot.overview.normal, 5);
        assert_eq!(snapshot.overview.quota_exhausted, 1);

        let normal = snapshot.page(&query(Some(2), Some(2), Some("normal")));
        assert_eq!(normal.filtered_total, 5);
        assert_eq!(normal.total_pages, 3);
        assert_eq!(normal.items.len(), 2);
        assert!(normal.items.iter().all(|item| !item.disabled));

        let disabled = snapshot.page(&query(None, None, Some("disabled")));
        assert_eq!(
            disabled
                .items
                .iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            vec![6]
        );

        // An unknown status shows everything; limits are clamped.
        let all = snapshot.page(&query(Some(0), Some(10_000), Some("proxy_blocked")));
        assert_eq!(
            (all.page, all.limit, all.filtered_total),
            (1, MAX_PAGE_LIMIT, 7)
        );
        assert_eq!(
            snapshot.page(&query(None, Some(0), None)).limit,
            DEFAULT_PAGE_LIMIT
        );
    }

    /// A Free account whose real quota ran out: 50 of 50 queried, overage on, disabled for quota,
    /// with `used` credits recorded here.
    fn free_credential(id: u64, used: f64) -> CredentialStatusItem {
        let mut item = credential_item(id, true, None, 0, 0.0, Some(50.0));
        item.subscription_title = Some("KIRO FREE".to_string());
        item.disabled_reason = Some("QuotaExceeded".to_string());
        item.kiro_metering_usage = used;
        let info = item.account_info.as_mut().unwrap();
        info.subscription_title = Some("KIRO FREE".to_string());
        info.usage_limit = 50.0;
        info.current_overages = 120.0;
        item
    }

    #[test]
    fn free_accounts_show_as_the_chosen_tier_with_used_credits_wrapped_by_its_limit() {
        for (mode, title, limit) in [
            (PublicPoolFreeAccounts::Power, "KIRO POWER", 10_000.0),
            (PublicPoolFreeAccounts::ProMax, "KIRO PRO MAX", 5_000.0),
            (PublicPoolFreeAccounts::ProPlus, "KIRO PRO+", 2_000.0),
        ] {
            let paid = credential_item(1, false, None, 0, 0.0, Some(30.0));
            let snapshot = snapshot_with(vec![paid, free_credential(2, 23_456.5)], mode);
            let response = snapshot.page(&query(None, None, None));
            let free = response.items.iter().find(|item| item.id == 2).unwrap();
            assert_eq!(free.subscription_title.as_deref(), Some(title), "{mode:?}");
            let quota = free.quota.as_ref().unwrap();
            assert_eq!(quota.usage_limit, limit, "{mode:?}");
            assert_eq!(quota.current_usage, 23_456.5 % limit, "{mode:?}");
            assert_eq!(quota.current_overages, 0.0);
            assert!(!quota.overage_enabled);
            // The account stays disabled; only the quota reason goes with the Free quota.
            assert!(free.disabled);
            assert_eq!(free.disabled_reason, None);
            assert_eq!(snapshot.overview.quota_exhausted, 0, "{mode:?}");
            assert_eq!(snapshot.overview.total, 2);

            // Paid accounts keep their own quota, overage and all.
            let paid = response.items.iter().find(|item| item.id == 1).unwrap();
            let quota = paid.quota.as_ref().unwrap();
            assert_eq!((quota.current_usage, quota.usage_limit), (30.0, 100.0));
            assert!(quota.overage_enabled);

            let json = serde_json::to_string(&response).unwrap();
            assert!(!json.to_ascii_lowercase().contains("free"), "{json}");
        }
    }

    #[test]
    fn free_accounts_under_the_limit_show_their_credits_as_they_are() {
        let mut free = free_credential(2, 1_234.0);
        free.disabled = false;
        free.disabled_reason = None;
        let snapshot = snapshot_with(vec![free], PublicPoolFreeAccounts::Power);
        let response = snapshot.page(&query(None, None, None));
        let quota = response.items[0].quota.as_ref().unwrap();
        assert_eq!(
            (quota.current_usage, quota.usage_limit),
            (1_234.0, 10_000.0)
        );
        assert_eq!(snapshot.overview.normal, 1);
        assert_eq!(snapshot.overview.quota_exhausted, 0);
    }

    #[test]
    fn hidden_free_accounts_leave_no_trace_in_items_or_counts() {
        let mut busy_free = free_credential(3, 10.0);
        busy_free.disabled = false;
        busy_free.disabled_reason = None;
        busy_free.in_flight_requests = 2;
        busy_free.failure_count = 1;
        // Not queried yet: tier unknown, so it shows.
        let mut unqueried = credential_item(4, false, None, 0, 0.0, None);
        unqueried.subscription_title = None;
        let credentials = vec![
            credential_item(1, false, None, 0, 0.0, Some(30.0)),
            free_credential(2, 99.0),
            busy_free,
            unqueried,
        ];
        let snapshot = snapshot_with(credentials, PublicPoolFreeAccounts::Hide);
        let overview = &snapshot.overview;
        assert_eq!(
            (overview.total, overview.enabled, overview.disabled),
            (2, 2, 0)
        );
        assert_eq!(
            (overview.schedulable, overview.in_use, overview.failing),
            (2, 0, 0)
        );
        assert_eq!(overview.global_in_flight_requests, 0);
        assert_eq!(overview.quota_exhausted, 0);
        assert_eq!(overview.normal, 2);
        assert_eq!(overview.consumed_credits, 0.0);

        let response = snapshot.page(&query(None, None, None));
        assert_eq!(
            response
                .items
                .iter()
                .map(|item| item.id)
                .collect::<Vec<_>>(),
            vec![1, 4]
        );
        assert_eq!(response.filtered_total, 2);
        let disabled = snapshot.page(&query(None, None, Some("disabled")));
        assert_eq!(disabled.filtered_total, 0);
    }

    #[test]
    fn the_free_account_setting_defaults_to_power_and_parses_every_choice() {
        let update: PublicPoolConfigUpdate = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert!(update.enabled && update.free_accounts.is_none());
        for (raw, mode) in [
            ("power", PublicPoolFreeAccounts::Power),
            ("pro_max", PublicPoolFreeAccounts::ProMax),
            ("pro_plus", PublicPoolFreeAccounts::ProPlus),
            ("hide", PublicPoolFreeAccounts::Hide),
        ] {
            let update: PublicPoolConfigUpdate =
                serde_json::from_str(&format!(r#"{{"enabled":false,"freeAccounts":"{raw}"}}"#))
                    .unwrap();
            assert_eq!(update.free_accounts, Some(mode));
        }
        assert!(
            serde_json::from_str::<PublicPoolConfigUpdate>(
                r#"{"enabled":true,"freeAccounts":"free"}"#
            )
            .is_err()
        );
        assert_eq!(
            PublicPoolFreeAccounts::default(),
            PublicPoolFreeAccounts::Power
        );
    }
}
