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

const ROOT = fs.realpathSync(path.resolve(import.meta.dirname, '../..'))
const { binary: BINARY, artifactRoot: ARTIFACT_ROOT } = resolveRuntimeValidationPaths(ROOT)
const ROUNDS = Number.parseInt(process.env.KIRO_REQUEST_ADMISSION_ROUNDS || '5', 10)
const PROBES_PER_INSTANCE = Number.parseInt(
  process.env.KIRO_REQUEST_ADMISSION_PROBES || '32',
  10,
)
const STABILITY_WAVES = Number.parseInt(
  process.env.KIRO_REQUEST_ADMISSION_STABILITY_WAVES || '5',
  10,
)
const MAX_REJECTION_P95_MS = Number.parseFloat(
  process.env.KIRO_REQUEST_ADMISSION_MAX_P95_MS || '250',
)
const POSTGRES_URL_TEMPLATE = requiredEnvironment('KIRO_REQUEST_ADMISSION_POSTGRES_URL_TEMPLATE')
const REDIS_URL = requiredEnvironment('KIRO_REQUEST_ADMISSION_REDIS_URL')
const REDIS_PREFIX = requiredEnvironment('KIRO_REQUEST_ADMISSION_REDIS_PREFIX')
const VALIDATE_ONLY = process.env.KIRO_REQUEST_ADMISSION_VALIDATE_ONLY === '1'
const REQUIRED_DATABASE_COUNT = ROUNDS
const POSTGRES_DATABASES = String(process.env.KIRO_REQUEST_ADMISSION_POSTGRES_DATABASES || '')
  .split(',')
  .map((value) => value.trim())
  .filter(Boolean)

// All secrets in this runner are isolated fixtures. Reports and error output redact them anyway.
const REQUEST_KEYS = Object.freeze({
  a: 'sk-ra-isolated-channel-a',
  b: 'sk-ra-isolated-channel-b',
  c: 'sk-ra-isolated-channel-c',
})
const ADMIN_KEY = 'sk-ra-isolated-admin'
const LOCAL_TOKEN = 'ra-isolated-local-token'
const REQUEST_KEY_IDS = Object.freeze(Object.fromEntries(
  Object.entries(REQUEST_KEYS).map(([name, value]) => [name, sha256(value)]),
))
const RUN_ID = `request-admission-${new Date().toISOString().replace(/[-:.TZ]/g, '')}-${process.pid}-${crypto.randomBytes(3).toString('hex')}`
const TEMP_ROOT = path.join(ARTIFACT_ROOT, 'runtime', 'request-admission-multi-instance', RUN_ID)
const REPORT_ROOT = path.join(ARTIFACT_ROOT, 'reports', 'request-admission-multi-instance')
const REPORT_PATH = path.join(REPORT_ROOT, `${RUN_ID}.json`)
let redisTarget = null
let redisProxyProcess = null

const INITIAL_ADMISSION = Object.freeze({
  rpm: 4,
  maxConcurrentRequests: 0,
  maxQueuedRequests: 0,
  queueTimeoutMs: 0,
})
const QUEUE_ADMISSION = Object.freeze({
  rpm: 0,
  maxConcurrentRequests: 1,
  maxQueuedRequests: 1,
  queueTimeoutMs: 350,
})
const CHAOS_ADMISSION = Object.freeze({
  rpm: 0,
  maxConcurrentRequests: 1,
  maxQueuedRequests: 0,
  queueTimeoutMs: 0,
})
const CHAOS_CELLS = Object.freeze([
  { name: 'redis_0ms', toxic: null },
  { name: 'redis_75ms', toxic: { type: 'latency', attributes: { latency: 75, jitter: 0 } } },
  { name: 'redis_150ms', toxic: { type: 'latency', attributes: { latency: 150, jitter: 0 } } },
  { name: 'redis_reset_peer', toxic: { type: 'reset_peer', attributes: { timeout: 0 } } },
])

const ACCEPTANCE = Object.freeze({
  sameKeyRpmAcceptedPerInstance: INITIAL_ADMISSION.rpm,
  sameKeyRpmAggregateAccepted: INITIAL_ADMISSION.rpm * 2,
  perKeyRpmAcceptedPerInstance: INITIAL_ADMISSION.rpm,
  activeSameKeyAggregate: 2,
  maxAdmissionRejectedUpstreamHits: 0,
  maxChaosP95Ms: MAX_REJECTION_P95_MS,
  maxChaosP95GrowthOverBaselineMs: 60,
  maxPlateauP95Ms: MAX_REJECTION_P95_MS,
  maxPlateauFdSpreadPerService: 4,
  maxPlateauRssGrowthKbPerService: 24 * 1024,
  maxFdEndGrowthPerService: 12,
  maxRssEndGrowthKbPerService: 48 * 1024,
  maxUsageDeadlockRetriesPerRound: 0,
  maxSlowUsageWritesPerRound: 1,
})

if (!Number.isInteger(ROUNDS) || ROUNDS < 3 || ROUNDS > 5) {
  throw new Error('KIRO_REQUEST_ADMISSION_ROUNDS must be an integer between 3 and 5')
}
if (!Number.isInteger(PROBES_PER_INSTANCE) || PROBES_PER_INSTANCE < 16
  || PROBES_PER_INSTANCE > 128) {
  throw new Error('KIRO_REQUEST_ADMISSION_PROBES must be an integer between 16 and 128')
}
if (!Number.isInteger(STABILITY_WAVES) || STABILITY_WAVES < 3 || STABILITY_WAVES > 10) {
  throw new Error('KIRO_REQUEST_ADMISSION_STABILITY_WAVES must be an integer between 3 and 10')
}
if (!Number.isFinite(MAX_REJECTION_P95_MS) || MAX_REJECTION_P95_MS < 25
  || MAX_REJECTION_P95_MS > 2_000) {
  throw new Error('KIRO_REQUEST_ADMISSION_MAX_P95_MS must be between 25 and 2000')
}

function requiredEnvironment(name) {
  const value = String(process.env[name] || '').trim()
  if (!value) throw new Error(`${name} is required`)
  return value
}

const SAFE_ENV_NAMES = [
  'PATH',
  'TMPDIR',
  'TMP',
  'TEMP',
  'LANG',
  'LC_ALL',
  'LC_CTYPE',
  'TZ',
  'USER',
  'LOGNAME',
]

function minimalEnvironment(extra = {}) {
  const environment = {}
  for (const name of SAFE_ENV_NAMES) {
    if (typeof process.env[name] === 'string' && process.env[name] !== '') {
      environment[name] = process.env[name]
    }
  }
  return { ...environment, ...extra }
}

