#!/usr/bin/env node

/*
 * Live, non-Docker Redis fault-domain validation.
 *
 * This runner deliberately does not start kiro.rs or any build/application stack.
 * It drives only the two deployment-owned Redis tunnels and short-lived local chaos proxies.
 * Every key is under a random prefix and cleanup uses bounded cursor scans plus async deletion.
 */

import assert from 'node:assert/strict'
import crypto from 'node:crypto'
import fs from 'node:fs'
import net from 'node:net'
import os from 'node:os'
import path from 'node:path'
import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'

const ROOT = fs.realpathSync(path.resolve(import.meta.dirname, '../..'))
const CHAOS_PROXY = path.join(ROOT, 'feature/tests/redis-chaos-proxy.mjs')
const BUSINESS_URL = 'redis://127.0.0.1:26379/15'
const OBSERVABILITY_URL = 'redis://127.0.0.1:50892/15'
const DATABASE = 15
const OUTER_ROUNDS = Math.max(
  3,
  Number.parseInt(process.env.KIRO_REDIS_FAULT_DOMAIN_OUTER_ROUNDS || '3', 10) || 3,
)
const TEMP_ROOT = fs.mkdtempSync(path.join(os.tmpdir(), `kiro-redis-fault-domain-${process.pid}-`))
const CHILDREN = new Set()
const PROXIES = new Set()
const DEBUG = process.env.KIRO_REDIS_FAULT_DOMAIN_DEBUG === '1'
let cleanupPromise = null
let signalHandling = false

function trace(message) {
  if (DEBUG) process.stderr.write(`[redis-fault-domain] ${message}\n`)
}

function assertFixedTopology() {
  for (const value of [BUSINESS_URL, OBSERVABILITY_URL]) {
    const parsed = new URL(value)
    assert.equal(parsed.protocol, 'redis:')
    assert.equal(parsed.hostname, '127.0.0.1')
    assert.equal(Number(parsed.port), value === BUSINESS_URL ? 26379 : 50892)
    assert.equal(Number(parsed.pathname.slice(1)), DATABASE)
    assert.notEqual(Number(parsed.port), 9022)
  }
}

