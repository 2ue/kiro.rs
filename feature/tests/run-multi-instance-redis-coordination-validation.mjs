#!/usr/bin/env node

import assert from 'node:assert/strict'
import fs from 'node:fs'
import net from 'node:net'
import os from 'node:os'
import path from 'node:path'
import { spawn } from 'node:child_process'

const ROOT = fs.realpathSync(path.resolve(import.meta.dirname, '../..'))
const REDIS_URL = requiredEnvironment('KIRO_MULTI_INSTANCE_REDIS_URL')
const ISOLATED = process.env.KIRO_RS_TEST_REDIS_ISOLATED === '1'
const OUTER_ROUNDS = boundedInteger('KIRO_MULTI_INSTANCE_REDIS_OUTER_ROUNDS', 3, 1, 10)
const SCOPE = process.env.KIRO_MULTI_INSTANCE_REDIS_SCOPE || 'multi-instance-redis-coordination'
const ACTIVE_CHILDREN = new Set()
let redisTarget
let tempRoot
let initialDatabaseEmpty = false
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

function validateInputs() {
  if (!ISOLATED) throw new Error('KIRO_RS_TEST_REDIS_ISOLATED=1 is required')
  if (!/^[a-z0-9][a-z0-9._-]{0,63}$/.test(SCOPE)) {
    throw new Error('KIRO_MULTI_INSTANCE_REDIS_SCOPE has an invalid format')
  }
  const redis = new URL(REDIS_URL)
  if (redis.protocol !== 'redis:') throw new Error('Redis URL must use redis://')
  if (!['127.0.0.1', 'localhost', '::1'].includes(redis.hostname)) {
    throw new Error('Redis URL must target loopback')
  }
  if (redis.search || redis.hash) throw new Error('Redis URL must not contain query or fragment data')
  const databaseText = redis.pathname.replace(/^\//, '')
  if (!/^\d+$/.test(databaseText)) throw new Error('Redis URL must name a database')
  const database = Number(databaseText)
  if (!Number.isSafeInteger(database) || database < 1 || database > 15) {
    throw new Error('Redis URL must use an isolated nonzero database in 1..15')
  }
  const port = Number(redis.port || 6379)
  if (port === 9022) throw new Error('port 9022 is protected')
  return { redis, database, port }
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

function redisCommands(target, command) {
  const commands = []
  if (target.redis.password) {
    const password = decodeURIComponent(target.redis.password)
    if (target.redis.username) {
      commands.push(['AUTH', decodeURIComponent(target.redis.username), password])
    } else {
      commands.push(['AUTH', password])
    }
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
    socket.once('timeout', () => finish(new Error('Redis control command timed out')))
    socket.once('error', (error) => finish(error))
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
    if (initialDatabaseEmpty && redisTarget) {
      const size = await redisCommands(redisTarget, ['DBSIZE']).catch(() => null)
      residualKeyCount = Number.isSafeInteger(size) ? size : null
      databaseEmpty = size === 0
    }
    if (tempRoot) fs.rmSync(tempRoot, { recursive: true, force: true })
    return {
      childGroupsStopped: ACTIVE_CHILDREN.size === 0,
      databaseEmpty,
      databaseFlushed: false,
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
  redisTarget = validateInputs()
  tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), `kiro-multi-instance-redis-${process.pid}-`))
  const before = await redisCommands(redisTarget, ['DBSIZE'])
  if (before !== 0) {
    throw new Error(`isolated Redis database ${redisTarget.database} is not empty (${before} keys)`)
  }
  initialDatabaseEmpty = true

  const testName = 'kiro::token_manager::manager::tests::redis_two_instance_connections_preserve_lease_queue_and_rpm_authority_for_five_rounds'
  const script = `
set -euo pipefail
cargo fmt --all -- --check
git diff --check
for round in $(seq 1 ${OUTER_ROUNDS}); do
  echo "multi-instance-redis outer_round=$round"
  cargo test ${testName} -- --exact --nocapture --test-threads=1
done
`
  const child = spawn(path.join(ROOT, 'feature/tests/run-cargo-scoped.sh'), [
    SCOPE, '--', 'env',
    'RUSTUP_TOOLCHAIN=1.92.0',
    `KIRO_RS_TEST_REDIS_URL=${REDIS_URL}`,
    'KIRO_RS_REQUIRE_STORAGE_TESTS=1',
    'bash', '-lc', script,
  ], {
    cwd: ROOT,
    env: {
      PATH: process.env.PATH || '/usr/bin:/bin',
      HOME: process.env.HOME || os.homedir(),
      TMPDIR: tempRoot,
      KIRO_RS_TEST_REDIS_URL: REDIS_URL,
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
  assert.equal(exit.code, 0, `scoped multi-instance Redis matrix failed: ${JSON.stringify(exit)}`)
  const after = await redisCommands(redisTarget, ['DBSIZE'])
  assert.equal(after, 0, 'multi-instance fixture left keys in its isolated Redis database')

  const cleaned = await cleanup()
  assert.deepEqual(cleaned, {
    childGroupsStopped: true,
    databaseEmpty: true,
    databaseFlushed: false,
    residualKeyCount: 0,
    tempRemoved: true,
  })
  process.stdout.write(`${JSON.stringify({
    result: 'pass',
    scope: SCOPE,
    outerRounds: OUTER_ROUNDS,
    internalRoundsPerInvocation: 5,
    totalCoordinationRounds: OUTER_ROUNDS * 5,
    redisDatabase: redisTarget.database,
    protected9022ProbeSkipped: true,
    cleanup: cleaned,
  }, null, 2)}\n`)
}

main().catch(async (error) => {
  const cleaned = await cleanup().catch(() => null)
  if (!signalHandling) {
    process.stderr.write(`multi-instance Redis validation failed: ${error.message}; cleanup=${JSON.stringify(cleaned)}\n`)
    process.exitCode = 1
  }
})
