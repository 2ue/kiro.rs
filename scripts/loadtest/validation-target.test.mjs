import assert from 'node:assert/strict'
import fs from 'node:fs'
import path from 'node:path'
import test from 'node:test'

import { resolveLoadTarget } from './validation-target.mjs'

const RUNNERS = ['kiro-load-runner.mjs', 'kiro-conversation-load-runner.mjs']

test('accepts explicit loopback and explicit remote targets for five rounds', () => {
  for (let round = 0; round < 5; round += 1) {
    const local = resolveLoadTarget({ baseUrl: `http://127.0.0.1:${19022 + round}`, apiKey: 'fixture' }, {})
    assert.equal(local.baseUrl.port, String(19022 + round))
    assert.equal(local.apiKey, 'fixture')

    const remote = resolveLoadTarget({
      baseUrl: 'https://validation.invalid',
      apiKey: 'fixture',
      allowRemote: 'true',
    }, {})
    assert.equal(remote.baseUrl.hostname, 'validation.invalid')
  }
})

test('rejects missing, malformed, unsafe protocol, implicit remote, and missing key for five rounds', () => {
  const cases = [
    [{ apiKey: 'fixture' }, {}, /explicit --base-url/],
    [{ baseUrl: 'not-a-url', apiKey: 'fixture' }, {}, /base URL is invalid/],
    [{ baseUrl: 'file:///tmp/report', apiKey: 'fixture' }, {}, /must use http or https/],
    [{ baseUrl: 'https://validation.invalid', apiKey: 'fixture' }, {}, /explicit --allow-remote true/],
    [{ baseUrl: 'http://127.0.0.1:19022' }, {}, /explicit --api-key/],
  ]
  for (let round = 0; round < 5; round += 1) {
    for (const [args, environment, message] of cases) {
      assert.throws(() => resolveLoadTarget(args, environment), message)
    }
  }
})

test('rejects protected port 9022 through every loopback spelling for five rounds', () => {
  const targets = [
    'http://127.0.0.1:9022',
    'http://localhost:9022',
    'http://[::1]:9022',
  ]
  for (let round = 0; round < 5; round += 1) {
    for (const baseUrl of targets) {
      assert.throws(
        () => resolveLoadTarget({ baseUrl, apiKey: 'fixture', allowRemote: 'true' }, {}),
        /port 9022 is protected/,
      )
    }
  }
})

test('load runners contain no protected service or default-key fallback', () => {
  for (const runner of RUNNERS) {
    const source = fs.readFileSync(path.join(import.meta.dirname, runner), 'utf8')
    assert.match(source, /resolveLoadTarget\(args\)/)
    assert.doesNotMatch(source, /127\.0\.0\.1:9022/)
    assert.doesNotMatch(source, /sk-kiro-rs-local-debug/)
  }
})

test('loadtest documentation uses frozen binaries and external report roots', () => {
  const source = fs.readFileSync(
    path.resolve(import.meta.dirname, '../../docs/testing/loadtest.md'),
    'utf8',
  )
  assert.doesNotMatch(source, /cargo run --bin kiro_loadtest/)
  assert.doesNotMatch(source, /--report target\//)
  assert.doesNotMatch(source, /--base-url http:\/\/(?:127\.0\.0\.1|localhost):9022/)
  assert.match(source, /feature\/tests\/run-cargo-scoped\.sh/)
  assert.match(source, /KIRO_VALIDATION_ARTIFACT_DIR/)
})
