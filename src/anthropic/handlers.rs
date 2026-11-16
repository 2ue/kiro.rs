//! Anthropic Messages 入口与账号编排。
use super::{
    body_processing, envelope,
    inference_attempt_budget::InferenceAttemptBudget,
    middleware::AppState,
    model_capabilities::{ModelResolution, normalize_model_id, strip_model_compat_suffixes},
    payload_guard::PayloadGuardConfig,
    prompt_cache::PromptCacheBounds,
    request_admission::{RequestRejectionAttribution, RequestRejectionReason},
    request_body::MessagesBody,
    request_facts::*,
    types::{
        CountTokensRequest, CountTokensResponse, MessagesRequest, ModelsQuery, ModelsResponse,
        Thinking,
    },
    usage::{RequestRejectionUsageContext, UsageRouteSubtype},
};
use crate::account::{AccountRouteRequest, AccountRouteRequestPreparationCache};
use crate::{model::config::*, token};
use axum::{
    Json as JsonExtractor,
    extract::{Extension, OriginalUri, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Json, Response},
};
use bytes::Bytes;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::{
    sync::{Arc, atomic::AtomicU64},
    time::Duration,
};
#[path = "handlers/injection_preview.rs"]
mod injection_preview;
#[path = "handlers/parsed_body_pipeline.rs"]
mod parsed_body_pipeline;
#[path = "handlers/request_entry.rs"]
mod request_entry;
pub(crate) use injection_preview::{
    PromptInjectionPreviewRequest, PromptInjectionPreviewResponse, preview_prompt_injection,
};

#[derive(Debug, Clone)]
struct RequestRuntimeConfig {
    prompt_cache_simulation_mode: PromptCacheSimulationMode,
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
    compiled_routes: Arc<CompiledRoutes>,
    compat_profile: CompatProfile,
    model_resolution_mode: ModelResolutionMode,
    model_mapping: ModelMappingConfig,
    payload_guard_enabled: bool,
    payload_guard_mode: PayloadGuardMode,
    payload_guard_max_bytes: usize,
    payload_guard_safety_margin_bytes: usize,
    payload_guard_trim_history: bool,
    payload_guard_external_enabled: bool,
    stream_pre_output_hold_secs: u64,
    inference_upstream_max_attempts: u32,
    image_processing: ImageProcessingConfig,
    prompt_injection: PromptInjectionConfig,
    route_features: RouteFeaturesConfig,
    missing_max_tokens: MissingMaxTokensConfig,
    local_request_compat: crate::model::config::LocalRequestCompatConfig,
    payload_shaping: PayloadShapingConfig,
    accounts: AccountsConfig,
}

impl RequestRuntimeConfig {
    /// `endpoint` 上生效的路由功能。
    fn route_features_for(&self, endpoint: &str) -> RouteFeatureSet {
        self.compiled_routes
            .match_endpoint(endpoint)
            .map(|route| route.features)
            .unwrap_or_default()
    }

    /// `endpoint` 上允许注入的项。
    fn injections_for(&self, endpoint: &str) -> crate::model::config::ResolvedInjections {
        self.compiled_routes
            .match_endpoint(endpoint)
            .map(|route| route.injections.clone())
            .unwrap_or_default()
    }

    /// 账号流式首输出前的静默保留窗口；0 表示一直保留到首个语义输出。
    fn external_stream_pre_output_hold(&self) -> Option<Duration> {
        (self.stream_pre_output_hold_secs > 0)
            .then(|| Duration::from_secs(self.stream_pre_output_hold_secs))
    }

    fn from_app_state(state: &AppState) -> Self {
        let config = state.runtime.snapshot();
        Self {
            prompt_cache_simulation_mode: state.prompt_cache_simulation_mode,
            prompt_cache_target_read_ratio: state.prompt_cache_target_read_ratio,
            prompt_cache_token_scale: state.prompt_cache_token_scale,
            prompt_cache_max_simulated_input_tokens: state.prompt_cache_max_simulated_input_tokens,
            prompt_cache_cap_jitter_min_tokens: state.prompt_cache_cap_jitter_min_tokens,
            prompt_cache_cap_jitter_max_tokens: state.prompt_cache_cap_jitter_max_tokens,
            prompt_cache_scale_min_input_tokens: state.prompt_cache_scale_min_input_tokens,
            prompt_cache_creation_control: state.prompt_cache_creation_control,
            prompt_cache_bounds: state.prompt_cache_bounds,
            reported_usage: ReportedUsageConfig::clone(&state.reported_usage),
            cache_policy: CachePolicyConfig::clone(&state.cache_policy),
            compiled_routes: config
                .compiled_routes
                .clone()
                .unwrap_or_else(|| Arc::new(config.routes.compile_with_config(&config))),
            compat_profile: state.compat_profile,
            model_resolution_mode: state.model_resolution_mode,
            model_mapping: ModelMappingConfig::clone(&state.model_mapping).normalized(),
            payload_guard_enabled: state.payload_guard_enabled,
            payload_guard_mode: state.payload_guard_mode,
            payload_guard_max_bytes: state.payload_guard_max_bytes,
            payload_guard_safety_margin_bytes: state.payload_guard_safety_margin_bytes,
            payload_guard_trim_history: state.payload_guard_trim_history,
            payload_guard_external_enabled: state.payload_guard_external_enabled,
            stream_pre_output_hold_secs: 15,
            inference_upstream_max_attempts:
                super::inference_attempt_budget::DEFAULT_INFERENCE_UPSTREAM_MAX_ATTEMPTS,
            image_processing: state.image_processing.normalized(),
            prompt_injection: PromptInjectionConfig::clone(&state.prompt_injection).normalized(),
            route_features: RouteFeaturesConfig::clone(&state.route_features).normalized(),
            missing_max_tokens: state.missing_max_tokens.normalized(),
            local_request_compat: crate::model::config::LocalRequestCompatConfig::default(),
            payload_shaping: state.payload_shaping,
            accounts: {
                let mut accounts = AccountsConfig::clone(&state.accounts);
                accounts.routes = config.routes.clone();
                accounts
            },
        }
    }

