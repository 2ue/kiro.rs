use anyhow::bail;
use chrono::{DateTime, Duration, Utc};
use reqwest::header::HeaderMap;
use serde::de::DeserializeOwned;
use serde_json::json;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration as StdDuration;

use crate::anthropic::inference_attempt_budget::{AuxiliaryAttemptBudget, AuxiliaryAttemptKind};
use crate::http_client::{
    HttpSendError, ProxyConfig, build_client, execute_with_response_header_timeout,
    response_bytes_with_limit_and_body_timeout, send_with_response_header_timeout,
};
use crate::kiro::endpoint::configured_upstream_url;
use crate::kiro::machine_id;
use crate::kiro::model::credentials::KiroCredentials;
use crate::kiro::model::token_refresh::{
    ExternalIdpRefreshResponse, IdcRefreshRequest, IdcRefreshResponse, RefreshRequest,
    RefreshResponse,
};
use crate::kiro::model::usage_limits::UsageLimitsResponse;
use crate::kiro::protocol::{is_external_idp_credentials, resolve_profile_arn};
use crate::model::config::Config;

use super::auxiliary::{
    AuxiliaryConcurrencyController, AuxiliaryConcurrencyKind, AuxiliaryConcurrencyPermit,
    TokenRefreshAdmissionController,
};

const TOKEN_SERVICE_RESPONSE_MAX_BYTES: usize = 1024 * 1024;
const TOKEN_SERVICE_RESPONSE_TIMEOUT_SECS: u64 = 60;
#[cfg(test)]
// Test-only timeout used by fake OAuth endpoints. Keep it short enough to prove bounded
// failure recovery, but not so tight that full-suite parallel scheduling starves recovery
// success requests before the local fake server can send headers.
const CONTROLLED_TEST_RESPONSE_HEADER_TIMEOUT: StdDuration = StdDuration::from_millis(1000);
#[cfg(test)]
const CONTROLLED_TEST_RESPONSE_HEADER_TIMEOUT_MARKER: &str = "kiro_test_timeout_ms=1000";

fn uses_controlled_test_response_header_timeout(token_endpoint: &str) -> bool {
    #[cfg(test)]
    {
        token_endpoint.contains(CONTROLLED_TEST_RESPONSE_HEADER_TIMEOUT_MARKER)
    }
    #[cfg(not(test))]
    {
        let _ = token_endpoint;
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefreshFailureStage {
    Validation,
    RequestSend,
    ResponseHeaders,
    ResponseBody,
    ResponseStatus,
    ResponseDecode,
    ResponseValidate,
    Coordination,
    Persistence,
    Internal,
}

impl RefreshFailureStage {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Validation => "validation",
            Self::RequestSend => "request_send",
            Self::ResponseHeaders => "response_headers",
            Self::ResponseBody => "response_body",
            Self::ResponseStatus => "response_status",
            Self::ResponseDecode => "response_decode",
            Self::ResponseValidate => "response_validate",
            Self::Coordination => "coordination",
            Self::Persistence => "persistence",
            Self::Internal => "internal",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefreshFailureKind {
    InvalidGrant,
    CredentialAuth,
    RateLimited,
    UpstreamUnavailable,
    Network,
    Timeout,
    Protocol,
    Oversize,
    MalformedResponse,
    MissingToken,
    InvalidConfiguration,
    Coordination,
    Persistence,
    Internal,
}

impl RefreshFailureKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::InvalidGrant => "invalid_grant",
            Self::CredentialAuth => "credential_auth",
            Self::RateLimited => "rate_limited",
            Self::UpstreamUnavailable => "upstream_unavailable",
            Self::Network => "network",
            Self::Timeout => "timeout",
            Self::Protocol => "protocol",
            Self::Oversize => "oversize",
            Self::MalformedResponse => "malformed_response",
            Self::MissingToken => "missing_token",
            Self::InvalidConfiguration => "invalid_configuration",
            Self::Coordination => "coordination",
            Self::Persistence => "persistence",
            Self::Internal => "internal",
        }
    }
}

/// Low-cardinality refresh failure safe for logs and public error chains.
///
/// It intentionally never stores the response body, endpoint URL, request body, or the
/// underlying error text because each may contain credential material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RefreshFailure {
    pub(crate) stage: RefreshFailureStage,
    pub(crate) kind: RefreshFailureKind,
    pub(crate) status: Option<u16>,
    pub(crate) retry_after: Option<StdDuration>,
    pub(crate) send_committed: bool,
    /// True when this error was replayed from a short per-credential failure wave.
    /// Replayed callers must not repeat the leader's credential-health mutation.
    pub(crate) shared_failure_wave: bool,
}

impl RefreshFailure {
    pub(crate) fn new(
        stage: RefreshFailureStage,
        kind: RefreshFailureKind,
        status: Option<u16>,
        retry_after: Option<StdDuration>,
        send_committed: bool,
    ) -> Self {
        Self {
            stage,
            kind,
            status,
            retry_after,
            send_committed,
            shared_failure_wave: false,
        }
    }

    pub(crate) fn into_shared_failure_wave(mut self) -> Self {
        self.shared_failure_wave = true;
        self
    }
}

impl fmt::Display for RefreshFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "token refresh failed: stage={} kind={} status={} retry_after_ms={} send_committed={}",
            self.stage.as_str(),
            self.kind.as_str(),
            self.status
                .map(|status| status.to_string())
                .unwrap_or_else(|| "none".to_string()),
            self.retry_after
                .map(|retry_after| retry_after.as_millis().to_string())
                .unwrap_or_else(|| "none".to_string()),
            self.send_committed,
        )
    }
}

impl std::error::Error for RefreshFailure {}

pub(crate) struct RefreshSendAdmission {
    auxiliary_budget: Option<Arc<AuxiliaryAttemptBudget>>,
    concurrency: Arc<AuxiliaryConcurrencyController>,
    token_refresh_admission: Arc<TokenRefreshAdmissionController>,
    send_started: Arc<AtomicBool>,
}

impl RefreshSendAdmission {
    pub(crate) fn new(
        auxiliary_budget: Option<Arc<AuxiliaryAttemptBudget>>,
        concurrency: Arc<AuxiliaryConcurrencyController>,
        token_refresh_admission: Arc<TokenRefreshAdmissionController>,
    ) -> Self {
        Self::new_with_send_started_marker(
            auxiliary_budget,
            concurrency,
            token_refresh_admission,
            Arc::new(AtomicBool::new(false)),
        )
    }

    pub(crate) fn new_with_send_started_marker(
        auxiliary_budget: Option<Arc<AuxiliaryAttemptBudget>>,
        concurrency: Arc<AuxiliaryConcurrencyController>,
        token_refresh_admission: Arc<TokenRefreshAdmissionController>,
        send_started: Arc<AtomicBool>,
    ) -> Self {
        Self {
            auxiliary_budget,
            concurrency,
            token_refresh_admission,
            send_started,
        }
    }

    async fn reserve_send(&self) -> anyhow::Result<AuxiliaryConcurrencyPermit> {
        if let Some(budget) = &self.auxiliary_budget {
            budget.ensure_available(AuxiliaryAttemptKind::TokenRefresh)?;
        }
        let concurrency_permit = self
            .concurrency
            .try_acquire(AuxiliaryConcurrencyKind::TokenRefresh)?;
        self.token_refresh_admission.reserve().await?;
        if let Some(budget) = &self.auxiliary_budget {
            budget.reserve(AuxiliaryAttemptKind::TokenRefresh)?;
        }
        // The marker is set only after every admission authority has committed and immediately
        // before the HTTP send. A manager-side drop guard can then close a local failure wave if
        // the caller is cancelled while the request or response is in flight.
        self.send_started.store(true, Ordering::Release);
        Ok(concurrency_permit)
    }
}

async fn reserve_refresh_send(
    admission: Option<&RefreshSendAdmission>,
) -> anyhow::Result<Option<AuxiliaryConcurrencyPermit>> {
    match admission {
        Some(admission) => admission.reserve_send().await.map(Some),
        None => Ok(None),
    }
}

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

/// 请求热路径的 Token 刷新边界（真实到期前保留 5 分钟安全余量）。
///
/// 刷新 singleflight、跨实例 peer 接受和刷新 CAS 都必须使用这个边界。上游可能合法
/// 签发不足 10 分钟的短期 Token；若请求热路径改用更宽的预警窗口，刚刷新的 Token 会
/// 立即再次刷新并在突发流量下放大 OAuth RPM。
pub(crate) fn is_token_expired(credentials: &KiroCredentials) -> bool {
    is_token_expiring_within(credentials, 5).unwrap_or(true)
}

/// 10 分钟预警窗口，只用于状态展示或主动后台维护，不驱动请求热路径刷新。
#[cfg(test)]
pub(crate) fn is_token_expiring_soon(credentials: &KiroCredentials) -> bool {
    is_token_expiring_within(credentials, 10).unwrap_or(false)
}

/// 验证 refreshToken 的基本有效性
pub(crate) fn validate_refresh_token(credentials: &KiroCredentials) -> anyhow::Result<()> {
    let refresh_token = credentials.refresh_token.as_ref().ok_or_else(|| {
        RefreshFailure::new(
            RefreshFailureStage::RequestSend,
            RefreshFailureKind::InvalidConfiguration,
            None,
            None,
            false,
        )
    })?;

    if refresh_token.is_empty() {
        return Err(RefreshFailure::new(
            RefreshFailureStage::Validation,
            RefreshFailureKind::InvalidConfiguration,
            None,
            None,
            false,
        )
        .into());
    }

    if refresh_token.len() < 100 || refresh_token.ends_with("...") || refresh_token.contains("...")
    {
        return Err(RefreshFailure::new(
            RefreshFailureStage::Validation,
            RefreshFailureKind::InvalidConfiguration,
            None,
            None,
            false,
        )
        .into());
    }

    Ok(())
}

#[cfg(test)]
pub(super) fn is_invalid_grant_response(status: reqwest::StatusCode, body_text: &str) -> bool {
    status == reqwest::StatusCode::BAD_REQUEST
        && oauth_error_code_matches(body_text.as_bytes(), "invalid_grant")
}

fn oauth_error_code_matches(body: &[u8], expected: &str) -> bool {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .is_some_and(|code| code == expected)
}

fn retry_after_duration(headers: &HeaderMap) -> Option<StdDuration> {
    let value = headers.get("retry-after")?.to_str().ok()?.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(StdDuration::from_secs(seconds));
    }
    let retry_at = DateTime::parse_from_rfc2822(value)
        .ok()
        .map(|date| date.with_timezone(&Utc))?;
    let seconds = retry_at.signed_duration_since(Utc::now()).num_seconds();
    Some(StdDuration::from_secs(seconds.max(1) as u64))
}

fn request_error_kind(error: &reqwest::Error) -> RefreshFailureKind {
    if error.is_timeout() {
        RefreshFailureKind::Timeout
    } else if error.is_builder() {
        RefreshFailureKind::Protocol
    } else {
        RefreshFailureKind::Network
    }
}

fn refresh_failure_from_http_send(
    error: HttpSendError,
    default_stage: RefreshFailureStage,
    default_send_committed: bool,
) -> RefreshFailure {
    match error {
        HttpSendError::Request(error) => {
            let send_committed =
                default_send_committed || (!error.is_connect() && !error.is_builder());
            RefreshFailure::new(
                default_stage,
                request_error_kind(&error),
                None,
                None,
                send_committed,
            )
        }
        HttpSendError::ResponseHeaderTimeout { .. } => RefreshFailure::new(
            RefreshFailureStage::ResponseHeaders,
            RefreshFailureKind::Timeout,
            None,
            None,
            true,
        ),
        HttpSendError::ResponseBodyTimeout { .. } => RefreshFailure::new(
            RefreshFailureStage::ResponseBody,
            RefreshFailureKind::Timeout,
            None,
            None,
            true,
        ),
        HttpSendError::ResponseBodyTooLarge { .. } => RefreshFailure::new(
            RefreshFailureStage::ResponseBody,
            RefreshFailureKind::Oversize,
            None,
            None,
            true,
        ),
    }
}

async fn send_token_refresh_request(
    client: &reqwest::Client,
    request: reqwest::Request,
    controlled_test_timeout: bool,
) -> Result<reqwest::Response, RefreshFailure> {
    #[cfg(test)]
    if controlled_test_timeout {
        return match tokio::time::timeout(
            CONTROLLED_TEST_RESPONSE_HEADER_TIMEOUT,
            client.execute(request),
        )
        .await
        {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(error)) => Err(RefreshFailure::new(
                RefreshFailureStage::RequestSend,
                request_error_kind(&error),
                None,
                None,
                !error.is_connect() && !error.is_builder(),
            )),
            Err(_) => Err(RefreshFailure::new(
                RefreshFailureStage::ResponseHeaders,
                RefreshFailureKind::Timeout,
                None,
                None,
                true,
            )),
        };
    }
    #[cfg(not(test))]
    let _ = controlled_test_timeout;

    execute_with_response_header_timeout(client, request, TOKEN_SERVICE_RESPONSE_TIMEOUT_SECS)
        .await
        .map_err(|error| {
            refresh_failure_from_http_send(error, RefreshFailureStage::RequestSend, false)
        })
}

fn build_token_refresh_request(
    request: reqwest::RequestBuilder,
) -> Result<reqwest::Request, RefreshFailure> {
    request.build().map_err(|_| {
        RefreshFailure::new(
            RefreshFailureStage::Validation,
            RefreshFailureKind::Protocol,
            None,
            None,
            false,
        )
    })
}

