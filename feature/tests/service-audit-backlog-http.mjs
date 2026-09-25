#!/usr/bin/env node

/*
 * Service-audit backlog validation.
 *
 * This is deliberately a narrow fake-upstream HTTP case, not a production
 * load test. One credential and one global dispatch slot are occupied by a
 * stream which keeps producing data. Normal requests must remain pending until
 * that stream naturally completes, then complete under the existing queue
 * policy. One queued request is cancelled by the client to verify cleanup.
 */

import assert from 'node:assert/strict'
import crypto from 'node:crypto'
import fs from 'node:fs'
import http from 'node:http'
import net from 'node:net'
import os from 'node:os'
import path from 'node:path'
import { spawn } from 'node:child_process'
import { performance } from 'node:perf_hooks'

import { resolveRuntimeValidationPaths } from './runtime-validation-paths.mjs'

const ROOT = fs.realpathSync(path.resolve(import.meta.dirname, '../..'))
const { binary: BINARY, artifactRoot: ARTIFACT_ROOT } = resolveRuntimeValidationPaths(ROOT)
const POSTGRES_URL = requiredEnvironment('KIRO_BACKLOG_POSTGRES_URL')
const REDIS_URL = requiredEnvironment('KIRO_BACKLOG_REDIS_URL')
const REDIS_PREFIX = requiredEnvironment('KIRO_BACKLOG_REDIS_PREFIX')
const RUN_ID = `service-audit-backlog-${new Date().toISOString().replace(/[-:.TZ]/g, '')}-${process.pid}-${crypto.randomBytes(3).toString('hex')}`
const TEMP_ROOT = fs.mkdtempSync(path.join(os.tmpdir(), `${RUN_ID}-`))
const REPORT_ROOT = path.join(ARTIFACT_ROOT, 'reports', 'service-audit-backlog-http')
const REPORT_PATH = path.join(REPORT_ROOT, `${RUN_ID}.json`)
const REQUEST_KEY = 'sk-service-audit-backlog-request'
const ADMIN_KEY = 'sk-service-audit-backlog-admin'
const LOCAL_TOKEN = 'service-audit-backlog-local-token'
const MODEL = 'claude-sonnet-4'
const STREAM_HOLD_MS = 4_500
const QUEUE_PROBE_MS = 1_000
const MIN_STREAM_CHUNKS_BEFORE_RELEASE = 8

const SAFE_ENV_NAMES = [
  'PATH', 'TMPDIR', 'TMP', 'TEMP', 'LANG', 'LC_ALL', 'LC_CTYPE', 'TZ', 'USER', 'LOGNAME',
]

function requiredEnvironment(name) {
  const value = String(process.env[name] || '').trim()
  if (!value) throw new Error(`${name} is required`)
  return value
}

