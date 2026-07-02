use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::{
    fs::{self, OpenOptions},
    io::AsyncWriteExt,
    sync::mpsc,
};

use crate::{
    kiro::model::requests::{
        conversation::Message,
        kiro::KiroRequest,
        tool::{ToolResult, ToolUseEntry},
    },
    model::config::ToolFormatDebugConfig,
};

use super::payload_guard::{PayloadGuardReport, ToolUseFormatDiagnostics};

const TOOL_FORMAT_DEBUG_BODY_HASH_SAMPLE_BYTES: usize = 4096;

#[derive(Debug, Clone)]
pub struct ToolFormatDebugEvent<'a> {
    pub request_id: &'a str,
    pub endpoint: &'a str,
    pub stream: bool,
    pub requested_model: &'a str,
    pub upstream_model: Option<&'a str>,
    pub error_message: &'a str,
    pub attempted_body: Option<&'a str>,
    pub request: &'a KiroRequest,
    pub report: Option<&'a PayloadGuardReport>,
    pub diagnostics: ToolUseFormatDiagnostics,
}

#[derive(Default)]
struct SamplerState {
    window_start: Option<Instant>,
    global_count: u32,
    request_body_count: u32,
    fingerprint_counts: HashMap<String, u32>,
    group_counts: HashMap<String, u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SamplingDropReason {
    FingerprintLimit,
    GroupLimit,
    GlobalLimit,
    ChannelFull,
    ChannelClosed,
    Disabled,
}

impl SamplingDropReason {
    fn as_str(self) -> &'static str {
        match self {
            SamplingDropReason::FingerprintLimit => "fingerprint_limit",
            SamplingDropReason::GroupLimit => "group_limit",
            SamplingDropReason::GlobalLimit => "global_limit",
            SamplingDropReason::ChannelFull => "channel_full",
            SamplingDropReason::ChannelClosed => "channel_closed",
            SamplingDropReason::Disabled => "disabled",
        }
    }
}

#[derive(Debug, Clone)]
struct SamplingDecision {
    sampled: bool,
    sampled_index_in_window: u32,
    fingerprint_seen_in_window: u32,
    group_seen_in_window: u32,
    global_seen_in_window: u32,
    drop_reason: Option<SamplingDropReason>,
    request_body_captured: bool,
    request_body_seen_in_window: u32,
    request_body_drop_reason: Option<&'static str>,
}

impl SamplerState {
    fn decide(
        &mut self,
        config: &ToolFormatDebugConfig,
        fingerprint: &str,
        group_key: &str,
        has_request_body: bool,
    ) -> SamplingDecision {
        let now = Instant::now();
        if self.window_start.is_none_or(|start| {
            now.duration_since(start) >= Duration::from_secs(config.window_secs)
        }) {
            self.window_start = Some(now);
            self.global_count = 0;
            self.request_body_count = 0;
            self.fingerprint_counts.clear();
            self.group_counts.clear();
        }

        let fingerprint_seen = self
            .fingerprint_counts
            .get(fingerprint)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        let group_seen = self
            .group_counts
            .get(group_key)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        let global_seen = self.global_count.saturating_add(1);

        self.fingerprint_counts
            .insert(fingerprint.to_string(), fingerprint_seen);
        self.group_counts.insert(group_key.to_string(), group_seen);
        self.global_count = global_seen;

        let drop_reason = if fingerprint_seen > config.max_records_per_fingerprint {
            Some(SamplingDropReason::FingerprintLimit)
        } else if group_seen > config.max_records_per_group {
            Some(SamplingDropReason::GroupLimit)
        } else if global_seen > config.max_records_global {
            Some(SamplingDropReason::GlobalLimit)
        } else {
            None
        };
        let mut request_body_seen = self.request_body_count;
        let mut request_body_captured = false;
        let mut request_body_drop_reason = None;
        if drop_reason.is_none() {
            if !has_request_body {
                request_body_drop_reason = Some("missing");
            } else if !config.capture_request_body {
                request_body_drop_reason = Some("disabled");
            } else if config.max_request_body_bytes == 0 {
                request_body_drop_reason = Some("max_bytes_zero");
            } else if request_body_seen >= config.max_request_body_records_per_window {
                request_body_drop_reason = Some("request_body_record_limit");
            } else {
                request_body_seen = request_body_seen.saturating_add(1);
                self.request_body_count = request_body_seen;
                request_body_captured = true;
            }
        }

        SamplingDecision {
            sampled: drop_reason.is_none(),
            sampled_index_in_window: fingerprint_seen,
            fingerprint_seen_in_window: fingerprint_seen,
            group_seen_in_window: group_seen,
            global_seen_in_window: global_seen,
            drop_reason,
            request_body_captured,
            request_body_seen_in_window: request_body_seen,
            request_body_drop_reason,
        }
    }
}

pub struct ToolFormatDebugRecorder {
    config: ToolFormatDebugConfig,
    tx: Option<mpsc::Sender<String>>,
    sampler: Mutex<SamplerState>,
    dropped_channel: AtomicU64,
    dropped_rate_limited: AtomicU64,
}

impl ToolFormatDebugRecorder {
    pub fn disabled() -> Arc<Self> {
        Arc::new(Self {
            config: ToolFormatDebugConfig {
                enabled: false,
                ..ToolFormatDebugConfig::default()
            },
            tx: None,
            sampler: Mutex::new(SamplerState::default()),
            dropped_channel: AtomicU64::new(0),
            dropped_rate_limited: AtomicU64::new(0),
        })
    }

