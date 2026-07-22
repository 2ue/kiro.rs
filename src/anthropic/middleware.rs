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
use crate::common::auth::RequestApiKeyStore;
use crate::external_pool::ExternalPoolManager;
use crate::kiro::provider::KiroProvider;
use crate::model::config::{
    BodyConversionConfig, CachePolicyConfig, CompatProfile, ExternalPoolsConfig,
    ImageProcessingConfig, MissingMaxTokensConfig, ModelMappingConfig, ModelResolutionMode,
    PayloadGuardMode, PayloadShapingConfig, PromptCacheCreationControlConfig,
    PromptCacheSimulationMode, PromptSteeringConfig, ReportedUsageConfig, ThinkingTriggerMode,
    normalize_defined_cache_routes,
};

use super::{
    envelope,
    files::AnthropicFileStore,
    model_capabilities::ModelCapabilitiesCatalog,
    pricing::PricingCatalog,
    prompt_cache::{PromptCacheBounds, PromptCacheTracker},
    prompt_cache_creation_control::PromptCacheCreationController,
    tool_format_debug::ToolFormatDebugRecorder,
    usage::UsageRecorder,
};

/// 应用共享状态
#[derive(Clone)]
pub struct AppState {
    /// 客户端请求 API Key 内存索引。
    pub request_api_keys: Arc<RequestApiKeyStore>,
    /// Kiro Provider（可选，用于实际 API 调用）
    /// 内部使用 MultiTokenManager，已支持线程安全的多凭据管理
    pub kiro_provider: Option<Arc<KiroProvider>>,
    /// 是否开启非流式响应的 thinking 块提取
    pub extract_thinking: bool,
    /// thinking 触发策略
    pub thinking_trigger_mode: ThinkingTriggerMode,
    /// 请求级 usage 记录器
    pub usage_recorder: Arc<UsageRecorder>,
    /// tool-use 格式错误内部诊断写盘器。关闭时为 no-op。
    pub tool_format_debug_recorder: Arc<ToolFormatDebugRecorder>,
    /// 模型价格目录。仅用于统计计价，失败不影响请求。
    pub pricing_catalog: Arc<PricingCatalog>,
    /// 模型能力目录。仅用于 /models 和后台观测，失败不影响请求调度。
    pub model_capabilities: Arc<ModelCapabilitiesCatalog>,
    /// Anthropic Files 兼容上传暂存区，用于 Claude Code 图片/文件 source.file_id。
    pub file_store: Arc<AnthropicFileStore>,
    /// 本地 prompt-cache tracker
    pub prompt_cache: Arc<PromptCacheTracker>,
    /// 本地 prompt-cache creation 上报频次控制器
    pub prompt_cache_creation_controller: Arc<PromptCacheCreationController>,
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
    /// 本地 prompt-cache creation 上报频次控制配置
    pub prompt_cache_creation_control: PromptCacheCreationControlConfig,
    /// 本地 prompt-cache 内存和条目边界
    pub prompt_cache_bounds: PromptCacheBounds,
    /// 下游 usage 上报投影配置
    pub reported_usage: ReportedUsageConfig,
    /// 路径级缓存策略覆盖
    pub cache_policy: CachePolicyConfig,
    /// 已定义的 /dfcache/{name} 自定义 high-cache 路由。
    pub defined_cache_routes: Vec<String>,
    /// Anthropic compatibility profile
    pub compat_profile: CompatProfile,
    /// 请求模型解析策略
    pub model_resolution_mode: ModelResolutionMode,
    /// 模型映射和兜底规则
    pub model_mapping: ModelMappingConfig,
    /// 是否在响应头中暴露代理改写动作
    pub expose_proxy_warnings: bool,
    /// 是否启用发送 Kiro 上游前的最终 payload 防护
    pub payload_guard_enabled: bool,
    /// payload guard 大小裁剪触发模式
    pub payload_guard_mode: PayloadGuardMode,
    /// Kiro 上游请求 JSON body 最大字节数
    pub payload_guard_max_bytes: usize,
    /// payload guard 安全余量字节数
    pub payload_guard_safety_margin_bytes: usize,
    /// payload 超限时是否裁剪旧历史
    pub payload_guard_trim_history: bool,
    /// 外部备用池是否复用同一套 payload guard / shaping 配置
    pub payload_guard_external_enabled: bool,
    /// 是否把工具 cache_control 转成 Kiro cachePoint
    pub kiro_cache_point_enabled: bool,
    /// cachePoint 是否仅按工具 cache_control 插入
    pub kiro_cache_point_tools_only: bool,
    /// 是否记录 cachePoint 插入计划
    pub kiro_cache_point_record_plan: bool,
    /// Kiro 上游流式响应正文静默超时秒数
    pub kiro_upstream_stream_idle_timeout_secs: u64,
    /// 多模态图片/文件预处理配置
    pub image_processing: ImageProcessingConfig,
    /// 本地 Anthropic -> Kiro 转换能力配置
    pub body_conversion: BodyConversionConfig,
    /// 统一提示词引导配置
    pub prompt_steering: PromptSteeringConfig,
    /// Messages 请求缺少顶层 max_tokens 时的入口兼容策略
    pub missing_max_tokens: MissingMaxTokensConfig,
    /// payload shaping 配置
    pub payload_shaping: PayloadShapingConfig,
    /// 外部备用号池和直连策略配置。
    pub external_pools: ExternalPoolsConfig,
    /// 外部备用号池管理器。
    pub external_pool_manager: Option<Arc<ExternalPoolManager>>,
}

