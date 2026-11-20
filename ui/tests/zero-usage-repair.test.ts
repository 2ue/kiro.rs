// Run with: node --test ui/tests/*.test.ts   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  ZERO_USAGE_SIDE_DEFAULTS,
  resolveZeroUsageSide,
  zeroUsageRepairError,
  zeroUsageSideError,
} from '../src/features/runtime/zero-usage-repair.ts'

test('未配置时取默认值：估算开、兜底保持估算、范围 1–20、比例 0.5%', () => {
  assert.deepEqual(resolveZeroUsageSide(undefined), ZERO_USAGE_SIDE_DEFAULTS)
  assert.deepEqual(resolveZeroUsageSide(null), {
    enabled: true,
    fallback: 'keep',
    minTokens: 1,
    maxTokens: 20,
    ratioPercent: 0.5,
  })
  assert.deepEqual(resolveZeroUsageSide({ enabled: false, fallback: null, maxTokens: 50 }), {
    ...ZERO_USAGE_SIDE_DEFAULTS,
    enabled: false,
    maxTokens: 50,
  })
})

test('校验与后端一致：下限至少 1、上限不小于下限、比例大于 0', () => {
  assert.equal(zeroUsageRepairError(undefined), null)
  assert.equal(zeroUsageRepairError({ output: { fallback: 'range', minTokens: 3, maxTokens: 9 } }), null)
  assert.match(zeroUsageSideError({ ...ZERO_USAGE_SIDE_DEFAULTS, minTokens: 0 }, '输出')!, /输出范围下限/)
  assert.match(zeroUsageSideError({ ...ZERO_USAGE_SIDE_DEFAULTS, minTokens: 5, maxTokens: 4 }, '输出')!, /上限/)
  assert.match(zeroUsageSideError({ ...ZERO_USAGE_SIDE_DEFAULTS, maxTokens: 1.5 }, '输入')!, /整数/)
  assert.match(zeroUsageSideError({ ...ZERO_USAGE_SIDE_DEFAULTS, ratioPercent: 0 }, '输入')!, /比例/)
  assert.match(zeroUsageRepairError({ input: { minTokens: 0 } })!, /^输入/)
  assert.match(zeroUsageRepairError({ output: { ratioPercent: 20_000 } })!, /^输出/)
})
