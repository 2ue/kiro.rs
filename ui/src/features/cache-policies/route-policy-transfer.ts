// 路由策略 JSON 的导出与导入解析。只有类型导入，`node --test` 可以直接加载（见 ui/tests/）。
import type {
  CacheRoutePolicyPatch,
  CacheTtlMode,
  InjectionId,
  InjectionItemOverride,
  PromptCacheStrategyType,
  ReportedUsagePathPolicy,
  RouteConfig,
  RouteFeatureOverride,
  RouteRule,
  RouteRuleMode,
  ThinkingTriggerMode,
} from '@/types/api'
import { displayRoutePrefix } from '../routes/route-utils.ts'

/** 本项目导出文件的标记，用来拒绝无关 JSON。 */
export const ROUTE_POLICY_FORMAT = 'claude-pool-route-policy'
export const ROUTE_POLICY_VERSION = 1
/** main（kiro-rs）导出文件的标记与它支持的最高版本；导入时按字段映射兼容。 */
export const MAIN_ROUTE_POLICY_FORMAT = 'kiro-rs-route-policy'
export const MAIN_ROUTE_POLICY_MAX_VERSION = 2
/** 路由策略文件只有几 KB，远超这个大小的内容不是路由策略。 */
export const ROUTE_POLICY_MAX_BYTES = 1024 * 1024

export type RouteInjectionOverrides = Partial<Record<InjectionId, InjectionItemOverride>>

/** 路由上可随策略导出的字段：不含 id、前缀和启用状态。 */
export interface RoutePolicy {
  cachePolicy: CacheRoutePolicyPatch
  /** 路由级下游 usage 整形；null 表示路由没有单独设置。 */
  reportedUsage: ReportedUsagePathPolicy | null
  /** 路由上显式设置的注入开关；没出现的项跟随全局默认。 */
  promptInjection: RouteInjectionOverrides
  /** 路由上显式设置的路由功能；对象型功能为 null 表示显式关闭。 */
  features: RouteFeatureOverride
  accountRule: RouteRule
  modelRule: RouteRule
}

export interface RoutePolicyFile extends RoutePolicy {
  format: typeof ROUTE_POLICY_FORMAT
  version: number
  exportedAt: string
  /** 仅供导入时展示来源，导入不会使用它作为前缀。 */
  sourceRoute: string
  /** 路由名称，导入时作为新路由名称的初始值。 */
  name: string
}

export type RoutePolicyImport =
  | {
      ok: true
      /** 文件来源：本项目导出，或 main（kiro-rs）导出。 */
      origin: 'claude-pool' | 'main'
      version: number
      sourceRoute: string | null
      name: string | null
      policy: RoutePolicy
      /** 文件里有、但本项目没有对应设置而被忽略的字段（Kiro 专属等）。 */
      ignored: string[]
      /** 导入后行为会和原实例不同、需要醒目提示的项。 */
      highlights: string[]
    }
  | { ok: false; error: string }

const CACHE_TYPES: PromptCacheStrategyType[] = [
  'no_cache',
  'current_high_cache',
  'prefix_tool',
  'stable_segment_cache',
  'prefix_tool_plus',
  'bounded_high_cache',
]
/** main 的策略名与本项目不同的映射。 */
const MAIN_CACHE_TYPES: Record<string, PromptCacheStrategyType> = {
  kiro_rs_tool: 'prefix_tool',
  kiro_rs_tool_plus: 'prefix_tool_plus',
}
/** cachePolicy 里的对象型字段；值的具体范围由后端在保存时校验。 */
const CACHE_OBJECT_KEYS = [
  'simulation',
  'creationControl',
  'reportedUsage',
  'bounds',
  'prefixTool',
  'prefixToolPlus',
  'stableSegment',
  'hitShaping',
  'coldRequestGuard',
  'zeroUsageRepair',
] as const
const MAIN_CACHE_KEYS: Record<string, (typeof CACHE_OBJECT_KEYS)[number]> = {
  kiroRsTool: 'prefixTool',
  kiroRsToolPlus: 'prefixToolPlus',
}
/** main 有、本项目没有的 cachePolicy 字段。 */
const MAIN_IGNORED_CACHE_KEYS: Record<string, string> = {
  cachePoint: 'Kiro cachePoint 注入',
}
const CACHE_TTL_MODES: CacheTtlMode[] = [
  'follow_request_default_5m',
  'follow_request_default_1h',
  'force_5m',
  'force_1h',
  'upstream_then_request_default_5m',
]