    fn from_config_with_fallback(config: &Config, fallback: Self) -> Self {
        Self::from_config_with_defaults(
            config,
            fallback.prompt_cache_simulation_mode,
            fallback.prompt_cache_target_read_ratio,
            fallback.prompt_cache_token_scale,
        )
    }

    fn from_config(config: &Config) -> Self {
        Self::from_config_with_defaults(config, PromptCacheSimulationMode::HighCache, 0.0, 1.0)
    }

    fn from_config_with_defaults(
        config: &Config,
        prompt_cache_simulation_mode: PromptCacheSimulationMode,
        fallback_target_read_ratio: f64,
        fallback_token_scale: f64,
    ) -> Self {
        Self {
            prompt_cache_simulation_mode,
            prompt_cache_target_read_ratio: if config.prompt_cache_target_read_ratio.is_finite() {
                config.prompt_cache_target_read_ratio.clamp(0.0, 0.99)
            } else {
                fallback_target_read_ratio
            },
            prompt_cache_token_scale: if config.prompt_cache_token_scale.is_finite() {
                config.prompt_cache_token_scale.clamp(1.0, 3.0)
            } else {
                fallback_token_scale
            },
            prompt_cache_max_simulated_input_tokens: config
                .prompt_cache_max_simulated_input_tokens
                .max(0),
            prompt_cache_cap_jitter_min_tokens: config.prompt_cache_cap_jitter_min_tokens.max(0),
            prompt_cache_cap_jitter_max_tokens: config.prompt_cache_cap_jitter_max_tokens.max(0),
            prompt_cache_scale_min_input_tokens: config.prompt_cache_scale_min_input_tokens.max(0),
            prompt_cache_creation_control: config.prompt_cache_creation_control.normalized(),
            prompt_cache_bounds: PromptCacheBounds::from_config(
                config.prompt_cache_max_entries_per_account,
                config.prompt_cache_max_entries_global,
                config.prompt_cache_entry_ttl_secs,
                config.prompt_cache_estimated_bytes_limit,
            ),
            reported_usage: config.reported_usage.normalized(),
            cache_policy: config.cache_policy.clone(),
            compiled_routes: config
                .compiled_routes
                .clone()
                .unwrap_or_else(|| Arc::new(config.routes.compile_with_config(config))),
            compat_profile: config.compat_profile,
            model_resolution_mode: config.model_resolution_mode,
            model_mapping: config.model_mapping.clone().normalized(),
            payload_guard_enabled: config
                .route_features
                .defaults
                .payload_guard
                .map(|guard| guard.enabled)
                .unwrap_or(false),
            payload_guard_mode: config
                .route_features
                .defaults
                .payload_guard
                .map(|guard| guard.mode)
                .unwrap_or(Default::default()),
            payload_guard_max_bytes: config
                .route_features
                .defaults
                .payload_guard
                .map(|guard| guard.max_bytes)
                .unwrap_or(0),
            payload_guard_safety_margin_bytes: config
                .route_features
                .defaults
                .payload_guard
                .map(|guard| guard.safety_margin_bytes)
                .unwrap_or(0),
            payload_guard_trim_history: config
                .route_features
                .defaults
                .payload_guard
                .map(|guard| guard.trim_history)
                .unwrap_or(false),
            payload_guard_external_enabled: config
                .route_features
                .defaults
                .payload_guard
                .map(|guard| guard.external_enabled)
                .unwrap_or(false),
            stream_pre_output_hold_secs: config
                .stream_pre_output_hold_secs
                .min(crate::model::config::STREAM_PRE_OUTPUT_HOLD_MAX_SECS),
            inference_upstream_max_attempts: config.inference_upstream_max_attempts.clamp(1, 10),
            image_processing: config
                .route_features
                .defaults
                .image_processing
                .unwrap_or_default()
                .normalized(),
            prompt_injection: config.prompt_injection.clone().normalized(),
            route_features: config.route_features.clone().normalized(),
            missing_max_tokens: config
                .route_features
                .defaults
                .missing_max_tokens
                .unwrap_or_default()
                .normalized(),
            local_request_compat: config
                .route_features
                .defaults
                .reasoning_compat
                .unwrap_or_default(),
            payload_shaping: config
                .route_features
                .defaults
                .payload_shaping
                .unwrap_or_default(),
            accounts: {
                let mut accounts = config.accounts.clone();
                accounts.routes = config.routes.clone();
                accounts.transport_proxy_url = config.proxy_url.clone();
                accounts.transport_proxy_username = config.proxy_username.clone();
                accounts.transport_proxy_password = config.proxy_password.clone();
                accounts
            },
        }
    }