function minimalEnvironment(extra = {}) {
  const environment = {}
  for (const name of SAFE_ENV_NAMES) {
    if (process.env[name]) environment[name] = process.env[name]
  }
  return { ...environment, ...extra }
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

function sha256(value) {
  return crypto.createHash('sha256').update(value).digest('hex')
}

function sha256File(file) {
  return sha256(fs.readFileSync(file))
}

function safeTail(file, lines = 80) {
  if (!fs.existsSync(file)) return ''
  return fs.readFileSync(file, 'utf8').split(/\r?\n/).slice(-lines).join('\n')
}

function summarizeFakeCaptures(captureDir) {
  if (!fs.existsSync(captureDir)) return []
  return fs.readdirSync(captureDir).sort().map((name) => {
    const file = path.join(captureDir, name)
    const value = JSON.parse(fs.readFileSync(file, 'utf8'))
    return {
      name,
      requestId: value.requestId,
      path: value.path,
      target: value.headers?.['x-amz-target'] || null,
      contentType: value.headers?.['content-type'] || null,
      bodyKeys: Object.keys(value.body || {}).sort(),
      bodyStream: value.body?.stream ?? null,
      bodyOrigin: value.body?.origin ?? null,
      sha256: sha256File(file),
    }
  })
}

function redact(value) {
  let output = String(value || '')
  for (const secret of [POSTGRES_URL, REDIS_URL, REDIS_PREFIX, REQUEST_KEY, ADMIN_KEY, LOCAL_TOKEN, BINARY]) {
    if (secret) output = output.split(secret).join('<redacted>')
  }
  return output
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

function waitForExit(child, timeoutMs) {
  if (child.exitCode !== null || child.signalCode !== null) return Promise.resolve(true)
  return new Promise((resolve) => {
    const timer = setTimeout(() => {
      child.off('exit', onExit)
      resolve(false)
    }, timeoutMs)
    function onExit() {
      clearTimeout(timer)
      resolve(true)
    }
    child.once('exit', onExit)
  })
}

async function terminate(child, label) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return
  child.kill('SIGTERM')
  if (!(await waitForExit(child, 5_000))) {
    child.kill('SIGKILL')
    if (!(await waitForExit(child, 5_000))) throw new Error(`failed to stop ${label}`)
  }
}

function writeJson(file, value) {
  fs.writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`, { mode: 0o600 })
}

function databaseNameFromUrl(urlText) {
  const parsed = new URL(urlText)
  const database = decodeURIComponent(parsed.pathname.replace(/^\//, ''))
  if (!/^kiro_load_chaos_[a-z0-9_]{3,80}$/.test(database)) {
    throw new Error(`KIRO_BACKLOG_POSTGRES_URL must use a caller-owned kiro_load_chaos_* database: ${database}`)
  }
  if (!['127.0.0.1', 'localhost', '::1'].includes(parsed.hostname)) {
    throw new Error('KIRO_BACKLOG_POSTGRES_URL must target loopback')
  }
  if (Number(parsed.port || 5432) === 9022) throw new Error('port 9022 is protected')
  return database
}

function redisTargetFromUrl(urlText) {
  const parsed = new URL(urlText)
  if (parsed.protocol !== 'redis:') throw new Error('KIRO_BACKLOG_REDIS_URL must use redis://')
  if (parsed.username || parsed.password) throw new Error('KIRO_BACKLOG_REDIS_URL must not contain auth material')
  if (!['127.0.0.1', 'localhost', '::1'].includes(parsed.hostname)) {
    throw new Error('KIRO_BACKLOG_REDIS_URL must target loopback')
  }
  const database = Number(parsed.pathname.replace(/^\//, ''))
  if (!Number.isInteger(database) || database < 1 || database > 15) {
    throw new Error('KIRO_BACKLOG_REDIS_URL must use an isolated Redis database in 1..15')
  }
  if (!/^[a-z0-9][a-z0-9:._-]{7,95}$/.test(REDIS_PREFIX)) {
    throw new Error('KIRO_BACKLOG_REDIS_PREFIX has an invalid format')
  }
  if (REDIS_PREFIX.includes('kiro_rs:local')) throw new Error('Redis prefix is not caller-owned')
  return { parsed, port: Number(parsed.port || 6379), database }
}

function encodeRedisCommands(commands) {
  return Buffer.from(commands.map((parts) => (
    `*${parts.length}\r\n${parts.map((part) => {
      const text = String(part)
      return `$${Buffer.byteLength(text)}\r\n${text}\r\n`
    }).join('')}`
  )).join(''))
}

function parseRedisReply(buffer, offset = 0) {
  if (offset >= buffer.length) return null
  const type = String.fromCharCode(buffer[offset])
  const lineEnd = buffer.indexOf('\r\n', offset)
  if (lineEnd < 0) return null
  const line = buffer.toString('utf8', offset + 1, lineEnd)
  const next = lineEnd + 2
  if (type === '+' || type === '-' || type === ':') {
    return { type, value: type === ':' ? Number(line) : line, next }
  }
  if (type === '$') {
    const length = Number(line)
    if (length === -1) return { type, value: null, next }
    const end = next + length
    if (buffer.length < end + 2) return null
    return { type, value: buffer.toString('utf8', next, end), next: end + 2 }
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
  throw new Error(`unsupported Redis response type ${type}`)
}

function redisCommand(target, command) {
  const commands = [['SELECT', String(target.database)], command]
  return new Promise((resolve, reject) => {
    const socket = net.connect({ host: target.parsed.hostname, port: target.port })
    const payload = encodeRedisCommands(commands)
    let received = Buffer.alloc(0)
    let cursor = 0
    const replies = []
    let settled = false
    const finish = (error, value) => {
      if (settled) return
      settled = true
      socket.destroy()
      if (error) reject(error)
      else resolve(value)
    }
    socket.setTimeout(5_000)
    socket.once('connect', () => socket.write(payload))
    socket.on('data', (chunk) => {
      received = Buffer.concat([received.subarray(cursor), chunk])
      cursor = 0
      try {
        for (;;) {
          const reply = parseRedisReply(received, cursor)
          if (!reply) return
          if (reply.type === '-') return finish(new Error(`Redis command failed: ${reply.value}`))
          replies.push(reply.value)
          cursor = reply.next
          if (replies.length === commands.length) return finish(null, replies.at(-1))
        }
      } catch (error) {
        finish(error)
      }
    })
    socket.once('timeout', () => finish(new Error('Redis control command timed out')))
    socket.once('error', (error) => finish(error))
  })
}

async function cleanupRedis(target) {
  let cursor = '0'
  let scanned = 0
  let removed = 0
  do {
    const reply = await redisCommand(target, ['SCAN', cursor, 'MATCH', `${REDIS_PREFIX}:*`, 'COUNT', '1000'])
    cursor = String(reply?.[0] || '0')
    const keys = Array.isArray(reply?.[1]) ? reply[1] : []
    scanned += keys.length
    if (keys.length) removed += Number(await redisCommand(target, ['UNLINK', ...keys]) || 0)
  } while (cursor !== '0')
  const remainingReply = await redisCommand(target, ['SCAN', '0', 'MATCH', `${REDIS_PREFIX}:*`, 'COUNT', '1000'])
  const remaining = Array.isArray(remainingReply?.[1]) ? remainingReply[1].length : 0
  return { scanned, removed, remaining }
}

function serviceConfig({ databaseUrl, redisUrl, servicePort, upstreamPort }) {
  return {
    postgres: { url: databaseUrl, maxConnections: 6, migrateOnStart: true },
    redis: { url: redisUrl, keyPrefix: REDIS_PREFIX },
    host: '127.0.0.1',
    port: servicePort,
    apiKey: REQUEST_KEY,
    apiKeys: [],
    adminApiKey: ADMIN_KEY,
    requestAdmission: {
      rpm: 0,
      maxConcurrentRequests: 8,
      maxQueuedRequests: 8,
      queueTimeoutMs: 15_000,
    },
    defaultEndpoint: 'cli',
    kiroUpstreamBaseUrl: `http://127.0.0.1:${upstreamPort}`,
    credentialRpm: 0,
    credentialMaxConcurrentRequests: 1,
    credentialDispatchMaxWaitSecs: 15,
    credentialInFlightLeaseMaxSecs: 10,
    dispatchGlobalMaxConcurrentRequests: 1,
    dispatchMaxQueuedRequests: 8,
    kiroUpstreamResponseTimeoutSecs: 30,
    kiroUpstreamStreamIdleTimeoutSecs: 8,
    credentialRetryMaxAttempts: 0,
    inferenceUpstreamMaxAttempts: 1,
    auxiliaryUpstreamMaxAttempts: 1,
    loadBalancingMode: 'balanced',
    externalPools: {
      externalPoolsEnabled: false,
      fallbackOnSchedulerRedisDegraded: false,
      fallbackOnNoAvailableCredentials: false,
      fallbackOnLocalCapacityExhausted: false,
      fallbackOnLocalTransientExhausted: false,
    },
  }
}

