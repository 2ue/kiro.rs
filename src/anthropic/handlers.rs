//! Anthropic API Handler 函数

use std::{
    collections::{HashMap, HashSet},
    convert::Infallible,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use crate::kiro::model::events::Event;
use crate::kiro::model::requests::kiro::KiroRequest;
use crate::kiro::parser::decoder::EventStreamDecoder;
use crate::model::config::{
    CompatProfile, Config, ExternalPoolsConfig, ModelMappingConfig, ModelResolutionMode,
    PayloadGuardMode, PayloadShapingConfig, PromptCacheCreationControlConfig,
    PromptCacheSimulationMode, ReportedUsageConfig,
};
use crate::token;
use anyhow::Error;
use axum::{
    Json as JsonExtractor,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Json, Response},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use bytes::Bytes;
use chrono::Utc;
use futures::{Stream, StreamExt, stream};
use reqwest::header::{CONTENT_TYPE as REQWEST_CONTENT_TYPE, LOCATION as REQWEST_LOCATION};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::{Instant, interval, sleep_until};

use super::converter::{
    ConversionError, ConverterOptions, convert_request_with_resolved_model,
    extract_stable_conversation_id, infer_document_media_type_from_url,
    infer_image_format_from_url,
};
use super::envelope;
use super::middleware::AppState;
use super::model_capabilities::{ModelResolution, ModelResolutionSource};
use super::payload_guard::{
    PayloadByteBreakdown, PayloadGuardConfig, PayloadGuardError, PayloadGuardReport,
    breakdown_anthropic_messages_request, breakdown_kiro_request, guard_anthropic_messages_request,
    guard_kiro_request,
};
use super::prompt_cache::{PromptCacheProfile, PromptCacheScope};
use super::stream::{SseEvent, StreamContext};
use super::types::{
    CountTokensRequest, CountTokensResponse, MessagesRequest, ModelsResponse, OutputConfig,
    Thinking,
};
use super::usage::{
    ExternalPoolAttempt, ExternalPoolUsageSnapshot, UsageRecord, UsageRecordStatus, UsageRouteKind,
    UsageRouteSubtype, UsageSource,
};
use super::websearch;
use crate::external_pool::{
    ExternalPoolFinalError, ExternalPoolForwardOutcome, ExternalPoolManager, ExternalRouteRequest,
};
use crate::http_client::response_bytes_with_body_timeout;
use crate::kiro::call_trace::KiroCredentialAttempt;
use crate::kiro::provider::{KiroProvider, KiroStreamCompletion};
use crate::kiro::token_manager::LocalPoolRouteStateKind;

const MAX_REMOTE_MULTIMODAL_BYTES: usize = 20 * 1024 * 1024;
const UPSTREAM_INVALID_REQUEST_MESSAGE: &str =
    "Invalid request. Simplify the message, tools, tool results, files, or images and retry.";

#[derive(Clone)]
struct RequestUsageContext {
    recorder: Arc<super::usage::UsageRecorder>,
    prompt_cache: Arc<super::prompt_cache::PromptCacheTracker>,
    prompt_cache_creation_controller:
        Arc<super::prompt_cache_creation_control::PromptCacheCreationController>,
    pricing_catalog: Arc<super::pricing::PricingCatalog>,
    request_id: String,
    endpoint: &'static str,
    stream: bool,
    model: String,
    upstream_model: Option<String>,
    model_resolution_source: Option<String>,
    model_resolution_note: Option<String>,
    conversation_id: Option<String>,
    prompt_cache_scope_conversation_id: Option<String>,
    input_tokens: i32,
    context_window_tokens: i32,
    prompt_cache_profile: Option<PromptCacheProfile>,
    simulation_mode: PromptCacheSimulationMode,
    prompt_cache_target_read_ratio: f64,
    prompt_cache_token_scale: f64,
    prompt_cache_max_simulated_input_tokens: i32,
    prompt_cache_cap_jitter_min_tokens: i32,
    prompt_cache_cap_jitter_max_tokens: i32,
    prompt_cache_scale_min_input_tokens: i32,
    prompt_cache_creation_control: PromptCacheCreationControlConfig,
    reported_cache_usage_policy: Option<super::cache::ReportedCacheUsagePolicy>,
    simulated_usage: Option<super::cache::CacheSimulation>,
    simulated_source: Option<UsageSource>,
    payload_breakdown: Option<PayloadByteBreakdown>,
    payload_guard_report: Option<PayloadGuardReport>,
    route_subtype_override: Option<UsageRouteSubtype>,
    fallback_reason: Option<String>,
    local_preflight: Option<serde_json::Value>,
    external_attempts: Vec<ExternalPoolAttempt>,
    started_at: Instant,
    first_token_latency_ms: Arc<AtomicU64>,
}

#[derive(Clone)]
struct ExternalFallbackContext {
    manager: Arc<ExternalPoolManager>,
    config: ExternalPoolsConfig,
    raw_body: Bytes,
    headers: HeaderMap,
    endpoint: &'static str,
    payload: MessagesRequest,
    model_resolution: Option<ModelResolution>,
    reported_usage: ReportedUsageConfig,
    prompt_cache: Arc<super::prompt_cache::PromptCacheTracker>,
    prompt_cache_creation_controller:
        Arc<super::prompt_cache_creation_control::PromptCacheCreationController>,
    prompt_cache_target_read_ratio: f64,
    prompt_cache_token_scale: f64,
    prompt_cache_max_simulated_input_tokens: i32,
    prompt_cache_cap_jitter_min_tokens: i32,
    prompt_cache_cap_jitter_max_tokens: i32,
    prompt_cache_scale_min_input_tokens: i32,
    prompt_cache_creation_control: PromptCacheCreationControlConfig,
    model_capabilities: Arc<super::model_capabilities::ModelCapabilitiesCatalog>,
    pricing_catalog: Arc<super::pricing::PricingCatalog>,
    recorder: Arc<super::usage::UsageRecorder>,
    payload_guard_external_enabled: bool,
    payload_guard_initial_config: PayloadGuardConfig,
    payload_guard_retry_config: Option<PayloadGuardConfig>,
}

#[derive(Clone)]
struct CredentialUsageContext {
    request: RequestUsageContext,
    credential_id: Option<u64>,
    credential_label: Option<String>,
    sticky_bound: bool,
    fallback_from_sticky: bool,
    credential_attempts: Vec<KiroCredentialAttempt>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CredentialErrorHint {
    id: u64,
    label: Option<String>,
}

#[derive(Debug, Clone)]
struct RequestRuntimeConfig {
    extract_thinking: bool,
    prompt_cache_target_read_ratio: f64,
    prompt_cache_token_scale: f64,
    prompt_cache_max_simulated_input_tokens: i32,
    prompt_cache_cap_jitter_min_tokens: i32,
    prompt_cache_cap_jitter_max_tokens: i32,
    prompt_cache_scale_min_input_tokens: i32,
    prompt_cache_creation_control: PromptCacheCreationControlConfig,
    reported_usage: ReportedUsageConfig,
    compat_profile: CompatProfile,
    model_resolution_mode: ModelResolutionMode,
    model_mapping: ModelMappingConfig,
    expose_proxy_warnings: bool,
    payload_guard_enabled: bool,
    payload_guard_mode: PayloadGuardMode,
    payload_guard_max_bytes: usize,
    payload_guard_safety_margin_bytes: usize,
    payload_guard_trim_history: bool,
    payload_guard_external_enabled: bool,
    payload_shaping: PayloadShapingConfig,
}

impl RequestRuntimeConfig {
    fn from_app_state(state: &AppState) -> Self {
        Self {
            extract_thinking: state.extract_thinking,
            prompt_cache_target_read_ratio: state.prompt_cache_target_read_ratio,
            prompt_cache_token_scale: state.prompt_cache_token_scale,
            prompt_cache_max_simulated_input_tokens: state.prompt_cache_max_simulated_input_tokens,
            prompt_cache_cap_jitter_min_tokens: state.prompt_cache_cap_jitter_min_tokens,
            prompt_cache_cap_jitter_max_tokens: state.prompt_cache_cap_jitter_max_tokens,
            prompt_cache_scale_min_input_tokens: state.prompt_cache_scale_min_input_tokens,
            prompt_cache_creation_control: state.prompt_cache_creation_control,
            reported_usage: state.reported_usage.clone(),
            compat_profile: state.compat_profile,
            model_resolution_mode: state.model_resolution_mode,
            model_mapping: state.model_mapping.clone().normalized(),
            expose_proxy_warnings: state.expose_proxy_warnings,
            payload_guard_enabled: state.payload_guard_enabled,
            payload_guard_mode: state.payload_guard_mode,
            payload_guard_max_bytes: state.payload_guard_max_bytes,
            payload_guard_safety_margin_bytes: state.payload_guard_safety_margin_bytes,
            payload_guard_trim_history: state.payload_guard_trim_history,
            payload_guard_external_enabled: state.payload_guard_external_enabled,
            payload_shaping: state.payload_shaping,
        }
    }

    fn from_config_with_fallback(config: &Config, fallback: Self) -> Self {
        Self {
            extract_thinking: config.extract_thinking,
            prompt_cache_target_read_ratio: if config.prompt_cache_target_read_ratio.is_finite() {
                config.prompt_cache_target_read_ratio.clamp(0.0, 0.99)
            } else {
                fallback.prompt_cache_target_read_ratio
            },
            prompt_cache_token_scale: if config.prompt_cache_token_scale.is_finite() {
                config.prompt_cache_token_scale.clamp(1.0, 3.0)
            } else {
                fallback.prompt_cache_token_scale
            },
            prompt_cache_max_simulated_input_tokens: config
                .prompt_cache_max_simulated_input_tokens
                .max(0),
            prompt_cache_cap_jitter_min_tokens: config.prompt_cache_cap_jitter_min_tokens.max(0),
            prompt_cache_cap_jitter_max_tokens: config.prompt_cache_cap_jitter_max_tokens.max(0),
            prompt_cache_scale_min_input_tokens: config.prompt_cache_scale_min_input_tokens.max(0),
            prompt_cache_creation_control: config.prompt_cache_creation_control.normalized(),
            reported_usage: config.reported_usage.normalized(),
            compat_profile: config.compat_profile,
            model_resolution_mode: config.model_resolution_mode,
            model_mapping: config.model_mapping.clone().normalized(),
            expose_proxy_warnings: config.expose_proxy_warnings || config.compat_profile.is_debug(),
            payload_guard_enabled: config.payload_guard_enabled,
            payload_guard_mode: config.payload_guard_mode,
            payload_guard_max_bytes: config.payload_guard_max_bytes,
            payload_guard_safety_margin_bytes: config.payload_guard_safety_margin_bytes,
            payload_guard_trim_history: config.payload_guard_trim_history,
            payload_guard_external_enabled: config.payload_guard_external_enabled,
            payload_shaping: config.payload_shaping,
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
}

#[derive(Clone)]
struct PayloadTooLongRetryRequest {
    request: KiroRequest,
    config: PayloadGuardConfig,
    endpoint: &'static str,
    requested_model: String,
    upstream_model: Option<String>,
    conversation_id: String,
    conversion_warnings: Option<String>,
}

impl PayloadTooLongRetryRequest {
    fn new(
        request: KiroRequest,
        runtime_config: &RequestRuntimeConfig,
        endpoint: &'static str,
        requested_model: &str,
        upstream_model: Option<&str>,
        conversation_id: &str,
        conversion_warnings: Option<String>,
    ) -> Option<Self> {
        runtime_config.too_long_retry_enabled().then(|| Self {
            request,
            config: runtime_config.payload_guard_config(),
            endpoint,
            requested_model: requested_model.to_string(),
            upstream_model: upstream_model.map(str::to_string),
            conversation_id: conversation_id.to_string(),
            conversion_warnings,
        })
    }

    fn build_retry_body(
        self,
        usage_context: &mut RequestUsageContext,
    ) -> Result<(String, Option<String>), PayloadGuardError> {
        let mut request = self.request;
        let (request_body, report) = guard_kiro_request(&mut request, self.config)?;
        log_payload_guard_report(
            &report,
            self.endpoint,
            &self.requested_model,
            self.upstream_model.as_deref(),
            Some(&self.conversation_id),
        );
        let breakdown = breakdown_kiro_request(&request, &request_body);
        log_payload_byte_breakdown(
            should_log_payload_byte_breakdown(&report).then_some(breakdown),
            &report,
            self.endpoint,
            &self.requested_model,
            self.upstream_model.as_deref(),
            Some(&self.conversation_id),
        );
        usage_context.set_payload_diagnostics(Some(breakdown), report.clone());
        let warnings_header = merge_warning_headers(self.conversion_warnings, Some(&report));
        Ok((request_body, warnings_header))
    }
}

fn request_runtime_config(state: &AppState, provider: &KiroProvider) -> RequestRuntimeConfig {
    RequestRuntimeConfig::from_config_with_fallback(
        &provider.runtime_config(),
        RequestRuntimeConfig::from_app_state(state),
    )
}

fn parse_messages_payload(raw_body: &Bytes) -> Result<MessagesRequest, Response> {
    let payload = serde_json::from_slice::<MessagesRequest>(raw_body).map_err(|err| {
        envelope::error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            format!("Invalid JSON body: {}", err),
        )
    })?;
    if payload.model.trim().is_empty() {
        return Err(envelope::error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "model: field is required and cannot be empty",
        ));
    }
    Ok(payload)
}

fn build_external_fallback_context(
    state: &AppState,
    provider: &KiroProvider,
    runtime_config: &RequestRuntimeConfig,
    endpoint: &'static str,
    raw_body: Bytes,
    headers: HeaderMap,
    payload: &MessagesRequest,
) -> Option<ExternalFallbackContext> {
    let manager = state.external_pool_manager.clone()?;
    let config = provider.runtime_config().external_pools;
    config
        .external_pools_enabled
        .then_some(ExternalFallbackContext {
            manager,
            config,
            raw_body,
            headers,
            endpoint,
            payload: payload.clone(),
            model_resolution: None,
            reported_usage: runtime_config.reported_usage.clone(),
            prompt_cache: state.prompt_cache.clone(),
            prompt_cache_creation_controller: state.prompt_cache_creation_controller.clone(),
            prompt_cache_target_read_ratio: runtime_config.prompt_cache_target_read_ratio,
            prompt_cache_token_scale: runtime_config.prompt_cache_token_scale,
            prompt_cache_max_simulated_input_tokens: runtime_config
                .prompt_cache_max_simulated_input_tokens,
            prompt_cache_cap_jitter_min_tokens: runtime_config.prompt_cache_cap_jitter_min_tokens,
            prompt_cache_cap_jitter_max_tokens: runtime_config.prompt_cache_cap_jitter_max_tokens,
            prompt_cache_scale_min_input_tokens: runtime_config.prompt_cache_scale_min_input_tokens,
            prompt_cache_creation_control: runtime_config.prompt_cache_creation_control,
            model_capabilities: state.model_capabilities.clone(),
            pricing_catalog: state.pricing_catalog.clone(),
            recorder: state.usage_recorder.clone(),
            payload_guard_external_enabled: runtime_config.payload_guard_external_enabled,
            payload_guard_initial_config: runtime_config.initial_payload_guard_config(),
            payload_guard_retry_config: runtime_config
                .too_long_retry_enabled()
                .then(|| runtime_config.payload_guard_config()),
        })
}

impl ExternalFallbackContext {
    fn refresh_payload(&mut self, payload: &MessagesRequest) {
        self.payload = payload.clone();
        self.raw_body = serialize_messages_request_body(payload);
    }

    async fn should_fail_fast_local(&self) -> bool {
        if !local_pool_capacity_fail_fast_enabled(&self.config) {
            return false;
        }
        self.manager.has_available_pool(&self.config).await
    }

    async fn direct_policy_response(&self, request_id: &str) -> Option<Response> {
        let reason = self
            .manager
            .direct_policy_reason(&self.config, self.endpoint, &self.payload.model)
            .await?;
        Some(
            self.manager
                .forward_with_failover(
                    self.config.clone(),
                    self.route_request(
                        request_id.to_string(),
                        UsageRouteSubtype::ExternalDirectPolicy,
                        None,
                        Some(reason),
                        false,
                        None,
                        Vec::new(),
                    ),
                )
                .await,
        )
    }

    async fn local_pool_preflight_outcome(
        &self,
        provider: &KiroProvider,
        request_id: &str,
        model: &str,
    ) -> Option<ExternalPoolForwardOutcome> {
        if !self.config.local_pool_preflight_enabled {
            return None;
        }
        let state = provider.local_pool_route_state(Some(model));
        if !state.kind.should_route_external() {
            return None;
        }
        let Some(reason) = local_pool_route_fallback_reason(state.kind, &self.config) else {
            return None;
        };
        if !self.manager.has_eligible_pool(&self.config).await {
            return None;
        }

        let reason = reason.to_string();
        tracing::warn!(
            request_id,
            reason = %reason,
            local_total = state.total,
            local_available = state.available,
            local_dispatchable = state.dispatchable,
            local_usable = state.usable,
            retry_after_secs = ?state.retry_after_secs,
            "local credential pool is not immediately schedulable; routing request directly to external pool"
        );
        Some(
            self.manager
                .forward_with_failover_result(
                    self.config.clone(),
                    self.route_request(
                        request_id.to_string(),
                        UsageRouteSubtype::ExternalFallbackPreflight,
                        Some(reason.clone()),
                        None,
                        false,
                        Some(json!({
                            "reason": reason,
                            "state": state,
                        })),
                        Vec::new(),
                    ),
                )
                .await,
        )
    }

    async fn fallback_after_local_error(
        &self,
        request_id: &str,
        error_message: &str,
        local_attempts: Vec<KiroCredentialAttempt>,
    ) -> Option<Response> {
        match self
            .fallback_after_local_error_outcome(request_id, error_message, local_attempts)
            .await?
        {
            ExternalPoolForwardOutcome::Response(response) => Some(response),
            ExternalPoolForwardOutcome::FinalError(err) => Some(err.into_response(request_id)),
        }
    }

    async fn fallback_after_local_error_outcome(
        &self,
        request_id: &str,
        error_message: &str,
        local_attempts: Vec<KiroCredentialAttempt>,
    ) -> Option<ExternalPoolForwardOutcome> {
        let reason = classify_local_error_for_external_fallback(
            error_message,
            &local_attempts,
            &self.config,
        )?;
        if self.config.local_pool_circuit_enabled {
            let mut seen_credentials = HashSet::new();
            let mut recorded = false;
            for attempt in &local_attempts {
                if seen_credentials.insert(attempt.credential_id) {
                    recorded = true;
                    let _ = self
                        .manager
                        .record_local_pool_failure(
                            &self.config,
                            Some(attempt.credential_id),
                            &reason,
                        )
                        .await;
                }
            }
            if !recorded {
                let _ = self
                    .manager
                    .record_local_pool_failure(&self.config, None, &reason)
                    .await;
            }
        }
        if !self.manager.has_eligible_pool(&self.config).await {
            return None;
        }
        let local_preflight = Some(json!({
            "reason": reason.clone(),
            "error": error_message,
            "attemptCount": local_attempts.len(),
        }));
        let route_subtype = if local_attempts.is_empty() {
            UsageRouteSubtype::ExternalFallbackPreflight
        } else {
            UsageRouteSubtype::ExternalFallbackAfterLocalAttempts
        };
        Some(
            self.manager
                .forward_with_failover_result(
                    self.config.clone(),
                    self.route_request(
                        request_id.to_string(),
                        route_subtype,
                        Some(reason),
                        None,
                        true,
                        local_preflight,
                        local_attempts,
                    ),
                )
                .await,
        )
    }

    fn route_request(
        &self,
        request_id: String,
        route_subtype: UsageRouteSubtype,
        fallback_reason: Option<String>,
        direct_policy_reason: Option<String>,
        local_attempted: bool,
        local_preflight: Option<serde_json::Value>,
        local_attempts: Vec<KiroCredentialAttempt>,
    ) -> ExternalRouteRequest {
        let guarded_payload = self.guarded_route_payload();
        ExternalRouteRequest {
            raw_body: guarded_payload.raw_body,
            headers: self.headers.clone(),
            endpoint: self.endpoint,
            payload: guarded_payload.payload,
            upstream_model: self
                .model_resolution
                .as_ref()
                .and_then(|resolution| resolution.upstream_model.clone()),
            model_resolution_source: self
                .model_resolution
                .as_ref()
                .map(|resolution| resolution.source.as_str().to_string()),
            model_resolution_note: self
                .model_resolution
                .as_ref()
                .and_then(|resolution| resolution.note.clone()),
            route_subtype,
            fallback_reason,
            direct_policy_reason,
            local_attempted,
            local_preflight,
            local_attempts,
            reported_usage: self.reported_usage.clone(),
            prompt_cache: self.prompt_cache.clone(),
            prompt_cache_creation_controller: self.prompt_cache_creation_controller.clone(),
            prompt_cache_target_read_ratio: self.prompt_cache_target_read_ratio,
            prompt_cache_token_scale: self.prompt_cache_token_scale,
            prompt_cache_max_simulated_input_tokens: self.prompt_cache_max_simulated_input_tokens,
            prompt_cache_cap_jitter_min_tokens: self.prompt_cache_cap_jitter_min_tokens,
            prompt_cache_cap_jitter_max_tokens: self.prompt_cache_cap_jitter_max_tokens,
            prompt_cache_scale_min_input_tokens: self.prompt_cache_scale_min_input_tokens,
            prompt_cache_creation_control: self.prompt_cache_creation_control,
            model_capabilities: self.model_capabilities.clone(),
            pricing_catalog: self.pricing_catalog.clone(),
            request_id,
            recorder: self.recorder.clone(),
            started_at: Instant::now(),
            first_token_latency_ms: Arc::new(AtomicU64::new(0)),
            payload_breakdown: guarded_payload.payload_breakdown,
            payload_guard_report: guarded_payload.payload_guard_report,
            payload_guard_retry_config: guarded_payload.payload_guard_retry_config,
        }
    }

    fn guarded_route_payload(&self) -> GuardedExternalRoutePayload {
        let retry_config = self
            .payload_guard_external_enabled
            .then_some(())
            .and(self.payload_guard_retry_config);
        if !self.payload_guard_external_enabled {
            return GuardedExternalRoutePayload {
                raw_body: self.raw_body.clone(),
                payload: self.payload.clone(),
                payload_breakdown: None,
                payload_guard_report: None,
                payload_guard_retry_config: None,
            };
        }

        let mut payload = self.payload.clone();
        let guard_config = self.payload_guard_initial_config;
        match guard_anthropic_messages_request(&mut payload, guard_config, self.raw_body.len()) {
            Ok((body, report)) => {
                let should_send_serialized = report.was_modified()
                    || (guard_config.max_bytes > 0
                        && self.raw_body.len() > guard_config.max_bytes
                        && body.len() <= self.raw_body.len());
                let raw_body = if should_send_serialized {
                    Bytes::from(body)
                } else {
                    self.raw_body.clone()
                };
                let include_diagnostics = should_log_payload_byte_breakdown(&report)
                    || (guard_config.max_bytes > 0 && self.raw_body.len() > guard_config.max_bytes);
                let breakdown = include_diagnostics
                    .then(|| breakdown_anthropic_messages_request(&payload, raw_body.len()));
                if include_diagnostics {
                    log_payload_guard_report(
                        &report,
                        self.endpoint,
                        &self.payload.model,
                        self.model_resolution
                            .as_ref()
                            .and_then(|resolution| resolution.upstream_model.as_deref()),
                        extract_stable_conversation_id(&payload).as_deref(),
                    );
                    log_payload_byte_breakdown(
                        breakdown,
                        &report,
                        self.endpoint,
                        &self.payload.model,
                        self.model_resolution
                            .as_ref()
                            .and_then(|resolution| resolution.upstream_model.as_deref()),
                        extract_stable_conversation_id(&payload).as_deref(),
                    );
                }
                GuardedExternalRoutePayload {
                    raw_body,
                    payload,
                    payload_breakdown: breakdown,
                    payload_guard_report: include_diagnostics.then_some(report),
                    payload_guard_retry_config: retry_config,
                }
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    endpoint = self.endpoint,
                    model = %self.payload.model,
                    "external pool payload guard failed; forwarding original request body"
                );
                GuardedExternalRoutePayload {
                    raw_body: self.raw_body.clone(),
                    payload: self.payload.clone(),
                    payload_breakdown: None,
                    payload_guard_report: None,
                    payload_guard_retry_config: None,
                }
            }
        }
    }
}

fn local_pool_capacity_fail_fast_enabled(config: &ExternalPoolsConfig) -> bool {
    config.local_pool_preflight_enabled && config.fallback_on_local_capacity_exhausted
}

fn local_pool_route_fallback_reason(
    kind: LocalPoolRouteStateKind,
    config: &ExternalPoolsConfig,
) -> Option<&'static str> {
    match kind {
        LocalPoolRouteStateKind::Ready => None,
        LocalPoolRouteStateKind::NoCredentials if config.fallback_on_no_available_credentials => {
            Some("local_no_credentials")
        }
        LocalPoolRouteStateKind::AllDisabled if config.fallback_on_no_available_credentials => {
            Some("local_all_disabled")
        }
        LocalPoolRouteStateKind::ProxyBlocked if config.fallback_on_no_available_credentials => {
            Some("local_proxy_blocked")
        }
        LocalPoolRouteStateKind::NoModelCompatible if config.fallback_on_unsupported_model => {
            Some("local_no_model_compatible")
        }
        LocalPoolRouteStateKind::AllCoolingDown if config.fallback_on_local_transient_exhausted => {
            Some("local_all_cooling_down")
        }
        LocalPoolRouteStateKind::CapacityFull if config.fallback_on_local_capacity_exhausted => {
            Some("local_capacity_full")
        }
        _ => None,
    }
}

