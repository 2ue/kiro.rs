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
use std::time::Duration;
use tokio::time::sleep;

use crate::http_client::{ProxyConfig, build_client};
use crate::kiro::endpoint::{KiroEndpoint, RequestContext};
use crate::kiro::machine_id;
use crate::kiro::model::available_models::{KiroAvailableModel, KiroAvailableModelsResponse};
use crate::kiro::model::credentials::KiroCredentials;
use crate::kiro::token_manager::{
    CallContext, CredentialRiskControlReason, InFlightKind, InFlightLeaseGuard, MultiTokenManager,
};
use crate::model::config::{Config, TlsBackend};
use parking_lot::Mutex;

/// 每个凭据的最大重试次数
const MAX_RETRIES_PER_CREDENTIAL: usize = 3;

/// 总重试次数硬上限（避免无限重试）
const MAX_TOTAL_RETRIES: usize = 9;

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
    sticky_bound: bool,
    fallback_from_sticky: bool,
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
    sticky_bound: bool,
    fallback_from_sticky: bool,
    reported: AtomicBool,
}

impl KiroApiCompletion {
    fn new(
        token_manager: Arc<MultiTokenManager>,
        credential_id: u64,
        in_flight_lease: Option<InFlightLeaseGuard>,
        session_id: Option<String>,
        sticky_bound: bool,
        fallback_from_sticky: bool,
    ) -> Self {
        Self {
            token_manager,
            credential_id,
            in_flight_lease: Mutex::new(in_flight_lease),
            session_id,
            sticky_bound,
            fallback_from_sticky,
            reported: AtomicBool::new(false),
        }
    }

