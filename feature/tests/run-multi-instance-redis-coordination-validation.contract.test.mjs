import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import test from 'node:test'
import { spawn } from 'node:child_process'

const ROOT = fs.realpathSync(path.resolve(import.meta.dirname, '../..'))
const RUNNER = path.join(ROOT, 'feature/tests/run-multi-instance-redis-coordination-validation.mjs')
const LIVE_NONEMPTY_URL = String(
  process.env.KIRO_MULTI_INSTANCE_CONTRACT_NONEMPTY_REDIS_URL || '',
).trim()

function runRunner(environment) {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'kiro-multi-instance-contract-'))
  const marker = path.join(tempRoot, 'cargo-called')
  const bin = path.join(tempRoot, 'bin')
  fs.mkdirSync(bin)
  const cargo = path.join(bin, 'cargo')
  fs.writeFileSync(cargo, `#!/bin/sh\ntouch ${JSON.stringify(marker)}\nexit 97\n`, { mode: 0o755 })

  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [RUNNER], {
      cwd: ROOT,
      env: {
        PATH: `${bin}${path.delimiter}${process.env.PATH || '/usr/bin:/bin'}`,
        HOME: process.env.HOME || os.homedir(),
        TMPDIR: tempRoot,
        ...environment,
      },
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
      const cargoCalled = fs.existsSync(marker)
      fs.rmSync(tempRoot, { recursive: true, force: true })
      resolve({ code, signal, stdout, stderr, cargoCalled })
    })
  })
}

for (let round = 1; round <= 3; round += 1) {
  test(`missing Redis URL rejects before Cargo round ${round}`, async () => {
    const result = await runRunner({ KIRO_RS_TEST_REDIS_ISOLATED: '1' })
    assert.notEqual(result.code, 0)
    assert.match(result.stderr, /KIRO_MULTI_INSTANCE_REDIS_URL is required/)
    assert.equal(result.cargoCalled, false)
  })

  test(`isolation marker must be exact 1 round ${round}`, async () => {
    const result = await runRunner({
      KIRO_MULTI_INSTANCE_REDIS_URL: 'redis://127.0.0.1:26379/15',
      KIRO_RS_TEST_REDIS_ISOLATED: round === 1 ? '0' : 'true',
    })
    assert.notEqual(result.code, 0)
    assert.match(result.stderr, /KIRO_RS_TEST_REDIS_ISOLATED=1 is required/)
    assert.equal(result.cargoCalled, false)
  })

  test(`DB0 rejects before Redis and Cargo round ${round}`, async () => {
    const result = await runRunner({
      KIRO_MULTI_INSTANCE_REDIS_URL: 'redis://127.0.0.1:26379/0',
      KIRO_RS_TEST_REDIS_ISOLATED: '1',
    })
    assert.notEqual(result.code, 0)
    assert.match(result.stderr, /nonzero database in 1\.\.15/)
    assert.equal(result.cargoCalled, false)
  })

  test(`protected 9022 rejects without probing it round ${round}`, async () => {
    const result = await runRunner({
      KIRO_MULTI_INSTANCE_REDIS_URL: 'redis://127.0.0.1:9022/15',
      KIRO_RS_TEST_REDIS_ISOLATED: '1',
    })
    assert.notEqual(result.code, 0)
    assert.match(result.stderr, /port 9022 is protected/)
    assert.equal(result.cargoCalled, false)
  })
}

test('runner source has no Docker or protected-listener inspection', () => {
  const source = fs.readFileSync(RUNNER, 'utf8')
  assert.doesNotMatch(source, /docker\s+(?:compose|run|exec|start|stop|rm)/i)
  assert.doesNotMatch(source, /\bFLUSH(?:DB|ALL)\b/i)
  assert.doesNotMatch(source, /lsof[^\n]*9022|netstat[^\n]*9022|ss[^\n]*9022/i)
})

test('runner refuses dirty Redis and reports residue instead of flushing', () => {
  const source = fs.readFileSync(RUNNER, 'utf8')
  assert.match(source, /const before = await redisCommands\(redisTarget, \['DBSIZE'\]\)/)
  assert.match(source, /if \(before !== 0\)/)
  assert.match(source, /residualKeyCount/)
  assert.match(source, /databaseFlushed: false/)
})

test('caller-confirmed nonempty Redis rejects before Cargo', {
  skip: LIVE_NONEMPTY_URL ? false : 'set KIRO_MULTI_INSTANCE_CONTRACT_NONEMPTY_REDIS_URL to opt in',
}, async () => {
  const parsed = new URL(LIVE_NONEMPTY_URL)
  assert.equal(parsed.protocol, 'redis:')
  assert.ok(['127.0.0.1', 'localhost', '::1'].includes(parsed.hostname))
  assert.notEqual(parsed.port, '9022')
  const result = await runRunner({
    KIRO_MULTI_INSTANCE_REDIS_URL: LIVE_NONEMPTY_URL,
    KIRO_RS_TEST_REDIS_ISOLATED: '1',
  })
  assert.notEqual(result.code, 0)
  assert.match(result.stderr, /is not empty/)
  assert.equal(result.cargoCalled, false)
})