struct GuardedExternalRoutePayload {
    raw_body: Bytes,
    payload: MessagesRequest,
    payload_breakdown: Option<PayloadByteBreakdown>,
    payload_guard_report: Option<PayloadGuardReport>,
    payload_guard_retry_config: Option<PayloadGuardConfig>,
}

fn serialize_messages_request_body(payload: &MessagesRequest) -> Bytes {
    serde_json::to_vec(payload)
        .map(Bytes::from)
        .unwrap_or_default()
}

fn classify_local_error_for_external_fallback(
    message: &str,
    attempts: &[KiroCredentialAttempt],
    config: &ExternalPoolsConfig,
) -> Option<String> {
    let lower = message.to_ascii_lowercase();
    if config.fallback_on_unsupported_model && is_unsupported_model_error(&lower, attempts) {
        return Some("unsupported_model".to_string());
    }
    if is_request_error_that_must_not_fallback(&lower, attempts) {
        return None;
    }
    if config.fallback_on_local_capacity_exhausted
        && (lower.contains("本地凭据调度容量暂不可用")
            || lower.contains("凭据调度等待队列已满")
            || lower.contains("排队等待超时")
            || lower.contains("并发槽位已满")
            || lower.contains("临时可调度: 0")
            || lower.contains("max_concurrent_requests"))
    {
        return Some("local_capacity_exhausted".to_string());
    }
    if config.fallback_on_local_transient_exhausted
        && (lower.contains("临时冷却")
            || lower.contains("429")
            || lower.contains("too many")
            || lower.contains("rate limit")
            || lower.contains("server_error")
            || lower.contains("transient")
            || lower.contains("network")
            || lower.contains("send_error")
            || lower.contains("stream_error")
            || lower.contains("502")
            || lower.contains("503")
            || lower.contains("504"))
    {
        return Some("local_transient_exhausted".to_string());
    }
    if config.fallback_on_no_available_credentials
        && (lower.contains("所有凭据")
            || lower.contains("所有可用凭据")
            || lower.contains("所有凭据已用尽")
            || lower.contains("无可用凭据")
            || lower.contains("quota_exhausted")
            || lower.contains("risk_control")
            || lower.contains("credential_failure"))
    {
        return Some("no_available_credentials".to_string());
    }
    let last_error_type = attempts
        .last()
        .and_then(|attempt| attempt.error_type.as_deref())
        .unwrap_or_default();
    match last_error_type {
        "transient_error" | "send_error" | "server_error" | "non_eventstream"
            if config.fallback_on_local_transient_exhausted =>
        {
            Some("local_transient_exhausted".to_string())
        }
        "quota_exhausted" | "risk_control" | "credential_failure"
            if config.fallback_on_no_available_credentials =>
        {
            Some("no_available_credentials".to_string())
        }
        _ => None,
    }
}

fn is_unsupported_model_error(lower_message: &str, attempts: &[KiroCredentialAttempt]) -> bool {
    if lower_message.contains("invalid_model_id")
        || lower_message.contains("invalid model")
        || lower_message.contains("model_not_found")
        || lower_message.contains("model not found")
        || lower_message.contains("unsupported model")
        || lower_message.contains("模型不支持")
        || lower_message.contains("没有支持当前模型")
    {
        return true;
    }

    attempts.iter().any(|attempt| {
        matches!(
            attempt.error_type.as_deref(),
            Some("unsupported_model") | Some("invalid_model") | Some("invalid_model_id")
        )
    })
}

fn is_request_error_that_must_not_fallback(
    lower_message: &str,
    attempts: &[KiroCredentialAttempt],
) -> bool {
    if lower_message.contains("bad request")
        || lower_message.contains("invalid_request")
        || lower_message.contains("content_length_exceeds_threshold")
        || lower_message.contains("input is too long")
        || lower_message.contains("context window is full")
        || lower_message.contains("improperly formed")
        || lower_message.contains("json schema is invalid")
        || lower_message.contains("invalid json")
        || lower_message.contains("tool schema")
    {
        return true;
    }
    attempts.iter().any(|attempt| {
        matches!(
            attempt.error_type.as_deref(),
            Some("bad_request") | Some("client_error") | Some("invalid_request_error")
        ) || attempt.status == Some(400)
    })
}

impl CredentialErrorHint {
    fn display_label(&self) -> String {
        credential_display_label(self.id, self.label.as_deref())
    }
}

impl RequestUsageContext {
    fn first_token_latency_ms(&self) -> Option<u64> {
        let value = self.first_token_latency_ms.load(Ordering::Acquire);
        (value > 0).then_some(value)
    }

    fn mark_first_token_if_output(&self, events: &[SseEvent]) {
        if events.iter().any(is_first_token_output_event) {
            let elapsed = self.started_at.elapsed().as_millis().max(1) as u64;
            let _ = self.first_token_latency_ms.compare_exchange(
                0,
                elapsed,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }

    fn cache_amplification(&self) -> Option<super::cache::CacheAmplification> {
        if self.simulation_mode != PromptCacheSimulationMode::HighCache {
            return None;
        }

        Some(super::cache::CacheAmplification::new(
            self.prompt_cache_token_scale,
            self.prompt_cache_max_simulated_input_tokens,
            self.prompt_cache_cap_jitter_min_tokens,
            self.prompt_cache_cap_jitter_max_tokens,
            self.prompt_cache_scale_min_input_tokens,
            self.prompt_cache_profile
                .as_ref()
                .map(|profile| profile.cache_jitter_seed())
                .unwrap_or(0),
        ))
    }

    fn attach_credential(
        self,
        credential_id: Option<u64>,
        credential_label: Option<String>,
        sticky_bound: bool,
        fallback_from_sticky: bool,
        credential_attempts: Vec<KiroCredentialAttempt>,
    ) -> CredentialUsageContext {
        CredentialUsageContext {
            request: self,
            credential_id,
            credential_label,
            sticky_bound,
            fallback_from_sticky,
            credential_attempts,
        }
    }

    fn with_payload_diagnostics(
        mut self,
        breakdown: Option<PayloadByteBreakdown>,
        report: PayloadGuardReport,
    ) -> Self {
        self.set_payload_diagnostics(breakdown, report);
        self
    }

    fn set_payload_diagnostics(
        &mut self,
        breakdown: Option<PayloadByteBreakdown>,
        report: PayloadGuardReport,
    ) {
        self.payload_breakdown = breakdown;
        self.payload_guard_report = Some(report);
    }

    fn mark_local_rescue_after_external(
        &mut self,
        reason: impl Into<String>,
        local_preflight: Option<serde_json::Value>,
        external_attempts: Vec<ExternalPoolAttempt>,
    ) {
        self.route_subtype_override = Some(UsageRouteSubtype::LocalRescueAfterExternal);
        self.fallback_reason = Some(reason.into());
        self.local_preflight = local_preflight;
        self.external_attempts = external_attempts;
    }

    fn attach_provider_error_credential(
        self,
        provider: &crate::kiro::provider::KiroProvider,
        error_message: &str,
        credential_attempts: Vec<KiroCredentialAttempt>,
    ) -> CredentialUsageContext {
        let hint = extract_credential_error_hint(error_message);
        let attempt_hint = credential_attempts.last();
        let credential_id = hint
            .as_ref()
            .map(|hint| hint.id)
            .or_else(|| attempt_hint.map(|attempt| attempt.credential_id));
        let credential_label = credential_id
            .and_then(|id| {
                provider
                    .credential_label(id)
                    .or_else(|| hint.as_ref().and_then(|hint| hint.label.clone()))
                    .or_else(|| attempt_hint.and_then(|attempt| attempt.credential_label.clone()))
            })
            .or_else(|| hint.and_then(|hint| hint.label));

        self.attach_credential(
            credential_id,
            credential_label,
            false,
            false,
            credential_attempts,
        )
    }

    fn reported_cache_usage_policy(&self) -> Option<super::cache::ReportedCacheUsagePolicy> {
        self.reported_cache_usage_policy.clone()
    }

    fn reported_usage_for_downstream(
        &self,
        usage: super::cache::CacheUsage,
        usage_source: UsageSource,
    ) -> super::cache::CacheUsage {
        if usage_source != UsageSource::LocalPromptCache
            || self.simulation_mode != PromptCacheSimulationMode::HighCache
        {
            return usage;
        }

        self.reported_cache_usage_policy
            .clone()
            .map(|policy| {
                usage.with_reported_cache_usage_policy_and_raw(
                    policy,
                    super::cache::RawUsage::uncached(self.input_tokens, usage.output_tokens),
                )
            })
            .unwrap_or(usage)
    }

    fn ensure_reported_usage_for_record(
        &self,
        usage: super::cache::CacheUsage,
        usage_source: UsageSource,
    ) -> super::cache::CacheUsage {
        if usage_source != UsageSource::LocalPromptCache
            || self.simulation_mode != PromptCacheSimulationMode::HighCache
        {
            return usage;
        }

        let Some(policy) = self.reported_cache_usage_policy.clone() else {
            return usage;
        };

        if policy.should_rewrite_local_prompt_cache_usage(usage) {
            usage.with_reported_cache_usage_policy_and_raw(
                policy,
                super::cache::RawUsage::uncached(self.input_tokens, usage.output_tokens),
            )
        } else {
            usage
        }
    }
}

fn is_first_token_output_event(event: &SseEvent) -> bool {
    match event.event.as_str() {
        "content_block_delta" => {
            let Some(delta) = event.data.get("delta").and_then(|value| value.as_object()) else {
                return false;
            };
            match delta.get("type").and_then(|value| value.as_str()) {
                Some("text_delta") => delta
                    .get("text")
                    .and_then(|value| value.as_str())
                    .is_some_and(|text| !text.is_empty()),
                Some("thinking_delta") => delta
                    .get("thinking")
                    .and_then(|value| value.as_str())
                    .is_some_and(|thinking| !thinking.is_empty()),
                Some("input_json_delta") => delta
                    .get("partial_json")
                    .and_then(|value| value.as_str())
                    .is_some_and(|json| !json.is_empty()),
                _ => false,
            }
        }
        "content_block_start" => event
            .data
            .get("content_block")
            .and_then(|value| value.get("type"))
            .and_then(|value| value.as_str())
            .is_some_and(|block_type| {
                matches!(
                    block_type,
                    "tool_use" | "server_tool_use" | "redacted_thinking"
                )
            }),
        _ => false,
    }
}

fn reported_cache_usage_policy(
    endpoint: &str,
    simulation_mode: PromptCacheSimulationMode,
    reported_usage: &ReportedUsageConfig,
    seed: u64,
) -> Option<super::cache::ReportedCacheUsagePolicy> {
    if simulation_mode != PromptCacheSimulationMode::HighCache {
        return None;
    }

    super::cache::ReportedCacheUsagePolicy::from_path_policy(
        reported_usage.policy_for_path(endpoint),
        seed,
    )
}

fn should_build_local_prompt_cache_usage(simulation_mode: PromptCacheSimulationMode) -> bool {
    simulation_mode == PromptCacheSimulationMode::HighCache
}

fn usage_snapshot(usage: super::cache::CacheUsage) -> ExternalPoolUsageSnapshot {
    ExternalPoolUsageSnapshot {
        total_input_tokens: usage.total_input_tokens,
        input_tokens: usage.input_tokens,
        billable_input_tokens: usage.billable_input_tokens(),
        output_tokens: usage.output_tokens,
        cache_read_input_tokens: usage.cache_read_input_tokens,
        cache_creation_input_tokens: usage.cache_creation_input_tokens,
        cache_creation_5m_input_tokens: usage.cache_creation_5m_input_tokens,
        cache_creation_1h_input_tokens: usage.cache_creation_1h_input_tokens,
    }
}

fn raw_usage_from_metadata_or_estimate(
    metadata_usage: Option<&crate::kiro::model::events::MetadataTokenUsage>,
    input_tokens: i32,
    output_tokens: i32,
) -> super::cache::CacheUsage {
    metadata_usage
        .map(|usage| super::cache::CacheUsage {
            total_input_tokens: usage.total_input_tokens(),
            input_tokens: usage.input_tokens(),
            output_tokens: usage.output_tokens,
            cache_creation_input_tokens: usage.cache_write_input_tokens,
            cache_read_input_tokens: usage.cache_read_input_tokens,
            cache_creation_5m_input_tokens: usage.cache_write_input_tokens,
            cache_creation_1h_input_tokens: 0,
        })
        .unwrap_or(super::cache::CacheUsage {
            total_input_tokens: input_tokens,
            input_tokens,
            output_tokens,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        })
}

fn credential_display_label(id: u64, label: Option<&str>) -> String {
    let prefix = format!("#{}", id);
    let Some(label) = label.map(str::trim).filter(|label| !label.is_empty()) else {
        return prefix;
    };

    if label == prefix || label.starts_with(&format!("{} ", prefix)) {
        label.to_string()
    } else {
        format!("{} {}", prefix, label)
    }
}

fn extract_credential_error_hint(message: &str) -> Option<CredentialErrorHint> {
    let marker = "凭据 #";
    let marker_start = message.rfind(marker)?;
    let digits_start = marker_start + marker.len();
    let digits_len = message[digits_start..]
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .map(char::len_utf8)
        .sum::<usize>();
    if digits_len == 0 {
        return None;
    }

    let digits_end = digits_start + digits_len;
    let id = message[digits_start..digits_end].parse::<u64>().ok()?;
    let label = message[digits_end..]
        .trim_start()
        .trim_start_matches(['#', ' '])
        .split(['）', ')', '，', ',', '：', ':'])
        .next()
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(ToString::to_string);

    Some(CredentialErrorHint { id, label })
}

fn log_provider_call_failure(message: &str) {
    if let Some(hint) = extract_credential_error_hint(message) {
        tracing::warn!(
            credential_id = hint.id,
            credential_label = %hint.display_label(),
            error = %message,
            "模型请求失败"
        );
    } else {
        tracing::warn!(error = %message, "模型请求失败");
    }
}

fn log_provider_warning_with_hint(message: &str, reason: &'static str) {
    if let Some(hint) = extract_credential_error_hint(message) {
        tracing::warn!(
            credential_id = hint.id,
            credential_label = %hint.display_label(),
            error = %message,
            "{}", reason
        );
    } else {
        tracing::warn!(error = %message, "{}", reason);
    }
}

fn log_provider_rate_limit_with_hint(message: &str, retry_after_secs: u64) {
    if let Some(hint) = extract_credential_error_hint(message) {
        tracing::warn!(
            credential_id = hint.id,
            credential_label = %hint.display_label(),
            error = %message,
            retry_after_secs,
            "模型请求或本地凭据调度临时不可用，返回 429"
        );
    } else {
        tracing::warn!(
            error = %message,
            retry_after_secs,
            "模型请求或本地凭据调度临时不可用，返回 429"
        );
    }
}

fn log_provider_error_with_hint(message: &str, reason: &'static str) {
    if let Some(hint) = extract_credential_error_hint(message) {
        tracing::error!(
            credential_id = hint.id,
            credential_label = %hint.display_label(),
            error = %message,
            "{}", reason
        );
    } else {
        tracing::error!(error = %message, "{}", reason);
    }
}

impl CredentialUsageContext {
    fn scope(&self) -> Option<PromptCacheScope> {
        Some(PromptCacheScope {
            credential_id: self.credential_id?,
            conversation_id: self.request.prompt_cache_scope_conversation_id.clone()?,
            model: self
                .request
                .upstream_model
                .clone()
                .unwrap_or_else(|| self.request.model.clone()),
        })
    }

    fn usage_source(
        &self,
        usage: &super::cache::CacheUsage,
        metadata_usage: Option<&crate::kiro::model::events::MetadataTokenUsage>,
        context_estimated: bool,
    ) -> UsageSource {
        if self.uses_local_prompt_cache_fallback(metadata_usage, usage) {
            UsageSource::LocalPromptCache
        } else if metadata_usage.is_some() {
            UsageSource::UpstreamMetadata
        } else if self.request.simulated_source.is_some() && super::cache::usage_has_cache(usage) {
            self.request.simulated_source.unwrap()
        } else if context_estimated {
            UsageSource::ContextEstimate
        } else {
            UsageSource::RequestEstimate
        }
    }

    fn final_reported_usage_for_success(
        &self,
        usage: super::cache::CacheUsage,
        usage_source: UsageSource,
    ) -> super::cache::CacheUsage {
        let reported_usage = self
            .request
            .reported_usage_for_downstream(usage, usage_source);
        self.apply_creation_frequency_control(reported_usage, usage_source)
    }

    fn canonical_reported_usage_for_success(
        &self,
        usage: super::cache::CacheUsage,
        usage_source: UsageSource,
    ) -> super::cache::CacheUsage {
        let reported_usage = self.final_reported_usage_for_success(usage, usage_source);
        self.request
            .ensure_reported_usage_for_record(reported_usage, usage_source)
    }

    fn apply_creation_frequency_control(
        &self,
        reported_usage: super::cache::CacheUsage,
        usage_source: UsageSource,
    ) -> super::cache::CacheUsage {
        if usage_source != UsageSource::LocalPromptCache
            || self.request.simulation_mode != PromptCacheSimulationMode::HighCache
        {
            return reported_usage;
        }

        let scope = self.scope();
        self.request.prompt_cache_creation_controller.apply_success(
            scope.as_ref(),
            self.request.prompt_cache_creation_control,
            reported_usage,
        )
    }

    fn preview_creation_frequency_control(
        &self,
        reported_usage: super::cache::CacheUsage,
        usage_source: UsageSource,
    ) -> super::cache::CacheUsage {
        if usage_source != UsageSource::LocalPromptCache
            || self.request.simulation_mode != PromptCacheSimulationMode::HighCache
        {
            return reported_usage;
        }

        let scope = self.scope();
        self.request
            .prompt_cache_creation_controller
            .preview_success(
                scope.as_ref(),
                self.request.prompt_cache_creation_control,
                reported_usage,
            )
    }

    fn uses_local_prompt_cache_fallback(
        &self,
        metadata_usage: Option<&crate::kiro::model::events::MetadataTokenUsage>,
        usage: &super::cache::CacheUsage,
    ) -> bool {
        self.request.simulation_mode == PromptCacheSimulationMode::HighCache
            && metadata_usage.is_some_and(super::cache::metadata_cache_is_empty)
            && self.request.simulated_source == Some(UsageSource::LocalPromptCache)
            && super::cache::usage_has_cache(usage)
    }

    fn record_success_from_stream(&self, ctx: &StreamContext) {
        let Some(usage) = ctx.final_usage() else {
            return;
        };
        let metadata_usage = ctx.metadata_usage();
        let context_estimated = metadata_usage.is_none() && ctx.context_input_tokens_seen();
        let usage_source = self.usage_source(&usage, metadata_usage, context_estimated);
        let reported_usage = ctx
            .final_reported_usage()
            .unwrap_or_else(|| self.canonical_reported_usage_for_success(usage, usage_source));
        let raw_usage = raw_usage_from_metadata_or_estimate(
            metadata_usage,
            ctx.context_input_tokens
                .unwrap_or(self.request.input_tokens),
            usage.output_tokens,
        );
        self.record_success_reported(reported_usage, usage_source, Some(raw_usage));
    }

    fn final_reported_usage_for_stream(
        &self,
        final_usage: super::cache::CacheUsage,
        metadata_usage: Option<&crate::kiro::model::events::MetadataTokenUsage>,
        context_estimated: bool,
    ) -> super::cache::CacheUsage {
        let usage_source = self.usage_source(&final_usage, metadata_usage, context_estimated);
        self.canonical_reported_usage_for_success(final_usage, usage_source)
    }

    fn record_stream_failure_from_context(
        &self,
        status: UsageRecordStatus,
        usage: Option<super::cache::CacheUsage>,
        error_detail: Option<(String, String)>,
        metadata_usage: Option<&crate::kiro::model::events::MetadataTokenUsage>,
        context_input_tokens: Option<i32>,
    ) {
        let usage = usage.unwrap_or(super::cache::CacheUsage {
            total_input_tokens: self.request.input_tokens,
            input_tokens: self.request.input_tokens,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        });
        let context_estimated = metadata_usage.is_none() && context_input_tokens.is_some();
        let source = self.usage_source(&usage, metadata_usage, context_estimated);
        let raw_usage = raw_usage_from_metadata_or_estimate(
            metadata_usage,
            context_input_tokens.unwrap_or(self.request.input_tokens),
            usage.output_tokens,
        );
        let (error_type, error_message) = error_detail.unwrap_or_else(|| {
            (
                "api_error".to_string(),
                "upstream stream did not complete successfully".to_string(),
            )
        });
        let error_detail = format!("{}: {}", error_type, error_message);
        self.record(
            status,
            usage,
            source,
            Some(raw_usage),
            Some(error_type),
            Some(error_message),
            Some(error_detail),
        );
    }

    #[cfg(test)]
    fn record_success(
        &self,
        usage: super::cache::CacheUsage,
        usage_source: UsageSource,
        _context_estimated: bool,
    ) {
        let raw_usage = usage;
        let usage = self
            .request
            .ensure_reported_usage_for_record(usage, usage_source);
        self.record_success_reported(usage, usage_source, Some(raw_usage));
    }

    fn record_success_reported(
        &self,
        usage: super::cache::CacheUsage,
        usage_source: UsageSource,
        raw_usage: Option<super::cache::CacheUsage>,
    ) {
        self.record(
            UsageRecordStatus::Success,
            usage,
            usage_source,
            raw_usage,
            None,
            None,
            None,
        );

        if usage_source != UsageSource::LocalPromptCache {
            return;
        }

        if let Some(scope) = self.scope() {
            self.request.prompt_cache.update(
                Some(scope),
                self.request.prompt_cache_profile.as_ref(),
                self.request.prompt_cache_target_read_ratio,
            );
        }
    }

    fn record_failure(
        &self,
        status: UsageRecordStatus,
        error_type: impl Into<String>,
        error_message: impl Into<String>,
    ) {
        let error_type = error_type.into();
        let error_message = error_message.into();
        let error_detail = format!("{}: {}", error_type, error_message);
        let usage = super::cache::CacheUsage {
            total_input_tokens: self.request.input_tokens,
            input_tokens: self.request.input_tokens,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        };
        self.record(
            status,
            usage,
            UsageSource::None,
            Some(usage),
            Some(error_type),
            Some(error_message),
            Some(error_detail),
        );
    }

    fn record_client_dropped(&self) {
        self.record_failure(
            UsageRecordStatus::ClientDropped,
            "client_dropped",
            "downstream client dropped before upstream stream completed",
        );
    }

    fn record(
        &self,
        status: UsageRecordStatus,
        usage: super::cache::CacheUsage,
        usage_source: UsageSource,
        raw_usage: Option<super::cache::CacheUsage>,
        error_type: Option<String>,
        error_message: Option<String>,
        error_detail: Option<String>,
    ) {
        let pricing = self.request.pricing_catalog.estimate(
            self.request
                .upstream_model
                .as_deref()
                .unwrap_or(&self.request.model),
            usage,
        );
        let include_payload_diagnostics =
            should_persist_payload_diagnostics(status, self.request.payload_guard_report.as_ref());
        let payload_breakdown = if include_payload_diagnostics {
            self.request
                .payload_breakdown
                .and_then(|breakdown| serde_json::to_value(breakdown).ok())
        } else {
            None
        };
        let payload_guard_report = if include_payload_diagnostics {
            self.request
                .payload_guard_report
                .as_ref()
                .and_then(|report| serde_json::to_value(report).ok())
        } else {
            None
        };
        self.request.recorder.record(UsageRecord {
            id: self.request.request_id.clone(),
            created_at: Utc::now().to_rfc3339(),
            endpoint: self.request.endpoint.to_string(),
            stream: self.request.stream,
            model: self.request.model.clone(),
            upstream_model: self.request.upstream_model.clone(),
            model_resolution_source: self.request.model_resolution_source.clone(),
            model_resolution_note: self.request.model_resolution_note.clone(),
            conversation_id: self.request.conversation_id.clone(),
            credential_id: self.credential_id,
            credential_label: self.credential_label.clone(),
            status,
            usage_source,
            raw_usage: raw_usage.map(usage_snapshot),
            total_input_tokens: self.request.input_tokens,
            compat_input_tokens: usage.input_tokens,
            billable_input_tokens: usage.billable_input_tokens(),
            output_tokens: usage.output_tokens,
            cache_read_input_tokens: usage.cache_read_input_tokens,
            cache_creation_input_tokens: usage.cache_creation_input_tokens,
            cache_creation_5m_input_tokens: usage.cache_creation_5m_input_tokens,
            cache_creation_1h_input_tokens: usage.cache_creation_1h_input_tokens,
            estimated_cost_usd: pricing.cost_usd,
            pricing_available: pricing.available,
            pricing_model: Some(pricing.model),
            duration_ms: self.request.started_at.elapsed().as_millis() as u64,
            first_token_latency_ms: self.request.first_token_latency_ms(),
            simulated: usage_source.is_simulated(),
            sticky_bound: self.sticky_bound,
            fallback_from_sticky: self.fallback_from_sticky,
            credential_attempts: self.credential_attempts.clone(),
            route_kind: Some(UsageRouteKind::LocalCredential),
            route_subtype: Some(self.request.route_subtype_override.unwrap_or_else(|| {
                if status == UsageRecordStatus::Success {
                    UsageRouteSubtype::LocalSuccess
                } else {
                    UsageRouteSubtype::LocalErrorNoFallback
                }
            })),
            fallback_reason: self.request.fallback_reason.clone(),
            direct_policy_reason: None,
            local_attempted: Some(true),
            local_preflight: self.request.local_preflight.clone(),
            external_pool_id: None,
            external_pool_name: None,
            external_attempts: self.request.external_attempts.clone(),
            usage_projection_applied: None,
            external_pool_billing: None,
            error_type,
            error_message,
            error_detail,
            payload_breakdown,
            payload_guard_report,
        });
    }
}

fn should_persist_payload_diagnostics(
    status: UsageRecordStatus,
    report: Option<&PayloadGuardReport>,
) -> bool {
    if status != UsageRecordStatus::Success {
        return true;
    }
    let Some(report) = report else {
        return false;
    };
    report.was_modified()
        || report.still_oversized
        || (report.max_bytes > 0 && report.final_bytes > report.max_bytes.saturating_mul(70) / 100)
}

#[derive(Clone)]
struct StreamUsageGuard {
    usage_context: CredentialUsageContext,
    completed: Arc<AtomicBool>,
}

impl StreamUsageGuard {
    fn new(usage_context: CredentialUsageContext) -> Self {
        Self {
            usage_context,
            completed: Arc::new(AtomicBool::new(false)),
        }
    }

    fn context(&self) -> &CredentialUsageContext {
        &self.usage_context
    }

    fn complete(&self) {
        self.completed.store(true, Ordering::Release);
    }
}

impl Drop for StreamUsageGuard {
    fn drop(&mut self) {
        if self.completed.load(Ordering::Acquire) {
            return;
        }
        if self.completed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.usage_context.record_client_dropped();
    }
}

fn credential_label(provider: &crate::kiro::provider::KiroProvider, id: u64) -> Option<String> {
    provider.credential_label(id)
}

async fn materialize_remote_multimodal_sources(
    payload: &mut MessagesRequest,
    caller_user_agent: Option<&str>,
) -> Result<(), String> {
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(25))
        .redirect(reqwest::redirect::Policy::none());
    // 透传调用方原始 User-Agent；若调用方未提供则不强制设置。
    if let Some(ua) = caller_user_agent {
        if !ua.is_empty() {
            builder = builder.user_agent(ua);
        }
    }
    let client = builder
        .build()
        .map_err(|e| format!("failed to create remote source client: {}", e))?;

    for message in &mut payload.messages {
        materialize_content_sources(&client, &mut message.content).await?;
    }

    Ok(())
}

fn normalize_base64_image_media_types(payload: &mut MessagesRequest) -> usize {
    let mut fixed = 0usize;
    for message in &mut payload.messages {
        fixed += normalize_content_base64_image_media_types(&mut message.content);
    }
    if fixed > 0 {
        tracing::warn!(
            fixed,
            "base64 image media_type mismatches were corrected before upstream routing"
        );
    }
    fixed
}

fn normalize_content_base64_image_media_types(content: &mut Value) -> usize {
    let Value::Array(items) = content else {
        return 0;
    };

    let mut fixed = 0usize;
    for item in items {
        let Some(obj) = item.as_object_mut() else {
            continue;
        };
        if obj.get("type").and_then(Value::as_str) != Some("image") {
            continue;
        }
        let Some(source) = obj.get_mut("source").and_then(Value::as_object_mut) else {
            continue;
        };
        if source.get("type").and_then(Value::as_str) != Some("base64") {
            continue;
        }
        let Some(data) = source.get("data").and_then(Value::as_str) else {
            continue;
        };
        let Ok(bytes) = BASE64_STANDARD.decode(data) else {
            continue;
        };
        let Some(detected_media_type) = infer_image_media_type_from_bytes(&bytes) else {
            continue;
        };
        let declared_media_type = source
            .get("media_type")
            .and_then(Value::as_str)
            .map(normalize_media_type);
        if declared_media_type.as_deref() == Some(detected_media_type) {
            continue;
        }
        source.insert(
            "media_type".to_string(),
            Value::String(detected_media_type.to_string()),
        );
        fixed += 1;
    }
    fixed
}

async fn materialize_content_sources(
    client: &reqwest::Client,
    content: &mut Value,
) -> Result<(), String> {
    let Value::Array(items) = content else {
        return Ok(());
    };

    for item in items {
        let Some((block_type, url, provided_media_type)) = remote_source_info(item) else {
            continue;
        };
        if url.starts_with("data:") {
            continue;
        }

        let (media_type, data) =
            download_remote_multimodal_source(client, &block_type, &url, provided_media_type)
                .await?;
        replace_source_with_base64(item, media_type, data);
    }

    Ok(())
}

fn remote_source_info(item: &Value) -> Option<(String, String, Option<String>)> {
    let obj = item.as_object()?;
    let block_type = obj.get("type")?.as_str()?;
    if block_type != "image" && block_type != "document" {
        return None;
    }
    let source = obj.get("source")?.as_object()?;
    if source.get("type")?.as_str()? != "url" {
        return None;
    }
    let url = source.get("url")?.as_str()?.to_string();
    let media_type = source
        .get("media_type")
        .and_then(|v| v.as_str())
        .map(normalize_media_type);
    Some((block_type.to_string(), url, media_type))
}

async fn download_remote_multimodal_source(
    client: &reqwest::Client,
    block_type: &str,
    url: &str,
    provided_media_type: Option<String>,
) -> Result<(String, String), String> {
    let mut current_url = url.to_string();
    let mut response = None;

    for redirect_count in 0..=5 {
        if !current_url.starts_with("https://") && !current_url.starts_with("http://") {
            return Err(format!(
                "{} URL source must use http or https: {}",
                block_type, current_url
            ));
        }

        ensure_safe_remote_url_resolves(&current_url)
            .await
            .map_err(|reason| format!("{} URL rejected: {}", block_type, reason))?;

        let candidate = client
            .get(&current_url)
            .send()
            .await
            .map_err(|e| format!("failed to download {} URL source: {}", block_type, e))?;

        if candidate.status().is_redirection() {
            if redirect_count >= 5 {
                return Err(format!("{} URL source has too many redirects", block_type));
            }

            let location = candidate
                .headers()
                .get(REQWEST_LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| {
                    format!(
                        "{} URL source redirect is missing Location header",
                        block_type
                    )
                })?;
            let next_url = candidate
                .url()
                .join(location)
                .map_err(|e| format!("invalid {} URL redirect: {}", block_type, e))?;
            current_url = next_url.to_string();
            continue;
        }

        response = Some(candidate);
        break;
    }

    let response =
        response.ok_or_else(|| format!("failed to download {} URL source", block_type))?;
    let final_url = response.url().to_string();
    if !final_url.starts_with("https://") && !final_url.starts_with("http://") {
        return Err(format!(
            "{} URL source must use http or https: {}",
            block_type, final_url
        ));
    }
    ensure_safe_remote_url_resolves(&final_url)
        .await
        .map_err(|reason| format!("{} URL rejected: {}", block_type, reason))?;

    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "failed to download {} URL source: HTTP {}",
            block_type, status
        ));
    }

    if response
        .content_length()
        .is_some_and(|len| len > MAX_REMOTE_MULTIMODAL_BYTES as u64)
    {
        return Err(format!(
            "{} URL source exceeds {} bytes",
            block_type, MAX_REMOTE_MULTIMODAL_BYTES
        ));
    }

    let response_media_type = response
        .headers()
        .get(REQWEST_CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(normalize_media_type);
    let bytes = read_limited_response_body(response, block_type).await?;

    let media_type = infer_remote_media_type(
        block_type,
        &final_url,
        provided_media_type.as_deref(),
        response_media_type.as_deref(),
        bytes.as_slice(),
    )
    .ok_or_else(|| {
        format!(
            "unsupported {} URL media type for {}",
            block_type, final_url
        )
    })?;

    Ok((media_type, BASE64_STANDARD.encode(bytes.as_slice())))
}

