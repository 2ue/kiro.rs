use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration as StdDuration, Instant};

use chrono::{
    DateTime, Datelike, Duration as ChronoDuration, FixedOffset, NaiveDate, TimeZone, Timelike, Utc,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::mpsc;

use crate::kiro::call_trace::{KiroCredentialAttempt, summarize_attempts};
use crate::storage::postgres::PostgresUsageStore;
use crate::storage::redis_cache::RedisStore;

const DEFAULT_QUERY_LIMIT: usize = 100;
const DEFAULT_PAGE_QUERY_LIMIT: usize = 20;
const MAX_QUERY_LIMIT: usize = 1000;
const USAGE_WRITER_QUEUE_CAPACITY: usize = 4096;
const USAGE_WRITER_MAX_ATTEMPTS: u32 = 3;
const USAGE_WRITER_BATCH_MAX: usize = 64;
const USAGE_DASHBOARD_REDIS_TIMEOUT_SECS: u64 = 2;
const USAGE_DASHBOARD_POSTGRES_TIMEOUT_SECS: u64 = 5;
const ERROR_DIAGNOSTIC_MAX_TEXT_BYTES: usize = 2048;
const ERROR_DIAGNOSTIC_MAX_METADATA_BYTES: usize = 8192;
const ERROR_DIAGNOSTIC_MAX_STRING_BYTES: usize = 512;
const ERROR_DIAGNOSTIC_MAX_ARRAY_ITEMS: usize = 20;
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
    pub status: Option<u16>,
    pub action: String,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLatencyTrace {
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
    pub client_dropped_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_reason: Option<StreamTerminalReason>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum StreamTerminalReason {
    Completed,
    UpstreamStatusError,
    UpstreamJsonException,
    UpstreamIdleTimeout,
    MalformedSse,
    ClientDropped,
    InternalError,
}

impl UsageLatencyTrace {
    pub fn is_empty(&self) -> bool {
        self.payload_guard_ms.is_none()
            && self.upstream_header_ms.is_none()
            && self.first_upstream_chunk_ms.is_none()
            && self.first_output_delta_ms.is_none()
            && self.first_thinking_delta_ms.is_none()
            && self.first_visible_text_delta_ms.is_none()
            && self.stream_gap_to_first_output_ms.is_none()
            && self.chunks_before_first_output.is_none()
            && self.events_before_first_output.is_none()
            && self.client_dropped_ms.is_none()
            && self.terminal_reason.is_none()
    }
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
    pub upstream_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_resolution_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_resolution_note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
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
    pub payload_breakdown: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_guard_report: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct UsageRecordQuery {
    pub limit: usize,
    pub q: Option<String>,
    pub endpoint: Option<String>,
    pub conversation_id: Option<String>,
    pub credential_id: Option<u64>,
    pub external_pool_id: Option<u64>,
    pub model: Option<String>,
    pub status: Option<UsageRecordStatus>,
    pub source: Option<UsageSource>,
    pub stream: Option<bool>,
    pub min_cache_read: Option<i32>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
}

impl Default for UsageRecordQuery {
    fn default() -> Self {
        Self {
            limit: DEFAULT_QUERY_LIMIT,
            q: None,
            endpoint: None,
            conversation_id: None,
            credential_id: None,
            external_pool_id: None,
            model: None,
            status: None,
            source: None,
            stream: None,
            min_cache_read: None,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRealtimeStats {
    pub window_seconds: u32,
    pub requests: usize,
    pub rpm: f64,
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
            rpm: 0.0,
            input_tpm: 0.0,
            output_tpm: 0.0,
            total_tpm: 0.0,
            billable_tpm: 0.0,
        }
    }

    pub fn from_totals(
        window_seconds: u32,
        requests: usize,
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
            rpm: requests as f64 * scale,
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecorderStats {
    pub in_memory_limit: usize,
    pub in_memory_records: usize,
    pub postgres_enabled: bool,
    pub writer_queue_enabled: bool,
    pub writer_queue_capacity: usize,
    pub writer_queue_available: usize,
    pub dropped_persist_records: u64,
}

pub struct UsageRecorder {
    records: Mutex<VecDeque<UsageRecord>>,
    limit: usize,
    postgres_store: Option<Arc<PostgresUsageStore>>,
    redis_store: Option<Arc<RedisStore>>,
    writer_tx: Option<mpsc::Sender<UsageRecord>>,
    dropped_persist_records: AtomicU64,
}

enum UsageDashboardCacheRead {
    Hit(UsageDashboardResponse),
    Empty,
    Timeout,
}

fn normalize_error_diagnostics(mut record: UsageRecord) -> UsageRecord {
    if matches!(record.status, UsageRecordStatus::Success)
        && record.error_message.is_none()
        && record.error_detail.is_none()
        && record.error_metadata.is_none()
    {
        return record;
    }

    let (error_message, message_truncated) =
        truncate_error_text(record.error_message.take(), ERROR_DIAGNOSTIC_MAX_TEXT_BYTES);
    let (error_detail, detail_truncated) =
        truncate_error_text(record.error_detail.take(), ERROR_DIAGNOSTIC_MAX_TEXT_BYTES);
    record.error_message = error_message;
    record.error_detail = error_detail;
    record.error_metadata = sanitize_error_metadata(
        record.error_metadata.take(),
        message_truncated,
        detail_truncated,
        ERROR_DIAGNOSTIC_MAX_METADATA_BYTES,
    );
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
    max_bytes: usize,
) -> Option<Value> {
    if value.is_none() && !message_truncated && !detail_truncated {
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

    if message_truncated || detail_truncated || metadata_truncated {
        ensure_metadata_object_flags(
            &mut value,
            message_truncated,
            detail_truncated,
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
            writer_tx: None,
            dropped_persist_records: AtomicU64::new(0),
        }
    }

    pub fn with_postgres_and_redis(
        limit: usize,
        postgres_store: Arc<PostgresUsageStore>,
        redis_store: Option<Arc<RedisStore>>,
    ) -> Self {
        let writer_tx = if tokio::runtime::Handle::try_current().is_ok() {
            let (tx, rx) = mpsc::channel(USAGE_WRITER_QUEUE_CAPACITY);
            tokio::spawn(usage_writer_loop(postgres_store.clone(), rx));
            Some(tx)
        } else {
            tracing::warn!(
                "创建 UsageRecorder 时没有运行中的 Tokio runtime，将同步写入 PgSQL usage"
            );
            None
        };
        Self {
            records: Mutex::new(VecDeque::with_capacity(limit.max(1).min(1024))),
            limit: limit.max(1),
            postgres_store: Some(postgres_store),
            redis_store,
            writer_tx,
            dropped_persist_records: AtomicU64::new(0),
        }
    }

    pub fn record(&self, record: UsageRecord) {
        let record = normalize_error_diagnostics(record);
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

        self.record_usage_redis(record.clone());

        if let Some(tx) = &self.writer_tx {
            match tx.try_send(record.clone()) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    let dropped = self.dropped_persist_records.fetch_add(1, Ordering::Relaxed) + 1;
                    tracing::warn!(dropped, "PgSQL usage 写入队列已满，本条 usage 持久化被丢弃");
                }
                Err(mpsc::error::TrySendError::Closed(record)) => {
                    self.persist_usage_sync(record);
                }
            }
        } else {
            self.persist_usage_sync(record);
        }
    }

    fn record_usage_redis(&self, record: UsageRecord) {
        let Some(redis) = &self.redis_store else {
            return;
        };
        let redis = redis.clone();
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::spawn(async move {
                if let Err(err) = redis.record_usage_summary(&record).await {
                    tracing::warn!("写入 Redis usage summary 失败: {}", err);
                }
            });
        } else if let Err(err) =
            block_on_usage_store(async move { redis.record_usage_summary(&record).await })
        {
            tracing::warn!("写入 Redis usage summary 失败: {}", err);
        }
    }

    pub fn writer_stats(&self) -> UsageRecorderStats {
        let in_memory_records = self.records.lock().len();
        let (writer_queue_enabled, writer_queue_capacity, writer_queue_available) =
            if let Some(tx) = &self.writer_tx {
                (true, tx.max_capacity(), tx.capacity())
            } else {
                (false, 0, 0)
            };
        UsageRecorderStats {
            in_memory_limit: self.limit,
            in_memory_records,
            postgres_enabled: self.postgres_store.is_some(),
            writer_queue_enabled,
            writer_queue_capacity,
            writer_queue_available,
            dropped_persist_records: self.dropped_persist_records.load(Ordering::Relaxed),
        }
    }

    fn persist_usage_sync(&self, record: UsageRecord) {
        if let Some(store) = &self.postgres_store {
            let store = store.clone();
            if let Err(err) = block_on_usage_store(async move { store.record(record).await }) {
                tracing::warn!("写入 PgSQL usage record 失败: {}", err);
            }
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
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            let redis_query = query.clone();
            match block_on_usage_store(async move {
                redis.usage_records_page(redis_query, page, limit).await
            }) {
                Ok(Some(result)) => return result,
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!("分页查询 Redis usage records 失败，回退 PgSQL: {}", err)
                }
            }
        }

        if let Some(store) = &self.postgres_store {
            let store = store.clone();
            let query_for_fallback = query.clone();
            return block_on_usage_store(async move { store.query_page(query, page, limit).await })
                .unwrap_or_else(|err| {
                    tracing::warn!("分页查询 PgSQL usage records 失败，回退内存记录: {}", err);
                    self.query_page_memory(query_for_fallback, page, limit)
                });
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
            let store = store.clone();
            return block_on_usage_store(async move { store.summary(high_cache_threshold).await })
                .unwrap_or_else(|err| {
                    tracing::warn!("汇总 PgSQL usage records 失败，回退内存记录: {}", err);
                    self.summary_memory(high_cache_threshold)
                });
        }
        self.summary_memory(high_cache_threshold)
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
                    Ok(Ok(Some(dashboard))) => Ok(UsageDashboardCacheRead::Hit(dashboard)),
                    Ok(Ok(None)) => Ok(UsageDashboardCacheRead::Empty),
                    Ok(Err(err)) => Err(err),
                    Err(_) => Ok(UsageDashboardCacheRead::Timeout),
                }
            }) {
                Ok(UsageDashboardCacheRead::Hit(dashboard)) => return Ok(dashboard),
                Ok(UsageDashboardCacheRead::Empty) => {}
                Ok(UsageDashboardCacheRead::Timeout) => {
                    anyhow::bail!(
                        "读取 Redis usage dashboard 超过 {} 秒，已中止本次后台查询",
                        USAGE_DASHBOARD_REDIS_TIMEOUT_SECS
                    );
                }
                Err(err) => {
                    tracing::warn!("读取 Redis usage dashboard 失败，回退 PgSQL: {}", err)
                }
            }
        }

        if let Some(store) = &self.postgres_store {
            let store = store.clone();
            let timezone = timezone.map(str::to_string);
            return block_on_usage_store(async move {
                match tokio::time::timeout(
                    StdDuration::from_secs(USAGE_DASHBOARD_POSTGRES_TIMEOUT_SECS),
                    store.dashboard(timezone.as_deref(), high_cache_threshold),
                )
                .await
                {
                    Ok(result) => result,
                    Err(_) => anyhow::bail!(
                        "读取 PgSQL usage dashboard 超过 {} 秒，已中止本次后台查询",
                        USAGE_DASHBOARD_POSTGRES_TIMEOUT_SECS
                    ),
                }
            });
        }

        anyhow::bail!("usage dashboard 需要 Redis 或 PgSQL 聚合存储")
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
                });
                entry.requests += 1;
                entry.cache_read_input_tokens += record.cache_read_input_tokens as i64;
                entry.cache_creation_input_tokens += record.cache_creation_input_tokens as i64;
                entry.estimated_cost_usd += record.estimated_cost_usd;
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
                        });
                entry.requests += 1;
                entry.cache_read_input_tokens += record.cache_read_input_tokens as i64;
                entry.cache_creation_input_tokens += record.cache_creation_input_tokens as i64;
                entry.estimated_cost_usd += record.estimated_cost_usd;
            }
        }

        summary.top_credentials = top_aggregates(credentials);
        summary.top_conversations = top_aggregates(conversations);
        summary.realtime = UsageRealtimeStats::from_totals(
            REALTIME_USAGE_WINDOW_SECS,
            realtime_requests,
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
            if record.pricing_available {
                entry.priced_requests += 1;
            } else {
                entry.unpriced_requests += 1;
            }
        }
        summaries
    }

    pub fn clear(&self) {
        self.records.lock().clear();
        if let Some(redis) = &self.redis_store {
            let redis = redis.clone();
            if let Err(err) = block_on_usage_store(async move { redis.clear_usage_summary().await })
            {
                tracing::warn!("清空 Redis usage summary 失败: {}", err);
            }
        }
        if let Some(store) = &self.postgres_store {
            let store = store.clone();
            if let Err(err) = block_on_usage_store(async move { store.clear().await }) {
                tracing::warn!("清空 PgSQL usage records 失败: {}", err);
            }
        }
    }
}

