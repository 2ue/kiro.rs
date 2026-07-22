#!/usr/bin/env node

/*
 * Product-level external takeover gate for SchedulerRedisDegraded without
 * Docker or Cargo. The caller must provide a frozen kiro.rs binary, an owned
 * artifact directory, a pre-created caller-owned PostgreSQL database, and a
 * loopback Redis DB/prefix. The runner starts only:
 *
 * - fake local Kiro upstream;
 * - fake external OpenAI/Anthropic-compatible upstream;
 * - the repository's loopback redis-chaos-proxy;
 * - one temporary kiro.rs process on a random non-9022 port.
 */

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

const ROOT = fs.realpathSync(path.resolve(import.meta.dirname, '../..'))
const { binary: BINARY, artifactRoot: ARTIFACT_ROOT } = resolveRuntimeValidationPaths(ROOT)
const POSTGRES_URL = String(process.env.KIRO_EXTERNAL_TAKEOVER_POSTGRES_URL || '').trim()
const POSTGRES_URL_TEMPLATE = String(
  process.env.KIRO_EXTERNAL_TAKEOVER_POSTGRES_URL_TEMPLATE || '',
).trim()
const POSTGRES_DATABASES = String(process.env.KIRO_EXTERNAL_TAKEOVER_POSTGRES_DATABASES || '')
  .split(',')
  .map((value) => value.trim())
  .filter(Boolean)
const REDIS_URL = requiredEnvironment('KIRO_EXTERNAL_TAKEOVER_REDIS_URL')
const REDIS_PREFIX = requiredEnvironment('KIRO_EXTERNAL_TAKEOVER_REDIS_PREFIX')
const OUTER_ROUNDS = boundedInteger('KIRO_EXTERNAL_TAKEOVER_OUTER_ROUNDS', 3, 1, 5)
const REQUESTS_PER_ROUND = boundedInteger('KIRO_EXTERNAL_TAKEOVER_REQUESTS', 5, 3, 10)
const RECOVERY_REQUESTS = boundedInteger('KIRO_EXTERNAL_TAKEOVER_RECOVERY_REQUESTS', 5, 5, 20)
const LATENCY_MS = boundedInteger('KIRO_EXTERNAL_TAKEOVER_REDIS_LATENCY_MS', 500, 251, 2_000)
const FALLBACK_ENABLED = process.env.KIRO_EXTERNAL_TAKEOVER_FALLBACK_ENABLED !== 'false'
const VALIDATE_ONLY = process.env.KIRO_EXTERNAL_TAKEOVER_VALIDATE_ONLY === '1'
const REQUEST_KEY = 'sk-external-takeover-request'
const ADMIN_KEY = 'sk-external-takeover-admin'
const EXTERNAL_KEY = 'sk-external-takeover-fake-external'
const MODEL = 'claude-sonnet-4'
const RUN_ID = `external-takeover-${new Date().toISOString().replace(/[-:.TZ]/g, '')}-${process.pid}-${crypto.randomBytes(3).toString('hex')}`
const TEMP_ROOT = fs.mkdtempSync(path.join(os.tmpdir(), `${RUN_ID}-`))
const REPORT_ROOT = path.join(ARTIFACT_ROOT, 'reports', 'external-takeover-scheduler-degraded')
const REPORT_PATH = path.join(REPORT_ROOT, `${RUN_ID}.json`)
const ACTIVE_CHILDREN = new Set()
const ACTIVE_SERVERS = new Set()
let cleanupStarted = false
let redisTarget = null
let fake = null
let service = null
let proxyProcess = null

