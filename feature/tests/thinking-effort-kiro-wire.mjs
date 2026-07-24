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

const ROOT = path.resolve(import.meta.dirname, '../..')
const REPO_ROOT_REAL = fs.realpathSync(ROOT)
const FIXTURE_MODE = process.env.KIRO_THINKING_WIRE_FIXTURE_MODE
  || (process.env.KIRO_THINKING_WIRE_SIGNAL_FIXTURE === '1' ? 'signal_idle' : '')
const FIXTURE_MODES = new Set([
  'contract',
  'signal_idle',
  'signal_race',
  'cleanup_error',
  'cleanup_timeout',
  'redis_error',
  'redis_timeout',
  'command_timeout',
  'command_spawn_error',
  'server_socket_hang',
  'startup_error',
])
if (FIXTURE_MODE && !FIXTURE_MODES.has(FIXTURE_MODE)) {
  throw new Error(`unsupported KIRO_THINKING_WIRE_FIXTURE_MODE: ${FIXTURE_MODE}`)
}
const IS_FIXTURE = FIXTURE_MODE !== ''
const SIGNAL_FIXTURE_MODE = FIXTURE_MODE.startsWith('signal_')
  || FIXTURE_MODE.startsWith('cleanup_')
  || FIXTURE_MODE.startsWith('redis_')
  || FIXTURE_MODE.startsWith('command_')
  || FIXTURE_MODE === 'server_socket_hang'
  || FIXTURE_MODE === 'startup_error'
const runtimePaths = IS_FIXTURE ? null : resolveRuntimeValidationPaths(ROOT)
const BINARY = runtimePaths?.binary || ''
const ARTIFACT_ROOT = runtimePaths?.artifactRoot || ''
if (!IS_FIXTURE) fs.accessSync(BINARY, fs.constants.X_OK)
const ROUNDS = IS_FIXTURE
  ? 5
  : Number.parseInt(process.env.KIRO_THINKING_WIRE_ROUNDS || '5', 10)
const ENDPOINTS = ['cli', 'ide']
const EFFORTS = ['absent', 'low', 'medium', 'high', 'xhigh', 'max']
const FAKE_MODEL_IDS = ['claude-opus-4-8', 'claude-opus-4.8']
const FAKE_EFFORT_SCHEMA = Object.freeze({
  path: 'output_config.effort',
  values: ['low', 'medium', 'high', 'xhigh', 'max'],
  default: 'high',
})
const EXPECTED_CLAUDE_VERSION = process.env.KIRO_EXPECTED_CLAUDE_VERSION || '2.1.197'
if (!/^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/.test(EXPECTED_CLAUDE_VERSION)) {
  throw new Error('KIRO_EXPECTED_CLAUDE_VERSION must be a version identifier')
}
const RUN_ID = `thinking-effort-wire-${Date.now()}-${process.pid}-${crypto.randomBytes(3).toString('hex')}`
const REQUEST_KEY = `sk-request-${RUN_ID}`
const ADMIN_KEY = `sk-admin-${RUN_ID}`
const CREDENTIALS = Object.fromEntries(ENDPOINTS.map((endpoint) => [
  endpoint,
  `ksk_${endpoint}_${crypto.randomBytes(24).toString('hex')}`,
]))
const REDIS_PREFIXES = Object.fromEntries(ENDPOINTS.map((endpoint) => [
  endpoint,
  `kiro_rs:validation:${RUN_ID}:${endpoint}`,
]))
const REDIS_FOREIGN_SENTINEL = `kiro_rs:validation-foreign:${RUN_ID}`
const REDIS_FOREIGN_VALUE = crypto.randomBytes(24).toString('hex')
const MAX_CAPTURE_BYTES = 16 * 1024 * 1024
const MAX_COMMAND_OUTPUT_BYTES = 8 * 1024 * 1024
const COMMAND_TIMEOUT_MS = 15_000
const PROCESS_PROBE_TIMEOUT_MS = 2_000
const PROCESS_TERM_TIMEOUT_MS = 8_000
const PROCESS_KILL_TIMEOUT_MS = 4_000
const MAX_SERVICE_RSS_GROWTH_KB = 128 * 1024
const MAX_SERVICE_PEAK_RSS_KB = 1024 * 1024
const MAX_SERVICE_FD_GROWTH = 32
const MAX_SERVICE_PEAK_FDS = 512
const MAX_RUNNER_RSS_GROWTH_BYTES = 128 * 1024 * 1024
const MAX_RUNNER_HEAP_GROWTH_BYTES = 64 * 1024 * 1024
const MAX_WALL_DURATION_MS = parseOptionalIntegerEnvironment(
  'KIRO_THINKING_WIRE_MAX_WALL_MS',
  20 * 60 * 1000,
  60_000,
  60 * 60 * 1000,
)
const PROGRESS_LOGGING_ENABLED = process.env.KIRO_VALIDATION_PROGRESS === '1'
const SOURCE_MANIFEST_PATHS = [
  'Cargo.toml',
  'Cargo.lock',
  'src',
  'data',
  'admin-ui/dist',
  'ui/dist',
]
const SOURCE_MANIFEST_EXCLUDES = [
  ':(exclude,icase,glob)**/kiro_idc_users*.txt',
]
const ACTIVE_CHILDREN = new Set()
const ACTIVE_SERVERS = new Set()
const FORBIDDEN_PORTS = new Set([9022])
const ALLOCATED_PORTS = new Set()
let activeCase = null
let cleanupPromise = null
let shutdownExitCode = null
let fixtureBlockedSpawnAttempts = 0
let fixtureKillEscalations = 0
let fixtureRedisState = null
let redisForeignSentinelSeeded = false
let redisForeignSentinelIntegrityFailed = false
let REDIS_URL = IS_FIXTURE
  ? ''
  : requiredEnvironment('KIRO_THINKING_WIRE_REDIS_URL')

if (ROUNDS !== 5) {
  throw new Error('KIRO_THINKING_WIRE_ROUNDS must be exactly 5 for the A09/D07 wire gate')
}

const DATABASE_OWNER = IS_FIXTURE
  ? 'fixture_owner'
  : requiredEnvironment('KIRO_THINKING_WIRE_DATABASE_OWNER')
if (!/^[a-z0-9_]{8,36}$/.test(DATABASE_OWNER)) {
  throw new Error('KIRO_THINKING_WIRE_DATABASE_OWNER must match ^[a-z0-9_]{8,36}$')
}

const POSTGRES_URLS = IS_FIXTURE
  ? { cli: 'postgres://signal-fixture.invalid/cli', ide: 'postgres://signal-fixture.invalid/ide' }
  : {
      cli: requiredEnvironment('KIRO_THINKING_WIRE_CLI_POSTGRES_URL'),
      ide: requiredEnvironment('KIRO_THINKING_WIRE_IDE_POSTGRES_URL'),
    }

const POSTGRES_IDENTITIES = IS_FIXTURE
  ? {
      cli: { database: 'fixture_cli', marker: 'fixture-cli' },
      ide: { database: 'fixture_ide', marker: 'fixture-ide' },
    }
  : validateIsolationUrls(POSTGRES_URLS, REDIS_URL, DATABASE_OWNER)

const CLAUDE = IS_FIXTURE
  ? ''
  : resolveExecutable(process.env.KIRO_CLAUDE_BINARY || 'claude')
const PSQL = IS_FIXTURE
  ? ''
  : resolveExecutable(process.env.KIRO_PSQL_BINARY || 'psql')
const TEMP_ROOT = SIGNAL_FIXTURE_MODE
  ? fs.mkdtempSync(path.join(os.tmpdir(), `${RUN_ID}-`))
  : IS_FIXTURE
    ? ''
    : path.join(ARTIFACT_ROOT, 'runtime', 'thinking-effort-wire', RUN_ID)
const REPORT_ROOT = IS_FIXTURE
  ? ''
  : path.join(ARTIFACT_ROOT, 'reports', 'thinking-effort-wire')
const REPORT_PATH = IS_FIXTURE ? '' : path.join(REPORT_ROOT, `${RUN_ID}.json`)

class ShutdownRequested extends Error {}

function throwIfShutdownRequested() {
  if (shutdownExitCode !== null) throw new ShutdownRequested('validation shutdown requested')
}

function requiredEnvironment(name) {
  const value = String(process.env[name] || '').trim()
  if (!value) throw new Error(`${name} is required`)
  return value
}

function parseOptionalIntegerEnvironment(name, fallback, minimum, maximum) {
  const raw = process.env[name]
  if (raw === undefined || raw === '') return fallback
  const value = Number.parseInt(raw, 10)
  if (!Number.isSafeInteger(value) || value < minimum || value > maximum) {
    throw new Error(`${name} must be an integer in ${minimum}..${maximum}`)
  }
  return value
}

function progressLog(event) {
  if (!PROGRESS_LOGGING_ENABLED) return
  console.error(JSON.stringify({
    schemaVersion: 1,
    kind: 'thinking_effort_wire_progress',
    timestamp: new Date().toISOString(),
    ...event,
  }))
}

function pathIsWithin(parent, candidate) {
  const relative = path.relative(parent, candidate)
  return relative === '' || (
    relative !== '..'
    && !relative.startsWith(`..${path.sep}`)
    && !path.isAbsolute(relative)
  )
}

function assertExternalDirectory(directory, label) {
  const realPath = fs.realpathSync(directory)
  if (pathIsWithin(REPO_ROOT_REAL, realPath)) {
    throw new Error(`${label} must resolve outside the repository`)
  }
  return realPath
}

function isLoopbackHost(hostname) {
  return ['127.0.0.1', 'localhost', '::1', '[::1]'].includes(hostname.toLowerCase())
}

function postgresOwnerMarker(owner, endpoint) {
  return `kiro-thinking-wire-owner:${owner}:${endpoint}`
}

