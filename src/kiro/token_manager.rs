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

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration as StdDuration, Instant};

use crate::http_client::{ProxyConfig, build_client};
use crate::kiro::machine_id;
use crate::kiro::model::credentials::KiroCredentials;
use crate::kiro::model::token_refresh::{
    IdcRefreshRequest, IdcRefreshResponse, RefreshRequest, RefreshResponse,
};
use crate::kiro::model::usage_limits::UsageLimitsResponse;
use crate::model::config::Config;

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
}

/// 统计数据持久化条目
#[derive(Serialize, Deserialize)]
struct StatsEntry {
    success_count: u64,
    last_used_at: Option<String>,
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
    /// API 调用成功次数
    pub success_count: u64,
    /// 最后一次 API 调用时间（RFC3339 格式）
    pub last_used_at: Option<String>,
    /// 是否配置了凭据级代理
    pub has_proxy: bool,
    /// 代理 URL（用于前端展示）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,
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
    /// 单凭据最大并发请求数。0 表示不限制。
    pub max_concurrent_requests: u32,
    /// 并发占用 lease 自动回收阈值。0 表示关闭自动回收。
    pub in_flight_lease_max_secs: u64,
    /// 预热剩余请求数。
    pub warmup_remaining: u32,
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
    /// 凭据文件路径（用于回写）
    credentials_path: Option<PathBuf>,
    /// 是否为多凭据格式（数组格式才回写）
    is_multiple_format: bool,
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
}

