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
    CompatProfile, ModelMappingConfig, ModelResolutionMode, PayloadGuardMode, PayloadShapingConfig,
    PromptCacheCreationControlConfig, PromptCacheSimulationMode, ReportedUsageConfig,
    ThinkingTriggerMode,
};

use super::{
    handlers::{
        count_tokens, count_tokens_dfcache, get_models, get_models_dfcache, post_messages,
        post_messages_cc, post_messages_dfcache, post_messages_ha, post_messages_real_cache_usage,
    },
    middleware::{AppState, auth_middleware, cors_layer},
    model_capabilities::ModelCapabilitiesCatalog,
    pricing::PricingCatalog,
    prompt_cache::{PromptCacheBounds, PromptCacheTracker},
    prompt_cache_creation_control::PromptCacheCreationController,
    usage::UsageRecorder,
};

/// 请求体最大大小限制 (50MB)
const MAX_BODY_SIZE: usize = 50 * 1024 * 1024;

/// 创建 Anthropic API 路由
///
/// # 端点
/// - `GET /v1/models` - 获取可用模型列表
/// - `POST /v1/messages` - 创建消息（对话）
/// - `POST /v1/messages/count_tokens` - 计算 token 数量
/// - `GET /na/v1/models` - 获取可用模型列表（保留真实上游 usage）
/// - `POST /na/v1/messages` - 创建消息（保留真实上游 usage）
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
/// # 参数
/// - `api_key`: API 密钥，用于验证客户端请求
/// - `kiro_provider`: 可选的 KiroProvider，用于调用上游 API

/// 创建带有 KiroProvider 的 Anthropic API 路由
#[allow(clippy::too_many_arguments)]
pub fn create_router_with_provider(
    request_api_keys: Arc<RequestApiKeyStore>,
    kiro_provider: Option<Arc<KiroProvider>>,
    extract_thinking: bool,
    usage_recorder: Arc<UsageRecorder>,
    prompt_cache: Arc<PromptCacheTracker>,
    prompt_cache_creation_controller: Arc<PromptCacheCreationController>,
    pricing_catalog: Arc<PricingCatalog>,
    model_capabilities: Arc<ModelCapabilitiesCatalog>,
    prompt_cache_target_read_ratio: f64,
    prompt_cache_token_scale: f64,
    prompt_cache_max_simulated_input_tokens: i32,
    prompt_cache_cap_jitter_min_tokens: i32,
    prompt_cache_cap_jitter_max_tokens: i32,
    prompt_cache_scale_min_input_tokens: i32,
    prompt_cache_creation_control: PromptCacheCreationControlConfig,
    prompt_cache_bounds: PromptCacheBounds,
    reported_usage: ReportedUsageConfig,
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
    payload_shaping: PayloadShapingConfig,
    external_pool_manager: Option<Arc<ExternalPoolManager>>,
) -> Router {
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
        payload_shaping,
    )
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
        .route("/messages", post(post_messages))
        .route("/messages/count_tokens", post(count_tokens))
        .layer(middleware::from_fn_with_state(
            v1_state.clone(),
            auth_middleware,
        ))
        .with_state(v1_state);

    // 需要认证的 /na/v1 路由（默认只上报真实上游 usage）
    let na_v1_routes = Router::new()
        .route("/models", get(get_models))
        .route("/messages", post(post_messages_real_cache_usage))
        .route("/messages/count_tokens", post(count_tokens))
        .layer(middleware::from_fn_with_state(
            na_v1_state.clone(),
            auth_middleware,
        ))
        .with_state(na_v1_state);

    // 需要认证的 /cc/v1 路由（Claude Code 兼容端点）
    // 与 /v1 的区别：流式响应会等待 contextUsageEvent 后再发送 message_start
    let cc_v1_routes = Router::new()
        .route("/models", get(get_models))
        .route("/messages", post(post_messages_cc))
        .route("/messages/count_tokens", post(count_tokens))
        .layer(middleware::from_fn_with_state(
            cc_v1_state.clone(),
            auth_middleware,
        ))
        .with_state(cc_v1_state);

    // 需要认证的 /ha/v1 路由（high-cache；usage 上报由 /ha 路径覆盖项独立控制）
    let ha_v1_routes = Router::new()
        .route("/models", get(get_models))
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
            .with_prompt_cache_simulation_mode(PromptCacheSimulationMode::HighCache),
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
    fn route_prompt_cache_states_force_all_message_paths_to_high_cache() {
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
                PromptCacheSimulationMode::HighCache
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
