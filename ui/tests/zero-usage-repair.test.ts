// Run with: node --test ui/tests/*.test.ts   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  ZERO_USAGE_SIDE_DEFAULTS,
  mergeZeroUsageRepair,
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

test('校验与后端一致：下限至少 1、上限不小于下限且不超过 1000000、比例大于 0 且不超过 10000%', () => {
  assert.equal(zeroUsageRepairError(undefined), null)
  assert.equal(zeroUsageRepairError({ output: { fallback: 'range', minTokens: 3, maxTokens: 9 } }), null)
  assert.equal(zeroUsageRepairError({ output: { minTokens: 1, maxTokens: 1_000_000, ratioPercent: 10_000 } }), null)
  assert.match(zeroUsageSideError({ ...ZERO_USAGE_SIDE_DEFAULTS, minTokens: 0 }, '输出')!, /输出范围下限/)
  assert.match(zeroUsageSideError({ ...ZERO_USAGE_SIDE_DEFAULTS, minTokens: 5, maxTokens: 4 }, '输出')!, /上限/)
  assert.match(zeroUsageSideError({ ...ZERO_USAGE_SIDE_DEFAULTS, maxTokens: 1_000_001 }, '输出')!, /上限/)
  assert.match(zeroUsageSideError({ ...ZERO_USAGE_SIDE_DEFAULTS, maxTokens: 1.5 }, '输入')!, /整数/)
  assert.match(zeroUsageSideError({ ...ZERO_USAGE_SIDE_DEFAULTS, ratioPercent: 0 }, '输入')!, /比例/)
  assert.match(zeroUsageRepairError({ input: { minTokens: 0 } })!, /^输入/)
  assert.match(zeroUsageRepairError({ output: { ratioPercent: 20_000 } })!, /^输出/)
})

test('模板与路由逐字段合并：路由写了的字段覆盖模板，null 和未写的沿用模板', () => {
  assert.equal(mergeZeroUsageRepair(undefined, undefined), undefined)
  assert.equal(mergeZeroUsageRepair(null, null), undefined)
  const template = {
    input: { enabled: false },
    output: { fallback: 'ratio' as const, ratioPercent: 2, maxTokens: 50 },
  }
  assert.deepEqual(mergeZeroUsageRepair(template, undefined), template)
  assert.deepEqual(mergeZeroUsageRepair(undefined, { output: { fallback: 'range' } }), { output: { fallback: 'range' } })
  assert.deepEqual(
    mergeZeroUsageRepair(template, { output: { fallback: 'range', ratioPercent: null, minTokens: 3 } }),
    {
      input: { enabled: false },
      output: { fallback: 'range', ratioPercent: 2, maxTokens: 50, minTokens: 3 },
    },
  )
  // 合并不改动传入的模板。
  assert.deepEqual(template.output, { fallback: 'ratio', ratioPercent: 2, maxTokens: 50 })
})
