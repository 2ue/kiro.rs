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
const SIGNAL_FIXTURE_MODE = process.env.KIRO_BARE_INVOKE_SIGNAL_FIXTURE === '1'
const runtimePaths = SIGNAL_FIXTURE_MODE ? null : resolveRuntimeValidationPaths(ROOT)
const BINARY = runtimePaths?.binary || ''
const ARTIFACT_ROOT = runtimePaths?.artifactRoot || ''
const POSTGRES_URL = SIGNAL_FIXTURE_MODE
  ? 'postgres://signal-fixture.invalid/owned'
  : requiredEnvironment('KIRO_BARE_INVOKE_POSTGRES_URL')
let REDIS_URL = SIGNAL_FIXTURE_MODE ? '' : requiredEnvironment('KIRO_BARE_INVOKE_REDIS_URL')
const ROUNDS = SIGNAL_FIXTURE_MODE
  ? 5
  : Number.parseInt(process.env.KIRO_BARE_INVOKE_ROUNDS || '5', 10)
const CLAUDE = process.env.KIRO_CLAUDE_BINARY || 'claude'
const RUN_ID = `bare-invoke-${Date.now()}-${process.pid}-${crypto.randomBytes(3).toString('hex')}`
const TEMP_ROOT = fs.mkdtempSync(path.join(os.tmpdir(), `${RUN_ID}-`))
const REPORT_ROOT = SIGNAL_FIXTURE_MODE
  ? ''
  : path.join(ARTIFACT_ROOT, 'reports', 'bare-invoke-claude-cli')
const REPORT_PATH = SIGNAL_FIXTURE_MODE ? '' : path.join(REPORT_ROOT, `${RUN_ID}.json`)
const REQUEST_KEY = `sk-request-${RUN_ID}`
const ADMIN_KEY = `sk-admin-${RUN_ID}`
const KIRO_KEY = `ksk_${crypto.randomBytes(24).toString('hex')}|us-east-1`
const REDIS_PREFIX = `kiro_rs:validation:${RUN_ID}`
const ACTIVE_CHILDREN = new Set()
const ACTIVE_FAKE_SERVERS = new Set()
let ownedCleanupPromise = null

if (ROUNDS !== 5) {
  throw new Error('KIRO_BARE_INVOKE_ROUNDS must be exactly 5 for the C2 gate')
}

function requiredEnvironment(name) {
  const value = String(process.env[name] || '').trim()
  if (!value) throw new Error(`${name} is required`)
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
    const detail = String(result.stderr || '').trim().slice(0, 500)
    const spawnError = result.error ? ` error=${String(result.error).slice(0, 300)}` : ''
    throw new Error(`${command} failed with status ${result.status}:${spawnError} ${detail}`)
  }
  return String(result.stdout || '').trim()
}

function redactDiagnosticText(value) {
  let text = String(value || '')
  for (const forbidden of [REQUEST_KEY, ADMIN_KEY, KIRO_KEY, POSTGRES_URL, REDIS_URL]) {
    if (!forbidden) continue
    text = text.split(forbidden).join('<redacted>')
  }
  if (text.length <= 2400) return text
  return `${text.slice(0, 1200)}\n<diagnostic_truncated chars=${text.length}>\n${text.slice(-1200)}`
}

