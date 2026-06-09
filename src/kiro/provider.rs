//! Kiro API Provider
//!
//! 核心组件，负责与 Kiro API 通信
//! 支持流式和非流式请求
//! 支持多凭据故障转移和重试
//! 支持按凭据级 endpoint 切换不同 Kiro API 端点

use chrono::Utc;
use reqwest::Client;
use reqwest::header::{CONTENT_TYPE, HeaderMap};
use std::collections::{HashMap, HashSet};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use tokio::time::sleep;

use crate::http_client::{ProxyConfig, build_client};
use crate::kiro::call_trace::{KiroCallError, KiroCredentialAttempt, summarize_attempts};
use crate::kiro::endpoint::{KiroEndpoint, RequestContext};
use crate::kiro::machine_id;
use crate::kiro::model::available_models::{KiroAvailableModel, KiroAvailableModelsResponse};
use crate::kiro::model::credentials::KiroCredentials;
use crate::kiro::token_manager::{
    AcquireMode, CallContext, CredentialRiskControlReason, InFlightKind, InFlightLeaseGuard,
    ManagerSnapshot, MultiTokenManager, TransientFailureKind,
};
use crate::model::config::{Config, TlsBackend};
use parking_lot::Mutex;

/// 自动模式下的小账号池最少尝试次数，保持既有 1-3 个账号时最多 9 次的行为。
const MIN_AUTO_RETRY_ATTEMPTS: usize = 9;

fn effective_payload_guard_limit_for_logging(config: &Config) -> usize {
    const MIN_EFFECTIVE_LIMIT_BYTES: usize = 64 * 1024;
    let max_bytes = config.payload_guard_max_bytes;
    if max_bytes == 0 || config.payload_guard_safety_margin_bytes == 0 {
        return max_bytes;
    }
    if max_bytes <= MIN_EFFECTIVE_LIMIT_BYTES {
        return max_bytes;
    }
    let margin = config
        .payload_guard_safety_margin_bytes
        .min(max_bytes.saturating_sub(MIN_EFFECTIVE_LIMIT_BYTES));
    max_bytes.saturating_sub(margin)
}

fn should_log_upstream_body_size_at_info(body_bytes: usize, config: &Config) -> bool {
    let payload_guard_limit = effective_payload_guard_limit_for_logging(config);
    let near_payload_guard_limit =
        payload_guard_limit > 0 && body_bytes > payload_guard_limit.saturating_mul(70) / 100;
    let compression_enabled =
        config.compression.enabled && config.compression.whitespace_compression;

    near_payload_guard_limit || compression_enabled
}

/// Kiro API Provider
///
/// 核心组件，负责与 Kiro API 通信
/// 支持多凭据故障转移和重试机制
/// 按凭据 `endpoint` 字段选择 [`KiroEndpoint`] 实现
pub struct KiroProvider {
    token_manager: Arc<MultiTokenManager>,
    /// 全局代理配置（用于凭据无自定义代理时的回退）
    global_proxy: Option<ProxyConfig>,
    /// Client 缓存：key = effective proxy config, value = reqwest::Client
    /// 不同代理配置的凭据使用不同的 Client，共享相同代理的凭据复用 Client
    client_cache: Mutex<HashMap<Option<ProxyConfig>, Client>>,
    /// TLS 后端配置
    tls_backend: TlsBackend,
    /// 端点实现注册表（key: endpoint 名称）
    endpoints: HashMap<String, Arc<dyn KiroEndpoint>>,
    /// 默认端点名称（凭据未指定 endpoint 时使用）
    default_endpoint: String,
}

pub struct KiroApiResponse {
    response: reqwest::Response,
    completion: KiroApiCompletion,
}

struct ApiCallResponse {
    response: reqwest::Response,
    credential_id: u64,
    in_flight_lease: Option<InFlightLeaseGuard>,
    session_id: Option<String>,
    model: Option<String>,
    sticky_bound: bool,
    fallback_from_sticky: bool,
    attempts: Vec<KiroCredentialAttempt>,
    started_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CredentialAuthFailureDecision {
    ForceRefreshRetry,
    Retry { excluded_current: bool },
    Exhausted,
}

/// 非流式调用完成上报器。
///
/// 非流式响应头返回后，body 读取和事件解析仍可能失败或被取消。
/// 这个 guard 用 Drop 兜底释放并发槽，避免调用链中途退出导致凭据长期不可调度。
pub struct KiroApiCompletion {
    token_manager: Arc<MultiTokenManager>,
    credential_id: u64,
    in_flight_lease: Mutex<Option<InFlightLeaseGuard>>,
    session_id: Option<String>,
    model: Option<String>,
    sticky_bound: bool,
    fallback_from_sticky: bool,
    attempts: Vec<KiroCredentialAttempt>,
    reported: AtomicBool,
    started_at: Instant,
}

impl KiroApiCompletion {
    fn new(
        token_manager: Arc<MultiTokenManager>,
        credential_id: u64,
        in_flight_lease: Option<InFlightLeaseGuard>,
        session_id: Option<String>,
        model: Option<String>,
        sticky_bound: bool,
        fallback_from_sticky: bool,
        attempts: Vec<KiroCredentialAttempt>,
        started_at: Instant,
    ) -> Self {
        Self {
            token_manager,
            credential_id,
            in_flight_lease: Mutex::new(in_flight_lease),
            session_id,
            model,
            sticky_bound,
            fallback_from_sticky,
            attempts,
            reported: AtomicBool::new(false),
            started_at,
        }
    }

    pub fn report_success(&self) {
        if self.reported.swap(true, Ordering::AcqRel) {
            return;
        }
        if self.in_flight_lease.lock().take().is_some() {
            self.token_manager.report_success_for_session_with_latency(
                self.credential_id,
                self.model.as_deref(),
                self.session_id.as_deref(),
                Some(self.started_at.elapsed()),
            );
        }
    }

    pub fn release(&self) {
        if self.reported.swap(true, Ordering::AcqRel) {
            return;
        }
        self.in_flight_lease.lock().take();
    }

    pub fn credential_id(&self) -> u64 {
        self.credential_id
    }

    pub fn sticky_bound(&self) -> bool {
        self.sticky_bound
    }

    pub fn fallback_from_sticky(&self) -> bool {
        self.fallback_from_sticky
    }

    pub fn attempts(&self) -> &[KiroCredentialAttempt] {
        &self.attempts
    }
}

impl Drop for KiroApiCompletion {
    fn drop(&mut self) {
        if self.reported.load(Ordering::Acquire) {
            return;
        }
        self.release();
    }
}

impl KiroApiResponse {
    pub fn credential_id(&self) -> u64 {
        self.completion.credential_id()
    }

    pub fn sticky_bound(&self) -> bool {
        self.completion.sticky_bound()
    }

    pub fn fallback_from_sticky(&self) -> bool {
        self.completion.fallback_from_sticky()
    }

    pub fn attempts(&self) -> &[KiroCredentialAttempt] {
        self.completion.attempts()
    }

    pub fn into_parts(self) -> (reqwest::Response, KiroApiCompletion) {
        (self.response, self.completion)
    }
}

/// 流式调用完成上报器。
///
/// Provider 只能确认上游返回了成功响应头；流式 body 是否完整消费需要
/// SSE 处理链路在 EOF、读错误或 idle timeout 时回报。
pub struct KiroStreamCompletion {
    token_manager: Arc<MultiTokenManager>,
    credential_id: u64,
    in_flight_lease: Mutex<Option<InFlightLeaseGuard>>,
    session_id: Option<String>,
    model: Option<String>,
    sticky_bound: bool,
    fallback_from_sticky: bool,
    attempts: Vec<KiroCredentialAttempt>,
    reported: AtomicBool,
    started_at: Instant,
}

impl KiroStreamCompletion {
    fn new(
        token_manager: Arc<MultiTokenManager>,
        credential_id: u64,
        in_flight_lease: Option<InFlightLeaseGuard>,
        session_id: Option<String>,
        model: Option<String>,
        sticky_bound: bool,
        fallback_from_sticky: bool,
        attempts: Vec<KiroCredentialAttempt>,
        started_at: Instant,
    ) -> Self {
        Self {
            token_manager,
            credential_id,
            in_flight_lease: Mutex::new(in_flight_lease),
            session_id,
            model,
            sticky_bound,
            fallback_from_sticky,
            attempts,
            reported: AtomicBool::new(false),
            started_at,
        }
    }

    /// 上游流正常 EOF 后调用，计入成功并清理 sticky 软失败计数。
    pub fn report_success(&self) {
        if self.reported.swap(true, Ordering::AcqRel) {
            return;
        }
        self.token_manager.report_success_for_session_with_latency(
            self.credential_id,
            self.model.as_deref(),
            self.session_id.as_deref(),
            Some(self.started_at.elapsed()),
        );
        self.in_flight_lease.lock().take();
    }

    /// 上游流中断、idle timeout 或上游错误事件时调用。
    ///
    /// 这里不调用 `report_failure`，避免瞬态流读取问题直接禁用账号。
    pub fn report_soft_failure(&self) {
        if self.reported.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(session_id) = self.session_id.as_deref() {
            self.token_manager
                .record_session_soft_failure(session_id, self.credential_id);
        }
        self.in_flight_lease.lock().take();
    }

    /// 上游流读取错误、idle timeout 或上游错误事件时调用，并让调度器短暂避开该凭据。
    pub fn report_upstream_stream_failure(&self, reason: impl Into<String>) {
        if self.reported.swap(true, Ordering::AcqRel) {
            return;
        }
        let reason = reason.into();
        if let Err(err) = self.token_manager.report_transient_failure_kind(
            self.credential_id,
            self.model.as_deref(),
            TransientFailureKind::Stream,
            None,
            format!("stream_error {}", reason),
        ) {
            tracing::warn!(
                credential_id = self.credential_id,
                "记录上游流式失败冷却失败: {}",
                err
            );
        }
        if let Some(session_id) = self.session_id.as_deref() {
            self.token_manager
                .record_session_soft_failure(session_id, self.credential_id);
        }
        self.in_flight_lease.lock().take();
    }

    pub fn touch(&self) {
        if let Some(lease) = self.in_flight_lease.lock().as_ref() {
            lease.touch();
        }
    }

