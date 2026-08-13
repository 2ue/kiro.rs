#!/usr/bin/env node

import assert from 'node:assert/strict'
import crypto from 'node:crypto'
import fs from 'node:fs'
import http from 'node:http'
import net from 'node:net'
import path from 'node:path'
import { spawn, spawnSync } from 'node:child_process'
import { performance } from 'node:perf_hooks'

import { resolveRuntimeValidationPaths } from './runtime-validation-paths.mjs'

const ROOT = path.resolve(import.meta.dirname, '../..')
const { binary: BINARY, artifactRoot: ARTIFACT_ROOT } = resolveRuntimeValidationPaths(ROOT)
const ROUNDS = Number.parseInt(process.env.KIRO_E05_ROUNDS || '3', 10)
const REQUESTS_PER_ROUND = Number.parseInt(process.env.KIRO_E05_REQUESTS || '5', 10)
const SCHEDULER_FAULT_KIND = process.env.KIRO_E04_REDIS_FAULT || 'latency'
const SCHEDULER_LATENCY_MS = Number.parseInt(process.env.KIRO_E04_REDIS_LATENCY_MS || '500', 10)
const SCHEDULER_FALLBACK_ENABLED = process.env.KIRO_E04_FALLBACK_ENABLED !== 'false'
const SCHEDULER_RECOVERY_REQUESTS = Number.parseInt(
  process.env.KIRO_E04_RECOVERY_REQUESTS || '5',
  10,
)
// Capacity/queue coordination intentionally has a wider budget than sticky
// affinity. Affinity-only latency above the old 75ms budget must not be
// treated as a capacity-degraded pool or routed to external fallback.
const SCHEDULER_HOT_PATH_TIMEOUT_MS = 250
const REQUEST_KEY = 'sk-request-e05-isolated-validation'
const ADMIN_KEY = 'sk-admin-e05-isolated-validation'
const POSTGRES_URL_TEMPLATE = requiredEnvironment('KIRO_E05_POSTGRES_URL_TEMPLATE')
const REDIS_URL = requiredEnvironment('KIRO_E05_REDIS_URL')
const REDIS_PREFIX = requiredEnvironment('KIRO_E05_REDIS_PREFIX')
const VALIDATE_ONLY = process.env.KIRO_E05_VALIDATE_ONLY === '1'
const RUN_ID = `e05-${new Date().toISOString().replace(/[-:.TZ]/g, '')}-${process.pid}-${crypto.randomBytes(3).toString('hex')}`
const TEMP_ROOT = path.join(ARTIFACT_ROOT, 'runtime', 'e05', RUN_ID)
const REPORT_ROOT = path.join(ARTIFACT_ROOT, 'reports', 'e05')
const REPORT_PATH = path.join(REPORT_ROOT, `${RUN_ID}.json`)
const ALL_MODES = [
  'no_credentials',
  'all_disabled',
  'unsupported_model',
  'local_all_cooling',
  'local_capacity_full',
  'scheduler_redis_degraded',
  'scheduler_redis_chaos',
  'fallback_disabled_no_credentials',
  'external_error_no_loop',
  'local_ready_transient',
]
const MODES = process.env.KIRO_E05_MODES
  ? process.env.KIRO_E05_MODES.split(',').map((mode) => mode.trim()).filter(Boolean)
  : ALL_MODES
const REQUIRED_DATABASE_COUNT = MODES.length * ROUNDS
const POSTGRES_DATABASES = String(process.env.KIRO_E05_POSTGRES_DATABASES || '')
  .split(',')
  .map((value) => value.trim())
  .filter(Boolean)
const ACTIVE_CHILDREN = new Set()
let redisTarget = null

if (!Number.isInteger(ROUNDS) || ROUNDS < 3 || ROUNDS > 10) {
  throw new Error('KIRO_E05_ROUNDS must be an integer between 3 and 10')
}
if (!Number.isInteger(REQUESTS_PER_ROUND) || REQUESTS_PER_ROUND < 5 || REQUESTS_PER_ROUND > 10) {
  throw new Error('KIRO_E05_REQUESTS must be an integer between 5 and 10')
}
if (MODES.length === 0 || MODES.some((mode) => !ALL_MODES.includes(mode))) {
  throw new Error(`KIRO_E05_MODES must contain only: ${ALL_MODES.join(',')}`)
}
if (MODES.includes('scheduler_redis_chaos')) {
  if (!['latency', 'disconnect'].includes(SCHEDULER_FAULT_KIND)) {
    throw new Error('KIRO_E04_REDIS_FAULT must be latency or disconnect')
  }
  if (!Number.isInteger(SCHEDULER_LATENCY_MS)
    || SCHEDULER_LATENCY_MS < 0
    || SCHEDULER_LATENCY_MS > 2_000) {
    throw new Error('KIRO_E04_REDIS_LATENCY_MS must be an integer between 0 and 2000')
  }
  if (!Number.isInteger(SCHEDULER_RECOVERY_REQUESTS)
    || SCHEDULER_RECOVERY_REQUESTS < 5
    || SCHEDULER_RECOVERY_REQUESTS > 20) {
    throw new Error('KIRO_E04_RECOVERY_REQUESTS must be an integer between 5 and 20')
  }
}

function requiredEnvironment(name) {
  const value = String(process.env[name] || '').trim()
  if (!value) throw new Error(`${name} is required`)
  return value
}

