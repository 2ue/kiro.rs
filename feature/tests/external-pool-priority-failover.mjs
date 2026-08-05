#!/usr/bin/env node

/*
 * Isolated product-level external-pool failover validation.
 *
 * The caller owns the PostgreSQL database and Redis database. This runner
 * starts only three loopback HTTP upstreams and one frozen kiro.rs process.
 * It deliberately keeps the local credential available so external-direct
 * requests can prove that a failed external pool does not silently return to
 * the local credential path.
 */

import assert from 'node:assert/strict'
import crypto from 'node:crypto'
import fs from 'node:fs'
import http from 'node:http'
import os from 'node:os'
import path from 'node:path'
import { spawn, spawnSync } from 'node:child_process'

import { resolveRuntimeValidationPaths } from './runtime-validation-paths.mjs'
import { validationChildEnvironment } from './validation-child-env.mjs'

const ROOT = fs.realpathSync(path.resolve(import.meta.dirname, '../..'))
const { binary: BINARY, artifactRoot: ARTIFACT_ROOT } = resolveRuntimeValidationPaths(ROOT)
const POSTGRES_URL = required('KIRO_EXTERNAL_FAILOVER_POSTGRES_URL')
const REDIS_URL = required('KIRO_EXTERNAL_FAILOVER_REDIS_URL')
const REDIS_PREFIX = required('KIRO_EXTERNAL_FAILOVER_REDIS_PREFIX')
const REQUEST_KEY = 'sk-external-failover-request'
const ADMIN_KEY = 'sk-external-failover-admin'
const MODEL = 'claude-sonnet-4'
const RUN_ID = `external-failover-${Date.now()}-${process.pid}-${crypto.randomBytes(3).toString('hex')}`
const TEMP_ROOT = fs.mkdtempSync(path.join(os.tmpdir(), `${RUN_ID}-`))
const REPORT_ROOT = path.join(ARTIFACT_ROOT, 'reports', 'external-pool-priority-failover')
const REPORT_PATH = path.join(REPORT_ROOT, `${RUN_ID}.json`)
const CHILDREN = new Set()
const SERVERS = new Set()
const CLIENT_AGENT = new http.Agent({
  keepAlive: true,
  maxSockets: boundedInteger(process.env.KIRO_EXTERNAL_FAILOVER_CLIENT_SOCKETS, 64, 8, 256),
  maxFreeSockets: 32,
})
const BUSINESS_MARKER_RE = /\b(?:priority-failover-burst[12]|priority-recovery|rate-limit-failover|external-direct-no-local-rescue)-?\d*\b/
const STRESS_ENABLED = process.env.KIRO_EXTERNAL_FAILOVER_STRESS === '1'
const STRESS_BURST_CONCURRENCY = boundedInteger(
  process.env.KIRO_EXTERNAL_FAILOVER_STRESS_BURST,
  256,
  64,
  2048,
)
const STRESS_RPM = boundedInteger(process.env.KIRO_EXTERNAL_FAILOVER_STRESS_RPM, 1200, 120, 6000)
const STRESS_DURATION_SECONDS = boundedInteger(
  process.env.KIRO_EXTERNAL_FAILOVER_STRESS_SECONDS,
  900,
  60,
  3600,
)
const STRESS_MAX_IN_FLIGHT = boundedInteger(
  process.env.KIRO_EXTERNAL_FAILOVER_STRESS_IN_FLIGHT,
  1024,
  64,
  4096,
)
const STRESS_SAME_POOL_RETRY_COUNT = boundedInteger(
  process.env.KIRO_EXTERNAL_FAILOVER_STRESS_SAME_POOL_RETRY,
  1,
  0,
  3,
)
const KEEP_TEMP = process.env.KIRO_EXTERNAL_FAILOVER_KEEP_TEMP === '1'

function required(name) {
  const value = String(process.env[name] || '').trim()
  if (!value) throw new Error(`${name} is required`)
  return value
}

function boundedInteger(value, fallback, min, max) {
  const parsed = Number(value)
  if (!Number.isFinite(parsed)) return fallback
  return Math.min(max, Math.max(min, Math.floor(parsed)))
}

function sha256(value) {
  return crypto.createHash('sha256').update(value).digest('hex')
}

