// Run with: node --test ui/tests/*.test.ts   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  MAIN_ROUTE_POLICY_FORMAT,
  ROUTE_POLICY_FORMAT,
  ROUTE_POLICY_VERSION,
  buildRoutePolicyFile,
  missingRuleAccountIds,
  parseRoutePolicyImport,
  routeFromImportedPolicy,
  routePolicyFileName,
} from '../src/features/cache-policies/route-policy-transfer.ts'
import type { RouteConfig } from '../src/types/routes.ts'

const route = (patch: Partial<RouteConfig> = {}): RouteConfig => ({
  id: 7,
  prefix: '/dfcache/team-a',
  name: '团队 A',
  enabled: false,
  cachePolicy: { cacheType: 'prefix_tool', routeNamespace: true, prefixTool: { coverageRatio: 0.8 }, cacheTtl: { account: 'force_1h' } },
  reportedUsage: null,
  features: { thinkingTrigger: 'always', payloadGuard: null, historySanitize: true },
  promptInjection: { taskQuality: { enabled: true }, custom: {} },
  accountRule: { mode: 'allow_list', rules: ['1', '2'] },
  modelRule: { mode: 'deny_list', rules: ['claude-opus-*'] },
  ...patch,
})

const ok = (text: string) => {
  const result = parseRoutePolicyImport(text)
  if (!result.ok) assert.fail(`expected ok, got: ${result.error}`)
  return result
}
const error = (value: unknown) => {
  const result = parseRoutePolicyImport(typeof value === 'string' ? value : JSON.stringify(value))
  assert.equal(result.ok, false, 'expected an error')
  return (result as { error: string }).error
}
const file = (patch: Record<string, unknown> = {}) => ({
  format: ROUTE_POLICY_FORMAT,
  version: ROUTE_POLICY_VERSION,
  cachePolicy: { cacheType: 'current_high_cache' },
  ...patch,
})

test('导出只含策略字段和版本号，不含 id、前缀与启用状态', () => {
  const exported = buildRoutePolicyFile(route(), new Date('2026-01-02T03:04:05Z'))
  assert.equal(exported.format, ROUTE_POLICY_FORMAT)
  assert.equal(exported.version, ROUTE_POLICY_VERSION)
  assert.equal(exported.exportedAt, '2026-01-02T03:04:05.000Z')
  assert.equal(exported.sourceRoute, '/dfcache/team-a')
  for (const key of ['id', 'prefix', 'enabled']) assert.equal(key in exported, false, key)
  assert.equal(exported.reportedUsage, null)
  // 空设置不导出。
  assert.deepEqual(exported.promptInjection, { taskQuality: { enabled: true } })
  assert.deepEqual(exported.features, { thinkingTrigger: 'always', payloadGuard: null, historySanitize: true })
  assert.equal(buildRoutePolicyFile(route({ prefix: '' })).sourceRoute, '/')
})

test('导出再导入得到同样的策略，导入不带路由本身', () => {
  const source = route({ reportedUsage: { enabled: true } as RouteConfig['reportedUsage'] })
  const imported = ok(JSON.stringify(buildRoutePolicyFile(source)))
  assert.equal(imported.origin, 'claude-pool')
  assert.equal(imported.sourceRoute, '/dfcache/team-a')
  assert.equal(imported.name, '团队 A')
  assert.deepEqual(imported.ignored, [])
  assert.deepEqual(imported.policy, {
    cachePolicy: source.cachePolicy,
    reportedUsage: { enabled: true },
    promptInjection: { taskQuality: { enabled: true } },
    features: source.features,
    accountRule: source.accountRule,
    modelRule: source.modelRule,
  })
  const next = routeFromImportedPolicy(imported.policy)
  assert.equal(next.id, 0)
  assert.equal(next.prefix, '')
  assert.equal(next.enabled, true)
  assert.deepEqual(next.reportedUsage, { enabled: true })
  assert.equal('reportedUsage' in routeFromImportedPolicy({ ...imported.policy, reportedUsage: null }), false)
})

test('对象型路由功能的 null 是显式关闭，导入后保留', () => {
  const imported = ok(JSON.stringify(file({ features: { imageProcessing: null, payloadGuard: { enabled: true } } })))
  assert.deepEqual(imported.policy.features, { imageProcessing: null, payloadGuard: { enabled: true } })
})

test('无效 JSON、无关 JSON 与不支持的版本给出明确提示', () => {
  assert.match(error(''), /请粘贴 JSON/)
  assert.match(error('{ "format": '), /^JSON 格式错误/)
  assert.match(error('[1, 2]'), /顶层必须是对象/)
  assert.match(error({ hello: 1 }), /不是路由策略文件/)
  assert.match(error(file({ version: 2 })), /不支持的版本 2/)
  assert.match(error(file({ version: '1' })), /不支持的版本/)
  assert.match(error(`${JSON.stringify(file())}${' '.repeat(1024 * 1024)}`), /超过 1 MB/)
})