function credentialsFixture() {
  return [{
    id: 1,
    kiroApiKey: LOCAL_TOKEN,
    authMethod: 'api_key',
    endpoint: 'cli',
    priority: 0,
    maxConcurrentRequests: 1,
    rpm: 0,
    rateLimitAutoDisableEnabled: false,
    supportedModels: [MODEL],
  }]
}

function messageBody(marker, stream) {
  return JSON.stringify({
    model: MODEL,
    max_tokens: 64,
    stream,
    metadata: { user_id: JSON.stringify({ session_id: sha256(marker).slice(0, 36) }) },
    messages: [{ role: 'user', content: marker }],
  })
}

function requestHeaders() {
  return { 'x-api-key': REQUEST_KEY, 'content-type': 'application/json', connection: 'close' }
}

async function timedRequest(baseUrl, marker, stream = false, signal) {
  const started = performance.now()
  const response = await fetch(`${baseUrl}/v1/messages`, {
    method: 'POST',
    headers: requestHeaders(),
    body: messageBody(marker, stream),
    signal,
  })
  const headersMs = performance.now() - started
  const text = await response.text()
  return {
    marker,
    status: response.status,
    headersMs: Number(headersMs.toFixed(2)),
    totalMs: Number((performance.now() - started).toFixed(2)),
    text: redact(text),
    requestId: response.headers.get('request-id') || response.headers.get('x-request-id'),
  }
}

