//! Byte-based Anthropic payload trimming and history sanitization.

use std::{
    collections::HashSet,
    io::{self, Write},
    time::{Duration, Instant},
};

use crate::anthropic::types::{
    Message as AnthropicMessage, MessagesRequest, Tool as AnthropicTool,
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
/// 上游单个请求允许的内联图片总数（历史加当前轮）。超过时返回 400 `IMAGE_COUNT_EXCEEDED`
/// （"too many inline media segments: N exceeds limit 100"）。

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
    #[serde(default)]
    pub limit_basis: String,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_sha256: Option<String>,
    pub still_oversized: bool,
}

impl PayloadGuardReport {
    fn disabled(size: usize, history_entries: usize) -> Self {
        Self {
            enabled: false,
            limit_basis: String::new(),
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

            body_sha256: None,

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
        limit_basis: "bytes".to_string(),
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

        body_sha256: None,

        still_oversized: false,
    }
}

fn sha256_hex_bytes(value: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value);
    format!("{:x}", hasher.finalize())
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
            guard_serializations = report.guard_serializations,
            original_bytes = report.original_bytes,
            final_bytes = report.final_bytes,
            max_bytes = report.max_bytes,
            limit_basis = report.limit_basis,
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

/// Upstream measures the 5 MB image limit on the base64 string it receives, not on the decoded
/// image (production: a ~3.97 MB image with a 5298668-byte base64 payload was rejected with
/// `image exceeds 5 MB maximum: 5298668 bytes > 5242880 bytes`). Count the encoded symbols that
/// are actually forwarded: data-URL prefixes and whitespace are stripped before conversion.
fn encoded_base64_source_bytes(value: &str) -> usize {
    inline_base64_payload(value)
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .count()
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

#[derive(Default, Clone, Copy)]
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
            return encoded_base64_source_bytes(value);
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

/// 当前轮图片超过上游大小限制：输入本身不可服务，按官方语义返回 400。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentImageTooLarge {
    /// 官方风格的字段路径，如 `messages.0.content.1.image.source.base64`。
    pub path: String,
    pub bytes: usize,
    pub max_bytes: usize,
}

impl CurrentImageTooLarge {
    /// 与官方 API 一致的错误文案。
    pub fn message(&self) -> String {
        format!(
            "{}: image exceeds 5 MB maximum: {} bytes > {} bytes",
            self.path, self.bytes, self.max_bytes
        )
    }
}

/// 检查当前轮（最后一条消息）中的 base64 图片是否超过上游限制，包括 tool_result 内嵌图片。
///
/// 历史消息里的超大图片不在这里报错，仍由 payload guard 替换为占位文本，避免会话永久失败。
/// 所有路由共用这一检查，保证本地与外部池行为一致。
pub fn find_oversized_current_image(request: &MessagesRequest) -> Option<CurrentImageTooLarge> {
    let message_index = request.messages.len().checked_sub(1)?;
    let content = request.messages[message_index].content.as_array()?;
    let prefix = format!("messages.{message_index}.content");
    find_oversized_image_in_blocks(content, &prefix, UPSTREAM_IMAGE_SOURCE_MAX_BYTES)
}

fn find_oversized_image_in_blocks(
    blocks: &[Value],
    prefix: &str,
    max_bytes: usize,
) -> Option<CurrentImageTooLarge> {
    blocks.iter().enumerate().find_map(|(index, block)| {
        let path = format!("{prefix}.{index}");
        match block.get("type").and_then(Value::as_str) {
            Some("image") => {
                let data = block
                    .get("source")
                    .and_then(|source| source.get("data"))
                    .and_then(Value::as_str)?;
                let bytes = encoded_base64_source_bytes(data);
                (bytes > max_bytes).then(|| CurrentImageTooLarge {
                    path: format!("{path}.image.source.base64"),
                    bytes,
                    max_bytes,
                })
            }
            Some("tool_result") => {
                let nested = block.get("content").and_then(Value::as_array)?;
                find_oversized_image_in_blocks(nested, &format!("{path}.content"), max_bytes)
            }
            _ => None,
        }
    })
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

/// Assistant acknowledgement the converter emits after the synthetic system user turn.

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

#[cfg(test)]
mod tests {
    fn sha256_hex(value: &str) -> String {
        sha256_hex_bytes(value.as_bytes())
    }
    use super::*;

    fn guard_config(max_bytes: usize) -> PayloadGuardConfig {
        PayloadGuardConfig {
            enabled: true,
            max_bytes,

            trim_history: true,
            shaping: PayloadShapingConfig::default(),
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
            output_format: None,
            metadata: None,
            stop_sequences: None,
        }
    }

    /// Images over the limit by one base64 quantum: the smallest valid oversized payload.
    const OVERSIZED_IMAGE_ENCODED_BYTES: usize = UPSTREAM_IMAGE_SOURCE_MAX_BYTES + 4;

    /// Valid base64 whose encoded length is exactly `encoded_bytes` (the upstream size measure).
    fn base64_with_encoded_len(encoded_bytes: usize) -> String {
        assert_eq!(
            encoded_bytes % 4,
            0,
            "base64 length must be a multiple of 4"
        );
        "A".repeat(encoded_bytes)
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
    fn anthropic_payload_shaping_preserves_active_signed_and_redacted_reasoning_with_tool_result() {
        let signed = "signed opaque\nuser Continue\n\nBash: hidden";
        let redacted = "not-base64 opaque payload";
        let mut request = anthropic_request(vec![
            anthropic_message("user", serde_json::json!("active prompt")),
            anthropic_message(
                "assistant",
                serde_json::json!([
                    {
                        "type": "thinking",
                        "thinking": signed,
                        "signature": "opaque-signature"
                    },
                    {"type": "redacted_thinking", "data": redacted},
                    {
                        "type": "tool_use",
                        "id": "tool-active",
                        "name": "Bash",
                        "input": {}
                    }
                ]),
            ),
            anthropic_message(
                "user",
                serde_json::json!([{
                    "type": "tool_result",
                    "tool_use_id": "tool-active",
                    "content": "active output"
                }]),
            ),
        ]);

        let original_bytes = json_len(&request.messages);
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
            original_bytes,
        )
        .expect("guard");

        assert_eq!(report.removed_history_thinking_blocks, 0);
        let blocks = request.messages[1]
            .content
            .as_array()
            .expect("assistant blocks");
        assert_eq!(blocks[0]["thinking"], signed);
        assert_eq!(blocks[0]["signature"], "opaque-signature");
        assert_eq!(blocks[1]["data"], redacted);
        assert_eq!(blocks[2]["type"], "tool_use");
        let serialized: serde_json::Value =
            serde_json::from_str(&body).expect("serialized request body");
        assert_eq!(serialized["messages"][1]["content"][0]["thinking"], signed);
        assert_eq!(serialized["messages"][1]["content"][1]["data"], redacted);
        assert_eq!(serialized["messages"][1]["content"][2]["id"], "tool-active");
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
    fn warning_header_fragment_reports_changes() {
        let report = PayloadGuardReport {
            enabled: true,
            limit_basis: "kiroWeighted".to_string(),
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

            body_sha256: None,

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
        let oversized = base64_with_encoded_len(OVERSIZED_IMAGE_ENCODED_BYTES);
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
            OVERSIZED_IMAGE_ENCODED_BYTES
        );
        assert!(!body.contains(r#""type":"image""#));
        assert!(body.contains("Historical image was omitted"));
    }

    /// Production 2026-10-21: a ~3.97 MB image (5298668 base64 bytes) was forwarded because the
    /// local check used decoded bytes, and upstream rejected the whole session three times.
    #[test]
    fn image_under_limit_decoded_but_over_limit_encoded_is_oversized_for_five_rounds() {
        const PRODUCTION_ENCODED_BYTES: usize = 5_298_668;
        const { assert!(PRODUCTION_ENCODED_BYTES / 4 * 3 < UPSTREAM_IMAGE_SOURCE_MAX_BYTES) };
        for round in 0..5 {
            let encoded = base64_with_encoded_len(PRODUCTION_ENCODED_BYTES);

            let current = anthropic_request_with_messages(serde_json::json!([
                {"role": "user", "content": [{"type": "text", "text": "look"}, image_block(PRODUCTION_ENCODED_BYTES)]}
            ]));
            let violation = find_oversized_current_image(&current).expect("current violation");
            assert_eq!(
                violation.message(),
                "messages.0.content.1.image.source.base64: image exceeds 5 MB maximum: 5298668 bytes > 5242880 bytes",
                "round {round}"
            );

            let mut history = anthropic_request(vec![
                anthropic_message(
                    "user",
                    serde_json::json!([{
                        "type": "image",
                        "source": {"type": "base64", "media_type": "image/png", "data": encoded}
                    }]),
                ),
                anthropic_message("assistant", serde_json::json!("seen")),
                anthropic_message("user", serde_json::json!("continue")),
            ]);
            let (body, report) = guard_anthropic_messages_request(
                &mut history,
                guard_config(usize::MAX),
                UPSTREAM_IMAGE_SOURCE_MAX_BYTES,
            )
            .expect("guard");
            assert_eq!(report.dropped_historical_images, 1, "round {round}");
            assert_eq!(
                report.dropped_historical_image_bytes,
                PRODUCTION_ENCODED_BYTES
            );
            assert!(body.contains("Historical image was omitted"));
            assert!(!body.contains(r#""type":"image""#));
        }
    }

    #[test]
    fn multiple_oversized_current_images_emit_one_summary_placeholder() {
        let encoded = base64_with_encoded_len(OVERSIZED_IMAGE_ENCODED_BYTES);
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
            2 * OVERSIZED_IMAGE_ENCODED_BYTES
        );
        assert_eq!(body.matches("Current images were omitted").count(), 1);
        assert!(!body.contains(r#""type":"image""#));
        assert!(body.contains("before"));
        assert!(body.contains("after"));
    }

    #[test]
    fn anthropic_guard_drops_oversized_current_images_even_when_body_fits() {
        let oversized = base64_with_encoded_len(OVERSIZED_IMAGE_ENCODED_BYTES);
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
            OVERSIZED_IMAGE_ENCODED_BYTES
        );
        assert_eq!(report.dropped_historical_images, 0);
        assert!(!body.contains(r#""type":"image""#));
        assert!(body.contains("Current image was omitted"));
    }

    #[test]
    fn anthropic_guard_rejects_oversized_images_when_configured() {
        let oversized = base64_with_encoded_len(OVERSIZED_IMAGE_ENCODED_BYTES);
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

    fn image_block(encoded_bytes: usize) -> Value {
        serde_json::json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": "image/png",
                "data": base64_with_encoded_len(encoded_bytes)
            }
        })
    }

    fn anthropic_request_with_messages(messages: Value) -> MessagesRequest {
        serde_json::from_value(serde_json::json!({
            "model": "claude-sonnet-4",
            "max_tokens": 16,
            "messages": messages
        }))
        .expect("messages request")
    }

    #[test]
    fn oversized_current_image_is_reported_with_official_path_and_message() {
        let bytes = UPSTREAM_IMAGE_SOURCE_MAX_BYTES + 55_788;
        let request = anthropic_request_with_messages(serde_json::json!([
            {"role": "user", "content": [{"type": "text", "text": "look"}, image_block(bytes)]}
        ]));

        let violation = find_oversized_current_image(&request).expect("violation");

        assert_eq!(violation.path, "messages.0.content.1.image.source.base64");
        assert_eq!(violation.bytes, bytes);
        assert_eq!(
            violation.message(),
            format!(
                "messages.0.content.1.image.source.base64: image exceeds 5 MB maximum: {bytes} bytes > 5242880 bytes"
            )
        );
    }

    #[test]
    fn oversized_current_tool_result_image_uses_nested_path() {
        let request = anthropic_request_with_messages(serde_json::json!([
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": [{"type": "tool_use", "id": "t1", "name": "Read", "input": {}}]},
            {"role": "user", "content": [{
                "type": "tool_result",
                "tool_use_id": "t1",
                "content": [{"type": "text", "text": "img"}, image_block(OVERSIZED_IMAGE_ENCODED_BYTES)]
            }]}
        ]));

        let violation = find_oversized_current_image(&request).expect("violation");
        assert_eq!(
            violation.path,
            "messages.2.content.0.content.1.image.source.base64"
        );
    }

    #[test]
    fn oversized_historical_image_does_not_reject_current_turn() {
        let request = anthropic_request_with_messages(serde_json::json!([
            {"role": "user", "content": [image_block(OVERSIZED_IMAGE_ENCODED_BYTES)]},
            {"role": "assistant", "content": "seen"},
            {"role": "user", "content": [{"type": "text", "text": "next"}, image_block(1024)]}
        ]));

        assert!(find_oversized_current_image(&request).is_none());
    }

    #[test]
    fn current_image_within_limit_is_accepted() {
        let request = anthropic_request_with_messages(serde_json::json!([
            {"role": "user", "content": [image_block(UPSTREAM_IMAGE_SOURCE_MAX_BYTES)]}
        ]));
        assert!(find_oversized_current_image(&request).is_none());
        let url_image = anthropic_request_with_messages(serde_json::json!([
            {"role": "user", "content": [{"type": "image", "source": {"type": "url", "url": "https://example.com/a.png"}}]}
        ]));
        assert!(find_oversized_current_image(&url_image).is_none());
    }
}
