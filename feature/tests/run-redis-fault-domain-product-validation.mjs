#!/usr/bin/env node

import assert from 'node:assert/strict'
import fs from 'node:fs'
import net from 'node:net'
import os from 'node:os'
import path from 'node:path'
import { spawn } from 'node:child_process'

import { validationChildEnvironment } from './validation-child-env.mjs'

const ROOT = fs.realpathSync(path.resolve(import.meta.dirname, '../..'))
const CHAOS_PROXY = path.join(ROOT, 'feature/tests/redis-chaos-proxy.mjs')
const BUSINESS_URL = requiredEnvironment('KIRO_REDIS_FAULT_DOMAIN_BUSINESS_URL')
const OBSERVABILITY_URL = requiredEnvironment('KIRO_REDIS_FAULT_DOMAIN_OBSERVABILITY_URL')
const ISOLATED = process.env.KIRO_RS_TEST_REDIS_ISOLATED === '1'
const OUTER_ROUNDS = boundedInteger('KIRO_REDIS_FAULT_DOMAIN_OUTER_ROUNDS', 3, 1, 5)
const SCOPE = String(process.env.KIRO_REDIS_FAULT_DOMAIN_SCOPE || 'redis-fault-domain-product')
const TEMP_ROOT = fs.mkdtempSync(path.join(os.tmpdir(), `kiro-redis-fault-domain-product-${process.pid}-`))

const ACTIVE_CHILDREN = new Set()
const PROXIES = new Set()
let cleanupPromise = null
let signalHandling = false
let testReadyFile = null

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

function normalizeHost(hostname) {
  const host = hostname.toLowerCase()
  if (host === '127.0.0.1' || host === 'localhost' || host === '::1' || host === '[::1]') {
    return '<loopback>'
  }
  return host
}

