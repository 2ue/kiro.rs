import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { spawn } from 'node:child_process'

const ROOT = fs.realpathSync(path.resolve(import.meta.dirname, '../..'))
const RUNNER = path.join(ROOT, 'feature/tests/e03-real-two-process-scheduler.mjs')
const E03_ENV_KEYS = [
  'KIRO_RS_BINARY', 'KIRO_VALIDATION_ARTIFACT_DIR',
  'KIRO_E03_POSTGRES_URL_TEMPLATE', 'KIRO_E03_REDIS_URL', 'KIRO_E03_REDIS_PREFIX',
  'KIRO_E03_POSTGRES_DATABASES', 'KIRO_E03_OUTER_ROUNDS',
  'KIRO_E03_VALIDATE_ONLY', 'KIRO_E03_CONTRACT_HOLD', 'KIRO_E03_READY_FILE',
]

function fixture() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'kiro-e03-contract-'))
  const bin = path.join(root, 'candidate')
  const artifacts = path.join(root, 'artifacts')
  const fakeBin = path.join(root, 'bin')
  const sideEffect = path.join(root, 'side-effect')
  fs.mkdirSync(artifacts)
  fs.mkdirSync(fakeBin)
  fs.writeFileSync(bin, '#!/bin/sh\nexit 99\n', { mode: 0o755 })
  for (const name of ['cargo', 'docker']) {
    fs.writeFileSync(
      path.join(fakeBin, name),
      `#!/bin/sh\ntouch ${JSON.stringify(sideEffect)}\nexit 97\n`,
      { mode: 0o755 },
    )
  }
  return { root, bin, artifacts, fakeBin, sideEffect }
}

function cleanEnvironment(extra = {}, owned) {
  const environment = { ...process.env }
  for (const key of E03_ENV_KEYS) delete environment[key]
  return {
    ...environment,
    PATH: `${owned.fakeBin}${path.delimiter}${process.env.PATH || '/usr/bin:/bin'}`,
    HOME: process.env.HOME || os.homedir(),
    TMPDIR: owned.root,
    KIRO_RS_BINARY: owned.bin,
    KIRO_VALIDATION_ARTIFACT_DIR: owned.artifacts,
    KIRO_E03_POSTGRES_URL_TEMPLATE: 'postgres://kiro_rs:isolated@127.0.0.1:25432/{database}',
    KIRO_E03_POSTGRES_DATABASES: 'kiro_e03_contract_r1',
    KIRO_E03_REDIS_URL: 'redis://127.0.0.1:26379/15',
    KIRO_E03_REDIS_PREFIX: `kiro_rs.e03.contract.${process.pid}`,
    KIRO_E03_VALIDATE_ONLY: '1',
    ...extra,
  }
}

function runRunner(extra = {}) {
  const owned = fixture()
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [RUNNER], {
      cwd: ROOT,
      env: cleanEnvironment(extra, owned),
      stdio: ['ignore', 'pipe', 'pipe'],
    })
    let stdout = ''
    let stderr = ''
    child.stdout.setEncoding('utf8')
    child.stderr.setEncoding('utf8')
    child.stdout.on('data', (chunk) => { stdout += chunk })
    child.stderr.on('data', (chunk) => { stderr += chunk })
    child.once('error', reject)
    child.once('exit', (code, signal) => {
      const result = {
        code, signal, stdout, stderr,
        sideEffect: fs.existsSync(owned.sideEffect),
        leftovers: fs.readdirSync(owned.root).filter((name) => name.startsWith('e03-')),
      }
      fs.rmSync(owned.root, { recursive: true, force: true })
      resolve(result)
    })
  })
}

async function rejects(extra, pattern) {
  const result = await runRunner(extra)
  assert.notEqual(result.code, 0, JSON.stringify(result))
  assert.match(result.stderr, pattern)
  assert.equal(result.sideEffect, false, 'early rejection executed Docker or Cargo')
  assert.deepEqual(result.leftovers, [], 'early rejection left an owned temp directory')
}

