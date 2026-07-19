use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use crate::kiro::model::credentials::KiroCredentials;
use crate::storage::postgres::ProxyResourceRow;
use crate::storage::redis_cache::SchedulerHealthState;

use super::types::InFlightKind;

#[derive(Debug, Clone)]
pub(super) struct ProxyResourceRuntime {
    pub(super) id: u64,
    pub(super) name: String,
    pub(super) proxy_url: String,
    pub(super) proxy_username: Option<String>,
    pub(super) proxy_password: Option<String>,
    pub(super) enabled: bool,
}

#[derive(Debug, Clone)]
pub(super) enum ProxyResourceAvailability {
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

/// 单个凭据条目的状态
pub(super) struct CredentialEntry {
    /// 凭据唯一 ID
    pub(super) id: u64,
    /// 凭据信息
    pub(super) credentials: KiroCredentials,
    /// API 调用连续失败次数
    pub(super) failure_count: u32,
    /// Token 刷新连续失败次数
    pub(super) refresh_failure_count: u32,
    /// Last authoritative PgSQL runtime-state revision applied by this process.
    pub(super) runtime_revision: u64,
    /// Reset generation used to fence runtime mutations created before an Admin reset/update.
    pub(super) runtime_generation: u64,
    /// A correctness-critical PgSQL mutation is waiting to be replayed. A pending success reset
    /// alone is safe to replay without quarantining the credential.
    pub(super) runtime_persistence_degraded: bool,
    /// 是否已禁用
    pub(super) disabled: bool,
    /// 禁用原因（用于区分手动禁用 vs 自动禁用，便于自愈）
    pub(super) disabled_reason: Option<DisabledReason>,
    /// API 调用成功次数
    pub(super) success_count: u64,
    /// 调度器实际选中该凭据的总次数。
    pub(super) total_selection_count: u64,
    /// 最后一次 API 调用时间（RFC3339 格式）
    pub(super) last_used_at: Option<String>,
    /// 临时冷却到期时间（上游 Retry-After/瞬态错误触发），不持久化。
    pub(super) cooldown_until: Option<Instant>,
    /// 临时冷却原因，便于诊断。
    pub(super) cooldown_reason: Option<String>,
    /// 按真实上游模型维度同步的 Redis 冷却镜像。
    pub(super) model_cooldowns: HashMap<String, CredentialModelCooldown>,
    /// 下一次本地限流允许发送请求的时间，不持久化。
    pub(super) rate_limit_available_at: Option<Instant>,
    /// 生成当前限流时间的 effective RPM；配置变化时用于拒绝旧状态。
    pub(super) rate_limit_rpm: Option<u32>,
    /// 生成当前限流时间的调度 lease；仅 owner 可回滚未实际发出的请求。
    pub(super) rate_limit_owner_lease_id: Option<u64>,
    /// Redis pacing deadline，作为跨连接乱序响应的单调 reservation 身份。
    pub(super) rate_limit_redis_deadline_ms: Option<i64>,
    /// 本进程已选中该凭据、但 Redis admission 尚未确认的临时 pacing 占位。
    ///
    /// 该状态必须与 Redis 权威 pacing 分离，否则后台 Redis 快照可能在 EVAL 返回前
    /// 清掉本地占位，使同一进程的请求再次选中相同凭据并形成重选风暴。
    pub(super) pending_redis_admission: Option<PendingRedisAdmission>,
    /// 当前正在使用该凭据的请求数，不持久化。
    pub(super) in_flight_requests: u32,
    /// 当前正在使用该凭据的请求 lease，不持久化。
    pub(super) in_flight_leases: Vec<InFlightLease>,
    /// 预热剩余请求数。仅影响 balanced 选择，不伪造 success_count。
    pub(super) warmup_remaining: u32,
    /// 近期上游健康状态；Redis 部署下在调度前同步。
    pub(super) health: SchedulerHealthState,
    /// 按真实上游模型维度同步的 Redis 健康状态镜像。
    pub(super) model_health: HashMap<String, SchedulerHealthState>,
    /// 本进程内的近期调度选中事件；无 Redis 时用于计算短窗口调度压力。
    pub(super) selection_events: VecDeque<Instant>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct PendingRedisAdmission {
    pub(super) lease_id: u64,
    pub(super) rate_limit_available_at: Option<Instant>,
    pub(super) baseline_in_flight_requests: u32,
    #[allow(dead_code)]
    pub(super) baseline_global_in_flight_requests: u32,
}

#[derive(Debug, Clone)]
pub(super) struct CredentialModelCooldown {
    pub(super) model: String,
    pub(super) until: Instant,
    pub(super) reason: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct InFlightLease {
    pub(super) id: u64,
    pub(super) acquired_at: Instant,
    pub(super) last_seen_at: Instant,
    pub(super) kind: InFlightKind,
    pub(super) weight_units: u32,
    /// `true` when this process owns the guard that created the lease. Redis
    /// snapshots also contain leases owned by other instances; those mirrors
    /// must disappear locally as soon as Redis no longer reports them.
    pub(super) locally_owned: bool,
}

/// 会话到凭据的粘性绑定。
#[derive(Clone, PartialEq, Eq)]
pub(super) struct SessionBinding {
    pub(super) credential_id: u64,
    pub(super) last_used_at: DateTime<Utc>,
    pub(super) soft_failure_count: u32,
    /// A local-first binding is authoritative until its queued Redis write finishes.
    pub(super) redis_persist_pending: bool,
}

/// 禁用原因
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DisabledReason {
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
    pub(super) fn as_str(self) -> &'static str {
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

    pub(super) fn from_str(value: &str) -> Option<Self> {
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

    pub(super) fn label(self) -> &'static str {
        match self {
            DisabledReason::Manual => "手动禁用",
            DisabledReason::TooManyFailures => "连续 API 调用失败",
            DisabledReason::TooManyRefreshFailures => "连续 Token 刷新失败",
            DisabledReason::QuotaExceeded => "额度耗尽",
            DisabledReason::InvalidRefreshToken => "refreshToken 失效",
            DisabledReason::InvalidConfig => "凭据配置无效",
            DisabledReason::TemporarilySuspended => "临时风控/暂停",
            DisabledReason::AccountSuspended => "账号暂停/封禁",
            DisabledReason::AccountLocked => "账号锁定",
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

impl CredentialRiskControlReason {
    pub(super) fn disabled_reason(self) -> DisabledReason {
        match self {
            CredentialRiskControlReason::TemporarilySuspended => {
                DisabledReason::TemporarilySuspended
            }
            CredentialRiskControlReason::AccountSuspended => DisabledReason::AccountSuspended,
            CredentialRiskControlReason::AccountLocked => DisabledReason::AccountLocked,
        }
    }

    pub(super) fn event_reason(self) -> &'static str {
        self.disabled_reason().as_str()
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            CredentialRiskControlReason::TemporarilySuspended => "临时风控/暂停",
            CredentialRiskControlReason::AccountSuspended => "账号暂停/封禁",
            CredentialRiskControlReason::AccountLocked => "账号锁定",
        }
    }
}

/// 统计数据持久化条目
#[derive(Serialize, Deserialize)]
pub(super) struct StatsEntry {
    pub(super) success_count: u64,
    #[serde(default)]
    pub(super) selection_count: u64,
    pub(super) last_used_at: Option<String>,
}