    fn effective_payload_guard_max_bytes(&self) -> usize {
        const MIN_EFFECTIVE_LIMIT_BYTES: usize = 64 * 1024;
        let max_bytes = self.payload_guard_max_bytes;
        if max_bytes == 0 || self.payload_guard_safety_margin_bytes == 0 {
            return max_bytes;
        }
        if max_bytes <= MIN_EFFECTIVE_LIMIT_BYTES {
            return max_bytes;
        }
        let margin = self
            .payload_guard_safety_margin_bytes
            .min(max_bytes.saturating_sub(MIN_EFFECTIVE_LIMIT_BYTES));
        max_bytes.saturating_sub(margin)
    }

    fn payload_guard_config(&self) -> PayloadGuardConfig {
        PayloadGuardConfig {
            enabled: self.payload_guard_enabled,
            max_bytes: self.effective_payload_guard_max_bytes(),
            trim_history: self.payload_guard_trim_history,
            shaping: self.payload_shaping,
        }
    }

    fn initial_payload_guard_config(&self) -> PayloadGuardConfig {
        match self.payload_guard_mode {
            PayloadGuardMode::Preemptive => self.payload_guard_config(),
            PayloadGuardMode::OnTooLong => PayloadGuardConfig {
                enabled: self.payload_guard_enabled,
                max_bytes: 0,
                trim_history: false,
                shaping: self.payload_shaping,
            },
        }
    }

    fn too_long_retry_enabled(&self) -> bool {
        self.payload_guard_mode == PayloadGuardMode::OnTooLong
            && self.payload_guard_enabled
            && self.payload_guard_max_bytes > 0
    }

    fn legacy_cache_route_policy_default(&self) -> CacheRoutePolicy {
        CacheRoutePolicy {
            cache_type: PromptCacheStrategyType::CurrentHighCache,
            simulation: CacheSimulationPolicy {
                enabled: self.prompt_cache_simulation_mode == PromptCacheSimulationMode::HighCache,
                target_read_ratio: self.prompt_cache_target_read_ratio,
                token_scale: self.prompt_cache_token_scale,
                max_simulated_input_tokens: self.prompt_cache_max_simulated_input_tokens,
                cap_jitter_min_tokens: self.prompt_cache_cap_jitter_min_tokens,
                cap_jitter_max_tokens: self.prompt_cache_cap_jitter_max_tokens,
                scale_min_input_tokens: self.prompt_cache_scale_min_input_tokens,
            }
            .normalized(),
            creation_control: self.prompt_cache_creation_control.normalized(),
            reported_usage: self.reported_usage.default.normalized(),

            bounds: CacheBoundsPolicy {
                max_entries_per_account: self.prompt_cache_bounds.max_entries_per_account,
                max_entries_global: self.prompt_cache_bounds.max_entries_global,
                entry_ttl_secs: self.prompt_cache_bounds.entry_ttl.as_secs(),
                estimated_bytes_limit: self.prompt_cache_bounds.estimated_bytes_limit,
            },
            prefix_tool: PrefixToolCachePolicy::default(),
            stable_segment: StableSegmentCachePolicy::default(),
            hit_shaping: crate::model::config::CacheHitShapingPolicy::default(),
            cold_request_guard: crate::model::config::ColdRequestGuardPolicy::default(),
            prefix_tool_plus: crate::model::config::PrefixToolPlusCachePolicy::default(),
            cache_ttl: crate::model::config::CacheTtlPolicy::default(),
        }
        .normalized()
    }

    fn cache_policy_for_path(&self, path: &str) -> ResolvedCacheRoutePolicy {
        self.compiled_routes
            .match_endpoint(path)
            .expect("builtin root route")
            .cache_policy
            .clone()
    }
}

fn cache_route_for_request_stream(
    cache_route: ResolvedCacheRoutePolicy,
    stream: bool,
) -> ResolvedCacheRoutePolicy {
    if stream
        || !cache_route
            .policy
            .reported_usage
            .skip_non_stream_usage_projection
    {
        return cache_route;
    }

    disable_cache_route(cache_route)
}

fn disable_cache_route(mut cache_route: ResolvedCacheRoutePolicy) -> ResolvedCacheRoutePolicy {
    cache_route.policy.cache_type = PromptCacheStrategyType::NoCache;
    cache_route.policy.simulation.enabled = false;
    cache_route.policy.creation_control.enabled = false;
    cache_route.policy.reported_usage.enabled = false;
    cache_route.policy = cache_route.policy.normalized();
    cache_route.namespace = None;
    cache_route
}

fn prompt_cache_simulation_mode_for_policy(policy: &CacheRoutePolicy) -> PromptCacheSimulationMode {
    match policy.cache_type {
        PromptCacheStrategyType::CurrentHighCache | PromptCacheStrategyType::BoundedHighCache
            if policy.simulation.enabled =>
        {
            PromptCacheSimulationMode::HighCache
        }
        PromptCacheStrategyType::NoCache
        | PromptCacheStrategyType::CurrentHighCache
        | PromptCacheStrategyType::BoundedHighCache
        | PromptCacheStrategyType::PrefixTool
        | PromptCacheStrategyType::PrefixToolPlus
        | PromptCacheStrategyType::StableSegmentCache => PromptCacheSimulationMode::Disabled,
    }
}

fn prompt_cache_bounds_for_policy(policy: &CacheRoutePolicy) -> PromptCacheBounds {
    PromptCacheBounds::from_config(
        policy.bounds.max_entries_per_account,
        policy.bounds.max_entries_global,
        policy.bounds.entry_ttl_secs,
        policy.bounds.estimated_bytes_limit,
    )
}

