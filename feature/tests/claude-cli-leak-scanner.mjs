import crypto from 'node:crypto'

const MAX_SUSPICIOUS_LINES = 16
const MAX_PREVIEW_CHARS = 240

const patternDefinitions = [
  {
    name: 'new_continue_transcript',
    pattern: /(?:^|\n)user Continue(?:\r?\n|$)/i,
    rawSeverity: 'warning',
  },
  {
    name: 'legacy_tool_results_transcript',
    pattern: /(?:^|\n)user Tool results provided\.?/i,
    rawSeverity: 'warning',
  },
  {
    name: 'tool_results_heading',
    pattern: /(?:^|\n)Tool results:\s*(?:\r?\n|$)/i,
    rawSeverity: 'warning',
  },
  {
    name: 'function_results_envelope',
    pattern: /<\/?function_results>/i,
    rawSeverity: 'warning',
  },
  {
    name: 'function_calls_envelope',
    pattern: /<\/?function_calls>/i,
    rawSeverity: 'warning',
  },
  {
    name: 'invoke_envelope',
    pattern: /<invoke\s+name=/i,
    rawSeverity: 'warning',
  },
  {
    name: 'known_hash_tool_name',
    pattern: /\b(?:bash|read|edit|write|glob|grep|websearch|webfetch|task)Hash[0-9a-f]{8}\b/i,
    rawSeverity: 'warning',
  },
  {
    name: 'generic_hash_tool_name',
    pattern: /\b[A-Za-z][A-Za-z0-9]{0,50}Hash[0-9a-f]{8}\b/,
    rawSeverity: 'warning',
  },
  {
    name: 'credential_json_field',
    pattern: /["']?(?:refresh[_-]?token|access[_-]?token|client[_-]?secret|profileArn|api[_-]?key)["']?\s*:/i,
    rawSeverity: 'error',
  },
  {
    name: 'credential_json_phrase',
    pattern: /\b(?:credential|credentials)\s*(?:json|file|object)\b/i,
    rawSeverity: 'error',
  },
  {
    name: 'refresh_token_phrase',
    pattern: /\brefresh[\s_-]+token\b/i,
    rawSeverity: 'error',
  },
  {
    name: 'access_token_phrase',
    pattern: /\baccess[\s_-]+token\b/i,
    rawSeverity: 'error',
  },
  {
    name: 'api_key_phrase',
    pattern: /\bAPI[\s_-]+key\b/i,
    rawSeverity: 'error',
  },
  {
    name: 'bearer_token',
    pattern: /\bBearer\s+[A-Za-z0-9._~+/=-]{20,}\b/i,
    rawSeverity: 'error',
  },
  {
    name: 'jwt_like_token',
    pattern: /\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9._-]{8,}\.[A-Za-z0-9._-]{8,}\b/,
    rawSeverity: 'error',
  },
  {
    name: 'api_key_value',
    pattern: /\b(?:sk|ak|api)[-_][A-Za-z0-9_-]{16,}\b/i,
    rawSeverity: 'error',
  },
  {
    name: 'internal_pool_or_scheduler_term',
    pattern: /\b(?:fallback|external|upstream)\s+pool\b|\b(?:credential|scheduler|cooldown|routeKind|routeSubtype|local_credential|external_pool)\b/i,
    rawSeverity: 'error',
  },
]

export const LEAK_PATTERNS = Object.freeze(
  patternDefinitions.map(({ name, pattern }) => Object.freeze([name, pattern])),
)

const PUBLIC_PATTERN_DEFINITIONS = Object.freeze(patternDefinitions)

function sha256(value) {
  return crypto.createHash('sha256').update(String(value)).digest('hex')
}

function matchingPatterns(text, { raw = false } = {}) {
  const value = String(text || '')
  return PUBLIC_PATTERN_DEFINITIONS
    .filter((definition) => !raw || definition.rawSeverity === 'error')
    .filter((definition) => definition.pattern.test(value))
    .map((definition) => definition.name)
}

function redactSensitiveValues(value, knownSecrets = []) {
  let text = String(value || '')
  for (const secret of knownSecrets) {
    const candidate = String(secret || '')
    if (candidate) text = text.split(candidate).join('<redacted>')
  }
  text = text
    .replace(
      /((?:refresh[_-]?token|access[_-]?token|client[_-]?secret|profileArn|api[_-]?key)\s*[:=]\s*)(["']?)([^"',\s}]+)\2/gi,
      '$1<redacted>',
    )
    .replace(/\bBearer\s+[A-Za-z0-9._~+/=-]{20,}\b/gi, 'Bearer <redacted>')
    .replace(/\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9._-]{8,}\.[A-Za-z0-9._-]{8,}\b/g, '<redacted-jwt>')
    .replace(/\b(?:sk|ak|api)[-_][A-Za-z0-9_-]{16,}\b/gi, '<redacted-api-key>')
  return text
}

export function redactTranscriptLine(value, knownSecrets = []) {
  const redacted = redactSensitiveValues(value, knownSecrets)
  if (redacted.length <= MAX_PREVIEW_CHARS) return redacted
  return `${redacted.slice(0, MAX_PREVIEW_CHARS)}<preview-truncated>`
}

export function summarizeTranscript(stdout, { knownSecrets = [] } = {}) {
  const text = String(stdout || '')
  const lines = text.split(/\r?\n/)
  const suspiciousLines = []
  let errorMatchCount = 0
  let warningMatchCount = 0

  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index]
    const warningMatches = matchingPatterns(line)
    if (warningMatches.length === 0) continue
    const errorMatches = matchingPatterns(line, { raw: true })
    errorMatchCount += errorMatches.length
    warningMatchCount += warningMatches.length - errorMatches.length
    if (suspiciousLines.length >= MAX_SUSPICIOUS_LINES) continue
    suspiciousLines.push({
      lineNumber: index + 1,
      matchNames: warningMatches,
      severity: errorMatches.length > 0 ? 'error' : 'warning',
      lineSha256: sha256(line),
      redactedPreview: redactTranscriptLine(line, knownSecrets),
    })
  }

  return {
    sha256: sha256(text),
    byteLength: Buffer.byteLength(text),
    lineCount: lines.length,
    suspiciousLineCount: suspiciousLines.length,
    errorMatchCount,
    warningMatchCount,
    suspiciousLines,
  }
}

export function detectLeaks(
  parsed,
  stderr,
  stdout = '',
  { knownSecrets = [] } = {},
) {
  const surfaces = {
    assistantText: parsed?.assistantText,
    resultText: parsed?.resultText,
    stderr,
    toolNames: Array.isArray(parsed?.toolNames) ? parsed.toolNames.join('\n') : '',
  }
  const matches = []
  for (const [surface, text] of Object.entries(surfaces)) {
    for (const name of matchingPatterns(text)) {
      matches.push(`${surface}:${name}`)
    }
    for (const secret of knownSecrets) {
      const value = String(secret || '')
      if (value && String(text || '').includes(value)) {
        matches.push(`${surface}:known_secret`)
      }
    }
  }

  for (const name of matchingPatterns(stdout, { raw: true })) {
    matches.push(`stdoutTranscript:${name}`)
  }
  return [...new Set(matches)]
}
