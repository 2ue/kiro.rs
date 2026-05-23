//! Admin API 类型定义

use serde::{Deserialize, Serialize};

use crate::model::config::{CompatProfile, CompressionConfig, ReportedUsageConfig};

// ============ 凭据状态 ============

/// 所有凭据状态响应
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialsStatusResponse {
    /// 凭据总数
    pub total: usize,
    /// 可用凭据数量（未禁用）
    pub available: usize,
    /// 当前活跃凭据 ID
    pub current_id: u64,
    /// 各凭据状态列表
    pub credentials: Vec<CredentialStatusItem>,
}

/// 分页凭据状态响应
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialsPageResponse {
    /// 凭据总数
    pub total: usize,
    /// 可用凭据数量（未禁用）
    pub available: usize,
    /// 当前活跃凭据 ID
    pub current_id: u64,
    /// 当前页码（从 1 开始）
    pub page: usize,
    /// 每页数量
    pub limit: usize,
    /// 总页数
    pub total_pages: usize,
    /// 当前页凭据状态列表
    pub credentials: Vec<CredentialStatusItem>,
}

/// 单个凭据的状态信息
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialStatusItem {
    /// 凭据唯一 ID
    pub id: u64,
    /// 优先级（数字越小优先级越高）
    pub priority: u32,
    /// 是否被禁用
    pub disabled: bool,
    /// 连续失败次数
    pub failure_count: u32,
    /// 是否为当前活跃凭据
    pub is_current: bool,
    /// Token 过期时间（RFC3339 格式）
    pub expires_at: Option<String>,
    /// 认证方式
    pub auth_method: Option<String>,
    /// 是否有 Profile ARN
    pub has_profile_arn: bool,
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
    /// 端点名称（决定该凭据走哪套 Kiro API，已回退到默认端点）
    pub endpoint: String,
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
    /// 并发占用自动回收阈值。0 表示关闭。
    pub in_flight_lease_max_secs: u64,
    /// 预热剩余请求数。
    pub warmup_remaining: u32,
    /// 该凭据已记录的估算费用（USD）。
    pub estimated_cost_usd: f64,
    /// 有价格表命中的请求数。
    pub priced_requests: usize,
    /// 无价格表命中的请求数。
    pub unpriced_requests: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportCredentialsQuery {
    pub format: Option<String>,
}

// ============ 操作请求 ============

/// 启用/禁用凭据请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetDisabledRequest {
    /// 是否禁用
    pub disabled: bool,
}

/// 修改优先级请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetPriorityRequest {
    /// 新优先级值
    pub priority: u32,
}

/// 修改凭据预热状态请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetWarmupRequest {
    /// 预热剩余请求数；0 表示关闭预热。
    pub warmup_remaining: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearInFlightRequest {
    /// 只清理超过该秒数未活跃的并发占用；不传表示清理该凭据全部占用。
    #[serde(default)]
    pub min_idle_secs: Option<u64>,
}

/// 添加凭据请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddCredentialRequest {
    /// 刷新令牌（OAuth 凭据必填，API Key 凭据不需要）
    pub refresh_token: Option<String>,

    /// 认证方式（可选，默认 social）
    #[serde(default = "default_auth_method")]
    pub auth_method: String,

    /// OIDC Client ID（IdC 认证需要）
    pub client_id: Option<String>,

    /// OIDC Client Secret（IdC 认证需要）
    pub client_secret: Option<String>,

    /// 优先级（可选，默认 0）
    #[serde(default)]
    pub priority: u32,

    /// 凭据级 Region 配置（用于 OIDC token 刷新）
    /// 未配置时回退到 config.json 的全局 region
    pub region: Option<String>,

    /// 凭据级 Auth Region（用于 Token 刷新）
    pub auth_region: Option<String>,

    /// 凭据级 API Region（用于 API 请求）
    pub api_region: Option<String>,

    /// 凭据级 Machine ID（可选，64 位字符串）
    /// 未配置时回退到 config.json 的 machineId
    pub machine_id: Option<String>,

    /// 用户邮箱（可选，用于前端显示）
    pub email: Option<String>,

    /// 凭据级代理 URL（可选，特殊值 "direct" 表示不使用代理）
    pub proxy_url: Option<String>,

    /// 凭据级代理认证用户名（可选）
    pub proxy_username: Option<String>,

    /// 凭据级代理认证密码（可选）
    pub proxy_password: Option<String>,

    /// Kiro API Key（API Key 凭据必填，格式: ksk_xxxxxxxx）
    /// 设置后直接作为 Bearer Token 使用，无需 refreshToken
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kiro_api_key: Option<String>,

    /// 端点名称（可选，未配置时使用 config.defaultEndpoint）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
}

fn default_auth_method() -> String {
    "social".to_string()
}

