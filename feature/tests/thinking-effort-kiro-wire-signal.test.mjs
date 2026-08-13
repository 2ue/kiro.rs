#!/usr/bin/env node

import assert from 'node:assert/strict'
import fs from 'node:fs'
import net from 'node:net'
import os from 'node:os'
import path from 'node:path'
import { spawn, spawnSync } from 'node:child_process'
import test from 'node:test'

const ROOT = path.resolve(import.meta.dirname, '../..')
const RUNNER = path.join(ROOT, 'feature/tests/thinking-effort-kiro-wire.mjs')
const SIGNALS = [
  ['SIGHUP', 129],
  ['SIGINT', 130],
  ['SIGTERM', 143],
]
const ROUNDS = 3

function processStartIdentity(pid) {
  const result = spawnSync('ps', ['-o', 'lstart=', '-p', String(pid)], {
    encoding: 'utf8',
    env: { PATH: process.env.PATH || '/usr/bin:/bin', LANG: 'C', LC_ALL: 'C' },
    timeout: 2_000,
  })
  if (result.status !== 0 || result.error) return null
  return String(result.stdout || '').trim().replace(/\s+/g, ' ') || null
}

function processGroupMembers(pgid) {
  const result = spawnSync('ps', ['-axo', 'pid=,pgid=,lstart='], {
    encoding: 'utf8',
    env: { PATH: process.env.PATH || '/usr/bin:/bin', LANG: 'C', LC_ALL: 'C' },
    timeout: 2_000,
  })
  if (result.status !== 0 || result.error) return []
  return String(result.stdout || '').split(/\r?\n/).flatMap((line) => {
    const match = line.trim().match(/^(\d+)\s+(\d+)\s+(.+)$/)
    if (!match || Number(match[2]) !== pgid) return []
    return [{
      pid: Number(match[1]),
      pgid: Number(match[2]),
      startIdentity: match[3].trim().replace(/\s+/g, ' '),
    }]
  })
}

function originalProcessAlive(identity) {
  return Boolean(identity?.startIdentity)
    && processStartIdentity(identity.pid) === identity.startIdentity
}

function killOwnedGroup(identity) {
  if (!identity || !Number.isInteger(identity.pgid)) return
  const members = processGroupMembers(identity.pgid)
  if (members.length === 0) return
  const leaderIdentity = members.find((member) => member.pid === identity.pgid)?.startIdentity || null
  if (!leaderIdentity || leaderIdentity !== identity.startIdentity) return
  try { process.kill(-identity.pgid, 'SIGKILL') } catch {}
}

