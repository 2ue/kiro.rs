import assert from 'node:assert/strict'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { spawnSync } from 'node:child_process'
import test from 'node:test'

import { resolveRuntimeValidationPaths } from './runtime-validation-paths.mjs'
import { validationChildEnvironment } from './validation-child-env.mjs'

const ROOT = fs.realpathSync(path.resolve(import.meta.dirname, '../..'))
const THINKING_RUNNER = path.join(import.meta.dirname, 'thinking-effort-kiro-wire.mjs')
const RUNNERS = [
  'bare-invoke-claude-cli.mjs',
  'claude-cli-long-session-continue.mjs',
  'request-api-key-admission-multi-instance.mjs',
  'scheduler-fairness-sticky-race.mjs',
  'strict-local-first-routing.mjs',
  'aws-api-key-region-lifecycle.mjs',
  'frozen-load-chaos-runner.mjs',
  'thinking-effort-kiro-wire.mjs',
]

function withEnvironment(values, callback) {
  const previous = new Map()
  for (const [name, value] of Object.entries(values)) {
    previous.set(name, process.env[name])
    if (value === undefined) delete process.env[name]
    else process.env[name] = value
  }
  try {
    return callback()
  } finally {
    for (const [name, value] of previous) {
      if (value === undefined) delete process.env[name]
      else process.env[name] = value
    }
  }
}

function externalFixture(callback) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'kiro-runtime-paths-'))
  const binary = path.join(root, 'kiro-rs')
  const artifacts = path.join(root, 'artifacts')
  fs.writeFileSync(binary, 'frozen fixture', { mode: 0o700 })
  fs.mkdirSync(artifacts)
  try {
    return callback({ root, binary, artifacts })
  } finally {
    fs.rmSync(root, { recursive: true, force: true })
  }
}

test('accepts only owned external binary and artifact roots for five rounds', () => {
  for (let round = 0; round < 5; round += 1) {
    externalFixture(({ binary, artifacts }) => {
      const resolved = withEnvironment({
        KIRO_RS_BINARY: binary,
        KIRO_VALIDATION_ARTIFACT_DIR: artifacts,
      }, () => resolveRuntimeValidationPaths(ROOT))
      assert.equal(resolved.binary, fs.realpathSync(binary))
      assert.equal(resolved.artifactRoot, fs.realpathSync(artifacts))
    })
  }
})

test('rejects missing, relative, nonexistent, and wrong-type paths for five rounds', () => {
  externalFixture(({ root, binary, artifacts }) => {
    const cases = [
      {
        env: { KIRO_RS_BINARY: undefined, KIRO_VALIDATION_ARTIFACT_DIR: artifacts },
        message: /KIRO_RS_BINARY is required/,
      },
      {
        env: { KIRO_RS_BINARY: binary, KIRO_VALIDATION_ARTIFACT_DIR: undefined },
        message: /KIRO_VALIDATION_ARTIFACT_DIR is required/,
      },
      {
        env: { KIRO_RS_BINARY: './kiro-rs', KIRO_VALIDATION_ARTIFACT_DIR: artifacts },
        message: /KIRO_RS_BINARY must be an absolute path/,
      },
      {
        env: { KIRO_RS_BINARY: binary, KIRO_VALIDATION_ARTIFACT_DIR: './reports' },
        message: /KIRO_VALIDATION_ARTIFACT_DIR must be an absolute path/,
      },
      {
        env: { KIRO_RS_BINARY: path.join(root, 'missing'), KIRO_VALIDATION_ARTIFACT_DIR: artifacts },
        message: /KIRO_RS_BINARY does not exist/,
      },
      {
        env: { KIRO_RS_BINARY: artifacts, KIRO_VALIDATION_ARTIFACT_DIR: artifacts },
        message: /KIRO_RS_BINARY must reference a file/,
      },
      {
        env: { KIRO_RS_BINARY: binary, KIRO_VALIDATION_ARTIFACT_DIR: binary },
        message: /KIRO_VALIDATION_ARTIFACT_DIR must reference an existing directory/,
      },
    ]
    for (let round = 0; round < 5; round += 1) {
      for (const fixture of cases) {
        assert.throws(
          () => withEnvironment(fixture.env, () => resolveRuntimeValidationPaths(ROOT)),
          fixture.message,
        )
      }
    }
  })
})

