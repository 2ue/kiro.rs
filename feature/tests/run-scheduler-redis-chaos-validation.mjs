#!/usr/bin/env node

import assert from 'node:assert/strict'
import fs from 'node:fs'
import net from 'node:net'
import os from 'node:os'
import path from 'node:path'
import { spawn, spawnSync } from 'node:child_process'

const ROOT = fs.realpathSync(path.resolve(import.meta.dirname, '../..'))
const DIRECT_REDIS_URL = requiredEnvironment('KIRO_SCHEDULER_CHAOS_REDIS_DIRECT_URL')
const ISOLATED = process.env.KIRO_RS_TEST_REDIS_ISOLATED === '1'
const OUTER_ROUNDS = boundedInteger('KIRO_SCHEDULER_CHAOS_OUTER_ROUNDS', 3, 1, 5)
const SCOPE = process.env.KIRO_SCHEDULER_CHAOS_SCOPE || 'scheduler-redis-chaos-real'
const TEMP_ROOT = fs.mkdtempSync(path.join(os.tmpdir(), `kiro-scheduler-chaos-${process.pid}-`))
let testReadyFile = null
const ACTIVE_CHILDREN = new Set()
let proxy = null
let proxyInfo = null
let initialDatabaseEmpty = false
let cleanupPromise = null

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

function optionalTestReadyFile() {
  const raw = String(process.env.KIRO_SCHEDULER_CHAOS_TEST_READY_FILE || '').trim()
  if (!raw) return null
  if (!path.isAbsolute(raw)) {
    throw new Error('KIRO_SCHEDULER_CHAOS_TEST_READY_FILE must be an absolute path')
  }
  const parent = path.dirname(raw)
  if (!fs.existsSync(parent)) {
    throw new Error('KIRO_SCHEDULER_CHAOS_TEST_READY_FILE parent must exist')
  }
  const parentReal = fs.realpathSync(parent)
  if (parentReal === ROOT || parentReal.startsWith(`${ROOT}${path.sep}`)) {
    throw new Error('KIRO_SCHEDULER_CHAOS_TEST_READY_FILE must be outside the repository')
  }
  if (fs.existsSync(raw)) {
    throw new Error('KIRO_SCHEDULER_CHAOS_TEST_READY_FILE must not already exist')
  }
  return raw
}

