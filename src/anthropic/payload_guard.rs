//! Final Kiro payload size guard.
//!
//! Kiro upstream can return a generic `400 Improperly formed request` when the
//! serialized request body is too large. This guard runs after Anthropic->Kiro
//! conversion, measures the actual JSON payload bytes, and trims old history
//! entries while preserving Kiro history invariants.

use std::{
    collections::HashSet,
    io::{self, Write},
    time::{Duration, Instant},
};

use crate::anthropic::types::{
    Message as AnthropicMessage, MessagesRequest, Tool as AnthropicTool,
};
use crate::kiro::model::requests::{
    conversation::{Message, UserInputMessage, UserMessage},
    kiro::KiroRequest,
    tool::{Tool, ToolResult},
};
use crate::model::config::{OversizedImageHandling, PayloadShapingConfig};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

const CURRENT_FIT_MIN_TEXT_CHARS: usize = 512;
const CURRENT_FIT_MAX_ITERATIONS: usize = 64;
const CURRENT_FIT_OVERHEAD_BYTES: usize = 512;
const PAYLOAD_GUARD_SLOW_LOG_THRESHOLD: Duration = Duration::from_millis(25);
const UPSTREAM_IMAGE_SOURCE_MAX_BYTES: usize = 5 * 1024 * 1024;
const EMPTY_TOOL_RESULT_CONTENT_PLACEHOLDER: &str = "Tool result content was empty.";
const EMPTY_USER_CONTENT_PLACEHOLDER: &str = ".";
const TOOL_FORMAT_DIAGNOSTIC_MAX_HISTORY_ENTRIES: usize = 512;
const TOOL_FORMAT_DIAGNOSTIC_MAX_ITEMS: usize = 4096;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PayloadByteBreakdown {
    pub total_bytes: usize,
    pub history_bytes: usize,
    pub current_message_bytes: usize,
    pub current_content_bytes: usize,
    pub current_tools_bytes: usize,
    pub current_tool_results_bytes: usize,
    pub current_images_bytes: usize,
    pub history_tool_results_bytes: usize,
    pub history_images_bytes: usize,
    #[serde(default)]
    pub history_reasoning_content_bytes: usize,
    pub history_entries: usize,
    pub current_tool_count: usize,
    pub current_tool_result_count: usize,
    pub current_image_count: usize,
    pub largest_tool_bytes: usize,
    pub largest_history_tool_result_bytes: usize,
    pub largest_current_tool_result_bytes: usize,
    pub history_tool_use_count: usize,
    pub history_tool_result_count: usize,
    #[serde(default)]
    pub history_reasoning_content_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolUseFormatDiagnostics {
    pub history_entries_total: usize,
    pub history_entries_scanned: usize,
    pub scan_truncated: bool,
    pub tool_items_scanned: usize,
    pub tool_item_scan_truncated: bool,
    pub current_tool_count: usize,
    pub current_tool_result_count: usize,
    pub history_tool_use_count: usize,
    pub history_tool_result_count: usize,
    pub last_assistant_tool_use_count: usize,
    pub current_results_matching_last_assistant: usize,
    pub current_results_not_matching_last_assistant: usize,
    pub duplicate_current_tool_result_ids: usize,
    pub duplicate_history_tool_use_ids: usize,
    pub duplicate_history_tool_result_ids: usize,
    pub duplicate_tool_names: usize,
    pub empty_tool_names: usize,
    pub empty_tool_descriptions: usize,
    pub invalid_tool_schema_property_keys: usize,
    pub empty_tool_use_ids: usize,
    pub empty_tool_result_ids: usize,
    pub non_object_tool_use_inputs: usize,
    pub history_tool_names_missing_from_tools: usize,
}

impl ToolUseFormatDiagnostics {
    pub fn has_tool_payload(&self) -> bool {
        self.current_tool_count > 0
            || self.current_tool_result_count > 0
            || self.history_tool_use_count > 0
            || self.history_tool_result_count > 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PayloadGuardConfig {
    pub enabled: bool,
    pub max_bytes: usize,
    pub trim_history: bool,
    pub shaping: PayloadShapingConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PayloadGuardReport {
    pub enabled: bool,
    pub max_bytes: usize,
    pub original_bytes: usize,
    pub final_bytes: usize,
    pub original_history_entries: usize,
    pub final_history_entries: usize,
    pub trimmed_history_entries: usize,
    #[serde(default)]
    pub guard_serializations: usize,
    #[serde(default)]
    pub history_trim_passes: usize,
    pub aligned_leading_entries: usize,
    pub removed_empty_tool_uses: usize,
    #[serde(default)]
    pub removed_duplicate_tool_uses: usize,
    #[serde(default)]
    pub renamed_duplicate_tool_uses: usize,
    pub removed_orphan_tool_results: usize,
    #[serde(default)]
    pub removed_duplicate_tool_results: usize,
    #[serde(default)]
    pub textified_duplicate_tool_results: usize,
    pub textified_orphan_tool_results: usize,
    pub removed_orphan_tool_uses: usize,
    #[serde(default)]
    pub flattened_history_tool_uses: usize,
    #[serde(default)]
    pub textified_history_tool_results: usize,
    #[serde(default)]
    pub removed_history_tools: usize,
    pub truncated_history_tool_results: usize,
    pub truncated_history_tool_result_chars: usize,
    pub removed_history_thinking_blocks: usize,
    pub removed_history_thinking_chars: usize,
    pub trimmed_web_fetch_blocks: usize,
    pub trimmed_web_fetch_chars: usize,
    pub compressed_tool_definitions: usize,
    pub compressed_tool_definition_bytes: usize,
    pub truncated_current_tool_results: usize,
    pub truncated_current_tool_result_chars: usize,
    pub truncated_current_documents: usize,
    pub truncated_current_document_chars: usize,
    pub truncated_current_user_content: usize,
    pub truncated_current_user_content_chars: usize,
    pub dropped_current_images: usize,
    pub dropped_current_image_bytes: usize,
    #[serde(default)]
    pub dropped_historical_images: usize,
    #[serde(default)]
    pub dropped_historical_image_bytes: usize,
    #[serde(default)]
    pub kiro_cache_points_planned: usize,
    #[serde(default)]
    pub kiro_cache_points_inserted: usize,
    #[serde(default)]
    pub cache_point_retry_without_cache_point: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_point_retry_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_format_diagnostics: Option<ToolUseFormatDiagnostics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_format_debug_ref: Option<Value>,
    pub still_oversized: bool,
}

impl PayloadGuardReport {
    fn disabled(size: usize, history_entries: usize) -> Self {
        Self {
            enabled: false,
            max_bytes: 0,
            original_bytes: size,
            final_bytes: size,
            original_history_entries: history_entries,
            final_history_entries: history_entries,
            trimmed_history_entries: 0,
            guard_serializations: 0,
            history_trim_passes: 0,
            aligned_leading_entries: 0,
            removed_empty_tool_uses: 0,
            removed_duplicate_tool_uses: 0,
            renamed_duplicate_tool_uses: 0,
            removed_orphan_tool_results: 0,
            removed_duplicate_tool_results: 0,
            textified_duplicate_tool_results: 0,
            textified_orphan_tool_results: 0,
            removed_orphan_tool_uses: 0,
            flattened_history_tool_uses: 0,
            textified_history_tool_results: 0,
            removed_history_tools: 0,
            truncated_history_tool_results: 0,
            truncated_history_tool_result_chars: 0,
            removed_history_thinking_blocks: 0,
            removed_history_thinking_chars: 0,
            trimmed_web_fetch_blocks: 0,
            trimmed_web_fetch_chars: 0,
            compressed_tool_definitions: 0,
            compressed_tool_definition_bytes: 0,
            truncated_current_tool_results: 0,
            truncated_current_tool_result_chars: 0,
            truncated_current_documents: 0,
            truncated_current_document_chars: 0,
            truncated_current_user_content: 0,
            truncated_current_user_content_chars: 0,
            dropped_current_images: 0,
            dropped_current_image_bytes: 0,
            dropped_historical_images: 0,
            dropped_historical_image_bytes: 0,
            kiro_cache_points_planned: 0,
            kiro_cache_points_inserted: 0,
            cache_point_retry_without_cache_point: false,
            cache_point_retry_reason: None,
            body_sha256: None,
            tool_use_format_diagnostics: None,
            tool_format_debug_ref: None,
            still_oversized: false,
        }
    }

    pub fn was_modified(&self) -> bool {
        self.trimmed_history_entries > 0
            || self.aligned_leading_entries > 0
            || self.removed_empty_tool_uses > 0
            || self.removed_duplicate_tool_uses > 0
            || self.renamed_duplicate_tool_uses > 0
            || self.removed_orphan_tool_results > 0
            || self.removed_duplicate_tool_results > 0
            || self.textified_duplicate_tool_results > 0
            || self.textified_orphan_tool_results > 0
            || self.removed_orphan_tool_uses > 0
            || self.flattened_history_tool_uses > 0
            || self.textified_history_tool_results > 0
            || self.removed_history_tools > 0
            || self.truncated_history_tool_results > 0
            || self.removed_history_thinking_blocks > 0
            || self.trimmed_web_fetch_blocks > 0
            || self.compressed_tool_definitions > 0
            || self.truncated_current_tool_results > 0
            || self.truncated_current_documents > 0
            || self.truncated_current_user_content > 0
            || self.dropped_current_images > 0
            || self.dropped_historical_images > 0
    }

    pub fn warning_header_fragment(&self) -> Option<String> {
        if !self.was_modified() && !self.still_oversized {
            return None;
        }
        let mut parts = Vec::new();
        if self.trimmed_history_entries > 0 {
            parts.push(format!(
                "payload-trimmed-history={}",
                self.trimmed_history_entries
            ));
        }
        if self.aligned_leading_entries > 0 {
            parts.push(format!(
                "payload-aligned-history={}",
                self.aligned_leading_entries
            ));
        }
        if self.removed_empty_tool_uses > 0 {
            parts.push(format!(
                "payload-empty-tool-uses={}",
                self.removed_empty_tool_uses
            ));
        }
        if self.removed_duplicate_tool_uses > 0 {
            parts.push(format!(
                "payload-duplicate-tool-uses={}",
                self.removed_duplicate_tool_uses
            ));
        }
        if self.renamed_duplicate_tool_uses > 0 {
            parts.push(format!(
                "payload-renamed-duplicate-tool-uses={}",
                self.renamed_duplicate_tool_uses
            ));
        }
        if self.removed_orphan_tool_results > 0 {
            parts.push(format!(
                "payload-orphan-tool-results={}",
                self.removed_orphan_tool_results
            ));
        }
        if self.removed_duplicate_tool_results > 0 {
            parts.push(format!(
                "payload-duplicate-tool-results={}",
                self.removed_duplicate_tool_results
            ));
        }
        if self.textified_duplicate_tool_results > 0 {
            parts.push(format!(
                "payload-textified-duplicate-tool-results={}",
                self.textified_duplicate_tool_results
            ));
        }
        if self.textified_orphan_tool_results > 0 {
            parts.push(format!(
                "payload-textified-tool-results={}",
                self.textified_orphan_tool_results
            ));
        }
        if self.removed_orphan_tool_uses > 0 {
            parts.push(format!(
                "payload-orphan-tool-uses={}",
                self.removed_orphan_tool_uses
            ));
        }
        if self.truncated_history_tool_results > 0 {
            parts.push(format!(
                "payload-history-tool-results-truncated={}",
                self.truncated_history_tool_results
            ));
        }
        if self.removed_history_thinking_blocks > 0 {
            parts.push(format!(
                "payload-history-thinking-blocks={}",
                self.removed_history_thinking_blocks
            ));
        }
        if self.trimmed_web_fetch_blocks > 0 {
            parts.push(format!(
                "payload-web-fetch-trimmed={}",
                self.trimmed_web_fetch_blocks
            ));
        }
        if self.compressed_tool_definitions > 0 {
            parts.push(format!(
                "payload-tools-compressed={}",
                self.compressed_tool_definitions
            ));
        }
        if self.truncated_current_tool_results > 0 {
            parts.push(format!(
                "payload-current-tool-results-truncated={}",
                self.truncated_current_tool_results
            ));
        }
        if self.truncated_current_documents > 0 {
            parts.push(format!(
                "payload-current-documents-truncated={}",
                self.truncated_current_documents
            ));
        }
        if self.truncated_current_user_content > 0 {
            parts.push(format!(
                "payload-current-content-truncated={}",
                self.truncated_current_user_content
            ));
        }
        if self.dropped_current_images > 0 {
            parts.push(format!(
                "payload-current-images-dropped={}",
                self.dropped_current_images
            ));
        }
        if self.dropped_historical_images > 0 {
            parts.push(format!(
                "payload-history-images-dropped={}",
                self.dropped_historical_images
            ));
        }
        if self.still_oversized {
            parts.push(format!("payload-oversized={}", self.final_bytes));
        }
        (!parts.is_empty()).then(|| parts.join(","))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadGuardError {
    Serialize(String),
    OversizedImage {
        current_images: usize,
        current_image_bytes: usize,
        historical_images: usize,
        historical_image_bytes: usize,
        max_source_bytes: usize,
    },
}

impl std::fmt::Display for PayloadGuardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PayloadGuardError::Serialize(err) => write!(f, "序列化请求失败: {}", err),
            PayloadGuardError::OversizedImage {
                current_images,
                current_image_bytes,
                historical_images,
                historical_image_bytes,
                max_source_bytes,
            } => write!(
                f,
                "image exceeds upstream size limit: current_images={}, current_image_bytes={}, historical_images={}, historical_image_bytes={}, max_source_bytes={}",
                current_images,
                current_image_bytes,
                historical_images,
                historical_image_bytes,
                max_source_bytes
            ),
        }
    }
}

pub fn guard_kiro_request(
    request: &mut KiroRequest,
    config: PayloadGuardConfig,
) -> Result<(String, PayloadGuardReport), PayloadGuardError> {
    let guard_started_at = Instant::now();
    let mut serialize_elapsed = Duration::ZERO;
    let mut repair_elapsed = Duration::ZERO;
    let mut shaping_elapsed = Duration::ZERO;
    let mut trim_elapsed = Duration::ZERO;
    let mut current_shaping_elapsed = Duration::ZERO;
    let mut history_trim_iterations = 0usize;
    let mut guard_serializations = 0usize;

    let serialize_started_at = Instant::now();
    let original_body = serialize_request(request)?;
    guard_serializations += 1;
    serialize_elapsed += serialize_started_at.elapsed();
    let original_bytes = original_body.len();
    let original_history_entries = request.conversation_state.history.len();

    if !config.enabled {
        let mut report = PayloadGuardReport::disabled(original_bytes, original_history_entries);
        report.guard_serializations = guard_serializations;
        set_cache_point_report_fields(&mut report, request);
        report.body_sha256 = Some(sha256_hex(&original_body));
        return Ok((original_body, report));
    }
    let size_limit_enabled = config.max_bytes > 0;

    let mut report = PayloadGuardReport {
        enabled: true,
        max_bytes: config.max_bytes,
        original_bytes,
        final_bytes: original_bytes,
        original_history_entries,
        final_history_entries: original_history_entries,
        trimmed_history_entries: 0,
        guard_serializations,
        history_trim_passes: 0,
        aligned_leading_entries: 0,
        removed_empty_tool_uses: 0,
        removed_duplicate_tool_uses: 0,
        renamed_duplicate_tool_uses: 0,
        removed_orphan_tool_results: 0,
        removed_duplicate_tool_results: 0,
        textified_duplicate_tool_results: 0,
        textified_orphan_tool_results: 0,
        removed_orphan_tool_uses: 0,
        flattened_history_tool_uses: 0,
        textified_history_tool_results: 0,
        removed_history_tools: 0,
        truncated_history_tool_results: 0,
        truncated_history_tool_result_chars: 0,
        removed_history_thinking_blocks: 0,
        removed_history_thinking_chars: 0,
        trimmed_web_fetch_blocks: 0,
        trimmed_web_fetch_chars: 0,
        compressed_tool_definitions: 0,
        compressed_tool_definition_bytes: 0,
        truncated_current_tool_results: 0,
        truncated_current_tool_result_chars: 0,
        truncated_current_documents: 0,
        truncated_current_document_chars: 0,
        truncated_current_user_content: 0,
        truncated_current_user_content_chars: 0,
        dropped_current_images: 0,
        dropped_current_image_bytes: 0,
        dropped_historical_images: 0,
        dropped_historical_image_bytes: 0,
        kiro_cache_points_planned: 0,
        kiro_cache_points_inserted: 0,
        cache_point_retry_without_cache_point: false,
        cache_point_retry_reason: None,
        body_sha256: None,
        tool_use_format_diagnostics: None,
        tool_format_debug_ref: None,
        still_oversized: false,
    };

    let repair_started_at = Instant::now();
    report.aligned_leading_entries +=
        align_history_to_user(&mut request.conversation_state.history);

    let initial_repair = repair_request(request);
    let initial_repair_modified = initial_repair.was_modified();
    add_repair_stats_to_report(&mut report, initial_repair);
    repair_elapsed += repair_started_at.elapsed();

    let mut body = if initial_repair_modified {
        let serialize_started_at = Instant::now();
        let body = serialize_request(request)?;
        guard_serializations += 1;
        serialize_elapsed += serialize_started_at.elapsed();
        body
    } else {
        original_body
    };
    report.final_bytes = body.len();

    if config.shaping.enabled {
        let shaping_started_at = Instant::now();
        reject_oversized_images_if_configured(
            config.shaping,
            find_oversized_kiro_images(request, UPSTREAM_IMAGE_SOURCE_MAX_BYTES),
            UPSTREAM_IMAGE_SOURCE_MAX_BYTES,
        )?;
        let safety_shaping = apply_payload_safety_shaping(request, config.shaping);
        let should_reserialize = safety_shaping.was_modified();
        add_shaping_stats_to_report(&mut report, safety_shaping);
        let current_safety_shaping = apply_current_payload_safety_shaping(request, config.shaping);
        let should_reserialize = should_reserialize || current_safety_shaping.was_modified();
        add_current_shaping_stats_to_report(&mut report, &current_safety_shaping);
        if should_reserialize {
            let serialize_started_at = Instant::now();
            body = serialize_request(request)?;
            guard_serializations += 1;
            serialize_elapsed += serialize_started_at.elapsed();
            report.final_bytes = body.len();
        }
        shaping_elapsed += shaping_started_at.elapsed();
    }

    if size_limit_enabled && report.final_bytes > config.max_bytes && config.shaping.enabled {
        let shaping_started_at = Instant::now();
        let shaping = apply_payload_shaping(request, config.shaping);
        let should_reserialize = shaping.was_modified();
        add_shaping_stats_to_report(&mut report, shaping);

        if should_reserialize {
            let serialize_started_at = Instant::now();
            body = serialize_request(request)?;
            guard_serializations += 1;
            serialize_elapsed += serialize_started_at.elapsed();
            report.final_bytes = body.len();
        }
        shaping_elapsed += shaping_started_at.elapsed();
    }

    if size_limit_enabled && config.trim_history {
        let trim_started_at = Instant::now();
        while report.final_bytes > config.max_bytes
            && !request.conversation_state.history.is_empty()
        {
            history_trim_iterations += 1;
            let removed = {
                let state = &mut request.conversation_state;
                trim_history_to_estimated_budget(
                    &mut state.history,
                    &state
                        .current_message
                        .user_input_message
                        .user_input_message_context
                        .tool_results,
                    report.final_bytes,
                    config.max_bytes,
                )
            };
            if removed == 0 {
                break;
            }
            report.trimmed_history_entries += removed;

            let aligned = align_history_to_user(&mut request.conversation_state.history);
            report.aligned_leading_entries += aligned;

            let repair = repair_request(request);
            add_repair_stats_to_report(&mut report, repair);

            let serialize_started_at = Instant::now();
            body = serialize_request(request)?;
            guard_serializations += 1;
            serialize_elapsed += serialize_started_at.elapsed();
            let new_size = body.len();
            report.final_bytes = new_size;
        }
        trim_elapsed += trim_started_at.elapsed();
    }

    if size_limit_enabled
        && report.final_bytes > config.max_bytes
        && config.shaping.enabled
        && current_payload_shaping_enabled(config.shaping)
    {
        let current_started_at = Instant::now();
        let (new_body, current_stats) = apply_current_payload_shaping_until_fit(
            request,
            config.shaping,
            config.max_bytes,
            body,
            &mut serialize_elapsed,
            &mut guard_serializations,
        )?;
        add_current_shaping_stats_to_report(&mut report, &current_stats);
        body = new_body;
        report.final_bytes = body.len();
        current_shaping_elapsed += current_started_at.elapsed();

        if current_stats.was_modified() {
            let repair_started_at = Instant::now();
            let repair = repair_request(request);
            let should_reserialize = repair.was_modified();
            add_repair_stats_to_report(&mut report, repair);
            repair_elapsed += repair_started_at.elapsed();

            if should_reserialize {
                let serialize_started_at = Instant::now();
                body = serialize_request(request)?;
                guard_serializations += 1;
                serialize_elapsed += serialize_started_at.elapsed();
                report.final_bytes = body.len();
            }
        }
    }

    let final_repair_started_at = Instant::now();
    let final_repair = repair_request(request);
    let should_reserialize = final_repair.was_modified();
    add_repair_stats_to_report(&mut report, final_repair);
    repair_elapsed += final_repair_started_at.elapsed();

    if should_reserialize {
        let serialize_started_at = Instant::now();
        body = serialize_request(request)?;
        guard_serializations += 1;
        serialize_elapsed += serialize_started_at.elapsed();
    }

    report.final_history_entries = request.conversation_state.history.len();
    report.final_bytes = body.len();
    report.guard_serializations = guard_serializations;
    report.history_trim_passes = history_trim_iterations;
    report.still_oversized = size_limit_enabled && report.final_bytes > config.max_bytes;
    set_cache_point_report_fields(&mut report, request);
    report.body_sha256 = Some(sha256_hex(&body));

    log_payload_guard_timing(
        "kiro",
        guard_started_at.elapsed(),
        serialize_elapsed,
        repair_elapsed,
        shaping_elapsed,
        trim_elapsed,
        current_shaping_elapsed,
        history_trim_iterations,
        &report,
    );

    Ok((body, report))
}

pub fn breakdown_kiro_request(
    request: &KiroRequest,
    serialized_body: &str,
) -> PayloadByteBreakdown {
    let state = &request.conversation_state;
    let current_user = &state.current_message.user_input_message;
    let context = &current_user.user_input_message_context;
    let (history_reasoning_content_count, history_reasoning_content_bytes) =
        history_reasoning_content_stats(&state.history);

    PayloadByteBreakdown {
        total_bytes: serialized_body.len(),
        history_bytes: json_len(&state.history),
        current_message_bytes: json_len(&state.current_message),
        current_content_bytes: current_user.content.len(),
        current_tools_bytes: json_len(&context.tools),
        current_tool_results_bytes: json_len(&context.tool_results),
        current_images_bytes: json_len(&current_user.images),
        history_tool_results_bytes: history_tool_results_bytes(&state.history),
        history_images_bytes: history_images_bytes(&state.history),
        history_reasoning_content_bytes,
        history_entries: state.history.len(),
        current_tool_count: context.tools.len(),
        current_tool_result_count: context.tool_results.len(),
        current_image_count: current_user.images.len(),
        largest_tool_bytes: context.tools.iter().map(json_len).max().unwrap_or(0),
        largest_history_tool_result_bytes: largest_history_tool_result_bytes(&state.history),
        largest_current_tool_result_bytes: context
            .tool_results
            .iter()
            .map(json_len)
            .max()
            .unwrap_or(0),
        history_tool_use_count: count_history_tool_uses(&state.history),
        history_tool_result_count: count_history_tool_results(&state.history),
        history_reasoning_content_count,
    }
}

pub fn diagnose_kiro_tool_use_format(request: &KiroRequest) -> ToolUseFormatDiagnostics {
    let state = &request.conversation_state;
    let current_user = &state.current_message.user_input_message;
    let current_context = &current_user.user_input_message_context;
    let history_entries_total = state.history.len();
    let scan_start =
        history_entries_total.saturating_sub(TOOL_FORMAT_DIAGNOSTIC_MAX_HISTORY_ENTRIES);
    let history_entries_scanned = history_entries_total.saturating_sub(scan_start);
    let scan_truncated = scan_start > 0;
    let mut tool_items_scanned = 0usize;
    let mut tool_item_scan_truncated = false;

    let mut tool_names = HashSet::new();
    let mut duplicate_tool_names = 0usize;
    let mut empty_tool_names = 0usize;
    let mut empty_tool_descriptions = 0usize;
    let mut invalid_tool_schema_property_keys = 0usize;
    for tool in &current_context.tools {
        if !claim_tool_diagnostic_item(&mut tool_items_scanned, &mut tool_item_scan_truncated) {
            break;
        }
        let spec = &tool.tool_specification;
        let name = spec.name.trim();
        if name.is_empty() {
            empty_tool_names += 1;
        } else if !tool_names.insert(name.to_ascii_lowercase()) {
            duplicate_tool_names += 1;
        }
        if spec.description.trim().is_empty() {
            empty_tool_descriptions += 1;
        }
        invalid_tool_schema_property_keys += count_invalid_tool_schema_property_keys(
            &spec.input_schema.json,
            &mut tool_items_scanned,
            &mut tool_item_scan_truncated,
        );
    }

    let last_assistant_tool_use_ids: HashSet<String> =
        match state.history.iter().rev().find_map(|message| {
            if let Message::Assistant(assistant) = message {
                Some(assistant)
            } else {
                None
            }
        }) {
            Some(assistant) => assistant
                .assistant_response_message
                .tool_uses
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .filter_map(|tool_use| {
                    let id = tool_use.tool_use_id.trim();
                    (!id.is_empty()).then(|| id.to_string())
                })
                .collect(),
            None => HashSet::new(),
        };

    let mut current_result_ids = HashSet::new();
    let mut current_results_matching_last_assistant = 0usize;
    let mut current_results_not_matching_last_assistant = 0usize;
    let mut duplicate_current_tool_result_ids = 0usize;
    let mut empty_tool_result_ids = 0usize;
    for result in &current_context.tool_results {
        if !claim_tool_diagnostic_item(&mut tool_items_scanned, &mut tool_item_scan_truncated) {
            break;
        }
        let id = result.tool_use_id.trim();
        if id.is_empty() {
            empty_tool_result_ids += 1;
            continue;
        }
        if !current_result_ids.insert(id.to_string()) {
            duplicate_current_tool_result_ids += 1;
        }
        if last_assistant_tool_use_ids.contains(id) {
            current_results_matching_last_assistant += 1;
        } else {
            current_results_not_matching_last_assistant += 1;
        }
    }

    let mut history_tool_use_ids = HashSet::new();
    let mut history_tool_result_ids = HashSet::new();
    let mut history_tool_use_count = 0usize;
    let mut history_tool_result_count = 0usize;
    let mut duplicate_history_tool_use_ids = 0usize;
    let mut duplicate_history_tool_result_ids = 0usize;
    let mut empty_tool_use_ids = 0usize;
    let mut non_object_tool_use_inputs = 0usize;
    let mut history_tool_names_missing_from_tools = 0usize;

    for message in state.history.iter().skip(scan_start) {
        match message {
            Message::Assistant(assistant) => {
                let Some(tool_uses) = &assistant.assistant_response_message.tool_uses else {
                    continue;
                };
                for tool_use in tool_uses {
                    if !claim_tool_diagnostic_item(
                        &mut tool_items_scanned,
                        &mut tool_item_scan_truncated,
                    ) {
                        break;
                    }
                    history_tool_use_count += 1;
                    let id = tool_use.tool_use_id.trim();
                    if id.is_empty() {
                        empty_tool_use_ids += 1;
                    } else if !history_tool_use_ids.insert(id.to_string()) {
                        duplicate_history_tool_use_ids += 1;
                    }

                    let name = tool_use.name.trim();
                    if name.is_empty() {
                        empty_tool_names += 1;
                    } else if !tool_names.contains(&name.to_ascii_lowercase()) {
                        history_tool_names_missing_from_tools += 1;
                    }

                    if !tool_use.input.is_object() {
                        non_object_tool_use_inputs += 1;
                    }
                }
            }
            Message::User(user) => {
                for result in &user
                    .user_input_message
                    .user_input_message_context
                    .tool_results
                {
                    if !claim_tool_diagnostic_item(
                        &mut tool_items_scanned,
                        &mut tool_item_scan_truncated,
                    ) {
                        break;
                    }
                    history_tool_result_count += 1;
                    let id = result.tool_use_id.trim();
                    if id.is_empty() {
                        empty_tool_result_ids += 1;
                    } else if !history_tool_result_ids.insert(id.to_string()) {
                        duplicate_history_tool_result_ids += 1;
                    }
                }
            }
        }
    }

    ToolUseFormatDiagnostics {
        history_entries_total,
        history_entries_scanned,
        scan_truncated,
        tool_items_scanned,
        tool_item_scan_truncated,
        current_tool_count: current_context.tools.len(),
        current_tool_result_count: current_context.tool_results.len(),
        history_tool_use_count,
        history_tool_result_count,
        last_assistant_tool_use_count: last_assistant_tool_use_ids.len(),
        current_results_matching_last_assistant,
        current_results_not_matching_last_assistant,
        duplicate_current_tool_result_ids,
        duplicate_history_tool_use_ids,
        duplicate_history_tool_result_ids,
        duplicate_tool_names,
        empty_tool_names,
        empty_tool_descriptions,
        invalid_tool_schema_property_keys,
        empty_tool_use_ids,
        empty_tool_result_ids,
        non_object_tool_use_inputs,
        history_tool_names_missing_from_tools,
    }
}

fn count_invalid_tool_schema_property_keys(
    value: &serde_json::Value,
    scanned: &mut usize,
    truncated: &mut bool,
) -> usize {
    if *scanned >= TOOL_FORMAT_DIAGNOSTIC_MAX_ITEMS {
        *truncated = true;
        return 0;
    }

    match value {
        serde_json::Value::Object(obj) => {
            let mut invalid = 0usize;
            if let Some(serde_json::Value::Object(properties)) = obj.get("properties") {
                for (name, schema) in properties {
                    if !claim_tool_diagnostic_item(scanned, truncated) {
                        return invalid;
                    }
                    if !is_valid_tool_schema_property_key(name) {
                        invalid += 1;
                    }
                    invalid += count_invalid_tool_schema_property_keys(schema, scanned, truncated);
                    if *truncated {
                        return invalid;
                    }
                }
            }

            for key in ["$defs", "patternProperties", "dependentSchemas"] {
                if let Some(serde_json::Value::Object(map)) = obj.get(key) {
                    for child in map.values() {
                        invalid +=
                            count_invalid_tool_schema_property_keys(child, scanned, truncated);
                        if *truncated {
                            return invalid;
                        }
                    }
                }
            }

            for key in [
                "items",
                "contains",
                "not",
                "if",
                "then",
                "else",
                "propertyNames",
                "contentSchema",
            ] {
                if let Some(child) = obj.get(key) {
                    invalid += count_invalid_tool_schema_property_keys(child, scanned, truncated);
                    if *truncated {
                        return invalid;
                    }
                }
            }

            for key in ["prefixItems", "oneOf", "anyOf", "allOf"] {
                if let Some(serde_json::Value::Array(items)) = obj.get(key) {
                    for child in items {
                        invalid +=
                            count_invalid_tool_schema_property_keys(child, scanned, truncated);
                        if *truncated {
                            return invalid;
                        }
                    }
                }
            }
            invalid
        }
        serde_json::Value::Array(items) => {
            let mut invalid = 0usize;
            for item in items {
                invalid += count_invalid_tool_schema_property_keys(item, scanned, truncated);
                if *truncated {
                    return invalid;
                }
            }
            invalid
        }
        _ => 0,
    }
}

fn is_valid_tool_schema_property_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

fn claim_tool_diagnostic_item(scanned: &mut usize, truncated: &mut bool) -> bool {
    if *scanned >= TOOL_FORMAT_DIAGNOSTIC_MAX_ITEMS {
        *truncated = true;
        return false;
    }
    *scanned += 1;
    true
}

#[cfg(test)]
pub fn guard_anthropic_messages_request(
    request: &mut MessagesRequest,
    config: PayloadGuardConfig,
    original_body_bytes: usize,
) -> Result<(String, PayloadGuardReport), PayloadGuardError> {
    let (body, report) =
        guard_anthropic_messages_request_inner(request, config, original_body_bytes, None)?;
    let body = String::from_utf8(body.to_vec())
        .map_err(|err| PayloadGuardError::Serialize(err.to_string()))?;
    Ok((body, report))
}

pub fn guard_anthropic_messages_request_reusing_body(
    request: &mut MessagesRequest,
    config: PayloadGuardConfig,
    original_body: &Bytes,
) -> Result<(Bytes, PayloadGuardReport), PayloadGuardError> {
    guard_anthropic_messages_request_inner(
        request,
        config,
        original_body.len(),
        Some(original_body),
    )
}

fn serialize_anthropic_body_into(
    body: &mut Option<String>,
    request: &MessagesRequest,
    serialize_elapsed: &mut Duration,
    guard_serializations: &mut usize,
) -> Result<usize, PayloadGuardError> {
    let serialize_started_at = Instant::now();
    let serialized = serialize_anthropic_request(request)?;
    *guard_serializations = (*guard_serializations).saturating_add(1);
    *serialize_elapsed += serialize_started_at.elapsed();
    let len = serialized.len();
    *body = Some(serialized);
    Ok(len)
}

fn take_or_serialize_anthropic_body(
    body: &mut Option<String>,
    request: &MessagesRequest,
    serialize_elapsed: &mut Duration,
    guard_serializations: &mut usize,
) -> Result<String, PayloadGuardError> {
    if let Some(body) = body.take() {
        return Ok(body);
    }
    let serialize_started_at = Instant::now();
    let serialized = serialize_anthropic_request(request)?;
    *guard_serializations = (*guard_serializations).saturating_add(1);
    *serialize_elapsed += serialize_started_at.elapsed();
    Ok(serialized)
}

fn guard_anthropic_messages_request_inner(
    request: &mut MessagesRequest,
    config: PayloadGuardConfig,
    original_body_bytes: usize,
    original_body: Option<&Bytes>,
) -> Result<(Bytes, PayloadGuardReport), PayloadGuardError> {
    let guard_started_at = Instant::now();
    let mut serialize_elapsed = Duration::ZERO;
    let mut repair_elapsed = Duration::ZERO;
    let mut shaping_elapsed = Duration::ZERO;
    let mut trim_elapsed = Duration::ZERO;
    let mut current_shaping_elapsed = Duration::ZERO;
    let mut history_trim_iterations = 0usize;
    let mut guard_serializations = 0usize;

    let mut body = None;
    let original_history_entries = request.messages.len().saturating_sub(1);
    let size_limit_enabled = config.max_bytes > 0;

    if original_body.is_none()
        || !config.enabled
        || (size_limit_enabled && original_body_bytes > config.max_bytes)
    {
        serialize_anthropic_body_into(
            &mut body,
            request,
            &mut serialize_elapsed,
            &mut guard_serializations,
        )?;
    }

    if !config.enabled {
        let mut report =
            PayloadGuardReport::disabled(original_body_bytes, original_history_entries);
        report.guard_serializations = guard_serializations;
        let body = body
            .map(Bytes::from)
            .or_else(|| original_body.cloned())
            .ok_or_else(|| PayloadGuardError::Serialize("missing request body".to_string()))?;
        report.body_sha256 = Some(sha256_hex_bytes(&body));
        return Ok((body, report));
    }

    let mut report = new_payload_guard_report(
        config.max_bytes,
        original_body_bytes,
        original_history_entries,
    );
    let mut final_bytes = if let Some(body) = body.as_ref() {
        body.len()
    } else {
        original_body_bytes
    };
    report.final_bytes = final_bytes;

    let repair_started_at = Instant::now();
    let repair = repair_anthropic_messages(request);
    let should_reserialize = repair.was_modified();
    add_anthropic_repair_stats_to_report(&mut report, repair);
    repair_elapsed += repair_started_at.elapsed();

    if should_reserialize {
        final_bytes = serialize_anthropic_body_into(
            &mut body,
            request,
            &mut serialize_elapsed,
            &mut guard_serializations,
        )?;
        report.final_bytes = final_bytes;
    }

    if config.shaping.enabled {
        let shaping_started_at = Instant::now();
        reject_oversized_images_if_configured(
            config.shaping,
            find_oversized_anthropic_images(request, UPSTREAM_IMAGE_SOURCE_MAX_BYTES),
            UPSTREAM_IMAGE_SOURCE_MAX_BYTES,
        )?;
        let safety_shaping = apply_anthropic_payload_safety_shaping(request, config.shaping);
        let should_reserialize = safety_shaping.was_modified();
        add_shaping_stats_to_report(&mut report, safety_shaping);
        let current_safety_shaping =
            apply_anthropic_current_payload_safety_shaping(request, config.shaping);
        let should_reserialize = should_reserialize || current_safety_shaping.was_modified();
        add_current_shaping_stats_to_report(&mut report, &current_safety_shaping);
        if should_reserialize {
            final_bytes = serialize_anthropic_body_into(
                &mut body,
                request,
                &mut serialize_elapsed,
                &mut guard_serializations,
            )?;
            report.final_bytes = final_bytes;
        }
        shaping_elapsed += shaping_started_at.elapsed();
    }

    if size_limit_enabled && final_bytes > config.max_bytes && config.shaping.enabled {
        let shaping_started_at = Instant::now();
        let shaping = apply_anthropic_payload_shaping(request, config.shaping);
        let should_reserialize = shaping.was_modified();
        add_shaping_stats_to_report(&mut report, shaping);

        if should_reserialize {
            final_bytes = serialize_anthropic_body_into(
                &mut body,
                request,
                &mut serialize_elapsed,
                &mut guard_serializations,
            )?;
            report.final_bytes = final_bytes;
        }
        shaping_elapsed += shaping_started_at.elapsed();
    }

    if size_limit_enabled && config.trim_history {
        let trim_started_at = Instant::now();
        while final_bytes > config.max_bytes && request.messages.len() > 1 {
            history_trim_iterations += 1;
            let removed = trim_anthropic_history_to_estimated_budget(
                &mut request.messages,
                final_bytes,
                config.max_bytes,
            );
            if removed == 0 {
                break;
            }
            report.trimmed_history_entries += removed;

            let repair_started_at = Instant::now();
            let repair = repair_anthropic_messages(request);
            add_anthropic_repair_stats_to_report(&mut report, repair);
            repair_elapsed += repair_started_at.elapsed();

            let new_size = serialize_anthropic_body_into(
                &mut body,
                request,
                &mut serialize_elapsed,
                &mut guard_serializations,
            )?;
            final_bytes = new_size;
            report.final_bytes = final_bytes;
        }
        trim_elapsed += trim_started_at.elapsed();
    }

    if size_limit_enabled
        && final_bytes > config.max_bytes
        && config.shaping.enabled
        && current_payload_shaping_enabled(config.shaping)
    {
        let current_started_at = Instant::now();
        let current_body = take_or_serialize_anthropic_body(
            &mut body,
            request,
            &mut serialize_elapsed,
            &mut guard_serializations,
        )?;
        let (new_body, current_stats) = apply_anthropic_current_payload_shaping_until_fit(
            request,
            config.shaping,
            config.max_bytes,
            current_body,
            &mut serialize_elapsed,
            &mut guard_serializations,
        )?;
        add_current_shaping_stats_to_report(&mut report, &current_stats);
        final_bytes = new_body.len();
        body = Some(new_body);
        report.final_bytes = final_bytes;
        current_shaping_elapsed += current_started_at.elapsed();

        if current_stats.was_modified() {
            let repair_started_at = Instant::now();
            let repair = repair_anthropic_messages(request);
            let should_reserialize = repair.was_modified();
            add_anthropic_repair_stats_to_report(&mut report, repair);
            repair_elapsed += repair_started_at.elapsed();

            if should_reserialize {
                final_bytes = serialize_anthropic_body_into(
                    &mut body,
                    request,
                    &mut serialize_elapsed,
                    &mut guard_serializations,
                )?;
                report.final_bytes = final_bytes;
            }
        }
    }

    report.final_history_entries = request.messages.len().saturating_sub(1);
    report.still_oversized = size_limit_enabled && final_bytes > config.max_bytes;
    report.guard_serializations = guard_serializations;
    report.history_trim_passes = history_trim_iterations;
    let body = body
        .map(Bytes::from)
        .or_else(|| original_body.cloned())
        .ok_or_else(|| PayloadGuardError::Serialize("missing request body".to_string()))?;
    report.body_sha256 = Some(sha256_hex_bytes(&body));
    log_payload_guard_timing(
        "anthropic",
        guard_started_at.elapsed(),
        serialize_elapsed,
        repair_elapsed,
        shaping_elapsed,
        trim_elapsed,
        current_shaping_elapsed,
        history_trim_iterations,
        &report,
    );
    Ok((body, report))
}

pub fn breakdown_anthropic_messages_request(
    request: &MessagesRequest,
    total_bytes: usize,
) -> PayloadByteBreakdown {
    let history_end = request.messages.len().saturating_sub(1);
    let current_message = request.messages.last();
    let (history_reasoning_content_count, history_reasoning_content_bytes) =
        anthropic_history_reasoning_content_stats(&request.messages[..history_end]);

    PayloadByteBreakdown {
        total_bytes,
        history_bytes: json_len(&request.messages[..history_end]),
        current_message_bytes: current_message.map(json_len).unwrap_or(0),
        current_content_bytes: current_message
            .map(|message| json_len(&message.content))
            .unwrap_or(0),
        current_tools_bytes: json_len(&request.tools),
        current_tool_results_bytes: current_message
            .map(|message| content_tool_results_bytes(&message.content))
            .unwrap_or(0),
        current_images_bytes: current_message
            .map(|message| content_images_bytes(&message.content))
            .unwrap_or(0),
        history_tool_results_bytes: request.messages[..history_end]
            .iter()
            .map(|message| content_tool_results_bytes(&message.content))
            .sum(),
        history_images_bytes: request.messages[..history_end]
            .iter()
            .map(|message| content_images_bytes(&message.content))
            .sum(),
        history_reasoning_content_bytes,
        history_entries: history_end,
        current_tool_count: request.tools.as_ref().map(Vec::len).unwrap_or(0),
        current_tool_result_count: current_message
            .map(|message| count_content_blocks_by_type(&message.content, "tool_result"))
            .unwrap_or(0),
        current_image_count: current_message
            .map(|message| count_content_blocks_by_type(&message.content, "image"))
            .unwrap_or(0),
        largest_tool_bytes: request
            .tools
            .as_ref()
            .and_then(|tools| tools.iter().map(json_len).max())
            .unwrap_or(0),
        largest_history_tool_result_bytes: request.messages[..history_end]
            .iter()
            .flat_map(|message| content_blocks_by_type(&message.content, "tool_result"))
            .map(json_len)
            .max()
            .unwrap_or(0),
        largest_current_tool_result_bytes: current_message
            .map(|message| {
                content_blocks_by_type(&message.content, "tool_result")
                    .into_iter()
                    .map(json_len)
                    .max()
                    .unwrap_or(0)
            })
            .unwrap_or(0),
        history_tool_use_count: request.messages[..history_end]
            .iter()
            .map(|message| count_content_blocks_by_type(&message.content, "tool_use"))
            .sum(),
        history_tool_result_count: request.messages[..history_end]
            .iter()
            .map(|message| count_content_blocks_by_type(&message.content, "tool_result"))
            .sum(),
        history_reasoning_content_count,
    }
}

fn new_payload_guard_report(
    max_bytes: usize,
    original_bytes: usize,
    original_history_entries: usize,
) -> PayloadGuardReport {
    PayloadGuardReport {
        enabled: true,
        max_bytes,
        original_bytes,
        final_bytes: original_bytes,
        original_history_entries,
        final_history_entries: original_history_entries,
        trimmed_history_entries: 0,
        guard_serializations: 0,
        history_trim_passes: 0,
        aligned_leading_entries: 0,
        removed_empty_tool_uses: 0,
        removed_duplicate_tool_uses: 0,
        renamed_duplicate_tool_uses: 0,
        removed_orphan_tool_results: 0,
        removed_duplicate_tool_results: 0,
        textified_duplicate_tool_results: 0,
        textified_orphan_tool_results: 0,
        removed_orphan_tool_uses: 0,
        flattened_history_tool_uses: 0,
        textified_history_tool_results: 0,
        removed_history_tools: 0,
        truncated_history_tool_results: 0,
        truncated_history_tool_result_chars: 0,
        removed_history_thinking_blocks: 0,
        removed_history_thinking_chars: 0,
        trimmed_web_fetch_blocks: 0,
        trimmed_web_fetch_chars: 0,
        compressed_tool_definitions: 0,
        compressed_tool_definition_bytes: 0,
        truncated_current_tool_results: 0,
        truncated_current_tool_result_chars: 0,
        truncated_current_documents: 0,
        truncated_current_document_chars: 0,
        truncated_current_user_content: 0,
        truncated_current_user_content_chars: 0,
        dropped_current_images: 0,
        dropped_current_image_bytes: 0,
        dropped_historical_images: 0,
        dropped_historical_image_bytes: 0,
        kiro_cache_points_planned: 0,
        kiro_cache_points_inserted: 0,
        cache_point_retry_without_cache_point: false,
        cache_point_retry_reason: None,
        body_sha256: None,
        tool_use_format_diagnostics: None,
        tool_format_debug_ref: None,
        still_oversized: false,
    }
}

fn sha256_hex_bytes(value: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value);
    format!("{:x}", hasher.finalize())
}

fn sha256_hex(value: &str) -> String {
    sha256_hex_bytes(value.as_bytes())
}

pub fn serialize_kiro_request(request: &KiroRequest) -> Result<String, PayloadGuardError> {
    serialize_request(request)
}

fn serialize_request(request: &KiroRequest) -> Result<String, PayloadGuardError> {
    let normalized_request;
    let request = if request
        .additional_model_request_fields
        .as_ref()
        .is_some_and(|fields| {
            fields.output_config.is_some()
                && fields
                    .thinking
                    .as_ref()
                    .is_some_and(|thinking| thinking.thinking_type != "adaptive")
        }) {
        normalized_request = {
            let mut request = request.clone();
            request.normalize_output_config_thinking_compatibility();
            request
        };
        &normalized_request
    } else {
        request
    };

    if request.tool_cache_point_insert_after.is_empty() {
        return serde_json::to_string(request)
            .map_err(|err| PayloadGuardError::Serialize(err.to_string()));
    }

    let mut value = serde_json::to_value(request)
        .map_err(|err| PayloadGuardError::Serialize(err.to_string()))?;
    insert_tool_cache_points(&mut value, &request.tool_cache_point_insert_after);
    serde_json::to_string(&value).map_err(|err| PayloadGuardError::Serialize(err.to_string()))
}

fn serialize_anthropic_request(request: &MessagesRequest) -> Result<String, PayloadGuardError> {
    serde_json::to_string(request).map_err(|err| PayloadGuardError::Serialize(err.to_string()))
}

fn set_cache_point_report_fields(report: &mut PayloadGuardReport, request: &KiroRequest) {
    if !request.cache_point_plan_recording_enabled {
        return;
    }
    report.kiro_cache_points_planned = request.tool_cache_point_insert_after.len();
    report.kiro_cache_points_inserted =
        valid_tool_cache_point_insertions(request, &request.tool_cache_point_insert_after);
}

fn valid_tool_cache_point_insertions(request: &KiroRequest, plan: &[usize]) -> usize {
    if plan.is_empty() {
        return 0;
    }
    let tool_count = request
        .conversation_state
        .current_message
        .user_input_message
        .user_input_message_context
        .tools
        .len();
    let mut indices = plan
        .iter()
        .copied()
        .filter(|idx| *idx < tool_count)
        .collect::<Vec<_>>();
    indices.sort_unstable();
    indices.dedup();
    indices.len()
}

fn insert_tool_cache_points(value: &mut Value, plan: &[usize]) -> usize {
    if plan.is_empty() {
        return 0;
    }
    let Some(tools) = value
        .pointer_mut(
            "/conversationState/currentMessage/userInputMessage/userInputMessageContext/tools",
        )
        .and_then(Value::as_array_mut)
    else {
        return 0;
    };

    let mut indices = plan
        .iter()
        .copied()
        .filter(|idx| *idx < tools.len())
        .collect::<Vec<_>>();
    indices.sort_unstable();
    indices.dedup();
    let inserted = indices.len();
    for idx in indices.into_iter().rev() {
        tools.insert(
            idx + 1,
            serde_json::json!({
                "cachePoint": {
                    "type": "default"
                }
            }),
        );
    }
    inserted
}

#[derive(Default)]
struct CountingWriter {
    bytes: usize,
}

impl Write for CountingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.bytes = self.bytes.saturating_add(buf.len());
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn json_len<T: serde::Serialize + ?Sized>(value: &T) -> usize {
    let mut writer = CountingWriter::default();
    serde_json::to_writer(&mut writer, value)
        .map(|_| writer.bytes)
        .unwrap_or(0)
}

fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

#[allow(clippy::too_many_arguments)]
fn log_payload_guard_timing(
    payload_kind: &'static str,
    total_elapsed: Duration,
    serialize_elapsed: Duration,
    repair_elapsed: Duration,
    shaping_elapsed: Duration,
    trim_elapsed: Duration,
    current_shaping_elapsed: Duration,
    history_trim_iterations: usize,
    report: &PayloadGuardReport,
) {
    if !report.enabled {
        return;
    }

    if total_elapsed >= PAYLOAD_GUARD_SLOW_LOG_THRESHOLD
        || report.was_modified()
        || report.still_oversized
    {
        tracing::debug!(
            payload_kind,
            total_ms = duration_ms(total_elapsed),
            serialize_ms = duration_ms(serialize_elapsed),
            repair_ms = duration_ms(repair_elapsed),
            shaping_ms = duration_ms(shaping_elapsed),
            history_trim_ms = duration_ms(trim_elapsed),
            current_shaping_ms = duration_ms(current_shaping_elapsed),
            history_trim_iterations,
            guard_serializations = report.guard_serializations,
            original_bytes = report.original_bytes,
            final_bytes = report.final_bytes,
            max_bytes = report.max_bytes,
            modified = report.was_modified(),
            flattened_history_tool_uses = report.flattened_history_tool_uses,
            textified_history_tool_results = report.textified_history_tool_results,
            removed_history_tools = report.removed_history_tools,
            still_oversized = report.still_oversized,
            "payload guard timing"
        );
    }
}

fn content_blocks(content: &Value) -> Option<&Vec<Value>> {
    content.as_array()
}

fn content_blocks_mut(content: &mut Value) -> Option<&mut Vec<Value>> {
    content.as_array_mut()
}

fn block_type(block: &Value) -> Option<&str> {
    block.get("type").and_then(Value::as_str)
}

fn content_blocks_by_type<'a>(content: &'a Value, expected_type: &str) -> Vec<&'a Value> {
    content_blocks(content)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| block_type(block) == Some(expected_type))
                .collect()
        })
        .unwrap_or_default()
}

