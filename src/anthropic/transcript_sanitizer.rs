//! Filters internal tool transcript scaffolding accidentally emitted as assistant text.
//!
//! The filter deliberately requires a complete, project-specific transcript signature. Single
//! words such as `Continue`, `user`, or `Hash` remain ordinary visible text.

use std::collections::HashSet;

use bytes::Bytes;
use serde::Deserialize;

use super::converter::{deterministic_mapped_tool_name, legacy_overlong_mapped_tool_name};
use super::request_facts::RawMessagesBodyProbe;
#[cfg(test)]
use super::request_facts::probe_raw_messages_body;
use super::types::{Message as AnthropicMessage, MessagesRequest};

pub(crate) const RESPONSE_PROTOCOL_CONTAMINATION_DETAIL: &str =
    "upstream assistant response contained suppressed internal protocol transcript";

const MAX_CANDIDATE_BYTES: usize = 4096;
const NEW_ROLE_LINES: &[&str] = &["user Continue"];
const OLD_ROLE_LINES: &[&str] = &["user Tool results provided.", "user Tool results provided"];
const ROLELESS_RESULT_LINES: &[&str] = &["Tool results:"];
const RAW_PREFILTER_MARKERS: &[&[u8]] = &[
    b"user Continue",
    b"user Tool results provided.",
    b"user Tool results provided",
    b"Tool results:",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum TranscriptLeakKind {
    ContinueToolResult,
    LegacyToolResults,
    RolelessToolResults,
}

impl TranscriptLeakKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ContinueToolResult => "continue_tool_result",
            Self::LegacyToolResults => "legacy_tool_results",
            Self::RolelessToolResults => "roleless_tool_results",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct MarkdownFence {
    marker: char,
    len: usize,
}

#[derive(Debug)]
struct Candidate {
    kind: TranscriptLeakKind,
    buffer: String,
}

#[derive(Debug, Default)]
enum State {
    #[default]
    Scan,
    Candidate(Candidate),
    DropUntilBoundary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CandidateStatus {
    Possible,
    Confirmed,
    Rejected,
}

/// Incremental sanitizer shared by streaming, non-streaming, and history conversion paths.
#[derive(Debug, Default)]
pub(crate) struct ToolTranscriptSanitizer {
    known_tool_names: HashSet<String>,
    state: State,
    line_probe: String,
    at_line_start: bool,
    fence: Option<MarkdownFence>,
    suppressed_blocks: u32,
    suppressed_chars: usize,
    matched_kinds: HashSet<TranscriptLeakKind>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct AssistantHistorySanitization {
    pub(crate) messages: u32,
    pub(crate) blocks: u32,
    pub(crate) chars: usize,
    pub(crate) kinds: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RawAssistantHistorySanitizationError {
    InvalidJson,
    Serialize,
}

impl std::fmt::Display for RawAssistantHistorySanitizationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidJson => formatter.write_str(
                "assistant history could not be inspected because the JSON body is invalid",
            ),
            Self::Serialize => formatter.write_str(
                "assistant history was inspected but the sanitized JSON body could not be serialized",
            ),
        }
    }
}

/// Sanitizes assistant text in a normalized Anthropic request payload.
///
/// User text, tool results, and tool inputs are intentionally untouched. Unsigned assistant
/// thinking is sanitized as thinking, while signed or redacted thinking remains opaque and
/// byte-for-byte stable so its integrity metadata can never be paired with rewritten data.
pub(crate) fn sanitize_messages_request_assistant_history(
    request: &mut MessagesRequest,
) -> AssistantHistorySanitization {
    let known_tool_names = collect_known_tool_names_from_request(request);
    sanitize_assistant_message_runs(&mut request.messages, known_tool_names)
}

pub(crate) fn collect_known_tool_names_from_request(request: &MessagesRequest) -> Vec<String> {
    let mut known_tool_names = request
        .tools
        .iter()
        .flatten()
        .map(|tool| tool.name.clone())
        .collect::<Vec<_>>();
    known_tool_names.extend(request.messages.iter().flat_map(|message| {
        message
            .content
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|block| {
                (block.get("type").and_then(|value| value.as_str()) == Some("tool_use"))
                    .then(|| block.get("name").and_then(|value| value.as_str()))
                    .flatten()
                    .map(str::to_string)
            })
    }));
    known_tool_names
}

/// Sanitizes assistant history in a raw Anthropic request without discarding unmodeled fields.
///
/// Raw external-pool routes run before `MessagesRequest` deserialization. Parsing into the typed
/// request and serializing it again would silently drop fields added by newer Anthropic clients,
/// so this path mutates only each message's `content` inside a generic JSON value. Clean payloads
/// return `None` and retain their original bytes exactly.
#[cfg(test)]
pub(crate) fn sanitize_raw_request_assistant_history(
    raw_body: &[u8],
) -> Result<Option<(Vec<u8>, AssistantHistorySanitization)>, RawAssistantHistorySanitizationError> {
    let raw_body = Bytes::copy_from_slice(raw_body);
    let probe = probe_raw_messages_body(&raw_body);
    sanitize_raw_request_assistant_history_with_probe(&raw_body, &probe)
}

pub(crate) fn sanitize_raw_request_assistant_history_with_probe(
    raw_body: &Bytes,
    probe: &RawMessagesBodyProbe,
) -> Result<Option<(Vec<u8>, AssistantHistorySanitization)>, RawAssistantHistorySanitizationError> {
    if !probe.matches_body(raw_body) || probe.scan_error().is_some() {
        return Err(RawAssistantHistorySanitizationError::InvalidJson);
    }
    if !raw_body_may_contain_transcript_marker(raw_body) {
        return Ok(None);
    }
    let mut deserializer = serde_json::Deserializer::from_slice(raw_body);
    deserializer.disable_recursion_limit();
    let mut value = serde_json::Value::deserialize(&mut deserializer)
        .map_err(|_| RawAssistantHistorySanitizationError::InvalidJson)?;
    deserializer
        .end()
        .map_err(|_| RawAssistantHistorySanitizationError::InvalidJson)?;
    let known_tool_names = collect_known_tool_names_from_value(&value);
    let Some(messages) = value
        .get_mut("messages")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return Ok(None);
    };
    let mut projected = messages
        .iter()
        .map(|message| AnthropicMessage {
            role: message
                .get("role")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            content: message
                .get("content")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        })
        .collect::<Vec<_>>();

    let report = sanitize_assistant_message_runs(&mut projected, known_tool_names);
    if report.blocks == 0 {
        return Ok(None);
    }

    for (message, sanitized) in messages.iter_mut().zip(projected) {
        if message.get("role").and_then(serde_json::Value::as_str) == Some("assistant") {
            message["content"] = sanitized.content;
        }
    }
    serde_json::to_vec(&value)
        .map(|body| Some((body, report)))
        .map_err(|_| RawAssistantHistorySanitizationError::Serialize)
}

fn raw_body_may_contain_transcript_marker(raw_body: &[u8]) -> bool {
    let has_literal_marker = raw_body.iter().enumerate().any(|(offset, first)| {
        (*first == b'u' || *first == b'T')
            && RAW_PREFILTER_MARKERS
                .iter()
                .filter(|marker| marker.first() == Some(first))
                .any(|marker| raw_body[offset..].starts_with(marker))
    });
    has_literal_marker || escaped_json_may_contain_transcript_marker(raw_body)
}

fn escaped_json_may_contain_transcript_marker(raw_body: &[u8]) -> bool {
    if !raw_body.contains(&b'\\') {
        return false;
    }

    let mut matched = [0usize; RAW_PREFILTER_MARKERS.len()];
    let mut offset = 0usize;
    while offset < raw_body.len() {
        let (decoded, consumed) = decode_ascii_json_escape(&raw_body[offset..]);
        offset += consumed;
        let Some(decoded) = decoded else {
            matched.fill(0);
            continue;
        };

        for (marker, matched_bytes) in RAW_PREFILTER_MARKERS.iter().zip(&mut matched) {
            if decoded == marker[*matched_bytes] {
                *matched_bytes += 1;
                if *matched_bytes == marker.len() {
                    return true;
                }
            } else {
                *matched_bytes = usize::from(decoded == marker[0]);
            }
        }
    }
    false
}

