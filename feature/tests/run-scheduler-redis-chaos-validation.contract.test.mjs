import assert from 'node:assert/strict'
import fs from 'node:fs'
import net from 'node:net'
import os from 'node:os'
import path from 'node:path'
import { spawn, spawnSync } from 'node:child_process'
import test from 'node:test'

const ROOT = fs.realpathSync(path.resolve(import.meta.dirname, '../..'))
const RUNNER = path.join(ROOT, 'feature/tests/run-scheduler-redis-chaos-validation.mjs')
const STATIC_DIRECT_REDIS_URL = 'redis://127.0.0.1:1/15'
const LIVE_EMPTY_REDIS_URL = String(
  process.env.KIRO_SCHEDULER_CHAOS_CONTRACT_EMPTY_REDIS_URL || '',
).trim()
const LIVE_NONEMPTY_REDIS_URL = String(
  process.env.KIRO_SCHEDULER_CHAOS_CONTRACT_NONEMPTY_REDIS_URL || '',
).trim()
const EARLY_ROUNDS = 3
const SIGNAL_ROUNDS = 3

function databaseNumber(redisUrl) {
  if (!redisUrl) return null
  const parsed = new URL(redisUrl)
  const database = Number(parsed.pathname.replace(/^\//, ''))
  if (parsed.protocol !== 'redis:'
      || !['127.0.0.1', 'localhost', '::1'].includes(parsed.hostname)
      || !Number.isSafeInteger(database)
      || database < 1
      || database > 15) {
    throw new Error('live scheduler chaos contract URLs must target loopback Redis DB1..15')
  }
  if (Number(parsed.port || 6379) === 9022) {
    throw new Error('live scheduler chaos contract URLs must not use protected port 9022')
  }
  return database
}

const EMPTY_DATABASE = databaseNumber(LIVE_EMPTY_REDIS_URL)
const NONEMPTY_DATABASE = databaseNumber(LIVE_NONEMPTY_REDIS_URL)

function runnerEnvironment(overrides = {}) {
  const env = { ...process.env }
  for (const name of [
    'KIRO_SCHEDULER_CHAOS_REDIS_DIRECT_URL',
    'KIRO_RS_TEST_REDIS_ISOLATED',
    'KIRO_SCHEDULER_CHAOS_SCOPE',
    'KIRO_SCHEDULER_CHAOS_OUTER_ROUNDS',
    'KIRO_SCHEDULER_CHAOS_TEST_READY_FILE',
  ]) delete env[name]
  Object.assign(env, {
    PATH: process.env.PATH || '/usr/bin:/bin',
    TMPDIR: process.env.TMPDIR || os.tmpdir(),
    KIRO_RS_TEST_REDIS_ISOLATED: '1',
    KIRO_SCHEDULER_CHAOS_REDIS_DIRECT_URL: STATIC_DIRECT_REDIS_URL,
  }, overrides)
  return env
}

function runRunner(overrides = {}, options = {}) {
  const fixtureRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'kiro-chaos-contract-sync-'))
  try {
    const result = spawnSync(process.execPath, [RUNNER], {
      cwd: ROOT,
      env: runnerEnvironment({ TMPDIR: fixtureRoot, ...overrides }),
      encoding: 'utf8',
      timeout: options.timeout ?? 15_000,
      maxBuffer: 2 * 1024 * 1024,
    })
    assert.deepEqual(fs.readdirSync(fixtureRoot), [], 'runner left owned temporary files')
    return result
  } finally {
    fs.rmSync(fixtureRoot, { recursive: true, force: true })
  }
}

function parseResp(buffer, offset = 0) {
  if (offset >= buffer.length) return null
  const type = String.fromCharCode(buffer[offset])
  const lineEnd = buffer.indexOf('\r\n', offset + 1)
  if (lineEnd < 0) return null
  const line = buffer.subarray(offset + 1, lineEnd).toString('utf8')
  const next = lineEnd + 2
  if (type === '+' || type === '-' || type === ':') {
    return { value: type === ':' ? Number(line) : line, next }
  }
  if (type === '$') {
    const length = Number(line)
    if (length === -1) return { value: null, next }
    const end = next + length
    if (end + 2 > buffer.length) return null
    return { value: buffer.subarray(next, end).toString('utf8'), next: end + 2 }
  }
  if (type === '*') {
    const count = Number(line)
    const values = []
    let cursor = next
    for (let index = 0; index < count; index += 1) {
      const item = parseResp(buffer, cursor)
      if (!item) return null
      values.push(item.value)
      cursor = item.next
    }
    return { value: values, next: cursor }
  }
  throw new Error(`unsupported Redis response type ${type}`)
}