fn reported_usage_config_for_policy(policy: ReportedUsagePathPolicy) -> ReportedUsageConfig {
    ReportedUsageConfig { default: policy }
}

fn request_runtime_config(state: &AppState) -> Arc<RequestRuntimeConfig> {
    let (config, generation) = state.runtime.snapshot_with_generation();
    let key = RequestRuntimeConfigCacheKey {
        runtime: Arc::as_ptr(&state.runtime) as usize,
        generation,
        prompt_cache_simulation_mode: state.prompt_cache_simulation_mode,
        prompt_cache_target_read_ratio_bits: state.prompt_cache_target_read_ratio.to_bits(),
        prompt_cache_token_scale_bits: state.prompt_cache_token_scale.to_bits(),
    };
    {
        let cache = REQUEST_RUNTIME_CONFIG_CACHE.lock();
        if let Some((_, cached)) = cache.iter().find(|(cached_key, _)| *cached_key == key) {
            return cached.clone();
        }
    }
    let built = Arc::new(RequestRuntimeConfig::from_config_with_fallback(
        &config,
        RequestRuntimeConfig::from_app_state(state),
    ));
    let mut cache = REQUEST_RUNTIME_CONFIG_CACHE.lock();
    // Entries of older generations are never asked for again.
    cache.retain(|(cached_key, _)| {
        cached_key.runtime != key.runtime || cached_key.generation == key.generation
    });
    if cache.len() >= REQUEST_RUNTIME_CONFIG_CACHE_MAX {
        cache.remove(0);
    }
    cache.push((key, built.clone()));
    built
}

fn external_request_model_resolution(requested_model: &str) -> ModelResolution {
    let mut resolution = ModelResolution::pass_through(normalize_model_id(requested_model));
    // Accounts apply their own mapping pipeline. Do not expose the local
    // resolver's diagnostic note for a deliberate external pass-through.
    resolution.note = None;
    resolution
}

fn current_image_too_large_response(
    violation: &super::payload_guard::CurrentImageTooLarge,
) -> Response {
    let mut response = envelope::error_response(
        StatusCode::BAD_REQUEST,
        "invalid_request_error",
        violation.message(),
    );
    response.headers_mut().insert(
        "x-should-retry",
        axum::http::HeaderValue::from_static("false"),
    );
    response
}

pub async fn get_models(
    State(state): State<AppState>,
    OriginalUri(uri): OriginalUri,
    Query(query): Query<ModelsQuery>,
) -> Response {
    models_list_response(&state, &query, uri.path()).await
}
pub async fn get_model(
    State(state): State<AppState>,
    OriginalUri(uri): OriginalUri,
    Path(params): Path<std::collections::HashMap<String, String>>,
) -> Response {
    let model_id = params
        .get("model_id")
        .map(String::as_str)
        .unwrap_or_default();
    model_retrieve_response(&state, model_id, uri.path()).await
}
async fn route_models(state: &AppState, endpoint: &str) -> Vec<super::types::Model> {
    let config = state.runtime.snapshot();
    let Some(route) = config.routes.match_endpoint(endpoint) else {
        return Vec::new();
    };
    let Some(manager) = &state.account_manager else {
        return Vec::new();
    };
    manager
        .filter_route_models(
            route,
            endpoint,
            state.model_capabilities.standard_anthropic_models(),
        )
        .await
}
async fn models_list_response(state: &AppState, query: &ModelsQuery, endpoint: &str) -> Response {
    Json(ModelsResponse::paginate(
        route_models(state, endpoint).await,
        query,
    ))
    .into_response()
}
async fn model_retrieve_response(state: &AppState, model_id: &str, endpoint: &str) -> Response {
    let listed = state.model_capabilities.standard_anthropic_model(model_id);
    if let Some(model) = listed {
        if route_models(state, endpoint)
            .await
            .iter()
            .any(|entry| entry.id == model.id)
        {
            return Json(model).into_response();
        }
    }
    envelope::error_response(
        StatusCode::NOT_FOUND,
        "not_found_error",
        format!("model: {model_id}"),
    )
}

pub(crate) fn resolve_defined_cache_route(
    state: &AppState,
    route: &str,
) -> Result<String, Response> {
    let candidate = format!("/dfcache/{route}");
    let Some(prefix) = normalize_defined_cache_route(&candidate) else {
        return Err(envelope::error_response(
            StatusCode::NOT_FOUND,
            "not_found_error",
            format!("dfcache route is invalid: {candidate}"),
        ));
    };
    let config = state.runtime.snapshot();
    if !config.routes.routes.iter().any(|route| {
        route.enabled && crate::model::routes::normalize_route_prefix(&route.prefix) == prefix
    }) {
        return Err(envelope::error_response(
            StatusCode::NOT_FOUND,
            "not_found_error",
            format!("dfcache route is not configured: {prefix}"),
        ));
    }
    Ok(prefix)
}

pub(crate) fn record_pre_usage_rejection(
    attribution: Option<&RequestRejectionAttribution>,
    reason: RequestRejectionReason,
    endpoint: &str,
    response: &Response,
) {
    record_pre_usage_rejection_with_metadata_and_context(
        attribution,
        reason,
        endpoint,
        response,
        None,
        None,
    );
}