function processMetrics(child) {
  if (!child?.pid) return null
  const ps = spawnSync('ps', ['-o', 'rss=,vsz=,%cpu=,etime=', '-p', String(child.pid)], {
    encoding: 'utf8',
    timeout: 2_000,
  })
  if (ps.status !== 0) return null
  const fields = ps.stdout.trim().split(/\s+/)
  if (fields.length < 4) return null
  const [rssKib, vszKib, cpuPercent, elapsed] = fields
  const lsof = spawnSync('sh', ['-c', `lsof -nP -p ${Number(child.pid)} 2>/dev/null | wc -l`], {
    encoding: 'utf8',
    timeout: 3_000,
  })
  const established = spawnSync(
    'sh',
    ['-c', `lsof -nP -a -p ${Number(child.pid)} -iTCP -sTCP:ESTABLISHED 2>/dev/null | wc -l`],
    { encoding: 'utf8', timeout: 3_000 },
  )
  return {
    at: new Date().toISOString(),
    rssKib: Number(rssKib) || null,
    vszKib: Number(vszKib) || null,
    cpuPercent: Number(cpuPercent) || null,
    elapsed,
    fdCount: Number(lsof.stdout.trim()) || null,
    establishedTcpCount: Number(established.stdout.trim()) || null,
  }
}

function redact(value) {
  return String(value || '')
    .replaceAll(POSTGRES_URL, '<redacted-postgres>')
    .replaceAll(REDIS_URL, '<redacted-redis>')
    .replaceAll(REDIS_PREFIX, '<redacted-redis-prefix>')
    .replaceAll(REQUEST_KEY, '<redacted-request-key>')
    .replaceAll(ADMIN_KEY, '<redacted-admin-key>')
    .replace(/failover-local-token/g, '<redacted-local-token>')
    .slice(-30_000)
}

