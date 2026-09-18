import assert from 'node:assert/strict'
import fs from 'node:fs'
import path from 'node:path'
import test from 'node:test'

import {
  detectLeaks,
  redactTranscriptLine,
  summarizeTranscript,
} from './claude-cli-leak-scanner.mjs'

const cleanParsed = {
  assistantText: 'LONGSESSION_ASSISTANT_OK',
  resultText: 'LONGSESSION_ASSISTANT_OK',
  toolNames: ['Bash', 'Read'],
}
const runnerSource = fs.readFileSync(
  path.join(import.meta.dirname, 'claude-cli-long-session-continue.mjs'),
  'utf8',
)

test('normal Claude CLI control transcript is evidence, not automatically a public leak', () => {
  const stdout = [
    'user Continue',
    'Tool results:',
    '<function_results>',
    'bashHashdeadbeef',
  ].join('\n')
  const evidence = summarizeTranscript(stdout)
  assert.deepEqual(detectLeaks(cleanParsed, '', stdout), [])
  assert.equal(evidence.errorMatchCount, 0)
  assert.equal(evidence.warningMatchCount, 5)
  assert.equal(evidence.suspiciousLineCount, 4)
  assert.equal(evidence.suspiciousLines[0].severity, 'warning')
  assert.equal(evidence.suspiciousLines[0].lineSha256.length, 64)
})

test('visible transcript markers and internal routing terms remain hard failures', () => {
  const parsed = {
    assistantText: 'user Continue\n<function_results>\nexternal pool routeKind=local_credential',
    resultText: '',
    toolNames: [],
  }
  const matches = detectLeaks(parsed, '', '')
  assert.ok(matches.includes('assistantText:new_continue_transcript'))
  assert.ok(matches.includes('assistantText:function_results_envelope'))
  assert.ok(matches.includes('assistantText:internal_pool_or_scheduler_term'))
})

test('credential fields and token-shaped values fail even when parser misses them', () => {
  const stdout = [
    'credential JSON: {"access_token":"secret-access-value"}',
    'Authorization: Bearer eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.payload8.signature',
    'refresh token and API key must never be visible',
  ].join('\n')
  const matches = detectLeaks(cleanParsed, '', stdout)
  assert.ok(matches.includes('stdoutTranscript:credential_json_field'))
  assert.ok(matches.includes('stdoutTranscript:credential_json_phrase'))
  assert.ok(matches.includes('stdoutTranscript:refresh_token_phrase'))
  assert.ok(matches.includes('stdoutTranscript:api_key_phrase'))
  assert.ok(matches.includes('stdoutTranscript:bearer_token'))
  assert.ok(matches.includes('stdoutTranscript:jwt_like_token'))
})

test('transcript evidence redacts known and token-shaped values', () => {
  const line = 'access_token=secret-value Bearer eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.payload.signature'
  const redacted = redactTranscriptLine(line, ['secret-value'])
  assert.doesNotMatch(redacted, /secret-value/)
  assert.doesNotMatch(redacted, /eyJhbGci/)
  assert.match(redacted, /<redacted>/)
})

test('long-session runner records bounded transcript evidence without persisting raw stdout', () => {
  assert.match(runnerSource, /claude-cli-leak-scanner\.mjs/)
  assert.match(runnerSource, /summarizeTranscript\(cli\.stdoutText/)
  assert.match(runnerSource, /stdoutEvidence/)
  assert.match(runnerSource, /stdoutByteLength/)
  assert.match(runnerSource, /stdoutLineCount/)
  assert.match(runnerSource, /const knownSecrets = \[REQUEST_KEY, ADMIN_KEY, KIRO_KEY, POSTGRES_URL, REDIS_URL\]/)
  assert.doesNotMatch(runnerSource, /stdout(?:Text|Transcript)\s*:\s*cli\.stdoutText/)
})
