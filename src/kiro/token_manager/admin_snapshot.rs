use serde::Serialize;

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