for (let round = 1; round <= 3; round += 1) {
  test(`valid validate-only contract round ${round}`, async () => {
    const result = await runRunner()
    assert.equal(result.code, 0, result.stderr)
    assert.equal(result.sideEffect, false)
    assert.deepEqual(result.leftovers, [])
    const report = JSON.parse(result.stdout)
    assert.equal(report.result, 'validated')
    assert.equal(report.protected9022ProbeSkipped, true)
    assert.deepEqual(report.cleanup.redisPrefixKeysRemaining, [])
    assert.equal(report.cleanup.tempRemoved, true)
  })

  test(`missing PostgreSQL template rejects before side effects round ${round}`, async () => {
    await rejects({ KIRO_E03_POSTGRES_URL_TEMPLATE: '' }, /KIRO_E03_POSTGRES_URL_TEMPLATE is required/)
  })

  test(`missing Redis URL rejects before side effects round ${round}`, async () => {
    await rejects({ KIRO_E03_REDIS_URL: '' }, /KIRO_E03_REDIS_URL is required/)
  })

  test(`missing Redis prefix rejects before side effects round ${round}`, async () => {
    await rejects({ KIRO_E03_REDIS_PREFIX: '' }, /KIRO_E03_REDIS_PREFIX is required/)
  })

  test(`PostgreSQL template placeholder is mandatory round ${round}`, async () => {
    await rejects({ KIRO_E03_POSTGRES_URL_TEMPLATE: 'postgres://kiro_rs:x@127.0.0.1:25432/static' }, /literal \{database\} placeholder/)
  })

  test(`remote PostgreSQL rejects round ${round}`, async () => {
    await rejects({ KIRO_E03_POSTGRES_URL_TEMPLATE: 'postgres://kiro_rs:x@example.com:25432/{database}' }, /must target loopback/)
  })

  test(`remote Redis rejects round ${round}`, async () => {
    await rejects({ KIRO_E03_REDIS_URL: 'redis://example.com:26379/15' }, /must target loopback/)
  })

  test(`Redis DB0 rejects round ${round}`, async () => {
    await rejects({ KIRO_E03_REDIS_URL: 'redis://127.0.0.1:26379/0' }, /nonzero database in 1\.\.15/)
  })

  test(`Redis protected 9022 rejects without probing round ${round}`, async () => {
    await rejects({ KIRO_E03_REDIS_URL: 'redis://127.0.0.1:9022/15' }, /port 9022 is protected/)
  })

  test(`PostgreSQL protected 9022 rejects without probing round ${round}`, async () => {
    await rejects({ KIRO_E03_POSTGRES_URL_TEMPLATE: 'postgres://kiro_rs:x@127.0.0.1:9022/{database}' }, /port 9022 is protected/)
  })

  test(`production Redis prefix rejects round ${round}`, async () => {
    await rejects({ KIRO_E03_REDIS_PREFIX: 'kiro_rs:local' }, /caller-owned temporary prefix/)
  })

  test(`PostgreSQL database count mismatch rejects before side effects round ${round}`, async () => {
    await rejects({
      KIRO_E03_VALIDATE_ONLY: '0',
      KIRO_E03_OUTER_ROUNDS: '2',
      KIRO_E03_POSTGRES_DATABASES: 'kiro_e03_contract_r1',
    }, /must contain exactly 2 pre-created database names/)
  })

  test(`non-owned PostgreSQL database name rejects before side effects round ${round}`, async () => {
    await rejects({
      KIRO_E03_VALIDATE_ONLY: '0',
      KIRO_E03_POSTGRES_DATABASES: 'postgres',
    }, /caller-owned kiro_e03_\* names/)
  })

  test(`out-of-range rounds reject before side effects round ${round}`, async () => {
    await rejects({ KIRO_E03_OUTER_ROUNDS: round === 1 ? '0' : '4' }, /must be an integer in 1\.\.3/)
  })
}

for (const [signal, code] of [['SIGHUP', 129], ['SIGINT', 130], ['SIGTERM', 143]]) {
  for (let round = 1; round <= 3; round += 1) {
    test(`${signal} removes owned ready file and temp root round ${round}`, async () => {
      const owned = fixture()
      const readyFile = path.join(owned.root, `ready-${signal}-${round}.json`)
      const child = spawn(process.execPath, [RUNNER], {
        cwd: ROOT,
        env: cleanEnvironment({
          KIRO_E03_CONTRACT_HOLD: '1',
          KIRO_E03_READY_FILE: readyFile,
        }, owned),
        stdio: ['ignore', 'pipe', 'pipe'],
      })
      try {
        const ready = await new Promise((resolve, reject) => {
          const deadline = Date.now() + 10_000
          const poll = () => {
            if (fs.existsSync(readyFile)) return resolve(JSON.parse(fs.readFileSync(readyFile, 'utf8')))
            if (child.exitCode !== null) return reject(new Error(`runner exited before ready: ${child.exitCode}`))
            if (Date.now() >= deadline) return reject(new Error('ready file timeout'))
            setTimeout(poll, 20)
          }
          poll()
        })
        assert.ok(fs.existsSync(ready.tempRoot))
        child.kill(signal)
        const exit = await new Promise((resolve, reject) => {
          child.once('error', reject)
          child.once('exit', (exitCode, exitSignal) => resolve({ exitCode, exitSignal }))
        })
        assert.equal(exit.exitCode, code, JSON.stringify(exit))
        assert.equal(exit.exitSignal, null)
        assert.equal(fs.existsSync(readyFile), false)
        assert.equal(fs.existsSync(ready.tempRoot), false)
        assert.equal(fs.existsSync(owned.sideEffect), false)
      } finally {
        if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL')
        fs.rmSync(owned.root, { recursive: true, force: true })
      }
    })
  }
}

test('runner contains no Cargo, default target lookup, Docker invocation, or protected-port probe', () => {
  const source = fs.readFileSync(RUNNER, 'utf8')
  assert.doesNotMatch(source, /['"]cargo['"]|run-cargo-scoped|target\/(?:debug|release)/i)
  assert.doesNotMatch(source, /(?:spawn|spawnSync|command)\s*\([^\n]*['"]docker['"]/i)
  assert.doesNotMatch(source, /\blsof\s+[^\n]*9022|\bnetstat\s+[^\n]*9022|\bss\s+[^\n]*9022/i)
  assert.match(source, /spawn\(BINARY/)
  assert.match(source, /SIGKILL/)
  assert.match(source, /credentialInFlightLeaseMaxSecs/)
  assert.match(source, /scheduler:rate_limit:2/)
})