struct TokenRefreshHttpResponse {
    status: reqwest::StatusCode,
    headers: HeaderMap,
    body: bytes::Bytes,
}

async fn read_token_refresh_response(
    response: reqwest::Response,
) -> Result<TokenRefreshHttpResponse, RefreshFailure> {
    let status = response.status();
    let headers = response.headers().clone();
    // Only a 400 response needs its body to distinguish the two OAuth codes that affect
    // credential health. All other status classes are determined from headers alone, so a
    // stalled or oversized error body cannot hide a 401/403/429/5xx classification.
    if !status.is_success() && status != reqwest::StatusCode::BAD_REQUEST {
        return Err(token_refresh_status_failure(&TokenRefreshHttpResponse {
            status,
            headers,
            body: bytes::Bytes::new(),
        }));
    }
    #[cfg(test)]
    if headers.contains_key("x-kiro-test-body-timeout") {
        let body = tokio::time::timeout(
            StdDuration::from_millis(25),
            response_bytes_with_limit_and_body_timeout(
                response,
                0,
                TOKEN_SERVICE_RESPONSE_MAX_BYTES,
            ),
        )
        .await
        .map_err(|_| {
            RefreshFailure::new(
                RefreshFailureStage::ResponseBody,
                RefreshFailureKind::Timeout,
                Some(status.as_u16()),
                None,
                true,
            )
        })?
        .map_err(|error| {
            let mut failure =
                refresh_failure_from_http_send(error, RefreshFailureStage::ResponseBody, true);
            failure.status = Some(status.as_u16());
            failure
        })?;
        return Ok(TokenRefreshHttpResponse {
            status,
            headers,
            body,
        });
    }
    let body = response_bytes_with_limit_and_body_timeout(
        response,
        TOKEN_SERVICE_RESPONSE_TIMEOUT_SECS,
        TOKEN_SERVICE_RESPONSE_MAX_BYTES,
    )
    .await
    .map_err(|error| {
        let mut failure =
            refresh_failure_from_http_send(error, RefreshFailureStage::ResponseBody, true);
        failure.status = Some(status.as_u16());
        failure
    })?;
    Ok(TokenRefreshHttpResponse {
        status,
        headers,
        body,
    })
}

fn token_refresh_status_failure(response: &TokenRefreshHttpResponse) -> RefreshFailure {
    let status = response.status;
    let kind = if status == reqwest::StatusCode::BAD_REQUEST
        && oauth_error_code_matches(&response.body, "invalid_grant")
    {
        RefreshFailureKind::InvalidGrant
    } else if oauth_error_code_matches(&response.body, "invalid_client")
        || matches!(status.as_u16(), 401 | 403)
    {
        RefreshFailureKind::CredentialAuth
    } else if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        RefreshFailureKind::RateLimited
    } else if status.is_server_error() {
        RefreshFailureKind::UpstreamUnavailable
    } else {
        RefreshFailureKind::Protocol
    };
    let retry_after = (kind == RefreshFailureKind::RateLimited)
        .then(|| retry_after_duration(&response.headers))
        .flatten();
    RefreshFailure::new(
        RefreshFailureStage::ResponseStatus,
        kind,
        Some(status.as_u16()),
        retry_after,
        true,
    )
}

fn decode_token_refresh_response<T: DeserializeOwned>(
    body: &[u8],
    access_token_field: &str,
) -> Result<T, RefreshFailure> {
    let value = serde_json::from_slice::<serde_json::Value>(body).map_err(|_| {
        RefreshFailure::new(
            RefreshFailureStage::ResponseDecode,
            RefreshFailureKind::MalformedResponse,
            Some(200),
            None,
            true,
        )
    })?;
    if !value
        .get(access_token_field)
        .and_then(serde_json::Value::as_str)
        .is_some_and(|token| !token.trim().is_empty())
    {
        return Err(RefreshFailure::new(
            RefreshFailureStage::ResponseValidate,
            RefreshFailureKind::MissingToken,
            Some(200),
            None,
            true,
        ));
    }
    serde_json::from_value(value).map_err(|_| {
        RefreshFailure::new(
            RefreshFailureStage::ResponseDecode,
            RefreshFailureKind::MalformedResponse,
            Some(200),
            None,
            true,
        )
    })
}

/// 刷新 Token
#[cfg(test)]
pub(crate) async fn refresh_token(
    credentials: &KiroCredentials,
    config: &Config,
    proxy: Option<&ProxyConfig>,
) -> anyhow::Result<KiroCredentials> {
    let client = Arc::new(build_client(proxy, 60, config.tls_backend).map_err(|_| {
        RefreshFailure::new(
            RefreshFailureStage::Internal,
            RefreshFailureKind::Internal,
            None,
            None,
            false,
        )
    })?);
    refresh_token_with_client(credentials, config, client, None).await
}

