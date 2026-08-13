#!/usr/bin/env node

/*
 * E03: two real kiro.rs processes sharing one PostgreSQL authority and one
 * Redis scheduler namespace. This runner never invokes Docker or Cargo. The
 * caller must provide a frozen candidate binary plus pre-created, empty
 * caller-owned PostgreSQL databases on the already-running project's isolated
 * infrastructure.
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
const POSTGRES_URL_TEMPLATE = requiredEnvironment('KIRO_E03_POSTGRES_URL_TEMPLATE')
const REDIS_URL = requiredEnvironment('KIRO_E03_REDIS_URL')
const REDIS_PREFIX = requiredEnvironment('KIRO_E03_REDIS_PREFIX')
const OUTER_ROUNDS = boundedInteger('KIRO_E03_OUTER_ROUNDS', 1, 1, 3)
const VALIDATE_ONLY = process.env.KIRO_E03_VALIDATE_ONLY === '1'
const CONTRACT_HOLD = process.env.KIRO_E03_CONTRACT_HOLD === '1'
const POSTGRES_DATABASES = String(process.env.KIRO_E03_POSTGRES_DATABASES || '')
  .split(',')
  .map((value) => value.trim())
  .filter(Boolean)
const READY_FILE = optionalReadyFile()
const LEASE_MAX_SECS = 3
const RUN_ID = `e03-${new Date().toISOString().replace(/[-:.TZ]/g, '')}-${process.pid}-${crypto.randomBytes(3).toString('hex')}`
const TEMP_ROOT = fs.mkdtempSync(path.join(os.tmpdir(), `${RUN_ID}-`))
const REPORT_ROOT = path.join(ARTIFACT_ROOT, 'reports', 'e03-real-two-process-scheduler')
const REPORT_PATH = path.join(REPORT_ROOT, `${RUN_ID}.json`)
const ACTIVE_CHILDREN = new Set()
const ACTIVE_SERVERS = new Set()
const OWNED_PORTS = new Set()
let cleanupPromise = null
let redisTarget = null
let fake = null
let services = []
let signalHandling = false

function optionalReadyFile() {
  const value = String(process.env.KIRO_E03_READY_FILE || '').trim()
  if (!value) return null
  if (!path.isAbsolute(value)) throw new Error('KIRO_E03_READY_FILE must be an absolute path')
  const parent = path.dirname(value)
  if (!fs.existsSync(parent)) throw new Error('KIRO_E03_READY_FILE parent must exist')
  const parentReal = fs.realpathSync(parent)
  if (parentReal === ROOT || parentReal.startsWith(`${ROOT}${path.sep}`)) {
    throw new Error('KIRO_E03_READY_FILE must be outside the repository')
  }
  if (fs.existsSync(value)) throw new Error('KIRO_E03_READY_FILE must not already exist')
  return value
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
  for (const secret of [POSTGRES_URL_TEMPLATE, REDIS_URL]) {
    if (secret) text = text.split(secret).join('<redacted>')
  }
  text = text.replace(/e03-token-[12]/g, '<redacted-local-token>')
  text = text.replace(/sk-e03-[A-Za-z0-9_-]+/g, '<redacted-external-key>')
  if (text.length <= 4000) return text
  return `${text.slice(0, 2000)}\n<diagnostic_truncated chars=${text.length}>\n${text.slice(-2000)}`
}

function validateInputs() {
  if (!POSTGRES_URL_TEMPLATE.includes('{database}')) {
    throw new Error('KIRO_E03_POSTGRES_URL_TEMPLATE must contain the literal {database} placeholder')
  }
  if ((POSTGRES_URL_TEMPLATE.match(/\{database\}/g) || []).length !== 1) {
    throw new Error('KIRO_E03_POSTGRES_URL_TEMPLATE must contain exactly one {database} placeholder')
  }
  const postgres = new URL(POSTGRES_URL_TEMPLATE.replace('{database}', 'kiro_e03_validation'))
  if (!['postgres:', 'postgresql:'].includes(postgres.protocol)) {
    throw new Error('KIRO_E03_POSTGRES_URL_TEMPLATE must use PostgreSQL')
  }
  if (!['127.0.0.1', 'localhost', '::1'].includes(postgres.hostname)) {
    throw new Error('KIRO_E03_POSTGRES_URL_TEMPLATE must target loopback')
  }
  if (Number(postgres.port || 5432) === 9022) throw new Error('port 9022 is protected')
  if (!VALIDATE_ONLY) {
    if (POSTGRES_DATABASES.length !== OUTER_ROUNDS) {
      throw new Error(`KIRO_E03_POSTGRES_DATABASES must contain exactly ${OUTER_ROUNDS} pre-created database names`)
    }
    for (const database of POSTGRES_DATABASES) {
      if (!/^kiro_e03_[a-z0-9_]{3,80}$/.test(database)) {
        throw new Error('KIRO_E03_POSTGRES_DATABASES must contain caller-owned kiro_e03_* names')
      }
    }
  }
  const redis = new URL(REDIS_URL)
  if (redis.protocol !== 'redis:') throw new Error('KIRO_E03_REDIS_URL must use redis://')
  if (!['127.0.0.1', 'localhost', '::1'].includes(redis.hostname)) {
    throw new Error('KIRO_E03_REDIS_URL must target loopback')
  }
  if (redis.search || redis.hash) throw new Error('KIRO_E03_REDIS_URL must not contain query or fragment data')
  const dbText = redis.pathname.replace(/^\//, '')
  if (!/^\d+$/.test(dbText)) throw new Error('KIRO_E03_REDIS_URL must name a Redis database')
  const database = Number(dbText)
  if (!Number.isSafeInteger(database) || database < 1 || database > 15) {
    throw new Error('KIRO_E03_REDIS_URL must use an isolated nonzero database in 1..15')
  }
  const port = Number(redis.port || 6379)
  if (port === 9022) throw new Error('port 9022 is protected')
  if (REDIS_PREFIX.includes('kiro_rs:local')) {
    throw new Error('KIRO_E03_REDIS_PREFIX must be a caller-owned temporary prefix')
  }
  if (!/^[a-z0-9][a-z0-9:._-]{7,95}$/.test(REDIS_PREFIX)) {
    throw new Error('KIRO_E03_REDIS_PREFIX has an invalid format')
  }
  return { postgres, redis, database, redisPort: port }
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
  throw new Error(`unsupported Redis response type ${type}`)
}

async function redisCommand(commandParts) {
  const commands = []
  if (redisTarget.redis.username || redisTarget.redis.password) {
    if (redisTarget.redis.username) {
      commands.push(['AUTH', decodeURIComponent(redisTarget.redis.username), decodeURIComponent(redisTarget.redis.password || '')])
    } else {
      commands.push(['AUTH', decodeURIComponent(redisTarget.redis.password || '')])
    }
  }
  commands.push(['SELECT', String(redisTarget.database)], commandParts)
  const payload = encodeRedisCommands(commands)
  return new Promise((resolve, reject) => {
    const socket = net.connect({ host: redisTarget.redis.hostname, port: redisTarget.redisPort })
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
          if (reply.type === '-') {
            finish(new Error(`Redis command failed: ${reply.value}`))
            return
          }
          replies.push(reply.value)
          cursor = reply.next
          if (replies.length === commands.length) {
            finish(null, replies.at(-1))
            return
          }
        }
      } catch (error) {
        finish(error)
      }
    })
    socket.once('timeout', () => finish(new Error('Redis command timed out')))
    socket.once('error', (error) => finish(error))
  })
}

async function redisPrefixKeys() {
  let cursor = '0'
  const keys = []
  do {
    const values = await redisCommand(['SCAN', cursor, 'MATCH', `${REDIS_PREFIX}:*`, 'COUNT', '1000'])
    cursor = String(values?.[0] || '0')
    if (Array.isArray(values?.[1])) keys.push(...values[1])
  } while (cursor !== '0')
  return [...new Set(keys)].sort()
}

async function deleteRedisPrefix() {
  let cursor = '0'
  let removed = 0
  do {
    const values = await redisCommand(['SCAN', cursor, 'MATCH', `${REDIS_PREFIX}:*`, 'COUNT', '1000'])
    cursor = String(values?.[0] || '0')
    const keys = Array.isArray(values?.[1]) ? values[1] : []
    if (keys.length) removed += Number(await redisCommand(['UNLINK', ...keys]) || 0)
  } while (cursor !== '0')
  return removed
}

async function waitFor(predicate, description, timeoutMs = 30_000, intervalMs = 50) {
  const deadline = Date.now() + timeoutMs
  let lastError = null
  while (Date.now() < deadline) {
    try {
      const value = await predicate()
      if (value) return value
    } catch (error) {
      lastError = error
    }
    await new Promise((resolve) => setTimeout(resolve, intervalMs))
  }
  throw new Error(`timeout waiting for ${description}${lastError ? `: ${lastError.message}` : ''}`)
}

function traceStep(label, details = {}) {
  if (process.env.KIRO_E03_TRACE_STEPS !== '1') return
  process.stderr.write(`${JSON.stringify({
    trace: 'e03',
    at: new Date().toISOString(),
    label,
    ...details,
  })}\n`)
}

async function reservePort() {
  for (;;) {
    const selected = await new Promise((resolve, reject) => {
      const server = net.createServer()
      server.unref()
      server.once('error', reject)
      server.listen(0, '127.0.0.1', () => {
        const address = server.address()
        const port = typeof address === 'object' && address ? address.port : 0
        server.close((error) => (error ? reject(error) : resolve(port)))
      })
    })
    if (selected !== 9022) {
      OWNED_PORTS.add(selected)
      return selected
    }
  }
}

function portAcceptsConnections(port) {
  return new Promise((resolve) => {
    const socket = net.connect({ host: '127.0.0.1', port })
    let settled = false
    const finish = (value) => {
      if (settled) return
      settled = true
      socket.destroy()
      resolve(value)
    }
    socket.setTimeout(250)
    socket.once('connect', () => finish(true))
    socket.once('timeout', () => finish(false))
    socket.once('error', () => finish(false))
  })
}

function eventFrame(eventType, payload) {
  // Use the existing AWS event-stream framing contract without exposing the
  // fixture's raw protocol outside the owned test process.
  const encodedHeaders = encodeEventHeaders(eventType)
  const body = Buffer.from(JSON.stringify(payload))
  const totalLength = 12 + encodedHeaders.length + body.length + 4
  const frame = Buffer.alloc(totalLength)
  frame.writeUInt32BE(totalLength, 0)
  frame.writeUInt32BE(encodedHeaders.length, 4)
  frame.writeUInt32BE(crc32(frame.subarray(0, 8)), 8)
  encodedHeaders.copy(frame, 12)
  body.copy(frame, 12 + encodedHeaders.length)
  frame.writeUInt32BE(crc32(frame.subarray(0, totalLength - 4)), totalLength - 4)
  return frame
}

function encodeEventHeaders(eventType) {
  const values = [
    [':message-type', 'event'],
    [':event-type', eventType],
    [':content-type', 'application/json'],
  ]
  const parts = []
  for (const [name, value] of values) {
    const nameBytes = Buffer.from(name)
    const valueBytes = Buffer.from(value)
    const length = Buffer.alloc(2)
    length.writeUInt16BE(valueBytes.length)
    parts.push(Buffer.from([nameBytes.length]), nameBytes, Buffer.from([7]), length, valueBytes)
  }
  return Buffer.concat(parts)
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

function readBody(request) {
  return new Promise((resolve, reject) => {
    const chunks = []
    request.on('data', (chunk) => chunks.push(chunk))
    request.on('end', () => resolve(Buffer.concat(chunks).toString('utf8')))
    request.on('error', reject)
  })
}

function writeJson(response, status, body) {
  const bytes = Buffer.from(JSON.stringify(body))
  response.writeHead(status, {
    'content-type': 'application/json',
    'content-length': bytes.length,
    connection: 'close',
  })
  response.end(bytes)
}

function modelDiscoveryResponse() {
  return {
    models: [
      { modelId: 'claude-sonnet-4', modelName: 'E03 Sonnet' },
      { modelId: 'claude-haiku-4.5', modelName: 'E03 Haiku' },
    ].map((model) => ({
      ...model,
      supportedInputTypes: ['TEXT'],
      tokenLimits: { maxInputTokens: 1000000, maxOutputTokens: 64000 },
      additionalModelRequestFieldsSchema: {
        type: 'object',
        properties: {
          output_config: {
            type: 'object',
            properties: {
              effort: {
                type: 'string',
                enum: ['low', 'medium', 'high', 'max'],
                default: 'high',
              },
            },
          },
        },
      },
    })),
    nextToken: null,
  }
}

function createFakeUpstreams() {
  const localRecords = []
  const externalRecords = []
  const clientErrors = []
  const holds = new Map()
  const recordClientError = (server, error) => {
    const rawPrefix = error?.rawPacket
      ? error.rawPacket
        .subarray(0, 2048)
        .toString('latin1')
        .replace(/[^\x09\x0a\x0d\x20-\x7e]/g, '?')
      : null
    clientErrors.push({
      server,
      code: error?.code || null,
      reason: error?.reason || null,
      message: error?.message || null,
      bytesParsed: Number.isSafeInteger(error?.bytesParsed) ? error.bytesParsed : null,
      rawPrefix: rawPrefix ? redact(rawPrefix) : null,
    })
  }
  const local = http.createServer(async (request, response) => {
    const raw = await readBody(request)
    const marker = raw.match(/E03-[A-Za-z0-9_-]+/)?.[0] || 'E03-unknown'
    const target = String(request.headers['x-amz-target'] || '')
    const url = new URL(request.url || '/', 'http://127.0.0.1')
    if (target.endsWith('.ListAvailableModels') || url.pathname.endsWith('/ListAvailableModels')) {
      writeJson(response, 200, modelDiscoveryResponse())
      return
    }
    const token = String(request.headers.authorization || '').match(/e03-token-(\d+)/)?.[1] || '0'
    const record = { marker, token, startedAt: Date.now(), completedAt: null }
    localRecords.push(record)
    if (!['1', '2'].includes(token)) {
      writeJson(response, 401, { message: 'isolated fixture credential rejected' })
      return
    }
    const held = holds.get(marker)
    response.writeHead(200, {
      'content-type': 'application/vnd.amazon.eventstream',
      connection: 'close',
    })
    if (held?.stream) {
      response.write(eventFrame('assistantResponseEvent', {
        content: `renew-open ${marker}`,
        messageStatus: 'IN_PROGRESS',
      }))
      held.response = response
      held.timer = setInterval(() => {
        if (!response.destroyed) {
          response.write(eventFrame('assistantResponseEvent', {
            content: '.',
            messageStatus: 'IN_PROGRESS',
          }))
        }
      }, 450)
      held.release = () => {
        clearInterval(held.timer)
        if (response.destroyed) return
        response.write(eventFrame('assistantResponseEvent', {
          content: `done ${marker}`,
          messageStatus: 'COMPLETED',
        }))
        response.end(eventFrame('metadataEvent', {
          tokenUsage: { uncachedInputTokens: 8, cacheReadInputTokens: 0, cacheWriteInputTokens: 0, outputTokens: 4, totalTokens: 12 },
        }))
        record.completedAt = Date.now()
      }
      response.on('close', () => {
        clearInterval(held.timer)
        if (!record.completedAt) record.closedAt = Date.now()
        holds.delete(marker)
      })
      return
    }
    response.end(Buffer.concat([
      eventFrame('assistantResponseEvent', { content: `local-${token} ${marker}`, messageStatus: 'COMPLETED' }),
      eventFrame('metadataEvent', { tokenUsage: { uncachedInputTokens: 8, cacheReadInputTokens: 0, cacheWriteInputTokens: 0, outputTokens: 4, totalTokens: 12 } }),
    ]))
    record.completedAt = Date.now()
  })
  const external = http.createServer(async (request, response) => {
    const raw = await readBody(request)
    const marker = raw.match(/E03-[A-Za-z0-9_-]+/)?.[0] || 'E03-unknown'
    externalRecords.push({ marker, at: Date.now() })
    writeJson(response, 200, {
      id: `e03-external-${externalRecords.length}`,
      type: 'message', role: 'assistant', model: 'claude-sonnet-4',
      content: [{ type: 'text', text: `external ${marker}` }],
      stop_reason: 'end_turn', stop_sequence: null, usage: { input_tokens: 8, output_tokens: 2 },
    })
  })
  local.on('clientError', (error, socket) => {
    recordClientError('local', error)
    socket.end('HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n')
  })
  external.on('clientError', (error, socket) => {
    recordClientError('external', error)
    socket.end('HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n')
  })
  return {
    localRecords,
    externalRecords,
    clientErrors,
    holds,
    async listen(localPort, externalPort) {
      await Promise.all([
        new Promise((resolve, reject) => { local.once('error', reject); local.listen(localPort, '127.0.0.1', resolve) }),
        new Promise((resolve, reject) => { external.once('error', reject); external.listen(externalPort, '127.0.0.1', resolve) }),
      ])
      ACTIVE_SERVERS.add(local)
      ACTIVE_SERVERS.add(external)
    },
    async close() {
      for (const hold of holds.values()) hold.release?.()
      await Promise.all([...ACTIVE_SERVERS].map((server) => new Promise((resolve) => {
        if (!server.listening) return resolve()
        server.close(() => resolve())
      })))
      ACTIVE_SERVERS.clear()
    },
    hold(marker) {
      const item = { stream: true, release: null }
      holds.set(marker, item)
      return item
    },
    release(marker) {
      const item = holds.get(marker)
      if (!item) return
      item.release?.()
      holds.delete(marker)
    },
    localHits(marker) { return localRecords.filter((item) => item.marker === marker).length },
    externalHits() { return externalRecords.length },
  }
}

function deterministicSession(seed) {
  const value = sha256(seed)
  return `${value.slice(0, 8)}-${value.slice(8, 12)}-4${value.slice(13, 16)}-8${value.slice(17, 20)}-${value.slice(20, 32)}`
}

function credentialsFixture() {
  return [
    {
      id: 1, accessToken: 'e03-token-1', machineId: deterministicSession('e03-machine-1'), expiresAt: '2099-01-01T00:00:00Z',
      authMethod: 'social', endpoint: 'ide', profileArn: 'arn:aws:codewhisperer:us-east-1:123456789012:profile/E03_ONE',
      maxConcurrentRequests: 1, rpm: 0, supportedModels: ['claude-sonnet-4'], disabled: false,
    },
    {
      id: 2, accessToken: 'e03-token-2', machineId: deterministicSession('e03-machine-2'), expiresAt: '2099-01-01T00:00:00Z',
      authMethod: 'social', endpoint: 'ide', profileArn: 'arn:aws:codewhisperer:us-east-1:123456789012:profile/E03_TWO',
      maxConcurrentRequests: 8, rpm: 2, supportedModels: ['claude-haiku-4.5'], disabled: false,
    },
  ]
}

function serviceConfig({ databaseUrl, redisUrl, port, localPort }) {
  return {
    postgres: { url: databaseUrl, maxConnections: 8, migrateOnStart: true },
    redis: { url: redisUrl, keyPrefix: REDIS_PREFIX },
    host: '127.0.0.1', port,
    apiKey: 'sk-e03-request', adminApiKey: 'sk-e03-admin',
    defaultEndpoint: 'ide',
    kiroUpstreamBaseUrl: `http://127.0.0.1:${localPort}/kiro`,
    kiroUpstreamResponseTimeoutSecs: 30,
    kiroUpstreamStreamIdleTimeoutSecs: 15,
    kiroUpstreamStreamRetryEnabled: false,
    credentialRetryMaxAttempts: 0,
    inferenceUpstreamMaxAttempts: 1,
    credentialWarmupRequests: 0,
    credentialRpm: 0,
    credentialMaxConcurrentRequests: 1,
    credentialInFlightLeaseMaxSecs: LEASE_MAX_SECS,
    credentialDispatchMaxWaitSecs: 5,
    dispatchGlobalMaxConcurrentRequests: 0,
    dispatchMaxQueuedRequests: 0,
    loadBalancingMode: 'balanced', schedulerTopK: 1,
    externalPools: {
      externalPoolsEnabled: true,
      fallbackOnLocalCapacityExhausted: false,
      fallbackOnSchedulerRedisDegraded: true,
      fallbackOnNoAvailableCredentials: true,
      fallbackOnLocalTransientExhausted: false,
      fallbackOnUnsupportedModel: false,
      externalPoolAutoDisableEnabled: false,
      externalPoolRequestTimeoutSecs: 10,
      externalPoolCapacityMode: 'fail_fast',
      externalPoolLocalRescueEnabled: false,
    },
  }
}

async function startService(configPath, credentialsPath, logPath, port) {
  const logFd = fs.openSync(logPath, 'a')
  const child = spawn(BINARY, ['--config', configPath, '--credentials', credentialsPath], {
    cwd: ROOT,
    env: validationChildEnvironment({
      KIRO_API_KEY: '', KIRO_RS_HOST: '127.0.0.1', KIRO_RS_PORT: String(port),
      RUST_LOG: 'kiro_rs::kiro::token_manager=debug,kiro_rs::anthropic::handlers=info,kiro_rs=info',
    }),
    stdio: ['ignore', logFd, logFd], detached: true,
  })
  ACTIVE_CHILDREN.add(child)
  child.once('exit', () => { try { fs.closeSync(logFd) } catch {} ACTIVE_CHILDREN.delete(child) })
  const service = { child, port, baseUrl: `http://127.0.0.1:${port}`, logPath, configPath, credentialsPath }
  try {
    await waitFor(async () => {
      if (child.exitCode !== null) throw new Error(`service exited ${child.exitCode}`)
      try { return (await fetch(`${service.baseUrl}/healthz`)).ok } catch { return false }
    }, `service ${port} health`, 60_000, 100)
  } catch (error) {
    const tail = fs.existsSync(logPath) ? redact(fs.readFileSync(logPath, 'utf8').slice(-20_000)) : ''
    throw new Error(`${error.message}\n${tail}`)
  }
  service.resourcesStart = processResources(child.pid)
  services.push(service)
  return service
}

async function stopService(service, signal = 'SIGTERM') {
  if (!service?.child || service.child.exitCode !== null || service.child.signalCode !== null) return
  try { process.kill(-service.child.pid, signal) } catch { service.child.kill(signal) }
  await waitFor(() => service.child.exitCode !== null || service.child.signalCode !== null, `service ${service.port} stop`, 10_000, 50)
  ACTIVE_CHILDREN.delete(service.child)
}

function processResources(pid) {
  const ps = spawnSync('ps', ['-o', 'rss=', '-p', String(pid)], { encoding: 'utf8' })
  const rssKb = Number.parseInt(String(ps.stdout || '').trim(), 10) || 0
  const lsof = spawnSync('lsof', ['-nP', '-p', String(pid)], { encoding: 'utf8' })
  const fdCount = String(lsof.stdout || '').trim().split('\n').filter(Boolean).length
  return { rssKb, fdCount }
}

function requestBody(marker, model = 'claude-sonnet-4', stream = false) {
  return JSON.stringify({
    model, max_tokens: 64, stream,
    metadata: { user_id: JSON.stringify({ session_id: deterministicSession(marker) }) },
    messages: [{ role: 'user', content: marker }],
  })
}

async function request(service, marker, model = 'claude-sonnet-4', stream = false, signal) {
  const started = performance.now()
  const response = await fetch(`${service.baseUrl}/v1/messages`, {
    method: 'POST', headers: { 'x-api-key': 'sk-e03-request', 'content-type': 'application/json', connection: 'close' },
    body: requestBody(marker, model, stream), signal,
  })
  const text = await response.text()
  return {
    status: response.status, text,
    requestId: response.headers.get('request-id') || response.headers.get('x-request-id'),
    retryAfter: response.headers.get('retry-after'),
    totalMs: Number((performance.now() - started).toFixed(2)),
  }
}

function startRequestProbe(service, marker, model = 'claude-sonnet-4', stream = false) {
  const controller = new AbortController()
  const probe = {
    marker,
    controller,
    settled: false,
    outcome: null,
    promise: null,
  }
  probe.promise = request(service, marker, model, stream, controller.signal).then(
    (response) => {
      probe.settled = true
      probe.outcome = { response }
      return response
    },
    (error) => {
      probe.settled = true
      probe.outcome = { error }
      throw error
    },
  )
  return probe
}

function requestProbeSummary(probe) {
  if (!probe.settled) return { marker: probe.marker, state: 'pending' }
  if (probe.outcome?.response) {
    const { status, retryAfter, totalMs, text } = probe.outcome.response
    return { marker: probe.marker, state: 'response', status, retryAfter, totalMs, text: redact(text) }
  }
  return {
    marker: probe.marker,
    state: 'error',
    error: redact(probe.outcome?.error?.stack || probe.outcome?.error?.message || 'unknown error'),
  }
}

async function assertProbePendingFor(probe, holdMs, description) {
  await new Promise((resolve) => setTimeout(resolve, holdMs))
  assert.equal(probe.settled, false, `${description} completed before capacity release: ${JSON.stringify(requestProbeSummary(probe))}`)
}

async function waitForProbe(probe, description, timeoutMs = 30_000) {
  let timeout = null
  try {
    return await Promise.race([
      probe.promise,
      new Promise((_, reject) => {
        timeout = setTimeout(() => reject(new Error(`timeout waiting for ${description}`)), timeoutMs)
      }),
    ])
  } catch (error) {
    probe.controller.abort()
    throw error
  } finally {
    if (timeout) clearTimeout(timeout)
  }
}

async function openHeldStream(service, marker) {
  const controller = new AbortController()
  let responseOutcome = null
  const responseTask = (async () => {
    const response = await fetch(`${service.baseUrl}/v1/messages`, {
      method: 'POST', headers: { 'x-api-key': 'sk-e03-request', 'content-type': 'application/json', connection: 'close' },
      body: requestBody(marker, 'claude-sonnet-4', true), signal: controller.signal,
    })
    if (response.status !== 200) {
      throw new Error(`${marker} stream status ${response.status}: ${redact(await response.text())}`)
    }
    assert.ok(response.body)
    return response
  })().then(
    (response) => {
      responseOutcome = { response }
      return responseOutcome
    },
    (error) => {
      responseOutcome = { error }
      return responseOutcome
    },
  )
  await waitFor(() => {
    if (fake.localHits(marker) >= 1) return true
    if (responseOutcome?.error) throw responseOutcome.error
    return false
  }, `${marker} reaches local fake upstream`, 10_000, 50)
  return {
    controller,
    async drain() {
      const outcome = await responseTask
      if (outcome.error) {
        if (outcome.error?.name === 'AbortError') return
        throw outcome.error
      }
      const reader = outcome.response.body.getReader()
      try { for (;;) { const next = await reader.read(); if (next.done) break } } catch {}
    },
  }
}

function adminHeaders() {
  return { authorization: 'Bearer sk-e03-admin', 'content-type': 'application/json', connection: 'close' }
}

async function adminJson(service, method, suffix, body) {
  const response = await fetch(`${service.baseUrl}${suffix}`, {
    method, headers: adminHeaders(), body: body === undefined ? undefined : JSON.stringify(body),
  })
  const text = await response.text()
  if (!response.ok) throw new Error(`admin ${method} ${suffix} -> ${response.status}: ${redact(text)}`)
  return text ? JSON.parse(text) : {}
}

async function installExternalPool(service, externalPort) {
  await adminJson(service, 'POST', '/api/admin/external-pools', {
    name: 'e03-isolated-external', baseUrl: `http://127.0.0.1:${externalPort}/external`,
    apiKey: 'sk-e03-external', authType: 'bearer', enabled: true, priority: 0,
    maxConcurrentRequests: 32, usageProjectionMode: 'pass_through', requestBodyMode: 'normalized',
    rawModelMode: 'none', preservePath: true, supportedModels: ['claude-sonnet-4', 'claude-haiku-4.5'],
  })
  await waitFor(async () => {
    return externalPoolStatus(service)
  }, 'isolated external pool dispatchable', 20_000, 100)
}

async function externalPoolStatus(service) {
  const status = await adminJson(service, 'GET', '/api/admin/external-pools/status')
  return status.pools?.some((pool) => pool.dispatchable) ? status : null
}

async function credentialsSummary(service) {
  return adminJson(service, 'GET', '/api/admin/credentials/summary')
}

async function redisLeaseSnapshot(credentialId = 1) {
  const key = `${REDIS_PREFIX}:scheduler:inflight:${credentialId}:last_seen`
  const values = await redisCommand(['ZRANGE', key, '0', '-1', 'WITHSCORES'])
  const leases = []
  for (let index = 0; index < values.length; index += 2) {
    leases.push({ id: values[index], lastSeenMs: Number(values[index + 1]) })
  }
  return leases
}

async function runRound(round) {
  const database = POSTGRES_DATABASES[round - 1]
  assert.ok(database, `missing caller-owned PostgreSQL database for round ${round}`)
  const localPort = await reservePort()
  const externalPort = await reservePort()
  await fake.listen(localPort, externalPort)
  const dbUrl = POSTGRES_URL_TEMPLATE.replace('{database}', database)
  const redisUrl = REDIS_URL
  const roundRoot = path.join(TEMP_ROOT, `round-${round}`)
  fs.mkdirSync(roundRoot, { recursive: true, mode: 0o700 })
  const configPaths = []
  const credentialPath = path.join(roundRoot, 'credentials.json')
  fs.writeFileSync(credentialPath, `${JSON.stringify(credentialsFixture(), null, 2)}\n`, { mode: 0o600 })
  const start = async (name, port) => {
    const configPath = path.join(roundRoot, `${name}.config.json`)
    const logPath = path.join(roundRoot, `${name}.log`)
    fs.writeFileSync(configPath, `${JSON.stringify(serviceConfig({ databaseUrl: dbUrl, redisUrl, port, localPort }), null, 2)}\n`, { mode: 0o600 })
    configPaths.push(configPath)
    return startService(configPath, credentialPath, logPath, port)
  }
  const serviceA = await start('a', await reservePort())
  const serviceB = await start('b', await reservePort())
  await installExternalPool(serviceA, externalPort)
  await waitFor(() => externalPoolStatus(serviceB), 'second service external pool propagation', 20_000, 100)
  await waitFor(() => credentialsSummary(serviceB).then((summary) => summary.total >= 2 ? summary : null), 'second service credential import', 20_000, 100)

  const baselineExternal = fake.externalHits()
  const sanityA = await request(serviceA, `E03-R${round}-SANITY-A`)
  const sanityB = await request(serviceB, `E03-R${round}-SANITY-B`)
  assert.equal(sanityA.status, 200, redact(sanityA.text))
  assert.equal(sanityB.status, 200, redact(sanityB.text))
  assert.equal(fake.externalHits(), baselineExternal, 'healthy local traffic was misrouted to external pool')

  const renewMarker = `E03-R${round}-RENEW-HOLDER`
  fake.hold(renewMarker)
  traceStep('renew.open.start', { round })
  const renewStream = await openHeldStream(serviceA, renewMarker)
  traceStep('renew.open.done', { round, localHits: fake.localHits(renewMarker) })
  traceStep('renew.before.snapshot.start', { round })
  const beforeRenew = await redisLeaseSnapshot(1)
  traceStep('renew.before.snapshot.done', { round, leases: beforeRenew.length })
  assert.equal(beforeRenew.length, 1)
  await new Promise((resolve) => setTimeout(resolve, (LEASE_MAX_SECS * 1000) + 1_500))
  traceStep('renew.after.snapshot.start', { round })
  const afterRenew = await redisLeaseSnapshot(1)
  traceStep('renew.after.snapshot.done', { round, leases: afterRenew.length })
  assert.equal(afterRenew.length, 1, 'renewed lease disappeared while stream was alive')
  assert.ok(afterRenew[0].lastSeenMs > beforeRenew[0].lastSeenMs + 1_000, JSON.stringify({ beforeRenew, afterRenew }))
  traceStep('renew.blocked.request.start', { round })
  const blockedMarker = `E03-R${round}-RENEW-BLOCKED`
  const blockedLocalHitsBefore = fake.localHits(blockedMarker)
  const blockedProbe = startRequestProbe(serviceB, blockedMarker)
  await assertProbePendingFor(blockedProbe, 1_250, `round ${round} shared-capacity request`)
  traceStep('renew.blocked.request.pending', { round, localHits: fake.localHits(blockedMarker), externalHits: fake.externalHits() })
  assert.equal(fake.localHits(blockedMarker), blockedLocalHitsBefore, 'shared-capacity waiter reached local upstream while holder lease was alive')
  assert.equal(fake.externalHits(), baselineExternal, 'shared-capacity waiter was incorrectly sent to external pool')
  traceStep('renew.release.start', { round })
  fake.release(renewMarker)
  traceStep('renew.drain.start', { round })
  await renewStream.drain()
  traceStep('renew.drain.done', { round })
  traceStep('renew.blocked.request.await_recovery.start', { round })
  const blockedAfterRelease = await waitForProbe(blockedProbe, `round ${round} shared-capacity waiter recovery`, 20_000)
  traceStep('renew.blocked.request.done', { round, status: blockedAfterRelease.status })
  assert.equal(blockedAfterRelease.status, 200, redact(blockedAfterRelease.text))
  assert.equal(fake.localHits(blockedMarker), blockedLocalHitsBefore + 1, 'shared-capacity waiter did not resume on local upstream after holder release')
  assert.equal(fake.externalHits(), baselineExternal, 'shared-capacity waiter recovery was incorrectly sent to external pool')
  traceStep('renew.release.snapshot.start', { round })
  await waitFor(async () => (await redisLeaseSnapshot(1)).length === 0, 'renewed lease release', 10_000, 100)
  traceStep('renew.release.snapshot.done', { round })
  traceStep('renew.recovery.request.start', { round })
  const afterRelease = await request(serviceB, `E03-R${round}-RELEASE-RECOVERY`)
  traceStep('renew.recovery.request.done', { round, status: afterRelease.status })
  assert.equal(afterRelease.status, 200, redact(afterRelease.text))

  const crashMarker = `E03-R${round}-CRASH-HOLDER`
  fake.hold(crashMarker)
  const crashStream = await openHeldStream(serviceA, crashMarker)
  const crashLeases = await redisLeaseSnapshot(1)
  assert.equal(crashLeases.length, 1)
  await stopService(serviceA, 'SIGKILL')
  crashStream.controller.abort()
  await crashStream.drain()
  const postKillMarker = `E03-R${round}-POST-KILL-IMMEDIATE`
  const postKillLocalHitsBefore = fake.localHits(postKillMarker)
  const postKillProbe = startRequestProbe(serviceB, postKillMarker)
  await assertProbePendingFor(postKillProbe, 1_250, `round ${round} post-SIGKILL stale-lease request`)
  assert.equal(fake.localHits(postKillMarker), postKillLocalHitsBefore, 'post-SIGKILL waiter reached local upstream before stale lease TTL')
  assert.equal(fake.externalHits(), baselineExternal, 'fresh SIGKILL lease was classified as external-fallback capacity')
  const staleLeaseRecovery = await waitForProbe(postKillProbe, `round ${round} stale lease TTL recovery`, 20_000)
  assert.equal(staleLeaseRecovery.status, 200, redact(staleLeaseRecovery.text))
  assert.equal(fake.localHits(postKillMarker), postKillLocalHitsBefore + 1, 'post-SIGKILL waiter did not recover on local upstream after stale lease TTL')
  assert.equal(fake.externalHits(), baselineExternal, 'post-SIGKILL recovery was sent to external pool')
  const afterTtl = await request(serviceB, `E03-R${round}-POST-KILL-TTL-RECOVERY`)
  assert.equal(afterTtl.status, 200, redact(afterTtl.text))
  await waitFor(async () => (await redisLeaseSnapshot(1)).length === 0, 'stale crash lease cleanup', 10_000, 100)
  const summaryAfterKill = await credentialsSummary(serviceB)
  assert.equal(summaryAfterKill.disabled, 0, JSON.stringify(summaryAfterKill))
  assert.equal(fake.externalHits(), baselineExternal, 'post-TTL local recovery was sent to external pool')

  services = services.filter((service) => service !== serviceA)
  const serviceARestarted = await start('a-restarted', await reservePort())
  const restartResponse = await request(serviceARestarted, `E03-R${round}-RESTART-LOCAL`)
  assert.equal(restartResponse.status, 200, redact(restartResponse.text))
  assert.equal(fake.externalHits(), baselineExternal)

  const rpm1 = await request(serviceARestarted, `E03-R${round}-RPM-1`, 'claude-haiku-4.5')
  const rpm2 = await request(serviceB, `E03-R${round}-RPM-2`, 'claude-haiku-4.5')
  assert.equal(rpm1.status, 200, redact(rpm1.text))
  assert.equal(rpm2.status, 200, redact(rpm2.text))
  const rpmKey = `${REDIS_PREFIX}:scheduler:rate_limit:2`
  assert.ok(await redisCommand(['PTTL', rpmKey]) > 0, 'shared RPM deadline was not persisted in Redis')
  const rpm3 = await request(serviceARestarted, `E03-R${round}-RPM-3`, 'claude-haiku-4.5')
  assert.notEqual(rpm3.status, 200, redact(rpm3.text))
  await stopService(serviceB)
  services = services.filter((service) => service !== serviceB)
  const serviceBRestarted = await start('b-restarted', await reservePort())
  const rpm4 = await request(serviceBRestarted, `E03-R${round}-RPM-4`, 'claude-haiku-4.5')
  assert.notEqual(rpm4.status, 200, redact(rpm4.text))
  assert.equal(fake.externalHits(), baselineExternal, 'RPM saturation was incorrectly sent to external pool')

  const finalSummary = await credentialsSummary(serviceBRestarted)
  assert.equal(finalSummary.disabled, 0, JSON.stringify(finalSummary))
  const finalLeases = await redisLeaseSnapshot(1)
  assert.equal(finalLeases.length, 0)
  const resources = [...new Set([serviceARestarted, serviceBRestarted])].map((service) => ({
    name: service === serviceARestarted ? 'a' : 'b',
    start: service.resourcesStart,
    end: processResources(service.child.pid),
  }))
  return {
    round, localPort, externalPort, database,
    renew: {
      beforeLastSeenMs: beforeRenew[0].lastSeenMs,
      afterLastSeenMs: afterRenew[0].lastSeenMs,
      blockedPendingMs: 1_250,
      blockedStatusAfterRelease: blockedAfterRelease.status,
    },
    crash: {
      immediatePendingMs: 1_250,
      staleLeaseRecoveryStatus: staleLeaseRecovery.status,
      ttlRecoveryStatus: afterTtl.status,
    },
    rpm: { firstStatuses: [rpm1.status, rpm2.status], postRestartStatuses: [rpm3.status, rpm4.status] },
    externalHits: fake.externalHits() - baselineExternal,
    disabled: finalSummary.disabled,
    resources,
  }
}

async function cleanup() {
  if (cleanupPromise) return cleanupPromise
  cleanupPromise = (async () => {
    for (const service of [...services].reverse()) await stopService(service).catch(() => {})
    services = []
    await fake?.close().catch(() => {})
    const redisRemoved = VALIDATE_ONLY ? 0 : await deleteRedisPrefix().catch(() => -1)
    fs.rmSync(TEMP_ROOT, { recursive: true, force: true })
    if (READY_FILE) fs.rmSync(READY_FILE, { force: true })
    const occupiedPorts = []
    for (const port of OWNED_PORTS) {
      if (await portAcceptsConnections(port)) occupiedPorts.push(port)
    }
    return {
      childGroupsStopped: ACTIVE_CHILDREN.size === 0,
      serversStopped: ACTIVE_SERVERS.size === 0,
      redisRemoved,
      redisPrefixKeysRemaining: VALIDATE_ONLY
        ? []
        : await redisPrefixKeys().catch(() => ['unavailable']),
      databasePreserved: true,
      occupiedPorts,
      tempRemoved: !fs.existsSync(TEMP_ROOT),
    }
  })()
  return cleanupPromise
}

function assertCleanupComplete(cleaned) {
  assert.equal(cleaned.childGroupsStopped, true, JSON.stringify(cleaned))
  assert.equal(cleaned.serversStopped, true, JSON.stringify(cleaned))
  assert.deepEqual(cleaned.redisPrefixKeysRemaining, [], JSON.stringify(cleaned))
  assert.equal(cleaned.databasePreserved, true, JSON.stringify(cleaned))
  assert.deepEqual(cleaned.occupiedPorts, [], JSON.stringify(cleaned))
  assert.equal(cleaned.tempRemoved, true, JSON.stringify(cleaned))
}

function writeFailureDiagnostics(error) {
  try {
    fs.mkdirSync(REPORT_ROOT, { recursive: true, mode: 0o700 })
    const serviceLogs = services.map((service) => ({
      port: service.port,
      exited: service.child.exitCode !== null || service.child.signalCode !== null,
      exitCode: service.child.exitCode,
      signalCode: service.child.signalCode,
      logTail: fs.existsSync(service.logPath)
        ? redact(fs.readFileSync(service.logPath, 'utf8').slice(-20_000))
        : '<missing>',
    }))
    const diagnosticPath = path.join(REPORT_ROOT, `${RUN_ID}.failure.json`)
    fs.writeFileSync(diagnosticPath, `${JSON.stringify({
      result: 'failure-diagnostic',
      runId: RUN_ID,
      error: redact(error.stack || error.message),
      fakeUpstream: {
        localRecords: fake?.localRecords ?? [],
        externalRecords: fake?.externalRecords ?? [],
        clientErrors: fake?.clientErrors ?? [],
      },
      services: serviceLogs,
    }, null, 2)}\n`, { mode: 0o600 })
    return diagnosticPath
  } catch (diagnosticError) {
    return `<diagnostic-write-failed ${redact(diagnosticError.message)}>`
  }
}

for (const [signal, code] of [['SIGHUP', 129], ['SIGINT', 130], ['SIGTERM', 143]]) {
  process.on(signal, () => {
    if (signalHandling) return
    signalHandling = true
    void cleanup().finally(() => { process.exitCode = code })
  })
}

async function main() {
  redisTarget = validateInputs()
  if (VALIDATE_ONLY) {
    if (READY_FILE) {
      fs.writeFileSync(READY_FILE, `${JSON.stringify({ ready: true, pid: process.pid, tempRoot: TEMP_ROOT })}\n`, {
        encoding: 'utf8', flag: 'wx', mode: 0o600,
      })
    }
    if (CONTRACT_HOLD) {
      while (!signalHandling) await new Promise((resolve) => setTimeout(resolve, 20))
      return
    }
    const cleaned = await cleanup()
    assertCleanupComplete(cleaned)
    process.stdout.write(`${JSON.stringify({ result: 'validated', protected9022ProbeSkipped: true, cleanup: cleaned })}\n`)
    return
  }
  fake = createFakeUpstreams()
  const beforeKeys = await redisPrefixKeys()
  assert.equal(beforeKeys.length, 0, `Redis prefix is not empty before E03: ${beforeKeys.length}`)
  const results = []
  for (let round = 1; round <= OUTER_ROUNDS; round += 1) {
    results.push(await runRound(round))
    const roundCleanup = await cleanup()
    assertCleanupComplete(roundCleanup)
    cleanupPromise = null
    fake = createFakeUpstreams()
  }
  const finalCleanup = await cleanup()
  assertCleanupComplete(finalCleanup)
  fs.mkdirSync(REPORT_ROOT, { recursive: true, mode: 0o700 })
  fs.writeFileSync(REPORT_PATH, `${JSON.stringify({
    result: 'pass', runId: RUN_ID, rounds: results, binarySha256: sha256(fs.readFileSync(BINARY)), cleanup: finalCleanup,
  }, null, 2)}\n`, { mode: 0o600 })
  process.stdout.write(`${JSON.stringify({ result: 'pass', runId: RUN_ID, outerRounds: OUTER_ROUNDS, reportPath: REPORT_PATH, rounds: results, cleanup: finalCleanup }, null, 2)}\n`)
}

main().catch(async (error) => {
  const failureDiagnosticPath = writeFailureDiagnostics(error)
  const cleanupResult = await cleanup().catch((cleanupError) => ({ error: redact(cleanupError.message) }))
  if (!signalHandling) {
    process.stderr.write(`E03 real two-process scheduler failed: ${redact(error.stack || error.message)}; failureDiagnosticPath=${failureDiagnosticPath}; cleanup=${JSON.stringify(cleanupResult)}\n`)
    process.exitCode = 1
  }
})
