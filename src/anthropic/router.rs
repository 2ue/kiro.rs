//! Anthropic API 路由配置

use std::sync::Arc;

use axum::{
    Router,
    extract::DefaultBodyLimit,
    middleware,
    routing::{get, post},
};

use crate::common::auth::RequestApiKeyStore;
use crate::external_pool::ExternalPoolManager;
use crate::kiro::provider::KiroProvider;
use crate::model::config::{
    BodyConversionConfig, CachePolicyConfig, CompatProfile, Config, ExternalPoolsConfig,
    ImageProcessingConfig, MissingMaxTokensConfig, ModelMappingConfig, ModelResolutionMode,
    PayloadGuardMode, PayloadShapingConfig, PromptCacheCreationControlConfig,
    PromptCacheSimulationMode, PromptSteeringConfig, ReportedUsageConfig, ThinkingTriggerMode,
    ToolFormatDebugConfig,
};

use super::{
    files::{
        delete_file, delete_file_dfcache, get_file, get_file_content, get_file_content_dfcache,
        get_file_dfcache, list_files, upload_file,
    },
    handlers::{
        count_tokens, count_tokens_cc, count_tokens_dfcache, get_models, get_models_dfcache,
        post_messages, post_messages_cc, post_messages_dfcache, post_messages_ha,
        post_messages_real_cache_usage,
    },
    middleware::{AppState, auth_middleware, cors_layer},
    model_capabilities::ModelCapabilitiesCatalog,
    pricing::PricingCatalog,
    prompt_cache::{PromptCacheBounds, PromptCacheTracker},
    prompt_cache_creation_control::PromptCacheCreationController,
    tool_format_debug::ToolFormatDebugRecorder,
    usage::UsageRecorder,
};

/// 请求体最大大小限制 (50MB)
const MAX_BODY_SIZE: usize = 50 * 1024 * 1024;

pub struct AnthropicRouterDependencies {
    pub request_api_keys: Arc<RequestApiKeyStore>,
    pub kiro_provider: Option<Arc<KiroProvider>>,
    pub usage_recorder: Arc<UsageRecorder>,
    pub prompt_cache: Arc<PromptCacheTracker>,
    pub prompt_cache_creation_controller: Arc<PromptCacheCreationController>,
    pub pricing_catalog: Arc<PricingCatalog>,
    pub model_capabilities: Arc<ModelCapabilitiesCatalog>,
    pub external_pool_manager: Option<Arc<ExternalPoolManager>>,
}

pub struct AnthropicRouterConfig {
    extract_thinking: bool,
    prompt_cache_target_read_ratio: f64,
    prompt_cache_token_scale: f64,
    prompt_cache_max_simulated_input_tokens: i32,
    prompt_cache_cap_jitter_min_tokens: i32,
    prompt_cache_cap_jitter_max_tokens: i32,
    prompt_cache_scale_min_input_tokens: i32,
    prompt_cache_creation_control: PromptCacheCreationControlConfig,
    prompt_cache_bounds: PromptCacheBounds,
    reported_usage: ReportedUsageConfig,
    cache_policy: CachePolicyConfig,
    defined_cache_routes: Vec<String>,
    compat_profile: CompatProfile,
    thinking_trigger_mode: ThinkingTriggerMode,
    model_resolution_mode: ModelResolutionMode,
    model_mapping: ModelMappingConfig,
    expose_proxy_warnings: bool,
    payload_guard_enabled: bool,
    payload_guard_mode: PayloadGuardMode,
    payload_guard_max_bytes: usize,
    payload_guard_safety_margin_bytes: usize,
    payload_guard_trim_history: bool,
    payload_guard_external_enabled: bool,
    kiro_cache_point_enabled: bool,
    kiro_cache_point_tools_only: bool,
    kiro_cache_point_record_plan: bool,
    kiro_upstream_stream_idle_timeout_secs: u64,
    image_processing: ImageProcessingConfig,
    body_conversion: BodyConversionConfig,
    prompt_steering: PromptSteeringConfig,
    missing_max_tokens: MissingMaxTokensConfig,
    payload_shaping: PayloadShapingConfig,
    external_pools: ExternalPoolsConfig,
    tool_format_debug: ToolFormatDebugConfig,
}