fn count_content_blocks_by_type(content: &Value, expected_type: &str) -> usize {
    content_blocks(content)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| block_type(block) == Some(expected_type))
                .count()
        })
        .unwrap_or(0)
}

fn content_tool_results_bytes(content: &Value) -> usize {
    content_blocks_by_type(content, "tool_result")
        .into_iter()
        .map(json_len)
        .sum()
}

fn content_images_bytes(content: &Value) -> usize {
    content_blocks_by_type(content, "image")
        .into_iter()
        .map(json_len)
        .sum()
}

fn is_anthropic_reasoning_content_block(block: &Value) -> bool {
    matches!(
        block_type(block),
        Some("thinking") | Some("redacted_thinking")
    ) || block.get("thinking").is_some()
        || block.get("signature").is_some()
}

/// Returns the assistant immediately preceding the current user tool-result turn.
///
/// The assistant's signed/redacted reasoning is part of the active tool continuation and must
/// not be treated as disposable historical context. Only a fully paired latest assistant is
/// protected; ordinary assistant history remains eligible for shaping.
fn active_anthropic_tool_turn_assistant_index(messages: &[AnthropicMessage]) -> Option<usize> {
    let current_index = messages.len().checked_sub(1)?;
    let current = messages.get(current_index)?;
    if current.role != "user" {
        return None;
    }
    let result_ids = anthropic_tool_result_ids(&current.content);
    if result_ids.is_empty() {
        return None;
    }
    let assistant_index = current_index.checked_sub(1)?;
    let assistant = messages.get(assistant_index)?;
    if assistant.role != "assistant" {
        return None;
    }
    let use_ids = anthropic_tool_use_ids(&assistant.content);
    (!use_ids.is_empty() && use_ids.iter().all(|id| result_ids.contains(id)))
        .then_some(assistant_index)
}

fn anthropic_history_reasoning_content_stats(messages: &[AnthropicMessage]) -> (usize, usize) {
    messages
        .iter()
        .filter(|message| message.role == "assistant")
        .filter_map(|message| content_blocks(&message.content))
        .flat_map(|blocks| blocks.iter())
        .filter(|block| is_anthropic_reasoning_content_block(block))
        .fold((0usize, 0usize), |(count, bytes), block| {
            (
                count.saturating_add(1),
                bytes.saturating_add(json_len(block)),
            )
        })
}

fn count_history_tool_uses(history: &[Message]) -> usize {
    history
        .iter()
        .map(|message| match message {
            Message::Assistant(assistant) => assistant
                .assistant_response_message
                .tool_uses
                .as_ref()
                .map(Vec::len)
                .unwrap_or(0),
            Message::User(_) => 0,
        })
        .sum()
}

fn count_history_tool_results(history: &[Message]) -> usize {
    history
        .iter()
        .map(|message| match message {
            Message::User(user) => user
                .user_input_message
                .user_input_message_context
                .tool_results
                .len(),
            Message::Assistant(_) => 0,
        })
        .sum()
}

fn history_tool_results_bytes(history: &[Message]) -> usize {
    history
        .iter()
        .map(|message| match message {
            Message::User(user) => json_len(
                &user
                    .user_input_message
                    .user_input_message_context
                    .tool_results,
            ),
            Message::Assistant(_) => 0,
        })
        .sum()
}

fn largest_history_tool_result_bytes(history: &[Message]) -> usize {
    history
        .iter()
        .filter_map(|message| match message {
            Message::User(user) => Some(
                user.user_input_message
                    .user_input_message_context
                    .tool_results
                    .iter()
                    .map(json_len)
                    .max()
                    .unwrap_or(0),
            ),
            Message::Assistant(_) => None,
        })
        .max()
        .unwrap_or(0)
}

fn history_images_bytes(history: &[Message]) -> usize {
    history
        .iter()
        .map(|message| match message {
            Message::User(user) => json_len(&user.user_input_message.images),
            Message::Assistant(_) => 0,
        })
        .sum()
}

fn history_reasoning_content_stats(history: &[Message]) -> (usize, usize) {
    history
        .iter()
        .filter_map(|message| match message {
            Message::Assistant(assistant) => assistant
                .assistant_response_message
                .reasoning_content
                .as_ref(),
            Message::User(_) => None,
        })
        .fold((0usize, 0usize), |(count, bytes), reasoning_content| {
            (
                count.saturating_add(1),
                bytes.saturating_add(json_len(reasoning_content)),
            )
        })
}

fn kiro_image_source_bytes(image: &crate::kiro::model::requests::conversation::KiroImage) -> usize {
    image
        .source
        .bytes
        .as_ref()
        .map(|bytes| decoded_base64_source_bytes(bytes).unwrap_or(bytes.len()))
        .unwrap_or_else(|| json_len(image))
}

fn decoded_base64_source_bytes(value: &str) -> Option<usize> {
    let value = inline_base64_payload(value);
    let mut symbols = 0usize;
    let mut padding = 0usize;
    let mut saw_padding = false;

    for byte in value.bytes() {
        if byte.is_ascii_whitespace() {
            continue;
        }
        if byte == b'=' {
            saw_padding = true;
            padding = padding.checked_add(1)?;
            if padding > 2 {
                return None;
            }
        } else {
            if saw_padding || !(byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/')) {
                return None;
            }
        }
        symbols = symbols.checked_add(1)?;
    }

    if symbols == 0 {
        return Some(0);
    }
    let complete = (symbols / 4).checked_mul(3)?;
    let remainder = symbols % 4;
    if padding > 0 {
        if remainder != 0 || symbols < 4 {
            return None;
        }
        return complete.checked_sub(padding);
    }
    match remainder {
        0 => Some(complete),
        2 => complete.checked_add(1),
        3 => complete.checked_add(2),
        _ => None,
    }
}