    pub fn credential_id(&self) -> u64 {
        self.credential_id
    }

    pub fn sticky_bound(&self) -> bool {
        self.sticky_bound
    }

    pub fn fallback_from_sticky(&self) -> bool {
        self.fallback_from_sticky
    }

    pub fn attempts(&self) -> &[KiroCredentialAttempt] {
        &self.attempts
    }
}

impl Drop for KiroStreamCompletion {
    fn drop(&mut self) {
        if self.reported.load(Ordering::Acquire) {
            return;
        }
        self.report_soft_failure();
    }
}

pub struct KiroStreamResponse {
    response: reqwest::Response,
    completion: KiroStreamCompletion,
}

impl KiroStreamResponse {
    pub fn into_parts(self) -> (reqwest::Response, KiroStreamCompletion) {
        (self.response, self.completion)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Instant;

    use chrono::{Duration, Utc};

    use super::{CredentialRiskControlReason, KiroProvider, KiroStreamCompletion};
    use crate::kiro::model::credentials::KiroCredentials;
    use crate::kiro::token_manager::MultiTokenManager;
    use crate::model::config::Config;

    #[test]
    fn extracts_model_and_conversation_id_from_kiro_request() {
        let body = r#"{
            "conversationState": {
                "conversationId": "session-123",
                "currentMessage": {
                    "userInputMessage": {
                        "modelId": "claude-opus-4"
                    }
                }
            }
        }"#;

        assert_eq!(
            KiroProvider::test_extract_conversation_id_from_request(body).as_deref(),
            Some("session-123")
        );
        assert_eq!(
            KiroProvider::test_extract_model_from_request(body).as_deref(),
            Some("claude-opus-4")
        );
    }

    #[test]
    fn ignores_blank_conversation_id() {
        let body = r#"{"conversationState":{"conversationId":"  "}}"#;

        assert_eq!(
            KiroProvider::test_extract_conversation_id_from_request(body),
            None
        );
    }

    #[test]
    fn credential_log_label_always_includes_id() {
        assert_eq!(KiroProvider::format_credential_log_label(6, None), "#6");
        assert_eq!(
            KiroProvider::format_credential_log_label(6, Some("prevotrj@gmail.com".to_string())),
            "#6 prevotrj@gmail.com"
        );
        assert_eq!(
            KiroProvider::format_credential_log_label(6, Some("#6 custom".to_string())),
            "#6 custom"
        );
    }

    #[test]
    fn detects_risk_controlled_upstream_errors() {
        assert_eq!(
            KiroProvider::detect_risk_control_error(
                reqwest::StatusCode::FORBIDDEN,
                r#"{"reason":"TEMPORARILY_SUSPENDED","message":"User ID is temporarily suspended"}"#
            ),
            Some(CredentialRiskControlReason::TemporarilySuspended)
        );
        assert_eq!(
            KiroProvider::detect_risk_control_error(
                reqwest::StatusCode::FORBIDDEN,
                r#"{"message":"Your User ID temporarily is suspended. We've locked your account as a security precaution.","reason":null}"#
            ),
            Some(CredentialRiskControlReason::TemporarilySuspended)
        );
        assert_eq!(
            KiroProvider::detect_risk_control_error(
                reqwest::StatusCode::FORBIDDEN,
                r#"{"__type":"AccountSuspendedException","message":"Account suspended"}"#
            ),
            Some(CredentialRiskControlReason::AccountSuspended)
        );
        assert_eq!(
            KiroProvider::detect_risk_control_error(
                reqwest::StatusCode::LOCKED,
                r#"{"message":"Locked"}"#
            ),
            Some(CredentialRiskControlReason::AccountLocked)
        );
        assert_eq!(
            KiroProvider::detect_risk_control_error(
                reqwest::StatusCode::FORBIDDEN,
                r#"{"message":"We've locked your account as a security precaution."}"#
            ),
            Some(CredentialRiskControlReason::AccountLocked)
        );
        assert_eq!(
            KiroProvider::detect_risk_control_error(
                reqwest::StatusCode::FORBIDDEN,
                r#"{"message":"The bearer token included in the request is invalid"}"#
            ),
            None
        );
        assert_eq!(
            KiroProvider::detect_risk_control_error(
                reqwest::StatusCode::FORBIDDEN,
                r#"{"message":"User is not authorized to make this call.","reason":null}"#
            ),
            None
        );
    }

    #[test]
    fn stream_completion_reports_success_once() {
        let mut cred = KiroCredentials::default();
        cred.access_token = Some("token".to_string());
        cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let manager = Arc::new(
            MultiTokenManager::new(Config::default(), vec![cred], None, None, false).unwrap(),
        );
        let completion = KiroStreamCompletion::new(
            manager.clone(),
            1,
            None,
            Some("session".into()),
            Some("claude-sonnet-4.5".into()),
            false,
            false,
            Vec::new(),
            Instant::now(),
        );

        completion.report_success();
        completion.report_success();

        let snapshot = manager.snapshot();
        assert_eq!(snapshot.entries[0].success_count, 1);
    }

    #[test]
    fn stream_completion_soft_failure_does_not_count_success() {
        let mut cred = KiroCredentials::default();
        cred.access_token = Some("token".to_string());
        cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let manager = Arc::new(
            MultiTokenManager::new(Config::default(), vec![cred], None, None, false).unwrap(),
        );
        let completion = KiroStreamCompletion::new(
            manager.clone(),
            1,
            None,
            Some("session".into()),
            Some("claude-sonnet-4.5".into()),
            false,
            false,
            Vec::new(),
            Instant::now(),
        );

        completion.report_soft_failure();
        completion.report_success();

        let snapshot = manager.snapshot();
        assert_eq!(snapshot.entries[0].success_count, 0);
    }

    #[test]
    fn stream_completion_upstream_failure_cools_down_credential() {
        let mut cred = KiroCredentials::default();
        cred.access_token = Some("token".to_string());
        cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let manager = Arc::new(
            MultiTokenManager::new(Config::default(), vec![cred], None, None, false).unwrap(),
        );
        let completion = KiroStreamCompletion::new(
            manager.clone(),
            1,
            None,
            Some("session".into()),
            Some("claude-sonnet-4.5".into()),
            false,
            false,
            Vec::new(),
            Instant::now(),
        );

        completion.report_upstream_stream_failure("upstream stream read error");
        completion.report_success();

        let snapshot = manager.snapshot();
        assert_eq!(snapshot.entries[0].success_count, 0);
        assert!(snapshot.entries[0].cooled_down);
        assert_eq!(snapshot.entries[0].failure_count, 0);
    }

    #[test]
    fn api_completion_drop_releases_in_flight_without_counting_success() {
        let mut cred = KiroCredentials::default();
        cred.access_token = Some("token".to_string());
        cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let manager = Arc::new(
            MultiTokenManager::new(Config::default(), vec![cred], None, None, false).unwrap(),
        );
        let lease = manager.acquire_in_flight_lease_for_test(1);

        {
            let _completion = super::KiroApiCompletion::new(
                manager.clone(),
                1,
                lease,
                Some("session".into()),
                Some("claude-sonnet-4.5".into()),
                false,
                false,
                Vec::new(),
                Instant::now(),
            );
        }

        let snapshot = manager.snapshot();
        assert_eq!(snapshot.entries[0].in_flight_requests, 0);
        assert_eq!(snapshot.entries[0].success_count, 0);
    }

    #[test]
    fn api_completion_report_success_once() {
        let mut cred = KiroCredentials::default();
        cred.access_token = Some("token".to_string());
        cred.expires_at = Some((Utc::now() + Duration::hours(1)).to_rfc3339());
        let manager = Arc::new(
            MultiTokenManager::new(Config::default(), vec![cred], None, None, false).unwrap(),
        );
        let lease = manager.acquire_in_flight_lease_for_test(1);

        let completion = super::KiroApiCompletion::new(
            manager.clone(),
            1,
            lease,
            Some("session".into()),
            Some("claude-sonnet-4.5".into()),
            false,
            false,
            Vec::new(),
            Instant::now(),
        );
        completion.report_success();
        completion.report_success();
        drop(completion);

        let snapshot = manager.snapshot();
        assert_eq!(snapshot.entries[0].in_flight_requests, 0);
        assert_eq!(snapshot.entries[0].success_count, 1);
    }

    #[test]
    fn accepts_kiro_json_labeled_event_stream_content_type() {
        assert!(KiroProvider::is_event_stream_content_type(
            "application/vnd.amazon.eventstream"
        ));
        assert!(KiroProvider::is_event_stream_content_type(
            "application/octet-stream; charset=utf-8"
        ));
        assert!(KiroProvider::is_event_stream_content_type(
            "application/json"
        ));
        assert!(!KiroProvider::is_event_stream_content_type("text/plain"));
    }

    #[test]
    fn auto_retry_attempts_cover_large_credential_pool() {
        let config = Config::default();

        assert_eq!(KiroProvider::test_max_retry_attempts(1, &config), 9);
        assert_eq!(KiroProvider::test_max_retry_attempts(3, &config), 9);
        assert_eq!(KiroProvider::test_max_retry_attempts(25, &config), 25);
    }

    #[test]
    fn configured_retry_attempts_override_auto_pool_size() {
        let mut config = Config::default();
        config.credential_retry_max_attempts = 12;

        assert_eq!(KiroProvider::test_max_retry_attempts(25, &config), 12);
    }
}

impl KiroProvider {
    /// 创建带代理配置和端点注册表的 KiroProvider 实例
    ///
    /// # Arguments
    /// * `token_manager` - 多凭据 Token 管理器
    /// * `proxy` - 全局代理配置
    /// * `endpoints` - 端点名 → 实现的注册表（至少包含 `default_endpoint` 对应条目）
    /// * `default_endpoint` - 凭据未显式指定 endpoint 时使用的名称
    pub fn with_proxy(
        token_manager: Arc<MultiTokenManager>,
        proxy: Option<ProxyConfig>,
        endpoints: HashMap<String, Arc<dyn KiroEndpoint>>,
        default_endpoint: String,
    ) -> Self {
        assert!(
            endpoints.contains_key(&default_endpoint),
            "默认端点 {} 未在 endpoints 注册表中",
            default_endpoint
        );
        let tls_backend = token_manager.runtime_config().tls_backend;
        // 预热：构建全局代理对应的 Client
        let initial_client =
            build_client(proxy.as_ref(), 720, tls_backend).expect("创建 HTTP 客户端失败");
        let mut cache = HashMap::new();
        cache.insert(proxy.clone(), initial_client);

        Self {
            token_manager,
            global_proxy: proxy,
            client_cache: Mutex::new(cache),
            tls_backend,
            endpoints,
            default_endpoint,
        }
    }