function validateInputs() {
  const placeholderCount = (POSTGRES_URL_TEMPLATE.match(/\{database\}/g) || []).length
  if (placeholderCount !== 1) {
    throw new Error('KIRO_E05_POSTGRES_URL_TEMPLATE must contain exactly one literal {database} placeholder')
  }
  const postgresSampleDatabase = POSTGRES_DATABASES[0] || 'kiro_e05_contract_sample'
  const postgres = new URL(POSTGRES_URL_TEMPLATE.replace('{database}', postgresSampleDatabase))
  if (!['postgres:', 'postgresql:'].includes(postgres.protocol)) {
    throw new Error('KIRO_E05_POSTGRES_URL_TEMPLATE must use PostgreSQL')
  }
  if (!['127.0.0.1', 'localhost', '::1'].includes(postgres.hostname)) {
    throw new Error('KIRO_E05_POSTGRES_URL_TEMPLATE must target loopback')
  }
  if (Number(postgres.port || 5432) === 9022) throw new Error('port 9022 is protected')
  if (POSTGRES_DATABASES.length !== REQUIRED_DATABASE_COUNT) {
    throw new Error(`KIRO_E05_POSTGRES_DATABASES must contain exactly ${REQUIRED_DATABASE_COUNT} pre-created database names`)
  }
  for (const database of POSTGRES_DATABASES) {
    if (!/^kiro_e05_[a-z0-9_]{3,80}$/.test(database)) {
      throw new Error('KIRO_E05_POSTGRES_DATABASES must contain caller-owned kiro_e05_* names')
    }
  }

  const redis = new URL(REDIS_URL)
  if (redis.protocol !== 'redis:') throw new Error('KIRO_E05_REDIS_URL must use redis://')
  if (redis.username || redis.password) throw new Error('KIRO_E05_REDIS_URL must not contain Redis auth material')
  if (!['127.0.0.1', 'localhost', '::1'].includes(redis.hostname)) {
    throw new Error('KIRO_E05_REDIS_URL must target loopback')
  }
  if (redis.search || redis.hash) throw new Error('KIRO_E05_REDIS_URL must not contain query or fragment data')
  const redisPort = Number(redis.port || 6379)
  if (redisPort === 9022) throw new Error('port 9022 is protected')
  const dbText = redis.pathname.replace(/^\//, '')
  if (!/^\d+$/.test(dbText)) throw new Error('KIRO_E05_REDIS_URL must name a Redis database')
  const redisDatabase = Number(dbText)
  if (!Number.isSafeInteger(redisDatabase) || redisDatabase < 1 || redisDatabase > 15) {
    throw new Error('KIRO_E05_REDIS_URL must use an isolated nonzero database in 1..15')
  }
  if (REDIS_PREFIX.includes('kiro_rs:local')) {
    throw new Error('KIRO_E05_REDIS_PREFIX must be a caller-owned temporary prefix')
  }
  if (!/^[a-z0-9][a-z0-9:._-]{7,95}$/.test(REDIS_PREFIX)) {
    throw new Error('KIRO_E05_REDIS_PREFIX has an invalid format')
  }
  redisTarget = { redis, redisPort, redisDatabase }
  return {
    postgresHost: postgres.hostname,
    postgresPort: Number(postgres.port || 5432),
    postgresDatabaseCount: POSTGRES_DATABASES.length,
    redisHost: redis.hostname,
    redisPort,
    redisDatabase,
    redisPrefixSha256: sha256(REDIS_PREFIX),
  }
}

function postgresUrlFor(database) {
  return POSTGRES_URL_TEMPLATE.replace('{database}', encodeURIComponent(database))
}

function databaseFor(modeIndex, round) {
  return POSTGRES_DATABASES[modeIndex * ROUNDS + round - 1]
}

function sha256(value) {
  return crypto.createHash('sha256').update(value).digest('hex')
}

function sha256File(file) {
  return sha256(fs.readFileSync(file))
}

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: ROOT,
    encoding: 'utf8',
    maxBuffer: 16 * 1024 * 1024,
    ...options,
  })
  if (result.status !== 0) {
    const stderr = String(result.stderr || '').trim().slice(0, 2000)
    throw new Error(`${command} ${args.join(' ')} failed (${result.status}): ${stderr}`)
  }
  return String(result.stdout || '').trim()
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
        server.close((error) => error ? reject(error) : resolve(selected))
      })
    })
    if (port !== 9022) return port
  }
}

async function waitForTcp(port, timeoutMs = 30_000) {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    const connected = await new Promise((resolve) => {
      const socket = net.connect({ host: '127.0.0.1', port })
      socket.setTimeout(300)
      socket.once('connect', () => {
        socket.destroy()
        resolve(true)
      })
      socket.once('timeout', () => {
        socket.destroy()
        resolve(false)
      })
      socket.once('error', () => resolve(false))
    })
    if (connected) return
    await new Promise((resolve) => setTimeout(resolve, 100))
  }
  throw new Error(`timeout waiting for 127.0.0.1:${port}`)
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

function writeKiroSuccess(response, text) {
  const body = Buffer.concat([
    eventFrame('assistantResponseEvent', {
      content: text,
      messageStatus: 'COMPLETED',
    }),
    eventFrame('metadataEvent', {
      tokenUsage: {
        uncachedInputTokens: 8,
        cacheReadInputTokens: 0,
        cacheWriteInputTokens: 0,
        outputTokens: 4,
        totalTokens: 12,
      },
    }),
  ])
  response.writeHead(200, {
    'content-type': 'application/vnd.amazon.eventstream',
    'content-length': body.length,
    connection: 'close',
  })
  response.end(body)
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

async function consumeRequest(request) {
  for await (const _chunk of request) {
    // Drain the request before replying so keep-alive state cannot affect hit counts.
  }
}

function createFakeUpstreams() {
  const state = {
    caseId: null,
    localInferenceHits: 0,
    caseLocalInferenceHits: 0,
    localAuxiliaryHits: 0,
    externalHits: 0,
    records: [],
  }
  const local = http.createServer(async (request, response) => {
    await consumeRequest(request)
    const url = new URL(request.url || '/', 'http://127.0.0.1')
    const target = String(request.headers['x-amz-target'] || '')
    const kind = target.endsWith('.ListAvailableModels')
      || url.pathname.toLowerCase().endsWith('/listavailablemodels')
      ? 'auxiliary'
      : 'inference'
    state.records.push({
      caseId: state.caseId,
      channel: 'local',
      kind,
      path: request.url,
      target: target || null,
    })
    if (kind === 'inference') {
      state.localInferenceHits += 1
      state.caseLocalInferenceHits += 1
      if (state.caseId?.startsWith('scheduler_redis_chaos-')) {
        writeKiroSuccess(response, `local-ok ${state.caseId}`)
        return
      }
      if (state.caseId?.startsWith('local_capacity_full-') && state.caseLocalInferenceHits === 1) {
        // Keep the only local credential slot occupied while the case sends
        // independent probes that must fall back to the external pool.
        await new Promise((resolve) => setTimeout(resolve, 4_000))
      }
      if (state.caseId?.startsWith('local_all_cooling-')) {
        writeJson(response, 429, {
          message: 'controlled local rate limit',
          reason: 'THROTTLING_EXCEPTION',
        })
        return
      }
      writeJson(response, 500, { message: 'controlled local server error', reason: 'SERVER_ERROR' })
      return
    }
    state.localAuxiliaryHits += 1
    if (state.caseId?.startsWith('local_capacity_full-')
      || state.caseId?.startsWith('scheduler_redis_degraded-')
      || state.caseId?.startsWith('scheduler_redis_chaos-')) {
      writeJson(response, 200, {
        defaultModel: { modelId: 'claude-sonnet-4' },
        models: [{
          modelId: 'claude-sonnet-4',
          modelName: 'Claude Sonnet 4',
          supportedInputTypes: ['TEXT'],
        }],
      })
      return
    }
    writeJson(response, 500, {
      message: 'controlled model discovery failure',
      reason: 'SERVER_ERROR',
    })
  })
  const external = http.createServer(async (request, response) => {
    await consumeRequest(request)
    state.externalHits += 1
    state.records.push({
      caseId: state.caseId,
      channel: 'external',
      kind: 'inference',
      path: request.url,
    })
    if (state.caseId?.startsWith('external_error_no_loop-')) {
      writeJson(response, 500, {
        type: 'error',
        error: { type: 'api_error', message: 'controlled external server error' },
      })
      return
    }
    writeJson(response, 200, {
      id: `msg_external_${state.externalHits}`,
      type: 'message',
      role: 'assistant',
      model: 'claude-sonnet-4',
      content: [{ type: 'text', text: 'external-ok' }],
      stop_reason: 'end_turn',
      stop_sequence: null,
      usage: { input_tokens: 8, output_tokens: 2 },
    })
  })

  return {
    state,
    setCase(caseId) {
      state.caseId = caseId
      state.caseLocalInferenceHits = 0
    },
    snapshot() {
      return {
        localInferenceHits: state.localInferenceHits,
        localAuxiliaryHits: state.localAuxiliaryHits,
        externalHits: state.externalHits,
      }
    },
    async listen(localPort, externalPort) {
      await Promise.all([
        new Promise((resolve, reject) => {
          local.once('error', reject)
          local.listen(localPort, '127.0.0.1', resolve)
        }),
        new Promise((resolve, reject) => {
          external.once('error', reject)
          external.listen(externalPort, '127.0.0.1', resolve)
        }),
      ])
    },
    async close() {
      await Promise.all([
        new Promise((resolve) => local.close(resolve)),
        new Promise((resolve) => external.close(resolve)),
      ])
    },
  }
}

function delta(after, before) {
  return Object.fromEntries(Object.keys(after).map((key) => [key, after[key] - before[key]]))
}

async function timedRequest(url, options = {}) {
  const started = performance.now()
  const response = await fetch(url, options)
  const headersAt = performance.now()
  const text = await response.text()
  const ended = performance.now()
  return {
    status: response.status,
    text,
    requestId: response.headers.get('request-id') || response.headers.get('x-request-id'),
    headerMs: Number((headersAt - started).toFixed(2)),
    totalMs: Number((ended - started).toFixed(2)),
  }
}

async function waitForCondition(predicate, description, timeoutMs = 10_000) {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    if (predicate()) return
    await new Promise((resolve) => setTimeout(resolve, 20))
  }
  throw new Error(`timeout waiting for ${description}`)
}

