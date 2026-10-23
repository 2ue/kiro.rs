import type {
  InjectionCatalogEntry,
  InjectionId,
  InjectionItemOverride,
  PromptInjectionConfig,
  RouteFeatureOverride,
  RouteFeatureSet,
  RouteFeaturesConfig,
  RuntimeConfig,
} from '@/types/api'

/** Same prefix rule as the backend: exact match or followed by a `/` segment boundary. */
export function prefixMatches(prefix: string, path: string): boolean {
  if (prefix === '/') return true
  return path === prefix || (path.startsWith(prefix) && path.charAt(prefix.length) === '/')
}

export function canonicalPrefix(prefix: string): string {
  const trimmed = prefix.trim()
  if (!trimmed) return ''
  const withSlash = trimmed.startsWith('/') ? trimmed : `/${trimmed}`
  return withSlash.replace(/\/+$/, '') || '/'
}

export type SettingSource = { kind: 'default' } | { kind: 'route'; prefix: string }

export interface EffectiveItem {
  enabled: boolean
  externalPool: boolean
  source: SettingSource
}

/** Longest matching prefix that sets this field wins, otherwise the global default. */
export function effectiveItem(config: PromptInjectionConfig, route: string, id: InjectionId): EffectiveItem {
  const path = routeEndpoint(route)
  let enabled: { value: boolean; prefix: string } | null = null
  let externalPool: { value: boolean; prefix: string } | null = null
  for (const [prefix, items] of Object.entries(config.pathOverrides)) {
    const item = items[id]
    if (!item || !prefixMatches(prefix, path)) continue
    if (item.enabled !== undefined && (!enabled || prefix.length > enabled.prefix.length)) {
      enabled = { value: item.enabled, prefix }
    }
    if (item.externalPool !== undefined && (!externalPool || prefix.length > externalPool.prefix.length)) {
      externalPool = { value: item.externalPool, prefix }
    }
  }
  const fallback = config.defaults[id]
  return {
    enabled: enabled?.value ?? fallback?.enabled ?? false,
    externalPool: externalPool?.value ?? fallback?.externalPool ?? false,
    source: enabled ? { kind: 'route', prefix: enabled.prefix } : { kind: 'default' },
  }
}

/** The setting inherited by `route` if its own override were removed. */
export function inheritedItem(config: PromptInjectionConfig, route: string, id: InjectionId): EffectiveItem {
  const withoutRoute: PromptInjectionConfig = { ...config, pathOverrides: { ...config.pathOverrides } }
  const own = withoutRoute.pathOverrides[route]
  if (own?.[id]) {
    const rest = { ...own }
    delete rest[id]
    withoutRoute.pathOverrides[route] = rest
  }
  return effectiveItem(withoutRoute, route, id)
}

export function routeOverride(config: PromptInjectionConfig, route: string, id: InjectionId): InjectionItemOverride {
  return config.pathOverrides[route]?.[id] ?? {}
}

export function withRouteOverride(
  config: PromptInjectionConfig,
  route: string,
  id: InjectionId,
  next: InjectionItemOverride,
): PromptInjectionConfig {
  const items = { ...(config.pathOverrides[route] ?? {}) }
  if (next.enabled === undefined && next.externalPool === undefined) delete items[id]
  else items[id] = next
  const pathOverrides = { ...config.pathOverrides }
  if (Object.keys(items).length === 0) delete pathOverrides[route]
  else pathOverrides[route] = items
  return { ...config, pathOverrides }
}

/** Moves every override from one route key to another (rename) or drops it (`next = null`). */
export function moveRouteOverrides<T>(map: Record<string, T>, from: string | null, next: string | null, value: T | null): Record<string, T> {
  const result = { ...map }
  if (from) delete result[from]
  if (next) delete result[next]
  if (next && value && Object.keys(value as object).length > 0) result[next] = value
  return result
}