function validateInputs() {
  if (!ISOLATED) {
    throw new Error('KIRO_RS_TEST_REDIS_ISOLATED=1 is required')
  }
  if (!/^[a-z0-9][a-z0-9._-]{0,63}$/.test(SCOPE)) {
    throw new Error('KIRO_SCHEDULER_CHAOS_SCOPE has an invalid format')
  }
  const redis = new URL(DIRECT_REDIS_URL)
  if (redis.protocol !== 'redis:') throw new Error('direct Redis URL must use redis://')
  if (!['127.0.0.1', 'localhost', '::1'].includes(redis.hostname)) {
    throw new Error('direct Redis URL must target loopback')
  }
  const databaseText = redis.pathname.replace(/^\//, '')
  if (!/^\d+$/.test(databaseText)) throw new Error('direct Redis URL must name a database')
  const database = Number(databaseText)
  if (!Number.isSafeInteger(database) || database < 1 || database > 15) {
    throw new Error('direct Redis URL must use an isolated nonzero database in 1..15')
  }
  const port = Number(redis.port || 6379)
  if (port === 9022) throw new Error('port 9022 is protected')
  return { redis, database, port }
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

function listeningPids(port) {
  const result = spawnSync('lsof', ['-nP', `-iTCP:${port}`, '-sTCP:LISTEN', '-t'], {
    cwd: ROOT,
    encoding: 'utf8',
    maxBuffer: 1024 * 1024,
  })
  if (result.status !== 0 && result.status !== 1) throw new Error(`lsof failed for port ${port}`)
  return String(result.stdout || '').split(/\s+/).filter(Boolean).map(Number)
}

async function stopChild(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return
  const waitForExit = (timeoutMs) => new Promise((resolve) => {
    if (child.exitCode !== null || child.signalCode !== null) {
      resolve(true)
      return
    }
    let timer
    const onExit = () => {
      clearTimeout(timer)
      resolve(true)
    }
    child.once('exit', onExit)
    timer = setTimeout(() => {
      child.off('exit', onExit)
      resolve(false)
    }, timeoutMs)
  })
  try { process.kill(-child.pid, 'SIGTERM') } catch { child.kill('SIGTERM') }
  const stopped = await waitForExit(5_000)
  if (!stopped && child.exitCode === null && child.signalCode === null) {
    try { process.kill(-child.pid, 'SIGKILL') } catch { child.kill('SIGKILL') }
    const killed = await waitForExit(5_000)
    if (!killed) throw new Error(`owned child process group ${child.pid} did not exit`)
  }
  ACTIVE_CHILDREN.delete(child)
}

async function fetchJson(url, options = {}) {
  const response = await fetch(url, options)
  const text = await response.text()
  const body = text ? JSON.parse(text) : {}
  if (!response.ok) throw new Error(`${options.method || 'GET'} ${url} -> ${response.status}: ${text}`)
  return body
}

async function startProxy({ redis, database, port }) {
  const proxyPort = await reservePort()
  const apiPort = await reservePort()
  assert.deepEqual(listeningPids(proxyPort), [])
  assert.deepEqual(listeningPids(apiPort), [])
  const child = spawn(process.execPath, [
    path.join(import.meta.dirname, 'redis-chaos-proxy.mjs'),
    '--listen-port', String(proxyPort),
    '--api-port', String(apiPort),
    '--upstream-host', redis.hostname,
    '--upstream-port', String(port),
    '--database', String(database),
    '--name', 'redis',
    '--allow-flush',
  ], {
    cwd: TEMP_ROOT,
    env: { PATH: process.env.PATH || '/usr/bin:/bin', TMPDIR: os.tmpdir() },
    detached: true,
    stdio: ['ignore', 'pipe', 'pipe'],
  })
  ACTIVE_CHILDREN.add(child)
  child.once('exit', () => ACTIVE_CHILDREN.delete(child))
  const stderr = []
  child.stderr.on('data', (chunk) => {
    if (Buffer.concat(stderr).length < 64 * 1024) stderr.push(chunk)
  })
  const line = await Promise.race([
    new Promise((resolve, reject) => {
      let pending = ''
      child.stdout.on('data', (chunk) => {
        pending += chunk.toString('utf8')
        const end = pending.indexOf('\n')
        if (end >= 0) resolve(pending.slice(0, end))
      })
      child.once('error', reject)
      child.once('exit', (code) => reject(new Error(
        `chaos proxy exited before readiness: ${code} ${Buffer.concat(stderr).toString('utf8')}`,
      )))
    }),
    new Promise((_, reject) => setTimeout(() => reject(new Error('chaos proxy readiness timeout')), 10_000)),
  ])
  const info = JSON.parse(line)
  assert.equal(info.ready, true)
  assert.equal(info.proxyPort, proxyPort)
  assert.equal(info.apiPort, apiPort)
  assert.equal(info.upstreamDatabase, database)
  assert.equal(info.protected9022ProbeSkipped, true)
  assert.deepEqual(listeningPids(proxyPort), [child.pid])
  assert.deepEqual(listeningPids(apiPort), [child.pid])
  return { child, ...info }
}

async function flushOwnedDatabase() {
  if (!proxyInfo || !initialDatabaseEmpty) return false
  await fetchJson(`http://127.0.0.1:${proxyInfo.apiPort}/database/flush`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ confirm: 'isolated' }),
  })
  const after = await fetchJson(`http://127.0.0.1:${proxyInfo.apiPort}/database/size`)
  return after.size === 0
}

async function cleanup() {
  if (cleanupPromise) return cleanupPromise
  cleanupPromise = (async () => {
    let databaseEmpty = false
    const nonProxyChildren = [...ACTIVE_CHILDREN].filter((child) => child !== proxy)
    await Promise.all(nonProxyChildren.map((child) => stopChild(child)))
    if (proxyInfo && proxy?.exitCode === null) {
      try {
        await fetchJson(`http://127.0.0.1:${proxyInfo.apiPort}/proxies/redis`, {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ enabled: true }),
        })
        databaseEmpty = await flushOwnedDatabase()
      } catch {}
    }
    await Promise.all([...ACTIVE_CHILDREN].map((child) => stopChild(child)))
    const portsReleased = !proxyInfo || (
      listeningPids(proxyInfo.proxyPort).length === 0
      && listeningPids(proxyInfo.apiPort).length === 0
    )
    fs.rmSync(TEMP_ROOT, { recursive: true, force: true })
    if (testReadyFile) fs.rmSync(testReadyFile, { force: true })
    return {
      databaseEmpty,
      childGroupsStopped: ACTIVE_CHILDREN.size === 0,
      portsReleased,
      tempRemoved: !fs.existsSync(TEMP_ROOT),
    }
  })()
  return cleanupPromise
}

async function holdForSignalAfterProxyReady() {
  if (!testReadyFile) return true
  const payload = {
    ready: true,
    pid: process.pid,
    proxyPid: proxyInfo.child.pid,
    tempRoot: TEMP_ROOT,
    proxyPort: proxyInfo.proxyPort,
    apiPort: proxyInfo.apiPort,
    database: proxyInfo.upstreamDatabase,
  }
  fs.writeFileSync(testReadyFile, `${JSON.stringify(payload)}\n`, {
    encoding: 'utf8',
    flag: 'wx',
    mode: 0o600,
  })
  while (!signalHandling) {
    await new Promise((resolve) => setTimeout(resolve, 20))
  }
  return false
}

let signalHandling = false
for (const [signal, code] of [['SIGHUP', 129], ['SIGINT', 130], ['SIGTERM', 143]]) {
  process.on(signal, () => {
    if (signalHandling) return
    signalHandling = true
    void cleanup().finally(() => { process.exitCode = code })
  })
}

