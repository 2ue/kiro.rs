use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TlsBackend {
    Rustls,
    NativeTls,
}

impl Default for TlsBackend {
    fn default() -> Self {
        Self::Rustls
    }
}

/// 路径级 prompt-cache usage 模式。
///
/// 该枚举仍作为内部请求链路标记使用；外部配置不再选择缓存模式：
/// `/v1`、`/cc/v1`、`/ha/v1`、`/na/v1` 消息路径固定使用 high-cache；
/// `/na` 默认通过路径级上报策略关闭本地模拟 cache usage 补足。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PromptCacheSimulationMode {
    Disabled,
    #[serde(alias = "local-prompt-cache")]
    HighCache,
}

impl Default for PromptCacheSimulationMode {
    fn default() -> Self {
        Self::HighCache
    }
}

/// 上游请求压缩配置。
///
/// 默认总开关关闭；如果启用，默认只执行低风险的 whitespace 压缩。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CompressionConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default = "default_true")]
    pub whitespace_compression: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ReportedUsageFieldMode {
    Preserve,
    SampleMax,
    SampleTarget,
}

impl Default for ReportedUsageFieldMode {
    fn default() -> Self {
        Self::Preserve
    }
}

/// 单个 usage 字段的下游上报策略。
///
/// 这些策略只用于把内部计算出的 usage 投影成下游响应和后台记录看到的 usage；
/// 不参与 prompt-cache tracker、reader 命中或上游请求计算。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReportedUsageFieldPolicy {
    #[serde(default)]
    pub mode: ReportedUsageFieldMode,

    /// `sample-max` 的上限。
    #[serde(default)]
    pub max_tokens: i32,

    /// `sample-target` 的中心目标值。
    #[serde(default)]
    pub target_tokens: i32,

    /// `sample-target` 的常规最大倍率。writer 默认 1.1，即约 110%。
    #[serde(default = "default_reported_usage_normal_max_multiplier")]
    pub normal_max_multiplier: f64,

    /// input 被压低后的差值是否转入 cache_read_input_tokens。
    #[serde(default)]
    pub move_delta_to_cache_read: bool,
}

impl Default for ReportedUsageFieldPolicy {
    fn default() -> Self {
        Self {
            mode: ReportedUsageFieldMode::Preserve,
            max_tokens: 0,
            target_tokens: 0,
            normal_max_multiplier: default_reported_usage_normal_max_multiplier(),
            move_delta_to_cache_read: false,
        }
    }
}

impl ReportedUsageFieldPolicy {
    pub fn preserve() -> Self {
        Self::default()
    }

    pub fn sample_max(max_tokens: i32) -> Self {
        Self {
            mode: ReportedUsageFieldMode::SampleMax,
            max_tokens,
            ..Self::default()
        }
    }

    pub fn sample_input_max(max_tokens: i32) -> Self {
        Self {
            move_delta_to_cache_read: true,
            ..Self::sample_max(max_tokens)
        }
    }

    pub fn sample_target(target_tokens: i32) -> Self {
        Self {
            mode: ReportedUsageFieldMode::SampleTarget,
            target_tokens,
            ..Self::default()
        }
    }

    pub fn normalized(&self) -> Self {
        let mut normalized = self.clone();
        normalized.max_tokens = normalized.max_tokens.max(0);
        normalized.target_tokens = normalized.target_tokens.max(0);
        normalized.normal_max_multiplier =
            normalize_reported_usage_normal_max_multiplier(normalized.normal_max_multiplier);
        normalized
    }

