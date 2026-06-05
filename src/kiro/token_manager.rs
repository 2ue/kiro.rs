//! Token 管理模块
//!
//! 负责 Token 过期检测和刷新，支持 Social 和 IdC 认证方式
//! 支持多凭据 (MultiTokenManager) 管理

use anyhow::bail;
use chrono::{DateTime, Duration, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex as TokioMutex, Notify};

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration as StdDuration, Instant};

use crate::http_client::{ProxyConfig, build_client};
use crate::kiro::machine_id;
use crate::kiro::model::credentials::KiroCredentials;
use crate::kiro::model::token_refresh::{
    IdcRefreshRequest, IdcRefreshResponse, RefreshRequest, RefreshResponse,
};
use crate::kiro::model::usage_limits::UsageLimitsResponse;
use crate::model::config::Config;
use crate::storage::postgres::{
    CredentialRuntimeStateRow, CredentialStatsRow, PostgresStore, ProxyResourceRow,
};
use crate::storage::redis_cache::{
    RedisStore, SchedulerCredentialState, SchedulerGlobalCapacityState, SchedulerHealthState,
    SchedulerSessionBinding,
};

/// 检查 Token 是否在指定时间内过期
pub(crate) fn is_token_expiring_within(
    credentials: &KiroCredentials,
    minutes: i64,
) -> Option<bool> {
    credentials
        .expires_at
        .as_ref()
        .and_then(|expires_at| DateTime::parse_from_rfc3339(expires_at).ok())
        .map(|expires| expires <= Utc::now() + Duration::minutes(minutes))
}

/// 检查 Token 是否已过期（提前 5 分钟判断）
pub(crate) fn is_token_expired(credentials: &KiroCredentials) -> bool {
    is_token_expiring_within(credentials, 5).unwrap_or(true)
}

/// 检查 Token 是否即将过期（10分钟内）
pub(crate) fn is_token_expiring_soon(credentials: &KiroCredentials) -> bool {
    is_token_expiring_within(credentials, 10).unwrap_or(false)
}

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

/// 验证 refreshToken 的基本有效性
pub(crate) fn validate_refresh_token(credentials: &KiroCredentials) -> anyhow::Result<()> {
    let refresh_token = credentials
        .refresh_token
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("缺少 refreshToken"))?;

    if refresh_token.is_empty() {
        bail!("refreshToken 为空");
    }

    if refresh_token.len() < 100 || refresh_token.ends_with("...") || refresh_token.contains("...")
    {
        bail!(
            "refreshToken 已被截断（长度: {} 字符）。\n\
             这通常是 Kiro IDE 为了防止凭证被第三方工具使用而故意截断的。",
            refresh_token.len()
        );
    }

    Ok(())
}

#[derive(Debug, Clone)]
struct ProxyResourceRuntime {
    id: u64,
    name: String,
    proxy_url: String,
    proxy_username: Option<String>,
    proxy_password: Option<String>,
    enabled: bool,
}

#[derive(Debug, Clone)]
enum ProxyResourceAvailability {
    Available(ProxyResourceRuntime),
    Missing(u64),
    Disabled(ProxyResourceRuntime),
}

impl From<ProxyResourceRow> for ProxyResourceRuntime {
    fn from(row: ProxyResourceRow) -> Self {
        Self {
            id: row.id,
            name: row.name,
            proxy_url: row.proxy_url,
            proxy_username: row.proxy_username,
            proxy_password: row.proxy_password,
            enabled: row.enabled,
        }
    }
}

/// Refresh Token 永久失效错误
///
/// 当服务端返回 400 + `invalid_grant` 时，表示 refreshToken 已被撤销或过期，
/// 不应重试，需立即禁用对应凭据。
#[derive(Debug)]
pub(crate) struct RefreshTokenInvalidError {
    pub message: String,
}

impl fmt::Display for RefreshTokenInvalidError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for RefreshTokenInvalidError {}

/// 刷新 Token
pub(crate) async fn refresh_token(
    credentials: &KiroCredentials,
    config: &Config,
    proxy: Option<&ProxyConfig>,
) -> anyhow::Result<KiroCredentials> {
    // API Key 凭据不支持 Token 刷新：底层契约级拦截
    // 其他调用点（try_ensure_token / 活跃路径 / add_credential）在调用前已显式分流 API Key；
    // 仅 force_refresh_token_for 未分流，此处 bail 让错误自然传播为 400 BAD_REQUEST。
    if credentials.is_api_key_credential() {
        bail!("API Key 凭据不支持刷新 Token");
    }

    validate_refresh_token(credentials)?;

    // 根据 auth_method 选择刷新方式
    // 如果未指定 auth_method，根据是否有 clientId/clientSecret 自动判断
    let auth_method = credentials.auth_method.as_deref().unwrap_or_else(|| {
        if credentials.client_id.is_some() && credentials.client_secret.is_some() {
            "idc"
        } else {
            "social"
        }
    });

    if auth_method.eq_ignore_ascii_case("idc")
        || auth_method.eq_ignore_ascii_case("builder-id")
        || auth_method.eq_ignore_ascii_case("iam")
    {
        refresh_idc_token(credentials, config, proxy).await
    } else {
        refresh_social_token(credentials, config, proxy).await
    }
}

/// 刷新 Social Token
async fn refresh_social_token(
    credentials: &KiroCredentials,
    config: &Config,
    proxy: Option<&ProxyConfig>,
) -> anyhow::Result<KiroCredentials> {
    tracing::info!("正在刷新 Social Token...");

    let refresh_token = credentials.refresh_token.as_ref().unwrap();
    // 优先级：凭据.auth_region > 凭据.region > config.auth_region > config.region
    let region = credentials.effective_auth_region(config);

    let refresh_url = format!("https://prod.{}.auth.desktop.kiro.dev/refreshToken", region);
    let refresh_domain = format!("prod.{}.auth.desktop.kiro.dev", region);
    let machine_id = machine_id::generate_from_credentials(credentials, config);
    let kiro_version = &config.kiro_version;

    let client = build_client(proxy, 60, config.tls_backend)?;
    let body = RefreshRequest {
        refresh_token: refresh_token.to_string(),
    };

    let response = client
        .post(&refresh_url)
        .header("Accept", "application/json, text/plain, */*")
        .header("Content-Type", "application/json")
        .header(
            "User-Agent",
            format!("KiroIDE-{}-{}", kiro_version, machine_id),
        )
        .header("Accept-Encoding", "gzip, compress, deflate, br")
        .header("host", &refresh_domain)
        .header("Connection", "close")
        .json(&body)
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        let body_text = response.text().await.unwrap_or_default();

        // 400 + invalid_grant + Invalid refresh token provided → refreshToken 永久失效
        if status.as_u16() == 400
            && body_text.contains("\"invalid_grant\"")
            && body_text.contains("Invalid refresh token provided")
        {
            return Err(RefreshTokenInvalidError {
                message: format!("Social refreshToken 已失效 (invalid_grant): {}", body_text),
            }
            .into());
        }

        let error_msg = match status.as_u16() {
            401 => "OAuth 凭证已过期或无效，需要重新认证",
            403 => "权限不足，无法刷新 Token",
            429 => "请求过于频繁，已被限流",
            500..=599 => "服务器错误，AWS OAuth 服务暂时不可用",
            _ => "Token 刷新失败",
        };
        bail!("{}: {} {}", error_msg, status, body_text);
    }

    let data: RefreshResponse = response.json().await?;

    let mut new_credentials = credentials.clone();
    new_credentials.access_token = Some(data.access_token);

    if let Some(new_refresh_token) = data.refresh_token {
        new_credentials.refresh_token = Some(new_refresh_token);
    }

    if let Some(profile_arn) = data.profile_arn {
        new_credentials.profile_arn = Some(profile_arn);
    }

    if let Some(expires_in) = data.expires_in {
        let expires_at = Utc::now() + Duration::seconds(expires_in);
        new_credentials.expires_at = Some(expires_at.to_rfc3339());
    }

    Ok(new_credentials)
}

/// 刷新 IdC Token (AWS SSO OIDC)
async fn refresh_idc_token(
    credentials: &KiroCredentials,
    config: &Config,
    proxy: Option<&ProxyConfig>,
) -> anyhow::Result<KiroCredentials> {
    tracing::info!("正在刷新 IdC Token...");

    let refresh_token = credentials.refresh_token.as_ref().unwrap();
    let client_id = credentials
        .client_id
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("IdC 刷新需要 clientId"))?;
    let client_secret = credentials
        .client_secret
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("IdC 刷新需要 clientSecret"))?;

    // 优先级：凭据.auth_region > 凭据.region > config.auth_region > config.region
    let region = credentials.effective_auth_region(config);
    let refresh_url = format!("https://oidc.{}.amazonaws.com/token", region);
    let os_name = &config.system_version;
    let node_version = &config.node_version;

    let x_amz_user_agent = "aws-sdk-js/3.980.0 KiroIDE";
    let user_agent = format!(
        "aws-sdk-js/3.980.0 ua/2.1 os/{} lang/js md/nodejs#{} api/sso-oidc#3.980.0 m/E KiroIDE",
        os_name, node_version
    );

    let client = build_client(proxy, 60, config.tls_backend)?;
    let body = IdcRefreshRequest {
        client_id: client_id.to_string(),
        client_secret: client_secret.to_string(),
        refresh_token: refresh_token.to_string(),
        grant_type: "refresh_token".to_string(),
    };

    let response = client
        .post(&refresh_url)
        .header("content-type", "application/json")
        .header("x-amz-user-agent", x_amz_user_agent)
        .header("user-agent", &user_agent)
        .header("host", format!("oidc.{}.amazonaws.com", region))
        .header("amz-sdk-invocation-id", uuid::Uuid::new_v4().to_string())
        .header("amz-sdk-request", "attempt=1; max=4")
        .header("Connection", "close")
        .json(&body)
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        let body_text = response.text().await.unwrap_or_default();

        // 400 + invalid_grant + Invalid refresh token provided → refreshToken 永久失效
        if status.as_u16() == 400
            && body_text.contains("\"invalid_grant\"")
            && body_text.contains("Invalid refresh token provided")
        {
            return Err(RefreshTokenInvalidError {
                message: format!("IdC refreshToken 已失效 (invalid_grant): {}", body_text),
            }
            .into());
        }

        let error_msg = match status.as_u16() {
            401 => "IdC 凭证已过期或无效，需要重新认证",
            403 => "权限不足，无法刷新 Token",
            429 => "请求过于频繁，已被限流",
            500..=599 => "服务器错误，AWS OIDC 服务暂时不可用",
            _ => "IdC Token 刷新失败",
        };
        bail!("{}: {} {}", error_msg, status, body_text);
    }

    let data: IdcRefreshResponse = response.json().await?;

    let mut new_credentials = credentials.clone();
    new_credentials.access_token = Some(data.access_token);

    if let Some(new_refresh_token) = data.refresh_token {
        new_credentials.refresh_token = Some(new_refresh_token);
    }

    if let Some(expires_in) = data.expires_in {
        let expires_at = Utc::now() + Duration::seconds(expires_in);
        new_credentials.expires_at = Some(expires_at.to_rfc3339());
    }

    // 同步更新 profile_arn（如果 IdC 响应中包含）
    if let Some(profile_arn) = data.profile_arn {
        new_credentials.profile_arn = Some(profile_arn);
    }

    Ok(new_credentials)
}