fn record_pre_usage_rejection_with_metadata_and_context(
    attribution: Option<&RequestRejectionAttribution>,
    reason: RequestRejectionReason,
    endpoint: &str,
    response: &Response,
    extra_metadata: Option<Value>,
    request_context: Option<RequestRejectionUsageContext>,
) {
    let Some(attribution) = attribution else {
        return;
    };
    let Some(request_id) = response
        .headers()
        .get("request-id")
        .and_then(|value| value.to_str().ok())
    else {
        return;
    };
    attribution.record_with_metadata_and_context(
        reason,
        "handler_preflight",
        response.status(),
        request_id,
        endpoint,
        extra_metadata,
        request_context,
    );
}

pub async fn post_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    attribution: Option<Extension<RequestRejectionAttribution>>,
    MessagesBody(raw_body, request_api_key_id, ingress_id): MessagesBody,
) -> Response {
    post_messages_for_endpoint(
        state,
        headers,
        raw_body,
        "/v1/messages".to_string(),
        request_api_key_id,
        ingress_id,
        attribution.map(|Extension(value)| value),
    )
    .await
}

pub async fn post_messages_na(
    State(state): State<AppState>,
    headers: HeaderMap,
    attribution: Option<Extension<RequestRejectionAttribution>>,
    MessagesBody(raw_body, request_api_key_id, ingress_id): MessagesBody,
) -> Response {
    post_messages_for_endpoint(
        state,
        headers,
        raw_body,
        "/na/v1/messages".to_string(),
        request_api_key_id,
        ingress_id,
        attribution.map(|Extension(value)| value),
    )
    .await
}

pub async fn post_messages_ha(
    State(state): State<AppState>,
    headers: HeaderMap,
    attribution: Option<Extension<RequestRejectionAttribution>>,
    MessagesBody(raw_body, request_api_key_id, ingress_id): MessagesBody,
) -> Response {
    post_messages_for_endpoint(
        state,
        headers,
        raw_body,
        "/ha/v1/messages".to_string(),
        request_api_key_id,
        ingress_id,
        attribution.map(|Extension(value)| value),
    )
    .await
}

pub async fn post_messages_dfcache(
    State(state): State<AppState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    attribution: Option<Extension<RequestRejectionAttribution>>,
    MessagesBody(raw_body, request_api_key_id, ingress_id): MessagesBody,
) -> Response {
    post_messages_for_endpoint(
        state,
        headers,
        raw_body,
        uri.path().to_string(),
        request_api_key_id,
        ingress_id,
        attribution.map(|Extension(value)| value),
    )
    .await
}

pub(crate) async fn post_messages_for_endpoint(
    state: AppState,
    headers: HeaderMap,
    raw_body: Bytes,
    endpoint: String,
    request_api_key_id: Option<String>,
    ingress_id: Option<String>,
    attribution: Option<RequestRejectionAttribution>,
) -> Response {
    request_entry::handle_messages_endpoint(
        state,
        headers,
        raw_body,
        endpoint,
        request_api_key_id,
        ingress_id,
        attribution,
    )
    .await
}

pub async fn count_tokens(
    State(state): State<AppState>,
    JsonExtractor(payload): JsonExtractor<CountTokensRequest>,
) -> Response {
    count_tokens_for_endpoint(state, payload, "/v1/messages/count_tokens").await
}

pub async fn count_tokens_na(
    State(state): State<AppState>,
    JsonExtractor(payload): JsonExtractor<CountTokensRequest>,
) -> Response {
    count_tokens_for_endpoint(state, payload, "/na/v1/messages/count_tokens").await
}

pub async fn count_tokens_ha(
    State(state): State<AppState>,
    JsonExtractor(payload): JsonExtractor<CountTokensRequest>,
) -> Response {
    count_tokens_for_endpoint(state, payload, "/ha/v1/messages/count_tokens").await
}

pub async fn count_tokens_cc(
    State(state): State<AppState>,
    JsonExtractor(payload): JsonExtractor<CountTokensRequest>,
) -> Response {
    count_tokens_for_endpoint(state, payload, "/cc/v1/messages/count_tokens").await
}

fn multimodal_preprocessing_error_response(message: String) -> Response {
    if body_processing::is_remote_multimodal_capacity_error(&message) {
        let mut response = envelope::error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "api_error",
            "Remote media processing is temporarily at capacity. Retry shortly.",
        );
        response.headers_mut().insert(
            header::RETRY_AFTER,
            axum::http::HeaderValue::from_static("1"),
        );
        response
    } else {
        envelope::error_response(StatusCode::BAD_REQUEST, "invalid_request_error", message)
    }
}