test('不认识的字段和非法取值逐项报错', () => {
  assert.match(error(file({ extra: true })), /不认识的字段：extra/)
  assert.match(error(file({ cachePolicy: 'x' })), /cachePolicy 必须是对象/)
  assert.match(error(file({ cachePolicy: { cacheType: 'kiro_rs_tool' } })), /cachePolicy\.cacheType 取值无效/)
  assert.match(error(file({ cachePolicy: { cachePoint: {} } })), /cachePolicy 中有不认识的字段：cachePoint/)
  assert.match(error(file({ cachePolicy: { routeNamespace: 'yes' } })), /routeNamespace 必须是 true 或 false/)
  assert.match(error(file({ cachePolicy: { simulation: 1 } })), /cachePolicy\.simulation 必须是对象/)
  assert.match(error(file({ cachePolicy: { cacheTtl: { local: 'force_5m' } } })), /cacheTtl 中有不认识的字段：local/)
  assert.match(error(file({ cachePolicy: { cacheTtl: { account: 'forever' } } })), /cacheTtl\.account 取值无效/)
  assert.match(error(file({ reportedUsage: [] })), /reportedUsage 必须是对象或 null/)
  assert.match(error(file({ promptInjection: { webSearchGuidance: { enabled: true } } })), /不认识的注入项：webSearchGuidance/)
  assert.match(error(file({ promptInjection: { custom: { enabled: 'on' } } })), /custom\.enabled 必须是 true 或 false/)
  assert.match(error(file({ promptInjection: { custom: { account: true } } })), /custom 中有不认识的字段：account/)
  assert.match(error(file({ features: { channelLeakGuard: true } })), /features 中有不认识的字段：channelLeakGuard/)
  assert.match(error(file({ features: { thinkingTrigger: 'sometimes' } })), /thinkingTrigger 取值无效/)
  assert.match(error(file({ features: { historySanitize: 1 } })), /historySanitize 必须是 true 或 false/)
  assert.match(error(file({ features: { payloadGuard: true } })), /payloadGuard 必须是对象或 null/)
  assert.match(error(file({ accountRule: { mode: 'some' } })), /accountRule\.mode 取值无效/)
  assert.match(error(file({ accountRule: { mode: 'allow_list', rules: ['1', 'abc'] } })), /只能是账号 ID 或 \*/)
  assert.match(error(file({ modelRule: { rules: 'claude-*' } })), /modelRule\.rules 必须是字符串数组/)
  assert.match(error(file({ modelRule: { mode: 'allow_list', list: [] } })), /modelRule 中有不认识的字段：list/)
  assert.match(error(file({ name: 1 })), /name 必须是字符串/)
})

test('文件里带 id、前缀或启用状态时忽略它们并列出，前缀不会被导入', () => {
  const imported = ok(JSON.stringify(file({ id: 3, prefix: '/cc', enabled: false })))
  assert.equal(imported.ignored.length, 3)
  assert.ok(imported.ignored.every((item) => item.includes('路由本身不随策略导入')))
  assert.equal(routeFromImportedPolicy(imported.policy).prefix, '')
})

test('缺省的规则视为全部允许，规则去掉空白项', () => {
  const imported = ok(JSON.stringify(file({ modelRule: { mode: 'allow_list', rules: [' claude-sonnet-4-5 ', ''] } })))
  assert.deepEqual(imported.policy.accountRule, { mode: 'allow_all', rules: [] })
  assert.deepEqual(imported.policy.modelRule, { mode: 'allow_list', rules: ['claude-sonnet-4-5'] })
})

test('兼容 main 版本 2：字段名映射，Kiro 专属字段忽略并列出', () => {
  const imported = ok(JSON.stringify({
    format: MAIN_ROUTE_POLICY_FORMAT,
    version: 2,
    exportedAt: '2026-01-01T00:00:00.000Z',
    sourceRoute: '/cc',
    cachePolicy: {
      cacheType: 'kiro_rs_tool_plus',
      routeNamespace: false,
      kiroRsTool: { coverageRatio: 0.5 },
      kiroRsToolPlus: { coverageRatio: 0.9 },
      cachePoint: { enabled: true },
      cacheTtl: { local: 'force_5m', externalPool: 'upstream_then_request_default_5m' },
      hitShaping: { enabled: true },
    },
    promptInjection: {
      identityPrompt: { enabled: true, externalPool: false },
      languageTurnReminder: { enabled: true },
      emptyTurnNudge: { enabled: false },
    },
    features: { thinkingModelName: true, thinkingTrigger: 'real_request', channelLeakGuard: true, modelDrivenWebSearch: false },
  }))
  assert.equal(imported.origin, 'main')
  assert.equal(imported.sourceRoute, '/cc')
  assert.equal(imported.name, null)
  assert.deepEqual(imported.policy.cachePolicy, {
    cacheType: 'prefix_tool_plus',
    routeNamespace: false,
    prefixTool: { coverageRatio: 0.5 },
    prefixToolPlus: { coverageRatio: 0.9 },
    cacheTtl: { account: 'upstream_then_request_default_5m' },
    hitShaping: { enabled: true },
  })
  assert.deepEqual(imported.policy.promptInjection, { identityPrompt: { enabled: true } })
  assert.deepEqual(imported.policy.features, { thinkingModelName: true, thinkingTrigger: 'real_request' })
  assert.equal(imported.highlights.length, 1)
  assert.match(imported.highlights[0], /promptInjection\.identityPrompt.*只对本地账号生效/)
  assert.equal(imported.policy.reportedUsage, null)
  assert.deepEqual(imported.policy.accountRule, { mode: 'allow_all', rules: [] })
  const ignored = imported.ignored.join('\n')
  for (const field of [
    'cachePolicy.cachePoint',
    'cachePolicy.cacheTtl.local',
    'promptInjection.identityPrompt.externalPool',
    'promptInjection.languageTurnReminder',
    'promptInjection.emptyTurnNudge',
    'features.channelLeakGuard',
    'features.modelDrivenWebSearch',
  ]) {
    assert.ok(ignored.includes(field), field)
  }
})

