import type { RouteConfig, UsageOverviewRouteRow, UsageRouteLatencyRow } from '@/types/api'
import { cacheHitRate } from './cache-hit-rate.ts'
import { displayRoutePrefix, sortRoutes } from '../routes/route-utils.ts'

/** 后端用量里无法解析出路由前缀的入口。 */
export const UNKNOWN_ROUTE_KEY = 'unknown'

/** 用量与筛选里的路由键：默认路由为 "/"，其它为归一化前缀（与后端口径一致）。 */
export function routeUsageKey(prefix: string): string {
  return displayRoutePrefix(prefix)
}

export type RouteRowStatus = 'enabled' | 'disabled' | 'deleted' | 'unknown'

export type RouteOverviewRow = {
  /** 路由键："/"、"/cc"、"/dfcache/x"，或 `unknown` */
  key: string
  /** 已配置的路由；用量里出现但配置里已不存在时为空 */
  route?: RouteConfig
  /** 显示名：路由配置的名称，找不到时为前缀 */
  name: string
  status: RouteRowStatus
  usage?: UsageOverviewRouteRow
  latency?: UsageRouteLatencyRow
}

function routeName(route: RouteConfig): string {
  const name = route.name.trim()
  return name || routeUsageKey(route.prefix)
}

/**
 * 路由显示名解析：配置里的名称与前缀不同时写成"名称 前缀"，相同或找不到时只写前缀。
 * 图例和统计卡都用它，保证同一个路由在页面上只有一种叫法。
 */
export function routeLabelResolver(configs: RouteConfig[]): (routeKey: string) => string {
  const byKey = new Map(configs.map((route) => [routeUsageKey(route.prefix), route]))
  return (routeKey) => {
    if (routeKey === UNKNOWN_ROUTE_KEY) return '未识别入口'
    const route = byKey.get(routeKey)
    if (!route) return routeKey
    const name = routeName(route)
    return name === routeKey ? routeKey : `${name} ${routeKey}`
  }
}

/**
 * 路由表的行：已配置路由（含没有流量的）+ 用量里出现但配置里已不存在的前缀。
 * 有流量的排前面，按可计费、请求数降序；没有流量的按路由页同一顺序（内置在前）。
 */
export function buildRouteOverviewRows(
  configs: RouteConfig[],
  usage: UsageOverviewRouteRow[],
  latency: UsageRouteLatencyRow[] = [],
): RouteOverviewRow[] {
  const usageByKey = new Map(usage.map((item) => [item.route, item]))
  const latencyByKey = new Map(latency.map((item) => [item.route, item]))
  const ordered = sortRoutes(configs)
  const configOrder = new Map(ordered.map((route, index) => [routeUsageKey(route.prefix), index]))
  const configuredKeys = new Set(configOrder.keys())
  const rows: RouteOverviewRow[] = [
    ...ordered.map((route) => {
      const key = routeUsageKey(route.prefix)
      return {
        key,
        route,
        name: routeName(route),
        status: (route.enabled ? 'enabled' : 'disabled') as RouteRowStatus,
        usage: usageByKey.get(key),
        latency: latencyByKey.get(key),
      }
    }),
    ...usage
      .filter((item) => !configuredKeys.has(item.route))
      .map((item) => ({
        key: item.route,
        route: undefined,
        name: item.route === UNKNOWN_ROUTE_KEY ? '未识别入口' : item.route,
        status: (item.route === UNKNOWN_ROUTE_KEY ? 'unknown' : 'deleted') as RouteRowStatus,
        usage: item,
        latency: latencyByKey.get(item.route),
      })),
  ]
  const order = (row: RouteOverviewRow) => configOrder.get(row.key) ?? Number.MAX_SAFE_INTEGER
  return rows.sort((left, right) => {
    const leftRequests = left.usage?.metrics.requests ?? 0
    const rightRequests = right.usage?.metrics.requests ?? 0
    const leftActive = leftRequests > 0 ? 0 : 1
    const rightActive = rightRequests > 0 ? 0 : 1
    return (
      leftActive - rightActive ||
      (right.usage?.billableCostUsd ?? 0) - (left.usage?.billableCostUsd ?? 0) ||
      rightRequests - leftRequests ||
      order(left) - order(right) ||
      left.key.localeCompare(right.key)
    )
  })
}

export type RouteInsights = {
  /** 当前范围内有请求的路由数（含已删除前缀） */
  activeRoutes: number
  /** 已配置且启用的路由数 */
  enabledRoutes: number
  configuredRoutes: number
  /** 有流量、但配置里已不存在的前缀数 */
  unconfiguredActiveRoutes: number
  topBillable?: RouteOverviewRow
  lowestHitRate?: { row: RouteOverviewRow; rate: number }
  highestErrorRate?: { row: RouteOverviewRow; rate: number }
}

/** 统计卡用的路由洞察：只看有流量的路由。 */
export function routeInsights(rows: RouteOverviewRow[]): RouteInsights {
  const active = rows.filter((row) => (row.usage?.metrics.requests ?? 0) > 0)
  let topBillable: RouteOverviewRow | undefined
  let lowestHitRate: RouteInsights['lowestHitRate']
  let highestErrorRate: RouteInsights['highestErrorRate']
  for (const row of active) {
    const usage = row.usage!
    if (!topBillable || usage.billableCostUsd > (topBillable.usage?.billableCostUsd ?? 0)) topBillable = row
    const rate = cacheHitRate(usage.metrics)
    if (rate != null && (!lowestHitRate || rate < lowestHitRate.rate)) lowestHitRate = { row, rate }
    const errorRate = usage.metrics.requests > 0 ? usage.metrics.errorRequests / usage.metrics.requests : 0
    if (errorRate > 0 && (!highestErrorRate || errorRate > highestErrorRate.rate)) highestErrorRate = { row, rate: errorRate }
  }
  return {
    activeRoutes: active.length,
    enabledRoutes: rows.filter((row) => row.status === 'enabled').length,
    configuredRoutes: rows.filter((row) => row.route).length,
    unconfiguredActiveRoutes: active.filter((row) => !row.route).length,
    topBillable: topBillable && (topBillable.usage?.billableCostUsd ?? 0) > 0 ? topBillable : undefined,
    lowestHitRate,
    highestErrorRate,
  }
}