    fn validate(&self, label: &str) -> Result<(), String> {
        if self.max_tokens < 0 {
            return Err(format!("{} 的 sample-max 上限不能小于 0", label));
        }
        if self.target_tokens < 0 {
            return Err(format!("{} 的 sample-target 目标值不能小于 0", label));
        }
        if !self.normal_max_multiplier.is_finite() || self.normal_max_multiplier < 1.0 {
            return Err(format!(
                "{} 的 sample-target 最大倍率必须大于或等于 1",
                label
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReportedUsagePathPolicy {
    #[serde(default = "default_true")]
    pub enabled: bool,

    #[serde(default)]
    pub input: ReportedUsageFieldPolicy,

    #[serde(default)]
    pub output: ReportedUsageFieldPolicy,

    #[serde(default)]
    pub cache_read: ReportedUsageFieldPolicy,

    #[serde(default)]
    pub cache_creation: ReportedUsageFieldPolicy,
}

impl Default for ReportedUsagePathPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            input: ReportedUsageFieldPolicy::preserve(),
            output: ReportedUsageFieldPolicy::preserve(),
            cache_read: ReportedUsageFieldPolicy::preserve(),
            cache_creation: ReportedUsageFieldPolicy::preserve(),
        }
    }
}

impl ReportedUsagePathPolicy {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }

    pub fn normalized(&self) -> Self {
        Self {
            enabled: self.enabled,
            input: self.input.normalized(),
            output: self.output.normalized(),
            cache_read: self.cache_read.normalized(),
            cache_creation: self.cache_creation.normalized(),
        }
    }

    fn validate(&self, label: &str) -> Result<(), String> {
        self.input.validate(&format!("{} input", label))?;
        self.output.validate(&format!("{} output", label))?;
        self.cache_read.validate(&format!("{} cacheRead", label))?;
        self.cache_creation
            .validate(&format!("{} cacheCreation", label))?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReportedUsageConfig {
    #[serde(default)]
    pub default: ReportedUsagePathPolicy,

    #[serde(default)]
    pub path_overrides: BTreeMap<String, ReportedUsagePathPolicy>,
}

impl Default for ReportedUsageConfig {
    fn default() -> Self {
        let mut path_overrides = BTreeMap::new();
        path_overrides.insert("/na".to_string(), ReportedUsagePathPolicy::disabled());
        path_overrides.insert(
            "/cc".to_string(),
            ReportedUsagePathPolicy {
                input: ReportedUsageFieldPolicy::sample_input_max(96),
                cache_creation: ReportedUsageFieldPolicy::sample_target(3_000),
                ..ReportedUsagePathPolicy::default()
            },
        );
        path_overrides.insert(
            "/ha".to_string(),
            ReportedUsagePathPolicy {
                input: ReportedUsageFieldPolicy::sample_input_max(96),
                ..ReportedUsagePathPolicy::default()
            },
        );

        Self {
            default: ReportedUsagePathPolicy::default(),
            path_overrides,
        }
    }
}

impl ReportedUsageConfig {
    pub fn normalized(&self) -> Self {
        let path_overrides = self
            .path_overrides
            .iter()
            .filter_map(|(prefix, policy)| {
                normalize_reported_usage_path_prefix(prefix)
                    .map(|prefix| (prefix, policy.normalized()))
            })
            .collect();

        Self {
            default: self.default.normalized(),
            path_overrides,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        self.default.validate("默认上报策略")?;
        for (prefix, policy) in &self.path_overrides {
            let Some(normalized_prefix) = normalize_reported_usage_path_prefix(prefix) else {
                return Err("路径覆盖前缀不能为空".to_string());
            };
            policy.validate(&format!("路径 {} 上报策略", normalized_prefix))?;
        }
        Ok(())
    }

    pub fn policy_for_path(&self, path: &str) -> ReportedUsagePathPolicy {
        let normalized = self.normalized();
        normalized
            .path_overrides
            .iter()
            .filter(|(prefix, _)| reported_usage_path_matches(prefix, path))
            .max_by_key(|(prefix, _)| prefix.len())
            .map(|(_, policy)| policy.clone())
            .unwrap_or(normalized.default)
    }
}

impl Default for CompressionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            whitespace_compression: true,
        }
    }
}

/// Anthropic compatibility profile.
///
/// `claude-code` keeps the pragmatic rewrites needed by Claude Code CLI and
/// the Kiro upstream. `anthropic-strict` minimizes synthetic protocol and
/// prompt rewrites for detector-style checks. `debug` follows `claude-code`
/// behavior but exposes proxy warning headers by default.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CompatProfile {
    ClaudeCode,
    AnthropicStrict,
    Debug,
}

impl Default for CompatProfile {
    fn default() -> Self {
        Self::ClaudeCode
    }
}

impl CompatProfile {
    pub fn is_strict(self) -> bool {
        matches!(self, Self::AnthropicStrict)
    }

    pub fn is_debug(self) -> bool {
        matches!(self, Self::Debug)
    }

    pub fn allows_unsigned_thinking(self) -> bool {
        !self.is_strict()
    }
}