/// 获取使用额度信息
pub(crate) async fn get_usage_limits(
    credentials: &KiroCredentials,
    config: &Config,
    token: &str,
    proxy: Option<&ProxyConfig>,
) -> anyhow::Result<UsageLimitsResponse> {
    tracing::debug!("正在获取使用额度信息...");

    // 优先级：凭据.api_region > config.api_region > config.region
    let region = credentials.effective_api_region(config);
    let host = format!("q.{}.amazonaws.com", region);
    let machine_id = machine_id::generate_from_credentials(credentials, config);
    let kiro_version = &config.kiro_version;
    let os_name = &config.system_version;
    let node_version = &config.node_version;

    // 构建 URL
    let mut url = format!(
        "https://{}/getUsageLimits?origin=AI_EDITOR&resourceType=AGENTIC_REQUEST",
        host
    );

    // profileArn 是可选的
    if let Some(profile_arn) = &credentials.profile_arn {
        url.push_str(&format!("&profileArn={}", urlencoding::encode(profile_arn)));
    }

    // 构建 User-Agent headers
    let user_agent = format!(
        "aws-sdk-js/1.0.0 ua/2.1 os/{} lang/js md/nodejs#{} api/codewhispererruntime#1.0.0 m/N,E KiroIDE-{}-{}",
        os_name, node_version, kiro_version, machine_id
    );
    let amz_user_agent = format!("aws-sdk-js/1.0.0 KiroIDE-{}-{}", kiro_version, machine_id);

    let client = build_client(proxy, 60, config.tls_backend)?;

    let mut request = client
        .get(&url)
        .header("x-amz-user-agent", &amz_user_agent)
        .header("user-agent", &user_agent)
        .header("host", &host)
        .header("amz-sdk-invocation-id", uuid::Uuid::new_v4().to_string())
        .header("amz-sdk-request", "attempt=1; max=1")
        .header("Authorization", format!("Bearer {}", token))
        .header("Connection", "close");

    if credentials.is_api_key_credential() {
        request = request.header("tokentype", "API_KEY");
    }

    let response = request.send().await?;

    let status = response.status();
    if !status.is_success() {
        let body_text = response.text().await.unwrap_or_default();
        let error_msg = match status.as_u16() {
            401 => "认证失败，Token 无效或已过期",
            403 => "权限不足，无法获取使用额度",
            429 => "请求过于频繁，已被限流",
            500..=599 => "服务器错误，AWS 服务暂时不可用",
            _ => "获取使用额度失败",
        };
        bail!("{}: {} {}", error_msg, status, body_text);
    }

    let data: UsageLimitsResponse = response.json().await?;
    Ok(data)
}

// ============================================================================
// 多凭据 Token 管理器
// ============================================================================

/// 单个凭据条目的状态
struct CredentialEntry {
    /// 凭据唯一 ID
    id: u64,
    /// 凭据信息
    credentials: KiroCredentials,
    /// API 调用连续失败次数
    failure_count: u32,
    /// Token 刷新连续失败次数
    refresh_failure_count: u32,
    /// 是否已禁用
    disabled: bool,
    /// 禁用原因（用于区分手动禁用 vs 自动禁用，便于自愈）
    disabled_reason: Option<DisabledReason>,
    /// API 调用成功次数
    success_count: u64,
    /// 调度器实际选中该凭据的总次数。
    total_selection_count: u64,
    /// 最后一次 API 调用时间（RFC3339 格式）
    last_used_at: Option<String>,
    /// 临时冷却到期时间（上游 Retry-After/瞬态错误触发），不持久化。
    cooldown_until: Option<Instant>,
    /// 临时冷却原因，便于诊断。
    cooldown_reason: Option<String>,
    /// 下一次本地限流允许发送请求的时间，不持久化。
    rate_limit_available_at: Option<Instant>,
    /// 当前正在使用该凭据的请求数，不持久化。
    in_flight_requests: u32,
    /// 当前正在使用该凭据的请求 lease，不持久化。
    in_flight_leases: Vec<InFlightLease>,
    /// 预热剩余请求数。仅影响 balanced 选择，不伪造 success_count。
    warmup_remaining: u32,
    /// 近期上游健康状态；Redis 部署下在调度前同步。
    health: SchedulerHealthState,
    /// 本进程内的近期调度选中事件；无 Redis 时用于计算短窗口调度压力。
    selection_events: VecDeque<Instant>,
}

#[derive(Debug, Clone)]
struct InFlightLease {
    id: u64,
    acquired_at: Instant,
    last_seen_at: Instant,
    kind: InFlightKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InFlightKind {
    Api,
    Stream,
    Mcp,
    Test,
}

impl InFlightKind {
    fn as_str(self) -> &'static str {
        match self {
            InFlightKind::Api => "api",
            InFlightKind::Stream => "stream",
            InFlightKind::Mcp => "mcp",
            InFlightKind::Test => "test",
        }
    }

    fn from_str(value: &str) -> Self {
        match value {
            "stream" => InFlightKind::Stream,
            "mcp" => InFlightKind::Mcp,
            "test" => InFlightKind::Test,
            _ => InFlightKind::Api,
        }
    }
}

/// 会话到凭据的粘性绑定。
struct SessionBinding {
    credential_id: u64,
    last_used_at: DateTime<Utc>,
    soft_failure_count: u32,
}

/// 禁用原因
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DisabledReason {
    /// Admin API 手动禁用
    Manual,
    /// 连续失败达到阈值后自动禁用
    TooManyFailures,
    /// Token 刷新连续失败达到阈值后自动禁用
    TooManyRefreshFailures,
    /// 额度已用尽（如 MONTHLY_REQUEST_COUNT / OVERAGE_REQUEST_LIMIT_EXCEEDED）
    QuotaExceeded,
    /// Refresh Token 永久失效（服务端返回 invalid_grant）
    InvalidRefreshToken,
    /// 凭据配置无效（如 authMethod=api_key 但缺少 kiroApiKey）
    InvalidConfig,
    /// 上游明确返回临时风控/暂停
    TemporarilySuspended,
    /// 上游明确返回账号已暂停/封禁
    AccountSuspended,
    /// 上游明确返回账号锁定
    AccountLocked,
}

impl DisabledReason {
    fn as_str(self) -> &'static str {
        match self {
            DisabledReason::Manual => "Manual",
            DisabledReason::TooManyFailures => "TooManyFailures",
            DisabledReason::TooManyRefreshFailures => "TooManyRefreshFailures",
            DisabledReason::QuotaExceeded => "QuotaExceeded",
            DisabledReason::InvalidRefreshToken => "InvalidRefreshToken",
            DisabledReason::InvalidConfig => "InvalidConfig",
            DisabledReason::TemporarilySuspended => "TemporarilySuspended",
            DisabledReason::AccountSuspended => "AccountSuspended",
            DisabledReason::AccountLocked => "AccountLocked",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "Manual" => Some(DisabledReason::Manual),
            "TooManyFailures" => Some(DisabledReason::TooManyFailures),
            "TooManyRefreshFailures" => Some(DisabledReason::TooManyRefreshFailures),
            "QuotaExceeded" => Some(DisabledReason::QuotaExceeded),
            "InvalidRefreshToken" => Some(DisabledReason::InvalidRefreshToken),
            "InvalidConfig" => Some(DisabledReason::InvalidConfig),
            "TemporarilySuspended" => Some(DisabledReason::TemporarilySuspended),
            "AccountSuspended" => Some(DisabledReason::AccountSuspended),
            "AccountLocked" => Some(DisabledReason::AccountLocked),
            _ => None,
        }
    }
}

/// 上游明确返回的账号风控/暂停状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialRiskControlReason {
    TemporarilySuspended,
    AccountSuspended,
    AccountLocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransientFailureKind {
    RateLimit,
    Server,
    Network,
    Stream,
    Protocol,
    Auth,
}

impl TransientFailureKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::RateLimit => "rate_limit",
            Self::Server => "server",
            Self::Network => "network",
            Self::Stream => "stream",
            Self::Protocol => "protocol",
            Self::Auth => "auth",
        }
    }
}

impl CredentialRiskControlReason {
    fn disabled_reason(self) -> DisabledReason {
        match self {
            CredentialRiskControlReason::TemporarilySuspended => {
                DisabledReason::TemporarilySuspended
            }
            CredentialRiskControlReason::AccountSuspended => DisabledReason::AccountSuspended,
            CredentialRiskControlReason::AccountLocked => DisabledReason::AccountLocked,
        }
    }

    fn event_reason(self) -> &'static str {
        self.disabled_reason().as_str()
    }

    fn label(self) -> &'static str {
        match self {
            CredentialRiskControlReason::TemporarilySuspended => "临时风控/暂停",
            CredentialRiskControlReason::AccountSuspended => "账号暂停/封禁",
            CredentialRiskControlReason::AccountLocked => "账号锁定",
        }
    }
}

/// 统计数据持久化条目
#[derive(Serialize, Deserialize)]
struct StatsEntry {
    success_count: u64,
    #[serde(default)]
    selection_count: u64,
    last_used_at: Option<String>,
}

fn block_on_storage<T>(
    operation: &'static str,
    future: impl Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(|| handle.block_on(future))
    } else {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(future)
    }
    .map_err(|err| anyhow::anyhow!("{}失败: {}", operation, err))
}

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

// ============================================================================
// Admin API 公开结构
// ============================================================================

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
    /// Token 刷新锁，确保同一时间只有一个刷新操作
    refresh_lock: TokioMutex<()>,
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
    /// 统计数据是否有未落盘更新
    stats_dirty: AtomicBool,
    /// 会话粘性绑定：conversationId -> credential id
    session_bindings: Mutex<HashMap<String, SessionBinding>>,
    /// 凭据并发槽释放通知，用于所有凭据占满时排队等待。
    in_flight_notify: Arc<Notify>,
    next_in_flight_lease_id: AtomicU64,
    queued_requests: Arc<AtomicU32>,
}

/// 每个凭据最大 API 调用失败次数
const MAX_FAILURES_PER_CREDENTIAL: u32 = 3;
/// 会话绑定最长保留时间，避免长期运行进程无限增长。
const SESSION_BINDING_TTL_SECS: i64 = 6 * 60 * 60;
/// 会话绑定表上限。
const MAX_SESSION_BINDINGS: usize = 10_000;
/// 同一会话绑定账号连续软失败达到该阈值后，本次请求允许临时 fallback。
const MAX_SESSION_SOFT_FAILURES: u32 = 2;
/// 并发排队等待的周期性唤醒间隔，避免极端竞态下丢失通知后永久睡眠。
const CONCURRENCY_WAIT_WAKEUP_SECS: u64 = 30;
const SELECTION_WINDOW_10S: StdDuration = StdDuration::from_secs(10);
const SELECTION_WINDOW_60S: StdDuration = StdDuration::from_secs(60);
const SELECTION_WINDOW_5M: StdDuration = StdDuration::from_secs(5 * 60);

/// API 调用上下文
///
/// 绑定特定凭据的调用上下文，确保 token、credentials 和 id 的一致性
/// 用于解决并发调用时 current_id 竞态问题
pub struct CallContext {
    /// 凭据 ID（用于 report_success/report_failure）
    pub id: u64,
    /// 凭据信息（用于构建请求头）
    pub credentials: KiroCredentials,
    /// 访问 Token
    pub token: String,
    /// 本次请求是否实际命中了已有会话绑定。
    pub sticky_bound: bool,
    /// 本次请求是否从已有会话绑定临时 fallback 到其他凭据。
    pub fallback_from_sticky: bool,
    /// 本次调度占用的并发 lease；Admin 手动测试等未跟踪调用为 None。
    in_flight_lease: Option<InFlightLeaseGuard>,
}

impl CallContext {
    #[cfg(test)]
    pub(crate) fn in_flight_lease_id(&self) -> Option<u64> {
        self.in_flight_lease.as_ref().map(InFlightLeaseGuard::id)
    }

    pub fn release_in_flight(&mut self) {
        self.in_flight_lease = None;
    }

    pub fn take_in_flight_lease(&mut self) -> Option<InFlightLeaseGuard> {
        self.in_flight_lease.take()
    }

    pub fn mark_in_flight_kind(&self, kind: InFlightKind) {
        if let Some(lease) = &self.in_flight_lease {
            lease.set_kind(kind);
        }
    }
}

pub struct InFlightLeaseGuard {
    entries: Arc<Mutex<Vec<CredentialEntry>>>,
    redis_store: Option<Arc<RedisStore>>,
    in_flight_notify: Arc<Notify>,
    credential_id: u64,
    lease_id: u64,
    released: AtomicBool,
}

impl fmt::Debug for InFlightLeaseGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InFlightLeaseGuard")
            .field("credential_id", &self.credential_id)
            .field("lease_id", &self.lease_id)
            .finish_non_exhaustive()
    }
}

impl InFlightLeaseGuard {
    fn new(
        entries: Arc<Mutex<Vec<CredentialEntry>>>,
        redis_store: Option<Arc<RedisStore>>,
        in_flight_notify: Arc<Notify>,
        credential_id: u64,
        lease_id: u64,
    ) -> Self {
        Self {
            entries,
            redis_store,
            in_flight_notify,
            credential_id,
            lease_id,
            released: AtomicBool::new(false),
        }
    }

    #[cfg(test)]
    pub(crate) fn id(&self) -> u64 {
        self.lease_id
    }