    pub fn new(config: ToolFormatDebugConfig) -> Arc<Self> {
        let config = config.normalized();
        if !config.enabled {
            return Self::disabled();
        }

        if tokio::runtime::Handle::try_current().is_err() {
            tracing::warn!("tool format debug 已启用，但当前没有 Tokio runtime，诊断写盘关闭");
            return Self::disabled();
        }

        let (tx, rx) = mpsc::channel(config.channel_capacity);
        tokio::spawn(tool_format_debug_writer_loop(
            PathBuf::from(config.dir.clone()),
            config.roll_interval_secs,
            config.max_file_bytes,
            rx,
        ));

        Arc::new(Self {
            config,
            tx: Some(tx),
            sampler: Mutex::new(SamplerState::default()),
            dropped_channel: AtomicU64::new(0),
            dropped_rate_limited: AtomicU64::new(0),
        })
    }

    pub fn record(&self, event: ToolFormatDebugEvent<'_>) -> Option<Value> {
        if !self.config.enabled {
            return None;
        }

        let attempted_body = event.attempted_body;
        let body_identity = body_identity_for_sampling(&event);
        let mut snapshot = ToolFormatDebugSnapshot::from_event(&self.config, event, body_identity);
        let fingerprint = snapshot.sampling.fingerprint.clone();
        let group_key = snapshot.sampling.group_key.clone();
        let decision = self.sampler.lock().decide(
            &self.config,
            &fingerprint,
            &group_key,
            attempted_body.is_some(),
        );

        snapshot.sampling.sampled = decision.sampled;
        snapshot.sampling.sampled_index_in_window = decision.sampled_index_in_window;
        snapshot.sampling.fingerprint_seen_in_window = decision.fingerprint_seen_in_window;
        snapshot.sampling.group_seen_in_window = decision.group_seen_in_window;
        snapshot.sampling.global_seen_in_window = decision.global_seen_in_window;
        snapshot.sampling.drop_reason = decision.drop_reason.map(|reason| reason.as_str());
        if decision.sampled {
            if let Some(body) = attempted_body {
                snapshot.body.body_sha256 = sha256_hex(body);
            }
        }
        snapshot.request_body = ToolFormatRequestBodySnapshot::from_attempted_body(
            &self.config,
            attempted_body,
            decision.request_body_captured,
            decision.request_body_drop_reason,
            decision.request_body_seen_in_window,
            decision.sampled,
        );

        if !decision.sampled {
            self.dropped_rate_limited.fetch_add(1, Ordering::Relaxed);
            return Some(debug_ref(
                &snapshot,
                false,
                snapshot.sampling.drop_reason,
                self.dropped_rate_limited.load(Ordering::Relaxed),
                self.dropped_channel.load(Ordering::Relaxed),
                None,
            ));
        }

        let line = match serialize_snapshot_with_budget(&mut snapshot, self.config.max_record_bytes)
        {
            Ok(line) => line,
            Err(err) => {
                tracing::warn!("序列化 tool format debug 失败: {}", err);
                return Some(debug_ref(
                    &snapshot,
                    false,
                    Some("serialize_error"),
                    self.dropped_rate_limited.load(Ordering::Relaxed),
                    self.dropped_channel.load(Ordering::Relaxed),
                    None,
                ));
            }
        };

        let record_bytes = line.len();
        let Some(tx) = &self.tx else {
            return Some(debug_ref(
                &snapshot,
                false,
                Some(SamplingDropReason::Disabled.as_str()),
                self.dropped_rate_limited.load(Ordering::Relaxed),
                self.dropped_channel.load(Ordering::Relaxed),
                Some(record_bytes),
            ));
        };

        match tx.try_send(line) {
            Ok(()) => Some(debug_ref(
                &snapshot,
                true,
                None,
                self.dropped_rate_limited.load(Ordering::Relaxed),
                self.dropped_channel.load(Ordering::Relaxed),
                Some(record_bytes),
            )),
            Err(mpsc::error::TrySendError::Full(_)) => {
                let dropped = self.dropped_channel.fetch_add(1, Ordering::Relaxed) + 1;
                Some(debug_ref(
                    &snapshot,
                    false,
                    Some(SamplingDropReason::ChannelFull.as_str()),
                    self.dropped_rate_limited.load(Ordering::Relaxed),
                    dropped,
                    Some(record_bytes),
                ))
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                let dropped = self.dropped_channel.fetch_add(1, Ordering::Relaxed) + 1;
                Some(debug_ref(
                    &snapshot,
                    false,
                    Some(SamplingDropReason::ChannelClosed.as_str()),
                    self.dropped_rate_limited.load(Ordering::Relaxed),
                    dropped,
                    Some(record_bytes),
                ))
            }
        }
    }
}

fn debug_ref(
    snapshot: &ToolFormatDebugSnapshot,
    sampled: bool,
    drop_reason: Option<&str>,
    dropped_rate_limited: u64,
    dropped_channel: u64,
    record_bytes: Option<usize>,
) -> Value {
    json!({
        "enabled": true,
        "sink": "file",
        "sampled": sampled,
        "fingerprint": snapshot.sampling.fingerprint,
        "groupKey": snapshot.sampling.group_key,
        "sampledIndexInWindow": snapshot.sampling.sampled_index_in_window,
        "fingerprintSeenInWindow": snapshot.sampling.fingerprint_seen_in_window,
        "groupSeenInWindow": snapshot.sampling.group_seen_in_window,
        "globalSeenInWindow": snapshot.sampling.global_seen_in_window,
        "dropReason": drop_reason,
        "droppedRateLimited": dropped_rate_limited,
        "droppedChannel": dropped_channel,
        "recordBytes": record_bytes,
        "requestBody": {
            "captured": snapshot.request_body.captured,
            "dropReason": snapshot.request_body.drop_reason,
            "bytes": snapshot.request_body.bytes,
            "truncated": snapshot.request_body.truncated,
            "seenInWindow": snapshot.request_body.seen_in_window,
            "limitInWindow": snapshot.request_body.limit_in_window,
        },
    })
}

