import assert from 'node:assert/strict'
import { test } from 'node:test'
import { defaultPoolForm, poolFormFromPool } from '../src/features/external-pools/external-pool-utils.ts'
import type { ExternalPool } from '../src/types/api.ts'

test('外部账号原始用量模式默认兼容旧数据，编辑回显账号自己的选择', () => {
  assert.equal(defaultPoolForm().rawUsageSource, 'legacy_max')
  const legacy = { modelMappingRules: [], supportedModels: [], routeRules: [] } as unknown as ExternalPool
  assert.equal(poolFormFromPool(legacy).rawUsageSource, 'legacy_max')
  assert.equal(poolFormFromPool({ ...legacy, rawUsageSource: 'upstream_last' }).rawUsageSource, 'upstream_last')
})