function validateIsolationUrls(postgresUrls, redisUrl, owner) {
  const parsedPostgres = Object.entries(postgresUrls).map(([endpoint, value]) => {
    let parsed
    try {
      parsed = new URL(value)
    } catch {
      throw new Error(`KIRO_THINKING_WIRE_${endpoint.toUpperCase()}_POSTGRES_URL must be a valid URL`)
    }
    if (!['postgres:', 'postgresql:'].includes(parsed.protocol)) {
      throw new Error(`KIRO_THINKING_WIRE_${endpoint.toUpperCase()}_POSTGRES_URL must use PostgreSQL`)
    }
    if (!isLoopbackHost(parsed.hostname)) {
      throw new Error(`KIRO_THINKING_WIRE_${endpoint.toUpperCase()}_POSTGRES_URL must target loopback`)
    }
    if (parsed.hash) {
      throw new Error(`KIRO_THINKING_WIRE_${endpoint.toUpperCase()}_POSTGRES_URL must not contain a fragment`)
    }
    for (const name of parsed.searchParams.keys()) {
      if (name !== 'sslmode') {
        throw new Error(
          `KIRO_THINKING_WIRE_${endpoint.toUpperCase()}_POSTGRES_URL contains unsupported query parameter ${name}`,
        )
      }
    }
    const database = decodeURIComponent(parsed.pathname.replace(/^\//, ''))
    const expectedDatabase = `kiro_thinking_wire_${owner}_${endpoint}`
    if (database !== expectedDatabase) {
      throw new Error(
        `KIRO_THINKING_WIRE_${endpoint.toUpperCase()}_POSTGRES_URL must name ${expectedDatabase}`,
      )
    }
    return {
      endpoint,
      parsed,
      database,
      marker: postgresOwnerMarker(owner, endpoint),
    }
  })
  if (parsedPostgres[0].database === parsedPostgres[1].database) {
    throw new Error('CLI and IDE must use two different caller-owned empty PostgreSQL databases')
  }

  let parsedRedis
  try {
    parsedRedis = new URL(redisUrl)
  } catch {
    throw new Error('KIRO_THINKING_WIRE_REDIS_URL must be a valid URL')
  }
  if (parsedRedis.protocol !== 'redis:') {
    throw new Error('KIRO_THINKING_WIRE_REDIS_URL must use plain Redis on isolated loopback')
  }
  if (!isLoopbackHost(parsedRedis.hostname)) {
    throw new Error('KIRO_THINKING_WIRE_REDIS_URL must target an isolated loopback Redis')
  }
  if (parsedRedis.username && !parsedRedis.password) {
    throw new Error('KIRO_THINKING_WIRE_REDIS_URL username requires a password')
  }
  if (parsedRedis.search || parsedRedis.hash) {
    throw new Error('KIRO_THINKING_WIRE_REDIS_URL must not contain query parameters or a fragment')
  }
  const redisDatabase = parsedRedis.pathname.replace(/^\//, '') || '0'
  if (!/^\d+$/.test(redisDatabase)) {
    throw new Error('KIRO_THINKING_WIRE_REDIS_URL must contain a numeric database')
  }

  return Object.fromEntries(parsedPostgres.map((entry) => [entry.endpoint, entry]))
}

function resolveExecutable(command) {
  if (path.isAbsolute(command)) {
    fs.accessSync(command, fs.constants.X_OK)
    return fs.realpathSync(command)
  }
  const candidates = [
    process.env.VOLTA_HOME ? path.join(process.env.VOLTA_HOME, 'bin', command) : null,
    path.join(os.homedir(), '.volta', 'bin', command),
    ...String(process.env.PATH || '')
      .split(path.delimiter)
      .filter(Boolean)
      .map((directory) => path.join(directory, command)),
  ].filter(Boolean)
  for (const candidate of candidates) {
    try {
      fs.accessSync(candidate, fs.constants.X_OK)
      return fs.realpathSync(candidate)
    } catch {}
  }
  throw new Error(`unable to resolve Claude binary: ${command}`)
}

function sha256(value) {
  return crypto.createHash('sha256').update(value).digest('hex')
}

async function sha256File(file) {
  const hash = crypto.createHash('sha256')
  for await (const chunk of fs.createReadStream(file)) hash.update(chunk)
  return hash.digest('hex')
}

async function frozenExecutableIdentity(file) {
  if (!path.isAbsolute(file) || fs.realpathSync(file) !== file) {
    throw new Error('runtime executable must be an absolute canonical path')
  }
  fs.accessSync(file, fs.constants.X_OK)
  const stat = fs.statSync(file, { bigint: true })
  if (!stat.isFile()) throw new Error('runtime executable must remain a regular file')
  return {
    canonicalPathSha256: sha256(file),
    device: String(stat.dev),
    inode: String(stat.ino),
    size: String(stat.size),
    mode: String(stat.mode),
    mtimeNs: String(stat.mtimeNs),
    ctimeNs: String(stat.ctimeNs),
    sha256: await sha256File(file),
  }
}

function sameFrozenExecutable(left, right) {
  return JSON.stringify(left) === JSON.stringify(right)
}

function assertReportRedacted(serialized, forbiddenValues) {
  for (const rawValue of forbiddenValues) {
    const value = String(rawValue || '')
    if (!value) continue
    const encoded = encodeURIComponent(value)
    if (serialized.includes(value) || (encoded !== value && serialized.includes(encoded))) {
      throw new Error('report redaction gate rejected a sensitive value')
    }
  }
  if (/(?:https?|postgres(?:ql)?|redis|file)(?::|%3a)(?:\/\/|%2f%2f)/i.test(serialized)) {
    throw new Error('report redaction gate rejected a URL')
  }
  if (/(?:^|[^A-Za-z0-9])(?:sk-[A-Za-z0-9_-]{8,}|ksk_[A-Za-z0-9_-]{8,})(?:$|[^A-Za-z0-9])/m.test(serialized)) {
    throw new Error('report redaction gate rejected an API key')
  }
}

async function workingTreeSourceManifest() {
  const pathspec = ['--', ...SOURCE_MANIFEST_PATHS, ...SOURCE_MANIFEST_EXCLUDES]
  const [head, trackedDiffSha256, stagedDiffSha256, unstagedDiffSha256, status] = await Promise.all([
    commandOutput('git', ['rev-parse', 'HEAD']),
    commandSha256('git', [
      'diff', 'HEAD', '--binary', '--no-ext-diff', '--no-textconv', ...pathspec,
    ]),
    commandSha256('git', [
      'diff', '--cached', '--binary', '--no-ext-diff', '--no-textconv', ...pathspec,
    ]),
    commandSha256('git', [
      'diff', '--binary', '--no-ext-diff', '--no-textconv', ...pathspec,
    ]),
    commandOutput('git', [
      'status', '--porcelain=v1', '-z', '--untracked-files=no', ...pathspec,
    ]),
  ])
  return {
    gitHead: head,
    dirty: status.length > 0,
    statusSha256: sha256(status),
    trackedDiffSha256,
    stagedDiffSha256,
    unstagedDiffSha256,
    scope: {
      trackedOnly: true,
      paths: [...SOURCE_MANIFEST_PATHS],
      protectedCredentialFilesExcluded: true,
      untrackedFilesEnumerated: false,
    },
  }
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

async function waitForOwnedChildExit(child, timeoutMs) {
  if (child.exitCode !== null || child.signalCode !== null) {
    return {
      code: child.exitCode,
      signal: child.signalCode,
      error: null,
      timedOut: false,
    }
  }
  return await new Promise((resolve) => {
    let settled = false
    let timer = null
    const onError = (error) => finish({ code: null, signal: null, error, timedOut: false })
    const onClose = (code, signal) => finish({ code, signal, error: null, timedOut: false })
    const finish = (value) => {
      if (settled) return
      settled = true
      if (timer) clearTimeout(timer)
      child.off('error', onError)
      child.off('close', onClose)
      resolve(value)
    }
    child.once('error', onError)
    child.once('close', onClose)
    timer = setTimeout(() => finish({
      code: child.exitCode,
      signal: child.signalCode,
      error: null,
      timedOut: true,
    }), timeoutMs)
    timer.unref()
  })
}

async function waitForOwnedChildClose(child, timeoutMs) {
  if (child.ownedClosed) return true
  return await new Promise((resolve) => {
    let timer = null
    const finish = (closed) => {
      if (timer) clearTimeout(timer)
      child.off('close', onClose)
      resolve(closed)
    }
    const onClose = () => finish(true)
    child.once('close', onClose)
    timer = setTimeout(() => finish(false), timeoutMs)
    timer.unref()
  })
}

async function settleWithin(promise, timeoutMs, timeoutMessage) {
  let timer = null
  try {
    return await new Promise((resolve, reject) => {
      timer = setTimeout(() => reject(new Error(timeoutMessage)), timeoutMs)
      timer.unref()
      Promise.resolve(promise).then(resolve, reject)
    })
  } finally {
    if (timer) clearTimeout(timer)
  }
}

function boundedSpawnSync(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: options.cwd || ROOT,
    env: options.env || minimalEnvironment(),
    encoding: 'utf8',
    maxBuffer: options.maxBuffer || MAX_COMMAND_OUTPUT_BYTES,
    timeout: options.timeout || PROCESS_PROBE_TIMEOUT_MS,
  })
  if (result.error) {
    throw new Error(`${path.basename(command)} failed or timed out`)
  }
  return result
}

function processStartIdentity(pid) {
  if (!Number.isInteger(pid) || pid <= 0) return null
  const result = boundedSpawnSync('ps', ['-o', 'lstart=', '-p', String(pid)])
  if (result.status !== 0) return null
  const value = String(result.stdout || '').trim().replace(/\s+/g, ' ')
  return value || null
}

function processGroupMembers(pgid) {
  if (!Number.isInteger(pgid) || pgid <= 0) return []
  const result = boundedSpawnSync('ps', ['-axo', 'pid=,pgid=,lstart='])
  if (result.status !== 0) throw new Error(`failed to inspect process group ${pgid}`)
  const members = []
  for (const line of String(result.stdout || '').split(/\r?\n/)) {
    const match = line.trim().match(/^(\d+)\s+(\d+)\s+(.+)$/)
    if (!match || Number(match[2]) !== pgid) continue
    members.push({
      pid: Number(match[1]),
      pgid: Number(match[2]),
      startIdentity: match[3].trim().replace(/\s+/g, ' '),
    })
  }
  return members
}

function trackOwnedChild(child, label) {
  child.ownedLabel = label
  child.ownedPgid = Number.isInteger(child.pid) ? child.pid : null
  child.ownedStartIdentity = Number.isInteger(child.pid) ? processStartIdentity(child.pid) : null
  child.ownedClosed = false
  ACTIVE_CHILDREN.add(child)
  child.once('close', () => {
    child.ownedClosed = true
  })
  return child
}

function spawnOwned(command, args, options, label) {
  throwIfShutdownRequested()
  if (options?.detached !== true) {
    throw new Error('owned child processes must start in an isolated process group')
  }
  return trackOwnedChild(spawn(command, args, options), label)
}

function validateOwnedGroupSnapshot(child, currentLeaderIdentity, members) {
  const pgid = child?.ownedPgid
  if (!Number.isInteger(pgid)) return { pgid: null, members: [] }
  if (currentLeaderIdentity && !child.ownedStartIdentity) {
    throw new Error(`refusing to signal process group ${pgid} without a start identity`)
  }
  if (
    currentLeaderIdentity
    && child.ownedStartIdentity
    && currentLeaderIdentity !== child.ownedStartIdentity
  ) {
    throw new Error(`refusing to signal reused process group ${pgid}`)
  }
  const currentLeader = members.find((member) => member.pid === pgid)
  if (
    currentLeader
    && child.ownedStartIdentity
    && currentLeader.startIdentity !== child.ownedStartIdentity
  ) {
    throw new Error(`refusing to signal reused process group ${pgid}`)
  }
  return { pgid, members }
}

function assertOwnedGroupIdentity(child) {
  const pgid = child?.ownedPgid
  if (!Number.isInteger(pgid)) return { pgid: null, members: [] }
  const members = processGroupMembers(pgid)
  const currentLeaderIdentity = members.find((member) => member.pid === pgid)?.startIdentity || null
  return validateOwnedGroupSnapshot(child, currentLeaderIdentity, members)
}

async function waitForOwnedGroupEmpty(child, timeoutMs) {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    const state = assertOwnedGroupIdentity(child)
    if (state.members.length === 0) return true
    await new Promise((resolve) => setTimeout(resolve, 50))
  }
  return assertOwnedGroupIdentity(child).members.length === 0
}

function signalOwnedGroup(child, signal) {
  const state = assertOwnedGroupIdentity(child)
  if (!state.pgid || state.members.length === 0) return false
  process.kill(-state.pgid, signal)
  return true
}

async function commandOutput(command, args, timeoutMs = COMMAND_TIMEOUT_MS, options = {}) {
  const stdout = []
  const stderr = []
  let captured = 0
  const child = spawnOwned(command, args, {
    cwd: options.cwd || ROOT,
    env: options.env || minimalEnvironment(),
    detached: true,
    stdio: ['ignore', 'pipe', 'pipe'],
  }, `command:${path.basename(command)}`)
  const capture = (target) => (chunk) => {
    captured += chunk.length
    if (captured <= MAX_COMMAND_OUTPUT_BYTES) target.push(chunk)
  }
  child.stdout.on('data', capture(stdout))
  child.stderr.on('data', capture(stderr))
  const result = await waitForOwnedChildExit(child, timeoutMs)
  await stopChild(child, {
    termTimeoutMs: result.timedOut ? 100 : 1_000,
    killTimeoutMs: PROCESS_KILL_TIMEOUT_MS,
  })
  if (captured > MAX_COMMAND_OUTPUT_BYTES) throw new Error(`${path.basename(command)} output overflow`)
  if (result.timedOut || result.error || result.code !== 0) {
    throw new Error(`${path.basename(command)} failed or timed out`)
  }
  return Buffer.concat(stdout).toString('utf8').trim()
}

function postgresCommandEnvironment(urlValue, applicationName) {
  const parsed = new URL(urlValue)
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

async function inspectPostgresIdentity(endpoint, requireEmpty) {
  const identity = POSTGRES_IDENTITIES[endpoint]
  const sql = [
    "SELECT current_database(),",
    "COALESCE(shobj_description(oid, 'pg_database'), ''),",
    "(SELECT COUNT(*) FROM information_schema.tables",
    " WHERE table_schema NOT IN ('pg_catalog', 'information_schema'))",
    'FROM pg_database WHERE datname = current_database();',
  ].join(' ')
  const output = await commandOutput(PSQL, [
    '-X',
    '-A',
    '-t',
    '-F',
    '\t',
    '-v',
    'ON_ERROR_STOP=1',
    '-c',
    sql,
  ], COMMAND_TIMEOUT_MS, {
    cwd: TEMP_ROOT,
    env: postgresCommandEnvironment(
      POSTGRES_URLS[endpoint],
      `kiro_thinking_wire_${DATABASE_OWNER}_${endpoint}`,
    ),
  })
  const rows = output.split(/\r?\n/).filter(Boolean)
  if (rows.length !== 1) throw new Error(`${endpoint} PostgreSQL identity query returned ${rows.length} rows`)
  const [database, marker, tableCountText] = rows[0].split('\t')
  const tableCount = Number.parseInt(tableCountText, 10)
  if (database !== identity.database) throw new Error(`${endpoint} PostgreSQL database identity mismatch`)
  if (marker !== identity.marker) throw new Error(`${endpoint} PostgreSQL owner marker mismatch`)
  if (!Number.isInteger(tableCount) || tableCount < 0) {
    throw new Error(`${endpoint} PostgreSQL table count was invalid`)
  }
  if (requireEmpty && tableCount !== 0) {
    throw new Error(`${endpoint} PostgreSQL database is not empty`)
  }
  if (!requireEmpty && tableCount === 0) {
    throw new Error(`${endpoint} PostgreSQL database was not migrated by the isolated service`)
  }
  return {
    database,
    ownerMarkerSha256: sha256(marker),
    tableCount,
    emptyRequired: requireEmpty,
  }
}

async function commandSha256(command, args, timeoutMs = 60_000) {
  const hash = crypto.createHash('sha256')
  const child = spawnOwned(command, args, {
    cwd: ROOT,
    env: minimalEnvironment(),
    detached: true,
    stdio: ['ignore', 'pipe', 'ignore'],
  }, `hash:${path.basename(command)}`)
  child.stdout.on('data', (chunk) => hash.update(chunk))
  const result = await waitForOwnedChildExit(child, timeoutMs)
  await stopChild(child, { termTimeoutMs: 2_000, killTimeoutMs: PROCESS_KILL_TIMEOUT_MS })
  if (result.timedOut || result.error || result.code !== 0) {
    throw new Error(`${path.basename(command)} hash command failed`)
  }
  return hash.digest('hex')
}

function listeningPids(port) {
  const result = boundedSpawnSync('lsof', ['-nP', `-iTCP:${port}`, '-sTCP:LISTEN', '-t'], {
    maxBuffer: 1024 * 1024,
  })
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
    throwIfShutdownRequested()
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
    if (!FORBIDDEN_PORTS.has(port) && !ALLOCATED_PORTS.has(port)) {
      ALLOCATED_PORTS.add(port)
      return port
    }
  }
}

async function waitForHealth(baseUrl, child, timeoutMs = 60_000) {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    throwIfShutdownRequested()
    if (child.spawnError) {
      throw new Error('failed to start isolated kiro-rs service')
    }
    if (child.exitCode !== null || child.signalCode !== null) {
      throw new Error(`kiro-rs exited before health check for isolated ${baseUrl}`)
    }
    try {
      const response = await fetch(`${baseUrl}/healthz`, { signal: AbortSignal.timeout(1_000) })
      if (response.ok) return
    } catch {}
    await new Promise((resolve) => setTimeout(resolve, 100))
  }
  throw new Error('timed out waiting for isolated kiro-rs health check')
}

async function stopChild(child, options = {}) {
  if (!child) return { stopped: true, members: [] }
  if (!Number.isInteger(child.ownedPgid)) {
    ACTIVE_CHILDREN.delete(child)
    return { stopped: true, members: [] }
  }

  const initial = assertOwnedGroupIdentity(child)
  if (initial.members.length > 0) {
    try { signalOwnedGroup(child, 'SIGTERM') } catch (error) {
      if (error?.code !== 'ESRCH') throw error
    }
  }
  const termTimeoutMs = FIXTURE_MODE === 'cleanup_timeout'
    ? 300
    : options.termTimeoutMs ?? PROCESS_TERM_TIMEOUT_MS
  const killTimeoutMs = FIXTURE_MODE === 'cleanup_timeout'
    ? 1_000
    : options.killTimeoutMs ?? PROCESS_KILL_TIMEOUT_MS
  let stopped = await waitForOwnedGroupEmpty(child, termTimeoutMs)
  if (!stopped) {
    if (IS_FIXTURE) fixtureKillEscalations += 1
    try { signalOwnedGroup(child, 'SIGKILL') } catch (error) {
      if (error?.code !== 'ESRCH') throw error
    }
    stopped = await waitForOwnedGroupEmpty(child, killTimeoutMs)
  }
  const remaining = assertOwnedGroupIdentity(child).members
  if (!stopped || remaining.length > 0) {
    throw new Error(
      `owned process group ${child.ownedPgid} (${child.ownedLabel || 'child'}) did not stop`,
    )
  }
  if (!child.ownedClosed) {
    const closed = await waitForOwnedChildClose(child, 2_000)
    if (!closed || !child.ownedClosed) {
      throw new Error(`owned child ${child.ownedLabel || child.ownedPgid} did not close stdio`)
    }
  }
  ACTIVE_CHILDREN.delete(child)
  return { stopped: true, members: [] }
}

