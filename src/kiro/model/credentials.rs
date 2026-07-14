//! Kiro OAuth 凭证数据模型
//!
//! 支持从 Kiro IDE 的凭证文件加载，使用 Social 认证方式
//! 支持单凭据和多凭据配置格式

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::{fmt, fs};

use base64::{
    Engine as _,
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
};

use crate::http_client::ProxyConfig;
use crate::model::config::Config;
use crate::model::model_support::{model_is_supported_by_list, normalize_supported_models};

/// Kiro API Key/headless 凭据默认应走 CLI runtime 协议。
///
/// `Config.defaultEndpoint` 默认为 `ide`，OAuth/IDE 登录账号继续使用该默认值；
/// 但 `ksk_...` API Key 是 Kiro CLI/headless 认证形态，默认走 `cli` 可以避免
/// 误带 IDE/profile 语义。
pub const KIRO_API_KEY_DEFAULT_ENDPOINT: &str = "cli";

/// Kiro OAuth 凭证
#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct KiroCredentials {
    /// 凭据唯一标识符（自增 ID）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,

    /// 凭据创建时间（由 PgSQL credentials.created_at 提供，仅用于运行时展示）
    #[serde(skip)]
    pub created_at: Option<String>,

    /// 凭据更新时间（由 PgSQL credentials.updated_at 提供，仅用于运行时展示）
    #[serde(skip)]
    pub updated_at: Option<String>,

    /// PgSQL credentials.revision，仅用于持久化层乐观并发控制。
    #[serde(skip)]
    pub storage_revision: u64,

    /// 访问令牌
    #[serde(skip_serializing_if = "Option::is_none", alias = "access_token")]
    pub access_token: Option<String>,

    /// 刷新令牌
    #[serde(skip_serializing_if = "Option::is_none", alias = "refresh_token")]
    pub refresh_token: Option<String>,

    /// Profile ARN
    #[serde(skip_serializing_if = "Option::is_none", alias = "profile_arn")]
    pub profile_arn: Option<String>,

    /// 过期时间 (RFC3339 格式)
    #[serde(
        skip_serializing_if = "Option::is_none",
        alias = "expires_at",
        alias = "expired"
    )]
    pub expires_at: Option<String>,

    /// 认证方式 (social / idc / external_idp / api_key)
    #[serde(skip_serializing_if = "Option::is_none", alias = "auth_method")]
    pub auth_method: Option<String>,

    /// 上游身份提供方（BuilderId / Enterprise / ExternalIdp / Github / Google 等）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,

    /// OIDC Client ID (IdC 认证需要)
    #[serde(skip_serializing_if = "Option::is_none", alias = "client_id")]
    pub client_id: Option<String>,

    /// OIDC Client Secret (IdC 认证需要)
    #[serde(skip_serializing_if = "Option::is_none", alias = "client_secret")]
    pub client_secret: Option<String>,

    /// 外部 IdP OAuth token endpoint（external_idp 认证需要）
    #[serde(skip_serializing_if = "Option::is_none", alias = "token_endpoint")]
    pub token_endpoint: Option<String>,

    /// 外部 IdP issuer URL（可选，主要用于导入留痕和排查）
    #[serde(skip_serializing_if = "Option::is_none", alias = "issuer_url")]
    pub issuer_url: Option<String>,

    /// 外部 IdP OAuth scopes（可选；存在时刷新时原样带给 token endpoint）
    #[serde(skip_serializing_if = "Option::is_none", alias = "scope")]
    pub scopes: Option<String>,

    /// 凭据优先级（数字越小优先级越高，默认为 0）
    #[serde(default)]
    #[serde(skip_serializing_if = "is_zero")]
    pub priority: u32,

    /// 凭据级最大并发请求数覆盖。
    ///
    /// `None` 表示继承全局 `credentialMaxConcurrentRequests`；
    /// `Some(0)` 表示该凭据不限制并发；
    /// `Some(n)` 表示该凭据最多同时处理 n 个请求。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_concurrent_requests: Option<u32>,

    /// 凭据级每分钟请求数覆盖。
    ///
    /// `None` 表示继承全局 `credentialRpm`；
    /// `Some(0)` 表示该凭据不做本地 RPM 限制；
    /// `Some(n)` 表示该凭据每分钟最多调度 n 次。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rpm: Option<u32>,

    /// 429 临时风控是否自动禁用账号。
    ///
    /// `None`/`Some(true)` 表示保持默认行为：429 suspicious activity/temporary limits
    /// 风控响应会禁用账号；`Some(false)` 表示仅进入 429 冷却，不自动禁用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_auto_disable_enabled: Option<bool>,

    /// 凭据级 Region 配置（用于 OIDC token 刷新）
    /// 未配置时回退到 config.json 的全局 region
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,

    /// 凭据级 Auth Region（用于 Token 刷新）
    #[serde(skip_serializing_if = "Option::is_none", alias = "auth_region")]
    pub auth_region: Option<String>,

    /// 凭据级 API Region（用于 API 请求）
    #[serde(skip_serializing_if = "Option::is_none", alias = "api_region")]
    pub api_region: Option<String>,

    /// 凭据级 Machine ID 配置（可选）
    /// 未配置时回退到 config.json 的 machineId；都未配置时由 refreshToken 派生
    #[serde(skip_serializing_if = "Option::is_none", alias = "machine_id")]
    pub machine_id: Option<String>,

    /// 用户邮箱（从 Anthropic API 获取）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,

    /// 订阅等级（KIRO PRO+ / KIRO FREE 等）
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub subscription_title: Option<String>,

    /// 凭据支持的模型列表。空列表表示不限制模型调度。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_models: Vec<String>,

    /// 凭据级代理 URL（可选）
    /// 支持 http/https/socks5 协议
    /// 特殊值 "direct" 表示显式不使用代理（即使全局配置了代理）
    /// 未配置时回退到全局代理配置
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,

    /// 凭据级代理认证用户名（可选）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_username: Option<String>,

    /// 凭据级代理认证密码（可选）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_password: Option<String>,

    /// 绑定的代理/家宽资源 ID（可选）
    ///
    /// 直接配置 proxyUrl 时优先使用凭据级代理；未配置 proxyUrl 时才使用该资源。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_resource_id: Option<u64>,

    /// 凭据是否被禁用（默认为 false）
    #[serde(default)]
    pub disabled: bool,

    /// Kiro API Key（headless 模式）
    /// 格式: ksk_xxxxxxxx
    /// 设置后直接作为 Bearer Token 使用，无需 refreshToken
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kiro_api_key: Option<String>,

    /// 端点名称（可选）
    ///
    /// 决定该凭据走哪套 Kiro API。未配置时回退到 `config.defaultEndpoint`（默认 "ide"）。
    /// 端点名必须在启动时注册的端点 registry 中存在。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
}