function redisDbSize(redisUrl) {
  const url = new URL(redisUrl)
  const database = Number(url.pathname.replace(/^\//, ''))
  const commands = []
  if (url.password) {
    const password = decodeURIComponent(url.password)
    if (url.username) commands.push(['AUTH', decodeURIComponent(url.username), password])
    else commands.push(['AUTH', password])
  }
  commands.push(['SELECT', String(database)], ['DBSIZE'])
  const payload = Buffer.concat(commands.map((parts) => {
    const encoded = [Buffer.from(`*${parts.length}\r\n`)]
    for (const part of parts) {
      const bytes = Buffer.from(part)
      encoded.push(Buffer.from(`$${bytes.length}\r\n`), bytes, Buffer.from('\r\n'))
    }
    return Buffer.concat(encoded)
  }))
  return new Promise((resolve, reject) => {
    const socket = net.connect({
      host: url.hostname,
      port: Number(url.port || 6379),
    })
    let received = Buffer.alloc(0)
    const replies = []
    let cursor = 0
    const parse = () => {
      for (;;) {
        const reply = parseResp(received, cursor)
        if (!reply) return
        replies.push(reply.value)
        cursor = reply.next
        if (replies.length === commands.length) {
          socket.end()
          const result = replies.at(-1)
          if (!Number.isSafeInteger(result)) {
            reject(new Error(`Redis DBSIZE returned ${JSON.stringify(result)}`))
          } else {
            resolve(result)
          }
          return
        }
      }
    }
    socket.setTimeout(5_000)
    socket.once('connect', () => socket.write(payload))
    socket.on('data', (chunk) => {
      received = Buffer.concat([received.subarray(cursor), chunk])
      cursor = 0
      try { parse() } catch (error) { socket.destroy(); reject(error) }
    })
    socket.once('timeout', () => {
      socket.destroy()
      reject(new Error('Redis DBSIZE timed out'))
    })
    socket.once('error', reject)
  })
}

function listeningPids(port) {
  const result = spawnSync('lsof', ['-nP', `-iTCP:${port}`, '-sTCP:LISTEN', '-t'], {
    cwd: ROOT,
    encoding: 'utf8',
    maxBuffer: 1024 * 1024,
  })
  if (result.status !== 0 && result.status !== 1) {
    throw new Error(`lsof failed for owned port ${port}: ${result.stderr}`)
  }
  return String(result.stdout || '').split(/\s+/).filter(Boolean).map(Number)
}

async function waitFor(predicate, timeoutMs = 10_000) {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    const value = predicate()
    if (value) return value
    await new Promise((resolve) => setTimeout(resolve, 25))
  }
  throw new Error('condition did not become true before timeout')
}

function waitForExit(child, timeoutMs = 15_000) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`runner ${child.pid} did not exit`)), timeoutMs)
    child.once('error', (error) => {
      clearTimeout(timer)
      reject(error)
    })
    child.once('exit', (code, signal) => {
      clearTimeout(timer)
      resolve({ code, signal })
    })
  })
}

test('scheduler chaos runner keeps the protected port numeric-only and never probes it', () => {
  const source = fs.readFileSync(RUNNER, 'utf8')
  assert.match(source, /port === 9022/)
  assert.doesNotMatch(source, /listeningPids\(9022\)/)
  assert.doesNotMatch(source, /-iTCP:9022/)
})

const earlyCases = [
  {
    name: 'missing direct Redis URL',
    env: { KIRO_SCHEDULER_CHAOS_REDIS_DIRECT_URL: undefined },
    error: /KIRO_SCHEDULER_CHAOS_REDIS_DIRECT_URL is required/,
  },
  {
    name: 'isolation marker is zero',
    env: { KIRO_RS_TEST_REDIS_ISOLATED: '0' },
    error: /KIRO_RS_TEST_REDIS_ISOLATED=1 is required/,
  },
  {
    name: 'isolation marker is textual true rather than one',
    env: { KIRO_RS_TEST_REDIS_ISOLATED: 'true' },
    error: /KIRO_RS_TEST_REDIS_ISOLATED=1 is required/,
  },
  {
    name: 'database zero',
    env: {
      KIRO_SCHEDULER_CHAOS_REDIS_DIRECT_URL: 'redis://127.0.0.1:1/0',
    },
    error: /isolated nonzero database in 1\.\.15/,
  },
  {
    name: 'protected port 9022',
    env: {
      KIRO_SCHEDULER_CHAOS_REDIS_DIRECT_URL: 'redis://127.0.0.1:9022/15',
    },
    error: /port 9022 is protected/,
  },
]