function processResources(pid) {
  const ps = boundedSpawnSync('ps', ['-o', 'rss=', '-p', String(pid)])
  if (ps.status !== 0 || ps.error) throw new Error('failed to sample isolated service RSS')
  const rssKb = Number.parseInt(String(ps.stdout || '').trim(), 10) || 0
  const lsof = boundedSpawnSync('lsof', ['-nP', '-a', '-p', String(pid), '-F', 'f'], {
    maxBuffer: 1024 * 1024,
  })
  if (lsof.status !== 0 || lsof.error) throw new Error('failed to sample isolated service FDs')
  const fdCount = String(lsof.stdout || '').split('\n').filter((line) => line.startsWith('f')).length
  return { rssKb, fdCount }
}

function registerServer(name, server) {
  throwIfShutdownRequested()
  if ('requestTimeout' in server) server.requestTimeout = 30_000
  if ('headersTimeout' in server) server.headersTimeout = 15_000
  if ('keepAliveTimeout' in server) server.keepAliveTimeout = 1_000
  const sockets = new Set()
  const onConnection = (socket) => {
    sockets.add(socket)
    socket.once('close', () => sockets.delete(socket))
  }
  server.on('connection', onConnection)
  const tracked = {
    name,
    server,
    sockets,
    async close() {
      const closePromise = server.listening
        ? new Promise((resolve, reject) => {
            server.close((error) => error ? reject(error) : resolve())
          })
        : Promise.resolve()
      const socketCloses = [...sockets].map((socket) => new Promise((resolve) => {
        if (!sockets.has(socket)) {
          resolve()
          return
        }
        socket.once('close', resolve)
        socket.destroy()
      }))
      if (typeof server.closeAllConnections === 'function') server.closeAllConnections()
      await settleWithin(
        Promise.all([closePromise, ...socketCloses]),
        2_000,
        `owned server ${name} close timed out`,
      )
      server.off('connection', onConnection)
    },
  }
  ACTIVE_SERVERS.add(tracked)
  return tracked
}

async function closeTrackedServer(tracked) {
  if (!tracked) return
  await tracked.close()
  if (tracked.server.listening) {
    throw new Error(`owned server ${tracked.name} remained listening`)
  }
  if (tracked.sockets.size > 0) {
    throw new Error(`owned server ${tracked.name} retained active sockets`)
  }
  ACTIVE_SERVERS.delete(tracked)
}

async function collectBody(request, limit = MAX_CAPTURE_BYTES) {
  const chunks = []
  let bytes = 0
  for await (const chunk of request) {
    bytes += chunk.length
    if (bytes > limit) throw new Error('isolated request exceeded capture bound')
    chunks.push(chunk)
  }
  return Buffer.concat(chunks)
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

function writeEventStream(response, includeReasoning) {
  const frames = [
    eventFrame('assistantResponseEvent', {
      content: 'wire-ok',
      messageStatus: 'COMPLETED',
    }),
    eventFrame('metadataEvent', {
      tokenUsage: {
        uncachedInputTokens: 24,
        cacheReadInputTokens: 0,
        cacheWriteInputTokens: 0,
        outputTokens: 8,
        totalTokens: 32,
      },
    }),
  ]
  if (includeReasoning) {
    frames.unshift(eventFrame('reasoningContentEvent', { text: 'isolated wire reasoning' }))
  }
  const body = Buffer.concat(frames)
  response.writeHead(200, {
    'content-type': 'application/vnd.amazon.eventstream',
    'content-length': body.length,
    connection: 'close',
  })
  response.end(body)
}

function modelDiscoveryResponse() {
  return {
    models: FAKE_MODEL_IDS.map((modelId) => ({
      modelId,
      modelName: `Thinking wire fixture ${modelId}`,
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
                enum: [...FAKE_EFFORT_SCHEMA.values],
                default: FAKE_EFFORT_SCHEMA.default,
              },
            },
          },
        },
      },
    })),
    nextToken: null,
  }
}

function selectedWireFacts(bodyBuffer, request, url, detectedEndpoint) {
  const body = JSON.parse(bodyBuffer.toString('utf8'))
  const current = body?.conversationState?.currentMessage?.userInputMessage || {}
  const fields = body?.additionalModelRequestFields ?? null
  const authorization = String(request.headers.authorization || '')
  return {
    endpoint: detectedEndpoint,
    model: current.modelId ?? null,
    origin: current.origin ?? null,
    additionalModelRequestFields: fields,
    topLevelKeys: body && typeof body === 'object' && !Array.isArray(body)
      ? Object.keys(body).sort()
      : [],
    bodyBytes: bodyBuffer.length,
    bodySha256: sha256(bodyBuffer),
    method: request.method || '',
    path: url.pathname,
    target: String(request.headers['x-amz-target'] || ''),
    logicalHost: String(request.headers.host || ''),
    contentType: String(request.headers['content-type'] || '').split(';', 1)[0].trim().toLowerCase(),
    authorizationBearer: authorization.startsWith('Bearer '),
    tokenType: String(request.headers.tokentype || request.headers.TokenType || ''),
  }
}

function exactProtocolViolations({ endpoint, kind, request, url, body }) {
  const violations = []
  const target = String(request.headers['x-amz-target'] || '')
  const host = String(request.headers.host || '')
  const tokenType = String(request.headers.tokentype || request.headers.TokenType || '')
  const contentType = String(request.headers['content-type'] || '')
    .split(';', 1)[0]
    .trim()
    .toLowerCase()
  const expectedHost = endpoint === 'cli'
    ? (kind === 'model_discovery' ? 'management.us-east-1.kiro.dev' : 'runtime.us-east-1.kiro.dev')
    : 'q.us-east-1.amazonaws.com'

  if (request.method !== (endpoint === 'ide' && kind === 'model_discovery' ? 'GET' : 'POST')) {
    violations.push('method')
  }
  if (host !== expectedHost) violations.push('host')
  if (tokenType !== 'API_KEY') violations.push('token_type')

  if (kind === 'model_discovery' && endpoint === 'cli') {
    if (url.pathname !== '/thinking-wire/') violations.push('path')
    if (target !== 'AmazonCodeWhispererService.ListAvailableModels') violations.push('target')
    if (contentType !== 'application/x-amz-json-1.0') violations.push('content_type')
    if (String(request.headers.accept || '') !== '*/*') violations.push('accept')
    if (url.searchParams.get('origin') !== 'KIRO_CLI') violations.push('query_origin')
    if (body?.origin !== 'KIRO_CLI') violations.push('body_origin')
  } else if (kind === 'model_discovery') {
    if (url.pathname !== '/thinking-wire/ListAvailableModels') violations.push('path')
    if (target !== '') violations.push('target')
    if (String(request.headers.accept || '') !== 'application/json') violations.push('accept')
    if (url.searchParams.get('origin') !== 'AI_EDITOR') violations.push('query_origin')
    if (url.searchParams.get('maxResults') !== '50') violations.push('max_results')
  } else if (endpoint === 'cli') {
    if (url.pathname !== '/thinking-wire/') violations.push('path')
    if (target !== 'AmazonCodeWhispererStreamingService.GenerateAssistantResponse') {
      violations.push('target')
    }
    if (contentType !== 'application/x-amz-json-1.0') violations.push('content_type')
    const origin = body?.conversationState?.currentMessage?.userInputMessage?.origin
    if (origin !== 'KIRO_CLI') violations.push('origin')
  } else {
    if (url.pathname !== '/thinking-wire/generateAssistantResponse') violations.push('path')
    if (target !== '') violations.push('target')
    if (contentType !== 'application/json') violations.push('content_type')
    const origin = body?.conversationState?.currentMessage?.userInputMessage?.origin
    if (origin !== 'AI_EDITOR') violations.push('origin')
  }
  return violations
}

function schemaEffortForWire(body) {
  const value = body?.additionalModelRequestFields?.output_config?.effort
  if (typeof value !== 'string') return null
  const normalized = value.trim().toLowerCase()
  return FAKE_EFFORT_SCHEMA.values.includes(normalized) ? normalized : null
}

