use anyhow::bail;
use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use std::fmt;

use crate::http_client::{ProxyConfig, build_client, send_with_response_header_timeout};
use crate::kiro::machine_id;
use crate::kiro::model::credentials::KiroCredentials;
use crate::kiro::model::token_refresh::{
    ExternalIdpRefreshResponse, IdcRefreshRequest, IdcRefreshResponse, RefreshRequest,
    RefreshResponse,
};
use crate::kiro::model::usage_limits::UsageLimitsResponse;
use crate::kiro::protocol::{is_external_idp_credentials, resolve_profile_arn};
use crate::model::config::Config;

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

/// 检查 Token 是否已过期（提前 5 分钟判断）
pub(crate) fn is_token_expired(credentials: &KiroCredentials) -> bool {
    is_token_expiring_within(credentials, 5).unwrap_or(true)
}

/// 检查 Token 是否即将过期（10分钟内）
pub(crate) fn is_token_expiring_soon(credentials: &KiroCredentials) -> bool {
    is_token_expiring_within(credentials, 10).unwrap_or(false)
}

/// 验证 refreshToken 的基本有效性
pub(crate) fn validate_refresh_token(credentials: &KiroCredentials) -> anyhow::Result<()> {
    let refresh_token = credentials
        .refresh_token
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("缺少 refreshToken"))?;

    if refresh_token.is_empty() {
        bail!("refreshToken 为空");
    }

    if refresh_token.len() < 100 || refresh_token.ends_with("...") || refresh_token.contains("...")
    {
        bail!(
            "refreshToken 已被截断（长度: {} 字符）。\n\
             这通常是 Kiro IDE 为了防止凭证被第三方工具使用而故意截断的。",
            refresh_token.len()
        );
    }

    Ok(())
}

/// Refresh Token 永久失效错误
///
/// 当服务端返回 400 + `invalid_grant` 时，表示 refreshToken 已被撤销或过期，
/// 不应重试，需立即禁用对应凭据。
#[derive(Debug)]
pub(crate) struct RefreshTokenInvalidError {
    pub message: String,
}

impl fmt::Display for RefreshTokenInvalidError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for RefreshTokenInvalidError {}

/// The refresh service definitively rejected the client credentials.
///
/// Only this typed error contributes to the bounded permanent refresh-failure
/// counter. Transport, coordination, storage and unknown protocol failures stay
/// transient so infrastructure pressure cannot disable a healthy credential.
#[derive(Debug)]
pub(crate) struct RefreshCredentialRejectedError {
    pub message: String,
}

impl fmt::Display for RefreshCredentialRejectedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for RefreshCredentialRejectedError {}

pub(super) fn is_invalid_grant_response(status: reqwest::StatusCode, body_text: &str) -> bool {
    if status.as_u16() != 400 {
        return false;
    }

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body_text) {
        if value.get("error").and_then(|value| value.as_str()) == Some("invalid_grant") {
            return true;
        }
    }

    body_text.contains("\"invalid_grant\"")
}

pub(super) fn is_refresh_credential_rejected_response(
    status: reqwest::StatusCode,
    body_text: &str,
) -> bool {
    if !matches!(status.as_u16(), 400 | 401 | 403) {
        return false;
    }

    let Ok(value) = serde_json::from_str::<serde_json::Value>(body_text) else {
        return false;
    };
    ["error", "errorCode", "code", "__type"]
        .into_iter()
        .filter_map(|key| value.get(key).and_then(serde_json::Value::as_str))
        .map(|raw| {
            raw.rsplit(['#', ':'])
                .next()
                .unwrap_or(raw)
                .to_ascii_lowercase()
        })
        .any(|code| {
            matches!(
                code.as_str(),
                "invalid_client"
                    | "unauthorized_client"
                    | "invalidclientexception"
                    | "unauthorizedclientexception"
            )
        })
}

