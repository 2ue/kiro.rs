import type {
  CacheRoutePolicyPatch,
  InjectionId,
  InjectionItemOverride,
  PromptCacheStrategyType,
  RouteFeatureOverride,
  ThinkingTriggerMode,
} from '@/types/api'

/** Marker of a route policy file, so unrelated JSON is rejected with a clear message. */
export const ROUTE_POLICY_FORMAT = 'kiro-rs-route-policy'
export const ROUTE_POLICY_VERSION = 2
/** Route policy files are a few KB; anything far larger is not a route policy. */
export const ROUTE_POLICY_MAX_BYTES = 1024 * 1024

export type RoutePolicyFile = {
  format: typeof ROUTE_POLICY_FORMAT
  version: number
  exportedAt: string
  /** Informational only; the route itself is never imported. */
  sourceRoute: string
  cachePolicy: CacheRoutePolicyPatch
  /** The route's explicit prompt-injection settings; items left out follow the defaults. */
  promptInjection: RouteInjectionOverrides
  /** The route's explicit feature settings. */
  features: RouteFeatureOverride
}

export type RouteInjectionOverrides = Partial<Record<InjectionId, InjectionItemOverride>>

export type RoutePolicyImport =
  | {
      ok: true
      sourceRoute: string | null
      cachePolicy: CacheRoutePolicyPatch
      promptInjection: RouteInjectionOverrides
      features: RouteFeatureOverride
    }
  | { ok: false; error: string }

const CACHE_TYPES: PromptCacheStrategyType[] = [
  'no_cache',
  'current_high_cache',
  'kiro_rs_tool',
  'stable_segment_cache',
  'kiro_rs_tool_plus',
  'bounded_high_cache',
]

/** Object-valued cache policy fields; any other key is dropped on import. */
const CACHE_OBJECT_KEYS: Array<keyof CacheRoutePolicyPatch> = [
  'simulation',
  'creationControl',
  'reportedUsage',
  'cachePoint',
  'bounds',
  'kiroRsTool',
  'kiroRsToolPlus',
  'stableSegment',
  'hitShaping',
  'coldRequestGuard',
]