async function waitForReady(baseUrl, child) {
  const deadline = Date.now() + 60_000
  let last = ''
  while (Date.now() < deadline) {
    if (child.exitCode !== null) throw new Error(`service exited early: ${child.exitCode}`)
    try {
      const response = await fetch(`${baseUrl}/readyz`)
      last = `${response.status} ${await response.text()}`
      if (response.status === 200 && last.includes('"ready"')) return
    } catch (error) {
      last = String(error.message || error)
    }
    await sleep(200)
  }
  throw new Error(`service did not become ready: ${last}`)
}

function startProcess(command, args, logPath, env = {}) {
  const log = fs.openSync(logPath, 'a')
  const child = spawn(command, args, {
    cwd: ROOT,
    env: minimalEnvironment(env),
    stdio: ['ignore', log, log],
  })
  child.once('exit', () => fs.closeSync(log))
  return child
}

async function startFake(fakePort, logPath, captureDir) {
  const child = startProcess(
    process.env.KIRO_BACKLOG_LOADTEST_BINARY,
    [
      '--fake-only', 'true',
      '--fake-listen', `127.0.0.1:${fakePort}`,
      '--scenario', 'long-stream',
      '--fake-kiro-eventstream', 'true',
      '--fake-delay-ms', '0',
      '--fake-stream-chunks', '45',
      '--fake-stream-chunk-delay-ms', '100',
      '--fake-capture-dir', captureDir,
    ],
    logPath,
    { RUST_LOG: 'warn' },
  )
  const deadline = Date.now() + 20_000
  while (Date.now() < deadline) {
    try {
      const socket = net.createConnection({ host: '127.0.0.1', port: fakePort })
      await new Promise((resolve, reject) => {
        socket.once('connect', resolve)
        socket.once('error', reject)
      })
      socket.destroy()
      return child
    } catch {
      await sleep(100)
    }
  }
  throw new Error('fake upstream did not listen')
}

async function probeFake(fakePort) {
  const response = await fetch(`http://127.0.0.1:${fakePort}/kiro`, {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
      'x-amz-target': 'AmazonCodeWhispererService.GenerateAssistantResponse',
    },
    body: JSON.stringify({ stream: true, messages: [] }),
  })
  assert.equal(response.status, 200, `fake upstream probe status ${response.status}`)
  const bytes = await response.arrayBuffer()
  assert.ok(bytes.byteLength > 0, 'fake upstream probe returned an empty body')
}

