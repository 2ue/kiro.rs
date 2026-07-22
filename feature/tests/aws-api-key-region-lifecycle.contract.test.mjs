#!/usr/bin/env node

import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { spawnSync } from 'node:child_process'
import test from 'node:test'

const ROOT = path.resolve(import.meta.dirname, '../..')
const SCRIPT = path.join(ROOT, 'feature/tests/aws-api-key-region-lifecycle.mjs')

function fixtureEnv(overrides = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'kiro-f06-contract-'))
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
      KIRO_F06_POSTGRES_URL: 'postgres://f06:f06-password@127.0.0.1:25432/kiro_f06_contract_a',
      KIRO_F06_REDIS_URL: 'redis://127.0.0.1:26379/6',
      KIRO_F06_REDIS_PREFIX: `kiro_rs:f06_contract:${process.pid}`,
      KIRO_F06_VALIDATE_ONLY: '1',
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

test('validate-only accepts caller-owned loopback PostgreSQL and Redis without side effects', () => {
  const result = run()
  assert.equal(result.status, 0, result.stderr)
  const body = JSON.parse(result.stdout)
  assert.equal(body.result, 'validate_only')
  assert.equal(body.dockerUsed, false)
  assert.equal(body.cargoUsed, false)
  assert.equal(body.protected9022ProbeSkipped, true)
  assert.equal(body.postgresDatabase, 'kiro_f06_contract_a')
  assert.equal(body.redisDatabase, 6)
  assert.equal(body.createsPostgresDatabase, false)
  assert.equal(body.flushesRedisDatabase, false)
})

test('rejects protected 9022 PostgreSQL and Redis ports before runtime work', () => {
  const pg = run({
    KIRO_F06_POSTGRES_URL: 'postgres://f06:f06-password@127.0.0.1:9022/kiro_f06_contract_a',
  })
  assert.notEqual(pg.status, 0)
  assert.match(pg.stderr, /port 9022 is protected/)

  const redis = run({ KIRO_F06_REDIS_URL: 'redis://127.0.0.1:9022/6' })
  assert.notEqual(redis.status, 0)
  assert.match(redis.stderr, /port 9022 is protected/)
})

test('rejects shared Redis DB0 and shared local Redis prefix', () => {
  const db0 = run({ KIRO_F06_REDIS_URL: 'redis://127.0.0.1:26379/0' })
  assert.notEqual(db0.status, 0)
  assert.match(db0.stderr, /isolated nonzero database/)

  const prefix = run({ KIRO_F06_REDIS_PREFIX: 'kiro_rs:local' })
  assert.notEqual(prefix.status, 0)
  assert.match(prefix.stderr, /caller-owned temporary prefix/)
})

test('rejects non-loopback dependencies and unsafe database names', () => {
  const pgHost = run({
    KIRO_F06_POSTGRES_URL: 'postgres://f06:f06-password@example.com:25432/kiro_f06_contract_a',
  })
  assert.notEqual(pgHost.status, 0)
  assert.match(pgHost.stderr, /must target loopback/)

  const redisHost = run({ KIRO_F06_REDIS_URL: 'redis://example.com:26379/6' })
  assert.notEqual(redisHost.status, 0)
  assert.match(redisHost.stderr, /must target loopback/)

  const pgName = run({
    KIRO_F06_POSTGRES_URL: 'postgres://f06:f06-password@127.0.0.1:25432/postgres',
  })
  assert.notEqual(pgName.status, 0)
  assert.match(pgName.stderr, /kiro_f06_\*/)
})

test('runner source never invokes Docker, Cargo, database creation, Redis flush, or process-env inheritance', () => {
  const source = fs.readFileSync(SCRIPT, 'utf8')
  assert.doesNotMatch(source, /spawn(?:Sync)?\(\s*['"]docker['"]/i)
  assert.doesNotMatch(source, /\bdocker\s+(?:run|exec|compose|start|rm|pull|build|inspect)\b/i)
  assert.doesNotMatch(source, /spawn(?:Sync)?\(\s*['"]cargo['"]/i)
  assert.doesNotMatch(source, /\bcargo\s+(?:test|check|build|run|fetch)\b/i)
  assert.doesNotMatch(source, /\bCREATE\s+DATABASE\b/i)
  assert.doesNotMatch(source, /\bFLUSH(?:DB|ALL)\b/i)
  assert.doesNotMatch(source, /\.\.\.process\.env/)
})
