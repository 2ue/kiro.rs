#!/usr/bin/env node

import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { spawn, spawnSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import test from 'node:test'

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..')
const WRAPPER = path.join(REPO_ROOT, 'feature/tests/run-cargo-scoped.sh')
const ROUNDS = 3

function createFixture(label) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), `kiro-build-lifecycle-${label}-`))
  const targetRoot = path.join(root, 'targets')
  const stateDir = path.join(root, 'state')
  fs.mkdirSync(targetRoot)
  fs.mkdirSync(stateDir)
  return {
    root,
    targetRoot,
    stateDir,
    env: {
      ...process.env,
      KIRO_VALIDATION_TARGET_ROOT: targetRoot,
      KIRO_VALIDATION_STATE_DIR: stateDir,
      KIRO_VALIDATION_TEST_MODE: '1',
      KIRO_VALIDATION_TEST_AVAILABLE_KIB: '1048576',
      KIRO_VALIDATION_MIN_FREE_KIB: '1',
      KIRO_VALIDATION_RESERVE_KIB: '1',
      KIRO_VALIDATION_MAX_BUILD_KIB: '1024',
      KIRO_VALIDATION_LOCK_TIMEOUT_SECS: '2',
    },
  }
}

function removeFixture(fixture) {
  fs.rmSync(fixture.root, { recursive: true, force: true })
}

function ownedEntries(directory, prefixes) {
  return fs.readdirSync(directory).filter((entry) => prefixes.some((prefix) => entry.startsWith(prefix)))
}

function assertNoOwnedResidue(fixture) {
  assert.deepEqual(ownedEntries(fixture.targetRoot, ['.validation-build-']), [])
  assert.deepEqual(ownedEntries(fixture.stateDir, ['.reservation-', '.reservation-tmp-']), [])
}

function runWrapperSync(fixture, args, options = {}) {
  return spawnSync('/bin/bash', [WRAPPER, ...args], {
    cwd: REPO_ROOT,
    env: fixture.env,
    encoding: 'utf8',
    timeout: options.timeout ?? 15_000,
    maxBuffer: 1024 * 1024,
  })
}

function spawnWrapper(fixture, scope, script) {
  return spawn('/bin/bash', [WRAPPER, scope, '--', '/bin/bash', '-lc', script], {
    cwd: REPO_ROOT,
    env: fixture.env,
    stdio: ['ignore', 'pipe', 'pipe'],
  })
}

function waitForExit(child, timeoutMs = 15_000) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`process ${child.pid} did not exit`)), timeoutMs)
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

async function waitFor(predicate, timeoutMs = 5_000) {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    const value = predicate()
    if (value) return value
    await new Promise((resolve) => setTimeout(resolve, 20))
  }
  throw new Error('condition did not become true before timeout')
}

function currentBuildDirectory(fixture) {
  const name = fs.readdirSync(fixture.targetRoot).find((entry) => entry.startsWith('.validation-build-'))
  return name ? path.join(fixture.targetRoot, name) : null
}

async function waitForCommandOwner(fixture) {
  return waitFor(() => {
    const buildDirectory = currentBuildDirectory(fixture)
    if (!buildDirectory) return null
    const metadata = path.join(buildDirectory, '.command-owner')
    return fs.existsSync(metadata) ? { buildDirectory, metadata } : null
  })
}

async function waitForProcessGroupExit(pgid, timeoutMs = 5_000) {
  await waitFor(() => {
    try {
      process.kill(-pgid, 0)
      return false
    } catch {
      return true
    }
  }, timeoutMs)
}