function createFakeKiroUpstream() {
  const records = []
  const server = http.createServer((request, response) => {
    void (async () => {
      const bodyBuffer = await collectBody(request)
      const target = String(request.headers['x-amz-target'] || '')
      const url = new URL(request.url || '/', 'http://127.0.0.1')
      const authorization = String(request.headers.authorization || '')
      const bearer = authorization.startsWith('Bearer ') ? authorization.slice(7) : ''
      const credentialEndpoint = ENDPOINTS.find((endpoint) => CREDENTIALS[endpoint] === bearer) || null
      const modelDiscoveryEndpoint = target.endsWith('.ListAvailableModels')
        ? 'cli'
        : url.pathname.endsWith('/ListAvailableModels')
          ? 'ide'
          : null

      if (modelDiscoveryEndpoint) {
        let body = null
        if (bodyBuffer.length > 0) {
          try { body = JSON.parse(bodyBuffer.toString('utf8')) } catch {}
        }
        const protocolViolations = exactProtocolViolations({
          endpoint: modelDiscoveryEndpoint,
          kind: 'model_discovery',
          request,
          url,
          body,
        })
        records.push({
          kind: 'model_discovery',
          credentialEndpoint,
          transportEndpoint: modelDiscoveryEndpoint,
          protocolViolations,
          schemaPath: FAKE_EFFORT_SCHEMA.path,
          schemaValues: [...FAKE_EFFORT_SCHEMA.values],
        })
        if (
          !credentialEndpoint
          || credentialEndpoint !== modelDiscoveryEndpoint
          || protocolViolations.length > 0
        ) {
          writeJson(response, 401, { message: 'isolated model discovery endpoint mismatch' })
          return
        }
        writeJson(response, 200, modelDiscoveryResponse())
        return
      }
      if (url.pathname.endsWith('/getUsageLimits')) {
        const protocolViolations = []
        if (request.method !== 'GET') protocolViolations.push('method')
        if (url.pathname !== '/thinking-wire/getUsageLimits') protocolViolations.push('path')
        if (url.searchParams.get('origin') !== 'AI_EDITOR') protocolViolations.push('query_origin')
        if (url.searchParams.get('resourceType') !== 'AGENTIC_REQUEST') {
          protocolViolations.push('resource_type')
        }
        if (String(request.headers.host || '') !== 'q.us-east-1.amazonaws.com') {
          protocolViolations.push('host')
        }
        if (String(request.headers.tokentype || '') !== 'API_KEY') {
          protocolViolations.push('token_type')
        }
        records.push({ kind: 'balance', credentialEndpoint, protocolViolations })
        if (!credentialEndpoint || protocolViolations.length > 0) {
          writeJson(response, 400, { message: 'isolated balance protocol mismatch' })
          return
        }
        writeJson(response, 200, {
          subscriptionInfo: {
            subscriptionTitle: 'THINKING WIRE FIXTURE',
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

      const detectedEndpoint = target.endsWith('.GenerateAssistantResponse')
        ? 'cli'
        : url.pathname.endsWith('/generateAssistantResponse')
          ? 'ide'
          : 'unknown'
      if (!activeCase || detectedEndpoint === 'unknown') {
        records.push({ kind: 'unknown', caseId: activeCase?.id || null })
        writeJson(response, 404, { message: 'unsupported isolated fixture request' })
        return
      }

      let facts
      let parsedBody
      try {
        parsedBody = JSON.parse(bodyBuffer.toString('utf8'))
        facts = selectedWireFacts(bodyBuffer, request, url, detectedEndpoint)
      } catch {
        records.push({ kind: 'invalid_json', caseId: activeCase.id, bodySha256: sha256(bodyBuffer) })
        writeJson(response, 400, { message: 'invalid isolated wire body' })
        return
      }
      facts.protocolViolations = exactProtocolViolations({
        endpoint: detectedEndpoint,
        kind: 'inference',
        request,
        url,
        body: parsedBody,
      })
      facts.credentialMatchesEndpoint = credentialEndpoint === activeCase.endpoint
      records.push({ kind: 'inference', caseId: activeCase.id, facts })
      if (
        detectedEndpoint !== activeCase.endpoint
        || !facts.credentialMatchesEndpoint
        || facts.protocolViolations.length > 0
      ) {
        writeJson(response, 400, { message: 'isolated endpoint assertion failed' })
        return
      }
      writeEventStream(response, schemaEffortForWire(parsedBody) !== null)
    })().catch(() => {
      if (!response.headersSent) writeJson(response, 500, { message: 'isolated fake failure' })
      else response.destroy()
    })
  })
  const tracked = registerServer('fake-kiro', server)
  return {
    records,
    tracked,
    async listen(port) {
      throwIfShutdownRequested()
      await new Promise((resolve, reject) => {
        server.once('error', reject)
        server.listen(port, '127.0.0.1', resolve)
      })
    },
  }
}

function selectedInboundFacts(bodyBuffer, request) {
  const body = JSON.parse(bodyBuffer.toString('utf8'))
  return {
    model: body?.model ?? null,
    stream: body?.stream ?? null,
    thinking: body?.thinking ?? null,
    outputConfig: body?.output_config ?? null,
    topLevelKeys: body && typeof body === 'object' && !Array.isArray(body)
      ? Object.keys(body).sort()
      : [],
    contentEncoding: String(request.headers['content-encoding'] || ''),
    requestApiKeyPresent: Boolean(request.headers['x-api-key'] || request.headers.authorization),
    bodyBytes: bodyBuffer.length,
    bodySha256: sha256(bodyBuffer),
  }
}

function forwardedHeaders(headers, bodyLength) {
  const forwarded = { ...headers }
  for (const name of [
    'host',
    'connection',
    'content-length',
    'transfer-encoding',
    'proxy-authorization',
    'upgrade',
  ]) delete forwarded[name]
  forwarded['content-length'] = String(bodyLength)
  forwarded.connection = 'close'
  return forwarded
}

function responseHeaders(headers) {
  const result = { ...headers }
  for (const name of ['connection', 'content-length', 'transfer-encoding', 'keep-alive', 'upgrade']) {
    delete result[name]
  }
  return result
}

function createIngressProxy(servicePort, endpoint) {
  const records = []
  const server = http.createServer((request, response) => {
    void (async () => {
      const bodyBuffer = await collectBody(request)
      const url = new URL(request.url || '/', 'http://127.0.0.1')
      const kind = url.pathname.endsWith('/v1/messages/count_tokens')
        ? 'count_tokens'
        : url.pathname.endsWith('/v1/messages')
          ? 'messages'
          : request.method === 'HEAD' && url.pathname === '/cc'
            ? 'cc_head_probe'
            : 'other'
      if (kind === 'messages') {
        try {
          records.push({
            kind,
            endpoint,
            caseId: activeCase?.id || null,
            facts: selectedInboundFacts(bodyBuffer, request),
          })
        } catch {
          records.push({ kind: 'invalid_json', endpoint, caseId: activeCase?.id || null })
        }
      } else {
        records.push({
          kind,
          endpoint,
          caseId: activeCase?.id || null,
          method: request.method,
          path: url.pathname,
        })
      }

      const upstream = http.request({
        host: '127.0.0.1',
        port: servicePort,
        method: request.method,
        path: request.url,
        headers: forwardedHeaders(request.headers, bodyBuffer.length),
      }, (upstreamResponse) => {
        response.writeHead(
          upstreamResponse.statusCode || 502,
          responseHeaders(upstreamResponse.headers),
        )
        upstreamResponse.pipe(response)
      })
      upstream.once('error', () => {
        if (!response.headersSent) response.writeHead(502, { connection: 'close' })
        response.end()
      })
      upstream.setTimeout(15_000, () => upstream.destroy(new Error('isolated ingress timeout')))
      upstream.end(bodyBuffer)
    })().catch(() => {
      if (!response.headersSent) response.writeHead(500, { connection: 'close' })
      response.end()
    })
  })
  const tracked = registerServer(`ingress-${endpoint}`, server)
  return {
    records,
    tracked,
    async listen(port) {
      throwIfShutdownRequested()
      await new Promise((resolve, reject) => {
        server.once('error', reject)
        server.listen(port, '127.0.0.1', resolve)
      })
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

function parseRedisRequest(buffer, offset = 0) {
  if (offset >= buffer.length || buffer[offset] !== 42) return null
  const lineEnd = buffer.indexOf('\r\n', offset + 1)
  if (lineEnd < 0) return null
  const count = Number(buffer.subarray(offset + 1, lineEnd).toString())
  if (!Number.isInteger(count) || count < 0) throw new Error('invalid fake Redis array count')
  let cursor = lineEnd + 2
  const values = []
  for (let index = 0; index < count; index += 1) {
    if (cursor >= buffer.length || buffer[cursor] !== 36) return null
    const lengthEnd = buffer.indexOf('\r\n', cursor + 1)
    if (lengthEnd < 0) return null
    const length = Number(buffer.subarray(cursor + 1, lengthEnd).toString())
    if (!Number.isInteger(length) || length < 0) throw new Error('invalid fake Redis bulk length')
    const start = lengthEnd + 2
    const end = start + length
    if (buffer.length < end + 2) return null
    values.push(buffer.subarray(start, end).toString())
    cursor = end + 2
  }
  return { values, next: cursor }
}

function encodeRedisResponse(value) {
  if (value === null) return Buffer.from('$-1\r\n')
  if (typeof value === 'number') return Buffer.from(`:${value}\r\n`)
  if (Array.isArray(value)) {
    return Buffer.concat([
      Buffer.from(`*${value.length}\r\n`),
      ...value.map(encodeRedisResponse),
    ])
  }
  return Buffer.from(`$${Buffer.byteLength(String(value))}\r\n${String(value)}\r\n`)
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
    const socket = net.connect({
      host: url.hostname.replace(/^\[|\]$/g, ''),
      port: Number(url.port || 6379),
    })
    const chunks = []
    socket.setTimeout(FIXTURE_MODE === 'redis_timeout' ? 250 : 5_000)
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
          reject(new Error('Redis cleanup command failed'))
          return
        }
        values.push(parsed.value)
        cursor = parsed.next
      }
      socket.end()
      resolve(values.slice(values.length - commands.length))
    })
    socket.once('timeout', () => {
      socket.destroy()
      reject(new Error('Redis cleanup timed out'))
    })
    socket.once('error', reject)
  })
}

async function seedRedisForeignSentinel() {
  const values = await redisPipeline(REDIS_URL, [
    ['SET', REDIS_FOREIGN_SENTINEL, REDIS_FOREIGN_VALUE, 'NX'],
  ])
  if (values.length !== 1 || values[0] !== 'OK') {
    throw new Error('Redis foreign sentinel already existed or could not be created')
  }
  redisForeignSentinelSeeded = true
  redisForeignSentinelIntegrityFailed = false
}

function recordRedisForeignSentinelIntegrity(preserved, removed) {
  if (!preserved || !removed) redisForeignSentinelIntegrityFailed = true
  return !redisForeignSentinelIntegrityFailed
}

async function verifyAndRemoveRedisForeignSentinel() {
  if (!redisForeignSentinelSeeded) {
    const intact = !redisForeignSentinelIntegrityFailed
    return { seeded: false, preserved: intact, removed: intact }
  }
  const [value] = await redisPipeline(REDIS_URL, [['GET', REDIS_FOREIGN_SENTINEL]])
  const preserved = value === REDIS_FOREIGN_VALUE
  const [removed] = await redisPipeline(REDIS_URL, [['DEL', REDIS_FOREIGN_SENTINEL]])
  const [after] = await redisPipeline(REDIS_URL, [['GET', REDIS_FOREIGN_SENTINEL]])
  redisForeignSentinelSeeded = false
  const removedCleanly = removed === 1 && after === null
  const intact = recordRedisForeignSentinelIntegrity(preserved, removedCleanly)
  return { seeded: true, preserved: preserved && intact, removed: removedCleanly && intact }
}

async function removeOwnedRedisKeys(maxPasses = 4) {
  if (!REDIS_URL) return { complete: false, removed: 0, passes: 0, prefixes: 0 }
  const script = `
    local cursor = ARGV[2]
    local removed = 0
    if cursor == '0' then
      removed = removed + redis.call('DEL', ARGV[3])
    end
    for page = 1, 64 do
      local result = redis.call('SCAN', cursor, 'MATCH', ARGV[1], 'COUNT', 200)
      cursor = result[1]
      local keys = result[2]
      if #keys > 0 then
        removed = removed + redis.call('DEL', unpack(keys))
      end
      if cursor == '0' then break end
    end
    return {removed, cursor}
  `
  const cursors = Object.fromEntries(ENDPOINTS.map((endpoint) => [endpoint, '0']))
  let removed = 0
  for (let pass = 1; pass <= maxPasses; pass += 1) {
    const pending = ENDPOINTS.filter((endpoint) => cursors[endpoint] !== null)
    if (pending.length === 0) {
      return { complete: true, removed, passes: pass - 1, prefixes: ENDPOINTS.length }
    }
    const values = await redisPipeline(REDIS_URL, pending.map((endpoint) => [
      'EVAL',
      script,
      0,
      `${REDIS_PREFIXES[endpoint]}:*`,
      cursors[endpoint],
      REDIS_PREFIXES[endpoint],
    ]))
    for (let index = 0; index < pending.length; index += 1) {
      const result = values[index]
      if (!Array.isArray(result) || result.length !== 2) {
        throw new Error('Redis cleanup returned an invalid bounded-scan result')
      }
      removed += Number(result[0] || 0)
      const cursor = String(result[1])
      cursors[pending[index]] = cursor === '0' ? null : cursor
    }
  }
  return {
    complete: ENDPOINTS.every((endpoint) => cursors[endpoint] === null),
    removed,
    passes: maxPasses,
    prefixes: ENDPOINTS.length,
  }
}

async function cleanupOwnedRuntime({ redisPasses = 4 } = {}) {
  if (cleanupPromise) return cleanupPromise
  const running = (async () => {
    if (FIXTURE_MODE === 'signal_race') {
      await new Promise((resolve) => setTimeout(resolve, 30))
    }
    const childErrors = []
    for (let pass = 0; pass < 3 && ACTIVE_CHILDREN.size > 0; pass += 1) {
      const results = await Promise.allSettled([...ACTIVE_CHILDREN].map((child) => stopChild(child)))
      for (const result of results) {
        if (result.status === 'rejected') childErrors.push(result.reason?.message || 'child cleanup failed')
      }
    }
    let redis = null
    let redisAttempts = 0
    for (let attempt = 1; attempt <= 3; attempt += 1) {
      redisAttempts = attempt
      redis = await removeOwnedRedisKeys(redisPasses).catch(() => ({
        complete: false,
        removed: 0,
        passes: 0,
        prefixes: ENDPOINTS.length,
      }))
      if (redis.complete) break
      await new Promise((resolve) => setTimeout(resolve, 50))
    }
    const foreignSentinel = await verifyAndRemoveRedisForeignSentinel().catch(() => ({
      seeded: redisForeignSentinelSeeded,
      preserved: false,
      removed: false,
    }))
    const serverResults = await Promise.allSettled(
      [...ACTIVE_SERVERS].map((server) => closeTrackedServer(server)),
    )
    const serverErrors = serverResults
      .filter((result) => result.status === 'rejected')
      .map((result) => result.reason?.message || 'server cleanup failed')
    if (TEMP_ROOT) fs.rmSync(TEMP_ROOT, { recursive: true, force: true })
    return {
      childGroupsStopped: ACTIVE_CHILDREN.size === 0 && childErrors.length === 0,
      childErrors,
      serversStopped: ACTIVE_SERVERS.size === 0 && serverErrors.length === 0,
      serverErrors,
      redis: {
        ...redis,
        attempts: redisAttempts,
        complete: redis.complete && foreignSentinel.preserved && foreignSentinel.removed,
        foreignSentinel,
      },
      tempRemoved: !TEMP_ROOT || !fs.existsSync(TEMP_ROOT),
    }
  })()
  cleanupPromise = running
  try {
    return await running
  } finally {
    if (cleanupPromise === running) cleanupPromise = null
  }
}

const SIGNAL_EXIT_CODES = new Map([
  ['SIGHUP', 129],
  ['SIGINT', 130],
  ['SIGTERM', 143],
])
function initiateShutdown(exitCode) {
  if (shutdownExitCode !== null) return
  shutdownExitCode = exitCode
  const hardExit = setTimeout(() => process.exit(exitCode), 25_000)
  hardExit.unref()
  void (async () => {
    let cleaned = await cleanupOwnedRuntime({ redisPasses: 2 }).catch(() => null)
    if (
      !cleaned
      || !cleaned.childGroupsStopped
      || !cleaned.serversStopped
      || !cleaned.redis.complete
      || !cleaned.tempRemoved
    ) {
      cleaned = await cleanupOwnedRuntime({ redisPasses: 2 }).catch(() => null)
    }
    clearTimeout(hardExit)
    process.exit(exitCode)
  })()
}

for (const [signal, exitCode] of SIGNAL_EXIT_CODES) {
  process.on(signal, () => initiateShutdown(exitCode))
}

function parseClaudeJsonl(stdout) {
  let finalUsage = null
  let resultText = ''
  let thinkingSeen = false
  let textSeen = false
  for (const line of stdout.split(/\r?\n/)) {
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
        if (block?.type === 'thinking') thinkingSeen = true
        if (block?.type === 'text' && String(block.text || '').includes('wire-ok')) textSeen = true
      }
    }
    if (value?.type === 'result') {
      if (typeof value.result === 'string') resultText = value.result
      if (value.usage && typeof value.usage === 'object') finalUsage = value.usage
    }
  }
  const usage = Object.fromEntries(Object.entries(finalUsage || {}).filter(([, value]) => (
    typeof value === 'number' && Number.isFinite(value)
  )))
  const inputTokens = [
    'input_tokens',
    'cache_creation_input_tokens',
    'cache_read_input_tokens',
  ].reduce((sum, key) => sum + Math.max(0, Number(usage[key] || 0)), 0)
  const outputTokens = Math.max(0, Number(usage.output_tokens || 0))
  const thinkingTokens = Number(
    finalUsage?.output_tokens_details?.thinking_tokens
      ?? finalUsage?.thinking_tokens
      ?? 0,
  )
  return {
    usage,
    inputTokens,
    outputTokens,
    thinkingTokens: Number.isFinite(thinkingTokens) ? thinkingTokens : 0,
    hasNonzeroInput: inputTokens > 0,
    hasNonzeroOutput: outputTokens > 0,
    thinkingSeen,
    wireTextSeen: textSeen || resultText.includes('wire-ok'),
  }
}

function isolatedClaudeEnvironment(home, configDir, ingressPort) {
  return minimalEnvironment({
    HOME: home,
    CLAUDE_CONFIG_DIR: configDir,
    ANTHROPIC_BASE_URL: `http://127.0.0.1:${ingressPort}/cc`,
    ANTHROPIC_API_KEY: REQUEST_KEY,
    CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: '1',
    DISABLE_AUTOUPDATER: '1',
    DISABLE_ERROR_REPORTING: '1',
    DISABLE_TELEMETRY: '1',
    CI: '1',
    TERM: 'dumb',
    NO_PROXY: '127.0.0.1,localhost,::1',
    no_proxy: '127.0.0.1,localhost,::1',
  })
}