async function portAcceptsConnections(port) {
  return await new Promise((resolve) => {
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
}

async function waitForReady(child, timeoutMs = 10_000) {
  return await new Promise((resolve, reject) => {
    let buffer = ''
    let settled = false
    let timer = null
    const finish = (callback, value) => {
      if (settled) return
      settled = true
      if (timer) clearTimeout(timer)
      child.stdout.off('data', onData)
      child.off('error', onError)
      child.off('exit', onExit)
      callback(value)
    }
    const onError = (error) => finish(reject, error)
    const onExit = (code, signal) => finish(
      reject,
      new Error(`lifecycle fixture exited before ready: code=${code} signal=${signal}`),
    )
    const onData = (chunk) => {
      buffer += chunk
      if (Buffer.byteLength(buffer) > 64 * 1024) {
        finish(reject, new Error('thinking wire lifecycle readiness output overflow'))
        return
      }
      const newline = buffer.indexOf('\n')
      if (newline < 0) return
      try {
        finish(resolve, JSON.parse(buffer.slice(0, newline)))
      } catch (error) {
        finish(reject, error)
      }
    }
    child.stdout.setEncoding('utf8')
    child.stdout.on('data', onData)
    child.once('error', onError)
    child.once('exit', onExit)
    timer = setTimeout(
      () => finish(reject, new Error('thinking wire lifecycle fixture readiness timed out')),
      timeoutMs,
    )
    timer.unref()
  })
}

async function waitForExit(child, timeoutMs = 30_000) {
  if (child.exitCode !== null || child.signalCode !== null) {
    return { code: child.exitCode, signal: child.signalCode }
  }
  return await new Promise((resolve, reject) => {
    let timer = null
    const finish = (callback, value) => {
      if (timer) clearTimeout(timer)
      child.off('exit', onExit)
      child.off('error', onError)
      callback(value)
    }
    const onExit = (code, signal) => finish(resolve, { code, signal })
    const onError = (error) => finish(reject, error)
    child.once('exit', onExit)
    child.once('error', onError)
    timer = setTimeout(
      () => finish(reject, new Error('thinking wire lifecycle fixture exit timed out')),
      timeoutMs,
    )
    timer.unref()
  })
}

async function waitForCondition(callback, timeoutMs = 10_000) {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    if (await callback()) return true
    await new Promise((resolve) => setTimeout(resolve, 50))
  }
  return await callback()
}

function readCleanupAck(ackPath) {
  return JSON.parse(fs.readFileSync(ackPath, 'utf8'))
}

async function runLifecycleCase({ mode, signal, expectedCode }) {
  const parentRoot = fs.mkdtempSync(path.join(os.tmpdir(), `thinking-wire-${mode}-`))
  const ackPath = path.join(parentRoot, 'redis-cleanup-ack.json')
  const child = spawn(process.execPath, [RUNNER], {
    cwd: ROOT,
    env: {
      PATH: process.env.PATH || '/usr/bin:/bin',
      TMPDIR: os.tmpdir(),
      LANG: 'C',
      LC_ALL: 'C',
      KIRO_THINKING_WIRE_FIXTURE_MODE: mode,
      KIRO_SIGNAL_ACK_PATH: ackPath,
    },
    detached: true,
    stdio: ['ignore', 'pipe', 'pipe'],
  })
  const runnerIdentity = {
    pid: child.pid,
    pgid: child.pid,
    startIdentity: processStartIdentity(child.pid),
  }
  const stderr = []
  let stderrBytes = 0
  child.stderr.on('data', (chunk) => {
    stderrBytes += chunk.length
    if (stderrBytes <= 1024 * 1024) stderr.push(chunk)
  })
  let ready = null

  try {
    assert.ok(runnerIdentity.startIdentity, 'runner start identity must be captured')
    ready = await waitForReady(child)
    assert.equal(ready.ready, true)
    assert.equal(ready.mode, mode)
    for (const port of [ready.fakePort, ready.ingressPort, ready.redisPort]) {
      assert.notEqual(port, 9022)
    }
    assert.equal(await portAcceptsConnections(ready.fakePort), true)
    assert.equal(await portAcceptsConnections(ready.ingressPort), true)
    assert.equal(await portAcceptsConnections(ready.redisPort), true)
    assert.equal(fs.existsSync(ready.tempRoot), true)
    assert.equal(ready.ownedChildren.length, 1)
    for (const identity of ready.ownedChildren) {
      assert.ok(identity.startIdentity)
      assert.equal(identity.pgid, identity.pid)
      assert.equal(originalProcessAlive(identity), true)
      assert.equal(processGroupMembers(identity.pgid).some((member) => (
        member.pid === identity.pid && member.startIdentity === identity.startIdentity
      )), true)
    }

    const exitStarted = Date.now()
    const exiting = waitForExit(child)
    if (signal) child.kill(signal)
    assert.deepEqual(await exiting, { code: expectedCode, signal: null })
    const exitDurationMs = Date.now() - exitStarted

    assert.equal(await waitForCondition(() => !fs.existsSync(ready.tempRoot)), true)
    assert.equal(await waitForCondition(async () => !await portAcceptsConnections(ready.fakePort)), true)
    assert.equal(await waitForCondition(async () => !await portAcceptsConnections(ready.ingressPort)), true)
    assert.equal(await waitForCondition(async () => !await portAcceptsConnections(ready.redisPort)), true)
    for (const identity of ready.ownedChildren) {
      assert.equal(await waitForCondition(() => !originalProcessAlive(identity)), true)
      assert.deepEqual(processGroupMembers(identity.pgid), [])
    }

    assert.equal(fs.existsSync(ackPath), true)
    const ack = readCleanupAck(ackPath)
    assert.deepEqual([...ack.patterns].sort(), [...ack.expectedPatterns].sort())
    assert.deepEqual(ack.ownedRemaining, [])
    assert.equal(ack.foreignPreserved, true)
    assert.equal(ack.foreignRemoved, true)
    assert.deepEqual(ack.protocolErrors, [])
    assert.deepEqual(ack.fixture.ports, {
      fakePort: ready.fakePort,
      ingressPort: ready.ingressPort,
      redisPort: ready.redisPort,
    })
    assert.equal(ack.fixture.tempRoot, ready.tempRoot)
    assert.deepEqual(ack.fixture.ownedChildren, ready.ownedChildren)
    if (mode === 'signal_race') assert.ok(ack.blockedSpawnAttempts > 0)
    else assert.equal(ack.blockedSpawnAttempts, 0)
    if (mode === 'cleanup_timeout' || mode === 'command_timeout') {
      assert.ok(ack.killEscalations > 0)
    }
    else assert.equal(ack.killEscalations, 0)
    if (mode.startsWith('redis_')) assert.equal(ack.injectedRedisFailures, 1)
    else assert.equal(ack.injectedRedisFailures, 0)
    assert.equal(ack.commandTimeoutRejected, mode === 'command_timeout')
    assert.equal(ack.commandSpawnRejected, mode === 'command_spawn_error')
    assert.equal(
      ack.heldServerSocketsClosed,
      mode === 'server_socket_hang' ? true : null,
    )
    if (!signal) {
      assert.ok(exitDurationMs < 2_500, `natural cleanup took ${exitDurationMs}ms`)
    }
    assert.ok(stderrBytes <= 1024 * 1024)
    assert.equal(Buffer.concat(stderr).toString('utf8'), '')
  } finally {
    if (originalProcessAlive(runnerIdentity)) {
      try { child.kill('SIGTERM') } catch {}
      await waitForCondition(() => !originalProcessAlive(runnerIdentity), 5_000)
    }
    killOwnedGroup(runnerIdentity)
    if (ready) {
      for (const identity of ready.ownedChildren || []) killOwnedGroup(identity)
      await waitForCondition(async () => (
        !await portAcceptsConnections(ready.fakePort)
        && !await portAcceptsConnections(ready.ingressPort)
        && !await portAcceptsConnections(ready.redisPort)
      ), 5_000)
      if (!originalProcessAlive(runnerIdentity) && ready.tempRoot.startsWith(os.tmpdir())) {
        fs.rmSync(ready.tempRoot, { recursive: true, force: true })
      }
    }
    fs.rmSync(parentRoot, { recursive: true, force: true })
  }
}

for (const mode of ['signal_idle', 'signal_race']) {
  for (const [signal, expectedCode] of SIGNALS) {
    for (let round = 1; round <= ROUNDS; round += 1) {
      test(`${mode} ${signal} round ${round} cleans PGIDs, ports, Redis ownership and temp`, async () => {
        await runLifecycleCase({ mode, signal, expectedCode })
      })
    }
  }
}

for (const [mode, expectedCode] of [['cleanup_error', 1], ['cleanup_timeout', 124]]) {
  for (let round = 1; round <= ROUNDS; round += 1) {
    test(`${mode} round ${round} cleans all owned resources`, async () => {
      await runLifecycleCase({ mode, signal: null, expectedCode })
    })
  }
}

for (const mode of ['redis_error', 'redis_timeout']) {
  for (let round = 1; round <= ROUNDS; round += 1) {
    test(`${mode} round ${round} retries cleanup and preserves foreign Redis state`, async () => {
      await runLifecycleCase({ mode, signal: null, expectedCode: 1 })
    })
  }
}

for (let round = 1; round <= ROUNDS; round += 1) {
  test(`command_timeout round ${round} bounds child timeout and naturally drains handles`, async () => {
    await runLifecycleCase({ mode: 'command_timeout', signal: null, expectedCode: 0 })
  })
  test(`command_spawn_error round ${round} rejects spawn and naturally drains handles`, async () => {
    await runLifecycleCase({ mode: 'command_spawn_error', signal: null, expectedCode: 0 })
  })
  test(`server_socket_hang round ${round} destroys held sockets before natural exit`, async () => {
    await runLifecycleCase({ mode: 'server_socket_hang', signal: null, expectedCode: 0 })
  })
}

async function runStartupErrorCase() {
  const parentRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'thinking-wire-startup-error-'))
  const ackPath = path.join(parentRoot, 'redis-cleanup-ack.json')
  const child = spawn(process.execPath, [RUNNER], {
    cwd: ROOT,
    env: {
      PATH: process.env.PATH || '/usr/bin:/bin',
      TMPDIR: os.tmpdir(),
      LANG: 'C',
      LC_ALL: 'C',
      KIRO_THINKING_WIRE_FIXTURE_MODE: 'startup_error',
      KIRO_SIGNAL_ACK_PATH: ackPath,
    },
    detached: true,
    stdio: ['ignore', 'pipe', 'pipe'],
  })
  const runnerIdentity = {
    pid: child.pid,
    pgid: child.pid,
    startIdentity: processStartIdentity(child.pid),
  }
  const stdout = []
  const stderr = []
  child.stdout.on('data', (chunk) => stdout.push(chunk))
  child.stderr.on('data', (chunk) => stderr.push(chunk))
  let ack = null
  try {
    assert.ok(runnerIdentity.startIdentity)
    assert.deepEqual(await waitForExit(child), { code: 1, signal: null })
    assert.equal(Buffer.concat(stdout).toString('utf8'), '')
    assert.equal(
      Buffer.concat(stderr).toString('utf8'),
      'thinking wire signal fixture failed: injected startup failure\n',
    )
    assert.equal(fs.existsSync(ackPath), true)
    ack = readCleanupAck(ackPath)
    assert.deepEqual(ack.ownedRemaining, [])
    assert.equal(ack.foreignPreserved, true)
    assert.equal(ack.foreignRemoved, true)
    assert.deepEqual(ack.protocolErrors, [])
    assert.equal(ack.commandTimeoutRejected, false)
    assert.equal(ack.commandSpawnRejected, false)
    assert.equal(ack.heldServerSocketsClosed, null)
    const ports = Object.values(ack.fixture.ports)
    for (const port of ports) {
      assert.notEqual(port, 9022)
      assert.equal(await waitForCondition(async () => !await portAcceptsConnections(port)), true)
    }
    assert.equal(await waitForCondition(() => !fs.existsSync(ack.fixture.tempRoot)), true)
    for (const identity of ack.fixture.ownedChildren) {
      assert.equal(await waitForCondition(() => !originalProcessAlive(identity)), true)
      assert.deepEqual(processGroupMembers(identity.pgid), [])
    }
  } finally {
    if (originalProcessAlive(runnerIdentity)) {
      try { child.kill('SIGTERM') } catch {}
      await waitForCondition(() => !originalProcessAlive(runnerIdentity), 5_000)
    }
    killOwnedGroup(runnerIdentity)
    for (const identity of ack?.fixture?.ownedChildren || []) killOwnedGroup(identity)
    fs.rmSync(parentRoot, { recursive: true, force: true })
  }
}

for (let round = 1; round <= ROUNDS; round += 1) {
  test(`startup_error round ${round} cleans resources before readiness`, async () => {
    await runStartupErrorCase()
  })
}