fn inline_base64_payload(value: &str) -> &str {
    let trimmed = value.trim_start_matches(|ch: char| ch.is_ascii_whitespace());
    let Some(rest) = trimmed.strip_prefix("data:") else {
        return value;
    };
    let Some((metadata, payload)) = rest.split_once(',') else {
        return value;
    };
    metadata
        .split(';')
        .any(|part| part.trim().eq_ignore_ascii_case("base64"))
        .then_some(payload)
        .unwrap_or(value)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct OversizedImageViolation {
    current_images: usize,
    current_image_bytes: usize,
    historical_images: usize,
    historical_image_bytes: usize,
}

impl OversizedImageViolation {
    fn has_images(self) -> bool {
        self.current_images > 0 || self.historical_images > 0
    }
}

fn reject_oversized_images_if_configured(
    config: PayloadShapingConfig,
    violation: OversizedImageViolation,
    max_source_bytes: usize,
) -> Result<(), PayloadGuardError> {
    if config.oversized_image_handling != OversizedImageHandling::Reject || !violation.has_images()
    {
        return Ok(());
    }
    Err(PayloadGuardError::OversizedImage {
        current_images: violation.current_images,
        current_image_bytes: violation.current_image_bytes,
        historical_images: violation.historical_images,
        historical_image_bytes: violation.historical_image_bytes,
        max_source_bytes,
    })
}

fn find_oversized_kiro_images(
    request: &KiroRequest,
    max_source_bytes: usize,
) -> OversizedImageViolation {
    if max_source_bytes == 0 {
        return OversizedImageViolation::default();
    }

    let mut violation = OversizedImageViolation::default();
    for image in &request
        .conversation_state
        .current_message
        .user_input_message
        .images
    {
        let bytes = kiro_image_source_bytes(image);
        if bytes > max_source_bytes {
            violation.current_images += 1;
            violation.current_image_bytes += bytes;
        }
    }
    for message in &request.conversation_state.history {
        let Message::User(user) = message else {
            continue;
        };
        for image in &user.user_input_message.images {
            let bytes = kiro_image_source_bytes(image);
            if bytes > max_source_bytes {
                violation.historical_images += 1;
                violation.historical_image_bytes += bytes;
            }
        }
    }
    violation
}

fn drop_oversized_history_images(
    history: &mut [Message],
    max_source_bytes: usize,
) -> (usize, usize) {
    if max_source_bytes == 0 {
        return (0, 0);
    }

    let mut dropped = 0usize;
    let mut dropped_bytes = 0usize;
    for message in history {
        let Message::User(user) = message else {
            continue;
        };
        let images = &mut user.user_input_message.images;
        if images.is_empty() {
            continue;
        }
        let before = images.len();
        images.retain(|image| {
            let bytes = kiro_image_source_bytes(image);
            if bytes > max_source_bytes {
                dropped += 1;
                dropped_bytes += bytes;
                false
            } else {
                true
            }
        });
        let removed = before.saturating_sub(images.len());
        if removed > 0 {
            let note = if removed == 1 {
                "[Historical image was omitted because it exceeded the upstream 5 MB image size limit.]"
            } else {
                "[Historical images were omitted because they exceeded the upstream 5 MB image size limit.]"
            };
            append_text(&mut user.user_input_message.content, note);
        }
    }
    (dropped, dropped_bytes)
}

#[derive(Default)]
struct ShapingStats {
    truncated_history_tool_results: usize,
    truncated_history_tool_result_chars: usize,
    removed_history_thinking_blocks: usize,
    removed_history_thinking_chars: usize,
    dropped_historical_images: usize,
    dropped_historical_image_bytes: usize,
    trimmed_web_fetch_blocks: usize,
    trimmed_web_fetch_chars: usize,
    compressed_tool_definitions: usize,
    compressed_tool_definition_bytes: usize,
}

impl ShapingStats {
    fn was_modified(&self) -> bool {
        self.truncated_history_tool_results > 0
            || self.removed_history_thinking_blocks > 0
            || self.dropped_historical_images > 0
            || self.trimmed_web_fetch_blocks > 0
            || self.compressed_tool_definitions > 0
    }
}

fn add_shaping_stats_to_report(report: &mut PayloadGuardReport, shaping: ShapingStats) {
    report.truncated_history_tool_results += shaping.truncated_history_tool_results;
    report.truncated_history_tool_result_chars += shaping.truncated_history_tool_result_chars;
    report.removed_history_thinking_blocks += shaping.removed_history_thinking_blocks;
    report.removed_history_thinking_chars += shaping.removed_history_thinking_chars;
    report.dropped_historical_images += shaping.dropped_historical_images;
    report.dropped_historical_image_bytes += shaping.dropped_historical_image_bytes;
    report.trimmed_web_fetch_blocks += shaping.trimmed_web_fetch_blocks;
    report.trimmed_web_fetch_chars += shaping.trimmed_web_fetch_chars;
    report.compressed_tool_definitions += shaping.compressed_tool_definitions;
    report.compressed_tool_definition_bytes += shaping.compressed_tool_definition_bytes;
}

fn add_current_shaping_stats_to_report(
    report: &mut PayloadGuardReport,
    current: &CurrentShapingStats,
) {
    report.truncated_current_tool_results += current.truncated_current_tool_results;
    report.truncated_current_tool_result_chars += current.truncated_current_tool_result_chars;
    report.truncated_current_documents += current.truncated_current_documents;
    report.truncated_current_document_chars += current.truncated_current_document_chars;
    report.truncated_current_user_content += current.truncated_current_user_content;
    report.truncated_current_user_content_chars += current.truncated_current_user_content_chars;
    report.dropped_current_images += current.dropped_current_images;
    report.dropped_current_image_bytes += current.dropped_current_image_bytes;
}

fn apply_payload_safety_shaping(
    request: &mut KiroRequest,
    config: PayloadShapingConfig,
) -> ShapingStats {
    let mut stats = ShapingStats::default();
    if config.oversized_image_handling != OversizedImageHandling::DropWithPlaceholder {
        return stats;
    }
    let result = drop_oversized_history_images(
        &mut request.conversation_state.history,
        UPSTREAM_IMAGE_SOURCE_MAX_BYTES,
    );
    stats.dropped_historical_images += result.0;
    stats.dropped_historical_image_bytes += result.1;
    stats
}

fn apply_anthropic_payload_safety_shaping(
    request: &mut MessagesRequest,
    config: PayloadShapingConfig,
) -> ShapingStats {
    let mut stats = ShapingStats::default();
    let history_end = request.messages.len().saturating_sub(1);

    if config.discard_historical_thinking {
        let protected_assistant = active_anthropic_tool_turn_assistant_index(&request.messages)
            .filter(|index| *index < history_end);
        let result = discard_anthropic_history_thinking(
            &mut request.messages[..history_end],
            protected_assistant,
        );
        stats.removed_history_thinking_blocks += result.0;
        stats.removed_history_thinking_chars += result.1;
    }

    if config.oversized_image_handling == OversizedImageHandling::DropWithPlaceholder {
        let result = drop_oversized_anthropic_history_images(
            &mut request.messages[..history_end],
            UPSTREAM_IMAGE_SOURCE_MAX_BYTES,
        );
        stats.dropped_historical_images += result.0;
        stats.dropped_historical_image_bytes += result.1;
    }
    stats
}

fn apply_current_payload_safety_shaping(
    request: &mut KiroRequest,
    config: PayloadShapingConfig,
) -> CurrentShapingStats {
    let mut stats = CurrentShapingStats::default();
    if config.oversized_image_handling != OversizedImageHandling::DropWithPlaceholder {
        return stats;
    }
    let result = drop_oversized_current_images(
        &mut request
            .conversation_state
            .current_message
            .user_input_message,
        UPSTREAM_IMAGE_SOURCE_MAX_BYTES,
    );
    stats.dropped_current_images += result.0;
    stats.dropped_current_image_bytes += result.1;
    stats
}

fn apply_anthropic_current_payload_safety_shaping(
    request: &mut MessagesRequest,
    config: PayloadShapingConfig,
) -> CurrentShapingStats {
    let mut stats = CurrentShapingStats::default();
    if config.oversized_image_handling != OversizedImageHandling::DropWithPlaceholder {
        return stats;
    }
    let result = drop_oversized_anthropic_current_images(request, UPSTREAM_IMAGE_SOURCE_MAX_BYTES);
    stats.dropped_current_images += result.0;
    stats.dropped_current_image_bytes += result.1;
    stats
}

pub fn sanitize_anthropic_messages_for_external_forwarding(
    request: &mut MessagesRequest,
    config: PayloadShapingConfig,
) -> bool {
    apply_anthropic_payload_safety_shaping(request, config).was_modified()
}

fn apply_payload_shaping(request: &mut KiroRequest, config: PayloadShapingConfig) -> ShapingStats {
    let mut stats = ShapingStats::default();

    if config.truncate_historical_tool_results {
        let result = truncate_history_tool_results(
            &mut request.conversation_state.history,
            config.historical_tool_result_max_chars,
            config.historical_tool_result_head_lines,
            config.historical_tool_result_tail_lines,
        );
        stats.truncated_history_tool_results += result.0;
        stats.truncated_history_tool_result_chars += result.1;
    }

    if config.web_fetch_trim_enabled {
        let result = trim_history_web_fetch_content(
            &mut request.conversation_state.history,
            config.web_fetch_body_max_chars,
        );
        stats.trimmed_web_fetch_blocks += result.0;
        stats.trimmed_web_fetch_chars += result.1;
    }

    if config.discard_historical_thinking {
        let protected_assistant = active_kiro_tool_turn_assistant_index(
            &request.conversation_state.history,
            &request
                .conversation_state
                .current_message
                .user_input_message
                .user_input_message_context
                .tool_results,
        );
        let result =
            discard_history_thinking(&mut request.conversation_state.history, protected_assistant);
        stats.removed_history_thinking_blocks += result.0;
        stats.removed_history_thinking_chars += result.1;
    }

    if config.compress_tool_definitions && config.tool_definitions_budget_bytes > 0 {
        let before = json_len(
            &request
                .conversation_state
                .current_message
                .user_input_message
                .user_input_message_context
                .tools,
        );
        if before > config.tool_definitions_budget_bytes {
            let compressed = compress_tool_definitions(
                &mut request
                    .conversation_state
                    .current_message
                    .user_input_message
                    .user_input_message_context
                    .tools,
                config.tool_definitions_budget_bytes,
                config.tool_description_max_chars,
                config.tool_schema_annotation_max_chars,
            );
            let after = json_len(
                &request
                    .conversation_state
                    .current_message
                    .user_input_message
                    .user_input_message_context
                    .tools,
            );
            if compressed > 0 || after < before {
                stats.compressed_tool_definitions += compressed;
                stats.compressed_tool_definition_bytes += before.saturating_sub(after);
            }
        }
    }

    stats
}

fn apply_anthropic_payload_shaping(
    request: &mut MessagesRequest,
    config: PayloadShapingConfig,
) -> ShapingStats {
    let mut stats = ShapingStats::default();
    let history_end = request.messages.len().saturating_sub(1);

    if config.truncate_historical_tool_results {
        let result = truncate_anthropic_history_tool_results(
            &mut request.messages[..history_end],
            config.historical_tool_result_max_chars,
            config.historical_tool_result_head_lines,
            config.historical_tool_result_tail_lines,
        );
        stats.truncated_history_tool_results += result.0;
        stats.truncated_history_tool_result_chars += result.1;
    }

    if config.web_fetch_trim_enabled {
        let result = trim_anthropic_history_web_fetch_content(
            &mut request.messages[..history_end],
            config.web_fetch_body_max_chars,
        );
        stats.trimmed_web_fetch_blocks += result.0;
        stats.trimmed_web_fetch_chars += result.1;
    }

    if config.discard_historical_thinking {
        let protected_assistant = active_anthropic_tool_turn_assistant_index(&request.messages)
            .filter(|index| *index < history_end);
        let result = discard_anthropic_history_thinking(
            &mut request.messages[..history_end],
            protected_assistant,
        );
        stats.removed_history_thinking_blocks += result.0;
        stats.removed_history_thinking_chars += result.1;
    }

    if config.compress_tool_definitions && config.tool_definitions_budget_bytes > 0 {
        if let Some(tools) = request.tools.as_mut() {
            let before = json_len(tools);
            if before > config.tool_definitions_budget_bytes {
                let compressed = compress_anthropic_tool_definitions(
                    tools,
                    config.tool_definitions_budget_bytes,
                    config.tool_description_max_chars,
                    config.tool_schema_annotation_max_chars,
                );
                let after = json_len(tools);
                if compressed > 0 || after < before {
                    stats.compressed_tool_definitions += compressed;
                    stats.compressed_tool_definition_bytes += before.saturating_sub(after);
                }
            }
        }
    }

    stats
}

fn truncate_anthropic_history_tool_results(
    messages: &mut [AnthropicMessage],
    max_chars: usize,
    head_lines: usize,
    tail_lines: usize,
) -> (usize, usize) {
    if max_chars == 0 {
        return (0, 0);
    }

    let mut truncated = 0usize;
    let mut omitted_chars = 0usize;
    for message in messages {
        if message.role != "user" {
            continue;
        }
        let Some(blocks) = content_blocks_mut(&mut message.content) else {
            continue;
        };
        for block in blocks {
            if block_type(block) != Some("tool_result") {
                continue;
            }
            let result = truncate_anthropic_tool_result_block(
                block,
                max_chars,
                head_lines,
                tail_lines,
                "historical tool result",
                true,
            );
            truncated += result.0;
            omitted_chars += result.1;
        }
    }
    (truncated, omitted_chars)
}

fn trim_anthropic_history_web_fetch_content(
    messages: &mut [AnthropicMessage],
    max_chars: usize,
) -> (usize, usize) {
    if max_chars == 0 {
        return (0, 0);
    }
    let mut blocks_trimmed = 0usize;
    let mut omitted_chars = 0usize;
    for message in messages {
        let Some(blocks) = content_blocks_mut(&mut message.content) else {
            continue;
        };
        for block in blocks {
            if block_type(block) != Some("tool_result") {
                continue;
            }
            let result = trim_anthropic_tool_result_web_fetch_block(block, max_chars);
            blocks_trimmed += result.0;
            omitted_chars += result.1;
        }
    }
    (blocks_trimmed, omitted_chars)
}

fn discard_anthropic_history_thinking(
    messages: &mut [AnthropicMessage],
    protected_assistant: Option<usize>,
) -> (usize, usize) {
    let mut removed_blocks = 0usize;
    let mut removed_chars = 0usize;
    for (index, message) in messages.iter_mut().enumerate() {
        if message.role != "assistant" {
            continue;
        }
        if protected_assistant == Some(index) {
            continue;
        }
        if let Some(text) = message.content.as_str() {
            let (cleaned, blocks, chars) = remove_tagged_blocks(text, "thinking");
            if blocks > 0 {
                message.content = Value::String(cleaned);
                removed_blocks += blocks;
                removed_chars += chars;
            }
            continue;
        }
        let Some(blocks) = content_blocks_mut(&mut message.content) else {
            continue;
        };
        let before = blocks.len();
        let mut chars = 0usize;
        blocks.retain(|block| {
            let is_thinking = is_anthropic_reasoning_content_block(block);
            if is_thinking {
                chars += json_len(block);
            }
            !is_thinking
        });
        let removed = before.saturating_sub(blocks.len());
        if removed > 0 && blocks.is_empty() {
            blocks.push(serde_json::json!({"type": "text", "text": " "}));
        }
        removed_blocks += removed;
        removed_chars += chars;
    }
    (removed_blocks, removed_chars)
}

fn anthropic_image_source_bytes(block: &Value) -> usize {
    let Some(source) = block.get("source").and_then(Value::as_object) else {
        return json_len(block);
    };
    for key in ["data", "base64", "bytes"] {
        if let Some(value) = source.get(key).and_then(Value::as_str) {
            return decoded_base64_source_bytes(value).unwrap_or(value.len());
        }
    }
    json_len(block)
}

fn count_oversized_anthropic_content_images(
    content: &Value,
    max_source_bytes: usize,
) -> (usize, usize) {
    if max_source_bytes == 0 {
        return (0, 0);
    }
    let mut count = 0usize;
    let mut bytes_total = 0usize;
    for block in content_blocks_by_type(content, "image") {
        let bytes = anthropic_image_source_bytes(block);
        if bytes > max_source_bytes {
            count += 1;
            bytes_total += bytes;
        }
    }
    (count, bytes_total)
}

fn find_oversized_anthropic_images(
    request: &MessagesRequest,
    max_source_bytes: usize,
) -> OversizedImageViolation {
    if max_source_bytes == 0 {
        return OversizedImageViolation::default();
    }

    let mut violation = OversizedImageViolation::default();
    let history_end = request.messages.len().saturating_sub(1);
    for message in request.messages.iter().take(history_end) {
        let (count, bytes) =
            count_oversized_anthropic_content_images(&message.content, max_source_bytes);
        violation.historical_images += count;
        violation.historical_image_bytes += bytes;
    }
    if let Some(message) = request.messages.last() {
        let (count, bytes) =
            count_oversized_anthropic_content_images(&message.content, max_source_bytes);
        violation.current_images += count;
        violation.current_image_bytes += bytes;
    }
    violation
}

fn oversized_historical_image_placeholder(dropped: usize) -> Value {
    let text = if dropped == 1 {
        "[Historical image was omitted because it exceeded the upstream 5 MB image size limit.]"
    } else {
        "[Historical images were omitted because they exceeded the upstream 5 MB image size limit.]"
    };
    serde_json::json!({
        "type": "text",
        "text": text
    })
}

fn drop_oversized_anthropic_history_images(
    messages: &mut [AnthropicMessage],
    max_source_bytes: usize,
) -> (usize, usize) {
    if max_source_bytes == 0 {
        return (0, 0);
    }

    let mut dropped = 0usize;
    let mut dropped_bytes = 0usize;
    for message in messages {
        let Some(blocks) = content_blocks_mut(&mut message.content) else {
            continue;
        };
        let mut retained = Vec::with_capacity(blocks.len());
        let mut placeholder_index = None;
        let mut message_dropped = 0usize;
        for block in std::mem::take(blocks) {
            let bytes =
                (block_type(&block) == Some("image")).then(|| anthropic_image_source_bytes(&block));
            if bytes.is_some_and(|bytes| bytes > max_source_bytes) {
                placeholder_index.get_or_insert(retained.len());
                message_dropped += 1;
                dropped += 1;
                dropped_bytes += bytes.unwrap_or_default();
            } else {
                retained.push(block);
            }
        }
        if let Some(index) = placeholder_index {
            retained.insert(
                index,
                oversized_historical_image_placeholder(message_dropped),
            );
        }
        *blocks = retained;
    }
    (dropped, dropped_bytes)
}

fn compress_anthropic_tool_definitions(
    tools: &mut [AnthropicTool],
    budget_bytes: usize,
    description_max_chars: usize,
    annotation_max_chars: usize,
) -> usize {
    if tools.is_empty() {
        return 0;
    }

    let adaptive_description_max = if budget_bytes > 0 {
        description_max_chars.min((budget_bytes / tools.len()).max(256))
    } else {
        description_max_chars
    };

    let mut changed = 0usize;
    for tool in tools.iter_mut() {
        if truncate_string_field(
            &mut tool.description,
            adaptive_description_max,
            "tool description",
        ) {
            changed += 1;
        }
        let mut schema = serde_json::to_value(&tool.input_schema).unwrap_or(Value::Null);
        let schema_changes = truncate_schema_annotations(&mut schema, annotation_max_chars);
        if schema_changes > 0 {
            if let Ok(next_schema) = serde_json::from_value(schema) {
                tool.input_schema = next_schema;
                changed += schema_changes;
            }
        }
    }

    if budget_bytes > 0 && json_len(tools) > budget_bytes {
        let hard_description_max = adaptive_description_max.min(512);
        for tool in tools.iter_mut() {
            if truncate_string_field(
                &mut tool.description,
                hard_description_max,
                "tool description",
            ) {
                changed += 1;
            }
        }
    }

    changed
}

fn truncate_anthropic_tool_result_block(
    block: &mut Value,
    max_chars: usize,
    head_lines: usize,
    tail_lines: usize,
    label: &str,
    skip_web_fetch: bool,
) -> (usize, usize) {
    let Some(content) = block.get_mut("content") else {
        return (0, 0);
    };
    truncate_anthropic_content_texts(
        content,
        max_chars,
        head_lines,
        tail_lines,
        label,
        skip_web_fetch,
    )
}

fn truncate_anthropic_content_texts(
    value: &mut Value,
    max_chars: usize,
    head_lines: usize,
    tail_lines: usize,
    label: &str,
    skip_web_fetch: bool,
) -> (usize, usize) {
    if max_chars == 0 {
        return (0, 0);
    }
    if let Some(text) = value.as_str().map(str::to_string) {
        if skip_web_fetch && looks_like_web_fetch_text(&text) {
            return (0, 0);
        }
        let original_chars = text.chars().count();
        if original_chars <= max_chars {
            return (0, 0);
        }
        let replacement = truncate_text_head_tail(&text, max_chars, head_lines, tail_lines, label);
        let replacement_chars = replacement.chars().count();
        *value = Value::String(replacement);
        return (1, original_chars.saturating_sub(replacement_chars));
    }
    if let Some(items) = value.as_array_mut() {
        let mut truncated = 0usize;
        let mut omitted = 0usize;
        for item in items {
            if let Some(text) = item.as_str().map(str::to_string) {
                if skip_web_fetch && looks_like_web_fetch_text(&text) {
                    continue;
                }
                let original_chars = text.chars().count();
                if original_chars > max_chars {
                    let replacement =
                        truncate_text_head_tail(&text, max_chars, head_lines, tail_lines, label);
                    let replacement_chars = replacement.chars().count();
                    *item = Value::String(replacement);
                    truncated += 1;
                    omitted += original_chars.saturating_sub(replacement_chars);
                }
                continue;
            }
            let Some(text_value) = item.get_mut("text") else {
                continue;
            };
            let result = truncate_anthropic_content_texts(
                text_value,
                max_chars,
                head_lines,
                tail_lines,
                label,
                skip_web_fetch,
            );
            truncated += result.0;
            omitted += result.1;
        }
        return (truncated, omitted);
    }
    (0, 0)
}

fn trim_anthropic_tool_result_web_fetch_block(
    block: &mut Value,
    max_chars: usize,
) -> (usize, usize) {
    let Some(content) = block.get_mut("content") else {
        return (0, 0);
    };
    trim_anthropic_web_fetch_texts(content, max_chars)
}

fn trim_anthropic_web_fetch_texts(value: &mut Value, max_chars: usize) -> (usize, usize) {
    if let Some(text) = value.as_str().map(str::to_string) {
        let (trimmed, omitted, changed) = trim_web_fetch_text(&text, max_chars);
        if changed {
            *value = Value::String(trimmed);
            return (1, omitted);
        }
        return (0, 0);
    }
    if let Some(items) = value.as_array_mut() {
        let mut trimmed = 0usize;
        let mut omitted = 0usize;
        for item in items {
            if let Some(text) = item.as_str().map(str::to_string) {
                let (replacement, chars, changed) = trim_web_fetch_text(&text, max_chars);
                if changed {
                    *item = Value::String(replacement);
                    trimmed += 1;
                    omitted += chars;
                }
                continue;
            }
            let Some(text_value) = item.get_mut("text") else {
                continue;
            };
            let result = trim_anthropic_web_fetch_texts(text_value, max_chars);
            trimmed += result.0;
            omitted += result.1;
        }
        return (trimmed, omitted);
    }
    (0, 0)
}

#[derive(Default)]
struct AnthropicRepairStats {
    normalized_empty_tool_results: usize,
    aligned_leading_entries: usize,
    removed_orphan_tool_results: usize,
    removed_duplicate_tool_results: usize,
    removed_orphan_tool_uses: usize,
}

impl AnthropicRepairStats {
    fn was_modified(&self) -> bool {
        self.normalized_empty_tool_results > 0
            || self.aligned_leading_entries > 0
            || self.removed_orphan_tool_results > 0
            || self.removed_duplicate_tool_results > 0
            || self.removed_orphan_tool_uses > 0
    }
}

fn repair_anthropic_messages(request: &mut MessagesRequest) -> AnthropicRepairStats {
    let mut stats = AnthropicRepairStats::default();
    stats.normalized_empty_tool_results +=
        normalize_empty_anthropic_tool_result_contents(&mut request.messages);
    stats.aligned_leading_entries += align_anthropic_messages_to_user(&mut request.messages);
    let result = filter_invalid_anthropic_tool_results(&mut request.messages);
    stats.removed_orphan_tool_results += result.removed_orphans;
    stats.removed_duplicate_tool_results += result.removed_duplicates;
    stats.removed_orphan_tool_uses += remove_unpaired_anthropic_tool_uses(&mut request.messages);
    stats
}

fn add_anthropic_repair_stats_to_report(
    report: &mut PayloadGuardReport,
    repair: AnthropicRepairStats,
) {
    report.aligned_leading_entries += repair.aligned_leading_entries;
    report.removed_orphan_tool_results += repair.removed_orphan_tool_results;
    report.removed_duplicate_tool_results += repair.removed_duplicate_tool_results;
    report.removed_orphan_tool_uses += repair.removed_orphan_tool_uses;
}

fn normalize_empty_anthropic_tool_result_contents(messages: &mut [AnthropicMessage]) -> usize {
    let mut normalized = 0usize;
    for message in messages {
        if message.role != "user" {
            continue;
        }
        let Some(blocks) = content_blocks_mut(&mut message.content) else {
            continue;
        };
        for block in blocks {
            if block_type(block) != Some("tool_result") {
                continue;
            }
            if anthropic_tool_result_has_non_empty_content(block) {
                continue;
            }
            if let Some(obj) = block.as_object_mut() {
                obj.insert(
                    "content".to_string(),
                    serde_json::json!([{
                        "type": "text",
                        "text": EMPTY_TOOL_RESULT_CONTENT_PLACEHOLDER
                    }]),
                );
                normalized += 1;
            }
        }
    }
    normalized
}

fn anthropic_tool_result_has_non_empty_content(block: &Value) -> bool {
    match block.get("content") {
        Some(Value::String(text)) => !text.trim().is_empty(),
        Some(Value::Array(items)) => items.iter().any(anthropic_tool_result_item_has_content),
        Some(Value::Null) | None => false,
        Some(Value::Object(obj)) => !obj.is_empty(),
        Some(_) => true,
    }
}

fn anthropic_tool_result_item_has_content(item: &Value) -> bool {
    if let Some(text) = item.as_str() {
        return !text.trim().is_empty();
    }
    if let Some(text) = item.get("text").and_then(Value::as_str) {
        if !text.trim().is_empty() {
            return true;
        }
    }
    match item {
        Value::Null => false,
        Value::Object(obj) => obj
            .iter()
            .any(|(key, value)| key != "type" && key != "text" && !value.is_null()),
        _ => true,
    }
}

fn align_anthropic_messages_to_user(messages: &mut Vec<AnthropicMessage>) -> usize {
    let removed = messages
        .iter()
        .take(messages.len().saturating_sub(1))
        .take_while(|message| message.role == "assistant")
        .count();
    if removed > 0 {
        messages.drain(..removed);
    }
    removed
}

#[derive(Default)]
struct AnthropicToolResultFilterStats {
    removed_orphans: usize,
    removed_duplicates: usize,
}

fn filter_invalid_anthropic_tool_results(
    messages: &mut [AnthropicMessage],
) -> AnthropicToolResultFilterStats {
    let mut stats = AnthropicToolResultFilterStats::default();
    for idx in 0..messages.len() {
        if messages[idx].role != "user" {
            continue;
        }
        let valid_ids = if idx > 0 && messages[idx - 1].role == "assistant" {
            anthropic_tool_use_ids(&messages[idx - 1].content)
        } else {
            HashSet::new()
        };
        let Some(blocks) = content_blocks_mut(&mut messages[idx].content) else {
            continue;
        };
        let mut seen = HashSet::new();
        blocks.retain(|block| {
            if block_type(block) != Some("tool_result") {
                return true;
            }
            let id = block
                .get("tool_use_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !valid_ids.contains(id) {
                stats.removed_orphans += 1;
                return false;
            }
            if !seen.insert(id.to_string()) {
                stats.removed_duplicates += 1;
                return false;
            }
            true
        });
        if blocks.is_empty() {
            blocks.push(serde_json::json!({"type": "text", "text": " "}));
        }
    }
    stats
}

fn remove_unpaired_anthropic_tool_uses(messages: &mut [AnthropicMessage]) -> usize {
    let mut removed = 0usize;
    for idx in 0..messages.len() {
        if messages[idx].role != "assistant" {
            continue;
        }
        let paired_ids = messages
            .get(idx + 1)
            .filter(|message| message.role == "user")
            .map(|message| anthropic_tool_result_ids(&message.content))
            .unwrap_or_default();
        let Some(blocks) = content_blocks_mut(&mut messages[idx].content) else {
            continue;
        };
        let before = blocks.len();
        blocks.retain(|block| {
            if block_type(block) != Some("tool_use") {
                return true;
            }
            block
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| paired_ids.contains(id))
        });
        let delta = before.saturating_sub(blocks.len());
        removed += delta;
        if delta > 0 && blocks.is_empty() {
            blocks.push(serde_json::json!({"type": "text", "text": " "}));
        }
    }
    removed
}

fn anthropic_tool_use_ids(content: &Value) -> HashSet<String> {
    content_blocks(content)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| block_type(block) == Some("tool_use"))
                .filter_map(|block| block.get("id").and_then(Value::as_str))
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn anthropic_tool_result_ids(content: &Value) -> HashSet<String> {
    content_blocks(content)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| block_type(block) == Some("tool_result"))
                .filter_map(|block| block.get("tool_use_id").and_then(Value::as_str))
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn trim_anthropic_history_to_estimated_budget(
    messages: &mut Vec<AnthropicMessage>,
    current_bytes: usize,
    target_bytes: usize,
) -> usize {
    if messages.len() <= 1 || current_bytes <= target_bytes {
        return 0;
    }

    let mut prefix_len = 0usize;
    let mut removed_item_bytes = 0usize;
    let mut estimated_bytes = current_bytes;
    while estimated_bytes > target_bytes && prefix_len + 1 < messages.len() {
        let Some(relative_end) =
            messages[prefix_len..]
                .iter()
                .enumerate()
                .skip(1)
                .find_map(|(idx, message)| {
                    (message.role == "user" && !has_anthropic_tool_result(&message.content))
                        .then_some(idx)
                })
        else {
            break;
        };
        let previous_prefix_len = prefix_len;
        prefix_len += relative_end;
        removed_item_bytes = removed_item_bytes.saturating_add(
            messages[previous_prefix_len..prefix_len]
                .iter()
                .map(json_len)
                .sum::<usize>(),
        );
        estimated_bytes =
            current_bytes.saturating_sub(json_array_prefix_reduction_from_item_bytes(
                messages.len(),
                prefix_len,
                removed_item_bytes,
            ));
    }

    if prefix_len > 0 {
        messages.drain(0..prefix_len);
    }
    prefix_len
}

fn has_anthropic_tool_result(content: &Value) -> bool {
    count_content_blocks_by_type(content, "tool_result") > 0
}

fn truncate_history_tool_results(
    history: &mut [Message],
    max_chars: usize,
    head_lines: usize,
    tail_lines: usize,
) -> (usize, usize) {
    if max_chars == 0 {
        return (0, 0);
    }

    let mut truncated = 0usize;
    let mut omitted_chars = 0usize;
    for message in history {
        let Message::User(user) = message else {
            continue;
        };
        for result in &mut user
            .user_input_message
            .user_input_message_context
            .tool_results
        {
            for item in &mut result.content {
                let Some(value) = item.get_mut("text") else {
                    continue;
                };
                let Some(text) = value.as_str() else {
                    continue;
                };
                if looks_like_web_fetch_text(text) {
                    continue;
                }
                if text.chars().count() <= max_chars {
                    continue;
                }
                let original_chars = text.chars().count();
                let replacement = truncate_text_head_tail(
                    text,
                    max_chars,
                    head_lines,
                    tail_lines,
                    "historical tool result",
                );
                let replacement_chars = replacement.chars().count();
                *value = serde_json::Value::String(replacement);
                truncated += 1;
                omitted_chars += original_chars.saturating_sub(replacement_chars);
            }
        }
    }
    (truncated, omitted_chars)
}

fn looks_like_web_fetch_text(text: &str) -> bool {
    web_fetch_body_range(text).is_some()
}

fn truncate_text_head_tail(
    text: &str,
    max_chars: usize,
    head_lines: usize,
    tail_lines: usize,
    label: &str,
) -> String {
    let original_chars = text.chars().count();
    if original_chars <= max_chars {
        return text.to_string();
    }

    let lines: Vec<&str> = text.lines().collect();
    let mut head = Vec::new();
    let mut tail = Vec::new();
    let mut used_chars = 0usize;
    let soft_budget = max_chars.saturating_sub(256).max(max_chars / 2);
    let head_budget = soft_budget.saturating_mul(2) / 3;
    let tail_budget = soft_budget.saturating_sub(head_budget);

    for line in lines.iter().take(head_lines) {
        let line_chars = line.chars().count().saturating_add(1);
        if !head.is_empty() && used_chars.saturating_add(line_chars) > head_budget {
            break;
        }
        used_chars += line_chars;
        head.push(*line);
    }

    let mut tail_chars = 0usize;
    for line in lines.iter().rev().take(tail_lines) {
        let line_chars = line.chars().count().saturating_add(1);
        if !tail.is_empty() && tail_chars.saturating_add(line_chars) > tail_budget {
            break;
        }
        tail_chars += line_chars;
        tail.push(*line);
    }
    tail.reverse();

    if head.is_empty() && tail.is_empty() {
        return format!(
            "[{} truncated by proxy: original_chars={}, preserved=0_chars]",
            label, original_chars
        );
    }

    let mut out = String::new();
    if !head.is_empty() {
        out.push_str(&head.join("\n"));
        out.push('\n');
    }
    out.push_str(&format!(
        "\n[{} truncated by proxy: original_chars={}, preserved=head:{}_lines,tail:{}_lines]\n\n",
        label,
        original_chars,
        head.len(),
        tail.len()
    ));
    if !tail.is_empty() {
        out.push_str(&tail.join("\n"));
    }

    fit_truncated_text_with_marker(&out, text, max_chars, label, original_chars)
}

fn safe_truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    text.chars().take(max_chars).collect()
}

fn fit_truncated_text_with_marker(
    candidate: &str,
    original: &str,
    max_chars: usize,
    label: &str,
    original_chars: usize,
) -> String {
    if candidate.chars().count() <= max_chars {
        return candidate.to_string();
    }

    let marker = format!(
        "\n\n[{} truncated by proxy: original_chars={}, preserved=head_tail_chars]\n\n",
        label, original_chars
    );
    let marker_chars = marker.chars().count();
    if marker_chars >= max_chars {
        return safe_truncate_chars(&marker, max_chars);
    }

    let text_budget = max_chars.saturating_sub(marker_chars);
    let head_budget = text_budget.saturating_mul(2) / 3;
    let tail_budget = text_budget.saturating_sub(head_budget);
    let head = original.chars().take(head_budget).collect::<String>();
    let tail = original
        .chars()
        .rev()
        .take(tail_budget)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    let mut out = String::with_capacity(max_chars);
    out.push_str(&head);
    out.push_str(&marker);
    out.push_str(&tail);
    if out.chars().count() > max_chars {
        safe_truncate_chars(&out, max_chars)
    } else {
        out
    }
}

fn active_kiro_tool_turn_assistant_index(
    history: &[Message],
    current_results: &[ToolResult],
) -> Option<usize> {
    if current_results.is_empty() {
        return None;
    }
    let assistant_index = history.len().checked_sub(1)?;
    let Message::Assistant(assistant) = history.get(assistant_index)? else {
        return None;
    };
    let tool_uses = assistant.assistant_response_message.tool_uses.as_ref()?;
    if tool_uses.is_empty() {
        return None;
    }
    let result_ids = current_results
        .iter()
        .map(|result| result.tool_use_id.as_str())
        .collect::<HashSet<_>>();
    tool_uses
        .iter()
        .map(|tool_use| tool_use.tool_use_id.as_str())
        .all(|id| result_ids.contains(id))
        .then_some(assistant_index)
}

fn discard_history_thinking(
    history: &mut [Message],
    protected_assistant: Option<usize>,
) -> (usize, usize) {
    let mut blocks = 0usize;
    let mut chars = 0usize;
    for (index, message) in history.iter_mut().enumerate() {
        let Message::Assistant(assistant) = message else {
            continue;
        };
        if protected_assistant == Some(index) {
            continue;
        }
        if let Some(reasoning_content) = assistant
            .assistant_response_message
            .reasoning_content
            .take()
        {
            blocks = blocks.saturating_add(1);
            chars = chars.saturating_add(json_len(&reasoning_content));
        }
        let (cleaned, removed_blocks, removed_chars) =
            remove_tagged_blocks(&assistant.assistant_response_message.content, "thinking");
        if removed_blocks > 0 {
            assistant.assistant_response_message.content = cleaned;
            blocks += removed_blocks;
            chars += removed_chars;
        }
    }
    (blocks, chars)
}

fn remove_tagged_blocks(text: &str, tag: &str) -> (String, usize, usize) {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let mut remaining = text;
    let mut output = String::with_capacity(text.len());
    let mut blocks = 0usize;
    let mut removed_chars = 0usize;

    while let Some(start) = remaining.find(&open) {
        output.push_str(&remaining[..start]);
        let after_open = start + open.len();
        if let Some(close_offset) = remaining[after_open..].find(&close) {
            let end = after_open + close_offset + close.len();
            removed_chars += remaining[start..end].chars().count();
            blocks += 1;
            remaining = &remaining[end..];
        } else {
            removed_chars += remaining[start..].chars().count();
            blocks += 1;
            remaining = "";
            break;
        }
    }
    output.push_str(remaining);
    (collapse_excess_blank_lines(&output), blocks, removed_chars)
}

fn collapse_excess_blank_lines(text: &str) -> String {
    let mut output = String::new();
    let mut blank_count = 0usize;
    for line in text.lines() {
        if line.trim().is_empty() {
            blank_count += 1;
            if blank_count <= 2 {
                output.push('\n');
            }
        } else {
            blank_count = 0;
            if !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            output.push_str(line);
            output.push('\n');
        }
    }
    output.trim().to_string()
}

fn trim_history_web_fetch_content(history: &mut [Message], max_chars: usize) -> (usize, usize) {
    if max_chars == 0 {
        return (0, 0);
    }

    let mut blocks = 0usize;
    let mut chars = 0usize;
    for message in history {
        let Message::User(user) = message else {
            continue;
        };
        let (trimmed, omitted, changed) =
            trim_web_fetch_text(&user.user_input_message.content, max_chars);
        if changed {
            user.user_input_message.content = trimmed;
            blocks += 1;
            chars += omitted;
        }
        for result in &mut user
            .user_input_message
            .user_input_message_context
            .tool_results
        {
            for item in &mut result.content {
                let Some(value) = item.get_mut("text") else {
                    continue;
                };
                let Some(text) = value.as_str() else {
                    continue;
                };
                let (trimmed, omitted, changed) = trim_web_fetch_text(text, max_chars);
                if changed {
                    *value = serde_json::Value::String(trimmed);
                    blocks += 1;
                    chars += omitted;
                }
            }
        }
    }
    (blocks, chars)
}

fn trim_web_fetch_text(text: &str, max_chars: usize) -> (String, usize, bool) {
    let Some((body_start, body_end, separator)) = web_fetch_body_range(text) else {
        return (text.to_string(), 0, false);
    };

    let body = &text[body_start..body_end];
    if body.chars().count() <= max_chars {
        return (text.to_string(), 0, false);
    }
    let compact = compact_web_fetch_body(body, max_chars);
    if compact.chars().count() >= body.chars().count().saturating_sub(1_000) {
        return (text.to_string(), 0, false);
    }

    let omitted = body.chars().count().saturating_sub(compact.chars().count());
    let mut output = String::with_capacity(text.len().saturating_sub(omitted));
    output.push_str(&text[..body_start]);
    output.push_str(&compact);
    output.push_str("\n\n[Proxy note: web page navigation, repeated links, and image data were trimmed before upstream processing.]");
    output.push_str(separator);
    output.push_str(&text[body_end + separator.len()..]);
    (output, omitted, true)
}

fn web_fetch_body_range(text: &str) -> Option<(usize, usize, &'static str)> {
    let markers = ["Web page content:\n---\n", "Web page content:\r\n---\r\n"];
    let (marker_start, marker) = markers
        .iter()
        .filter_map(|marker| text.find(marker).map(|idx| (idx, *marker)))
        .min_by_key(|(idx, _)| *idx)?;

    let body_start = marker_start + marker.len();
    let rest = &text[body_start..];
    let separators = ["\n---\n\n", "\n---\n", "\r\n---\r\n\r\n", "\r\n---\r\n"];
    let (body_end_rel, separator) = separators
        .iter()
        .filter_map(|separator| rest.rfind(separator).map(|idx| (idx, *separator)))
        .max_by_key(|(idx, _)| *idx)?;

    Some((body_start, body_start + body_end_rel, separator))
}