test('main 文件没写策略类型时按 main 的口径视为无缓存', () => {
  const imported = ok(JSON.stringify({ format: MAIN_ROUTE_POLICY_FORMAT, version: 2, cachePolicy: {} }))
  assert.equal(imported.policy.cachePolicy.cacheType, 'no_cache')
})

test('兼容 main 版本 1 的 identity 与 webSearch', () => {
  const imported = ok(JSON.stringify({
    format: MAIN_ROUTE_POLICY_FORMAT,
    version: 1,
    cachePolicy: { cacheType: 'current_high_cache' },
    identity: { enabled: true, outputGuardEnabled: false },
    webSearch: true,
  }))
  assert.deepEqual(imported.policy.promptInjection, { identityPrompt: { enabled: true } })
  assert.deepEqual(imported.policy.features, {})
  const ignored = imported.ignored.join('\n')
  assert.ok(ignored.includes('promptInjection.identityTurnReminder'))
  assert.ok(ignored.includes('features.channelLeakGuard'))
  assert.ok(ignored.includes('features.modelDrivenWebSearch'))
  // 旧总开关显式关闭时身份声明也关闭。
  const off = ok(JSON.stringify({ format: MAIN_ROUTE_POLICY_FORMAT, version: 1, cachePolicy: {}, identity: { enabled: false, promptEnabled: true } }))
  assert.deepEqual(off.policy.promptInjection, { identityPrompt: { enabled: false } })
})

test('main 文件的未知字段、未知注入项与过新的版本报错', () => {
  const main = (patch: Record<string, unknown>) => ({ format: MAIN_ROUTE_POLICY_FORMAT, version: 2, cachePolicy: {}, ...patch })
  assert.match(error(main({ version: 3 })), /不支持的 main 导出版本 3/)
  assert.match(error(main({ accountRule: {} })), /不认识的字段：accountRule/)
  assert.match(error(main({ promptInjection: { madeUp: { enabled: true } } })), /不认识的注入项：madeUp/)
  assert.match(error(main({ features: { historySanitize: true } })), /features 中有不认识的字段：historySanitize/)
  assert.match(error(main({ cachePolicy: { cacheType: 'unknown' } })), /cacheType 取值无效/)
  assert.match(error({ format: MAIN_ROUTE_POLICY_FORMAT, version: 1, cachePolicy: {}, identity: { extra: true } }), /identity 中有不认识的字段：extra/)
})

test('账号规则里当前实例没有的账号 ID', () => {
  assert.deepEqual(missingRuleAccountIds({ mode: 'allow_list', rules: ['1', '*', '9'] }, [1, 2]), ['9'])
  assert.deepEqual(missingRuleAccountIds({ mode: 'allow_all', rules: ['9'] }, []), [])
})

test('导出文件名', () => {
  assert.equal(routePolicyFileName('/dfcache/team-a'), 'route-policy-dfcache-team-a.json')
  assert.equal(routePolicyFileName('/'), 'route-policy-root.json')
})

test('零用量估算随路由策略导出导入，无缓存路由也保留', () => {
  const zeroUsageRepair = {
    input: { enabled: false },
    output: { fallback: 'ratio' as const, ratioPercent: 1.5, minTokens: 2, maxTokens: 80 },
  }
  for (const cacheType of ['current_high_cache', 'no_cache'] as const) {
    const source = route({ cachePolicy: { cacheType, zeroUsageRepair } })
    const imported = ok(JSON.stringify(buildRoutePolicyFile(source)))
    assert.deepEqual(imported.policy.cachePolicy.zeroUsageRepair, zeroUsageRepair, cacheType)
  }
  assert.match(error(file({ cachePolicy: { cacheType: 'no_cache', zeroUsageRepair: 'on' } })), /zeroUsageRepair/)
})

test('返回请求模型随路由功能导出导入，类型不对时报错', () => {
  const source = route({ features: { returnRequestModel: false } })
  const imported = ok(JSON.stringify(buildRoutePolicyFile(source)))
  assert.deepEqual(imported.policy.features, { returnRequestModel: false })
  assert.match(error(file({ features: { returnRequestModel: 'off' } })), /returnRequestModel/)
})