/// KNA 应用配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default = "default_host")]
    pub host: String,

    #[serde(default = "default_port")]
    pub port: u16,

    #[serde(default = "default_region")]
    pub region: String,

    /// Auth Region（用于 Token 刷新），未配置时回退到 region
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_region: Option<String>,

    /// API Region（用于 API 请求），未配置时回退到 region
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_region: Option<String>,

    #[serde(default = "default_kiro_version")]
    pub kiro_version: String,

    #[serde(default)]
    pub machine_id: Option<String>,

    #[serde(default)]
    pub api_key: Option<String>,

    #[serde(default = "default_system_version")]
    pub system_version: String,

    #[serde(default = "default_node_version")]
    pub node_version: String,

    #[serde(default = "default_tls_backend")]
    pub tls_backend: TlsBackend,

    /// 外部 count_tokens API 地址（可选）
    #[serde(default)]
    pub count_tokens_api_url: Option<String>,

    /// count_tokens API 密钥（可选）
    #[serde(default)]
    pub count_tokens_api_key: Option<String>,

    /// count_tokens API 认证类型（可选，"x-api-key" 或 "bearer"，默认 "x-api-key"）
    #[serde(default = "default_count_tokens_auth_type")]
    pub count_tokens_auth_type: String,

    /// HTTP 代理地址（可选）
    /// 支持格式: http://host:port, https://host:port, socks5://host:port
    #[serde(default)]
    pub proxy_url: Option<String>,

    /// 代理认证用户名（可选）
    #[serde(default)]
    pub proxy_username: Option<String>,

    /// 代理认证密码（可选）
    #[serde(default)]
    pub proxy_password: Option<String>,

    /// Admin API 密钥（可选，启用 Admin API 功能）
    #[serde(default)]
    pub admin_api_key: Option<String>,

    /// 单凭据目标请求速率（RPM）。
    ///
    /// `None` 或 `0` 表示禁用本地凭据级限速；`>0` 会按每个凭据计算最小请求间隔，
    /// 并在调度时优先分流到其他可用凭据。
    #[serde(default)]
    pub credential_rpm: Option<u32>,

    /// 单凭据最大并发请求数。
    ///
    /// `0` 表示不限制；`>0` 时，同一凭据同时在处理的请求达到上限后，
    /// 新请求会优先调度到其他可用凭据。
    #[serde(default)]
    pub credential_max_concurrent_requests: u32,

    /// 上游瞬态错误没有 Retry-After 时，对单个凭据设置的临时冷却秒数。
    #[serde(default = "default_credential_transient_cooldown_secs")]
    pub credential_transient_cooldown_secs: u64,

    /// 单个凭据临时冷却最长秒数，用于限制 Retry-After 头的影响范围。
    #[serde(default = "default_credential_max_cooldown_secs")]
    pub credential_max_cooldown_secs: u64,

    /// 单个请求等待凭据可调度的最长秒数。
    ///
    /// `0` 表示不限制等待时间；`>0` 时，如果所有可用凭据持续处于冷却、
    /// 本地限流或并发占满状态，超过该时间后返回明确的本地调度限流错误。
    #[serde(default = "default_credential_dispatch_max_wait_secs")]
    pub credential_dispatch_max_wait_secs: u64,

    /// 并发占用 lease 的最大存活秒数。
    ///
    /// `0` 表示不自动回收；`>0` 时，调度前会清理超过该时间仍未释放的占用，
    /// 避免异常路径导致某个凭据永久被视为并发占满。
    #[serde(default = "default_credential_in_flight_lease_max_secs")]
    pub credential_in_flight_lease_max_secs: u64,

    /// 新凭据预热请求次数。预热期内 balanced 会降低该凭据调度权重，但不会伪造 success_count。
    #[serde(default = "default_credential_warmup_requests")]
    pub credential_warmup_requests: u32,

    /// balanced 模式下预热凭据参与真实业务请求调度的概率百分比。
    #[serde(default = "default_credential_warmup_selection_percent")]
    pub credential_warmup_selection_percent: u32,

    /// 是否持久化凭据文件变更（Token 刷新、禁用、优先级等）。
    #[serde(default = "default_credentials_persist")]
    pub credentials_persist: bool,

    /// 是否持久化调度统计缓存（success_count、last_used_at）。
    #[serde(default = "default_credential_stats_persist")]
    pub credential_stats_persist: bool,

    /// 输入压缩配置。默认不启用；启用后默认只做 whitespace 压缩。
    #[serde(default)]
    pub compression: CompressionConfig,

    /// 负载均衡模式（"priority" 或 "balanced"）
    #[serde(default = "default_load_balancing_mode")]
    pub load_balancing_mode: String,

    /// Anthropic 兼容 profile（默认 claude-code）。
    #[serde(default = "default_compat_profile")]
    pub compat_profile: CompatProfile,

    /// 是否开启非流式响应的 thinking 块提取（默认 true）
    ///
    /// 启用后，非流式响应中的 `<thinking>...</thinking>` 标签会被解析为
    /// 独立的 `{"type": "thinking", ...}` 内容块,与流式响应行为一致。
    #[serde(default = "default_extract_thinking")]
    pub extract_thinking: bool,

    /// 本地 prompt-cache 模拟的目标 cache read 中心比例。
    ///
    /// 对 high-cache 生效；读取仍必须命中同一凭据、
    /// 会话、模型下已创建过的缓存前缀，不会凭空制造 cache read。实际比例会
    /// 围绕该值做小范围确定性浮动，避免每次都精确落在同一个百分比。
    #[serde(default = "default_prompt_cache_target_read_ratio")]
    pub prompt_cache_target_read_ratio: f64,

    /// high-cache 模拟专用的 total input 放大倍数。
    ///
    /// 只影响本地 high-cache usage 模拟，不影响真实 Kiro metadata cache。
    #[serde(default = "default_prompt_cache_token_scale")]
    pub prompt_cache_token_scale: f64,

    /// high-cache 模拟 total input 的上限。
    ///
    /// 触顶时会按稳定 fingerprint 做确定性 soft-cap 抖动，避免每次固定卡在上限。
    #[serde(default = "default_prompt_cache_max_simulated_input_tokens")]
    pub prompt_cache_max_simulated_input_tokens: i32,

    /// high-cache 模拟触顶 soft-cap 的最小扣减 token。
    #[serde(default = "default_prompt_cache_cap_jitter_min_tokens")]
    pub prompt_cache_cap_jitter_min_tokens: i32,

    /// high-cache 模拟触顶 soft-cap 的最大扣减 token。
    #[serde(default = "default_prompt_cache_cap_jitter_max_tokens")]
    pub prompt_cache_cap_jitter_max_tokens: i32,

    /// 基础输入达到该门槛后才启用 high-cache token scale，避免短测试请求被放大。
    #[serde(default = "default_prompt_cache_scale_min_input_tokens")]
    pub prompt_cache_scale_min_input_tokens: i32,

    /// 下游 usage 上报投影配置。
    ///
    /// 默认策略先应用，再按路径前缀使用最长匹配覆盖；只影响 response usage
    /// 和后台 usage record，不影响 prompt-cache reader 计算、tracker 更新和上游请求。
    #[serde(default)]
    pub reported_usage: ReportedUsageConfig,

    /// 请求级 usage record 内存保留上限。
    #[serde(default = "default_usage_record_limit")]
    pub usage_record_limit: usize,

    /// 是否将 usage record 追加写入 JSONL 文件。
    #[serde(default = "default_usage_record_persist")]
    pub usage_record_persist: bool,

    /// Admin 高缓存请求阈值。
    #[serde(default = "default_high_cache_threshold")]
    pub high_cache_threshold: i32,

    /// 默认端点名称（凭据未显式指定 endpoint 时使用，默认 "ide"）
    #[serde(default = "default_endpoint")]
    pub default_endpoint: String,

    /// 是否在响应头中暴露代理改写动作（默认 false）。
    ///
    /// 启用后，凡涉及消息合并 / 孤立 tool_use|tool_result 清理 / thinking 覆写等
    /// 代理侧的隐式改写都会通过 `x-kiro-rs-warnings` 响应头汇总反馈，便于排查。
    /// 仅写头，不会修改响应体，对客户端无副作用。
    #[serde(default = "default_expose_proxy_warnings")]
    pub expose_proxy_warnings: bool,

    /// 配置文件路径（运行时元数据，不写入 JSON）
    #[serde(skip)]
    config_path: Option<PathBuf>,
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    8080
}