fn compact_web_fetch_body(body: &str, max_chars: usize) -> String {
    let mut kept = Vec::new();
    let mut seen = HashSet::new();
    let mut chars = 0usize;
    let mut blank = false;
    for raw_line in body.lines() {
        let mut line = raw_line.trim();
        if line.starts_with("![") && line.contains("](data:image/") {
            continue;
        }
        if line.is_empty() {
            if blank {
                continue;
            }
            blank = true;
        } else {
            blank = false;
        }
        if line.len() > 500 {
            line = safe_prefix_by_bytes(line, 500);
        }
        let normalized = line.to_ascii_lowercase();
        if !normalized.is_empty() && !seen.insert(normalized) {
            continue;
        }
        let line_chars = line.chars().count().saturating_add(1);
        if chars.saturating_add(line_chars) > max_chars {
            break;
        }
        chars += line_chars;
        kept.push(line.to_string());
    }
    kept.join("\n")
}

fn safe_prefix_by_bytes(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = 0usize;
    for (idx, ch) in text.char_indices() {
        let next = idx + ch.len_utf8();
        if next > max_bytes {
            break;
        }
        end = next;
    }
    &text[..end]
}

fn compress_tool_definitions(
    tools: &mut [Tool],
    budget_bytes: usize,
    description_max_chars: usize,
    annotation_max_chars: usize,
) -> usize {
    if tools.is_empty() {
        return 0;
    }

    let adaptive_description_max = if budget_bytes > 0 {
        description_max_chars.min((budget_bytes / tools.len()).max(256))
    } else {
        description_max_chars
    };

    let mut changed = 0usize;
    for tool in tools.iter_mut() {
        let spec = &mut tool.tool_specification;
        if truncate_string_field(
            &mut spec.description,
            adaptive_description_max,
            "tool description",
        ) {
            changed += 1;
        }
        if truncate_schema_annotations(&mut spec.input_schema.json, annotation_max_chars) > 0 {
            changed += 1;
        }
    }

    if budget_bytes > 0 && json_len(tools) > budget_bytes {
        let hard_description_max = adaptive_description_max.min(512);
        for tool in tools.iter_mut() {
            let spec = &mut tool.tool_specification;
            if truncate_string_field(
                &mut spec.description,
                hard_description_max,
                "tool description",
            ) {
                changed += 1;
            }
        }
    }

    changed
}

fn truncate_string_field(value: &mut String, max_chars: usize, label: &str) -> bool {
    if max_chars == 0 || value.chars().count() <= max_chars {
        return false;
    }
    let original_chars = value.chars().count();
    let keep = max_chars.saturating_sub(96).max(max_chars / 2);
    let prefix: String = value.chars().take(keep).collect();
    *value = format!(
        "{}\n[{} truncated by proxy: original_chars={}]",
        prefix, label, original_chars
    );
    true
}

fn truncate_schema_annotations(value: &mut serde_json::Value, max_chars: usize) -> usize {
    if max_chars == 0 {
        return 0;
    }

    match value {
        serde_json::Value::Object(map) => {
            let mut changed = 0usize;
            for key in ["description", "title", "$comment", "examples"] {
                if let Some(item) = map.get_mut(key) {
                    changed += truncate_annotation_value(item, max_chars);
                }
            }
            for item in map.values_mut() {
                changed += truncate_schema_annotations(item, max_chars);
            }
            changed
        }
        serde_json::Value::Array(items) => items
            .iter_mut()
            .map(|item| truncate_schema_annotations(item, max_chars))
            .sum(),
        _ => 0,
    }
}

fn truncate_annotation_value(value: &mut serde_json::Value, max_chars: usize) -> usize {
    match value {
        serde_json::Value::String(text) => {
            let mut owned = std::mem::take(text);
            let changed = truncate_string_field(&mut owned, max_chars, "schema annotation");
            *text = owned;
            usize::from(changed)
        }
        serde_json::Value::Array(items) => {
            let before = items.len();
            if items.len() > 3 {
                items.truncate(3);
            }
            let nested: usize = items
                .iter_mut()
                .map(|item| truncate_annotation_value(item, max_chars))
                .sum();
            nested + usize::from(items.len() != before)
        }
        serde_json::Value::Object(_) => truncate_schema_annotations(value, max_chars),
        _ => 0,
    }
}

#[derive(Default)]
struct CurrentShapingStats {
    truncated_current_tool_results: usize,
    truncated_current_tool_result_chars: usize,
    truncated_current_documents: usize,
    truncated_current_document_chars: usize,
    truncated_current_user_content: usize,
    truncated_current_user_content_chars: usize,
    dropped_current_images: usize,
    dropped_current_image_bytes: usize,
}

impl CurrentShapingStats {
    fn was_modified(&self) -> bool {
        self.truncated_current_tool_results > 0
            || self.truncated_current_tool_result_chars > 0
            || self.truncated_current_documents > 0
            || self.truncated_current_document_chars > 0
            || self.truncated_current_user_content > 0
            || self.truncated_current_user_content_chars > 0
            || self.dropped_current_images > 0
            || self.dropped_current_image_bytes > 0
    }
}

fn current_payload_shaping_enabled(config: PayloadShapingConfig) -> bool {
    config.fit_current_payload_to_budget
        || config.truncate_current_tool_results
        || config.truncate_current_user_content
        || config.truncate_current_documents
        || config.truncate_current_images
}

fn apply_current_payload_shaping_until_fit(
    request: &mut KiroRequest,
    config: PayloadShapingConfig,
    max_bytes: usize,
    body: String,
    serialize_elapsed: &mut Duration,
    guard_serializations: &mut usize,
) -> Result<(String, CurrentShapingStats), PayloadGuardError> {
    let mut body = body;
    let mut stats = CurrentShapingStats::default();

    if body.len() <= max_bytes {
        return Ok((body, stats));
    }

    let mut tool_budget = normalize_current_text_budget(config.current_tool_result_max_chars);
    let mut document_budget = normalize_current_text_budget(config.current_document_max_chars);
    let mut user_content_budget =
        normalize_current_text_budget(config.current_user_content_max_chars);

    let truncate_tool_results =
        config.fit_current_payload_to_budget || config.truncate_current_tool_results;
    let truncate_documents =
        config.fit_current_payload_to_budget || config.truncate_current_documents;
    let truncate_user_content =
        config.fit_current_payload_to_budget || config.truncate_current_user_content;
    let truncate_images = config.fit_current_payload_to_budget || config.truncate_current_images;

    if truncate_tool_results {
        let changed = truncate_current_tool_results(request, tool_budget);
        stats.truncated_current_tool_results += changed.0;
        stats.truncated_current_tool_result_chars += changed.1;
    }
    if truncate_documents {
        let changed = truncate_current_documents(
            &mut request
                .conversation_state
                .current_message
                .user_input_message
                .content,
            document_budget,
        );
        stats.truncated_current_documents += changed.0;
        stats.truncated_current_document_chars += changed.1;
    }
    if truncate_user_content {
        let changed = truncate_current_user_content(
            &mut request
                .conversation_state
                .current_message
                .user_input_message
                .content,
            user_content_budget,
        );
        stats.truncated_current_user_content += changed.0;
        stats.truncated_current_user_content_chars += changed.1;
    }
    if truncate_images {
        let changed = drop_current_images_to_budget(
            &mut request
                .conversation_state
                .current_message
                .user_input_message,
            config.current_images_max_bytes,
        );
        stats.dropped_current_images += changed.0;
        stats.dropped_current_image_bytes += changed.1;
    }

    let serialize_started_at = Instant::now();
    body = serialize_request(request)?;
    *guard_serializations = (*guard_serializations).saturating_add(1);
    *serialize_elapsed += serialize_started_at.elapsed();

    let mut iterations = 0usize;
    while body.len() > max_bytes && iterations < CURRENT_FIT_MAX_ITERATIONS {
        iterations += 1;
        let before_len = body.len();
        let mut changed = false;

        if truncate_tool_results
            && tool_budget.is_some_and(|budget| budget > CURRENT_FIT_MIN_TEXT_CHARS)
        {
            tool_budget = next_fit_budget(tool_budget, body.len(), max_bytes);
            let result = truncate_current_tool_results(request, tool_budget);
            if result.0 > 0 {
                stats.truncated_current_tool_results += result.0;
                stats.truncated_current_tool_result_chars += result.1;
                changed = true;
            }
        }

        if !changed
            && truncate_documents
            && document_budget.is_some_and(|budget| budget > CURRENT_FIT_MIN_TEXT_CHARS)
        {
            document_budget = next_fit_budget(document_budget, body.len(), max_bytes);
            let result = truncate_current_documents(
                &mut request
                    .conversation_state
                    .current_message
                    .user_input_message
                    .content,
                document_budget,
            );
            if result.0 > 0 {
                stats.truncated_current_documents += result.0;
                stats.truncated_current_document_chars += result.1;
                changed = true;
            }
        }

        if !changed
            && truncate_user_content
            && user_content_budget.is_some_and(|budget| budget > CURRENT_FIT_MIN_TEXT_CHARS)
        {
            user_content_budget = next_fit_budget(user_content_budget, body.len(), max_bytes);
            let result = truncate_current_user_content(
                &mut request
                    .conversation_state
                    .current_message
                    .user_input_message
                    .content,
                user_content_budget,
            );
            if result.0 > 0 {
                stats.truncated_current_user_content += result.0;
                stats.truncated_current_user_content_chars += result.1;
                changed = true;
            }
        }

        if !changed && truncate_images {
            let required_reduction = body
                .len()
                .saturating_sub(max_bytes)
                .saturating_add(CURRENT_FIT_OVERHEAD_BYTES);
            let result = drop_current_images_for_body_reduction(
                &mut request
                    .conversation_state
                    .current_message
                    .user_input_message,
                required_reduction,
            );
            if result.0 > 0 {
                stats.dropped_current_images += result.0;
                stats.dropped_current_image_bytes += result.1;
                changed = true;
            }
        }

        if !changed {
            break;
        }

        let serialize_started_at = Instant::now();
        body = serialize_request(request)?;
        *guard_serializations = (*guard_serializations).saturating_add(1);
        *serialize_elapsed += serialize_started_at.elapsed();
        if body.len() >= before_len {
            break;
        }
    }

    Ok((body, stats))
}

fn normalize_current_text_budget(max_chars: usize) -> Option<usize> {
    (max_chars > 0).then(|| max_chars.max(CURRENT_FIT_MIN_TEXT_CHARS))
}

fn next_fit_budget(
    current_budget: Option<usize>,
    current_bytes: usize,
    target_bytes: usize,
) -> Option<usize> {
    let budget = current_budget?;
    if budget <= CURRENT_FIT_MIN_TEXT_CHARS {
        return Some(CURRENT_FIT_MIN_TEXT_CHARS);
    }
    let overage = current_bytes
        .saturating_sub(target_bytes)
        .saturating_add(CURRENT_FIT_OVERHEAD_BYTES);
    let reduction = overage
        .max(budget / 4)
        .min(budget - CURRENT_FIT_MIN_TEXT_CHARS);
    Some(
        budget
            .saturating_sub(reduction)
            .max(CURRENT_FIT_MIN_TEXT_CHARS),
    )
}

fn apply_anthropic_current_payload_shaping_until_fit(
    request: &mut MessagesRequest,
    config: PayloadShapingConfig,
    max_bytes: usize,
    body: String,
    serialize_elapsed: &mut Duration,
    guard_serializations: &mut usize,
) -> Result<(String, CurrentShapingStats), PayloadGuardError> {
    let mut body = body;
    let mut stats = CurrentShapingStats::default();

    if body.len() <= max_bytes {
        return Ok((body, stats));
    }

    let mut tool_budget = normalize_current_text_budget(config.current_tool_result_max_chars);
    let mut document_budget = normalize_current_text_budget(config.current_document_max_chars);
    let mut user_content_budget =
        normalize_current_text_budget(config.current_user_content_max_chars);

    let truncate_tool_results =
        config.fit_current_payload_to_budget || config.truncate_current_tool_results;
    let truncate_documents =
        config.fit_current_payload_to_budget || config.truncate_current_documents;
    let truncate_user_content =
        config.fit_current_payload_to_budget || config.truncate_current_user_content;
    let truncate_images = config.fit_current_payload_to_budget || config.truncate_current_images;

    if truncate_tool_results {
        let changed = truncate_anthropic_current_tool_results(request, tool_budget);
        stats.truncated_current_tool_results += changed.0;
        stats.truncated_current_tool_result_chars += changed.1;
    }
    if truncate_documents {
        let changed = truncate_anthropic_current_documents(request, document_budget);
        stats.truncated_current_documents += changed.0;
        stats.truncated_current_document_chars += changed.1;
    }
    if truncate_user_content {
        let changed = truncate_anthropic_current_user_content(request, user_content_budget);
        stats.truncated_current_user_content += changed.0;
        stats.truncated_current_user_content_chars += changed.1;
    }
    if truncate_images {
        let changed =
            drop_anthropic_current_images_to_budget(request, config.current_images_max_bytes);
        stats.dropped_current_images += changed.0;
        stats.dropped_current_image_bytes += changed.1;
    }

    let serialize_started_at = Instant::now();
    body = serialize_anthropic_request(request)?;
    *guard_serializations = (*guard_serializations).saturating_add(1);
    *serialize_elapsed += serialize_started_at.elapsed();

    let mut iterations = 0usize;
    while body.len() > max_bytes && iterations < CURRENT_FIT_MAX_ITERATIONS {
        iterations += 1;
        let before_len = body.len();
        let mut changed = false;

        if truncate_tool_results
            && tool_budget.is_some_and(|budget| budget > CURRENT_FIT_MIN_TEXT_CHARS)
        {
            tool_budget = next_fit_budget(tool_budget, body.len(), max_bytes);
            let result = truncate_anthropic_current_tool_results(request, tool_budget);
            if result.0 > 0 {
                stats.truncated_current_tool_results += result.0;
                stats.truncated_current_tool_result_chars += result.1;
                changed = true;
            }
        }

        if !changed
            && truncate_documents
            && document_budget.is_some_and(|budget| budget > CURRENT_FIT_MIN_TEXT_CHARS)
        {
            document_budget = next_fit_budget(document_budget, body.len(), max_bytes);
            let result = truncate_anthropic_current_documents(request, document_budget);
            if result.0 > 0 {
                stats.truncated_current_documents += result.0;
                stats.truncated_current_document_chars += result.1;
                changed = true;
            }
        }

        if !changed
            && truncate_user_content
            && user_content_budget.is_some_and(|budget| budget > CURRENT_FIT_MIN_TEXT_CHARS)
        {
            user_content_budget = next_fit_budget(user_content_budget, body.len(), max_bytes);
            let result = truncate_anthropic_current_user_content(request, user_content_budget);
            if result.0 > 0 {
                stats.truncated_current_user_content += result.0;
                stats.truncated_current_user_content_chars += result.1;
                changed = true;
            }
        }

        if !changed && truncate_images {
            let required_reduction = body
                .len()
                .saturating_sub(max_bytes)
                .saturating_add(CURRENT_FIT_OVERHEAD_BYTES);
            let result =
                drop_anthropic_current_images_for_body_reduction(request, required_reduction);
            if result.0 > 0 {
                stats.dropped_current_images += result.0;
                stats.dropped_current_image_bytes += result.1;
                changed = true;
            }
        }

        if !changed {
            break;
        }

        let serialize_started_at = Instant::now();
        body = serialize_anthropic_request(request)?;
        *guard_serializations = (*guard_serializations).saturating_add(1);
        *serialize_elapsed += serialize_started_at.elapsed();
        if body.len() >= before_len {
            break;
        }
    }

    Ok((body, stats))
}

fn truncate_anthropic_current_tool_results(
    request: &mut MessagesRequest,
    max_chars: Option<usize>,
) -> (usize, usize) {
    let Some(max_chars) = max_chars else {
        return (0, 0);
    };
    let Some(message) = request.messages.last_mut() else {
        return (0, 0);
    };
    let Some(blocks) = content_blocks_mut(&mut message.content) else {
        return (0, 0);
    };
    let mut truncated = 0usize;
    let mut omitted = 0usize;
    for block in blocks {
        if block_type(block) != Some("tool_result") {
            continue;
        }
        let result = truncate_anthropic_tool_result_block(
            block,
            max_chars,
            120,
            80,
            "current tool result",
            false,
        );
        truncated += result.0;
        omitted += result.1;
    }
    (truncated, omitted)
}

fn truncate_anthropic_current_documents(
    request: &mut MessagesRequest,
    max_chars: Option<usize>,
) -> (usize, usize) {
    let Some(max_chars) = max_chars else {
        return (0, 0);
    };
    let Some(message) = request.messages.last_mut() else {
        return (0, 0);
    };
    let Some(blocks) = content_blocks_mut(&mut message.content) else {
        return (0, 0);
    };
    let mut truncated = 0usize;
    let mut omitted = 0usize;
    for block in blocks {
        if block_type(block) != Some("document") {
            continue;
        }
        for path in [["text"].as_slice(), ["source", "text"].as_slice()] {
            let Some(value) = nested_value_mut(block, path) else {
                continue;
            };
            let result = truncate_anthropic_content_texts(
                value,
                max_chars,
                120,
                80,
                "current document",
                false,
            );
            truncated += result.0;
            omitted += result.1;
        }
    }
    (truncated, omitted)
}

fn truncate_anthropic_current_user_content(
    request: &mut MessagesRequest,
    max_chars: Option<usize>,
) -> (usize, usize) {
    let Some(max_chars) = max_chars else {
        return (0, 0);
    };
    let Some(message) = request.messages.last_mut() else {
        return (0, 0);
    };
    if let Some(text) = message.content.as_str().map(str::to_string) {
        let original_chars = text.chars().count();
        if original_chars <= max_chars {
            return (0, 0);
        }
        let replacement =
            truncate_text_head_tail(&text, max_chars, 160, 100, "current user content");
        let replacement_chars = replacement.chars().count();
        message.content = Value::String(replacement);
        return (1, original_chars.saturating_sub(replacement_chars));
    }
    let Some(blocks) = content_blocks_mut(&mut message.content) else {
        return (0, 0);
    };
    let mut truncated = 0usize;
    let mut omitted = 0usize;
    for block in blocks {
        if block_type(block) != Some("text") {
            continue;
        }
        let Some(value) = block.get_mut("text") else {
            continue;
        };
        let result = truncate_anthropic_content_texts(
            value,
            max_chars,
            160,
            100,
            "current user content",
            false,
        );
        truncated += result.0;
        omitted += result.1;
    }
    (truncated, omitted)
}

fn drop_anthropic_current_images_to_budget(
    request: &mut MessagesRequest,
    max_bytes: usize,
) -> (usize, usize) {
    let mut dropped = 0usize;
    let mut dropped_bytes = 0usize;
    while current_anthropic_images_bytes(request) > max_bytes {
        let result = drop_largest_anthropic_current_image(request);
        if result.0 == 0 {
            break;
        }
        dropped += result.0;
        dropped_bytes += result.1;
    }
    if dropped > 0 {
        let note = if dropped == 1 {
            "[Current image was omitted because it exceeded the request image budget.]"
        } else {
            "[Current images were omitted because they exceeded the request image budget.]"
        };
        append_anthropic_current_text(request, note);
    }
    (dropped, dropped_bytes)
}

fn oversized_current_image_placeholder(dropped: usize) -> Value {
    let text = if dropped == 1 {
        "[Current image was omitted because it exceeded the upstream 5 MB image size limit.]"
    } else {
        "[Current images were omitted because they exceeded the upstream 5 MB image size limit.]"
    };
    serde_json::json!({
        "type": "text",
        "text": text
    })
}

fn drop_oversized_anthropic_current_images(
    request: &mut MessagesRequest,
    max_source_bytes: usize,
) -> (usize, usize) {
    if max_source_bytes == 0 {
        return (0, 0);
    }
    let Some(message) = request.messages.last_mut() else {
        return (0, 0);
    };
    let Some(blocks) = content_blocks_mut(&mut message.content) else {
        return (0, 0);
    };

    let mut dropped = 0usize;
    let mut dropped_bytes = 0usize;
    let mut retained = Vec::with_capacity(blocks.len());
    let mut placeholder_index = None;
    for block in std::mem::take(blocks) {
        let bytes =
            (block_type(&block) == Some("image")).then(|| anthropic_image_source_bytes(&block));
        if bytes.is_some_and(|bytes| bytes > max_source_bytes) {
            placeholder_index.get_or_insert(retained.len());
            dropped += 1;
            dropped_bytes += bytes.unwrap_or_default();
        } else {
            retained.push(block);
        }
    }
    if let Some(index) = placeholder_index {
        retained.insert(index, oversized_current_image_placeholder(dropped));
    }
    *blocks = retained;
    (dropped, dropped_bytes)
}

fn current_anthropic_images_bytes(request: &MessagesRequest) -> usize {
    request
        .messages
        .last()
        .map(|message| content_images_bytes(&message.content))
        .unwrap_or(0)
}

fn drop_largest_anthropic_current_image(request: &mut MessagesRequest) -> (usize, usize) {
    let Some(message) = request.messages.last_mut() else {
        return (0, 0);
    };
    let Some(blocks) = content_blocks_mut(&mut message.content) else {
        return (0, 0);
    };
    let Some((idx, bytes)) = blocks
        .iter()
        .enumerate()
        .filter(|(_, block)| block_type(block) == Some("image"))
        .map(|(idx, block)| (idx, json_len(block)))
        .max_by_key(|(_, bytes)| *bytes)
    else {
        return (0, 0);
    };
    blocks.remove(idx);
    (1, bytes)
}

fn drop_anthropic_current_images_for_body_reduction(
    request: &mut MessagesRequest,
    required_bytes: usize,
) -> (usize, usize) {
    let mut dropped = 0usize;
    let mut dropped_bytes = 0usize;
    while dropped_bytes < required_bytes {
        let result = drop_largest_anthropic_current_image(request);
        if result.0 == 0 {
            break;
        }
        dropped += result.0;
        dropped_bytes = dropped_bytes.saturating_add(result.1);
    }
    if dropped > 0 {
        let note = if dropped == 1 {
            "[Current image was omitted because it exceeded the request image budget.]"
        } else {
            "[Current images were omitted because they exceeded the request image budget.]"
        };
        append_anthropic_current_text(request, note);
    }
    (dropped, dropped_bytes)
}

fn append_anthropic_current_text(request: &mut MessagesRequest, text: &str) {
    let Some(message) = request.messages.last_mut() else {
        return;
    };
    if let Some(existing) = message.content.as_str().map(str::to_string) {
        message.content = Value::String(format!("{}\n\n{}", existing, text));
        return;
    }
    if let Some(blocks) = content_blocks_mut(&mut message.content) {
        blocks.push(serde_json::json!({"type": "text", "text": text}));
    }
}

fn nested_value_mut<'a>(value: &'a mut Value, path: &[&str]) -> Option<&'a mut Value> {
    let mut current = value;
    for key in path {
        current = current.get_mut(*key)?;
    }
    Some(current)
}

fn truncate_current_tool_results(
    request: &mut KiroRequest,
    max_chars: Option<usize>,
) -> (usize, usize) {
    let Some(max_chars) = max_chars else {
        return (0, 0);
    };

    let mut truncated = 0usize;
    let mut omitted_chars = 0usize;
    for result in &mut request
        .conversation_state
        .current_message
        .user_input_message
        .user_input_message_context
        .tool_results
    {
        for item in &mut result.content {
            let Some(value) = item.get_mut("text") else {
                continue;
            };
            let Some(text) = value.as_str() else {
                continue;
            };
            if text.chars().count() <= max_chars {
                continue;
            }
            let original_chars = text.chars().count();
            let replacement =
                truncate_text_head_tail(text, max_chars, 120, 80, "current tool result");
            let replacement_chars = replacement.chars().count();
            *value = serde_json::Value::String(replacement);
            truncated += 1;
            omitted_chars += original_chars.saturating_sub(replacement_chars);
        }
    }
    (truncated, omitted_chars)
}

fn truncate_current_documents(content: &mut String, max_chars: Option<usize>) -> (usize, usize) {
    let Some(max_chars) = max_chars else {
        return (0, 0);
    };

    let original = content.clone();
    let mut remaining = original.as_str();
    let mut output = String::with_capacity(original.len());
    let mut truncated = 0usize;
    let mut omitted_chars = 0usize;

    while let Some(open_rel) = remaining.find("<document") {
        let open_idx = open_rel;
        output.push_str(&remaining[..open_idx]);
        let after_open = &remaining[open_idx..];
        let Some(tag_end_rel) = after_open.find('>') else {
            output.push_str(after_open);
            *content = output;
            return (truncated, omitted_chars);
        };
        let tag_end = open_idx + tag_end_rel + 1;
        output.push_str(&remaining[open_idx..tag_end]);
        let body_start = tag_end;
        let after_tag = &remaining[body_start..];
        let Some(close_rel) = after_tag.find("</document>") else {
            output.push_str(after_tag);
            *content = output;
            return (truncated, omitted_chars);
        };
        let body = &after_tag[..close_rel];
        if body.chars().count() > max_chars {
            let replacement = truncate_text_head_tail(body, max_chars, 120, 80, "current document");
            omitted_chars += body
                .chars()
                .count()
                .saturating_sub(replacement.chars().count());
            output.push_str(&replacement);
            truncated += 1;
        } else {
            output.push_str(body);
        }
        output.push_str("</document>");
        remaining = &after_tag[close_rel + "</document>".len()..];
    }

    output.push_str(remaining);
    if truncated > 0 {
        *content = output;
    }
    (truncated, omitted_chars)
}

fn truncate_current_user_content(content: &mut String, max_chars: Option<usize>) -> (usize, usize) {
    let Some(max_chars) = max_chars else {
        return (0, 0);
    };
    if contains_document_block(content) {
        return truncate_text_outside_document_blocks(content, max_chars);
    }
    let original_chars = content.chars().count();
    if original_chars <= max_chars {
        return (0, 0);
    }
    let replacement = truncate_text_head_tail(content, max_chars, 160, 100, "current user content");
    let replacement_chars = replacement.chars().count();
    *content = replacement;
    (1, original_chars.saturating_sub(replacement_chars))
}

fn truncate_text_outside_document_blocks(content: &mut String, max_chars: usize) -> (usize, usize) {
    let original = content.clone();
    let document_ranges = document_block_ranges(&original);
    if document_ranges.is_empty() {
        return (0, 0);
    }

    let mut outside_chars = 0usize;
    let mut cursor = 0usize;
    for (start, end) in &document_ranges {
        outside_chars += original[cursor..*start].chars().count();
        cursor = *end;
    }
    outside_chars += original[cursor..].chars().count();

    if outside_chars <= max_chars {
        return (0, 0);
    }

    let marker = format!(
        "\n[current user content outside documents truncated by proxy: original_chars={}, preserved=head_tail_chars]\n",
        outside_chars
    );
    let marker_chars = marker.chars().count();
    let text_budget = max_chars.saturating_sub(marker_chars);
    let head_budget = text_budget.saturating_mul(2) / 3;
    let tail_budget = text_budget.saturating_sub(head_budget);
    let skip_start = head_budget;
    let skip_end = outside_chars.saturating_sub(tail_budget);

    let mut output = String::with_capacity(original.len().min(max_chars + 4096));
    let mut outside_pos = 0usize;
    let mut marker_inserted = false;
    cursor = 0;
    for (start, end) in document_ranges {
        append_truncated_outside_document_text(
            &original[cursor..start],
            &mut output,
            &mut outside_pos,
            skip_start,
            skip_end,
            &marker,
            &mut marker_inserted,
        );
        output.push_str(&original[start..end]);
        cursor = end;
    }
    append_truncated_outside_document_text(
        &original[cursor..],
        &mut output,
        &mut outside_pos,
        skip_start,
        skip_end,
        &marker,
        &mut marker_inserted,
    );

    if !marker_inserted || output == original {
        return (0, 0);
    }

    let replacement_outside_chars = head_budget
        .saturating_add(tail_budget)
        .saturating_add(marker_chars);
    *content = output;
    (1, outside_chars.saturating_sub(replacement_outside_chars))
}

fn append_truncated_outside_document_text(
    segment: &str,
    output: &mut String,
    outside_pos: &mut usize,
    skip_start: usize,
    skip_end: usize,
    marker: &str,
    marker_inserted: &mut bool,
) {
    for ch in segment.chars() {
        let pos = *outside_pos;
        *outside_pos += 1;
        if pos < skip_start || pos >= skip_end {
            output.push(ch);
        } else if !*marker_inserted {
            if !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            output.push_str(marker);
            *marker_inserted = true;
        }
    }
}

fn document_block_ranges(content: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut search_from = 0usize;
    while let Some(open_rel) = content[search_from..].find("<document") {
        let open = search_from + open_rel;
        let Some(tag_end_rel) = content[open..].find('>') else {
            break;
        };
        let body_start = open + tag_end_rel + 1;
        let Some(close_rel) = content[body_start..].find("</document>") else {
            break;
        };
        let end = body_start + close_rel + "</document>".len();
        ranges.push((open, end));
        search_from = end;
    }
    ranges
}

fn contains_document_block(content: &str) -> bool {
    content.contains("<document") && content.contains("</document>")
}

fn drop_current_images_to_budget(user: &mut UserInputMessage, max_bytes: usize) -> (usize, usize) {
    let mut dropped = 0usize;
    let mut dropped_bytes = 0usize;
    while !user.images.is_empty() && json_len(&user.images) > max_bytes {
        let result = drop_largest_current_image(user);
        if result.0 == 0 {
            break;
        }
        dropped += result.0;
        dropped_bytes += result.1;
    }
    if dropped > 0 {
        let note = if dropped == 1 {
            "[Current image was omitted because it exceeded the request image budget.]"
        } else {
            "[Current images were omitted because they exceeded the request image budget.]"
        };
        append_text(&mut user.content, note);
    }
    (dropped, dropped_bytes)
}

fn drop_oversized_current_images(
    user: &mut UserInputMessage,
    max_source_bytes: usize,
) -> (usize, usize) {
    if max_source_bytes == 0 || user.images.is_empty() {
        return (0, 0);
    }

    let before = user.images.len();
    let mut dropped_bytes = 0usize;
    user.images.retain(|image| {
        let bytes = kiro_image_source_bytes(image);
        if bytes > max_source_bytes {
            dropped_bytes += bytes;
            false
        } else {
            true
        }
    });

    let dropped = before.saturating_sub(user.images.len());
    if dropped > 0 {
        let note = if dropped == 1 {
            "[Current image was omitted because it exceeded the upstream 5 MB image size limit.]"
        } else {
            "[Current images were omitted because they exceeded the upstream 5 MB image size limit.]"
        };
        append_text(&mut user.content, note);
    }
    (dropped, dropped_bytes)
}

fn drop_largest_current_image(user: &mut UserInputMessage) -> (usize, usize) {
    let Some((idx, bytes)) = user
        .images
        .iter()
        .enumerate()
        .map(|(idx, image)| (idx, json_len(image)))
        .max_by_key(|(_, bytes)| *bytes)
    else {
        return (0, 0);
    };
    user.images.remove(idx);
    (1, bytes)
}

fn drop_current_images_for_body_reduction(
    user: &mut UserInputMessage,
    required_bytes: usize,
) -> (usize, usize) {
    let mut dropped = 0usize;
    let mut dropped_bytes = 0usize;
    while dropped_bytes < required_bytes {
        let result = drop_largest_current_image(user);
        if result.0 == 0 {
            break;
        }
        dropped += result.0;
        dropped_bytes = dropped_bytes.saturating_add(result.1);
    }
    if dropped > 0 {
        let note = if dropped == 1 {
            "[Current image was omitted because it exceeded the request image budget.]"
        } else {
            "[Current images were omitted because they exceeded the request image budget.]"
        };
        append_text(&mut user.content, note);
    }
    (dropped, dropped_bytes)
}

fn trim_history_to_estimated_budget(
    history: &mut Vec<Message>,
    current_results: &[ToolResult],
    current_bytes: usize,
    target_bytes: usize,
) -> usize {
    if history.is_empty() || current_bytes <= target_bytes {
        return 0;
    }

    let mut prefix_len = 0usize;
    let mut removed_item_bytes = 0usize;
    let mut estimated_bytes = current_bytes;
    while estimated_bytes > target_bytes && prefix_len < history.len() {
        let remaining = &history[prefix_len..];
        let next_turn = remaining
            .iter()
            .enumerate()
            .skip(1)
            .find_map(|(idx, message)| {
                let Message::User(user) = message else {
                    return None;
                };
                user.user_input_message
                    .user_input_message_context
                    .tool_results
                    .is_empty()
                    .then_some(idx)
            });
        let relative_end = match next_turn {
            Some(idx) => idx,
            None if current_results.is_empty() => remaining.len(),
            None => break,
        };
        if relative_end == 0 {
            break;
        }
        let previous_prefix_len = prefix_len;
        prefix_len += relative_end;
        removed_item_bytes = removed_item_bytes.saturating_add(
            history[previous_prefix_len..prefix_len]
                .iter()
                .map(json_len)
                .sum::<usize>(),
        );
        estimated_bytes =
            current_bytes.saturating_sub(json_array_prefix_reduction_from_item_bytes(
                history.len(),
                prefix_len,
                removed_item_bytes,
            ));
    }

    if prefix_len > 0 {
        history.drain(0..prefix_len);
    }
    prefix_len
}