    /// 根据凭据的代理配置获取（或创建并缓存）对应的 reqwest::Client
    fn client_for(&self, credentials: &KiroCredentials) -> anyhow::Result<Client> {
        let effective = credentials.effective_proxy(self.global_proxy.as_ref());
        let mut cache = self.client_cache.lock();
        if let Some(client) = cache.get(&effective) {
            return Ok(client.clone());
        }
        let client = build_client(effective.as_ref(), 720, self.tls_backend)?;
        cache.insert(effective, client.clone());
        Ok(client)
    }

    /// 获取凭据的脱敏展示名称，用于请求级 usage 记录。
    pub fn credential_label(&self, id: u64) -> Option<String> {
        self.token_manager
            .snapshot()
            .entries
            .into_iter()
            .find(|entry| entry.id == id)
            .and_then(|entry| {
                entry.email.or(entry.masked_api_key).or(entry
                    .endpoint
                    .map(|endpoint| format!("#{} {}", id, endpoint)))
            })
    }

    /// 获取当前运行时配置快照。
    pub fn runtime_config(&self) -> Config {
        self.token_manager.runtime_config()
    }

    /// 获取调度器状态快照，用于错误响应携带当前冷却/容量信息。
    pub fn manager_snapshot(&self) -> ManagerSnapshot {
        self.token_manager.snapshot()
    }

    fn credential_log_label(&self, id: u64) -> String {
        Self::format_credential_log_label(id, self.credential_label(id))
    }

    fn format_credential_log_label(id: u64, label: Option<String>) -> String {
        let prefix = format!("#{}", id);
        let Some(label) = label
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        else {
            return prefix;
        };

        if label == prefix || label.starts_with(&format!("{} ", prefix)) {
            label
        } else {
            format!("{} {}", prefix, label)
        }
    }

    pub fn attempts_from_error(err: &anyhow::Error) -> Vec<KiroCredentialAttempt> {
        err.downcast_ref::<KiroCallError>()
            .map(|err| err.attempts().to_vec())
            .unwrap_or_default()
    }

    fn traced_error(
        message: impl Into<String>,
        attempts: &[KiroCredentialAttempt],
    ) -> anyhow::Error {
        KiroCallError::new(message, attempts.to_vec()).into()
    }

    fn push_attempt(
        attempts: &mut Vec<KiroCredentialAttempt>,
        attempt: usize,
        credential_id: u64,
        credential_label: &str,
        status: Option<reqwest::StatusCode>,
        action: &str,
        error_type: Option<&str>,
        error_message: Option<String>,
        started_at: Instant,
        model: Option<&str>,
    ) {
        attempts.push(
            KiroCredentialAttempt::new(
                attempt,
                credential_id,
                Some(credential_label.to_string()),
                status,
                action,
                error_type,
                error_message,
                started_at.elapsed().as_millis() as u64,
            )
            .with_model(model),
        );
    }

    fn log_attempt_chain(
        request_id: Option<&str>,
        api_type: &str,
        attempts: &[KiroCredentialAttempt],
        outcome: &str,
    ) {
        if attempts.is_empty() {
            return;
        }
        let chain = summarize_attempts(attempts);
        match request_id {
            Some(request_id) => tracing::info!(
                request_id,
                api_type,
                outcome,
                credential_chain = %chain,
                "Kiro API 凭据调用链路"
            ),
            None => tracing::info!(
                api_type,
                outcome,
                credential_chain = %chain,
                "Kiro API 凭据调用链路"
            ),
        }
    }

    /// 根据凭据选择 endpoint 实现
    fn endpoint_for(&self, credentials: &KiroCredentials) -> anyhow::Result<Arc<dyn KiroEndpoint>> {
        let name = credentials
            .endpoint
            .as_deref()
            .unwrap_or(&self.default_endpoint);
        self.endpoints
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("未知端点: {}", name))
    }

    fn maybe_exclude_after_soft_failure(
        &self,
        session_id: Option<&str>,
        model: Option<&str>,
        credential_id: u64,
        credential_label: &str,
        excluded_ids: &mut HashSet<u64>,
    ) {
        let Some(session_id) = session_id else {
            return;
        };
        if !self
            .token_manager
            .record_session_soft_failure(session_id, credential_id)
        {
            return;
        }

        if self
            .token_manager
            .has_alternate_usable_credential(model, excluded_ids, credential_id)
        {
            tracing::warn!(
                credential_id,
                credential_label = %credential_label,
                session_id,
                "会话软失败达到阈值，临时排除当前凭据并 fallback"
            );
            excluded_ids.insert(credential_id);
        } else {
            tracing::warn!(
                credential_id,
                credential_label = %credential_label,
                session_id,
                "会话软失败达到阈值，但没有其他可用凭据；保留当前凭据继续重试"
            );
        }
    }

    async fn handle_credential_auth_failure(
        &self,
        call_scope: &str,
        status: reqwest::StatusCode,
        body: &str,
        ctx: &CallContext,
        endpoint: &dyn KiroEndpoint,
        credential_label: &str,
        model: Option<&str>,
        session_id: Option<&str>,
        excluded_ids: &mut HashSet<u64>,
        force_refreshed: &mut HashSet<u64>,
    ) -> anyhow::Result<CredentialAuthFailureDecision> {
        // token 被上游失效时，先给当前凭据一次强制刷新机会；刷新成功不写入 Auth 冷却，
        // 否则下一轮调度可能跳过刚刷新成功的账号。
        if endpoint.is_bearer_token_invalid(body) && !force_refreshed.contains(&ctx.id) {
            force_refreshed.insert(ctx.id);
            tracing::info!(
                credential_id = ctx.id,
                credential_label = %credential_label,
                call_scope,
                "凭据 token 疑似被上游失效，尝试强制刷新"
            );
            if self
                .token_manager
                .force_refresh_token_for(ctx.id)
                .await
                .is_ok()
            {
                tracing::info!(
                    credential_id = ctx.id,
                    credential_label = %credential_label,
                    call_scope,
                    "凭据 token 强制刷新成功，重试请求"
                );
                return Ok(CredentialAuthFailureDecision::ForceRefreshRetry);
            }
            tracing::warn!(
                credential_id = ctx.id,
                credential_label = %credential_label,
                call_scope,
                "凭据 token 强制刷新失败，计入失败"
            );
        }

        self.token_manager.report_transient_failure_kind(
            ctx.id,
            model,
            TransientFailureKind::Auth,
            None,
            format!("auth_error {} {}", status, body),
        )?;

        let has_available = self.token_manager.report_failure(ctx.id);
        if let Some(session_id) = session_id {
            self.token_manager
                .unbind_session_if_bound_to(session_id, ctx.id);
        }

        if !has_available {
            return Ok(CredentialAuthFailureDecision::Exhausted);
        }

        let excluded_current =
            if self
                .token_manager
                .has_alternate_usable_credential(model, excluded_ids, ctx.id)
            {
                excluded_ids.insert(ctx.id);
                true
            } else {
                false
            };

        Ok(CredentialAuthFailureDecision::Retry { excluded_current })
    }

    fn finish_attempt(&self, ctx: &mut CallContext) {
        ctx.release_in_flight();
    }

    /// 发送非流式 API 请求
    ///
    /// 支持多凭据故障转移（见 [`Self::call_api_with_retry`]）
    #[allow(dead_code)]
    pub async fn call_api(&self, request_body: &str) -> anyhow::Result<reqwest::Response> {
        let response = self.call_api_with_context(request_body).await?;
        let (response, completion) = response.into_parts();
        completion.report_success();
        Ok(response)
    }

    /// 发送非流式 API 请求，并返回实际使用的凭据与 sticky 会话信息。
    pub async fn call_api_with_context(
        &self,
        request_body: &str,
    ) -> anyhow::Result<KiroApiResponse> {
        self.call_api_with_context_with_request_id(request_body, None)
            .await
    }

    pub async fn call_api_with_context_with_request_id(
        &self,
        request_body: &str,
        request_id: Option<&str>,
    ) -> anyhow::Result<KiroApiResponse> {
        self.call_api_with_context_with_request_id_and_mode(
            request_body,
            request_id,
            AcquireMode::WaitForCapacity,
        )
        .await
    }

    pub async fn call_api_with_context_with_request_id_fail_fast(
        &self,
        request_body: &str,
        request_id: Option<&str>,
    ) -> anyhow::Result<KiroApiResponse> {
        self.call_api_with_context_with_request_id_and_mode(
            request_body,
            request_id,
            AcquireMode::FailFastOnCapacity,
        )
        .await
    }

    pub async fn call_api_with_context_with_request_id_max_wait(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        max_wait: Duration,
    ) -> anyhow::Result<KiroApiResponse> {
        self.call_api_with_context_with_request_id_and_mode(
            request_body,
            request_id,
            AcquireMode::WaitForCapacityMax(max_wait),
        )
        .await
    }

    async fn call_api_with_context_with_request_id_and_mode(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        acquire_mode: AcquireMode,
    ) -> anyhow::Result<KiroApiResponse> {
        let result = self
            .call_api_with_retry(request_body, false, request_id, acquire_mode)
            .await?;
        Ok(KiroApiResponse {
            response: result.response,
            completion: KiroApiCompletion::new(
                self.token_manager.clone(),
                result.credential_id,
                result.in_flight_lease,
                result.session_id,
                result.model,
                result.sticky_bound,
                result.fallback_from_sticky,
                result.attempts,
                result.started_at,
            ),
        })
    }