test('rejects lexical and symlink paths that resolve into the repository for five rounds', () => {
  externalFixture(({ root, binary, artifacts }) => {
    const binaryLink = path.join(root, 'binary-link')
    const artifactLink = path.join(root, 'artifact-link')
    fs.symlinkSync(path.join(ROOT, 'Cargo.toml'), binaryLink)
    fs.symlinkSync(path.join(ROOT, 'feature'), artifactLink)
    const cases = [
      { KIRO_RS_BINARY: path.join(ROOT, 'Cargo.toml'), KIRO_VALIDATION_ARTIFACT_DIR: artifacts },
      { KIRO_RS_BINARY: binary, KIRO_VALIDATION_ARTIFACT_DIR: path.join(ROOT, 'feature') },
      { KIRO_RS_BINARY: binaryLink, KIRO_VALIDATION_ARTIFACT_DIR: artifacts },
      { KIRO_RS_BINARY: binary, KIRO_VALIDATION_ARTIFACT_DIR: artifactLink },
    ]
    for (let round = 0; round < 5; round += 1) {
      for (const environment of cases) {
        assert.throws(
          () => withEnvironment(environment, () => resolveRuntimeValidationPaths(ROOT)),
          /resolves inside the repository/,
        )
      }
    }
  })
})

test('canonicalizes external symlinks and rejects an artifact ancestor of the repository for five rounds', () => {
  for (let round = 0; round < 5; round += 1) {
    externalFixture(({ root, binary, artifacts }) => {
      const binaryLink = path.join(root, 'external-binary-link')
      const artifactLink = path.join(root, 'external-artifact-link')
      fs.symlinkSync(binary, binaryLink)
      fs.symlinkSync(artifacts, artifactLink)
      const resolved = withEnvironment({
        KIRO_RS_BINARY: binaryLink,
        KIRO_VALIDATION_ARTIFACT_DIR: artifactLink,
      }, () => resolveRuntimeValidationPaths(ROOT))
      assert.equal(path.isAbsolute(resolved.binary), true)
      assert.equal(path.isAbsolute(resolved.artifactRoot), true)
      assert.equal(resolved.binary, fs.realpathSync(binary))
      assert.equal(resolved.artifactRoot, fs.realpathSync(artifacts))
      assert.throws(
        () => withEnvironment({
          KIRO_RS_BINARY: binary,
          KIRO_VALIDATION_ARTIFACT_DIR: path.dirname(ROOT),
        }, () => resolveRuntimeValidationPaths(ROOT)),
        /must not contain the repository/,
      )
    })
  }
})

test('rejects direct debug and release Cargo outputs even when they are outside the repository', () => {
  for (let round = 0; round < 5; round += 1) {
    externalFixture(({ root, artifacts }) => {
      for (const profile of ['debug', 'release']) {
        const targetDirectory = path.join(root, 'target', profile)
        const targetBinary = path.join(targetDirectory, 'kiro-rs')
        const targetLink = path.join(root, `${profile}-candidate-link`)
        fs.mkdirSync(targetDirectory, { recursive: true })
        fs.writeFileSync(targetBinary, 'direct Cargo output fixture', { mode: 0o700 })
        fs.symlinkSync(targetBinary, targetLink)
        for (const candidate of [targetBinary, targetLink]) {
          assert.throws(
            () => withEnvironment({
              KIRO_RS_BINARY: candidate,
              KIRO_VALIDATION_ARTIFACT_DIR: artifacts,
            }, () => resolveRuntimeValidationPaths(ROOT)),
            /copied frozen candidate, not target\/debug or target\/release/,
          )
        }
      }
    })
  }
})

test('treats two-dot-prefixed names as inside a synthetic repository for five rounds', () => {
  for (let round = 0; round < 5; round += 1) {
    externalFixture(({ root, binary, artifacts }) => {
      const syntheticRepo = path.join(root, 'synthetic-repository')
      const internalBinary = path.join(syntheticRepo, '..candidate')
      const internalArtifacts = path.join(syntheticRepo, '..artifacts')
      fs.mkdirSync(syntheticRepo)
      fs.writeFileSync(internalBinary, 'internal fixture', { mode: 0o700 })
      fs.mkdirSync(internalArtifacts)
      assert.throws(
        () => withEnvironment({
          KIRO_RS_BINARY: internalBinary,
          KIRO_VALIDATION_ARTIFACT_DIR: artifacts,
        }, () => resolveRuntimeValidationPaths(syntheticRepo)),
        /resolves inside the repository/,
      )
      assert.throws(
        () => withEnvironment({
          KIRO_RS_BINARY: binary,
          KIRO_VALIDATION_ARTIFACT_DIR: internalArtifacts,
        }, () => resolveRuntimeValidationPaths(syntheticRepo)),
        /resolves inside the repository/,
      )
    })
  }
})