async fn tool_format_debug_writer_loop(
    dir: PathBuf,
    roll_interval_secs: u64,
    max_file_bytes: u64,
    mut rx: mpsc::Receiver<String>,
) {
    let mut state = ToolFormatDebugWriterState::new(dir, roll_interval_secs, max_file_bytes);
    while let Some(mut line) = rx.recv().await {
        line.push('\n');
        let path = match state.path_for_next_record(line.len() as u64).await {
            Ok(path) => path,
            Err(err) => {
                tracing::warn!("选择 tool format debug 滚动文件失败: {}", err);
                continue;
            }
        };
        if let Err(err) = append_line(&path, line.as_bytes()).await {
            tracing::warn!(path = %path.display(), "写入 tool format debug 文件失败: {}", err);
        } else {
            state.record_written(line.len() as u64);
        }
    }
}

struct ToolFormatDebugWriterState {
    dir: PathBuf,
    roll_interval_secs: u64,
    max_file_bytes: u64,
    period_start_epoch_secs: Option<u64>,
    sequence: u32,
    current_path: Option<PathBuf>,
    current_bytes: u64,
}

impl ToolFormatDebugWriterState {
    fn new(dir: PathBuf, roll_interval_secs: u64, max_file_bytes: u64) -> Self {
        Self {
            dir,
            roll_interval_secs: roll_interval_secs.max(60),
            max_file_bytes: max_file_bytes.max(1),
            period_start_epoch_secs: None,
            sequence: 0,
            current_path: None,
            current_bytes: 0,
        }
    }

    async fn path_for_next_record(&mut self, record_bytes: u64) -> std::io::Result<PathBuf> {
        let period_start = current_roll_period_start(self.roll_interval_secs);
        if self.period_start_epoch_secs != Some(period_start) {
            self.period_start_epoch_secs = Some(period_start);
            self.sequence = 0;
            self.current_path = None;
            self.current_bytes = 0;
        }

        if self.current_path.is_none() {
            self.select_existing_or_new_file(period_start).await?;
        }

        if self.current_bytes > 0
            && self.current_bytes.saturating_add(record_bytes) > self.max_file_bytes
        {
            self.sequence = self.sequence.saturating_add(1);
            self.current_path = Some(roll_file_path(&self.dir, period_start, self.sequence));
            self.current_bytes = file_len(self.current_path.as_ref().expect("path")).await?;
        }

        Ok(self.current_path.clone().expect("tool debug path selected"))
    }

    async fn select_existing_or_new_file(&mut self, period_start: u64) -> std::io::Result<()> {
        loop {
            let path = roll_file_path(&self.dir, period_start, self.sequence);
            let len = file_len(&path).await?;
            if len < self.max_file_bytes {
                self.current_path = Some(path);
                self.current_bytes = len;
                return Ok(());
            }
            self.sequence = self.sequence.saturating_add(1);
        }
    }

    fn record_written(&mut self, bytes: u64) {
        self.current_bytes = self.current_bytes.saturating_add(bytes);
    }
}