async fn read_limited_response_body(
    response: reqwest::Response,
    block_type: &str,
) -> Result<Vec<u8>, String> {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();

    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|e| format!("failed to read {} URL source: {}", block_type, e))?;
        if bytes.len() + chunk.len() > MAX_REMOTE_MULTIMODAL_BYTES {
            return Err(format!(
                "{} URL source exceeds {} bytes",
                block_type, MAX_REMOTE_MULTIMODAL_BYTES
            ));
        }
        bytes.extend_from_slice(&chunk);
    }

    Ok(bytes)
}

fn replace_source_with_base64(item: &mut Value, media_type: String, data: String) {
    let Some(obj) = item.as_object_mut() else {
        return;
    };
    obj.insert(
        "source".to_string(),
        json!({
            "type": "base64",
            "media_type": media_type,
            "data": data
        }),
    );
}

fn infer_remote_media_type(
    block_type: &str,
    url: &str,
    provided: Option<&str>,
    response: Option<&str>,
    bytes: &[u8],
) -> Option<String> {
    for candidate in [provided, response].into_iter().flatten() {
        if is_supported_remote_media_type(block_type, candidate) {
            return Some(candidate.to_string());
        }
    }

    if block_type == "image" {
        if let Some(media_type) = infer_image_media_type_from_bytes(bytes) {
            return Some(media_type.to_string());
        }
        return infer_image_format_from_url(url)
            .and_then(|format| image_media_type_from_format(&format).map(str::to_string));
    }

    if bytes.starts_with(b"%PDF") {
        return Some("application/pdf".to_string());
    }
    let inferred = infer_document_media_type_from_url(url);
    is_supported_remote_media_type(block_type, &inferred).then_some(inferred)
}

fn is_supported_remote_media_type(block_type: &str, media_type: &str) -> bool {
    match block_type {
        "image" => matches!(
            media_type,
            "image/jpeg" | "image/png" | "image/gif" | "image/webp"
        ),
        "document" => matches!(
            media_type,
            "application/pdf"
                | "text/plain"
                | "text/markdown"
                | "text/html"
                | "text/csv"
                | "application/json"
        ),
        _ => false,
    }
}

fn normalize_media_type(raw: &str) -> String {
    raw.split(';')
        .next()
        .unwrap_or(raw)
        .trim()
        .to_ascii_lowercase()
}

fn infer_image_media_type_from_bytes(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']) {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

fn image_media_type_from_format(format: &str) -> Option<&'static str> {
    match format {
        "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

/// 拒绝指向私有/回环/链路本地/云元数据等敏感网络的 URL，避免 SSRF。
fn ensure_safe_remote_url(url_str: &str) -> Result<(), String> {
    let parsed = ::url::Url::parse(url_str).map_err(|e| format!("invalid URL: {}", e))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| "URL missing host".to_string())?;

    let lower = host.to_ascii_lowercase();
    const BLOCKED_HOSTS: &[&str] = &[
        "localhost",
        "ip6-localhost",
        "ip6-loopback",
        "metadata.google.internal",
        "metadata",
        "instance-data",
    ];
    if BLOCKED_HOSTS.contains(&lower.as_str()) || lower.ends_with(".localhost") {
        return Err(format!("host {} is blocked", host));
    }

    let parsed_host_ip = match parsed.host() {
        Some(::url::Host::Ipv4(ip)) => Some(std::net::IpAddr::V4(ip)),
        Some(::url::Host::Ipv6(ip)) => Some(std::net::IpAddr::V6(ip)),
        _ => host.parse::<std::net::IpAddr>().ok(),
    };
    if let Some(addr) = parsed_host_ip {
        if is_blocked_ip(&addr) {
            return Err(format!("IP {} is in a blocked range", addr));
        }
    }

    Ok(())
}

async fn ensure_safe_remote_url_resolves(url_str: &str) -> Result<(), String> {
    ensure_safe_remote_url(url_str)?;

    let parsed = ::url::Url::parse(url_str).map_err(|e| format!("invalid URL: {}", e))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| "URL missing host".to_string())?;
    if host.parse::<std::net::IpAddr>().is_ok() {
        return Ok(());
    }

    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| "URL has no resolvable port".to_string())?;
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| format!("DNS lookup failed for {}: {}", host, e))?;

    let mut resolved_any = false;
    for addr in addrs {
        resolved_any = true;
        let ip = addr.ip();
        if is_blocked_ip(&ip) {
            return Err(format!("resolved IP {} is in a blocked range", ip));
        }
    }

    if !resolved_any {
        return Err(format!("DNS lookup returned no records for {}", host));
    }

    Ok(())
}

fn is_blocked_ip(addr: &std::net::IpAddr) -> bool {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    match addr {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_documentation()
                // CGNAT 100.64.0.0/10
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64)
                // AWS/GCP/Azure metadata 169.254.169.254 已被 link_local 覆盖
                || *v4 == Ipv4Addr::new(0, 0, 0, 0)
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // ULA fc00::/7
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                // Link-local fe80::/10
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                // IPv4-mapped: 解出来再判
                || v6
                    .to_ipv4_mapped()
                    .map(|m| is_blocked_ip(&IpAddr::V4(m)))
                    .unwrap_or(false)
                || *v6 == Ipv6Addr::UNSPECIFIED
        }
    }
}

fn prepare_usage_context(
    state: &AppState,
    runtime_config: RequestRuntimeConfig,
    endpoint: &'static str,
    stream: bool,
    payload: &MessagesRequest,
    model_resolution: Option<ModelResolution>,
    conversation_id: Option<String>,
    stable_conversation_id: Option<String>,
    input_tokens: i32,
) -> RequestUsageContext {
    let prompt_cache_model = model_resolution
        .as_ref()
        .and_then(|resolution| resolution.upstream_model.as_deref())
        .unwrap_or(&payload.model);
    let prompt_cache_supported = state
        .model_capabilities
        .supports_prompt_caching_for(prompt_cache_model)
        .unwrap_or(true);
    let prompt_cache_profile = match state.prompt_cache_simulation_mode {
        PromptCacheSimulationMode::Disabled => None,
        PromptCacheSimulationMode::HighCache if prompt_cache_supported => state
            .prompt_cache
            .build_high_cache_profile_for_model(payload, input_tokens, prompt_cache_model),
        PromptCacheSimulationMode::HighCache => None,
    };
    let (simulated_usage, simulated_source) = build_simulated_usage(
        state,
        stable_conversation_id.as_deref(),
        prompt_cache_profile.as_ref(),
    );
    let request_id = envelope::request_id();
    let reported_cache_creation_seed = prompt_cache_profile
        .as_ref()
        .map(|profile| profile.cache_jitter_seed())
        .unwrap_or(0)
        ^ fastrand::u64(..);
    let reported_cache_usage_policy = reported_cache_usage_policy(
        endpoint,
        state.prompt_cache_simulation_mode,
        &runtime_config.reported_usage,
        reported_cache_creation_seed,
    );

    RequestUsageContext {
        recorder: state.usage_recorder.clone(),
        prompt_cache: state.prompt_cache.clone(),
        prompt_cache_creation_controller: state.prompt_cache_creation_controller.clone(),
        pricing_catalog: state.pricing_catalog.clone(),
        request_id,
        endpoint,
        stream,
        model: payload.model.clone(),
        upstream_model: model_resolution
            .as_ref()
            .and_then(|resolution| resolution.upstream_model.clone()),
        model_resolution_source: model_resolution
            .as_ref()
            .map(|resolution| resolution.source.as_str().to_string()),
        model_resolution_note: model_resolution
            .as_ref()
            .and_then(|resolution| resolution.note.clone()),
        conversation_id,
        prompt_cache_scope_conversation_id: stable_conversation_id,
        input_tokens,
        context_window_tokens: model_resolution
            .as_ref()
            .and_then(|resolution| resolution.upstream_model.as_deref())
            .and_then(|model| state.model_capabilities.max_input_tokens_for(model))
            .unwrap_or_else(|| {
                let model = model_resolution
                    .as_ref()
                    .and_then(|resolution| resolution.upstream_model.as_deref())
                    .unwrap_or(&payload.model);
                get_context_window_size(model)
            }),
        prompt_cache_profile,
        simulation_mode: state.prompt_cache_simulation_mode,
        prompt_cache_target_read_ratio: runtime_config.prompt_cache_target_read_ratio,
        prompt_cache_token_scale: runtime_config.prompt_cache_token_scale,
        prompt_cache_max_simulated_input_tokens: runtime_config
            .prompt_cache_max_simulated_input_tokens,
        prompt_cache_cap_jitter_min_tokens: runtime_config.prompt_cache_cap_jitter_min_tokens,
        prompt_cache_cap_jitter_max_tokens: runtime_config.prompt_cache_cap_jitter_max_tokens,
        prompt_cache_scale_min_input_tokens: runtime_config.prompt_cache_scale_min_input_tokens,
        prompt_cache_creation_control: runtime_config.prompt_cache_creation_control,
        reported_cache_usage_policy,
        simulated_usage,
        simulated_source,
        payload_breakdown: None,
        payload_guard_report: None,
        route_subtype_override: None,
        fallback_reason: None,
        local_preflight: None,
        external_attempts: Vec::new(),
        started_at: Instant::now(),
        first_token_latency_ms: Arc::new(AtomicU64::new(0)),
    }
}

fn prompt_cache_scope_conversation_id(
    mode: PromptCacheSimulationMode,
    payload: &MessagesRequest,
) -> Option<String> {
    match mode {
        PromptCacheSimulationMode::Disabled => None,
        PromptCacheSimulationMode::HighCache => extract_stable_conversation_id(payload),
    }
}

fn build_simulated_usage(
    state: &AppState,
    conversation_id: Option<&str>,
    prompt_cache_profile: Option<&PromptCacheProfile>,
) -> (Option<super::cache::CacheSimulation>, Option<UsageSource>) {
    match state.prompt_cache_simulation_mode {
        PromptCacheSimulationMode::Disabled => (None, None),
        PromptCacheSimulationMode::HighCache => {
            if conversation_id.is_none() {
                return (None, None);
            }

            // credential_id 需要等 provider 选中账号后才能确定；这里先保留 profile，
            // 真正的 local prompt-cache 计算在 attach credential 后重新完成。
            if prompt_cache_profile.is_some() {
                (None, Some(UsageSource::LocalPromptCache))
            } else {
                (None, None)
            }
        }
    }
}

fn prepare_credential_usage_context(
    usage_context: RequestUsageContext,
    provider: &crate::kiro::provider::KiroProvider,
    credential_id: u64,
    sticky_bound: bool,
    fallback_from_sticky: bool,
    credential_attempts: Vec<KiroCredentialAttempt>,
) -> CredentialUsageContext {
    let mut usage_context = usage_context;
    if matches!(
        usage_context.simulation_mode,
        PromptCacheSimulationMode::HighCache
    ) {
        let scope = usage_context
            .prompt_cache_scope_conversation_id
            .as_ref()
            .map(|conversation_id| PromptCacheScope {
                credential_id,
                conversation_id: conversation_id.clone(),
                model: usage_context
                    .upstream_model
                    .clone()
                    .unwrap_or_else(|| usage_context.model.clone()),
            });
        let prompt_usage = usage_context.prompt_cache.compute(
            scope,
            usage_context.prompt_cache_profile.as_ref(),
            usage_context.prompt_cache_target_read_ratio,
        );
        usage_context.simulated_usage =
            super::cache::CacheSimulation::from_prompt_cache_with_ratio_and_amplification(
                prompt_usage,
                usage_context.prompt_cache_target_read_ratio,
                usage_context.cache_amplification(),
            );
        if usage_context.simulated_usage.is_some() {
            usage_context.simulated_source = Some(UsageSource::LocalPromptCache);
        } else {
            usage_context.simulated_source = None;
        }
    }

    usage_context.attach_credential(
        Some(credential_id),
        credential_label(provider, credential_id),
        sticky_bound,
        fallback_from_sticky,
        credential_attempts,
    )
}

/// 将 KiroProvider 错误映射为 HTTP 响应
fn cooldown_retry_after_secs(
    provider: Option<&crate::kiro::provider::KiroProvider>,
    fallback_secs: u64,
) -> u64 {
    let fallback_secs = fallback_secs.max(1);
    let Some(provider) = provider else {
        return fallback_secs;
    };
    let snapshot = provider.manager_snapshot();
    let retry_after = snapshot
        .entries
        .iter()
        .filter(|entry| !entry.disabled)
        .filter_map(|entry| {
            (entry.cooldown_remaining_secs > 0).then_some(entry.cooldown_remaining_secs)
        })
        .min()
        .unwrap_or(fallback_secs);
    retry_after.max(1)
}

fn map_provider_error(
    err: Error,
    request_id: Option<&str>,
    provider: Option<&crate::kiro::provider::KiroProvider>,
) -> Response {
    let err_str = err.to_string();

    // Provider content length thresholds and model context windows are different limits.
    if is_upstream_payload_too_long_error(&err_str) {
        let message = "Request input content length exceeded the request threshold. This limit is separate from the model context window. Reduce oversized tools, system prompt, documents, images, tool results, or conversation history.";
        log_provider_warning_with_hint(&err_str, "请求被拒绝：输入内容长度超过接口阈值");
        return if let Some(request_id) = request_id {
            envelope::error_response_with_id(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                message,
                request_id,
            )
        } else {
            envelope::error_response(StatusCode::BAD_REQUEST, "invalid_request_error", message)
        };
    }

    if is_upstream_context_window_full_error(&err_str) {
        let message = "Context window is full. Reduce conversation history, system prompt, tools, documents, images, or tool results.";
        log_provider_warning_with_hint(&err_str, "请求被拒绝：上下文窗口已满");
        return if let Some(request_id) = request_id {
            envelope::error_response_with_id(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                message,
                request_id,
            )
        } else {
            envelope::error_response(StatusCode::BAD_REQUEST, "invalid_request_error", message)
        };
    }

    if is_upstream_improperly_formed_error(&err_str) {
        log_provider_warning_with_hint(
            &err_str,
            "请求被拒绝：Kiro payload 形态不合法（不应切换账号重试）",
        );
        return if let Some(request_id) = request_id {
            envelope::error_response_with_id(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                UPSTREAM_INVALID_REQUEST_MESSAGE,
                request_id,
            )
        } else {
            envelope::error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                UPSTREAM_INVALID_REQUEST_MESSAGE,
            )
        };
    }

    if is_upstream_invalid_model_error(&err_str) {
        log_provider_warning_with_hint(&err_str, "请求被拒绝：上游模型不支持（不应切换账号重试）");
        let message =
            "Model is not available from the current upstream. Select a supported model and retry.";
        return if let Some(request_id) = request_id {
            envelope::error_response_with_id(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                message,
                request_id,
            )
        } else {
            envelope::error_response(StatusCode::BAD_REQUEST, "invalid_request_error", message)
        };
    }

    if is_upstream_bad_request_error(&err_str) {
        log_provider_warning_with_hint(&err_str, "请求被上游以 400 拒绝（不应切换账号重试）");
        return if let Some(request_id) = request_id {
            envelope::error_response_with_id(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                UPSTREAM_INVALID_REQUEST_MESSAGE,
                request_id,
            )
        } else {
            envelope::error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                UPSTREAM_INVALID_REQUEST_MESSAGE,
            )
        };
    }

    if err_str.contains("临时冷却")
        || err_str.contains("本地限流")
        || err_str.contains("凭据调度排队等待超时")
        || err_str.contains("暂不可调度")
        || err_str.contains("retry-after")
        || err_str.contains("Retry-After")
        || err_str.contains("429")
    {
        let retry_after_secs = retry_after_secs_from_error(&err_str)
            .map(|secs| secs.max(1))
            .unwrap_or_else(|| cooldown_retry_after_secs(provider, 1));
        log_provider_rate_limit_with_hint(&err_str, retry_after_secs);
        let message = format!(
            "Too many requests. Retry after {} seconds.",
            retry_after_secs
        );
        return if let Some(request_id) = request_id {
            envelope::error_response_with_id_and_headers(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limit_error",
                message,
                request_id,
                [("retry-after", retry_after_secs.to_string())],
            )
        } else {
            envelope::error_response(StatusCode::TOO_MANY_REQUESTS, "rate_limit_error", message)
        };
    }

    if err_str.contains("所有凭据均已禁用")
        || err_str.contains("所有凭据已用尽")
        || err_str.contains("没有支持当前模型的可用凭据")
    {
        log_provider_error_with_hint(&err_str, "没有可调度凭据");
        let message = format!("No available credentials: {}", err);
        return if let Some(request_id) = request_id {
            envelope::error_response_with_id(
                StatusCode::SERVICE_UNAVAILABLE,
                "service_unavailable",
                message,
                request_id,
            )
        } else {
            envelope::error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "service_unavailable",
                message,
            )
        };
    }

    log_provider_error_with_hint(&err_str, "Kiro API 调用失败");
    if let Some(request_id) = request_id {
        envelope::error_response_with_id(StatusCode::BAD_GATEWAY, "api_error", err_str, request_id)
    } else {
        envelope::error_response(StatusCode::BAD_GATEWAY, "api_error", err_str)
    }
}

