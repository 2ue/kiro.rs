// Run with: node --test ui/tests/*.test.ts   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  CACHE_HIT_RATE_CRITICAL,
  CACHE_HIT_RATE_WARNING,
  cacheHitRate,
  cacheHitRateLevel,
  sumCacheTokens,
} from '../src/features/overview/cache-hit-rate.ts'
import {
  compareUsageTier,
  credentialUsageTier,
  externalPoolUsageTier,
} from '../src/features/overview/usage-order.ts'

test('cache hit rate uses the sub2api denominator', () => {
  assert.equal(cacheHitRate({ uncachedInputTokens: 200, cacheReadInputTokens: 600, cacheCreationInputTokens: 200 }), 0.6)
  assert.equal(cacheHitRate({ uncachedInputTokens: 0, cacheReadInputTokens: 0, cacheCreationInputTokens: 0 }), null)
})

test('cache hit rate is token weighted: sum tokens first, never average the rates', () => {
  const hot = { uncachedInputTokens: 10, cacheReadInputTokens: 90, cacheCreationInputTokens: 0 } // 90%
  const cold = { uncachedInputTokens: 900, cacheReadInputTokens: 0, cacheCreationInputTokens: 100 } // 0%
  assert.equal(cacheHitRate(sumCacheTokens([hot, cold])), 90 / 1100)
})

test('cache hit rate levels follow the sub2api thresholds', () => {
  assert.equal(cacheHitRateLevel(null), 'none')
  assert.equal(cacheHitRateLevel(CACHE_HIT_RATE_WARNING), 'ok')
  assert.equal(cacheHitRateLevel(0.7), 'warning')
  assert.equal(cacheHitRateLevel(CACHE_HIT_RATE_CRITICAL - 0.01), 'critical')
})

test('local accounts: in use first, disabled or credit-blocked last', () => {
  const base = { disabled: false, inFlightRequests: 0, cooldownRemainingSecs: 0 }
  assert.equal(credentialUsageTier({ ...base, inFlightRequests: 3 }, true), 'active')
  assert.equal(credentialUsageTier(base, true), 'ready')
  assert.equal(credentialUsageTier({ ...base, cooldownRemainingSecs: 20 }, true), 'cooling')
  assert.equal(credentialUsageTier({ ...base, inFlightRequests: 2, cooldownRemainingSecs: 20 }, true), 'active')
  assert.equal(credentialUsageTier({ ...base, disabled: true, inFlightRequests: 1 }, true), 'unavailable')
  assert.equal(credentialUsageTier(base, false), 'unavailable')
})

test('tier order is stable and ignores the in-flight count within a tier', () => {
  const tiers = ['unavailable', 'cooling', 'active', 'ready'] as const
  assert.deepEqual([...tiers].sort(compareUsageTier), ['active', 'ready', 'cooling', 'unavailable'])
  const base = { disabled: false, cooldownRemainingSecs: 0 }
  assert.equal(
    compareUsageTier(
      credentialUsageTier({ ...base, inFlightRequests: 1 }, true),
      credentialUsageTier({ ...base, inFlightRequests: 9 }, true),
    ),
    0,
  )
})

test('external pools: deleted, disabled or auto-disabled pools are unavailable', () => {
  const pool = { enabled: true, autoDisabled: false }
  assert.equal(externalPoolUsageTier(pool, { inFlight: 2, cooldownRemainingSecs: 0 }), 'active')
  assert.equal(externalPoolUsageTier(pool, undefined), 'ready')
  assert.equal(externalPoolUsageTier(pool, { inFlight: 0, cooldownRemainingSecs: 5 }), 'cooling')
  assert.equal(externalPoolUsageTier({ ...pool, autoDisabled: true }, { inFlight: 1, cooldownRemainingSecs: 0 }), 'unavailable')
  assert.equal(externalPoolUsageTier(undefined, undefined), 'unavailable')
})
