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
use crate::model::config::PayloadShapingConfig;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const CURRENT_FIT_MIN_TEXT_CHARS: usize = 512;
const CURRENT_FIT_MAX_ITERATIONS: usize = 64;
const CURRENT_FIT_OVERHEAD_BYTES: usize = 512;
const PAYLOAD_GUARD_SLOW_LOG_THRESHOLD: Duration = Duration::from_millis(25);
const EMPTY_TOOL_RESULT_CONTENT_PLACEHOLDER: &str = "Tool result content was empty.";

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
    pub history_entries: usize,
    pub current_tool_count: usize,
    pub current_tool_result_count: usize,
    pub current_image_count: usize,
    pub largest_tool_bytes: usize,
    pub largest_history_tool_result_bytes: usize,
    pub largest_current_tool_result_bytes: usize,
    pub history_tool_use_count: usize,
    pub history_tool_result_count: usize,
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
            aligned_leading_entries: 0,
            removed_empty_tool_uses: 0,
            removed_duplicate_tool_uses: 0,
            renamed_duplicate_tool_uses: 0,
            removed_orphan_tool_results: 0,
            removed_duplicate_tool_results: 0,
            textified_duplicate_tool_results: 0,
            textified_orphan_tool_results: 0,
            removed_orphan_tool_uses: 0,
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
            || self.truncated_history_tool_results > 0
            || self.removed_history_thinking_blocks > 0
            || self.trimmed_web_fetch_blocks > 0
            || self.compressed_tool_definitions > 0
            || self.truncated_current_tool_results > 0
            || self.truncated_current_documents > 0
            || self.truncated_current_user_content > 0
            || self.dropped_current_images > 0
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
        if self.still_oversized {
            parts.push(format!("payload-oversized={}", self.final_bytes));
        }
        Some(parts.join(","))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadGuardError {
    Serialize(String),
}

impl std::fmt::Display for PayloadGuardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PayloadGuardError::Serialize(err) => write!(f, "序列化请求失败: {}", err),
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

    let serialize_started_at = Instant::now();
    let original_body = serialize_request(request)?;
    serialize_elapsed += serialize_started_at.elapsed();
    let original_bytes = original_body.len();
    let original_history_entries = request.conversation_state.history.len();

    if !config.enabled {
        return Ok((
            original_body,
            PayloadGuardReport::disabled(original_bytes, original_history_entries),
        ));
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
        aligned_leading_entries: 0,
        removed_empty_tool_uses: 0,
        removed_duplicate_tool_uses: 0,
        renamed_duplicate_tool_uses: 0,
        removed_orphan_tool_results: 0,
        removed_duplicate_tool_results: 0,
        textified_duplicate_tool_results: 0,
        textified_orphan_tool_results: 0,
        removed_orphan_tool_uses: 0,
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
        still_oversized: false,
    };

    let repair_started_at = Instant::now();
    report.aligned_leading_entries +=
        align_history_to_user(&mut request.conversation_state.history);

    let initial_repair = repair_request(request);
    add_repair_stats_to_report(&mut report, initial_repair);
    repair_elapsed += repair_started_at.elapsed();

    let serialize_started_at = Instant::now();
    let mut body = serialize_request(request)?;
    serialize_elapsed += serialize_started_at.elapsed();
    report.final_bytes = body.len();

    if size_limit_enabled && report.final_bytes > config.max_bytes && config.shaping.enabled {
        let shaping_started_at = Instant::now();
        let shaping = apply_payload_shaping(request, config.shaping);
        report.truncated_history_tool_results += shaping.truncated_history_tool_results;
        report.truncated_history_tool_result_chars += shaping.truncated_history_tool_result_chars;
        report.removed_history_thinking_blocks += shaping.removed_history_thinking_blocks;
        report.removed_history_thinking_chars += shaping.removed_history_thinking_chars;
        report.trimmed_web_fetch_blocks += shaping.trimmed_web_fetch_blocks;
        report.trimmed_web_fetch_chars += shaping.trimmed_web_fetch_chars;
        report.compressed_tool_definitions += shaping.compressed_tool_definitions;
        report.compressed_tool_definition_bytes += shaping.compressed_tool_definition_bytes;

        if shaping.was_modified() {
            let serialize_started_at = Instant::now();
            body = serialize_request(request)?;
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
            let before = request.conversation_state.history.len();
            trim_oldest_history_unit(&mut request.conversation_state.history);
            let after_trim = request.conversation_state.history.len();
            report.trimmed_history_entries += before.saturating_sub(after_trim);

            let aligned = align_history_to_user(&mut request.conversation_state.history);
            report.aligned_leading_entries += aligned;

            let repair = repair_request(request);
            add_repair_stats_to_report(&mut report, repair);

            let serialize_started_at = Instant::now();
            body = serialize_request(request)?;
            serialize_elapsed += serialize_started_at.elapsed();
            let new_size = body.len();
            if new_size >= report.final_bytes && after_trim == before {
                break;
            }
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
        )?;
        report.truncated_current_tool_results += current_stats.truncated_current_tool_results;
        report.truncated_current_tool_result_chars +=
            current_stats.truncated_current_tool_result_chars;
        report.truncated_current_documents += current_stats.truncated_current_documents;
        report.truncated_current_document_chars += current_stats.truncated_current_document_chars;
        report.truncated_current_user_content += current_stats.truncated_current_user_content;
        report.truncated_current_user_content_chars +=
            current_stats.truncated_current_user_content_chars;
        report.dropped_current_images += current_stats.dropped_current_images;
        report.dropped_current_image_bytes += current_stats.dropped_current_image_bytes;
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
                serialize_elapsed += serialize_started_at.elapsed();
                report.final_bytes = body.len();
            }
        }
    }

    report.final_history_entries = request.conversation_state.history.len();
    report.final_bytes = body.len();
    report.still_oversized = size_limit_enabled && report.final_bytes > config.max_bytes;

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
    }
}