fn decode_ascii_json_escape(input: &[u8]) -> (Option<u8>, usize) {
    let Some(&first) = input.first() else {
        return (None, 0);
    };
    if first != b'\\' {
        return (first.is_ascii().then_some(first), 1);
    }
    let Some(&escaped) = input.get(1) else {
        return (None, 1);
    };
    let simple = match escaped {
        b'"' | b'\\' | b'/' => Some(escaped),
        b'b' => Some(0x08),
        b'f' => Some(0x0c),
        b'n' => Some(b'\n'),
        b'r' => Some(b'\r'),
        b't' => Some(b'\t'),
        _ => None,
    };
    if simple.is_some() {
        return (simple, 2);
    }
    if escaped != b'u' || input.len() < 6 {
        return (None, 2);
    }

    let mut codepoint = 0u16;
    for byte in &input[2..6] {
        let Some(nibble) = hex_nibble(*byte) else {
            return (None, 6);
        };
        codepoint = (codepoint << 4) | u16::from(nibble);
    }
    (u8::try_from(codepoint).ok().filter(u8::is_ascii), 6)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub(crate) fn collect_known_tool_names_from_value(value: &serde_json::Value) -> Vec<String> {
    let mut names = value
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tool| tool.get("name").and_then(serde_json::Value::as_str))
        .map(str::to_string)
        .collect::<Vec<_>>();
    names.extend(
        value
            .get("messages")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .flat_map(|message| {
                message
                    .get("content")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
            })
            .filter_map(|block| {
                (block.get("type").and_then(serde_json::Value::as_str) == Some("tool_use"))
                    .then(|| block.get("name").and_then(serde_json::Value::as_str))
                    .flatten()
            })
            .map(str::to_string),
    );
    names
}

/// Sanitizes text blocks in a complete Anthropic Messages response value.
///
/// Tool calls, usage, and unknown response fields are preserved. Thinking follows the same
/// unsigned-versus-atomic signed/redacted policy as request history.
pub(crate) fn sanitize_response_content(
    response: &mut serde_json::Value,
    known_tool_names: impl IntoIterator<Item = String>,
) -> AssistantHistorySanitization {
    let Some(original) = response.get("content").cloned() else {
        return AssistantHistorySanitization::default();
    };
    let mut message = AnthropicMessage {
        role: "assistant".to_string(),
        content: original,
    };
    let report = sanitize_assistant_message_runs_with_policy(
        std::slice::from_mut(&mut message),
        known_tool_names.into_iter().collect::<Vec<_>>(),
        true,
        true,
    );
    if report.blocks > 0 {
        response["content"] = message.content;
    }
    report
}

pub(crate) fn sanitize_assistant_message_runs(
    messages: &mut [AnthropicMessage],
    known_tool_names: impl IntoIterator<Item = String> + Clone,
) -> AssistantHistorySanitization {
    sanitize_assistant_message_runs_with_policy(messages, known_tool_names, true, false)
}

fn sanitize_assistant_message_runs_with_policy(
    messages: &mut [AnthropicMessage],
    known_tool_names: impl IntoIterator<Item = String> + Clone,
    preserve_authenticated_thinking: bool,
    inspect_authenticated_thinking: bool,
) -> AssistantHistorySanitization {
    let mut report = AssistantHistorySanitization::default();
    let mut start = 0usize;
    while start < messages.len() {
        if messages[start].role != "assistant" {
            start += 1;
            continue;
        }
        let mut end = start + 1;
        while end < messages.len() && messages[end].role == "assistant" {
            end += 1;
        }
        merge_sanitization_report(
            &mut report,
            sanitize_assistant_run(
                &mut messages[start..end],
                known_tool_names.clone(),
                preserve_authenticated_thinking,
                inspect_authenticated_thinking,
            ),
        );
        start = end;
    }
    report
}

#[derive(Debug, Clone, Copy)]
enum TextLocation {
    String(usize),
    Block(usize, usize),
}

fn sanitize_assistant_run(
    messages: &mut [AnthropicMessage],
    known_tool_names: impl IntoIterator<Item = String>,
    preserve_authenticated_thinking: bool,
    inspect_authenticated_thinking: bool,
) -> AssistantHistorySanitization {
    let known_tool_names = known_tool_names.into_iter().collect::<Vec<_>>();
    let originals = messages
        .iter()
        .map(|message| message.content.clone())
        .collect::<Vec<_>>();
    let mut sanitizer = ToolTranscriptSanitizer::new(known_tool_names.iter().cloned());
    let mut thinking_sanitizer = ToolTranscriptSanitizer::new(known_tool_names.iter().cloned());
    let mut removed_blocks = HashSet::new();
    let mut last_text = None;
    let mut released_text = Vec::new();

    for (message_idx, message) in messages.iter_mut().enumerate() {
        match &mut message.content {
            serde_json::Value::String(text) => {
                *text = sanitizer.push(text);
                last_text = Some(TextLocation::String(message_idx));
            }
            serde_json::Value::Array(blocks) => {
                for (block_idx, block) in blocks.iter_mut().enumerate() {
                    let block_type = block
                        .get("type")
                        .and_then(|value| value.as_str())
                        .map(str::to_string);
                    if block_type.as_deref() == Some("text") {
                        let Some(text) = block
                            .get("text")
                            .and_then(|value| value.as_str())
                            .map(str::to_string)
                        else {
                            continue;
                        };
                        block["text"] = serde_json::Value::String(sanitizer.push(&text));
                        last_text = Some(TextLocation::Block(message_idx, block_idx));
                    } else if matches!(
                        block_type.as_deref(),
                        Some("thinking" | "redacted_thinking")
                    ) {
                        let pending = sanitizer.structured_tool_boundary();
                        if !pending.is_empty() {
                            released_text.push((last_text, pending));
                        }
                        // Signed thinking and redacted thinking are upstream-authenticated
                        // opaque values. Never scan, rewrite, or remove their body or signature:
                        // changing either side can make the provider reject the whole history.
                        let (field, atomic) = if block_type.as_deref() == Some("thinking") {
                            (
                                "thinking",
                                block
                                    .get("signature")
                                    .and_then(|value| value.as_str())
                                    .is_some_and(|signature| !signature.is_empty()),
                            )
                        } else {
                            ("data", true)
                        };
                        if atomic && preserve_authenticated_thinking {
                            if inspect_authenticated_thinking {
                                // Complete upstream responses are rejected when authenticated
                                // thinking carries internal transcript scaffolding, but the
                                // opaque body must remain untouched while it is being inspected.
                                let _ = thinking_sanitizer.push(
                                    block
                                        .get(field)
                                        .and_then(|value| value.as_str())
                                        .unwrap_or_default(),
                                );
                                let _ = thinking_sanitizer.finish();
                            }
                            continue;
                        }
                        let Some(original) = block
                            .get(field)
                            .and_then(|value| value.as_str())
                            .map(str::to_string)
                        else {
                            continue;
                        };
                        let suppressed_before = thinking_sanitizer.suppressed_blocks();
                        let mut safe = thinking_sanitizer.push(&original);
                        safe.push_str(&thinking_sanitizer.finish());
                        let suppressed = thinking_sanitizer.suppressed_blocks() > suppressed_before;
                        if suppressed {
                            if safe.is_empty() {
                                removed_blocks.insert((message_idx, block_idx));
                            } else {
                                block[field] = serde_json::Value::String(safe);
                            }
                        }
                    } else if block_type.is_some() {
                        let pending = sanitizer.structured_tool_boundary();
                        if !pending.is_empty() {
                            released_text.push((last_text, pending));
                        }
                    }
                }
            }
            _ => {
                let pending = sanitizer.structured_tool_boundary();
                if !pending.is_empty() {
                    released_text.push((last_text, pending));
                }
            }
        }
    }
    let tail = sanitizer.finish();
    if !tail.is_empty() {
        released_text.push((last_text, tail));
    }
    for (location, text) in released_text {
        append_to_location(messages, location, &text);
    }

    let mut report = sanitization_report_for_complete_block(&sanitizer);
    merge_sanitization_report(
        &mut report,
        sanitization_report_for_complete_block(&thinking_sanitizer),
    );
    if report.blocks == 0 {
        for (message, original) in messages.iter_mut().zip(originals) {
            message.content = original;
        }
        return AssistantHistorySanitization::default();
    }

    for (message_idx, message) in messages.iter_mut().enumerate() {
        match &mut message.content {
            serde_json::Value::String(text) if text.is_empty() => *text = " ".to_string(),
            serde_json::Value::Array(blocks) => {
                let mut block_idx = 0usize;
                blocks.retain(|block| {
                    let remove = removed_blocks.contains(&(message_idx, block_idx));
                    block_idx += 1;
                    !remove
                        && (block.get("type").and_then(|value| value.as_str()) != Some("text")
                            || block
                                .get("text")
                                .and_then(|value| value.as_str())
                                .is_some_and(|text| !text.is_empty()))
                });
                if blocks.is_empty() {
                    blocks.push(serde_json::json!({"type": "text", "text": " "}));
                }
            }
            _ => {}
        }
    }
    let changed_messages = messages
        .iter()
        .zip(&originals)
        .filter(|(message, original)| message.content != **original)
        .count();

    report.messages = changed_messages.min(u32::MAX as usize) as u32;
    report
}

fn sanitization_report_for_complete_block(
    sanitizer: &ToolTranscriptSanitizer,
) -> AssistantHistorySanitization {
    AssistantHistorySanitization {
        messages: 0,
        blocks: sanitizer.suppressed_blocks(),
        chars: sanitizer.suppressed_chars(),
        kinds: sanitizer
            .matched_kinds()
            .into_iter()
            .map(str::to_string)
            .collect(),
    }
}

