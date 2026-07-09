//! Anthropic API Handler 函数

use std::{
    collections::{HashMap, HashSet},
    convert::Infallible,
    fmt::Write as _,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
};

use crate::kiro::model::events::Event;
use crate::kiro::model::requests::kiro::KiroRequest;
use crate::kiro::parser::decoder::EventStreamDecoder;
use crate::model::config::{
    BodyConversionConfig, CacheBoundsPolicy, CachePointPolicy, CachePolicyConfig, CacheRoutePolicy,
    CacheSimulationPolicy, CompatProfile, Config, ExternalPoolsConfig, ImageProcessingConfig,
    KiroRsToolCachePolicy, MissingMaxTokensConfig, MissingMaxTokensPolicy, ModelMappingConfig,
    ModelResolutionMode, PayloadGuardMode, PayloadShapingConfig, PromptCacheCreationControlConfig,
    PromptCacheSimulationMode, PromptCacheStrategyType, ReportedUsageConfig,
    ReportedUsagePathPolicy, ResolvedCacheRoutePolicy, ThinkingTriggerMode,
    normalize_defined_cache_route, normalize_defined_cache_routes, resolve_cache_policy_for_path,
};
use crate::token;
use anyhow::Error;
use axum::{
    Json as JsonExtractor,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Json, Response},
};
use bytes::Bytes;
use chrono::Utc;
use futures::{Stream, StreamExt, stream};
use parking_lot::Mutex;
use reqwest::header::CONTENT_TYPE as REQWEST_CONTENT_TYPE;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::time::{Instant, interval, sleep_until};

use super::body_capabilities::ParsedAnthropicBodyPlan;
use super::body_processing;
use super::converter::{
    ConversionError, ConverterOptions, ProxyWarnings, convert_request_with_resolved_model,
    extract_stable_conversation_id,
};
use super::envelope;
use super::middleware::AppState;
use super::model_capabilities::{
    ModelResolution, ModelResolutionSource, strip_model_compat_suffixes,
};
use super::payload_guard::{
    PayloadByteBreakdown, PayloadGuardConfig, PayloadGuardError, PayloadGuardReport,
    ToolUseFormatDiagnostics, breakdown_kiro_request, diagnose_kiro_tool_use_format,
    guard_kiro_request, serialize_kiro_request,
};
use super::payload_guard_runtime::prepare_kiro_request_body;
use super::prompt_cache::{
    KiroRsToolPromptCachePlan, PromptCacheBounds, PromptCacheProfile, PromptCacheScope,
};
use super::request_facts::{
    probe_raw_messages_body, raw_messages_body_hints,
    rewrite_raw_missing_top_level_max_tokens_with_probe,
};
use super::stream::{SseEvent, StreamContext};
use super::tool_format_debug::{ToolFormatDebugEvent, ToolFormatDebugRecorder};
use super::types::{
    CountTokensRequest, CountTokensResponse, MessagesRequest, ModelsResponse, OutputConfig,
    Thinking,
};
use super::usage::{
    ExternalPoolAttempt, ExternalPoolUsageSnapshot, StreamTerminalReason, UsageLatencyTrace,
    UsagePublicError, UsageRecord, UsageRecordStatus, UsageRouteKind, UsageRouteSubtype,
    UsageSource,
};
use super::websearch;
use crate::external_pool::{
    ExternalPoolFinalError, ExternalPoolForwardOutcome, ExternalPoolManager,
    ExternalPoolRequestBodyMode, ExternalRouteRequest,
};
use crate::http_client::response_bytes_with_body_timeout;
use crate::kiro::call_trace::KiroCredentialAttempt;
use crate::kiro::provider::{KiroProvider, KiroStreamCompletion};
use crate::kiro::token_manager::LocalPoolRouteStateKind;

#[path = "handlers/local_body_pipeline.rs"]
mod local_body_pipeline;
#[path = "handlers/parsed_body_pipeline.rs"]
mod parsed_body_pipeline;
#[path = "handlers/request_entry.rs"]
mod request_entry;

const UPSTREAM_INVALID_REQUEST_MESSAGE: &str = envelope::PUBLIC_INVALID_REQUEST_MESSAGE;
const LATENCY_COUNTER_UNSET: u32 = u32::MAX;
const OBSERVABILITY_HASH_BYTES: usize = 8;
const SLOW_FIRST_VISIBLE_TEXT_MS: u64 = 10_000;
const SLOW_STREAM_GAP_MS: u64 = 10_000;
const SLOW_RESPONSE_MS: u64 = 60_000;
const SLOW_EVENTS_BEFORE_FIRST_OUTPUT: u32 = 20;

#[derive(Debug, Clone, Default)]
struct TextDigestSummary {
    bytes: usize,
    chars: usize,
    segments: usize,
    hash: Option<String>,
}

struct TextDigestBuilder {
    hasher: Sha256,
    bytes: usize,
    chars: usize,
    segments: usize,
}

impl TextDigestBuilder {
    fn new() -> Self {
        Self {
            hasher: Sha256::new(),
            bytes: 0,
            chars: 0,
            segments: 0,
        }
    }

    fn add_text(&mut self, text: &str) {
        if self.segments > 0 {
            self.hasher.update(b"\n");
        }
        self.hasher.update(text.as_bytes());
        self.bytes = self.bytes.saturating_add(text.len());
        self.chars = self.chars.saturating_add(text.chars().count());
        self.segments = self.segments.saturating_add(1);
    }

    fn finish(self) -> TextDigestSummary {
        TextDigestSummary {
            bytes: self.bytes,
            chars: self.chars,
            segments: self.segments,
            hash: (self.segments > 0).then(|| {
                let digest = self.hasher.finalize();
                short_digest_hex(&digest)
            }),
        }
    }
}

#[derive(Debug, Clone)]
struct AnthropicContentSummary {
    kind: &'static str,
    text: TextDigestSummary,
    tool_result_count: usize,
    tool_use_count: usize,
    image_count: usize,
    document_count: usize,
    other_block_count: usize,
}

#[derive(Clone)]
struct RequestUsageContext {
    recorder: Arc<super::usage::UsageRecorder>,
    tool_format_debug_recorder: Arc<ToolFormatDebugRecorder>,
    prompt_cache: Arc<super::prompt_cache::PromptCacheTracker>,
    prompt_cache_creation_controller:
        Arc<super::prompt_cache_creation_control::PromptCacheCreationController>,
    pricing_catalog: Arc<super::pricing::PricingCatalog>,
    request_id: String,
    error_id: String,
    endpoint: String,
    stream: bool,
    model: String,
    upstream_model: Option<String>,
    model_resolution_source: Option<String>,
    model_resolution_note: Option<String>,
    requested_max_tokens: i32,
    downstream_stop_reason: Arc<Mutex<Option<String>>>,
    conversation_id: Option<String>,
    prompt_cache_scope_conversation_id: Option<String>,
    input_tokens: i32,
    context_window_tokens: i32,
    prompt_cache_profile: Option<PromptCacheProfile>,
    kiro_rs_tool_prompt_cache_plan: Option<KiroRsToolPromptCachePlan>,
    prompt_cache_route_namespace: Option<String>,
    prompt_cache_strategy_type: PromptCacheStrategyType,
    simulation_mode: PromptCacheSimulationMode,
    prompt_cache_target_read_ratio: f64,
    prompt_cache_token_scale: f64,
    prompt_cache_max_simulated_input_tokens: i32,
    prompt_cache_cap_jitter_min_tokens: i32,
    prompt_cache_cap_jitter_max_tokens: i32,
    prompt_cache_scale_min_input_tokens: i32,
    prompt_cache_creation_control: PromptCacheCreationControlConfig,
    prompt_cache_bounds: PromptCacheBounds,
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
    capacity_weight_units: Arc<AtomicU32>,
    latency: RequestLatencyTraceState,
}

#[derive(Clone)]
struct RequestLatencyTraceState {
    payload_guard_ms: Option<u64>,
    upstream_header_latency_ms: Arc<AtomicU64>,
    first_upstream_chunk_latency_ms: Arc<AtomicU64>,
    first_thinking_delta_latency_ms: Arc<AtomicU64>,
    first_visible_text_delta_latency_ms: Arc<AtomicU64>,
    client_dropped_latency_ms: Arc<AtomicU64>,
    upstream_chunks_seen: Arc<AtomicU32>,
    events_seen_before_first_output: Arc<AtomicU32>,
    chunks_before_first_output: Arc<AtomicU32>,
    events_before_first_output: Arc<AtomicU32>,
    upstream_bytes_before_first_output: Arc<AtomicU64>,
    upstream_frames_before_first_output: Arc<AtomicU32>,
    upstream_events_before_first_output: Arc<AtomicU32>,
    upstream_frames_without_downstream_events_before_first_output: Arc<AtomicU32>,
    upstream_pending_chunks_before_first_output: Arc<AtomicU32>,
    upstream_frame_decode_errors_before_first_output: Arc<AtomicU32>,
    upstream_event_parse_errors_before_first_output: Arc<AtomicU32>,
    upstream_event_types_before_first_output: Arc<Mutex<HashMap<&'static str, u32>>>,
    terminal_reason: Arc<Mutex<Option<StreamTerminalReason>>>,
}

impl RequestLatencyTraceState {
    fn new() -> Self {
        Self {
            payload_guard_ms: None,
            upstream_header_latency_ms: Arc::new(AtomicU64::new(0)),
            first_upstream_chunk_latency_ms: Arc::new(AtomicU64::new(0)),
            first_thinking_delta_latency_ms: Arc::new(AtomicU64::new(0)),
            first_visible_text_delta_latency_ms: Arc::new(AtomicU64::new(0)),
            client_dropped_latency_ms: Arc::new(AtomicU64::new(0)),
            upstream_chunks_seen: Arc::new(AtomicU32::new(0)),
            events_seen_before_first_output: Arc::new(AtomicU32::new(0)),
            chunks_before_first_output: Arc::new(AtomicU32::new(LATENCY_COUNTER_UNSET)),
            events_before_first_output: Arc::new(AtomicU32::new(LATENCY_COUNTER_UNSET)),
            upstream_bytes_before_first_output: Arc::new(AtomicU64::new(0)),
            upstream_frames_before_first_output: Arc::new(AtomicU32::new(0)),
            upstream_events_before_first_output: Arc::new(AtomicU32::new(0)),
            upstream_frames_without_downstream_events_before_first_output: Arc::new(
                AtomicU32::new(0),
            ),
            upstream_pending_chunks_before_first_output: Arc::new(AtomicU32::new(0)),
            upstream_frame_decode_errors_before_first_output: Arc::new(AtomicU32::new(0)),
            upstream_event_parse_errors_before_first_output: Arc::new(AtomicU32::new(0)),
            upstream_event_types_before_first_output: Arc::new(Mutex::new(HashMap::new())),
            terminal_reason: Arc::new(Mutex::new(None)),
        }
    }
}

fn load_latency_ms(value: &AtomicU64) -> Option<u64> {
    let value = value.load(Ordering::Acquire);
    (value > 0).then_some(value)
}

fn load_latency_counter(value: &AtomicU32) -> Option<u32> {
    let value = value.load(Ordering::Acquire);
    (value != LATENCY_COUNTER_UNSET).then_some(value)
}

fn short_digest_hex(digest: &[u8]) -> String {
    let mut out = String::with_capacity(OBSERVABILITY_HASH_BYTES * 2);
    for byte in digest.iter().take(OBSERVABILITY_HASH_BYTES) {
        let _ = write!(out, "{:02x}", byte);
    }
    out
}

fn short_text_hash(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    short_digest_hex(&digest)
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn summarize_anthropic_content(content: &Value) -> AnthropicContentSummary {
    let mut text = TextDigestBuilder::new();
    let mut summary = AnthropicContentSummary {
        kind: value_kind(content),
        text: TextDigestSummary::default(),
        tool_result_count: 0,
        tool_use_count: 0,
        image_count: 0,
        document_count: 0,
        other_block_count: 0,
    };

    summarize_anthropic_content_value(content, &mut summary, &mut text);
    summary.text = text.finish();
    summary
}

fn summarize_anthropic_content_value(
    content: &Value,
    summary: &mut AnthropicContentSummary,
    text: &mut TextDigestBuilder,
) {
    match content {
        Value::String(value) => text.add_text(value),
        Value::Array(blocks) => {
            for block in blocks {
                summarize_anthropic_content_block(block, summary, text);
            }
        }
        Value::Object(_) => summarize_anthropic_content_block(content, summary, text),
        _ => {}
    }
}

fn summarize_anthropic_content_block(
    block: &Value,
    summary: &mut AnthropicContentSummary,
    text: &mut TextDigestBuilder,
) {
    let Some(object) = block.as_object() else {
        if let Some(value) = block.as_str() {
            text.add_text(value);
        } else {
            summary.other_block_count = summary.other_block_count.saturating_add(1);
        }
        return;
    };

    match object.get("type").and_then(Value::as_str) {
        Some("text") => {
            if let Some(value) = object.get("text").and_then(Value::as_str) {
                text.add_text(value);
            }
        }
        Some("tool_result") => {
            summary.tool_result_count = summary.tool_result_count.saturating_add(1);
        }
        Some("tool_use") => {
            summary.tool_use_count = summary.tool_use_count.saturating_add(1);
        }
        Some("image") | Some("image_url") => {
            summary.image_count = summary.image_count.saturating_add(1);
        }
        Some("document") => {
            summary.document_count = summary.document_count.saturating_add(1);
        }
        Some(_) => {
            if let Some(value) = object.get("text").and_then(Value::as_str) {
                text.add_text(value);
            } else {
                summary.other_block_count = summary.other_block_count.saturating_add(1);
            }
        }
        None => {
            if let Some(value) = object.get("text").and_then(Value::as_str) {
                text.add_text(value);
            } else {
                summary.other_block_count = summary.other_block_count.saturating_add(1);
            }
        }
    }
}

fn count_tool_use_blocks(content: &Value) -> usize {
    match content {
        Value::Array(blocks) => blocks.iter().map(count_tool_use_blocks).sum(),
        Value::Object(object) => usize::from(
            object
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|block_type| block_type == "tool_use"),
        ),
        _ => 0,
    }
}

fn log_anthropic_request_summary(endpoint: &str, payload: &MessagesRequest) {
    if !tracing::enabled!(tracing::Level::DEBUG) {
        return;
    }

    let last_user = payload
        .messages
        .iter()
        .enumerate()
        .rev()
        .find(|(_, message)| message.role == "user");
    let (last_user_index, last_user_summary) = last_user
        .map(|(index, message)| {
            (
                Some(index),
                Some(summarize_anthropic_content(&message.content)),
            )
        })
        .unwrap_or((None, None));
    let assistant_tool_use_count: usize = payload
        .messages
        .iter()
        .filter(|message| message.role == "assistant")
        .map(|message| count_tool_use_blocks(&message.content))
        .sum();
    let metadata_user_hash = payload
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.user_id.as_deref())
        .map(short_text_hash);

    tracing::debug!(
        endpoint,
        model = %payload.model,
        message_count = payload.messages.len(),
        last_user_index = ?last_user_index,
        last_user_content_kind = last_user_summary.as_ref().map(|summary| summary.kind),
        last_user_text_bytes = last_user_summary.as_ref().map(|summary| summary.text.bytes).unwrap_or(0),
        last_user_text_chars = last_user_summary.as_ref().map(|summary| summary.text.chars).unwrap_or(0),
        last_user_text_segments = last_user_summary.as_ref().map(|summary| summary.text.segments).unwrap_or(0),
        last_user_text_hash = ?last_user_summary.as_ref().and_then(|summary| summary.text.hash.as_deref()),
        last_user_tool_result_count = last_user_summary.as_ref().map(|summary| summary.tool_result_count).unwrap_or(0),
        last_user_tool_use_count = last_user_summary.as_ref().map(|summary| summary.tool_use_count).unwrap_or(0),
        last_user_image_count = last_user_summary.as_ref().map(|summary| summary.image_count).unwrap_or(0),
        last_user_document_count = last_user_summary.as_ref().map(|summary| summary.document_count).unwrap_or(0),
        last_user_other_block_count = last_user_summary.as_ref().map(|summary| summary.other_block_count).unwrap_or(0),
        assistant_tool_use_count,
        tool_definition_count = payload.tools.as_ref().map(Vec::len).unwrap_or(0),
        system_message_count = payload.system.as_ref().map(Vec::len).unwrap_or(0),
        metadata_user_hash = ?metadata_user_hash,
        "Anthropic request summary"
    );
}

fn log_thinking_request_trace(
    endpoint: &str,
    payload: &MessagesRequest,
    runtime_config: &RequestRuntimeConfig,
) {
    if !tracing::enabled!(tracing::Level::DEBUG) {
        return;
    }

    let thinking_type = payload
        .thinking
        .as_ref()
        .map(|thinking| thinking.thinking_type.as_str());
    let thinking_requested = payload
        .thinking
        .as_ref()
        .is_some_and(|thinking| thinking.is_enabled());
    let output_effort = payload
        .output_config
        .as_ref()
        .map(|config| config.effort.as_str());
    let latest_user_has_visible_thinking_signal =
        request_has_claude_code_visible_thinking_signal(payload);

    tracing::debug!(
        endpoint,
        model = %payload.model,
        trigger_mode = ?runtime_config.thinking_trigger_mode,
        thinking_requested,
        thinking_type = ?thinking_type,
        output_effort = ?output_effort,
        force_visible_thinking = should_force_visible_thinking(payload, runtime_config),
        latest_user_has_visible_thinking_signal,
        "Anthropic thinking request trace"
    );
}

fn log_kiro_conversion_summary(
    endpoint: &str,
    payload: &MessagesRequest,
    model_resolution: &ModelResolution,
    kiro_request: &KiroRequest,
    request_bytes: usize,
    payload_guard_report: Option<&PayloadGuardReport>,
    warnings: &ProxyWarnings,
) {
    if !tracing::enabled!(tracing::Level::DEBUG) {
        return;
    }

    let current_user_input = &kiro_request
        .conversation_state
        .current_message
        .user_input_message;
    let current_context = &current_user_input.user_input_message_context;
    let current_content = current_user_input.content.as_str();
    let warnings_header = warnings.encode_header();
    let history_entries = kiro_request.conversation_state.history.len();
    let original_history_entries = payload_guard_report
        .map(|report| report.original_history_entries)
        .unwrap_or(history_entries);
    let final_history_entries = payload_guard_report
        .map(|report| report.final_history_entries)
        .unwrap_or(history_entries);

    tracing::debug!(
        endpoint,
        requested_model = %payload.model,
        upstream_model = ?model_resolution.upstream_model,
        conversation_id = %kiro_request.conversation_state.conversation_id,
        request_bytes,
        payload_guard_enabled = payload_guard_report.is_some(),
        original_history_entries,
        final_history_entries,
        current_message_bytes = current_content.len(),
        current_message_chars = current_content.chars().count(),
        current_message_hash = %short_text_hash(current_content),
        current_tool_count = current_context.tools.len(),
        current_tool_result_count = current_context.tool_results.len(),
        current_image_count = current_user_input.images.len(),
        warning_header = ?warnings_header,
        warning_prefill_dropped = warnings.prefill_dropped,
        warning_orphan_tool_results = warnings.orphan_tool_results,
        warning_orphan_tool_results_textified = warnings.orphan_tool_results_textified,
        warning_orphan_tool_uses = warnings.orphan_tool_uses,
        warning_duplicate_tool_results = warnings.duplicate_tool_results,
        warning_duplicate_tool_results_textified = warnings.duplicate_tool_results_textified,
        warning_tool_result_content_placeholders = warnings.tool_result_content_placeholders,
        warning_empty_content_placeholders = warnings.empty_content_placeholders,
        "Kiro conversion summary"
    );
}

fn saturating_fetch_add_u32(value: &AtomicU32, amount: u32) {
    let _ = value.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_add(amount))
    });
}

fn saturating_fetch_add_u64(value: &AtomicU64, amount: u64) {
    let _ = value.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_add(amount))
    });
}

#[derive(Clone)]
struct ExternalFallbackContext {
    manager: Arc<ExternalPoolManager>,
    config: ExternalPoolsConfig,
    raw_body: Bytes,
    headers: HeaderMap,
    endpoint: String,
    payload: MessagesRequest,
    model_resolution: Option<ModelResolution>,
    reported_usage: ReportedUsageConfig,
    prompt_cache: Arc<super::prompt_cache::PromptCacheTracker>,
    prompt_cache_creation_controller:
        Arc<super::prompt_cache_creation_control::PromptCacheCreationController>,
    prompt_cache_strategy_type: PromptCacheStrategyType,
    prompt_cache_simulation_mode: PromptCacheSimulationMode,
    prompt_cache_route_namespace: Option<String>,
    prompt_cache_target_read_ratio: f64,
    prompt_cache_token_scale: f64,
    prompt_cache_max_simulated_input_tokens: i32,
    prompt_cache_cap_jitter_min_tokens: i32,
    prompt_cache_cap_jitter_max_tokens: i32,
    prompt_cache_scale_min_input_tokens: i32,
    prompt_cache_creation_control: PromptCacheCreationControlConfig,
    prompt_cache_bounds: PromptCacheBounds,
    kiro_rs_tool_cache_policy: KiroRsToolCachePolicy,
    model_capabilities: Arc<super::model_capabilities::ModelCapabilitiesCatalog>,
    pricing_catalog: Arc<super::pricing::PricingCatalog>,
    recorder: Arc<super::usage::UsageRecorder>,
    error_id: String,
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
    error_metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CredentialErrorHint {
    id: u64,
    label: Option<String>,
}

#[derive(Debug, Clone)]
struct RequestRuntimeConfig {
    extract_thinking: bool,
    thinking_trigger_mode: ThinkingTriggerMode,
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
    defined_cache_routes: Vec<String>,
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
    kiro_cache_point_enabled: bool,
    kiro_cache_point_tools_only: bool,
    kiro_cache_point_record_plan: bool,
    kiro_upstream_stream_idle_timeout_secs: u64,
    image_processing: ImageProcessingConfig,
    body_conversion: BodyConversionConfig,
    missing_max_tokens: MissingMaxTokensConfig,
    payload_shaping: PayloadShapingConfig,
    external_pools: ExternalPoolsConfig,
}

impl RequestRuntimeConfig {
    fn from_app_state(state: &AppState) -> Self {
        Self {
            extract_thinking: state.extract_thinking,
            thinking_trigger_mode: state.thinking_trigger_mode,
            prompt_cache_simulation_mode: state.prompt_cache_simulation_mode,
            prompt_cache_target_read_ratio: state.prompt_cache_target_read_ratio,
            prompt_cache_token_scale: state.prompt_cache_token_scale,
            prompt_cache_max_simulated_input_tokens: state.prompt_cache_max_simulated_input_tokens,
            prompt_cache_cap_jitter_min_tokens: state.prompt_cache_cap_jitter_min_tokens,
            prompt_cache_cap_jitter_max_tokens: state.prompt_cache_cap_jitter_max_tokens,
            prompt_cache_scale_min_input_tokens: state.prompt_cache_scale_min_input_tokens,
            prompt_cache_creation_control: state.prompt_cache_creation_control,
            prompt_cache_bounds: state.prompt_cache_bounds,
            reported_usage: state.reported_usage.clone(),
            cache_policy: state.cache_policy.clone(),
            defined_cache_routes: state.defined_cache_routes.clone(),
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
            kiro_cache_point_enabled: state.kiro_cache_point_enabled,
            kiro_cache_point_tools_only: state.kiro_cache_point_tools_only,
            kiro_cache_point_record_plan: state.kiro_cache_point_record_plan,
            kiro_upstream_stream_idle_timeout_secs: state.kiro_upstream_stream_idle_timeout_secs,
            image_processing: state.image_processing.normalized(),
            body_conversion: state.body_conversion,
            missing_max_tokens: state.missing_max_tokens.normalized(),
            payload_shaping: state.payload_shaping,
            external_pools: state.external_pools.clone(),
        }
    }

    fn from_config_with_fallback(config: &Config, fallback: Self) -> Self {
        Self {
            extract_thinking: config.extract_thinking,
            thinking_trigger_mode: config.thinking_trigger_mode,
            prompt_cache_simulation_mode: fallback.prompt_cache_simulation_mode,
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
            prompt_cache_bounds: PromptCacheBounds::from_config(
                config.prompt_cache_max_entries_per_account,
                config.prompt_cache_max_entries_global,
                config.prompt_cache_entry_ttl_secs,
                config.prompt_cache_estimated_bytes_limit,
            ),
            reported_usage: config.reported_usage.normalized(),
            cache_policy: config
                .cache_policy
                .clone()
                .with_builtin_path_defaults()
                .with_legacy_defined_cache_route_defaults(&config.defined_cache_routes)
                .normalized(),
            defined_cache_routes: normalize_defined_cache_routes(&config.defined_cache_routes),
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
            kiro_cache_point_enabled: config.kiro_cache_point_enabled,
            kiro_cache_point_tools_only: config.kiro_cache_point_tools_only,
            kiro_cache_point_record_plan: config.kiro_cache_point_record_plan,
            kiro_upstream_stream_idle_timeout_secs: config.kiro_upstream_stream_idle_timeout_secs,
            image_processing: config.image_processing.normalized(),
            body_conversion: config.body_conversion,
            missing_max_tokens: config.missing_max_tokens.normalized(),
            payload_shaping: config.payload_shaping,
            external_pools: config.external_pools.clone(),
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
            cache_point: CachePointPolicy {
                enabled: self.kiro_cache_point_enabled,
                tools_only: self.kiro_cache_point_tools_only,
                record_plan: self.kiro_cache_point_record_plan,
            },
            bounds: CacheBoundsPolicy {
                max_entries_per_account: self.prompt_cache_bounds.max_entries_per_account,
                max_entries_global: self.prompt_cache_bounds.max_entries_global,
                entry_ttl_secs: self.prompt_cache_bounds.entry_ttl.as_secs(),
                estimated_bytes_limit: self.prompt_cache_bounds.estimated_bytes_limit,
            },
            kiro_rs_tool: KiroRsToolCachePolicy::default(),
        }
        .normalized()
    }