function validateInputs() {
  const placeholderCount = (POSTGRES_URL_TEMPLATE.match(/\{database\}/g) || []).length
  if (placeholderCount !== 1) {
    throw new Error('KIRO_REQUEST_ADMISSION_POSTGRES_URL_TEMPLATE must contain exactly one literal {database} placeholder')
  }
  const sampleDatabase = POSTGRES_DATABASES[0] || 'kiro_request_admission_contract_sample'
  const postgres = new URL(POSTGRES_URL_TEMPLATE.replace('{database}', sampleDatabase))
  if (!['postgres:', 'postgresql:'].includes(postgres.protocol)) {
    throw new Error('KIRO_REQUEST_ADMISSION_POSTGRES_URL_TEMPLATE must use PostgreSQL')
  }
  if (!['127.0.0.1', 'localhost', '::1'].includes(postgres.hostname)) {
    throw new Error('KIRO_REQUEST_ADMISSION_POSTGRES_URL_TEMPLATE must target loopback')
  }
  if (Number(postgres.port || 5432) === 9022) throw new Error('port 9022 is protected')
  if (postgres.hash) throw new Error('KIRO_REQUEST_ADMISSION_POSTGRES_URL_TEMPLATE must not contain a fragment')
  if (POSTGRES_DATABASES.length !== REQUIRED_DATABASE_COUNT) {
    throw new Error(`KIRO_REQUEST_ADMISSION_POSTGRES_DATABASES must contain exactly ${REQUIRED_DATABASE_COUNT} pre-created database names`)
  }
  for (const database of POSTGRES_DATABASES) {
    if (!/^kiro_request_admission_[a-z0-9_]{3,80}$/.test(database)) {
      throw new Error('KIRO_REQUEST_ADMISSION_POSTGRES_DATABASES must contain caller-owned kiro_request_admission_* names')
    }
  }

  const redis = new URL(REDIS_URL)
  if (redis.protocol !== 'redis:') throw new Error('KIRO_REQUEST_ADMISSION_REDIS_URL must use redis://')
  if (redis.username || redis.password) {
    throw new Error('KIRO_REQUEST_ADMISSION_REDIS_URL must not contain Redis auth material')
  }
  if (!['127.0.0.1', 'localhost', '::1'].includes(redis.hostname)) {
    throw new Error('KIRO_REQUEST_ADMISSION_REDIS_URL must target loopback')
  }
  if (Number(redis.port || 6379) === 9022) throw new Error('port 9022 is protected')
  if (redis.search || redis.hash) throw new Error('KIRO_REQUEST_ADMISSION_REDIS_URL must not contain query or fragment data')
  const dbText = redis.pathname.replace(/^\//, '')
  if (!/^\d+$/.test(dbText)) {
    throw new Error('KIRO_REQUEST_ADMISSION_REDIS_URL must name a Redis database')
  }
  const redisDatabase = Number(dbText)
  if (!Number.isSafeInteger(redisDatabase) || redisDatabase < 1 || redisDatabase > 15) {
    throw new Error('KIRO_REQUEST_ADMISSION_REDIS_URL must use an isolated nonzero database in 1..15')
  }
  if (REDIS_PREFIX.includes('kiro_rs:local')) {
    throw new Error('KIRO_REQUEST_ADMISSION_REDIS_PREFIX must be a caller-owned temporary prefix')
  }
  if (!/^[a-z0-9][a-z0-9:._-]{7,95}$/.test(REDIS_PREFIX)) {
    throw new Error('KIRO_REQUEST_ADMISSION_REDIS_PREFIX has an invalid format')
  }
  redisTarget = {
    redis,
    redisPort: Number(redis.port || 6379),
    redisDatabase,
  }
  return {
    postgresHost: postgres.hostname,
    postgresPort: Number(postgres.port || 5432),
    postgresDatabaseCount: POSTGRES_DATABASES.length,
    redisHost: redis.hostname,
    redisPort: redisTarget.redisPort,
    redisDatabase,
  }
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
    maxBuffer: 32 * 1024 * 1024,
    env: minimalEnvironment(),
    ...options,
  })
  if (result.status !== 0) {
    const stderr = String(result.stderr || '').trim().slice(0, 4000)
    throw new Error(`${command} ${args.join(' ')} failed (${result.status}): ${stderr}`)
  }
  return String(result.stdout || '').trim()
}