fn default_region() -> String {
    "us-east-1".to_string()
}

fn default_kiro_version() -> String {
    "0.11.107".to_string()
}

fn default_system_version() -> String {
    const SYSTEM_VERSIONS: &[&str] = &["darwin#24.6.0", "win32#10.0.22631"];
    SYSTEM_VERSIONS[fastrand::usize(..SYSTEM_VERSIONS.len())].to_string()
}

fn default_node_version() -> String {
    "22.22.0".to_string()
}

fn default_count_tokens_auth_type() -> String {
    "x-api-key".to_string()
}

fn default_tls_backend() -> TlsBackend {
    TlsBackend::Rustls
}

fn default_load_balancing_mode() -> String {
    "priority".to_string()
}

fn default_credential_transient_cooldown_secs() -> u64 {
    10
}

fn default_credential_max_cooldown_secs() -> u64 {
    300
}

fn default_credential_dispatch_max_wait_secs() -> u64 {
    120
}

fn default_credential_in_flight_lease_max_secs() -> u64 {
    900
}

fn default_credential_warmup_requests() -> u32 {
    3
}

fn default_credential_warmup_selection_percent() -> u32 {
    5
}

fn default_credentials_persist() -> bool {
    true
}

fn default_credential_stats_persist() -> bool {
    true
}