fn append_to_location(
    messages: &mut [AnthropicMessage],
    location: Option<TextLocation>,
    suffix: &str,
) {
    if suffix.is_empty() {
        return;
    }
    match location {
        Some(TextLocation::String(message_idx)) => {
            if let Some(text) = messages[message_idx].content.as_str() {
                messages[message_idx].content =
                    serde_json::Value::String(format!("{text}{suffix}"));
            }
        }
        Some(TextLocation::Block(message_idx, block_idx)) => {
            let blocks = messages[message_idx]
                .content
                .as_array_mut()
                .expect("text block location must remain an array");
            let existing = blocks[block_idx]["text"].as_str().unwrap_or_default();
            blocks[block_idx]["text"] = serde_json::Value::String(format!("{existing}{suffix}"));
        }
        None => {}
    }
}

fn merge_sanitization_report(
    target: &mut AssistantHistorySanitization,
    source: AssistantHistorySanitization,
) {
    target.messages = target.messages.saturating_add(source.messages);
    target.blocks = target.blocks.saturating_add(source.blocks);
    target.chars = target.chars.saturating_add(source.chars);
    let mut kinds = target.kinds.drain(..).collect::<HashSet<_>>();
    kinds.extend(source.kinds);
    target.kinds = kinds.into_iter().collect();
    target.kinds.sort_unstable();
}

impl ToolTranscriptSanitizer {
    pub(crate) fn new(known_tool_names: impl IntoIterator<Item = String>) -> Self {
        let known_tool_names = known_tool_names
            .into_iter()
            .flat_map(|name| {
                let mapped = deterministic_mapped_tool_name(&name);
                let legacy = legacy_overlong_mapped_tool_name(&name);
                [
                    Some(name.to_ascii_lowercase()),
                    Some(mapped.to_ascii_lowercase()),
                    legacy.map(|value| value.to_ascii_lowercase()),
                ]
            })
            .flatten()
            .filter(|name| !name.is_empty())
            .collect();
        Self {
            known_tool_names,
            state: State::Scan,
            line_probe: String::new(),
            at_line_start: true,
            fence: None,
            suppressed_blocks: 0,
            suppressed_chars: 0,
            matched_kinds: HashSet::new(),
        }
    }

    /// Accepts one upstream text chunk and returns only text already proven safe to expose.
    pub(crate) fn push(&mut self, chunk: &str) -> String {
        let mut output = String::new();
        self.process_text(chunk, &mut output);
        output
    }

    /// Ends the current assistant text segment at a trusted structured-tool boundary.
    ///
    /// An unconfirmed candidate is ordinary text and is released. A confirmed transcript remains
    /// suppressed, and scanning restarts for text emitted after the structured tool event.
    pub(crate) fn structured_tool_boundary(&mut self) -> String {
        self.finish_segment()
    }

    /// Flushes safe pending text at response EOF.
    pub(crate) fn finish(&mut self) -> String {
        self.finish_segment()
    }

    pub(crate) fn suppressed_blocks(&self) -> u32 {
        self.suppressed_blocks
    }

    pub(crate) fn suppressed_chars(&self) -> usize {
        self.suppressed_chars
    }

    pub(crate) fn matched_kinds(&self) -> Vec<&'static str> {
        let mut kinds = self
            .matched_kinds
            .iter()
            .map(|kind| kind.as_str())
            .collect::<Vec<_>>();
        kinds.sort_unstable();
        kinds
    }

    fn process_text(&mut self, text: &str, output: &mut String) {
        for ch in text.chars() {
            self.process_char(ch, output);
        }
    }

    fn process_char(&mut self, ch: char, output: &mut String) {
        let state = std::mem::take(&mut self.state);
        match state {
            State::Scan => {
                self.state = State::Scan;
                self.process_scan_char(ch, output);
            }
            State::DropUntilBoundary => {
                self.suppressed_chars = self.suppressed_chars.saturating_add(1);
                self.state = State::DropUntilBoundary;
            }
            State::Candidate(mut candidate) => {
                candidate.buffer.push(ch);
                match self.candidate_status(&candidate) {
                    CandidateStatus::Possible => self.state = State::Candidate(candidate),
                    CandidateStatus::Confirmed => {
                        self.record_suppressed_candidate(&candidate);
                        self.state = State::DropUntilBoundary;
                    }
                    CandidateStatus::Rejected => {
                        self.state = State::Scan;
                        self.release_candidate(candidate, output);
                    }
                }
            }
        }
    }

    fn process_scan_char(&mut self, ch: char, output: &mut String) {
        if !self.at_line_start {
            output.push(ch);
            if ch == '\n' {
                self.at_line_start = true;
            }
            return;
        }

        self.line_probe.push(ch);
        if ch == '\n' {
            self.finish_probe_line(output);
            return;
        }

        if self.fence.is_none() {
            if let Some(fence) = opening_fence_decided_before_eol(&self.line_probe) {
                self.fence = Some(fence);
                output.push_str(&self.line_probe);
                self.line_probe.clear();
                self.at_line_start = false;
                return;
            }
        }

        let role_possible = self.fence.is_none() && role_line_prefix_possible(&self.line_probe);
        if !role_possible && !fence_line_prefix_possible(&self.line_probe, self.fence) {
            output.push_str(&self.line_probe);
            self.line_probe.clear();
            self.at_line_start = false;
        }
    }

    fn finish_probe_line(&mut self, output: &mut String) {
        let line = self.line_probe.clone();
        let logical = line
            .strip_suffix('\n')
            .unwrap_or(&line)
            .strip_suffix('\r')
            .unwrap_or_else(|| line.strip_suffix('\n').unwrap_or(&line));
        let scaffold_line = logical.trim_end_matches([' ', '\t']);

        if self.fence.is_none() {
            let kind = if NEW_ROLE_LINES.contains(&scaffold_line) {
                Some(TranscriptLeakKind::ContinueToolResult)
            } else if OLD_ROLE_LINES.contains(&scaffold_line) {
                Some(TranscriptLeakKind::LegacyToolResults)
            } else if ROLELESS_RESULT_LINES.contains(&scaffold_line) {
                Some(TranscriptLeakKind::RolelessToolResults)
            } else {
                None
            };
            if let Some(kind) = kind {
                self.line_probe.clear();
                self.at_line_start = true;
                self.state = State::Candidate(Candidate { kind, buffer: line });
                return;
            }
        }

        if let Some(action) = fence_line_action(logical, self.fence) {
            self.fence = action;
        }
        output.push_str(&line);
        self.line_probe.clear();
        self.at_line_start = true;
    }

    fn candidate_status(&self, candidate: &Candidate) -> CandidateStatus {
        if candidate.buffer.len() > MAX_CANDIDATE_BYTES {
            return CandidateStatus::Rejected;
        }
        let Some(first_newline) = candidate.buffer.find('\n') else {
            return CandidateStatus::Possible;
        };
        let tail = &candidate.buffer[first_newline + 1..];
        match candidate.kind {
            TranscriptLeakKind::ContinueToolResult => self.new_candidate_status(tail),
            TranscriptLeakKind::LegacyToolResults => self.old_candidate_status(tail),
            TranscriptLeakKind::RolelessToolResults => self.roleless_candidate_status(tail),
        }
    }

    fn new_candidate_status(&self, tail: &str) -> CandidateStatus {
        let Some(rest) = consume_required_blank_lines(tail) else {
            return blank_prefix_status(tail);
        };
        self.colon_tool_header_status(rest)
    }

    fn old_candidate_status(&self, tail: &str) -> CandidateStatus {
        let Some(rest) = consume_required_blank_lines(tail) else {
            return blank_prefix_status(tail);
        };

        // Older payload-guard versions narrated a completed result directly as
        // `toolName: output`, without the later `Tool results:` / `[toolName]` wrapper.
        // Accept both shapes so sessions created before the placeholder change cannot replay it.
        let direct_status = self.colon_tool_header_status(rest);
        if direct_status != CandidateStatus::Rejected {
            return direct_status;
        }

        let (heading, rest) = match take_complete_line(rest) {
            Some(parts) => parts,
            None => {
                let partial = rest.strip_suffix('\r').unwrap_or(rest);
                return if scaffold_line_prefix_possible("Tool results:", partial) {
                    CandidateStatus::Possible
                } else {
                    CandidateStatus::Rejected
                };
            }
        };
        if heading.trim_end_matches([' ', '\t']) != "Tool results:" {
            return CandidateStatus::Rejected;
        }

        let Some(rest) = consume_required_blank_lines(rest) else {
            return blank_prefix_status(rest);
        };
        self.bracket_tool_header_status(rest)
    }

    fn roleless_candidate_status(&self, tail: &str) -> CandidateStatus {
        let Some(rest) = consume_required_blank_lines(tail) else {
            return blank_prefix_status(tail);
        };
        self.bracket_tool_header_status(rest)
    }

    fn colon_tool_header_status(&self, value: &str) -> CandidateStatus {
        let line_end = value.find('\n').unwrap_or(value.len());
        let line = value[..line_end]
            .strip_suffix('\r')
            .unwrap_or(&value[..line_end]);
        if let Some(colon) = line.find(':') {
            let raw_name = &line[..colon];
            let name = raw_name.strip_suffix(" error").unwrap_or(raw_name);
            return if self.is_internal_tool_name(name) {
                CandidateStatus::Confirmed
            } else {
                CandidateStatus::Rejected
            };
        }
        if line_end < value.len() || line.len() > 128 {
            CandidateStatus::Rejected
        } else {
            CandidateStatus::Possible
        }
    }

    fn bracket_tool_header_status(&self, value: &str) -> CandidateStatus {
        if value.is_empty() || value == "\r" {
            return CandidateStatus::Possible;
        }
        if !value.starts_with('[') {
            return CandidateStatus::Rejected;
        }
        let line_end = value.find('\n').unwrap_or(value.len());
        let line = &value[..line_end];
        if let Some(close) = line.find(']') {
            return if self.is_internal_tool_name(&line[1..close]) {
                CandidateStatus::Confirmed
            } else {
                CandidateStatus::Rejected
            };
        }
        if line_end < value.len() || line.len() > 128 {
            CandidateStatus::Rejected
        } else {
            CandidateStatus::Possible
        }
    }

    fn is_internal_tool_name(&self, name: &str) -> bool {
        self.known_tool_names.contains(&name.to_ascii_lowercase())
    }

    fn is_internal_tool_name_prefix(&self, name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        !name.is_empty()
            && self
                .known_tool_names
                .iter()
                .any(|known| known.starts_with(&name))
    }

    fn release_candidate(&mut self, candidate: Candidate, output: &mut String) {
        let Some(first_newline) = candidate.buffer.find('\n') else {
            output.push_str(&candidate.buffer);
            self.at_line_start = false;
            return;
        };
        let split = first_newline + 1;
        output.push_str(&candidate.buffer[..split]);
        self.at_line_start = true;
        self.line_probe.clear();
        self.process_text(&candidate.buffer[split..], output);
    }

    fn record_suppressed_candidate(&mut self, candidate: &Candidate) {
        self.suppressed_blocks = self.suppressed_blocks.saturating_add(1);
        self.suppressed_chars = self
            .suppressed_chars
            .saturating_add(candidate.buffer.chars().count());
        self.matched_kinds.insert(candidate.kind);
    }

    fn candidate_has_sensitive_tool_prefix(&self, candidate: &Candidate) -> bool {
        let Some(first_newline) = candidate.buffer.find('\n') else {
            return false;
        };
        let tail = &candidate.buffer[first_newline + 1..];
        match candidate.kind {
            TranscriptLeakKind::ContinueToolResult => {
                let Some(rest) = consume_required_blank_lines(tail) else {
                    return false;
                };
                let line_end = rest.find('\n').unwrap_or(rest.len());
                let raw_name = rest[..line_end]
                    .strip_suffix('\r')
                    .unwrap_or(&rest[..line_end]);
                let name = raw_name.strip_suffix(" error").unwrap_or(raw_name);
                !name.is_empty() && self.is_internal_tool_name_prefix(name)
            }
            TranscriptLeakKind::LegacyToolResults => {
                let Some(rest) = consume_required_blank_lines(tail) else {
                    return false;
                };

                let line_end = rest.find(['\r', '\n', ':']).unwrap_or(rest.len());
                let direct_name = rest[..line_end]
                    .strip_suffix(" error")
                    .unwrap_or(&rest[..line_end]);
                if !direct_name.is_empty() && self.is_internal_tool_name_prefix(direct_name) {
                    return true;
                }

                let Some((heading, rest)) = take_complete_line(rest) else {
                    return false;
                };
                if heading.trim_end_matches([' ', '\t']) != "Tool results:" {
                    return false;
                }
                let Some(rest) = consume_required_blank_lines(rest) else {
                    return false;
                };
                let Some(name) = rest.strip_prefix('[') else {
                    return false;
                };
                let line_end = name.find(['\r', '\n', ']']).unwrap_or(name.len());
                let name = &name[..line_end];
                !name.is_empty() && self.is_internal_tool_name_prefix(name)
            }
            TranscriptLeakKind::RolelessToolResults => {
                let Some(rest) = consume_required_blank_lines(tail) else {
                    return false;
                };
                let Some(name) = rest.strip_prefix('[') else {
                    return false;
                };
                let line_end = name.find(['\r', '\n', ']']).unwrap_or(name.len());
                let name = &name[..line_end];
                !name.is_empty() && self.is_internal_tool_name_prefix(name)
            }
        }
    }

    fn finish_segment(&mut self) -> String {
        let mut output = String::new();
        loop {
            match std::mem::take(&mut self.state) {
                State::Scan => {
                    output.push_str(&self.line_probe);
                    self.line_probe.clear();
                    break;
                }
                State::Candidate(candidate) => {
                    if self.candidate_has_sensitive_tool_prefix(&candidate) {
                        self.record_suppressed_candidate(&candidate);
                        break;
                    }
                    self.state = State::Scan;
                    self.release_candidate(candidate, &mut output);
                }
                State::DropUntilBoundary => break,
            }
        }

        self.state = State::Scan;
        self.line_probe.clear();
        self.at_line_start = true;
        self.fence = None;
        output
    }
}

