import assert from 'node:assert/strict'
import { test } from 'node:test'
import { resolveCredentialAuthMethod } from '../src/lib/credential-auth.ts'

test('repairs legacy social marker on complete BuilderId OIDC credentials', () => {
  assert.equal(resolveCredentialAuthMethod({
    provider: 'BuilderId',
    authMethod: 'social',
    clientId: 'client',
    clientSecret: 'secret',
  }), 'idc')
})

test('keeps genuine social credentials social', () => {
  assert.equal(resolveCredentialAuthMethod({ provider: 'Google', authMethod: 'social' }), 'social')
  assert.equal(resolveCredentialAuthMethod({ provider: 'Github' }), 'social')
})

test('keeps external IdP fields on the external IdP path', () => {
  assert.equal(resolveCredentialAuthMethod({
    provider: 'Enterprise',
    clientId: 'client',
    clientSecret: 'secret',
    tokenEndpoint: 'https://login.example.invalid/token',
  }), 'external_idp')
})

test('infers AWS OIDC when the auth method is missing', () => {
  assert.equal(resolveCredentialAuthMethod({
    provider: 'BuilderId',
    clientId: 'client',
    clientSecret: 'secret',
  }), 'idc')
})