async fn count_tokens_for_endpoint(
    state: AppState,
    mut payload: CountTokensRequest,
    endpoint: &str,
) -> Response {
    let capture_request_id = crate::diagnostics::capture::is_active().then(envelope::request_id);
    tracing::info!(
        endpoint,
        model = %payload.model,
        message_count = %payload.messages.len(),
        "Received POST messages/count_tokens request"
    );

    // Use the live runtime config, like the messages path, so admin changes to prompt
    // injection are reflected in token counts without a restart.
    let runtime_config = request_runtime_config(&state);
    // count_tokens 使用与 messages 相同的 system 注入后估算。
    super::prompt_injection::apply_to_count_tokens_request(
        &runtime_config.prompt_injection,
        &runtime_config.injections_for(endpoint),
        &mut payload,
    );

    let _multimodal_report = if let Some(image_processing) =
        runtime_config.route_features_for(endpoint).image_processing
    {
        match body_processing::prepare_multimodal_message_sources(
            &mut payload.messages,
            None,
            image_processing,
        )
        .await
        {
            Ok(report) => Some(report),
            Err(message) => {
                tracing::warn!("count_tokens 多模态 source 处理失败: {}", message);
                return multimodal_preprocessing_error_response(message);
            }
        }
    } else {
        None
    };

    let (total_tokens, calculation) = token::count_all_tokens_with_source(
        &payload.model,
        payload.system.as_deref(),
        &payload.messages,
        payload.tools.as_deref(),
        capture_request_id.as_deref(),
    );
    let input_tokens = total_tokens.max(1) as i32;
    if let Some(request_id) = capture_request_id.as_deref() {
        crate::diagnostics::capture::record("count_tokens_result", Some(request_id), || {
            serde_json::json!({
                "endpoint": endpoint,
                "model": payload.model,
                "inputTokens": input_tokens,
                "calculation": calculation,
            })
        });
    }
    let mut response = Json(CountTokensResponse { input_tokens }).into_response();
    if let Some(request_id) = capture_request_id {
        response
            .extensions_mut()
            .insert(crate::diagnostics::capture::CaptureRequestId(request_id));
    }
    response
}

pub async fn count_tokens_dfcache(
    State(state): State<AppState>,
    OriginalUri(uri): OriginalUri,
    JsonExtractor(payload): JsonExtractor<CountTokensRequest>,
) -> Response {
    count_tokens_for_endpoint(state, payload, uri.path()).await
}

pub async fn post_messages_cc(
    State(state): State<AppState>,
    headers: HeaderMap,
    attribution: Option<Extension<RequestRejectionAttribution>>,
    MessagesBody(raw_body, request_api_key_id, ingress_id): MessagesBody,
) -> Response {
    post_messages_for_endpoint(
        state,
        headers,
        raw_body,
        "/cc/v1/messages".to_string(),
        request_api_key_id,
        ingress_id,
        attribution.map(|Extension(value)| value),
    )
    .await
}

const REQUEST_RUNTIME_CONFIG_CACHE_MAX: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RequestRuntimeConfigCacheKey {
    runtime: usize,
    generation: u64,
    prompt_cache_simulation_mode: PromptCacheSimulationMode,
    prompt_cache_target_read_ratio_bits: u64,
    prompt_cache_token_scale_bits: u64,
}

type RequestRuntimeConfigCacheEntry = (RequestRuntimeConfigCacheKey, Arc<RequestRuntimeConfig>);

static REQUEST_RUNTIME_CONFIG_CACHE: std::sync::LazyLock<
    Mutex<Vec<RequestRuntimeConfigCacheEntry>>,
> = std::sync::LazyLock::new(|| Mutex::new(Vec::new()));

struct ThinkingModelDefaults {
    thinking_type: &'static str,
    budget_tokens: i32,
}

fn claude_public_version(model: &str, family: &str) -> Option<(u32, Option<u32>)> {
    let prefix = format!("claude-{family}-");
    let rest = model.strip_prefix(&prefix)?;
    let major_digits: String = rest.chars().take_while(|ch| ch.is_ascii_digit()).collect();
    if major_digits.is_empty() {
        return None;
    }
    let major = major_digits.parse().ok()?;
    let rest = &rest[major_digits.len()..];
    if rest.is_empty() {
        return Some((major, None));
    }
    let rest = rest.strip_prefix(['-', '.'])?;
    let minor_digits: String = rest.chars().take_while(|ch| ch.is_ascii_digit()).collect();
    if minor_digits.is_empty() || minor_digits.len() > 3 {
        return None;
    }
    Some((major, Some(minor_digits.parse().ok()?)))
}

fn model_version_uses_adaptive_thinking(model: &str, family: &str) -> bool {
    claude_public_version(model, family).is_some_and(|(major, minor)| {
        major >= 5 || (major == 4 && minor.is_some_and(|minor| minor >= 6))
    })
}

/// Opus/Sonnet/Haiku/Fable 4.6+ and the opus/sonnet/fable aliases take `thinking: adaptive`;
/// older models need `enabled` with `budget_tokens`.
fn base_model_uses_adaptive_thinking(model_base: &str) -> bool {
    matches!(
        model_base,
        "opus" | "opusplan" | "best" | "default" | "sonnet" | "fable"
    ) || ["opus", "sonnet", "haiku", "fable"]
        .iter()
        .any(|family| model_version_uses_adaptive_thinking(model_base, family))
}

fn thinking_model_defaults(model: &str) -> Option<ThinkingModelDefaults> {
    let (model_base, requested_thinking) = strip_model_compat_suffixes(model);
    if !requested_thinking {
        return None;
    }

    let adaptive = base_model_uses_adaptive_thinking(&model_base);
    Some(ThinkingModelDefaults {
        thinking_type: if adaptive { "adaptive" } else { "enabled" },
        budget_tokens: if adaptive { 0 } else { 20000 },
    })
}

