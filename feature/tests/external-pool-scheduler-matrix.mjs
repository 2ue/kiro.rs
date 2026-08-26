#!/usr/bin/env node

/*
 * Product-level external-pool scheduler matrix.
 *
 * This is not a unit test oracle. It starts one frozen kiro.rs process, three
 * loopback Anthropic-compatible mock upstreams, and drives real HTTP requests
 * through the service. Each scenario changes the mock upstream behavior and,
 * where needed, the per-pool scheduler config through the admin API.
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

const POSTGRES_URL = requiredEnvironment('KIRO_EXTERNAL_MATRIX_POSTGRES_URL')
const REDIS_URL = requiredEnvironment('KIRO_EXTERNAL_MATRIX_REDIS_URL')
const REDIS_PREFIX = requiredEnvironment('KIRO_EXTERNAL_MATRIX_REDIS_PREFIX')

const REQUEST_KEY = 'sk-external-matrix-request'
const ADMIN_KEY = 'sk-external-matrix-admin'
const MODEL = process.env.KIRO_EXTERNAL_MATRIX_MODEL || 'claude-sonnet-4'
const ROUTE = process.env.KIRO_EXTERNAL_MATRIX_ROUTE || '/v1/messages'
const ROUTE_RULE = ROUTE.replace(/\/v1\/messages$/i, '') || '/v1'
const REQUESTS_PER_SCENARIO = boundedInteger(
  process.env.KIRO_EXTERNAL_MATRIX_REQUESTS_PER_SCENARIO,
  16,
  4,
  1000,
)
const MAX_CONCURRENCY = boundedInteger(
  process.env.KIRO_EXTERNAL_MATRIX_MAX_CONCURRENCY,
  8,
  1,
  256,
)
const TARGET_RPM = boundedInteger(process.env.KIRO_EXTERNAL_MATRIX_RPM, 240, 1, 6000)
const CLIENT_TIMEOUT_MS = boundedInteger(
  process.env.KIRO_EXTERNAL_MATRIX_CLIENT_TIMEOUT_MS,
  15_000,
  1000,
  120_000,
)
const STREAM_CHUNK_DELAY_MS = boundedInteger(
  process.env.KIRO_EXTERNAL_MATRIX_STREAM_CHUNK_DELAY_MS,
  35,
  0,
  5000,
)
const STREAM_CHUNKS = boundedInteger(
  process.env.KIRO_EXTERNAL_MATRIX_STREAM_CHUNKS,
  4,
  1,
  128,
)
const SCENARIO_FILTER = new Set(
  String(process.env.KIRO_EXTERNAL_MATRIX_SCENARIOS || '')
    .split(',')
    .map((item) => item.trim())
    .filter(Boolean),
)
const KEEP_TEMP = process.env.KIRO_EXTERNAL_MATRIX_KEEP_TEMP === '1'

const RUN_ID = `external-matrix-${Date.now()}-${process.pid}-${crypto.randomBytes(3).toString('hex')}`
const TEMP_ROOT = fs.mkdtempSync(path.join(os.tmpdir(), `${RUN_ID}-`))
const REPORT_ROOT = path.join(ARTIFACT_ROOT, 'reports', 'external-pool-scheduler-matrix')
const REPORT_PATH = path.join(REPORT_ROOT, `${RUN_ID}.json`)
const CHILDREN = new Set()
const SERVERS = new Set()
const MOCK_NAMES = ['primary', 'backup_a', 'backup_b']
let lastScenarioReport = null

const BEHAVIOR = {
  jsonSuccess: { type: 'json_success' },
  streamSuccess: { type: 'stream_success' },
  success: { type: 'success' },
}

const BASE_POOL_CONFIG = {
  enabled: true,
  maxConcurrentRequests: 64,
  routeMode: 'allow_all',
  routeRules: [],
  autoDisablePolicy: 'inherit',
  preOutputStreamRetryMode: 'inherit',
}

const BASE_EXTERNAL_POOLS_CONFIG = {
  externalPoolsEnabled: true,
  externalDirectPolicyEnabled: true,
  directExternalPathRules: [ROUTE_RULE],
  directExternalModelRules: [MODEL],
  localPoolRouteMode: 'allow_all',
  localPoolRouteRules: [],
  externalPoolRouteMode: 'allow_all',
  externalPoolRouteRules: [],
  fallbackOnLocalCapacityExhausted: false,
  fallbackOnSchedulerRedisDegraded: false,
  fallbackOnNoAvailableCredentials: false,
  fallbackOnLocalTransientExhausted: false,
  fallbackOnUnsupportedModel: false,
  localPoolPreflightEnabled: false,
  externalPoolLocalRescueEnabled: false,
  externalPoolAutoDisableEnabled: true,
  externalPoolAutoDisableOnAuthError: true,
  externalPoolAutoDisableOnSecurityLock: true,
  externalPoolAutoDisableOnQuotaExhausted: false,
  externalPoolAutoDisableOnMisconfiguredEndpoint: false,
  externalPoolAutoDisableOnChannelDisabled: true,
  externalPoolGlobalMaxConcurrentRequests: 256,
  externalPoolMaxQueuedRequests: 256,
  externalPoolRequestTimeoutSecs: 5,
  externalPoolStreamRequestTimeoutSecs: 0,
  externalPoolStreamIdleTimeoutSecs: 2,
  externalPoolRetryMaxAttempts: 8,
  externalPoolRetryStatusCodes: [408, 425, 429, 500, 502, 503, 504, 529],
  externalPoolRetryOnNetworkError: true,
  externalPoolRetryOnProtocolError: true,
  externalPoolSamePoolRetryCount: 1,
  externalPoolSamePoolRetryStatusCodes: [408, 425, 429, 500, 502, 503, 504, 529],
  externalPoolSamePoolRetryDelayMs: 10,
  externalPoolTransientFailurePriorityPenalty: 20,
  externalPoolRateLimitCooldownSecs: 2,
  externalPoolServerErrorCooldownSecs: 2,
  externalPoolNetworkErrorCooldownSecs: 2,
  externalPoolProtocolErrorCooldownSecs: 2,
  externalPoolModelUnavailableCooldownSecs: 2,
  externalPoolModelUnavailableCooldownMode: 'model',
  externalPoolCapacityMode: 'fail_fast',
  externalPoolStreamPreOutputRetryEnabled: true,
  externalPoolUsageProjectionCostFloorEnabled: true,
  externalPoolUsageProjectionCostFloorMarginPercent: 10,
  externalPoolUsageProjectionUpliftPercent: 0,
  externalPoolUsageProjectionOutputUpliftMinTokens: 0,
  externalPoolUsageProjectionOutputUpliftPercent: 0,
  externalPoolUsageDebugEnabled: false,
}

const SCENARIOS = [
  {
    id: 'direct_policy_disabled_uses_local_first',
    stream: false,
    requestCount: Math.max(12, Math.floor(REQUESTS_PER_SCENARIO / 2)),
    concurrency: Math.min(MAX_CONCURRENCY, 4),
    externalPoolsOverrides: {
      externalDirectPolicyEnabled: false,
      externalPoolLocalRescueEnabled: false,
    },
    localBehavior: { type: 'success' },
    behaviors: {
      primary: { type: 'json_success' },
      backup_a: { type: 'json_success' },
      backup_b: { type: 'json_success' },
    },
    expect: { all200: true, totalExternalHitsMax: 0, localHitsMinRequests: 1 },
  },
  {
    id: 'local_transient_then_external_fallback',
    stream: false,
    requestCount: Math.max(12, Math.floor(REQUESTS_PER_SCENARIO / 2)),
    concurrency: Math.min(MAX_CONCURRENCY, 4),
    externalPoolsOverrides: {
      externalDirectPolicyEnabled: false,
      externalPoolLocalRescueEnabled: false,
      fallbackOnLocalTransientExhausted: true,
    },
    localBehavior: { type: 'json_error', status: 500, message: 'local transient 500' },
    behaviors: {
      primary: { type: 'json_success' },
      backup_a: { type: 'json_success' },
      backup_b: { type: 'json_success' },
    },
    expect: { all200: true, localHitsMin: 1, primaryHitsMin: 1 },
  },
  {
    id: 'local_capacity_external_fail_local_rescue',
    stream: false,
    requestCount: Math.max(8, Math.floor(REQUESTS_PER_SCENARIO / 2)),
    concurrency: Math.min(MAX_CONCURRENCY, 8),
    localHolder: { delayMs: 5600 },
    waitBeforeMs: 6000,
    externalPoolsOverrides: {
      externalDirectPolicyEnabled: false,
      externalPoolLocalRescueEnabled: true,
      externalPoolLocalRescueOnCapacity: true,
      fallbackOnLocalCapacityExhausted: true,
      localPoolPreflightEnabled: true,
      externalPoolSamePoolRetryCount: 0,
    },
    localBehavior: { type: 'success' },
    behaviors: {
      primary: { type: 'json_error', status: 500, delayMs: 900, message: 'primary down during rescue test' },
      backup_a: { type: 'json_error', status: 500, delayMs: 900, message: 'backup_a down during rescue test' },
      backup_b: { type: 'json_error', status: 500, delayMs: 900, message: 'backup_b down during rescue test' },
    },
    expect: {
      all200: true,
      primaryHitsMin: 1,
      backupHitsMin: 1,
      localHitsMinRequests: 1,
    },
  },
  {
    id: 'local_capacity_external_fail_no_local_rescue',
    stream: false,
    requestCount: Math.max(8, Math.floor(REQUESTS_PER_SCENARIO / 2)),
    concurrency: Math.min(MAX_CONCURRENCY, 8),
    localHolder: { delayMs: 9000 },
    waitBeforeMs: 6000,
    externalPoolsOverrides: {
      externalDirectPolicyEnabled: false,
      externalPoolLocalRescueEnabled: false,
      fallbackOnLocalCapacityExhausted: true,
      localPoolPreflightEnabled: true,
      externalPoolSamePoolRetryCount: 0,
    },
    localBehavior: { type: 'success' },
    behaviors: {
      primary: { type: 'json_error', status: 500, delayMs: 900, message: 'primary down without rescue' },
      backup_a: { type: 'json_error', status: 500, delayMs: 900, message: 'backup_a down without rescue' },
      backup_b: { type: 'json_error', status: 500, delayMs: 900, message: 'backup_b down without rescue' },
    },
    expect: {
      no200: true,
      primaryHitsMin: 1,
      backupHitsMin: 1,
      localHitsMax: 0,
    },
  },
  {
    id: 'direct_external_fail_never_local_rescue',
    stream: false,
    requestCount: Math.max(8, Math.floor(REQUESTS_PER_SCENARIO / 2)),
    concurrency: Math.min(MAX_CONCURRENCY, 3),
    externalPoolsOverrides: {
      externalDirectPolicyEnabled: true,
      externalPoolLocalRescueEnabled: true,
    },
    localBehavior: { type: 'success' },
    behaviors: {
      primary: { type: 'json_error', status: 500, message: 'direct primary down' },
      backup_a: { type: 'json_error', status: 500, message: 'direct backup_a down' },
      backup_b: { type: 'json_error', status: 500, message: 'direct backup_b down' },
    },
    expect: { no200: true, primaryHitsMin: 1, backupHitsMin: 1, noLocal: true },
  },
  {
    id: 'normal_stream_primary',
    stream: true,
    requestCount: REQUESTS_PER_SCENARIO,
    concurrency: Math.min(MAX_CONCURRENCY, 6),
    behaviors: {
      primary: { type: 'stream_success' },
      backup_a: { type: 'stream_success' },
      backup_b: { type: 'stream_success' },
    },
    expect: { all200: true, primaryHitsMin: 1, noLocal: true },
  },
  {
    id: 'normal_non_stream_primary',
    stream: false,
    requestCount: REQUESTS_PER_SCENARIO,
    concurrency: Math.min(MAX_CONCURRENCY, 6),
    behaviors: {
      primary: { type: 'json_success' },
      backup_a: { type: 'json_success' },
      backup_b: { type: 'json_success' },
    },
    expect: { all200: true, primaryHitsMin: 1, noLocal: true },
  },
  {
    id: 'single_pool_route_block_falls_to_backup',
    stream: false,
    requestCount: Math.max(8, Math.floor(REQUESTS_PER_SCENARIO / 2)),
    concurrency: Math.min(MAX_CONCURRENCY, 4),
    poolOverrides: {
      primary: { routeMode: 'allow_list', routeRules: ['/cc'] },
      backup_a: { routeMode: 'allow_all', routeRules: [] },
      backup_b: { routeMode: 'allow_all', routeRules: [] },
    },
    behaviors: {
      primary: { type: 'json_success' },
      backup_a: { type: 'json_success' },
      backup_b: { type: 'json_success' },
    },
    expect: { all200: true, primaryHitsMax: 0, backupHitsMin: 1, noLocal: true },
  },
  {
    id: 'local_route_block_falls_to_external',
    stream: false,
    requestCount: Math.max(8, Math.floor(REQUESTS_PER_SCENARIO / 2)),
    concurrency: Math.min(MAX_CONCURRENCY, 4),
    externalPoolsOverrides: {
      externalDirectPolicyEnabled: true,
      externalPoolLocalRescueEnabled: false,
      localPoolRouteMode: 'deny_list',
      localPoolRouteRules: [ROUTE_RULE],
    },
    behaviors: {
      primary: { type: 'json_success' },
      backup_a: { type: 'json_success' },
      backup_b: { type: 'json_success' },
    },
    expect: { all200: true, primaryHitsMin: 1, backupHitsMax: 0, noLocal: true },
  },
  {
    id: 'priority_500_stream_failover',
    stream: true,
    requestCount: Math.max(24, REQUESTS_PER_SCENARIO),
    concurrency: Math.min(MAX_CONCURRENCY, 8),
    behaviors: {
      primary: { type: 'json_error', status: 500, message: 'primary persistent 500' },
      backup_a: { type: 'stream_success' },
      backup_b: { type: 'stream_success' },
    },
    expect: {
      all200: true,
      primaryHitsMin: 1,
      primaryHitsMaxMultiplier: 1.25,
      backupHitsMin: 1,
      noLocal: true,
    },
  },
  {
    id: 'recovery_after_500_backoff',
    stream: false,
    requestCount: 8,
    concurrency: 1,
    clearState: false,
    waitBeforeMs: 35_000,
    behaviors: {
      primary: { type: 'json_success' },
      backup_a: { type: 'json_success' },
      backup_b: { type: 'json_success' },
    },
    expect: { all200: true, primaryHitsMin: 1, noLocal: true },
  },
  {
    id: 'rate_limit_429_non_stream_failover',
    stream: false,
    requestCount: Math.max(18, REQUESTS_PER_SCENARIO),
    concurrency: Math.min(MAX_CONCURRENCY, 6),
    behaviors: {
      primary: { type: 'json_error', status: 429, message: 'primary upstream rate limited' },
      backup_a: { type: 'json_success' },
      backup_b: { type: 'json_success' },
    },
    expect: {
      all200: true,
      primaryHitsMin: 1,
      primaryHitsMaxMultiplier: 1.5,
      backupHitsMin: 1,
      noLocal: true,
    },
  },
  {
    id: 'auth_403_auto_disable_failover',
    stream: false,
    requestCount: Math.max(8, Math.floor(REQUESTS_PER_SCENARIO / 2)),
    concurrency: Math.min(MAX_CONCURRENCY, 2),
    behaviors: {
      primary: { type: 'json_error', status: 403, message: 'invalid api key' },
      backup_a: { type: 'json_success' },
      backup_b: { type: 'json_success' },
    },
    expect: {
      all200: true,
      primaryHitsMin: 1,
      primaryHitsMaxMultiplier: 0.75,
      backupHitsMin: 1,
      primaryAutoDisabled: true,
      noLocal: true,
    },
  },
  {
    id: 'intermittent_500_mixed_stream',
    stream: true,
    requestCount: Math.max(30, REQUESTS_PER_SCENARIO),
    concurrency: Math.min(MAX_CONCURRENCY, 6),
    behaviors: {
      primary: { type: 'mixed' },
      backup_a: { type: 'stream_success' },
      backup_b: { type: 'stream_success' },
    },
    expect: { successMinRatio: 0.8, primaryHitsMin: 1, backupHitsMin: 1, noLocal: true },
  },
  {
    id: 'slow_first_byte_stream',
    stream: true,
    requestCount: Math.max(8, Math.floor(REQUESTS_PER_SCENARIO / 2)),
    concurrency: Math.min(MAX_CONCURRENCY, 3),
    behaviors: {
      primary: { type: 'slow_first_byte', delayMs: 900 },
      backup_a: { type: 'stream_success' },
      backup_b: { type: 'stream_success' },
    },
    expect: { all200: true, primaryHitsMin: 1, firstTextP95MinMs: 700, noLocal: true },
  },
  {
    id: 'stream_idle_before_commit_no_takeover',
    stream: true,
    requestCount: Math.max(8, Math.floor(REQUESTS_PER_SCENARIO / 2)),
    concurrency: Math.min(MAX_CONCURRENCY, 2),
    behaviors: {
      primary: { type: 'stream_idle', idleMs: 4200 },
      backup_a: { type: 'stream_success' },
      backup_b: { type: 'stream_success' },
    },
    expect: { primaryHitsMin: 1, backupHitsMax: 0, noLocal: true },
  },
  {
    id: 'stream_pre_output_error_retry',
    stream: true,
    requestCount: Math.max(8, Math.floor(REQUESTS_PER_SCENARIO / 2)),
    concurrency: Math.min(MAX_CONCURRENCY, 2),
    behaviors: {
      primary: { type: 'stream_pre_output_error' },
      backup_a: { type: 'stream_success' },
      backup_b: { type: 'stream_success' },
    },
    expect: { successMinRatio: 0.75, primaryHitsMin: 1, backupHitsMin: 1, noLocal: true },
  },
  {
    id: 'stream_post_output_error_no_replay',
    stream: true,
    requestCount: Math.max(6, Math.floor(REQUESTS_PER_SCENARIO / 3)),
    concurrency: 1,
    behaviors: {
      primary: { type: 'stream_post_output_error' },
      backup_a: { type: 'stream_success' },
      backup_b: { type: 'stream_success' },
    },
    expect: {
      primaryHitsMin: 1,
      backupHitsMax: 0,
      downstreamErrorEventMin: 1,
      noLocal: true,
    },
  },
  {
    id: 'concurrency_saturation_uses_backup',
    stream: true,
    requestCount: Math.max(16, REQUESTS_PER_SCENARIO),
    concurrency: Math.min(MAX_CONCURRENCY, 8),
    poolOverrides: {
      primary: { maxConcurrentRequests: 1 },
      backup_a: { maxConcurrentRequests: 64 },
      backup_b: { maxConcurrentRequests: 64 },
    },
    behaviors: {
      primary: { type: 'long_stream', chunkCount: 16, chunkDelayMs: 90 },
      backup_a: { type: 'stream_success' },
      backup_b: { type: 'stream_success' },
    },
    expect: { all200: true, primaryHitsMin: 1, backupHitsMin: 1, noLocal: true },
  },
  {
    id: 'sustained_500_with_backup_rpm',
    stream: false,
    requestCount: Math.max(20, REQUESTS_PER_SCENARIO),
    concurrency: Math.min(MAX_CONCURRENCY, 5),
    durationSecs: 5,
    rpm: TARGET_RPM,
    behaviors: {
      primary: { type: 'json_error', status: 500, message: 'primary sustained 500' },
      backup_a: { type: 'json_success', delayMs: 80 },
      backup_b: { type: 'json_success', delayMs: 80 },
    },
    expect: { all200: true, primaryHitsMin: 1, backupHitsMin: 1, noLocal: true },
  },
  {
    id: 'all_pools_persistent_500',
    stream: false,
    requestCount: Math.max(8, Math.floor(REQUESTS_PER_SCENARIO / 2)),
    concurrency: Math.min(MAX_CONCURRENCY, 3),
    behaviors: {
      primary: { type: 'json_error', status: 500, message: 'primary down' },
      backup_a: { type: 'json_error', status: 500, message: 'backup_a down' },
      backup_b: { type: 'json_error', status: 500, message: 'backup_b down' },
    },
    expect: { no200: true, primaryHitsMin: 1, backupHitsMin: 1, noLocal: true },
  },
]

function requiredEnvironment(name) {
  const value = String(process.env[name] || '').trim()
  if (!value) throw new Error(`${name} is required`)
  return value
}

function boundedInteger(value, fallback, min, max) {
  const parsed = Number(value)
  if (!Number.isFinite(parsed)) return fallback
  return Math.min(max, Math.max(min, Math.floor(parsed)))
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

async function waitForCondition(predicate, description, timeoutMs = 10_000) {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    if (await predicate()) return
    await sleep(10)
  }
  throw new Error(`timeout waiting for ${description}`)
}

function sha256(value) {
  return crypto.createHash('sha256').update(value).digest('hex')
}

function redact(value) {
  let output = String(value || '')
  for (const secret of [POSTGRES_URL, REDIS_URL, REDIS_PREFIX, REQUEST_KEY, ADMIN_KEY]) {
    if (secret) output = output.split(secret).join('<redacted>')
  }
  return output
    .replace(/sk-[A-Za-z0-9_-]+/g, '<redacted-key>')
    .replace(/\u001b\[[0-9;]*m/g, '')
}

function validateStorage() {
  const pg = new URL(POSTGRES_URL)
  if (!['postgres:', 'postgresql:'].includes(pg.protocol)) throw new Error('PostgreSQL URL must use postgres://')
  if (!['127.0.0.1', 'localhost', '::1'].includes(pg.hostname)) throw new Error('PostgreSQL URL must target loopback')
  if (Number(pg.port || 5432) === 9022) throw new Error('PostgreSQL protected port 9022')
  const redis = new URL(REDIS_URL)
  if (redis.protocol !== 'redis:') throw new Error('Redis URL must use redis://')
  if (!['127.0.0.1', 'localhost', '::1'].includes(redis.hostname)) throw new Error('Redis URL must target loopback')
  if (Number(redis.port || 6379) === 9022) throw new Error('Redis protected port 9022')
  const db = Number(redis.pathname.replace(/^\//, ''))
  if (!Number.isInteger(db) || db < 1 || db > 15) throw new Error('Redis URL must use isolated DB 1..15')
  if (!/^[a-z0-9][a-z0-9:._-]{7,120}$/.test(REDIS_PREFIX)) throw new Error('invalid Redis prefix')
  return { postgresHost: pg.hostname, redisHost: redis.hostname, redisDb: db }
}

function reservePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer()
    server.listen(0, '127.0.0.1', () => {
      const address = server.address()
      const port = typeof address === 'object' && address ? address.port : 0
      server.close((error) => {
        if (error) reject(error)
        else if (port === 9022) reservePort().then(resolve, reject)
        else resolve(port)
      })
    })
    server.once('error', reject)
  })
}

function readBody(request) {
  return new Promise((resolve, reject) => {
    const chunks = []
    request.on('data', (chunk) => chunks.push(chunk))
    request.once('end', () => resolve(Buffer.concat(chunks).toString('utf8')))
    request.once('error', reject)
  })
}

function safeJsonParse(value) {
  try {
    return JSON.parse(value)
  } catch {
    return null
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

function encodeEventStreamHeaders(headers) {
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

function eventStreamFrame(eventType, payload) {
  const headers = encodeEventStreamHeaders({
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

function extractMatrixMarker(raw) {
  return raw.match(/external scheduler matrix\s+([A-Za-z0-9_.:-]+)/)?.[1] || 'unknown'
}

function json(response, status, body, extraHeaders = {}) {
  const payload = Buffer.from(JSON.stringify(body))
  response.writeHead(status, {
    'content-type': 'application/json',
    'content-length': payload.length,
    connection: 'close',
    ...extraHeaders,
  })
  response.end(payload)
}

function sseFrame(event, payload) {
  return `event: ${event}\ndata: ${JSON.stringify(payload)}\n\n`
}

async function writeJsonSuccess(response, name, state, behavior = {}) {
  if (behavior.delayMs) await sleep(behavior.delayMs)
  state.nonStreamHits += 1
  json(response, 200, {
    id: `msg_${name}_${state.hits}`,
    type: 'message',
    role: 'assistant',
    model: MODEL,
    content: [{ type: 'text', text: `${name}-ok-${state.hits}` }],
    stop_reason: 'end_turn',
    usage: {
      input_tokens: behavior.inputTokens ?? 19,
      output_tokens: behavior.outputTokens ?? 5,
      cache_read_input_tokens: behavior.cacheReadTokens ?? 0,
      cache_creation_input_tokens: behavior.cacheCreationTokens ?? 0,
    },
  })
}

async function writeStreamSuccess(response, name, state, behavior = {}) {
  if (behavior.delayMs) await sleep(behavior.delayMs)
  state.streamHits += 1
  response.writeHead(200, {
    'content-type': 'text/event-stream',
    connection: 'close',
    'cache-control': 'no-cache',
  })
  response.write(sseFrame('message_start', {
    type: 'message_start',
    message: {
      id: `msg_${name}_${state.hits}`,
      type: 'message',
      role: 'assistant',
      model: MODEL,
      content: [],
      stop_reason: null,
      usage: {
        input_tokens: behavior.inputTokens ?? 19,
        output_tokens: 0,
        cache_read_input_tokens: behavior.cacheReadTokens ?? 0,
        cache_creation_input_tokens: behavior.cacheCreationTokens ?? 0,
      },
    },
  }))
  response.write(sseFrame('content_block_start', {
    type: 'content_block_start',
    index: 0,
    content_block: { type: 'text', text: '' },
  }))
  const chunkCount = behavior.chunkCount ?? STREAM_CHUNKS
  const delay = behavior.chunkDelayMs ?? STREAM_CHUNK_DELAY_MS
  for (let index = 0; index < chunkCount; index += 1) {
    response.write(sseFrame('content_block_delta', {
      type: 'content_block_delta',
      index: 0,
      delta: { type: 'text_delta', text: `${name}-chunk-${index}` },
    }))
    if (delay > 0) await sleep(delay)
  }
  response.write(sseFrame('content_block_stop', {
    type: 'content_block_stop',
    index: 0,
  }))
  response.write(sseFrame('message_delta', {
    type: 'message_delta',
    delta: { stop_reason: 'end_turn', stop_sequence: null },
    usage: { output_tokens: behavior.outputTokens ?? 5 },
  }))
  response.write(sseFrame('message_stop', { type: 'message_stop' }))
  response.end()
}

function createMockUpstream(name, initialBehavior) {
  const state = {
    name,
    behavior: initialBehavior,
    hits: 0,
    errors: 0,
    streamHits: 0,
    nonStreamHits: 0,
    requests: [],
    behaviorHits: {},
    authHeaders: [],
    requestIds: [],
  }
  const server = http.createServer(async (request, response) => {
    const bodyText = await readBody(request)
    const body = safeJsonParse(bodyText)
    const behavior = { ...(state.behavior || BEHAVIOR.success) }
    const requestId = String(request.headers['x-request-id'] || request.headers['request-id'] || '').trim() || null
    state.hits += 1
    state.behaviorHits[behavior.type || 'success'] = (state.behaviorHits[behavior.type || 'success'] || 0) + 1
    state.requests.push({
      path: request.url,
      method: request.method,
      model: body?.model || null,
      stream: Boolean(body?.stream),
      requestId,
      acceptEncoding: request.headers['accept-encoding'] || null,
      authorization: request.headers.authorization ? 'present' : null,
      xApiKey: request.headers['x-api-key'] ? 'present' : null,
    })
    state.authHeaders.push({
      authorization: request.headers.authorization ? 'present' : null,
      xApiKey: request.headers['x-api-key'] ? 'present' : null,
      acceptEncoding: request.headers['accept-encoding'] || null,
    })
    if (requestId) state.requestIds.push(requestId)

    if (behavior.type === 'json_error' || behavior.type === 'continuous_error') {
      if (behavior.delayMs) await sleep(behavior.delayMs)
      state.errors += 1
      const status = behavior.status || 500
      json(response, status, {
        type: 'error',
        error: {
          type: behavior.errorType || (
            status === 429 ? 'rate_limit_error' : status === 403 ? 'permission_error' : 'api_error'
          ),
          message: behavior.message || `${name} controlled ${status}`,
        },
      }, behavior.retryAfter ? { 'retry-after': String(behavior.retryAfter) } : {})
      return
    }

    if (behavior.type === 'slow_first_byte') {
      await sleep(behavior.delayMs || 900)
      if (body?.stream) await writeStreamSuccess(response, name, state, behavior)
      else await writeJsonSuccess(response, name, state, behavior)
      return
    }

    if (behavior.type === 'stream_idle') {
      state.streamHits += 1
      response.writeHead(200, {
        'content-type': 'text/event-stream',
        connection: 'close',
        'cache-control': 'no-cache',
      })
      response.write(sseFrame('message_start', {
        type: 'message_start',
        message: {
          id: `msg_${name}_${state.hits}`,
          type: 'message',
          role: 'assistant',
          model: MODEL,
          content: [],
          stop_reason: null,
          usage: { input_tokens: 19, output_tokens: 0 },
        },
      }))
      response.write(sseFrame('content_block_start', {
        type: 'content_block_start',
        index: 0,
        content_block: { type: 'text', text: '' },
      }))
      await sleep(behavior.idleMs || 4200)
      response.destroy()
      return
    }

    if (behavior.type === 'stream_pre_output_error') {
      state.streamHits += 1
      state.errors += 1
      response.writeHead(200, {
        'content-type': 'text/event-stream',
        connection: 'close',
        'cache-control': 'no-cache',
      })
      response.write(sseFrame('message_start', {
        type: 'message_start',
        message: {
          id: `msg_${name}_${state.hits}`,
          type: 'message',
          role: 'assistant',
          model: MODEL,
          content: [],
          stop_reason: null,
          usage: { input_tokens: 19, output_tokens: 0 },
        },
      }))
      response.write(sseFrame('error', {
        type: 'error',
        error: { type: 'api_error', message: `${name} pre-output error` },
      }))
      response.end()
      return
    }

    if (behavior.type === 'stream_post_output_error') {
      state.streamHits += 1
      state.errors += 1
      response.writeHead(200, {
        'content-type': 'text/event-stream',
        connection: 'close',
        'cache-control': 'no-cache',
      })
      response.write(sseFrame('message_start', {
        type: 'message_start',
        message: {
          id: `msg_${name}_${state.hits}`,
          type: 'message',
          role: 'assistant',
          model: MODEL,
          content: [],
          stop_reason: null,
          usage: { input_tokens: 19, output_tokens: 0 },
        },
      }))
      response.write(sseFrame('content_block_start', {
        type: 'content_block_start',
        index: 0,
        content_block: { type: 'text', text: '' },
      }))
      response.write(sseFrame('content_block_delta', {
        type: 'content_block_delta',
        index: 0,
        delta: { type: 'text_delta', text: `${name}-partial` },
      }))
      response.write(sseFrame('error', {
        type: 'error',
        error: { type: 'api_error', message: `${name} post-output error` },
      }))
      response.end()
      return
    }

    if (behavior.type === 'mixed') {
      const slot = state.hits % 6
      if (slot === 0) {
        state.errors += 1
        json(response, 500, { type: 'error', error: { type: 'api_error', message: `${name} mixed 500` } })
        return
      }
      if (slot === 1) {
        state.errors += 1
        json(response, 429, { type: 'error', error: { type: 'rate_limit_error', message: `${name} mixed 429` } })
        return
      }
      if (slot === 2) {
        state.streamHits += 1
        state.errors += 1
        response.writeHead(200, {
          'content-type': 'text/event-stream',
          connection: 'close',
          'cache-control': 'no-cache',
        })
        response.write(sseFrame('message_start', {
          type: 'message_start',
          message: {
            id: `msg_${name}_${state.hits}`,
            type: 'message',
            role: 'assistant',
            model: MODEL,
            content: [],
            stop_reason: null,
            usage: { input_tokens: 19, output_tokens: 0 },
          },
        }))
        response.write(sseFrame('content_block_start', {
          type: 'content_block_start',
          index: 0,
          content_block: { type: 'text', text: '' },
        }))
        response.write(sseFrame('content_block_delta', {
          type: 'content_block_delta',
          index: 0,
          delta: { type: 'text_delta', text: `${name}-mixed-partial` },
        }))
        response.write(sseFrame('error', {
          type: 'error',
          error: { type: 'api_error', message: `${name} mixed post-output error` },
        }))
        response.end()
        return
      }
      if (body?.stream) await writeStreamSuccess(response, name, state, behavior)
      else await writeJsonSuccess(response, name, state, behavior)
      return
    }

    if (behavior.type === 'long_stream') {
      await writeStreamSuccess(response, name, state, {
        ...behavior,
        chunkCount: behavior.chunkCount ?? 20,
        chunkDelayMs: behavior.chunkDelayMs ?? 100,
      })
      return
    }

    if (behavior.type === 'stream_success') {
      await writeStreamSuccess(response, name, state, behavior)
      return
    }

    if (behavior.type === 'json_success') {
      await writeJsonSuccess(response, name, state, behavior)
      return
    }

    if (body?.stream) await writeStreamSuccess(response, name, state, behavior)
    else await writeJsonSuccess(response, name, state, behavior)
  })
  SERVERS.add(server)
  return {
    state,
    setBehavior(behavior) {
      state.behavior = { ...behavior }
    },
    snapshot() {
      return {
        hits: state.hits,
        errors: state.errors,
        streamHits: state.streamHits,
        nonStreamHits: state.nonStreamHits,
        requests: state.requests.length,
      }
    },
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
  const state = {
    hits: 0,
    inferenceHits: 0,
    businessHits: 0,
    errors: 0,
    activeBusinessInFlight: 0,
    behavior: { type: 'success' },
    markerHits: {},
    requests: [],
  }
  const server = http.createServer(async (request, response) => {
    const body = await readBody(request)
    const target = String(request.headers['x-amz-target'] || '')
    const isAuxiliary = target.endsWith('.ListAvailableModels')
      || String(request.url || '').toLowerCase().includes('listavailablemodels')
    const business = body.includes('external scheduler matrix')
    const marker = business ? extractMatrixMarker(body) : 'auxiliary'
    const behavior = { ...(state.behavior || { type: 'success' }) }
    const markerHits = (state.markerHits[marker] || 0) + 1
    state.markerHits[marker] = markerHits
    state.hits += 1
    if (!isAuxiliary) state.inferenceHits += 1
    if (business) state.businessHits += 1
    const record = {
      path: request.url,
      method: request.method,
      business,
      marker,
      auxiliary: isAuxiliary,
      body: body.slice(0, 200),
      status: null,
    }
    state.requests.push(record)

    if (isAuxiliary) {
      record.status = 200
      json(response, 200, {
        models: [{
          modelId: MODEL,
          modelName: 'external matrix local model',
          supportedInputTypes: ['TEXT'],
        }],
      })
      return
    }

    if (business) state.activeBusinessInFlight += 1
    try {
      const failing =
        behavior.type === 'json_error'
        || behavior.type === 'continuous_error'
        || (behavior.type === 'fail_once' && markerHits === 1)
      const delayMs = marker.includes('LOCAL-HOLDER')
        ? behavior.holderDelayMs ?? 450
        : behavior.delayMs ?? 0
      if (delayMs > 0) await sleep(delayMs)
      if (failing) {
        state.errors += 1
        const status = behavior.status || 500
        record.status = status
        json(response, status, {
          type: 'error',
          error: {
            type: status === 429 ? 'rate_limit_error' : status === 403 ? 'permission_error' : 'api_error',
            message: behavior.message || `local controlled ${status}`,
          },
        })
        return
      }

      const bodyBuffer = Buffer.concat([
        eventStreamFrame('assistantResponseEvent', {
          content: `local-ok ${marker}`,
          messageStatus: 'COMPLETED',
        }),
        eventStreamFrame('metadataEvent', {
          tokenUsage: {
            uncachedInputTokens: 19,
            cacheReadInputTokens: 0,
            cacheWriteInputTokens: 0,
            outputTokens: 5,
            totalTokens: 24,
          },
        }),
      ])
      record.status = 200
      response.writeHead(200, {
        'content-type': 'application/vnd.amazon.eventstream',
        'content-length': bodyBuffer.length,
        connection: 'close',
      })
      response.end(bodyBuffer)
    } finally {
      if (business) state.activeBusinessInFlight = Math.max(0, state.activeBusinessInFlight - 1)
    }
  })
  SERVERS.add(server)
  return {
    state,
    setBehavior(behavior) {
      state.behavior = { ...(behavior || { type: 'success' }) }
      state.markerHits = {}
    },
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

function requestTimed(url, options = {}) {
  const parsed = new URL(url)
  const started = performance.now()
  const textLimit = Number.isFinite(options.textLimit) ? options.textLimit : 4000
  return new Promise((resolve) => {
    let settled = false
    let responseStatus = null
    let responseHeaders = {}
    let firstChunkMs = null
    let firstTextMs = null
    let firstThinkingMs = null
    let sseBuffer = ''
    let bodyText = ''
    const eventTypes = {}
    let downstreamErrorEvents = 0

    const finish = (result) => {
      if (settled) return
      settled = true
      resolve({
        status: responseStatus ?? result.status ?? 'network_error',
        headers: responseHeaders,
        text: bodyText.slice(0, textLimit),
        totalMs: Number((performance.now() - started).toFixed(2)),
        ttfbMs: result.ttfbMs ?? null,
        firstChunkMs,
        firstTextMs,
        firstThinkingMs,
        eventTypes,
        downstreamErrorEvents,
        error: result.error || null,
        aborted: result.aborted || false,
      })
    }

    const noteEvent = (name) => {
      if (!name) return
      eventTypes[name] = (eventTypes[name] || 0) + 1
      if (name === 'error') downstreamErrorEvents += 1
    }

    const handleSseFrame = (frame) => {
      const lines = frame.split(/\r?\n/)
      const event = lines.find((line) => line.startsWith('event:'))?.slice(6).trim() || null
      const data = lines
        .filter((line) => line.startsWith('data:'))
        .map((line) => line.slice(5).trimStart())
        .join('\n')
      noteEvent(event)
      const parsedData = safeJsonParse(data)
      const now = Number((performance.now() - started).toFixed(2))
      const type = parsedData?.type || event
      if (type) noteEvent(type)
      const delta = parsedData?.delta
      if (firstTextMs === null && delta?.type === 'text_delta' && String(delta.text || '').length > 0) {
        firstTextMs = now
      }
      if (
        firstThinkingMs === null
        && (delta?.type === 'thinking_delta' || type === 'thinking_delta' || type === 'content_block_delta_thinking')
      ) {
        firstThinkingMs = now
      }
      if (type === 'error' || parsedData?.error) downstreamErrorEvents += 1
    }

    const feedSse = (chunk) => {
      sseBuffer += chunk
      for (;;) {
        const index = sseBuffer.indexOf('\n\n')
        if (index < 0) break
        const frame = sseBuffer.slice(0, index)
        sseBuffer = sseBuffer.slice(index + 2)
        handleSseFrame(frame)
      }
    }

    const request = http.request(parsed, {
      method: options.method || 'GET',
      headers: options.headers || {},
    }, (response) => {
      responseStatus = response.statusCode || 0
      responseHeaders = response.headers || {}
      const ttfbMs = Number((performance.now() - started).toFixed(2))
      response.on('data', (chunk) => {
        if (firstChunkMs === null) firstChunkMs = Number((performance.now() - started).toFixed(2))
        const text = chunk.toString('utf8')
        if (bodyText.length < 64_000) bodyText += text
        if (String(responseHeaders['content-type'] || '').includes('text/event-stream')) {
          feedSse(text)
        }
      })
      response.once('end', () => {
        if (sseBuffer.trim()) handleSseFrame(sseBuffer)
        finish({ ttfbMs })
      })
      response.once('aborted', () => finish({ ttfbMs, aborted: true, error: 'response_aborted' }))
      response.once('error', (error) => finish({ ttfbMs, error: String(error?.message || error) }))
    })
    request.setTimeout(options.timeoutMs || CLIENT_TIMEOUT_MS, () => {
      request.destroy(new Error('client_timeout'))
    })
    request.once('error', (error) => {
      finish({ status: error?.message === 'client_timeout' ? 'client_timeout' : 'network_error', error: String(error?.message || error) })
    })
    if (options.body !== undefined) request.write(options.body)
    request.end()
  })
}

function admin(baseUrl, pathName, options = {}) {
  return requestTimed(`${baseUrl}${pathName}`, {
    ...options,
    headers: {
      authorization: `Bearer ${ADMIN_KEY}`,
      'content-type': 'application/json',
      ...(options.headers || {}),
    },
  })
}

async function runtimeConfig(baseUrl) {
  const response = await admin(baseUrl, '/api/admin/config/runtime', { textLimit: 250_000 })
  assert.equal(response.status, 200, `runtime config read failed: ${response.text}`)
  return JSON.parse(response.text)
}

async function updateRuntimeExternalPools(baseUrl, overrides = {}) {
  const current = await runtimeConfig(baseUrl)
  const externalPools = {
    ...BASE_EXTERNAL_POOLS_CONFIG,
    ...(current.externalPools || {}),
    ...BASE_EXTERNAL_POOLS_CONFIG,
    ...overrides,
  }
  const response = await admin(baseUrl, '/api/admin/config/runtime', {
    method: 'PUT',
    textLimit: 1_000_000,
    body: JSON.stringify({
      ...current,
      externalPools,
    }),
  })
  assert.equal(response.status, 200, `runtime config update failed: ${response.text}`)
  return JSON.parse(response.text)
}

function requestBody(marker, stream) {
  return {
    model: MODEL,
    max_tokens: 128,
    stream,
    messages: [{ role: 'user', content: `external scheduler matrix ${marker}` }],
  }
}

function postMessage(baseUrl, marker, stream, options = {}) {
  return requestTimed(`${baseUrl}${ROUTE}`, {
    method: 'POST',
    headers: {
      'x-api-key': REQUEST_KEY,
      'content-type': 'application/json',
    },
    body: JSON.stringify(requestBody(marker, stream)),
    timeoutMs: options.timeoutMs || CLIENT_TIMEOUT_MS,
  })
}

function processMetrics(pid) {
  if (!pid) return null
  const ps = spawnSync('ps', ['-o', 'rss=,vsz=,%cpu=,etime=', '-p', String(pid)], {
    encoding: 'utf8',
    timeout: 3000,
  })
  if (ps.status !== 0) return null
  const fields = String(ps.stdout || '').trim().split(/\s+/)
  if (fields.length < 4) return null
  const [rssKib, vszKib, cpuPercent, elapsed] = fields
  const fd = spawnSync('sh', ['-c', `lsof -nP -p ${Number(pid)} 2>/dev/null | wc -l`], {
    encoding: 'utf8',
    timeout: 3000,
  })
  return {
    at: new Date().toISOString(),
    rssKib: Number(rssKib) || null,
    vszKib: Number(vszKib) || null,
    cpuPercent: Number(cpuPercent) || null,
    elapsed,
    fdCount: Number(fd.stdout.trim()) || null,
  }
}

function spawnService(configPath, credentialsPath, logPath) {
  const fd = fs.openSync(logPath, 'a')
  const child = spawn(BINARY, ['--config', configPath, '--credentials', credentialsPath], {
    cwd: TEMP_ROOT,
    env: validationChildEnvironment({
      RUST_LOG: 'kiro_rs::external_pool=debug,kiro_rs::anthropic=info,kiro_rs=info',
      KIRO_API_KEY: '',
    }),
    stdio: ['ignore', fd, fd],
    detached: true,
  })
  CHILDREN.add(child)
  child.once('exit', () => {
    try {
      fs.closeSync(fd)
    } catch {}
    CHILDREN.delete(child)
  })
  return child
}

async function stopChild(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return
  try {
    process.kill(-child.pid, 'SIGTERM')
  } catch {
    child.kill('SIGTERM')
  }
  const deadline = Date.now() + 10_000
  while (Date.now() < deadline && child.exitCode === null && child.signalCode === null) {
    await sleep(50)
  }
  if (child.exitCode === null && child.signalCode === null) {
    try {
      process.kill(-child.pid, 'SIGKILL')
    } catch {
      child.kill('SIGKILL')
    }
  }
  CHILDREN.delete(child)
}

async function waitForHealth(baseUrl, child) {
  const deadline = Date.now() + 60_000
  let last = ''
  while (Date.now() < deadline) {
    if (child.exitCode !== null) throw new Error(`service exited before health: ${child.exitCode}`)
    const response = await requestTimed(`${baseUrl}/healthz`, { timeoutMs: 1000 }).catch((error) => ({
      status: 'network_error',
      text: String(error?.message || error),
    }))
    last = `${response.status} ${response.text || ''}`
    if (response.status === 200) return
    await sleep(150)
  }
  throw new Error(`service health timeout: ${last}`)
}

function baseConfig(servicePort, localPort) {
  return {
    postgres: { url: POSTGRES_URL, maxConnections: 8, migrateOnStart: true },
    redis: { url: REDIS_URL, keyPrefix: REDIS_PREFIX },
    host: '127.0.0.1',
    port: servicePort,
    apiKey: REQUEST_KEY,
    adminApiKey: ADMIN_KEY,
    requestAdmission: { rpm: 0, maxConcurrentRequests: 0, maxQueuedRequests: 0, queueTimeoutMs: 0 },
    defaultEndpoint: 'ide',
    kiroUpstreamBaseUrl: `http://127.0.0.1:${localPort}/kiro`,
    kiroUpstreamResponseTimeoutSecs: 10,
    kiroUpstreamStreamIdleTimeoutSecs: 10,
    credentialRetryMaxAttempts: 1,
    inferenceUpstreamMaxAttempts: 8,
    credentialWarmupRequests: 0,
    credentialMaxConcurrentRequests: 1,
    externalPools: BASE_EXTERNAL_POOLS_CONFIG,
  }
}

function credentials() {
  return [{
    accessToken: 'external-matrix-local-token',
    expiresAt: '2099-01-01T00:00:00Z',
    authMethod: 'social',
    endpoint: 'ide',
    priority: 0,
    maxConcurrentRequests: 1,
    rpm: 0,
    supportedModels: [MODEL],
    disabled: false,
  }]
}

async function createPools(baseUrl, ports) {
  const created = {}
  for (const [name, priority] of [['primary', 1], ['backup_a', 10], ['backup_b', 20]]) {
    const response = await admin(baseUrl, '/api/admin/external-pools', {
      method: 'POST',
      body: JSON.stringify({
        name,
        baseUrl: `http://127.0.0.1:${ports[name]}/anthropic`,
        apiKey: `sk-${name}-matrix`,
        authType: 'bearer',
        enabled: true,
        priority,
        maxConcurrentRequests: BASE_POOL_CONFIG.maxConcurrentRequests,
        usageProjectionMode: 'pass_through',
        requestBodyMode: 'normalized',
        rawModelMode: 'none',
        autoDisablePolicy: 'inherit',
        preOutputStreamRetryMode: 'inherit',
        preservePath: true,
        normalizeModelVersionDots: false,
        modelMappingMode: 'processed_mapping',
        modelMappingRequireMatch: false,
        modelMappingRules: [],
        supportedModels: [MODEL],
        routeMode: 'allow_all',
        routeRules: [],
        notes: `${RUN_ID}-${name}`,
      }),
    })
    assert.equal(response.status, 200, `${name} create failed: ${response.text}`)
    created[name] = JSON.parse(response.text)
  }
  await sleep(1500)
  return created
}

async function clearPoolState(baseUrl, pools) {
  for (const pool of Object.values(pools)) {
    await admin(baseUrl, `/api/admin/external-pools/${pool.id}/enabled`, {
      method: 'POST',
      body: JSON.stringify({ enabled: true }),
    })
    await admin(baseUrl, `/api/admin/external-pools/${pool.id}/auto-disabled/clear`, { method: 'POST' })
    await admin(baseUrl, `/api/admin/external-pools/${pool.id}/cooldown/clear`, { method: 'POST' })
  }
}

async function updatePoolScenarioConfig(baseUrl, pools, scenario) {
  for (const [name, pool] of Object.entries(pools)) {
    const defaults = {
      priority: name === 'primary' ? 1 : name === 'backup_a' ? 10 : 20,
      ...BASE_POOL_CONFIG,
    }
    const override = scenario.poolOverrides?.[name] || {}
    const response = await admin(baseUrl, `/api/admin/external-pools/${pool.id}`, {
      method: 'PUT',
      body: JSON.stringify({
        ...defaults,
        ...override,
        notes: `${RUN_ID}-${scenario.id}-${name}`,
      }),
    })
    assert.equal(response.status, 200, `${scenario.id} ${name} update failed: ${response.text}`)
    pools[name] = JSON.parse(response.text)
  }
}

function metricSummary(values) {
  const sorted = values.filter((value) => Number.isFinite(value)).sort((a, b) => a - b)
  if (sorted.length === 0) return { count: 0, p50: null, p95: null, p99: null, max: null, min: null }
  const pick = (p) => sorted[Math.min(sorted.length - 1, Math.floor((sorted.length - 1) * p))]
  return {
    count: sorted.length,
    min: sorted[0],
    p50: pick(0.5),
    p95: pick(0.95),
    p99: pick(0.99),
    max: sorted[sorted.length - 1],
  }
}

function summarizeResponses(responses) {
  const statusCounts = {}
  const ttfb = []
  const firstText = []
  const total = []
  let downstreamErrorEvents = 0
  for (const response of responses) {
    statusCounts[String(response.status)] = (statusCounts[String(response.status)] || 0) + 1
    if (typeof response.ttfbMs === 'number') ttfb.push(response.ttfbMs)
    if (typeof response.firstTextMs === 'number') firstText.push(response.firstTextMs)
    if (typeof response.totalMs === 'number') total.push(response.totalMs)
    downstreamErrorEvents += response.downstreamErrorEvents || 0
  }
  return {
    statusCounts,
    ttfb: metricSummary(ttfb),
    firstText: metricSummary(firstText),
    total: metricSummary(total),
    downstreamErrorEvents,
  }
}

async function runScenario(baseUrl, scenario, upstreams, localUpstream, servicePid, pools) {
  await updateRuntimeExternalPools(baseUrl, scenario.externalPoolsOverrides || {})
  if (scenario.clearState !== false) {
    await clearPoolState(baseUrl, pools)
  }
  await updatePoolScenarioConfig(baseUrl, pools, scenario)
  for (const name of MOCK_NAMES) {
    upstreams[name].setBehavior(scenario.behaviors?.[name] || BEHAVIOR.success)
  }
  localUpstream.setBehavior({
    ...(scenario.localBehavior || { type: 'success' }),
    holderDelayMs: scenario.localHolder?.delayMs,
  })
  if (scenario.waitBeforeMs) await sleep(scenario.waitBeforeMs)
  else await sleep(500)

  let localHolder = null
  if (scenario.localHolder) {
    const holderTimeoutMs = Math.max(
      CLIENT_TIMEOUT_MS,
      (scenario.localHolder.delayMs || 0) + ((scenario.requestCount || REQUESTS_PER_SCENARIO) * 750) + 10_000,
    )
    localHolder = postMessage(baseUrl, `${scenario.id}-LOCAL-HOLDER`, false, {
      timeoutMs: holderTimeoutMs,
    })
    await waitForCondition(
      () => localUpstream.state.activeBusinessInFlight > 0,
      `${scenario.id} local holder in-flight`,
      5000,
    )
  }

  const hitsBefore = Object.fromEntries(MOCK_NAMES.map((name) => [name, upstreams[name].state.hits]))
  const localBefore = localUpstream.state.businessHits
  const resources = []
  const startMetrics = processMetrics(servicePid)
  if (startMetrics) resources.push({ label: 'start', ...startMetrics })

  const requestCount = scenario.requestCount
  const concurrency = Math.max(1, scenario.concurrency || MAX_CONCURRENCY)
  const rpm = Math.max(1, scenario.rpm || TARGET_RPM)
  const intervalMs = scenario.durationSecs ? Math.max(5, Math.floor(60_000 / rpm)) : 0
  const deadline = scenario.durationSecs ? Date.now() + scenario.durationSecs * 1000 : null
  const inFlight = new Set()
  const tasks = []
  let sent = 0
  let nextAt = Date.now()

  const launch = (index) => {
    const task = postMessage(baseUrl, `${scenario.id}-${index}`, scenario.stream)
      .finally(() => inFlight.delete(task))
    inFlight.add(task)
    tasks.push(task)
  }

  if (deadline) {
    while (Date.now() < deadline && sent < requestCount) {
      const now = Date.now()
      if (now >= nextAt && inFlight.size < concurrency) {
        launch(sent)
        sent += 1
        nextAt = now + intervalMs
      } else {
        await sleep(5)
      }
      if (resources.length < 12) {
        const sample = processMetrics(servicePid)
        if (sample) resources.push({ label: `sample-${resources.length}`, ...sample })
      }
    }
  } else {
    while (sent < requestCount) {
      while (inFlight.size >= concurrency) {
        await Promise.race([...inFlight])
      }
      launch(sent)
      sent += 1
      if (resources.length < 12) {
        const sample = processMetrics(servicePid)
        if (sample) resources.push({ label: `sample-${resources.length}`, ...sample })
      }
    }
  }

  const responses = await Promise.all(tasks)
  while (inFlight.size > 0) await Promise.race([...inFlight])
  const localHolderResponse = localHolder ? await localHolder : null
  const endMetrics = processMetrics(servicePid)
  if (endMetrics) resources.push({ label: 'end', ...endMetrics })

  const hitsAfter = Object.fromEntries(MOCK_NAMES.map((name) => [name, upstreams[name].state.hits]))
  const upstreamHits = Object.fromEntries(
    MOCK_NAMES.map((name) => [name, hitsAfter[name] - hitsBefore[name]]),
  )
  const statusResponse = await admin(baseUrl, '/api/admin/external-pools/status')
  assert.equal(statusResponse.status, 200, `${scenario.id} status failed: ${statusResponse.text}`)
  const poolStatus = JSON.parse(statusResponse.text)
  const summary = summarizeResponses(responses)
  const localHits = localUpstream.state.businessHits - localBefore
  const report = {
    id: scenario.id,
    stream: scenario.stream,
    requestCount,
    sent,
    completed: responses.length,
    concurrency,
    rpm: scenario.durationSecs ? rpm : null,
    durationSecs: scenario.durationSecs || null,
    upstreamHits,
    localHits,
    summary,
    responses: responses.map((response) => ({
      status: response.status,
      totalMs: response.totalMs,
      ttfbMs: response.ttfbMs,
      firstTextMs: response.firstTextMs,
      downstreamErrorEvents: response.downstreamErrorEvents,
      aborted: response.aborted,
      error: response.error,
      text: redact(response.text || '').slice(0, 500),
    })),
    localHolder: localHolderResponse ? {
      status: localHolderResponse.status,
      totalMs: localHolderResponse.totalMs,
      text: redact(localHolderResponse.text || '').slice(0, 500),
    } : null,
    poolStatus,
    resources,
  }
  lastScenarioReport = report
  evaluateScenario(report, scenario)
  return report
}

function evaluateScenario(report, scenario) {
  const expect = scenario.expect || {}
  const statusCounts = report.summary.statusCounts
  const ok = statusCounts['200'] || 0
  const totalResponses = report.completed
  const backupHits = (report.upstreamHits.backup_a || 0) + (report.upstreamHits.backup_b || 0)
  const totalExternalHits = Object.values(report.upstreamHits).reduce((sum, value) => sum + value, 0)
  const poolStatusList = Array.isArray(report.poolStatus)
    ? report.poolStatus
    : Array.isArray(report.poolStatus?.pools)
      ? report.poolStatus.pools
      : []

  if (scenario.localHolder) {
    assert.equal(report.localHolder?.status, 200, `${scenario.id} local holder failed: ${JSON.stringify(report.localHolder)}`)
  }
  if (expect.noLocal) {
    assert.equal(report.localHits, 0, `${scenario.id} unexpectedly used local credential path`)
  }
  if (expect.localHitsMin !== undefined) {
    assert.ok(report.localHits >= expect.localHitsMin, `${scenario.id} local hits too low: ${report.localHits}`)
  }
  if (expect.localHitsMinRequests !== undefined) {
    const minHits = Math.ceil(report.requestCount * expect.localHitsMinRequests)
    assert.ok(report.localHits >= minHits, `${scenario.id} local hits too low: ${report.localHits}, min=${minHits}`)
  }
  if (expect.localHitsMax !== undefined) {
    assert.ok(report.localHits <= expect.localHitsMax, `${scenario.id} local hits too high: ${report.localHits}`)
  }
  if (expect.totalExternalHitsMax !== undefined) {
    assert.ok(
      totalExternalHits <= expect.totalExternalHitsMax,
      `${scenario.id} external hits too high: ${JSON.stringify(report.upstreamHits)}`,
    )
  }
  if (expect.all200) {
    assert.equal(ok, totalResponses, `${scenario.id} expected all 200, got ${JSON.stringify(statusCounts)}`)
  }
  if (expect.no200) {
    assert.equal(ok, 0, `${scenario.id} expected no 200, got ${JSON.stringify(statusCounts)}`)
  }
  if (expect.successMinRatio !== undefined) {
    assert.ok(
      ok >= Math.ceil(totalResponses * expect.successMinRatio),
      `${scenario.id} success ratio too low: ok=${ok} total=${totalResponses} statuses=${JSON.stringify(statusCounts)}`,
    )
  }
  if (expect.primaryHitsMin !== undefined) {
    assert.ok(report.upstreamHits.primary >= expect.primaryHitsMin, `${scenario.id} primary hits too low: ${JSON.stringify(report.upstreamHits)}`)
  }
  if (expect.primaryHitsMax !== undefined) {
    assert.ok(report.upstreamHits.primary <= expect.primaryHitsMax, `${scenario.id} primary hits too high: ${JSON.stringify(report.upstreamHits)}`)
  }
  if (expect.primaryHitsMaxMultiplier !== undefined) {
    const max = Math.ceil(totalResponses * expect.primaryHitsMaxMultiplier)
    assert.ok(report.upstreamHits.primary <= max, `${scenario.id} primary kept receiving traffic: hits=${report.upstreamHits.primary}, max=${max}`)
  }
  if (expect.backupHitsMin !== undefined) {
    assert.ok(backupHits >= expect.backupHitsMin, `${scenario.id} backup did not take over: ${JSON.stringify(report.upstreamHits)}`)
  }
  if (expect.backupHitsMax !== undefined) {
    assert.ok(backupHits <= expect.backupHitsMax, `${scenario.id} backup was used after downstream output: ${JSON.stringify(report.upstreamHits)}`)
  }
  if (expect.downstreamErrorEventMin !== undefined) {
    assert.ok(
      report.summary.downstreamErrorEvents >= expect.downstreamErrorEventMin,
      `${scenario.id} expected downstream error events: ${JSON.stringify(report.summary)}`,
    )
  }
  if (expect.firstTextP95MinMs !== undefined) {
    assert.ok(
      report.summary.firstText.p95 >= expect.firstTextP95MinMs,
      `${scenario.id} first text p95 too low for slow-first-byte case: ${JSON.stringify(report.summary.firstText)}`,
    )
  }
  if (expect.primaryAutoDisabled) {
    const primary = poolStatusList.find((item) => item.pool?.name === 'primary')
    assert.equal(primary?.pool?.autoDisabled, true, `${scenario.id} primary was not auto-disabled: ${JSON.stringify(primary)}`)
  }
}

function readTail(file, bytes = 24_000) {
  try {
    const stat = fs.statSync(file)
    const fd = fs.openSync(file, 'r')
    const size = Math.min(bytes, stat.size)
    const buffer = Buffer.alloc(size)
    fs.readSync(fd, buffer, 0, size, Math.max(0, stat.size - size))
    fs.closeSync(fd)
    return buffer.toString('utf8')
  } catch {
    return ''
  }
}

async function main() {
  const storage = validateStorage()
  fs.mkdirSync(REPORT_ROOT, { recursive: true, mode: 0o700 })
  fs.mkdirSync(TEMP_ROOT, { recursive: true, mode: 0o700 })

  const servicePort = await reservePort()
  const localPort = await reservePort()
  const upstreamPorts = {
    primary: await reservePort(),
    backup_a: await reservePort(),
    backup_b: await reservePort(),
  }
  const logPath = path.join(TEMP_ROOT, 'service.log')
  const configPath = path.join(TEMP_ROOT, 'config.json')
  const credentialsPath = path.join(TEMP_ROOT, 'credentials.json')

  const upstreams = {
    primary: createMockUpstream('primary', BEHAVIOR.success),
    backup_a: createMockUpstream('backup_a', BEHAVIOR.success),
    backup_b: createMockUpstream('backup_b', BEHAVIOR.success),
  }
  const localUpstream = createLocalUpstream()

  await Promise.all([
    upstreams.primary.listen(upstreamPorts.primary),
    upstreams.backup_a.listen(upstreamPorts.backup_a),
    upstreams.backup_b.listen(upstreamPorts.backup_b),
    localUpstream.listen(localPort),
  ])

  const config = baseConfig(servicePort, localPort)
  fs.writeFileSync(configPath, `${JSON.stringify(config, null, 2)}\n`, { mode: 0o600 })
  fs.writeFileSync(credentialsPath, `${JSON.stringify(credentials(), null, 2)}\n`, { mode: 0o600 })

  const service = spawnService(configPath, credentialsPath, logPath)
  const baseUrl = `http://127.0.0.1:${servicePort}`
  const scenarioReports = []
  let pools = null

  try {
    await waitForHealth(baseUrl, service)
    pools = await createPools(baseUrl, upstreamPorts)
    const poolStatusBefore = await admin(baseUrl, '/api/admin/external-pools/status')
    assert.equal(poolStatusBefore.status, 200, poolStatusBefore.text)

    const selectedScenarios = SCENARIO_FILTER.size > 0
      ? SCENARIOS.filter((scenario) => SCENARIO_FILTER.has(scenario.id))
      : SCENARIOS
    assert.ok(selectedScenarios.length > 0, 'no matrix scenarios selected')

    for (const scenario of selectedScenarios) {
      const started = performance.now()
      process.stderr.write(`matrix scenario start ${scenario.id}\n`)
      const report = await runScenario(baseUrl, scenario, upstreams, localUpstream, service.pid, pools)
      report.elapsedMs = Number((performance.now() - started).toFixed(2))
      scenarioReports.push(report)
      process.stderr.write(`matrix scenario pass ${scenario.id} ${JSON.stringify({
        statuses: report.summary.statusCounts,
        hits: report.upstreamHits,
        localHits: report.localHits,
      })}\n`)
    }

    const report = {
      schemaVersion: 1,
      result: 'pass',
      runId: RUN_ID,
      generatedAt: new Date().toISOString(),
      binarySha256: sha256(fs.readFileSync(BINARY)),
      storage,
      config: {
        model: MODEL,
        route: ROUTE,
        routeRule: ROUTE_RULE,
        requestsPerScenario: REQUESTS_PER_SCENARIO,
        maxConcurrency: MAX_CONCURRENCY,
        rpm: TARGET_RPM,
        scenarioFilter: [...SCENARIO_FILTER],
      },
      upstreamTotals: Object.fromEntries(
        Object.entries(upstreams).map(([name, upstream]) => [name, {
          hits: upstream.state.hits,
          errors: upstream.state.errors,
          streamHits: upstream.state.streamHits,
          nonStreamHits: upstream.state.nonStreamHits,
          behaviorHits: upstream.state.behaviorHits,
          requestCount: upstream.state.requests.length,
          requestIds: upstream.state.requestIds.slice(-20),
          authHeaders: upstream.state.authHeaders.slice(-5),
        }]),
      ),
      local: {
        hits: localUpstream.state.hits,
        inferenceHits: localUpstream.state.inferenceHits,
        businessHits: localUpstream.state.businessHits,
      },
      scenarios: scenarioReports,
      tempRoot: KEEP_TEMP ? TEMP_ROOT : null,
    }
    fs.writeFileSync(REPORT_PATH, `${JSON.stringify(report, null, 2)}\n`, { mode: 0o600 })
    process.stdout.write(`${REPORT_PATH}\n`)
  } catch (error) {
    const failure = {
      schemaVersion: 1,
      result: 'fail',
      runId: RUN_ID,
      generatedAt: new Date().toISOString(),
      error: redact(error?.stack || error?.message || error),
      binarySha256: fs.existsSync(BINARY) ? sha256(fs.readFileSync(BINARY)) : null,
      scenarioReports,
      lastScenarioReport,
      serviceLogTail: redact(readTail(logPath)),
      tempRoot: KEEP_TEMP ? TEMP_ROOT : null,
    }
    fs.mkdirSync(REPORT_ROOT, { recursive: true, mode: 0o700 })
    fs.writeFileSync(REPORT_PATH, `${JSON.stringify(failure, null, 2)}\n`, { mode: 0o600 })
    process.stderr.write(`external pool scheduler matrix failed: ${redact(error?.stack || error?.message || error)}\n`)
    process.stderr.write(`report=${REPORT_PATH}\n`)
    throw error
  } finally {
    await stopChild(service).catch(() => {})
    await Promise.all([...SERVERS].map((server) => new Promise((resolve) => server.close(resolve))))
    if (!KEEP_TEMP) {
      fs.rmSync(TEMP_ROOT, { recursive: true, force: true })
    } else {
      process.stderr.write(`kept validation temp root: ${TEMP_ROOT}\n`)
    }
  }
}

main().catch((error) => {
  process.stderr.write(`external pool scheduler matrix failed: ${redact(error?.message || error)}\n`)
  process.exitCode = 1
})