fn is_upstream_payload_too_long_error(value: &str) -> bool {
    if value.contains("CONTENT_LENGTH_EXCEEDS_THRESHOLD") {
        return true;
    }

    let lower = value.to_ascii_lowercase();
    lower.contains("input is too long")
        || lower.contains("payload is too large")
        || lower.contains("request payload is too large")
        || lower.contains("request body is too large")
        || lower.contains("content length exceeded")
        || lower.contains("content length exceeds")
        || lower.contains("input content length exceeded")
}

fn is_upstream_context_window_full_error(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("context window is full") && lower.contains("reduce conversation history")
}

fn is_upstream_improperly_formed_error(value: &str) -> bool {
    value.contains("IMPROPERLY_FORMED")
        || value
            .to_ascii_lowercase()
            .contains("improperly formed request")
}

fn is_upstream_bad_request_error(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("400 bad request")
        || lower.contains("bad_request")
        || lower.contains("assistant-prefill")
        || lower.contains("assistant prefill")
        || lower.contains("last message must be user")
        || lower.contains("请求无效")
        || lower.contains("请求参数错误")
}

fn is_upstream_invalid_model_error(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("invalid model")
        || lower.contains("invalid_model_id")
        || lower.contains("model not found")
        || lower.contains("model_not_found")
        || lower.contains("unsupported model")
}

fn is_upstream_too_long_error(value: &str) -> bool {
    is_upstream_payload_too_long_error(value) || is_upstream_context_window_full_error(value)
}

fn should_retry_payload_guard_after_error(
    value: &str,
    attempted_body_bytes: usize,
    retry_max_bytes: usize,
) -> bool {
    is_upstream_too_long_error(value)
        || (retry_max_bytes > 0
            && attempted_body_bytes > retry_max_bytes
            && is_upstream_improperly_formed_error(value))
}

fn merge_credential_attempts(
    mut prefix: Vec<KiroCredentialAttempt>,
    attempts: Vec<KiroCredentialAttempt>,
) -> Vec<KiroCredentialAttempt> {
    if prefix.is_empty() {
        return attempts;
    }
    prefix.extend(attempts);
    prefix
}

fn retry_after_secs_from_error(value: &str) -> Option<u64> {
    let lower = value.to_lowercase();
    for marker in ["retry_after_secs=", "retry-after=", "retry after "] {
        let Some(index) = lower.find(marker) else {
            continue;
        };
        let tail = &lower[index + marker.len()..];
        let digits: String = tail
            .chars()
            .skip_while(|ch| !ch.is_ascii_digit())
            .take_while(|ch| ch.is_ascii_digit())
            .collect();
        if let Ok(seconds) = digits.parse::<u64>() {
            return Some(seconds);
        }
    }
    None
}

fn conversion_error_response(e: &ConversionError) -> Response {
    let (error_type, message) = match e {
        ConversionError::UnsupportedModel(model) => {
            ("invalid_request_error", format!("模型不支持: {}", model))
        }
        ConversionError::EmptyMessages => ("invalid_request_error", "消息列表为空".to_string()),
        ConversionError::UnsupportedContent(message) => ("invalid_request_error", message.clone()),
    };
    envelope::error_response(StatusCode::BAD_REQUEST, error_type, message)
}

fn resolve_request_model(
    state: &AppState,
    runtime_config: &RequestRuntimeConfig,
    endpoint: &'static str,
    payload: &MessagesRequest,
) -> Result<ModelResolution, Response> {
    let resolution = state.model_capabilities.resolve_model_with_mapping(
        &payload.model,
        runtime_config.model_resolution_mode,
        &runtime_config.model_mapping,
    );
    if resolution.source == ModelResolutionSource::Unsupported {
        tracing::warn!(
            endpoint,
            requested_model = %payload.model,
            model_resolution_mode = %runtime_config.model_resolution_mode.as_str(),
            resolution = %resolution.source.as_str(),
            "请求模型解析失败"
        );
        return Err(envelope::error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            format!(
                "模型不支持: {}。请同步 Kiro 模型能力或配置为当前上游支持的模型。",
                payload.model
            ),
        ));
    }

    if let Some(upstream_model) = resolution.upstream_model.as_deref() {
        tracing::debug!(
            endpoint,
            requested_model = %resolution.requested_model,
            upstream_model = %upstream_model,
            model_resolution_mode = %runtime_config.model_resolution_mode.as_str(),
            resolution = %resolution.source.as_str(),
            remapped = resolution.is_remapped(),
            note = ?resolution.note,
            "请求模型解析完成"
        );
    }

    Ok(resolution)
}

fn should_expose_proxy_warnings(runtime_config: &RequestRuntimeConfig) -> bool {
    runtime_config.expose_proxy_warnings && !runtime_config.compat_profile.is_strict()
}

fn merge_warning_headers(
    conversion_warnings: Option<String>,
    payload_report: Option<&PayloadGuardReport>,
) -> Option<String> {
    let mut warnings = Vec::new();
    if let Some(value) = conversion_warnings.filter(|value| !value.trim().is_empty()) {
        warnings.push(value);
    }
    if let Some(fragment) = payload_report.and_then(PayloadGuardReport::warning_header_fragment) {
        if !fragment.trim().is_empty() {
            warnings.push(fragment);
        }
    }
    (!warnings.is_empty()).then(|| warnings.join(","))
}

fn payload_guard_error_response(err: PayloadGuardError) -> Response {
    match err {
        PayloadGuardError::Serialize(message) => {
            tracing::error!("序列化请求失败: {}", message);
            envelope::error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", message)
        }
    }
}

fn log_payload_guard_report(
    report: &PayloadGuardReport,
    endpoint: &str,
    requested_model: &str,
    upstream_model: Option<&str>,
    conversation_id: Option<&str>,
) {
    if !report.enabled {
        return;
    }
    if report.was_modified() || report.still_oversized {
        tracing::warn!(
            endpoint,
            requested_model,
            upstream_model,
            conversation_id,
            original_bytes = report.original_bytes,
            final_bytes = report.final_bytes,
            max_bytes = report.max_bytes,
            original_history_entries = report.original_history_entries,
            final_history_entries = report.final_history_entries,
            trimmed_history_entries = report.trimmed_history_entries,
            aligned_leading_entries = report.aligned_leading_entries,
            removed_empty_tool_uses = report.removed_empty_tool_uses,
            removed_duplicate_tool_uses = report.removed_duplicate_tool_uses,
            renamed_duplicate_tool_uses = report.renamed_duplicate_tool_uses,
            removed_orphan_tool_results = report.removed_orphan_tool_results,
            removed_duplicate_tool_results = report.removed_duplicate_tool_results,
            textified_duplicate_tool_results = report.textified_duplicate_tool_results,
            textified_orphan_tool_results = report.textified_orphan_tool_results,
            removed_orphan_tool_uses = report.removed_orphan_tool_uses,
            truncated_history_tool_results = report.truncated_history_tool_results,
            truncated_history_tool_result_chars = report.truncated_history_tool_result_chars,
            removed_history_thinking_blocks = report.removed_history_thinking_blocks,
            removed_history_thinking_chars = report.removed_history_thinking_chars,
            trimmed_web_fetch_blocks = report.trimmed_web_fetch_blocks,
            trimmed_web_fetch_chars = report.trimmed_web_fetch_chars,
            compressed_tool_definitions = report.compressed_tool_definitions,
            compressed_tool_definition_bytes = report.compressed_tool_definition_bytes,
            truncated_current_tool_results = report.truncated_current_tool_results,
            truncated_current_tool_result_chars = report.truncated_current_tool_result_chars,
            truncated_current_documents = report.truncated_current_documents,
            truncated_current_document_chars = report.truncated_current_document_chars,
            truncated_current_user_content = report.truncated_current_user_content,
            truncated_current_user_content_chars = report.truncated_current_user_content_chars,
            dropped_current_images = report.dropped_current_images,
            dropped_current_image_bytes = report.dropped_current_image_bytes,
            still_oversized = report.still_oversized,
            "Kiro payload guard applied before upstream call"
        );
    } else if report.max_bytes > 0
        && report.original_bytes > report.max_bytes.saturating_mul(80) / 100
    {
        tracing::debug!(
            endpoint,
            requested_model,
            upstream_model,
            conversation_id,
            payload_bytes = report.final_bytes,
            max_bytes = report.max_bytes,
            history_entries = report.final_history_entries,
            "Kiro payload guard observed large request"
        );
    }
}

fn should_log_payload_byte_breakdown(report: &PayloadGuardReport) -> bool {
    report.was_modified()
        || report.still_oversized
        || (report.max_bytes > 0 && report.final_bytes > report.max_bytes.saturating_mul(70) / 100)
}

fn log_payload_byte_breakdown(
    breakdown: Option<PayloadByteBreakdown>,
    report: &PayloadGuardReport,
    endpoint: &str,
    requested_model: &str,
    upstream_model: Option<&str>,
    conversation_id: Option<&str>,
) {
    let Some(breakdown) = breakdown else {
        tracing::debug!(
            endpoint,
            requested_model,
            upstream_model,
            conversation_id,
            total_bytes = report.final_bytes,
            max_bytes = report.max_bytes,
            still_oversized = report.still_oversized,
            "Kiro payload byte breakdown skipped for small unmodified request"
        );
        return;
    };

    tracing::debug!(
        endpoint,
        requested_model,
        upstream_model,
        conversation_id,
        total_bytes = breakdown.total_bytes,
        max_bytes = report.max_bytes,
        history_bytes = breakdown.history_bytes,
        current_message_bytes = breakdown.current_message_bytes,
        current_content_bytes = breakdown.current_content_bytes,
        current_tools_bytes = breakdown.current_tools_bytes,
        current_tool_results_bytes = breakdown.current_tool_results_bytes,
        current_images_bytes = breakdown.current_images_bytes,
        history_tool_results_bytes = breakdown.history_tool_results_bytes,
        history_images_bytes = breakdown.history_images_bytes,
        history_entries = breakdown.history_entries,
        current_tool_count = breakdown.current_tool_count,
        current_tool_result_count = breakdown.current_tool_result_count,
        current_image_count = breakdown.current_image_count,
        largest_tool_bytes = breakdown.largest_tool_bytes,
        largest_history_tool_result_bytes = breakdown.largest_history_tool_result_bytes,
        largest_current_tool_result_bytes = breakdown.largest_current_tool_result_bytes,
        history_tool_use_count = breakdown.history_tool_use_count,
        history_tool_result_count = breakdown.history_tool_result_count,
        still_oversized = report.still_oversized,
        "Kiro payload byte breakdown"
    );
}

fn should_extract_unsigned_thinking(
    runtime_config: &RequestRuntimeConfig,
    thinking_enabled: bool,
) -> bool {
    runtime_config.extract_thinking
        && thinking_enabled
        && runtime_config.compat_profile.allows_unsigned_thinking()
}

fn websearch_supported_for_profile(profile: CompatProfile) -> bool {
    !profile.is_strict()
}

/// GET /v1/models
///
/// 返回可用的模型列表
pub async fn get_models(State(state): State<AppState>) -> impl IntoResponse {
    tracing::info!("Received GET /v1/models request");

    let models = state.model_capabilities.anthropic_models();

    Json(ModelsResponse {
        object: "list".to_string(),
        data: models,
    })
}

/// POST /v1/messages
///
/// 创建消息（对话）
pub async fn post_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    raw_body: Bytes,
) -> Response {
    let payload = match parse_messages_payload(&raw_body) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    post_messages_inner(state, headers, raw_body, payload, "/v1/messages").await
}

/// POST /na/v1/messages
///
/// 创建消息（对话），底层 high-cache 计算保持开启；默认只上报真实上游 cache usage。
pub async fn post_messages_real_cache_usage(
    State(state): State<AppState>,
    headers: HeaderMap,
    raw_body: Bytes,
) -> Response {
    let payload = match parse_messages_payload(&raw_body) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    post_messages_inner(state, headers, raw_body, payload, "/na/v1/messages").await
}

/// POST /ha/v1/messages
///
/// 创建消息（对话），使用 high-cache 计算；下游 usage 上报由 `/ha` 路径覆盖项独立控制。
pub async fn post_messages_ha(
    State(state): State<AppState>,
    headers: HeaderMap,
    raw_body: Bytes,
) -> Response {
    let payload = match parse_messages_payload(&raw_body) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    post_messages_inner(state, headers, raw_body, payload, "/ha/v1/messages").await
}

async fn post_messages_inner(
    state: AppState,
    headers: HeaderMap,
    raw_body: Bytes,
    mut payload: MessagesRequest,
    endpoint: &'static str,
) -> Response {
    tracing::debug!(
        endpoint = endpoint,
        model = %payload.model,
        max_tokens = %payload.max_tokens,
        stream = %payload.stream,
        message_count = %payload.messages.len(),
        "Received POST messages request"
    );
    // 检查 KiroProvider 是否可用
    let provider = match &state.kiro_provider {
        Some(p) => p.clone(),
        None => {
            tracing::error!("KiroProvider 未配置");
            return envelope::error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "service_unavailable",
                "Kiro API provider not configured",
            );
        }
    };
    let runtime_config = request_runtime_config(&state, &provider);
    let mut external_fallback = build_external_fallback_context(
        &state,
        &provider,
        &runtime_config,
        endpoint,
        raw_body,
        headers.clone(),
        &payload,
    );

    // 检测模型名是否包含 "thinking" 后缀，若包含则覆写 thinking 配置
    override_thinking_from_model_name(&mut payload);

    let caller_ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok());
    if let Err(message) = materialize_remote_multimodal_sources(&mut payload, caller_ua).await {
        tracing::warn!("多模态远程 source 处理失败: {}", message);
        return envelope::error_response(StatusCode::BAD_REQUEST, "invalid_request_error", message);
    }
    normalize_base64_image_media_types(&mut payload);

    if let Some(external) = external_fallback.as_mut() {
        external.refresh_payload(&payload);
    }

    let model_resolution = match resolve_request_model(&state, &runtime_config, endpoint, &payload)
    {
        Ok(resolution) => resolution,
        Err(response) => {
            if let Some(external_response) = maybe_forward_external_after_local_error(
                external_fallback.as_ref(),
                &envelope::request_id(),
                &format!("模型不支持: {}", payload.model),
                Vec::new(),
            )
            .await
            {
                return external_response;
            }
            return response;
        }
    };
    if let Some(external) = external_fallback.as_mut() {
        external.model_resolution = Some(model_resolution.clone());
    }
    if let Some(external) = external_fallback.as_ref() {
        let request_id = envelope::request_id();
        if let Some(response) = external.direct_policy_response(&request_id).await {
            return response;
        }
    }

    // 检查是否为 WebSearch 请求
    if websearch::has_web_search_tool(&payload) {
        if !websearch_supported_for_profile(runtime_config.compat_profile) {
            return envelope::error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "web_search server-tool synthesis is disabled in anthropic-strict profile",
            );
        }
        tracing::info!("检测到 WebSearch 工具，路由到 WebSearch 处理");

        // 估算输入 tokens
        let input_tokens = token::count_all_tokens(
            payload.model.clone(),
            payload.system.clone(),
            payload.messages.clone(),
            payload.tools.clone(),
        ) as i32;

        return websearch::handle_websearch_request(provider, &payload, input_tokens).await;
    }

    // 转换请求
    let conversion_result = match convert_request_with_resolved_model(
        &payload,
        ConverterOptions {
            compat_profile: runtime_config.compat_profile,
            prompt_cache_simulation_mode: state.prompt_cache_simulation_mode,
        },
        &model_resolution,
    ) {
        Ok(result) => result,
        Err(e) => {
            tracing::warn!("请求转换失败: {}", e);
            return conversion_error_response(&e);
        }
    };

    // 构建 Kiro 请求（profile_arn 由 provider 层根据实际凭据注入）
    let mut kiro_request = KiroRequest {
        conversation_state: conversion_result.conversation_state,
        profile_arn: None,
    };
    let conversation_id = kiro_request.conversation_state.conversation_id.clone();

    let too_long_retry = PayloadTooLongRetryRequest::new(
        kiro_request.clone(),
        &runtime_config,
        endpoint,
        &payload.model,
        model_resolution.upstream_model.as_deref(),
        &conversation_id,
        should_expose_proxy_warnings(&runtime_config)
            .then(|| conversion_result.warnings.encode_header())
            .flatten(),
    );
    let (request_body, payload_guard_report) = match guard_kiro_request(
        &mut kiro_request,
        runtime_config.initial_payload_guard_config(),
    ) {
        Ok(result) => result,
        Err(err) => return payload_guard_error_response(err),
    };
    log_payload_guard_report(
        &payload_guard_report,
        endpoint,
        &payload.model,
        model_resolution.upstream_model.as_deref(),
        Some(&conversation_id),
    );
    let payload_breakdown = should_log_payload_byte_breakdown(&payload_guard_report)
        .then(|| breakdown_kiro_request(&kiro_request, &request_body));
    log_payload_byte_breakdown(
        payload_breakdown,
        &payload_guard_report,
        endpoint,
        &payload.model,
        model_resolution.upstream_model.as_deref(),
        Some(&conversation_id),
    );
    if model_resolution.is_remapped() {
        tracing::info!(
            endpoint,
            requested_model = %model_resolution.requested_model,
            upstream_model = ?model_resolution.upstream_model,
            resolution = %model_resolution.source.as_str(),
            note = ?model_resolution.note,
            conversation_id = %conversation_id,
            "Kiro upstream model mapping applied to request payload"
        );
    };

    tracing::debug!(
        endpoint = endpoint,
        requested_model = %payload.model,
        upstream_model = ?model_resolution.upstream_model,
        conversation_id = %conversation_id,
        request_bytes = request_body.len(),
        history_entries = payload_guard_report.final_history_entries,
        current_tool_count = kiro_request.conversation_state.current_message.user_input_message.user_input_message_context.tools.len(),
        current_tool_result_count = kiro_request.conversation_state.current_message.user_input_message.user_input_message_context.tool_results.len(),
        current_image_count = kiro_request.conversation_state.current_message.user_input_message.images.len(),
        "Kiro request prepared"
    );
    tracing::trace!(
        endpoint = endpoint,
        requested_model = %payload.model,
        upstream_model = ?model_resolution.upstream_model,
        conversation_id = %conversation_id,
        request_body = %request_body,
        "Kiro request body"
    );

    // 估算输入 tokens
    let input_tokens = token::count_all_tokens(
        payload.model.clone(),
        payload.system.clone(),
        payload.messages.clone(),
        payload.tools.clone(),
    ) as i32;
    let usage_context = prepare_usage_context(
        &state,
        runtime_config.clone(),
        endpoint,
        payload.stream,
        &payload,
        Some(model_resolution.clone()),
        Some(conversation_id),
        prompt_cache_scope_conversation_id(state.prompt_cache_simulation_mode, &payload),
        input_tokens,
    )
    .with_payload_diagnostics(payload_breakdown, payload_guard_report.clone());

    // 检查是否启用了thinking
    let thinking_enabled = payload
        .thinking
        .as_ref()
        .map(|t| t.is_enabled())
        .unwrap_or(false);

    let tool_name_map = conversion_result.tool_name_map;
    let known_tool_names = conversion_result.known_tool_names;
    let warnings_header = if should_expose_proxy_warnings(&runtime_config) {
        merge_warning_headers(
            conversion_result.warnings.encode_header(),
            Some(&payload_guard_report),
        )
    } else {
        None
    };
    let extract_xml_thinking = runtime_config.compat_profile.allows_unsigned_thinking();

    if payload.stream {
        // 流式响应
        handle_stream_request(
            provider,
            &request_body,
            &payload.model,
            model_resolution
                .upstream_model
                .as_deref()
                .unwrap_or(&payload.model),
            input_tokens,
            usage_context.context_window_tokens,
            thinking_enabled,
            extract_xml_thinking,
            tool_name_map,
            known_tool_names,
            usage_context,
            warnings_header,
            too_long_retry,
            external_fallback,
        )
        .await
    } else {
        // 非流式响应：仅在配置开启时提取 thinking 块
        let extract_thinking = should_extract_unsigned_thinking(&runtime_config, thinking_enabled);
        handle_non_stream_request(
            provider,
            &request_body,
            &payload.model,
            model_resolution
                .upstream_model
                .as_deref()
                .unwrap_or(&payload.model),
            input_tokens,
            extract_thinking,
            tool_name_map,
            known_tool_names,
            usage_context,
            warnings_header,
            too_long_retry,
            external_fallback,
        )
        .await
    }
}

async fn call_api_stream_maybe_fail_fast(
    provider: &Arc<KiroProvider>,
    request_body: &str,
    request_id: Option<&str>,
    external_fallback: Option<&ExternalFallbackContext>,
) -> anyhow::Result<crate::kiro::provider::KiroStreamResponse> {
    if let Some(external) = external_fallback {
        if external.should_fail_fast_local().await {
            return provider
                .call_api_stream_with_request_id_fail_fast(request_body, request_id)
                .await;
        }
    }
    provider
        .call_api_stream_with_request_id(request_body, request_id)
        .await
}

async fn call_api_maybe_fail_fast(
    provider: &Arc<KiroProvider>,
    request_body: &str,
    request_id: Option<&str>,
    external_fallback: Option<&ExternalFallbackContext>,
) -> anyhow::Result<crate::kiro::provider::KiroApiResponse> {
    if let Some(external) = external_fallback {
        if external.should_fail_fast_local().await {
            return provider
                .call_api_with_context_with_request_id_fail_fast(request_body, request_id)
                .await;
        }
    }
    provider
        .call_api_with_context_with_request_id(request_body, request_id)
        .await
}

async fn maybe_forward_external_after_local_error(
    external_fallback: Option<&ExternalFallbackContext>,
    request_id: &str,
    message: &str,
    attempts: Vec<KiroCredentialAttempt>,
) -> Option<Response> {
    external_fallback?
        .fallback_after_local_error(request_id, message, attempts)
        .await
}

async fn maybe_external_fallback_after_local_error_outcome(
    external_fallback: Option<&ExternalFallbackContext>,
    request_id: &str,
    message: &str,
    attempts: Vec<KiroCredentialAttempt>,
) -> Option<ExternalPoolForwardOutcome> {
    external_fallback?
        .fallback_after_local_error_outcome(request_id, message, attempts)
        .await
}

fn local_rescue_reason_after_external_error(
    config: &ExternalPoolsConfig,
    err: &ExternalPoolFinalError,
    local_fallback_reason: Option<&str>,
) -> Option<&'static str> {
    if local_fallback_reason.is_some() {
        return None;
    }
    if !config.external_pool_local_rescue_enabled {
        return None;
    }
    if config.external_pool_local_rescue_on_rate_limit && err.is_rate_limit() {
        return Some("external_rate_limit");
    }
    if config.external_pool_local_rescue_on_timeout && err.is_timeout_like() {
        return Some("external_timeout");
    }
    if config.external_pool_local_rescue_on_capacity && err.is_capacity_like() {
        return Some("external_capacity");
    }
    None
}

fn external_rescue_preflight(reason: &str, err: &ExternalPoolFinalError) -> serde_json::Value {
    json!({
        "reason": reason,
        "externalStatus": err.status.as_u16(),
        "externalErrorType": err.route_error_type,
        "externalResponseErrorType": err.response_error_type,
        "externalRetryable": err.retryable,
        "externalPoolId": err.pool_id,
        "externalPoolName": err.pool_name,
        "externalAttemptCount": err.attempts.len(),
        "externalError": err.message,
    })
}

