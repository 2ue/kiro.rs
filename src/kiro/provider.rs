//! Kiro API Provider
//!
//! 核心组件，负责与 Kiro API 通信
//! 支持流式和非流式请求
//! 支持多凭据故障转移和重试
//! 支持按凭据级 endpoint 切换不同 Kiro API 端点

use chrono::Utc;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use reqwest::{Client, Method};
use std::collections::{HashMap, HashSet};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};
use tokio::time::sleep;

use crate::http_client::{
    ProxyConfig, build_client, response_text_with_body_timeout, send_with_response_header_timeout,
};
use crate::kiro::call_trace::{
    KiroCallError, KiroCredentialAttempt, SelectionFailureSummary, summarize_attempts,
};
use crate::kiro::endpoint::{KiroEndpoint, RequestContext};
use crate::kiro::machine_id;
use crate::kiro::model::available_models::{KiroAvailableModel, KiroAvailableModelsResponse};
use crate::kiro::model::credentials::KiroCredentials;
use crate::kiro::protocol::{
    extract_first_profile_arn, is_external_idp_credentials, is_real_profile_arn,
};
use crate::kiro::token_manager::{
    AcquireMode, CallContext, CredentialRiskControlReason, EXTERNAL_CREDENTIAL_CONTEXT_ID,
    InFlightKind, InFlightLeaseGuard, LocalPoolRouteState, MultiTokenManager, TransientFailureKind,
};
use crate::model::config::{Config, TlsBackend};
use parking_lot::Mutex;

/// 自动模式下的小账号池最少尝试次数，保持既有 1-3 个账号时最多 9 次的行为。
const MIN_AUTO_RETRY_ATTEMPTS: usize = 9;

