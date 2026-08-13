import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { spawn, spawnSync } from 'node:child_process'

const ROOT = path.resolve(import.meta.dirname, '../..')
const RUNNER = path.join(ROOT, 'feature/tests/thinking-effort-claude-cli-capture.mjs')
const THIS_TEST = path.join(ROOT, 'feature/tests/thinking-effort-claude-cli-capture-signal.test.mjs')
const TEMP_PREFIX = 'thinking-effort-capture-'
const SIGNALS = [
  ['SIGHUP', 129],
  ['SIGINT', 130],
  ['SIGTERM', 143],
]

function command(command, args) {
  return spawnSync(command, args, {
    cwd: ROOT,
    encoding: 'utf8',
    maxBuffer: 4 * 1024 * 1024,
  })
}

function captureTempDirs() {
  const root = os.tmpdir()
  return new Set(
    fs.readdirSync(root, { withFileTypes: true })
      .filter((entry) => entry.isDirectory() && entry.name.startsWith(TEMP_PREFIX))
      .map((entry) => path.join(root, entry.name)),
  )
}

function processExists(pid) {
  if (!Number.isInteger(pid) || pid <= 0) return false
  try {
    process.kill(pid, 0)
    return true
  } catch {
    return false
  }
}

function processStartIdentity(pid) {
  if (!processExists(pid)) return null
  const result = command('ps', ['-o', 'lstart=', '-p', String(pid)])
  if (result.status !== 0) return null
  const value = String(result.stdout || '').trim()
  return value || null
}

function sameProcessExists(owner) {
  return processStartIdentity(owner.pid) === owner.startIdentity
}

function directChildren(pid) {
  const result = command('pgrep', ['-P', String(pid)])
  if (result.status !== 0 && result.status !== 1) {
    throw new Error(`pgrep failed for pid ${pid}`)
  }
  return String(result.stdout || '')
    .split(/\s+/)
    .filter(Boolean)
    .map(Number)
    .filter(Number.isInteger)
}

function descendants(pid) {
  const found = []
  const pending = [pid]
  const seen = new Set(pending)
  while (pending.length > 0) {
    const parent = pending.shift()
    for (const child of directChildren(parent)) {
      if (seen.has(child)) continue
      seen.add(child)
      found.push(child)
      pending.push(child)
    }
  }
  return found
}

function listeningPortsForPid(pid) {
  const result = command('lsof', [
    '-nP',
    '-a',
    '-p',
    String(pid),
    '-iTCP',
    '-sTCP:LISTEN',
    '-F',
    'n',
  ])
  if (result.status !== 0 && result.status !== 1) {
    throw new Error(`lsof failed for pid ${pid}`)
  }
  return String(result.stdout || '')
    .split(/\r?\n/)
    .filter((line) => line.startsWith('n'))
    .map((line) => line.match(/:(\d+)$/)?.[1])
    .filter(Boolean)
    .map(Number)
    .sort((left, right) => left - right)
}

function listeningPids(port) {
  const result = command('lsof', ['-nP', `-iTCP:${port}`, '-sTCP:LISTEN', '-t'])
  if (result.status !== 0 && result.status !== 1) {
    throw new Error(`lsof failed for port ${port}`)
  }
  return String(result.stdout || '')
    .split(/\s+/)
    .filter(Boolean)
    .map(Number)
    .sort((left, right) => left - right)
}

async function waitFor(predicate, description, timeoutMs = 20_000) {
  const deadline = Date.now() + timeoutMs
  let lastError = null
  while (Date.now() < deadline) {
    try {
      const value = predicate()
      if (value) return value
    } catch (error) {
      lastError = error
    }
    await new Promise((resolve) => setTimeout(resolve, 25))
  }
  throw new Error(`timed out waiting for ${description}${lastError ? `: ${lastError.message}` : ''}`)
}

async function stopRunner(child) {
  if (!child || child.exitCode !== null) return
  try {
    process.kill(-child.pid, 'SIGKILL')
  } catch {
    child.kill('SIGKILL')
  }
  await Promise.race([
    new Promise((resolve) => child.once('exit', resolve)),
    new Promise((resolve) => setTimeout(resolve, 5_000)),
  ])
}

