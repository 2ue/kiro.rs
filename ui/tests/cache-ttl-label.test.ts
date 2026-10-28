// Run with: node --test ui/tests/*.test.ts   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { cacheWriteTtlMarker, cacheWriteTtlTitle } from '../src/features/usage/cache-ttl-label.ts'

const split = (total: number, fiveMinutes: number, oneHour: number) => ({
  cacheCreationInputTokens: total,
  cacheCreation5mInputTokens: fiveMinutes,
  cacheCreation1hInputTokens: oneHour,
})

test('a write that all went to the 1-hour cache is marked 1h', () => {
  assert.equal(cacheWriteTtlMarker(split(12_300, 0, 12_300)), '1h')
})

test('a mixed write names how much of it went to the 1-hour cache', () => {
  assert.equal(cacheWriteTtlMarker(split(12_300, 2_300, 10_000)), '1h 10K')
})

test('5-minute writes, cache reads and empty rows carry no marker', () => {
  assert.equal(cacheWriteTtlMarker(split(12_300, 12_300, 0)), undefined)
  assert.equal(cacheWriteTtlMarker(split(0, 0, 0)), undefined)
  // Older records may lack the split fields.
  assert.equal(cacheWriteTtlMarker({ cacheCreationInputTokens: 500 }), undefined)
})

test('the tooltip spells out the exact split, and is absent without a write', () => {
  assert.equal(cacheWriteTtlTitle(split(12_300, 2_300, 10_000)), '缓存写入 12,300：5 分钟 2,300 · 1 小时 10,000')
  assert.equal(cacheWriteTtlTitle(split(0, 0, 0)), undefined)
})
