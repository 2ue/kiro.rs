//! Anthropic API 中间件

use std::sync::Arc;

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
};

use crate::common::auth;
use crate::external_pool::ExternalPoolManager;
use crate::kiro::provider::KiroProvider;
use crate::model::config::{
    CompatProfile, ModelResolutionMode, PayloadGuardMode, PayloadShapingConfig,
    PromptCacheSimulationMode, ReportedUsageConfig,
};

use super::{
    envelope, model_capabilities::ModelCapabilitiesCatalog, pricing::PricingCatalog,
    prompt_cache::PromptCacheTracker, usage::UsageRecorder,
};

/// 应用共享状态
#[derive(Clone)]
pub struct AppState {
    /// API 密钥
    pub api_key: String,
    /// Kiro Provider（可选，用于实际 API 调用）
    /// 内部使用 MultiTokenManager，已支持线程安全的多凭据管理
    pub kiro_provider: Option<Arc<KiroProvider>>,
    /// 是否开启非流式响应的 thinking 块提取
    pub extract_thinking: bool,
    /// 请求级 usage 记录器
    pub usage_recorder: Arc<UsageRecorder>,
    /// 模型价格目录。仅用于统计计价，失败不影响请求。
    pub pricing_catalog: Arc<PricingCatalog>,
    /// 模型能力目录。仅用于 /models 和后台观测，失败不影响请求调度。
    pub model_capabilities: Arc<ModelCapabilitiesCatalog>,
    /// 本地 prompt-cache tracker
    pub prompt_cache: Arc<PromptCacheTracker>,
    /// 本地 prompt-cache usage 模拟模式
    pub prompt_cache_simulation_mode: PromptCacheSimulationMode,
    /// 本地 prompt-cache 模拟目标 cache read 比例
    pub prompt_cache_target_read_ratio: f64,
    /// high-cache 模拟专用的 total input 放大倍数
    pub prompt_cache_token_scale: f64,
    /// high-cache 模拟 total input 的上限
    pub prompt_cache_max_simulated_input_tokens: i32,
    /// high-cache 模拟触顶 soft-cap 最小扣减
    pub prompt_cache_cap_jitter_min_tokens: i32,
    /// high-cache 模拟触顶 soft-cap 最大扣减
    pub prompt_cache_cap_jitter_max_tokens: i32,
    /// high-cache 模拟启用 scale 的最小基础输入
    pub prompt_cache_scale_min_input_tokens: i32,
    /// 下游 usage 上报投影配置
    pub reported_usage: ReportedUsageConfig,
    /// Anthropic compatibility profile
    pub compat_profile: CompatProfile,
    /// 请求模型解析策略
    pub model_resolution_mode: ModelResolutionMode,
    /// 是否在响应头中暴露代理改写动作
    pub expose_proxy_warnings: bool,
    /// 是否启用发送 Kiro 上游前的最终 payload 防护
    pub payload_guard_enabled: bool,
    /// payload guard 大小裁剪触发模式
    pub payload_guard_mode: PayloadGuardMode,
    /// Kiro 上游请求 JSON body 最大字节数
    pub payload_guard_max_bytes: usize,
    /// payload 超限时是否裁剪旧历史
    pub payload_guard_trim_history: bool,
    /// payload shaping 配置
    pub payload_shaping: PayloadShapingConfig,
    /// 外部备用号池管理器。
    pub external_pool_manager: Option<Arc<ExternalPoolManager>>,
}