function parseRedisTarget(name, raw) {
  let parsed
  try {
    parsed = new URL(raw)
  } catch {
    throw new Error(`${name} must be a valid redis:// URL`)
  }
  if (parsed.protocol !== 'redis:') {
    throw new Error(`${name} must use redis:// because the local chaos proxy is plaintext TCP`)
  }
  if (!['127.0.0.1', 'localhost', '::1', '[::1]'].includes(parsed.hostname)) {
    throw new Error(`${name} must target loopback Redis`)
  }
  const port = Number(parsed.port || 6379)
  if (!Number.isSafeInteger(port) || port < 1 || port > 65535) {
    throw new Error(`${name} has an invalid port`)
  }
  if (port === 9022) throw new Error(`${name} cannot use protected port 9022`)
  const databaseText = parsed.pathname.replace(/^\//, '')
  if (!/^\d+$/.test(databaseText)) {
    throw new Error(`${name} must name a Redis database`)
  }
  const database = Number(databaseText)
  if (!Number.isSafeInteger(database) || database < 1 || database > 15) {
    throw new Error(`${name} must use an isolated nonzero Redis database in 1..15`)
  }
  return {
    name,
    raw,
    parsed,
    hostname: parsed.hostname,
    port,
    database,
    authority: `${normalizeHost(parsed.hostname)}:${port}`,
  }
}

function validateInputs() {
  if (!ISOLATED) {
    throw new Error('KIRO_RS_TEST_REDIS_ISOLATED=1 is required')
  }
  if (!/^[a-z0-9][a-z0-9._-]{0,63}$/.test(SCOPE)) {
    throw new Error('KIRO_REDIS_FAULT_DOMAIN_SCOPE has an invalid format')
  }
  const business = parseRedisTarget('KIRO_REDIS_FAULT_DOMAIN_BUSINESS_URL', BUSINESS_URL)
  const observability = parseRedisTarget(
    'KIRO_REDIS_FAULT_DOMAIN_OBSERVABILITY_URL',
    OBSERVABILITY_URL,
  )
  if (business.authority === observability.authority) {
    throw new Error('business and observability Redis URLs must use distinct network authorities')
  }
  return { business, observability }
}

function optionalTestReadyFile() {
  const raw = String(process.env.KIRO_REDIS_FAULT_DOMAIN_TEST_READY_FILE || '').trim()
  if (!raw) return null
  if (!path.isAbsolute(raw)) {
    throw new Error('KIRO_REDIS_FAULT_DOMAIN_TEST_READY_FILE must be an absolute path')
  }
  const parent = path.dirname(raw)
  if (!fs.existsSync(parent)) {
    throw new Error('KIRO_REDIS_FAULT_DOMAIN_TEST_READY_FILE parent must exist')
  }
  const parentReal = fs.realpathSync(parent)
  if (parentReal === ROOT || parentReal.startsWith(`${ROOT}${path.sep}`)) {
    throw new Error('KIRO_REDIS_FAULT_DOMAIN_TEST_READY_FILE must be outside the repository')
  }
  if (fs.existsSync(raw)) {
    throw new Error('KIRO_REDIS_FAULT_DOMAIN_TEST_READY_FILE must not already exist')
  }
  return raw
}

function listeningPids(port) {
  const child = spawn('lsof', ['-nP', `-iTCP:${port}`, '-sTCP:LISTEN', '-t'], {
    cwd: ROOT,
    env: { PATH: process.env.PATH || '/usr/bin:/bin' },
    stdio: ['ignore', 'pipe', 'pipe'],
  })
  return new Promise((resolve, reject) => {
    let stdout = ''
    let stderr = ''
    child.stdout.on('data', (chunk) => { stdout += chunk.toString('utf8') })
    child.stderr.on('data', (chunk) => { stderr += chunk.toString('utf8') })
    child.once('error', reject)
    child.once('exit', (code) => {
      if (code !== 0 && code !== 1) {
        reject(new Error(`lsof failed for port ${port}: ${stderr}`))
        return
      }
      resolve(stdout.split(/\s+/).filter(Boolean).map(Number))
    })
  })
}

async function tcpProbe(target) {
  await new Promise((resolve, reject) => {
    const socket = net.createConnection({ host: target.hostname, port: target.port })
    const timer = setTimeout(() => {
      socket.destroy()
      reject(new Error(`${target.name} TCP prerequisite is unreachable`))
    }, 2_000)
    socket.once('connect', () => {
      clearTimeout(timer)
      socket.destroy()
      resolve()
    })
    socket.once('error', () => {
      clearTimeout(timer)
      reject(new Error(`${target.name} TCP prerequisite is unreachable`))
    })
  })
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
      try {
        resolve(JSON.parse(pending.slice(0, end)))
      } catch (error) {
        reject(error)
      }
    })
    child.once('error', (error) => {
      clearTimeout(timer)
      reject(error)
    })
    child.once('exit', (code) => {
      clearTimeout(timer)
      reject(new Error(`Redis chaos proxy exited before readiness (${code}): ${stderr.join('')}`))
    })
  })
}

