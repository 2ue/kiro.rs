// Run with: node --test ui/tests/   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  effectiveFeatures,
  effectiveItem,
  inheritedItem,
  routeEndpoint,
} from '../src/features/prompt-injection/injection-model.ts'
import { parseRoutePolicyImport, buildRoutePolicyFile } from '../src/features/cache-policies/route-policy-transfer.ts'
import type { PromptInjectionConfig, RouteFeaturesConfig } from '../src/types/api.ts'

function config(): PromptInjectionConfig {
  return {
    applyToCountTokens: true,
    defaults: {
      taskQuality: { enabled: false, externalPool: false },
      identityPrompt: { enabled: false, externalPool: false },
    } as PromptInjectionConfig['defaults'],
    texts: {},
    pathOverrides: {
      '/cc': { taskQuality: { enabled: true } },
      '/dfcache': { taskQuality: { enabled: true } },
      '/dfcache/a': { taskQuality: { enabled: false }, identityPrompt: { externalPool: true } },
    },
  }
}

test('routeEndpoint maps /v1 to its mounted messages path', () => {
  assert.equal(routeEndpoint('/v1'), '/v1/messages')
  assert.equal(routeEndpoint('/cc'), '/cc/v1/messages')
  assert.equal(routeEndpoint('/dfcache/a'), '/dfcache/a/v1/messages')
})

test('effectiveItem uses the longest matching prefix and reports its source', () => {
  const c = config()
  assert.deepEqual(effectiveItem(c, '/v1', 'taskQuality'), { enabled: false, externalPool: false, source: { kind: 'default' } })
  assert.equal(effectiveItem(c, '/cc', 'taskQuality').enabled, true)
  assert.deepEqual(effectiveItem(c, '/dfcache/b', 'taskQuality').source, { kind: 'route', prefix: '/dfcache' })
  assert.equal(effectiveItem(c, '/dfcache/a', 'taskQuality').enabled, false)
  // Fields resolve separately: externalPool comes from /dfcache/a, enabled from the default.
  const identity = effectiveItem(c, '/dfcache/a', 'identityPrompt')
  assert.equal(identity.enabled, false)
  assert.equal(identity.externalPool, true)
  // A prefix only matches at a segment boundary.
  assert.equal(effectiveItem(c, '/ccx', 'taskQuality').enabled, false)
})

test('inheritedItem ignores the route own setting', () => {
  const c = config()
  assert.equal(inheritedItem(c, '/dfcache/a', 'taskQuality').enabled, true)
  assert.equal(inheritedItem(c, '/cc', 'taskQuality').enabled, false)
})

test('effectiveFeatures applies shorter prefixes first, longer ones win', () => {
  const features: RouteFeaturesConfig = {
    defaults: {
      thinkingModelName: true,
      thinkingTrigger: 'real_request',
      channelLeakGuard: false,
      channelLeakGuardStream: true,
      modelDrivenWebSearch: false,
    },
    pathOverrides: {
      '/dfcache': { modelDrivenWebSearch: true, thinkingTrigger: 'off' },
      '/dfcache/a': { modelDrivenWebSearch: false },
    },
  }
  const a = effectiveFeatures(features, '/dfcache/a')
  assert.equal(a.modelDrivenWebSearch, false)
  assert.equal(a.thinkingTrigger, 'off')
  assert.equal(effectiveFeatures(features, '/dfcache/b').modelDrivenWebSearch, true)
  assert.equal(effectiveFeatures(features, '/v1').thinkingTrigger, 'real_request')
})

test('version 1 route files convert identity and WebSearch into the new sections', () => {
  const parsed = parseRoutePolicyImport(JSON.stringify({
    format: 'kiro-rs-route-policy',
    version: 1,
    cachePolicy: { cacheType: 'current_high_cache' },
    identity: { enabled: true, turnReminderEnabled: false, applyToExternalPool: true },
    webSearch: true,
  }))
  assert.equal(parsed.ok, true)
  if (!parsed.ok) return
  assert.deepEqual(parsed.promptInjection, {
    identityPrompt: { enabled: true, externalPool: true },
    identityTurnReminder: { enabled: false },
  })
  assert.deepEqual(parsed.features, { channelLeakGuard: true, modelDrivenWebSearch: true })

  const off = parseRoutePolicyImport(JSON.stringify({
    format: 'kiro-rs-route-policy',
    version: 1,
    cachePolicy: { cacheType: 'no_cache' },
    identity: { enabled: false, promptEnabled: true },
  }))
  assert.equal(off.ok, true)
  if (!off.ok) return
  assert.deepEqual(off.promptInjection, {
    identityPrompt: { enabled: false },
    identityTurnReminder: { enabled: false },
  })
  assert.deepEqual(off.features, { channelLeakGuard: false })
})

test('version 2 files round-trip and reject unknown items', () => {
  const file = buildRoutePolicyFile(
    '/dfcache/team',
    { cacheType: 'current_high_cache' },
    { taskQuality: { enabled: true }, identityPrompt: { externalPool: true } },
    { thinkingTrigger: 'always', modelDrivenWebSearch: true },
  )
  const parsed = parseRoutePolicyImport(JSON.stringify(file))
  assert.equal(parsed.ok, true)
  if (!parsed.ok) return
  assert.deepEqual(parsed.promptInjection, file.promptInjection)
  assert.deepEqual(parsed.features, file.features)

  const bad = parseRoutePolicyImport(JSON.stringify({ ...file, promptInjection: { notAnItem: { enabled: true } } }))
  assert.equal(bad.ok, false)
  const badTrigger = parseRoutePolicyImport(JSON.stringify({ ...file, features: { thinkingTrigger: 'sometimes' } }))
  assert.equal(badTrigger.ok, false)
})