impl fmt::Debug for KiroCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("KiroCredentials")
            .field("id", &self.id)
            .field("created_at_present", &self.created_at.is_some())
            .field("updated_at_present", &self.updated_at.is_some())
            .field("storage_revision", &self.storage_revision)
            .field("access_token_present", &self.access_token.is_some())
            .field("refresh_token_present", &self.refresh_token.is_some())
            .field("profile_arn_present", &self.profile_arn.is_some())
            .field("expires_at_present", &self.expires_at.is_some())
            .field("auth_method_present", &self.auth_method.is_some())
            .field("provider_present", &self.provider.is_some())
            .field("client_id_present", &self.client_id.is_some())
            .field("client_secret_present", &self.client_secret.is_some())
            .field("token_endpoint_present", &self.token_endpoint.is_some())
            .field("issuer_url_present", &self.issuer_url.is_some())
            .field("scopes_present", &self.scopes.is_some())
            .field("priority", &self.priority)
            .field("max_concurrent_requests", &self.max_concurrent_requests)
            .field("rpm", &self.rpm)
            .field(
                "rate_limit_auto_disable_enabled",
                &self.rate_limit_auto_disable_enabled,
            )
            .field("region_present", &self.region.is_some())
            .field("auth_region_present", &self.auth_region.is_some())
            .field("api_region_present", &self.api_region.is_some())
            .field("machine_id_present", &self.machine_id.is_some())
            .field("email_present", &self.email.is_some())
            .field(
                "subscription_title_present",
                &self.subscription_title.is_some(),
            )
            .field("supported_model_count", &self.supported_models.len())
            .field("proxy_url_present", &self.proxy_url.is_some())
            .field("proxy_username_present", &self.proxy_username.is_some())
            .field("proxy_password_present", &self.proxy_password.is_some())
            .field("proxy_resource_id", &self.proxy_resource_id)
            .field("disabled", &self.disabled)
            .field("kiro_api_key_present", &self.kiro_api_key.is_some())
            .field("endpoint_present", &self.endpoint.is_some())
            .finish()
    }
}

/// 判断是否为零（用于跳过序列化）
fn is_zero(value: &u32) -> bool {
    *value == 0
}

fn compact_protocol_value(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn normalized_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// 解析 Kiro API Key 便捷格式：`ksk_xxx|region`。
///
/// 返回 `(api_key, region)`。没有 `|region` 时只返回 key。
/// 该函数不打印、不记录原始 key，调用方负责继续按敏感字段处理。
pub fn split_kiro_api_key_and_region(raw: &str) -> Option<(String, Option<String>)> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let Some((key, region)) = trimmed.split_once('|') else {
        return Some((trimmed.to_string(), None));
    };

    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    let region = region.trim();
    Some((
        key.to_string(),
        (!region.is_empty()).then(|| region.to_string()),
    ))
}

fn looks_like_kiro_api_key_text(value: &str) -> bool {
    let key = value
        .trim()
        .split_once('|')
        .map(|(key, _)| key)
        .unwrap_or_else(|| value.trim())
        .trim();
    key.starts_with("ksk_")
}

fn api_key_credential_from_text(raw: &str, priority: u32) -> Option<KiroCredentials> {
    let (api_key, region) = split_kiro_api_key_and_region(raw)?;
    if !looks_like_kiro_api_key_text(&api_key) {
        return None;
    }

    let mut credential = KiroCredentials {
        auth_method: Some("api_key".to_string()),
        kiro_api_key: Some(api_key),
        priority,
        region: region.clone(),
        auth_region: region.clone(),
        api_region: region,
        endpoint: Some(KIRO_API_KEY_DEFAULT_ENDPOINT.to_string()),
        ..Default::default()
    };
    credential.normalize_api_key_defaults();
    Some(credential)
}

fn api_key_credentials_from_plain_text(content: &str) -> Option<Vec<KiroCredentials>> {
    let mut credentials = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let credential = api_key_credential_from_text(line, credentials.len() as u32)?;
        credentials.push(credential);
    }

    (!credentials.is_empty()).then_some(credentials)
}

fn canonicalize_auth_method_value(value: &str) -> &str {
    match compact_protocol_value(value).as_str() {
        "builderid" | "iam" | "idc" => "idc",
        "apikey" => "api_key",
        "externalidp" | "enterprise" | "iamsso" | "awsidc" | "internal" => "external_idp",
        "social" => "social",
        _ => value,
    }
}

fn trimmed_non_empty(value: &Option<String>) -> Option<&str> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn microsoft_token_endpoint_from_issuer(issuer: &str) -> Option<String> {
    let parsed = url::Url::parse(issuer.trim()).ok()?;
    if parsed.scheme() != "https" {
        return None;
    }

    let host = parsed.host_str()?.to_ascii_lowercase();
    if host != "login.microsoftonline.com" {
        return None;
    }

    let mut segments = parsed.path_segments()?;
    let tenant = segments.find(|segment| !segment.is_empty())?;
    if tenant.eq_ignore_ascii_case("oauth2") {
        return None;
    }

    Some(format!(
        "https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token"
    ))
}

fn jwt_issuer(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| URL_SAFE.decode(payload))
        .ok()?;
    let value = serde_json::from_slice::<serde_json::Value>(&decoded).ok()?;
    value
        .get("iss")
        .and_then(|issuer| issuer.as_str())
        .map(str::to_string)
}

fn looks_like_microsoft_refresh_token(refresh_token: &str) -> bool {
    let trimmed = refresh_token.trim();
    trimmed.starts_with("1.") || trimmed.starts_with("0.")
}

fn default_external_idp_scopes(client_id: &str) -> String {
    format!(
        "api://{client_id}/codewhisperer:conversations api://{client_id}/codewhisperer:completions offline_access"
    )
}

pub(crate) fn profile_arn_region(profile_arn: &str) -> Option<&str> {
    let parts: Vec<&str> = profile_arn.trim().splitn(6, ':').collect();
    if parts.len() < 6 || parts[0] != "arn" || parts[2] != "codewhisperer" {
        return None;
    }
    let region = parts[3].trim();
    (!region.is_empty()).then_some(region)
}

/// 凭据配置（支持单对象或数组格式）
///
/// 自动识别配置文件格式：
/// - 单对象格式（旧格式，向后兼容）
/// - 数组格式（新格式，支持多凭据）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CredentialsConfig {
    /// 单个凭据（旧格式）
    Single(KiroCredentials),
    /// 多凭据数组（新格式）
    Multiple(Vec<KiroCredentials>),
}

impl CredentialsConfig {
    /// 从文件加载凭据配置
    ///
    /// - 如果文件不存在，返回空数组
    /// - 如果文件内容为空，返回空数组
    /// - 支持单对象或数组格式
    pub fn load<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let path = path.as_ref();

        // 文件不存在时返回空数组
        if !path.exists() {
            return Ok(CredentialsConfig::Multiple(vec![]));
        }

        let content = fs::read_to_string(path)?;

        // 文件为空时返回空数组
        if content.trim().is_empty() {
            return Ok(CredentialsConfig::Multiple(vec![]));
        }

        match serde_json::from_str(&content) {
            Ok(config) => Ok(config),
            Err(json_error) => {
                if let Some(credentials) = api_key_credentials_from_plain_text(&content) {
                    Ok(CredentialsConfig::Multiple(credentials))
                } else {
                    Err(json_error.into())
                }
            }
        }
    }

    /// 转换为按优先级排序的凭据列表
    pub fn into_sorted_credentials(self) -> Vec<KiroCredentials> {
        match self {
            CredentialsConfig::Single(mut cred) => {
                cred.canonicalize_auth_method();
                cred.normalize_supported_models();
                cred.normalize_api_key_defaults();
                cred.normalize_external_idp_defaults();
                vec![cred]
            }
            CredentialsConfig::Multiple(mut creds) => {
                // 按优先级排序（数字越小优先级越高）
                creds.sort_by_key(|c| c.priority);
                for cred in &mut creds {
                    cred.canonicalize_auth_method();
                    cred.normalize_supported_models();
                    cred.normalize_api_key_defaults();
                    cred.normalize_external_idp_defaults();
                }
                creds
            }
        }
    }

    /// 判断是否为多凭据格式（数组格式）
    pub fn is_multiple(&self) -> bool {
        matches!(self, CredentialsConfig::Multiple(_))
    }
}

impl KiroCredentials {
    /// 特殊值：显式不使用代理
    pub const PROXY_DIRECT: &'static str = "direct";