const INJECTION_IDS: InjectionId[] = ['taskQuality', 'languageConstraint', 'custom', 'identityPrompt']
/** main 独有的注入项：导入时忽略。 */
const MAIN_ONLY_INJECTION_IDS = [
  'languageTurnReminder',
  'identityTurnReminder',
  'chunkedWritePolicy',
  'chunkedWriteToolSuffix',
  'thinkingControls',
  'thinkingOutputPolicy',
  'toolChoicePrefix',
  'toolChoiceTurnReminder',
  'webSearchGuidance',
  'emptyTurnNudge',
]

const FEATURE_OBJECT_KEYS = ['missingMaxTokens', 'reasoningCompat', 'payloadGuard', 'payloadShaping', 'imageProcessing'] as const
const FEATURE_BOOL_KEYS = ['historySanitize', 'thinkingModelName', 'returnRequestModel'] as const
const THINKING_TRIGGERS: ThinkingTriggerMode[] = ['real_request', 'always', 'off']
/** main 独有的路由功能：导入时忽略。 */
const MAIN_ONLY_FEATURE_KEYS = ['channelLeakGuard', 'channelLeakGuardStream', 'modelDrivenWebSearch']

const RULE_MODES: RouteRuleMode[] = ['allow_all', 'allow_list', 'deny_list']

const FILE_KEYS = [
  'format',
  'version',
  'exportedAt',
  'sourceRoute',
  'name',
  'cachePolicy',
  'reportedUsage',
  'promptInjection',
  'features',
  'accountRule',
  'modelRule',
]
/** 路由自身的字段：不随策略导入，出现时忽略并列出。 */
const ROUTE_IDENTITY_KEYS = ['id', 'prefix', 'enabled']
const MAIN_FILE_KEYS = [
  'format',
  'version',
  'exportedAt',
  'sourceRoute',
  'cachePolicy',
  'promptInjection',
  'features',
  'identity',
  'webSearch',
]

type Dialect = 'claude-pool' | 'main'

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function describe(value: unknown): string {
  const text = typeof value === 'string' ? `"${value}"` : JSON.stringify(value) ?? String(value)
  return text.length > 60 ? `${text.slice(0, 57)}...` : text
}

function clone<T>(value: T): T {
  return structuredClone(value)
}

/** 去掉空设置，只保留后端认识的 `enabled`。 */
function cleanInjection(input: RouteConfig['promptInjection'] | undefined): RouteInjectionOverrides {
  const result: RouteInjectionOverrides = {}
  for (const [id, item] of Object.entries(input ?? {})) {
    if (item?.enabled !== undefined) result[id as InjectionId] = { enabled: item.enabled }
  }
  return result
}

/** 路由卡片“导出 JSON”的内容：只含策略字段，不含 id 和前缀。 */
export function buildRoutePolicyFile(route: RouteConfig, now: Date = new Date()): RoutePolicyFile {
  return {
    format: ROUTE_POLICY_FORMAT,
    version: ROUTE_POLICY_VERSION,
    exportedAt: now.toISOString(),
    sourceRoute: displayRoutePrefix(route.prefix),
    name: route.name ?? '',
    cachePolicy: clone(route.cachePolicy ?? {}),
    reportedUsage: route.reportedUsage ? clone(route.reportedUsage) : null,
    promptInjection: cleanInjection(route.promptInjection),
    features: clone(route.features ?? {}),
    accountRule: clone(route.accountRule ?? { mode: 'allow_all', rules: [] }),
    modelRule: clone(route.modelRule ?? { mode: 'allow_all', rules: [] }),
  }
}

/** 导出文件名，例如 `/dfcache/team-a` -> `route-policy-dfcache-team-a.json`，根路由为 `root`。 */
export function routePolicyFileName(sourceRoute: string): string {
  const slug = sourceRoute.replace(/^\/+|\/+$/g, '').replace(/[^a-zA-Z0-9._-]+/g, '-') || 'root'
  return `route-policy-${slug}.json`
}