async function runSignalCase(signal, expectedExitCode, round) {
  const tempBefore = captureTempDirs()
  const stdout = []
  const stderr = []
  const child = spawn(process.execPath, [RUNNER], {
    cwd: ROOT,
    env: {
      ...process.env,
      KIRO_THINKING_CAPTURE_ROUNDS: '5',
    },
    detached: true,
    stdio: ['ignore', 'pipe', 'pipe'],
  })
  child.stdout.on('data', (chunk) => stdout.push(chunk))
  child.stderr.on('data', (chunk) => stderr.push(chunk))
  let ownedTemp = null
  let ownedPort = null
  let ownedDescendants = []

  try {
    const ready = await waitFor(() => {
      if (child.exitCode !== null) {
        throw new Error(`runner exited early with ${child.exitCode}`)
      }
      const newTemps = [...captureTempDirs()].filter((entry) => !tempBefore.has(entry))
      const ports = listeningPortsForPid(child.pid).filter((port) => port !== 9022)
      const childPids = descendants(child.pid)
      if (newTemps.length !== 1 || ports.length !== 1 || childPids.length === 0) return null
      return { newTemps, ports, childPids }
    }, `${signal} fixture resources in round ${round}`)

    ownedTemp = ready.newTemps[0]
    ownedPort = ready.ports[0]
    ownedDescendants = ready.childPids
      .map((pid) => ({ pid, startIdentity: processStartIdentity(pid) }))
      .filter((owner) => owner.startIdentity !== null)
    assert.notEqual(ownedPort, 9022)
    assert.equal(fs.existsSync(ownedTemp), true)
    assert.deepEqual(listeningPids(ownedPort), [child.pid])

    process.kill(-child.pid, signal)
    const outcome = await new Promise((resolve, reject) => {
      child.once('error', reject)
      child.once('exit', (code, exitSignal) => resolve({ code, exitSignal }))
    })
    assert.equal(outcome.code, expectedExitCode, `${signal} round ${round}: stderr=${Buffer.concat(stderr).toString('utf8').slice(0, 300)}`)
    assert.equal(outcome.exitSignal, null)

    let cleanupState = null
    try {
      await waitFor(
        () => {
          cleanupState = {
            tempExists: fs.existsSync(ownedTemp),
            listenerPids: listeningPids(ownedPort),
            liveDescendantOwners: ownedDescendants
              .filter(sameProcessExists)
              .map((owner) => owner.pid),
          }
          return !cleanupState.tempExists
            && cleanupState.listenerPids.length === 0
            && cleanupState.liveDescendantOwners.length === 0
        },
        `${signal} cleanup in round ${round}`,
      )
    } catch (error) {
      throw new Error(`${error.message}; state=${JSON.stringify(cleanupState)}`)
    }
    assert.equal(Buffer.concat(stdout).toString('utf8').includes('"result": "observation_complete"'), false)
    return {
      signal,
      round,
      exitCode: outcome.code,
      descendants: ownedDescendants.length,
      cleanup: true,
    }
  } finally {
    await stopRunner(child)
    for (const candidate of [...captureTempDirs()].filter((entry) => !tempBefore.has(entry))) {
      fs.rmSync(candidate, { recursive: true, force: true })
    }
    if (ownedPort !== null) assert.deepEqual(listeningPids(ownedPort), [])
    for (const owner of ownedDescendants) assert.equal(sameProcessExists(owner), false)
  }
}

test('thinking effort capture does not inspect an existing 9022 listener', () => {
  const forbiddenPatterns = [
    new RegExp('listeningPids' + '\\(9022\\)'),
    new RegExp('protected9022' + 'PidsBefore'),
    new RegExp('protected9022' + 'Unchanged'),
  ]
  for (const file of [RUNNER, THIS_TEST]) {
    const source = fs.readFileSync(file, 'utf8')
    for (const pattern of forbiddenPatterns) assert.doesNotMatch(source, pattern)
  }
})

test('thinking effort capture cleans owned runtime on HUP INT and TERM for three rounds each', { timeout: 180_000 }, async () => {
  const results = []
  for (const [signal, expectedExitCode] of SIGNALS) {
    for (let round = 1; round <= 3; round += 1) {
      results.push(await runSignalCase(signal, expectedExitCode, round))
    }
  }
  assert.equal(results.length, 9)
  assert.equal(results.every((result) => result.cleanup), true)
})