function collectFailureDiagnostics(error) {
  const serviceLogs = []
  try {
    if (fs.existsSync(TEMP_ROOT)) {
      const stack = [TEMP_ROOT]
      while (stack.length > 0) {
        const current = stack.pop()
        for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
          const fullPath = path.join(current, entry.name)
          if (entry.isDirectory()) {
            stack.push(fullPath)
          } else if (entry.isFile() && entry.name === 'service.log') {
            const raw = fs.readFileSync(fullPath, 'utf8')
            const redacted = redactUnbounded(raw)
            serviceLogs.push({
              relativePath: path.relative(TEMP_ROOT, fullPath),
              redactedTail: redacted.slice(-120_000),
              sha256: sha256(redacted),
            })
          }
        }
      }
    }
  } catch (diagnosticError) {
    serviceLogs.push({ diagnosticError: String(diagnosticError?.message || diagnosticError) })
  }
  return {
    result: 'fail',
    runId: RUN_ID,
    error: redact(error?.stack || error?.message || error),
    fallbackEnabled: FALLBACK_ENABLED,
    redisLatencyMs: LATENCY_MS,
    binarySha256: fs.existsSync(BINARY) ? sha256(fs.readFileSync(BINARY)) : null,
    fakeSnapshot: fake ? fake.snapshot() : null,
    serviceExitCode: service ? service.exitCode : null,
    proxyExitCode: proxyProcess ? proxyProcess.exitCode : null,
    dockerUsed: false,
    cargoUsed: false,
    protected9022ProbeSkipped: true,
    serviceLogs,
  }
}

function writeFailureReport(report) {
  try {
    fs.mkdirSync(REPORT_ROOT, { recursive: true, mode: 0o700 })
    const failurePath = path.join(REPORT_ROOT, `${RUN_ID}.failure.json`)
    fs.writeFileSync(failurePath, `${JSON.stringify(report, null, 2)}\n`, { mode: 0o600 })
    return failurePath
  } catch (error) {
    return null
  }
}

function requiredEnvironment(name) {
  const value = String(process.env[name] || '').trim()
  if (!value) throw new Error(`${name} is required`)
  return value
}