async fn usage_writer_loop(store: Arc<PostgresUsageStore>, mut rx: mpsc::Receiver<UsageRecord>) {
    while let Some(first) = rx.recv().await {
        let mut records = Vec::with_capacity(USAGE_WRITER_BATCH_MAX);
        records.push(first);
        while records.len() < USAGE_WRITER_BATCH_MAX {
            match rx.try_recv() {
                Ok(record) => records.push(record),
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => break,
            }
        }
        persist_usage_batch_with_retry(&store, records).await;
    }
}

async fn persist_usage_batch_with_retry(
    store: &Arc<PostgresUsageStore>,
    records: Vec<UsageRecord>,
) {
    let first_request_id = records
        .first()
        .map(|record| record.id.as_str())
        .unwrap_or_default()
        .to_string();
    let record_count = records.len();
    let mut attempt = 1;
    loop {
        match store.record_batch(records.clone()).await {
            Ok(()) => break,
            Err(err) if attempt < USAGE_WRITER_MAX_ATTEMPTS => {
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
            Err(err) => {
                tracing::warn!(
                    request_id = %first_request_id,
                    record_count,
                    attempt,
                    "批量写入 PgSQL usage record 最终失败，已放弃本批持久化: {}",
                    err
                );
                break;
            }
        }
    }
}

fn block_on_usage_store<T>(
    future: impl std::future::Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    let started_at = Instant::now();
    let result = if let Ok(handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(|| handle.block_on(future))
    } else {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(future)
    };
    let elapsed = started_at.elapsed();
    if elapsed >= StdDuration::from_millis(100) {
        tracing::warn!(
            elapsed_ms = elapsed.as_millis() as u64,
            "同步 usage 存储操作耗时较长"
        );
    }
    result
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
    if let Some(model) = &query.model {
        if &record.model != model && record.upstream_model.as_ref() != Some(model) {
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
    let attempt_chain = summarize_attempts(&record.credential_attempts);

    [
        Some(record.id.as_str()),
        Some(record.created_at.as_str()),
        Some(record.endpoint.as_str()),
        Some(record.model.as_str()),
        record.upstream_model.as_deref(),
        record.model_resolution_source.as_deref(),
        record.model_resolution_note.as_deref(),
        record.conversation_id.as_deref(),
        external_pool_id.as_deref(),
        record.external_pool_name.as_deref(),
        record.credential_label.as_deref(),
        Some(status),
        Some(source),
        record.error_type.as_deref(),
        record.error_message.as_deref(),
        record.error_detail.as_deref(),
        record.pricing_model.as_deref(),
        Some(estimated_cost.as_str()),
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
            upstream_model: None,
            model_resolution_source: None,
            model_resolution_note: None,
            conversation_id: Some("session-a".to_string()),
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
            payload_breakdown: None,
            payload_guard_report: None,
        }
    }

    fn record(id: &str, cache_read: i32, source: UsageSource) -> UsageRecord {
        record_with_time(id, cache_read, source, Utc::now().to_rfc3339())
    }

    #[test]
    fn usage_record_deserializes_historical_json_without_error_diagnostics() {
        let mut value = serde_json::to_value(record("historical", 0, UsageSource::None)).unwrap();
        let object = value.as_object_mut().unwrap();
        object.remove("errorStatusCode");
        object.remove("errorSource");
        object.remove("errorId");
        object.remove("errorMetadata");

        let decoded: UsageRecord = serde_json::from_value(value).unwrap();

        assert_eq!(decoded.error_status_code, None);
        assert_eq!(decoded.error_source, None);
        assert_eq!(decoded.error_id, None);
        assert_eq!(decoded.error_metadata, None);
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
        let metadata = record.error_metadata.as_ref().unwrap();
        let metadata_len = serde_json::to_vec(metadata).unwrap().len();
        assert!(metadata_len <= ERROR_DIAGNOSTIC_MAX_METADATA_BYTES);
        assert_eq!(metadata["messageTruncated"], true);
        assert_eq!(metadata["detailTruncated"], true);
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
        recorder.record(record("3", 30, UsageSource::LocalPromptCache));

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
        recorder.record(record("2", 20_000, UsageSource::LocalPromptCache));

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
        assert_eq!(summary.realtime.rpm, 2.0);
        assert_eq!(summary.realtime.total_tpm, 220.0);
        assert_eq!(summary.realtime.billable_tpm, 120.0);
        assert_eq!(summary.top_credentials[0].key, "1");
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