#[cfg(test)]
fn json_array_prefix_reduction<T: Serialize>(items: &[T], prefix_len: usize) -> usize {
    let prefix_len = prefix_len.min(items.len());
    let item_bytes = items[..prefix_len].iter().map(json_len).sum::<usize>();
    json_array_prefix_reduction_from_item_bytes(items.len(), prefix_len, item_bytes)
}

fn json_array_prefix_reduction_from_item_bytes(
    item_count: usize,
    prefix_len: usize,
    item_bytes: usize,
) -> usize {
    let prefix_len = prefix_len.min(item_count);
    let separators_before = item_count.saturating_sub(1);
    let separators_after = item_count.saturating_sub(prefix_len).saturating_sub(1);
    item_bytes.saturating_add(separators_before.saturating_sub(separators_after))
}

fn align_history_to_user(history: &mut Vec<Message>) -> usize {
    let removed = history
        .iter()
        .take_while(|message| matches!(message, Message::Assistant(_)))
        .count();
    if removed > 0 {
        history.drain(0..removed);
    }
    removed
}

#[derive(Default)]
struct RepairStats {
    normalized_empty_tool_results: usize,
    removed_empty_tool_uses: usize,
    removed_duplicate_tool_uses: usize,
    renamed_duplicate_tool_uses: usize,
    removed_orphan_tool_results: usize,
    removed_duplicate_tool_results: usize,
    removed_orphan_tool_uses: usize,
}

impl RepairStats {
    fn was_modified(&self) -> bool {
        self.normalized_empty_tool_results > 0
            || self.removed_empty_tool_uses > 0
            || self.removed_duplicate_tool_uses > 0
            || self.renamed_duplicate_tool_uses > 0
            || self.removed_orphan_tool_results > 0
            || self.removed_duplicate_tool_results > 0
            || self.removed_orphan_tool_uses > 0
    }
}

fn repair_request(request: &mut KiroRequest) -> RepairStats {
    let mut stats = RepairStats::default();
    let conversation_state = &mut request.conversation_state;
    let history = &mut conversation_state.history;
    let current_user = &mut conversation_state.current_message.user_input_message;

    stats.normalized_empty_tool_results += normalize_empty_tool_result_contents(history);
    stats.normalized_empty_tool_results +=
        normalize_tool_results_content(&mut current_user.user_input_message_context.tool_results);

    stats.removed_empty_tool_uses += strip_empty_tool_uses(history);
    stats.removed_duplicate_tool_uses += dedupe_tool_uses(history);
    stats.renamed_duplicate_tool_uses += rename_repeated_tool_use_ids(history, current_user);
    stats.removed_duplicate_tool_results += dedupe_history_tool_results(history);
    let history_results = repair_orphan_tool_results(history);
    stats.removed_orphan_tool_results += history_results.removed_orphan_tool_results;

    let current_dedupe = dedupe_current_tool_results(current_user);
    stats.removed_duplicate_tool_results += current_dedupe.removed_duplicate_tool_results;
    let current_results = repair_current_orphan_tool_results(history, current_user);
    stats.removed_orphan_tool_results += current_results.removed_orphan_tool_results;

    stats.removed_orphan_tool_uses += remove_unpaired_tool_uses(
        history,
        &current_user.user_input_message_context.tool_results,
    );
    stats
}

fn add_repair_stats_to_report(report: &mut PayloadGuardReport, repair: RepairStats) {
    report.removed_empty_tool_uses += repair.removed_empty_tool_uses;
    report.removed_duplicate_tool_uses += repair.removed_duplicate_tool_uses;
    report.renamed_duplicate_tool_uses += repair.renamed_duplicate_tool_uses;
    report.removed_orphan_tool_results += repair.removed_orphan_tool_results;
    report.removed_duplicate_tool_results += repair.removed_duplicate_tool_results;
    report.removed_orphan_tool_uses += repair.removed_orphan_tool_uses;
}

fn normalize_empty_tool_result_contents(history: &mut [Message]) -> usize {
    let mut normalized = 0usize;
    for message in history {
        let Message::User(user) = message else {
            continue;
        };
        normalized += normalize_tool_results_content(
            &mut user
                .user_input_message
                .user_input_message_context
                .tool_results,
        );
    }
    normalized
}

fn normalize_tool_results_content(results: &mut [ToolResult]) -> usize {
    let mut normalized = 0usize;
    for result in results {
        if tool_result_has_non_empty_content(result) {
            continue;
        }
        let mut item = serde_json::Map::new();
        item.insert(
            "text".to_string(),
            Value::String(EMPTY_TOOL_RESULT_CONTENT_PLACEHOLDER.to_string()),
        );
        result.content = vec![item];
        normalized += 1;
    }
    normalized
}

fn tool_result_has_non_empty_content(result: &ToolResult) -> bool {
    result.content.iter().any(tool_result_item_has_content)
}

fn tool_result_item_has_content(item: &serde_json::Map<String, Value>) -> bool {
    if let Some(text) = item.get("text").and_then(Value::as_str) {
        if !text.trim().is_empty() {
            return true;
        }
    }
    item.iter()
        .any(|(key, value)| key != "type" && key != "text" && !value.is_null())
}

fn strip_empty_tool_uses(history: &mut [Message]) -> usize {
    let mut removed = 0;
    for message in history {
        if let Message::Assistant(assistant) = message {
            if assistant
                .assistant_response_message
                .tool_uses
                .as_ref()
                .is_some_and(Vec::is_empty)
            {
                assistant.assistant_response_message.tool_uses = None;
                removed += 1;
            }
        }
    }
    removed
}

fn dedupe_tool_uses(history: &mut [Message]) -> usize {
    let mut removed = 0;
    for message in history {
        let Message::Assistant(assistant) = message else {
            continue;
        };
        let Some(tool_uses) = &mut assistant.assistant_response_message.tool_uses else {
            continue;
        };
        let original_len = tool_uses.len();
        let mut seen = HashSet::new();
        tool_uses.retain(|tool_use| {
            !tool_use.tool_use_id.trim().is_empty() && seen.insert(tool_use.tool_use_id.clone())
        });
        removed += original_len.saturating_sub(tool_uses.len());
        if tool_uses.is_empty() {
            assistant.assistant_response_message.tool_uses = None;
        }
    }
    removed
}

fn rename_repeated_tool_use_ids(
    history: &mut [Message],
    current_user: &mut UserInputMessage,
) -> usize {
    let mut seen = HashSet::new();
    let mut renamed = 0;

    for idx in 0..history.len() {
        let (_before, rest) = history.split_at_mut(idx);
        let Some((current, after)) = rest.split_first_mut() else {
            continue;
        };
        let Message::Assistant(assistant) = current else {
            continue;
        };
        let Some(tool_uses) = &mut assistant.assistant_response_message.tool_uses else {
            continue;
        };

        let mut next_results: Option<&mut Vec<ToolResult>> = if after.is_empty() {
            Some(&mut current_user.user_input_message_context.tool_results)
        } else {
            match after.split_first_mut() {
                Some((Message::User(user), _)) => Some(
                    &mut user
                        .user_input_message
                        .user_input_message_context
                        .tool_results,
                ),
                _ => None,
            }
        };

        for tool_use in tool_uses.iter_mut() {
            let tool_use_id = tool_use.tool_use_id.trim().to_string();
            if tool_use_id.is_empty() {
                continue;
            }
            if seen.insert(tool_use_id.clone()) {
                continue;
            }

            let new_id = make_unique_duplicate_tool_use_id(&tool_use_id, &seen, idx);
            if let Some(results) = next_results.as_deref_mut() {
                rename_matching_tool_results(results, &tool_use_id, &new_id);
            }
            tool_use.tool_use_id = new_id.clone();
            seen.insert(new_id);
            renamed += 1;
        }
    }

    renamed
}

fn make_unique_duplicate_tool_use_id(
    original_id: &str,
    seen: &HashSet<String>,
    assistant_index: usize,
) -> String {
    let mut candidate_index = 1usize;
    loop {
        let candidate = format!(
            "{}__dup{}_{}",
            original_id, assistant_index, candidate_index
        );
        if !seen.contains(&candidate) {
            return candidate;
        }
        candidate_index += 1;
    }
}

fn rename_matching_tool_results(results: &mut [ToolResult], old_id: &str, new_id: &str) {
    for result in results {
        if result.tool_use_id == old_id {
            result.tool_use_id = new_id.to_string();
        }
    }
}

fn dedupe_history_tool_results(history: &mut [Message]) -> usize {
    let mut removed = 0;
    for message in history {
        let Message::User(user) = message else {
            continue;
        };
        removed += dedupe_tool_results_keep_first(
            &mut user
                .user_input_message
                .user_input_message_context
                .tool_results,
        )
        .removed_duplicate_tool_results;
    }
    removed
}

fn dedupe_current_tool_results(user: &mut UserInputMessage) -> RepairStats {
    dedupe_tool_results_keep_first(&mut user.user_input_message_context.tool_results)
}

fn dedupe_tool_results_keep_first(results: &mut Vec<ToolResult>) -> RepairStats {
    let mut stats = RepairStats::default();
    if results.len() <= 1 {
        return stats;
    }

    let original_len = results.len();
    let mut seen = HashSet::new();
    results.retain(|result| seen.insert(result.tool_use_id.clone()));

    stats.removed_duplicate_tool_results += original_len.saturating_sub(results.len());
    stats
}

fn repair_orphan_tool_results(history: &mut [Message]) -> RepairStats {
    let mut stats = RepairStats::default();
    for idx in 0..history.len() {
        let valid_ids = previous_assistant_tool_use_ids(history, idx);

        let Message::User(user) = &mut history[idx] else {
            continue;
        };
        let repaired = repair_user_tool_results(&valid_ids, &mut user.user_input_message);
        stats.removed_orphan_tool_results += repaired.removed_orphan_tool_results;
    }
    stats
}

fn repair_current_orphan_tool_results(
    history: &[Message],
    current_user: &mut UserInputMessage,
) -> RepairStats {
    let valid_ids = last_assistant_tool_use_ids(history);
    repair_user_input_tool_results(&valid_ids, current_user)
}

fn repair_user_tool_results(valid_ids: &HashSet<String>, user: &mut UserMessage) -> RepairStats {
    repair_tool_results(
        valid_ids,
        &mut user.user_input_message_context.tool_results,
        &mut user.content,
    )
}

fn repair_user_input_tool_results(
    valid_ids: &HashSet<String>,
    user: &mut UserInputMessage,
) -> RepairStats {
    repair_tool_results(
        valid_ids,
        &mut user.user_input_message_context.tool_results,
        &mut user.content,
    )
}

fn repair_tool_results(
    valid_ids: &HashSet<String>,
    results: &mut Vec<ToolResult>,
    content: &mut String,
) -> RepairStats {
    let mut stats = RepairStats::default();
    if results.is_empty() {
        return stats;
    }

    let original_len = results.len();
    results.retain(|result| valid_ids.contains(&result.tool_use_id));
    stats.removed_orphan_tool_results += original_len.saturating_sub(results.len());
    if original_len != results.len() && results.is_empty() && content.trim().is_empty() {
        *content = EMPTY_USER_CONTENT_PLACEHOLDER.to_string();
    }
    stats
}

fn remove_unpaired_tool_uses(history: &mut [Message], current_results: &[ToolResult]) -> usize {
    let mut removed = 0;
    for idx in 0..history.len() {
        let paired_ids = next_tool_result_ids(history, idx, current_results);
        let Message::Assistant(assistant) = &mut history[idx] else {
            continue;
        };
        let Some(tool_uses) = &mut assistant.assistant_response_message.tool_uses else {
            continue;
        };
        let original_len = tool_uses.len();
        tool_uses.retain(|tool_use| paired_ids.contains(&tool_use.tool_use_id));
        removed += original_len.saturating_sub(tool_uses.len());
        if tool_uses.is_empty() {
            assistant.assistant_response_message.tool_uses = None;
        }
    }
    removed
}

fn previous_assistant_tool_use_ids(history: &[Message], idx: usize) -> HashSet<String> {
    if idx == 0 {
        return HashSet::new();
    }
    assistant_tool_use_ids(history.get(idx - 1))
}

fn last_assistant_tool_use_ids(history: &[Message]) -> HashSet<String> {
    assistant_tool_use_ids(history.last())
}

fn next_tool_result_ids(
    history: &[Message],
    idx: usize,
    current_results: &[ToolResult],
) -> HashSet<String> {
    if let Some(Message::User(user)) = history.get(idx + 1) {
        return user
            .user_input_message
            .user_input_message_context
            .tool_results
            .iter()
            .map(|result| result.tool_use_id.clone())
            .collect();
    }

    if idx + 1 == history.len() {
        return current_results
            .iter()
            .map(|result| result.tool_use_id.clone())
            .collect();
    }

    HashSet::new()
}

fn assistant_tool_use_ids(message: Option<&Message>) -> HashSet<String> {
    let Some(Message::Assistant(assistant)) = message else {
        return HashSet::new();
    };
    assistant
        .assistant_response_message
        .tool_uses
        .as_ref()
        .map(|tool_uses| {
            tool_uses
                .iter()
                .map(|tool_use| tool_use.tool_use_id.clone())
                .collect()
        })
        .unwrap_or_default()
}

