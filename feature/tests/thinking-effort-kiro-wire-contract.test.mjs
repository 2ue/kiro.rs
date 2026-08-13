#!/usr/bin/env node

import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { spawnSync } from 'node:child_process'
import test from 'node:test'

const ROOT = path.resolve(import.meta.dirname, '../..')
const RUNNER = path.join(ROOT, 'feature/tests/thinking-effort-kiro-wire.mjs')
const source = fs.readFileSync(RUNNER, 'utf8')

function contractTempEntries() {
  return fs.readdirSync(os.tmpdir())
    .filter((name) => name.startsWith('thinking-wire-contract-'))
    .sort()
}

function runContractFixture() {
  const tempBefore = contractTempEntries()
  const result = spawnSync(process.execPath, [RUNNER], {
    cwd: ROOT,
    env: {
      PATH: process.env.PATH || '/usr/bin:/bin',
      TMPDIR: os.tmpdir(),
      KIRO_THINKING_WIRE_FIXTURE_MODE: 'contract',
      AWS_ACCESS_KEY_ID: 'must-not-be-inherited',
      AWS_SECRET_ACCESS_KEY: 'must-not-be-inherited',
      AWS_SESSION_TOKEN: 'must-not-be-inherited',
      ANTHROPIC_API_KEY: 'must-not-be-inherited',
      ANTHROPIC_AUTH_TOKEN: 'must-not-be-inherited',
      CLAUDE_CODE_OAUTH_TOKEN: 'must-not-be-inherited',
      KIRO_API_KEY: 'must-not-be-inherited',
      KIRO_RS_POSTGRES_URL: 'postgres://must-not-be-inherited.invalid/database',
      KIRO_RS_REDIS_URL: 'redis://must-not-be-inherited.invalid/0',
      DATABASE_URL: 'postgres://must-not-be-inherited.invalid/database',
      REDIS_URL: 'redis://must-not-be-inherited.invalid/0',
      PGPASSWORD: 'must-not-be-inherited',
      HTTP_PROXY: 'http://must-not-be-inherited.invalid',
      HTTPS_PROXY: 'http://must-not-be-inherited.invalid',
    },
    encoding: 'utf8',
    timeout: 10_000,
    maxBuffer: 4 * 1024 * 1024,
  })
  assert.ifError(result.error)
  assert.equal(result.status, 0, result.stderr)
  assert.equal(result.stderr, '')
  assert.deepEqual(contractTempEntries(), tempBefore)
  return JSON.parse(result.stdout)
}

test('contract fixture behavior is stable for five independent rounds', () => {
  for (let round = 0; round < 5; round += 1) {
    const facts = runContractFixture()
    assert.equal(facts.captureResults.length, 8)
    for (const result of facts.captureResults) assert.deepEqual(result.violations, [])
    assert.equal(facts.effort.explicitMax, 'max')
    assert.equal(facts.effort.disabled, null)
    assert.equal(facts.effort.schema.path, 'output_config.effort')
    assert.deepEqual(facts.effort.schema.values, ['low', 'medium', 'high', 'xhigh', 'max'])
    assert.deepEqual(facts.models, ['claude-opus-4-8', 'claude-opus-4.8'])
    assert.deepEqual(facts.protocolResults, [[], [], [], []])
    assert.ok(facts.mutatedProtocol.includes('method'))
    assert.ok(facts.mutatedProtocol.includes('path'))
    assert.ok(facts.mutatedProtocol.includes('host'))
    assert.ok(facts.mutatedProtocol.includes('content_type'))
    assert.deepEqual(facts.versionPolicy, {
      defaultMinimumAcceptsCurrent: {
        policy: 'minimum',
        actualVersion: '2.1.220',
        expectedVersion: null,
        minimumVersion: '2.1.197',
      },
      exactAcceptsMatching: {
        policy: 'exact',
        actualVersion: '2.1.220',
        expectedVersion: '2.1.220',
        minimumVersion: null,
      },
      belowMinimumRejected: true,
      exactMismatchRejected: true,
    })
    assert.deepEqual(facts.environment.serviceInheritedForbidden, [])
    assert.deepEqual(facts.environment.claudeInheritedForbidden, [])
    assert.equal(facts.environment.postgresPinned, true)
    assert.equal(facts.environment.redisPinned, true)
    assert.equal(facts.environment.migratePinned, true)
    assert.equal(facts.environment.compressionPinned, true)
    assert.equal(facts.environment.claudeBaseUrlPinned, true)
    assert.equal(facts.environment.claudeHomePinned, true)
    assert.deepEqual(facts.isolation.databases, {
      cli: 'kiro_thinking_wire_contract_owner_cli',
      ide: 'kiro_thinking_wire_contract_owner_ide',
    })
    assert.deepEqual(facts.isolation.markers, {
      cli: 'kiro-thinking-wire-owner:contract_owner:cli',
      ide: 'kiro-thinking-wire-owner:contract_owner:ide',
    })
    assert.equal(facts.isolation.invalidDatabaseRejected, true)
    assert.equal(facts.isolation.externalCwdAccepted, true)
    assert.equal(facts.isolation.repositoryCwdRejected, true)
    assert.deepEqual(facts.lifecycle, {
      shutdownSpawnRejected: true,
      nonDetachedSpawnRejected: true,
      matchingIdentityAccepted: true,
      reusedLeaderRejected: true,
      reusedMemberRejected: true,
      missingIdentityRejected: true,
      sentinelInitiallyIntact: true,
      sentinelCorruptionRejected: true,
      sentinelRetryCannotMaskCorruption: true,
    })
    assert.deepEqual(facts.reportPolicy, {
      safeAccepted: true,
      urlRejected: true,
      encodedUrlRejected: true,
      keyRejected: true,
      tempPathRejected: true,
      encodedSecretRejected: true,
    })
  }
})

