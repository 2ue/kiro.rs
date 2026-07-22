#!/usr/bin/env node

import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { spawnSync } from 'node:child_process'
import test from 'node:test'

const ROOT = path.resolve(import.meta.dirname, '../..')
const SCRIPT = path.join(ROOT, 'feature/tests/external-takeover-scheduler-degraded-nondocker.mjs')

function fixtureEnv(overrides = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'kiro-external-takeover-contract-'))
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
      KIRO_EXTERNAL_TAKEOVER_POSTGRES_URL:
        'postgres://user:pass@127.0.0.1:50891/kiro_external_takeover_contract_a',
      KIRO_EXTERNAL_TAKEOVER_OUTER_ROUNDS: '1',
      KIRO_EXTERNAL_TAKEOVER_REDIS_URL: 'redis://127.0.0.1:26379/5',
      KIRO_EXTERNAL_TAKEOVER_REDIS_PREFIX: `kiro_rs:external_takeover_contract:${process.pid}`,
      KIRO_EXTERNAL_TAKEOVER_VALIDATE_ONLY: '1',
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

test('validate-only accepts caller-owned loopback PG/Redis and records no Docker/Cargo', () => {
  const result = run()
  assert.equal(result.status, 0, result.stderr)
  const value = JSON.parse(result.stdout)
  assert.equal(value.result, 'validate_only')
  assert.equal(value.dockerUsed, false)
  assert.equal(value.cargoUsed, false)
  assert.equal(value.protected9022ProbeSkipped, true)
  assert.equal(value.postgresDatabase, 'kiro_external_takeover_contract_a')
  assert.equal(value.postgresDatabaseCount, 1)
  assert.equal(value.redisDatabase, 5)
})

test('multi-round validation requires caller-owned PostgreSQL database isolation', () => {
  const isolated = run({
    KIRO_EXTERNAL_TAKEOVER_POSTGRES_URL: '',
    KIRO_EXTERNAL_TAKEOVER_POSTGRES_URL_TEMPLATE:
      'postgres://user:pass@127.0.0.1:50891/{database}',
    KIRO_EXTERNAL_TAKEOVER_POSTGRES_DATABASES:
      'kiro_external_takeover_contract_a,kiro_external_takeover_contract_b,kiro_external_takeover_contract_c',
    KIRO_EXTERNAL_TAKEOVER_OUTER_ROUNDS: '3',
  })
  assert.equal(isolated.status, 0, isolated.stderr)
  assert.equal(JSON.parse(isolated.stdout).postgresDatabaseCount, 3)

  const shared = run({
    KIRO_EXTERNAL_TAKEOVER_OUTER_ROUNDS: '3',
  })
  assert.notEqual(shared.status, 0)
  assert.match(shared.stderr, /URL_TEMPLATE plus DATABASES/)
})

test('rejects protected 9022 PostgreSQL port before runtime work', () => {
  const result = run({
    KIRO_EXTERNAL_TAKEOVER_POSTGRES_URL:
      'postgres://user:pass@127.0.0.1:9022/kiro_external_takeover_contract_a',
  })
  assert.notEqual(result.status, 0)
  assert.match(result.stderr, /port 9022 is protected/)
})

test('rejects protected 9022 Redis port before runtime work', () => {
  const result = run({ KIRO_EXTERNAL_TAKEOVER_REDIS_URL: 'redis://127.0.0.1:9022/5' })
  assert.notEqual(result.status, 0)
  assert.match(result.stderr, /port 9022 is protected/)
})

test('rejects Redis DB0 because it is not caller-isolated', () => {
  const result = run({ KIRO_EXTERNAL_TAKEOVER_REDIS_URL: 'redis://127.0.0.1:26379/0' })
  assert.notEqual(result.status, 0)
  assert.match(result.stderr, /isolated nonzero database/)
})

test('rejects non-loopback dependencies', () => {
  const pg = run({
    KIRO_EXTERNAL_TAKEOVER_POSTGRES_URL:
      'postgres://user:pass@example.com:5432/kiro_external_takeover_contract_a',
  })
  assert.notEqual(pg.status, 0)
  assert.match(pg.stderr, /must target loopback/)

  const redis = run({ KIRO_EXTERNAL_TAKEOVER_REDIS_URL: 'redis://example.com:26379/5' })
  assert.notEqual(redis.status, 0)
  assert.match(redis.stderr, /must target loopback/)
})

test('rejects unsafe database name and shared local prefix', () => {
  const pg = run({
    KIRO_EXTERNAL_TAKEOVER_POSTGRES_URL: 'postgres://user:pass@127.0.0.1:50891/postgres',
  })
  assert.notEqual(pg.status, 0)
  assert.match(pg.stderr, /kiro_external_takeover_\*/)

  const redis = run({ KIRO_EXTERNAL_TAKEOVER_REDIS_PREFIX: 'kiro_rs:local' })
  assert.notEqual(redis.status, 0)
  assert.match(redis.stderr, /caller-owned temporary prefix/)
})

test('runner source never invokes Docker or Cargo', () => {
  const source = fs.readFileSync(SCRIPT, 'utf8')
  assert.doesNotMatch(source, /spawn(?:Sync)?\(\s*['"]docker['"]/i)
  assert.doesNotMatch(source, /spawn(?:Sync)?\(\s*['"]cargo['"]/i)
  assert.doesNotMatch(source, /\bdocker\s+(?:run|exec|compose|start|rm|pull|build)\b/i)
  assert.doesNotMatch(source, /\bcargo\s+(?:test|check|build|run|fetch)\b/i)
})
