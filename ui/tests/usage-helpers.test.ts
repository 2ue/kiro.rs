import assert from 'node:assert/strict'
import { test } from 'node:test'
import { credentialIdentityLabel, usageRecordCredentialIdentity } from '../src/features/usage/usage-identity.ts'

test('credential labels appear with ids in retry chains and remove duplicate id prefixes', () => {
  assert.equal(credentialIdentityLabel(17, 'alice@example.com'), '#17 alice@example.com')
  assert.equal(credentialIdentityLabel(17, '#17 alice@example.com'), '#17 alice@example.com')
  assert.equal(credentialIdentityLabel(17), '#17')

})

test('usage account identity falls back to the latest retry account label', () => {
  assert.equal(
    usageRecordCredentialIdentity({
      credentialId: undefined,
      credentialLabel: undefined,
      credentialAttempts: [
        { attempt: 1, credentialId: 17, credentialLabel: 'alice@example.com', action: 'retry', durationMs: 1 },
        { attempt: 2, credentialId: 23, credentialLabel: 'bob@example.com', action: 'fail', durationMs: 2 },
      ],
    }),
    '#23 bob@example.com',
  )
  assert.equal(
    usageRecordCredentialIdentity({
      credentialId: 17,
      credentialLabel: undefined,
      credentialAttempts: [{ attempt: 1, credentialId: 17, credentialLabel: 'alice@example.com', action: 'fail', durationMs: 1 }],
    }),
    '#17 alice@example.com',
  )
})