/// 每个凭据最大 API 调用失败次数
const MAX_FAILURES_PER_CREDENTIAL: u32 = 3;
/// 统计数据持久化防抖间隔
const STATS_SAVE_DEBOUNCE: StdDuration = StdDuration::from_secs(30);
/// 会话绑定最长保留时间，避免长期运行进程无限增长。
const SESSION_BINDING_TTL_SECS: i64 = 6 * 60 * 60;
/// 会话绑定表上限。
const MAX_SESSION_BINDINGS: usize = 10_000;
/// 同一会话绑定账号连续软失败达到该阈值后，本次请求允许临时 fallback。
const MAX_SESSION_SOFT_FAILURES: u32 = 2;
/// 并发排队等待的周期性唤醒间隔，避免极端竞态下丢失通知后永久睡眠。
const CONCURRENCY_WAIT_WAKEUP_SECS: u64 = 30;

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
        in_flight_notify: Arc<Notify>,
        credential_id: u64,
        lease_id: u64,
    ) -> Self {
        Self {
            entries,
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
        if release_in_flight_lease_from_entries(&self.entries, self.credential_id, self.lease_id) {
            self.in_flight_notify.notify_waiters();
        }
    }

    pub fn touch(&self) {
        touch_in_flight_lease_from_entries(&self.entries, self.credential_id, self.lease_id);
    }

    pub fn set_kind(&self, kind: InFlightKind) {
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
    /// * `credentials_path` - 凭据文件路径（用于回写）
    /// * `is_multiple_format` - 是否为多凭据格式（数组格式才回写）
    pub fn new(
        config: Config,
        credentials: Vec<KiroCredentials>,
        proxy: Option<ProxyConfig>,
        credentials_path: Option<PathBuf>,
        is_multiple_format: bool,
    ) -> anyhow::Result<Self> {
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
                    last_used_at: None,
                    cooldown_until: None,
                    cooldown_reason: None,
                    rate_limit_available_at: None,
                    in_flight_requests: 0,
                    in_flight_leases: Vec::new(),
                    warmup_remaining: 0,
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
        let manager = Self {
            config: Mutex::new(config),
            proxy,
            entries: Arc::new(Mutex::new(entries)),
            current_id: Mutex::new(initial_id),
            refresh_lock: TokioMutex::new(()),
            credentials_path,
            is_multiple_format,
            load_balancing_mode: Mutex::new(load_balancing_mode),
            last_stats_save_at: Mutex::new(None),
            stats_dirty: AtomicBool::new(false),
            session_bindings: Mutex::new(HashMap::new()),
            in_flight_notify: Arc::new(Notify::new()),
            next_in_flight_lease_id: AtomicU64::new(1),
        };

        // 如果有新分配的 ID 或新生成的 machineId，立即持久化到配置文件
        if has_new_ids || has_new_machine_ids {
            if let Err(e) = manager.persist_credentials() {
                tracing::warn!("补全凭据 ID/machineId 后持久化失败: {}", e);
            } else {
                tracing::info!("已补全凭据 ID/machineId 并写回配置文件");
            }
        }

        // 加载持久化的统计数据（success_count, last_used_at）
        manager.load_stats();

        Ok(manager)
    }

    /// 获取当前运行时配置快照。
    pub fn runtime_config(&self) -> Config {
        self.config.lock().clone()
    }

    /// 更新当前运行时配置并写回 config.json。
    pub fn update_runtime_config(&self, update: impl FnOnce(&mut Config)) -> anyhow::Result<()> {
        use anyhow::Context;

        let mut updated = self.runtime_config();
        update(&mut updated);
        let config_path = updated.config_path().map(|path| path.to_path_buf());

        let Some(config_path) = config_path else {
            {
                let mut config = self.config.lock();
                *config = updated;
            }
            self.update_credential_rpm_from_config();
            self.notify_dispatch_state_changed();
            tracing::warn!("配置文件路径未知，运行时配置仅在当前进程生效");
            return Ok(());
        };

        let mut persisted = Config::load(&config_path)
            .with_context(|| format!("重新加载配置失败: {}", config_path.display()))?;
        persisted.credential_rpm = updated.credential_rpm;
        persisted.credential_max_concurrent_requests = updated.credential_max_concurrent_requests;
        persisted.credential_transient_cooldown_secs = updated.credential_transient_cooldown_secs;
        persisted.credential_max_cooldown_secs = updated.credential_max_cooldown_secs;
        persisted.credential_dispatch_max_wait_secs = updated.credential_dispatch_max_wait_secs;
        persisted.credential_in_flight_lease_max_secs = updated.credential_in_flight_lease_max_secs;
        persisted.credential_warmup_requests = updated.credential_warmup_requests;
        persisted.credential_warmup_selection_percent = updated.credential_warmup_selection_percent;
        persisted.compression = updated.compression.clone();
        persisted.prompt_cache_target_read_ratio = updated.prompt_cache_target_read_ratio;
        persisted.prompt_cache_token_scale = updated.prompt_cache_token_scale;
        persisted.prompt_cache_max_simulated_input_tokens =
            updated.prompt_cache_max_simulated_input_tokens;
        persisted.prompt_cache_cap_jitter_min_tokens = updated.prompt_cache_cap_jitter_min_tokens;
        persisted.prompt_cache_cap_jitter_max_tokens = updated.prompt_cache_cap_jitter_max_tokens;
        persisted.prompt_cache_scale_min_input_tokens = updated.prompt_cache_scale_min_input_tokens;
        persisted.reported_usage = updated.reported_usage.clone();
        persisted.high_cache_threshold = updated.high_cache_threshold;
        persisted.compat_profile = updated.compat_profile;
        persisted.extract_thinking = updated.extract_thinking;
        persisted.expose_proxy_warnings = updated.expose_proxy_warnings;
        persisted
            .save()
            .with_context(|| format!("持久化运行时配置失败: {}", config_path.display()))?;

        {
            let mut config = self.config.lock();
            *config = updated;
        }
        self.update_credential_rpm_from_config();
        self.notify_dispatch_state_changed();

        Ok(())
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
        entry: &CredentialEntry,
        model: Option<&str>,
        now: Instant,
        max_concurrent_requests: u32,
    ) -> bool {
        Self::credential_is_usable_for_model(entry, model)
            && Self::entry_cooldown_remaining(entry, now).is_none()
            && Self::entry_rate_limit_remaining(entry, now).is_none()
            && Self::entry_has_concurrency_capacity(entry, max_concurrent_requests)
    }

    fn credential_is_temporarily_available(
        entry: &CredentialEntry,
        model: Option<&str>,
        now: Instant,
    ) -> bool {
        Self::credential_is_usable_for_model(entry, model)
            && Self::entry_cooldown_remaining(entry, now).is_none()
            && Self::entry_rate_limit_remaining(entry, now).is_none()
    }

    fn entry_has_concurrency_capacity(
        entry: &CredentialEntry,
        max_concurrent_requests: u32,
    ) -> bool {
        max_concurrent_requests == 0 || entry.in_flight_requests < max_concurrent_requests
    }

    fn max_concurrent_requests(&self) -> u32 {
        self.config.lock().credential_max_concurrent_requests
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

    fn cooldown_duration_from_retry_after(&self, retry_after: Option<StdDuration>) -> StdDuration {
        let config = self.config.lock();
        let fallback = StdDuration::from_secs(config.credential_transient_cooldown_secs);
        let max = StdDuration::from_secs(config.credential_max_cooldown_secs.max(1));
        let requested = retry_after.unwrap_or(fallback);
        requested.clamp(StdDuration::from_secs(1), max)
    }

    fn update_credential_rpm_from_config(&self) {
        if self.rate_limit_interval().is_none() {
            let mut entries = self.entries.lock();
            for entry in entries.iter_mut() {
                entry.rate_limit_available_at = None;
            }
        }
    }

    fn min_dispatch_wait(
        &self,
        entries: &[CredentialEntry],
        model: Option<&str>,
        excluded_ids: &HashSet<u64>,
        now: Instant,
    ) -> Option<StdDuration> {
        entries
            .iter()
            .filter(|entry| {
                !excluded_ids.contains(&entry.id)
                    && Self::credential_is_usable_for_model(entry, model)
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
        model: Option<&str>,
        excluded_ids: &HashSet<u64>,
        now: Instant,
        max_concurrent_requests: u32,
    ) -> usize {
        if max_concurrent_requests == 0 {
            return 0;
        }
        entries
            .iter()
            .filter(|entry| {
                !excluded_ids.contains(&entry.id)
                    && Self::credential_is_usable_for_model(entry, model)
                    && Self::entry_cooldown_remaining(entry, now).is_none()
                    && Self::entry_rate_limit_remaining(entry, now).is_none()
                    && !Self::entry_has_concurrency_capacity(entry, max_concurrent_requests)
            })
            .count()
    }

    fn mark_rate_limited_at(&self, id: u64, now: Instant) {
        let Some(interval) = self.rate_limit_interval() else {
            return;
        };
        let mut entries = self.entries.lock();
        if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
            let base = entry
                .rate_limit_available_at
                .filter(|at| *at > now)
                .unwrap_or(now);
            entry.rate_limit_available_at = Some(base + interval);
        }
    }

    fn acquire_in_flight_slot(&self, id: u64) -> Option<InFlightLeaseGuard> {
        self.cleanup_expired_in_flight_leases();
        let max_concurrent_requests = self.max_concurrent_requests();
        let lease_id = self.next_in_flight_lease_id.fetch_add(1, Ordering::Relaxed);
        let now = Instant::now();
        let mut entries = self.entries.lock();
        if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
            if !Self::entry_has_concurrency_capacity(entry, max_concurrent_requests) {
                return None;
            }
            entry.in_flight_requests = entry.in_flight_requests.saturating_add(1);
            entry.in_flight_leases.push(InFlightLease {
                id: lease_id,
                acquired_at: now,
                last_seen_at: now,
                kind: InFlightKind::Api,
            });
            return Some(InFlightLeaseGuard::new(
                self.entries.clone(),
                self.in_flight_notify.clone(),
                id,
                lease_id,
            ));
        }
        None
    }

    #[cfg(test)]
    pub(crate) fn acquire_in_flight_lease_for_test(&self, id: u64) -> Option<InFlightLeaseGuard> {
        self.acquire_in_flight_slot(id)
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

    pub fn cleanup_expired_in_flight_leases(&self) -> usize {
        let Some(max_age) = self.in_flight_lease_max_age() else {
            return 0;
        };
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
        cleaned
    }

    pub fn clear_in_flight_leases(
        &self,
        credential_id: u64,
        min_idle: Option<StdDuration>,
    ) -> usize {
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

    fn notify_dispatch_state_changed(&self) {
        self.in_flight_notify.notify_waiters();
    }

    async fn wait_for_dispatch_capacity(
        &self,
        wait_for: Option<StdDuration>,
        max_wait_remaining: Option<StdDuration>,
    ) {
        let fallback = StdDuration::from_secs(CONCURRENCY_WAIT_WAKEUP_SECS);
        let mut wakeup = wait_for.unwrap_or(fallback).min(fallback);
        if let Some(remaining) = max_wait_remaining {
            wakeup = wakeup.min(remaining);
        }
        if wakeup.is_zero() {
            return;
        }
        let _ = tokio::time::timeout(wakeup, self.in_flight_notify.notified()).await;
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
        let entries = self.entries.lock();
        let now = Instant::now();
        let max_concurrent_requests = self.max_concurrent_requests();
        entries.iter().any(|entry| {
            entry.id != current_id
                && !excluded_ids.contains(&entry.id)
                && Self::credential_is_dispatchable(entry, model, now, max_concurrent_requests)
        })
    }

    /// 根据负载均衡模式选择下一个凭据，并排除本次请求已临时失败的凭据。
    fn select_next_credential_excluding(
        &self,
        model: Option<&str>,
        excluded_ids: &HashSet<u64>,
    ) -> Option<(u64, KiroCredentials)> {
        let entries = self.entries.lock();
        let now = Instant::now();
        let max_concurrent_requests = self.max_concurrent_requests();

        // 过滤可用凭据
        let available: Vec<_> = entries
            .iter()
            .filter(|e| {
                !excluded_ids.contains(&e.id)
                    && Self::credential_is_dispatchable(e, model, now, max_concurrent_requests)
            })
            .collect();

        if available.is_empty() {
            return None;
        }

        let mode = self.load_balancing_mode.lock().clone();
        let mode = mode.as_str();

        match mode {
            "balanced" => {
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

                // 预热不是后台主动打流量，而是在真实业务请求中低概率参与调度；
                // 成功后才扣 warmup_remaining，且不伪造 success_count。
                if !ready.is_empty() && !warming.is_empty() {
                    let percent = self
                        .config
                        .lock()
                        .credential_warmup_selection_percent
                        .min(100);
                    if percent > 0 && fastrand::u32(0..100) < percent {
                        let entry = warming
                            .iter()
                            .min_by_key(|e| (e.success_count, e.credentials.priority))?;
                        return Some((entry.id, entry.credentials.clone()));
                    }

                    let entry = ready
                        .iter()
                        .min_by_key(|e| (e.success_count, e.credentials.priority))?;
                    return Some((entry.id, entry.credentials.clone()));
                }

                let candidates = if ready.is_empty() { warming } else { ready };
                let entry = candidates
                    .iter()
                    .min_by_key(|e| (e.success_count, e.credentials.priority))?;

                Some((entry.id, entry.credentials.clone()))
            }
            _ => {
                // priority 模式（默认）：选择优先级最高的
                let entry = available.iter().min_by_key(|e| e.credentials.priority)?;
                Some((entry.id, entry.credentials.clone()))
            }
        }
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
        let bound_id = {
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
        let now = Instant::now();
        let max_concurrent_requests = self.max_concurrent_requests();
        entries
            .iter()
            .find(|e| {
                e.id == bound_id
                    && Self::credential_is_dispatchable(e, model, now, max_concurrent_requests)
            })
            .map(|e| (e.id, e.credentials.clone()))
    }

    fn bound_credential_id(&self, session_id: &str) -> Option<u64> {
        self.session_bindings
            .lock()
            .get(session_id)
            .map(|binding| binding.credential_id)
    }

    fn bound_credential_exists_but_unusable(&self, session_id: &str, model: Option<&str>) -> bool {
        let Some(bound_id) = self.bound_credential_id(session_id) else {
            return false;
        };

        let entries = self.entries.lock();
        let now = Instant::now();
        entries
            .iter()
            .find(|e| e.id == bound_id)
            .is_none_or(|e| !Self::credential_is_temporarily_available(e, model, now))
    }

    fn bind_session_to_credential(&self, session_id: &str, credential_id: u64) {
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
        self.session_bindings.lock().remove(session_id);
    }

    /// 仅当指定会话当前绑定到该凭据时清理绑定。
    pub fn unbind_session_if_bound_to(&self, session_id: &str, credential_id: u64) {
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
        self.session_bindings
            .lock()
            .retain(|_, binding| binding.credential_id != credential_id);
    }

    /// 记录绑定账号的一次软失败。返回 true 表示本次请求可以临时 fallback。
    pub fn record_session_soft_failure(&self, session_id: &str, credential_id: u64) -> bool {
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

        loop {
            self.cleanup_expired_in_flight_leases();
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
                    let is_balanced = self.load_balancing_mode.lock().as_str() == "balanced";

                    // balanced 模式：新会话重新均衡选择；已有会话已在 bound_hit 返回
                    // priority 模式：优先使用 current_id 指向的凭据
                    let current_hit = if is_balanced {
                        None
                    } else {
                        let entries = self.entries.lock();
                        let current_id = *self.current_id.lock();
                        let now = Instant::now();
                        let max_concurrent_requests = self.max_concurrent_requests();
                        entries
                            .iter()
                            .find(|e| {
                                e.id == current_id
                                    && !excluded_ids.contains(&e.id)
                                    && Self::credential_is_dispatchable(
                                        e,
                                        model,
                                        now,
                                        max_concurrent_requests,
                                    )
                            })
                            .map(|e| (e.id, e.credentials.clone()))
                    };

                    if let Some(hit) = current_hit {
                        AcquireDecision::Selected(hit.0, hit.1, false, fallback_from_sticky)
                    } else {
                        // 当前凭据不可用或 balanced 模式，根据负载均衡策略选择
                        let mut best = self.select_next_credential_excluding(model, excluded_ids);

                        // 没有可用凭据：如果是"自动禁用导致全灭"，做一次类似重启的自愈
                        if best.is_none() {
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
                                    }
                                }
                                drop(entries);
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
                            // 注意：必须在 bail! 之前计算 available_count，
                            // 因为 available_count() 会尝试获取 entries 锁，
                            // 而此时我们已经持有该锁，会导致死锁
                            let available = entries.iter().filter(|e| !e.disabled).count();
                            let usable = entries
                                .iter()
                                .filter(|e| Self::credential_is_usable_for_model(e, model))
                                .count();
                            let dispatchable = {
                                let now = Instant::now();
                                let max_concurrent_requests = self.max_concurrent_requests();
                                entries
                                    .iter()
                                    .filter(|e| {
                                        !excluded_ids.contains(&e.id)
                                            && Self::credential_is_dispatchable(
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
                                let concurrency_blocked = self.concurrency_blocked_count(
                                    &entries,
                                    model,
                                    excluded_ids,
                                    now,
                                    max_concurrent_requests,
                                );
                                if concurrency_blocked > 0
                                    && concurrency_blocked
                                        >= entries
                                            .iter()
                                            .filter(|e| {
                                                !excluded_ids.contains(&e.id)
                                                    && Self::credential_is_usable_for_model(
                                                        e, model,
                                                    )
                                            })
                                            .count()
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
                                        wait_for: self.min_dispatch_wait(
                                            &entries,
                                            model,
                                            excluded_ids,
                                            now,
                                        ),
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

            let Some(in_flight_lease) = self.acquire_in_flight_slot(id) else {
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

            // 尝试获取/刷新 Token
            match self.try_ensure_token(id, &credentials, true).await {
                Ok(ctx) => {
                    self.mark_rate_limited_at(ctx.id, Instant::now());
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
            let token = credentials
                .kiro_api_key
                .clone()
                .ok_or_else(|| anyhow::anyhow!("API Key 凭据缺少 kiroApiKey"))?;
            return Ok(CallContext {
                id,
                credentials: credentials.clone(),
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
                // 确实需要刷新
                let effective_proxy = current_creds.effective_proxy(self.proxy.as_ref());
                let config = self.runtime_config();
                let new_creds =
                    refresh_token(&current_creds, &config, effective_proxy.as_ref()).await?;

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

                // 回写凭据到文件（仅多凭据格式），失败只记录警告
                if let Err(e) = self.persist_credentials() {
                    tracing::warn!("Token 刷新后持久化失败（不影响本次请求）: {}", e);
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

        let token = creds
            .access_token
            .clone()
            .ok_or_else(|| anyhow::anyhow!("没有可用的 accessToken"))?;

        if update_refresh_health {
            let mut entries = self.entries.lock();
            if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                entry.refresh_failure_count = 0;
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

    /// 将凭据列表回写到源文件
    ///
    /// 仅在以下条件满足时回写：
    /// - 源文件是多凭据格式（数组）
    /// - credentials_path 已设置
    ///
    /// # Returns
    /// - `Ok(true)` - 成功写入文件
    /// - `Ok(false)` - 跳过写入（非多凭据格式或无路径配置）
    /// - `Err(_)` - 写入失败
    fn persist_credentials(&self) -> anyhow::Result<bool> {
        use anyhow::Context;

        // 仅多凭据格式才回写
        if !self.runtime_config().credentials_persist || !self.is_multiple_format {
            return Ok(false);
        }

        let path = match &self.credentials_path {
            Some(p) => p,
            None => return Ok(false),
        };

        // 收集所有凭据
        let credentials: Vec<KiroCredentials> = {
            let entries = self.entries.lock();
            entries
                .iter()
                .map(|e| {
                    let mut cred = e.credentials.clone();
                    cred.canonicalize_auth_method();
                    // 同步 disabled 状态到凭据对象
                    cred.disabled = e.disabled;
                    cred
                })
                .collect()
        };

        // 序列化为 pretty JSON
        let json = serde_json::to_string_pretty(&credentials).context("序列化凭据失败")?;

        // 写入文件（在 Tokio runtime 内使用 block_in_place 避免阻塞 worker）
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::block_in_place(|| crate::common::fs::write_file_atomic(path, &json))
                .with_context(|| format!("回写凭据文件失败: {:?}", path))?;
        } else {
            crate::common::fs::write_file_atomic(path, &json)
                .with_context(|| format!("回写凭据文件失败: {:?}", path))?;
        }

        tracing::debug!("已回写凭据到文件: {:?}", path);
        Ok(true)
    }

    /// 获取缓存目录（凭据文件所在目录）
    pub fn cache_dir(&self) -> Option<PathBuf> {
        self.credentials_path
            .as_ref()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
    }

    /// 统计数据文件路径
    fn stats_path(&self) -> Option<PathBuf> {
        if !self.runtime_config().credential_stats_persist {
            return None;
        }
        self.cache_dir().map(|d| d.join("kiro_stats.json"))
    }

    /// 从磁盘加载统计数据并应用到当前条目
    fn load_stats(&self) {
        let path = match self.stats_path() {
            Some(p) => p,
            None => return,
        };

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return, // 首次运行时文件不存在
        };

        let stats: HashMap<String, StatsEntry> = match serde_json::from_str(&content) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("解析统计缓存失败，将忽略: {}", e);
                return;
            }
        };

        let mut entries = self.entries.lock();
        for entry in entries.iter_mut() {
            if let Some(s) = stats.get(&entry.id.to_string()) {
                entry.success_count = s.success_count;
                entry.last_used_at = s.last_used_at.clone();
            }
        }
        *self.last_stats_save_at.lock() = Some(Instant::now());
        self.stats_dirty.store(false, Ordering::Relaxed);
        tracing::info!("已从缓存加载 {} 条统计数据", stats.len());
    }

    /// 将当前统计数据持久化到磁盘
    fn save_stats(&self) {
        let path = match self.stats_path() {
            Some(p) => p,
            None => return,
        };

        let stats: HashMap<String, StatsEntry> = {
            let entries = self.entries.lock();
            entries
                .iter()
                .map(|e| {
                    (
                        e.id.to_string(),
                        StatsEntry {
                            success_count: e.success_count,
                            last_used_at: e.last_used_at.clone(),
                        },
                    )
                })
                .collect()
        };

        match serde_json::to_string_pretty(&stats) {
            Ok(json) => {
                if let Err(e) = crate::common::fs::write_file_atomic(&path, json) {
                    tracing::warn!("保存统计缓存失败: {}", e);
                } else {
                    *self.last_stats_save_at.lock() = Some(Instant::now());
                    self.stats_dirty.store(false, Ordering::Relaxed);
                }
            }
            Err(e) => tracing::warn!("序列化统计数据失败: {}", e),
        }
    }

    /// 标记统计数据已更新，并按 debounce 策略决定是否立即落盘
    fn save_stats_debounced(&self) {
        self.stats_dirty.store(true, Ordering::Relaxed);

        let should_flush = {
            let last = *self.last_stats_save_at.lock();
            match last {
                Some(last_saved_at) => last_saved_at.elapsed() >= STATS_SAVE_DEBOUNCE,
                None => true,
            }
        };

        if should_flush {
            self.save_stats();
        }
    }

    /// 报告指定凭据 API 调用成功
    ///
    /// 重置该凭据的失败计数
    ///
    /// # Arguments
    /// * `id` - 凭据 ID（来自 CallContext）
    pub fn report_success(&self, id: u64) {
        {
            let mut entries = self.entries.lock();
            if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                entry.failure_count = 0;
                entry.refresh_failure_count = 0;
                entry.cooldown_until = None;
                entry.cooldown_reason = None;
                if entry.warmup_remaining > 0 {
                    entry.warmup_remaining -= 1;
                }
                entry.success_count += 1;
                entry.last_used_at = Some(Utc::now().to_rfc3339());
                tracing::debug!(
                    "凭据 #{} API 调用成功（累计 {} 次）",
                    id,
                    entry.success_count
                );
            }
        }
        self.save_stats_debounced();
        self.notify_dispatch_state_changed();
    }

    /// 报告指定凭据遇到上游瞬态错误，按 Retry-After 或默认值临时冷却。
    ///
    /// 不增加 failure_count，不禁用凭据；冷却到期后自动重新参与调度。
    pub fn report_transient_failure(
        &self,
        id: u64,
        model: Option<&str>,
        retry_after: Option<StdDuration>,
        reason: impl Into<String>,
    ) -> bool {
        let reason = reason.into();
        let duration = self.cooldown_duration_from_retry_after(retry_after);
        let now = Instant::now();
        let until = now + duration;

        {
            let mut entries = self.entries.lock();
            let usable_count = entries
                .iter()
                .filter(|e| Self::credential_is_usable_for_model(e, model))
                .count();
            let Some(entry) = entries.iter_mut().find(|e| e.id == id) else {
                return entries.iter().any(|e| !e.disabled);
            };

            if entry.disabled {
                return entries.iter().any(|e| !e.disabled);
            }

            if usable_count <= 1 {
                entry.last_used_at = Some(Utc::now().to_rfc3339());
                tracing::warn!(
                    "凭据 #{} 遇到上游瞬态错误，但没有其他可用凭据；不进入本地临时冷却: {}",
                    id,
                    reason
                );
                return false;
            }

            entry.cooldown_until = Some(until);
            entry.cooldown_reason = Some(reason.clone());
            entry.last_used_at = Some(Utc::now().to_rfc3339());
        }

        tracing::warn!(
            "凭据 #{} 因上游瞬态错误进入临时冷却 {} 秒: {}",
            id,
            duration.as_secs(),
            reason
        );

        let entries = self.entries.lock();
        let max_concurrent_requests = self.max_concurrent_requests();
        entries.iter().any(|e| {
            e.id != id
                && Self::credential_is_dispatchable(
                    e,
                    model,
                    Instant::now(),
                    max_concurrent_requests,
                )
        })
    }

    /// 报告指定会话在该凭据上的 API 调用成功，并清理 sticky 软失败计数。
    pub fn report_success_for_session(&self, id: u64, session_id: Option<&str>) {
        self.report_success(id);
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

            entry.failure_count += 1;
            entry.last_used_at = Some(Utc::now().to_rfc3339());
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
        {
            let entries = self.entries.lock();
            if entries.iter().any(|e| e.id == id && e.disabled) {
                drop(entries);
                self.unbind_sessions_for_credential(id);
            }
        }
        self.save_stats_debounced();
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
            entry.last_used_at = Some(Utc::now().to_rfc3339());
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
        self.save_stats_debounced();
        self.notify_dispatch_state_changed();
        result
    }

    /// 报告指定凭据刷新 Token 失败。
    ///
    /// 连续刷新失败达到阈值后禁用凭据并切换，阈值内保持当前凭据不切换，
    /// 与 API 401/403 的累计失败策略保持一致。
    pub fn report_refresh_failure(&self, id: u64) -> bool {
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

            entry.last_used_at = Some(Utc::now().to_rfc3339());
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
        {
            let entries = self.entries.lock();
            if entries.iter().any(|e| e.id == id && e.disabled) {
                drop(entries);
                self.unbind_sessions_for_credential(id);
            }
        }
        self.save_stats_debounced();
        self.notify_dispatch_state_changed();
        result
    }

    /// 报告指定凭据的 refreshToken 永久失效（invalid_grant）。
    ///
    /// 立即禁用凭据，不累计、不重试。
    /// 返回是否还有可用凭据。
    pub fn report_refresh_token_invalid(&self, id: u64) -> bool {
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

            entry.last_used_at = Some(Utc::now().to_rfc3339());
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
        self.save_stats_debounced();
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
        let entries = self.entries.lock();
        let current_id = *self.current_id.lock();
        let available = entries.iter().filter(|e| !e.disabled).count();
        let now = Instant::now();
        let max_concurrent_requests = self.max_concurrent_requests();
        let lease_max_age = self.in_flight_lease_max_age();

        ManagerSnapshot {
            entries: entries
                .iter()
                .map(|e| {
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
                        success_count: e.success_count,
                        last_used_at: e.last_used_at.clone(),
                        has_proxy: e.credentials.proxy_url.is_some(),
                        proxy_url: e.credentials.proxy_url.clone(),
                        refresh_failure_count: e.refresh_failure_count,
                        disabled_reason: e.disabled_reason.map(|r| {
                            match r {
                                DisabledReason::Manual => "Manual",
                                DisabledReason::TooManyFailures => "TooManyFailures",
                                DisabledReason::TooManyRefreshFailures => "TooManyRefreshFailures",
                                DisabledReason::QuotaExceeded => "QuotaExceeded",
                                DisabledReason::InvalidRefreshToken => "InvalidRefreshToken",
                                DisabledReason::InvalidConfig => "InvalidConfig",
                            }
                            .to_string()
                        }),
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
                        max_concurrent_requests,
                        in_flight_lease_max_secs: lease_max_age
                            .map(|duration| duration.as_secs())
                            .unwrap_or(0),
                        warmup_remaining: e.warmup_remaining,
                    }
                })
                .collect(),
            current_id,
            total: entries.len(),
            available,
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
        } else {
            self.select_highest_priority();
        }
        self.notify_dispatch_state_changed();
        // 持久化更改
        self.persist_credentials()?;
        Ok(())
    }

    /// 设置凭据优先级（Admin API）
    ///
    /// 修改优先级后会立即按新优先级重新选择当前凭据。
    /// 即使持久化失败，内存中的优先级和当前凭据选择也会生效。
    pub fn set_priority(&self, id: u64, priority: u32) -> anyhow::Result<()> {
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
        // 持久化更改
        self.persist_credentials()?;
        Ok(())
    }

    /// 重置凭据失败计数并重新启用（Admin API）
    pub fn reset_and_enable(&self, id: u64) -> anyhow::Result<()> {
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
        self.notify_dispatch_state_changed();
        // 持久化更改
        self.persist_credentials()?;
        Ok(())
    }

    /// 获取指定凭据的使用额度（Admin API）
    pub async fn get_usage_limits_for(&self, id: u64) -> anyhow::Result<UsageLimitsResponse> {
        let credentials = {
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.credentials.clone())
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?
        };

        // API Key 凭据直接使用 kiro_api_key，无需刷新
        let token = if credentials.is_api_key_credential() {
            credentials
                .kiro_api_key
                .clone()
                .ok_or_else(|| anyhow::anyhow!("API Key 凭据缺少 kiroApiKey"))?
        } else {
            // 检查是否需要刷新 token
            let needs_refresh =
                is_token_expired(&credentials) || is_token_expiring_soon(&credentials);

            if needs_refresh {
                let _guard = self.refresh_lock.lock().await;
                let current_creds = {
                    let entries = self.entries.lock();
                    entries
                        .iter()
                        .find(|e| e.id == id)
                        .map(|e| e.credentials.clone())
                        .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?
                };

                if is_token_expired(&current_creds) || is_token_expiring_soon(&current_creds) {
                    let effective_proxy = current_creds.effective_proxy(self.proxy.as_ref());
                    let config = self.runtime_config();
                    let new_creds =
                        refresh_token(&current_creds, &config, effective_proxy.as_ref()).await?;
                    {
                        let mut entries = self.entries.lock();
                        if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                            entry.credentials = new_creds.clone();
                        }
                    }
                    // 持久化失败只记录警告，不影响本次请求
                    if let Err(e) = self.persist_credentials() {
                        tracing::warn!("Token 刷新后持久化失败（不影响本次请求）: {}", e);
                    }
                    new_creds
                        .access_token
                        .ok_or_else(|| anyhow::anyhow!("刷新后无 access_token"))?
                } else {
                    current_creds
                        .access_token
                        .ok_or_else(|| anyhow::anyhow!("凭据无 access_token"))?
                }
            } else {
                credentials
                    .access_token
                    .ok_or_else(|| anyhow::anyhow!("凭据无 access_token"))?
            }
        };

        let credentials = {
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.credentials.clone())
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?
        };

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
                if let Err(e) = self.persist_credentials() {
                    tracing::warn!("订阅等级更新后持久化失败（不影响本次请求）: {}", e);
                }
            }
        }

        Ok(usage_limits)
    }

    /// 添加新凭据（Admin API）
    ///
    /// # 流程
    /// 1. 验证凭据基本字段（API Key: kiroApiKey 不为空; OAuth: refreshToken 不为空）
    /// 2. 基于 kiroApiKey 或 refreshToken 的 SHA-256 哈希检测重复
    /// 3. OAuth: 尝试刷新 Token 验证凭据有效性; API Key: 跳过
    /// 4. 分配新 ID（当前最大 ID + 1）
    /// 5. 添加到 entries 列表
    /// 6. 持久化到配置文件
    ///
    /// # 返回
    /// - `Ok(u64)` - 新凭据 ID
    /// - `Err(_)` - 验证失败或添加失败
    pub async fn add_credential(&self, new_cred: KiroCredentials) -> anyhow::Result<u64> {
        // 1. 基本验证
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
            let effective_proxy = new_cred.effective_proxy(self.proxy.as_ref());
            let config = self.runtime_config();
            refresh_token(&new_cred, &config, effective_proxy.as_ref()).await?
        };

        // 4. 分配新 ID
        let new_id = {
            let entries = self.entries.lock();
            entries.iter().map(|e| e.id).max().unwrap_or(0) + 1
        };

        // 5. 设置 ID 并保留用户输入的元数据
        validated_cred.id = Some(new_id);
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
        validated_cred.kiro_api_key = new_cred.kiro_api_key;
        validated_cred.endpoint = new_cred.endpoint;

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
                last_used_at: None,
                cooldown_until: None,
                cooldown_reason: None,
                rate_limit_available_at: None,
                in_flight_requests: 0,
                in_flight_leases: Vec::new(),
                warmup_remaining: self.runtime_config().credential_warmup_requests,
            });
        }

        // 6. 持久化
        self.persist_credentials()?;
        self.notify_dispatch_state_changed();

        tracing::info!("成功添加凭据 #{}", new_id);
        Ok(new_id)
    }

    /// 设置凭据预热剩余请求数。0 表示关闭预热。
    pub fn set_warmup_remaining(&self, id: u64, warmup_remaining: u32) -> anyhow::Result<()> {
        {
            let mut entries = self.entries.lock();
            let entry = entries
                .iter_mut()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;
            entry.warmup_remaining = warmup_remaining;
        }
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
    /// 6. 持久化到文件
    ///
    /// # 返回
    /// - `Ok(())` - 删除成功
    /// - `Err(_)` - 凭据不存在、未禁用或持久化失败
    pub fn delete_credential(&self, id: u64) -> anyhow::Result<()> {
        let was_current = {
            let mut entries = self.entries.lock();

            // 查找凭据
            let entry = entries
                .iter()
                .find(|e| e.id == id)
                .ok_or_else(|| anyhow::anyhow!("凭据不存在: {}", id))?;

            // 检查是否已禁用
            if !entry.disabled {
                anyhow::bail!("只能删除已禁用的凭据（请先禁用凭据 #{}）", id);
            }

            // 记录是否是当前凭据
            let current_id = *self.current_id.lock();
            let was_current = current_id == id;

            // 删除凭据
            entries.retain(|e| e.id != id);

            was_current
        };

        // 如果删除的是当前凭据，切换到优先级最高的可用凭据
        if was_current {
            self.select_highest_priority();
        }
        self.unbind_sessions_for_credential(id);
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

        // 持久化更改
        self.persist_credentials()?;

        // 立即回写统计数据，清除已删除凭据的残留条目
        self.save_stats();

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

        // 无条件调用 refresh_token
        let effective_proxy = credentials.effective_proxy(self.proxy.as_ref());
        let config = self.runtime_config();
        let new_creds = refresh_token(&credentials, &config, effective_proxy.as_ref()).await?;

        // 更新 entries 中对应凭据
        {
            let mut entries = self.entries.lock();
            if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                entry.credentials = new_creds;
                entry.refresh_failure_count = 0;
            }
        }

        // 持久化
        if let Err(e) = self.persist_credentials() {
            tracing::warn!("强制刷新 Token 后持久化失败: {}", e);
        }

        tracing::info!("凭据 #{} Token 已强制刷新", id);
        Ok(())
    }

    /// 获取负载均衡模式（Admin API）
    pub fn get_load_balancing_mode(&self) -> String {
        self.load_balancing_mode.lock().clone()
    }

    fn persist_load_balancing_mode(&self, mode: &str) -> anyhow::Result<()> {
        use anyhow::Context;

        let config_path = match self.runtime_config().config_path() {
            Some(path) => path.to_path_buf(),
            None => {
                tracing::warn!("配置文件路径未知，负载均衡模式仅在当前进程生效: {}", mode);
                return Ok(());
            }
        };

        let mut config = Config::load(&config_path)
            .with_context(|| format!("重新加载配置失败: {}", config_path.display()))?;
        config.load_balancing_mode = mode.to_string();
        config
            .save()
            .with_context(|| format!("持久化负载均衡模式失败: {}", config_path.display()))?;

        Ok(())
    }

    /// 设置负载均衡模式（Admin API）
    pub fn set_load_balancing_mode(&self, mode: String) -> anyhow::Result<()> {
        // 验证模式值
        if mode != "priority" && mode != "balanced" {
            anyhow::bail!("无效的负载均衡模式: {}", mode);
        }

        let previous_mode = self.get_load_balancing_mode();
        if previous_mode == mode {
            return Ok(());
        }

        *self.load_balancing_mode.lock() = mode.clone();

        if self.runtime_config().credentials_persist {
            if let Err(err) = self.persist_load_balancing_mode(&mode) {
                *self.load_balancing_mode.lock() = previous_mode;
                return Err(err);
            }
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
    fn test_set_load_balancing_mode_persists_to_config_file() {
        let config_path =
            std::env::temp_dir().join(format!("kiro-load-balancing-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(&config_path, r#"{"loadBalancingMode":"priority"}"#).unwrap();

        let config = Config::load(&config_path).unwrap();
        let manager =
            MultiTokenManager::new(config, vec![KiroCredentials::default()], None, None, false)
                .unwrap();

        manager
            .set_load_balancing_mode("balanced".to_string())
            .unwrap();

        let persisted = Config::load(&config_path).unwrap();
        assert_eq!(persisted.load_balancing_mode, "balanced");
        assert_eq!(manager.get_load_balancing_mode(), "balanced");

        std::fs::remove_file(&config_path).unwrap();
    }

    #[test]
    fn test_set_load_balancing_mode_respects_disabled_persistence() {
        let config_path =
            std::env::temp_dir().join(format!("kiro-no-persist-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(
            &config_path,
            r#"{"loadBalancingMode":"priority","credentialsPersist":false}"#,
        )
        .unwrap();

        let config = Config::load(&config_path).unwrap();
        let manager =
            MultiTokenManager::new(config, vec![KiroCredentials::default()], None, None, false)
                .unwrap();

        manager
            .set_load_balancing_mode("balanced".to_string())
            .unwrap();

        let persisted = Config::load(&config_path).unwrap();
        assert_eq!(persisted.load_balancing_mode, "priority");
        assert_eq!(manager.get_load_balancing_mode(), "balanced");

        std::fs::remove_file(&config_path).unwrap();
    }

    #[test]
    fn test_update_runtime_config_persists_reported_usage_policy() {
        use crate::model::config::{
            ReportedUsageConfig, ReportedUsageFieldPolicy, ReportedUsagePathPolicy,
        };

        let config_path =
            std::env::temp_dir().join(format!("kiro-runtime-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(&config_path, r#"{"loadBalancingMode":"priority"}"#).unwrap();

        let config = Config::load(&config_path).unwrap();
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

        let persisted = Config::load(&config_path).unwrap();
        assert_eq!(persisted.credential_dispatch_max_wait_secs, 77);
        assert_eq!(
            persisted
                .reported_usage
                .policy_for_path("/custom/v1/messages")
                .input
                .max_tokens,
            42
        );
        assert_eq!(
            manager
                .runtime_config()
                .reported_usage
                .policy_for_path("/custom/v1/messages")
                .input
                .max_tokens,
            42
        );

        std::fs::remove_file(&config_path).unwrap();
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

        assert!(manager.report_transient_failure(1, None, Some(StdDuration::from_secs(20)), "429"));

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
    async fn test_transient_failure_does_not_cool_down_only_usable_credential() {
        let mut config = Config::default();
        config.credential_transient_cooldown_secs = 60;

        let mut disabled = KiroCredentials::default();
        disabled.disabled = true;
        let mut active = KiroCredentials::default();
        active.access_token = Some("active-token".to_string());
        active.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            MultiTokenManager::new(config, vec![disabled, active], None, None, false).unwrap();

        assert!(!manager.report_transient_failure(
            2,
            None,
            Some(StdDuration::from_secs(20)),
            "429"
        ));

        let snapshot = manager.snapshot();
        let active = snapshot.entries.iter().find(|entry| entry.id == 2).unwrap();
        assert!(!active.disabled);
        assert_eq!(active.failure_count, 0);
        assert!(!active.cooled_down);

        let mut ctx = manager.acquire_context(None).await.unwrap();
        assert_eq!(ctx.id, 2);
        ctx.release_in_flight();
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
    async fn test_transient_cooldown_waits_until_dispatchable() {
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
        assert!(manager.report_transient_failure(
            1,
            None,
            Some(StdDuration::from_millis(20)),
            "429"
        ));
        assert!(!manager.report_transient_failure(
            2,
            None,
            Some(StdDuration::from_millis(20)),
            "429"
        ));

        let started = Instant::now();
        let mut ctx = tokio::time::timeout(
            StdDuration::from_millis(1_500),
            manager.acquire_context(None),
        )
        .await
        .expect("临时冷却恢复后应继续调度")
        .expect("等待请求应成功获取凭据");

        assert!(started.elapsed() >= StdDuration::from_millis(900));
        ctx.release_in_flight();
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