async function runClaude({ endpoint, effort, round, ingressPort }) {
  throwIfShutdownRequested()
  const id = `${endpoint}-${effort}-${round}`
  const home = path.join(TEMP_ROOT, 'homes', id)
  const configDir = path.join(TEMP_ROOT, 'configs', id)
  const projectRoot = path.join(TEMP_ROOT, 'projects', id)
  fs.mkdirSync(home, { recursive: true, mode: 0o700 })
  fs.mkdirSync(configDir, { recursive: true, mode: 0o700 })
  fs.mkdirSync(projectRoot, { recursive: true, mode: 0o700 })
  const projectCwd = assertExternalDirectory(projectRoot, 'Claude cwd')
  const environment = isolatedClaudeEnvironment(home, configDir, ingressPort)

  const args = [
    '--bare',
    '--print',
    '--verbose',
    '--output-format',
    'stream-json',
    '--include-partial-messages',
    '--no-session-persistence',
    '--prompt-suggestions',
    'false',
    '--disable-slash-commands',
    '--session-id',
    crypto.randomUUID(),
    '--model',
    'opus',
    '--tools',
    '',
  ]
  if (effort !== 'absent') args.push('--effort', effort)
  args.push('--', `Reply with exactly wire-ok. Isolated ${id}.`)

  const started = performance.now()
  throwIfShutdownRequested()
  const child = spawnOwned(CLAUDE, args, {
    cwd: projectCwd,
    env: environment,
    detached: true,
    stdio: ['ignore', 'pipe', 'pipe'],
  }, `claude:${id}`)
  const stdout = []
  const stderr = []
  let capturedBytes = 0
  const capture = (target) => (chunk) => {
    capturedBytes += chunk.length
    if (capturedBytes <= 8 * 1024 * 1024) target.push(chunk)
  }
  child.stdout.on('data', capture(stdout))
  child.stderr.on('data', capture(stderr))

  const result = await waitForOwnedChildExit(child, 90_000)
  await stopChild(child, { termTimeoutMs: 2_000, killTimeoutMs: PROCESS_KILL_TIMEOUT_MS })
  const exit = {
    code: result.code,
    signal: result.signal,
    spawnError: result.error?.message || null,
  }
  const stdoutText = Buffer.concat(stdout).toString('utf8')
  const stderrText = Buffer.concat(stderr).toString('utf8')
  return {
    id,
    ...exit,
    timedOut: result.timedOut,
    captureOverflow: capturedBytes > 8 * 1024 * 1024,
    durationMs: Number((performance.now() - started).toFixed(2)),
    cwdExternal: true,
    parsed: parseClaudeJsonl(stdoutText),
    stdoutSha256: sha256(stdoutText),
    stderrSha256: sha256(stderrText),
  }
}

function isolatedConfig({ endpoint, servicePort, upstreamPort }) {
  return {
    postgres: {
      url: POSTGRES_URLS[endpoint],
      maxConnections: 4,
      migrateOnStart: true,
    },
    redis: {
      url: REDIS_URL,
      keyPrefix: REDIS_PREFIXES[endpoint],
    },
    host: '127.0.0.1',
    port: servicePort,
    apiKey: REQUEST_KEY,
    adminApiKey: ADMIN_KEY,
    defaultEndpoint: endpoint,
    kiroUpstreamBaseUrl: `http://127.0.0.1:${upstreamPort}/thinking-wire`,
    kiroUpstreamResponseTimeoutSecs: 10,
    kiroUpstreamStreamRetryEnabled: false,
    credentialRetryMaxAttempts: 1,
    inferenceUpstreamMaxAttempts: 1,
    credentialWarmupRequests: 0,
    externalPoolsEnabled: false,
    bodyConversion: {
      nativeReasoningFields: true,
    },
  }
}

function isolatedCredential(endpoint) {
  return [{
    authMethod: 'api_key',
    kiroApiKey: CREDENTIALS[endpoint],
    region: 'us-east-1',
    authRegion: 'us-east-1',
    apiRegion: 'us-east-1',
    endpoint,
  }]
}

function isolatedServiceEnvironment(endpoint, servicePort) {
  return minimalEnvironment({
    KIRO_API_KEY: '',
    KIRO_RS_HOST: '127.0.0.1',
    KIRO_RS_PORT: String(servicePort),
    KIRO_RS_POSTGRES_URL: POSTGRES_URLS[endpoint],
    KIRO_RS_REDIS_URL: REDIS_URL,
    KIRO_RS_POSTGRES_MIGRATE_ON_START: 'true',
    KIRO_RS_POSTGRES_COMPRESS_USAGE_ROLLUPS_ON_START: 'false',
    RUST_LOG: 'warn',
    NO_PROXY: '127.0.0.1,localhost,::1',
    no_proxy: '127.0.0.1,localhost,::1',
  })
}

function startService({ endpoint, configPath, credentialsPath, logPath, servicePort, serviceCwd }) {
  throwIfShutdownRequested()
  const verifiedServiceCwd = assertExternalDirectory(serviceCwd, 'isolated service cwd')
  const logFd = fs.openSync(logPath, 'a', 0o600)
  let child
  try {
    child = spawnOwned(BINARY, ['--config', configPath, '--credentials', credentialsPath], {
      cwd: verifiedServiceCwd,
      env: isolatedServiceEnvironment(endpoint, servicePort),
      detached: true,
      stdio: ['ignore', logFd, logFd],
    }, `kiro-rs:${endpoint}`)
  } catch (error) {
    fs.closeSync(logFd)
    throw error
  }
  child.spawnError = null
  let logClosed = false
  const release = () => {
    if (!logClosed) {
      logClosed = true
      fs.closeSync(logFd)
    }
  }
  child.once('error', (error) => {
    child.spawnError = error.message
    release()
  })
  child.once('close', release)
  return child
}

function normalizedKnownEffort(value) {
  const normalized = typeof value === 'string' ? value.trim().toLowerCase() : ''
  return EFFORTS.includes(normalized) && normalized !== 'absent' ? normalized : null
}

function expectedWireEffort(inbound) {
  const thinkingType = String(inbound?.thinking?.type || '').toLowerCase()
  if (thinkingType === 'disabled') return null
  const explicit = normalizedKnownEffort(inbound?.outputConfig?.effort)
  if (explicit && FAKE_EFFORT_SCHEMA.values.includes(explicit)) return explicit
  if (thinkingType === 'adaptive') return FAKE_EFFORT_SCHEMA.default
  return null
}

function validateCase({ endpoint, effort, cli, inboundRecords, wireRecords }) {
  const violations = []
  if (cli.spawnError) violations.push('claude_spawn_error')
  if (cli.timedOut) violations.push('claude_timeout')
  if (cli.captureOverflow) violations.push('claude_output_capture_overflow')
  if (cli.code !== 0) violations.push('claude_nonzero_exit')
  if (!cli.parsed.wireTextSeen) violations.push('fake_eventstream_text_not_consumed')
  if (!cli.parsed.hasNonzeroInput) violations.push('final_input_usage_missing_or_zero')
  if (!cli.parsed.hasNonzeroOutput) violations.push('final_output_usage_missing_or_zero')
  if (inboundRecords.length !== 1) violations.push('unexpected_cc_inference_count')
  if (wireRecords.length !== 1) violations.push('unexpected_kiro_inference_count')

  const inbound = inboundRecords[0]?.facts || null
  const wire = wireRecords[0]?.facts || null
  if (
    !Number.isInteger(inbound?.bodyBytes)
    || inbound.bodyBytes <= 0
    || inbound.bodyBytes > MAX_CAPTURE_BYTES
    || !/^[0-9a-f]{64}$/.test(String(inbound?.bodySha256 || ''))
  ) {
    violations.push('invalid_ingress_body_bound_or_hash')
  }
  if (
    !Number.isInteger(wire?.bodyBytes)
    || wire.bodyBytes <= 0
    || wire.bodyBytes > MAX_CAPTURE_BYTES
    || !/^[0-9a-f]{64}$/.test(String(wire?.bodySha256 || ''))
  ) {
    violations.push('invalid_wire_body_bound_or_hash')
  }
  if (effort !== 'absent' && inbound?.outputConfig?.effort !== effort) {
    violations.push('claude_effort_not_preserved_at_cc_ingress')
  }
  const expectedEffort = expectedWireEffort(inbound)
  if (expectedEffort !== null && !cli.parsed.thinkingSeen) {
    violations.push('fake_eventstream_thinking_not_consumed')
  }
  const wireEffort = wire?.additionalModelRequestFields?.output_config?.effort ?? null
  if (wireEffort !== expectedEffort) violations.push('kiro_wire_effort_mismatch')
  if (!FAKE_MODEL_IDS.includes(wire?.model)) violations.push('kiro_wire_model_not_advertised')
  if (wire?.endpoint !== endpoint || !wire?.credentialMatchesEndpoint) {
    violations.push('credential_endpoint_mismatch')
  }
  if (!wire?.authorizationBearer || wire?.tokenType !== 'API_KEY') {
    violations.push('fake_credential_headers_mismatch')
  }
  if ((wire?.protocolViolations || []).length > 0) violations.push('kiro_wire_protocol_mismatch')

  const wireThinking = wire?.additionalModelRequestFields?.thinking ?? null
  if (endpoint === 'cli') {
    if (wire?.origin !== 'KIRO_CLI') violations.push('cli_origin_mismatch')
  } else {
    if (wire?.origin !== 'AI_EDITOR') violations.push('ide_origin_mismatch')
  }

  return {
    violations,
    inbound,
    wire,
    expectedEffort,
    wireThinking,
    modelResolution: {
      inboundModel: inbound?.model ?? null,
      wireModel: wire?.model ?? null,
      changed: inbound?.model !== wire?.model,
      advertised: FAKE_MODEL_IDS.includes(wire?.model),
    },
  }
}

function updatePeak(peak, sample) {
  peak.rssKb = Math.max(peak.rssKb, sample.rssKb)
  peak.fdCount = Math.max(peak.fdCount, sample.fdCount)
}

async function runEndpoint({ endpoint, upstreamPort, fake }) {
  throwIfShutdownRequested()
  const endpointRoot = path.join(TEMP_ROOT, endpoint)
  fs.mkdirSync(endpointRoot, { recursive: true, mode: 0o700 })
  const serviceCwd = path.join(endpointRoot, 'service-cwd')
  fs.mkdirSync(serviceCwd, { recursive: true, mode: 0o700 })
  const serviceCwdReal = assertExternalDirectory(serviceCwd, 'isolated service cwd')
  const postgresBefore = await inspectPostgresIdentity(endpoint, true)
  const servicePort = await reservePort()
  const ingressPort = await reservePort()
  const configPath = path.join(endpointRoot, 'config.json')
  const credentialsPath = path.join(endpointRoot, 'credentials.json')
  const logPath = path.join(endpointRoot, 'service.log')
  fs.writeFileSync(
    configPath,
    `${JSON.stringify(isolatedConfig({ endpoint, servicePort, upstreamPort }), null, 2)}\n`,
    { mode: 0o600 },
  )
  fs.writeFileSync(
    credentialsPath,
    `${JSON.stringify(isolatedCredential(endpoint), null, 2)}\n`,
    { mode: 0o600 },
  )

  const service = startService({
    endpoint,
    configPath,
    credentialsPath,
    logPath,
    servicePort,
    serviceCwd: serviceCwdReal,
  })
  const baseUrl = `http://127.0.0.1:${servicePort}`
  const ingress = createIngressProxy(servicePort, endpoint)
  await ingress.listen(ingressPort)
  const cases = []
  let resourcesStart = { rssKb: 0, fdCount: 0 }
  const resourcesPeak = { rssKb: 0, fdCount: 0 }
  let resourcesEnd = { rssKb: 0, fdCount: 0 }
  let postgresAfter = null
  try {
    await waitForHealth(baseUrl, service)
    assert.deepEqual(listeningPids(servicePort), [service.pid])
    assert.deepEqual(listeningPids(ingressPort), [process.pid])
    resourcesStart = processResources(service.pid)
    updatePeak(resourcesPeak, resourcesStart)
    postgresAfter = await inspectPostgresIdentity(endpoint, false)

    for (const effort of EFFORTS) {
      for (let round = 1; round <= ROUNDS; round += 1) {
        throwIfShutdownRequested()
        const id = `${endpoint}-${effort}-${round}`
        activeCase = { id, endpoint, effort, round }
        progressLog({ event: 'case_start', id, endpoint, effort, round })
        const ingressStart = ingress.records.length
        const wireStart = fake.records.length
        const cli = await runClaude({ endpoint, effort, round, ingressPort })
        const inboundRecords = ingress.records
          .slice(ingressStart)
          .filter((record) => record.caseId === id && record.kind === 'messages')
        const wireRecords = fake.records
          .slice(wireStart)
          .filter((record) => record.caseId === id && record.kind === 'inference')
        const validation = validateCase({ endpoint, effort, cli, inboundRecords, wireRecords })
        const resource = processResources(service.pid)
        updatePeak(resourcesPeak, resource)
        cases.push({
          id,
          endpoint,
          effort,
          round,
          exitCode: cli.code,
          signal: cli.signal,
          timedOut: cli.timedOut,
          durationMs: cli.durationMs,
          expectedWireEffort: validation.expectedEffort,
          effortSchema: FAKE_EFFORT_SCHEMA,
          inbound: validation.inbound,
          wire: validation.wire,
          modelResolution: validation.modelResolution,
          usage: cli.parsed.usage,
          inputTokens: cli.parsed.inputTokens,
          outputTokens: cli.parsed.outputTokens,
          thinkingTokens: cli.parsed.thinkingTokens,
          thinkingSeen: cli.parsed.thinkingSeen,
          wireTextSeen: cli.parsed.wireTextSeen,
          claudeCwdExternal: cli.cwdExternal,
          stdoutSha256: cli.stdoutSha256,
          stderrSha256: cli.stderrSha256,
          violations: validation.violations,
          resource,
        })
        progressLog({
          event: 'case_done',
          id,
          endpoint,
          effort,
          round,
          durationMs: cli.durationMs,
          exitCode: cli.code,
          timedOut: cli.timedOut,
          violations: validation.violations,
        })
        activeCase = null
      }
    }
    resourcesEnd = processResources(service.pid)
    updatePeak(resourcesPeak, resourcesEnd)
  } finally {
    activeCase = null
    await stopChild(service)
    await closeTrackedServer(ingress.tracked)
  }

  return {
    endpoint,
    servicePort,
    ingressPort,
    cases,
    resources: {
      start: resourcesStart,
      peak: resourcesPeak,
      end: resourcesEnd,
      rssGrowthKb: Math.max(0, resourcesEnd.rssKb - resourcesStart.rssKb),
      fdGrowth: Math.max(0, resourcesEnd.fdCount - resourcesStart.fdCount),
    },
    postgres: { before: postgresBefore, after: postgresAfter },
    serviceCwdExternal: true,
    inboundTotals: {
      messages: ingress.records.filter((record) => record.kind === 'messages').length,
      countTokens: ingress.records.filter((record) => record.kind === 'count_tokens').length,
      ccHeadProbe: ingress.records.filter((record) => record.kind === 'cc_head_probe').length,
      invalidJson: ingress.records.filter((record) => record.kind === 'invalid_json').length,
      other: ingress.records.filter((record) => record.kind === 'other').length,
    },
    inboundCaseCounts: Object.fromEntries(cases.map((item) => [
      item.id,
      ingress.records.filter((record) => (
        record.kind === 'messages' && record.caseId === item.id
      )).length,
    ])),
    ingressOtherSamples: ingress.records
      .filter((record) => record.kind === 'other' || record.kind === 'cc_head_probe')
      .slice(0, 20)
      .map((record) => ({
        caseId: record.caseId,
        method: record.method,
        path: record.path,
      })),
  }
}

