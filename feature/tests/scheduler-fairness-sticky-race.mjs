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
import { validationChildEnvironment } from './validation-child-env.mjs'

const ROOT = path.resolve(import.meta.dirname, '../..')
const { binary: BINARY, artifactRoot: ARTIFACT_ROOT } = resolveRuntimeValidationPaths(ROOT)
const ROUNDS = Number.parseInt(process.env.KIRO_E01_E02_ROUNDS || '3', 10)
const ALL_MODES = ['priority', 'balanced', 'health_balanced', 'weighted_least_inflight']
const MODES = process.env.KIRO_E01_E02_MODES
  ? process.env.KIRO_E01_E02_MODES.split(',').map((mode) => mode.trim()).filter(Boolean)
  : ALL_MODES
const ACCOUNT_COUNT = 60
const SHORT_REQUESTS = 120
const PRIORITY_PREFERRED_COUNT = 10
const STICKY_REPEATS = 6
const LONG_HOLDERS = 30
const SHORT_DURING_LONG = 30
const RACE_BATCHES = 3
const RACE_REQUESTS_PER_INSTANCE = 12
const WEIGHTED_REQUESTS = 90
const REQUEST_KEY = 'sk-request-e0102-isolated-validation'
const ADMIN_KEY = 'sk-admin-e0102-isolated-validation'
const POSTGRES_URL_TEMPLATE = requiredEnvironment('KIRO_E01_E02_POSTGRES_URL_TEMPLATE')
const REDIS_URL = requiredEnvironment('KIRO_E01_E02_REDIS_URL')
const REDIS_PREFIX = requiredEnvironment('KIRO_E01_E02_REDIS_PREFIX')
const VALIDATE_ONLY = process.env.KIRO_E01_E02_VALIDATE_ONLY === '1'
const RUN_ID = `e0102-${new Date().toISOString().replace(/[-:.TZ]/g, '')}-${process.pid}-${crypto.randomBytes(3).toString('hex')}`
const TEMP_ROOT = path.join(ARTIFACT_ROOT, 'runtime', 'e01-e02', RUN_ID)
const REPORT_ROOT = path.join(ARTIFACT_ROOT, 'reports', 'e01-e02')
const REPORT_PATH = path.join(REPORT_ROOT, `${RUN_ID}.json`)
const REQUIRED_DATABASE_COUNT = MODES.length * ROUNDS
const POSTGRES_DATABASES = String(process.env.KIRO_E01_E02_POSTGRES_DATABASES || '')
  .split(',')
  .map((value) => value.trim())
  .filter(Boolean)
let redisTarget = null

// These thresholds are intentionally declared before any workload executes.
// A failing run must not relax them based on the observed distribution.
const ACCEPTANCE_CONTRACT = Object.freeze({
  priorityShort: {
    preferredPriority: 0,
    nonPreferredPriority: 10,
    preferredCredentialIds: [1, 10],
    selectedCredentialCoverage: PRIORITY_PREFERRED_COUNT,
    preferredIdsOnly: true,
    maxCountSpread: 1,
  },
  fairShort: {
    selectedCredentialCoverage: ACCOUNT_COUNT,
    maxCountSpread: 1,
    maxSingleCredentialShare: 0.02,
  },
  sticky: {
    sequentialUniqueCredentials: 1,
    fallbackMustDifferWhenBoundCredentialFull: true,
    reboundMustReturnToOriginal: true,
  },
  longShort: {
    expectedUnionCoverage: ACCOUNT_COUNT,
    maxPeakInFlightPerCredential: 1,
    maxQueueDepth: LONG_HOLDERS + SHORT_DURING_LONG,
  },
  candidateLeaseRace: {
    batches: RACE_BATCHES,
    requestsPerBatch: RACE_REQUESTS_PER_INSTANCE * 2,
    uniqueCredentialsPerBatch: RACE_REQUESTS_PER_INSTANCE * 2,
    minimumObservedReselectLogs: 1,
    maxPeakInFlightPerCredential: 1,
  },
  weightedCapacity: {
    requests: WEIGHTED_REQUESTS,
    scoreDefinition: 'inFlightRequests / maxConcurrentRequests',
    schedulerTopK: 1,
    groups: [
      { credentialIds: [1, 20], accountCount: 20, maxConcurrentRequests: 1 },
      { credentialIds: [21, 40], accountCount: 20, maxConcurrentRequests: 2 },
      { credentialIds: [41, 60], accountCount: 20, maxConcurrentRequests: 4 },
    ],
    theoreticalGreedyPhases: [
      { admitted: 60, totals: [20, 20, 20], reason: 'all zero-load accounts receive one lease' },
      { admitted: 80, totals: [20, 20, 40], reason: 'max=4 group has normalized load 0.25' },
      { admitted: 90, totals: [20, 30, 40], reason: 'max=2 and max=4 tie at 0.5; lower credential ID wins' },
    ],
    exactExpectedTotals: [20, 30, 40],
    requireStrictMonotonicTotals: true,
  },
  resources: {
    maxFdEndGrowth: 8,
    maxFocusedRssEndKb: 256 * 1024,
  },
})

if (!Number.isInteger(ROUNDS) || ROUNDS < 3 || ROUNDS > 5) {
  throw new Error('KIRO_E01_E02_ROUNDS must be an integer between 3 and 5')
}
if (MODES.length === 0 || MODES.some((mode) => !ALL_MODES.includes(mode))) {
  throw new Error(`KIRO_E01_E02_MODES must contain only: ${ALL_MODES.join(',')}`)
}

function requiredEnvironment(name) {
  const value = String(process.env[name] || '').trim()
  if (!value) throw new Error(`${name} is required`)
  return value
}