fn take_complete_line(value: &str) -> Option<(&str, &str)> {
    let newline = value.find('\n')?;
    let line = value[..newline]
        .strip_suffix('\r')
        .unwrap_or(&value[..newline]);
    Some((line, &value[newline + 1..]))
}

fn consume_required_blank_lines(mut value: &str) -> Option<&str> {
    let mut consumed = false;
    while let Some((line, rest)) = take_complete_line(value) {
        if !line.bytes().all(|byte| byte == b' ' || byte == b'\t') {
            break;
        }
        consumed = true;
        value = rest;
    }
    consumed.then_some(value)
}

fn blank_prefix_status(value: &str) -> CandidateStatus {
    if value
        .bytes()
        .all(|byte| byte == b' ' || byte == b'\t' || byte == b'\r')
    {
        CandidateStatus::Possible
    } else {
        CandidateStatus::Rejected
    }
}

fn role_line_prefix_possible(probe: &str) -> bool {
    let probe = probe.strip_suffix('\r').unwrap_or(probe);
    NEW_ROLE_LINES
        .iter()
        .chain(OLD_ROLE_LINES)
        .chain(ROLELESS_RESULT_LINES)
        .any(|line| scaffold_line_prefix_possible(line, probe))
}

fn scaffold_line_prefix_possible(line: &str, probe: &str) -> bool {
    line.starts_with(probe)
        || probe
            .strip_prefix(line)
            .is_some_and(|suffix| suffix.bytes().all(|byte| byte == b' ' || byte == b'\t'))
}

fn leading_fence_parts(line: &str) -> Option<(char, usize, &str)> {
    let spaces = line.bytes().take_while(|byte| *byte == b' ').count();
    if spaces > 3 {
        return None;
    }
    let rest = &line[spaces..];
    let marker = rest.chars().next()?;
    if marker != '`' && marker != '~' {
        return None;
    }
    let len = rest.chars().take_while(|ch| *ch == marker).count();
    let suffix = &rest[len * marker.len_utf8()..];
    Some((marker, len, suffix))
}

fn fence_line_prefix_possible(probe: &str, fence: Option<MarkdownFence>) -> bool {
    let spaces = probe.bytes().take_while(|byte| *byte == b' ').count();
    if spaces == probe.len() {
        return spaces <= 3;
    }
    let Some((marker, len, suffix)) = leading_fence_parts(probe) else {
        return false;
    };
    match fence {
        None => len < 3 || suffix.is_empty(),
        Some(open) => {
            marker == open.marker
                && (len < open.len
                    || suffix
                        .chars()
                        .all(|ch| ch == ' ' || ch == '\t' || ch == '\r'))
        }
    }
}

fn opening_fence_decided_before_eol(probe: &str) -> Option<MarkdownFence> {
    let (marker, len, suffix) = leading_fence_parts(probe)?;
    (len >= 3 && !suffix.is_empty()).then_some(MarkdownFence { marker, len })
}