export function effectiveFeatures(config: RouteFeaturesConfig, route: string): RouteFeatureSet {
  const path = routeEndpoint(route)
  const matching = Object.entries(config.pathOverrides)
    .filter(([prefix]) => prefixMatches(prefix, path))
    .sort(([a], [b]) => a.length - b.length)
  const resolved = { ...config.defaults }
  for (const [, override] of matching) {
    for (const [key, value] of Object.entries(override) as Array<[keyof RouteFeatureSet, never]>) {
      if (value !== undefined) resolved[key] = value
    }
  }
  return resolved
}

export function inheritedFeatures(config: RouteFeaturesConfig, route: string): RouteFeatureSet {
  const pathOverrides = { ...config.pathOverrides }
  delete pathOverrides[route]
  return effectiveFeatures({ ...config, pathOverrides }, route)
}

export function routeFeatureOverride(config: RouteFeaturesConfig, route: string): RouteFeatureOverride {
  return config.pathOverrides[route] ?? {}
}

/** Why an item that is switched on still does not take effect on this route, if it doesn't. */
export function dependencyBlock(
  entry: InjectionCatalogEntry,
  config: RuntimeConfig,
  route: string,
): string | null {
  const effective = (id: InjectionId) => effectiveItem(config.promptInjection, route, id).enabled
  if (config.compatProfile === 'anthropic-strict' && entry.strictBlocks) {
    return entry.id === 'thinkingControls' || entry.id === 'thinkingOutputPolicy'
      ? 'anthropic-strict 模式下只在强制可见 thinking 时注入'
      : 'anthropic-strict 模式下不注入'
  }
  switch (entry.dependsOn) {
    case 'thinkingControls':
      return effective('thinkingControls') ? null : '依赖的 thinking 控制标签已关闭'
    case 'bodyConversion.toolChoiceSteering':
      return config.bodyConversion.toolChoiceSteering ? null : '运行配置里的结构化 tool_choice 处理已关闭'
    case 'routeFeatures.modelDrivenWebSearch':
      return effectiveFeatures(config.routeFeatures, route).modelDrivenWebSearch ? null : '这个路由没有开启 WebSearch 模型驱动'
    default:
      return null
  }
}

export const CATEGORY_LABEL: Record<InjectionCatalogEntry['category'], string> = {
  operator: '运营引导',
  protocol: '协议兼容',
  feature: '功能附带',
  recovery: '故障恢复',
}

export const BLOCK_REASON_LABEL: Record<string, string> = {
  off: '开关关闭',
  external_pool_unsupported: '外部池不支持',
  external_pool_off: '外部池开关关闭',
  strict: 'strict 模式',
}

export const THINKING_TRIGGER_LABEL: Record<RouteFeatureSet['thinkingTrigger'], string> = {
  real_request: '按真实请求',
  always: '总是开启',
  off: '不自动开启',
}

/** Route-matrix routes for a prompt-injection page: built-ins, registered and configured routes. */
export function knownRoutes(config: RuntimeConfig): string[] {
  const routes = new Set<string>(['/v1', '/cc', '/ha', '/na'])
  for (const route of config.definedCacheRoutes ?? []) routes.add(canonicalPrefix(route))
  for (const prefix of Object.keys(config.cachePolicy.pathOverrides ?? {})) routes.add(canonicalPrefix(prefix))
  for (const prefix of Object.keys(config.promptInjection.pathOverrides)) routes.add(canonicalPrefix(prefix))
  for (const prefix of Object.keys(config.routeFeatures.pathOverrides)) routes.add(canonicalPrefix(prefix))
  routes.delete('')
  routes.delete('/')
  const builtIn = ['/v1', '/cc', '/ha', '/na']
  return Array.from(routes).sort((a, b) => {
    const ai = builtIn.indexOf(a)
    const bi = builtIn.indexOf(b)
    if (ai !== -1 || bi !== -1) return (ai === -1 ? 99 : ai) - (bi === -1 ? 99 : bi)
    return a.localeCompare(b)
  })
}

/** The messages entry path a route prefix serves (`/v1` is mounted at `/v1/messages`). */
export function routeEndpoint(route: string): string {
  return route === '/v1' ? '/v1/messages' : `${route}/v1/messages`
}
