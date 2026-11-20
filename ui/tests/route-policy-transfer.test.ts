// Run with: node --test ui/tests/*.test.ts   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  ROUTE_POLICY_FORMAT,
  ROUTE_POLICY_VERSION,
  buildRoutePolicyFile,
  parseRoutePolicyImport,
} from '../src/features/cache-policies/route-policy-transfer.ts'
import type { CacheRoutePolicyPatch } from '../src/types/api.ts'

const roundTrip = (cachePolicy: CacheRoutePolicyPatch) => {
  const parsed = parseRoutePolicyImport(JSON.stringify(buildRoutePolicyFile('/dfcache/team-a', cachePolicy, {}, {})))
  if (!parsed.ok) assert.fail(`expected ok, got: ${parsed.error}`)
  return parsed.cachePolicy
}

const importError = (cachePolicy: Record<string, unknown>) => {
  const parsed = parseRoutePolicyImport(JSON.stringify({
    format: ROUTE_POLICY_FORMAT,
    version: ROUTE_POLICY_VERSION,
    cachePolicy,
  }))
  assert.equal(parsed.ok, false, 'expected an error')
  return (parsed as { error: string }).error
}

test('零用量估算随路由策略导出导入，无缓存路由也保留', () => {
  const zeroUsageRepair = {
    input: { enabled: false },
    output: { fallback: 'ratio' as const, ratioPercent: 1.5, minTokens: 2, maxTokens: 80 },
  }
  for (const cacheType of ['current_high_cache', 'kiro_rs_tool_plus', 'no_cache'] as const) {
    assert.deepEqual(roundTrip({ cacheType, zeroUsageRepair }).zeroUsageRepair, zeroUsageRepair, cacheType)
  }
  assert.match(importError({ cacheType: 'no_cache', zeroUsageRepair: 'on' }), /zeroUsageRepair/)
})

test('缓存写入时长随路由策略导出导入，不再在导入时丢失', () => {
  const cacheTtl = { local: 'force_1h' as const, externalPool: 'upstream_then_request_default_5m' as const }
  assert.deepEqual(roundTrip({ cacheType: 'kiro_rs_tool', cacheTtl }).cacheTtl, cacheTtl)
  assert.match(importError({ cacheType: 'kiro_rs_tool', cacheTtl: ['force_1h'] }), /cacheTtl/)
})