fn append_text(content: &mut String, text: &str) {
    if text.is_empty() {
        return;
    }
    if content.trim().is_empty() {
        *content = text.to_string();
    } else {
        content.push_str("\n\n");
        content.push_str(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kiro::model::requests::conversation::{
        AssistantMessage, ConversationState, CurrentMessage, HistoryAssistantMessage,
        HistoryUserMessage, KiroImage, ReasoningContent, UserInputMessage, UserInputMessageContext,
    };
    use crate::kiro::model::requests::kiro::{
        AdditionalModelRequestFields, KiroOutputConfig, KiroThinkingConfig,
    };
    use crate::kiro::model::requests::tool::{
        InputSchema, Tool, ToolResult, ToolSpecification, ToolUseEntry,
    };

    const TEST_MODEL: &str = "test-model";

    fn request_with_history(history: Vec<Message>) -> KiroRequest {
        KiroRequest {
            conversation_state: ConversationState::new("conv-test")
                .with_current_message(CurrentMessage::new(UserInputMessage::new(
                    "current", TEST_MODEL,
                )))
                .with_history(history),
            profile_arn: None,
            additional_model_request_fields: None,
            tool_cache_point_insert_after: Vec::new(),
            cache_point_plan_recording_enabled: true,
        }
    }

    fn guard_config(max_bytes: usize) -> PayloadGuardConfig {
        PayloadGuardConfig {
            enabled: true,
            max_bytes,
            trim_history: true,
            shaping: PayloadShapingConfig::default(),
        }
    }

    fn shaping_config(shaping: PayloadShapingConfig) -> PayloadGuardConfig {
        PayloadGuardConfig {
            enabled: true,
            max_bytes: 1,
            trim_history: false,
            shaping,
        }
    }

    fn anthropic_message(role: &str, content: Value) -> AnthropicMessage {
        AnthropicMessage {
            role: role.to_string(),
            content,
        }
    }

    #[test]
    fn serialize_kiro_request_normalizes_output_config_with_non_adaptive_thinking_for_five_rounds()
    {
        for round in 0..5 {
            let mut request = request_with_history(Vec::new());
            request.additional_model_request_fields = Some(AdditionalModelRequestFields {
                thinking: Some(KiroThinkingConfig {
                    thinking_type: "disabled".to_string(),
                    display: None,
                }),
                output_config: Some(KiroOutputConfig {
                    effort: "max".to_string(),
                }),
                reasoning: None,
            });

            let body = serialize_kiro_request(&request).expect("serialize Kiro request");
            let value: Value = serde_json::from_str(&body).expect("Kiro body JSON");
            assert!(
                value["additionalModelRequestFields"]
                    .get("thinking")
                    .is_none(),
                "round {round}: body={body}"
            );
            assert_eq!(
                value["additionalModelRequestFields"]["output_config"]["effort"], "max",
                "round {round}"
            );
        }
    }

    fn anthropic_request(messages: Vec<AnthropicMessage>) -> MessagesRequest {
        MessagesRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 128,
            messages,
            stream: false,
            system: None,
            tools: None,
            tool_choice: None,
            thinking: None,
            output_config: None,
            metadata: None,
        }
    }

    fn base64_zeros_for_decoded_bytes(decoded_bytes: usize) -> String {
        let complete_groups = decoded_bytes / 3;
        let remainder = decoded_bytes % 3;
        let mut encoded = "A".repeat(complete_groups.saturating_mul(4));
        match remainder {
            1 => encoded.push_str("AA=="),
            2 => encoded.push_str("AAA="),
            _ => {}
        }
        encoded
    }

    fn guard_config_with_shaping(
        max_bytes: usize,
        trim_history: bool,
        shaping: PayloadShapingConfig,
    ) -> PayloadGuardConfig {
        PayloadGuardConfig {
            enabled: true,
            max_bytes,
            trim_history,
            shaping,
        }
    }

    fn tool_result_text(result: &ToolResult) -> &str {
        result.content[0]
            .get("text")
            .and_then(|value| value.as_str())
            .expect("tool result text")
    }

    fn assert_structured_history_tool_turn(
        history: &[Message],
        assistant_idx: usize,
        result_idx: usize,
        expected_id: &str,
        expected_result: &str,
    ) {
        let Message::Assistant(assistant) = &history[assistant_idx] else {
            panic!("expected assistant at history index {assistant_idx}");
        };
        let tool_uses = assistant
            .assistant_response_message
            .tool_uses
            .as_ref()
            .expect("expected structured tool use");
        assert_eq!(tool_uses.len(), 1);
        assert_eq!(tool_uses[0].tool_use_id, expected_id);

        let Message::User(user) = &history[result_idx] else {
            panic!("expected user at history index {result_idx}");
        };
        let tool_results = &user
            .user_input_message
            .user_input_message_context
            .tool_results;
        assert_eq!(tool_results.len(), 1);
        assert_eq!(tool_results[0].tool_use_id, expected_id);
        assert_eq!(tool_result_text(&tool_results[0]), expected_result);
    }

    #[test]
    fn payload_guard_report_records_body_hash_without_body() {
        let mut request = request_with_history(vec![Message::User(HistoryUserMessage::new(
            "old user content that must not be copied into diagnostics",
            TEST_MODEL,
        ))]);

        let (body, report) =
            guard_kiro_request(&mut request, guard_config(usize::MAX)).expect("guard");

        let expected_hash = sha256_hex(&body);
        assert_eq!(report.body_sha256.as_deref(), Some(expected_hash.as_str()));
        let serialized = serde_json::to_string(&report).unwrap();
        assert!(serialized.contains("bodySha256"));
        assert!(!serialized.contains("old user content"));
        assert!(!serialized.contains(&body));
    }

    #[test]
    fn anthropic_payload_guard_report_records_body_hash_without_body() {
        let mut request = anthropic_request(vec![anthropic_message(
            "user",
            serde_json::json!("current user content that must not be copied into diagnostics"),
        )]);
        let original = serde_json::to_string(&request).unwrap();

        let (body, report) = guard_anthropic_messages_request(
            &mut request,
            guard_config(usize::MAX),
            original.len(),
        )
        .expect("guard");

        let expected_hash = sha256_hex(&body);
        assert_eq!(report.body_sha256.as_deref(), Some(expected_hash.as_str()));
        let serialized = serde_json::to_string(&report).unwrap();
        assert!(serialized.contains("bodySha256"));
        assert!(!serialized.contains("current user content"));
        assert!(!serialized.contains(&body));
    }

    #[test]
    fn clean_anthropic_raw_body_is_zero_copy_and_byte_identical_for_one_hundred_rounds() {
        for content_bytes in [1_024usize, 100 * 1_024, 1_024 * 1_024, 5 * 1_024 * 1_024] {
            let content = "x".repeat(content_bytes);
            let raw = Bytes::from(format!(
                "{{\n  \"futureField\": {{\"preserve\": true}},\n  \"model\": \"claude-sonnet-4-6\",\n  \"max_tokens\": 128,\n  \"messages\": [{{\"role\":\"user\",\"content\":{}}}],\n  \"stream\": false\n}}\n",
                serde_json::to_string(&content).unwrap()
            ));
            let parsed = serde_json::from_slice::<MessagesRequest>(&raw).unwrap();

            for round in 0..100 {
                let mut request = parsed.clone();
                let (guarded, report) = guard_anthropic_messages_request_reusing_body(
                    &mut request,
                    guard_config(raw.len() + 1),
                    &raw,
                )
                .expect("clean raw body should pass without serialization");

                assert_eq!(guarded, raw, "content_bytes={content_bytes}, round={round}");
                assert_eq!(
                    guarded.as_ptr(),
                    raw.as_ptr(),
                    "clean raw Bytes must share the original allocation: content_bytes={content_bytes}, round={round}"
                );
                assert_eq!(report.guard_serializations, 0);
                assert_eq!(report.history_trim_passes, 0);
                assert!(!report.was_modified());
                assert_eq!(report.original_bytes, raw.len());
                assert_eq!(report.final_bytes, raw.len());
                assert_eq!(
                    report.body_sha256.as_deref(),
                    Some(sha256_hex_bytes(&raw).as_str())
                );
                assert!(
                    std::str::from_utf8(&guarded)
                        .unwrap()
                        .contains("futureField")
                );
            }
        }
    }

    #[test]
    fn clean_kiro_guard_serializes_once_and_is_stable_for_one_hundred_rounds() {
        for content_bytes in [1_024usize, 100 * 1_024, 1_024 * 1_024, 5 * 1_024 * 1_024] {
            let mut template = request_with_history(Vec::new());
            template
                .conversation_state
                .current_message
                .user_input_message
                .content = "x".repeat(content_bytes);
            let expected = serialize_kiro_request(&template).expect("baseline Kiro serialization");

            for round in 0..100 {
                let mut request = template.clone();
                let (body, report) = guard_kiro_request(
                    &mut request,
                    guard_config(expected.len().saturating_add(1)),
                )
                .expect("clean Kiro guard");

                assert_eq!(
                    body, expected,
                    "content_bytes={content_bytes}, round={round}"
                );
                assert_eq!(
                    report.guard_serializations, 1,
                    "clean Kiro guard must not perform a redundant second full serialization: content_bytes={content_bytes}, round={round}"
                );
                assert_eq!(report.history_trim_passes, 0);
                assert!(!report.was_modified());
                assert!(!report.still_oversized);
            }
        }
    }

    #[test]
    fn leading_assistant_repair_is_batched_and_stable_for_five_rounds() {
        for entries in [1_000usize, 4_000, 16_000] {
            let messages = (0..entries)
                .map(|index| {
                    anthropic_message(
                        "assistant",
                        serde_json::json!({"type": "text", "text": format!("assistant-{index}")}),
                    )
                })
                .collect::<Vec<_>>();
            let template = anthropic_request(messages);
            let raw = Bytes::from(serde_json::to_vec(&template).expect("raw leading history"));

            for round in 0..5 {
                let mut request = template.clone();
                let (_body, report) = guard_anthropic_messages_request_reusing_body(
                    &mut request,
                    guard_config(raw.len().saturating_add(1)),
                    &raw,
                )
                .expect("batch leading-assistant repair");

                assert_eq!(
                    request.messages.len(),
                    1,
                    "entries={entries}, round={round}"
                );
                assert_eq!(
                    report.aligned_leading_entries,
                    entries - 1,
                    "entries={entries}, round={round}"
                );
                assert_eq!(report.guard_serializations, 1);
                assert_eq!(report.history_trim_passes, 0);
            }
        }
    }

    #[test]
    #[ignore = "run in release as an isolated clean/dirty payload size and serialization probe"]
    fn payload_guard_release_size_matrix_probe() {
        fn percentile(sorted: &[u128], percentile: usize) -> u128 {
            let index = ((sorted.len() * percentile).saturating_sub(1) / 100).min(sorted.len() - 1);
            sorted[index]
        }

        fn dirty_request(content_bytes: usize) -> MessagesRequest {
            anthropic_request(vec![
                anthropic_message("user", Value::String("x".repeat(content_bytes))),
                anthropic_message(
                    "assistant",
                    serde_json::json!([{
                        "type": "tool_use",
                        "id": "tool-valid",
                        "name": "Bash",
                        "input": {"command": "true"}
                    }]),
                ),
                anthropic_message(
                    "user",
                    serde_json::json!([
                        {"type": "tool_result", "tool_use_id": "tool-valid", "content": "valid"},
                        {"type": "tool_result", "tool_use_id": "tool-valid", "content": "duplicate-marker"},
                        {"type": "tool_result", "tool_use_id": "tool-orphan", "content": "orphan-marker"},
                        {"type": "text", "text": "current"}
                    ]),
                ),
            ])
        }

        const SIZES: [usize; 4] = [1 << 10, 100 << 10, 1 << 20, 5 << 20];
        const ROUNDS: usize = 5;

        for content_bytes in SIZES {
            for mode in ["clean_anthropic", "dirty_anthropic", "clean_kiro"] {
                let (anthropic_template, raw, kiro_template) = match mode {
                    "clean_anthropic" => {
                        let request = anthropic_request(vec![anthropic_message(
                            "user",
                            Value::String("x".repeat(content_bytes)),
                        )]);
                        let raw = Bytes::from(format!(
                            " {{\n  \"future_field\" : true,\n  \"model\" : \"claude-sonnet-4-6\",\n  \"max_tokens\" : 128,\n  \"messages\" : [{{\"role\":\"user\",\"content\":{}}}],\n  \"stream\" : false\n }} ",
                            serde_json::to_string(&"x".repeat(content_bytes)).unwrap()
                        ));
                        (Some(request), Some(raw), None)
                    }
                    "dirty_anthropic" => {
                        let request = dirty_request(content_bytes);
                        let raw = Bytes::from(serde_json::to_vec(&request).unwrap());
                        (Some(request), Some(raw), None)
                    }
                    "clean_kiro" => {
                        let mut request = request_with_history(Vec::new());
                        request
                            .conversation_state
                            .current_message
                            .user_input_message
                            .content = "x".repeat(content_bytes);
                        (None, None, Some(request))
                    }
                    _ => unreachable!(),
                };
                let input_bytes = raw
                    .as_ref()
                    .map(Bytes::len)
                    .or_else(|| {
                        kiro_template
                            .as_ref()
                            .map(|request| serialize_kiro_request(request).unwrap().len())
                    })
                    .unwrap();
                let mut latencies_us = Vec::with_capacity(ROUNDS);
                let mut observed_serializations = Vec::with_capacity(ROUNDS);

                for round in 0..ROUNDS {
                    let started = Instant::now();
                    let report = if let (Some(template), Some(raw)) =
                        (anthropic_template.as_ref(), raw.as_ref())
                    {
                        let mut request = template.clone();
                        let (body, report) = guard_anthropic_messages_request_reusing_body(
                            &mut request,
                            guard_config(raw.len().saturating_add(1)),
                            raw,
                        )
                        .expect("Anthropic perf probe");
                        if mode == "clean_anthropic" {
                            assert_eq!(body, *raw, "round {round}");
                            assert_eq!(body.as_ptr(), raw.as_ptr(), "round {round}");
                            assert_eq!(report.guard_serializations, 0, "round {round}");
                        } else {
                            assert!(report.was_modified(), "round {round}");
                            assert_eq!(report.guard_serializations, 1, "round {round}");
                            assert!(!body.windows(16).any(|window| window == b"duplicate-marker"));
                            assert!(!body.windows(13).any(|window| window == b"orphan-marker"));
                        }
                        report
                    } else {
                        let mut request = kiro_template.as_ref().unwrap().clone();
                        let (body, report) = guard_kiro_request(
                            &mut request,
                            guard_config(input_bytes.saturating_add(1)),
                        )
                        .expect("Kiro perf probe");
                        assert_eq!(body.len(), input_bytes, "round {round}");
                        assert_eq!(report.guard_serializations, 1, "round {round}");
                        report
                    };
                    latencies_us.push(started.elapsed().as_micros());
                    observed_serializations.push(report.guard_serializations);
                }

                latencies_us.sort_unstable();
                println!(
                    "PAYLOAD_GUARD_PERF mode={} input_bytes={} rounds={} latency_us_p50={} latency_us_p95={} latency_us_p99={} serializations={:?}",
                    mode,
                    input_bytes,
                    ROUNDS,
                    percentile(&latencies_us, 50),
                    percentile(&latencies_us, 95),
                    percentile(&latencies_us, 99),
                    observed_serializations,
                );
            }
        }
    }

    #[test]
    fn guard_trims_old_history_until_under_limit() {
        let mut history = Vec::new();
        for idx in 0..10 {
            history.push(Message::User(HistoryUserMessage::new(
                format!("user {} {}", idx, "x".repeat(500)),
                TEST_MODEL,
            )));
            history.push(Message::Assistant(HistoryAssistantMessage::new(format!(
                "assistant {} {}",
                idx,
                "y".repeat(500)
            ))));
        }
        let mut request = request_with_history(history);
        let (body, report) =
            guard_kiro_request(&mut request, guard_config(5_000)).expect("guard should trim");

        assert!(body.len() <= 5_000);
        assert!(report.trimmed_history_entries > 0);
        assert!(request.conversation_state.history.len() < report.original_history_entries);
        assert!(matches!(
            request.conversation_state.history.first(),
            Some(Message::User(_))
        ));
    }

    #[test]
    fn anthropic_guard_trims_old_history_until_under_limit() {
        let mut messages = Vec::new();
        for idx in 0..24 {
            messages.push(anthropic_message(
                "user",
                serde_json::json!(format!("history {} {}", idx, "x".repeat(600))),
            ));
            messages.push(anthropic_message(
                "assistant",
                serde_json::json!([{
                    "type": "text",
                    "text": format!("answer {} {}", idx, "y".repeat(400)),
                }]),
            ));
        }
        messages.push(anthropic_message(
            "user",
            serde_json::json!("current question"),
        ));
        let mut request = anthropic_request(messages);
        let original = serde_json::to_string(&request).unwrap();

        let (body, report) =
            guard_anthropic_messages_request(&mut request, guard_config(8_000), original.len())
                .expect("external guard should trim");

        assert!(body.len() <= 8_000, "body len was {}", body.len());
        assert!(report.trimmed_history_entries > 0);
        assert!(!report.still_oversized);
        assert_eq!(
            request.messages.last().unwrap().content,
            serde_json::json!("current question")
        );

        let breakdown = breakdown_anthropic_messages_request(&request, body.len());
        assert_eq!(breakdown.total_bytes, body.len());
        assert_eq!(
            breakdown.history_entries,
            request.messages.len().saturating_sub(1)
        );
    }

    #[test]
    fn kiro_history_trim_removes_a_complete_logical_tool_turn_atomically() {
        let assistant = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage::new("calling")
                .with_tool_uses(vec![ToolUseEntry::new("tool-old", "Bash")]),
        };
        let mut result = HistoryUserMessage::new("tool result envelope", TEST_MODEL);
        result.user_input_message.user_input_message_context = UserInputMessageContext::new()
            .with_tool_results(vec![ToolResult::success("tool-old", "secret old output")]);
        let mut history = vec![
            Message::User(HistoryUserMessage::new("old prompt", TEST_MODEL)),
            Message::Assistant(assistant),
            Message::User(result),
            Message::Assistant(HistoryAssistantMessage::new("old final")),
            Message::User(HistoryUserMessage::new("next prompt", TEST_MODEL)),
            Message::Assistant(HistoryAssistantMessage::new("next answer")),
        ];

        let current_bytes = json_len(&history);
        let target_bytes = current_bytes.saturating_sub(json_array_prefix_reduction(&history, 4));
        let removed =
            trim_history_to_estimated_budget(&mut history, &[], current_bytes, target_bytes);

        assert_eq!(removed, 4);
        assert_eq!(history.len(), 2);
        let Message::User(first) = &history[0] else {
            panic!("next logical turn must start with its user prompt");
        };
        assert_eq!(first.user_input_message.content, "next prompt");
        let serialized = serde_json::to_string(&history).unwrap();
        assert!(!serialized.contains("old prompt"));
        assert!(!serialized.contains("secret old output"));
        assert!(!serialized.contains("old final"));
    }

    #[test]
    fn kiro_history_trim_preserves_the_active_current_tool_result_pair() {
        let active_assistant = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage::new("calling")
                .with_tool_uses(vec![ToolUseEntry::new("tool-active", "Bash")]),
        };
        let mut history = vec![
            Message::User(HistoryUserMessage::new("active prompt", TEST_MODEL)),
            Message::Assistant(active_assistant),
        ];
        let current_results = vec![ToolResult::success("tool-active", "active output")];

        let current_bytes = json_len(&history);
        assert_eq!(
            trim_history_to_estimated_budget(&mut history, &current_results, current_bytes, 0),
            0
        );
        assert_eq!(history.len(), 2);

        let mut request = request_with_history(history);
        request
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context =
            UserInputMessageContext::new().with_tool_results(current_results);
        let (_body, report) = guard_kiro_request(&mut request, guard_config(1)).expect("guard");
        assert_eq!(report.trimmed_history_entries, 0);
        assert_eq!(report.removed_orphan_tool_results, 0);
        assert_eq!(report.removed_orphan_tool_uses, 0);
        assert!(report.still_oversized);
        assert_eq!(request.conversation_state.history.len(), 2);
        assert_eq!(
            request
                .conversation_state
                .current_message
                .user_input_message
                .user_input_message_context
                .tool_results
                .len(),
            1
        );
    }

    #[test]
    fn anthropic_history_trim_removes_a_complete_logical_tool_turn_atomically() {
        let mut messages = vec![
            anthropic_message("user", serde_json::json!("old prompt")),
            anthropic_message(
                "assistant",
                serde_json::json!([
                    {"type": "text", "text": "calling"},
                    {"type": "tool_use", "id": "tool-old", "name": "Bash", "input": {}}
                ]),
            ),
            anthropic_message(
                "user",
                serde_json::json!([{
                    "type": "tool_result",
                    "tool_use_id": "tool-old",
                    "content": "secret old output"
                }]),
            ),
            anthropic_message("assistant", serde_json::json!("old final")),
            anthropic_message("user", serde_json::json!("next prompt")),
        ];

        let current_bytes = json_len(&messages);
        let target_bytes = current_bytes.saturating_sub(json_array_prefix_reduction(&messages, 4));
        let removed =
            trim_anthropic_history_to_estimated_budget(&mut messages, current_bytes, target_bytes);

        assert_eq!(removed, 4);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, serde_json::json!("next prompt"));
    }

    #[test]
    fn anthropic_history_trim_preserves_an_active_tool_result_pair() {
        let mut messages = vec![
            anthropic_message("user", serde_json::json!("active prompt")),
            anthropic_message(
                "assistant",
                serde_json::json!([{
                    "type": "tool_use",
                    "id": "tool-active",
                    "name": "Bash",
                    "input": {}
                }]),
            ),
            anthropic_message(
                "user",
                serde_json::json!([{
                    "type": "tool_result",
                    "tool_use_id": "tool-active",
                    "content": "active output"
                }]),
            ),
        ];
        let original = serde_json::to_value(&messages).unwrap();

        let current_bytes = json_len(&messages);
        assert_eq!(
            trim_anthropic_history_to_estimated_budget(&mut messages, current_bytes, 0),
            0
        );
        assert_eq!(serde_json::to_value(&messages).unwrap(), original);
    }

    #[test]
    fn anthropic_history_batch_trim_matches_exact_json_reduction() {
        let mut messages = Vec::new();
        for idx in 0..200 {
            messages.push(anthropic_message(
                "user",
                serde_json::json!(format!("history-{idx}-{}", "x".repeat(128))),
            ));
            messages.push(anthropic_message(
                "assistant",
                serde_json::json!([{
                    "type": "text",
                    "text": format!("answer-{idx}-{}", "y".repeat(128)),
                }]),
            ));
        }
        messages.push(anthropic_message("user", serde_json::json!("current")));

        let before = serde_json::to_vec(&messages).unwrap().len();
        let expected_prefix = 300usize;
        let expected_reduction = json_array_prefix_reduction(&messages, expected_prefix);
        let target = before.saturating_sub(expected_reduction);
        let removed = trim_anthropic_history_to_estimated_budget(&mut messages, before, target);
        let after = serde_json::to_vec(&messages).unwrap().len();

        assert_eq!(removed, expected_prefix);
        assert_eq!(before.saturating_sub(after), expected_reduction);
        assert_eq!(after, target);
        assert_eq!(
            messages.last().unwrap().content,
            serde_json::json!("current")
        );
        assert_eq!(messages.first().unwrap().role, "user");
    }

    #[test]
    fn kiro_history_batch_trim_matches_exact_json_reduction() {
        let mut history = Vec::new();
        for idx in 0..200 {
            history.push(Message::User(HistoryUserMessage::new(
                format!("history-{idx}-{}", "x".repeat(128)),
                TEST_MODEL,
            )));
            history.push(Message::Assistant(HistoryAssistantMessage::new(format!(
                "answer-{idx}-{}",
                "y".repeat(128)
            ))));
        }

        let before = serde_json::to_vec(&history).unwrap().len();
        let expected_prefix = 300usize;
        let expected_reduction = json_array_prefix_reduction(&history, expected_prefix);
        let target = before.saturating_sub(expected_reduction);
        let removed = trim_history_to_estimated_budget(&mut history, &[], before, target);
        let after = serde_json::to_vec(&history).unwrap().len();

        assert_eq!(removed, expected_prefix);
        assert_eq!(before.saturating_sub(after), expected_reduction);
        assert_eq!(after, target);
        assert!(matches!(history.first(), Some(Message::User(_))));
    }

    #[test]
    fn anthropic_guard_large_history_uses_one_trim_pass_and_constant_serializations() {
        let mut messages = Vec::new();
        for idx in 0..1_000 {
            messages.push(anthropic_message(
                "user",
                serde_json::json!(format!("history-{idx}-{}", "x".repeat(256))),
            ));
            messages.push(anthropic_message(
                "assistant",
                serde_json::json!([{
                    "type": "text",
                    "text": format!("answer-{idx}-{}", "y".repeat(256)),
                }]),
            ));
        }
        messages.push(anthropic_message("user", serde_json::json!("current")));
        let mut request = anthropic_request(messages);
        let original = serde_json::to_vec(&request).unwrap();

        let (body, report) =
            guard_anthropic_messages_request(&mut request, guard_config(40_000), original.len())
                .expect("large Anthropic history should be trimmed in one batch");

        assert!(body.len() <= 40_000);
        assert_eq!(report.history_trim_passes, 1);
        assert!(report.guard_serializations <= 2, "report={report:?}");
        assert!(report.trimmed_history_entries > 1_000);
        assert_eq!(
            request.messages.last().unwrap().content,
            serde_json::json!("current")
        );
    }

    #[test]
    fn kiro_guard_large_history_uses_one_trim_pass_and_constant_serializations() {
        let mut history = Vec::new();
        for idx in 0..1_000 {
            history.push(Message::User(HistoryUserMessage::new(
                format!("history-{idx}-{}", "x".repeat(256)),
                TEST_MODEL,
            )));
            history.push(Message::Assistant(HistoryAssistantMessage::new(format!(
                "answer-{idx}-{}",
                "y".repeat(256)
            ))));
        }
        let mut request = request_with_history(history);

        let (body, report) = guard_kiro_request(&mut request, guard_config(40_000))
            .expect("large Kiro history should be trimmed in one batch");

        assert!(body.len() <= 40_000);
        assert_eq!(report.history_trim_passes, 1);
        assert!(report.guard_serializations <= 3, "report={report:?}");
        assert!(report.trimmed_history_entries > 1_000);
        assert_eq!(
            request
                .conversation_state
                .current_message
                .user_input_message
                .content,
            "current"
        );
    }

    #[test]
    fn anthropic_guard_keeps_first_valid_result_and_drops_duplicate_and_orphan_content() {
        let messages = vec![
            anthropic_message("user", serde_json::json!("run")),
            anthropic_message(
                "assistant",
                serde_json::json!([{
                    "type": "tool_use",
                    "id": "tool-1",
                    "name": "Bash",
                    "input": {}
                }]),
            ),
            anthropic_message(
                "user",
                serde_json::json!([
                    {"type": "tool_result", "tool_use_id": "tool-1", "content": "first valid output"},
                    {"type": "tool_result", "tool_use_id": "tool-1", "content": "duplicate secret output"},
                    {"type": "tool_result", "tool_use_id": "tool-orphan", "content": "orphan secret output"},
                    {"type": "text", "text": "safe user text"}
                ]),
            ),
        ];
        let mut request = anthropic_request(messages);
        let original_len = serde_json::to_vec(&request).unwrap().len();

        let (body, report) =
            guard_anthropic_messages_request(&mut request, guard_config(usize::MAX), original_len)
                .expect("guard");

        assert_eq!(report.removed_duplicate_tool_results, 1);
        assert_eq!(report.removed_orphan_tool_results, 1);
        assert_eq!(report.textified_duplicate_tool_results, 0);
        assert_eq!(report.textified_orphan_tool_results, 0);
        let serialized = body;
        assert!(serialized.contains("first valid output"));
        assert!(serialized.contains("safe user text"));
        assert!(!serialized.contains("duplicate secret output"));
        assert!(!serialized.contains("orphan secret output"));
    }

    #[test]
    fn guard_repairs_orphan_tool_results_after_trim() {
        let assistant = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage::new("tool call")
                .with_tool_uses(vec![ToolUseEntry::new("tool-1", "readFile")]),
        };
        let mut user = HistoryUserMessage::new("result message", TEST_MODEL);
        user.user_input_message.user_input_message_context = UserInputMessageContext::new()
            .with_tool_results(vec![
                ToolResult::success("tool-1", "valid result"),
                ToolResult::success("tool-orphan", "orphan result"),
            ]);

        let mut request = request_with_history(vec![
            Message::User(HistoryUserMessage::new("old", TEST_MODEL)),
            Message::Assistant(assistant),
            Message::User(user),
        ]);

        let (_body, report) = guard_kiro_request(&mut request, guard_config(usize::MAX))
            .expect("guard should repair");

        assert_eq!(report.removed_orphan_tool_results, 1);
        assert_eq!(report.flattened_history_tool_uses, 0);
        assert_eq!(report.textified_history_tool_results, 0);
        assert_structured_history_tool_turn(
            &request.conversation_state.history,
            1,
            2,
            "tool-1",
            "valid result",
        );
        let Message::User(user) = &request.conversation_state.history[2] else {
            panic!("expected user");
        };
        assert_eq!(user.user_input_message.content, "result message");
        assert!(!user.user_input_message.content.contains("orphan result"));
        assert!(!user.user_input_message.content.contains("valid result"));
    }

    #[test]
    fn guard_normalizes_empty_history_tool_result_content() {
        let assistant = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage::new("tool call")
                .with_tool_uses(vec![ToolUseEntry::new("tool-1", "readFile")]),
        };
        let mut user = HistoryUserMessage::new("result message", TEST_MODEL);
        user.user_input_message.user_input_message_context = UserInputMessageContext::new()
            .with_tool_results(vec![ToolResult::success("tool-1", "")]);

        let mut request = request_with_history(vec![
            Message::User(HistoryUserMessage::new("read", TEST_MODEL)),
            Message::Assistant(assistant),
            Message::User(user),
        ]);

        let (body, report) =
            guard_kiro_request(&mut request, guard_config(usize::MAX)).expect("guard");

        assert_eq!(report.removed_orphan_tool_results, 0);
        assert_eq!(report.flattened_history_tool_uses, 0);
        assert_eq!(report.textified_history_tool_results, 0);
        assert!(body.contains(EMPTY_TOOL_RESULT_CONTENT_PLACEHOLDER));
        assert_structured_history_tool_turn(
            &request.conversation_state.history,
            1,
            2,
            "tool-1",
            EMPTY_TOOL_RESULT_CONTENT_PLACEHOLDER,
        );
    }

    #[test]
    fn guard_normalizes_empty_current_tool_result_content() {
        let assistant = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage::new("tool call")
                .with_tool_uses(vec![ToolUseEntry::new("tool-1", "readFile")]),
        };
        let mut current = UserInputMessage::new("current result", TEST_MODEL);
        current.user_input_message_context = UserInputMessageContext::new()
            .with_tool_results(vec![ToolResult::success("tool-1", "   ")]);
        let mut request = request_with_history(vec![
            Message::User(HistoryUserMessage::new("read", TEST_MODEL)),
            Message::Assistant(assistant),
        ]);
        request.conversation_state.current_message = CurrentMessage::new(current);

        let (body, report) =
            guard_kiro_request(&mut request, guard_config(usize::MAX)).expect("guard");

        assert_eq!(report.removed_orphan_tool_results, 0);
        assert!(body.contains(EMPTY_TOOL_RESULT_CONTENT_PLACEHOLDER));
        assert_eq!(
            tool_result_text(
                &request
                    .conversation_state
                    .current_message
                    .user_input_message
                    .user_input_message_context
                    .tool_results[0]
            ),
            EMPTY_TOOL_RESULT_CONTENT_PLACEHOLDER
        );
    }

    #[test]
    fn guard_preserves_completed_history_tool_cycles_for_plain_current_message() {
        let first_assistant = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage::new("running build")
                .with_tool_uses(vec![ToolUseEntry::new("tool-1", "exec_command")]),
        };
        let mut first_result = HistoryUserMessage::new("", TEST_MODEL);
        first_result.user_input_message.user_input_message_context = UserInputMessageContext::new()
            .with_tool_results(vec![ToolResult::success("tool-1", "build ok")]);

        let second_assistant = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage::new("")
                .with_tool_uses(vec![ToolUseEntry::new("tool-2", "exec_command")]),
        };
        let mut second_result = HistoryUserMessage::new("", TEST_MODEL);
        second_result.user_input_message.user_input_message_context =
            UserInputMessageContext::new()
                .with_tool_results(vec![ToolResult::success("tool-2", "tests pass")]);

        let mut request = request_with_history(vec![
            Message::User(HistoryUserMessage::new("run the build", TEST_MODEL)),
            Message::Assistant(first_assistant),
            Message::User(first_result),
            Message::Assistant(second_assistant),
            Message::User(second_result),
        ]);
        request
            .conversation_state
            .current_message
            .user_input_message
            .content = "Summarize everything above.".to_string();

        let (_body, report) =
            guard_kiro_request(&mut request, guard_config(usize::MAX)).expect("guard");

        assert_eq!(report.flattened_history_tool_uses, 0);
        assert_eq!(report.textified_history_tool_results, 0);
        assert_eq!(report.removed_orphan_tool_uses, 0);
        assert_eq!(report.removed_orphan_tool_results, 0);
        assert_structured_history_tool_turn(
            &request.conversation_state.history,
            1,
            2,
            "tool-1",
            "build ok",
        );
        assert_structured_history_tool_turn(
            &request.conversation_state.history,
            3,
            4,
            "tool-2",
            "tests pass",
        );
        let Message::User(first_result) = &request.conversation_state.history[2] else {
            panic!("expected first result user");
        };
        let Message::User(second_result) = &request.conversation_state.history[4] else {
            panic!("expected second result user");
        };
        assert!(first_result.user_input_message.content.is_empty());
        assert!(second_result.user_input_message.content.is_empty());
        assert!(
            request
                .conversation_state
                .current_message
                .user_input_message
                .user_input_message_context
                .tool_results
                .is_empty()
        );
    }

    #[test]
    fn guard_preserves_completed_and_active_current_tool_turns() {
        let completed_assistant = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage::new("first")
                .with_tool_uses(vec![ToolUseEntry::new("tool-old", "readFile")]),
        };
        let mut completed_result = HistoryUserMessage::new("", TEST_MODEL);
        completed_result
            .user_input_message
            .user_input_message_context = UserInputMessageContext::new()
            .with_tool_results(vec![ToolResult::success("tool-old", "old content")]);
        let active_assistant = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage::new("active")
                .with_tool_uses(vec![ToolUseEntry::new("tool-current", "readFile")]),
        };
        let mut current = UserInputMessage::new("", TEST_MODEL);
        current.user_input_message_context = UserInputMessageContext::new()
            .with_tool_results(vec![ToolResult::success("tool-current", "current content")]);

        let mut request = request_with_history(vec![
            Message::User(HistoryUserMessage::new("read old", TEST_MODEL)),
            Message::Assistant(completed_assistant),
            Message::User(completed_result),
            Message::Assistant(active_assistant),
        ]);
        request.conversation_state.current_message = CurrentMessage::new(current);

        let (_body, report) =
            guard_kiro_request(&mut request, guard_config(usize::MAX)).expect("guard");

        assert_eq!(report.flattened_history_tool_uses, 0);
        assert_eq!(report.textified_history_tool_results, 0);
        assert_structured_history_tool_turn(
            &request.conversation_state.history,
            1,
            2,
            "tool-old",
            "old content",
        );

        let Message::Assistant(active_assistant) =
            request.conversation_state.history.last().unwrap()
        else {
            panic!("expected active assistant");
        };
        assert_eq!(
            active_assistant
                .assistant_response_message
                .tool_uses
                .as_ref()
                .expect("active tool use")[0]
                .tool_use_id,
            "tool-current"
        );
        assert_eq!(
            request
                .conversation_state
                .current_message
                .user_input_message
                .user_input_message_context
                .tool_results[0]
                .tool_use_id,
            "tool-current"
        );
    }

    #[test]
    fn anthropic_guard_normalizes_empty_tool_result_content_without_trimming() {
        let mut request = anthropic_request(vec![
            anthropic_message("user", serde_json::json!("read")),
            anthropic_message(
                "assistant",
                serde_json::json!([
                    {"type": "tool_use", "id": "tool-1", "name": "readFile", "input": {"path": "/tmp/a"}}
                ]),
            ),
            anthropic_message(
                "user",
                serde_json::json!([
                    {"type": "tool_result", "tool_use_id": "tool-1", "content": []}
                ]),
            ),
        ]);
        let original = serde_json::to_string(&request).unwrap();

        let (body, report) = guard_anthropic_messages_request(
            &mut request,
            guard_config(usize::MAX),
            original.len(),
        )
        .expect("guard");

        assert_eq!(report.removed_orphan_tool_results, 0);
        assert!(body.contains(EMPTY_TOOL_RESULT_CONTENT_PLACEHOLDER));
        let blocks = request.messages[2].content.as_array().expect("blocks");
        assert_eq!(
            blocks[0]["content"][0]["text"],
            EMPTY_TOOL_RESULT_CONTENT_PLACEHOLDER
        );
    }

    #[test]
    fn guard_renames_repeated_tool_use_ids_and_matching_results() {
        let assistant_one = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage::new("first tool call")
                .with_tool_uses(vec![ToolUseEntry::new("tool-1", "readFile")]),
        };
        let mut result_one = HistoryUserMessage::new("first result", TEST_MODEL);
        result_one.user_input_message.user_input_message_context = UserInputMessageContext::new()
            .with_tool_results(vec![ToolResult::success("tool-1", "first content")]);

        let assistant_two = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage::new("second tool call")
                .with_tool_uses(vec![ToolUseEntry::new("tool-1", "readFile")]),
        };
        let mut result_two = HistoryUserMessage::new("second result", TEST_MODEL);
        result_two.user_input_message.user_input_message_context = UserInputMessageContext::new()
            .with_tool_results(vec![ToolResult::success("tool-1", "second content")]);

        let mut request = request_with_history(vec![
            Message::User(HistoryUserMessage::new("read first", TEST_MODEL)),
            Message::Assistant(assistant_one),
            Message::User(result_one),
            Message::Assistant(assistant_two),
            Message::User(result_two),
        ]);

        let (_body, report) =
            guard_kiro_request(&mut request, guard_config(usize::MAX)).expect("guard");

        assert_eq!(report.renamed_duplicate_tool_uses, 1);
        assert_eq!(report.removed_orphan_tool_uses, 0);
        assert_eq!(report.removed_orphan_tool_results, 0);
        assert_eq!(report.flattened_history_tool_uses, 0);
        assert_eq!(report.textified_history_tool_results, 0);
        assert_structured_history_tool_turn(
            &request.conversation_state.history,
            1,
            2,
            "tool-1",
            "first content",
        );

        let Message::Assistant(assistant) = &request.conversation_state.history[3] else {
            panic!("expected second assistant");
        };
        let renamed_id = assistant
            .assistant_response_message
            .tool_uses
            .as_ref()
            .expect("second structured tool use")[0]
            .tool_use_id
            .clone();
        assert_ne!(renamed_id, "tool-1");
        assert_structured_history_tool_turn(
            &request.conversation_state.history,
            3,
            4,
            &renamed_id,
            "second content",
        );
        let Message::User(user) = &request.conversation_state.history[4] else {
            panic!("expected second result user");
        };
        assert_eq!(user.user_input_message.content, "second result");
    }

    #[test]
    fn guard_renames_repeated_tool_use_id_for_current_result() {
        let assistant_one = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage::new("first tool call")
                .with_tool_uses(vec![ToolUseEntry::new("tool-1", "readFile")]),
        };
        let mut result_one = HistoryUserMessage::new("first result", TEST_MODEL);
        result_one.user_input_message.user_input_message_context = UserInputMessageContext::new()
            .with_tool_results(vec![ToolResult::success("tool-1", "first content")]);
        let assistant_two = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage::new("current tool call")
                .with_tool_uses(vec![ToolUseEntry::new("tool-1", "readFile")]),
        };
        let mut request = request_with_history(vec![
            Message::User(HistoryUserMessage::new("read first", TEST_MODEL)),
            Message::Assistant(assistant_one),
            Message::User(result_one),
            Message::Assistant(assistant_two),
        ]);
        request
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context = UserInputMessageContext::new()
            .with_tool_results(vec![ToolResult::success("tool-1", "current content")]);

        let (_body, report) =
            guard_kiro_request(&mut request, guard_config(usize::MAX)).expect("guard");

        assert_eq!(report.renamed_duplicate_tool_uses, 1);
        assert_eq!(report.flattened_history_tool_uses, 0);
        assert_eq!(report.textified_history_tool_results, 0);
        assert_structured_history_tool_turn(
            &request.conversation_state.history,
            1,
            2,
            "tool-1",
            "first content",
        );
        let Message::Assistant(assistant) = request.conversation_state.history.last().unwrap()
        else {
            panic!("expected last assistant");
        };
        let renamed_id = &assistant
            .assistant_response_message
            .tool_uses
            .as_ref()
            .expect("tool use")[0]
            .tool_use_id;
        assert_eq!(
            &request
                .conversation_state
                .current_message
                .user_input_message
                .user_input_message_context
                .tool_results[0]
                .tool_use_id,
            renamed_id
        );
    }

    #[test]
    fn guard_drops_duplicate_current_tool_results_without_textifying() {
        let assistant = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage::new("tool call").with_tool_uses(vec![
                ToolUseEntry::new("tool-1", "readFile"),
                ToolUseEntry::new("tool-2", "readFile"),
            ]),
        };
        let mut request = request_with_history(vec![
            Message::User(HistoryUserMessage::new("read", TEST_MODEL)),
            Message::Assistant(assistant),
        ]);
        request
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context = UserInputMessageContext::new().with_tool_results(vec![
            ToolResult::success("tool-1", "first result"),
            ToolResult::success("tool-1", "duplicate result"),
            ToolResult::success("tool-2", "second result"),
        ]);

        let (_body, report) =
            guard_kiro_request(&mut request, guard_config(usize::MAX)).expect("guard");

        assert_eq!(report.removed_duplicate_tool_results, 1);
        assert_eq!(report.textified_duplicate_tool_results, 0);
        let current = &request
            .conversation_state
            .current_message
            .user_input_message;
        assert_eq!(current.user_input_message_context.tool_results.len(), 2);
        assert_eq!(
            tool_result_text(&current.user_input_message_context.tool_results[0]),
            "first result"
        );
        assert!(!current.content.contains("duplicate result"));
    }

    #[test]
    fn guard_counts_empty_tool_result_id_as_orphan_without_exposing_content() {
        let assistant = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage::new("tool call")
                .with_tool_uses(vec![ToolUseEntry::new("tool-1", "readFile")]),
        };
        let mut request = request_with_history(vec![
            Message::User(HistoryUserMessage::new("read", TEST_MODEL)),
            Message::Assistant(assistant),
        ]);
        let current = &mut request
            .conversation_state
            .current_message
            .user_input_message;
        current.content = "safe current text".to_string();
        current.user_input_message_context = UserInputMessageContext::new()
            .with_tool_results(vec![ToolResult::success("", "empty-id secret result")]);

        let (body, report) =
            guard_kiro_request(&mut request, guard_config(usize::MAX)).expect("guard");

        assert_eq!(report.removed_orphan_tool_results, 1);
        assert_eq!(report.removed_duplicate_tool_results, 0);
        assert_eq!(report.textified_orphan_tool_results, 0);
        assert_eq!(
            request
                .conversation_state
                .current_message
                .user_input_message
                .content,
            "safe current text"
        );
        assert!(!body.contains("empty-id secret result"));
    }

    #[test]
    fn guard_marks_oversized_without_rejecting_current_message() {
        let mut request = KiroRequest {
            conversation_state: ConversationState::new("conv-test").with_current_message(
                CurrentMessage::new(UserInputMessage::new("x".repeat(10_000), TEST_MODEL)),
            ),
            profile_arn: None,
            additional_model_request_fields: None,
            tool_cache_point_insert_after: Vec::new(),
            cache_point_plan_recording_enabled: true,
        };

        let (body, report) = guard_kiro_request(&mut request, guard_config(1_000))
            .expect("oversized current message should be passed through to Kiro");

        assert!(body.len() > 1_000);
        assert!(report.still_oversized);
        assert_eq!(report.final_bytes, body.len());
        assert_eq!(report.trimmed_history_entries, 0);
    }

    #[test]
    fn guard_zero_max_bytes_repairs_without_size_limit() {
        let assistant = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage {
                content: "empty tools".to_string(),
                tool_uses: Some(Vec::new()),
                reasoning_content: None,
            },
        };
        let mut request = request_with_history(vec![
            Message::User(HistoryUserMessage::new("user", TEST_MODEL)),
            Message::Assistant(assistant),
        ]);
        request
            .conversation_state
            .current_message
            .user_input_message
            .content = "x".repeat(10_000);

        let (body, report) = guard_kiro_request(
            &mut request,
            PayloadGuardConfig {
                enabled: true,
                max_bytes: 0,
                trim_history: true,
                shaping: PayloadShapingConfig::default(),
            },
        )
        .expect("zero max bytes should disable only size limiting");

        assert!(body.len() > 1_000);
        assert_eq!(report.max_bytes, 0);
        assert_eq!(report.final_bytes, body.len());
        assert_eq!(report.trimmed_history_entries, 0);
        assert_eq!(report.removed_empty_tool_uses, 1);
        assert!(!report.still_oversized);
        assert_eq!(request.conversation_state.history.len(), 2);
        let Message::Assistant(assistant) = &request.conversation_state.history[1] else {
            panic!("expected assistant");
        };
        assert!(assistant.assistant_response_message.tool_uses.is_none());
    }

    #[test]
    fn guard_zero_max_bytes_preserves_multi_turn_structured_tool_history() {
        const TOOL_CYCLES: usize = 12;
        let mut history = vec![Message::User(HistoryUserMessage::new(
            "run all checks",
            TEST_MODEL,
        ))];
        for idx in 0..TOOL_CYCLES {
            let tool_use_id = format!("tool-{idx}");
            history.push(Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: AssistantMessage::new(format!("running check {idx}"))
                    .with_tool_uses(vec![ToolUseEntry::new(tool_use_id.clone(), "exec_command")]),
            }));
            let mut result = HistoryUserMessage::new(format!("result turn {idx}"), TEST_MODEL);
            result.user_input_message.user_input_message_context = UserInputMessageContext::new()
                .with_tool_results(vec![ToolResult::success(
                    tool_use_id,
                    format!("check {idx} passed"),
                )]);
            history.push(Message::User(result));
        }

        let mut request = request_with_history(history);
        request
            .conversation_state
            .current_message
            .user_input_message
            .content = "summarize the checks".to_string();

        let (body, report) = guard_kiro_request(
            &mut request,
            PayloadGuardConfig {
                enabled: true,
                max_bytes: 0,
                trim_history: true,
                shaping: PayloadShapingConfig::default(),
            },
        )
        .expect("zero max bytes should preserve valid structured history");

        assert_eq!(report.max_bytes, 0);
        assert_eq!(report.trimmed_history_entries, 0);
        assert_eq!(report.flattened_history_tool_uses, 0);
        assert_eq!(report.textified_history_tool_results, 0);
        assert!(!report.still_oversized);
        assert_eq!(
            request.conversation_state.history.len(),
            1 + TOOL_CYCLES * 2
        );

        let serialized: Value = serde_json::from_str(&body).expect("serialized Kiro request");
        let serialized_history = serialized["conversationState"]["history"]
            .as_array()
            .expect("serialized history");
        for idx in 0..TOOL_CYCLES {
            let assistant_idx = 1 + idx * 2;
            let result_idx = assistant_idx + 1;
            let tool_use_id = format!("tool-{idx}");
            let result_text = format!("check {idx} passed");
            assert_structured_history_tool_turn(
                &request.conversation_state.history,
                assistant_idx,
                result_idx,
                &tool_use_id,
                &result_text,
            );
            assert!(
                serialized_history[assistant_idx]["assistantResponseMessage"]["toolUses"]
                    .is_array()
            );
            assert!(
                serialized_history[result_idx]["userInputMessage"]["userInputMessageContext"]
                    ["toolResults"]
                    .is_array()
            );
        }

        let breakdown = breakdown_kiro_request(&request, &body);
        assert_eq!(breakdown.history_tool_use_count, TOOL_CYCLES);
        assert_eq!(breakdown.history_tool_result_count, TOOL_CYCLES);
    }

    #[test]
    fn converted_twelve_cycle_bash_history_stays_structured_without_transcript_text() {
        const TOOL_CYCLES: usize = 12;
        let mut messages = vec![AnthropicMessage {
            role: "user".to_string(),
            content: serde_json::json!("run checks"),
        }];
        for idx in 0..TOOL_CYCLES {
            let tool_use_id = format!("toolu_{idx}");
            messages.push(AnthropicMessage {
                role: "assistant".to_string(),
                content: serde_json::json!([
                    {"type": "text", "text": format!("running {idx}")},
                    {"type": "tool_use", "id": tool_use_id, "name": "Bash", "input": {"command": format!("check-{idx}")}}
                ]),
            });
            messages.push(AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!([{
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": format!("result-{idx}")
                }]),
            });
        }
        messages.push(AnthropicMessage {
            role: "assistant".to_string(),
            content: serde_json::json!("checks complete"),
        });
        messages.push(AnthropicMessage {
            role: "user".to_string(),
            content: serde_json::json!("summarize"),
        });

        let mut input_schema = std::collections::HashMap::new();
        input_schema.insert("type".to_string(), serde_json::json!("object"));
        input_schema.insert(
            "properties".to_string(),
            serde_json::json!({"command": {"type": "string"}}),
        );
        let anthropic = MessagesRequest {
            model: "claude-sonnet-4".to_string(),
            max_tokens: 1024,
            messages,
            system: None,
            stream: true,
            tools: Some(vec![AnthropicTool {
                name: "Bash".to_string(),
                description: "run a command".to_string(),
                input_schema,
                tool_type: None,
                max_uses: None,
                cache_control: None,
            }]),
            thinking: None,
            tool_choice: None,
            output_config: None,
            metadata: None,
        };
        let converted = crate::anthropic::converter::convert_request(&anthropic).expect("convert");
        let mapped_bash = converted
            .tool_name_map
            .iter()
            .find_map(|(mapped, original)| (original == "Bash").then(|| mapped.clone()))
            .expect("mapped Bash name");
        let mut request = KiroRequest {
            conversation_state: converted.conversation_state,
            profile_arn: None,
            additional_model_request_fields: converted.additional_model_request_fields,
            tool_cache_point_insert_after: converted.tool_cache_point_insert_after,
            cache_point_plan_recording_enabled: converted.cache_point_plan_recording_enabled,
        };

        let (body, report) = guard_kiro_request(
            &mut request,
            PayloadGuardConfig {
                enabled: true,
                max_bytes: 0,
                trim_history: true,
                shaping: PayloadShapingConfig::default(),
            },
        )
        .expect("guard");

        assert_eq!(report.flattened_history_tool_uses, 0);
        assert_eq!(report.textified_history_tool_results, 0);
        assert_eq!(report.trimmed_history_entries, 0);
        assert!(!body.contains("user Continue"));
        assert!(!body.contains("Tool results:"));
        assert!(!body.contains(&format!("{mapped_bash}:")));
        assert!(body.contains(&mapped_bash));
        let breakdown = breakdown_kiro_request(&request, &body);
        assert_eq!(breakdown.history_tool_use_count, TOOL_CYCLES);
        assert_eq!(breakdown.history_tool_result_count, TOOL_CYCLES);

        for message in &request.conversation_state.history {
            match message {
                Message::Assistant(assistant) => {
                    assert!(
                        !assistant
                            .assistant_response_message
                            .content
                            .contains(&mapped_bash)
                    );
                }
                Message::User(user) => {
                    assert!(!user.user_input_message.content.contains(&mapped_bash));
                }
            }
        }
    }

    #[test]
    fn converted_20_and_100_cycle_histories_trim_atomically_for_five_rounds() {
        fn build_request(tool_cycles: usize) -> (KiroRequest, String) {
            let mut messages = vec![AnthropicMessage {
                role: "user".to_string(),
                content: serde_json::json!("run structured check 0"),
            }];
            for idx in 0..tool_cycles {
                let tool_use_id = format!("toolu_cycle_{idx}");
                messages.push(AnthropicMessage {
                    role: "assistant".to_string(),
                    content: serde_json::json!([
                        {"type": "text", "text": format!("running structured check {idx}")},
                        {
                            "type": "tool_use",
                            "id": tool_use_id,
                            "name": "Bash",
                            "input": {"command": format!("check-{idx}")}
                        }
                    ]),
                });
                messages.push(AnthropicMessage {
                    role: "user".to_string(),
                    content: serde_json::json!([{
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": format!("result-{idx}-{}", "x".repeat(256))
                    }]),
                });
                if idx + 1 < tool_cycles {
                    messages.push(AnthropicMessage {
                        role: "assistant".to_string(),
                        content: serde_json::json!(format!("structured check {idx} complete")),
                    });
                    messages.push(AnthropicMessage {
                        role: "user".to_string(),
                        content: serde_json::json!(format!("run structured check {}", idx + 1)),
                    });
                }
            }

            let mut input_schema = std::collections::HashMap::new();
            input_schema.insert("type".to_string(), serde_json::json!("object"));
            input_schema.insert(
                "properties".to_string(),
                serde_json::json!({"command": {"type": "string"}}),
            );
            let anthropic = MessagesRequest {
                model: "claude-sonnet-4".to_string(),
                max_tokens: 1024,
                messages,
                system: None,
                stream: true,
                tools: Some(vec![AnthropicTool {
                    name: "Bash".to_string(),
                    description: "run a command".to_string(),
                    input_schema,
                    tool_type: None,
                    max_uses: None,
                    cache_control: None,
                }]),
                thinking: None,
                tool_choice: None,
                output_config: None,
                metadata: None,
            };
            let converted =
                crate::anthropic::converter::convert_request(&anthropic).expect("convert cycles");
            let mapped_bash = converted
                .tool_name_map
                .iter()
                .find_map(|(mapped, original)| (original == "Bash").then(|| mapped.clone()))
                .expect("mapped Bash name");
            (
                KiroRequest {
                    conversation_state: converted.conversation_state,
                    profile_arn: None,
                    additional_model_request_fields: converted.additional_model_request_fields,
                    tool_cache_point_insert_after: converted.tool_cache_point_insert_after,
                    cache_point_plan_recording_enabled: converted
                        .cache_point_plan_recording_enabled,
                },
                mapped_bash,
            )
        }

        fn tool_use_ids(message: &Message) -> Vec<&str> {
            let Message::Assistant(assistant) = message else {
                return Vec::new();
            };
            assistant
                .assistant_response_message
                .tool_uses
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|tool_use| tool_use.tool_use_id.as_str())
                .collect()
        }

        fn tool_result_ids(message: &Message) -> Vec<&str> {
            let Message::User(user) = message else {
                return Vec::new();
            };
            user.user_input_message
                .user_input_message_context
                .tool_results
                .iter()
                .map(|result| result.tool_use_id.as_str())
                .collect()
        }

        for tool_cycles in [20usize, 100] {
            for round in 1..=5 {
                let (mut request, mapped_bash) = build_request(tool_cycles);
                let original_history_len = request.conversation_state.history.len();
                let mut baseline_request = request.clone();
                let (untrimmed_body, baseline_report) = guard_kiro_request(
                    &mut baseline_request,
                    PayloadGuardConfig {
                        enabled: true,
                        max_bytes: 0,
                        trim_history: true,
                        shaping: PayloadShapingConfig::default(),
                    },
                )
                .expect("serialize untrimmed baseline");
                assert_eq!(baseline_report.trimmed_history_entries, 0);
                let max_bytes = untrimmed_body.len() * 2 / 3;

                let (body, report) = guard_kiro_request(
                    &mut request,
                    PayloadGuardConfig {
                        enabled: true,
                        max_bytes,
                        trim_history: true,
                        shaping: PayloadShapingConfig::default(),
                    },
                )
                .expect("trim structured cycles");

                assert!(
                    report.trimmed_history_entries > 0,
                    "cycles={tool_cycles} round={round} baseline_bytes={} max_bytes={} original_bytes={} final_bytes={} original_history={} final_history={} removed_orphan_uses={} removed_orphan_results={}",
                    untrimmed_body.len(),
                    max_bytes,
                    report.original_bytes,
                    report.final_bytes,
                    report.original_history_entries,
                    report.final_history_entries,
                    report.removed_orphan_tool_uses,
                    report.removed_orphan_tool_results,
                );
                assert!(
                    !report.still_oversized,
                    "cycles={tool_cycles} round={round}"
                );
                assert!(
                    body.len() <= max_bytes,
                    "cycles={tool_cycles} round={round}"
                );
                assert!(
                    request.conversation_state.history.len() < original_history_len,
                    "cycles={tool_cycles} round={round}"
                );
                assert_eq!(report.flattened_history_tool_uses, 0);
                assert_eq!(report.textified_history_tool_results, 0);
                assert!(!body.contains("user Continue"));
                assert!(!body.contains("Tool results:"));
                assert!(!body.contains(&format!("{mapped_bash}:")));

                let history = &request.conversation_state.history;
                let current_results = &request
                    .conversation_state
                    .current_message
                    .user_input_message
                    .user_input_message_context
                    .tool_results;
                assert_eq!(
                    current_results.len(),
                    1,
                    "cycles={tool_cycles} round={round}"
                );
                let active_id = format!("toolu_cycle_{}", tool_cycles - 1);
                assert_eq!(current_results[0].tool_use_id, active_id);

                let mut use_count = 0usize;
                let mut history_result_count = 0usize;
                for (index, message) in history.iter().enumerate() {
                    let uses = tool_use_ids(message);
                    if !uses.is_empty() {
                        use_count += uses.len();
                        let paired_results = if let Some(next) = history.get(index + 1) {
                            tool_result_ids(next)
                        } else {
                            current_results
                                .iter()
                                .map(|result| result.tool_use_id.as_str())
                                .collect()
                        };
                        for tool_use_id in uses {
                            assert!(
                                paired_results.contains(&tool_use_id),
                                "orphan tool_use={tool_use_id} cycles={tool_cycles} round={round}"
                            );
                        }
                    }

                    let results = tool_result_ids(message);
                    if !results.is_empty() {
                        history_result_count += results.len();
                        assert!(
                            index > 0,
                            "leading tool_result cycles={tool_cycles} round={round}"
                        );
                        let paired_uses = tool_use_ids(&history[index - 1]);
                        for tool_result_id in results {
                            assert!(
                                paired_uses.contains(&tool_result_id),
                                "orphan tool_result={tool_result_id} cycles={tool_cycles} round={round}"
                            );
                        }
                    }
                }

                assert!(use_count > 0, "cycles={tool_cycles} round={round}");
                assert_eq!(
                    use_count,
                    history_result_count + current_results.len(),
                    "cycles={tool_cycles} round={round}"
                );
                assert!(
                    tool_use_ids(history.last().expect("active assistant"))
                        .contains(&active_id.as_str()),
                    "active tool use was trimmed cycles={tool_cycles} round={round}"
                );

                for message in history {
                    match message {
                        Message::Assistant(assistant) => assert!(
                            !assistant
                                .assistant_response_message
                                .content
                                .contains(&mapped_bash)
                        ),
                        Message::User(user) => {
                            assert!(!user.user_input_message.content.contains(&mapped_bash))
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn tool_use_format_diagnostics_counts_structure_without_content() {
        let first_assistant = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage::new("first").with_tool_uses(vec![
                ToolUseEntry::new("tool-old", "missingTool").with_input(serde_json::json!("raw")),
            ]),
        };

        let mut first_user = HistoryUserMessage::new("first result", TEST_MODEL);
        first_user.user_input_message.user_input_message_context = UserInputMessageContext::new()
            .with_tool_results(vec![ToolResult::success("tool-old", "old result")]);

        let last_assistant = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage::new("last").with_tool_uses(vec![
                ToolUseEntry::new("tool-last", "read").with_input(serde_json::json!({})),
                ToolUseEntry::new(" ", "read").with_input(serde_json::json!({})),
            ]),
        };

        let mut current = UserInputMessage::new("current", TEST_MODEL);
        current.user_input_message_context = UserInputMessageContext::new()
            .with_tools(vec![
                Tool {
                    tool_specification: ToolSpecification {
                        name: "read".to_string(),
                        description: "read".to_string(),
                        input_schema: InputSchema::from_json(serde_json::json!({
                            "type": "object",
                            "properties": {}
                        })),
                    },
                },
                Tool {
                    tool_specification: ToolSpecification {
                        name: "READ".to_string(),
                        description: "duplicate".to_string(),
                        input_schema: InputSchema::from_json(serde_json::json!({
                            "type": "object",
                            "properties": {}
                        })),
                    },
                },
            ])
            .with_tool_results(vec![
                ToolResult::success("tool-last", "kept"),
                ToolResult::success("tool-old", "not adjacent"),
                ToolResult::success("tool-last", "duplicate"),
            ]);

        let request = KiroRequest {
            conversation_state: ConversationState::new("conv-test")
                .with_history(vec![
                    Message::Assistant(first_assistant),
                    Message::User(first_user),
                    Message::Assistant(last_assistant),
                ])
                .with_current_message(CurrentMessage::new(current)),
            profile_arn: None,
            additional_model_request_fields: None,
            tool_cache_point_insert_after: Vec::new(),
            cache_point_plan_recording_enabled: true,
        };

        let diagnostics = diagnose_kiro_tool_use_format(&request);

        assert!(diagnostics.has_tool_payload());
        assert_eq!(diagnostics.tool_items_scanned, 9);
        assert!(!diagnostics.tool_item_scan_truncated);
        assert_eq!(diagnostics.current_tool_count, 2);
        assert_eq!(diagnostics.duplicate_tool_names, 1);
        assert_eq!(diagnostics.history_tool_use_count, 3);
        assert_eq!(diagnostics.history_tool_result_count, 1);
        assert_eq!(diagnostics.last_assistant_tool_use_count, 1);
        assert_eq!(diagnostics.current_results_matching_last_assistant, 2);
        assert_eq!(diagnostics.current_results_not_matching_last_assistant, 1);
        assert_eq!(diagnostics.duplicate_current_tool_result_ids, 1);
        assert_eq!(diagnostics.empty_tool_use_ids, 1);
        assert_eq!(diagnostics.non_object_tool_use_inputs, 1);
        assert_eq!(diagnostics.history_tool_names_missing_from_tools, 1);
        assert_eq!(diagnostics.empty_tool_descriptions, 0);
        assert_eq!(diagnostics.invalid_tool_schema_property_keys, 0);
    }

    #[test]
    fn tool_use_format_diagnostics_counts_empty_descriptions_and_invalid_schema_keys() {
        let mut current = UserInputMessage::new("current", TEST_MODEL);
        current.user_input_message_context =
            UserInputMessageContext::new().with_tools(vec![Tool {
                tool_specification: ToolSpecification {
                    name: "probe".to_string(),
                    description: "".to_string(),
                    input_schema: InputSchema::from_json(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "bad key": {"type": "string"},
                            "nested": {
                                "type": "object",
                                "properties": {
                                    "path/to": {"type": "string"}
                                }
                            }
                        }
                    })),
                },
            }]);

        let request = KiroRequest {
            conversation_state: ConversationState::new("conv-test")
                .with_current_message(CurrentMessage::new(current)),
            profile_arn: None,
            additional_model_request_fields: None,
            tool_cache_point_insert_after: Vec::new(),
            cache_point_plan_recording_enabled: true,
        };

        let diagnostics = diagnose_kiro_tool_use_format(&request);

        assert_eq!(diagnostics.empty_tool_descriptions, 1);
        assert_eq!(diagnostics.invalid_tool_schema_property_keys, 2);
    }

    #[test]
    fn tool_schema_key_diagnostics_ignore_schema_map_keys() {
        let mut current = UserInputMessage::new("current", TEST_MODEL);
        current.user_input_message_context =
            UserInputMessageContext::new().with_tools(vec![Tool {
                tool_specification: ToolSpecification {
                    name: "probe".to_string(),
                    description: "probe".to_string(),
                    input_schema: InputSchema::from_json(serde_json::json!({
                        "type": "object",
                        "$defs": {
                            "bad def/key": {
                                "type": "object",
                                "properties": {
                                    "nested bad": {"type": "string"}
                                }
                            }
                        },
                        "patternProperties": {
                            "^bad pattern key .+$": {
                                "type": "object",
                                "properties": {
                                    "another bad": {"type": "string"}
                                }
                            }
                        },
                        "dependentSchemas": {
                            "not-a-property/key": {
                                "type": "object",
                                "properties": {
                                    "third bad": {"type": "string"}
                                }
                            }
                        }
                    })),
                },
            }]);

        let request = KiroRequest {
            conversation_state: ConversationState::new("conv-test")
                .with_current_message(CurrentMessage::new(current)),
            profile_arn: None,
            additional_model_request_fields: None,
            tool_cache_point_insert_after: Vec::new(),
            cache_point_plan_recording_enabled: true,
        };

        let diagnostics = diagnose_kiro_tool_use_format(&request);

        assert_eq!(
            diagnostics.invalid_tool_schema_property_keys, 3,
            "$defs, patternProperties, and dependentSchemas map keys are schema identifiers, not object property keys"
        );
    }

    #[test]
    fn warning_header_fragment_reports_changes() {
        let report = PayloadGuardReport {
            enabled: true,
            max_bytes: 1000,
            original_bytes: 2000,
            final_bytes: 900,
            original_history_entries: 4,
            final_history_entries: 2,
            trimmed_history_entries: 2,
            guard_serializations: 3,
            history_trim_passes: 1,
            aligned_leading_entries: 1,
            removed_empty_tool_uses: 1,
            removed_duplicate_tool_uses: 1,
            renamed_duplicate_tool_uses: 1,
            removed_orphan_tool_results: 1,
            removed_duplicate_tool_results: 1,
            textified_duplicate_tool_results: 1,
            textified_orphan_tool_results: 1,
            removed_orphan_tool_uses: 1,
            flattened_history_tool_uses: 1,
            textified_history_tool_results: 1,
            removed_history_tools: 1,
            truncated_history_tool_results: 1,
            truncated_history_tool_result_chars: 10,
            removed_history_thinking_blocks: 1,
            removed_history_thinking_chars: 10,
            trimmed_web_fetch_blocks: 1,
            trimmed_web_fetch_chars: 10,
            compressed_tool_definitions: 1,
            compressed_tool_definition_bytes: 10,
            truncated_current_tool_results: 1,
            truncated_current_tool_result_chars: 10,
            truncated_current_documents: 1,
            truncated_current_document_chars: 10,
            truncated_current_user_content: 1,
            truncated_current_user_content_chars: 10,
            dropped_current_images: 1,
            dropped_current_image_bytes: 10,
            dropped_historical_images: 1,
            dropped_historical_image_bytes: 10,
            kiro_cache_points_planned: 0,
            kiro_cache_points_inserted: 0,
            cache_point_retry_without_cache_point: false,
            cache_point_retry_reason: None,
            body_sha256: None,
            tool_use_format_diagnostics: None,
            tool_format_debug_ref: None,
            still_oversized: false,
        };

        let header = report.warning_header_fragment().expect("header");
        assert!(header.contains("payload-trimmed-history=2"));
        assert!(header.contains("payload-empty-tool-uses=1"));
        assert!(header.contains("payload-duplicate-tool-uses=1"));
        assert!(header.contains("payload-renamed-duplicate-tool-uses=1"));
        assert!(header.contains("payload-duplicate-tool-results=1"));
        assert!(header.contains("payload-textified-duplicate-tool-results=1"));
        assert!(header.contains("payload-textified-tool-results=1"));
        assert!(header.contains("payload-history-tool-results-truncated=1"));
        assert!(header.contains("payload-tools-compressed=1"));
        assert!(header.contains("payload-current-tool-results-truncated=1"));
        assert!(header.contains("payload-current-documents-truncated=1"));
        assert!(header.contains("payload-current-content-truncated=1"));
        assert!(header.contains("payload-current-images-dropped=1"));
    }

    #[test]
    fn warning_header_fragment_does_not_expose_internal_history_tool_flattening() {
        let mut report = new_payload_guard_report(usize::MAX, 100, 4);
        report.flattened_history_tool_uses = 2;
        report.textified_history_tool_results = 2;
        report.removed_history_tools = 1;

        assert!(report.was_modified());
        assert!(report.warning_header_fragment().is_none());
    }

    #[test]
    fn json_len_matches_serde_json_string_length() {
        let value = serde_json::json!({
            "alpha": [1, 2, 3],
            "beta": {
                "nested": true,
                "text": "payload length check"
            }
        });

        assert_eq!(
            json_len(&value),
            serde_json::to_string(&value).unwrap().len()
        );
    }

    #[test]
    fn cache_point_plan_inserts_markers_in_serialized_kiro_body() {
        let mut current = UserInputMessage::new("current", TEST_MODEL);
        current.user_input_message_context = UserInputMessageContext::new().with_tools(vec![
            Tool {
                tool_specification: ToolSpecification {
                    name: "read".to_string(),
                    description: "read".to_string(),
                    input_schema: InputSchema::default(),
                },
            },
            Tool {
                tool_specification: ToolSpecification {
                    name: "write".to_string(),
                    description: "write".to_string(),
                    input_schema: InputSchema::default(),
                },
            },
        ]);
        let mut request = KiroRequest {
            conversation_state: ConversationState::new("conv-test")
                .with_current_message(CurrentMessage::new(current)),
            profile_arn: None,
            additional_model_request_fields: None,
            tool_cache_point_insert_after: vec![0, 1, 1, 99],
            cache_point_plan_recording_enabled: true,
        };

        let (body, report) =
            guard_kiro_request(&mut request, guard_config(usize::MAX)).expect("guard");
        let value: Value = serde_json::from_str(&body).expect("body json");
        let tools = value
            .pointer(
                "/conversationState/currentMessage/userInputMessage/userInputMessageContext/tools",
            )
            .and_then(Value::as_array)
            .expect("tools array");

        assert_eq!(tools.len(), 4);
        assert!(tools[0].get("toolSpecification").is_some());
        assert_eq!(tools[1]["cachePoint"]["type"], "default");
        assert!(tools[2].get("toolSpecification").is_some());
        assert_eq!(tools[3]["cachePoint"]["type"], "default");
        assert_eq!(report.kiro_cache_points_planned, 4);
        assert_eq!(report.kiro_cache_points_inserted, 2);
    }

    #[test]
    fn cache_point_plan_is_not_serialized_by_plain_serde_json() {
        let mut request = request_with_history(Vec::new());
        request.tool_cache_point_insert_after = vec![0];
        let body = serde_json::to_string(&request).expect("plain serialize");

        assert!(!body.contains("toolCachePointInsertAfter"));
        assert!(!body.contains("cachePoint"));
    }

    #[test]
    fn guard_strips_empty_tool_uses() {
        let assistant = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage {
                content: "empty tools".to_string(),
                tool_uses: Some(Vec::new()),
                reasoning_content: None,
            },
        };
        let mut request = request_with_history(vec![
            Message::User(HistoryUserMessage::new("user", TEST_MODEL)),
            Message::Assistant(assistant),
        ]);

        let (_body, report) =
            guard_kiro_request(&mut request, guard_config(usize::MAX)).expect("guard");

        assert_eq!(report.removed_empty_tool_uses, 1);
        let Message::Assistant(assistant) = &request.conversation_state.history[1] else {
            panic!("expected assistant");
        };
        assert!(assistant.assistant_response_message.tool_uses.is_none());
    }

    #[test]
    fn stripping_empty_tool_uses_preserves_native_reasoning_for_five_rounds() {
        for round in 0..5 {
            let expected = ReasoningContent::reasoning_text(
                format!("thought {round}"),
                format!("signature-{round}"),
            );
            let assistant = HistoryAssistantMessage {
                assistant_response_message: AssistantMessage {
                    content: format!("answer {round}"),
                    tool_uses: Some(Vec::new()),
                    reasoning_content: Some(expected.clone()),
                },
            };
            let mut request = request_with_history(vec![
                Message::User(HistoryUserMessage::new("user", TEST_MODEL)),
                Message::Assistant(assistant),
            ]);

            let (_body, report) = guard_kiro_request(
                &mut request,
                guard_config_with_shaping(
                    usize::MAX,
                    true,
                    PayloadShapingConfig {
                        discard_historical_thinking: false,
                        ..PayloadShapingConfig::default()
                    },
                ),
            )
            .expect("guard");

            assert_eq!(report.removed_empty_tool_uses, 1, "round {round}");
            let Message::Assistant(assistant) = &request.conversation_state.history[1] else {
                panic!("expected assistant");
            };
            assert!(assistant.assistant_response_message.tool_uses.is_none());
            assert_eq!(
                assistant.assistant_response_message.reasoning_content,
                Some(expected),
                "round {round}"
            );
        }
    }

    #[test]
    fn guard_aligns_leading_assistant_and_repairs_result() {
        let assistant = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage::new("tool call")
                .with_tool_uses(vec![ToolUseEntry::new("tool-1", "readFile")]),
        };
        let mut user = HistoryUserMessage::new("result message", TEST_MODEL);
        user.user_input_message.user_input_message_context = UserInputMessageContext::new()
            .with_tool_results(vec![ToolResult::success("tool-1", "valid result")]);
        let mut request =
            request_with_history(vec![Message::Assistant(assistant), Message::User(user)]);

        let (_body, report) =
            guard_kiro_request(&mut request, guard_config(usize::MAX)).expect("guard");

        assert_eq!(report.aligned_leading_entries, 1);
        assert_eq!(report.removed_orphan_tool_results, 1);
        assert!(matches!(
            request.conversation_state.history.first(),
            Some(Message::User(_))
        ));
    }

    #[test]
    fn guard_keeps_tool_use_paired_with_current_message_result() {
        let assistant = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage::new("tool call")
                .with_tool_uses(vec![ToolUseEntry::new("tool-1", "readFile")]),
        };
        let mut request = request_with_history(vec![
            Message::User(HistoryUserMessage::new("read", TEST_MODEL)),
            Message::Assistant(assistant),
        ]);
        request
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context = UserInputMessageContext::new()
            .with_tool_results(vec![ToolResult::success("tool-1", "valid result")]);

        let (_body, report) =
            guard_kiro_request(&mut request, guard_config(usize::MAX)).expect("guard");

        assert_eq!(report.removed_orphan_tool_uses, 0);
        assert_eq!(report.removed_orphan_tool_results, 0);
        let Message::Assistant(assistant) = &request.conversation_state.history[1] else {
            panic!("expected assistant");
        };
        assert_eq!(
            assistant
                .assistant_response_message
                .tool_uses
                .as_ref()
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(
            request
                .conversation_state
                .current_message
                .user_input_message
                .user_input_message_context
                .tool_results
                .len(),
            1
        );
    }

    #[test]
    fn payload_shaping_truncates_history_tool_results_but_preserves_current_results() {
        let old_assistant = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage::new("old tool call")
                .with_tool_uses(vec![ToolUseEntry::new("old-tool", "readFile")]),
        };
        let mut old_result = HistoryUserMessage::new("old result", TEST_MODEL);
        old_result.user_input_message.user_input_message_context = UserInputMessageContext::new()
            .with_tool_results(vec![ToolResult::success("old-tool", "old\n".repeat(5_000))]);
        let current_assistant = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage::new("current tool call")
                .with_tool_uses(vec![ToolUseEntry::new("current-tool", "readFile")]),
        };
        let current_result = "current\n".repeat(5_000);
        let mut request = request_with_history(vec![
            Message::User(HistoryUserMessage::new("read old", TEST_MODEL)),
            Message::Assistant(old_assistant),
            Message::User(old_result),
            Message::Assistant(current_assistant),
        ]);
        request
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context =
            UserInputMessageContext::new().with_tool_results(vec![ToolResult::success(
                "current-tool",
                current_result.clone(),
            )]);

        let (_body, report) = guard_kiro_request(
            &mut request,
            shaping_config(PayloadShapingConfig {
                historical_tool_result_max_chars: 1_000,
                historical_tool_result_head_lines: 8,
                historical_tool_result_tail_lines: 4,
                discard_historical_thinking: false,
                compress_tool_definitions: false,
                web_fetch_trim_enabled: false,
                ..PayloadShapingConfig::default()
            }),
        )
        .expect("guard");

        assert_eq!(report.truncated_history_tool_results, 1);
        assert_eq!(report.flattened_history_tool_uses, 0);
        assert_eq!(report.textified_history_tool_results, 0);
        let Message::User(user) = &request.conversation_state.history[2] else {
            panic!("expected historical user");
        };
        assert_eq!(
            user.user_input_message
                .user_input_message_context
                .tool_results
                .len(),
            1
        );
        let historical_text = tool_result_text(
            &user
                .user_input_message
                .user_input_message_context
                .tool_results[0],
        );
        assert!(historical_text.chars().count() <= 1_120);
        assert!(historical_text.contains("historical tool result truncated by proxy"));
        assert_eq!(user.user_input_message.content, "old result");

        let current_text = tool_result_text(
            &request
                .conversation_state
                .current_message
                .user_input_message
                .user_input_message_context
                .tool_results[0],
        );
        assert_eq!(current_text, current_result);
    }

    #[test]
    fn payload_shaping_truncates_current_tool_results_when_enabled() {
        for round in 0..5 {
            let assistant = HistoryAssistantMessage {
                assistant_response_message: AssistantMessage::new("current tool call")
                    .with_tool_uses(vec![ToolUseEntry::new("current-tool", "readFile")]),
            };
            let mut request = request_with_history(vec![
                Message::User(HistoryUserMessage::new("read", TEST_MODEL)),
                Message::Assistant(assistant),
            ]);
            request
                .conversation_state
                .current_message
                .user_input_message
                .user_input_message_context =
                UserInputMessageContext::new().with_tool_results(vec![ToolResult::success(
                    "current-tool",
                    "current result\n".repeat(5_000),
                )]);

            let (body, report) = guard_kiro_request(
                &mut request,
                guard_config_with_shaping(
                    6_000,
                    false,
                    PayloadShapingConfig {
                        truncate_historical_tool_results: false,
                        discard_historical_thinking: false,
                        compress_tool_definitions: false,
                        web_fetch_trim_enabled: false,
                        truncate_current_tool_results: true,
                        current_tool_result_max_chars: 1_000,
                        ..PayloadShapingConfig::default()
                    },
                ),
            )
            .expect("guard");

            assert!(body.len() <= 6_000, "round {round}");
            assert!(!report.still_oversized, "round {round}");
            assert!(report.truncated_current_tool_results > 0, "round {round}");
            let current_text = tool_result_text(
                &request
                    .conversation_state
                    .current_message
                    .user_input_message
                    .user_input_message_context
                    .tool_results[0],
            );
            assert!(current_text.chars().count() <= 1_000);
            assert!(current_text.contains("current tool result truncated by proxy"));
        }
    }

    #[test]
    fn payload_shaping_truncates_current_user_content_when_enabled() {
        let mut request = request_with_history(Vec::new());
        request
            .conversation_state
            .current_message
            .user_input_message
            .content = "current message ".repeat(5_000);

        let (body, report) = guard_kiro_request(
            &mut request,
            guard_config_with_shaping(
                5_000,
                false,
                PayloadShapingConfig {
                    truncate_historical_tool_results: false,
                    discard_historical_thinking: false,
                    compress_tool_definitions: false,
                    web_fetch_trim_enabled: false,
                    truncate_current_user_content: true,
                    current_user_content_max_chars: 1_200,
                    ..PayloadShapingConfig::default()
                },
            ),
        )
        .expect("guard");

        assert!(body.len() <= 5_000);
        assert!(!report.still_oversized);
        assert!(report.truncated_current_user_content > 0);
        assert!(
            request
                .conversation_state
                .current_message
                .user_input_message
                .content
                .contains("current user content truncated by proxy")
        );
    }

    #[test]
    fn payload_shaping_truncates_current_documents_without_breaking_tags() {
        for round in 0..5 {
            let mut request = request_with_history(Vec::new());
            request
                .conversation_state
                .current_message
                .user_input_message
                .content = format!(
                "before\n<document media_type=\"application/pdf\">\n{}\n</document>\nafter",
                "pdf body ".repeat(5_000)
            );

            let (body, report) = guard_kiro_request(
                &mut request,
                guard_config_with_shaping(
                    6_000,
                    false,
                    PayloadShapingConfig {
                        truncate_historical_tool_results: false,
                        discard_historical_thinking: false,
                        compress_tool_definitions: false,
                        web_fetch_trim_enabled: false,
                        truncate_current_documents: true,
                        truncate_current_user_content: true,
                        current_document_max_chars: 1_200,
                        current_user_content_max_chars: 1_200,
                        ..PayloadShapingConfig::default()
                    },
                ),
            )
            .expect("guard");

            let content = &request
                .conversation_state
                .current_message
                .user_input_message
                .content;
            assert!(body.len() <= 6_000, "round {round}");
            assert!(!report.still_oversized, "round {round}");
            assert!(report.truncated_current_documents > 0, "round {round}");
            assert_eq!(report.truncated_current_user_content, 0);
            assert!(content.contains("<document media_type=\"application/pdf\">"));
            assert!(content.contains("</document>"));
            assert!(content.contains("current document truncated by proxy"));
        }
    }

    #[test]
    fn payload_shaping_drops_current_images_only_when_enabled() {
        let mut request = request_with_history(Vec::new());
        request
            .conversation_state
            .current_message
            .user_input_message
            .images = vec![
            KiroImage::from_base64("png", "a".repeat(12_000)),
            KiroImage::from_base64("jpeg", "b".repeat(12_000)),
        ];

        let (body, report) = guard_kiro_request(
            &mut request,
            guard_config_with_shaping(
                5_000,
                false,
                PayloadShapingConfig {
                    truncate_historical_tool_results: false,
                    discard_historical_thinking: false,
                    compress_tool_definitions: false,
                    web_fetch_trim_enabled: false,
                    truncate_current_images: true,
                    current_images_max_bytes: 1,
                    ..PayloadShapingConfig::default()
                },
            ),
        )
        .expect("guard");

        assert!(body.len() <= 5_000);
        assert!(!report.still_oversized);
        assert_eq!(report.dropped_current_images, 2);
        assert!(
            request
                .conversation_state
                .current_message
                .user_input_message
                .images
                .is_empty()
        );
        assert!(
            request
                .conversation_state
                .current_message
                .user_input_message
                .content
                .contains(
                    "Current images were omitted because they exceeded the request image budget"
                )
        );
        assert_eq!(
            request
                .conversation_state
                .current_message
                .user_input_message
                .content
                .matches(
                    "Current images were omitted because they exceeded the request image budget"
                )
                .count(),
            1,
            "image budget shaping must emit exactly one summary placeholder"
        );
    }

    #[test]
    fn current_fit_batches_image_drops_and_counts_serializations_for_five_rounds() {
        let encoded = "A".repeat(512 * 1_024);

        for round in 0..5 {
            let mut kiro_template = request_with_history(Vec::new());
            kiro_template
                .conversation_state
                .current_message
                .user_input_message
                .images = (0..4)
                .map(|_| KiroImage::from_base64("png", encoded.clone()))
                .collect();
            let mut one_kiro_image = request_with_history(Vec::new());
            one_kiro_image
                .conversation_state
                .current_message
                .user_input_message
                .images = vec![KiroImage::from_base64("png", encoded.clone())];
            let kiro_target = serialize_kiro_request(&one_kiro_image)
                .unwrap()
                .len()
                .saturating_add(4 * 1_024);
            let shaping = PayloadShapingConfig {
                truncate_historical_tool_results: false,
                discard_historical_thinking: false,
                compress_tool_definitions: false,
                web_fetch_trim_enabled: false,
                fit_current_payload_to_budget: true,
                current_tool_result_max_chars: 0,
                current_document_max_chars: 0,
                current_user_content_max_chars: 0,
                current_images_max_bytes: usize::MAX,
                ..PayloadShapingConfig::default()
            };

            let (kiro_body, kiro_report) = guard_kiro_request(
                &mut kiro_template,
                guard_config_with_shaping(kiro_target, false, shaping),
            )
            .expect("batched Kiro current image fit");
            assert!(kiro_body.len() <= kiro_target, "round {round}");
            assert_eq!(kiro_report.dropped_current_images, 3, "round {round}");
            assert_eq!(kiro_report.guard_serializations, 3, "round {round}");
            assert_eq!(
                kiro_template
                    .conversation_state
                    .current_message
                    .user_input_message
                    .images
                    .len(),
                1,
                "round {round}"
            );
            assert_eq!(
                kiro_template
                    .conversation_state
                    .current_message
                    .user_input_message
                    .content
                    .matches(
                        "Current images were omitted because they exceeded the request image budget"
                    )
                    .count(),
                1,
                "round {round}"
            );

            let image_block = |data: &str| {
                serde_json::json!({
                    "type": "image",
                    "source": {"type": "base64", "media_type": "image/png", "data": data}
                })
            };
            let mut anthropic_template = anthropic_request(vec![anthropic_message(
                "user",
                Value::Array((0..4).map(|_| image_block(&encoded)).collect()),
            )]);
            let one_anthropic_image = anthropic_request(vec![anthropic_message(
                "user",
                Value::Array(vec![image_block(&encoded)]),
            )]);
            let anthropic_target = serde_json::to_vec(&one_anthropic_image)
                .unwrap()
                .len()
                .saturating_add(4 * 1_024);
            let anthropic_raw = Bytes::from(serde_json::to_vec(&anthropic_template).unwrap());
            let (anthropic_body, anthropic_report) = guard_anthropic_messages_request_reusing_body(
                &mut anthropic_template,
                guard_config_with_shaping(anthropic_target, false, shaping),
                &anthropic_raw,
            )
            .expect("batched Anthropic current image fit");
            assert!(anthropic_body.len() <= anthropic_target, "round {round}");
            assert_eq!(anthropic_report.dropped_current_images, 3, "round {round}");
            assert_eq!(anthropic_report.guard_serializations, 3, "round {round}");
            assert_eq!(
                count_content_blocks_by_type(&anthropic_template.messages[0].content, "image"),
                1,
                "round {round}"
            );
            assert_eq!(
                String::from_utf8_lossy(&anthropic_body)
                    .matches(
                        "Current images were omitted because they exceeded the request image budget"
                    )
                    .count(),
                1,
                "round {round}"
            );
        }
    }

    #[test]
    fn payload_shaping_fit_current_payload_to_budget_enables_current_trimming() {
        let mut request = request_with_history(vec![
            Message::User(HistoryUserMessage::new("old user", TEST_MODEL)),
            Message::Assistant(HistoryAssistantMessage {
                assistant_response_message: AssistantMessage::new("current tool call")
                    .with_tool_uses(vec![ToolUseEntry::new("current-tool", "readFile")]),
            }),
        ]);
        request
            .conversation_state
            .current_message
            .user_input_message
            .content = format!(
            "before\n<document media_type=\"application/pdf\">\n{}\n</document>\n{}",
            "pdf body ".repeat(5_000),
            "current message ".repeat(5_000)
        );
        request
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context =
            UserInputMessageContext::new().with_tool_results(vec![ToolResult::success(
                "current-tool",
                "current result\n".repeat(5_000),
            )]);

        let (body, report) = guard_kiro_request(
            &mut request,
            guard_config_with_shaping(
                9_000,
                false,
                PayloadShapingConfig {
                    truncate_historical_tool_results: false,
                    discard_historical_thinking: false,
                    compress_tool_definitions: false,
                    web_fetch_trim_enabled: false,
                    fit_current_payload_to_budget: true,
                    current_tool_result_max_chars: 2_000,
                    current_document_max_chars: 2_000,
                    current_user_content_max_chars: 2_000,
                    ..PayloadShapingConfig::default()
                },
            ),
        )
        .expect("guard");

        assert!(body.len() <= 9_000, "final body was {} bytes", body.len());
        assert!(!report.still_oversized);
        assert_eq!(
            report.guard_serializations, 2,
            "the initial measurement and current-fit serialization must both be counted"
        );
        assert!(report.truncated_current_tool_results > 0);
        assert!(report.truncated_current_documents > 0);
        let content = &request
            .conversation_state
            .current_message
            .user_input_message
            .content;
        assert!(content.contains("<document media_type=\"application/pdf\">"));
        assert!(content.contains("</document>"));
        assert!(content.contains("current document truncated by proxy"));
        assert!(
            tool_result_text(
                &request
                    .conversation_state
                    .current_message
                    .user_input_message
                    .user_input_message_context
                    .tool_results[0]
            )
            .contains("current tool result truncated by proxy")
        );
    }

    #[test]
    fn payload_shaping_does_not_trim_current_payload_when_fit_disabled() {
        let mut request = request_with_history(Vec::new());
        let original = "current message ".repeat(5_000);
        request
            .conversation_state
            .current_message
            .user_input_message
            .content = original.clone();

        let (_body, report) = guard_kiro_request(
            &mut request,
            guard_config_with_shaping(
                5_000,
                false,
                PayloadShapingConfig {
                    truncate_historical_tool_results: false,
                    discard_historical_thinking: false,
                    compress_tool_definitions: false,
                    web_fetch_trim_enabled: false,
                    ..PayloadShapingConfig::default()
                },
            ),
        )
        .expect("guard");

        assert!(report.still_oversized);
        assert_eq!(report.truncated_current_user_content, 0);
        assert_eq!(
            request
                .conversation_state
                .current_message
                .user_input_message
                .content,
            original
        );
    }

    #[test]
    fn payload_shaping_discards_only_historical_thinking_blocks() {
        let assistant = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage::new(
                "visible\n<thinking>hidden chain</thinking>\nanswer",
            ),
        };
        let mut request = request_with_history(vec![
            Message::User(HistoryUserMessage::new("question", TEST_MODEL)),
            Message::Assistant(assistant),
        ]);

        let (_body, report) = guard_kiro_request(
            &mut request,
            shaping_config(PayloadShapingConfig {
                truncate_historical_tool_results: false,
                compress_tool_definitions: false,
                web_fetch_trim_enabled: false,
                ..PayloadShapingConfig::default()
            }),
        )
        .expect("guard");

        assert_eq!(report.removed_history_thinking_blocks, 1);
        let Message::Assistant(assistant) = &request.conversation_state.history[1] else {
            panic!("expected assistant");
        };
        assert_eq!(
            assistant.assistant_response_message.content,
            "visible\n\nanswer"
        );
    }

    #[test]
    fn payload_shaping_discards_native_reasoning_as_one_atomic_block_for_five_rounds() {
        for round in 0..5 {
            for native in [
                ReasoningContent::reasoning_text(
                    format!("native thought {round}"),
                    format!("signature-{round}"),
                ),
                ReasoningContent::redacted_content(base64_zeros_for_decoded_bytes(round + 1)),
            ] {
                let native_is_signed = matches!(native, ReasoningContent::ReasoningText(_));
                let native_bytes = json_len(&native);
                let assistant = HistoryAssistantMessage {
                    assistant_response_message: AssistantMessage::new(format!(
                        "active-visible\n<thinking>active-legacy {round}</thinking>\nactive-answer"
                    ))
                    .with_tool_uses(vec![ToolUseEntry::new("tool-1", "read")])
                    .with_reasoning_content(native),
                };
                let mut request = request_with_history(vec![
                    Message::User(HistoryUserMessage::new("question", TEST_MODEL)),
                    Message::Assistant(HistoryAssistantMessage {
                        assistant_response_message: AssistantMessage::new(format!(
                            "stale-visible\n<thinking>stale-legacy {round}</thinking>\nstale-answer"
                        ))
                        .with_reasoning_content(ReasoningContent::reasoning_text(
                            format!("stale thought {round}"),
                            format!("stale-signature-{round}"),
                        )),
                    }),
                    Message::Assistant(assistant),
                ]);
                request
                    .conversation_state
                    .current_message
                    .user_input_message
                    .user_input_message_context = UserInputMessageContext::new()
                    .with_tool_results(vec![ToolResult::success("tool-1", "done")]);

                let (body, report) = guard_kiro_request(
                    &mut request,
                    shaping_config(PayloadShapingConfig {
                        truncate_historical_tool_results: false,
                        compress_tool_definitions: false,
                        web_fetch_trim_enabled: false,
                        ..PayloadShapingConfig::default()
                    }),
                )
                .expect("guard");

                assert_eq!(report.removed_history_thinking_blocks, 2, "round {round}");
                assert!(
                    report.removed_history_thinking_chars >= native_bytes,
                    "round {round}"
                );
                let Message::Assistant(stale) = &request.conversation_state.history[1] else {
                    panic!("expected stale assistant");
                };
                assert_eq!(
                    stale.assistant_response_message.content,
                    "stale-visible\n\nstale-answer"
                );
                assert!(stale.assistant_response_message.reasoning_content.is_none());
                let Message::Assistant(assistant) = &request.conversation_state.history[2] else {
                    panic!("expected assistant");
                };
                assert_eq!(
                    assistant.assistant_response_message.content,
                    format!(
                        "active-visible\n<thinking>active-legacy {round}</thinking>\nactive-answer"
                    )
                );
                assert!(
                    assistant
                        .assistant_response_message
                        .reasoning_content
                        .is_some()
                );
                assert_eq!(
                    assistant
                        .assistant_response_message
                        .tool_uses
                        .as_ref()
                        .map(Vec::len),
                    Some(1)
                );
                assert!(body.contains("reasoningContent"));
                if native_is_signed {
                    assert!(body.contains("signature-"));
                }
                assert!(body.contains("<thinking>"));
            }
        }
    }

    #[test]
    fn anthropic_guard_discards_historical_thinking_even_when_body_fits() {
        let mut request = anthropic_request(vec![
            anthropic_message("user", serde_json::json!("question")),
            anthropic_message(
                "assistant",
                serde_json::json!([
                    {
                        "type": "thinking",
                        "thinking": "signed history",
                        "signature": "invalid-signature"
                    },
                    {"type": "text", "text": "visible answer"}
                ]),
            ),
            anthropic_message("user", serde_json::json!("continue")),
        ]);

        let (body, report) = guard_anthropic_messages_request(
            &mut request,
            guard_config_with_shaping(
                usize::MAX,
                false,
                PayloadShapingConfig {
                    truncate_historical_tool_results: false,
                    compress_tool_definitions: false,
                    web_fetch_trim_enabled: false,
                    discard_historical_thinking: true,
                    ..PayloadShapingConfig::default()
                },
            ),
            512,
        )
        .expect("guard");

        assert_eq!(report.removed_history_thinking_blocks, 1);
        assert!(!body.contains("invalid-signature"));
        assert!(!body.contains("signed history"));
        assert!(body.contains("visible answer"));
    }

    #[test]
    fn anthropic_guard_drops_oversized_historical_images_even_when_body_fits() {
        let oversized = base64_zeros_for_decoded_bytes(UPSTREAM_IMAGE_SOURCE_MAX_BYTES + 1);
        let mut request = anthropic_request(vec![
            anthropic_message(
                "user",
                serde_json::json!([
                    {
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": "image/png",
                            "data": oversized
                        }
                    }
                ]),
            ),
            anthropic_message("user", serde_json::json!("continue")),
        ]);

        let (body, report) = guard_anthropic_messages_request(
            &mut request,
            guard_config_with_shaping(
                usize::MAX,
                false,
                PayloadShapingConfig {
                    truncate_historical_tool_results: false,
                    discard_historical_thinking: false,
                    compress_tool_definitions: false,
                    web_fetch_trim_enabled: false,
                    ..PayloadShapingConfig::default()
                },
            ),
            512,
        )
        .expect("guard");

        assert_eq!(report.dropped_historical_images, 1);
        assert_eq!(
            report.dropped_historical_image_bytes,
            UPSTREAM_IMAGE_SOURCE_MAX_BYTES + 1
        );
        assert!(!body.contains(r#""type":"image""#));
        assert!(body.contains("Historical image was omitted"));
    }

    #[test]
    fn image_source_size_uses_decoded_base64_bytes_for_five_rounds() {
        for round in 0..5 {
            for decoded_bytes in [
                UPSTREAM_IMAGE_SOURCE_MAX_BYTES - 1,
                UPSTREAM_IMAGE_SOURCE_MAX_BYTES,
                UPSTREAM_IMAGE_SOURCE_MAX_BYTES + 1,
            ] {
                let encoded = base64_zeros_for_decoded_bytes(decoded_bytes);
                let plain = serde_json::json!({
                    "type": "image",
                    "source": {"type": "base64", "data": encoded}
                });
                assert_eq!(
                    anthropic_image_source_bytes(&plain),
                    decoded_bytes,
                    "round {round}"
                );

                let wrapped = serde_json::json!({
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "data": format!("data:image/png;base64,{}\n", encoded)
                    }
                });
                assert_eq!(
                    anthropic_image_source_bytes(&wrapped),
                    decoded_bytes,
                    "round {round}"
                );

                let kiro = KiroImage::from_base64("png", encoded);
                assert_eq!(
                    kiro_image_source_bytes(&kiro),
                    decoded_bytes,
                    "round {round}"
                );
            }
        }
    }

    #[test]
    fn exact_five_mib_decoded_images_are_not_dropped_for_five_rounds() {
        for _round in 0..5 {
            let encoded = base64_zeros_for_decoded_bytes(UPSTREAM_IMAGE_SOURCE_MAX_BYTES);
            let mut anthropic = anthropic_request(vec![anthropic_message(
                "user",
                serde_json::json!([{
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": "image/png",
                        "data": encoded
                    }
                }]),
            )]);
            let (body, report) = guard_anthropic_messages_request(
                &mut anthropic,
                guard_config(usize::MAX),
                UPSTREAM_IMAGE_SOURCE_MAX_BYTES,
            )
            .expect("exact-limit Anthropic image");
            assert_eq!(report.dropped_current_images, 0);
            assert_eq!(
                count_content_blocks_by_type(&anthropic.messages[0].content, "image"),
                1
            );
            assert!(body.contains(r#""type":"image""#));

            let mut kiro = request_with_history(Vec::new());
            kiro.conversation_state
                .current_message
                .user_input_message
                .images = vec![KiroImage::from_base64(
                "png",
                base64_zeros_for_decoded_bytes(UPSTREAM_IMAGE_SOURCE_MAX_BYTES),
            )];
            let (_body, report) = guard_kiro_request(&mut kiro, guard_config(usize::MAX))
                .expect("exact-limit Kiro image");
            assert_eq!(report.dropped_current_images, 0);
            assert_eq!(
                kiro.conversation_state
                    .current_message
                    .user_input_message
                    .images
                    .len(),
                1
            );
        }
    }

    #[test]
    fn multiple_oversized_current_images_emit_one_summary_placeholder() {
        let encoded = base64_zeros_for_decoded_bytes(UPSTREAM_IMAGE_SOURCE_MAX_BYTES + 1);
        let image = || {
            serde_json::json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/png",
                    "data": encoded.clone()
                }
            })
        };
        let mut request = anthropic_request(vec![anthropic_message(
            "user",
            serde_json::json!([
                {"type": "text", "text": "before"},
                image(),
                image(),
                {"type": "text", "text": "after"}
            ]),
        )]);

        let (body, report) =
            guard_anthropic_messages_request(&mut request, guard_config(usize::MAX), encoded.len())
                .expect("guard");

        assert_eq!(report.dropped_current_images, 2);
        assert_eq!(
            report.dropped_current_image_bytes,
            2 * (UPSTREAM_IMAGE_SOURCE_MAX_BYTES + 1)
        );
        assert_eq!(body.matches("Current images were omitted").count(), 1);
        assert!(!body.contains(r#""type":"image""#));
        assert!(body.contains("before"));
        assert!(body.contains("after"));
    }

    #[test]
    fn anthropic_guard_drops_oversized_current_images_even_when_body_fits() {
        let oversized = base64_zeros_for_decoded_bytes(UPSTREAM_IMAGE_SOURCE_MAX_BYTES + 1);
        let mut request = anthropic_request(vec![anthropic_message(
            "user",
            serde_json::json!([
                {
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": "image/png",
                        "data": oversized
                    }
                }
            ]),
        )]);

        let (body, report) = guard_anthropic_messages_request(
            &mut request,
            guard_config_with_shaping(
                usize::MAX,
                false,
                PayloadShapingConfig {
                    truncate_historical_tool_results: false,
                    discard_historical_thinking: false,
                    compress_tool_definitions: false,
                    web_fetch_trim_enabled: false,
                    ..PayloadShapingConfig::default()
                },
            ),
            512,
        )
        .expect("guard");

        assert_eq!(report.dropped_current_images, 1);
        assert_eq!(
            report.dropped_current_image_bytes,
            UPSTREAM_IMAGE_SOURCE_MAX_BYTES + 1
        );
        assert_eq!(report.dropped_historical_images, 0);
        assert!(!body.contains(r#""type":"image""#));
        assert!(body.contains("Current image was omitted"));
    }

    #[test]
    fn anthropic_guard_rejects_oversized_images_when_configured() {
        let oversized = base64_zeros_for_decoded_bytes(UPSTREAM_IMAGE_SOURCE_MAX_BYTES + 1);
        let mut request = anthropic_request(vec![anthropic_message(
            "user",
            serde_json::json!([
                {
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": "image/png",
                        "data": oversized
                    }
                }
            ]),
        )]);

        let err = guard_anthropic_messages_request(
            &mut request,
            guard_config_with_shaping(
                usize::MAX,
                false,
                PayloadShapingConfig {
                    oversized_image_handling: OversizedImageHandling::Reject,
                    ..PayloadShapingConfig::default()
                },
            ),
            512,
        )
        .expect_err("oversized image should be rejected");

        assert!(matches!(
            err,
            PayloadGuardError::OversizedImage {
                current_images: 1,
                historical_images: 0,
                max_source_bytes: UPSTREAM_IMAGE_SOURCE_MAX_BYTES,
                ..
            }
        ));
        assert_eq!(
            count_content_blocks_by_type(&request.messages[0].content, "image"),
            1
        );
    }

    #[test]
    fn payload_shaping_trims_web_fetch_history_before_generic_tool_result_truncation() {
        let assistant = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage::new("web fetch")
                .with_tool_uses(vec![ToolUseEntry::new("web-1", "WebFetch")]),
        };
        let body = (0..250)
            .map(|idx| format!("line-{idx} {}", "content".repeat(20)))
            .collect::<Vec<_>>()
            .join("\n");
        let web_fetch_result = format!(
            "Fetched https://example.test\n\nWeb page content:\n---\n{}\n---\n\nmetadata",
            body
        );
        let mut user = HistoryUserMessage::new("web result", TEST_MODEL);
        user.user_input_message.user_input_message_context = UserInputMessageContext::new()
            .with_tool_results(vec![ToolResult::success("web-1", web_fetch_result)]);
        let mut request = request_with_history(vec![
            Message::User(HistoryUserMessage::new("fetch", TEST_MODEL)),
            Message::Assistant(assistant),
            Message::User(user),
        ]);

        let (_body, report) = guard_kiro_request(
            &mut request,
            shaping_config(PayloadShapingConfig {
                historical_tool_result_max_chars: 1_000,
                historical_tool_result_head_lines: 4,
                historical_tool_result_tail_lines: 2,
                web_fetch_body_max_chars: 3_000,
                discard_historical_thinking: false,
                compress_tool_definitions: false,
                ..PayloadShapingConfig::default()
            }),
        )
        .expect("guard");

        assert_eq!(report.trimmed_web_fetch_blocks, 1);
        assert_eq!(report.truncated_history_tool_results, 0);
        assert_eq!(report.flattened_history_tool_uses, 0);
        assert_eq!(report.textified_history_tool_results, 0);
        let Message::User(user) = &request.conversation_state.history[2] else {
            panic!("expected historical user");
        };
        assert_eq!(
            user.user_input_message
                .user_input_message_context
                .tool_results
                .len(),
            1
        );
        let text = tool_result_text(
            &user
                .user_input_message
                .user_input_message_context
                .tool_results[0],
        );
        assert!(text.contains("Proxy note: web page navigation"));
        assert!(text.chars().count() > 1_000);
        assert!(text.chars().count() < 4_120);
        assert_eq!(user.user_input_message.content, "web result");
    }

    #[test]
    fn incomplete_web_fetch_history_falls_back_to_generic_tool_result_truncation() {
        let assistant = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage::new("web fetch")
                .with_tool_uses(vec![ToolUseEntry::new("web-1", "WebFetch")]),
        };
        let malformed_web_fetch = format!(
            "Fetched https://example.test\n\nWeb page content:\n---\n{}",
            "body ".repeat(5_000)
        );
        let mut user = HistoryUserMessage::new("web result", TEST_MODEL);
        user.user_input_message.user_input_message_context = UserInputMessageContext::new()
            .with_tool_results(vec![ToolResult::success("web-1", malformed_web_fetch)]);
        let mut request = request_with_history(vec![
            Message::User(HistoryUserMessage::new("fetch", TEST_MODEL)),
            Message::Assistant(assistant),
            Message::User(user),
        ]);

        let (_body, report) = guard_kiro_request(
            &mut request,
            shaping_config(PayloadShapingConfig {
                historical_tool_result_max_chars: 1_000,
                web_fetch_body_max_chars: 3_000,
                discard_historical_thinking: false,
                compress_tool_definitions: false,
                ..PayloadShapingConfig::default()
            }),
        )
        .expect("guard");

        assert_eq!(report.trimmed_web_fetch_blocks, 0);
        assert_eq!(report.truncated_history_tool_results, 1);
        assert_eq!(report.flattened_history_tool_uses, 0);
        assert_eq!(report.textified_history_tool_results, 0);
        let Message::User(user) = &request.conversation_state.history[2] else {
            panic!("expected historical user");
        };
        assert_eq!(
            user.user_input_message
                .user_input_message_context
                .tool_results
                .len(),
            1
        );
        let text = tool_result_text(
            &user
                .user_input_message
                .user_input_message_context
                .tool_results[0],
        );
        assert!(text.chars().count() <= 1_120);
        assert_eq!(user.user_input_message.content, "web result");
    }

    #[test]
    fn payload_shaping_compresses_current_tool_definitions_without_removing_tools() {
        for round in 0..5 {
            let mut request = request_with_history(Vec::new());
            request
                .conversation_state
                .current_message
                .user_input_message
                .user_input_message_context =
                UserInputMessageContext::new().with_tools(vec![Tool {
                    tool_specification: ToolSpecification {
                        name: "largeTool".to_string(),
                        description: "description ".repeat(1_000),
                        input_schema: InputSchema::from_json(serde_json::json!({
                            "type": "object",
                            "description": "root ".repeat(500),
                            "properties": {
                                "path": {
                                    "type": "string",
                                    "description": "path ".repeat(500),
                                    "examples": [
                                        "example ".repeat(500),
                                        "example ".repeat(500),
                                        "example ".repeat(500),
                                        "example ".repeat(500)
                                    ]
                                }
                            }
                        })),
                    },
                }]);

            let (_body, report) = guard_kiro_request(
                &mut request,
                shaping_config(PayloadShapingConfig {
                    truncate_historical_tool_results: false,
                    discard_historical_thinking: false,
                    web_fetch_trim_enabled: false,
                    tool_definitions_budget_bytes: 1_024,
                    tool_description_max_chars: 256,
                    tool_schema_annotation_max_chars: 128,
                    ..PayloadShapingConfig::default()
                }),
            )
            .expect("guard");

            assert!(report.compressed_tool_definitions > 0, "round {round}");
            assert!(report.compressed_tool_definition_bytes > 0, "round {round}");
            let tools = &request
                .conversation_state
                .current_message
                .user_input_message
                .user_input_message_context
                .tools;
            assert_eq!(tools.len(), 1);
            assert_eq!(tools[0].tool_specification.name, "largeTool");
            assert!(
                tools[0]
                    .tool_specification
                    .description
                    .contains("tool description truncated by proxy")
            );
            assert!(
                serde_json::to_string(&tools[0].tool_specification.input_schema)
                    .expect("schema")
                    .contains("schema annotation truncated by proxy")
            );
        }
    }

    #[test]
    fn payload_shaping_tool_definition_budget_zero_disables_tool_compression() {
        for round in 0..5 {
            let mut request = request_with_history(Vec::new());
            let description = "description ".repeat(1_000);
            request
                .conversation_state
                .current_message
                .user_input_message
                .user_input_message_context =
                UserInputMessageContext::new().with_tools(vec![Tool {
                    tool_specification: ToolSpecification {
                        name: "largeTool".to_string(),
                        description: description.clone(),
                        input_schema: InputSchema::from_json(serde_json::json!({
                            "type": "object",
                            "description": "root ".repeat(500),
                            "properties": {
                                "path": {
                                    "type": "string",
                                    "description": "path ".repeat(500)
                                }
                            }
                        })),
                    },
                }]);

            let (_body, report) = guard_kiro_request(
                &mut request,
                shaping_config(PayloadShapingConfig {
                    truncate_historical_tool_results: false,
                    discard_historical_thinking: false,
                    web_fetch_trim_enabled: false,
                    tool_definitions_budget_bytes: 0,
                    tool_description_max_chars: 256,
                    tool_schema_annotation_max_chars: 128,
                    ..PayloadShapingConfig::default()
                }),
            )
            .expect("guard");

            assert_eq!(report.compressed_tool_definitions, 0, "round {round}");
            assert_eq!(
                request
                    .conversation_state
                    .current_message
                    .user_input_message
                    .user_input_message_context
                    .tools[0]
                    .tool_specification
                    .description,
                description
            );
        }
    }

    #[test]
    fn payload_breakdown_reports_current_tool_and_history_sizes() {
        let assistant = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage::new("tool call")
                .with_tool_uses(vec![ToolUseEntry::new("tool-1", "readFile")])
                .with_reasoning_content(ReasoningContent::reasoning_text(
                    "native thought",
                    "signature",
                )),
        };
        let mut user = HistoryUserMessage::new("result message", TEST_MODEL);
        user.user_input_message.user_input_message_context = UserInputMessageContext::new()
            .with_tool_results(vec![ToolResult::success("tool-1", "valid result")]);
        let mut request = request_with_history(vec![
            Message::User(HistoryUserMessage::new("read", TEST_MODEL)),
            Message::Assistant(assistant),
            Message::User(user),
        ]);
        request
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context = UserInputMessageContext::new()
            .with_tools(vec![Tool {
                tool_specification: ToolSpecification {
                    name: "readFile".to_string(),
                    description: "read files".to_string(),
                    input_schema: InputSchema::from_json(serde_json::json!({
                        "type": "object",
                        "properties": {
                            "path": {"type": "string"}
                        }
                    })),
                },
            }])
            .with_tool_results(vec![ToolResult::success("tool-current", "current result")]);

        let body = serde_json::to_string(&request).expect("serialize");
        let breakdown = breakdown_kiro_request(&request, &body);

        assert_eq!(breakdown.total_bytes, body.len());
        assert_eq!(breakdown.history_entries, 3);
        assert_eq!(breakdown.current_tool_count, 1);
        assert_eq!(breakdown.history_tool_use_count, 1);
        assert_eq!(breakdown.history_tool_result_count, 1);
        assert_eq!(breakdown.history_reasoning_content_count, 1);
        assert!(breakdown.current_tools_bytes > 0);
        assert!(breakdown.current_tool_results_bytes > 0);
        assert!(breakdown.history_tool_results_bytes > 0);
        assert!(breakdown.history_reasoning_content_bytes > 0);
        assert!(breakdown.largest_tool_bytes > 0);
        assert!(breakdown.largest_history_tool_result_bytes > 0);
        assert!(breakdown.largest_current_tool_result_bytes > 0);
    }

    #[test]
    fn anthropic_breakdown_counts_whole_native_reasoning_blocks_for_five_rounds() {
        for round in 0..5 {
            let mut request = anthropic_request(vec![
                anthropic_message("user", serde_json::json!("initial")),
                anthropic_message(
                    "assistant",
                    serde_json::json!([
                        {
                            "type": "thinking",
                            "thinking": format!("thought {round}"),
                            "signature": format!("signature-{round}")
                        },
                        {
                            "type": "redacted_thinking",
                            "data": base64_zeros_for_decoded_bytes(round + 1)
                        },
                        {"type": "text", "text": "visible"}
                    ]),
                ),
                anthropic_message("user", serde_json::json!("continue")),
            ]);
            let total_bytes = serde_json::to_vec(&request).unwrap().len();
            let breakdown = breakdown_anthropic_messages_request(&request, total_bytes);

            assert_eq!(
                breakdown.history_reasoning_content_count, 2,
                "round {round}"
            );
            let expected_bytes = request.messages[1]
                .content
                .as_array()
                .unwrap()
                .iter()
                .take(2)
                .map(json_len)
                .sum::<usize>();
            assert_eq!(
                breakdown.history_reasoning_content_bytes, expected_bytes,
                "round {round}"
            );

            let (_body, report) = guard_anthropic_messages_request(
                &mut request,
                guard_config_with_shaping(
                    usize::MAX,
                    false,
                    PayloadShapingConfig {
                        truncate_historical_tool_results: false,
                        compress_tool_definitions: false,
                        web_fetch_trim_enabled: false,
                        discard_historical_thinking: true,
                        ..PayloadShapingConfig::default()
                    },
                ),
                total_bytes,
            )
            .expect("guard");
            assert_eq!(report.removed_history_thinking_blocks, 2, "round {round}");
            let after = breakdown_anthropic_messages_request(&request, total_bytes);
            assert_eq!(after.history_reasoning_content_count, 0, "round {round}");
            assert_eq!(after.history_reasoning_content_bytes, 0, "round {round}");
        }
    }

    #[test]
    fn image_history_bytes_are_counted() {
        let mut user = HistoryUserMessage::new("image", TEST_MODEL);
        user.user_input_message.images = vec![KiroImage::from_base64("png", "a".repeat(2048))];
        let mut request = request_with_history(vec![Message::User(user)]);

        let (_body, report) =
            guard_kiro_request(&mut request, guard_config(usize::MAX)).expect("guard");

        assert!(report.original_bytes > 2048);
    }

    #[test]
    fn kiro_guard_drops_oversized_historical_images_even_when_body_fits() {
        let mut user = HistoryUserMessage::new("image", TEST_MODEL);
        user.user_input_message.images = vec![KiroImage::from_base64(
            "png",
            base64_zeros_for_decoded_bytes(UPSTREAM_IMAGE_SOURCE_MAX_BYTES + 1),
        )];
        let mut request = request_with_history(vec![Message::User(user)]);

        let (body, report) =
            guard_kiro_request(&mut request, guard_config(usize::MAX)).expect("guard");

        assert_eq!(report.dropped_historical_images, 1);
        assert_eq!(
            report.dropped_historical_image_bytes,
            UPSTREAM_IMAGE_SOURCE_MAX_BYTES + 1
        );
        let Message::User(user) = &request.conversation_state.history[0] else {
            panic!("expected user");
        };
        assert!(user.user_input_message.images.is_empty());
        assert!(user.user_input_message.content.contains(
            "Historical image was omitted because it exceeded the upstream 5 MB image size limit"
        ));
        assert!(!body.contains(&"A".repeat(128)));
    }

    #[test]
    fn kiro_guard_drops_oversized_current_images_even_when_body_fits() {
        let mut request = request_with_history(Vec::new());
        request
            .conversation_state
            .current_message
            .user_input_message
            .images = vec![KiroImage::from_base64(
            "png",
            base64_zeros_for_decoded_bytes(UPSTREAM_IMAGE_SOURCE_MAX_BYTES + 1),
        )];

        let (body, report) =
            guard_kiro_request(&mut request, guard_config(usize::MAX)).expect("guard");

        assert_eq!(report.dropped_current_images, 1);
        assert_eq!(
            report.dropped_current_image_bytes,
            UPSTREAM_IMAGE_SOURCE_MAX_BYTES + 1
        );
        assert!(
            request
                .conversation_state
                .current_message
                .user_input_message
                .images
                .is_empty()
        );
        assert!(
            request
                .conversation_state
                .current_message
                .user_input_message
                .content
                .contains("Current image was omitted because it exceeded the upstream 5 MB image size limit")
        );
        assert!(!body.contains(&"A".repeat(128)));
    }

    #[test]
    fn kiro_guard_rejects_oversized_images_when_configured() {
        let mut request = request_with_history(Vec::new());
        request
            .conversation_state
            .current_message
            .user_input_message
            .images = vec![KiroImage::from_base64(
            "png",
            base64_zeros_for_decoded_bytes(UPSTREAM_IMAGE_SOURCE_MAX_BYTES + 1),
        )];

        let err = guard_kiro_request(
            &mut request,
            guard_config_with_shaping(
                usize::MAX,
                true,
                PayloadShapingConfig {
                    oversized_image_handling: OversizedImageHandling::Reject,
                    ..PayloadShapingConfig::default()
                },
            ),
        )
        .expect_err("oversized image should be rejected");

        assert!(matches!(
            err,
            PayloadGuardError::OversizedImage {
                current_images: 1,
                historical_images: 0,
                max_source_bytes: UPSTREAM_IMAGE_SOURCE_MAX_BYTES,
                ..
            }
        ));
        assert_eq!(
            request
                .conversation_state
                .current_message
                .user_input_message
                .images
                .len(),
            1
        );
        assert!(
            !request
                .conversation_state
                .current_message
                .user_input_message
                .content
                .contains("omitted")
        );
    }
}