async function runSignalFixture() {
  const ackPath = requiredEnvironment('KIRO_SIGNAL_ACK_PATH')
  const fakePort = await reservePort()
  const ingressPort = await reservePort()
  const redisPort = await reservePort()

  for (const [name, port] of [['fake', fakePort], ['ingress', ingressPort]]) {
    const server = http.createServer((_request, response) => {
      response.writeHead(204)
      response.end()
    })
    registerServer(`signal-${name}`, server)
    await new Promise((resolve, reject) => {
      server.once('error', reject)
      server.listen(port, '127.0.0.1', resolve)
    })
  }

  const state = {
    keys: new Map([
      [REDIS_PREFIXES.cli, 'cli-root'],
      [`${REDIS_PREFIXES.cli}:owned-a`, 'cli-a'],
      [`${REDIS_PREFIXES.cli}:owned-b`, 'cli-b'],
      [REDIS_PREFIXES.ide, 'ide-root'],
      [`${REDIS_PREFIXES.ide}:owned-a`, 'ide-a'],
      [`${REDIS_PREFIXES.ide}:owned-b`, 'ide-b'],
      [REDIS_FOREIGN_SENTINEL, REDIS_FOREIGN_VALUE],
    ]),
    patterns: [],
    protocolErrors: [],
    foreignPreserved: false,
    foreignFinalGet: false,
    injectedFailures: 0,
    ownedChildren: [],
    commandTimeoutRejected: false,
    commandSpawnRejected: false,
    heldServerSocketsClosed: null,
  }
  fixtureRedisState = state
  redisForeignSentinelSeeded = true
  const writeAck = () => {
    if (!state.foreignFinalGet) return
    const ownedRemaining = [...state.keys.keys()].filter((key) => (
      ENDPOINTS.some((endpoint) => (
        key === REDIS_PREFIXES[endpoint] || key.startsWith(`${REDIS_PREFIXES[endpoint]}:`)
      ))
    ))
    fs.writeFileSync(ackPath, `${JSON.stringify({
      patterns: state.patterns,
      expectedPatterns: ENDPOINTS.map((endpoint) => `${REDIS_PREFIXES[endpoint]}:*`),
      ownedRemaining,
      foreignPreserved: state.foreignPreserved,
      foreignRemoved: !state.keys.has(REDIS_FOREIGN_SENTINEL),
      protocolErrors: state.protocolErrors,
      blockedSpawnAttempts: fixtureBlockedSpawnAttempts,
      killEscalations: fixtureKillEscalations,
      injectedRedisFailures: state.injectedFailures,
      commandTimeoutRejected: state.commandTimeoutRejected,
      commandSpawnRejected: state.commandSpawnRejected,
      heldServerSocketsClosed: state.heldServerSocketsClosed,
      fixture: {
        tempRoot: TEMP_ROOT,
        ports: { fakePort, ingressPort, redisPort },
        ownedChildren: state.ownedChildren,
      },
    })}\n`, { mode: 0o600 })
  }
  const redisServer = net.createServer((socket) => {
    let pending = Buffer.alloc(0)
    socket.on('data', (data) => {
      pending = Buffer.concat([pending, data])
      const responses = []
      try {
        for (;;) {
          const parsed = parseRedisRequest(pending)
          if (!parsed) break
          pending = pending.subarray(parsed.next)
          const [rawCommand, ...args] = parsed.values
          const command = String(rawCommand || '').toUpperCase()
          if (command === 'EVAL') {
            if (
              state.injectedFailures === 0
              && (FIXTURE_MODE === 'redis_error' || FIXTURE_MODE === 'redis_timeout')
            ) {
              state.injectedFailures += 1
              if (FIXTURE_MODE === 'redis_timeout') return
              throw new Error('injected transient Redis cleanup error')
            }
            const [script, keyCount, pattern, cursor, exactPrefix] = args
            const allowedPatterns = ENDPOINTS.map((endpoint) => `${REDIS_PREFIXES[endpoint]}:*`)
            if (
              keyCount !== '0'
              || cursor !== '0'
              || !allowedPatterns.includes(pattern)
              || exactPrefix !== pattern.slice(0, -2)
              || !script.includes("redis.call('SCAN'")
              || !script.includes("redis.call('DEL'")
            ) {
              state.protocolErrors.push('invalid_eval_cleanup_contract')
              throw new Error('invalid EVAL cleanup contract')
            }
            state.patterns.push(pattern)
            const prefix = pattern.slice(0, -1)
            let removed = state.keys.delete(exactPrefix) ? 1 : 0
            for (const key of [...state.keys.keys()]) {
              if (key.startsWith(prefix)) {
                state.keys.delete(key)
                removed += 1
              }
            }
            responses.push(encodeRedisResponse([removed, '0']))
          } else if (command === 'GET') {
            const value = state.keys.get(args[0]) ?? null
            if (args[0] === REDIS_FOREIGN_SENTINEL) {
              if (value === REDIS_FOREIGN_VALUE) state.foreignPreserved = true
              if (value === null) state.foreignFinalGet = true
            }
            responses.push(encodeRedisResponse(value))
          } else if (command === 'DEL') {
            let removed = 0
            for (const key of args) {
              if (state.keys.delete(key)) removed += 1
            }
            responses.push(encodeRedisResponse(removed))
          } else {
            state.protocolErrors.push(`unexpected_${command.toLowerCase()}`)
            throw new Error(`unexpected fake Redis command ${command}`)
          }
        }
        if (responses.length > 0) socket.write(Buffer.concat(responses))
        writeAck()
      } catch (error) {
        socket.end(`-ERR ${String(error.message || 'fake Redis failure').replace(/[\r\n]/g, ' ')}\r\n`)
      }
    })
  })
  registerServer('signal-redis', redisServer)
  await new Promise((resolve, reject) => {
    redisServer.once('error', reject)
    redisServer.listen(redisPort, '127.0.0.1', resolve)
  })
  REDIS_URL = `redis://127.0.0.1:${redisPort}/0`

  fs.writeFileSync(path.join(TEMP_ROOT, 'owned-temp-marker'), 'owned\n', { mode: 0o600 })
  const childReadyPath = path.join(TEMP_ROOT, 'owned-child-ready')
  const childProgram = FIXTURE_MODE === 'cleanup_timeout'
    ? "const fs=require('fs');process.on('SIGTERM',()=>{});fs.writeFileSync(process.argv[1],'ready');setInterval(()=>{},1000)"
    : "const fs=require('fs');fs.writeFileSync(process.argv[1],'ready');setInterval(()=>{},1000)"
  const ownedChild = spawnOwned(process.execPath, ['-e', childProgram, childReadyPath], {
    detached: true,
    stdio: 'ignore',
  }, `fixture:${FIXTURE_MODE}:owned`)
  state.ownedChildren.push({
    pid: ownedChild.pid,
    pgid: ownedChild.ownedPgid,
    startIdentity: ownedChild.ownedStartIdentity,
  })
  const childReadyDeadline = Date.now() + 5_000
  while (!fs.existsSync(childReadyPath)) {
    if (Date.now() >= childReadyDeadline) throw new Error('owned fixture child readiness timed out')
    await new Promise((resolve) => setTimeout(resolve, 10))
  }

  if (FIXTURE_MODE === 'startup_error') {
    throw new Error('injected startup failure')
  }

  if (FIXTURE_MODE === 'signal_race') {
    const raceTimer = setInterval(() => {
      try {
        if (ACTIVE_CHILDREN.size < 32) {
          spawnOwned(process.execPath, ['-e', 'setTimeout(() => {}, 50)'], {
            detached: true,
            stdio: 'ignore',
          }, 'fixture:signal_race:short')
        }
      } catch (error) {
        if (error instanceof ShutdownRequested) fixtureBlockedSpawnAttempts += 1
        else throw error
      }
    }, 5)
    raceTimer.unref()
  }

  process.stdout.write(`${JSON.stringify({
    ready: true,
    mode: FIXTURE_MODE,
    tempRoot: TEMP_ROOT,
    ownedChildren: state.ownedChildren,
    fakePort,
    ingressPort,
    redisPort,
    redisPrefixes: REDIS_PREFIXES,
  })}\n`)

  if (FIXTURE_MODE === 'command_timeout') {
    try {
      await commandOutput(process.execPath, [
        '-e',
        "process.on('SIGTERM',()=>{});setInterval(()=>{},1000)",
      ], 100, { cwd: TEMP_ROOT })
    } catch {
      state.commandTimeoutRejected = true
    }
    if (!state.commandTimeoutRejected) {
      throw new Error('command timeout fixture unexpectedly succeeded')
    }
  }
  if (FIXTURE_MODE === 'command_spawn_error') {
    try {
      await commandOutput(path.join(TEMP_ROOT, 'missing-executable'), [], 100, { cwd: TEMP_ROOT })
    } catch {
      state.commandSpawnRejected = true
    }
    if (!state.commandSpawnRejected) {
      throw new Error('command spawn-error fixture unexpectedly succeeded')
    }
  }
  const heldServerSockets = []
  if (FIXTURE_MODE === 'server_socket_hang') {
    for (const port of [fakePort, ingressPort, redisPort]) {
      const socket = net.connect({ host: '127.0.0.1', port })
      socket.on('error', () => {})
      await settleWithin(
        new Promise((resolve, reject) => {
          socket.once('connect', resolve)
          socket.once('error', reject)
        }),
        1_000,
        'held server socket connection timed out',
      )
      heldServerSockets.push(socket)
    }
  }

  if (
    FIXTURE_MODE.startsWith('cleanup_')
    || FIXTURE_MODE.startsWith('redis_')
    || FIXTURE_MODE.startsWith('command_')
    || FIXTURE_MODE === 'server_socket_hang'
  ) {
    const exitCode = FIXTURE_MODE === 'cleanup_error' ? 1 : 124
    const effectiveExitCode = (
      FIXTURE_MODE.startsWith('command_') || FIXTURE_MODE === 'server_socket_hang'
    )
      ? 0
      : FIXTURE_MODE.startsWith('redis_')
        ? 1
        : exitCode
    await new Promise((resolve) => setTimeout(resolve, 50))
    shutdownExitCode = effectiveExitCode
    const cleaned = await cleanupOwnedRuntime({ redisPasses: 2 })
    if (
      !cleaned.childGroupsStopped
      || !cleaned.serversStopped
      || !cleaned.redis.complete
      || !cleaned.tempRemoved
    ) {
      throw new Error(`${FIXTURE_MODE} cleanup was incomplete: ${JSON.stringify(cleaned)}`)
    }
    if (FIXTURE_MODE === 'server_socket_hang') {
      await settleWithin(
        Promise.all(heldServerSockets.map((socket) => (
          socket.destroyed
            ? Promise.resolve()
            : new Promise((resolve) => socket.once('close', resolve))
        ))),
        1_000,
        'held server sockets did not close during cleanup',
      )
      state.heldServerSocketsClosed = heldServerSockets.every((socket) => socket.destroyed)
      writeAck()
      if (!state.heldServerSocketsClosed) {
        throw new Error('held server sockets remained active after cleanup')
      }
    }
    process.exitCode = effectiveExitCode
    return
  }
  await new Promise(() => {})
}

function summarizeCases(cases) {
  return Object.fromEntries(ENDPOINTS.flatMap((endpoint) => EFFORTS.map((effort) => {
    const selected = cases.filter((item) => item.endpoint === endpoint && item.effort === effort)
    return [`${endpoint}:${effort}`, {
      rounds: selected.length,
      inboundEfforts: [...new Set(selected.map((item) => (
        item.inbound?.outputConfig?.effort ?? null
      )))],
      wireEfforts: [...new Set(selected.map((item) => (
        item.wire?.additionalModelRequestFields?.output_config?.effort ?? null
      )))],
      wireThinkingVariants: [...new Set(selected.map((item) => JSON.stringify(
        item.wire?.additionalModelRequestFields?.thinking ?? null,
      )))].map(JSON.parse),
      inboundModels: [...new Set(selected.map((item) => item.inbound?.model ?? null))],
      models: [...new Set(selected.map((item) => item.wire?.model ?? null))],
      modelResolutionVariants: [...new Set(selected.map((item) => JSON.stringify(
        item.modelResolution,
      )))].map(JSON.parse),
      origins: [...new Set(selected.map((item) => item.wire?.origin ?? null))],
      inferenceHits: selected.filter((item) => item.wire !== null).length,
      violations: selected.reduce((count, item) => count + item.violations.length, 0),
    }]
  })))
}

function validateResourceBounds(endpointRuns, runnerStart, runnerEnd) {
  const violations = []
  for (const entry of endpointRuns) {
    const resources = entry.resources
    if (resources.rssGrowthKb > MAX_SERVICE_RSS_GROWTH_KB) {
      violations.push(`${entry.endpoint}:service_rss_growth`)
    }
    if (resources.peak.rssKb > MAX_SERVICE_PEAK_RSS_KB) {
      violations.push(`${entry.endpoint}:service_peak_rss`)
    }
    if (resources.fdGrowth > MAX_SERVICE_FD_GROWTH) {
      violations.push(`${entry.endpoint}:service_fd_growth`)
    }
    if (resources.peak.fdCount > MAX_SERVICE_PEAK_FDS) {
      violations.push(`${entry.endpoint}:service_peak_fds`)
    }
  }
  const runnerRssGrowthBytes = Math.max(0, runnerEnd.rss - runnerStart.rss)
  const runnerHeapGrowthBytes = Math.max(0, runnerEnd.heapUsed - runnerStart.heapUsed)
  if (runnerRssGrowthBytes > MAX_RUNNER_RSS_GROWTH_BYTES) violations.push('runner_rss_growth')
  if (runnerHeapGrowthBytes > MAX_RUNNER_HEAP_GROWTH_BYTES) violations.push('runner_heap_growth')
  return {
    violations,
    runner: {
      start: runnerStart,
      end: runnerEnd,
      rssGrowthBytes: runnerRssGrowthBytes,
      heapGrowthBytes: runnerHeapGrowthBytes,
    },
    limits: {
      serviceRssGrowthKb: MAX_SERVICE_RSS_GROWTH_KB,
      servicePeakRssKb: MAX_SERVICE_PEAK_RSS_KB,
      serviceFdGrowth: MAX_SERVICE_FD_GROWTH,
      servicePeakFds: MAX_SERVICE_PEAK_FDS,
      runnerRssGrowthBytes: MAX_RUNNER_RSS_GROWTH_BYTES,
      runnerHeapGrowthBytes: MAX_RUNNER_HEAP_GROWTH_BYTES,
      note: 'wire compatibility bound, not a soak or capacity benchmark',
    },
  }
}

