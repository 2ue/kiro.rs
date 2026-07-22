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

const ROOT = fs.realpathSync(path.resolve(import.meta.dirname, '../..'))
const ROUNDS = Number.parseInt(process.env.KIRO_F06_ROUNDS || '3', 10)
const { binary: BINARY, artifactRoot: ARTIFACT_ROOT } = resolveRuntimeValidationPaths(ROOT)
const POSTGRES_URL = requiredEnvironment('KIRO_F06_POSTGRES_URL')
const REDIS_URL = requiredEnvironment('KIRO_F06_REDIS_URL')
const REDIS_PREFIX = requiredEnvironment('KIRO_F06_REDIS_PREFIX')
const VALIDATE_ONLY = process.env.KIRO_F06_VALIDATE_ONLY === '1'
const ADMIN_KEY = 'sk-admin-f06-isolated-validation'
const REQUEST_KEY = 'sk-request-f06-isolated-validation'
const REGION_MATRIX = ['us-east-1', 'eu-central-1', 'f06-future-region-9']
const REPORT_FORBIDDEN_VALUES = new Set([
  POSTGRES_URL,
  REDIS_URL,
  REDIS_PREFIX,
  ADMIN_KEY,
  REQUEST_KEY,
  'stale-refresh-must-clear',
  'stale-access-must-clear',
  'stale-client-must-clear',
  'stale-secret-must-clear',
])
const MALFORMED_CREDENTIAL_VARIANTS = [
  { id: 'empty_key', plainFile: true, fields: () => ({ kiroApiKey: '|us-east-1' }) },
  { id: 'multiple_pipe', plainFile: true, fields: (secret) => ({ kiroApiKey: `${secret}|us-east-1|extra` }) },
  { id: 'region_whitespace', plainFile: true, fields: (secret) => ({ kiroApiKey: `${secret}|us east-1` }) },
  { id: 'region_control', plainFile: true, fields: (secret) => ({ kiroApiKey: `${secret}|us-east-1\nextra` }) },
  { id: 'region_host_unsafe', plainFile: true, fields: (secret) => ({ kiroApiKey: `${secret}|us-east-1.example` }) },
  { id: 'explicit_region_whitespace', plainFile: false, fields: (secret) => ({ kiroApiKey: secret, region: 'us east-1' }) },
  { id: 'explicit_auth_region_control', plainFile: false, fields: (secret) => ({ kiroApiKey: secret, authRegion: 'us-east-1\nextra' }) },
  { id: 'explicit_api_region_host_unsafe', plainFile: false, fields: (secret) => ({ kiroApiKey: secret, apiRegion: 'us-east-1.example' }) },
]
const RUN_ID = `f06-${new Date().toISOString().replace(/[-:.TZ]/g, '')}-${process.pid}-${crypto.randomBytes(3).toString('hex')}`
const TEMP_ROOT = path.join(ARTIFACT_ROOT, 'runtime', 'f06', RUN_ID)
const REPORT_ROOT = path.join(ARTIFACT_ROOT, 'reports', 'f06')
const REPORT_PATH = path.join(REPORT_ROOT, `${RUN_ID}.json`)
const ACTIVE_SERVICES = new Set()
let redisTarget = null
let PSQL = ''

if (!Number.isInteger(ROUNDS) || ROUNDS < 3 || ROUNDS > 10) {
  throw new Error('KIRO_F06_ROUNDS must be an integer between 3 and 10')
}

function requiredEnvironment(name) {
  const value = String(process.env[name] || '').trim()
  if (!value) throw new Error(`${name} is required`)
  return value
}