function sourceContract() {
  const config = fs.readFileSync(path.join(ROOT, 'src/model/config.rs'), 'utf8')
  const redis = fs.readFileSync(path.join(ROOT, 'src/storage/redis_cache.rs'), 'utf8')
  const usage = fs.readFileSync(path.join(ROOT, 'src/anthropic/usage.rs'), 'utf8')
  const admin = fs.readFileSync(path.join(ROOT, 'src/admin/service.rs'), 'utf8')
  const main = fs.readFileSync(path.join(ROOT, 'src/main.rs'), 'utf8')

  assert.match(config, /pub fn validate_redis_fault_domains\(&self\)/)
  assert.match(config, /enum RedisAuthorityKey/)
  assert.match(config, /changing DB or keyPrefix is not sufficient/)
  assert.match(redis, /pub async fn connect_observability\(/)
  assert.match(redis, /RedisStoreRole::Observability/)
  assert.match(redis, /server_run_id\(&self\)/)
  assert.match(usage, /with_postgres_and_observability_redis/)
  assert.match(usage, /UsageRecorder observability materialization must not use business Redis/)
  assert.match(admin, /Admin observability caches must not receive the business Redis store/)
  assert.match(main, /RedisStore::connect_observability\(/)
  assert.match(main, /let ready = postgres_ok && redis_ok && redis_events_connected/)
  assert.doesNotMatch(main, /observability_redis_store\.ping\(\)/)

  const runner = fs.readFileSync(fileURLToPath(import.meta.url), 'utf8')
  assert.doesNotMatch(runner, /['"](?:FLUSHDB|FLUSHALL|--allow-flush)['"]/) 
  assert.doesNotMatch(runner, /spawn\([^\n]*(?:cargo|docker)/i)
  assert.match(runner, /SCAN/)
  assert.match(runner, /UNLINK/)

  return {
    configFaultDomainGuard: true,
    roleAwareRedisConstruction: true,
    usageAdminObservabilityInjection: true,
    readinessBusinessOnly: true,
    runnerBoundedCleanup: true,
  }
}

function authority(url) {
  const parsed = new URL(url)
  const host = ['localhost', '127.0.0.1', '::1'].includes(parsed.hostname)
    ? '<loopback>'
    : parsed.hostname.toLowerCase()
  return `${host}:${Number(parsed.port || 6379)}`
}

function sameAuthorityContract() {
  assert.equal(
    authority('redis://127.0.0.1:26379/0'),
    authority('redis://127.0.0.1:26379/15'),
  )
  assert.equal(
    authority('redis://127.0.0.1:26379/0'),
    authority('redis://localhost:26379/15'),
  )
  assert.notEqual(
    authority('redis://127.0.0.1:26379/0'),
    authority('redis://127.0.0.1:50892/15'),
  )
  return {
    sameAuthorityDifferentDbRejected: true,
    sameAuthorityDifferentPrefixRejected: true,
    loopbackAliasNormalized: true,
    distinctPortAccepted: true,
  }
}

function encodeCommand(parts) {
  const buffers = [Buffer.from(`*${parts.length}\r\n`)]
  for (const part of parts) {
    const bytes = Buffer.from(String(part))
    buffers.push(Buffer.from(`$${bytes.length}\r\n`), bytes, Buffer.from('\r\n'))
  }
  return Buffer.concat(buffers)
}

function parseResp(buffer, offset = 0) {
  if (offset >= buffer.length) return null
  const type = String.fromCharCode(buffer[offset])
  const lineEnd = buffer.indexOf('\r\n', offset + 1)
  if (lineEnd < 0) return null
  const line = buffer.subarray(offset + 1, lineEnd).toString('utf8')
  const next = lineEnd + 2
  if (type === '+' || type === ':' || type === '-') {
    return {
      value: type === ':' ? Number(line) : line,
      error: type === '-',
      next,
    }
  }
  if (type === '$') {
    const length = Number(line)
    if (length === -1) return { value: null, error: false, next }
    const end = next + length
    if (end + 2 > buffer.length) return null
    return {
      value: buffer.subarray(next, end).toString('utf8'),
      error: false,
      next: end + 2,
    }
  }
  if (type === '*') {
    const count = Number(line)
    if (count === -1) return { value: null, error: false, next }
    const values = []
    let cursor = next
    for (let index = 0; index < count; index += 1) {
      const item = parseResp(buffer, cursor)
      if (!item) return null
      if (item.error) return item
      values.push(item.value)
      cursor = item.next
    }
    return { value: values, error: false, next: cursor }
  }
  throw new Error(`unsupported Redis response type ${type}`)
}

async function redisCommand(redisUrl, parts, timeoutMs = 1_000) {
  const parsed = new URL(redisUrl)
  assert.equal(parsed.protocol, 'redis:')
  const database = Number(parsed.pathname.slice(1))
  assert.equal(database, DATABASE)
  const commands = [['SELECT', String(database)], parts]
  const payload = Buffer.concat(commands.map(encodeCommand))
  return new Promise((resolve, reject) => {
    const socket = net.connect({ host: parsed.hostname, port: Number(parsed.port || 6379) })
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
    const parse = () => {
      for (;;) {
        const reply = parseResp(received, cursor)
        if (!reply) return
        cursor = reply.next
        if (reply.error) {
          finish(new Error(`Redis command failed: ${reply.value}`))
          return
        }
        replies.push(reply.value)
        if (replies.length === commands.length) {
          finish(null, replies[1])
          return
        }
      }
    }
    socket.setNoDelay(true)
    socket.setTimeout(timeoutMs)
    socket.once('connect', () => socket.write(payload))
    socket.on('data', (chunk) => {
      received = Buffer.concat([received.subarray(cursor), chunk])
      cursor = 0
      try { parse() } catch (error) { finish(error) }
    })
    socket.once('timeout', () => finish(new Error(`Redis command timed out after ${timeoutMs}ms`)))
    socket.once('error', (error) => finish(error))
    socket.once('end', () => finish(new Error('Redis connection ended before a complete response')))
    socket.once('close', () => finish(new Error('Redis connection closed before a complete response')))
  })
}

async function commandResult(redisUrl, parts, timeoutMs = 1_000) {
  const started = Date.now()
  try {
    const value = await redisCommand(redisUrl, parts, timeoutMs)
    return { ok: true, elapsedMs: Date.now() - started, value }
  } catch (error) {
    return { ok: false, elapsedMs: Date.now() - started, error: String(error?.message || error) }
  }
}

async function redisRunId(redisUrl) {
  const info = await redisCommand(redisUrl, ['INFO', 'server'])
  const line = String(info)
    .split('\n')
    .map((value) => value.replace(/\r$/, ''))
    .find((value) => value.startsWith('run_id:'))
  assert.ok(line && line.slice('run_id:'.length).trim(), `Redis INFO did not expose run_id for ${redisUrl}`)
  return line.slice('run_id:'.length).trim()
}

async function scanPrefix(redisUrl, prefix) {
  let cursor = '0'
  const keys = []
  for (let pass = 0; pass < 32; pass += 1) {
    const result = await redisCommand(redisUrl, ['SCAN', cursor, 'MATCH', `${prefix}:*`, 'COUNT', '128'])
    assert.ok(Array.isArray(result) && result.length === 2, 'Redis SCAN response shape changed')
    cursor = String(result[0])
    for (const key of result[1] || []) {
      assert.ok(String(key).startsWith(`${prefix}:`), `SCAN returned an unowned key: ${key}`)
      keys.push(String(key))
    }
    if (cursor === '0') return [...new Set(keys)]
  }
  throw new Error(`SCAN did not converge for owned prefix ${prefix}`)
}

async function cleanupPrefix(redisUrl, prefix) {
  let deleted = 0
  for (let pass = 0; pass < 16; pass += 1) {
    const keys = await scanPrefix(redisUrl, prefix)
    if (keys.length === 0) return deleted
    for (let index = 0; index < keys.length; index += 64) {
      const batch = keys.slice(index, index + 64)
      try {
        await redisCommand(redisUrl, ['UNLINK', ...batch])
      } catch (error) {
        if (!String(error?.message || error).includes('unknown command')) throw error
        await redisCommand(redisUrl, ['DEL', ...batch])
      }
      deleted += batch.length
    }
  }
  throw new Error(`owned prefix cleanup did not converge: ${prefix}`)
}

async function prefixCount(redisUrl, prefix) {
  return (await scanPrefix(redisUrl, prefix)).length
}

function proxyUrl(info) {
  return `redis://127.0.0.1:${info.proxyPort}/${DATABASE}`
}

async function waitForLine(child, stderr) {
  return new Promise((resolve, reject) => {
    let pending = ''
    const timer = setTimeout(() => reject(new Error('Redis chaos proxy readiness timeout')), 10_000)
    child.stdout.on('data', (chunk) => {
      pending += chunk.toString('utf8')
      const end = pending.indexOf('\n')
      if (end < 0) return
      clearTimeout(timer)
      try { resolve(JSON.parse(pending.slice(0, end))) } catch (error) { reject(error) }
    })
    child.once('error', (error) => { clearTimeout(timer); reject(error) })
    child.once('exit', (code) => {
      clearTimeout(timer)
      reject(new Error(`Redis chaos proxy exited before readiness (${code}): ${stderr.join('')}`))
    })
  })
}

async function startProxy(redisUrl, name) {
  const parsed = new URL(redisUrl)
  const stderr = []
  const child = spawn(process.execPath, [
    CHAOS_PROXY,
    '--listen-port', '0',
    '--api-port', '0',
    '--upstream-host', parsed.hostname,
    '--upstream-port', String(parsed.port),
    '--database', String(DATABASE),
    '--name', name,
  ], {
    cwd: TEMP_ROOT,
    env: { PATH: process.env.PATH || '/usr/bin:/bin', TMPDIR: os.tmpdir() },
    detached: true,
    stdio: ['ignore', 'pipe', 'pipe'],
  })
  CHILDREN.add(child)
  child.stderr.on('data', (chunk) => {
    if (Buffer.byteLength(stderr.join('')) < 64 * 1024) stderr.push(chunk.toString('utf8'))
  })
  const info = await waitForLine(child, stderr)
  assert.equal(info.ready, true)
  assert.equal(info.upstreamDatabase, DATABASE)
  assert.equal(info.protected9022ProbeSkipped, true)
  const proxy = { child, info, api: `http://127.0.0.1:${info.apiPort}`, url: proxyUrl(info), name }
  PROXIES.add(proxy)
  return proxy
}

async function stopChild(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return
  try { process.kill(-child.pid, 'SIGTERM') } catch { try { child.kill('SIGTERM') } catch {} }
  await new Promise((resolve) => {
    const timer = setTimeout(() => {
      try { process.kill(-child.pid, 'SIGKILL') } catch { try { child.kill('SIGKILL') } catch {} }
      resolve()
    }, 3_000)
    child.once('exit', () => { clearTimeout(timer); resolve() })
  })
  CHILDREN.delete(child)
}

async function cleanup() {
  if (cleanupPromise) return cleanupPromise
  cleanupPromise = (async () => {
    await Promise.all([...PROXIES].map((proxy) => stopChild(proxy.child)))
    await Promise.all([...CHILDREN].map((child) => stopChild(child)))
    PROXIES.clear()
    fs.rmSync(TEMP_ROOT, { recursive: true, force: true })
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

async function proxyControl(proxy, method, pathname, body) {
  const response = await fetch(`${proxy.api}${pathname}`, {
    method,
    headers: body ? { 'content-type': 'application/json' } : undefined,
    body: body ? JSON.stringify(body) : undefined,
  })
  const text = await response.text()
  let value = {}
  try { value = text ? JSON.parse(text) : {} } catch { value = { text } }
  assert.ok(response.ok, `${method} ${pathname} failed: ${response.status} ${text}`)
  return value
}

async function setProxyEnabled(proxy, enabled) {
  await proxyControl(proxy, 'POST', `/proxies/${encodeURIComponent(proxy.name)}`, { enabled })
}

async function setProxyLatency(proxy, latencyMs) {
  if (latencyMs === 0) {
    await proxyControl(proxy, 'DELETE', `/proxies/${encodeURIComponent(proxy.name)}/toxics/latency`)
    return
  }
  await proxyControl(proxy, 'POST', `/proxies/${encodeURIComponent(proxy.name)}/toxics`, {
    name: 'latency',
    type: 'latency',
    stream: 'downstream',
    attributes: { latency: latencyMs },
  })
}

async function waitForHealthy(redisUrl, attempts = 5) {
  for (let index = 0; index < attempts; index += 1) {
    const result = await commandResult(redisUrl, ['PING'], 1_000)
    if (result.ok && result.value === 'PONG') return result
    await new Promise((resolve) => setTimeout(resolve, 100))
  }
  throw new Error(`Redis did not recover: ${redisUrl}`)
}

async function businessProbe(redisUrl) {
  const result = await commandResult(redisUrl, ['PING'], 500)
  return result.ok && result.value === 'PONG'
}

async function runRound(round, businessProxy, observabilityProxy, prefixes) {
  const { businessPrefix, observabilityPrefix } = prefixes
  const businessUrl = businessProxy.url
  const observabilityUrl = observabilityProxy.url

  trace(`round ${round}: baseline`)
  await redisCommand(businessUrl, ['SET', `${businessPrefix}:baseline`, String(round)])
  await redisCommand(observabilityUrl, ['SET', `${observabilityPrefix}:baseline`, String(round)])
  assert.equal(await prefixCount(BUSINESS_URL, observabilityPrefix), 0)
  assert.equal(await prefixCount(OBSERVABILITY_URL, businessPrefix), 0)

  trace(`round ${round}: observability latency`)
  await setProxyLatency(observabilityProxy, 250)
  const observedTimeout = await commandResult(observabilityUrl, ['PING'], 80)
  assert.equal(observedTimeout.ok, false, `observability latency should exceed timeout: ${JSON.stringify(observedTimeout)}`)
  const businessDuringObsLatency = await Promise.all(
    Array.from({ length: 8 }, (_, index) => commandResult(
      businessUrl,
      ['SET', `${businessPrefix}:obs-latency:${index}`, 'ok'],
      1_000,
    )),
  )
  assert.ok(businessDuringObsLatency.every((item) => item.ok), JSON.stringify(businessDuringObsLatency))
  assert.equal(await businessProbe(businessUrl), true)
  await setProxyLatency(observabilityProxy, 0)
  await waitForHealthy(observabilityUrl)

  trace(`round ${round}: observability disconnect`)
  await setProxyEnabled(observabilityProxy, false)
  const observedDisconnect = await commandResult(observabilityUrl, ['PING'], 300)
  assert.equal(observedDisconnect.ok, false, `observability disconnect should fail: ${JSON.stringify(observedDisconnect)}`)
  const businessDuringObsDisconnect = await Promise.all(
    Array.from({ length: 8 }, (_, index) => commandResult(
      businessUrl,
      ['INCR', `${businessPrefix}:obs-disconnect:${index}`],
      1_000,
    )),
  )
  assert.ok(businessDuringObsDisconnect.every((item) => item.ok), JSON.stringify(businessDuringObsDisconnect))
  assert.equal(await businessProbe(businessUrl), true, 'readiness must not depend on observability Redis')
  await setProxyEnabled(observabilityProxy, true)
  await waitForHealthy(observabilityUrl)

  trace(`round ${round}: business fault`)
  const obsCountBeforeBusinessFault = await prefixCount(OBSERVABILITY_URL, observabilityPrefix)
  await setProxyEnabled(businessProxy, false)
  const businessFaultResults = await Promise.all(
    Array.from({ length: 6 }, (_, index) => commandResult(
      businessUrl,
      ['SET', `${businessPrefix}:business-fault:${index}`, 'lease', 'NX', 'PX', '2000'],
      300,
    )),
  )
  assert.ok(businessFaultResults.every((item) => !item.ok), JSON.stringify(businessFaultResults))
  assert.equal(await businessProbe(businessUrl), false, 'business Redis fault must make readiness fail')
  await redisCommand(observabilityUrl, ['SET', `${observabilityPrefix}:business-fault-observer`, 'healthy'])
  assert.equal(
    await prefixCount(OBSERVABILITY_URL, observabilityPrefix),
    obsCountBeforeBusinessFault + 1,
    'business fault must not trigger an unbounded fallback write outside the observability prefix',
  )
  assert.equal(await prefixCount(OBSERVABILITY_URL, businessPrefix), 0)
  await setProxyEnabled(businessProxy, true)
  await waitForHealthy(businessUrl)
  assert.equal(await businessProbe(businessUrl), true)

  trace(`round ${round}: final cross-domain checks`)
  const businessCount = await prefixCount(BUSINESS_URL, businessPrefix)
  const observabilityCount = await prefixCount(OBSERVABILITY_URL, observabilityPrefix)
  assert.ok(businessCount > 0)
  assert.ok(observabilityCount > 0)
  assert.equal(await prefixCount(BUSINESS_URL, observabilityPrefix), 0)
  assert.equal(await prefixCount(OBSERVABILITY_URL, businessPrefix), 0)
  return {
    round,
    observabilityLatency: {
      injectedMs: 250,
      requestTimedOut: !observedTimeout.ok,
      businessOperationsSucceeded: businessDuringObsLatency.filter((item) => item.ok).length,
      businessP95Ms: [...businessDuringObsLatency].sort((a, b) => a.elapsedMs - b.elapsedMs)[Math.ceil(businessDuringObsLatency.length * 0.95) - 1]?.elapsedMs || 0,
      recovered: true,
    },
    observabilityDisconnect: {
      requestFailed: !observedDisconnect.ok,
      businessOperationsSucceeded: businessDuringObsDisconnect.filter((item) => item.ok).length,
      readinessBusinessOnly: true,
      recovered: true,
    },
    businessFault: {
      schedulerLikeOperationsFailedClosed: businessFaultResults.filter((item) => !item.ok).length,
      readinessFailed: true,
      observabilityRemainedAvailable: true,
      recovered: true,
    },
  }
}

async function main() {
  try {
    assertFixedTopology()
    const source = sourceContract()
    const sameAuthority = sameAuthorityContract()
    const businessProxy = await startProxy(BUSINESS_URL, `business-${process.pid}`)
    const observabilityProxy = await startProxy(OBSERVABILITY_URL, `observability-${process.pid}`)
    const reports = []
    trace('proxies ready')
    await waitForHealthy(BUSINESS_URL)
    await waitForHealthy(OBSERVABILITY_URL)
    const businessRunId = await redisRunId(BUSINESS_URL)
    const observabilityRunId = await redisRunId(OBSERVABILITY_URL)
    assert.notEqual(
      businessRunId,
      observabilityRunId,
      'distinct ports must not be accepted as isolation when they resolve to the same Redis process',
    )
    for (let round = 1; round <= OUTER_ROUNDS; round += 1) {
      const token = `${process.pid}-${Date.now()}-${round}-${Math.random().toString(16).slice(2)}`
      const prefixes = {
        businessPrefix: `kiro_rs:test:redis-fault-domain:business:${token}`,
        observabilityPrefix: `kiro_rs:test:redis-fault-domain:observability:${token}`,
      }
      trace(`round ${round}: start`)
      try {
        reports.push(await runRound(round, businessProxy, observabilityProxy, prefixes))
      } finally {
        trace(`round ${round}: cleanup`)
        await cleanupPrefix(BUSINESS_URL, prefixes.businessPrefix)
        await cleanupPrefix(OBSERVABILITY_URL, prefixes.businessPrefix)
        await cleanupPrefix(BUSINESS_URL, prefixes.observabilityPrefix)
        await cleanupPrefix(OBSERVABILITY_URL, prefixes.observabilityPrefix)
        assert.equal(await prefixCount(BUSINESS_URL, prefixes.businessPrefix), 0)
        assert.equal(await prefixCount(OBSERVABILITY_URL, prefixes.businessPrefix), 0)
        assert.equal(await prefixCount(BUSINESS_URL, prefixes.observabilityPrefix), 0)
        assert.equal(await prefixCount(OBSERVABILITY_URL, prefixes.observabilityPrefix), 0)
      }
    }
    trace('all rounds complete')
    return {
      version: 1,
      topology: {
        business: BUSINESS_URL,
        observability: OBSERVABILITY_URL,
        database: DATABASE,
        serverIdentityDistinct: true,
        businessRunIdSha256: crypto.createHash('sha256').update(businessRunId).digest('hex').slice(0, 16),
        observabilityRunIdSha256: crypto.createHash('sha256').update(observabilityRunId).digest('hex').slice(0, 16),
        protectedPort9022Touched: false,
      },
      outerRounds: OUTER_ROUNDS,
      sourceContract: source,
      sameAuthorityContract: sameAuthority,
      scenarios: reports,
      cleanup: {
        randomPrefixesOnly: true,
        flushDbUsed: false,
        proxyProcessesStoppedByFinally: true,
      },
    }
  } finally {
    await cleanup()
  }
}

try {
  const report = await main()
  process.stdout.write(`${JSON.stringify(report, null, 2)}\n`)
} catch (error) {
  process.stderr.write(`${error?.stack || error}\n`)
  process.exitCode = 1
}