async function waitForHealth(baseUrl, processHandle, timeoutMs = 60_000) {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    if (processHandle.exitCode !== null) {
      throw new Error(`kiro-rs exited before health check: ${processHandle.exitCode}`)
    }
    try {
      const response = await fetch(`${baseUrl}/healthz`)
      if (response.ok) return
    } catch {}
    await new Promise((resolve) => setTimeout(resolve, 100))
  }
  throw new Error(`timeout waiting for ${baseUrl}/healthz`)
}

function processResources(pid) {
  const ps = spawnSync('ps', ['-o', 'rss=', '-p', String(pid)], { encoding: 'utf8' })
  const rssKb = Number.parseInt(String(ps.stdout || '').trim(), 10) || 0
  const lsof = spawnSync('lsof', ['-nP', '-p', String(pid)], { encoding: 'utf8' })
  const fdCount = String(lsof.stdout || '').trim().split('\n').filter(Boolean).length
  return { rssKb, fdCount }
}

function diagnosticLogLines(logPath) {
  if (!fs.existsSync(logPath)) return []
  const selected = fs.readFileSync(logPath, 'utf8')
    .replace(/\u001b\[[0-9;]*m/g, '')
    .replaceAll(REQUEST_KEY, '<redacted-request-key>')
    .replaceAll(ADMIN_KEY, '<redacted-admin-key>')
    .replaceAll('sk-e05-fake-external', '<redacted-external-key>')
    .replace(/e05-local-token-\d+/g, '<redacted-local-token>')
    .split('\n')
    .filter((line) => [
      'Redis 调度热路径',
      'Redis 会话粘性',
      'routing request directly to external pool',
      'fresh local state permits external fallback',
      'external fallback suppressed',
      'Kiro API 凭据调用链路',
    ].some((marker) => line.includes(marker)))
  return selected.slice(-80)
}

function minimalChildEnv(extra = {}) {
  return {
    PATH: process.env.PATH || '/usr/bin:/bin',
    TMPDIR: process.env.TMPDIR || '/tmp',
    ...extra,
  }
}

function startChild(command, args, options = {}) {
  const child = spawn(command, args, {
    cwd: ROOT,
    stdio: options.stdio || ['ignore', 'pipe', 'pipe'],
    env: minimalChildEnv(options.env || {}),
  })
  ACTIVE_CHILDREN.add(child)
  child.once('exit', () => ACTIVE_CHILDREN.delete(child))
  return child
}

async function startRedisProxy(target) {
  const script = path.join(ROOT, 'feature/tests/redis-chaos-proxy.mjs')
  const child = startChild(process.execPath, [
    script,
    '--listen-host', '127.0.0.1',
    '--listen-port', '0',
    '--api-host', '127.0.0.1',
    '--api-port', '0',
    '--upstream-host', target.redis.hostname,
    '--upstream-port', String(target.redisPort),
    '--database', String(target.redisDatabase),
    '--name', 'redis',
  ])
  let stderr = ''
  child.stderr.on('data', (chunk) => { stderr += chunk.toString('utf8') })
  const ready = await new Promise((resolve, reject) => {
    let stdout = ''
    const timer = setTimeout(() => reject(new Error(`redis proxy did not become ready: ${stderr.slice(-4000)}`)), 10_000)
    child.stdout.on('data', (chunk) => {
      stdout += chunk.toString('utf8')
      const line = stdout.split('\n').find((entry) => entry.trim().startsWith('{'))
      if (!line) return
      clearTimeout(timer)
      try {
        resolve(JSON.parse(line))
      } catch (error) {
        reject(error)
      }
    })
    child.once('exit', (code) => {
      clearTimeout(timer)
      reject(new Error(`redis proxy exited before ready code=${code}: ${stderr.slice(-4000)}`))
    })
  })
  assert.equal(ready.protected9022ProbeSkipped, true)
  return { child, ...ready }
}

async function stopChild(handle, label) {
  if (!handle || handle.exitCode !== null || handle.signalCode !== null) return
  const waitForExit = (timeoutMs) => new Promise((resolve) => {
    if (handle.exitCode !== null || handle.signalCode !== null) return resolve(true)
    let timer
    const onExit = () => {
      clearTimeout(timer)
      resolve(true)
    }
    handle.once('exit', onExit)
    timer = setTimeout(() => {
      handle.off('exit', onExit)
      resolve(false)
    }, timeoutMs)
  })
  handle.kill('SIGTERM')
  const exited = await waitForExit(8_000)
  if (!exited && handle.exitCode === null && handle.signalCode === null) {
    handle.kill('SIGKILL')
    await waitForExit(8_000)
  }
  if (handle.exitCode === null && handle.signalCode === null) {
    throw new Error(`failed to stop ${label}`)
  }
  ACTIVE_CHILDREN.delete(handle)
}

async function stopService(handle) {
  await stopChild(handle, 'kiro-rs service')
}

function startService(configPath, credentialsPath, logPath) {
  const log = fs.openSync(logPath, 'a')
  const handle = startChild(BINARY, ['--config', configPath, '--credentials', credentialsPath], {
    env: { RUST_LOG: 'info', KIRO_API_KEY: '' },
    stdio: ['ignore', log, log],
  })
  handle.once('exit', () => {
    try { fs.closeSync(log) } catch {}
  })
  return handle
}

function encodeRedisCommands(commands) {
  return Buffer.concat(commands.map((parts) => {
    const encoded = [Buffer.from(`*${parts.length}\r\n`)]
    for (const part of parts) {
      const bytes = Buffer.from(String(part))
      encoded.push(Buffer.from(`$${bytes.length}\r\n`), bytes, Buffer.from('\r\n'))
    }
    return Buffer.concat(encoded)
  }))
}

function parseRedisReply(buffer, offset = 0) {
  if (offset >= buffer.length) return null
  const type = String.fromCharCode(buffer[offset])
  const lineEnd = buffer.indexOf('\r\n', offset + 1)
  if (lineEnd < 0) return null
  const line = buffer.subarray(offset + 1, lineEnd).toString('utf8')
  const next = lineEnd + 2
  if (type === '+' || type === '-' || type === ':') {
    return { type, value: type === ':' ? Number(line) : line, next }
  }
  if (type === '$') {
    const length = Number(line)
    if (length === -1) return { type, value: null, next }
    const end = next + length
    if (end + 2 > buffer.length) return null
    return { type, value: buffer.subarray(next, end).toString('utf8'), next: end + 2 }
  }
  if (type === '*') {
    const count = Number(line)
    const values = []
    let cursor = next
    for (let index = 0; index < count; index += 1) {
      const item = parseRedisReply(buffer, cursor)
      if (!item) return null
      if (item.type === '-') throw new Error(`Redis command failed: ${item.value}`)
      values.push(item.value)
      cursor = item.next
    }
    return { type, value: values, next: cursor }
  }
  throw new Error(`unsupported Redis reply type ${type}`)
}

async function redisCommand(parts, timeoutMs = 5_000) {
  assert(redisTarget)
  const commands = []
  if (redisTarget.redisDatabase !== 0) commands.push(['SELECT', String(redisTarget.redisDatabase)])
  commands.push(parts)
  return await new Promise((resolve, reject) => {
    const socket = net.connect({ host: redisTarget.redis.hostname, port: redisTarget.redisPort })
    const chunks = []
    socket.setTimeout(timeoutMs)
    socket.once('connect', () => socket.write(encodeRedisCommands(commands)))
    socket.on('data', (chunk) => {
      chunks.push(chunk)
      const buffer = Buffer.concat(chunks)
      try {
        let cursor = 0
        let last
        for (let index = 0; index < commands.length; index += 1) {
          const parsed = parseRedisReply(buffer, cursor)
          if (!parsed) return
          if (parsed.type === '-') throw new Error(`Redis command failed: ${parsed.value}`)
          last = parsed.value
          cursor = parsed.next
        }
        socket.end()
        resolve(last)
      } catch (error) {
        socket.destroy()
        reject(error)
      }
    })
    socket.once('timeout', () => {
      socket.destroy()
      reject(new Error('Redis command timed out'))
    })
    socket.once('error', reject)
  })
}

async function scanOwnedRedisKeys(limit = 50_000) {
  const keys = []
  let cursor = '0'
  do {
    const reply = await redisCommand(['SCAN', cursor, 'MATCH', `${REDIS_PREFIX}:*`, 'COUNT', '1000'])
    cursor = String(reply[0])
    for (const key of reply[1]) {
      keys.push(key)
      if (keys.length > limit) throw new Error('too many owned Redis keys to clean safely')
    }
  } while (cursor !== '0')
  return keys
}

async function cleanupOwnedRedisKeys() {
  if (!redisTarget) return { removed: 0, remaining: null }
  const keys = await scanOwnedRedisKeys()
  let removed = 0
  for (let index = 0; index < keys.length; index += 100) {
    const chunk = keys.slice(index, index + 100)
    if (chunk.length) removed += Number(await redisCommand(['DEL', ...chunk])) || 0
  }
  const remaining = (await scanOwnedRedisKeys()).length
  return { removed, remaining }
}

function adminHeaders() {
  return { authorization: `Bearer ${ADMIN_KEY}`, 'content-type': 'application/json' }
}

function requestHeaders() {
  return { 'x-api-key': REQUEST_KEY, 'content-type': 'application/json' }
}

function credentialsFor(mode) {
  if (['no_credentials', 'fallback_disabled_no_credentials', 'external_error_no_loop'].includes(mode)) {
    return []
  }
  const count = ['local_ready_transient', 'scheduler_redis_chaos'].includes(mode)
    ? 60
    : mode === 'unsupported_model' ? 3 : 1
  return Array.from({ length: count }, (_, index) => ({
    accessToken: `e05-local-token-${index + 1}`,
    expiresAt: '2099-01-01T00:00:00Z',
    authMethod: 'social',
    endpoint: 'ide',
    priority: 0,
    maxConcurrentRequests: 1,
    rpm: 0,
    supportedModels: ['claude-sonnet-4'],
    disabled: mode === 'all_disabled',
  }))
}

function requestModelFor(mode) {
  return mode === 'unsupported_model' ? 'claude-opus-4' : 'claude-sonnet-4'
}

function expectedBootstrapAuxiliaryHits(mode) {
  if (['local_ready_transient', 'scheduler_redis_chaos'].includes(mode)) return 4
  if (mode === 'unsupported_model') return 3
  if (mode === 'local_all_cooling') return 1
  if (mode === 'local_capacity_full') return 1
  if (mode === 'scheduler_redis_degraded') return 1
  return 0
}

async function runCase({
  mode,
  round,
  postgresUrl,
  redisProxyPort,
  redisProxyApiPort,
  localPort,
  externalPort,
  fake,
}) {
  const caseId = `${mode}-${round}`
  // Each case owns a different PostgreSQL authority. Give it a matching Redis
  // namespace so a cooldown or lease from an earlier mode cannot contaminate it.
  const redisKeyPrefix = `${REDIS_PREFIX}:${RUN_ID}:${caseId}`
  const caseRoot = path.join(TEMP_ROOT, caseId)
  fs.mkdirSync(caseRoot, { recursive: true, mode: 0o700 })
  const configPath = path.join(caseRoot, 'config.json')
  const credentialsPath = path.join(caseRoot, 'credentials.json')
  const logPath = path.join(caseRoot, 'service.log')
  const servicePort = await reservePort()
  assert.notEqual(servicePort, 9022)
  const baseUrl = `http://127.0.0.1:${servicePort}`
  const requestModel = requestModelFor(mode)
  const config = {
    postgres: {
      url: postgresUrl,
      maxConnections: 4,
      migrateOnStart: true,
    },
    redis: {
      url: `redis://127.0.0.1:${redisProxyPort}/${redisTarget.redisDatabase}`,
      keyPrefix: redisKeyPrefix,
    },
    host: '127.0.0.1',
    port: servicePort,
    apiKey: REQUEST_KEY,
    adminApiKey: ADMIN_KEY,
    defaultEndpoint: 'ide',
    kiroUpstreamBaseUrl: `http://127.0.0.1:${localPort}/kiro`,
    kiroUpstreamResponseTimeoutSecs: 5,
    credentialRetryMaxAttempts: 6,
    inferenceUpstreamMaxAttempts: 4,
    credentialPromptLogicRetryEnabled: false,
    credentialWarmupRequests: 0,
    credentialMaxConcurrentRequests: 1,
    externalPools: {
      externalPoolsEnabled: true,
      externalPoolRetryMaxAttempts: 1,
      externalPoolRequestTimeoutSecs: 5,
      externalPoolServerErrorCooldownSecs: 1,
      externalPoolCapacityMode: 'fail_fast',
      externalPoolLocalRescueEnabled: false,
      externalPoolAutoDisableEnabled: false,
      fallbackOnLocalCapacityExhausted: true,
      fallbackOnSchedulerRedisDegraded: true,
      fallbackOnNoAvailableCredentials: true,
      fallbackOnLocalTransientExhausted: true,
      fallbackOnUnsupportedModel: true,
      localPoolPreflightEnabled: true,
    },
  }
  if (mode === 'fallback_disabled_no_credentials') {
    config.externalPools.fallbackOnNoAvailableCredentials = false
  }
  if (mode === 'scheduler_redis_chaos') {
    config.externalPools.fallbackOnSchedulerRedisDegraded = SCHEDULER_FALLBACK_ENABLED
  }
  fs.writeFileSync(configPath, `${JSON.stringify(config, null, 2)}\n`, { mode: 0o600 })
  fs.writeFileSync(credentialsPath, `${JSON.stringify(credentialsFor(mode), null, 2)}\n`, { mode: 0o600 })

  fake.setCase(caseId)
  const hitsBeforeCase = fake.snapshot()
  const service = startService(configPath, credentialsPath, logPath)
  let schedulerToxicInstalled = false
  let schedulerChaosFaultActive = false
  try {
    await waitForHealth(baseUrl, service)
    const resourcesStart = processResources(service.pid)
    const resourceSamples = [{ label: 'start', ...resourcesStart }]
    const sampleResources = (label) => {
      const sample = { label, ...processResources(service.pid) }
      resourceSamples.push(sample)
      return sample
    }
    const createPool = await timedRequest(`${baseUrl}/api/admin/external-pools`, {
      method: 'POST',
      headers: adminHeaders(),
      body: JSON.stringify({
        name: `e05-external-${caseId}`,
        baseUrl: `http://127.0.0.1:${externalPort}/external`,
        apiKey: 'sk-e05-fake-external',
        authType: 'bearer',
        enabled: true,
        priority: 0,
        maxConcurrentRequests: 10,
        usageProjectionMode: 'pass_through',
        requestBodyMode: 'normalized',
        rawModelMode: 'none',
        preservePath: true,
        supportedModels: [requestModel],
      }),
    })
    assert.equal(createPool.status, 200, createPool.text)
    const poolStatusResponse = await timedRequest(`${baseUrl}/api/admin/external-pools/status`, {
      headers: adminHeaders(),
    })
    assert.equal(poolStatusResponse.status, 200, poolStatusResponse.text)
    const poolStatus = JSON.parse(poolStatusResponse.text)
    const runtimeConfigResponse = await timedRequest(`${baseUrl}/api/admin/config/runtime`, {
      headers: adminHeaders(),
    })
    assert.equal(runtimeConfigResponse.status, 200, runtimeConfigResponse.text)
    const runtimeConfig = JSON.parse(runtimeConfigResponse.text)
    assert.equal(runtimeConfig.externalPools?.externalPoolsEnabled, true,
      `external pools disabled in runtime config: ${runtimeConfigResponse.text}`)
    // The runtime manager keeps a 250 ms availability snapshot, while the external
    // coordinator can cold-load its static pool list with a 500 ms timeout and a
    // short negative cache. This gate validates routing behavior, not that separate
    // post-create cache propagation window.
    await new Promise((resolve) => setTimeout(resolve, 1_500))

    if (mode === 'scheduler_redis_degraded') {
      const toxic = await timedRequest(`http://127.0.0.1:${redisProxyApiPort}/proxies/redis/toxics`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({
          name: 'scheduler-latency',
          type: 'latency',
          stream: 'downstream',
          toxicity: 1,
          attributes: { latency: SCHEDULER_LATENCY_MS, jitter: 0 },
        }),
      })
      assert.ok([200, 201].includes(toxic.status), toxic.text)
      schedulerToxicInstalled = true
      // Expire route snapshots created while provisioning the external pool.
      await new Promise((resolve) => setTimeout(resolve, 350))
    }
    if (mode === 'scheduler_redis_chaos') {
      if (SCHEDULER_FAULT_KIND === 'latency') {
        const toxic = await timedRequest(`http://127.0.0.1:${redisProxyApiPort}/proxies/redis/toxics`, {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({
            name: 'scheduler-chaos-latency',
            type: 'latency',
            stream: 'downstream',
            toxicity: 1,
            attributes: { latency: SCHEDULER_LATENCY_MS, jitter: 0 },
          }),
        })
        assert.ok([200, 201].includes(toxic.status), toxic.text)
      } else {
        const disabled = await timedRequest(
          `http://127.0.0.1:${redisProxyApiPort}/proxies/redis`,
          {
            method: 'POST',
            headers: { 'content-type': 'application/json' },
            body: JSON.stringify({ enabled: false }),
          },
        )
        assert.equal(disabled.status, 200, disabled.text)
      }
      schedulerChaosFaultActive = true
      // Expire route snapshots created while provisioning the external pool.
      await new Promise((resolve) => setTimeout(resolve, 350))
    }

    const hitsBeforeRequests = fake.snapshot()
    const requests = []
    let capacityHolder = null
    if (mode === 'local_capacity_full') {
      capacityHolder = timedRequest(`${baseUrl}/v1/messages`, {
        method: 'POST',
        headers: requestHeaders(),
        body: JSON.stringify({
          model: requestModel,
          max_tokens: 32,
          stream: false,
          messages: [{ role: 'user', content: `E05 ${caseId} capacity holder` }],
        }),
      })
      await waitForCondition(
        () => delta(fake.snapshot(), hitsBeforeRequests).localInferenceHits === 1,
        `${caseId} local capacity holder`,
      )
    }
    for (let requestIndex = 1; requestIndex <= REQUESTS_PER_ROUND; requestIndex += 1) {
      const before = fake.snapshot()
      const response = await timedRequest(`${baseUrl}/v1/messages`, {
        method: 'POST',
        headers: requestHeaders(),
        body: JSON.stringify({
          model: requestModel,
          max_tokens: 32,
          stream: false,
          messages: [{ role: 'user', content: `E05 ${caseId} request ${requestIndex}` }],
        }),
      })
      const hits = delta(fake.snapshot(), before)
      if (mode === 'local_ready_transient') {
        const diagnostic = JSON.stringify({ status: response.status, text: response.text, hits })
        assert.notEqual(response.status, 200,
          `local-ready request ${requestIndex} unexpectedly succeeded: ${diagnostic}`)
        assert.equal(hits.externalHits, 0,
          `local-ready request ${requestIndex} escaped to external: ${diagnostic}`)
        assert.ok(hits.localInferenceHits >= 1 && hits.localInferenceHits <= 4,
          `local-ready request ${requestIndex} did not reach bounded local attempts: ${diagnostic}`)
        assert.equal(response.text.includes('external-ok'), false)
      } else if (mode === 'scheduler_redis_chaos') {
        const diagnostic = JSON.stringify({
          fault: SCHEDULER_FAULT_KIND,
          latencyMs: SCHEDULER_LATENCY_MS,
          fallbackEnabled: SCHEDULER_FALLBACK_ENABLED,
          status: response.status,
          text: response.text,
          hits,
        })
        if (SCHEDULER_FAULT_KIND === 'disconnect') {
          assert.notEqual(response.status, 200, diagnostic)
          assert.equal(hits.localInferenceHits, 0, diagnostic)
          assert.equal(hits.externalHits, 0, diagnostic)
          assert.ok(response.requestId, `disconnect error omitted request id: ${diagnostic}`)
          assert.equal(
            /redis|credential|scheduler|external[ _-]?pool/i.test(response.text),
            false,
            `disconnect error exposed internal routing state: ${diagnostic}`,
          )
        } else if (SCHEDULER_LATENCY_MS < SCHEDULER_HOT_PATH_TIMEOUT_MS) {
          assert.equal(response.status, 200, diagnostic)
          assert.equal(hits.localInferenceHits, 1, diagnostic)
          assert.equal(hits.externalHits, 0, diagnostic)
          assert.equal(response.text.includes('local-ok'), true, diagnostic)
        } else if (!SCHEDULER_FALLBACK_ENABLED) {
          assert.notEqual(response.status, 200, diagnostic)
          assert.equal(hits.localInferenceHits, 0, diagnostic)
          assert.equal(hits.externalHits, 0, diagnostic)
          assert.ok(response.requestId, `degraded error omitted request id: ${diagnostic}`)
        } else if (SCHEDULER_LATENCY_MS === SCHEDULER_HOT_PATH_TIMEOUT_MS) {
          assert.equal(response.status, 200, diagnostic)
          assert.equal(
            hits.localInferenceHits + hits.externalHits,
            1,
            `75ms boundary must select exactly one upstream path: ${diagnostic}`,
          )
          assert.equal(
            response.text.includes(hits.localInferenceHits === 1 ? 'local-ok' : 'external-ok'),
            true,
            diagnostic,
          )
        } else {
          assert.equal(response.status, 200, diagnostic)
          assert.equal(hits.localInferenceHits, 0, diagnostic)
          assert.equal(hits.externalHits, 1, diagnostic)
          assert.equal(response.text.includes('external-ok'), true, diagnostic)
        }
      } else if (mode === 'fallback_disabled_no_credentials') {
        const diagnostic = JSON.stringify({ status: response.status, text: response.text, hits, poolStatus })
        assert.notEqual(response.status, 200, diagnostic)
        assert.equal(hits.localInferenceHits, 0, diagnostic)
        assert.equal(hits.externalHits, 0, diagnostic)
      } else if (mode === 'external_error_no_loop') {
        const diagnostic = JSON.stringify({ status: response.status, text: response.text, hits, poolStatus })
        assert.notEqual(response.status, 200, diagnostic)
        assert.equal(hits.localInferenceHits, 0, diagnostic)
        assert.equal(hits.externalHits, 1, diagnostic)
      } else {
        const diagnostic = JSON.stringify({
          status: response.status,
          text: response.text,
          hits,
          poolStatus,
        })
        assert.equal(response.status, 200, diagnostic)
        assert.equal(hits.externalHits, 1, diagnostic)
        assert.equal(response.text.includes('external-ok'), true, diagnostic)
        if (mode === 'local_all_cooling') {
          assert.ok(hits.localInferenceHits <= 1, diagnostic)
        } else {
          assert.equal(hits.localInferenceHits, 0, diagnostic)
        }
      }
      requests.push({ requestIndex, ...response, hits })
      sampleResources(`fault-request-${requestIndex}`)
      if (mode === 'external_error_no_loop' && requestIndex < REQUESTS_PER_ROUND) {
        // Prove that the pool recovers after its bounded server-error cooldown.
        // Without this wait, later requests correctly observe cooldown and do not
        // reach the fake upstream, which would not test the no-loop path again.
        await new Promise((resolve) => setTimeout(resolve, 1_100))
      }
    }
    const capacityHolderResponse = capacityHolder ? await capacityHolder : null
    if (capacityHolderResponse) {
      // The holder itself first consumes the local slot, receives the controlled
      // local 500, then performs one separately classified transient fallback.
      // Capacity probes above are measured from snapshots taken after the holder
      // reached local, so their per-request local hit remains exactly zero.
      assert.equal(capacityHolderResponse.status, 200, JSON.stringify(capacityHolderResponse))
      assert.equal(capacityHolderResponse.text.includes('external-ok'), true)
    }
    let schedulerRecoveryResponse = null
    let schedulerRecoveryHits = null
    let schedulerChaosRecoveryMs = null
    const schedulerChaosRecoveryProbes = []
    const schedulerChaosRecoveryRequests = []
    if (mode === 'scheduler_redis_degraded') {
      const removeToxic = await timedRequest(
        `http://127.0.0.1:${redisProxyApiPort}/proxies/redis/toxics/scheduler-latency`,
        { method: 'DELETE' },
      )
      assert.equal(removeToxic.status, 204, removeToxic.text)
      schedulerToxicInstalled = false
      await new Promise((resolve) => setTimeout(resolve, 5_000))
      const beforeRecovery = fake.snapshot()
      schedulerRecoveryResponse = await timedRequest(`${baseUrl}/v1/messages`, {
        method: 'POST',
        headers: requestHeaders(),
        body: JSON.stringify({
          model: requestModel,
          max_tokens: 32,
          stream: false,
          messages: [{ role: 'user', content: `E05 ${caseId} recovery` }],
        }),
      })
      schedulerRecoveryHits = delta(fake.snapshot(), beforeRecovery)
      assert.equal(schedulerRecoveryHits.localInferenceHits, 1, JSON.stringify({
        schedulerRecoveryResponse,
        schedulerRecoveryHits,
      }))
      assert.equal(schedulerRecoveryHits.externalHits, 1, JSON.stringify({
        schedulerRecoveryResponse,
        schedulerRecoveryHits,
      }))
      assert.equal(schedulerRecoveryResponse.status, 200, JSON.stringify(schedulerRecoveryResponse))
    }
    if (mode === 'scheduler_redis_chaos') {
      if (SCHEDULER_FAULT_KIND === 'latency') {
        const removeToxic = await timedRequest(
          `http://127.0.0.1:${redisProxyApiPort}/proxies/redis/toxics/scheduler-chaos-latency`,
          { method: 'DELETE' },
        )
        assert.equal(removeToxic.status, 204, removeToxic.text)
      } else {
        const enabled = await timedRequest(
          `http://127.0.0.1:${redisProxyApiPort}/proxies/redis`,
          {
            method: 'POST',
            headers: { 'content-type': 'application/json' },
            body: JSON.stringify({ enabled: true }),
          },
        )
        assert.equal(enabled.status, 200, enabled.text)
      }
      schedulerChaosFaultActive = false

      const recoveryStartedAt = performance.now()
      const recoveryDeadline = Date.now() + 60_000
      while (Date.now() < recoveryDeadline) {
        const beforeProbe = fake.snapshot()
        const response = await timedRequest(`${baseUrl}/v1/messages`, {
          method: 'POST',
          headers: requestHeaders(),
          body: JSON.stringify({
            model: requestModel,
            max_tokens: 32,
            stream: false,
            messages: [{ role: 'user', content: `E04 ${caseId} recovery probe` }],
          }),
        })
        const hits = delta(fake.snapshot(), beforeProbe)
        schedulerChaosRecoveryProbes.push({ response, hits })
        sampleResources(`recovery-probe-${schedulerChaosRecoveryProbes.length}`)
        if (response.status === 200
          && hits.localInferenceHits === 1
          && hits.externalHits === 0
          && response.text.includes('local-ok')) {
          schedulerChaosRecoveryMs = Number((performance.now() - recoveryStartedAt).toFixed(2))
          break
        }
        await new Promise((resolve) => setTimeout(resolve, 500))
      }
      assert.notEqual(
        schedulerChaosRecoveryMs,
        null,
        `scheduler did not recover to local routing within 60s: ${JSON.stringify(schedulerChaosRecoveryProbes)}`,
      )

      for (let recoveryIndex = 1;
        recoveryIndex <= SCHEDULER_RECOVERY_REQUESTS;
        recoveryIndex += 1) {
        const beforeRecovery = fake.snapshot()
        const response = await timedRequest(`${baseUrl}/v1/messages`, {
          method: 'POST',
          headers: requestHeaders(),
          body: JSON.stringify({
            model: requestModel,
            max_tokens: 32,
            stream: false,
            messages: [{
              role: 'user',
              content: `E04 ${caseId} stable recovery ${recoveryIndex}`,
            }],
          }),
        })
        const hits = delta(fake.snapshot(), beforeRecovery)
        const diagnostic = JSON.stringify({ recoveryIndex, response, hits })
        assert.equal(response.status, 200, diagnostic)
        assert.equal(hits.localInferenceHits, 1, diagnostic)
        assert.equal(hits.externalHits, 0, diagnostic)
        assert.equal(response.text.includes('local-ok'), true, diagnostic)
        schedulerChaosRecoveryRequests.push({ recoveryIndex, ...response, hits })
        sampleResources(`stable-recovery-${recoveryIndex}`)
      }
    }
    const resourcesPeak = resourceSamples.reduce((peak, sample) => ({
      rssKb: Math.max(peak.rssKb, sample.rssKb),
      fdCount: Math.max(peak.fdCount, sample.fdCount),
    }), resourcesStart)
    await new Promise((resolve) => setTimeout(resolve, 1_000))
    const resourcesEnd = sampleResources('idle-end')
    const hitsAfterRequests = fake.snapshot()
    const bootstrapHits = delta(hitsBeforeRequests, hitsBeforeCase)
    const caseHits = delta(hitsAfterRequests, hitsBeforeRequests)
    if (mode === 'local_ready_transient') {
      assert.equal(caseHits.externalHits, 0)
      assert.ok(caseHits.localInferenceHits <= REQUESTS_PER_ROUND * 4)
    } else if (mode === 'fallback_disabled_no_credentials') {
      assert.equal(caseHits.localInferenceHits, 0)
      assert.equal(caseHits.externalHits, 0)
    } else if (mode === 'external_error_no_loop') {
      assert.equal(caseHits.localInferenceHits, 0)
      assert.equal(caseHits.externalHits, REQUESTS_PER_ROUND)
    } else if (mode === 'scheduler_redis_chaos') {
      assert.equal(schedulerChaosRecoveryRequests.length, SCHEDULER_RECOVERY_REQUESTS)
      assert.ok(schedulerChaosRecoveryProbes.length >= 1)
      assert.ok(schedulerChaosRecoveryMs >= 0 && schedulerChaosRecoveryMs <= 60_000)
    } else if (mode === 'local_capacity_full') {
      assert.equal(caseHits.localInferenceHits, 1)
      assert.equal(caseHits.externalHits, REQUESTS_PER_ROUND + 1)
    } else if (mode === 'scheduler_redis_degraded') {
      assert.equal(caseHits.localInferenceHits, 1)
      assert.equal(caseHits.externalHits, REQUESTS_PER_ROUND + 1)
    } else if (mode === 'local_all_cooling') {
      assert.equal(caseHits.localInferenceHits, 1)
      assert.equal(caseHits.externalHits, REQUESTS_PER_ROUND)
    } else {
      assert.equal(caseHits.localInferenceHits, 0)
      assert.equal(caseHits.externalHits, REQUESTS_PER_ROUND)
    }
    assert.equal(bootstrapHits.localInferenceHits, 0)
    assert.equal(bootstrapHits.localAuxiliaryHits, expectedBootstrapAuxiliaryHits(mode))
    return {
      caseId,
      mode,
      round,
      redisKeyPrefixSha256: sha256(redisKeyPrefix),
      servicePort,
      requests,
      capacityHolderResponse,
      schedulerRecoveryResponse,
      schedulerRecoveryHits,
      schedulerChaos: mode === 'scheduler_redis_chaos' ? {
        faultKind: SCHEDULER_FAULT_KIND,
        latencyMs: SCHEDULER_FAULT_KIND === 'latency' ? SCHEDULER_LATENCY_MS : null,
        fallbackEnabled: SCHEDULER_FALLBACK_ENABLED,
        hotPathTimeoutMs: SCHEDULER_HOT_PATH_TIMEOUT_MS,
        recoveryMs: schedulerChaosRecoveryMs,
        recoveryProbes: schedulerChaosRecoveryProbes,
        recoveryRequests: schedulerChaosRecoveryRequests,
      } : null,
      bootstrapHits,
      caseHits,
      poolStatus,
      diagnosticLogLines: diagnosticLogLines(logPath),
      resources: {
        start: resourcesStart,
        peak: resourcesPeak,
        end: { rssKb: resourcesEnd.rssKb, fdCount: resourcesEnd.fdCount },
        samples: resourceSamples,
      },
    }
  } catch (error) {
    const rawTail = fs.existsSync(logPath)
      ? fs.readFileSync(logPath, 'utf8').slice(-20_000)
      : '<service log unavailable>'
    const redactedTail = rawTail
      .replaceAll(REQUEST_KEY, '<redacted-request-key>')
      .replaceAll(ADMIN_KEY, '<redacted-admin-key>')
      .replaceAll('sk-e05-fake-external', '<redacted-external-key>')
      .replace(/e05-local-token-\d+/g, '<redacted-local-token>')
    throw new Error(`${error.stack || error.message}\nservice log tail:\n${redactedTail}`)
  } finally {
    if (schedulerToxicInstalled) {
      await fetch(
        `http://127.0.0.1:${redisProxyApiPort}/proxies/redis/toxics/scheduler-latency`,
        { method: 'DELETE' },
      ).catch(() => {})
    }
    if (schedulerChaosFaultActive) {
      if (SCHEDULER_FAULT_KIND === 'latency') {
        await fetch(
          `http://127.0.0.1:${redisProxyApiPort}/proxies/redis/toxics/scheduler-chaos-latency`,
          { method: 'DELETE' },
        ).catch(() => {})
      } else {
        await fetch(
          `http://127.0.0.1:${redisProxyApiPort}/proxies/redis`,
          {
            method: 'POST',
            headers: { 'content-type': 'application/json' },
            body: JSON.stringify({ enabled: true }),
          },
        ).catch(() => {})
      }
    }
    await stopService(service)
  }
}