    /// 使用指定凭据发送一次非流式 API 请求。
    ///
    /// Admin 测试账号连通性时使用；不参与负载均衡、不做凭据 fallback，
    /// 失败也不累计禁用计数，避免手动测试改变调度状态。
    pub async fn call_api_with_credential(
        &self,
        credential_id: u64,
        request_body: &str,
    ) -> anyhow::Result<KiroApiResponse> {
        let mut ctx = self
            .token_manager
            .acquire_context_for_credential(credential_id)
            .await?;
        ctx.mark_in_flight_kind(InFlightKind::Test);
        let credential_label = self.credential_log_label(ctx.id);
        let credential_context = format!("凭据 {}", credential_label);

        let config = self.token_manager.runtime_config();
        let machine_id = machine_id::generate_from_credentials(&ctx.credentials, &config);
        let endpoint = self.endpoint_for(&ctx.credentials).map_err(|e| {
            anyhow::anyhow!(
                "非流式 API 凭据 endpoint 解析失败（{}）: {}",
                credential_context,
                e
            )
        })?;

        let rctx = RequestContext {
            credentials: &ctx.credentials,
            token: &ctx.token,
            machine_id: &machine_id,
            config: &config,
        };

        let url = endpoint.api_url(&rctx);
        let body = crate::http_client::maybe_compress_json_whitespace(
            endpoint.transform_api_body(request_body, &rctx),
            config.compression.enabled && config.compression.whitespace_compression,
        );
        if should_log_upstream_body_size_at_info(body.len(), &config) {
            tracing::info!(
                endpoint = endpoint.name(),
                credential_id = ctx.id,
                credential_label = %credential_label,
                upstream_body_bytes = body.len(),
                pre_endpoint_body_bytes = request_body.len(),
                compression_enabled = config.compression.enabled
                    && config.compression.whitespace_compression,
                "Kiro upstream request body size"
            );
        } else {
            tracing::debug!(
                endpoint = endpoint.name(),
                credential_id = ctx.id,
                credential_label = %credential_label,
                upstream_body_bytes = body.len(),
                pre_endpoint_body_bytes = request_body.len(),
                compression_enabled = config.compression.enabled
                    && config.compression.whitespace_compression,
                "Kiro upstream request body size"
            );
        }
        let base = self
            .client_for(&ctx.credentials)
            .map_err(|e| {
                anyhow::anyhow!(
                    "非流式 API 创建 HTTP client 失败（{}）: {}",
                    credential_context,
                    e
                )
            })?
            .post(&url)
            .body(body)
            .header("content-type", "application/json")
            .header("Connection", "close");
        let request = endpoint.decorate_api(base, &rctx);

        let response = request.send().await.map_err(|e| {
            anyhow::anyhow!("非流式 API 请求发送失败（{}）: {}", credential_context, e)
        })?;
        let status = response.status();
        if status.is_success() {
            return Ok(KiroApiResponse {
                response,
                completion: KiroApiCompletion::new(
                    self.token_manager.clone(),
                    ctx.id,
                    ctx.take_in_flight_lease(),
                    None,
                    None,
                    false,
                    false,
                    Vec::new(),
                    Instant::now(),
                ),
            });
        }

        let body = response.text().await.unwrap_or_default();
        anyhow::bail!(
            "非流式 API 请求失败（{}）: {} {}",
            credential_context,
            status,
            body
        );
    }