impl AppState {
    /// 创建新的应用状态
    pub fn new(
        request_api_keys: Arc<RequestApiKeyStore>,
        extract_thinking: bool,
        usage_recorder: Arc<UsageRecorder>,
        prompt_cache: Arc<PromptCacheTracker>,
        prompt_cache_creation_controller: Arc<PromptCacheCreationController>,
        prompt_cache_simulation_mode: PromptCacheSimulationMode,
        prompt_cache_target_read_ratio: f64,
        compat_profile: CompatProfile,
        expose_proxy_warnings: bool,
    ) -> Self {
        Self {
            request_api_keys,
            kiro_provider: None,
            extract_thinking,
            thinking_trigger_mode: ThinkingTriggerMode::RealRequest,
            usage_recorder,
            tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
            pricing_catalog: Arc::new(PricingCatalog::new()),
            model_capabilities: Arc::new(ModelCapabilitiesCatalog::new()),
            file_store: Arc::new(AnthropicFileStore::default()),
            prompt_cache,
            prompt_cache_creation_controller,
            prompt_cache_simulation_mode,
            prompt_cache_target_read_ratio: prompt_cache_target_read_ratio.clamp(0.0, 0.99),
            prompt_cache_token_scale: 1.0,
            prompt_cache_max_simulated_input_tokens: 0,
            prompt_cache_cap_jitter_min_tokens: 0,
            prompt_cache_cap_jitter_max_tokens: 0,
            prompt_cache_scale_min_input_tokens: 0,
            prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
            prompt_cache_bounds: PromptCacheBounds::default(),
            reported_usage: ReportedUsageConfig::default(),
            cache_policy: CachePolicyConfig::default(),
            defined_cache_routes: Vec::new(),
            compat_profile,
            model_resolution_mode: ModelResolutionMode::Compatible,
            model_mapping: ModelMappingConfig::default(),
            expose_proxy_warnings: expose_proxy_warnings || compat_profile.is_debug(),
            payload_guard_enabled: true,
            payload_guard_mode: PayloadGuardMode::OnTooLong,
            payload_guard_max_bytes: 450 * 1024,
            payload_guard_safety_margin_bytes: 32 * 1024,
            payload_guard_trim_history: true,
            payload_guard_external_enabled: true,
            kiro_cache_point_enabled: false,
            kiro_cache_point_tools_only: true,
            kiro_cache_point_record_plan: true,
            kiro_upstream_stream_idle_timeout_secs: 180,
            image_processing: ImageProcessingConfig::default(),
            body_conversion: BodyConversionConfig::default(),
            prompt_steering: PromptSteeringConfig::default(),
            missing_max_tokens: MissingMaxTokensConfig::default(),
            payload_shaping: PayloadShapingConfig::default(),
            external_pools: ExternalPoolsConfig::default(),
            external_pool_manager: None,
        }
    }

    pub fn with_prompt_cache_creation_control(
        mut self,
        config: PromptCacheCreationControlConfig,
    ) -> Self {
        self.prompt_cache_creation_control = config.normalized();
        self
    }

    pub fn with_thinking_trigger_mode(mut self, mode: ThinkingTriggerMode) -> Self {
        self.thinking_trigger_mode = mode;
        self
    }

