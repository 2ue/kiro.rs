#!/usr/bin/env node

import assert from 'node:assert/strict'
import fs from 'node:fs'
import net from 'node:net'
import os from 'node:os'
import path from 'node:path'
import { spawn } from 'node:child_process'

const ROOT = fs.realpathSync(path.resolve(import.meta.dirname, '../..'))
const REDIS_URL = requiredEnvironment('KIRO_TOKEN_REFRESH_CLUSTER_REDIS_URL')
const POSTGRES_URL = requiredEnvironment('KIRO_TOKEN_REFRESH_CLUSTER_POSTGRES_URL')
const REDIS_ISOLATED = process.env.KIRO_RS_TEST_REDIS_ISOLATED === '1'
const POSTGRES_ISOLATED = process.env.KIRO_RS_TEST_POSTGRES_ISOLATED === '1'
const OUTER_ROUNDS = boundedInteger('KIRO_TOKEN_REFRESH_CLUSTER_OUTER_ROUNDS', 3, 1, 10)
const SCOPE = process.env.KIRO_TOKEN_REFRESH_CLUSTER_SCOPE || 'token-refresh-cluster'
const ACTIVE_CHILDREN = new Set()
let redisTarget
let postgresTarget
let redisWasEmpty = false
let tempRoot
let cleanupPromise
let signalHandling = false

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

function loopback(hostname) {
  return hostname === '127.0.0.1' || hostname === 'localhost' || hostname === '::1'
}