test('normal mode fails closed before creating runtime resources when required paths are absent', () => {
  for (let round = 0; round < 5; round += 1) {
    const result = spawnSync(process.execPath, [RUNNER], {
      cwd: ROOT,
      env: {
        PATH: process.env.PATH || '/usr/bin:/bin',
        TMPDIR: os.tmpdir(),
      },
      encoding: 'utf8',
      timeout: 5_000,
      maxBuffer: 1024 * 1024,
    })
    assert.ifError(result.error)
    assert.notEqual(result.status, 0)
    assert.match(result.stderr, /KIRO_RS_BINARY is required/)
    assert.equal(result.stdout, '')
  }
})

test('source structure has no Cargo discovery, repository targets, inherited environment spread or normative thinking oracle', () => {
  assert.doesNotMatch(source, /\b(?:cargo|rustc)\b/)
  assert.doesNotMatch(source, /target\/(?:debug|release)/)
  assert.doesNotMatch(source, /path\.join\(ROOT, ['"]target['"]\)/)
  assert.doesNotMatch(source, /\.\.\.process\.env/)
  assert.doesNotMatch(source, /cli_wire_kept_unsupported_thinking/)
  assert.doesNotMatch(source, /ide_adaptive_thinking_injection_mismatch/)
  assert.doesNotMatch(source, /wireThinking\?\.type\s*!==/)
  assert.doesNotMatch(source, /ls-files['"],\s*['"]--others/)
  assert.doesNotMatch(source, /untrackedRaw|untracked source manifest/)
  assert.doesNotMatch(source, /Promise\.race/)
  assert.doesNotMatch(source, /listeningPids\(9022\)/)
  assert.match(source, /const FORBIDDEN_PORTS = new Set\(\[9022\]\)/)
  assert.match(source, /!FORBIDDEN_PORTS\.has\(port\)/)
  assert.match(source, /resolveRuntimeValidationPaths\(ROOT\)/)
  assert.match(source, /SOURCE_MANIFEST_PATHS/)
  assert.match(source, /--untracked-files=no/)
  assert.match(source, /protectedCredentialFilesExcluded: true/)
  assert.match(source, /untrackedFilesEnumerated: false/)
  assert.match(source, /await inspectPostgresIdentity\(endpoint, true\)/)
  assert.match(source, /trackOwnedChild\(/)
  assert.match(source, /spawnOwned\(/)
  assert.equal((source.match(/\bspawn\(/g) || []).length, 1)
  assert.match(source, /assertOwnedGroupIdentity\(/)
  assert.match(source, /validateOwnedGroupSnapshot\(/)
  assert.match(source, /waitForOwnedChildExit\(/)
  assert.match(source, /waitForOwnedChildClose\(/)
  assert.match(source, /tracked\.sockets\.size/)
  assert.match(source, /socket\.destroy\(\)/)
  assert.match(source, /assertExternalDirectory\(projectRoot, ['"]Claude cwd['"]\)/)
  assert.match(source, /frozenExecutableIdentity\(BINARY\)/)
  assert.match(source, /assertReportRedacted\(serialized, \[/)
  assert.match(source, /workingTreeSourceManifest\(/)
})
