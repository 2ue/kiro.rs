// Run with: node --test ui/tests/*.test.ts   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  accountUsageSummaryParts,
  accountUsageSummaryText,
  effectiveUsageCompensation,
} from '../src/features/accounts/account-usage.ts'

const config = {
  usageProjectionUpliftPercent: 25,
  usageProjectionCostFloorEnabled: true,
  usageProjectionCostFloorMarginPercent: 10,
  usageProjectionOutputUpliftMinTokens: 0,
  usageProjectionOutputUpliftPercent: 0,
  streamResponseMode: 'event_passthrough' as const,
}
const shaping = { usageProjectionMode: 'current_path_policy' as const, streamResponseMode: undefined, overrides: {} }

test('没有单账号覆盖时全部取全局值', () => {
  const effective = effectiveUsageCompensation({}, config)
  assert.deepEqual(effective.cacheUpliftPercent, { value: 25, source: 'global', global: 25 })
  assert.deepEqual(effective.costFloorEnabled, { value: true, source: 'global', global: true })
  assert.equal(effective.costFloorMarginPercent.value, 10)
})

test('单账号 overrides 优先，null 视为未覆盖（与后端 merge 一致）', () => {
  const effective = effectiveUsageCompensation(
    { usageProjectionUpliftPercent: 0, usageProjectionCostFloorEnabled: null, usageProjectionCostFloorMarginPercent: 30 },
    config,
  )
  assert.deepEqual(effective.cacheUpliftPercent, { value: 0, source: 'account', global: 25 })
  assert.equal(effective.costFloorEnabled.source, 'global')
  assert.deepEqual(effective.costFloorMarginPercent, { value: 30, source: 'account', global: 10 })
})

test('摘要显示生效值：账号覆盖的余量体现在成本底线上并标注单账号覆盖', () => {
  const parts = accountUsageSummaryParts({ ...shaping, overrides: { usageProjectionCostFloorMarginPercent: 30 } }, config)
  const floor = parts.find((part) => part.key === 'costFloor')!
  assert.equal(floor.text, '成本底线 +30%')
  assert.equal(floor.overridden, true)
  assert.match(floor.hint, /全局为 开启 \+10%/)
  assert.match(floor.hint, /编辑账号 → 覆盖全局参数/)
  const cache = parts.find((part) => part.key === 'cache')!
  assert.equal(cache.text, '缓存 +25%')
  assert.equal(cache.overridden, false)
  assert.match(cache.hint, /运行配置 → 账号池 → 用量补偿/)
})

test('单账号把补偿覆盖成关闭时仍显示出来，全局关闭的项不显示', () => {
  const parts = accountUsageSummaryParts(
    { ...shaping, overrides: { usageProjectionUpliftPercent: 0, usageProjectionCostFloorEnabled: false } },
    config,
  )
  assert.equal(parts.find((part) => part.key === 'cache')?.text, '缓存补偿：关')
  assert.equal(parts.find((part) => part.key === 'costFloor')?.text, '成本底线：关')
  assert.equal(parts.find((part) => part.key === 'output'), undefined)
})

test('单账号开启输出补偿：阈值与百分比都大于 0 才算开启', () => {
  const on = accountUsageSummaryParts({ ...shaping, overrides: { usageProjectionOutputUpliftMinTokens: 1000, usageProjectionOutputUpliftPercent: 20 } }, config)
  assert.equal(on.find((part) => part.key === 'output')?.text, '输出 ≥ 1000 后 +20%')
  const half = accountUsageSummaryParts({ ...shaping, overrides: { usageProjectionOutputUpliftPercent: 20 } }, config)
  assert.equal(half.find((part) => part.key === 'output')?.text, '输出补偿：关')
})

test('透传上游的账号不列补偿项；成本底线说明含兜底口径', () => {
  const passThrough = accountUsageSummaryParts({ usageProjectionMode: 'pass_through', overrides: { usageProjectionUpliftPercent: 50 } }, config)
  assert.deepEqual(passThrough.map((part) => part.key), ['usage', 'stream'])
  const floor = accountUsageSummaryParts(shaping, config).find((part) => part.key === 'costFloor')!
  assert.match(floor.hint, /不低于上游原始成本/)
  assert.match(floor.hint, /兜底/)
})

test('纯文本摘要带上“单账号覆盖”，流式单账号指定也算覆盖', () => {
  const parts = accountUsageSummaryParts(
    { usageProjectionMode: 'current_path_policy', streamResponseMode: 'event_passthrough', overrides: { usageProjectionUpliftPercent: 40 } },
    config,
  )
  assert.equal(
    accountUsageSummaryText(parts),
    'Usage：按路径整理 · 流式：事件透传（单账号覆盖） · 缓存 +40%（单账号覆盖） · 成本底线 +10%',
  )
})

test('悬停说明的全局位置直达运行配置对应分组：补偿项到用量补偿，流式到超时与流式', () => {
  const parts = accountUsageSummaryParts({ ...shaping, overrides: { usageProjectionOutputUpliftMinTokens: 1, usageProjectionOutputUpliftPercent: 1 } }, config)
  for (const key of ['cache', 'costFloor', 'output'] as const) {
    const item = parts.find((p) => p.key === key)!
    assert.deepEqual(item.global, { label: '运行配置 → 账号池 → 用量补偿', to: '/runtime?tab=uplift' })
    assert.equal(item.account, '编辑账号 → 覆盖全局参数')
  }
  assert.equal(parts.find((p) => p.key === 'stream')?.global?.to, '/runtime?tab=timeouts')
  // 透传上游的账号补偿不生效，不给用量补偿的全局链接
  const passThrough = accountUsageSummaryParts({ usageProjectionMode: 'pass_through', overrides: {} }, config)
  assert.equal(passThrough.find((p) => p.key === 'usage')?.global, undefined)
})