/// 检测模型名是否包含 "thinking" 后缀，若包含则在调用方未显式配置时注入 thinking
///
/// - 调用方已指定 `thinking` 字段：保留原值
/// - 调用方未指定：按模型族注入默认 thinking，确保 `-thinking` 兼容模型有真实 thinking 输出
///   - Opus/Sonnet 4.6+ 和别名只注入 `adaptive`；effort 由目标能力合同决定
///   - 旧模型使用 `enabled` + `budget_tokens`
fn override_thinking_from_model_name(payload: &mut MessagesRequest) -> Result<(), String> {
    let Some(defaults) = thinking_model_defaults(&payload.model) else {
        return Ok(());
    };

    if payload.thinking.is_none() {
        tracing::info!(
            model = %payload.model,
            thinking_type = defaults.thinking_type,
            "模型名包含 thinking 后缀，注入默认 thinking 配置"
        );
        let budget_tokens = if defaults.thinking_type == "enabled" {
            if payload.max_tokens <= 1_024 {
                return Err(
                    "max_tokens must be greater than 1024 for an enabled thinking model"
                        .to_string(),
                );
            }
            defaults
                .budget_tokens
                .min(payload.max_tokens.saturating_sub(1))
        } else {
            defaults.budget_tokens
        };
        payload.thinking = Some(Thinking {
            thinking_type: defaults.thinking_type.to_string(),
            budget_tokens,
            display: None,
        });
    } else {
        tracing::debug!(
            model = %payload.model,
            "调用方已指定 thinking 配置，保留原值"
        );
    }

    Ok(())
}

fn apply_thinking_trigger_mode(payload: &mut MessagesRequest, trigger_mode: ThinkingTriggerMode) {
    let should_trigger = match trigger_mode {
        ThinkingTriggerMode::Always => true,
        ThinkingTriggerMode::RealRequest => {
            request_has_claude_code_visible_thinking_signal(payload)
        }
        ThinkingTriggerMode::Off => false,
    };
    if !should_trigger {
        return;
    }

    if payload
        .thinking
        .as_ref()
        .is_some_and(|thinking| thinking.thinking_type == "disabled")
    {
        return;
    }

    match payload.thinking.as_mut() {
        Some(thinking) if thinking.is_enabled() => {}
        Some(thinking) => {
            tracing::debug!(
                model = %payload.model,
                thinking_type = %thinking.thinking_type,
                trigger_mode = ?trigger_mode,
                "thinking 触发信号已匹配，但未知 thinking 类型保持原值并拒绝隐式触发"
            );
            return;
        }
        None => {
            payload.thinking = Some(Thinking {
                thinking_type: "adaptive".to_string(),
                budget_tokens: 0,
                display: None,
            });
        }
    }
}

fn request_has_claude_code_visible_thinking_signal(payload: &MessagesRequest) -> bool {
    latest_natural_user_text(payload)
        .as_deref()
        .is_some_and(user_text_has_claude_code_visible_thinking_signal)
}

fn latest_natural_user_text(payload: &MessagesRequest) -> Option<String> {
    payload
        .messages
        .iter()
        .rev()
        .filter(|message| message.role == "user")
        .filter_map(|message| {
            let text = text_from_user_content(&message.content);
            if text.trim().is_empty() {
                None
            } else {
                Some(text)
            }
        })
        .next()
}

fn text_from_user_content(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(text_from_user_content_block)
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(_) => text_from_user_content_block(content).unwrap_or_default(),
        _ => String::new(),
    }
}

fn text_from_user_content_block(block: &Value) -> Option<String> {
    let Some(object) = block.as_object() else {
        return block.as_str().map(ToOwned::to_owned);
    };
    if object
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|block_type| block_type == "tool_result")
    {
        return None;
    }
    object
        .get("text")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn user_text_has_claude_code_visible_thinking_signal(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("requesting deeper reasoning on this turn")
        || lower.contains("reason as thoroughly as the task warrants")
        || contains_ascii_word(&lower, "ultrathink")
}

fn contains_ascii_word(haystack: &str, needle: &str) -> bool {
    haystack.match_indices(needle).any(|(start, _)| {
        let before = haystack[..start].chars().next_back();
        let after = haystack[start + needle.len()..].chars().next();
        !before.is_some_and(is_ascii_word_char) && !after.is_some_and(is_ascii_word_char)
    })
}

fn is_ascii_word_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'
}

/// POST /v1/messages/count_tokens
///
/// 计算消息的 token 数量