    /// 从 Kiro 上游同步可用模型列表。
    ///
    /// 该方法只用于后台模型能力同步：失败会返回给调用方记录状态，不会写入调度失败、
    /// 不会禁用凭据，也不会占用请求并发槽。由于同步会真实调用 Kiro 上游，
    /// 这里只自动使用未禁用凭据，避免后台任务绕过用户手动禁用。
    pub async fn list_available_models(&self) -> anyhow::Result<Vec<KiroAvailableModel>> {
        let credential_ids: Vec<u64> = self
            .token_manager
            .snapshot()
            .entries
            .into_iter()
            .filter(|entry| !entry.disabled)
            .map(|entry| entry.id)
            .collect();
        let mut last_error: Option<anyhow::Error> = None;

        for id in credential_ids {
            let ctx = match self.token_manager.acquire_context_for_credential(id).await {
                Ok(ctx) => ctx,
                Err(err) => {
                    last_error = Some(anyhow::anyhow!("凭据 #{} 获取 token 失败: {}", id, err));
                    continue;
                }
            };
            match self.list_available_models_for_context(&ctx).await {
                Ok(models) if !models.is_empty() => return Ok(models),
                Ok(_) => {
                    last_error = Some(anyhow::anyhow!("凭据 #{} 返回空模型列表", id));
                }
                Err(err) => {
                    let label = self.credential_log_label(ctx.id);
                    tracing::warn!(
                        credential_id = ctx.id,
                        credential_label = %label,
                        "同步 Kiro 模型能力失败: {}",
                        err
                    );
                    last_error = Some(err);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("没有已启用且可用于同步模型能力的凭据")))
    }

    async fn list_available_models_for_context(
        &self,
        ctx: &CallContext,
    ) -> anyhow::Result<Vec<KiroAvailableModel>> {
        let config = self.token_manager.runtime_config();
        let machine_id = machine_id::generate_from_credentials(&ctx.credentials, &config);
        let endpoint = self.endpoint_for(&ctx.credentials)?;
        let client = self.client_for(&ctx.credentials)?;
        let rctx = RequestContext {
            credentials: &ctx.credentials,
            token: &ctx.token,
            machine_id: &machine_id,
            config: &config,
        };

        let mut all_models = Vec::new();
        let mut next_token: Option<String> = None;
        for _ in 0..20 {
            let url = endpoint.models_url(&rctx, next_token.as_deref());
            let request = endpoint.decorate_models(client.get(&url), &rctx);
            let response = request.send().await?;
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            if !status.is_success() {
                anyhow::bail!("ListAvailableModels 失败: {} {}", status, body);
            }
            let parsed: KiroAvailableModelsResponse = serde_json::from_str(&body)?;
            all_models.extend(
                parsed
                    .models
                    .into_iter()
                    .filter(|model| !model.model_id.trim().is_empty()),
            );
            next_token = parsed.next_token.filter(|token| !token.trim().is_empty());
            if next_token.is_none() {
                break;
            }
        }

        Ok(all_models)
    }

    /// 发送流式 API 请求
    #[allow(dead_code)]
    pub async fn call_api_stream(&self, request_body: &str) -> anyhow::Result<KiroStreamResponse> {
        self.call_api_stream_with_request_id(request_body, None)
            .await
    }

    pub async fn call_api_stream_with_request_id(
        &self,
        request_body: &str,
        request_id: Option<&str>,
    ) -> anyhow::Result<KiroStreamResponse> {
        self.call_api_stream_with_request_id_and_mode(
            request_body,
            request_id,
            AcquireMode::WaitForCapacity,
        )
        .await
    }

    pub async fn call_api_stream_with_request_id_fail_fast(
        &self,
        request_body: &str,
        request_id: Option<&str>,
    ) -> anyhow::Result<KiroStreamResponse> {
        self.call_api_stream_with_request_id_and_mode(
            request_body,
            request_id,
            AcquireMode::FailFastOnCapacity,
        )
        .await
    }

    pub async fn call_api_stream_with_request_id_max_wait(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        max_wait: Duration,
    ) -> anyhow::Result<KiroStreamResponse> {
        self.call_api_stream_with_request_id_and_mode(
            request_body,
            request_id,
            AcquireMode::WaitForCapacityMax(max_wait),
        )
        .await
    }

    async fn call_api_stream_with_request_id_and_mode(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        acquire_mode: AcquireMode,
    ) -> anyhow::Result<KiroStreamResponse> {
        let result = self
            .call_api_with_retry(request_body, true, request_id, acquire_mode)
            .await?;
        Ok(KiroStreamResponse {
            response: result.response,
            completion: KiroStreamCompletion::new(
                self.token_manager.clone(),
                result.credential_id,
                result.in_flight_lease,
                result.session_id,
                result.model,
                result.sticky_bound,
                result.fallback_from_sticky,
                result.attempts,
                result.started_at,
            ),
        })
    }

    /// 发送 MCP API 请求（WebSearch 等工具调用）
    pub async fn call_mcp(&self, request_body: &str) -> anyhow::Result<reqwest::Response> {
        self.call_mcp_with_retry(request_body).await
    }

    /// 内部方法：带重试逻辑的 MCP API 调用
    async fn call_mcp_with_retry(&self, request_body: &str) -> anyhow::Result<reqwest::Response> {
        let total_credentials = self.token_manager.total_count();
        let max_retries =
            Self::max_retry_attempts(total_credentials, &self.token_manager.runtime_config());
        let mut last_error: Option<anyhow::Error> = None;
        let mut force_refreshed: HashSet<u64> = HashSet::new();
        let mut excluded_ids: HashSet<u64> = HashSet::new();

        for attempt in 0..max_retries {
            // MCP 调用（WebSearch 等工具）不涉及模型选择，无需按模型过滤凭据
            let mut ctx = match self
                .token_manager
                .acquire_context_for_session(None, None, &excluded_ids)
                .await
            {
                Ok(c) => c,
                Err(e) => {
                    last_error = Some(e);
                    continue;
                }
            };
            ctx.mark_in_flight_kind(InFlightKind::Mcp);
            let credential_label = self.credential_log_label(ctx.id);
            let credential_context = format!("凭据 {}", credential_label);
            let attempt_started_at = Instant::now();

            let config = self.token_manager.runtime_config();
            let machine_id = machine_id::generate_from_credentials(&ctx.credentials, &config);

            let endpoint = match self.endpoint_for(&ctx.credentials) {
                Ok(e) => e,
                Err(e) => {
                    last_error = Some(anyhow::anyhow!(
                        "MCP 凭据 endpoint 解析失败（{}）: {}",
                        credential_context,
                        e
                    ));
                    // endpoint 解析失败：记为失败，换下一张凭据
                    tracing::warn!(
                        credential_id = ctx.id,
                        credential_label = %credential_label,
                        "MCP 凭据 endpoint 解析失败（{}），计入失败: {}",
                        credential_context,
                        e
                    );
                    self.token_manager.report_failure(ctx.id);
                    self.finish_attempt(&mut ctx);
                    continue;
                }
            };

            let rctx = RequestContext {
                credentials: &ctx.credentials,
                token: &ctx.token,
                machine_id: &machine_id,
                config: &config,
            };

            let url = endpoint.mcp_url(&rctx);
            let body = crate::http_client::maybe_compress_json_whitespace(
                endpoint.transform_mcp_body(request_body, &rctx),
                config.compression.enabled && config.compression.whitespace_compression,
            );

            let client = match self.client_for(&ctx.credentials) {
                Ok(client) => client,
                Err(e) => {
                    self.finish_attempt(&mut ctx);
                    anyhow::bail!("MCP 创建 HTTP client 失败（{}）: {}", credential_context, e);
                }
            };
            let base = client
                .post(&url)
                .body(body)
                .header("content-type", "application/json")
                .header("Connection", "close");
            let request = endpoint.decorate_mcp(base, &rctx);

            let response = match request.send().await {
                Ok(resp) => resp,
                Err(e) => {
                    tracing::warn!(
                        credential_id = ctx.id,
                        credential_label = %credential_label,
                        "MCP 请求发送失败（{}，尝试 {}/{}）: {}",
                        credential_context,
                        attempt + 1,
                        max_retries,
                        e
                    );
                    last_error = Some(anyhow::anyhow!(
                        "MCP 请求发送失败（{}）: {}",
                        credential_context,
                        e
                    ));
                    if let Err(err) = self.token_manager.report_transient_failure_kind(
                        ctx.id,
                        None,
                        TransientFailureKind::Network,
                        None,
                        format!("send_error {}", e),
                    ) {
                        self.finish_attempt(&mut ctx);
                        anyhow::bail!(
                            "MCP 请求发送失败（{}，调度状态写入失败）: {}",
                            credential_context,
                            err
                        );
                    }
                    self.finish_attempt(&mut ctx);
                    if attempt + 1 < max_retries {
                        sleep(Self::retry_delay(attempt)).await;
                    }
                    continue;
                }
            };

            let status = response.status();
            let retry_after = Self::retry_after_duration(response.headers());

            // 成功响应
            if status.is_success() {
                self.token_manager.report_success_with_latency(
                    ctx.id,
                    None,
                    Some(attempt_started_at.elapsed()),
                );
                self.finish_attempt(&mut ctx);
                return Ok(response);
            }

            // 失败响应
            let body = response.text().await.unwrap_or_default();

            if let Some(risk_reason) = Self::detect_risk_control_error(status, &body) {
                tracing::error!(
                    credential_id = ctx.id,
                    credential_label = %credential_label,
                    risk_reason = ?risk_reason,
                    "MCP 请求失败（{}，命中上游风控/封禁状态，禁用凭据并切换，尝试 {}/{}）: {} {}",
                    credential_context,
                    attempt + 1,
                    max_retries,
                    status,
                    body
                );
                let has_available = self.token_manager.report_risk_controlled(
                    ctx.id,
                    risk_reason,
                    format!("MCP {} {}", status, body),
                );
                if !has_available {
                    self.finish_attempt(&mut ctx);
                    anyhow::bail!(
                        "MCP 请求失败（{}，所有凭据已用尽）: {} {}",
                        credential_context,
                        status,
                        body
                    );
                }
                last_error = Some(anyhow::anyhow!(
                    "MCP 请求失败（{}）: {} {}",
                    credential_context,
                    status,
                    body
                ));
                self.finish_attempt(&mut ctx);
                continue;
            }

            // 402 额度用尽
            if status.as_u16() == 402 && endpoint.is_monthly_request_limit(&body) {
                tracing::warn!(
                    credential_id = ctx.id,
                    credential_label = %credential_label,
                    "MCP 请求失败（{}，额度已用尽，禁用凭据并切换，尝试 {}/{}）: {} {}",
                    credential_context,
                    attempt + 1,
                    max_retries,
                    status,
                    body
                );
                let has_available = self.token_manager.report_quota_exhausted(ctx.id);
                if !has_available {
                    self.finish_attempt(&mut ctx);
                    anyhow::bail!(
                        "MCP 请求失败（{}，所有凭据已用尽）: {} {}",
                        credential_context,
                        status,
                        body
                    );
                }
                last_error = Some(anyhow::anyhow!(
                    "MCP 请求失败（{}）: {} {}",
                    credential_context,
                    status,
                    body
                ));
                self.finish_attempt(&mut ctx);
                continue;
            }

            if status.as_u16() == 402 {
                last_error = Some(anyhow::anyhow!(
                    "MCP 请求失败（{}，支付状态待确认）: {} {}",
                    credential_context,
                    status,
                    body
                ));
                if let Err(err) = self.token_manager.report_transient_failure_kind(
                    ctx.id,
                    None,
                    TransientFailureKind::RateLimit,
                    retry_after,
                    format!("payment_required {} {}", status, body),
                ) {
                    self.finish_attempt(&mut ctx);
                    anyhow::bail!(
                        "MCP 请求失败（{}，调度状态写入失败）: {}",
                        credential_context,
                        err
                    );
                }
                self.finish_attempt(&mut ctx);
                continue;
            }

            // 400 Bad Request
            if status.as_u16() == 400 {
                self.finish_attempt(&mut ctx);
                anyhow::bail!(
                    "MCP 请求失败（{}）: {} {}",
                    credential_context,
                    status,
                    body
                );
            }

            // 401/403 凭据问题
            if matches!(status.as_u16(), 401 | 403) {
                tracing::warn!(
                    credential_id = ctx.id,
                    credential_label = %credential_label,
                    "MCP 请求失败（{}，可能为凭据错误，尝试 {}/{}）: {} {}",
                    credential_context,
                    attempt + 1,
                    max_retries,
                    status,
                    body
                );
                let decision = match self
                    .handle_credential_auth_failure(
                        "MCP",
                        status,
                        &body,
                        &ctx,
                        endpoint.as_ref(),
                        &credential_label,
                        None,
                        None,
                        &mut excluded_ids,
                        &mut force_refreshed,
                    )
                    .await
                {
                    Ok(decision) => decision,
                    Err(err) => {
                        self.finish_attempt(&mut ctx);
                        anyhow::bail!(
                            "MCP 请求失败（{}，调度状态写入失败）: {}",
                            credential_context,
                            err
                        );
                    }
                };
                last_error = Some(anyhow::anyhow!(
                    "MCP 请求失败（{}）: {} {}",
                    credential_context,
                    status,
                    body
                ));
                self.finish_attempt(&mut ctx);
                if matches!(decision, CredentialAuthFailureDecision::Exhausted) {
                    anyhow::bail!(
                        "MCP 请求失败（{}，所有凭据已用尽）: {} {}",
                        credential_context,
                        status,
                        body
                    );
                }
                continue;
            }

            // 瞬态错误
            if matches!(status.as_u16(), 408 | 429) || status.is_server_error() {
                tracing::warn!(
                    credential_id = ctx.id,
                    credential_label = %credential_label,
                    "MCP 请求失败（{}，上游瞬态错误，尝试 {}/{}）: {} {}",
                    credential_context,
                    attempt + 1,
                    max_retries,
                    status,
                    body
                );
                last_error = Some(anyhow::anyhow!(
                    "MCP 请求失败（{}）: {} {}",
                    credential_context,
                    status,
                    body
                ));
                let kind = if status.as_u16() == 429 {
                    TransientFailureKind::RateLimit
                } else {
                    TransientFailureKind::Server
                };
                if let Err(err) = self.token_manager.report_transient_failure_kind(
                    ctx.id,
                    None,
                    kind,
                    retry_after,
                    format!("{} {}", status, body),
                ) {
                    self.finish_attempt(&mut ctx);
                    anyhow::bail!(
                        "MCP 请求失败（{}，调度状态写入失败）: {}",
                        credential_context,
                        err
                    );
                }
                self.finish_attempt(&mut ctx);
                if attempt + 1 < max_retries {
                    sleep(Self::retry_delay(attempt)).await;
                }
                continue;
            }

            // 其他 4xx
            if status.is_client_error() {
                self.finish_attempt(&mut ctx);
                anyhow::bail!(
                    "MCP 请求失败（{}）: {} {}",
                    credential_context,
                    status,
                    body
                );
            }

            // 兜底
            tracing::warn!(
                credential_id = ctx.id,
                credential_label = %credential_label,
                "MCP 请求失败（{}，未知错误，尝试 {}/{}）: {} {}",
                credential_context,
                attempt + 1,
                max_retries,
                status,
                body
            );
            last_error = Some(anyhow::anyhow!(
                "MCP 请求失败（{}）: {} {}",
                credential_context,
                status,
                body
            ));
            if let Err(err) = self.token_manager.report_transient_failure_kind(
                ctx.id,
                None,
                TransientFailureKind::Protocol,
                retry_after,
                format!("unknown_error {} {}", status, body),
            ) {
                self.finish_attempt(&mut ctx);
                anyhow::bail!(
                    "MCP 请求失败（{}，调度状态写入失败）: {}",
                    credential_context,
                    err
                );
            }
            self.finish_attempt(&mut ctx);
            if attempt + 1 < max_retries {
                sleep(Self::retry_delay(attempt)).await;
            }
        }

        Err(last_error.unwrap_or_else(|| {
            anyhow::anyhow!("MCP 请求失败：已达到最大重试次数（{}次）", max_retries)
        }))
    }

    /// 内部方法：带重试逻辑的 API 调用
    ///
    /// 重试策略：
    /// - `credentialRetryMaxAttempts > 0` 时使用显式上限
    /// - 默认 `0` 自动按凭据池规模放大，小账号池保持最多 9 次，大账号池至少覆盖一轮凭据
    /// - 每个凭据触发瞬态错误后会进入临时冷却，后续调度优先换其他凭据
    async fn call_api_with_retry(
        &self,
        request_body: &str,
        is_stream: bool,
        request_id: Option<&str>,
        acquire_mode: AcquireMode,
    ) -> anyhow::Result<ApiCallResponse> {
        let total_credentials = self.token_manager.total_count();
        let max_retries =
            Self::max_retry_attempts(total_credentials, &self.token_manager.runtime_config());
        let mut last_error: Option<anyhow::Error> = None;
        let mut force_refreshed: HashSet<u64> = HashSet::new();
        let api_type = if is_stream { "流式" } else { "非流式" };
        let mut attempts: Vec<KiroCredentialAttempt> = Vec::new();

        // 尝试从请求体中提取模型信息
        let model = Self::extract_model_from_request(request_body);
        let conversation_id = Self::extract_conversation_id_from_request(request_body);
        let mut excluded_ids: HashSet<u64> = HashSet::new();

        for attempt in 0..max_retries {
            // 获取调用上下文（绑定 index、credentials、token）
            let mut ctx = match self
                .token_manager
                .acquire_context_for_session_with_mode(
                    model.as_deref(),
                    conversation_id.as_deref(),
                    &excluded_ids,
                    acquire_mode,
                )
                .await
            {
                Ok(c) => c,
                Err(e) => {
                    if last_error.is_none() {
                        last_error = Some(e);
                    } else {
                        tracing::warn!(
                            error = %e,
                            "获取凭据失败，但保留之前的上游错误"
                        );
                    }
                    break;
                }
            };
            ctx.mark_in_flight_kind(if is_stream {
                InFlightKind::Stream
            } else {
                InFlightKind::Api
            });
            let credential_label = self.credential_log_label(ctx.id);
            let credential_context = format!("凭据 {}", credential_label);
            let attempt_started_at = Instant::now();

            let config = self.token_manager.runtime_config();
            let machine_id = machine_id::generate_from_credentials(&ctx.credentials, &config);

            let endpoint = match self.endpoint_for(&ctx.credentials) {
                Ok(e) => e,
                Err(e) => {
                    let message = format!(
                        "{} API 凭据 endpoint 解析失败（{}）: {}",
                        api_type, credential_context, e
                    );
                    Self::push_attempt(
                        &mut attempts,
                        attempt,
                        ctx.id,
                        &credential_label,
                        None,
                        "retry",
                        Some("endpoint_error"),
                        Some(message.clone()),
                        attempt_started_at,
                        model.as_deref(),
                    );
                    last_error = Some(anyhow::anyhow!(message));
                    if let Some(session_id) = conversation_id.as_deref() {
                        self.token_manager
                            .unbind_session_if_bound_to(session_id, ctx.id);
                    }
                    tracing::warn!(
                        credential_id = ctx.id,
                        credential_label = %credential_label,
                        "凭据 endpoint 解析失败（{}），计入失败: {}",
                        credential_context,
                        e
                    );
                    self.token_manager.report_failure(ctx.id);
                    self.finish_attempt(&mut ctx);
                    continue;
                }
            };

            let rctx = RequestContext {
                credentials: &ctx.credentials,
                token: &ctx.token,
                machine_id: &machine_id,
                config: &config,
            };

            let url = endpoint.api_url(&rctx);
            let body = crate::http_client::maybe_compress_json_whitespace(
                endpoint.transform_api_body(request_body, &rctx),
                config.compression.enabled && config.compression.whitespace_compression,
            );
            if should_log_upstream_body_size_at_info(body.len(), &config) {
                tracing::info!(
                    request_id,
                    api_type,
                    endpoint = endpoint.name(),
                    credential_id = ctx.id,
                    credential_label = %credential_label,
                    attempt = attempt + 1,
                    max_retries,
                    model = model.as_deref(),
                    conversation_id = conversation_id.as_deref(),
                    upstream_body_bytes = body.len(),
                    pre_endpoint_body_bytes = request_body.len(),
                    compression_enabled = config.compression.enabled
                        && config.compression.whitespace_compression,
                    "Kiro upstream request body size"
                );
            } else {
                tracing::debug!(
                    request_id,
                    api_type,
                    endpoint = endpoint.name(),
                    credential_id = ctx.id,
                    credential_label = %credential_label,
                    attempt = attempt + 1,
                    max_retries,
                    model = model.as_deref(),
                    conversation_id = conversation_id.as_deref(),
                    upstream_body_bytes = body.len(),
                    pre_endpoint_body_bytes = request_body.len(),
                    compression_enabled = config.compression.enabled
                        && config.compression.whitespace_compression,
                    "Kiro upstream request body size"
                );
            }

            let client = match self.client_for(&ctx.credentials) {
                Ok(client) => client,
                Err(e) => {
                    let message = format!(
                        "{} API 创建 HTTP client 失败（{}）: {}",
                        api_type, credential_context, e
                    );
                    Self::push_attempt(
                        &mut attempts,
                        attempt,
                        ctx.id,
                        &credential_label,
                        None,
                        "fail",
                        Some("client_error"),
                        Some(message.clone()),
                        attempt_started_at,
                        model.as_deref(),
                    );
                    Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                    self.finish_attempt(&mut ctx);
                    return Err(Self::traced_error(message, &attempts));
                }
            };
            let base = client
                .post(&url)
                .body(body)
                .header("content-type", "application/json")
                .header("Connection", "close");
            let request = endpoint.decorate_api(base, &rctx);

            let response = match request.send().await {
                Ok(resp) => resp,
                Err(e) => {
                    let message = format!(
                        "{} API 请求发送失败（{}）: {}",
                        api_type, credential_context, e
                    );
                    tracing::warn!(
                        credential_id = ctx.id,
                        credential_label = %credential_label,
                        "API 请求发送失败（{}，尝试 {}/{}）: {}",
                        credential_context,
                        attempt + 1,
                        max_retries,
                        e
                    );
                    // 网络错误通常是上游/链路瞬态问题，不应导致"禁用凭据"或"切换凭据"
                    // （否则一段时间网络抖动会把所有凭据都误禁用，需要重启才能恢复）
                    Self::push_attempt(
                        &mut attempts,
                        attempt,
                        ctx.id,
                        &credential_label,
                        None,
                        "retry",
                        Some("send_error"),
                        Some(message.clone()),
                        attempt_started_at,
                        model.as_deref(),
                    );
                    last_error = Some(anyhow::anyhow!(message));
                    if let Err(err) = self.token_manager.report_transient_failure_kind(
                        ctx.id,
                        model.as_deref(),
                        TransientFailureKind::Network,
                        None,
                        format!("send_error {}", e),
                    ) {
                        let final_message = format!(
                            "{} API 请求发送失败（{}，调度状态写入失败）: {}",
                            api_type, credential_context, err
                        );
                        if let Some(last) = attempts.last_mut() {
                            last.action = "fail".to_string();
                            last.error_type = Some("scheduler_state_error".to_string());
                            last.error_message = Some(final_message.clone());
                        }
                        Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                        self.finish_attempt(&mut ctx);
                        return Err(Self::traced_error(final_message, &attempts));
                    }
                    self.maybe_exclude_after_soft_failure(
                        conversation_id.as_deref(),
                        model.as_deref(),
                        ctx.id,
                        &credential_label,
                        &mut excluded_ids,
                    );
                    self.finish_attempt(&mut ctx);
                    if attempt + 1 < max_retries {
                        sleep(Self::retry_delay(attempt)).await;
                    }
                    continue;
                }
            };

            let status = response.status();
            let retry_after = Self::retry_after_duration(response.headers());

            // 成功响应
            if status.is_success() {
                if is_stream && !Self::is_event_stream_response(&response) {
                    let content_type = response
                        .headers()
                        .get(CONTENT_TYPE)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_string();
                    let body = response.text().await.unwrap_or_default();
                    let exception = Self::extract_aws_exception(&body);

                    tracing::warn!(
                        credential_id = ctx.id,
                        credential_label = %credential_label,
                        "流式 API 返回 2xx 但不是 eventstream（{}，尝试 {}/{}）: content-type={}, exception={:?}, body={}",
                        credential_context,
                        attempt + 1,
                        max_retries,
                        content_type,
                        exception,
                        body
                    );

                    let message = format!(
                        "{} API 返回非 eventstream 响应（{}）: content-type={}, exception={:?}, body={}",
                        api_type, credential_context, content_type, exception, body
                    );

                    if attempt + 1 < max_retries
                        && exception
                            .as_deref()
                            .is_some_and(Self::is_retryable_aws_exception)
                    {
                        Self::push_attempt(
                            &mut attempts,
                            attempt,
                            ctx.id,
                            &credential_label,
                            Some(status),
                            "retry",
                            Some("non_eventstream"),
                            Some(message.clone()),
                            attempt_started_at,
                            model.as_deref(),
                        );
                        if let Err(err) = self.token_manager.report_transient_failure_kind(
                            ctx.id,
                            model.as_deref(),
                            TransientFailureKind::Protocol,
                            retry_after,
                            format!("non_eventstream {} {}", status, body),
                        ) {
                            let final_message = format!(
                                "{} API 返回非 eventstream 响应（{}，调度状态写入失败）: {}",
                                api_type, credential_context, err
                            );
                            if let Some(last) = attempts.last_mut() {
                                last.action = "fail".to_string();
                                last.error_type = Some("scheduler_state_error".to_string());
                                last.error_message = Some(final_message.clone());
                            }
                            Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                            self.finish_attempt(&mut ctx);
                            return Err(Self::traced_error(final_message, &attempts));
                        }
                        self.maybe_exclude_after_soft_failure(
                            conversation_id.as_deref(),
                            model.as_deref(),
                            ctx.id,
                            &credential_label,
                            &mut excluded_ids,
                        );
                        last_error = Some(anyhow::anyhow!(message));
                        self.finish_attempt(&mut ctx);
                        sleep(Self::retry_delay(attempt)).await;
                        continue;
                    }

                    Self::push_attempt(
                        &mut attempts,
                        attempt,
                        ctx.id,
                        &credential_label,
                        Some(status),
                        "fail",
                        Some("non_eventstream"),
                        Some(message.clone()),
                        attempt_started_at,
                        model.as_deref(),
                    );
                    Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                    self.finish_attempt(&mut ctx);
                    return Err(Self::traced_error(message, &attempts));
                }
                return Ok(ApiCallResponse {
                    response,
                    credential_id: ctx.id,
                    in_flight_lease: ctx.take_in_flight_lease(),
                    session_id: conversation_id.clone(),
                    model: model.clone(),
                    sticky_bound: ctx.sticky_bound,
                    fallback_from_sticky: ctx.fallback_from_sticky,
                    attempts: {
                        Self::push_attempt(
                            &mut attempts,
                            attempt,
                            ctx.id,
                            &credential_label,
                            Some(status),
                            "success",
                            None::<&str>,
                            None::<String>,
                            attempt_started_at,
                            model.as_deref(),
                        );
                        Self::log_attempt_chain(request_id, api_type, &attempts, "success");
                        attempts
                    },
                    started_at: attempt_started_at,
                });
            }

            // 失败响应：读取 body 用于日志/错误信息
            let body = response.text().await.unwrap_or_default();

            if let Some(risk_reason) = Self::detect_risk_control_error(status, &body) {
                let message = format!(
                    "{} API 请求失败（{}）: {} {}",
                    api_type, credential_context, status, body
                );
                tracing::error!(
                    credential_id = ctx.id,
                    credential_label = %credential_label,
                    risk_reason = ?risk_reason,
                    "API 请求失败（{}，命中上游风控/封禁状态，禁用凭据并切换，尝试 {}/{}）: {} {}",
                    credential_context,
                    attempt + 1,
                    max_retries,
                    status,
                    body
                );
                let has_available = self.token_manager.report_risk_controlled(
                    ctx.id,
                    risk_reason,
                    format!("{} API {} {}", api_type, status, body),
                );
                if let Some(session_id) = conversation_id.as_deref() {
                    self.token_manager
                        .unbind_session_if_bound_to(session_id, ctx.id);
                }
                if !has_available {
                    let final_message = format!(
                        "{} API 请求失败（{}，所有凭据已用尽）: {} {}",
                        api_type, credential_context, status, body
                    );
                    Self::push_attempt(
                        &mut attempts,
                        attempt,
                        ctx.id,
                        &credential_label,
                        Some(status),
                        "fail",
                        Some("risk_control"),
                        Some(final_message.clone()),
                        attempt_started_at,
                        model.as_deref(),
                    );
                    Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                    self.finish_attempt(&mut ctx);
                    return Err(Self::traced_error(final_message, &attempts));
                }
                Self::push_attempt(
                    &mut attempts,
                    attempt,
                    ctx.id,
                    &credential_label,
                    Some(status),
                    "disable_and_retry",
                    Some("risk_control"),
                    Some(message.clone()),
                    attempt_started_at,
                    model.as_deref(),
                );
                last_error = Some(anyhow::anyhow!(message));
                self.finish_attempt(&mut ctx);
                continue;
            }

            // 402 Payment Required 且额度用尽：禁用凭据并故障转移
            if status.as_u16() == 402 && endpoint.is_monthly_request_limit(&body) {
                let message = format!(
                    "{} API 请求失败（{}）: {} {}",
                    api_type, credential_context, status, body
                );
                tracing::warn!(
                    credential_id = ctx.id,
                    credential_label = %credential_label,
                    "API 请求失败（{}，额度已用尽，禁用凭据并切换，尝试 {}/{}）: {} {}",
                    credential_context,
                    attempt + 1,
                    max_retries,
                    status,
                    body
                );

                let has_available = self.token_manager.report_quota_exhausted(ctx.id);
                if let Some(session_id) = conversation_id.as_deref() {
                    self.token_manager
                        .unbind_session_if_bound_to(session_id, ctx.id);
                }
                if !has_available {
                    let final_message = format!(
                        "{} API 请求失败（{}，所有凭据已用尽）: {} {}",
                        api_type, credential_context, status, body
                    );
                    Self::push_attempt(
                        &mut attempts,
                        attempt,
                        ctx.id,
                        &credential_label,
                        Some(status),
                        "fail",
                        Some("quota_exhausted"),
                        Some(final_message.clone()),
                        attempt_started_at,
                        model.as_deref(),
                    );
                    Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                    self.finish_attempt(&mut ctx);
                    return Err(Self::traced_error(final_message, &attempts));
                }

                Self::push_attempt(
                    &mut attempts,
                    attempt,
                    ctx.id,
                    &credential_label,
                    Some(status),
                    "disable_and_retry",
                    Some("quota_exhausted"),
                    Some(message.clone()),
                    attempt_started_at,
                    model.as_deref(),
                );
                last_error = Some(anyhow::anyhow!(message));
                self.maybe_exclude_after_soft_failure(
                    conversation_id.as_deref(),
                    model.as_deref(),
                    ctx.id,
                    &credential_label,
                    &mut excluded_ids,
                );
                self.finish_attempt(&mut ctx);
                continue;
            }

            if status.as_u16() == 402 {
                let message = format!(
                    "{} API 请求失败（{}，支付状态待确认）: {} {}",
                    api_type, credential_context, status, body
                );
                Self::push_attempt(
                    &mut attempts,
                    attempt,
                    ctx.id,
                    &credential_label,
                    Some(status),
                    "transient_retry",
                    Some("payment_required"),
                    Some(message.clone()),
                    attempt_started_at,
                    model.as_deref(),
                );
                last_error = Some(anyhow::anyhow!(message));
                if let Err(err) = self.token_manager.report_transient_failure_kind(
                    ctx.id,
                    model.as_deref(),
                    TransientFailureKind::RateLimit,
                    retry_after,
                    format!("payment_required {} {}", status, body),
                ) {
                    let final_message = format!(
                        "{} API 请求失败（{}，调度状态写入失败）: {}",
                        api_type, credential_context, err
                    );
                    Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                    self.finish_attempt(&mut ctx);
                    return Err(Self::traced_error(final_message, &attempts));
                }
                self.finish_attempt(&mut ctx);
                continue;
            }

            // 400 Bad Request - 请求问题，重试/切换凭据无意义
            if status.as_u16() == 400 {
                let message = format!(
                    "{} API 请求失败（{}）: {} {}",
                    api_type, credential_context, status, body
                );
                Self::push_attempt(
                    &mut attempts,
                    attempt,
                    ctx.id,
                    &credential_label,
                    Some(status),
                    "fail",
                    Some("bad_request"),
                    Some(message.clone()),
                    attempt_started_at,
                    model.as_deref(),
                );
                Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                self.finish_attempt(&mut ctx);
                return Err(Self::traced_error(message, &attempts));
            }

            // 401/403 - 更可能是凭据/权限问题：计入失败并允许故障转移
            if matches!(status.as_u16(), 401 | 403) {
                let message = format!(
                    "{} API 请求失败（{}）: {} {}",
                    api_type, credential_context, status, body
                );
                tracing::warn!(
                    credential_id = ctx.id,
                    credential_label = %credential_label,
                    "API 请求失败（{}，可能为凭据错误，尝试 {}/{}）: {} {}",
                    credential_context,
                    attempt + 1,
                    max_retries,
                    status,
                    body
                );
                let decision = match self
                    .handle_credential_auth_failure(
                        api_type,
                        status,
                        &body,
                        &ctx,
                        endpoint.as_ref(),
                        &credential_label,
                        model.as_deref(),
                        conversation_id.as_deref(),
                        &mut excluded_ids,
                        &mut force_refreshed,
                    )
                    .await
                {
                    Ok(decision) => decision,
                    Err(err) => {
                        let final_message = format!(
                            "{} API 请求失败（{}，调度状态写入失败）: {}",
                            api_type, credential_context, err
                        );
                        Self::push_attempt(
                            &mut attempts,
                            attempt,
                            ctx.id,
                            &credential_label,
                            Some(status),
                            "fail",
                            Some("scheduler_state_error"),
                            Some(final_message.clone()),
                            attempt_started_at,
                            model.as_deref(),
                        );
                        Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                        self.finish_attempt(&mut ctx);
                        return Err(Self::traced_error(final_message, &attempts));
                    }
                };

                if matches!(decision, CredentialAuthFailureDecision::ForceRefreshRetry) {
                    Self::push_attempt(
                        &mut attempts,
                        attempt,
                        ctx.id,
                        &credential_label,
                        Some(status),
                        "force_refresh_and_retry",
                        Some("auth_error"),
                        Some(message.clone()),
                        attempt_started_at,
                        model.as_deref(),
                    );
                    self.finish_attempt(&mut ctx);
                    continue;
                }

                if matches!(decision, CredentialAuthFailureDecision::Exhausted) {
                    let final_message = format!(
                        "{} API 请求失败（{}，所有凭据已用尽）: {} {}",
                        api_type, credential_context, status, body
                    );
                    Self::push_attempt(
                        &mut attempts,
                        attempt,
                        ctx.id,
                        &credential_label,
                        Some(status),
                        "fail",
                        Some("credential_failure"),
                        Some(final_message.clone()),
                        attempt_started_at,
                        model.as_deref(),
                    );
                    Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                    self.finish_attempt(&mut ctx);
                    return Err(Self::traced_error(final_message, &attempts));
                }

                let action = match decision {
                    CredentialAuthFailureDecision::Retry {
                        excluded_current: true,
                    } => "failure_count_exclude_and_retry",
                    CredentialAuthFailureDecision::Retry {
                        excluded_current: false,
                    } => "failure_count_and_retry",
                    CredentialAuthFailureDecision::ForceRefreshRetry
                    | CredentialAuthFailureDecision::Exhausted => unreachable!(),
                };

                Self::push_attempt(
                    &mut attempts,
                    attempt,
                    ctx.id,
                    &credential_label,
                    Some(status),
                    action,
                    Some("credential_failure"),
                    Some(message.clone()),
                    attempt_started_at,
                    model.as_deref(),
                );
                last_error = Some(anyhow::anyhow!(message));
                self.finish_attempt(&mut ctx);
                continue;
            }

            // 429/408/5xx - 瞬态上游错误：重试但不禁用或切换凭据
            // （避免 429 high traffic / 502 high load 等瞬态错误把所有凭据锁死）
            if matches!(status.as_u16(), 408 | 429) || status.is_server_error() {
                let message = format!(
                    "{} API 请求失败（{}）: {} {}",
                    api_type, credential_context, status, body
                );
                tracing::warn!(
                    credential_id = ctx.id,
                    credential_label = %credential_label,
                    "API 请求失败（{}，上游瞬态错误，尝试 {}/{}）: {} {}",
                    credential_context,
                    attempt + 1,
                    max_retries,
                    status,
                    body
                );
                Self::push_attempt(
                    &mut attempts,
                    attempt,
                    ctx.id,
                    &credential_label,
                    Some(status),
                    "transient_retry",
                    Some("transient_error"),
                    Some(message.clone()),
                    attempt_started_at,
                    model.as_deref(),
                );
                last_error = Some(anyhow::anyhow!(message));
                let kind = if status.as_u16() == 429 {
                    TransientFailureKind::RateLimit
                } else {
                    TransientFailureKind::Server
                };
                if let Err(err) = self.token_manager.report_transient_failure_kind(
                    ctx.id,
                    model.as_deref(),
                    kind,
                    retry_after,
                    format!("{} {}", status, body),
                ) {
                    let final_message = format!(
                        "{} API 请求失败（{}，调度状态写入失败）: {}",
                        api_type, credential_context, err
                    );
                    if let Some(last) = attempts.last_mut() {
                        last.action = "fail".to_string();
                        last.error_type = Some("scheduler_state_error".to_string());
                        last.error_message = Some(final_message.clone());
                    }
                    Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                    self.finish_attempt(&mut ctx);
                    return Err(Self::traced_error(final_message, &attempts));
                }
                self.maybe_exclude_after_soft_failure(
                    conversation_id.as_deref(),
                    model.as_deref(),
                    ctx.id,
                    &credential_label,
                    &mut excluded_ids,
                );
                self.finish_attempt(&mut ctx);
                if attempt + 1 < max_retries {
                    sleep(Self::retry_delay(attempt)).await;
                }
                continue;
            }

            // 其他 4xx - 通常为请求/配置问题：直接返回，不计入凭据失败
            if status.is_client_error() {
                let message = format!(
                    "{} API 请求失败（{}）: {} {}",
                    api_type, credential_context, status, body
                );
                Self::push_attempt(
                    &mut attempts,
                    attempt,
                    ctx.id,
                    &credential_label,
                    Some(status),
                    "fail",
                    Some("client_error"),
                    Some(message.clone()),
                    attempt_started_at,
                    model.as_deref(),
                );
                Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                self.finish_attempt(&mut ctx);
                return Err(Self::traced_error(message, &attempts));
            }

            // 兜底：当作可重试的瞬态错误处理（不切换凭据）
            let message = format!(
                "{} API 请求失败（{}）: {} {}",
                api_type, credential_context, status, body
            );
            tracing::warn!(
                credential_id = ctx.id,
                credential_label = %credential_label,
                "API 请求失败（{}，未知错误，尝试 {}/{}）: {} {}",
                credential_context,
                attempt + 1,
                max_retries,
                status,
                body
            );
            Self::push_attempt(
                &mut attempts,
                attempt,
                ctx.id,
                &credential_label,
                Some(status),
                "retry",
                Some("unknown_error"),
                Some(message.clone()),
                attempt_started_at,
                model.as_deref(),
            );
            last_error = Some(anyhow::anyhow!(message));
            if let Err(err) = self.token_manager.report_transient_failure_kind(
                ctx.id,
                model.as_deref(),
                TransientFailureKind::Protocol,
                retry_after,
                format!("unknown_error {} {}", status, body),
            ) {
                let final_message = format!(
                    "{} API 请求失败（{}，调度状态写入失败）: {}",
                    api_type, credential_context, err
                );
                if let Some(last) = attempts.last_mut() {
                    last.action = "fail".to_string();
                    last.error_type = Some("scheduler_state_error".to_string());
                    last.error_message = Some(final_message.clone());
                }
                Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
                self.finish_attempt(&mut ctx);
                return Err(Self::traced_error(final_message, &attempts));
            }
            self.maybe_exclude_after_soft_failure(
                conversation_id.as_deref(),
                model.as_deref(),
                ctx.id,
                &credential_label,
                &mut excluded_ids,
            );
            self.finish_attempt(&mut ctx);
            if attempt + 1 < max_retries {
                sleep(Self::retry_delay(attempt)).await;
            }
        }

        // 所有重试都失败
        Self::log_attempt_chain(request_id, api_type, &attempts, "fail");
        let message = last_error.map(|err| err.to_string()).unwrap_or_else(|| {
            format!(
                "{} API 请求失败：已达到最大重试次数（{}次）",
                api_type, max_retries
            )
        });
        Err(Self::traced_error(message, &attempts))
    }

    /// 从请求体中提取模型信息
    ///
    /// 尝试解析 JSON 请求体，提取 conversationState.currentMessage.userInputMessage.modelId
    fn extract_model_from_request(request_body: &str) -> Option<String> {
        use serde_json::Value;

        let json: Value = serde_json::from_str(request_body).ok()?;

        json.get("conversationState")?
            .get("currentMessage")?
            .get("userInputMessage")?
            .get("modelId")?
            .as_str()
            .map(|s| s.to_string())
    }

    /// 从请求体中提取 Kiro conversationId，用于账号粘性调度。
    fn extract_conversation_id_from_request(request_body: &str) -> Option<String> {
        use serde_json::Value;

        let json: Value = serde_json::from_str(request_body).ok()?;

        json.get("conversationState")?
            .get("conversationId")?
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    }

    #[cfg(test)]
    pub(crate) fn test_extract_model_from_request(request_body: &str) -> Option<String> {
        Self::extract_model_from_request(request_body)
    }

    #[cfg(test)]
    pub(crate) fn test_extract_conversation_id_from_request(request_body: &str) -> Option<String> {
        Self::extract_conversation_id_from_request(request_body)
    }

    fn max_retry_attempts(total_credentials: usize, config: &Config) -> usize {
        let configured = config.credential_retry_max_attempts as usize;
        if configured > 0 {
            return configured.max(1);
        }

        total_credentials.max(1).max(MIN_AUTO_RETRY_ATTEMPTS)
    }

    #[cfg(test)]
    pub(crate) fn test_max_retry_attempts(total_credentials: usize, config: &Config) -> usize {
        Self::max_retry_attempts(total_credentials, config)
    }

    fn retry_delay(attempt: usize) -> Duration {
        // 指数退避 + 少量抖动，避免上游抖动时放大故障
        const BASE_MS: u64 = 200;
        const MAX_MS: u64 = 2_000;
        let exp = BASE_MS.saturating_mul(2u64.saturating_pow(attempt.min(6) as u32));
        let backoff = exp.min(MAX_MS);
        let jitter_max = (backoff / 4).max(1);
        let jitter = fastrand::u64(0..=jitter_max);
        Duration::from_millis(backoff.saturating_add(jitter))
    }

    fn retry_after_duration(headers: &HeaderMap) -> Option<Duration> {
        let value = headers.get("retry-after")?.to_str().ok()?.trim();
        if value.is_empty() {
            return None;
        }

        if let Ok(seconds) = value.parse::<u64>() {
            return Some(Duration::from_secs(seconds));
        }

        let retry_at = chrono::DateTime::parse_from_rfc2822(value)
            .ok()
            .map(|dt| dt.with_timezone(&Utc))?;
        let seconds = retry_at.signed_duration_since(Utc::now()).num_seconds();
        if seconds <= 0 {
            Some(Duration::from_secs(1))
        } else {
            Some(Duration::from_secs(seconds as u64))
        }
    }

    fn detect_risk_control_error(
        status: reqwest::StatusCode,
        body: &str,
    ) -> Option<CredentialRiskControlReason> {
        let lower = body.to_ascii_lowercase();

        if status.as_u16() == 423 {
            return Some(CredentialRiskControlReason::AccountLocked);
        }

        if body.contains("TEMPORARILY_SUSPENDED")
            || lower.contains("temporarily suspended")
            || lower.contains("temporary suspended")
            || lower.contains("temporarily is suspended")
            || lower.contains("is temporarily suspended")
        {
            return Some(CredentialRiskControlReason::TemporarilySuspended);
        }

        if body.contains("PERMANENTLY_SUSPENDED")
            || body.contains("ACCOUNT_SUSPENDED")
            || body.contains("AccountSuspendedException")
            || lower.contains("account suspended")
            || lower.contains("permanently suspended")
            || lower.contains("user is suspended")
            || lower.contains("user id is suspended")
        {
            return Some(CredentialRiskControlReason::AccountSuspended);
        }

        if lower.contains("account locked")
            || lower.contains("user locked")
            || lower.contains("locked account")
            || lower.contains("locked your account")
        {
            return Some(CredentialRiskControlReason::AccountLocked);
        }

        let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
            return None;
        };
        for pointer in [
            "/reason",
            "/error/reason",
            "/__type",
            "/code",
            "/error/code",
            "/exceptionType",
            "/error/exceptionType",
        ] {
            let Some(text) = value.pointer(pointer).and_then(|v| v.as_str()) else {
                continue;
            };
            match text {
                "TEMPORARILY_SUSPENDED" => {
                    return Some(CredentialRiskControlReason::TemporarilySuspended);
                }
                "ACCOUNT_SUSPENDED" | "PERMANENTLY_SUSPENDED" | "AccountSuspendedException" => {
                    return Some(CredentialRiskControlReason::AccountSuspended);
                }
                "ACCOUNT_LOCKED" | "LOCKED" | "AccountLockedException" => {
                    return Some(CredentialRiskControlReason::AccountLocked);
                }
                _ => {}
            }
        }

        None
    }