async function openHolder(baseUrl, marker) {
  const started = performance.now()
  const response = await fetch(`${baseUrl}/v1/messages`, {
    method: 'POST',
    headers: requestHeaders(),
    body: messageBody(marker, true),
  })
  if (response.status !== 200) {
    const body = await response.text()
    throw new Error(`${marker}: expected 200, got ${response.status}: ${redact(body)}`)
  }
  assert.ok(response.body)
  const reader = response.body.getReader()
  let chunks = 0
  let ended = false
  let readError = null
  const reading = (async () => {
    try {
      for (;;) {
        const next = await reader.read()
        if (next.done) {
          ended = true
          break
        }
        chunks += 1
      }
    } catch (error) {
      readError = error
    }
  })()
  return {
    startedAt: Date.now(),
    firstChunkMs: Number((performance.now() - started).toFixed(2)),
    get chunks() { return chunks },
    get ended() { return ended },
    get readError() { return readError },
    async drain() {
      await reading
    },
  }
}

async function main() {
  const database = databaseNameFromUrl(POSTGRES_URL)
  const redisTarget = redisTargetFromUrl(REDIS_URL)
  const loadtestBinary = requireExternalLoadtestBinary()
  const servicePort = await reservePort()
  const fakePort = await reservePort()
  const configPath = path.join(TEMP_ROOT, 'config.json')
  const credentialsPath = path.join(TEMP_ROOT, 'credentials.json')
  const fakeLog = path.join(TEMP_ROOT, 'fake.log')
  const fakeCaptureDir = path.join(TEMP_ROOT, 'fake-captures')
  const serviceLog = path.join(TEMP_ROOT, 'service.log')
  const baseUrl = `http://127.0.0.1:${servicePort}`
  writeJson(configPath, serviceConfig({
    databaseUrl: POSTGRES_URL,
    redisUrl: REDIS_URL,
    servicePort,
    upstreamPort: fakePort,
  }))
  writeJson(credentialsPath, credentialsFixture())

  let fake = null
  let service = null
  let holder = null
  const probes = []
  const startedAt = new Date().toISOString()
  let cleanup = null
  let pass = false
  let failure = null
  try {
    fake = await startFake(fakePort, fakeLog, fakeCaptureDir)
    await probeFake(fakePort)
    service = startProcess(
      BINARY,
      ['--config', configPath, '--credentials', credentialsPath],
      serviceLog,
      {
        KIRO_API_KEY: '',
        KIRO_RS_HOST: '127.0.0.1',
        KIRO_RS_PORT: String(servicePort),
        RUST_LOG: 'kiro_rs::kiro::provider=debug,kiro_rs::kiro::endpoint=debug,kiro_rs=info',
      },
    )
    await waitForReady(baseUrl, service)

    holder = await openHolder(baseUrl, 'BACKLOG-HOLDER')
    await sleep(250)
    assert.ok(holder.chunks >= 2, `holder did not keep producing data: ${holder.chunks}`)

    const queuedAt = Date.now()
    const queuedMarkers = ['BACKLOG-NORMAL-1', 'BACKLOG-NORMAL-2', 'BACKLOG-CANCEL']
    const controllers = new Map()
    for (const marker of queuedMarkers) {
      const controller = new AbortController()
      controllers.set(marker, controller)
      probes.push({
        marker,
        queuedAt,
        promise: timedRequest(baseUrl, marker, false, controller.signal)
          .then((value) => ({ kind: 'response', value, settledAt: Date.now() }))
          .catch((error) => ({ kind: 'error', error: { name: error.name, message: error.message }, settledAt: Date.now() })),
      })
    }

    await sleep(QUEUE_PROBE_MS)
    assert.equal(holder.ended, false, 'active long stream ended before the queue probe')
    assert.ok(holder.chunks >= MIN_STREAM_CHUNKS_BEFORE_RELEASE, `holder output stalled at ${holder.chunks} chunks`)
    const pendingAtProbe = probes.map((probe) => probe.promise)
    const settledBeforeRelease = await Promise.all(pendingAtProbe.map(async (promise) => (
      Promise.race([promise.then(() => true), sleep(20).then(() => false)])
    )))
    assert.deepEqual(settledBeforeRelease, [false, false, false], `normal request was not queued: ${JSON.stringify(settledBeforeRelease)}`)

    controllers.get('BACKLOG-CANCEL').abort()
    const cancelResult = await probes[2].promise
    assert.equal(cancelResult.kind, 'error', JSON.stringify(cancelResult))
    assert.equal(cancelResult.error.name, 'AbortError', JSON.stringify(cancelResult))

    await holder.drain()
    assert.equal(holder.readError, null, `holder stream read failed: ${holder.readError?.message || holder.readError}`)
    assert.equal(holder.ended, true)
    const normalResults = await Promise.all(probes.slice(0, 2).map((probe) => probe.promise))
    for (const result of normalResults) {
      assert.equal(result.kind, 'response', JSON.stringify(result))
      assert.equal(result.value.status, 200, JSON.stringify(result))
      assert.ok(result.settledAt - queuedAt >= 2_500, `request bypassed held capacity: ${JSON.stringify(result)}`)
    }
    const recovery = await timedRequest(baseUrl, 'BACKLOG-RECOVERY', false)
    assert.equal(recovery.status, 200, JSON.stringify(recovery))

    pass = true
    cleanup = { holderChunks: holder.chunks, holderFirstChunkMs: holder.firstChunkMs, normalResults: normalResults.map((item) => item.value), recovery }
  } catch (error) {
    failure = redact(error.stack || error.message || error)
  } finally {
    await terminate(service, 'backlog service').catch(() => {})
    await terminate(fake, 'backlog fake upstream').catch(() => {})
    cleanup = { ...(cleanup || {}), redis: await cleanupRedis(redisTarget).catch((error) => ({ error: redact(error.message) })) }
    fs.mkdirSync(REPORT_ROOT, { recursive: true, mode: 0o700 })
    const report = {
      result: pass ? 'pass' : 'fail',
      runId: RUN_ID,
      startedAt,
      finishedAt: new Date().toISOString(),
      database,
      servicePort,
      fakePort,
      productSha256: sha256File(BINARY),
      loadtestSha256: sha256File(loadtestBinary),
      config: {
        credentialMaxConcurrentRequests: 1,
        dispatchGlobalMaxConcurrentRequests: 1,
        dispatchMaxQueuedRequests: 8,
        credentialDispatchMaxWaitSecs: 15,
        streamIdleTimeoutSecs: 8,
      },
      streamHoldMs: STREAM_HOLD_MS,
      queueProbeMs: QUEUE_PROBE_MS,
      cleanup,
      failure,
      logs: {
        serviceSha256: fs.existsSync(serviceLog) ? sha256File(serviceLog) : null,
        fakeSha256: fs.existsSync(fakeLog) ? sha256File(fakeLog) : null,
        serviceTail: redact(safeTail(serviceLog)),
        fakeTail: redact(safeTail(fakeLog)),
      },
      fakeCapture: summarizeFakeCaptures(fakeCaptureDir),
    }
    fs.writeFileSync(REPORT_PATH, `${JSON.stringify(report, null, 2)}\n`, { mode: 0o600 })
    fs.rmSync(TEMP_ROOT, { recursive: true, force: true })
    process.stdout.write(`${JSON.stringify({ ...report, reportPath: REPORT_PATH }, null, 2)}\n`)
  }
  if (!pass) process.exitCode = 1
}

function requireExternalLoadtestBinary() {
  const value = String(process.env.KIRO_BACKLOG_LOADTEST_BINARY || '').trim()
  if (!value || !fs.existsSync(value)) throw new Error('KIRO_BACKLOG_LOADTEST_BINARY must point to the frozen kiro_loadtest binary')
  const resolved = fs.realpathSync(value)
  if (resolved.startsWith(`${ROOT}${path.sep}`)) throw new Error('KIRO_BACKLOG_LOADTEST_BINARY must be repository-external')
  return resolved
}

main().catch((error) => {
  process.stderr.write(`${redact(error.stack || error.message || error)}\n`)
  process.exitCode = 1
})
