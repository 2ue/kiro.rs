#!/usr/bin/env node

import assert from 'node:assert/strict'
import crypto from 'node:crypto'
import fs from 'node:fs'
import http from 'node:http'
import net from 'node:net'
import os from 'node:os'
import path from 'node:path'
import { spawn, spawnSync } from 'node:child_process'
import { performance } from 'node:perf_hooks'

import { resolveRuntimeValidationPaths } from './runtime-validation-paths.mjs'
import { validationChildEnvironment } from './validation-child-env.mjs'

const ROOT = path.resolve(import.meta.dirname, '../..')
const { binary: BINARY, artifactRoot: ARTIFACT_ROOT } = resolveRuntimeValidationPaths(ROOT)
const POSTGRES_URL = requiredEnvironment('KIRO_LONG_SESSION_POSTGRES_URL')
const REDIS_URL = requiredEnvironment('KIRO_LONG_SESSION_REDIS_URL')
const CLAUDE = process.env.KIRO_CLAUDE_BINARY || 'claude'
const ROUNDS = parseBoundedInteger('KIRO_LONG_SESSION_ROUNDS', 5, 1, 5)
const TOOL_CYCLES = parseBoundedInteger('KIRO_LONG_SESSION_TOOL_CYCLES', 20, 1, 100)
const RUN_ID = `long-session-${Date.now()}-${process.pid}-${crypto.randomBytes(3).toString('hex')}`
const TEMP_ROOT = fs.mkdtempSync(path.join(os.tmpdir(), `${RUN_ID}-`))
const REPORT_ROOT = path.join(ARTIFACT_ROOT, 'reports', 'claude-cli-long-session-continue')
const REPORT_PATH = path.join(REPORT_ROOT, `${RUN_ID}.json`)
const REQUEST_KEY = `sk-request-${RUN_ID}`
const ADMIN_KEY = `sk-admin-${RUN_ID}`
const KIRO_KEY = `ksk_${crypto.randomBytes(24).toString('hex')}|us-east-1`
const REDIS_PREFIX = `kiro_rs:validation:${RUN_ID}`
const ACTIVE_CHILDREN = new Set()
const ACTIVE_SERVERS = new Set()
let cleanupPromise = null

const LEAK_PATTERNS = [
  ['new_continue_transcript', /(?:^|\n)user Continue(?:\r?\n|$)/i],
  ['legacy_tool_results_transcript', /(?:^|\n)user Tool results provided\.?/i],
  ['tool_results_heading', /(?:^|\n)Tool results:\s*(?:\r?\n|$)/i],
  ['function_results_envelope', /<\/?function_results>/i],
  ['function_calls_envelope', /<\/?function_calls>/i],
  ['invoke_envelope', /<invoke\s+name=/i],
  ['known_hash_tool_name', /\b(?:bash|read|edit|write|glob|grep|websearch|webfetch|task)Hash[0-9a-f]{8}\b/i],
  ['generic_hash_tool_name', /\b[A-Za-z][A-Za-z0-9]{0,50}Hash[0-9a-f]{8}\b/],
]

function requiredEnvironment(name) {
  const value = String(process.env[name] || '').trim()
  if (!value) throw new Error(`${name} is required`)
  return value
}

function parseBoundedInteger(name, fallback, minimum, maximum) {
  const raw = process.env[name]
  const value = raw === undefined ? fallback : Number.parseInt(raw, 10)
  if (!Number.isSafeInteger(value) || value < minimum || value > maximum) {
    throw new Error(`${name} must be an integer in ${minimum}..${maximum}`)
  }
  return value
}

function sha256(value) {
  return crypto.createHash('sha256').update(value).digest('hex')
}

function sha256File(file) {
  return sha256(fs.readFileSync(file))
}

function commandOutput(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: ROOT,
    encoding: 'utf8',
    maxBuffer: 4 * 1024 * 1024,
    ...options,
  })
  if (result.status !== 0) {
    const detail = String(result.stderr || '').trim().slice(0, 600)
    const spawnError = result.error ? ` error=${String(result.error).slice(0, 300)}` : ''
    throw new Error(`${command} failed with status ${result.status}:${spawnError} ${detail}`)
  }
  return String(result.stdout || '').trim()
}

function redactDiagnosticText(value) {
  let text = String(value || '')
  for (const forbidden of [REQUEST_KEY, ADMIN_KEY, KIRO_KEY, POSTGRES_URL, REDIS_URL]) {
    text = text.split(forbidden).join('<redacted>')
  }
  if (text.length <= 3000) return text
  return `${text.slice(0, 1500)}\n<diagnostic_truncated chars=${text.length}>\n${text.slice(-1500)}`
}