    /// 比较会影响认证、调度、代理解析和后台展示的持久化字段。
    pub fn same_dispatch_config(&self, other: &Self) -> bool {
        self.id == other.id
            && self.access_token == other.access_token
            && self.refresh_token == other.refresh_token
            && self.profile_arn == other.profile_arn
            && self.expires_at == other.expires_at
            && self.auth_method == other.auth_method
            && self.provider == other.provider
            && self.client_id == other.client_id
            && self.client_secret == other.client_secret
            && self.token_endpoint == other.token_endpoint
            && self.issuer_url == other.issuer_url
            && self.scopes == other.scopes
            && self.priority == other.priority
            && self.max_concurrent_requests == other.max_concurrent_requests
            && self.rpm == other.rpm
            && self.rate_limit_auto_disable_enabled == other.rate_limit_auto_disable_enabled
            && self.region == other.region
            && self.auth_region == other.auth_region
            && self.api_region == other.api_region
            && self.machine_id == other.machine_id
            && self.email == other.email
            && self.subscription_title == other.subscription_title
            && self.supported_models == other.supported_models
            && self.proxy_url == other.proxy_url
            && self.proxy_username == other.proxy_username
            && self.proxy_password == other.proxy_password
            && self.proxy_resource_id == other.proxy_resource_id
            && self.disabled == other.disabled
            && self.kiro_api_key == other.kiro_api_key
            && self.endpoint == other.endpoint
    }

    /// 获取默认凭证文件路径
    pub fn default_credentials_path() -> &'static str {
        "credentials.json"
    }

    /// 429 临时风控是否自动禁用账号。未配置时默认启用，保持旧行为。
    pub fn rate_limit_auto_disable_enabled(&self) -> bool {
        self.rate_limit_auto_disable_enabled.unwrap_or(true)
    }

    /// 获取有效的 Auth Region（用于 Token 刷新）
    /// 优先级：凭据.auth_region > 凭据.region > config.auth_region > config.region
    pub fn effective_auth_region<'a>(&'a self, config: &'a Config) -> &'a str {
        self.auth_region
            .as_deref()
            .or(self.region.as_deref())
            .unwrap_or(config.effective_auth_region())
    }

    /// 获取有效的 API Region（用于 API 请求）
    /// 优先级：凭据.api_region > profileArn region > config.api_region > config.region
    pub fn effective_api_region<'a>(&'a self, config: &'a Config) -> &'a str {
        self.api_region
            .as_deref()
            .or_else(|| self.profile_arn.as_deref().and_then(profile_arn_region))
            .unwrap_or(config.effective_api_region())
    }

    /// 获取有效的代理配置
    /// 优先级：凭据代理 > 全局代理 > 无代理
    /// 特殊值 "direct" 表示显式不使用代理（即使全局配置了代理）
    pub fn effective_proxy(&self, global_proxy: Option<&ProxyConfig>) -> Option<ProxyConfig> {
        match self.proxy_url.as_deref() {
            Some(url) if url.eq_ignore_ascii_case(Self::PROXY_DIRECT) => None,
            Some(url) => {
                let mut proxy = ProxyConfig::new(url);
                if let (Some(username), Some(password)) =
                    (&self.proxy_username, &self.proxy_password)
                {
                    proxy = proxy.with_auth(username, password);
                }
                Some(proxy)
            }
            None => global_proxy.cloned(),
        }
    }

    pub fn canonicalize_auth_method(&mut self) {
        let auth_method = match &self.auth_method {
            Some(m) => m,
            None => return,
        };

        let canonical = canonicalize_auth_method_value(auth_method);
        if canonical != auth_method {
            self.auth_method = Some(canonical.to_string());
        }
    }

    /// 规范化 Kiro API Key/headless 凭据。
    ///
    /// 支持把 `kiroApiKey: "ksk_xxx|eu-central-1"` 拆成真实 key 和区域；
    /// API Key 凭据默认走 `cli` endpoint；如果只给了 `region`，同步补齐
    /// `authRegion/apiRegion`，避免 API 请求仍回退到全局区域。
    pub fn normalize_api_key_defaults(&mut self) {
        self.canonicalize_auth_method();
        if !self.is_api_key_credential() {
            return;
        }

        let Some(raw_key) = self.kiro_api_key.clone() else {
            return;
        };
        if raw_key.trim().is_empty() {
            return;
        }

        if let Some((api_key, parsed_region)) = split_kiro_api_key_and_region(&raw_key) {
            self.kiro_api_key = Some(api_key);
            if let Some(region) = parsed_region {
                if self.region.as_deref().is_none_or(str::is_empty) {
                    self.region = Some(region.clone());
                }
                if self.auth_region.as_deref().is_none_or(str::is_empty) {
                    self.auth_region = Some(region.clone());
                }
                if self.api_region.as_deref().is_none_or(str::is_empty) {
                    self.api_region = Some(region);
                }
            }
        }

        self.auth_method = Some("api_key".to_string());
        self.refresh_token = None;
        self.provider = None;
        self.client_id = None;
        self.client_secret = None;
        self.token_endpoint = None;
        self.issuer_url = None;
        self.scopes = None;
        self.access_token = None;
        self.expires_at = None;
        self.profile_arn = None;

        if self.auth_region.as_deref().is_none_or(str::is_empty) {
            self.auth_region = self.region.clone();
        }
        if self.api_region.as_deref().is_none_or(str::is_empty) {
            self.api_region = self.region.clone();
        }
        if self.endpoint.as_deref().is_none_or(str::is_empty) {
            self.endpoint = Some(KIRO_API_KEY_DEFAULT_ENDPOINT.to_string());
        }

        self.kiro_api_key = normalized_optional(self.kiro_api_key.take());
        self.region = normalized_optional(self.region.take());
        self.auth_region = normalized_optional(self.auth_region.take());
        self.api_region = normalized_optional(self.api_region.take());
        self.endpoint = normalized_optional(self.endpoint.take());
    }

    /// 补齐 external_idp 账号的可推导字段。
    ///
    /// 企业 SSO 导出格式经常没有 AWS SSO device-flow 的 clientSecret。Microsoft
    /// Entra ID 这类 public-client refresh token 只需要 clientId、refreshToken
    /// 和 token endpoint；当导入 JSON 只带 issuerUrl 或 accessToken 时，可以安全
    /// 推导 token endpoint。缺 scopes 时按 Kiro 官方 CodeWhisperer scope 补齐。
    pub fn normalize_external_idp_defaults(&mut self) {
        self.canonicalize_auth_method();
        if !self.is_external_idp_refresh_credential() {
            return;
        }

        if self.token_endpoint.as_deref().is_none_or(str::is_empty) {
            if let Some(endpoint) =
                trimmed_non_empty(&self.issuer_url).and_then(microsoft_token_endpoint_from_issuer)
            {
                self.token_endpoint = Some(endpoint);
            } else if let Some(endpoint) = trimmed_non_empty(&self.access_token)
                .and_then(jwt_issuer)
                .as_deref()
                .and_then(microsoft_token_endpoint_from_issuer)
            {
                self.token_endpoint = Some(endpoint);
            } else if trimmed_non_empty(&self.refresh_token)
                .is_some_and(looks_like_microsoft_refresh_token)
            {
                self.token_endpoint =
                    Some("https://login.microsoftonline.com/common/oauth2/v2.0/token".to_string());
            }
        }

        if self.scopes.as_deref().is_none_or(str::is_empty) {
            if let Some(client_id) = trimmed_non_empty(&self.client_id) {
                self.scopes = Some(default_external_idp_scopes(client_id));
            }
        }
    }

    /// 检查凭据是否支持 Opus 模型
    ///
    /// Free 账号不支持 Opus 模型，需要 PRO 或更高等级订阅
    pub fn supports_opus(&self) -> bool {
        match &self.subscription_title {
            Some(title) => {
                let title_upper = title.to_uppercase();
                // 如果包含 FREE，则不支持 Opus
                !title_upper.contains("FREE")
            }
            // 如果还没有获取订阅信息，暂时允许（首次使用时会获取）
            None => true,
        }
    }

    pub fn normalize_supported_models(&mut self) {
        self.supported_models =
            normalize_supported_models(std::mem::take(&mut self.supported_models));
    }

    pub fn supports_model(&self, candidates: &[Option<&str>]) -> bool {
        model_is_supported_by_list(&self.supported_models, candidates)
    }

    /// 检查是否为 API Key 凭据
    ///
    /// API Key 凭据直接使用 kiro_api_key 作为 Bearer Token，无需 refreshToken
    pub fn is_api_key_credential(&self) -> bool {
        self.kiro_api_key.is_some()
            || self
                .auth_method
                .as_deref()
                .map(|m| compact_protocol_value(m) == "apikey")
                .unwrap_or(false)
    }

    /// 检查是否应使用 AWS SSO OIDC refresh token 协议刷新。
    ///
    /// Enterprise / external IdP 与 Builder ID 一样走 OIDC 刷新，但请求 Kiro API
    /// 时仍保留 external_idp 语义以附加 TokenType。
    pub fn is_idc_refresh_credential(&self) -> bool {
        if self.is_api_key_credential() {
            return false;
        }
        if self.is_external_idp_refresh_credential() {
            return false;
        }

        self.auth_method.as_deref().is_some_and(|m| {
            matches!(
                compact_protocol_value(m).as_str(),
                "idc" | "builderid" | "iam"
            )
        }) || (self.client_id.is_some() && self.client_secret.is_some())
    }

    /// 检查是否应使用外部 IdP 自带 token endpoint 刷新。
    ///
    /// 这类凭证来自企业 SSO，例如 Microsoft Entra ID，通常只有 public
    /// clientId、refreshToken、tokenEndpoint 和 scopes，不存在 AWS SSO
    /// OIDC device-flow 的 clientSecret。
    pub fn is_external_idp_refresh_credential(&self) -> bool {
        if self.is_api_key_credential() {
            return false;
        }

        self.auth_method.as_deref().is_some_and(|m| {
            matches!(
                compact_protocol_value(m).as_str(),
                "externalidp" | "enterprise" | "iamsso" | "awsidc" | "internal"
            )
        }) || self.provider.as_deref().is_some_and(|p| {
            matches!(
                compact_protocol_value(p).as_str(),
                "enterprise" | "externalidp" | "iamsso" | "awsidc" | "internal"
            )
        })
    }
}