/// 处理流式请求
async fn handle_stream_request(
    provider: std::sync::Arc<crate::kiro::provider::KiroProvider>,
    request_body: &str,
    model: &str,
    preflight_model: &str,
    input_tokens: i32,
    context_window_tokens: i32,
    thinking_enabled: bool,
    extract_xml_thinking: bool,
    tool_name_map: HashMap<String, String>,
    known_tool_names: HashSet<String>,
    usage_context: RequestUsageContext,
    warnings_header: Option<String>,
    too_long_retry: Option<PayloadTooLongRetryRequest>,
    external_fallback: Option<ExternalFallbackContext>,
) -> Response {
    // 调用 Kiro API（支持多凭据故障转移）
    let mut usage_context = usage_context;
    let mut warnings_header = warnings_header;
    let request_id = usage_context.request_id.clone();
    let mut retry_attempt_prefix: Vec<KiroCredentialAttempt> = Vec::new();
    if let Some(external) = external_fallback.as_ref() {
        if let Some(outcome) = external
            .local_pool_preflight_outcome(provider.as_ref(), &request_id, preflight_model)
            .await
        {
            return match outcome {
                ExternalPoolForwardOutcome::Response(response) => response,
                ExternalPoolForwardOutcome::FinalError(err) => err.into_response(&request_id),
            };
        }
    }
    let response = match call_api_stream_maybe_fail_fast(
        &provider,
        request_body,
        Some(&request_id),
        external_fallback.as_ref(),
    )
    .await
    {
        Ok(resp) => resp,
        Err(e) => {
            let message = e.to_string();
            let attempts = KiroProvider::attempts_from_error(&e);
            log_provider_call_failure(&message);
            if let Some(retry) = too_long_retry.filter(|retry| {
                should_retry_payload_guard_after_error(
                    &message,
                    request_body.len(),
                    retry.config.max_bytes,
                )
            }) {
                tracing::warn!(
                    request_id,
                    "Kiro stream request rejected as too long; applying configured payload guard and retrying once"
                );
                retry_attempt_prefix = attempts.clone();
                let (retry_body, retry_warnings_header) =
                    match retry.build_retry_body(&mut usage_context) {
                        Ok(result) => result,
                        Err(err) => {
                            usage_context
                            .attach_provider_error_credential(&provider, &message, attempts)
                            .record_failure(
                                UsageRecordStatus::Error,
                                "payload_guard_error",
                                format!(
                                    "payload guard retry failed after upstream too-long error: {}",
                                    err
                                ),
                            );
                            return payload_guard_error_response(err);
                        }
                    };
                warnings_header = retry_warnings_header;
                match call_api_stream_maybe_fail_fast(
                    &provider,
                    &retry_body,
                    Some(&request_id),
                    external_fallback.as_ref(),
                )
                .await
                {
                    Ok(resp) => resp,
                    Err(retry_error) => {
                        let retry_message = retry_error.to_string();
                        let retry_attempts = KiroProvider::attempts_from_error(&retry_error);
                        let all_attempts =
                            merge_credential_attempts(retry_attempt_prefix.clone(), retry_attempts);
                        log_provider_call_failure(&retry_message);
                        if let Some(outcome) = maybe_external_fallback_after_local_error_outcome(
                            external_fallback.as_ref(),
                            &request_id,
                            &retry_message,
                            all_attempts.clone(),
                        )
                        .await
                        {
                            match outcome {
                                ExternalPoolForwardOutcome::Response(response) => return response,
                                ExternalPoolForwardOutcome::FinalError(err) => {
                                    if let Some(external) = external_fallback.as_ref() {
                                        let local_fallback_reason =
                                            classify_local_error_for_external_fallback(
                                                &retry_message,
                                                &all_attempts,
                                                &external.config,
                                            );
                                        if let Some(reason) =
                                            local_rescue_reason_after_external_error(
                                                &external.config,
                                                &err,
                                                local_fallback_reason.as_deref(),
                                            )
                                        {
                                            tracing::warn!(
                                                request_id,
                                                reason,
                                                max_wait_secs = external
                                                    .config
                                                    .external_pool_local_rescue_max_wait_secs,
                                                "external fallback failed with a rescuable error; retrying local credentials once"
                                            );
                                            usage_context.mark_local_rescue_after_external(
                                                reason,
                                                Some(external_rescue_preflight(reason, &err)),
                                                err.attempts.clone(),
                                            );
                                            retry_attempt_prefix = all_attempts.clone();
                                            match provider
                                                .call_api_stream_with_request_id_max_wait(
                                                    &retry_body,
                                                    Some(&request_id),
                                                    Duration::from_secs(
                                                        external
                                                            .config
                                                            .external_pool_local_rescue_max_wait_secs,
                                                    ),
                                                )
                                                .await
                                            {
                                                Ok(resp) => resp,
                                                Err(rescue_error) => {
                                                    let rescue_message = rescue_error.to_string();
                                                    let rescue_attempts =
                                                        KiroProvider::attempts_from_error(
                                                            &rescue_error,
                                                        );
                                                    let all_attempts =
                                                        merge_credential_attempts(
                                                            retry_attempt_prefix.clone(),
                                                            rescue_attempts,
                                                        );
                                                    log_provider_call_failure(&rescue_message);
                                                    usage_context
                                                        .attach_provider_error_credential(
                                                            &provider,
                                                            &rescue_message,
                                                            all_attempts,
                                                        )
                                                        .record_failure(
                                                            UsageRecordStatus::Error,
                                                            "api_error",
                                                            rescue_message,
                                                        );
                                                    return map_provider_error(
                                                        rescue_error,
                                                        Some(&request_id),
                                                        Some(provider.as_ref()),
                                                    );
                                                }
                                            }
                                        } else {
                                            return err.into_response(&request_id);
                                        }
                                    } else {
                                        return err.into_response(&request_id);
                                    }
                                }
                            }
                        } else {
                            usage_context
                                .attach_provider_error_credential(
                                    &provider,
                                    &retry_message,
                                    all_attempts,
                                )
                                .record_failure(
                                    UsageRecordStatus::Error,
                                    "api_error",
                                    retry_message,
                                );
                            return map_provider_error(
                                retry_error,
                                Some(&request_id),
                                Some(provider.as_ref()),
                            );
                        }
                    }
                }
            } else {
                if let Some(outcome) = maybe_external_fallback_after_local_error_outcome(
                    external_fallback.as_ref(),
                    &request_id,
                    &message,
                    attempts.clone(),
                )
                .await
                {
                    match outcome {
                        ExternalPoolForwardOutcome::Response(response) => return response,
                        ExternalPoolForwardOutcome::FinalError(err) => {
                            if let Some(external) = external_fallback.as_ref() {
                                let local_fallback_reason =
                                    classify_local_error_for_external_fallback(
                                        &message,
                                        &attempts,
                                        &external.config,
                                    );
                                if let Some(reason) = local_rescue_reason_after_external_error(
                                    &external.config,
                                    &err,
                                    local_fallback_reason.as_deref(),
                                ) {
                                    tracing::warn!(
                                        request_id,
                                        reason,
                                        max_wait_secs = external
                                            .config
                                            .external_pool_local_rescue_max_wait_secs,
                                        "external fallback failed with a rescuable error; retrying local credentials once"
                                    );
                                    usage_context.mark_local_rescue_after_external(
                                        reason,
                                        Some(external_rescue_preflight(reason, &err)),
                                        err.attempts.clone(),
                                    );
                                    retry_attempt_prefix = attempts.clone();
                                    match provider
                                        .call_api_stream_with_request_id_max_wait(
                                            request_body,
                                            Some(&request_id),
                                            Duration::from_secs(
                                                external
                                                    .config
                                                    .external_pool_local_rescue_max_wait_secs,
                                            ),
                                        )
                                        .await
                                    {
                                        Ok(resp) => resp,
                                        Err(rescue_error) => {
                                            let rescue_message = rescue_error.to_string();
                                            let rescue_attempts =
                                                KiroProvider::attempts_from_error(&rescue_error);
                                            let all_attempts = merge_credential_attempts(
                                                retry_attempt_prefix.clone(),
                                                rescue_attempts,
                                            );
                                            log_provider_call_failure(&rescue_message);
                                            usage_context
                                                .attach_provider_error_credential(
                                                    &provider,
                                                    &rescue_message,
                                                    all_attempts,
                                                )
                                                .record_failure(
                                                    UsageRecordStatus::Error,
                                                    "api_error",
                                                    rescue_message,
                                                );
                                            return map_provider_error(
                                                rescue_error,
                                                Some(&request_id),
                                                Some(provider.as_ref()),
                                            );
                                        }
                                    }
                                } else {
                                    return err.into_response(&request_id);
                                }
                            } else {
                                return err.into_response(&request_id);
                            }
                        }
                    }
                } else {
                    usage_context
                        .attach_provider_error_credential(&provider, &message, attempts)
                        .record_failure(UsageRecordStatus::Error, "api_error", message);
                    return map_provider_error(e, Some(&request_id), Some(provider.as_ref()));
                }
            }
        }
    };
    let (response, completion) = response.into_parts();
    let credential_attempts =
        merge_credential_attempts(retry_attempt_prefix, completion.attempts().to_vec());
    let credential_usage = prepare_credential_usage_context(
        usage_context,
        &provider,
        completion.credential_id(),
        completion.sticky_bound(),
        completion.fallback_from_sticky(),
        credential_attempts,
    );

    // 创建流处理上下文
    let mut ctx = StreamContext::new_with_simulation_with_known_tools(
        model,
        input_tokens,
        context_window_tokens,
        thinking_enabled,
        extract_xml_thinking,
        tool_name_map,
        known_tool_names,
        credential_usage.request.simulated_usage,
        credential_usage.request.simulation_mode,
    );
    ctx.set_reported_cache_usage_policy(credential_usage.request.reported_cache_usage_policy());

    // 生成初始事件
    let initial_events = ctx.generate_initial_events_with_reported_usage_mapper(|reported_usage| {
        credential_usage
            .preview_creation_frequency_control(reported_usage, UsageSource::LocalPromptCache)
    });

    // 创建 SSE 流
    let response_request_id = credential_usage.request.request_id.clone();
    let stream = create_sse_stream(response, ctx, initial_events, completion, credential_usage);

    // 返回 SSE 响应
    let mut builder = envelope::sse_builder_with_id(&response_request_id);
    if let Some(warnings) = warnings_header {
        builder = builder.header("x-kiro-rs-warnings", warnings);
    }
    builder.body(Body::from_stream(stream)).unwrap()
}

/// Ping 事件间隔（25秒）
const PING_INTERVAL_SECS: u64 = 25;
/// 上游 eventstream 读空闲超时（180秒）
const UPSTREAM_IDLE_TIMEOUT_SECS: u64 = 180;

/// 创建 ping 事件的 SSE 字符串
fn create_ping_sse() -> Bytes {
    Bytes::from("event: ping\ndata: {\"type\": \"ping\"}\n\n")
}

/// 创建 SSE 事件流
fn create_sse_stream(
    response: reqwest::Response,
    ctx: StreamContext,
    initial_events: Vec<SseEvent>,
    completion: KiroStreamCompletion,
    usage_context: CredentialUsageContext,
) -> impl Stream<Item = Result<Bytes, Infallible>> {
    let usage_guard = StreamUsageGuard::new(usage_context);
    // 先发送初始事件
    let initial_stream = stream::iter(
        initial_events
            .into_iter()
            .map(|e| Ok(Bytes::from(e.to_sse_string()))),
    );

    // 然后处理 Kiro 响应流，同时每25秒发送 ping 保活
    let body_stream = response.bytes_stream();

    let processing_stream = stream::unfold(
        (
            body_stream,
            ctx,
            EventStreamDecoder::new(),
            false,
            completion,
            usage_guard,
            interval(Duration::from_secs(PING_INTERVAL_SECS)),
            Instant::now() + Duration::from_secs(UPSTREAM_IDLE_TIMEOUT_SECS),
        ),
        |(
            mut body_stream,
            mut ctx,
            mut decoder,
            finished,
            completion,
            usage_guard,
            mut ping_interval,
            mut idle_deadline,
        )| async move {
            if finished {
                return None;
            }

            let idle_sleep = sleep_until(idle_deadline);
            tokio::pin!(idle_sleep);

            // 使用 select! 同时等待数据、ping 定时器和上游空闲超时
            tokio::select! {
                // 处理数据流
                chunk_result = body_stream.next() => {
                    match chunk_result {
                        Some(Ok(chunk)) => {
                            idle_deadline = Instant::now() + Duration::from_secs(UPSTREAM_IDLE_TIMEOUT_SECS);
                            completion.touch();
                            // 解码事件
                            if let Err(e) = decoder.feed(&chunk) {
                                tracing::warn!("缓冲区溢出: {}", e);
                            }

                            let mut events = Vec::new();
                            for result in decoder.decode_iter() {
                                match result {
                                    Ok(frame) => {
                                        if let Ok(event) = Event::from_frame(frame) {
                                            let sse_events = ctx.process_kiro_event(&event);
                                            events.extend(sse_events);
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!("解码事件失败: {}", e);
                                    }
                                }
                            }

                            // 转换为 SSE 字节流
                            usage_guard
                                .context()
                                .request
                                .mark_first_token_if_output(&events);
                            let bytes: Vec<Result<Bytes, Infallible>> = events
                                .into_iter()
                                .map(|e| Ok(Bytes::from(e.to_sse_string())))
                                .collect();

                            Some((stream::iter(bytes), (body_stream, ctx, decoder, false, completion, usage_guard, ping_interval, idle_deadline)))
                        }
                        Some(Err(e)) => {
                            tracing::error!("读取响应流失败: {}", e);
                            completion.report_upstream_stream_failure(format!(
                                "upstream stream read error: {}",
                                e
                            ));
                            // 读取错误：关闭已有内容块后发送 SSE error，不再发送正常 message_stop。
                            ctx.record_stream_error("api_error", format!("upstream stream read error: {}", e));
                            let error_detail = ctx.stream_error_detail();
                            let final_events = ctx.generate_final_events();
                            usage_guard
                                .context()
                                .request
                                .mark_first_token_if_output(&final_events);
                            usage_guard.context().record_stream_failure_from_context(
                                UsageRecordStatus::StreamError,
                                ctx.final_usage(),
                                error_detail,
                                ctx.metadata_usage(),
                                ctx.context_input_tokens,
                            );
                            usage_guard.complete();
                            let bytes: Vec<Result<Bytes, Infallible>> = final_events
                                .into_iter()
                                .map(|e| Ok(Bytes::from(e.to_sse_string())))
                                .collect();
                            Some((stream::iter(bytes), (body_stream, ctx, decoder, true, completion, usage_guard, ping_interval, idle_deadline)))
                        }
                        None => {
                            // 流结束，发送最终事件
                            if ctx.has_stream_error() {
                                let scheduler_reason = ctx
                                    .stream_error_detail()
                                    .map(|(kind, detail)| format!("{}: {}", kind, detail))
                                    .unwrap_or_else(|| "upstream stream error event".to_string());
                                completion.report_upstream_stream_failure(scheduler_reason);
                            } else {
                                completion.report_success();
                            }
                            let had_stream_error = ctx.has_stream_error();
                            let error_detail = ctx.stream_error_detail();
                            let final_events = if had_stream_error {
                                ctx.generate_final_events()
                            } else {
                                ctx.generate_final_events_with_reported_usage_mapper(
                                    |final_usage, _reported_usage, metadata_usage, context_estimated| {
                                        usage_guard.context().final_reported_usage_for_stream(
                                            final_usage,
                                            metadata_usage,
                                            context_estimated,
                                        )
                                    },
                                )
                            };
                            usage_guard
                                .context()
                                .request
                                .mark_first_token_if_output(&final_events);
                            if had_stream_error {
                                usage_guard.context().record_stream_failure_from_context(
                                    UsageRecordStatus::StreamError,
                                    ctx.final_usage(),
                                    error_detail,
                                    ctx.metadata_usage(),
                                    ctx.context_input_tokens,
                                );
                            } else {
                                usage_guard.context().record_success_from_stream(&ctx);
                            }
                            usage_guard.complete();
                            let bytes: Vec<Result<Bytes, Infallible>> = final_events
                                .into_iter()
                                .map(|e| Ok(Bytes::from(e.to_sse_string())))
                                .collect();
                            Some((stream::iter(bytes), (body_stream, ctx, decoder, true, completion, usage_guard, ping_interval, idle_deadline)))
                        }
                    }
                }
                _ = &mut idle_sleep => {
                    tracing::error!(
                        "上游响应流超过 {} 秒未产生数据，结束流并发送错误事件",
                        UPSTREAM_IDLE_TIMEOUT_SECS
                    );
                    completion.report_upstream_stream_failure("upstream stream idle timeout");
                    ctx.record_stream_error("api_error", "upstream stream idle timeout");
                    let error_detail = ctx.stream_error_detail();
                    let final_events = ctx.generate_final_events();
                    usage_guard
                        .context()
                        .request
                        .mark_first_token_if_output(&final_events);
                    usage_guard.context().record_stream_failure_from_context(
                        UsageRecordStatus::UpstreamTimeout,
                        ctx.final_usage(),
                        error_detail,
                        ctx.metadata_usage(),
                        ctx.context_input_tokens,
                    );
                    usage_guard.complete();
                    let bytes: Vec<Result<Bytes, Infallible>> = final_events
                        .into_iter()
                        .map(|e| Ok(Bytes::from(e.to_sse_string())))
                        .collect();
                    Some((stream::iter(bytes), (body_stream, ctx, decoder, true, completion, usage_guard, ping_interval, idle_deadline)))
                }
                // 发送 ping 保活
                _ = ping_interval.tick() => {
                    tracing::trace!("发送 ping 保活事件");
                    let bytes: Vec<Result<Bytes, Infallible>> = vec![Ok(create_ping_sse())];
                    Some((stream::iter(bytes), (body_stream, ctx, decoder, false, completion, usage_guard, ping_interval, idle_deadline)))
                }
            }
        },
    )
    .flatten();

    initial_stream.chain(processing_stream)
}

use super::converter::get_context_window_size;

/// 处理非流式请求
async fn handle_non_stream_request(
    provider: std::sync::Arc<crate::kiro::provider::KiroProvider>,
    request_body: &str,
    model: &str,
    preflight_model: &str,
    input_tokens: i32,
    thinking_enabled: bool,
    tool_name_map: HashMap<String, String>,
    known_tool_names: HashSet<String>,
    usage_context: RequestUsageContext,
    warnings_header: Option<String>,
    too_long_retry: Option<PayloadTooLongRetryRequest>,
    external_fallback: Option<ExternalFallbackContext>,
) -> Response {
    // 调用 Kiro API（支持多凭据故障转移）
    let mut usage_context = usage_context;
    let mut warnings_header = warnings_header;
    let request_id = usage_context.request_id.clone();
    let mut retry_attempt_prefix: Vec<KiroCredentialAttempt> = Vec::new();
    if let Some(external) = external_fallback.as_ref() {
        if let Some(outcome) = external
            .local_pool_preflight_outcome(provider.as_ref(), &request_id, preflight_model)
            .await
        {
            return match outcome {
                ExternalPoolForwardOutcome::Response(response) => response,
                ExternalPoolForwardOutcome::FinalError(err) => err.into_response(&request_id),
            };
        }
    }
    let api_response = match call_api_maybe_fail_fast(
        &provider,
        request_body,
        Some(&request_id),
        external_fallback.as_ref(),
    )
    .await
    {
        Ok(resp) => resp,
        Err(e) => {
            let message = e.to_string();
            let attempts = KiroProvider::attempts_from_error(&e);
            log_provider_call_failure(&message);
            if let Some(retry) = too_long_retry.filter(|retry| {
                should_retry_payload_guard_after_error(
                    &message,
                    request_body.len(),
                    retry.config.max_bytes,
                )
            }) {
                tracing::warn!(
                    request_id,
                    "Kiro non-stream request rejected as too long; applying configured payload guard and retrying once"
                );
                retry_attempt_prefix = attempts.clone();
                let (retry_body, retry_warnings_header) =
                    match retry.build_retry_body(&mut usage_context) {
                        Ok(result) => result,
                        Err(err) => {
                            usage_context
                            .attach_provider_error_credential(&provider, &message, attempts)
                            .record_failure(
                                UsageRecordStatus::Error,
                                "payload_guard_error",
                                format!(
                                    "payload guard retry failed after upstream too-long error: {}",
                                    err
                                ),
                            );
                            return payload_guard_error_response(err);
                        }
                    };
                warnings_header = retry_warnings_header;
                match call_api_maybe_fail_fast(
                    &provider,
                    &retry_body,
                    Some(&request_id),
                    external_fallback.as_ref(),
                )
                .await
                {
                    Ok(resp) => resp,
                    Err(retry_error) => {
                        let retry_message = retry_error.to_string();
                        let retry_attempts = KiroProvider::attempts_from_error(&retry_error);
                        let all_attempts =
                            merge_credential_attempts(retry_attempt_prefix.clone(), retry_attempts);
                        log_provider_call_failure(&retry_message);
                        if let Some(outcome) = maybe_external_fallback_after_local_error_outcome(
                            external_fallback.as_ref(),
                            &request_id,
                            &retry_message,
                            all_attempts.clone(),
                        )
                        .await
                        {
                            match outcome {
                                ExternalPoolForwardOutcome::Response(response) => return response,
                                ExternalPoolForwardOutcome::FinalError(err) => {
                                    if let Some(external) = external_fallback.as_ref() {
                                        let local_fallback_reason =
                                            classify_local_error_for_external_fallback(
                                                &retry_message,
                                                &all_attempts,
                                                &external.config,
                                            );
                                        if let Some(reason) =
                                            local_rescue_reason_after_external_error(
                                                &external.config,
                                                &err,
                                                local_fallback_reason.as_deref(),
                                            )
                                        {
                                            tracing::warn!(
                                                request_id,
                                                reason,
                                                max_wait_secs = external
                                                    .config
                                                    .external_pool_local_rescue_max_wait_secs,
                                                "external fallback failed with a rescuable error; retrying local credentials once"
                                            );
                                            usage_context.mark_local_rescue_after_external(
                                                reason,
                                                Some(external_rescue_preflight(reason, &err)),
                                                err.attempts.clone(),
                                            );
                                            retry_attempt_prefix = all_attempts.clone();
                                            match provider
                                                .call_api_with_context_with_request_id_max_wait(
                                                    &retry_body,
                                                    Some(&request_id),
                                                    Duration::from_secs(
                                                        external
                                                            .config
                                                            .external_pool_local_rescue_max_wait_secs,
                                                    ),
                                                )
                                                .await
                                            {
                                                Ok(resp) => resp,
                                                Err(rescue_error) => {
                                                    let rescue_message = rescue_error.to_string();
                                                    let rescue_attempts =
                                                        KiroProvider::attempts_from_error(
                                                            &rescue_error,
                                                        );
                                                    let all_attempts =
                                                        merge_credential_attempts(
                                                            retry_attempt_prefix.clone(),
                                                            rescue_attempts,
                                                        );
                                                    log_provider_call_failure(&rescue_message);
                                                    usage_context
                                                        .attach_provider_error_credential(
                                                            &provider,
                                                            &rescue_message,
                                                            all_attempts,
                                                        )
                                                        .record_failure(
                                                            UsageRecordStatus::Error,
                                                            "api_error",
                                                            rescue_message,
                                                        );
                                                    return map_provider_error(
                                                        rescue_error,
                                                        Some(&request_id),
                                                        Some(provider.as_ref()),
                                                    );
                                                }
                                            }
                                        } else {
                                            return err.into_response(&request_id);
                                        }
                                    } else {
                                        return err.into_response(&request_id);
                                    }
                                }
                            }
                        } else {
                            usage_context
                                .attach_provider_error_credential(
                                    &provider,
                                    &retry_message,
                                    all_attempts,
                                )
                                .record_failure(
                                    UsageRecordStatus::Error,
                                    "api_error",
                                    retry_message,
                                );
                            return map_provider_error(
                                retry_error,
                                Some(&request_id),
                                Some(provider.as_ref()),
                            );
                        }
                    }
                }
            } else {
                if let Some(outcome) = maybe_external_fallback_after_local_error_outcome(
                    external_fallback.as_ref(),
                    &request_id,
                    &message,
                    attempts.clone(),
                )
                .await
                {
                    match outcome {
                        ExternalPoolForwardOutcome::Response(response) => return response,
                        ExternalPoolForwardOutcome::FinalError(err) => {
                            if let Some(external) = external_fallback.as_ref() {
                                let local_fallback_reason =
                                    classify_local_error_for_external_fallback(
                                        &message,
                                        &attempts,
                                        &external.config,
                                    );
                                if let Some(reason) = local_rescue_reason_after_external_error(
                                    &external.config,
                                    &err,
                                    local_fallback_reason.as_deref(),
                                ) {
                                    tracing::warn!(
                                        request_id,
                                        reason,
                                        max_wait_secs = external
                                            .config
                                            .external_pool_local_rescue_max_wait_secs,
                                        "external fallback failed with a rescuable error; retrying local credentials once"
                                    );
                                    usage_context.mark_local_rescue_after_external(
                                        reason,
                                        Some(external_rescue_preflight(reason, &err)),
                                        err.attempts.clone(),
                                    );
                                    retry_attempt_prefix = attempts.clone();
                                    match provider
                                        .call_api_with_context_with_request_id_max_wait(
                                            request_body,
                                            Some(&request_id),
                                            Duration::from_secs(
                                                external
                                                    .config
                                                    .external_pool_local_rescue_max_wait_secs,
                                            ),
                                        )
                                        .await
                                    {
                                        Ok(resp) => resp,
                                        Err(rescue_error) => {
                                            let rescue_message = rescue_error.to_string();
                                            let rescue_attempts =
                                                KiroProvider::attempts_from_error(&rescue_error);
                                            let all_attempts = merge_credential_attempts(
                                                retry_attempt_prefix.clone(),
                                                rescue_attempts,
                                            );
                                            log_provider_call_failure(&rescue_message);
                                            usage_context
                                                .attach_provider_error_credential(
                                                    &provider,
                                                    &rescue_message,
                                                    all_attempts,
                                                )
                                                .record_failure(
                                                    UsageRecordStatus::Error,
                                                    "api_error",
                                                    rescue_message,
                                                );
                                            return map_provider_error(
                                                rescue_error,
                                                Some(&request_id),
                                                Some(provider.as_ref()),
                                            );
                                        }
                                    }
                                } else {
                                    return err.into_response(&request_id);
                                }
                            } else {
                                return err.into_response(&request_id);
                            }
                        }
                    }
                } else {
                    usage_context
                        .attach_provider_error_credential(&provider, &message, attempts)
                        .record_failure(UsageRecordStatus::Error, "api_error", message);
                    return map_provider_error(e, Some(&request_id), Some(provider.as_ref()));
                }
            }
        }
    };
    let credential_attempts =
        merge_credential_attempts(retry_attempt_prefix, api_response.attempts().to_vec());
    let credential_usage = prepare_credential_usage_context(
        usage_context,
        &provider,
        api_response.credential_id(),
        api_response.sticky_bound(),
        api_response.fallback_from_sticky(),
        credential_attempts,
    );
    let (response, completion) = api_response.into_parts();

    // 读取响应体
    let body_bytes = match response_bytes_with_body_timeout(
        response,
        provider
            .runtime_config()
            .kiro_upstream_response_timeout_secs,
    )
    .await
    {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::error!("读取响应体失败: {}", e);
            credential_usage.record_failure(
                UsageRecordStatus::Error,
                "api_error",
                format!("读取响应失败: {}", e),
            );
            completion.release();
            return envelope::error_response_with_id(
                StatusCode::BAD_GATEWAY,
                "api_error",
                format!("读取响应失败: {}", e),
                &credential_usage.request.request_id,
            );
        }
    };

    // 解析事件流
    let mut decoder = EventStreamDecoder::new();
    if let Err(e) = decoder.feed(&body_bytes) {
        tracing::warn!("缓冲区溢出: {}", e);
    }

    let mut text_content = String::new();
    let mut tool_uses: Vec<serde_json::Value> = Vec::new();
    let mut has_tool_use = false;
    let mut stop_reason = "end_turn".to_string();
    // 从 contextUsageEvent 计算的实际输入 tokens
    let mut context_input_tokens: Option<i32> = None;
    let mut metadata_usage: Option<crate::kiro::model::events::MetadataTokenUsage> = None;
    let mut native_thinking_content = String::new();
    let mut native_thinking_signature: Option<String> = None;
    let mut redacted_thinking: Option<String> = None;
    let mut seen_tool_sigs: HashSet<String> = HashSet::new();

    // 收集工具调用的增量 JSON
    let mut tool_json_buffers: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    for result in decoder.decode_iter() {
        match result {
            Ok(frame) => {
                if let Ok(event) = Event::from_frame(frame) {
                    match event {
                        Event::AssistantResponse(resp) => {
                            text_content.push_str(&resp.content);
                        }
                        Event::Code(code) => {
                            text_content.push_str(&code.content);
                        }
                        Event::ReasoningContent(reasoning) => {
                            if let Some(redacted) = reasoning.redacted_content {
                                if !redacted.is_empty() {
                                    redacted_thinking = Some(redacted);
                                }
                            }
                            if !reasoning.text.is_empty() {
                                native_thinking_content = reasoning.text;
                            }
                            if reasoning.signature.is_some() {
                                native_thinking_signature = reasoning.signature;
                            }
                        }
                        Event::ToolUse(tool_use) => {
                            has_tool_use = true;

                            // 累积工具的 JSON 输入
                            let buffer = tool_json_buffers
                                .entry(tool_use.tool_use_id.clone())
                                .or_insert_with(String::new);
                            buffer.push_str(&tool_use.input);

                            // 如果是完整的工具调用，添加到列表
                            if tool_use.stop {
                                let input: serde_json::Value = if buffer.is_empty() {
                                    serde_json::json!({})
                                } else {
                                    serde_json::from_str(buffer).unwrap_or_else(|e| {
                                        tracing::warn!(
                                            "工具输入 JSON 解析失败: {}, tool_use_id: {}",
                                            e,
                                            tool_use.tool_use_id
                                        );
                                        serde_json::json!({})
                                    })
                                };

                                let original_name = tool_name_map
                                    .get(&tool_use.name)
                                    .cloned()
                                    .unwrap_or_else(|| tool_use.name.clone());
                                let sig = crate::anthropic::stream::tool_use_signature(
                                    &original_name,
                                    &input,
                                );
                                if seen_tool_sigs.insert(sig) {
                                    tool_uses.push(json!({
                                        "type": "tool_use",
                                        "id": tool_use.tool_use_id,
                                        "name": original_name,
                                        "input": input
                                    }));
                                } else {
                                    tracing::debug!(
                                        tool = %original_name,
                                        tool_use_id = %tool_use.tool_use_id,
                                        "重复的结构化 tool_use 已跳过"
                                    );
                                }
                            }
                        }
                        Event::ContextUsage(context_usage) => {
                            // 从上下文使用百分比计算实际的 input_tokens
                            let window_size = credential_usage.request.context_window_tokens;
                            let actual_input_tokens =
                                (context_usage.context_usage_percentage * (window_size as f64)
                                    / 100.0) as i32;
                            context_input_tokens = Some(actual_input_tokens);
                            // 上下文使用量达到 100% 时，设置 stop_reason 为 model_context_window_exceeded
                            if context_usage.context_usage_percentage >= 100.0 {
                                stop_reason = "model_context_window_exceeded".to_string();
                            }
                            tracing::debug!(
                                "收到 contextUsageEvent: {}%, 计算 input_tokens: {}",
                                context_usage.context_usage_percentage,
                                actual_input_tokens
                            );
                        }
                        Event::Metadata(metadata) => {
                            if let Some(token_usage) = metadata.token_usage {
                                tracing::debug!(
                                    input_tokens = token_usage.input_tokens(),
                                    output_tokens = token_usage.output_tokens,
                                    cache_read_input_tokens = token_usage.cache_read_input_tokens,
                                    cache_write_input_tokens = token_usage.cache_write_input_tokens,
                                    "非流式响应收到 metadataEvent token usage"
                                );
                                metadata_usage = Some(token_usage);
                            }
                        }
                        Event::MessageMetadata(metadata) => {
                            if let Some(token_usage) = metadata.token_usage {
                                tracing::debug!(
                                    conversation_id = ?metadata.conversation_id,
                                    utterance_id = ?metadata.utterance_id,
                                    input_tokens = token_usage.input_tokens(),
                                    output_tokens = token_usage.output_tokens,
                                    cache_read_input_tokens = token_usage.cache_read_input_tokens,
                                    cache_write_input_tokens = token_usage.cache_write_input_tokens,
                                    "非流式响应收到 messageMetadataEvent token usage"
                                );
                                metadata_usage = Some(token_usage);
                            }
                        }
                        Event::Metering(metering) => {
                            tracing::debug!(usage = metering.usage, "非流式响应收到 meteringEvent");
                        }
                        Event::InvalidState(invalid) => {
                            let message = invalid.error_text();
                            tracing::warn!(
                                reason = %invalid.reason,
                                message = %message,
                                "非流式响应收到 invalidStateEvent"
                            );
                            credential_usage.record_failure(
                                UsageRecordStatus::Error,
                                "invalid_request_error",
                                message.clone(),
                            );
                            completion.release();
                            return envelope::error_response_with_id(
                                StatusCode::BAD_REQUEST,
                                "invalid_request_error",
                                message,
                                &credential_usage.request.request_id,
                            );
                        }
                        Event::Exception { exception_type, .. } => {
                            if exception_type == "ContentLengthExceededException" {
                                stop_reason = "max_tokens".to_string();
                            }
                        }
                        _ => {}
                    }
                }
            }
            Err(e) => {
                tracing::warn!("解码事件失败: {}", e);
            }
        }
    }

    // 确定 stop_reason
    if has_tool_use && stop_reason == "end_turn" {
        stop_reason = "tool_use".to_string();
    }

    // 构建响应内容
    let mut content: Vec<serde_json::Value> = Vec::new();
    let mut append_recovered_blocks = |text: &str, content: &mut Vec<serde_json::Value>| {
        if text.is_empty() {
            return;
        }
        for block in
            super::stream::extract_invoke_content_blocks(text, &known_tool_names, &tool_name_map)
        {
            if block["type"] == "tool_use" {
                let name = block["name"].as_str().unwrap_or("");
                let input = block["input"].clone();
                let sig = crate::anthropic::stream::tool_use_signature(name, &input);
                if seen_tool_sigs.insert(sig) {
                    content.push(block);
                } else {
                    tracing::debug!(tool = %name, "重复的泄漏 tool_use 已跳过");
                }
            } else if block["type"] == "text" {
                if block["text"].as_str().is_some_and(|text| !text.is_empty()) {
                    content.push(block);
                }
            } else {
                content.push(block);
            }
        }
    };

    if thinking_enabled && redacted_thinking.is_some() {
        content.push(json!({
            "type": "redacted_thinking",
            "data": redacted_thinking.unwrap()
        }));
    } else if thinking_enabled && !native_thinking_content.is_empty() {
        let mut thinking_block = json!({
            "type": "thinking",
            "thinking": native_thinking_content
        });
        if let Some(signature) = native_thinking_signature {
            if !signature.is_empty() {
                thinking_block["signature"] = json!(signature);
            }
        }
        content.push(thinking_block);
        append_recovered_blocks(&text_content, &mut content);
    } else if thinking_enabled {
        // 从完整文本中提取 thinking 块
        let (thinking, remaining_text) =
            super::stream::extract_thinking_from_complete_text(&text_content);

        if let Some(thinking_text) = thinking {
            content.push(json!({
                "type": "thinking",
                "thinking": thinking_text
            }));
        }

        append_recovered_blocks(&remaining_text, &mut content);
    } else if !text_content.is_empty() {
        append_recovered_blocks(&text_content, &mut content);
    }

    content.extend(tool_uses);

    // 估算输出 tokens
    let output_tokens = metadata_usage
        .as_ref()
        .map(|usage| usage.output_tokens)
        .unwrap_or_else(|| token::estimate_output_tokens(&content));

    // 优先使用 metadataEvent 的准确 usage，其次使用 contextUsageEvent 估算值。
    let final_input_tokens = metadata_usage
        .as_ref()
        .map(|usage| usage.total_input_tokens())
        .or(context_input_tokens)
        .unwrap_or(input_tokens);
    let usage_input_tokens =
        if should_build_local_prompt_cache_usage(credential_usage.request.simulation_mode) {
            final_input_tokens.max(credential_usage.request.input_tokens)
        } else {
            final_input_tokens
        };

    let usage = super::cache::build_usage_with_simulation_policy(
        metadata_usage.as_ref(),
        usage_input_tokens,
        output_tokens,
        credential_usage.request.simulated_usage,
        should_build_local_prompt_cache_usage(credential_usage.request.simulation_mode),
    );
    let has_metadata = metadata_usage.is_some();
    let context_estimated = !has_metadata && context_input_tokens.is_some();
    let usage_source =
        credential_usage.usage_source(&usage, metadata_usage.as_ref(), context_estimated);
    let reported_usage = credential_usage.canonical_reported_usage_for_success(usage, usage_source);
    let raw_usage = raw_usage_from_metadata_or_estimate(
        metadata_usage.as_ref(),
        final_input_tokens,
        output_tokens,
    );
    credential_usage.record_success_reported(reported_usage, usage_source, Some(raw_usage));
    completion.report_success();

    // 构建 Anthropic 响应
    let response_body = json!({
        "id": envelope::message_id(),
        "type": "message",
        "role": "assistant",
        "content": content,
        "model": model,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": reported_usage.to_anthropic_usage_json()
    });

    envelope::json_response_with_id(
        StatusCode::OK,
        response_body,
        &credential_usage.request.request_id,
        warnings_header,
    )
}