fn default_compat_profile() -> CompatProfile {
    CompatProfile::ClaudeCode
}

fn default_extract_thinking() -> bool {
    true
}

fn default_true() -> bool {
    true
}

fn default_prompt_cache_target_read_ratio() -> f64 {
    0.98
}

fn default_prompt_cache_token_scale() -> f64 {
    1.6
}

fn default_prompt_cache_max_simulated_input_tokens() -> i32 {
    300_000
}

fn default_prompt_cache_cap_jitter_min_tokens() -> i32 {
    12_000
}

fn default_prompt_cache_cap_jitter_max_tokens() -> i32 {
    24_000
}

fn default_prompt_cache_scale_min_input_tokens() -> i32 {
    20_000
}

fn default_usage_record_limit() -> usize {
    5000
}

fn default_usage_record_persist() -> bool {
    true
}

fn default_high_cache_threshold() -> i32 {
    10_000
}

fn default_endpoint() -> String {
    crate::kiro::endpoint::ide::IDE_ENDPOINT_NAME.to_string()
}

fn default_expose_proxy_warnings() -> bool {
    false
}

fn default_reported_usage_normal_max_multiplier() -> f64 {
    1.1
}

fn normalize_reported_usage_normal_max_multiplier(value: f64) -> f64 {
    if value.is_finite() && value >= 1.0 {
        value.min(10.0)
    } else {
        default_reported_usage_normal_max_multiplier()
    }
}

fn normalize_reported_usage_path_prefix(prefix: &str) -> Option<String> {
    let trimmed = prefix.trim();
    if trimmed.is_empty() {
        return None;
    }
    let with_slash = if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{}", trimmed)
    };
    let normalized = with_slash.trim_end_matches('/').to_string();
    Some(if normalized.is_empty() {
        "/".to_string()
    } else {
        normalized
    })
}