fn external_route_request_raw(
    state: &AppState,
    runtime_config: &RequestRuntimeConfig,
    cache_route: &ResolvedCacheRoutePolicy,
    headers: HeaderMap,
    raw_body: Bytes,
    endpoint: &str,
    request_id: String,
    inference_attempt_budget: Arc<InferenceAttemptBudget>,
    request_api_key_id: Option<String>,
    ingress_id: Option<String>,
    raw_probe: Arc<RawMessagesBodyProbe>,
) -> AccountRouteRequest {
    let model_hint = raw_probe.model.clone();
    let stream_hint = raw_probe.stream;
    crate::diagnostics::capture::record("client_request", Some(&request_id), || {
        serde_json::json!({
            "route": "external_raw",
            "endpoint": endpoint,
            "requestedModel": model_hint,
            "stream": stream_hint,
            "routeSubtype": "External",
            "requestApiKeyId": request_api_key_id,
            "ingressId": ingress_id,
            "headers": crate::diagnostics::capture::http_headers_json(&headers),
            "rawBody": crate::diagnostics::capture::body_json(&raw_body),
        })
    });
    let effective_cache_route =
        cache_route_for_request_stream(cache_route.clone(), stream_hint.unwrap_or(false));
    let policy = &effective_cache_route.policy;
    let projection_payload = serde_json::from_slice::<MessagesRequest>(&raw_body).ok();
    AccountRouteRequest {
        effective_raw_body: raw_body.clone(),
        effective_raw_probe: Some(raw_probe),
        preparation_cache: Arc::new(AccountRouteRequestPreparationCache::default()),
        raw_body,
        headers,
        endpoint: endpoint.to_string(),
        payload: projection_payload,
        history_sanitize_enabled: false,
        model_hint,
        stream_hint,
        request_input_tokens: 0,
        upstream_model: None,
        model_resolution_source: None,
        model_resolution_note: None,
        route_subtype: UsageRouteSubtype::Account,
        reported_usage: reported_usage_config_for_policy(policy.reported_usage.clone()),
        prompt_cache: state.prompt_cache.clone(),
        prompt_cache_creation_controller: state.prompt_cache_creation_controller.clone(),
        prompt_cache_strategy_type: policy.cache_type,
        prompt_cache_simulation_mode: prompt_cache_simulation_mode_for_policy(policy),
        prompt_cache_route_namespace: effective_cache_route.namespace.clone(),
        cache_ttl_mode: policy.cache_ttl.account,
        prompt_cache_target_read_ratio: policy.simulation.target_read_ratio,
        prompt_cache_token_scale: policy.simulation.token_scale,
        prompt_cache_max_simulated_input_tokens: policy.simulation.max_simulated_input_tokens,
        prompt_cache_cap_jitter_min_tokens: policy.simulation.cap_jitter_min_tokens,
        prompt_cache_cap_jitter_max_tokens: policy.simulation.cap_jitter_max_tokens,
        prompt_cache_scale_min_input_tokens: policy.simulation.scale_min_input_tokens,
        prompt_cache_creation_control: policy.creation_control,
        prompt_cache_bounds: prompt_cache_bounds_for_policy(policy),
        prefix_tool_cache_policy: policy.prefix_tool,
        prefix_tool_plus_cache_policy: policy.prefix_tool_plus,
        stable_segment_cache_policy: policy.stable_segment,
        cache_hit_shaping_policy: policy.hit_shaping,
        model_capabilities: state.model_capabilities.clone(),
        pricing_catalog: state.pricing_catalog.clone(),
        request_id: request_id.clone(),
        // The public error ID is the request id so clients and logs share one identifier.
        error_id: request_id.clone(),
        recorder: state.usage_recorder.clone(),
        started_at: inference_attempt_budget.started_at().into(),
        stream_pre_output_hold: runtime_config.external_stream_pre_output_hold(),
        early_commit: None,
        first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        latency_trace: Arc::new(crate::account::ExternalLatencyTraceState::default()),
        payload_breakdown: None,
        payload_guard_report: None,
        payload_guard_external_enabled: false,
        payload_guard_initial_config: runtime_config.initial_payload_guard_config(),
        payload_guard_retry_config: None,
        inference_attempt_budget,
        request_api_key_id,
    }
}

#[allow(clippy::too_many_arguments)]
async fn post_messages_inner(
    state: AppState,
    headers: HeaderMap,
    effective_raw_body: Bytes,
    effective_raw_probe: Arc<RawMessagesBodyProbe>,
    raw_body: Bytes,
    endpoint: String,
    budget: Arc<InferenceAttemptBudget>,
    request_api_key_id: Option<String>,
    ingress_id: Option<String>,
    attribution: Option<RequestRejectionAttribution>,
) -> Response {
    let runtime = request_runtime_config(&state);
    let Some(manager) = state
        .account_manager
        .as_ref()
        .filter(|_| runtime.accounts.enabled)
    else {
        let request_id = envelope::request_id();
        let response = envelope::error_response_with_id_and_headers(
            StatusCode::from_u16(529).unwrap(),
            "overloaded_error",
            envelope::public_message_with_error_id(
                envelope::PUBLIC_PROVIDER_NOT_READY_MESSAGE,
                &request_id,
            ),
            &request_id,
            [("x-error-id", request_id.clone())],
        );
        record_pre_usage_rejection_with_metadata_and_context(
            attribution.as_ref(),
            RequestRejectionReason::ProviderNotReady,
            &endpoint,
            &response,
            None,
            Some(RequestRejectionUsageContext {
                model: effective_raw_probe.model.clone(),
                stream: effective_raw_probe.stream,
                requested_max_tokens: effective_raw_probe
                    .max_tokens
                    .and_then(|value| i32::try_from(value).ok()),
                ..Default::default()
            }),
        );
        return response;
    };
    let cache_route = runtime.cache_policy_for_path(&endpoint);
    let features = runtime.route_features_for(&endpoint);
    let mut route = external_route_request_raw(
        &state,
        &runtime,
        &cache_route,
        headers,
        effective_raw_body,
        &endpoint,
        envelope::request_id(),
        budget,
        request_api_key_id,
        ingress_id,
        effective_raw_probe,
    );
    route.raw_body = raw_body;
    route.history_sanitize_enabled = features.history_sanitize;
    route.payload_guard_retry_config = features
        .payload_guard
        .filter(|config| config.enabled && config.mode == PayloadGuardMode::OnTooLong)
        .map(|config| request_entry::route_guard_config(config, features.payload_shaping));

    manager
        .forward_with_failover(runtime.accounts.clone(), route)
        .await
}

#[cfg(test)]
#[path = "handlers/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "passthrough_tests.rs"]
mod passthrough_tests;