    pub fn release(&self) {
        if self.released.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut released_in_redis = false;
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            let credential_id = self.credential_id;
            let lease_id = self.lease_id;
            match block_on_storage("释放 Redis 并发 lease", async move {
                let released = redis
                    .release_in_flight_lease(credential_id, lease_id)
                    .await?;
                if released {
                    let payload = serde_json::json!({
                        "kind": "dispatch_wakeup",
                        "credentialId": credential_id,
                        "leaseId": lease_id,
                        "changedAt": Utc::now().to_rfc3339(),
                    })
                    .to_string();
                    redis.publish_dispatch_wakeup(payload).await?;
                }
                Ok(released)
            }) {
                Ok(released) => released_in_redis = released,
                Err(err) => tracing::warn!(
                    credential_id,
                    lease_id,
                    "释放 Redis 并发 lease 失败: {}",
                    err
                ),
            }
        }
        if release_in_flight_lease_from_entries(&self.entries, self.credential_id, self.lease_id) {
            self.in_flight_notify.notify_waiters();
        } else if released_in_redis {
            self.in_flight_notify.notify_waiters();
        }
    }

    pub fn touch(&self) {
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            let credential_id = self.credential_id;
            let lease_id = self.lease_id;
            if let Err(err) = block_on_storage("更新 Redis 并发 lease 活跃时间", async move {
                redis.touch_in_flight_lease(credential_id, lease_id).await
            }) {
                tracing::warn!(
                    credential_id,
                    lease_id,
                    "更新 Redis 并发 lease 活跃时间失败: {}",
                    err
                );
            }
        }
        touch_in_flight_lease_from_entries(&self.entries, self.credential_id, self.lease_id);
    }

    pub fn set_kind(&self, kind: InFlightKind) {
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            let credential_id = self.credential_id;
            let lease_id = self.lease_id;
            let kind_name = kind.as_str().to_string();
            if let Err(err) = block_on_storage("更新 Redis 并发 lease 类型", async move {
                redis
                    .set_in_flight_lease_kind(credential_id, lease_id, &kind_name)
                    .await
            }) {
                tracing::warn!(
                    credential_id,
                    lease_id,
                    "更新 Redis 并发 lease 类型失败: {}",
                    err
                );
            }
        }
        set_in_flight_lease_kind_from_entries(
            &self.entries,
            self.credential_id,
            self.lease_id,
            kind,
        );
    }
}

impl Drop for InFlightLeaseGuard {
    fn drop(&mut self) {
        self.release();
    }
}

struct DispatchQueueGuard {
    redis_store: Option<Arc<RedisStore>>,
    local_queued: Arc<AtomicU32>,
    in_flight_notify: Arc<Notify>,
    released: AtomicBool,
}

impl DispatchQueueGuard {
    fn new(
        redis_store: Option<Arc<RedisStore>>,
        local_queued: Arc<AtomicU32>,
        in_flight_notify: Arc<Notify>,
    ) -> Self {
        Self {
            redis_store,
            local_queued,
            in_flight_notify,
            released: AtomicBool::new(false),
        }
    }

    fn release(&self) {
        if self.released.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            if let Err(err) = block_on_storage("释放 Redis 调度排队占位", async move {
                redis.leave_dispatch_queue().await
            }) {
                tracing::warn!("释放 Redis 调度排队占位失败: {}", err);
            }
        } else {
            self.local_queued.fetch_sub(1, Ordering::AcqRel);
        }
        self.in_flight_notify.notify_waiters();
    }
}

impl Drop for DispatchQueueGuard {
    fn drop(&mut self) {
        self.release();
    }
}

fn release_in_flight_lease_from_entries(
    entries: &Mutex<Vec<CredentialEntry>>,
    credential_id: u64,
    lease_id: u64,
) -> bool {
    let mut entries = entries.lock();
    let Some(entry) = entries.iter_mut().find(|e| e.id == credential_id) else {
        return false;
    };

    if let Some(index) = entry
        .in_flight_leases
        .iter()
        .position(|lease| lease.id == lease_id)
    {
        entry.in_flight_leases.remove(index);
        if entry.in_flight_requests > 0 {
            entry.in_flight_requests -= 1;
        } else {
            tracing::debug!(
                "凭据 #{} 并发 lease #{} 释放时计数已为空",
                credential_id,
                lease_id
            );
        }
        true
    } else {
        false
    }
}

fn touch_in_flight_lease_from_entries(
    entries: &Mutex<Vec<CredentialEntry>>,
    credential_id: u64,
    lease_id: u64,
) {
    let mut entries = entries.lock();
    if let Some(entry) = entries.iter_mut().find(|e| e.id == credential_id) {
        if let Some(lease) = entry
            .in_flight_leases
            .iter_mut()
            .find(|lease| lease.id == lease_id)
        {
            lease.last_seen_at = Instant::now();
        }
    }
}

