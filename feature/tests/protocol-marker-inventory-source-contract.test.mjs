import assert from 'node:assert/strict'
import fs from 'node:fs'
import path from 'node:path'
import test from 'node:test'

const ROOT = fs.realpathSync(path.resolve(import.meta.dirname, '../..'))
const SRC = path.join(ROOT, 'src')

function walkRustFiles(dir, out = []) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    if (entry.name === 'target') {
      continue
    }
    const absolute = path.join(dir, entry.name)
    if (entry.isDirectory()) {
      walkRustFiles(absolute, out)
    } else if (entry.isFile() && entry.name.endsWith('.rs')) {
      out.push(absolute)
    }
  }
  return out
}

function relative(file) {
  return path.relative(ROOT, file).split(path.sep).join('/')
}

function sourceFile(relativePath) {
  return fs.readFileSync(path.join(ROOT, relativePath), 'utf8')
}

function productionSection(source) {
  const marker = '\n#[cfg(test)]\nmod tests'
  const index = source.indexOf(marker)
  return index === -1 ? source : source.slice(0, index)
}

function stripComments(source) {
  return source
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .replace(/(^|[^:])\/\/.*$/gm, '$1')
}

function productionCode(relativePath) {
  return stripComments(productionSection(sourceFile(relativePath)))
}

function productionRustFiles() {
  return walkRustFiles(SRC)
    .map(relative)
    .filter((file) => !file.endsWith('/tests.rs'))
}

function markerInventory(markers) {
  const inventory = Object.fromEntries(markers.map((marker) => [marker, []]))
  for (const file of productionRustFiles()) {
    const code = productionCode(file)
    for (const marker of markers) {
      if (code.includes(marker)) {
        inventory[marker].push(file)
      }
    }
  }
  for (const files of Object.values(inventory)) {
    files.sort()
  }
  return inventory
}

function sourceWindow(source, marker, length = 1_200) {
  const start = source.indexOf(marker)
  assert.notEqual(start, -1, `missing source marker: ${marker}`)
  return source.slice(start, start + length)
}

test('production transcript markers are confined to sanitizer and stream protocol adapters', () => {
  const markers = [
    'user Continue',
    'user Tool results provided',
    'Tool results:',
    '<function_results>',
    '</function_results>',
    '<function_calls>',
    '</function_calls>',
    '<invoke',
    '</invoke>',
    '[previous output]',
    '[trimmed output]',
    '[duplicate output]',
  ]

  assert.deepEqual(markerInventory(markers), {
    'user Continue': ['src/anthropic/transcript_sanitizer.rs'],
    'user Tool results provided': ['src/anthropic/transcript_sanitizer.rs'],
    'Tool results:': ['src/anthropic/stream.rs', 'src/anthropic/transcript_sanitizer.rs'],
    '<function_results>': ['src/anthropic/stream.rs'],
    '</function_results>': ['src/anthropic/stream.rs'],
    '<function_calls>': ['src/anthropic/stream.rs'],
    '</function_calls>': ['src/anthropic/stream.rs'],
    '<invoke': [],
    '</invoke>': [],
    '[previous output]': [],
    '[trimmed output]': [],
    '[duplicate output]': [],
  })

  const converter = productionCode('src/anthropic/converter.rs')
  assert.doesNotMatch(converter, /"[^"]*user Continue/)
  assert.doesNotMatch(converter, /"[^"]*Tool results provided/)
  assert.doesNotMatch(converter, /"[^"]*Tool results:/)
})