async function startProxy(target, name) {
  const stderr = []
  const child = spawn(process.execPath, [
    CHAOS_PROXY,
    '--listen-port', '0',
    '--api-port', '0',
    '--upstream-host', target.hostname,
    '--upstream-port', String(target.port),
    '--database', String(target.database),
    '--name', name,
  ], {
    cwd: TEMP_ROOT,
    env: { PATH: process.env.PATH || '/usr/bin:/bin', TMPDIR: os.tmpdir() },
    detached: true,
    stdio: ['ignore', 'pipe', 'pipe'],
  })
  ACTIVE_CHILDREN.add(child)
  child.once('exit', () => ACTIVE_CHILDREN.delete(child))
  child.stderr.on('data', (chunk) => {
    if (Buffer.byteLength(stderr.join('')) < 64 * 1024) stderr.push(chunk.toString('utf8'))
  })
  const info = await waitForLine(child, stderr)
  assert.equal(info.ready, true)
  assert.equal(info.upstreamDatabase, target.database)
  assert.equal(info.protected9022ProbeSkipped, true)
  const listenPids = await listeningPids(info.proxyPort)
  const apiPids = await listeningPids(info.apiPort)
  assert.deepEqual(listenPids, [child.pid])
  assert.deepEqual(apiPids, [child.pid])
  const proxyUrl = new URL(target.raw)
  proxyUrl.hostname = '127.0.0.1'
  proxyUrl.port = String(info.proxyPort)
  const proxy = {
    child,
    name,
    api: `http://127.0.0.1:${info.apiPort}`,
    proxyPort: info.proxyPort,
    apiPort: info.apiPort,
    redisUrl: proxyUrl.toString(),
  }
  PROXIES.add(proxy)
  return proxy
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
  try {
    process.kill(-child.pid, 'SIGTERM')
  } catch {
    try { child.kill('SIGTERM') } catch {}
  }
  const stopped = await waitForExit(5_000)
  if (!stopped && child.exitCode === null && child.signalCode === null) {
    try {
      process.kill(-child.pid, 'SIGKILL')
    } catch {
      try { child.kill('SIGKILL') } catch {}
    }
    const killed = await waitForExit(5_000)
    if (!killed) throw new Error(`owned child process group ${child.pid} did not exit`)
  }
  ACTIVE_CHILDREN.delete(child)
}

async function fetchJson(url, options = {}) {
  const response = await fetch(url, options)
  const text = await response.text()
  let body = {}
  try {
    body = text ? JSON.parse(text) : {}
  } catch {
    body = { text }
  }
  if (!response.ok) throw new Error(`${options.method || 'GET'} ${url} -> ${response.status}: ${text}`)
  return body
}

async function resetProxy(proxy) {
  await fetchJson(`${proxy.api}/proxies/${encodeURIComponent(proxy.name)}`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ enabled: true }),
  })
  await fetchJson(`${proxy.api}/proxies/${encodeURIComponent(proxy.name)}/toxics/fault-domain-response-latency`, {
    method: 'DELETE',
  }).catch(() => {})
}

async function cleanup() {
  if (cleanupPromise) return cleanupPromise
  cleanupPromise = (async () => {
    const nonProxyChildren = [...ACTIVE_CHILDREN].filter(
      (child) => ![...PROXIES].some((proxy) => proxy.child === child),
    )
    await Promise.all(nonProxyChildren.map((child) => stopChild(child)))
    await Promise.all([...PROXIES].map((proxy) => resetProxy(proxy).catch(() => {})))
    await Promise.all([...ACTIVE_CHILDREN].map((child) => stopChild(child)))
    const portsReleased = await Promise.all([...PROXIES].flatMap((proxy) => [
      listeningPids(proxy.proxyPort),
      listeningPids(proxy.apiPort),
    ]))
    PROXIES.clear()
    fs.rmSync(TEMP_ROOT, { recursive: true, force: true })
    if (testReadyFile) fs.rmSync(testReadyFile, { force: true })
    return {
      childGroupsStopped: ACTIVE_CHILDREN.size === 0,
      portsReleased: portsReleased.every((pids) => pids.length === 0),
      tempRemoved: !fs.existsSync(TEMP_ROOT),
    }
  })()
  return cleanupPromise
}