/// 添加凭据成功响应
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddCredentialResponse {
    pub success: bool,
    pub message: String,
    /// 新添加的凭据 ID
    pub credential_id: u64,
    /// 用户邮箱（如果获取成功）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

/// 测试凭据模型调用请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestCredentialRequest {
    /// Anthropic 兼容模型名，例如 claude-opus-4-5-20251101
    pub model: String,
    /// 测试消息，默认 hi
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

/// 测试凭据模型调用响应
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestCredentialResponse {
    pub success: bool,
    pub credential_id: u64,
    /// 前端选择的 Anthropic 兼容模型名
    pub model: String,
    /// 发送到 Kiro 上游的模型 ID
    pub model_id: String,
    pub prompt: String,
    pub response: String,
    pub duration_ms: u64,
}

// ============ 余额查询 ============

/// 余额查询响应
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BalanceResponse {
    /// 凭据 ID
    pub id: u64,
    /// 订阅类型
    pub subscription_title: Option<String>,
    /// 当前使用量
    pub current_usage: f64,
    /// 使用限额
    pub usage_limit: f64,
    /// 剩余额度
    pub remaining: f64,
    /// 使用百分比
    pub usage_percentage: f64,
    /// 下次重置时间（Unix 时间戳）
    pub next_reset_at: Option<f64>,
}

// ============ 负载均衡配置 ============

/// 负载均衡模式响应
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadBalancingModeResponse {
    /// 当前模式（"priority" 或 "balanced"）
    pub mode: String,
}

/// 设置负载均衡模式请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetLoadBalancingModeRequest {
    /// 模式（"priority" 或 "balanced"）
    pub mode: String,
}

// ============ 运行时全局配置 ============

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeConfigResponse {
    pub credential_rpm: u32,
    pub credential_max_concurrent_requests: u32,
    pub credential_transient_cooldown_secs: u64,
    pub credential_max_cooldown_secs: u64,
    pub credential_dispatch_max_wait_secs: u64,
    pub credential_in_flight_lease_max_secs: u64,
    pub credential_warmup_requests: u32,
    pub credential_warmup_selection_percent: u32,
    pub compression_enabled: bool,
    pub whitespace_compression: bool,
    pub prompt_cache_target_read_ratio: f64,
    pub prompt_cache_token_scale: f64,
    pub prompt_cache_max_simulated_input_tokens: i32,
    pub prompt_cache_cap_jitter_min_tokens: i32,
    pub prompt_cache_cap_jitter_max_tokens: i32,
    pub prompt_cache_scale_min_input_tokens: i32,
    pub reported_usage: ReportedUsageConfig,
    pub high_cache_threshold: i32,
    pub compat_profile: CompatProfile,
    pub extract_thinking: bool,
    pub expose_proxy_warnings: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateRuntimeConfigRequest {
    pub credential_rpm: u32,
    #[serde(default)]
    pub credential_max_concurrent_requests: u32,
    pub credential_transient_cooldown_secs: u64,
    pub credential_max_cooldown_secs: u64,
    #[serde(default)]
    pub credential_dispatch_max_wait_secs: Option<u64>,
    #[serde(default)]
    pub credential_in_flight_lease_max_secs: Option<u64>,
    pub credential_warmup_requests: u32,
    #[serde(default)]
    pub credential_warmup_selection_percent: Option<u32>,
    pub compression_enabled: bool,
    #[serde(default = "default_true")]
    pub whitespace_compression: bool,
    #[serde(default)]
    pub prompt_cache_target_read_ratio: Option<f64>,
    #[serde(default)]
    pub prompt_cache_token_scale: Option<f64>,
    #[serde(default)]
    pub prompt_cache_max_simulated_input_tokens: Option<i32>,
    #[serde(default)]
    pub prompt_cache_cap_jitter_min_tokens: Option<i32>,
    #[serde(default)]
    pub prompt_cache_cap_jitter_max_tokens: Option<i32>,
    #[serde(default)]
    pub prompt_cache_scale_min_input_tokens: Option<i32>,
    #[serde(default)]
    pub reported_usage: Option<ReportedUsageConfig>,
    #[serde(default)]
    pub high_cache_threshold: Option<i32>,
    #[serde(default)]
    pub compat_profile: Option<CompatProfile>,
    #[serde(default)]
    pub extract_thinking: Option<bool>,
    #[serde(default)]
    pub expose_proxy_warnings: Option<bool>,
}

impl UpdateRuntimeConfigRequest {
    pub fn compression(&self) -> CompressionConfig {
        CompressionConfig {
            enabled: self.compression_enabled,
            whitespace_compression: self.whitespace_compression,
        }
    }
}

fn default_true() -> bool {
    true
}

// ============ 通用响应 ============

/// 操作成功响应
#[derive(Debug, Serialize)]
pub struct SuccessResponse {
    pub success: bool,
    pub message: String,
}

impl SuccessResponse {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            success: true,
            message: message.into(),
        }
    }
}

/// 错误响应
#[derive(Debug, Serialize)]
pub struct AdminErrorResponse {
    pub error: AdminError,
}

#[derive(Debug, Serialize)]
pub struct AdminError {
    #[serde(rename = "type")]
    pub error_type: String,
    pub message: String,
}

impl AdminErrorResponse {
    pub fn new(error_type: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            error: AdminError {
                error_type: error_type.into(),
                message: message.into(),
            },
        }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new("invalid_request", message)
    }

    pub fn authentication_error() -> Self {
        Self::new("authentication_error", "Invalid or missing admin API key")
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new("not_found", message)
    }

    pub fn api_error(message: impl Into<String>) -> Self {
        Self::new("api_error", message)
    }

    pub fn internal_error(message: impl Into<String>) -> Self {
        Self::new("internal_error", message)
    }
}
