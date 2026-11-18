//! Admin API 类型定义

use serde::{Deserialize, Serialize};

use crate::anthropic::pricing::ModelPricing;
use crate::model::config::{
    AccountsConfig, CachePolicyConfig, CompatProfile, ModelMappingConfig, ModelResolutionMode,
    PromptCacheCreationControlConfig, PromptInjectionConfig, ReportedUsageConfig,
    RequestAdmissionConfig, RouteFeaturesConfig,
};

// ============ 凭据状态 ============

// ============ 操作请求 ============

/// 修改支持模型列表。空列表表示不限制模型调度。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetSupportedModelsRequest {
    #[serde(default)]
    pub supported_models: Vec<String>,
}

/// 使用账号兼容 /v1/models 接口发现支持模型。创建态需要传 baseUrl/apiKey；
/// 编辑态可只传覆盖项，空 key 表示使用已保存 key。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverAccountSupportedModelsRequest {
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub auth_type: Option<crate::account::AccountAuthType>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupportedModelsResponse {
    pub supported_models: Vec<String>,
    pub count: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UsageCleanupMode {
    /// 软删除 created_at 早于 cutoff 的可见明细，页面和当前统计口径不再包含这些记录。
    SoftDelete,
    /// 硬删除 deleted_at 早于 cutoff 的已软删除明细，真正降低表体积。
    HardDelete,
}

impl Default for UsageCleanupMode {
    fn default() -> Self {
        Self::SoftDelete
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UsageCleanupJobStatus {
    Idle,
    Queued,
    Running,
    Paused,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageCleanupRequest {
    /// 清理模式；默认 soft_delete。
    #[serde(default)]
    pub mode: UsageCleanupMode,
    /// 是否同时清理历史汇总。默认 false：只清理明细，保留汇总。
    #[serde(default)]
    pub include_summary: bool,
    /// 清理多少天之前的数据；soft_delete 按 created_at 匹配，hard_delete 对活跃明细按
    /// created_at 匹配、对已软删除明细按 deleted_at 匹配。
    /// 0 表示以任务启动时刻为 cutoff，清理当时之前全部匹配记录；未传默认 0；最大 3650。
    #[serde(default)]
    pub older_than_days: Option<u32>,
    /// 自定义 cutoff，优先级高于 older_than_days。
    #[serde(default)]
    pub cutoff_before: Option<String>,
    /// 每批处理行数，默认 10000。
    #[serde(default)]
    pub batch_size: Option<usize>,
    /// 兼容旧客户端的内部安全批次上限；页面不再暴露这个参数。
    #[serde(default)]
    pub max_batches: Option<usize>,
    /// 本轮最多清理多少条；不传或传 0 表示清理全部匹配明细。
    #[serde(default)]
    pub max_rows: Option<u64>,
    /// 批次之间暂停毫秒数，默认 10。
    #[serde(default)]
    pub pause_ms_between_batches: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageCleanupResumeRequest {
    pub job_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageCleanupPreviewResponse {
    pub mode: UsageCleanupMode,
    pub include_summary: bool,
    pub cutoff_at: String,
    pub matched_rows: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oldest_created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub newest_created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageCleanupStatusResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    pub status: UsageCleanupJobStatus,
    pub phase: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<UsageCleanupMode>,
    pub include_summary: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cutoff_at: Option<String>,
    pub batch_size: usize,
    pub max_batches: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_rows: Option<u64>,
    pub pause_ms_between_batches: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_rows: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining_rows: Option<u64>,
    pub processed_rows: u64,
    pub last_batch_rows: u64,
    pub batches: usize,
    pub redis_deleted_keys: usize,
    pub redis_delete_commands: usize,
    pub redis_max_command_keys: usize,
    pub redis_scan_passes: usize,
    pub redis_used_del_fallback: bool,
    pub redis_pass_limit_reached: bool,
    pub cancel_requested: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

// ============ 代理/家宽资源 ============

/// 复制账号：新账号沿用来源账号的全部配置（含密钥），只改名字，并默认停用等人工确认。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyAccountRequest {
    /// 新账号名称；留空则为“{来源名} 副本”。
    #[serde(default)]
    pub name: Option<String>,
    /// 复制后是否直接启用；默认 false，避免同一上游密钥立即双倍并发。
    #[serde(default)]
    pub enabled: bool,
}

/// 从公信力来源拉到的模型候选：目录里已有的标出来，让用户勾选新增项。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCatalogCandidate {
    pub model: String,
    pub display_name: String,
    pub max_input_tokens: Option<i32>,
    pub max_output_tokens: Option<i32>,
    pub supports_prompt_caching: Option<bool>,
    pub supported_input_types: Vec<String>,
    /// 已在目录里（含手动与种子）。
    pub in_catalog: bool,
    /// 有价格数据。
    pub has_pricing: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCatalogDiscoverResponse {
    pub source: String,
    pub source_url: String,
    pub fetched_at: String,
    pub candidates: Vec<ModelCatalogCandidate>,
}

/// 用户在弹层里确认后批量加入目录。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCatalogAdoptRequest {
    pub models: Vec<String>,
    /// 加入后是否对外列出；默认 true。
    #[serde(default = "default_true_flag")]
    pub listed: bool,
}

fn default_true_flag() -> bool {
    true
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCatalogAdoptResponse {
    pub added: Vec<String>,
    pub skipped: Vec<String>,
    pub status: crate::anthropic::model_capabilities::ModelCapabilitiesStatus,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountTestRequest {
    pub model: String,
    #[serde(default)]
    pub prompt: Option<String>,
}

// ============ 账号信息查询 ============

// ============ 负载均衡配置 ============

/// 系统版本响应
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemVersionResponse {
    /// 当前服务版本，来自 Cargo package version。
    pub version: &'static str,
}

// ============ 访问密钥 ============

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessKeysResponse {
    /// 下游客户端调用 /v1/messages 等接口时使用的请求 Key。
    pub request_api_key: String,
    pub masked_request_api_key: String,
    /// 下游客户端调用 API Key 列表。`requestApiKey` 为兼容旧前端保留，等于这里的第一项。
    pub request_api_keys: Vec<RequestApiKeyItem>,
    pub default_request_admission: RequestAdmissionConfig,
    /// 管理后台登录和 /api/admin 认证使用的 Key。
    pub admin_api_key: String,
    pub masked_admin_api_key: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestApiKeyItem {
    pub id: String,
    pub api_key: String,
    pub masked_api_key: String,
    pub primary: bool,
    pub name: String,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_admission: Option<RequestAdmissionConfig>,
    pub usage_limits: Vec<crate::anthropic::request_key_limits::KeyUsageLimit>,
    pub allowed_routes: Vec<String>,
}

/// Usage against every limit of one request key.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestKeyUsageLimitItem {
    pub key_id: String,
    pub enabled: bool,
    pub statuses: Vec<crate::anthropic::request_key_limits::KeyLimitStatus>,
    /// Set when the counters could not be read (requests for this key are then rejected).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestKeyUsageLimitsResponse {
    /// Day and month windows reset at midnight in this timezone.
    pub timezone: String,
    pub items: Vec<RequestKeyUsageLimitItem>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetRequestKeyUsageRequest {
    pub period: crate::anthropic::request_key_limits::KeyLimitPeriod,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateRequestApiKeyRequest {
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub request_admission: Option<RequestAdmissionConfig>,
    /// Replaces the key's usage limits; an empty list removes them.
    #[serde(default)]
    pub usage_limits: Option<Vec<crate::anthropic::request_key_limits::KeyUsageLimit>>,
    #[serde(default)]
    pub allowed_routes: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateRequestApiKeyRequest {
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub request_admission: Option<RequestAdmissionConfig>,
    /// Replaces the key's usage limits; an empty list removes them.
    #[serde(default)]
    pub usage_limits: Option<Vec<crate::anthropic::request_key_limits::KeyUsageLimit>>,
    #[serde(default)]
    pub allowed_routes: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAdminApiKeyRequest {
    pub admin_api_key: String,
}

// ============ 运行时全局配置 ============

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeConfigResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_password: Option<String>,
    /// 单实例、单个请求 API Key 的准入配置；不表示跨实例聚合上限。
    pub request_admission: RequestAdmissionConfig,
    pub stream_pre_output_hold_secs: u64,
    pub inference_upstream_max_attempts: u32,
    pub prompt_injection: PromptInjectionConfig,
    pub route_features: RouteFeaturesConfig,
    pub prompt_cache_target_read_ratio: f64,
    pub prompt_cache_token_scale: f64,
    pub prompt_cache_max_simulated_input_tokens: i32,
    pub prompt_cache_cap_jitter_min_tokens: i32,
    pub prompt_cache_cap_jitter_max_tokens: i32,
    pub prompt_cache_scale_min_input_tokens: i32,
    pub prompt_cache_creation_control: PromptCacheCreationControlConfig,
    pub prompt_cache_max_entries_per_account: usize,
    pub prompt_cache_max_entries_global: usize,
    pub prompt_cache_entry_ttl_secs: u64,
    pub prompt_cache_estimated_bytes_limit: u64,
    pub reported_usage: ReportedUsageConfig,
    pub cache_policy: CachePolicyConfig,
    pub accounts: AccountsConfig,
    pub high_cache_threshold: i32,
    pub compat_profile: CompatProfile,
    pub model_resolution_mode: ModelResolutionMode,
    pub model_mapping: ModelMappingConfig,
    pub audit_log_retention_enabled: bool,
    pub audit_log_retention_days: u32,
    pub tls_backend: crate::model::config::TlsBackend,
    pub usage_record_limit: usize,
}

/// 预览请求：请求条件，加上可选的未保存草稿（只用于这次预览）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptInjectionPreviewBody {
    #[serde(flatten)]
    pub request: crate::anthropic::PromptInjectionPreviewRequest,
    #[serde(default)]
    pub draft: Option<PromptInjectionDraft>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PromptInjectionDraft {
    #[serde(default)]
    pub prompt_injection: Option<PromptInjectionConfig>,
    #[serde(default)]
    pub route_features: Option<RouteFeaturesConfig>,
}

/// 管理端目录接口的返回。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptInjectionCatalogResponse {
    pub items: Vec<crate::model::prompt_injection::catalog::CatalogEntry>,
    pub fixed: Vec<crate::model::prompt_injection::catalog::FixedInjection>,
    pub max_text_chars: usize,
    pub max_total_text_chars: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateRuntimeConfigRequest {
    #[serde(default)]
    pub request_admission: Option<RequestAdmissionConfig>,
    #[serde(default)]
    pub stream_pre_output_hold_secs: Option<u64>,
    #[serde(default)]
    pub inference_upstream_max_attempts: Option<u32>,
    #[serde(default)]
    pub prompt_injection: Option<PromptInjectionConfig>,
    #[serde(default)]
    pub route_features: Option<RouteFeaturesConfig>,
    #[serde(default)]
    pub prompt_cache_target_read_ratio: Option<f64>,
    #[serde(default)]
    pub prompt_cache_token_scale: Option<f64>,
    #[serde(default)]
    pub prompt_cache_max_simulated_input_tokens: Option<i32>,
    #[serde(default)]
    pub prompt_cache_cap_jitter_min_tokens: Option<i32>,
    #[serde(default)]
    pub prompt_cache_cap_jitter_max_tokens: Option<i32>,
    #[serde(default)]
    pub prompt_cache_scale_min_input_tokens: Option<i32>,
    #[serde(default)]
    pub prompt_cache_creation_control: Option<PromptCacheCreationControlConfig>,
    #[serde(default)]
    pub prompt_cache_max_entries_per_account: Option<usize>,
    #[serde(default)]
    pub prompt_cache_max_entries_global: Option<usize>,
    #[serde(default)]
    pub prompt_cache_entry_ttl_secs: Option<u64>,
    #[serde(default)]
    pub prompt_cache_estimated_bytes_limit: Option<u64>,
    #[serde(default)]
    pub reported_usage: Option<ReportedUsageConfig>,
    #[serde(default)]
    pub cache_policy: Option<CachePolicyConfig>,
    #[serde(default)]
    pub accounts: Option<AccountsConfig>,
    #[serde(default)]
    pub high_cache_threshold: Option<i32>,
    #[serde(default)]
    pub compat_profile: Option<CompatProfile>,
    #[serde(default)]
    pub model_resolution_mode: Option<ModelResolutionMode>,
    #[serde(default)]
    pub model_mapping: Option<ModelMappingConfig>,
    #[serde(default)]
    pub audit_log_retention_enabled: Option<bool>,
    #[serde(default)]
    pub audit_log_retention_days: Option<u32>,
    #[serde(default, deserialize_with = "deserialize_nullable_update")]
    pub proxy_url: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_nullable_update")]
    pub proxy_username: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_nullable_update")]
    pub proxy_password: Option<Option<String>>,
    #[serde(default)]
    pub tls_backend: Option<crate::model::config::TlsBackend>,
    #[serde(default)]
    pub usage_record_limit: Option<usize>,
}

// ============ 手动模型补充 ============

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualModelPricingRequest {
    pub input_cost_per_million: f64,
    pub output_cost_per_million: f64,
    /// 5-minute cache write.
    #[serde(default)]
    pub cache_creation_input_cost_per_million: Option<f64>,
    /// 1-hour cache write; defaults to 2x input.
    #[serde(default)]
    pub cache_creation_1h_input_cost_per_million: Option<f64>,
    #[serde(default)]
    pub cache_read_input_cost_per_million: Option<f64>,
}

impl ManualModelPricingRequest {
    pub fn to_pricing(&self) -> Option<ModelPricing> {
        let input = cost_per_token(self.input_cost_per_million)?;
        let output = cost_per_token(self.output_cost_per_million)?;
        let cache_creation = self
            .cache_creation_input_cost_per_million
            .and_then(cost_per_token)
            .unwrap_or(input * 1.25);
        let cache_read = self
            .cache_read_input_cost_per_million
            .and_then(cost_per_token)
            .unwrap_or(input * 0.1);
        let cache_creation_1h = self
            .cache_creation_1h_input_cost_per_million
            .and_then(cost_per_token)
            .unwrap_or(0.0);
        Some(
            ModelPricing {
                input_cost_per_token: input,
                output_cost_per_token: output,
                cache_creation_input_token_cost: cache_creation,
                cache_creation_1h_input_token_cost: cache_creation_1h,
                cache_read_input_token_cost: cache_read,
            }
            .with_derived_cache_creation_1h_cost(),
        )
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpsertManualModelRequest {
    #[serde(default = "default_manual_model_listed")]
    pub listed: bool,
    pub model: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub max_input_tokens: Option<i32>,
    #[serde(default)]
    pub max_output_tokens: Option<i32>,
    #[serde(default)]
    pub supports_prompt_caching: Option<bool>,
    #[serde(default)]
    pub supported_input_types: Vec<String>,
    #[serde(default)]
    pub pricing: Option<ManualModelPricingRequest>,
    #[serde(default)]
    pub clear_pricing: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManualModelResponse {
    pub success: bool,
    pub message: String,
    pub model: String,
}

impl ManualModelResponse {
    pub fn new(model: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            success: true,
            model: model.into(),
            message: message.into(),
        }
    }
}

fn cost_per_token(value: f64) -> Option<f64> {
    value.is_finite().then_some(value.max(0.0) / 1_000_000.0)
}

// ============ 通用响应 ============

/// 操作成功响应
#[derive(Debug, Serialize)]
pub struct SuccessResponse {
    pub success: bool,
    pub message: String,
}

impl SuccessResponse {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            success: true,
            message: message.into(),
        }
    }
}

/// 错误响应
#[derive(Debug, Serialize)]
pub struct AdminErrorResponse {
    pub error: AdminError,
}

#[derive(Debug, Serialize)]
pub struct AdminError {
    #[serde(rename = "type")]
    pub error_type: String,
    pub message: String,
}

impl AdminErrorResponse {
    pub fn new(error_type: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            error: AdminError {
                error_type: error_type.into(),
                message: message.into(),
            },
        }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new("invalid_request", message)
    }

    pub fn authentication_error() -> Self {
        Self::new("authentication_error", "Invalid or missing admin API key")
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new("not_found", message)
    }

    pub fn api_error(message: impl Into<String>) -> Self {
        Self::new("api_error", message)
    }

    pub fn internal_error(message: impl Into<String>) -> Self {
        Self::new("internal_error", message)
    }
}

// ============ 订阅等级模型列表 ============

fn default_manual_model_listed() -> bool {
    true
}

fn deserialize_nullable_update<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(deserializer).map(Some)
}