for (const fixture of earlyCases) {
  for (let round = 1; round <= EARLY_ROUNDS; round += 1) {
    test(`rejects ${fixture.name} before proxy or Cargo, round ${round}`, () => {
      const result = runRunner(fixture.env, { timeout: 5_000 })
      assert.notEqual(result.status, 0, result.stdout)
      assert.match(`${result.stdout}\n${result.stderr}`, fixture.error)
      assert.doesNotMatch(result.stderr, /validation-build-admission|cargo test/)
    })
  }
}

for (let round = 1; round <= EARLY_ROUNDS; round += 1) {
  test(`rejects a non-empty isolated database without changing its key count, round ${round}`, {
    skip: !LIVE_NONEMPTY_REDIS_URL,
  }, async () => {
    const directUrl = LIVE_NONEMPTY_REDIS_URL
    const before = await redisDbSize(directUrl)
    assert.ok(before > 0, `database ${NONEMPTY_DATABASE} must be pre-populated for this contract: ${before}`)
    const result = runRunner({
      KIRO_SCHEDULER_CHAOS_REDIS_DIRECT_URL: directUrl,
      KIRO_SCHEDULER_CHAOS_SCOPE: `contract-nonempty-${process.pid}-${round}`,
      KIRO_SCHEDULER_CHAOS_OUTER_ROUNDS: '1',
    }, { timeout: 10_000 })
    const after = await redisDbSize(directUrl)
    assert.notEqual(result.status, 0, result.stdout)
    assert.match(`${result.stdout}\n${result.stderr}`, /is not empty \(/)
    assert.equal(after, before, 'non-empty database key count changed')
    assert.doesNotMatch(result.stderr, /validation-build-admission|cargo test/)
  })
}

for (const [signal, expectedCode] of [['SIGHUP', 129], ['SIGINT', 130], ['SIGTERM', 143]]) {
  for (let round = 1; round <= SIGNAL_ROUNDS; round += 1) {
    test(`${signal} cleans the owned proxy, ports, temporary root, and ready file, round ${round}`, {
      skip: !LIVE_EMPTY_REDIS_URL,
    }, async () => {
      const fixtureRoot = fs.mkdtempSync(path.join(os.tmpdir(), `kiro-chaos-contract-${signal.toLowerCase()}-`))
      const readyFile = path.join(fixtureRoot, 'ready.json')
      const child = spawn(process.execPath, [RUNNER], {
        cwd: ROOT,
        env: runnerEnvironment({
          TMPDIR: fixtureRoot,
          KIRO_SCHEDULER_CHAOS_REDIS_DIRECT_URL: LIVE_EMPTY_REDIS_URL,
          KIRO_SCHEDULER_CHAOS_SCOPE: `contract-signal-${signal.toLowerCase()}-${process.pid}-${round}`,
          KIRO_SCHEDULER_CHAOS_TEST_READY_FILE: readyFile,
          KIRO_SCHEDULER_CHAOS_OUTER_ROUNDS: '1',
        }),
        stdio: ['ignore', 'pipe', 'pipe'],
      })
      const stdout = []
      const stderr = []
      child.stdout.on('data', (chunk) => stdout.push(chunk))
      child.stderr.on('data', (chunk) => stderr.push(chunk))
      let payload
      try {
        payload = await waitFor(() => {
          if (!fs.existsSync(readyFile)) return null
          return JSON.parse(fs.readFileSync(readyFile, 'utf8'))
        })
        assert.equal(payload.database, EMPTY_DATABASE)
        assert.notEqual(payload.proxyPort, 9022)
        assert.notEqual(payload.apiPort, 9022)
        assert.ok(Number.isInteger(payload.proxyPid) && payload.proxyPid > 1)
        assert.ok(fs.existsSync(payload.tempRoot), 'runner temp root was not created')
        assert.deepEqual(listeningPids(payload.proxyPort), [payload.proxyPid])
        assert.deepEqual(listeningPids(payload.apiPort), [payload.proxyPid])

        child.kill(signal)
        const exit = await waitForExit(child)
        assert.deepEqual(exit, { code: expectedCode, signal: null },
          `${Buffer.concat(stderr).toString('utf8')}\n${Buffer.concat(stdout).toString('utf8')}`)
        await waitFor(() => (
          !fs.existsSync(payload.tempRoot)
          && !fs.existsSync(readyFile)
          && listeningPids(payload.proxyPort).length === 0
          && listeningPids(payload.apiPort).length === 0
        ))
        assert.deepEqual(fs.readdirSync(fixtureRoot), [])
        assert.equal(await redisDbSize(LIVE_EMPTY_REDIS_URL), 0)
        assert.doesNotMatch(Buffer.concat(stderr).toString('utf8'), /validation-build-admission|cargo test/)
      } finally {
        if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL')
        fs.rmSync(fixtureRoot, { recursive: true, force: true })
      }
    })
  }
}
