// 提示词注入的纯逻辑：文本状态、长度校验、批量默认值、路由矩阵与缓存前缀变化判断。
// 只有类型导入，`node --test` 可以直接加载（见 ui/tests/）。
import type {
  InjectionCatalogEntry,
  InjectionId,
  InjectionItemOverride,
  InjectionTextVariant,
  PromptInjectionConfig,
} from '@/types/api'
import { displayRoutePrefix } from '../routes/route-utils.ts'

type Texts = PromptInjectionConfig['texts']
type Defaults = Partial<Record<string, { enabled?: boolean }>>

/** 与后端 `chars().count()` 同一口径：按 Unicode 码点计数。 */
export function injectionCharCount(text: string): number {
  return Array.from(text).length
}

/** 后端实际注入的文本：改过且去掉首尾空白后不为空，否则用内置文本（与后端 `PromptInjectionConfig::text` 一致）。 */
export function effectiveInjectionText(texts: Texts, variant: Pick<InjectionTextVariant, 'key' | 'defaultText'>): string {
  const edited = texts[variant.key]?.trim()
  return edited ? edited : variant.defaultText
}

/** 保存后是否与内置文本不同（后端会删掉与内置文本相同或为空的改动）。 */
export function isInjectionTextEdited(texts: Texts, variant: Pick<InjectionTextVariant, 'key' | 'defaultText'>): boolean {
  return effectiveInjectionText(texts, variant) !== variant.defaultText
}

export interface InjectionTextLimits {
  maxTextChars: number
  maxTotalTextChars: number
}

export interface InjectionTextCheck {
  /** 所有可编辑文本合计字符数。 */
  total: number
  overTotal: boolean
  /** 超过单段上限的文本。 */
  overItems: Array<{ key: string; label: string; chars: number }>
}

export function checkInjectionTexts(texts: Texts, catalog: InjectionCatalogEntry[], limits: InjectionTextLimits): InjectionTextCheck {
  let total = 0
  const overItems: InjectionTextCheck['overItems'] = []
  for (const entry of catalog) {
    for (const variant of entry.texts) {
      const chars = injectionCharCount(effectiveInjectionText(texts, variant))
      total += chars
      if (chars > limits.maxTextChars) {
        overItems.push({ key: variant.key, label: entry.texts.length > 1 ? `${entry.name} · ${variant.label}` : entry.name, chars })
      }
    }
  }
  return { total, overTotal: total > limits.maxTotalTextChars, overItems }
}

/** 保存前的长度校验；通过返回 null，否则返回阻止保存的原因。 */
export function injectionTextError(texts: Texts, catalog: InjectionCatalogEntry[], limits: InjectionTextLimits): string | null {
  const check = checkInjectionTexts(texts, catalog, limits)
  if (check.overItems.length > 0) {
    const item = check.overItems[0]!
    return `${item.label}的文本 ${item.chars} 字符，超过单段上限 ${limits.maxTextChars}`
  }
  if (check.overTotal) return `可编辑文本合计 ${check.total} 字符，超过上限 ${limits.maxTotalTextChars}`
  return null
}

export type BulkDefaultMode = 'off' | 'fresh'

/** “全部默认值设为关”/“全部恢复新装默认”：只改全局默认，路由上的显式设置不变。 */
export function withAllInjectionDefaults(config: PromptInjectionConfig, catalog: InjectionCatalogEntry[], mode: BulkDefaultMode): PromptInjectionConfig {
  const defaults = { ...config.defaults }
  for (const entry of catalog) {
    defaults[entry.id] = { ...defaults[entry.id], enabled: mode === 'off' ? false : entry.defaultEnabled }
  }
  return { ...config, defaults }
}

/** 批量操作是否不会改变任何默认值（按钮据此禁用）。 */
export function injectionDefaultsAlready(config: PromptInjectionConfig, catalog: InjectionCatalogEntry[], mode: BulkDefaultMode): boolean {
  return catalog.every((entry) => (config.defaults[entry.id]?.enabled ?? false) === (mode === 'off' ? false : entry.defaultEnabled))
}

export interface InjectionRouteLike {
  id: number
  prefix: string
  name: string
  enabled: boolean
  promptInjection: Partial<Record<string, InjectionItemOverride>>
}