async function main() {
  const inputSummary = validateInputs()
  if (VALIDATE_ONLY) {
    process.stdout.write(`${JSON.stringify({
      result: 'validate-only-pass',
      modes: MODES,
      roundsPerMode: ROUNDS,
      requiredDatabases: REQUIRED_DATABASE_COUNT,
      inputSummary,
      dockerUsed: false,
      createsPostgresDatabases: false,
      flushDbUsed: false,
      protected9022ProbeSkipped: true,
    }, null, 2)}\n`)
    return
  }

  const binarySha256AtStart = sha256File(BINARY)
  fs.mkdirSync(TEMP_ROOT, { recursive: true, mode: 0o700 })
  fs.mkdirSync(REPORT_ROOT, { recursive: true })
  const localPort = await reservePort()
  const externalPort = await reservePort()
  const fake = createFakeUpstreams()
  const cases = []
  const cleanup = {
    childProcessesStopped: false,
    redisKeys: null,
    tempSecretsRemoved: false,
    portsReleased: false,
  }
  let runError = null
  let redisProxy = null

  try {
    redisProxy = await startRedisProxy(redisTarget)
    await waitForTcp(redisProxy.proxyPort)
    await fake.listen(localPort, externalPort)

    for (const [modeIndex, mode] of MODES.entries()) {
      for (let round = 1; round <= ROUNDS; round += 1) {
        const database = databaseFor(modeIndex, round)
        cases.push(await runCase({
          mode,
          round,
          postgresUrl: postgresUrlFor(database),
          redisProxyPort: redisProxy.proxyPort,
          redisProxyApiPort: redisProxy.apiPort,
          localPort,
          externalPort,
          fake,
        }))
      }
    }

    const gitRevision = run('git', ['rev-parse', 'HEAD'])
    const dirty = run('git', ['status', '--porcelain=v1'])
    const diff = run('git', ['diff', '--binary'])
    const binarySha256AtEnd = sha256File(BINARY)
    assert.equal(
      binarySha256AtEnd,
      binarySha256AtStart,
      'kiro-rs binary changed while E05 was running; discard the mixed-candidate result',
    )
    const report = {
      schemaVersion: 1,
      caseId: 'E05-strict-local-first-focused',
      runId: RUN_ID,
      generatedAt: new Date().toISOString(),
      result: 'pass',
      roundsPerMode: ROUNDS,
      requestsPerRound: REQUESTS_PER_ROUND,
      modes: MODES,
      schedulerChaosProfile: MODES.includes('scheduler_redis_chaos') ? {
        faultKind: SCHEDULER_FAULT_KIND,
        latencyMs: SCHEDULER_FAULT_KIND === 'latency' ? SCHEDULER_LATENCY_MS : null,
        fallbackEnabled: SCHEDULER_FALLBACK_ENABLED,
        recoveryRequests: SCHEDULER_RECOVERY_REQUESTS,
        hotPathTimeoutMs: SCHEDULER_HOT_PATH_TIMEOUT_MS,
      } : null,
      gitRevision,
      dirty: Boolean(dirty),
      dirtyDiffSha256: sha256(diff),
      binaryPath: path.relative(ROOT, BINARY),
      binarySha256: binarySha256AtEnd,
      isolation: {
        servicePort9022Touched: false,
        dockerUsed: false,
        createsPostgresDatabases: false,
        flushDbUsed: false,
        protected9022ProbeSkipped: true,
        postgresTemplateSha256: sha256(POSTGRES_URL_TEMPLATE),
        postgresDatabases: POSTGRES_DATABASES.map((database) => sha256(database)),
        redisAuthority: {
          host: inputSummary.redisHost,
          port: inputSummary.redisPort,
          database: inputSummary.redisDatabase,
          prefixSha256: inputSummary.redisPrefixSha256,
        },
        redisProxyPort: redisProxy.proxyPort,
        redisProxyApiPort: redisProxy.apiPort,
        localFakePort: localPort,
        externalFakePort: externalPort,
      },
      cases,
      cleanup,
    }
    fs.writeFileSync(REPORT_PATH, `${JSON.stringify(report, null, 2)}\n`)
  } catch (error) {
    runError = error
    const gitRevision = run('git', ['rev-parse', 'HEAD'])
    const dirty = run('git', ['status', '--porcelain=v1'])
    const diff = run('git', ['diff', '--binary'])
    const binarySha256AtEnd = sha256File(BINARY)
    const failure = String(error?.stack || error?.message || error).slice(0, 64 * 1024)
    const report = {
      schemaVersion: 1,
      caseId: 'E05-strict-local-first-focused',
      runId: RUN_ID,
      generatedAt: new Date().toISOString(),
      result: 'fail',
      roundsPerMode: ROUNDS,
      requestsPerRound: REQUESTS_PER_ROUND,
      modes: MODES,
      schedulerChaosProfile: MODES.includes('scheduler_redis_chaos') ? {
        faultKind: SCHEDULER_FAULT_KIND,
        latencyMs: SCHEDULER_FAULT_KIND === 'latency' ? SCHEDULER_LATENCY_MS : null,
        fallbackEnabled: SCHEDULER_FALLBACK_ENABLED,
        recoveryRequests: SCHEDULER_RECOVERY_REQUESTS,
        hotPathTimeoutMs: SCHEDULER_HOT_PATH_TIMEOUT_MS,
      } : null,
      gitRevision,
      dirty: Boolean(dirty),
      dirtyDiffSha256: sha256(diff),
      binaryPath: path.relative(ROOT, BINARY),
      binarySha256: binarySha256AtEnd,
      binaryStableDuringRun: binarySha256AtEnd === binarySha256AtStart,
      failure: { message: failure },
      isolation: {
        servicePort9022Touched: false,
        dockerUsed: false,
        createsPostgresDatabases: false,
        flushDbUsed: false,
        protected9022ProbeSkipped: true,
        postgresTemplateSha256: sha256(POSTGRES_URL_TEMPLATE),
        postgresDatabases: POSTGRES_DATABASES.map((database) => sha256(database)),
        redisAuthority: {
          host: inputSummary.redisHost,
          port: inputSummary.redisPort,
          database: inputSummary.redisDatabase,
          prefixSha256: inputSummary.redisPrefixSha256,
        },
        redisProxyPort: redisProxy?.proxyPort || null,
        redisProxyApiPort: redisProxy?.apiPort || null,
        localFakePort: localPort,
        externalFakePort: externalPort,
      },
      cases,
      cleanup,
    }
    fs.writeFileSync(REPORT_PATH, `${JSON.stringify(report, null, 2)}\n`)
  } finally {
    await fake.close().catch(() => {})
    await Promise.all([...ACTIVE_CHILDREN].map((child) => stopChild(child, 'owned validation child').catch(() => {})))
    cleanup.childProcessesStopped = ACTIVE_CHILDREN.size === 0
    cleanup.redisKeys = await cleanupOwnedRedisKeys().catch((error) => ({
      error: String(error?.message || error),
    }))
    fs.rmSync(TEMP_ROOT, { recursive: true, force: true })
    cleanup.tempSecretsRemoved = !fs.existsSync(TEMP_ROOT)
    const ownedPorts = [
      redisProxy?.proxyPort,
      redisProxy?.apiPort,
      localPort,
      externalPort,
    ].filter((port) => Number.isSafeInteger(port) && port !== 0)
    cleanup.portsReleased = (await Promise.all(
      ownedPorts.map(async (port) => {
        try {
          await waitForTcp(port, 250)
          return false
        } catch {
          return true
        }
      }),
    )).every(Boolean)
    if (fs.existsSync(REPORT_PATH)) {
      const report = JSON.parse(fs.readFileSync(REPORT_PATH, 'utf8'))
      report.cleanup = cleanup
      const cleanupPass = cleanup.childProcessesStopped
        && cleanup.tempSecretsRemoved
        && cleanup.portsReleased
        && cleanup.redisKeys
        && cleanup.redisKeys.remaining === 0
      report.result = cleanupPass ? report.result : 'fail'
      fs.writeFileSync(REPORT_PATH, `${JSON.stringify(report, null, 2)}\n`)
    }
  }

  if (runError) {
    throw new Error(`${runError.stack || runError.message || runError}\nreport: ${REPORT_PATH}`)
  }
  const report = JSON.parse(fs.readFileSync(REPORT_PATH, 'utf8'))
  assert.equal(report.result, 'pass')
  assert.equal(report.cleanup.childProcessesStopped, true)
  assert.equal(report.cleanup.tempSecretsRemoved, true)
  assert.equal(report.cleanup.portsReleased, true)
  assert.equal(report.cleanup.redisKeys.remaining, 0)
  process.stdout.write(`${REPORT_PATH}\n`)
}

main().catch((error) => {
  process.stderr.write(`E05 strict local-first validation failed: ${error.stack || error.message}\n`)
  process.exitCode = 1
})