function parseCachePolicy(input: unknown, dialect: Dialect, ignored: string[]): CacheRoutePolicyPatch | string {
  if (!isPlainObject(input)) return 'cachePolicy 必须是对象'
  const policy: Record<string, unknown> = {}
  for (const [rawKey, value] of Object.entries(input)) {
    if (value === null || value === undefined) continue
    if (rawKey === 'cacheType') {
      const mapped = dialect === 'main' && typeof value === 'string' ? (MAIN_CACHE_TYPES[value] ?? value) : value
      if (typeof mapped !== 'string' || !CACHE_TYPES.includes(mapped as PromptCacheStrategyType)) {
        return `cachePolicy.cacheType 取值无效：${describe(value)}，可选 ${CACHE_TYPES.join('、')}`
      }
      policy.cacheType = mapped
      continue
    }
    if (rawKey === 'routeNamespace') {
      if (typeof value !== 'boolean') return 'cachePolicy.routeNamespace 必须是 true 或 false'
      policy.routeNamespace = value
      continue
    }
    if (rawKey === 'cacheTtl') {
      if (!isPlainObject(value)) return 'cachePolicy.cacheTtl 必须是对象'
      const ttl: Record<string, CacheTtlMode> = {}
      for (const [ttlKey, mode] of Object.entries(value)) {
        if (mode === null || mode === undefined) continue
        if (dialect === 'main' && ttlKey === 'local') {
          ignored.push('cachePolicy.cacheTtl.local（Kiro 本地账号的缓存写入时长）')
          continue
        }
        // main 的外部池写入时长就是本项目账号的写入时长。
        const target = dialect === 'main' && ttlKey === 'externalPool' ? 'account' : ttlKey
        if (target !== 'account') return `cachePolicy.cacheTtl 中有不认识的字段：${ttlKey}`
        if (!CACHE_TTL_MODES.includes(mode as CacheTtlMode)) {
          return `cachePolicy.cacheTtl.${ttlKey} 取值无效：${describe(mode)}，可选 ${CACHE_TTL_MODES.join('、')}`
        }
        ttl.account = mode as CacheTtlMode
      }
      if (Object.keys(ttl).length > 0) policy.cacheTtl = ttl
      continue
    }
    if (dialect === 'main' && MAIN_IGNORED_CACHE_KEYS[rawKey]) {
      ignored.push(`cachePolicy.${rawKey}（${MAIN_IGNORED_CACHE_KEYS[rawKey]}）`)
      continue
    }
    const key = dialect === 'main' ? (MAIN_CACHE_KEYS[rawKey] ?? rawKey) : rawKey
    if (!(CACHE_OBJECT_KEYS as readonly string[]).includes(key)) return `cachePolicy 中有不认识的字段：${rawKey}`
    if (!isPlainObject(value)) return `cachePolicy.${rawKey} 必须是对象`
    policy[key] = clone(value)
  }
  if (dialect === 'main' && policy.cacheType === undefined) policy.cacheType = 'no_cache'
  return policy as CacheRoutePolicyPatch
}

function parseInjection(input: unknown, dialect: Dialect, ignored: string[], highlights: string[]): RouteInjectionOverrides | string {
  if (input === undefined || input === null) return {}
  if (!isPlainObject(input)) return 'promptInjection 必须是对象'
  const result: RouteInjectionOverrides = {}
  for (const [id, item] of Object.entries(input)) {
    if (!INJECTION_IDS.includes(id as InjectionId)) {
      if (dialect === 'main' && MAIN_ONLY_INJECTION_IDS.includes(id)) {
        if (item !== null && item !== undefined) ignored.push(`promptInjection.${id}（Kiro 专属注入项）`)
        continue
      }
      return `promptInjection 中有不认识的注入项：${id}，可选 ${INJECTION_IDS.join('、')}`
    }
    if (item === null || item === undefined) continue
    if (!isPlainObject(item)) return `promptInjection.${id} 必须是对象`
    const parsed: InjectionItemOverride = {}
    let externalPool: unknown
    for (const [key, value] of Object.entries(item)) {
      if (key === 'enabled') {
        if (value === null || value === undefined) continue
        if (typeof value !== 'boolean') return `promptInjection.${id}.enabled 必须是 true 或 false`
        parsed.enabled = value
        continue
      }
      if (dialect === 'main' && key === 'externalPool') {
        externalPool = value
        if (value !== null && value !== undefined) ignored.push(`promptInjection.${id}.externalPool（外部池单独开关，本项目注入按路由生效）`)
        continue
      }
      return `promptInjection.${id} 中有不认识的字段：${key}`
    }
    // 与配置迁移命令同口径：保留 enabled，但 main 里只对本地账号生效的项要提示。
    if (parsed.enabled === true && externalPool === false) {
      highlights.push(`promptInjection.${id}：main 里只对本地账号生效（externalPool=false），导入后会对所有账号生效`)
    }
    if (parsed.enabled !== undefined) result[id as InjectionId] = parsed
  }
  return result
}

