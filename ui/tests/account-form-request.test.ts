// Run with: node --test ui/tests/*.test.ts   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  accountFormFromAccount,
  buildAccountRequest,
  defaultAccountForm,
  diffAccountRequest,
  hasMaskedProxyCredentials,
  stableStringify,
  type AccountFormDraft,
} from '../src/features/accounts/account-utils.ts'
import type { Account } from '../src/types/api.ts'

const account = {
  id: 5,
  name: '主力',
  baseUrl: 'https://api.example.com',
  maskedApiKey: 'sk-ant...abcd',
  tags: ['vip', 'cn'],
  proxyUrl: 'http://***:***@1.2.3.4:8080/',
  costMultiplier: 1.2,
  overrides: { requestTimeoutSecs: 120, usageProjectionUpliftPercent: 30 },
  authType: 'x_api_key',
  headerProfile: 'claude_code_mimic',
  appendBetaQuery: true,
  headerOverrides: { 'x-a': '1', 'x-b': '2' },
  wireProfile: 'default',
  tlsProfile: 'default',
  enabled: true,
  priority: 10,
  maxConcurrentRequests: 8,
  usageProjectionMode: 'current_path_policy',
  streamResponseMode: null,
  autoDisablePolicy: 'inherit',
  preOutputStreamRetryMode: 'inherit',
  modelMappingMode: 'processed_mapping',
  modelMappingRequireMatch: false,
  modelMappingRules: [{ enabled: true, source: 'claude-sonnet-4.5', target: 'claude-sonnet-4-5', kind: 'alias' }],
  supportedModels: ['claude-sonnet-4-5', 'claude-opus-4-6'],
  routeMode: 'allow_all',
  routeRules: [],
  notes: '备注',
} as unknown as Account

const initial = accountFormFromAccount(account)
const diff = (patch: Partial<AccountFormDraft>) =>
  diffAccountRequest(buildAccountRequest(initial), buildAccountRequest({ ...structuredClone(initial), ...patch }))

test('没改任何字段时差量为空；密钥留空表示不改', () => {
  assert.deepEqual(diff({}), {})
  assert.deepEqual(diff({ apiKey: '' }), {})
})

test('只提交变化的字段', () => {
  assert.deepEqual(diff({ name: '新名字' }), { name: '新名字' })
  assert.deepEqual(diff({ apiKey: 'sk-new' }), { apiKey: 'sk-new' })
  assert.deepEqual(diff({ priority: 20, enabled: false }), { priority: 20, enabled: false })
  assert.deepEqual(diff({ supportedModelsText: 'claude-sonnet-4-5' }), { supportedModels: ['claude-sonnet-4-5'] })
  assert.deepEqual(diff({ tags: ['vip'] }), { tags: ['vip'] })
  assert.deepEqual(diff({ streamResponseMode: 'event_passthrough' }), { streamResponseMode: 'event_passthrough' })
})

test('打码的代理地址原样不动时不提交；改主机保留 *** 时提交（后端沿用原凭据）；清空提交 null', () => {
  assert.equal(hasMaskedProxyCredentials(initial.proxyUrl), true)
  assert.deepEqual(diff({ proxyUrl: `  ${initial.proxyUrl}  ` }), {})
  assert.deepEqual(diff({ proxyUrl: 'http://***:***@5.6.7.8:8080/' }), { proxyUrl: 'http://***:***@5.6.7.8:8080/' })
  assert.deepEqual(diff({ proxyUrl: '' }), { proxyUrl: null })
})

test('对象和数组用稳定 JSON 比较：键顺序不同、覆盖值清空后填回原值都不算改动', () => {
  assert.equal(stableStringify({ b: 1, a: [1, { d: 2, c: 3 }] }), stableStringify({ a: [1, { c: 3, d: 2 }], b: 1 }))
  assert.notEqual(stableStringify([1, 2]), stableStringify([2, 1]))
  assert.deepEqual(diff({ headerOverridesText: 'x-b: 2\nx-a: 1' }), {})
  assert.deepEqual(diff({ overrides: { usageProjectionUpliftPercent: 30, requestTimeoutSecs: 120, streamIdleTimeoutSecs: null } }), {})
  assert.deepEqual(diff({ overrides: { requestTimeoutSecs: 120, usageProjectionUpliftPercent: null } }), { overrides: { requestTimeoutSecs: 120 } })
  assert.deepEqual(diff({ modelMappingRulesText: 'claude-sonnet-4.5 -> claude-sonnet-4-5\n' }), {})
})

test('新建账号整份生成请求：空代理为 null，继承的 SSE 方式为 null，标签去空白去重', () => {
  const request = buildAccountRequest({ ...defaultAccountForm(), name: 'n', baseUrl: 'https://a.com', apiKey: 'sk', tags: [' a ', 'a', 'b'] })
  assert.equal(request.apiKey, 'sk')
  assert.equal(request.proxyUrl, null)
  assert.equal(request.streamResponseMode, null)
  assert.deepEqual(request.tags, ['a', 'b'])
  assert.deepEqual(request.overrides, {})
})

test('原始用量开关默认旧模式，编辑只提交当前账号的模式变化', () => {
  assert.equal(defaultAccountForm().rawUsageSource, 'legacy_max')
  const draft = accountFormFromAccount({ ...account, rawUsageSource: 'upstream_last' })
  assert.equal(buildAccountRequest(draft).rawUsageSource, 'upstream_last')
  const changes = diffAccountRequest(draft, { ...draft, rawUsageSource: 'legacy_max' })
  assert.deepEqual(changes, { rawUsageSource: 'legacy_max' })
})

test('最终记录最大计算模式只修改当前账号的原始口径', () => {
  const draft = accountFormFromAccount({ ...account, rawUsageSource: 'upstream_last_max_cost' })
  const request = buildAccountRequest(draft)
  assert.equal(request.rawUsageSource, 'upstream_last_max_cost')
  assert.deepEqual(diffAccountRequest({ ...request, rawUsageSource: 'legacy_max' }, request), { rawUsageSource: 'upstream_last_max_cost' })
})


test('模型映射预设只生成标准横杠 ID', async () => {
  const { DIRECT_MODEL_MAPPING_PRESETS, PROCESSED_MODEL_MAPPING_PRESETS } = await import('../src/features/accounts/account-utils.ts')
  for (const rules of [DIRECT_MODEL_MAPPING_PRESETS, PROCESSED_MODEL_MAPPING_PRESETS]) {
    assert.equal(new Set(rules.map((r) => r.source)).size, rules.length)
    assert.ok(rules.every((r) => !r.source.includes('.') && !r.target.includes('.')))
  }
})


test('测试模型选项不再把点号 ID 转成横杠', async () => {
  const { toAccountTestModelName } = await import('../src/lib/test-models.ts')
  assert.equal(toAccountTestModelName('claude-opus-4.6'), null)
  assert.equal(toAccountTestModelName('opus-4.6'), null)
  assert.equal(toAccountTestModelName('claude-sonnet-4-5-20250929'), 'claude-sonnet-4-5-20250929')
})