fn reported_usage_path_matches(prefix: &str, path: &str) -> bool {
    if prefix == "/" {
        return true;
    }
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            region: default_region(),
            auth_region: None,
            api_region: None,
            kiro_version: default_kiro_version(),
            machine_id: None,
            api_key: None,
            system_version: default_system_version(),
            node_version: default_node_version(),
            tls_backend: default_tls_backend(),
            count_tokens_api_url: None,
            count_tokens_api_key: None,
            count_tokens_auth_type: default_count_tokens_auth_type(),
            proxy_url: None,
            proxy_username: None,
            proxy_password: None,
            admin_api_key: None,
            credential_rpm: None,
            credential_max_concurrent_requests: 0,
            credential_transient_cooldown_secs: default_credential_transient_cooldown_secs(),
            credential_max_cooldown_secs: default_credential_max_cooldown_secs(),
            credential_dispatch_max_wait_secs: default_credential_dispatch_max_wait_secs(),
            credential_in_flight_lease_max_secs: default_credential_in_flight_lease_max_secs(),
            credential_warmup_requests: default_credential_warmup_requests(),
            credential_warmup_selection_percent: default_credential_warmup_selection_percent(),
            credentials_persist: default_credentials_persist(),
            credential_stats_persist: default_credential_stats_persist(),
            compression: CompressionConfig::default(),
            load_balancing_mode: default_load_balancing_mode(),
            compat_profile: default_compat_profile(),
            extract_thinking: default_extract_thinking(),
            prompt_cache_target_read_ratio: default_prompt_cache_target_read_ratio(),
            prompt_cache_token_scale: default_prompt_cache_token_scale(),
            prompt_cache_max_simulated_input_tokens:
                default_prompt_cache_max_simulated_input_tokens(),
            prompt_cache_cap_jitter_min_tokens: default_prompt_cache_cap_jitter_min_tokens(),
            prompt_cache_cap_jitter_max_tokens: default_prompt_cache_cap_jitter_max_tokens(),
            prompt_cache_scale_min_input_tokens: default_prompt_cache_scale_min_input_tokens(),
            reported_usage: ReportedUsageConfig::default(),
            usage_record_limit: default_usage_record_limit(),
            usage_record_persist: default_usage_record_persist(),
            high_cache_threshold: default_high_cache_threshold(),
            default_endpoint: default_endpoint(),
            expose_proxy_warnings: default_expose_proxy_warnings(),
            config_path: None,
        }
    }
}