/** 开着也不会注入的原因：strict 兼容模式拦截，或文本为空。 */
export function injectionBlockReason(entry: InjectionCatalogEntry, texts: Texts, strict: boolean): string | null {
  if (strict && entry.strictBlocks) return 'anthropic-strict 模式下不注入'
  if (entry.texts.length > 0 && entry.texts.every((variant) => !effectiveInjectionText(texts, variant).trim())) {
    return '文本为空，开启也不注入'
  }
  return null
}

function routeEnabledValue(route: InjectionRouteLike, defaults: Defaults, id: InjectionId): { enabled: boolean; explicit: boolean } {
  const own = route.promptInjection[id]?.enabled
  if (own !== undefined) return { enabled: own, explicit: true }
  return { enabled: defaults[id]?.enabled ?? false, explicit: false }
}

export type InjectionMatrixState = 'on' | 'off' | 'blocked'

export interface InjectionMatrixCell {
  id: InjectionId
  state: InjectionMatrixState
  /** 路由上显式设置，而不是跟随默认。 */
  explicit: boolean
  /** state 为 blocked 时的原因。 */
  reason: string | null
}

export interface InjectionMatrixRow {
  id: number
  prefix: string
  name: string
  enabled: boolean
  cells: InjectionMatrixCell[]
}

/** 路由 × 注入项矩阵：每条路由上每一项实际是开、关还是开着但不生效。行顺序与传入的路由一致。 */
export function routeInjectionMatrix(
  routes: InjectionRouteLike[],
  catalog: InjectionCatalogEntry[],
  config: Pick<PromptInjectionConfig, 'defaults' | 'texts'>,
  strict: boolean,
): InjectionMatrixRow[] {
  return routes.map((route) => ({
    id: route.id,
    prefix: displayRoutePrefix(route.prefix),
    name: route.name,
    enabled: route.enabled,
    cells: catalog.map((entry) => {
      const value = routeEnabledValue(route, config.defaults, entry.id)
      const reason = value.enabled ? injectionBlockReason(entry, config.texts, strict) : null
      return {
        id: entry.id,
        state: reason ? 'blocked' : value.enabled ? 'on' : 'off',
        explicit: value.explicit,
        reason,
      }
    }),
  }))
}

export interface InjectionSnapshot {
  config: Pick<PromptInjectionConfig, 'defaults' | 'texts'>
  routes: InjectionRouteLike[]
}

export interface InjectionPrefixChange {
  id: InjectionId
  name: string
  /** 受影响路由的显示前缀。 */
  routes: string[]
}

function injectedText(entry: InjectionCatalogEntry, route: InjectionRouteLike, snapshot: InjectionSnapshot, strict: boolean): string | null {
  if (!routeEnabledValue(route, snapshot.config.defaults, entry.id).enabled) return null
  if (injectionBlockReason(entry, snapshot.config.texts, strict)) return null
  return entry.texts.map((variant) => effectiveInjectionText(snapshot.config.texts, variant)).join('\n')
}

/**
 * 保存后哪些注入项会改变已有路由写进 system 的内容。这几项都计入缓存前缀，内容一变，
 * 这些路由上已有会话的缓存前缀就会失效一次。按路由 id 对照前后状态；新出现的路由没有旧缓存，不计入。
 */
export function injectionPrefixChanges(
  catalog: InjectionCatalogEntry[],
  before: InjectionSnapshot,
  after: InjectionSnapshot,
  strict: boolean,
): InjectionPrefixChange[] {
  const previous = new Map(before.routes.map((route) => [route.id, route]))
  const changes: InjectionPrefixChange[] = []
  for (const entry of catalog) {
    const routes: string[] = []
    for (const route of after.routes) {
      const old = previous.get(route.id)
      if (!old) continue
      if (injectedText(entry, old, before, strict) !== injectedText(entry, route, after, strict)) {
        routes.push(displayRoutePrefix(route.prefix))
      }
    }
    if (routes.length > 0) changes.push({ id: entry.id, name: entry.name, routes })
  }
  return changes
}

/** 确认弹窗里的一句话：`任务质量（/cc、/dfcache/a）；身份声明（/）`。 */
export function describePrefixChanges(changes: InjectionPrefixChange[]): string {
  return changes.map((change) => `${change.name}（${change.routes.join('、')}）`).join('；')
}