    fn is_event_stream_response(response: &reqwest::Response) -> bool {
        response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(Self::is_event_stream_content_type)
    }

    fn is_event_stream_content_type(content_type: &str) -> bool {
        content_type
            .split(';')
            .next()
            .map(|media_type| {
                let media_type = media_type.trim().to_ascii_lowercase();
                media_type == "application/vnd.amazon.eventstream"
                    || media_type == "application/octet-stream"
                    // Kiro occasionally labels AWS event-stream framed bodies as JSON.
                    // The downstream decoder still validates the actual frame format.
                    || media_type == "application/json"
            })
            .unwrap_or(false)
    }

    fn extract_aws_exception(body: &str) -> Option<String> {
        let value: serde_json::Value = serde_json::from_str(body).ok()?;
        value
            .get("__type")
            .or_else(|| value.get("code"))
            .or_else(|| value.get("Code"))
            .or_else(|| value.get("type"))
            .or_else(|| value.pointer("/error/type"))
            .or_else(|| value.pointer("/error/code"))
            .and_then(|v| v.as_str())
            .map(|s| s.rsplit(['#', '.']).next().unwrap_or(s).to_string())
    }

    fn is_retryable_aws_exception(exception: &str) -> bool {
        matches!(
            exception,
            "ThrottlingException"
                | "TooManyRequestsException"
                | "InternalServerException"
                | "ServiceUnavailableException"
                | "RequestTimeoutException"
        )
    }
}