fn set_in_flight_lease_kind_from_entries(
    entries: &Mutex<Vec<CredentialEntry>>,
    credential_id: u64,
    lease_id: u64,
    kind: InFlightKind,
) {
    let mut entries = entries.lock();
    if let Some(entry) = entries.iter_mut().find(|e| e.id == credential_id) {
        if let Some(lease) = entry
            .in_flight_leases
            .iter_mut()
            .find(|lease| lease.id == lease_id)
        {
            lease.kind = kind;
            lease.last_seen_at = Instant::now();
        }
    }
}

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
                    rate_limit_available_at: None,
                    in_flight_requests: 0,
                    in_flight_leases: Vec::new(),
                    warmup_remaining: 0,
                    health: SchedulerHealthState::default(),
                    selection_events: VecDeque::new(),
                }
            })
            .collect();

        // 校验 API Key 凭据配置完整性：authMethod=api_key 时必须提供 kiroApiKey
        let mut entries = entries;
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
            refresh_lock: TokioMutex::new(()),
            postgres_store,
            redis_store,
            proxy_resources: Arc::new(Mutex::new(proxy_resources)),
            load_balancing_mode: Mutex::new(load_balancing_mode),
            last_stats_save_at: Mutex::new(None),
            stats_dirty: AtomicBool::new(false),
            session_bindings: Mutex::new(HashMap::new()),
            in_flight_notify: Arc::new(Notify::new()),
            next_in_flight_lease_id: AtomicU64::new(1),
            queued_requests: Arc::new(AtomicU32::new(0)),
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
        manager.refresh_scheduler_state_from_redis_best_effort();

        Ok(manager)
    }

    /// 获取当前运行时配置快照。
    pub fn runtime_config(&self) -> Config {
        self.config.lock().clone()
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

    fn is_opus_model(model: Option<&str>) -> bool {
        model
            .map(|m| m.to_lowercase().contains("opus"))
            .unwrap_or(false)
    }

    fn credential_is_usable_for_model(entry: &CredentialEntry, model: Option<&str>) -> bool {
        if entry.disabled {
            return false;
        }
        if Self::is_opus_model(model) && !entry.credentials.supports_opus() {
            return false;
        }
        true
    }

    fn entry_cooldown_remaining(entry: &CredentialEntry, now: Instant) -> Option<StdDuration> {
        entry
            .cooldown_until
            .and_then(|until| until.checked_duration_since(now))
    }

    fn entry_rate_limit_remaining(entry: &CredentialEntry, now: Instant) -> Option<StdDuration> {
        entry
            .rate_limit_available_at
            .and_then(|until| until.checked_duration_since(now))
    }

    fn credential_is_dispatchable(
        proxy_resources: &HashMap<u64, ProxyResourceRuntime>,
        entry: &CredentialEntry,
        model: Option<&str>,
        now: Instant,
        max_concurrent_requests: u32,
    ) -> bool {
        Self::credential_is_usable_for_model(entry, model)
            && Self::credential_proxy_is_dispatchable(&entry.credentials, proxy_resources)
            && Self::entry_cooldown_remaining(entry, now).is_none()
            && Self::entry_rate_limit_remaining(entry, now).is_none()
            && Self::entry_has_concurrency_capacity(entry, max_concurrent_requests)
    }

    fn credential_is_temporarily_available(
        proxy_resources: &HashMap<u64, ProxyResourceRuntime>,
        entry: &CredentialEntry,
        model: Option<&str>,
        now: Instant,
    ) -> bool {
        Self::credential_is_usable_for_model(entry, model)
            && Self::credential_proxy_is_dispatchable(&entry.credentials, proxy_resources)
            && Self::entry_cooldown_remaining(entry, now).is_none()
            && Self::entry_rate_limit_remaining(entry, now).is_none()
    }

    fn credential_is_dispatch_candidate(
        proxy_resources: &HashMap<u64, ProxyResourceRuntime>,
        entry: &CredentialEntry,
        model: Option<&str>,
        excluded_ids: &HashSet<u64>,
    ) -> bool {
        !excluded_ids.contains(&entry.id)
            && Self::credential_is_usable_for_model(entry, model)
            && Self::credential_proxy_is_dispatchable(&entry.credentials, proxy_resources)
    }

    fn credential_proxy_availability(
        credentials: &KiroCredentials,
        proxy_resources: &HashMap<u64, ProxyResourceRuntime>,
    ) -> Option<ProxyResourceAvailability> {
        if credentials.proxy_url.is_some() {
            return None;
        }
        let resource_id = credentials.proxy_resource_id?;
        let Some(resource) = proxy_resources.get(&resource_id) else {
            return Some(ProxyResourceAvailability::Missing(resource_id));
        };
        if !resource.enabled {
            return Some(ProxyResourceAvailability::Disabled(resource.clone()));
        }
        Some(ProxyResourceAvailability::Available(resource.clone()))
    }

    fn credential_proxy_is_dispatchable(
        credentials: &KiroCredentials,
        proxy_resources: &HashMap<u64, ProxyResourceRuntime>,
    ) -> bool {
        match Self::credential_proxy_availability(credentials, proxy_resources) {
            Some(ProxyResourceAvailability::Missing(_))
            | Some(ProxyResourceAvailability::Disabled(_)) => false,
            Some(ProxyResourceAvailability::Available(_)) | None => true,
        }
    }

    fn proxy_unavailable_error(
        credential_id: Option<u64>,
        availability: ProxyResourceAvailability,
    ) -> anyhow::Error {
        match availability {
            ProxyResourceAvailability::Missing(resource_id) => anyhow::anyhow!(
                "凭据 #{} 绑定的代理资源 #{} 不存在，已阻止回退到全局代理/直连",
                credential_id.unwrap_or_default(),
                resource_id
            ),
            ProxyResourceAvailability::Disabled(resource) => anyhow::anyhow!(
                "凭据 #{} 绑定的代理资源「{}」已禁用，已阻止回退到全局代理/直连",
                credential_id.unwrap_or_default(),
                resource.name
            ),
            ProxyResourceAvailability::Available(_) => {
                anyhow::anyhow!("代理资源可用状态异常")
            }
        }
    }

    fn effective_max_concurrent_requests(
        entry: &CredentialEntry,
        global_max_concurrent_requests: u32,
    ) -> u32 {
        entry
            .credentials
            .max_concurrent_requests
            .unwrap_or(global_max_concurrent_requests)
    }

    fn entry_has_concurrency_capacity(
        entry: &CredentialEntry,
        global_max_concurrent_requests: u32,
    ) -> bool {
        let max_concurrent_requests =
            Self::effective_max_concurrent_requests(entry, global_max_concurrent_requests);
        max_concurrent_requests == 0 || entry.in_flight_requests < max_concurrent_requests
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
            .map(|entry| {
                Self::effective_max_concurrent_requests(entry, global_max_concurrent_requests)
            })
            .unwrap_or(global_max_concurrent_requests)
    }

    fn global_max_concurrent_requests(&self) -> u32 {
        self.config.lock().dispatch_global_max_concurrent_requests
    }

    fn max_queued_requests(&self) -> u32 {
        self.config.lock().dispatch_max_queued_requests
    }

    fn in_flight_lease_max_age(&self) -> Option<StdDuration> {
        let secs = self.config.lock().credential_in_flight_lease_max_secs;
        (secs > 0).then(|| StdDuration::from_secs(secs))
    }

    fn rate_limit_interval(&self) -> Option<StdDuration> {
        let rpm = self.config.lock().credential_rpm.unwrap_or(0);
        if rpm == 0 {
            return None;
        }

        let millis = (60_000u64 / rpm as u64).max(1);
        Some(StdDuration::from_millis(millis))
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

    fn scheduler_score(
        &self,
        entry: &CredentialEntry,
        now_ms: i64,
        selection_pressure: f64,
    ) -> f64 {
        let config = self.config.lock();
        let max_concurrent = Self::effective_max_concurrent_requests(
            entry,
            config.credential_max_concurrent_requests,
        );
        let load = if max_concurrent > 0 {
            entry.in_flight_requests as f64 / max_concurrent as f64
        } else {
            entry.in_flight_requests as f64
        };
        let probation = entry
            .health
            .probation_until_ms
            .is_some_and(|until_ms| until_ms > now_ms) as u8 as f64;
        entry.credentials.priority as f64 * config.scheduler_priority_weight.max(0.0)
            + load * config.scheduler_load_weight.max(0.0)
            + entry.health.recent_error_rate.clamp(0.0, 1.0)
                * config.scheduler_error_weight.max(0.0)
            + entry.health.latency_ewma_ms.unwrap_or(0.0).max(0.0)
                * config.scheduler_latency_weight.max(0.0)
            + probation * config.scheduler_probation_weight.max(0.0)
            + selection_pressure.max(0.0) * config.scheduler_selection_pressure_weight.max(0.0)
            + (entry.total_selection_count as f64).ln_1p()
                * config.scheduler_total_selection_weight.max(0.0)
    }

    fn prune_local_selection_events(entry: &mut CredentialEntry, now: Instant) {
        while entry.selection_events.front().is_some_and(|selected_at| {
            now.saturating_duration_since(*selected_at) > SELECTION_WINDOW_5M
        }) {
            entry.selection_events.pop_front();
        }
        entry.health.recent_selection_count_10s = entry
            .selection_events
            .iter()
            .filter(|selected_at| {
                now.saturating_duration_since(**selected_at) <= SELECTION_WINDOW_10S
            })
            .count()
            .min(u32::MAX as usize) as u32;
        entry.health.recent_selection_count_60s = entry
            .selection_events
            .iter()
            .filter(|selected_at| {
                now.saturating_duration_since(**selected_at) <= SELECTION_WINDOW_60S
            })
            .count()
            .min(u32::MAX as usize) as u32;
        entry.health.recent_selection_count_5m =
            entry.selection_events.len().min(u32::MAX as usize) as u32;
    }

    fn record_local_selection(entry: &mut CredentialEntry, now: Instant) {
        entry.total_selection_count = entry.total_selection_count.saturating_add(1);
        entry.selection_events.push_back(now);
        Self::prune_local_selection_events(entry, now);
    }

    fn refresh_local_selection_windows_locked(entries: &mut [CredentialEntry], now: Instant) {
        for entry in entries {
            Self::prune_local_selection_events(entry, now);
        }
    }

    fn selection_pressure_for_candidates(
        entry: &CredentialEntry,
        candidates: &[&CredentialEntry],
    ) -> f64 {
        if candidates.len() <= 1 {
            return 0.0;
        }
        let total_recent: u64 = candidates
            .iter()
            .map(|candidate| candidate.health.recent_selection_count_60s as u64)
            .sum();
        if total_recent == 0 {
            return 0.0;
        }
        let share = entry.health.recent_selection_count_60s as f64 / total_recent as f64;
        let expected_share = 1.0 / candidates.len() as f64;
        (share / expected_share - 1.0).max(0.0)
    }

    fn warmup_target_share(&self, warming_count: usize) -> f64 {
        if warming_count == 0 {
            return 0.0;
        }
        let config = self.config.lock();
        let per_warming = config.credential_warmup_selection_percent.min(100) as f64 / 100.0;
        let max_share = config.credential_warmup_max_selection_percent.min(100) as f64 / 100.0;
        (per_warming * warming_count as f64).min(max_share).min(1.0)
    }

    fn should_select_warming(
        &self,
        ready: &[&CredentialEntry],
        warming: &[&CredentialEntry],
        available: &[&CredentialEntry],
    ) -> bool {
        if warming.is_empty() {
            return false;
        }
        if ready.is_empty() {
            return true;
        }
        let target_share = self.warmup_target_share(warming.len());
        if target_share <= 0.0 {
            return false;
        }
        let total_recent: u64 = available
            .iter()
            .map(|entry| entry.health.recent_selection_count_60s as u64)
            .sum();
        if total_recent == 0 {
            return true;
        }
        let warming_recent: u64 = warming
            .iter()
            .map(|entry| entry.health.recent_selection_count_60s as u64)
            .sum();
        let current_share = warming_recent as f64 / total_recent as f64;
        current_share < target_share
    }

    fn update_credential_rpm_from_config(&self) {
        if self.rate_limit_interval().is_none() {
            let ids: Vec<u64> = {
                let entries = self.entries.lock();
                entries.iter().map(|entry| entry.id).collect()
            };
            let mut entries = self.entries.lock();
            for entry in entries.iter_mut() {
                entry.rate_limit_available_at = None;
            }
            drop(entries);
            if let Some(redis) = &self.redis_store {
                let redis = redis.clone();
                if let Err(err) = block_on_storage("清理 Redis 凭据限流状态", async move {
                    for id in ids {
                        redis.clear_rate_limit(id).await?;
                    }
                    Ok(())
                }) {
                    tracing::warn!("清理 Redis 凭据限流状态失败: {}", err);
                }
            }
        }
    }

    fn min_dispatch_wait(
        &self,
        entries: &[CredentialEntry],
        proxy_resources: &HashMap<u64, ProxyResourceRuntime>,
        model: Option<&str>,
        excluded_ids: &HashSet<u64>,
        now: Instant,
    ) -> Option<StdDuration> {
        entries
            .iter()
            .filter(|entry| {
                Self::credential_is_dispatch_candidate(proxy_resources, entry, model, excluded_ids)
            })
            .filter_map(|entry| {
                match (
                    Self::entry_cooldown_remaining(entry, now),
                    Self::entry_rate_limit_remaining(entry, now),
                ) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    (Some(a), None) => Some(a),
                    (None, Some(b)) => Some(b),
                    (None, None) => None,
                }
            })
            .min()
    }

    fn concurrency_blocked_count(
        &self,
        entries: &[CredentialEntry],
        proxy_resources: &HashMap<u64, ProxyResourceRuntime>,
        model: Option<&str>,
        excluded_ids: &HashSet<u64>,
        now: Instant,
        max_concurrent_requests: u32,
    ) -> usize {
        entries
            .iter()
            .filter(|entry| {
                Self::credential_is_dispatch_candidate(proxy_resources, entry, model, excluded_ids)
                    && Self::entry_cooldown_remaining(entry, now).is_none()
                    && Self::entry_rate_limit_remaining(entry, now).is_none()
                    && !Self::entry_has_concurrency_capacity(entry, max_concurrent_requests)
            })
            .count()
    }

    fn mark_rate_limited_at(&self, id: u64, now: Instant) -> anyhow::Result<()> {
        let Some(interval) = self.rate_limit_interval() else {
            return Ok(());
        };
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            let next_at_ms = block_on_storage("更新 Redis 凭据限流状态", async move {
                redis.bump_rate_limit_available_at(id, interval).await
            })?;
            let now_ms = Utc::now().timestamp_millis();
            let next_at = instant_from_epoch_ms(next_at_ms, now_ms, Instant::now());
            let mut entries = self.entries.lock();
            if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                entry.rate_limit_available_at = next_at;
            }
            return Ok(());
        }
        let mut entries = self.entries.lock();
        if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
            let base = entry
                .rate_limit_available_at
                .filter(|at| *at > now)
                .unwrap_or(now);
            entry.rate_limit_available_at = Some(base + interval);
        }
        Ok(())
    }

    fn acquire_in_flight_slot(&self, id: u64) -> anyhow::Result<Option<InFlightLeaseGuard>> {
        self.cleanup_expired_in_flight_leases_result()?;
        let max_concurrent_requests = self.max_concurrent_requests();
        let effective_max_concurrent_requests =
            self.effective_max_concurrent_requests_for_id(id, max_concurrent_requests);
        let global_max_concurrent_requests = self.global_max_concurrent_requests();
        let now = Instant::now();

        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            let max_age = self.in_flight_lease_max_age();
            let lease_id = block_on_storage("生成 Redis 并发 lease ID", {
                let redis = redis.clone();
                async move { redis.next_in_flight_lease_id().await }
            })?;

            match block_on_storage("占用 Redis 凭据并发槽", {
                let redis = redis.clone();
                async move {
                    redis
                        .acquire_dispatch_lease(
                            id,
                            lease_id,
                            effective_max_concurrent_requests,
                            global_max_concurrent_requests,
                            max_age,
                            InFlightKind::Api.as_str(),
                        )
                        .await
                }
            }) {
                Ok(Some(_count)) => {
                    let mut entries = self.entries.lock();
                    if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                        entry.in_flight_requests = entry.in_flight_requests.saturating_add(1);
                        entry.in_flight_leases.push(InFlightLease {
                            id: lease_id,
                            acquired_at: now,
                            last_seen_at: now,
                            kind: InFlightKind::Api,
                        });
                    }
                    return Ok(Some(InFlightLeaseGuard::new(
                        self.entries.clone(),
                        self.redis_store.clone(),
                        self.in_flight_notify.clone(),
                        id,
                        lease_id,
                    )));
                }
                Ok(None) => return Ok(None),
                Err(err) => return Err(err),
            }
        }

        let lease_id = self.next_in_flight_lease_id.fetch_add(1, Ordering::Relaxed);
        let mut entries = self.entries.lock();
        let global_in_flight: u32 = entries.iter().map(|entry| entry.in_flight_requests).sum();
        if global_max_concurrent_requests > 0 && global_in_flight >= global_max_concurrent_requests
        {
            return Ok(None);
        }
        if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
            if !Self::entry_has_concurrency_capacity(entry, max_concurrent_requests) {
                return Ok(None);
            }
            entry.in_flight_requests = entry.in_flight_requests.saturating_add(1);
            entry.in_flight_leases.push(InFlightLease {
                id: lease_id,
                acquired_at: now,
                last_seen_at: now,
                kind: InFlightKind::Api,
            });
            return Ok(Some(InFlightLeaseGuard::new(
                self.entries.clone(),
                self.redis_store.clone(),
                self.in_flight_notify.clone(),
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
            }
            self.refresh_scheduler_state_from_redis()?;
            return Ok(cleaned);
        }
        let now = Instant::now();
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
        Ok(cleaned)
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
                    self.refresh_scheduler_state_from_redis_best_effort();
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
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            let admitted = block_on_storage("占用 Redis 调度排队名额", async move {
                redis.try_enter_dispatch_queue(max_queued).await
            })?;
            if !admitted {
                return Ok(None);
            }
        } else {
            loop {
                let queued = self.queued_requests.load(Ordering::Acquire);
                if max_queued > 0 && queued >= max_queued {
                    return Ok(None);
                }
                if self
                    .queued_requests
                    .compare_exchange(queued, queued + 1, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    break;
                }
            }
        }
        Ok(Some(DispatchQueueGuard::new(
            self.redis_store.clone(),
            self.queued_requests.clone(),
            self.in_flight_notify.clone(),
        )))
    }

    fn global_capacity_state(&self) -> SchedulerGlobalCapacityState {
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            return block_on_storage("读取 Redis 全局调度容量", async move {
                redis.global_capacity_state().await
            })
            .unwrap_or_else(|err| {
                tracing::warn!("读取 Redis 全局调度容量失败: {}", err);
                SchedulerGlobalCapacityState::default()
            });
        }
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

    fn dispatch_max_wait(&self) -> Option<StdDuration> {
        let secs = self.config.lock().credential_dispatch_max_wait_secs;
        (secs > 0).then(|| StdDuration::from_secs(secs))
    }

    fn dispatch_wait_exceeded(
        &self,
        started_at: Instant,
        now: Instant,
    ) -> Option<(StdDuration, StdDuration)> {
        let max_wait = self.dispatch_max_wait()?;
        let waited = now.saturating_duration_since(started_at);
        (waited >= max_wait).then_some((waited, max_wait))
    }

    fn dispatch_wait_remaining(&self, started_at: Instant, now: Instant) -> Option<StdDuration> {
        let max_wait = self.dispatch_max_wait()?;
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
            Self::refresh_local_selection_windows_locked(&mut entries, now);
        }
        let proxy_resources = self.proxy_resources.lock();
        entries.iter().any(|entry| {
            entry.id != current_id
                && !excluded_ids.contains(&entry.id)
                && Self::credential_is_dispatchable(
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
        let max_concurrent_requests = self.max_concurrent_requests();

        // 过滤可用凭据
        let available: Vec<_> = entries
            .iter()
            .filter(|e| {
                !excluded_ids.contains(&e.id)
                    && Self::credential_is_dispatchable(
                        &proxy_resources,
                        e,
                        model,
                        now,
                        max_concurrent_requests,
                    )
            })
            .collect();

        if available.is_empty() {
            return None;
        }

        let mode = self.load_balancing_mode.lock().clone();
        let mode = mode.as_str();

        match mode {
            "health_balanced" => {
                let ready: Vec<_> = available
                    .iter()
                    .filter(|e| e.warmup_remaining == 0)
                    .copied()
                    .collect();
                let warming: Vec<_> = available
                    .iter()
                    .filter(|e| e.warmup_remaining > 0)
                    .copied()
                    .collect();

                // 预热不是后台主动打流量，而是在真实业务请求中按目标份额参与调度；
                // 份额按预热账号数量放大并受最大预热份额限制，避免批量导入后长期吃不到流量。
                let candidates = if self.should_select_warming(&ready, &warming, &available) {
                    warming
                } else {
                    ready
                };
                let entry =
                    self.select_health_weighted(candidates, Utc::now().timestamp_millis())?;

                Some((entry.id, entry.credentials.clone()))
            }
            "balanced" => {
                let ready: Vec<_> = available
                    .iter()
                    .filter(|entry| entry.warmup_remaining == 0)
                    .copied()
                    .collect();
                let warming: Vec<_> = available
                    .iter()
                    .filter(|entry| entry.warmup_remaining > 0)
                    .copied()
                    .collect();
                let candidates = if self.should_select_warming(&ready, &warming, &available) {
                    warming
                } else {
                    ready
                };
                let entry = candidates
                    .iter()
                    .min_by_key(|entry| Self::balanced_selection_key(entry))?;
                Some((entry.id, entry.credentials.clone()))
            }
            _ => {
                // priority 模式（默认）：优先级仍是第一排序，但同优先级账号优先选低并发。
                let entry = available
                    .iter()
                    .min_by_key(|e| Self::priority_selection_key(e))?;
                Some((entry.id, entry.credentials.clone()))
            }
        }
    }

    fn select_health_weighted<'a>(
        &self,
        candidates: Vec<&'a CredentialEntry>,
        now_ms: i64,
    ) -> Option<&'a CredentialEntry> {
        let mut scored: Vec<(&CredentialEntry, f64)> = candidates
            .iter()
            .copied()
            .map(|entry| {
                let pressure = Self::selection_pressure_for_candidates(entry, &candidates);
                (entry, self.scheduler_score(entry, now_ms, pressure))
            })
            .collect();
        scored.sort_by(|(left_entry, left), (right_entry, right)| {
            left.partial_cmp(right)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left_entry.id.cmp(&right_entry.id))
        });
        let top_k = self.config.lock().scheduler_top_k.max(1) as usize;
        let top = &scored[..scored.len().min(top_k)];
        let worst_score = top.last()?.1;
        let total_weight: f64 = top
            .iter()
            .map(|(_, score)| (worst_score - score + 1.0).max(0.01))
            .sum();
        let mut roll = fastrand::f64() * total_weight;
        for (entry, score) in top {
            roll -= (worst_score - score + 1.0).max(0.01);
            if roll <= 0.0 {
                return Some(*entry);
            }
        }
        top.last().map(|(entry, _)| *entry)
    }

    fn balanced_selection_key(entry: &CredentialEntry) -> (u32, u32, u32, u64, u32, u64, u64) {
        (
            entry.in_flight_requests,
            entry.health.recent_selection_count_10s,
            entry.health.recent_selection_count_60s,
            entry.success_count,
            entry.credentials.priority,
            entry.total_selection_count,
            entry.id,
        )
    }

    fn priority_selection_key(entry: &CredentialEntry) -> (u32, u32, u32, u32, u64, u64, u64) {
        (
            entry.credentials.priority,
            entry.in_flight_requests,
            entry.health.recent_selection_count_10s,
            entry.health.recent_selection_count_60s,
            entry.success_count,
            entry.total_selection_count,
            entry.id,
        )
    }

    fn prune_session_bindings_locked(bindings: &mut HashMap<String, SessionBinding>) {
        let now = Utc::now();
        bindings.retain(|_, binding| {
            now.signed_duration_since(binding.last_used_at)
                .num_seconds()
                <= SESSION_BINDING_TTL_SECS
        });

        if bindings.len() <= MAX_SESSION_BINDINGS {
            return;
        }

        let mut sessions_by_age: Vec<_> = bindings
            .iter()
            .map(|(session_id, binding)| (session_id.clone(), binding.last_used_at))
            .collect();
        sessions_by_age.sort_by_key(|(_, last_used_at)| *last_used_at);

        let remove_count = bindings.len() - MAX_SESSION_BINDINGS;
        for (session_id, _) in sessions_by_age.into_iter().take(remove_count) {
            bindings.remove(&session_id);
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
            return None;
        }
        let bound_id = if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            match block_on_storage("读取 Redis 会话绑定", async move {
                redis.get_session_binding(session_id).await
            }) {
                Ok(binding) => binding.map(|binding| binding.credential_id),
                Err(err) => {
                    tracing::warn!("读取 Redis 会话绑定失败，本次不使用会话粘性绑定: {}", err);
                    return None;
                }
            }
        } else {
            let mut bindings = self.session_bindings.lock();
            Self::prune_session_bindings_locked(&mut bindings);
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
                    && Self::credential_is_dispatchable(
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
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            match block_on_storage("读取 Redis 会话绑定", async move {
                redis.get_session_binding(session_id).await
            }) {
                Ok(binding) => return binding.map(|binding| binding.credential_id),
                Err(err) => {
                    tracing::warn!("读取 Redis 会话绑定失败，本次不使用会话粘性绑定: {}", err);
                    return None;
                }
            }
        }
        self.session_bindings
            .lock()
            .get(session_id)
            .map(|binding| binding.credential_id)
    }

    fn bound_credential_exists_but_unusable(&self, session_id: &str, model: Option<&str>) -> bool {
        if let Err(err) = self.refresh_scheduler_state_from_redis() {
            tracing::warn!("检查会话绑定可用性前同步 Redis 调度状态失败: {}", err);
            return true;
        }
        let Some(bound_id) = self.bound_credential_id(session_id) else {
            return false;
        };

        let entries = self.entries.lock();
        let proxy_resources = self.proxy_resources.lock();
        let now = Instant::now();
        entries.iter().find(|e| e.id == bound_id).is_none_or(|e| {
            !Self::credential_is_temporarily_available(&proxy_resources, e, model, now)
        })
    }

    fn bind_session_to_credential(&self, session_id: &str, credential_id: u64) {
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            if let Err(err) = block_on_storage("写入 Redis 会话绑定", async move {
                let soft_failure_count = redis
                    .get_session_binding(session_id)
                    .await?
                    .filter(|binding| binding.credential_id == credential_id)
                    .map(|binding| binding.soft_failure_count)
                    .unwrap_or(0);
                let binding = SchedulerSessionBinding {
                    credential_id,
                    last_used_at: Utc::now(),
                    soft_failure_count,
                };
                redis
                    .set_session_binding(session_id, &binding, SESSION_BINDING_TTL_SECS as usize)
                    .await
            }) {
                tracing::warn!("写入 Redis 会话绑定失败，本次不写入本进程镜像: {}", err);
            }
            return;
        }
        let mut bindings = self.session_bindings.lock();
        Self::prune_session_bindings_locked(&mut bindings);
        match bindings.get_mut(session_id) {
            Some(binding) if binding.credential_id == credential_id => {
                binding.last_used_at = Utc::now();
            }
            _ => {
                bindings.insert(
                    session_id.to_string(),
                    SessionBinding {
                        credential_id,
                        last_used_at: Utc::now(),
                        soft_failure_count: 0,
                    },
                );
            }
        }
    }

    /// 清理指定会话的粘性绑定。
    pub fn unbind_session(&self, session_id: &str) {
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            if let Err(err) = block_on_storage("删除 Redis 会话绑定", async move {
                redis.delete_session_binding(session_id).await
            }) {
                tracing::warn!("删除 Redis 会话绑定失败: {}", err);
            }
        }
        self.session_bindings.lock().remove(session_id);
    }

    /// 仅当指定会话当前绑定到该凭据时清理绑定。
    pub fn unbind_session_if_bound_to(&self, session_id: &str, credential_id: u64) {
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            match block_on_storage("读取 Redis 会话绑定", async move {
                redis.get_session_binding(session_id).await
            }) {
                Ok(Some(binding)) if binding.credential_id == credential_id => {
                    self.unbind_session(session_id);
                    return;
                }
                Ok(_) => return,
                Err(err) => {
                    tracing::warn!("读取 Redis 会话绑定失败，跳过会话绑定清理: {}", err);
                    return;
                }
            }
        }
        let mut bindings = self.session_bindings.lock();
        if bindings
            .get(session_id)
            .is_some_and(|binding| binding.credential_id == credential_id)
        {
            bindings.remove(session_id);
        }
    }

    /// 清理某个凭据关联的所有会话绑定。
    pub fn unbind_sessions_for_credential(&self, credential_id: u64) {
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            if let Err(err) = block_on_storage("删除 Redis 凭据会话绑定", async move {
                redis.delete_sessions_for_credential(credential_id).await
            }) {
                tracing::warn!(credential_id, "删除 Redis 凭据会话绑定失败: {}", err);
            }
        }
        self.session_bindings
            .lock()
            .retain(|_, binding| binding.credential_id != credential_id);
    }

    /// 记录绑定账号的一次软失败。返回 true 表示本次请求可以临时 fallback。
    pub fn record_session_soft_failure(&self, session_id: &str, credential_id: u64) -> bool {
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            match block_on_storage("记录 Redis 会话软失败", async move {
                redis
                    .record_session_soft_failure(
                        session_id,
                        credential_id,
                        MAX_SESSION_SOFT_FAILURES,
                        SESSION_BINDING_TTL_SECS as usize,
                    )
                    .await
            }) {
                Ok(should_fallback) => return should_fallback,
                Err(err) => {
                    tracing::warn!(
                        "记录 Redis 会话软失败失败，本次不触发会话 fallback: {}",
                        err
                    );
                    return false;
                }
            }
        }
        let mut bindings = self.session_bindings.lock();
        if let Some(binding) = bindings.get_mut(session_id) {
            if binding.credential_id == credential_id {
                binding.last_used_at = Utc::now();
                binding.soft_failure_count = binding.soft_failure_count.saturating_add(1);
                return binding.soft_failure_count >= MAX_SESSION_SOFT_FAILURES;
            }
        }
        false
    }

    /// 清理绑定账号的软失败计数。
    pub fn clear_session_soft_failure(&self, session_id: &str, credential_id: u64) {
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            if let Err(err) = block_on_storage("清理 Redis 会话软失败", async move {
                redis
                    .clear_session_soft_failure(
                        session_id,
                        credential_id,
                        SESSION_BINDING_TTL_SECS as usize,
                    )
                    .await
            }) {
                tracing::warn!("清理 Redis 会话软失败失败: {}", err);
            }
        }
        let mut bindings = self.session_bindings.lock();
        if let Some(binding) = bindings.get_mut(session_id) {
            if binding.credential_id == credential_id {
                binding.last_used_at = Utc::now();
                binding.soft_failure_count = 0;
            }
        }
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
        enum AcquireDecision {
            Selected(u64, KiroCredentials, bool, bool),
            WaitForDispatch {
                available: usize,
                total: usize,
                max_concurrent_requests: u32,
                wait_for: Option<StdDuration>,
            },
        }

        let total = self.total_count();
        let max_attempts = (total * MAX_FAILURES_PER_CREDENTIAL as usize).max(1);
        let mut attempt_count = 0;
        let dispatch_wait_started_at = Instant::now();
        let mut queue_guard: Option<DispatchQueueGuard> = None;

        loop {
            self.refresh_scheduler_state_from_redis()?;
            self.cleanup_expired_in_flight_leases_result()?;
            if attempt_count >= max_attempts {
                anyhow::bail!(
                    "所有凭据均无法获取有效 Token（可用: {}/{}）",
                    self.available_count(),
                    total
                );
            }

            let decision = {
                let existing_bound_id = session_id.and_then(|sid| self.bound_credential_id(sid));
                let bound_hit =
                    session_id.and_then(|sid| self.get_bound_credential(sid, model, excluded_ids));

                if let Some(hit) = bound_hit {
                    AcquireDecision::Selected(hit.0, hit.1, true, false)
                } else {
                    let fallback_from_sticky = existing_bound_id.is_some();
                    {
                        // 根据负载均衡策略选择；priority 模式也会在同优先级账号之间优先低并发。
                        let mut best = self.select_next_credential_excluding(model, excluded_ids);

                        // 没有可用凭据：如果是"自动禁用导致全灭"，做一次类似重启的自愈
                        if best.is_none() {
                            let mut healed_ids = Vec::new();
                            let mut entries = self.entries.lock();
                            if entries.iter().any(|e| {
                                e.disabled
                                    && e.disabled_reason == Some(DisabledReason::TooManyFailures)
                            }) {
                                tracing::warn!(
                                    "所有凭据均已被自动禁用，执行自愈：重置失败计数并重新启用（等价于重启）"
                                );
                                for e in entries.iter_mut() {
                                    if e.disabled_reason == Some(DisabledReason::TooManyFailures) {
                                        e.disabled = false;
                                        e.disabled_reason = None;
                                        e.failure_count = 0;
                                        healed_ids.push(e.id);
                                    }
                                }
                                drop(entries);
                                for healed_id in healed_ids {
                                    if let Err(err) = self.persist_credential_entry(healed_id) {
                                        tracing::warn!(
                                            credential_id = healed_id,
                                            "自动自愈后持久化凭据失败: {}",
                                            err
                                        );
                                    }
                                    self.save_runtime_state_for(healed_id);
                                }
                                self.publish_credentials_changed("auto_heal_too_many_failures");
                                best = self.select_next_credential_excluding(model, excluded_ids);
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
                            // 注意：必须在 bail! 之前计算 available_count，
                            // 因为 available_count() 会尝试获取 entries 锁，
                            // 而此时我们已经持有该锁，会导致死锁
                            let available = entries.iter().filter(|e| !e.disabled).count();
                            let usable = entries
                                .iter()
                                .filter(|e| {
                                    Self::credential_is_usable_for_model(e, model)
                                        && Self::credential_proxy_is_dispatchable(
                                            &e.credentials,
                                            &proxy_resources,
                                        )
                                })
                                .count();
                            let dispatchable = {
                                let now = Instant::now();
                                let max_concurrent_requests = self.max_concurrent_requests();
                                entries
                                    .iter()
                                    .filter(|e| {
                                        !excluded_ids.contains(&e.id)
                                            && Self::credential_is_dispatchable(
                                                &proxy_resources,
                                                e,
                                                model,
                                                now,
                                                max_concurrent_requests,
                                            )
                                    })
                                    .count()
                            };
                            let excluded_usable = entries
                                .iter()
                                .filter(|e| {
                                    excluded_ids.contains(&e.id)
                                        && Self::credential_is_usable_for_model(e, model)
                                })
                                .count();
                            if usable > 0 && excluded_usable >= usable {
                                anyhow::bail!(
                                    "本次请求临时排除了所有可用凭据（可用: {}/{}, 临时排除: {}）",
                                    available,
                                    total,
                                    excluded_usable
                                );
                            }
                            if available > 0 && usable == 0 && model.is_some() {
                                anyhow::bail!(
                                    "没有支持当前模型的可用凭据（可用: {}/{}）",
                                    available,
                                    total
                                );
                            }
                            if usable > 0 && dispatchable == 0 {
                                let now = Instant::now();
                                let max_concurrent_requests = self.max_concurrent_requests();
                                let dispatch_candidate_count = entries
                                    .iter()
                                    .filter(|e| {
                                        Self::credential_is_dispatch_candidate(
                                            &proxy_resources,
                                            e,
                                            model,
                                            excluded_ids,
                                        )
                                    })
                                    .count();
                                let cooldown_blocked = entries
                                    .iter()
                                    .filter(|e| {
                                        Self::credential_is_dispatch_candidate(
                                            &proxy_resources,
                                            e,
                                            model,
                                            excluded_ids,
                                        ) && Self::entry_cooldown_remaining(e, now).is_some()
                                    })
                                    .count();
                                let wait_for = self.min_dispatch_wait(
                                    &entries,
                                    &proxy_resources,
                                    model,
                                    excluded_ids,
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
                                        "所有可用凭据均处于上游临时冷却（可用: {}/{}, 临时可调度: 0, max_concurrent_requests={}, retry_after_secs={}）",
                                        available,
                                        total,
                                        max_concurrent_requests,
                                        retry_after_secs
                                    );
                                }
                                let concurrency_blocked = self.concurrency_blocked_count(
                                    &entries,
                                    &proxy_resources,
                                    model,
                                    excluded_ids,
                                    now,
                                    max_concurrent_requests,
                                );
                                if concurrency_blocked > 0
                                    && concurrency_blocked >= dispatch_candidate_count
                                {
                                    AcquireDecision::WaitForDispatch {
                                        available,
                                        total,
                                        max_concurrent_requests,
                                        wait_for: None,
                                    }
                                } else {
                                    AcquireDecision::WaitForDispatch {
                                        available,
                                        total,
                                        max_concurrent_requests,
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
                    max_concurrent_requests,
                    wait_for,
                } => {
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
                        self.dispatch_wait_exceeded(dispatch_wait_started_at, now)
                    {
                        anyhow::bail!(
                            "凭据调度排队等待超时（可用: {}/{}, 临时可调度: 0, max_concurrent_requests={}, waited_secs={}, max_wait_secs={}, retry_after_secs={}）",
                            available,
                            total,
                            max_concurrent_requests,
                            waited.as_secs(),
                            max_wait.as_secs(),
                            retry_after_secs.max(1)
                        );
                    }
                    tracing::debug!(
                        available,
                        total,
                        max_concurrent_requests,
                        retry_after_secs,
                        "所有可用凭据暂不可调度，进入排队等待"
                    );
                    self.wait_for_dispatch_capacity(
                        wait_for,
                        self.dispatch_wait_remaining(dispatch_wait_started_at, now),
                    )
                    .await;
                    continue;
                }
            };

            let Some(in_flight_lease) = self.acquire_in_flight_slot(id)? else {
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
                    self.dispatch_wait_exceeded(dispatch_wait_started_at, now)
                {
                    anyhow::bail!(
                        "凭据调度排队等待超时（可用: {}/{}, 临时可调度: 0, max_concurrent_requests={}, waited_secs={}, max_wait_secs={}, retry_after_secs=1）",
                        self.available_count(),
                        total,
                        self.max_concurrent_requests(),
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
                    self.dispatch_wait_remaining(dispatch_wait_started_at, now),
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
            // 获取刷新锁，确保同一时间只有一个刷新操作
            let _guard = self.refresh_lock.lock().await;

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

                // 只回写当前凭据行，避免旧内存快照覆盖其他实例新增凭据。
                if let Err(e) = self.persist_credential_entry(id) {
                    tracing::warn!("Token 刷新后持久化失败（不影响本次请求）: {}", e);
                } else {
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
                self.save_runtime_state_for(id);
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
            Self::credential_proxy_availability(&creds, &resources)
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
            unavailable => Err(Self::proxy_unavailable_error(creds.id, unavailable)),
        }
    }

    fn effective_proxy_display(&self, creds: &KiroCredentials) -> (Option<String>, String) {
        match creds.proxy_url.as_deref() {
            Some(url) if url.eq_ignore_ascii_case(KiroCredentials::PROXY_DIRECT) => {
                (None, "direct".to_string())
            }
            Some(url) => (Some(url.to_string()), "credential".to_string()),
            None => {
                if let Some(resource_id) = creds.proxy_resource_id {
                    let resources = self.proxy_resources.lock();
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

    fn proxy_resource_name(&self, resource_id: Option<u64>) -> Option<String> {
        let resource_id = resource_id?;
        self.proxy_resources
            .lock()
            .get(&resource_id)
            .map(|resource| resource.name.clone())
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
        let existing_ids: HashSet<u64> = entries.iter().map(|entry| entry.id).collect();
        for entry in entries.iter_mut() {
            if let Some(credential) = by_id.get(&entry.id) {
                let mut credential = credential.clone();
                credential.canonicalize_auth_method();
                entry.disabled = credential.disabled;
                entry.credentials = credential;
                changed = true;
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
                rate_limit_available_at: None,
                in_flight_requests: 0,
                in_flight_leases: Vec::new(),
                warmup_remaining: 0,
                health: SchedulerHealthState::default(),
                selection_events: VecDeque::new(),
            });
            changed = true;
        }
        if changed {
            entries.sort_by_key(|entry| (entry.credentials.priority, entry.id));
        }
        drop(entries);
        if changed {
            self.load_stats();
            self.load_runtime_state();
            self.select_highest_priority();
        }
        Ok(changed || proxy_changed)
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
        let Some(redis) = &self.redis_store else {
            return;
        };
        let redis = redis.clone();
        let payload = self.redis_event_payload("runtime_config_changed", version, reason);
        if let Err(err) = block_on_storage("发布 Redis 运行配置变更通知", async move {
            redis.publish_runtime_config_changed(payload).await
        }) {
            tracing::warn!("发布 Redis 运行配置变更通知失败: {}", err);
        }
    }

    fn publish_credentials_changed(&self, reason: &str) {
        if let Some(store) = &self.postgres_store {
            let store = store.clone();
            let reason_owned = reason.to_string();
            if let Err(err) = block_on_storage("记录凭据事件到 PgSQL", async move {
                store
                    .record_credential_event(
                        None,
                        "credentials_changed",
                        Some(&reason_owned),
                        serde_json::json!({ "reason": reason_owned }),
                    )
                    .await
            }) {
                tracing::warn!("记录凭据事件失败: {}", err);
            }
        }
        let Some(redis) = &self.redis_store else {
            return;
        };
        let redis = redis.clone();
        let payload = self.redis_event_payload("credentials_changed", None, reason);
        if let Err(err) = block_on_storage("发布 Redis 凭据变更通知", async move {
            redis.publish_credentials_changed(payload).await
        }) {
            tracing::warn!("发布 Redis 凭据变更通知失败: {}", err);
        }
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
        let stats: HashMap<u64, CredentialStatsRow> = {
            let entries = self.entries.lock();
            entries
                .iter()
                .map(|e| {
                    (
                        e.id,
                        CredentialStatsRow {
                            success_count: e.success_count,
                            selection_count: e.total_selection_count,
                            last_used_at: e.last_used_at.clone(),
                        },
                    )
                })
                .collect()
        };

        let store = store.clone();
        match block_on_storage("保存凭据统计到 PgSQL", async move {
            store.save_credential_stats(&stats).await
        }) {
            Ok(()) => {
                *self.last_stats_save_at.lock() = Some(Instant::now());
                self.stats_dirty.store(false, Ordering::Relaxed);
            }
            Err(e) => tracing::warn!("{}", e),
        }
    }

    /// 从 PgSQL 加载凭据运行态（失败计数、禁用原因、预热次数）。
    fn load_runtime_state(&self) {
        let Some(store) = &self.postgres_store else {
            return;
        };
        let store = store.clone();
        let states = match block_on_storage("从 PgSQL 加载凭据运行态", async move {
            store.load_credential_runtime_state().await
        }) {
            Ok(states) => states,
            Err(e) => {
                tracing::warn!("{}", e);
                return;
            }
        };

        let mut entries = self.entries.lock();
        for entry in entries.iter_mut() {
            if let Some(state) = states.get(&entry.id) {
                entry.failure_count = state.failure_count;
                entry.refresh_failure_count = state.refresh_failure_count;
                entry.warmup_remaining = state.warmup_remaining;
                if let Some(reason) = state
                    .disabled_reason
                    .as_deref()
                    .and_then(DisabledReason::from_str)
                {
                    entry.disabled_reason = Some(reason);
                } else if !entry.disabled {
                    entry.disabled_reason = None;
                }
            }
        }
        tracing::info!("已从 PgSQL 加载 {} 条凭据运行态", states.len());
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
        let state = state.clone();
        block_on_storage("保存凭据运行态到 PgSQL", async move {
            store.save_credential_runtime_state_for(id, &state).await
        })?;
        Ok(true)
    }

    fn persist_success_state(&self, id: u64, last_used_at: &str) {
        let Some(store) = &self.postgres_store else {
            return;
        };
        let store = store.clone();
        let last_used_at = last_used_at.to_string();
        match block_on_storage("记录凭据成功统计到 PgSQL", async move {
            store.record_credential_success(id, &last_used_at).await
        }) {
            Ok(()) => {
                *self.last_stats_save_at.lock() = Some(Instant::now());
                self.stats_dirty.store(false, Ordering::Relaxed);
            }
            Err(e) => tracing::warn!("{}", e),
        }
    }

    fn apply_runtime_state_for(&self, id: u64, state: &CredentialRuntimeStateRow) -> bool {
        let mut disabled_now = false;
        let mut entries = self.entries.lock();
        if let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) {
            let was_disabled = entry.disabled;
            entry.failure_count = state.failure_count;
            entry.refresh_failure_count = state.refresh_failure_count;
            entry.warmup_remaining = state.warmup_remaining;
            if let Some(reason) = state
                .disabled_reason
                .as_deref()
                .and_then(DisabledReason::from_str)
            {
                entry.disabled = true;
                entry.disabled_reason = Some(reason);
            } else if !entry.disabled {
                entry.disabled_reason = None;
            }
            disabled_now = !was_disabled && entry.disabled;
        }
        disabled_now
    }

    fn persist_api_failure_state(
        &self,
        id: u64,
        last_used_at: &str,
    ) -> anyhow::Result<Option<CredentialRuntimeStateRow>> {
        let Some(store) = &self.postgres_store else {
            return Ok(None);
        };
        let store = store.clone();
        let last_used_at = last_used_at.to_string();
        let state = block_on_storage("记录凭据失败计数到 PgSQL", async move {
            store
                .record_credential_api_failure(id, &last_used_at, MAX_FAILURES_PER_CREDENTIAL)
                .await
        })?;
        Ok(Some(state))
    }

    fn persist_refresh_failure_state(
        &self,
        id: u64,
        last_used_at: &str,
    ) -> anyhow::Result<Option<CredentialRuntimeStateRow>> {
        let Some(store) = &self.postgres_store else {
            return Ok(None);
        };
        let store = store.clone();
        let last_used_at = last_used_at.to_string();
        let state = block_on_storage("记录凭据刷新失败计数到 PgSQL", async move {
            store
                .record_credential_refresh_failure(id, &last_used_at, MAX_FAILURES_PER_CREDENTIAL)
                .await
        })?;
        Ok(Some(state))
    }

    fn persist_disabled_state(
        &self,
        id: u64,
        reason: DisabledReason,
        failure_count: Option<u32>,
        refresh_failure_count: Option<u32>,
        last_used_at: &str,
    ) -> anyhow::Result<Option<CredentialRuntimeStateRow>> {
        let Some(store) = &self.postgres_store else {
            return Ok(None);
        };
        let store = store.clone();
        let last_used_at = last_used_at.to_string();
        let state = block_on_storage("记录凭据禁用状态到 PgSQL", async move {
            store
                .mark_credential_disabled(
                    id,
                    reason.as_str(),
                    failure_count,
                    refresh_failure_count,
                    &last_used_at,
                )
                .await
        })?;
        Ok(Some(state))
    }

    fn persist_last_used_at(&self, id: u64, last_used_at: &str) {
        let Some(store) = &self.postgres_store else {
            return;
        };
        let store = store.clone();
        let last_used_at = last_used_at.to_string();
        if let Err(e) = block_on_storage("记录凭据最后使用时间到 PgSQL", async move {
            store
                .update_credential_last_used_at(id, &last_used_at)
                .await
        }) {
            tracing::warn!("{}", e);
        }
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
        let states = block_on_storage("从 Redis 同步调度运行态", async move {
            redis.scheduler_state_for_credentials(&ids).await
        })?;
        self.apply_scheduler_states(states);
        Ok(())
    }

    fn refresh_scheduler_state_from_redis_best_effort(&self) {
        if let Err(err) = self.refresh_scheduler_state_from_redis() {
            tracing::warn!("从 Redis 同步调度运行态失败: {}", err);
        }
    }

    fn apply_scheduler_states(&self, states: HashMap<u64, SchedulerCredentialState>) {
        let now_ms = Utc::now().timestamp_millis();
        let now = Instant::now();
        let mut entries = self.entries.lock();
        for entry in entries.iter_mut() {
            let state = states.get(&entry.id).cloned().unwrap_or_default();
            entry.cooldown_until = state
                .cooldown
                .as_ref()
                .and_then(|cooldown| instant_from_epoch_ms(cooldown.until_ms, now_ms, now));
            entry.cooldown_reason = state.cooldown.and_then(|cooldown| cooldown.reason);
            entry.rate_limit_available_at = state
                .rate_limit_available_at_ms
                .and_then(|until_ms| instant_from_epoch_ms(until_ms, now_ms, now));
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
    }

    fn record_scheduler_selection(&self, id: u64) {
        let now = Instant::now();
        {
            let mut entries = self.entries.lock();
            if let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) {
                Self::record_local_selection(entry, now);
            }
        }
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            match block_on_storage("记录 Redis 调度选中次数", async move {
                redis.record_scheduler_selection(id).await
            }) {
                Ok(health) => {
                    let mut entries = self.entries.lock();
                    if let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) {
                        entry.health = health;
                    }
                }
                Err(err) => {
                    tracing::warn!(credential_id = id, "记录 Redis 调度选中次数失败: {}", err);
                }
            }
        }
        if let Some(store) = &self.postgres_store {
            let store = store.clone();
            if let Err(err) = block_on_storage("记录 PgSQL 调度选中次数", async move {
                store.record_credential_selection(id).await
            }) {
                tracing::warn!(credential_id = id, "记录 PgSQL 调度选中次数失败: {}", err);
            } else {
                *self.last_stats_save_at.lock() = Some(Instant::now());
                self.stats_dirty.store(false, Ordering::Relaxed);
            }
        }
    }

    fn clear_scheduler_state_for_credential(&self, id: u64, clear_in_flight: bool) {
        {
            let mut entries = self.entries.lock();
            if let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) {
                entry.cooldown_until = None;
                entry.cooldown_reason = None;
                entry.rate_limit_available_at = None;
                entry.health = SchedulerHealthState::default();
                if clear_in_flight {
                    entry.in_flight_requests = 0;
                    entry.in_flight_leases.clear();
                }
            }
        }
        let Some(redis) = &self.redis_store else {
            return;
        };
        let redis = redis.clone();
        if let Err(err) = block_on_storage("清理 Redis 凭据调度状态", async move {
            redis.clear_scheduler_cooldown(id).await?;
            redis.clear_scheduler_health(id).await?;
            redis.clear_rate_limit(id).await?;
            redis.delete_sessions_for_credential(id).await?;
            if clear_in_flight {
                redis.clear_in_flight_leases(id, None).await?;
            }
            Ok(())
        }) {
            tracing::warn!(credential_id = id, "清理 Redis 凭据调度状态失败: {}", err);
        }
    }

    /// 报告指定凭据 API 调用成功
    ///
    /// 重置该凭据的失败计数
    ///
    /// # Arguments
    /// * `id` - 凭据 ID（来自 CallContext）
    #[allow(dead_code)]
    pub fn report_success(&self, id: u64) {
        self.report_success_with_latency(id, None);
    }

    pub fn report_success_with_latency(&self, id: u64, latency: Option<StdDuration>) {
        let mut last_used_at: Option<String> = None;
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
                let now = Utc::now().to_rfc3339();
                entry.last_used_at = Some(now.clone());
                entry.health.recent_error_rate *= 1.0 - alpha;
                entry.health.transient_failure_streak =
                    entry.health.transient_failure_streak.saturating_sub(1);
                if let Some(latency) = latency {
                    let latency_ms = latency.as_millis() as f64;
                    entry.health.latency_ewma_ms = Some(
                        entry
                            .health
                            .latency_ewma_ms
                            .map(|previous| previous + alpha * (latency_ms - previous))
                            .unwrap_or(latency_ms),
                    );
                }
                last_used_at = Some(now);
                tracing::debug!(
                    "凭据 #{} API 调用成功（累计 {} 次）",
                    id,
                    entry.success_count
                );
            }
        }
        if let Some(last_used_at) = last_used_at {
            self.persist_success_state(id, &last_used_at);
        }
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            match block_on_storage("记录 Redis 调度成功健康状态", async move {
                redis.record_scheduler_success(id, latency, alpha).await
            }) {
                Ok(health) => {
                    let mut entries = self.entries.lock();
                    if let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) {
                        entry.health = health;
                    }
                }
                Err(err) => tracing::warn!(
                    credential_id = id,
                    "记录 Redis 调度成功健康状态失败: {}",
                    err
                ),
            }
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

        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            let reason_for_redis = reason.clone();
            match block_on_storage("写入 Redis 凭据临时冷却与健康状态", async move {
                redis
                    .record_scheduler_transient_failure(
                        id,
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
                    .await
            }) {
                Ok((cooldown, health)) => {
                    let mut entries = self.entries.lock();
                    if let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) {
                        entry.cooldown_until = instant_from_epoch_ms(
                            cooldown.until_ms,
                            Utc::now().timestamp_millis(),
                            Instant::now(),
                        );
                        entry.cooldown_reason = cooldown.reason;
                        entry.health = health;
                        entry.last_used_at = Some(Utc::now().to_rfc3339());
                    }
                }
                Err(err) => return Err(err),
            }
        } else {
            let mut entries = self.entries.lock();
            if let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) {
                entry.health.transient_failure_streak =
                    entry.health.transient_failure_streak.saturating_add(1);
                entry.health.recent_error_rate += alpha * (1.0 - entry.health.recent_error_rate);
                entry.health.last_error_kind = Some(kind.as_str().to_string());
                entry.health.last_error_reason = Some(reason.clone());
                entry.health.last_error_at_ms = Some(now_ms);
                let duration = Self::local_cooldown_duration(
                    retry_after,
                    base,
                    max,
                    multiplier,
                    jitter,
                    entry.health.transient_failure_streak,
                );
                let until = now + duration;
                if entry.cooldown_until.is_none_or(|existing| until > existing) {
                    entry.cooldown_until = Some(until);
                    entry.cooldown_reason = Some(reason.clone());
                }
                entry.health.probation_until_ms =
                    Some(
                        entry.health.probation_until_ms.unwrap_or(0).max(
                            now_ms + duration.as_millis() as i64 + probation.as_millis() as i64,
                        ),
                    );
                entry.last_used_at = Some(Utc::now().to_rfc3339());
            }
        }

        let duration = {
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|entry| entry.id == id)
                .and_then(|entry| Self::entry_cooldown_remaining(entry, Instant::now()))
                .unwrap_or(base)
        };

        tracing::warn!(
            "凭据 #{} 因 {} 瞬态错误进入临时冷却 {} 秒: {}",
            id,
            kind.as_str(),
            duration.as_secs(),
            reason
        );

        let has_alternate = {
            let entries = self.entries.lock();
            let proxy_resources = self.proxy_resources.lock();
            let max_concurrent_requests = self.max_concurrent_requests();
            entries.iter().any(|e| {
                e.id != id
                    && Self::credential_is_dispatchable(
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
        self.report_success_for_session_with_latency(id, session_id, None);
    }

    pub fn report_success_for_session_with_latency(
        &self,
        id: u64,
        session_id: Option<&str>,
        latency: Option<StdDuration>,
    ) {
        self.report_success_with_latency(id, latency);
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
        let mut persisted = false;
        match self.persist_api_failure_state(id, &last_used_at) {
            Ok(Some(state)) => {
                persisted = true;
                self.apply_runtime_state_for(id, &state);
            }
            Ok(None) => {}
            Err(err) => tracing::warn!("记录凭据失败计数到 PgSQL 失败: {}", err),
        }
        let disabled = {
            let entries = self.entries.lock();
            entries.iter().any(|e| e.id == id && e.disabled)
        };
        if disabled {
            self.select_highest_priority();
            self.unbind_sessions_for_credential(id);
            self.clear_scheduler_state_for_credential(id, false);
            if !persisted {
                if let Err(err) = self.persist_credential_entry(id) {
                    tracing::warn!("失败禁用凭据后持久化凭据失败: {}", err);
                }
            }
        }
        if !persisted {
            self.save_runtime_state_for(id);
            self.persist_last_used_at(id, &last_used_at);
        }
        let result = {
            let entries = self.entries.lock();
            entries.iter().any(|e| !e.disabled)
        };
        self.publish_credentials_changed("credential_failure_reported");
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
        let mut persisted = false;
        match self.persist_disabled_state(
            id,
            DisabledReason::QuotaExceeded,
            Some(MAX_FAILURES_PER_CREDENTIAL),
            None,
            &last_used_at,
        ) {
            Ok(Some(state)) => {
                persisted = true;
                self.apply_runtime_state_for(id, &state);
            }
            Ok(None) => {}
            Err(err) => tracing::warn!("记录额度禁用状态到 PgSQL 失败: {}", err),
        }
        if !persisted {
            if let Err(err) = self.persist_credential_entry(id) {
                tracing::warn!("额度禁用凭据后持久化凭据失败: {}", err);
            }
            self.save_runtime_state_for(id);
            self.persist_last_used_at(id, &last_used_at);
        }
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
        let mut persisted = false;
        match self.persist_disabled_state(
            id,
            disabled_reason,
            Some(MAX_FAILURES_PER_CREDENTIAL),
            None,
            &last_used_at,
        ) {
            Ok(Some(state)) => {
                persisted = true;
                self.apply_runtime_state_for(id, &state);
            }
            Ok(None) => {}
            Err(err) => tracing::warn!("记录风控禁用状态到 PgSQL 失败: {}", err),
        }
        if !persisted {
            if let Err(err) = self.persist_credential_entry(id) {
                tracing::warn!("风控禁用凭据后持久化凭据失败: {}", err);
            }
            self.save_runtime_state_for(id);
            self.persist_last_used_at(id, &last_used_at);
        }
        if let Some(store) = &self.postgres_store {
            let store = store.clone();
            let event_reason = reason.event_reason().to_string();
            let detail_value = serde_json::json!({
                "reason": event_reason,
                "detail": detail,
            });
            if let Err(err) = block_on_storage("记录凭据风控事件到 PgSQL", async move {
                store
                    .record_credential_event(
                        Some(id),
                        "credential_risk_controlled",
                        Some(&event_reason),
                        detail_value,
                    )
                    .await
            }) {
                tracing::warn!("记录凭据风控事件失败: {}", err);
            }
        }
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
        let mut persisted = false;
        match self.persist_refresh_failure_state(id, &last_used_at) {
            Ok(Some(state)) => {
                persisted = true;
                self.apply_runtime_state_for(id, &state);
            }
            Ok(None) => {}
            Err(err) => tracing::warn!("记录凭据刷新失败计数到 PgSQL 失败: {}", err),
        }
        let disabled = {
            let entries = self.entries.lock();
            entries.iter().any(|e| e.id == id && e.disabled)
        };
        if disabled {
            self.select_highest_priority();
            self.unbind_sessions_for_credential(id);
            self.clear_scheduler_state_for_credential(id, false);
            if !persisted {
                if let Err(err) = self.persist_credential_entry(id) {
                    tracing::warn!("刷新失败禁用凭据后持久化凭据失败: {}", err);
                }
            }
        }
        if !persisted {
            self.save_runtime_state_for(id);
            self.persist_last_used_at(id, &last_used_at);
        }
        let result = {
            let entries = self.entries.lock();
            entries.iter().any(|e| !e.disabled)
        };
        self.publish_credentials_changed("credential_refresh_failure_reported");
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
        let mut persisted = false;
        match self.persist_disabled_state(
            id,
            DisabledReason::InvalidRefreshToken,
            None,
            None,
            &last_used_at,
        ) {
            Ok(Some(state)) => {
                persisted = true;
                self.apply_runtime_state_for(id, &state);
            }
            Ok(None) => {}
            Err(err) => tracing::warn!("记录 refreshToken 失效状态到 PgSQL 失败: {}", err),
        }
        if !persisted {
            if let Err(err) = self.persist_credential_entry(id) {
                tracing::warn!("refreshToken 失效禁用凭据后持久化凭据失败: {}", err);
            }
            self.save_runtime_state_for(id);
            self.persist_last_used_at(id, &last_used_at);
        }
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
        self.refresh_stats_from_postgres();
        self.refresh_scheduler_state_from_redis_best_effort();
        let global_capacity = self.global_capacity_state();
        let config = self.config.lock().clone();
        let mut entries = self.entries.lock();
        if self.redis_store.is_none() {
            Self::refresh_local_selection_windows_locked(&mut entries, Instant::now());
        }
        let current_id = *self.current_id.lock();
        let available = entries.iter().filter(|e| !e.disabled).count();
        let now = Instant::now();
        let now_ms = Utc::now().timestamp_millis();
        let max_concurrent_requests = self.max_concurrent_requests();
        let lease_max_age = self.in_flight_lease_max_age();
        let score_candidates: Vec<_> = {
            let proxy_resources = self.proxy_resources.lock();
            entries
                .iter()
                .filter(|entry| {
                    Self::credential_is_dispatchable(
                        &proxy_resources,
                        entry,
                        None,
                        now,
                        max_concurrent_requests,
                    )
                })
                .collect()
        };

        ManagerSnapshot {
            entries: entries
                .iter()
                .map(|e| {
                    let (effective_proxy_url, effective_proxy_source) =
                        self.effective_proxy_display(&e.credentials);
                    let proxy_resource_id = e.credentials.proxy_resource_id;
                    let proxy_resource_name = self.proxy_resource_name(proxy_resource_id);
                    let oldest_in_flight_age_secs = e
                        .in_flight_leases
                        .iter()
                        .map(|lease| now.saturating_duration_since(lease.acquired_at).as_secs())
                        .max()
                        .unwrap_or(0);
                    let newest_in_flight_idle_secs = e
                        .in_flight_leases
                        .iter()
                        .map(|lease| now.saturating_duration_since(lease.last_seen_at).as_secs())
                        .min()
                        .unwrap_or(0);
                    CredentialEntrySnapshot {
                        id: e.id,
                        created_at: e.credentials.created_at.clone(),
                        updated_at: e.credentials.updated_at.clone(),
                        priority: e.credentials.priority,
                        disabled: e.disabled,
                        failure_count: e.failure_count,
                        auth_method: if e.credentials.is_api_key_credential() {
                            Some("api_key".to_string())
                        } else {
                            e.credentials.auth_method.as_deref().map(|m| {
                                if m.eq_ignore_ascii_case("builder-id")
                                    || m.eq_ignore_ascii_case("iam")
                                {
                                    "idc".to_string()
                                } else {
                                    m.to_string()
                                }
                            })
                        },
                        has_profile_arn: e.credentials.profile_arn.is_some(),
                        expires_at: if e.credentials.is_api_key_credential() {
                            None // API Key 凭据本地不维护过期时间（服务端策略未知）
                        } else {
                            e.credentials.expires_at.clone()
                        },
                        refresh_token_hash: if e.credentials.is_api_key_credential() {
                            None
                        } else {
                            e.credentials.refresh_token.as_deref().map(sha256_hex)
                        },
                        api_key_hash: if e.credentials.is_api_key_credential() {
                            e.credentials.kiro_api_key.as_deref().map(sha256_hex)
                        } else {
                            None
                        },
                        masked_api_key: if e.credentials.is_api_key_credential() {
                            e.credentials.kiro_api_key.as_deref().map(mask_api_key)
                        } else {
                            None
                        },
                        email: e.credentials.email.clone(),
                        subscription_title: e.credentials.subscription_title.clone(),
                        success_count: e.success_count,
                        total_selection_count: e.total_selection_count,
                        last_used_at: e.last_used_at.clone(),
                        has_proxy: effective_proxy_url.is_some(),
                        proxy_url: e.credentials.proxy_url.clone(),
                        proxy_resource_id,
                        proxy_resource_name,
                        effective_proxy_url,
                        effective_proxy_source,
                        refresh_failure_count: e.refresh_failure_count,
                        disabled_reason: e.disabled_reason.map(|r| r.as_str().to_string()),
                        endpoint: e.credentials.endpoint.clone(),
                        cooled_down: Self::entry_cooldown_remaining(e, now).is_some(),
                        cooldown_remaining_secs: Self::entry_cooldown_remaining(e, now)
                            .map(|duration| duration.as_secs().saturating_add(1))
                            .unwrap_or(0),
                        cooldown_reason: if Self::entry_cooldown_remaining(e, now).is_some() {
                            e.cooldown_reason.clone()
                        } else {
                            None
                        },
                        rate_limited: Self::entry_rate_limit_remaining(e, now).is_some(),
                        rate_limit_remaining_secs: Self::entry_rate_limit_remaining(e, now)
                            .map(|duration| duration.as_secs().saturating_add(1))
                            .unwrap_or(0),
                        in_flight_requests: e.in_flight_requests,
                        oldest_in_flight_age_secs,
                        newest_in_flight_idle_secs,
                        max_concurrent_requests: Self::effective_max_concurrent_requests(
                            e,
                            max_concurrent_requests,
                        ),
                        max_concurrent_requests_override: e.credentials.max_concurrent_requests,
                        in_flight_lease_max_secs: lease_max_age
                            .map(|duration| duration.as_secs())
                            .unwrap_or(0),
                        warmup_remaining: e.warmup_remaining,
                        transient_failure_streak: e.health.transient_failure_streak,
                        recent_error_rate: e.health.recent_error_rate,
                        latency_ewma_ms: e.health.latency_ewma_ms,
                        last_error_kind: e.health.last_error_kind.clone(),
                        last_error_reason: e.health.last_error_reason.clone(),
                        last_error_at_ms: e.health.last_error_at_ms,
                        in_probation: e
                            .health
                            .probation_until_ms
                            .is_some_and(|until_ms| until_ms > now_ms),
                        probation_remaining_secs: e
                            .health
                            .probation_until_ms
                            .filter(|until_ms| *until_ms > now_ms)
                            .map(|until_ms| ((until_ms - now_ms) as u64).div_ceil(1000))
                            .unwrap_or(0),
                        scheduler_selection_count: e.total_selection_count,
                        recent_scheduler_selection_count_10s: e.health.recent_selection_count_10s,
                        recent_scheduler_selection_count_60s: e.health.recent_selection_count_60s,
                        recent_scheduler_selection_count_5m: e.health.recent_selection_count_5m,
                        scheduler_selection_pressure: Self::selection_pressure_for_candidates(
                            e,
                            &score_candidates,
                        ),
                        scheduler_score: self.scheduler_score(
                            e,
                            now_ms,
                            Self::selection_pressure_for_candidates(e, &score_candidates),
                        ),
                    }
                })
                .collect(),
            current_id,
            total: entries.len(),
            available,
            global_in_flight_requests: global_capacity.in_flight_requests,
            queued_requests: global_capacity.queued_requests,
            global_max_concurrent_requests: config.dispatch_global_max_concurrent_requests,
            max_queued_requests: config.dispatch_max_queued_requests,
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
                entry.rate_limit_available_at = None;
            } else {
                entry.disabled_reason = Some(DisabledReason::Manual);
                entry.cooldown_until = None;
                entry.cooldown_reason = None;
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
    pub async fn probe_usage_limits_for_credentials(
        &self,
        mut credentials: KiroCredentials,
    ) -> anyhow::Result<UsageLimitsResponse> {
        credentials.canonicalize_auth_method();
        let credentials = self.resolve_proxy_for_credential(credentials)?;
        let effective_proxy = credentials.effective_proxy(self.proxy.as_ref());
        let config = self.runtime_config();

        if credentials.is_api_key_credential() {
            let token = credentials
                .kiro_api_key
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("API Key 凭据缺少 kiroApiKey"))?;
            return get_usage_limits(&credentials, &config, token, effective_proxy.as_ref()).await;
        }

        let refreshed = refresh_token(&credentials, &config, effective_proxy.as_ref()).await?;
        let token = refreshed
            .access_token
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Token 刷新成功但未返回 accessToken"))?;
        get_usage_limits(&refreshed, &config, token, effective_proxy.as_ref()).await
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

        let warmup_remaining = self.runtime_config().credential_warmup_requests;
        let new_id = if let Some(store) = &self.postgres_store {
            validated_cred.disabled = false;
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
                disabled: false,
                disabled_reason: None,
                success_count: 0,
                total_selection_count: 0,
                last_used_at: None,
                cooldown_until: None,
                cooldown_reason: None,
                rate_limit_available_at: None,
                in_flight_requests: 0,
                in_flight_leases: Vec::new(),
                warmup_remaining,
                health: SchedulerHealthState::default(),
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

        // 获取刷新锁防止并发刷新
        let _guard = self.refresh_lock.lock().await;
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
    fn test_sha256_hex() {
        let result = sha256_hex("test");
        assert_eq!(
            result,
            "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
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
        manager.report_success_with_latency(1, Some(StdDuration::from_millis(120)));

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