    pub fn report_success(&self) {
        if self.reported.swap(true, Ordering::AcqRel) {
            return;
        }
        if self.in_flight_lease.lock().take().is_some() {
            self.token_manager
                .report_success_for_session(self.credential_id, self.session_id.as_deref());
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
    sticky_bound: bool,
    fallback_from_sticky: bool,
    reported: AtomicBool,
}

impl KiroStreamCompletion {
    fn new(
        token_manager: Arc<MultiTokenManager>,
        credential_id: u64,
        in_flight_lease: Option<InFlightLeaseGuard>,
        session_id: Option<String>,
        sticky_bound: bool,
        fallback_from_sticky: bool,
    ) -> Self {
        Self {
            token_manager,
            credential_id,
            in_flight_lease: Mutex::new(in_flight_lease),
            session_id,
            sticky_bound,
            fallback_from_sticky,
            reported: AtomicBool::new(false),
        }
    }

    /// 上游流正常 EOF 后调用，计入成功并清理 sticky 软失败计数。
    pub fn report_success(&self) {
        if self.reported.swap(true, Ordering::AcqRel) {
            return;
        }
        self.token_manager
            .report_success_for_session(self.credential_id, self.session_id.as_deref());
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
                r#"{"message":"The bearer token included in the request is invalid"}"#
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
            false,
            false,
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
            false,
            false,
        );

        completion.report_soft_failure();
        completion.report_success();

        let snapshot = manager.snapshot();
        assert_eq!(snapshot.entries[0].success_count, 0);
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
                false,
                false,
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
            false,
            false,
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
        let result = self.call_api_with_retry(request_body, false).await?;
        Ok(KiroApiResponse {
            response: result.response,
            completion: KiroApiCompletion::new(
                self.token_manager.clone(),
                result.credential_id,
                result.in_flight_lease,
                result.session_id,
                result.sticky_bound,
                result.fallback_from_sticky,
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
                    false,
                    false,
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
    /// 不会禁用凭据，也不会占用请求并发槽。
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

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("没有可用于同步模型能力的凭据")))
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
    pub async fn call_api_stream(&self, request_body: &str) -> anyhow::Result<KiroStreamResponse> {
        let result = self.call_api_with_retry(request_body, true).await?;
        Ok(KiroStreamResponse {
            response: result.response,
            completion: KiroStreamCompletion::new(
                self.token_manager.clone(),
                result.credential_id,
                result.in_flight_lease,
                result.session_id,
                result.sticky_bound,
                result.fallback_from_sticky,
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
        let max_retries = (total_credentials * MAX_RETRIES_PER_CREDENTIAL).min(MAX_TOTAL_RETRIES);
        let mut last_error: Option<anyhow::Error> = None;
        let mut force_refreshed: HashSet<u64> = HashSet::new();

        for attempt in 0..max_retries {
            // MCP 调用（WebSearch 等工具）不涉及模型选择，无需按模型过滤凭据
            let mut ctx = match self.token_manager.acquire_context(None).await {
                Ok(c) => c,
                Err(e) => {
                    last_error = Some(e);
                    continue;
                }
            };
            ctx.mark_in_flight_kind(InFlightKind::Mcp);
            let credential_label = self.credential_log_label(ctx.id);
            let credential_context = format!("凭据 {}", credential_label);

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
                self.token_manager.report_success(ctx.id);
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
                // token 被上游失效：先尝试 force-refresh，每凭据仅一次机会
                if endpoint.is_bearer_token_invalid(&body) && !force_refreshed.contains(&ctx.id) {
                    force_refreshed.insert(ctx.id);
                    tracing::info!(
                        credential_id = ctx.id,
                        credential_label = %credential_label,
                        "MCP 凭据 token 疑似被上游失效，尝试强制刷新"
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
                            "MCP 凭据 token 强制刷新成功，重试请求"
                        );
                        self.finish_attempt(&mut ctx);
                        continue;
                    }
                    tracing::warn!(
                        credential_id = ctx.id,
                        credential_label = %credential_label,
                        "MCP 凭据 token 强制刷新失败，计入失败"
                    );
                }

                let has_available = self.token_manager.report_failure(ctx.id);
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
                if let Err(err) = self.token_manager.report_transient_failure(
                    ctx.id,
                    None,
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
    /// - 每个凭据最多重试 MAX_RETRIES_PER_CREDENTIAL 次
    /// - 总重试次数 = min(凭据数量 × 每凭据重试次数, MAX_TOTAL_RETRIES)
    /// - 硬上限 9 次，避免无限重试
    async fn call_api_with_retry(
        &self,
        request_body: &str,
        is_stream: bool,
    ) -> anyhow::Result<ApiCallResponse> {
        let total_credentials = self.token_manager.total_count();
        let max_retries = (total_credentials * MAX_RETRIES_PER_CREDENTIAL).min(MAX_TOTAL_RETRIES);
        let mut last_error: Option<anyhow::Error> = None;
        let mut force_refreshed: HashSet<u64> = HashSet::new();
        let api_type = if is_stream { "流式" } else { "非流式" };

        // 尝试从请求体中提取模型信息
        let model = Self::extract_model_from_request(request_body);
        let conversation_id = Self::extract_conversation_id_from_request(request_body);
        let mut excluded_ids: HashSet<u64> = HashSet::new();

        for attempt in 0..max_retries {
            // 获取调用上下文（绑定 index、credentials、token）
            let mut ctx = match self
                .token_manager
                .acquire_context_for_session(
                    model.as_deref(),
                    conversation_id.as_deref(),
                    &excluded_ids,
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

            let config = self.token_manager.runtime_config();
            let machine_id = machine_id::generate_from_credentials(&ctx.credentials, &config);

            let endpoint = match self.endpoint_for(&ctx.credentials) {
                Ok(e) => e,
                Err(e) => {
                    last_error = Some(anyhow::anyhow!(
                        "{} API 凭据 endpoint 解析失败（{}）: {}",
                        api_type,
                        credential_context,
                        e
                    ));
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

            let client = match self.client_for(&ctx.credentials) {
                Ok(client) => client,
                Err(e) => {
                    self.finish_attempt(&mut ctx);
                    anyhow::bail!(
                        "{} API 创建 HTTP client 失败（{}）: {}",
                        api_type,
                        credential_context,
                        e
                    );
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
                    last_error = Some(anyhow::anyhow!(
                        "{} API 请求发送失败（{}）: {}",
                        api_type,
                        credential_context,
                        e
                    ));
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

                    let err = anyhow::anyhow!(
                        "{} API 返回非 eventstream 响应（{}）: content-type={}, exception={:?}, body={}",
                        api_type,
                        credential_context,
                        content_type,
                        exception,
                        body
                    );

                    if attempt + 1 < max_retries
                        && exception
                            .as_deref()
                            .is_some_and(Self::is_retryable_aws_exception)
                    {
                        self.maybe_exclude_after_soft_failure(
                            conversation_id.as_deref(),
                            model.as_deref(),
                            ctx.id,
                            &credential_label,
                            &mut excluded_ids,
                        );
                        last_error = Some(err);
                        self.finish_attempt(&mut ctx);
                        sleep(Self::retry_delay(attempt)).await;
                        continue;
                    }

                    self.finish_attempt(&mut ctx);
                    return Err(err);
                }
                return Ok(ApiCallResponse {
                    response,
                    credential_id: ctx.id,
                    in_flight_lease: ctx.take_in_flight_lease(),
                    session_id: conversation_id.clone(),
                    sticky_bound: ctx.sticky_bound,
                    fallback_from_sticky: ctx.fallback_from_sticky,
                });
            }

            // 失败响应：读取 body 用于日志/错误信息
            let body = response.text().await.unwrap_or_default();

            if let Some(risk_reason) = Self::detect_risk_control_error(status, &body) {
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
                    self.finish_attempt(&mut ctx);
                    anyhow::bail!(
                        "{} API 请求失败（{}，所有凭据已用尽）: {} {}",
                        api_type,
                        credential_context,
                        status,
                        body
                    );
                }
                last_error = Some(anyhow::anyhow!(
                    "{} API 请求失败（{}）: {} {}",
                    api_type,
                    credential_context,
                    status,
                    body
                ));
                self.finish_attempt(&mut ctx);
                continue;
            }

            // 402 Payment Required 且额度用尽：禁用凭据并故障转移
            if status.as_u16() == 402 && endpoint.is_monthly_request_limit(&body) {
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
                    self.finish_attempt(&mut ctx);
                    anyhow::bail!(
                        "{} API 请求失败（{}，所有凭据已用尽）: {} {}",
                        api_type,
                        credential_context,
                        status,
                        body
                    );
                }

                last_error = Some(anyhow::anyhow!(
                    "{} API 请求失败（{}）: {} {}",
                    api_type,
                    credential_context,
                    status,
                    body
                ));
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

            // 400 Bad Request - 请求问题，重试/切换凭据无意义
            if status.as_u16() == 400 {
                self.finish_attempt(&mut ctx);
                anyhow::bail!(
                    "{} API 请求失败（{}）: {} {}",
                    api_type,
                    credential_context,
                    status,
                    body
                );
            }

            // 401/403 - 更可能是凭据/权限问题：计入失败并允许故障转移
            if matches!(status.as_u16(), 401 | 403) {
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

                // token 被上游失效：先尝试 force-refresh，每凭据仅一次机会
                if endpoint.is_bearer_token_invalid(&body) && !force_refreshed.contains(&ctx.id) {
                    force_refreshed.insert(ctx.id);
                    tracing::info!(
                        credential_id = ctx.id,
                        credential_label = %credential_label,
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
                            "凭据 token 强制刷新成功，重试请求"
                        );
                        self.finish_attempt(&mut ctx);
                        continue;
                    }
                    tracing::warn!(
                        credential_id = ctx.id,
                        credential_label = %credential_label,
                        "凭据 token 强制刷新失败，计入失败"
                    );
                }

                let has_available = self.token_manager.report_failure(ctx.id);
                if !has_available {
                    if let Some(session_id) = conversation_id.as_deref() {
                        self.token_manager
                            .unbind_session_if_bound_to(session_id, ctx.id);
                    }
                }
                if !has_available {
                    self.finish_attempt(&mut ctx);
                    anyhow::bail!(
                        "{} API 请求失败（{}，所有凭据已用尽）: {} {}",
                        api_type,
                        credential_context,
                        status,
                        body
                    );
                }

                last_error = Some(anyhow::anyhow!(
                    "{} API 请求失败（{}）: {} {}",
                    api_type,
                    credential_context,
                    status,
                    body
                ));
                self.finish_attempt(&mut ctx);
                continue;
            }

            // 429/408/5xx - 瞬态上游错误：重试但不禁用或切换凭据
            // （避免 429 high traffic / 502 high load 等瞬态错误把所有凭据锁死）
            if matches!(status.as_u16(), 408 | 429) || status.is_server_error() {
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
                last_error = Some(anyhow::anyhow!(
                    "{} API 请求失败（{}）: {} {}",
                    api_type,
                    credential_context,
                    status,
                    body
                ));
                if let Err(err) = self.token_manager.report_transient_failure(
                    ctx.id,
                    model.as_deref(),
                    retry_after,
                    format!("{} {}", status, body),
                ) {
                    self.finish_attempt(&mut ctx);
                    anyhow::bail!(
                        "{} API 请求失败（{}，调度状态写入失败）: {}",
                        api_type,
                        credential_context,
                        err
                    );
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
                self.finish_attempt(&mut ctx);
                anyhow::bail!(
                    "{} API 请求失败（{}）: {} {}",
                    api_type,
                    credential_context,
                    status,
                    body
                );
            }

            // 兜底：当作可重试的瞬态错误处理（不切换凭据）
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
            last_error = Some(anyhow::anyhow!(
                "{} API 请求失败（{}）: {} {}",
                api_type,
                credential_context,
                status,
                body
            ));
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
        Err(last_error.unwrap_or_else(|| {
            anyhow::anyhow!(
                "{} API 请求失败：已达到最大重试次数（{}次）",
                api_type,
                max_retries
            )
        }))
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
        {
            return Some(CredentialRiskControlReason::TemporarilySuspended);
        }

        if body.contains("PERMANENTLY_SUSPENDED")
            || body.contains("ACCOUNT_SUSPENDED")
            || body.contains("AccountSuspendedException")
            || lower.contains("account suspended")
            || lower.contains("permanently suspended")
            || lower.contains("user is suspended")
        {
            return Some(CredentialRiskControlReason::AccountSuspended);
        }

        if lower.contains("account locked")
            || lower.contains("user locked")
            || lower.contains("locked account")
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