pub(crate) async fn refresh_token_with_client(
    credentials: &KiroCredentials,
    config: &Config,
    client: Arc<reqwest::Client>,
    admission: Option<RefreshSendAdmission>,
) -> anyhow::Result<KiroCredentials> {
    let mut normalized_credentials = credentials.clone();
    normalized_credentials.canonicalize_auth_method();
    normalized_credentials.normalize_external_idp_defaults();
    let credentials = &normalized_credentials;

    // API Key 凭据不支持 Token 刷新：底层契约级拦截
    // 其他调用点（try_ensure_token / 活跃路径 / add_credential）在调用前已显式分流 API Key；
    // 仅 force_refresh_token_for 未分流，此处 bail 让错误自然传播为 400 BAD_REQUEST。
    if credentials.is_api_key_credential() {
        return Err(RefreshFailure::new(
            RefreshFailureStage::Validation,
            RefreshFailureKind::InvalidConfiguration,
            None,
            None,
            false,
        )
        .into());
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

    let mut refresh_credentials = credentials.clone();
    refresh_credentials.auth_method = Some(auth_method.to_string());
    if refresh_credentials.is_external_idp_refresh_credential() {
        refresh_external_idp_token(credentials, config, &client, admission.as_ref()).await
    } else if refresh_credentials.is_idc_refresh_credential() {
        refresh_idc_token(credentials, config, &client, admission.as_ref()).await
    } else {
        refresh_social_token(credentials, config, &client, admission.as_ref()).await
    }
}

/// 刷新 Social Token
async fn refresh_social_token(
    credentials: &KiroCredentials,
    config: &Config,
    client: &reqwest::Client,
    admission: Option<&RefreshSendAdmission>,
) -> anyhow::Result<KiroCredentials> {
    tracing::info!("正在刷新 Social Token...");

    let refresh_token = credentials.refresh_token.as_ref().unwrap();
    // 优先级：凭据.auth_region > 凭据.region > config.auth_region > config.region
    let region = credentials.effective_auth_region(config);

    let refresh_url = format!("https://prod.{}.auth.desktop.kiro.dev/refreshToken", region);
    let refresh_domain = format!("prod.{}.auth.desktop.kiro.dev", region);
    let machine_id = machine_id::generate_from_credentials(credentials, config);
    let kiro_version = &config.kiro_version;

    let body = RefreshRequest {
        refresh_token: refresh_token.to_string(),
    };

    let request = client
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
        .json(&body);
    let request = build_token_refresh_request(request)?;
    let _send_permit = reserve_refresh_send(admission).await?;
    let response = send_token_refresh_request(client, request, false).await?;
    let response = read_token_refresh_response(response).await?;
    if !response.status.is_success() {
        return Err(token_refresh_status_failure(&response).into());
    }

    let data: RefreshResponse = decode_token_refresh_response(&response.body, "accessToken")?;

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

/// 刷新外部 IdP Token（Microsoft Entra ID 等 OAuth v2 token endpoint）
async fn refresh_external_idp_token(
    credentials: &KiroCredentials,
    config: &Config,
    client: &reqwest::Client,
    admission: Option<&RefreshSendAdmission>,
) -> anyhow::Result<KiroCredentials> {
    tracing::info!("正在刷新 External IdP Token...");

    let refresh_token = credentials.refresh_token.as_ref().unwrap();
    let client_id = credentials.client_id.as_ref().ok_or_else(|| {
        RefreshFailure::new(
            RefreshFailureStage::Validation,
            RefreshFailureKind::InvalidConfiguration,
            None,
            None,
            false,
        )
    })?;
    let token_endpoint = credentials.token_endpoint.as_ref().ok_or_else(|| {
        RefreshFailure::new(
            RefreshFailureStage::Validation,
            RefreshFailureKind::InvalidConfiguration,
            None,
            None,
            false,
        )
    })?;

    let mut form = vec![
        ("client_id", client_id.as_str()),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token.as_str()),
    ];
    if let Some(scopes) = credentials
        .scopes
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        form.push(("scope", scopes));
    }

    let request = client
        .post(token_endpoint)
        .header("Accept", "application/json")
        .header("User-Agent", format!("KiroIDE-{}", config.kiro_version))
        .header("Connection", "close")
        .form(&form);
    let request = build_token_refresh_request(request)?;
    let _send_permit = reserve_refresh_send(admission).await?;
    let response = send_token_refresh_request(
        client,
        request,
        uses_controlled_test_response_header_timeout(token_endpoint),
    )
    .await?;
    let response = read_token_refresh_response(response).await?;
    if !response.status.is_success() {
        return Err(token_refresh_status_failure(&response).into());
    }

    let data: ExternalIdpRefreshResponse =
        decode_token_refresh_response(&response.body, "access_token")?;

    let mut new_credentials = credentials.clone();
    new_credentials.access_token = Some(data.access_token);

    if let Some(new_refresh_token) = data.refresh_token {
        new_credentials.refresh_token = Some(new_refresh_token);
    }

    if let Some(scope) = data.scope {
        new_credentials.scopes = Some(scope);
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
    client: &reqwest::Client,
    admission: Option<&RefreshSendAdmission>,
) -> anyhow::Result<KiroCredentials> {
    tracing::info!("正在刷新 IdC Token...");

    let refresh_token = credentials.refresh_token.as_ref().unwrap();
    let client_id = credentials.client_id.as_ref().ok_or_else(|| {
        RefreshFailure::new(
            RefreshFailureStage::Validation,
            RefreshFailureKind::InvalidConfiguration,
            None,
            None,
            false,
        )
    })?;
    let client_secret = credentials.client_secret.as_ref().ok_or_else(|| {
        RefreshFailure::new(
            RefreshFailureStage::Validation,
            RefreshFailureKind::InvalidConfiguration,
            None,
            None,
            false,
        )
    })?;

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

    let body = IdcRefreshRequest {
        client_id: client_id.to_string(),
        client_secret: client_secret.to_string(),
        refresh_token: refresh_token.to_string(),
        grant_type: "refresh_token".to_string(),
    };

    let request = client
        .post(&refresh_url)
        .header("content-type", "application/json")
        .header("x-amz-user-agent", x_amz_user_agent)
        .header("user-agent", &user_agent)
        .header("host", format!("oidc.{}.amazonaws.com", region))
        .header("amz-sdk-invocation-id", uuid::Uuid::new_v4().to_string())
        .header("amz-sdk-request", "attempt=1; max=4")
        .header("Connection", "close")
        .json(&body);
    let request = build_token_refresh_request(request)?;
    let _send_permit = reserve_refresh_send(admission).await?;
    let response = send_token_refresh_request(client, request, false).await?;
    let response = read_token_refresh_response(response).await?;
    if !response.status.is_success() {
        return Err(token_refresh_status_failure(&response).into());
    }

    let data: IdcRefreshResponse = decode_token_refresh_response(&response.body, "accessToken")?;

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

    // 优先级：凭据.api_region > profileArn region > config.api_region > config.region
    let region = credentials.effective_api_region(config);
    let host = format!("q.{}.amazonaws.com", region);
    let machine_id = machine_id::generate_from_credentials(credentials, config);
    let kiro_version = &config.kiro_version;
    let os_name = &config.system_version;
    let node_version = &config.node_version;

    // 构建 URL
    let mut url = configured_upstream_url(
        config,
        "getUsageLimits?origin=AI_EDITOR&resourceType=AGENTIC_REQUEST",
    )
    .unwrap_or_else(|| {
        format!(
            "https://{}/getUsageLimits?origin=AI_EDITOR&resourceType=AGENTIC_REQUEST",
            host
        )
    });

    if let Some(profile_arn) = resolve_profile_arn(credentials, config) {
        url.push_str(&format!(
            "&profileArn={}",
            urlencoding::encode(&profile_arn)
        ));
    }

    // 构建 User-Agent headers
    let user_agent = usage_limits_user_agent(os_name, node_version, kiro_version, &machine_id);
    let amz_user_agent = usage_limits_amz_user_agent(kiro_version, &machine_id);

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
    if is_external_idp_credentials(credentials) {
        request = request.header("TokenType", "EXTERNAL_IDP");
    }

    let response =
        send_with_response_header_timeout(request, config.kiro_upstream_response_timeout_secs)
            .await?;

    let status = response.status();
    let response_body = response_bytes_with_limit_and_body_timeout(
        response,
        config.kiro_upstream_response_timeout_secs,
        TOKEN_SERVICE_RESPONSE_MAX_BYTES,
    )
    .await?;
    if !status.is_success() {
        let error_msg = match status.as_u16() {
            401 => "认证失败，Token 无效或已过期",
            403 => "权限不足，无法获取使用额度",
            429 => "请求过于频繁，已被限流",
            500..=599 => "服务器错误，AWS 服务暂时不可用",
            _ => "获取使用额度失败",
        };
        bail!("{}: {}", error_msg, status);
    }

    let data: UsageLimitsResponse = serde_json::from_slice(&response_body)?;
    Ok(data)
}

/// 设置 Kiro/AWS Q Overages 开关。
pub(crate) async fn set_overage_status(
    credentials: &KiroCredentials,
    config: &Config,
    token: &str,
    proxy: Option<&ProxyConfig>,
    enabled: bool,
) -> anyhow::Result<()> {
    let region = credentials.effective_api_region(config);
    let host = format!("q.{}.amazonaws.com", region);
    let machine_id = machine_id::generate_from_credentials(credentials, config);
    let kiro_version = &config.kiro_version;
    let os_name = &config.system_version;
    let node_version = &config.node_version;
    let status = if enabled { "ENABLED" } else { "DISABLED" };
    let profile_arn = resolve_profile_arn(credentials, config)
        .ok_or_else(|| anyhow::anyhow!("当前凭据缺少可用于设置超额的 profileArn"))?;

    let url = configured_upstream_url(config, "setUserPreference")
        .unwrap_or_else(|| format!("https://{}/setUserPreference", host));
    let user_agent = usage_limits_user_agent(os_name, node_version, kiro_version, &machine_id);
    let amz_user_agent = usage_limits_amz_user_agent(kiro_version, &machine_id);
    let payload = json!({
        "overageConfiguration": {
            "overageStatus": status,
        },
        "profileArn": profile_arn,
    });

    let client = build_client(proxy, 60, config.tls_backend)?;
    let mut request = client
        .post(&url)
        .header("accept", "application/json")
        .header("content-type", "application/json")
        .header("x-amz-user-agent", &amz_user_agent)
        .header("user-agent", &user_agent)
        .header("host", &host)
        .header("amz-sdk-invocation-id", uuid::Uuid::new_v4().to_string())
        .header("amz-sdk-request", "attempt=1; max=1")
        .header("Authorization", format!("Bearer {}", token))
        .header("Connection", "close")
        .json(&payload);

    if credentials.is_api_key_credential() {
        request = request.header("tokentype", "API_KEY");
    }
    if is_external_idp_credentials(credentials) {
        request = request.header("TokenType", "EXTERNAL_IDP");
    }

    let response =
        send_with_response_header_timeout(request, config.kiro_upstream_response_timeout_secs)
            .await?;
    let status_code = response.status();
    let _response_body = response_bytes_with_limit_and_body_timeout(
        response,
        config.kiro_upstream_response_timeout_secs,
        TOKEN_SERVICE_RESPONSE_MAX_BYTES,
    )
    .await?;
    if !status_code.is_success() {
        bail!("设置超额开关失败: {}", status_code);
    }
    Ok(())
}

pub(super) fn usage_limits_user_agent(
    os_name: &str,
    node_version: &str,
    kiro_version: &str,
    machine_id: &str,
) -> String {
    format!(
        "aws-sdk-js/1.0.0 ua/2.1 os/{} lang/js md/nodejs#{} api/codewhispererruntime#1.0.0 m/N,E KiroIDE-{}-{}",
        os_name, node_version, kiro_version, machine_id
    )
}

pub(super) fn usage_limits_amz_user_agent(kiro_version: &str, machine_id: &str) -> String {
    format!("aws-sdk-js/1.0.0 KiroIDE-{}-{}", kiro_version, machine_id)
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::fs;
    use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration as StdDuration, Instant};

    use anyhow::Context;
    use axum::extract::{Form, State};
    use axum::http::{HeaderMap, Uri};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use chrono::{Duration as ChronoDuration, Utc};
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::{
        AuxiliaryAttemptBudget, AuxiliaryAttemptKind, RefreshFailure, RefreshFailureKind,
        RefreshFailureStage, get_usage_limits, refresh_token,
    };
    use crate::kiro::model::credentials::KiroCredentials;
    use crate::kiro::token_manager::{
        AcquireMode, AutomaticTokenRecoveryOutcome, AuxiliaryConcurrencyKind, MultiTokenManager,
    };
    use crate::model::config::{Config, TlsBackend};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    #[repr(u8)]
    enum FakeOAuthScenario {
        InvalidGrant = 0,
        InvalidClient = 1,
        Forbidden = 2,
        RateLimited = 3,
        ServerError = 4,
        Timeout = 5,
        Disconnect = 6,
        MalformedSuccess = 7,
        MissingToken = 8,
        BodyTimeout = 9,
        Oversize = 10,
        ProtocolStatus = 11,
        Unauthorized = 12,
        Success = 13,
    }

    impl FakeOAuthScenario {
        const FAILURES: [Self; 12] = [
            Self::InvalidClient,
            Self::Unauthorized,
            Self::Forbidden,
            Self::RateLimited,
            Self::ServerError,
            Self::Timeout,
            Self::Disconnect,
            Self::MalformedSuccess,
            Self::MissingToken,
            Self::BodyTimeout,
            Self::Oversize,
            Self::ProtocolStatus,
        ];

        const HEALTH_NEUTRAL_FAILURES: [Self; 8] = [
            Self::ServerError,
            Self::Timeout,
            Self::Disconnect,
            Self::MalformedSuccess,
            Self::MissingToken,
            Self::BodyTimeout,
            Self::Oversize,
            Self::ProtocolStatus,
        ];

        fn from_u8(value: u8) -> Self {
            match value {
                0 => Self::InvalidGrant,
                1 => Self::InvalidClient,
                2 => Self::Forbidden,
                3 => Self::RateLimited,
                4 => Self::ServerError,
                5 => Self::Timeout,
                6 => Self::Disconnect,
                7 => Self::MalformedSuccess,
                8 => Self::MissingToken,
                9 => Self::BodyTimeout,
                10 => Self::Oversize,
                11 => Self::ProtocolStatus,
                12 => Self::Unauthorized,
                _ => Self::Success,
            }
        }

        fn label(self) -> &'static str {
            match self {
                Self::InvalidGrant => "invalid_grant",
                Self::InvalidClient => "invalid_client",
                Self::Unauthorized => "401",
                Self::Forbidden => "403",
                Self::RateLimited => "429",
                Self::ServerError => "500",
                Self::Timeout => "timeout",
                Self::Disconnect => "disconnect",
                Self::MalformedSuccess => "malformed_success",
                Self::MissingToken => "missing_token",
                Self::BodyTimeout => "body_timeout",
                Self::Oversize => "oversize",
                Self::ProtocolStatus => "protocol_status",
                Self::Success => "success",
            }
        }

        fn expected_kind(self) -> RefreshFailureKind {
            match self {
                Self::InvalidGrant => RefreshFailureKind::InvalidGrant,
                Self::InvalidClient | Self::Unauthorized | Self::Forbidden => {
                    RefreshFailureKind::CredentialAuth
                }
                Self::RateLimited => RefreshFailureKind::RateLimited,
                Self::ServerError => RefreshFailureKind::UpstreamUnavailable,
                Self::Timeout => RefreshFailureKind::Timeout,
                Self::Disconnect => RefreshFailureKind::Network,
                Self::MalformedSuccess => RefreshFailureKind::MalformedResponse,
                Self::MissingToken => RefreshFailureKind::MissingToken,
                Self::BodyTimeout => RefreshFailureKind::Timeout,
                Self::Oversize => RefreshFailureKind::Oversize,
                Self::ProtocolStatus => RefreshFailureKind::Protocol,
                Self::Success => panic!("success has no failure kind"),
            }
        }

        fn expected_health_kind(self) -> Option<&'static str> {
            match self {
                Self::InvalidClient | Self::Unauthorized | Self::Forbidden => Some("auth"),
                Self::RateLimited => Some("rate_limit"),
                _ => None,
            }
        }

        fn requires_cooldown_wait(self) -> bool {
            matches!(
                self,
                Self::InvalidClient | Self::Unauthorized | Self::Forbidden | Self::RateLimited
            )
        }
    }

    #[derive(Clone)]
    struct FakeOAuthState {
        scenario: Arc<AtomicU8>,
        hits: Arc<AtomicUsize>,
        active: Arc<AtomicUsize>,
        peak_active: Arc<AtomicUsize>,
        request_bodies: Arc<Mutex<HashSet<Vec<u8>>>>,
    }

    impl FakeOAuthState {
        fn new(scenario: FakeOAuthScenario) -> Self {
            Self {
                scenario: Arc::new(AtomicU8::new(scenario as u8)),
                hits: Arc::new(AtomicUsize::new(0)),
                active: Arc::new(AtomicUsize::new(0)),
                peak_active: Arc::new(AtomicUsize::new(0)),
                request_bodies: Arc::new(Mutex::new(HashSet::new())),
            }
        }

        fn set_scenario(&self, scenario: FakeOAuthScenario) {
            self.scenario.store(scenario as u8, Ordering::Release);
        }

        fn scenario(&self) -> FakeOAuthScenario {
            FakeOAuthScenario::from_u8(self.scenario.load(Ordering::Acquire))
        }

        fn hits(&self) -> usize {
            self.hits.load(Ordering::Acquire)
        }

        fn peak_active(&self) -> usize {
            self.peak_active.load(Ordering::Acquire)
        }

        fn active(&self) -> usize {
            self.active.load(Ordering::Acquire)
        }

        async fn wait_until_idle(&self, timeout: std::time::Duration) -> bool {
            tokio::time::timeout(timeout, async {
                while self.active() != 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                }
            })
            .await
            .is_ok()
        }

        fn distinct_request_bodies(&self) -> usize {
            self.request_bodies
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .len()
        }
    }

    struct FakeOAuthActiveGuard(Arc<AtomicUsize>);

    impl Drop for FakeOAuthActiveGuard {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::AcqRel);
        }
    }

    struct FakeOAuthServer {
        token_endpoint: String,
        state: FakeOAuthState,
        task: tokio::task::JoinHandle<()>,
    }

    impl FakeOAuthServer {
        async fn start(scenario: FakeOAuthScenario) -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind fake OAuth server");
            let address = listener.local_addr().expect("fake OAuth address");
            let state = FakeOAuthState::new(scenario);
            let server_state = state.clone();
            let task = tokio::spawn(async move {
                loop {
                    let Ok((mut socket, _)) = listener.accept().await else {
                        break;
                    };
                    let state = server_state.clone();
                    tokio::spawn(async move {
                        let Ok(body) = read_fake_oauth_request(&mut socket).await else {
                            return;
                        };
                        state.hits.fetch_add(1, Ordering::AcqRel);
                        state
                            .request_bodies
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .insert(body);
                        let active = state.active.fetch_add(1, Ordering::AcqRel) + 1;
                        state.peak_active.fetch_max(active, Ordering::AcqRel);
                        let _active_guard = FakeOAuthActiveGuard(state.active.clone());

                        match state.scenario() {
                            FakeOAuthScenario::Timeout => {
                                let mut eof_probe = [0_u8; 1];
                                tokio::select! {
                                    _ = tokio::time::sleep(std::time::Duration::from_secs(120)) => {}
                                    _ = socket.read(&mut eof_probe) => {}
                                }
                            }
                            FakeOAuthScenario::BodyTimeout => {
                                let partial = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-Kiro-Test-Body-Timeout: 1\r\nContent-Length: 128\r\nConnection: close\r\n\r\n{\"access_token\":";
                                let _ = socket.write_all(partial).await;
                                let mut eof_probe = [0_u8; 1];
                                tokio::select! {
                                    _ = tokio::time::sleep(std::time::Duration::from_secs(120)) => {}
                                    _ = socket.read(&mut eof_probe) => {}
                                }
                            }
                            FakeOAuthScenario::Disconnect => {}
                            scenario => {
                                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                                let response = match scenario {
                                    FakeOAuthScenario::InvalidGrant => fake_oauth_response(
                                        "400 Bad Request",
                                        br#"{"error":"invalid_grant","error_description":"expired"}"#,
                                    ),
                                    FakeOAuthScenario::InvalidClient => fake_oauth_response(
                                        "400 Bad Request",
                                        br#"{"error":"invalid_client"}"#,
                                    ),
                                    FakeOAuthScenario::Unauthorized => fake_oauth_response(
                                        "401 Unauthorized",
                                        br#"{"error":"unauthorized"}"#,
                                    ),
                                    FakeOAuthScenario::Forbidden => fake_oauth_response(
                                        "403 Forbidden",
                                        br#"{"error":"access_denied"}"#,
                                    ),
                                    FakeOAuthScenario::ServerError => fake_oauth_response(
                                        "500 Internal Server Error",
                                        br#"{"error":"server_error"}"#,
                                    ),
                                    FakeOAuthScenario::RateLimited => {
                                        fake_oauth_response_with_headers(
                                            "429 Too Many Requests",
                                            &["Retry-After: 1"],
                                            br#"{"error":"slow_down"}"#,
                                        )
                                    }
                                    FakeOAuthScenario::MalformedSuccess => fake_oauth_response(
                                        "200 OK",
                                        br#"{"access_token":"unterminated"#,
                                    ),
                                    FakeOAuthScenario::MissingToken => fake_oauth_response(
                                        "200 OK",
                                        br#"{"expires_in":3600,"token_type":"Bearer"}"#,
                                    ),
                                    FakeOAuthScenario::Oversize => {
                                        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 1048577\r\nConnection: close\r\n\r\n".to_vec()
                                    }
                                    FakeOAuthScenario::ProtocolStatus => fake_oauth_response(
                                        "418 I'm a teapot",
                                        br#"{"error":"unexpected_status"}"#,
                                    ),
                                    FakeOAuthScenario::Success => fake_oauth_response(
                                        "200 OK",
                                        br#"{"access_token":"recovered-access-token","expires_in":360}"#,
                                    ),
                                    FakeOAuthScenario::Timeout
                                    | FakeOAuthScenario::BodyTimeout
                                    | FakeOAuthScenario::Disconnect => unreachable!(),
                                };
                                let _ = socket.write_all(&response).await;
                            }
                        }
                    });
                }
            });
            let token_endpoint = if scenario == FakeOAuthScenario::Timeout {
                format!("http://{address}/token?kiro_test_timeout_ms=1000")
            } else {
                format!("http://{address}/token")
            };
            Self {
                token_endpoint,
                state,
                task,
            }
        }
    }

    impl Drop for FakeOAuthServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    fn fake_oauth_response(status: &str, body: &[u8]) -> Vec<u8> {
        fake_oauth_response_with_headers(status, &[], body)
    }

    fn fake_oauth_response_with_headers(status: &str, headers: &[&str], body: &[u8]) -> Vec<u8> {
        let extra_headers = if headers.is_empty() {
            String::new()
        } else {
            format!("{}\r\n", headers.join("\r\n"))
        };
        let mut response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        response
    }

    async fn read_fake_oauth_request(
        socket: &mut tokio::net::TcpStream,
    ) -> anyhow::Result<Vec<u8>> {
        let mut received = Vec::with_capacity(2 * 1024);
        let mut buffer = [0_u8; 4 * 1024];
        let header_end = loop {
            let read = socket.read(&mut buffer).await?;
            anyhow::ensure!(read > 0, "client closed before OAuth request headers");
            received.extend_from_slice(&buffer[..read]);
            anyhow::ensure!(received.len() <= 64 * 1024, "OAuth fixture header bound");
            if let Some(index) = received.windows(4).position(|part| part == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let header_text = std::str::from_utf8(&received[..header_end])?;
        let content_length = header_text
            .split("\r\n")
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .context("fake OAuth request missing Content-Length")?;
        let mut body = received.split_off(header_end);
        anyhow::ensure!(
            body.len() <= content_length,
            "OAuth body exceeds Content-Length"
        );
        while body.len() < content_length {
            let read = socket.read(&mut buffer).await?;
            anyhow::ensure!(read > 0, "client closed before OAuth request body");
            body.extend_from_slice(&buffer[..read]);
        }
        body.truncate(content_length);
        Ok(body)
    }

    fn fake_expired_external_idp_credentials(
        pool_size: usize,
        request_namespace: usize,
        token_endpoint: &str,
    ) -> Vec<KiroCredentials> {
        (1..=pool_size)
            .map(|id| KiroCredentials {
                id: Some(id as u64),
                access_token: Some(format!("expired-access-{request_namespace}-{id}")),
                refresh_token: Some(format!(
                    "refresh-{request_namespace}-{id}-{}",
                    "x".repeat(192)
                )),
                expires_at: Some((Utc::now() - ChronoDuration::hours(1)).to_rfc3339()),
                auth_method: Some("external_idp".to_string()),
                client_id: Some(format!("fake-client-{request_namespace}-{id}")),
                token_endpoint: Some(token_endpoint.to_string()),
                ..Default::default()
            })
            .collect()
    }

    fn fake_refresh_manager(
        pool_size: usize,
        request_namespace: usize,
        token_endpoint: &str,
    ) -> Arc<MultiTokenManager> {
        fake_refresh_manager_with_tls(
            pool_size,
            request_namespace,
            token_endpoint,
            TlsBackend::Rustls,
            0,
        )
    }

    fn fake_refresh_manager_with_tls(
        pool_size: usize,
        request_namespace: usize,
        token_endpoint: &str,
        tls_backend: TlsBackend,
        credential_max_concurrent_requests: u32,
    ) -> Arc<MultiTokenManager> {
        fake_refresh_manager_with_tls_and_auxiliary_limit(
            pool_size,
            request_namespace,
            token_endpoint,
            tls_backend,
            credential_max_concurrent_requests,
            16,
        )
    }

    fn fake_refresh_manager_with_tls_and_auxiliary_limit(
        pool_size: usize,
        request_namespace: usize,
        token_endpoint: &str,
        tls_backend: TlsBackend,
        credential_max_concurrent_requests: u32,
        auxiliary_max_concurrent_requests: u32,
    ) -> Arc<MultiTokenManager> {
        let defaults = Config::default();
        fake_refresh_manager_with_all_limits(
            pool_size,
            request_namespace,
            token_endpoint,
            tls_backend,
            credential_max_concurrent_requests,
            auxiliary_max_concurrent_requests,
            defaults.token_refresh_max_rpm,
            defaults.token_refresh_burst,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn fake_refresh_manager_with_all_limits(
        pool_size: usize,
        request_namespace: usize,
        token_endpoint: &str,
        tls_backend: TlsBackend,
        credential_max_concurrent_requests: u32,
        auxiliary_max_concurrent_requests: u32,
        token_refresh_max_rpm: u32,
        token_refresh_burst: u32,
    ) -> Arc<MultiTokenManager> {
        let mut config = Config::default();
        config.credential_auth_error_cooldown_secs = 1;
        config.credential_rate_limit_cooldown_secs = 1;
        config.credential_max_cooldown_secs = 2;
        config.credential_cooldown_jitter_percent = 0;
        config.credential_max_concurrent_requests = credential_max_concurrent_requests;
        config.auxiliary_upstream_max_concurrent_requests = auxiliary_max_concurrent_requests;
        config.token_refresh_max_rpm = token_refresh_max_rpm;
        config.token_refresh_burst = token_refresh_burst;
        config.tls_backend = tls_backend;
        Arc::new(
            MultiTokenManager::new(
                config,
                fake_expired_external_idp_credentials(pool_size, request_namespace, token_endpoint),
                None,
                None,
                false,
            )
            .expect("construct fake OAuth token manager"),
        )
    }

    fn fake_automatic_recovery_manager(
        pool_size: usize,
        request_namespace: usize,
        token_endpoint: &str,
    ) -> Arc<MultiTokenManager> {
        let mut credentials =
            fake_expired_external_idp_credentials(pool_size, request_namespace, token_endpoint);
        for credential in &mut credentials {
            credential.expires_at = Some((Utc::now() + ChronoDuration::hours(1)).to_rfc3339());
        }
        let mut config = Config::default();
        config.credential_auth_error_cooldown_secs = 1;
        config.credential_rate_limit_cooldown_secs = 1;
        config.credential_max_cooldown_secs = 2;
        config.credential_cooldown_jitter_percent = 0;
        config.tls_backend = TlsBackend::NativeTls;
        Arc::new(
            MultiTokenManager::new(config, credentials, None, None, false)
                .expect("construct automatic auth recovery manager"),
        )
    }

    async fn acquire_from_fake_refresh_manager(
        manager: &MultiTokenManager,
    ) -> anyhow::Result<crate::kiro::token_manager::CallContext> {
        let auxiliary_budget = Arc::new(AuxiliaryAttemptBudget::new(2));
        manager
            .acquire_context_for_session_with_mode_and_auxiliary_budget(
                None,
                None,
                &HashSet::new(),
                AcquireMode::FailFastOnCapacity,
                1,
                Some(auxiliary_budget),
            )
            .await
    }

    #[derive(Clone)]
    struct TokenEndpointState {
        captured_form: Arc<Mutex<Option<HashMap<String, String>>>>,
    }

    async fn mock_external_idp_token_endpoint(
        State(state): State<TokenEndpointState>,
        Form(form): Form<HashMap<String, String>>,
    ) -> Json<serde_json::Value> {
        *state.captured_form.lock().unwrap() = Some(form);
        Json(json!({
            "access_token": "new-access-token",
            "refresh_token": "new-refresh-token",
            "expires_in": 3600,
            "scope": "offline_access codewhisperer:conversations"
        }))
    }

    #[derive(Clone)]
    struct UsageEndpointState {
        captured: Arc<Mutex<Option<(String, String, String, String)>>>,
    }

    async fn mock_usage_limits_endpoint(
        State(state): State<UsageEndpointState>,
        headers: HeaderMap,
        uri: Uri,
    ) -> Json<serde_json::Value> {
        let captured = (
            headers
                .get("host")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string(),
            headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string(),
            headers
                .get("tokentype")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string(),
            uri.to_string(),
        );
        *state.captured.lock().unwrap() = Some(captured);
        Json(json!({
            "subscriptionInfo": {
                "subscriptionTitle": "KIRO TEST",
                "overageCapability": "OVERAGE_CAPABLE"
            },
            "usageBreakdownList": [{
                "currentUsageWithPrecision": 1.0,
                "usageLimitWithPrecision": 100.0
            }]
        }))
    }

    #[tokio::test]
    async fn api_key_usage_limits_honors_transport_override_and_region_headers() {
        let captured = Arc::new(Mutex::new(None));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/fake/getUsageLimits", get(mock_usage_limits_endpoint))
            .with_state(UsageEndpointState {
                captured: captured.clone(),
            });
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let credentials = KiroCredentials {
            auth_method: Some("api_key".to_string()),
            kiro_api_key: Some("ksk_fake_balance".to_string()),
            api_region: Some("ap-south-2".to_string()),
            ..Default::default()
        };
        let mut config = Config::default();
        config.kiro_upstream_base_url = Some(format!("http://{addr}/fake"));

        let usage = get_usage_limits(
            &credentials,
            &config,
            credentials.kiro_api_key.as_deref().unwrap(),
            None,
        )
        .await
        .unwrap();
        server.abort();

        assert_eq!(usage.subscription_title(), Some("KIRO TEST"));
        let (host, authorization, token_type, uri) = captured.lock().unwrap().clone().unwrap();
        assert_eq!(host, "q.ap-south-2.amazonaws.com");
        assert_eq!(authorization, "Bearer ksk_fake_balance");
        assert_eq!(token_type, "API_KEY");
        assert_eq!(
            uri,
            "/fake/getUsageLimits?origin=AI_EDITOR&resourceType=AGENTIC_REQUEST"
        );
    }

    #[tokio::test]
    async fn external_idp_refresh_uses_token_endpoint_without_client_secret() {
        let captured_form = Arc::new(Mutex::new(None));
        let state = TokenEndpointState {
            captured_form: captured_form.clone(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/token", post(mock_external_idp_token_endpoint))
            .with_state(state);
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let credentials = KiroCredentials {
            auth_method: Some("external_idp".to_string()),
            refresh_token: Some("r".repeat(150)),
            client_id: Some("client-123".to_string()),
            token_endpoint: Some(format!("http://{addr}/token")),
            scopes: Some("offline_access codewhisperer:conversations".to_string()),
            profile_arn: Some(
                "arn:aws:codewhisperer:us-east-1:123456789012:profile/REAL".to_string(),
            ),
            ..Default::default()
        };

        let refreshed = refresh_token(&credentials, &Config::default(), None)
            .await
            .unwrap();

        server.abort();

        assert_eq!(refreshed.access_token.as_deref(), Some("new-access-token"));
        assert_eq!(
            refreshed.refresh_token.as_deref(),
            Some("new-refresh-token")
        );
        assert_eq!(
            refreshed.scopes.as_deref(),
            Some("offline_access codewhisperer:conversations")
        );
        assert_eq!(refreshed.profile_arn, credentials.profile_arn);
        assert!(refreshed.expires_at.is_some());

        let form = captured_form.lock().unwrap().clone().unwrap();
        assert_eq!(
            form.get("client_id").map(String::as_str),
            Some("client-123")
        );
        assert_eq!(
            form.get("grant_type").map(String::as_str),
            Some("refresh_token")
        );
        assert_eq!(
            form.get("scope").map(String::as_str),
            Some("offline_access codewhisperer:conversations")
        );
        assert!(!form.contains_key("client_secret"));
    }

    fn fake_refresh_manager_from_credentials(
        credentials: Vec<KiroCredentials>,
        auxiliary_max_concurrent_requests: u32,
    ) -> Arc<MultiTokenManager> {
        let mut config = Config::default();
        config.credential_auth_error_cooldown_secs = 1;
        config.credential_rate_limit_cooldown_secs = 1;
        config.credential_max_cooldown_secs = 2;
        config.credential_cooldown_jitter_percent = 0;
        config.credential_max_concurrent_requests = 1;
        config.auxiliary_upstream_max_concurrent_requests = auxiliary_max_concurrent_requests;
        config.tls_backend = TlsBackend::NativeTls;
        Arc::new(
            MultiTokenManager::new(config, credentials, None, None, false)
                .expect("construct focused fake OAuth token manager"),
        )
    }

    async fn acquire_with_auxiliary_budget(
        manager: &MultiTokenManager,
        max_attempts: u32,
    ) -> (
        anyhow::Result<crate::kiro::token_manager::CallContext>,
        Arc<AuxiliaryAttemptBudget>,
    ) {
        let budget = Arc::new(AuxiliaryAttemptBudget::new(max_attempts));
        let result = manager
            .acquire_context_for_session_with_mode_and_auxiliary_budget(
                None,
                None,
                &HashSet::new(),
                AcquireMode::FailFastOnCapacity,
                1,
                Some(budget.clone()),
            )
            .await;
        (result, budget)
    }

    #[tokio::test]
    async fn oauth_refresh_builder_failure_has_zero_http_hits_zero_attempts_and_no_health_mutation()
    {
        for round in 1..=5 {
            let credentials =
                fake_expired_external_idp_credentials(1, 29_000_000 + round, "http://[::1");
            let manager = fake_refresh_manager_from_credentials(credentials, 1);
            let (result, budget) = acquire_with_auxiliary_budget(&manager, 2).await;
            assert!(
                result.is_err(),
                "round {round}: invalid endpoint must fail locally"
            );

            let attempts = budget.snapshot();
            assert_eq!(attempts.consumed, 0, "round {round}");
            assert_eq!(attempts.token_refresh_attempts, 0, "round {round}");
            assert_eq!(attempts.profile_discovery_attempts, 0, "round {round}");

            let snapshot = manager.snapshot();
            let credential = snapshot.entries.first().expect("one credential");
            assert_eq!(credential.refresh_failure_count, 0, "round {round}");
            assert!(!credential.disabled, "round {round}");
            assert!(!credential.cooled_down, "round {round}");
            assert!(credential.last_error_kind.is_none(), "round {round}");
            assert_eq!(snapshot.global_in_flight_requests, 0, "round {round}");

            let concurrency = manager.auxiliary_concurrency_snapshot();
            assert_eq!(concurrency.in_flight, 0, "round {round}");
            assert_eq!(concurrency.peak_in_flight, 0, "round {round}");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn auxiliary_focus_exhausted_refresh_budget_still_selects_a_ready_local_token_for_five_rounds()
     {
        for scenario in FakeOAuthScenario::FAILURES {
            let server = FakeOAuthServer::start(scenario).await;
            for round in 1..=5 {
                let mut credentials = fake_expired_external_idp_credentials(
                    3,
                    scenario as usize * 10_000 + round,
                    &server.token_endpoint,
                );
                credentials[2].access_token = Some(format!("ready-access-{round}"));
                credentials[2].expires_at =
                    Some((Utc::now() + ChronoDuration::hours(1)).to_rfc3339());
                let manager = fake_refresh_manager_from_credentials(credentials, 16);
                let hits_before = server.state.hits();

                let (result, budget) = acquire_with_auxiliary_budget(&manager, 2).await;
                let context = result.expect("third ready-token credential must stay local");
                assert_eq!(
                    context.id,
                    3,
                    "scenario={}, round={round}",
                    scenario.label()
                );
                assert_eq!(context.token, format!("ready-access-{round}"));
                drop(context);

                let hits = server.state.hits() - hits_before;
                assert!(hits <= 2, "scenario={}, round={round}", scenario.label());
                let auxiliary = budget.snapshot();
                assert_eq!(auxiliary.consumed, 2);
                assert_eq!(auxiliary.token_refresh_attempts, 2);
                assert_eq!(auxiliary.profile_discovery_attempts, 0);
                let snapshot = manager.snapshot();
                assert_eq!(
                    snapshot
                        .entries
                        .iter()
                        .map(|entry| entry.refresh_failure_count)
                        .sum::<u32>(),
                    0
                );
                let health_mutations = snapshot
                    .entries
                    .iter()
                    .filter(|entry| entry.last_error_kind.is_some())
                    .count();
                let expected_health_mutations =
                    usize::from(scenario.expected_health_kind().is_some()) * 2;
                assert_eq!(health_mutations, expected_health_mutations);
                if let Some(expected_kind) = scenario.expected_health_kind() {
                    assert_eq!(
                        snapshot
                            .entries
                            .iter()
                            .filter(|entry| entry.last_error_kind.as_deref() == Some(expected_kind))
                            .count(),
                        2
                    );
                }
                let ready = snapshot.entries.iter().find(|entry| entry.id == 3).unwrap();
                assert_eq!(ready.refresh_failure_count, 0);
                assert!(!ready.cooled_down);
                assert!(ready.last_error_kind.is_none());
                assert_eq!(snapshot.global_in_flight_requests, 0);
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn auxiliary_focus_all_expired_pool_returns_typed_budget_error_without_pool_fanout_for_five_rounds()
     {
        let server = FakeOAuthServer::start(FakeOAuthScenario::ServerError).await;
        for round in 1..=5 {
            let credentials = fake_expired_external_idp_credentials(
                60,
                30_000_000 + round,
                &server.token_endpoint,
            );
            let manager = fake_refresh_manager_from_credentials(credentials, 16);
            let hits_before = server.state.hits();
            let (result, budget) = acquire_with_auxiliary_budget(&manager, 2).await;
            let error = match result {
                Ok(_) => panic!("all-expired pool must exhaust refresh budget"),
                Err(error) => error,
            };
            assert!(
                error
                    .downcast_ref::<
                        crate::anthropic::inference_attempt_budget::AuxiliaryAttemptBudgetExhausted,
                    >()
                    .is_some(),
                "round {round}: {error}"
            );
            assert_eq!(server.state.hits() - hits_before, 2, "round {round}");
            assert_eq!(budget.snapshot().token_refresh_attempts, 2);
            let snapshot = manager.snapshot();
            assert_eq!(
                snapshot
                    .entries
                    .iter()
                    .map(|entry| entry.refresh_failure_count)
                    .sum::<u32>(),
                0
            );
            assert_eq!(
                snapshot
                    .entries
                    .iter()
                    .filter(|entry| entry.cooled_down)
                    .count(),
                0
            );
            assert_eq!(
                snapshot
                    .entries
                    .iter()
                    .filter(|entry| entry.disabled)
                    .count(),
                0
            );
            assert_eq!(snapshot.global_in_flight_requests, 0);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn auxiliary_focus_concurrency_saturation_is_fail_fast_and_health_neutral_for_five_rounds()
     {
        const CONCURRENCY: usize = 8;
        for round in 1..=5 {
            let server = FakeOAuthServer::start(FakeOAuthScenario::Timeout).await;
            let credentials = fake_expired_external_idp_credentials(
                20,
                40_000_000 + round,
                &server.token_endpoint,
            );
            let manager = fake_refresh_manager_from_credentials(credentials, 1);
            let barrier = Arc::new(tokio::sync::Barrier::new(CONCURRENCY));
            let mut tasks = Vec::new();
            for _ in 0..CONCURRENCY {
                let manager = manager.clone();
                let barrier = barrier.clone();
                tasks.push(tokio::spawn(async move {
                    barrier.wait().await;
                    acquire_with_auxiliary_budget(&manager, 2).await.0.map(drop)
                }));
            }

            let mut saturated = 0;
            let mut exhausted = 0;
            for task in tasks {
                let error = match task.await.expect("concurrency task joins") {
                    Ok(()) => panic!("controlled timeout burst must fail"),
                    Err(error) => error,
                };
                if error
                    .downcast_ref::<crate::kiro::token_manager::AuxiliaryConcurrencySaturated>()
                    .is_some()
                {
                    saturated += 1;
                } else if error
                    .downcast_ref::<
                        crate::anthropic::inference_attempt_budget::AuxiliaryAttemptBudgetExhausted,
                    >()
                    .is_some()
                {
                    exhausted += 1;
                } else {
                    panic!("unexpected auxiliary burst error: {error}");
                }
            }

            assert!(saturated >= CONCURRENCY - 1, "round {round}");
            assert_eq!(saturated + exhausted, CONCURRENCY, "round {round}");
            assert!(server.state.hits() <= 2, "round {round}");
            let snapshot = manager.snapshot();
            assert_eq!(
                snapshot
                    .entries
                    .iter()
                    .map(|entry| entry.refresh_failure_count)
                    .sum::<u32>(),
                0,
                "round {round}"
            );
            assert_eq!(
                snapshot
                    .entries
                    .iter()
                    .filter(|entry| entry.cooled_down)
                    .count(),
                0,
                "round {round}"
            );
            assert_eq!(snapshot.global_in_flight_requests, 0);
            let auxiliary = manager.auxiliary_concurrency_snapshot();
            assert_eq!(auxiliary.limit, 1);
            assert_eq!(auxiliary.in_flight, 0);
            assert_eq!(auxiliary.peak_in_flight, 1);
            assert!(auxiliary.rejected >= (CONCURRENCY - 1) as u64);
        }
    }

    #[tokio::test]
    async fn auxiliary_focus_preexhausted_budget_sends_nothing_and_never_takes_process_permit_for_five_rounds()
     {
        for round in 1..=5 {
            let server = FakeOAuthServer::start(FakeOAuthScenario::ServerError).await;
            let manager = fake_refresh_manager(1, 50_000_000 + round, &server.token_endpoint);
            let budget = Arc::new(AuxiliaryAttemptBudget::new(1));
            budget.reserve(AuxiliaryAttemptKind::TokenRefresh).unwrap();
            let error = match manager
                .acquire_context_for_session_with_mode_and_auxiliary_budget(
                    None,
                    None,
                    &HashSet::new(),
                    AcquireMode::FailFastOnCapacity,
                    1,
                    Some(budget),
                )
                .await
            {
                Ok(_) => panic!("pre-exhausted auxiliary budget must reject refresh"),
                Err(error) => error,
            };
            assert!(
                error
                    .downcast_ref::<
                        crate::anthropic::inference_attempt_budget::AuxiliaryAttemptBudgetExhausted,
                    >()
                    .is_some(),
                "round {round}: {error}"
            );
            assert_eq!(server.state.hits(), 0, "round {round}");
            let auxiliary = manager.auxiliary_concurrency_snapshot();
            assert_eq!(auxiliary.in_flight, 0, "round {round}");
            assert_eq!(auxiliary.peak_in_flight, 0, "round {round}");
            let snapshot = manager.snapshot();
            let entry = &snapshot.entries[0];
            assert_eq!(entry.refresh_failure_count, 0, "round {round}");
            assert!(!entry.cooled_down, "round {round}");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn auxiliary_focus_cancelled_refresh_releases_process_permit_for_five_rounds() {
        for round in 1..=5 {
            let server = FakeOAuthServer::start(FakeOAuthScenario::Timeout).await;
            let manager = fake_refresh_manager_with_tls_and_auxiliary_limit(
                1,
                60_000_000 + round,
                &server.token_endpoint,
                TlsBackend::NativeTls,
                1,
                1,
            );
            let running_manager = manager.clone();
            let task =
                tokio::spawn(
                    async move { acquire_with_auxiliary_budget(&running_manager, 2).await.0 },
                );
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while manager.auxiliary_concurrency_snapshot().in_flight == 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("refresh must acquire the process permit");
            task.abort();
            let _ = task.await;
            assert_eq!(
                manager.auxiliary_concurrency_snapshot().in_flight,
                0,
                "round {round}"
            );
            let controller = manager.auxiliary_concurrency_controller();
            let recovered = controller
                .try_acquire(AuxiliaryConcurrencyKind::TokenRefresh)
                .expect("cancelled refresh must release its permit");
            drop(recovered);
            assert_eq!(controller.snapshot().in_flight, 0, "round {round}");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn auxiliary_focus_external_credential_refresh_uses_shared_controller_for_five_rounds() {
        const CONCURRENCY: usize = 8;
        for round in 1..=5 {
            let server = FakeOAuthServer::start(FakeOAuthScenario::Timeout).await;
            let manager = fake_refresh_manager_with_tls_and_auxiliary_limit(
                1,
                70_000_000 + round,
                &server.token_endpoint,
                TlsBackend::NativeTls,
                1,
                1,
            );
            let credential = fake_expired_external_idp_credentials(
                1,
                71_000_000 + round,
                &server.token_endpoint,
            )
            .remove(0);
            let barrier = Arc::new(tokio::sync::Barrier::new(CONCURRENCY));
            let tasks = (0..CONCURRENCY)
                .map(|_| {
                    let manager = manager.clone();
                    let credential = credential.clone();
                    let barrier = barrier.clone();
                    tokio::spawn(async move {
                        barrier.wait().await;
                        manager
                            .acquire_context_for_external_credentials(credential)
                            .await
                    })
                })
                .collect::<Vec<_>>();
            let mut saturated = 0;
            for task in tasks {
                let error = match task.await.expect("external credential task joins") {
                    Ok(_) => panic!("controlled external refresh must fail"),
                    Err(error) => error,
                };
                if error
                    .downcast_ref::<crate::kiro::token_manager::AuxiliaryConcurrencySaturated>()
                    .is_some()
                {
                    saturated += 1;
                }
            }
            assert!(saturated >= CONCURRENCY - 1, "round {round}");
            assert_eq!(server.state.hits(), 1, "round {round}");
            let auxiliary = manager.auxiliary_concurrency_snapshot();
            assert_eq!(auxiliary.peak_in_flight, 1, "round {round}");
            assert_eq!(auxiliary.in_flight, 0, "round {round}");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn oauth_refresh_failures_recover_naturally_in_same_manager_for_five_rounds() {
        for scenario in FakeOAuthScenario::FAILURES {
            let server = FakeOAuthServer::start(scenario).await;

            for round in 1..=5 {
                // Each round starts from an explicitly expired credential. Reusing the prior
                // round's six-minute recovery token would test the old ten-minute refresh loop,
                // not failure recovery in the same manager instance.
                let manager = fake_refresh_manager(
                    1,
                    scenario as usize * 10_000 + round,
                    &server.token_endpoint,
                );
                server.state.set_scenario(scenario);
                let hits_before = server.state.hits();
                let error = match acquire_from_fake_refresh_manager(&manager).await {
                    Ok(_) => panic!("controlled fake OAuth failure must fail acquisition"),
                    Err(error) => error,
                };
                assert_eq!(
                    server.state.hits() - hits_before,
                    1,
                    "scenario={}, round={round}",
                    scenario.label()
                );

                if FakeOAuthScenario::HEALTH_NEUTRAL_FAILURES.contains(&scenario) {
                    let typed = error
                        .downcast_ref::<RefreshFailure>()
                        .expect("health-neutral refresh failures remain typed");
                    assert_eq!(typed.kind, scenario.expected_kind());
                    assert!(typed.send_committed);
                    assert!(!error.to_string().contains("server_error"));
                    assert!(!error.to_string().contains("recovered-access-token"));
                }

                let failed = manager.snapshot();
                let entry = &failed.entries[0];
                assert_eq!(entry.refresh_failure_count, 0, "round {round}");
                assert!(!entry.disabled, "round {round}");
                assert_eq!(
                    entry.last_error_kind.as_deref(),
                    scenario.expected_health_kind(),
                    "scenario={}, round={round}",
                    scenario.label()
                );
                assert_eq!(
                    entry.cooled_down,
                    scenario.requires_cooldown_wait(),
                    "scenario={}, round={round}",
                    scenario.label()
                );
                assert_eq!(failed.global_in_flight_requests, 0);

                server.state.set_scenario(FakeOAuthScenario::Success);
                if scenario.requires_cooldown_wait() {
                    let retry_after = manager.cooldown_retry_after_hint_secs(1);
                    tokio::time::sleep(StdDuration::from_millis(
                        retry_after.saturating_mul(1_000).saturating_add(100),
                    ))
                    .await;
                } else {
                    // Health-neutral failures use a short per-credential negative result instead
                    // of mutating scheduler cooldown. Let that bounded first-wave window expire
                    // before proving natural recovery in the same manager.
                    tokio::time::sleep(StdDuration::from_millis(550)).await;
                }
                let recovery_hits_before = server.state.hits();
                let recovered = acquire_from_fake_refresh_manager(&manager)
                    .await
                    .expect("same manager must recover without an operator reset");
                assert_eq!(recovered.id, 1);
                drop(recovered);
                assert_eq!(server.state.hits() - recovery_hits_before, 1);
                let recovered = manager.snapshot();
                assert_eq!(recovered.entries[0].refresh_failure_count, 0);
                assert!(!recovered.entries[0].disabled);
                assert!(!recovered.entries[0].cooled_down);
                assert_eq!(recovered.global_in_flight_requests, 0);
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn oauth_refresh_transient_failure_wave_singleflights_32_waiters_for_five_rounds() {
        const CONCURRENCY: usize = 32;
        let scenarios = [
            FakeOAuthScenario::ServerError,
            FakeOAuthScenario::Timeout,
            FakeOAuthScenario::Disconnect,
            FakeOAuthScenario::MalformedSuccess,
        ];

        for scenario in scenarios {
            for round in 1..=5 {
                let server = FakeOAuthServer::start(scenario).await;
                let manager = fake_refresh_manager_with_tls(
                    1,
                    81_000_000 + scenario as usize * 100 + round,
                    &server.token_endpoint,
                    TlsBackend::NativeTls,
                    0,
                );
                let barrier = Arc::new(tokio::sync::Barrier::new(CONCURRENCY));
                let mut tasks = Vec::with_capacity(CONCURRENCY);
                for _ in 0..CONCURRENCY {
                    let manager = manager.clone();
                    let barrier = barrier.clone();
                    tasks.push(tokio::spawn(async move {
                        let budget = Arc::new(AuxiliaryAttemptBudget::new(2));
                        barrier.wait().await;
                        let result = manager
                            .acquire_context_for_session_with_mode_and_auxiliary_budget(
                                None,
                                None,
                                &HashSet::new(),
                                AcquireMode::FailFastOnCapacity,
                                1,
                                Some(budget.clone()),
                            )
                            .await;
                        (result, budget.snapshot())
                    }));
                }

                let mut replayed = 0;
                let mut consumed = 0;
                let mut token_refresh_attempts = 0;
                for task in tasks {
                    let (result, budget) = task.await.expect("failure-wave task joins");
                    let error = match result {
                        Ok(_) => panic!("controlled OAuth failure must be shared"),
                        Err(error) => error,
                    };
                    let failure = error
                        .downcast_ref::<RefreshFailure>()
                        .expect("leader and followers retain the typed refresh error");
                    assert_eq!(failure.kind, scenario.expected_kind(), "round {round}");
                    replayed += usize::from(failure.shared_failure_wave);
                    consumed += budget.consumed as usize;
                    token_refresh_attempts += budget.token_refresh_attempts as usize;
                }

                assert_eq!(
                    server.state.hits(),
                    1,
                    "scenario={scenario:?}, round={round}"
                );
                assert_eq!(
                    server.state.distinct_request_bodies(),
                    1,
                    "scenario={scenario:?}, round={round}"
                );
                assert_eq!(
                    replayed,
                    CONCURRENCY - 1,
                    "scenario={scenario:?}, round={round}: exactly one leader owns the failure"
                );
                assert_eq!(
                    consumed, 1,
                    "scenario={scenario:?}, round={round}: followers consume no auxiliary budget"
                );
                assert_eq!(
                    token_refresh_attempts, 1,
                    "scenario={scenario:?}, round={round}"
                );
                assert!(
                    server
                        .state
                        .wait_until_idle(std::time::Duration::from_secs(2))
                        .await,
                    "scenario={scenario:?}, round={round}: OAuth connection did not close"
                );
                assert_eq!(
                    server.state.active(),
                    0,
                    "scenario={scenario:?}, round={round}"
                );
                assert_eq!(
                    manager.auxiliary_concurrency_snapshot().in_flight,
                    0,
                    "scenario={scenario:?}, round={round}"
                );
                let snapshot = manager.snapshot();
                assert_eq!(snapshot.global_in_flight_requests, 0);
                assert_eq!(snapshot.entries[0].refresh_failure_count, 0);
                assert!(!snapshot.entries[0].cooled_down);
                assert!(!snapshot.entries[0].disabled);
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn automatic_auth_recovery_success_singleflights_32_callers_for_five_rounds() {
        const CONCURRENCY: usize = 32;

        for round in 1..=5 {
            let server = FakeOAuthServer::start(FakeOAuthScenario::Success).await;
            let manager =
                fake_automatic_recovery_manager(1, 83_000_000 + round, &server.token_endpoint);
            let expected = manager
                .snapshot()
                .entries
                .first()
                .expect("automatic recovery credential")
                .id;
            assert_eq!(expected, 1);
            let expected_token = format!("expired-access-{}-1", 83_000_000 + round);
            let barrier = Arc::new(tokio::sync::Barrier::new(CONCURRENCY));
            let mut tasks = Vec::with_capacity(CONCURRENCY);
            for _ in 0..CONCURRENCY {
                let manager = manager.clone();
                let barrier = barrier.clone();
                let expected_token = expected_token.clone();
                tasks.push(tokio::spawn(async move {
                    let budget = Arc::new(AuxiliaryAttemptBudget::new(2));
                    barrier.wait().await;
                    let result = manager
                        .recover_invalid_access_token_for(1, &expected_token, 0, budget.clone())
                        .await;
                    (result, budget.snapshot())
                }));
            }

            let mut refreshed = 0;
            let mut changed = 0;
            let mut consumed = 0;
            for task in tasks {
                let (outcome, budget) = task.await.expect("automatic recovery task joins");
                match outcome.expect("automatic recovery succeeds") {
                    AutomaticTokenRecoveryOutcome::Refreshed => refreshed += 1,
                    AutomaticTokenRecoveryOutcome::CredentialChanged => changed += 1,
                }
                consumed += budget.consumed;
            }
            assert_eq!(refreshed, 1, "round {round}");
            assert_eq!(changed, CONCURRENCY - 1, "round {round}");
            assert_eq!(consumed, 1, "round {round}");
            assert_eq!(server.state.hits(), 1, "round {round}");
            assert_eq!(server.state.distinct_request_bodies(), 1, "round {round}");
            let snapshot = manager.snapshot();
            assert_eq!(snapshot.entries[0].failure_count, 0, "round {round}");
            assert_eq!(
                snapshot.entries[0].refresh_failure_count, 0,
                "round {round}"
            );
            assert!(!snapshot.entries[0].cooled_down, "round {round}");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn automatic_auth_recovery_failure_singleflights_and_stays_private_for_five_rounds() {
        const CONCURRENCY: usize = 32;

        for round in 1..=5 {
            let server = FakeOAuthServer::start(FakeOAuthScenario::ServerError).await;
            let namespace = 84_000_000 + round;
            let manager = fake_automatic_recovery_manager(1, namespace, &server.token_endpoint);
            let expected_token = format!("expired-access-{namespace}-1");
            let barrier = Arc::new(tokio::sync::Barrier::new(CONCURRENCY));
            let mut tasks = Vec::with_capacity(CONCURRENCY);
            for _ in 0..CONCURRENCY {
                let manager = manager.clone();
                let barrier = barrier.clone();
                let expected_token = expected_token.clone();
                tasks.push(tokio::spawn(async move {
                    let budget = Arc::new(AuxiliaryAttemptBudget::new(2));
                    barrier.wait().await;
                    let error = manager
                        .recover_invalid_access_token_for(1, &expected_token, 0, budget.clone())
                        .await
                        .expect_err("controlled automatic recovery must fail");
                    (error, budget.snapshot())
                }));
            }

            let mut leaders = 0;
            let mut followers = 0;
            let mut consumed = 0;
            for task in tasks {
                let (error, budget) = task.await.expect("automatic failure task joins");
                let failure = error
                    .downcast_ref::<RefreshFailure>()
                    .expect("automatic failure remains typed");
                assert_eq!(failure.kind, RefreshFailureKind::UpstreamUnavailable);
                if failure.shared_failure_wave {
                    followers += 1;
                } else {
                    leaders += 1;
                }
                let display = error.to_string();
                let debug = format!("{error:?}");
                assert!(!display.contains(&expected_token));
                assert!(!debug.contains(&expected_token));
                consumed += budget.consumed;
            }
            assert_eq!(leaders, 1, "round {round}");
            assert_eq!(followers, CONCURRENCY - 1, "round {round}");
            assert_eq!(consumed, 1, "round {round}");
            assert_eq!(server.state.hits(), 1, "round {round}");
            let snapshot = manager.snapshot();
            assert_eq!(snapshot.entries[0].failure_count, 0, "round {round}");
            assert_eq!(
                snapshot.entries[0].refresh_failure_count, 0,
                "round {round}"
            );
            assert!(!snapshot.entries[0].cooled_down, "round {round}");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn automatic_auth_recovery_zero_remaining_budget_sends_nothing_and_is_health_neutral() {
        for round in 1..=5 {
            let server = FakeOAuthServer::start(FakeOAuthScenario::Success).await;
            let namespace = 85_000_000 + round;
            let manager = fake_automatic_recovery_manager(1, namespace, &server.token_endpoint);
            let expected_token = format!("expired-access-{namespace}-1");
            let budget = Arc::new(AuxiliaryAttemptBudget::new(1));
            budget
                .reserve(AuxiliaryAttemptKind::ProfileDiscovery)
                .expect("consume the only auxiliary send");

            let error = manager
                .recover_invalid_access_token_for(1, &expected_token, 0, budget.clone())
                .await
                .expect_err("zero remaining budget rejects recovery");
            assert!(
                error
                    .downcast_ref::<crate::anthropic::inference_attempt_budget::AuxiliaryAttemptBudgetExhausted>()
                    .is_some(),
                "round {round}"
            );
            assert_eq!(server.state.hits(), 0, "round {round}");
            assert_eq!(budget.snapshot().token_refresh_attempts, 0, "round {round}");
            let snapshot = manager.snapshot();
            assert_eq!(snapshot.entries[0].failure_count, 0, "round {round}");
            assert_eq!(
                snapshot.entries[0].refresh_failure_count, 0,
                "round {round}"
            );
            assert!(!snapshot.entries[0].cooled_down, "round {round}");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn automatic_auth_recovery_changed_token_is_zero_send_and_revision_regression_fails_closed()
     {
        for round in 1..=5 {
            let server = FakeOAuthServer::start(FakeOAuthScenario::Success).await;
            let namespace = 85_500_000 + round;
            let manager = fake_automatic_recovery_manager(1, namespace, &server.token_endpoint);
            let expected_token = format!("expired-access-{namespace}-1");

            let changed_budget = Arc::new(AuxiliaryAttemptBudget::new(1));
            let outcome = manager
                .recover_invalid_access_token_for(1, "different-token", 0, changed_budget.clone())
                .await
                .expect("a replaced access token supersedes the failed request");
            assert_eq!(
                outcome,
                AutomaticTokenRecoveryOutcome::CredentialChanged,
                "round {round}"
            );
            assert_eq!(changed_budget.snapshot().consumed, 0, "round {round}");

            let regression_budget = Arc::new(AuxiliaryAttemptBudget::new(1));
            let error = manager
                .recover_invalid_access_token_for(1, &expected_token, 1, regression_budget.clone())
                .await
                .expect_err("a local revision behind the failed request must fail closed");
            let failure = error
                .downcast_ref::<RefreshFailure>()
                .expect("revision regression returns a typed refresh failure");
            assert_eq!(
                failure.stage,
                RefreshFailureStage::Coordination,
                "round {round}"
            );
            assert_eq!(
                failure.kind,
                RefreshFailureKind::Coordination,
                "round {round}"
            );
            assert!(!failure.send_committed, "round {round}");
            assert_eq!(regression_budget.snapshot().consumed, 0, "round {round}");
            assert_eq!(server.state.hits(), 0, "round {round}");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn automatic_auth_recovery_isolated_per_credential_for_five_rounds() {
        const PER_CREDENTIAL_CONCURRENCY: usize = 16;

        for round in 1..=5 {
            let server = FakeOAuthServer::start(FakeOAuthScenario::Success).await;
            let namespace = 86_000_000 + round;
            let manager = fake_automatic_recovery_manager(2, namespace, &server.token_endpoint);
            let barrier = Arc::new(tokio::sync::Barrier::new(PER_CREDENTIAL_CONCURRENCY * 2));
            let mut tasks = Vec::with_capacity(PER_CREDENTIAL_CONCURRENCY * 2);
            for id in [1_u64, 2] {
                for _ in 0..PER_CREDENTIAL_CONCURRENCY {
                    let manager = manager.clone();
                    let barrier = barrier.clone();
                    let expected_token = format!("expired-access-{namespace}-{id}");
                    tasks.push(tokio::spawn(async move {
                        let budget = Arc::new(AuxiliaryAttemptBudget::new(2));
                        barrier.wait().await;
                        manager
                            .recover_invalid_access_token_for(id, &expected_token, 0, budget)
                            .await
                    }));
                }
            }
            for task in tasks {
                task.await
                    .expect("per-credential recovery task joins")
                    .expect("per-credential recovery succeeds");
            }
            assert_eq!(server.state.hits(), 2, "round {round}");
            assert_eq!(server.state.distinct_request_bodies(), 2, "round {round}");
            assert!(server.state.peak_active() >= 2, "round {round}");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn oauth_refresh_auth_and_retry_after_wave_mutate_health_once_for_five_rounds() {
        const CONCURRENCY: usize = 32;

        for scenario in [
            FakeOAuthScenario::InvalidClient,
            FakeOAuthScenario::RateLimited,
        ] {
            for round in 1..=5 {
                let server = FakeOAuthServer::start(scenario).await;
                let manager = fake_refresh_manager_with_tls(
                    1,
                    82_000_000 + scenario as usize * 100 + round,
                    &server.token_endpoint,
                    TlsBackend::NativeTls,
                    0,
                );
                let barrier = Arc::new(tokio::sync::Barrier::new(CONCURRENCY));
                let mut tasks = Vec::with_capacity(CONCURRENCY);
                for _ in 0..CONCURRENCY {
                    let manager = manager.clone();
                    let barrier = barrier.clone();
                    tasks.push(tokio::spawn(async move {
                        barrier.wait().await;
                        acquire_from_fake_refresh_manager(&manager).await.map(drop)
                    }));
                }
                for task in tasks {
                    assert!(
                        task.await.expect("health-wave task joins").is_err(),
                        "controlled auth/rate-limit refresh must fail"
                    );
                }

                assert_eq!(
                    server.state.hits(),
                    1,
                    "scenario={scenario:?}, round={round}"
                );
                let snapshot = manager.snapshot();
                let entry = &snapshot.entries[0];
                assert_eq!(
                    entry.transient_failure_streak, 1,
                    "scenario={scenario:?}, round={round}: followers must not amplify health"
                );
                assert_eq!(
                    entry.last_error_kind.as_deref(),
                    scenario.expected_health_kind(),
                    "scenario={scenario:?}, round={round}"
                );
                assert!(entry.cooled_down, "scenario={scenario:?}, round={round}");
                assert!(!entry.disabled, "scenario={scenario:?}, round={round}");
                assert_eq!(snapshot.global_in_flight_requests, 0);
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn oauth_invalid_grant_is_the_only_fake_failure_that_permanently_disables() {
        for round in 1..=5 {
            let server = FakeOAuthServer::start(FakeOAuthScenario::InvalidGrant).await;
            let manager = fake_refresh_manager(1, 80_000_000 + round, &server.token_endpoint);
            let error = match acquire_from_fake_refresh_manager(&manager).await {
                Ok(_) => panic!("invalid_grant must fail acquisition"),
                Err(error) => error,
            };
            assert!(!error.to_string().contains("expired"));
            assert_eq!(server.state.hits(), 1, "round {round}");
            let failed = manager.snapshot();
            assert!(failed.entries[0].disabled, "round {round}");
            assert_eq!(failed.entries[0].refresh_failure_count, 0, "round {round}");

            server.state.set_scenario(FakeOAuthScenario::Success);
            assert!(acquire_from_fake_refresh_manager(&manager).await.is_err());
            assert_eq!(
                server.state.hits(),
                1,
                "permanently disabled invalid_grant credential must not be retried"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn oauth_refresh_failures_expose_only_low_cardinality_typed_metadata() {
        let scenarios = [
            FakeOAuthScenario::InvalidGrant,
            FakeOAuthScenario::InvalidClient,
            FakeOAuthScenario::Unauthorized,
            FakeOAuthScenario::Forbidden,
            FakeOAuthScenario::RateLimited,
            FakeOAuthScenario::ServerError,
            FakeOAuthScenario::Timeout,
            FakeOAuthScenario::Disconnect,
            FakeOAuthScenario::MalformedSuccess,
            FakeOAuthScenario::MissingToken,
            FakeOAuthScenario::BodyTimeout,
            FakeOAuthScenario::Oversize,
            FakeOAuthScenario::ProtocolStatus,
        ];
        for scenario in scenarios {
            let server = FakeOAuthServer::start(scenario).await;
            let credentials = fake_expired_external_idp_credentials(
                1,
                81_000_000 + scenario as usize,
                &server.token_endpoint,
            )
            .remove(0);
            let error = refresh_token(&credentials, &Config::default(), None)
                .await
                .unwrap_err();
            let typed = error
                .downcast_ref::<RefreshFailure>()
                .expect("all fake token-service failures must remain typed");
            assert_eq!(typed.kind, scenario.expected_kind(), "{scenario:?}");
            assert_eq!(
                typed.stage,
                match scenario {
                    FakeOAuthScenario::InvalidGrant
                    | FakeOAuthScenario::InvalidClient
                    | FakeOAuthScenario::Unauthorized
                    | FakeOAuthScenario::Forbidden
                    | FakeOAuthScenario::RateLimited
                    | FakeOAuthScenario::ServerError
                    | FakeOAuthScenario::ProtocolStatus => RefreshFailureStage::ResponseStatus,
                    FakeOAuthScenario::Timeout => RefreshFailureStage::ResponseHeaders,
                    FakeOAuthScenario::Disconnect => RefreshFailureStage::RequestSend,
                    FakeOAuthScenario::MalformedSuccess => RefreshFailureStage::ResponseDecode,
                    FakeOAuthScenario::MissingToken => RefreshFailureStage::ResponseValidate,
                    FakeOAuthScenario::BodyTimeout | FakeOAuthScenario::Oversize => {
                        RefreshFailureStage::ResponseBody
                    }
                    FakeOAuthScenario::Success => unreachable!(),
                },
                "{scenario:?}"
            );
            assert!(typed.send_committed, "{scenario:?}");
            assert_eq!(
                typed.status,
                match scenario {
                    FakeOAuthScenario::InvalidGrant | FakeOAuthScenario::InvalidClient => Some(400),
                    FakeOAuthScenario::Unauthorized => Some(401),
                    FakeOAuthScenario::Forbidden => Some(403),
                    FakeOAuthScenario::RateLimited => Some(429),
                    FakeOAuthScenario::ServerError => Some(500),
                    FakeOAuthScenario::MalformedSuccess
                    | FakeOAuthScenario::MissingToken
                    | FakeOAuthScenario::BodyTimeout
                    | FakeOAuthScenario::Oversize => Some(200),
                    FakeOAuthScenario::ProtocolStatus => Some(418),
                    FakeOAuthScenario::Timeout | FakeOAuthScenario::Disconnect => None,
                    FakeOAuthScenario::Success => unreachable!(),
                }
            );
            assert_eq!(
                typed.retry_after,
                (scenario == FakeOAuthScenario::RateLimited).then_some(StdDuration::from_secs(1))
            );
            let public = error.to_string();
            for forbidden in [
                "invalid_client",
                "server_error",
                "slow_down",
                "recovered-access-token",
                credentials.refresh_token.as_deref().unwrap(),
            ] {
                assert!(!public.contains(forbidden), "{scenario:?}: {public}");
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn oauth_refresh_independent_manager_burst_attempts_are_request_bounded() {
        const POOL_SIZE: usize = 20;
        const EXPECTED_REQUEST_AUXILIARY_MAX_ATTEMPTS: usize = 2;
        const RECOVERY_CONCURRENCY: usize = 16;
        let mut violations = Vec::new();

        for scenario in FakeOAuthScenario::FAILURES {
            for concurrency in [1_usize, 8, 32] {
                for round in 1..=5 {
                    let server = FakeOAuthServer::start(scenario).await;
                    let mut tasks = Vec::with_capacity(concurrency);
                    for request_index in 0..concurrency {
                        let namespace = scenario as usize * 1_000_000
                            + concurrency * 10_000
                            + round * 100
                            + request_index;
                        let manager = fake_refresh_manager_with_tls(
                            POOL_SIZE,
                            namespace,
                            &server.token_endpoint,
                            TlsBackend::NativeTls,
                            0,
                        );
                        tasks.push(tokio::spawn(async move {
                            let result = acquire_from_fake_refresh_manager(&manager).await;
                            let snapshot = manager.snapshot();
                            (result.map(drop), snapshot)
                        }));
                    }

                    let started_at = Instant::now();
                    let mut cooled = 0_usize;
                    let mut disabled = 0_usize;
                    let mut refresh_failures = 0_usize;
                    let mut auth_health_failures = 0_usize;
                    for task in tasks {
                        let (result, snapshot) = task.await.expect("burst request task joins");
                        assert!(result.is_err(), "controlled OAuth burst must fail");
                        assert_eq!(snapshot.global_in_flight_requests, 0);
                        cooled += snapshot
                            .entries
                            .iter()
                            .filter(|entry| entry.cooled_down)
                            .count();
                        disabled += snapshot
                            .entries
                            .iter()
                            .filter(|entry| entry.disabled)
                            .count();
                        refresh_failures += snapshot
                            .entries
                            .iter()
                            .map(|entry| entry.refresh_failure_count as usize)
                            .sum::<usize>();
                        auth_health_failures += snapshot
                            .entries
                            .iter()
                            .filter(|entry| entry.last_error_kind.as_deref() == Some("auth"))
                            .count();
                    }
                    let elapsed_ms = started_at.elapsed().as_millis();
                    let hits = server.state.hits();
                    let peak_active = server.state.peak_active();
                    let expected_send_cap = concurrency * EXPECTED_REQUEST_AUXILIARY_MAX_ATTEMPTS;

                    eprintln!(
                        "OAUTH_REFRESH_RED_BURST scenario={} concurrency={} pool={} round={} hits={} distinct={} peak_active={} cooled={} disabled={} refresh_failures={} auth_health={} elapsed_ms={}",
                        scenario.label(),
                        concurrency,
                        POOL_SIZE,
                        round,
                        hits,
                        server.state.distinct_request_bodies(),
                        peak_active,
                        cooled,
                        disabled,
                        refresh_failures,
                        auth_health_failures,
                        elapsed_ms
                    );

                    assert!(server.state.distinct_request_bodies() <= hits);
                    assert_eq!(refresh_failures, 0);
                    let health_mutating = scenario.expected_health_kind().is_some();
                    assert_eq!(cooled, if health_mutating { hits } else { 0 });
                    assert_eq!(
                        auth_health_failures,
                        if scenario.expected_health_kind() == Some("auth") {
                            hits
                        } else {
                            0
                        }
                    );
                    assert_eq!(disabled, 0);
                    if hits > expected_send_cap {
                        violations.push(format!(
                            "scenario={} concurrency={} round={} attempted={} server_hits={} expected<={}",
                            scenario.label(),
                            concurrency,
                            round,
                            hits,
                            hits,
                            expected_send_cap
                        ));
                    }
                    let recovery_server = FakeOAuthServer::start(FakeOAuthScenario::Success).await;
                    let recovery_hits_before = recovery_server.state.hits();
                    let mut recovery_tasks = Vec::new();
                    for recovery_index in 0..RECOVERY_CONCURRENCY {
                        let manager = fake_refresh_manager_with_tls(
                            1,
                            9_000_000 + round * 100 + recovery_index,
                            &recovery_server.token_endpoint,
                            TlsBackend::NativeTls,
                            0,
                        );
                        recovery_tasks.push(tokio::spawn(async move {
                            acquire_from_fake_refresh_manager(&manager).await.map(drop)
                        }));
                    }
                    for task in recovery_tasks {
                        task.await
                            .expect("recovery task joins")
                            .expect("all auxiliary permits must recover after errors");
                    }
                    assert_eq!(
                        recovery_server.state.hits() - recovery_hits_before,
                        RECOVERY_CONCURRENCY
                    );
                    assert_eq!(recovery_server.state.active(), 0);
                }
            }
        }

        assert!(
            violations.is_empty(),
            "auxiliary burst budget violations:\n{}",
            violations.join("\n")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn oauth_refresh_shared_manager_burst_has_process_concurrency_cap() {
        // Keep untouched credentials available for the immediate controller-recovery probe.
        // The dedicated single-credential test above owns cooldown-expiry recovery semantics.
        const POOL_SIZE: usize = 128;
        const EXPECTED_REQUEST_AUXILIARY_MAX_ATTEMPTS: usize = 2;
        const EXPECTED_PROCESS_AUXILIARY_MAX_CONCURRENCY: usize = 16;
        let mut violations = Vec::new();

        for scenario in FakeOAuthScenario::FAILURES {
            for concurrency in [1_usize, 8, 32] {
                for round in 1..=5 {
                    let server = FakeOAuthServer::start(scenario).await;
                    // This matrix isolates the process concurrency controller. The independent
                    // 60 RPM / burst-8 token-bucket test owns rate-admission behavior.
                    let manager = fake_refresh_manager_with_all_limits(
                        POOL_SIZE,
                        scenario as usize * 100_000 + concurrency * 100 + round,
                        &server.token_endpoint,
                        TlsBackend::NativeTls,
                        1,
                        EXPECTED_PROCESS_AUXILIARY_MAX_CONCURRENCY as u32,
                        6_000,
                        256,
                    );
                    let barrier = Arc::new(tokio::sync::Barrier::new(concurrency));
                    let mut tasks = Vec::with_capacity(concurrency);
                    for _ in 0..concurrency {
                        let manager = manager.clone();
                        let barrier = barrier.clone();
                        tasks.push(tokio::spawn(async move {
                            barrier.wait().await;
                            acquire_from_fake_refresh_manager(&manager).await.map(drop)
                        }));
                    }

                    let started_at = Instant::now();
                    for task in tasks {
                        assert!(
                            task.await
                                .expect("shared-manager burst task joins")
                                .is_err(),
                            "controlled shared-manager OAuth burst must fail"
                        );
                    }
                    let elapsed_ms = started_at.elapsed().as_millis();
                    let snapshot = manager.snapshot();
                    let hits = server.state.hits();
                    let distinct = server.state.distinct_request_bodies();
                    let peak_active = server.state.peak_active();
                    let cooled = snapshot
                        .entries
                        .iter()
                        .filter(|entry| entry.cooled_down)
                        .count();
                    let disabled = snapshot
                        .entries
                        .iter()
                        .filter(|entry| entry.disabled)
                        .count();
                    let refresh_failures = snapshot
                        .entries
                        .iter()
                        .map(|entry| entry.refresh_failure_count as usize)
                        .sum::<usize>();
                    let auth_health_failures = snapshot
                        .entries
                        .iter()
                        .filter(|entry| entry.last_error_kind.as_deref() == Some("auth"))
                        .count();
                    let expected_send_cap = concurrency * EXPECTED_REQUEST_AUXILIARY_MAX_ATTEMPTS;

                    eprintln!(
                        "OAUTH_REFRESH_RED_SHARED_BURST scenario={} concurrency={} pool={} round={} hits={} distinct={} peak_active={} cooled={} disabled={} refresh_failures={} auth_health={} elapsed_ms={}",
                        scenario.label(),
                        concurrency,
                        POOL_SIZE,
                        round,
                        hits,
                        distinct,
                        peak_active,
                        cooled,
                        disabled,
                        refresh_failures,
                        auth_health_failures,
                        elapsed_ms
                    );

                    assert_eq!(snapshot.global_in_flight_requests, 0);
                    assert!(distinct <= hits);
                    assert_eq!(refresh_failures, 0);
                    if let Some(expected_kind) = scenario.expected_health_kind() {
                        assert!(cooled > 0 && cooled <= hits);
                        assert_eq!(
                            auth_health_failures,
                            if expected_kind == "auth" { cooled } else { 0 }
                        );
                    } else {
                        assert_eq!(cooled, 0);
                        assert_eq!(auth_health_failures, 0);
                    }
                    assert_eq!(disabled, 0);
                    if hits > expected_send_cap {
                        violations.push(format!(
                            "scenario={} concurrency={} round={} attempted={} server_hits={} expected<={}",
                            scenario.label(),
                            concurrency,
                            round,
                            hits,
                            hits,
                            expected_send_cap
                        ));
                    }
                    if peak_active > EXPECTED_PROCESS_AUXILIARY_MAX_CONCURRENCY {
                        violations.push(format!(
                            "scenario={} concurrency={} round={} peak_active={} expected<={}",
                            scenario.label(),
                            concurrency,
                            round,
                            peak_active,
                            EXPECTED_PROCESS_AUXILIARY_MAX_CONCURRENCY
                        ));
                    }

                    let auxiliary_before_recovery = manager.auxiliary_concurrency_snapshot();
                    assert_eq!(
                        auxiliary_before_recovery.in_flight,
                        0,
                        "scenario={} concurrency={} round={}: auxiliary permits must be released before recovery",
                        scenario.label(),
                        concurrency,
                        round,
                    );
                    assert!(
                        server
                            .state
                            .wait_until_idle(std::time::Duration::from_secs(2))
                            .await,
                        "scenario={} concurrency={} round={}: failure-wave connections did not close before recovery",
                        scenario.label(),
                        concurrency,
                        round,
                    );
                    server.state.set_scenario(FakeOAuthScenario::Success);
                    let recovery_hits_before = server.state.hits();
                    let recovery_barrier = Arc::new(tokio::sync::Barrier::new(
                        EXPECTED_PROCESS_AUXILIARY_MAX_CONCURRENCY,
                    ));
                    let mut recovery_tasks = Vec::new();
                    for _ in 0..EXPECTED_PROCESS_AUXILIARY_MAX_CONCURRENCY {
                        let manager = manager.clone();
                        let barrier = recovery_barrier.clone();
                        recovery_tasks.push(tokio::spawn(async move {
                            barrier.wait().await;
                            acquire_from_fake_refresh_manager(&manager).await.map(drop)
                        }));
                    }
                    for task in recovery_tasks {
                        task.await
                            .expect("shared recovery task joins")
                            .expect("shared manager must recover after auxiliary failures");
                    }
                    let recovery_hits = server.state.hits() - recovery_hits_before;
                    eprintln!(
                        "OAUTH_REFRESH_RED_SHARED_RECOVERY scenario={} concurrency={} pool={} round={} hits={} peak_active={}",
                        scenario.label(),
                        concurrency,
                        POOL_SIZE,
                        round,
                        recovery_hits,
                        server.state.peak_active(),
                    );
                    assert!(recovery_hits > 0);
                    assert!(
                        recovery_hits <= EXPECTED_PROCESS_AUXILIARY_MAX_CONCURRENCY,
                        "scenario={} concurrency={} round={}: recovery refresh sends {} must not exceed one send per caller",
                        scenario.label(),
                        concurrency,
                        round,
                        recovery_hits,
                    );
                    assert_eq!(server.state.active(), 0);
                    assert_eq!(manager.snapshot().global_in_flight_requests, 0);
                }
            }
        }

        assert!(
            violations.is_empty(),
            "shared-manager auxiliary burst violations:\n{}",
            violations.join("\n")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn oauth_refresh_six_minute_token_singleflights_concurrent_waiters_for_five_rounds() {
        const CONCURRENCY: usize = 16;

        for round in 1..=5 {
            let server = FakeOAuthServer::start(FakeOAuthScenario::Success).await;
            // A zero credential concurrency limit is the documented unlimited mode. It lets every
            // caller reach the same per-credential refresh mutex, which directly exercises the
            // singleflight contract rather than scheduler spreading across different credentials.
            let manager = fake_refresh_manager_with_tls(
                1,
                19_000_000 + round,
                &server.token_endpoint,
                TlsBackend::NativeTls,
                0,
            );
            let barrier = Arc::new(tokio::sync::Barrier::new(CONCURRENCY));
            let mut tasks = Vec::with_capacity(CONCURRENCY);
            for _ in 0..CONCURRENCY {
                let manager = manager.clone();
                let barrier = barrier.clone();
                tasks.push(tokio::spawn(async move {
                    barrier.wait().await;
                    acquire_from_fake_refresh_manager(&manager).await.map(drop)
                }));
            }

            for task in tasks {
                task.await
                    .expect("six-minute singleflight task joins")
                    .expect("six-minute refreshed token remains request-usable");
            }

            assert_eq!(
                server.state.hits(),
                1,
                "round {round}: concurrent waiters must share one successful refresh"
            );
            assert_eq!(server.state.distinct_request_bodies(), 1, "round {round}");
            assert_eq!(server.state.active(), 0, "round {round}");
            assert_eq!(
                manager.auxiliary_concurrency_snapshot().in_flight,
                0,
                "round {round}"
            );
            assert_eq!(
                manager.snapshot().global_in_flight_requests,
                0,
                "round {round}"
            );

            acquire_from_fake_refresh_manager(&manager)
                .await
                .expect("an immediate later request reuses the six-minute token");
            assert_eq!(
                server.state.hits(),
                1,
                "round {round}: an immediately subsequent request must not refresh again"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn oauth_refresh_shared_success_sends_at_most_once_per_caller() {
        const POOL_SIZE: usize = 64;
        let mut violations = Vec::new();

        for concurrency in [1_usize, 8, 16, 32] {
            for round in 1..=5 {
                let server = FakeOAuthServer::start(FakeOAuthScenario::Success).await;
                let manager = fake_refresh_manager_with_all_limits(
                    POOL_SIZE,
                    20_000_000 + concurrency * 100 + round,
                    &server.token_endpoint,
                    TlsBackend::NativeTls,
                    1,
                    64,
                    6_000,
                    256,
                );
                let barrier = Arc::new(tokio::sync::Barrier::new(concurrency));
                let mut tasks = Vec::with_capacity(concurrency);
                for _ in 0..concurrency {
                    let manager = manager.clone();
                    let barrier = barrier.clone();
                    tasks.push(tokio::spawn(async move {
                        barrier.wait().await;
                        acquire_from_fake_refresh_manager(&manager)
                            .await
                            .map(|context| context.id)
                    }));
                }

                let started_at = Instant::now();
                let mut selected_ids = Vec::with_capacity(concurrency);
                for task in tasks {
                    selected_ids.push(
                        task.await
                            .expect("shared success task joins")
                            .expect("fake OAuth success must acquire a context"),
                    );
                }
                selected_ids.sort_unstable();
                let elapsed_ms = started_at.elapsed().as_millis();
                let hits = server.state.hits();
                let distinct = server.state.distinct_request_bodies();
                eprintln!(
                    "OAUTH_REFRESH_RED_SHARED_SUCCESS concurrency={} pool={} round={} hits={} distinct={} peak_active={} selected_ids={:?} elapsed_ms={}",
                    concurrency,
                    POOL_SIZE,
                    round,
                    hits,
                    distinct,
                    server.state.peak_active(),
                    selected_ids,
                    elapsed_ms
                );
                assert_eq!(server.state.active(), 0);
                assert_eq!(manager.snapshot().global_in_flight_requests, 0);
                assert!(distinct <= hits);
                if hits > concurrency {
                    violations.push(format!(
                        "concurrency={} round={} hits={} expected<={}",
                        concurrency, round, hits, concurrency
                    ));
                }
            }
        }

        assert!(
            violations.is_empty(),
            "successful shared refresh sent more than once per caller:\n{}",
            violations.join("\n")
        );
    }

    #[tokio::test]
    async fn external_idp_real_credential_file_refreshes_when_env_set() {
        let Ok(path) = std::env::var("KIRO_RS_REAL_EXTERNAL_IDP_CREDENTIAL_FILE") else {
            eprintln!("skip real external_idp credential test: env not set");
            return;
        };
        let raw = fs::read_to_string(&path).expect("read real external_idp credential file");
        let mut credentials: KiroCredentials =
            serde_json::from_str(&raw).expect("parse real external_idp credential file");
        credentials.canonicalize_auth_method();

        assert_eq!(credentials.auth_method.as_deref(), Some("external_idp"));
        assert!(credentials.is_external_idp_refresh_credential());
        assert!(
            credentials
                .refresh_token
                .as_deref()
                .is_some_and(|v| !v.is_empty())
        );
        assert!(
            credentials
                .client_id
                .as_deref()
                .is_some_and(|v| !v.is_empty())
        );
        assert!(
            credentials
                .token_endpoint
                .as_deref()
                .is_some_and(|v| !v.is_empty())
        );
        assert!(
            credentials
                .profile_arn
                .as_deref()
                .is_some_and(|v| !v.is_empty())
        );

        let config = Config::default();
        let refreshed = refresh_token(&credentials, &config, None)
            .await
            .expect("refresh real external_idp credential");
        let access_token = refreshed
            .access_token
            .as_deref()
            .filter(|value| !value.is_empty())
            .expect("refreshed access token");

        assert!(
            refreshed
                .refresh_token
                .as_deref()
                .is_some_and(|v| !v.is_empty())
        );
        assert!(
            refreshed
                .expires_at
                .as_deref()
                .is_some_and(|v| !v.is_empty())
        );
        assert_eq!(refreshed.token_endpoint, credentials.token_endpoint);
        assert_eq!(refreshed.issuer_url, credentials.issuer_url);
        assert_eq!(refreshed.profile_arn, credentials.profile_arn);

        let usage = super::get_usage_limits(&refreshed, &config, access_token, None)
            .await
            .expect("query usage with real external_idp credential");
        assert!(usage.usage_limit() >= usage.current_usage());

        if let Ok(output_path) = std::env::var("KIRO_RS_REAL_EXTERNAL_IDP_REFRESH_OUTPUT") {
            fs::write(
                &output_path,
                serde_json::to_string_pretty(&refreshed).expect("serialize refreshed credential"),
            )
            .expect("write refreshed real external_idp credential");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&output_path, fs::Permissions::from_mode(0o600))
                    .expect("chmod refreshed real external_idp credential");
            }
            eprintln!(
                "real external_idp credential refreshed and saved: {}",
                output_path
            );
        }
    }
}
