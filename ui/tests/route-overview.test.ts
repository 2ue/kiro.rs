// Run with: node --test ui/tests/*.test.ts   (Node 22.18+ strips the TypeScript types natively)
import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  buildRouteOverviewRows,
  routeInsights,
  routeLabelResolver,
  routeUsageKey,
} from '../src/features/overview/route-overview.ts'

const route = (id: number, prefix: string, name: string, enabled = true) => ({
  id,
  prefix,
  name,
  enabled,
  cachePolicy: {},
  features: {},
  promptInjection: {},
  accountRule: { mode: 'allow_all' as const, rules: [] },
  modelRule: { mode: 'allow_all' as const, rules: [] },
})

const usage = (
  routeKey: string,
  values: { requests: number; errors?: number; billable?: number; uncached?: number; cacheRead?: number },
) => ({
  route: routeKey,
  endpoints: [`${routeKey === '/' ? '' : routeKey}/v1/messages`],
  metrics: {
    requests: values.requests,
    successRequests: values.requests - (values.errors ?? 0),
    errorRequests: values.errors ?? 0,
    errorRate: values.requests > 0 ? (values.errors ?? 0) / values.requests : 0,
    inputTokens: 0,
    uncachedInputTokens: values.uncached ?? 0,
    outputTokens: 0,
    cacheReadInputTokens: values.cacheRead ?? 0,
    cacheCreationInputTokens: 0,
    estimatedCostUsd: 0,
    originalCostUsd: 0,
    pricedRequests: values.requests,
    unpricedRequests: 0,
    averageDurationMs: 0,
  },
  rawCostUsd: 0,
  shapedCostUsd: 0,
  upliftedCostUsd: 0,
  reportedCostUsd: 0,
  billableCostUsd: values.billable ?? 0,
  profitUsd: 0,
  costFloorDeltaUsd: 0,
  costFloorAppliedRequests: 0,
  accountCostUsd: 0,
})

test('路由键与后端口径一致：默认路由为 "/"，其它为归一化前缀', () => {
  assert.equal(routeUsageKey(''), '/')
  assert.equal(routeUsageKey('/v1'), '/')
  assert.equal(routeUsageKey('cc/'), '/cc')
  assert.equal(routeUsageKey('/dfcache/team-a'), '/dfcache/team-a')
})

test('路由表行：已配置路由（含无流量）+ 配置里已删除但仍有流量的前缀，有流量的按可计费排前', () => {
  const configs = [route(1, '', 'default'), route(2, '/cc', '/cc'), route(3, '/ha', '/ha', false), route(4, '/dfcache/team', 'team')]
  const rows = buildRouteOverviewRows(
    configs,
    [
      usage('/cc', { requests: 10, billable: 1 }),
      usage('/', { requests: 5, billable: 3 }),
      usage('/dfcache/gone', { requests: 2, billable: 0.5 }),
      usage('unknown', { requests: 1 }),
    ],
    [{ route: '/cc', samples: 10, p50DurationMs: 100, p95DurationMs: 200, p99DurationMs: 300, firstTokenSamples: 0 }],
  )
  assert.deepEqual(rows.map((row) => [row.key, row.status]), [
    ['/', 'enabled'],
    ['/cc', 'enabled'],
    ['/dfcache/gone', 'deleted'],
    ['unknown', 'unknown'],
    // 无流量的已配置路由按路由页顺序：内置在前，停用的也列出来。
    ['/ha', 'disabled'],
    ['/dfcache/team', 'enabled'],
  ])
  assert.equal(rows.find((row) => row.key === '/cc')?.latency?.p95DurationMs, 200)
  assert.equal(rows.find((row) => row.key === '/dfcache/gone')?.route, undefined)
  assert.equal(rows.find((row) => row.key === '/')?.name, 'default')
})