function parseFeatures(input: unknown, dialect: Dialect, ignored: string[]): RouteFeatureOverride | string {
  if (input === undefined || input === null) return {}
  if (!isPlainObject(input)) return 'features 必须是对象'
  const result: Record<string, unknown> = {}
  for (const [key, value] of Object.entries(input)) {
    if (dialect === 'main' && MAIN_ONLY_FEATURE_KEYS.includes(key)) {
      if (value !== null && value !== undefined) ignored.push(`features.${key}（Kiro 专属功能）`)
      continue
    }
    if (key === 'thinkingTrigger') {
      if (value === null || value === undefined) continue
      if (!THINKING_TRIGGERS.includes(value as ThinkingTriggerMode)) {
        return `features.thinkingTrigger 取值无效：${describe(value)}，可选 ${THINKING_TRIGGERS.join('、')}`
      }
      result.thinkingTrigger = value
      continue
    }
    // main 只有 thinkingModelName 一个布尔功能。
    const boolKeys: readonly string[] = dialect === 'main' ? ['thinkingModelName'] : FEATURE_BOOL_KEYS
    if (boolKeys.includes(key)) {
      if (value === null || value === undefined) continue
      if (typeof value !== 'boolean') return `features.${key} 必须是 true 或 false`
      result[key] = value
      continue
    }
    if (dialect === 'claude-pool' && (FEATURE_OBJECT_KEYS as readonly string[]).includes(key)) {
      if (value === undefined) continue
      // null 是“在这条路由上显式关闭”，与缺省（跟随默认）不同，必须保留。
      if (value !== null && !isPlainObject(value)) return `features.${key} 必须是对象或 null`
      result[key] = value === null ? null : clone(value)
      continue
    }
    return `features 中有不认识的字段：${key}`
  }
  return result as RouteFeatureOverride
}

function parseRule(input: unknown, label: 'accountRule' | 'modelRule'): RouteRule | string {
  if (input === undefined || input === null) return { mode: 'allow_all', rules: [] }
  if (!isPlainObject(input)) return `${label} 必须是对象`
  let mode: RouteRuleMode = 'allow_all'
  let rules: string[] = []
  for (const [key, value] of Object.entries(input)) {
    if (key === 'mode') {
      if (value === null || value === undefined) continue
      if (!RULE_MODES.includes(value as RouteRuleMode)) {
        return `${label}.mode 取值无效：${describe(value)}，可选 ${RULE_MODES.join('、')}`
      }
      mode = value as RouteRuleMode
    } else if (key === 'rules') {
      if (value === null || value === undefined) continue
      if (!Array.isArray(value) || value.some((rule) => typeof rule !== 'string')) return `${label}.rules 必须是字符串数组`
      rules = (value as string[]).map((rule) => rule.trim()).filter(Boolean)
    } else {
      return `${label} 中有不认识的字段：${key}`
    }
  }
  if (label === 'accountRule') {
    const bad = rules.find((rule) => rule !== '*' && !/^\d+$/.test(rule))
    if (bad !== undefined) return `accountRule.rules 只能是账号 ID 或 *：${describe(bad)}`
  }
  return { mode, rules }
}

function parseOptionalString(value: unknown, label: string): string | null | { error: string } {
  if (value === undefined || value === null) return null
  if (typeof value !== 'string') return { error: `${label} 必须是字符串` }
  return value.trim() || null
}