impl AppState {
    /// 创建新的应用状态
    pub fn new(
        api_key: impl Into<String>,
        extract_thinking: bool,
        usage_recorder: Arc<UsageRecorder>,
        prompt_cache: Arc<PromptCacheTracker>,
        prompt_cache_simulation_mode: PromptCacheSimulationMode,
        prompt_cache_target_read_ratio: f64,
        compat_profile: CompatProfile,
        expose_proxy_warnings: bool,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            kiro_provider: None,
            extract_thinking,
            usage_recorder,
            pricing_catalog: Arc::new(PricingCatalog::new()),
            model_capabilities: Arc::new(ModelCapabilitiesCatalog::new()),
            prompt_cache,
            prompt_cache_simulation_mode,
            prompt_cache_target_read_ratio: prompt_cache_target_read_ratio.clamp(0.0, 0.99),
            prompt_cache_token_scale: 1.0,
            prompt_cache_max_simulated_input_tokens: 0,
            prompt_cache_cap_jitter_min_tokens: 0,
            prompt_cache_cap_jitter_max_tokens: 0,
            prompt_cache_scale_min_input_tokens: 0,
            reported_usage: ReportedUsageConfig::default(),
            compat_profile,
            model_resolution_mode: ModelResolutionMode::Compatible,
            expose_proxy_warnings: expose_proxy_warnings || compat_profile.is_debug(),
            payload_guard_enabled: true,
            payload_guard_mode: PayloadGuardMode::Preemptive,
            payload_guard_max_bytes: 450 * 1024,
            payload_guard_trim_history: true,
            payload_shaping: PayloadShapingConfig::default(),
            external_pool_manager: None,
        }
    }

    pub fn with_prompt_cache_amplification(
        mut self,
        token_scale: f64,
        max_simulated_input_tokens: i32,
        cap_jitter_min_tokens: i32,
        cap_jitter_max_tokens: i32,
        scale_min_input_tokens: i32,
    ) -> Self {
        self.prompt_cache_token_scale = token_scale;
        self.prompt_cache_max_simulated_input_tokens = max_simulated_input_tokens;
        self.prompt_cache_cap_jitter_min_tokens = cap_jitter_min_tokens;
        self.prompt_cache_cap_jitter_max_tokens = cap_jitter_max_tokens;
        self.prompt_cache_scale_min_input_tokens = scale_min_input_tokens;
        self
    }

    pub fn with_pricing_catalog(mut self, pricing_catalog: Arc<PricingCatalog>) -> Self {
        self.pricing_catalog = pricing_catalog;
        self
    }

    pub fn with_model_capabilities(
        mut self,
        model_capabilities: Arc<ModelCapabilitiesCatalog>,
    ) -> Self {
        self.model_capabilities = model_capabilities;
        self
    }

    pub fn with_reported_usage(mut self, reported_usage: ReportedUsageConfig) -> Self {
        self.reported_usage = reported_usage.normalized();
        self
    }

    pub fn with_model_resolution_mode(mut self, mode: ModelResolutionMode) -> Self {
        self.model_resolution_mode = mode;
        self
    }

    pub fn with_prompt_cache_simulation_mode(mut self, mode: PromptCacheSimulationMode) -> Self {
        self.prompt_cache_simulation_mode = mode;
        self
    }

    pub fn with_payload_guard(
        mut self,
        enabled: bool,
        mode: PayloadGuardMode,
        max_bytes: usize,
        trim_history: bool,
        payload_shaping: PayloadShapingConfig,
    ) -> Self {
        self.payload_guard_enabled = enabled;
        self.payload_guard_mode = mode;
        self.payload_guard_max_bytes = max_bytes;
        self.payload_guard_trim_history = trim_history;
        self.payload_shaping = payload_shaping;
        self
    }

    /// 设置 KiroProvider
    pub fn with_kiro_provider(mut self, provider: Arc<KiroProvider>) -> Self {
        self.kiro_provider = Some(provider);
        self
    }

    pub fn with_external_pool_manager(
        mut self,
        external_pool_manager: Arc<ExternalPoolManager>,
    ) -> Self {
        self.external_pool_manager = Some(external_pool_manager);
        self
    }
}

/// API Key 认证中间件
pub async fn auth_middleware(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    match auth::extract_api_key(&request) {
        Some(key) if auth::constant_time_eq(&key, &state.api_key) => {
            let mut response = next.run(request).await;
            if !response.headers().contains_key("request-id") {
                let request_id = envelope::request_id();
                envelope::insert_request_id_headers(response.headers_mut(), &request_id);
            }
            response
        }
        _ => envelope::error_response(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "Invalid API key",
        ),
    }
}

/// CORS 中间件层
///
/// **安全说明**：当前配置允许所有来源（Any），这是为了支持公开 API 服务。
/// 如果需要更严格的安全控制，请根据实际需求配置具体的允许来源、方法和头信息。
///
/// # 配置说明
/// - `allow_origin(Any)`: 允许任何来源的请求
/// - `allow_methods(Any)`: 允许任何 HTTP 方法
/// - `allow_headers(Any)`: 允许任何请求头
pub fn cors_layer() -> tower_http::cors::CorsLayer {
    use tower_http::cors::{Any, CorsLayer};

    CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
}
