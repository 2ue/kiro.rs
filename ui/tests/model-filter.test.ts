import assert from 'node:assert/strict'
import { test } from 'node:test'
import { defaultPoolForm, poolFormFromPool, poolSupportedModelsSummary } from '../src/features/external-pools/external-pool-utils.ts'
import type { ExternalPool } from '../src/types/api.ts'

test('模型筛选缺省白名单，黑名单编辑回显和摘要保留语义', () => {
  const legacy = { modelMappingRules: [], supportedModels: [], routeRules: [] } as unknown as ExternalPool
  assert.equal(defaultPoolForm().modelFilterMode, 'allowlist')
  assert.equal(poolFormFromPool(legacy).modelFilterMode, 'allowlist')
  const denied = { ...legacy, modelFilterMode: 'denylist', supportedModels: ['fable-5'] } as ExternalPool
  assert.equal(poolFormFromPool(denied).modelFilterMode, 'denylist')
  assert.equal(poolFormFromPool(denied).supportedModelsText, 'fable-5')
  assert.equal(poolSupportedModelsSummary(denied), '黑名单：禁止 fable-5')
  assert.equal(poolSupportedModelsSummary({ ...denied, supportedModels: [] }), '黑名单：不排除模型')
})