function boundedInteger(name, fallback, minimum, maximum) {
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

function redact(value) {
  let text = String(value || '')
  for (const secret of [
    POSTGRES_URL,
    POSTGRES_URL_TEMPLATE,
    ...POSTGRES_DATABASES,
    REDIS_URL,
    REQUEST_KEY,
    ADMIN_KEY,
    EXTERNAL_KEY,
  ]) {
    if (secret) text = text.split(secret).join('<redacted>')
  }
  text = text.replace(/external-takeover-token-\d+/g, '<redacted-local-token>')
  if (text.length <= 4000) return text
  return `${text.slice(0, 2000)}\n<diagnostic_truncated chars=${text.length}>\n${text.slice(-2000)}`
}

function redactUnbounded(value) {
  let text = String(value || '')
  for (const secret of [
    POSTGRES_URL,
    POSTGRES_URL_TEMPLATE,
    ...POSTGRES_DATABASES,
    REDIS_URL,
    REQUEST_KEY,
    ADMIN_KEY,
    EXTERNAL_KEY,
  ]) {
    if (secret) text = text.split(secret).join('<redacted>')
  }
  return text.replace(/external-takeover-token-\d+/g, '<redacted-local-token>')
}

function validateInputs() {
  const postgresUrls = resolvePostgresUrls()
  const postgres = new URL(postgresUrls[0])
  if (!['postgres:', 'postgresql:'].includes(postgres.protocol)) {
    throw new Error('KIRO_EXTERNAL_TAKEOVER PostgreSQL URL must use PostgreSQL')
  }
  if (!['127.0.0.1', 'localhost', '::1'].includes(postgres.hostname)) {
    throw new Error('KIRO_EXTERNAL_TAKEOVER PostgreSQL URL must target loopback')
  }
  if (Number(postgres.port || 5432) === 9022) throw new Error('port 9022 is protected')
  for (const postgresUrl of postgresUrls) {
    const parsedPostgres = new URL(postgresUrl)
    if (parsedPostgres.protocol !== postgres.protocol
      || parsedPostgres.hostname !== postgres.hostname
      || Number(parsedPostgres.port || 5432) !== Number(postgres.port || 5432)
      || parsedPostgres.username !== postgres.username) {
      throw new Error('KIRO_EXTERNAL_TAKEOVER PostgreSQL round databases must share one loopback authority')
    }
    const database = decodeURIComponent(parsedPostgres.pathname.replace(/^\//, ''))
    if (!/^kiro_external_takeover_[a-z0-9_]{3,80}$/.test(database)) {
      throw new Error('KIRO_EXTERNAL_TAKEOVER PostgreSQL inputs must name caller-owned kiro_external_takeover_* databases')
    }
  }

  const redis = new URL(REDIS_URL)
  if (redis.protocol !== 'redis:') throw new Error('KIRO_EXTERNAL_TAKEOVER_REDIS_URL must use redis://')
  if (redis.username || redis.password) {
    throw new Error('KIRO_EXTERNAL_TAKEOVER_REDIS_URL must not contain Redis auth material')
  }
  if (!['127.0.0.1', 'localhost', '::1'].includes(redis.hostname)) {
    throw new Error('KIRO_EXTERNAL_TAKEOVER_REDIS_URL must target loopback')
  }
  if (redis.search || redis.hash) throw new Error('KIRO_EXTERNAL_TAKEOVER_REDIS_URL must not contain query or fragment data')
  const redisPort = Number(redis.port || 6379)
  if (redisPort === 9022) throw new Error('port 9022 is protected')
  const dbText = redis.pathname.replace(/^\//, '')
  if (!/^\d+$/.test(dbText)) throw new Error('KIRO_EXTERNAL_TAKEOVER_REDIS_URL must name a Redis database')
  const redisDatabase = Number(dbText)
  if (!Number.isSafeInteger(redisDatabase) || redisDatabase < 1 || redisDatabase > 15) {
    throw new Error('KIRO_EXTERNAL_TAKEOVER_REDIS_URL must use an isolated nonzero database in 1..15')
  }
  if (REDIS_PREFIX.includes('kiro_rs:local')) {
    throw new Error('KIRO_EXTERNAL_TAKEOVER_REDIS_PREFIX must be a caller-owned temporary prefix')
  }
  if (!/^[a-z0-9][a-z0-9:._-]{7,95}$/.test(REDIS_PREFIX)) {
    throw new Error('KIRO_EXTERNAL_TAKEOVER_REDIS_PREFIX has an invalid format')
  }
  return {
    postgres,
    postgresDatabase: decodeURIComponent(postgres.pathname.replace(/^\//, '')),
    postgresDatabaseCount: postgresUrls.length,
    redis,
    redisPort,
    redisDatabase,
  }
}

function resolvePostgresUrls() {
  if (POSTGRES_URL_TEMPLATE) {
    const placeholderCount = (POSTGRES_URL_TEMPLATE.match(/\{database\}/g) || []).length
    if (placeholderCount !== 1) {
      throw new Error('KIRO_EXTERNAL_TAKEOVER_POSTGRES_URL_TEMPLATE must contain exactly one literal {database} placeholder')
    }
    if (POSTGRES_URL) {
      throw new Error('provide either KIRO_EXTERNAL_TAKEOVER_POSTGRES_URL or KIRO_EXTERNAL_TAKEOVER_POSTGRES_URL_TEMPLATE, not both')
    }
    if (POSTGRES_DATABASES.length !== OUTER_ROUNDS) {
      throw new Error(`KIRO_EXTERNAL_TAKEOVER_POSTGRES_DATABASES must contain exactly ${OUTER_ROUNDS} pre-created database names`)
    }
    return POSTGRES_DATABASES.map((database) => {
      if (!/^kiro_external_takeover_[a-z0-9_]{3,80}$/.test(database)) {
        throw new Error('KIRO_EXTERNAL_TAKEOVER_POSTGRES_DATABASES must contain caller-owned kiro_external_takeover_* names')
      }
      return POSTGRES_URL_TEMPLATE.replace('{database}', encodeURIComponent(database))
    })
  }
  if (!POSTGRES_URL) {
    throw new Error('KIRO_EXTERNAL_TAKEOVER_POSTGRES_URL or KIRO_EXTERNAL_TAKEOVER_POSTGRES_URL_TEMPLATE is required')
  }
  if (OUTER_ROUNDS !== 1) {
    throw new Error('single KIRO_EXTERNAL_TAKEOVER_POSTGRES_URL is only valid with KIRO_EXTERNAL_TAKEOVER_OUTER_ROUNDS=1; provide URL_TEMPLATE plus DATABASES for multi-round isolation')
  }
  return [POSTGRES_URL]
}

function postgresUrlForRound(round) {
  return resolvePostgresUrls()[round - 1]
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
    // Drain before replying.
  }
}

function createFakeUpstreams() {
  const state = {
    localInferenceHits: 0,
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
    state.records.push({ channel: 'local', kind, path: request.url, target: target || null })
    if (kind === 'auxiliary') {
      state.localAuxiliaryHits += 1
      writeJson(response, 200, {
        defaultModel: { modelId: MODEL },
        models: [{
          modelId: MODEL,
          modelName: 'Claude Sonnet 4',
          supportedInputTypes: ['TEXT'],
        }],
      })
      return
    }
    state.localInferenceHits += 1
    writeKiroSuccess(response, 'local-ok')
  })
  const external = http.createServer(async (request, response) => {
    await consumeRequest(request)
    state.externalHits += 1
    state.records.push({ channel: 'external', kind: 'inference', path: request.url })
    writeJson(response, 200, {
      id: `msg_external_${state.externalHits}`,
      type: 'message',
      role: 'assistant',
      model: MODEL,
      content: [{ type: 'text', text: 'external-ok' }],
      stop_reason: 'end_turn',
      stop_sequence: null,
      usage: { input_tokens: 8, output_tokens: 2 },
    })
  })
  ACTIVE_SERVERS.add(local)
  ACTIVE_SERVERS.add(external)
  return {
    state,
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
      await Promise.all([closeServer(local), closeServer(external)])
    },
  }
}

function delta(after, before) {
  return Object.fromEntries(Object.keys(after).map((key) => [key, after[key] - before[key]]))
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

async function waitForHealth(baseUrl, handle, timeoutMs = 60_000) {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    if (handle.exitCode !== null) throw new Error(`kiro-rs exited before health check: ${handle.exitCode}`)
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

function adminHeaders() {
  return { authorization: `Bearer ${ADMIN_KEY}`, 'content-type': 'application/json' }
}

function requestHeaders() {
  return { 'x-api-key': REQUEST_KEY, 'content-type': 'application/json' }
}

function credentials() {
  return Array.from({ length: 4 }, (_, index) => ({
    accessToken: `external-takeover-token-${index + 1}`,
    expiresAt: '2099-01-01T00:00:00Z',
    authMethod: 'social',
    endpoint: 'ide',
    priority: 0,
    maxConcurrentRequests: 10,
    rpm: 0,
    supportedModels: [MODEL],
  }))
}

function startChild(command, args, options = {}) {
  const child = spawn(command, args, {
    cwd: ROOT,
    stdio: options.stdio || ['ignore', 'pipe', 'pipe'],
    env: validationChildEnvironment(options.env || {}),
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
    const timer = setTimeout(() => reject(new Error(`redis proxy did not become ready: ${redact(stderr)}`)), 10_000)
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
      reject(new Error(`redis proxy exited before ready code=${code}: ${redact(stderr)}`))
    })
  })
  assert.equal(ready.protected9022ProbeSkipped, true)
  return { child, ...ready }
}

function startService(configPath, credentialsPath, logPath, servicePort) {
  const log = fs.openSync(logPath, 'a')
  const handle = spawn(BINARY, ['--config', configPath, '--credentials', credentialsPath], {
    cwd: ROOT,
    env: validationChildEnvironment({
      RUST_LOG: 'info',
      KIRO_API_KEY: '',
      KIRO_RS_HOST: '127.0.0.1',
      KIRO_RS_PORT: String(servicePort),
    }),
    stdio: ['ignore', log, log],
  })
  ACTIVE_CHILDREN.add(handle)
  handle.once('exit', () => {
    ACTIVE_CHILDREN.delete(handle)
    fs.closeSync(log)
  })
  return handle
}

async function stopChild(handle, label) {
  if (!handle || handle.exitCode !== null) return
  handle.kill('SIGTERM')
  const exited = await Promise.race([
    new Promise((resolve) => handle.once('exit', () => resolve(true))),
    new Promise((resolve) => setTimeout(() => resolve(false), 8_000)),
  ])
  if (!exited && handle.exitCode === null) {
    handle.kill('SIGKILL')
    await new Promise((resolve) => handle.once('exit', resolve))
  }
  if (handle.exitCode === null) throw new Error(`failed to stop ${label}`)
}

async function closeServer(server) {
  if (!server || !server.listening) return
  await new Promise((resolve) => server.close(resolve))
  ACTIVE_SERVERS.delete(server)
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
  if (type === '+' || type === '-' || type === ':') return { type, value: type === ':' ? Number(line) : line, next }
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

async function redisCommand(parts) {
  assert(redisTarget)
  const commands = []
  if (redisTarget.redisDatabase !== 0) commands.push(['SELECT', String(redisTarget.redisDatabase)])
  commands.push(parts)
  return await new Promise((resolve, reject) => {
    const socket = net.connect({ host: redisTarget.redis.hostname, port: redisTarget.redisPort })
    const chunks = []
    socket.setTimeout(5_000)
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

async function scanOwnedRedisKeys(limit = 10_000) {
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
    if (chunk.length === 0) continue
    removed += Number(await redisCommand(['DEL', ...chunk])) || 0
  }
  const remaining = (await scanOwnedRedisKeys()).length
  return { removed, remaining }
}

async function installRedisLatency(apiPort) {
  const response = await timedRequest(`http://127.0.0.1:${apiPort}/proxies/redis/toxics`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      name: 'scheduler-latency',
      type: 'latency',
      stream: 'downstream',
      toxicity: 1,
      attributes: { latency: LATENCY_MS, jitter: 0 },
    }),
  })
  assert.ok([200, 201].includes(response.status), response.text)
}

async function removeRedisLatency(apiPort) {
  const response = await timedRequest(`http://127.0.0.1:${apiPort}/proxies/redis/toxics/scheduler-latency`, {
    method: 'DELETE',
  })
  assert.equal(response.status, 204, response.text)
}

function responseHasInternalRoutingText(text) {
  return /redis|credential|scheduler|external[ _-]?pool|fallback|lease|dispatch/i.test(text)
}

async function runRound(round, target) {
  const localPort = await reservePort()
  const externalPort = await reservePort()
  const servicePort = await reservePort()
  fake = createFakeUpstreams()
  await fake.listen(localPort, externalPort)
  const redisProxy = await startRedisProxy(target)
  proxyProcess = redisProxy.child
  const caseRoot = path.join(TEMP_ROOT, `round-${round}`)
  fs.mkdirSync(caseRoot, { recursive: true, mode: 0o700 })
  const configPath = path.join(caseRoot, 'config.json')
  const credentialsPath = path.join(caseRoot, 'credentials.json')
  const logPath = path.join(caseRoot, 'service.log')
  const baseUrl = `http://127.0.0.1:${servicePort}`
  const config = {
    postgres: { url: postgresUrlForRound(round), maxConnections: 4, migrateOnStart: true },
    redis: { url: `redis://127.0.0.1:${redisProxy.proxyPort}/${target.redisDatabase}`, keyPrefix: REDIS_PREFIX },
    host: '127.0.0.1',
    port: servicePort,
    apiKey: REQUEST_KEY,
    adminApiKey: ADMIN_KEY,
    defaultEndpoint: 'ide',
    kiroUpstreamBaseUrl: `http://127.0.0.1:${localPort}/kiro`,
    kiroUpstreamResponseTimeoutSecs: 5,
    credentialRetryMaxAttempts: 2,
    inferenceUpstreamMaxAttempts: 2,
    credentialPromptLogicRetryEnabled: false,
    credentialWarmupRequests: 0,
    credentialMaxConcurrentRequests: 10,
    externalPools: {
      externalPoolsEnabled: true,
      externalPoolRetryMaxAttempts: 0,
      externalPoolRequestTimeoutSecs: 5,
      externalPoolCapacityMode: 'fail_fast',
      externalPoolLocalRescueEnabled: false,
      externalPoolAutoDisableEnabled: false,
      fallbackOnLocalCapacityExhausted: FALLBACK_ENABLED,
      fallbackOnSchedulerRedisDegraded: FALLBACK_ENABLED,
      fallbackOnNoAvailableCredentials: FALLBACK_ENABLED,
      fallbackOnLocalTransientExhausted: FALLBACK_ENABLED,
      fallbackOnUnsupportedModel: false,
      localPoolPreflightEnabled: true,
    },
  }
  fs.writeFileSync(configPath, `${JSON.stringify(config, null, 2)}\n`, { mode: 0o600 })
  fs.writeFileSync(credentialsPath, `${JSON.stringify(credentials(), null, 2)}\n`, { mode: 0o600 })

  service = startService(configPath, credentialsPath, logPath, servicePort)
  await waitForHealth(baseUrl, service)
  const resourcesStart = processResources(service.pid)

  const createPool = await timedRequest(`${baseUrl}/api/admin/external-pools`, {
    method: 'POST',
    headers: adminHeaders(),
    body: JSON.stringify({
      name: `external-takeover-${RUN_ID}-${round}`,
      baseUrl: `http://127.0.0.1:${externalPort}/external`,
      apiKey: EXTERNAL_KEY,
      authType: 'bearer',
      enabled: true,
      priority: 0,
      maxConcurrentRequests: 10,
      usageProjectionMode: 'pass_through',
      requestBodyMode: 'normalized',
      rawModelMode: 'none',
      preservePath: true,
      supportedModels: [MODEL],
    }),
  })
  assert.equal(createPool.status, 200, createPool.text)
  await new Promise((resolve) => setTimeout(resolve, 350))
  await installRedisLatency(redisProxy.apiPort)
  await new Promise((resolve) => setTimeout(resolve, 350))

  const requests = []
  const hitsBeforeFault = fake.snapshot()
  for (let requestIndex = 1; requestIndex <= REQUESTS_PER_ROUND; requestIndex += 1) {
    const before = fake.snapshot()
    const response = await timedRequest(`${baseUrl}/v1/messages`, {
      method: 'POST',
      headers: requestHeaders(),
      body: JSON.stringify({
        model: MODEL,
        max_tokens: 32,
        stream: false,
        messages: [{ role: 'user', content: `external takeover round ${round} request ${requestIndex}` }],
      }),
    })
    const hits = delta(fake.snapshot(), before)
    const diagnostic = JSON.stringify({ round, requestIndex, response, hits })
    if (FALLBACK_ENABLED) {
      assert.equal(response.status, 200, diagnostic)
      assert.equal(hits.localInferenceHits, 0, diagnostic)
      assert.equal(hits.externalHits, 1, diagnostic)
      assert.equal(response.text.includes('external-ok'), true, diagnostic)
    } else {
      assert.notEqual(response.status, 200, diagnostic)
      assert.equal(hits.localInferenceHits, 0, diagnostic)
      assert.equal(hits.externalHits, 0, diagnostic)
      assert.ok(response.requestId, diagnostic)
      assert.equal(responseHasInternalRoutingText(response.text), false, diagnostic)
    }
    requests.push({ requestIndex, ...response, hits })
  }
  const faultHits = delta(fake.snapshot(), hitsBeforeFault)

  await removeRedisLatency(redisProxy.apiPort)
  const recoveryProbes = []
  let recoveryMs = null
  const recoveryStartedAt = performance.now()
  const recoveryDeadline = Date.now() + 60_000
  while (Date.now() < recoveryDeadline) {
    const before = fake.snapshot()
    const response = await timedRequest(`${baseUrl}/v1/messages`, {
      method: 'POST',
      headers: requestHeaders(),
      body: JSON.stringify({
        model: MODEL,
        max_tokens: 32,
        stream: false,
        messages: [{ role: 'user', content: `external takeover recovery probe ${round}` }],
      }),
    })
    const hits = delta(fake.snapshot(), before)
    recoveryProbes.push({ response, hits })
    if (response.status === 200
      && hits.localInferenceHits === 1
      && hits.externalHits === 0
      && response.text.includes('local-ok')) {
      recoveryMs = Number((performance.now() - recoveryStartedAt).toFixed(2))
      break
    }
    await new Promise((resolve) => setTimeout(resolve, 500))
  }
  assert.notEqual(recoveryMs, null, `scheduler did not recover to local routing: ${JSON.stringify(recoveryProbes)}`)

  const stableRecovery = []
  for (let recoveryIndex = 1; recoveryIndex <= RECOVERY_REQUESTS; recoveryIndex += 1) {
    const before = fake.snapshot()
    const response = await timedRequest(`${baseUrl}/v1/messages`, {
      method: 'POST',
      headers: requestHeaders(),
      body: JSON.stringify({
        model: MODEL,
        max_tokens: 32,
        stream: false,
        messages: [{ role: 'user', content: `external takeover stable recovery ${round}-${recoveryIndex}` }],
      }),
    })
    const hits = delta(fake.snapshot(), before)
    const diagnostic = JSON.stringify({ round, recoveryIndex, response, hits })
    assert.equal(response.status, 200, diagnostic)
    assert.equal(hits.localInferenceHits, 1, diagnostic)
    assert.equal(hits.externalHits, 0, diagnostic)
    assert.equal(response.text.includes('local-ok'), true, diagnostic)
    stableRecovery.push({ recoveryIndex, ...response, hits })
  }

  const resourcesEnd = processResources(service.pid)
  await stopChild(service, 'kiro-rs')
  service = null
  await stopChild(proxyProcess, 'redis proxy')
  proxyProcess = null
  await fake.close()
  fake = null
  const redisCleanup = await cleanupOwnedRedisKeys()

  return {
    round,
    fallbackEnabled: FALLBACK_ENABLED,
    latencyMs: LATENCY_MS,
    servicePort,
    redisProxyPort: redisProxy.proxyPort,
    redisProxyApiPort: redisProxy.apiPort,
    requests,
    faultHits,
    recoveryMs,
    recoveryProbeCount: recoveryProbes.length,
    stableRecovery,
    resources: { start: resourcesStart, end: resourcesEnd },
    redisCleanup,
    logTailSha256: fs.existsSync(logPath) ? sha256(redact(fs.readFileSync(logPath, 'utf8')).slice(-20_000)) : null,
  }
}

async function cleanup() {
  if (cleanupStarted) return { alreadyStarted: true }
  cleanupStarted = true
  const errors = []
  try { await stopChild(service, 'kiro-rs') } catch (error) { errors.push(String(error?.message || error)) }
  try { await stopChild(proxyProcess, 'redis proxy') } catch (error) { errors.push(String(error?.message || error)) }
  if (fake) {
    try { await fake.close() } catch (error) { errors.push(String(error?.message || error)) }
  }
  for (const server of [...ACTIVE_SERVERS]) {
    try { await closeServer(server) } catch (error) { errors.push(String(error?.message || error)) }
  }
  for (const child of [...ACTIVE_CHILDREN]) {
    try { await stopChild(child, 'child') } catch (error) { errors.push(String(error?.message || error)) }
  }
  let redisCleanup = null
  try { redisCleanup = await cleanupOwnedRedisKeys() } catch (error) { errors.push(String(error?.message || error)) }
  try { fs.rmSync(TEMP_ROOT, { recursive: true, force: true }) } catch (error) { errors.push(String(error?.message || error)) }
  return { errors, redisCleanup, tempRemoved: !fs.existsSync(TEMP_ROOT) }
}

for (const signal of ['SIGHUP', 'SIGINT', 'SIGTERM']) {
  process.on(signal, () => {
    void cleanup().finally(() => {
      process.exit(signal === 'SIGHUP' ? 129 : signal === 'SIGINT' ? 130 : 143)
    })
  })
}

async function main() {
  redisTarget = validateInputs()
  if (VALIDATE_ONLY) {
    console.log(JSON.stringify({
      result: 'validate_only',
      dockerUsed: false,
      cargoUsed: false,
      protected9022ProbeSkipped: true,
      postgresDatabase: redisTarget.postgresDatabase,
      postgresDatabaseCount: redisTarget.postgresDatabaseCount,
      redisDatabase: redisTarget.redisDatabase,
      redisPrefixSha256: sha256(REDIS_PREFIX),
    }, null, 2))
    return
  }
  const reports = []
  for (let round = 1; round <= OUTER_ROUNDS; round += 1) {
    reports.push(await runRound(round, redisTarget))
  }
  const finalCleanup = await cleanup()
  const report = {
    result: 'pass',
    runId: RUN_ID,
    rounds: OUTER_ROUNDS,
    requestsPerRound: REQUESTS_PER_ROUND,
    recoveryRequests: RECOVERY_REQUESTS,
    fallbackEnabled: FALLBACK_ENABLED,
    redisLatencyMs: LATENCY_MS,
    binarySha256: sha256(fs.readFileSync(BINARY)),
    postgresDatabase: redisTarget.postgresDatabase,
    redisDatabase: redisTarget.redisDatabase,
    redisPrefixSha256: sha256(REDIS_PREFIX),
    dockerUsed: false,
    cargoUsed: false,
    protected9022ProbeSkipped: true,
    reports,
    cleanup: finalCleanup,
  }
  fs.mkdirSync(REPORT_ROOT, { recursive: true, mode: 0o700 })
  fs.writeFileSync(REPORT_PATH, `${JSON.stringify(report, null, 2)}\n`, { mode: 0o600 })
  console.log(JSON.stringify({
    result: 'pass',
    runId: RUN_ID,
    reportPath: REPORT_PATH,
    rounds: OUTER_ROUNDS,
    fallbackEnabled: FALLBACK_ENABLED,
    redisLatencyMs: LATENCY_MS,
    dockerUsed: false,
    cargoUsed: false,
    protected9022ProbeSkipped: true,
    cleanup: finalCleanup,
  }, null, 2))
}

main().catch(async (error) => {
  const failureReport = collectFailureDiagnostics(error)
  const failureReportPath = writeFailureReport(failureReport)
  const finalCleanup = await cleanup()
  console.error(JSON.stringify({
    result: 'fail',
    error: failureReport.error,
    failureReportPath,
    cleanup: finalCleanup,
    dockerUsed: false,
    cargoUsed: false,
    protected9022ProbeSkipped: true,
  }, null, 2))
  process.exitCode = 1
})