async fn file_len(path: &Path) -> std::io::Result<u64> {
    match fs::metadata(path).await {
        Ok(metadata) => Ok(metadata.len()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(err) => Err(err),
    }
}

fn current_roll_period_start(roll_interval_secs: u64) -> u64 {
    let now = Utc::now().timestamp().max(0) as u64;
    let interval = roll_interval_secs.max(60);
    now - (now % interval)
}

fn roll_file_path(dir: &Path, period_start_epoch_secs: u64, sequence: u32) -> PathBuf {
    let period_start =
        DateTime::from_timestamp(period_start_epoch_secs as i64, 0).unwrap_or_else(Utc::now);
    dir.join(format!(
        "tool-format-{}-{:03}.jsonl",
        period_start.format("%Y-%m-%d-%H%M"),
        sequence
    ))
}

async fn append_line(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    file.write_all(bytes).await?;
    Ok(())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolFormatDebugSnapshot {
    ts: String,
    request_id: String,
    endpoint: String,
    stream: bool,
    requested_model: String,
    upstream_model: Option<String>,
    error_class: &'static str,
    error_reason: &'static str,
    error_message_class: &'static str,
    body: ToolFormatBodySnapshot,
    history: ToolFormatHistorySnapshot,
    tool_counts: ToolFormatCountsSnapshot,
    repair: ToolFormatRepairSnapshot,
    anomalies: ToolFormatAnomalySnapshot,
    samples: ToolFormatSamples,
    request_body: ToolFormatRequestBodySnapshot,
    sampling: ToolFormatSamplingSnapshot,
    record_truncated: bool,
}

impl ToolFormatDebugSnapshot {
    fn from_event(
        config: &ToolFormatDebugConfig,
        event: ToolFormatDebugEvent<'_>,
        body_sha256: String,
    ) -> Self {
        let repair = event
            .report
            .map(ToolFormatRepairSnapshot::from)
            .unwrap_or_default();
        let samples = collect_samples(event.request, config);
        let error_message_class = classify_error_message(event.error_message);
        let fingerprint = fingerprint(&event, &body_sha256);
        let group_key = group_key(&event, &repair);

        Self {
            ts: Utc::now().to_rfc3339(),
            request_id: truncate_string(event.request_id, config.max_string_bytes),
            endpoint: truncate_string(event.endpoint, config.max_string_bytes),
            stream: event.stream,
            requested_model: truncate_string(event.requested_model, config.max_string_bytes),
            upstream_model: event
                .upstream_model
                .map(|model| truncate_string(model, config.max_string_bytes)),
            error_class: "tool_use_format",
            error_reason: "REQUEST_BODY_INVALID",
            error_message_class,
            body: ToolFormatBodySnapshot {
                final_bytes: event
                    .attempted_body
                    .map(str::len)
                    .or_else(|| event.report.map(|report| report.final_bytes)),
                original_bytes: event.report.map(|report| report.original_bytes),
                still_oversized: event.report.map(|report| report.still_oversized),
                body_sha256,
            },
            history: ToolFormatHistorySnapshot {
                entries_total: event.diagnostics.history_entries_total,
                entries_scanned: event.diagnostics.history_entries_scanned,
                scan_truncated: event.diagnostics.scan_truncated,
                tool_items_scanned: event.diagnostics.tool_items_scanned,
                tool_item_scan_truncated: event.diagnostics.tool_item_scan_truncated,
            },
            tool_counts: ToolFormatCountsSnapshot {
                current_tools: event.diagnostics.current_tool_count,
                current_tool_results: event.diagnostics.current_tool_result_count,
                history_tool_uses: event.diagnostics.history_tool_use_count,
                history_tool_results: event.diagnostics.history_tool_result_count,
                last_assistant_tool_uses: event.diagnostics.last_assistant_tool_use_count,
                matched_current_results: event.diagnostics.current_results_matching_last_assistant,
                unmatched_current_results: event
                    .diagnostics
                    .current_results_not_matching_last_assistant,
            },
            repair,
            anomalies: ToolFormatAnomalySnapshot {
                duplicate_tool_names: event.diagnostics.duplicate_tool_names,
                empty_tool_names: event.diagnostics.empty_tool_names,
                empty_tool_use_ids: event.diagnostics.empty_tool_use_ids,
                empty_tool_result_ids: event.diagnostics.empty_tool_result_ids,
                duplicate_current_tool_result_ids: event
                    .diagnostics
                    .duplicate_current_tool_result_ids,
                duplicate_history_tool_use_ids: event.diagnostics.duplicate_history_tool_use_ids,
                duplicate_history_tool_result_ids: event
                    .diagnostics
                    .duplicate_history_tool_result_ids,
                non_object_tool_use_inputs: event.diagnostics.non_object_tool_use_inputs,
                history_tool_names_missing_from_tools: event
                    .diagnostics
                    .history_tool_names_missing_from_tools,
            },
            samples,
            request_body: ToolFormatRequestBodySnapshot::missing(config),
            sampling: ToolFormatSamplingSnapshot {
                fingerprint,
                group_key,
                sampled: false,
                sampled_index_in_window: 0,
                fingerprint_seen_in_window: 0,
                group_seen_in_window: 0,
                global_seen_in_window: 0,
                drop_reason: None,
                limit_per_fingerprint: config.max_records_per_fingerprint,
                limit_per_group: config.max_records_per_group,
                global_limit: config.max_records_global,
                window_secs: config.window_secs,
            },
            record_truncated: false,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolFormatBodySnapshot {
    final_bytes: Option<usize>,
    original_bytes: Option<usize>,
    still_oversized: Option<bool>,
    body_sha256: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolFormatRequestBodySnapshot {
    captured: bool,
    drop_reason: Option<&'static str>,
    bytes: Option<usize>,
    max_bytes: usize,
    sha256: Option<String>,
    truncated: bool,
    seen_in_window: u32,
    limit_in_window: u32,
    content: Option<String>,
}

impl ToolFormatRequestBodySnapshot {
    fn missing(config: &ToolFormatDebugConfig) -> Self {
        Self {
            captured: false,
            drop_reason: Some("missing"),
            bytes: None,
            max_bytes: config.max_request_body_bytes,
            sha256: None,
            truncated: false,
            seen_in_window: 0,
            limit_in_window: config.max_request_body_records_per_window,
            content: None,
        }
    }

    fn from_attempted_body(
        config: &ToolFormatDebugConfig,
        attempted_body: Option<&str>,
        capture: bool,
        drop_reason: Option<&'static str>,
        seen_in_window: u32,
        include_sha256: bool,
    ) -> Self {
        let Some(body) = attempted_body else {
            return Self {
                seen_in_window,
                ..Self::missing(config)
            };
        };
        let bytes = body.len();
        let sha256 = include_sha256.then(|| sha256_hex(body));
        if !capture {
            return Self {
                captured: false,
                drop_reason: drop_reason.or(Some("not_captured")),
                bytes: Some(bytes),
                max_bytes: config.max_request_body_bytes,
                sha256,
                truncated: false,
                seen_in_window,
                limit_in_window: config.max_request_body_records_per_window,
                content: None,
            };
        }

        let (content, truncated) = truncate_string_to_limit(body, config.max_request_body_bytes);
        Self {
            captured: true,
            drop_reason: None,
            bytes: Some(bytes),
            max_bytes: config.max_request_body_bytes,
            sha256,
            truncated,
            seen_in_window,
            limit_in_window: config.max_request_body_records_per_window,
            content: Some(content),
        }
    }

    fn truncate_content_to(&mut self, max_bytes: usize) {
        let Some(content) = self.content.as_deref() else {
            return;
        };
        let (content, truncated) = truncate_string_to_limit(content, max_bytes);
        self.content = Some(content);
        self.truncated = self.truncated || truncated;
        self.drop_reason.get_or_insert("record_size_limit");
    }

    fn drop_content(&mut self, reason: &'static str) {
        self.content = None;
        self.captured = false;
        self.drop_reason = Some(reason);
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolFormatHistorySnapshot {
    entries_total: usize,
    entries_scanned: usize,
    scan_truncated: bool,
    tool_items_scanned: usize,
    tool_item_scan_truncated: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolFormatCountsSnapshot {
    current_tools: usize,
    current_tool_results: usize,
    history_tool_uses: usize,
    history_tool_results: usize,
    last_assistant_tool_uses: usize,
    matched_current_results: usize,
    unmatched_current_results: usize,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolFormatRepairSnapshot {
    removed_empty_tool_uses: usize,
    removed_duplicate_tool_uses: usize,
    renamed_duplicate_tool_uses: usize,
    removed_orphan_tool_results: usize,
    removed_duplicate_tool_results: usize,
    textified_duplicate_tool_results: usize,
    textified_orphan_tool_results: usize,
    removed_orphan_tool_uses: usize,
    flattened_history_tool_uses: usize,
    textified_history_tool_results: usize,
    removed_history_tools: usize,
    truncated_history_tool_results: usize,
    trimmed_web_fetch_blocks: usize,
    compressed_tool_definitions: usize,
    truncated_current_tool_results: usize,
    truncated_current_documents: usize,
    truncated_current_user_content: usize,
    dropped_current_images: usize,
}

impl From<&PayloadGuardReport> for ToolFormatRepairSnapshot {
    fn from(report: &PayloadGuardReport) -> Self {
        Self {
            removed_empty_tool_uses: report.removed_empty_tool_uses,
            removed_duplicate_tool_uses: report.removed_duplicate_tool_uses,
            renamed_duplicate_tool_uses: report.renamed_duplicate_tool_uses,
            removed_orphan_tool_results: report.removed_orphan_tool_results,
            removed_duplicate_tool_results: report.removed_duplicate_tool_results,
            textified_duplicate_tool_results: report.textified_duplicate_tool_results,
            textified_orphan_tool_results: report.textified_orphan_tool_results,
            removed_orphan_tool_uses: report.removed_orphan_tool_uses,
            flattened_history_tool_uses: report.flattened_history_tool_uses,
            textified_history_tool_results: report.textified_history_tool_results,
            removed_history_tools: report.removed_history_tools,
            truncated_history_tool_results: report.truncated_history_tool_results,
            trimmed_web_fetch_blocks: report.trimmed_web_fetch_blocks,
            compressed_tool_definitions: report.compressed_tool_definitions,
            truncated_current_tool_results: report.truncated_current_tool_results,
            truncated_current_documents: report.truncated_current_documents,
            truncated_current_user_content: report.truncated_current_user_content,
            dropped_current_images: report.dropped_current_images,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolFormatAnomalySnapshot {
    duplicate_tool_names: usize,
    empty_tool_names: usize,
    empty_tool_use_ids: usize,
    empty_tool_result_ids: usize,
    duplicate_current_tool_result_ids: usize,
    duplicate_history_tool_use_ids: usize,
    duplicate_history_tool_result_ids: usize,
    non_object_tool_use_inputs: usize,
    history_tool_names_missing_from_tools: usize,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolFormatSamples {
    non_object_tool_inputs: Vec<ToolFormatSample>,
    empty_tool_use_ids: Vec<ToolFormatSample>,
    empty_tool_result_ids: Vec<ToolFormatSample>,
    duplicate_tool_use_ids: Vec<ToolFormatSample>,
    duplicate_tool_result_ids: Vec<ToolFormatSample>,
    missing_tool_names: Vec<ToolFormatSample>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolFormatSample {
    history_index: Option<usize>,
    role: &'static str,
    reason: &'static str,
    tool_name: Option<String>,
    tool_use_id_hash: Option<String>,
    input_type: Option<&'static str>,
    input_bytes: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolFormatSamplingSnapshot {
    fingerprint: String,
    group_key: String,
    sampled: bool,
    sampled_index_in_window: u32,
    fingerprint_seen_in_window: u32,
    group_seen_in_window: u32,
    global_seen_in_window: u32,
    drop_reason: Option<&'static str>,
    limit_per_fingerprint: u32,
    limit_per_group: u32,
    global_limit: u32,
    window_secs: u64,
}

fn collect_samples(request: &KiroRequest, config: &ToolFormatDebugConfig) -> ToolFormatSamples {
    let mut samples = ToolFormatSamples::default();
    let mut tool_names = HashSet::new();
    for tool in &request
        .conversation_state
        .current_message
        .user_input_message
        .user_input_message_context
        .tools
    {
        let name = tool.tool_specification.name.trim().to_ascii_lowercase();
        if !name.is_empty() {
            tool_names.insert(name);
        }
    }

    let mut seen_tool_uses = HashSet::new();
    let mut seen_tool_results = HashSet::new();
    for (idx, message) in request.conversation_state.history.iter().enumerate() {
        match message {
            Message::Assistant(assistant) => {
                if let Some(tool_uses) = &assistant.assistant_response_message.tool_uses {
                    for tool_use in tool_uses {
                        collect_tool_use_sample(
                            &mut samples,
                            config,
                            Some(idx),
                            "assistant",
                            tool_use,
                            &tool_names,
                            &mut seen_tool_uses,
                        );
                    }
                }
            }
            Message::User(user) => {
                for result in &user
                    .user_input_message
                    .user_input_message_context
                    .tool_results
                {
                    collect_tool_result_sample(
                        &mut samples,
                        config,
                        Some(idx),
                        "user",
                        result,
                        &mut seen_tool_results,
                    );
                }
            }
        }
    }

    for result in &request
        .conversation_state
        .current_message
        .user_input_message
        .user_input_message_context
        .tool_results
    {
        collect_tool_result_sample(
            &mut samples,
            config,
            None,
            "current_user",
            result,
            &mut seen_tool_results,
        );
    }

    samples
}

fn collect_tool_use_sample(
    samples: &mut ToolFormatSamples,
    config: &ToolFormatDebugConfig,
    history_index: Option<usize>,
    role: &'static str,
    tool_use: &ToolUseEntry,
    known_tool_names: &HashSet<String>,
    seen_tool_uses: &mut HashSet<String>,
) {
    let id = tool_use.tool_use_id.trim();
    let name = tool_use.name.trim();
    if id.is_empty() {
        push_sample(
            &mut samples.empty_tool_use_ids,
            config,
            sample(history_index, role, "empty_tool_use_id", name, id, tool_use),
        );
    } else if !seen_tool_uses.insert(id.to_string()) {
        push_sample(
            &mut samples.duplicate_tool_use_ids,
            config,
            sample(
                history_index,
                role,
                "duplicate_tool_use_id",
                name,
                id,
                tool_use,
            ),
        );
    }
    if !name.is_empty() && !known_tool_names.contains(&name.to_ascii_lowercase()) {
        push_sample(
            &mut samples.missing_tool_names,
            config,
            sample(history_index, role, "missing_tool_name", name, id, tool_use),
        );
    }
    if !tool_use.input.is_object() {
        push_sample(
            &mut samples.non_object_tool_inputs,
            config,
            sample(
                history_index,
                role,
                "non_object_tool_input",
                name,
                id,
                tool_use,
            ),
        );
    }
}

fn collect_tool_result_sample(
    samples: &mut ToolFormatSamples,
    config: &ToolFormatDebugConfig,
    history_index: Option<usize>,
    role: &'static str,
    result: &ToolResult,
    seen_tool_results: &mut HashSet<String>,
) {
    let id = result.tool_use_id.trim();
    if id.is_empty() {
        push_sample(
            &mut samples.empty_tool_result_ids,
            config,
            ToolFormatSample {
                history_index,
                role,
                reason: "empty_tool_result_id",
                tool_name: None,
                tool_use_id_hash: None,
                input_type: None,
                input_bytes: None,
            },
        );
    } else if !seen_tool_results.insert(id.to_string()) {
        push_sample(
            &mut samples.duplicate_tool_result_ids,
            config,
            ToolFormatSample {
                history_index,
                role,
                reason: "duplicate_tool_result_id",
                tool_name: None,
                tool_use_id_hash: Some(short_hash(id)),
                input_type: None,
                input_bytes: None,
            },
        );
    }
}

fn sample(
    history_index: Option<usize>,
    role: &'static str,
    reason: &'static str,
    tool_name: &str,
    tool_use_id: &str,
    tool_use: &ToolUseEntry,
) -> ToolFormatSample {
    ToolFormatSample {
        history_index,
        role,
        reason,
        tool_name: (!tool_name.trim().is_empty()).then(|| truncate_string(tool_name, 128)),
        tool_use_id_hash: (!tool_use_id.trim().is_empty()).then(|| short_hash(tool_use_id)),
        input_type: Some(value_type(&tool_use.input)),
        input_bytes: serde_json::to_vec(&tool_use.input)
            .ok()
            .map(|bytes| bytes.len()),
    }
}

fn push_sample(
    list: &mut Vec<ToolFormatSample>,
    config: &ToolFormatDebugConfig,
    sample: ToolFormatSample,
) {
    if list.len() < config.max_samples_per_kind {
        list.push(sample);
    }
}

fn value_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn classify_error_message(message: &str) -> &'static str {
    let lower = message.to_ascii_lowercase();
    if lower.contains("invalid tool use format") {
        "invalid_tool_use_format"
    } else if lower.contains("request_body_invalid") {
        "request_body_invalid"
    } else {
        "tool_format_error"
    }
}

fn body_identity_for_sampling(event: &ToolFormatDebugEvent<'_>) -> String {
    if let Some(body_sha256) = event
        .report
        .and_then(|report| report.body_sha256.as_deref())
    {
        return body_sha256.to_string();
    }
    if let Some(body) = event.attempted_body {
        return sampled_body_hash(body);
    }
    short_hash(event.error_message)
}

fn sampled_body_hash(body: &str) -> String {
    let bytes = body.as_bytes();
    let head_len = bytes.len().min(TOOL_FORMAT_DEBUG_BODY_HASH_SAMPLE_BYTES);
    let tail_start = bytes
        .len()
        .saturating_sub(TOOL_FORMAT_DEBUG_BODY_HASH_SAMPLE_BYTES)
        .max(head_len);

    let mut hasher = Sha256::new();
    hasher.update(bytes.len().to_le_bytes());
    hasher.update(&bytes[..head_len]);
    if tail_start < bytes.len() {
        hasher.update(&bytes[tail_start..]);
    }
    let digest = hasher.finalize();
    hex_prefix(&digest, 32)
}

fn fingerprint(event: &ToolFormatDebugEvent<'_>, body_sha256: &str) -> String {
    short_hash(&format!(
        "{}|{}|{:?}|{}|{}|{}|{}|{}",
        event.endpoint,
        event.requested_model,
        event.upstream_model,
        classify_error_message(event.error_message),
        body_sha256,
        event.diagnostics.current_tool_count,
        event.diagnostics.current_tool_result_count,
        event.diagnostics.last_assistant_tool_use_count
    ))
}

fn group_key(event: &ToolFormatDebugEvent<'_>, repair: &ToolFormatRepairSnapshot) -> String {
    short_hash(&format!(
        "{}|{:?}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
        event.endpoint,
        event.upstream_model,
        classify_error_message(event.error_message),
        event.diagnostics.history_tool_use_count,
        event.diagnostics.history_tool_result_count,
        event.diagnostics.current_tool_result_count,
        event
            .diagnostics
            .current_results_not_matching_last_assistant,
        event.diagnostics.duplicate_history_tool_use_ids,
        event.diagnostics.non_object_tool_use_inputs,
        repair.flattened_history_tool_uses,
        repair.textified_history_tool_results
    ))
}

fn serialize_snapshot_with_budget(
    snapshot: &mut ToolFormatDebugSnapshot,
    max_record_bytes: usize,
) -> Result<String, serde_json::Error> {
    let mut line = serde_json::to_string(snapshot)?;
    if line.len() <= max_record_bytes {
        return Ok(line);
    }

    snapshot.samples = ToolFormatSamples::default();
    snapshot.record_truncated = true;
    line = serde_json::to_string(snapshot)?;
    if line.len() <= max_record_bytes {
        return Ok(line);
    }

    if snapshot.request_body.content.is_some() {
        let target_body_bytes = max_record_bytes.saturating_div(2).max(128);
        snapshot.request_body.truncate_content_to(target_body_bytes);
        line = serde_json::to_string(snapshot)?;
        if line.len() <= max_record_bytes {
            return Ok(line);
        }

        snapshot.request_body.drop_content("record_size_limit");
        line = serde_json::to_string(snapshot)?;
    }

    Ok(line)
}

fn sha256_hex(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    hex_prefix(&digest, 32)
}

fn short_hash(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    hex_prefix(&digest, 8)
}

fn hex_prefix(bytes: &[u8], len: usize) -> String {
    let mut out = String::with_capacity(len * 2);
    for byte in bytes.iter().take(len) {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn truncate_string(value: &str, max_bytes: usize) -> String {
    let (value, truncated) = truncate_string_to_limit(value, max_bytes);
    if truncated {
        format!("{value}...")
    } else {
        value
    }
}

fn truncate_string_to_limit(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_string(), false);
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_string(), true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kiro::model::requests::{
        conversation::{
            AssistantMessage, ConversationState, CurrentMessage, HistoryAssistantMessage,
            HistoryUserMessage, UserInputMessage, UserInputMessageContext,
        },
        tool::ToolUseEntry,
    };

    fn test_request() -> KiroRequest {
        let assistant = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage::new("tool call").with_tool_uses(vec![
                ToolUseEntry::new("tool-1", "missingTool").with_input(json!("bad-input")),
                ToolUseEntry::new("tool-1", "missingTool").with_input(json!({})),
            ]),
        };
        let mut user = HistoryUserMessage::new("result", "model");
        user.user_input_message.user_input_message_context = UserInputMessageContext::new()
            .with_tool_results(vec![
                ToolResult::success("tool-1", "secret result content"),
                ToolResult::success("tool-1", "duplicate secret"),
            ]);

        KiroRequest {
            conversation_state: ConversationState::new("conv")
                .with_current_message(CurrentMessage::new(UserInputMessage::new(
                    "current", "model",
                )))
                .with_history(vec![
                    Message::User(HistoryUserMessage::new("user", "model")),
                    Message::Assistant(assistant),
                    Message::User(user),
                ]),
            profile_arn: None,
            additional_model_request_fields: None,
            tool_cache_point_insert_after: Vec::new(),
            cache_point_plan_recording_enabled: true,
        }
    }

    fn test_event<'a>(
        request: &'a KiroRequest,
        diagnostics: ToolUseFormatDiagnostics,
    ) -> ToolFormatDebugEvent<'a> {
        ToolFormatDebugEvent {
            request_id: "req-test",
            endpoint: "/cc/v1/messages",
            stream: false,
            requested_model: "claude-sonnet-4-6",
            upstream_model: Some("claude-sonnet-4.6"),
            error_message: "400 Bad Request Invalid tool use format REQUEST_BODY_INVALID",
            attempted_body: None,
            request,
            report: None,
            diagnostics,
        }
    }

    fn test_diagnostics() -> ToolUseFormatDiagnostics {
        ToolUseFormatDiagnostics {
            history_entries_total: 3,
            history_entries_scanned: 3,
            scan_truncated: false,
            tool_items_scanned: 4,
            tool_item_scan_truncated: false,
            current_tool_count: 0,
            current_tool_result_count: 0,
            history_tool_use_count: 2,
            history_tool_result_count: 2,
            last_assistant_tool_use_count: 2,
            current_results_matching_last_assistant: 0,
            current_results_not_matching_last_assistant: 0,
            duplicate_current_tool_result_ids: 0,
            duplicate_history_tool_use_ids: 1,
            duplicate_history_tool_result_ids: 1,
            duplicate_tool_names: 0,
            empty_tool_names: 0,
            empty_tool_use_ids: 0,
            empty_tool_result_ids: 0,
            non_object_tool_use_inputs: 1,
            history_tool_names_missing_from_tools: 2,
        }
    }

    #[test]
    fn sampler_limits_same_fingerprint_without_blocking() {
        let config = ToolFormatDebugConfig {
            max_records_per_fingerprint: 1,
            max_records_per_group: 10,
            max_records_global: 10,
            ..ToolFormatDebugConfig::default()
        }
        .normalized();
        let mut sampler = SamplerState::default();
        let first = sampler.decide(&config, "fp", "group", true);
        let second = sampler.decide(&config, "fp", "group", true);

        assert!(first.sampled);
        assert!(!second.sampled);
        assert_eq!(
            second.drop_reason,
            Some(SamplingDropReason::FingerprintLimit)
        );
    }

    #[test]
    fn snapshot_samples_do_not_include_tool_result_content() {
        let request = test_request();
        let snapshot = ToolFormatDebugSnapshot::from_event(
            &ToolFormatDebugConfig::default().normalized(),
            test_event(&request, test_diagnostics()),
            "test-body-hash".to_string(),
        );
        let json = serde_json::to_string(&snapshot).unwrap();

        assert!(json.contains("nonObjectToolInputs"));
        assert!(json.contains("duplicateToolResultIds"));
        assert!(!json.contains("secret result content"));
        assert!(!json.contains("duplicate secret"));
    }

    #[tokio::test]
    async fn recorder_writes_sampled_jsonl_and_rate_limits_same_fingerprint() {
        let dir = std::env::temp_dir().join(format!(
            "kiro-tool-format-debug-test-{}-{}",
            std::process::id(),
            fastrand::u64(..)
        ));
        let config = ToolFormatDebugConfig {
            dir: dir.to_string_lossy().to_string(),
            max_records_per_fingerprint: 1,
            max_records_per_group: 10,
            max_records_global: 10,
            ..ToolFormatDebugConfig::default()
        };
        let recorder = ToolFormatDebugRecorder::new(config);
        let request = test_request();

        let first = recorder
            .record(test_event(&request, test_diagnostics()))
            .expect("first ref");
        let second = recorder
            .record(test_event(&request, test_diagnostics()))
            .expect("second ref");

        assert_eq!(first["sampled"], true);
        assert_eq!(second["sampled"], false);
        assert_eq!(second["dropReason"], "fingerprint_limit");

        let content = wait_debug_dir_contains(&dir, "invalid_tool_use_format").await;
        assert!(content.contains("req-test"));
        assert!(content.contains("invalid_tool_use_format"));
        assert!(!content.contains("secret result content"));
        assert!(!content.contains("duplicate secret"));

        let _ = fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn recorder_captures_attempted_body_only_within_body_limit() {
        let dir = std::env::temp_dir().join(format!(
            "kiro-tool-format-debug-body-test-{}-{}",
            std::process::id(),
            fastrand::u64(..)
        ));
        let config = ToolFormatDebugConfig {
            dir: dir.to_string_lossy().to_string(),
            max_records_per_fingerprint: 10,
            max_records_per_group: 10,
            max_records_global: 10,
            max_record_bytes: 16 * 1024,
            max_request_body_bytes: 64,
            max_request_body_records_per_window: 1,
            ..ToolFormatDebugConfig::default()
        };
        let recorder = ToolFormatDebugRecorder::new(config);
        let request = test_request();
        let diagnostics = test_diagnostics();
        let body =
            r#"{"messages":[{"role":"user","content":"request body content for diagnostics"}]}"#;
        let event = ToolFormatDebugEvent {
            request_id: "req-body-1",
            endpoint: "/cc/v1/messages",
            stream: false,
            requested_model: "claude-sonnet-4-6",
            upstream_model: Some("claude-sonnet-4.6"),
            error_message: "400 Bad Request Invalid tool use format REQUEST_BODY_INVALID",
            attempted_body: Some(body),
            request: &request,
            report: None,
            diagnostics,
        };
        let first = recorder.record(event).expect("first ref");
        let second = recorder
            .record(ToolFormatDebugEvent {
                request_id: "req-body-2",
                endpoint: "/cc/v1/messages",
                stream: false,
                requested_model: "claude-sonnet-4-6",
                upstream_model: Some("claude-sonnet-4.6"),
                error_message: "400 Bad Request Invalid tool use format REQUEST_BODY_INVALID",
                attempted_body: Some(body),
                request: &request,
                report: None,
                diagnostics,
            })
            .expect("second ref");

        assert_eq!(first["sampled"], true);
        assert_eq!(first["requestBody"]["captured"], true);
        assert_eq!(second["sampled"], true);
        assert_eq!(second["requestBody"]["captured"], false);
        assert_eq!(
            second["requestBody"]["dropReason"],
            "request_body_record_limit"
        );

        let content = wait_debug_dir_contains(&dir, "req-body-2").await;
        assert!(content.contains("req-body-1"));
        assert!(content.contains("req-body-2"));
        assert!(content.contains("request body content for diagnostics"));
        assert!(content.contains("request_body_record_limit"));

        let _ = fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn writer_state_rolls_when_file_size_budget_would_be_exceeded() {
        let dir = std::env::temp_dir().join(format!(
            "kiro-tool-format-debug-roll-test-{}-{}",
            std::process::id(),
            fastrand::u64(..)
        ));
        let mut state = ToolFormatDebugWriterState::new(dir.clone(), 30 * 60, 10);

        let first = state.path_for_next_record(8).await.expect("first path");
        state.record_written(8);
        let second = state.path_for_next_record(8).await.expect("second path");

        assert_ne!(first, second);
        assert!(first.to_string_lossy().ends_with("-000.jsonl"));
        assert!(second.to_string_lossy().ends_with("-001.jsonl"));

        let _ = fs::remove_dir_all(&dir).await;
    }

    async fn read_debug_dir(dir: &Path) -> String {
        let mut entries = match fs::read_dir(dir).await {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return String::new(),
            Err(err) => panic!("debug dir: {err}"),
        };
        let mut content = String::new();
        while let Some(entry) = entries.next_entry().await.expect("debug entry") {
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with("tool-format-")
            {
                content.push_str(&fs::read_to_string(entry.path()).await.expect("debug jsonl"));
            }
        }
        content
    }

    async fn wait_debug_dir_contains(dir: &Path, needle: &str) -> String {
        let mut last = String::new();
        for _ in 0..50 {
            last = read_debug_dir(dir).await;
            if last.contains(needle) {
                return last;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        last
    }
}
