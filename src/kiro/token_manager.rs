//! Token 管理模块
//!
//! 负责 Token 过期检测和刷新，支持 Social 和 IdC 认证方式
//! 支持多凭据 (MultiTokenManager) 管理

use anyhow::bail;
use chrono::{DateTime, Duration, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex as TokioMutex;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
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
    /// 累计 402 (MONTHLY_REQUEST_COUNT) 命中次数
    /// 达到阈值才永久禁用,期间用 cooldown_until 做软冷却(自 v2026.4)
    quota_strike_count: u32,
    /// 软冷却到期时间;到期后会被自愈逻辑自动启用
    cooldown_until: Option<DateTime<Utc>>,
}

/// 会话到凭据的粘性绑定。
struct SessionBinding {
    credential_id: u64,
    last_used_at: DateTime<Utc>,
    soft_failure_count: u32,
}

/// Redis 不可用或测试场景下的进程内 429 冷却降级状态。
#[derive(Clone)]
struct RateLimitFallbackState {
    rate_limited_until: DateTime<Utc>,
    strike_count: u64,
    upstream_status: Option<u16>,
    reason: Option<String>,
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
    /// 额度已用尽（如 MONTHLY_REQUEST_COUNT）
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
    /// 调度状态：healthy / disabled / rate_limited / quota_cooldown。
    pub scheduling_status: String,
    /// 调度状态原因。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduling_reason: Option<String>,
    /// 调度状态到期时间（RFC3339）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduling_until: Option<String>,
    /// 最近一次上游状态码。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_upstream_status: Option<u16>,
    /// 429 冷却档位/次数。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limited_count: Option<u64>,
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
    config: Config,
    proxy: Option<ProxyConfig>,
    /// 凭据条目列表
    entries: Mutex<Vec<CredentialEntry>>,
    /// 当前活动凭据 ID
    current_id: Mutex<u64>,
    /// Token 刷新锁，确保同一时间只有一个刷新操作
    refresh_lock: TokioMutex<()>,
    /// 凭据文件路径（用于回写,作为 PG 落盘的热备份）
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
    /// PostgreSQL 句柄(自 v2026.4 持久化层)
    db: Option<crate::storage::Db>,
    /// Redis 句柄(预留,balance / session 等可重建状态用)
    redis: Option<crate::storage::RedisPool>,
    /// Redis 不可用时的进程内 429 冷却降级状态；不持久化、不跨进程。
    rate_limit_fallbacks: Mutex<HashMap<u64, RateLimitFallbackState>>,
    /// Redis 不可用时的进程内全池 429 退避状态；不持久化、不跨进程。
    global_rate_limit_fallback_until: Mutex<Option<DateTime<Utc>>>,
    /// 配额冷却参数:(strike_limit, cooldown_minutes),由 attach_storage 时从 app_config 读入
    quota_settings: Mutex<(u32, i64)>,
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
/// Redis 429 冷却 key 前缀。
const RATE_LIMIT_REDIS_PREFIX: &str = "kiro:sched:v1";
/// Kiro 429 无 reset 时的冷却档位（秒）。
const RATE_LIMIT_COOLDOWN_LEVELS_SECS: [i64; 5] = [45, 120, 300, 900, 1800];
/// 429 冷却最大值（秒）。
const RATE_LIMIT_MAX_COOLDOWN_SECS: i64 = 7200;
/// 调度状态 Redis TTL（秒）。
const SCHED_STATE_TTL_SECS: i64 = 7 * 24 * 60 * 60;
/// 全池 429 波动观测窗口（秒）。
const GLOBAL_RATE_LIMIT_WINDOW_SECS: i64 = 5 * 60;
/// 触发全池退避所需的最小账号数。
const GLOBAL_RATE_LIMIT_MIN_ACCOUNTS: usize = 3;
/// 触发全池退避的账号比例分子（默认 3/5 = 60%）。
const GLOBAL_RATE_LIMIT_RATIO_NUMERATOR: usize = 3;
/// 触发全池退避的账号比例分母（默认 3/5 = 60%）。
const GLOBAL_RATE_LIMIT_RATIO_DENOMINATOR: usize = 5;
/// 全池 429 退避最短秒数。
const GLOBAL_RATE_LIMIT_BACKOFF_MIN_SECS: i64 = 180;
/// 全池 429 退避最长秒数。
const GLOBAL_RATE_LIMIT_BACKOFF_MAX_SECS: i64 = 300;

#[derive(Debug, Clone, Default)]
struct SchedulerCredentialState {
    rate_limited_until_ms: Option<i64>,
    temp_unschedulable_until_ms: Option<i64>,
    manual_recovery_required: bool,
}

impl SchedulerCredentialState {
    fn is_schedulable_at(&self, now_ms: i64) -> bool {
        if self.manual_recovery_required {
            return false;
        }
        if self
            .rate_limited_until_ms
            .is_some_and(|until| now_ms < until)
        {
            return false;
        }
        if self
            .temp_unschedulable_until_ms
            .is_some_and(|until| now_ms < until)
        {
            return false;
        }
        true
    }
}

/// API 调用上下文
///
/// 绑定特定凭据的调用上下文，确保 token、credentials 和 id 的一致性
/// 用于解决并发调用时 current_id 竞态问题
#[derive(Clone)]
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
                    quota_strike_count: 0,
                    cooldown_until: None,
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
            config,
            proxy,
            entries: Mutex::new(entries),
            current_id: Mutex::new(initial_id),
            refresh_lock: TokioMutex::new(()),
            credentials_path,
            is_multiple_format,
            load_balancing_mode: Mutex::new(load_balancing_mode),
            last_stats_save_at: Mutex::new(None),
            stats_dirty: AtomicBool::new(false),
            session_bindings: Mutex::new(HashMap::new()),
            db: None,
            redis: None,
            rate_limit_fallbacks: Mutex::new(HashMap::new()),
            global_rate_limit_fallback_until: Mutex::new(None),
            quota_settings: Mutex::new((3, 30)),
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

    /// 获取配置的引用
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// 注入数据持久化层句柄(PG/Redis)。
    ///
    /// 启动期调用一次,启用 PG 持久化路径:
    /// - 若 PG `credentials` 表为空,把当前内存凭据 seed 进 PG(等价"首次迁移")
    /// - 若 PG 已有数据,以 PG 为准重载内存,文件视为热备
    /// - 任何 entries 修改之后异步双写 PG + 文件,失败仅打日志不阻塞
    pub async fn attach_storage(&mut self, storage: crate::storage::Storage) -> anyhow::Result<()> {
        use anyhow::Context;
        let row = sqlx::query("SELECT COUNT(*)::bigint AS n FROM credentials")
            .fetch_one(&storage.db)
            .await
            .context("查询 credentials 表行数失败")?;
        let count: i64 = sqlx::Row::try_get(&row, "n")?;

        if count == 0 {
            tracing::info!("PG credentials 表为空,从文件 seed 一次");
            seed_credentials_to_pg(&storage.db, &self.entries.lock()).await?;
            seed_stats_to_pg(&storage.db, &self.entries.lock()).await?;
        } else {
            tracing::info!("PG credentials 表已有 {} 条,以 PG 为准重载内存", count);
            let loaded = load_credentials_from_pg(&storage.db).await?;
            *self.entries.lock() = loaded;
            self.select_highest_priority();
        }

        self.db = Some(storage.db);
        self.redis = Some(storage.redis);
        self.hydrate_rate_limit_fallbacks_from_redis().await;
        Ok(())
    }

