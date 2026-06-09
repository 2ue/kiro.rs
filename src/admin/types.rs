//! Admin API 类型定义

use serde::{Deserialize, Serialize};

use crate::anthropic::pricing::ModelPricing;
use crate::model::config::{
    CompatProfile, CompressionConfig, ExternalPoolsConfig, ModelMappingConfig, ModelResolutionMode,
    PayloadGuardMode, PayloadShapingConfig, PayloadShapingConfigPatch,
    PromptCacheCreationControlConfig, ReportedUsageConfig,
};

// ============ 凭据状态 ============

/// 凭据账号信息的最后一次查询快照。
///
/// 额度和用量来自上游 getUsageLimits，属于会变化的外部状态；这里表示“最后一次查询结果”，
/// 前端应配合 checkedAt 展示，避免误解为实时值。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialAccountInfo {
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
    /// 上次查询时间（RFC3339 格式）
    pub checked_at: String,
}

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
    /// 全局正在处理的调度请求数量。
    pub global_in_flight_requests: u32,
    /// 全局等待调度容量的请求数量。
    pub queued_requests: u32,
    /// 全局最大并发限制。0 表示不限。
    pub global_max_concurrent_requests: u32,
    /// 全局等待队列上限。0 表示不限。
    pub max_queued_requests: u32,
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
    pub global_in_flight_requests: u32,
    pub queued_requests: u32,
    pub global_max_concurrent_requests: u32,
    pub max_queued_requests: u32,
    /// 当前页码（从 1 开始）
    pub page: usize,
    /// 每页数量
    pub limit: usize,
    /// 总页数
    pub total_pages: usize,
    /// 查询条件命中的凭据总数；无筛选时等于 total。
    pub filtered_total: usize,
    /// 查询条件命中的未禁用凭据数；无筛选时等于 available。
    pub filtered_available: usize,
    /// 当前页凭据状态列表
    pub credentials: Vec<CredentialStatusItem>,
}

/// 单个凭据的状态信息
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialStatusItem {
    /// 凭据唯一 ID
    pub id: u64,
    /// 凭据创建时间（RFC3339 格式）
    pub created_at: Option<String>,
    /// 凭据更新时间（RFC3339 格式）
    pub updated_at: Option<String>,
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
    /// 订阅等级（KIRO PRO+ / KIRO FREE 等）
    pub subscription_title: Option<String>,
    /// 上次查询到的账号信息快照。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_info: Option<CredentialAccountInfo>,
    /// API 调用成功次数
    pub success_count: u64,
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
    /// 绑定的代理/家宽资源 ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_resource_id: Option<u64>,
    /// 绑定的代理/家宽资源名称
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_resource_name: Option<String>,
    /// 实际生效代理 URL
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_proxy_url: Option<String>,
    /// 实际代理来源：credential / resource / global / direct / none
    pub effective_proxy_source: String,
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
    /// 当前生效的单凭据最大并发请求数。0 表示不限制。
    pub max_concurrent_requests: u32,
    /// 凭据级最大并发覆盖值。None 表示继承全局；Some(0) 表示该账号不限并发。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_concurrent_requests_override: Option<u32>,
    /// 并发占用自动回收阈值。0 表示关闭。
    pub in_flight_lease_max_secs: u64,
    /// 预热剩余请求数。
    pub warmup_remaining: u32,
    /// 连续瞬态失败次数。
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
    /// 降权观察剩余秒数。
    pub probation_remaining_secs: u64,
    /// 调度选中次数。
    pub scheduler_selection_count: u64,
    /// 10 秒内调度选中次数。
    pub recent_scheduler_selection_count_10s: u32,
    /// 60 秒内调度选中次数。
    pub recent_scheduler_selection_count_60s: u32,
    /// 5 分钟内调度选中次数。
    pub recent_scheduler_selection_count_5m: u32,
    /// 近期调度压力，1 表示约等于平均份额，越高表示近期被选中过多。
    pub scheduler_selection_pressure: f64,
    /// 调度健康评分，越低越优先。
    pub scheduler_score: f64,
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

/// 修改凭据级最大并发请求数覆盖。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetCredentialConcurrencyRequest {
    /// None 表示继承全局；Some(0) 表示该账号不限并发；Some(n) 表示该账号最多 n 并发。
    #[serde(default)]
    pub max_concurrent_requests: Option<u32>,
}