async function main() {
  const redisTarget = validateInputs()
  testReadyFile = optionalTestReadyFile()
  proxyInfo = await startProxy(redisTarget)
  proxy = proxyInfo.child
  const before = await fetchJson(`http://127.0.0.1:${proxyInfo.apiPort}/database/size`)
  if (before.size !== 0) {
    throw new Error(`isolated Redis database ${redisTarget.database} is not empty (${before.size} keys)`)
  }
  initialDatabaseEmpty = true

  if (!(await holdForSignalAfterProxyReady())) return

  const proxyRedis = new URL(DIRECT_REDIS_URL)
  proxyRedis.hostname = '127.0.0.1'
  proxyRedis.port = String(proxyInfo.proxyPort)
  const testNames = [
    'kiro::token_manager::manager::tests::redis_affinity_latency_does_not_degrade_capacity_coordination',
    'kiro::token_manager::manager::tests::redis_capacity_latency_boundary_and_recovery_matrix',
    'kiro::token_manager::manager::tests::redis_capacity_consecutive_timeouts_open_breaker_without_all_disabled',
    'kiro::token_manager::manager::tests::redis_lease_release_is_non_blocking_under_latency_and_burst',
    'kiro::token_manager::manager::tests::redis_capacity_disconnect_reconnect_recovers_same_manager',
    'kiro::token_manager::manager::tests::redis_usage_writer_and_scheduler_joint_fault_matrix_recovers_without_spin_or_false_disable',
    'kiro::token_manager::manager::tests::cancelled_provisional_redis_acquire_rolls_back_local_and_tombstones_remote',
    'kiro::token_manager::manager::tests::redis_commit_unknown_provisional_acquire_leaves_no_lease',
  ]
  const testCommands = testNames.map((name) => (
    `cargo test ${name} -- --exact --nocapture --test-threads=1`
  )).join('\n')
  const script = `
set -euo pipefail
cargo fmt --all -- --check
git diff --check
for round in $(seq 1 ${OUTER_ROUNDS}); do
  echo "scheduler-redis-chaos outer_round=$round"
${testCommands.split('\n').map((line) => `  ${line}`).join('\n')}
done
`
  const command = spawn(path.join(ROOT, 'feature/tests/run-cargo-scoped.sh'), [
    SCOPE, '--', 'env',
    'RUSTUP_TOOLCHAIN=1.92.0',
    `KIRO_RS_TEST_REDIS_URL=${proxyRedis.toString()}`,
    'KIRO_RS_REQUIRE_STORAGE_TESTS=1',
    `KIRO_RS_TEST_TOXIPROXY_API=http://127.0.0.1:${proxyInfo.apiPort}`,
    'KIRO_RS_TEST_TOXIPROXY_NAME=redis',
    'bash', '-lc', script,
  ], {
    cwd: ROOT,
    env: {
      PATH: process.env.PATH || '/usr/bin:/bin',
      HOME: process.env.HOME || os.homedir(),
      TMPDIR: process.env.TMPDIR || os.tmpdir(),
      KIRO_RS_TEST_REDIS_URL: proxyRedis.toString(),
      KIRO_RS_REQUIRE_STORAGE_TESTS: '1',
      KIRO_RS_TEST_TOXIPROXY_API: `http://127.0.0.1:${proxyInfo.apiPort}`,
      KIRO_RS_TEST_TOXIPROXY_NAME: 'redis',
    },
    detached: true,
    stdio: 'inherit',
  })
  ACTIVE_CHILDREN.add(command)
  command.once('exit', () => ACTIVE_CHILDREN.delete(command))
  const exit = await new Promise((resolve) => {
    command.once('exit', (code, signal) => resolve({ code, signal }))
    command.once('error', (error) => resolve({ code: null, signal: null, error }))
  })
  assert.equal(exit.code, 0, `scoped Cargo chaos matrix failed: ${JSON.stringify(exit)}`)
  const result = {
    result: 'pass',
    scope: SCOPE,
    outerRounds: OUTER_ROUNDS,
    exactTests: testNames.length,
    exactInvocations: testNames.length * OUTER_ROUNDS,
    redisDatabase: redisTarget.database,
    protected9022ProbeSkipped: true,
  }
  const cleaned = await cleanup()
  assert.deepEqual(cleaned, {
    databaseEmpty: true,
    childGroupsStopped: true,
    portsReleased: true,
    tempRemoved: true,
  })
  process.stdout.write(`${JSON.stringify({ ...result, cleanup: cleaned }, null, 2)}\n`)
}

main().catch(async (error) => {
  const cleaned = await cleanup().catch(() => null)
  if (!signalHandling) {
    process.stderr.write(`scheduler Redis chaos validation failed: ${error.message}; cleanup=${JSON.stringify(cleaned)}\n`)
    process.exitCode = 1
  }
})
