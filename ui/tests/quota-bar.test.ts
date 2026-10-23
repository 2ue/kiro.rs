// Run with: node --test ui/tests/*.test.ts   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { quotaBarModel } from '../src/features/credentials/credential-utils.ts'
import { formatQuotaFixed } from '../src/lib/format.ts'

const info = (currentUsage: number, usageLimit: number, currentOverages = 0) => ({ currentUsage, usageLimit, currentOverages })

test('within the limit the bar is a single segment with the usual tones', () => {
  assert.deepEqual(quotaBarModel(info(250, 1000)), {
    basePercent: 25, baseTone: 'success', inOverage: false, overage: 0, usagePercent: 25, baseWidth: 25, overageWidth: 0,
  })
  assert.equal(quotaBarModel(info(850, 1000)).baseTone, 'warning')
  assert.equal(quotaBarModel(info(960, 1000)).baseTone, 'error')
  // Exactly at the limit is full but not overage.
  const full = quotaBarModel(info(1000, 1000))
  assert.equal(full.inOverage, false)
  assert.equal(full.baseWidth, 100)
})

test('the overage segment grows with the actual overage', () => {
  const widths = [100, 250, 1000, 1500].map((extra) => quotaBarModel(info(1000 + extra, 1000)).overageWidth)
  for (let i = 1; i < widths.length; i += 1) assert.ok(widths[i] > widths[i - 1], `${widths}`)
  const model = quotaBarModel(info(1250, 1000))
  assert.equal(model.inOverage, true)
  assert.equal(model.basePercent, 100)
  assert.equal(model.overage, 250)
  assert.equal(model.baseWidth, 80) // 1000 / 1250
  assert.equal(model.overageWidth, 20)
})

test('both segments keep a minimum width at the extremes', () => {
  const huge = quotaBarModel(info(10_000, 1000))
  assert.equal(huge.baseWidth, 40)
  assert.equal(huge.overageWidth, 60)
  const tiny = quotaBarModel(info(1000.5, 1000))
  assert.equal(tiny.overageWidth, 4)
  assert.equal(tiny.baseWidth + tiny.overageWidth, 100)
})

test('currentOverages counts as overage credits when usage stops at the limit', () => {
  const model = quotaBarModel(info(1000, 1000, 250))
  assert.equal(model.inOverage, true)
  assert.equal(model.overage, 250)
  assert.equal(model.baseWidth, 80)
  // When both are reported the larger one wins, so overage is never counted twice.
  assert.equal(quotaBarModel(info(1300, 1000, 250)).overage, 300)
  assert.equal(quotaBarModel(info(1100, 1000, 250)).overage, 250)
})

test('missing or invalid data renders an empty bar', () => {
  for (const value of [undefined, info(0, 0), info(Number.NaN, 1000), info(-5, 1000), info(500, 0, 3)]) {
    const model = quotaBarModel(value)
    assert.equal(model.inOverage, false)
    assert.equal(model.baseWidth, 0)
    assert.equal(model.overageWidth, 0)
  }
})

test('usage percent passes 100 in overage', () => {
  assert.equal(quotaBarModel(info(250, 1000)).usagePercent, 25)
  assert.equal(quotaBarModel(info(1250, 1000)).usagePercent, 125)
  assert.equal(quotaBarModel(info(1000, 1000, 500)).usagePercent, 150)
})

test('quota numbers keep at most 6 decimals without trailing zeros', () => {
  assert.equal(formatQuotaFixed(1000), '1,000')
  assert.equal(formatQuotaFixed(12.5), '12.5')
  assert.equal(formatQuotaFixed(0.000123), '0.000123')
  assert.equal(formatQuotaFixed(1.23456789), '1.234568')
  assert.equal(formatQuotaFixed(Number.NaN), '-')
})
