//! Admin API 类型定义

use serde::{Deserialize, Deserializer, Serialize};

use crate::anthropic::pricing::ModelPricing;
use crate::model::config::{
    BodyConversionConfig, CachePolicyConfig, CompatProfile, CompressionConfig, ExternalPoolsConfig,
    ImageProcessingConfig, KiroAgentModeStrategy, MissingMaxTokensConfig, ModelMappingConfig,
    ModelResolutionMode, PayloadGuardMode, PayloadShapingConfig, PayloadShapingConfigPatch,
    PromptCacheCreationControlConfig, PromptSteeringConfig, ReportedUsageConfig,
    RequestAdmissionConfig, ThinkingTriggerMode, WeightedCapacityConfig,
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
    /// 按本系统订阅规则计算的总积分。
    #[serde(default)]
    pub credit_limit: f64,
    /// 按本系统订阅规则计算的剩余积分。
    #[serde(default)]
    pub credit_remaining: f64,
    /// 订阅基础积分。
    #[serde(default)]
    pub credit_base: f64,
    /// 开启超额后的额外积分。
    #[serde(default)]
    pub credit_bonus: f64,
    /// 上游 Overages 开关状态：ENABLED / DISABLED / UNKNOWN。
    #[serde(default)]
    pub overage_status: Option<String>,
    /// 上游 Overages 能力：OVERAGE_CAPABLE / NOT_OVERAGE_CAPABLE 等。
    #[serde(default)]
    pub overage_capability: Option<String>,
    /// Overage 上限（美元）。
    #[serde(default)]
    pub overage_cap: f64,
    /// Overage 单价（美元）。
    #[serde(default)]
    pub overage_rate: f64,
    /// 已产生 Overage 费用（美元）。
    #[serde(default)]
    pub current_overages: f64,
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialAccountInfoListResponse {
    pub items: Vec<CredentialAccountInfoItem>,
    pub updated_at: String,
    pub fresh: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialAccountInfoItem {
    pub id: u64,
    pub subscription_title: Option<String>,
    pub current_usage: f64,
    pub usage_limit: f64,
    pub remaining: f64,
    pub usage_percentage: f64,
    pub credit_limit: f64,
    pub credit_remaining: f64,
    pub credit_base: f64,
    pub credit_bonus: f64,
    pub overage_status: Option<String>,
    pub overage_capability: Option<String>,
    pub overage_cap: f64,
    pub overage_rate: f64,
    pub current_overages: f64,
    pub next_reset_at: Option<f64>,
    pub checked_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialRuntimeResponse {
    pub items: Vec<CredentialRuntimeItem>,
    pub updated_at: String,
    pub fresh: bool,
}

/// 高频变化的凭据运行态字段。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialRuntimeItem {
    pub id: u64,
    pub is_current: bool,
    pub failure_count: u32,
    pub refresh_failure_count: u32,
    pub success_count: u64,
    pub last_used_at: Option<String>,
    pub expires_at: Option<String>,
    pub cooled_down: bool,
    pub cooldown_remaining_secs: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cooldown_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cooldowns: Vec<CredentialCooldown>,
    pub rate_limited: bool,
    pub rate_limit_remaining_secs: u64,
    pub in_flight_requests: u32,
    pub oldest_in_flight_age_secs: u64,
    pub newest_in_flight_idle_secs: u64,
    pub max_concurrent_requests: u32,
    pub in_flight_lease_max_secs: u64,
    pub rpm: u32,
    pub transient_failure_streak: u32,
    pub recent_error_rate: f64,
    pub latency_ewma_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error_reason: Option<String>,
    pub last_error_at_ms: Option<i64>,
    pub in_probation: bool,
    pub probation_remaining_secs: u64,
    pub scheduler_selection_count: u64,
    pub recent_scheduler_selection_count_10s: u32,
    pub recent_scheduler_selection_count_60s: u32,
    pub recent_scheduler_selection_count_5m: u32,
    pub scheduler_selection_pressure: f64,
    pub scheduler_score: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_models: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialUsageSummaryResponse {
    pub items: Vec<CredentialUsageSummaryItem>,
    pub updated_at: String,
    pub fresh: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialUsageSummaryItem {
    pub id: u64,
    pub estimated_cost_usd: f64,
    pub original_cost_usd: f64,
    pub kiro_metering_usage: f64,
    pub priced_requests: usize,
    pub unpriced_requests: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkCredentialActionResponse {
    pub total_matched: usize,
    pub total_attempted: usize,
    pub success: usize,
    pub failed: usize,
    pub skipped: usize,
    pub errors: Vec<BulkCredentialActionError>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkCredentialActionError {
    pub id: u64,
    pub message: String,
}

/// 轻量凭据列表响应。
///
/// 该响应只包含列表首屏和筛选分页需要的基础字段，不携带运行态、账号额度或费用聚合。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialListResponse {
    pub page: usize,
    pub limit: usize,
    pub total: usize,
    pub available: usize,
    pub filtered_total: usize,
    pub filtered_available: usize,
    pub total_pages: usize,
    pub items: Vec<CredentialListItem>,
}

/// 轻量凭据列表项。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialListItem {
    pub id: u64,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub priority: u32,
    pub disabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
    pub auth_method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_resource_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_resource_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_proxy_url: Option<String>,
    pub effective_proxy_source: String,
    pub endpoint: String,
    /// 当前生效的单凭据最大并发请求数。0 表示不限制。
    pub max_concurrent_requests: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_concurrent_requests_override: Option<u32>,
    /// 当前生效的凭据每分钟请求数。0 表示不限制。
    pub rpm: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpm_override: Option<u32>,
    /// 429 临时风控是否自动禁用账号。
    pub rate_limit_auto_disable_enabled: bool,
    pub warmup_remaining: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_models: Vec<String>,
}

/// 凭据数量与全局调度容量概览。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialSummaryResponse {
    pub total: usize,
    pub available: usize,
    pub disabled: usize,
    pub current_id: Option<u64>,
    pub global_in_flight_requests: u32,
    pub queued_requests: u32,
    pub global_max_concurrent_requests: u32,
    pub max_queued_requests: u32,
    pub updated_at: String,
    pub runtime_fresh: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialCooldown {
    pub model: Option<String>,
    pub global: bool,
    pub remaining_secs: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
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
    /// 上游身份提供方
    #[serde(skip_serializing_if = "Option::is_none")]
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
    /// 当前生效的凭据每分钟请求数。0 表示不限制。
    pub rpm: u32,
    /// 凭据级 RPM 覆盖值。None 表示继承全局；Some(0) 表示该账号不限 RPM。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpm_override: Option<u32>,
    /// 429 临时风控是否自动禁用账号。
    pub rate_limit_auto_disable_enabled: bool,
    /// 并发占用自动回收阈值。0 表示关闭。
    pub in_flight_lease_max_secs: u64,
    /// 预热剩余请求数。
    pub warmup_remaining: u32,
    /// 凭据支持的模型列表。空列表表示不限制模型调度。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_models: Vec<String>,
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
    /// Kiro 上游 meteringEvent 累计上报的积分用量，仅用于和上游核对。
    pub kiro_metering_usage: f64,
    /// 有价格表命中的请求数。
    pub priced_requests: usize,
    /// 无价格表命中的请求数。
    pub unpriced_requests: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportCredentialsQuery {
    pub format: Option<String>,
    /// Comma-separated credential ids to export. Empty or absent means export all credentials.
    pub ids: Option<String>,
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
#[derive(Debug, Clone, Deserialize)]
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
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetCredentialConcurrencyRequest {
    /// None 表示继承全局；Some(0) 表示该账号不限并发；Some(n) 表示该账号最多 n 并发。
    #[serde(default)]
    pub max_concurrent_requests: Option<u32>,
}

/// 修改凭据级 RPM 覆盖。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetCredentialRpmRequest {
    /// None 表示继承全局；Some(0) 表示该账号不限 RPM；Some(n) 表示该账号最多 n RPM。
    #[serde(default)]
    pub rpm: Option<u32>,
}

/// 修改 429 临时风控自动禁用开关。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetCredentialRateLimitAutoDisableRequest {
    pub enabled: bool,
}

/// 修改支持模型列表。空列表表示不限制模型调度。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetSupportedModelsRequest {
    #[serde(default)]
    pub supported_models: Vec<String>,
}

/// 使用外部池兼容 /v1/models 接口发现支持模型。创建态需要传 baseUrl/apiKey；
/// 编辑态可只传覆盖项，空 key 表示使用已保存 key。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverExternalPoolSupportedModelsRequest {
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub auth_type: Option<crate::external_pool::ExternalPoolAuthType>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupportedModelsResponse {
    pub supported_models: Vec<String>,
    pub count: usize,
}

/// 修改凭据 Region 覆盖值。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetCredentialRegionsRequest {
    /// 凭据级兼容 Region；主要作为 Auth Region 回退字段。空字符串表示清空覆盖。
    #[serde(default, deserialize_with = "deserialize_optional_string_update")]
    pub region: Option<Option<String>>,
    /// 凭据级 Auth Region。空字符串表示清空覆盖。
    #[serde(default, deserialize_with = "deserialize_optional_string_update")]
    pub auth_region: Option<Option<String>>,
    /// 凭据级 API Region。空字符串表示清空覆盖。
    #[serde(default, deserialize_with = "deserialize_optional_string_update")]
    pub api_region: Option<Option<String>>,
}

fn deserialize_optional_string_update<'de, D>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Some)
}

/// 只更新凭据认证相关字段，不修改调度参数、代理、统计和运行态。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCredentialAuthRequest {
    #[serde(default, alias = "access_token")]
    pub access_token: Option<String>,
    #[serde(default, alias = "expires_at", alias = "expired")]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub auth_method: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default, alias = "token_endpoint")]
    pub token_endpoint: Option<String>,
    #[serde(default, alias = "issuer_url")]
    pub issuer_url: Option<String>,
    #[serde(default, alias = "scope")]
    pub scopes: Option<String>,
    #[serde(default)]
    pub kiro_api_key: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
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
    Queued,
    Running,
    Paused,
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
    /// 0 表示以任务启动时刻为 cutoff，清理当时之前全部匹配记录；最大 3650。
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

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageCleanupResumeRequest {
    pub job_id: String,
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
    pub phase: String,
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
    pub redis_deleted_keys: usize,
    pub redis_delete_commands: usize,
    pub redis_max_command_keys: usize,
    pub redis_scan_passes: usize,
    pub redis_used_del_fallback: bool,
    pub redis_pass_limit_reached: bool,
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
    /// 访问令牌（可选；主要用于从 SSO JWT issuer 推导 token endpoint）
    #[serde(alias = "access_token")]
    pub access_token: Option<String>,

    /// 访问令牌过期时间（可选；新增时仍会刷新验证）
    #[serde(alias = "expires_at", alias = "expired")]
    pub expires_at: Option<String>,

    /// 刷新令牌（OAuth 凭据必填，API Key 凭据不需要）
    #[serde(alias = "refresh_token")]
    pub refresh_token: Option<String>,

    /// 认证方式（可选；缺省时由服务端根据凭据字段自动推断）
    #[serde(default = "default_auth_method", alias = "auth_method")]
    pub auth_method: String,

    /// 上游身份提供方（BuilderId / Enterprise / ExternalIdp / Github / Google 等）
    #[serde(default)]
    pub provider: Option<String>,

    /// OIDC Client ID（IdC 认证需要）
    #[serde(alias = "client_id")]
    pub client_id: Option<String>,

    /// OIDC Client Secret（IdC 认证需要）
    #[serde(alias = "client_secret")]
    pub client_secret: Option<String>,

    /// 外部 IdP OAuth token endpoint（external_idp 认证需要）
    #[serde(alias = "token_endpoint")]
    pub token_endpoint: Option<String>,

    /// 外部 IdP issuer URL（可选）
    #[serde(alias = "issuer_url")]
    pub issuer_url: Option<String>,

    /// 外部 IdP OAuth scopes（可选，存在时刷新会原样带给 token endpoint）
    #[serde(alias = "scope")]
    pub scopes: Option<String>,

    /// Profile ARN（IdC 凭据可选，用于 Amazon Q / CodeWhisperer profile）
    #[serde(alias = "profile_arn")]
    pub profile_arn: Option<String>,

    /// 优先级（可选，默认 0）
    #[serde(default)]
    pub priority: u32,

    /// 凭据级最大并发覆盖。None 表示继承全局，0 表示该账号不限并发。
    #[serde(default, alias = "max_concurrent_requests")]
    pub max_concurrent_requests: Option<u32>,

    /// 凭据级 RPM 覆盖。None 表示继承全局，0 表示该账号不限 RPM。
    #[serde(default)]
    pub rpm: Option<u32>,

    /// 429 临时风控是否自动禁用账号。默认 true；false 表示仅进入 429 冷却。
    #[serde(default)]
    pub rate_limit_auto_disable_enabled: Option<bool>,

    /// 新增后是否禁用启动。默认 false。
    #[serde(default)]
    pub disabled: Option<bool>,

    /// 新增后预热剩余请求数；不传时使用运行配置 credentialWarmupRequests。
    #[serde(default)]
    pub warmup_remaining: Option<u32>,

    /// 新增后尝试开启上游 Overages。失败不会回滚账号导入，但会在响应中返回 warning。
    #[serde(default)]
    pub enable_overage_after_import: Option<bool>,

    /// 凭据支持的模型列表。空列表表示不限制模型调度。
    #[serde(default)]
    pub supported_models: Vec<String>,

    /// API Key 凭据新增时是否自动请求上游模型列表并写入 supported_models。
    ///
    /// None 表示使用调用入口的默认行为；单账号新增保持旧行为自动发现，批量导入默认关闭。
    #[serde(default, alias = "auto_discover_supported_models")]
    pub auto_discover_supported_models: Option<bool>,

    /// 凭据级 Region 配置（用于 OIDC token 刷新）
    /// 未配置时回退到 config.json 的全局 region
    pub region: Option<String>,

    /// 凭据级 Auth Region（用于 Token 刷新）
    #[serde(alias = "auth_region")]
    pub auth_region: Option<String>,

    /// 凭据级 API Region（用于 API 请求）
    #[serde(alias = "api_region")]
    pub api_region: Option<String>,

    /// 凭据级 Machine ID（可选，64 位字符串）
    /// 未配置时回退到 config.json 的 machineId
    #[serde(alias = "machine_id")]
    pub machine_id: Option<String>,

    /// 用户邮箱（可选，用于前端显示）
    pub email: Option<String>,

    /// 凭据级代理 URL（可选，特殊值 "direct" 表示不使用代理）
    #[serde(alias = "proxy_url")]
    pub proxy_url: Option<String>,

    /// 凭据级代理认证用户名（可选）
    #[serde(alias = "proxy_username")]
    pub proxy_username: Option<String>,

    /// 凭据级代理认证密码（可选）
    #[serde(alias = "proxy_password")]
    pub proxy_password: Option<String>,

    /// 绑定的代理/家宽资源 ID（可选）
    #[serde(alias = "proxy_resource_id")]
    pub proxy_resource_id: Option<u64>,

    /// Kiro API Key（API Key 凭据必填，格式: ksk_xxxxxxxx）
    /// 设置后直接作为 Bearer Token 使用，无需 refreshToken
    #[serde(skip_serializing_if = "Option::is_none", alias = "kiro_api_key")]
    pub kiro_api_key: Option<String>,

    /// 端点名称（可选，未配置时使用 config.defaultEndpoint）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
}

fn default_auth_method() -> String {
    String::new()
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
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
    pub rpm: Option<Option<u32>>,
    #[serde(default)]
    pub rate_limit_auto_disable_enabled: Option<bool>,
    #[serde(default)]
    pub provider: Option<String>,
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
    #[serde(default)]
    pub enable_overage_after_import: Option<bool>,
    #[serde(default)]
    pub supported_models: Option<Vec<String>>,
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
    /// 批量导入时是否自动发现 API Key 支持模型并写入模型白名单。默认关闭，避免导入后意外加模型限制。
    #[serde(default)]
    pub auto_discover_supported_models: bool,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyResourceTestRequest {
    pub proxy_url: Option<String>,
    pub proxy_username: Option<String>,
    pub proxy_password: Option<String>,
    pub test_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyResourceTestResponse {
    pub success: bool,
    pub message: String,
    pub proxy_url: String,
    pub test_url: String,
    pub status: Option<u16>,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_preview: Option<String>,
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

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetCredentialProxyRequest {
    pub proxy_resource_id: Option<u64>,
    pub proxy_url: Option<String>,
    pub proxy_username: Option<String>,
    pub proxy_password: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchUpdateCredentialsRequest {
    pub ids: Vec<u64>,
    #[serde(default)]
    pub priority: Option<SetPriorityRequest>,
    #[serde(default)]
    pub regions: Option<SetCredentialRegionsRequest>,
    #[serde(default)]
    pub concurrency: Option<SetCredentialConcurrencyRequest>,
    #[serde(default)]
    pub rpm: Option<SetCredentialRpmRequest>,
    #[serde(default)]
    pub rate_limit_auto_disable: Option<SetCredentialRateLimitAutoDisableRequest>,
    #[serde(default)]
    pub proxy: Option<SetCredentialProxyRequest>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchUpdateCredentialItem {
    pub id: u64,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchUpdateCredentialsResponse {
    pub total: usize,
    pub success: usize,
    pub failed: usize,
    pub items: Vec<BatchUpdateCredentialItem>,
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
    /// 按本系统订阅规则计算的总积分。
    #[serde(default)]
    pub credit_limit: f64,
    /// 按本系统订阅规则计算的剩余积分。
    #[serde(default)]
    pub credit_remaining: f64,
    /// 订阅基础积分。
    #[serde(default)]
    pub credit_base: f64,
    /// 开启超额后的额外积分。
    #[serde(default)]
    pub credit_bonus: f64,
    /// 上游 Overages 开关状态：ENABLED / DISABLED / UNKNOWN。
    #[serde(default)]
    pub overage_status: Option<String>,
    /// 上游 Overages 能力：OVERAGE_CAPABLE / NOT_OVERAGE_CAPABLE 等。
    #[serde(default)]
    pub overage_capability: Option<String>,
    /// Overage 上限（美元）。
    #[serde(default)]
    pub overage_cap: f64,
    /// Overage 单价（美元）。
    #[serde(default)]
    pub overage_rate: f64,
    /// 已产生 Overage 费用（美元）。
    #[serde(default)]
    pub current_overages: f64,
    /// 下次重置时间（Unix 时间戳）
    pub next_reset_at: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetCredentialOverageRequest {
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialCreditSummaryResponse {
    pub total_credentials: usize,
    pub enabled_credentials: usize,
    pub disabled_credentials: usize,
    pub total_credit_limit: f64,
    pub total_credit_remaining: f64,
    pub total_current_usage: f64,
    pub enabled_credit_limit: f64,
    pub enabled_credit_remaining: f64,
    pub disabled_credit_limit: f64,
    pub disabled_credit_remaining: f64,
    /// Recorded estimated cost for all credentials in usage history.
    #[serde(default)]
    pub total_estimated_cost_usd: f64,
    /// Recorded original/upstream cost for all credentials in usage history.
    #[serde(default)]
    pub total_original_cost_usd: f64,
    /// Recorded estimated cost for currently enabled credentials in usage history.
    #[serde(default)]
    pub enabled_estimated_cost_usd: f64,
    /// Recorded original/upstream cost for currently enabled credentials in usage history.
    #[serde(default)]
    pub enabled_original_cost_usd: f64,
    /// Recorded estimated cost for currently disabled credentials in usage history.
    #[serde(default)]
    pub disabled_estimated_cost_usd: f64,
    /// Recorded original/upstream cost for currently disabled credentials in usage history.
    #[serde(default)]
    pub disabled_original_cost_usd: f64,
    pub last_checked_at: Option<String>,
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
    /// 是否查询订阅标题。默认保持旧行为：查询。
    #[serde(default = "default_true")]
    pub query_subscription: bool,
    /// 是否查询当前用量/额度。默认保持旧行为：查询。
    #[serde(default = "default_true")]
    pub query_usage: bool,
    /// 是否发送一次最小模型请求做验活。默认发送模型请求。
    #[serde(default = "default_true")]
    pub check_liveness: bool,
    /// 验活模型；为空时使用管理端默认验活模型。
    #[serde(default)]
    pub liveness_model: Option<String>,
    /// 验活提示词；为空时使用管理端默认提示词。
    #[serde(default)]
    pub liveness_prompt: Option<String>,
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
    pub subscription_checked: bool,
    pub usage_checked: bool,
    pub liveness_checked: bool,
    pub subscription_ok: Option<bool>,
    pub usage_ok: Option<bool>,
    pub liveness_ok: Option<bool>,
    pub usage_error: Option<String>,
    pub liveness_error: Option<String>,
    pub liveness_model: Option<String>,
    pub liveness_response: Option<String>,
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

/// 系统版本响应
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemVersionResponse {
    /// 当前服务版本，来自 Cargo package version。
    pub version: &'static str,
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
    /// 下游客户端调用 API Key 列表。`requestApiKey` 为兼容旧前端保留，等于这里的第一项。
    pub request_api_keys: Vec<RequestApiKeyItem>,
    /// 管理后台登录和 /api/admin 认证使用的 Key。
    pub admin_api_key: String,
    pub masked_admin_api_key: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestApiKeyItem {
    pub id: String,
    pub api_key: String,
    pub masked_api_key: String,
    pub primary: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateRequestApiKeyRequest {
    #[serde(default)]
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateRequestApiKeyRequest {
    #[serde(default)]
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAdminApiKeyRequest {
    pub admin_api_key: String,
}

// ============ 运行时全局配置 ============

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuxiliaryUpstreamRuntimeResponse {
    pub configured_limit: u32,
    pub in_flight: u32,
    pub peak_in_flight: u32,
    pub rejected: u64,
    pub refresh_client_cache_entries: usize,
    pub refresh_client_cache_max_entries: usize,
    pub refresh_client_builds: u64,
    pub refresh_client_hits: u64,
    pub refresh_client_misses: u64,
    pub refresh_client_cache_saturated: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenRefreshAdmissionRuntimeResponse {
    pub authority: String,
    pub breaker_state: String,
    pub next_probe_after_ms: u64,
    pub configured_rpm: u32,
    pub configured_burst: u32,
    pub admitted: u64,
    pub rate_limited: u64,
    pub coordination_rejected: u64,
    pub redis_errors: u64,
    pub last_retry_after_ms: u64,
    pub remaining_milli_tokens: u64,
}

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
    /// 单实例、单个请求 API Key 的准入配置；不表示跨实例聚合上限。
    pub request_admission: RequestAdmissionConfig,
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
    pub kiro_upstream_response_timeout_secs: u64,
    pub kiro_upstream_stream_idle_timeout_secs: u64,
    pub kiro_upstream_stream_retry_enabled: bool,
    pub kiro_upstream_stream_retry_max_attempts: u32,
    pub inference_upstream_max_attempts: u32,
    pub auxiliary_upstream_max_attempts: u32,
    pub auxiliary_upstream_max_concurrent_requests: u32,
    pub auxiliary_upstream_runtime: AuxiliaryUpstreamRuntimeResponse,
    pub token_refresh_max_rpm: u32,
    pub token_refresh_burst: u32,
    pub token_refresh_admission_runtime: TokenRefreshAdmissionRuntimeResponse,
    pub kiro_upstream_stream_retry_on_idle_timeout: bool,
    pub kiro_upstream_stream_retry_on_read_error: bool,
    pub kiro_upstream_stream_retry_on_status_error: bool,
    pub credential_retry_max_attempts: u32,
    pub credential_prompt_logic_retry_enabled: bool,
    pub credential_prompt_logic_retry_max_attempts: u32,
    pub credential_in_flight_lease_max_secs: u64,
    pub dispatch_global_max_concurrent_requests: u32,
    pub dispatch_max_queued_requests: u32,
    pub weighted_capacity: WeightedCapacityConfig,
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
    pub selection_failure_sample_limit: usize,
    pub selection_failure_record_enabled: bool,
    pub compression_enabled: bool,
    pub whitespace_compression: bool,
    pub image_processing: ImageProcessingConfig,
    pub body_conversion: BodyConversionConfig,
    pub prompt_steering: PromptSteeringConfig,
    pub missing_max_tokens: MissingMaxTokensConfig,
    pub payload_guard_enabled: bool,
    pub payload_guard_mode: PayloadGuardMode,
    pub payload_guard_max_bytes: u64,
    pub payload_guard_safety_margin_bytes: u64,
    pub payload_guard_trim_history: bool,
    pub payload_guard_external_enabled: bool,
    pub kiro_cache_point_enabled: bool,
    pub kiro_cache_point_tools_only: bool,
    pub kiro_cache_point_record_plan: bool,
    pub payload_shaping: PayloadShapingConfig,
    pub prompt_cache_target_read_ratio: f64,
    pub prompt_cache_token_scale: f64,
    pub prompt_cache_max_simulated_input_tokens: i32,
    pub prompt_cache_cap_jitter_min_tokens: i32,
    pub prompt_cache_cap_jitter_max_tokens: i32,
    pub prompt_cache_scale_min_input_tokens: i32,
    pub prompt_cache_creation_control: PromptCacheCreationControlConfig,
    pub prompt_cache_max_entries_per_account: usize,
    pub prompt_cache_max_entries_global: usize,
    pub prompt_cache_entry_ttl_secs: u64,
    pub prompt_cache_estimated_bytes_limit: u64,
    pub reported_usage: ReportedUsageConfig,
    pub cache_policy: CachePolicyConfig,
    pub defined_cache_routes: Vec<String>,
    pub external_pools: ExternalPoolsConfig,
    pub high_cache_threshold: i32,
    pub compat_profile: CompatProfile,
    pub kiro_agent_mode_strategy: KiroAgentModeStrategy,
    pub model_resolution_mode: ModelResolutionMode,
    pub model_mapping: ModelMappingConfig,
    pub extract_thinking: bool,
    pub thinking_trigger_mode: ThinkingTriggerMode,
    pub expose_proxy_warnings: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateRuntimeConfigRequest {
    pub credential_rpm: u32,
    #[serde(default)]
    pub request_admission: Option<RequestAdmissionConfig>,
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
    pub kiro_upstream_response_timeout_secs: Option<u64>,
    #[serde(default)]
    pub kiro_upstream_stream_idle_timeout_secs: Option<u64>,
    #[serde(default)]
    pub kiro_upstream_stream_retry_enabled: Option<bool>,
    #[serde(default)]
    pub kiro_upstream_stream_retry_max_attempts: Option<u32>,
    #[serde(default)]
    pub inference_upstream_max_attempts: Option<u32>,
    #[serde(default)]
    pub auxiliary_upstream_max_attempts: Option<u32>,
    #[serde(default)]
    pub auxiliary_upstream_max_concurrent_requests: Option<u32>,
    #[serde(default)]
    pub token_refresh_max_rpm: Option<u32>,
    #[serde(default)]
    pub token_refresh_burst: Option<u32>,
    #[serde(default)]
    pub kiro_upstream_stream_retry_on_idle_timeout: Option<bool>,
    #[serde(default)]
    pub kiro_upstream_stream_retry_on_read_error: Option<bool>,
    #[serde(default)]
    pub kiro_upstream_stream_retry_on_status_error: Option<bool>,
    #[serde(default)]
    pub credential_retry_max_attempts: Option<u32>,
    #[serde(default)]
    pub credential_prompt_logic_retry_enabled: Option<bool>,
    #[serde(default)]
    pub credential_prompt_logic_retry_max_attempts: Option<u32>,
    #[serde(default)]
    pub credential_in_flight_lease_max_secs: Option<u64>,
    #[serde(default)]
    pub dispatch_global_max_concurrent_requests: Option<u32>,
    #[serde(default)]
    pub dispatch_max_queued_requests: Option<u32>,
    #[serde(default)]
    pub weighted_capacity: Option<WeightedCapacityConfig>,
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
    #[serde(default)]
    pub selection_failure_sample_limit: Option<usize>,
    #[serde(default)]
    pub selection_failure_record_enabled: Option<bool>,
    pub compression_enabled: bool,
    #[serde(default = "default_true")]
    pub whitespace_compression: bool,
    #[serde(default)]
    pub image_processing: Option<ImageProcessingConfig>,
    #[serde(default)]
    pub body_conversion: Option<BodyConversionConfig>,
    #[serde(default)]
    pub prompt_steering: Option<PromptSteeringConfig>,
    #[serde(default)]
    pub missing_max_tokens: Option<MissingMaxTokensConfig>,
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
    pub payload_guard_external_enabled: Option<bool>,
    #[serde(default)]
    pub kiro_cache_point_enabled: Option<bool>,
    #[serde(default)]
    pub kiro_cache_point_tools_only: Option<bool>,
    #[serde(default)]
    pub kiro_cache_point_record_plan: Option<bool>,
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
    pub prompt_cache_max_entries_per_account: Option<usize>,
    #[serde(default)]
    pub prompt_cache_max_entries_global: Option<usize>,
    #[serde(default)]
    pub prompt_cache_entry_ttl_secs: Option<u64>,
    #[serde(default)]
    pub prompt_cache_estimated_bytes_limit: Option<u64>,
    #[serde(default)]
    pub reported_usage: Option<ReportedUsageConfig>,
    #[serde(default)]
    pub cache_policy: Option<CachePolicyConfig>,
    #[serde(default)]
    pub defined_cache_routes: Option<Vec<String>>,
    #[serde(default)]
    pub external_pools: Option<ExternalPoolsConfig>,
    #[serde(default)]
    pub high_cache_threshold: Option<i32>,
    #[serde(default)]
    pub compat_profile: Option<CompatProfile>,
    #[serde(default)]
    pub kiro_agent_mode_strategy: Option<KiroAgentModeStrategy>,
    #[serde(default)]
    pub model_resolution_mode: Option<ModelResolutionMode>,
    #[serde(default)]
    pub model_mapping: Option<ModelMappingConfig>,
    #[serde(default)]
    pub extract_thinking: Option<bool>,
    #[serde(default)]
    pub thinking_trigger_mode: Option<ThinkingTriggerMode>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_credential_request_accepts_snake_case_import_fields() {
        let json = serde_json::json!({
            "refresh_token": "fake-refresh-token",
            "access_token": "fake-access-token",
            "expires_at": "2026-06-28T00:00:00Z",
            "auth_method": "idc",
            "client_id": "fake-client-id",
            "client_secret": "fake-client-secret",
            "token_endpoint": "https://login.example.com/oauth2/v2.0/token",
            "issuer_url": "https://login.example.com/tenant/v2.0",
            "scopes": "offline_access codewhisperer:conversations",
            "profile_arn": "arn:aws:codewhisperer:us-east-1:123456789012:profile/FAKE",
            "region": "us-east-1",
            "auth_region": "us-west-2",
            "api_region": "eu-west-1",
            "machine_id": "fake-machine-id",
            "max_concurrent_requests": 3
        });

        let req: AddCredentialRequest = serde_json::from_value(json).unwrap();

        assert_eq!(req.access_token.as_deref(), Some("fake-access-token"));
        assert_eq!(req.expires_at.as_deref(), Some("2026-06-28T00:00:00Z"));
        assert_eq!(req.refresh_token.as_deref(), Some("fake-refresh-token"));
        assert_eq!(req.auth_method, "idc");
        assert_eq!(req.client_id.as_deref(), Some("fake-client-id"));
        assert_eq!(req.client_secret.as_deref(), Some("fake-client-secret"));
        assert_eq!(
            req.token_endpoint.as_deref(),
            Some("https://login.example.com/oauth2/v2.0/token")
        );
        assert_eq!(
            req.issuer_url.as_deref(),
            Some("https://login.example.com/tenant/v2.0")
        );
        assert_eq!(
            req.scopes.as_deref(),
            Some("offline_access codewhisperer:conversations")
        );
        assert_eq!(
            req.profile_arn.as_deref(),
            Some("arn:aws:codewhisperer:us-east-1:123456789012:profile/FAKE")
        );
        assert_eq!(req.region.as_deref(), Some("us-east-1"));
        assert_eq!(req.auth_region.as_deref(), Some("us-west-2"));
        assert_eq!(req.api_region.as_deref(), Some("eu-west-1"));
        assert_eq!(req.machine_id.as_deref(), Some("fake-machine-id"));
        assert_eq!(req.max_concurrent_requests, Some(3));
        assert_eq!(req.auto_discover_supported_models, None);
    }

    #[test]
    fn add_credential_request_accepts_camel_case_import_fields() {
        let json = serde_json::json!({
            "refreshToken": "fake-refresh-token",
            "accessToken": "fake-access-token",
            "expired": "2026-06-28T00:00:00Z",
            "authMethod": "idc",
            "clientId": "fake-client-id",
            "clientSecret": "fake-client-secret",
            "tokenEndpoint": "https://login.example.com/oauth2/v2.0/token",
            "issuerUrl": "https://login.example.com/tenant/v2.0",
            "scopes": "offline_access codewhisperer:completions",
            "profileArn": "arn:aws:codewhisperer:us-east-1:123456789012:profile/FAKE",
            "region": "us-east-1",
            "authRegion": "us-west-2",
            "apiRegion": "eu-west-1",
            "machineId": "fake-machine-id",
            "maxConcurrentRequests": 3
        });

        let req: AddCredentialRequest = serde_json::from_value(json).unwrap();

        assert_eq!(req.access_token.as_deref(), Some("fake-access-token"));
        assert_eq!(req.expires_at.as_deref(), Some("2026-06-28T00:00:00Z"));
        assert_eq!(req.refresh_token.as_deref(), Some("fake-refresh-token"));
        assert_eq!(req.auth_method, "idc");
        assert_eq!(req.client_id.as_deref(), Some("fake-client-id"));
        assert_eq!(req.client_secret.as_deref(), Some("fake-client-secret"));
        assert_eq!(
            req.token_endpoint.as_deref(),
            Some("https://login.example.com/oauth2/v2.0/token")
        );
        assert_eq!(
            req.issuer_url.as_deref(),
            Some("https://login.example.com/tenant/v2.0")
        );
        assert_eq!(
            req.scopes.as_deref(),
            Some("offline_access codewhisperer:completions")
        );
        assert_eq!(
            req.profile_arn.as_deref(),
            Some("arn:aws:codewhisperer:us-east-1:123456789012:profile/FAKE")
        );
        assert_eq!(req.region.as_deref(), Some("us-east-1"));
        assert_eq!(req.auth_region.as_deref(), Some("us-west-2"));
        assert_eq!(req.api_region.as_deref(), Some("eu-west-1"));
        assert_eq!(req.machine_id.as_deref(), Some("fake-machine-id"));
        assert_eq!(req.max_concurrent_requests, Some(3));
        assert_eq!(req.auto_discover_supported_models, None);
    }

    #[test]
    fn import_requests_default_model_autodiscovery_off_but_accept_override() {
        let add_req: AddCredentialRequest = serde_json::from_value(serde_json::json!({
            "kiroApiKey": "ksk_fake",
            "autoDiscoverSupportedModels": true
        }))
        .unwrap();
        assert_eq!(add_req.auto_discover_supported_models, Some(true));

        let batch_req: BatchCredentialImportRequest = serde_json::from_value(serde_json::json!({
            "credentials": [{ "kiroApiKey": "ksk_fake" }]
        }))
        .unwrap();
        assert!(!batch_req.auto_discover_supported_models);

        let batch_override: BatchCredentialImportRequest =
            serde_json::from_value(serde_json::json!({
                "autoDiscoverSupportedModels": true,
                "credentials": [{ "kiroApiKey": "ksk_fake" }]
            }))
            .unwrap();
        assert!(batch_override.auto_discover_supported_models);
    }

    #[test]
    fn validate_external_credentials_defaults_liveness_on_but_accepts_override() {
        let default_req: ValidateExternalCredentialsRequest =
            serde_json::from_value(serde_json::json!({
                "credentials": [{ "kiroApiKey": "ksk_fake" }]
            }))
            .unwrap();
        assert!(default_req.query_subscription);
        assert!(default_req.query_usage);
        assert!(default_req.check_liveness);

        let override_req: ValidateExternalCredentialsRequest =
            serde_json::from_value(serde_json::json!({
                "checkLiveness": false,
                "credentials": [{ "kiroApiKey": "ksk_fake" }]
            }))
            .unwrap();
        assert!(!override_req.check_liveness);
    }

    #[test]
    fn add_credential_request_leaves_missing_auth_method_for_server_inference() {
        let json = serde_json::json!({
            "refreshToken": "fake-refresh-token",
            "accessToken": "fake-access-token",
            "clientId": "fake-client-id",
            "clientSecret": "fake-client-secret",
            "profileArn": "arn:aws:codewhisperer:us-east-1:123456789012:profile/FAKE"
        });

        let req: AddCredentialRequest = serde_json::from_value(json).unwrap();

        assert_eq!(req.auth_method, "");
        assert_eq!(req.client_id.as_deref(), Some("fake-client-id"));
        assert_eq!(req.client_secret.as_deref(), Some("fake-client-secret"));
    }

    #[test]
    fn batch_update_credentials_request_accepts_priority_and_clear_overrides() {
        let json = serde_json::json!({
            "ids": [1, 2, 2],
            "priority": { "priority": 0 },
            "concurrency": { "maxConcurrentRequests": null },
            "rpm": { "rpm": null }
        });

        let req: BatchUpdateCredentialsRequest = serde_json::from_value(json).unwrap();

        assert_eq!(req.ids, vec![1, 2, 2]);
        assert_eq!(req.priority.unwrap().priority, 0);
        assert_eq!(req.concurrency.unwrap().max_concurrent_requests, None);
        assert_eq!(req.rpm.unwrap().rpm, None);
        assert!(req.regions.is_none());
        assert!(req.proxy.is_none());
    }
}