function redact(value) {
  let output = String(value || '')
  for (const key of Object.values(REQUEST_KEYS)) output = output.replaceAll(key, '<request-key>')
  for (const secret of [POSTGRES_URL_TEMPLATE, REDIS_URL, REDIS_PREFIX, BINARY, ARTIFACT_ROOT]) {
    if (secret) output = output.replaceAll(secret, '<redacted>')
  }
  return output
    .replaceAll(ADMIN_KEY, '<admin-key>')
    .replaceAll(LOCAL_TOKEN, '<local-token>')
    .replace(/\u001b\[[0-9;]*m/g, '')
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

async function isPortOpen(port) {
  return new Promise((resolve) => {
    const socket = net.connect({ host: '127.0.0.1', port })
    socket.setTimeout(250)
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
}

async function waitForTcp(port, timeoutMs = 30_000) {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    if (await isPortOpen(port)) return
    await sleep(100)
  }
  throw new Error(`timeout waiting for 127.0.0.1:${port}`)
}

async function waitFor(predicate, description, timeoutMs = 10_000, intervalMs = 25) {
  const deadline = Date.now() + timeoutMs
  let lastError = null
  while (Date.now() < deadline) {
    try {
      const value = await predicate()
      if (value) return value
    } catch (error) {
      lastError = error
    }
    await sleep(intervalMs)
  }
  const suffix = lastError ? `: ${lastError.message}` : ''
  throw new Error(`timeout waiting for ${description}${suffix}`)
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

function processResources(pid) {
  const ps = spawnSync('ps', ['-o', 'rss=', '-p', String(pid)], { encoding: 'utf8' })
  const rssKb = Number.parseInt(String(ps.stdout || '').trim(), 10) || 0
  const lsof = spawnSync('lsof', ['-nP', '-Ff', '-p', String(pid)], { encoding: 'utf8' })
  const fdCount = new Set(
    String(lsof.stdout || '').split('\n')
      .filter((line) => /^f\d+[a-z]*$/.test(line))
      .map((line) => line.match(/^f(\d+)/)?.[1])
      .filter(Boolean),
  ).size
  return { rssKb, fdCount }
}

function quantiles(values) {
  assert.ok(values.length > 0)
  const sorted = [...values].sort((a, b) => a - b)
  const pick = (fraction) => sorted[Math.min(sorted.length - 1, Math.ceil(sorted.length * fraction) - 1)]
  return {
    count: sorted.length,
    min: Number(sorted[0].toFixed(2)),
    p50: Number(pick(0.5).toFixed(2)),
    p95: Number(pick(0.95).toFixed(2)),
    p99: Number(pick(0.99).toFixed(2)),
    max: Number(sorted.at(-1).toFixed(2)),
  }
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

async function readBody(request) {
  const chunks = []
  for await (const chunk of request) chunks.push(chunk)
  return Buffer.concat(chunks).toString('utf8')
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

function extractMarker(raw) {
  return raw.match(/RA-[A-Za-z0-9_-]+/)?.[0] || 'RA-unknown'
}

function createFakeUpstream() {
  const records = []
  const holds = new Map()

  function createHold(marker) {
    assert.ok(!holds.has(marker), `duplicate hold ${marker}`)
    let release
    const gate = new Promise((resolve) => { release = resolve })
    holds.set(marker, { gate, release })
  }

  function release(marker) {
    const hold = holds.get(marker)
    if (!hold) return
    holds.delete(marker)
    hold.release()
  }

  const server = http.createServer(async (request, response) => {
    const raw = await readBody(request)
    const marker = extractMarker(raw)
    const target = String(request.headers['x-amz-target'] || '')
    const kind = target.endsWith('.ListAvailableModels')
      || String(request.url || '').toLowerCase().includes('listavailablemodels')
      ? 'auxiliary'
      : 'inference'
    const record = { marker, kind, startedAt: Date.now(), completedAt: null }
    records.push(record)

    if (kind === 'auxiliary') {
      writeJson(response, 200, {
        models: [{
          modelId: 'claude-sonnet-4',
          modelName: 'Request Admission Isolated Sonnet',
          supportedInputTypes: ['TEXT'],
        }],
      })
      record.completedAt = Date.now()
      return
    }

    const hold = holds.get(marker)
    if (hold) {
      response.writeHead(200, {
        'content-type': 'application/vnd.amazon.eventstream',
        connection: 'close',
      })
      response.write(eventFrame('assistantResponseEvent', {
        content: `holder-open ${marker}`,
        messageStatus: 'IN_PROGRESS',
      }))
      await hold.gate
      response.write(eventFrame('assistantResponseEvent', {
        content: `holder-complete ${marker}`,
        messageStatus: 'COMPLETED',
      }))
      response.end(eventFrame('metadataEvent', {
        tokenUsage: {
          uncachedInputTokens: 8,
          cacheReadInputTokens: 0,
          cacheWriteInputTokens: 0,
          outputTokens: 4,
          totalTokens: 12,
        },
      }))
      record.completedAt = Date.now()
      return
    }

    const body = Buffer.concat([
      eventFrame('assistantResponseEvent', {
        content: `ok ${marker}`,
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
    record.completedAt = Date.now()
  })

  return {
    records,
    createHold,
    release,
    releaseAll() {
      for (const marker of [...holds.keys()]) release(marker)
    },
    inferenceCount(prefix = '') {
      return records.filter((record) => record.kind === 'inference'
        && record.marker.startsWith(prefix)).length
    },
    hasInference(marker) {
      return records.some((record) => record.kind === 'inference' && record.marker === marker)
    },
    async listen(port) {
      await new Promise((resolve, reject) => {
        server.once('error', reject)
        server.listen(port, '127.0.0.1', resolve)
      })
    },
    async close() {
      this.releaseAll()
      await new Promise((resolve) => server.close(resolve))
    },
  }
}

function startService({ configPath, credentialsPath, logPath, port }) {
  const log = fs.openSync(logPath, 'a')
  const handle = spawn(BINARY, ['--config', configPath, '--credentials', credentialsPath], {
    cwd: ROOT,
    env: minimalEnvironment({
      KIRO_API_KEY: '',
      KIRO_RS_HOST: '127.0.0.1',
      KIRO_RS_PORT: String(port),
      RUST_LOG: 'kiro_rs::anthropic::request_admission=debug,kiro_rs=info',
    }),
    stdio: ['ignore', log, log],
  })
  handle.once('exit', () => fs.closeSync(log))
  return handle
}

async function stopService(handle) {
  if (!handle || handle.exitCode !== null) return
  handle.kill('SIGTERM')
  const exited = await Promise.race([
    new Promise((resolve) => handle.once('exit', () => resolve(true))),
    sleep(10_000).then(() => false),
  ])
  if (!exited && handle.exitCode === null) {
    handle.kill('SIGKILL')
    await new Promise((resolve) => handle.once('exit', resolve))
  }
}

async function waitForHealth(baseUrl, handle, timeoutMs = 60_000) {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    if (handle.exitCode !== null) {
      throw new Error(`kiro-rs exited before health check: ${handle.exitCode}`)
    }
    try {
      const response = await fetch(`${baseUrl}/healthz`)
      if (response.ok) return
    } catch {}
    await sleep(100)
  }
  throw new Error(`timeout waiting for ${baseUrl}/healthz`)
}

function adminHeaders() {
  return {
    authorization: `Bearer ${ADMIN_KEY}`,
    'content-type': 'application/json',
    connection: 'close',
  }
}

function requestHeaders(key) {
  return { 'x-api-key': key, 'content-type': 'application/json', connection: 'close' }
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
    retryAfter: response.headers.get('retry-after'),
    requestId: response.headers.get('request-id') || response.headers.get('x-request-id'),
    headerMs: Number((headersAt - started).toFixed(2)),
    totalMs: Number((ended - started).toFixed(2)),
  }
}

function deterministicSessionId(seed) {
  const value = sha256(seed)
  return `${value.slice(0, 8)}-${value.slice(8, 12)}-4${value.slice(13, 16)}-8${value.slice(17, 20)}-${value.slice(20, 32)}`
}

function messageBody(marker, stream = false) {
  return JSON.stringify({
    model: 'claude-sonnet-4',
    max_tokens: 64,
    stream,
    metadata: {
      user_id: JSON.stringify({ session_id: deterministicSessionId(marker) }),
    },
    messages: [{ role: 'user', content: marker }],
  })
}

function sendMessage(service, key, marker, options = {}) {
  return timedRequest(`${service.baseUrl}/v1/messages`, {
    method: 'POST',
    headers: requestHeaders(key),
    body: messageBody(marker, false),
    signal: options.signal,
  })
}

async function openHolder(service, key, marker, fake) {
  fake.createHold(marker)
  const started = performance.now()
  const response = await fetch(`${service.baseUrl}/v1/messages`, {
    method: 'POST',
    headers: requestHeaders(key),
    body: messageBody(marker, true),
  })
  if (response.status !== 200) {
    const text = await response.text()
    assert.equal(response.status, 200, text)
  }
  assert.ok(response.body, `${marker}: missing response body`)
  const reader = response.body.getReader()
  const first = await Promise.race([
    reader.read(),
    sleep(10_000).then(() => { throw new Error(`${marker}: no first stream chunk`) }),
  ])
  assert.equal(first.done, false, `${marker}: stream ended before hold`)
  assert.equal(fake.hasInference(marker), true, `${marker}: holder did not reach fake upstream`)
  return {
    marker,
    headerAndFirstChunkMs: Number((performance.now() - started).toFixed(2)),
    async complete() {
      fake.release(marker)
      for (;;) {
        const next = await reader.read()
        if (next.done) break
      }
    },
  }
}

function credentialFixture() {
  return [{
    id: 1,
    accessToken: LOCAL_TOKEN,
    machineId: deterministicSessionId('request-admission-machine'),
    expiresAt: '2099-01-01T00:00:00Z',
    authMethod: 'social',
    endpoint: 'ide',
    profileArn: 'arn:aws:codewhisperer:us-east-1:123456789012:profile/REQUEST_ADMISSION',
    maxConcurrentRequests: 64,
    rpm: 0,
    supportedModels: ['claude-sonnet-4'],
    disabled: false,
  }]
}

function serviceConfig({ databaseUrl, redisUrl, redisKeyPrefix, servicePort, upstreamPort }) {
  return {
    postgres: { url: databaseUrl, maxConnections: 6, migrateOnStart: true },
    redis: { url: redisUrl, keyPrefix: redisKeyPrefix },
    host: '127.0.0.1',
    port: servicePort,
    apiKey: REQUEST_KEYS.a,
    apiKeys: [REQUEST_KEYS.b, REQUEST_KEYS.c],
    adminApiKey: ADMIN_KEY,
    requestAdmission: INITIAL_ADMISSION,
    defaultEndpoint: 'ide',
    kiroUpstreamBaseUrl: `http://127.0.0.1:${upstreamPort}/kiro`,
    kiroUpstreamResponseTimeoutSecs: 120,
    kiroUpstreamStreamIdleTimeoutSecs: 30,
    kiroUpstreamStreamRetryEnabled: false,
    credentialRetryMaxAttempts: 1,
    inferenceUpstreamMaxAttempts: 1,
    credentialWarmupRequests: 0,
    credentialRpm: 0,
    credentialMaxConcurrentRequests: 64,
    credentialDispatchMaxWaitSecs: 3,
    dispatchGlobalMaxConcurrentRequests: 128,
    dispatchMaxQueuedRequests: 128,
    loadBalancingMode: 'balanced',
    schedulerTopK: 1,
    externalPools: {
      externalPoolsEnabled: false,
      fallbackOnLocalCapacityExhausted: false,
      fallbackOnSchedulerRedisDegraded: false,
      fallbackOnNoAvailableCredentials: false,
      fallbackOnLocalTransientExhausted: false,
      fallbackOnUnsupportedModel: false,
    },
  }
}

async function startCaseService({
  name,
  caseRoot,
  databaseUrl,
  redisProxyPort,
  redisDatabase,
  redisKeyPrefix,
  upstreamPort,
  servicePorts,
}) {
  const port = await reservePort()
  assert.notEqual(port, 9022)
  servicePorts.push(port)
  const root = path.join(caseRoot, name)
  fs.mkdirSync(root, { recursive: true, mode: 0o700 })
  const configPath = path.join(root, 'config.json')
  const credentialsPath = path.join(root, 'credentials.json')
  const logPath = path.join(root, 'service.log')
  fs.writeFileSync(configPath, `${JSON.stringify(serviceConfig({
    databaseUrl,
    redisUrl: `redis://127.0.0.1:${redisProxyPort}/${redisDatabase}`,
    redisKeyPrefix,
    servicePort: port,
    upstreamPort,
  }), null, 2)}\n`, { mode: 0o600 })
  fs.writeFileSync(credentialsPath, `${JSON.stringify(credentialFixture(), null, 2)}\n`, {
    mode: 0o600,
  })
  const handle = startService({ configPath, credentialsPath, logPath, port })
  const service = {
    name,
    port,
    baseUrl: `http://127.0.0.1:${port}`,
    handle,
    logPath,
  }
  try {
    await waitForHealth(service.baseUrl, handle)
    service.resourcesStart = processResources(handle.pid)
    return service
  } catch (error) {
    await stopService(handle)
    const tail = fs.existsSync(logPath) ? redact(fs.readFileSync(logPath, 'utf8').slice(-30_000)) : ''
    throw new Error(`${name} startup failed: ${error.message}\n${tail}`)
  }
}

async function getRuntime(service) {
  const response = await timedRequest(`${service.baseUrl}/api/admin/config/runtime`, {
    headers: adminHeaders(),
  })
  assert.equal(response.status, 200, response.text)
  return JSON.parse(response.text)
}

async function putAdmission(service, admission) {
  const runtime = await getRuntime(service)
  const response = await timedRequest(`${service.baseUrl}/api/admin/config/runtime`, {
    method: 'PUT',
    headers: adminHeaders(),
    body: JSON.stringify({ ...runtime, requestAdmission: admission }),
  })
  assert.equal(response.status, 200, response.text)
  assert.deepEqual(JSON.parse(response.text).requestAdmission, admission)
  return response
}

async function waitAdmission(service, admission, timeoutMs = 10_000) {
  return waitFor(async () => {
    const runtime = await getRuntime(service)
    return JSON.stringify(runtime.requestAdmission) === JSON.stringify(admission) ? runtime : null
  }, `${service.name} admission ${JSON.stringify(admission)}`, timeoutMs, 50)
}

async function runRpmBoundary(service, key, prefix) {
  const accepted = []
  for (let index = 1; index <= INITIAL_ADMISSION.rpm; index += 1) {
    const response = await sendMessage(service, key, `${prefix}-ACCEPT-${index}`)
    assert.equal(response.status, 200, JSON.stringify(response))
    accepted.push(response)
  }
  const rejected = await sendMessage(service, key, `${prefix}-REJECT`)
  assert.equal(rejected.status, 429, JSON.stringify(rejected))
  assert.ok(rejected.requestId, JSON.stringify(rejected))
  return {
    accepted: accepted.length,
    rejected: 1,
    acceptedLatency: quantiles(accepted.map((item) => item.totalMs)),
    rejectedLatency: rejected.totalMs,
    retryAfter: rejected.retryAfter,
  }
}

async function pendingRequest(service, key, marker) {
  const controller = new AbortController()
  const promise = sendMessage(service, key, marker, { signal: controller.signal })
    .then((response) => ({ kind: 'response', response }))
    .catch((error) => ({ kind: 'error', name: error.name, message: error.message }))
  return { controller, promise }
}

async function queueWorkload(services, fake, prefix) {
  const queuedMarkers = services.map((service) => `${prefix}-${service.name}-QUEUE-CANCEL`)
  const queued = await Promise.all(services.map((service, index) => (
    pendingRequest(service, REQUEST_KEYS.a, queuedMarkers[index])
  )))
  await sleep(100)
  for (const marker of queuedMarkers) assert.equal(fake.hasInference(marker), false, marker)

  const upstreamBeforeFull = fake.inferenceCount(prefix)
  const full = await Promise.all(services.map((service) => (
    sendMessage(service, REQUEST_KEYS.a, `${prefix}-${service.name}-QUEUE-FULL`)
  )))
  for (const response of full) assert.equal(response.status, 429, JSON.stringify(response))
  assert.equal(fake.inferenceCount(prefix), upstreamBeforeFull)

  for (const request of queued) request.controller.abort()
  const cancelled = await Promise.all(queued.map((request) => request.promise))
  for (const result of cancelled) {
    assert.equal(result.kind, 'error', JSON.stringify(result))
    assert.equal(result.name, 'AbortError', JSON.stringify(result))
  }
  await sleep(75)

  const upstreamBeforeTimeout = fake.inferenceCount(prefix)
  const timeout = await Promise.all(services.map((service) => (
    sendMessage(service, REQUEST_KEYS.a, `${prefix}-${service.name}-QUEUE-TIMEOUT`)
  )))
  for (const response of timeout) {
    assert.equal(response.status, 429, JSON.stringify(response))
    assert.ok(response.totalMs >= QUEUE_ADMISSION.queueTimeoutMs - 50, JSON.stringify(response))
  }
  assert.equal(fake.inferenceCount(prefix), upstreamBeforeTimeout)

  return {
    queueFull: full.map((item) => ({ status: item.status, totalMs: item.totalMs })),
    cancelled,
    queueTimeout: timeout.map((item) => ({ status: item.status, totalMs: item.totalMs })),
  }
}

async function installToxic(toxiproxyApiPort, cell) {
  await removeToxic(toxiproxyApiPort)
  if (!cell.toxic) return
  if (cell.toxic.type === 'reset_peer') {
    await setProxyEnabled(toxiproxyApiPort, false)
    return
  }
  const response = await timedRequest(
    `http://127.0.0.1:${toxiproxyApiPort}/proxies/redis/toxics`,
    {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        name: 'request-admission-chaos',
        type: cell.toxic.type,
        stream: 'downstream',
        toxicity: 1,
        attributes: cell.toxic.attributes,
      }),
    },
  )
  assert.ok([200, 201].includes(response.status), response.text)
}

async function removeToxic(toxiproxyApiPort) {
  const response = await fetch(
    `http://127.0.0.1:${toxiproxyApiPort}/proxies/redis/toxics/request-admission-chaos`,
    { method: 'DELETE' },
  ).catch(() => null)
  if (response) assert.ok([204, 404].includes(response.status), await response.text())
  await setProxyEnabled(toxiproxyApiPort, true)
}

async function setProxyEnabled(toxiproxyApiPort, enabled) {
  const response = await timedRequest(
    `http://127.0.0.1:${toxiproxyApiPort}/proxies/redis`,
    {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        name: 'redis',
        enabled,
      }),
    },
  )
  assert.equal(response.status, 200, response.text)
}

