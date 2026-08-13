import assert from 'node:assert/strict'
import fs from 'node:fs'
import path from 'node:path'
import test from 'node:test'

const ROOT = fs.realpathSync(path.resolve(import.meta.dirname, '../..'))

function sourceFile(relativePath) {
  return fs.readFileSync(path.join(ROOT, relativePath), 'utf8')
}

function sourceWindow(source, marker, length = 1_200) {
  const start = source.indexOf(marker)
  assert.notEqual(start, -1, `missing source marker: ${marker}`)
  return source.slice(start, start + length)
}

function sourceBetween(source, startMarker, endMarker) {
  const start = source.indexOf(startMarker)
  assert.notEqual(start, -1, `missing source start marker: ${startMarker}`)
  const end = source.indexOf(endMarker, start)
  assert.notEqual(end, -1, `missing source end marker: ${endMarker}`)
  return source.slice(start, end)
}

test('sanitizer trusts request-known tool names and deterministic mappings, not arbitrary Hash-shaped text', () => {
  const source = sourceFile('src/anthropic/transcript_sanitizer.rs')
  const implementation = source.split('#[cfg(test)]\nmod tests')[0]

  assert.match(
    implementation,
    /use super::converter::\{deterministic_mapped_tool_name, legacy_overlong_mapped_tool_name\}/,
  )

  const constructor = sourceWindow(source, 'pub(crate) fn new(known_tool_names', 900)
  assert.match(constructor, /deterministic_mapped_tool_name\(&name\)/)
  assert.match(constructor, /legacy_overlong_mapped_tool_name\(&name\)/)
  assert.match(constructor, /name\.to_ascii_lowercase\(\)/)
  assert.match(constructor, /mapped\.to_ascii_lowercase\(\)/)
  assert.match(constructor, /legacy\.map\(\|value\| value\.to_ascii_lowercase\(\)\)/)
  assert.match(constructor, /filter\(\|name\| !name\.is_empty\(\)\)/)

  const exactMatch = sourceWindow(source, 'fn is_internal_tool_name(&self, name: &str)', 260)
  assert.match(exactMatch, /self\.known_tool_names\.contains\(&name\.to_ascii_lowercase\(\)\)/)

  const prefixMatch = sourceWindow(source, 'fn is_internal_tool_name_prefix(&self, name: &str)', 420)
  assert.match(prefixMatch, /known\.starts_with\(&name\)/)
  assert.doesNotMatch(prefixMatch, /Hash|\[0-9a-fA-F\]/)

  const colonStatus = sourceWindow(source, 'fn colon_tool_header_status(&self, value: &str)', 650)
  assert.match(colonStatus, /self\.is_internal_tool_name\(name\)/)
  assert.doesNotMatch(colonStatus, /Hash|\[0-9a-fA-F\]/)

  const bracketStatus = sourceWindow(source, 'fn bracket_tool_header_status(&self, value: &str)', 650)
  assert.match(bracketStatus, /self\.is_internal_tool_name\(&line\[1\.\.close\]\)/)
  assert.doesNotMatch(bracketStatus, /Hash|\[0-9a-fA-F\]/)

  assert.doesNotMatch(implementation, /Regex::new|regex::Regex|Hash\[0-9a-fA-F\]|\[0-9a-fA-F\]\{8\}/)
  assert.match(source, /hash_shaped_artifact_name_is_not_trusted_without_an_exact_request_tool/)
  assert.match(source, /artifactHashdeadbeef/)
  assert.match(source, /exact_legacy_overlong_mapping_is_suppressed_without_trusting_arbitrary_suffixes/)
})

test('raw assistant-history sanitizer keeps marker-free request bodies byte-identical', () => {
  const source = sourceFile('src/anthropic/transcript_sanitizer.rs')
  const rawFunction = sourceWindow(
    source,
    'pub(crate) fn sanitize_raw_request_assistant_history_with_probe',
    2_000,
  )

  const probeGuard = rawFunction.indexOf('if !probe.matches_body(raw_body) || probe.scan_error().is_some()')
  const markerGuard = rawFunction.indexOf('if !raw_body_may_contain_transcript_marker(raw_body)')
  const deserializer = rawFunction.indexOf('serde_json::Deserializer::from_slice(raw_body)')
  const serializer = rawFunction.indexOf('serde_json::to_vec(&value)')

  assert.notEqual(probeGuard, -1)
  assert.notEqual(markerGuard, -1)
  assert.notEqual(deserializer, -1)
  assert.notEqual(serializer, -1)
  assert.ok(probeGuard < markerGuard, 'raw-body probe guard must run before marker prefilter')
  assert.ok(markerGuard < deserializer, 'clean marker-free bodies must return before JSON DOM parse')
  assert.ok(deserializer < serializer, 'serialization may only happen after a confirmed mutation path')
  assert.match(rawFunction, /return Ok\(None\);/)
  assert.match(rawFunction, /let known_tool_names = collect_known_tool_names_from_value\(&value\)/)
  assert.match(rawFunction, /if report\.blocks == 0 \{\s*return Ok\(None\);\s*\}/s)

  const markerScan = sourceWindow(source, 'fn raw_body_may_contain_transcript_marker', 1_800)
  assert.match(markerScan, /has_literal_marker \|\| escaped_json_may_contain_transcript_marker\(raw_body\)/)
  assert.match(markerScan, /decode_ascii_json_escape/)
  assert.match(source, /raw_request_sanitization_keeps_clean_body_byte_identical/)
  assert.match(source, /raw_request_sanitization_preserves_unmodeled_fields/)
  assert.match(source, /raw_request_prefilter_cannot_be_bypassed_by_unicode_escaped_marker_text/)
})