/// 检测模型名是否包含 "thinking" 后缀，若包含则在调用方未显式配置时注入 thinking
///
/// - 调用方已指定 `thinking` 字段：保留原值
/// - 调用方未指定：根据模型注入
///   - Opus 4.6 / 4.7：adaptive 类型
///   - 其他模型：enabled 类型
///   - budget_tokens 固定为 20000
/// - `output_config.effort` 同样仅在调用方未设置时填充
fn override_thinking_from_model_name(payload: &mut MessagesRequest) {
    let model_lower = payload.model.to_lowercase();
    if !model_lower.contains("thinking") {
        return;
    }

    let model_base = model_lower.strip_suffix("[1m]").unwrap_or(&model_lower);
    let is_opus_alias = matches!(
        model_base,
        "opus-thinking" | "opusplan-thinking" | "best-thinking" | "default-thinking"
    );
    let is_opus_4_7 = is_opus_alias
        || (model_base.contains("opus")
            && (model_base.contains("4-7")
                || model_base.contains("4.7")
                || model_base == "opus"
                || model_base == "opusplan"
                || model_base == "best"
                || model_base == "default"));
    let is_opus_4_6 =
        model_base.contains("opus") && (model_base.contains("4-6") || model_base.contains("4.6"));
    let is_adaptive_opus = is_opus_4_7 || is_opus_4_6;

    let thinking_type = if is_adaptive_opus {
        "adaptive"
    } else {
        "enabled"
    };

    if payload.thinking.is_none() {
        tracing::info!(
            model = %payload.model,
            thinking_type = thinking_type,
            "模型名包含 thinking 后缀，注入默认 thinking 配置"
        );
        payload.thinking = Some(Thinking {
            thinking_type: thinking_type.to_string(),
            budget_tokens: 20000,
        });
    } else {
        tracing::debug!(
            model = %payload.model,
            "调用方已指定 thinking 配置，保留原值"
        );
    }

    if is_adaptive_opus && payload.output_config.is_none() {
        payload.output_config = Some(OutputConfig {
            effort: if is_opus_4_7 { "xhigh" } else { "high" }.to_string(),
        });
    }
}

/// POST /v1/messages/count_tokens
///
/// 计算消息的 token 数量
pub async fn count_tokens(
    JsonExtractor(payload): JsonExtractor<CountTokensRequest>,
) -> impl IntoResponse {
    tracing::info!(
        model = %payload.model,
        message_count = %payload.messages.len(),
        "Received POST /v1/messages/count_tokens request"
    );

    let total_tokens = token::count_all_tokens(
        payload.model,
        payload.system,
        payload.messages,
        payload.tools,
    ) as i32;

    Json(CountTokensResponse {
        input_tokens: total_tokens.max(1) as i32,
    })
}

