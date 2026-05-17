use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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

/// 本地 prompt-cache usage 模拟模式。
///
/// 默认关闭；`high-cache` 会在上游 metadata 未报告 cache 时用本地缓存估算补足。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PromptCacheSimulationMode {
    Disabled,
    LocalPromptCache,
    HighCache,
}

impl Default for PromptCacheSimulationMode {
    fn default() -> Self {
        Self::Disabled
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

    /// 本地 prompt-cache usage 模拟模式（默认 disabled）。
    #[serde(default = "default_prompt_cache_simulation_mode")]
    pub prompt_cache_simulation_mode: PromptCacheSimulationMode,

    /// 本地 prompt-cache 模拟的目标 cache read 中心比例。
    ///
    /// 对 local-prompt-cache 和 high-cache 生效；读取仍必须命中同一凭据、
    /// 会话、模型下已创建过的缓存前缀，不会凭空制造 cache read。实际比例会
    /// 围绕该值做小范围确定性浮动，避免每次都精确落在同一个百分比。
    #[serde(default = "default_prompt_cache_target_read_ratio")]
    pub prompt_cache_target_read_ratio: f64,

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

    /// 端点特定的配置
    ///
    /// 键为端点名（如 "ide" / "cli"），值为该端点自由定义的参数对象。
    /// 未在此表出现的端点沿用实现内置默认值。
    #[serde(default)]
    pub endpoints: HashMap<String, serde_json::Value>,

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

fn default_compat_profile() -> CompatProfile {
    CompatProfile::ClaudeCode
}

fn default_extract_thinking() -> bool {
    true
}

fn default_prompt_cache_simulation_mode() -> PromptCacheSimulationMode {
    PromptCacheSimulationMode::Disabled
}

fn default_prompt_cache_target_read_ratio() -> f64 {
    0.85
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
            load_balancing_mode: default_load_balancing_mode(),
            compat_profile: default_compat_profile(),
            extract_thinking: default_extract_thinking(),
            prompt_cache_simulation_mode: default_prompt_cache_simulation_mode(),
            prompt_cache_target_read_ratio: default_prompt_cache_target_read_ratio(),
            usage_record_limit: default_usage_record_limit(),
            usage_record_persist: default_usage_record_persist(),
            high_cache_threshold: default_high_cache_threshold(),
            default_endpoint: default_endpoint(),
            endpoints: HashMap::new(),
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
        fs::write(path, content)
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
    fn prompt_cache_simulation_mode_deserializes_high_cache() {
        let config: Config = serde_json::from_str(
            r#"{
                "apiKey": "sk-test",
                "promptCacheSimulationMode": "high-cache"
            }"#,
        )
        .unwrap();

        assert_eq!(
            config.prompt_cache_simulation_mode,
            PromptCacheSimulationMode::HighCache
        );
    }
}