function resolveExecutable(command) {
  if (path.isAbsolute(command)) {
    fs.accessSync(command, fs.constants.X_OK)
    return fs.realpathSync(command)
  }
  const candidates = [
    process.env.VOLTA_HOME ? path.join(process.env.VOLTA_HOME, 'bin', command) : null,
    path.join(os.homedir(), '.volta', 'bin', command),
    ...String(process.env.PATH || '').split(path.delimiter).filter(Boolean).map((dir) => path.join(dir, command)),
  ].filter(Boolean)
  for (const candidate of candidates) {
    try {
      fs.accessSync(candidate, fs.constants.X_OK)
      return fs.realpathSync(candidate)
    } catch {}
  }
  throw new Error(`unable to resolve executable: ${command}`)
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
  const postgres = new URL(POSTGRES_URL)
  if (!['postgres:', 'postgresql:'].includes(postgres.protocol)) {
    throw new Error('KIRO_F06_POSTGRES_URL must use PostgreSQL')
  }
  if (!['127.0.0.1', 'localhost', '::1'].includes(postgres.hostname)) {
    throw new Error('KIRO_F06_POSTGRES_URL must target loopback')
  }
  if (Number(postgres.port || 5432) === 9022) throw new Error('port 9022 is protected')
  if (postgres.hash) throw new Error('KIRO_F06_POSTGRES_URL must not contain a fragment')
  for (const key of postgres.searchParams.keys()) {
    if (key !== 'sslmode') throw new Error(`KIRO_F06_POSTGRES_URL contains unsupported query parameter ${key}`)
  }
  const postgresDatabase = decodeURIComponent(postgres.pathname.replace(/^\//, ''))
  if (!/^kiro_f06_[a-z0-9_]{3,80}$/.test(postgresDatabase)) {
    throw new Error('KIRO_F06_POSTGRES_URL must name a caller-owned kiro_f06_* database')
  }

  const redis = new URL(REDIS_URL)
  if (redis.protocol !== 'redis:') throw new Error('KIRO_F06_REDIS_URL must use redis://')
  if (redis.username || redis.password) throw new Error('KIRO_F06_REDIS_URL must not contain Redis auth material')
  if (!['127.0.0.1', 'localhost', '::1'].includes(redis.hostname)) {
    throw new Error('KIRO_F06_REDIS_URL must target loopback')
  }
  if (Number(redis.port || 6379) === 9022) throw new Error('port 9022 is protected')
  if (redis.search || redis.hash) throw new Error('KIRO_F06_REDIS_URL must not contain query or fragment data')
  const dbText = redis.pathname.replace(/^\//, '')
  if (!/^\d+$/.test(dbText)) throw new Error('KIRO_F06_REDIS_URL must name a Redis database')
  const redisDatabase = Number(dbText)
  if (!Number.isSafeInteger(redisDatabase) || redisDatabase < 1 || redisDatabase > 15) {
    throw new Error('KIRO_F06_REDIS_URL must use an isolated nonzero database in 1..15')
  }
  if (REDIS_PREFIX.includes('kiro_rs:local')) {
    throw new Error('KIRO_F06_REDIS_PREFIX must be a caller-owned temporary prefix')
  }
  if (!/^[a-z0-9][a-z0-9:._-]{7,95}$/.test(REDIS_PREFIX)) {
    throw new Error('KIRO_F06_REDIS_PREFIX has an invalid format')
  }
  redisTarget = {
    redis,
    redisPort: Number(redis.port || 6379),
    redisDatabase,
    postgresDatabase,
  }
  return redisTarget
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
    env: minimalEnvironment(),
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

function writeJson(response, status, value) {
  const body = Buffer.from(JSON.stringify(value))
  response.writeHead(status, {
    'content-type': 'application/json',
    'content-length': body.length,
    connection: 'close',
  })
  response.end(body)
}

function classifyUpstreamRequest(request, url) {
  const target = String(request.headers['x-amz-target'] || '')
  if (target.endsWith('.ListAvailableModels')) return 'model_discovery'
  if (target.endsWith('.GenerateAssistantResponse')) return 'inference'
  if (url.pathname.endsWith('/getUsageLimits')) return 'balance'
  if (url.pathname.endsWith('/setUserPreference')) return 'overage'
  return 'unknown'
}

function expectedHost(kind, region) {
  if (kind === 'model_discovery') return `management.${region}.kiro.dev`
  if (kind === 'inference') return `runtime.${region}.kiro.dev`
  return `q.${region}.amazonaws.com`
}

function createFakeUpstream() {
  const records = []
  const expectedKeys = new Map()
  let activeCase = null
  const server = http.createServer((request, response) => {
    const url = new URL(request.url || '/', 'http://127.0.0.1')
    const kind = classifyUpstreamRequest(request, url)
    const authorization = String(request.headers.authorization || '')
    const bearer = authorization.startsWith('Bearer ') ? authorization.slice(7) : ''
    const keyInfo = expectedKeys.get(bearer)
    const keyDigest = bearer ? sha256(bearer).slice(0, 16) : null
    const record = {
      caseId: activeCase,
      kind,
      method: request.method,
      path: `${url.pathname}${url.search}`,
      logicalHost: String(request.headers.host || ''),
      authorizationScheme: authorization.split(' ', 1)[0] || null,
      keyDigest,
      tokenType: String(request.headers.tokentype || ''),
      target: String(request.headers['x-amz-target'] || ''),
      valid: false,
    }
    records.push(record)

    if (!keyInfo || keyInfo.caseId !== activeCase) {
      writeJson(response, 401, { message: 'unknown fake credential' })
      return
    }
    const hostValid = record.logicalHost === expectedHost(kind, keyInfo.region)
    record.valid = kind !== 'unknown'
      && hostValid
      && record.authorizationScheme === 'Bearer'
      && record.tokenType === 'API_KEY'
    if (!record.valid) {
      writeJson(response, 400, { message: 'fake upstream protocol assertion failed' })
      return
    }

    if (kind === 'model_discovery') {
      writeJson(response, 200, {
        models: [{
          modelId: 'claude-sonnet-4',
          modelName: 'F06 Sonnet',
          supportedInputTypes: ['text'],
          tokenLimits: { maxInputTokens: 200000, maxOutputTokens: 8192 },
        }],
        nextToken: null,
      })
      return
    }
    if (kind === 'balance') {
      writeJson(response, 200, {
        subscriptionInfo: {
          subscriptionTitle: 'KIRO F06 TEST',
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
    if (kind === 'inference') {
      const body = Buffer.concat([
        eventFrame('assistantResponseEvent', {
          content: `f06 scheduler selected ${keyInfo.digest}`,
          messageStatus: 'COMPLETED',
        }),
        eventFrame('metadataEvent', {
          tokenUsage: {
            uncachedInputTokens: 8,
            cacheReadInputTokens: 0,
            cacheWriteInputTokens: 0,
            outputTokens: 6,
            totalTokens: 14,
          },
        }),
      ])
      response.writeHead(200, {
        'content-type': 'application/vnd.amazon.eventstream',
        'content-length': body.length,
        connection: 'close',
      })
      response.end(body)
      return
    }
    writeJson(response, 404, { message: 'unsupported fake route' })
  })

  return {
    records,
    expectedKeys,
    setActiveCase(caseId) {
      activeCase = caseId
    },
    async listen(port) {
      await new Promise((resolve, reject) => {
        server.once('error', reject)
        server.listen(port, '127.0.0.1', resolve)
      })
    },
    async close() {
      await new Promise((resolve) => server.close(resolve))
    },
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
    headers: Object.fromEntries(response.headers.entries()),
    text,
    headerMs: headersAt - started,
    totalMs: ended - started,
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

async function waitForRejectedStartup(baseUrl, processHandle, timeoutMs = 30_000) {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    if (processHandle.exitCode !== null) {
      return { rejected: true, exitCode: processHandle.exitCode, becameHealthy: false }
    }
    try {
      const response = await fetch(`${baseUrl}/healthz`)
      if (response.ok) {
        return { rejected: false, exitCode: null, becameHealthy: true }
      }
    } catch {}
    await new Promise((resolve) => setTimeout(resolve, 100))
  }
  return { rejected: false, exitCode: processHandle.exitCode, becameHealthy: false }
}

function processResources(pid) {
  const ps = spawnSync('ps', ['-o', 'rss=', '-p', String(pid)], { encoding: 'utf8' })
  const rssKb = Number.parseInt(String(ps.stdout || '').trim(), 10) || 0
  const lsof = spawnSync('lsof', ['-nP', '-p', String(pid)], { encoding: 'utf8' })
  const fdCount = String(lsof.stdout || '').trim().split('\n').filter(Boolean).length
  return { rssKb, fdCount }
}

async function stopService(handle) {
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
  ACTIVE_SERVICES.delete(handle)
}

function startService({ configPath, credentialsPath, logPath, port }) {
  const log = fs.openSync(logPath, 'a')
  const handle = spawn(BINARY, ['--config', configPath, '--credentials', credentialsPath], {
    cwd: ROOT,
    env: minimalEnvironment({
      RUST_LOG: 'info',
      KIRO_API_KEY: '',
      KIRO_RS_HOST: '127.0.0.1',
      KIRO_RS_PORT: String(port),
    }),
    stdio: ['ignore', log, log],
  })
  ACTIVE_SERVICES.add(handle)
  handle.once('exit', () => {
    ACTIVE_SERVICES.delete(handle)
    fs.closeSync(log)
  })
  return handle
}

function isolatedServiceConfig({ port, upstreamPort, redisKeyPrefix }) {
  return {
    postgres: {
      url: POSTGRES_URL,
      maxConnections: 4,
      migrateOnStart: true,
    },
    redis: {
      url: REDIS_URL,
      keyPrefix: redisKeyPrefix,
    },
    host: '127.0.0.1',
    port,
    apiKey: REQUEST_KEY,
    adminApiKey: ADMIN_KEY,
    defaultEndpoint: 'ide',
    kiroUpstreamBaseUrl: `http://127.0.0.1:${upstreamPort}/f06`,
    kiroUpstreamResponseTimeoutSecs: 5,
    credentialRetryMaxAttempts: 1,
    inferenceUpstreamMaxAttempts: 1,
    credentialWarmupRequests: 0,
  }
}

function postgresCommandEnvironment(applicationName) {
  const parsed = new URL(POSTGRES_URL)
  const environment = minimalEnvironment({
    PGHOST: parsed.hostname.replace(/^\[|\]$/g, ''),
    PGPORT: parsed.port || '5432',
    PGDATABASE: decodeURIComponent(parsed.pathname.replace(/^\//, '')),
    PGUSER: decodeURIComponent(parsed.username || process.env.USER || ''),
    PGCONNECT_TIMEOUT: '5',
    PGAPPNAME: applicationName,
  })
  if (parsed.password) environment.PGPASSWORD = decodeURIComponent(parsed.password)
  const sslMode = parsed.searchParams.get('sslmode')
  if (sslMode) environment.PGSSLMODE = sslMode
  return environment
}

function psql(sql, applicationName = 'kiro_f06_validation') {
  const result = spawnSync(PSQL, ['-X', '-A', '-t', '-v', 'ON_ERROR_STOP=1', '-c', sql], {
    cwd: TEMP_ROOT,
    encoding: 'utf8',
    maxBuffer: 8 * 1024 * 1024,
    env: postgresCommandEnvironment(applicationName),
  })
  if (result.status !== 0) {
    const stderr = String(result.stderr || '').trim().slice(0, 2000)
    throw new Error(`psql query failed (${result.status}): ${stderr}`)
  }
  return String(result.stdout || '').trim()
}

function postgresTableExists(tableName) {
  const raw = psql(`SELECT to_regclass('public.${tableName}') IS NOT NULL;`, 'kiro_f06_table_probe')
  return raw === 't'
}

function assertInitialDatabaseOwnedAndSafe() {
  const tableCountText = psql(`
    SELECT COUNT(*)
    FROM information_schema.tables
    WHERE table_schema NOT IN ('pg_catalog', 'information_schema');
  `, 'kiro_f06_initial_database_probe')
  const tableCount = Number.parseInt(tableCountText, 10)
  if (!Number.isInteger(tableCount) || tableCount < 0) {
    throw new Error('initial PostgreSQL table count was invalid')
  }
  if (!postgresTableExists('credentials')) return { tableCount, activeCredentials: 0 }
  const activeCredentials = Number.parseInt(psql(`
    SELECT COUNT(*) FROM credentials WHERE deleted_at IS NULL;
  `, 'kiro_f06_initial_credential_probe'), 10)
  if (!Number.isInteger(activeCredentials) || activeCredentials < 0) {
    throw new Error('initial active credential count was invalid')
  }
  if (activeCredentials !== 0) {
    throw new Error('KIRO_F06_POSTGRES_URL database must not contain active credentials before validation')
  }
  return { tableCount, activeCredentials }
}

function emptyCredentialState() {
  return {
    activeRows: 0,
    deletedRows: 0,
    authKind: null,
    apiKeyHashPresent: false,
    apiKeyPresent: false,
    apiKeyContainsPipe: false,
    authMethod: null,
    region: null,
    authRegion: null,
    apiRegion: null,
    endpoint: null,
    oauthFieldsCleared: false,
  }
}

function databaseCredentialState() {
  if (!postgresTableExists('credentials')) return emptyCredentialState()
  const raw = psql(`
    SELECT json_build_object(
      'activeRows', COUNT(*) FILTER (WHERE deleted_at IS NULL),
      'deletedRows', COUNT(*) FILTER (WHERE deleted_at IS NOT NULL),
      'authKind', MIN(auth_kind) FILTER (WHERE deleted_at IS NULL),
      'apiKeyHashPresent', COALESCE(bool_and(api_key_hash IS NOT NULL) FILTER (WHERE deleted_at IS NULL), false),
      'apiKeyPresent', COALESCE(bool_and(COALESCE(data->>'kiroApiKey', data->>'kiro_api_key') IS NOT NULL) FILTER (WHERE deleted_at IS NULL), false),
      'apiKeyContainsPipe', COALESCE(bool_or(COALESCE(data->>'kiroApiKey', data->>'kiro_api_key') LIKE '%|%') FILTER (WHERE deleted_at IS NULL), false),
      'authMethod', MIN(COALESCE(data->>'authMethod', data->>'auth_method')) FILTER (WHERE deleted_at IS NULL),
      'region', MIN(data->>'region') FILTER (WHERE deleted_at IS NULL),
      'authRegion', MIN(COALESCE(data->>'authRegion', data->>'auth_region')) FILTER (WHERE deleted_at IS NULL),
      'apiRegion', MIN(COALESCE(data->>'apiRegion', data->>'api_region')) FILTER (WHERE deleted_at IS NULL),
      'endpoint', MIN(data->>'endpoint') FILTER (WHERE deleted_at IS NULL),
      'oauthFieldsCleared', COALESCE(bool_and(
        data->>'refreshToken' IS NULL AND data->>'refresh_token' IS NULL
        AND data->>'accessToken' IS NULL AND data->>'access_token' IS NULL
        AND data->>'clientId' IS NULL AND data->>'client_id' IS NULL
        AND data->>'clientSecret' IS NULL AND data->>'client_secret' IS NULL
        AND data->>'profileArn' IS NULL AND data->>'profile_arn' IS NULL
      ) FILTER (WHERE deleted_at IS NULL), false)
    )::text
    FROM credentials;
  `)
  return JSON.parse(raw)
}

function auditState() {
  if (!postgresTableExists('admin_audit_logs')) {
    return { rows: 0, exportRows: 0, secretLikeRows: 0 }
  }
  const raw = psql(`
    SELECT json_build_object(
      'rows', COUNT(*),
      'exportRows', COUNT(*) FILTER (WHERE action = 'export_credentials'),
      'secretLikeRows', COUNT(*) FILTER (
        WHERE detail::text LIKE '%ksk_%'
           OR COALESCE(error_message, '') LIKE '%ksk_%'
      )
    )::text
    FROM admin_audit_logs;
  `)
  return JSON.parse(raw)
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

function countKinds(records) {
  const counts = { model_discovery: 0, balance: 0, inference: 0, overage: 0, unknown: 0 }
  for (const record of records) counts[record.kind] = (counts[record.kind] || 0) + 1
  return counts
}

function percentile(values, percentileValue) {
  if (values.length === 0) return null
  const sorted = [...values].sort((a, b) => a - b)
  const index = Math.min(sorted.length - 1, Math.ceil((percentileValue / 100) * sorted.length) - 1)
  return Number(sorted[index].toFixed(2))
}

function latencySummary(values) {
  return {
    count: values.length,
    p50: percentile(values, 50),
    p95: percentile(values, 95),
    p99: percentile(values, 99),
  }
}

function cleanupSucceeded(cleanup) {
  return cleanup.ownedRedisRemoved === true
    && cleanup.ownedRedisRemaining === 0
    && cleanup.tempSecretsRemoved === true
    && cleanup.portsReleased === true
}

function adminHeaders() {
  return {
    authorization: `Bearer ${ADMIN_KEY}`,
    'content-type': 'application/json',
  }
}

function requestHeaders() {
  return {
    'x-api-key': REQUEST_KEY,
    'content-type': 'application/json',
  }
}

function malformedCredentialFields(variant, entry, round) {
  const secret = `ksk_f06_invalid_${entry}_${variant.id}_${round}_${crypto.randomBytes(8).toString('hex')}`
  const fields = variant.fields(secret)
  REPORT_FORBIDDEN_VALUES.add(secret)
  for (const value of Object.values(fields)) {
    if (typeof value === 'string') REPORT_FORBIDDEN_VALUES.add(value)
  }
  return fields
}

async function runMalformedCredentialFileCases({ mode, upstreamPort, fake }) {
  const results = []
  const variants = MALFORMED_CREDENTIAL_VARIANTS.filter((variant) => mode !== 'plain_file' || variant.plainFile)
  for (let round = 1; round <= 3; round += 1) {
    for (const variant of variants) {
      const caseId = `${mode}_invalid_${variant.id}_${round}`
      const fields = malformedCredentialFields(variant, mode, round)
      fake.setActiveCase(caseId)
      const caseRoot = path.join(TEMP_ROOT, caseId)
      fs.mkdirSync(caseRoot, { recursive: true })
      const credentialsPath = path.join(caseRoot, mode === 'json_file' ? 'credentials.json' : 'credentials.txt')
      const configPath = path.join(caseRoot, 'config.json')
      const logPath = path.join(caseRoot, 'service.log')
      const port = await reservePort()
      assert.notEqual(port, 9022)
      const baseUrl = `http://127.0.0.1:${port}`
      const redisKeyPrefix = `${REDIS_PREFIX}:${caseId}`
      const config = isolatedServiceConfig({
        port,
        upstreamPort,
        redisKeyPrefix,
      })
      fs.writeFileSync(configPath, `${JSON.stringify(config, null, 2)}\n`, { mode: 0o600 })
      if (mode === 'json_file') {
        fs.writeFileSync(credentialsPath, `${JSON.stringify([{
          authMethod: 'api_key',
          ...fields,
        }], null, 2)}\n`, { mode: 0o600 })
      } else {
        fs.writeFileSync(credentialsPath, `# malformed isolated fixture\n${fields.kiroApiKey}\n`, { mode: 0o600 })
      }

      const before = fake.records.length
      const service = startService({ configPath, credentialsPath, logPath, port })
      const outcome = await waitForRejectedStartup(baseUrl, service)
      if (outcome.becameHealthy || service.exitCode === null) await stopService(service)
      assert.equal(outcome.rejected, true, `${caseId} was not rejected before startup`)
      assert.notEqual(outcome.exitCode, 0, `${caseId} exited successfully`)
      const auxiliaryHits = countKinds(fake.records.slice(before))
      assert.deepEqual(auxiliaryHits, {
        model_discovery: 0,
        balance: 0,
        inference: 0,
        overage: 0,
        unknown: 0,
      })
      const dbState = databaseCredentialState()
      assert.equal(dbState.activeRows, 0)
      const log = fs.readFileSync(logPath, 'utf8')
      for (const value of Object.values(fields)) {
        assert.equal(log.includes(value), false, `${caseId} log exposed malformed credential input`)
      }
      if (mode === 'json_file') {
        assert.ok(
          log.includes('invalid API-key import fields') || log.includes('invalid kiroApiKey pipe format'),
          `${caseId} lacked a classified error`,
        )
      }
      await cleanupOwnedRedisKeys()
      fs.rmSync(caseRoot, { recursive: true, force: true })
      results.push({ caseId, variant: variant.id, round, rejected: true, auxiliaryHits, activeRows: 0 })
    }
  }
  return results
}

async function runCase({ mode, round, upstreamPort, fake, latencies, statuses }) {
  const caseId = `${mode}-${round}`
  const region = REGION_MATRIX[(round - 1) % REGION_MATRIX.length]
  const secret = `ksk_f06_${mode}_${round}_${crypto.randomBytes(12).toString('hex')}`
  const digest = sha256(secret).slice(0, 16)
  REPORT_FORBIDDEN_VALUES.add(secret)
  fake.expectedKeys.set(secret, { caseId, region, digest })
  fake.setActiveCase(caseId)

  const caseRoot = path.join(TEMP_ROOT, caseId)
  fs.mkdirSync(caseRoot, { recursive: true })
  const credentialsPath = path.join(caseRoot, 'credentials.input')
  const configPath = path.join(caseRoot, 'config.json')
  const logPath = path.join(caseRoot, 'service.log')
  const port = await reservePort()
  assert.notEqual(port, 9022)
  const baseUrl = `http://127.0.0.1:${port}`
  const redisKeyPrefix = `${REDIS_PREFIX}:${caseId}`
  await cleanupOwnedRedisKeys()
  const config = isolatedServiceConfig({
    port,
    upstreamPort,
    redisKeyPrefix,
  })
  fs.writeFileSync(configPath, `${JSON.stringify(config, null, 2)}\n`, { mode: 0o600 })

  if (mode === 'admin_api') {
    fs.writeFileSync(credentialsPath, '', { mode: 0o600 })
  } else if (mode === 'json_file') {
    fs.writeFileSync(credentialsPath, `${JSON.stringify([{
      authMethod: 'API KEY',
      kiroApiKey: ` ${secret}|${region} `,
      refreshToken: 'stale-refresh-must-clear',
      accessToken: 'stale-access-must-clear',
      clientId: 'stale-client-must-clear',
      clientSecret: 'stale-secret-must-clear',
      profileArn: `arn:aws:codewhisperer:wrong-region:123:profile/STALE`,
    }], null, 2)}\n`, { mode: 0o600 })
  } else {
    fs.writeFileSync(credentialsPath, `# isolated fake key\n ${secret}|${region} \n`, { mode: 0o600 })
  }

  const recordStart = fake.records.length
  let service = startService({ configPath, credentialsPath, logPath, port })
  await waitForHealth(baseUrl, service)
  const resourcesStart = processResources(service.pid)

  let credentialId
  let importCounts
  const malformedAdminApi = []
  if (mode === 'admin_api') {
    for (const variant of MALFORMED_CREDENTIAL_VARIANTS) {
      const fields = malformedCredentialFields(variant, 'admin_api', round)
      const before = fake.records.length
      const rejected = await timedRequest(`${baseUrl}/api/admin/credentials`, {
        method: 'POST',
        headers: adminHeaders(),
        body: JSON.stringify({ authMethod: 'api_key', ...fields }),
      })
      statuses.push(rejected.status)
      assert.equal(rejected.status, 400, rejected.text)
      for (const value of Object.values(fields)) {
        assert.equal(rejected.text.includes(value), false, 'malformed API-key field was reflected in response')
      }
      const auxiliaryHits = countKinds(fake.records.slice(before))
      assert.deepEqual(auxiliaryHits, {
        model_discovery: 0,
        balance: 0,
        inference: 0,
        overage: 0,
        unknown: 0,
      })
      assert.equal(databaseCredentialState().activeRows, 0)
      malformedAdminApi.push({
        variant: variant.id,
        round,
        status: rejected.status,
        auxiliaryHits,
        activeRows: 0,
      })
    }
    const before = fake.records.length
    const add = await timedRequest(`${baseUrl}/api/admin/credentials`, {
      method: 'POST',
      headers: adminHeaders(),
      body: JSON.stringify({
        authMethod: 'API KEY',
        kiroApiKey: ` ${secret}|${region} `,
        refreshToken: 'stale-refresh-must-clear',
        accessToken: 'stale-access-must-clear',
        clientId: 'stale-client-must-clear',
        clientSecret: 'stale-secret-must-clear',
        profileArn: 'arn:aws:codewhisperer:wrong-region:123:profile/STALE',
      }),
    })
    statuses.push(add.status)
    latencies.headers.push(add.headerMs)
    latencies.total.push(add.totalMs)
    assert.equal(add.status, 200, add.text)
    credentialId = JSON.parse(add.text).credentialId
    importCounts = countKinds(fake.records.slice(before))
    assert.equal(importCounts.model_discovery, 1)
    assert.equal(importCounts.balance, 1)
  } else {
    const list = await timedRequest(`${baseUrl}/api/admin/credentials`, { headers: adminHeaders() })
    statuses.push(list.status)
    assert.equal(list.status, 200, list.text)
    const parsed = JSON.parse(list.text)
    credentialId = parsed.credentials?.[0]?.id
    importCounts = countKinds(fake.records.slice(recordStart))
    assert.ok(importCounts.model_discovery <= 1, `unexpected bootstrap model hits: ${JSON.stringify(importCounts)}`)
    assert.equal(importCounts.balance, 0)
  }
  assert.ok(Number.isInteger(credentialId) && credentialId > 0)

  const dbState = databaseCredentialState()
  assert.equal(dbState.activeRows, 1)
  assert.equal(dbState.authKind, 'api_key')
  assert.equal(dbState.apiKeyHashPresent, true)
  assert.equal(dbState.apiKeyPresent, true)
  assert.equal(dbState.apiKeyContainsPipe, false)
  assert.equal(dbState.authMethod, 'api_key')
  assert.equal(dbState.region, region)
  assert.equal(dbState.authRegion, region)
  assert.equal(dbState.apiRegion, region)
  assert.equal(dbState.endpoint, 'cli')
  assert.equal(dbState.oauthFieldsCleared, true)

  const list = await timedRequest(`${baseUrl}/api/admin/credentials`, { headers: adminHeaders() })
  statuses.push(list.status)
  assert.equal(list.status, 200, list.text)
  assert.equal(list.text.includes(secret), false, 'ordinary credential list exposed full API key')
  const listed = JSON.parse(list.text).credentials.find((item) => item.id === credentialId)
  assert.ok(listed)
  assert.equal(listed.authMethod, 'api_key')
  assert.equal(listed.apiRegion, region)
  assert.ok(typeof listed.maskedApiKey === 'string' && listed.maskedApiKey !== secret)

  const inferenceBefore = fake.records.length
  const inference = await timedRequest(`${baseUrl}/v1/messages`, {
    method: 'POST',
    headers: requestHeaders(),
    body: JSON.stringify({
      model: 'claude-sonnet-4',
      max_tokens: 32,
      messages: [{ role: 'user', content: `f06 scheduler check ${caseId}` }],
    }),
  })
  statuses.push(inference.status)
  latencies.headers.push(inference.headerMs)
  latencies.total.push(inference.totalMs)
  latencies.firstText.push(inference.headerMs)
  assert.equal(inference.status, 200, inference.text)
  assert.ok(inference.text.includes(digest), 'inference response did not prove selected key digest')
  const inferenceRecords = fake.records.slice(inferenceBefore)
  assert.deepEqual(countKinds(inferenceRecords), {
    model_discovery: 0,
    balance: 0,
    inference: 1,
    overage: 0,
    unknown: 0,
  })
  assert.ok(inferenceRecords.every((record) => record.valid))

  const duplicateBefore = fake.records.length
  const duplicate = await timedRequest(`${baseUrl}/api/admin/credentials`, {
    method: 'POST',
    headers: adminHeaders(),
    body: JSON.stringify({ authMethod: 'api_key', kiroApiKey: `${secret}|${region}` }),
  })
  statuses.push(duplicate.status)
  assert.equal(duplicate.status, 409, duplicate.text)
  const duplicateCounts = countKinds(fake.records.slice(duplicateBefore))
  assert.deepEqual(duplicateCounts, {
    model_discovery: 0,
    balance: 0,
    inference: 0,
    overage: 0,
    unknown: 0,
  }, 'duplicate import generated auxiliary upstream traffic')

  const exportFormats = ['json', 'backup-json', 'jsonl']
  const exportFormat = exportFormats[(round - 1) % exportFormats.length]
  const exported = await timedRequest(`${baseUrl}/api/admin/credentials/export?format=${exportFormat}`, {
    headers: adminHeaders(),
  })
  statuses.push(exported.status)
  assert.equal(exported.status, 200, exported.text)
  assert.ok(exported.text.includes(secret), 'explicit admin backup did not contain the requested reusable credential')
  assert.equal(exported.headers['cache-control'], 'no-store, private')
  assert.equal(exported.headers.pragma, 'no-cache')
  assert.equal(exported.headers['x-content-type-options'], 'nosniff')

  const resourcesPeak = processResources(service.pid)
  await stopService(service)

  const reloadStart = fake.records.length
  service = startService({ configPath, credentialsPath, logPath, port })
  await waitForHealth(baseUrl, service)
  await new Promise((resolve) => setTimeout(resolve, 150))
  const reloadList = await timedRequest(`${baseUrl}/api/admin/credentials`, { headers: adminHeaders() })
  statuses.push(reloadList.status)
  assert.equal(reloadList.status, 200, reloadList.text)
  assert.equal(reloadList.text.includes(secret), false)
  const reloadItem = JSON.parse(reloadList.text).credentials.find((item) => item.id === credentialId)
  assert.equal(reloadItem.apiRegion, region)
  assert.equal(reloadItem.endpoint, 'cli')

  const reloadInferenceBefore = fake.records.length
  const reloadInference = await timedRequest(`${baseUrl}/v1/messages`, {
    method: 'POST',
    headers: requestHeaders(),
    body: JSON.stringify({
      model: 'claude-sonnet-4',
      max_tokens: 32,
      messages: [{ role: 'user', content: `f06 reload scheduler check ${caseId}` }],
    }),
  })
  statuses.push(reloadInference.status)
  latencies.headers.push(reloadInference.headerMs)
  latencies.total.push(reloadInference.totalMs)
  latencies.firstText.push(reloadInference.headerMs)
  assert.equal(reloadInference.status, 200, reloadInference.text)
  assert.ok(reloadInference.text.includes(digest))
  const reloadInferenceRecords = fake.records.slice(reloadInferenceBefore)
  assert.equal(countKinds(reloadInferenceRecords).inference, 1)
  assert.ok(reloadInferenceRecords.every((record) => record.valid))

  const activeDelete = await timedRequest(`${baseUrl}/api/admin/credentials/${credentialId}`, {
    method: 'DELETE',
    headers: adminHeaders(),
  })
  statuses.push(activeDelete.status)
  assert.equal(activeDelete.status, 400, activeDelete.text)

  const disabled = await timedRequest(`${baseUrl}/api/admin/credentials/${credentialId}/disabled`, {
    method: 'POST',
    headers: adminHeaders(),
    body: JSON.stringify({ disabled: true }),
  })
  statuses.push(disabled.status)
  assert.equal(disabled.status, 200, disabled.text)

  const removed = await timedRequest(`${baseUrl}/api/admin/credentials/${credentialId}`, {
    method: 'DELETE',
    headers: adminHeaders(),
  })
  statuses.push(removed.status)
  assert.equal(removed.status, 200, removed.text)
  const afterDelete = databaseCredentialState()
  assert.equal(afterDelete.activeRows, 0)
  assert.ok(afterDelete.deletedRows >= 1)

  await new Promise((resolve) => setTimeout(resolve, 150))
  const audit = auditState()
  assert.ok(audit.exportRows >= 1)
  assert.equal(audit.secretLikeRows, 0)

  await new Promise((resolve) => setTimeout(resolve, 250))
  const resourcesEnd = processResources(service.pid)
  resourcesPeak.rssKb = Math.max(resourcesPeak.rssKb, resourcesEnd.rssKb)
  resourcesPeak.fdCount = Math.max(resourcesPeak.fdCount, resourcesEnd.fdCount)
  await stopService(service)
  const serviceLog = fs.readFileSync(logPath, 'utf8')
  assert.equal(serviceLog.includes(secret), false, 'service log exposed full API key')

  const caseRecords = fake.records.slice(recordStart)
  assert.ok(caseRecords.every((record) => record.valid), JSON.stringify(caseRecords.filter((record) => !record.valid)))
  assert.equal(countKinds(caseRecords).unknown, 0)
  await cleanupOwnedRedisKeys()
  fs.rmSync(caseRoot, { recursive: true, force: true })

  return {
    caseId,
    mode,
    round,
    keyDigest: digest,
    region,
    credentialId,
    dbState,
    importAuxiliaryHits: importCounts,
    duplicateAuxiliaryHits: duplicateCounts,
    reloadHits: countKinds(fake.records.slice(reloadStart)),
    totalHits: countKinds(caseRecords),
    capturedRequests: caseRecords.map((record) => ({
      kind: record.kind,
      method: record.method,
      path: record.path,
      logicalHost: record.logicalHost,
      authorizationScheme: record.authorizationScheme,
      keyDigest: record.keyDigest,
      tokenType: record.tokenType,
      target: record.target,
      valid: record.valid,
    })),
    export: {
      format: exportFormat,
      cacheControl: exported.headers['cache-control'],
      pragma: exported.headers.pragma,
      contentTypeOptions: exported.headers['x-content-type-options'],
      explicitSecretPresent: true,
    },
    audit,
    malformedAdminApi,
    resources: { start: resourcesStart, peak: resourcesPeak, end: resourcesEnd },
    requestIds: [
      inference.headers['request-id'] || inference.headers['x-request-id'],
      reloadInference.headers['request-id'] || reloadInference.headers['x-request-id'],
    ].filter(Boolean),
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
      postgresDatabase: inputIdentity.postgresDatabase,
      redisDatabase: inputIdentity.redisDatabase,
      redisPrefixSha256: sha256(REDIS_PREFIX),
      createsPostgresDatabase: false,
      flushesRedisDatabase: false,
    })}\n`)
    return
  }
  PSQL = resolveExecutable(process.env.KIRO_PSQL_BINARY || 'psql')
  fs.mkdirSync(TEMP_ROOT, { recursive: true, mode: 0o700 })
  fs.mkdirSync(REPORT_ROOT, { recursive: true })
  const upstreamPort = await reservePort()
  const fake = createFakeUpstream()
  const cases = []
  let malformedJsonFile = []
  let malformedPlainFile = []
  const latencies = { headers: [], total: [], firstText: [] }
  const statuses = []
  let postgresInitialState = null
  let cleanup = {
    ownedRedisRemoved: false,
    ownedRedisRemaining: null,
    tempSecretsRemoved: false,
    portsReleased: false,
  }

  try {
    postgresInitialState = assertInitialDatabaseOwnedAndSafe()
    await cleanupOwnedRedisKeys()
    await fake.listen(upstreamPort)

    const modes = ['admin_api', 'json_file', 'plain_file']
    malformedJsonFile = await runMalformedCredentialFileCases({
      mode: 'json_file',
      upstreamPort,
      fake,
    })
    malformedPlainFile = await runMalformedCredentialFileCases({
      mode: 'plain_file',
      upstreamPort,
      fake,
    })
    for (const mode of modes) {
      for (let round = 1; round <= ROUNDS; round += 1) {
        process.stderr.write(`F06 ${mode} round ${round}/${ROUNDS}\n`)
        cases.push(await runCase({
          mode,
          round,
          upstreamPort,
          fake,
          latencies,
          statuses,
        }))
      }
    }

    const statusDistribution = Object.fromEntries(
      [...new Set(statuses)].sort((a, b) => a - b).map((status) => [String(status), statuses.filter((value) => value === status).length]),
    )
    const gitRevision = run('git', ['rev-parse', 'HEAD'])
    const dirty = run('git', ['status', '--porcelain=v1'])
    const diff = run('git', ['diff', '--binary'])
    const report = {
      schemaVersion: 3,
      caseId: 'F06',
      runId: RUN_ID,
      generatedAt: new Date().toISOString(),
      result: 'pass',
      roundsPerEntry: ROUNDS,
      entries: ['admin_api', 'json_file', 'plain_file'],
      gitRevision,
      dirty: Boolean(dirty),
      dirtyDiffSha256: sha256(diff),
      binaryPathSha256: sha256(BINARY),
      binarySha256: sha256File(BINARY),
      isolation: {
        dockerUsed: false,
        cargoUsed: false,
        postgresDatabaseCreatedByRunner: false,
        redisDatabaseFlushedByRunner: false,
        redisOwnedPrefixCleanupOnly: true,
        servicePort9022UsedByRunner: false,
        realAwsOrKiroConfigured: false,
        realAwsOrKiroAccessObserved: false,
        outboundFirewallEnforced: false,
        postgresDatabase: inputIdentity.postgresDatabase,
        postgresInitialState,
        redisDatabase: inputIdentity.redisDatabase,
        redisPrefixSha256: sha256(REDIS_PREFIX),
        fakeUpstreamPort: upstreamPort,
      },
      statusDistribution,
      latencyMs: {
        ttfb: latencySummary(latencies.headers),
        total: latencySummary(latencies.total),
        firstThinking: null,
        firstText: latencySummary(latencies.firstText),
      },
      malformedInput: {
        adminApi: cases.flatMap((item) => item.malformedAdminApi),
        jsonFile: malformedJsonFile,
        plainFile: malformedPlainFile,
      },
      cases,
      auxiliaryTotals: countKinds(fake.records),
      sampledRequestIds: cases.flatMap((item) => item.requestIds).slice(0, 20),
      sampledErrorIds: [],
      cleanup,
    }
    const serializedReport = `${JSON.stringify(report, null, 2)}\n`
    for (const forbidden of REPORT_FORBIDDEN_VALUES) {
      assert.equal(
        serializedReport.includes(forbidden),
        false,
        'generated report contains a reusable credential or secret fixture',
      )
    }
    fs.writeFileSync(REPORT_PATH, serializedReport)
  } finally {
    await Promise.all([...ACTIVE_SERVICES].map((handle) => stopService(handle)))
    await fake.close().catch(() => {})
    const redisCleanup = await cleanupOwnedRedisKeys().catch((error) => ({
      removed: 0,
      remaining: `cleanup_failed:${error.message}`,
    }))
    cleanup.ownedRedisRemoved = redisCleanup.remaining === 0
    cleanup.ownedRedisRemaining = redisCleanup.remaining
    fs.rmSync(TEMP_ROOT, { recursive: true, force: true })
    cleanup.tempSecretsRemoved = !fs.existsSync(TEMP_ROOT)
    const ports = [upstreamPort]
    cleanup.portsReleased = await Promise.all(ports.map(async (port) => {
      try {
        await waitForTcp(port, 250)
        return false
      } catch {
        return true
      }
    })).then((values) => values.every(Boolean))
    if (fs.existsSync(REPORT_PATH)) {
      const report = JSON.parse(fs.readFileSync(REPORT_PATH, 'utf8'))
      report.cleanup = cleanup
      report.result = cleanupSucceeded(cleanup) ? report.result : 'fail'
      fs.writeFileSync(REPORT_PATH, `${JSON.stringify(report, null, 2)}\n`)
    }
  }

  const report = JSON.parse(fs.readFileSync(REPORT_PATH, 'utf8'))
  assert.equal(report.result, 'pass')
  assert.ok(cleanupSucceeded(report.cleanup))
  process.stdout.write(`${REPORT_PATH}\n`)
}

main().catch((error) => {
  process.stderr.write(`F06 lifecycle validation failed: ${error.message}\n`)
  process.exitCode = 1
})
