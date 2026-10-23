// Run with: node --test ui/tests/*.test.ts   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { quotaBarModel } from '../src/features/credentials/credential-utils.ts'

const info = (currentUsage: number, usageLimit: number, currentOverages = 0) => ({ currentUsage, usageLimit, currentOverages })

test('within the limit the bar is a single segment with the usual tones', () => {
  assert.deepEqual(quotaBarModel(info(250, 1000)), {
    basePercent: 25, baseTone: 'success', inOverage: false, overageUsage: 0, baseWidth: 25, overageWidth: 0,
  })
  assert.equal(quotaBarModel(info(850, 1000)).baseTone, 'warning')
  assert.equal(quotaBarModel(info(960, 1000)).baseTone, 'error')
  // Exactly at the limit is full but not overage.
  const full = quotaBarModel(info(1000, 1000))
  assert.equal(full.inOverage, false)
  assert.equal(full.baseWidth, 100)
})

test('past the limit the base is full and the overage follows on the same track', () => {
  const model = quotaBarModel(info(1250, 1000))
  assert.equal(model.inOverage, true)
  assert.equal(model.basePercent, 100)
  assert.equal(model.overageUsage, 250)
  assert.equal(model.baseWidth, 80) // 1000 / 1250
  assert.equal(model.overageWidth, 20)
  assert.equal(model.baseWidth + model.overageWidth, 100)
})

test('large overage keeps the base segment visible', () => {
  const model = quotaBarModel(info(10_000, 1000))
  assert.equal(model.baseWidth, 40)
  assert.equal(model.overageWidth, 60)
})

test('upstream overage counter alone marks overage without inventing a size', () => {
  const model = quotaBarModel(info(1000, 1000, 0.12))
  assert.equal(model.inOverage, true)
  assert.equal(model.overageUsage, 0)
  assert.equal(model.baseWidth, 85)
  assert.equal(model.overageWidth, 15)
})

test('missing or invalid data renders an empty bar', () => {
  for (const value of [undefined, info(0, 0), info(Number.NaN, 1000), info(-5, 1000), info(500, 0, 3)]) {
    const model = quotaBarModel(value)
    assert.equal(model.inOverage, false)
    assert.equal(model.baseWidth, 0)
    assert.equal(model.overageWidth, 0)
  }
})