function validateStorageUrls() {
  const postgres = new URL(POSTGRES_URL)
  if (!['postgres:', 'postgresql:'].includes(postgres.protocol)) {
    throw new Error('KIRO_LONG_SESSION_POSTGRES_URL must use PostgreSQL')
  }
  if (!['127.0.0.1', 'localhost', '::1'].includes(postgres.hostname)) {
    throw new Error('KIRO_LONG_SESSION_POSTGRES_URL must target loopback')
  }
  const database = decodeURIComponent(postgres.pathname.replace(/^\//, ''))
  if (!/^kiro_long_session_[a-z0-9_]{6,80}$/.test(database)) {
    throw new Error('KIRO_LONG_SESSION_POSTGRES_URL must name a caller-owned kiro_long_session_* database')
  }

  const redis = new URL(REDIS_URL)
  if (redis.protocol !== 'redis:') {
    throw new Error('KIRO_LONG_SESSION_REDIS_URL must use Redis')
  }
  if (!['127.0.0.1', 'localhost', '::1'].includes(redis.hostname)) {
    throw new Error('KIRO_LONG_SESSION_REDIS_URL must target loopback')
  }
  const redisPort = Number(redis.port || 6379)
  if (redisPort === 9022) throw new Error('KIRO_LONG_SESSION_REDIS_URL must not target port 9022')
}

function listeningPids(port) {
  const result = spawnSync(
    'lsof',
    ['-nP', `-iTCP:${port}`, '-sTCP:LISTEN', '-t'],
    { cwd: ROOT, encoding: 'utf8', maxBuffer: 1024 * 1024 },
  )
  if (result.status !== 0 && result.status !== 1) {
    throw new Error(`lsof failed while checking owned port ${port}`)
  }
  return String(result.stdout || '').split(/\s+/).filter(Boolean).map(Number).sort((a, b) => a - b)
}

async function reservePort() {
  for (;;) {
    const port = await new Promise((resolve, reject) => {
      const server = net.createServer()
      server.unref()
      server.once('error', reject)
      server.listen(0, '127.0.0.1', () => {
        const address = server.address()
        const selected = typeof address === 'object' && address ? address.port : 0
        server.close((error) => (error ? reject(error) : resolve(selected)))
      })
    })
    if (port !== 9022) return port
  }
}

async function waitForHealth(baseUrl, child, timeoutMs = 60_000) {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    if (child.exitCode !== null) {
      throw new Error(`kiro-rs exited before health check with status ${child.exitCode}`)
    }
    try {
      const response = await fetch(`${baseUrl}/healthz`)
      if (response.ok) return
    } catch {}
    await new Promise((resolve) => setTimeout(resolve, 100))
  }
  throw new Error('timed out waiting for isolated kiro-rs health check')
}

async function stopChild(child) {
  if (!child || child.exitCode !== null) return
  try { process.kill(-child.pid, 'SIGTERM') } catch { child.kill('SIGTERM') }
  const exited = await Promise.race([
    new Promise((resolve) => child.once('exit', () => resolve(true))),
    new Promise((resolve) => setTimeout(() => resolve(false), 8_000)),
  ])
  if (!exited && child.exitCode === null) {
    try { process.kill(-child.pid, 'SIGKILL') } catch { child.kill('SIGKILL') }
    await new Promise((resolve) => child.once('exit', resolve))
  }
  ACTIVE_CHILDREN.delete(child)
}

function crc32(bytes) {
  let crc = 0xffffffff
  for (const byte of bytes) {
    crc ^= byte
    for (let bit = 0; bit < 8; bit += 1) {
      const mask = -(crc & 1)
      crc = (crc >>> 1) ^ (0xedb88320 & mask)
    }
  }
  return (~crc) >>> 0
}

function encodeHeaders(headers) {
  const parts = []
  for (const [name, value] of Object.entries(headers)) {
    const nameBytes = Buffer.from(name)
    const valueBytes = Buffer.from(value)
    const length = Buffer.alloc(2)
    length.writeUInt16BE(valueBytes.length)
    parts.push(Buffer.from([nameBytes.length]), nameBytes, Buffer.from([7]), length, valueBytes)
  }
  return Buffer.concat(parts)
}

function eventFrame(eventType, payload) {
  const headers = encodeHeaders({
    ':message-type': 'event',
    ':event-type': eventType,
    ':content-type': 'application/json',
  })
  const body = Buffer.from(JSON.stringify(payload))
  const totalLength = 12 + headers.length + body.length + 4
  const frame = Buffer.alloc(totalLength)
  frame.writeUInt32BE(totalLength, 0)
  frame.writeUInt32BE(headers.length, 4)
  frame.writeUInt32BE(crc32(frame.subarray(0, 8)), 8)
  headers.copy(frame, 12)
  body.copy(frame, 12 + headers.length)
  frame.writeUInt32BE(crc32(frame.subarray(0, totalLength - 4)), totalLength - 4)
  return frame
}

function metadataFrame() {
  return eventFrame('metadataEvent', {
    tokenUsage: {
      uncachedInputTokens: 32,
      cacheReadInputTokens: 0,
      cacheWriteInputTokens: 0,
      outputTokens: 16,
      totalTokens: 48,
    },
  })
}

function writeJson(response, status, value) {
  const body = Buffer.from(JSON.stringify(value))
  response.writeHead(status, {
    'content-type': 'application/json',
    'content-length': body.length,
    connection: 'close',
  })
  response.end(body)
}

function writeEventStream(response, frames) {
  const body = Buffer.concat(frames)
  response.writeHead(200, {
    'content-type': 'application/vnd.amazon.eventstream',
    'content-length': body.length,
    connection: 'close',
  })
  response.end(body)
}

function stringValuesAtKey(value, key, output = []) {
  if (Array.isArray(value)) {
    for (const child of value) stringValuesAtKey(child, key, output)
  } else if (value && typeof value === 'object') {
    for (const [childKey, child] of Object.entries(value)) {
      if (childKey === key && typeof child === 'string') output.push(child)
      stringValuesAtKey(child, key, output)
    }
  }
  return output
}

function inspectWireBody(body, scenario) {
  const text = body.toString('utf8')
  let parsed = null
  try { parsed = JSON.parse(text) } catch {}
  const state = parsed?.conversationState
  const history = Array.isArray(state?.history) ? state.history : []
  const current = state?.currentMessage?.userInputMessage
  const tools = current?.userInputMessageContext?.tools
  const toolNames = Array.isArray(tools)
    ? tools.map((tool) => tool?.toolSpecification?.name).filter((name) => typeof name === 'string')
    : []
  const toolUseIds = parsed ? stringValuesAtKey(parsed, 'toolUseId') : []
  return {
    bodyBytes: body.length,
    bodySha256: sha256(body),
    validJson: parsed !== null,
    historyEntries: history.length,
    toolNames,
    currentUserPresent: text.includes(scenario.userMarker),
    firstUserPresent: text.includes(scenario.firstUserMarker),
    previousAssistantPresent: scenario.previousAssistantMarker
      ? text.includes(scenario.previousAssistantMarker)
      : null,
    toolOutputPresent: scenario.toolOutputMarker
      ? text.includes(scenario.toolOutputMarker)
      : null,
    toolUseIdPresent: scenario.toolUseId ? toolUseIds.includes(scenario.toolUseId) : null,
  }
}

function representsPublicTool(upstreamName, publicName) {
  const lower = publicName.toLowerCase()
  return upstreamName === publicName
    || upstreamName === lower
    || new RegExp(`^${lower}Hash[0-9a-f]{8}$`).test(upstreamName)
}

function createFakeUpstream() {
  let activeScenario = null
  const records = []
  const server = http.createServer((request, response) => {
    const chunks = []
    request.on('data', (chunk) => chunks.push(chunk))
    request.on('end', () => {
      const body = Buffer.concat(chunks)
      const target = String(request.headers['x-amz-target'] || '')
      const url = new URL(request.url || '/', 'http://127.0.0.1')
      if (target.endsWith('.ListAvailableModels')) {
        records.push({ kind: 'model_discovery', scenario: activeScenario?.id || null })
        writeJson(response, 200, {
          models: [{
            modelId: 'claude-sonnet-4',
            modelName: 'Long session fixture',
            supportedInputTypes: ['text'],
            tokenLimits: { maxInputTokens: 200000, maxOutputTokens: 8192 },
          }],
          nextToken: null,
        })
        return
      }
      if (url.pathname.endsWith('/getUsageLimits')) {
        records.push({ kind: 'balance', scenario: activeScenario?.id || null })
        writeJson(response, 200, {
          subscriptionInfo: {
            subscriptionTitle: 'LONG SESSION FIXTURE',
            overageCapability: 'OVERAGE_CAPABLE',
          },
          overageConfiguration: { overageStatus: 'DISABLED' },
          usageBreakdownList: [{
            currentUsageWithPrecision: 1,
            usageLimitWithPrecision: 100,
            overageCap: 0,
            overageRate: 0,
            currentOverages: 0,
          }],
        })
        return
      }
      if (!target.endsWith('.GenerateAssistantResponse') || !activeScenario) {
        records.push({ kind: 'unknown', scenario: activeScenario?.id || null })
        writeJson(response, 404, { message: 'unsupported isolated fixture request' })
        return
      }

      activeScenario.inferenceHits += 1
      const inspection = inspectWireBody(body, activeScenario)
      records.push({
        kind: 'inference',
        scenario: activeScenario.id,
        ordinal: activeScenario.inferenceHits,
        ...inspection,
      })

      if (activeScenario.toolName && activeScenario.inferenceHits === 1) {
        activeScenario.upstreamToolName = inspection.toolNames.find((name) => (
          representsPublicTool(name, activeScenario.toolName)
        )) || null
        if (!activeScenario.upstreamToolName) {
          writeJson(response, 500, { message: 'public tool has no Kiro wire mapping' })
          return
        }
        writeEventStream(response, [
          eventFrame('toolUseEvent', {
            name: activeScenario.upstreamToolName,
            toolUseId: activeScenario.toolUseId,
            input: JSON.stringify(activeScenario.toolInput),
            stop: true,
          }),
          metadataFrame(),
        ])
        return
      }

      writeEventStream(response, [
        eventFrame('assistantResponseEvent', {
          content: activeScenario.assistantMarker,
          messageStatus: 'COMPLETED',
        }),
        metadataFrame(),
      ])
    })
  })

  return {
    records,
    setScenario(scenario) { activeScenario = scenario },
    async listen(port) {
      await new Promise((resolve, reject) => {
        server.once('error', reject)
        server.listen(port, '127.0.0.1', resolve)
      })
    },
    async close() {
      if (!server.listening) return
      await new Promise((resolve) => server.close(resolve))
    },
  }
}

function encodeRedisCommand(argumentsList) {
  const parts = [Buffer.from(`*${argumentsList.length}\r\n`)]
  for (const argument of argumentsList) {
    const value = Buffer.from(String(argument))
    parts.push(Buffer.from(`$${value.length}\r\n`), value, Buffer.from('\r\n'))
  }
  return Buffer.concat(parts)
}

function parseRedisResponse(buffer, offset = 0) {
  if (offset >= buffer.length) return null
  const type = String.fromCharCode(buffer[offset])
  const lineEnd = buffer.indexOf('\r\n', offset + 1)
  if (lineEnd < 0) return null
  const line = buffer.subarray(offset + 1, lineEnd).toString()
  if (type === '+' || type === ':' || type === '-') {
    return { value: type === ':' ? Number(line) : line, error: type === '-', next: lineEnd + 2 }
  }
  if (type === '$') {
    const length = Number(line)
    if (length === -1) return { value: null, error: false, next: lineEnd + 2 }
    const start = lineEnd + 2
    const end = start + length
    if (buffer.length < end + 2) return null
    return { value: buffer.subarray(start, end).toString(), error: false, next: end + 2 }
  }
  if (type === '*') {
    const count = Number(line)
    let cursor = lineEnd + 2
    const values = []
    for (let index = 0; index < count; index += 1) {
      const parsed = parseRedisResponse(buffer, cursor)
      if (!parsed) return null
      if (parsed.error) return parsed
      values.push(parsed.value)
      cursor = parsed.next
    }
    return { value: values, error: false, next: cursor }
  }
  throw new Error(`unsupported Redis response type ${type}`)
}

async function redisPipeline(commands) {
  const url = new URL(REDIS_URL)
  const database = Number.parseInt(url.pathname.replace(/^\//, '') || '0', 10)
  const pipeline = []
  if (url.password) {
    pipeline.push(url.username
      ? ['AUTH', decodeURIComponent(url.username), decodeURIComponent(url.password)]
      : ['AUTH', decodeURIComponent(url.password)])
  }
  if (database !== 0) pipeline.push(['SELECT', database])
  pipeline.push(...commands)
  const payload = Buffer.concat(pipeline.map(encodeRedisCommand))

  return await new Promise((resolve, reject) => {
    const socket = net.connect({ host: url.hostname, port: Number(url.port || 6379) })
    const chunks = []
    socket.setTimeout(5_000)
    socket.once('connect', () => socket.write(payload))
    socket.on('data', (chunk) => {
      chunks.push(chunk)
      const buffer = Buffer.concat(chunks)
      let cursor = 0
      const values = []
      for (let index = 0; index < pipeline.length; index += 1) {
        const parsed = parseRedisResponse(buffer, cursor)
        if (!parsed) return
        if (parsed.error) {
          socket.destroy()
          reject(new Error(`Redis command failed: ${parsed.value}`))
          return
        }
        values.push(parsed.value)
        cursor = parsed.next
      }
      socket.end()
      resolve(values)
    })
    socket.once('timeout', () => {
      socket.destroy()
      reject(new Error('Redis cleanup timed out'))
    })
    socket.once('error', reject)
  })
}

async function removeOwnedRedisKeys(maxPasses = 4) {
  const script = `
    local cursor = ARGV[2]
    local removed = 0
    for page = 1, 64 do
      local result = redis.call('SCAN', cursor, 'MATCH', ARGV[1], 'COUNT', 200)
      cursor = result[1]
      local keys = result[2]
      if #keys > 0 then
        removed = removed + redis.call('DEL', unpack(keys))
      end
      if cursor == '0' then break end
    end
    return {removed, cursor}
  `
  let cursor = '0'
  let removed = 0
  for (let pass = 1; pass <= maxPasses; pass += 1) {
    const values = await redisPipeline([
      ['EVAL', script, 0, `${REDIS_PREFIX}:*`, cursor],
    ])
    const result = values.at(-1)
    if (!Array.isArray(result) || result.length !== 2) {
      throw new Error('Redis cleanup returned an invalid bounded-scan result')
    }
    removed += Number(result[0] || 0)
    cursor = String(result[1])
    if (cursor === '0') return { complete: true, removed, passes: pass }
  }
  return { complete: false, removed, passes: maxPasses }
}

async function cleanupOwnedRuntime({ redisPasses = 4 } = {}) {
  if (cleanupPromise) return cleanupPromise
  cleanupPromise = (async () => {
    await Promise.all([...ACTIVE_CHILDREN].map((child) => stopChild(child)))
    await Promise.all([...ACTIVE_SERVERS].map(async (server) => {
      await Promise.race([
        server.close().catch(() => {}),
        new Promise((resolve) => setTimeout(resolve, 2_000)),
      ])
      ACTIVE_SERVERS.delete(server)
    }))
    const redis = await removeOwnedRedisKeys(redisPasses).catch(() => ({
      complete: false,
      removed: 0,
      passes: 0,
    }))
    fs.rmSync(TEMP_ROOT, { recursive: true, force: true })
    return {
      childGroupsStopped: ACTIVE_CHILDREN.size === 0,
      serversStopped: ACTIVE_SERVERS.size === 0,
      redis,
      tempRemoved: !fs.existsSync(TEMP_ROOT),
    }
  })()
  return cleanupPromise
}

const SIGNAL_EXIT_CODES = new Map([['SIGHUP', 129], ['SIGINT', 130], ['SIGTERM', 143]])
let handlingSignal = false
for (const [signal, exitCode] of SIGNAL_EXIT_CODES) {
  process.on(signal, () => {
    if (handlingSignal) return
    handlingSignal = true
    const hardExit = setTimeout(() => process.exit(exitCode), 15_000)
    hardExit.unref()
    void cleanupOwnedRuntime({ redisPasses: 1 }).finally(() => {
      clearTimeout(hardExit)
      process.exit(exitCode)
    })
  })
}

function numericValues(value, output = []) {
  if (typeof value === 'number' && Number.isFinite(value)) output.push(value)
  else if (Array.isArray(value)) value.forEach((child) => numericValues(child, output))
  else if (value && typeof value === 'object') {
    Object.values(value).forEach((child) => numericValues(child, output))
  }
  return output
}

function parseClaudeJsonl(stdout) {
  const toolUseIds = new Set()
  const toolResultIds = new Set()
  const toolNames = new Set()
  const sessionIds = new Set()
  const assistantText = []
  let finalUsage = null
  let resultText = ''
  for (const line of stdout.split('\n')) {
    if (!line.trim()) continue
    let value
    try { value = JSON.parse(line) } catch { continue }
    if (typeof value?.session_id === 'string') sessionIds.add(value.session_id)
    const content = value?.message?.content
    if (Array.isArray(content)) {
      for (const block of content) {
        if (block?.type === 'tool_use' && block.id) {
          toolUseIds.add(block.id)
          if (typeof block.name === 'string') toolNames.add(block.name)
        }
        if (block?.type === 'tool_result' && block.tool_use_id) toolResultIds.add(block.tool_use_id)
        if (block?.type === 'text' && typeof block.text === 'string') assistantText.push(block.text)
      }
    }
    if (value?.type === 'result') {
      if (typeof value.result === 'string') resultText = value.result
      if (value.usage && typeof value.usage === 'object') finalUsage = value.usage
    }
  }
  return {
    toolUseIds: [...toolUseIds],
    toolResultIds: [...toolResultIds],
    toolNames: [...toolNames],
    sessionIds: [...sessionIds],
    assistantText: assistantText.join(''),
    resultText,
    finalUsage,
    hasNonzeroUsage: numericValues(finalUsage).some((value) => value > 0),
  }
}

function detectLeaks(parsed, stderr) {
  const surfaces = {
    assistantText: parsed.assistantText,
    resultText: parsed.resultText,
    stderr,
    toolNames: parsed.toolNames.join('\n'),
  }
  const matches = []
  for (const [surface, text] of Object.entries(surfaces)) {
    for (const [name, pattern] of LEAK_PATTERNS) {
      if (pattern.test(text)) matches.push(`${surface}:${name}`)
    }
  }
  return matches
}

async function runClaude({ baseUrl, sessionRoot, prompt, firstTurn, sessionId }) {
  const home = path.join(sessionRoot, 'home')
  const configDir = path.join(sessionRoot, 'config')
  const projectRoot = path.join(sessionRoot, 'project')
  fs.mkdirSync(home, { recursive: true, mode: 0o700 })
  fs.mkdirSync(configDir, { recursive: true, mode: 0o700 })
  fs.mkdirSync(projectRoot, { recursive: true, mode: 0o700 })
  const environment = validationChildEnvironment({
    HOME: home,
    CLAUDE_CONFIG_DIR: configDir,
    ANTHROPIC_BASE_URL: `${baseUrl}/cc`,
    ANTHROPIC_API_KEY: REQUEST_KEY,
    CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: '1',
    DISABLE_AUTOUPDATER: '1',
    DISABLE_ERROR_REPORTING: '1',
    DISABLE_TELEMETRY: '1',
    CI: '1',
    TERM: 'dumb',
  })
  delete environment.CLAUDECODE
  const args = [
    '--bare',
    '--print',
    '--verbose',
    '--output-format=stream-json',
    '--include-partial-messages',
    '--prompt-suggestions',
    'false',
    '--disable-slash-commands',
    '--model',
    'sonnet',
    '--tools',
    'Bash,Read',
    '--dangerously-skip-permissions',
  ]
  if (firstTurn) args.push('--session-id', sessionId)
  else args.push('--continue')
  args.push('--', prompt)

  const started = performance.now()
  const child = spawn(CLAUDE, args, {
    cwd: projectRoot,
    env: environment,
    detached: true,
    stdio: ['ignore', 'pipe', 'pipe'],
  })
  ACTIVE_CHILDREN.add(child)
  const stdout = []
  const stderr = []
  let bytes = 0
  const capture = (target) => (chunk) => {
    bytes += chunk.length
    if (bytes > 16 * 1024 * 1024) {
      try { process.kill(-child.pid, 'SIGTERM') } catch {}
      return
    }
    target.push(chunk)
  }
  child.stdout.on('data', capture(stdout))
  child.stderr.on('data', capture(stderr))

  let timedOut = false
  const timer = setTimeout(() => {
    timedOut = true
    try { process.kill(-child.pid, 'SIGTERM') } catch {}
    setTimeout(() => {
      if (child.exitCode === null) {
        try { process.kill(-child.pid, 'SIGKILL') } catch {}
      }
    }, 2_000).unref()
  }, 90_000)
  const exit = await new Promise((resolve) => {
    child.once('exit', (code, signal) => resolve({ code, signal }))
    child.once('error', (error) => resolve({ code: null, signal: null, error }))
  })
  clearTimeout(timer)
  ACTIVE_CHILDREN.delete(child)
  const stdoutText = Buffer.concat(stdout).toString('utf8')
  const stderrText = Buffer.concat(stderr).toString('utf8')
  return {
    ...exit,
    timedOut,
    durationMs: Number((performance.now() - started).toFixed(2)),
    stdoutText,
    stderrText,
    stdoutSha256: sha256(stdoutText),
    stderrSha256: sha256(stderrText),
  }
}

function isolatedConfig({ servicePort, upstreamPort }) {
  return {
    postgres: { url: POSTGRES_URL, maxConnections: 4, migrateOnStart: true },
    redis: { url: REDIS_URL, keyPrefix: REDIS_PREFIX },
    host: '127.0.0.1',
    port: servicePort,
    apiKey: REQUEST_KEY,
    adminApiKey: ADMIN_KEY,
    defaultEndpoint: 'cli',
    kiroUpstreamBaseUrl: `http://127.0.0.1:${upstreamPort}/fixture`,
    kiroUpstreamResponseTimeoutSecs: 10,
    credentialRetryMaxAttempts: 1,
    inferenceUpstreamMaxAttempts: 1,
    credentialWarmupRequests: 0,
    externalPoolsEnabled: false,
  }
}

function createScenario({ round, turn, firstUserMarker, previousAssistantMarker, toolName, projectRoot }) {
  const base = `LONGSESSION_R${round}_T${turn}`
  const toolOutputMarker = toolName ? `${base}_${toolName.toUpperCase()}_OUTPUT` : null
  const toolUseId = toolName ? `toolu_long_r${round}_t${turn}` : null
  const toolInput = toolName === 'Bash'
    ? { command: `printf '%s' '${toolOutputMarker}'` }
    : toolName === 'Read'
      ? { file_path: path.join(projectRoot, 'read-fixture.txt') }
      : null
  return {
    id: `round-${round}-turn-${turn}`,
    round,
    turn,
    firstUserMarker,
    previousAssistantMarker,
    userMarker: `${base}_USER`,
    assistantMarker: `${base}_ASSISTANT_OK`,
    toolName,
    toolUseId,
    toolInput,
    toolOutputMarker,
    upstreamToolName: null,
    inferenceHits: 0,
  }
}

function assertScenarioWire(records, scenario) {
  const inference = records.filter((record) => (
    record.kind === 'inference' && record.scenario === scenario.id
  ))
  const expectedHits = scenario.toolName ? 2 : 1
  assert.equal(inference.length, expectedHits, `${scenario.id}: unexpected inference count`)
  for (const record of inference) {
    assert.equal(record.validJson, true, `${scenario.id}: Kiro wire body is not JSON`)
    assert.equal(record.currentUserPresent, true, `${scenario.id}: current user marker missing on wire`)
    if (scenario.turn > 1) {
      assert.equal(record.previousAssistantPresent, true, `${scenario.id}: previous assistant turn missing on wire`)
    }
    assert.equal(
      record.toolNames.some((name) => representsPublicTool(name, 'Bash'))
        && record.toolNames.some((name) => representsPublicTool(name, 'Read')),
      true,
      `${scenario.id}: current tool catalog lost Bash or Read; wire=${JSON.stringify(record.toolNames)}`,
    )
  }
  if (scenario.toolName) {
    assert.equal(
      representsPublicTool(scenario.upstreamToolName || '', scenario.toolName),
      true,
      `${scenario.id}: fake upstream did not use the request-local tool mapping`,
    )
    assert.equal(inference[0].toolOutputPresent, false, `${scenario.id}: tool output appeared before execution`)
    assert.equal(inference[1].toolOutputPresent, true, `${scenario.id}: tool output missing from follow-up wire body`)
    assert.equal(inference[1].toolUseIdPresent, true, `${scenario.id}: paired tool ID missing from follow-up wire body`)
    assert.ok(inference[1].historyEntries > inference[0].historyEntries, `${scenario.id}: tool history did not advance`)
  }
  return inference
}

async function main() {
  validateStorageUrls()
  const servicePort = await reservePort()
  const upstreamPort = await reservePort()
  assert.notEqual(servicePort, 9022)
  assert.notEqual(upstreamPort, 9022)
  assert.deepEqual(listeningPids(servicePort), [])
  assert.deepEqual(listeningPids(upstreamPort), [])

  const fake = createFakeUpstream()
  ACTIVE_SERVERS.add(fake)
  let service = null
  let report = null
  const cleanup = {
    childGroupsStopped: false,
    serviceStopped: false,
    fakeStopped: false,
    redisKeysRemoved: false,
    tempRemoved: false,
    portsReleased: false,
    protected9022ProbeSkipped: true,
  }

  try {
    await fake.listen(upstreamPort)
    const configPath = path.join(TEMP_ROOT, 'config.json')
    const credentialsPath = path.join(TEMP_ROOT, 'missing-credentials.json')
    const serviceLogPath = path.join(TEMP_ROOT, 'service.log')
    fs.writeFileSync(
      configPath,
      `${JSON.stringify(isolatedConfig({ servicePort, upstreamPort }), null, 2)}\n`,
      { mode: 0o600 },
    )
    const serviceLog = fs.openSync(serviceLogPath, 'a', 0o600)
    service = spawn(BINARY, ['--config', configPath, '--credentials', credentialsPath], {
      cwd: ROOT,
      env: validationChildEnvironment({
        KIRO_API_KEY: KIRO_KEY,
        KIRO_RS_HOST: '127.0.0.1',
        KIRO_RS_PORT: String(servicePort),
        RUST_LOG: 'warn',
      }),
      stdio: ['ignore', serviceLog, serviceLog],
      detached: true,
    })
    ACTIVE_CHILDREN.add(service)
    service.once('exit', () => {
      ACTIVE_CHILDREN.delete(service)
      fs.closeSync(serviceLog)
    })
    const baseUrl = `http://127.0.0.1:${servicePort}`
    await waitForHealth(baseUrl, service)
    assert.deepEqual(listeningPids(servicePort), [service.pid])

    const cliVersion = commandOutput(CLAUDE, ['--version'])
    const sessions = []
    for (let round = 1; round <= ROUNDS; round += 1) {
      const sessionRoot = path.join(TEMP_ROOT, `session-${round}`)
      const projectRoot = path.join(sessionRoot, 'project')
      fs.mkdirSync(projectRoot, { recursive: true, mode: 0o700 })
      const sessionId = crypto.randomUUID()
      const turns = []
      let previousAssistantMarker = null
      let firstUserMarker = null

      for (let turn = 1; turn <= TOOL_CYCLES + 2; turn += 1) {
        const toolName = turn === 1 || turn === TOOL_CYCLES + 2
          ? null
          : turn % 2 === 0 ? 'Bash' : 'Read'
        const scenario = createScenario({
          round,
          turn,
          firstUserMarker: firstUserMarker || `LONGSESSION_R${round}_T1_USER`,
          previousAssistantMarker,
          toolName,
          projectRoot,
        })
        if (turn === 1) firstUserMarker = scenario.userMarker
        if (toolName === 'Read') {
          fs.writeFileSync(
            path.join(projectRoot, 'read-fixture.txt'),
            `${scenario.toolOutputMarker}\n`,
            { mode: 0o600 },
          )
        }
        fake.setScenario(scenario)
        const cli = await runClaude({
          baseUrl,
          sessionRoot,
          prompt: `${scenario.userMarker}. Return the upstream fixture response exactly.`,
          firstTurn: turn === 1,
          sessionId,
        })
        const parsed = parseClaudeJsonl(cli.stdoutText)
        const leaks = detectLeaks(parsed, cli.stderrText)
        assert.equal(cli.timedOut, false, `${scenario.id}: Claude CLI timed out`)
        assert.equal(
          cli.code,
          0,
          `${scenario.id}: Claude CLI exit code=${cli.code} signal=${cli.signal}; `
            + `stdout=${JSON.stringify(redactDiagnosticText(cli.stdoutText))}; `
            + `stderr=${JSON.stringify(redactDiagnosticText(cli.stderrText))}`,
        )
        assert.equal(parsed.hasNonzeroUsage, true, `${scenario.id}: final usage missing or all zero`)
        assert.equal(parsed.sessionIds.length, 1, `${scenario.id}: expected one CLI session ID`)
        assert.equal(parsed.sessionIds[0], sessionId, `${scenario.id}: --continue changed the session ID`)
        assert.equal(parsed.assistantText.includes(scenario.assistantMarker), true, `${scenario.id}: final marker missing`)
        assert.deepEqual(leaks, [], `${scenario.id}: internal transcript marker leaked: ${leaks.join(', ')}`)

        if (toolName) {
          assert.equal(parsed.toolUseIds.length, 1, `${scenario.id}: expected one tool_use`)
          assert.equal(parsed.toolResultIds.length, 1, `${scenario.id}: expected one tool_result`)
          assert.deepEqual(parsed.toolNames, [toolName], `${scenario.id}: wrong public tool name`)
          assert.equal(parsed.toolUseIds[0], scenario.toolUseId, `${scenario.id}: tool_use ID changed`)
          assert.equal(parsed.toolResultIds[0], scenario.toolUseId, `${scenario.id}: tool_result ID changed`)
          assert.equal(scenario.inferenceHits, 2, `${scenario.id}: tool loop did not make two inference calls`)
        } else {
          assert.equal(parsed.toolUseIds.length, 0, `${scenario.id}: text turn emitted tool_use`)
          assert.equal(parsed.toolResultIds.length, 0, `${scenario.id}: text turn emitted tool_result`)
          assert.equal(scenario.inferenceHits, 1, `${scenario.id}: text turn made extra inference calls`)
        }

        const wire = assertScenarioWire(fake.records, scenario)
        turns.push({
          id: scenario.id,
          turn,
          invocation: turn === 1 ? 'session-id' : 'continue',
          toolName,
          upstreamToolName: scenario.upstreamToolName,
          exitCode: cli.code,
          durationMs: cli.durationMs,
          inferenceHits: scenario.inferenceHits,
          toolUseCount: parsed.toolUseIds.length,
          toolResultCount: parsed.toolResultIds.length,
          sessionIdSha256: sha256(sessionId),
          hasNonzeroUsage: parsed.hasNonzeroUsage,
          usage: parsed.finalUsage,
          leakMatches: leaks,
          firstWireBodyBytes: wire[0].bodyBytes,
          lastWireBodyBytes: wire.at(-1).bodyBytes,
          historyEntries: wire.at(-1).historyEntries,
          stdoutSha256: cli.stdoutSha256,
          stderrSha256: cli.stderrSha256,
        })
        previousAssistantMarker = scenario.assistantMarker
      }

      assert.equal(turns.filter((turn) => turn.invocation === 'session-id').length, 1)
      assert.equal(turns.filter((turn) => turn.invocation === 'continue').length, TOOL_CYCLES + 1)
      assert.ok(turns.at(-1).historyEntries > turns[0].historyEntries, `round ${round}: history did not grow`)
      sessions.push({
        round,
        sessionIdSha256: sha256(sessionId),
        turns,
        firstHistoryEntries: turns[0].historyEntries,
        finalHistoryEntries: turns.at(-1).historyEntries,
        firstWireBodyBytes: turns[0].firstWireBodyBytes,
        finalWireBodyBytes: turns.at(-1).lastWireBodyBytes,
      })
    }

    const allTurns = sessions.flatMap((session) => session.turns)
    report = {
      schemaVersion: 1,
      result: 'pass',
      runId: RUN_ID,
      gateQualified: ROUNDS === 5 && [20, 100].includes(TOOL_CYCLES),
      gitHead: commandOutput('git', ['rev-parse', 'HEAD']),
      dirty: commandOutput('git', ['status', '--short']).length > 0,
      binarySha256: sha256File(BINARY),
      claudeCliVersion: cliVersion,
      rounds: ROUNDS,
      toolCyclesPerSession: TOOL_CYCLES,
      sessions,
      totals: {
        sessions: sessions.length,
        cliTurns: allTurns.length,
        continueTurns: allTurns.filter((turn) => turn.invocation === 'continue').length,
        toolTurns: allTurns.filter((turn) => turn.toolName !== null).length,
        bashTurns: allTurns.filter((turn) => turn.toolName === 'Bash').length,
        readTurns: allTurns.filter((turn) => turn.toolName === 'Read').length,
        inferenceHits: allTurns.reduce((sum, turn) => sum + turn.inferenceHits, 0),
        toolUseCount: allTurns.reduce((sum, turn) => sum + turn.toolUseCount, 0),
        toolResultCount: allTurns.reduce((sum, turn) => sum + turn.toolResultCount, 0),
        leakMatches: allTurns.reduce((sum, turn) => sum + turn.leakMatches.length, 0),
        fakeModelDiscoveryRequests: fake.records.filter((record) => record.kind === 'model_discovery').length,
        fakeUnknownRequests: fake.records.filter((record) => record.kind === 'unknown').length,
      },
      isolation: {
        servicePort,
        upstreamPort,
        forbiddenPorts: [9022],
        protected9022ProbeSkipped: true,
        isolatedHomePerSession: true,
        isolatedClaudeConfigDirPerSession: true,
        isolatedProjectPerSession: true,
        fakeKiroCredential: true,
        callerOwnedPostgresDatabase: true,
        ownedRedisPrefixSha256: sha256(REDIS_PREFIX),
      },
      callerResponsibilities: {
        postgresDatabaseMustBeCreatedEmptyBeforeRun: true,
        postgresDatabaseMustBeDroppedAfterRun: true,
        runnerNeverDropsOrReusesTheCallerDatabase: true,
      },
      cleanup,
    }
  } finally {
    const owned = await cleanupOwnedRuntime({ redisPasses: 4 })
    cleanup.childGroupsStopped = owned.childGroupsStopped
    cleanup.serviceStopped = service === null || service.exitCode !== null
    cleanup.fakeStopped = owned.serversStopped
    cleanup.redisKeysRemoved = owned.redis.complete
    cleanup.tempRemoved = owned.tempRemoved
    cleanup.portsReleased = listeningPids(servicePort).length === 0
      && listeningPids(upstreamPort).length === 0

    if (report) {
      report.cleanup = cleanup
      report.result = Object.values(cleanup).every(Boolean) ? 'pass' : 'fail'
      fs.mkdirSync(REPORT_ROOT, { recursive: true })
      const serialized = `${JSON.stringify(report, null, 2)}\n`
      for (const forbidden of [REQUEST_KEY, ADMIN_KEY, KIRO_KEY, POSTGRES_URL, REDIS_URL, TEMP_ROOT]) {
        assert.equal(serialized.includes(forbidden), false, 'report contains a secret, URL, or temp path')
      }
      fs.writeFileSync(REPORT_PATH, serialized, { mode: 0o600 })
    }
  }

  assert.ok(report)
  assert.equal(report.result, 'pass')
  assert.equal(report.totals.sessions, ROUNDS)
  assert.equal(report.totals.cliTurns, ROUNDS * (TOOL_CYCLES + 2))
  assert.equal(report.totals.toolTurns, ROUNDS * TOOL_CYCLES)
  assert.equal(report.totals.toolUseCount, ROUNDS * TOOL_CYCLES)
  assert.equal(report.totals.toolResultCount, ROUNDS * TOOL_CYCLES)
  assert.equal(report.totals.leakMatches, 0)
  assert.equal(report.totals.fakeUnknownRequests, 0)
  process.stdout.write(`${REPORT_PATH}\n`)
}

main().catch(async (error) => {
  await cleanupOwnedRuntime({ redisPasses: 1 }).catch(() => {})
  process.stderr.write(`Claude CLI long-session validation failed: ${error.message}\n`)
  process.exitCode = 1
})