pub fn guard_anthropic_messages_request(
    request: &mut MessagesRequest,
    config: PayloadGuardConfig,
    original_body_bytes: usize,
) -> Result<(String, PayloadGuardReport), PayloadGuardError> {
    let guard_started_at = Instant::now();
    let mut serialize_elapsed = Duration::ZERO;
    let mut repair_elapsed = Duration::ZERO;
    let mut shaping_elapsed = Duration::ZERO;
    let mut trim_elapsed = Duration::ZERO;
    let mut current_shaping_elapsed = Duration::ZERO;
    let mut history_trim_iterations = 0usize;

    let serialize_started_at = Instant::now();
    let mut body = serialize_anthropic_request(request)?;
    serialize_elapsed += serialize_started_at.elapsed();
    let original_history_entries = request.messages.len().saturating_sub(1);

    if !config.enabled {
        return Ok((
            body,
            PayloadGuardReport::disabled(original_body_bytes, original_history_entries),
        ));
    }

    let size_limit_enabled = config.max_bytes > 0;
    let mut report = new_payload_guard_report(
        config.max_bytes,
        original_body_bytes,
        original_history_entries,
    );
    let mut final_bytes = if size_limit_enabled && original_body_bytes > config.max_bytes {
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
        let serialize_started_at = Instant::now();
        body = serialize_anthropic_request(request)?;
        serialize_elapsed += serialize_started_at.elapsed();
        final_bytes = body.len();
        report.final_bytes = final_bytes;
    }

    if size_limit_enabled && final_bytes > config.max_bytes && config.shaping.enabled {
        let shaping_started_at = Instant::now();
        let shaping = apply_anthropic_payload_shaping(request, config.shaping);
        report.truncated_history_tool_results += shaping.truncated_history_tool_results;
        report.truncated_history_tool_result_chars += shaping.truncated_history_tool_result_chars;
        report.removed_history_thinking_blocks += shaping.removed_history_thinking_blocks;
        report.removed_history_thinking_chars += shaping.removed_history_thinking_chars;
        report.trimmed_web_fetch_blocks += shaping.trimmed_web_fetch_blocks;
        report.trimmed_web_fetch_chars += shaping.trimmed_web_fetch_chars;
        report.compressed_tool_definitions += shaping.compressed_tool_definitions;
        report.compressed_tool_definition_bytes += shaping.compressed_tool_definition_bytes;

        if shaping.was_modified() {
            let serialize_started_at = Instant::now();
            body = serialize_anthropic_request(request)?;
            serialize_elapsed += serialize_started_at.elapsed();
            final_bytes = body.len();
            report.final_bytes = final_bytes;
        }
        shaping_elapsed += shaping_started_at.elapsed();
    }

    if size_limit_enabled && config.trim_history {
        let trim_started_at = Instant::now();
        while final_bytes > config.max_bytes && request.messages.len() > 1 {
            history_trim_iterations += 1;
            let before = request.messages.len();
            trim_oldest_anthropic_history_unit(&mut request.messages);
            let after_trim = request.messages.len();
            report.trimmed_history_entries += before.saturating_sub(after_trim);

            let repair_started_at = Instant::now();
            let repair = repair_anthropic_messages(request);
            add_anthropic_repair_stats_to_report(&mut report, repair);
            repair_elapsed += repair_started_at.elapsed();

            let serialize_started_at = Instant::now();
            body = serialize_anthropic_request(request)?;
            serialize_elapsed += serialize_started_at.elapsed();
            let new_size = body.len();
            if new_size >= final_bytes && after_trim == before {
                break;
            }
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
        let (new_body, current_stats) = apply_anthropic_current_payload_shaping_until_fit(
            request,
            config.shaping,
            config.max_bytes,
            body,
        )?;
        report.truncated_current_tool_results += current_stats.truncated_current_tool_results;
        report.truncated_current_tool_result_chars +=
            current_stats.truncated_current_tool_result_chars;
        report.truncated_current_documents += current_stats.truncated_current_documents;
        report.truncated_current_document_chars += current_stats.truncated_current_document_chars;
        report.truncated_current_user_content += current_stats.truncated_current_user_content;
        report.truncated_current_user_content_chars +=
            current_stats.truncated_current_user_content_chars;
        report.dropped_current_images += current_stats.dropped_current_images;
        report.dropped_current_image_bytes += current_stats.dropped_current_image_bytes;
        body = new_body;
        final_bytes = body.len();
        report.final_bytes = final_bytes;
        current_shaping_elapsed += current_started_at.elapsed();

        if current_stats.was_modified() {
            let repair_started_at = Instant::now();
            let repair = repair_anthropic_messages(request);
            let should_reserialize = repair.was_modified();
            add_anthropic_repair_stats_to_report(&mut report, repair);
            repair_elapsed += repair_started_at.elapsed();

            if should_reserialize {
                let serialize_started_at = Instant::now();
                body = serialize_anthropic_request(request)?;
                serialize_elapsed += serialize_started_at.elapsed();
                final_bytes = body.len();
                report.final_bytes = final_bytes;
            }
        }
    }

    report.final_history_entries = request.messages.len().saturating_sub(1);
    report.still_oversized = size_limit_enabled && final_bytes > config.max_bytes;
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
        aligned_leading_entries: 0,
        removed_empty_tool_uses: 0,
        removed_duplicate_tool_uses: 0,
        renamed_duplicate_tool_uses: 0,
        removed_orphan_tool_results: 0,
        removed_duplicate_tool_results: 0,
        textified_duplicate_tool_results: 0,
        textified_orphan_tool_results: 0,
        removed_orphan_tool_uses: 0,
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
        still_oversized: false,
    }
}