test('all runtime runners share the fail-closed external path contract', () => {
  for (const runner of RUNNERS) {
    const source = fs.readFileSync(path.join(import.meta.dirname, runner), 'utf8')
    assert.match(source, /resolveRuntimeValidationPaths\(ROOT\)/)
    assert.doesNotMatch(source, /target\/(?:debug|release)\/kiro-rs/)
    assert.doesNotMatch(source, /path\.join\(ROOT, ['"]target['"]/)
  }
})

test('runtime runners do not inspect an existing 9022 listener', () => {
  const forbiddenPatterns = [
    new RegExp('listenerSnapshot' + '\\(9022\\)'),
    new RegExp('listeningPids' + '\\(9022\\)'),
    new RegExp('protected9022' + 'PidsBefore'),
    new RegExp('protected9022' + 'Unchanged'),
  ]
  for (const runner of RUNNERS) {
    const source = fs.readFileSync(path.join(import.meta.dirname, runner), 'utf8')
    for (const pattern of forbiddenPatterns) assert.doesNotMatch(source, pattern)
  }
})

test('validation child environment is allowlisted and does not inherit credentials or storage URLs', () => {
  for (let round = 0; round < 5; round += 1) {
    const environment = withEnvironment({
      DATABASE_URL: 'postgres://secret@example.invalid/db',
      REDIS_URL: 'redis://secret@example.invalid/0',
      ANTHROPIC_API_KEY: 'sk-ant-secret',
      ANTHROPIC_AUTH_TOKEN: 'anthropic-token-secret',
      OPENAI_API_KEY: 'sk-openai-secret',
      KIRO_API_KEY: 'ksk_secret',
      KIRO_RS_TEST_REDIS_URL: 'redis://127.0.0.1:6379/9',
      KIRO_VALIDATION_SHOULD_NOT_LEAK: 'present',
      PATH: process.env.PATH || '/usr/bin:/bin',
    }, () => validationChildEnvironment({ KIRO_RS_PORT: '19022' }))

    assert.equal(environment.KIRO_RS_PORT, '19022')
    assert.equal(typeof environment.PATH, 'string')
    for (const forbidden of [
      'DATABASE_URL',
      'REDIS_URL',
      'ANTHROPIC_API_KEY',
      'ANTHROPIC_AUTH_TOKEN',
      'OPENAI_API_KEY',
      'KIRO_API_KEY',
      'KIRO_RS_TEST_REDIS_URL',
      'KIRO_VALIDATION_SHOULD_NOT_LEAK',
    ]) {
      assert.equal(environment[forbidden], undefined, `${forbidden} leaked in round ${round}`)
    }
  }
})

test('non-test validation runners do not inherit full process.env', () => {
  const runnerFiles = fs.readdirSync(import.meta.dirname)
    .filter((name) => name.endsWith('.mjs') && !name.endsWith('.test.mjs'))
    .sort()
  assert.ok(runnerFiles.length > 0)
  for (const runner of runnerFiles) {
    const source = fs.readFileSync(path.join(import.meta.dirname, runner), 'utf8')
    assert.doesNotMatch(source, /\.\.\.process\.env/, `${runner} inherits full process.env`)
  }
})

test('thinking wire runner rejects a non-executable external candidate before runtime setup', () => {
  for (let round = 0; round < 5; round += 1) {
    externalFixture(({ binary, artifacts }) => {
      fs.chmodSync(binary, 0o600)
      const result = spawnSync(process.execPath, [THINKING_RUNNER], {
        cwd: ROOT,
        env: {
          PATH: process.env.PATH || '/usr/bin:/bin',
          TMPDIR: os.tmpdir(),
          KIRO_RS_BINARY: binary,
          KIRO_VALIDATION_ARTIFACT_DIR: artifacts,
        },
        encoding: 'utf8',
        timeout: 5_000,
        maxBuffer: 1024 * 1024,
      })
      assert.ifError(result.error)
      assert.notEqual(result.status, 0)
      assert.match(result.stderr, /EACCES|permission denied/i)
      assert.deepEqual(fs.readdirSync(artifacts), [])
    })
  }
})
