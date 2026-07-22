#!/usr/bin/env node

import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { spawnSync } from 'node:child_process'
import test from 'node:test'

const ROOT = path.resolve(import.meta.dirname, '../..')
const SCRIPT = path.join(ROOT, 'feature/tests/scheduler-fairness-sticky-race.mjs')

function databases(count = 12) {
  return Array.from({ length: count }, (_, index) => (
    `kiro_e0102_contract_${process.pid}_${index + 1}`
  )).join(',')
}

function fixtureEnv(overrides = {}) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'kiro-e0102-contract-'))
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
      KIRO_E01_E02_VALIDATE_ONLY: '1',
      KIRO_E01_E02_ROUNDS: '3',
      KIRO_E01_E02_POSTGRES_URL_TEMPLATE:
        'postgres://user:pass@127.0.0.1:50891/{database}',
      KIRO_E01_E02_POSTGRES_DATABASES: databases(),
      KIRO_E01_E02_REDIS_URL: 'redis://127.0.0.1:26379/6',
      KIRO_E01_E02_REDIS_PREFIX: `kiro_rs:e0102_contract:${process.pid}`,
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
  assert.equal(value.caseId, 'E01-E02-scheduler-fairness-sticky-race')
  assert.equal(value.dockerUsed, false)
  assert.equal(value.cargoUsed, false)
  assert.equal(value.protected9022ProbeSkipped, true)
  assert.equal(value.requiredDatabaseCount, 12)
  assert.deepEqual(value.modes, ['priority', 'balanced', 'health_balanced', 'weighted_least_inflight'])
  assert.equal(value.redisDatabase, 6)
})

test('database list must match modes times rounds', () => {
  const result = run({ KIRO_E01_E02_POSTGRES_DATABASES: databases(11) })
  assert.notEqual(result.status, 0)
  assert.match(result.stderr, /exactly 12 pre-created database names/)
})

test('mode subset changes required database count', () => {
  const result = run({
    KIRO_E01_E02_MODES: 'balanced,weighted_least_inflight',
    KIRO_E01_E02_POSTGRES_DATABASES: databases(6),
  })
  assert.equal(result.status, 0, result.stderr)
  const value = JSON.parse(result.stdout)
  assert.equal(value.requiredDatabaseCount, 6)
  assert.deepEqual(value.modes, ['balanced', 'weighted_least_inflight'])
})

test('rejects unsafe PostgreSQL configuration before runtime work', () => {
  const noPlaceholder = run({
    KIRO_E01_E02_POSTGRES_URL_TEMPLATE:
      'postgres://user:pass@127.0.0.1:50891/kiro_e0102_static',
  })
  assert.notEqual(noPlaceholder.status, 0)
  assert.match(noPlaceholder.stderr, /literal \{database\} placeholder/)

  const nonLoopback = run({
    KIRO_E01_E02_POSTGRES_URL_TEMPLATE:
      'postgres://user:pass@example.com:5432/{database}',
  })
  assert.notEqual(nonLoopback.status, 0)
  assert.match(nonLoopback.stderr, /must target loopback/)

  const protectedPort = run({
    KIRO_E01_E02_POSTGRES_URL_TEMPLATE:
      'postgres://user:pass@127.0.0.1:9022/{database}',
  })
  assert.notEqual(protectedPort.status, 0)
  assert.match(protectedPort.stderr, /port 9022 is protected/)

  const unsafeDb = run({ KIRO_E01_E02_POSTGRES_DATABASES: 'postgres,'.repeat(12).replace(/,$/, '') })
  assert.notEqual(unsafeDb.status, 0)
  assert.match(unsafeDb.stderr, /caller-owned kiro_e0102_\* names/)
})

test('rejects unsafe Redis configuration before runtime work', () => {
  const db0 = run({ KIRO_E01_E02_REDIS_URL: 'redis://127.0.0.1:26379/0' })
  assert.notEqual(db0.status, 0)
  assert.match(db0.stderr, /nonzero database in 1\.\.15/)

  const nonLoopback = run({ KIRO_E01_E02_REDIS_URL: 'redis://example.com:26379/6' })
  assert.notEqual(nonLoopback.status, 0)
  assert.match(nonLoopback.stderr, /must target loopback/)

  const protectedPort = run({ KIRO_E01_E02_REDIS_URL: 'redis://127.0.0.1:9022/6' })
  assert.notEqual(protectedPort.status, 0)
  assert.match(protectedPort.stderr, /port 9022 is protected/)

  const unsafePrefix = run({ KIRO_E01_E02_REDIS_PREFIX: 'kiro_rs:local' })
  assert.notEqual(unsafePrefix.status, 0)
  assert.match(unsafePrefix.stderr, /caller-owned temporary prefix/)
})

test('runner source never invokes Docker or Cargo', () => {
  const source = fs.readFileSync(SCRIPT, 'utf8')
  assert.doesNotMatch(source, /spawn(?:Sync)?\(\s*['"]docker['"]/i)
  assert.doesNotMatch(source, /spawn(?:Sync)?\(\s*['"]cargo['"]/i)
  assert.doesNotMatch(source, /\bdocker\s+(?:run|exec|compose|start|rm|pull|build)\b/i)
  assert.doesNotMatch(source, /\bcargo\s+(?:test|check|build|run|fetch)\b/i)
})

test('weighted least inflight dynamic case uses pure normalized load score', () => {
  const source = fs.readFileSync(SCRIPT, 'utf8')
  assert.match(
    source,
    /schedulerPriorityWeight:\s*mode\s*===\s*['"]weighted_least_inflight['"]\s*\?\s*0\s*:\s*1/,
  )
  assert.match(
    source,
    /schedulerErrorWeight:\s*mode\s*===\s*['"]weighted_least_inflight['"]\s*\?\s*0\s*:\s*100/,
  )
  assert.match(
    source,
    /schedulerLatencyWeight:\s*mode\s*===\s*['"]weighted_least_inflight['"]\s*\?\s*0\s*:\s*0\.01/,
  )
  assert.match(
    source,
    /schedulerProbationWeight:\s*mode\s*===\s*['"]weighted_least_inflight['"]\s*\?\s*0\s*:\s*50/,
  )
  assert.match(
    source,
    /schedulerSelectionPressureWeight:\s*mode\s*===\s*['"]weighted_least_inflight['"]\s*\?\s*0\s*:\s*25/,
  )
  assert.match(source, /scoreDefinition:\s*['"]inFlightRequests \/ maxConcurrentRequests['"]/)
  assert.match(source, /function waitForRuntimeConcurrency\(/)
  assert.match(source, /await waitForRuntimeConcurrency\(primary\.baseUrl,\s*expectedConcurrency,\s*casePrefix\)/)
})

test('focused scheduler runner uses FD and absolute RSS resource gates', () => {
  const source = fs.readFileSync(SCRIPT, 'utf8')
  assert.match(source, /maxFocusedRssEndKb:\s*256\s*\*\s*1024/)
  assert.match(source, /value\.fdCount\.end\s*<=\s*value\.fdCount\.start\s*\+\s*ACCEPTANCE_CONTRACT\.resources\.maxFdEndGrowth/)
  assert.match(source, /value\.rssKb\.end\s*<=\s*ACCEPTANCE_CONTRACT\.resources\.maxFocusedRssEndKb/)
  assert.doesNotMatch(source, /maxRssEndOverPeakGrowthKb/)
})