#[cfg(test)]
impl KiroCredentials {
    fn from_json(json_string: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json_string)
    }

    fn to_pretty_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::config::Config;

    #[test]
    fn debug_output_redacts_all_credential_strings() {
        let credentials = KiroCredentials {
            id: Some(42),
            created_at: Some("created-at-sensitive-value".to_string()),
            updated_at: Some("updated-at-sensitive-value".to_string()),
            storage_revision: 7,
            access_token: Some("access-token-sensitive-value".to_string()),
            refresh_token: Some("refresh-token-sensitive-value".to_string()),
            profile_arn: Some("profile-arn-sensitive-value".to_string()),
            expires_at: Some("expires-at-sensitive-value".to_string()),
            auth_method: Some("auth-method-sensitive-value".to_string()),
            provider: Some("provider-sensitive-value".to_string()),
            client_id: Some("client-id-sensitive-value".to_string()),
            client_secret: Some("client-secret-sensitive-value".to_string()),
            token_endpoint: Some("token-endpoint-sensitive-value".to_string()),
            issuer_url: Some("issuer-url-sensitive-value".to_string()),
            scopes: Some("scopes-sensitive-value".to_string()),
            priority: 3,
            max_concurrent_requests: Some(4),
            rpm: Some(5),
            rate_limit_auto_disable_enabled: Some(false),
            region: Some("region-sensitive-value".to_string()),
            auth_region: Some("auth-region-sensitive-value".to_string()),
            api_region: Some("api-region-sensitive-value".to_string()),
            machine_id: Some("machine-id-sensitive-value".to_string()),
            email: Some("email-sensitive-value@example.invalid".to_string()),
            subscription_title: Some("subscription-sensitive-value".to_string()),
            supported_models: vec!["model-sensitive-value".to_string()],
            proxy_url: Some("http://proxy-user:proxy-pass@example.invalid".to_string()),
            proxy_username: Some("proxy-username-sensitive-value".to_string()),
            proxy_password: Some("proxy-password-sensitive-value".to_string()),
            proxy_resource_id: Some(6),
            disabled: true,
            kiro_api_key: Some("kiro-api-key-sensitive-value".to_string()),
            endpoint: Some("endpoint-sensitive-value".to_string()),
        };

        let debug_output = format!("{credentials:?}");
        for sensitive_value in [
            credentials.created_at.as_deref().unwrap(),
            credentials.updated_at.as_deref().unwrap(),
            credentials.access_token.as_deref().unwrap(),
            credentials.refresh_token.as_deref().unwrap(),
            credentials.profile_arn.as_deref().unwrap(),
            credentials.expires_at.as_deref().unwrap(),
            credentials.auth_method.as_deref().unwrap(),
            credentials.provider.as_deref().unwrap(),
            credentials.client_id.as_deref().unwrap(),
            credentials.client_secret.as_deref().unwrap(),
            credentials.token_endpoint.as_deref().unwrap(),
            credentials.issuer_url.as_deref().unwrap(),
            credentials.scopes.as_deref().unwrap(),
            credentials.region.as_deref().unwrap(),
            credentials.auth_region.as_deref().unwrap(),
            credentials.api_region.as_deref().unwrap(),
            credentials.machine_id.as_deref().unwrap(),
            credentials.email.as_deref().unwrap(),
            credentials.subscription_title.as_deref().unwrap(),
            credentials.supported_models[0].as_str(),
            credentials.proxy_url.as_deref().unwrap(),
            credentials.proxy_username.as_deref().unwrap(),
            credentials.proxy_password.as_deref().unwrap(),
            credentials.kiro_api_key.as_deref().unwrap(),
            credentials.endpoint.as_deref().unwrap(),
        ] {
            assert!(
                !debug_output.contains(sensitive_value),
                "Debug output leaked {sensitive_value:?}: {debug_output}"
            );
        }
        assert!(debug_output.contains("id: Some(42)"));
        assert!(debug_output.contains("access_token_present: true"));
        assert!(debug_output.contains("email_present: true"));
    }

    #[test]
    fn test_from_json() {
        let json = r#"{
            "accessToken": "test_token",
            "refreshToken": "test_refresh",
            "profileArn": "arn:aws:test",
            "expiresAt": "2024-01-01T00:00:00Z",
            "authMethod": "social"
        }"#;

        let creds = KiroCredentials::from_json(json).unwrap();
        assert_eq!(creds.access_token, Some("test_token".to_string()));
        assert_eq!(creds.refresh_token, Some("test_refresh".to_string()));
        assert_eq!(creds.profile_arn, Some("arn:aws:test".to_string()));
        assert_eq!(creds.expires_at, Some("2024-01-01T00:00:00Z".to_string()));
        assert_eq!(creds.auth_method, Some("social".to_string()));
    }

    #[test]
    fn test_from_json_accepts_snake_case_import_fields() {
        let json = r#"{
            "access_token": "test_access",
            "refresh_token": "test_refresh",
            "profile_arn": "arn:aws:codewhisperer:us-east-1:123456789012:profile/FAKE",
            "expires_at": "2026-06-10T15:53:19.000Z",
            "auth_method": "idc",
            "provider": "Enterprise",
            "client_id": "fake-client-id",
            "client_secret": "fake-client-secret",
            "token_endpoint": "https://login.example.com/oauth2/v2.0/token",
            "issuer_url": "https://login.example.com/tenant/v2.0",
            "scopes": "offline_access codewhisperer:conversations",
            "region": "us-east-1",
            "auth_region": "us-west-2",
            "api_region": "eu-west-1",
            "machine_id": "fake-machine-id"
        }"#;

        let creds = KiroCredentials::from_json(json).unwrap();

        assert_eq!(creds.access_token.as_deref(), Some("test_access"));
        assert_eq!(creds.refresh_token.as_deref(), Some("test_refresh"));
        assert_eq!(
            creds.profile_arn.as_deref(),
            Some("arn:aws:codewhisperer:us-east-1:123456789012:profile/FAKE")
        );
        assert_eq!(
            creds.expires_at.as_deref(),
            Some("2026-06-10T15:53:19.000Z")
        );
        assert_eq!(creds.auth_method.as_deref(), Some("idc"));
        assert_eq!(creds.provider.as_deref(), Some("Enterprise"));
        assert_eq!(creds.client_id.as_deref(), Some("fake-client-id"));
        assert_eq!(creds.client_secret.as_deref(), Some("fake-client-secret"));
        assert_eq!(
            creds.token_endpoint.as_deref(),
            Some("https://login.example.com/oauth2/v2.0/token")
        );
        assert_eq!(
            creds.issuer_url.as_deref(),
            Some("https://login.example.com/tenant/v2.0")
        );
        assert_eq!(
            creds.scopes.as_deref(),
            Some("offline_access codewhisperer:conversations")
        );
        assert_eq!(creds.region.as_deref(), Some("us-east-1"));
        assert_eq!(creds.auth_region.as_deref(), Some("us-west-2"));
        assert_eq!(creds.api_region.as_deref(), Some("eu-west-1"));
        assert_eq!(creds.machine_id.as_deref(), Some("fake-machine-id"));
    }

    #[test]
    fn test_from_json_with_unknown_keys() {
        let json = r#"{
            "accessToken": "test_token",
            "unknownField": "should be ignored"
        }"#;

        let creds = KiroCredentials::from_json(json).unwrap();
        assert_eq!(creds.access_token, Some("test_token".to_string()));
    }

    #[test]
    fn supported_models_empty_allows_any_and_nonempty_filters_candidates() {
        let mut creds = KiroCredentials::default();
        assert!(creds.supports_model(&[Some("claude-sonnet-4")]));

        creds.supported_models = vec![
            " claude-sonnet-4 ".to_string(),
            "CLAUDE-SONNET-4".to_string(),
            "claude-haiku-4.5".to_string(),
        ];
        creds.normalize_supported_models();

        assert_eq!(
            creds.supported_models,
            vec![
                "claude-sonnet-4".to_string(),
                "claude-haiku-4.5".to_string()
            ]
        );
        assert!(creds.supports_model(&[Some("Claude-Sonnet-4")]));
        assert!(creds.supports_model(&[None, Some("claude-haiku-4.5")]));
        assert!(!creds.supports_model(&[Some("claude-opus-4.5")]));
        assert!(!creds.supports_model(&[None]));
    }

    #[test]
    fn test_to_json() {
        let creds = KiroCredentials {
            id: None,
            access_token: Some("token".to_string()),
            auth_method: Some("social".to_string()),
            provider: Some("Github".to_string()),
            ..Default::default()
        };

        let json = creds.to_pretty_json().unwrap();
        assert!(json.contains("accessToken"));
        assert!(json.contains("authMethod"));
        assert!(json.contains("provider"));
        assert!(!json.contains("refreshToken"));
        // priority 为 0 时不序列化
        assert!(!json.contains("priority"));
    }

    #[test]
    fn test_default_credentials_path() {
        assert_eq!(
            KiroCredentials::default_credentials_path(),
            "credentials.json"
        );
    }

    #[test]
    fn test_priority_default() {
        let json = r#"{"refreshToken": "test"}"#;
        let creds = KiroCredentials::from_json(json).unwrap();
        assert_eq!(creds.priority, 0);
    }

    #[test]
    fn test_priority_explicit() {
        let json = r#"{"refreshToken": "test", "priority": 5}"#;
        let creds = KiroCredentials::from_json(json).unwrap();
        assert_eq!(creds.priority, 5);
    }

    #[test]
    fn rate_limit_auto_disable_defaults_to_enabled() {
        let creds = KiroCredentials::default();
        assert!(creds.rate_limit_auto_disable_enabled());

        let parsed = KiroCredentials::from_json("{}").unwrap();
        assert!(parsed.rate_limit_auto_disable_enabled());
    }

    #[test]
    fn rate_limit_auto_disable_can_be_disabled() {
        let parsed =
            KiroCredentials::from_json(r#"{"rateLimitAutoDisableEnabled":false}"#).unwrap();
        assert!(!parsed.rate_limit_auto_disable_enabled());

        let serialized = serde_json::to_value(&parsed).unwrap();
        assert_eq!(serialized["rateLimitAutoDisableEnabled"], false);
    }

    #[test]
    fn test_external_idp_auth_method_is_preserved_for_protocol_headers() {
        let json = r#"{
            "refreshToken": "test_refresh",
            "authMethod": "Enterprise",
            "clientId": "client123",
            "clientSecret": "secret456",
            "provider": "Enterprise"
        }"#;
        let mut creds = KiroCredentials::from_json(json).unwrap();
        creds.canonicalize_auth_method();
        assert_eq!(creds.auth_method.as_deref(), Some("external_idp"));
        assert_eq!(creds.provider.as_deref(), Some("Enterprise"));
        assert!(creds.is_external_idp_refresh_credential());
        assert!(!creds.is_idc_refresh_credential());
    }

    #[test]
    fn test_external_idp_alias_auth_methods_are_canonicalized() {
        for auth_method in [
            "external-idp",
            "external IDP",
            "externalidp",
            "iam_sso",
            "IAMSSO",
            "aws-idc",
            "AWS_IDC",
            "Internal",
        ] {
            let mut creds = KiroCredentials {
                auth_method: Some(auth_method.to_string()),
                refresh_token: Some("test_refresh".to_string()),
                client_id: Some("client123".to_string()),
                client_secret: Some("secret456".to_string()),
                ..Default::default()
            };

            creds.canonicalize_auth_method();
            assert_eq!(creds.auth_method.as_deref(), Some("external_idp"));
            assert!(creds.is_external_idp_refresh_credential());
            assert!(!creds.is_idc_refresh_credential());
        }
    }

    #[test]
    fn test_external_idp_import_fields_and_expired_alias() {
        let json = r#"{
            "access_token": "access",
            "refresh_token": "refresh",
            "auth_method": "external_idp",
            "client_id": "client123",
            "profile_arn": "arn:aws:codewhisperer:us-east-1:123456789012:profile/REAL",
            "expired": "2026-06-27T07:49:39Z",
            "token_endpoint": "https://login.example.com/oauth2/v2.0/token",
            "issuer_url": "https://login.example.com/tenant/v2.0",
            "scopes": "api://client123/codewhisperer:conversations offline_access"
        }"#;

        let mut creds = KiroCredentials::from_json(json).unwrap();
        creds.canonicalize_auth_method();

        assert_eq!(creds.auth_method.as_deref(), Some("external_idp"));
        assert_eq!(creds.expires_at.as_deref(), Some("2026-06-27T07:49:39Z"));
        assert_eq!(
            creds.token_endpoint.as_deref(),
            Some("https://login.example.com/oauth2/v2.0/token")
        );
        assert_eq!(
            creds.scopes.as_deref(),
            Some("api://client123/codewhisperer:conversations offline_access")
        );
        assert!(creds.is_external_idp_refresh_credential());
        assert!(!creds.is_idc_refresh_credential());
    }

    #[test]
    fn test_external_idp_defaults_are_derived_from_issuer_url_without_client_secret() {
        let mut creds = KiroCredentials {
            auth_method: Some("external_idp".to_string()),
            refresh_token: Some("1.example-refresh-token".to_string()),
            client_id: Some("client-123".to_string()),
            issuer_url: Some("https://login.microsoftonline.com/tenant-abc/v2.0".to_string()),
            ..Default::default()
        };

        creds.normalize_external_idp_defaults();

        assert_eq!(creds.auth_method.as_deref(), Some("external_idp"));
        assert_eq!(
            creds.token_endpoint.as_deref(),
            Some("https://login.microsoftonline.com/tenant-abc/oauth2/v2.0/token")
        );
        assert_eq!(
            creds.scopes.as_deref(),
            Some(
                "api://client-123/codewhisperer:conversations api://client-123/codewhisperer:completions offline_access"
            )
        );
        assert!(creds.client_secret.is_none());
        assert!(creds.is_external_idp_refresh_credential());
        assert!(!creds.is_idc_refresh_credential());
    }

    #[test]
    fn test_external_idp_defaults_are_derived_from_access_token_issuer() {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD
            .encode(br#"{"iss":"https://login.microsoftonline.com/tenant-from-token/v2.0"}"#);
        let mut creds = KiroCredentials {
            auth_method: Some("external_idp".to_string()),
            access_token: Some(format!("{header}.{payload}.")),
            refresh_token: Some("1.example-refresh-token".to_string()),
            client_id: Some("client-456".to_string()),
            ..Default::default()
        };

        creds.normalize_external_idp_defaults();

        assert_eq!(
            creds.token_endpoint.as_deref(),
            Some("https://login.microsoftonline.com/tenant-from-token/oauth2/v2.0/token")
        );
        assert_eq!(
            creds.scopes.as_deref(),
            Some(
                "api://client-456/codewhisperer:conversations api://client-456/codewhisperer:completions offline_access"
            )
        );
        assert!(creds.client_secret.is_none());
    }

    #[test]
    fn test_external_idp_defaults_fall_back_to_microsoft_common_for_msal_refresh_token() {
        let mut creds = KiroCredentials {
            auth_method: Some("external_idp".to_string()),
            refresh_token: Some("1.msal-refresh-token-without-issuer".to_string()),
            client_id: Some("client-789".to_string()),
            ..Default::default()
        };

        creds.normalize_external_idp_defaults();

        assert_eq!(
            creds.token_endpoint.as_deref(),
            Some("https://login.microsoftonline.com/common/oauth2/v2.0/token")
        );
        assert_eq!(
            creds.scopes.as_deref(),
            Some(
                "api://client-789/codewhisperer:conversations api://client-789/codewhisperer:completions offline_access"
            )
        );
        assert!(creds.client_secret.is_none());
    }

    #[test]
    fn test_api_key_auth_method_canonicalization_does_not_trip_oidc_refresh() {
        let mut creds = KiroCredentials {
            auth_method: Some("API KEY".to_string()),
            kiro_api_key: Some("ksk_test_key".to_string()),
            provider: Some("Enterprise".to_string()),
            profile_arn: Some("arn:aws:codewhisperer:us-east-1:123:profile/STALE".to_string()),
            ..Default::default()
        };

        creds.canonicalize_auth_method();
        assert_eq!(creds.auth_method.as_deref(), Some("api_key"));
        assert!(creds.is_api_key_credential());
        assert!(!creds.is_idc_refresh_credential());
    }

    #[test]
    fn test_api_key_pipe_region_normalizes_to_cli_endpoint_and_regions() {
        let mut creds = KiroCredentials {
            auth_method: Some("API KEY".to_string()),
            kiro_api_key: Some("ksk_test_key|eu-central-1".to_string()),
            endpoint: None,
            ..Default::default()
        };

        creds.normalize_api_key_defaults();

        assert_eq!(creds.auth_method.as_deref(), Some("api_key"));
        assert_eq!(creds.kiro_api_key.as_deref(), Some("ksk_test_key"));
        assert_eq!(creds.region.as_deref(), Some("eu-central-1"));
        assert_eq!(creds.auth_region.as_deref(), Some("eu-central-1"));
        assert_eq!(creds.api_region.as_deref(), Some("eu-central-1"));
        assert_eq!(
            creds.endpoint.as_deref(),
            Some(KIRO_API_KEY_DEFAULT_ENDPOINT)
        );
        assert!(creds.refresh_token.is_none());
        assert!(creds.profile_arn.is_none());
        assert!(creds.client_id.is_none());
    }

    #[test]
    fn test_api_key_normalization_preserves_explicit_endpoint_and_api_region() {
        let mut creds = KiroCredentials {
            auth_method: Some("api_key".to_string()),
            kiro_api_key: Some("ksk_test_key|eu-central-1".to_string()),
            region: Some("us-east-1".to_string()),
            api_region: Some("us-west-2".to_string()),
            endpoint: Some("ide".to_string()),
            ..Default::default()
        };

        creds.normalize_api_key_defaults();

        assert_eq!(creds.region.as_deref(), Some("us-east-1"));
        assert_eq!(creds.auth_region.as_deref(), Some("eu-central-1"));
        assert_eq!(creds.api_region.as_deref(), Some("us-west-2"));
        assert_eq!(creds.endpoint.as_deref(), Some("ide"));
    }

    #[test]
    fn test_credentials_config_plain_text_api_key_with_region() {
        let path = std::env::temp_dir().join(format!(
            "kiro-rs-credentials-api-key-{}.txt",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &path,
            "ksk_first|eu-central-1\n# comment\nksk_second|us-east-1\n",
        )
        .unwrap();

        let config = CredentialsConfig::load(&path).unwrap();
        let credentials = config.into_sorted_credentials();

        assert_eq!(credentials.len(), 2);
        assert_eq!(credentials[0].auth_method.as_deref(), Some("api_key"));
        assert_eq!(credentials[0].kiro_api_key.as_deref(), Some("ksk_first"));
        assert_eq!(credentials[0].api_region.as_deref(), Some("eu-central-1"));
        assert_eq!(credentials[0].endpoint.as_deref(), Some("cli"));
        assert_eq!(credentials[1].kiro_api_key.as_deref(), Some("ksk_second"));
        assert_eq!(credentials[1].api_region.as_deref(), Some("us-east-1"));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_credentials_config_single() {
        let json = r#"{"refreshToken": "test", "expiresAt": "2025-12-31T00:00:00Z"}"#;
        let config: CredentialsConfig = serde_json::from_str(json).unwrap();
        assert!(matches!(config, CredentialsConfig::Single(_)));
    }

    #[test]
    fn test_credentials_config_multiple() {
        let json = r#"[
            {"refreshToken": "test1", "priority": 1},
            {"refreshToken": "test2", "priority": 0}
        ]"#;
        let config: CredentialsConfig = serde_json::from_str(json).unwrap();
        assert!(matches!(config, CredentialsConfig::Multiple(_)));
        assert_eq!(config.into_sorted_credentials().len(), 2);
    }

    #[test]
    fn test_credentials_config_priority_sorting() {
        let json = r#"[
            {"refreshToken": "t1", "priority": 2},
            {"refreshToken": "t2", "priority": 0},
            {"refreshToken": "t3", "priority": 1}
        ]"#;
        let config: CredentialsConfig = serde_json::from_str(json).unwrap();
        let list = config.into_sorted_credentials();

        // 验证按优先级排序
        assert_eq!(list[0].refresh_token, Some("t2".to_string())); // priority 0
        assert_eq!(list[1].refresh_token, Some("t3".to_string())); // priority 1
        assert_eq!(list[2].refresh_token, Some("t1".to_string())); // priority 2
    }

    // ============ Region 字段测试 ============

    #[test]
    fn test_region_field_parsing() {
        // 测试解析包含 region 字段的 JSON
        let json = r#"{
            "refreshToken": "test_refresh",
            "region": "us-east-1"
        }"#;

        let creds = KiroCredentials::from_json(json).unwrap();
        assert_eq!(creds.refresh_token, Some("test_refresh".to_string()));
        assert_eq!(creds.region, Some("us-east-1".to_string()));
    }

    #[test]
    fn test_region_field_missing_backward_compat() {
        // 测试向后兼容：不包含 region 字段的旧格式 JSON
        let json = r#"{
            "refreshToken": "test_refresh",
            "authMethod": "social"
        }"#;

        let creds = KiroCredentials::from_json(json).unwrap();
        assert_eq!(creds.refresh_token, Some("test_refresh".to_string()));
        assert_eq!(creds.region, None);
    }

    #[test]
    fn test_region_field_serialization() {
        let creds = KiroCredentials {
            id: None,
            refresh_token: Some("test".to_string()),
            region: Some("eu-west-1".to_string()),
            ..Default::default()
        };

        let json = creds.to_pretty_json().unwrap();
        assert!(json.contains("region"));
        assert!(json.contains("eu-west-1"));
    }

    #[test]
    fn test_region_field_none_not_serialized() {
        let creds = KiroCredentials {
            id: None,
            refresh_token: Some("test".to_string()),
            ..Default::default()
        };

        let json = creds.to_pretty_json().unwrap();
        assert!(!json.contains("region"));
    }

    // ============ MachineId 字段测试 ============

    #[test]
    fn test_machine_id_field_parsing() {
        let machine_id = "a".repeat(64);
        let json = format!(
            r#"{{
                "refreshToken": "test_refresh",
                "machineId": "{machine_id}"
            }}"#
        );

        let creds = KiroCredentials::from_json(&json).unwrap();
        assert_eq!(creds.refresh_token, Some("test_refresh".to_string()));
        assert_eq!(creds.machine_id, Some(machine_id));
    }

    #[test]
    fn test_machine_id_field_serialization() {
        let mut creds = KiroCredentials::default();
        creds.refresh_token = Some("test".to_string());
        creds.machine_id = Some("b".repeat(64));

        let json = creds.to_pretty_json().unwrap();
        assert!(json.contains("machineId"));
    }

    #[test]
    fn test_machine_id_field_none_not_serialized() {
        let mut creds = KiroCredentials::default();
        creds.refresh_token = Some("test".to_string());
        creds.machine_id = None;

        let json = creds.to_pretty_json().unwrap();
        assert!(!json.contains("machineId"));
    }

    #[test]
    fn test_multiple_credentials_with_different_regions() {
        // 测试多凭据场景下不同凭据使用各自的 region
        let json = r#"[
            {"refreshToken": "t1", "region": "us-east-1"},
            {"refreshToken": "t2", "region": "eu-west-1"},
            {"refreshToken": "t3"}
        ]"#;

        let config: CredentialsConfig = serde_json::from_str(json).unwrap();
        let list = config.into_sorted_credentials();

        assert_eq!(list[0].region, Some("us-east-1".to_string()));
        assert_eq!(list[1].region, Some("eu-west-1".to_string()));
        assert_eq!(list[2].region, None);
    }

    #[test]
    fn test_region_field_with_all_fields() {
        // 测试包含所有字段的完整 JSON
        let json = r#"{
            "id": 1,
            "accessToken": "access",
            "refreshToken": "refresh",
            "profileArn": "arn:aws:test",
            "expiresAt": "2025-12-31T00:00:00Z",
            "authMethod": "idc",
            "clientId": "client123",
            "clientSecret": "secret456",
            "priority": 5,
            "region": "ap-northeast-1"
        }"#;

        let creds = KiroCredentials::from_json(json).unwrap();
        assert_eq!(creds.id, Some(1));
        assert_eq!(creds.access_token, Some("access".to_string()));
        assert_eq!(creds.refresh_token, Some("refresh".to_string()));
        assert_eq!(creds.profile_arn, Some("arn:aws:test".to_string()));
        assert_eq!(creds.expires_at, Some("2025-12-31T00:00:00Z".to_string()));
        assert_eq!(creds.auth_method, Some("idc".to_string()));
        assert_eq!(creds.client_id, Some("client123".to_string()));
        assert_eq!(creds.client_secret, Some("secret456".to_string()));
        assert_eq!(creds.priority, 5);
        assert_eq!(creds.region, Some("ap-northeast-1".to_string()));
    }

    #[test]
    fn test_region_roundtrip() {
        // 测试序列化和反序列化的往返一致性
        let original = KiroCredentials {
            id: Some(42),
            access_token: Some("token".to_string()),
            refresh_token: Some("refresh".to_string()),
            auth_method: Some("social".to_string()),
            priority: 3,
            region: Some("us-west-2".to_string()),
            machine_id: Some("c".repeat(64)),
            ..Default::default()
        };

        let json = original.to_pretty_json().unwrap();
        let parsed = KiroCredentials::from_json(&json).unwrap();

        assert_eq!(parsed.id, original.id);
        assert_eq!(parsed.access_token, original.access_token);
        assert_eq!(parsed.refresh_token, original.refresh_token);
        assert_eq!(parsed.priority, original.priority);
        assert_eq!(parsed.region, original.region);
        assert_eq!(parsed.machine_id, original.machine_id);
    }

    // ============ auth_region / api_region 字段测试 ============

    #[test]
    fn test_auth_region_field_parsing() {
        let json = r#"{
            "refreshToken": "test_refresh",
            "authRegion": "eu-central-1"
        }"#;
        let creds = KiroCredentials::from_json(json).unwrap();
        assert_eq!(creds.auth_region, Some("eu-central-1".to_string()));
        assert_eq!(creds.api_region, None);
    }

    #[test]
    fn test_api_region_field_parsing() {
        let json = r#"{
            "refreshToken": "test_refresh",
            "apiRegion": "ap-southeast-1"
        }"#;
        let creds = KiroCredentials::from_json(json).unwrap();
        assert_eq!(creds.api_region, Some("ap-southeast-1".to_string()));
        assert_eq!(creds.auth_region, None);
    }

    #[test]
    fn test_auth_api_region_serialization() {
        let mut creds = KiroCredentials::default();
        creds.refresh_token = Some("test".to_string());
        creds.auth_region = Some("eu-west-1".to_string());
        creds.api_region = Some("us-west-2".to_string());

        let json = creds.to_pretty_json().unwrap();
        assert!(json.contains("authRegion"));
        assert!(json.contains("eu-west-1"));
        assert!(json.contains("apiRegion"));
        assert!(json.contains("us-west-2"));
    }

    #[test]
    fn test_auth_api_region_none_not_serialized() {
        let mut creds = KiroCredentials::default();
        creds.refresh_token = Some("test".to_string());
        creds.auth_region = None;
        creds.api_region = None;

        let json = creds.to_pretty_json().unwrap();
        assert!(!json.contains("authRegion"));
        assert!(!json.contains("apiRegion"));
    }

    #[test]
    fn test_auth_api_region_roundtrip() {
        let mut original = KiroCredentials::default();
        original.refresh_token = Some("refresh".to_string());
        original.region = Some("us-east-1".to_string());
        original.auth_region = Some("eu-west-1".to_string());
        original.api_region = Some("ap-northeast-1".to_string());

        let json = original.to_pretty_json().unwrap();
        let parsed = KiroCredentials::from_json(&json).unwrap();

        assert_eq!(parsed.region, original.region);
        assert_eq!(parsed.auth_region, original.auth_region);
        assert_eq!(parsed.api_region, original.api_region);
    }

    #[test]
    fn test_backward_compat_no_auth_api_region() {
        // 旧格式 JSON 不包含 authRegion/apiRegion，应正常解析
        let json = r#"{
            "refreshToken": "test_refresh",
            "region": "us-east-1"
        }"#;
        let creds = KiroCredentials::from_json(json).unwrap();
        assert_eq!(creds.region, Some("us-east-1".to_string()));
        assert_eq!(creds.auth_region, None);
        assert_eq!(creds.api_region, None);
    }

    // ============ effective_auth_region / effective_api_region 优先级测试 ============

    #[test]
    fn test_effective_auth_region_credential_auth_region_highest() {
        // 凭据.auth_region > 凭据.region > config.auth_region > config.region
        let mut config = Config::default();
        config.region = "config-region".to_string();
        config.auth_region = Some("config-auth-region".to_string());

        let mut creds = KiroCredentials::default();
        creds.region = Some("cred-region".to_string());
        creds.auth_region = Some("cred-auth-region".to_string());

        assert_eq!(creds.effective_auth_region(&config), "cred-auth-region");
    }

    #[test]
    fn test_effective_auth_region_fallback_to_credential_region() {
        let mut config = Config::default();
        config.region = "config-region".to_string();
        config.auth_region = Some("config-auth-region".to_string());

        let mut creds = KiroCredentials::default();
        creds.region = Some("cred-region".to_string());
        // auth_region 未设置

        assert_eq!(creds.effective_auth_region(&config), "cred-region");
    }

    #[test]
    fn test_effective_auth_region_fallback_to_config_auth_region() {
        let mut config = Config::default();
        config.region = "config-region".to_string();
        config.auth_region = Some("config-auth-region".to_string());

        let creds = KiroCredentials::default();
        // auth_region 和 region 均未设置

        assert_eq!(creds.effective_auth_region(&config), "config-auth-region");
    }

    #[test]
    fn test_effective_auth_region_fallback_to_config_region() {
        let mut config = Config::default();
        config.region = "config-region".to_string();
        // config.auth_region 未设置

        let creds = KiroCredentials::default();

        assert_eq!(creds.effective_auth_region(&config), "config-region");
    }

    #[test]
    fn test_effective_api_region_credential_api_region_highest() {
        // 凭据.api_region > profileArn region > config.api_region > config.region
        let mut config = Config::default();
        config.region = "config-region".to_string();
        config.api_region = Some("config-api-region".to_string());

        let mut creds = KiroCredentials::default();
        creds.api_region = Some("cred-api-region".to_string());
        creds.profile_arn =
            Some("arn:aws:codewhisperer:eu-central-1:123456789012:profile/FAKE".to_string());

        assert_eq!(creds.effective_api_region(&config), "cred-api-region");
    }

    #[test]
    fn test_effective_api_region_fallback_to_profile_arn_region() {
        let mut config = Config::default();
        config.region = "config-region".to_string();
        config.api_region = Some("config-api-region".to_string());

        let mut creds = KiroCredentials::default();
        creds.profile_arn =
            Some("arn:aws:codewhisperer:eu-central-1:123456789012:profile/FAKE".to_string());

        assert_eq!(creds.effective_api_region(&config), "eu-central-1");
    }

    #[test]
    fn test_effective_api_region_fallback_to_config_api_region() {
        let mut config = Config::default();
        config.region = "config-region".to_string();
        config.api_region = Some("config-api-region".to_string());

        let creds = KiroCredentials::default();

        assert_eq!(creds.effective_api_region(&config), "config-api-region");
    }

    #[test]
    fn test_effective_api_region_fallback_to_config_region() {
        let mut config = Config::default();
        config.region = "config-region".to_string();

        let creds = KiroCredentials::default();

        assert_eq!(creds.effective_api_region(&config), "config-region");
    }

    #[test]
    fn test_effective_api_region_ignores_credential_region() {
        // 凭据.region 不参与 api_region 的回退链
        let mut config = Config::default();
        config.region = "config-region".to_string();

        let mut creds = KiroCredentials::default();
        creds.region = Some("cred-region".to_string());

        assert_eq!(creds.effective_api_region(&config), "config-region");
    }

    #[test]
    fn test_auth_and_api_region_independent() {
        // auth_region 和 api_region 互不影响
        let mut config = Config::default();
        config.region = "default".to_string();

        let mut creds = KiroCredentials::default();
        creds.auth_region = Some("auth-only".to_string());
        creds.api_region = Some("api-only".to_string());

        assert_eq!(creds.effective_auth_region(&config), "auth-only");
        assert_eq!(creds.effective_api_region(&config), "api-only");
    }

    // ============ 凭据级代理优先级测试 ============

    #[test]
    fn test_effective_proxy_credential_overrides_global() {
        let global = ProxyConfig::new("http://global:8080");
        let mut creds = KiroCredentials::default();
        creds.proxy_url = Some("socks5://cred:1080".to_string());

        let result = creds.effective_proxy(Some(&global));
        assert_eq!(result, Some(ProxyConfig::new("socks5://cred:1080")));
    }

    #[test]
    fn test_effective_proxy_credential_with_auth() {
        let global = ProxyConfig::new("http://global:8080");
        let mut creds = KiroCredentials::default();
        creds.proxy_url = Some("http://proxy:3128".to_string());
        creds.proxy_username = Some("user".to_string());
        creds.proxy_password = Some("pass".to_string());

        let result = creds.effective_proxy(Some(&global));
        let expected = ProxyConfig::new("http://proxy:3128").with_auth("user", "pass");
        assert_eq!(result, Some(expected));
    }

    #[test]
    fn test_effective_proxy_direct_bypasses_global() {
        let global = ProxyConfig::new("http://global:8080");
        let mut creds = KiroCredentials::default();
        creds.proxy_url = Some("direct".to_string());

        let result = creds.effective_proxy(Some(&global));
        assert_eq!(result, None);
    }

    #[test]
    fn test_effective_proxy_direct_case_insensitive() {
        let global = ProxyConfig::new("http://global:8080");
        let mut creds = KiroCredentials::default();
        creds.proxy_url = Some("DIRECT".to_string());

        let result = creds.effective_proxy(Some(&global));
        assert_eq!(result, None);
    }

    #[test]
    fn test_effective_proxy_fallback_to_global() {
        let global = ProxyConfig::new("http://global:8080");
        let creds = KiroCredentials::default();

        let result = creds.effective_proxy(Some(&global));
        assert_eq!(result, Some(ProxyConfig::new("http://global:8080")));
    }

    #[test]
    fn test_effective_proxy_none_when_no_proxy() {
        let creds = KiroCredentials::default();
        let result = creds.effective_proxy(None);
        assert_eq!(result, None);
    }
}