async function main() {
  throwIfShutdownRequested()
  fs.mkdirSync(TEMP_ROOT, { recursive: true, mode: 0o700 })
  assertExternalDirectory(TEMP_ROOT, 'runtime temp root')
  const started = performance.now()
  const runnerResourcesStart = process.memoryUsage()
  const binaryIdentityBefore = await frozenExecutableIdentity(BINARY)
  const claudeIdentityBefore = await frozenExecutableIdentity(CLAUDE)
  const psqlIdentity = await frozenExecutableIdentity(PSQL)
  const sourceIdentity = await workingTreeSourceManifest()
  const upstreamPort = await reservePort()
  const fake = createFakeKiroUpstream()
  const endpointRuns = []
  let report = null
  let cleanupDetails = null
  let cleanup = {
    childGroupsStopped: false,
    serversStopped: false,
    redisKeysRemoved: false,
    tempRemoved: false,
    portsReleased: false,
    forbiddenPortsNeverAllocated: false,
  }
  const overallTimer = setTimeout(() => initiateShutdown(124), MAX_WALL_DURATION_MS)
  overallTimer.unref()

  try {
    await seedRedisForeignSentinel()
    await fake.listen(upstreamPort)
    assert.deepEqual(listeningPids(upstreamPort), [process.pid])
    const versionRoot = path.join(TEMP_ROOT, 'claude-version')
    const versionHome = path.join(versionRoot, 'home')
    const versionConfig = path.join(versionRoot, 'config')
    fs.mkdirSync(versionHome, { recursive: true, mode: 0o700 })
    fs.mkdirSync(versionConfig, { recursive: true, mode: 0o700 })
    const versionCwd = assertExternalDirectory(versionRoot, 'Claude version cwd')
    const claudeCliVersionOutput = await commandOutput(CLAUDE, ['--version'], COMMAND_TIMEOUT_MS, {
      cwd: versionCwd,
      env: minimalEnvironment({
        HOME: versionHome,
        CLAUDE_CONFIG_DIR: versionConfig,
        DISABLE_AUTOUPDATER: '1',
        DISABLE_ERROR_REPORTING: '1',
        DISABLE_TELEMETRY: '1',
      }),
    })
    const escapedVersion = EXPECTED_CLAUDE_VERSION.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
    if (!new RegExp(`(^|\\s)${escapedVersion}(?=\\s|$|\\()`).test(claudeCliVersionOutput)) {
      throw new Error(`Claude CLI version must be exactly identifiable as ${EXPECTED_CLAUDE_VERSION}`)
    }

    for (const endpoint of ENDPOINTS) {
      throwIfShutdownRequested()
      const endpointRun = await runEndpoint({ endpoint, upstreamPort, fake })
      endpointRuns.push(endpointRun)
    }
    const cases = endpointRuns.flatMap((entry) => entry.cases)
    const expectedCasesPerEndpoint = EFFORTS.length * ROUNDS
    const unknownRequests = fake.records.filter((record) => record.kind === 'unknown').length
    const invalidWireJson = fake.records.filter((record) => record.kind === 'invalid_json').length
    const violations = cases.reduce((count, item) => count + item.violations.length, 0)
    const protocolViolations = fake.records.reduce((count, record) => (
      count + (record.protocolViolations?.length || record.facts?.protocolViolations?.length || 0)
    ), 0)
    const discoveryCounts = Object.fromEntries(ENDPOINTS.map((endpoint) => [
      endpoint,
      fake.records.filter((record) => (
        record.kind === 'model_discovery'
        && record.credentialEndpoint === endpoint
        && record.transportEndpoint === endpoint
      )).length,
    ]))
    const modelDiscoveryEndpoints = [...new Set(fake.records
      .filter((record) => (
        record.kind === 'model_discovery'
        && record.credentialEndpoint === record.transportEndpoint
      ))
      .map((record) => record.credentialEndpoint))].sort()
    const modelDiscoverySchemaEndpoints = [...new Set(fake.records
      .filter((record) => (
        record.kind === 'model_discovery'
        && record.schemaPath === FAKE_EFFORT_SCHEMA.path
        && JSON.stringify(record.schemaValues) === JSON.stringify(FAKE_EFFORT_SCHEMA.values)
        && record.protocolViolations.length === 0
        && record.credentialEndpoint === record.transportEndpoint
      ))
      .map((record) => record.credentialEndpoint))].sort()
    const discoveryComplete = ENDPOINTS.every((endpoint) => (
      modelDiscoveryEndpoints.includes(endpoint)
      && modelDiscoverySchemaEndpoints.includes(endpoint)
      && discoveryCounts[endpoint] === 1
    ))
    const ingressBudgetComplete = endpointRuns.every((entry) => (
      entry.inboundTotals.messages === expectedCasesPerEndpoint
      && entry.inboundTotals.countTokens === 0
      && entry.inboundTotals.ccHeadProbe === expectedCasesPerEndpoint
      && entry.inboundTotals.invalidJson === 0
      && entry.inboundTotals.other === 0
    ))
    const inferenceHits = fake.records.filter((record) => record.kind === 'inference').length
    const inferenceBudgetComplete = inferenceHits === cases.length
    const balanceCounts = Object.fromEntries(ENDPOINTS.map((endpoint) => [
      endpoint,
      fake.records.filter((record) => (
        record.kind === 'balance' && record.credentialEndpoint === endpoint
      )).length,
    ]))
    const auxiliaryBudgetComplete = ENDPOINTS.every((endpoint) => balanceCounts[endpoint] <= 1)
    const runnerResourcesEnd = process.memoryUsage()
    const resourceGate = validateResourceBounds(
      endpointRuns,
      runnerResourcesStart,
      runnerResourcesEnd,
    )
    const wallWithinBound = performance.now() - started <= MAX_WALL_DURATION_MS
    const binaryIdentityAfter = await frozenExecutableIdentity(BINARY)
    const claudeIdentityAfter = await frozenExecutableIdentity(CLAUDE)
    const binaryUnchanged = sameFrozenExecutable(binaryIdentityBefore, binaryIdentityAfter)
    const claudeBinaryUnchanged = sameFrozenExecutable(claudeIdentityBefore, claudeIdentityAfter)
    report = {
      schemaVersion: 2,
      result: violations === 0
        && unknownRequests === 0
        && invalidWireJson === 0
        && protocolViolations === 0
        && discoveryComplete
        && ingressBudgetComplete
        && inferenceBudgetComplete
        && auxiliaryBudgetComplete
        && resourceGate.violations.length === 0
        && wallWithinBound
        && binaryUnchanged
        && claudeBinaryUnchanged
        ? 'pass'
        : 'fail',
      runId: RUN_ID,
      sourceIdentity,
      binaryIdentity: binaryIdentityBefore,
      claudeBinaryIdentity: claudeIdentityBefore,
      psqlBinaryIdentity: psqlIdentity,
      claudeCliVersion: EXPECTED_CLAUDE_VERSION,
      claudeCliVersionOutputSha256: sha256(claudeCliVersionOutput),
      expectedClaudeVersion: EXPECTED_CLAUDE_VERSION,
      rounds: ROUNDS,
      endpoints: ENDPOINTS,
      efforts: EFFORTS,
      totalCases: cases.length,
      cases,
      summary: summarizeCases(cases),
      totals: {
        inferenceHits,
        modelDiscoveryHits: fake.records.filter((record) => record.kind === 'model_discovery').length,
        modelDiscoverySchemaHits: fake.records.filter((record) => (
          record.kind === 'model_discovery'
          && record.schemaPath === FAKE_EFFORT_SCHEMA.path
          && record.protocolViolations.length === 0
        )).length,
        discoveryCounts,
        balanceCounts,
        modelDiscoveryEndpoints,
        modelDiscoverySchemaEndpoints,
        balanceHits: fake.records.filter((record) => record.kind === 'balance').length,
        unknownRequests,
        invalidWireJson,
        protocolViolations,
        violations,
      },
      resources: Object.fromEntries(endpointRuns.map((entry) => [entry.endpoint, entry.resources])),
      resourceGate,
      requestBudgets: {
        expectedCasesPerEndpoint,
        ingressBudgetComplete,
        inferenceBudgetComplete,
        discoveryComplete,
        auxiliaryBudgetComplete,
        countTokensExpected: 0,
        ccHeadProbeExpected: expectedCasesPerEndpoint,
        ingressOtherExpected: 0,
        modelDiscoveryExpectedPerEndpoint: 1,
        balanceMaximumPerEndpoint: 1,
      },
      ingressTotals: Object.fromEntries(endpointRuns.map((entry) => [
        entry.endpoint,
        entry.inboundTotals,
      ])),
      inboundCaseCounts: Object.fromEntries(endpointRuns.map((entry) => [
        entry.endpoint,
        entry.inboundCaseCounts,
      ])),
      ingressOtherSamples: Object.fromEntries(endpointRuns.map((entry) => [
        entry.endpoint,
        entry.ingressOtherSamples,
      ])),
      isolation: {
        upstreamPort,
        servicePorts: endpointRuns.map((entry) => entry.servicePort),
        ingressPorts: endpointRuns.map((entry) => entry.ingressPort),
        forbiddenPorts: [...FORBIDDEN_PORTS],
        forbiddenPortsNeverAllocated: [...ALLOCATED_PORTS]
          .every((port) => !FORBIDDEN_PORTS.has(port)),
        explicitCredentialEndpointFiles: true,
        postgres: Object.fromEntries(endpointRuns.map((entry) => [entry.endpoint, entry.postgres])),
        postgresOwnerMarkerVerified: endpointRuns.every((entry) => (
          entry.postgres.before.ownerMarkerSha256 === entry.postgres.after.ownerMarkerSha256
          && entry.postgres.before.tableCount === 0
          && entry.postgres.after.tableCount > 0
        )),
        distinctPostgresDatabases: POSTGRES_IDENTITIES.cli.database
          !== POSTGRES_IDENTITIES.ide.database,
        distinctRedisPrefixes: REDIS_PREFIXES.cli !== REDIS_PREFIXES.ide,
        binaryUnchanged,
        claudeBinaryUnchanged,
        serviceEnvironmentOverridesPinned: ENDPOINTS.every((endpoint) => {
          const environment = isolatedServiceEnvironment(endpoint, 1)
          return environment.KIRO_RS_POSTGRES_URL === POSTGRES_URLS[endpoint]
            && environment.KIRO_RS_REDIS_URL === REDIS_URL
            && environment.KIRO_RS_POSTGRES_MIGRATE_ON_START === 'true'
            && environment.KIRO_RS_POSTGRES_COMPRESS_USAGE_ROLLUPS_ON_START === 'false'
        }),
        serviceCwdExternal: endpointRuns.every((entry) => entry.serviceCwdExternal),
        claudeCwdExternal: cases.every((entry) => entry.claudeCwdExternal),
        isolatedHomePerCase: true,
        isolatedClaudeConfigPerCase: true,
        isolatedProjectPerCase: true,
        fakeKiroCredential: true,
        fakeModelDiscoverySchema: {
          modelIds: FAKE_MODEL_IDS,
          effort: FAKE_EFFORT_SCHEMA,
        },
        fakeEventStream: true,
      },
      capturePolicy: {
        thinkingFieldsAreObservedNotNormative: true,
        endpointThinkingPresenceDoesNotAffectPass: true,
        effortIsCheckedOnlyAgainstAdvertisedFakeSchema: true,
        modelResolutionIsRecordedAndMustEndAtAnAdvertisedModel: true,
      },
      callerResponsibilities: {
        cliPostgresDatabaseMustBeCreatedEmptyBeforeRun: true,
        idePostgresDatabaseMustBeCreatedEmptyBeforeRun: true,
        bothPostgresDatabasesMustBeDroppedAfterRunEvenOnFailure: true,
        runnerNeverClearsReusesOrDropsCallerDatabases: true,
        artifactRootMustBeDeletedAfterRedactedEvidenceIsExtracted: true,
      },
      cleanup,
      wallDurationMs: Number((performance.now() - started).toFixed(2)),
      wallDurationLimitMs: MAX_WALL_DURATION_MS,
    }
  } finally {
    clearTimeout(overallTimer)
    let owned = await cleanupOwnedRuntime({ redisPasses: 4 })
    if (!owned.childGroupsStopped || !owned.serversStopped || !owned.redis.complete || !owned.tempRemoved) {
      owned = await cleanupOwnedRuntime({ redisPasses: 4 })
    }
    cleanupDetails = owned
    cleanup.childGroupsStopped = owned.childGroupsStopped
    cleanup.serversStopped = owned.serversStopped
    cleanup.redisKeysRemoved = owned.redis.complete
    cleanup.tempRemoved = owned.tempRemoved
    cleanup.portsReleased = [...ALLOCATED_PORTS]
      .every((port) => listeningPids(port).length === 0)
    cleanup.forbiddenPortsNeverAllocated = [...ALLOCATED_PORTS]
      .every((port) => !FORBIDDEN_PORTS.has(port))
    if (report) {
      report.cleanup = cleanup
      report.cleanupDetails = {
        childErrors: cleanupDetails.childErrors,
        serverErrors: cleanupDetails.serverErrors,
        redis: cleanupDetails.redis,
      }
      report.wallDurationMs = Number((performance.now() - started).toFixed(2))
      if (report.wallDurationMs > MAX_WALL_DURATION_MS) report.result = 'fail'
      if (!Object.values(cleanup).every(Boolean)) report.result = 'fail'
      fs.mkdirSync(REPORT_ROOT, { recursive: true, mode: 0o700 })
      const serialized = `${JSON.stringify(report, null, 2)}\n`
      assertReportRedacted(serialized, [
        REQUEST_KEY,
        ADMIN_KEY,
        CREDENTIALS.cli,
        CREDENTIALS.ide,
        POSTGRES_URLS.cli,
        POSTGRES_URLS.ide,
        REDIS_URL,
        REDIS_FOREIGN_VALUE,
        TEMP_ROOT,
        ARTIFACT_ROOT,
        BINARY,
        CLAUDE,
        PSQL,
      ])
      fs.writeFileSync(REPORT_PATH, serialized, { mode: 0o600 })
    }
  }

  assert.ok(report)
  assert.equal(report.totalCases, ENDPOINTS.length * EFFORTS.length * ROUNDS)
  assert.equal(report.totals.inferenceHits, report.totalCases)
  assert.deepEqual(report.totals.discoveryCounts, { cli: 1, ide: 1 })
  assert.equal(report.requestBudgets.ingressBudgetComplete, true)
  assert.equal(report.requestBudgets.inferenceBudgetComplete, true)
  assert.equal(report.requestBudgets.auxiliaryBudgetComplete, true)
  assert.equal(report.totals.unknownRequests, 0)
  assert.equal(report.totals.invalidWireJson, 0)
  assert.deepEqual(report.totals.modelDiscoveryEndpoints, ENDPOINTS)
  assert.deepEqual(report.totals.modelDiscoverySchemaEndpoints, ENDPOINTS)
  assert.equal(report.result, 'pass', `thinking wire gate failed; inspect ${REPORT_PATH}`)
  process.stdout.write(`${REPORT_PATH}\n`)
}