/// 只更新凭据认证相关字段，不修改调度参数、代理、统计和运行态。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCredentialAuthRequest {
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub auth_method: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub kiro_api_key: Option<String>,
    #[serde(default)]
    pub auth_region: Option<String>,
    #[serde(default)]
    pub api_region: Option<String>,
    #[serde(default)]
    pub machine_id: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub endpoint: Option<String>,
    /// 更新认证材料后是否清理冷却、失败计数等运行态。默认 false，避免影响其他数据。
    #[serde(default)]
    pub reset_runtime_state: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearInFlightRequest {
    /// 只清理超过该秒数未活跃的并发占用；不传表示清理该凭据全部占用。
    #[serde(default)]
    pub min_idle_secs: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UsageCleanupMode {
    /// 软删除 created_at 早于 cutoff 的可见明细，页面和当前统计口径不再包含这些记录。
    SoftDelete,
    /// 硬删除 deleted_at 早于 cutoff 的已软删除明细，真正降低表体积。
    HardDelete,
}

impl Default for UsageCleanupMode {
    fn default() -> Self {
        Self::SoftDelete
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UsageCleanupJobStatus {
    Idle,
    Running,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageCleanupRequest {
    /// 清理模式；默认 soft_delete。
    #[serde(default)]
    pub mode: UsageCleanupMode,
    /// 清理多少天之前的数据；soft_delete 对应 created_at，hard_delete 对应 deleted_at。
    #[serde(default)]
    pub older_than_days: Option<u32>,
    /// 自定义 cutoff，优先级高于 older_than_days。
    #[serde(default)]
    pub cutoff_before: Option<String>,
    /// 每批处理行数，默认 1000。
    #[serde(default)]
    pub batch_size: Option<usize>,
    /// 内部安全批次上限；不传或传 0 时默认 10000，页面默认不暴露这个参数。
    #[serde(default)]
    pub max_batches: Option<usize>,
    /// 批次之间暂停毫秒数，默认 100。
    #[serde(default)]
    pub pause_ms_between_batches: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageCleanupPreviewResponse {
    pub mode: UsageCleanupMode,
    pub cutoff_at: String,
    pub matched_rows: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub newest_created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageCleanupStatusResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    pub status: UsageCleanupJobStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<UsageCleanupMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cutoff_at: Option<String>,
    pub batch_size: usize,
    pub max_batches: usize,
    pub pause_ms_between_batches: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_rows: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining_rows: Option<u64>,
    pub processed_rows: u64,
    pub last_batch_rows: u64,
    pub batches: usize,
    pub cancel_requested: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// 添加凭据请求
#[derive(Debug, Clone, Deserialize)]
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

    /// 凭据级最大并发覆盖。None 表示继承全局，0 表示该账号不限并发。
    #[serde(default)]
    pub max_concurrent_requests: Option<u32>,

    /// 新增后是否禁用启动。默认 false。
    #[serde(default)]
    pub disabled: Option<bool>,

    /// 新增后预热剩余请求数；不传时使用运行配置 credentialWarmupRequests。
    #[serde(default)]
    pub warmup_remaining: Option<u32>,

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

    /// 绑定的代理/家宽资源 ID（可选）
    pub proxy_resource_id: Option<u64>,

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

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BatchCredentialImportDefaults {
    #[serde(default)]
    pub disabled: Option<bool>,
    #[serde(default)]
    pub priority: Option<u32>,
    #[serde(default)]
    pub max_concurrent_requests: Option<Option<u32>>,
    #[serde(default)]
    pub auth_region: Option<String>,
    #[serde(default)]
    pub api_region: Option<String>,
    #[serde(default)]
    pub proxy_url: Option<String>,
    #[serde(default)]
    pub proxy_username: Option<String>,
    #[serde(default)]
    pub proxy_password: Option<String>,
    #[serde(default)]
    pub proxy_resource_id: Option<Option<u64>>,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub warmup_remaining: Option<u32>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BatchCredentialImportDuplicateMode {
    Skip,
    Error,
}

impl Default for BatchCredentialImportDuplicateMode {
    fn default() -> Self {
        Self::Skip
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchCredentialImportRequest {
    #[serde(default)]
    pub defaults: BatchCredentialImportDefaults,
    #[serde(default)]
    pub duplicate_mode: BatchCredentialImportDuplicateMode,
    #[serde(default)]
    pub continue_on_error: bool,
    pub credentials: Vec<AddCredentialRequest>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchCredentialImportItem {
    pub index: usize,
    pub ok: bool,
    pub skipped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchCredentialImportResponse {
    pub total: usize,
    pub success: usize,
    pub skipped: usize,
    pub failed: usize,
    pub items: Vec<BatchCredentialImportItem>,
}

// ============ 代理/家宽资源 ============

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyResourceResponse {
    pub id: u64,
    pub name: String,
    pub proxy_url: String,
    pub proxy_username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_password: Option<String>,
    pub has_password: bool,
    pub enabled: bool,
    pub notes: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub credential_count: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyResourcesResponse {
    pub resources: Vec<ProxyResourceResponse>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateProxyResourceRequest {
    pub name: String,
    pub proxy_url: String,
    pub proxy_username: Option<String>,
    pub proxy_password: Option<String>,
    #[serde(default = "default_proxy_resource_enabled")]
    pub enabled: bool,
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProxyResourceRequest {
    pub name: Option<String>,
    pub proxy_url: Option<String>,
    pub proxy_username: Option<String>,
    pub proxy_password: Option<String>,
    #[serde(default)]
    pub clear_username: bool,
    #[serde(default)]
    pub clear_password: bool,
    pub enabled: Option<bool>,
    pub notes: Option<String>,
    #[serde(default)]
    pub clear_notes: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetCredentialProxyRequest {
    pub proxy_resource_id: Option<u64>,
    pub proxy_url: Option<String>,
    pub proxy_username: Option<String>,
    pub proxy_password: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalPoolTestRequest {
    pub model: String,
    #[serde(default)]
    pub prompt: Option<String>,
}

fn default_proxy_resource_enabled() -> bool {
    true
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

// ============ 账号信息查询 ============

/// 账号信息查询响应。字段中的使用量和总额单位是 Kiro credits，不是美元。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BalanceResponse {
    /// 凭据 ID
    pub id: u64,
    /// 查询时间（RFC3339 格式）
    pub checked_at: String,
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshCredentialInfoRequest {
    pub ids: Vec<u64>,
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialInfoRefreshItem {
    pub id: u64,
    pub email: Option<String>,
    pub disabled: bool,
    pub ok: bool,
    pub info: Option<BalanceResponse>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialInfoRefreshResponse {
    pub total: usize,
    pub success: usize,
    pub failed: usize,
    pub items: Vec<CredentialInfoRefreshItem>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateExistingCredentialsRequest {
    /// all / enabled / disabled / selected
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub ids: Vec<u64>,
    #[serde(default = "default_true")]
    pub force: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateExternalCredentialsRequest {
    #[serde(default)]
    pub credentials: Vec<AddCredentialRequest>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialValidationInfo {
    pub subscription_title: Option<String>,
    pub current_usage: f64,
    pub usage_limit: f64,
    pub usage_percentage: f64,
    pub checked_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialValidationItem {
    pub id: Option<u64>,
    pub index: Option<usize>,
    pub email: Option<String>,
    pub disabled: Option<bool>,
    pub ok: bool,
    pub previous: Option<CredentialValidationInfo>,
    pub current: Option<CredentialValidationInfo>,
    pub change_kind: String,
    pub subscription_key: String,
    pub subscription_title: String,
    pub error: Option<String>,
    pub matched_existing_credential_id: Option<u64>,
    pub existing_disabled: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialValidationGroup {
    pub key: String,
    pub title: String,
    pub count: usize,
    pub items: Vec<CredentialValidationItem>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialValidationResponse {
    pub total: usize,
    pub success: usize,
    pub failed: usize,
    pub downgraded: usize,
    pub upgraded: usize,
    pub unchanged: usize,
    pub groups: Vec<CredentialValidationGroup>,
}

// ============ 负载均衡配置 ============

/// 负载均衡模式响应
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadBalancingModeResponse {
    /// 当前模式（"priority"、"balanced" 或 "health_balanced"）
    pub mode: String,
}

/// 设置负载均衡模式请求
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetLoadBalancingModeRequest {
    /// 模式（"priority"、"balanced" 或 "health_balanced"）
    pub mode: String,
}

// ============ 访问密钥 ============

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessKeysResponse {
    /// 下游客户端调用 /v1/messages 等接口时使用的请求 Key。
    pub request_api_key: String,
    pub masked_request_api_key: String,
    /// 管理后台登录和 /api/admin 认证使用的 Key。
    pub admin_api_key: String,
    pub masked_admin_api_key: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAdminApiKeyRequest {
    pub admin_api_key: String,
}

// ============ 运行时全局配置 ============

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeConfigResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_password: Option<String>,
    pub credential_rpm: u32,
    pub credential_max_concurrent_requests: u32,
    pub credential_transient_cooldown_secs: u64,
    pub credential_rate_limit_cooldown_secs: u64,
    pub credential_server_error_cooldown_secs: u64,
    pub credential_network_error_cooldown_secs: u64,
    pub credential_stream_error_cooldown_secs: u64,
    pub credential_protocol_error_cooldown_secs: u64,
    pub credential_auth_error_cooldown_secs: u64,
    pub credential_cooldown_backoff_multiplier: f64,
    pub credential_cooldown_jitter_percent: u32,
    pub credential_probation_secs: u64,
    pub credential_max_cooldown_secs: u64,
    pub credential_dispatch_max_wait_secs: u64,
    pub credential_retry_max_attempts: u32,
    pub credential_in_flight_lease_max_secs: u64,
    pub dispatch_global_max_concurrent_requests: u32,
    pub dispatch_max_queued_requests: u32,
    pub credential_warmup_requests: u32,
    pub credential_warmup_selection_percent: u32,
    pub credential_warmup_max_selection_percent: u32,
    pub scheduler_error_ewma_alpha: f64,
    pub scheduler_priority_weight: f64,
    pub scheduler_load_weight: f64,
    pub scheduler_error_weight: f64,
    pub scheduler_latency_weight: f64,
    pub scheduler_probation_weight: f64,
    pub scheduler_selection_pressure_weight: f64,
    pub scheduler_total_selection_weight: f64,
    pub scheduler_top_k: u32,
    pub compression_enabled: bool,
    pub whitespace_compression: bool,
    pub payload_guard_enabled: bool,
    pub payload_guard_mode: PayloadGuardMode,
    pub payload_guard_max_bytes: u64,
    pub payload_guard_safety_margin_bytes: u64,
    pub payload_guard_trim_history: bool,
    pub payload_shaping: PayloadShapingConfig,
    pub prompt_cache_target_read_ratio: f64,
    pub prompt_cache_token_scale: f64,
    pub prompt_cache_max_simulated_input_tokens: i32,
    pub prompt_cache_cap_jitter_min_tokens: i32,
    pub prompt_cache_cap_jitter_max_tokens: i32,
    pub prompt_cache_scale_min_input_tokens: i32,
    pub prompt_cache_creation_control: PromptCacheCreationControlConfig,
    pub reported_usage: ReportedUsageConfig,
    pub external_pools: ExternalPoolsConfig,
    pub high_cache_threshold: i32,
    pub compat_profile: CompatProfile,
    pub model_resolution_mode: ModelResolutionMode,
    pub model_mapping: ModelMappingConfig,
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
    #[serde(default)]
    pub credential_rate_limit_cooldown_secs: Option<u64>,
    #[serde(default)]
    pub credential_server_error_cooldown_secs: Option<u64>,
    #[serde(default)]
    pub credential_network_error_cooldown_secs: Option<u64>,
    #[serde(default)]
    pub credential_stream_error_cooldown_secs: Option<u64>,
    #[serde(default)]
    pub credential_protocol_error_cooldown_secs: Option<u64>,
    #[serde(default)]
    pub credential_auth_error_cooldown_secs: Option<u64>,
    #[serde(default)]
    pub credential_cooldown_backoff_multiplier: Option<f64>,
    #[serde(default)]
    pub credential_cooldown_jitter_percent: Option<u32>,
    #[serde(default)]
    pub credential_probation_secs: Option<u64>,
    pub credential_max_cooldown_secs: u64,
    #[serde(default)]
    pub credential_dispatch_max_wait_secs: Option<u64>,
    #[serde(default)]
    pub credential_retry_max_attempts: Option<u32>,
    #[serde(default)]
    pub credential_in_flight_lease_max_secs: Option<u64>,
    #[serde(default)]
    pub dispatch_global_max_concurrent_requests: Option<u32>,
    #[serde(default)]
    pub dispatch_max_queued_requests: Option<u32>,
    pub credential_warmup_requests: u32,
    #[serde(default)]
    pub credential_warmup_selection_percent: Option<u32>,
    #[serde(default)]
    pub credential_warmup_max_selection_percent: Option<u32>,
    #[serde(default)]
    pub scheduler_error_ewma_alpha: Option<f64>,
    #[serde(default)]
    pub scheduler_priority_weight: Option<f64>,
    #[serde(default)]
    pub scheduler_load_weight: Option<f64>,
    #[serde(default)]
    pub scheduler_error_weight: Option<f64>,
    #[serde(default)]
    pub scheduler_latency_weight: Option<f64>,
    #[serde(default)]
    pub scheduler_probation_weight: Option<f64>,
    #[serde(default)]
    pub scheduler_selection_pressure_weight: Option<f64>,
    #[serde(default)]
    pub scheduler_total_selection_weight: Option<f64>,
    #[serde(default)]
    pub scheduler_top_k: Option<u32>,
    pub compression_enabled: bool,
    #[serde(default = "default_true")]
    pub whitespace_compression: bool,
    #[serde(default)]
    pub payload_guard_enabled: Option<bool>,
    #[serde(default)]
    pub payload_guard_mode: Option<PayloadGuardMode>,
    #[serde(default)]
    pub payload_guard_max_bytes: Option<u64>,
    #[serde(default)]
    pub payload_guard_safety_margin_bytes: Option<u64>,
    #[serde(default)]
    pub payload_guard_trim_history: Option<bool>,
    #[serde(default)]
    pub payload_shaping: Option<PayloadShapingConfigPatch>,
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
    pub prompt_cache_creation_control: Option<PromptCacheCreationControlConfig>,
    #[serde(default)]
    pub reported_usage: Option<ReportedUsageConfig>,
    #[serde(default)]
    pub external_pools: Option<ExternalPoolsConfig>,
    #[serde(default)]
    pub high_cache_threshold: Option<i32>,
    #[serde(default)]
    pub compat_profile: Option<CompatProfile>,
    #[serde(default)]
    pub model_resolution_mode: Option<ModelResolutionMode>,
    #[serde(default)]
    pub model_mapping: Option<ModelMappingConfig>,
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

// ============ 手动模型补充 ============

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualModelPricingRequest {
    pub input_cost_per_million: f64,
    pub output_cost_per_million: f64,
    #[serde(default)]
    pub cache_creation_input_cost_per_million: Option<f64>,
    #[serde(default)]
    pub cache_read_input_cost_per_million: Option<f64>,
}

impl ManualModelPricingRequest {
    pub fn to_pricing(&self) -> Option<ModelPricing> {
        let input = cost_per_token(self.input_cost_per_million)?;
        let output = cost_per_token(self.output_cost_per_million)?;
        let cache_creation = self
            .cache_creation_input_cost_per_million
            .and_then(cost_per_token)
            .unwrap_or(input * 1.25);
        let cache_read = self
            .cache_read_input_cost_per_million
            .and_then(cost_per_token)
            .unwrap_or(input * 0.1);
        Some(ModelPricing {
            input_cost_per_token: input,
            output_cost_per_token: output,
            cache_creation_input_token_cost: cache_creation,
            cache_read_input_token_cost: cache_read,
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpsertManualModelRequest {
    pub model: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub max_input_tokens: Option<i32>,
    #[serde(default)]
    pub max_output_tokens: Option<i32>,
    #[serde(default)]
    pub supports_prompt_caching: Option<bool>,
    #[serde(default)]
    pub supported_input_types: Vec<String>,
    #[serde(default)]
    pub pricing: Option<ManualModelPricingRequest>,
    #[serde(default)]
    pub clear_pricing: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualModelResponse {
    pub success: bool,
    pub message: String,
    pub model: String,
}

impl ManualModelResponse {
    pub fn new(model: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            success: true,
            model: model.into(),
            message: message.into(),
        }
    }
}

fn cost_per_token(value: f64) -> Option<f64> {
    value.is_finite().then_some(value.max(0.0) / 1_000_000.0)
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
