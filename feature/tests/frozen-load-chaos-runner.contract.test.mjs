#!/usr/bin/env node

import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { spawnSync } from 'node:child_process'
import test from 'node:test'

const ROOT = path.resolve(import.meta.dirname, '../..')
const SCRIPT = path.join(ROOT, 'feature/tests/frozen-load-chaos-runner.mjs')

function databases(count) {
  return Array.from({ length: count }, (_, index) => (
    `kiro_load_chaos_contract_${String(index + 1).padStart(2, '0')}`
  )).join(',')
}

function fixtureEnv(overrides = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'kiro-load-chaos-contract-'))
  const binary = path.join(root, 'kiro-rs')
  const loadtest = path.join(root, 'kiro-loadtest')
  const artifact = path.join(root, 'artifacts')
  fs.writeFileSync(binary, '#!/bin/sh\nexit 0\n', { mode: 0o700 })
  fs.writeFileSync(loadtest, '#!/bin/sh\nexit 0\n', { mode: 0o700 })
  fs.mkdirSync(artifact, { recursive: true, mode: 0o700 })
  return {
    root,
    env: {
      ...process.env,
      KIRO_RS_BINARY: binary,
      KIRO_LOADTEST_BINARY: loadtest,
      KIRO_VALIDATION_ARTIFACT_DIR: artifact,
      KIRO_LOAD_CHAOS_POSTGRES_URL_TEMPLATE:
        'postgres://load_chaos:isolated@127.0.0.1:25432/{database}',
      KIRO_LOAD_CHAOS_POSTGRES_DATABASES: databases(3),
      KIRO_LOAD_CHAOS_REDIS_URL: 'redis://127.0.0.1:26379/8',
      KIRO_LOAD_CHAOS_REDIS_PREFIX: `kiro_rs:load_chaos_contract:${process.pid}`,
      KIRO_LOAD_CHAOS_VALIDATE_ONLY: '1',
      ...overrides,
    },
  }
}

function run(overrides = {}, args = []) {
  const { root, env } = fixtureEnv(overrides)
  try {
    return spawnSync(process.execPath, [SCRIPT, ...args], {
      cwd: ROOT,
      env,
      encoding: 'utf8',
      maxBuffer: 8 * 1024 * 1024,
    })
  } finally {
    fs.rmSync(root, { recursive: true, force: true })
  }
}

test('script has valid JavaScript syntax', () => {
  const result = spawnSync(process.execPath, ['--check', SCRIPT], {
    cwd: ROOT,
    encoding: 'utf8',
  })
  assert.equal(result.status, 0, result.stderr)
})

test('validate-only accepts caller-owned l3 inputs without side effects', () => {
  const result = run()
  assert.equal(result.status, 0, result.stderr)
  const body = JSON.parse(result.stdout)
  assert.equal(body.result, 'validate_only')
  assert.equal(body.tier, 'l3')
  assert.equal(body.requiredDatabaseCount, 3)
  assert.equal(body.postgresDatabaseCount, 3)
  assert.equal(body.redisDatabase, 8)
  assert.equal(body.dockerUsed, false)
  assert.equal(body.cargoUsed, false)
  assert.equal(body.protected9022ProbeSkipped, true)
  assert.equal(body.createsPostgresDatabase, false)
  assert.equal(body.dropsPostgresDatabase, false)
  assert.equal(body.flushesRedisDatabase, false)
  assert.equal(body.inheritedProcessEnvironment, false)
})

test('tier determines exact caller-owned database count', () => {
  const l4Bad = run({ KIRO_LOAD_CHAOS_POSTGRES_DATABASES: databases(3) }, ['--tier', 'l4'])
  assert.notEqual(l4Bad.status, 0)
  assert.match(l4Bad.stderr, /exactly 6 pre-created database names/)

  const l4 = run({ KIRO_LOAD_CHAOS_POSTGRES_DATABASES: databases(6) }, ['--tier', 'l4'])
  assert.equal(l4.status, 0, l4.stderr)
  assert.equal(JSON.parse(l4.stdout).requiredDatabaseCount, 6)

  const l5 = run({ KIRO_LOAD_CHAOS_POSTGRES_DATABASES: databases(1) }, ['--tier', 'l5'])
  assert.equal(l5.status, 0, l5.stderr)
  assert.equal(JSON.parse(l5.stdout).requiredDatabaseCount, 1)
})

