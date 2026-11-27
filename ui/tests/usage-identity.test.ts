import assert from 'node:assert/strict'
import { test } from 'node:test'
import { accountIdentityLabel, usageRecordAccountIdentity } from '../src/features/usage/usage-identity.ts'

test('account identities retain names without repeating the id', () => {
  assert.equal(accountIdentityLabel(17, 'alice@example.com'), '#17 alice@example.com')
  assert.equal(accountIdentityLabel(17, '#17 alice@example.com'), '#17 alice@example.com')
})

test('usage errors fall back to the final retry account without mixing different account names', () => {
  const externalAttempts = [
    { attempt: 1, poolId: 17, poolName: 'alice@example.com', action: 'retry', durationMs: 1 },
    { attempt: 2, poolId: 23, poolName: 'bob@example.com', action: 'fail', durationMs: 2 },
  ]
  assert.equal(usageRecordAccountIdentity({ externalAttempts }), '#23 bob@example.com')
  assert.equal(usageRecordAccountIdentity({ accountId: 17, externalAttempts }), '#17 alice@example.com')
  assert.equal(usageRecordAccountIdentity({ accountId: 42, externalAttempts }), '#42')
})