function contractWireFacts(endpoint, thinking) {
  return {
    endpoint,
    model: 'claude-opus-4-8',
    origin: endpoint === 'cli' ? 'KIRO_CLI' : 'AI_EDITOR',
    additionalModelRequestFields: {
      output_config: { effort: 'max' },
      ...(thinking === undefined ? {} : { thinking }),
    },
    bodyBytes: 1,
    bodySha256: '0'.repeat(64),
    credentialMatchesEndpoint: true,
    authorizationBearer: true,
    tokenType: 'API_KEY',
    protocolViolations: [],
  }
}

function runContractFixture() {
  const contractOwner = 'contract_owner'
  const isolation = validateIsolationUrls({
    cli: `postgres://127.0.0.1/kiro_thinking_wire_${contractOwner}_cli`,
    ide: `postgres://127.0.0.1/kiro_thinking_wire_${contractOwner}_ide`,
  }, 'redis://127.0.0.1:6379/15', contractOwner)
  let invalidDatabaseRejected = false
  try {
    validateIsolationUrls({
      cli: 'postgres://127.0.0.1/existing_database',
      ide: `postgres://127.0.0.1/kiro_thinking_wire_${contractOwner}_ide`,
    }, 'redis://127.0.0.1:6379/15', contractOwner)
  } catch {
    invalidDatabaseRejected = true
  }
  const contractExternalRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'thinking-wire-contract-'))
  let externalCwdAccepted = false
  let repositoryCwdRejected = false
  let shutdownSpawnRejected = false
  let nonDetachedSpawnRejected = false
  const reportPolicy = {
    safeAccepted: false,
    urlRejected: false,
    encodedUrlRejected: false,
    keyRejected: false,
    tempPathRejected: false,
    encodedSecretRejected: false,
  }
  try {
    externalCwdAccepted = assertExternalDirectory(
      contractExternalRoot,
      'contract external cwd',
    ) === fs.realpathSync(contractExternalRoot)
    try {
      assertExternalDirectory(ROOT, 'contract repository cwd')
    } catch {
      repositoryCwdRejected = true
    }
    const previousShutdownExitCode = shutdownExitCode
    shutdownExitCode = 143
    try {
      spawnOwned(process.execPath, ['-e', 'process.exit(99)'], { stdio: 'ignore' }, 'blocked-contract')
    } catch (error) {
      shutdownSpawnRejected = error instanceof ShutdownRequested
    } finally {
      shutdownExitCode = previousShutdownExitCode
    }
    try {
      spawnOwned(process.execPath, ['-e', 'process.exit(99)'], { stdio: 'ignore' }, 'unsafe-contract')
    } catch (error) {
      nonDetachedSpawnRejected = /isolated process group/.test(error.message)
    }

    assertReportRedacted(JSON.stringify({ route: '/thinking-wire/', sha256: '0'.repeat(64) }), [])
    reportPolicy.safeAccepted = true
    for (const [name, serialized, forbidden] of [
      ['urlRejected', JSON.stringify({ value: 'redis://127.0.0.1:6379/15' }), []],
      ['encodedUrlRejected', JSON.stringify({ value: 'redis%3A%2F%2F127.0.0.1%2F15' }), []],
      ['keyRejected', JSON.stringify({ value: 'ksk_fixture_secret_value' }), []],
      ['tempPathRejected', JSON.stringify({ value: contractExternalRoot }), [contractExternalRoot]],
      ['encodedSecretRejected', JSON.stringify({ value: encodeURIComponent('secret/value') }), ['secret/value']],
    ]) {
      try {
        assertReportRedacted(serialized, forbidden)
      } catch {
        reportPolicy[name] = true
      }
    }
  } finally {
    fs.rmSync(contractExternalRoot, { recursive: true, force: true })
  }

  const ownedIdentity = {
    ownedPgid: 4242,
    ownedStartIdentity: 'Mon Jul 17 12:00:00 2026',
  }
  const matchingIdentityAccepted = validateOwnedGroupSnapshot(
    ownedIdentity,
    ownedIdentity.ownedStartIdentity,
    [{ pid: 4242, pgid: 4242, startIdentity: ownedIdentity.ownedStartIdentity }],
  ).members.length === 1
  let reusedLeaderRejected = false
  let reusedMemberRejected = false
  let missingIdentityRejected = false
  try {
    validateOwnedGroupSnapshot(ownedIdentity, 'Mon Jul 17 12:00:01 2026', [])
  } catch {
    reusedLeaderRejected = true
  }
  try {
    validateOwnedGroupSnapshot(ownedIdentity, null, [
      { pid: 4242, pgid: 4242, startIdentity: 'Mon Jul 17 12:00:01 2026' },
    ])
  } catch {
    reusedMemberRejected = true
  }
  try {
    validateOwnedGroupSnapshot({ ownedPgid: 4242, ownedStartIdentity: null }, 'alive', [])
  } catch {
    missingIdentityRejected = true
  }
  const previousSentinelIntegrityFailed = redisForeignSentinelIntegrityFailed
  redisForeignSentinelIntegrityFailed = false
  const sentinelInitiallyIntact = recordRedisForeignSentinelIntegrity(true, true)
  const sentinelCorruptionRejected = !recordRedisForeignSentinelIntegrity(false, true)
  const sentinelRetryCannotMaskCorruption = !recordRedisForeignSentinelIntegrity(true, true)
  redisForeignSentinelIntegrityFailed = previousSentinelIntegrityFailed
  const inbound = {
    model: 'claude-opus-4-8',
    thinking: { type: 'adaptive' },
    outputConfig: { effort: 'max' },
    bodyBytes: 1,
    bodySha256: '1'.repeat(64),
  }
  const cli = {
    spawnError: null,
    timedOut: false,
    captureOverflow: false,
    code: 0,
    parsed: {
      wireTextSeen: true,
      hasNonzeroInput: true,
      hasNonzeroOutput: true,
      thinkingSeen: true,
    },
  }
  const thinkingVariants = [
    undefined,
    null,
    { type: 'adaptive', display: 'summarized' },
    { type: 'future-observed-value' },
  ]
  const captureResults = []
  for (const endpoint of ENDPOINTS) {
    for (const thinking of thinkingVariants) {
      const result = validateCase({
        endpoint,
        effort: 'max',
        cli,
        inboundRecords: [{ facts: inbound }],
        wireRecords: [{ facts: contractWireFacts(endpoint, thinking) }],
      })
      captureResults.push({ endpoint, thinking: thinking ?? null, violations: result.violations })
    }
  }

  const protocolCases = [
    {
      endpoint: 'cli',
      kind: 'model_discovery',
      method: 'POST',
      url: 'http://127.0.0.1/thinking-wire/?origin=KIRO_CLI',
      headers: {
        host: 'management.us-east-1.kiro.dev',
        tokentype: 'API_KEY',
        accept: '*/*',
        'content-type': 'application/x-amz-json-1.0',
        'x-amz-target': 'AmazonCodeWhispererService.ListAvailableModels',
      },
      body: { origin: 'KIRO_CLI' },
    },
    {
      endpoint: 'ide',
      kind: 'model_discovery',
      method: 'GET',
      url: 'http://127.0.0.1/thinking-wire/ListAvailableModels?origin=AI_EDITOR&maxResults=50',
      headers: {
        host: 'q.us-east-1.amazonaws.com',
        tokentype: 'API_KEY',
        accept: 'application/json',
      },
      body: null,
    },
    {
      endpoint: 'cli',
      kind: 'inference',
      method: 'POST',
      url: 'http://127.0.0.1/thinking-wire/',
      headers: {
        host: 'runtime.us-east-1.kiro.dev',
        tokentype: 'API_KEY',
        'content-type': 'application/x-amz-json-1.0',
        'x-amz-target': 'AmazonCodeWhispererStreamingService.GenerateAssistantResponse',
      },
      body: { conversationState: { currentMessage: { userInputMessage: { origin: 'KIRO_CLI' } } } },
    },
    {
      endpoint: 'ide',
      kind: 'inference',
      method: 'POST',
      url: 'http://127.0.0.1/thinking-wire/generateAssistantResponse',
      headers: {
        host: 'q.us-east-1.amazonaws.com',
        tokentype: 'API_KEY',
        'content-type': 'application/json',
      },
      body: { conversationState: { currentMessage: { userInputMessage: { origin: 'AI_EDITOR' } } } },
    },
  ]
  const protocolResults = protocolCases.map((fixture) => exactProtocolViolations({
    endpoint: fixture.endpoint,
    kind: fixture.kind,
    request: { method: fixture.method, headers: fixture.headers },
    url: new URL(fixture.url),
    body: fixture.body,
  }))
  const mutatedProtocol = exactProtocolViolations({
    endpoint: 'cli',
    kind: 'inference',
    request: { method: 'GET', headers: {} },
    url: new URL('http://127.0.0.1/wrong'),
    body: {},
  })
  const serviceEnvironment = isolatedServiceEnvironment('cli', 19022)
  const claudeEnvironment = isolatedClaudeEnvironment('/tmp/contract-home', '/tmp/contract-config', 19023)
  const forbiddenInheritedNames = [
    'AWS_ACCESS_KEY_ID',
    'AWS_SECRET_ACCESS_KEY',
    'AWS_SESSION_TOKEN',
    'ANTHROPIC_API_KEY',
    'ANTHROPIC_AUTH_TOKEN',
    'CLAUDE_CODE_OAUTH_TOKEN',
    'KIRO_API_KEY',
    'KIRO_RS_POSTGRES_URL',
    'KIRO_RS_REDIS_URL',
    'DATABASE_URL',
    'REDIS_URL',
    'PGPASSWORD',
    'HTTP_PROXY',
    'HTTPS_PROXY',
  ]
  process.stdout.write(`${JSON.stringify({
    captureResults,
    effort: {
      explicitMax: expectedWireEffort(inbound),
      disabled: expectedWireEffort({
        thinking: { type: 'disabled' },
        outputConfig: { effort: 'max' },
      }),
      schema: FAKE_EFFORT_SCHEMA,
    },
    models: FAKE_MODEL_IDS,
    protocolResults,
    mutatedProtocol,
    environment: {
      serviceInheritedForbidden: forbiddenInheritedNames.filter((name) => (
        process.env[name] !== undefined && serviceEnvironment[name] === process.env[name]
      )),
      claudeInheritedForbidden: forbiddenInheritedNames.filter((name) => (
        process.env[name] !== undefined && claudeEnvironment[name] === process.env[name]
      )),
      postgresPinned: serviceEnvironment.KIRO_RS_POSTGRES_URL === POSTGRES_URLS.cli,
      redisPinned: serviceEnvironment.KIRO_RS_REDIS_URL === REDIS_URL,
      migratePinned: serviceEnvironment.KIRO_RS_POSTGRES_MIGRATE_ON_START === 'true',
      compressionPinned:
        serviceEnvironment.KIRO_RS_POSTGRES_COMPRESS_USAGE_ROLLUPS_ON_START === 'false',
      claudeBaseUrlPinned: claudeEnvironment.ANTHROPIC_BASE_URL === 'http://127.0.0.1:19023/cc',
      claudeHomePinned: claudeEnvironment.HOME === '/tmp/contract-home',
    },
    isolation: {
      databases: Object.fromEntries(ENDPOINTS.map((endpoint) => [
        endpoint,
        isolation[endpoint].database,
      ])),
      markers: Object.fromEntries(ENDPOINTS.map((endpoint) => [
        endpoint,
        isolation[endpoint].marker,
      ])),
      invalidDatabaseRejected,
      externalCwdAccepted,
      repositoryCwdRejected,
    },
    lifecycle: {
      shutdownSpawnRejected,
      nonDetachedSpawnRejected,
      matchingIdentityAccepted,
      reusedLeaderRejected,
      reusedMemberRejected,
      missingIdentityRejected,
      sentinelInitiallyIntact,
      sentinelCorruptionRejected,
      sentinelRetryCannotMaskCorruption,
    },
    reportPolicy,
  })}\n`)
}

if (FIXTURE_MODE === 'contract') {
  try {
    runContractFixture()
  } catch (error) {
    process.stderr.write(`thinking wire contract fixture failed: ${error.message}\n`)
    process.exitCode = 1
  }
} else if (SIGNAL_FIXTURE_MODE) {
  runSignalFixture().catch(async (error) => {
    shutdownExitCode ??= 1
    await cleanupOwnedRuntime({ redisPasses: 2 }).catch(() => {})
    process.stderr.write(`thinking wire signal fixture failed: ${error.message}\n`)
    process.exitCode = 1
  })
} else {
  main().catch(async (error) => {
    if (error instanceof ShutdownRequested || shutdownExitCode !== null) return
    shutdownExitCode ??= 1
    await cleanupOwnedRuntime({ redisPasses: 4 }).catch(() => {})
    process.stderr.write(`thinking effort Kiro wire validation failed: ${error.message}\n`)
    process.exitCode = 1
  })
}