const INJECTION_IDS: InjectionId[] = [
  'taskQuality',
  'languageConstraint',
  'languageTurnReminder',
  'custom',
  'identityPrompt',
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

const FEATURE_BOOL_KEYS = ['thinkingModelName', 'channelLeakGuard', 'channelLeakGuardStream', 'modelDrivenWebSearch'] as const
const THINKING_TRIGGERS: ThinkingTriggerMode[] = ['real_request', 'always', 'off']

/** Version 1 files carried identity masking and WebSearch in their own shapes. */
const LEGACY_IDENTITY_KEYS = [
  'enabled',
  'promptEnabled',
  'turnReminderEnabled',
  'outputGuardEnabled',
  'outputGuardStreamEnabled',
  'applyToExternalPool',
] as const

function convertLegacyIdentity(
  identity: Record<string, unknown>,
): { promptInjection: RouteInjectionOverrides; features: RouteFeatureOverride } | string {
  const values: Partial<Record<(typeof LEGACY_IDENTITY_KEYS)[number], boolean>> = {}
  for (const [key, value] of Object.entries(identity)) {
    if (!LEGACY_IDENTITY_KEYS.includes(key as (typeof LEGACY_IDENTITY_KEYS)[number])) {
      return `identity 中有未知字段：${key}`
    }
    if (value === null || value === undefined) continue
    if (typeof value !== 'boolean') return `identity.${key} 必须是 true 或 false`
    values[key as (typeof LEGACY_IDENTITY_KEYS)[number]] = value
  }
  // The old switch gated every identity sub-feature; an explicit "off" turns them all off.
  const gate = values.enabled
  const combine = (sub: boolean | undefined) => (gate === false ? false : gate === true ? (sub ?? true) : sub)
  const promptInjection: RouteInjectionOverrides = {}
  const features: RouteFeatureOverride = {}
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

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

export function buildRoutePolicyFile(
  sourceRoute: string,
  cachePolicy: CacheRoutePolicyPatch,
  promptInjection: RouteInjectionOverrides,
  features: RouteFeatureOverride,
  now: Date = new Date(),
): RoutePolicyFile {
  return {
    format: ROUTE_POLICY_FORMAT,
    version: ROUTE_POLICY_VERSION,
    exportedAt: now.toISOString(),
    sourceRoute,
    cachePolicy: structuredClone(cachePolicy),
    promptInjection: structuredClone(promptInjection),
    features: structuredClone(features),
  }
}

function parseInjection(input: unknown): RouteInjectionOverrides | string {
  if (input === undefined || input === null) return {}
  if (!isPlainObject(input)) return 'promptInjection 必须是对象'
  const result: RouteInjectionOverrides = {}
  for (const [id, item] of Object.entries(input)) {
    if (!INJECTION_IDS.includes(id as InjectionId)) return `promptInjection 中有未知注入项：${id}`
    if (!isPlainObject(item)) return `promptInjection.${id} 必须是对象`
    const parsed: InjectionItemOverride = {}
    for (const [key, value] of Object.entries(item)) {
      if (key !== 'enabled' && key !== 'externalPool') return `promptInjection.${id} 中有未知字段：${key}`
      if (value === null || value === undefined) continue
      if (typeof value !== 'boolean') return `promptInjection.${id}.${key} 必须是 true 或 false`
      parsed[key] = value
    }
    if (parsed.enabled !== undefined || parsed.externalPool !== undefined) result[id as InjectionId] = parsed
  }
  return result
}

function parseFeatures(input: unknown): RouteFeatureOverride | string {
  if (input === undefined || input === null) return {}
  if (!isPlainObject(input)) return 'features 必须是对象'
  const result: RouteFeatureOverride = {}
  for (const [key, value] of Object.entries(input)) {
    if (value === null || value === undefined) continue
    if (key === 'thinkingTrigger') {
      if (!THINKING_TRIGGERS.includes(value as ThinkingTriggerMode)) return `features.thinkingTrigger 取值无效：${String(value)}`
      result.thinkingTrigger = value as ThinkingTriggerMode
    } else if ((FEATURE_BOOL_KEYS as readonly string[]).includes(key)) {
      if (typeof value !== 'boolean') return `features.${key} 必须是 true 或 false`
      result[key as (typeof FEATURE_BOOL_KEYS)[number]] = value
    } else {
      return `features 中有未知字段：${key}`
    }
  }
  return result
}

/** File name for an exported route, e.g. `/dfcache/team-a` -> `route-policy-dfcache-team-a.json`. */
export function routePolicyFileName(sourceRoute: string): string {
  const slug = sourceRoute.replace(/^\/+|\/+$/g, '').replace(/[^a-zA-Z0-9._-]+/g, '-') || 'root'
  return `route-policy-${slug}.json`
}

/**
 * Parses an exported route policy. Only structure is checked here; field values are validated by
 * the backend when the route is saved. The route is not taken from the file.
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
  if (data.format !== ROUTE_POLICY_FORMAT) {
    return { ok: false, error: `不是路由策略文件：缺少 "format": "${ROUTE_POLICY_FORMAT}"，请使用“导出”生成的 JSON` }
  }
  if (typeof data.version !== 'number' || data.version > ROUTE_POLICY_VERSION) {
    return { ok: false, error: `不支持的版本 ${String(data.version)}，当前只支持版本 ${ROUTE_POLICY_VERSION}` }
  }

  const cachePolicy = data.cachePolicy
  if (!isPlainObject(cachePolicy)) return { ok: false, error: 'cachePolicy 必须是对象' }
  const cacheType = cachePolicy.cacheType ?? 'no_cache'
  if (typeof cacheType !== 'string' || !CACHE_TYPES.includes(cacheType as PromptCacheStrategyType)) {
    return { ok: false, error: `不支持的缓存策略类型：${String(cacheType)}` }
  }

  let promptInjection: RouteInjectionOverrides
  let features: RouteFeatureOverride
  if (data.version < 2) {
    const identityInput = data.identity ?? {}
    if (!isPlainObject(identityInput)) return { ok: false, error: 'identity 必须是对象' }
    const converted = convertLegacyIdentity(identityInput)
    if (typeof converted === 'string') return { ok: false, error: converted }
    if (data.webSearch !== undefined && data.webSearch !== null && typeof data.webSearch !== 'boolean') {
      return { ok: false, error: 'webSearch 必须是 true 或 false' }
    }
    promptInjection = converted.promptInjection
    features = {
      ...converted.features,
      ...(typeof data.webSearch === 'boolean' ? { modelDrivenWebSearch: data.webSearch } : {}),
    }
  } else {
    const parsedInjection = parseInjection(data.promptInjection)
    if (typeof parsedInjection === 'string') return { ok: false, error: parsedInjection }
    const parsedFeatures = parseFeatures(data.features)
    if (typeof parsedFeatures === 'string') return { ok: false, error: parsedFeatures }
    promptInjection = parsedInjection
    features = parsedFeatures
  }

  // Wrong-typed nested values would break the editor, so check each known field's shape here.
  const policy: Record<string, unknown> = { cacheType }
  if (cachePolicy.routeNamespace !== undefined && cachePolicy.routeNamespace !== null) {
    if (typeof cachePolicy.routeNamespace !== 'boolean') {
      return { ok: false, error: 'cachePolicy.routeNamespace 必须是 true 或 false' }
    }
    policy.routeNamespace = cachePolicy.routeNamespace
  }
  for (const key of CACHE_OBJECT_KEYS) {
    const value = cachePolicy[key]
    if (value === undefined || value === null) continue
    if (!isPlainObject(value)) return { ok: false, error: `cachePolicy.${key} 必须是对象` }
    policy[key] = value
  }

  return {
    ok: true,
    sourceRoute: typeof data.sourceRoute === 'string' && data.sourceRoute.trim() ? data.sourceRoute.trim() : null,
    cachePolicy: structuredClone(policy) as CacheRoutePolicyPatch,
    promptInjection,
    features,
  }
}

/** Downloads a route policy as a pretty-printed JSON file. */
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