for (let round = 1; round <= ROUNDS; round += 1) {
  test(`success cleanup round ${round}`, () => {
    const fixture = createFixture(`success-${round}`)
    try {
      const result = runWrapperSync(fixture, [
        `success-${round}`,
        '--',
        '/bin/bash',
        '-lc',
        'mkdir -p "$CARGO_TARGET_DIR/probe"; printf ok > "$CARGO_TARGET_DIR/probe/result"',
      ])
      assert.equal(result.status, 0, result.stderr)
      assert.match(result.stderr, /removed=true reservation_released=true/)
      assertNoOwnedResidue(fixture)
    } finally {
      removeFixture(fixture)
    }
  })

  test(`business failure cleanup round ${round}`, () => {
    const fixture = createFixture(`failure-${round}`)
    try {
      const result = runWrapperSync(fixture, [
        `failure-${round}`,
        '--',
        '/bin/bash',
        '-lc',
        'mkdir -p "$CARGO_TARGET_DIR/probe"; printf failed > "$CARGO_TARGET_DIR/probe/result"; exit 23',
      ])
      assert.equal(result.status, 23, result.stderr)
      assert.match(result.stderr, /removed=true reservation_released=true/)
      assertNoOwnedResidue(fixture)
    } finally {
      removeFixture(fixture)
    }
  })
}

for (const [signal, expectedCode] of [['SIGHUP', 129], ['SIGINT', 130], ['SIGTERM', 143]]) {
  for (let round = 1; round <= ROUNDS; round += 1) {
    test(`${signal} cleanup round ${round}`, async () => {
      const fixture = createFixture(`${signal.toLowerCase()}-${round}`)
      const child = spawnWrapper(
        fixture,
        `${signal.toLowerCase()}-${round}`,
        'mkdir -p "$CARGO_TARGET_DIR/probe"; while :; do sleep 1; done',
      )
      const exitPromise = waitForExit(child)
      try {
        await waitForCommandOwner(fixture)
        child.kill(signal)
        const exit = await exitPromise
        assert.deepEqual(exit, { code: expectedCode, signal: null })
        assertNoOwnedResidue(fixture)
      } finally {
        if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL')
        removeFixture(fixture)
      }
    })
  }
}

for (let round = 1; round <= ROUNDS; round += 1) {
  test(`SIGKILL owner waits for command group before stale reap round ${round}`, async () => {
    const fixture = createFixture(`sigkill-${round}`)
    const child = spawnWrapper(
      fixture,
      `sigkill-${round}`,
      'mkdir -p "$CARGO_TARGET_DIR/probe"; while :; do sleep 1; done',
    )
    const exitPromise = waitForExit(child)
    let pgid = null
    try {
      const { metadata } = await waitForCommandOwner(fixture)
      pgid = Number(fs.readFileSync(path.join(metadata, 'pgid'), 'utf8').trim())
      assert.ok(Number.isInteger(pgid) && pgid > 1)

      child.kill('SIGKILL')
      assert.deepEqual(await exitPromise, { code: null, signal: 'SIGKILL' })

      const activeReap = runWrapperSync(fixture, ['--reap-stale'])
      assert.equal(activeReap.status, 75, activeReap.stderr)
      assert.ok(currentBuildDirectory(fixture))

      process.kill(-pgid, 'SIGKILL')
      await waitForProcessGroupExit(pgid)
      const staleReap = runWrapperSync(fixture, ['--reap-stale'])
      assert.equal(staleReap.status, 0, staleReap.stderr)
      assertNoOwnedResidue(fixture)
    } finally {
      if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL')
      if (pgid) {
        try { process.kill(-pgid, 'SIGKILL') } catch {}
      }
      removeFixture(fixture)
    }
  })
}

for (let round = 1; round <= ROUNDS; round += 1) {
  test(`unknown owner is preserved and blocks reap round ${round}`, () => {
    const fixture = createFixture(`unknown-${round}`)
    try {
      const unknown = path.join(
        fixture.targetRoot,
        `.validation-build-unknown.pid-999999.fixture-${round}`,
      )
      fs.mkdirSync(unknown)
      fs.writeFileSync(path.join(unknown, 'sentinel'), 'preserve\n')

      const result = runWrapperSync(fixture, ['--reap-stale'])
      assert.equal(result.status, 73, result.stderr)
      assert.equal(fs.readFileSync(path.join(unknown, 'sentinel'), 'utf8'), 'preserve\n')
      assert.deepEqual(ownedEntries(fixture.stateDir, ['.reservation-', '.reservation-tmp-']), [])
    } finally {
      removeFixture(fixture)
    }
  })
}