test('统计卡只看有流量的路由：可计费最高、命中率最低、错误率最高', () => {
  const configs = [route(1, '', 'default'), route(2, '/cc', '/cc'), route(3, '/ha', '/ha', false)]
  const rows = buildRouteOverviewRows(configs, [
    usage('/', { requests: 10, billable: 3, uncached: 100, cacheRead: 900 }),
    usage('/cc', { requests: 4, errors: 2, billable: 1, uncached: 600, cacheRead: 400 }),
    usage('/dfcache/gone', { requests: 1, billable: 0.2 }),
  ])
  const insights = routeInsights(rows)
  assert.equal(insights.activeRoutes, 3)
  assert.equal(insights.enabledRoutes, 2)
  assert.equal(insights.configuredRoutes, 3)
  assert.equal(insights.unconfiguredActiveRoutes, 1)
  assert.equal(insights.topBillable?.key, '/')
  assert.equal(insights.lowestHitRate?.row.key, '/cc')
  assert.equal(insights.lowestHitRate?.rate, 0.4)
  assert.equal(insights.highestErrorRate?.row.key, '/cc')
  assert.equal(insights.highestErrorRate?.rate, 0.5)

  // 没有错误、没有输入、没有可计费时不硬凑一个路由出来。
  const quiet = routeInsights(buildRouteOverviewRows(configs, [usage('/', { requests: 3 })]))
  assert.equal(quiet.highestErrorRate, undefined)
  assert.equal(quiet.lowestHitRate, undefined)
  assert.equal(quiet.topBillable, undefined)
})

test('路由显示名：名称与前缀不同写成"名称 前缀"，找不到配置时用前缀', () => {
  const labelOf = routeLabelResolver([route(1, '', 'default'), route(2, '/cc', '/cc'), route(3, '/dfcache/team', ' ')])
  assert.equal(labelOf('/'), 'default /')
  assert.equal(labelOf('/cc'), '/cc')
  assert.equal(labelOf('/dfcache/team'), '/dfcache/team')
  assert.equal(labelOf('/dfcache/gone'), '/dfcache/gone')
  assert.equal(labelOf('unknown'), '未识别入口')
})

test('路由配置没加载成功时，配置里找不到的前缀不判成已删除', () => {
  const rows = buildRouteOverviewRows(
    [],
    [usage('/cc', { requests: 3 }), usage('unknown', { requests: 1 })],
    [],
    false,
  )
  assert.deepEqual(rows.map((row) => [row.key, row.status]), [
    ['/cc', 'unverified'],
    ['unknown', 'unknown'],
  ])
  const insights = routeInsights(rows)
  assert.equal(insights.unconfiguredActiveRoutes, 0)
  // 加载成功后同一前缀才是已删除。
  assert.equal(routeInsights(buildRouteOverviewRows([], [usage('/cc', { requests: 3 })])).unconfiguredActiveRoutes, 1)
})

test('命中率最低排除缓存策略为“无缓存”的路由，并记下排除了几个', () => {
  const noCache = { ...route(2, '/cc', '/cc'), cachePolicy: { cacheType: 'no_cache' as const } }
  const configs = [route(1, '', 'default'), noCache, route(3, '/ha', '/ha')]
  const rows = buildRouteOverviewRows(configs, [
    usage('/', { requests: 10, uncached: 100, cacheRead: 900 }),
    usage('/cc', { requests: 10, uncached: 1000, cacheRead: 0 }),
    usage('/ha', { requests: 10, uncached: 500, cacheRead: 500 }),
  ])
  const insights = routeInsights(rows)
  assert.equal(insights.lowestHitRate?.row.key, '/ha')
  assert.equal(insights.lowestHitRate?.rate, 0.5)
  assert.equal(insights.noCacheExcluded, 1)

  // 只剩无缓存路由有流量时没有可比较的路由。
  const only = routeInsights(buildRouteOverviewRows([noCache], [usage('/cc', { requests: 2, uncached: 10 })]))
  assert.equal(only.lowestHitRate, undefined)
  assert.equal(only.noCacheExcluded, 1)
})