impl AnthropicRouterConfig {
    pub fn from_runtime_config(config: &Config) -> Self {
        Self {
            extract_thinking: config.extract_thinking,
            prompt_cache_target_read_ratio: config.prompt_cache_target_read_ratio,
            prompt_cache_token_scale: config.prompt_cache_token_scale,
            prompt_cache_max_simulated_input_tokens: config.prompt_cache_max_simulated_input_tokens,
            prompt_cache_cap_jitter_min_tokens: config.prompt_cache_cap_jitter_min_tokens,
            prompt_cache_cap_jitter_max_tokens: config.prompt_cache_cap_jitter_max_tokens,
            prompt_cache_scale_min_input_tokens: config.prompt_cache_scale_min_input_tokens,
            prompt_cache_creation_control: config.prompt_cache_creation_control.normalized(),
            prompt_cache_bounds: PromptCacheBounds::from_config(
                config.prompt_cache_max_entries_per_account,
                config.prompt_cache_max_entries_global,
                config.prompt_cache_entry_ttl_secs,
                config.prompt_cache_estimated_bytes_limit,
            ),
            reported_usage: config.reported_usage.clone(),
            cache_policy: config.cache_policy.clone(),
            defined_cache_routes: config.defined_cache_routes.clone(),
            compat_profile: config.compat_profile,
            thinking_trigger_mode: config.thinking_trigger_mode,
            model_resolution_mode: config.model_resolution_mode,
            model_mapping: config.model_mapping.clone().normalized(),
            expose_proxy_warnings: config.expose_proxy_warnings,
            payload_guard_enabled: config.payload_guard_enabled,
            payload_guard_mode: config.payload_guard_mode,
            payload_guard_max_bytes: config.payload_guard_max_bytes,
            payload_guard_safety_margin_bytes: config.payload_guard_safety_margin_bytes,
            payload_guard_trim_history: config.payload_guard_trim_history,
            payload_guard_external_enabled: config.payload_guard_external_enabled,
            kiro_cache_point_enabled: config.kiro_cache_point_enabled,
            kiro_cache_point_tools_only: config.kiro_cache_point_tools_only,
            kiro_cache_point_record_plan: config.kiro_cache_point_record_plan,
            kiro_upstream_stream_idle_timeout_secs: config.kiro_upstream_stream_idle_timeout_secs,
            image_processing: config.image_processing.normalized(),
            body_conversion: config.body_conversion.clone(),
            prompt_steering: config.prompt_steering.clone().normalized(),
            missing_max_tokens: config.missing_max_tokens.normalized(),
            payload_shaping: config.payload_shaping,
            external_pools: config.external_pools.clone(),
            tool_format_debug: config.tool_format_debug.clone(),
        }
    }
}

