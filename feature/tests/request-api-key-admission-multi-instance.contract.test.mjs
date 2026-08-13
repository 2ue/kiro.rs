#!/usr/bin/env node

import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { spawnSync } from 'node:child_process'
import test from 'node:test'

const ROOT = path.resolve(import.meta.dirname, '../..')
const SCRIPT = path.join(ROOT, 'feature/tests/request-api-key-admission-multi-instance.mjs')

function databases(count = 3) {
  return Array.from({ length: count }, (_, index) => (
    `kiro_request_admission_contract_${String(index + 1).padStart(2, '0')}`
  )).join(',')
}

function fixtureEnv(overrides = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'kiro-request-admission-contract-'))
  const binary = path.join(root, 'kiro-rs')
  const artifact = path.join(root, 'artifacts')
  fs.writeFileSync(binary, '#!/bin/sh\nexit 0\n', { mode: 0o700 })
  fs.mkdirSync(artifact, { recursive: true, mode: 0o700 })
  return {
    root,
    env: {
      ...process.env,
      KIRO_RS_BINARY: binary,
      KIRO_VALIDATION_ARTIFACT_DIR: artifact,
      KIRO_REQUEST_ADMISSION_ROUNDS: '3',
      KIRO_REQUEST_ADMISSION_POSTGRES_URL_TEMPLATE:
        'postgres://request_admission:isolated@127.0.0.1:25432/{database}',
      KIRO_REQUEST_ADMISSION_POSTGRES_DATABASES: databases(),
      KIRO_REQUEST_ADMISSION_REDIS_URL: 'redis://127.0.0.1:26379/7',
      KIRO_REQUEST_ADMISSION_REDIS_PREFIX: `kiro_rs:request_admission_contract:${process.pid}`,
      KIRO_REQUEST_ADMISSION_VALIDATE_ONLY: '1',
      ...overrides,
    },
  }
}

function run(overrides = {}) {
  const { root, env } = fixtureEnv(overrides)
  try {
    return spawnSync(process.execPath, [SCRIPT], {
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

test('validate-only accepts caller-owned loopback PostgreSQL and Redis inputs', () => {
  const result = run()
  assert.equal(result.status, 0, result.stderr)
  const body = JSON.parse(result.stdout)
  assert.equal(body.result, 'validate_only')
  assert.equal(body.dockerUsed, false)
  assert.equal(body.cargoUsed, false)
  assert.equal(body.protected9022ProbeSkipped, true)
  assert.equal(body.postgresDatabaseCount, 3)
  assert.equal(body.redisDatabase, 7)
  assert.equal(body.createsPostgresDatabase, false)
  assert.equal(body.flushesRedisDatabase, false)
  assert.equal(body.usesDockerToxiproxy, false)
})

test('database list must match rounds and use caller-owned names', () => {
  const count = run({ KIRO_REQUEST_ADMISSION_POSTGRES_DATABASES: databases(2) })
  assert.notEqual(count.status, 0)
  assert.match(count.stderr, /exactly 3 pre-created database names/)

  const unsafe = run({ KIRO_REQUEST_ADMISSION_POSTGRES_DATABASES: 'postgres,postgres,postgres' })
  assert.notEqual(unsafe.status, 0)
  assert.match(unsafe.stderr, /caller-owned kiro_request_admission_\* names/)
})

test('rejects unsafe PostgreSQL and Redis dependencies before runtime work', () => {
  const pgHost = run({
    KIRO_REQUEST_ADMISSION_POSTGRES_URL_TEMPLATE:
      'postgres://request_admission:isolated@example.com:25432/{database}',
  })
  assert.notEqual(pgHost.status, 0)
  assert.match(pgHost.stderr, /must target loopback/)

  const pgPort = run({
    KIRO_REQUEST_ADMISSION_POSTGRES_URL_TEMPLATE:
      'postgres://request_admission:isolated@127.0.0.1:9022/{database}',
  })
  assert.notEqual(pgPort.status, 0)
  assert.match(pgPort.stderr, /port 9022 is protected/)

  const redisDb0 = run({ KIRO_REQUEST_ADMISSION_REDIS_URL: 'redis://127.0.0.1:26379/0' })
  assert.notEqual(redisDb0.status, 0)
  assert.match(redisDb0.stderr, /isolated nonzero database/)

  const redisHost = run({ KIRO_REQUEST_ADMISSION_REDIS_URL: 'redis://example.com:26379/7' })
  assert.notEqual(redisHost.status, 0)
  assert.match(redisHost.stderr, /must target loopback/)

  const redisPrefix = run({ KIRO_REQUEST_ADMISSION_REDIS_PREFIX: 'kiro_rs:local' })
  assert.notEqual(redisPrefix.status, 0)
  assert.match(redisPrefix.stderr, /caller-owned temporary prefix/)
})

test('runner source uses local redis-chaos-proxy and never invokes Docker, Cargo, database creation, Redis flush, or env inheritance', () => {
  const source = fs.readFileSync(SCRIPT, 'utf8')
  assert.match(source, /redis-chaos-proxy\.mjs/)
  assert.doesNotMatch(source, /spawn(?:Sync)?\(\s*['"]docker['"]/i)
  assert.doesNotMatch(source, /\bdocker\s+(?:run|exec|compose|start|rm|pull|build|inspect|ps|info)\b/i)
  assert.doesNotMatch(source, /spawn(?:Sync)?\(\s*['"]cargo['"]/i)
  assert.doesNotMatch(source, /\bcargo\s+(?:test|check|build|run|fetch)\b/i)
  assert.doesNotMatch(source, /\bCREATE\s+DATABASE\b/i)
  assert.doesNotMatch(source, /\bFLUSH(?:DB|ALL)\b/i)
  assert.doesNotMatch(source, /host\.docker\.internal/)
  assert.doesNotMatch(source, /listenerSnapshot/)
  assert.doesNotMatch(source, /\.\.\.process\.env/)
})