test('current and history tool-result-only placeholders stay semantic without transcript prose', () => {
  const converter = productionCode('src/anthropic/converter.rs')
  assert.match(converter, /const EMPTY_USER_CONTENT_PLACEHOLDER: &str = "\.";/)
  assert.match(converter, /const TOOL_RESULTS_PROVIDED_PLACEHOLDER: &str = "Tool result received\.";/)
  assert.doesNotMatch(converter, /const TOOL_RESULTS_PROVIDED_PLACEHOLDER: &str = EMPTY_USER_CONTENT_PLACEHOLDER;/)
  assert.doesNotMatch(converter, /const TOOL_RESULTS_PROVIDED_PLACEHOLDER: &str = "Tool results provided/)

  const history = productionCode('src/anthropic/converter/history.rs')
  assert.match(history, /content = TOOL_RESULTS_PROVIDED_PLACEHOLDER\.to_string\(\);/)
  assert.doesNotMatch(history, /Tool results provided|Tool results:/)

  const pairing = productionCode('src/anthropic/converter/tool_pairing.rs')
  assert.match(pairing, /user\.user_input_message\.content = super::EMPTY_USER_CONTENT_PLACEHOLDER\.to_string\(\);/)
  assert.doesNotMatch(pairing, /Tool results provided|Tool results:/)

  const payloadGuard = productionCode('src/anthropic/payload_guard.rs')
  assert.match(payloadGuard, /const EMPTY_TOOL_RESULT_CONTENT_PLACEHOLDER: &str = "Tool result content was empty\.";/)
  assert.match(payloadGuard, /const EMPTY_USER_CONTENT_PLACEHOLDER: &str = "\.";/)
  assert.match(payloadGuard, /\*content = EMPTY_USER_CONTENT_PLACEHOLDER\.to_string\(\);/)
  assert.doesNotMatch(payloadGuard, /Tool results provided|Tool results:/)
})

test('invalid tool-result repair drops structure without textifying rejected content', () => {
  const converterPairing = productionCode('src/anthropic/converter/tool_pairing.rs')
  const historyRepair = sourceWindow(converterPairing, 'pub(super) fn sanitize_history_tool_results', 2_800)
  assert.match(historyRepair, /results\.retain\(\|result\| \{/)
  assert.match(historyRepair, /return false;/)
  assert.match(historyRepair, /user\.user_input_message\.content = super::EMPTY_USER_CONTENT_PLACEHOLDER\.to_string\(\);/)
  assert.doesNotMatch(historyRepair, /push_str|format!\([^)]*result|result\.content/)

  const validateRepair = sourceWindow(converterPairing, 'pub(super) fn validate_tool_pairing', 3_600)
  assert.match(validateRepair, /Vec<ToolResult>/)
  assert.match(validateRepair, /filtered/)
  assert.doesNotMatch(validateRepair, /textified|push_str|format!\([^)]*result/)

  const payloadGuard = productionCode('src/anthropic/payload_guard.rs')
  const orphanRepair = sourceWindow(payloadGuard, 'fn repair_tool_results', 1_000)
  assert.match(orphanRepair, /results\.retain\(\|result\| valid_ids\.contains\(&result\.tool_use_id\)\)/)
  assert.match(orphanRepair, /EMPTY_USER_CONTENT_PLACEHOLDER/)
  assert.doesNotMatch(orphanRepair, /push_str|format!\(/)

  const productionAggregate = productionRustFiles()
    .map((file) => productionCode(file))
    .join('\n')
  for (const field of [
    'orphan_tool_results_textified',
    'duplicate_tool_results_textified',
    'textified_duplicate_tool_results',
    'textified_orphan_tool_results',
    'flattened_history_tool_uses',
    'textified_history_tool_results',
  ]) {
    assert.doesNotMatch(
      productionAggregate,
      new RegExp(`${field}\\s*(?:\\+=|\\.saturating_add)`),
      `${field} must remain a diagnostic field, not a production textification action`,
    )
  }
})

test('literal function-call recovery stays scoped to strict envelopes and never parses bare invoke text', () => {
  const stream = productionCode('src/anthropic/stream.rs')
  const envelopeParser = sourceWindow(stream, 'const FUNCTION_CALLS_TAGS', 2_400)
  assert.match(envelopeParser, /\("<function_calls>", "<\/function_calls>"\)/)
  assert.match(envelopeParser, /const INVOKE_TAG_NAMES: &\[&str\] = &\["invoke", "antml:invoke"\]/)
  assert.match(envelopeParser, /parse_named_protocol_open_tag/)
  assert.match(envelopeParser, /strip_prefix\("name=\\\""\)/)
  assert.match(envelopeParser, /strip_suffix\('>"'\)|strip_suffix\('"'\)/)

  const extractor = sourceWindow(stream, 'pub(crate) fn extract_invoke_content_blocks', 2_700)
  assert.match(extractor, /find_next_literal_protocol_open/)
  assert.match(extractor, /parse_function_calls_envelope/)
  assert.match(extractor, /all_tools_known/)
  assert.match(extractor, /known_tool_names\.contains\(&call\.name\)/)
  assert.match(extractor, /protocol_visible_prefix/)
  assert.doesNotMatch(extractor, /find_next_named_protocol_open\(text/)

  const sniffing = sourceWindow(stream, 'fn drain_invoke_sniff_buffer', 4_200)
  assert.match(sniffing, /find_next_literal_protocol_open/)
  assert.match(sniffing, /FunctionCallsEnvelope::Incomplete/)
  assert.doesNotMatch(sniffing, /<invoke/)

  const scanner = productionCode('src/anthropic/stream.rs')
  assert.match(scanner, /const MAX_FUNCTION_CALLS_ENVELOPE_HOLD_BYTES: usize = 262_144;/)
  assert.match(
    scanner,
    /fn find_next_literal_protocol_open[\s\S]*find_next_function_calls_open/,
  )
})
