import assert from 'node:assert/strict'
import { test } from 'node:test'
import { isKiroApiKeyAccount, runKiroApiKeyBatch, serializeKiroApiKeys } from '../src/lib/kiro-api-keys.ts'
import { parseImportText } from '../src/lib/credential-import/sources.ts'
import type { BatchCreateKiroApiKeysResponse, CreateKiroApiKeyItem } from '../src/types/api.ts'

const key: CreateKiroApiKeyItem = {
  keyId: 'official-1', keyPrefix: 'ksk_full', profileArn: 'profile', persisted: true, email: 'one@example.invalid',
  credentialId: 1, apiKey: 'ksk_full_test_value', label: 'test-1', region: 'eu-central-1', createdAt: 1786856404260,
}

function success(id: number): BatchCreateKiroApiKeysResponse {
  const result = { ...key, credentialId: id }
  return { total: 1, success: 1, skipped: 0, failed: 0, keys: [result], items: [{ credentialId: id, ok: true, skipped: false, result }] }
}

test('default export is one full key per line and metadata formats preserve information', () => {
  assert.equal(serializeKiroApiKeys([key]), 'ksk_full_test_value\n')
  assert.equal(serializeKiroApiKeys([key], 'txt'), 'ksk_full_test_value\n')
  assert.equal(serializeKiroApiKeys([key], 'email-txt'), 'one@example.invalid\tksk_full_test_value\n')
  assert.equal(serializeKiroApiKeys([key], 'region-txt'), 'ksk_full_test_value----eu-central-1\n')
  const json = JSON.parse(serializeKiroApiKeys([key], 'json'))
  assert.equal(json[0].email, key.email)
  assert.equal(json[0].apiKey, key.apiKey)
  assert.equal(json[0].persisted, undefined)
  const csv = serializeKiroApiKeys([{ ...key, label: 'name,"quoted"' }], 'csv')
  assert.ok(csv.includes('"name,""quoted"""'))
  assert.ok(csv.includes(key.apiKey))
  for (const format of ['txt', 'region-txt', 'json'] as const) {
    const parsed = parseImportText(serializeKiroApiKeys([key], format))
    assert.equal(parsed.accounts.length, 1)
    assert.equal(parsed.accounts[0].credential.kiroApiKey, key.apiKey)
    if (format !== 'txt') assert.equal(parsed.accounts[0].credential.region, key.region)
    assert.equal(parsed.accounts[0].credential.authMethod, 'api_key')
  }
})

test('API key accounts are excluded even if the auth marker is absent', () => {
  assert.equal(isKiroApiKeyAccount({ authMethod: 'API_KEY' }), true)
  assert.equal(isKiroApiKeyAccount({ maskedApiKey: 'ksk_***' }), true)
  assert.equal(isKiroApiKeyAccount({ apiKeyHash: 'hash' }), true)
  assert.equal(isKiroApiKeyAccount({ authMethod: 'social' }), false)
})

test('batch delivers results immediately, deduplicates IDs and continues after a request fails', async () => {
  const calls: number[] = []
  const results: BatchCreateKiroApiKeysResponse[] = []
  await runKiroApiKeyBatch([1, 2, 1, 3], 'prefix', async (id, prefix) => {
    assert.equal(results.length, calls.length, 'previous result must be delivered before next send')
    calls.push(id)
    assert.equal(prefix, 'prefix')
    if (id === 2) throw new Error('network failed')
    return success(id)
  }, (result) => results.push(result), () => false)
  assert.deepEqual(calls, [1, 2, 3])
  assert.equal(results[1].failed, 1)
  assert.equal(results[1].items[0].error, 'network failed')
  assert.deepEqual(results.flatMap((result) => result.keys).map((result) => result.credentialId), [1, 3])
})

test('stop preserves the current successful result without sending the next account', async () => {
  let stopped = false
  const calls: number[] = []
  const keys: CreateKiroApiKeyItem[] = []
  await runKiroApiKeyBatch([1, 2], 'test', async (id) => {
    calls.push(id)
    stopped = true
    return success(id)
  }, (result) => keys.push(...result.keys), () => stopped)
  assert.deepEqual(calls, [1])
  assert.equal(keys[0].apiKey, key.apiKey)
})
