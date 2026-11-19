// Run with: node --test ui/tests/*.test.ts   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { atLeastInterval, HEAVY_QUERY_MIN_INTERVAL_MS } from '../src/lib/refetch-interval.ts'

test('扫明细的重查询自动刷新不快于 60 秒，关闭保持关闭', () => {
  assert.equal(HEAVY_QUERY_MIN_INTERVAL_MS, 60_000)
  assert.equal(atLeastInterval(10_000), 60_000)
  assert.equal(atLeastInterval(30_000), 60_000)
  assert.equal(atLeastInterval(120_000), 120_000)
  assert.equal(atLeastInterval(false), false)
  assert.equal(atLeastInterval(0), false)
  assert.equal(atLeastInterval(Number.NaN), false)
  assert.equal(atLeastInterval(5_000, 15_000), 15_000)
})
