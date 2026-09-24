#!/usr/bin/env node

import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { spawnSync } from 'node:child_process'
import test from 'node:test'

const ROOT = path.resolve(import.meta.dirname, '../..')
const SCRIPT = path.join(ROOT, 'feature/tests/strict-local-first-routing.mjs')

function fixtureEnv(overrides = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'account-runtime-e05-contract-'))
  const binary = path.join(root, 'account-runtime')
  const artifact = path.join(root, 'artifacts')
  fs.writeFileSync(binary, '#!/bin/sh\nexit 0\n', { mode: 0o700 })
  fs.mkdirSync(artifact, { recursive: true, mode: 0o700 })
  const modes = overrides.ACCOUNT_RUNTIME_E05_MODES || 'no_credentials,scheduler_redis_chaos'
  const rounds = Number.parseInt(overrides.ACCOUNT_RUNTIME_E05_ROUNDS || '3', 10)
  const databaseCount = modes.split(',').filter(Boolean).length * rounds
  const databases = Array.from({ length: databaseCount }, (_, index) => (
    `account_runtime_e05_contract_${String(index + 1).padStart(2, '0')}`
  )).join(',')
  return {
    root,
    env: {
      ...process.env,
      ACCOUNT_RUNTIME_BINARY: binary,
      ACCOUNT_RUNTIME_VALIDATION_ARTIFACT_DIR: artifact,
      ACCOUNT_RUNTIME_E05_VALIDATE_ONLY: '1',
      ACCOUNT_RUNTIME_E05_MODES: modes,
      ACCOUNT_RUNTIME_E05_ROUNDS: String(rounds),
      ACCOUNT_RUNTIME_E05_POSTGRES_URL_TEMPLATE: 'postgres://e05:e05-password@127.0.0.1:15432/{database}',
      ACCOUNT_RUNTIME_E05_POSTGRES_DATABASES: databases,
      ACCOUNT_RUNTIME_E05_REDIS_URL: 'redis://127.0.0.1:16379/5',
      ACCOUNT_RUNTIME_E05_REDIS_PREFIX: 'account_runtime:e05_contract',
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

test('validate-only accepts caller-owned PostgreSQL and Redis inputs', () => {
  const result = run()
  assert.equal(result.status, 0, result.stderr)
  const body = JSON.parse(result.stdout)
  assert.equal(body.result, 'validate-only-pass')
  assert.equal(body.requiredDatabases, 6)
  assert.equal(body.dockerUsed, false)
  assert.equal(body.createsPostgresDatabases, false)
  assert.equal(body.flushDbUsed, false)
  assert.equal(body.protected9022ProbeSkipped, true)
})

test('rejects missing or non-owned PostgreSQL database list before runtime work', () => {
  const result = run({ ACCOUNT_RUNTIME_E05_POSTGRES_DATABASES: 'postgres,account_runtime_e05_good_001' })
  assert.notEqual(result.status, 0)
  assert.match(result.stderr, /exactly 6 pre-created database names|caller-owned account_runtime_e05_/)
})

test('rejects Redis DB0 and protected port before runtime work', () => {
  const db0 = run({ ACCOUNT_RUNTIME_E05_REDIS_URL: 'redis://127.0.0.1:16379/0' })
  assert.notEqual(db0.status, 0)
  assert.match(db0.stderr, /isolated nonzero database/)

  const protectedPort = run({ ACCOUNT_RUNTIME_E05_REDIS_URL: 'redis://127.0.0.1:9022/5' })
  assert.notEqual(protectedPort.status, 0)
  assert.match(protectedPort.stderr, /port 9022 is protected/)
})

test('rejects unsafe Redis prefix before runtime work', () => {
  const result = run({ ACCOUNT_RUNTIME_E05_REDIS_PREFIX: 'account_runtime:local:e05' })
  assert.notEqual(result.status, 0)
  assert.match(result.stderr, /caller-owned temporary prefix/)
})

test('source is non-Docker and uses redis-chaos-proxy plus bounded prefix cleanup', () => {
  const source = fs.readFileSync(SCRIPT, 'utf8')
  assert.doesNotMatch(source, /spawnSync\(['"]docker['"]/)
  assert.doesNotMatch(source, /CREATE DATABASE/)
  assert.doesNotMatch(source, /FLUSHDB/)
  assert.doesNotMatch(source, /ACCOUNT_RUNTIME_E05_ALLOW_DOCKER/)
  assert.doesNotMatch(source, /\.\.\.process\.env/)
  assert.match(source, /minimalChildEnv/)
  assert.match(source, /redis-chaos-proxy\.mjs/)
  assert.match(source, /cleanupOwnedRedisKeys/)
  assert.match(source, /ACCOUNT_RUNTIME_E05_VALIDATE_ONLY/)
})