function listeningPids(port) {
  const result = spawnSync(
    'lsof',
    ['-nP', `-iTCP:${port}`, '-sTCP:LISTEN', '-t'],
    { cwd: ROOT, encoding: 'utf8', maxBuffer: 1024 * 1024 },
  )
  if (result.status !== 0 && result.status !== 1) {
    throw new Error(`lsof failed while checking port ${port}`)
  }
  return String(result.stdout || '')
    .split(/\s+/)
    .filter(Boolean)
    .map(Number)
    .sort((left, right) => left - right)
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
  try {
    process.kill(-child.pid, 'SIGTERM')
  } catch {
    child.kill('SIGTERM')
  }
  const exited = await Promise.race([
    new Promise((resolve) => child.once('exit', () => resolve(true))),
    new Promise((resolve) => setTimeout(() => resolve(false), 8_000)),
  ])
  if (!exited && child.exitCode === null) {
    try {
      process.kill(-child.pid, 'SIGKILL')
    } catch {
      child.kill('SIGKILL')
    }
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

function metadataFrame() {
  return eventFrame('metadataEvent', {
    tokenUsage: {
      uncachedInputTokens: 24,
      cacheReadInputTokens: 0,
      cacheWriteInputTokens: 0,
      outputTokens: 16,
      totalTokens: 40,
    },
  })
}

function createFakeUpstream() {
  let activeScenario = null
  const records = []
  const server = http.createServer((request, response) => {
    const chunks = []
    request.on('data', (chunk) => chunks.push(chunk))
    request.on('end', () => {
      const target = String(request.headers['x-amz-target'] || '')
      const url = new URL(request.url || '/', 'http://127.0.0.1')
      if (target.endsWith('.ListAvailableModels')) {
        records.push({ kind: 'model_discovery', scenario: activeScenario?.id || null })
        writeJson(response, 200, {
          models: [{
            modelId: 'claude-sonnet-4',
            modelName: 'CLI protocol fixture',
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
            subscriptionTitle: 'CLI PROTOCOL FIXTURE',
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
      records.push({
        kind: 'inference',
        scenario: activeScenario.id,
        ordinal: activeScenario.inferenceHits,
        bodyBytes: Buffer.concat(chunks).length,
      })
      if (activeScenario.mode === 'structured' && activeScenario.inferenceHits === 1) {
        writeEventStream(response, [
          eventFrame('toolUseEvent', {
            name: 'Bash',
            toolUseId: `toolu_${activeScenario.id}`,
            input: JSON.stringify({ command: 'printf structured-ok' }),
            stop: true,
          }),
          metadataFrame(),
        ])
        return
      }

      const content = activeScenario.mode === 'structured'
        ? 'structured-finished'
        : activeScenario.inferenceHits === 1
          ? activeScenario.content
          : 'unexpected-negative-followup'
      writeEventStream(response, [
        eventFrame('assistantResponseEvent', {
          content,
          messageStatus: 'COMPLETED',
        }),
        metadataFrame(),
      ])
    })
  })

  return {
    records,
    setScenario(scenario) {
      activeScenario = scenario
    },
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

async function redisPipeline(urlValue, commands) {
  const url = new URL(urlValue)
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
  if (!REDIS_URL) return { complete: false, removed: 0, passes: 0 }
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
      if cursor == '0' then
        break
      end
    end
    return {removed, cursor}
  `
  let cursor = '0'
  let removed = 0
  for (let pass = 1; pass <= maxPasses; pass += 1) {
    const values = await redisPipeline(REDIS_URL, [
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
  if (ownedCleanupPromise) return ownedCleanupPromise
  ownedCleanupPromise = (async () => {
    await Promise.all([...ACTIVE_CHILDREN].map((child) => stopChild(child)))
    await Promise.all([...ACTIVE_FAKE_SERVERS].map(async (server) => {
      await Promise.race([
        server.close().catch(() => {}),
        new Promise((resolve) => setTimeout(resolve, 2_000)),
      ])
      ACTIVE_FAKE_SERVERS.delete(server)
    }))
    const redis = await removeOwnedRedisKeys(redisPasses).catch(() => ({
      complete: false,
      removed: 0,
      passes: 0,
    }))
    fs.rmSync(TEMP_ROOT, { recursive: true, force: true })
    return {
      childrenStopped: ACTIVE_CHILDREN.size === 0,
      fakeServersStopped: ACTIVE_FAKE_SERVERS.size === 0,
      redis,
      tempRemoved: !fs.existsSync(TEMP_ROOT),
    }
  })()
  return ownedCleanupPromise
}

const SIGNAL_EXIT_CODES = new Map([
  ['SIGHUP', 129],
  ['SIGINT', 130],
  ['SIGTERM', 143],
])
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

function parseClaudeJsonl(stdout) {
  const toolUseIds = new Set()
  const toolResultIds = new Set()
  const assistantText = []
  let finalUsage = null
  let resultText = ''
  for (const line of stdout.split('\n')) {
    if (!line.trim()) continue
    let value
    try {
      value = JSON.parse(line)
    } catch {
      continue
    }
    const content = value?.message?.content
    if (Array.isArray(content)) {
      for (const block of content) {
        if (block?.type === 'tool_use' && block.id) toolUseIds.add(block.id)
        if (block?.type === 'tool_result' && block.tool_use_id) {
          toolResultIds.add(block.tool_use_id)
        }
        if (block?.type === 'text' && typeof block.text === 'string') {
          assistantText.push(block.text)
        }
      }
    }
    if (value?.type === 'result') {
      if (typeof value.result === 'string') resultText = value.result
      if (value.usage && typeof value.usage === 'object') finalUsage = value.usage
    }
  }
  const usageNumbers = Object.values(finalUsage || {}).filter(
    (value) => typeof value === 'number' && Number.isFinite(value),
  )
  return {
    toolUseCount: toolUseIds.size,
    toolResultCount: toolResultIds.size,
    assistantText: assistantText.join(''),
    resultText,
    finalUsage,
    hasNonzeroUsage: usageNumbers.some((value) => value > 0),
  }
}

async function runClaude({ baseUrl, projectRoot, prompt }) {
  const home = path.join(TEMP_ROOT, 'claude-home')
  const configDir = path.join(TEMP_ROOT, 'claude-config')
  fs.mkdirSync(home, { recursive: true, mode: 0o700 })
  fs.mkdirSync(configDir, { recursive: true, mode: 0o700 })
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
    '--no-session-persistence',
    '--model',
    'sonnet',
    '--tools',
    'Bash',
    '--dangerously-skip-permissions',
    '--',
    prompt,
  ]
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
    if (bytes > 8 * 1024 * 1024) {
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
    postgres: {
      url: POSTGRES_URL,
      maxConnections: 4,
      migrateOnStart: true,
    },
    redis: {
      url: REDIS_URL,
      keyPrefix: REDIS_PREFIX,
    },
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

async function runSignalFixture() {
  const ackPath = requiredEnvironment('KIRO_SIGNAL_ACK_PATH')
  const fakePort = await reservePort()
  const redisPort = await reservePort()
  const fakeServer = http.createServer((_request, response) => {
    response.writeHead(204)
    response.end()
  })
  const fake = {
    async close() {
      if (!fakeServer.listening) return
      await new Promise((resolve) => fakeServer.close(resolve))
    },
  }
  ACTIVE_FAKE_SERVERS.add(fake)
  await new Promise((resolve, reject) => {
    fakeServer.once('error', reject)
    fakeServer.listen(fakePort, '127.0.0.1', resolve)
  })

  const redisServer = net.createServer((socket) => {
    socket.once('data', () => {
      fs.writeFileSync(ackPath, 'bounded-owned-prefix-cleanup\n', { mode: 0o600 })
      socket.end('*2\r\n:0\r\n$1\r\n0\r\n')
    })
  })
  await new Promise((resolve, reject) => {
    redisServer.once('error', reject)
    redisServer.listen(redisPort, '127.0.0.1', resolve)
  })
  REDIS_URL = `redis://127.0.0.1:${redisPort}/0`

  fs.writeFileSync(path.join(TEMP_ROOT, 'owned-temp-marker'), 'owned\n', { mode: 0o600 })
  const ownedChild = spawn(
    process.execPath,
    ['-e', 'setInterval(() => {}, 1000)'],
    { detached: true, stdio: 'ignore' },
  )
  ACTIVE_CHILDREN.add(ownedChild)
  ownedChild.once('exit', () => ACTIVE_CHILDREN.delete(ownedChild))
  process.stdout.write(`${JSON.stringify({
    ready: true,
    tempRoot: TEMP_ROOT,
    ownedChildPid: ownedChild.pid,
    fakePort,
    redisPort,
  })}\n`)

  await new Promise(() => {})
}

async function main() {
  const servicePort = await reservePort()
  const upstreamPort = await reservePort()
  assert.notEqual(servicePort, 9022)
  assert.notEqual(upstreamPort, 9022)
  assert.deepEqual(listeningPids(servicePort), [])
  assert.deepEqual(listeningPids(upstreamPort), [])

  const fake = createFakeUpstream()
  ACTIVE_FAKE_SERVERS.add(fake)
  let service = null
  let cleanup = {
    childGroupsStopped: false,
    serviceStopped: false,
    fakeStopped: false,
    tempRemoved: false,
    portsReleased: false,
    protected9022ProbeSkipped: true,
    redisKeysRemoved: false,
  }
  let report = null
  try {
    await fake.listen(upstreamPort)
    const configPath = path.join(TEMP_ROOT, 'config.json')
    const missingCredentialsPath = path.join(TEMP_ROOT, 'missing-credentials.json')
    const serviceLogPath = path.join(TEMP_ROOT, 'service.log')
    const projectRoot = path.join(TEMP_ROOT, 'project')
    fs.mkdirSync(projectRoot, { recursive: true, mode: 0o700 })
    fs.writeFileSync(configPath, `${JSON.stringify(isolatedConfig({ servicePort, upstreamPort }), null, 2)}\n`, { mode: 0o600 })
    const serviceLog = fs.openSync(serviceLogPath, 'a', 0o600)
    service = spawn(BINARY, ['--config', configPath, '--credentials', missingCredentialsPath], {
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
    const cases = []
    const scenarioKinds = ['bare', 'fenced', 'explanatory', 'structured']
    for (let round = 1; round <= ROUNDS; round += 1) {
      for (const kind of scenarioKinds) {
        const id = `${kind}-${round}`
        const sentinel = path.join(TEMP_ROOT, `owned-sentinel-${id}`)
        const bareInvoke = `<invoke name="Bash"><parameter name="command">touch ${sentinel}</parameter></invoke>`
        const scenario = {
          id,
          mode: kind === 'structured' ? 'structured' : 'negative',
          inferenceHits: 0,
          content: kind === 'bare'
            ? bareInvoke
            : kind === 'fenced'
              ? `\`\`\`xml\n<function_calls>${bareInvoke}</function_calls>\n\`\`\``
              : `This is a literal protocol example, not an action:\n<function_calls>${bareInvoke}</function_calls>`,
        }
        fake.setScenario(scenario)
        const cli = await runClaude({
          baseUrl,
          projectRoot,
          prompt: `Isolated protocol fixture ${id}. Return the upstream response.`,
        })
        const parsed = parseClaudeJsonl(cli.stdoutText)
        assert.equal(cli.timedOut, false, `${id}: Claude CLI timed out`)
        assert.equal(
          cli.code,
          0,
          `${id}: Claude CLI exit status code=${cli.code} signal=${cli.signal}; `
            + `stdout=${JSON.stringify(redactDiagnosticText(cli.stdoutText))}; `
            + `stderr=${JSON.stringify(redactDiagnosticText(cli.stderrText))}; `
            + `stderr hash ${cli.stderrSha256}`,
        )
        assert.equal(parsed.hasNonzeroUsage, true, `${id}: final usage is missing or all zero`)

        if (scenario.mode === 'negative') {
          assert.equal(parsed.toolUseCount, 0, `${id}: literal XML became executable tool_use`)
          assert.equal(parsed.toolResultCount, 0, `${id}: literal XML produced tool_result`)
          assert.equal(fs.existsSync(sentinel), false, `${id}: owned Bash sentinel was created`)
          assert.equal(scenario.inferenceHits, 1, `${id}: negative case caused an extra inference turn`)
          assert.equal(parsed.assistantText.includes('<invoke name="Bash">'), true, `${id}: literal invoke text disappeared`)
          if (kind === 'fenced' || kind === 'explanatory') {
            assert.equal(parsed.assistantText.includes('<function_calls>'), true, `${id}: literal envelope disappeared`)
          }
        } else {
          assert.equal(parsed.toolUseCount, 1, `${id}: structured ToolUseEvent was not exposed once`)
          assert.equal(parsed.toolResultCount, 1, `${id}: structured Bash result did not round-trip once`)
          assert.equal(scenario.inferenceHits, 2, `${id}: structured tool loop did not use two inference turns`)
          assert.equal(parsed.assistantText.includes('structured-finished'), true, `${id}: structured final text missing`)
          assert.equal(fs.existsSync(sentinel), false, `${id}: structured control touched negative sentinel`)
        }

        cases.push({
          id,
          kind,
          round,
          exitCode: cli.code,
          durationMs: cli.durationMs,
          inferenceHits: scenario.inferenceHits,
          toolUseCount: parsed.toolUseCount,
          toolResultCount: parsed.toolResultCount,
          hasNonzeroUsage: parsed.hasNonzeroUsage,
          usage: parsed.finalUsage,
          literalInvokeVisible: parsed.assistantText.includes('<invoke name="Bash">'),
          sentinelCreated: fs.existsSync(sentinel),
          stdoutSha256: cli.stdoutSha256,
          stderrSha256: cli.stderrSha256,
        })
      }
    }

    report = {
      schemaVersion: 1,
      result: 'pass',
      runId: RUN_ID,
      gitHead: commandOutput('git', ['rev-parse', 'HEAD']),
      dirty: commandOutput('git', ['status', '--short']).length > 0,
      binarySha256: sha256File(BINARY),
      claudeCliVersion: cliVersion,
      rounds: ROUNDS,
      cases,
      totals: {
        cases: cases.length,
        negativeCases: cases.filter((item) => item.kind !== 'structured').length,
        structuredCases: cases.filter((item) => item.kind === 'structured').length,
        inferenceHits: cases.reduce((sum, item) => sum + item.inferenceHits, 0),
        toolUseCount: cases.reduce((sum, item) => sum + item.toolUseCount, 0),
        toolResultCount: cases.reduce((sum, item) => sum + item.toolResultCount, 0),
        fakeModelDiscoveryRequests: fake.records.filter((item) => item.kind === 'model_discovery').length,
        fakeUnknownRequests: fake.records.filter((item) => item.kind === 'unknown').length,
      },
      isolation: {
        servicePort,
        upstreamPort,
        forbiddenPorts: [9022],
        protected9022ProbeSkipped: true,
        isolatedHome: true,
        isolatedClaudeConfigDir: true,
        isolatedProject: true,
        fakeKiroCredential: true,
        callerOwnedPostgresDatabase: true,
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
    cleanup.childGroupsStopped = owned.childrenStopped
    cleanup.serviceStopped = service === null || service.exitCode !== null
    cleanup.fakeStopped = owned.fakeServersStopped
    cleanup.redisKeysRemoved = owned.redis.complete
    cleanup.tempRemoved = owned.tempRemoved
    cleanup.portsReleased = listeningPids(servicePort).length === 0
      && listeningPids(upstreamPort).length === 0

    if (report) {
      report.cleanup = cleanup
      report.result = Object.values(cleanup).every(Boolean) ? 'pass' : 'fail'
      fs.mkdirSync(REPORT_ROOT, { recursive: true })
      const serialized = `${JSON.stringify(report, null, 2)}\n`
      for (const forbidden of [REQUEST_KEY, ADMIN_KEY, KIRO_KEY, POSTGRES_URL, REDIS_URL]) {
        assert.equal(serialized.includes(forbidden), false, 'report contains a secret or connection URL')
      }
      fs.writeFileSync(REPORT_PATH, serialized, { mode: 0o600 })
    }
  }

  assert.ok(report)
  assert.equal(report.result, 'pass')
  assert.equal(report.totals.cases, 20)
  assert.equal(report.totals.negativeCases, 15)
  assert.equal(report.totals.structuredCases, 5)
  assert.equal(report.totals.toolUseCount, 5)
  assert.equal(report.totals.toolResultCount, 5)
  assert.equal(report.totals.fakeUnknownRequests, 0)
  process.stdout.write(`${REPORT_PATH}\n`)
}

if (!SIGNAL_FIXTURE_MODE) {
  main().catch(async (error) => {
    await cleanupOwnedRuntime({ redisPasses: 1 }).catch(() => {})
    process.stderr.write(`bare invoke Claude CLI validation failed: ${error.message}\n`)
    process.exitCode = 1
  })
} else {
  runSignalFixture().catch(async (error) => {
    await cleanupOwnedRuntime({ redisPasses: 1 }).catch(() => {})
    process.stderr.write(`bare invoke signal fixture failed: ${error.message}\n`)
    process.exitCode = 1
  })
}
