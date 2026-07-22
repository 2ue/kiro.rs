use std::time::Duration as StdDuration;

use crate::kiro::model::credentials::KiroCredentials;

use super::concurrency::InFlightLeaseGuard;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InFlightKind {
    Api,
    Stream,
    Mcp,
    Test,
}

impl InFlightKind {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            InFlightKind::Api => "api",
            InFlightKind::Stream => "stream",
            InFlightKind::Mcp => "mcp",
            InFlightKind::Test => "test",
        }
    }

    pub(super) fn from_str(value: &str) -> Self {
        match value {
            "stream" => InFlightKind::Stream,
            "mcp" => InFlightKind::Mcp,
            "test" => InFlightKind::Test,
            _ => InFlightKind::Api,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CredentialAuthUpdate {
    pub access_token: Option<String>,
    pub expires_at: Option<String>,
    pub refresh_token: Option<String>,
    pub auth_method: Option<String>,
    pub provider: Option<String>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub token_endpoint: Option<String>,
    pub issuer_url: Option<String>,
    pub scopes: Option<String>,
    pub kiro_api_key: Option<String>,
    pub region: Option<String>,
    pub auth_region: Option<String>,
    pub api_region: Option<String>,
    pub machine_id: Option<String>,
    pub email: Option<String>,
    pub endpoint: Option<String>,
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
    pub(super) fn as_str(self) -> &'static str {
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

pub const EXTERNAL_CREDENTIAL_CONTEXT_ID: u64 = 0;

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
    pub(super) in_flight_lease: Option<InFlightLeaseGuard>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquireMode {
    WaitForCapacity,
    FailFastOnCapacity,
    FailFastOnCapacityWaitForRedis(StdDuration),
    WaitForCapacityMax(StdDuration),
}

impl AcquireMode {
    pub(super) fn is_fail_fast(self) -> bool {
        matches!(
            self,
            Self::FailFastOnCapacity | Self::FailFastOnCapacityWaitForRedis(_)
        )
    }

    pub(super) fn is_redis_degraded_fail_fast(self) -> bool {
        matches!(self, Self::FailFastOnCapacity)
    }

    pub(super) fn max_wait_override(self) -> Option<StdDuration> {
        match self {
            Self::FailFastOnCapacityWaitForRedis(duration) | Self::WaitForCapacityMax(duration) => {
                Some(duration)
            }
            _ => None,
        }
    }
}