function databaseUrlForRound(round) {
  const database = POSTGRES_DATABASES[round - 1]
  return {
    database,
    url: POSTGRES_URL_TEMPLATE.replace('{database}', encodeURIComponent(database)),
  }
}

function startChild(command, args, options = {}) {
  const child = spawn(command, args, {
    cwd: ROOT,
    stdio: options.stdio || ['ignore', 'pipe', 'pipe'],
    env: minimalEnvironment(options.env || {}),
  })
  return child
}

async function stopChild(handle, label) {
  if (!handle || handle.exitCode !== null) return
  handle.kill('SIGTERM')
  const exited = await Promise.race([
    new Promise((resolve) => handle.once('exit', () => resolve(true))),
    sleep(8_000).then(() => false),
  ])
  if (!exited && handle.exitCode === null) {
    handle.kill('SIGKILL')
    await new Promise((resolve) => handle.once('exit', resolve))
  }
  if (handle.exitCode === null) throw new Error(`failed to stop ${label}`)
}

async function startRedisProxy() {
  assert(redisTarget)
  const script = path.join(ROOT, 'feature/tests/redis-chaos-proxy.mjs')
  const child = startChild(process.execPath, [
    script,
    '--listen-host', '127.0.0.1',
    '--listen-port', '0',
    '--api-host', '127.0.0.1',
    '--api-port', '0',
    '--upstream-host', redisTarget.redis.hostname,
    '--upstream-port', String(redisTarget.redisPort),
    '--database', String(redisTarget.redisDatabase),
    '--name', 'redis',
  ])
  redisProxyProcess = child
  let stderr = ''
  child.stderr.on('data', (chunk) => { stderr += chunk.toString('utf8') })
  const ready = await new Promise((resolve, reject) => {
    let stdout = ''
    const timer = setTimeout(() => reject(new Error(`redis proxy did not become ready: ${stderr}`)), 10_000)
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
      reject(new Error(`redis proxy exited before ready code=${code}: ${stderr}`))
    })
  })
  assert.equal(ready.protected9022ProbeSkipped, true)
  return ready
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