function parseClaudePoolFile(data: Record<string, unknown>): RoutePolicyImport {
  const ignored: string[] = []
  for (const key of Object.keys(data)) {
    if (ROUTE_IDENTITY_KEYS.includes(key)) {
      ignored.push(`${key}（路由本身不随策略导入，前缀需要重新填写）`)
      continue
    }
    if (!FILE_KEYS.includes(key)) return { ok: false, error: `不认识的字段：${key}` }
  }
  const version = data.version
  if (typeof version !== 'number' || !Number.isInteger(version) || version < 1 || version > ROUTE_POLICY_VERSION) {
    return { ok: false, error: `不支持的版本 ${describe(version)}，当前只支持版本 ${ROUTE_POLICY_VERSION}` }
  }
  const sourceRoute = parseOptionalString(data.sourceRoute, 'sourceRoute')
  if (isError(sourceRoute)) return { ok: false, error: sourceRoute.error }
  const name = parseOptionalString(data.name, 'name')
  if (isError(name)) return { ok: false, error: name.error }

  const cachePolicy = parseCachePolicy(data.cachePolicy, 'claude-pool', ignored)
  if (typeof cachePolicy === 'string') return { ok: false, error: cachePolicy }
  let reportedUsage: ReportedUsagePathPolicy | null = null
  if (data.reportedUsage !== undefined && data.reportedUsage !== null) {
    if (!isPlainObject(data.reportedUsage)) return { ok: false, error: 'reportedUsage 必须是对象或 null' }
    reportedUsage = clone(data.reportedUsage) as unknown as ReportedUsagePathPolicy
  }
  const promptInjection = parseInjection(data.promptInjection, 'claude-pool', ignored, [])
  if (typeof promptInjection === 'string') return { ok: false, error: promptInjection }
  const features = parseFeatures(data.features, 'claude-pool', ignored)
  if (typeof features === 'string') return { ok: false, error: features }
  const accountRule = parseRule(data.accountRule, 'accountRule')
  if (typeof accountRule === 'string') return { ok: false, error: accountRule }
  const modelRule = parseRule(data.modelRule, 'modelRule')
  if (typeof modelRule === 'string') return { ok: false, error: modelRule }

  return {
    ok: true,
    origin: 'claude-pool',
    version,
    sourceRoute,
    name,
    policy: { cachePolicy, reportedUsage, promptInjection, features, accountRule, modelRule },
    ignored,
    highlights: [],
  }
}

/** main 版本 1 的身份隐藏写在 identity 里；按 main 的规则拆成注入项和功能。 */
const LEGACY_IDENTITY_KEYS = [
  'enabled',
  'promptEnabled',
  'turnReminderEnabled',
  'outputGuardEnabled',
  'outputGuardStreamEnabled',
  'applyToExternalPool',
] as const

function convertLegacyIdentity(identity: Record<string, unknown>): { promptInjection: Record<string, unknown>; features: Record<string, unknown> } | string {
  const values: Partial<Record<(typeof LEGACY_IDENTITY_KEYS)[number], boolean>> = {}
  for (const [key, value] of Object.entries(identity)) {
    if (!LEGACY_IDENTITY_KEYS.includes(key as (typeof LEGACY_IDENTITY_KEYS)[number])) return `identity 中有不认识的字段：${key}`
    if (value === null || value === undefined) continue
    if (typeof value !== 'boolean') return `identity.${key} 必须是 true 或 false`
    values[key as (typeof LEGACY_IDENTITY_KEYS)[number]] = value
  }
  // 旧总开关管住所有身份子功能：显式关闭时全部关闭。
  const gate = values.enabled
  const combine = (sub: boolean | undefined) => (gate === false ? false : gate === true ? (sub ?? true) : sub)
  const promptInjection: Record<string, unknown> = {}
  const features: Record<string, unknown> = {}
  const prompt = combine(values.promptEnabled)
  if (prompt !== undefined || values.applyToExternalPool !== undefined) {
    promptInjection.identityPrompt = {
      ...(prompt === undefined ? {} : { enabled: prompt }),
      ...(values.applyToExternalPool === undefined ? {} : { externalPool: values.applyToExternalPool }),
    }
  }
  const reminder = combine(values.turnReminderEnabled)
  if (reminder !== undefined) promptInjection.identityTurnReminder = { enabled: reminder }
  const guard = combine(values.outputGuardEnabled)
  if (guard !== undefined) features.channelLeakGuard = guard
  if (values.outputGuardStreamEnabled !== undefined) features.channelLeakGuardStream = values.outputGuardStreamEnabled
  return { promptInjection, features }
}