test('rejects unsafe PostgreSQL and Redis dependencies before runtime work', () => {
  const pgHost = run({
    KIRO_LOAD_CHAOS_POSTGRES_URL_TEMPLATE:
      'postgres://load_chaos:isolated@example.com:25432/{database}',
  })
  assert.notEqual(pgHost.status, 0)
  assert.match(pgHost.stderr, /must target loopback/)

  const pgPort = run({
    KIRO_LOAD_CHAOS_POSTGRES_URL_TEMPLATE:
      'postgres://load_chaos:isolated@127.0.0.1:9022/{database}',
  })
  assert.notEqual(pgPort.status, 0)
  assert.match(pgPort.stderr, /port 9022 is protected/)

  const pgName = run({ KIRO_LOAD_CHAOS_POSTGRES_DATABASES: 'postgres,postgres,postgres' })
  assert.notEqual(pgName.status, 0)
  assert.match(pgName.stderr, /caller-owned kiro_load_chaos_\* names/)

  const redisDb0 = run({ KIRO_LOAD_CHAOS_REDIS_URL: 'redis://127.0.0.1:26379/0' })
  assert.notEqual(redisDb0.status, 0)
  assert.match(redisDb0.stderr, /isolated nonzero database/)

  const redisAuth = run({ KIRO_LOAD_CHAOS_REDIS_URL: 'redis://:secret@127.0.0.1:26379/8' })
  assert.notEqual(redisAuth.status, 0)
  assert.match(redisAuth.stderr, /must not contain Redis auth material/)

  const redisPrefix = run({ KIRO_LOAD_CHAOS_REDIS_PREFIX: 'kiro_rs:local' })
  assert.notEqual(redisPrefix.status, 0)
  assert.match(redisPrefix.stderr, /caller-owned temporary prefix/)
})

test('rejects direct Cargo target binaries even when outside the repository', () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'kiro-load-chaos-target-contract-'))
  const targetBinary = path.join(root, 'target', 'release', 'kiro-loadtest')
  fs.mkdirSync(path.dirname(targetBinary), { recursive: true })
  fs.writeFileSync(targetBinary, '#!/bin/sh\nexit 0\n', { mode: 0o700 })
  try {
    const result = run({ KIRO_LOADTEST_BINARY: targetBinary })
    assert.notEqual(result.status, 0)
    assert.match(result.stderr, /not target\/debug or target\/release output/)
  } finally {
    fs.rmSync(root, { recursive: true, force: true })
  }
})

test('runner source never invokes Docker, Cargo, database creation, Redis flush, or process-env inheritance', () => {
  const source = fs.readFileSync(SCRIPT, 'utf8')
  assert.doesNotMatch(source, /spawn(?:Sync)?\(\s*['"]docker['"]/i)
  assert.doesNotMatch(source, /\bdocker\s+(?:run|exec|compose|start|rm|pull|build|inspect|ps|info)\b/i)
  assert.doesNotMatch(source, /spawn(?:Sync)?\(\s*['"]cargo['"]/i)
  assert.doesNotMatch(source, /\bcargo\s+(?:test|check|build|run|fetch)\b/i)
  assert.doesNotMatch(source, /\bCREATE\s+DATABASE\b/i)
  assert.doesNotMatch(source, /\bDROP\s+DATABASE\b/i)
  assert.doesNotMatch(source, /\bFLUSH(?:DB|ALL)\b/i)
  assert.doesNotMatch(source, /\.\.\.process\.env/)
})