/// 创建 Anthropic API 路由
///
/// # 端点
/// - `GET /v1/models` - 获取可用模型列表
/// - `POST /v1/messages` - 创建消息（对话）
/// - `POST /v1/messages/count_tokens` - 计算 token 数量
/// - `GET /na/v1/models` - 获取可用模型列表（no-cache）
/// - `POST /na/v1/messages` - 创建消息（no-cache）
/// - `POST /na/v1/messages/count_tokens` - 计算 token 数量
/// - `GET /ha/v1/models` - 获取可用模型列表（high-cache，usage 上报由 `/ha` 覆盖项控制）
/// - `POST /ha/v1/messages` - 创建消息（high-cache，usage 上报由 `/ha` 覆盖项控制）
/// - `POST /ha/v1/messages/count_tokens` - 计算 token 数量
///
/// # 认证
/// 所有 `/v1` 路径需要 API Key 认证，支持：
/// - `x-api-key` header
/// - `Authorization: Bearer <token>` header
///
/// 创建带有 KiroProvider 的 Anthropic API 路由
pub fn create_router_with_provider(
    dependencies: AnthropicRouterDependencies,
    config: AnthropicRouterConfig,
) -> Router {
    let AnthropicRouterDependencies {
        request_api_keys,
        kiro_provider,
        usage_recorder,
        prompt_cache,
        prompt_cache_creation_controller,
        pricing_catalog,
        model_capabilities,
        external_pool_manager,
    } = dependencies;
    let AnthropicRouterConfig {
        extract_thinking,
        prompt_cache_target_read_ratio,
        prompt_cache_token_scale,
        prompt_cache_max_simulated_input_tokens,
        prompt_cache_cap_jitter_min_tokens,
        prompt_cache_cap_jitter_max_tokens,
        prompt_cache_scale_min_input_tokens,
        prompt_cache_creation_control,
        prompt_cache_bounds,
        reported_usage,
        cache_policy,
        defined_cache_routes,
        compat_profile,
        thinking_trigger_mode,
        model_resolution_mode,
        model_mapping,
        expose_proxy_warnings,
        payload_guard_enabled,
        payload_guard_mode,
        payload_guard_max_bytes,
        payload_guard_safety_margin_bytes,
        payload_guard_trim_history,
        payload_guard_external_enabled,
        kiro_cache_point_enabled,
        kiro_cache_point_tools_only,
        kiro_cache_point_record_plan,
        kiro_upstream_stream_idle_timeout_secs,
        image_processing,
        body_conversion,
        prompt_steering,
        missing_max_tokens,
        payload_shaping,
        external_pools,
        tool_format_debug,
    } = config;
    let tool_format_debug_recorder = ToolFormatDebugRecorder::new(tool_format_debug);
    let mut base_state = AppState::new(
        request_api_keys,
        extract_thinking,
        usage_recorder,
        prompt_cache,
        prompt_cache_creation_controller,
        PromptCacheSimulationMode::HighCache,
        prompt_cache_target_read_ratio,
        compat_profile,
        expose_proxy_warnings,
    )
    .with_prompt_cache_amplification(
        prompt_cache_token_scale,
        prompt_cache_max_simulated_input_tokens,
        prompt_cache_cap_jitter_min_tokens,
        prompt_cache_cap_jitter_max_tokens,
        prompt_cache_scale_min_input_tokens,
    )
    .with_prompt_cache_creation_control(prompt_cache_creation_control)
    .with_prompt_cache_bounds(prompt_cache_bounds)
    .with_reported_usage(reported_usage)
    .with_cache_policy(cache_policy)
    .with_defined_cache_routes(defined_cache_routes)
    .with_thinking_trigger_mode(thinking_trigger_mode)
    .with_model_resolution_mode(model_resolution_mode)
    .with_model_mapping(model_mapping)
    .with_payload_guard(
        payload_guard_enabled,
        payload_guard_mode,
        payload_guard_max_bytes,
        payload_guard_safety_margin_bytes,
        payload_guard_trim_history,
        payload_guard_external_enabled,
        kiro_cache_point_enabled,
        kiro_cache_point_tools_only,
        kiro_cache_point_record_plan,
        kiro_upstream_stream_idle_timeout_secs,
        image_processing,
        body_conversion,
        prompt_steering,
        payload_shaping,
    )
    .with_missing_max_tokens(missing_max_tokens)
    .with_external_pools(external_pools)
    .with_tool_format_debug_recorder(tool_format_debug_recorder)
    .with_pricing_catalog(pricing_catalog)
    .with_model_capabilities(model_capabilities);
    if let Some(provider) = kiro_provider {
        base_state = base_state.with_kiro_provider(provider);
    }
    if let Some(manager) = external_pool_manager {
        base_state = base_state.with_external_pool_manager(manager);
    }

    let (v1_state, na_v1_state, cc_v1_state, ha_v1_state) = route_prompt_cache_states(base_state);
    let define_cache_state = v1_state.clone();

    // 需要认证的 /v1 路由（默认 high-cache）
    let v1_routes = Router::new()
        .route("/models", get(get_models))
        .route("/files", get(list_files).post(upload_file))
        .route("/files/{file_id}", get(get_file).delete(delete_file))
        .route("/files/{file_id}/content", get(get_file_content))
        .route("/messages", post(post_messages))
        .route("/messages/count_tokens", post(count_tokens))
        .layer(middleware::from_fn_with_state(
            v1_state.clone(),
            auth_middleware,
        ))
        .with_state(v1_state);

    // 需要认证的 /na/v1 路由（默认 no-cache）
    let na_v1_routes = Router::new()
        .route("/models", get(get_models))
        .route("/files", get(list_files).post(upload_file))
        .route("/files/{file_id}", get(get_file).delete(delete_file))
        .route("/files/{file_id}/content", get(get_file_content))
        .route("/messages", post(post_messages_real_cache_usage))
        .route("/messages/count_tokens", post(count_tokens))
        .layer(middleware::from_fn_with_state(
            na_v1_state.clone(),
            auth_middleware,
        ))
        .with_state(na_v1_state);

    // 需要认证的 /cc/v1 路由（Claude Code 兼容端点）
    // 与 /v1 的区别：实时流式返回，最终 message_delta.usage 修正用量。
    let cc_v1_routes = Router::new()
        .route("/models", get(get_models))
        .route("/files", get(list_files).post(upload_file))
        .route("/files/{file_id}", get(get_file).delete(delete_file))
        .route("/files/{file_id}/content", get(get_file_content))
        .route("/messages", post(post_messages_cc))
        .route("/messages/count_tokens", post(count_tokens_cc))
        .layer(middleware::from_fn_with_state(
            cc_v1_state.clone(),
            auth_middleware,
        ))
        .with_state(cc_v1_state);

    // 需要认证的 /ha/v1 路由（high-cache；usage 上报由 /ha 路径覆盖项独立控制）
    let ha_v1_routes = Router::new()
        .route("/models", get(get_models))
        .route("/files", get(list_files).post(upload_file))
        .route("/files/{file_id}", get(get_file).delete(delete_file))
        .route("/files/{file_id}/content", get(get_file_content))
        .route("/messages", post(post_messages_ha))
        .route("/messages/count_tokens", post(count_tokens))
        .layer(middleware::from_fn_with_state(
            ha_v1_state.clone(),
            auth_middleware,
        ))
        .with_state(ha_v1_state);

    // 需要认证的 /dfcache/{route}/v1 路由。
    // route 必须在 definedCacheRoutes 中显式定义，避免未知路径被默认放行。
    let dfcache_routes = Router::new()
        .route("/{route}/v1/models", get(get_models_dfcache))
        .route("/{route}/v1/files", get(list_files).post(upload_file))
        .route(
            "/{route}/v1/files/{file_id}",
            get(get_file_dfcache).delete(delete_file_dfcache),
        )
        .route(
            "/{route}/v1/files/{file_id}/content",
            get(get_file_content_dfcache),
        )
        .route("/{route}/v1/messages", post(post_messages_dfcache))
        .route(
            "/{route}/v1/messages/count_tokens",
            post(count_tokens_dfcache),
        )
        .layer(middleware::from_fn_with_state(
            define_cache_state.clone(),
            auth_middleware,
        ))
        .with_state(define_cache_state);

    Router::new()
        .nest("/v1", v1_routes)
        .nest("/na/v1", na_v1_routes)
        .nest("/cc/v1", cc_v1_routes)
        .nest("/ha/v1", ha_v1_routes)
        .nest("/dfcache", dfcache_routes)
        .layer(cors_layer())
        .layer(DefaultBodyLimit::max(MAX_BODY_SIZE))
}