test('assistant sanitizer only mutates assistant text or thinking and keeps tool/user payloads as data', () => {
  const source = sourceFile('src/anthropic/transcript_sanitizer.rs')
  const runLoop = sourceWindow(source, 'fn sanitize_assistant_run', 3_600)

  assert.match(runLoop, /if block_type\.as_deref\(\) == Some\("text"\)/)
  assert.match(runLoop, /Some\("thinking" \| "redacted_thinking"\)/)
  assert.match(runLoop, /block\["text"\] = serde_json::Value::String\(sanitizer\.push\(&text\)\)/)
  assert.match(runLoop, /let pending = sanitizer\.structured_tool_boundary\(\)/)
  assert.doesNotMatch(runLoop, /block\["input"\]\s*=/)

  const assistantRuns = sourceWindow(source, 'pub(crate) fn sanitize_assistant_message_runs', 1_000)
  assert.match(assistantRuns, /if messages\[start\]\.role != "assistant" \{/)
  assert.match(assistantRuns, /while end < messages\.len\(\) && messages\[end\]\.role == "assistant"/)

  assert.match(source, /normalized_request_sanitization_keeps_user_and_tool_data_but_drops_signed_leak/)
  assert.match(source, /request\.messages\[0\]\.content, serde_json::json!\(fixture\)/)
  assert.match(source, /request\.messages\[2\]\.content\[0\]\["content"\], fixture/)
})

test('signed and redacted thinking contamination is atomic and never recombined with stale integrity data', () => {
  const source = sourceFile('src/anthropic/transcript_sanitizer.rs')
  const runLoop = sourceWindow(source, 'let (field, atomic) = if block_type.as_deref() == Some("thinking")', 2_400)
  assert.match(runLoop, /"signature"/)
  assert.match(runLoop, /\("data", true\)/)
  assert.match(runLoop, /let suppressed_before = thinking_sanitizer\.suppressed_blocks\(\)/)
  assert.match(runLoop, /let _ = thinking_sanitizer\.push\(signature\)/)
  assert.match(runLoop, /if suppressed && atomic \{\s*removed_blocks\.insert\(\(message_idx, block_idx\)\);/s)
  assert.match(runLoop, /else if suppressed/)
  assert.match(source, /assistant_history_thinking_policy_is_atomic_for_signed_and_redacted/)
  assert.match(source, /clean_signed_redacted_and_fenced_thinking_are_value_identical/)

  const handlers = sourceFile('src/anthropic/handlers.rs')
  const completeThinking = sourceWindow(handlers, 'fn sanitize_complete_thinking_segment', 900)
  assert.match(completeThinking, /integrity_values: impl IntoIterator/)
  assert.match(completeThinking, /for value in integrity_values/)
  assert.match(completeThinking, /let _ = sanitizer\.push\(value\)/)

  const nonStreamAppend = sourceWindow(handlers, 'fn append_non_stream_reasoning_and_text', 2_400)
  assert.match(nonStreamAppend, /let \(safe_thinking, polluted\) = sanitize_complete_thinking_segment/)
  assert.match(nonStreamAppend, /if !polluted \|\| signature\.is_none\(\)/)
  assert.match(
    nonStreamAppend,
    /let output = if signature\.is_some\(\) \{\s*native_thinking_content\s*\} else \{\s*safe_thinking\.as_str\(\)\s*\}/s,
  )
})

test('history converter builds sanitizer scope from all current and historical tool-name authorities', () => {
  const source = sourceFile('src/anthropic/converter/history.rs')
  const buildHistory = sourceWindow(source, 'pub(super) fn build_history', 2_900)
  assert.match(buildHistory, /req\s*\.\s*tools[\s\S]*\.map\(\|tool\| tool\.name\.clone\(\)\)/)
  assert.match(buildHistory, /known_tool_names\.extend\(tool_name_map\.keys\(\)\.cloned\(\)\)/)
  assert.match(buildHistory, /known_tool_names\.extend\(tool_name_map\.values\(\)\.cloned\(\)\)/)
  assert.match(buildHistory, /block\.get\("type"\)[\s\S]*Some\("tool_use"\)/)
  assert.match(buildHistory, /block\.get\("name"\)/)

  const conversion = sourceWindow(source, 'fn convert_assistant_message_with_known_tools', 1_400)
  assert.match(conversion, /options\.compat_profile != CompatProfile::AnthropicStrict/)
  assert.match(conversion, /ToolTranscriptSanitizer::new\(known_tool_names\.iter\(\)\.cloned\(\)\)/)

  const mergeDefense = sourceWindow(
    source,
    'A Kiro assistant history item flattens every Anthropic visible text block',
    4_500,
  )
  assert.match(mergeDefense, /final_known_tool_names\.extend\(tool_name_map\.keys\(\)\.cloned\(\)\)/)
  assert.match(mergeDefense, /final_known_tool_names\.extend\(tool_name_map\.values\(\)\.cloned\(\)\)/)
  assert.match(mergeDefense, /ToolTranscriptSanitizer::new\(final_known_tool_names\.iter\(\)\.cloned\(\)\)/)
  assert.match(mergeDefense, /sanitizer\.push\("\\n\\n"\)/)
  assert.match(mergeDefense, /sanitized internal tool transcript reconstructed while merging assistant history/)
})

test('request entry blocks strict contamination and prevents raw external bypass after sanitization', () => {
  const source = sourceFile('src/anthropic/handlers/request_entry.rs')
  const entry = sourceWindow(source, 'pub(super) async fn handle_messages_endpoint', 5_500)

  assert.match(entry, /let effective_raw_body = raw_body\.clone\(\)/)
  assert.match(entry, /sanitize_raw_request_assistant_history_with_probe\(\s*&raw_body,\s*&raw_probe/s)
  assert.match(entry, /if runtime_config\.compat_profile\.is_strict\(\)/)
  assert.match(entry, /"strict_request_protocol_contamination"/)
  assert.match(entry, /raw_body = Bytes::from\(sanitized_body\)/)
  assert.match(entry, /request_history_contaminated = true/)
  assert.match(entry, /should_try_raw_external_routes\(request_history_contaminated\)/)

  const rawExternalGate = sourceWindow(source, 'fn should_try_raw_external_routes', 160)
  assert.match(rawExternalGate, /!request_history_contaminated/)
})

test('stream response contamination uses fail-closed terminal semantics instead of blank or partial success', () => {
  const streamSource = sourceFile('src/anthropic/stream.rs')
  const context = sourceWindow(streamSource, 'pub struct StreamContext', 5_500)
  assert.match(context, /tool_transcript_sanitizer: ToolTranscriptSanitizer/)
  assert.match(context, /thinking_transcript_sanitizer: ToolTranscriptSanitizer/)
  assert.match(context, /tool_context_leak_markers: Vec<&'static str>/)

  const metrics = sourceWindow(streamSource, 'pub fn suppressed_tool_context_leak_blocks', 1_200)
  assert.match(metrics, /tool_transcript_sanitizer[\s\S]*saturating_add\(self\.thinking_transcript_sanitizer\.suppressed_blocks\(\)\)/)
  assert.match(metrics, /pub fn suppressed_tool_context_leak_chars/)
  assert.match(metrics, /pub fn suppressed_tool_context_leak_kinds/)

  const finalEvents = sourceWindow(streamSource, 'let safe_pending = self.tool_transcript_sanitizer.finish()', 1_100)
  assert.match(finalEvents, /self\.record_stream_error\("api_error", RESPONSE_PROTOCOL_CONTAMINATION_DETAIL\)/)
  assert.match(finalEvents, /if self\.stream_error\.is_some\(\)/)
  assert.match(finalEvents, /Self::create_error_event\(error_type, message\)/)
  assert.match(finalEvents, /return events;/)

  const testHelper = sourceWindow(streamSource, 'fn assert_protocol_contamination_error', 700)
  assert.match(testHelper, /event\.event != "message_delta" && event\.event != "message_stop"/)
  assert.match(testHelper, /RESPONSE_PROTOCOL_CONTAMINATION_DETAIL\.to_string\(\)/)

  const handlers = sourceFile('src/anthropic/handlers.rs')
  const midChunk = sourceWindow(handlers, 'let mut protocol_failure: Option<(StreamRetryReason, String)> = None', 2_600)
  assert.match(midChunk, /suppressed_before = state[\s\S]*suppressed_tool_context_leak_blocks\(\)/)
  assert.match(midChunk, /StreamRetryReason::ProtocolContamination/)
  assert.match(midChunk, /break;/)

  const precommitRetry = sourceWindow(handlers, 'if let Some((retry_reason, detail)) = protocol_failure', 2_000)
  assert.match(precommitRetry, /if !state\.downstream_committed/)
  assert.match(precommitRetry, /retry_stream_before_downstream_commit/)
  assert.match(precommitRetry, /StreamTerminalReason::ProtocolContamination/)
  assert.match(precommitRetry, /finish_stream_with_recorded_error/)
})

test('non-stream response contamination records usage failure and returns sanitized gateway error', () => {
  const source = sourceFile('src/anthropic/handlers.rs')
  const nonStream = sourceWindow(
    source,
    'let suppressed_tool_context_leak_blocks = transcript_sanitizer',
    1_700,
  )

  assert.match(nonStream, /thinking_transcript_sanitizer\.suppressed_blocks\(\)/)
  assert.match(nonStream, /suppressed_tool_context_leak_kinds\.sort_unstable\(\)/)
  assert.match(nonStream, /mark_suppressed_tool_context_leak/)
  assert.match(nonStream, /credential_usage\.record_failure\(\s*UsageRecordStatus::Error,\s*"api_error",\s*RESPONSE_PROTOCOL_CONTAMINATION_DETAIL/s)
  assert.match(nonStream, /StatusCode::BAD_GATEWAY/)
  assert.match(nonStream, /envelope::PUBLIC_PROCESSING_FAILED_MESSAGE/)

  const usageSource = sourceFile('src/anthropic/usage.rs')
  assert.match(usageSource, /suppressed_tool_context_leak_blocks: Option<u32>/)
  assert.match(usageSource, /suppressed_tool_context_leak_chars: Option<u64>/)
  assert.match(usageSource, /suppressed_tool_context_leak_kinds: Option<Vec<String>>/)
  assert.match(usageSource, /ProtocolContamination/)
})

test('external-pool normalized response paths classify protocol contamination and do not emit success after fail closed', () => {
  const source = sourceFile('src/external_pool.rs')
  const errorFn = sourceWindow(source, 'fn external_protocol_contamination_error', 800)
  assert.match(errorFn, /RESPONSE_PROTOCOL_CONTAMINATION_DETAIL\.to_string\(\)/)
  assert.match(errorFn, /retryable: true/)
  assert.match(errorFn, /"protocol_contamination"\.to_string\(\)/)

  const projector = sourceWindow(source, 'protocol_contamination: bool', 1_400)
  assert.match(projector, /protocol_contamination: false/)

  const processor = sourceWindow(source, 'struct ExternalAnthropicTranscriptState', 1_600)
  assert.match(processor, /sanitizer: ToolTranscriptSanitizer/)
  assert.match(processor, /thinking_sanitizer: ToolTranscriptSanitizer/)

  const failClosed = sourceWindow(source, 'fn fail_protocol_contamination(&mut self)', 600)
  assert.match(failClosed, /fail_with_processing_error\(RESPONSE_PROTOCOL_CONTAMINATION_DETAIL\)/)

  const failWithError = sourceWindow(source, 'fn fail_with_processing_error', 1_200)
  assert.match(failWithError, /self\.fatal = true/)
  assert.match(failWithError, /self\.pending_fatal_error = Some\(detail\.to_string\(\)\)/)
  assert.match(failWithError, /external_safe_processing_error_event/)
  assert.doesNotMatch(failWithError, /message_stop|stop_reason/)

  const safeError = sourceWindow(source, 'fn external_safe_processing_error_event', 800)
  assert.match(safeError, /"type": "error"/)
  assert.match(safeError, /"error": \{"type": "api_error", "message": message\}/)
  assert.match(safeError, /event: error/)
  assert.doesNotMatch(safeError, /message_stop|stop_reason/)
})

test('source tests already cover the user-reported truncated leak shapes and non-Hash variants', () => {
  const sanitizer = sourceFile('src/anthropic/transcript_sanitizer.rs')
  const tests = sourceBetween(
    sanitizer,
    '#[cfg(test)]\nmod tests',
    'fn raw_request_sanitization_preserves_unmodeled_fields',
  )

  for (const literal of [
    'user Continue\\n\\nbashHashd1e9567d',
    'user Continue\\n\\nBash',
    'user Tool results provided.\\n\\nTool results:\\n\\n[readHash9b9a8d05',
    'Tool results:\\nordinary prose',
    'artifactHashdeadbeef',
    'fenced_quoted_and_indented_examples_are_not_suppressed',
  ]) {
    assert.match(tests, new RegExp(literal.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')))
  }

  const stream = sourceFile('src/anthropic/stream.rs')
  assert.match(stream, /tool_context_leak_text_only_end_turn/)
  assert.match(stream, /tool_context_leak_markers_do_not_flag_normal_tool_use_turn/)
  assert.match(stream, /response_protocol_contamination_detected/)
})