/// 刷新 Token
pub(crate) async fn refresh_token(
    credentials: &KiroCredentials,
    config: &Config,
    proxy: Option<&ProxyConfig>,
) -> anyhow::Result<KiroCredentials> {
    let mut normalized_credentials = credentials.clone();
    normalized_credentials.canonicalize_auth_method();
    normalized_credentials.normalize_external_idp_defaults();
    let credentials = &normalized_credentials;

    // API Key 凭据不支持 Token 刷新：底层契约级拦截
    // 其他调用点（try_ensure_token / 活跃路径 / add_credential）在调用前已显式分流 API Key；
    // 仅 force_refresh_token_for 未分流，此处 bail 让错误自然传播为 400 BAD_REQUEST。
    if credentials.is_api_key_credential() {
        bail!("API Key 凭据不支持刷新 Token");
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
        refresh_external_idp_token(credentials, config, proxy).await
    } else if refresh_credentials.is_idc_refresh_credential() {
        refresh_idc_token(credentials, config, proxy).await
    } else {
        refresh_social_token(credentials, config, proxy).await
    }
}

/// 刷新 Social Token
async fn refresh_social_token(
    credentials: &KiroCredentials,
    config: &Config,
    proxy: Option<&ProxyConfig>,
) -> anyhow::Result<KiroCredentials> {
    tracing::info!("正在刷新 Social Token...");

    let refresh_token = credentials.refresh_token.as_ref().unwrap();
    // 优先级：凭据.auth_region > 凭据.region > config.auth_region > config.region
    let region = credentials.effective_auth_region(config);

    let refresh_url = format!("https://prod.{}.auth.desktop.kiro.dev/refreshToken", region);
    let refresh_domain = format!("prod.{}.auth.desktop.kiro.dev", region);
    let machine_id = machine_id::generate_from_credentials(credentials, config);
    let kiro_version = &config.kiro_version;

    let client = build_client(proxy, 60, config.tls_backend)?;
    let body = RefreshRequest {
        refresh_token: refresh_token.to_string(),
    };

    let response = client
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
        .json(&body)
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        let body_text = response.text().await.unwrap_or_default();

        // 400 + invalid_grant 表示 refreshToken 已失效；不要按瞬态错误重试。
        if is_invalid_grant_response(status, &body_text) {
            return Err(RefreshTokenInvalidError {
                message: format!("Social refreshToken 已失效 (invalid_grant): {}", body_text),
            }
            .into());
        }

        let error_msg = match status.as_u16() {
            401 => "OAuth 凭证已过期或无效，需要重新认证",
            403 => "权限不足，无法刷新 Token",
            429 => "请求过于频繁，已被限流",
            500..=599 => "服务器错误，AWS OAuth 服务暂时不可用",
            _ => "Token 刷新失败",
        };
        if is_refresh_credential_rejected_response(status, &body_text) {
            return Err(RefreshCredentialRejectedError {
                message: format!("{}: {} {}", error_msg, status, body_text),
            }
            .into());
        }
        bail!("{}: {} {}", error_msg, status, body_text);
    }

    let data: RefreshResponse = response.json().await?;

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
    proxy: Option<&ProxyConfig>,
) -> anyhow::Result<KiroCredentials> {
    tracing::info!("正在刷新 External IdP Token...");

    let refresh_token = credentials.refresh_token.as_ref().unwrap();
    let client_id = credentials
        .client_id
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("External IdP 刷新需要 clientId"))?;
    let token_endpoint = credentials
        .token_endpoint
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("External IdP 刷新需要 tokenEndpoint"))?;

    let client = build_client(proxy, 60, config.tls_backend)?;
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

    let response = client
        .post(token_endpoint)
        .header("Accept", "application/json")
        .header("User-Agent", format!("KiroIDE-{}", config.kiro_version))
        .header("Connection", "close")
        .form(&form)
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        let body_text = response.text().await.unwrap_or_default();

        if is_invalid_grant_response(status, &body_text) {
            return Err(RefreshTokenInvalidError {
                message: format!(
                    "External IdP refreshToken 已失效 (invalid_grant): {}",
                    body_text
                ),
            }
            .into());
        }

        let error_msg = match status.as_u16() {
            400 | 401 => "External IdP 凭证已过期或无效，需要重新认证",
            403 => "权限不足，无法刷新 External IdP Token",
            429 => "请求过于频繁，已被限流",
            500..=599 => "External IdP 服务暂时不可用",
            _ => "External IdP Token 刷新失败",
        };
        if is_refresh_credential_rejected_response(status, &body_text) {
            return Err(RefreshCredentialRejectedError {
                message: format!("{}: {} {}", error_msg, status, body_text),
            }
            .into());
        }
        bail!("{}: {} {}", error_msg, status, body_text);
    }

    let data: ExternalIdpRefreshResponse = response.json().await?;

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
    proxy: Option<&ProxyConfig>,
) -> anyhow::Result<KiroCredentials> {
    tracing::info!("正在刷新 IdC Token...");

    let refresh_token = credentials.refresh_token.as_ref().unwrap();
    let client_id = credentials
        .client_id
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("IdC 刷新需要 clientId"))?;
    let client_secret = credentials
        .client_secret
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("IdC 刷新需要 clientSecret"))?;

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

    let client = build_client(proxy, 60, config.tls_backend)?;
    let body = IdcRefreshRequest {
        client_id: client_id.to_string(),
        client_secret: client_secret.to_string(),
        refresh_token: refresh_token.to_string(),
        grant_type: "refresh_token".to_string(),
    };

    let response = client
        .post(&refresh_url)
        .header("content-type", "application/json")
        .header("x-amz-user-agent", x_amz_user_agent)
        .header("user-agent", &user_agent)
        .header("host", format!("oidc.{}.amazonaws.com", region))
        .header("amz-sdk-invocation-id", uuid::Uuid::new_v4().to_string())
        .header("amz-sdk-request", "attempt=1; max=4")
        .header("Connection", "close")
        .json(&body)
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        let body_text = response.text().await.unwrap_or_default();

        // 400 + invalid_grant 表示 refreshToken 已失效；不要按瞬态错误重试。
        if is_invalid_grant_response(status, &body_text) {
            return Err(RefreshTokenInvalidError {
                message: format!("IdC refreshToken 已失效 (invalid_grant): {}", body_text),
            }
            .into());
        }

        let error_msg = match status.as_u16() {
            401 => "IdC 凭证已过期或无效，需要重新认证",
            403 => "权限不足，无法刷新 Token",
            429 => "请求过于频繁，已被限流",
            500..=599 => "服务器错误，AWS OIDC 服务暂时不可用",
            _ => "IdC Token 刷新失败",
        };
        if is_refresh_credential_rejected_response(status, &body_text) {
            return Err(RefreshCredentialRejectedError {
                message: format!("{}: {} {}", error_msg, status, body_text),
            }
            .into());
        }
        bail!("{}: {} {}", error_msg, status, body_text);
    }

    let data: IdcRefreshResponse = response.json().await?;

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
    let mut url = format!(
        "https://{}/getUsageLimits?origin=AI_EDITOR&resourceType=AGENTIC_REQUEST",
        host
    );

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
    if !status.is_success() {
        let body_text = response.text().await.unwrap_or_default();
        let error_msg = match status.as_u16() {
            401 => "认证失败，Token 无效或已过期",
            403 => "权限不足，无法获取使用额度",
            429 => "请求过于频繁，已被限流",
            500..=599 => "服务器错误，AWS 服务暂时不可用",
            _ => "获取使用额度失败",
        };
        bail!("{}: {} {}", error_msg, status, body_text);
    }

    let data: UsageLimitsResponse = response.json().await?;
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

    let url = format!("https://{}/setUserPreference", host);
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
    if !status_code.is_success() {
        let body_text = response.text().await.unwrap_or_default();
        bail!("设置超额开关失败: {} {}", status_code, body_text);
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
    use std::collections::HashMap;
    use std::fs;
    use std::sync::{Arc, Mutex};

    use axum::extract::{Form, State};
    use axum::http::StatusCode;
    use axum::routing::post;
    use axum::{Json, Router};
    use serde_json::json;

    use super::{is_refresh_credential_rejected_response, refresh_token};
    use crate::kiro::model::credentials::KiroCredentials;
    use crate::model::config::Config;

    #[derive(Clone)]
    struct TokenEndpointState {
        captured_form: Arc<Mutex<Option<HashMap<String, String>>>>,
    }

    #[derive(Clone)]
    struct TokenErrorEndpointState {
        status: StatusCode,
        body: serde_json::Value,
    }

    #[test]
    fn refresh_rejection_classifier_uses_a_structured_allowlist() {
        assert!(!is_refresh_credential_rejected_response(
            reqwest::StatusCode::UNAUTHORIZED,
            "not-json"
        ));
        assert!(!is_refresh_credential_rejected_response(
            reqwest::StatusCode::FORBIDDEN,
            "{}"
        ));
        assert!(is_refresh_credential_rejected_response(
            reqwest::StatusCode::UNAUTHORIZED,
            r#"{"error":"invalid_client"}"#
        ));
        assert!(is_refresh_credential_rejected_response(
            reqwest::StatusCode::FORBIDDEN,
            r#"{"code":"UnauthorizedClientException"}"#
        ));
        assert!(is_refresh_credential_rejected_response(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":"invalid_client"}"#
        ));
        assert!(is_refresh_credential_rejected_response(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"__type":"aws.protocol#UnauthorizedClientException"}"#
        ));
        assert!(!is_refresh_credential_rejected_response(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":"slow_down"}"#
        ));
        assert!(!is_refresh_credential_rejected_response(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":"invalid_client"}"#
        ));
        assert!(!is_refresh_credential_rejected_response(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            "{}"
        ));
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

    async fn mock_external_idp_error_endpoint(
        State(state): State<TokenErrorEndpointState>,
    ) -> (StatusCode, Json<serde_json::Value>) {
        (state.status, Json(state.body))
    }

    async fn external_idp_refresh_error(
        status: StatusCode,
        body: serde_json::Value,
    ) -> anyhow::Error {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/token", post(mock_external_idp_error_endpoint))
            .with_state(TokenErrorEndpointState { status, body });
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let credentials = KiroCredentials {
            auth_method: Some("external_idp".to_string()),
            refresh_token: Some("r".repeat(150)),
            client_id: Some("client-123".to_string()),
            token_endpoint: Some(format!("http://{addr}/token")),
            ..Default::default()
        };

        let error = refresh_token(&credentials, &Config::default(), None)
            .await
            .unwrap_err();
        server.abort();
        error
    }

    #[tokio::test]
    async fn external_idp_refresh_errors_preserve_permanence_classification() {
        let rejected = external_idp_refresh_error(
            StatusCode::UNAUTHORIZED,
            json!({"error": "invalid_client"}),
        )
        .await;
        assert!(
            rejected
                .downcast_ref::<super::RefreshCredentialRejectedError>()
                .is_some()
        );

        let unstructured_unauthorized =
            external_idp_refresh_error(StatusCode::UNAUTHORIZED, json!({})).await;
        assert!(
            unstructured_unauthorized
                .downcast_ref::<super::RefreshCredentialRejectedError>()
                .is_none()
        );
        assert!(
            unstructured_unauthorized
                .downcast_ref::<super::RefreshTokenInvalidError>()
                .is_none()
        );

        let rate_limited = external_idp_refresh_error(
            StatusCode::TOO_MANY_REQUESTS,
            json!({"error": "invalid_client"}),
        )
        .await;
        assert!(
            rate_limited
                .downcast_ref::<super::RefreshCredentialRejectedError>()
                .is_none()
        );
        assert!(
            rate_limited
                .downcast_ref::<super::RefreshTokenInvalidError>()
                .is_none()
        );

        let invalid_grant =
            external_idp_refresh_error(StatusCode::BAD_REQUEST, json!({"error": "invalid_grant"}))
                .await;
        assert!(
            invalid_grant
                .downcast_ref::<super::RefreshTokenInvalidError>()
                .is_some()
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