fn route_prompt_cache_states(base_state: AppState) -> (AppState, AppState, AppState, AppState) {
    (
        base_state
            .clone()
            .with_prompt_cache_simulation_mode(PromptCacheSimulationMode::HighCache),
        base_state
            .clone()
            .with_prompt_cache_simulation_mode(PromptCacheSimulationMode::Disabled),
        base_state
            .clone()
            .with_prompt_cache_simulation_mode(PromptCacheSimulationMode::HighCache),
        base_state.with_prompt_cache_simulation_mode(PromptCacheSimulationMode::HighCache),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_state(mode: PromptCacheSimulationMode) -> AppState {
        AppState::new(
            Arc::new(RequestApiKeyStore::new(["test-key"])),
            true,
            Arc::new(UsageRecorder::new(10)),
            Arc::new(PromptCacheTracker::default()),
            Arc::new(PromptCacheCreationController::default()),
            mode,
            0.98,
            CompatProfile::ClaudeCode,
            false,
        )
    }

    #[test]
    fn route_prompt_cache_states_keep_na_no_cache_and_other_builtin_cache_paths_high_cache() {
        for base_mode in [
            PromptCacheSimulationMode::Disabled,
            PromptCacheSimulationMode::HighCache,
        ] {
            let (v1_state, na_v1_state, cc_v1_state, ha_v1_state) =
                route_prompt_cache_states(base_state(base_mode));

            assert_eq!(
                v1_state.prompt_cache_simulation_mode,
                PromptCacheSimulationMode::HighCache
            );
            assert_eq!(
                na_v1_state.prompt_cache_simulation_mode,
                PromptCacheSimulationMode::Disabled
            );
            assert_eq!(
                cc_v1_state.prompt_cache_simulation_mode,
                PromptCacheSimulationMode::HighCache
            );
            assert_eq!(
                ha_v1_state.prompt_cache_simulation_mode,
                PromptCacheSimulationMode::HighCache
            );
            assert!(Arc::ptr_eq(
                &v1_state.prompt_cache,
                &na_v1_state.prompt_cache
            ));
            assert!(Arc::ptr_eq(
                &v1_state.prompt_cache,
                &cc_v1_state.prompt_cache
            ));
            assert!(Arc::ptr_eq(
                &v1_state.prompt_cache,
                &ha_v1_state.prompt_cache
            ));
        }
    }
}