function parseMainFile(data: Record<string, unknown>): RoutePolicyImport {
  const ignored: string[] = []
  for (const key of Object.keys(data)) {
    if (!MAIN_FILE_KEYS.includes(key)) return { ok: false, error: `不认识的字段：${key}` }
  }
  const version = data.version
  if (typeof version !== 'number' || !Number.isInteger(version) || version < 1 || version > MAIN_ROUTE_POLICY_MAX_VERSION) {
    return { ok: false, error: `不支持的 main 导出版本 ${describe(version)}，只支持版本 1–${MAIN_ROUTE_POLICY_MAX_VERSION}` }
  }
  const sourceRoute = parseOptionalString(data.sourceRoute, 'sourceRoute')
  if (isError(sourceRoute)) return { ok: false, error: sourceRoute.error }

  const cachePolicy = parseCachePolicy(data.cachePolicy, 'main', ignored)
  if (typeof cachePolicy === 'string') return { ok: false, error: cachePolicy }

  let injectionInput: unknown = data.promptInjection
  let featureInput: unknown = data.features
  if (version < 2) {
    const identity = data.identity ?? {}
    if (!isPlainObject(identity)) return { ok: false, error: 'identity 必须是对象' }
    const converted = convertLegacyIdentity(identity)
    if (typeof converted === 'string') return { ok: false, error: converted }
    if (data.webSearch !== undefined && data.webSearch !== null && typeof data.webSearch !== 'boolean') {
      return { ok: false, error: 'webSearch 必须是 true 或 false' }
    }
    injectionInput = converted.promptInjection
    featureInput = {
      ...converted.features,
      ...(typeof data.webSearch === 'boolean' ? { modelDrivenWebSearch: data.webSearch } : {}),
    }
  }
  const highlights: string[] = []
  const promptInjection = parseInjection(injectionInput, 'main', ignored, highlights)
  if (typeof promptInjection === 'string') return { ok: false, error: promptInjection }
  const features = parseFeatures(featureInput, 'main', ignored)
  if (typeof features === 'string') return { ok: false, error: features }

  return {
    ok: true,
    origin: 'main',
    version,
    sourceRoute,
    name: null,
    policy: {
      cachePolicy,
      reportedUsage: null,
      promptInjection,
      features,
      accountRule: { mode: 'allow_all', rules: [] },
      modelRule: { mode: 'allow_all', rules: [] },
    },
    ignored,
    highlights,
  }
}

function isError(value: unknown): value is { error: string } {
  return isPlainObject(value) && typeof value.error === 'string'
}

/**
 * 解析导入的路由策略。这里校验结构、枚举和开关类型；嵌套参数的数值范围由后端在新建路由时校验。
 * 文件里的路由（前缀）不会被导入。
 */
export function parseRoutePolicyImport(text: string): RoutePolicyImport {
  if (new TextEncoder().encode(text).length > ROUTE_POLICY_MAX_BYTES) {
    return { ok: false, error: '内容超过 1 MB，不是有效的路由策略文件' }
  }
  const trimmed = text.trim()
  if (!trimmed) return { ok: false, error: '请粘贴 JSON 或选择 JSON 文件' }
  let data: unknown
  try {
    data = JSON.parse(trimmed)
  } catch (error) {
    return { ok: false, error: `JSON 格式错误：${error instanceof Error ? error.message : String(error)}` }
  }
  if (!isPlainObject(data)) return { ok: false, error: 'JSON 顶层必须是对象' }
  if (data.format === ROUTE_POLICY_FORMAT) return parseClaudePoolFile(data)
  if (data.format === MAIN_ROUTE_POLICY_FORMAT) return parseMainFile(data)
  return {
    ok: false,
    error: `不是路由策略文件：缺少 "format": "${ROUTE_POLICY_FORMAT}"，请使用路由卡片上“导出 JSON”生成的文件`,
  }
}

/** 账号规则里在当前实例找不到的账号 ID（跨实例导入时提醒核对）。 */
export function missingRuleAccountIds(rule: RouteRule, accountIds: readonly number[]): string[] {
  if (rule.mode === 'allow_all') return []
  const known = new Set(accountIds.map(String))
  return rule.rules.filter((id) => id !== '*' && !known.has(id))
}

/** 以导入的策略作为新建路由的内容；前缀、名称由新建流程填写。 */
export function routeFromImportedPolicy(policy: RoutePolicy): RouteConfig {
  return {
    id: 0,
    prefix: '',
    name: '',
    enabled: true,
    cachePolicy: clone(policy.cachePolicy),
    ...(policy.reportedUsage ? { reportedUsage: clone(policy.reportedUsage) } : {}),
    features: clone(policy.features),
    promptInjection: clone(policy.promptInjection),
    accountRule: clone(policy.accountRule),
    modelRule: clone(policy.modelRule),
  }
}

/** 下载为格式化的 JSON 文件。 */
export function downloadRoutePolicyFile(file: RoutePolicyFile): void {
  const blob = new Blob([`${JSON.stringify(file, null, 2)}\n`], { type: 'application/json' })
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  a.download = routePolicyFileName(file.sourceRoute)
  document.body.appendChild(a)
  a.click()
  a.remove()
  URL.revokeObjectURL(url)
}
