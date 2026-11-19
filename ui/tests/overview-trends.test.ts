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
import {
  accountCostDelta,
  averagePerMinute,
  completedBuckets,
  costDeltaPercent,
  costMultiplier,
  latencyRangeWithinLimit,
  perMinute,
  weightedAverageDuration,
  zeroBelowCent,
} from '../src/features/overview/overview-metrics.ts'

test('average duration over buckets is weighted by requests, never averaged per bucket', () => {
  const buckets = [
    { requests: 10, averageDurationMs: 100 },
    { requests: 90, averageDurationMs: 1000 },
  ]
  assert.equal(weightedAverageDuration(buckets, (bucket) => bucket), 910)
  assert.equal(weightedAverageDuration([{ requests: 0, averageDurationMs: 5 }], (bucket) => bucket), null)
  // 老缓存里没有 averageDurationMs 时按 0 处理。
  assert.equal(weightedAverageDuration([{ requests: 4 }], (bucket) => bucket), 0)
})

test('RPM / TPM come from hour buckets divided by 60', () => {
  assert.equal(perMinute(600), 10)
  assert.equal(averagePerMinute([{ n: 600 }, { n: 1200 }], (bucket) => bucket.n), 15)
  assert.equal(averagePerMinute([], (bucket: { n: number }) => bucket.n), null)
})

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

test('差额不到半分按 0：不再显示红色的 -$0.00', () => {
  assert.equal(zeroBelowCent(-0.004), 0)
  assert.equal(zeroBelowCent(0.0049), 0)
  assert.equal(zeroBelowCent(-0.005), -0.005)
  assert.equal(zeroBelowCent(1.23), 1.23)
  assert.equal(zeroBelowCent(Number.NaN), 0)
  assert.deepEqual(accountCostDelta(1.001, 1.004), { deltaUsd: 0, percentText: '0.0%', tone: 'even' })
  assert.equal(accountCostDelta(1, 1.01).tone, 'loss')
})

test('平均 RPM / TPM 只用已结束的小时，进行中的那一小时不进分母', () => {
  const buckets = [{ n: 600 }, { n: 1200 }, { n: 60 }]
  // 第 3 个桶是进行中的小时（partialFrom = 2）。
  assert.equal(averagePerMinute(completedBuckets(buckets, 2), (bucket) => bucket.n), 15)
  assert.equal(completedBuckets(buckets, 3).length, 3)
  assert.equal(completedBuckets(buckets, 99).length, 3)
  assert.equal(completedBuckets(buckets, -1).length, 0)
  // 范围里只有进行中的小时：没有可平均的桶。
  assert.equal(averagePerMinute(completedBuckets(buckets.slice(0, 1), 0), (bucket) => bucket.n), null)
})

test('延迟分位的范围上限按后端口径先对齐整点再比较', () => {
  assert.equal(latencyRangeWithinLimit('2026-11-11T00:00:00Z', '2026-11-19T00:00:00Z'), true)
  // 恰好 8 天但不是整点：from 向下、to 向上对齐后是 8 天 1 小时，后端会拒绝。
  assert.equal(latencyRangeWithinLimit('2026-11-11T00:30:00Z', '2026-11-19T00:30:00Z'), false)
  assert.equal(latencyRangeWithinLimit('2026-11-11T00:30:00Z', '2026-11-18T23:30:00Z'), true)
  assert.equal(latencyRangeWithinLimit('2026-11-11T01:00:00+08:00', '2026-11-19T01:00:00+08:00'), true)
  assert.equal(latencyRangeWithinLimit('bad', '2026-11-19T00:00:00Z'), false)
})