impl Config {
    /// 获取默认配置文件路径
    pub fn default_config_path() -> &'static str {
        "config.json"
    }

    /// 获取有效的 Auth Region（用于 Token 刷新）
    /// 优先使用 auth_region，未配置时回退到 region
    pub fn effective_auth_region(&self) -> &str {
        self.auth_region.as_deref().unwrap_or(&self.region)
    }

    /// 获取有效的 API Region（用于 API 请求）
    /// 优先使用 api_region，未配置时回退到 region
    pub fn effective_api_region(&self) -> &str {
        self.api_region.as_deref().unwrap_or(&self.region)
    }

    /// 从文件加载配置
    pub fn load<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            // 配置文件不存在，返回默认配置
            let mut config = Self::default();
            config.config_path = Some(path.to_path_buf());
            return Ok(config);
        }

        let content = fs::read_to_string(path)?;
        let mut config: Config = serde_json::from_str(&content)?;
        config.config_path = Some(path.to_path_buf());
        Ok(config)
    }

    /// 获取配置文件路径（如果有）
    pub fn config_path(&self) -> Option<&Path> {
        self.config_path.as_deref()
    }

    /// 将当前配置写回原始配置文件
    pub fn save(&self) -> anyhow::Result<()> {
        let path = self
            .config_path
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("配置文件路径未知，无法保存配置"))?;

        let content = serde_json::to_string_pretty(self).context("序列化配置失败")?;
        crate::common::fs::write_file_atomic(path, content)
            .with_context(|| format!("写入配置文件失败: {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_compat_profile_is_claude_code() {
        assert_eq!(Config::default().compat_profile, CompatProfile::ClaudeCode);
    }

    #[test]
    fn default_prompt_cache_target_read_ratio_is_98_percent() {
        assert_eq!(Config::default().prompt_cache_target_read_ratio, 0.98);
    }

    #[test]
    fn default_runtime_controls_are_conservative() {
        let config = Config::default();

        assert_eq!(config.credential_rpm, None);
        assert_eq!(config.credential_max_concurrent_requests, 0);
        assert_eq!(config.credential_transient_cooldown_secs, 10);
        assert_eq!(config.credential_max_cooldown_secs, 300);
        assert_eq!(config.credential_dispatch_max_wait_secs, 120);
        assert_eq!(config.credential_in_flight_lease_max_secs, 900);
        assert_eq!(config.credential_warmup_requests, 3);
        assert_eq!(config.credential_warmup_selection_percent, 5);
        assert!(config.credentials_persist);
        assert!(config.credential_stats_persist);
        assert!(!config.compression.enabled);
        assert!(config.compression.whitespace_compression);
        assert_eq!(
            config
                .reported_usage
                .policy_for_path("/cc/v1/messages")
                .input
                .max_tokens,
            96
        );
    }

    #[test]
    fn default_prompt_cache_token_amplification_is_configured() {
        let config = Config::default();

        assert_eq!(config.prompt_cache_token_scale, 1.6);
        assert_eq!(config.prompt_cache_max_simulated_input_tokens, 300_000);
        assert_eq!(config.prompt_cache_cap_jitter_min_tokens, 12_000);
        assert_eq!(config.prompt_cache_cap_jitter_max_tokens, 24_000);
        assert_eq!(config.prompt_cache_scale_min_input_tokens, 20_000);
    }

    #[test]
    fn compat_profile_deserializes_from_camel_case_config() {
        let config: Config = serde_json::from_str(
            r#"{
                "apiKey": "sk-test",
                "compatProfile": "anthropic-strict"
            }"#,
        )
        .unwrap();

        assert_eq!(config.compat_profile, CompatProfile::AnthropicStrict);
    }

    #[test]
    fn legacy_prompt_cache_simulation_mode_is_ignored() {
        let config: Config = serde_json::from_str(
            r#"{
                "apiKey": "sk-test",
                "promptCacheSimulationMode": "disabled"
            }"#,
        )
        .unwrap();

        assert_eq!(config.prompt_cache_target_read_ratio, 0.98);
    }

    #[test]
    fn legacy_local_prompt_cache_mode_is_ignored() {
        let config: Config = serde_json::from_str(
            r#"{
                "apiKey": "sk-test",
                "promptCacheSimulationMode": "local-prompt-cache"
            }"#,
        )
        .unwrap();

        let json = serde_json::to_string(&config).unwrap();

        assert!(!json.contains("promptCacheSimulationMode"));
    }

    #[test]
    fn reported_usage_deserializes_from_camel_case_config() {
        let config: Config = serde_json::from_str(
            r#"{
                "apiKey": "sk-test",
                "reportedUsage": {
                    "default": {
                        "input": { "mode": "preserve" }
                    },
                    "pathOverrides": {
                        "cc": {
                            "input": {
                                "mode": "sample-max",
                                "maxTokens": 64,
                                "moveDeltaToCacheRead": true
                            },
                            "cacheCreation": {
                                "mode": "sample-target",
                                "targetTokens": 2048
                            }
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        let policy = config.reported_usage.policy_for_path("/cc/v1/messages");
        assert_eq!(policy.input.mode, ReportedUsageFieldMode::SampleMax);
        assert_eq!(policy.input.max_tokens, 64);
        assert!(policy.input.move_delta_to_cache_read);
        assert_eq!(
            policy.cache_creation.mode,
            ReportedUsageFieldMode::SampleTarget
        );
        assert_eq!(policy.cache_creation.target_tokens, 2048);
    }

    #[test]
    fn prompt_cache_token_amplification_deserializes_from_camel_case_config() {
        let config: Config = serde_json::from_str(
            r#"{
                "apiKey": "sk-test",
                "promptCacheTokenScale": 1.5,
                "promptCacheMaxSimulatedInputTokens": 200000,
                "promptCacheCapJitterMinTokens": 5000,
                "promptCacheCapJitterMaxTokens": 20000,
                "promptCacheScaleMinInputTokens": 10000
            }"#,
        )
        .unwrap();

        assert_eq!(config.prompt_cache_token_scale, 1.5);
        assert_eq!(config.prompt_cache_max_simulated_input_tokens, 200_000);
        assert_eq!(config.prompt_cache_cap_jitter_min_tokens, 5_000);
        assert_eq!(config.prompt_cache_cap_jitter_max_tokens, 20_000);
        assert_eq!(config.prompt_cache_scale_min_input_tokens, 10_000);
    }

    #[test]
    fn reported_usage_default_and_prefix_overrides_match_longest_prefix() {
        let mut reported_usage = ReportedUsageConfig::default();
        reported_usage.path_overrides.insert(
            "/cc/v1".to_string(),
            ReportedUsagePathPolicy {
                input: ReportedUsageFieldPolicy::sample_input_max(32),
                ..ReportedUsagePathPolicy::default()
            },
        );

        assert_eq!(
            reported_usage.policy_for_path("/v1/messages").input.mode,
            ReportedUsageFieldMode::Preserve
        );
        assert_eq!(
            reported_usage
                .policy_for_path("/cc/v1/messages")
                .input
                .max_tokens,
            32
        );
        assert_eq!(
            reported_usage.policy_for_path("/cc/other").input.max_tokens,
            96
        );
        assert!(!reported_usage.policy_for_path("/na/v1/messages").enabled);
    }
}