function validateInputs() {
  if (!REDIS_ISOLATED) throw new Error('KIRO_RS_TEST_REDIS_ISOLATED=1 is required')
  if (!POSTGRES_ISOLATED) throw new Error('KIRO_RS_TEST_POSTGRES_ISOLATED=1 is required')
  if (!/^[a-z0-9][a-z0-9._-]{0,63}$/.test(SCOPE)) {
    throw new Error('KIRO_TOKEN_REFRESH_CLUSTER_SCOPE has an invalid format')
  }

  const redis = new URL(REDIS_URL)
  if (redis.protocol !== 'redis:') throw new Error('Redis URL must use redis://')
  if (!loopback(redis.hostname)) throw new Error('Redis URL must target loopback')
  if (redis.search || redis.hash) throw new Error('Redis URL must not contain query or fragment data')
  const databaseText = redis.pathname.replace(/^\//, '')
  if (!/^\d+$/.test(databaseText)) throw new Error('Redis URL must name a database')
  const database = Number(databaseText)
  if (!Number.isSafeInteger(database) || database < 1 || database > 15) {
    throw new Error('Redis URL must use an isolated nonzero database in 1..15')
  }
  const redisPort = Number(redis.port || 6379)
  if (redisPort === 9022) throw new Error('port 9022 is protected')

  const postgres = new URL(POSTGRES_URL)
  if (!['postgres:', 'postgresql:'].includes(postgres.protocol)) {
    throw new Error('PostgreSQL URL must use postgres:// or postgresql://')
  }
  if (!loopback(postgres.hostname)) throw new Error('PostgreSQL URL must target loopback')
  if (!postgres.pathname || postgres.pathname === '/') {
    throw new Error('PostgreSQL URL must name a database')
  }
  const postgresPort = Number(postgres.port || 5432)
  if (postgresPort === 9022) throw new Error('port 9022 is protected')
  return { redis: { redis, database, port: redisPort }, postgres: { postgres, port: postgresPort } }
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
  throw new Error(`unsupported Redis control reply ${type}`)
}

function redisCommand(target, command) {
  const commands = []
  if (target.redis.password) {
    const password = decodeURIComponent(target.redis.password)
    commands.push(target.redis.username
      ? ['AUTH', decodeURIComponent(target.redis.username), password]
      : ['AUTH', password])
  }
  commands.push(['SELECT', String(target.database)], command)
  const payload = encodeRedisCommands(commands)
  return new Promise((resolve, reject) => {
    const socket = net.connect({ host: target.redis.hostname, port: target.port })
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

function probeTcp(host, port, label) {
  return new Promise((resolve, reject) => {
    const socket = net.connect({ host, port })
    let settled = false
    const finish = (error) => {
      if (settled) return
      settled = true
      socket.destroy()
      if (error) reject(error)
      else resolve()
    }
    socket.setTimeout(5_000)
    socket.once('connect', () => finish())
    socket.once('timeout', () => finish(new Error(`${label} TCP probe timed out`)))
    socket.once('error', () => finish(new Error(`${label} TCP probe failed`)))
  })
}

async function stopChild(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return
  const waitForExit = (timeoutMs) => new Promise((resolve) => {
    if (child.exitCode !== null || child.signalCode !== null) return resolve(true)
    let timer
    const onExit = () => { clearTimeout(timer); resolve(true) }
    child.once('exit', onExit)
    timer = setTimeout(() => { child.off('exit', onExit); resolve(false) }, timeoutMs)
  })
  try { process.kill(-child.pid, 'SIGTERM') } catch { child.kill('SIGTERM') }
  if (!(await waitForExit(10_000)) && child.exitCode === null && child.signalCode === null) {
    try { process.kill(-child.pid, 'SIGKILL') } catch { child.kill('SIGKILL') }
    if (!(await waitForExit(10_000))) throw new Error(`owned child process group ${child.pid} did not exit`)
  }
  ACTIVE_CHILDREN.delete(child)
}

async function cleanup() {
  if (cleanupPromise) return cleanupPromise
  cleanupPromise = (async () => {
    await Promise.all([...ACTIVE_CHILDREN].map((child) => stopChild(child)))
    let databaseEmpty = false
    let residualKeyCount = null
    if (redisWasEmpty && redisTarget) {
      const size = await redisCommand(redisTarget, ['DBSIZE']).catch(() => null)
      residualKeyCount = Number.isSafeInteger(size) ? size : null
      databaseEmpty = size === 0
    }
    if (tempRoot) fs.rmSync(tempRoot, { recursive: true, force: true })
    return {
      childGroupsStopped: ACTIVE_CHILDREN.size === 0,
      redisDatabaseEmpty: databaseEmpty,
      redisDatabaseFlushed: false,
      residualKeyCount,
      tempRemoved: !tempRoot || !fs.existsSync(tempRoot),
    }
  })()
  return cleanupPromise
}

for (const [signal, exitCode] of [['SIGHUP', 129], ['SIGINT', 130], ['SIGTERM', 143]]) {
  process.on(signal, () => {
    if (signalHandling) return
    signalHandling = true
    void cleanup().finally(() => { process.exitCode = exitCode })
  })
}

async function main() {
  const targets = validateInputs()
  redisTarget = targets.redis
  postgresTarget = targets.postgres
  tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), `kiro-token-refresh-cluster-${process.pid}-`))
  const before = await redisCommand(redisTarget, ['DBSIZE'])
  if (before !== 0) throw new Error(`isolated Redis database ${redisTarget.database} is not empty (${before} keys)`)
  redisWasEmpty = true
  await probeTcp(postgresTarget.postgres.hostname, postgresTarget.port, 'PostgreSQL')

  const tests = [
    'kiro::token_manager::manager::tests::refresh_cluster_tests::token_refresh_two_manager_rotating_and_non_rotating_share_one_send_and_pg_authority_for_five_rounds',
    'kiro::token_manager::manager::tests::refresh_cluster_tests::token_refresh_two_manager_pg_cas_fences_stale_rotating_and_non_rotating_results_for_five_rounds',
    'kiro::token_manager::manager::tests::refresh_cluster_tests::token_refresh_two_manager_failure_replay_and_cancelled_leader_recover_without_send_amplification_for_five_rounds',
    'kiro::token_manager::manager::tests::refresh_cluster_tests::token_refresh_two_manager_cancelled_health_claim_is_reclaimed_once_for_five_rounds',
    'storage::postgres::tests::postgres_refresh_field_cas_fences_non_rotating_refresh_by_access_token_for_five_rounds',
    'storage::redis_cache::tests::token_refresh_redis_stale_leader_cannot_overwrite_success_for_five_rounds',
    'storage::redis_cache::tests::token_refresh_redis_failure_replay_health_claim_and_identity_are_fenced_for_five_rounds',
  ]
  const commandScript = [
    'set -euo pipefail',
    'cargo fmt --all -- --check',
    'git diff --check',
    ...Array.from({ length: OUTER_ROUNDS }, (_, index) => [
      `echo token-refresh-cluster outer_round=${index + 1}`,
      ...tests.map((testName) => `cargo test ${testName} -- --exact --nocapture --test-threads=1`),
    ]).flat(),
  ].join('\n')
  const child = spawn(path.join(ROOT, 'feature/tests/run-cargo-scoped.sh'), [
    SCOPE, '--', 'env',
    'RUSTUP_TOOLCHAIN=1.92.0',
    `KIRO_RS_TEST_REDIS_URL=${REDIS_URL}`,
    `KIRO_RS_TEST_POSTGRES_URL=${POSTGRES_URL}`,
    'KIRO_RS_REQUIRE_STORAGE_TESTS=1',
    'bash', '-lc', commandScript,
  ], {
    cwd: ROOT,
    env: {
      PATH: process.env.PATH || '/usr/bin:/bin',
      HOME: process.env.HOME || os.homedir(),
      TMPDIR: tempRoot,
      KIRO_RS_TEST_REDIS_URL: REDIS_URL,
      KIRO_RS_TEST_POSTGRES_URL: POSTGRES_URL,
      KIRO_RS_REQUIRE_STORAGE_TESTS: '1',
    },
    detached: true,
    stdio: 'inherit',
  })
  ACTIVE_CHILDREN.add(child)
  child.once('exit', () => ACTIVE_CHILDREN.delete(child))
  const exit = await new Promise((resolve) => {
    child.once('exit', (code, signal) => resolve({ code, signal }))
    child.once('error', (error) => resolve({ code: null, signal: null, error }))
  })
  assert.equal(exit.code, 0, `scoped token-refresh cluster matrix failed: ${JSON.stringify(exit)}`)
  assert.equal(await redisCommand(redisTarget, ['DBSIZE']), 0, 'refresh cluster left Redis keys behind')
  const cleaned = await cleanup()
  assert.deepEqual(cleaned, {
    childGroupsStopped: true,
    redisDatabaseEmpty: true,
    redisDatabaseFlushed: false,
    residualKeyCount: 0,
    tempRemoved: true,
  })
  process.stdout.write(`${JSON.stringify({
    result: 'pass',
    scope: SCOPE,
    outerRounds: OUTER_ROUNDS,
    tests: tests.length,
    internalRoundsPerTest: 5,
    redisDatabase: redisTarget.database,
    postgresDatabase: postgresTarget.postgres.pathname.slice(1),
    protected9022ProbeSkipped: true,
    dockerStarted: false,
    cleanup: cleaned,
  }, null, 2)}\n`)
}

main().catch(async (error) => {
  const cleaned = await cleanup().catch(() => null)
  if (!signalHandling) {
    process.stderr.write(`token-refresh cluster validation failed: ${error.message}; cleanup=${JSON.stringify(cleaned)}\n`)
    process.exitCode = 1
  }
})
