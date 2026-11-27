import assert from 'node:assert/strict'
import { test } from 'node:test'
import { credentialIdentityLabel } from '../src/features/usage/usage-identity.ts'

test('credential labels appear with ids in retry chains and remove duplicate id prefixes', () => {
  assert.equal(credentialIdentityLabel(17, 'alice@example.com'), '#17 alice@example.com')
  assert.equal(credentialIdentityLabel(17, '#17 alice@example.com'), '#17 alice@example.com')
  assert.equal(credentialIdentityLabel(17), '#17')

})