fn serialize_request(request: &KiroRequest) -> Result<String, PayloadGuardError> {
    serde_json::to_string(request).map_err(|err| PayloadGuardError::Serialize(err.to_string()))
}

fn serialize_anthropic_request(request: &MessagesRequest) -> Result<String, PayloadGuardError> {
    serde_json::to_string(request).map_err(|err| PayloadGuardError::Serialize(err.to_string()))
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
            original_bytes = report.original_bytes,
            final_bytes = report.final_bytes,
            max_bytes = report.max_bytes,
            modified = report.was_modified(),
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

#[derive(Default)]
struct ShapingStats {
    truncated_history_tool_results: usize,
    truncated_history_tool_result_chars: usize,
    removed_history_thinking_blocks: usize,
    removed_history_thinking_chars: usize,
    trimmed_web_fetch_blocks: usize,
    trimmed_web_fetch_chars: usize,
    compressed_tool_definitions: usize,
    compressed_tool_definition_bytes: usize,
}

impl ShapingStats {
    fn was_modified(&self) -> bool {
        self.truncated_history_tool_results > 0
            || self.removed_history_thinking_blocks > 0
            || self.trimmed_web_fetch_blocks > 0
            || self.compressed_tool_definitions > 0
    }
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
        let result = discard_history_thinking(&mut request.conversation_state.history);
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
        let result = discard_anthropic_history_thinking(&mut request.messages[..history_end]);
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

fn discard_anthropic_history_thinking(messages: &mut [AnthropicMessage]) -> (usize, usize) {
    let mut removed_blocks = 0usize;
    let mut removed_chars = 0usize;
    for message in messages {
        if message.role != "assistant" {
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
            let is_thinking = matches!(
                block_type(block),
                Some("thinking") | Some("redacted_thinking")
            ) || block.get("thinking").is_some()
                || block.get("signature").is_some();
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
    textified_orphan_tool_results: usize,
    removed_orphan_tool_uses: usize,
}

impl AnthropicRepairStats {
    fn was_modified(&self) -> bool {
        self.normalized_empty_tool_results > 0
            || self.aligned_leading_entries > 0
            || self.removed_orphan_tool_results > 0
            || self.textified_orphan_tool_results > 0
            || self.removed_orphan_tool_uses > 0
    }
}

fn repair_anthropic_messages(request: &mut MessagesRequest) -> AnthropicRepairStats {
    let mut stats = AnthropicRepairStats::default();
    stats.normalized_empty_tool_results +=
        normalize_empty_anthropic_tool_result_contents(&mut request.messages);
    stats.aligned_leading_entries += align_anthropic_messages_to_user(&mut request.messages);
    let result = textify_orphan_anthropic_tool_results(&mut request.messages);
    stats.removed_orphan_tool_results += result.0;
    stats.textified_orphan_tool_results += result.1;
    stats.removed_orphan_tool_uses += remove_unpaired_anthropic_tool_uses(&mut request.messages);
    stats
}

fn add_anthropic_repair_stats_to_report(
    report: &mut PayloadGuardReport,
    repair: AnthropicRepairStats,
) {
    report.aligned_leading_entries += repair.aligned_leading_entries;
    report.removed_orphan_tool_results += repair.removed_orphan_tool_results;
    report.textified_orphan_tool_results += repair.textified_orphan_tool_results;
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
    let mut removed = 0usize;
    while messages.len() > 1
        && messages
            .first()
            .is_some_and(|message| message.role == "assistant")
    {
        messages.remove(0);
        removed += 1;
    }
    removed
}

fn textify_orphan_anthropic_tool_results(messages: &mut [AnthropicMessage]) -> (usize, usize) {
    let mut removed = 0usize;
    let mut textified = 0usize;
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
        for block in blocks {
            if block_type(block) != Some("tool_result") {
                continue;
            }
            let id = block
                .get("tool_use_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !valid_ids.contains(id) {
                let text = anthropic_tool_result_to_text(block);
                *block = serde_json::json!({
                    "type": "text",
                    "text": format!("[trimmed orphan tool_result {}]\n{}", id, text),
                });
                removed += 1;
                textified += 1;
            }
        }
    }
    (removed, textified)
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

fn anthropic_tool_result_to_text(block: &Value) -> String {
    let Some(content) = block.get("content") else {
        return block.to_string();
    };
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    if let Some(items) = content.as_array() {
        let mut parts = Vec::new();
        for item in items {
            if let Some(text) = item.as_str() {
                parts.push(text.to_string());
            } else if let Some(text) = item.get("text").and_then(Value::as_str) {
                parts.push(text.to_string());
            } else if !item.is_null() {
                parts.push(item.to_string());
            }
        }
        return parts.join("\n");
    }
    content.to_string()
}

fn trim_oldest_anthropic_history_unit(messages: &mut Vec<AnthropicMessage>) {
    if messages.len() <= 1 {
        return;
    }
    if messages.len() >= 3
        && messages
            .first()
            .is_some_and(|message| message.role == "assistant")
        && messages.get(1).is_some_and(|message| {
            message.role == "user" && has_anthropic_tool_result(&message.content)
        })
    {
        messages.drain(0..2);
        return;
    }
    messages.remove(0);
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

fn discard_history_thinking(history: &mut [Message]) -> (usize, usize) {
    let mut blocks = 0usize;
    let mut chars = 0usize;
    for message in history {
        let Message::Assistant(assistant) = message else {
            continue;
        };
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

    body = serialize_request(request)?;

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
            let result = drop_largest_current_image(
                &mut request
                    .conversation_state
                    .current_message
                    .user_input_message,
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

        body = serialize_request(request)?;
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

    body = serialize_anthropic_request(request)?;

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
            let result = drop_largest_anthropic_current_image(request);
            if result.0 > 0 {
                stats.dropped_current_images += result.0;
                stats.dropped_current_image_bytes += result.1;
                changed = true;
            }
        }

        if !changed {
            break;
        }

        body = serialize_anthropic_request(request)?;
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
        append_anthropic_current_text(
            request,
            &format!(
                "[current images omitted by proxy: count={}, removed_json_bytes={}]",
                dropped, dropped_bytes
            ),
        );
    }
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
        append_text(
            &mut user.content,
            &format!(
                "[current images omitted by proxy: count={}, removed_json_bytes={}]",
                dropped, dropped_bytes
            ),
        );
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
    if user.images.is_empty() {
        append_text(
            &mut user.content,
            &format!(
                "[current image omitted by proxy: removed_json_bytes={}]",
                bytes
            ),
        );
    }
    (1, bytes)
}

fn trim_oldest_history_unit(history: &mut Vec<Message>) {
    if history.is_empty() {
        return;
    }

    if history.len() >= 2 && starts_with_tool_pair(history) {
        history.drain(0..2);
        return;
    }

    history.remove(0);
}

fn starts_with_tool_pair(history: &[Message]) -> bool {
    let Some(Message::Assistant(assistant)) = history.first() else {
        return false;
    };
    let has_tool_uses = assistant
        .assistant_response_message
        .tool_uses
        .as_ref()
        .is_some_and(|items| !items.is_empty());
    if !has_tool_uses {
        return false;
    }
    history
        .get(1)
        .and_then(|msg| match msg {
            Message::User(user) => Some(
                !user
                    .user_input_message
                    .user_input_message_context
                    .tool_results
                    .is_empty(),
            ),
            Message::Assistant(_) => None,
        })
        .unwrap_or(false)
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
    textified_duplicate_tool_results: usize,
    textified_orphan_tool_results: usize,
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
            || self.textified_duplicate_tool_results > 0
            || self.textified_orphan_tool_results > 0
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
    stats.textified_orphan_tool_results += history_results.textified_orphan_tool_results;

    let current_dedupe = dedupe_current_tool_results(current_user);
    stats.removed_duplicate_tool_results += current_dedupe.removed_duplicate_tool_results;
    stats.textified_duplicate_tool_results += current_dedupe.textified_duplicate_tool_results;
    let current_results = repair_current_orphan_tool_results(history, current_user);
    stats.removed_orphan_tool_results += current_results.removed_orphan_tool_results;
    stats.textified_orphan_tool_results += current_results.textified_orphan_tool_results;

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
    report.textified_duplicate_tool_results += repair.textified_duplicate_tool_results;
    report.textified_orphan_tool_results += repair.textified_orphan_tool_results;
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
            &mut user.user_input_message.content,
            false,
        )
        .removed_duplicate_tool_results;
    }
    removed
}

fn dedupe_current_tool_results(user: &mut UserInputMessage) -> RepairStats {
    dedupe_tool_results_keep_first(
        &mut user.user_input_message_context.tool_results,
        &mut user.content,
        true,
    )
}

fn dedupe_tool_results_keep_first(
    results: &mut Vec<ToolResult>,
    content: &mut String,
    textify_duplicates: bool,
) -> RepairStats {
    let mut stats = RepairStats::default();
    if results.len() <= 1 {
        return stats;
    }

    let original_len = results.len();
    let mut seen = HashSet::new();
    let mut duplicate_text = Vec::new();
    results.retain(|result| {
        let keep = !result.tool_use_id.trim().is_empty() && seen.insert(result.tool_use_id.clone());
        if !keep && textify_duplicates {
            if let Some(text) = tool_result_to_text(result) {
                duplicate_text.push(format!(
                    "[duplicate tool result {}]\n{}",
                    result.tool_use_id, text
                ));
            }
        }
        keep
    });

    stats.removed_duplicate_tool_results += original_len.saturating_sub(results.len());
    stats.textified_duplicate_tool_results += duplicate_text.len();
    if !duplicate_text.is_empty() {
        append_text(content, &duplicate_text.join("\n\n"));
    }
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
        stats.textified_orphan_tool_results += repaired.textified_orphan_tool_results;
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
    let mut orphan_text = Vec::new();
    results.retain(|result| {
        let keep = valid_ids.contains(&result.tool_use_id);
        if !keep {
            if let Some(text) = tool_result_to_text(result) {
                orphan_text.push(format!(
                    "[trimmed tool result {}]\n{}",
                    result.tool_use_id, text
                ));
            }
        }
        keep
    });
    stats.removed_orphan_tool_results += original_len.saturating_sub(results.len());
    stats.textified_orphan_tool_results += orphan_text.len();
    if !orphan_text.is_empty() {
        append_text(content, &orphan_text.join("\n\n"));
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

fn tool_result_to_text(result: &ToolResult) -> Option<String> {
    let mut parts = Vec::new();
    for item in &result.content {
        if let Some(text) = item.get("text").and_then(|value| value.as_str()) {
            if !text.is_empty() {
                parts.push(text.to_string());
            }
        } else if !item.is_empty() {
            parts.push(serde_json::Value::Object(item.clone()).to_string());
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
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
        HistoryUserMessage, KiroImage, UserInputMessage, UserInputMessageContext,
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
        let Message::User(user) = &request.conversation_state.history[2] else {
            panic!("expected user");
        };
        assert_eq!(
            user.user_input_message
                .user_input_message_context
                .tool_results
                .len(),
            1
        );
        assert!(user.user_input_message.content.contains("orphan result"));
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
        assert!(body.contains(EMPTY_TOOL_RESULT_CONTENT_PLACEHOLDER));
        let Message::User(user) = &request.conversation_state.history[2] else {
            panic!("expected user");
        };
        assert_eq!(
            tool_result_text(
                &user
                    .user_input_message
                    .user_input_message_context
                    .tool_results[0]
            ),
            EMPTY_TOOL_RESULT_CONTENT_PLACEHOLDER
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

        let Message::Assistant(assistant) = &request.conversation_state.history[3] else {
            panic!("expected second assistant");
        };
        let renamed_id = assistant
            .assistant_response_message
            .tool_uses
            .as_ref()
            .expect("tool use")
            .first()
            .expect("first tool use")
            .tool_use_id
            .clone();
        assert_ne!(renamed_id, "tool-1");
        let Message::User(user) = &request.conversation_state.history[4] else {
            panic!("expected second result user");
        };
        assert_eq!(
            user.user_input_message
                .user_input_message_context
                .tool_results[0]
                .tool_use_id,
            renamed_id
        );
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
    fn guard_textifies_duplicate_current_tool_results() {
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
        assert_eq!(report.textified_duplicate_tool_results, 1);
        let current = &request
            .conversation_state
            .current_message
            .user_input_message;
        assert_eq!(current.user_input_message_context.tool_results.len(), 2);
        assert!(current.content.contains("duplicate result"));
    }

    #[test]
    fn guard_marks_oversized_without_rejecting_current_message() {
        let mut request = KiroRequest {
            conversation_state: ConversationState::new("conv-test").with_current_message(
                CurrentMessage::new(UserInputMessage::new("x".repeat(10_000), TEST_MODEL)),
            ),
            profile_arn: None,
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
    fn warning_header_fragment_reports_changes() {
        let report = PayloadGuardReport {
            enabled: true,
            max_bytes: 1000,
            original_bytes: 2000,
            final_bytes: 900,
            original_history_entries: 4,
            final_history_entries: 2,
            trimmed_history_entries: 2,
            aligned_leading_entries: 1,
            removed_empty_tool_uses: 1,
            removed_duplicate_tool_uses: 1,
            renamed_duplicate_tool_uses: 1,
            removed_orphan_tool_results: 1,
            removed_duplicate_tool_results: 1,
            textified_duplicate_tool_results: 1,
            textified_orphan_tool_results: 1,
            removed_orphan_tool_uses: 1,
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
    fn guard_strips_empty_tool_uses() {
        let assistant = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage {
                content: "empty tools".to_string(),
                tool_uses: Some(Vec::new()),
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
        let Message::User(user) = &request.conversation_state.history[2] else {
            panic!("expected historical user");
        };
        let historical_text = tool_result_text(
            &user
                .user_input_message
                .user_input_message_context
                .tool_results[0],
        );
        assert!(historical_text.chars().count() <= 1_000);
        assert!(historical_text.contains("historical tool result truncated by proxy"));

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

        assert!(body.len() <= 6_000);
        assert!(!report.still_oversized);
        assert!(report.truncated_current_tool_results > 0);
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
        assert!(body.len() <= 6_000);
        assert!(!report.still_oversized);
        assert!(report.truncated_current_documents > 0);
        assert_eq!(report.truncated_current_user_content, 0);
        assert!(content.contains("<document media_type=\"application/pdf\">"));
        assert!(content.contains("</document>"));
        assert!(content.contains("current document truncated by proxy"));
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
                .contains("current image omitted by proxy")
        );
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
        let Message::User(user) = &request.conversation_state.history[2] else {
            panic!("expected historical user");
        };
        let text = tool_result_text(
            &user
                .user_input_message
                .user_input_message_context
                .tool_results[0],
        );
        assert!(text.contains("Proxy note: web page navigation"));
        assert!(text.chars().count() > 1_000);
        assert!(text.chars().count() < 4_000);
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
        let Message::User(user) = &request.conversation_state.history[2] else {
            panic!("expected historical user");
        };
        let text = tool_result_text(
            &user
                .user_input_message
                .user_input_message_context
                .tool_results[0],
        );
        assert!(text.chars().count() <= 1_000);
    }

    #[test]
    fn payload_shaping_compresses_current_tool_definitions_without_removing_tools() {
        let mut request = request_with_history(Vec::new());
        request
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context = UserInputMessageContext::new().with_tools(vec![Tool {
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

        assert!(report.compressed_tool_definitions > 0);
        assert!(report.compressed_tool_definition_bytes > 0);
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

    #[test]
    fn payload_shaping_tool_definition_budget_zero_disables_tool_compression() {
        let mut request = request_with_history(Vec::new());
        let description = "description ".repeat(1_000);
        request
            .conversation_state
            .current_message
            .user_input_message
            .user_input_message_context = UserInputMessageContext::new().with_tools(vec![Tool {
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

        assert_eq!(report.compressed_tool_definitions, 0);
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

    #[test]
    fn payload_breakdown_reports_current_tool_and_history_sizes() {
        let assistant = HistoryAssistantMessage {
            assistant_response_message: AssistantMessage::new("tool call")
                .with_tool_uses(vec![ToolUseEntry::new("tool-1", "readFile")]),
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
        assert!(breakdown.current_tools_bytes > 0);
        assert!(breakdown.current_tool_results_bytes > 0);
        assert!(breakdown.history_tool_results_bytes > 0);
        assert!(breakdown.largest_tool_bytes > 0);
        assert!(breakdown.largest_history_tool_result_bytes > 0);
        assert!(breakdown.largest_current_tool_result_bytes > 0);
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
}
