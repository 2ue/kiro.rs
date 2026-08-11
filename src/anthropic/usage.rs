use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration as StdDuration, Instant};

use chrono::{
    DateTime, Datelike, Duration as ChronoDuration, FixedOffset, NaiveDate, TimeZone, Timelike, Utc,
};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::runtime::{Runtime, RuntimeFlavor};
use tokio::sync::{Notify, Semaphore, mpsc};
use tokio::task::JoinHandle;

use crate::common::upstream_error::RawUpstreamError;
use crate::kiro::call_trace::{KiroCredentialAttempt, summarize_attempts};
use crate::storage::postgres::PostgresUsageStore;
use crate::storage::redis_cache::RedisStore;

const DEFAULT_QUERY_LIMIT: usize = 100;
const DEFAULT_PAGE_QUERY_LIMIT: usize = 20;
const MAX_QUERY_LIMIT: usize = 1000;
const USAGE_WRITER_QUEUE_CAPACITY: usize = 4096;
const USAGE_WRITER_MAX_ATTEMPTS: u32 = 3;
const USAGE_WRITER_BATCH_MAX: usize = 64;
const USAGE_WRITER_BATCH_COALESCE_DELAY: StdDuration = StdDuration::from_millis(25);
const USAGE_WRITER_POSTGRES_TIMEOUT_SECS: u64 = 5;
const USAGE_REDIS_WRITER_QUEUE_CAPACITY: usize = 4096;
const USAGE_REDIS_WRITER_BATCH_MAX: usize = 64;
const USAGE_REDIS_WRITER_BATCH_COALESCE_DELAY: StdDuration = StdDuration::from_millis(10);
const USAGE_REDIS_WRITER_TIMEOUT_SECS: u64 = 2;
const USAGE_WRITER_ABORT_JOIN_TIMEOUT: StdDuration = StdDuration::from_millis(100);
const USAGE_DASHBOARD_POSTGRES_TIMEOUT_SECS: u64 = 120;
const USAGE_DASHBOARD_REDIS_TIMEOUT_SECS: u64 = 2;
const USAGE_DASHBOARD_MAX_CONCURRENT_QUERIES: usize = 2;
const USAGE_DASHBOARD_GATE_WAIT_MS: u64 = 30_000;
const USAGE_POSTGRES_FALLBACK_CACHE_TTL: StdDuration = StdDuration::from_secs(1);
const ERROR_DIAGNOSTIC_MAX_TEXT_BYTES: usize = 2048;
const ERROR_DIAGNOSTIC_MAX_METADATA_BYTES: usize = 8192;
const ERROR_DIAGNOSTIC_MAX_STRING_BYTES: usize = 512;
const ERROR_DIAGNOSTIC_MAX_ARRAY_ITEMS: usize = 20;
const REQUEST_REJECTION_ERROR_TYPE: &str = "request_rejection";
const REQUEST_REJECTION_ERROR_MESSAGE: &str = "request rejected before upstream dispatch";
pub const REALTIME_USAGE_WINDOW_SECS: u32 = 60;
pub const DEFAULT_USAGE_DASHBOARD_TIMEZONE: &str = "Asia/Shanghai";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum UsageRecordStatus {
    Success,
    Error,
    StreamError,
    UpstreamTimeout,
    ClientDropped,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UsageRouteKind {
    LocalCredential,
    ExternalPool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UsageRouteSubtype {
    LocalSuccess,
    LocalErrorNoFallback,
    LocalRescueAfterExternal,
    ExternalFallbackPreflight,
    ExternalFallbackAfterLocalAttempts,
    ExternalDirectPolicy,
    ExternalError,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExternalPoolAttempt {
    pub attempt: u32,
    pub pool_id: u64,
    pub pool_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbound_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    pub action: String,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_upstream_error: Option<RawUpstreamError>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExternalPoolUsageSnapshot {
    pub total_input_tokens: i32,
    pub input_tokens: i32,
    pub billable_input_tokens: i32,
    pub output_tokens: i32,
    pub cache_read_input_tokens: i32,
    pub cache_creation_input_tokens: i32,
    pub cache_creation_5m_input_tokens: i32,
    pub cache_creation_1h_input_tokens: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExternalPoolBilling {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_input_tokens: Option<i32>,
    pub raw_usage: ExternalPoolUsageSnapshot,
    #[serde(default)]
    pub shaped_usage: ExternalPoolUsageSnapshot,
    pub reported_usage: ExternalPoolUsageSnapshot,
    #[serde(default)]
    pub usage_projection_applied: bool,
    pub raw_cost_usd: f64,
    #[serde(default)]
    pub shaped_cost_usd: f64,
    #[serde(default)]
    pub uplifted_cost_usd: f64,
    #[serde(default)]
    pub profit_usd: f64,
    #[serde(default)]
    pub reported_cost_usd: f64,
    #[serde(default)]
    pub billable_cost_usd: f64,
    #[serde(default)]
    pub cost_floor_delta_usd: f64,
    #[serde(default)]
    pub cost_floor_applied: bool,
    pub pricing_available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing_model: Option<String>,
    pub usage_projection_mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_response_mode: Option<String>,
    #[serde(default)]
    pub usage_estimated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_estimate_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_candidate_path: Option<String>,
    #[serde(default)]
    pub body_usage_projection_applied: bool,
}

impl ExternalPoolBilling {
    pub fn effective_shaped_cost_usd(&self) -> f64 {
        if self.pricing_available && self.shaped_cost_usd == 0.0 && self.reported_cost_usd > 0.0 {
            self.reported_cost_usd
        } else {
            self.shaped_cost_usd
        }
    }

    pub fn effective_uplifted_cost_usd(&self) -> f64 {
        if self.pricing_available && self.uplifted_cost_usd == 0.0 && self.reported_cost_usd > 0.0 {
            self.reported_cost_usd
        } else {
            self.uplifted_cost_usd
        }
    }

    pub fn effective_profit_usd(&self) -> f64 {
        self.effective_uplifted_cost_usd() - self.raw_cost_usd
    }
}

impl UsageRecordStatus {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "success" => Some(Self::Success),
            "error" => Some(Self::Error),
            "stream_error" => Some(Self::StreamError),
            "upstream_timeout" => Some(Self::UpstreamTimeout),
            "client_dropped" => Some(Self::ClientDropped),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum UsageSource {
    UpstreamMetadata,
    LocalPromptCache,
    ContextEstimate,
    RequestEstimate,
    None,
}

impl UsageSource {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "upstream_metadata" => Some(Self::UpstreamMetadata),
            "local_prompt_cache" => Some(Self::LocalPromptCache),
            "context_estimate" => Some(Self::ContextEstimate),
            "request_estimate" => Some(Self::RequestEstimate),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    pub fn is_simulated(self) -> bool {
        matches!(self, Self::LocalPromptCache)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLatencyTrace {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inference_attempts:
        Option<crate::anthropic::inference_attempt_budget::InferenceAttemptSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auxiliary_attempts:
        Option<crate::anthropic::inference_attempt_budget::AuxiliaryAttemptSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity_weight_units: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_input_tokens: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_guard_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_header_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_upstream_chunk_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_output_delta_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_thinking_delta_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_visible_text_delta_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_gap_to_first_output_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunks_before_first_output: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events_before_first_output: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_bytes_before_first_output: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_frames_before_first_output: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_events_before_first_output: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_frames_without_downstream_events_before_first_output: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_pending_chunks_before_first_output: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_frame_decode_errors_before_first_output: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_event_parse_errors_before_first_output: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_event_types_before_first_output: Option<HashMap<String, u32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_retry_attempts: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_retry_dispatch_failures: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_retry_reasons: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_dropped_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_reason: Option<StreamTerminalReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_message_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saw_upstream_completed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspected_intent_preamble_end_turn: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_preamble_risk: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspected_tool_context_leak_end_turn: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_context_leak_markers: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suppressed_tool_context_leak_blocks: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suppressed_tool_context_leak_chars: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suppressed_tool_context_leak_kinds: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant_tail_intent_hint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_turn_anomaly_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_turn_anomaly_risk: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_eof_without_completed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_upstream_event_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_upstream_events: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saw_upstream_assistant_response: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saw_upstream_tool_use: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saw_upstream_metadata: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_assistant_content_chars: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filtered_trivial_text_blocks: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filtered_trivial_text_chars: Option<u32>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum StreamTerminalReason {
    Completed,
    UpstreamStatusError,
    UpstreamJsonException,
    UpstreamIdleTimeout,
    MalformedSse,
    ProtocolContamination,
    ClientDropped,
    InternalError,
}

impl UsageLatencyTrace {
    pub fn is_empty(&self) -> bool {
        self.inference_attempts.is_none()
            && self.auxiliary_attempts.is_none()
            && self.payload_guard_ms.is_none()
            && self.capacity_weight_units.is_none()
            && self.estimated_input_tokens.is_none()
            && self.upstream_header_ms.is_none()
            && self.first_upstream_chunk_ms.is_none()
            && self.first_output_delta_ms.is_none()
            && self.first_thinking_delta_ms.is_none()
            && self.first_visible_text_delta_ms.is_none()
            && self.stream_gap_to_first_output_ms.is_none()
            && self.chunks_before_first_output.is_none()
            && self.events_before_first_output.is_none()
            && self.upstream_bytes_before_first_output.is_none()
            && self.upstream_frames_before_first_output.is_none()
            && self.upstream_events_before_first_output.is_none()
            && self
                .upstream_frames_without_downstream_events_before_first_output
                .is_none()
            && self.upstream_pending_chunks_before_first_output.is_none()
            && self
                .upstream_frame_decode_errors_before_first_output
                .is_none()
            && self
                .upstream_event_parse_errors_before_first_output
                .is_none()
            && self.upstream_event_types_before_first_output.is_none()
            && self.stream_retry_attempts.is_none()
            && self.stream_retry_dispatch_failures.is_none()
            && self.stream_retry_reasons.is_none()
            && self.client_dropped_ms.is_none()
            && self.terminal_reason.is_none()
            && self.upstream_message_status.is_none()
            && self.saw_upstream_completed.is_none()
            && self.stop_reason_source.is_none()
            && self.suspected_intent_preamble_end_turn.is_none()
            && self.intent_preamble_risk.is_none()
            && self.suspected_tool_context_leak_end_turn.is_none()
            && self.tool_context_leak_markers.is_none()
            && self.suppressed_tool_context_leak_blocks.is_none()
            && self.suppressed_tool_context_leak_chars.is_none()
            && self.suppressed_tool_context_leak_kinds.is_none()
            && self.assistant_tail_intent_hint.is_none()
            && self.end_turn_anomaly_reason.is_none()
            && self.end_turn_anomaly_risk.is_none()
            && self.upstream_eof_without_completed.is_none()
            && self.last_upstream_event_type.is_none()
            && self.last_upstream_events.is_none()
            && self.saw_upstream_assistant_response.is_none()
            && self.saw_upstream_tool_use.is_none()
            && self.saw_upstream_metadata.is_none()
            && self.last_assistant_content_chars.is_none()
            && self.filtered_trivial_text_blocks.is_none()
            && self.filtered_trivial_text_chars.is_none()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UsagePublicError {
    pub status_code: u16,
    pub error_type: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecord {
    pub id: String,
    pub created_at: String,
    pub endpoint: String,
    pub stream: bool,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_max_tokens: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downstream_stop_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_outbound_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_resolution_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_resolution_note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_api_key_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_label: Option<String>,
    pub status: UsageRecordStatus,
    pub usage_source: UsageSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_usage: Option<ExternalPoolUsageSnapshot>,
    pub total_input_tokens: i32,
    pub compat_input_tokens: i32,
    pub billable_input_tokens: i32,
    pub output_tokens: i32,
    pub cache_read_input_tokens: i32,
    pub cache_creation_input_tokens: i32,
    pub cache_creation_5m_input_tokens: i32,
    pub cache_creation_1h_input_tokens: i32,
    #[serde(default)]
    pub estimated_cost_usd: f64,
    #[serde(default)]
    pub original_cost_usd: f64,
    #[serde(default)]
    pub kiro_metering_usage: f64,
    #[serde(default)]
    pub pricing_available: bool,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pricing_model: Option<String>,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_token_latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_trace: Option<UsageLatencyTrace>,
    pub simulated: bool,
    pub sticky_bound: bool,
    pub fallback_from_sticky: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub credential_attempts: Vec<KiroCredentialAttempt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_kind: Option<UsageRouteKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_subtype: Option<UsageRouteSubtype>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direct_policy_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_attempted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_preflight: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_pool_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_pool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_attempts: Vec<ExternalPoolAttempt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_projection_applied: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_pool_billing: Option<ExternalPoolBilling>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_status_code: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_metadata: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_upstream_error: Option<RawUpstreamError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_error_status_code: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_error_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_error_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_breakdown: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_guard_report: Option<serde_json::Value>,
}

/// Builds a bounded diagnostic record for a sampled gateway rejection.
///
/// `observed_count` is the monotonic count observed when the sample was selected. It is not the
/// exact number of rejected requests represented by this record.
pub(crate) fn sampled_request_rejection_usage_record(
    request_id: &str,
    endpoint: &str,
    request_api_key_id: Option<String>,
    reason: &'static str,
    stage: &'static str,
    status: http::StatusCode,
    observed_count: u64,
) -> UsageRecord {
    UsageRecord {
        id: request_id.to_string(),
        created_at: Utc::now().to_rfc3339(),
        endpoint: endpoint.to_string(),
        stream: false,
        model: "unknown".to_string(),
        requested_max_tokens: None,
        downstream_stop_reason: None,
        upstream_model: None,
        external_outbound_model: None,
        model_resolution_source: None,
        model_resolution_note: None,
        conversation_id: None,
        request_api_key_id,
        credential_id: None,
        credential_label: None,
        status: UsageRecordStatus::Error,
        usage_source: UsageSource::None,
        raw_usage: None,
        total_input_tokens: 0,
        compat_input_tokens: 0,
        billable_input_tokens: 0,
        output_tokens: 0,
        cache_read_input_tokens: 0,
        cache_creation_input_tokens: 0,
        cache_creation_5m_input_tokens: 0,
        cache_creation_1h_input_tokens: 0,
        estimated_cost_usd: 0.0,
        original_cost_usd: 0.0,
        kiro_metering_usage: 0.0,
        pricing_available: false,
        pricing_model: None,
        duration_ms: 0,
        first_token_latency_ms: None,
        response_latency_ms: None,
        latency_trace: None,
        simulated: false,
        sticky_bound: false,
        fallback_from_sticky: false,
        credential_attempts: Vec::new(),
        route_kind: None,
        route_subtype: None,
        fallback_reason: None,
        direct_policy_reason: None,
        local_attempted: None,
        local_preflight: None,
        external_pool_id: None,
        external_pool_name: None,
        external_attempts: Vec::new(),
        usage_projection_applied: None,
        external_pool_billing: None,
        error_type: Some(REQUEST_REJECTION_ERROR_TYPE.to_string()),
        error_message: Some(REQUEST_REJECTION_ERROR_MESSAGE.to_string()),
        error_detail: None,
        error_status_code: Some(status.as_u16()),
        error_source: Some(REQUEST_REJECTION_ERROR_TYPE.to_string()),
        error_id: Some(request_id.to_string()),
        error_metadata: Some(serde_json::json!({
            "sampled": true,
            "observedCount": observed_count,
            "observedCountIsExact": false,
            "stage": stage,
            "reason": reason,
        })),
        raw_upstream_error: None,
        public_error_status_code: None,
        public_error_type: None,
        public_error_message: None,
        payload_breakdown: None,
        payload_guard_report: None,
    }
}

#[derive(Debug, Clone)]
pub struct UsageRecordQuery {
    pub limit: usize,
    pub request_id: Option<String>,
    pub q: Option<String>,
    pub endpoint: Option<String>,
    pub conversation_id: Option<String>,
    pub request_api_key_id: Option<String>,
    pub credential_id: Option<u64>,
    pub external_pool_id: Option<u64>,
    pub route_kind: Option<UsageRouteKind>,
    pub model: Option<String>,
    pub status: Option<UsageRecordStatus>,
    pub source: Option<UsageSource>,
    pub stream: Option<bool>,
    pub min_cache_read: Option<i32>,
    pub min_first_token_latency_ms: Option<u64>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
}

impl Default for UsageRecordQuery {
    fn default() -> Self {
        Self {
            limit: DEFAULT_QUERY_LIMIT,
            request_id: None,
            q: None,
            endpoint: None,
            conversation_id: None,
            request_api_key_id: None,
            credential_id: None,
            external_pool_id: None,
            route_kind: None,
            model: None,
            status: None,
            source: None,
            stream: None,
            min_cache_read: None,
            min_first_token_latency_ms: None,
            since: None,
            until: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecordsResult {
    pub total: usize,
    pub records: Vec<UsageRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecordsPageResult {
    pub page: usize,
    pub limit: usize,
    pub has_next: bool,
    pub records: Vec<UsageRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageAggregate {
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub requests: usize,
    pub cache_read_input_tokens: i64,
    pub cache_creation_input_tokens: i64,
    pub estimated_cost_usd: f64,
    #[serde(default)]
    pub original_cost_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRealtimeStats {
    pub window_seconds: u32,
    pub requests: usize,
    #[serde(default)]
    pub success_requests: usize,
    #[serde(default)]
    pub error_requests: usize,
    pub rpm: f64,
    #[serde(default)]
    pub success_rpm: f64,
    #[serde(default)]
    pub error_rpm: f64,
    pub input_tpm: f64,
    pub output_tpm: f64,
    pub total_tpm: f64,
    pub billable_tpm: f64,
}

impl UsageRealtimeStats {
    pub fn empty(window_seconds: u32) -> Self {
        Self {
            window_seconds,
            requests: 0,
            success_requests: 0,
            error_requests: 0,
            rpm: 0.0,
            success_rpm: 0.0,
            error_rpm: 0.0,
            input_tpm: 0.0,
            output_tpm: 0.0,
            total_tpm: 0.0,
            billable_tpm: 0.0,
        }
    }

    pub fn from_totals_with_status(
        window_seconds: u32,
        requests: usize,
        success_requests: usize,
        error_requests: usize,
        input_tokens: i64,
        output_tokens: i64,
        billable_input_tokens: i64,
    ) -> Self {
        let scale = if window_seconds == 0 {
            0.0
        } else {
            60.0 / window_seconds as f64
        };
        let input_tpm = input_tokens.max(0) as f64 * scale;
        let output_tpm = output_tokens.max(0) as f64 * scale;
        Self {
            window_seconds,
            requests,
            success_requests,
            error_requests,
            rpm: requests as f64 * scale,
            success_rpm: success_requests as f64 * scale,
            error_rpm: error_requests as f64 * scale,
            input_tpm,
            output_tpm,
            total_tpm: input_tpm + output_tpm,
            billable_tpm: (billable_input_tokens.max(0) + output_tokens.max(0)) as f64 * scale,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSummary {
    pub total_requests: usize,
    pub success_requests: usize,
    pub error_requests: usize,
    pub high_cache_requests: usize,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_cache_read_input_tokens: i64,
    pub total_cache_creation_input_tokens: i64,
    pub total_estimated_cost_usd: f64,
    #[serde(default)]
    pub total_original_cost_usd: f64,
    #[serde(default)]
    pub total_kiro_metering_usage: f64,
    pub priced_requests: usize,
    pub unpriced_requests: usize,
    pub local_prompt_cache_requests: usize,
    pub local_prompt_cache_input_tokens: i64,
    pub local_prompt_cache_read_input_tokens: i64,
    pub local_prompt_cache_creation_input_tokens: i64,
    pub simulated_requests: usize,
    pub upstream_metadata_requests: usize,
    pub external_pool_billing: UsageExternalPoolBillingSummary,
    pub realtime: UsageRealtimeStats,
    pub top_credentials: Vec<UsageAggregate>,
    pub top_conversations: Vec<UsageAggregate>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageExternalPoolBillingSummary {
    pub requests: usize,
    pub priced_requests: usize,
    pub unpriced_requests: usize,
    pub cost_floor_applied_requests: usize,
    pub raw_cost_usd: f64,
    pub shaped_cost_usd: f64,
    pub uplifted_cost_usd: f64,
    pub profit_usd: f64,
    pub reported_cost_usd: f64,
    pub billable_cost_usd: f64,
    pub cost_floor_delta_usd: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageExternalPoolBillingByPool {
    pub pool_id: u64,
    pub pool_name: String,
    pub requests: usize,
    pub priced_requests: usize,
    pub unpriced_requests: usize,
    pub cost_floor_applied_requests: usize,
    pub raw_cost_usd: f64,
    pub shaped_cost_usd: f64,
    pub uplifted_cost_usd: f64,
    pub profit_usd: f64,
    pub reported_cost_usd: f64,
    pub billable_cost_usd: f64,
    pub cost_floor_delta_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDashboardResponse {
    pub generated_at: String,
    pub timezone: String,
    pub windows: Vec<UsageDashboardWindow>,
    pub series: UsageDashboardSeries,
    pub top: UsageDashboardTop,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDashboardWindowsResponse {
    pub generated_at: String,
    pub timezone: String,
    pub windows: Vec<UsageDashboardWindow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDashboardSeriesResponse {
    pub generated_at: String,
    pub timezone: String,
    pub series: UsageDashboardSeries,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDashboardTopResponse {
    pub generated_at: String,
    pub top: UsageDashboardTop,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDashboardBreakdownResponse {
    pub generated_at: String,
    pub timezone: String,
    pub window_key: String,
    pub status_breakdown: Vec<UsageBreakdownItem>,
    pub usage_source_breakdown: Vec<UsageBreakdownItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDashboardExternalPoolBillingResponse {
    pub generated_at: String,
    pub timezone: String,
    pub window_key: String,
    pub external_pool_billing_by_pool: Vec<UsageExternalPoolBillingByPool>,
}

#[derive(Debug, Clone)]
pub struct UsageExternalPoolRiskQuery {
    pub timezone: String,
    pub window_key: String,
    pub window_label: String,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub warning_threshold_tokens: i64,
    pub critical_threshold_tokens: i64,
    pub pool_id: Option<u64>,
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub stream: Option<bool>,
    pub limit: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct UsageExternalPoolRiskCostConfig {
    pub cost_floor_enabled: bool,
    pub cost_floor_margin_percent: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageExternalPoolRiskResponse {
    pub generated_at: String,
    pub timezone: String,
    pub window: UsageExternalPoolRiskWindow,
    pub thresholds: UsageExternalPoolRiskThresholds,
    pub filters: UsageExternalPoolRiskFilters,
    pub totals: UsageExternalPoolRiskTotals,
    pub raw_cache: UsageExternalPoolRiskCacheStats,
    pub reported_cache: UsageExternalPoolRiskCacheStats,
    pub cost: UsageExternalPoolRiskCostStats,
    pub buckets: Vec<UsageExternalPoolRiskBucket>,
    pub by_pool: Vec<UsageExternalPoolRiskGroup>,
    pub by_path: Vec<UsageExternalPoolRiskGroup>,
    pub by_model: Vec<UsageExternalPoolRiskGroup>,
    pub samples: Vec<UsageExternalPoolRiskSample>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageExternalPoolRiskWindow {
    pub key: String,
    pub label: String,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageExternalPoolRiskThresholds {
    pub warning_tokens: i64,
    pub critical_tokens: i64,
    pub cost_floor_enabled: bool,
    pub cost_floor_margin_percent: u32,
    pub cost_target_multiplier: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageExternalPoolRiskFilters {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageExternalPoolRiskTotals {
    pub records: usize,
    pub success_records: usize,
    pub error_records: usize,
    pub stream_records: usize,
    pub non_stream_records: usize,
    pub priced_records: usize,
    pub unpriced_records: usize,
    pub raw_usage_records: usize,
    pub reported_usage_records: usize,
    pub missing_external_pool_billing_records: usize,
    pub output_zero_records: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageExternalPoolRiskCacheStats {
    pub min_read_tokens: i64,
    pub max_read_tokens: i64,
    pub avg_read_tokens: f64,
    pub total_read_tokens: i64,
    pub min_write_tokens: i64,
    pub max_write_tokens: i64,
    pub avg_write_tokens: f64,
    pub total_write_tokens: i64,
    pub read_warning_count: usize,
    pub write_warning_count: usize,
    pub either_warning_count: usize,
    pub read_critical_count: usize,
    pub write_critical_count: usize,
    pub either_critical_count: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageExternalPoolRiskCostStats {
    pub raw_cost_usd: f64,
    pub reported_cost_usd: f64,
    pub target_cost_usd: f64,
    pub profit_usd: f64,
    pub total_loss_usd: f64,
    pub total_target_gap_usd: f64,
    pub max_loss_usd: f64,
    pub max_target_gap_usd: f64,
    pub max_raw_cost_usd: f64,
    pub max_reported_cost_usd: f64,
    pub below_raw_count: usize,
    pub below_target_count: usize,
    pub cost_floor_applied_records: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_cost_ratio: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avg_cost_ratio: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_ratio: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageExternalPoolRiskBucket {
    pub key: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<i64>,
    pub raw_read_count: usize,
    pub raw_write_count: usize,
    pub reported_read_count: usize,
    pub reported_write_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageExternalPoolRiskGroup {
    pub key: String,
    pub label: String,
    pub records: usize,
    pub success_records: usize,
    pub warning_records: usize,
    pub critical_records: usize,
    pub output_zero_records: usize,
    pub raw_read_max: i64,
    pub raw_write_max: i64,
    pub reported_read_max: i64,
    pub reported_write_max: i64,
    pub raw_cost_usd: f64,
    pub reported_cost_usd: f64,
    pub target_cost_usd: f64,
    pub profit_usd: f64,
    pub total_loss_usd: f64,
    pub total_target_gap_usd: f64,
    pub below_raw_count: usize,
    pub below_target_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageExternalPoolRiskSample {
    pub id: String,
    pub created_at: String,
    pub endpoint: String,
    pub stream: bool,
    pub model: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_pool_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_pool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_projection_mode: Option<String>,
    pub external_pool_billing_present: bool,
    pub cost_floor_applied: bool,
    pub raw_input_tokens: i64,
    pub raw_output_tokens: i64,
    pub raw_cache_read_input_tokens: i64,
    pub raw_cache_creation_input_tokens: i64,
    pub reported_input_tokens: i64,
    pub reported_output_tokens: i64,
    pub reported_cache_read_input_tokens: i64,
    pub reported_cache_creation_input_tokens: i64,
    pub raw_cost_usd: f64,
    pub reported_cost_usd: f64,
    pub target_cost_usd: f64,
    pub loss_usd: f64,
    pub target_gap_usd: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_ratio: Option<f64>,
    pub risk_reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDashboardWindow {
    pub key: String,
    pub label: String,
    pub from: String,
    pub to: String,
    pub summary: UsageDashboardSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDashboardSummary {
    pub total_requests: usize,
    pub success_requests: usize,
    pub error_requests: usize,
    pub error_rate: f64,
    pub stream_requests: usize,
    pub non_stream_requests: usize,
    pub high_cache_requests: usize,
    pub total_input_tokens: i64,
    pub billable_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_cache_read_input_tokens: i64,
    pub total_cache_creation_input_tokens: i64,
    pub cache_read_ratio: f64,
    pub total_estimated_cost_usd: f64,
    #[serde(default)]
    pub total_original_cost_usd: f64,
    #[serde(default)]
    pub total_kiro_metering_usage: f64,
    pub priced_requests: usize,
    pub unpriced_requests: usize,
    pub average_duration_ms: f64,
    pub p95_duration_ms: u64,
    pub sticky_bound_requests: usize,
    pub fallback_from_sticky_requests: usize,
    pub simulated_requests: usize,
    pub upstream_metadata_requests: usize,
    pub external_pool_billing: UsageExternalPoolBillingSummary,
    #[serde(default)]
    pub external_pool_billing_by_pool: Vec<UsageExternalPoolBillingByPool>,
    pub status_breakdown: Vec<UsageBreakdownItem>,
    pub usage_source_breakdown: Vec<UsageBreakdownItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageBreakdownItem {
    pub key: String,
    pub label: String,
    pub requests: usize,
    pub ratio: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDashboardSeries {
    pub hourly_24h: Vec<UsageSeriesPoint>,
    pub daily_7d: Vec<UsageSeriesPoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSeriesPoint {
    pub key: String,
    pub label: String,
    pub from: String,
    pub to: String,
    pub requests: usize,
    pub success_requests: usize,
    pub error_requests: usize,
    pub total_input_tokens: i64,
    pub billable_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_estimated_cost_usd: f64,
    #[serde(default)]
    pub total_original_cost_usd: f64,
    #[serde(default)]
    pub total_kiro_metering_usage: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDashboardTop {
    pub window_key: String,
    pub models: Vec<UsageTopAggregate>,
    pub credentials: Vec<UsageTopAggregate>,
    pub endpoints: Vec<UsageTopAggregate>,
    pub errors: Vec<UsageTopAggregate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageTopAggregate {
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub requests: usize,
    pub error_requests: usize,
    pub total_input_tokens: i64,
    pub billable_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_cache_read_input_tokens: i64,
    pub total_cache_creation_input_tokens: i64,
    pub total_estimated_cost_usd: f64,
    #[serde(default)]
    pub total_original_cost_usd: f64,
    #[serde(default)]
    pub total_kiro_metering_usage: f64,
}

#[derive(Debug, Clone)]
pub struct UsageDashboardWindowSpec {
    pub key: String,
    pub label: String,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

pub fn usage_dashboard_timezone(value: Option<&str>) -> (String, FixedOffset) {
    let raw = value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_USAGE_DASHBOARD_TIMEZONE);

    match raw {
        DEFAULT_USAGE_DASHBOARD_TIMEZONE | "Asia/Beijing" | "Asia/Chongqing" | "CST" => (
            DEFAULT_USAGE_DASHBOARD_TIMEZONE.to_string(),
            east_offset(8 * 3600),
        ),
        "UTC" | "Etc/UTC" | "Z" => ("UTC".to_string(), east_offset(0)),
        _ => parse_fixed_offset(raw)
            .map(|offset| (raw.to_string(), offset))
            .unwrap_or_else(|| {
                tracing::warn!(
                    timezone = raw,
                    fallback = DEFAULT_USAGE_DASHBOARD_TIMEZONE,
                    "未知 usage dashboard 时区，回退到默认时区"
                );
                (
                    DEFAULT_USAGE_DASHBOARD_TIMEZONE.to_string(),
                    east_offset(8 * 3600),
                )
            }),
    }
}

pub fn usage_dashboard_windows(
    now: DateTime<Utc>,
    offset: FixedOffset,
) -> Vec<UsageDashboardWindowSpec> {
    let local_now = now.with_timezone(&offset);
    let today_start = local_midnight_utc(offset, local_now.date_naive());
    let yesterday_start = today_start - ChronoDuration::days(1);
    let month_start = local_month_start_utc(offset, local_now.date_naive());

    vec![
        dashboard_window("today", "今天", today_start, now),
        dashboard_window(
            "last24h",
            "最近24小时",
            now - ChronoDuration::hours(24),
            now,
        ),
        dashboard_window("yesterday", "昨天", yesterday_start, today_start),
        dashboard_window("last7d", "最近7天", now - ChronoDuration::days(7), now),
        dashboard_window("last30d", "最近30天", now - ChronoDuration::days(30), now),
        dashboard_window("thisMonth", "本月", month_start, now),
    ]
}

pub fn usage_dashboard_window_spec_for_key(
    now: DateTime<Utc>,
    offset: FixedOffset,
    key: &str,
) -> UsageDashboardWindowSpec {
    let requested_key = key.trim();
    let windows = usage_dashboard_windows(now, offset);
    windows
        .iter()
        .find(|window| window.key == requested_key)
        .or_else(|| windows.iter().find(|window| window.key == "today"))
        .or_else(|| windows.first())
        .cloned()
        .expect("usage dashboard always has at least one window")
}

pub fn usage_dashboard_hourly_windows(
    now: DateTime<Utc>,
    offset: FixedOffset,
) -> Vec<UsageDashboardWindowSpec> {
    let local_now = now.with_timezone(&offset);
    let current_hour = offset
        .with_ymd_and_hms(
            local_now.year(),
            local_now.month(),
            local_now.day(),
            local_now.hour(),
            0,
            0,
        )
        .single()
        .unwrap_or(local_now)
        .with_timezone(&Utc);
    let first_hour = current_hour - ChronoDuration::hours(23);

    (0..24)
        .map(|idx| {
            let from = first_hour + ChronoDuration::hours(idx);
            let natural_to = from + ChronoDuration::hours(1);
            let to = natural_to.min(now);
            let local_from = from.with_timezone(&offset);
            dashboard_window(
                format!("h{}", idx + 1),
                local_from.format("%m-%d %H:00").to_string(),
                from,
                to,
            )
        })
        .collect()
}

pub fn usage_dashboard_daily_windows(
    now: DateTime<Utc>,
    offset: FixedOffset,
) -> Vec<UsageDashboardWindowSpec> {
    let local_now = now.with_timezone(&offset);
    let today_start = local_midnight_utc(offset, local_now.date_naive());
    let first_day = today_start - ChronoDuration::days(6);

    (0..7)
        .map(|idx| {
            let from = first_day + ChronoDuration::days(idx);
            let natural_to = from + ChronoDuration::days(1);
            let to = natural_to.min(now);
            let local_from = from.with_timezone(&offset);
            dashboard_window(
                format!("d{}", idx + 1),
                local_from.format("%m-%d").to_string(),
                from,
                to,
            )
        })
        .collect()
}

fn dashboard_window(
    key: impl Into<String>,
    label: impl Into<String>,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> UsageDashboardWindowSpec {
    UsageDashboardWindowSpec {
        key: key.into(),
        label: label.into(),
        from,
        to,
    }
}

fn east_offset(seconds: i32) -> FixedOffset {
    FixedOffset::east_opt(seconds).expect("valid fixed offset")
}

fn local_midnight_utc(offset: FixedOffset, date: NaiveDate) -> DateTime<Utc> {
    offset
        .with_ymd_and_hms(date.year(), date.month(), date.day(), 0, 0, 0)
        .single()
        .expect("fixed offset local midnight")
        .with_timezone(&Utc)
}

fn local_month_start_utc(offset: FixedOffset, date: NaiveDate) -> DateTime<Utc> {
    let first_day =
        NaiveDate::from_ymd_opt(date.year(), date.month(), 1).expect("valid month start date");
    local_midnight_utc(offset, first_day)
}

fn parse_fixed_offset(value: &str) -> Option<FixedOffset> {
    let value = value
        .strip_prefix("UTC")
        .or_else(|| value.strip_prefix("GMT"))
        .unwrap_or(value);
    if value.is_empty() {
        return Some(east_offset(0));
    }

    let (sign, rest) = match value.as_bytes().first().copied() {
        Some(b'+') => (1, &value[1..]),
        Some(b'-') => (-1, &value[1..]),
        _ => return None,
    };
    let mut parts = rest.split(':');
    let hours: i32 = parts.next()?.parse().ok()?;
    let minutes: i32 = parts.next().unwrap_or("0").parse().ok()?;
    if parts.next().is_some() || hours > 23 || minutes > 59 {
        return None;
    }
    FixedOffset::east_opt(sign * (hours * 3600 + minutes * 60))
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecorderStats {
    pub accepting: bool,
    pub in_memory_limit: usize,
    pub in_memory_records: usize,
    pub redis_enabled: bool,
    pub redis_queue_enabled: bool,
    pub redis_queue_capacity: usize,
    pub redis_queue_available: usize,
    pub redis_writer_accepted: u64,
    pub redis_writer_finished: u64,
    pub backpressured_redis_records: u64,
    pub dropped_redis_records: u64,
    pub postgres_enabled: bool,
    pub writer_queue_enabled: bool,
    pub writer_queue_capacity: usize,
    pub writer_queue_available: usize,
    pub writer_accepted: u64,
    pub writer_finished: u64,
    pub backpressured_persist_records: u64,
    pub dropped_persist_records: u64,
    pub rejected_after_shutdown: u64,
    pub rejected_by_cleanup_watermark: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageRecordOutcome {
    Accepted,
    RejectedShuttingDown,
    RejectedCleanupWatermark,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageWriterDrainStatus {
    pub target: u64,
    pub finished: u64,
    pub drained: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageRecorderDrainReport {
    pub postgres: UsageWriterDrainStatus,
    pub redis: UsageWriterDrainStatus,
    pub timed_out: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct UsageRecorderShutdownReport {
    pub already_started: bool,
    pub drained: bool,
    pub timed_out: bool,
    pub postgres_abandoned: u64,
    pub redis_abandoned: u64,
    pub stats: UsageRecorderStats,
}

#[derive(Default)]
struct UsageWriterProgress {
    accepted: AtomicU64,
    finished: AtomicU64,
    changed: Notify,
}

impl UsageWriterProgress {
    fn accepted(&self) -> u64 {
        self.accepted.load(Ordering::Acquire)
    }

    fn finished(&self) -> u64 {
        self.finished.load(Ordering::Acquire)
    }

    fn mark_accepted(&self) {
        self.accepted.fetch_add(1, Ordering::Release);
    }

    fn mark_finished(&self, count: usize) {
        self.finished.fetch_add(count as u64, Ordering::Release);
        self.changed.notify_waiters();
    }

    async fn wait_until(&self, target: u64) -> u64 {
        loop {
            let changed = self.changed.notified();
            let finished = self.finished();
            if finished >= target {
                return finished;
            }
            changed.await;
        }
    }
}

struct UsageWriterControl {
    sender: Mutex<Option<mpsc::Sender<UsageRecord>>>,
    task: Mutex<Option<JoinHandle<()>>>,
    progress: Arc<UsageWriterProgress>,
}

enum UsageWriterEnqueueError {
    Full(UsageRecord),
    Closed(UsageRecord),
}

impl UsageWriterControl {
    fn new(
        sender: mpsc::Sender<UsageRecord>,
        task: JoinHandle<()>,
        progress: Arc<UsageWriterProgress>,
    ) -> Self {
        Self {
            sender: Mutex::new(Some(sender)),
            task: Mutex::new(Some(task)),
            progress,
        }
    }

    fn enqueue(&self, record: UsageRecord) -> Result<bool, UsageWriterEnqueueError> {
        let Some(sender) = self.sender.lock().as_ref().cloned() else {
            return Err(UsageWriterEnqueueError::Closed(record));
        };
        match sender.try_send(record) {
            Ok(()) => {
                self.progress.mark_accepted();
                Ok(false)
            }
            Err(mpsc::error::TrySendError::Closed(record)) => {
                Err(UsageWriterEnqueueError::Closed(record))
            }
            Err(mpsc::error::TrySendError::Full(record)) => {
                Err(UsageWriterEnqueueError::Full(record))
            }
        }
    }

    fn close(&self) {
        self.sender.lock().take();
    }

    fn take_task(&self) -> Option<JoinHandle<()>> {
        self.task.lock().take()
    }

    fn queue_stats(&self) -> (bool, usize, usize) {
        self.sender
            .lock()
            .as_ref()
            .map(|sender| (true, sender.max_capacity(), sender.capacity()))
            .unwrap_or((false, 0, 0))
    }
}

#[derive(Default)]
struct UsageShutdownState {
    started: AtomicBool,
    complete: AtomicBool,
    timed_out: AtomicBool,
    changed: Notify,
}

#[derive(Debug, Clone)]
struct CachedUsageDashboard {
    timezone: String,
    high_cache_threshold: i32,
    revision: UsagePostgresCacheRevision,
    stored_at: Instant,
    response: UsageDashboardResponse,
}

#[derive(Debug, Clone)]
struct CachedUsageSummary {
    high_cache_threshold: i32,
    revision: UsagePostgresCacheRevision,
    stored_at: Instant,
    response: UsageSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UsagePostgresCacheRevision {
    writer_accepted: u64,
    writer_finished: u64,
    cleanup_watermark_micros: i64,
}

pub struct UsageRecorder {
    records: Mutex<VecDeque<UsageRecord>>,
    limit: usize,
    postgres_store: Option<Arc<PostgresUsageStore>>,
    redis_store: Option<Arc<RedisStore>>,
    writer: Option<Arc<UsageWriterControl>>,
    redis_writer: Option<Arc<UsageWriterControl>>,
    dashboard_query_gate: Arc<Semaphore>,
    postgres_summary_cache: Mutex<Option<CachedUsageSummary>>,
    postgres_dashboard_cache: Mutex<Option<CachedUsageDashboard>>,
    lifecycle: Arc<RwLock<()>>,
    accepting: Arc<AtomicBool>,
    shutdown: Arc<UsageShutdownState>,
    rejected_after_shutdown: AtomicU64,
    cleanup_watermark_micros: AtomicI64,
    rejected_by_cleanup_watermark: AtomicU64,
    backpressured_persist_records: AtomicU64,
    backpressured_redis_records: AtomicU64,
    dropped_persist_records: Arc<AtomicU64>,
    dropped_redis_records: Arc<AtomicU64>,
}

fn normalize_error_diagnostics(mut record: UsageRecord) -> UsageRecord {
    if matches!(record.status, UsageRecordStatus::Success)
        && record.error_message.is_none()
        && record.error_detail.is_none()
        && record.error_metadata.is_none()
        && record.raw_upstream_error.is_none()
        && record.public_error_message.is_none()
    {
        return record;
    }

    let (error_message, message_truncated) =
        truncate_error_text(record.error_message.take(), ERROR_DIAGNOSTIC_MAX_TEXT_BYTES);
    let (error_detail, detail_truncated) =
        truncate_error_text(record.error_detail.take(), ERROR_DIAGNOSTIC_MAX_TEXT_BYTES);
    let (public_error_message, public_message_truncated) = truncate_error_text(
        record.public_error_message.take(),
        ERROR_DIAGNOSTIC_MAX_TEXT_BYTES,
    );
    record.error_message = error_message;
    record.error_detail = error_detail;
    record.public_error_message = public_error_message;
    record.error_metadata = sanitize_error_metadata(
        record.error_metadata.take(),
        message_truncated,
        detail_truncated,
        public_message_truncated,
        ERROR_DIAGNOSTIC_MAX_METADATA_BYTES,
    );
    record.raw_upstream_error = record
        .raw_upstream_error
        .take()
        .map(RawUpstreamError::normalize);
    for attempt in &mut record.credential_attempts {
        attempt.raw_upstream_error = attempt
            .raw_upstream_error
            .take()
            .map(RawUpstreamError::normalize);
    }
    for attempt in &mut record.external_attempts {
        attempt.raw_upstream_error = attempt
            .raw_upstream_error
            .take()
            .map(RawUpstreamError::normalize);
    }
    record
}

fn truncate_error_text(value: Option<String>, max_bytes: usize) -> (Option<String>, bool) {
    let Some(value) = value else {
        return (None, false);
    };
    if value.len() <= max_bytes {
        return (Some(value), false);
    }

    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    let mut truncated = value[..end].to_string();
    truncated.push_str("...");
    (Some(truncated), true)
}

fn sanitize_error_metadata(
    value: Option<Value>,
    message_truncated: bool,
    detail_truncated: bool,
    public_message_truncated: bool,
    max_bytes: usize,
) -> Option<Value> {
    if value.is_none() && !message_truncated && !detail_truncated && !public_message_truncated {
        return None;
    }
    let mut value = value.unwrap_or_else(|| Value::Object(Map::new()));
    remove_duplicate_error_fields(&mut value);
    let original_bytes = serialized_len(&value);
    let mut metadata_truncated = false;
    if original_bytes > max_bytes {
        metadata_truncated = true;
        shrink_metadata_value(&mut value);
    }

    let mut final_bytes = serialized_len(&value);
    if final_bytes > max_bytes {
        value = Value::Object(Map::from_iter([
            ("metadataTruncated".to_string(), Value::Bool(true)),
            (
                "originalMetadataBytes".to_string(),
                Value::from(original_bytes as u64),
            ),
        ]));
        final_bytes = serialized_len(&value);
    }

    if message_truncated || detail_truncated || public_message_truncated || metadata_truncated {
        ensure_metadata_object_flags(
            &mut value,
            message_truncated,
            detail_truncated,
            public_message_truncated,
            metadata_truncated,
            original_bytes,
            final_bytes,
        );
    }

    Some(value)
}

fn serialized_len(value: &Value) -> usize {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX)
}

fn remove_duplicate_error_fields(value: &mut Value) {
    match value {
        Value::Object(map) => {
            let duplicate_keys = map
                .keys()
                .filter(|key| is_duplicate_error_metadata_key(key))
                .cloned()
                .collect::<Vec<_>>();
            for key in duplicate_keys {
                map.remove(&key);
            }
            for value in map.values_mut() {
                remove_duplicate_error_fields(value);
            }
        }
        Value::Array(items) => {
            for value in items {
                remove_duplicate_error_fields(value);
            }
        }
        _ => {}
    }
}

fn is_duplicate_error_metadata_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect::<String>();
    matches!(
        normalized.as_str(),
        "errorid"
            | "requestid"
            | "statuscode"
            | "errorstatuscode"
            | "source"
            | "errorsource"
            | "message"
            | "rawmessage"
            | "publicerrortype"
            | "publicstatuscode"
            | "internalsource"
            | "upstreamstatuscode"
    )
}

fn shrink_metadata_value(value: &mut Value) {
    match value {
        Value::String(text) => {
            let (truncated, _) = truncate_error_text(
                Some(std::mem::take(text)),
                ERROR_DIAGNOSTIC_MAX_STRING_BYTES,
            );
            *text = truncated.unwrap_or_default();
        }
        Value::Array(items) => {
            if items.len() > ERROR_DIAGNOSTIC_MAX_ARRAY_ITEMS {
                items.truncate(ERROR_DIAGNOSTIC_MAX_ARRAY_ITEMS);
            }
            for item in items {
                shrink_metadata_value(item);
            }
        }
        Value::Object(map) => {
            for value in map.values_mut() {
                shrink_metadata_value(value);
            }
        }
        _ => {}
    }
}

fn ensure_metadata_object_flags(
    value: &mut Value,
    message_truncated: bool,
    detail_truncated: bool,
    public_message_truncated: bool,
    metadata_truncated: bool,
    original_bytes: usize,
    final_bytes: usize,
) {
    if !value.is_object() {
        let old = std::mem::take(value);
        let mut map = Map::new();
        map.insert("value".to_string(), old);
        *value = Value::Object(map);
    }
    let Some(map) = value.as_object_mut() else {
        return;
    };
    if message_truncated {
        map.insert("messageTruncated".to_string(), Value::Bool(true));
    }
    if detail_truncated {
        map.insert("detailTruncated".to_string(), Value::Bool(true));
    }
    if public_message_truncated {
        map.insert("publicMessageTruncated".to_string(), Value::Bool(true));
    }
    if metadata_truncated {
        map.insert("metadataTruncated".to_string(), Value::Bool(true));
        map.insert(
            "originalMetadataBytes".to_string(),
            Value::from(original_bytes as u64),
        );
        map.insert(
            "finalMetadataBytes".to_string(),
            Value::from(final_bytes as u64),
        );
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CredentialCostSummary {
    pub estimated_cost_usd: f64,
    pub original_cost_usd: f64,
    pub kiro_metering_usage: f64,
    pub priced_requests: usize,
    pub unpriced_requests: usize,
}

impl UsageRecorder {
    #[cfg(test)]
    pub fn new(limit: usize) -> Self {
        let limit = limit.max(1);
        Self {
            records: Mutex::new(VecDeque::with_capacity(limit.min(1024))),
            limit,
            postgres_store: None,
            redis_store: None,
            writer: None,
            redis_writer: None,
            dashboard_query_gate: Arc::new(Semaphore::new(USAGE_DASHBOARD_MAX_CONCURRENT_QUERIES)),
            postgres_summary_cache: Mutex::new(None),
            postgres_dashboard_cache: Mutex::new(None),
            lifecycle: Arc::new(RwLock::new(())),
            accepting: Arc::new(AtomicBool::new(true)),
            shutdown: Arc::new(UsageShutdownState::default()),
            rejected_after_shutdown: AtomicU64::new(0),
            cleanup_watermark_micros: AtomicI64::new(0),
            rejected_by_cleanup_watermark: AtomicU64::new(0),
            backpressured_persist_records: AtomicU64::new(0),
            backpressured_redis_records: AtomicU64::new(0),
            dropped_persist_records: Arc::new(AtomicU64::new(0)),
            dropped_redis_records: Arc::new(AtomicU64::new(0)),
        }
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub fn records_snapshot(&self) -> Vec<UsageRecord> {
        self.records.lock().iter().cloned().collect()
    }

    #[cfg(test)]
    pub(crate) fn with_postgres_and_redis(
        limit: usize,
        postgres_store: Arc<PostgresUsageStore>,
        redis_store: Option<Arc<RedisStore>>,
    ) -> Self {
        Self::with_postgres_internal(limit, postgres_store, redis_store)
    }

    fn with_postgres_internal(
        limit: usize,
        postgres_store: Arc<PostgresUsageStore>,
        redis_store: Option<Arc<RedisStore>>,
    ) -> Self {
        let initial_cleanup_watermark = block_on_usage_store({
            let postgres_store = postgres_store.clone();
            async move { postgres_store.soft_delete_cleanup_watermark().await }
        })
        .unwrap_or_else(|err| {
            tracing::warn!(error = %err, "读取 usage cleanup watermark 失败，启动时暂按未清理处理");
            None
        });
        if let (Some(redis), Some(cutoff)) =
            (redis_store.as_ref(), initial_cleanup_watermark.as_ref())
        {
            redis.note_usage_cleanup_watermark(*cutoff);
        }
        let initial_cleanup_watermark_micros = initial_cleanup_watermark
            .as_ref()
            .map(|cutoff| cutoff.timestamp_micros().max(0))
            .unwrap_or(0);
        let runtime_available = tokio::runtime::Handle::try_current().is_ok();
        let dropped_persist_records = Arc::new(AtomicU64::new(0));
        let dropped_redis_records = Arc::new(AtomicU64::new(0));
        let writer = if runtime_available {
            let (tx, rx) = mpsc::channel(USAGE_WRITER_QUEUE_CAPACITY);
            let progress = Arc::new(UsageWriterProgress::default());
            let task = tokio::spawn(usage_writer_loop(
                postgres_store.clone(),
                rx,
                dropped_persist_records.clone(),
                progress.clone(),
            ));
            Some(Arc::new(UsageWriterControl::new(tx, task, progress)))
        } else {
            tracing::warn!(
                "创建 UsageRecorder 时没有运行中的 Tokio runtime，将同步写入 PgSQL usage"
            );
            None
        };
        let redis_writer = redis_store.as_ref().and_then(|redis| {
            if runtime_available {
                let (tx, rx) = mpsc::channel(USAGE_REDIS_WRITER_QUEUE_CAPACITY);
                let progress = Arc::new(UsageWriterProgress::default());
                let task = tokio::spawn(usage_redis_writer_loop(
                    redis.clone(),
                    rx,
                    dropped_redis_records.clone(),
                    progress.clone(),
                ));
                Some(Arc::new(UsageWriterControl::new(tx, task, progress)))
            } else {
                tracing::warn!(
                    "创建 UsageRecorder 时没有运行中的 Tokio runtime，将同步写入 Redis usage summary"
                );
                None
            }
        });
        Self {
            records: Mutex::new(VecDeque::with_capacity(limit.max(1).min(1024))),
            limit: limit.max(1),
            postgres_store: Some(postgres_store),
            redis_store,
            writer,
            redis_writer,
            dashboard_query_gate: Arc::new(Semaphore::new(USAGE_DASHBOARD_MAX_CONCURRENT_QUERIES)),
            postgres_summary_cache: Mutex::new(None),
            postgres_dashboard_cache: Mutex::new(None),
            lifecycle: Arc::new(RwLock::new(())),
            accepting: Arc::new(AtomicBool::new(true)),
            shutdown: Arc::new(UsageShutdownState::default()),
            rejected_after_shutdown: AtomicU64::new(0),
            cleanup_watermark_micros: AtomicI64::new(initial_cleanup_watermark_micros),
            rejected_by_cleanup_watermark: AtomicU64::new(0),
            backpressured_persist_records: AtomicU64::new(0),
            backpressured_redis_records: AtomicU64::new(0),
            dropped_persist_records,
            dropped_redis_records,
        }
    }

    /// Build the production recorder with PostgreSQL as the only usage data store.
    ///
    /// Scheduler coordination may use Redis independently, but it must not be attached here:
    /// per-request usage materialization can otherwise contend with scheduler operations on the
    /// same single-threaded Redis server.
    #[cfg(test)]
    pub fn with_postgres(limit: usize, postgres_store: Arc<PostgresUsageStore>) -> Self {
        Self::with_postgres_internal(limit, postgres_store, None)
    }

    /// Build the production recorder with an optional, independently configured observability
    /// Redis. Startup validates that this store does not share the business Redis authority.
    /// `None` deliberately means PostgreSQL-only; it must never be replaced by scheduler Redis.
    pub fn with_postgres_and_observability_redis(
        limit: usize,
        postgres_store: Arc<PostgresUsageStore>,
        observability_redis_store: Option<Arc<RedisStore>>,
    ) -> Self {
        assert!(
            observability_redis_store
                .as_ref()
                .is_none_or(|redis| redis.is_observability()),
            "UsageRecorder observability materialization must not use business Redis"
        );
        Self::with_postgres_internal(limit, postgres_store, observability_redis_store)
    }

    pub fn record(&self, record: UsageRecord) -> UsageRecordOutcome {
        let _lifecycle = self.lifecycle.read();
        if !self.accepting.load(Ordering::Acquire) {
            let rejected = self.rejected_after_shutdown.fetch_add(1, Ordering::Relaxed) + 1;
            if should_log_usage_counter(rejected) {
                tracing::warn!(rejected, "UsageRecorder 已停止接收，拒绝新的 usage record");
            }
            return UsageRecordOutcome::RejectedShuttingDown;
        }
        let record = normalize_error_diagnostics(record);
        let record_micros = parse_record_time(&record.created_at)
            .unwrap_or_else(Utc::now)
            .timestamp_micros()
            .max(0);
        let cleanup_watermark = self.cleanup_watermark_micros.load(Ordering::Acquire);
        if record_micros < cleanup_watermark {
            let rejected = self
                .rejected_by_cleanup_watermark
                .fetch_add(1, Ordering::Relaxed)
                + 1;
            if should_log_usage_counter(rejected) {
                tracing::debug!(
                    rejected,
                    request_id = %record.id,
                    record_micros,
                    cleanup_watermark,
                    "usage record 早于清理 watermark，拒绝晚到重放"
                );
            }
            return UsageRecordOutcome::RejectedCleanupWatermark;
        }

        self.record_usage_postgres(record.clone());

        {
            let mut records = self.records.lock();
            if let Some(index) = records.iter().position(|existing| existing.id == record.id) {
                records.remove(index);
            }
            records.push_back(record.clone());
            while records.len() > self.limit {
                records.pop_front();
            }
        }

        self.record_usage_redis(record);
        UsageRecordOutcome::Accepted
    }

    fn record_usage_postgres(&self, record: UsageRecord) {
        if let Some(writer) = &self.writer {
            match writer.enqueue(record) {
                Ok(false) => {}
                Ok(true) => {
                    self.backpressured_persist_records
                        .fetch_add(1, Ordering::Relaxed);
                }
                Err(UsageWriterEnqueueError::Full(_record)) => {
                    let count = self
                        .backpressured_persist_records
                        .fetch_add(1, Ordering::Relaxed)
                        + 1;
                    let dropped = self.dropped_persist_records.fetch_add(1, Ordering::Relaxed) + 1;
                    if should_log_usage_counter(count) {
                        tracing::warn!(
                            count,
                            dropped,
                            "PgSQL usage 队列已满，已丢弃本条持久化记录以避免阻塞主请求"
                        );
                    }
                }
                Err(UsageWriterEnqueueError::Closed(_record)) => {
                    let dropped = self.dropped_persist_records.fetch_add(1, Ordering::Relaxed) + 1;
                    if should_log_usage_counter(dropped) {
                        tracing::warn!(
                            dropped,
                            "PgSQL usage writer 已关闭，已丢弃本条持久化记录以避免阻塞主请求"
                        );
                    }
                }
            }
        } else {
            let dropped = self.dropped_persist_records.fetch_add(1, Ordering::Relaxed) + 1;
            if should_log_usage_counter(dropped) {
                tracing::warn!(
                    dropped,
                    "PgSQL usage writer 不可用，已丢弃本条持久化记录以避免阻塞主请求"
                );
            }
        }
    }

    fn record_usage_redis(&self, record: UsageRecord) {
        if self.redis_store.is_none() {
            return;
        }
        if let Some(writer) = &self.redis_writer {
            match writer.enqueue(record) {
                Ok(false) => {}
                Ok(true) => {
                    self.backpressured_redis_records
                        .fetch_add(1, Ordering::Relaxed);
                }
                Err(UsageWriterEnqueueError::Full(_record)) => {
                    let count = self
                        .backpressured_redis_records
                        .fetch_add(1, Ordering::Relaxed)
                        + 1;
                    let dropped = self.dropped_redis_records.fetch_add(1, Ordering::Relaxed) + 1;
                    if should_log_usage_counter(count) {
                        tracing::warn!(
                            count,
                            dropped,
                            "Redis usage 队列已满，已丢弃本条 summary 记录以避免阻塞主请求"
                        );
                    }
                }
                Err(UsageWriterEnqueueError::Closed(_record)) => {
                    let dropped = self.dropped_redis_records.fetch_add(1, Ordering::Relaxed) + 1;
                    if should_log_usage_counter(dropped) {
                        tracing::warn!(
                            dropped,
                            "Redis usage writer 已关闭，已丢弃本条 summary 记录以避免阻塞主请求"
                        );
                    }
                }
            }
            return;
        }

        let dropped = self.dropped_redis_records.fetch_add(1, Ordering::Relaxed) + 1;
        if should_log_usage_counter(dropped) {
            tracing::warn!(
                dropped,
                "Redis usage writer 不可用，已丢弃本条 summary 记录以避免阻塞主请求"
            );
        }
    }

    pub fn writer_stats(&self) -> UsageRecorderStats {
        let in_memory_records = self.records.lock().len();
        let (writer_queue_enabled, writer_queue_capacity, writer_queue_available) = self
            .writer
            .as_ref()
            .map(|writer| writer.queue_stats())
            .unwrap_or((false, 0, 0));
        let (redis_queue_enabled, redis_queue_capacity, redis_queue_available) = self
            .redis_writer
            .as_ref()
            .map(|writer| writer.queue_stats())
            .unwrap_or((false, 0, 0));
        UsageRecorderStats {
            accepting: self.accepting.load(Ordering::Acquire),
            in_memory_limit: self.limit,
            in_memory_records,
            redis_enabled: self.redis_store.is_some(),
            redis_queue_enabled,
            redis_queue_capacity,
            redis_queue_available,
            redis_writer_accepted: self
                .redis_writer
                .as_ref()
                .map(|writer| writer.progress.accepted())
                .unwrap_or(0),
            redis_writer_finished: self
                .redis_writer
                .as_ref()
                .map(|writer| writer.progress.finished())
                .unwrap_or(0),
            backpressured_redis_records: self.backpressured_redis_records.load(Ordering::Relaxed),
            dropped_redis_records: self.dropped_redis_records.load(Ordering::Relaxed),
            postgres_enabled: self.postgres_store.is_some(),
            writer_queue_enabled,
            writer_queue_capacity,
            writer_queue_available,
            writer_accepted: self
                .writer
                .as_ref()
                .map(|writer| writer.progress.accepted())
                .unwrap_or(0),
            writer_finished: self
                .writer
                .as_ref()
                .map(|writer| writer.progress.finished())
                .unwrap_or(0),
            backpressured_persist_records: self
                .backpressured_persist_records
                .load(Ordering::Relaxed),
            dropped_persist_records: self.dropped_persist_records.load(Ordering::Relaxed),
            rejected_after_shutdown: self.rejected_after_shutdown.load(Ordering::Relaxed),
            rejected_by_cleanup_watermark: self
                .rejected_by_cleanup_watermark
                .load(Ordering::Relaxed),
        }
    }

    pub async fn drain(&self, timeout: StdDuration) -> UsageRecorderDrainReport {
        let deadline = tokio::time::Instant::now() + timeout;
        let (postgres, postgres_timed_out) =
            drain_usage_writer(self.writer.as_deref(), deadline).await;
        let (redis, redis_timed_out) =
            drain_usage_writer(self.redis_writer.as_deref(), deadline).await;
        UsageRecorderDrainReport {
            postgres,
            redis,
            timed_out: postgres_timed_out || redis_timed_out,
        }
    }

    pub async fn shutdown(&self, timeout: StdDuration) -> UsageRecorderShutdownReport {
        let already_started = self
            .shutdown
            .started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err();
        if !already_started {
            {
                let _lifecycle = self.lifecycle.write();
                self.accepting.store(false, Ordering::Release);
                if let Some(writer) = &self.writer {
                    writer.close();
                }
                if let Some(writer) = &self.redis_writer {
                    writer.close();
                }
            }

            let writer = self.writer.clone();
            let redis_writer = self.redis_writer.clone();
            let shutdown = self.shutdown.clone();
            tokio::spawn(async move {
                let deadline = tokio::time::Instant::now() + timeout;
                let postgres_timed_out =
                    wait_usage_writer_task("PgSQL", writer.as_deref(), deadline).await;
                let redis_timed_out =
                    wait_usage_writer_task("Redis", redis_writer.as_deref(), deadline).await;
                shutdown
                    .timed_out
                    .store(postgres_timed_out || redis_timed_out, Ordering::Release);
                shutdown.complete.store(true, Ordering::Release);
                shutdown.changed.notify_waiters();
            });
        }

        let wait_timed_out = tokio::time::timeout(timeout, self.wait_for_shutdown())
            .await
            .is_err();
        self.shutdown_report(already_started, wait_timed_out)
    }

    async fn wait_for_shutdown(&self) {
        loop {
            let changed = self.shutdown.changed.notified();
            if self.shutdown.complete.load(Ordering::Acquire) {
                return;
            }
            changed.await;
        }
    }

    fn shutdown_report(
        &self,
        already_started: bool,
        wait_timed_out: bool,
    ) -> UsageRecorderShutdownReport {
        let stats = self.writer_stats();
        let postgres_abandoned = stats.writer_accepted.saturating_sub(stats.writer_finished);
        let redis_abandoned = stats
            .redis_writer_accepted
            .saturating_sub(stats.redis_writer_finished);
        let timed_out = wait_timed_out || self.shutdown.timed_out.load(Ordering::Acquire);
        UsageRecorderShutdownReport {
            already_started,
            drained: self.shutdown.complete.load(Ordering::Acquire)
                && !timed_out
                && postgres_abandoned == 0
                && redis_abandoned == 0,
            timed_out,
            postgres_abandoned,
            redis_abandoned,
            stats,
        }
    }

    pub fn query(&self, query: UsageRecordQuery) -> UsageRecordsResult {
        if let Some(store) = &self.postgres_store {
            let store = store.clone();
            let db_query = query.clone();
            return block_on_usage_store(async move { store.query(db_query).await })
                .unwrap_or_else(|err| {
                    tracing::warn!("查询 PgSQL usage records 失败，回退内存记录: {}", err);
                    self.query_memory(query)
                });
        }
        self.query_memory(query)
    }

    fn query_memory(&self, query: UsageRecordQuery) -> UsageRecordsResult {
        let limit = normalize_limit(query.limit);
        let mut matched: Vec<UsageRecord> = self
            .records
            .lock()
            .iter()
            .rev()
            .filter(|record| record_matches(record, &query))
            .cloned()
            .collect();
        let total = matched.len();
        matched.truncate(limit);
        UsageRecordsResult {
            total,
            records: matched,
        }
    }

    pub fn query_page(
        &self,
        query: UsageRecordQuery,
        page: usize,
        limit: usize,
    ) -> UsageRecordsPageResult {
        if let Some(store) = &self.postgres_store {
            let store = store.clone();
            let query_for_fallback = query.clone();
            return block_on_usage_store(async move { store.query_page(query, page, limit).await })
                .unwrap_or_else(|err| {
                    tracing::warn!(
                        "分页查询 PgSQL usage records 失败，回退 Redis/内存记录: {}",
                        err
                    );
                    self.query_page_without_postgres(query_for_fallback, page, limit)
                });
        }

        self.query_page_without_postgres(query, page, limit)
    }

    fn query_page_without_postgres(
        &self,
        query: UsageRecordQuery,
        page: usize,
        limit: usize,
    ) -> UsageRecordsPageResult {
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            let redis_query = query.clone();
            match block_on_usage_store(async move {
                redis.usage_records_page(redis_query, page, limit).await
            }) {
                Ok(Some(result)) => return result,
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!("分页查询 Redis usage records 失败，回退内存记录: {}", err)
                }
            }
        }

        self.query_page_memory(query, page, limit)
    }

    fn query_page_memory(
        &self,
        query: UsageRecordQuery,
        page: usize,
        limit: usize,
    ) -> UsageRecordsPageResult {
        let page = normalize_page(page);
        let limit = normalize_page_limit(limit);
        let start = page.saturating_sub(1).saturating_mul(limit);
        let mut records: Vec<UsageRecord> = self
            .records
            .lock()
            .iter()
            .rev()
            .filter(|record| record_matches(record, &query))
            .skip(start)
            .take(limit.saturating_add(1))
            .cloned()
            .collect();
        let has_next = records.len() > limit;
        if has_next {
            records.truncate(limit);
        }

        UsageRecordsPageResult {
            page,
            limit,
            has_next,
            records,
        }
    }

    pub fn summary(&self, high_cache_threshold: i32) -> UsageSummary {
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            match block_on_usage_store(
                async move { redis.usage_summary(high_cache_threshold).await },
            ) {
                Ok(Some(summary)) => return summary,
                Ok(None) => {}
                Err(err) => tracing::warn!("读取 Redis usage summary 失败，回退 PgSQL: {}", err),
            }
        }
        if let Some(store) = &self.postgres_store {
            let revision = self.postgres_cache_revision();
            if let Some(cached) = self.cached_postgres_summary(high_cache_threshold, revision) {
                return cached;
            }
            let store = store.clone();
            let summary =
                block_on_usage_store(async move { store.summary(high_cache_threshold).await })
                    .unwrap_or_else(|err| {
                        tracing::warn!("汇总 PgSQL usage records 失败，回退内存记录: {}", err);
                        self.summary_memory(high_cache_threshold)
                    });
            self.store_postgres_summary_cache(high_cache_threshold, revision, &summary);
            return summary;
        }
        self.summary_memory(high_cache_threshold)
    }

    fn dashboard_query<T, Fut>(
        &self,
        label: &'static str,
        timeout_secs: u64,
        future: Fut,
    ) -> anyhow::Result<T>
    where
        T: Send + 'static,
        Fut: std::future::Future<Output = anyhow::Result<T>> + Send + 'static,
    {
        let gate = self.dashboard_query_gate.clone();
        block_on_usage_store(async move {
            let permit = match tokio::time::timeout(
                StdDuration::from_millis(USAGE_DASHBOARD_GATE_WAIT_MS),
                gate.acquire_owned(),
            )
            .await
            {
                Ok(Ok(permit)) => permit,
                Ok(Err(_)) => anyhow::bail!("usage dashboard 查询入口已关闭"),
                Err(_) => anyhow::bail!("usage dashboard 查询繁忙，请稍后重试"),
            };

            let result = tokio::time::timeout(StdDuration::from_secs(timeout_secs), future).await;
            drop(permit);

            match result {
                Ok(result) => result,
                Err(_) => anyhow::bail!(
                    "读取 {} 超过 {} 秒，已中止本次后台查询",
                    label,
                    timeout_secs
                ),
            }
        })
    }

    fn postgres_cache_revision(&self) -> UsagePostgresCacheRevision {
        let (writer_accepted, writer_finished) = self
            .writer
            .as_ref()
            .map(|writer| (writer.progress.accepted(), writer.progress.finished()))
            .unwrap_or((0, 0));
        UsagePostgresCacheRevision {
            writer_accepted,
            writer_finished,
            cleanup_watermark_micros: self.cleanup_watermark_micros.load(Ordering::Acquire),
        }
    }

    fn cached_postgres_summary(
        &self,
        high_cache_threshold: i32,
        revision: UsagePostgresCacheRevision,
    ) -> Option<UsageSummary> {
        let cached = self.postgres_summary_cache.lock();
        cached.as_ref().and_then(|cached| {
            if cached.high_cache_threshold == high_cache_threshold
                && cached.revision == revision
                && cached.stored_at.elapsed() <= USAGE_POSTGRES_FALLBACK_CACHE_TTL
            {
                Some(cached.response.clone())
            } else {
                None
            }
        })
    }

    fn store_postgres_summary_cache(
        &self,
        high_cache_threshold: i32,
        revision: UsagePostgresCacheRevision,
        response: &UsageSummary,
    ) {
        *self.postgres_summary_cache.lock() = Some(CachedUsageSummary {
            high_cache_threshold,
            revision,
            stored_at: Instant::now(),
            response: response.clone(),
        });
    }

    fn cached_postgres_dashboard(
        &self,
        timezone: &str,
        high_cache_threshold: i32,
        revision: UsagePostgresCacheRevision,
    ) -> Option<UsageDashboardResponse> {
        let cached = self.postgres_dashboard_cache.lock();
        cached.as_ref().and_then(|cached| {
            if cached.timezone == timezone
                && cached.high_cache_threshold == high_cache_threshold
                && cached.revision == revision
                && cached.stored_at.elapsed() <= USAGE_POSTGRES_FALLBACK_CACHE_TTL
            {
                Some(cached.response.clone())
            } else {
                None
            }
        })
    }

    fn store_postgres_dashboard_cache(
        &self,
        timezone: String,
        high_cache_threshold: i32,
        revision: UsagePostgresCacheRevision,
        response: &UsageDashboardResponse,
    ) {
        *self.postgres_dashboard_cache.lock() = Some(CachedUsageDashboard {
            timezone,
            high_cache_threshold,
            revision,
            stored_at: Instant::now(),
            response: response.clone(),
        });
    }

    pub fn dashboard(
        &self,
        timezone: Option<&str>,
        high_cache_threshold: i32,
    ) -> anyhow::Result<UsageDashboardResponse> {
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            let timezone = timezone.map(str::to_string);
            match block_on_usage_store(async move {
                match tokio::time::timeout(
                    StdDuration::from_secs(USAGE_DASHBOARD_REDIS_TIMEOUT_SECS),
                    redis.usage_dashboard(timezone.as_deref(), high_cache_threshold),
                )
                .await
                {
                    Ok(result) => result,
                    Err(_) => anyhow::bail!(
                        "读取 Redis usage dashboard 超过 {} 秒",
                        USAGE_DASHBOARD_REDIS_TIMEOUT_SECS
                    ),
                }
            }) {
                Ok(Some(dashboard)) => return Ok(dashboard),
                Ok(None) => {}
                Err(err) => tracing::warn!("读取 Redis usage dashboard 失败，回退 PgSQL: {}", err),
            }
        }
        if let Some(store) = &self.postgres_store {
            let store = store.clone();
            let timezone_input = timezone.map(str::to_string);
            let cache_timezone = usage_dashboard_timezone(timezone).0;
            let revision = self.postgres_cache_revision();
            if let Some(cached) =
                self.cached_postgres_dashboard(&cache_timezone, high_cache_threshold, revision)
            {
                return Ok(cached);
            }
            let dashboard = self.dashboard_query(
                "PgSQL usage dashboard",
                USAGE_DASHBOARD_POSTGRES_TIMEOUT_SECS,
                async move {
                    store
                        .dashboard(timezone_input.as_deref(), high_cache_threshold)
                        .await
                },
            )?;
            self.store_postgres_dashboard_cache(
                cache_timezone,
                high_cache_threshold,
                revision,
                &dashboard,
            );
            return Ok(dashboard);
        }

        anyhow::bail!("usage dashboard 的精确窗口与 P95 需要 PgSQL 聚合存储")
    }

    pub fn dashboard_windows(
        &self,
        timezone: Option<&str>,
        high_cache_threshold: i32,
    ) -> anyhow::Result<UsageDashboardWindowsResponse> {
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            let timezone = timezone.map(str::to_string);
            match block_on_usage_store(async move {
                match tokio::time::timeout(
                    StdDuration::from_secs(USAGE_DASHBOARD_REDIS_TIMEOUT_SECS),
                    redis.usage_dashboard_windows_only(timezone.as_deref(), high_cache_threshold),
                )
                .await
                {
                    Ok(result) => result,
                    Err(_) => anyhow::bail!(
                        "读取 Redis usage dashboard windows 超过 {} 秒",
                        USAGE_DASHBOARD_REDIS_TIMEOUT_SECS
                    ),
                }
            }) {
                Ok(Some((generated_at, timezone, windows))) => {
                    return Ok(UsageDashboardWindowsResponse {
                        generated_at,
                        timezone,
                        windows,
                    });
                }
                Ok(None) => {}
                Err(err) => tracing::warn!(
                    "读取 Redis usage dashboard windows 失败，回退 PgSQL: {}",
                    err
                ),
            }
        }
        if let Some(store) = &self.postgres_store {
            let store = store.clone();
            let timezone = timezone.map(str::to_string);
            let (generated_at, timezone, windows) = self.dashboard_query(
                "PgSQL usage dashboard windows",
                USAGE_DASHBOARD_POSTGRES_TIMEOUT_SECS,
                async move {
                    store
                        .dashboard_windows_only(timezone.as_deref(), high_cache_threshold)
                        .await
                },
            )?;
            return Ok(UsageDashboardWindowsResponse {
                generated_at,
                timezone,
                windows,
            });
        }
        anyhow::bail!("usage dashboard windows 的精确人口与 P95 需要 PgSQL 聚合存储")
    }

    pub fn dashboard_series(
        &self,
        timezone: Option<&str>,
    ) -> anyhow::Result<UsageDashboardSeriesResponse> {
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            let timezone = timezone.map(str::to_string);
            match block_on_usage_store(async move {
                match tokio::time::timeout(
                    StdDuration::from_secs(USAGE_DASHBOARD_REDIS_TIMEOUT_SECS),
                    redis.usage_dashboard_series_only(timezone.as_deref()),
                )
                .await
                {
                    Ok(result) => result,
                    Err(_) => anyhow::bail!(
                        "读取 Redis usage dashboard series 超过 {} 秒",
                        USAGE_DASHBOARD_REDIS_TIMEOUT_SECS
                    ),
                }
            }) {
                Ok(Some((generated_at, timezone, series))) => {
                    return Ok(UsageDashboardSeriesResponse {
                        generated_at,
                        timezone,
                        series,
                    });
                }
                Ok(None) => {}
                Err(err) => tracing::warn!(
                    "读取 Redis usage dashboard series 失败，回退 PgSQL: {}",
                    err
                ),
            }
        }
        if let Some(store) = &self.postgres_store {
            let store = store.clone();
            let timezone = timezone.map(str::to_string);
            let (generated_at, timezone, series) = self.dashboard_query(
                "PgSQL usage dashboard series",
                USAGE_DASHBOARD_POSTGRES_TIMEOUT_SECS,
                async move { store.dashboard_series_only(timezone.as_deref()).await },
            )?;
            return Ok(UsageDashboardSeriesResponse {
                generated_at,
                timezone,
                series,
            });
        }
        anyhow::bail!("usage dashboard series 需要 Redis 或 PgSQL 聚合存储")
    }

    pub fn dashboard_top(
        &self,
        timezone: Option<&str>,
        window_key: Option<&str>,
    ) -> anyhow::Result<UsageDashboardTopResponse> {
        let window_key = window_key
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("lifetime");
        if window_key != "lifetime" {
            if let Some(store) = &self.postgres_store {
                let store = store.clone();
                let timezone = timezone.map(str::to_string);
                let window_key = window_key.to_string();
                let (generated_at, top) = self.dashboard_query(
                    "PgSQL usage dashboard window top",
                    USAGE_DASHBOARD_POSTGRES_TIMEOUT_SECS,
                    async move {
                        store
                            .dashboard_top_for_window(timezone.as_deref(), &window_key)
                            .await
                    },
                )?;
                return Ok(UsageDashboardTopResponse { generated_at, top });
            }
            anyhow::bail!("usage dashboard window top 需要 PgSQL 聚合存储")
        }

        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            match block_on_usage_store(async move {
                match tokio::time::timeout(
                    StdDuration::from_secs(USAGE_DASHBOARD_REDIS_TIMEOUT_SECS),
                    redis.usage_dashboard_top_only(),
                )
                .await
                {
                    Ok(result) => result,
                    Err(_) => anyhow::bail!(
                        "读取 Redis usage dashboard top 超过 {} 秒",
                        USAGE_DASHBOARD_REDIS_TIMEOUT_SECS
                    ),
                }
            }) {
                Ok(Some((generated_at, top))) => {
                    return Ok(UsageDashboardTopResponse { generated_at, top });
                }
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!("读取 Redis usage dashboard top 失败，回退 PgSQL: {}", err)
                }
            }
        }
        if let Some(store) = &self.postgres_store {
            let store = store.clone();
            let (generated_at, top) = self.dashboard_query(
                "PgSQL usage dashboard top",
                USAGE_DASHBOARD_POSTGRES_TIMEOUT_SECS,
                async move { store.dashboard_top_only().await },
            )?;
            return Ok(UsageDashboardTopResponse { generated_at, top });
        }
        anyhow::bail!("usage dashboard top 需要 Redis 或 PgSQL 聚合存储")
    }

    pub fn dashboard_breakdown(
        &self,
        timezone: Option<&str>,
        window_key: &str,
    ) -> anyhow::Result<UsageDashboardBreakdownResponse> {
        if let Some(store) = &self.postgres_store {
            let store = store.clone();
            let timezone = timezone.map(str::to_string);
            let window_key = window_key.to_string();
            let (generated_at, timezone, window_key, status_breakdown, usage_source_breakdown) =
                self.dashboard_query(
                    "PgSQL usage dashboard breakdown",
                    USAGE_DASHBOARD_POSTGRES_TIMEOUT_SECS,
                    async move {
                        store
                            .dashboard_breakdown_only(timezone.as_deref(), &window_key)
                            .await
                    },
                )?;
            return Ok(UsageDashboardBreakdownResponse {
                generated_at,
                timezone,
                window_key,
                status_breakdown,
                usage_source_breakdown,
            });
        }
        anyhow::bail!("usage dashboard breakdown 的精确窗口人口需要 PgSQL 聚合存储")
    }

    pub fn dashboard_external_pool_billing(
        &self,
        timezone: Option<&str>,
        window_key: &str,
    ) -> anyhow::Result<UsageDashboardExternalPoolBillingResponse> {
        if let Some(store) = &self.postgres_store {
            let store = store.clone();
            let timezone = timezone.map(str::to_string);
            let window_key = window_key.to_string();
            let (generated_at, timezone, window_key, external_pool_billing_by_pool) = self
                .dashboard_query(
                    "PgSQL usage dashboard external pool billing",
                    USAGE_DASHBOARD_POSTGRES_TIMEOUT_SECS,
                    async move {
                        store
                            .dashboard_external_pool_billing_only(timezone.as_deref(), &window_key)
                            .await
                    },
                )?;
            return Ok(UsageDashboardExternalPoolBillingResponse {
                generated_at,
                timezone,
                window_key,
                external_pool_billing_by_pool,
            });
        }
        anyhow::bail!("usage dashboard external pool billing 的精确窗口人口需要 PgSQL 聚合存储")
    }

    pub fn external_pool_usage_risk(
        &self,
        query: UsageExternalPoolRiskQuery,
        cost_config: UsageExternalPoolRiskCostConfig,
    ) -> anyhow::Result<UsageExternalPoolRiskResponse> {
        if let Some(store) = &self.postgres_store {
            let store = store.clone();
            return self.dashboard_query(
                "PgSQL external pool usage risk",
                USAGE_DASHBOARD_POSTGRES_TIMEOUT_SECS,
                async move { store.external_pool_usage_risk(query, cost_config).await },
            );
        }
        anyhow::bail!("外部池 usage 风控需要 PgSQL usage 明细存储")
    }

    fn summary_memory(&self, high_cache_threshold: i32) -> UsageSummary {
        let records = self.records.lock();
        let mut summary = UsageSummary {
            total_requests: records.len(),
            success_requests: 0,
            error_requests: 0,
            high_cache_requests: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_read_input_tokens: 0,
            total_cache_creation_input_tokens: 0,
            total_estimated_cost_usd: 0.0,
            total_original_cost_usd: 0.0,
            total_kiro_metering_usage: 0.0,
            priced_requests: 0,
            unpriced_requests: 0,
            local_prompt_cache_requests: 0,
            local_prompt_cache_input_tokens: 0,
            local_prompt_cache_read_input_tokens: 0,
            local_prompt_cache_creation_input_tokens: 0,
            simulated_requests: 0,
            upstream_metadata_requests: 0,
            external_pool_billing: UsageExternalPoolBillingSummary::default(),
            realtime: UsageRealtimeStats::empty(REALTIME_USAGE_WINDOW_SECS),
            top_credentials: Vec::new(),
            top_conversations: Vec::new(),
        };
        let mut credentials: HashMap<String, UsageAggregate> = HashMap::new();
        let mut conversations: HashMap<String, UsageAggregate> = HashMap::new();
        let realtime_cutoff =
            Utc::now() - chrono::Duration::seconds(REALTIME_USAGE_WINDOW_SECS as i64);
        let mut realtime_requests = 0usize;
        let mut realtime_success_requests = 0usize;
        let mut realtime_error_requests = 0usize;
        let mut realtime_input_tokens = 0i64;
        let mut realtime_output_tokens = 0i64;
        let mut realtime_billable_input_tokens = 0i64;

        for record in records.iter() {
            if record.status == UsageRecordStatus::Success {
                summary.success_requests += 1;
            } else {
                summary.error_requests += 1;
            }
            if record.cache_read_input_tokens >= high_cache_threshold {
                summary.high_cache_requests += 1;
            }
            if record.simulated {
                summary.simulated_requests += 1;
            }
            if record.usage_source == UsageSource::UpstreamMetadata {
                summary.upstream_metadata_requests += 1;
            }
            if record.route_kind == Some(UsageRouteKind::ExternalPool) {
                summary.external_pool_billing.requests += 1;
                if let Some(billing) = &record.external_pool_billing {
                    if billing.pricing_available {
                        summary.external_pool_billing.priced_requests += 1;
                    } else {
                        summary.external_pool_billing.unpriced_requests += 1;
                    }
                    if billing.cost_floor_applied {
                        summary.external_pool_billing.cost_floor_applied_requests += 1;
                    }
                    summary.external_pool_billing.raw_cost_usd += billing.raw_cost_usd;
                    summary.external_pool_billing.shaped_cost_usd +=
                        billing.effective_shaped_cost_usd();
                    summary.external_pool_billing.uplifted_cost_usd +=
                        billing.effective_uplifted_cost_usd();
                    summary.external_pool_billing.profit_usd += billing.effective_profit_usd();
                    summary.external_pool_billing.reported_cost_usd += billing.reported_cost_usd;
                    summary.external_pool_billing.billable_cost_usd += billing.billable_cost_usd;
                    summary.external_pool_billing.cost_floor_delta_usd +=
                        billing.cost_floor_delta_usd;
                } else {
                    summary.external_pool_billing.unpriced_requests += 1;
                }
            }
            summary.total_input_tokens += record.total_input_tokens as i64;
            summary.total_output_tokens += record.output_tokens as i64;
            summary.total_cache_read_input_tokens += record.cache_read_input_tokens as i64;
            summary.total_cache_creation_input_tokens += record.cache_creation_input_tokens as i64;
            summary.total_estimated_cost_usd += record.estimated_cost_usd;
            summary.total_original_cost_usd += record.original_cost_usd;
            summary.total_kiro_metering_usage += record.kiro_metering_usage;
            if record.pricing_available {
                summary.priced_requests += 1;
            } else {
                summary.unpriced_requests += 1;
            }
            if record.usage_source == UsageSource::LocalPromptCache {
                summary.local_prompt_cache_requests += 1;
                summary.local_prompt_cache_input_tokens += record.total_input_tokens as i64;
                summary.local_prompt_cache_read_input_tokens +=
                    record.cache_read_input_tokens as i64;
                summary.local_prompt_cache_creation_input_tokens +=
                    record.cache_creation_input_tokens as i64;
            }
            if DateTime::parse_from_rfc3339(&record.created_at)
                .map(|created_at| created_at.with_timezone(&Utc) >= realtime_cutoff)
                .unwrap_or(false)
            {
                realtime_requests += 1;
                if record.status == UsageRecordStatus::Success {
                    realtime_success_requests += 1;
                } else {
                    realtime_error_requests += 1;
                }
                realtime_input_tokens += record.total_input_tokens as i64;
                realtime_output_tokens += record.output_tokens as i64;
                realtime_billable_input_tokens += record.billable_input_tokens as i64;
            }

            if let Some(id) = record.credential_id {
                let key = id.to_string();
                let entry = credentials.entry(key.clone()).or_insert(UsageAggregate {
                    key,
                    label: record.credential_label.clone(),
                    requests: 0,
                    cache_read_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                    estimated_cost_usd: 0.0,
                    original_cost_usd: 0.0,
                });
                entry.requests += 1;
                entry.cache_read_input_tokens += record.cache_read_input_tokens as i64;
                entry.cache_creation_input_tokens += record.cache_creation_input_tokens as i64;
                entry.estimated_cost_usd += record.estimated_cost_usd;
                entry.original_cost_usd += record.original_cost_usd;
                if entry.label.is_none() {
                    entry.label = record.credential_label.clone();
                }
            }

            if let Some(conversation_id) = &record.conversation_id {
                let entry =
                    conversations
                        .entry(conversation_id.clone())
                        .or_insert(UsageAggregate {
                            key: conversation_id.clone(),
                            label: None,
                            requests: 0,
                            cache_read_input_tokens: 0,
                            cache_creation_input_tokens: 0,
                            estimated_cost_usd: 0.0,
                            original_cost_usd: 0.0,
                        });
                entry.requests += 1;
                entry.cache_read_input_tokens += record.cache_read_input_tokens as i64;
                entry.cache_creation_input_tokens += record.cache_creation_input_tokens as i64;
                entry.estimated_cost_usd += record.estimated_cost_usd;
                entry.original_cost_usd += record.original_cost_usd;
            }
        }

        summary.top_credentials = top_aggregates(credentials);
        summary.top_conversations = top_aggregates(conversations);
        summary.realtime = UsageRealtimeStats::from_totals_with_status(
            REALTIME_USAGE_WINDOW_SECS,
            realtime_requests,
            realtime_success_requests,
            realtime_error_requests,
            realtime_input_tokens,
            realtime_output_tokens,
            realtime_billable_input_tokens,
        );
        summary
    }

    pub fn credential_cost_summary(&self) -> HashMap<u64, CredentialCostSummary> {
        if let Some(store) = &self.postgres_store {
            let store = store.clone();
            return block_on_usage_store(async move { store.credential_cost_summary().await })
                .unwrap_or_else(|err| {
                    tracing::warn!("汇总 PgSQL 凭据费用失败，回退内存记录: {}", err);
                    self.credential_cost_summary_memory()
                });
        }
        self.credential_cost_summary_memory()
    }

    pub fn credential_cost_summary_for_ids(
        &self,
        ids: &[u64],
    ) -> HashMap<u64, CredentialCostSummary> {
        if ids.is_empty() {
            return HashMap::new();
        }
        if let Some(store) = &self.postgres_store {
            let store = store.clone();
            let ids = ids.to_vec();
            let fallback_ids = ids.clone();
            return block_on_usage_store(async move {
                store.credential_cost_summary_for_ids(&ids).await
            })
            .unwrap_or_else(|err| {
                tracing::warn!("按 ID 汇总 PgSQL 凭据费用失败，回退内存记录: {}", err);
                self.credential_cost_summary_memory_for_ids(fallback_ids.as_slice())
            });
        }
        self.credential_cost_summary_memory_for_ids(ids)
    }

    fn credential_cost_summary_memory(&self) -> HashMap<u64, CredentialCostSummary> {
        let mut summaries: HashMap<u64, CredentialCostSummary> = HashMap::new();
        for record in self.records.lock().iter() {
            let Some(credential_id) = record.credential_id else {
                continue;
            };
            let entry = summaries.entry(credential_id).or_default();
            entry.estimated_cost_usd += record.estimated_cost_usd;
            entry.original_cost_usd += record.original_cost_usd;
            entry.kiro_metering_usage += record.kiro_metering_usage;
            if record.pricing_available {
                entry.priced_requests += 1;
            } else {
                entry.unpriced_requests += 1;
            }
        }
        summaries
    }

    fn credential_cost_summary_memory_for_ids(
        &self,
        ids: &[u64],
    ) -> HashMap<u64, CredentialCostSummary> {
        let wanted: HashSet<u64> = ids.iter().copied().collect();
        let mut summaries: HashMap<u64, CredentialCostSummary> = HashMap::new();
        for record in self.records.lock().iter() {
            let Some(credential_id) = record.credential_id else {
                continue;
            };
            if !wanted.contains(&credential_id) {
                continue;
            }
            let entry = summaries.entry(credential_id).or_default();
            entry.estimated_cost_usd += record.estimated_cost_usd;
            entry.original_cost_usd += record.original_cost_usd;
            entry.kiro_metering_usage += record.kiro_metering_usage;
            if record.pricing_available {
                entry.priced_requests += 1;
            } else {
                entry.unpriced_requests += 1;
            }
        }
        summaries
    }

    pub async fn advance_cleanup_watermark(&self, cutoff: DateTime<Utc>) -> anyhow::Result<usize> {
        let cutoff_micros = cutoff.timestamp_micros().max(0);
        self.cleanup_watermark_micros
            .fetch_max(cutoff_micros, Ordering::AcqRel);
        let removed = self.remove_memory_records_before(cutoff);
        if let Some(redis) = &self.redis_store {
            redis.advance_usage_cleanup_watermark(cutoff).await?;
        }
        Ok(removed)
    }

    pub fn remove_memory_records_before(&self, cutoff: DateTime<Utc>) -> usize {
        let mut records = self.records.lock();
        let before = records.len();
        records.retain(|record| {
            DateTime::parse_from_rfc3339(&record.created_at)
                .map(|created_at| created_at.with_timezone(&Utc) >= cutoff)
                .unwrap_or(true)
        });
        before.saturating_sub(records.len())
    }
}

async fn drain_usage_writer(
    writer: Option<&UsageWriterControl>,
    deadline: tokio::time::Instant,
) -> (UsageWriterDrainStatus, bool) {
    let Some(writer) = writer else {
        return (UsageWriterDrainStatus::default(), false);
    };
    let target = writer.progress.accepted();
    let finished = writer.progress.finished();
    if finished >= target {
        return (
            UsageWriterDrainStatus {
                target,
                finished,
                drained: true,
            },
            false,
        );
    }
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() {
        return (
            UsageWriterDrainStatus {
                target,
                finished,
                drained: false,
            },
            true,
        );
    }
    match tokio::time::timeout(remaining, writer.progress.wait_until(target)).await {
        Ok(finished) => (
            UsageWriterDrainStatus {
                target,
                finished,
                drained: true,
            },
            false,
        ),
        Err(_) => (
            UsageWriterDrainStatus {
                target,
                finished: writer.progress.finished(),
                drained: false,
            },
            true,
        ),
    }
}

async fn wait_usage_writer_task(
    writer_name: &'static str,
    writer: Option<&UsageWriterControl>,
    deadline: tokio::time::Instant,
) -> bool {
    let Some(writer) = writer else {
        return false;
    };
    let Some(mut task) = writer.take_task() else {
        return false;
    };
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    if !remaining.is_zero() {
        match tokio::time::timeout(remaining, &mut task).await {
            Ok(Ok(())) => return false,
            Ok(Err(err)) => {
                tracing::warn!(writer_name, "usage writer 异常退出: {}", err);
                return false;
            }
            Err(_) => {}
        }
    }
    task.abort();
    if tokio::time::timeout(USAGE_WRITER_ABORT_JOIN_TIMEOUT, &mut task)
        .await
        .is_err()
    {
        tracing::warn!(
            writer_name,
            timeout_ms = USAGE_WRITER_ABORT_JOIN_TIMEOUT.as_millis() as u64,
            "等待已取消的 usage writer 退出再次超时"
        );
    }
    tracing::warn!(writer_name, "等待 usage writer 排空超时，已停止 writer");
    true
}

fn should_log_usage_counter(value: u64) -> bool {
    value == 1 || value.is_power_of_two()
}

async fn usage_writer_loop(
    store: Arc<PostgresUsageStore>,
    mut rx: mpsc::Receiver<UsageRecord>,
    dropped_records: Arc<AtomicU64>,
    progress: Arc<UsageWriterProgress>,
) {
    while let Some(first) = rx.recv().await {
        let mut records = Vec::with_capacity(USAGE_WRITER_BATCH_MAX);
        records.push(first);
        drain_usage_writer_queue(&mut rx, &mut records);
        if records.len() < USAGE_WRITER_BATCH_MAX {
            match tokio::time::timeout(USAGE_WRITER_BATCH_COALESCE_DELAY, rx.recv()).await {
                Ok(Some(record)) => {
                    records.push(record);
                    drain_usage_writer_queue(&mut rx, &mut records);
                }
                Ok(None) | Err(_) => {}
            }
        }
        let record_count = records.len();
        persist_usage_batch_with_retry(&store, records, &dropped_records).await;
        progress.mark_finished(record_count);
    }
}

fn drain_usage_writer_queue(rx: &mut mpsc::Receiver<UsageRecord>, records: &mut Vec<UsageRecord>) {
    while records.len() < USAGE_WRITER_BATCH_MAX {
        match rx.try_recv() {
            Ok(record) => records.push(record),
            Err(mpsc::error::TryRecvError::Empty) => break,
            Err(mpsc::error::TryRecvError::Disconnected) => break,
        }
    }
}

async fn persist_usage_batch_with_retry(
    store: &Arc<PostgresUsageStore>,
    records: Vec<UsageRecord>,
    dropped_records: &Arc<AtomicU64>,
) {
    let first_request_id = records
        .first()
        .map(|record| record.id.as_str())
        .unwrap_or_default()
        .to_string();
    let record_count = records.len();
    let mut attempt = 1;
    loop {
        let result = tokio::time::timeout(
            StdDuration::from_secs(USAGE_WRITER_POSTGRES_TIMEOUT_SECS),
            store.record_batch(records.clone()),
        )
        .await;
        match result {
            Ok(Ok(())) => break,
            Err(_) if attempt < USAGE_WRITER_MAX_ATTEMPTS => {
                let delay_ms = 100u64.saturating_mul(2u64.saturating_pow(attempt - 1));
                tracing::warn!(
                    request_id = %first_request_id,
                    record_count,
                    attempt,
                    timeout_secs = USAGE_WRITER_POSTGRES_TIMEOUT_SECS,
                    "批量写入 PgSQL usage record 超时，准备重试"
                );
                tokio::time::sleep(StdDuration::from_millis(delay_ms)).await;
                attempt += 1;
            }
            Err(_) => {
                let dropped = dropped_records.fetch_add(record_count as u64, Ordering::Relaxed)
                    + record_count as u64;
                tracing::warn!(
                    request_id = %first_request_id,
                    record_count,
                    attempt,
                    timeout_secs = USAGE_WRITER_POSTGRES_TIMEOUT_SECS,
                    dropped,
                    "批量写入 PgSQL usage record 最终超时，已放弃本批持久化"
                );
                break;
            }
            Ok(Err(err)) if attempt < USAGE_WRITER_MAX_ATTEMPTS => {
                let delay_ms = 100u64.saturating_mul(2u64.saturating_pow(attempt - 1));
                tracing::warn!(
                    request_id = %first_request_id,
                    record_count,
                    attempt,
                    "批量写入 PgSQL usage record 失败，准备重试: {}",
                    err
                );
                tokio::time::sleep(StdDuration::from_millis(delay_ms)).await;
                attempt += 1;
            }
            Ok(Err(err)) => {
                let dropped = dropped_records.fetch_add(record_count as u64, Ordering::Relaxed)
                    + record_count as u64;
                tracing::warn!(
                    request_id = %first_request_id,
                    record_count,
                    attempt,
                    dropped,
                    "批量写入 PgSQL usage record 最终失败，已放弃本批持久化: {}",
                    err
                );
                break;
            }
        }
    }
}

async fn usage_redis_writer_loop(
    redis: Arc<RedisStore>,
    mut rx: mpsc::Receiver<UsageRecord>,
    dropped_records: Arc<AtomicU64>,
    progress: Arc<UsageWriterProgress>,
) {
    while let Some(first) = rx.recv().await {
        let mut records = Vec::with_capacity(USAGE_REDIS_WRITER_BATCH_MAX);
        records.push(first);
        drain_usage_redis_writer_queue(&mut rx, &mut records);
        if records.len() < USAGE_REDIS_WRITER_BATCH_MAX {
            match tokio::time::timeout(USAGE_REDIS_WRITER_BATCH_COALESCE_DELAY, rx.recv()).await {
                Ok(Some(record)) => {
                    records.push(record);
                    drain_usage_redis_writer_queue(&mut rx, &mut records);
                }
                Ok(None) | Err(_) => {}
            }
        }
        let record_count = records.len();
        persist_usage_redis_batch_with_timeout(&redis, records, &dropped_records).await;
        progress.mark_finished(record_count);
    }
}

fn drain_usage_redis_writer_queue(
    rx: &mut mpsc::Receiver<UsageRecord>,
    records: &mut Vec<UsageRecord>,
) {
    while records.len() < USAGE_REDIS_WRITER_BATCH_MAX {
        match rx.try_recv() {
            Ok(record) => records.push(record),
            Err(mpsc::error::TryRecvError::Empty) => break,
            Err(mpsc::error::TryRecvError::Disconnected) => break,
        }
    }
}

async fn persist_usage_redis_batch_with_timeout(
    redis: &Arc<RedisStore>,
    records: Vec<UsageRecord>,
    dropped_records: &Arc<AtomicU64>,
) {
    let record_count = records.len();
    let first_request_id = records
        .first()
        .map(|record| record.id.as_str())
        .unwrap_or_default()
        .to_string();
    let started_at = Instant::now();
    let results = run_bounded_usage_batch(
        records,
        StdDuration::from_secs(USAGE_REDIS_WRITER_TIMEOUT_SECS),
        |record| async move { redis.record_usage_summary(&record).await.map(|_| ()) },
    )
    .await;
    let mut failed = 0u64;
    let mut last_error: Option<String> = None;
    for result in results {
        if let Err(err) = result {
            failed += 1;
            last_error = Some(err.to_string());
        }
    }
    if failed > 0 {
        let dropped = dropped_records.fetch_add(failed, Ordering::Relaxed) + failed;
        tracing::warn!(
            request_id = %first_request_id,
            record_count,
            failed,
            dropped,
            timeout_secs = USAGE_REDIS_WRITER_TIMEOUT_SECS,
            elapsed_ms = started_at.elapsed().as_millis() as u64,
            error = ?last_error,
            "写入 Redis usage summary 失败，已丢弃部分观测记录"
        );
    } else {
        let elapsed = started_at.elapsed();
        if elapsed >= StdDuration::from_millis(250) {
            tracing::debug!(
                request_id = %first_request_id,
                record_count,
                elapsed_ms = elapsed.as_millis() as u64,
                "Redis usage summary 批量写入耗时较长"
            );
        }
    }
}

async fn run_bounded_usage_batch<T, F, Fut>(
    items: Vec<T>,
    batch_timeout: StdDuration,
    write: F,
) -> Vec<anyhow::Result<()>>
where
    F: Fn(T) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
{
    let deadline = tokio::time::Instant::now() + batch_timeout;
    let mut results = Vec::with_capacity(items.len());
    for item in items {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            results.push(Err(anyhow::anyhow!(
                "usage batch timed out after {}ms before the operation started",
                batch_timeout.as_millis()
            )));
            continue;
        }
        let result = tokio::time::timeout(remaining, write(item))
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "usage batch timed out after {}ms",
                    batch_timeout.as_millis()
                )
            })
            .and_then(|result| result);
        results.push(result);
    }
    results
}

fn block_on_usage_store<T: Send>(
    future: impl std::future::Future<Output = anyhow::Result<T>> + Send,
) -> anyhow::Result<T> {
    let started_at = Instant::now();
    let result = block_on_usage_runtime(future)?;
    let elapsed = started_at.elapsed();
    if elapsed >= StdDuration::from_millis(100) {
        tracing::warn!(
            elapsed_ms = elapsed.as_millis() as u64,
            "同步 usage 存储操作耗时较长"
        );
    }
    result
}

fn block_on_usage_runtime<T: Send>(
    future: impl std::future::Future<Output = T> + Send,
) -> anyhow::Result<T> {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        match handle.runtime_flavor() {
            RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(|| block_on_usage_fallback_thread(future))
            }
            _ => block_on_usage_fallback_thread(future),
        }
    } else {
        Ok(tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(future))
    }
}

fn block_on_usage_fallback_thread<T: Send>(
    future: impl std::future::Future<Output = T> + Send,
) -> anyhow::Result<T> {
    std::thread::scope(|scope| {
        scope
            .spawn(move || usage_fallback_runtime().block_on(future))
            .join()
            .map_err(|_| anyhow::anyhow!("usage 存储线程异常退出"))
    })
}

fn usage_fallback_runtime() -> &'static Runtime {
    static FALLBACK_RUNTIME: OnceLock<Runtime> = OnceLock::new();
    FALLBACK_RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("kiro-usage-store")
            .enable_all()
            .build()
            .expect("创建 usage 存储 runtime 失败")
    })
}

fn normalize_limit(limit: usize) -> usize {
    if limit == 0 {
        DEFAULT_QUERY_LIMIT
    } else {
        limit.min(MAX_QUERY_LIMIT)
    }
}

fn normalize_page_limit(limit: usize) -> usize {
    if limit == 0 {
        DEFAULT_PAGE_QUERY_LIMIT
    } else {
        limit.min(MAX_QUERY_LIMIT)
    }
}

fn normalize_page(page: usize) -> usize {
    page.max(1)
}

fn record_matches(record: &UsageRecord, query: &UsageRecordQuery) -> bool {
    if let Some(request_id) = &query.request_id {
        if &record.id != request_id {
            return false;
        }
    }
    if let Some(q) = &query.q {
        if !record_matches_search(record, q) {
            return false;
        }
    }
    if let Some(endpoint) = &query.endpoint {
        let endpoint = endpoint.trim().to_ascii_lowercase();
        if !endpoint.is_empty() && !record.endpoint.to_ascii_lowercase().contains(&endpoint) {
            return false;
        }
    }
    if let Some(conversation_id) = &query.conversation_id {
        if record.conversation_id.as_ref() != Some(conversation_id) {
            return false;
        }
    }
    if let Some(request_api_key_id) = &query.request_api_key_id {
        if record.request_api_key_id.as_ref() != Some(request_api_key_id) {
            return false;
        }
    }
    if let Some(credential_id) = query.credential_id {
        if record.credential_id != Some(credential_id) {
            return false;
        }
    }
    if let Some(external_pool_id) = query.external_pool_id {
        if record.external_pool_id != Some(external_pool_id) {
            return false;
        }
    }
    if let Some(route_kind) = query.route_kind {
        if record.route_kind != Some(route_kind) {
            return false;
        }
    }
    if let Some(model) = &query.model {
        if &record.model != model
            && record.upstream_model.as_ref() != Some(model)
            && record.external_outbound_model.as_ref() != Some(model)
        {
            return false;
        }
    }
    if let Some(status) = query.status {
        if record.status != status {
            return false;
        }
    }
    if let Some(source) = query.source {
        if record.usage_source != source {
            return false;
        }
    }
    if let Some(stream) = query.stream {
        if record.stream != stream {
            return false;
        }
    }
    if let Some(min_cache_read) = query.min_cache_read {
        if record.cache_read_input_tokens < min_cache_read {
            return false;
        }
    }
    if let Some(min_first_token_latency_ms) = query.min_first_token_latency_ms {
        if record
            .first_token_latency_ms
            .map_or(true, |value| value < min_first_token_latency_ms)
        {
            return false;
        }
    }
    if let Some(since) = query.since {
        let Some(created_at) = parse_record_time(&record.created_at) else {
            return false;
        };
        if created_at < since {
            return false;
        }
    }
    if let Some(until) = query.until {
        let Some(created_at) = parse_record_time(&record.created_at) else {
            return false;
        };
        if created_at > until {
            return false;
        }
    }
    true
}

fn record_matches_search(record: &UsageRecord, q: &str) -> bool {
    let q = q.trim().to_ascii_lowercase();
    if q.is_empty() {
        return true;
    }

    let status = usage_status_value(record.status);
    let source = usage_source_value(record.usage_source);
    let credential_id = record.credential_id.map(|id| id.to_string());
    let external_pool_id = record.external_pool_id.map(|id| id.to_string());
    let estimated_cost = record.estimated_cost_usd.to_string();
    let original_cost = record.original_cost_usd.to_string();
    let kiro_metering_usage = record.kiro_metering_usage.to_string();
    let attempt_chain = summarize_attempts(&record.credential_attempts);

    [
        Some(record.id.as_str()),
        Some(record.created_at.as_str()),
        Some(record.endpoint.as_str()),
        Some(record.model.as_str()),
        record.upstream_model.as_deref(),
        record.external_outbound_model.as_deref(),
        record.model_resolution_source.as_deref(),
        record.model_resolution_note.as_deref(),
        record.conversation_id.as_deref(),
        record.request_api_key_id.as_deref(),
        external_pool_id.as_deref(),
        record.external_pool_name.as_deref(),
        Some(original_cost.as_str()),
        record.credential_label.as_deref(),
        Some(status),
        Some(source),
        record.error_type.as_deref(),
        record.error_message.as_deref(),
        record.error_detail.as_deref(),
        record.pricing_model.as_deref(),
        Some(estimated_cost.as_str()),
        Some(kiro_metering_usage.as_str()),
        Some(attempt_chain.as_str()),
        credential_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|value| value.to_ascii_lowercase().contains(&q))
}

fn usage_status_value(status: UsageRecordStatus) -> &'static str {
    match status {
        UsageRecordStatus::Success => "success",
        UsageRecordStatus::Error => "error",
        UsageRecordStatus::StreamError => "stream_error",
        UsageRecordStatus::UpstreamTimeout => "upstream_timeout",
        UsageRecordStatus::ClientDropped => "client_dropped",
    }
}

fn usage_source_value(source: UsageSource) -> &'static str {
    match source {
        UsageSource::UpstreamMetadata => "upstream_metadata",
        UsageSource::LocalPromptCache => "local_prompt_cache",
        UsageSource::ContextEstimate => "context_estimate",
        UsageSource::RequestEstimate => "request_estimate",
        UsageSource::None => "none",
    }
}

fn parse_record_time(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn top_aggregates(map: HashMap<String, UsageAggregate>) -> Vec<UsageAggregate> {
    let mut values: Vec<_> = map.into_values().collect();
    values.sort_by_key(|item| {
        (
            std::cmp::Reverse((item.estimated_cost_usd * 1_000_000.0).round() as i64),
            std::cmp::Reverse(item.requests),
            std::cmp::Reverse(item.cache_read_input_tokens),
        )
    });
    values.truncate(10);
    values
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::sync::{Notify, Semaphore};

    fn record_with_time(
        id: &str,
        cache_read: i32,
        source: UsageSource,
        created_at: String,
    ) -> UsageRecord {
        UsageRecord {
            id: id.to_string(),
            created_at,
            endpoint: "/v1/messages".to_string(),
            stream: true,
            model: "claude-sonnet-4-5".to_string(),
            requested_max_tokens: None,
            downstream_stop_reason: None,
            upstream_model: None,
            external_outbound_model: None,
            model_resolution_source: None,
            model_resolution_note: None,
            conversation_id: Some("session-a".to_string()),
            request_api_key_id: Some("request-key-a".to_string()),
            credential_id: Some(1),
            credential_label: Some("test@example.com".to_string()),
            status: UsageRecordStatus::Success,
            usage_source: source,
            raw_usage: None,
            total_input_tokens: 100,
            compat_input_tokens: 50,
            billable_input_tokens: 50,
            output_tokens: 10,
            cache_read_input_tokens: cache_read,
            cache_creation_input_tokens: 5,
            cache_creation_5m_input_tokens: 5,
            cache_creation_1h_input_tokens: 0,
            estimated_cost_usd: 0.001,
            original_cost_usd: 0.001,
            kiro_metering_usage: 0.0,
            pricing_available: true,
            pricing_model: Some("claude-sonnet-4-5".to_string()),
            duration_ms: 10,
            first_token_latency_ms: None,
            response_latency_ms: Some(10),
            latency_trace: None,
            simulated: source.is_simulated(),
            sticky_bound: false,
            fallback_from_sticky: false,
            credential_attempts: Vec::new(),
            route_kind: None,
            route_subtype: None,
            fallback_reason: None,
            direct_policy_reason: None,
            local_attempted: None,
            local_preflight: None,
            external_pool_id: None,
            external_pool_name: None,
            external_attempts: Vec::new(),
            usage_projection_applied: None,
            external_pool_billing: None,
            error_type: None,
            error_message: None,
            error_detail: None,
            error_status_code: None,
            error_source: None,
            error_id: None,
            error_metadata: None,
            raw_upstream_error: None,
            public_error_status_code: None,
            public_error_type: None,
            public_error_message: None,
            payload_breakdown: None,
            payload_guard_report: None,
        }
    }

    fn record(id: &str, cache_read: i32, source: UsageSource) -> UsageRecord {
        record_with_time(id, cache_read, source, Utc::now().to_rfc3339())
    }

    #[test]
    fn sampled_request_rejection_factory_preserves_bounded_contract_for_five_rounds() {
        const CASES: [(&str, &str, http::StatusCode, u64); 5] = [
            (
                "rpm_limit",
                "request_admission",
                http::StatusCode::TOO_MANY_REQUESTS,
                1,
            ),
            (
                "concurrency_full",
                "request_admission",
                http::StatusCode::TOO_MANY_REQUESTS,
                2,
            ),
            (
                "body_too_large",
                "request_body",
                http::StatusCode::PAYLOAD_TOO_LARGE,
                4,
            ),
            (
                "provider_unavailable",
                "provider_preflight",
                http::StatusCode::SERVICE_UNAVAILABLE,
                8,
            ),
            (
                "payload_guard_rejected",
                "payload_guard",
                http::StatusCode::BAD_REQUEST,
                16,
            ),
        ];

        for (round, (reason, stage, status, observed_count)) in CASES.into_iter().enumerate() {
            let request_id = format!("sampled-rejection-{round}");
            let request_api_key_id = format!("request-key-digest-{round}");
            let usage = sampled_request_rejection_usage_record(
                &request_id,
                "/v1/messages",
                Some(request_api_key_id.clone()),
                reason,
                stage,
                status,
                observed_count,
            );

            assert_eq!(usage.id, request_id, "round {round}");
            assert!(
                DateTime::parse_from_rfc3339(&usage.created_at).is_ok(),
                "round {round}"
            );
            assert_eq!(usage.endpoint, "/v1/messages", "round {round}");
            assert!(!usage.stream, "round {round}");
            assert_eq!(usage.model, "unknown", "round {round}");
            assert_eq!(
                usage.request_api_key_id.as_deref(),
                Some(request_api_key_id.as_str()),
                "round {round}"
            );
            assert_eq!(usage.status, UsageRecordStatus::Error, "round {round}");
            assert_eq!(usage.usage_source, UsageSource::None, "round {round}");
            assert_eq!(usage.total_input_tokens, 0, "round {round}");
            assert_eq!(usage.compat_input_tokens, 0, "round {round}");
            assert_eq!(usage.billable_input_tokens, 0, "round {round}");
            assert_eq!(usage.output_tokens, 0, "round {round}");
            assert_eq!(usage.cache_read_input_tokens, 0, "round {round}");
            assert_eq!(usage.cache_creation_input_tokens, 0, "round {round}");
            assert_eq!(usage.cache_creation_5m_input_tokens, 0, "round {round}");
            assert_eq!(usage.cache_creation_1h_input_tokens, 0, "round {round}");
            assert_eq!(usage.estimated_cost_usd, 0.0, "round {round}");
            assert_eq!(usage.original_cost_usd, 0.0, "round {round}");
            assert_eq!(usage.kiro_metering_usage, 0.0, "round {round}");
            assert!(!usage.pricing_available, "round {round}");
            assert_eq!(
                usage.error_type.as_deref(),
                Some(REQUEST_REJECTION_ERROR_TYPE),
                "round {round}"
            );
            assert_eq!(
                usage.error_source.as_deref(),
                Some(REQUEST_REJECTION_ERROR_TYPE),
                "round {round}"
            );
            assert_eq!(
                usage.error_message.as_deref(),
                Some(REQUEST_REJECTION_ERROR_MESSAGE),
                "round {round}"
            );
            assert!(usage.error_detail.is_none(), "round {round}");
            assert_eq!(
                usage.error_status_code,
                Some(status.as_u16()),
                "round {round}"
            );
            assert_eq!(usage.error_id.as_deref(), Some(request_id.as_str()));
            assert_eq!(
                usage.error_metadata,
                Some(json!({
                    "sampled": true,
                    "observedCount": observed_count,
                    "observedCountIsExact": false,
                    "stage": stage,
                    "reason": reason,
                })),
                "round {round}"
            );
            assert!(usage.public_error_message.is_none(), "round {round}");
            assert!(usage.payload_breakdown.is_none(), "round {round}");
            assert!(usage.payload_guard_report.is_none(), "round {round}");
        }
    }

    fn recorder_with_test_writer(
        capacity: usize,
        pause_first: bool,
    ) -> (
        Arc<UsageRecorder>,
        Arc<Mutex<Vec<String>>>,
        Arc<Notify>,
        Arc<Semaphore>,
    ) {
        let progress = Arc::new(UsageWriterProgress::default());
        let worker_progress = progress.clone();
        let persisted = Arc::new(Mutex::new(Vec::new()));
        let worker_persisted = persisted.clone();
        let started = Arc::new(Notify::new());
        let worker_started = started.clone();
        let gate = Arc::new(Semaphore::new(0));
        let worker_gate = gate.clone();
        let (sender, mut receiver) = mpsc::channel::<UsageRecord>(capacity.max(1));
        let task = tokio::spawn(async move {
            let mut first = true;
            while let Some(record) = receiver.recv().await {
                if first && pause_first {
                    worker_started.notify_one();
                    let _permit = worker_gate.acquire().await.expect("test gate open");
                }
                first = false;
                worker_persisted.lock().push(record.id);
                worker_progress.mark_finished(1);
            }
        });
        let recorder = Arc::new(UsageRecorder {
            records: Mutex::new(VecDeque::with_capacity(16)),
            limit: 16,
            postgres_store: None,
            redis_store: None,
            writer: Some(Arc::new(UsageWriterControl::new(sender, task, progress))),
            redis_writer: None,
            dashboard_query_gate: Arc::new(Semaphore::new(USAGE_DASHBOARD_MAX_CONCURRENT_QUERIES)),
            postgres_summary_cache: Mutex::new(None),
            postgres_dashboard_cache: Mutex::new(None),
            lifecycle: Arc::new(RwLock::new(())),
            accepting: Arc::new(AtomicBool::new(true)),
            shutdown: Arc::new(UsageShutdownState::default()),
            rejected_after_shutdown: AtomicU64::new(0),
            cleanup_watermark_micros: AtomicI64::new(0),
            rejected_by_cleanup_watermark: AtomicU64::new(0),
            backpressured_persist_records: AtomicU64::new(0),
            backpressured_redis_records: AtomicU64::new(0),
            dropped_persist_records: Arc::new(AtomicU64::new(0)),
            dropped_redis_records: Arc::new(AtomicU64::new(0)),
        });
        (recorder, persisted, started, gate)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn synchronous_store_bridge_works_inside_current_thread_runtime() {
        let value = block_on_usage_store(async {
            tokio::time::sleep(StdDuration::from_millis(1)).await;
            Ok::<_, anyhow::Error>(42)
        })
        .unwrap();

        assert_eq!(value, 42);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn usage_store_bridge_does_not_drive_future_on_current_tokio_worker() {
        let caller_thread = std::thread::current().id();
        let future_thread = block_on_usage_runtime(async move { std::thread::current().id() })
            .expect("usage runtime bridge should run");

        assert_ne!(
            future_thread, caller_thread,
            "usage/dashboard storage futures must not be driven on the HTTP Tokio worker"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn dashboard_query_gate_bounds_non_core_usage_queries() {
        let recorder = Arc::new(UsageRecorder::new(16));
        let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let peak = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let mut holders = Vec::new();
        for index in 0..USAGE_DASHBOARD_MAX_CONCURRENT_QUERIES {
            let recorder = recorder.clone();
            let active = active.clone();
            let peak = peak.clone();
            holders.push(tokio::task::spawn_blocking(move || {
                recorder
                    .dashboard_query("test dashboard query", 1, async move {
                        let now_active = active.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now_active, Ordering::SeqCst);
                        tokio::time::sleep(StdDuration::from_millis(250)).await;
                        active.fetch_sub(1, Ordering::SeqCst);
                        Ok::<_, anyhow::Error>(index)
                    })
                    .unwrap()
            }));
        }

        let started = Instant::now();
        while active.load(Ordering::SeqCst) < USAGE_DASHBOARD_MAX_CONCURRENT_QUERIES {
            assert!(
                started.elapsed() < StdDuration::from_secs(1),
                "dashboard holder queries did not acquire the gate"
            );
            tokio::time::sleep(StdDuration::from_millis(5)).await;
        }

        let queued_recorder = recorder.clone();
        let queued_active = active.clone();
        let queued_peak = peak.clone();
        let queued = tokio::task::spawn_blocking(move || {
            queued_recorder
                .dashboard_query("test dashboard query", 1, async move {
                    let now_active = queued_active.fetch_add(1, Ordering::SeqCst) + 1;
                    queued_peak.fetch_max(now_active, Ordering::SeqCst);
                    queued_active.fetch_sub(1, Ordering::SeqCst);
                    Ok::<_, anyhow::Error>(999usize)
                })
                .unwrap()
        });
        tokio::time::sleep(StdDuration::from_millis(50)).await;
        assert!(
            !queued.is_finished(),
            "third dashboard query should wait behind the gate instead of bypassing it"
        );

        for holder in holders {
            holder.await.unwrap();
        }
        assert_eq!(queued.await.unwrap(), 999);
        assert_eq!(
            peak.load(Ordering::SeqCst),
            USAGE_DASHBOARD_MAX_CONCURRENT_QUERIES
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dashboard_windows_uses_redis_observability_without_postgres_for_three_rounds() {
        let Some(redis_url) = crate::storage::integration_test_url("KIRO_RS_TEST_REDIS_URL") else {
            eprintln!("跳过 Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };
        let mut config = crate::model::config::Config::default();
        config.redis.url = Some(redis_url);
        config.redis.key_prefix = format!(
            "kiro_rs:test:recorder-dashboard-redis-first:{}",
            uuid::Uuid::new_v4()
        );
        let redis = Arc::new(RedisStore::connect(&config).await.unwrap());
        redis.clear_usage_summary().await.unwrap();

        for round in 0..3 {
            let mut usage = record(
                &format!("recorder-dashboard-redis-first-{round}"),
                1_000,
                UsageSource::UpstreamMetadata,
            );
            usage.total_input_tokens = 1_200;
            usage.billable_input_tokens = 1_200;
            redis.record_usage_summary(&usage).await.unwrap();

            let recorder = UsageRecorder {
                records: Mutex::new(VecDeque::with_capacity(16)),
                limit: 16,
                postgres_store: None,
                redis_store: Some(redis.clone()),
                writer: None,
                redis_writer: None,
                dashboard_query_gate: Arc::new(Semaphore::new(
                    USAGE_DASHBOARD_MAX_CONCURRENT_QUERIES,
                )),
                postgres_summary_cache: Mutex::new(None),
                postgres_dashboard_cache: Mutex::new(None),
                lifecycle: Arc::new(RwLock::new(())),
                accepting: Arc::new(AtomicBool::new(true)),
                shutdown: Arc::new(UsageShutdownState::default()),
                rejected_after_shutdown: AtomicU64::new(0),
                cleanup_watermark_micros: AtomicI64::new(0),
                rejected_by_cleanup_watermark: AtomicU64::new(0),
                backpressured_persist_records: AtomicU64::new(0),
                backpressured_redis_records: AtomicU64::new(0),
                dropped_persist_records: Arc::new(AtomicU64::new(0)),
                dropped_redis_records: Arc::new(AtomicU64::new(0)),
            };

            let windows = recorder.dashboard_windows(Some("UTC"), 500).unwrap();
            let last24h = windows
                .windows
                .iter()
                .find(|window| window.key == "last24h")
                .unwrap();
            assert_eq!(last24h.summary.total_requests, round + 1);
            assert_eq!(last24h.summary.high_cache_requests, round + 1);
            assert_eq!(
                last24h.summary.total_input_tokens,
                (round + 1) as i64 * 1_200
            );
        }

        redis.clear_usage_summary().await.unwrap();
    }

    #[tokio::test]
    async fn in_memory_usage_cleanup_watermark_rejects_replay_for_three_rounds() {
        let recorder = UsageRecorder::new(32);

        for round in 0..3 {
            let mut old = record(
                &format!("memory-cleanup-old-round-{round}"),
                0,
                UsageSource::None,
            );
            old.created_at = Utc::now().to_rfc3339();
            assert_eq!(recorder.record(old.clone()), UsageRecordOutcome::Accepted);
            tokio::time::sleep(StdDuration::from_millis(2)).await;
            let cutoff = Utc::now();
            assert_eq!(
                recorder.advance_cleanup_watermark(cutoff).await.unwrap(),
                1,
                "round {round}: the pre-cutoff memory record is removed"
            );
            assert_eq!(
                recorder.record(old),
                UsageRecordOutcome::RejectedCleanupWatermark,
                "round {round}: late memory replay is rejected"
            );

            let mut new = record(
                &format!("memory-cleanup-new-round-{round}"),
                0,
                UsageSource::None,
            );
            new.created_at = Utc::now().to_rfc3339();
            assert_eq!(recorder.record(new), UsageRecordOutcome::Accepted);
            assert_eq!(recorder.remove_memory_records_before(Utc::now()), 1);
        }

        assert_eq!(recorder.writer_stats().rejected_by_cleanup_watermark, 3);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn persistent_usage_cleanup_falls_back_to_postgres_and_survives_restart_for_three_rounds()
    {
        let Some(postgres_url) = crate::storage::integration_test_url("KIRO_RS_TEST_POSTGRES_URL")
        else {
            eprintln!("跳过 PgSQL+Redis 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let Some(redis_url) = crate::storage::integration_test_url("KIRO_RS_TEST_REDIS_URL") else {
            eprintln!("跳过 PgSQL+Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };
        let mut config = crate::model::config::Config::default();
        config.postgres.url = Some(postgres_url);
        config.postgres.max_connections = 2;
        config.redis.url = Some(redis_url);
        config.redis.key_prefix = format!("kiro_rs:test:usage-cleanup:{}", uuid::Uuid::new_v4());
        let postgres = Arc::new(
            crate::storage::postgres::PostgresStore::connect_test(&config)
                .await
                .unwrap(),
        );
        let postgres_usage = Arc::new(PostgresUsageStore::new(postgres.clone()));
        let redis = Arc::new(RedisStore::connect(&config).await.unwrap());
        redis.clear_usage_summary().await.unwrap();

        for round in 0..3 {
            let recorder = Arc::new(UsageRecorder::with_postgres_and_redis(
                64,
                postgres_usage.clone(),
                Some(redis.clone()),
            ));
            let mut old = record(
                &format!("persistent-cleanup-old-round-{round}"),
                600,
                UsageSource::UpstreamMetadata,
            );
            old.created_at = Utc::now().to_rfc3339();
            assert_eq!(recorder.record(old.clone()), UsageRecordOutcome::Accepted);
            tokio::time::sleep(StdDuration::from_millis(2)).await;
            let cutoff = Utc::now();
            tokio::time::sleep(StdDuration::from_millis(2)).await;
            let mut new = record(
                &format!("persistent-cleanup-new-round-{round}"),
                600,
                UsageSource::UpstreamMetadata,
            );
            new.created_at = Utc::now().to_rfc3339();
            assert_eq!(recorder.record(new), UsageRecordOutcome::Accepted);
            let drained = recorder.drain(StdDuration::from_secs(2)).await;
            assert!(drained.postgres.drained, "round {round}: postgres writer");
            assert!(drained.redis.drained, "round {round}: redis writer");

            let effective_cutoff = postgres_usage
                .advance_soft_delete_cleanup_watermark(cutoff)
                .await
                .unwrap();
            recorder
                .advance_cleanup_watermark(effective_cutoff)
                .await
                .unwrap();
            redis.invalidate_usage_derived_cache().await.unwrap();
            loop {
                let batch = postgres_usage
                    .soft_delete_cleanup_batch(cutoff, 10)
                    .await
                    .unwrap();
                if batch.has_remaining == Some(false) {
                    break;
                }
            }
            redis.clear_usage_summary().await.unwrap();

            assert!(
                redis.usage_summary(500).await.unwrap().is_none(),
                "round {round}: invalidated Redis summary must force PgSQL fallback"
            );
            let summary = recorder.summary(500);
            assert_eq!(summary.total_requests, 1, "round {round}");
            assert!((summary.total_estimated_cost_usd - 0.001).abs() < 1e-12);
            let dashboard = recorder.dashboard(Some("UTC"), 500).unwrap();
            let last24h = dashboard
                .windows
                .iter()
                .find(|window| window.key == "last24h")
                .unwrap();
            assert_eq!(last24h.summary.total_requests, 1, "round {round}");
            assert!((last24h.summary.total_estimated_cost_usd - 0.001).abs() < 1e-12);
            let costs = recorder.credential_cost_summary();
            let credential = costs.get(&1).expect("remaining credential cost");
            assert!((credential.estimated_cost_usd - 0.001).abs() < 1e-12);

            let mut summary_latencies = Vec::with_capacity(20);
            for _ in 0..20 {
                let started = Instant::now();
                assert_eq!(recorder.summary(500).total_requests, 1);
                summary_latencies.push(started.elapsed().as_micros() as u64);
            }
            summary_latencies.sort_unstable();
            let summary_p95 = summary_latencies[18];
            let mut dashboard_latencies = Vec::with_capacity(10);
            for _ in 0..10 {
                let started = Instant::now();
                recorder.dashboard(Some("UTC"), 500).unwrap();
                dashboard_latencies.push(started.elapsed().as_micros() as u64);
            }
            dashboard_latencies.sort_unstable();
            let dashboard_p95 = dashboard_latencies[8];
            eprintln!(
                "usage cleanup fallback round {round}: summary_p95_us={summary_p95}, dashboard_p95_us={dashboard_p95}"
            );
            assert!(
                summary_p95 < 250_000,
                "round {round}: PgSQL summary fallback p95 was {summary_p95}us"
            );
            assert!(
                dashboard_p95 < 500_000,
                "round {round}: PgSQL dashboard fallback p95 was {dashboard_p95}us"
            );
            assert!(recorder.shutdown(StdDuration::from_secs(2)).await.drained);
            drop(recorder);

            let restarted = UsageRecorder::with_postgres_and_redis(
                64,
                postgres_usage.clone(),
                Some(redis.clone()),
            );
            assert_eq!(
                restarted.record(old),
                UsageRecordOutcome::RejectedCleanupWatermark,
                "round {round}: restart reloads the persistent watermark"
            );
            assert_eq!(restarted.summary(500).total_requests, 1, "round {round}");
            assert!(restarted.shutdown(StdDuration::from_secs(2)).await.drained);
        }

        postgres.drop_test_schema().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn production_postgres_only_usage_never_materializes_redis_for_five_rounds() {
        let Some(postgres_url) = crate::storage::integration_test_url("KIRO_RS_TEST_POSTGRES_URL")
        else {
            eprintln!("跳过 PgSQL+Redis 集成测试：未设置 KIRO_RS_TEST_POSTGRES_URL");
            return;
        };
        let Some(redis_url) = crate::storage::integration_test_url("KIRO_RS_TEST_REDIS_URL") else {
            eprintln!("跳过 PgSQL+Redis 集成测试：未设置 KIRO_RS_TEST_REDIS_URL");
            return;
        };
        let mut config = crate::model::config::Config::default();
        config.postgres.url = Some(postgres_url);
        config.postgres.max_connections = 2;
        config.redis.url = Some(redis_url);
        config.redis.key_prefix =
            format!("kiro_rs:test:usage-postgres-only:{}", uuid::Uuid::new_v4());
        let postgres = Arc::new(
            crate::storage::postgres::PostgresStore::connect_test(&config)
                .await
                .unwrap(),
        );
        let postgres_usage = Arc::new(PostgresUsageStore::new(postgres.clone()));
        let redis = RedisStore::connect(&config).await.unwrap();
        assert_eq!(redis.clear_usage_summary().await.unwrap(), 0);

        let recorder = Arc::new(UsageRecorder::with_postgres(1024, postgres_usage));
        for round in 0..5 {
            for index in 0..128 {
                assert_eq!(
                    recorder.record(record(
                        &format!("postgres-only-round-{round}-record-{index}"),
                        600,
                        UsageSource::UpstreamMetadata,
                    )),
                    UsageRecordOutcome::Accepted,
                );
            }

            let drained = recorder.drain(StdDuration::from_secs(5)).await;
            assert!(drained.postgres.drained, "round {round}: PostgreSQL writer");
            assert_eq!(drained.redis, UsageWriterDrainStatus::default());
            let stats = recorder.writer_stats();
            assert!(!stats.redis_enabled, "round {round}");
            assert!(!stats.redis_queue_enabled, "round {round}");
            assert_eq!(stats.redis_writer_accepted, 0, "round {round}");
            assert_eq!(stats.redis_writer_finished, 0, "round {round}");
            assert_eq!(stats.dropped_redis_records, 0, "round {round}");
            assert_eq!(
                recorder.summary(500).total_requests,
                (round + 1) * 128,
                "round {round}: PostgreSQL is the usage authority",
            );
            assert_eq!(
                redis.clear_usage_summary().await.unwrap(),
                0,
                "round {round}: production usage must not create Redis usage keys",
            );
        }

        let shutdown = recorder.shutdown(StdDuration::from_secs(5)).await;
        assert!(shutdown.drained);
        assert!(!shutdown.stats.redis_enabled);
        assert_eq!(redis.clear_usage_summary().await.unwrap(), 0);
        postgres.drop_test_schema().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn writer_drops_when_full_and_shutdown_drains_accepted_records() {
        let (recorder, persisted, started, gate) = recorder_with_test_writer(1, true);
        assert_eq!(
            recorder.record(record("queued-1", 0, UsageSource::None)),
            UsageRecordOutcome::Accepted
        );
        started.notified().await;
        assert_eq!(
            recorder.record(record("queued-2", 0, UsageSource::None)),
            UsageRecordOutcome::Accepted
        );

        let third_recorder = recorder.clone();
        let started_at = Instant::now();
        assert_eq!(
            third_recorder.record(record("queued-3", 0, UsageSource::None)),
            UsageRecordOutcome::Accepted
        );
        assert!(
            started_at.elapsed() < StdDuration::from_millis(50),
            "full usage writer should not block the request path"
        );

        gate.add_permits(1);
        let report = recorder.shutdown(StdDuration::from_secs(1)).await;
        assert!(report.drained);
        assert!(!report.timed_out);
        assert_eq!(report.stats.writer_accepted, 2);
        assert_eq!(report.stats.writer_finished, 2);
        assert_eq!(report.stats.backpressured_persist_records, 1);
        assert_eq!(report.stats.dropped_persist_records, 1);
        assert_eq!(persisted.lock().as_slice(), &["queued-1", "queued-2"]);

        assert_eq!(
            recorder.record(record("late", 0, UsageSource::None)),
            UsageRecordOutcome::RejectedShuttingDown
        );
        assert_eq!(recorder.writer_stats().rejected_after_shutdown, 1);
        assert!(
            recorder
                .query(UsageRecordQuery::default())
                .records
                .iter()
                .all(|record| record.id != "late")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drain_reports_timeout_without_closing_then_shutdown_finishes() {
        let (recorder, persisted, started, gate) = recorder_with_test_writer(1, true);
        recorder.record(record("slow", 0, UsageSource::None));
        started.notified().await;

        let drain = recorder.drain(StdDuration::from_millis(20)).await;
        assert!(drain.timed_out);
        assert!(!drain.postgres.drained);
        assert_eq!(drain.postgres.target, 1);
        assert!(recorder.writer_stats().accepting);

        gate.add_permits(1);
        let shutdown = recorder.shutdown(StdDuration::from_secs(1)).await;
        assert!(shutdown.drained);
        assert_eq!(persisted.lock().as_slice(), &["slow"]);

        let repeated = recorder.shutdown(StdDuration::from_secs(1)).await;
        assert!(repeated.already_started);
        assert!(repeated.drained);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn saturated_writer_drops_without_synchronous_fallback() {
        let (recorder, persisted, started, gate) = recorder_with_test_writer(1, true);
        recorder.record(record("busy", 0, UsageSource::None));
        started.notified().await;
        recorder.record(record("queued", 0, UsageSource::None));

        let started_at = Instant::now();
        assert_eq!(
            recorder.record(record("fallback", 0, UsageSource::None)),
            UsageRecordOutcome::Accepted
        );
        let elapsed = started_at.elapsed();
        assert!(
            elapsed < StdDuration::from_millis(50),
            "saturated usage writer should drop instead of synchronously persisting"
        );
        assert_eq!(recorder.writer_stats().writer_accepted, 2);
        assert_eq!(recorder.writer_stats().backpressured_persist_records, 1);
        assert_eq!(recorder.writer_stats().dropped_persist_records, 1);

        gate.add_permits(1);
        assert!(recorder.shutdown(StdDuration::from_secs(1)).await.drained);
        assert_eq!(persisted.lock().as_slice(), &["busy", "queued"]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_shutdown_owner_does_not_strand_usage_recorder() {
        let (recorder, persisted, started, gate) = recorder_with_test_writer(1, true);
        recorder.record(record("slow", 0, UsageSource::None));
        started.notified().await;

        let shutdown_recorder = recorder.clone();
        let owner =
            tokio::spawn(
                async move { shutdown_recorder.shutdown(StdDuration::from_secs(1)).await },
            );
        tokio::time::timeout(StdDuration::from_millis(200), async {
            while recorder.writer_stats().accepting {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("shutdown should close admission promptly");
        owner.abort();
        let _ = owner.await;

        gate.add_permits(1);
        let report = recorder.shutdown(StdDuration::from_secs(1)).await;
        assert!(report.already_started);
        assert!(report.drained);
        assert!(!report.timed_out);
        assert_eq!(persisted.lock().as_slice(), &["slow"]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_timeout_aborts_writer_and_publishes_final_report() {
        let (recorder, persisted, started, _gate) = recorder_with_test_writer(1, true);
        recorder.record(record("stuck", 0, UsageSource::None));
        started.notified().await;

        let first = recorder.shutdown(StdDuration::from_millis(20)).await;
        assert!(first.timed_out);
        let repeated = recorder.shutdown(StdDuration::from_secs(1)).await;
        assert!(repeated.already_started);
        assert!(repeated.timed_out);
        assert!(!repeated.drained);
        assert_eq!(repeated.postgres_abandoned, 1);
        assert!(persisted.lock().is_empty());
    }

    #[tokio::test]
    async fn bounded_usage_batch_uses_one_shared_deadline_without_waiter_fanout() {
        use std::sync::atomic::AtomicUsize;

        for round in 1..=5 {
            let active = Arc::new(AtomicUsize::new(0));
            let max_active = Arc::new(AtomicUsize::new(0));
            let started_at = Instant::now();
            let results = run_bounded_usage_batch(
                (0..8).collect::<Vec<_>>(),
                StdDuration::from_millis(500),
                |_| {
                    let active = Arc::clone(&active);
                    let max_active = Arc::clone(&max_active);
                    async move {
                        let now_active = active.fetch_add(1, Ordering::SeqCst) + 1;
                        max_active.fetch_max(now_active, Ordering::SeqCst);
                        tokio::time::sleep(StdDuration::from_millis(20)).await;
                        active.fetch_sub(1, Ordering::SeqCst);
                        Ok(())
                    }
                },
            )
            .await;
            assert!(results.iter().all(Result::is_ok), "round {round}");
            assert_eq!(max_active.load(Ordering::SeqCst), 1, "round {round}");
            assert!(started_at.elapsed() < StdDuration::from_millis(500));

            let started_at = Instant::now();
            let results =
                run_bounded_usage_batch(vec![1, 2, 3], StdDuration::from_millis(20), |_| {
                    std::future::pending::<anyhow::Result<()>>()
                })
                .await;
            assert!(results.iter().all(Result::is_err), "round {round}");
            assert!(started_at.elapsed() < StdDuration::from_millis(200));
        }
    }

    #[test]
    fn credential_cost_summary_includes_kiro_metering_usage() {
        let recorder = UsageRecorder::new(10);
        let mut first = record("metering-1", 0, UsageSource::ContextEstimate);
        first.credential_id = Some(7);
        first.kiro_metering_usage = 0.125;
        let mut second = record("metering-2", 0, UsageSource::UpstreamMetadata);
        second.credential_id = Some(7);
        second.kiro_metering_usage = 0.375;

        recorder.record(first);
        recorder.record(second);

        let all = recorder.credential_cost_summary();
        let summary = all.get(&7).expect("credential summary");
        assert!((summary.kiro_metering_usage - 0.5).abs() < f64::EPSILON);

        let by_id = recorder.credential_cost_summary_for_ids(&[7]);
        let summary = by_id.get(&7).expect("credential summary by id");
        assert!((summary.kiro_metering_usage - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn usage_record_deserializes_historical_json_without_error_diagnostics() {
        let mut value = serde_json::to_value(record("historical", 0, UsageSource::None)).unwrap();
        let object = value.as_object_mut().unwrap();
        object.remove("requestedMaxTokens");
        object.remove("downstreamStopReason");
        object.remove("errorStatusCode");
        object.remove("errorSource");
        object.remove("errorId");
        object.remove("errorMetadata");
        object.remove("publicErrorStatusCode");
        object.remove("publicErrorType");
        object.remove("publicErrorMessage");

        let decoded: UsageRecord = serde_json::from_value(value).unwrap();

        assert_eq!(decoded.requested_max_tokens, None);
        assert_eq!(decoded.downstream_stop_reason, None);
        assert_eq!(decoded.error_status_code, None);
        assert_eq!(decoded.error_source, None);
        assert_eq!(decoded.error_id, None);
        assert_eq!(decoded.error_metadata, None);
        assert_eq!(decoded.public_error_status_code, None);
        assert_eq!(decoded.public_error_type, None);
        assert_eq!(decoded.public_error_message, None);
    }

    #[test]
    fn recorder_removes_duplicate_error_metadata_fields_recursively() {
        let recorder = UsageRecorder::new(10);
        let mut usage = record("diagnostic-duplicates", 0, UsageSource::None);
        usage.status = UsageRecordStatus::Error;
        usage.error_id = Some("req_01canonical".to_string());
        usage.error_status_code = Some(429);
        usage.error_source = Some("local_account".to_string());
        usage.error_metadata = Some(json!({
            "errorId": "req_01duplicate",
            "requestId": "req_duplicate",
            "statusCode": 429,
            "responseErrorType": "rate_limit_error",
            "selectionFailure": {
                "requestId": "req_duplicate_nested",
                "primaryReason": "rpm_limited",
                "sampledAccounts": [
                    {
                        "accountId": 7,
                        "statusCode": 429,
                        "reason": "rpm_limited"
                    }
                ]
            }
        }));

        recorder.record(usage);

        let records = recorder.query(UsageRecordQuery::default()).records;
        let metadata = records[0].error_metadata.as_ref().unwrap();
        assert!(metadata.get("errorId").is_none());
        assert!(metadata.get("requestId").is_none());
        assert!(metadata.get("statusCode").is_none());
        assert_eq!(metadata["responseErrorType"], "rate_limit_error");
        assert_eq!(
            metadata
                .pointer("/selectionFailure/primaryReason")
                .and_then(Value::as_str),
            Some("rpm_limited")
        );
        assert!(metadata.pointer("/selectionFailure/requestId").is_none());
        assert!(
            metadata
                .pointer("/selectionFailure/sampledAccounts/0/statusCode")
                .is_none()
        );
        assert_eq!(
            metadata
                .pointer("/selectionFailure/sampledAccounts/0/accountId")
                .and_then(Value::as_u64),
            Some(7)
        );
    }

    #[test]
    fn recorder_bounds_error_text_and_metadata_size() {
        let recorder = UsageRecorder::new(10);
        let mut usage = record("diagnostic-bounds", 0, UsageSource::None);
        usage.status = UsageRecordStatus::Error;
        usage.error_message = Some("x".repeat(ERROR_DIAGNOSTIC_MAX_TEXT_BYTES + 128));
        usage.error_detail = Some("y".repeat(ERROR_DIAGNOSTIC_MAX_TEXT_BYTES + 128));
        usage.public_error_message = Some("p".repeat(ERROR_DIAGNOSTIC_MAX_TEXT_BYTES + 128));
        usage.error_metadata = Some(json!({
            "large": "z".repeat(ERROR_DIAGNOSTIC_MAX_METADATA_BYTES + 1024),
            "items": (0..64).map(|idx| json!({
                "idx": idx,
                "message": format!("duplicate-message-{idx}")
            })).collect::<Vec<_>>()
        }));

        recorder.record(usage);

        let records = recorder.query(UsageRecordQuery::default()).records;
        let record = &records[0];
        assert!(
            record.error_message.as_ref().unwrap().len()
                <= ERROR_DIAGNOSTIC_MAX_TEXT_BYTES + "...".len()
        );
        assert!(
            record.error_detail.as_ref().unwrap().len()
                <= ERROR_DIAGNOSTIC_MAX_TEXT_BYTES + "...".len()
        );
        assert!(
            record.public_error_message.as_ref().unwrap().len()
                <= ERROR_DIAGNOSTIC_MAX_TEXT_BYTES + "...".len()
        );
        let metadata = record.error_metadata.as_ref().unwrap();
        let metadata_len = serde_json::to_vec(metadata).unwrap().len();
        assert!(metadata_len <= ERROR_DIAGNOSTIC_MAX_METADATA_BYTES);
        assert_eq!(metadata["messageTruncated"], true);
        assert_eq!(metadata["detailTruncated"], true);
        assert_eq!(metadata["publicMessageTruncated"], true);
        assert_eq!(metadata["metadataTruncated"], true);
        assert!(metadata.get("originalMetadataBytes").is_some());
        assert!(
            metadata
                .get("items")
                .and_then(Value::as_array)
                .is_none_or(|items| items.len() <= ERROR_DIAGNOSTIC_MAX_ARRAY_ITEMS)
        );
    }

    #[test]
    fn recorder_respects_limit_and_filters() {
        let recorder = UsageRecorder::new(2);
        recorder.record(record("1", 10, UsageSource::UpstreamMetadata));
        recorder.record(record("2", 20, UsageSource::LocalPromptCache));
        let mut slow_record = record("3", 30, UsageSource::LocalPromptCache);
        slow_record.first_token_latency_ms = Some(12_000);
        recorder.record(slow_record);

        let all = recorder.query(UsageRecordQuery::default());
        assert_eq!(all.total, 2);
        assert_eq!(all.records[0].id, "3");

        let query = UsageRecordQuery {
            source: Some(UsageSource::LocalPromptCache),
            min_cache_read: Some(25),
            ..Default::default()
        };
        let filtered = recorder.query(query);
        assert_eq!(filtered.total, 1);
        assert_eq!(filtered.records[0].id, "3");

        let slow = recorder.query(UsageRecordQuery {
            min_first_token_latency_ms: Some(10_000),
            ..Default::default()
        });
        assert_eq!(slow.total, 1);
        assert_eq!(slow.records[0].id, "3");
    }

    #[test]
    fn recorder_query_page_paginates_filtered_records() {
        let recorder = UsageRecorder::new(10);
        recorder.record(record("1", 10, UsageSource::LocalPromptCache));
        recorder.record(record("2", 20, UsageSource::LocalPromptCache));
        recorder.record(record("3", 30, UsageSource::LocalPromptCache));
        recorder.record(record("4", 40, UsageSource::UpstreamMetadata));

        let first_page = recorder.query_page(
            UsageRecordQuery {
                source: Some(UsageSource::LocalPromptCache),
                ..Default::default()
            },
            1,
            2,
        );

        assert_eq!(first_page.page, 1);
        assert_eq!(first_page.limit, 2);
        assert!(first_page.has_next);
        assert_eq!(first_page.records.len(), 2);
        assert_eq!(first_page.records[0].id, "3");
        assert_eq!(first_page.records[1].id, "2");

        let second_page = recorder.query_page(
            UsageRecordQuery {
                source: Some(UsageSource::LocalPromptCache),
                ..Default::default()
            },
            2,
            2,
        );

        assert_eq!(second_page.page, 2);
        assert_eq!(second_page.limit, 2);
        assert!(!second_page.has_next);
        assert_eq!(second_page.records.len(), 1);
        assert_eq!(second_page.records[0].id, "1");
    }

    #[test]
    fn recorder_query_supports_exact_request_id_and_model_aliases() {
        let recorder = UsageRecorder::new(10);
        let mut first = record("req_01alpha", 10, UsageSource::UpstreamMetadata);
        first.model = "claude-sonnet-4".to_string();
        first.upstream_model = Some("claude-sonnet-4-20250514".to_string());
        first.request_api_key_id = Some("request-key-alpha".to_string());
        recorder.record(first);

        let mut second = record("req_01beta", 20, UsageSource::UpstreamMetadata);
        second.model = "claude-sonnet-4.5".to_string();
        second.external_outbound_model = Some("claude-sonnet-4-5".to_string());
        second.request_api_key_id = Some("request-key-beta".to_string());
        recorder.record(second);

        let by_request_id = recorder.query(UsageRecordQuery {
            request_id: Some("req_01alpha".to_string()),
            ..Default::default()
        });
        assert_eq!(by_request_id.total, 1);
        assert_eq!(by_request_id.records[0].id, "req_01alpha");

        let by_request_api_key = recorder.query(UsageRecordQuery {
            request_api_key_id: Some("request-key-beta".to_string()),
            ..Default::default()
        });
        assert_eq!(by_request_api_key.total, 1);
        assert_eq!(by_request_api_key.records[0].id, "req_01beta");

        let by_request_api_key_search = recorder.query(UsageRecordQuery {
            q: Some("request-key-alpha".to_string()),
            ..Default::default()
        });
        assert_eq!(by_request_api_key_search.total, 1);
        assert_eq!(by_request_api_key_search.records[0].id, "req_01alpha");

        let by_upstream_model = recorder.query(UsageRecordQuery {
            model: Some("claude-sonnet-4-20250514".to_string()),
            ..Default::default()
        });
        assert_eq!(by_upstream_model.total, 1);
        assert_eq!(by_upstream_model.records[0].id, "req_01alpha");

        let by_external_outbound_model = recorder.query(UsageRecordQuery {
            model: Some("claude-sonnet-4-5".to_string()),
            ..Default::default()
        });
        assert_eq!(by_external_outbound_model.total, 1);
        assert_eq!(by_external_outbound_model.records[0].id, "req_01beta");
    }

    #[test]
    fn recorder_query_page_defaults_to_twenty_and_uses_has_next() {
        let recorder = UsageRecorder::new(25);
        for index in 1..=21 {
            recorder.record(record(
                &index.to_string(),
                index,
                UsageSource::LocalPromptCache,
            ));
        }

        let first_page = recorder.query_page(UsageRecordQuery::default(), 1, 0);

        assert_eq!(first_page.page, 1);
        assert_eq!(first_page.limit, 20);
        assert!(first_page.has_next);
        assert_eq!(first_page.records.len(), 20);
        assert_eq!(first_page.records[0].id, "21");
        assert_eq!(first_page.records[19].id, "2");

        let second_page = recorder.query_page(UsageRecordQuery::default(), 2, 0);

        assert_eq!(second_page.limit, 20);
        assert!(!second_page.has_next);
        assert_eq!(second_page.records.len(), 1);
        assert_eq!(second_page.records[0].id, "1");
    }

    #[test]
    fn recorder_search_matches_model_account_session_and_error_text() {
        let recorder = UsageRecorder::new(10);
        let mut first = record("1", 10, UsageSource::LocalPromptCache);
        first.model = "claude-sonnet-4-5".to_string();
        first.credential_label = Some("alpha@example.com".to_string());
        first.conversation_id = Some("session-alpha".to_string());
        recorder.record(first);

        let mut second = record("2", 20, UsageSource::UpstreamMetadata);
        second.model = "claude-opus-4-5".to_string();
        second.credential_id = Some(42);
        second.credential_label = Some("beta@example.com".to_string());
        second.conversation_id = Some("session-beta".to_string());
        second.error_message = Some("upstream quota exceeded".to_string());
        recorder.record(second);

        let by_model = recorder.query(UsageRecordQuery {
            q: Some("opus".to_string()),
            ..Default::default()
        });
        assert_eq!(by_model.total, 1);
        assert_eq!(by_model.records[0].id, "2");

        let by_account = recorder.query(UsageRecordQuery {
            q: Some("alpha@example".to_string()),
            ..Default::default()
        });
        assert_eq!(by_account.total, 1);
        assert_eq!(by_account.records[0].id, "1");

        let by_credential_id = recorder.query(UsageRecordQuery {
            q: Some("beta@example".to_string()),
            ..Default::default()
        });
        assert_eq!(by_credential_id.total, 1);
        assert_eq!(by_credential_id.records[0].id, "2");

        let by_error = recorder.query(UsageRecordQuery {
            q: Some("quota".to_string()),
            ..Default::default()
        });
        assert_eq!(by_error.total, 1);
        assert_eq!(by_error.records[0].id, "2");

        let mut chained = record("3", 30, UsageSource::LocalPromptCache);
        chained.credential_attempts = vec![
            KiroCredentialAttempt::new(
                0,
                6,
                Some("first@example.com".to_string()),
                Some(reqwest::StatusCode::TOO_MANY_REQUESTS),
                "transient_retry",
                Some("transient_error"),
                Some("429 Too Many Requests".to_string()),
                10,
            ),
            KiroCredentialAttempt::new(
                1,
                9,
                Some("second@example.com".to_string()),
                Some(reqwest::StatusCode::OK),
                "success",
                None::<&str>,
                None::<String>,
                20,
            ),
        ];
        recorder.record(chained);

        let by_chain = recorder.query(UsageRecordQuery {
            q: Some("#6(429)>#9(200)".to_string()),
            ..Default::default()
        });
        assert_eq!(by_chain.total, 1);
        assert_eq!(by_chain.records[0].id, "3");
    }

    #[test]
    fn summary_counts_high_cache_and_sources() {
        let recorder = UsageRecorder::new(10);
        recorder.record(record("1", 5, UsageSource::UpstreamMetadata));
        let mut second = record("2", 20_000, UsageSource::LocalPromptCache);
        second.original_cost_usd = 0.009;
        recorder.record(second);

        let summary = recorder.summary(10_000);
        assert_eq!(summary.total_requests, 2);
        assert_eq!(summary.success_requests, 2);
        assert_eq!(summary.high_cache_requests, 1);
        assert_eq!(summary.simulated_requests, 1);
        assert_eq!(summary.upstream_metadata_requests, 1);
        assert_eq!(summary.local_prompt_cache_requests, 1);
        assert_eq!(summary.local_prompt_cache_input_tokens, 100);
        assert_eq!(summary.local_prompt_cache_read_input_tokens, 20_000);
        assert_eq!(summary.local_prompt_cache_creation_input_tokens, 5);
        assert_eq!(summary.realtime.window_seconds, REALTIME_USAGE_WINDOW_SECS);
        assert_eq!(summary.realtime.requests, 2);
        assert_eq!(summary.realtime.success_requests, 2);
        assert_eq!(summary.realtime.error_requests, 0);
        assert_eq!(summary.realtime.rpm, 2.0);
        assert_eq!(summary.realtime.success_rpm, 2.0);
        assert_eq!(summary.realtime.error_rpm, 0.0);
        assert_eq!(summary.realtime.total_tpm, 220.0);
        assert_eq!(summary.realtime.billable_tpm, 120.0);
        assert!((summary.total_original_cost_usd - 0.010).abs() < f64::EPSILON);
        assert_eq!(summary.top_credentials[0].key, "1");
    }

    #[test]
    fn credential_cost_summary_aggregates_kiro_metering_usage() {
        let recorder = UsageRecorder::new(10);
        let mut first = record("1", 5, UsageSource::UpstreamMetadata);
        first.credential_id = Some(7);
        first.estimated_cost_usd = 0.25;
        first.original_cost_usd = 0.50;
        first.kiro_metering_usage = 1.5;
        first.pricing_available = true;
        recorder.record(first);

        let mut second = record("2", 5, UsageSource::UpstreamMetadata);
        second.credential_id = Some(7);
        second.estimated_cost_usd = 0.75;
        second.original_cost_usd = 1.25;
        second.kiro_metering_usage = 2.25;
        second.pricing_available = false;
        recorder.record(second);

        let summary = recorder.credential_cost_summary_for_ids(&[7]);
        let credential = summary.get(&7).expect("credential summary");

        assert_eq!(credential.estimated_cost_usd, 1.0);
        assert_eq!(credential.original_cost_usd, 1.75);
        assert_eq!(credential.kiro_metering_usage, 3.75);
        assert_eq!(credential.priced_requests, 1);
        assert_eq!(credential.unpriced_requests, 1);
    }

    #[test]
    fn recorder_filters_by_time_window_and_invalid_times_do_not_match() {
        let recorder = UsageRecorder::new(10);
        recorder.record(record_with_time(
            "old",
            10,
            UsageSource::UpstreamMetadata,
            "2026-01-01T00:00:00Z".to_string(),
        ));
        recorder.record(record_with_time(
            "new",
            20,
            UsageSource::LocalPromptCache,
            "2026-01-02T00:00:00Z".to_string(),
        ));
        recorder.record(record_with_time(
            "bad-time",
            30,
            UsageSource::RequestEstimate,
            "not-a-time".to_string(),
        ));

        let since = DateTime::parse_from_rfc3339("2026-01-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let filtered = recorder.query(UsageRecordQuery {
            since: Some(since),
            ..Default::default()
        });

        assert_eq!(filtered.total, 1);
        assert_eq!(filtered.records[0].id, "new");
    }
}