async function scanOwnedRedisKeys(limit = 20_000) {
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

async function chaosWorkload(services, fake, toxiproxyApiPort, prefix) {
  const cells = []
  for (const cell of CHAOS_CELLS) {
    await installToxic(toxiproxyApiPort, cell)
    await sleep(100)
    const upstreamBefore = fake.inferenceCount(prefix)
    const started = performance.now()
    const responses = await Promise.all(services.flatMap((service) => (
      Array.from({ length: PROBES_PER_INSTANCE }, (_, index) => (
        sendMessage(
          service,
          REQUEST_KEYS.a,
          `${prefix}-${cell.name}-${service.name}-${index + 1}`,
        )
      ))
    )))
    const wallMs = Number((performance.now() - started).toFixed(2))
    for (const response of responses) {
      assert.equal(response.status, 429, JSON.stringify(response))
      assert.ok(response.requestId, JSON.stringify(response))
    }
    const upstreamDelta = fake.inferenceCount(prefix) - upstreamBefore
    assert.equal(upstreamDelta, ACCEPTANCE.maxAdmissionRejectedUpstreamHits)
    cells.push({
      name: cell.name,
      requests: responses.length,
      status429: responses.filter((item) => item.status === 429).length,
      upstreamDelta,
      wallMs,
      latency: quantiles(responses.map((item) => item.totalMs)),
      resourcesAfter: Object.fromEntries(
        services.map((service) => [service.name, processResources(service.handle.pid)]),
      ),
    })
  }
  await removeToxic(toxiproxyApiPort)
  const baseline = cells[0].latency.p95
  for (const cell of cells) {
    assert.ok(cell.latency.p95 <= ACCEPTANCE.maxChaosP95Ms, JSON.stringify(cell))
    assert.ok(
      cell.latency.p95 <= baseline + ACCEPTANCE.maxChaosP95GrowthOverBaselineMs,
      JSON.stringify({ baseline, cell }),
    )
  }
  return cells
}

async function stabilityWorkload(services, fake, prefix) {
  const waves = []
  for (let wave = 1; wave <= STABILITY_WAVES; wave += 1) {
    const upstreamBefore = fake.inferenceCount(prefix)
    const started = performance.now()
    const responses = await Promise.all(services.flatMap((service) => (
      Array.from({ length: PROBES_PER_INSTANCE }, (_, index) => (
        sendMessage(
          service,
          REQUEST_KEYS.a,
          `${prefix}-PLATEAU-${wave}-${service.name}-${index + 1}`,
        )
      ))
    )))
    const wallMs = Number((performance.now() - started).toFixed(2))
    for (const response of responses) assert.equal(response.status, 429, JSON.stringify(response))
    const upstreamDelta = fake.inferenceCount(prefix) - upstreamBefore
    assert.equal(upstreamDelta, 0)
    await sleep(250)
    const resources = Object.fromEntries(
      services.map((service) => [service.name, processResources(service.handle.pid)]),
    )
    const latency = quantiles(responses.map((response) => response.totalMs))
    assert.ok(latency.p95 <= ACCEPTANCE.maxPlateauP95Ms, JSON.stringify({ wave, latency }))
    waves.push({
      wave,
      requests: responses.length,
      status429: responses.length,
      upstreamDelta,
      wallMs,
      latency,
      resources,
    })
  }
  await sleep(2_000)
  const idleResources = Object.fromEntries(
    services.map((service) => [service.name, processResources(service.handle.pid)]),
  )
  const plateau = {}
  for (const service of services) {
    const rss = waves.map((wave) => wave.resources[service.name].rssKb)
    const fds = waves.map((wave) => wave.resources[service.name].fdCount)
    plateau[service.name] = {
      firstRssKb: rss[0],
      lastRssKb: rss.at(-1),
      rssGrowthKb: rss.at(-1) - rss[0],
      minFd: Math.min(...fds),
      maxFd: Math.max(...fds),
      fdSpread: Math.max(...fds) - Math.min(...fds),
      idle: idleResources[service.name],
    }
    assert.ok(
      plateau[service.name].rssGrowthKb <= ACCEPTANCE.maxPlateauRssGrowthKbPerService,
      JSON.stringify({ service: service.name, plateau: plateau[service.name] }),
    )
    assert.ok(
      plateau[service.name].fdSpread <= ACCEPTANCE.maxPlateauFdSpreadPerService,
      JSON.stringify({ service: service.name, plateau: plateau[service.name] }),
    )
  }
  return { waves, idleResources, plateau }
}

async function fetchUsage(service, requestApiKeyId) {
  const response = await timedRequest(
    `${service.baseUrl}/api/admin/usage-records?limit=1000&requestApiKeyId=${requestApiKeyId}`,
    { headers: adminHeaders() },
  )
  assert.equal(response.status, 200, response.text)
  return JSON.parse(response.text)
}

async function usageEvidence(service) {
  const results = {}
  for (const [name, keyId] of Object.entries(REQUEST_KEY_IDS)) {
    const result = await waitFor(async () => {
      const current = await fetchUsage(service, keyId)
      const sampled = current.records.filter((record) => record.errorType === 'request_rejection')
      const accepted = current.records.filter((record) => record.errorType !== 'request_rejection')
      const reasons = new Set(sampled.map((record) => record.errorMetadata?.reason))
      const minimumAccepted = name === 'a' ? 14 : 9
      const expectedReasons = name === 'a'
        ? [
            'admission_rpm',
            'admission_queue_full',
            'admission_queue_timeout',
            'admission_concurrency_full',
          ]
        : ['admission_rpm']
      return accepted.length >= minimumAccepted
        && expectedReasons.every((reason) => reasons.has(reason))
        ? current
        : null
    }, `usage records for request key ${name}`, 10_000, 100)
    assert.ok(result.records.every((record) => record.requestApiKeyId === keyId))
    const sampled = result.records.filter((record) => record.errorType === 'request_rejection')
    const accepted = result.records.filter((record) => record.errorType !== 'request_rejection')
    const reasons = {}
    for (const record of sampled) {
      assert.equal(record.errorMetadata?.sampled, true, JSON.stringify(record))
      assert.equal(record.errorMetadata?.stage, 'admission', JSON.stringify(record))
      assert.equal(record.errorStatusCode, 429, JSON.stringify(record))
      const reason = record.errorMetadata?.reason || 'unknown'
      reasons[reason] = (reasons[reason] || 0) + 1
    }
    results[name] = {
      digest: keyId,
      total: result.total,
      returned: result.records.length,
      accepted: accepted.length,
      sampledRejected: sampled.length,
      sampledReasons: reasons,
    }
  }
  assert.ok(results.a.accepted > 0, JSON.stringify(results))
  assert.ok(results.a.sampledRejected > 0, JSON.stringify(results))
  assert.ok(results.b.accepted > 0, JSON.stringify(results))
  assert.ok(results.c.accepted > 0, JSON.stringify(results))
  return results
}

async function configOutageWorkload({ services, toxiproxyApiPort }) {
  const before = await Promise.all(services.map(getRuntime))
  assert.deepEqual(before[0].requestAdmission, CHAOS_ADMISSION)
  assert.deepEqual(before[1].requestAdmission, CHAOS_ADMISSION)

  await setProxyEnabled(toxiproxyApiPort, false)
  await sleep(250)
  const outageAdmission = {
    rpm: 0,
    maxConcurrentRequests: 7,
    maxQueuedRequests: 0,
    queueTimeoutMs: 0,
  }
  const update = await putAdmission(services[0], outageAdmission)
  const primaryDuringOutage = await getRuntime(services[0])
  assert.deepEqual(primaryDuringOutage.requestAdmission, outageAdmission)
  await sleep(500)
  const secondaryDuringOutage = await getRuntime(services[1])
  const convergedDuringOutage = JSON.stringify(secondaryDuringOutage.requestAdmission)
    === JSON.stringify(outageAdmission)
  assert.equal(convergedDuringOutage, false, JSON.stringify(secondaryDuringOutage.requestAdmission))

  await setProxyEnabled(toxiproxyApiPort, true)
  const reconnectStarted = performance.now()
  await waitAdmission(services[1], outageAdmission, 12_000)
  const reconnectConvergenceMs = Number((performance.now() - reconnectStarted).toFixed(2))

  const eventAdmission = {
    rpm: 0,
    maxConcurrentRequests: 3,
    maxQueuedRequests: 0,
    queueTimeoutMs: 0,
  }
  const eventStarted = performance.now()
  await putAdmission(services[0], eventAdmission)
  await waitAdmission(services[1], eventAdmission, 10_000)
  const eventConvergenceMs = Number((performance.now() - eventStarted).toFixed(2))
  return {
    primaryPutStatus: update.status,
    primaryUpdatedDuringOutage: true,
    secondaryConvergedDuringOutage: convergedDuringOutage,
    reconnectConvergenceMs,
    eventConvergenceMs,
    finalAdmission: eventAdmission,
  }
}

function logEvidence(services) {
  const all = services.map((service) => (
    fs.existsSync(service.logPath) ? fs.readFileSync(service.logPath, 'utf8') : ''
  )).join('\n')
  const rawSecretPresent = [
    ...Object.values(REQUEST_KEYS),
    ADMIN_KEY,
    LOCAL_TOKEN,
  ].some((secret) => all.includes(secret))
  const rejectionDetails = (all.match(/authenticated inference request rejected before upstream dispatch/g) || []).length
  const suppressionSummaries = (all.match(/request API key admission rejection detail logs suppressed by global budget/g) || []).length
  const usageDeadlockRetries = (all.match(/deadlock detected/g) || []).length
  const slowUsageWrites = (all.match(/PgSQL usage 批量写入耗时较长/g) || []).length
  const schedulerRedisHotPathTimeouts = (all.match(/Redis 调度热路径/g) || []).length
  return {
    rawSecretPresent,
    rejectionDetails,
    suppressionSummaries,
    usageDeadlockRetries,
    slowUsageWrites,
    schedulerRedisHotPathTimeouts,
    redactedRelevantTail: redact(all).split('\n').filter((line) => (
      line.includes('request API key admission')
      || line.includes('authenticated inference request rejected')
      || line.includes('Redis 运行时事件')
      || line.includes('deadlock detected')
      || line.includes('PgSQL usage 批量写入耗时较长')
    )).slice(-80),
  }
}

async function runRound({
  round,
  redisProxyPort,
  toxiproxyApiPort,
  upstreamPort,
  fake,
  servicePorts,
}) {
  const prefix = `RA-R${round}`
  const caseRoot = path.join(TEMP_ROOT, `round-${round}`)
  fs.mkdirSync(caseRoot, { recursive: true, mode: 0o700 })
  const { database, url: databaseUrl } = databaseUrlForRound(round)
  const redisKeyPrefix = `${REDIS_PREFIX}:round:${round}`
  await cleanupOwnedRedisKeys()
  const services = []
  let holders = []
  try {
    services.push(await startCaseService({
      name: 'primary',
      caseRoot,
      databaseUrl,
      redisProxyPort,
      redisDatabase: redisTarget.redisDatabase,
      redisKeyPrefix,
      upstreamPort,
      servicePorts,
    }))
    services.push(await startCaseService({
      name: 'secondary',
      caseRoot,
      databaseUrl,
      redisProxyPort,
      redisDatabase: redisTarget.redisDatabase,
      redisKeyPrefix,
      upstreamPort,
      servicePorts,
    }))
    await Promise.all(services.map((service) => waitAdmission(service, INITIAL_ADMISSION)))

    const sameKeyHitsBefore = fake.inferenceCount(prefix)
    const sameKey = await Promise.all(services.map((service) => (
      runRpmBoundary(service, REQUEST_KEYS.a, `${prefix}-SAME-${service.name}`)
    )))
    const sameKeyHits = fake.inferenceCount(prefix) - sameKeyHitsBefore
    assert.equal(sameKey.reduce((sum, item) => sum + item.accepted, 0),
      ACCEPTANCE.sameKeyRpmAggregateAccepted)
    assert.equal(sameKeyHits, ACCEPTANCE.sameKeyRpmAggregateAccepted)

    const differentKeyHitsBefore = fake.inferenceCount(prefix)
    const differentKey = await Promise.all(services.flatMap((service) => ([
      runRpmBoundary(service, REQUEST_KEYS.b, `${prefix}-KEY-B-${service.name}`),
      runRpmBoundary(service, REQUEST_KEYS.c, `${prefix}-KEY-C-${service.name}`),
    ])))
    const differentKeyHits = fake.inferenceCount(prefix) - differentKeyHitsBefore
    assert.equal(differentKeyHits, INITIAL_ADMISSION.rpm * services.length * 2)

    const configPropagationStarted = performance.now()
    await putAdmission(services[0], QUEUE_ADMISSION)
    await waitAdmission(services[1], QUEUE_ADMISSION)
    const configPropagationMs = Number((performance.now() - configPropagationStarted).toFixed(2))

    holders = await Promise.all(services.map((service) => (
      openHolder(service, REQUEST_KEYS.a, `${prefix}-${service.name}-HOLDER`, fake)
    )))
    assert.equal(holders.length, ACCEPTANCE.activeSameKeyAggregate)

    const independentKeyHitsBefore = fake.inferenceCount(prefix)
    const independentKeys = await Promise.all([
      sendMessage(services[0], REQUEST_KEYS.b, `${prefix}-PRIMARY-INDEPENDENT-B`),
      sendMessage(services[1], REQUEST_KEYS.c, `${prefix}-SECONDARY-INDEPENDENT-C`),
    ])
    for (const response of independentKeys) assert.equal(response.status, 200, JSON.stringify(response))
    assert.equal(fake.inferenceCount(prefix) - independentKeyHitsBefore, 2)

    const queue = await queueWorkload(services, fake, prefix)

    const wakeMarkers = services.map((service) => `${prefix}-${service.name}-CONFIG-WAKE`)
    const wakeRequests = await Promise.all(services.map((service, index) => (
      pendingRequest(service, REQUEST_KEYS.a, wakeMarkers[index])
    )))
    await sleep(100)
    for (const marker of wakeMarkers) assert.equal(fake.hasInference(marker), false, marker)
    const expandedAdmission = { ...QUEUE_ADMISSION, maxConcurrentRequests: 2 }
    const wakeStarted = performance.now()
    await putAdmission(services[0], expandedAdmission)
    const wakeResults = await Promise.all(wakeRequests.map((request) => request.promise))
    const wakeMs = Number((performance.now() - wakeStarted).toFixed(2))
    for (const result of wakeResults) {
      assert.equal(result.kind, 'response', JSON.stringify(result))
      assert.equal(result.response.status, 200, JSON.stringify(result))
    }
    for (const marker of wakeMarkers) assert.equal(fake.hasInference(marker), true, marker)
    await waitAdmission(services[1], expandedAdmission)

    await putAdmission(services[0], CHAOS_ADMISSION)
    await waitAdmission(services[1], CHAOS_ADMISSION)
    const resourcesBeforeChaos = Object.fromEntries(
      services.map((service) => [service.name, processResources(service.handle.pid)]),
    )
    const chaos = await chaosWorkload(services, fake, toxiproxyApiPort, prefix)
    const resourcesAfterChaos = Object.fromEntries(
      services.map((service) => [service.name, processResources(service.handle.pid)]),
    )
    const stability = await stabilityWorkload(services, fake, prefix)
    const resourcesAfterStability = Object.fromEntries(
      services.map((service) => [service.name, processResources(service.handle.pid)]),
    )

    const holderHeaderAndFirstChunkMs = holders.map((holder) => holder.headerAndFirstChunkMs)
    await Promise.all(holders.map((holder) => holder.complete()))
    holders = []
    await sleep(300)
    const recovery = await Promise.all([
      sendMessage(services[0], REQUEST_KEYS.a, `${prefix}-PRIMARY-RECOVERY`),
      sendMessage(services[1], REQUEST_KEYS.a, `${prefix}-SECONDARY-RECOVERY`),
    ])
    for (const response of recovery) assert.equal(response.status, 200, JSON.stringify(response))

    const configOutage = await configOutageWorkload({ services, toxiproxyApiPort })
    const usage = await usageEvidence(services[0])
    const logs = logEvidence(services)
    assert.equal(logs.rawSecretPresent, false, JSON.stringify(logs))
    assert.ok(
      logs.usageDeadlockRetries <= ACCEPTANCE.maxUsageDeadlockRetriesPerRound,
      JSON.stringify({ round, usageDeadlockRetries: logs.usageDeadlockRetries }),
    )
    assert.ok(
      logs.slowUsageWrites <= ACCEPTANCE.maxSlowUsageWritesPerRound,
      JSON.stringify({ round, slowUsageWrites: logs.slowUsageWrites }),
    )

    await sleep(1_000)
    const resourcesEnd = Object.fromEntries(
      services.map((service) => [service.name, processResources(service.handle.pid)]),
    )
    for (const service of services) {
      const start = resourcesBeforeChaos[service.name]
      const end = resourcesEnd[service.name]
      assert.ok(end.fdCount <= start.fdCount + ACCEPTANCE.maxFdEndGrowthPerService,
        JSON.stringify({ service: service.name, start, end }))
      assert.ok(end.rssKb <= start.rssKb + ACCEPTANCE.maxRssEndGrowthKbPerService,
        JSON.stringify({ service: service.name, start, end }))
    }

    return {
      round,
      database,
      servicePorts: services.map((service) => service.port),
      sameKey: { perInstance: sameKey, aggregateAccepted: sameKeyHits },
      differentKey: { cells: differentKey, aggregateAccepted: differentKeyHits },
      configPropagationMs,
      holderHeaderAndFirstChunkMs,
      independentKeys: independentKeys.map((item) => ({ status: item.status, totalMs: item.totalMs })),
      queue,
      configWake: { wakeMs, responses: wakeResults.map((item) => item.response.status) },
      chaos,
      stability,
      recovery: recovery.map((item) => ({ status: item.status, totalMs: item.totalMs })),
      configOutage,
      usage,
      resources: {
        start: Object.fromEntries(services.map((service) => [service.name, service.resourcesStart])),
        beforeChaos: resourcesBeforeChaos,
        afterChaos: resourcesAfterChaos,
        afterStability: resourcesAfterStability,
        end: resourcesEnd,
      },
      logs,
    }
  } catch (error) {
    const tails = services.map((service) => ({
      service: service.name,
      tail: fs.existsSync(service.logPath)
        ? redact(fs.readFileSync(service.logPath, 'utf8').slice(-30_000))
        : '<unavailable>',
    }))
    throw new Error(`${error.stack || error.message}\nlogs:\n${JSON.stringify(tails)}`)
  } finally {
    for (const holder of holders) fake.release(holder.marker)
    await removeToxic(toxiproxyApiPort).catch(() => {})
    await setProxyEnabled(toxiproxyApiPort, true).catch(() => {})
    await Promise.all(services.map((service) => stopService(service.handle)))
    await cleanupOwnedRedisKeys().catch(() => {})
  }
}

async function main() {
  const inputIdentity = validateInputs()
  if (VALIDATE_ONLY) {
    process.stdout.write(`${JSON.stringify({
      result: 'validate_only',
      dockerUsed: false,
      cargoUsed: false,
      protected9022ProbeSkipped: true,
      postgresDatabaseCount: inputIdentity.postgresDatabaseCount,
      redisDatabase: inputIdentity.redisDatabase,
      redisPrefixSha256: sha256(REDIS_PREFIX),
      createsPostgresDatabase: false,
      flushesRedisDatabase: false,
      usesDockerToxiproxy: false,
    })}\n`)
    return
  }
  fs.mkdirSync(TEMP_ROOT, { recursive: true, mode: 0o700 })
  fs.mkdirSync(REPORT_ROOT, { recursive: true })
  const upstreamPort = await reservePort()
  const servicePorts = []
  let redisProxy = null
  const fake = createFakeUpstream()
  const rounds = []
  const cleanup = {
    redisProxyStopped: false,
    ownedRedisRemoved: false,
    ownedRedisRemaining: null,
    tempSecretsRemoved: false,
    portsReleased: false,
    protectedPortProbeSkipped: true,
  }
  let runError = null

  try {
    await cleanupOwnedRedisKeys()
    redisProxy = await startRedisProxy()
    await waitForTcp(redisProxy.proxyPort)
    await fake.listen(upstreamPort)

    for (let round = 1; round <= ROUNDS; round += 1) {
      rounds.push(await runRound({
        round,
        redisProxyPort: redisProxy.proxyPort,
        toxiproxyApiPort: redisProxy.apiPort,
        upstreamPort,
        fake,
        servicePorts,
      }))
    }
  } catch (error) {
    runError = error
  } finally {
    fake.releaseAll()
    await fake.close().catch(() => {})
    if (redisProxyProcess) {
      await stopChild(redisProxyProcess, 'redis proxy').catch(() => {})
      cleanup.redisProxyStopped = redisProxyProcess.exitCode !== null
    } else {
      cleanup.redisProxyStopped = true
    }
    const redisCleanup = await cleanupOwnedRedisKeys().catch((error) => ({
      removed: 0,
      remaining: `cleanup_failed:${error.message}`,
    }))
    cleanup.ownedRedisRemoved = redisCleanup.remaining === 0
    cleanup.ownedRedisRemaining = redisCleanup.remaining
    fs.rmSync(TEMP_ROOT, { recursive: true, force: true })
    cleanup.tempSecretsRemoved = !fs.existsSync(TEMP_ROOT)
    await sleep(250)
    const allPorts = [
      redisProxy?.proxyPort,
      redisProxy?.apiPort,
      upstreamPort,
      ...servicePorts,
    ].filter((port) => Number.isInteger(port))
    cleanup.portsReleased = (await Promise.all(allPorts.map(isPortOpen))).every((open) => !open)
  }

  const gitRevision = run('git', ['rev-parse', 'HEAD'])
  const dirty = run('git', ['status', '--porcelain=v1'])
  const diff = run('git', ['diff', '--binary'])
  const report = {
    schemaVersion: 1,
    caseId: 'request-api-key-admission-multi-instance',
    runId: RUN_ID,
    generatedAt: new Date().toISOString(),
    result: runError ? 'fail' : 'pass',
    error: runError ? redact(runError.stack || runError.message) : null,
    roundsRequested: ROUNDS,
    probesPerInstancePerChaosCell: PROBES_PER_INSTANCE,
    stabilityWaves: STABILITY_WAVES,
    acceptance: ACCEPTANCE,
    requestKeyIds: REQUEST_KEY_IDS,
    gitRevision,
    dirty: Boolean(dirty),
    dirtyDiffSha256: sha256(diff),
    runnerSha256: sha256File(import.meta.filename),
    binaryPathSha256: sha256(BINARY),
    binarySha256: sha256File(BINARY),
    isolation: {
      dockerUsed: false,
      cargoUsed: false,
      usesDockerToxiproxy: false,
      createsPostgresDatabase: false,
      flushesRedisDatabase: false,
      redisOwnedPrefixCleanupOnly: true,
      servicePort9022Touched: false,
      protectedPortProbeSkipped: true,
      forbiddenPorts: [9022],
      postgresDatabaseCount: POSTGRES_DATABASES.length,
      postgresDatabases: POSTGRES_DATABASES.map((database) => sha256(database)),
      redisDatabase: inputIdentity.redisDatabase,
      redisPrefixSha256: sha256(REDIS_PREFIX),
      redisProxyPort: redisProxy?.proxyPort || null,
      redisProxyApiPort: redisProxy?.apiPort || null,
      fakeUpstreamPort: upstreamPort,
      servicePorts,
    },
    rounds,
    cleanup,
  }
  const serialized = `${JSON.stringify(report, null, 2)}\n`
  assert.equal([
    ...Object.values(REQUEST_KEYS),
    ADMIN_KEY,
    LOCAL_TOKEN,
    POSTGRES_URL_TEMPLATE,
    REDIS_URL,
    REDIS_PREFIX,
    BINARY,
    ARTIFACT_ROOT,
  ]
    .some((secret) => serialized.includes(secret)), false)
  fs.writeFileSync(REPORT_PATH, serialized)
  process.stdout.write(`${REPORT_PATH}\n`)
  if (runError) throw runError
}

main().catch((error) => {
  process.stderr.write(`request admission multi-instance validation failed: ${redact(error.stack || error.message)}\n`)
  process.exitCode = 1
})
