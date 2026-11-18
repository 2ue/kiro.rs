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
  accountUsageTier,
} from '../src/features/overview/usage-order.ts'
import { accountCostDelta, costDeltaPercent, costMultiplier } from '../src/features/overview/overview-metrics.ts'

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

test('accounts: deleted, disabled or auto-disabled pools are unavailable', () => {
  const pool = { enabled: true, autoDisabled: false }
  assert.equal(accountUsageTier(pool, { inFlight: 2, cooldownRemainingSecs: 0 }), 'active')
  assert.equal(accountUsageTier(pool, undefined), 'ready')
  assert.equal(accountUsageTier(pool, { inFlight: 0, cooldownRemainingSecs: 5 }), 'cooling')
  assert.equal(accountUsageTier({ ...pool, autoDisabled: true }, { inFlight: 1, cooldownRemainingSecs: 0 }), 'unavailable')
  assert.equal(accountUsageTier(undefined, undefined), 'unavailable')
})

test('account cost columns: multiplier, delta share and the reported-vs-account delta', () => {
  assert.equal(costMultiplier(2, 1), '2.000')
  assert.equal(costMultiplier(0, 1), '-')
  assert.equal(costDeltaPercent(0.5, 2), '25.00%')
  assert.equal(costDeltaPercent(0.5, 0), '-')
  // 报告估值 - 账号成本：正值是毛利，负值亏损；账号成本为 0 时没有占比。
  assert.deepEqual(accountCostDelta(1.5, 1), { deltaUsd: 0.5, percentText: '50.0%', tone: 'profit' })
  assert.deepEqual(accountCostDelta(0.8, 1), { deltaUsd: -0.19999999999999996, percentText: '-20.0%', tone: 'loss' })
  assert.deepEqual(accountCostDelta(1, 0), { deltaUsd: 1, percentText: '-', tone: 'profit' })
})