/// POST /cc/v1/messages
///
/// Claude Code 兼容端点，与 /v1/messages 的区别在于：
/// - 流式响应实时转发 Kiro eventstream，避免 Claude Code CLI 长时间没有过程输出
/// - 最终 usage 仍会在 message_delta 和 usage records 中修正
pub async fn post_messages_cc(
    State(state): State<AppState>,
    headers: HeaderMap,
    raw_body: Bytes,
) -> Response {
    let mut payload = match parse_messages_payload(&raw_body) {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    tracing::debug!(
        model = %payload.model,
        max_tokens = %payload.max_tokens,
        stream = %payload.stream,
        message_count = %payload.messages.len(),
        "Received POST /cc/v1/messages request"
    );

    // 检查 KiroProvider 是否可用
    let provider = match &state.kiro_provider {
        Some(p) => p.clone(),
        None => {
            tracing::error!("KiroProvider 未配置");
            return envelope::error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "service_unavailable",
                "Kiro API provider not configured",
            );
        }
    };
    let runtime_config = request_runtime_config(&state, &provider);
    let mut external_fallback = build_external_fallback_context(
        &state,
        &provider,
        &runtime_config,
        "/cc/v1/messages",
        raw_body,
        headers.clone(),
        &payload,
    );

    // 检测模型名是否包含 "thinking" 后缀，若包含则覆写 thinking 配置
    override_thinking_from_model_name(&mut payload);

    let caller_ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok());
    if let Err(message) = materialize_remote_multimodal_sources(&mut payload, caller_ua).await {
        tracing::warn!("多模态远程 source 处理失败: {}", message);
        return envelope::error_response(StatusCode::BAD_REQUEST, "invalid_request_error", message);
    }
    normalize_base64_image_media_types(&mut payload);

    if let Some(external) = external_fallback.as_mut() {
        external.refresh_payload(&payload);
    }

    let model_resolution =
        match resolve_request_model(&state, &runtime_config, "/cc/v1/messages", &payload) {
            Ok(resolution) => resolution,
            Err(response) => {
                if let Some(external_response) = maybe_forward_external_after_local_error(
                    external_fallback.as_ref(),
                    &envelope::request_id(),
                    &format!("模型不支持: {}", payload.model),
                    Vec::new(),
                )
                .await
                {
                    return external_response;
                }
                return response;
            }
        };
    if let Some(external) = external_fallback.as_mut() {
        external.model_resolution = Some(model_resolution.clone());
    }
    if let Some(external) = external_fallback.as_ref() {
        let request_id = envelope::request_id();
        if let Some(response) = external.direct_policy_response(&request_id).await {
            return response;
        }
    }

    // 检查是否为 WebSearch 请求
    if websearch::has_web_search_tool(&payload) {
        if !websearch_supported_for_profile(runtime_config.compat_profile) {
            return envelope::error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "web_search server-tool synthesis is disabled in anthropic-strict profile",
            );
        }
        tracing::info!("检测到 WebSearch 工具，路由到 WebSearch 处理");

        // 估算输入 tokens
        let input_tokens = token::count_all_tokens(
            payload.model.clone(),
            payload.system.clone(),
            payload.messages.clone(),
            payload.tools.clone(),
        ) as i32;

        return websearch::handle_websearch_request(provider, &payload, input_tokens).await;
    }

    // 转换请求
    let conversion_result = match convert_request_with_resolved_model(
        &payload,
        ConverterOptions {
            compat_profile: runtime_config.compat_profile,
            prompt_cache_simulation_mode: state.prompt_cache_simulation_mode,
        },
        &model_resolution,
    ) {
        Ok(result) => result,
        Err(e) => {
            tracing::warn!("请求转换失败: {}", e);
            return conversion_error_response(&e);
        }
    };

    // 构建 Kiro 请求（profile_arn 由 provider 层根据实际凭据注入）
    let mut kiro_request = KiroRequest {
        conversation_state: conversion_result.conversation_state,
        profile_arn: None,
    };
    let conversation_id = kiro_request.conversation_state.conversation_id.clone();

    let too_long_retry = PayloadTooLongRetryRequest::new(
        kiro_request.clone(),
        &runtime_config,
        "/cc/v1/messages",
        &payload.model,
        model_resolution.upstream_model.as_deref(),
        &conversation_id,
        should_expose_proxy_warnings(&runtime_config)
            .then(|| conversion_result.warnings.encode_header())
            .flatten(),
    );
    let (request_body, payload_guard_report) = match guard_kiro_request(
        &mut kiro_request,
        runtime_config.initial_payload_guard_config(),
    ) {
        Ok(result) => result,
        Err(err) => return payload_guard_error_response(err),
    };
    log_payload_guard_report(
        &payload_guard_report,
        "/cc/v1/messages",
        &payload.model,
        model_resolution.upstream_model.as_deref(),
        Some(&conversation_id),
    );
    let payload_breakdown = should_log_payload_byte_breakdown(&payload_guard_report)
        .then(|| breakdown_kiro_request(&kiro_request, &request_body));
    log_payload_byte_breakdown(
        payload_breakdown,
        &payload_guard_report,
        "/cc/v1/messages",
        &payload.model,
        model_resolution.upstream_model.as_deref(),
        Some(&conversation_id),
    );
    if model_resolution.is_remapped() {
        tracing::info!(
            endpoint = "/cc/v1/messages",
            requested_model = %model_resolution.requested_model,
            upstream_model = ?model_resolution.upstream_model,
            resolution = %model_resolution.source.as_str(),
            note = ?model_resolution.note,
            conversation_id = %conversation_id,
            "Kiro upstream model mapping applied to request payload"
        );
    };

    tracing::debug!(
        endpoint = "/cc/v1/messages",
        requested_model = %payload.model,
        upstream_model = ?model_resolution.upstream_model,
        conversation_id = %conversation_id,
        request_bytes = request_body.len(),
        history_entries = payload_guard_report.final_history_entries,
        current_tool_count = kiro_request.conversation_state.current_message.user_input_message.user_input_message_context.tools.len(),
        current_tool_result_count = kiro_request.conversation_state.current_message.user_input_message.user_input_message_context.tool_results.len(),
        current_image_count = kiro_request.conversation_state.current_message.user_input_message.images.len(),
        "Kiro request prepared"
    );
    tracing::trace!(
        endpoint = "/cc/v1/messages",
        requested_model = %payload.model,
        upstream_model = ?model_resolution.upstream_model,
        conversation_id = %conversation_id,
        request_body = %request_body,
        "Kiro request body"
    );

    // 估算输入 tokens
    let input_tokens = token::count_all_tokens(
        payload.model.clone(),
        payload.system.clone(),
        payload.messages.clone(),
        payload.tools.clone(),
    ) as i32;
    let usage_context = prepare_usage_context(
        &state,
        runtime_config.clone(),
        "/cc/v1/messages",
        payload.stream,
        &payload,
        Some(model_resolution.clone()),
        Some(conversation_id),
        prompt_cache_scope_conversation_id(state.prompt_cache_simulation_mode, &payload),
        input_tokens,
    )
    .with_payload_diagnostics(payload_breakdown, payload_guard_report.clone());

    // 检查是否启用了thinking
    let thinking_enabled = payload
        .thinking
        .as_ref()
        .map(|t| t.is_enabled())
        .unwrap_or(false);

    let tool_name_map = conversion_result.tool_name_map;
    let known_tool_names = conversion_result.known_tool_names;
    let warnings_header = if should_expose_proxy_warnings(&runtime_config) {
        merge_warning_headers(
            conversion_result.warnings.encode_header(),
            Some(&payload_guard_report),
        )
    } else {
        None
    };
    let extract_xml_thinking = runtime_config.compat_profile.allows_unsigned_thinking();

    if payload.stream {
        // 流式响应（实时模式）
        handle_stream_request(
            provider,
            &request_body,
            &payload.model,
            model_resolution
                .upstream_model
                .as_deref()
                .unwrap_or(&payload.model),
            input_tokens,
            usage_context.context_window_tokens,
            thinking_enabled,
            extract_xml_thinking,
            tool_name_map,
            known_tool_names,
            usage_context,
            warnings_header,
            too_long_retry,
            external_fallback,
        )
        .await
    } else {
        // 非流式响应：仅在配置开启时提取 thinking 块
        let extract_thinking = should_extract_unsigned_thinking(&runtime_config, thinking_enabled);
        handle_non_stream_request(
            provider,
            &request_body,
            &payload.model,
            model_resolution
                .upstream_model
                .as_deref()
                .unwrap_or(&payload.model),
            input_tokens,
            extract_thinking,
            tool_name_map,
            known_tool_names,
            usage_context,
            warnings_header,
            too_long_retry,
            external_fallback,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::cache::{self, CacheUsage};
    use crate::anthropic::pricing::PricingCatalog;
    use crate::anthropic::prompt_cache::PromptCacheTracker;
    use crate::anthropic::prompt_cache_creation_control::PromptCacheCreationController;
    use crate::anthropic::types::{Message, SystemMessage};
    use crate::anthropic::usage::{UsageRecordQuery, UsageRecorder};
    use crate::kiro::model::events::MetadataTokenUsage;
    use serde_json::json;

    fn messages_request_for_model(model: &str) -> MessagesRequest {
        MessagesRequest {
            model: model.to_string(),
            max_tokens: 16,
            messages: vec![Message {
                role: "user".to_string(),
                content: json!("hello"),
            }],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        }
    }

    #[tokio::test]
    async fn parse_messages_payload_rejects_empty_model_before_routing() {
        for model in ["", "   "] {
            let body = Bytes::from(
                json!({
                    "model": model,
                    "max_tokens": 16,
                    "messages": [{"role": "user", "content": "hello"}]
                })
                .to_string(),
            );

            let response = parse_messages_payload(&body).expect_err("empty model rejected");

            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("read error body");
            let value: serde_json::Value = serde_json::from_slice(&body).expect("json envelope");
            assert_eq!(value["error"]["type"], "invalid_request_error");
            assert_eq!(
                value["error"]["message"],
                "model: field is required and cannot be empty"
            );
            assert!(
                value["request_id"]
                    .as_str()
                    .is_some_and(|request_id| request_id.starts_with("req_01"))
            );
        }
    }

    #[test]
    fn normalize_base64_image_media_types_uses_detected_bytes() {
        let jpeg = BASE64_STANDARD.encode([0xff, 0xd8, 0xff, 0xdb, 0x00, 0x43, 0x00]);
        let mut payload = messages_request_for_model("claude-sonnet-4-5-20250929");
        payload.messages[0].content = json!([{
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": "image/png",
                "data": jpeg
            }
        }]);

        let fixed = normalize_base64_image_media_types(&mut payload);

        assert_eq!(fixed, 1);
        assert_eq!(
            payload.messages[0].content[0]["source"]["media_type"],
            "image/jpeg"
        );
    }

    fn runtime_config_for_payload_guard(
        mode: PayloadGuardMode,
        enabled: bool,
        max_bytes: usize,
    ) -> RequestRuntimeConfig {
        RequestRuntimeConfig {
            extract_thinking: true,
            prompt_cache_target_read_ratio: 0.98,
            prompt_cache_token_scale: 1.0,
            prompt_cache_max_simulated_input_tokens: 0,
            prompt_cache_cap_jitter_min_tokens: 0,
            prompt_cache_cap_jitter_max_tokens: 0,
            prompt_cache_scale_min_input_tokens: 0,
            prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
            reported_usage: ReportedUsageConfig::default(),
            compat_profile: CompatProfile::ClaudeCode,
            model_resolution_mode: ModelResolutionMode::Compatible,
            model_mapping: ModelMappingConfig::default(),
            expose_proxy_warnings: false,
            payload_guard_enabled: enabled,
            payload_guard_mode: mode,
            payload_guard_max_bytes: max_bytes,
            payload_guard_safety_margin_bytes: 0,
            payload_guard_trim_history: true,
            payload_guard_external_enabled: true,
            payload_shaping: PayloadShapingConfig::default(),
        }
    }

    #[test]
    fn on_too_long_initial_guard_repairs_without_size_trimming() {
        let runtime_config =
            runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);

        let initial = runtime_config.initial_payload_guard_config();

        assert!(initial.enabled);
        assert_eq!(initial.max_bytes, 0);
        assert!(!initial.trim_history);
        assert!(runtime_config.too_long_retry_enabled());
        assert_eq!(runtime_config.payload_guard_config().max_bytes, 460_800);
        assert!(runtime_config.payload_guard_config().trim_history);
    }

    #[test]
    fn payload_guard_safety_margin_reduces_effective_size_target() {
        let mut runtime_config =
            runtime_config_for_payload_guard(PayloadGuardMode::Preemptive, true, 460_800);
        runtime_config.payload_guard_safety_margin_bytes = 32 * 1024;

        assert_eq!(runtime_config.payload_guard_config().max_bytes, 428_032);

        runtime_config.payload_guard_max_bytes = 0;
        assert_eq!(runtime_config.payload_guard_config().max_bytes, 0);
    }

    #[test]
    fn on_too_long_retry_requires_enabled_guard_and_positive_limit() {
        assert!(
            !runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, false, 460_800)
                .too_long_retry_enabled()
        );
        assert!(
            !runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 0)
                .too_long_retry_enabled()
        );
        assert!(
            !runtime_config_for_payload_guard(PayloadGuardMode::Preemptive, true, 460_800)
                .too_long_retry_enabled()
        );
    }

    #[test]
    fn payload_guard_retry_treats_large_improper_request_as_possible_size_error() {
        assert!(should_retry_payload_guard_after_error(
            r#"400 Bad Request {"message":"Input is too long.","reason":"CONTENT_LENGTH_EXCEEDS_THRESHOLD"}"#,
            100,
            460_800,
        ));
        assert!(should_retry_payload_guard_after_error(
            r#"400 Bad Request {"message":"Improperly formed request.","reason":null}"#,
            700_000,
            460_800,
        ));
        assert!(!should_retry_payload_guard_after_error(
            r#"400 Bad Request {"message":"Improperly formed request.","reason":null}"#,
            100_000,
            460_800,
        ));
        assert!(!should_retry_payload_guard_after_error(
            r#"400 Bad Request {"message":"Improperly formed request.","reason":null}"#,
            700_000,
            0,
        ));
    }

    #[test]
    fn thinking_suffix_opus_4_7_uses_adaptive() {
        let mut payload = messages_request_for_model("claude-opus-4-7-thinking");

        override_thinking_from_model_name(&mut payload);

        let thinking = payload.thinking.expect("thinking should be set");
        assert_eq!(thinking.thinking_type, "adaptive");
        assert_eq!(thinking.budget_tokens, 20000);
        assert_eq!(
            payload
                .output_config
                .expect("output_config should be set")
                .effort,
            "xhigh"
        );
    }

    #[test]
    fn thinking_suffix_opus_alias_uses_opus_4_7_adaptive_defaults() {
        let mut payload = messages_request_for_model("opus-thinking");

        override_thinking_from_model_name(&mut payload);

        let thinking = payload.thinking.expect("thinking should be set");
        assert_eq!(thinking.thinking_type, "adaptive");
        assert_eq!(
            payload
                .output_config
                .expect("output_config should be set")
                .effort,
            "xhigh"
        );
    }

    #[test]
    fn thinking_suffix_sonnet_stays_enabled() {
        let mut payload = messages_request_for_model("claude-sonnet-4-6-thinking");

        override_thinking_from_model_name(&mut payload);

        let thinking = payload.thinking.expect("thinking should be set");
        assert_eq!(thinking.thinking_type, "enabled");
        assert!(payload.output_config.is_none());
    }

    #[test]
    fn path_reported_usage_policy_samples_natural_usage() {
        let reported_usage_config = ReportedUsageConfig::default();
        let usage = CacheUsage {
            total_input_tokens: 100_000,
            input_tokens: 50_000,
            output_tokens: 1,
            cache_creation_input_tokens: 50_000,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 50_000,
            cache_creation_1h_input_tokens: 0,
        };
        let values: Vec<i32> = (0..24)
            .map(|seed| {
                let policy = reported_cache_usage_policy(
                    "/cc/v1/messages",
                    PromptCacheSimulationMode::HighCache,
                    &reported_usage_config,
                    seed,
                )
                .expect("policy should apply");
                usage
                    .with_reported_cache_usage_policy(policy)
                    .cache_creation_input_tokens
            })
            .collect();

        assert!(values.iter().all(|value| (1..=3_600).contains(value)));
        assert!(values.windows(2).any(|pair| pair[1] < pair[0]));
        assert!(values.iter().any(|value| value % 10 != 0));

        let reported = usage.with_reported_cache_usage_policy(
            reported_cache_usage_policy(
                "/cc/v1/messages",
                PromptCacheSimulationMode::HighCache,
                &reported_usage_config,
                9,
            )
            .expect("policy should apply"),
        );
        assert!((1..=96).contains(&reported.input_tokens));
        assert_eq!(
            reported.cache_read_input_tokens,
            usage.input_tokens.saturating_sub(reported.input_tokens)
        );
        assert!(reported.cache_read_input_tokens < usage.input_tokens);
        assert_eq!(reported.output_tokens, 1);

        let raw_reported = usage.with_reported_cache_usage_policy_and_raw(
            reported_cache_usage_policy(
                "/cc/v1/messages",
                PromptCacheSimulationMode::HighCache,
                &reported_usage_config,
                9,
            )
            .expect("policy should apply"),
            cache::RawUsage::uncached(100_000, 1),
        );
        assert!((1..=96).contains(&raw_reported.input_tokens));
        assert_eq!(
            raw_reported.cache_read_input_tokens,
            usage
                .cache_read_input_tokens
                .saturating_add(100_000_i32.saturating_sub(raw_reported.input_tokens))
        );
    }

    #[test]
    fn reported_usage_rewrite_only_changes_local_prompt_cache_downstream_usage() {
        let reported_usage_config = ReportedUsageConfig::default();
        let v1_policy = reported_cache_usage_policy(
            "/v1/messages",
            PromptCacheSimulationMode::HighCache,
            &reported_usage_config,
            0,
        )
        .expect("default policy should apply");
        let unchanged_usage = CacheUsage {
            total_input_tokens: 100_000,
            input_tokens: 10_000,
            output_tokens: 1,
            cache_creation_input_tokens: 50_000,
            cache_read_input_tokens: 40_000,
            cache_creation_5m_input_tokens: 50_000,
            cache_creation_1h_input_tokens: 0,
        };
        let v1_reported = unchanged_usage.with_reported_cache_usage_policy_and_raw(
            v1_policy,
            cache::RawUsage::uncached(100_000, 1),
        );
        assert_eq!(v1_reported.input_tokens, 100_000);
        assert_eq!(v1_reported.output_tokens, 1);
        assert_eq!(
            v1_reported.cache_creation_input_tokens,
            unchanged_usage.cache_creation_input_tokens
        );
        assert_eq!(
            v1_reported.cache_read_input_tokens,
            unchanged_usage.cache_read_input_tokens
        );
        assert_eq!(
            reported_cache_usage_policy(
                "/cc/v1/messages",
                PromptCacheSimulationMode::Disabled,
                &reported_usage_config,
                0,
            ),
            None
        );

        let prompt_cache = Arc::new(PromptCacheTracker::default());
        let usage_recorder = Arc::new(UsageRecorder::new(10));
        let usage_context = RequestUsageContext {
            recorder: usage_recorder.clone(),
            prompt_cache,
            prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
            pricing_catalog: Arc::new(PricingCatalog::new()),
            request_id: "req_reported_limit".to_string(),
            endpoint: "/cc/v1/messages",
            stream: false,
            model: "claude-sonnet-4-6".to_string(),
            upstream_model: None,
            model_resolution_source: None,
            model_resolution_note: None,
            conversation_id: Some("session-limit".to_string()),
            prompt_cache_scope_conversation_id: Some("session-limit".to_string()),
            input_tokens: 100_000,
            context_window_tokens: 200_000,
            prompt_cache_profile: None,
            simulation_mode: PromptCacheSimulationMode::HighCache,
            prompt_cache_target_read_ratio: 0.95,
            prompt_cache_token_scale: 1.0,
            prompt_cache_max_simulated_input_tokens: 0,
            prompt_cache_cap_jitter_min_tokens: 0,
            prompt_cache_cap_jitter_max_tokens: 0,
            prompt_cache_scale_min_input_tokens: 0,
            prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
            reported_cache_usage_policy: reported_cache_usage_policy(
                "/cc/v1/messages",
                PromptCacheSimulationMode::HighCache,
                &reported_usage_config,
                7,
            ),
            simulated_usage: None,
            simulated_source: Some(UsageSource::LocalPromptCache),
            payload_breakdown: None,
            payload_guard_report: None,
            route_subtype_override: None,
            fallback_reason: None,
            local_preflight: None,
            external_attempts: Vec::new(),
            started_at: Instant::now(),
            first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        };
        let usage = CacheUsage {
            total_input_tokens: 100_000,
            input_tokens: 10_000,
            output_tokens: 1,
            cache_creation_input_tokens: 50_000,
            cache_read_input_tokens: 40_000,
            cache_creation_5m_input_tokens: 50_000,
            cache_creation_1h_input_tokens: 0,
        };

        let capped =
            usage_context.reported_usage_for_downstream(usage, UsageSource::LocalPromptCache);
        assert!((0..=3_300).contains(&capped.cache_creation_input_tokens));
        assert!((1..=96).contains(&capped.input_tokens));
        assert_eq!(
            capped.cache_read_input_tokens,
            usage.cache_read_input_tokens.saturating_add(
                usage_context
                    .input_tokens
                    .saturating_sub(capped.input_tokens)
            )
        );
        assert!(capped.cache_read_input_tokens > usage.cache_read_input_tokens);

        let upstream_metadata =
            usage_context.reported_usage_for_downstream(usage, UsageSource::UpstreamMetadata);
        assert_eq!(upstream_metadata.cache_creation_input_tokens, 50_000);
    }

    #[test]
    fn cc_local_prompt_cache_stream_reported_usage_caps_prod_like_input() {
        let reported_usage_config = ReportedUsageConfig::default();
        let request_input_tokens = 17_241;
        let usage_recorder = Arc::new(UsageRecorder::new(10));
        let usage_context = RequestUsageContext {
            recorder: usage_recorder.clone(),
            prompt_cache: Arc::new(PromptCacheTracker::default()),
            prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
            pricing_catalog: Arc::new(PricingCatalog::new()),
            request_id: "req_prod_like_cc_reported_usage".to_string(),
            endpoint: "/cc/v1/messages",
            stream: true,
            model: "claude-opus-4-6".to_string(),
            upstream_model: Some("claude-opus-4.6".to_string()),
            model_resolution_source: None,
            model_resolution_note: None,
            conversation_id: Some("conversation-prod-like".to_string()),
            prompt_cache_scope_conversation_id: Some("conversation-prod-like".to_string()),
            input_tokens: request_input_tokens,
            context_window_tokens: 200_000,
            prompt_cache_profile: None,
            simulation_mode: PromptCacheSimulationMode::HighCache,
            prompt_cache_target_read_ratio: 0.99,
            prompt_cache_token_scale: 2.0,
            prompt_cache_max_simulated_input_tokens: 300_000,
            prompt_cache_cap_jitter_min_tokens: 0,
            prompt_cache_cap_jitter_max_tokens: 0,
            prompt_cache_scale_min_input_tokens: 20_000,
            prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
            reported_cache_usage_policy: reported_cache_usage_policy(
                "/cc/v1/messages",
                PromptCacheSimulationMode::HighCache,
                &reported_usage_config,
                7,
            ),
            simulated_usage: Some(cache::CacheSimulation {
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 36_109,
                cache_creation_5m_input_tokens: 0,
                cache_creation_1h_input_tokens: 0,
                target_cache_ratio: Some(0.99),
                amplification: None,
            }),
            simulated_source: Some(UsageSource::LocalPromptCache),
            payload_breakdown: None,
            payload_guard_report: None,
            route_subtype_override: None,
            fallback_reason: None,
            local_preflight: None,
            external_attempts: Vec::new(),
            started_at: Instant::now(),
            first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        };
        let credential_usage =
            usage_context.attach_credential(Some(131), None, false, false, Vec::new());
        let prod_like_usage = CacheUsage {
            total_input_tokens: 57_499,
            input_tokens: 21_390,
            output_tokens: 6,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 36_109,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        };

        let reported =
            credential_usage.final_reported_usage_for_stream(prod_like_usage, None, true);

        assert!((1..=96).contains(&reported.input_tokens));
        assert_eq!(reported.output_tokens, 6);
        assert_eq!(reported.cache_creation_input_tokens, 0);
        assert_eq!(
            reported.cache_read_input_tokens,
            prod_like_usage
                .cache_read_input_tokens
                .saturating_add(request_input_tokens.saturating_sub(reported.input_tokens))
        );
        let raw_usage = raw_usage_from_metadata_or_estimate(
            None,
            request_input_tokens,
            prod_like_usage.output_tokens,
        );

        credential_usage.record_success_reported(
            reported,
            UsageSource::LocalPromptCache,
            Some(raw_usage),
        );
        let records = usage_recorder.query(UsageRecordQuery::default());
        assert_eq!(records.total, 1);
        let record = records.records.first().expect("usage record should exist");
        assert_eq!(record.compat_input_tokens, reported.input_tokens);
        assert_eq!(record.output_tokens, 6);
        assert_eq!(
            record.cache_creation_input_tokens,
            reported.cache_creation_input_tokens
        );
        assert_eq!(
            record.cache_read_input_tokens,
            reported.cache_read_input_tokens
        );
        let raw_usage = record.raw_usage.expect("raw usage should be retained");
        assert_eq!(raw_usage.total_input_tokens, request_input_tokens);
        assert_eq!(raw_usage.input_tokens, request_input_tokens);
        assert_eq!(raw_usage.output_tokens, prod_like_usage.output_tokens);
        assert_eq!(raw_usage.cache_creation_input_tokens, 0);
        assert_eq!(raw_usage.cache_read_input_tokens, 0);
    }

    #[test]
    fn first_token_detection_ignores_initial_empty_blocks() {
        assert!(!is_first_token_output_event(&SseEvent::new(
            "message_start",
            json!({"type": "message_start"})
        )));
        assert!(!is_first_token_output_event(&SseEvent::new(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {"type": "text", "text": ""}
            })
        )));
        assert!(is_first_token_output_event(&SseEvent::new(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "text_delta", "text": "hello"}
            })
        )));
        assert!(is_first_token_output_event(&SseEvent::new(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": 1,
                "content_block": {"type": "tool_use", "id": "toolu_1", "name": "read", "input": {}}
            })
        )));
    }

    #[test]
    fn path_overrides_independently_control_reported_usage_fields() {
        let reported_usage_config = ReportedUsageConfig::default();
        let usage = CacheUsage {
            total_input_tokens: 100_000,
            input_tokens: 10_000,
            output_tokens: 1,
            cache_creation_input_tokens: 50_000,
            cache_read_input_tokens: 40_000,
            cache_creation_5m_input_tokens: 50_000,
            cache_creation_1h_input_tokens: 0,
        };
        let prompt_cache = Arc::new(PromptCacheTracker::default());
        let usage_recorder = Arc::new(UsageRecorder::new(10));

        let v1_context = RequestUsageContext {
            recorder: usage_recorder.clone(),
            prompt_cache: prompt_cache.clone(),
            prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
            pricing_catalog: Arc::new(PricingCatalog::new()),
            request_id: "req_v1_policy".to_string(),
            endpoint: "/v1/messages",
            stream: false,
            model: "claude-sonnet-4-6".to_string(),
            upstream_model: None,
            model_resolution_source: None,
            model_resolution_note: None,
            conversation_id: Some("session-policy".to_string()),
            prompt_cache_scope_conversation_id: Some("session-policy".to_string()),
            input_tokens: 100_000,
            context_window_tokens: 200_000,
            prompt_cache_profile: None,
            simulation_mode: PromptCacheSimulationMode::HighCache,
            prompt_cache_target_read_ratio: 0.95,
            prompt_cache_token_scale: 1.0,
            prompt_cache_max_simulated_input_tokens: 0,
            prompt_cache_cap_jitter_min_tokens: 0,
            prompt_cache_cap_jitter_max_tokens: 0,
            prompt_cache_scale_min_input_tokens: 0,
            prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
            reported_cache_usage_policy: reported_cache_usage_policy(
                "/v1/messages",
                PromptCacheSimulationMode::HighCache,
                &reported_usage_config,
                7,
            ),
            simulated_usage: None,
            simulated_source: Some(UsageSource::LocalPromptCache),
            payload_breakdown: None,
            payload_guard_report: None,
            route_subtype_override: None,
            fallback_reason: None,
            local_preflight: None,
            external_attempts: Vec::new(),
            started_at: Instant::now(),
            first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        };
        let cc_context = RequestUsageContext {
            endpoint: "/cc/v1/messages",
            request_id: "req_cc_policy".to_string(),
            reported_cache_usage_policy: reported_cache_usage_policy(
                "/cc/v1/messages",
                PromptCacheSimulationMode::HighCache,
                &reported_usage_config,
                7,
            ),
            ..v1_context.clone()
        };
        let ha_context = RequestUsageContext {
            endpoint: "/ha/v1/messages",
            request_id: "req_ha_policy".to_string(),
            reported_cache_usage_policy: reported_cache_usage_policy(
                "/ha/v1/messages",
                PromptCacheSimulationMode::HighCache,
                &reported_usage_config,
                7,
            ),
            ..v1_context.clone()
        };
        let na_context = RequestUsageContext {
            endpoint: "/na/v1/messages",
            request_id: "req_na_policy".to_string(),
            reported_cache_usage_policy: reported_cache_usage_policy(
                "/na/v1/messages",
                PromptCacheSimulationMode::HighCache,
                &reported_usage_config,
                7,
            ),
            ..v1_context.clone()
        };

        assert!(v1_context.reported_cache_usage_policy().is_some());
        assert!(cc_context.reported_cache_usage_policy().is_some());
        assert!(ha_context.reported_cache_usage_policy().is_some());
        assert!(na_context.reported_cache_usage_policy().is_some());

        let v1_reported =
            v1_context.reported_usage_for_downstream(usage, UsageSource::LocalPromptCache);
        assert_eq!(v1_reported.input_tokens, v1_context.input_tokens);
        assert_eq!(v1_reported.output_tokens, usage.output_tokens);
        assert_eq!(
            v1_reported.cache_creation_input_tokens,
            usage.cache_creation_input_tokens
        );
        assert_eq!(
            v1_reported.cache_read_input_tokens,
            usage.cache_read_input_tokens
        );

        let cc_reported =
            cc_context.reported_usage_for_downstream(usage, UsageSource::LocalPromptCache);
        assert!((1..=96).contains(&cc_reported.input_tokens));
        assert!((0..=3_300).contains(&cc_reported.cache_creation_input_tokens));
        assert_eq!(
            cc_reported.cache_read_input_tokens,
            usage.cache_read_input_tokens.saturating_add(
                cc_context
                    .input_tokens
                    .saturating_sub(cc_reported.input_tokens)
            )
        );
        assert_eq!(cc_reported.output_tokens, usage.output_tokens);

        let ha_reported =
            ha_context.reported_usage_for_downstream(usage, UsageSource::LocalPromptCache);
        assert!((1..=96).contains(&ha_reported.input_tokens));
        assert_eq!(
            ha_reported.cache_creation_input_tokens,
            usage.cache_creation_input_tokens
        );
        assert_eq!(
            ha_reported.cache_creation_5m_input_tokens,
            usage.cache_creation_5m_input_tokens
        );
        assert_eq!(
            ha_reported.cache_creation_1h_input_tokens,
            usage.cache_creation_1h_input_tokens
        );
        assert_eq!(
            ha_reported.cache_read_input_tokens,
            usage.cache_read_input_tokens.saturating_add(
                ha_context
                    .input_tokens
                    .saturating_sub(ha_reported.input_tokens)
            )
        );
        assert_eq!(ha_reported.output_tokens, usage.output_tokens);

        let na_reported =
            na_context.reported_usage_for_downstream(usage, UsageSource::LocalPromptCache);
        assert_eq!(na_reported.total_input_tokens, na_context.input_tokens);
        assert_eq!(na_reported.input_tokens, na_context.input_tokens);
        assert_eq!(na_reported.cache_creation_input_tokens, 0);
        assert_eq!(na_reported.cache_read_input_tokens, 0);
        assert_eq!(na_reported.cache_creation_5m_input_tokens, 0);
        assert_eq!(na_reported.cache_creation_1h_input_tokens, 0);
        assert_eq!(na_reported.output_tokens, usage.output_tokens);
    }

    #[test]
    fn provider_error_hint_extracts_credential_for_failure_records() {
        let hint = extract_credential_error_hint(
            "非流式 API 请求失败（凭据 #2 IlmiMiazzi@gmail.com）: 429 Too Many Requests",
        )
        .expect("credential hint");
        assert_eq!(hint.id, 2);
        assert_eq!(hint.label.as_deref(), Some("IlmiMiazzi@gmail.com"));
        assert_eq!(hint.display_label(), "#2 IlmiMiazzi@gmail.com");

        let prompt_cache = Arc::new(PromptCacheTracker::default());
        let usage_recorder = Arc::new(UsageRecorder::new(10));
        let usage_context = RequestUsageContext {
            recorder: usage_recorder.clone(),
            prompt_cache,
            prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
            pricing_catalog: Arc::new(PricingCatalog::new()),
            request_id: "req_error_hint".to_string(),
            endpoint: "/v1/messages",
            stream: false,
            model: "claude-sonnet-4-6".to_string(),
            upstream_model: None,
            model_resolution_source: None,
            model_resolution_note: None,
            conversation_id: Some("session-error".to_string()),
            prompt_cache_scope_conversation_id: Some("session-error".to_string()),
            input_tokens: 4096,
            context_window_tokens: 200_000,
            prompt_cache_profile: None,
            simulation_mode: PromptCacheSimulationMode::HighCache,
            prompt_cache_target_read_ratio: 0.95,
            prompt_cache_token_scale: 1.0,
            prompt_cache_max_simulated_input_tokens: 0,
            prompt_cache_cap_jitter_min_tokens: 0,
            prompt_cache_cap_jitter_max_tokens: 0,
            prompt_cache_scale_min_input_tokens: 0,
            prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
            reported_cache_usage_policy: None,
            simulated_usage: None,
            simulated_source: None,
            payload_breakdown: None,
            payload_guard_report: None,
            route_subtype_override: None,
            fallback_reason: None,
            local_preflight: None,
            external_attempts: Vec::new(),
            started_at: Instant::now(),
            first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        };
        usage_context
            .attach_credential(Some(hint.id), hint.label, false, false, Vec::new())
            .record_failure(UsageRecordStatus::Error, "api_error", "upstream failed");

        let records = usage_recorder.query(Default::default()).records;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].credential_id, Some(2));
        assert_eq!(
            records[0].credential_label.as_deref(),
            Some("IlmiMiazzi@gmail.com")
        );
    }

    #[tokio::test]
    async fn content_length_threshold_error_is_not_reported_as_context_window_full() {
        let response = map_provider_error(
            anyhow::anyhow!(
                "{}",
                r#"流式 API 请求失败（凭据 #1 test@example.com）: 400 Bad Request {"message":"Input is too long.","reason":"CONTENT_LENGTH_EXCEEDS_THRESHOLD"}"#
            ),
            Some("req_test_content_length"),
            None,
        );

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");
        let message = value
            .pointer("/error/message")
            .and_then(|v| v.as_str())
            .expect("error message");

        assert!(message.contains("input content length exceeded"));
        assert!(message.contains("separate from the model context window"));
        assert!(!message.contains("Context window is full"));
    }

    #[tokio::test]
    async fn malformed_upstream_error_uses_generic_user_message() {
        let response = map_provider_error(
            anyhow::anyhow!(
                "{}",
                r#"流式 API 请求失败（凭据 #1 test@example.com，请求无效）: 400 Bad Request {"message":"Improperly formed request.","reason":null}"#
            ),
            Some("req_test_malformed"),
            None,
        );

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");
        let message = value
            .pointer("/error/message")
            .and_then(|v| v.as_str())
            .expect("error message");

        assert_eq!(message, UPSTREAM_INVALID_REQUEST_MESSAGE);
        assert!(!message.contains("tool_use"));
        assert!(!message.contains("转换"));
    }

    #[tokio::test]
    async fn opaque_400_bad_request_maps_to_invalid_request_not_gateway() {
        let response = map_provider_error(
            anyhow::anyhow!(
                "{}",
                "流式 API 请求失败（凭据 #6 ***，请求无效）: 400 Bad Request <failed to read response body: error decoding response body>"
            ),
            Some("req_test_opaque_bad_request"),
            None,
        );

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(
            value.pointer("/error/type").and_then(|v| v.as_str()),
            Some("invalid_request_error")
        );
        assert_eq!(
            value.pointer("/error/message").and_then(|v| v.as_str()),
            Some(UPSTREAM_INVALID_REQUEST_MESSAGE)
        );
    }

    #[test]
    fn local_prompt_cache_updates_even_when_context_tokens_are_estimated() {
        let prompt_cache = Arc::new(PromptCacheTracker::default());
        let usage_recorder = Arc::new(UsageRecorder::new(10));
        let payload = MessagesRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 16,
            messages: vec![Message {
                role: "user".to_string(),
                content: json!([
                    {
                        "type": "text",
                        "text": "cacheable prompt block ".repeat(700),
                        "cache_control": {"type": "ephemeral"}
                    }
                ]),
            }],
            stream: true,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };
        let profile = prompt_cache.build_profile(&payload, 4096);
        let usage_context = RequestUsageContext {
            recorder: usage_recorder,
            prompt_cache: prompt_cache.clone(),
            prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
            pricing_catalog: Arc::new(PricingCatalog::new()),
            request_id: "req_test".to_string(),
            endpoint: "/v1/messages",
            stream: true,
            model: payload.model.clone(),
            upstream_model: None,
            model_resolution_source: None,
            model_resolution_note: None,
            conversation_id: Some("session-a".to_string()),
            prompt_cache_scope_conversation_id: Some("session-a".to_string()),
            input_tokens: 4096,
            context_window_tokens: 200_000,
            prompt_cache_profile: profile.clone(),
            simulation_mode: PromptCacheSimulationMode::HighCache,
            prompt_cache_target_read_ratio: 0.85,
            prompt_cache_token_scale: 1.0,
            prompt_cache_max_simulated_input_tokens: 0,
            prompt_cache_cap_jitter_min_tokens: 0,
            prompt_cache_cap_jitter_max_tokens: 0,
            prompt_cache_scale_min_input_tokens: 0,
            prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
            reported_cache_usage_policy: None,
            simulated_usage: None,
            simulated_source: Some(UsageSource::LocalPromptCache),
            payload_breakdown: None,
            payload_guard_report: None,
            route_subtype_override: None,
            fallback_reason: None,
            local_preflight: None,
            external_attempts: Vec::new(),
            started_at: Instant::now(),
            first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        }
        .attach_credential(Some(1), None, false, false, Vec::new());
        let usage = CacheUsage {
            total_input_tokens: 4096,
            input_tokens: 128,
            output_tokens: 1,
            cache_creation_input_tokens: 3968,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 3968,
            cache_creation_1h_input_tokens: 0,
        };

        usage_context.record_success(usage, UsageSource::LocalPromptCache, true);

        let scope = PromptCacheScope {
            credential_id: 1,
            conversation_id: "session-a".to_string(),
            model: payload.model,
        };
        let second = prompt_cache.compute(Some(scope), profile.as_ref(), 0.85);
        assert!(second.cache_read_input_tokens > 0);
    }

    #[test]
    fn high_cache_zero_metadata_fallback_updates_local_prompt_cache() {
        let prompt_cache = Arc::new(PromptCacheTracker::default());
        let usage_recorder = Arc::new(UsageRecorder::new(10));
        let payload = MessagesRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 16,
            messages: vec![Message {
                role: "user".to_string(),
                content: json!("hello"),
            }],
            stream: true,
            system: Some(vec![SystemMessage {
                text: "cacheable prompt block ".repeat(700),
                cache_control: None,
            }]),
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };
        let profile = prompt_cache.build_high_cache_profile(&payload, 4096);
        let usage_context = RequestUsageContext {
            recorder: usage_recorder,
            prompt_cache: prompt_cache.clone(),
            prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
            pricing_catalog: Arc::new(PricingCatalog::new()),
            request_id: "req_high_cache".to_string(),
            endpoint: "/v1/messages",
            stream: true,
            model: payload.model.clone(),
            upstream_model: None,
            model_resolution_source: None,
            model_resolution_note: None,
            conversation_id: Some("session-high-cache".to_string()),
            prompt_cache_scope_conversation_id: Some("session-high-cache".to_string()),
            input_tokens: 4096,
            context_window_tokens: 200_000,
            prompt_cache_profile: profile.clone(),
            simulation_mode: PromptCacheSimulationMode::HighCache,
            prompt_cache_target_read_ratio: 0.95,
            prompt_cache_token_scale: 1.0,
            prompt_cache_max_simulated_input_tokens: 0,
            prompt_cache_cap_jitter_min_tokens: 0,
            prompt_cache_cap_jitter_max_tokens: 0,
            prompt_cache_scale_min_input_tokens: 0,
            prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
            reported_cache_usage_policy: None,
            simulated_usage: Some(cache::CacheSimulation {
                cache_creation_input_tokens: 3968,
                cache_read_input_tokens: 0,
                cache_creation_5m_input_tokens: 3968,
                cache_creation_1h_input_tokens: 0,
                target_cache_ratio: Some(0.95),
                amplification: None,
            }),
            simulated_source: Some(UsageSource::LocalPromptCache),
            payload_breakdown: None,
            payload_guard_report: None,
            route_subtype_override: None,
            fallback_reason: None,
            local_preflight: None,
            external_attempts: Vec::new(),
            started_at: Instant::now(),
            first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        }
        .attach_credential(Some(1), None, false, false, Vec::new());
        let metadata = MetadataTokenUsage {
            uncached_input_tokens: 4096,
            output_tokens: 1,
            total_tokens: 4097,
            cache_read_input_tokens: 0,
            cache_write_input_tokens: 0,
        };
        let usage = cache::build_usage_with_simulation_policy(
            Some(&metadata),
            4096,
            1,
            usage_context.request.simulated_usage,
            true,
        );

        let source = usage_context.usage_source(&usage, Some(&metadata), false);
        assert_eq!(source, UsageSource::LocalPromptCache);
        usage_context.record_success(usage, source, false);

        let scope = PromptCacheScope {
            credential_id: 1,
            conversation_id: "session-high-cache".to_string(),
            model: payload.model,
        };
        let second = prompt_cache.compute(Some(scope), profile.as_ref(), 0.95);
        assert!(second.cache_read_input_tokens > 0);
    }

    #[test]
    fn high_cache_missing_metadata_fallback_conversation_reads_second_turn() {
        let prompt_cache = Arc::new(PromptCacheTracker::default());
        let usage_recorder = Arc::new(UsageRecorder::new(10));
        let state = AppState::new(
            Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
            true,
            usage_recorder,
            prompt_cache,
            Arc::new(PromptCacheCreationController::default()),
            PromptCacheSimulationMode::HighCache,
            0.95,
            CompatProfile::ClaudeCode,
            false,
        );
        let first_payload = MessagesRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 16,
            messages: vec![Message {
                role: "user".to_string(),
                content: json!("start high cache session"),
            }],
            stream: false,
            system: Some(vec![SystemMessage {
                text: "stable high cache system prompt ".repeat(700),
                cache_control: None,
            }]),
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };
        let first_conversation_id =
            extract_stable_conversation_id(&first_payload).expect("fallback id");
        let first_context = prepare_usage_context(
            &state,
            RequestRuntimeConfig::from_app_state(&state),
            "/v1/messages",
            false,
            &first_payload,
            None,
            Some(first_conversation_id.clone()),
            Some(first_conversation_id.clone()),
            4096,
        );
        let first_usage = attach_test_credential_usage(first_context, 1);
        let first_usage_body = cache::build_usage_with_simulation_policy(
            None,
            4096,
            1,
            first_usage.request.simulated_usage,
            true,
        );
        assert!(first_usage_body.cache_creation_input_tokens > 0);
        assert_eq!(first_usage_body.cache_read_input_tokens, 0);
        let first_source = first_usage.usage_source(&first_usage_body, None, false);
        assert_eq!(first_source, UsageSource::LocalPromptCache);
        first_usage.record_success(first_usage_body, first_source, false);

        let second_payload = MessagesRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 16,
            messages: vec![
                Message {
                    role: "user".to_string(),
                    content: json!("start high cache session"),
                },
                Message {
                    role: "assistant".to_string(),
                    content: json!("ready"),
                },
                Message {
                    role: "user".to_string(),
                    content: json!("continue the same session"),
                },
            ],
            stream: false,
            system: Some(vec![SystemMessage {
                text: "stable high cache system prompt ".repeat(700),
                cache_control: None,
            }]),
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };
        let second_conversation_id =
            extract_stable_conversation_id(&second_payload).expect("fallback id");
        assert_eq!(first_conversation_id, second_conversation_id);

        let second_context = prepare_usage_context(
            &state,
            RequestRuntimeConfig::from_app_state(&state),
            "/v1/messages",
            false,
            &second_payload,
            None,
            Some(second_conversation_id.clone()),
            Some(second_conversation_id),
            8192,
        );
        let second_usage = attach_test_credential_usage(second_context, 1);
        let second_usage_body = cache::build_usage_with_simulation_policy(
            None,
            8192,
            1,
            second_usage.request.simulated_usage,
            true,
        );

        assert!(second_usage_body.cache_read_input_tokens > 0);
        assert_eq!(
            second_usage.usage_source(&second_usage_body, None, false),
            UsageSource::LocalPromptCache
        );
    }

    #[test]
    fn disabled_prompt_cache_does_not_simulate_without_stable_conversation_id() {
        let prompt_cache = Arc::new(PromptCacheTracker::default());
        let usage_recorder = Arc::new(UsageRecorder::new(10));
        let state = AppState::new(
            Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
            true,
            usage_recorder,
            prompt_cache.clone(),
            Arc::new(PromptCacheCreationController::default()),
            PromptCacheSimulationMode::Disabled,
            0.95,
            CompatProfile::ClaudeCode,
            false,
        );
        let payload = MessagesRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 16,
            messages: vec![Message {
                role: "user".to_string(),
                content: json!([
                    {
                        "type": "text",
                        "text": "cacheable prompt block ".repeat(700),
                        "cache_control": {"type": "ephemeral"}
                    }
                ]),
            }],
            stream: true,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };
        let (simulation, source) = build_simulated_usage(&state, None, None);

        assert!(simulation.is_none());
        assert!(source.is_none());

        let context = prepare_usage_context(
            &state,
            RequestRuntimeConfig::from_app_state(&state),
            "/v1/messages",
            true,
            &payload,
            None,
            Some("random-conversation".to_string()),
            prompt_cache_scope_conversation_id(state.prompt_cache_simulation_mode, &payload),
            4096,
        );
        assert!(context.prompt_cache_profile.is_none());
        assert!(context.prompt_cache_scope_conversation_id.is_none());

        let credential_usage = attach_test_credential_usage(context, 1);
        assert!(credential_usage.request.simulated_usage.is_none());
        assert!(credential_usage.request.simulated_source.is_none());
    }

    #[test]
    fn disabled_prompt_cache_mode_does_not_build_local_profile_even_for_na_path() {
        let prompt_cache = Arc::new(PromptCacheTracker::default());
        let usage_recorder = Arc::new(UsageRecorder::new(10));
        let state = AppState::new(
            Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
            true,
            usage_recorder,
            prompt_cache,
            Arc::new(PromptCacheCreationController::default()),
            PromptCacheSimulationMode::Disabled,
            0.95,
            CompatProfile::ClaudeCode,
            false,
        );
        let payload = MessagesRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 16,
            messages: vec![Message {
                role: "user".to_string(),
                content: json!([
                    {
                        "type": "text",
                        "text": "cacheable prompt block ".repeat(700),
                        "cache_control": {"type": "ephemeral"}
                    }
                ]),
            }],
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let context = prepare_usage_context(
            &state,
            RequestRuntimeConfig::from_app_state(&state),
            "/na/v1/messages",
            false,
            &payload,
            None,
            Some("conversation-id".to_string()),
            prompt_cache_scope_conversation_id(state.prompt_cache_simulation_mode, &payload),
            4096,
        );

        assert_eq!(context.simulation_mode, PromptCacheSimulationMode::Disabled);
        assert!(context.prompt_cache_profile.is_none());
        assert!(context.prompt_cache_scope_conversation_id.is_none());
        assert!(context.simulated_usage.is_none());
        assert!(context.simulated_source.is_none());
        assert!(context.reported_cache_usage_policy.is_none());
    }

    fn attach_test_credential_usage(
        mut usage_context: RequestUsageContext,
        credential_id: u64,
    ) -> CredentialUsageContext {
        let scope = usage_context
            .prompt_cache_scope_conversation_id
            .as_ref()
            .map(|conversation_id| PromptCacheScope {
                credential_id,
                conversation_id: conversation_id.clone(),
                model: usage_context.model.clone(),
            });
        let prompt_usage = usage_context.prompt_cache.compute(
            scope,
            usage_context.prompt_cache_profile.as_ref(),
            usage_context.prompt_cache_target_read_ratio,
        );
        usage_context.simulated_usage =
            cache::CacheSimulation::from_prompt_cache_with_ratio_and_amplification(
                prompt_usage,
                usage_context.prompt_cache_target_read_ratio,
                usage_context.cache_amplification(),
            );
        usage_context.simulated_source = usage_context
            .simulated_usage
            .map(|_| UsageSource::LocalPromptCache);
        usage_context.attach_credential(Some(credential_id), None, false, false, Vec::new())
    }

    #[test]
    fn strict_profile_suppresses_proxy_warning_header() {
        let prompt_cache = Arc::new(PromptCacheTracker::default());
        let usage_recorder = Arc::new(UsageRecorder::new(10));
        let state = AppState::new(
            Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
            true,
            usage_recorder,
            prompt_cache,
            Arc::new(PromptCacheCreationController::default()),
            PromptCacheSimulationMode::Disabled,
            0.85,
            CompatProfile::AnthropicStrict,
            true,
        );

        assert!(!should_expose_proxy_warnings(
            &RequestRuntimeConfig::from_app_state(&state)
        ));
    }

    #[test]
    fn external_fallback_classifier_rejects_request_errors() {
        let config = ExternalPoolsConfig::default();

        assert_eq!(
            classify_local_error_for_external_fallback(
                r#"400 Bad Request {"message":"Input is too long.","reason":"CONTENT_LENGTH_EXCEEDS_THRESHOLD"}"#,
                &[],
                &config,
            ),
            None
        );
        assert_eq!(
            classify_local_error_for_external_fallback(
                "JSON schema is invalid for tool input_schema",
                &[],
                &config,
            ),
            None
        );

        let attempts = vec![KiroCredentialAttempt::new(
            0,
            1,
            None,
            Some(StatusCode::BAD_REQUEST),
            "fail",
            Some("client_error"),
            Some("bad request"),
            10,
        )];
        assert_eq!(
            classify_local_error_for_external_fallback("429 Too Many Requests", &attempts, &config),
            None
        );
    }

    #[test]
    fn external_fallback_classifier_allows_capacity_and_transient_errors() {
        let config = ExternalPoolsConfig::default();

        assert_eq!(
            classify_local_error_for_external_fallback(
                "本地凭据调度容量暂不可用，并发槽位已满",
                &[],
                &config,
            )
            .as_deref(),
            Some("local_capacity_exhausted")
        );
        assert_eq!(
            classify_local_error_for_external_fallback("429 Too Many Requests", &[], &config)
                .as_deref(),
            Some("local_transient_exhausted")
        );
    }

    #[test]
    fn external_fallback_classifier_respects_scheduler_fallback_toggles() {
        let mut config = ExternalPoolsConfig::default();

        config.fallback_on_local_capacity_exhausted = false;
        assert_eq!(
            classify_local_error_for_external_fallback(
                "本地凭据调度容量暂不可用，并发槽位已满",
                &[],
                &config,
            ),
            None
        );

        config = ExternalPoolsConfig::default();
        config.fallback_on_local_transient_exhausted = false;
        assert_eq!(
            classify_local_error_for_external_fallback("429 Too Many Requests", &[], &config),
            None
        );
        assert_eq!(
            classify_local_error_for_external_fallback(
                "upstream server_error",
                &[KiroCredentialAttempt::new(
                    0,
                    1,
                    None,
                    Some(StatusCode::BAD_GATEWAY),
                    "retry",
                    Some("server_error"),
                    Some("502"),
                    10,
                )],
                &config,
            ),
            None
        );

        config = ExternalPoolsConfig::default();
        config.fallback_on_no_available_credentials = false;
        assert_eq!(
            classify_local_error_for_external_fallback("所有凭据均已禁用（0/2）", &[], &config),
            None
        );

        config.fallback_on_no_available_credentials = true;
        assert_eq!(
            classify_local_error_for_external_fallback("所有凭据均已禁用（0/2）", &[], &config)
                .as_deref(),
            Some("no_available_credentials")
        );
    }

    #[test]
    fn local_pool_preflight_reason_respects_scheduler_fallback_toggles() {
        let mut config = ExternalPoolsConfig::default();

        assert!(local_pool_capacity_fail_fast_enabled(&config));
        assert_eq!(
            local_pool_route_fallback_reason(LocalPoolRouteStateKind::NoCredentials, &config),
            Some("local_no_credentials")
        );
        assert_eq!(
            local_pool_route_fallback_reason(LocalPoolRouteStateKind::AllDisabled, &config),
            Some("local_all_disabled")
        );
        assert_eq!(
            local_pool_route_fallback_reason(LocalPoolRouteStateKind::ProxyBlocked, &config),
            Some("local_proxy_blocked")
        );
        assert_eq!(
            local_pool_route_fallback_reason(LocalPoolRouteStateKind::AllCoolingDown, &config),
            Some("local_all_cooling_down")
        );
        assert_eq!(
            local_pool_route_fallback_reason(LocalPoolRouteStateKind::CapacityFull, &config),
            Some("local_capacity_full")
        );
        assert_eq!(
            local_pool_route_fallback_reason(LocalPoolRouteStateKind::NoModelCompatible, &config),
            None
        );

        config.fallback_on_no_available_credentials = false;
        assert_eq!(
            local_pool_route_fallback_reason(LocalPoolRouteStateKind::NoCredentials, &config),
            None
        );
        assert_eq!(
            local_pool_route_fallback_reason(LocalPoolRouteStateKind::AllDisabled, &config),
            None
        );
        assert_eq!(
            local_pool_route_fallback_reason(LocalPoolRouteStateKind::ProxyBlocked, &config),
            None
        );

        config = ExternalPoolsConfig::default();
        config.fallback_on_local_transient_exhausted = false;
        assert_eq!(
            local_pool_route_fallback_reason(LocalPoolRouteStateKind::AllCoolingDown, &config),
            None
        );

        config = ExternalPoolsConfig::default();
        config.fallback_on_local_capacity_exhausted = false;
        assert!(!local_pool_capacity_fail_fast_enabled(&config));
        assert_eq!(
            local_pool_route_fallback_reason(LocalPoolRouteStateKind::CapacityFull, &config),
            None
        );

        config = ExternalPoolsConfig::default();
        config.fallback_on_unsupported_model = true;
        assert_eq!(
            local_pool_route_fallback_reason(LocalPoolRouteStateKind::NoModelCompatible, &config),
            Some("local_no_model_compatible")
        );

        config.local_pool_preflight_enabled = false;
        assert!(!local_pool_capacity_fail_fast_enabled(&config));
    }

    #[test]
    fn external_fallback_classifier_gates_unsupported_model() {
        let mut config = ExternalPoolsConfig::default();
        config.fallback_on_unsupported_model = false;
        assert_eq!(
            classify_local_error_for_external_fallback("模型不支持: claude-future", &[], &config,),
            None
        );

        config.fallback_on_unsupported_model = true;
        assert_eq!(
            classify_local_error_for_external_fallback("模型不支持: claude-future", &[], &config,)
                .as_deref(),
            Some("unsupported_model")
        );
        assert_eq!(
            classify_local_error_for_external_fallback(
                r#"非流式 API 请求失败: 400 Bad Request {"message":"Invalid model. Please select a different model to continue.","reason":"INVALID_MODEL_ID"}"#,
                &[KiroCredentialAttempt::new(
                    0,
                    1,
                    None,
                    Some(StatusCode::BAD_REQUEST),
                    "fail",
                    Some("client_error"),
                    Some("bad request"),
                    10,
                )],
                &config,
            )
            .as_deref(),
            Some("unsupported_model")
        );
    }

    #[test]
    fn external_local_rescue_classifier_respects_error_type_and_toggles() {
        let config = ExternalPoolsConfig::default();
        let rate_limit = ExternalPoolFinalError {
            status: StatusCode::TOO_MANY_REQUESTS,
            response_error_type: "rate_limit_error".to_string(),
            route_error_type: "rate_limit".to_string(),
            message:
                r#"{"message":"Too many requests, please wait before trying again.","reason":"SERVICE_REQUEST_RATE_EXCEEDED"}"#
                    .to_string(),
            retryable: true,
            attempts: Vec::new(),
            pool_id: Some(1),
            pool_name: Some("backup".to_string()),
        };
        assert_eq!(
            local_rescue_reason_after_external_error(&config, &rate_limit, None),
            Some("external_rate_limit")
        );
        assert_eq!(
            local_rescue_reason_after_external_error(
                &config,
                &rate_limit,
                Some("no_available_credentials")
            ),
            None
        );

        let timeout = ExternalPoolFinalError {
            status: StatusCode::BAD_GATEWAY,
            response_error_type: "api_error".to_string(),
            route_error_type: "network_error".to_string(),
            message: "stream idle timeout".to_string(),
            retryable: true,
            attempts: Vec::new(),
            pool_id: Some(1),
            pool_name: Some("backup".to_string()),
        };
        assert_eq!(
            local_rescue_reason_after_external_error(&config, &timeout, None),
            Some("external_timeout")
        );

        let capacity = ExternalPoolFinalError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            response_error_type: "api_error".to_string(),
            route_error_type: "external_pool_capacity_full".to_string(),
            message: "Request capacity is full".to_string(),
            retryable: true,
            attempts: Vec::new(),
            pool_id: None,
            pool_name: None,
        };
        assert_eq!(
            local_rescue_reason_after_external_error(&config, &capacity, None),
            Some("external_capacity")
        );

        let bad_request = ExternalPoolFinalError {
            status: StatusCode::BAD_REQUEST,
            response_error_type: "invalid_request_error".to_string(),
            route_error_type: "client_error".to_string(),
            message: "Improperly formed request".to_string(),
            retryable: false,
            attempts: Vec::new(),
            pool_id: Some(1),
            pool_name: Some("backup".to_string()),
        };
        assert_eq!(
            local_rescue_reason_after_external_error(&config, &bad_request, None),
            None
        );

        let mut disabled = config.clone();
        disabled.external_pool_local_rescue_enabled = false;
        assert_eq!(
            local_rescue_reason_after_external_error(&disabled, &rate_limit, None),
            None
        );

        let mut no_rate_limit = config;
        no_rate_limit.external_pool_local_rescue_on_rate_limit = false;
        assert_eq!(
            local_rescue_reason_after_external_error(&no_rate_limit, &rate_limit, None),
            None
        );

        let mut no_capacity = no_rate_limit;
        no_capacity.external_pool_local_rescue_on_capacity = false;
        assert_eq!(
            local_rescue_reason_after_external_error(&no_capacity, &capacity, None),
            None
        );
    }

    #[test]
    fn remote_url_safety_rejects_local_and_private_targets() {
        for url in [
            "http://localhost/image.png",
            "http://127.0.0.1/image.png",
            "http://10.0.0.5/image.png",
            "http://169.254.169.254/latest/meta-data",
            "http://[::1]/image.png",
        ] {
            assert!(
                ensure_safe_remote_url(url).is_err(),
                "{url} should be blocked"
            );
        }
    }
}