fn fence_line_action(line: &str, fence: Option<MarkdownFence>) -> Option<Option<MarkdownFence>> {
    let (marker, len, suffix) = leading_fence_parts(line)?;
    match fence {
        None if len >= 3 => Some(Some(MarkdownFence { marker, len })),
        Some(open)
            if marker == open.marker
                && len >= open.len
                && suffix.chars().all(|ch| ch == ' ' || ch == '\t') =>
        {
            Some(None)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sanitizer() -> ToolTranscriptSanitizer {
        ToolTranscriptSanitizer::new(["Bash".to_string(), "Read".to_string()])
    }

    fn sanitize(text: &str) -> (String, ToolTranscriptSanitizer) {
        let mut guard = sanitizer();
        let mut output = guard.push(text);
        output.push_str(&guard.finish());
        (output, guard)
    }

    #[test]
    fn suppresses_continue_transcript_and_preserves_prefix() {
        let input = "正常说明。\n\nuser Continue\n\nbashHashd1e9567d: command output\nsecret tail";
        let (output, guard) = sanitize(input);
        assert_eq!(output, "正常说明。\n\n");
        assert_eq!(guard.suppressed_blocks(), 1);
        assert!(guard.suppressed_chars() > "secret tail".chars().count());
        assert_eq!(guard.matched_kinds(), vec!["continue_tool_result"]);
    }

    #[test]
    fn suppresses_legacy_transcript_through_eof_even_after_close_tag() {
        let input = "Safe prefix\nuser Tool results provided.\n\nTool results:\n\n[readHash9b9a8d05] file contents\n</function_results>\nLet me continue";
        let (output, guard) = sanitize(input);
        assert_eq!(output, "Safe prefix\n");
        assert_eq!(guard.matched_kinds(), vec!["legacy_tool_results"]);
    }

    #[test]
    fn suppresses_legacy_placeholder_with_direct_colon_tool_result() {
        let input =
            "Safe prefix\nuser Tool results provided.\n\nreadHash9b9a8d05: hidden\nold tail";
        let (output, guard) = sanitize(input);
        assert_eq!(output, "Safe prefix\n");
        assert_eq!(guard.matched_kinds(), vec!["legacy_tool_results"]);

        let truncated = "user Tool results provided.\n\nreadHash9b9a8d05";
        let (output, guard) = sanitize(truncated);
        assert_eq!(output, "");
        assert_eq!(guard.suppressed_blocks(), 1);
    }

    #[test]
    fn suppresses_roleless_legacy_scaffold_but_not_isolated_markers() {
        let input = "Safe prefix\nTool results:\n \t\n[bashHashd1e9567d] hidden\nsecret";
        let (output, guard) = sanitize(input);
        assert_eq!(output, "Safe prefix\n");
        assert_eq!(guard.matched_kinds(), vec!["roleless_tool_results"]);

        for safe in [
            "Tool results:\nordinary prose",
            "[bashHashd1e9567d] discussed without a heading",
            "Tool results:\n[bashHashd1e9567d] no scaffold blank line",
        ] {
            assert_eq!(sanitize(safe).0, safe);
        }
    }

    #[test]
    fn arbitrary_chunking_matches_one_shot_output() {
        let input = "中文 prefix\nuser Continue\n\nbashHashd1e9567d: output\nnever visible";
        let expected = sanitize(input).0;
        for split in input
            .char_indices()
            .map(|(idx, _)| idx)
            .chain(std::iter::once(input.len()))
        {
            let mut guard = sanitizer();
            let mut output = guard.push(&input[..split]);
            output.push_str(&guard.push(&input[split..]));
            output.push_str(&guard.finish());
            assert_eq!(output, expected, "split at byte {split}");
        }

        let mut guard = sanitizer();
        let mut output = String::new();
        for ch in input.chars() {
            output.push_str(&guard.push(&ch.to_string()));
        }
        output.push_str(&guard.finish());
        assert_eq!(output, expected);
    }

    #[test]
    fn clean_and_polluted_fixtures_match_one_shot_across_1000_unique_partitions() {
        const REQUIRED_PARTITIONS: usize = 1_000;
        const MAX_PARTITION_ATTEMPTS: usize = 100_000;

        let fixtures = vec![
            (
                "clean-inline",
                "A normal discussion of user Continue and bashHashd1e9567d stays visible."
                    .to_string(),
                true,
            ),
            (
                "clean-long-unicode",
                "中文段落🙂 with code `user Continue` and ordinary prose.\n".repeat(256),
                true,
            ),
            (
                "clean-fenced",
                "```text\nuser Continue\n\nbashHashd1e9567d: example output\n```\nThe example remains visible."
                    .to_string(),
                true,
            ),
            (
                "clean-quoted",
                "> user Continue\n>\n> bashHashd1e9567d: quoted example\nOutside the quote."
                    .to_string(),
                true,
            ),
            (
                "clean-indented",
                "    user Continue\n\n    bashHashd1e9567d: indented example\nVisible tail."
                    .to_string(),
                true,
            ),
            (
                "polluted-continue",
                "Visible prefix.\nuser Continue\n\nbashHashd1e9567d: hidden output\nsecret tail"
                    .to_string(),
                false,
            ),
            (
                "polluted-legacy",
                "Visible prefix.\nuser Tool results provided.\n\nTool results:\n\n[readHash9b9a8d05] hidden output"
                    .to_string(),
                false,
            ),
        ];

        for (fixture_index, (name, input, clean)) in fixtures.into_iter().enumerate() {
            let (expected, one_shot) = sanitize(&input);
            if clean {
                assert_eq!(expected, input, "clean fixture {name}");
                assert_eq!(one_shot.suppressed_blocks(), 0, "clean fixture {name}");
            } else {
                assert!(one_shot.suppressed_blocks() > 0, "polluted fixture {name}");
            }

            let boundaries = input
                .char_indices()
                .map(|(index, _)| index)
                .chain(std::iter::once(input.len()))
                .collect::<Vec<_>>();
            let mut unique_partitions = std::collections::HashSet::new();

            for attempt in 0..MAX_PARTITION_ATTEMPTS {
                if unique_partitions.len() == REQUIRED_PARTITIONS {
                    break;
                }

                let mut state = (attempt as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15)
                    ^ (fixture_index as u64 + 1).wrapping_mul(0xbf58_476d_1ce4_e5b9);
                let mut boundary_index = 0usize;
                let mut partition = Vec::new();
                while boundary_index + 1 < boundaries.len() {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    let remaining = boundaries.len() - 1 - boundary_index;
                    let width = 1 + state as usize % remaining.min(17);
                    boundary_index += width;
                    partition.push(boundaries[boundary_index]);
                }

                if !unique_partitions.insert(partition.clone()) {
                    continue;
                }

                let mut guard = sanitizer();
                let mut output = String::new();
                let mut start = 0usize;
                for end in partition {
                    output.push_str(&guard.push(&input[start..end]));
                    start = end;
                }
                output.push_str(&guard.finish());
                assert_eq!(output, expected, "fixture={name} attempt={attempt}");
                if clean {
                    assert_eq!(
                        guard.suppressed_blocks(),
                        0,
                        "clean fixture={name} attempt={attempt}"
                    );
                }
            }

            assert_eq!(
                unique_partitions.len(),
                REQUIRED_PARTITIONS,
                "fixture {name} did not generate enough distinct chunk partitions"
            );
        }
    }

    #[test]
    fn structured_tool_boundary_resumes_visible_text() {
        let mut guard = sanitizer();
        let mut output = guard.push("before\nuser Continue\n\nbashHashd1e9567d: hidden");
        output.push_str(&guard.structured_tool_boundary());
        output.push_str(&guard.push("after tool"));
        output.push_str(&guard.finish());
        assert_eq!(output, "before\nafter tool");
    }

    #[test]
    fn unconfirmed_candidates_are_released_unchanged() {
        for input in [
            "user Continue",
            "user Continue\nnormal prose",
            "user Continue\n\nunknown_tool: visible",
            "user Continue\n\nbashHash1234567: visible",
            "user Continue\nbashHashd1e9567d: no blank line",
            "user Tool results provided.\n\nTool output follows",
        ] {
            assert_eq!(sanitize(input).0, input, "input={input:?}");
        }
    }

    #[test]
    fn sensitive_truncated_tool_headers_fail_closed_at_eof_and_tool_boundary() {
        for input in [
            "user Continue\n\nbashHashd1e9567d",
            "user Continue\n\nBash",
            "user Tool results provided.\n\nTool results:\n\n[readHash9b9a8d05",
        ] {
            let (output, guard) = sanitize(input);
            assert_eq!(output, "", "input={input:?}");
            assert_eq!(guard.suppressed_blocks(), 1);
        }

        let mut guard = sanitizer();
        let mut output = guard.push("prefix\nuser Continue\n\nbashHashd1e95");
        output.push_str(&guard.structured_tool_boundary());
        output.push_str(&guard.push("after"));
        output.push_str(&guard.finish());
        assert_eq!(output, "prefix\nafter");
        assert_eq!(guard.suppressed_blocks(), 1);
    }

    #[test]
    fn fenced_quoted_and_indented_examples_are_not_suppressed() {
        let fenced = "```text\nuser Continue\n\nbashHashd1e9567d: example\n```\t\n~~~\nuser Tool results provided.\n\nTool results:\n\n[readHash9b9a8d05] example\n~~~";
        let quoted = "> user Continue\n>\n> bashHashd1e9567d: example";
        let indented = "    user Continue\n\n    bashHashd1e9567d: example";
        assert_eq!(sanitize(fenced).0, fenced);
        assert_eq!(sanitize(quoted).0, quoted);
        assert_eq!(sanitize(indented).0, indented);
    }

    #[test]
    fn isolated_markers_and_inline_discussion_are_not_suppressed() {
        for input in [
            "Tool results:\n[bashHashd1e9567d] discussed alone",
            "The text user Continue is inline.",
            "bashHashd1e9567d: a log line without a role boundary",
            "user Continue\n\nThe bashHashd1e9567d value is discussed in prose.",
        ] {
            assert_eq!(sanitize(input).0, input, "input={input:?}");
        }
    }

    #[test]
    fn hash_shaped_artifact_name_is_not_trusted_without_an_exact_request_tool() {
        for input in [
            "prefix\nuser Continue\n\nartifactHashdeadbeef: visible\nstill visible",
            "prefix\nuser Tool results provided.\n\nTool results:\n\n[artifactHashdeadbeef] visible",
            "prefix\nTool results:\n\n[artifactHashdeadbeef] visible",
        ] {
            let (output, guard) = sanitize(input);
            assert_eq!(output, input);
            assert_eq!(guard.suppressed_blocks(), 0);
        }
    }

    #[test]
    fn deterministic_mapped_name_is_trusted_only_for_its_request_tool() {
        let bash_mapped = deterministic_mapped_tool_name("Bash");
        assert_eq!(bash_mapped, "bashHashd1e9567d");
        let input = format!("prefix\nuser Continue\n\n{bash_mapped}: hidden");
        assert_eq!(sanitize(&input).0, "prefix\n");

        let other = deterministic_mapped_tool_name("artifact");
        assert_ne!(other, "artifactHashdeadbeef");
        let visible = "prefix\nuser Continue\n\nartifactHashdeadbeef: visible";
        assert_eq!(sanitize(visible).0, visible);
    }

    #[test]
    fn exact_legacy_overlong_mapping_is_suppressed_without_trusting_arbitrary_suffixes() {
        let original =
            "mcp__legacy_server_with_a_very_long_name__tool_with_a_very_long_historical_name";
        let legacy = legacy_overlong_mapped_tool_name(original).expect("overlong legacy mapping");
        let current = deterministic_mapped_tool_name(original);
        assert_ne!(legacy, current);
        assert!(legacy.len() <= 63);

        let polluted = format!("safe prefix\nuser Continue\n\n{legacy}: hidden output");
        for round in 0..5 {
            let mut guard = ToolTranscriptSanitizer::new([original.to_string()]);
            let mut output = String::new();
            for character in polluted.chars() {
                output.push_str(&guard.push(&character.to_string()));
            }
            output.push_str(&guard.finish());
            assert_eq!(output, "safe prefix\n", "round {round}");
            assert_eq!(guard.suppressed_blocks(), 1, "round {round}");
        }

        let arbitrary = "safe prefix\nuser Continue\n\nartifact_deadbeef: visible";
        let mut guard = ToolTranscriptSanitizer::new([original.to_string()]);
        let mut output = guard.push(arbitrary);
        output.push_str(&guard.finish());
        assert_eq!(output, arbitrary);
        assert_eq!(guard.suppressed_blocks(), 0);
    }

    #[test]
    fn supports_crlf_and_known_unhashed_tool_names() {
        let input = "prefix\r\nuser Continue\r\n\r\nBash: hidden\r\ntail";
        assert_eq!(sanitize(input).0, "prefix\r\n");

        let whitespace = "prefix\nuser Continue \t\n \t\nbashHashd1e9567d: hidden";
        assert_eq!(sanitize(whitespace).0, "prefix\n");
    }

    #[test]
    fn normalized_request_sanitization_keeps_signed_thinking_and_tool_data() {
        let fixture = "user Continue\n\nbashHashd1e9567d: hidden";
        let mut request: MessagesRequest = serde_json::from_value(serde_json::json!({
            "model": "claude-sonnet-4",
            "max_tokens": 128,
            "messages": [
                {"role": "user", "content": fixture},
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": fixture, "signature": "sig"},
                    {"type": "text", "text": "safe\nuser Cont"},
                    {"type": "text", "text": "inue\n\nbashHashd1e9567d: hidden"},
                    {"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"value": fixture}},
                    {"type": "text", "text": "after tool"}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": fixture}
                ]},
                {"role": "user", "content": "continue"}
            ],
            "tools": [{"name": "Bash", "description": "run", "input_schema": {"type": "object"}}]
        }))
        .unwrap();

        let report = sanitize_messages_request_assistant_history(&mut request);
        assert_eq!(report.messages, 1);
        assert_eq!(report.blocks, 1);
        assert_eq!(request.messages[0].content, serde_json::json!(fixture));
        assert_eq!(request.messages[1].content[0]["type"], "thinking");
        assert_eq!(request.messages[1].content[0]["thinking"], fixture);
        assert_eq!(request.messages[1].content[0]["signature"], "sig");
        assert_eq!(request.messages[1].content[1]["text"], "safe\n");
        assert_eq!(request.messages[1].content[2]["type"], "tool_use");
        assert_eq!(request.messages[1].content[2]["input"]["value"], fixture);
        assert_eq!(request.messages[1].content[3]["text"], "after tool");
        assert_eq!(request.messages[2].content[0]["content"], fixture);
    }

    #[test]
    fn assistant_history_thinking_policy_keeps_signed_and_redacted_opaque() {
        let fixture = "safe prefix\nuser Continue\n\nBash: hidden";
        for _round in 0..5 {
            let mut request: MessagesRequest = serde_json::from_value(serde_json::json!({
                "model": "claude-sonnet-4",
                "max_tokens": 128,
                "messages": [
                    {"role": "user", "content": fixture},
                    {"role": "assistant", "content": [
                        {"type": "thinking", "thinking": fixture},
                        {"type": "thinking", "thinking": fixture, "signature": "signed-value"},
                        {"type": "thinking", "thinking": "ordinary reasoning", "signature": fixture},
                        {"type": "redacted_thinking", "data": fixture},
                        {"type": "text", "text": "visible"},
                        {"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"discussion": fixture}}
                    ]},
                    {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": fixture}]}
                ],
                "tools": [{"name": "Bash", "description": "run", "input_schema": {"type": "object"}}]
            }))
            .unwrap();

            let report = sanitize_messages_request_assistant_history(&mut request);
            assert_eq!(report.blocks, 1);
            let blocks = request.messages[1].content.as_array().unwrap();
            assert_eq!(
                blocks[0],
                serde_json::json!({"type": "thinking", "thinking": "safe prefix\n"})
            );
            assert_eq!(
                blocks[1],
                serde_json::json!({"type": "thinking", "thinking": fixture, "signature": "signed-value"})
            );
            assert_eq!(
                blocks[2],
                serde_json::json!({"type": "thinking", "thinking": "ordinary reasoning", "signature": fixture})
            );
            assert_eq!(
                blocks[3],
                serde_json::json!({"type": "redacted_thinking", "data": fixture})
            );
            assert_eq!(
                blocks[4],
                serde_json::json!({"type": "text", "text": "visible"})
            );
            assert_eq!(blocks[5]["type"], "tool_use");
            assert_eq!(blocks[5]["input"]["discussion"], fixture);
            assert_eq!(request.messages[0].content, serde_json::json!(fixture));
            assert_eq!(request.messages[2].content[0]["content"], fixture);
        }
    }

    #[test]
    fn clean_signed_redacted_and_fenced_thinking_are_value_identical() {
        let fenced = "```text\nuser Continue\n\nBash: example\n```";
        let original = serde_json::json!({
            "model": "claude-sonnet-4",
            "max_tokens": 128,
            "messages": [{"role": "assistant", "content": [
                {"type": "thinking", "thinking": fenced},
                {"type": "thinking", "thinking": "ordinary reasoning", "signature": "opaque-signature"},
                {"type": "redacted_thinking", "data": "opaque-data"}
            ]}],
            "tools": [{"name": "Bash", "description": "run", "input_schema": {"type": "object"}}]
        });
        for _round in 0..5 {
            let mut request: MessagesRequest = serde_json::from_value(original.clone()).unwrap();
            let typed_original = serde_json::to_value(&request).unwrap();
            assert_eq!(
                sanitize_messages_request_assistant_history(&mut request),
                AssistantHistorySanitization::default()
            );
            assert_eq!(serde_json::to_value(request).unwrap(), typed_original);
        }
    }

    #[test]
    fn normalized_request_is_value_identical_when_cross_block_candidate_is_rejected() {
        let mut request: MessagesRequest = serde_json::from_value(serde_json::json!({
            "model": "claude-sonnet-4",
            "max_tokens": 128,
            "messages": [
                {"role": "user", "content": "start"},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "user Continue\n"},
                    {"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {}},
                    {"type": "text", "text": "normal prose"}
                ]},
                {"role": "user", "content": "next"}
            ],
            "tools": [{"name": "Bash", "description": "run", "input_schema": {"type": "object"}}]
        }))
        .unwrap();
        let original = serde_json::to_value(&request).unwrap();

        let report = sanitize_messages_request_assistant_history(&mut request);
        assert_eq!(report, AssistantHistorySanitization::default());
        assert_eq!(serde_json::to_value(&request).unwrap(), original);
    }

    #[test]
    fn normalized_request_learns_historical_tool_names_without_current_tools() {
        let mut request: MessagesRequest = serde_json::from_value(serde_json::json!({
            "model": "claude-sonnet-4",
            "max_tokens": 128,
            "messages": [
                {"role": "user", "content": "start"},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "safe\nuser Continue\n\nOldMcp: hidden"},
                    {"type": "tool_use", "id": "toolu_1", "name": "OldMcp", "input": {}},
                    {"type": "text", "text": "after tool"}
                ]},
                {"role": "user", "content": "next"}
            ]
        }))
        .unwrap();

        let report = sanitize_messages_request_assistant_history(&mut request);
        assert_eq!(report.blocks, 1);
        assert_eq!(request.messages[1].content[0]["text"], "safe\n");
        assert_eq!(request.messages[1].content[2]["text"], "after tool");
    }

    #[test]
    fn raw_request_sanitization_preserves_unmodeled_fields() {
        let raw = br#"{
            "model":"claude-sonnet-4",
            "max_tokens":128,
            "service_tier":"auto",
            "context_management":{"edits":[{"type":"clear_tool_uses_20250919"}]},
            "messages":[
                {"role":"user","content":"start","future_message_field":{"keep":true}},
                {"role":"assistant","content":[
                    {"type":"text","text":"safe\nuser Continue\n\nFutureMcp: hidden"},
                    {"type":"tool_use","id":"toolu_1","name":"FutureMcp","input":{},"caller":{"type":"direct"}},
                    {"type":"text","text":"after tool"}
                ],"future_assistant_field":"keep"},
                {"role":"user","content":"next"}
            ],
            "tools":[{"name":"FutureMcp","description":"run","input_schema":{"type":"object"},"future_tool_field":7}]
        }"#;

        let (body, report) = sanitize_raw_request_assistant_history(raw)
            .expect("assistant history inspection succeeds")
            .expect("polluted request is rewritten");
        assert_eq!(report.blocks, 1);
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["service_tier"], "auto");
        assert_eq!(
            value["context_management"]["edits"][0]["type"],
            "clear_tool_uses_20250919"
        );
        assert_eq!(value["messages"][0]["future_message_field"]["keep"], true);
        assert_eq!(value["messages"][1]["future_assistant_field"], "keep");
        assert_eq!(value["messages"][1]["content"][0]["text"], "safe\n");
        assert_eq!(value["messages"][1]["content"][1]["type"], "tool_use");
        assert_eq!(
            value["messages"][1]["content"][1]["caller"]["type"],
            "direct"
        );
        assert_eq!(value["messages"][1]["content"][2]["text"], "after tool");
        assert_eq!(value["tools"][0]["future_tool_field"], 7);
    }

    #[test]
    fn raw_request_and_complete_response_apply_thinking_policy_for_five_rounds() {
        let fixture = "safe prefix\nuser Continue\n\nBash: hidden";
        for _round in 0..5 {
            let raw = serde_json::to_vec(&serde_json::json!({
                "model": "claude-sonnet-4",
                "max_tokens": 128,
                "future": {"keep": true},
                "messages": [
                    {"role": "assistant", "content": [
                        {"type": "thinking", "thinking": fixture},
                        {"type": "thinking", "thinking": fixture, "signature": "opaque"},
                        {"type": "text", "text": "visible"}
                    ]}
                ],
                "tools": [{"name": "Bash", "description": "run", "input_schema": {"type": "object"}}]
            }))
            .unwrap();
            let (rewritten, report) = sanitize_raw_request_assistant_history(&raw)
                .unwrap()
                .expect("polluted request is rewritten");
            assert_eq!(report.blocks, 1);
            let rewritten: serde_json::Value = serde_json::from_slice(&rewritten).unwrap();
            assert_eq!(rewritten["future"]["keep"], true);
            assert_eq!(
                rewritten["messages"][0]["content"]
                    .as_array()
                    .unwrap()
                    .len(),
                3
            );
            assert_eq!(
                rewritten["messages"][0]["content"][0],
                serde_json::json!({"type": "thinking", "thinking": "safe prefix\n"})
            );
            assert_eq!(
                rewritten["messages"][0]["content"][1],
                serde_json::json!({"type": "thinking", "thinking": fixture, "signature": "opaque"})
            );
            assert_eq!(
                rewritten["messages"][0]["content"][2],
                serde_json::json!({"type": "text", "text": "visible"})
            );

            let mut response = serde_json::json!({
                "type": "message",
                "content": [
                    {"type": "thinking", "thinking": fixture},
                    {"type": "thinking", "thinking": fixture, "signature": "opaque"},
                    {"type": "redacted_thinking", "data": fixture},
                    {"type": "text", "text": "visible"},
                    {"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"discussion": fixture}}
                ],
                "usage": {"input_tokens": 10, "output_tokens": 20, "output_tokens_details": {"thinking_tokens": 12}},
                "future": "keep"
            });
            let report = sanitize_response_content(&mut response, ["Bash".to_string()]);
            // Complete upstream responses are scanned fail-closed, including authenticated
            // thinking/redacted-thinking payloads. Their bodies remain opaque and unchanged,
            // but each polluted block is reported so the external route can reject the body.
            assert_eq!(report.blocks, 3);
            assert_eq!(response["future"], "keep");
            assert_eq!(response["usage"]["output_tokens"], 20);
            assert_eq!(
                response["usage"]["output_tokens_details"]["thinking_tokens"],
                12
            );
            assert_eq!(response["content"][0]["type"], "thinking");
            assert_eq!(response["content"][0]["thinking"], "safe prefix\n");
            assert_eq!(response["content"][1]["type"], "thinking");
            assert_eq!(response["content"][1]["thinking"], fixture);
            assert_eq!(response["content"][1]["signature"], "opaque");
            assert_eq!(response["content"][2]["type"], "redacted_thinking");
            assert_eq!(response["content"][2]["data"], fixture);
            assert_eq!(response["content"][3]["text"], "visible");
            assert_eq!(response["content"][4]["input"]["discussion"], fixture);
        }
    }

    #[test]
    fn complete_response_text_only_suppression_preserves_usage_value_exactly() {
        let usage = serde_json::json!({
            "input_tokens": 10,
            "output_tokens": 20,
            "output_tokens_details": {"thinking_tokens": 12, "future": 7},
            "future_usage": {"keep": true}
        });
        for _round in 0..5 {
            let mut response = serde_json::json!({
                "content": [
                    {"type":"thinking","thinking":"clean reasoning","signature":"opaque"},
                    {"type":"text","text":"safe\nuser Continue\n\nBash: hidden"}
                ],
                "usage": usage.clone()
            });
            let report = sanitize_response_content(&mut response, ["Bash".to_string()]);
            assert_eq!(report.blocks, 1);
            assert_eq!(response["usage"], usage);
            assert_eq!(response["content"][0]["thinking"], "clean reasoning");
            assert_eq!(response["content"][1]["text"], "safe\n");
        }
    }

    #[test]
    fn raw_request_sanitization_keeps_clean_body_byte_identical() {
        let raw = br#"{ "model": "claude-sonnet-4", "max_tokens": 128, "messages": [{"role":"user","content":"user Continue is being discussed"}], "future": true }"#;
        assert!(raw_body_may_contain_transcript_marker(raw));
        assert!(
            sanitize_raw_request_assistant_history(raw)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn raw_request_prefilter_handles_unicode_escaped_newlines_without_false_rewrite() {
        let signed_only = br#"{"model":"claude-sonnet-4","max_tokens":128,"messages":[{"role":"assistant","content":[{"type":"thinking","thinking":"safe\u000auser Continue\u000a\u000aBash: hidden","signature":"opaque"}]}],"tools":[{"name":"Bash","input_schema":{"type":"object"}}]}"#.as_slice();
        for _round in 0..5 {
            assert!(raw_body_may_contain_transcript_marker(signed_only));
            assert!(
                sanitize_raw_request_assistant_history(signed_only)
                    .unwrap()
                    .is_none()
            );
        }

        let raw = br#"{"model":"claude-sonnet-4","max_tokens":128,"messages":[{"role":"assistant","content":[{"type":"text","text":"safe\r\u000auser Continue\r\u000a\r\u000aBash: hidden"}]}],"tools":[{"name":"Bash","input_schema":{"type":"object"}}]}"#.as_slice();
        let expected = serde_json::json!({"type":"text","text":"safe\r\n"});
        for _round in 0..5 {
            assert!(raw_body_may_contain_transcript_marker(raw));
            let (rewritten, report) = sanitize_raw_request_assistant_history(raw)
                .unwrap()
                .expect("polluted request is rewritten");
            assert_eq!(report.blocks, 1);
            let rewritten: serde_json::Value = serde_json::from_slice(&rewritten).unwrap();
            let content = rewritten["messages"][0]["content"].as_array().unwrap();
            assert_eq!(content, std::slice::from_ref(&expected));
        }

        let discussion = br#"{"model":"claude-sonnet-4","max_tokens":128,"messages":[{"role":"assistant","content":"The phrase user Continue is discussed in documentation."}],"tools":[{"name":"Bash","input_schema":{"type":"object"}}],"future":true}"#;
        assert!(raw_body_may_contain_transcript_marker(discussion));
        assert!(
            sanitize_raw_request_assistant_history(discussion)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn raw_request_prefilter_cannot_be_bypassed_by_unicode_escaped_marker_text() {
        let polluted = br#"{"model":"claude-sonnet-4","max_tokens":128,"messages":[{"role":"assistant","content":"safe\u000a\u0075\u0073\u0065\u0072\u0020\u0043\u006f\u006e\u0074\u0069\u006e\u0075\u0065\u000a\u000aBash: hidden"}],"tools":[{"name":"Bash","input_schema":{"type":"object"}}]}"#;
        for _round in 0..5 {
            assert!(raw_body_may_contain_transcript_marker(polluted));
            let (rewritten, report) = sanitize_raw_request_assistant_history(polluted)
                .unwrap()
                .expect("polluted request is rewritten");
            assert_eq!(report.blocks, 1);
            let rewritten: serde_json::Value = serde_json::from_slice(&rewritten).unwrap();
            assert_eq!(rewritten["messages"][0]["content"], "safe\n");
        }

        let clean_user = br#"{"model":"claude-sonnet-4","max_tokens":128,"messages":[{"role":"user","content":"\u0075ser Continue\n\nBash: literal user data"}],"tools":[{"name":"Bash","input_schema":{"type":"object"}}],"future":true}"#;
        for _round in 0..5 {
            assert!(raw_body_may_contain_transcript_marker(clean_user));
            assert!(
                sanitize_raw_request_assistant_history(clean_user)
                    .unwrap()
                    .is_none()
            );
        }

        let large_clean = format!(
            "{{\"model\":\"claude-sonnet-4\",\"max_tokens\":128,\"messages\":[{{\"role\":\"assistant\",\"content\":\"ordinary\\u0020discussion{}\"}}]}}",
            "x".repeat(1024 * 1024)
        );
        for _round in 0..3 {
            assert!(!raw_body_may_contain_transcript_marker(
                large_clean.as_bytes()
            ));
            assert!(
                sanitize_raw_request_assistant_history(large_clean.as_bytes())
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    fn raw_request_prefilter_detects_every_unicode_escaped_marker_position() {
        for marker in RAW_PREFILTER_MARKERS {
            for escaped_index in 0..marker.len() {
                let escaped = format!(
                    "{}\\u{:04X}{}",
                    String::from_utf8_lossy(&marker[..escaped_index]),
                    marker[escaped_index],
                    String::from_utf8_lossy(&marker[escaped_index + 1..])
                );
                assert!(
                    raw_body_may_contain_transcript_marker(escaped.as_bytes()),
                    "marker={:?} escaped_index={escaped_index} escaped={escaped}",
                    String::from_utf8_lossy(marker)
                );
            }
        }
    }

    #[test]
    fn raw_request_prefilter_skips_large_clean_unicode_escape_bodies() {
        let raw = format!(
            "{{\"messages\":[{{\"role\":\"assistant\",\"content\":\"{}\"}}]}}",
            "\\u4E2D".repeat(1024 * 1024 / 6)
        );
        for _round in 0..5 {
            assert!(!raw_body_may_contain_transcript_marker(raw.as_bytes()));
            assert!(
                sanitize_raw_request_assistant_history(raw.as_bytes())
                    .unwrap()
                    .is_none()
            );
        }

        let escaped_backslash = br#"{"messages":[{"role":"assistant","content":"\\u0075ser Continue is literal documentation"}]}"#;
        assert!(!raw_body_may_contain_transcript_marker(escaped_backslash));
    }

    #[test]
    fn raw_request_prefilter_skips_json_dom_for_marker_free_bodies() {
        let raw = br#"{ "model": "claude-sonnet-4", "messages": [{"role":"assistant","content":"artifactHashdeadbeef is ordinary prose"}], "future": true }"#;
        assert!(!raw_body_may_contain_transcript_marker(raw));
        assert!(
            sanitize_raw_request_assistant_history(raw)
                .unwrap()
                .is_none()
        );

        let malformed_but_marker_free = b"not json and no transcript scaffold";
        assert!(!raw_body_may_contain_transcript_marker(
            malformed_but_marker_free
        ));
        assert_eq!(
            sanitize_raw_request_assistant_history(malformed_but_marker_free),
            Err(RawAssistantHistorySanitizationError::InvalidJson)
        );
    }

    fn polluted_raw_request_at_depth(depth: usize, tool_name: &str) -> Vec<u8> {
        assert!(depth >= 1);
        let nested_containers = depth - 1;
        let mut raw = format!(r#"{{"model":"m","max_tokens":128,"future":"#).into_bytes();
        raw.extend(std::iter::repeat_n(b'[', nested_containers));
        raw.push(b'0');
        raw.extend(std::iter::repeat_n(b']', nested_containers));
        raw.extend_from_slice(
            format!(
                r#", "messages":[{{"role":"assistant","content":"safe\nuser Continue\n\n{tool_name}: hidden"}}],"tools":[{{"name":"{tool_name}","input_schema":{{"type":"object"}}}}]}}"#
            )
            .as_bytes(),
        );
        raw
    }

    #[test]
    fn raw_history_inspection_matches_entry_depth_and_hash_independent_signatures() {
        for round in 0..5 {
            for tool_name in ["Bash", "bashHashd1e9567d", "project_tool_without_hash"] {
                for depth in [127usize, 128, 129, 191, 192] {
                    let raw = polluted_raw_request_at_depth(depth, tool_name);
                    let (rewritten, report) = sanitize_raw_request_assistant_history(&raw)
                        .unwrap_or_else(|error| {
                            panic!("round {round}, depth {depth}, tool {tool_name}: {error}")
                        })
                        .expect("polluted assistant history is rewritten");
                    assert_eq!(report.blocks, 1, "round {round}, depth {depth}");
                    let mut deserializer = serde_json::Deserializer::from_slice(&rewritten);
                    deserializer.disable_recursion_limit();
                    let value =
                        serde_json::Value::deserialize(&mut deserializer).expect("sanitized JSON");
                    deserializer.end().expect("complete sanitized JSON");
                    assert_eq!(
                        value["messages"][0]["content"], "safe\n",
                        "round {round}, depth {depth}, tool {tool_name}"
                    );
                }

                let raw = polluted_raw_request_at_depth(193, tool_name);
                assert_eq!(
                    sanitize_raw_request_assistant_history(&raw),
                    Err(RawAssistantHistorySanitizationError::InvalidJson),
                    "round {round}, tool {tool_name}"
                );
            }
        }
    }

    #[test]
    fn raw_history_inspection_reports_marker_bearing_malformed_json() {
        let malformed = br#"{"model":"m","messages":[{"role":"assistant","content":"user Continue\n\nBash: hidden"}],}"#;
        for _round in 0..5 {
            assert_eq!(
                sanitize_raw_request_assistant_history(malformed),
                Err(RawAssistantHistorySanitizationError::InvalidJson)
            );
        }
    }

    #[test]
    fn optional_local_claude_jsonl_replay_suppresses_every_polluted_record() {
        let Ok(path) = std::env::var("KIRO_CLAUDE_TRANSCRIPT_FIXTURE") else {
            return;
        };
        let file = std::fs::read_to_string(path).expect("read Claude JSONL fixture");
        let known = [
            "Bash",
            "Read",
            "Edit",
            "Write",
            "bashHashd1e9567d",
            "readHash9b9a8d05",
            "editHash464c4ffd",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
        let mut polluted_records = 0u32;
        let mut suppressed_chars = 0usize;

        for line in file.lines() {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if value.get("type").and_then(|value| value.as_str()) != Some("assistant") {
                continue;
            }
            for block in value
                .pointer("/message/content")
                .and_then(|value| value.as_array())
                .into_iter()
                .flatten()
            {
                if block.get("type").and_then(|value| value.as_str()) != Some("text") {
                    continue;
                }
                let Some(text) = block.get("text").and_then(|value| value.as_str()) else {
                    continue;
                };
                let mut guard = ToolTranscriptSanitizer::new(known.iter().cloned());
                let mut output = guard.push(text);
                output.push_str(&guard.finish());
                if guard.suppressed_blocks() == 0 {
                    continue;
                }
                polluted_records = polluted_records.saturating_add(1);
                suppressed_chars = suppressed_chars.saturating_add(guard.suppressed_chars());

                let mut second_pass = ToolTranscriptSanitizer::new(known.iter().cloned());
                let _ = second_pass.push(&output);
                let _ = second_pass.finish();
                assert_eq!(
                    second_pass.suppressed_blocks(),
                    0,
                    "sanitization is idempotent"
                );
            }
        }

        let minimum_records = std::env::var("KIRO_CLAUDE_TRANSCRIPT_MIN_RECORDS")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(1);
        assert!(
            polluted_records >= minimum_records,
            "expected at least {minimum_records} polluted records, found {polluted_records}"
        );
        assert!(suppressed_chars > 0);
    }
}