    async fn hydrate_rate_limit_fallbacks_from_redis(&self) {
        let Some(redis) = self.redis.clone() else {
            return;
        };
        let ids: Vec<u64> = self.entries.lock().iter().map(|entry| entry.id).collect();
        if ids.is_empty() {
            return;
        }

        let mut conn = match redis.get().await {
            Ok(conn) => conn,
            Err(err) => {
                tracing::warn!("启动期读取 Redis 429 冷却状态失败: {}", err);
                return;
            }
        };

        let mut pipe = redis::pipe();
        for id in &ids {
            pipe.cmd("HMGET").arg(Self::sched_state_key(*id)).arg(&[
                "rate_limited_until_ms",
                "rate_limit_level",
                "rate_limited_status",
                "rate_limited_reason",
            ]);
        }

        let values: redis::RedisResult<
            Vec<(Option<i64>, Option<u64>, Option<u16>, Option<String>)>,
        > = pipe.query_async(&mut *conn).await;
        let values = match values {
            Ok(values) => values,
            Err(err) => {
                tracing::warn!("启动期解析 Redis 429 冷却状态失败: {}", err);
                return;
            }
        };

        let now_ms = Utc::now().timestamp_millis();
        let mut hydrated = 0usize;
        let mut states = self.rate_limit_fallbacks.lock();
        states.clear();
        for (id, (until_ms, level, upstream_status, reason)) in ids.into_iter().zip(values) {
            let Some(until_ms) = until_ms else {
                continue;
            };
            if until_ms <= now_ms {
                continue;
            }
            let Some(rate_limited_until) = DateTime::<Utc>::from_timestamp_millis(until_ms) else {
                continue;
            };
            states.insert(
                id,
                RateLimitFallbackState {
                    rate_limited_until,
                    strike_count: level.unwrap_or(1).max(1),
                    upstream_status,
                    reason,
                },
            );
            hydrated += 1;
        }

        if hydrated > 0 {
            tracing::info!("已从 Redis 恢复 {} 个账号的 429 冷却状态", hydrated);
        }
    }

    /// 从外部覆盖当前进程的负载均衡模式(仅内存,不持久化到 config.json)
    /// 供 app_config 在线 PUT 时同步生效
    pub fn override_load_balancing_mode(&self, mode: &str) {
        if mode != "priority" && mode != "balanced" {
            tracing::warn!("无效负载均衡模式 '{}',忽略", mode);
            return;
        }
        let mut current = self.load_balancing_mode.lock();
        if *current == mode {
            return;
        }
        tracing::info!("负载均衡模式热更新: {} -> {}", *current, mode);
        *current = mode.to_string();
    }

    /// 应用 app_config 中的配额阈值。
    pub fn set_quota_settings(&self, strike_limit: u32, cooldown_minutes: i64) {
        let strike_limit = strike_limit.max(1);
        let cooldown_minutes = cooldown_minutes.max(1);
        *self.quota_settings.lock() = (strike_limit, cooldown_minutes);
        tracing::info!(
            "配额阈值已应用: strike_limit={} cooldown_minutes={}",
            strike_limit,
            cooldown_minutes
        );
    }

    fn sched_state_key(id: u64) -> String {
        format!("{RATE_LIMIT_REDIS_PREFIX}:cred:{id}:state")
    }