function validateStorage() {
  const pg = new URL(POSTGRES_URL)
  if (!['postgres:', 'postgresql:'].includes(pg.protocol)) throw new Error('PostgreSQL URL must use postgres://')
  if (!['127.0.0.1', 'localhost', '::1'].includes(pg.hostname)) throw new Error('PostgreSQL URL must target loopback')
  if (Number(pg.port || 5432) === 9022) throw new Error('PostgreSQL protected port 9022')
  const redis = new URL(REDIS_URL)
  if (redis.protocol !== 'redis:') throw new Error('Redis URL must use redis://')
  if (!['127.0.0.1', 'localhost', '::1'].includes(redis.hostname)) throw new Error('Redis URL must target loopback')
  const db = Number(redis.pathname.replace(/^\//, ''))
  if (!Number.isInteger(db) || db < 1 || db > 15) throw new Error('Redis URL must use isolated DB 1..15')
  if (Number(redis.port || 6379) === 9022) throw new Error('Redis protected port 9022')
  if (!/^[a-z0-9][a-z0-9:._-]{7,95}$/.test(REDIS_PREFIX)) throw new Error('invalid Redis prefix')
  return { pgHost: pg.hostname, pgPort: Number(pg.port || 5432), redisHost: redis.hostname, redisPort: Number(redis.port || 6379), redisDb: db }
}

function reservePort() {
  return new Promise((resolve, reject) => {
    const server = netServer()
    server.listen(0, '127.0.0.1', () => {
      const address = server.address()
      const port = typeof address === 'object' && address ? address.port : 0
      server.close((error) => error ? reject(error) : resolve(port))
    })
    server.once('error', reject)
  })
}

function netServer() {
  return http.createServer()
}

function readBody(request) {
  return new Promise((resolve, reject) => {
    const chunks = []
    request.on('data', (chunk) => chunks.push(chunk))
    request.once('end', () => resolve(Buffer.concat(chunks).toString('utf8')))
    request.once('error', reject)
  })
}

function json(response, status, body) {
  const payload = Buffer.from(JSON.stringify(body))
  response.writeHead(status, {
    'content-type': 'application/json',
    'content-length': payload.length,
    connection: 'close',
  })
  response.end(payload)
}

function createUpstream(name, initialStatus) {
  const state = {
    name,
    status: initialStatus,
    delayMs: 0,
    hits: 0,
    requests: [],
  }
  const server = http.createServer(async (request, response) => {
    const body = await readBody(request)
    state.hits += 1
    state.requests.push({
      path: request.url,
      model: (() => {
        try { return JSON.parse(body).model || null } catch { return null }
      })(),
      anthropicVersion: request.headers['anthropic-version'] || null,
      status: state.status,
    })
    if (state.delayMs > 0) {
      await new Promise((resolve) => setTimeout(resolve, state.delayMs))
    }
    if (state.status !== 200) {
      json(response, state.status, {
        type: 'error',
        error: { type: state.status === 429 ? 'rate_limit_error' : 'api_error', message: `${name} controlled failure` },
      })
      return
    }
    json(response, 200, {
      id: `msg_${name}_${state.hits}`,
      type: 'message',
      role: 'assistant',
      model: MODEL,
      content: [{ type: 'text', text: `${name}-ok` }],
      stop_reason: 'end_turn',
      stop_sequence: null,
      usage: { input_tokens: 8, output_tokens: 2 },
    })
  })
  SERVERS.add(server)
  return {
    state,
    async listen(port) {
      await new Promise((resolve, reject) => {
        server.once('error', reject)
        server.listen(port, '127.0.0.1', resolve)
      })
    },
    async close() {
      await new Promise((resolve) => server.close(resolve))
      SERVERS.delete(server)
    },
  }
}

function createLocalUpstream() {
  const state = { hits: 0, inferenceHits: 0, requests: [] }
  const server = http.createServer(async (request, response) => {
    const body = await readBody(request)
    const marker = body.match(BUSINESS_MARKER_RE)?.[0] || null
    state.hits += 1
    if (marker) state.inferenceHits += 1
    state.requests.push({
      path: request.url,
      marker,
      method: request.method,
    })
    json(response, 200, { message: 'local-should-not-be-used' })
  })
  SERVERS.add(server)
  return {
    state,
    async listen(port) {
      await new Promise((resolve, reject) => {
        server.once('error', reject)
        server.listen(port, '127.0.0.1', resolve)
      })
    },
    async close() {
      await new Promise((resolve) => server.close(resolve))
      SERVERS.delete(server)
    },
  }
}

function spawnService(configPath, credentialsPath, logPath) {
  const fd = fs.openSync(logPath, 'a')
  const child = spawn(BINARY, ['--config', configPath, '--credentials', credentialsPath], {
    cwd: TEMP_ROOT,
    env: validationChildEnvironment({
      RUST_LOG: 'kiro_rs::external_pool=debug,kiro_rs::anthropic=debug,kiro_rs=info',
      KIRO_API_KEY: '',
    }),
    stdio: ['ignore', fd, fd],
    detached: true,
  })
  CHILDREN.add(child)
  child.once('exit', () => {
    try { fs.closeSync(fd) } catch {}
    CHILDREN.delete(child)
  })
  return child
}

async function stopChild(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return
  try { process.kill(-child.pid, 'SIGTERM') } catch { child.kill('SIGTERM') }
  const deadline = Date.now() + 10_000
  while (Date.now() < deadline && child.exitCode === null && child.signalCode === null) {
    await new Promise((resolve) => setTimeout(resolve, 50))
  }
  if (child.exitCode === null && child.signalCode === null) {
    try { process.kill(-child.pid, 'SIGKILL') } catch { child.kill('SIGKILL') }
  }
  CHILDREN.delete(child)
}

async function waitForHealth(baseUrl, child) {
  const deadline = Date.now() + 60_000
  while (Date.now() < deadline) {
    if (child.exitCode !== null) throw new Error(`service exited before health: ${child.exitCode}`)
    try {
      const response = await fetch(`${baseUrl}/healthz`)
      if (response.ok) return
    } catch {}
    await new Promise((resolve) => setTimeout(resolve, 100))
  }
  throw new Error('service health timeout')
}

async function requestJson(url, options = {}) {
  const parsed = new URL(url)
  if (parsed.protocol !== 'http:') {
    try {
      const response = await fetch(url, options)
      const text = await response.text()
      return { status: response.status, text, headers: response.headers }
    } catch (error) {
      const cause = error?.cause
      const detail = cause
        ? `${cause.code || cause.message || cause.name}:${cause.message || ''}`
        : String(error?.message || error)
      return { status: 'network_error', text: detail, headers: new Headers() }
    }
  }

  return new Promise((resolve) => {
    const request = http.request(parsed, {
      method: options.method || 'GET',
      headers: options.headers || {},
      agent: CLIENT_AGENT,
    }, (response) => {
      const chunks = []
      response.on('data', (chunk) => chunks.push(chunk))
      response.once('end', () => resolve({
        status: response.statusCode || 0,
        text: Buffer.concat(chunks).toString('utf8'),
        headers: response.headers,
      }))
      response.once('error', (error) => resolve({
        status: 'network_error',
        text: String(error?.message || error),
        headers: {},
      }))
    })
    request.once('error', (error) => resolve({
      status: 'network_error',
      text: String(error?.code ? `${error.code}:${error.message}` : error?.message || error),
      headers: {},
    }))
    if (options.body !== undefined && options.body !== null) request.write(options.body)
    request.end()
  })
}

async function postMessage(baseUrl, marker) {
  return requestJson(`${baseUrl}/v1/messages`, {
    method: 'POST',
    headers: { 'x-api-key': REQUEST_KEY, 'content-type': 'application/json' },
    body: JSON.stringify({
      model: MODEL,
      max_tokens: 32,
      stream: false,
      messages: [{ role: 'user', content: marker }],
    }),
  })
}

async function postMessages(baseUrl, phase, count) {
  return Promise.all(Array.from({ length: count }, (_, index) => postMessage(baseUrl, `${phase}-${index}`)
    .then((response) => ({ phase, index, response }))))
}

async function runFixedRatePhase(baseUrl, phase, durationSeconds, rpm, maxInFlight, child) {
  const startedAt = Date.now()
  const deadline = startedAt + durationSeconds * 1000
  const intervalMs = 60_000 / rpm
  const pending = new Set()
  const statusCounts = new Map()
  const failures = []
  const resourceSamples = []
  let nextAt = startedAt
  let sent = 0
  let completed = 0
  let nextResourceSampleAt = startedAt

  const launch = () => {
    if (Date.now() >= deadline || pending.size >= maxInFlight) return false
    const index = sent
    sent += 1
    const task = (async () => {
      try {
        const response = await postMessage(baseUrl, `${phase}-${index}`)
        completed += 1
        statusCounts.set(response.status, (statusCounts.get(response.status) || 0) + 1)
        if (response.status !== 200 && failures.length < 20) {
          failures.push({ index, status: response.status, text: response.text.slice(0, 200) })
        }
      } catch (error) {
        completed += 1
        statusCounts.set('network_error', (statusCounts.get('network_error') || 0) + 1)
        if (failures.length < 20) {
          failures.push({ index, status: 'network_error', text: String(error?.message || error) })
        }
      } finally {
        pending.delete(task)
      }
    })()
    pending.add(task)
    return true
  }

  while (Date.now() < deadline) {
    const now = Date.now()
    if (now >= nextResourceSampleAt) {
      const metrics = processMetrics(child)
      if (metrics) resourceSamples.push(metrics)
      nextResourceSampleAt = now + 10_000
    }
    while (nextAt <= now && nextAt < deadline && pending.size < maxInFlight) {
      launch()
      nextAt += intervalMs
    }
    const waitMs = Math.max(1, Math.min(25, nextAt - Date.now()))
    await new Promise((resolve) => setTimeout(resolve, waitMs))
  }
  await Promise.all([...pending])
  const finalMetrics = processMetrics(child)
  if (finalMetrics) resourceSamples.push(finalMetrics)
  return {
    phase,
    durationSeconds,
    targetRpm: rpm,
    sent,
    completed,
    statusCounts: Object.fromEntries(statusCounts),
    failures,
    resourceSamples,
    elapsedMs: Date.now() - startedAt,
  }
}

async function admin(baseUrl, pathName, options = {}) {
  return requestJson(`${baseUrl}${pathName}`, {
    ...options,
    headers: {
      authorization: `Bearer ${ADMIN_KEY}`,
      'content-type': 'application/json',
      ...(options.headers || {}),
    },
  })
}

async function main() {
  const storage = validateStorage()
  fs.mkdirSync(TEMP_ROOT, { recursive: true, mode: 0o700 })
  fs.mkdirSync(REPORT_ROOT, { recursive: true, mode: 0o700 })
  const servicePort = await reservePort()
  const localPort = await reservePort()
  const ports = { yuenan: await reservePort(), kkkkyue: await reservePort(), jinnyapi: await reservePort() }
  const upstreams = {
    yuenan: createUpstream('yuenan', 500),
    kkkkyue: createUpstream('kkkkyue', 200),
    jinnyapi: createUpstream('jinnyapi', 200),
  }
  const local = createLocalUpstream()
  await Promise.all([
    upstreams.yuenan.listen(ports.yuenan),
    upstreams.kkkkyue.listen(ports.kkkkyue),
    upstreams.jinnyapi.listen(ports.jinnyapi),
    local.listen(localPort),
  ])
  const configPath = path.join(TEMP_ROOT, 'config.json')
  const credentialsPath = path.join(TEMP_ROOT, 'credentials.json')
  const logPath = path.join(TEMP_ROOT, 'service.log')
  const config = {
    postgres: { url: POSTGRES_URL, maxConnections: 8, migrateOnStart: true },
    redis: { url: REDIS_URL, keyPrefix: REDIS_PREFIX },
    host: '127.0.0.1',
    port: servicePort,
    apiKey: REQUEST_KEY,
    adminApiKey: ADMIN_KEY,
    requestAdmission: { rpm: 0, maxConcurrentRequests: 0, maxQueuedRequests: 0, queueTimeoutMs: 0 },
    defaultEndpoint: 'ide',
    kiroUpstreamBaseUrl: `http://127.0.0.1:${localPort}/kiro`,
    kiroUpstreamResponseTimeoutSecs: 5,
    credentialRetryMaxAttempts: 0,
    inferenceUpstreamMaxAttempts: 8,
    credentialWarmupRequests: 0,
    credentialMaxConcurrentRequests: 4,
      externalPools: {
        externalPoolsEnabled: true,
        externalDirectPolicyEnabled: true,
        externalPoolGlobalMaxConcurrentRequests: STRESS_ENABLED ? 4096 : 512,
      externalPoolRetryMaxAttempts: 3,
      externalPoolRetryStatusCodes: [429, 500, 502, 503, 504],
      externalPoolSamePoolRetryCount: STRESS_ENABLED ? STRESS_SAME_POOL_RETRY_COUNT : 1,
      externalPoolSamePoolRetryStatusCodes: [429, 500, 502, 503, 504],
      externalPoolSamePoolRetryDelayMs: 10,
      externalPoolServerErrorCooldownSecs: 2,
      externalPoolTransientFailurePriorityPenalty: 20,
      externalPoolCapacityMode: 'fail_fast',
      externalPoolLocalRescueEnabled: true,
      externalPoolLocalRescueOnRateLimit: true,
      externalPoolLocalRescueOnCapacity: true,
      externalPoolLocalRescueOnTimeout: true,
    },
  }
  const credentials = [{
    accessToken: 'failover-local-token',
    expiresAt: '2099-01-01T00:00:00Z',
    authMethod: 'social',
    endpoint: 'ide',
    priority: 0,
    maxConcurrentRequests: 4,
    rpm: 0,
    supportedModels: [MODEL],
    disabled: false,
  }]
  fs.writeFileSync(configPath, `${JSON.stringify(config, null, 2)}\n`, { mode: 0o600 })
  fs.writeFileSync(credentialsPath, `${JSON.stringify(credentials, null, 2)}\n`, { mode: 0o600 })

  const child = spawnService(configPath, credentialsPath, logPath)
  const baseUrl = `http://127.0.0.1:${servicePort}`
  const results = []
  try {
    await waitForHealth(baseUrl, child)
    for (const [name, priority] of [['yuenan', 1], ['kkkkyue', 10], ['jinnyapi', 20]]) {
      const created = await admin(baseUrl, '/api/admin/external-pools', {
        method: 'POST',
        body: JSON.stringify({
          name,
          baseUrl: `http://127.0.0.1:${ports[name]}/anthropic`,
          apiKey: `sk-${name}-test`,
          authType: 'bearer',
          enabled: true,
          priority,
          maxConcurrentRequests: STRESS_ENABLED ? 1024 : 128,
          usageProjectionMode: 'pass_through',
          requestBodyMode: 'normalized',
          rawModelMode: 'none',
          preservePath: true,
          supportedModels: [MODEL],
        }),
      })
      assert.equal(created.status, 200, `${name} create failed: ${created.text}`)
      process.stderr.write(`created ${name}: ${created.text}\n`)
    }
    const statusSnapshot = await admin(baseUrl, '/api/admin/external-pools/status')
    process.stderr.write(`pool status: ${statusSnapshot.status} ${statusSnapshot.text}\n`)
    await new Promise((resolve) => setTimeout(resolve, 1_500))

    // A high-priority pool failing under burst traffic must not monopolize
    // later traffic. The first burst may already have selected the bad pool
    // before its health signal is visible, but the next burst must move to
    // healthy lower-priority external pools.
    const firstBurst = await postMessages(baseUrl, 'priority-failover-burst1', 24)
    const firstBurstStatus = await admin(baseUrl, '/api/admin/external-pools/status')
    process.stderr.write(
      `after first burst pool status: ${firstBurstStatus.status} ${firstBurstStatus.text}\n`,
    )
    for (const { phase, index, response } of firstBurst) {
      process.stderr.write(`a-failing ${index}: ${response.status} ${response.text.slice(0, 120)}\n`)
      assert.equal(response.status, 200, JSON.stringify({ phase, index, status: response.status, text: response.text }))
      results.push({ phase, index, status: response.status })
    }
    assert.equal(local.state.inferenceHits, 0, 'external-direct business traffic hit local credential during first burst')
    assert.ok(
      upstreams.kkkkyue.state.hits > 0,
      `kkkkyue never took over from failed yuenan; pool status after burst: ${firstBurstStatus.text}`,
    )
    assert.ok(
      upstreams.kkkkyue.state.hits + upstreams.jinnyapi.state.hits > 0,
      'no healthy lower-priority pool took over from failed yuenan',
    )
    const yuenanAfterFirstBurst = upstreams.yuenan.state.hits
    const healthyAfterFirstBurst = upstreams.kkkkyue.state.hits + upstreams.jinnyapi.state.hits

    const secondBurst = await postMessages(baseUrl, 'priority-failover-burst2', 24)
    for (const { phase, index, response } of secondBurst) {
      assert.equal(response.status, 200, JSON.stringify({ phase, index, status: response.status, text: response.text }))
      results.push({ phase, index, status: response.status })
    }
    assert.equal(local.state.inferenceHits, 0, 'external-direct business traffic hit local credential during second burst')
    assert.ok(
      upstreams.yuenan.state.hits - yuenanAfterFirstBurst <= 4,
      `turbulent priority-1 pool reclaimed too much second-burst traffic: ${upstreams.yuenan.state.hits - yuenanAfterFirstBurst}`,
    )
    assert.ok(
      upstreams.kkkkyue.state.hits + upstreams.jinnyapi.state.hits > healthyAfterFirstBurst,
      'healthy lower-priority pools did not carry the second burst',
    )
    process.stderr.write(`phase-a hits: ${JSON.stringify(Object.fromEntries(Object.entries(upstreams).map(([name, value]) => [name, value.state.hits])))}\n`)

    // Recovery: after the short soft-failure window expires, a healthy
    // high-priority pool must receive traffic again instead of being
    // permanently quarantined.
    upstreams.yuenan.state.status = 200
    await new Promise((resolve) => setTimeout(resolve, 35_000))
    const yuenanBeforeRecovery = upstreams.yuenan.state.hits
    for (let index = 0; index < 12; index += 1) {
      const response = await postMessage(baseUrl, `priority-recovery-${index}`)
      assert.equal(response.status, 200, JSON.stringify({ index, status: response.status, text: response.text }))
      results.push({ phase: 'a-recovered', index, status: response.status })
    }
    assert.equal(local.state.inferenceHits, 0, 'external-direct business traffic hit local credential during recovery')
    assert.ok(
      upstreams.yuenan.state.hits > yuenanBeforeRecovery,
      'recovered yuenan did not receive traffic again',
    )

    // A 429 on one pool is also a failover signal; when priority-1 is
    // temporarily failing and priority-10 is rate-limited, priority-20 must
    // take over instead of falling back to local.
    upstreams.yuenan.state.status = 503
    upstreams.kkkkyue.state.status = 429
    const before429 = {
      yuenan: upstreams.yuenan.state.hits,
      kkkkyue: upstreams.kkkkyue.state.hits,
      jinnyapi: upstreams.jinnyapi.state.hits,
    }
    for (let index = 0; index < 12; index += 1) {
      const response = await postMessage(baseUrl, `rate-limit-failover-${index}`)
      assert.equal(response.status, 200, JSON.stringify({ index, status: response.status, text: response.text }))
      results.push({ phase: 'b-429', index, status: response.status })
    }
    assert.equal(local.state.inferenceHits, 0, 'external-direct business traffic hit local credential during 503+429 turbulence')
    assert.ok(upstreams.yuenan.state.hits - before429.yuenan <= 3, '503 priority-1 pool was retried excessively')
    assert.ok(upstreams.kkkkyue.state.hits - before429.kkkkyue <= 3, '429 pool was retried excessively')
    assert.ok(
      upstreams.jinnyapi.state.hits - before429.jinnyapi > 0,
      'healthy priority-20 pool did not take over from 503+429 turbulence',
    )

    let stressReport = null
    if (STRESS_ENABLED) {
      // Sustained stress deliberately uses a much higher load than the
      // baseline burst. The primary pool remains unhealthy while the two
      // backups are slowed, so accumulated queue/lease/retry defects have
      // time to surface instead of being hidden by a short burst.
      upstreams.yuenan.state.status = 503
      upstreams.yuenan.state.delayMs = 250
      upstreams.kkkkyue.state.status = 200
      upstreams.kkkkyue.state.delayMs = 250
      upstreams.jinnyapi.state.status = 200
      upstreams.jinnyapi.state.delayMs = 250
      const stressBurst = await postMessages(
        baseUrl,
        'stress-burst',
        STRESS_BURST_CONCURRENCY,
      )
      const stressBurstStatusCounts = new Map()
      const stressBurstFailures = []
      for (const { response } of stressBurst) {
        stressBurstStatusCounts.set(
          String(response.status),
          (stressBurstStatusCounts.get(String(response.status)) || 0) + 1,
        )
        if (response.status !== 200 && stressBurstFailures.length < 40) {
          stressBurstFailures.push({
            status: response.status,
            text: response.text.slice(0, 300),
          })
        }
      }
      const stressBurstStatus = await admin(baseUrl, '/api/admin/external-pools/status')
      const stressBurstAlive = await requestJson(`${baseUrl}/healthz`)
      process.stderr.write(
        `stress burst statusCounts=${JSON.stringify(Object.fromEntries(stressBurstStatusCounts))} `
        + `failures=${JSON.stringify(stressBurstFailures)} `
        + `health=${stressBurstAlive.status} pools=${stressBurstStatus.status} ${stressBurstStatus.text.slice(0, 2000)}\n`,
      )
      assert.equal(
        stressBurstFailures.length,
        0,
        `stress burst contained failed requests: ${JSON.stringify(stressBurstFailures)}`,
      )
      const stressBefore = Object.fromEntries(
        Object.entries(upstreams).map(([name, value]) => [name, value.state.hits]),
      )
      stressReport = await runFixedRatePhase(
        baseUrl,
        'stress-fixed-rate',
        STRESS_DURATION_SECONDS,
        STRESS_RPM,
        STRESS_MAX_IN_FLIGHT,
        child,
      )
      assert.equal(
        stressReport.completed,
        stressReport.sent,
        `stress phase left incomplete requests: ${JSON.stringify(stressReport)}`,
      )
      assert.ok(
        stressReport.statusCounts['200'] > 0,
        `stress phase had no successful responses: ${JSON.stringify(stressReport)}`,
      )
      assert.equal(
        local.state.inferenceHits,
        0,
        'external-direct sustained stress traffic hit local credential',
      )
      assert.ok(
        upstreams.kkkkyue.state.hits > stressBefore.kkkkyue
          || upstreams.jinnyapi.state.hits > stressBefore.jinnyapi,
        'healthy backup pools did not carry sustained stress traffic',
      )
      process.stderr.write(`stress report: ${JSON.stringify(stressReport)}\n`)

      // Stop the fault and remove the artificial delay. Recovery must happen
      // after the long window, not only after a single retry.
      upstreams.yuenan.state.status = 200
      upstreams.yuenan.state.delayMs = 0
      upstreams.kkkkyue.state.delayMs = 0
      upstreams.jinnyapi.state.delayMs = 0
      await new Promise((resolve) => setTimeout(resolve, 35_000))
      const stressRecoveryBefore = upstreams.yuenan.state.hits
      const stressRecovery = await postMessages(baseUrl, 'stress-recovery', 64)
      for (const { response } of stressRecovery) {
        assert.equal(response.status, 200, `stress recovery returned ${response.status}: ${response.text.slice(0, 200)}`)
      }
      assert.ok(
        upstreams.yuenan.state.hits > stressRecoveryBefore,
        'primary pool did not receive traffic after sustained stress recovery',
      )
    }

    // Explicit external-direct must never rescue to the local credential,
    // even when every external pool returns a server error.
    upstreams.yuenan.state.status = 503
    upstreams.kkkkyue.state.status = 503
    upstreams.jinnyapi.state.status = 503
    const localBeforeInference = local.state.inferenceHits
    const directFailure = await postMessage(baseUrl, 'external-direct-no-local-rescue')
    assert.notEqual(directFailure.status, 200, `all external pools unexpectedly succeeded: ${directFailure.text}`)
    assert.equal(
      local.state.inferenceHits,
      localBeforeInference,
      'external-direct failure silently fell back to local credential',
    )
    assert.equal(local.state.inferenceHits, 0, 'external-direct business traffic hit local credential')
    results.push({ phase: 'c-direct-no-local-rescue', status: directFailure.status })

    const report = {
      schemaVersion: 1,
      result: 'pass',
      runId: RUN_ID,
      generatedAt: new Date().toISOString(),
      binarySha256: sha256(fs.readFileSync(BINARY)),
      storage: { ...storage, postgresUrlSha256: sha256(POSTGRES_URL), redisUrlSha256: sha256(REDIS_URL), redisPrefixSha256: sha256(REDIS_PREFIX) },
      upstreams: Object.fromEntries(Object.entries(upstreams).map(([name, value]) => [name, {
        status: value.state.status,
        hits: value.state.hits,
        requestCount: value.state.requests.length,
        anthropicVersionPresent: value.state.requests.every((request) => Boolean(request.anthropicVersion)),
      }])),
      local: {
        hits: local.state.hits,
        inferenceHits: local.state.inferenceHits,
        requestCount: local.state.requests.length,
        requests: local.state.requests.slice(-20),
      },
      stress: stressReport,
      tempRoot: KEEP_TEMP ? TEMP_ROOT : null,
      results,
    }
    fs.writeFileSync(REPORT_PATH, `${JSON.stringify(report, null, 2)}\n`, { mode: 0o600 })
  } catch (error) {
    const failure = {
      schemaVersion: 1,
      result: 'fail',
      runId: RUN_ID,
      error: redact(error?.stack || error?.message || error),
      binarySha256: sha256(fs.readFileSync(BINARY)),
      serviceLogTail: fs.existsSync(logPath) ? redact(fs.readFileSync(logPath, 'utf8')) : null,
      upstreams: Object.fromEntries(Object.entries(upstreams).map(([name, value]) => [name, {
        status: value.state.status,
        hits: value.state.hits,
      }])),
      local: {
        hits: local.state.hits,
        inferenceHits: local.state.inferenceHits,
        requestCount: local.state.requests.length,
        requests: local.state.requests.slice(-20),
      },
      stress: null,
      tempRoot: KEEP_TEMP ? TEMP_ROOT : null,
    }
    fs.mkdirSync(REPORT_ROOT, { recursive: true, mode: 0o700 })
    fs.writeFileSync(REPORT_PATH, `${JSON.stringify(failure, null, 2)}\n`, { mode: 0o600 })
    throw error
  } finally {
    await stopChild(child)
    await Promise.all([...SERVERS].map((server) => new Promise((resolve) => server.close(resolve))))
    if (!KEEP_TEMP) {
      fs.rmSync(TEMP_ROOT, { recursive: true, force: true })
    } else {
      process.stderr.write(`kept validation temp root: ${TEMP_ROOT}\n`)
    }
  }
  process.stdout.write(`${REPORT_PATH}\n`)
}

main().catch((error) => {
  process.stderr.write(`external pool priority failover validation failed: ${redact(error?.stack || error?.message || error)}\nreport=${REPORT_PATH}\n`)
  process.exitCode = 1
})