function validateInputs() {
  const placeholderCount = (POSTGRES_URL_TEMPLATE.match(/\{database\}/g) || []).length
  if (placeholderCount !== 1) {
    throw new Error('KIRO_E01_E02_POSTGRES_URL_TEMPLATE must contain exactly one literal {database} placeholder')
  }
  const sampleDatabase = POSTGRES_DATABASES[0] || 'kiro_e0102_contract_sample'
  const postgres = new URL(POSTGRES_URL_TEMPLATE.replace('{database}', sampleDatabase))
  if (!['postgres:', 'postgresql:'].includes(postgres.protocol)) {
    throw new Error('KIRO_E01_E02_POSTGRES_URL_TEMPLATE must use PostgreSQL')
  }
  if (!['127.0.0.1', 'localhost', '::1'].includes(postgres.hostname)) {
    throw new Error('KIRO_E01_E02_POSTGRES_URL_TEMPLATE must target loopback')
  }
  if (Number(postgres.port || 5432) === 9022) throw new Error('port 9022 is protected')
  if (POSTGRES_DATABASES.length !== REQUIRED_DATABASE_COUNT) {
    throw new Error(`KIRO_E01_E02_POSTGRES_DATABASES must contain exactly ${REQUIRED_DATABASE_COUNT} pre-created database names`)
  }
  for (const database of POSTGRES_DATABASES) {
    if (!/^kiro_e0102_[a-z0-9_]{3,80}$/.test(database)) {
      throw new Error('KIRO_E01_E02_POSTGRES_DATABASES must contain caller-owned kiro_e0102_* names')
    }
  }

  const redis = new URL(REDIS_URL)
  if (redis.protocol !== 'redis:') throw new Error('KIRO_E01_E02_REDIS_URL must use redis://')
  if (redis.username || redis.password) {
    throw new Error('KIRO_E01_E02_REDIS_URL must not contain Redis auth material')
  }
  if (!['127.0.0.1', 'localhost', '::1'].includes(redis.hostname)) {
    throw new Error('KIRO_E01_E02_REDIS_URL must target loopback')
  }
  if (redis.search || redis.hash) throw new Error('KIRO_E01_E02_REDIS_URL must not contain query or fragment data')
  const redisPort = Number(redis.port || 6379)
  if (redisPort === 9022) throw new Error('port 9022 is protected')
  const dbText = redis.pathname.replace(/^\//, '')
  if (!/^\d+$/.test(dbText)) throw new Error('KIRO_E01_E02_REDIS_URL must name a Redis database')
  const redisDatabase = Number(dbText)
  if (!Number.isSafeInteger(redisDatabase) || redisDatabase < 1 || redisDatabase > 15) {
    throw new Error('KIRO_E01_E02_REDIS_URL must use an isolated nonzero database in 1..15')
  }
  if (REDIS_PREFIX.includes('kiro_rs:local')) {
    throw new Error('KIRO_E01_E02_REDIS_PREFIX must be a caller-owned temporary prefix')
  }
  if (!/^[a-z0-9][a-z0-9:._-]{7,95}$/.test(REDIS_PREFIX)) {
    throw new Error('KIRO_E01_E02_REDIS_PREFIX has an invalid format')
  }
  redisTarget = { redis, redisPort, redisDatabase }
  return {
    postgresHost: postgres.hostname,
    postgresPort: Number(postgres.port || 5432),
    redisHost: redis.hostname,
    redisPort,
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
    ...options,
  })
  if (result.status !== 0) {
    const stderr = String(result.stderr || '').trim().slice(0, 4000)
    throw new Error(`${command} ${args.join(' ')} failed (${result.status}): ${stderr}`)
  }
  return String(result.stdout || '').trim()
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
  if (lineEnd === -1) return null
  const header = buffer.toString('utf8', offset + 1, lineEnd)
  const afterHeader = lineEnd + 2
  if (type === '+') return { type, value: header, next: afterHeader }
  if (type === '-') return { type, value: header, next: afterHeader }
  if (type === ':') return { type, value: Number(header), next: afterHeader }
  if (type === '$') {
    const length = Number(header)
    if (length === -1) return { type, value: null, next: afterHeader }
    const end = afterHeader + length
    if (buffer.length < end + 2) return null
    return { type, value: buffer.toString('utf8', afterHeader, end), next: end + 2 }
  }
  if (type === '*') {
    const count = Number(header)
    if (count === -1) return { type, value: null, next: afterHeader }
    const values = []
    let cursor = afterHeader
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

async function scanRedisPrefix(prefix, limit = 20_000) {
  const keys = []
  let cursor = '0'
  do {
    const reply = await redisCommand(['SCAN', cursor, 'MATCH', `${prefix}:*`, 'COUNT', '1000'])
    cursor = String(reply[0])
    for (const key of reply[1]) {
      keys.push(key)
      if (keys.length > limit) throw new Error(`too many Redis keys for owned prefix ${prefix}`)
    }
  } while (cursor !== '0')
  return keys
}

async function cleanupRedisPrefix(prefix) {
  const keys = await scanRedisPrefix(prefix)
  let removed = 0
  for (let index = 0; index < keys.length; index += 100) {
    const chunk = keys.slice(index, index + 100)
    if (chunk.length) removed += Number(await redisCommand(['DEL', ...chunk])) || 0
  }
  const remaining = (await scanRedisPrefix(prefix)).length
  return { prefixSha256: sha256(prefix), removed, remaining }
}

async function redisDiagnostics(prefixes) {
  const collect = async (args, maxChars = 64 * 1024) => {
    try {
      const value = await redisCommand(args, 2_000)
      return {
        ok: true,
        value: String(Array.isArray(value) ? JSON.stringify(value) : value).slice(-maxChars),
      }
    } catch (error) {
      return { ok: false, error: String(error.message || error).slice(0, 4_000) }
    }
  }
  const prefixCounts = []
  for (const prefix of prefixes) {
    try {
      prefixCounts.push({ prefixSha256: sha256(prefix), count: (await scanRedisPrefix(prefix)).length })
    } catch (error) {
      prefixCounts.push({ prefixSha256: sha256(prefix), error: String(error.message || error) })
    }
  }
  return {
    capturedAt: new Date().toISOString(),
    slowlogLength: await collect(['SLOWLOG', 'LEN'], 4_000),
    slowlog: await collect(['SLOWLOG', 'GET', '256']),
    latencyLatest: await collect(['LATENCY', 'LATEST'], 16_000),
    commandStats: await collect(['INFO', 'commandstats']),
    stats: await collect(['INFO', 'stats']),
    clients: await collect(['INFO', 'clients']),
    memory: await collect(['INFO', 'memory']),
    prefixCounts,
  }
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

async function waitForCondition(predicate, description, timeoutMs = 10_000) {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    if (predicate()) return
    await new Promise((resolve) => setTimeout(resolve, 10))
  }
  throw new Error(`timeout waiting for ${description}`)
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

async function readBody(request) {
  const chunks = []
  for await (const chunk of request) chunks.push(chunk)
  return Buffer.concat(chunks).toString('utf8')
}

function extractMarker(raw) {
  return raw.match(/E0102-[A-Za-z0-9_-]+/)?.[0] || 'E0102-unknown'
}

function upstreamDelayMs(marker) {
  if (marker.includes('-WEIGHT-')) return 3_000
  if (marker.includes('-RACE-')) return 500
  if (marker.includes('-STICKY-HOLDER-')) return 3_000
  if (marker.includes('-LONG-')) return 500
  return 2
}

function createFakeUpstreams() {
  const records = []
  const externalRecords = []
  const currentByCredential = new Map()
  const peakByCredential = new Map()
  const holds = new Map()
  let globalInFlight = 0
  let globalPeakInFlight = 0

  function hold(prefix, timeoutMs = 30_000) {
    assert.ok(!holds.has(prefix), `duplicate fake-upstream hold: ${prefix}`)
    let resolveGate
    const gate = new Promise((resolve) => {
      resolveGate = resolve
    })
    const entry = {
      gate,
      timer: null,
      release() {
        if (!holds.delete(prefix)) return
        clearTimeout(entry.timer)
        resolveGate()
      },
    }
    entry.timer = setTimeout(() => entry.release(), timeoutMs)
    entry.timer.unref()
    holds.set(prefix, entry)
  }

  function release(prefix) {
    holds.get(prefix)?.release()
  }

  function releaseAll() {
    for (const entry of [...holds.values()]) entry.release()
  }

  const local = http.createServer(async (request, response) => {
    const raw = await readBody(request)
    const marker = extractMarker(raw)
    const target = String(request.headers['x-amz-target'] || '')
    const authorization = String(request.headers.authorization || '')
    const selectedCredential = Number.parseInt(
      authorization.match(/e0102-token-(\d+)/)?.[1] || '0',
      10,
    )
    const kind = target.endsWith('.ListAvailableModels')
      || String(request.url || '').toLowerCase().includes('listavailablemodels')
      ? 'auxiliary'
      : 'inference'
    const record = {
      marker,
      kind,
      selectedCredential,
      startedAt: Date.now(),
      completedAt: null,
    }
    records.push(record)

    if (!Number.isInteger(selectedCredential) || selectedCredential < 1 || selectedCredential > ACCOUNT_COUNT) {
      writeJson(response, 401, { message: 'unknown fake credential' })
      return
    }
    if (kind === 'auxiliary') {
      writeJson(response, 200, {
        models: [{
          modelId: 'claude-sonnet-4',
          modelName: 'E01/E02 Sonnet',
          supportedInputTypes: ['TEXT'],
        }],
      })
      return
    }

    const current = (currentByCredential.get(selectedCredential) || 0) + 1
    currentByCredential.set(selectedCredential, current)
    peakByCredential.set(
      selectedCredential,
      Math.max(peakByCredential.get(selectedCredential) || 0, current),
    )
    globalInFlight += 1
    globalPeakInFlight = Math.max(globalPeakInFlight, globalInFlight)
    const held = [...holds.entries()].find(([prefix]) => marker.startsWith(prefix))?.[1]
    if (held) {
      await held.gate
    } else {
      await new Promise((resolve) => setTimeout(resolve, upstreamDelayMs(marker)))
    }
    const body = Buffer.concat([
      eventFrame('assistantResponseEvent', {
        content: `selected-${selectedCredential} ${marker}`,
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
    currentByCredential.set(selectedCredential, current - 1)
    globalInFlight -= 1
  })

  const external = http.createServer(async (request, response) => {
    const raw = await readBody(request)
    const marker = extractMarker(raw)
    externalRecords.push({ marker, at: Date.now() })
    writeJson(response, 200, {
      id: `msg_external_${externalRecords.length}`,
      type: 'message',
      role: 'assistant',
      model: 'claude-sonnet-4',
      content: [{ type: 'text', text: `external-unexpected ${marker}` }],
      stop_reason: 'end_turn',
      stop_sequence: null,
      usage: { input_tokens: 8, output_tokens: 2 },
    })
  })

  return {
    records,
    externalRecords,
    peakByCredential,
    get globalPeakInFlight() {
      return globalPeakInFlight
    },
    hold,
    release,
    inferenceRecords(prefix) {
      return records.filter((record) => record.kind === 'inference' && record.marker.startsWith(prefix))
    },
    externalHits(prefix) {
      return externalRecords.filter((record) => record.marker.startsWith(prefix)).length
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
      releaseAll()
      await Promise.all([
        new Promise((resolve) => local.close(resolve)),
        new Promise((resolve) => external.close(resolve)),
      ])
    },
  }
}

function processResources(pid) {
  const ps = spawnSync('ps', ['-o', 'rss=', '-p', String(pid)], { encoding: 'utf8' })
  const rssKb = Number.parseInt(String(ps.stdout || '').trim(), 10) || 0
  const lsof = spawnSync('lsof', ['-nP', '-p', String(pid)], { encoding: 'utf8' })
  const fdCount = String(lsof.stdout || '').trim().split('\n').filter(Boolean).length
  return { rssKb, fdCount }
}

function listenerSnapshot(port) {
  const result = spawnSync(
    'lsof',
    ['-nP', `-iTCP:${port}`, '-sTCP:LISTEN', '-Fpcn'],
    { encoding: 'utf8' },
  )
  if (result.status !== 0 && result.status !== 1) {
    throw new Error(`failed to inspect protected listener on port ${port}: ${result.stderr}`)
  }
  return String(result.stdout || '').trim().split('\n').filter(Boolean).sort()
}

function startService(configPath, credentialsPath, logPath, servicePort) {
  const log = fs.openSync(logPath, 'a')
  const handle = spawn(BINARY, ['--config', configPath, '--credentials', credentialsPath], {
    cwd: ROOT,
    env: validationChildEnvironment({
      KIRO_API_KEY: '',
      KIRO_RS_HOST: '127.0.0.1',
      KIRO_RS_PORT: String(servicePort),
      RUST_LOG: 'kiro_rs::kiro::token_manager=debug,kiro_rs=info',
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
    new Promise((resolve) => setTimeout(() => resolve(false), 10_000)),
  ])
  if (!exited && handle.exitCode === null) {
    handle.kill('SIGKILL')
    await new Promise((resolve) => handle.once('exit', resolve))
  }
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

function adminHeaders() {
  return { authorization: `Bearer ${ADMIN_KEY}`, 'content-type': 'application/json', connection: 'close' }
}

function requestHeaders() {
  return { 'x-api-key': REQUEST_KEY, 'content-type': 'application/json', connection: 'close' }
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
    errorId: response.headers.get('x-error-id'),
    headerMs: Number((headersAt - started).toFixed(2)),
    totalMs: Number((ended - started).toFixed(2)),
  }
}

function deterministicSessionId(seed) {
  const value = sha256(seed)
  return `${value.slice(0, 8)}-${value.slice(8, 12)}-4${value.slice(13, 16)}-8${value.slice(17, 20)}-${value.slice(20, 32)}`
}

function messageBody(marker, sessionSeed) {
  return JSON.stringify({
    model: 'claude-sonnet-4',
    max_tokens: 64,
    stream: false,
    metadata: {
      user_id: JSON.stringify({ session_id: deterministicSessionId(sessionSeed) }),
    },
    messages: [{ role: 'user', content: marker }],
  })
}

async function sendMessage(baseUrl, marker, sessionSeed = marker) {
  const response = await timedRequest(`${baseUrl}/v1/messages`, {
    method: 'POST',
    headers: requestHeaders(),
    body: messageBody(marker, sessionSeed),
  })
  const selectedCredential = Number.parseInt(
    response.text.match(/selected-(\d+)/)?.[1] || '0',
    10,
  )
  return { marker, selectedCredential, ...response }
}

function credentialsFor(mode) {
  return Array.from({ length: ACCOUNT_COUNT }, (_, index) => {
    const id = index + 1
    return {
      id,
      accessToken: `e0102-token-${id}`,
      machineId: deterministicSessionId(`e0102-machine-${id}`),
      expiresAt: '2099-01-01T00:00:00Z',
      authMethod: 'social',
      endpoint: 'ide',
      profileArn: `arn:aws:codewhisperer:us-east-1:123456789012:profile/E0102_${id}`,
      priority: mode === 'priority' && id > PRIORITY_PREFERRED_COUNT ? 10 : 0,
      maxConcurrentRequests: 1,
      rpm: 0,
      supportedModels: ['claude-sonnet-4'],
      disabled: false,
    }
  })
}

function serviceConfig({ databaseUrl, redisUrl, redisPrefix, servicePort, localPort, mode }) {
  return {
    postgres: {
      url: databaseUrl,
      maxConnections: 4,
      migrateOnStart: true,
    },
    redis: { url: redisUrl, keyPrefix: redisPrefix },
    host: '127.0.0.1',
    port: servicePort,
    apiKey: REQUEST_KEY,
    adminApiKey: ADMIN_KEY,
    requestAdmission: {
      rpm: 0,
      maxConcurrentRequests: 0,
      maxQueuedRequests: 0,
      queueTimeoutMs: 0,
    },
    defaultEndpoint: 'ide',
    kiroUpstreamBaseUrl: `http://127.0.0.1:${localPort}/kiro`,
    kiroUpstreamResponseTimeoutSecs: 120,
    kiroUpstreamStreamIdleTimeoutSecs: 8,
    kiroUpstreamStreamRetryEnabled: false,
    credentialRetryMaxAttempts: 1,
    inferenceUpstreamMaxAttempts: 1,
    credentialWarmupRequests: 0,
    credentialRpm: 0,
    credentialMaxConcurrentRequests: 1,
    credentialDispatchMaxWaitSecs: 5,
    dispatchGlobalMaxConcurrentRequests: 160,
    dispatchMaxQueuedRequests: 160,
    loadBalancingMode: mode,
    schedulerTopK: 1,
    schedulerPriorityWeight: mode === 'weighted_least_inflight' ? 0 : 1,
    schedulerLoadWeight: 100,
    schedulerErrorWeight: mode === 'weighted_least_inflight' ? 0 : 100,
    schedulerLatencyWeight: mode === 'weighted_least_inflight' ? 0 : 0.01,
    schedulerProbationWeight: mode === 'weighted_least_inflight' ? 0 : 50,
    schedulerSelectionPressureWeight: mode === 'weighted_least_inflight' ? 0 : 25,
    schedulerTotalSelectionWeight: 0,
    externalPools: {
      externalPoolsEnabled: true,
      externalPoolRetryMaxAttempts: 1,
      externalPoolRequestTimeoutSecs: 8,
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
}

async function configureExternalPool(baseUrl, externalPort, name) {
  const response = await timedRequest(`${baseUrl}/api/admin/external-pools`, {
    method: 'POST',
    headers: adminHeaders(),
    body: JSON.stringify({
      name,
      baseUrl: `http://127.0.0.1:${externalPort}/external`,
      apiKey: 'sk-e0102-fake-external',
      authType: 'bearer',
      enabled: true,
      priority: 0,
      maxConcurrentRequests: 200,
      usageProjectionMode: 'pass_through',
      requestBodyMode: 'normalized',
      rawModelMode: 'none',
      preservePath: true,
      supportedModels: ['claude-sonnet-4'],
    }),
  })
  assert.equal(response.status, 200, response.text)
  await new Promise((resolve) => setTimeout(resolve, 350))
}

function quantiles(values) {
  if (values.length === 0) return { min: 0, p50: 0, p95: 0, p99: 0, max: 0 }
  const sorted = [...values].sort((a, b) => a - b)
  const pick = (fraction) => sorted[Math.min(sorted.length - 1, Math.ceil(sorted.length * fraction) - 1)]
  return {
    min: sorted[0],
    p50: pick(0.5),
    p95: pick(0.95),
    p99: pick(0.99),
    max: sorted[sorted.length - 1],
  }
}

function selectionCounts(responses) {
  const counts = new Map()
  for (const response of responses) {
    assert.equal(response.status, 200, JSON.stringify(response))
    assert.ok(response.requestId, JSON.stringify(response))
    assert.ok(response.selectedCredential >= 1 && response.selectedCredential <= ACCOUNT_COUNT,
      JSON.stringify(response))
    counts.set(response.selectedCredential, (counts.get(response.selectedCredential) || 0) + 1)
  }
  return counts
}

function distributionSummary(responses) {
  const counts = selectionCounts(responses)
  const values = [...counts.values()]
  const total = values.reduce((sum, value) => sum + value, 0)
  return {
    total,
    coverage: counts.size,
    min: Math.min(...values),
    max: Math.max(...values),
    maxShare: Math.max(...values) / total,
    counts: Object.fromEntries([...counts.entries()].sort((a, b) => a[0] - b[0])),
  }
}

function diagnosticLogSummary(logPath) {
  if (!fs.existsSync(logPath)) {
    return {
      reselect: 0,
      queueWait: 0,
      redisDegraded: 0,
      pgSuccessSlowOver100: 0,
      pgSuccessMaxMs: 0,
      pgStatsSlowOver100: 0,
      pgStatsMaxMs: 0,
      lines: [],
    }
  }
  const reselectMarkers = [
    '并发槽已被其他请求占用',
    '并发槽已满，本次请求临时排除并重选',
  ]
  const redacted = fs.readFileSync(logPath, 'utf8')
    .replaceAll(REQUEST_KEY, '<redacted-request-key>')
    .replaceAll(ADMIN_KEY, '<redacted-admin-key>')
    .replaceAll('sk-e0102-fake-external', '<redacted-external-key>')
    .replace(/e0102-token-\d+/g, '<redacted-local-token>')
    .replace(/\u001b\[[0-9;]*m/g, '')
  const allLines = redacted.split('\n')
  const elapsedMs = (line) => {
    const matched = line.match(/elapsed_ms[=: ]+(\d+)/)
    return matched ? Number.parseInt(matched[1], 10) : 0
  }
  const pgSuccessLines = allLines.filter((line) =>
    line.includes('原子记录 PgSQL 凭据调用成功') && line.includes('同步存储操作耗时较长'))
  const pgStatsLines = allLines.filter((line) =>
    line.includes('保存凭据统计增量到 PgSQL') && line.includes('同步存储操作耗时较长'))
  const lines = allLines.filter((line) => [
    ...reselectMarkers,
    '进入排队等待',
    'Redis 调度热路径',
    '原子记录 PgSQL 凭据调用成功',
    '保存凭据统计增量到 PgSQL',
  ].some((marker) => line.includes(marker)))
  return {
    reselect: lines.filter((line) => reselectMarkers.some((marker) => line.includes(marker))).length,
    queueWait: lines.filter((line) => line.includes('进入排队等待')).length,
    redisDegraded: lines.filter((line) => line.includes('Redis 调度热路径')).length,
    pgSuccessSlowOver100: pgSuccessLines.length,
    pgSuccessMaxMs: Math.max(0, ...pgSuccessLines.map(elapsedMs)),
    pgStatsSlowOver100: pgStatsLines.length,
    pgStatsMaxMs: Math.max(0, ...pgStatsLines.map(elapsedMs)),
    lines: lines.slice(-100),
  }
}

async function fetchSummary(baseUrl) {
  const response = await timedRequest(`${baseUrl}/api/admin/credentials/summary`, {
    headers: adminHeaders(),
  })
  assert.equal(response.status, 200, response.text)
  return JSON.parse(response.text)
}

async function fetchRuntime(baseUrl) {
  const ids = Array.from({ length: ACCOUNT_COUNT }, (_, index) => index + 1).join(',')
  const response = await timedRequest(`${baseUrl}/api/admin/credentials/runtime?ids=${ids}`, {
    headers: adminHeaders(),
  })
  assert.equal(response.status, 200, response.text)
  return JSON.parse(response.text)
}

async function fetchExternalStatus(baseUrl) {
  const response = await timedRequest(`${baseUrl}/api/admin/external-pools/status`, {
    headers: adminHeaders(),
  })
  assert.equal(response.status, 200, response.text)
  return JSON.parse(response.text)
}

async function waitForExternalReady(services, timeoutMs = 15_000) {
  const deadline = Date.now() + timeoutMs
  let latest = []
  let lastError = null
  while (Date.now() < deadline) {
    try {
      latest = await Promise.all(services.map(async (service) => ({
        name: service.name,
        status: await fetchExternalStatus(service.baseUrl),
      })))
      if (latest.every(({ status }) => status.pools.length > 0
        && status.pools.some((pool) => pool.dispatchable))) {
        return latest
      }
    } catch (error) {
      lastError = error
    }
    await new Promise((resolve) => setTimeout(resolve, 100))
  }
  throw new Error(`external pools did not become dispatchable: ${JSON.stringify({
    latest,
    lastError: lastError ? String(lastError.stack || lastError.message) : null,
  })}`)
}

function startSampler(services) {
  const samples = []
  let stopped = false
  const loop = (async () => {
    while (!stopped) {
      const sample = { at: Date.now(), services: [] }
      for (const service of services) {
        const resources = processResources(service.handle.pid)
        let summary = null
        try {
          summary = await fetchSummary(service.baseUrl)
        } catch {}
        sample.services.push({ name: service.name, ...resources, summary })
      }
      samples.push(sample)
      await new Promise((resolve) => setTimeout(resolve, 25))
    }
  })()
  return {
    async stop() {
      stopped = true
      await loop
      return samples
    },
  }
}

async function setCredentialValue(baseUrl, id, field, value) {
  const endpoint = field === 'priority' ? 'priority' : 'concurrency'
  const body = field === 'priority'
    ? { priority: value }
    : { maxConcurrentRequests: value }
  const response = await timedRequest(`${baseUrl}/api/admin/credentials/${id}/${endpoint}`, {
    method: 'POST',
    headers: adminHeaders(),
    body: JSON.stringify(body),
  })
  assert.equal(response.status, 200, response.text)
}

async function waitForRuntimeConcurrency(baseUrl, expectedById, label) {
  await waitForCondition(
    async () => {
      const runtime = await fetchRuntime(baseUrl)
      if (!runtime.fresh) return false
      return runtime.items.every((item) => (
        expectedById.get(item.id) === undefined
          || item.maxConcurrentRequests === expectedById.get(item.id)
      ))
    },
    `${label} runtime concurrency propagation`,
    15_000,
  )
}

async function startCaseService({
  name,
  mode,
  databaseUrl,
  redisUrl,
  redisPrefix,
  localPort,
  externalPort,
  caseRoot,
  servicePorts,
  poolName,
}) {
  const servicePort = await reservePort()
  assert.notEqual(servicePort, 9022)
  servicePorts.push(servicePort)
  const serviceRoot = path.join(caseRoot, name)
  fs.mkdirSync(serviceRoot, { recursive: true, mode: 0o700 })
  const configPath = path.join(serviceRoot, 'config.json')
  const credentialsPath = path.join(serviceRoot, 'credentials.json')
  const logPath = path.join(serviceRoot, 'service.log')
  fs.writeFileSync(
    configPath,
    `${JSON.stringify(serviceConfig({
      databaseUrl,
      redisUrl,
      redisPrefix,
      servicePort,
      localPort,
      mode,
    }), null, 2)}\n`,
    { mode: 0o600 },
  )
  fs.writeFileSync(
    credentialsPath,
    `${JSON.stringify(credentialsFor(mode), null, 2)}\n`,
    { mode: 0o600 },
  )
  const handle = startService(configPath, credentialsPath, logPath, servicePort)
  const baseUrl = `http://127.0.0.1:${servicePort}`
  try {
    await waitForHealth(baseUrl, handle)
    await configureExternalPool(baseUrl, externalPort, poolName)
    const summary = await fetchSummary(baseUrl)
    assert.equal(summary.total, ACCOUNT_COUNT, JSON.stringify(summary))
    assert.equal(summary.available, ACCOUNT_COUNT, JSON.stringify(summary))
    return {
      name,
      baseUrl,
      servicePort,
      handle,
      logPath,
      resourcesStart: processResources(handle.pid),
    }
  } catch (error) {
    await stopService(handle)
    const startupLog = fs.existsSync(logPath)
      ? fs.readFileSync(logPath, 'utf8').slice(-30_000)
        .replaceAll(REQUEST_KEY, '<redacted-request-key>')
        .replaceAll(ADMIN_KEY, '<redacted-admin-key>')
        .replaceAll('sk-e0102-fake-external', '<redacted-external-key>')
        .replace(/e0102-token-\d+/g, '<redacted-local-token>')
      : '<startup log unavailable>'
    throw new Error(
      `${name} service failed during startup: ${error.stack || error.message}\nstartup log:\n${startupLog}`,
    )
  }
}

async function shortDistributionWorkload(primary, mode, casePrefix) {
  const responses = []
  const batchSize = mode === 'priority' ? PRIORITY_PREFERRED_COUNT : ACCOUNT_COUNT
  for (let offset = 0; offset < SHORT_REQUESTS; offset += batchSize) {
    const batch = await Promise.all(
      Array.from({ length: Math.min(batchSize, SHORT_REQUESTS - offset) }, (_, batchIndex) => {
        const index = offset + batchIndex + 1
        return sendMessage(
          primary.baseUrl,
          `${casePrefix}-SHORT-${index}`,
          `${casePrefix}-short-session-${index}`,
        )
      }),
    )
    responses.push(...batch)
  }
  const distribution = distributionSummary(responses)
  if (mode === 'priority') {
    const contract = ACCEPTANCE_CONTRACT.priorityShort
    assert.equal(distribution.coverage, contract.selectedCredentialCoverage, JSON.stringify(distribution))
    assert.ok(
      Object.keys(distribution.counts).every((id) => Number(id) >= contract.preferredCredentialIds[0]
        && Number(id) <= contract.preferredCredentialIds[1]),
      `priority selected outside the minimum-priority set: ${JSON.stringify(distribution)}`,
    )
    assert.ok(distribution.max - distribution.min <= contract.maxCountSpread,
      JSON.stringify(distribution))
  } else {
    const contract = ACCEPTANCE_CONTRACT.fairShort
    assert.equal(distribution.coverage, contract.selectedCredentialCoverage, JSON.stringify(distribution))
    assert.ok(distribution.max - distribution.min <= contract.maxCountSpread,
      JSON.stringify(distribution))
    assert.ok(distribution.maxShare <= contract.maxSingleCredentialShare,
      JSON.stringify(distribution))
  }
  return {
    distribution,
    latency: {
      headerMs: quantiles(responses.map((response) => response.headerMs)),
      totalMs: quantiles(responses.map((response) => response.totalMs)),
    },
    requestIds: responses.slice(0, 20).map((response) => response.requestId),
  }
}

async function stickyWorkload(primary, secondary, fake, casePrefix) {
  const sessionSeed = `${casePrefix}-sticky-session`
  const initial = await sendMessage(primary.baseUrl, `${casePrefix}-STICKY-INIT`, sessionSeed)
  assert.equal(initial.status, 200, JSON.stringify(initial))
  const originalCredential = initial.selectedCredential
  const sequential = []
  for (let index = 1; index <= STICKY_REPEATS; index += 1) {
    const service = index % 2 === 0 ? primary : secondary
    sequential.push(await sendMessage(
      service.baseUrl,
      `${casePrefix}-STICKY-SEQ-${index}`,
      sessionSeed,
    ))
  }
  assert.equal(
    new Set(sequential.map((response) => response.selectedCredential)).size,
    ACCEPTANCE_CONTRACT.sticky.sequentialUniqueCredentials,
    JSON.stringify(sequential),
  )
  assert.ok(sequential.every((response) => response.selectedCredential === originalCredential),
    JSON.stringify(sequential))

  const holderMarker = `${casePrefix}-STICKY-HOLDER-1`
  fake.hold(holderMarker)
  const holder = sendMessage(primary.baseUrl, holderMarker, sessionSeed)
  let fallback
  let phaseError = null
  try {
    await waitForCondition(
      () => {
        const records = fake.inferenceRecords(holderMarker)
        return records.length === 1 && records[0].completedAt === null
      },
      `${casePrefix} active sticky holder to reach fake upstream`,
    )
    fallback = await sendMessage(
      secondary.baseUrl,
      `${casePrefix}-STICKY-FALLBACK-1`,
      sessionSeed,
    )
    assert.equal(fallback.status, 200, JSON.stringify(fallback))
    assert.notEqual(fallback.selectedCredential, originalCredential, JSON.stringify(fallback))
  } catch (error) {
    phaseError = error
  } finally {
    fake.release(holderMarker)
  }
  const holderResult = await Promise.allSettled([holder])
  if (phaseError) throw phaseError
  if (holderResult[0].status === 'rejected') throw holderResult[0].reason
  const holderResponse = holderResult[0].value
  assert.equal(holderResponse.selectedCredential, originalCredential, JSON.stringify(holderResponse))
  const rebound = await sendMessage(
    secondary.baseUrl,
    `${casePrefix}-STICKY-REBOUND-1`,
    sessionSeed,
  )
  assert.equal(rebound.selectedCredential, originalCredential, JSON.stringify(rebound))
  return {
    originalCredential,
    sequentialCredentials: sequential.map((response) => response.selectedCredential),
    fallbackCredential: fallback.selectedCredential,
    reboundCredential: rebound.selectedCredential,
  }
}

async function longShortWorkload(primary, fake, casePrefix) {
  const longPrefix = `${casePrefix}-LONG-`
  fake.hold(longPrefix)
  const longPromises = Array.from({ length: LONG_HOLDERS }, (_, index) => sendMessage(
    primary.baseUrl,
    `${longPrefix}${index + 1}`,
    `${casePrefix}-long-session-${index + 1}`,
  ))
  let shortResponses
  let phaseError = null
  try {
    await waitForCondition(
      () => {
        const records = fake.inferenceRecords(longPrefix)
        return records.length === LONG_HOLDERS
          && records.every((record) => record.completedAt === null)
      },
      `${casePrefix} active long holders`,
    )
    shortResponses = await Promise.all(
      Array.from({ length: SHORT_DURING_LONG }, (_, index) => sendMessage(
        primary.baseUrl,
        `${casePrefix}-DURING-LONG-SHORT-${index + 1}`,
        `${casePrefix}-during-long-session-${index + 1}`,
      )),
    )
  } catch (error) {
    phaseError = error
  } finally {
    fake.release(longPrefix)
  }
  const longSettled = await Promise.allSettled(longPromises)
  if (phaseError) throw phaseError
  const longRejected = longSettled.find((result) => result.status === 'rejected')
  if (longRejected) throw longRejected.reason
  const longResponses = longSettled.map((result) => result.value)
  const all = [...longResponses, ...shortResponses]
  const distribution = distributionSummary(all)
  assert.equal(
    distribution.coverage,
    ACCEPTANCE_CONTRACT.longShort.expectedUnionCoverage,
    JSON.stringify(distribution),
  )
  assert.equal(distribution.max, 1, JSON.stringify(distribution))
  return {
    distribution,
    longCredentials: longResponses.map((response) => response.selectedCredential),
    shortCredentials: shortResponses.map((response) => response.selectedCredential),
    latency: {
      longTotalMs: quantiles(longResponses.map((response) => response.totalMs)),
      shortTotalMs: quantiles(shortResponses.map((response) => response.totalMs)),
    },
  }
}

async function raceWorkload(primary, secondary, fake, casePrefix) {
  const batches = []
  for (let batch = 1; batch <= RACE_BATCHES; batch += 1) {
    const batchPrefix = `${casePrefix}-RACE-${batch}-`
    fake.hold(batchPrefix)
    const pending = [
      ...Array.from({ length: RACE_REQUESTS_PER_INSTANCE }, (_, index) => sendMessage(
        primary.baseUrl,
        `${casePrefix}-RACE-${batch}-A-${index + 1}`,
        `${casePrefix}-race-${batch}-a-session-${index + 1}`,
      )),
      ...Array.from({ length: RACE_REQUESTS_PER_INSTANCE }, (_, index) => sendMessage(
        secondary.baseUrl,
        `${casePrefix}-RACE-${batch}-B-${index + 1}`,
        `${casePrefix}-race-${batch}-b-session-${index + 1}`,
      )),
    ]
    let phaseError = null
    let phaseSnapshot = null
    try {
      await waitForCondition(
        () => {
          const records = fake.inferenceRecords(batchPrefix)
          return records.length === ACCEPTANCE_CONTRACT.candidateLeaseRace.requestsPerBatch
            && records.every((record) => record.completedAt === null)
        },
        `${casePrefix} active race batch ${batch}`,
      )
    } catch (error) {
      phaseError = error
      const records = fake.inferenceRecords(batchPrefix)
      phaseSnapshot = {
        admitted: records.length,
        active: records.filter((record) => record.completedAt === null).length,
        selectedCredentials: records.map((record) => record.selectedCredential),
        externalHits: fake.externalHits(batchPrefix),
      }
    } finally {
      fake.release(batchPrefix)
    }
    const settled = await Promise.allSettled(pending)
    if (phaseError) {
      throw new Error(`${phaseError.stack || phaseError.message}; ${JSON.stringify({
        phaseSnapshot,
        responses: settled.map((result) => result.status === 'fulfilled'
          ? {
              status: result.value.status,
              selectedCredential: result.value.selectedCredential,
              errorId: result.value.errorId,
            }
          : { rejected: String(result.reason) }),
      })}`)
    }
    const rejected = settled.find((result) => result.status === 'rejected')
    if (rejected) throw rejected.reason
    const responses = settled.map((result) => result.value)
    const distribution = distributionSummary(responses)
    assert.equal(
      distribution.coverage,
      ACCEPTANCE_CONTRACT.candidateLeaseRace.uniqueCredentialsPerBatch,
      `candidate lease race oversold or failed to reselect: ${JSON.stringify(distribution)}`,
    )
    assert.equal(distribution.max, 1, JSON.stringify(distribution))
    batches.push({
      batch,
      distribution,
      latency: quantiles(responses.map((response) => response.totalMs)),
    })
  }
  return batches
}

async function healthWeightWorkload(primary, casePrefix) {
  for (let id = 51; id <= 60; id += 1) {
    await setCredentialValue(primary.baseUrl, id, 'priority', 10)
  }
  const responses = []
  for (let index = 1; index <= 40; index += 1) {
    responses.push(await sendMessage(
      primary.baseUrl,
      `${casePrefix}-HEALTH-WEIGHT-${index}`,
      `${casePrefix}-health-weight-session-${index}`,
    ))
  }
  const distribution = distributionSummary(responses)
  assert.ok(
    Object.keys(distribution.counts).every((id) => Number(id) <= 50),
    `health score selected priority-penalized credentials: ${JSON.stringify(distribution)}`,
  )
  return distribution
}

async function weightedCapacityWorkload(primary, fake, casePrefix) {
  for (let id = 21; id <= 40; id += 1) {
    await setCredentialValue(primary.baseUrl, id, 'concurrency', 2)
  }
  for (let id = 41; id <= 60; id += 1) {
    await setCredentialValue(primary.baseUrl, id, 'concurrency', 4)
  }
  const expectedConcurrency = new Map()
  for (let id = 1; id <= 20; id += 1) expectedConcurrency.set(id, 1)
  for (let id = 21; id <= 40; id += 1) expectedConcurrency.set(id, 2)
  for (let id = 41; id <= 60; id += 1) expectedConcurrency.set(id, 4)
  await waitForRuntimeConcurrency(primary.baseUrl, expectedConcurrency, casePrefix)
  const prefix = `${casePrefix}-WEIGHT-`
  fake.hold(prefix, 120_000)
  const pending = []
  let admissionError = null
  try {
    for (let index = 1; index <= WEIGHTED_REQUESTS; index += 1) {
      const before = fake.inferenceRecords(prefix).length
      pending.push(sendMessage(
        primary.baseUrl,
        `${prefix}${index}`,
        `${casePrefix}-weight-session-${index}`,
      ))
      await waitForCondition(
        () => fake.inferenceRecords(prefix).length === before + 1,
        `${casePrefix} weighted admission ${index}`,
        5_000,
      )
    }
    const admitted = fake.inferenceRecords(prefix)
    assert.equal(admitted.length, WEIGHTED_REQUESTS)
    assert.ok(
      admitted.every((record) => record.completedAt === null),
      `${casePrefix}: a weighted holder completed before all ${WEIGHTED_REQUESTS} admissions`,
    )
  } catch (error) {
    admissionError = error
  } finally {
    fake.release(prefix)
  }
  const settled = await Promise.allSettled(pending)
  if (admissionError) throw admissionError
  const rejected = settled.find((result) => result.status === 'rejected')
  if (rejected) throw rejected.reason
  const responses = settled.map((result) => result.value)
  const distribution = distributionSummary(responses)
  const groupTotals = [
    responses.filter((response) => response.selectedCredential <= 20).length,
    responses.filter((response) => response.selectedCredential >= 21
      && response.selectedCredential <= 40).length,
    responses.filter((response) => response.selectedCredential >= 41).length,
  ]
  assert.deepEqual(
    groupTotals,
    ACCEPTANCE_CONTRACT.weightedCapacity.exactExpectedTotals,
    JSON.stringify({ groupTotals, distribution, contract: ACCEPTANCE_CONTRACT.weightedCapacity }),
  )
  assert.ok(groupTotals[0] < groupTotals[1] && groupTotals[1] < groupTotals[2])
  for (const [idText, count] of Object.entries(distribution.counts)) {
    const id = Number(idText)
    const limit = id <= 20 ? 1 : id <= 40 ? 2 : 4
    assert.ok(count <= limit, `credential ${id} exceeded ${limit}: ${count}`)
  }
  return {
    groupTotals,
    distribution,
    theoreticalContract: ACCEPTANCE_CONTRACT.weightedCapacity,
    latency: quantiles(responses.map((response) => response.totalMs)),
  }
}

function summarizeSamples(samples, service) {
  const selected = samples
    .flatMap((sample) => sample.services)
    .filter((sample) => sample.name === service.name)
  return {
    rssKb: {
      start: service.resourcesStart.rssKb,
      peak: Math.max(service.resourcesStart.rssKb, ...selected.map((sample) => sample.rssKb)),
    },
    fdCount: {
      start: service.resourcesStart.fdCount,
      peak: Math.max(service.resourcesStart.fdCount, ...selected.map((sample) => sample.fdCount)),
    },
    globalInFlightPeak: Math.max(
      0,
      ...selected.map((sample) => sample.summary?.globalInFlightRequests || 0),
    ),
    queuedRequestsPeak: Math.max(
      0,
      ...selected.map((sample) => sample.summary?.queuedRequests || 0),
    ),
  }
}

async function runCase({
  mode,
  round,
  caseIndex,
  database,
  databaseUrl,
  redisUrl,
  redisPrefix,
  localPort,
  externalPort,
  fake,
  servicePorts,
}) {
  const casePrefix = `E0102-${mode.replaceAll('_', '-')}-R${round}`
  const caseRoot = path.join(TEMP_ROOT, `${mode}-${round}`)
  const auxiliaryAtCaseStart = fake.records.filter((record) => record.kind === 'auxiliary').length
  fs.mkdirSync(caseRoot, { recursive: true, mode: 0o700 })
  const beforeKeys = await scanRedisPrefix(redisPrefix)
  assert.equal(beforeKeys.length, 0, `${casePrefix}: Redis prefix is not empty before run`)
  const primary = await startCaseService({
    name: 'primary',
    mode,
    databaseUrl,
    redisUrl,
    redisPrefix,
    localPort,
    externalPort,
    caseRoot,
    servicePorts,
    poolName: `e0102-primary-${mode}-${caseIndex}`,
  })
  let secondary
  let sampler
  try {
    secondary = await startCaseService({
      name: 'secondary',
      mode,
      databaseUrl,
      redisUrl,
      redisPrefix,
      localPort,
      externalPort,
      caseRoot,
      servicePorts,
      poolName: `e0102-secondary-${mode}-${caseIndex}`,
    })
    const externalReady = await waitForExternalReady([primary, secondary])
    const auxiliaryBeforeWorkloads = fake.records
      .filter((record) => record.kind === 'auxiliary').length
    sampler = startSampler([primary, secondary])

    const short = await shortDistributionWorkload(primary, mode, casePrefix)
    const sticky = await stickyWorkload(primary, secondary, fake, casePrefix)
    const longShort = await longShortWorkload(primary, fake, casePrefix)
    const raceLogsBefore = {
      primary: diagnosticLogSummary(primary.logPath),
      secondary: diagnosticLogSummary(secondary.logPath),
    }
    const race = await raceWorkload(primary, secondary, fake, casePrefix)
    const raceLogsAfter = {
      primary: diagnosticLogSummary(primary.logPath),
      secondary: diagnosticLogSummary(secondary.logPath),
    }
    const raceReselect = (
      raceLogsAfter.primary.reselect + raceLogsAfter.secondary.reselect
      - raceLogsBefore.primary.reselect - raceLogsBefore.secondary.reselect
    )
    assert.ok(
      raceReselect >= ACCEPTANCE_CONTRACT.candidateLeaseRace.minimumObservedReselectLogs,
      `candidate-race fixture did not observe a race-local slot reselect: ${JSON.stringify({
        before: raceLogsBefore,
        after: raceLogsAfter,
        raceReselect,
      })}`,
    )
    const healthWeight = mode === 'health_balanced'
      ? await healthWeightWorkload(primary, casePrefix)
      : null
    const weightedCapacity = mode === 'weighted_least_inflight'
      ? await weightedCapacityWorkload(primary, fake, casePrefix)
      : null

    const samples = await sampler.stop()
    sampler = null
    await new Promise((resolve) => setTimeout(resolve, 500))
    const resources = {
      primary: summarizeSamples(samples, primary),
      secondary: summarizeSamples(samples, secondary),
    }
    resources.primary.rssKb.end = processResources(primary.handle.pid).rssKb
    resources.primary.fdCount.end = processResources(primary.handle.pid).fdCount
    resources.secondary.rssKb.end = processResources(secondary.handle.pid).rssKb
    resources.secondary.fdCount.end = processResources(secondary.handle.pid).fdCount

    const finalSummary = {
      primary: await fetchSummary(primary.baseUrl),
      secondary: await fetchSummary(secondary.baseUrl),
    }
    const finalRuntime = {
      primary: await fetchRuntime(primary.baseUrl),
      secondary: await fetchRuntime(secondary.baseUrl),
    }
    for (const [name, summary] of Object.entries(finalSummary)) {
      assert.equal(summary.globalInFlightRequests, 0, `${name}: ${JSON.stringify(summary)}`)
      assert.equal(summary.queuedRequests, 0, `${name}: ${JSON.stringify(summary)}`)
    }
    for (const [name, runtime] of Object.entries(finalRuntime)) {
      assert.ok(runtime.items.every((item) => item.inFlightRequests === 0),
        `${name}: ${JSON.stringify(runtime)}`)
    }
    for (const [name, value] of Object.entries(resources)) {
      assert.ok(
        value.fdCount.end <= value.fdCount.start + ACCEPTANCE_CONTRACT.resources.maxFdEndGrowth,
        `${name} FD did not recover: ${JSON.stringify(value.fdCount)}`,
      )
      assert.ok(
        value.rssKb.end <= ACCEPTANCE_CONTRACT.resources.maxFocusedRssEndKb,
        `${name} RSS exceeded focused absolute bound: ${JSON.stringify(value.rssKb)}`,
      )
      assert.ok(
        value.queuedRequestsPeak <= ACCEPTANCE_CONTRACT.longShort.maxQueueDepth,
        `${name} queue exceeded contract: ${JSON.stringify(value)}`,
      )
    }

    const logs = {
      primary: diagnosticLogSummary(primary.logPath),
      secondary: diagnosticLogSummary(secondary.logPath),
      raceReselect,
    }
    const reselectTotal = logs.primary.reselect + logs.secondary.reselect
    assert.ok(
      reselectTotal >= ACCEPTANCE_CONTRACT.candidateLeaseRace.minimumObservedReselectLogs,
      `candidate-race fixture did not observe a real slot reselect: ${JSON.stringify(logs)}`,
    )
    assert.equal(logs.primary.redisDegraded + logs.secondary.redisDegraded, 0,
      JSON.stringify(logs))
    assert.equal(logs.primary.pgSuccessSlowOver100 + logs.secondary.pgSuccessSlowOver100, 0,
      `steady success path synchronously rewrote PgSQL runtime state: ${JSON.stringify(logs)}`)
    assert.equal(fake.externalHits(casePrefix), 0,
      `${casePrefix}: local capacity existed but external pool was used`)
    const auxiliaryAfterWorkloads = fake.records
      .filter((record) => record.kind === 'auxiliary').length
    assert.equal(auxiliaryAfterWorkloads - auxiliaryBeforeWorkloads, 0,
      `${casePrefix}: profile/model auxiliary calls unexpectedly amplified`)

    const caseRecords = fake.inferenceRecords(casePrefix)
    const selectedCounts = Object.fromEntries(
      [...caseRecords.reduce((counts, record) => {
        counts.set(record.selectedCredential, (counts.get(record.selectedCredential) || 0) + 1)
        return counts
      }, new Map()).entries()].sort((a, b) => a[0] - b[0]),
    )
    return {
      caseId: casePrefix,
      mode,
      round,
      postgresDatabase: database,
      redisPrefixSha256: sha256(redisPrefix),
      accountCount: ACCOUNT_COUNT,
      servicePorts: [primary.servicePort, secondary.servicePort],
      short,
      sticky,
      longShort,
      race,
      healthWeight,
      weightedCapacity,
      hits: {
        localInference: caseRecords.length,
        startupAuxiliary: auxiliaryBeforeWorkloads - auxiliaryAtCaseStart,
        workloadAuxiliary: auxiliaryAfterWorkloads - auxiliaryBeforeWorkloads,
        external: fake.externalHits(casePrefix),
      },
      externalReady,
      selectedCounts,
      fakeGlobalPeakInFlight: fake.globalPeakInFlight,
      resources,
      finalSummary,
      logs,
    }
  } catch (error) {
    const logTails = [primary, secondary].filter(Boolean).map((service) => ({
      service: service.name,
      tail: fs.existsSync(service.logPath)
        ? fs.readFileSync(service.logPath, 'utf8').slice(-30_000)
          .replaceAll(REQUEST_KEY, '<redacted-request-key>')
          .replaceAll(ADMIN_KEY, '<redacted-admin-key>')
          .replaceAll('sk-e0102-fake-external', '<redacted-external-key>')
          .replace(/e0102-token-\d+/g, '<redacted-local-token>')
        : '<log unavailable>',
    }))
    throw new Error(`${error.stack || error.message}\nlogs:\n${JSON.stringify(logTails)}`)
  } finally {
    if (sampler) await sampler.stop().catch(() => {})
    await Promise.all([stopService(primary.handle), stopService(secondary?.handle)])
    await cleanupRedisPrefix(redisPrefix).catch(() => null)
  }
}

async function main() {
  const validated = validateInputs()
  if (VALIDATE_ONLY) {
    process.stdout.write(`${JSON.stringify({
      result: 'validate_only',
      caseId: 'E01-E02-scheduler-fairness-sticky-race',
      dockerUsed: false,
      cargoUsed: false,
      protected9022ProbeSkipped: true,
      rounds: ROUNDS,
      modes: MODES,
      requiredDatabaseCount: REQUIRED_DATABASE_COUNT,
      postgresHost: validated.postgresHost,
      postgresPort: validated.postgresPort,
      redisHost: validated.redisHost,
      redisPort: validated.redisPort,
      redisDatabase: validated.redisDatabase,
      redisPrefixSha256: sha256(REDIS_PREFIX),
      binarySha256: sha256File(BINARY),
    }, null, 2)}\n`)
    return
  }

  fs.mkdirSync(TEMP_ROOT, { recursive: true, mode: 0o700 })
  fs.mkdirSync(REPORT_ROOT, { recursive: true })
  const localPort = await reservePort()
  const externalPort = await reservePort()
  const fake = createFakeUpstreams()
  const cases = []
  const servicePorts = []
  const redisPrefixes = []
  const cleanup = {
    tempSecretsRemoved: false,
    portsReleased: false,
    protectedPortProbeSkipped: true,
    redisPrefixes: [],
  }
  let runError = null
  let finalRedisDiagnostics = null

  try {
    await redisCommand(['PING'])
    await fake.listen(localPort, externalPort)

    let caseIndex = 0
    for (const mode of MODES) {
      for (let round = 1; round <= ROUNDS; round += 1) {
        caseIndex += 1
        const database = POSTGRES_DATABASES[caseIndex - 1]
        const databaseUrl = POSTGRES_URL_TEMPLATE.replace('{database}', database)
        const redisPrefix = `${REDIS_PREFIX}:${mode}:${round}`
        redisPrefixes.push(redisPrefix)
        cases.push(await runCase({
          mode,
          round,
          caseIndex,
          database,
          databaseUrl,
          redisUrl: REDIS_URL,
          redisPrefix,
          localPort,
          externalPort,
          fake,
          servicePorts,
        }))
      }
    }
  } catch (error) {
    runError = error
    finalRedisDiagnostics = await redisDiagnostics(redisPrefixes)
  } finally {
    if (!finalRedisDiagnostics) {
      finalRedisDiagnostics = await redisDiagnostics(redisPrefixes)
    }
    await fake.close().catch(() => {})
    for (const prefix of redisPrefixes) {
      const result = await cleanupRedisPrefix(prefix).catch((error) => ({
        prefixSha256: sha256(prefix),
        error: String(error.message || error),
      }))
      cleanup.redisPrefixes.push(result)
    }
    fs.rmSync(TEMP_ROOT, { recursive: true, force: true })
    cleanup.tempSecretsRemoved = !fs.existsSync(TEMP_ROOT)
    await new Promise((resolve) => setTimeout(resolve, 250))
    const allPorts = [localPort, externalPort, ...servicePorts]
    cleanup.portsReleased = (await Promise.all(allPorts.map(isPortOpen))).every((open) => !open)
  }

  const gitRevision = run('git', ['rev-parse', 'HEAD'])
  const dirty = run('git', ['status', '--porcelain=v1'])
  const diff = run('git', ['diff', '--binary'])
  const sanitizedError = runError
    ? String(runError.stack || runError.message)
      .replaceAll(REQUEST_KEY, '<redacted-request-key>')
      .replaceAll(ADMIN_KEY, '<redacted-admin-key>')
      .replaceAll('sk-e0102-fake-external', '<redacted-external-key>')
      .replace(/e0102-token-\d+/g, '<redacted-local-token>')
    : null
  const report = {
    schemaVersion: 1,
    caseId: 'E01-E02-scheduler-fairness-sticky-race',
    runId: RUN_ID,
    generatedAt: new Date().toISOString(),
    result: runError ? 'fail' : 'pass',
    error: sanitizedError,
    acceptanceContract: ACCEPTANCE_CONTRACT,
    roundsPerMode: ROUNDS,
    modes: MODES,
    gitRevision,
    dirty: Boolean(dirty),
    dirtyDiffSha256: sha256(diff),
    runnerSha256: sha256File(import.meta.filename),
    binaryPath: path.relative(ROOT, BINARY),
    binarySha256: sha256File(BINARY),
    isolation: {
      servicePort9022Touched: false,
      protectedPortProbeSkipped: true,
      forbiddenPorts: [9022],
      postgresHost: validated.postgresHost,
      postgresPort: validated.postgresPort,
      postgresDatabaseCount: POSTGRES_DATABASES.length,
      redisHost: validated.redisHost,
      redisPort: validated.redisPort,
      redisDatabase: validated.redisDatabase,
      redisPrefixSha256: sha256(REDIS_PREFIX),
      localFakePort: localPort,
      externalFakePort: externalPort,
      dockerUsed: false,
    },
    cases,
    cleanup,
    redisDiagnostics: finalRedisDiagnostics,
  }
  fs.writeFileSync(REPORT_PATH, `${JSON.stringify(report, null, 2)}\n`)
  process.stdout.write(`${REPORT_PATH}\n`)
  if (runError) throw runError
}

main().catch((error) => {
  process.stderr.write(`${error.stack || error.message}\n`)
  process.exitCode = 1
})