async function holdForSignalAfterProxiesReady(businessProxy, observabilityProxy, targets) {
  if (!testReadyFile) return true
  const payload = {
    ready: true,
    pid: process.pid,
    tempRoot: TEMP_ROOT,
    businessProxyPid: businessProxy.child.pid,
    observabilityProxyPid: observabilityProxy.child.pid,
    businessProxyPort: businessProxy.proxyPort,
    observabilityProxyPort: observabilityProxy.proxyPort,
    businessDatabase: targets.business.database,
    observabilityDatabase: targets.observability.database,
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

function scopedCargoScript() {
  return `
set -euo pipefail
cargo fmt --all -- --check
git diff --check
for round in $(seq 1 "$KIRO_REDIS_FAULT_DOMAIN_OUTER_ROUNDS"); do
  echo "redis-fault-domain-product outer_round=$round"
  cargo test kiro::token_manager::manager::tests::redis_business_and_observability_fault_domains_are_independent_for_three_rounds -- --exact --nocapture --test-threads=1
done
`
}

async function runScopedCargo(businessProxy, observabilityProxy) {
  const env = validationChildEnvironment({
    RUSTUP_TOOLCHAIN: '1.92.0',
    KIRO_RS_TEST_BUSINESS_REDIS_URL: businessProxy.redisUrl,
    KIRO_RS_TEST_OBSERVABILITY_REDIS_URL: observabilityProxy.redisUrl,
    KIRO_RS_REQUIRE_STORAGE_TESTS: '1',
    KIRO_REDIS_FAULT_DOMAIN_OUTER_ROUNDS: String(OUTER_ROUNDS),
    KIRO_RS_TEST_BUSINESS_TOXIPROXY_API: businessProxy.api,
    KIRO_RS_TEST_BUSINESS_TOXIPROXY_NAME: businessProxy.name,
    KIRO_RS_TEST_OBSERVABILITY_TOXIPROXY_API: observabilityProxy.api,
    KIRO_RS_TEST_OBSERVABILITY_TOXIPROXY_NAME: observabilityProxy.name,
  })
  const command = spawn(path.join(ROOT, 'feature/tests/run-cargo-scoped.sh'), [
    SCOPE,
    '--',
    'bash',
    '-lc',
    scopedCargoScript(),
  ], {
    cwd: ROOT,
    env,
    detached: true,
    stdio: 'inherit',
  })
  ACTIVE_CHILDREN.add(command)
  command.once('exit', () => ACTIVE_CHILDREN.delete(command))
  return new Promise((resolve) => {
    command.once('exit', (code, signal) => resolve({ code, signal }))
    command.once('error', (error) => resolve({ code: null, signal: null, error: error.message }))
  })
}

for (const [signal, code] of [['SIGHUP', 129], ['SIGINT', 130], ['SIGTERM', 143]]) {
  process.on(signal, () => {
    if (signalHandling) return
    signalHandling = true
    void cleanup().finally(() => { process.exit(code) })
  })
}

async function main() {
  const targets = validateInputs()
  testReadyFile = optionalTestReadyFile()
  await tcpProbe(targets.business)
  await tcpProbe(targets.observability)
  const businessProxy = await startProxy(targets.business, `business-${process.pid}`)
  const observabilityProxy = await startProxy(targets.observability, `observability-${process.pid}`)
  if (!(await holdForSignalAfterProxiesReady(businessProxy, observabilityProxy, targets))) return

  const exit = await runScopedCargo(businessProxy, observabilityProxy)
  assert.equal(exit.code, 0, `scoped Cargo Redis fault-domain test failed: ${JSON.stringify(exit)}`)
  const cleaned = await cleanup()
  assert.deepEqual(cleaned, {
    childGroupsStopped: true,
    portsReleased: true,
    tempRemoved: true,
  })
  process.stdout.write(`${JSON.stringify({
    result: 'pass',
    scope: SCOPE,
    outerRounds: OUTER_ROUNDS,
    exactTests: 1,
    exactInvocations: OUTER_ROUNDS,
    internalRoundsPerInvocation: 3,
    businessAuthority: targets.business.authority,
    observabilityAuthority: targets.observability.authority,
    businessDatabase: targets.business.database,
    observabilityDatabase: targets.observability.database,
    protected9022ProbeSkipped: true,
    flushDbUsed: false,
    dockerUsed: false,
    cargoThroughScopedWrapper: true,
    cleanup: cleaned,
  }, null, 2)}\n`)
}

main().catch(async (error) => {
  const cleaned = await cleanup().catch(() => null)
  if (!signalHandling) {
    process.stderr.write(`Redis fault-domain product validation failed: ${error.message}; cleanup=${JSON.stringify(cleaned)}\n`)
    process.exitCode = 1
  }
})