    fn cache_policy_for_path(&self, path: &str) -> ResolvedCacheRoutePolicy {
        resolve_cache_policy_for_path(
            self.legacy_cache_route_policy_default(),
            &self.reported_usage,
            &self
                .cache_policy
                .clone()
                .with_builtin_path_defaults()
                .with_legacy_defined_cache_route_defaults(&self.defined_cache_routes),
            path,
        )
    }
}

fn cache_route_for_request_stream(
    mut cache_route: ResolvedCacheRoutePolicy,
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

    cache_route.policy.cache_type = PromptCacheStrategyType::NoCache;
    cache_route.policy.simulation.enabled = false;
    cache_route.policy.creation_control.enabled = false;
    cache_route.policy.cache_point.enabled = false;
    cache_route.policy.reported_usage.enabled = false;
    cache_route.policy = cache_route.policy.normalized();
    cache_route.namespace = None;
    cache_route
}

fn prompt_cache_simulation_mode_for_policy(policy: &CacheRoutePolicy) -> PromptCacheSimulationMode {
    match policy.cache_type {
        PromptCacheStrategyType::CurrentHighCache if policy.simulation.enabled => {
            PromptCacheSimulationMode::HighCache
        }
        PromptCacheStrategyType::NoCache
        | PromptCacheStrategyType::CurrentHighCache
        | PromptCacheStrategyType::KiroRsTool => PromptCacheSimulationMode::Disabled,
    }
}

fn prompt_cache_converter_mode_for_policy(policy: &CacheRoutePolicy) -> PromptCacheSimulationMode {
    match policy.cache_type {
        PromptCacheStrategyType::NoCache => PromptCacheSimulationMode::Disabled,
        PromptCacheStrategyType::CurrentHighCache => {
            prompt_cache_simulation_mode_for_policy(policy)
        }
        PromptCacheStrategyType::KiroRsTool => PromptCacheSimulationMode::HighCache,
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
    ReportedUsageConfig {
        default: policy,
        path_overrides: Default::default(),
    }
}

#[derive(Clone)]
struct PayloadTooLongRetryRequest {
    request: KiroRequest,
    config: PayloadGuardConfig,
    endpoint: String,
    requested_model: String,
    upstream_model: Option<String>,
    conversation_id: String,
    conversion_warnings: Option<String>,
}

impl PayloadTooLongRetryRequest {
    fn new(
        request: &KiroRequest,
        runtime_config: &RequestRuntimeConfig,
        endpoint: &str,
        requested_model: &str,
        upstream_model: Option<&str>,
        conversation_id: &str,
        conversion_warnings: Option<String>,
    ) -> Option<Self> {
        runtime_config.too_long_retry_enabled().then(|| Self {
            request: request.clone(),
            config: runtime_config.payload_guard_config(),
            endpoint: endpoint.to_string(),
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
            &self.endpoint,
            &self.requested_model,
            self.upstream_model.as_deref(),
            Some(&self.conversation_id),
        );
        let breakdown = breakdown_kiro_request(&request, &request_body);
        log_payload_byte_breakdown(
            should_log_payload_byte_breakdown(&report).then_some(breakdown),
            &report,
            &self.endpoint,
            &self.requested_model,
            self.upstream_model.as_deref(),
            Some(&self.conversation_id),
        );
        usage_context.set_payload_diagnostics(Some(breakdown), report.clone());
        let warnings_header = merge_warning_headers(self.conversion_warnings, Some(&report));
        Ok((request_body, warnings_header))
    }
}

#[derive(Clone)]
struct CachePointRetryRequest {
    request: KiroRequest,
    endpoint: String,
    requested_model: String,
    upstream_model: Option<String>,
    conversation_id: String,
}

impl CachePointRetryRequest {
    fn new(
        request: &KiroRequest,
        endpoint: &str,
        requested_model: &str,
        upstream_model: Option<&str>,
        conversation_id: &str,
    ) -> Option<Self> {
        request.has_tool_cache_point_plan().then(|| Self {
            request: request.clone(),
            endpoint: endpoint.to_string(),
            requested_model: requested_model.to_string(),
            upstream_model: upstream_model.map(str::to_string),
            conversation_id: conversation_id.to_string(),
        })
    }

    fn build_retry_body(
        self,
        reason: &str,
        usage_context: &mut RequestUsageContext,
    ) -> Result<String, PayloadGuardError> {
        let mut request = self.request;
        let planned = request.clear_tool_cache_point_plan();
        usage_context.attach_cache_point_retry(planned, reason);
        let body = serialize_kiro_request(&request)?;
        tracing::warn!(
            request_id = %usage_context.request_id,
            endpoint = %self.endpoint,
            requested_model = %self.requested_model,
            upstream_model = ?self.upstream_model,
            conversation_id = %self.conversation_id,
            planned_cache_points = planned,
            "Kiro cachePoint payload was rejected; retrying once without cachePoint"
        );
        Ok(body)
    }
}

fn request_runtime_config(state: &AppState, provider: &KiroProvider) -> RequestRuntimeConfig {
    RequestRuntimeConfig::from_config_with_fallback(
        &provider.runtime_config(),
        RequestRuntimeConfig::from_app_state(state),
    )
}

fn request_image_processing_config(state: &AppState) -> ImageProcessingConfig {
    state
        .kiro_provider
        .as_ref()
        .map(|provider| request_runtime_config(state, provider).image_processing)
        .unwrap_or_else(|| state.image_processing.normalized())
}

#[cfg(test)]
fn parse_messages_payload(raw_body: &Bytes) -> Result<MessagesRequest, Response> {
    let request_id = envelope::request_id();
    request_entry::parse_messages_payload(raw_body, &request_id)
        .map_err(|error| error.to_response(&request_id))
}

async fn maybe_raw_external_direct_response(
    state: &AppState,
    headers: HeaderMap,
    raw_body: Bytes,
    endpoint: &str,
) -> Option<Response> {
    let provider = state.kiro_provider.as_ref()?.clone();
    let manager = state.external_pool_manager.clone()?;
    let runtime_config = request_runtime_config(state, &provider);
    let cache_route = runtime_config.cache_policy_for_path(endpoint);
    let config = runtime_config.external_pools.clone();
    if !manager
        .has_eligible_pool_for_body_mode(&config, ExternalPoolRequestBodyMode::RawPassthrough)
        .await
    {
        return None;
    }

    let (model_hint, _) = raw_messages_body_hints(&raw_body);
    let reason = manager
        .direct_policy_reason(&config, endpoint, model_hint.as_deref().unwrap_or(""))
        .await?;
    let request_id = envelope::request_id();
    let route = raw_external_route_request(
        state,
        &runtime_config,
        &cache_route,
        headers,
        raw_body,
        endpoint,
        request_id,
        UsageRouteSubtype::ExternalDirectPolicy,
        None,
        Some(reason),
        None,
    );

    Some(manager.forward_with_failover(config, route).await)
}

async fn maybe_raw_external_preflight_response(
    state: &AppState,
    headers: HeaderMap,
    raw_body: Bytes,
    endpoint: &str,
) -> Option<Response> {
    let provider = state.kiro_provider.as_ref()?.clone();
    let manager = state.external_pool_manager.clone()?;
    let runtime_config = request_runtime_config(state, &provider);
    let cache_route = runtime_config.cache_policy_for_path(endpoint);
    let config = runtime_config.external_pools.clone();
    if !config.local_pool_preflight_enabled {
        return None;
    }
    if !manager
        .has_available_pool_for_body_mode(&config, ExternalPoolRequestBodyMode::RawPassthrough)
        .await
    {
        return None;
    }

    let local_state = provider.local_pool_route_state(None);
    if !local_state.kind.should_route_external() {
        return None;
    }
    let Some(reason) = local_pool_route_fallback_reason(local_state.kind, &config) else {
        return None;
    };

    let reason = reason.to_string();
    let request_id = envelope::request_id();
    tracing::warn!(
        request_id,
        reason = %reason,
        local_total = local_state.total,
        local_available = local_state.available,
        local_dispatchable = local_state.dispatchable,
        local_usable = local_state.usable,
        retry_after_secs = ?local_state.retry_after_secs,
        "local credential pool is not immediately schedulable; routing raw request directly to external pool before parsing body"
    );
    let route = raw_external_route_request(
        state,
        &runtime_config,
        &cache_route,
        headers,
        raw_body,
        endpoint,
        request_id,
        UsageRouteSubtype::ExternalFallbackPreflight,
        Some(reason.clone()),
        None,
        Some(json!({
            "reason": reason,
            "state": local_state,
            "preflightStage": "before_parse",
            "requiredBodyMode": ExternalPoolRequestBodyMode::RawPassthrough.as_str(),
        })),
    );

    Some(manager.forward_with_failover(config, route).await)
}

fn raw_external_route_request(
    state: &AppState,
    runtime_config: &RequestRuntimeConfig,
    cache_route: &ResolvedCacheRoutePolicy,
    headers: HeaderMap,
    raw_body: Bytes,
    endpoint: &str,
    request_id: String,
    route_subtype: UsageRouteSubtype,
    fallback_reason: Option<String>,
    direct_policy_reason: Option<String>,
    local_preflight: Option<serde_json::Value>,
) -> ExternalRouteRequest {
    let (model_hint, stream_hint) = raw_messages_body_hints(&raw_body);
    let effective_cache_route =
        cache_route_for_request_stream(cache_route.clone(), stream_hint.unwrap_or(false));
    let policy = &effective_cache_route.policy;
    ExternalRouteRequest {
        raw_body,
        headers,
        endpoint: endpoint.to_string(),
        payload: None,
        body_mode_filter: Some(ExternalPoolRequestBodyMode::RawPassthrough),
        model_hint,
        stream_hint,
        request_input_tokens: 0,
        upstream_model: None,
        model_resolution_source: None,
        model_resolution_note: None,
        route_subtype,
        fallback_reason,
        direct_policy_reason,
        local_attempted: false,
        local_preflight,
        local_attempts: Vec::new(),
        reported_usage: reported_usage_config_for_policy(policy.reported_usage.clone()),
        prompt_cache: state.prompt_cache.clone(),
        prompt_cache_creation_controller: state.prompt_cache_creation_controller.clone(),
        prompt_cache_strategy_type: policy.cache_type,
        prompt_cache_simulation_mode: prompt_cache_simulation_mode_for_policy(policy),
        prompt_cache_route_namespace: effective_cache_route.namespace.clone(),
        prompt_cache_target_read_ratio: policy.simulation.target_read_ratio,
        prompt_cache_token_scale: policy.simulation.token_scale,
        prompt_cache_max_simulated_input_tokens: policy.simulation.max_simulated_input_tokens,
        prompt_cache_cap_jitter_min_tokens: policy.simulation.cap_jitter_min_tokens,
        prompt_cache_cap_jitter_max_tokens: policy.simulation.cap_jitter_max_tokens,
        prompt_cache_scale_min_input_tokens: policy.simulation.scale_min_input_tokens,
        prompt_cache_creation_control: policy.creation_control,
        prompt_cache_bounds: prompt_cache_bounds_for_policy(policy),
        kiro_rs_tool_cache_policy: policy.kiro_rs_tool,
        model_capabilities: state.model_capabilities.clone(),
        pricing_catalog: state.pricing_catalog.clone(),
        request_id: request_id.clone(),
        error_id: envelope::request_id(),
        recorder: state.usage_recorder.clone(),
        started_at: Instant::now(),
        first_token_latency_ms: Arc::new(AtomicU64::new(0)),
        latency_trace: Arc::new(crate::external_pool::ExternalLatencyTraceState::default()),
        payload_breakdown: None,
        payload_guard_report: None,
        payload_guard_external_enabled: false,
        payload_guard_initial_config: runtime_config.initial_payload_guard_config(),
        payload_guard_retry_config: None,
    }
}

fn build_external_fallback_context(
    state: &AppState,
    runtime_config: &RequestRuntimeConfig,
    cache_route: &ResolvedCacheRoutePolicy,
    endpoint: &str,
    raw_body: Bytes,
    headers: HeaderMap,
    payload: &MessagesRequest,
) -> Option<ExternalFallbackContext> {
    let manager = state.external_pool_manager.clone()?;
    let config = runtime_config.external_pools.clone();
    let effective_cache_route = cache_route_for_request_stream(cache_route.clone(), payload.stream);
    let policy = &effective_cache_route.policy;
    config
        .external_pools_enabled
        .then_some(ExternalFallbackContext {
            manager,
            config,
            raw_body,
            headers,
            endpoint: endpoint.to_string(),
            payload: payload.clone(),
            model_resolution: None,
            reported_usage: reported_usage_config_for_policy(policy.reported_usage.clone()),
            prompt_cache: state.prompt_cache.clone(),
            prompt_cache_creation_controller: state.prompt_cache_creation_controller.clone(),
            prompt_cache_strategy_type: policy.cache_type,
            prompt_cache_simulation_mode: prompt_cache_simulation_mode_for_policy(policy),
            prompt_cache_route_namespace: effective_cache_route.namespace.clone(),
            prompt_cache_target_read_ratio: policy.simulation.target_read_ratio,
            prompt_cache_token_scale: policy.simulation.token_scale,
            prompt_cache_max_simulated_input_tokens: policy.simulation.max_simulated_input_tokens,
            prompt_cache_cap_jitter_min_tokens: policy.simulation.cap_jitter_min_tokens,
            prompt_cache_cap_jitter_max_tokens: policy.simulation.cap_jitter_max_tokens,
            prompt_cache_scale_min_input_tokens: policy.simulation.scale_min_input_tokens,
            prompt_cache_creation_control: policy.creation_control,
            prompt_cache_bounds: prompt_cache_bounds_for_policy(policy),
            kiro_rs_tool_cache_policy: policy.kiro_rs_tool,
            model_capabilities: state.model_capabilities.clone(),
            pricing_catalog: state.pricing_catalog.clone(),
            recorder: state.usage_recorder.clone(),
            error_id: envelope::request_id(),
            payload_guard_external_enabled: runtime_config.payload_guard_external_enabled,
            payload_guard_initial_config: runtime_config.initial_payload_guard_config(),
            payload_guard_retry_config: (runtime_config.payload_guard_external_enabled
                && runtime_config.too_long_retry_enabled())
            .then(|| runtime_config.payload_guard_config()),
        })
}

impl ExternalFallbackContext {
    fn refresh_payload(&mut self, payload: &MessagesRequest) {
        self.payload = payload.clone();
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
            .direct_policy_reason(&self.config, &self.endpoint, &self.payload.model)
            .await?;
        let route = match self.route_request(
            request_id.to_string(),
            UsageRouteSubtype::ExternalDirectPolicy,
            None,
            Some(reason),
            false,
            None,
            Vec::new(),
        ) {
            Ok(route) => route,
            Err(err) => return Some(payload_guard_error_response(err)),
        };
        Some(
            self.manager
                .forward_with_failover(self.config.clone(), route)
                .await,
        )
    }

    async fn local_pool_preflight_outcome(
        &self,
        provider: &KiroProvider,
        request_id: &str,
        model: Option<&str>,
    ) -> Option<ExternalPoolForwardOutcome> {
        if !self.config.local_pool_preflight_enabled {
            return None;
        }
        let state = provider.local_pool_route_state(model);
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
        let route = match self.route_request(
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
        ) {
            Ok(route) => route,
            Err(err) => {
                return Some(ExternalPoolForwardOutcome::Response(
                    payload_guard_error_response(err),
                ));
            }
        };
        Some(
            self.manager
                .forward_with_failover_result(self.config.clone(), route)
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
        let classification_attempts = local_attempts.clone();
        self.fallback_after_local_error_outcome_with_diagnostics(
            request_id,
            error_message,
            classification_attempts,
            local_attempts,
        )
        .await
    }

    async fn fallback_after_local_error_outcome_with_diagnostics(
        &self,
        request_id: &str,
        error_message: &str,
        classification_attempts: Vec<KiroCredentialAttempt>,
        diagnostic_attempts: Vec<KiroCredentialAttempt>,
    ) -> Option<ExternalPoolForwardOutcome> {
        let reason = classify_local_error_for_external_fallback(
            error_message,
            &classification_attempts,
            &self.config,
        )?;
        if self.config.local_pool_circuit_enabled {
            let mut seen_credentials = HashSet::new();
            let mut recorded = false;
            for attempt in &classification_attempts {
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
            "attemptCount": diagnostic_attempts.len(),
            "classificationAttemptCount": classification_attempts.len(),
        }));
        let route_subtype = if diagnostic_attempts.is_empty() {
            UsageRouteSubtype::ExternalFallbackPreflight
        } else {
            UsageRouteSubtype::ExternalFallbackAfterLocalAttempts
        };
        let route = match self.route_request(
            request_id.to_string(),
            route_subtype,
            Some(reason),
            None,
            true,
            local_preflight,
            diagnostic_attempts,
        ) {
            Ok(route) => route,
            Err(err) => {
                return Some(ExternalPoolForwardOutcome::Response(
                    payload_guard_error_response(err),
                ));
            }
        };
        Some(
            self.manager
                .forward_with_failover_result(self.config.clone(), route)
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
    ) -> Result<ExternalRouteRequest, PayloadGuardError> {
        Ok(ExternalRouteRequest {
            raw_body: self.raw_body.clone(),
            headers: self.headers.clone(),
            endpoint: self.endpoint.clone(),
            payload: Some(self.payload.clone()),
            body_mode_filter: None,
            model_hint: None,
            stream_hint: None,
            request_input_tokens: 0,
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
            prompt_cache_strategy_type: self.prompt_cache_strategy_type,
            prompt_cache_simulation_mode: self.prompt_cache_simulation_mode,
            prompt_cache_route_namespace: self.prompt_cache_route_namespace.clone(),
            prompt_cache_target_read_ratio: self.prompt_cache_target_read_ratio,
            prompt_cache_token_scale: self.prompt_cache_token_scale,
            prompt_cache_max_simulated_input_tokens: self.prompt_cache_max_simulated_input_tokens,
            prompt_cache_cap_jitter_min_tokens: self.prompt_cache_cap_jitter_min_tokens,
            prompt_cache_cap_jitter_max_tokens: self.prompt_cache_cap_jitter_max_tokens,
            prompt_cache_scale_min_input_tokens: self.prompt_cache_scale_min_input_tokens,
            prompt_cache_creation_control: self.prompt_cache_creation_control,
            prompt_cache_bounds: self.prompt_cache_bounds,
            kiro_rs_tool_cache_policy: self.kiro_rs_tool_cache_policy,
            model_capabilities: self.model_capabilities.clone(),
            pricing_catalog: self.pricing_catalog.clone(),
            request_id,
            error_id: self.error_id.clone(),
            recorder: self.recorder.clone(),
            started_at: Instant::now(),
            first_token_latency_ms: Arc::new(AtomicU64::new(0)),
            latency_trace: Arc::new(crate::external_pool::ExternalLatencyTraceState::default()),
            payload_breakdown: None,
            payload_guard_report: None,
            payload_guard_external_enabled: self.payload_guard_external_enabled,
            payload_guard_initial_config: self.payload_guard_initial_config,
            payload_guard_retry_config: self.payload_guard_retry_config,
        })
    }
}

fn local_pool_capacity_fail_fast_enabled(config: &ExternalPoolsConfig) -> bool {
    config.local_pool_preflight_enabled && config.fallback_on_local_capacity_exhausted
}

fn capacity_weight_units_for_local_request(provider: &KiroProvider, input_tokens: i32) -> u32 {
    let config = provider.runtime_config();
    if !config.weighted_capacity.enabled {
        return 1;
    }
    config
        .weighted_capacity
        .units_for_tokens(input_tokens.max(0) as u32)
        .clamp(1, 64)
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
        && (lower.contains("本地账号调度容量暂不可用")
            || lower.contains("本地凭据调度容量暂不可用")
            || lower.contains("账号调度等待队列已满")
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
        && (lower.contains("所有账号")
            || lower.contains("所有凭据")
            || lower.contains("所有可用账号")
            || lower.contains("所有可用凭据")
            || lower.contains("所有账号已用尽")
            || lower.contains("所有凭据已用尽")
            || lower.contains("无可用账号")
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
    fn elapsed_ms(&self) -> u64 {
        self.started_at.elapsed().as_millis().max(1) as u64
    }

    fn first_token_latency_ms(&self) -> Option<u64> {
        let value = self.first_token_latency_ms.load(Ordering::Acquire);
        (value > 0).then_some(value)
    }

    fn mark_payload_guard_latency(&mut self, elapsed: Duration) {
        self.latency.payload_guard_ms = Some(elapsed.as_millis().max(1) as u64);
    }

    fn set_capacity_weight_units(&self, units: u32) {
        self.capacity_weight_units
            .store(units.clamp(1, 64), Ordering::Release);
    }

    fn mark_upstream_header(&self) {
        let elapsed = self.elapsed_ms();
        let _ = self.latency.upstream_header_latency_ms.compare_exchange(
            0,
            elapsed,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    fn mark_first_upstream_chunk(&self) {
        if self.first_token_latency_ms.load(Ordering::Relaxed) > 0 {
            return;
        }
        self.latency
            .upstream_chunks_seen
            .fetch_add(1, Ordering::Relaxed);
        if self
            .latency
            .first_upstream_chunk_latency_ms
            .load(Ordering::Relaxed)
            > 0
        {
            return;
        }
        let elapsed = self.elapsed_ms();
        let _ = self
            .latency
            .first_upstream_chunk_latency_ms
            .compare_exchange(0, elapsed, Ordering::AcqRel, Ordering::Acquire);
    }

    fn has_first_output(&self) -> bool {
        self.first_token_latency_ms.load(Ordering::Acquire) > 0
    }

    fn mark_upstream_bytes_before_first_output(&self, byte_len: usize) {
        if self.has_first_output() {
            return;
        }
        saturating_fetch_add_u64(
            &self.latency.upstream_bytes_before_first_output,
            u64::try_from(byte_len).unwrap_or(u64::MAX),
        );
    }

    fn mark_upstream_pending_chunk_before_first_output(&self) {
        if self.has_first_output() {
            return;
        }
        saturating_fetch_add_u32(&self.latency.upstream_pending_chunks_before_first_output, 1);
    }

    fn mark_upstream_frame_before_first_output(&self) {
        if self.has_first_output() {
            return;
        }
        saturating_fetch_add_u32(&self.latency.upstream_frames_before_first_output, 1);
    }

    fn mark_upstream_event_before_first_output(&self, event: &Event, downstream_events_len: usize) {
        if self.has_first_output() {
            return;
        }

        saturating_fetch_add_u32(&self.latency.upstream_events_before_first_output, 1);
        if downstream_events_len == 0 {
            saturating_fetch_add_u32(
                &self
                    .latency
                    .upstream_frames_without_downstream_events_before_first_output,
                1,
            );
        }

        let kind = kiro_event_latency_kind(event);
        let mut counts = self.latency.upstream_event_types_before_first_output.lock();
        let entry = counts.entry(kind).or_insert(0);
        *entry = entry.saturating_add(1);
    }

    fn mark_upstream_frame_decode_error_before_first_output(&self) {
        if self.has_first_output() {
            return;
        }
        saturating_fetch_add_u32(
            &self
                .latency
                .upstream_frame_decode_errors_before_first_output,
            1,
        );
    }

    fn mark_upstream_event_parse_error_before_first_output(&self) {
        if self.has_first_output() {
            return;
        }
        saturating_fetch_add_u32(
            &self.latency.upstream_event_parse_errors_before_first_output,
            1,
        );
    }

    fn mark_stream_events(&self, events: &[SseEvent]) {
        self.mark_first_thinking_delta_if_output(events);
        self.mark_first_visible_text_delta_if_output(events);
        self.mark_first_token_if_output(events);
    }

    fn mark_first_thinking_delta_if_output(&self, events: &[SseEvent]) {
        if self
            .latency
            .first_thinking_delta_latency_ms
            .load(Ordering::Acquire)
            > 0
        {
            return;
        }
        if events.iter().any(is_thinking_delta_output_event) {
            let elapsed = self.elapsed_ms();
            let _ = self
                .latency
                .first_thinking_delta_latency_ms
                .compare_exchange(0, elapsed, Ordering::AcqRel, Ordering::Acquire);
        }
    }

    fn mark_first_visible_text_delta_if_output(&self, events: &[SseEvent]) {
        if self
            .latency
            .first_visible_text_delta_latency_ms
            .load(Ordering::Acquire)
            > 0
        {
            return;
        }
        if events.iter().any(is_visible_text_delta_output_event) {
            let elapsed = self.elapsed_ms();
            let _ = self
                .latency
                .first_visible_text_delta_latency_ms
                .compare_exchange(0, elapsed, Ordering::AcqRel, Ordering::Acquire);
        }
    }

    fn mark_stream_terminal(&self, reason: StreamTerminalReason) {
        let mut terminal_reason = self.latency.terminal_reason.lock();
        if terminal_reason.is_none() {
            *terminal_reason = Some(reason);
        }
    }

    fn set_downstream_stop_reason(&self, reason: impl Into<String>) {
        *self.downstream_stop_reason.lock() = Some(reason.into());
    }

    fn mark_client_dropped(&self) {
        let elapsed = self.elapsed_ms();
        let _ = self.latency.client_dropped_latency_ms.compare_exchange(
            0,
            elapsed,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        self.mark_stream_terminal(StreamTerminalReason::ClientDropped);
    }

    fn mark_first_token_if_output(&self, events: &[SseEvent]) {
        if self.first_token_latency_ms.load(Ordering::Acquire) > 0 {
            return;
        }

        let Some(index) = events.iter().position(is_first_token_output_event) else {
            if !events.is_empty() {
                saturating_fetch_add_u32(
                    &self.latency.events_seen_before_first_output,
                    u32::try_from(events.len()).unwrap_or(u32::MAX),
                );
            }
            return;
        };

        let elapsed = self.elapsed_ms();
        if self
            .first_token_latency_ms
            .compare_exchange(0, elapsed, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            let chunks_before = self
                .latency
                .upstream_chunks_seen
                .load(Ordering::Acquire)
                .saturating_sub(1);
            self.latency
                .chunks_before_first_output
                .store(chunks_before, Ordering::Release);
            let previous_events = self
                .latency
                .events_seen_before_first_output
                .load(Ordering::Acquire);
            let events_before =
                previous_events.saturating_add(u32::try_from(index).unwrap_or(u32::MAX));
            self.latency
                .events_before_first_output
                .store(events_before, Ordering::Release);
        }
    }

    fn latency_trace(&self) -> Option<UsageLatencyTrace> {
        let upstream_header_ms = load_latency_ms(&self.latency.upstream_header_latency_ms);
        let first_upstream_chunk_ms =
            load_latency_ms(&self.latency.first_upstream_chunk_latency_ms);
        let first_output_delta_ms = self.first_token_latency_ms();
        let first_thinking_delta_ms =
            load_latency_ms(&self.latency.first_thinking_delta_latency_ms);
        let first_visible_text_delta_ms =
            load_latency_ms(&self.latency.first_visible_text_delta_latency_ms);
        let stream_gap_to_first_output_ms = match (first_upstream_chunk_ms, first_output_delta_ms) {
            (Some(first_chunk), Some(first_output)) => {
                Some(first_output.saturating_sub(first_chunk))
            }
            _ => None,
        };
        let capacity_weight_units = self.capacity_weight_units.load(Ordering::Acquire);
        let include_upstream_diagnostics = first_upstream_chunk_ms.is_some();
        let upstream_event_types_before_first_output = {
            let counts = self.latency.upstream_event_types_before_first_output.lock();
            (!counts.is_empty()).then(|| {
                counts
                    .iter()
                    .map(|(kind, count)| ((*kind).to_string(), *count))
                    .collect()
            })
        };
        let trace = UsageLatencyTrace {
            capacity_weight_units: (capacity_weight_units > 1).then_some(capacity_weight_units),
            estimated_input_tokens: (capacity_weight_units > 1).then_some(self.input_tokens),
            payload_guard_ms: self.latency.payload_guard_ms,
            upstream_header_ms,
            first_upstream_chunk_ms,
            first_output_delta_ms,
            first_thinking_delta_ms,
            first_visible_text_delta_ms,
            stream_gap_to_first_output_ms,
            chunks_before_first_output: load_latency_counter(
                &self.latency.chunks_before_first_output,
            ),
            events_before_first_output: load_latency_counter(
                &self.latency.events_before_first_output,
            ),
            upstream_bytes_before_first_output: include_upstream_diagnostics.then_some(
                self.latency
                    .upstream_bytes_before_first_output
                    .load(Ordering::Acquire),
            ),
            upstream_frames_before_first_output: include_upstream_diagnostics.then_some(
                self.latency
                    .upstream_frames_before_first_output
                    .load(Ordering::Acquire),
            ),
            upstream_events_before_first_output: include_upstream_diagnostics.then_some(
                self.latency
                    .upstream_events_before_first_output
                    .load(Ordering::Acquire),
            ),
            upstream_frames_without_downstream_events_before_first_output:
                include_upstream_diagnostics.then_some(
                    self.latency
                        .upstream_frames_without_downstream_events_before_first_output
                        .load(Ordering::Acquire),
                ),
            upstream_pending_chunks_before_first_output: include_upstream_diagnostics.then_some(
                self.latency
                    .upstream_pending_chunks_before_first_output
                    .load(Ordering::Acquire),
            ),
            upstream_frame_decode_errors_before_first_output: include_upstream_diagnostics
                .then_some(
                    self.latency
                        .upstream_frame_decode_errors_before_first_output
                        .load(Ordering::Acquire),
                ),
            upstream_event_parse_errors_before_first_output: include_upstream_diagnostics
                .then_some(
                    self.latency
                        .upstream_event_parse_errors_before_first_output
                        .load(Ordering::Acquire),
                ),
            upstream_event_types_before_first_output,
            client_dropped_ms: load_latency_ms(&self.latency.client_dropped_latency_ms),
            terminal_reason: *self.latency.terminal_reason.lock(),
        };
        (!trace.is_empty()).then_some(trace)
    }

    fn cache_amplification(&self) -> Option<super::cache::CacheAmplification> {
        if self.prompt_cache_strategy_type != PromptCacheStrategyType::CurrentHighCache
            || self.simulation_mode != PromptCacheSimulationMode::HighCache
        {
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
            error_metadata: None,
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

    fn attach_tool_use_format_diagnostics(&mut self, diagnostics: ToolUseFormatDiagnostics) {
        if let Some(report) = self.payload_guard_report.as_mut() {
            report.tool_use_format_diagnostics = Some(diagnostics);
        }
    }

    fn attach_tool_format_debug_ref(&mut self, debug_ref: Option<serde_json::Value>) {
        let Some(debug_ref) = debug_ref else {
            return;
        };
        if let Some(report) = self.payload_guard_report.as_mut() {
            report.tool_format_debug_ref = Some(debug_ref);
        }
    }

    fn attach_cache_point_retry(&mut self, planned: usize, reason: &str) {
        if let Some(report) = self.payload_guard_report.as_mut() {
            report.kiro_cache_points_planned = report.kiro_cache_points_planned.max(planned);
            report.cache_point_retry_without_cache_point = true;
            report.cache_point_retry_reason = Some(reason.to_string());
        }
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
        self.reported_usage_for_downstream_with_raw(
            usage,
            usage_source,
            super::cache::RawUsage::uncached(self.input_tokens, usage.output_tokens),
        )
    }

    fn reported_usage_for_downstream_with_raw(
        &self,
        usage: super::cache::CacheUsage,
        usage_source: UsageSource,
        raw: super::cache::RawUsage,
    ) -> super::cache::CacheUsage {
        let usage = if usage_source == UsageSource::UpstreamMetadata {
            raw.to_cache_usage()
        } else {
            usage
        };
        let Some(policy) = self.reported_cache_usage_policy.clone() else {
            return usage;
        };
        let report_base = if self.uses_local_prompt_cache_strategy()
            && (usage_source == UsageSource::LocalPromptCache
                || super::cache::usage_has_cache(&usage))
        {
            usage
        } else {
            raw.to_cache_usage()
        };
        let mut reported =
            report_base.with_reported_cache_usage_policy_and_raw(policy.clone(), raw);
        reported = policy.apply_final_input_guard(reported);
        policy.apply_final_cache_read_guard(reported)
    }

    fn ensure_reported_usage_for_record(
        &self,
        usage: super::cache::CacheUsage,
        _usage_source: UsageSource,
    ) -> super::cache::CacheUsage {
        let Some(policy) = self.reported_cache_usage_policy.clone() else {
            return usage;
        };

        let raw = super::cache::RawUsage::uncached(self.input_tokens, usage.output_tokens);
        if self.uses_local_prompt_cache_strategy() && super::cache::usage_has_cache(&usage) {
            if policy.should_rewrite_local_prompt_cache_usage(usage) {
                usage.with_reported_cache_usage_policy_and_raw(policy, raw)
            } else {
                usage
            }
        } else {
            raw.to_cache_usage()
                .with_reported_cache_usage_policy_and_raw(policy, raw)
        }
    }

    fn ensure_reported_usage_for_record_with_raw(
        &self,
        usage: super::cache::CacheUsage,
        _usage_source: UsageSource,
        raw: super::cache::RawUsage,
    ) -> super::cache::CacheUsage {
        let Some(policy) = self.reported_cache_usage_policy.clone() else {
            return usage;
        };

        if self.uses_local_prompt_cache_strategy() && super::cache::usage_has_cache(&usage) {
            if policy.should_rewrite_local_prompt_cache_usage(usage) {
                usage.with_reported_cache_usage_policy_and_raw(policy, raw)
            } else {
                usage
            }
        } else {
            raw.to_cache_usage()
                .with_reported_cache_usage_policy_and_raw(policy, raw)
        }
    }

    fn uses_local_prompt_cache_strategy(&self) -> bool {
        matches!(
            self.prompt_cache_strategy_type,
            PromptCacheStrategyType::CurrentHighCache | PromptCacheStrategyType::KiroRsTool
        ) && (self.simulation_mode == PromptCacheSimulationMode::HighCache
            || self.kiro_rs_tool_prompt_cache_plan.is_some())
    }
}

fn is_first_token_output_event(event: &SseEvent) -> bool {
    if is_visible_text_delta_output_event(event) || is_thinking_delta_output_event(event) {
        return true;
    }
    match event.event.as_str() {
        "content_block_delta" => {
            let Some(delta) = event.data.get("delta").and_then(|value| value.as_object()) else {
                return false;
            };
            match delta.get("type").and_then(|value| value.as_str()) {
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

fn kiro_event_latency_kind(event: &Event) -> &'static str {
    match event {
        Event::AssistantResponse(_) => "assistant_response",
        Event::ToolUse(_) => "tool_use",
        Event::ReasoningContent(_) => "reasoning_content",
        Event::Metadata(_) => "metadata",
        Event::Metering(_) => "metering",
        Event::Code(_) => "code",
        Event::ContextUsage(_) => "context_usage",
        Event::MessageMetadata(_) => "message_metadata",
        Event::InvalidState(_) => "invalid_state",
        Event::Unknown {} => "unknown",
        Event::Error { .. } => "error",
        Event::Exception { .. } => "exception",
    }
}

fn is_visible_text_delta_output_event(event: &SseEvent) -> bool {
    if event.event != "content_block_delta" {
        return false;
    }
    event
        .data
        .get("delta")
        .and_then(|value| value.as_object())
        .filter(|delta| delta.get("type").and_then(|value| value.as_str()) == Some("text_delta"))
        .and_then(|delta| delta.get("text"))
        .and_then(|value| value.as_str())
        .is_some_and(|text| !text.is_empty())
}

fn is_thinking_delta_output_event(event: &SseEvent) -> bool {
    if event.event != "content_block_delta" {
        return false;
    }
    event
        .data
        .get("delta")
        .and_then(|value| value.as_object())
        .filter(|delta| {
            delta.get("type").and_then(|value| value.as_str()) == Some("thinking_delta")
        })
        .and_then(|delta| delta.get("thinking"))
        .and_then(|value| value.as_str())
        .is_some_and(|thinking| !thinking.is_empty())
}

#[cfg(test)]
fn reported_cache_usage_policy(
    strategy_type: PromptCacheStrategyType,
    simulation_mode: PromptCacheSimulationMode,
    reported_usage: &ReportedUsagePathPolicy,
    seed: u64,
) -> Option<super::cache::ReportedCacheUsagePolicy> {
    reported_cache_usage_policy_for_request(
        strategy_type,
        simulation_mode,
        reported_usage,
        seed,
        true,
    )
}

fn reported_cache_usage_policy_for_request(
    strategy_type: PromptCacheStrategyType,
    simulation_mode: PromptCacheSimulationMode,
    reported_usage: &ReportedUsagePathPolicy,
    seed: u64,
    stream: bool,
) -> Option<super::cache::ReportedCacheUsagePolicy> {
    if !should_apply_reported_usage(strategy_type, simulation_mode, reported_usage, stream) {
        return None;
    }

    super::cache::ReportedCacheUsagePolicy::from_path_policy(reported_usage.clone(), seed)
}

#[cfg(test)]
fn reported_cache_usage_policy_for_path(
    endpoint: &str,
    simulation_mode: PromptCacheSimulationMode,
    reported_usage: &ReportedUsageConfig,
    seed: u64,
) -> Option<super::cache::ReportedCacheUsagePolicy> {
    reported_cache_usage_policy(
        PromptCacheStrategyType::CurrentHighCache,
        simulation_mode,
        &reported_usage.policy_for_path(endpoint),
        seed,
    )
}

fn should_apply_reported_usage(
    strategy_type: PromptCacheStrategyType,
    simulation_mode: PromptCacheSimulationMode,
    reported_usage: &ReportedUsagePathPolicy,
    stream: bool,
) -> bool {
    if !reported_usage.enabled {
        return false;
    }
    if !stream && reported_usage.skip_non_stream_usage_projection {
        return false;
    }
    match strategy_type {
        PromptCacheStrategyType::NoCache => false,
        PromptCacheStrategyType::CurrentHighCache => {
            simulation_mode == PromptCacheSimulationMode::HighCache
        }
        PromptCacheStrategyType::KiroRsTool => false,
    }
}

fn should_build_local_prompt_cache_usage(
    strategy_type: PromptCacheStrategyType,
    simulation_mode: PromptCacheSimulationMode,
) -> bool {
    match strategy_type {
        PromptCacheStrategyType::NoCache => false,
        PromptCacheStrategyType::CurrentHighCache => {
            simulation_mode == PromptCacheSimulationMode::HighCache
        }
        PromptCacheStrategyType::KiroRsTool => true,
    }
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

fn raw_usage_to_reported_raw(usage: super::cache::CacheUsage) -> super::cache::RawUsage {
    super::cache::RawUsage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_creation_input_tokens: usage.cache_creation_input_tokens,
        cache_read_input_tokens: usage.cache_read_input_tokens,
        cache_creation_5m_input_tokens: usage.cache_creation_5m_input_tokens,
        cache_creation_1h_input_tokens: usage.cache_creation_1h_input_tokens,
    }
}

fn thinking_tokens_from_content(content: &[Value], output_tokens: i32) -> Option<i32> {
    let tokens = content
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("thinking"))
        .filter_map(|block| block.get("thinking").and_then(Value::as_str))
        .filter(|thinking| !thinking.is_empty())
        .map(|thinking| token::count_tokens(thinking) as i32)
        .fold(0_i32, i32::saturating_add);

    if tokens <= 0 {
        return None;
    }

    let output_tokens = output_tokens.max(0);
    if output_tokens > 0 {
        Some(tokens.min(output_tokens))
    } else {
        Some(tokens)
    }
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
    let (marker, marker_start) = message
        .rfind("账号 #")
        .map(|pos| ("账号 #", pos))
        .or_else(|| message.rfind("凭据 #").map(|pos| ("凭据 #", pos)))?;
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

fn log_provider_call_failure(message: &str, error_id: Option<&str>) {
    if let Some(hint) = extract_credential_error_hint(message) {
        tracing::warn!(
            error_id = ?error_id,
            credential_id = hint.id,
            credential_label = %hint.display_label(),
            error = %message,
            "模型请求失败"
        );
    } else {
        tracing::warn!(error_id = ?error_id, error = %message, "模型请求失败");
    }
}

fn log_provider_warning_with_hint(message: &str, reason: &'static str, error_id: Option<&str>) {
    if let Some(hint) = extract_credential_error_hint(message) {
        tracing::warn!(
            error_id = ?error_id,
            credential_id = hint.id,
            credential_label = %hint.display_label(),
            error = %message,
            "{}", reason
        );
    } else {
        tracing::warn!(error_id = ?error_id, error = %message, "{}", reason);
    }
}

fn log_provider_rate_limit_with_hint(message: &str, retry_after_secs: u64, error_id: Option<&str>) {
    if let Some(hint) = extract_credential_error_hint(message) {
        tracing::warn!(
            error_id = ?error_id,
            credential_id = hint.id,
            credential_label = %hint.display_label(),
            error = %message,
            retry_after_secs,
            "模型请求或本地账号调度临时不可用，返回 429"
        );
    } else {
        tracing::warn!(
            error_id = ?error_id,
            error = %message,
            retry_after_secs,
            "模型请求或本地账号调度临时不可用，返回 429"
        );
    }
}

fn log_provider_error_with_hint(message: &str, reason: &'static str, error_id: Option<&str>) {
    if let Some(hint) = extract_credential_error_hint(message) {
        tracing::error!(
            error_id = ?error_id,
            credential_id = hint.id,
            credential_label = %hint.display_label(),
            error = %message,
            "{}", reason
        );
    } else {
        tracing::error!(error_id = ?error_id, error = %message, "{}", reason);
    }
}

fn public_error_response(
    status: StatusCode,
    error_type: &'static str,
    message: impl Into<String>,
    request_id: Option<&str>,
    error_id: Option<&str>,
    extra_headers: impl IntoIterator<Item = (&'static str, String)>,
) -> Response {
    let request_id = request_id
        .map(str::to_string)
        .unwrap_or_else(envelope::request_id);
    let message = match error_id {
        Some(error_id) => envelope::public_message_with_error_id(&message.into(), error_id),
        None => message.into(),
    };
    let mut headers = extra_headers.into_iter().collect::<Vec<_>>();
    if let Some(error_id) = error_id {
        headers.push(("x-kiro-rs-error-id", error_id.to_string()));
    }
    envelope::error_response_with_id_and_headers(status, error_type, message, &request_id, headers)
}

impl CredentialUsageContext {
    fn with_error_metadata(mut self, error_metadata: Option<serde_json::Value>) -> Self {
        if error_metadata.is_some() {
            self.error_metadata = error_metadata;
        }
        self
    }

    fn scope(&self) -> Option<PromptCacheScope> {
        Some(PromptCacheScope::new(
            self.request.prompt_cache_scope_conversation_id.clone()?,
            self.request.prompt_cache_route_namespace.clone(),
        ))
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
        let usage = self.apply_creation_frequency_control(usage, usage_source);
        self.request
            .reported_usage_for_downstream(usage, usage_source)
    }

    fn final_reported_usage_for_success_with_raw(
        &self,
        usage: super::cache::CacheUsage,
        usage_source: UsageSource,
        raw_usage: super::cache::CacheUsage,
    ) -> super::cache::CacheUsage {
        let usage = self.apply_creation_frequency_control(usage, usage_source);
        self.request.reported_usage_for_downstream_with_raw(
            usage,
            usage_source,
            raw_usage_to_reported_raw(raw_usage),
        )
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

    fn canonical_reported_usage_for_success_with_raw(
        &self,
        usage: super::cache::CacheUsage,
        usage_source: UsageSource,
        raw_usage: super::cache::CacheUsage,
    ) -> super::cache::CacheUsage {
        let reported_usage =
            self.final_reported_usage_for_success_with_raw(usage, usage_source, raw_usage);
        self.request.ensure_reported_usage_for_record_with_raw(
            reported_usage,
            usage_source,
            raw_usage_to_reported_raw(raw_usage),
        )
    }

    fn apply_creation_frequency_control(
        &self,
        usage: super::cache::CacheUsage,
        usage_source: UsageSource,
    ) -> super::cache::CacheUsage {
        if usage_source != UsageSource::LocalPromptCache
            || self.request.simulation_mode != PromptCacheSimulationMode::HighCache
        {
            return usage;
        }

        let scope = self.scope();
        let credential_key = self
            .credential_id
            .map(|credential_id| format!("credential:{credential_id}"));
        let model = self.creation_control_model();
        self.request
            .prompt_cache_creation_controller
            .apply_success_with_context(
                scope.as_ref(),
                self.request.prompt_cache_creation_control,
                usage,
                credential_key.as_deref(),
                Some(model),
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
        let credential_key = self
            .credential_id
            .map(|credential_id| format!("credential:{credential_id}"));
        let model = self.creation_control_model();
        self.request
            .prompt_cache_creation_controller
            .preview_success_with_context(
                scope.as_ref(),
                self.request.prompt_cache_creation_control,
                reported_usage,
                credential_key.as_deref(),
                Some(model),
            )
    }

    fn creation_control_model(&self) -> &str {
        self.request
            .upstream_model
            .as_deref()
            .unwrap_or(self.request.model.as_str())
    }

    fn uses_local_prompt_cache_fallback(
        &self,
        metadata_usage: Option<&crate::kiro::model::events::MetadataTokenUsage>,
        usage: &super::cache::CacheUsage,
    ) -> bool {
        matches!(
            self.request.prompt_cache_strategy_type,
            PromptCacheStrategyType::CurrentHighCache | PromptCacheStrategyType::KiroRsTool
        ) && (self.request.prompt_cache_strategy_type == PromptCacheStrategyType::KiroRsTool
            || self.request.simulation_mode == PromptCacheSimulationMode::HighCache)
            && metadata_usage.is_some_and(super::cache::metadata_cache_is_empty)
            && self.request.simulated_source == Some(UsageSource::LocalPromptCache)
            && super::cache::usage_has_cache(usage)
    }

    fn record_success_from_stream(&self, ctx: &StreamContext) {
        let Some(usage) = ctx.final_usage() else {
            return;
        };
        self.request
            .set_downstream_stop_reason(ctx.downstream_stop_reason());
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
        self.record_success_reported_with_metering(
            reported_usage,
            usage_source,
            Some(raw_usage),
            ctx.kiro_metering_usage(),
        );
    }

    fn final_reported_usage_for_stream(
        &self,
        final_usage: super::cache::CacheUsage,
        metadata_usage: Option<&crate::kiro::model::events::MetadataTokenUsage>,
        context_estimated: bool,
    ) -> super::cache::CacheUsage {
        let usage_source = self.usage_source(&final_usage, metadata_usage, context_estimated);
        let raw_usage = raw_usage_from_metadata_or_estimate(
            metadata_usage,
            final_usage.input_tokens,
            final_usage.output_tokens,
        );
        self.canonical_reported_usage_for_success_with_raw(final_usage, usage_source, raw_usage)
    }

    fn record_stream_failure_from_context(
        &self,
        status: UsageRecordStatus,
        usage: Option<super::cache::CacheUsage>,
        error_detail: Option<(String, String)>,
        metadata_usage: Option<&crate::kiro::model::events::MetadataTokenUsage>,
        context_input_tokens: Option<i32>,
        kiro_metering_usage: Option<f64>,
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
        let public_error_message = match error_type.as_str() {
            "invalid_request_error" => envelope::PUBLIC_INVALID_REQUEST_MESSAGE,
            "rate_limit_error" => envelope::PUBLIC_RATE_LIMIT_MESSAGE,
            _ => envelope::PUBLIC_PROCESSING_FAILED_MESSAGE,
        };
        let public_error = Some(usage_public_error(
            StatusCode::OK,
            error_type.clone(),
            public_error_message,
            Some(&self.request.error_id),
        ));
        self.record(
            status,
            usage,
            source,
            Some(raw_usage),
            Some(error_type),
            Some(error_message),
            Some(error_detail),
            public_error,
            kiro_metering_usage,
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

    #[cfg(test)]
    fn record_success_reported(
        &self,
        usage: super::cache::CacheUsage,
        usage_source: UsageSource,
        raw_usage: Option<super::cache::CacheUsage>,
    ) {
        self.record_success_reported_with_metering(usage, usage_source, raw_usage, None);
    }

    fn record_success_reported_with_metering(
        &self,
        usage: super::cache::CacheUsage,
        usage_source: UsageSource,
        raw_usage: Option<super::cache::CacheUsage>,
        kiro_metering_usage: Option<f64>,
    ) {
        self.record(
            UsageRecordStatus::Success,
            usage,
            usage_source,
            raw_usage,
            None,
            None,
            None,
            None,
            kiro_metering_usage,
        );

        if usage_source != UsageSource::LocalPromptCache {
            return;
        }

        if let Some(scope) = self.scope() {
            match self.request.prompt_cache_strategy_type {
                PromptCacheStrategyType::NoCache => {}
                PromptCacheStrategyType::CurrentHighCache => {
                    self.request.prompt_cache.update_with_bounds(
                        Some(scope),
                        self.request.prompt_cache_profile.as_ref(),
                        self.request.prompt_cache_target_read_ratio,
                        self.request.prompt_cache_bounds,
                    );
                }
                PromptCacheStrategyType::KiroRsTool => {
                    if let Some(plan) = self.request.kiro_rs_tool_prompt_cache_plan.as_ref() {
                        self.request
                            .prompt_cache
                            .commit_kiro_rs_tool_success_with_bounds(
                                Some(scope),
                                plan,
                                self.request.prompt_cache_bounds,
                            );
                    }
                }
            }
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
        let public_error = if error_type == "client_dropped" {
            None
        } else {
            Some(provider_public_error_for_message(
                &error_message,
                Some(&self.request.error_id),
                None,
            ))
        };
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
            public_error,
            None,
        );
    }

    fn record_client_dropped(&self) {
        self.record_failure(
            UsageRecordStatus::ClientDropped,
            "client_dropped",
            "downstream client dropped before upstream stream completed",
        );
    }

    fn log_slow_interaction_diagnostic(
        &self,
        status: UsageRecordStatus,
        duration_ms: u64,
        latency_trace: Option<&UsageLatencyTrace>,
    ) {
        let first_visible_text_delta_ms =
            latency_trace.and_then(|trace| trace.first_visible_text_delta_ms);
        let stream_gap_to_first_output_ms =
            latency_trace.and_then(|trace| trace.stream_gap_to_first_output_ms);
        let events_before_first_output =
            latency_trace.and_then(|trace| trace.events_before_first_output);
        let slow_first_visible_text =
            first_visible_text_delta_ms.is_some_and(|value| value >= SLOW_FIRST_VISIBLE_TEXT_MS);
        let slow_stream_gap =
            stream_gap_to_first_output_ms.is_some_and(|value| value >= SLOW_STREAM_GAP_MS);
        let slow_response = duration_ms >= SLOW_RESPONSE_MS;
        let many_events_before_output = events_before_first_output
            .is_some_and(|value| value >= SLOW_EVENTS_BEFORE_FIRST_OUTPUT);

        if !slow_first_visible_text
            && !slow_stream_gap
            && !slow_response
            && !many_events_before_output
        {
            return;
        }

        let conversation_id_hash = self.request.conversation_id.as_deref().map(short_text_hash);
        tracing::warn!(
            request_id = %self.request.request_id,
            endpoint = %self.request.endpoint,
            stream = self.request.stream,
            status = ?status,
            requested_model = %self.request.model,
            upstream_model = ?self.request.upstream_model.as_deref(),
            conversation_id_hash = ?conversation_id_hash,
            credential_id = ?self.credential_id,
            route_subtype = ?self.request.route_subtype_override,
            duration_ms,
            first_token_latency_ms = ?self.request.first_token_latency_ms(),
            upstream_header_ms = ?latency_trace.and_then(|trace| trace.upstream_header_ms),
            first_upstream_chunk_ms = ?latency_trace.and_then(|trace| trace.first_upstream_chunk_ms),
            first_thinking_delta_ms = ?latency_trace.and_then(|trace| trace.first_thinking_delta_ms),
            first_visible_text_delta_ms = ?first_visible_text_delta_ms,
            stream_gap_to_first_output_ms = ?stream_gap_to_first_output_ms,
            events_before_first_output = ?events_before_first_output,
            chunks_before_first_output = ?latency_trace.and_then(|trace| trace.chunks_before_first_output),
            terminal_reason = ?latency_trace.and_then(|trace| trace.terminal_reason),
            slow_first_visible_text,
            slow_stream_gap,
            slow_response,
            many_events_before_output,
            "Kiro slow interaction diagnostic"
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
        public_error: Option<UsagePublicError>,
        kiro_metering_usage: Option<f64>,
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
        let duration_ms = self.request.started_at.elapsed().as_millis() as u64;
        let latency_trace = self.request.latency_trace();
        self.log_slow_interaction_diagnostic(status, duration_ms, latency_trace.as_ref());
        let raw_usage_snapshot = raw_usage.map(usage_snapshot);
        let diagnostic_total_input_tokens = if status == UsageRecordStatus::Success {
            raw_usage_snapshot
                .as_ref()
                .map(|usage| usage.total_input_tokens)
                .unwrap_or(self.request.input_tokens)
        } else {
            self.request.input_tokens
        };
        let error_source = match status {
            UsageRecordStatus::Success => None,
            UsageRecordStatus::ClientDropped => Some("downstream_client".to_string()),
            UsageRecordStatus::StreamError => Some("local_account_stream".to_string()),
            UsageRecordStatus::Error | UsageRecordStatus::UpstreamTimeout => {
                Some("local_account".to_string())
            }
        };
        let error_status_code = error_source.as_ref().and_then(|_| {
            self.credential_attempts
                .iter()
                .rev()
                .find_map(|attempt| attempt.status)
        });
        let error_id = error_source.as_ref().map(|_| self.request.error_id.clone());
        self.request.recorder.record(UsageRecord {
            id: self.request.request_id.clone(),
            created_at: Utc::now().to_rfc3339(),
            endpoint: self.request.endpoint.to_string(),
            stream: self.request.stream,
            model: self.request.model.clone(),
            upstream_model: self.request.upstream_model.clone(),
            external_outbound_model: None,
            model_resolution_source: self.request.model_resolution_source.clone(),
            model_resolution_note: self.request.model_resolution_note.clone(),
            requested_max_tokens: (self.request.requested_max_tokens > 0)
                .then_some(self.request.requested_max_tokens),
            downstream_stop_reason: self.request.downstream_stop_reason.lock().clone(),
            conversation_id: self.request.conversation_id.clone(),
            credential_id: self.credential_id,
            credential_label: self.credential_label.clone(),
            status,
            usage_source,
            raw_usage: raw_usage_snapshot,
            total_input_tokens: diagnostic_total_input_tokens,
            compat_input_tokens: usage.input_tokens,
            billable_input_tokens: usage.billable_input_tokens(),
            output_tokens: usage.output_tokens,
            cache_read_input_tokens: usage.cache_read_input_tokens,
            cache_creation_input_tokens: usage.cache_creation_input_tokens,
            cache_creation_5m_input_tokens: usage.cache_creation_5m_input_tokens,
            cache_creation_1h_input_tokens: usage.cache_creation_1h_input_tokens,
            estimated_cost_usd: pricing.cost_usd,
            kiro_metering_usage: kiro_metering_usage
                .filter(|usage| usage.is_finite())
                .unwrap_or(0.0),
            pricing_available: pricing.available,
            pricing_model: Some(pricing.model),
            duration_ms,
            first_token_latency_ms: self.request.first_token_latency_ms(),
            response_latency_ms: Some(duration_ms),
            latency_trace,
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
            error_status_code,
            error_source,
            error_id,
            error_metadata: self.error_metadata.clone(),
            public_error_status_code: public_error.as_ref().map(|error| error.status_code),
            public_error_type: public_error.as_ref().map(|error| error.error_type.clone()),
            public_error_message: public_error.map(|error| error.message),
            payload_breakdown,
            payload_guard_report,
        });
    }
}

fn provider_error_metadata(err: &Error) -> Option<serde_json::Value> {
    let selection_failure = KiroProvider::selection_failure_from_error(err)?;
    serde_json::to_value(json!({
        "selectionFailure": selection_failure
    }))
    .ok()
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
        || report.kiro_cache_points_planned > 0
        || report.kiro_cache_points_inserted > 0
        || report.cache_point_retry_without_cache_point
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
        self.usage_context.request.mark_client_dropped();
        self.usage_context.record_client_dropped();
    }
}

fn credential_label(provider: &crate::kiro::provider::KiroProvider, id: u64) -> Option<String> {
    provider.credential_label(id)
}

fn prepare_usage_context(
    state: &AppState,
    cache_route: ResolvedCacheRoutePolicy,
    endpoint: &str,
    stream: bool,
    payload: &MessagesRequest,
    model_resolution: Option<ModelResolution>,
    conversation_id: Option<String>,
    stable_conversation_id: Option<String>,
    input_tokens: i32,
) -> RequestUsageContext {
    let policy = cache_route.policy;
    let simulation_mode = prompt_cache_simulation_mode_for_policy(&policy);
    let strategy_type = policy.cache_type;
    let prompt_cache_model = model_resolution
        .as_ref()
        .and_then(|resolution| resolution.upstream_model.as_deref())
        .unwrap_or(&payload.model);
    let prompt_cache_supported = state
        .model_capabilities
        .supports_prompt_caching_for(prompt_cache_model)
        .unwrap_or(true);
    let scope = stable_conversation_id.as_ref().map(|conversation_id| {
        PromptCacheScope::new(conversation_id.clone(), cache_route.namespace.clone())
    });
    let (prompt_cache_profile, kiro_rs_tool_prompt_cache_plan) = match strategy_type {
        PromptCacheStrategyType::NoCache => (None, None),
        PromptCacheStrategyType::CurrentHighCache => match simulation_mode {
            PromptCacheSimulationMode::Disabled => (None, None),
            PromptCacheSimulationMode::HighCache if prompt_cache_supported => (
                state.prompt_cache.build_high_cache_profile_for_model(
                    payload,
                    input_tokens,
                    prompt_cache_model,
                ),
                None,
            ),
            PromptCacheSimulationMode::HighCache => (None, None),
        },
        PromptCacheStrategyType::KiroRsTool if prompt_cache_supported => (
            None,
            Some(state.prompt_cache.compute_kiro_rs_tool_with_bounds(
                scope.clone(),
                payload,
                input_tokens,
                prompt_cache_model,
                prompt_cache_bounds_for_policy(&policy),
                policy.kiro_rs_tool,
            )),
        ),
        PromptCacheStrategyType::KiroRsTool => (None, None),
    };
    let (simulated_usage, simulated_source) = match strategy_type {
        PromptCacheStrategyType::NoCache => (None, None),
        PromptCacheStrategyType::CurrentHighCache => build_simulated_usage(
            simulation_mode,
            stable_conversation_id.as_deref(),
            prompt_cache_profile.as_ref(),
        ),
        PromptCacheStrategyType::KiroRsTool => {
            let simulated_usage = kiro_rs_tool_prompt_cache_plan.as_ref().and_then(|plan| {
                let policy = policy.kiro_rs_tool.normalized();
                super::cache::CacheSimulation::from_prompt_cache_split_input_with_reported_input_range(
                    plan.usage(),
                    policy.reported_input_min_tokens,
                    policy.reported_input_max_tokens,
                    plan.cache_jitter_seed(),
                )
            });
            (
                simulated_usage,
                simulated_usage.map(|_| UsageSource::LocalPromptCache),
            )
        }
    };
    let request_id = envelope::request_id();
    let error_id = envelope::request_id();
    let reported_cache_creation_seed = prompt_cache_profile
        .as_ref()
        .map(|profile| profile.cache_jitter_seed())
        .unwrap_or(0)
        ^ fastrand::u64(..);
    let reported_cache_usage_policy = reported_cache_usage_policy_for_request(
        strategy_type,
        simulation_mode,
        &policy.reported_usage,
        reported_cache_creation_seed,
        stream,
    );

    RequestUsageContext {
        recorder: state.usage_recorder.clone(),
        tool_format_debug_recorder: state.tool_format_debug_recorder.clone(),
        prompt_cache: state.prompt_cache.clone(),
        prompt_cache_creation_controller: state.prompt_cache_creation_controller.clone(),
        pricing_catalog: state.pricing_catalog.clone(),
        request_id,
        error_id,
        endpoint: endpoint.to_string(),
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
        requested_max_tokens: payload.max_tokens.max(0),
        downstream_stop_reason: Arc::new(Mutex::new(None)),
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
        kiro_rs_tool_prompt_cache_plan,
        prompt_cache_route_namespace: cache_route.namespace,
        prompt_cache_strategy_type: strategy_type,
        simulation_mode,
        prompt_cache_target_read_ratio: policy.simulation.target_read_ratio,
        prompt_cache_token_scale: policy.simulation.token_scale,
        prompt_cache_max_simulated_input_tokens: policy.simulation.max_simulated_input_tokens,
        prompt_cache_cap_jitter_min_tokens: policy.simulation.cap_jitter_min_tokens,
        prompt_cache_cap_jitter_max_tokens: policy.simulation.cap_jitter_max_tokens,
        prompt_cache_scale_min_input_tokens: policy.simulation.scale_min_input_tokens,
        prompt_cache_creation_control: policy.creation_control,
        prompt_cache_bounds: prompt_cache_bounds_for_policy(&policy),
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
        capacity_weight_units: Arc::new(AtomicU32::new(1)),
        latency: RequestLatencyTraceState::new(),
    }
}

fn prompt_cache_scope_conversation_id(
    strategy_type: PromptCacheStrategyType,
    mode: PromptCacheSimulationMode,
    payload: &MessagesRequest,
) -> Option<String> {
    match strategy_type {
        PromptCacheStrategyType::NoCache => None,
        PromptCacheStrategyType::KiroRsTool => extract_stable_conversation_id(payload),
        PromptCacheStrategyType::CurrentHighCache => match mode {
            PromptCacheSimulationMode::Disabled => None,
            PromptCacheSimulationMode::HighCache => extract_stable_conversation_id(payload),
        },
    }
}

fn build_simulated_usage(
    simulation_mode: PromptCacheSimulationMode,
    conversation_id: Option<&str>,
    prompt_cache_profile: Option<&PromptCacheProfile>,
) -> (Option<super::cache::CacheSimulation>, Option<UsageSource>) {
    match simulation_mode {
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
            .map(|conversation_id| {
                let _ = credential_id;
                PromptCacheScope::new(
                    conversation_id.clone(),
                    usage_context.prompt_cache_route_namespace.clone(),
                )
            });
        let prompt_usage = usage_context.prompt_cache.compute_with_bounds(
            scope,
            usage_context.prompt_cache_profile.as_ref(),
            usage_context.prompt_cache_target_read_ratio,
            usage_context.prompt_cache_bounds,
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
    provider.cooldown_retry_after_hint_secs(fallback_secs)
}

fn public_message_with_optional_error_id(
    message: impl Into<String>,
    error_id: Option<&str>,
) -> String {
    let message = message.into();
    match error_id {
        Some(error_id) => envelope::public_message_with_error_id(&message, error_id),
        None => message,
    }
}

fn usage_public_error(
    status: StatusCode,
    error_type: impl Into<String>,
    message: impl Into<String>,
    error_id: Option<&str>,
) -> UsagePublicError {
    UsagePublicError {
        status_code: status.as_u16(),
        error_type: error_type.into(),
        message: public_message_with_optional_error_id(message, error_id),
    }
}

fn provider_public_error_for_message(
    err_str: &str,
    error_id: Option<&str>,
    provider: Option<&crate::kiro::provider::KiroProvider>,
) -> UsagePublicError {
    if is_upstream_payload_too_long_error(err_str) {
        return usage_public_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "Request input content length exceeded the request threshold. This limit is separate from the model context window. Reduce oversized tools, system prompt, documents, images, tool results, or conversation history.",
            error_id,
        );
    }

    if is_upstream_context_window_full_error(err_str) {
        return usage_public_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "Context window is full. Reduce conversation history, system prompt, tools, documents, images, or tool results.",
            error_id,
        );
    }

    if is_upstream_improperly_formed_error(err_str) || is_upstream_bad_request_error(err_str) {
        return usage_public_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            UPSTREAM_INVALID_REQUEST_MESSAGE,
            error_id,
        );
    }

    if is_upstream_invalid_model_error(err_str) {
        return usage_public_error(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            envelope::PUBLIC_MODEL_UNAVAILABLE_MESSAGE,
            error_id,
        );
    }

    if err_str.contains("临时冷却")
        || err_str.contains("本地限流")
        || err_str.contains("本地账号调度容量暂不可用")
        || err_str.contains("本地凭据调度容量暂不可用")
        || err_str.contains("账号调度等待队列已满")
        || err_str.contains("凭据调度等待队列已满")
        || err_str.contains("账号调度排队等待超时")
        || err_str.contains("凭据调度排队等待超时")
        || err_str.contains("暂不可调度")
        || err_str.contains("retry-after")
        || err_str.contains("Retry-After")
        || err_str.contains("429")
    {
        let retry_after_secs = retry_after_secs_from_error(err_str)
            .map(|secs| secs.max(1))
            .unwrap_or_else(|| cooldown_retry_after_secs(provider, 1));
        return usage_public_error(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_error",
            envelope::public_rate_limit_message(Some(retry_after_secs)),
            error_id,
        );
    }

    if err_str.contains("所有账号均已禁用")
        || err_str.contains("所有凭据均已禁用")
        || err_str.contains("所有账号均无法获取有效 Token")
        || err_str.contains("所有凭据均无法获取有效 Token")
        || err_str.contains("所有可用账号均因代理资源不可用")
        || err_str.contains("所有可用凭据均因代理资源不可用")
        || err_str.contains("所有账号已用尽")
        || err_str.contains("所有凭据已用尽")
        || err_str.contains("没有支持当前模型的可用账号")
        || err_str.contains("没有支持当前模型的可用凭据")
    {
        return usage_public_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "api_error",
            envelope::PUBLIC_ACCOUNT_UNAVAILABLE_MESSAGE,
            error_id,
        );
    }

    usage_public_error(
        StatusCode::BAD_GATEWAY,
        "api_error",
        envelope::PUBLIC_PROCESSING_FAILED_MESSAGE,
        error_id,
    )
}

fn map_provider_error(
    err: Error,
    request_id: Option<&str>,
    error_id: Option<&str>,
    provider: Option<&crate::kiro::provider::KiroProvider>,
) -> Response {
    let err_str = err.to_string();

    // Provider content length thresholds and model context windows are different limits.
    if is_upstream_payload_too_long_error(&err_str) {
        let message = "Request input content length exceeded the request threshold. This limit is separate from the model context window. Reduce oversized tools, system prompt, documents, images, tool results, or conversation history.";
        log_provider_warning_with_hint(&err_str, "请求被拒绝：输入内容长度超过接口阈值", error_id);
        return public_error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            message,
            request_id,
            error_id,
            std::iter::empty::<(&'static str, String)>(),
        );
    }

    if is_upstream_context_window_full_error(&err_str) {
        let message = "Context window is full. Reduce conversation history, system prompt, tools, documents, images, or tool results.";
        log_provider_warning_with_hint(&err_str, "请求被拒绝：上下文窗口已满", error_id);
        return public_error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            message,
            request_id,
            error_id,
            std::iter::empty::<(&'static str, String)>(),
        );
    }

    if is_upstream_improperly_formed_error(&err_str) {
        log_provider_warning_with_hint(
            &err_str,
            "请求被拒绝：Kiro payload 形态不合法（不应切换账号重试）",
            error_id,
        );
        return public_error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            UPSTREAM_INVALID_REQUEST_MESSAGE,
            request_id,
            error_id,
            std::iter::empty::<(&'static str, String)>(),
        );
    }

    if is_upstream_invalid_model_error(&err_str) {
        log_provider_warning_with_hint(
            &err_str,
            "请求被拒绝：上游模型不支持（不应切换账号重试）",
            error_id,
        );
        let message = envelope::PUBLIC_MODEL_UNAVAILABLE_MESSAGE;
        return public_error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            message,
            request_id,
            error_id,
            std::iter::empty::<(&'static str, String)>(),
        );
    }

    if is_upstream_bad_request_error(&err_str) {
        log_provider_warning_with_hint(
            &err_str,
            "请求被上游以 400 拒绝（不应切换账号重试）",
            error_id,
        );
        return public_error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            UPSTREAM_INVALID_REQUEST_MESSAGE,
            request_id,
            error_id,
            std::iter::empty::<(&'static str, String)>(),
        );
    }

    if err_str.contains("临时冷却")
        || err_str.contains("本地限流")
        || err_str.contains("本地账号调度容量暂不可用")
        || err_str.contains("本地凭据调度容量暂不可用")
        || err_str.contains("账号调度等待队列已满")
        || err_str.contains("凭据调度等待队列已满")
        || err_str.contains("账号调度排队等待超时")
        || err_str.contains("凭据调度排队等待超时")
        || err_str.contains("暂不可调度")
        || err_str.contains("retry-after")
        || err_str.contains("Retry-After")
        || err_str.contains("429")
    {
        let retry_after_secs = retry_after_secs_from_error(&err_str)
            .map(|secs| secs.max(1))
            .unwrap_or_else(|| cooldown_retry_after_secs(provider, 1));
        log_provider_rate_limit_with_hint(&err_str, retry_after_secs, error_id);
        let message = envelope::public_rate_limit_message(Some(retry_after_secs));
        return public_error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_error",
            message,
            request_id,
            error_id,
            [("retry-after", retry_after_secs.to_string())],
        );
    }

    if err_str.contains("所有账号均已禁用")
        || err_str.contains("所有凭据均已禁用")
        || err_str.contains("所有账号均无法获取有效 Token")
        || err_str.contains("所有凭据均无法获取有效 Token")
        || err_str.contains("所有可用账号均因代理资源不可用")
        || err_str.contains("所有可用凭据均因代理资源不可用")
        || err_str.contains("所有账号已用尽")
        || err_str.contains("所有凭据已用尽")
        || err_str.contains("没有支持当前模型的可用账号")
        || err_str.contains("没有支持当前模型的可用凭据")
    {
        log_provider_error_with_hint(&err_str, "没有可调度账号", error_id);
        return public_error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "api_error",
            envelope::PUBLIC_ACCOUNT_UNAVAILABLE_MESSAGE,
            request_id,
            error_id,
            std::iter::empty::<(&'static str, String)>(),
        );
    }

    log_provider_error_with_hint(&err_str, "Kiro API 调用失败", error_id);
    public_error_response(
        StatusCode::BAD_GATEWAY,
        "api_error",
        envelope::PUBLIC_PROCESSING_FAILED_MESSAGE,
        request_id,
        error_id,
        std::iter::empty::<(&'static str, String)>(),
    )
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

fn is_upstream_tool_use_format_error(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("invalid tool use format") || lower.contains("request_body_invalid")
}

fn is_upstream_bad_request_error(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("400 bad request")
        || lower.contains("bad_request")
        || lower.contains("request_body_invalid")
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

fn should_retry_without_cache_point_after_error(value: &str) -> bool {
    is_upstream_tool_use_format_error(value)
        || is_upstream_improperly_formed_error(value)
        || is_upstream_bad_request_error(value)
}

fn attach_and_log_tool_use_format_diagnostics(
    message: &str,
    attempted_body: &str,
    request: &KiroRequest,
    usage_context: &mut RequestUsageContext,
    endpoint: &str,
    requested_model: &str,
    upstream_model: &str,
) {
    if !is_upstream_tool_use_format_error(message) {
        return;
    }

    let diagnostics = diagnose_kiro_tool_use_format(request);
    usage_context.attach_tool_use_format_diagnostics(diagnostics);
    let debug_ref = usage_context
        .tool_format_debug_recorder
        .record(ToolFormatDebugEvent {
            request_id: &usage_context.request_id,
            endpoint,
            stream: usage_context.stream,
            requested_model,
            upstream_model: Some(upstream_model),
            error_message: message,
            attempted_body: Some(attempted_body),
            request,
            report: usage_context.payload_guard_report.as_ref(),
            diagnostics,
        });
    usage_context.attach_tool_format_debug_ref(debug_ref);

    tracing::debug!(
        request_id = %usage_context.request_id,
        endpoint,
        requested_model,
        upstream_model,
        has_tool_payload = diagnostics.has_tool_payload(),
        history_entries_total = diagnostics.history_entries_total,
        history_entries_scanned = diagnostics.history_entries_scanned,
        scan_truncated = diagnostics.scan_truncated,
        tool_items_scanned = diagnostics.tool_items_scanned,
        tool_item_scan_truncated = diagnostics.tool_item_scan_truncated,
        current_tool_count = diagnostics.current_tool_count,
        current_tool_result_count = diagnostics.current_tool_result_count,
        history_tool_use_count = diagnostics.history_tool_use_count,
        history_tool_result_count = diagnostics.history_tool_result_count,
        last_assistant_tool_use_count = diagnostics.last_assistant_tool_use_count,
        current_results_matching_last_assistant = diagnostics.current_results_matching_last_assistant,
        current_results_not_matching_last_assistant = diagnostics.current_results_not_matching_last_assistant,
        duplicate_current_tool_result_ids = diagnostics.duplicate_current_tool_result_ids,
        duplicate_history_tool_use_ids = diagnostics.duplicate_history_tool_use_ids,
        duplicate_history_tool_result_ids = diagnostics.duplicate_history_tool_result_ids,
        duplicate_tool_names = diagnostics.duplicate_tool_names,
        empty_tool_names = diagnostics.empty_tool_names,
        empty_tool_use_ids = diagnostics.empty_tool_use_ids,
        empty_tool_result_ids = diagnostics.empty_tool_result_ids,
        non_object_tool_use_inputs = diagnostics.non_object_tool_use_inputs,
        history_tool_names_missing_from_tools = diagnostics.history_tool_names_missing_from_tools,
        "Kiro rejected tool-use format; attached redacted request-structure diagnostics"
    );
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
        ConversionError::UnsupportedModel(model) => (
            "invalid_request_error",
            format!("The requested model is not available: {}", model),
        ),
        ConversionError::EmptyMessages => (
            "invalid_request_error",
            "messages: at least one message is required".to_string(),
        ),
        ConversionError::UnsupportedContent(message) => ("invalid_request_error", message.clone()),
    };
    envelope::error_response(StatusCode::BAD_REQUEST, error_type, message)
}

fn resolve_request_model(
    state: &AppState,
    runtime_config: &RequestRuntimeConfig,
    endpoint: &str,
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
            envelope::PUBLIC_MODEL_UNAVAILABLE_MESSAGE,
        ));
    }

    if let Some(upstream_model) = resolution.upstream_model.as_deref() {
        let (_, requested_thinking) = strip_model_compat_suffixes(&resolution.requested_model);
        let (_, upstream_thinking) = strip_model_compat_suffixes(upstream_model);
        if requested_thinking && !upstream_thinking {
            tracing::info!(
                endpoint,
                requested_model = %resolution.requested_model,
                upstream_model = %upstream_model,
                resolution = %resolution.source.as_str(),
                thinking_transport = "base_model_with_thinking_controls",
                "上游目录未提供对应 thinking 模型，使用基础模型并保留 thinking 控制"
            );
        } else if requested_thinking && upstream_thinking {
            tracing::info!(
                endpoint,
                requested_model = %resolution.requested_model,
                upstream_model = %upstream_model,
                resolution = %resolution.source.as_str(),
                thinking_transport = "thinking_model",
                "使用上游 thinking 模型"
            );
        }
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
            envelope::error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "api_error",
                envelope::PUBLIC_PROCESSING_FAILED_MESSAGE,
            )
        }
        PayloadGuardError::OversizedImage {
            current_images,
            current_image_bytes,
            historical_images,
            historical_image_bytes,
            max_source_bytes,
        } => {
            tracing::warn!(
                current_images,
                current_image_bytes,
                historical_images,
                historical_image_bytes,
                max_source_bytes,
                "request contains image input exceeding upstream image size limit"
            );
            envelope::error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "One or more images exceed the upstream 5 MB image size limit. Remove or resize the oversized image and retry.",
            )
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
    if report.kiro_cache_points_planned > 0 || report.kiro_cache_points_inserted > 0 {
        tracing::debug!(
            endpoint,
            requested_model,
            upstream_model,
            conversation_id,
            cache_points_planned = report.kiro_cache_points_planned,
            cache_points_inserted = report.kiro_cache_points_inserted,
            "Kiro cachePoint insertion plan applied"
        );
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
            flattened_history_tool_uses = report.flattened_history_tool_uses,
            textified_history_tool_results = report.textified_history_tool_results,
            removed_history_tools = report.removed_history_tools,
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

fn resolve_defined_cache_route(state: &AppState, route: &str) -> Result<String, Response> {
    let candidate = format!("/dfcache/{route}");
    let Some(prefix) = normalize_defined_cache_route(&candidate) else {
        return Err(envelope::error_response(
            StatusCode::NOT_FOUND,
            "not_found_error",
            format!("dfcache route is invalid: {candidate}"),
        ));
    };
    let defined_cache_routes = state
        .kiro_provider
        .as_ref()
        .map(|provider| {
            normalize_defined_cache_routes(&provider.runtime_config().defined_cache_routes)
        })
        .unwrap_or_else(|| state.defined_cache_routes.clone());
    if !defined_cache_routes.iter().any(|item| item == &prefix) {
        return Err(envelope::error_response(
            StatusCode::NOT_FOUND,
            "not_found_error",
            format!("dfcache route is not configured: {prefix}"),
        ));
    }
    Ok(prefix)
}

/// GET /dfcache/:route/v1/models
pub async fn get_models_dfcache(
    State(state): State<AppState>,
    Path(route): Path<String>,
) -> Response {
    if let Err(response) = resolve_defined_cache_route(&state, &route) {
        return response;
    }

    let models = state.model_capabilities.anthropic_models();
    Json(ModelsResponse {
        object: "list".to_string(),
        data: models,
    })
    .into_response()
}

/// POST /v1/messages
///
/// 创建消息（对话）
pub async fn post_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    raw_body: Bytes,
) -> Response {
    post_messages_for_endpoint(state, headers, raw_body, "/v1/messages".to_string()).await
}

/// POST /na/v1/messages
///
/// 创建消息（对话），默认不进入本地 prompt-cache 模拟，直接使用原始 usage。
pub async fn post_messages_real_cache_usage(
    State(state): State<AppState>,
    headers: HeaderMap,
    raw_body: Bytes,
) -> Response {
    post_messages_for_endpoint(state, headers, raw_body, "/na/v1/messages".to_string()).await
}

/// POST /ha/v1/messages
///
/// 创建消息（对话），使用 high-cache 计算；下游 usage 上报由 `/ha` 路径覆盖项独立控制。
pub async fn post_messages_ha(
    State(state): State<AppState>,
    headers: HeaderMap,
    raw_body: Bytes,
) -> Response {
    post_messages_for_endpoint(state, headers, raw_body, "/ha/v1/messages".to_string()).await
}

/// POST /dfcache/:route/v1/messages
///
/// 自定义 high-cache 路由。必须先在运行配置中定义 `/dfcache/{route}`。
pub async fn post_messages_dfcache(
    State(state): State<AppState>,
    Path(route): Path<String>,
    headers: HeaderMap,
    raw_body: Bytes,
) -> Response {
    let prefix = match resolve_defined_cache_route(&state, &route) {
        Ok(prefix) => prefix,
        Err(response) => return response,
    };
    let endpoint = format!("{prefix}/v1/messages");
    post_messages_for_endpoint(state, headers, raw_body, endpoint).await
}

async fn post_messages_for_endpoint(
    state: AppState,
    headers: HeaderMap,
    raw_body: Bytes,
    endpoint: String,
) -> Response {
    request_entry::handle_messages_endpoint(state, headers, raw_body, endpoint).await
}

async fn post_messages_inner(
    state: AppState,
    headers: HeaderMap,
    raw_body: Bytes,
    mut payload: MessagesRequest,
    endpoint: String,
) -> Response {
    tracing::debug!(
        endpoint = endpoint,
        model = %payload.model,
        max_tokens = %payload.max_tokens,
        stream = %payload.stream,
        message_count = %payload.messages.len(),
        "Received POST messages request"
    );
    log_anthropic_request_summary(&endpoint, &payload);
    // 检查 KiroProvider 是否可用
    let provider = match &state.kiro_provider {
        Some(p) => p.clone(),
        None => {
            tracing::error!("KiroProvider 未配置");
            return envelope::error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "api_error",
                envelope::PUBLIC_PROVIDER_NOT_READY_MESSAGE,
            );
        }
    };
    let runtime_config = request_runtime_config(&state, &provider);
    let cache_route = runtime_config.cache_policy_for_path(&endpoint);
    let mut external_fallback = build_external_fallback_context(
        &state,
        &runtime_config,
        &cache_route,
        &endpoint,
        raw_body,
        headers.clone(),
        &payload,
    );
    let parsed_body_plan =
        ParsedAnthropicBodyPlan::shared_compatible(runtime_config.image_processing);
    if let Err(response) = parsed_body_pipeline::prepare(
        &state,
        &headers,
        &endpoint,
        &mut payload,
        &runtime_config,
        parsed_body_plan,
    )
    .await
    {
        return response;
    }

    if let Some(external) = external_fallback.as_mut() {
        external.refresh_payload(&payload);
    }

    if let Some(external) = external_fallback.as_ref() {
        let request_id = envelope::request_id();
        if let Some(response) = external.direct_policy_response(&request_id).await {
            return response;
        }
    }

    let model_resolution = match resolve_request_model(&state, &runtime_config, &endpoint, &payload)
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

    // 检查是否为 WebSearch 请求
    if websearch::has_web_search_tool(&payload) {
        if !websearch_supported_for_profile(runtime_config.compat_profile) {
            return envelope::error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request_error",
                "The web_search tool is not supported for this request.",
            );
        }
        tracing::info!("检测到 WebSearch 工具，路由到 WebSearch 处理");

        // 估算输入 tokens
        let input_tokens = token::count_all_tokens(
            &payload.model,
            payload.system.as_deref(),
            &payload.messages,
            payload.tools.as_deref(),
        ) as i32;

        return websearch::handle_websearch_request(provider, &payload, input_tokens).await;
    }

    let local_cache_route = cache_route_for_request_stream(cache_route, payload.stream);
    let request_simulation_mode =
        prompt_cache_simulation_mode_for_policy(&local_cache_route.policy);
    let cache_type = local_cache_route.policy.cache_type;
    let prepared_local = match local_body_pipeline::prepare(
        &endpoint,
        &payload,
        &runtime_config,
        &local_cache_route,
        &model_resolution,
    ) {
        Ok(prepared) => prepared,
        Err(response) => return response,
    };
    let local_body_pipeline::PreparedLocalKiroBody {
        request_body,
        kiro_request,
        conversation_id,
        input_tokens,
        payload_breakdown,
        payload_guard_report,
        payload_guard_elapsed,
        thinking_enabled,
        tool_name_map,
        known_tool_names,
        warnings_header,
        extract_xml_thinking,
        too_long_retry,
        cache_point_retry,
    } = prepared_local;

    let mut usage_context = prepare_usage_context(
        &state,
        local_cache_route,
        &endpoint,
        payload.stream,
        &payload,
        Some(model_resolution.clone()),
        Some(conversation_id.clone()),
        prompt_cache_scope_conversation_id(cache_type, request_simulation_mode, &payload),
        input_tokens,
    );
    if let Some(report) = payload_guard_report.clone() {
        usage_context = usage_context.with_payload_diagnostics(payload_breakdown, report);
    }
    if let Some(elapsed) = payload_guard_elapsed {
        usage_context.mark_payload_guard_latency(elapsed);
    }
    let capacity_weight_units =
        capacity_weight_units_for_local_request(provider.as_ref(), input_tokens);
    usage_context.set_capacity_weight_units(capacity_weight_units);

    if payload.stream {
        let claude_code_noop_delta_keepalive =
            should_use_claude_code_noop_delta_keepalive(request_user_agent(&headers));
        // 流式响应
        handle_stream_request(
            provider,
            &request_body,
            &kiro_request,
            &payload.model,
            model_resolution
                .upstream_model
                .as_deref()
                .unwrap_or(&payload.model),
            payload.max_tokens,
            input_tokens,
            usage_context.context_window_tokens,
            thinking_enabled,
            extract_xml_thinking,
            tool_name_map,
            known_tool_names,
            usage_context,
            warnings_header,
            too_long_retry,
            cache_point_retry,
            external_fallback,
            runtime_config.kiro_upstream_stream_idle_timeout_secs,
            capacity_weight_units,
            claude_code_noop_delta_keepalive,
        )
        .await
    } else {
        // 非流式响应：仅在配置开启时提取 thinking 块
        let extract_thinking = should_extract_unsigned_thinking(&runtime_config, thinking_enabled);
        handle_non_stream_request(
            provider,
            &request_body,
            &kiro_request,
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
            cache_point_retry,
            external_fallback,
            capacity_weight_units,
        )
        .await
    }
}

async fn call_api_stream_maybe_fail_fast(
    provider: &Arc<KiroProvider>,
    request_body: &str,
    request_id: Option<&str>,
    external_fallback: Option<&ExternalFallbackContext>,
    capacity_weight_units: u32,
) -> anyhow::Result<crate::kiro::provider::KiroStreamResponse> {
    if let Some(external) = external_fallback {
        if external.should_fail_fast_local().await {
            return provider
                .call_api_stream_with_request_id_fail_fast_and_capacity_weight(
                    request_body,
                    request_id,
                    capacity_weight_units,
                )
                .await;
        }
    }
    provider
        .call_api_stream_with_request_id_and_capacity_weight(
            request_body,
            request_id,
            capacity_weight_units,
        )
        .await
}

async fn call_api_maybe_fail_fast(
    provider: &Arc<KiroProvider>,
    request_body: &str,
    request_id: Option<&str>,
    external_fallback: Option<&ExternalFallbackContext>,
    capacity_weight_units: u32,
) -> anyhow::Result<crate::kiro::provider::KiroApiResponse> {
    if let Some(external) = external_fallback {
        if external.should_fail_fast_local().await {
            return provider
                .call_api_with_context_with_request_id_fail_fast_and_capacity_weight(
                    request_body,
                    request_id,
                    capacity_weight_units,
                )
                .await;
        }
    }
    provider
        .call_api_with_context_with_request_id_and_capacity_weight(
            request_body,
            request_id,
            capacity_weight_units,
        )
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

async fn maybe_external_fallback_after_local_error_outcome_with_diagnostics(
    external_fallback: Option<&ExternalFallbackContext>,
    request_id: &str,
    message: &str,
    classification_attempts: Vec<KiroCredentialAttempt>,
    diagnostic_attempts: Vec<KiroCredentialAttempt>,
) -> Option<ExternalPoolForwardOutcome> {
    external_fallback?
        .fallback_after_local_error_outcome_with_diagnostics(
            request_id,
            message,
            classification_attempts,
            diagnostic_attempts,
        )
        .await
}

async fn maybe_local_pool_preflight_external_response(
    external_fallback: Option<&ExternalFallbackContext>,
    provider: &KiroProvider,
    request_id: &str,
    model: Option<&str>,
) -> Option<Response> {
    let outcome = external_fallback?
        .local_pool_preflight_outcome(provider, request_id, model)
        .await?;
    Some(match outcome {
        ExternalPoolForwardOutcome::Response(response) => response,
        ExternalPoolForwardOutcome::FinalError(err) => err.into_response(request_id),
    })
}

fn local_rescue_reason_after_external_error(
    config: &ExternalPoolsConfig,
    err: &ExternalPoolFinalError,
    _local_fallback_reason: Option<&str>,
) -> Option<&'static str> {
    if config.external_direct_policy_enabled {
        return None;
    }
    if !config.external_pool_local_rescue_enabled {
        return None;
    }
    if err.is_rate_limit() {
        return config
            .external_pool_local_rescue_on_rate_limit
            .then_some("external_rate_limit");
    }
    if err.is_timeout_like() {
        return config
            .external_pool_local_rescue_on_timeout
            .then_some("external_timeout");
    }
    if err.is_capacity_like() {
        return config
            .external_pool_local_rescue_on_capacity
            .then_some("external_capacity");
    }
    if err.is_public_invalid_request() {
        return Some("external_bad_request");
    }
    Some("external_error")
}

/// Last-chance local retry after an external-pool final error.
///
/// This deliberately calls the provider-local `*_max_wait` entrypoint directly. Do not route this
/// through `call_api_stream_maybe_fail_fast`, `call_api_maybe_fail_fast`, or
/// `ExternalFallbackContext::*fallback*`; those paths can choose the external pool again.
async fn call_stream_local_rescue_after_external_error(
    provider: &Arc<KiroProvider>,
    request_body: &str,
    request_id: &str,
    external: &ExternalFallbackContext,
    capacity_weight_units: u32,
) -> anyhow::Result<crate::kiro::provider::KiroStreamResponse> {
    provider
        .call_api_stream_with_request_id_max_wait_and_capacity_weight(
            request_body,
            Some(request_id),
            Duration::from_secs(external.config.external_pool_local_rescue_max_wait_secs),
            capacity_weight_units,
        )
        .await
}

/// Last-chance local retry after an external-pool final error.
///
/// This deliberately calls the provider-local `*_max_wait` entrypoint directly. Do not route this
/// through `call_api_stream_maybe_fail_fast`, `call_api_maybe_fail_fast`, or
/// `ExternalFallbackContext::*fallback*`; those paths can choose the external pool again.
async fn call_non_stream_local_rescue_after_external_error(
    provider: &Arc<KiroProvider>,
    request_body: &str,
    request_id: &str,
    external: &ExternalFallbackContext,
    capacity_weight_units: u32,
) -> anyhow::Result<crate::kiro::provider::KiroApiResponse> {
    provider
        .call_api_with_context_with_request_id_max_wait_and_capacity_weight(
            request_body,
            Some(request_id),
            Duration::from_secs(external.config.external_pool_local_rescue_max_wait_secs),
            capacity_weight_units,
        )
        .await
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
    kiro_request: &KiroRequest,
    model: &str,
    preflight_model: &str,
    requested_max_tokens: i32,
    input_tokens: i32,
    context_window_tokens: i32,
    thinking_enabled: bool,
    extract_xml_thinking: bool,
    tool_name_map: HashMap<String, String>,
    known_tool_names: HashSet<String>,
    usage_context: RequestUsageContext,
    warnings_header: Option<String>,
    too_long_retry: Option<PayloadTooLongRetryRequest>,
    cache_point_retry: Option<CachePointRetryRequest>,
    external_fallback: Option<ExternalFallbackContext>,
    stream_idle_timeout_secs: u64,
    capacity_weight_units: u32,
    claude_code_noop_delta_keepalive: bool,
) -> Response {
    // 调用 Kiro API（支持多凭据故障转移）
    let mut usage_context = usage_context;
    let mut warnings_header = warnings_header;
    let request_id = usage_context.request_id.clone();
    let mut retry_attempt_prefix: Vec<KiroCredentialAttempt> = Vec::new();
    if let Some(response) = maybe_local_pool_preflight_external_response(
        external_fallback.as_ref(),
        provider.as_ref(),
        &request_id,
        Some(preflight_model),
    )
    .await
    {
        return response;
    }
    let response = match call_api_stream_maybe_fail_fast(
        &provider,
        request_body,
        Some(&request_id),
        external_fallback.as_ref(),
        capacity_weight_units,
    )
    .await
    {
        Ok(resp) => resp,
        Err(e) => {
            let message = e.to_string();
            let attempts = KiroProvider::attempts_from_error(&e);
            log_provider_call_failure(&message, Some(&usage_context.error_id));
            let endpoint = usage_context.endpoint.clone();
            attach_and_log_tool_use_format_diagnostics(
                &message,
                request_body,
                kiro_request,
                &mut usage_context,
                &endpoint,
                model,
                preflight_model,
            );
            let should_payload_guard_retry = too_long_retry.as_ref().is_some_and(|retry| {
                should_retry_payload_guard_after_error(
                    &message,
                    request_body.len(),
                    retry.config.max_bytes,
                )
            });
            if let Some(retry) = cache_point_retry.filter(|_| {
                !should_payload_guard_retry
                    && should_retry_without_cache_point_after_error(&message)
            }) {
                retry_attempt_prefix = attempts.clone();
                let retry_body = match retry.build_retry_body(&message, &mut usage_context) {
                    Ok(result) => result,
                    Err(err) => {
                        usage_context
                            .attach_provider_error_credential(&provider, &message, attempts)
                            .with_error_metadata(provider_error_metadata(&e))
                            .record_failure(
                                UsageRecordStatus::Error,
                                "payload_guard_error",
                                format!(
                                    "cachePoint retry failed while building fallback payload: {}",
                                    err
                                ),
                            );
                        return payload_guard_error_response(err);
                    }
                };
                match call_api_stream_maybe_fail_fast(
                    &provider,
                    &retry_body,
                    Some(&request_id),
                    external_fallback.as_ref(),
                    capacity_weight_units,
                )
                .await
                {
                    Ok(resp) => resp,
                    Err(retry_error) => {
                        let retry_message = retry_error.to_string();
                        let retry_attempts = KiroProvider::attempts_from_error(&retry_error);
                        let classification_attempts = retry_attempts.clone();
                        let all_attempts =
                            merge_credential_attempts(retry_attempt_prefix.clone(), retry_attempts);
                        log_provider_call_failure(&retry_message, Some(&usage_context.error_id));
                        let endpoint = usage_context.endpoint.clone();
                        attach_and_log_tool_use_format_diagnostics(
                            &retry_message,
                            &retry_body,
                            kiro_request,
                            &mut usage_context,
                            &endpoint,
                            model,
                            preflight_model,
                        );
                        if let Some(outcome) =
                            maybe_external_fallback_after_local_error_outcome_with_diagnostics(
                                external_fallback.as_ref(),
                                &request_id,
                                &retry_message,
                                classification_attempts,
                                all_attempts.clone(),
                            )
                            .await
                        {
                            match outcome {
                                ExternalPoolForwardOutcome::Response(response) => return response,
                                ExternalPoolForwardOutcome::FinalError(err) => {
                                    return err.into_response(&request_id);
                                }
                            }
                        }
                        let error_id = usage_context.error_id.clone();
                        usage_context
                            .attach_provider_error_credential(
                                &provider,
                                &retry_message,
                                all_attempts,
                            )
                            .with_error_metadata(provider_error_metadata(&retry_error))
                            .record_failure(UsageRecordStatus::Error, "api_error", retry_message);
                        return map_provider_error(
                            retry_error,
                            Some(&request_id),
                            Some(&error_id),
                            Some(provider.as_ref()),
                        );
                    }
                }
            } else if let Some(retry) = too_long_retry.filter(|retry| {
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
                            .with_error_metadata(provider_error_metadata(&e))
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
                    capacity_weight_units,
                )
                .await
                {
                    Ok(resp) => resp,
                    Err(retry_error) => {
                        let retry_message = retry_error.to_string();
                        let retry_attempts = KiroProvider::attempts_from_error(&retry_error);
                        let classification_attempts = retry_attempts.clone();
                        let all_attempts =
                            merge_credential_attempts(retry_attempt_prefix.clone(), retry_attempts);
                        log_provider_call_failure(&retry_message, Some(&usage_context.error_id));
                        let endpoint = usage_context.endpoint.clone();
                        attach_and_log_tool_use_format_diagnostics(
                            &retry_message,
                            &retry_body,
                            kiro_request,
                            &mut usage_context,
                            &endpoint,
                            model,
                            preflight_model,
                        );
                        if let Some(outcome) =
                            maybe_external_fallback_after_local_error_outcome_with_diagnostics(
                                external_fallback.as_ref(),
                                &request_id,
                                &retry_message,
                                classification_attempts.clone(),
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
                                                &classification_attempts,
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
                                            match call_stream_local_rescue_after_external_error(
                                                &provider,
                                                &retry_body,
                                                &request_id,
                                                external,
                                                capacity_weight_units,
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
                                                    let all_attempts = merge_credential_attempts(
                                                        retry_attempt_prefix.clone(),
                                                        rescue_attempts,
                                                    );
                                                    log_provider_call_failure(
                                                        &rescue_message,
                                                        Some(&usage_context.error_id),
                                                    );
                                                    let error_id = usage_context.error_id.clone();
                                                    usage_context
                                                        .attach_provider_error_credential(
                                                            &provider,
                                                            &rescue_message,
                                                            all_attempts,
                                                        )
                                                        .with_error_metadata(
                                                            provider_error_metadata(&rescue_error),
                                                        )
                                                        .record_failure(
                                                            UsageRecordStatus::Error,
                                                            "api_error",
                                                            rescue_message,
                                                        );
                                                    return map_provider_error(
                                                        rescue_error,
                                                        Some(&request_id),
                                                        Some(&error_id),
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
                            let error_id = usage_context.error_id.clone();
                            usage_context
                                .attach_provider_error_credential(
                                    &provider,
                                    &retry_message,
                                    all_attempts,
                                )
                                .with_error_metadata(provider_error_metadata(&retry_error))
                                .record_failure(
                                    UsageRecordStatus::Error,
                                    "api_error",
                                    retry_message,
                                );
                            return map_provider_error(
                                retry_error,
                                Some(&request_id),
                                Some(&error_id),
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
                                    match call_stream_local_rescue_after_external_error(
                                        &provider,
                                        request_body,
                                        &request_id,
                                        external,
                                        capacity_weight_units,
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
                                            log_provider_call_failure(
                                                &rescue_message,
                                                Some(&usage_context.error_id),
                                            );
                                            let error_id = usage_context.error_id.clone();
                                            usage_context
                                                .attach_provider_error_credential(
                                                    &provider,
                                                    &rescue_message,
                                                    all_attempts,
                                                )
                                                .with_error_metadata(provider_error_metadata(
                                                    &rescue_error,
                                                ))
                                                .record_failure(
                                                    UsageRecordStatus::Error,
                                                    "api_error",
                                                    rescue_message,
                                                );
                                            return map_provider_error(
                                                rescue_error,
                                                Some(&request_id),
                                                Some(&error_id),
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
                    let error_id = usage_context.error_id.clone();
                    usage_context
                        .attach_provider_error_credential(&provider, &message, attempts)
                        .with_error_metadata(provider_error_metadata(&e))
                        .record_failure(UsageRecordStatus::Error, "api_error", message);
                    return map_provider_error(
                        e,
                        Some(&request_id),
                        Some(&error_id),
                        Some(provider.as_ref()),
                    );
                }
            }
        }
    };
    usage_context.mark_upstream_header();
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
    ctx.set_requested_max_tokens(requested_max_tokens);
    ctx.set_reported_cache_usage_policy(credential_usage.request.reported_cache_usage_policy());
    ctx.set_local_prompt_cache_projection_enabled(
        credential_usage.request.uses_local_prompt_cache_strategy(),
    );
    ctx.set_stream_error_id(credential_usage.request.error_id.clone());

    // 生成初始事件
    let initial_events = ctx.generate_initial_events_with_reported_usage_mapper(|reported_usage| {
        credential_usage
            .preview_creation_frequency_control(reported_usage, UsageSource::LocalPromptCache)
    });

    // 创建 SSE 流
    let response_request_id = credential_usage.request.request_id.clone();
    let stream = create_sse_stream(
        response,
        ctx,
        initial_events,
        completion,
        credential_usage,
        stream_idle_timeout_secs,
        claude_code_noop_delta_keepalive,
    );

    // 返回 SSE 响应
    let mut builder = envelope::sse_builder_with_id(&response_request_id);
    if let Some(warnings) = warnings_header {
        builder = builder.header("x-kiro-rs-warnings", warnings);
    }
    builder.body(Body::from_stream(stream)).unwrap()
}

/// Ping 事件间隔。Claude Code 的插件 UI 在长 thinking/tool_use 阶段可能没有可见正文；
/// 更短的保活能避免中间代理或客户端误判流已经停住，且不会污染模型输出内容。
const PING_INTERVAL_SECS: u64 = 5;
/// 上游 eventstream 默认读空闲超时（180秒）
const DEFAULT_UPSTREAM_IDLE_TIMEOUT_SECS: u64 = 180;
const JSON_STREAM_ERROR_SNIFF_MAX_BYTES: usize = 64 * 1024;
const CLAUDE_CODE_NOOP_DELTA_KEEPALIVE_MIN_VERSION: &str = "2.1.193";

fn request_user_agent(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
}

fn should_use_claude_code_noop_delta_keepalive(user_agent: Option<&str>) -> bool {
    let Some(version) = user_agent.and_then(extract_claude_code_cli_version) else {
        return false;
    };
    compare_three_part_semver(version, CLAUDE_CODE_NOOP_DELTA_KEEPALIVE_MIN_VERSION)
        .is_some_and(|ordering| ordering != std::cmp::Ordering::Less)
}

fn extract_claude_code_cli_version(user_agent: &str) -> Option<&str> {
    const PREFIX: &str = "claude-cli/";
    let trimmed = user_agent.trim();
    let prefix = trimmed.get(..PREFIX.len())?;
    if !prefix.eq_ignore_ascii_case(PREFIX) {
        return None;
    }
    let rest = &trimmed[PREFIX.len()..];
    let end = rest
        .find(|ch: char| !(ch.is_ascii_digit() || ch == '.'))
        .unwrap_or(rest.len());
    let version = &rest[..end];
    parse_three_part_semver(version).map(|_| version)
}

fn compare_three_part_semver(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    let a = parse_three_part_semver(a)?;
    let b = parse_three_part_semver(b)?;
    Some(a.cmp(&b))
}

fn parse_three_part_semver(version: &str) -> Option<[u32; 3]> {
    let mut result = [0_u32; 3];
    let mut parts = version.split('.');
    for slot in &mut result {
        let part = parts.next()?;
        if part.is_empty() || !part.chars().all(|ch| ch.is_ascii_digit()) {
            return None;
        }
        *slot = part.parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some(result)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct JsonStreamError {
    error_type: &'static str,
    internal_detail: String,
    body_preview: String,
}

enum JsonStreamSniffResult {
    Pass(Bytes),
    Pending,
    Error(JsonStreamError),
}

struct JsonStreamErrorSniffer {
    enabled: bool,
    decided: bool,
    buffer: Vec<u8>,
}

impl JsonStreamErrorSniffer {
    fn new(content_type: Option<&str>) -> Self {
        Self {
            enabled: content_type.is_some_and(is_json_media_type),
            decided: false,
            buffer: Vec::new(),
        }
    }

    fn inspect(&mut self, chunk: Bytes) -> JsonStreamSniffResult {
        if !self.enabled || self.decided {
            return JsonStreamSniffResult::Pass(chunk);
        }

        self.buffer.extend_from_slice(&chunk);

        let Some(first_non_ws) = self
            .buffer
            .iter()
            .copied()
            .find(|byte| !byte.is_ascii_whitespace())
        else {
            if self.buffer.len() > JSON_STREAM_ERROR_SNIFF_MAX_BYTES {
                return JsonStreamSniffResult::Error(
                    self.protocol_error("json_stream_error_body_too_large"),
                );
            }
            return JsonStreamSniffResult::Pending;
        };

        if !matches!(first_non_ws, b'{' | b'[') {
            self.decided = true;
            return JsonStreamSniffResult::Pass(Bytes::from(std::mem::take(&mut self.buffer)));
        }

        if self.buffer.len() > JSON_STREAM_ERROR_SNIFF_MAX_BYTES {
            return JsonStreamSniffResult::Error(
                self.protocol_error("json_stream_error_body_too_large"),
            );
        }

        let trimmed = trim_ascii_whitespace(&self.buffer);
        match serde_json::from_slice::<Value>(trimmed) {
            Ok(value) => {
                self.decided = true;
                JsonStreamSniffResult::Error(classify_json_stream_error(&value, trimmed))
            }
            Err(err) if err.is_eof() => JsonStreamSniffResult::Pending,
            Err(err) => JsonStreamSniffResult::Error(JsonStreamError {
                error_type: "api_error",
                internal_detail: format!("json_stream_malformed_json: {}", err),
                body_preview: bytes_preview(trimmed),
            }),
        }
    }

    fn finish(&mut self) -> Option<JsonStreamError> {
        if !self.enabled || self.decided || self.buffer.is_empty() {
            return None;
        }
        self.decided = true;

        let trimmed = trim_ascii_whitespace(&self.buffer);
        if trimmed.is_empty() {
            return Some(JsonStreamError {
                error_type: "api_error",
                internal_detail: "json_stream_empty_body".to_string(),
                body_preview: String::new(),
            });
        }

        Some(JsonStreamError {
            error_type: "api_error",
            internal_detail: "json_stream_incomplete_json".to_string(),
            body_preview: bytes_preview(trimmed),
        })
    }

    fn protocol_error(&self, reason: &str) -> JsonStreamError {
        JsonStreamError {
            error_type: "api_error",
            internal_detail: reason.to_string(),
            body_preview: bytes_preview(trim_ascii_whitespace(&self.buffer)),
        }
    }
}

fn is_json_media_type(content_type: &str) -> bool {
    content_type
        .split(';')
        .next()
        .map(|media_type| media_type.trim().eq_ignore_ascii_case("application/json"))
        .unwrap_or(false)
}

fn trim_ascii_whitespace(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map(|idx| idx + 1)
        .unwrap_or(start);
    &bytes[start..end]
}

fn bytes_preview(bytes: &[u8]) -> String {
    const PREVIEW_LIMIT: usize = 2048;
    let end = bytes.len().min(PREVIEW_LIMIT);
    let mut preview = String::from_utf8_lossy(&bytes[..end]).into_owned();
    if bytes.len() > PREVIEW_LIMIT {
        preview.push_str("...[truncated]");
    }
    preview
}

fn json_string_at<'a>(value: &'a Value, pointers: &[&str]) -> Option<&'a str> {
    pointers
        .iter()
        .find_map(|pointer| value.pointer(pointer).and_then(Value::as_str))
        .filter(|text| !text.trim().is_empty())
}

fn classify_json_stream_error(value: &Value, raw_body: &[u8]) -> JsonStreamError {
    let code = json_string_at(
        value,
        &[
            "/__type",
            "/code",
            "/Code",
            "/type",
            "/error/type",
            "/error/code",
            "/error/Code",
        ],
    )
    .map(str::to_string);
    let reason = json_string_at(
        value,
        &["/reason", "/Reason", "/error/reason", "/error/Reason"],
    )
    .map(str::to_string);
    let message = json_string_at(
        value,
        &[
            "/message",
            "/Message",
            "/error/message",
            "/error/Message",
            "/error",
        ],
    )
    .map(str::to_string);

    let combined = [&code, &reason, &message]
        .into_iter()
        .filter_map(|value| value.as_deref())
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();

    let error_type = if combined.contains("request_body_invalid")
        || combined.contains("invalid tool use")
        || combined.contains("validation")
        || combined.contains("bad request")
    {
        "invalid_request_error"
    } else if combined.contains("throttl")
        || combined.contains("too many requests")
        || combined.contains("rate")
    {
        "rate_limit_error"
    } else {
        "api_error"
    };

    let mut fields = Vec::new();
    if let Some(code) = code {
        fields.push(format!("code={}", code));
    }
    if let Some(reason) = reason {
        fields.push(format!("reason={}", reason));
    }
    if let Some(message) = message {
        fields.push(format!("message={}", message));
    }

    let internal_detail = if fields.is_empty() {
        "json_stream_unexpected_body".to_string()
    } else {
        format!("json_stream_exception {}", fields.join(" "))
    };

    JsonStreamError {
        error_type,
        internal_detail,
        body_preview: bytes_preview(raw_body),
    }
}

/// 创建 ping 事件的 SSE 字符串
fn create_ping_sse() -> Bytes {
    Bytes::from("event: ping\ndata: {\"type\": \"ping\"}\n\n")
}

fn finish_stream_with_recorded_error(
    ctx: &mut StreamContext,
    usage_guard: &StreamUsageGuard,
    status: UsageRecordStatus,
    terminal_reason: StreamTerminalReason,
) -> Vec<Result<Bytes, Infallible>> {
    let error_detail = ctx.stream_error_detail();
    let final_events = ctx.generate_final_events();
    usage_guard
        .context()
        .request
        .mark_stream_terminal(terminal_reason);
    usage_guard
        .context()
        .request
        .mark_stream_events(&final_events);
    usage_guard.context().record_stream_failure_from_context(
        status,
        ctx.final_usage(),
        error_detail,
        ctx.metadata_usage(),
        ctx.context_input_tokens,
        ctx.kiro_metering_usage(),
    );
    usage_guard.complete();
    final_events
        .into_iter()
        .map(|event| Ok(Bytes::from(event.to_sse_string())))
        .collect()
}

/// 创建 SSE 事件流
fn create_sse_stream(
    response: reqwest::Response,
    ctx: StreamContext,
    initial_events: Vec<SseEvent>,
    completion: KiroStreamCompletion,
    usage_context: CredentialUsageContext,
    stream_idle_timeout_secs: u64,
    claude_code_noop_delta_keepalive: bool,
) -> impl Stream<Item = Result<Bytes, Infallible>> {
    let usage_guard = StreamUsageGuard::new(usage_context);
    let stream_idle_timeout_secs = normalize_stream_idle_timeout_secs(stream_idle_timeout_secs);
    // 先发送初始事件
    let initial_stream = stream::iter(
        initial_events
            .into_iter()
            .map(|e| Ok(Bytes::from(e.to_sse_string()))),
    );

    // 然后处理 Kiro 响应流，同时定期发送 ping 保活
    let upstream_content_type = response
        .headers()
        .get(REQWEST_CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let json_sniffer = JsonStreamErrorSniffer::new(upstream_content_type.as_deref());
    let body_stream = response.bytes_stream();

    let processing_stream = stream::unfold(
        (
            body_stream,
            ctx,
            EventStreamDecoder::new(),
            json_sniffer,
            false,
            completion,
            usage_guard,
            interval(Duration::from_secs(PING_INTERVAL_SECS)),
            Instant::now() + Duration::from_secs(stream_idle_timeout_secs),
            stream_idle_timeout_secs,
        ),
        move |(
            mut body_stream,
            mut ctx,
            mut decoder,
            mut json_sniffer,
            finished,
            completion,
            usage_guard,
            mut ping_interval,
            mut idle_deadline,
            stream_idle_timeout_secs,
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
                            usage_guard
                                .context()
                                .request
                                .mark_first_upstream_chunk();
                            idle_deadline = Instant::now() + Duration::from_secs(stream_idle_timeout_secs);
                            completion.touch();

                            let chunk = match json_sniffer.inspect(chunk) {
                                JsonStreamSniffResult::Pass(chunk) => chunk,
                                JsonStreamSniffResult::Pending => {
                                    let bytes: Vec<Result<Bytes, Infallible>> = Vec::new();
                                    return Some((stream::iter(bytes), (body_stream, ctx, decoder, json_sniffer, false, completion, usage_guard, ping_interval, idle_deadline, stream_idle_timeout_secs)));
                                }
                                JsonStreamSniffResult::Error(error) => {
                                    tracing::warn!(
                                        error_type = error.error_type,
                                        error_detail = %error.internal_detail,
                                        body = %error.body_preview,
                                        "流式 API 返回 2xx JSON 错误体"
                                    );
                                    completion.report_upstream_stream_failure(error.internal_detail.clone());
                                    ctx.record_stream_error(error.error_type, error.internal_detail);
                                    let bytes = finish_stream_with_recorded_error(
                                        &mut ctx,
                                        &usage_guard,
                                        UsageRecordStatus::StreamError,
                                        StreamTerminalReason::UpstreamJsonException,
                                    );
                                    return Some((stream::iter(bytes), (body_stream, ctx, decoder, json_sniffer, true, completion, usage_guard, ping_interval, idle_deadline, stream_idle_timeout_secs)));
                                }
                            };

                            // 解码事件
                            if let Err(e) = decoder.feed(&chunk) {
                                tracing::warn!("缓冲区溢出: {}", e);
                                usage_guard
                                    .context()
                                    .request
                                    .mark_upstream_frame_decode_error_before_first_output();
                            }

                            let mut events = Vec::new();
                            let mut decoded_frames_in_chunk = 0_u32;
                            let mut first_output_reached_in_chunk =
                                usage_guard.context().request.has_first_output();
                            for result in decoder.decode_iter() {
                                match result {
                                    Ok(frame) => {
                                        decoded_frames_in_chunk =
                                            decoded_frames_in_chunk.saturating_add(1);
                                        let before_first_output =
                                            !first_output_reached_in_chunk
                                                && !usage_guard.context().request.has_first_output();
                                        match Event::from_frame(frame) {
                                            Ok(event) => {
                                                let sse_events = ctx.process_kiro_event(&event);
                                                let frame_has_first_output = sse_events
                                                    .iter()
                                                    .any(is_first_token_output_event);

                                                if before_first_output && !frame_has_first_output {
                                                    usage_guard
                                                        .context()
                                                        .request
                                                        .mark_upstream_frame_before_first_output();
                                                    usage_guard
                                                        .context()
                                                        .request
                                                        .mark_upstream_event_before_first_output(
                                                            &event,
                                                            sse_events.len(),
                                                        );
                                                }
                                                if frame_has_first_output {
                                                    first_output_reached_in_chunk = true;
                                                }

                                                events.extend(sse_events);
                                            }
                                            Err(_) => {
                                                if before_first_output {
                                                    usage_guard
                                                        .context()
                                                        .request
                                                        .mark_upstream_frame_before_first_output();
                                                    usage_guard
                                                        .context()
                                                        .request
                                                        .mark_upstream_event_parse_error_before_first_output();
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!("解码事件失败: {}", e);
                                        if !first_output_reached_in_chunk
                                            && !usage_guard.context().request.has_first_output()
                                        {
                                            usage_guard
                                                .context()
                                                .request
                                                .mark_upstream_frame_decode_error_before_first_output();
                                        }
                                    }
                                }
                            }
                            if !first_output_reached_in_chunk
                                && !usage_guard.context().request.has_first_output()
                            {
                                usage_guard
                                    .context()
                                    .request
                                    .mark_upstream_bytes_before_first_output(chunk.len());
                                if decoded_frames_in_chunk == 0 {
                                    usage_guard
                                        .context()
                                        .request
                                        .mark_upstream_pending_chunk_before_first_output();
                                }
                            }

                            // 转换为 SSE 字节流
                            usage_guard
                                .context()
                                .request
                                .mark_stream_events(&events);
                            let bytes: Vec<Result<Bytes, Infallible>> = events
                                .into_iter()
                                .map(|e| Ok(Bytes::from(e.to_sse_string())))
                                .collect();

                            Some((stream::iter(bytes), (body_stream, ctx, decoder, json_sniffer, false, completion, usage_guard, ping_interval, idle_deadline, stream_idle_timeout_secs)))
                        }
                        Some(Err(e)) => {
                            tracing::error!("读取响应流失败: {}", e);
                            completion.report_upstream_stream_failure(format!(
                                "upstream stream read error: {}",
                                e
                            ));
                            // 读取错误：关闭已有内容块后发送 SSE error，不再发送正常 message_stop。
                            ctx.record_stream_error("api_error", format!("upstream stream read error: {}", e));
                            let bytes = finish_stream_with_recorded_error(
                                &mut ctx,
                                &usage_guard,
                                UsageRecordStatus::StreamError,
                                StreamTerminalReason::InternalError,
                            );
                            Some((stream::iter(bytes), (body_stream, ctx, decoder, json_sniffer, true, completion, usage_guard, ping_interval, idle_deadline, stream_idle_timeout_secs)))
                        }
                        None => {
                            if let Some(error) = json_sniffer.finish() {
                                tracing::warn!(
                                    error_type = error.error_type,
                                    error_detail = %error.internal_detail,
                                    body = %error.body_preview,
                                    "流式 API 返回未完成的 JSON 错误体"
                                );
                                completion.report_upstream_stream_failure(error.internal_detail.clone());
                                ctx.record_stream_error(error.error_type, error.internal_detail);
                                let bytes = finish_stream_with_recorded_error(
                                    &mut ctx,
                                    &usage_guard,
                                    UsageRecordStatus::StreamError,
                                    StreamTerminalReason::UpstreamJsonException,
                                );
                                return Some((stream::iter(bytes), (body_stream, ctx, decoder, json_sniffer, true, completion, usage_guard, ping_interval, idle_deadline, stream_idle_timeout_secs)));
                            }
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
                                .mark_stream_terminal(if had_stream_error {
                                    StreamTerminalReason::UpstreamStatusError
                                } else {
                                    StreamTerminalReason::Completed
                                });
                            usage_guard
                                .context()
                                .request
                                .mark_stream_events(&final_events);
                            if had_stream_error {
                                usage_guard.context().record_stream_failure_from_context(
                                    UsageRecordStatus::StreamError,
                                    ctx.final_usage(),
                                    error_detail,
                                    ctx.metadata_usage(),
                                    ctx.context_input_tokens,
                                    ctx.kiro_metering_usage(),
                                );
                            } else {
                                usage_guard.context().record_success_from_stream(&ctx);
                            }
                            usage_guard.complete();
                            let bytes: Vec<Result<Bytes, Infallible>> = final_events
                                .into_iter()
                                .map(|e| Ok(Bytes::from(e.to_sse_string())))
                                .collect();
                            Some((stream::iter(bytes), (body_stream, ctx, decoder, json_sniffer, true, completion, usage_guard, ping_interval, idle_deadline, stream_idle_timeout_secs)))
                        }
                    }
                }
                _ = &mut idle_sleep => {
                    tracing::error!(
                        "上游响应流超过 {} 秒未产生数据，结束流并发送错误事件",
                        stream_idle_timeout_secs
                    );
                    completion.report_upstream_stream_failure("upstream stream idle timeout");
                    ctx.record_stream_error("api_error", "upstream stream idle timeout");
                    let bytes = finish_stream_with_recorded_error(
                        &mut ctx,
                        &usage_guard,
                        UsageRecordStatus::UpstreamTimeout,
                        StreamTerminalReason::UpstreamIdleTimeout,
                    );
                    Some((stream::iter(bytes), (body_stream, ctx, decoder, json_sniffer, true, completion, usage_guard, ping_interval, idle_deadline, stream_idle_timeout_secs)))
                }
                // 发送 ping 保活
                _ = ping_interval.tick() => {
                    let keepalive = if claude_code_noop_delta_keepalive {
                        ctx.claude_code_noop_delta_keepalive_event()
                            .map(|event| Bytes::from(event.to_sse_string()))
                    } else {
                        None
                    };
                    let bytes = match keepalive {
                        Some(bytes) => {
                            tracing::trace!("发送 Claude Code 空 delta 保活事件");
                            bytes
                        }
                        None => {
                            tracing::trace!("发送 ping 保活事件");
                            create_ping_sse()
                        }
                    };
                    let bytes: Vec<Result<Bytes, Infallible>> = vec![Ok(bytes)];
                    Some((stream::iter(bytes), (body_stream, ctx, decoder, json_sniffer, false, completion, usage_guard, ping_interval, idle_deadline, stream_idle_timeout_secs)))
                }
            }
        },
    )
    .flatten();

    initial_stream.chain(processing_stream)
}

fn normalize_stream_idle_timeout_secs(value: u64) -> u64 {
    if value == 0 {
        DEFAULT_UPSTREAM_IDLE_TIMEOUT_SECS
    } else {
        value
    }
}

use super::converter::get_context_window_size;

/// 处理非流式请求
async fn handle_non_stream_request(
    provider: std::sync::Arc<crate::kiro::provider::KiroProvider>,
    request_body: &str,
    kiro_request: &KiroRequest,
    model: &str,
    preflight_model: &str,
    input_tokens: i32,
    thinking_enabled: bool,
    tool_name_map: HashMap<String, String>,
    known_tool_names: HashSet<String>,
    usage_context: RequestUsageContext,
    warnings_header: Option<String>,
    too_long_retry: Option<PayloadTooLongRetryRequest>,
    cache_point_retry: Option<CachePointRetryRequest>,
    external_fallback: Option<ExternalFallbackContext>,
    capacity_weight_units: u32,
) -> Response {
    // 调用 Kiro API（支持多凭据故障转移）
    let mut usage_context = usage_context;
    let mut warnings_header = warnings_header;
    let request_id = usage_context.request_id.clone();
    let mut retry_attempt_prefix: Vec<KiroCredentialAttempt> = Vec::new();
    if let Some(response) = maybe_local_pool_preflight_external_response(
        external_fallback.as_ref(),
        provider.as_ref(),
        &request_id,
        Some(preflight_model),
    )
    .await
    {
        return response;
    }
    let api_response = match call_api_maybe_fail_fast(
        &provider,
        request_body,
        Some(&request_id),
        external_fallback.as_ref(),
        capacity_weight_units,
    )
    .await
    {
        Ok(resp) => resp,
        Err(e) => {
            let message = e.to_string();
            let attempts = KiroProvider::attempts_from_error(&e);
            log_provider_call_failure(&message, Some(&usage_context.error_id));
            let endpoint = usage_context.endpoint.clone();
            attach_and_log_tool_use_format_diagnostics(
                &message,
                request_body,
                kiro_request,
                &mut usage_context,
                &endpoint,
                model,
                preflight_model,
            );
            let should_payload_guard_retry = too_long_retry.as_ref().is_some_and(|retry| {
                should_retry_payload_guard_after_error(
                    &message,
                    request_body.len(),
                    retry.config.max_bytes,
                )
            });
            if let Some(retry) = cache_point_retry.filter(|_| {
                !should_payload_guard_retry
                    && should_retry_without_cache_point_after_error(&message)
            }) {
                retry_attempt_prefix = attempts.clone();
                let retry_body = match retry.build_retry_body(&message, &mut usage_context) {
                    Ok(result) => result,
                    Err(err) => {
                        usage_context
                            .attach_provider_error_credential(&provider, &message, attempts)
                            .with_error_metadata(provider_error_metadata(&e))
                            .record_failure(
                                UsageRecordStatus::Error,
                                "payload_guard_error",
                                format!(
                                    "cachePoint retry failed while building fallback payload: {}",
                                    err
                                ),
                            );
                        return payload_guard_error_response(err);
                    }
                };
                match call_api_maybe_fail_fast(
                    &provider,
                    &retry_body,
                    Some(&request_id),
                    external_fallback.as_ref(),
                    capacity_weight_units,
                )
                .await
                {
                    Ok(resp) => resp,
                    Err(retry_error) => {
                        let retry_message = retry_error.to_string();
                        let retry_attempts = KiroProvider::attempts_from_error(&retry_error);
                        let classification_attempts = retry_attempts.clone();
                        let all_attempts =
                            merge_credential_attempts(retry_attempt_prefix.clone(), retry_attempts);
                        log_provider_call_failure(&retry_message, Some(&usage_context.error_id));
                        let endpoint = usage_context.endpoint.clone();
                        attach_and_log_tool_use_format_diagnostics(
                            &retry_message,
                            &retry_body,
                            kiro_request,
                            &mut usage_context,
                            &endpoint,
                            model,
                            preflight_model,
                        );
                        if let Some(outcome) =
                            maybe_external_fallback_after_local_error_outcome_with_diagnostics(
                                external_fallback.as_ref(),
                                &request_id,
                                &retry_message,
                                classification_attempts,
                                all_attempts.clone(),
                            )
                            .await
                        {
                            match outcome {
                                ExternalPoolForwardOutcome::Response(response) => return response,
                                ExternalPoolForwardOutcome::FinalError(err) => {
                                    return err.into_response(&request_id);
                                }
                            }
                        }
                        let error_id = usage_context.error_id.clone();
                        usage_context
                            .attach_provider_error_credential(
                                &provider,
                                &retry_message,
                                all_attempts,
                            )
                            .with_error_metadata(provider_error_metadata(&retry_error))
                            .record_failure(UsageRecordStatus::Error, "api_error", retry_message);
                        return map_provider_error(
                            retry_error,
                            Some(&request_id),
                            Some(&error_id),
                            Some(provider.as_ref()),
                        );
                    }
                }
            } else if let Some(retry) = too_long_retry.filter(|retry| {
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
                            .with_error_metadata(provider_error_metadata(&e))
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
                    capacity_weight_units,
                )
                .await
                {
                    Ok(resp) => resp,
                    Err(retry_error) => {
                        let retry_message = retry_error.to_string();
                        let retry_attempts = KiroProvider::attempts_from_error(&retry_error);
                        let classification_attempts = retry_attempts.clone();
                        let all_attempts =
                            merge_credential_attempts(retry_attempt_prefix.clone(), retry_attempts);
                        log_provider_call_failure(&retry_message, Some(&usage_context.error_id));
                        let endpoint = usage_context.endpoint.clone();
                        attach_and_log_tool_use_format_diagnostics(
                            &retry_message,
                            &retry_body,
                            kiro_request,
                            &mut usage_context,
                            &endpoint,
                            model,
                            preflight_model,
                        );
                        if let Some(outcome) =
                            maybe_external_fallback_after_local_error_outcome_with_diagnostics(
                                external_fallback.as_ref(),
                                &request_id,
                                &retry_message,
                                classification_attempts.clone(),
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
                                                &classification_attempts,
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
                                            match call_non_stream_local_rescue_after_external_error(
                                                &provider,
                                                &retry_body,
                                                &request_id,
                                                external,
                                                capacity_weight_units,
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
                                                    let all_attempts = merge_credential_attempts(
                                                        retry_attempt_prefix.clone(),
                                                        rescue_attempts,
                                                    );
                                                    log_provider_call_failure(
                                                        &rescue_message,
                                                        Some(&usage_context.error_id),
                                                    );
                                                    let error_id = usage_context.error_id.clone();
                                                    usage_context
                                                        .attach_provider_error_credential(
                                                            &provider,
                                                            &rescue_message,
                                                            all_attempts,
                                                        )
                                                        .with_error_metadata(
                                                            provider_error_metadata(&rescue_error),
                                                        )
                                                        .record_failure(
                                                            UsageRecordStatus::Error,
                                                            "api_error",
                                                            rescue_message,
                                                        );
                                                    return map_provider_error(
                                                        rescue_error,
                                                        Some(&request_id),
                                                        Some(&error_id),
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
                            let error_id = usage_context.error_id.clone();
                            usage_context
                                .attach_provider_error_credential(
                                    &provider,
                                    &retry_message,
                                    all_attempts,
                                )
                                .with_error_metadata(provider_error_metadata(&retry_error))
                                .record_failure(
                                    UsageRecordStatus::Error,
                                    "api_error",
                                    retry_message,
                                );
                            return map_provider_error(
                                retry_error,
                                Some(&request_id),
                                Some(&error_id),
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
                                    match call_non_stream_local_rescue_after_external_error(
                                        &provider,
                                        request_body,
                                        &request_id,
                                        external,
                                        capacity_weight_units,
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
                                            log_provider_call_failure(
                                                &rescue_message,
                                                Some(&usage_context.error_id),
                                            );
                                            let error_id = usage_context.error_id.clone();
                                            usage_context
                                                .attach_provider_error_credential(
                                                    &provider,
                                                    &rescue_message,
                                                    all_attempts,
                                                )
                                                .with_error_metadata(provider_error_metadata(
                                                    &rescue_error,
                                                ))
                                                .record_failure(
                                                    UsageRecordStatus::Error,
                                                    "api_error",
                                                    rescue_message,
                                                );
                                            return map_provider_error(
                                                rescue_error,
                                                Some(&request_id),
                                                Some(&error_id),
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
                    let error_id = usage_context.error_id.clone();
                    usage_context
                        .attach_provider_error_credential(&provider, &message, attempts)
                        .with_error_metadata(provider_error_metadata(&e))
                        .record_failure(UsageRecordStatus::Error, "api_error", message);
                    return map_provider_error(
                        e,
                        Some(&request_id),
                        Some(&error_id),
                        Some(provider.as_ref()),
                    );
                }
            }
        }
    };
    usage_context.mark_upstream_header();
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
                envelope::PUBLIC_PROCESSING_FAILED_MESSAGE,
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
    let mut kiro_metering_usage: Option<f64> = None;
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
                                let input = crate::anthropic::stream::repair_tool_use_input_for_cli(
                                    &original_name,
                                    input,
                                );
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
                            if metering.usage.is_finite() {
                                kiro_metering_usage = Some(metering.usage);
                            }
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
                                UPSTREAM_INVALID_REQUEST_MESSAGE,
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
    if stop_reason == "end_turn"
        && !has_tool_use
        && super::stream::output_tokens_reached_requested_max_tokens(
            credential_usage.request.requested_max_tokens,
            output_tokens,
        )
    {
        stop_reason = "max_tokens".to_string();
    }
    credential_usage
        .request
        .set_downstream_stop_reason(stop_reason.clone());

    // 优先使用 metadataEvent 的准确 usage，其次使用 contextUsageEvent 估算值。
    let final_input_tokens = metadata_usage
        .as_ref()
        .map(|usage| usage.total_input_tokens())
        .or(context_input_tokens)
        .unwrap_or(input_tokens);
    let build_local_prompt_cache_usage = should_build_local_prompt_cache_usage(
        credential_usage.request.prompt_cache_strategy_type,
        credential_usage.request.simulation_mode,
    );
    let usage_input_tokens = if build_local_prompt_cache_usage {
        final_input_tokens.max(credential_usage.request.input_tokens)
    } else {
        final_input_tokens
    };

    let usage = super::cache::build_usage_with_simulation_policy(
        metadata_usage.as_ref(),
        usage_input_tokens,
        output_tokens,
        credential_usage.request.simulated_usage,
        build_local_prompt_cache_usage,
    );
    let has_metadata = metadata_usage.is_some();
    let context_estimated = !has_metadata && context_input_tokens.is_some();
    let raw_usage = raw_usage_from_metadata_or_estimate(
        metadata_usage.as_ref(),
        final_input_tokens,
        output_tokens,
    );
    let usage_source =
        credential_usage.usage_source(&usage, metadata_usage.as_ref(), context_estimated);
    let reported_usage = credential_usage.canonical_reported_usage_for_success_with_raw(
        usage,
        usage_source,
        raw_usage,
    );
    credential_usage.record_success_reported_with_metering(
        reported_usage,
        usage_source,
        Some(raw_usage),
        kiro_metering_usage,
    );
    completion.report_success();

    // 构建 Anthropic 响应
    let usage_json = reported_usage.to_anthropic_usage_json_with_thinking_tokens(
        thinking_tokens_from_content(&content, reported_usage.output_tokens),
    );
    let response_body = json!({
        "id": envelope::message_id(),
        "type": "message",
        "role": "assistant",
        "content": content,
        "model": model,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": usage_json
    });

    envelope::json_response_with_id(
        StatusCode::OK,
        response_body,
        &credential_usage.request.request_id,
        warnings_header,
    )
}

#[derive(Debug, Clone, Copy)]
struct ThinkingModelDefaults {
    thinking_type: &'static str,
    effort: Option<&'static str>,
    budget_tokens: i32,
}

fn claude_minor_version(model: &str, family: &str) -> Option<u32> {
    let prefix = format!("claude-{family}-4");
    let rest = model.strip_prefix(&prefix)?;
    let rest = rest.strip_prefix(['-', '.'])?;
    let digits: String = rest.chars().take_while(|ch| ch.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

fn thinking_model_defaults(model: &str) -> Option<ThinkingModelDefaults> {
    let (model_base, requested_thinking) = strip_model_compat_suffixes(model);
    if !requested_thinking {
        return None;
    }

    let is_opus_alias = matches!(
        model_base.as_str(),
        "opus" | "opusplan" | "best" | "default"
    );
    let is_sonnet_alias = model_base == "sonnet";
    let is_opus = is_opus_alias || model_base.contains("opus");
    let is_sonnet = is_sonnet_alias || model_base.contains("sonnet");

    let opus_minor = claude_minor_version(&model_base, "opus");
    let sonnet_minor = claude_minor_version(&model_base, "sonnet");
    let adaptive = is_opus_alias
        || is_sonnet_alias
        || opus_minor.is_some_and(|minor| minor >= 6)
        || sonnet_minor.is_some_and(|minor| minor >= 6);
    let effort = if is_opus_alias || opus_minor.is_some_and(|minor| minor >= 6) {
        Some("xhigh")
    } else if is_sonnet_alias || sonnet_minor.is_some_and(|minor| minor >= 6) {
        Some("high")
    } else if is_opus || is_sonnet {
        Some("high")
    } else {
        None
    };

    Some(ThinkingModelDefaults {
        thinking_type: if adaptive { "adaptive" } else { "enabled" },
        effort,
        budget_tokens: if adaptive { 0 } else { 20000 },
    })
}

/// 检测模型名是否包含 "thinking" 后缀，若包含则在调用方未显式配置时注入 thinking
///
/// - 调用方已指定 `thinking` 字段：保留原值
/// - 调用方未指定：按模型族注入默认 thinking，确保 `-thinking` 兼容模型有真实 thinking 输出
///   - Opus/Sonnet 4.6+ 和别名使用 `adaptive` + `output_config.effort`
///   - 旧模型使用 `enabled` + `budget_tokens`
/// - `output_config.effort` 在最终 thinking 类型为 adaptive 且未设置时按模型族填充
fn override_thinking_from_model_name(payload: &mut MessagesRequest) {
    let Some(defaults) = thinking_model_defaults(&payload.model) else {
        return;
    };

    if payload.thinking.is_none() {
        tracing::info!(
            model = %payload.model,
            thinking_type = defaults.thinking_type,
            effort = defaults.effort.unwrap_or(""),
            "模型名包含 thinking 后缀，注入默认 thinking 配置"
        );
        payload.thinking = Some(Thinking {
            thinking_type: defaults.thinking_type.to_string(),
            budget_tokens: defaults.budget_tokens,
        });
    } else {
        tracing::debug!(
            model = %payload.model,
            "调用方已指定 thinking 配置，保留原值"
        );
    }

    let final_thinking_type = payload
        .thinking
        .as_ref()
        .map(|thinking| thinking.thinking_type.as_str());
    if final_thinking_type == Some("adaptive") && payload.output_config.is_none() {
        payload.output_config = Some(OutputConfig {
            effort: defaults.effort.unwrap_or("high").to_string(),
        });
    }
}

fn apply_thinking_trigger_mode(
    payload: &mut MessagesRequest,
    runtime_config: &RequestRuntimeConfig,
) {
    let should_trigger = match runtime_config.thinking_trigger_mode {
        ThinkingTriggerMode::Always => true,
        ThinkingTriggerMode::RealRequest => {
            request_has_claude_code_visible_thinking_signal(payload)
        }
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
                trigger_mode = ?runtime_config.thinking_trigger_mode,
                "thinking 触发信号已匹配，改写未知 thinking 类型为 adaptive"
            );
            thinking.thinking_type = "adaptive".to_string();
            thinking.budget_tokens = 0;
        }
        None => {
            payload.thinking = Some(Thinking {
                thinking_type: "adaptive".to_string(),
                budget_tokens: 0,
            });
        }
    }

    if payload
        .thinking
        .as_ref()
        .is_some_and(|thinking| thinking.thinking_type == "adaptive")
        && payload.output_config.is_none()
    {
        payload.output_config = Some(OutputConfig {
            effort: "high".to_string(),
        });
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

fn should_force_visible_thinking(
    payload: &MessagesRequest,
    runtime_config: &RequestRuntimeConfig,
) -> bool {
    if !payload
        .thinking
        .as_ref()
        .is_some_and(|thinking| thinking.is_enabled())
    {
        return false;
    }

    match runtime_config.thinking_trigger_mode {
        ThinkingTriggerMode::Always => true,
        ThinkingTriggerMode::RealRequest => {
            request_has_claude_code_visible_thinking_signal(payload)
        }
    }
}

/// POST /v1/messages/count_tokens
///
/// 计算消息的 token 数量
pub async fn count_tokens(
    State(state): State<AppState>,
    JsonExtractor(mut payload): JsonExtractor<CountTokensRequest>,
) -> Response {
    tracing::info!(
        model = %payload.model,
        message_count = %payload.messages.len(),
        "Received POST /v1/messages/count_tokens request"
    );

    let image_processing = request_image_processing_config(&state);
    if let Err(message) = body_processing::prepare_multimodal_message_sources(
        &state.file_store,
        &mut payload.messages,
        None,
        image_processing,
    )
    .await
    {
        tracing::warn!("count_tokens 多模态 source 处理失败: {}", message);
        return envelope::error_response(StatusCode::BAD_REQUEST, "invalid_request_error", message);
    }

    let total_tokens = token::count_all_tokens(
        &payload.model,
        payload.system.as_deref(),
        &payload.messages,
        payload.tools.as_deref(),
    ) as i32;

    Json(CountTokensResponse {
        input_tokens: total_tokens.max(1) as i32,
    })
    .into_response()
}

/// POST /dfcache/:route/v1/messages/count_tokens
pub async fn count_tokens_dfcache(
    State(state): State<AppState>,
    Path(route): Path<String>,
    JsonExtractor(mut payload): JsonExtractor<CountTokensRequest>,
) -> Response {
    if let Err(response) = resolve_defined_cache_route(&state, &route) {
        return response;
    }
    let image_processing = request_image_processing_config(&state);
    if let Err(message) = body_processing::prepare_multimodal_message_sources(
        &state.file_store,
        &mut payload.messages,
        None,
        image_processing,
    )
    .await
    {
        tracing::warn!("dfcache count_tokens 多模态 source 处理失败: {}", message);
        return envelope::error_response(StatusCode::BAD_REQUEST, "invalid_request_error", message);
    }

    let total_tokens = token::count_all_tokens(
        &payload.model,
        payload.system.as_deref(),
        &payload.messages,
        payload.tools.as_deref(),
    ) as i32;

    Json(CountTokensResponse {
        input_tokens: total_tokens.max(1) as i32,
    })
    .into_response()
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
    post_messages_for_endpoint(state, headers, raw_body, "/cc/v1/messages".to_string()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::cache::{self, CacheUsage};
    use crate::anthropic::pricing::PricingCatalog;
    use crate::anthropic::prompt_cache::PromptCacheTracker;
    use crate::anthropic::prompt_cache_creation_control::PromptCacheCreationController;
    use crate::anthropic::types::{Message, Metadata, SystemMessage};
    use crate::anthropic::usage::{UsageRecordQuery, UsageRecorder};
    use crate::kiro::call_trace::{
        AccountRejectReason, KiroCallError, SelectionFailureStage, SelectionFailureSummary,
    };
    use crate::kiro::model::events::MetadataTokenUsage;
    use crate::model::config::{
        CachePointPolicyPatch, CachePolicyConfig, CacheRoutePolicyPatch,
        CacheSimulationPolicyPatch, PromptCacheCreationControlConfig, ReportedUsageFieldPolicy,
        ReportedUsagePathPolicy,
    };
    use serde_json::json;
    use std::collections::BTreeMap;

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

    #[test]
    fn anthropic_content_summary_skips_tool_result_text_for_latest_user() {
        let content = json!([
            {"type": "text", "text": "please answer this"},
            {
                "type": "tool_result",
                "tool_use_id": "toolu_1",
                "content": [{"type": "text", "text": "large command output"}]
            }
        ]);

        let summary = summarize_anthropic_content(&content);

        assert_eq!(summary.kind, "array");
        assert_eq!(summary.text.bytes, "please answer this".len());
        assert_eq!(summary.text.chars, "please answer this".chars().count());
        assert_eq!(summary.text.segments, 1);
        assert_eq!(
            summary.text.hash,
            Some(short_text_hash("please answer this"))
        );
        assert_eq!(summary.tool_result_count, 1);
    }

    #[test]
    fn count_tool_use_blocks_counts_assistant_tool_uses_without_text_hashing() {
        let content = json!([
            {"type": "text", "text": "I will call a tool."},
            {"type": "tool_use", "id": "toolu_1", "name": "Read", "input": {"file_path": "README.md"}},
            {"type": "tool_use", "id": "toolu_2", "name": "Grep", "input": {"pattern": "kiro"}}
        ]);

        assert_eq!(count_tool_use_blocks(&content), 2);
    }

    #[test]
    fn json_stream_sniffer_detects_request_body_invalid_exception() {
        let mut sniffer = JsonStreamErrorSniffer::new(Some("application/json; charset=utf-8"));

        let result = sniffer.inspect(Bytes::from_static(
            br#"{"message":"Invalid tool use format.","reason":"REQUEST_BODY_INVALID"}"#,
        ));

        match result {
            JsonStreamSniffResult::Error(error) => {
                assert_eq!(error.error_type, "invalid_request_error");
                assert!(error.internal_detail.contains("REQUEST_BODY_INVALID"));
                assert!(error.internal_detail.contains("Invalid tool use format."));
                assert!(error.body_preview.contains("Invalid tool use format."));
            }
            _ => panic!("expected JSON stream error"),
        }
    }

    #[test]
    fn json_stream_sniffer_passes_binary_eventstream_mislabeled_as_json() {
        let mut sniffer = JsonStreamErrorSniffer::new(Some("application/json"));
        let chunk = Bytes::from_static(&[0, 0, 0, 16, 0, 0, 0, 0]);

        match sniffer.inspect(chunk.clone()) {
            JsonStreamSniffResult::Pass(passed) => assert_eq!(passed, chunk),
            _ => panic!("expected binary eventstream chunk to pass through"),
        }
    }

    #[test]
    fn json_stream_sniffer_accumulates_split_json_exception() {
        let mut sniffer = JsonStreamErrorSniffer::new(Some("application/json"));

        assert!(matches!(
            sniffer.inspect(Bytes::from_static(br#"{"message":"Too many"#)),
            JsonStreamSniffResult::Pending
        ));

        match sniffer.inspect(Bytes::from_static(
            br#" requests","code":"ThrottlingException"}"#,
        )) {
            JsonStreamSniffResult::Error(error) => {
                assert_eq!(error.error_type, "rate_limit_error");
                assert!(error.internal_detail.contains("ThrottlingException"));
                assert!(error.internal_detail.contains("Too many requests"));
            }
            _ => panic!("expected split JSON stream error"),
        }
    }

    #[test]
    fn claude_code_noop_delta_keepalive_is_version_gated() {
        assert_eq!(
            extract_claude_code_cli_version("claude-cli/2.1.197 (external, cli)"),
            Some("2.1.197")
        );
        assert!(should_use_claude_code_noop_delta_keepalive(Some(
            "claude-cli/2.1.193 (external, cli)"
        )));
        assert!(should_use_claude_code_noop_delta_keepalive(Some(
            "Claude-CLI/2.1.197 (Claude Code)"
        )));
        assert!(!should_use_claude_code_noop_delta_keepalive(Some(
            "claude-cli/2.1.192 (external, cli)"
        )));
        assert!(!should_use_claude_code_noop_delta_keepalive(Some(
            "curl/8.0"
        )));
        assert!(!should_use_claude_code_noop_delta_keepalive(None));
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
    fn defined_cache_route_requires_explicit_configuration() {
        let state = AppState::new(
            Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
            true,
            Arc::new(UsageRecorder::new(10)),
            Arc::new(PromptCacheTracker::default()),
            Arc::new(PromptCacheCreationController::default()),
            PromptCacheSimulationMode::HighCache,
            0.98,
            CompatProfile::ClaudeCode,
            false,
        )
        .with_defined_cache_routes(vec!["/dfcache/cc".to_string()]);

        assert_eq!(
            resolve_defined_cache_route(&state, "cc").unwrap(),
            "/dfcache/cc"
        );
        assert!(resolve_defined_cache_route(&state, "aa").is_err());
        assert!(resolve_defined_cache_route(&state, "aa/b").is_err());
    }

    #[test]
    fn raw_external_route_request_is_preparse_raw_only() {
        let state = AppState::new(
            Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
            true,
            Arc::new(UsageRecorder::new(10)),
            Arc::new(PromptCacheTracker::default()),
            Arc::new(PromptCacheCreationController::default()),
            PromptCacheSimulationMode::HighCache,
            0.98,
            CompatProfile::ClaudeCode,
            false,
        );
        let runtime_config = RequestRuntimeConfig::from_app_state(&state);
        let cache_route = runtime_config.cache_policy_for_path("/cc/v1/messages");
        let raw_body = Bytes::from_static(
            br#"{"model":"client-model","stream":true,"messages":[{"role":"user","content":"hello"}]}"#,
        );

        let route = raw_external_route_request(
            &state,
            &runtime_config,
            &cache_route,
            HeaderMap::new(),
            raw_body.clone(),
            "/cc/v1/messages",
            "req_raw_preparse_test".to_string(),
            UsageRouteSubtype::ExternalFallbackPreflight,
            Some("local_capacity_full".to_string()),
            None,
            Some(json!({"preflightStage":"before_parse"})),
        );

        assert_eq!(route.raw_body, raw_body);
        assert!(route.payload.is_none());
        assert_eq!(
            route.body_mode_filter,
            Some(ExternalPoolRequestBodyMode::RawPassthrough)
        );
        assert_eq!(route.model_hint.as_deref(), Some("client-model"));
        assert_eq!(route.stream_hint, Some(true));
        assert_eq!(route.request_input_tokens, 0);
        assert!(!route.payload_guard_external_enabled);
        assert_eq!(
            route
                .local_preflight
                .as_ref()
                .and_then(|value| value.get("preflightStage"))
                .and_then(|value| value.as_str()),
            Some("before_parse")
        );
    }

    #[test]
    fn raw_external_route_request_applies_non_stream_skip_cache_route() {
        let mut cache_policy = CachePolicyConfig::default();
        cache_policy.path_overrides.insert(
            "/cc".to_string(),
            CacheRoutePolicyPatch {
                cache_type: Some(PromptCacheStrategyType::CurrentHighCache),
                reported_usage: Some(ReportedUsagePathPolicy {
                    skip_non_stream_usage_projection: true,
                    input: ReportedUsageFieldPolicy::sample_input_max(1),
                    ..ReportedUsagePathPolicy::default()
                }),
                ..CacheRoutePolicyPatch::default()
            },
        );
        let state = AppState::new(
            Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
            true,
            Arc::new(UsageRecorder::new(10)),
            Arc::new(PromptCacheTracker::default()),
            Arc::new(PromptCacheCreationController::default()),
            PromptCacheSimulationMode::HighCache,
            0.98,
            CompatProfile::ClaudeCode,
            false,
        )
        .with_cache_policy(cache_policy);
        let runtime_config = RequestRuntimeConfig::from_app_state(&state);
        let cache_route = runtime_config.cache_policy_for_path("/cc/v1/messages");
        assert_eq!(
            cache_route.policy.cache_type,
            PromptCacheStrategyType::CurrentHighCache
        );
        assert!(
            cache_route
                .policy
                .reported_usage
                .skip_non_stream_usage_projection
        );

        let non_stream_route = raw_external_route_request(
            &state,
            &runtime_config,
            &cache_route,
            HeaderMap::new(),
            Bytes::from_static(
                br#"{"model":"client-model","stream":false,"messages":[{"role":"user","content":"hello"}]}"#,
            ),
            "/cc/v1/messages",
            "req_raw_non_stream_skip".to_string(),
            UsageRouteSubtype::ExternalDirectPolicy,
            None,
            Some("direct_policy".to_string()),
            None,
        );

        assert_eq!(
            non_stream_route.prompt_cache_strategy_type,
            PromptCacheStrategyType::NoCache
        );
        assert_eq!(
            non_stream_route.prompt_cache_simulation_mode,
            PromptCacheSimulationMode::Disabled
        );
        assert!(non_stream_route.prompt_cache_route_namespace.is_none());
        assert!(!non_stream_route.reported_usage.default.enabled);
        assert!(
            non_stream_route
                .reported_usage
                .default
                .skip_non_stream_usage_projection
        );

        let stream_route = raw_external_route_request(
            &state,
            &runtime_config,
            &cache_route,
            HeaderMap::new(),
            Bytes::from_static(
                br#"{"model":"client-model","stream":true,"messages":[{"role":"user","content":"hello"}]}"#,
            ),
            "/cc/v1/messages",
            "req_raw_stream_skip".to_string(),
            UsageRouteSubtype::ExternalDirectPolicy,
            None,
            Some("direct_policy".to_string()),
            None,
        );

        assert_eq!(
            stream_route.prompt_cache_strategy_type,
            PromptCacheStrategyType::CurrentHighCache
        );
        assert!(stream_route.reported_usage.default.enabled);
        assert!(
            stream_route
                .reported_usage
                .default
                .skip_non_stream_usage_projection
        );
    }

    fn runtime_config_for_payload_guard(
        mode: PayloadGuardMode,
        enabled: bool,
        max_bytes: usize,
    ) -> RequestRuntimeConfig {
        RequestRuntimeConfig {
            extract_thinking: true,
            thinking_trigger_mode: ThinkingTriggerMode::RealRequest,
            prompt_cache_simulation_mode: PromptCacheSimulationMode::HighCache,
            prompt_cache_target_read_ratio: 0.98,
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
            kiro_cache_point_enabled: false,
            kiro_cache_point_tools_only: true,
            kiro_cache_point_record_plan: true,
            kiro_upstream_stream_idle_timeout_secs: 180,
            image_processing: ImageProcessingConfig::default(),
            body_conversion: BodyConversionConfig::default(),
            missing_max_tokens: MissingMaxTokensConfig::default(),
            payload_shaping: PayloadShapingConfig::default(),
            external_pools: ExternalPoolsConfig::default(),
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
    fn request_body_invalid_tool_format_is_bad_request_diagnostic_error() {
        let message = r#"400 Bad Request {"message":"Invalid tool use format.","reason":"REQUEST_BODY_INVALID"}"#;

        assert!(is_upstream_bad_request_error(message));
        assert!(is_upstream_tool_use_format_error(message));
        assert!(!should_retry_payload_guard_after_error(
            message, 100_000, 460_800,
        ));
    }

    #[test]
    fn thinking_suffix_opus_4_7_uses_adaptive_by_default() {
        let mut payload = messages_request_for_model("claude-opus-4-7-thinking");

        override_thinking_from_model_name(&mut payload);

        let thinking = payload.thinking.expect("thinking should be set");
        assert_eq!(thinking.thinking_type, "adaptive");
        assert_eq!(
            payload
                .output_config
                .expect("output_config should be filled")
                .effort,
            "xhigh"
        );
    }

    #[test]
    fn thinking_suffix_opus_alias_uses_adaptive_by_default() {
        let mut payload = messages_request_for_model("opus-thinking");

        override_thinking_from_model_name(&mut payload);

        let thinking = payload.thinking.expect("thinking should be set");
        assert_eq!(thinking.thinking_type, "adaptive");
        assert_eq!(
            payload
                .output_config
                .expect("output_config should be filled")
                .effort,
            "xhigh"
        );
    }

    #[test]
    fn thinking_suffix_sonnet_4_6_uses_adaptive_by_default() {
        let mut payload = messages_request_for_model("claude-sonnet-4-6-thinking");

        override_thinking_from_model_name(&mut payload);

        let thinking = payload.thinking.expect("thinking should be set");
        assert_eq!(thinking.thinking_type, "adaptive");
        assert_eq!(
            payload
                .output_config
                .expect("output_config should be filled")
                .effort,
            "high"
        );
    }

    #[test]
    fn thinking_suffix_sonnet_alias_uses_adaptive_by_default() {
        let mut payload = messages_request_for_model("sonnet-thinking");

        override_thinking_from_model_name(&mut payload);

        let thinking = payload.thinking.expect("thinking should be set");
        assert_eq!(thinking.thinking_type, "adaptive");
        assert_eq!(
            payload
                .output_config
                .expect("output_config should be filled")
                .effort,
            "high"
        );
    }

    #[test]
    fn thinking_suffix_sonnet_4_5_uses_enabled_by_default() {
        let mut payload = messages_request_for_model("claude-sonnet-4-5-thinking");

        override_thinking_from_model_name(&mut payload);

        let thinking = payload.thinking.expect("thinking should be set");
        assert_eq!(thinking.thinking_type, "enabled");
        assert_eq!(thinking.budget_tokens, 20000);
        assert!(payload.output_config.is_none());
    }

    #[test]
    fn thinking_suffix_preserves_explicit_enabled_without_effort() {
        let mut payload = messages_request_for_model("claude-opus-4-7-thinking");
        payload.thinking = Some(Thinking {
            thinking_type: "enabled".to_string(),
            budget_tokens: 4096,
        });

        override_thinking_from_model_name(&mut payload);

        let thinking = payload.thinking.expect("thinking should be preserved");
        assert_eq!(thinking.thinking_type, "enabled");
        assert_eq!(thinking.budget_tokens, 4096);
        assert!(payload.output_config.is_none());
    }

    #[test]
    fn thinking_suffix_fills_effort_for_explicit_adaptive() {
        let mut payload = messages_request_for_model("claude-sonnet-4-6-thinking");
        payload.thinking = Some(Thinking {
            thinking_type: "adaptive".to_string(),
            budget_tokens: 4096,
        });

        override_thinking_from_model_name(&mut payload);

        let thinking = payload.thinking.expect("thinking should be preserved");
        assert_eq!(thinking.thinking_type, "adaptive");
        assert_eq!(thinking.budget_tokens, 4096);
        assert_eq!(
            payload
                .output_config
                .expect("output_config should be filled")
                .effort,
            "high"
        );
    }

    #[test]
    fn thinking_trigger_real_request_preserves_empty_payload() {
        let mut payload = messages_request_for_model("claude-sonnet-4-6");
        let runtime_config =
            runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);

        apply_thinking_trigger_mode(&mut payload, &runtime_config);

        assert!(payload.thinking.is_none());
        assert!(payload.output_config.is_none());
        assert!(!should_force_visible_thinking(&payload, &runtime_config));
    }

    #[test]
    fn thinking_trigger_real_request_uses_claude_code_ultrathink_signal() {
        let mut payload = messages_request_for_model("claude-sonnet-4-6");
        payload.messages[0].content = json!("ultrathink analyze this issue");
        let runtime_config =
            runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);

        apply_thinking_trigger_mode(&mut payload, &runtime_config);

        let thinking = payload
            .thinking
            .as_ref()
            .expect("ultrathink should inject adaptive thinking");
        assert_eq!(thinking.thinking_type, "adaptive");
        assert_eq!(thinking.budget_tokens, 0);
        assert_eq!(
            payload
                .output_config
                .as_ref()
                .expect("output_config should be filled")
                .effort,
            "high"
        );
        assert!(should_force_visible_thinking(&payload, &runtime_config));
    }

    #[test]
    fn thinking_trigger_real_request_uses_claude_code_deep_reasoning_wrapper() {
        let mut payload = messages_request_for_model("claude-sonnet-4-6");
        payload.messages[0].content = json!(
            r#"<system-reminder>
The user included the keyword "ultrathink", requesting deeper reasoning on this turn. Reason as thoroughly as the task warrants.
</system-reminder>

Return a fix plan."#
        );
        let runtime_config =
            runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);

        apply_thinking_trigger_mode(&mut payload, &runtime_config);

        assert_eq!(
            payload
                .thinking
                .as_ref()
                .expect("Claude Code wrapper should inject thinking")
                .thinking_type,
            "adaptive"
        );
        assert!(should_force_visible_thinking(&payload, &runtime_config));
    }

    #[test]
    fn thinking_trigger_real_request_does_not_treat_think_hard_as_cli_keyword() {
        let mut payload = messages_request_for_model("claude-sonnet-4-6");
        payload.messages[0].content = json!("think hard about this issue");
        let runtime_config =
            runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);

        apply_thinking_trigger_mode(&mut payload, &runtime_config);

        assert!(payload.thinking.is_none());
        assert!(payload.output_config.is_none());
        assert!(!should_force_visible_thinking(&payload, &runtime_config));
    }

    #[test]
    fn thinking_trigger_real_request_respects_explicit_disabled_over_cli_signal() {
        let mut payload = messages_request_for_model("claude-sonnet-4-6");
        payload.messages[0].content = json!("ultrathink analyze this issue");
        payload.thinking = Some(Thinking {
            thinking_type: "disabled".to_string(),
            budget_tokens: 4096,
        });
        let runtime_config =
            runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);

        apply_thinking_trigger_mode(&mut payload, &runtime_config);

        let thinking = payload
            .thinking
            .as_ref()
            .expect("disabled thinking should be preserved");
        assert_eq!(thinking.thinking_type, "disabled");
        assert_eq!(thinking.budget_tokens, 4096);
        assert!(payload.output_config.is_none());
        assert!(!should_force_visible_thinking(&payload, &runtime_config));
    }

    #[test]
    fn thinking_trigger_real_request_ignores_old_user_signal_after_new_turn() {
        let mut payload = messages_request_for_model("claude-sonnet-4-6");
        payload.messages = vec![
            Message {
                role: "user".to_string(),
                content: json!("ultrathink analyze the old turn"),
            },
            Message {
                role: "assistant".to_string(),
                content: json!([{ "type": "text", "text": "done" }]),
            },
            Message {
                role: "user".to_string(),
                content: json!("new plain turn"),
            },
        ];
        let runtime_config =
            runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);

        apply_thinking_trigger_mode(&mut payload, &runtime_config);

        assert!(payload.thinking.is_none());
        assert!(payload.output_config.is_none());
        assert!(!should_force_visible_thinking(&payload, &runtime_config));
    }

    #[test]
    fn thinking_trigger_real_request_keeps_signal_across_tool_result_continuation() {
        let mut payload = messages_request_for_model("claude-sonnet-4-6");
        payload.messages = vec![
            Message {
                role: "user".to_string(),
                content: json!("ultrathink inspect the file then answer"),
            },
            Message {
                role: "assistant".to_string(),
                content: json!([{
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "Read",
                    "input": {"file_path": "src/main.rs"}
                }]),
            },
            Message {
                role: "user".to_string(),
                content: json!([{
                    "type": "tool_result",
                    "tool_use_id": "toolu_1",
                    "content": "file contents"
                }]),
            },
        ];
        let runtime_config =
            runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);

        apply_thinking_trigger_mode(&mut payload, &runtime_config);

        assert_eq!(
            payload
                .thinking
                .as_ref()
                .expect("current turn signal should survive tool-result continuation")
                .thinking_type,
            "adaptive"
        );
        assert!(should_force_visible_thinking(&payload, &runtime_config));
    }

    #[test]
    fn thinking_trigger_always_adds_adaptive_and_effort() {
        let mut payload = messages_request_for_model("claude-sonnet-4-6");
        let mut runtime_config =
            runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);
        runtime_config.thinking_trigger_mode = ThinkingTriggerMode::Always;

        apply_thinking_trigger_mode(&mut payload, &runtime_config);

        let thinking = payload
            .thinking
            .as_ref()
            .expect("thinking should be injected");
        assert_eq!(thinking.thinking_type, "adaptive");
        assert_eq!(thinking.budget_tokens, 0);
        assert_eq!(
            payload
                .output_config
                .as_ref()
                .expect("output_config should be filled")
                .effort,
            "high"
        );
        assert!(should_force_visible_thinking(&payload, &runtime_config));
    }

    #[test]
    fn thinking_trigger_always_preserves_disabled() {
        let mut payload = messages_request_for_model("claude-sonnet-4-6");
        payload.thinking = Some(Thinking {
            thinking_type: "disabled".to_string(),
            budget_tokens: 4096,
        });
        let mut runtime_config =
            runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);
        runtime_config.thinking_trigger_mode = ThinkingTriggerMode::Always;

        apply_thinking_trigger_mode(&mut payload, &runtime_config);

        let thinking = payload
            .thinking
            .as_ref()
            .expect("thinking should be preserved");
        assert_eq!(thinking.thinking_type, "disabled");
        assert_eq!(thinking.budget_tokens, 4096);
        assert!(payload.output_config.is_none());
        assert!(!should_force_visible_thinking(&payload, &runtime_config));
    }

    #[test]
    fn thinking_trigger_always_preserves_enabled_and_output_config() {
        let mut payload = messages_request_for_model("claude-sonnet-4-6");
        payload.thinking = Some(Thinking {
            thinking_type: "enabled".to_string(),
            budget_tokens: 4096,
        });
        payload.output_config = Some(OutputConfig {
            effort: "low".to_string(),
        });
        let mut runtime_config =
            runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);
        runtime_config.thinking_trigger_mode = ThinkingTriggerMode::Always;

        apply_thinking_trigger_mode(&mut payload, &runtime_config);

        let thinking = payload
            .thinking
            .as_ref()
            .expect("thinking should be preserved");
        assert_eq!(thinking.thinking_type, "enabled");
        assert_eq!(thinking.budget_tokens, 4096);
        assert_eq!(
            payload
                .output_config
                .as_ref()
                .expect("output_config should be preserved")
                .effort,
            "low"
        );
        assert!(should_force_visible_thinking(&payload, &runtime_config));
    }

    #[test]
    fn thinking_trigger_always_rewrites_unknown_type_to_adaptive() {
        let mut payload = messages_request_for_model("claude-sonnet-4-6");
        payload.thinking = Some(Thinking {
            thinking_type: "mystery".to_string(),
            budget_tokens: 4096,
        });
        let mut runtime_config =
            runtime_config_for_payload_guard(PayloadGuardMode::OnTooLong, true, 460_800);
        runtime_config.thinking_trigger_mode = ThinkingTriggerMode::Always;

        apply_thinking_trigger_mode(&mut payload, &runtime_config);

        let thinking = payload
            .thinking
            .as_ref()
            .expect("thinking should be rewritten");
        assert_eq!(thinking.thinking_type, "adaptive");
        assert_eq!(thinking.budget_tokens, 0);
        assert_eq!(
            payload
                .output_config
                .as_ref()
                .expect("output_config should be filled")
                .effort,
            "high"
        );
        assert!(should_force_visible_thinking(&payload, &runtime_config));
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
                let policy = reported_cache_usage_policy_for_path(
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
            reported_cache_usage_policy_for_path(
                "/cc/v1/messages",
                PromptCacheSimulationMode::HighCache,
                &reported_usage_config,
                9,
            )
            .expect("policy should apply"),
        );
        assert!((1..=96).contains(&reported.input_tokens));
        assert_eq!(reported.cache_read_input_tokens, 0);
        assert!(reported.cache_creation_input_tokens > 0);
        assert_eq!(reported.output_tokens, 1);

        let raw_reported = usage.with_reported_cache_usage_policy_and_raw(
            reported_cache_usage_policy_for_path(
                "/cc/v1/messages",
                PromptCacheSimulationMode::HighCache,
                &reported_usage_config,
                9,
            )
            .expect("policy should apply"),
            cache::RawUsage::uncached(100_000, 1),
        );
        assert!((1..=96).contains(&raw_reported.input_tokens));
        assert_eq!(raw_reported.cache_read_input_tokens, 0);
        assert!(raw_reported.cache_creation_input_tokens > 0);
    }

    #[test]
    fn path_reported_usage_skip_non_stream_blocks_non_stream_only() {
        let policy = ReportedUsagePathPolicy {
            skip_non_stream_usage_projection: true,
            input: ReportedUsageFieldPolicy::sample_input_max(96),
            ..ReportedUsagePathPolicy::default()
        };

        let non_stream_policy = reported_cache_usage_policy_for_request(
            PromptCacheStrategyType::CurrentHighCache,
            PromptCacheSimulationMode::HighCache,
            &policy,
            7,
            false,
        );
        assert!(non_stream_policy.is_none());

        let stream_policy = reported_cache_usage_policy_for_request(
            PromptCacheStrategyType::CurrentHighCache,
            PromptCacheSimulationMode::HighCache,
            &policy,
            7,
            true,
        );
        assert!(stream_policy.is_some());
    }

    #[test]
    fn path_reported_usage_skip_non_stream_disables_local_cache_route_only_for_non_stream() {
        let prompt_cache = Arc::new(PromptCacheTracker::default());
        let usage_recorder = Arc::new(UsageRecorder::new(10));
        let mut cache_policy = CachePolicyConfig::default();
        cache_policy.path_overrides.insert(
            "/kiro/v1/messages".to_string(),
            CacheRoutePolicyPatch {
                cache_type: Some(PromptCacheStrategyType::KiroRsTool),
                reported_usage: Some(ReportedUsagePathPolicy {
                    skip_non_stream_usage_projection: true,
                    ..ReportedUsagePathPolicy::default()
                }),
                ..CacheRoutePolicyPatch::default()
            },
        );
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
        )
        .with_cache_policy(cache_policy);

        let route =
            RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/kiro/v1/messages");
        assert_eq!(route.policy.cache_type, PromptCacheStrategyType::KiroRsTool);
        assert!(route.policy.reported_usage.skip_non_stream_usage_projection);

        let stream_route = cache_route_for_request_stream(route.clone(), true);
        assert_eq!(
            stream_route.policy.cache_type,
            PromptCacheStrategyType::KiroRsTool
        );
        assert!(
            stream_route
                .policy
                .reported_usage
                .skip_non_stream_usage_projection
        );

        let non_stream_route = cache_route_for_request_stream(route, false);
        assert_eq!(
            non_stream_route.policy.cache_type,
            PromptCacheStrategyType::NoCache
        );
        assert!(non_stream_route.namespace.is_none());
        assert!(!non_stream_route.policy.simulation.enabled);
        assert!(!non_stream_route.policy.creation_control.enabled);
        assert!(!non_stream_route.policy.cache_point.enabled);
        assert!(!non_stream_route.policy.reported_usage.enabled);
        assert!(
            non_stream_route
                .policy
                .reported_usage
                .skip_non_stream_usage_projection
        );
    }

    #[test]
    fn reported_usage_rewrite_shapes_high_cache_downstream_usage() {
        let reported_usage_config = ReportedUsageConfig::default();
        let v1_policy = reported_cache_usage_policy_for_path(
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
            reported_cache_usage_policy_for_path(
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
            tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
            prompt_cache,
            prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
            pricing_catalog: Arc::new(PricingCatalog::new()),
            request_id: "req_reported_limit".to_string(),
            error_id: "req_01reported_limit".to_string(),
            endpoint: "/cc/v1/messages".to_string(),
            stream: false,
            model: "claude-sonnet-4-6".to_string(),
            upstream_model: None,
            model_resolution_source: None,
            model_resolution_note: None,
            requested_max_tokens: 0,
            downstream_stop_reason: Arc::new(Mutex::new(None)),
            conversation_id: Some("session-limit".to_string()),
            prompt_cache_scope_conversation_id: Some("session-limit".to_string()),
            input_tokens: 100_000,
            context_window_tokens: 200_000,
            prompt_cache_profile: None,
            kiro_rs_tool_prompt_cache_plan: None,
            prompt_cache_route_namespace: None,
            prompt_cache_strategy_type: PromptCacheStrategyType::CurrentHighCache,
            simulation_mode: PromptCacheSimulationMode::HighCache,
            prompt_cache_target_read_ratio: 0.95,
            prompt_cache_token_scale: 1.0,
            prompt_cache_max_simulated_input_tokens: 0,
            prompt_cache_cap_jitter_min_tokens: 0,
            prompt_cache_cap_jitter_max_tokens: 0,
            prompt_cache_scale_min_input_tokens: 0,
            prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
            prompt_cache_bounds: PromptCacheBounds::default(),
            reported_cache_usage_policy: reported_cache_usage_policy_for_path(
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
            capacity_weight_units: Arc::new(AtomicU32::new(1)),
            latency: RequestLatencyTraceState::new(),
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
        assert!((1..=96).contains(&upstream_metadata.input_tokens));
        assert_eq!(upstream_metadata.cache_creation_input_tokens, 0);
        assert_eq!(upstream_metadata.cache_read_input_tokens, 0);

        let upstream_metadata_with_raw = usage_context.reported_usage_for_downstream_with_raw(
            usage,
            UsageSource::UpstreamMetadata,
            raw_usage_to_reported_raw(usage),
        );
        assert!((0..=3_300).contains(&upstream_metadata_with_raw.cache_creation_input_tokens));
        assert!((1..=96).contains(&upstream_metadata_with_raw.input_tokens));
        assert_eq!(
            upstream_metadata_with_raw.cache_read_input_tokens,
            usage.cache_read_input_tokens.saturating_add(
                usage
                    .input_tokens
                    .saturating_sub(upstream_metadata_with_raw.input_tokens)
            )
        );
        assert!(upstream_metadata_with_raw.cache_read_input_tokens > usage.cache_read_input_tokens);
    }

    #[test]
    fn upstream_metadata_raw_usage_is_shaped_by_high_cache_reported_usage() {
        let usage_context = RequestUsageContext {
            recorder: Arc::new(UsageRecorder::new(10)),
            tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
            prompt_cache: Arc::new(PromptCacheTracker::default()),
            prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
            pricing_catalog: Arc::new(PricingCatalog::new()),
            request_id: "req_upstream_raw_reported_limit".to_string(),
            error_id: "req_01upstream_raw_reported_limit".to_string(),
            endpoint: "/ha/v1/messages".to_string(),
            stream: false,
            model: "claude-haiku-4-5".to_string(),
            upstream_model: None,
            model_resolution_source: None,
            model_resolution_note: None,
            requested_max_tokens: 0,
            downstream_stop_reason: Arc::new(Mutex::new(None)),
            conversation_id: Some("session-upstream-raw-limit".to_string()),
            prompt_cache_scope_conversation_id: Some("session-upstream-raw-limit".to_string()),
            input_tokens: 1_234,
            context_window_tokens: 200_000,
            prompt_cache_profile: None,
            kiro_rs_tool_prompt_cache_plan: None,
            prompt_cache_route_namespace: None,
            prompt_cache_strategy_type: PromptCacheStrategyType::CurrentHighCache,
            simulation_mode: PromptCacheSimulationMode::HighCache,
            prompt_cache_target_read_ratio: 0.95,
            prompt_cache_token_scale: 1.0,
            prompt_cache_max_simulated_input_tokens: 0,
            prompt_cache_cap_jitter_min_tokens: 0,
            prompt_cache_cap_jitter_max_tokens: 0,
            prompt_cache_scale_min_input_tokens: 0,
            prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
            prompt_cache_bounds: PromptCacheBounds::default(),
            reported_cache_usage_policy: reported_cache_usage_policy(
                PromptCacheStrategyType::CurrentHighCache,
                PromptCacheSimulationMode::HighCache,
                &ReportedUsagePathPolicy {
                    input: ReportedUsageFieldPolicy::sample_input_max(500),
                    ..ReportedUsagePathPolicy::default()
                },
                11,
            ),
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
            capacity_weight_units: Arc::new(AtomicU32::new(1)),
            latency: RequestLatencyTraceState::new(),
        };
        let raw_usage = CacheUsage {
            total_input_tokens: 1_234,
            input_tokens: 1_234,
            output_tokens: 7,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        };

        let reported =
            usage_context.reported_usage_for_downstream(raw_usage, UsageSource::UpstreamMetadata);

        assert!((1..=500).contains(&reported.input_tokens));
        assert_eq!(reported.cache_read_input_tokens, 0);
        assert_eq!(reported.cache_creation_input_tokens, 0);
        assert_eq!(reported.output_tokens, 7);
        assert_eq!(
            reported.total_input_tokens,
            reported
                .input_tokens
                .saturating_add(reported.cache_read_input_tokens)
                .saturating_add(reported.cache_creation_input_tokens)
        );
    }

    #[test]
    fn cc_local_prompt_cache_stream_reported_usage_caps_prod_like_input() {
        let reported_usage_config = ReportedUsageConfig::default();
        let request_input_tokens = 17_241;
        let usage_recorder = Arc::new(UsageRecorder::new(10));
        let usage_context = RequestUsageContext {
            recorder: usage_recorder.clone(),
            tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
            prompt_cache: Arc::new(PromptCacheTracker::default()),
            prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
            pricing_catalog: Arc::new(PricingCatalog::new()),
            request_id: "req_prod_like_cc_reported_usage".to_string(),
            error_id: "req_01prod_like_cc_reported_usage".to_string(),
            endpoint: "/cc/v1/messages".to_string(),
            stream: true,
            model: "claude-opus-4-6".to_string(),
            upstream_model: Some("claude-opus-4.6".to_string()),
            model_resolution_source: None,
            model_resolution_note: None,
            requested_max_tokens: 0,
            downstream_stop_reason: Arc::new(Mutex::new(None)),
            conversation_id: Some("conversation-prod-like".to_string()),
            prompt_cache_scope_conversation_id: Some("conversation-prod-like".to_string()),
            input_tokens: request_input_tokens,
            context_window_tokens: 200_000,
            prompt_cache_profile: None,
            kiro_rs_tool_prompt_cache_plan: None,
            prompt_cache_route_namespace: None,
            prompt_cache_strategy_type: PromptCacheStrategyType::CurrentHighCache,
            simulation_mode: PromptCacheSimulationMode::HighCache,
            prompt_cache_target_read_ratio: 0.99,
            prompt_cache_token_scale: 2.0,
            prompt_cache_max_simulated_input_tokens: 300_000,
            prompt_cache_cap_jitter_min_tokens: 0,
            prompt_cache_cap_jitter_max_tokens: 0,
            prompt_cache_scale_min_input_tokens: 20_000,
            prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
            prompt_cache_bounds: PromptCacheBounds::default(),
            reported_cache_usage_policy: reported_cache_usage_policy_for_path(
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
                split_cached_input: false,
                ..Default::default()
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
            capacity_weight_units: Arc::new(AtomicU32::new(1)),
            latency: RequestLatencyTraceState::new(),
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
            prod_like_usage.cache_read_input_tokens.saturating_add(
                prod_like_usage
                    .input_tokens
                    .saturating_sub(reported.input_tokens)
            )
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
    fn success_usage_record_uses_raw_usage_for_actual_input_diagnostic() {
        let usage_recorder = Arc::new(UsageRecorder::new(10));
        let usage_context = RequestUsageContext {
            recorder: usage_recorder.clone(),
            tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
            prompt_cache: Arc::new(PromptCacheTracker::default()),
            prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
            pricing_catalog: Arc::new(PricingCatalog::new()),
            request_id: "req_context_actual_input".to_string(),
            error_id: "req_01context_actual_input".to_string(),
            endpoint: "/cc/v1/messages".to_string(),
            stream: false,
            model: "claude-sonnet-4-6".to_string(),
            upstream_model: None,
            model_resolution_source: None,
            model_resolution_note: None,
            requested_max_tokens: 0,
            downstream_stop_reason: Arc::new(Mutex::new(None)),
            conversation_id: Some("context-estimate-session".to_string()),
            prompt_cache_scope_conversation_id: None,
            input_tokens: 141,
            context_window_tokens: 200_000,
            prompt_cache_profile: None,
            kiro_rs_tool_prompt_cache_plan: None,
            prompt_cache_route_namespace: None,
            prompt_cache_strategy_type: PromptCacheStrategyType::NoCache,
            simulation_mode: PromptCacheSimulationMode::Disabled,
            prompt_cache_target_read_ratio: 0.95,
            prompt_cache_token_scale: 1.0,
            prompt_cache_max_simulated_input_tokens: 0,
            prompt_cache_cap_jitter_min_tokens: 0,
            prompt_cache_cap_jitter_max_tokens: 0,
            prompt_cache_scale_min_input_tokens: 0,
            prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
            prompt_cache_bounds: PromptCacheBounds::default(),
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
            capacity_weight_units: Arc::new(AtomicU32::new(1)),
            latency: RequestLatencyTraceState::new(),
        };
        let credential_usage =
            usage_context.attach_credential(Some(9), None, false, false, Vec::new());
        let usage = CacheUsage {
            total_input_tokens: 4_275,
            input_tokens: 4_275,
            output_tokens: 1,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        };

        credential_usage.record_success_reported(usage, UsageSource::ContextEstimate, Some(usage));

        let records = usage_recorder.query(UsageRecordQuery::default());
        assert_eq!(records.total, 1);
        let record = records.records.first().expect("usage record should exist");
        assert_eq!(record.total_input_tokens, 4_275);
        assert_eq!(record.compat_input_tokens, 4_275);
        assert_eq!(record.cache_creation_input_tokens, 0);
        assert_eq!(record.cache_read_input_tokens, 0);
        let raw_usage = record.raw_usage.expect("raw usage should be retained");
        assert_eq!(raw_usage.total_input_tokens, 4_275);
    }

    #[test]
    fn kiro_rs_tool_local_prompt_cache_uses_strategy_usage_without_legacy_reported_usage() {
        let usage_recorder = Arc::new(UsageRecorder::new(10));
        let usage_context = RequestUsageContext {
            recorder: usage_recorder.clone(),
            tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
            prompt_cache: Arc::new(PromptCacheTracker::default()),
            prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
            pricing_catalog: Arc::new(PricingCatalog::new()),
            request_id: "req_kiro_strategy_reported_usage".to_string(),
            error_id: "req_01kiro_strategy_reported_usage".to_string(),
            endpoint: "/kiro/v1/messages".to_string(),
            stream: false,
            model: "claude-sonnet-4-6".to_string(),
            upstream_model: None,
            model_resolution_source: None,
            model_resolution_note: None,
            requested_max_tokens: 0,
            downstream_stop_reason: Arc::new(Mutex::new(None)),
            conversation_id: Some("conversation-kiro-strategy".to_string()),
            prompt_cache_scope_conversation_id: Some("conversation-kiro-strategy".to_string()),
            input_tokens: 100_000,
            context_window_tokens: 200_000,
            prompt_cache_profile: None,
            kiro_rs_tool_prompt_cache_plan: None,
            prompt_cache_route_namespace: Some("/kiro".to_string()),
            prompt_cache_strategy_type: PromptCacheStrategyType::KiroRsTool,
            simulation_mode: PromptCacheSimulationMode::Disabled,
            prompt_cache_target_read_ratio: 0.5,
            prompt_cache_token_scale: 3.0,
            prompt_cache_max_simulated_input_tokens: 300_000,
            prompt_cache_cap_jitter_min_tokens: 0,
            prompt_cache_cap_jitter_max_tokens: 0,
            prompt_cache_scale_min_input_tokens: 0,
            prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
            prompt_cache_bounds: PromptCacheBounds::default(),
            reported_cache_usage_policy: reported_cache_usage_policy(
                PromptCacheStrategyType::KiroRsTool,
                PromptCacheSimulationMode::Disabled,
                &ReportedUsagePathPolicy {
                    input: ReportedUsageFieldPolicy::sample_input_max(96),
                    ..ReportedUsagePathPolicy::default()
                },
                7,
            ),
            simulated_usage: Some(cache::CacheSimulation {
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 60_000,
                cache_creation_5m_input_tokens: 0,
                cache_creation_1h_input_tokens: 0,
                target_cache_ratio: None,
                amplification: None,
                split_cached_input: true,
                ..Default::default()
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
            capacity_weight_units: Arc::new(AtomicU32::new(1)),
            latency: RequestLatencyTraceState::new(),
        };

        assert!(usage_context.reported_cache_usage_policy().is_none());
        let usage = cache::build_usage_with_simulation_policy(
            None,
            100_000,
            6,
            usage_context.simulated_usage,
            should_build_local_prompt_cache_usage(
                usage_context.prompt_cache_strategy_type,
                usage_context.simulation_mode,
            ),
        );
        assert_eq!(usage.input_tokens, 40_000);
        assert_eq!(usage.cache_read_input_tokens, 60_000);

        let credential_usage =
            usage_context.attach_credential(Some(131), None, false, false, Vec::new());
        let reported =
            credential_usage.final_reported_usage_for_success(usage, UsageSource::LocalPromptCache);

        assert_eq!(reported.input_tokens, 40_000);
        assert_eq!(reported.cache_read_input_tokens, 60_000);
        assert_eq!(reported.output_tokens, 6);

        let raw_usage = raw_usage_from_metadata_or_estimate(None, 100_000, 6);
        credential_usage.record_success_reported(
            reported,
            UsageSource::LocalPromptCache,
            Some(raw_usage),
        );
        let records = usage_recorder.query(UsageRecordQuery::default());
        assert_eq!(records.total, 1);
        let record = records.records.first().expect("usage record should exist");
        assert_eq!(record.compat_input_tokens, 40_000);
        assert_eq!(record.cache_read_input_tokens, 60_000);
        let raw_usage = record.raw_usage.expect("raw usage should be retained");
        assert_eq!(raw_usage.input_tokens, 100_000);
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
    fn local_latency_trace_records_markers_without_changing_first_output_semantics() {
        let mut usage_context = RequestUsageContext {
            recorder: Arc::new(UsageRecorder::new(10)),
            tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
            prompt_cache: Arc::new(PromptCacheTracker::default()),
            prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
            pricing_catalog: Arc::new(PricingCatalog::new()),
            request_id: "req_latency_trace".to_string(),
            error_id: "req_01latency_trace".to_string(),
            endpoint: "/cc/v1/messages".to_string(),
            stream: true,
            model: "claude-opus-4-8".to_string(),
            upstream_model: Some("claude-opus-4.8".to_string()),
            model_resolution_source: None,
            model_resolution_note: None,
            requested_max_tokens: 0,
            downstream_stop_reason: Arc::new(Mutex::new(None)),
            conversation_id: Some("session-latency".to_string()),
            prompt_cache_scope_conversation_id: Some("session-latency".to_string()),
            input_tokens: 100,
            context_window_tokens: 200_000,
            prompt_cache_profile: None,
            kiro_rs_tool_prompt_cache_plan: None,
            prompt_cache_route_namespace: None,
            prompt_cache_strategy_type: PromptCacheStrategyType::CurrentHighCache,
            simulation_mode: PromptCacheSimulationMode::Disabled,
            prompt_cache_target_read_ratio: 0.95,
            prompt_cache_token_scale: 1.0,
            prompt_cache_max_simulated_input_tokens: 0,
            prompt_cache_cap_jitter_min_tokens: 0,
            prompt_cache_cap_jitter_max_tokens: 0,
            prompt_cache_scale_min_input_tokens: 0,
            prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
            prompt_cache_bounds: PromptCacheBounds::default(),
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
            capacity_weight_units: Arc::new(AtomicU32::new(1)),
            latency: RequestLatencyTraceState::new(),
        };

        usage_context.mark_payload_guard_latency(Duration::from_millis(3));
        usage_context.mark_upstream_header();
        usage_context.mark_first_upstream_chunk();
        usage_context.mark_upstream_bytes_before_first_output(128);
        usage_context.mark_upstream_pending_chunk_before_first_output();
        usage_context.mark_upstream_frame_before_first_output();
        usage_context.mark_upstream_event_before_first_output(
            &Event::Metadata(crate::kiro::model::events::MetadataEvent::default()),
            0,
        );
        usage_context.mark_upstream_frame_before_first_output();
        usage_context.mark_upstream_frame_decode_error_before_first_output();
        usage_context.mark_upstream_event_parse_error_before_first_output();
        usage_context.mark_stream_events(&[
            SseEvent::new("message_start", json!({"type": "message_start"})),
            SseEvent::new(
                "content_block_start",
                json!({
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": {"type": "text", "text": ""}
                }),
            ),
        ]);
        assert!(usage_context.first_token_latency_ms().is_none());

        usage_context.mark_first_upstream_chunk();
        usage_context.mark_stream_events(&[
            SseEvent::new("ping", json!({"type": "ping"})),
            SseEvent::new(
                "content_block_delta",
                json!({
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": {"type": "thinking_delta", "thinking": "thinking"}
                }),
            ),
        ]);
        usage_context.mark_stream_events(&[SseEvent::new(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": 1,
                "delta": {"type": "text_delta", "text": "hello"}
            }),
        )]);
        usage_context.mark_stream_terminal(StreamTerminalReason::Completed);

        let trace = usage_context.latency_trace().expect("latency trace");
        assert_eq!(trace.payload_guard_ms, Some(3));
        assert!(trace.upstream_header_ms.is_some());
        assert!(trace.first_upstream_chunk_ms.is_some());
        assert_eq!(
            trace.first_output_delta_ms,
            usage_context.first_token_latency_ms()
        );
        assert_eq!(trace.first_thinking_delta_ms, trace.first_output_delta_ms);
        assert!(trace.first_visible_text_delta_ms.is_some());
        assert_eq!(trace.chunks_before_first_output, Some(1));
        assert_eq!(trace.events_before_first_output, Some(3));
        assert_eq!(trace.upstream_bytes_before_first_output, Some(128));
        assert_eq!(trace.upstream_frames_before_first_output, Some(2));
        assert_eq!(trace.upstream_events_before_first_output, Some(1));
        assert_eq!(
            trace.upstream_frames_without_downstream_events_before_first_output,
            Some(1)
        );
        assert_eq!(trace.upstream_pending_chunks_before_first_output, Some(1));
        assert_eq!(
            trace.upstream_frame_decode_errors_before_first_output,
            Some(1)
        );
        assert_eq!(
            trace.upstream_event_parse_errors_before_first_output,
            Some(1)
        );
        assert_eq!(
            trace
                .upstream_event_types_before_first_output
                .as_ref()
                .and_then(|counts| counts.get("metadata")),
            Some(&1)
        );
        assert!(trace.stream_gap_to_first_output_ms.is_some());
        assert_eq!(trace.terminal_reason, Some(StreamTerminalReason::Completed));
    }

    #[test]
    fn stream_success_records_requested_max_tokens_and_downstream_stop_reason() {
        let prompt_cache = Arc::new(PromptCacheTracker::default());
        let usage_recorder = Arc::new(UsageRecorder::new(10));
        let state = AppState::new(
            Arc::new(crate::common::auth::RequestApiKeyStore::new(["test-key"])),
            true,
            usage_recorder.clone(),
            prompt_cache,
            Arc::new(PromptCacheCreationController::default()),
            PromptCacheSimulationMode::Disabled,
            0.95,
            CompatProfile::ClaudeCode,
            false,
        );
        let mut payload = messages_request_for_model("claude-sonnet-4-6");
        payload.stream = true;
        payload.max_tokens = 100;

        let usage_context = prepare_usage_context(
            &state,
            RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/cc/v1/messages"),
            "/cc/v1/messages",
            true,
            &payload,
            None,
            Some("conv-stop-reason".to_string()),
            Some("conv-stop-reason".to_string()),
            50,
        );
        let credential_usage = attach_test_credential_usage(usage_context, 1);

        let mut stream_context = StreamContext::new_with_simulation(
            &payload.model,
            50,
            200_000,
            false,
            false,
            HashMap::new(),
            None,
            PromptCacheSimulationMode::Disabled,
        );
        stream_context.set_requested_max_tokens(payload.max_tokens);
        let _initial_events = stream_context.generate_initial_events();
        let mut events = Vec::new();
        let mut assistant_response = crate::kiro::model::events::AssistantResponseEvent::default();
        assistant_response.content = "near token limit".to_string();
        events.extend(
            stream_context.process_kiro_event(&Event::AssistantResponse(assistant_response)),
        );
        events.extend(stream_context.process_kiro_event(&Event::MessageMetadata(
            crate::kiro::model::events::MessageMetadataEvent {
                conversation_id: Some("conv-stop-reason".to_string()),
                utterance_id: Some("utt-stop-reason".to_string()),
                token_usage: Some(MetadataTokenUsage {
                    uncached_input_tokens: 50,
                    output_tokens: 95,
                    total_tokens: 145,
                    cache_read_input_tokens: 0,
                    cache_write_input_tokens: 0,
                }),
            },
        )));
        events.extend(stream_context.generate_final_events());
        assert!(events.iter().any(|event| {
            event.event == "message_delta" && event.data["delta"]["stop_reason"] == "max_tokens"
        }));

        credential_usage.record_success_from_stream(&stream_context);

        let records = usage_recorder.query(UsageRecordQuery::default());
        assert_eq!(records.total, 1);
        let record = records.records.first().expect("usage record should exist");
        assert_eq!(record.requested_max_tokens, Some(100));
        assert_eq!(record.downstream_stop_reason.as_deref(), Some("max_tokens"));
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
            tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
            prompt_cache: prompt_cache.clone(),
            prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
            pricing_catalog: Arc::new(PricingCatalog::new()),
            request_id: "req_v1_policy".to_string(),
            error_id: "req_01v1_policy".to_string(),
            endpoint: "/v1/messages".to_string(),
            stream: false,
            model: "claude-sonnet-4-6".to_string(),
            upstream_model: None,
            model_resolution_source: None,
            model_resolution_note: None,
            requested_max_tokens: 0,
            downstream_stop_reason: Arc::new(Mutex::new(None)),
            conversation_id: Some("session-policy".to_string()),
            prompt_cache_scope_conversation_id: Some("session-policy".to_string()),
            input_tokens: 100_000,
            context_window_tokens: 200_000,
            prompt_cache_profile: None,
            kiro_rs_tool_prompt_cache_plan: None,
            prompt_cache_route_namespace: None,
            prompt_cache_strategy_type: PromptCacheStrategyType::CurrentHighCache,
            simulation_mode: PromptCacheSimulationMode::HighCache,
            prompt_cache_target_read_ratio: 0.95,
            prompt_cache_token_scale: 1.0,
            prompt_cache_max_simulated_input_tokens: 0,
            prompt_cache_cap_jitter_min_tokens: 0,
            prompt_cache_cap_jitter_max_tokens: 0,
            prompt_cache_scale_min_input_tokens: 0,
            prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
            prompt_cache_bounds: PromptCacheBounds::default(),
            reported_cache_usage_policy: reported_cache_usage_policy_for_path(
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
            capacity_weight_units: Arc::new(AtomicU32::new(1)),
            latency: RequestLatencyTraceState::new(),
        };
        let cc_context = RequestUsageContext {
            endpoint: "/cc/v1/messages".to_string(),
            request_id: "req_cc_policy".to_string(),
            error_id: "req_01cc_policy".to_string(),
            reported_cache_usage_policy: reported_cache_usage_policy_for_path(
                "/cc/v1/messages",
                PromptCacheSimulationMode::HighCache,
                &reported_usage_config,
                7,
            ),
            ..v1_context.clone()
        };
        let ha_context = RequestUsageContext {
            endpoint: "/ha/v1/messages".to_string(),
            request_id: "req_ha_policy".to_string(),
            error_id: "req_01ha_policy".to_string(),
            reported_cache_usage_policy: reported_cache_usage_policy_for_path(
                "/ha/v1/messages",
                PromptCacheSimulationMode::HighCache,
                &reported_usage_config,
                7,
            ),
            ..v1_context.clone()
        };
        let na_context = RequestUsageContext {
            endpoint: "/na/v1/messages".to_string(),
            request_id: "req_na_policy".to_string(),
            error_id: "req_01na_policy".to_string(),
            prompt_cache_strategy_type: PromptCacheStrategyType::NoCache,
            simulation_mode: PromptCacheSimulationMode::Disabled,
            reported_cache_usage_policy: reported_cache_usage_policy(
                PromptCacheStrategyType::NoCache,
                PromptCacheSimulationMode::Disabled,
                &reported_usage_config.policy_for_path("/na/v1/messages"),
                7,
            ),
            simulated_source: None,
            ..v1_context.clone()
        };

        assert!(v1_context.reported_cache_usage_policy().is_some());
        assert!(cc_context.reported_cache_usage_policy().is_some());
        assert!(ha_context.reported_cache_usage_policy().is_some());
        assert!(na_context.reported_cache_usage_policy().is_none());

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

        let na_raw = cache::RawUsage::uncached(12_345, usage.output_tokens);
        let na_reported = na_context.reported_usage_for_downstream_with_raw(
            usage,
            UsageSource::UpstreamMetadata,
            na_raw,
        );
        assert_eq!(na_reported.input_tokens, 12_345);
        assert_eq!(na_reported.output_tokens, usage.output_tokens);
        assert_eq!(na_reported.cache_creation_input_tokens, 0);
        assert_eq!(na_reported.cache_read_input_tokens, 0);
        assert_eq!(na_reported.total_input_tokens, 12_345);
    }

    #[test]
    fn creation_control_preserves_reported_usage_input_policy() {
        let usage_recorder = Arc::new(UsageRecorder::new(10));
        let usage_context = RequestUsageContext {
            recorder: usage_recorder,
            tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
            prompt_cache: Arc::new(PromptCacheTracker::default()),
            prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
            pricing_catalog: Arc::new(PricingCatalog::new()),
            request_id: "req_creation_reported_policy".to_string(),
            error_id: "req_01creation_reported_policy".to_string(),
            endpoint: "/ha/v1/messages".to_string(),
            stream: true,
            model: "claude-sonnet-4-6".to_string(),
            upstream_model: None,
            model_resolution_source: None,
            model_resolution_note: None,
            requested_max_tokens: 0,
            downstream_stop_reason: Arc::new(Mutex::new(None)),
            conversation_id: Some("session-creation-policy".to_string()),
            prompt_cache_scope_conversation_id: Some("session-creation-policy".to_string()),
            input_tokens: 100_000,
            context_window_tokens: 200_000,
            prompt_cache_profile: None,
            kiro_rs_tool_prompt_cache_plan: None,
            prompt_cache_route_namespace: None,
            prompt_cache_strategy_type: PromptCacheStrategyType::CurrentHighCache,
            simulation_mode: PromptCacheSimulationMode::HighCache,
            prompt_cache_target_read_ratio: 0.95,
            prompt_cache_token_scale: 1.0,
            prompt_cache_max_simulated_input_tokens: 0,
            prompt_cache_cap_jitter_min_tokens: 0,
            prompt_cache_cap_jitter_max_tokens: 0,
            prompt_cache_scale_min_input_tokens: 0,
            prompt_cache_creation_control: PromptCacheCreationControlConfig {
                max_creation_tokens_per_event: 30_000,
                ..PromptCacheCreationControlConfig::default()
            },
            prompt_cache_bounds: PromptCacheBounds::default(),
            reported_cache_usage_policy: reported_cache_usage_policy(
                PromptCacheStrategyType::CurrentHighCache,
                PromptCacheSimulationMode::HighCache,
                &ReportedUsagePathPolicy {
                    input: ReportedUsageFieldPolicy::sample_input_max(96),
                    ..ReportedUsagePathPolicy::default()
                },
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
            capacity_weight_units: Arc::new(AtomicU32::new(1)),
            latency: RequestLatencyTraceState::new(),
        };
        let credential_usage =
            usage_context.attach_credential(Some(1), None, false, false, Vec::new());
        let usage = CacheUsage {
            total_input_tokens: 150_000,
            input_tokens: 100_000,
            output_tokens: 9,
            cache_creation_input_tokens: 50_000,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 50_000,
            cache_creation_1h_input_tokens: 0,
        };

        let reported = credential_usage
            .canonical_reported_usage_for_success(usage, UsageSource::LocalPromptCache);

        assert!((1..=96).contains(&reported.input_tokens));
        assert!((26_400..30_000).contains(&reported.cache_creation_input_tokens));
        assert_eq!(reported.cache_read_input_tokens, 0);
        assert_eq!(
            reported.total_input_tokens,
            reported
                .input_tokens
                .saturating_add(reported.cache_read_input_tokens)
                .saturating_add(reported.cache_creation_input_tokens)
        );
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
            tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
            prompt_cache,
            prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
            pricing_catalog: Arc::new(PricingCatalog::new()),
            request_id: "req_error_hint".to_string(),
            error_id: "req_01error_hint".to_string(),
            endpoint: "/v1/messages".to_string(),
            stream: false,
            model: "claude-sonnet-4-6".to_string(),
            upstream_model: None,
            model_resolution_source: None,
            model_resolution_note: None,
            requested_max_tokens: 0,
            downstream_stop_reason: Arc::new(Mutex::new(None)),
            conversation_id: Some("session-error".to_string()),
            prompt_cache_scope_conversation_id: Some("session-error".to_string()),
            input_tokens: 4096,
            context_window_tokens: 200_000,
            prompt_cache_profile: None,
            kiro_rs_tool_prompt_cache_plan: None,
            prompt_cache_route_namespace: None,
            prompt_cache_strategy_type: PromptCacheStrategyType::CurrentHighCache,
            simulation_mode: PromptCacheSimulationMode::HighCache,
            prompt_cache_target_read_ratio: 0.95,
            prompt_cache_token_scale: 1.0,
            prompt_cache_max_simulated_input_tokens: 0,
            prompt_cache_cap_jitter_min_tokens: 0,
            prompt_cache_cap_jitter_max_tokens: 0,
            prompt_cache_scale_min_input_tokens: 0,
            prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
            prompt_cache_bounds: PromptCacheBounds::default(),
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
            capacity_weight_units: Arc::new(AtomicU32::new(1)),
            latency: RequestLatencyTraceState::new(),
        };
        usage_context
            .attach_credential(Some(hint.id), hint.label, false, false, Vec::new())
            .with_error_metadata(Some(json!({
                "selectionFailure": {
                    "stage": "rpm_limit",
                    "primaryReason": "rpm_limited"
                }
            })))
            .record_failure(UsageRecordStatus::Error, "api_error", "upstream failed");

        let records = usage_recorder.query(Default::default()).records;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].credential_id, Some(2));
        assert_eq!(
            records[0].credential_label.as_deref(),
            Some("IlmiMiazzi@gmail.com")
        );
        assert_eq!(records[0].error_id.as_deref(), Some("req_01error_hint"));
        assert_eq!(records[0].error_source.as_deref(), Some("local_account"));
        assert_eq!(
            records[0]
                .error_metadata
                .as_ref()
                .and_then(|value| value.pointer("/selectionFailure/primaryReason"))
                .and_then(|value| value.as_str()),
            Some("rpm_limited")
        );
    }

    #[test]
    fn provider_error_metadata_wraps_selection_failure_without_error_id_duplication() {
        let mut reason_counts = BTreeMap::new();
        reason_counts.insert(AccountRejectReason::RpmLimited, 3);
        let summary = SelectionFailureSummary {
            request_id: "req_selection".to_string(),
            route: "/cc/v1/messages".to_string(),
            model: "claude-sonnet-4-6".to_string(),
            stage: SelectionFailureStage::RpmLimit,
            primary_reason: AccountRejectReason::RpmLimited,
            rejected_account_count: 3,
            waitable_account_count: 3,
            retry_after_ms: Some(1000),
            reason_counts,
            sampled_accounts: Vec::new(),
            dispatch_wait_ms: None,
            queue_depth: 0,
            global_in_flight: 0,
        };
        let err: Error = KiroCallError::new("local selection failed", Vec::new())
            .with_selection_failure(Some(summary))
            .into();

        let metadata = provider_error_metadata(&err).expect("selection failure metadata");

        assert_eq!(
            metadata
                .pointer("/selectionFailure/primaryReason")
                .and_then(|value| value.as_str()),
            Some("rpm_limited")
        );
        assert!(metadata.pointer("/errorId").is_none());
        assert!(metadata.pointer("/selectionFailure/errorId").is_none());
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

    #[tokio::test]
    async fn no_available_credentials_error_uses_public_account_message() {
        let response = map_provider_error(
            anyhow::anyhow!("所有凭据均已禁用（0/26）"),
            Some("req_no_account"),
            None,
            None,
        );

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(
            value.pointer("/error/type").and_then(|v| v.as_str()),
            Some("api_error")
        );
        let message = value
            .pointer("/error/message")
            .and_then(|v| v.as_str())
            .expect("error message");
        assert_eq!(message, envelope::PUBLIC_ACCOUNT_UNAVAILABLE_MESSAGE);
        assert_public_error_message_is_normalized(message);
        assert!(message.contains("account"));
        assert!(!message.contains("0/26"));
        assert_eq!(value["request_id"], "req_no_account");
    }

    #[tokio::test]
    async fn generic_provider_error_masks_raw_internal_details() {
        let response = map_provider_error(
            anyhow::anyhow!(
                "流式 API 请求失败（凭据 #37 shadow，请求失败）: 502 Bad Gateway raw upstream body"
            ),
            Some("req_generic_provider"),
            None,
            None,
        );

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(
            value.pointer("/error/type").and_then(|v| v.as_str()),
            Some("api_error")
        );
        let message = value
            .pointer("/error/message")
            .and_then(|v| v.as_str())
            .expect("error message");
        assert_eq!(message, envelope::PUBLIC_PROCESSING_FAILED_MESSAGE);
        assert_public_error_message_is_normalized(message);
        assert!(!message.contains("shadow"));
        assert_eq!(value["request_id"], "req_generic_provider");
    }

    #[tokio::test]
    async fn provider_error_response_exposes_matching_public_error_id() {
        let response = map_provider_error(
            anyhow::anyhow!(
                "流式 API 请求失败（凭据 #37 shadow，请求失败）: 502 Bad Gateway raw upstream body"
            ),
            Some("req_public_error_id"),
            Some("req_01public_error_id"),
            None,
        );

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(
            response
                .headers()
                .get("x-kiro-rs-error-id")
                .and_then(|value| value.to_str().ok()),
            Some("req_01public_error_id")
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("json body");
        assert_eq!(
            value.pointer("/error/type").and_then(|v| v.as_str()),
            Some("api_error")
        );
        let message = value
            .pointer("/error/message")
            .and_then(|v| v.as_str())
            .expect("error message");
        assert!(message.contains(envelope::PUBLIC_PROCESSING_FAILED_MESSAGE));
        assert!(message.contains("error ID: req_01public_error_id"));
        assert!(!message.contains("shadow"));
        assert_public_error_message_is_normalized(message);
    }

    fn assert_public_error_message_is_normalized(message: &str) {
        let lower = message.to_ascii_lowercase();
        for forbidden in [
            "credential",
            "external pool",
            "external_pool",
            "fallback",
            "preflight",
            "upstream",
            "备用池",
            "外部池",
            "凭据",
        ] {
            assert!(
                !lower.contains(forbidden),
                "public message leaked internal term {forbidden:?}: {message}"
            );
        }
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
            tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
            prompt_cache: prompt_cache.clone(),
            prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
            pricing_catalog: Arc::new(PricingCatalog::new()),
            request_id: "req_test".to_string(),
            error_id: "req_01test".to_string(),
            endpoint: "/v1/messages".to_string(),
            stream: true,
            model: payload.model.clone(),
            upstream_model: None,
            model_resolution_source: None,
            model_resolution_note: None,
            requested_max_tokens: 0,
            downstream_stop_reason: Arc::new(Mutex::new(None)),
            conversation_id: Some("session-a".to_string()),
            prompt_cache_scope_conversation_id: Some("session-a".to_string()),
            input_tokens: 4096,
            context_window_tokens: 200_000,
            prompt_cache_profile: profile.clone(),
            kiro_rs_tool_prompt_cache_plan: None,
            prompt_cache_route_namespace: None,
            prompt_cache_strategy_type: PromptCacheStrategyType::CurrentHighCache,
            simulation_mode: PromptCacheSimulationMode::HighCache,
            prompt_cache_target_read_ratio: 0.85,
            prompt_cache_token_scale: 1.0,
            prompt_cache_max_simulated_input_tokens: 0,
            prompt_cache_cap_jitter_min_tokens: 0,
            prompt_cache_cap_jitter_max_tokens: 0,
            prompt_cache_scale_min_input_tokens: 0,
            prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
            prompt_cache_bounds: PromptCacheBounds::default(),
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
            capacity_weight_units: Arc::new(AtomicU32::new(1)),
            latency: RequestLatencyTraceState::new(),
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
            conversation_id: "session-a".to_string(),
            route_namespace: None,
        };
        let second = prompt_cache.compute(Some(scope), profile.as_ref(), 0.85);
        assert!(second.cache_read_input_tokens > 0);
    }

    #[test]
    fn high_cache_zero_metadata_fallback_updates_local_prompt_cache() {
        let prompt_cache = Arc::new(PromptCacheTracker::default());
        let usage_recorder = Arc::new(UsageRecorder::new(10));
        let mut payload = MessagesRequest {
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
            tool_format_debug_recorder: ToolFormatDebugRecorder::disabled(),
            prompt_cache: prompt_cache.clone(),
            prompt_cache_creation_controller: Arc::new(PromptCacheCreationController::default()),
            pricing_catalog: Arc::new(PricingCatalog::new()),
            request_id: "req_high_cache".to_string(),
            error_id: "req_01high_cache".to_string(),
            endpoint: "/v1/messages".to_string(),
            stream: true,
            model: payload.model.clone(),
            upstream_model: None,
            model_resolution_source: None,
            model_resolution_note: None,
            requested_max_tokens: 0,
            downstream_stop_reason: Arc::new(Mutex::new(None)),
            conversation_id: Some("session-high-cache".to_string()),
            prompt_cache_scope_conversation_id: Some("session-high-cache".to_string()),
            input_tokens: 4096,
            context_window_tokens: 200_000,
            prompt_cache_profile: profile.clone(),
            kiro_rs_tool_prompt_cache_plan: None,
            prompt_cache_route_namespace: None,
            prompt_cache_strategy_type: PromptCacheStrategyType::CurrentHighCache,
            simulation_mode: PromptCacheSimulationMode::HighCache,
            prompt_cache_target_read_ratio: 0.95,
            prompt_cache_token_scale: 1.0,
            prompt_cache_max_simulated_input_tokens: 0,
            prompt_cache_cap_jitter_min_tokens: 0,
            prompt_cache_cap_jitter_max_tokens: 0,
            prompt_cache_scale_min_input_tokens: 0,
            prompt_cache_creation_control: PromptCacheCreationControlConfig::default(),
            prompt_cache_bounds: PromptCacheBounds::default(),
            reported_cache_usage_policy: None,
            simulated_usage: Some(cache::CacheSimulation {
                cache_creation_input_tokens: 3968,
                cache_read_input_tokens: 0,
                cache_creation_5m_input_tokens: 3968,
                cache_creation_1h_input_tokens: 0,
                target_cache_ratio: Some(0.95),
                amplification: None,
                split_cached_input: false,
                ..Default::default()
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
            capacity_weight_units: Arc::new(AtomicU32::new(1)),
            latency: RequestLatencyTraceState::new(),
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
            conversation_id: "session-high-cache".to_string(),
            route_namespace: None,
        };
        payload.messages.extend([
            Message {
                role: "assistant".to_string(),
                content: json!("ready"),
            },
            Message {
                role: "user".to_string(),
                content: json!("continue the same session"),
            },
        ]);
        let second_profile = prompt_cache.build_high_cache_profile(&payload, 8192);
        let second = prompt_cache.compute(Some(scope), second_profile.as_ref(), 0.95);
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
            RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/v1/messages"),
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
        let first_metadata = MetadataTokenUsage {
            uncached_input_tokens: 4096,
            output_tokens: 1,
            total_tokens: 4097,
            cache_read_input_tokens: 0,
            cache_write_input_tokens: 0,
        };
        let first_source =
            first_usage.usage_source(&first_usage_body, Some(&first_metadata), false);
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
            RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/v1/messages"),
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
    fn kiro_rs_tool_route_strategy_misses_first_then_reads_after_success() {
        let prompt_cache = Arc::new(PromptCacheTracker::default());
        let usage_recorder = Arc::new(UsageRecorder::new(10));
        let mut cache_policy = CachePolicyConfig::default();
        cache_policy.path_overrides.insert(
            "/kiro/v1/messages".to_string(),
            CacheRoutePolicyPatch {
                cache_type: Some(PromptCacheStrategyType::KiroRsTool),
                ..CacheRoutePolicyPatch::default()
            },
        );
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
        )
        .with_cache_policy(cache_policy);
        let session_id = "8bb5523b-ec7c-4540-a9ca-beb6d79f1552";
        let mut first_payload = MessagesRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 16,
            messages: vec![Message {
                role: "user".to_string(),
                content: json!("start kiro strategy session"),
            }],
            stream: false,
            system: Some(vec![SystemMessage {
                text: "stable kiro strategy system prompt ".repeat(700),
                cache_control: Some(json!({"type": "ephemeral"})),
            }]),
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: Some(Metadata {
                user_id: Some(format!("user_test_account__session_{session_id}")),
            }),
        };
        let cache_route =
            RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/kiro/v1/messages");
        assert_eq!(
            cache_route.policy.cache_type,
            PromptCacheStrategyType::KiroRsTool
        );
        let first_context = prepare_usage_context(
            &state,
            cache_route,
            "/kiro/v1/messages",
            false,
            &first_payload,
            None,
            Some(session_id.to_string()),
            prompt_cache_scope_conversation_id(
                PromptCacheStrategyType::KiroRsTool,
                PromptCacheSimulationMode::Disabled,
                &first_payload,
            ),
            4096,
        );

        assert_eq!(
            first_context.simulation_mode,
            PromptCacheSimulationMode::Disabled
        );
        assert_eq!(
            first_context.prompt_cache_scope_conversation_id.as_deref(),
            Some(session_id)
        );
        assert!(first_context.kiro_rs_tool_prompt_cache_plan.is_some());
        let first_simulation = first_context
            .simulated_usage
            .expect("first kiro request should project cache creation");
        assert!(first_simulation.cache_creation_input_tokens > 0);
        assert_eq!(first_simulation.cache_read_input_tokens, 0);
        let first_usage = first_context.attach_credential(Some(1), None, false, false, Vec::new());
        let first_usage_body = cache::build_usage_with_simulation_policy(
            None,
            4096,
            1,
            first_usage.request.simulated_usage,
            true,
        );
        assert!(first_usage_body.cache_creation_input_tokens > 0);
        assert_eq!(first_usage_body.cache_read_input_tokens, 0);
        assert!((32..=4_096).contains(&first_usage_body.input_tokens));
        let first_source = first_usage.usage_source(&first_usage_body, None, false);
        assert_eq!(first_source, UsageSource::LocalPromptCache);
        first_usage.record_success(first_usage_body, first_source, false);

        first_payload.messages.extend([
            Message {
                role: "assistant".to_string(),
                content: json!("ready"),
            },
            Message {
                role: "user".to_string(),
                content: json!("continue the same kiro strategy session"),
            },
        ]);
        let second_context = prepare_usage_context(
            &state,
            RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/kiro/v1/messages"),
            "/kiro/v1/messages",
            false,
            &first_payload,
            None,
            Some(session_id.to_string()),
            prompt_cache_scope_conversation_id(
                PromptCacheStrategyType::KiroRsTool,
                PromptCacheSimulationMode::Disabled,
                &first_payload,
            ),
            8192,
        );
        assert!(second_context.kiro_rs_tool_prompt_cache_plan.is_some());
        let second_simulation = second_context
            .simulated_usage
            .expect("second kiro request should project a cache read");
        assert!(second_simulation.cache_read_input_tokens > 0);
        let second_usage =
            cache::build_usage_with_simulation_policy(None, 8192, 1, Some(second_simulation), true);
        assert!(second_usage.cache_read_input_tokens > 0);
        assert!((32..=4_096).contains(&second_usage.input_tokens));
        assert_eq!(
            second_usage.input_tokens
                + second_usage.cache_creation_input_tokens
                + second_usage.cache_read_input_tokens,
            second_usage.total_input_tokens
        );
        assert_eq!(second_usage.total_input_tokens, 8192);
    }

    #[test]
    fn kiro_rs_tool_route_strategy_commits_without_credential_id() {
        let prompt_cache = Arc::new(PromptCacheTracker::default());
        let usage_recorder = Arc::new(UsageRecorder::new(10));
        let mut cache_policy = CachePolicyConfig::default();
        cache_policy.path_overrides.insert(
            "/kiro/v1/messages".to_string(),
            CacheRoutePolicyPatch {
                cache_type: Some(PromptCacheStrategyType::KiroRsTool),
                ..CacheRoutePolicyPatch::default()
            },
        );
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
        )
        .with_cache_policy(cache_policy);
        let session_id = "8bb5523b-ec7c-4540-a9ca-beb6d79f1552";
        let mut payload = MessagesRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 16,
            messages: vec![Message {
                role: "user".to_string(),
                content: json!("start kiro no credential session"),
            }],
            stream: false,
            system: Some(vec![SystemMessage {
                text: "stable kiro no credential system prompt ".repeat(700),
                cache_control: Some(json!({"type": "ephemeral"})),
            }]),
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: Some(Metadata {
                user_id: Some(format!("user_test_account__session_{session_id}")),
            }),
        };

        let first_context = prepare_usage_context(
            &state,
            RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/kiro/v1/messages"),
            "/kiro/v1/messages",
            false,
            &payload,
            None,
            Some(session_id.to_string()),
            prompt_cache_scope_conversation_id(
                PromptCacheStrategyType::KiroRsTool,
                PromptCacheSimulationMode::Disabled,
                &payload,
            ),
            4096,
        );
        let first_usage_body = cache::build_usage_with_simulation_policy(
            None,
            4096,
            1,
            first_context.simulated_usage,
            true,
        );
        let first_usage = first_context.attach_credential(None, None, false, false, Vec::new());
        let first_source = first_usage.usage_source(&first_usage_body, None, false);
        assert_eq!(first_source, UsageSource::LocalPromptCache);
        first_usage.record_success(first_usage_body, first_source, false);

        payload.messages.extend([
            Message {
                role: "assistant".to_string(),
                content: json!("ready"),
            },
            Message {
                role: "user".to_string(),
                content: json!("continue no credential session"),
            },
        ]);
        let second_context = prepare_usage_context(
            &state,
            RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/kiro/v1/messages"),
            "/kiro/v1/messages",
            false,
            &payload,
            None,
            Some(session_id.to_string()),
            prompt_cache_scope_conversation_id(
                PromptCacheStrategyType::KiroRsTool,
                PromptCacheSimulationMode::Disabled,
                &payload,
            ),
            8192,
        );
        let second_simulation = second_context
            .simulated_usage
            .expect("second kiro request should read cache without credential id");
        assert!(second_simulation.cache_read_input_tokens > 0);
        let second_usage =
            cache::build_usage_with_simulation_policy(None, 8192, 1, Some(second_simulation), true);
        assert!((32..=4_096).contains(&second_usage.input_tokens));
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
        let (simulation, source) =
            build_simulated_usage(PromptCacheSimulationMode::Disabled, None, None);

        assert!(simulation.is_none());
        assert!(source.is_none());

        let context = prepare_usage_context(
            &state,
            RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/v1/messages"),
            "/v1/messages",
            true,
            &payload,
            None,
            Some("random-conversation".to_string()),
            prompt_cache_scope_conversation_id(
                PromptCacheStrategyType::CurrentHighCache,
                state.prompt_cache_simulation_mode,
                &payload,
            ),
            4096,
        );
        assert!(context.prompt_cache_profile.is_none());
        assert!(context.prompt_cache_scope_conversation_id.is_none());

        let credential_usage = attach_test_credential_usage(context, 1);
        assert!(credential_usage.request.simulated_usage.is_none());
        assert!(credential_usage.request.simulated_source.is_none());
    }

    #[test]
    fn builtin_na_path_does_not_build_local_profile_or_reporting_policy() {
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
            RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/na/v1/messages"),
            "/na/v1/messages",
            false,
            &payload,
            None,
            Some("conversation-id".to_string()),
            prompt_cache_scope_conversation_id(
                PromptCacheStrategyType::NoCache,
                PromptCacheSimulationMode::Disabled,
                &payload,
            ),
            4096,
        );

        assert_eq!(
            context.prompt_cache_strategy_type,
            PromptCacheStrategyType::NoCache
        );
        assert_eq!(context.simulation_mode, PromptCacheSimulationMode::Disabled);
        assert!(context.prompt_cache_profile.is_none());
        assert!(context.kiro_rs_tool_prompt_cache_plan.is_none());
        assert!(context.prompt_cache_scope_conversation_id.is_none());
        assert_eq!(context.prompt_cache_route_namespace, None);
        assert!(context.simulated_usage.is_none());
        assert!(context.simulated_source.is_none());
        assert!(context.reported_cache_usage_policy.is_none());
    }

    #[test]
    fn no_cache_route_does_not_build_cache_profile_plan_or_shape_reporting() {
        let prompt_cache = Arc::new(PromptCacheTracker::default());
        let usage_recorder = Arc::new(UsageRecorder::new(10));
        let mut cache_policy = CachePolicyConfig::default();
        cache_policy.path_overrides.insert(
            "/plain".to_string(),
            CacheRoutePolicyPatch {
                cache_type: Some(PromptCacheStrategyType::NoCache),
                simulation: Some(CacheSimulationPolicyPatch {
                    enabled: Some(true),
                    ..CacheSimulationPolicyPatch::default()
                }),
                creation_control: Some(PromptCacheCreationControlConfig::default()),
                reported_usage: Some(ReportedUsagePathPolicy {
                    input: ReportedUsageFieldPolicy::sample_input_max(32),
                    ..ReportedUsagePathPolicy::default()
                }),
                cache_point: Some(CachePointPolicyPatch {
                    enabled: Some(true),
                    ..CachePointPolicyPatch::default()
                }),
                ..CacheRoutePolicyPatch::default()
            },
        );
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
        )
        .with_cache_policy(cache_policy);
        let payload = MessagesRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 16,
            messages: vec![Message {
                role: "user".to_string(),
                content: json!("plain request should not enter prompt cache"),
            }],
            stream: false,
            system: Some(vec![SystemMessage {
                text: "cacheable prompt block ".repeat(700),
                cache_control: Some(json!({"type": "ephemeral"})),
            }]),
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let cache_route = RequestRuntimeConfig::from_app_state(&state)
            .cache_policy_for_path("/plain/v1/messages");
        assert_eq!(
            cache_route.policy.cache_type,
            PromptCacheStrategyType::NoCache
        );
        assert_eq!(cache_route.namespace, None);
        assert_eq!(
            prompt_cache_simulation_mode_for_policy(&cache_route.policy),
            PromptCacheSimulationMode::Disabled
        );
        assert_eq!(
            prompt_cache_converter_mode_for_policy(&cache_route.policy),
            PromptCacheSimulationMode::Disabled
        );
        assert!(!cache_route.policy.cache_point.enabled);

        let context = prepare_usage_context(
            &state,
            cache_route,
            "/plain/v1/messages",
            false,
            &payload,
            None,
            Some("plain-session".to_string()),
            prompt_cache_scope_conversation_id(
                PromptCacheStrategyType::NoCache,
                PromptCacheSimulationMode::Disabled,
                &payload,
            ),
            4096,
        );

        assert_eq!(
            context.prompt_cache_strategy_type,
            PromptCacheStrategyType::NoCache
        );
        assert_eq!(context.simulation_mode, PromptCacheSimulationMode::Disabled);
        assert!(context.prompt_cache_profile.is_none());
        assert!(context.kiro_rs_tool_prompt_cache_plan.is_none());
        assert!(context.prompt_cache_scope_conversation_id.is_none());
        assert_eq!(context.prompt_cache_route_namespace, None);
        assert!(context.simulated_usage.is_none());
        assert!(context.simulated_source.is_none());
        assert!(context.reported_cache_usage_policy.is_none());

        let upstream_raw = CacheUsage {
            total_input_tokens: 4_165,
            input_tokens: 4_165,
            output_tokens: 7,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        };
        let reported = context.reported_usage_for_downstream_with_raw(
            cache::CacheUsage {
                total_input_tokens: 125_000,
                input_tokens: 100_000,
                output_tokens: 7,
                cache_creation_input_tokens: 20_000,
                cache_read_input_tokens: 5_000,
                cache_creation_5m_input_tokens: 20_000,
                cache_creation_1h_input_tokens: 0,
            },
            UsageSource::UpstreamMetadata,
            raw_usage_to_reported_raw(upstream_raw),
        );
        assert_eq!(reported.input_tokens, 4_165);
        assert_eq!(reported.output_tokens, 7);
        assert_eq!(reported.cache_creation_input_tokens, 0);
        assert_eq!(reported.cache_read_input_tokens, 0);
    }

    #[test]
    fn no_cache_disabled_reported_usage_preserves_upstream_metadata_usage() {
        let prompt_cache = Arc::new(PromptCacheTracker::default());
        let usage_recorder = Arc::new(UsageRecorder::new(10));
        let mut reported_usage = ReportedUsageConfig::default();
        reported_usage
            .path_overrides
            .insert("/na".to_string(), ReportedUsagePathPolicy::disabled());
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
        )
        .with_reported_usage(reported_usage);
        let payload = MessagesRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 16,
            messages: vec![Message {
                role: "user".to_string(),
                content: json!("no-cache request should preserve upstream metadata usage"),
            }],
            stream: true,
            system: Some(vec![SystemMessage {
                text: "large no-cache system prompt ".repeat(700),
                cache_control: Some(json!({"type": "ephemeral"})),
            }]),
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let context = prepare_usage_context(
            &state,
            RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/na/v1/messages"),
            "/na/v1/messages",
            true,
            &payload,
            None,
            Some("na-disabled-reported".to_string()),
            prompt_cache_scope_conversation_id(
                PromptCacheStrategyType::NoCache,
                PromptCacheSimulationMode::Disabled,
                &payload,
            ),
            32_000,
        );

        assert_eq!(
            context.prompt_cache_strategy_type,
            PromptCacheStrategyType::NoCache
        );
        assert!(context.reported_cache_usage_policy.is_none());

        let context_usage = CacheUsage {
            total_input_tokens: 32_000,
            input_tokens: 32_000,
            output_tokens: 3,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        };
        let upstream_raw = CacheUsage {
            total_input_tokens: 10,
            input_tokens: 10,
            output_tokens: 3,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        };

        let reported = context.reported_usage_for_downstream_with_raw(
            context_usage,
            UsageSource::UpstreamMetadata,
            raw_usage_to_reported_raw(upstream_raw),
        );

        assert_eq!(reported.input_tokens, 10);
        assert_eq!(reported.output_tokens, 3);
        assert_eq!(reported.cache_read_input_tokens, 0);
        assert_eq!(reported.cache_creation_input_tokens, 0);
    }

    #[test]
    fn no_cache_canonical_record_keeps_upstream_raw_when_default_reporting_exists() {
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
        let payload = MessagesRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 16,
            messages: vec![Message {
                role: "user".to_string(),
                content: json!("no-cache request should not be expanded by record shaping"),
            }],
            stream: true,
            system: Some(vec![SystemMessage {
                text: "large no-cache system prompt ".repeat(700),
                cache_control: Some(json!({"type": "ephemeral"})),
            }]),
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        };

        let context = prepare_usage_context(
            &state,
            RequestRuntimeConfig::from_app_state(&state).cache_policy_for_path("/na/v1/messages"),
            "/na/v1/messages",
            true,
            &payload,
            None,
            Some("na-default-reported".to_string()),
            prompt_cache_scope_conversation_id(
                PromptCacheStrategyType::NoCache,
                PromptCacheSimulationMode::Disabled,
                &payload,
            ),
            32_000,
        );

        assert_eq!(
            context.prompt_cache_strategy_type,
            PromptCacheStrategyType::NoCache
        );
        assert!(context.reported_cache_usage_policy.is_none());

        let context_usage = CacheUsage {
            total_input_tokens: 32_000,
            input_tokens: 32_000,
            output_tokens: 3,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        };
        let upstream_raw = CacheUsage {
            total_input_tokens: 10,
            input_tokens: 10,
            output_tokens: 3,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            cache_creation_5m_input_tokens: 0,
            cache_creation_1h_input_tokens: 0,
        };
        let credential_context = context.attach_credential(Some(1), None, false, false, Vec::new());

        let reported = credential_context.canonical_reported_usage_for_success_with_raw(
            context_usage,
            UsageSource::UpstreamMetadata,
            upstream_raw,
        );

        assert_eq!(reported.input_tokens, 10);
        assert_eq!(reported.output_tokens, 3);
        assert_eq!(reported.cache_read_input_tokens, 0);
        assert_eq!(reported.cache_creation_input_tokens, 0);
    }

    fn attach_test_credential_usage(
        mut usage_context: RequestUsageContext,
        credential_id: u64,
    ) -> CredentialUsageContext {
        let scope = usage_context
            .prompt_cache_scope_conversation_id
            .as_ref()
            .map(|conversation_id| {
                let _ = credential_id;
                PromptCacheScope::new(conversation_id.clone(), None)
            });
        let prompt_usage = usage_context.prompt_cache.compute_with_bounds(
            scope,
            usage_context.prompt_cache_profile.as_ref(),
            usage_context.prompt_cache_target_read_ratio,
            usage_context.prompt_cache_bounds,
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
    fn external_fallback_classifier_can_use_retry_stage_attempts_after_payload_guard_retry() {
        let config = ExternalPoolsConfig::default();
        let prior_too_long_attempt = KiroCredentialAttempt::new(
            0,
            63,
            Some("account@example.com".to_string()),
            Some(StatusCode::BAD_REQUEST),
            "fail",
            Some("client_error"),
            Some(
                r#"400 Bad Request {"message":"Input is too long.","reason":"CONTENT_LENGTH_EXCEEDS_THRESHOLD"}"#,
            ),
            1200,
        );
        let capacity_message = "本地账号调度容量暂不可用（可用: 7/29, 临时可调度: 0, global_credential_max_concurrent_requests=10, effective_credential_max_concurrent_requests=50, retry_after_secs=1）";

        let diagnostic_attempts =
            merge_credential_attempts(vec![prior_too_long_attempt.clone()], Vec::new());
        assert_eq!(
            classify_local_error_for_external_fallback(
                capacity_message,
                &diagnostic_attempts,
                &config,
            ),
            None
        );

        let retry_stage_attempts = Vec::new();
        assert_eq!(
            classify_local_error_for_external_fallback(
                capacity_message,
                &retry_stage_attempts,
                &config,
            )
            .as_deref(),
            Some("local_capacity_exhausted")
        );

        let retry_bad_request_attempt = vec![KiroCredentialAttempt::new(
            0,
            64,
            Some("retry@example.com".to_string()),
            Some(StatusCode::BAD_REQUEST),
            "fail",
            Some("client_error"),
            Some("bad request"),
            10,
        )];
        assert_eq!(
            classify_local_error_for_external_fallback(
                capacity_message,
                &retry_bad_request_attempt,
                &config,
            ),
            None
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
            error_id: "req_01rate_limit".to_string(),
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
            Some("external_rate_limit")
        );
        assert_eq!(
            local_rescue_reason_after_external_error(
                &config,
                &rate_limit,
                Some("local_capacity_exhausted")
            ),
            Some("external_rate_limit")
        );

        let timeout = ExternalPoolFinalError {
            status: StatusCode::BAD_GATEWAY,
            response_error_type: "api_error".to_string(),
            route_error_type: "network_error".to_string(),
            message: "stream idle timeout".to_string(),
            error_id: "req_01timeout".to_string(),
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
            error_id: "req_01capacity".to_string(),
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
            error_id: "req_01bad_request".to_string(),
            retryable: false,
            attempts: Vec::new(),
            pool_id: Some(1),
            pool_name: Some("backup".to_string()),
        };
        assert_eq!(
            local_rescue_reason_after_external_error(&config, &bad_request, None),
            Some("external_bad_request")
        );

        let mut disabled = config.clone();
        disabled.external_pool_local_rescue_enabled = false;
        assert_eq!(
            local_rescue_reason_after_external_error(&disabled, &rate_limit, None),
            None
        );

        let mut direct = config.clone();
        direct.external_direct_policy_enabled = true;
        assert_eq!(
            local_rescue_reason_after_external_error(&direct, &rate_limit, None),
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

        let server_error = ExternalPoolFinalError {
            status: StatusCode::BAD_GATEWAY,
            response_error_type: "api_error".to_string(),
            route_error_type: "server_error".to_string(),
            message: "external upstream failed".to_string(),
            error_id: "req_01server".to_string(),
            retryable: true,
            attempts: Vec::new(),
            pool_id: Some(1),
            pool_name: Some("backup".to_string()),
        };
        assert_eq!(
            local_rescue_reason_after_external_error(
                &no_capacity,
                &server_error,
                Some("local_transient_exhausted")
            ),
            Some("external_error")
        );
        assert_eq!(
            local_rescue_reason_after_external_error(
                &no_capacity,
                &server_error,
                Some("local_capacity_exhausted")
            ),
            Some("external_error")
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
                body_processing::ensure_safe_remote_url(url).is_err(),
                "{url} should be blocked"
            );
        }
    }
}