    fn sched_rate_limit_zset_key() -> &'static str {
        "kiro:sched:v1:cooldowns:rate_limit"
    }

    fn sched_event_stream_key() -> &'static str {
        "kiro:sched:v1:events"
    }

    fn sched_pool_rate_limit_accounts_key() -> &'static str {
        "kiro:sched:v1:pool:429_accounts"
    }

    fn sched_global_rate_limit_key() -> &'static str {
        "kiro:sched:v1:pool:global_backoff_until_ms"
    }

    fn rate_limit_cooldown_secs(level: i64) -> i64 {
        let base = RATE_LIMIT_COOLDOWN_LEVELS_SECS
            .get(level.saturating_sub(1).max(0) as usize)
            .copied()
            .unwrap_or(RATE_LIMIT_MAX_COOLDOWN_SECS);
        base.min(RATE_LIMIT_MAX_COOLDOWN_SECS)
    }

    fn jittered_cooldown_secs(base_secs: i64) -> i64 {
        let jitter = ((base_secs as f64) * 0.2).round() as i64;
        if jitter <= 0 {
            return base_secs.max(1);
        }
        let min = (base_secs - jitter).max(1);
        let max = (base_secs + jitter).max(min);
        fastrand::i64(min..=max)
    }

    fn global_rate_limit_threshold(pool_size: usize) -> usize {
        if pool_size == 0 {
            return GLOBAL_RATE_LIMIT_MIN_ACCOUNTS;
        }
        let ratio_threshold = pool_size
            .saturating_mul(GLOBAL_RATE_LIMIT_RATIO_NUMERATOR)
            .div_ceil(GLOBAL_RATE_LIMIT_RATIO_DENOMINATOR);
        GLOBAL_RATE_LIMIT_MIN_ACCOUNTS.max(ratio_threshold)
    }

    fn jittered_global_rate_limit_backoff_secs() -> i64 {
        fastrand::i64(GLOBAL_RATE_LIMIT_BACKOFF_MIN_SECS..=GLOBAL_RATE_LIMIT_BACKOFF_MAX_SECS)
    }

    fn usable_credential_count_for_global_backoff(&self) -> usize {
        self.entries
            .lock()
            .iter()
            .filter(|entry| !entry.disabled)
            .count()
    }

    fn maybe_write_global_rate_limit_fallback(&self) -> Option<i64> {
        let now = Utc::now();
        let pool_size = self.usable_credential_count_for_global_backoff();
        let threshold = Self::global_rate_limit_threshold(pool_size);
        let active_rate_limited = self
            .rate_limit_fallbacks
            .lock()
            .values()
            .filter(|state| state.rate_limited_until > now)
            .count();
        if active_rate_limited < threshold {
            return None;
        }

        let backoff_secs = Self::jittered_global_rate_limit_backoff_secs();
        let until = now + chrono::Duration::seconds(backoff_secs);
        let until_ms = until.timestamp_millis();
        *self.global_rate_limit_fallback_until.lock() = Some(until);
        tracing::warn!(
            "触发进程内全池 429 退避: active_rate_limited={} threshold={} pool_size={} backoff={}s until_ms={}",
            active_rate_limited,
            threshold,
            pool_size,
            backoff_secs,
            until_ms
        );
        Some(until_ms)
    }

    fn fallback_scheduler_state_for(&self, id: u64) -> SchedulerCredentialState {
        let states = self.rate_limit_fallbacks.lock();
        SchedulerCredentialState {
            rate_limited_until_ms: states
                .get(&id)
                .map(|state| state.rate_limited_until.timestamp_millis()),
            temp_unschedulable_until_ms: None,
            manual_recovery_required: false,
        }
    }

    fn write_rate_limit_fallback_state(
        &self,
        id: u64,
        upstream_status: Option<u16>,
        reason: Option<String>,
    ) -> (u64, i64, i64) {
        let mut states = self.rate_limit_fallbacks.lock();
        let strike_count = states
            .get(&id)
            .map(|state| state.strike_count.saturating_add(1))
            .unwrap_or(1);
        let cooldown_secs =
            Self::jittered_cooldown_secs(Self::rate_limit_cooldown_secs(strike_count as i64));
        let until = Utc::now() + chrono::Duration::seconds(cooldown_secs);
        let until_ms = until.timestamp_millis();
        states.insert(
            id,
            RateLimitFallbackState {
                rate_limited_until: until,
                strike_count,
                upstream_status,
                reason,
            },
        );
        (strike_count, cooldown_secs, until_ms)
    }

    fn mirror_rate_limit_fallback_state(
        &self,
        id: u64,
        strike_count: u64,
        until_ms: i64,
        upstream_status: Option<u16>,
        reason: Option<String>,
    ) {
        if let Some(until) = DateTime::<Utc>::from_timestamp_millis(until_ms) {
            self.rate_limit_fallbacks.lock().insert(
                id,
                RateLimitFallbackState {
                    rate_limited_until: until,
                    strike_count,
                    upstream_status,
                    reason,
                },
            );
        }
    }

    async fn scheduler_state_for(&self, id: u64) -> SchedulerCredentialState {
        self.scheduler_states_for(&[id])
            .await
            .remove(&id)
            .unwrap_or_default()
    }

    async fn scheduler_states_for(&self, ids: &[u64]) -> HashMap<u64, SchedulerCredentialState> {
        if ids.is_empty() {
            return HashMap::new();
        }

        let Some(redis) = self.redis.clone() else {
            return ids
                .iter()
                .map(|id| (*id, self.fallback_scheduler_state_for(*id)))
                .collect();
        };
        let mut conn = match redis.get().await {
            Ok(conn) => conn,
            Err(err) => {
                tracing::warn!("读取 Redis 调度状态失败，使用进程内降级状态: {}", err);
                return ids
                    .iter()
                    .map(|id| (*id, self.fallback_scheduler_state_for(*id)))
                    .collect();
            }
        };

        let mut pipe = redis::pipe();
        for id in ids {
            pipe.cmd("HMGET").arg(Self::sched_state_key(*id)).arg(&[
                "rate_limited_until_ms",
                "temp_unschedulable_until_ms",
                "manual_recovery_required",
            ]);
        }

        let values: redis::RedisResult<Vec<(Option<i64>, Option<i64>, Option<String>)>> =
            pipe.query_async(&mut *conn).await;

        match values {
            Ok(values) => ids
                .iter()
                .zip(values)
                .map(
                    |(id, (rate_limited_until_ms, temp_unschedulable_until_ms, manual))| {
                        let state = SchedulerCredentialState {
                            rate_limited_until_ms,
                            temp_unschedulable_until_ms,
                            manual_recovery_required: manual
                                .as_deref()
                                .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true")),
                        };
                        let fallback = self.fallback_scheduler_state_for(*id);
                        let now_ms = Utc::now().timestamp_millis();
                        let effective_state = if state.is_schedulable_at(now_ms)
                            && !fallback.is_schedulable_at(now_ms)
                        {
                            fallback
                        } else {
                            state
                        };
                        (*id, effective_state)
                    },
                )
                .collect(),
            Err(err) => {
                tracing::warn!("解析 Redis 调度状态失败，使用进程内降级状态: {}", err);
                ids.iter()
                    .map(|id| (*id, self.fallback_scheduler_state_for(*id)))
                    .collect()
            }
        }
    }

    async fn credential_is_schedulable_dynamic(&self, id: u64) -> bool {
        self.scheduler_state_for(id)
            .await
            .is_schedulable_at(Utc::now().timestamp_millis())
    }

    async fn maybe_write_global_rate_limit_backoff(
        &self,
        redis: &crate::storage::RedisPool,
        now_ms: i64,
    ) -> Option<i64> {
        let pool_size = self.usable_credential_count_for_global_backoff();
        let threshold = Self::global_rate_limit_threshold(pool_size);
        let window_start_ms = now_ms - GLOBAL_RATE_LIMIT_WINDOW_SECS.saturating_mul(1000);
        let mut conn = match redis.get().await {
            Ok(conn) => conn,
            Err(err) => {
                tracing::warn!("获取 Redis 连接失败，无法判断全池 429 退避: {}", err);
                return self.maybe_write_global_rate_limit_fallback();
            }
        };

        let cleanup: redis::RedisResult<()> = redis::pipe()
            .atomic()
            .cmd("ZREMRANGEBYSCORE")
            .arg(Self::sched_pool_rate_limit_accounts_key())
            .arg("-inf")
            .arg(window_start_ms - 1)
            .ignore()
            .cmd("EXPIRE")
            .arg(Self::sched_pool_rate_limit_accounts_key())
            .arg(GLOBAL_RATE_LIMIT_WINDOW_SECS * 2)
            .ignore()
            .query_async(&mut *conn)
            .await;
        if let Err(err) = cleanup {
            tracing::warn!("清理 Redis 全池 429 窗口失败: {}", err);
            return self.maybe_write_global_rate_limit_fallback();
        }

        let recent_count: usize = match redis::cmd("ZCOUNT")
            .arg(Self::sched_pool_rate_limit_accounts_key())
            .arg(window_start_ms)
            .arg("+inf")
            .query_async(&mut *conn)
            .await
        {
            Ok(count) => count,
            Err(err) => {
                tracing::warn!("统计 Redis 全池 429 窗口失败: {}", err);
                return self.maybe_write_global_rate_limit_fallback();
            }
        };
        if recent_count < threshold {
            return None;
        }

        let backoff_secs = Self::jittered_global_rate_limit_backoff_secs();
        let until_ms = now_ms + backoff_secs.saturating_mul(1000);
        let res: redis::RedisResult<()> = redis::pipe()
            .atomic()
            .cmd("SET")
            .arg(Self::sched_global_rate_limit_key())
            .arg(until_ms)
            .arg("PX")
            .arg(backoff_secs.saturating_mul(1000))
            .ignore()
            .cmd("XADD")
            .arg(Self::sched_event_stream_key())
            .arg("MAXLEN")
            .arg("~")
            .arg(10_000)
            .arg("*")
            .arg("kind")
            .arg("global_rate_limited")
            .arg("recent_429_accounts")
            .arg(recent_count)
            .arg("threshold")
            .arg(threshold)
            .arg("pool_size")
            .arg(pool_size)
            .arg("cooldown_until_ms")
            .arg(until_ms)
            .ignore()
            .query_async(&mut *conn)
            .await;
        if let Err(err) = res {
            tracing::warn!("写入 Redis 全池 429 退避失败: {}", err);
            return self.maybe_write_global_rate_limit_fallback();
        }

        if let Some(until) = DateTime::<Utc>::from_timestamp_millis(until_ms) {
            *self.global_rate_limit_fallback_until.lock() = Some(until);
        }
        tracing::warn!(
            "触发 Redis 全池 429 退避: recent_429_accounts={} threshold={} pool_size={} backoff={}s until_ms={}",
            recent_count,
            threshold,
            pool_size,
            backoff_secs,
            until_ms
        );
        Some(until_ms)
    }

    /// 记录一次 Kiro 429，并把账号放入 Redis 短冷却。
    ///
    /// 该状态只影响调度，不会把账号永久 disabled。
    pub async fn report_rate_limited(&self, id: u64, upstream_status: u16, reason: &str) {
        {
            let mut entries = self.entries.lock();
            if let Some(entry) = entries.iter_mut().find(|e| e.id == id) {
                entry.last_used_at = Some(Utc::now().to_rfc3339());
            }
        }

        let Some(redis) = self.redis.clone() else {
            let (level, cooldown_secs, until_ms) = self.write_rate_limit_fallback_state(
                id,
                Some(upstream_status),
                Some(reason.chars().take(512).collect()),
            );
            tracing::warn!(
                "Redis 未配置，使用进程内 429 冷却: credential_id={} status={} level={} cooldown={}s until_ms={}",
                id,
                upstream_status,
                level,
                cooldown_secs,
                until_ms
            );
            self.maybe_write_global_rate_limit_fallback();
            self.unbind_sessions_for_credential(id);
            return;
        };

        let key = Self::sched_state_key(id);
        let now_ms = Utc::now().timestamp_millis();
        let mut conn = match redis.get().await {
            Ok(conn) => conn,
            Err(err) => {
                tracing::warn!(
                    "获取 Redis 连接失败，降级为进程内 429 冷却: credential_id={} err={}",
                    id,
                    err
                );
                self.write_rate_limit_fallback_state(
                    id,
                    Some(upstream_status),
                    Some(reason.chars().take(512).collect()),
                );
                self.maybe_write_global_rate_limit_fallback();
                self.unbind_sessions_for_credential(id);
                return;
            }
        };

        let level: i64 = match redis::cmd("HINCRBY")
            .arg(&key)
            .arg("rate_limit_level")
            .arg(1_i64)
            .query_async(&mut *conn)
            .await
        {
            Ok(level) => level,
            Err(err) => {
                tracing::warn!(
                    "更新 429 冷却档位失败，降级为进程内冷却: credential_id={} err={}",
                    id,
                    err
                );
                self.write_rate_limit_fallback_state(
                    id,
                    Some(upstream_status),
                    Some(reason.chars().take(512).collect()),
                );
                self.maybe_write_global_rate_limit_fallback();
                self.unbind_sessions_for_credential(id);
                return;
            }
        };

        let base_secs = Self::rate_limit_cooldown_secs(level);
        let cooldown_secs = Self::jittered_cooldown_secs(base_secs);
        let until_ms = now_ms + cooldown_secs.saturating_mul(1000);
        let trimmed_reason: String = reason.chars().take(512).collect();

        let pipe_result: redis::RedisResult<()> = redis::pipe()
            .atomic()
            .cmd("HSET")
            .arg(&key)
            .arg("rate_limited_until_ms")
            .arg(until_ms)
            .arg("rate_limited_reason")
            .arg(&trimmed_reason)
            .arg("rate_limited_status")
            .arg(upstream_status as i64)
            .arg("last_rate_limited_at_ms")
            .arg(now_ms)
            .arg("manual_recovery_required")
            .arg("0")
            .ignore()
            .cmd("EXPIRE")
            .arg(&key)
            .arg(SCHED_STATE_TTL_SECS)
            .ignore()
            .cmd("ZADD")
            .arg(Self::sched_rate_limit_zset_key())
            .arg(until_ms)
            .arg(id)
            .ignore()
            .cmd("ZADD")
            .arg(Self::sched_pool_rate_limit_accounts_key())
            .arg(now_ms)
            .arg(id)
            .ignore()
            .cmd("XADD")
            .arg(Self::sched_event_stream_key())
            .arg("MAXLEN")
            .arg("~")
            .arg(10_000)
            .arg("*")
            .arg("kind")
            .arg("rate_limited")
            .arg("credential_id")
            .arg(id)
            .arg("status")
            .arg(upstream_status as i64)
            .arg("reason")
            .arg(&trimmed_reason)
            .arg("cooldown_until_ms")
            .arg(until_ms)
            .ignore()
            .query_async(&mut *conn)
            .await;

        if let Err(err) = pipe_result {
            tracing::warn!(
                "写入 429 冷却状态失败，降级为进程内冷却: credential_id={} err={}",
                id,
                err
            );
            self.write_rate_limit_fallback_state(
                id,
                Some(upstream_status),
                Some(trimmed_reason.clone()),
            );
            self.maybe_write_global_rate_limit_fallback();
            self.unbind_sessions_for_credential(id);
            return;
        }

        self.mirror_rate_limit_fallback_state(
            id,
            level.max(1) as u64,
            until_ms,
            Some(upstream_status),
            Some(trimmed_reason),
        );
        self.maybe_write_global_rate_limit_backoff(&redis, now_ms)
            .await;
        self.unbind_sessions_for_credential(id);
        tracing::warn!(
            "凭据 #{} 触发 429 冷却: level={} cooldown={}s until_ms={}",
            id,
            level,
            cooldown_secs,
            until_ms
        );
    }

    fn record_scheduler_success_async(&self, id: u64) {
        self.rate_limit_fallbacks.lock().remove(&id);
        *self.global_rate_limit_fallback_until.lock() = None;
        let Some(redis) = self.redis.clone() else {
            return;
        };
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        tokio::spawn(async move {
            let key = Self::sched_state_key(id);
            let now_ms = Utc::now().timestamp_millis();
            let mut conn = match redis.get().await {
                Ok(conn) => conn,
                Err(err) => {
                    tracing::warn!(
                        "获取 Redis 连接失败，跳过调度成功记录: credential_id={} err={}",
                        id,
                        err
                    );
                    return;
                }
            };

            let level: i64 = redis::cmd("HGET")
                .arg(&key)
                .arg("rate_limit_level")
                .query_async::<Option<i64>>(&mut *conn)
                .await
                .ok()
                .flatten()
                .unwrap_or(0);
            let next_level = level.saturating_sub(2).max(0);
            let res: redis::RedisResult<()> = redis::pipe()
                .atomic()
                .cmd("HSET")
                .arg(&key)
                .arg("rate_limit_level")
                .arg(next_level)
                .arg("last_success_at_ms")
                .arg(now_ms)
                .ignore()
                .cmd("HDEL")
                .arg(&key)
                .arg("rate_limited_until_ms")
                .arg("rate_limited_reason")
                .arg("rate_limited_status")
                .arg("last_rate_limited_at_ms")
                .ignore()
                .cmd("ZREM")
                .arg(Self::sched_rate_limit_zset_key())
                .arg(id)
                .ignore()
                .cmd("ZADD")
                .arg("kiro:sched:v1:pool:success_accounts")
                .arg(now_ms)
                .arg(id)
                .ignore()
                .cmd("DEL")
                .arg(Self::sched_global_rate_limit_key())
                .ignore()
                .query_async(&mut *conn)
                .await;
            if let Err(err) = res {
                tracing::warn!("清理 Redis 调度冷却失败: credential_id={} err={}", id, err);
            }
        });
    }

    /// 异步把当前内存凭据全量写回 PG。
    ///
    /// 通过 spawn 跑在后台,调用方不阻塞。任意失败只打 warn,不破坏内存状态。
    fn persist_credentials_to_pg_async(&self) {
        let Some(db) = self.db.clone() else {
            return;
        };
        let snapshot: Vec<PgCredentialRow> = self
            .entries
            .lock()
            .iter()
            .map(PgCredentialRow::from_entry)
            .collect();
        tokio::spawn(async move {
            if let Err(err) = upsert_credentials_to_pg(&db, &snapshot).await {
                tracing::warn!("PG 凭据持久化失败(非阻塞): {:#}", err);
            }
        });
    }

    /// 异步把统计字段写回 PG(success_count / failure_count / disabled / cooldown 等)。
    fn persist_stats_to_pg_async(&self) {
        let Some(db) = self.db.clone() else {
            return;
        };
        let snapshot: Vec<PgStatsRow> = self
            .entries
            .lock()
            .iter()
            .map(PgStatsRow::from_entry)
            .collect();
        tokio::spawn(async move {
            if let Err(err) = upsert_stats_to_pg(&db, &snapshot).await {
                tracing::warn!("PG 统计持久化失败(非阻塞): {:#}", err);
            }
        });
    }

    /// 写一条 quota_event 到 PG(soft_402 / hard_disabled / cooldown_recovered / manual_reset)
    fn record_quota_event_async(
        &self,
        credential_id: u64,
        kind: &'static str,
        reason: Option<String>,
        upstream_status: Option<i16>,
        cooldown_until: Option<DateTime<Utc>>,
    ) {
        let Some(db) = self.db.clone() else {
            return;
        };
        tokio::spawn(async move {
            let res = sqlx::query(
                "INSERT INTO quota_events (credential_id, kind, reason, upstream_status, cooldown_until) \
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(credential_id as i64)
            .bind(kind)
            .bind(reason)
            .bind(upstream_status)
            .bind(cooldown_until)
            .execute(&db)
            .await;
            if let Err(err) = res {
                tracing::warn!("写 quota_events 失败: {:#}", err);
            }
        });
    }

    /// 获取凭据总数
    pub fn total_count(&self) -> usize {
        self.entries.lock().len()
    }

    /// 获取可用凭据数量
    pub fn available_count(&self) -> usize {
        self.entries.lock().iter().filter(|e| !e.disabled).count()
    }

    pub async fn active_rate_limited_credential_ids(&self, model: Option<&str>) -> Vec<u64> {
        let candidate_ids: Vec<u64> = self
            .entries
            .lock()
            .iter()
            .filter(|entry| Self::credential_is_usable_for_model(entry, model))
            .map(|entry| entry.id)
            .collect();
        if candidate_ids.is_empty() {
            return Vec::new();
        }

        let states = self.scheduler_states_for(&candidate_ids).await;
        let now_ms = Utc::now().timestamp_millis();
        candidate_ids
            .into_iter()
            .filter(|id| {
                states
                    .get(id)
                    .is_some_and(|state| !state.is_schedulable_at(now_ms))
            })
            .collect()
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

    /// 根据负载均衡模式选择下一个凭据，并排除本次请求已临时失败的凭据。
    async fn select_next_credential_excluding(
        &self,
        model: Option<&str>,
        excluded_ids: &HashSet<u64>,
    ) -> Option<(u64, KiroCredentials)> {
        let candidates: Vec<_> = self
            .entries
            .lock()
            .iter()
            .filter(|e| {
                !excluded_ids.contains(&e.id) && Self::credential_is_usable_for_model(e, model)
            })
            .map(|e| {
                (
                    e.id,
                    e.credentials.clone(),
                    e.success_count,
                    e.credentials.priority,
                )
            })
            .collect();

        let candidate_ids: Vec<u64> = candidates.iter().map(|candidate| candidate.0).collect();
        let states = self.scheduler_states_for(&candidate_ids).await;
        let now_ms = Utc::now().timestamp_millis();
        let mut available = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            if states
                .get(&candidate.0)
                .is_none_or(|state| state.is_schedulable_at(now_ms))
            {
                available.push(candidate);
            }
        }

        if available.is_empty() {
            return None;
        }

        let mode = self.load_balancing_mode.lock().clone();
        let mode = mode.as_str();

        match mode {
            "balanced" => {
                // Least-Used 策略：选择成功次数最少的凭据
                // 平局时按优先级排序（数字越小优先级越高）
                let entry = available.iter().min_by_key(|e| (e.2, e.3))?;
                Some((entry.0, entry.1.clone()))
            }
            _ => {
                // priority 模式（默认）：选择优先级最高的
                let entry = available.iter().min_by_key(|e| e.3)?;
                Some((entry.0, entry.1.clone()))
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

    async fn get_bound_credential(
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

        let hit = self
            .entries
            .lock()
            .iter()
            .find(|e| e.id == bound_id && Self::credential_is_usable_for_model(e, model))
            .map(|e| (e.id, e.credentials.clone()));

        if let Some((id, credentials)) = hit {
            if self.credential_is_schedulable_dynamic(id).await {
                return Some((id, credentials));
            }
            self.unbind_session_if_bound_to(session_id, id);
        }

        None
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
        entries
            .iter()
            .find(|e| e.id == bound_id)
            .is_none_or(|e| !Self::credential_is_usable_for_model(e, model))
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
    #[allow(dead_code)]
    pub async fn acquire_context(&self, model: Option<&str>) -> anyhow::Result<CallContext> {
        self.acquire_context_for_session(model, None, &HashSet::new())
            .await
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
        // 入口先做一次配额冷却自愈,把过期的软冷却凭据放回池
        self.recover_quota_cooldowns();
        let total = self.total_count();
        let max_attempts = (total * MAX_FAILURES_PER_CREDENTIAL as usize).max(1);
        let mut attempt_count = 0;

        loop {
            if attempt_count >= max_attempts {
                anyhow::bail!(
                    "所有凭据均无法获取有效 Token（可用: {}/{}）",
                    self.available_count(),
                    total
                );
            }

            let (id, credentials, sticky_bound, fallback_from_sticky) = {
                let existing_bound_id = session_id.and_then(|sid| self.bound_credential_id(sid));
                let bound_hit = match session_id {
                    Some(sid) => self.get_bound_credential(sid, model, excluded_ids).await,
                    None => None,
                };

                if let Some(hit) = bound_hit {
                    (hit.0, hit.1, true, false)
                } else {
                    let fallback_from_sticky = existing_bound_id.is_some();
                    let is_balanced = self.load_balancing_mode.lock().as_str() == "balanced";

                    // balanced 模式：新会话重新均衡选择；已有会话已在 bound_hit 返回
                    // priority 模式：优先使用 current_id 指向的凭据
                    let current_hit = if is_balanced {
                        None
                    } else {
                        let current_id = *self.current_id.lock();
                        let candidate = self
                            .entries
                            .lock()
                            .iter()
                            .find(|e| {
                                e.id == current_id
                                    && !excluded_ids.contains(&e.id)
                                    && Self::credential_is_usable_for_model(e, model)
                            })
                            .map(|e| (e.id, e.credentials.clone()));
                        match candidate {
                            Some(hit) if self.credential_is_schedulable_dynamic(hit.0).await => {
                                Some(hit)
                            }
                            _ => None,
                        }
                    };

                    if let Some(hit) = current_hit {
                        (hit.0, hit.1, false, fallback_from_sticky)
                    } else {
                        // 当前凭据不可用或 balanced 模式，根据负载均衡策略选择
                        let mut best = self
                            .select_next_credential_excluding(model, excluded_ids)
                            .await;

                        // 没有可用凭据：如果是"自动禁用导致全灭"，做一次类似重启的自愈
                        if best.is_none() {
                            let recovered = {
                                let mut entries = self.entries.lock();
                                if entries.iter().any(|e| {
                                    e.disabled
                                        && e.disabled_reason
                                            == Some(DisabledReason::TooManyFailures)
                                }) {
                                    tracing::warn!(
                                        "所有凭据均已被自动禁用，执行自愈：重置失败计数并重新启用（等价于重启）"
                                    );
                                    for e in entries.iter_mut() {
                                        if e.disabled_reason
                                            == Some(DisabledReason::TooManyFailures)
                                        {
                                            e.disabled = false;
                                            e.disabled_reason = None;
                                            e.failure_count = 0;
                                        }
                                    }
                                    true
                                } else {
                                    false
                                }
                            };
                            if recovered {
                                best = self
                                    .select_next_credential_excluding(model, excluded_ids)
                                    .await;
                            }
                        }

                        if let Some((new_id, new_creds)) = best {
                            // 更新 current_id
                            let mut current_id = self.current_id.lock();
                            *current_id = new_id;
                            (new_id, new_creds, false, fallback_from_sticky)
                        } else {
                            let entries = self.entries.lock();
                            // 注意：必须在 bail! 之前计算 available_count，
                            // 因为 available_count() 会尝试获取 entries 锁，
                            // 而此时我们已经持有该锁，会导致死锁
                            let available = entries
                                .iter()
                                .filter(|e| Self::credential_is_usable_for_model(e, model))
                                .count();
                            if available > 0 {
                                if !excluded_ids.is_empty() {
                                    anyhow::bail!(
                                        "当前请求已尝试并排除所有可用凭据（{}/{} 可用，已排除 {} 个）",
                                        available,
                                        total,
                                        excluded_ids.len()
                                    );
                                }
                                anyhow::bail!(
                                    "所有可用凭据当前均不可调度（{}/{} 可用，可能处于 429 冷却）",
                                    available,
                                    total
                                );
                            }
                            anyhow::bail!("所有凭据均已禁用（{}/{}）", available, total);
                        }
                    }
                }
            };

            // 尝试获取/刷新 Token
            match self.try_ensure_token(id, &credentials).await {
                Ok(ctx) => {
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
                let new_creds =
                    refresh_token(&current_creds, &self.config, effective_proxy.as_ref()).await?;

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

        {
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
        })
    }

    /// 为 Admin 单凭据测试获取调用上下文。
    ///
    /// 该方法只按 ID 取凭据并保证 token 可用，不参与 current_id、负载均衡、
    /// sticky 会话或 429 冷却过滤，因此可以准确测试用户选中的那一张凭据。
    pub async fn acquire_context_for_credential(&self, id: u64) -> anyhow::Result<CallContext> {
        let credentials = {
            let entries = self.entries.lock();
            entries
                .iter()
                .find(|entry| entry.id == id)
                .map(|entry| entry.credentials.clone())
                .ok_or_else(|| anyhow::anyhow!("凭据 #{} 不存在", id))?
        };

        match self.try_ensure_token(id, &credentials).await {
            Ok(ctx) => Ok(ctx),
            Err(err) => {
                if err.downcast_ref::<RefreshTokenInvalidError>().is_some() {
                    tracing::warn!("凭据 #{} 测试时 refreshToken 永久失效: {}", id, err);
                    self.report_refresh_token_invalid(id);
                } else {
                    tracing::warn!("凭据 #{} 测试时 Token 刷新失败: {}", id, err);
                    self.report_refresh_failure(id);
                }
                Err(err)
            }
        }
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

        // 1) 异步写 PG(若已 attach_storage)
        self.persist_credentials_to_pg_async();

        // 2) 仅多凭据格式才回写文件(保留作为热备)
        if !self.is_multiple_format {
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
            tokio::task::block_in_place(|| std::fs::write(path, &json))
                .with_context(|| format!("回写凭据文件失败: {:?}", path))?;
        } else {
            std::fs::write(path, &json).with_context(|| format!("回写凭据文件失败: {:?}", path))?;
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
        // 异步双写 PG
        self.persist_stats_to_pg_async();

        let path = match self.stats_path() {
            Some(p) => p,
            None => {
                // 没有文件路径但有 PG 时,标记为已落盘
                if self.db.is_some() {
                    *self.last_stats_save_at.lock() = Some(Instant::now());
                    self.stats_dirty.store(false, Ordering::Relaxed);
                }
                return;
            }
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
                if let Err(e) = std::fs::write(&path, json) {
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
                entry.success_count += 1;
                entry.last_used_at = Some(Utc::now().to_rfc3339());
                // 配额连续成功一次即衰减 strike,避免软超限永久累积
                if entry.quota_strike_count > 0 {
                    entry.quota_strike_count -= 1;
                }
                entry.cooldown_until = None;
                tracing::debug!("凭据 #{} API 调用成功(累计 {} 次)", id, entry.success_count);
            }
        }
        self.record_scheduler_success_async(id);
        self.save_stats_debounced();
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
        result
    }

    /// 报告指定凭据额度已用尽(402 MONTHLY_REQUEST_COUNT)。
    ///
    /// **三次冷却策略(自 v2026.4)**:
    /// - 第 1、2 次:进入软冷却(30 分钟,可配置 `quota_cooldown_minutes`),
    ///   暂时禁用并切换;到期后自愈逻辑会自动启用,这样可以榨干"软超限"窗口。
    /// - 第 N 次(默认 3,可配置 `quota_soft_fail_limit`):永久禁用,标记 QuotaExceeded。
    pub fn report_quota_exhausted(&self, id: u64) -> bool {
        let (strike_limit, cooldown_minutes) = *self.quota_settings.lock();

        let emitted_event: Option<(&'static str, Option<DateTime<Utc>>)>;
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

            entry.quota_strike_count = entry.quota_strike_count.saturating_add(1);
            entry.last_used_at = Some(Utc::now().to_rfc3339());

            if entry.quota_strike_count >= strike_limit {
                // 永久禁用
                entry.disabled = true;
                entry.disabled_reason = Some(DisabledReason::QuotaExceeded);
                entry.failure_count = MAX_FAILURES_PER_CREDENTIAL;
                entry.cooldown_until = None;
                tracing::error!(
                    "凭据 #{} 累计 {} 次 402 已达阈值,永久禁用(QuotaExceeded)",
                    id,
                    entry.quota_strike_count
                );
                emitted_event = Some(("hard_disabled", None));
            } else {
                // 软冷却
                let until = Utc::now() + chrono::Duration::minutes(cooldown_minutes);
                entry.disabled = true;
                entry.disabled_reason = Some(DisabledReason::QuotaExceeded);
                entry.cooldown_until = Some(until);
                tracing::warn!(
                    "凭据 #{} 触发 402(累计 {}/{}),进入冷却至 {}",
                    id,
                    entry.quota_strike_count,
                    strike_limit,
                    until
                );
                emitted_event = Some(("soft_402", Some(until)));
            }

            // 切换到优先级最高的可用凭据
            if let Some(next) = entries
                .iter()
                .filter(|e| !e.disabled)
                .min_by_key(|e| e.credentials.priority)
            {
                *current_id = next.id;
                tracing::info!(
                    "已切换到凭据 #{}(优先级 {})",
                    next.id,
                    next.credentials.priority
                );
                true
            } else {
                tracing::error!("所有凭据均已禁用!");
                false
            }
        };
        if let Some((kind, cooldown)) = emitted_event {
            self.record_quota_event_async(
                id,
                kind,
                Some("MONTHLY_REQUEST_COUNT".to_string()),
                Some(402i16),
                cooldown,
            );
        }
        self.unbind_sessions_for_credential(id);
        self.save_stats_debounced();
        result
    }

    /// 自愈:把已过冷却时间的 QuotaExceeded 凭据重新启用。
    ///
    /// 调用点:`acquire_context_for_session` 入口,每次选择凭据前先扫描一遍。
    /// 不会改变永久禁用(strike 已达阈值)的凭据。
    fn recover_quota_cooldowns(&self) {
        let now = Utc::now();
        let mut recovered_ids: Vec<u64> = Vec::new();
        let mut entries = self.entries.lock();
        for entry in entries.iter_mut() {
            if entry.disabled && entry.disabled_reason == Some(DisabledReason::QuotaExceeded) {
                if let Some(until) = entry.cooldown_until {
                    if now >= until {
                        entry.disabled = false;
                        entry.disabled_reason = None;
                        entry.cooldown_until = None;
                        entry.failure_count = 0;
                        tracing::info!(
                            "凭据 #{} 配额冷却结束,自动恢复(累计 strike={})",
                            entry.id,
                            entry.quota_strike_count
                        );
                        recovered_ids.push(entry.id);
                    }
                }
            }
        }
        drop(entries);
        for id in &recovered_ids {
            self.record_quota_event_async(*id, "cooldown_recovered", None, None, None);
        }
        if !recovered_ids.is_empty() {
            self.save_stats_debounced();
        }
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
        let now = Utc::now();
        let rate_limit_fallbacks = self.rate_limit_fallbacks.lock().clone();

        ManagerSnapshot {
            entries: entries
                .iter()
                .map(|e| {
                    let rate_limit_state = rate_limit_fallbacks.get(&e.id);
                    let active_rate_limit =
                        rate_limit_state.filter(|state| state.rate_limited_until > now);
                    let active_quota_cooldown = e.cooldown_until.filter(|until| *until > now);
                    let (scheduling_status, scheduling_reason, scheduling_until) = if e.disabled {
                        (
                            "disabled".to_string(),
                            disabled_reason_to_str(e.disabled_reason),
                            e.cooldown_until.map(|until| until.to_rfc3339()),
                        )
                    } else if let Some(until) = active_quota_cooldown {
                        (
                            "quota_cooldown".to_string(),
                            Some("MONTHLY_REQUEST_COUNT".to_string()),
                            Some(until.to_rfc3339()),
                        )
                    } else if let Some(state) = active_rate_limit {
                        (
                            "rate_limited".to_string(),
                            state.reason.clone().or_else(|| Some("429".to_string())),
                            Some(state.rate_limited_until.to_rfc3339()),
                        )
                    } else {
                        ("healthy".to_string(), None, None)
                    };

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
                        scheduling_status,
                        scheduling_reason,
                        scheduling_until,
                        last_upstream_status: rate_limit_state
                            .and_then(|state| state.upstream_status),
                        rate_limited_count: rate_limit_state.map(|state| state.strike_count),
                    }
                })
                .collect(),
            current_id,
            total: entries.len(),
            available,
        }
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
            } else {
                entry.disabled_reason = Some(DisabledReason::Manual);
            }
        }
        if disabled {
            self.unbind_sessions_for_credential(id);
        } else {
            self.select_highest_priority();
        }
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
        }
        self.select_highest_priority();
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
                    let new_creds =
                        refresh_token(&current_creds, &self.config, effective_proxy.as_ref())
                            .await?;
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
        let usage_limits =
            get_usage_limits(&credentials, &self.config, &token, effective_proxy.as_ref()).await?;

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
            refresh_token(&new_cred, &self.config, effective_proxy.as_ref()).await?
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
                quota_strike_count: 0,
                cooldown_until: None,
            });
        }

        // 6. 持久化
        self.persist_credentials()?;

        tracing::info!("成功添加凭据 #{}", new_id);
        Ok(new_id)
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
        let new_creds = refresh_token(&credentials, &self.config, effective_proxy.as_ref()).await?;

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

        let config_path = match self.config.config_path() {
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

        if let Err(err) = self.persist_load_balancing_mode(&mode) {
            *self.load_balancing_mode.lock() = previous_mode;
            return Err(err);
        }

        tracing::info!("负载均衡模式已设置为: {}", mode);
        Ok(())
    }
}

// ============================================================================
// PG 持久化辅助(自 v2026.4)
// ============================================================================

/// 用于 upsert credentials 表的一行投影
struct PgCredentialRow {
    id: i64,
    auth_method: String,
    email: Option<String>,
    machine_id: Option<String>,
    profile_arn: Option<String>,
    subscription_title: Option<String>,
    endpoint: String,
    refresh_token: Option<String>,
    access_token: Option<String>,
    expires_at: Option<DateTime<Utc>>,
    client_id: Option<String>,
    client_secret: Option<String>,
    auth_region: Option<String>,
    api_region: Option<String>,
    kiro_api_key: Option<String>,
    proxy_url: Option<String>,
    proxy_username: Option<String>,
    proxy_password: Option<String>,
    refresh_token_hash: Option<String>,
    api_key_hash: Option<String>,
    priority: i32,
    disabled: bool,
    disabled_reason: Option<String>,
    failure_count: i32,
    refresh_failure_count: i32,
    success_count: i64,
    last_used_at: Option<DateTime<Utc>>,
    quota_strike_count: i32,
    cooldown_until: Option<DateTime<Utc>>,
}

/// 用于 update 统计字段的一行投影
struct PgStatsRow {
    id: i64,
    success_count: i64,
    failure_count: i32,
    refresh_failure_count: i32,
    disabled: bool,
    disabled_reason: Option<String>,
    last_used_at: Option<DateTime<Utc>>,
    quota_strike_count: i32,
    cooldown_until: Option<DateTime<Utc>>,
}

fn parse_rfc3339_to_utc(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn disabled_reason_to_str(reason: Option<DisabledReason>) -> Option<String> {
    reason.map(|r| match r {
        DisabledReason::Manual => "manual".to_string(),
        DisabledReason::TooManyFailures => "too_many_failures".to_string(),
        DisabledReason::TooManyRefreshFailures => "too_many_refresh_failures".to_string(),
        DisabledReason::QuotaExceeded => "quota_exceeded".to_string(),
        DisabledReason::InvalidRefreshToken => "invalid_refresh_token".to_string(),
        DisabledReason::InvalidConfig => "invalid_config".to_string(),
    })
}

fn disabled_reason_from_str(value: Option<String>) -> Option<DisabledReason> {
    value.as_deref().and_then(|s| match s {
        "manual" => Some(DisabledReason::Manual),
        "too_many_failures" => Some(DisabledReason::TooManyFailures),
        "too_many_refresh_failures" => Some(DisabledReason::TooManyRefreshFailures),
        "quota_exceeded" => Some(DisabledReason::QuotaExceeded),
        "invalid_refresh_token" => Some(DisabledReason::InvalidRefreshToken),
        "invalid_config" => Some(DisabledReason::InvalidConfig),
        _ => None,
    })
}

impl PgCredentialRow {
    fn from_entry(e: &CredentialEntry) -> Self {
        let cred = &e.credentials;
        let token_hash = cred
            .refresh_token
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(|t| {
                use sha2::{Digest, Sha256};
                hex::encode(Sha256::digest(t.as_bytes()))
            });
        let api_key_hash = cred
            .kiro_api_key
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(|t| {
                use sha2::{Digest, Sha256};
                hex::encode(Sha256::digest(t.as_bytes()))
            });
        Self {
            id: e.id as i64,
            auth_method: cred
                .auth_method
                .clone()
                .unwrap_or_else(|| "social".to_string()),
            email: cred.email.clone(),
            machine_id: cred.machine_id.clone(),
            profile_arn: cred.profile_arn.clone(),
            subscription_title: cred.subscription_title.clone(),
            endpoint: cred.endpoint.clone().unwrap_or_else(|| "ide".to_string()),
            refresh_token: cred.refresh_token.clone(),
            access_token: cred.access_token.clone(),
            expires_at: cred.expires_at.as_deref().and_then(parse_rfc3339_to_utc),
            client_id: cred.client_id.clone(),
            client_secret: cred.client_secret.clone(),
            auth_region: cred.auth_region.clone(),
            api_region: cred.api_region.clone(),
            kiro_api_key: cred.kiro_api_key.clone(),
            proxy_url: cred.proxy_url.clone(),
            proxy_username: cred.proxy_username.clone(),
            proxy_password: cred.proxy_password.clone(),
            refresh_token_hash: token_hash,
            api_key_hash,
            priority: cred.priority as i32,
            disabled: e.disabled,
            disabled_reason: disabled_reason_to_str(e.disabled_reason),
            failure_count: e.failure_count as i32,
            refresh_failure_count: e.refresh_failure_count as i32,
            success_count: e.success_count as i64,
            last_used_at: e.last_used_at.as_deref().and_then(parse_rfc3339_to_utc),
            quota_strike_count: e.quota_strike_count as i32,
            cooldown_until: e.cooldown_until,
        }
    }
}

impl PgStatsRow {
    fn from_entry(e: &CredentialEntry) -> Self {
        Self {
            id: e.id as i64,
            success_count: e.success_count as i64,
            failure_count: e.failure_count as i32,
            refresh_failure_count: e.refresh_failure_count as i32,
            disabled: e.disabled,
            disabled_reason: disabled_reason_to_str(e.disabled_reason),
            last_used_at: e.last_used_at.as_deref().and_then(parse_rfc3339_to_utc),
            quota_strike_count: e.quota_strike_count as i32,
            cooldown_until: e.cooldown_until,
        }
    }
}

async fn upsert_credentials_to_pg(
    db: &crate::storage::Db,
    rows: &[PgCredentialRow],
) -> anyhow::Result<()> {
    use anyhow::Context;
    let mut tx = db.begin().await?;
    for r in rows {
        sqlx::query(
            "INSERT INTO credentials (id, auth_method, email, machine_id, profile_arn, \
                subscription_title, endpoint, refresh_token, access_token, expires_at, \
                client_id, client_secret, auth_region, api_region, kiro_api_key, \
                proxy_url, proxy_username, proxy_password, \
                refresh_token_hash, api_key_hash, priority, \
                disabled, disabled_reason, failure_count, refresh_failure_count, \
                success_count, last_used_at, quota_strike_count, cooldown_until, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24,$25,$26,$27,$28,$29,NOW()) \
             ON CONFLICT (id) DO UPDATE SET \
                auth_method = EXCLUDED.auth_method, \
                email = EXCLUDED.email, \
                machine_id = EXCLUDED.machine_id, \
                profile_arn = EXCLUDED.profile_arn, \
                subscription_title = EXCLUDED.subscription_title, \
                endpoint = EXCLUDED.endpoint, \
                refresh_token = EXCLUDED.refresh_token, \
                access_token = EXCLUDED.access_token, \
                expires_at = EXCLUDED.expires_at, \
                client_id = EXCLUDED.client_id, \
                client_secret = EXCLUDED.client_secret, \
                auth_region = EXCLUDED.auth_region, \
                api_region = EXCLUDED.api_region, \
                kiro_api_key = EXCLUDED.kiro_api_key, \
                proxy_url = EXCLUDED.proxy_url, \
                proxy_username = EXCLUDED.proxy_username, \
                proxy_password = EXCLUDED.proxy_password, \
                refresh_token_hash = EXCLUDED.refresh_token_hash, \
                api_key_hash = EXCLUDED.api_key_hash, \
                priority = EXCLUDED.priority, \
                disabled = EXCLUDED.disabled, \
                disabled_reason = EXCLUDED.disabled_reason, \
                failure_count = EXCLUDED.failure_count, \
                refresh_failure_count = EXCLUDED.refresh_failure_count, \
                success_count = EXCLUDED.success_count, \
                last_used_at = EXCLUDED.last_used_at, \
                quota_strike_count = EXCLUDED.quota_strike_count, \
                cooldown_until = EXCLUDED.cooldown_until, \
                updated_at = NOW()",
        )
        .bind(r.id)
        .bind(&r.auth_method)
        .bind(&r.email)
        .bind(&r.machine_id)
        .bind(&r.profile_arn)
        .bind(&r.subscription_title)
        .bind(&r.endpoint)
        .bind(&r.refresh_token)
        .bind(&r.access_token)
        .bind(r.expires_at)
        .bind(&r.client_id)
        .bind(&r.client_secret)
        .bind(&r.auth_region)
        .bind(&r.api_region)
        .bind(&r.kiro_api_key)
        .bind(&r.proxy_url)
        .bind(&r.proxy_username)
        .bind(&r.proxy_password)
        .bind(&r.refresh_token_hash)
        .bind(&r.api_key_hash)
        .bind(r.priority)
        .bind(r.disabled)
        .bind(&r.disabled_reason)
        .bind(r.failure_count)
        .bind(r.refresh_failure_count)
        .bind(r.success_count)
        .bind(r.last_used_at)
        .bind(r.quota_strike_count)
        .bind(r.cooldown_until)
        .execute(&mut *tx)
        .await
        .with_context(|| format!("upsert credentials[{}] 失败", r.id))?;
    }
    tx.commit().await?;
    Ok(())
}

async fn upsert_stats_to_pg(db: &crate::storage::Db, rows: &[PgStatsRow]) -> anyhow::Result<()> {
    let mut tx = db.begin().await?;
    for r in rows {
        sqlx::query(
            "UPDATE credentials SET \
                success_count = $2, \
                failure_count = $3, \
                refresh_failure_count = $4, \
                disabled = $5, \
                disabled_reason = $6, \
                last_used_at = $7, \
                quota_strike_count = $8, \
                cooldown_until = $9, \
                updated_at = NOW() \
             WHERE id = $1",
        )
        .bind(r.id)
        .bind(r.success_count)
        .bind(r.failure_count)
        .bind(r.refresh_failure_count)
        .bind(r.disabled)
        .bind(&r.disabled_reason)
        .bind(r.last_used_at)
        .bind(r.quota_strike_count)
        .bind(r.cooldown_until)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn seed_credentials_to_pg(
    db: &crate::storage::Db,
    entries: &[CredentialEntry],
) -> anyhow::Result<()> {
    let rows: Vec<PgCredentialRow> = entries.iter().map(PgCredentialRow::from_entry).collect();
    upsert_credentials_to_pg(db, &rows).await
}

async fn seed_stats_to_pg(
    db: &crate::storage::Db,
    entries: &[CredentialEntry],
) -> anyhow::Result<()> {
    let rows: Vec<PgStatsRow> = entries.iter().map(PgStatsRow::from_entry).collect();
    upsert_stats_to_pg(db, &rows).await
}

async fn load_credentials_from_pg(db: &crate::storage::Db) -> anyhow::Result<Vec<CredentialEntry>> {
    use anyhow::Context;
    let rows = sqlx::query(
        "SELECT id, auth_method, email, machine_id, profile_arn, subscription_title, endpoint, \
                refresh_token, access_token, expires_at, client_id, client_secret, \
                auth_region, api_region, kiro_api_key, proxy_url, proxy_username, proxy_password, \
                priority, disabled, disabled_reason, failure_count, refresh_failure_count, \
                success_count, last_used_at, quota_strike_count, cooldown_until \
         FROM credentials ORDER BY priority, id",
    )
    .fetch_all(db)
    .await
    .context("查询 credentials 失败")?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        use sqlx::Row;
        let id: i64 = row.try_get("id")?;
        let auth_method: String = row.try_get("auth_method")?;
        let email: Option<String> = row.try_get("email").ok();
        let machine_id: Option<String> = row.try_get("machine_id").ok();
        let profile_arn: Option<String> = row.try_get("profile_arn").ok();
        let subscription_title: Option<String> = row.try_get("subscription_title").ok();
        let endpoint: String = row.try_get("endpoint")?;
        let refresh_token: Option<String> = row.try_get("refresh_token").ok();
        let access_token: Option<String> = row.try_get("access_token").ok();
        let expires_at: Option<DateTime<Utc>> = row.try_get("expires_at").ok();
        let client_id: Option<String> = row.try_get("client_id").ok();
        let client_secret: Option<String> = row.try_get("client_secret").ok();
        let auth_region: Option<String> = row.try_get("auth_region").ok();
        let api_region: Option<String> = row.try_get("api_region").ok();
        let kiro_api_key: Option<String> = row.try_get("kiro_api_key").ok();
        let proxy_url: Option<String> = row.try_get("proxy_url").ok();
        let proxy_username: Option<String> = row.try_get("proxy_username").ok();
        let proxy_password: Option<String> = row.try_get("proxy_password").ok();
        let priority: i32 = row.try_get("priority")?;
        let disabled: bool = row.try_get("disabled")?;
        let disabled_reason: Option<String> = row.try_get("disabled_reason").ok();
        let failure_count: i32 = row.try_get("failure_count")?;
        let refresh_failure_count: i32 = row.try_get("refresh_failure_count")?;
        let success_count: i64 = row.try_get("success_count")?;
        let last_used_at: Option<DateTime<Utc>> = row.try_get("last_used_at").ok();
        let quota_strike_count: i32 = row.try_get("quota_strike_count").unwrap_or(0);
        let cooldown_until: Option<DateTime<Utc>> = row.try_get("cooldown_until").ok();

        let credentials = KiroCredentials {
            id: Some(id as u64),
            access_token,
            refresh_token,
            profile_arn,
            expires_at: expires_at.map(|dt| dt.to_rfc3339()),
            auth_method: Some(auth_method),
            client_id,
            client_secret,
            priority: priority.max(0) as u32,
            region: None,
            auth_region,
            api_region,
            machine_id,
            email,
            subscription_title,
            proxy_url,
            proxy_username,
            proxy_password,
            disabled,
            kiro_api_key,
            endpoint: Some(endpoint),
        };

        out.push(CredentialEntry {
            id: id as u64,
            credentials,
            failure_count: failure_count.max(0) as u32,
            refresh_failure_count: refresh_failure_count.max(0) as u32,
            disabled,
            disabled_reason: disabled_reason_from_str(disabled_reason),
            success_count: success_count.max(0) as u64,
            last_used_at: last_used_at.map(|dt| dt.to_rfc3339()),
            quota_strike_count: quota_strike_count.max(0) as u32,
            cooldown_until,
        });
    }
    Ok(out)
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
        let ctx = manager.acquire_context(None).await.unwrap();
        assert!(ctx.token == "t1" || ctx.token == "t2");
        assert_eq!(manager.available_count(), 2);
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

        let ctx = manager.acquire_context(None).await.unwrap();
        assert_eq!(ctx.id, 2);
        assert_eq!(ctx.token, "good-token");
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
    async fn test_rate_limited_bound_session_is_unbound_and_skipped() {
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
            .acquire_context_for_session(None, Some("session-d"), &empty)
            .await
            .unwrap();
        manager.report_success_for_session(bound.id, Some("session-d"));

        manager
            .report_rate_limited(bound.id, 429, "too many requests")
            .await;

        let fallback = manager
            .acquire_context_for_session(None, Some("session-d"), &empty)
            .await
            .unwrap();

        assert_ne!(bound.id, fallback.id);
        assert!(!manager.credential_is_schedulable_dynamic(bound.id).await);
    }

    #[tokio::test]
    async fn test_global_rate_limit_wave_does_not_block_healthy_credentials() {
        let credentials: Vec<KiroCredentials> = (0..5)
            .map(|idx| {
                let mut cred = KiroCredentials::default();
                cred.access_token = Some(format!("t{}", idx + 1));
                cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
                cred
            })
            .collect();

        let manager =
            MultiTokenManager::new(Config::default(), credentials, None, None, false).unwrap();

        for id in 1..=3 {
            manager
                .report_rate_limited(id, 429, "too many requests")
                .await;
        }

        let ctx = manager.acquire_context(None).await.unwrap();
        assert!(
            ctx.id == 4 || ctx.id == 5,
            "全局 429 波动保护不应阻断仍健康的账号，实际选中 #{}",
            ctx.id
        );
        assert_eq!(manager.available_count(), 5);
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
    fn test_rate_limit_cooldown_levels_are_bounded() {
        assert_eq!(MultiTokenManager::rate_limit_cooldown_secs(1), 45);
        assert_eq!(MultiTokenManager::rate_limit_cooldown_secs(2), 120);
        assert_eq!(MultiTokenManager::rate_limit_cooldown_secs(3), 300);
        assert_eq!(MultiTokenManager::rate_limit_cooldown_secs(4), 900);
        assert_eq!(MultiTokenManager::rate_limit_cooldown_secs(5), 1800);
        assert_eq!(
            MultiTokenManager::rate_limit_cooldown_secs(99),
            RATE_LIMIT_MAX_COOLDOWN_SECS
        );
    }

    #[tokio::test]
    async fn test_rate_limited_credential_is_skipped_without_disabling() {
        let mut cred1 = KiroCredentials::default();
        cred1.access_token = Some("t1".to_string());
        cred1.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let mut cred2 = KiroCredentials::default();
        cred2.access_token = Some("t2".to_string());
        cred2.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            MultiTokenManager::new(Config::default(), vec![cred1, cred2], None, None, false)
                .unwrap();

        manager
            .report_rate_limited(1, 429, "too many requests")
            .await;

        let snapshot = manager.snapshot();
        let first = snapshot.entries.iter().find(|e| e.id == 1).unwrap();
        assert!(!first.disabled);
        assert_eq!(first.scheduling_status, "rate_limited");
        assert!(first.scheduling_until.is_some());
        assert_eq!(first.last_upstream_status, Some(429));
        assert_eq!(first.rate_limited_count, Some(1));
        assert_eq!(manager.available_count(), 2);

        let ctx = manager.acquire_context(None).await.unwrap();
        assert_eq!(ctx.id, 2);
        assert_eq!(ctx.token, "t2");
    }

    #[tokio::test]
    async fn test_all_rate_limited_credentials_are_unavailable_but_not_disabled() {
        let mut cred = KiroCredentials::default();
        cred.access_token = Some("t1".to_string());
        cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            MultiTokenManager::new(Config::default(), vec![cred], None, None, false).unwrap();

        manager
            .report_rate_limited(1, 429, "too many requests")
            .await;

        let err = manager
            .acquire_context(None)
            .await
            .err()
            .unwrap()
            .to_string();
        assert!(
            err.contains("不可调度"),
            "错误应提示不可调度，实际: {}",
            err
        );
        assert_eq!(manager.available_count(), 1);
        assert!(!manager.snapshot().entries[0].disabled);
    }

    #[tokio::test]
    async fn test_rate_limit_state_is_cleared_on_success() {
        let mut cred = KiroCredentials::default();
        cred.access_token = Some("t1".to_string());
        cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());

        let manager =
            MultiTokenManager::new(Config::default(), vec![cred], None, None, false).unwrap();

        manager
            .report_rate_limited(1, 429, "too many requests")
            .await;
        assert!(!manager.credential_is_schedulable_dynamic(1).await);

        manager.report_success(1);

        assert!(manager.credential_is_schedulable_dynamic(1).await);
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
