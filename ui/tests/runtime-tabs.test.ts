// Run with: node --test ui/tests/*.test.ts   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import { accountSectionAnchor, resolveRuntimeTab } from '../src/features/runtime/runtime-tabs.ts'
import {
  cacheUpliftEnabled,
  outputUpliftEnabled,
  withCacheUplift,
  withOutputUplift,
  type UsageCompensation,
} from '../src/features/runtime/usage-compensation.ts'
import { resolveRouteEditTab, routeEditorPath } from '../src/features/routes/route-utils.ts'

test('运行配置 ?tab=：页签名直达，账号池分组名打开账号池并定位', () => {
  assert.deepEqual(resolveRuntimeTab('proxy'), { tab: 'proxy', section: null })
  assert.deepEqual(resolveRuntimeTab('accounts'), { tab: 'accounts', section: null })
  assert.deepEqual(resolveRuntimeTab('cooldown'), { tab: 'accounts', section: 'cooldown' })
  assert.deepEqual(resolveRuntimeTab('uplift'), { tab: 'accounts', section: 'uplift' })
  for (const bad of [null, undefined, '', 'nope']) assert.deepEqual(resolveRuntimeTab(bad), { tab: 'admission', section: null })
  assert.equal(accountSectionAnchor('uplift'), 'runtime-accounts-uplift')
})

test('路由编辑页 ?tab=：认识的页签直达，main 的 behavior 打开注入页签，其它回到缓存', () => {
  assert.equal(resolveRouteEditTab('injection'), 'injection')
  assert.equal(resolveRouteEditTab(' Models '), 'models')
  assert.equal(resolveRouteEditTab('behavior'), 'injection')
  for (const bad of [null, '', 'cache', 'unknown']) assert.equal(resolveRouteEditTab(bad), 'cache')
  assert.equal(routeEditorPath(3), '/routes/3')
  assert.equal(routeEditorPath(3, 'cache'), '/routes/3')
  assert.equal(routeEditorPath(3, 'injection'), '/routes/3?tab=injection')
})

const comp = (patch: Partial<UsageCompensation> = {}): UsageCompensation => ({
  usageProjectionUpliftPercent: 0,
  usageProjectionCostFloorEnabled: true,
  usageProjectionCostFloorMarginPercent: 10,
  usageProjectionOutputUpliftMinTokens: 0,
  usageProjectionOutputUpliftPercent: 0,
  ...patch,
})

test('缓存补偿：打开即填 25%，已有值保留，关闭归 0', () => {
  assert.equal(cacheUpliftEnabled(comp()), false)
  assert.equal(withCacheUplift(comp(), true).usageProjectionUpliftPercent, 25)
  assert.equal(withCacheUplift(comp({ usageProjectionUpliftPercent: 40 }), true).usageProjectionUpliftPercent, 40)
  const off = withCacheUplift(comp({ usageProjectionUpliftPercent: 40 }), false)
  assert.equal(off.usageProjectionUpliftPercent, 0)
  assert.equal(cacheUpliftEnabled(off), false)
  // 不动其它字段。
  assert.equal(off.usageProjectionCostFloorMarginPercent, 10)
})

test('输出补偿：打开即填 1000 Token + 25%，两项都大于 0 才算开启，关闭两项归 0', () => {
  const on = withOutputUplift(comp(), true)
  assert.equal(on.usageProjectionOutputUpliftMinTokens, 1000)
  assert.equal(on.usageProjectionOutputUpliftPercent, 25)
  assert.equal(outputUpliftEnabled(on), true)
  assert.equal(outputUpliftEnabled(comp({ usageProjectionOutputUpliftMinTokens: 500 })), false)
  const kept = withOutputUplift(comp({ usageProjectionOutputUpliftMinTokens: 500 }), true)
  assert.deepEqual([kept.usageProjectionOutputUpliftMinTokens, kept.usageProjectionOutputUpliftPercent], [500, 25])
  const off = withOutputUplift(on, false)
  assert.deepEqual([off.usageProjectionOutputUpliftMinTokens, off.usageProjectionOutputUpliftPercent], [0, 0])
})