/// Kiro provider 不设置 reqwest 整请求总超时：流式正文由 Anthropic SSE idle timeout 管控，
/// 请求头和非流式 body 分别由专门的 timeout helper 管控。
const KIRO_CLIENT_TOTAL_TIMEOUT_SECS: u64 = 0;

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
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Instant;

    use chrono::{Duration, Utc};

    use super::{CredentialRiskControlReason, KiroProvider, KiroStreamCompletion};
    use crate::kiro::call_trace::{AccountRejectReason, SelectionFailureStage};
    use crate::kiro::endpoint::{IdeEndpoint, KiroEndpoint};
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
                reqwest::StatusCode::TOO_MANY_REQUESTS,
                r#"{"message":"Due to suspicious activity, we are imposing temporary limits on your account."}"#
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
    fn downgrades_only_429_temporary_risk_when_credential_opted_out() {
        let opted_out = KiroCredentials {
            rate_limit_auto_disable_enabled: Some(false),
            ..Default::default()
        };
        let default_credential = KiroCredentials::default();

        assert!(KiroProvider::should_downgrade_rate_limit_risk_to_cooldown(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            CredentialRiskControlReason::TemporarilySuspended,
            &opted_out
        ));
        assert!(!KiroProvider::should_downgrade_rate_limit_risk_to_cooldown(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            CredentialRiskControlReason::TemporarilySuspended,
            &default_credential
        ));
        assert!(!KiroProvider::should_downgrade_rate_limit_risk_to_cooldown(
            reqwest::StatusCode::FORBIDDEN,
            CredentialRiskControlReason::TemporarilySuspended,
            &opted_out
        ));
        assert!(!KiroProvider::should_downgrade_rate_limit_risk_to_cooldown(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            CredentialRiskControlReason::AccountSuspended,
            &opted_out
        ));
        assert!(!KiroProvider::should_downgrade_rate_limit_risk_to_cooldown(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            CredentialRiskControlReason::AccountLocked,
            &opted_out
        ));
    }

    #[test]
    fn classifies_bad_request_protocol_reasons() {
        assert_eq!(
            KiroProvider::classify_bad_request_reason(
                r#"{"message":"assistant-prefill final message is not supported; last message must be user"}"#
            ),
            "assistant_prefill_bad_request"
        );
        assert_eq!(
            KiroProvider::classify_bad_request_reason(
                r#"{"message":"profileArn is required for this request"}"#
            ),
            "profile_arn_bad_request"
        );
        assert_eq!(
            KiroProvider::classify_bad_request_reason(
                r#"{"message":"The request body is improperly formed"}"#
            ),
            "malformed_request"
        );
        assert_eq!(
            KiroProvider::classify_bad_request_reason(
                r#"{"message":"Invalid tool use format.","reason":"REQUEST_BODY_INVALID"}"#
            ),
            "tool_use_format_bad_request"
        );
        assert_eq!(
            KiroProvider::classify_bad_request_reason(r#"{"message":"unknown model"}"#),
            "model_invalid_bad_request"
        );
        assert_eq!(
            KiroProvider::classify_bad_request_reason(
                r#"{"message":"The requested model is not available for this endpoint. If this continues, contact the administrator with error ID: req_01example"}"#
            ),
            "model_unavailable_bad_request"
        );
        assert_eq!(
            KiroProvider::classify_bad_request_reason(
                r#"{"message":"Invalid model ID. Please select a different model to continue.","reason":"INVALID_MODEL_ID"}"#
            ),
            "model_invalid_bad_request"
        );
        assert_eq!(
            KiroProvider::classify_bad_request_reason(
                r#"{"message":"Image data cannot be empty.","reason":"REQUEST_BODY_INVALID"}"#
            ),
            "tool_use_format_bad_request"
        );
    }

    #[test]
    fn model_unavailable_retry_requires_reason_and_model() {
        assert!(KiroProvider::should_retry_model_unavailable_bad_request(
            "model_unavailable_bad_request",
            Some("claude-opus-4-8"),
        ));
        assert!(!KiroProvider::should_retry_model_unavailable_bad_request(
            "bad_request",
            Some("claude-opus-4-8"),
        ));
        assert!(!KiroProvider::should_retry_model_unavailable_bad_request(
            "model_unavailable_bad_request",
            None,
        ));
        assert!(!KiroProvider::should_retry_model_unavailable_bad_request(
            "model_unavailable_bad_request",
            Some("  "),
        ));
    }

    #[test]
    fn prompt_logic_retry_only_applies_to_enabled_protocol_reasons() {
        let mut config = Config::default();
        config.credential_prompt_logic_retry_enabled = false;
        config.credential_prompt_logic_retry_max_attempts = 2;
        assert!(!KiroProvider::should_retry_prompt_logic_bad_request(
            "tool_use_format_bad_request",
            Some("claude-sonnet-4"),
            &config,
            0,
        ));

        config.credential_prompt_logic_retry_enabled = true;
        assert!(KiroProvider::should_retry_prompt_logic_bad_request(
            "tool_use_format_bad_request",
            Some("claude-sonnet-4"),
            &config,
            0,
        ));
        assert!(KiroProvider::should_retry_prompt_logic_bad_request(
            "assistant_prefill_bad_request",
            Some("claude-sonnet-4"),
            &config,
            1,
        ));
        assert!(!KiroProvider::should_retry_prompt_logic_bad_request(
            "malformed_request",
            Some("claude-sonnet-4"),
            &config,
            0,
        ));
        assert!(!KiroProvider::should_retry_prompt_logic_bad_request(
            "tool_use_format_bad_request",
            None,
            &config,
            0,
        ));
        assert!(!KiroProvider::should_retry_prompt_logic_bad_request(
            "tool_use_format_bad_request",
            Some("claude-sonnet-4"),
            &config,
            2,
        ));
    }

    #[test]
    fn list_available_profiles_headers_attach_external_idp_token_type() {
        let credentials = KiroCredentials {
            auth_method: Some("external_idp".to_string()),
            ..Default::default()
        };
        let headers = KiroProvider::list_available_profiles_headers(
            &credentials,
            "token",
            &Config::default(),
            "machine",
            "codewhisperer.us-east-1.amazonaws.com",
        )
        .unwrap();

        assert_eq!(
            headers.get("TokenType").and_then(|v| v.to_str().ok()),
            Some("EXTERNAL_IDP")
        );
        assert_eq!(
            headers.get("Authorization").and_then(|v| v.to_str().ok()),
            Some("Bearer token")
        );
        assert_eq!(
            headers.get("host").and_then(|v| v.to_str().ok()),
            Some("codewhisperer.us-east-1.amazonaws.com")
        );
        let expected_x_amz_user_agent = format!(
            "aws-sdk-js/1.0.34 KiroIDE-{}-machine",
            Config::default().kiro_version
        );
        assert_eq!(
            headers
                .get("x-amz-user-agent")
                .and_then(|v| v.to_str().ok()),
            Some(expected_x_amz_user_agent.as_str())
        );
    }

    #[test]
    fn list_available_profiles_headers_do_not_attach_token_type_for_social() {
        let credentials = KiroCredentials {
            auth_method: Some("social".to_string()),
            ..Default::default()
        };
        let headers = KiroProvider::list_available_profiles_headers(
            &credentials,
            "token",
            &Config::default(),
            "machine",
            "codewhisperer.us-east-1.amazonaws.com",
        )
        .unwrap();

        assert!(headers.get("TokenType").is_none());
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

    #[tokio::test]
    async fn mcp_local_acquire_failure_stops_retry_loop() {
        let mut config = Config::default();
        config.credential_retry_max_attempts = 100_000;

        let mut disabled = KiroCredentials::default();
        disabled.disabled = true;
        let manager =
            Arc::new(MultiTokenManager::new(config, vec![disabled], None, None, false).unwrap());
        let mut endpoints: HashMap<String, Arc<dyn KiroEndpoint>> = HashMap::new();
        endpoints.insert("ide".to_string(), Arc::new(IdeEndpoint));
        let provider = KiroProvider::with_proxy(manager, None, endpoints, "ide".to_string());

        let err = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            provider.call_mcp("{}"),
        )
        .await
        .expect("本地无可用凭据时 MCP 不应跑满 retry 上限")
        .err()
        .unwrap()
        .to_string();

        assert!(
            err.contains("所有账号均已禁用"),
            "错误应直接来自本地调度失败，实际: {}",
            err
        );
    }

    #[tokio::test]
    async fn api_local_acquire_failure_attaches_selection_summary() {
        let mut config = Config::default();
        config.credential_retry_max_attempts = 100_000;

        let mut disabled = KiroCredentials::default();
        disabled.disabled = true;
        disabled.access_token = Some("secret-token-should-not-leak".to_string());
        let manager =
            Arc::new(MultiTokenManager::new(config, vec![disabled], None, None, false).unwrap());
        let mut endpoints: HashMap<String, Arc<dyn KiroEndpoint>> = HashMap::new();
        endpoints.insert("ide".to_string(), Arc::new(IdeEndpoint));
        let provider = KiroProvider::with_proxy(manager, None, endpoints, "ide".to_string());
        let body = r#"{
            "conversationState": {
                "currentMessage": {
                    "userInputMessage": {
                        "modelId": "claude-sonnet-4.5"
                    }
                }
            }
        }"#;

        let err = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            provider.call_api_with_context_with_request_id(body, Some("req-selection-1")),
        )
        .await
        .expect("本地无可用账号时 API 不应跑满 retry 上限")
        .err()
        .unwrap();

        let summary = KiroProvider::selection_failure_from_error(&err)
            .expect("API 调度失败应携带结构化选择失败摘要");
        assert_eq!(summary.request_id, "req-selection-1");
        assert_eq!(summary.route, "local_account");
        assert_eq!(summary.model, "claude-sonnet-4.5");
        assert_eq!(summary.stage, SelectionFailureStage::AccountEligibility);
        assert_eq!(summary.primary_reason, AccountRejectReason::Disabled);
        assert_eq!(
            summary.reason_counts.get(&AccountRejectReason::Disabled),
            Some(&1)
        );
        assert_eq!(summary.sampled_accounts.len(), 1);

        let serialized = serde_json::to_string(&summary).unwrap();
        assert!(!serialized.contains("secret-token-should-not-leak"));
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
            build_client(proxy.as_ref(), KIRO_CLIENT_TOTAL_TIMEOUT_SECS, tls_backend)
                .expect("创建 HTTP 客户端失败");
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
        let client = build_client(
            effective.as_ref(),
            KIRO_CLIENT_TOTAL_TIMEOUT_SECS,
            self.tls_backend,
        )?;
        cache.insert(effective, client.clone());
        Ok(client)
    }

    /// 获取凭据的脱敏展示名称，用于请求级 usage 记录。
    pub fn credential_label(&self, id: u64) -> Option<String> {
        self.token_manager.credential_display_label(id)
    }

    /// 获取当前运行时配置快照。
    pub fn runtime_config(&self) -> Config {
        self.token_manager.runtime_config()
    }

    /// 获取下游错误响应的 Retry-After 提示。
    ///
    /// 该路径只读本进程内存状态，避免在请求失败热路径触发 Admin 完整快照的
    /// PgSQL/Redis 同步。
    pub fn cooldown_retry_after_hint_secs(&self, fallback_secs: u64) -> u64 {
        self.token_manager
            .cooldown_retry_after_hint_secs(fallback_secs)
    }

    pub fn local_pool_route_state(&self, model: Option<&str>) -> LocalPoolRouteState {
        self.token_manager.local_pool_route_state(model)
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

    pub fn selection_failure_from_error(err: &anyhow::Error) -> Option<SelectionFailureSummary> {
        err.downcast_ref::<KiroCallError>()
            .and_then(|err| err.selection_failure().cloned())
    }

    fn traced_error(
        message: impl Into<String>,
        attempts: &[KiroCredentialAttempt],
    ) -> anyhow::Error {
        KiroCallError::new(message, attempts.to_vec()).into()
    }

    fn traced_error_with_selection_failure(
        message: impl Into<String>,
        attempts: &[KiroCredentialAttempt],
        selection_failure: Option<SelectionFailureSummary>,
    ) -> anyhow::Error {
        KiroCallError::new(message, attempts.to_vec())
            .with_selection_failure(selection_failure)
            .into()
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

    fn codewhisperer_host_for_region(region: &str) -> &'static str {
        if region.starts_with("eu-") {
            "codewhisperer.eu-central-1.amazonaws.com"
        } else {
            "codewhisperer.us-east-1.amazonaws.com"
        }
    }

    fn list_available_profiles_headers(
        credentials: &KiroCredentials,
        token: &str,
        config: &Config,
        machine_id: &str,
        host: &str,
    ) -> anyhow::Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {}", token))?,
        );
        headers.insert(
            "x-amz-user-agent",
            HeaderValue::from_str(&format!(
                "aws-sdk-js/1.0.34 KiroIDE-{}-{}",
                config.kiro_version, machine_id
            ))?,
        );
        headers.insert(
            "user-agent",
            HeaderValue::from_str(&format!(
                "aws-sdk-js/1.0.34 ua/2.1 os/{} lang/js md/nodejs#{} api/codewhispererruntime#1.0.34 m/E KiroIDE-{}-{}",
                config.system_version, config.node_version, config.kiro_version, machine_id
            ))?,
        );
        headers.insert("host", HeaderValue::from_str(host)?);
        headers.insert(
            "amz-sdk-invocation-id",
            HeaderValue::from_str(&uuid::Uuid::new_v4().to_string())?,
        );
        headers.insert(
            "amz-sdk-request",
            HeaderValue::from_static("attempt=1; max=1"),
        );
        headers.insert("Connection", HeaderValue::from_static("close"));

        if is_external_idp_credentials(credentials) {
            headers.insert("TokenType", HeaderValue::from_static("EXTERNAL_IDP"));
        }

        Ok(headers)
    }

    async fn fetch_enterprise_profile_arn_for_context(
        &self,
        ctx: &CallContext,
        config: &Config,
        machine_id: &str,
    ) -> anyhow::Result<Option<String>> {
        if !is_external_idp_credentials(&ctx.credentials) {
            return Ok(None);
        }
        if ctx
            .credentials
            .profile_arn
            .as_deref()
            .map(str::trim)
            .is_some_and(|arn| !arn.is_empty() && is_real_profile_arn(arn))
        {
            return Ok(ctx.credentials.profile_arn.clone());
        }

        let region = ctx.credentials.effective_api_region(config);
        let host = Self::codewhisperer_host_for_region(region);
        let url = format!("https://{}/ListAvailableProfiles", host);
        let client = self.client_for(&ctx.credentials)?;
        let request = client
            .post(&url)
            .headers(Self::list_available_profiles_headers(
                &ctx.credentials,
                &ctx.token,
                config,
                machine_id,
                host,
            )?)
            .body("{}");
        let response =
            send_with_response_header_timeout(request, config.kiro_upstream_response_timeout_secs)
                .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response_text_with_body_timeout(
                response,
                config.kiro_upstream_response_timeout_secs,
            )
            .await
            .unwrap_or_else(|err| format!("<failed to read response body: {}>", err));
            if status.as_u16() == 403 {
                tracing::warn!(
                    credential_id = ctx.id,
                    "ListAvailableProfiles 返回 403，保持本次请求使用流式 fallback，不持久化 profileArn: {}",
                    body
                );
                return Ok(None);
            }
            anyhow::bail!("ListAvailableProfiles 失败: {} {}", status, body);
        }

        let body =
            response_text_with_body_timeout(response, config.kiro_upstream_response_timeout_secs)
                .await?;

        Ok(extract_first_profile_arn(&body).filter(|arn| is_real_profile_arn(arn)))
    }

    async fn ensure_profile_arn_for_context(
        &self,
        ctx: &mut CallContext,
        config: &Config,
        machine_id: &str,
    ) {
        if !is_external_idp_credentials(&ctx.credentials) {
            return;
        }
        let existing = ctx
            .credentials
            .profile_arn
            .as_deref()
            .map(str::trim)
            .filter(|arn| !arn.is_empty() && is_real_profile_arn(arn));
        if existing.is_some() {
            return;
        }

        match self
            .fetch_enterprise_profile_arn_for_context(ctx, config, machine_id)
            .await
        {
            Ok(Some(profile_arn)) => {
                ctx.credentials.profile_arn = Some(profile_arn.clone());
                if ctx.id != EXTERNAL_CREDENTIAL_CONTEXT_ID {
                    if let Err(err) = self
                        .token_manager
                        .update_credential_profile_arn(ctx.id, Some(profile_arn.clone()))
                    {
                        tracing::warn!(
                            credential_id = ctx.id,
                            profile_arn = %profile_arn,
                            "Enterprise profileArn 已解析但持久化失败: {}",
                            err
                        );
                    } else {
                        tracing::info!(
                            credential_id = ctx.id,
                            profile_arn = %profile_arn,
                            "Enterprise profileArn 已解析并持久化"
                        );
                    }
                }
            }
            Ok(None) => {
                tracing::warn!(
                    credential_id = ctx.id,
                    "ListAvailableProfiles 未返回可用 profileArn，继续使用 fallback profileArn"
                );
            }
            Err(err) => {
                tracing::warn!(
                    credential_id = ctx.id,
                    "Enterprise profileArn 自愈失败，继续使用 fallback profileArn: {}",
                    err
                );
            }
        }
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

    fn maybe_exclude_after_transient_failure(
        &self,
        model: Option<&str>,
        credential_id: u64,
        credential_label: &str,
        excluded_ids: &mut HashSet<u64>,
    ) {
        if excluded_ids.contains(&credential_id) {
            return;
        }
        if self.token_manager.has_alternate_usable_credential_cached(
            model,
            excluded_ids,
            credential_id,
        ) {
            tracing::warn!(
                credential_id,
                credential_label = %credential_label,
                "账号发生上游瞬态错误，本次请求临时排除当前账号并重试其他账号"
            );
            excluded_ids.insert(credential_id);
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
        self.call_api_with_context_with_request_id_and_capacity_weight(request_body, request_id, 1)
            .await
    }

    pub async fn call_api_with_context_with_request_id_and_capacity_weight(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        capacity_weight_units: u32,
    ) -> anyhow::Result<KiroApiResponse> {
        self.call_api_with_context_with_request_id_and_mode(
            request_body,
            request_id,
            AcquireMode::WaitForCapacity,
            capacity_weight_units,
            None,
        )
        .await
    }

    pub async fn call_api_with_context_with_request_id_and_capacity_weight_and_model_filter(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        capacity_weight_units: u32,
        dispatch_model_filter: Option<&str>,
    ) -> anyhow::Result<KiroApiResponse> {
        self.call_api_with_context_with_request_id_and_mode(
            request_body,
            request_id,
            AcquireMode::WaitForCapacity,
            capacity_weight_units,
            dispatch_model_filter,
        )
        .await
    }

    #[allow(dead_code)]
    pub async fn call_api_with_context_with_request_id_fail_fast(
        &self,
        request_body: &str,
        request_id: Option<&str>,
    ) -> anyhow::Result<KiroApiResponse> {
        self.call_api_with_context_with_request_id_and_mode(
            request_body,
            request_id,
            AcquireMode::FailFastOnCapacity,
            1,
            None,
        )
        .await
    }

    #[allow(dead_code)]
    pub async fn call_api_with_context_with_request_id_fail_fast_and_capacity_weight(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        capacity_weight_units: u32,
    ) -> anyhow::Result<KiroApiResponse> {
        self.call_api_with_context_with_request_id_and_mode(
            request_body,
            request_id,
            AcquireMode::FailFastOnCapacity,
            capacity_weight_units,
            None,
        )
        .await
    }

    pub async fn call_api_with_context_with_request_id_fail_fast_and_capacity_weight_and_model_filter(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        capacity_weight_units: u32,
        dispatch_model_filter: Option<&str>,
    ) -> anyhow::Result<KiroApiResponse> {
        self.call_api_with_context_with_request_id_and_mode(
            request_body,
            request_id,
            AcquireMode::FailFastOnCapacity,
            capacity_weight_units,
            dispatch_model_filter,
        )
        .await
    }

    #[allow(dead_code)]
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
            1,
            None,
        )
        .await
    }

    #[allow(dead_code)]
    pub async fn call_api_with_context_with_request_id_max_wait_and_capacity_weight(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        max_wait: Duration,
        capacity_weight_units: u32,
    ) -> anyhow::Result<KiroApiResponse> {
        self.call_api_with_context_with_request_id_and_mode(
            request_body,
            request_id,
            AcquireMode::WaitForCapacityMax(max_wait),
            capacity_weight_units,
            None,
        )
        .await
    }

    pub async fn call_api_with_context_with_request_id_max_wait_and_capacity_weight_and_model_filter(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        max_wait: Duration,
        capacity_weight_units: u32,
        dispatch_model_filter: Option<&str>,
    ) -> anyhow::Result<KiroApiResponse> {
        self.call_api_with_context_with_request_id_and_mode(
            request_body,
            request_id,
            AcquireMode::WaitForCapacityMax(max_wait),
            capacity_weight_units,
            dispatch_model_filter,
        )
        .await
    }

    async fn call_api_with_context_with_request_id_and_mode(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        acquire_mode: AcquireMode,
        capacity_weight_units: u32,
        dispatch_model_filter: Option<&str>,
    ) -> anyhow::Result<KiroApiResponse> {
        let result = self
            .call_api_with_retry(
                request_body,
                false,
                request_id,
                acquire_mode,
                capacity_weight_units,
                dispatch_model_filter,
            )
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
        let ctx = self
            .token_manager
            .acquire_context_for_credential(credential_id)
            .await?;
        let credential_label = self.credential_log_label(ctx.id);
        let credential_context = format!("账号 {}", credential_label);
        self.call_api_with_single_context(ctx, request_body, credential_label, credential_context)
            .await
    }

    /// 使用外部临时凭据发送一次非流式 API 请求。
    ///
    /// 仅用于 Admin 外部 JSON 验活：不加入凭据池、不参与负载均衡、不累计调度状态。
    pub async fn call_api_with_external_credentials(
        &self,
        credentials: KiroCredentials,
        request_body: &str,
    ) -> anyhow::Result<KiroApiResponse> {
        let ctx = self
            .token_manager
            .acquire_context_for_external_credentials(credentials)
            .await?;
        self.call_api_with_single_context(
            ctx,
            request_body,
            "external".to_string(),
            "外部凭据".to_string(),
        )
        .await
    }

    /// 使用外部临时凭据同步可用模型列表。
    ///
    /// 该方法不把凭据写入调度池，适合 Admin 新增 / 导入 API Key 时先做能力发现，
    /// 然后再把发现结果写回 supportedModels。
    pub async fn list_available_models_for_external_credentials(
        &self,
        credentials: KiroCredentials,
    ) -> anyhow::Result<Vec<KiroAvailableModel>> {
        let ctx = self
            .token_manager
            .acquire_context_for_external_credentials(credentials)
            .await?;
        self.list_available_models_for_context(ctx).await
    }

    async fn call_api_with_single_context(
        &self,
        mut ctx: CallContext,
        request_body: &str,
        credential_label: String,
        credential_context: String,
    ) -> anyhow::Result<KiroApiResponse> {
        ctx.mark_in_flight_kind(InFlightKind::Test);
        let started_at = Instant::now();

        let config = self.token_manager.runtime_config();
        let machine_id = machine_id::generate_from_credentials(&ctx.credentials, &config);
        self.ensure_profile_arn_for_context(&mut ctx, &config, &machine_id)
            .await;
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
            .header("content-type", endpoint.content_type())
            .header("Connection", "close");
        let request = endpoint.decorate_api(base, &rctx);

        let response =
            send_with_response_header_timeout(request, config.kiro_upstream_response_timeout_secs)
                .await
                .map_err(|e| {
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
                    started_at,
                ),
            });
        }

        let body =
            response_text_with_body_timeout(response, config.kiro_upstream_response_timeout_secs)
                .await
                .unwrap_or_else(|err| format!("<failed to read response body: {}>", err));
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
                    last_error = Some(anyhow::anyhow!("账号 #{} 获取 token 失败: {}", id, err));
                    continue;
                }
            };
            let ctx_id = ctx.id;
            match self.list_available_models_for_context(ctx).await {
                Ok(models) if !models.is_empty() => return Ok(models),
                Ok(_) => {
                    last_error = Some(anyhow::anyhow!("账号 #{} 返回空模型列表", id));
                }
                Err(err) => {
                    let label = self.credential_log_label(ctx_id);
                    tracing::warn!(
                        credential_id = ctx_id,
                        credential_label = %label,
                        "同步 Kiro 模型能力失败: {}",
                        err
                    );
                    last_error = Some(err);
                }
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("没有已启用且可用于同步模型能力的账号")))
    }

    /// 使用指定凭据同步 Kiro 可用模型列表。
    ///
    /// 该方法会真实调用上游模型列表接口，但不占用普通请求并发槽。
    pub async fn list_available_models_for_credential(
        &self,
        id: u64,
    ) -> anyhow::Result<Vec<KiroAvailableModel>> {
        let ctx = self
            .token_manager
            .acquire_context_for_credential(id)
            .await
            .map_err(|err| anyhow::anyhow!("账号 #{} 获取 token 失败: {}", id, err))?;
        self.list_available_models_for_context(ctx).await
    }

    async fn list_available_models_for_context(
        &self,
        mut ctx: CallContext,
    ) -> anyhow::Result<Vec<KiroAvailableModel>> {
        let config = self.token_manager.runtime_config();
        let machine_id = machine_id::generate_from_credentials(&ctx.credentials, &config);
        self.ensure_profile_arn_for_context(&mut ctx, &config, &machine_id)
            .await;
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
            let mut request = match endpoint.models_method(&rctx) {
                Method::GET => client.get(&url),
                Method::POST => client.post(&url),
                method => client.request(method, &url),
            };
            if let Some(body) = endpoint.models_body(&rctx, next_token.as_deref()) {
                request = request.body(serde_json::to_vec(&body)?);
            }
            let request = endpoint.decorate_models(request, &rctx);
            let response = send_with_response_header_timeout(
                request,
                config.kiro_upstream_response_timeout_secs,
            )
            .await?;
            let status = response.status();
            let body = response_text_with_body_timeout(
                response,
                config.kiro_upstream_response_timeout_secs,
            )
            .await?;
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
        self.call_api_stream_with_request_id_and_capacity_weight(request_body, request_id, 1)
            .await
    }

    pub async fn call_api_stream_with_request_id_and_capacity_weight(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        capacity_weight_units: u32,
    ) -> anyhow::Result<KiroStreamResponse> {
        self.call_api_stream_with_request_id_and_mode(
            request_body,
            request_id,
            AcquireMode::WaitForCapacity,
            capacity_weight_units,
            None,
        )
        .await
    }

    pub async fn call_api_stream_with_request_id_and_capacity_weight_and_model_filter(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        capacity_weight_units: u32,
        dispatch_model_filter: Option<&str>,
    ) -> anyhow::Result<KiroStreamResponse> {
        self.call_api_stream_with_request_id_and_mode(
            request_body,
            request_id,
            AcquireMode::WaitForCapacity,
            capacity_weight_units,
            dispatch_model_filter,
        )
        .await
    }

    #[allow(dead_code)]
    pub async fn call_api_stream_with_request_id_fail_fast(
        &self,
        request_body: &str,
        request_id: Option<&str>,
    ) -> anyhow::Result<KiroStreamResponse> {
        self.call_api_stream_with_request_id_and_mode(
            request_body,
            request_id,
            AcquireMode::FailFastOnCapacity,
            1,
            None,
        )
        .await
    }

    #[allow(dead_code)]
    pub async fn call_api_stream_with_request_id_fail_fast_and_capacity_weight(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        capacity_weight_units: u32,
    ) -> anyhow::Result<KiroStreamResponse> {
        self.call_api_stream_with_request_id_and_mode(
            request_body,
            request_id,
            AcquireMode::FailFastOnCapacity,
            capacity_weight_units,
            None,
        )
        .await
    }

    pub async fn call_api_stream_with_request_id_fail_fast_and_capacity_weight_and_model_filter(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        capacity_weight_units: u32,
        dispatch_model_filter: Option<&str>,
    ) -> anyhow::Result<KiroStreamResponse> {
        self.call_api_stream_with_request_id_and_mode(
            request_body,
            request_id,
            AcquireMode::FailFastOnCapacity,
            capacity_weight_units,
            dispatch_model_filter,
        )
        .await
    }

    #[allow(dead_code)]
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
            1,
            None,
        )
        .await
    }

    #[allow(dead_code)]
    pub async fn call_api_stream_with_request_id_max_wait_and_capacity_weight(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        max_wait: Duration,
        capacity_weight_units: u32,
    ) -> anyhow::Result<KiroStreamResponse> {
        self.call_api_stream_with_request_id_and_mode(
            request_body,
            request_id,
            AcquireMode::WaitForCapacityMax(max_wait),
            capacity_weight_units,
            None,
        )
        .await
    }

    pub async fn call_api_stream_with_request_id_max_wait_and_capacity_weight_and_model_filter(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        max_wait: Duration,
        capacity_weight_units: u32,
        dispatch_model_filter: Option<&str>,
    ) -> anyhow::Result<KiroStreamResponse> {
        self.call_api_stream_with_request_id_and_mode(
            request_body,
            request_id,
            AcquireMode::WaitForCapacityMax(max_wait),
            capacity_weight_units,
            dispatch_model_filter,
        )
        .await
    }

    async fn call_api_stream_with_request_id_and_mode(
        &self,
        request_body: &str,
        request_id: Option<&str>,
        acquire_mode: AcquireMode,
        capacity_weight_units: u32,
        dispatch_model_filter: Option<&str>,
    ) -> anyhow::Result<KiroStreamResponse> {
        let result = self
            .call_api_with_retry(
                request_body,
                true,
                request_id,
                acquire_mode,
                capacity_weight_units,
                dispatch_model_filter,
            )
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
                    if last_error.is_none() {
                        last_error = Some(e);
                    } else {
                        tracing::warn!(
                            error = %e,
                            "MCP 获取凭据失败，但保留之前的上游错误"
                        );
                    }
                    break;
                }
            };
            ctx.mark_in_flight_kind(InFlightKind::Mcp);
            let credential_label = self.credential_log_label(ctx.id);
            let credential_context = format!("账号 {}", credential_label);
            let attempt_started_at = Instant::now();

            let config = self.token_manager.runtime_config();
            let machine_id = machine_id::generate_from_credentials(&ctx.credentials, &config);
            self.ensure_profile_arn_for_context(&mut ctx, &config, &machine_id)
                .await;

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
                .header("content-type", endpoint.content_type())
                .header("Connection", "close");
            let request = endpoint.decorate_mcp(base, &rctx);

            let response = match send_with_response_header_timeout(
                request,
                config.kiro_upstream_response_timeout_secs,
            )
            .await
            {
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
                    self.maybe_exclude_after_transient_failure(
                        None,
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
                self.token_manager.report_success_with_latency(
                    ctx.id,
                    None,
                    Some(attempt_started_at.elapsed()),
                );
                self.finish_attempt(&mut ctx);
                return Ok(response);
            }

            // 失败响应
            let body = response_text_with_body_timeout(
                response,
                config.kiro_upstream_response_timeout_secs,
            )
            .await
            .unwrap_or_else(|err| format!("<failed to read response body: {}>", err));

            if let Some(risk_reason) = Self::detect_risk_control_error(status, &body) {
                if Self::should_downgrade_rate_limit_risk_to_cooldown(
                    status,
                    risk_reason,
                    &ctx.credentials,
                ) {
                    tracing::warn!(
                        credential_id = ctx.id,
                        credential_label = %credential_label,
                        risk_reason = ?risk_reason,
                        "MCP 请求失败（{}，429 临时风控按账号配置仅进入冷却并切换，尝试 {}/{}）: {} {}",
                        credential_context,
                        attempt + 1,
                        max_retries,
                        status,
                        body
                    );
                    if let Err(err) = self.token_manager.report_transient_failure_kind(
                        ctx.id,
                        None,
                        TransientFailureKind::RateLimit,
                        retry_after,
                        format!("rate_limit_risk_control {} {}", status, body),
                    ) {
                        self.finish_attempt(&mut ctx);
                        anyhow::bail!(
                            "MCP 请求失败（{}，调度状态写入失败）: {}",
                            credential_context,
                            err
                        );
                    }
                    last_error = Some(anyhow::anyhow!(
                        "MCP 请求失败（{}）: {} {}",
                        credential_context,
                        status,
                        body
                    ));
                    self.maybe_exclude_after_transient_failure(
                        None,
                        ctx.id,
                        &credential_label,
                        &mut excluded_ids,
                    );
                    self.finish_attempt(&mut ctx);
                    continue;
                }

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
                        "MCP 请求失败（{}，所有账号已用尽）: {} {}",
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
                        "MCP 请求失败（{}，所有账号已用尽）: {} {}",
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
                self.maybe_exclude_after_transient_failure(
                    None,
                    ctx.id,
                    &credential_label,
                    &mut excluded_ids,
                );
                self.finish_attempt(&mut ctx);
                continue;
            }

            // 400 Bad Request
            if status.as_u16() == 400 {
                let bad_request_reason = Self::classify_bad_request_reason(&body);
                self.finish_attempt(&mut ctx);
                anyhow::bail!(
                    "MCP 请求失败（{}，{}）: {} {}",
                    credential_context,
                    Self::bad_request_reason_label(bad_request_reason),
                    status,
                    body
                );
            }

            // 401/403 凭据问题
            if matches!(status.as_u16(), 401 | 403) {
                tracing::warn!(
                    credential_id = ctx.id,
                    credential_label = %credential_label,
                    "MCP 请求失败（{}，可能为账号认证错误，尝试 {}/{}）: {} {}",
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
                        "MCP 请求失败（{}，所有账号已用尽）: {} {}",
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
                self.maybe_exclude_after_transient_failure(
                    None,
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
            self.maybe_exclude_after_transient_failure(
                None,
                ctx.id,
                &credential_label,
                &mut excluded_ids,
            );
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
        capacity_weight_units: u32,
        dispatch_model_filter: Option<&str>,
    ) -> anyhow::Result<ApiCallResponse> {
        let total_credentials = self.token_manager.total_count();
        let max_retries =
            Self::max_retry_attempts(total_credentials, &self.token_manager.runtime_config());
        let mut last_error: Option<anyhow::Error> = None;
        let mut last_selection_failure: Option<SelectionFailureSummary> = None;
        let mut force_refreshed: HashSet<u64> = HashSet::new();
        let api_type = if is_stream { "流式" } else { "非流式" };
        let mut attempts: Vec<KiroCredentialAttempt> = Vec::new();

        let model = dispatch_model_filter
            .map(str::to_string)
            .or_else(|| Self::extract_model_from_request(request_body));
        let conversation_id = Self::extract_conversation_id_from_request(request_body);
        let mut excluded_ids: HashSet<u64> = HashSet::new();
        let mut prompt_logic_retry_count = 0usize;

        for attempt in 0..max_retries {
            // 获取调用上下文（绑定 index、credentials、token）
            let mut ctx = match self
                .token_manager
                .acquire_context_for_session_with_mode(
                    model.as_deref(),
                    conversation_id.as_deref(),
                    &excluded_ids,
                    acquire_mode,
                    capacity_weight_units,
                )
                .await
            {
                Ok(c) => c,
                Err(e) => {
                    let selection_failure = self.token_manager.selection_failure_summary(
                        request_id.unwrap_or_default(),
                        "local_account",
                        model.as_deref(),
                        &e.to_string(),
                    );
                    if last_error.is_none() {
                        last_selection_failure = Some(selection_failure);
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
            let credential_context = format!("账号 {}", credential_label);
            let attempt_started_at = Instant::now();

            let config = self.token_manager.runtime_config();
            let machine_id = machine_id::generate_from_credentials(&ctx.credentials, &config);
            self.ensure_profile_arn_for_context(&mut ctx, &config, &machine_id)
                .await;

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
                .header("content-type", endpoint.content_type())
                .header("Connection", "close");
            let request = endpoint.decorate_api(base, &rctx);

            let response = match send_with_response_header_timeout(
                request,
                config.kiro_upstream_response_timeout_secs,
            )
            .await
            {
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
                    // 网络错误通常是上游/链路瞬态问题，不禁用凭据；若本机内存态存在备选，
                    // 仅在当前请求内临时排除失败账号，避免重试链反复命中同一账号。
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
                    self.maybe_exclude_after_transient_failure(
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
                    let body = response_text_with_body_timeout(
                        response,
                        config.kiro_upstream_response_timeout_secs,
                    )
                    .await
                    .unwrap_or_else(|err| format!("<failed to read response body: {}>", err));
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
                        self.maybe_exclude_after_transient_failure(
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
            let body = response_text_with_body_timeout(
                response,
                config.kiro_upstream_response_timeout_secs,
            )
            .await
            .unwrap_or_else(|err| format!("<failed to read response body: {}>", err));

            if let Some(risk_reason) = Self::detect_risk_control_error(status, &body) {
                let message = format!(
                    "{} API 请求失败（{}）: {} {}",
                    api_type, credential_context, status, body
                );
                if Self::should_downgrade_rate_limit_risk_to_cooldown(
                    status,
                    risk_reason,
                    &ctx.credentials,
                ) {
                    tracing::warn!(
                        credential_id = ctx.id,
                        credential_label = %credential_label,
                        risk_reason = ?risk_reason,
                        "API 请求失败（{}，429 临时风控按账号配置仅进入冷却并切换，尝试 {}/{}）: {} {}",
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
                        "rate_limit_cooldown_retry",
                        Some("rate_limit_risk_control"),
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
                        format!(
                            "rate_limit_risk_control {} API {} {}",
                            api_type, status, body
                        ),
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
                    self.maybe_exclude_after_transient_failure(
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
                        "{} API 请求失败（{}，所有账号已用尽）: {} {}",
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
                        "{} API 请求失败（{}，所有账号已用尽）: {} {}",
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

            // 400 Bad Request - 大多数是请求问题；模型/账号能力不匹配时允许换凭据重试。
            if status.as_u16() == 400 {
                let bad_request_reason = Self::classify_bad_request_reason(&body);
                let message = format!(
                    "{} API 请求失败（{}，{}）: {} {}",
                    api_type,
                    credential_context,
                    Self::bad_request_reason_label(bad_request_reason),
                    status,
                    body
                );
                if Self::should_retry_model_unavailable_bad_request(
                    bad_request_reason,
                    model.as_deref(),
                ) && attempt + 1 < max_retries
                    && self.token_manager.has_alternate_usable_credential_cached(
                        model.as_deref(),
                        &excluded_ids,
                        ctx.id,
                    )
                {
                    tracing::warn!(
                        credential_id = ctx.id,
                        credential_label = %credential_label,
                        model = model.as_deref(),
                        "API 请求失败（{}，当前账号不支持/不可用该模型，换未尝试账号重试，尝试 {}/{}）: {} {}",
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
                        "model_unavailable_retry_next",
                        Some(bad_request_reason),
                        Some(message.clone()),
                        attempt_started_at,
                        model.as_deref(),
                    );
                    if let Err(err) = self.token_manager.report_transient_failure_kind(
                        ctx.id,
                        model.as_deref(),
                        TransientFailureKind::Protocol,
                        retry_after,
                        format!("model_unavailable_bad_request {} {}", status, body),
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
                    if let Some(session_id) = conversation_id.as_deref() {
                        self.token_manager
                            .unbind_session_if_bound_to(session_id, ctx.id);
                    }
                    excluded_ids.insert(ctx.id);
                    last_error = Some(anyhow::anyhow!(message));
                    self.finish_attempt(&mut ctx);
                    continue;
                }
                if bad_request_reason == "profile_arn_bad_request" {
                    tracing::warn!(
                        credential_id = ctx.id,
                        credential_label = %credential_label,
                        "API 请求失败（{}，profileArn 可能失效，清理 profileArn 并尝试其他账号，尝试 {}/{}）: {} {}",
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
                        "profile_arn_retry",
                        Some(bad_request_reason),
                        Some(message.clone()),
                        attempt_started_at,
                        model.as_deref(),
                    );
                    if let Err(err) = self
                        .token_manager
                        .update_credential_profile_arn(ctx.id, None)
                    {
                        let final_message = format!(
                            "{} API 请求失败（{}，profileArn 状态清理失败）: {}",
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
                    if let Err(err) = self.token_manager.report_transient_failure_kind(
                        ctx.id,
                        model.as_deref(),
                        TransientFailureKind::Protocol,
                        None,
                        format!("profile_arn_bad_request {} {}", status, body),
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
                    if let Some(session_id) = conversation_id.as_deref() {
                        self.token_manager
                            .unbind_session_if_bound_to(session_id, ctx.id);
                    }
                    if self.token_manager.has_alternate_usable_credential(
                        model.as_deref(),
                        &excluded_ids,
                        ctx.id,
                    ) {
                        excluded_ids.insert(ctx.id);
                    }
                    last_error = Some(anyhow::anyhow!(message));
                    self.finish_attempt(&mut ctx);
                    continue;
                }
                if Self::should_retry_prompt_logic_bad_request(
                    bad_request_reason,
                    model.as_deref(),
                    &config,
                    prompt_logic_retry_count,
                ) && self.token_manager.has_alternate_usable_credential(
                    model.as_deref(),
                    &excluded_ids,
                    ctx.id,
                ) {
                    tracing::warn!(
                        credential_id = ctx.id,
                        credential_label = %credential_label,
                        bad_request_reason,
                        prompt_logic_retry_count = prompt_logic_retry_count + 1,
                        "API 请求失败（{}，提示/协议逻辑错误，按配置换未尝试账号重试，尝试 {}/{}）: {} {}",
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
                        "prompt_logic_retry_next",
                        Some(bad_request_reason),
                        Some(message.clone()),
                        attempt_started_at,
                        model.as_deref(),
                    );
                    if let Some(session_id) = conversation_id.as_deref() {
                        self.token_manager
                            .unbind_session_if_bound_to(session_id, ctx.id);
                    }
                    excluded_ids.insert(ctx.id);
                    prompt_logic_retry_count = prompt_logic_retry_count.saturating_add(1);
                    last_error = Some(anyhow::anyhow!(message));
                    self.finish_attempt(&mut ctx);
                    continue;
                }
                Self::push_attempt(
                    &mut attempts,
                    attempt,
                    ctx.id,
                    &credential_label,
                    Some(status),
                    "fail",
                    Some(bad_request_reason),
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
                    "API 请求失败（{}，可能为账号认证错误，尝试 {}/{}）: {} {}",
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
                        "{} API 请求失败（{}，所有账号已用尽）: {} {}",
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

            // 429/408/5xx - 瞬态上游错误：不禁用凭据；若本机内存态存在备选，
            // 仅在当前请求内临时排除失败账号，避免重试链反复命中同一账号。
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
                self.maybe_exclude_after_transient_failure(
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

            // 兜底：当作可重试的瞬态错误处理；若本机内存态存在备选，
            // 仅在当前请求内临时排除失败账号。
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
            self.maybe_exclude_after_transient_failure(
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
        Err(Self::traced_error_with_selection_failure(
            message,
            &attempts,
            last_selection_failure,
        ))
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

    fn should_downgrade_rate_limit_risk_to_cooldown(
        status: reqwest::StatusCode,
        reason: CredentialRiskControlReason,
        credentials: &KiroCredentials,
    ) -> bool {
        status == reqwest::StatusCode::TOO_MANY_REQUESTS
            && reason == CredentialRiskControlReason::TemporarilySuspended
            && !credentials.rate_limit_auto_disable_enabled()
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
            || (status.as_u16() == 429
                && lower.contains("suspicious activity")
                && lower.contains("temporary limits"))
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

    fn classify_bad_request_reason(body: &str) -> &'static str {
        let lower = body.to_ascii_lowercase();
        if lower.contains("assistant-prefill")
            || lower.contains("assistant prefill")
            || lower.contains("last message must be user")
        {
            return "assistant_prefill_bad_request";
        }
        if lower.contains("profilearn")
            || lower.contains("profile arn")
            || lower.contains("profile_arn")
        {
            return "profile_arn_bad_request";
        }
        if Self::bad_request_body_indicates_retryable_model_unavailable(&lower) {
            return "model_unavailable_bad_request";
        }
        if Self::bad_request_body_indicates_invalid_model(&lower) {
            return "model_invalid_bad_request";
        }
        if lower.contains("invalid tool use format") || lower.contains("request_body_invalid") {
            return "tool_use_format_bad_request";
        }
        if lower.contains("improperly formed")
            || lower.contains("malformed")
            || lower.contains("invalid request body")
        {
            return "malformed_request";
        }
        "bad_request"
    }

    fn bad_request_body_indicates_retryable_model_unavailable(lower_body: &str) -> bool {
        [
            "requested model is not available",
            "model is not available for this endpoint",
            "not available for this endpoint",
            "not available in this region",
            "not supported in this region",
            "not available for this account",
            "not enabled for this account",
        ]
        .iter()
        .any(|needle| lower_body.contains(needle))
    }

    fn bad_request_body_indicates_invalid_model(lower_body: &str) -> bool {
        [
            "invalid model",
            "invalid_model_id",
            "invalid_model",
            "model not found",
            "model_not_found",
            "unsupported model",
            "unknown model",
        ]
        .iter()
        .any(|needle| lower_body.contains(needle))
    }

    fn should_retry_model_unavailable_bad_request(reason: &str, model: Option<&str>) -> bool {
        reason == "model_unavailable_bad_request"
            && model.map(str::trim).is_some_and(|value| !value.is_empty())
    }

    fn should_retry_prompt_logic_bad_request(
        reason: &str,
        model: Option<&str>,
        config: &Config,
        already_retried: usize,
    ) -> bool {
        if !config.credential_prompt_logic_retry_enabled {
            return false;
        }
        if model.map(str::trim).is_none_or(str::is_empty) {
            return false;
        }
        if !matches!(
            reason,
            "tool_use_format_bad_request" | "assistant_prefill_bad_request"
        ) {
            return false;
        }
        let max_attempts = if config.credential_prompt_logic_retry_max_attempts == 0 {
            1
        } else {
            config.credential_prompt_logic_retry_max_attempts as usize
        };
        already_retried < max_attempts
    }

    fn bad_request_reason_label(reason: &str) -> &'static str {
        match reason {
            "model_unavailable_bad_request" | "model_invalid_bad_request" => "模型不可用",
            "assistant_prefill_bad_request"
            | "profile_arn_bad_request"
            | "tool_use_format_bad_request"
            | "malformed_request"
            | "bad_request" => "请求无效",
            _ => "请求无效",
        }
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