    pub fn with_prompt_cache_bounds(mut self, bounds: PromptCacheBounds) -> Self {
        self.prompt_cache_bounds = bounds;
        self
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

    pub fn with_tool_format_debug_recorder(
        mut self,
        recorder: Arc<ToolFormatDebugRecorder>,
    ) -> Self {
        self.tool_format_debug_recorder = recorder;
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

    pub fn with_cache_policy(mut self, cache_policy: CachePolicyConfig) -> Self {
        self.cache_policy = cache_policy.normalized();
        self
    }

    pub fn with_defined_cache_routes(mut self, routes: Vec<String>) -> Self {
        self.defined_cache_routes = normalize_defined_cache_routes(&routes);
        self
    }

    pub fn with_model_resolution_mode(mut self, mode: ModelResolutionMode) -> Self {
        self.model_resolution_mode = mode;
        self
    }

    pub fn with_model_mapping(mut self, model_mapping: ModelMappingConfig) -> Self {
        self.model_mapping = model_mapping.normalized();
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
        safety_margin_bytes: usize,
        trim_history: bool,
        external_enabled: bool,
        kiro_cache_point_enabled: bool,
        kiro_cache_point_tools_only: bool,
        kiro_cache_point_record_plan: bool,
        kiro_upstream_stream_idle_timeout_secs: u64,
        image_processing: ImageProcessingConfig,
        body_conversion: BodyConversionConfig,
        prompt_steering: PromptSteeringConfig,
        payload_shaping: PayloadShapingConfig,
    ) -> Self {
        self.payload_guard_enabled = enabled;
        self.payload_guard_mode = mode;
        self.payload_guard_max_bytes = max_bytes;
        self.payload_guard_safety_margin_bytes = safety_margin_bytes;
        self.payload_guard_trim_history = trim_history;
        self.payload_guard_external_enabled = external_enabled;
        self.kiro_cache_point_enabled = kiro_cache_point_enabled;
        self.kiro_cache_point_tools_only = kiro_cache_point_tools_only;
        self.kiro_cache_point_record_plan = kiro_cache_point_record_plan;
        self.kiro_upstream_stream_idle_timeout_secs = kiro_upstream_stream_idle_timeout_secs;
        self.image_processing = image_processing.normalized();
        self.body_conversion = body_conversion;
        self.prompt_steering = prompt_steering.normalized();
        self.payload_shaping = payload_shaping;
        self
    }

    pub fn with_missing_max_tokens(mut self, config: MissingMaxTokensConfig) -> Self {
        self.missing_max_tokens = config.normalized();
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

    pub fn with_external_pools(mut self, external_pools: ExternalPoolsConfig) -> Self {
        self.external_pools = external_pools;
        self
    }
}

/// API Key 认证中间件
pub async fn auth_middleware(
    State(state): State<AppState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let identity =
        auth::extract_api_key(&request).and_then(|key| state.request_api_keys.authenticate(&key));
    match identity {
        Some(identity) => {
            request.extensions_mut().insert(identity);
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

#[cfg(test)]
mod tests {
    use axum::{Router, routing::get};
    use tower::ServiceExt;

    use super::*;

    const JSON_413_BODY: &str = r#"{"source":"upstream","kind":"json-413"}"#;
    const TEXT_413_BODY: &str = "upstream plaintext 413";
    const UNTYPED_413_BODY: &str = "handler untyped 413";

    fn test_state() -> AppState {
        AppState::new(
            Arc::new(RequestApiKeyStore::new(["middleware-413-key"])),
            true,
            Arc::new(UsageRecorder::new(10)),
            Arc::new(PromptCacheTracker::default()),
            Arc::new(PromptCacheCreationController::default()),
            PromptCacheSimulationMode::HighCache,
            0.98,
            CompatProfile::ClaudeCode,
            false,
        )
    }

    async fn json_413() -> Response {
        Response::builder()
            .status(StatusCode::PAYLOAD_TOO_LARGE)
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .header("x-origin-marker", "json")
            .body(Body::from(JSON_413_BODY))
            .unwrap()
    }

    async fn plaintext_413() -> Response {
        Response::builder()
            .status(StatusCode::PAYLOAD_TOO_LARGE)
            .header(
                axum::http::header::CONTENT_TYPE,
                "text/plain; charset=utf-8",
            )
            .header("x-origin-marker", "text")
            .body(Body::from(TEXT_413_BODY))
            .unwrap()
    }

    async fn untyped_413() -> Response {
        Response::builder()
            .status(StatusCode::PAYLOAD_TOO_LARGE)
            .header("x-origin-marker", "untyped")
            .body(Body::from(UNTYPED_413_BODY))
            .unwrap()
    }

    #[tokio::test]
    async fn auth_middleware_preserves_downstream_413_bodies_for_five_rounds() {
        let state = test_state();
        let app = Router::new()
            .route("/json", get(json_413))
            .route("/text", get(plaintext_413))
            .route("/untyped", get(untyped_413))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                auth_middleware,
            ))
            .with_state(state);

        for _ in 0..5 {
            for (path, expected_content_type, expected_marker, expected_body) in [
                ("/json", Some("application/json"), "json", JSON_413_BODY),
                (
                    "/text",
                    Some("text/plain; charset=utf-8"),
                    "text",
                    TEXT_413_BODY,
                ),
                ("/untyped", None, "untyped", UNTYPED_413_BODY),
            ] {
                let response = app
                    .clone()
                    .oneshot(
                        Request::builder()
                            .uri(path)
                            .header("x-api-key", "middleware-413-key")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
                assert_eq!(
                    response
                        .headers()
                        .get(axum::http::header::CONTENT_TYPE)
                        .and_then(|value| value.to_str().ok()),
                    expected_content_type
                );
                assert_eq!(
                    response
                        .headers()
                        .get("x-origin-marker")
                        .and_then(|value| value.to_str().ok()),
                    Some(expected_marker)
                );
                let request_id = response
                    .headers()
                    .get("request-id")
                    .and_then(|value| value.to_str().ok())
                    .expect("middleware must add request-id")
                    .to_string();
                assert_eq!(
                    response
                        .headers()
                        .get("anthropic-request-id")
                        .and_then(|value| value.to_str().ok()),
                    Some(request_id.as_str())
                );
                let body = axum::body::to_bytes(response.into_body(), 1024)
                    .await
                    .unwrap();
                assert_eq!(body.as_ref(), expected_body.as_bytes());
            }
        }
    }
}
