import { useEffect, useMemo, useState, type ReactNode } from 'react'
import { Check, Plus, Trash2 } from 'lucide-react'
import { Input, Select, SelectContent, SelectItem, SelectTrigger, SelectValue, Switch, Textarea, Button } from '@/components/ui'
import { defaultCacheHitShaping, defaultPromptCacheCreationControl, inputSamplePolicy, pathPolicy, preserveFieldPolicy, writerSamplePolicy } from '@/lib/runtime-config-defaults'
import type {
  ModelCapabilitiesStatus,
  CachePolicyConfig,
  CacheRoutePolicyPatch,
  CacheTtlMode,
  CacheTtlPolicyPatch,
  ModelMappingConfig,
  ModelMappingRule,
  PayloadShapingConfig,
  PromptCacheCreationControlConfig,
  ReportedUsageFieldPolicy,
  ReportedUsagePathPolicy,
  RuntimeConfig,
} from '@/types/api'

// ─── 共用原子 ─────────────────────────────────────────────────────────────────

function numberValue(value: string, fallback: number): number {
  const parsed = Number(value)
  return Number.isFinite(parsed) ? parsed : fallback
}

export function NumField({
  label, desc, value, min, max, step, suffix, disabled, onChange,
}: {
  label: string; desc?: string; value: number; min?: number; max?: number; step?: number
  suffix?: string; disabled?: boolean; onChange: (v: number) => void
}) {
  return (
    <div className="min-w-0 space-y-1.5">
      <label className="block text-[13px] font-medium leading-5 text-foreground">{label}</label>
      <div className="flex min-h-8 items-center gap-2">
        <Input type="number" className="h-8 w-24 shrink-0 px-2 text-sm tabular-nums" value={value} min={min} max={max} step={step}
          inputMode={step && step < 1 ? 'decimal' : 'numeric'} disabled={disabled}
          onChange={(e) => onChange(numberValue(e.target.value, min ?? 0))} />
        {suffix && <span className="shrink-0 whitespace-nowrap text-xs text-muted-foreground">{suffix}</span>}
      </div>
      {desc && <div className="max-w-[34rem] text-[11px] leading-4 text-muted-foreground">{desc}</div>}
    </div>
  )
}

export function TogField({
  label, desc, checked, disabled, onChange,
}: {
  label: string; desc?: string; checked: boolean; disabled?: boolean; onChange: (v: boolean) => void
}) {
  return (
    <div className="min-w-0 space-y-1.5">
      <div className="flex min-h-8 items-center justify-between gap-4">
        <div className="min-w-0 text-[13px] font-medium leading-5 text-foreground">{label}</div>
        <Switch checked={checked} disabled={disabled} onCheckedChange={onChange} className="shrink-0" />
      </div>
      {desc && <div className="text-xs leading-5 text-muted-foreground">{desc}</div>}
    </div>
  )
}

function TwoCol({ children }: { children: React.ReactNode }) {
  return <div className="grid gap-x-6 gap-y-5 md:grid-cols-2">{children}</div>
}

function FieldGrid({ children }: { children: React.ReactNode }) {
  return <div className="grid gap-x-8 gap-y-5 sm:grid-cols-2">{children}</div>
}

/**
 * 文档式的参数分组：只用标题、细线和留白表达层级，不再给每个子组套一层卡片。
 * 适合放在策略弹窗中，避免多个分组嵌套后所有内容看起来同级。
 */
function ParameterGroup({
  title,
  desc,
  children,
  className = '',
}: {
  title: string
  desc?: string
  children: ReactNode
  className?: string
}) {
  return (
    <section className={`space-y-5 border-t border-border/60 pt-5 first:border-t-0 first:pt-0 ${className}`}>
      <header className="flex max-w-3xl items-start gap-2.5">
        <span className="mt-2 size-1.5 shrink-0 rounded-full bg-foreground/35" aria-hidden="true" />
        <div>
          <h3 className="text-sm font-semibold leading-5 tracking-tight text-foreground">{title}</h3>
          {desc && <p className="mt-1 text-xs leading-5 text-muted-foreground">{desc}</p>}
        </div>
      </header>
      <div className="min-w-0 space-y-5">{children}</div>
    </section>
  )
}

function EditorSection({
  id,
  title,
  desc,
  className = '',
  children,
}: {
  id?: string
  title: string
  desc?: string
  className?: string
  children: ReactNode
}) {
  return (
    <section id={id} className={`min-w-0 scroll-mt-4 ${className}`}>
      <header className="max-w-3xl">
        <h3 className="text-base font-semibold leading-6 tracking-tight text-foreground">{title}</h3>
        {desc && <p className="mt-1.5 text-xs leading-5 text-muted-foreground">{desc}</p>}
      </header>
      <div className="mt-7 min-w-0">{children}</div>
    </section>
  )
}

// ─── 缓存策略与路径绑定(cachePolicy + legacy defaults) ───────────────────────

type CacheSimulationPatch = NonNullable<CacheRoutePolicyPatch['simulation']>
type KiroRsToolPatch = NonNullable<CacheRoutePolicyPatch['kiroRsTool']>
type KiroRsToolPlusPatch = NonNullable<CacheRoutePolicyPatch['kiroRsToolPlus']>
type UsageShapingProfilePatch = NonNullable<KiroRsToolPlusPatch['shaping']>
type UsageFieldShapingPatch = NonNullable<UsageShapingProfilePatch['input']>
type UsageShapingMode = NonNullable<NonNullable<KiroRsToolPlusPatch['nonStream']>['mode']>
type SuppressedCreationTarget = NonNullable<KiroRsToolPlusPatch['suppressedCreationTarget']>
type StableSegmentPatch = NonNullable<CacheRoutePolicyPatch['stableSegment']>
type CacheHitShapingPatch = NonNullable<CacheRoutePolicyPatch['hitShaping']>
type ColdRequestGuardPatch = NonNullable<CacheRoutePolicyPatch['coldRequestGuard']>
export type CacheStrategyType = NonNullable<CacheRoutePolicyPatch['cacheType']>

/** 走本地模拟缓存参数（模拟、写入频次、reportedUsage、hitShaping）的策略。 */
export function isHighCacheStrategy(cacheType: CacheStrategyType): cacheType is 'current_high_cache' | 'bounded_high_cache' {
  return cacheType === 'current_high_cache' || cacheType === 'bounded_high_cache'
}

const BUILT_IN_CACHE_PREFIXES = ['/v1', '/cc', '/ha', '/na'] as const
const DFCACHE_ROUTE_PREFIX = '/dfcache/'
const CACHE_ENDPOINT_LABELS: Record<string, string> = {
  '/v1': '/v1/messages',
  '/cc': '/cc/v1/messages',
  '/ha': '/ha/v1/messages',
  '/na': '/na/v1/messages',
}

export function normalizeCachePolicyPath(prefix: string): string | null {
  const trimmed = prefix.trim()
  if (!trimmed) return null
  const withSlash = trimmed.startsWith('/') ? trimmed : `/${trimmed}`
  const normalized = withSlash.replace(/\/+$/, '') || '/'
  return canonicalCachePolicyPath(normalized)
}

function defaultSimulationPatch(): CacheSimulationPatch {
  return {
    enabled: true,
    targetReadRatio: 0.98,
    tokenScale: 1.6,
    maxSimulatedInputTokens: 300000,
    capJitterMinTokens: 12000,
    capJitterMaxTokens: 24000,
    scaleMinInputTokens: 20000,
  }
}

function defaultKiroRsToolPatch(): KiroRsToolPatch {
  return {
    coverageRatio: 1,
    maxCoverageTokens: 0,
    incrementalCreateEnabled: true,
    maxNewCreationTokensPerRequest: 0,
    cacheCurrentUserStablePrefix: false,
    currentUserStablePrefixMaxTokens: 0,
  }
}

// 与后端 KiroRsToolPlusCachePolicy::default() 保持一致
type FullFieldShaping = Required<UsageFieldShapingPatch>
type FullShapingProfile = {
  input: FullFieldShaping
  output: FullFieldShaping
  cacheRead: FullFieldShaping
  cacheCreation: FullFieldShaping
  capOverflowToInput: boolean
  minCostRatio: number
}

function mergeUsageFieldShaping(value?: UsageFieldShapingPatch): FullFieldShaping {
  return {
    multiplier: value?.multiplier ?? 1,
    jitterRatio: value?.jitterRatio ?? 0,
    minTokens: value?.minTokens ?? 0,
    maxTokens: value?.maxTokens ?? 0,
    maxJitterTokens: value?.maxJitterTokens ?? 0,
  }
}

function mergeUsageShapingProfile(value?: UsageShapingProfilePatch): FullShapingProfile {
  return {
    input: mergeUsageFieldShaping(value?.input),
    output: mergeUsageFieldShaping(value?.output),
    cacheRead: mergeUsageFieldShaping(value?.cacheRead),
    cacheCreation: mergeUsageFieldShaping(value?.cacheCreation),
    capOverflowToInput: value?.capOverflowToInput ?? false,
    minCostRatio: value?.minCostRatio ?? 0,
  }
}

function defaultKiroRsToolPlusPatch(): KiroRsToolPlusPatch {
  return mergeKiroRsToolPlusPatch()
}

function mergeKiroRsToolPlusPatch(value?: KiroRsToolPlusPatch) {
  return {
    coverageRatio: value?.coverageRatio ?? 1,
    maxCoverageTokens: value?.maxCoverageTokens ?? 0,
    incrementalCreateEnabled: value?.incrementalCreateEnabled ?? true,
    cacheCurrentUserStablePrefix: value?.cacheCurrentUserStablePrefix ?? false,
    currentUserStablePrefixMaxTokens: value?.currentUserStablePrefixMaxTokens ?? 0,
    creationFrequency: {
      ...defaultPromptCacheCreationControl(),
      enabled: false,
      ...value?.creationFrequency,
    },
    suppressedCreationTarget: value?.suppressedCreationTarget ?? 'input',
    shaping: mergeUsageShapingProfile(value?.shaping),
    nonStream: {
      mode: value?.nonStream?.mode ?? 'inherit',
      profile: mergeUsageShapingProfile(value?.nonStream?.profile),
    },
    smallRequest: {
      enabled: value?.smallRequest?.enabled ?? false,
      maxInputTokens: value?.smallRequest?.maxInputTokens ?? 1024,
      disableCache: value?.smallRequest?.disableCache ?? false,
      mode: value?.smallRequest?.mode ?? 'inherit',
      profile: mergeUsageShapingProfile(value?.smallRequest?.profile),
    },
  } satisfies KiroRsToolPlusPatch
}

// 与后端 StableSegmentCachePolicy::default() 保持一致
function defaultStableSegmentPatch(): StableSegmentPatch {
  return {
    coverageRatio: 0.9,
    maxCoverageTokens: 120000,
    minTotalInputTokens: 4096,
    minSegmentTokens: 1024,
    incrementalCreateEnabled: true,
    maxNewCreationTokensPerRequest: 30000,
    includeTools: true,
    includeSystem: true,
    includeHistory: true,
    historyMessageLimit: 40,
    honorExplicitCacheControl: true,
    autoCacheSystem: true,
    autoCacheTools: true,
    autoCacheHistoryMessageEnds: true,
    cacheCurrentUserStablePrefix: false,
    currentUserStablePrefixMaxTokens: 0,
    defaultTtlSecs: 300,
    extendedTtlSecs: 3600,
    reportedInputMinTokens: 32,
    reportedInputMaxTokens: 256,
    tokenScale: 1,
    maxSimulatedInputTokens: 0,
    capJitterMinTokens: 0,
    capJitterMaxTokens: 0,
    scaleMinInputTokens: 0,
    outputUpliftPercent: 0,
    outputUpliftMinTokens: 0,
    finalCacheReadMaxTokens: 0,
    finalCacheReadJitterMinTokens: 0,
    finalCacheReadJitterMaxTokens: 0,
    finalCacheCreationMaxTokens: 0,
    finalCacheCreationJitterMinTokens: 0,
    finalCacheCreationJitterMaxTokens: 0,
    finalOutputMaxTokens: 0,
    finalOutputJitterMinTokens: 0,
    finalOutputJitterMaxTokens: 0,
  }
}

function defaultUsagePatch(prefix: string): ReportedUsagePathPolicy {
  if (prefix === '/cc') return pathPolicy(true, inputSamplePolicy(96), writerSamplePolicy(3000))
  if (prefix === '/ha') return pathPolicy(true, inputSamplePolicy(96), preserveFieldPolicy())
  return normalizeDefinedCacheRoute(prefix)
    ? pathPolicy(true, inputSamplePolicy(96), preserveFieldPolicy())
    : pathPolicy()
}

function defaultRouteNamespace(prefix: string): boolean {
  return Boolean(normalizeDefinedCacheRoute(prefix))
}

function builtinDefaultCacheType(prefix: string): CacheStrategyType {
  return prefix === '/na' ? 'no_cache' : 'current_high_cache'
}

export function defaultPathCachePatch(prefix: string, cacheType: CacheStrategyType): CacheRoutePolicyPatch {
  if (cacheType === 'no_cache') {
    return { cacheType: 'no_cache' }
  }
  if (cacheType === 'kiro_rs_tool') {
    return {
      cacheType: 'kiro_rs_tool',
      reportedUsage: defaultUsagePatch(prefix),
      kiroRsTool: defaultKiroRsToolPatch(),
    }
  }
  if (cacheType === 'stable_segment_cache') {
    return {
      cacheType: 'stable_segment_cache',
      routeNamespace: defaultRouteNamespace(prefix),
      reportedUsage: defaultUsagePatch(prefix),
      stableSegment: defaultStableSegmentPatch(),
    }
  }
  if (cacheType === 'kiro_rs_tool_plus') {
    // 不写入参数：路径沿用 Plus 模板，编辑后才保存路径自己的整套参数。
    return { cacheType: 'kiro_rs_tool_plus' }
  }
  return {
    cacheType: cacheType === 'bounded_high_cache' ? 'bounded_high_cache' : 'current_high_cache',
    routeNamespace: defaultRouteNamespace(prefix),
    simulation: defaultSimulationPatch(),
    creationControl: defaultPromptCacheCreationControl(),
    reportedUsage: defaultUsagePatch(prefix),
  }
}

const SHARED_ROUTE_POLICY_FIELDS = ['cachePoint', 'bounds', 'cacheTtl'] as const

function pickDefinedRoutePolicyFields(
  policy: CacheRoutePolicyPatch,
  fields: readonly (keyof CacheRoutePolicyPatch)[],
): Partial<CacheRoutePolicyPatch> {
  const picked: Partial<CacheRoutePolicyPatch> = {}
  for (const field of fields) {
    if (policy[field] !== undefined) Object.assign(picked, { [field]: policy[field] })
  }
  return picked
}

/**
 * 路径切换策略时只保留新策略支持的字段，避免旧策略参数继续跟着路径提交。
 * cachePoint / bounds 是各缓存实现共用的缓存状态限制，其他字段按策略隔离。
 */
export function sanitizeCacheRoutePolicy(policy: CacheRoutePolicyPatch): CacheRoutePolicyPatch {
  const cacheType = policy.cacheType ?? 'no_cache'
  if (cacheType === 'no_cache') return { cacheType: 'no_cache' }

  const shared = pickDefinedRoutePolicyFields(policy, SHARED_ROUTE_POLICY_FIELDS)
  if (isHighCacheStrategy(cacheType)) {
    return {
      cacheType,
      ...shared,
      ...pickDefinedRoutePolicyFields(policy, ['routeNamespace', 'simulation', 'creationControl', 'reportedUsage', 'hitShaping', 'coldRequestGuard']),
    }
  }
  if (cacheType === 'kiro_rs_tool') {
    return {
      cacheType,
      ...shared,
      ...pickDefinedRoutePolicyFields(policy, ['reportedUsage', 'kiroRsTool', 'hitShaping', 'coldRequestGuard']),
    }
  }
  if (cacheType === 'stable_segment_cache') {
    return {
      cacheType,
      ...shared,
      ...pickDefinedRoutePolicyFields(policy, ['routeNamespace', 'reportedUsage', 'stableSegment', 'hitShaping', 'coldRequestGuard']),
    }
  }
  return {
    cacheType,
    ...shared,
    ...pickDefinedRoutePolicyFields(policy, ['kiroRsToolPlus']),
  }
}

const CACHE_STRATEGY_OPTIONS: Array<{
  value: CacheStrategyType
  title: string
  desc: string
}> = [
  { value: 'no_cache', title: '无缓存', desc: '直接使用原始用量，不进入缓存计算。' },
  { value: 'current_high_cache', title: '本地模拟缓存', desc: '本地模拟命中和创建，适合保持现有展示逻辑。' },
  { value: 'kiro_rs_tool', title: 'Kiro-RS Tool', desc: '按会话和路径追踪稳定前缀，逐步补建缓存。' },
  { value: 'stable_segment_cache', title: '稳定片段缓存', desc: '从 tools、system 和历史消息提取稳定缓存片段。' },
  { value: 'kiro_rs_tool_plus', title: 'Kiro-RS Tool Plus', desc: '分别控制四类 usage、写入频次、非流式和小请求。' },
  { value: 'bounded_high_cache', title: '上下限优先缓存', desc: '参数同本地模拟缓存；各字段上下限是最终约束，计费只在范围内补。' },
]

function StrategyChoice({
  value,
  onChange,
}: {
  value: CacheStrategyType
  onChange: (next: CacheStrategyType) => void
}) {
  return (
    <div className="grid gap-2 sm:grid-cols-2" role="radiogroup" aria-label="缓存策略">
      {CACHE_STRATEGY_OPTIONS.map((option) => {
        const selected = option.value === value
        return (
          <button
            key={option.value}
            type="button"
            role="radio"
            aria-checked={selected}
            onClick={() => onChange(option.value)}
            className={`min-w-0 rounded-lg border px-3.5 py-3 text-left transition-colors ${selected
              ? 'border-primary bg-primary/[0.04] shadow-[0_0_0_1px_hsl(var(--primary)/0.16)]'
              : 'border-border/80 hover:border-foreground/30 hover:bg-muted/25'
            }`}
          >
            <div className="flex items-start gap-2.5">
              <span className={`mt-0.5 flex size-4 shrink-0 items-center justify-center rounded-full border ${selected ? 'border-primary bg-primary text-primary-foreground' : 'border-muted-foreground/45 text-transparent'}`}>
                {selected && <Check className="size-3" aria-hidden="true" />}
              </span>
              <span className="min-w-0">
                <span className="block text-[13px] font-semibold leading-5 text-foreground">{option.title}</span>
                <span className="mt-0.5 block text-[11px] leading-4 text-muted-foreground">{option.desc}</span>
              </span>
            </div>
          </button>
        )
      })}
    </div>
  )
}

function isBuiltInCachePrefix(prefix: string): boolean {
  return (BUILT_IN_CACHE_PREFIXES as readonly string[]).includes(prefix)
}

export function canonicalCachePolicyPath(prefix: string): string {
  const normalized = prefix.replace(/\/+$/, '') || '/'
  const lower = normalized.toLowerCase()
  if (lower === '/cc/v1' || lower === '/cc/v1/messages') return '/cc'
  if (lower === '/ha/v1' || lower === '/ha/v1/messages') return '/ha'
  if (lower === '/na/v1' || lower === '/na/v1/messages') return '/na'
  return normalized
}

function cachePolicyPathAliases(prefix: string): string[] {
  if (prefix === '/cc') return ['/cc', '/cc/v1', '/cc/v1/messages']
  if (prefix === '/ha') return ['/ha', '/ha/v1', '/ha/v1/messages']
  if (prefix === '/na') return ['/na', '/na/v1', '/na/v1/messages']
  return [prefix]
}

export function routeOverrideForPrefix(
  pathOverrides: Record<string, CacheRoutePolicyPatch> | undefined,
  prefix: string
): CacheRoutePolicyPatch | undefined {
  for (const alias of cachePolicyPathAliases(prefix)) {
    const policy = pathOverrides?.[alias]
    if (policy) return policy
  }
  return undefined
}

export function reportedUsageForPrefix(
  pathOverrides: Record<string, ReportedUsagePathPolicy> | undefined,
  prefix: string
): ReportedUsagePathPolicy | undefined {
  for (const alias of cachePolicyPathAliases(prefix)) {
    const policy = pathOverrides?.[alias]
    if (policy) return policy
  }
  return undefined
}

export function deletePrefixAliases<T>(record: Record<string, T>, prefix: string) {
  for (const alias of cachePolicyPathAliases(prefix)) {
    delete record[alias]
  }
}

function cacheEndpointLabel(prefix: string): string {
  return CACHE_ENDPOINT_LABELS[prefix] ?? `${prefix}/v1/messages`
}

function compareCachePrefix(a: string, b: string): number {
  const aIndex = (BUILT_IN_CACHE_PREFIXES as readonly string[]).indexOf(a)
  const bIndex = (BUILT_IN_CACHE_PREFIXES as readonly string[]).indexOf(b)
  if (aIndex >= 0 || bIndex >= 0) {
    if (aIndex < 0) return 1
    if (bIndex < 0) return -1
    return aIndex - bIndex
  }
  return a.localeCompare(b)
}

function normalizedDefinedRoutesWith(routes: string[], prefix: string, enabled: boolean): string[] {
  const normalizedRoute = normalizeDefinedCacheRoute(prefix)
  if (!normalizedRoute) return normalizeDefinedCacheRoutes(routes)
  const next = routes.filter((route) => normalizeDefinedCacheRoute(route) !== normalizedRoute)
  if (enabled) next.push(normalizedRoute)
  return normalizeDefinedCacheRoutes(next)
}

function moveDefinedRoute(routes: string[], oldPrefix: string, nextPrefix: string): string[] {
  const oldRoute = normalizeDefinedCacheRoute(oldPrefix)
  if (!oldRoute || !routes.some((route) => normalizeDefinedCacheRoute(route) === oldRoute)) {
    return normalizeDefinedCacheRoutes(routes)
  }
  const withoutOld = normalizedDefinedRoutesWith(routes, oldPrefix, false)
  return normalizedDefinedRoutesWith(withoutOld, nextPrefix, Boolean(normalizeDefinedCacheRoute(nextPrefix)))
}

export function normalizeDefinedCacheRouteName(name: string): string | null {
  const trimmed = name.trim().toLowerCase()
  if (!trimmed) return null
  const suffix = trimmed.startsWith(DFCACHE_ROUTE_PREFIX)
    ? trimmed.slice(DFCACHE_ROUTE_PREFIX.length)
    : trimmed.replace(/^\/+|\/+$/g, '')
  if (!suffix || suffix.includes('/') || suffix.length > 64 || !/^[a-z0-9._-]+$/.test(suffix)) {
    return null
  }
  return suffix
}

export function buildDefinedCacheRoute(name: string): string | null {
  const normalizedName = normalizeDefinedCacheRouteName(name)
  return normalizedName ? `${DFCACHE_ROUTE_PREFIX}${normalizedName}` : null
}

function SimulationOverrideForm({
  value,
  onChange,
}: {
  value: CacheSimulationPatch
  onChange: (next: CacheSimulationPatch) => void
}) {
  const merged = { ...defaultSimulationPatch(), ...value }
  const set = <K extends keyof CacheSimulationPatch>(key: K) => (nextValue: CacheSimulationPatch[K]) =>
    onChange({ ...merged, [key]: nextValue })

  return (
    <div className="space-y-4">
      <TogField
        label="启用本地模拟缓存"
        desc="开启后，这个路径会用本地模拟缓存把原始用量换算成对外显示的缓存用量；关闭后不做这一步。"
        checked={merged.enabled ?? true}
        onChange={set('enabled')}
      />
      <FieldGrid>
        <NumField
          label="目标缓存读取比例"
          desc="希望输入里大约多少比例显示成缓存读取。越高，看起来命中越多；最高 0.99。"
          value={merged.targetReadRatio ?? 0.98}
          min={0}
          max={0.99}
          step={0.01}
          suffix="比例"
          onChange={set('targetReadRatio')}
        />
        <NumField
          label="输入放大倍数"
          desc="计算展示用量前先把输入按这个倍数放大，用来模拟更长上下文；1 表示不放大。"
          value={merged.tokenScale ?? 1.6}
          min={1}
          max={3}
          step={0.1}
          suffix="倍"
          onChange={set('tokenScale')}
        />
        <NumField
          label="模拟输入上限"
          desc="放大后的展示输入最多到这个数；0 表示不设上限。"
          value={merged.maxSimulatedInputTokens ?? 300000}
          min={0}
          suffix="Token"
          onChange={set('maxSimulatedInputTokens')}
        />
        <NumField
          label="放大生效门槛"
          desc="原始输入低于这个值时不放大，避免小请求也显示成很大的用量。"
          value={merged.scaleMinInputTokens ?? 20000}
          min={0}
          suffix="Token"
          onChange={set('scaleMinInputTokens')}
        />
        <NumField
          label="上限扣减下限"
          desc="接近上限时至少少显示这么多，避免每次都卡在同一个上限数字。"
          value={merged.capJitterMinTokens ?? 12000}
          min={0}
          suffix="Token"
          onChange={set('capJitterMinTokens')}
        />
        <NumField
          label="上限扣减上限"
          desc="接近上限时最多少显示这么多；必须大于等于扣减下限。"
          value={merged.capJitterMaxTokens ?? 24000}
          min={0}
          suffix="Token"
          onChange={set('capJitterMaxTokens')}
        />
      </FieldGrid>
    </div>
  )
}

const CACHE_TTL_MODE_OPTIONS: Array<{ value: CacheTtlMode; label: string; desc: string }> = [
  {
    value: 'follow_request_default_5m',
    label: '跟随请求，未指定按 5 分钟',
    desc: '请求的 cache_control 写了 ttl 就按它；没写（或只写 cache_control）按 5 分钟。与官方 API 一致。',
  },
  {
    value: 'follow_request_default_1h',
    label: '跟随请求，未指定按 1 小时',
    desc: '请求写了 ttl 就按它；没写按 1 小时。',
  },
  { value: 'force_5m', label: '固定 5 分钟', desc: '忽略请求的 ttl，一律按 5 分钟（普通缓存）。' },
  { value: 'force_1h', label: '固定 1 小时', desc: '忽略请求的 ttl，一律按 1 小时。' },
  {
    value: 'upstream_then_request_default_5m',
    label: '渠道 1 小时优先，其次跟随请求，未指定按 5 分钟',
    desc: '渠道返回了 1 小时写入就按 1 小时；渠道只返回 5 分钟或没有拆分时，按请求的 ttl，请求也没写按 5 分钟。',
  },
]

const DEFAULT_CACHE_TTL_MODE: CacheTtlMode = 'follow_request_default_5m'

function CacheTtlModeSelect({
  label,
  desc,
  value,
  external,
  onChange,
}: {
  label: string
  desc: string
  value: CacheTtlMode | undefined
  external: boolean
  onChange: (next: CacheTtlMode) => void
}) {
  const options = CACHE_TTL_MODE_OPTIONS.filter(
    (option) => external || option.value !== 'upstream_then_request_default_5m',
  )
  const current = value ?? DEFAULT_CACHE_TTL_MODE
  const selected = options.find((option) => option.value === current) ?? options[0]
  return (
    <div className="space-y-1.5">
      <div className="text-sm font-semibold text-foreground">{label}</div>
      <div className="text-xs leading-relaxed text-muted-foreground">{desc}</div>
      <Select value={selected.value} onValueChange={(next) => onChange(next as CacheTtlMode)}>
        <SelectTrigger size="sm" className="w-full max-w-md"><SelectValue /></SelectTrigger>
        <SelectContent>
          {options.map((option) => (
            <SelectItem key={option.value} value={option.value}>{option.label}</SelectItem>
          ))}
        </SelectContent>
      </Select>
      <div className="text-xs leading-relaxed text-muted-foreground">{selected.desc}</div>
    </div>
  )
}

function CacheTtlPolicyForm({
  value,
  onChange,
}: {
  value: CacheTtlPolicyPatch | undefined
  onChange: (next: CacheTtlPolicyPatch) => void
}) {
  return (
    <ParameterGroup
      title="缓存写入时长"
      desc="最终 usage 的缓存写入全部记为 5 分钟（cache_creation.ephemeral_5m_input_tokens，普通缓存）或 1 小时（ephemeral_1h_input_tokens），成本按对应单价计算。"
    >
      <FieldGrid>
        <CacheTtlModeSelect
          label="本地账号"
          desc="本地 Kiro 账号返回的缓存写入时长。"
          value={value?.local}
          external={false}
          onChange={(local) => onChange({ ...value, local })}
        />
        <CacheTtlModeSelect
          label="外部池账号"
          desc="外部池按本路径策略整形 usage 时的缓存写入时长；外部账号透传 usage 时不改。"
          value={value?.externalPool}
          external
          onChange={(externalPool) => onChange({ ...value, externalPool })}
        />
      </FieldGrid>
    </ParameterGroup>
  )
}

function ColdRequestGuardToggle({
  value,
  onChange,
}: {
  value: ColdRequestGuardPatch | undefined
  onChange: (next: ColdRequestGuardPatch) => void
}) {
  return (
    <TogField
      label="冷请求用量约束"
      desc="默认关闭，关闭时与未开启前完全一致。开启后，本地缓存没有命中的请求（新会话首轮、fork / resume 后、服务重启后的第一条）缓存读取为 0，输入 + 写入不超过真实输入；命中之后的轮次仍按上面的参数整形。"
      checked={Boolean(value?.enabled)}
      onChange={(enabled) => onChange({ enabled })}
    />
  )
}

function CacheHitShapingForm({
  value,
  bounded = false,
  onChange,
}: {
  value: CacheHitShapingPatch
  /** 上下限优先策略：计费下限只在上下限内补，说明文字随之调整。 */
  bounded?: boolean
  onChange: (next: CacheHitShapingPatch) => void
}) {
  const merged = { ...defaultCacheHitShaping(), ...value }
  const set = <K extends keyof CacheHitShapingPatch>(key: K) => (nextValue: CacheHitShapingPatch[K]) =>
    onChange({ ...merged, [key]: nextValue })

  return (
    <div className="space-y-4">
      <TogField
        label="启用命中率与小值整形"
        desc={bounded
          ? '在最终显示给下游的用量上执行：在各字段上下限内按目标命中率分配，写入保持较大量级，计费尽量不低于下限。'
          : '在最终显示给下游的用量上执行：在输入、读取、写入的总量内按目标命中率重新分配，写入保持较大量级，并保证计费不低于原始用量。'}
        checked={merged.enabled}
        onChange={set('enabled')}
      />
      <FieldGrid>
        <NumField
          label="目标命中率"
          desc="缓存读取 ÷（输入 + 读取 + 写入），口径与 sub2api 的 Token 使用趋势一致。会话首请求不受此约束；会受计费下限约束，放大倍数越高越容易达到。"
          value={merged.targetHitRatio}
          min={0}
          max={0.99}
          step={0.01}
          suffix="比例"
          disabled={!merged.enabled}
          onChange={set('targetHitRatio')}
        />
        <NumField
          label="命中率浮动"
          desc="在目标值之上随机多出的幅度，避免每次都精确等于目标值；例如目标 0.9、浮动 0.04，实际落在 0.90~0.94。"
          value={merged.ratioJitter}
          min={0}
          max={0.2}
          step={0.01}
          suffix="比例"
          disabled={!merged.enabled}
          onChange={set('ratioJitter')}
        />
        <NumField
          label="缓存读取最小值"
          desc="非零的缓存读取低于这个值时抬到这个值。0 表示不处理。"
          value={merged.minCacheReadTokens}
          min={0}
          suffix="Token"
          disabled={!merged.enabled}
          onChange={set('minCacheReadTokens')}
        />
        <NumField
          label="缓存写入最小值"
          desc="非零的缓存写入低于这个值时抬到这个值。0 表示不处理。"
          value={merged.minCacheCreationTokens}
          min={0}
          suffix="Token"
          disabled={!merged.enabled}
          onChange={set('minCacheCreationTokens')}
        />
        <NumField
          label="计费下限倍数"
          desc={bounded
            ? '整形后的计费尽量不低于原始用量计费的这个倍数，只在各字段上下限内补；补不上的差额累计，在之后放行写入的请求上补。0 表示不限制。'
            : '整形后的计费不低于原始用量计费的这个倍数；与目标命中率冲突时以这里为准。1 表示不亏损，0 表示不限制。'}
          value={merged.minCostRatio}
          min={0}
          max={5}
          step={0.05}
          suffix="倍"
          disabled={!merged.enabled}
          onChange={set('minCostRatio')}
        />
        <NumField
          label="缓存读取上限倍数"
          desc={bounded
            ? '缓存读取最多为原始输入的多少倍，是最终上限；触及上限后命中率和计费都可以低于目标。0 表示不限制。'
            : '缓存读取最多为原始输入的多少倍，用来控制读取数值不过大；触及上限后命中率可以低于目标，计费下限仍然生效。0 表示不限制。'}
          value={merged.maxReadMultiplier}
          min={0}
          max={50}
          step={0.5}
          suffix="倍"
          disabled={!merged.enabled}
          onChange={set('maxReadMultiplier')}
        />
        <NumField
          label="最小写入比例"
          desc="本次允许写入时，缓存写入至少为原始输入的这个比例，用来把写入调大；不允许写入的请求不受影响。0 表示不限制。"
          value={merged.minCreationRatio}
          min={0}
          max={5}
          step={0.05}
          suffix="倍"
          disabled={!merged.enabled}
          onChange={set('minCreationRatio')}
        />
      </FieldGrid>
    </div>
  )
}

function CreationControlOverrideForm({
  value,
  onChange,
}: {
  value: PromptCacheCreationControlConfig
  onChange: (next: PromptCacheCreationControlConfig) => void
}) {
  const merged = { ...defaultPromptCacheCreationControl(), ...value }
  const set = <K extends keyof PromptCacheCreationControlConfig>(key: K) => (nextValue: PromptCacheCreationControlConfig[K]) =>
    onChange({ ...merged, [key]: nextValue })

  return (
    <div className="space-y-4">
      <TogField
        label="启用缓存创建频次控制"
        desc="控制什么时候显示缓存写入数字，避免每次成功请求都显示写入；只影响对外显示的用量。"
        checked={merged.enabled}
        onChange={set('enabled')}
      />
      <FieldGrid>
        <NumField
          label="最小成功请求间隔"
          desc="两次显示缓存写入之间至少隔多少次成功请求；0 表示不按次数限制。"
          value={merged.minSuccessfulRequestsBetweenCreation}
          min={0}
          suffix="次"
          disabled={!merged.enabled}
          onChange={set('minSuccessfulRequestsBetweenCreation')}
        />
        <NumField
          label="最小时间间隔"
          desc="两次显示缓存写入之间至少间隔多久；0 表示不按时间限制。"
          value={merged.minCreationIntervalSecs}
          min={0}
          suffix="秒"
          disabled={!merged.enabled}
          onChange={set('minCreationIntervalSecs')}
        />
        <NumField
          label="最小累计增量"
          desc="累计新增输入达到多少 Token 后，才允许再次显示缓存写入；0 表示不限制。"
          value={merged.minCreationDeltaTokens}
          min={0}
          suffix="Token"
          disabled={!merged.enabled}
          onChange={set('minCreationDeltaTokens')}
        />
        <NumField
          label="单次展示上限"
          desc="一次响应里最多显示多少缓存写入 Token；0 表示不限制。"
          value={merged.maxCreationTokensPerEvent}
          min={0}
          suffix="Token"
          disabled={!merged.enabled}
          onChange={set('maxCreationTokensPerEvent')}
        />
        <NumField
          label="额度窗口长度"
          desc="按多长时间统计一次缓存写入展示额度；0 表示关闭窗口额度控制。"
          value={merged.creationBudgetWindowSecs}
          min={0}
          suffix="秒"
          disabled={!merged.enabled}
          onChange={set('creationBudgetWindowSecs')}
        />
        <NumField
          label="窗口展示额度"
          desc="一个时间窗口内最多显示多少缓存写入 Token；0 表示不限制。"
          value={merged.maxCreationTokensPerWindow}
          min={0}
          suffix="Token"
          disabled={!merged.enabled}
          onChange={set('maxCreationTokensPerWindow')}
        />
        <NumField
          label="空闲后清理状态"
          desc="这个路径多久没有新请求后清理临时计数，减少长期占用；0 表示不按空闲时间清理。"
          value={merged.expireAfterIdleSecs}
          min={0}
          suffix="秒"
          disabled={!merged.enabled}
          onChange={set('expireAfterIdleSecs')}
        />
      </FieldGrid>
    </div>
  )
}

function KiroRsToolPolicyForm({
  value,
  onChange,
}: {
  value: KiroRsToolPatch
  onChange: (next: KiroRsToolPatch) => void
}) {
  const merged = { ...defaultKiroRsToolPatch(), ...value }
  const set = <K extends keyof KiroRsToolPatch>(key: K) => (nextValue: KiroRsToolPatch[K]) =>
    onChange({ ...merged, [key]: nextValue })

  return (
    <div className="space-y-4">
      <FieldGrid>
        <NumField
          label="缓存覆盖比例"
          desc="本轮最多把多少稳定内容纳入 Kiro-RS Tool 缓存。1 表示保持当前表现；0 表示不创建也不读取。"
          value={merged.coverageRatio ?? 1}
          min={0}
          max={1}
          step={0.05}
          suffix="比例"
          onChange={set('coverageRatio')}
        />
        <NumField
          label="覆盖上限"
          desc="单次最多纳入多少 Token。0 表示不限制，保持当前 Kiro-RS Tool 表现。"
          value={merged.maxCoverageTokens ?? 0}
          min={0}
          suffix="Token"
          onChange={set('maxCoverageTokens')}
        />
        <NumField
          label="单次新增创建上限"
          desc="一次请求最多新增多少缓存。0 表示不限制；后续读取不会超过之前真正创建过的数量。"
          value={merged.maxNewCreationTokensPerRequest ?? 0}
          min={0}
          suffix="Token"
          onChange={set('maxNewCreationTokensPerRequest')}
        />
        <NumField
          label="当前用户前缀上限"
          desc="开启下方选项后，最多取当前用户文本前段多少 Token。0 表示不取。"
          value={merged.currentUserStablePrefixMaxTokens ?? 0}
          min={0}
          suffix="Token"
          disabled={!merged.cacheCurrentUserStablePrefix}
          onChange={set('currentUserStablePrefixMaxTokens')}
        />
      </FieldGrid>
      <TogField
        label="允许后续继续创建"
        desc="同一会话命中旧缓存后，如果又出现新的稳定内容，是否继续补创建。关闭后命中时只读不补建。"
        checked={merged.incrementalCreateEnabled ?? true}
        onChange={set('incrementalCreateEnabled')}
      />
      <TogField
        label="缓存当前用户稳定前缀"
        desc="默认关闭，和当前 Kiro-RS Tool 表现一致。开启后只取当前用户文本前段，适合确实有稳定长前缀的请求。"
        checked={merged.cacheCurrentUserStablePrefix ?? false}
        onChange={set('cacheCurrentUserStablePrefix')}
      />
    </div>
  )
}

function UsageFieldShapingForm({
  title,
  desc,
  value,
  onChange,
}: {
  title: string
  desc: string
  value: FullFieldShaping
  onChange: (next: FullFieldShaping) => void
}) {
  const set = <K extends keyof UsageFieldShapingPatch>(key: K) => (nextValue: number) =>
    onChange({ ...value, [key]: nextValue })
  const compactField = (
    label: string,
    description: string,
    field: keyof UsageFieldShapingPatch,
    fieldValue: number,
    min: number,
    max?: number,
    step?: number,
    suffix?: string,
    disabled = false,
  ) => (
    <div className="min-w-0 space-y-1.5">
      <label
        className="block text-xs font-medium text-foreground"
        title={description}
      >
        {label}
      </label>
      <div className="flex min-w-0 items-center gap-2">
        <Input
          type="number"
          className="h-9 w-24 shrink-0 px-2 text-sm"
          value={fieldValue}
          min={min}
          max={max}
          step={step}
          inputMode={step && step < 1 ? 'decimal' : 'numeric'}
          disabled={disabled}
          aria-label={`${title} · ${label}`}
          onChange={(event) => set(field)(numberValue(event.target.value, min))}
        />
        {suffix && <span className="shrink-0 text-xs text-muted-foreground">{suffix}</span>}
      </div>
      <div className="text-[11px] leading-4 text-muted-foreground">{description}</div>
    </div>
  )

  return (
    <section className="grid gap-5 py-6 first:pt-0 xl:grid-cols-[minmax(12rem,0.85fr)_minmax(0,4fr)] xl:items-start">
      <div className="max-w-xs xl:pt-1">
        <div className="text-[13px] font-semibold">{title}</div>
        <div className="mt-1.5 text-xs leading-5 text-muted-foreground">{desc}</div>
      </div>
      <div className="grid min-w-0 gap-x-6 gap-y-5 sm:grid-cols-2 xl:grid-cols-3">
        {compactField('放大倍数', '可大于 1；0 表示上报为 0。', 'multiplier', value.multiplier, 0, 1000, 0.05, '倍')}
        {compactField('浮动比例', '结果在 ×(1±比例) 内确定性浮动。', 'jitterRatio', value.jitterRatio, 0, 1, 0.01, '比例')}
        {compactField('下限', '非零时不低于该值；0 不限。', 'minTokens', value.minTokens, 0, undefined, undefined, 'Token')}
        {compactField('上限', '不超过该值；0 不限。', 'maxTokens', value.maxTokens, 0, undefined, undefined, 'Token')}
        {compactField('上限浮动', '触及上限时随机少报 0~该值。', 'maxJitterTokens', value.maxJitterTokens, 0, undefined, undefined, 'Token', value.maxTokens <= 0)}
      </div>
    </section>
  )
}

function UsageShapingProfileForm({
  value,
  onChange,
}: {
  value: FullShapingProfile
  onChange: (next: FullShapingProfile) => void
}) {
  const setField = (key: 'input' | 'output' | 'cacheRead' | 'cacheCreation') =>
    (next: FullFieldShaping) => onChange({ ...value, [key]: next })
  return (
    <div className="space-y-5">
      <div className="divide-y divide-border/60 border-y border-border/60">
        <UsageFieldShapingForm title="缓存读取 cache_read" desc="前缀命中部分。放大后再执行上下限。" value={value.cacheRead} onChange={setField('cacheRead')} />
        <UsageFieldShapingForm title="缓存写入 cache_creation" desc="新增前缀部分。放大后先经过写入频次控制，再执行上下限。" value={value.cacheCreation} onChange={setField('cacheCreation')} />
        <UsageFieldShapingForm title="输入 input" desc="未缓存部分。上下限作用于最终 input（含转入部分），计费下限补足的部分不受上限约束。" value={value.input} onChange={setField('input')} />
        <UsageFieldShapingForm title="输出 output" desc="上报的输出 Token。" value={value.output} onChange={setField('output')} />
      </div>
      <div className="grid gap-x-6 gap-y-5 border-t border-border/60 pt-4 md:grid-cols-2">
        <TogField
          label="读写超出上限转入 input"
          desc="关闭时被上限裁掉的缓存读取 / 写入直接丢弃；开启后计入 input。"
          checked={value.capOverflowToInput}
          onChange={(capOverflowToInput) => onChange({ ...value, capOverflowToInput })}
        />
        <NumField
          label="计费下限倍数"
          desc="整形后的计费不低于真实输入计费的这个倍数，差额补到 input；0 表示关闭，最大 100。"
          value={value.minCostRatio}
          min={0}
          max={100}
          step={0.05}
          suffix="倍"
          onChange={(minCostRatio) => onChange({ ...value, minCostRatio })}
        />
      </div>
    </div>
  )
}

function UsageShapingModeSelect({
  value,
  onChange,
}: {
  value: UsageShapingMode
  onChange: (next: UsageShapingMode) => void
}) {
  return (
    <Select value={value} onValueChange={(next) => onChange(next as UsageShapingMode)}>
      <SelectTrigger size="sm" className="w-full max-w-xs"><SelectValue /></SelectTrigger>
      <SelectContent>
        <SelectItem value="inherit">沿用默认整形参数</SelectItem>
        <SelectItem value="custom">使用单独参数</SelectItem>
        <SelectItem value="raw">不整形（上报前缀拆分原值，不做频次控制）</SelectItem>
      </SelectContent>
    </Select>
  )
}

function PlusPanel({
  eyebrow,
  title,
  desc,
  children,
}: {
  eyebrow: string
  title: string
  desc: string
  children: ReactNode
}) {
  return (
    <div className="space-y-7">
      <header className="border-b border-border/70 pb-5">
        <div className="text-[10px] font-semibold uppercase tracking-[0.16em] text-primary">{eyebrow}</div>
        <h3 className="mt-1 text-base font-semibold tracking-tight text-foreground">{title}</h3>
        <p className="mt-1.5 max-w-3xl text-xs leading-5 text-muted-foreground">{desc}</p>
      </header>
      {children}
    </div>
  )
}

function KiroRsToolPlusPolicyForm({
  value,
  onChange,
}: {
  value: KiroRsToolPlusPatch
  onChange: (next: KiroRsToolPlusPatch) => void
}) {
  const merged = mergeKiroRsToolPlusPatch(value)
  const set = <K extends keyof KiroRsToolPlusPatch>(key: K) => (nextValue: KiroRsToolPlusPatch[K]) =>
    onChange({ ...merged, [key]: nextValue })

  return (
    <div className="min-w-0 space-y-10">
          <PlusPanel eyebrow="01 / tracking" title="前缀追踪" desc="先定义本轮可以纳入缓存的稳定前缀范围，再决定命中后是否继续补建。">
            <div className="grid gap-8 lg:grid-cols-[minmax(0,1.25fr)_minmax(15rem,0.75fr)]">
            <ParameterGroup title="覆盖范围" desc="数值只描述本轮追踪边界，不改变下方 usage 整形。">
              <FieldGrid>
                <NumField
                  label="缓存覆盖比例"
                  desc="本轮最多把多少稳定前缀纳入缓存，范围 0 到 1。"
                  value={merged.coverageRatio ?? 1}
                  min={0}
                  max={1}
                  step={0.05}
                  suffix="比例"
                  onChange={set('coverageRatio')}
                />
                <NumField
                  label="覆盖上限"
                  desc="单次最多纳入多少 Token。0 表示不限制。"
                  value={merged.maxCoverageTokens ?? 0}
                  min={0}
                  suffix="Token"
                  onChange={set('maxCoverageTokens')}
                />
                <NumField
                  label="当前用户前缀上限"
                  desc="开启当前用户前缀后，最多取文本前段多少 Token。"
                  value={merged.currentUserStablePrefixMaxTokens ?? 0}
                  min={0}
                  suffix="Token"
                  disabled={!merged.cacheCurrentUserStablePrefix}
                  onChange={set('currentUserStablePrefixMaxTokens')}
                />
              </FieldGrid>
            </ParameterGroup>
            <ParameterGroup title="补建开关" desc="控制已有缓存命中后的后续行为。">
              <div className="space-y-5">
                <TogField
                  label="允许后续继续创建"
                  desc="命中旧缓存后出现新的稳定内容时继续补创建。"
                  checked={merged.incrementalCreateEnabled ?? true}
                  onChange={set('incrementalCreateEnabled')}
                />
                <TogField
                  label="缓存当前用户稳定前缀"
                  desc="只取当前用户文本前段，适合有稳定长前缀的请求。"
                  checked={merged.cacheCurrentUserStablePrefix ?? false}
                  onChange={set('cacheCurrentUserStablePrefix')}
                />
              </div>
              </ParameterGroup>
            </div>
          </PlusPanel>
          <PlusPanel eyebrow="02 / frequency" title="写入频次限制" desc="在放大之后、上下限之前执行，额度按放大后的写入计算。小请求关闭缓存或选择不整形时不参与。">
            <div className="grid gap-8 lg:grid-cols-[minmax(0,1.35fr)_minmax(15rem,0.65fr)]">
            <ParameterGroup title="频次与额度" desc="控制什么时候把新增缓存写入展示给客户端。">
              <CreationControlOverrideForm
                value={merged.creationFrequency}
                onChange={(creationFrequency) => onChange({ ...merged, creationFrequency })}
              />
            </ParameterGroup>
            <ParameterGroup title="被拦下的写入" desc="被频次规则拦下的写入如何计入本次 usage。">
              <Select
                value={merged.suppressedCreationTarget ?? 'input'}
                onValueChange={(next) => onChange({ ...merged, suppressedCreationTarget: next as SuppressedCreationTarget })}
              >
                <SelectTrigger size="sm" className="w-full max-w-xs"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem value="input">输入 input</SelectItem>
                  <SelectItem value="cache_read">缓存读取 cache_read</SelectItem>
                  <SelectItem value="drop">直接丢弃</SelectItem>
                </SelectContent>
              </Select>
              </ParameterGroup>
            </div>
          </PlusPanel>
          <PlusPanel eyebrow="03 / stream" title="默认整形（流式请求）" desc="执行顺序：放大与浮动 → 写入频次限制 → 读写上下限 → 输入 / 输出上下限 → 计费下限。">
            <UsageShapingProfileForm value={merged.shaping} onChange={set('shaping')} />
          </PlusPanel>
          <PlusPanel eyebrow="04 / non-stream" title="非流式请求整形" desc="非流式请求可以沿用流式参数、单独整形，或直接返回拆分前的原值。">
            <ParameterGroup title="请求模式" desc="先选模式，只有选择单独参数时才展开完整的整形表单。">
              <UsageShapingModeSelect
                value={merged.nonStream.mode}
                onChange={(mode) => onChange({ ...merged, nonStream: { ...merged.nonStream, mode } })}
              />
            </ParameterGroup>
            {merged.nonStream.mode === 'custom' && (
              <ParameterGroup title="单独参数" desc="这组参数只对非流式请求生效。">
                <UsageShapingProfileForm
                  value={merged.nonStream.profile}
                  onChange={(profile) => onChange({ ...merged, nonStream: { ...merged.nonStream, profile } })}
                />
              </ParameterGroup>
            )}
          </PlusPanel>
          <PlusPanel eyebrow="05 / small request" title="小请求整形" desc="真实输入不超过阈值的请求，优先级高于非流式整形。">
            <ParameterGroup title="触发条件" desc="先开启规则，再设置哪些请求被视为小请求。">
              <TogField
                label="启用小请求整形"
                checked={merged.smallRequest.enabled ?? false}
                onChange={(enabled) => onChange({ ...merged, smallRequest: { ...merged.smallRequest, enabled } })}
              />
              <div className="mt-5 grid gap-x-8 gap-y-5 lg:grid-cols-2">
                <NumField
                  label="小请求阈值"
                  desc="真实输入 Token 不超过该值视为小请求。"
                  value={merged.smallRequest.maxInputTokens ?? 1024}
                  min={0}
                  suffix="Token"
                  disabled={!merged.smallRequest.enabled}
                  onChange={(maxInputTokens) => onChange({ ...merged, smallRequest: { ...merged.smallRequest, maxInputTokens } })}
                />
                <TogField
                  label="小请求不上报缓存"
                  desc="缓存读取和写入全部并回 input，也不消耗写入频次额度。"
                  checked={merged.smallRequest.disableCache ?? false}
                  disabled={!merged.smallRequest.enabled}
                  onChange={(disableCache) => onChange({ ...merged, smallRequest: { ...merged.smallRequest, disableCache } })}
                />
              </div>
            </ParameterGroup>
            {merged.smallRequest.enabled && (
              <ParameterGroup title="小请求参数来源" desc="沿用默认整形或为小请求单独提供一份参数。">
                <UsageShapingModeSelect
                  value={merged.smallRequest.mode ?? 'inherit'}
                  onChange={(mode) => onChange({ ...merged, smallRequest: { ...merged.smallRequest, mode } })}
                />
                {merged.smallRequest.mode === 'custom' && (
                  <div className="mt-6">
                    <UsageShapingProfileForm
                      value={merged.smallRequest.profile}
                      onChange={(profile) => onChange({ ...merged, smallRequest: { ...merged.smallRequest, profile } })}
                    />
                  </div>
                )}
              </ParameterGroup>
            )}
          </PlusPanel>
    </div>
  )
}

function StableSegmentPolicyForm({
  value,
  onChange,
}: {
  value: StableSegmentPatch
  onChange: (next: StableSegmentPatch) => void
}) {
  const merged = { ...defaultStableSegmentPatch(), ...value }
  const set = <K extends keyof StableSegmentPatch>(key: K) => (nextValue: StableSegmentPatch[K]) =>
    onChange({ ...merged, [key]: nextValue })

  return (
    <div className="space-y-6">
      <ParameterGroup title="覆盖范围">
        <TwoCol>
          <NumField
            label="缓存覆盖比例"
            desc="本轮最多把多少比例的输入算作缓存覆盖，范围 0 到 1。"
            value={merged.coverageRatio ?? 0.9}
            min={0}
            max={1}
            step={0.05}
            suffix="比例"
            onChange={set('coverageRatio')}
          />
          <NumField
            label="覆盖上限"
            desc="单次最多覆盖多少 Token。0 表示不额外限制。"
            value={merged.maxCoverageTokens ?? 0}
            min={0}
            suffix="Token"
            onChange={set('maxCoverageTokens')}
          />
          <NumField
            label="最小总输入"
            desc="请求总输入低于这个值时不产生缓存，避免短请求制造缓存。"
            value={merged.minTotalInputTokens ?? 0}
            min={0}
            suffix="Token"
            onChange={set('minTotalInputTokens')}
          />
          <NumField
            label="最小稳定段"
            desc="稳定内容累计低于这个值时不建立缓存点。"
            value={merged.minSegmentTokens ?? 0}
            min={0}
            suffix="Token"
            onChange={set('minSegmentTokens')}
          />
        </TwoCol>
      </ParameterGroup>

      <ParameterGroup title="缓存创建">
        <TwoCol>
          <NumField
            label="单次新增创建上限"
            desc="一次请求最多新增多少缓存。0 表示不限制。"
            value={merged.maxNewCreationTokensPerRequest ?? 0}
            min={0}
            suffix="Token"
            onChange={set('maxNewCreationTokensPerRequest')}
          />
          <NumField
            label="当前用户前缀上限"
            desc="开启下方选项后，最多取当前用户文本前段多少 Token。"
            value={merged.currentUserStablePrefixMaxTokens ?? 0}
            min={0}
            suffix="Token"
            disabled={!merged.cacheCurrentUserStablePrefix}
            onChange={set('currentUserStablePrefixMaxTokens')}
          />
        </TwoCol>
        <TwoCol>
          <TogField
            label="允许后续继续创建"
            desc="命中已有缓存后，如果又出现新的稳定尾部，是否继续补创建。"
            checked={merged.incrementalCreateEnabled ?? true}
            onChange={set('incrementalCreateEnabled')}
          />
          <TogField
            label="缓存当前用户稳定前缀"
            desc="默认关闭。开启后把当前用户消息的稳定前段也纳入缓存，适合确实有固定长前缀的请求。"
            checked={merged.cacheCurrentUserStablePrefix ?? false}
            onChange={set('cacheCurrentUserStablePrefix')}
          />
        </TwoCol>
      </ParameterGroup>

      <ParameterGroup title="稳定内容来源">
        <TwoCol>
          <TogField
            label="包含 tools"
            desc="工具定义是否参与稳定内容计算。"
            checked={merged.includeTools ?? true}
            onChange={set('includeTools')}
          />
          <TogField
            label="包含 system"
            desc="system 提示词是否参与稳定内容计算。"
            checked={merged.includeSystem ?? true}
            onChange={set('includeSystem')}
          />
          <TogField
            label="包含历史消息"
            desc="历史消息是否参与稳定内容计算。"
            checked={merged.includeHistory ?? true}
            onChange={set('includeHistory')}
          />
          <NumField
            label="历史消息条数上限"
            desc="最多纳入多少条历史消息。0 表示不限制。"
            value={merged.historyMessageLimit ?? 0}
            min={0}
            suffix="条"
            disabled={!merged.includeHistory}
            onChange={set('historyMessageLimit')}
          />
        </TwoCol>
      </ParameterGroup>

      <ParameterGroup title="缓存点规则">
        <TwoCol>
          <TogField
            label="识别请求里的 cache_control"
            desc="开启后按客户端显式标记的 cache_control 和 TTL 建立缓存点。"
            checked={merged.honorExplicitCacheControl ?? true}
            onChange={set('honorExplicitCacheControl')}
          />
          <TogField
            label="自动缓存 system"
            desc="请求没有显式 cache_control 时，自动在 system 结尾建立缓存点。"
            checked={merged.autoCacheSystem ?? true}
            onChange={set('autoCacheSystem')}
          />
          <TogField
            label="自动缓存 tools"
            desc="请求没有显式 cache_control 时，自动在 tools 结尾建立缓存点。"
            checked={merged.autoCacheTools ?? true}
            onChange={set('autoCacheTools')}
          />
          <TogField
            label="自动缓存历史消息边界"
            desc="在历史消息的边界自动建立缓存点。"
            checked={merged.autoCacheHistoryMessageEnds ?? true}
            onChange={set('autoCacheHistoryMessageEnds')}
          />
        </TwoCol>
        <TwoCol>
          <NumField
            label="默认 TTL"
            desc="普通缓存点的有效期，必须大于 0。"
            value={merged.defaultTtlSecs ?? 300}
            min={1}
            suffix="秒"
            onChange={set('defaultTtlSecs')}
          />
          <NumField
            label="长 TTL"
            desc="客户端标记 1 小时缓存时使用，不能小于默认 TTL。"
            value={merged.extendedTtlSecs ?? 3600}
            min={1}
            suffix="秒"
            onChange={set('extendedTtlSecs')}
          />
        </TwoCol>
      </ParameterGroup>

      <ParameterGroup title="返回的 input_tokens">
        <TwoCol>
          <NumField
            label="最小值"
            desc="扣除缓存部分后，返回给客户端的 input_tokens 不低于这个值。"
            value={merged.reportedInputMinTokens ?? 0}
            min={0}
            suffix="Token"
            onChange={set('reportedInputMinTokens')}
          />
          <NumField
            label="最大值"
            desc="返回给客户端的 input_tokens 不高于这个值。0 表示不限制；大于 0 时不能小于最小值。"
            value={merged.reportedInputMaxTokens ?? 0}
            min={0}
            suffix="Token"
            onChange={set('reportedInputMaxTokens')}
          />
        </TwoCol>
      </ParameterGroup>

      <ParameterGroup title="读写放大">
        <TwoCol>
          <NumField
            label="放大倍数"
            desc="真实总输入先乘这个倍数，再按片段覆盖比例归入读写；input 仍是真实的未覆盖部分。1 表示不放大，最大 3。"
            value={merged.tokenScale ?? 1}
            min={1}
            max={3}
            step={0.05}
            suffix="倍"
            onChange={set('tokenScale')}
          />
          <NumField
            label="放大门槛"
            desc="真实总输入低于这个值时不放大。0 表示总是放大。"
            value={merged.scaleMinInputTokens ?? 0}
            min={0}
            suffix="Token"
            onChange={set('scaleMinInputTokens')}
          />
          <NumField
            label="放大后总输入上限"
            desc="放大后的总输入超过这个值时按上限截断。0 表示不限制。"
            value={merged.maxSimulatedInputTokens ?? 0}
            min={0}
            suffix="Token"
            onChange={set('maxSimulatedInputTokens')}
          />
          <NumField
            label="上限扣减最小值"
            desc="触到上限时额外扣减的最小量，避免每次都等于上限。"
            value={merged.capJitterMinTokens ?? 0}
            min={0}
            suffix="Token"
            onChange={set('capJitterMinTokens')}
          />
          <NumField
            label="上限扣减最大值"
            desc="触到上限时额外扣减的最大量，实际不超过上限的 8%。"
            value={merged.capJitterMaxTokens ?? 0}
            min={0}
            suffix="Token"
            onChange={set('capJitterMaxTokens')}
          />
        </TwoCol>
      </ParameterGroup>

      <ParameterGroup title="output 放大">
        <TwoCol>
          <NumField
            label="放大比例"
            desc="output 超过门槛时按这个比例放大，0 表示关闭，最大 200%。"
            value={merged.outputUpliftPercent ?? 0}
            min={0}
            max={200}
            suffix="%"
            onChange={set('outputUpliftPercent')}
          />
          <NumField
            label="放大门槛"
            desc="output 超过这个值才放大。0 表示所有非零 output 都放大。"
            value={merged.outputUpliftMinTokens ?? 0}
            min={0}
            suffix="Token"
            onChange={set('outputUpliftMinTokens')}
          />
        </TwoCol>
      </ParameterGroup>

      <ParameterGroup title="最终上限">
        <TwoCol>
          <NumField
            label="读取上限"
            desc="最终返回的 cache_read_input_tokens 不超过这个值。0 表示沿用默认 70 万。"
            value={merged.finalCacheReadMaxTokens ?? 0}
            min={0}
            suffix="Token"
            onChange={set('finalCacheReadMaxTokens')}
          />
          <NumField
            label="读取上限扣减最小值"
            desc="读取触到上限时的扣减区间下限。"
            value={merged.finalCacheReadJitterMinTokens ?? 0}
            min={0}
            suffix="Token"
            onChange={set('finalCacheReadJitterMinTokens')}
          />
          <NumField
            label="读取上限扣减最大值"
            desc="读取触到上限时的扣减区间上限。"
            value={merged.finalCacheReadJitterMaxTokens ?? 0}
            min={0}
            suffix="Token"
            onChange={set('finalCacheReadJitterMaxTokens')}
          />
          <NumField
            label="写入上限"
            desc="最终返回的 cache_creation_input_tokens 不超过这个值。0 表示沿用默认 40 万。"
            value={merged.finalCacheCreationMaxTokens ?? 0}
            min={0}
            suffix="Token"
            onChange={set('finalCacheCreationMaxTokens')}
          />
          <NumField
            label="写入上限扣减最小值"
            desc="写入触到上限时的扣减区间下限。"
            value={merged.finalCacheCreationJitterMinTokens ?? 0}
            min={0}
            suffix="Token"
            onChange={set('finalCacheCreationJitterMinTokens')}
          />
          <NumField
            label="写入上限扣减最大值"
            desc="写入触到上限时的扣减区间上限。"
            value={merged.finalCacheCreationJitterMaxTokens ?? 0}
            min={0}
            suffix="Token"
            onChange={set('finalCacheCreationJitterMaxTokens')}
          />
          <NumField
            label="output 上限"
            desc="放大后的 output 不超过这个值。0 表示不限制。"
            value={merged.finalOutputMaxTokens ?? 0}
            min={0}
            suffix="Token"
            onChange={set('finalOutputMaxTokens')}
          />
          <NumField
            label="output 上限扣减最小值"
            desc="output 触到上限时的扣减区间下限。"
            value={merged.finalOutputJitterMinTokens ?? 0}
            min={0}
            suffix="Token"
            onChange={set('finalOutputJitterMinTokens')}
          />
          <NumField
            label="output 上限扣减最大值"
            desc="output 触到上限时的扣减区间上限。"
            value={merged.finalOutputJitterMaxTokens ?? 0}
            min={0}
            suffix="Token"
            onChange={set('finalOutputJitterMaxTokens')}
          />
        </TwoCol>
      </ParameterGroup>
    </div>
  )
}

function NonStreamCacheToggle({
  value,
  fallbackPrefix,
  onChange,
}: {
  value?: ReportedUsagePathPolicy
  fallbackPrefix: string
  onChange: (next: ReportedUsagePathPolicy) => void
}) {
  const policy = value ?? defaultUsagePatch(fallbackPrefix)
  return (
    <TogField
      label="非流式 Usage 透传"
      desc="仅影响非流式请求。开启：所有非流式请求都不整理 usage、不推进本地缓存，外部池保持上游 usage 原样，本地凭证用上游原始 metadata。关闭：带 cache_control 且断点前内容达到模型最小缓存长度的请求按本路径缓存策略整理，其余同开启时透传。"
      checked={Boolean(policy.skipNonStreamUsageProjection)}
      onChange={(skipNonStreamUsageProjection) =>
        onChange({ ...policy, skipNonStreamUsageProjection })
      }
    />
  )
}

export function cachePolicyForStrategyTemplate(policy: CacheRoutePolicyPatch, cacheType: CacheStrategyType): CacheRoutePolicyPatch {
  if (cacheType === 'no_cache') return { cacheType: 'no_cache' }
  // 只透传已配置的值，不补默认：未配置的模板保持关闭且不新增字段。
  const coldRequestGuard = policy.coldRequestGuard ? { coldRequestGuard: policy.coldRequestGuard } : {}
  const cacheTtl = policy.cacheTtl ? { cacheTtl: policy.cacheTtl } : {}
  if (cacheType === 'kiro_rs_tool') {
    return {
      cacheType: 'kiro_rs_tool',
      reportedUsage: policy.reportedUsage ?? defaultUsagePatch('/v1'),
      kiroRsTool: policy.kiroRsTool ?? defaultKiroRsToolPatch(),
      hitShaping: policy.hitShaping ?? defaultCacheHitShaping(),
      ...coldRequestGuard,
      ...cacheTtl,
    }
  }
  if (cacheType === 'kiro_rs_tool_plus') {
    return {
      cacheType: 'kiro_rs_tool_plus',
      kiroRsToolPlus: policy.kiroRsToolPlus ?? defaultKiroRsToolPlusPatch(),
      ...cacheTtl,
    }
  }
  if (cacheType === 'stable_segment_cache') {
    return {
      cacheType: 'stable_segment_cache',
      reportedUsage: policy.reportedUsage ?? defaultUsagePatch('/v1'),
      stableSegment: policy.stableSegment ?? defaultStableSegmentPatch(),
      hitShaping: policy.hitShaping ?? defaultCacheHitShaping(),
      ...coldRequestGuard,
      ...cacheTtl,
    }
  }
  return {
    cacheType: cacheType === 'bounded_high_cache' ? 'bounded_high_cache' : 'current_high_cache',
    simulation: policy.simulation ?? defaultSimulationPatch(),
    creationControl: policy.creationControl ?? defaultPromptCacheCreationControl(),
    reportedUsage: policy.reportedUsage ?? defaultUsagePatch('/v1'),
    hitShaping: policy.hitShaping ?? defaultCacheHitShaping(),
    ...coldRequestGuard,
    ...cacheTtl,
  }
}

function currentHighCachePathDefaults(prefix: string, reportedUsage?: ReportedUsagePathPolicy): CacheRoutePolicyPatch {
  return {
    cacheType: 'current_high_cache',
    routeNamespace: defaultRouteNamespace(prefix),
    simulation: defaultSimulationPatch(),
    creationControl: defaultPromptCacheCreationControl(),
    reportedUsage: reportedUsage ?? defaultUsagePatch(prefix),
    hitShaping: defaultCacheHitShaping(),
  }
}

export function StrategyTemplateEditor({
  title,
  desc,
  showHeader = true,
  cacheType,
  policy,
  onChange,
}: {
  title: string
  desc: string
  showHeader?: boolean
  cacheType: CacheStrategyType
  policy: CacheRoutePolicyPatch
  onChange: (next: CacheRoutePolicyPatch) => void
}) {
  const template = cachePolicyForStrategyTemplate(policy, cacheType)
  const setSimulation = (simulation: CacheSimulationPatch) => onChange({ ...template, simulation })
  const setCreationControl = (creationControl: PromptCacheCreationControlConfig) => onChange({ ...template, creationControl })
  const setReportedUsage = (reportedUsage: ReportedUsagePathPolicy) => onChange({ ...template, reportedUsage })
  const setKiroRsTool = (kiroRsTool: KiroRsToolPatch) => onChange({ ...template, kiroRsTool })
  const setStableSegment = (stableSegment: StableSegmentPatch) => onChange({ ...template, stableSegment })
  const setHitShaping = (hitShaping: CacheHitShapingPatch) => onChange({ ...template, hitShaping })

  return (
    <div className="space-y-8">
      {showHeader && (
        <div className="border-b border-border/70 pb-4">
          <h2 className="text-base font-semibold text-foreground">{title}</h2>
          <div className="mt-1 text-xs leading-5 text-muted-foreground">{desc}</div>
        </div>
      )}
      {isHighCacheStrategy(cacheType) ? (
        <>
          <ParameterGroup title="模拟读取参数">
            <SimulationOverrideForm value={template.simulation ?? defaultSimulationPatch()} onChange={setSimulation} />
          </ParameterGroup>
          <ParameterGroup title="缓存创建展示频次">
            <CreationControlOverrideForm value={template.creationControl ?? defaultPromptCacheCreationControl()} onChange={setCreationControl} />
          </ParameterGroup>
          <PathPolicyEditor policy={template.reportedUsage ?? defaultUsagePatch('/v1')} onChange={setReportedUsage} />
          <ParameterGroup title="命中率与小值整形">
            <CacheHitShapingForm
              value={template.hitShaping ?? defaultCacheHitShaping()}
              bounded={cacheType === 'bounded_high_cache'}
              onChange={setHitShaping}
            />
            <ColdRequestGuardToggle
              value={template.coldRequestGuard}
              onChange={(coldRequestGuard) => onChange({ ...template, coldRequestGuard })}
            />
          </ParameterGroup>
        </>
      ) : cacheType === 'kiro_rs_tool_plus' ? (
        <KiroRsToolPlusPolicyForm
          value={template.kiroRsToolPlus ?? defaultKiroRsToolPlusPatch()}
          onChange={(kiroRsToolPlus) => onChange({ ...template, kiroRsToolPlus })}
        />
      ) : cacheType === 'stable_segment_cache' ? (
        <>
          <ParameterGroup title="非流式请求">
            <NonStreamCacheToggle
              value={template.reportedUsage}
              fallbackPrefix="/v1"
              onChange={setReportedUsage}
            />
          </ParameterGroup>
          <StableSegmentPolicyForm
            value={template.stableSegment ?? defaultStableSegmentPatch()}
            onChange={setStableSegment}
          />
          <ParameterGroup title="命中率与小值整形">
            <CacheHitShapingForm value={template.hitShaping ?? defaultCacheHitShaping()} onChange={setHitShaping} />
            <ColdRequestGuardToggle
              value={template.coldRequestGuard}
              onChange={(coldRequestGuard) => onChange({ ...template, coldRequestGuard })}
            />
          </ParameterGroup>
        </>
      ) : (
        <>
          <ParameterGroup title="非流式请求">
            <NonStreamCacheToggle
              value={template.reportedUsage}
              fallbackPrefix="/v1"
              onChange={setReportedUsage}
            />
          </ParameterGroup>
          <KiroRsToolPolicyForm
            value={template.kiroRsTool ?? defaultKiroRsToolPatch()}
            onChange={setKiroRsTool}
          />
          <ParameterGroup title="命中率与小值整形">
            <CacheHitShapingForm value={template.hitShaping ?? defaultCacheHitShaping()} onChange={setHitShaping} />
            <ColdRequestGuardToggle
              value={template.coldRequestGuard}
              onChange={(coldRequestGuard) => onChange({ ...template, coldRequestGuard })}
            />
          </ParameterGroup>
        </>
      )}
      {cacheType !== 'no_cache' && (
        <CacheTtlPolicyForm
          value={template.cacheTtl}
          onChange={(cacheTtl) => onChange({ ...template, cacheTtl })}
        />
      )}
    </div>
  )
}

export function pathPolicyWithStrategyDefaults(
  cachePolicy: CachePolicyConfig,
  prefix: string,
  policy: CacheRoutePolicyPatch
): CacheRoutePolicyPatch {
  const cacheType = policy.cacheType ?? 'no_cache'
  if (cacheType === 'no_cache') return { cacheType: 'no_cache' }
  if (cacheType === 'kiro_rs_tool_plus') {
    const plusTemplate = cachePolicyForStrategyTemplate(cachePolicy.kiroRsToolPlus ?? {}, 'kiro_rs_tool_plus')
    return {
      ...policy,
      cacheType,
      kiroRsToolPlus: policy.kiroRsToolPlus ?? plusTemplate.kiroRsToolPlus ?? defaultKiroRsToolPlusPatch(),
    }
  }
  const template = cacheType === 'kiro_rs_tool'
    ? cachePolicyForStrategyTemplate(cachePolicy.kiroRsTool ?? {}, 'kiro_rs_tool')
    : cacheType === 'bounded_high_cache'
      ? cachePolicyForStrategyTemplate(cachePolicy.boundedHighCache ?? {}, 'bounded_high_cache')
    : cacheType === 'stable_segment_cache'
      ? cachePolicyForStrategyTemplate(cachePolicy.stableSegmentCache ?? {}, 'stable_segment_cache')
      : cachePolicyForStrategyTemplate(
        {
          ...(cachePolicy.default ?? {}),
          ...(cachePolicy.currentHighCache ?? {}),
        },
        'current_high_cache'
      )
  return {
    ...template,
    ...policy,
    cacheType,
    hitShaping: policy.hitShaping ?? template.hitShaping ?? defaultCacheHitShaping(),
    ...(isHighCacheStrategy(cacheType)
      ? {
          routeNamespace: policy.routeNamespace ?? template.routeNamespace ?? defaultRouteNamespace(prefix),
          simulation: policy.simulation ?? template.simulation ?? defaultSimulationPatch(),
          creationControl: policy.creationControl ?? template.creationControl ?? defaultPromptCacheCreationControl(),
          reportedUsage: policy.reportedUsage ?? template.reportedUsage ?? defaultUsagePatch(prefix),
        }
      : cacheType === 'stable_segment_cache'
        ? {
            routeNamespace: policy.routeNamespace ?? defaultRouteNamespace(prefix),
            reportedUsage: policy.reportedUsage ?? template.reportedUsage ?? defaultUsagePatch(prefix),
            stableSegment: policy.stableSegment ?? template.stableSegment ?? defaultStableSegmentPatch(),
          }
        : {
            reportedUsage: policy.reportedUsage ?? template.reportedUsage ?? defaultUsagePatch(prefix),
            kiroRsTool: policy.kiroRsTool ?? template.kiroRsTool ?? defaultKiroRsToolPatch(),
          }),
  }
}

export function PathStrategyBindingCard({
  prefix,
  policy,
  cachePolicy,
  definedRoutes,
  builtIn,
  showDelete = true,
  onPrefixChange,
  onDelete,
  onChange,
  onDefinedRouteChange,
}: {
  prefix: string
  policy: CacheRoutePolicyPatch
  cachePolicy: CachePolicyConfig
  definedRoutes: string[]
  builtIn?: boolean
  showDelete?: boolean
  onPrefixChange: (nextPrefix: string) => void
  onDelete: () => void
  onChange: (next: CacheRoutePolicyPatch) => void
  onDefinedRouteChange: (enabled: boolean) => void
}) {
  const [draftPrefix, setDraftPrefix] = useState(prefix)
  const [draftDfcacheName, setDraftDfcacheName] = useState(normalizeDefinedCacheRouteName(prefix) ?? '')
  const [prefixError, setPrefixError] = useState<string | null>(null)
  const normalizedDefinedRoute = normalizeDefinedCacheRoute(prefix)
  const isDfcachePath = prefix.toLowerCase().startsWith(DFCACHE_ROUTE_PREFIX)
  const isRouteRegistered = Boolean(normalizedDefinedRoute && definedRoutes.includes(normalizedDefinedRoute))
  const effectiveCacheType: CacheStrategyType = policy.cacheType ?? 'no_cache'
  const effectivePolicy = pathPolicyWithStrategyDefaults(cachePolicy, prefix, policy)

  useEffect(() => {
    setDraftPrefix(prefix)
    setDraftDfcacheName(normalizeDefinedCacheRouteName(prefix) ?? '')
    setPrefixError(null)
  }, [prefix])

  const commitPrefix = () => {
    const normalized = isDfcachePath
      ? buildDefinedCacheRoute(draftDfcacheName)
      : normalizeCachePolicyPath(draftPrefix)
    if (!normalized) {
      setPrefixError(isDfcachePath ? '请输入路径名，例如 team-a' : '路径不能为空')
      setDraftPrefix(prefix)
      setDraftDfcacheName(normalizeDefinedCacheRouteName(prefix) ?? '')
      return
    }
    setPrefixError(null)
    onPrefixChange(normalized)
  }

  const setCacheType = (cacheType: CacheStrategyType) => {
    onChange(sanitizeCacheRoutePolicy(defaultPathCachePatch(prefix, cacheType)))
  }

  const patch = (next: Partial<CacheRoutePolicyPatch>) => {
    onChange({ ...effectivePolicy, ...next, cacheType: effectiveCacheType })
  }

  return (
    <div className="min-w-0">
        <div className="mb-7 flex items-start justify-between gap-4 border-b border-border/60 pb-4">
          <div>
            <div className="text-[10px] font-semibold uppercase tracking-[0.16em] text-muted-foreground">编辑路径策略</div>
            <div className="mt-1 font-mono text-sm font-semibold text-foreground">{prefix}</div>
          </div>
          <div className="shrink-0 text-right text-[11px] leading-5 text-muted-foreground">
            <div>完整编辑</div>
            <div className="hidden sm:block">修改后统一保存</div>
          </div>
        </div>
        <div className="space-y-9">
        <EditorSection
          id="path-scope"
          title="路径范围"
          desc="先确定这条规则匹配哪些请求，再决定是否暴露为 /dfcache 入口。"
        >
          <div className="grid gap-7 xl:grid-cols-[minmax(0,1.25fr)_minmax(17rem,0.75fr)]">
            <div className="min-w-0 space-y-2">
              <div className="text-[13px] font-medium text-foreground">{isDfcachePath ? '自定义路径后缀' : '路径前缀'}</div>
              {builtIn ? (
                <div className="max-w-sm rounded-md border border-border/70 px-3 py-2 font-mono text-sm text-foreground">{prefix}</div>
              ) : isDfcachePath ? (
                <div className="flex max-w-sm items-stretch overflow-hidden rounded-md border border-input bg-card">
                  <div className="flex items-center border-r border-input bg-muted/30 px-3 font-mono text-sm text-muted-foreground">
                    {DFCACHE_ROUTE_PREFIX}
                  </div>
                  <Input
                    className="min-w-0 border-0 shadow-none focus-visible:ring-0"
                    value={draftDfcacheName}
                    onChange={(event) => setDraftDfcacheName(event.target.value)}
                    onBlur={commitPrefix}
                    onKeyDown={(event) => {
                      if (event.key === 'Enter') event.currentTarget.blur()
                      if (event.key === 'Escape') {
                        setDraftDfcacheName(normalizeDefinedCacheRouteName(prefix) ?? '')
                        setPrefixError(null)
                      }
                    }}
                  />
                </div>
              ) : (
                <Input
                  className="max-w-sm"
                  value={draftPrefix}
                  onChange={(event) => setDraftPrefix(event.target.value)}
                  onBlur={commitPrefix}
                  onKeyDown={(event) => {
                    if (event.key === 'Enter') event.currentTarget.blur()
                    if (event.key === 'Escape') {
                      setDraftPrefix(prefix)
                      setPrefixError(null)
                    }
                  }}
                />
              )}
              <div className="max-w-sm text-xs leading-5 text-muted-foreground">
                {isDfcachePath
                  ? '固定前缀用于避开内置路由，只能修改后缀名。'
                  : '请求路径以 ' + cacheEndpointLabel(prefix) + ' 开头时使用这条规则；多个规则都能匹配时，用最长的那个。'}
              </div>
              {prefixError && <div className="text-xs text-destructive">{prefixError}</div>}
            </div>
            {isDfcachePath && (
              <div className="border-t border-border/60 pt-5 xl:border-l xl:border-t-0 xl:pl-6 xl:pt-0">
                <TogField
                  label="注册为 /dfcache 路由"
                  desc={normalizedDefinedRoute ? '开启后允许客户端访问这个 /dfcache/{name} 入口。' : '路径必须是 /dfcache/{name}，name 仅允许小写字母、数字、点、下划线或短横线。'}
                  checked={isRouteRegistered}
                  disabled={!normalizedDefinedRoute}
                  onChange={onDefinedRouteChange}
                />
              </div>
            )}
          </div>
          {!builtIn && showDelete && (
            <div className="mt-6 flex justify-start border-t border-border/60 pt-4">
              <Button type="button" variant="ghost" size="sm" className="px-0 text-destructive hover:bg-transparent hover:text-destructive" onClick={onDelete}>
                <Trash2 className="mr-1 h-4 w-4" />
                删除路径
              </Button>
            </div>
          )}
        </EditorSection>

        <EditorSection
          id="path-strategy"
          className="border-t border-border/70 pt-8"
          title="缓存策略"
          desc="这里是唯一的决策区。切换策略会载入对应的默认参数，下面只展示当前策略的设置。"
        >
          <StrategyChoice value={effectiveCacheType} onChange={setCacheType} />
        </EditorSection>

        <EditorSection
          id="path-parameters"
          className="border-t border-border/70 pt-8"
          title="策略参数"
          desc="根据上面的策略选择显示当前实现真正需要的参数。"
        >
          {effectiveCacheType === 'no_cache' ? (
            <div className="max-w-2xl border-l-2 border-muted-foreground/40 pl-4 text-sm leading-6 text-muted-foreground">
              这个路径直接走无缓存逻辑，不进入缓存计算，也不展示缓存参数。
            </div>
          ) : isHighCacheStrategy(effectiveCacheType) ? (
            <div className="space-y-7">
              <ParameterGroup title="缓存空间" desc="决定本路径是否使用独立的本地模拟缓存状态。">
                <TogField
                  label="独立路径缓存空间"
                  desc="开启后，这个路径的本地模拟缓存读取和写入按路径独立统计；关闭后与同一凭据、会话、模型的默认缓存空间共享。"
                  checked={Boolean(effectivePolicy.routeNamespace)}
                  onChange={(routeNamespace) => patch({ routeNamespace })}
                />
              </ParameterGroup>
              <ParameterGroup title="模拟读取参数" desc="控制本地如何模拟缓存命中和展示输入。">
                <SimulationOverrideForm
                  value={effectivePolicy.simulation ?? defaultSimulationPatch()}
                  onChange={(simulation) => patch({ simulation })}
                />
              </ParameterGroup>
              <ParameterGroup title="缓存创建展示频次" desc="限制对外展示缓存写入的频率，不改变真实缓存创建。">
                <CreationControlOverrideForm
                  value={effectivePolicy.creationControl ?? defaultPromptCacheCreationControl()}
                  onChange={(creationControl) => patch({ creationControl })}
                />
              </ParameterGroup>
              <PathPolicyEditor
                policy={effectivePolicy.reportedUsage ?? defaultUsagePatch(prefix)}
                onChange={(reportedUsage) => patch({ reportedUsage })}
              />
              <ParameterGroup title="命中率与小值整形" desc="最后处理命中率、小值和冷请求边界。">
                <CacheHitShapingForm
                  value={effectivePolicy.hitShaping ?? defaultCacheHitShaping()}
                  bounded={effectiveCacheType === 'bounded_high_cache'}
                  onChange={(hitShaping) => patch({ hitShaping })}
                />
                <ColdRequestGuardToggle
                  value={effectivePolicy.coldRequestGuard}
                  onChange={(coldRequestGuard) => patch({ coldRequestGuard })}
                />
              </ParameterGroup>
            </div>
          ) : effectiveCacheType === 'kiro_rs_tool_plus' ? (
            <KiroRsToolPlusPolicyForm
              value={effectivePolicy.kiroRsToolPlus ?? defaultKiroRsToolPlusPatch()}
              onChange={(kiroRsToolPlus) => patch({ kiroRsToolPlus })}
            />
          ) : effectiveCacheType === 'stable_segment_cache' ? (
            <div className="space-y-7">
              <ParameterGroup title="缓存空间" desc="决定这条路径是否拥有独立的稳定片段缓存空间。">
                <TogField
                  label="独立路径缓存空间"
                  desc="开启后这个路径的缓存只在本路径内读取和写入；关闭后与其他未开启的路径共享同一会话的缓存。"
                  checked={Boolean(effectivePolicy.routeNamespace)}
                  onChange={(routeNamespace) => patch({ routeNamespace })}
                />
              </ParameterGroup>
              <ParameterGroup title="非流式请求" desc="决定非流式响应是否直接使用上游 usage。">
                <NonStreamCacheToggle
                  value={effectivePolicy.reportedUsage}
                  fallbackPrefix={prefix}
                  onChange={(reportedUsage) => patch({ reportedUsage })}
                />
              </ParameterGroup>
              <StableSegmentPolicyForm
                value={effectivePolicy.stableSegment ?? defaultStableSegmentPatch()}
                onChange={(stableSegment) => patch({ stableSegment })}
              />
              <ParameterGroup title="命中率与小值整形" desc="最后处理命中率、小值和冷请求边界。">
                <CacheHitShapingForm
                  value={effectivePolicy.hitShaping ?? defaultCacheHitShaping()}
                  onChange={(hitShaping) => patch({ hitShaping })}
                />
                <ColdRequestGuardToggle
                  value={effectivePolicy.coldRequestGuard}
                  onChange={(coldRequestGuard) => patch({ coldRequestGuard })}
                />
              </ParameterGroup>
            </div>
          ) : (
            <div className="space-y-7">
              <ParameterGroup title="非流式请求" desc="决定非流式响应是否直接使用上游 usage。">
                <NonStreamCacheToggle
                  value={effectivePolicy.reportedUsage}
                  fallbackPrefix={prefix}
                  onChange={(reportedUsage) => patch({ reportedUsage })}
                />
              </ParameterGroup>
              <ParameterGroup title="稳定前缀规则" desc="控制 Kiro-RS Tool 本轮最多追踪和创建多少稳定内容。">
                <KiroRsToolPolicyForm
                  value={effectivePolicy.kiroRsTool ?? defaultKiroRsToolPatch()}
                  onChange={(kiroRsTool) => patch({ kiroRsTool })}
                />
              </ParameterGroup>
              <ParameterGroup title="命中率与小值整形" desc="最后处理命中率、小值和冷请求边界。">
                <CacheHitShapingForm
                  value={effectivePolicy.hitShaping ?? defaultCacheHitShaping()}
                  onChange={(hitShaping) => patch({ hitShaping })}
                />
                <ColdRequestGuardToggle
                  value={effectivePolicy.coldRequestGuard}
                  onChange={(coldRequestGuard) => patch({ coldRequestGuard })}
                />
              </ParameterGroup>
            </div>
          )}
          {effectiveCacheType !== 'no_cache' && (
            <div className="mt-7">
              <CacheTtlPolicyForm
                value={effectivePolicy.cacheTtl}
                onChange={(cacheTtl) => patch({ cacheTtl })}
              />
            </div>
          )}
        </EditorSection>
        </div>
    </div>
  )
}

export function CachePolicySettingsSection({
  config,
  onChange,
}: {
  config: RuntimeConfig
  onChange: (next: RuntimeConfig) => void
}) {
  const [newPath, setNewPath] = useState('')
  const [error, setError] = useState<string | null>(null)
  const cachePolicy = config.cachePolicy
  const paths = Array.from(new Set([
    ...BUILT_IN_CACHE_PREFIXES,
    ...Object.keys(cachePolicy.pathOverrides ?? {}).map(canonicalCachePolicyPath),
    ...Object.keys(config.reportedUsage.pathOverrides ?? {}).map(canonicalCachePolicyPath),
    ...config.definedCacheRoutes.map(canonicalCachePolicyPath),
  ])).sort(compareCachePrefix)

  const updateCachePolicy = (
    nextCachePolicy: CachePolicyConfig,
    nextReportedUsage = config.reportedUsage,
    nextDefinedRoutes = config.definedCacheRoutes
  ) => {
    onChange({
      ...config,
      cachePolicy: nextCachePolicy,
      reportedUsage: nextReportedUsage,
      definedCacheRoutes: nextDefinedRoutes,
    })
  }

  const mergedPolicyForPath = (prefix: string): CacheRoutePolicyPatch => {
    const normalizedPrefix = canonicalCachePolicyPath(prefix)
    const existing = routeOverrideForPrefix(cachePolicy.pathOverrides, normalizedPrefix)
    const legacyReportedUsage = reportedUsageForPrefix(config.reportedUsage.pathOverrides, normalizedPrefix)
    if (existing) {
      if (existing.cacheType === 'no_cache' || existing.cacheType === 'kiro_rs_tool' || existing.cacheType === 'kiro_rs_tool_plus' || existing.cacheType === 'stable_segment_cache' || existing.cacheType === 'bounded_high_cache') {
        return existing
      }
      return legacyReportedUsage ? { ...existing, reportedUsage: existing.reportedUsage ?? legacyReportedUsage } : existing
    }
    if (legacyReportedUsage) {
      return currentHighCachePathDefaults(normalizedPrefix, legacyReportedUsage)
    }
    if (isBuiltInCachePrefix(normalizedPrefix)) {
      return defaultPathCachePatch(normalizedPrefix, builtinDefaultCacheType(normalizedPrefix))
    }
    const normalizedRoute = normalizeDefinedCacheRoute(normalizedPrefix)
    if (normalizedRoute && config.definedCacheRoutes.includes(normalizedRoute)) {
      return currentHighCachePathDefaults(normalizedPrefix)
    }
    return { cacheType: 'no_cache' }
  }

  const setPathPolicy = (prefix: string, nextPolicy: CacheRoutePolicyPatch) => {
    const normalizedPrefix = canonicalCachePolicyPath(prefix)
    const pathOverrides = { ...(cachePolicy.pathOverrides ?? {}) }
    const reportedPathOverrides = { ...config.reportedUsage.pathOverrides }
    deletePrefixAliases(pathOverrides, normalizedPrefix)
    deletePrefixAliases(reportedPathOverrides, normalizedPrefix)
    pathOverrides[normalizedPrefix] = sanitizeCacheRoutePolicy(nextPolicy)
    updateCachePolicy(
      { ...cachePolicy, pathOverrides },
      { ...config.reportedUsage, pathOverrides: reportedPathOverrides }
    )
  }

  const addPath = () => {
    const prefix = buildDefinedCacheRoute(newPath)
    if (!prefix) {
      setError('请输入路径名，例如 team-a')
      return
    }
    if (paths.includes(prefix)) {
      setError(`${prefix} 已存在`)
      return
    }
    setError(null)
    setNewPath('')
    updateCachePolicy(
      {
        ...cachePolicy,
        pathOverrides: {
          ...(cachePolicy.pathOverrides ?? {}),
          [prefix]: { cacheType: 'no_cache' },
        },
      },
      config.reportedUsage,
      normalizedDefinedRoutesWith(config.definedCacheRoutes, prefix, Boolean(normalizeDefinedCacheRoute(prefix)))
    )
  }

  const renamePath = (oldPrefix: string, nextPrefix: string) => {
    const normalizedOld = canonicalCachePolicyPath(oldPrefix)
    const normalizedNext = canonicalCachePolicyPath(nextPrefix)
    if (normalizedOld === normalizedNext) return
    if (paths.includes(normalizedNext)) {
      setError(`${nextPrefix} 已存在`)
      return
    }
    const pathOverrides = { ...(cachePolicy.pathOverrides ?? {}) }
    const reportedPathOverrides = { ...config.reportedUsage.pathOverrides }
    const policy = mergedPolicyForPath(normalizedOld)
    deletePrefixAliases(pathOverrides, normalizedOld)
    deletePrefixAliases(reportedPathOverrides, normalizedOld)
    pathOverrides[normalizedNext] = policy.cacheType === 'no_cache' ? { cacheType: 'no_cache' } : policy
    setError(null)
    updateCachePolicy(
      { ...cachePolicy, pathOverrides },
      { ...config.reportedUsage, pathOverrides: reportedPathOverrides },
      moveDefinedRoute(config.definedCacheRoutes, normalizedOld, normalizedNext)
    )
  }

  const deletePath = (prefix: string) => {
    const normalizedPrefix = canonicalCachePolicyPath(prefix)
    const pathOverrides = { ...(cachePolicy.pathOverrides ?? {}) }
    const reportedPathOverrides = { ...config.reportedUsage.pathOverrides }
    deletePrefixAliases(pathOverrides, normalizedPrefix)
    deletePrefixAliases(reportedPathOverrides, normalizedPrefix)
    updateCachePolicy(
      { ...cachePolicy, pathOverrides },
      { ...config.reportedUsage, pathOverrides: reportedPathOverrides },
      normalizedDefinedRoutesWith(config.definedCacheRoutes, normalizedPrefix, false)
    )
  }

  const setDefinedRoute = (prefix: string, enabled: boolean) => {
    const normalizedPrefix = canonicalCachePolicyPath(prefix)
    updateCachePolicy(cachePolicy, config.reportedUsage, normalizedDefinedRoutesWith(config.definedCacheRoutes, normalizedPrefix, enabled))
  }

  const setCurrentTemplate = (next: CacheRoutePolicyPatch) => {
    updateCachePolicy({
      ...cachePolicy,
      currentHighCache: next,
      default: { ...cachePolicy.default, ...next },
    })
  }

  const setKiroTemplate = (next: CacheRoutePolicyPatch) => {
    updateCachePolicy({ ...cachePolicy, kiroRsTool: next })
  }

  const currentTemplate = cachePolicyForStrategyTemplate(
    { ...(cachePolicy.default ?? {}), ...(cachePolicy.currentHighCache ?? {}) },
    'current_high_cache'
  )
  const setStableSegmentTemplate = (next: CacheRoutePolicyPatch) => {
    updateCachePolicy({ ...cachePolicy, stableSegmentCache: next })
  }

  const kiroTemplate = cachePolicyForStrategyTemplate(cachePolicy.kiroRsTool ?? {}, 'kiro_rs_tool')
  const kiroPlusTemplate = cachePolicyForStrategyTemplate(cachePolicy.kiroRsToolPlus ?? {}, 'kiro_rs_tool_plus')
  const setKiroPlusTemplate = (next: CacheRoutePolicyPatch) => {
    updateCachePolicy({ ...cachePolicy, kiroRsToolPlus: next })
  }
  const stableSegmentTemplate = cachePolicyForStrategyTemplate(cachePolicy.stableSegmentCache ?? {}, 'stable_segment_cache')
  const boundedTemplate = cachePolicyForStrategyTemplate(cachePolicy.boundedHighCache ?? {}, 'bounded_high_cache')
  const setBoundedTemplate = (next: CacheRoutePolicyPatch) => {
    updateCachePolicy({ ...cachePolicy, boundedHighCache: next })
  }

  return (
    <div className="space-y-6">
      <div className="grid gap-4">
        <StrategyTemplateEditor
          title="本地模拟缓存策略默认参数"
          desc="使用本策略的路径会先读取这里的参数，再合并路径自己的参数。"
          cacheType="current_high_cache"
          policy={currentTemplate}
          onChange={setCurrentTemplate}
        />
        <StrategyTemplateEditor
          title="Kiro-RS Tool 缓存策略默认参数"
          desc="使用本策略的路径只读取这里属于 Kiro-RS Tool 的参数，不读取本地模拟策略参数。"
          cacheType="kiro_rs_tool"
          policy={kiroTemplate}
          onChange={setKiroTemplate}
        />
        <StrategyTemplateEditor
          title="稳定片段缓存策略默认参数"
          desc="使用本策略的路径只读取这里属于稳定片段缓存的参数，再合并路径自己的参数。"
          cacheType="stable_segment_cache"
          policy={stableSegmentTemplate}
          onChange={setStableSegmentTemplate}
        />
        <StrategyTemplateEditor
          title="Kiro-RS Tool Plus 缓存策略默认参数"
          desc="使用本策略且没有单独配置参数的路径读取这里的参数。可分别控制输入、输出、缓存读取、缓存写入的放大倍数和上下限，并限制写入频次。"
          cacheType="kiro_rs_tool_plus"
          policy={kiroPlusTemplate}
          onChange={setKiroPlusTemplate}
        />
        <StrategyTemplateEditor
          title="上下限优先缓存策略默认参数"
          desc="参数与本地模拟缓存相同，但不读取本地模拟策略的模板。各字段上下限是最终约束，计费只在范围内补，补不上的差额在之后放行写入的请求上补。"
          cacheType="bounded_high_cache"
          policy={boundedTemplate}
          onChange={setBoundedTemplate}
        />
      </div>

      <div className="space-y-4">
        <div>
          <div className="text-sm font-semibold">路径绑定</div>
          <div className="mt-1 text-xs leading-5 text-muted-foreground">
            每个路径都显式选择无缓存、本地模拟缓存策略、上下限优先缓存策略、Kiro-RS Tool 缓存策略、Kiro-RS Tool Plus 缓存策略或稳定片段缓存策略。
          </div>
        </div>
        <div className="flex flex-col gap-2 md:flex-row md:items-end">
          <div className="min-w-0 flex-1 space-y-1.5">
            <div className="text-sm font-semibold">新增自定义路径</div>
            <div className="text-xs leading-5 text-muted-foreground">
              /dfcache/ 是固定前缀，用来和内置路径分开，不能修改。
            </div>
            <div className="flex items-stretch overflow-hidden rounded-md border border-input bg-background">
              <div className="flex items-center border-r border-input bg-muted/30 px-3 font-mono text-sm text-muted-foreground">
                {DFCACHE_ROUTE_PREFIX}
              </div>
              <Input
                className="border-0 shadow-none focus-visible:ring-0"
                placeholder="team-a"
                value={newPath}
                onChange={(event) => setNewPath(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === 'Enter') addPath()
                }}
              />
            </div>
            <div className="text-xs text-muted-foreground">
              这里只填后缀，例如 team-a，最终路径是 /dfcache/team-a。
            </div>
          </div>
          <Button type="button" onClick={addPath}>
            <Plus className="mr-1 h-4 w-4" />
            新增路径
          </Button>
        </div>
        {error && <div className="text-xs text-destructive">{error}</div>}
        <div className="space-y-4">
          {paths.map((prefix) => (
            <PathStrategyBindingCard
              key={prefix}
              prefix={prefix}
              policy={mergedPolicyForPath(prefix)}
              cachePolicy={cachePolicy}
              definedRoutes={config.definedCacheRoutes}
              builtIn={isBuiltInCachePrefix(prefix)}
              onPrefixChange={(nextPrefix) => renamePath(prefix, nextPrefix)}
              onDelete={() => deletePath(prefix)}
              onChange={(nextPolicy) => setPathPolicy(prefix, nextPolicy)}
              onDefinedRouteChange={(enabled) => setDefinedRoute(prefix, enabled)}
            />
          ))}
        </div>
      </div>

      <div className="space-y-4">
        <div>
          <div className="text-sm font-semibold">统计展示</div>
          <div className="mt-1 text-xs leading-5 text-muted-foreground">
            只影响后台列表里是否标记为缓存命中较高，不改变请求处理，也不改变返回给客户端的用量数字。
          </div>
        </div>
        <TwoCol>
          <NumField
            label="缓存命中判定阈值"
            desc="缓存读取达到多少 Token 后，在后台统计里认为这次请求缓存命中较高。"
            value={config.highCacheThreshold}
            min={0}
            suffix="Token"
            onChange={(highCacheThreshold) => onChange({ ...config, highCacheThreshold })}
          />
        </TwoCol>
      </div>
    </div>
  )
}

// ─── 旧内容清理(payloadHistory) ───────────────────────────────────────────────

export function PayloadHistorySection({
  shaping, payloadSizeLimitEnabled, onChange,
}: {
  shaping: PayloadShapingConfig
  payloadSizeLimitEnabled: boolean
  onChange: (next: PayloadShapingConfig) => void
}) {
  const set = <K extends keyof PayloadShapingConfig>(key: K) => (v: PayloadShapingConfig[K]) =>
    onChange({ ...shaping, [key]: v })
  // 子字段仅在 payloadSizeLimitEnabled && shaping.enabled 时可编辑
  const branchEnabled = payloadSizeLimitEnabled && shaping.enabled
  return (
    <div className="space-y-4">
      <TogField
        label="启用内容清理"
        desc="请求太大时，优先清理较早的历史消息，尽量保留当前这一轮的内容。"
        checked={shaping.enabled}
        disabled={!payloadSizeLimitEnabled}
        onChange={set('enabled')}
      />
      <TwoCol>
        <TogField
          label="截短历史工具结果"
          desc="历史里的工具输出太长时只保留开头和结尾，中间省略。"
          checked={shaping.truncateHistoricalToolResults}
          disabled={!branchEnabled}
          onChange={set('truncateHistoricalToolResults')}
        />
        <NumField
          label="历史工具结果保留字符"
          desc="每条历史工具结果最多保留多少字符；值越小，请求越短，但丢失的信息也越多。"
          value={shaping.historicalToolResultMaxChars}
          min={0}
          suffix="字符"
          disabled={!branchEnabled}
          onChange={set('historicalToolResultMaxChars')}
        />
      </TwoCol>
      <TwoCol>
        <NumField
          label="保留头部行数"
          desc="截短工具结果时，开头最多保留多少行。"
          value={shaping.historicalToolResultHeadLines}
          min={0}
          suffix="行"
          disabled={!branchEnabled}
          onChange={set('historicalToolResultHeadLines')}
        />
        <NumField
          label="保留尾部行数"
          desc="截短工具结果时，结尾最多保留多少行。"
          value={shaping.historicalToolResultTailLines}
          min={0}
          suffix="行"
          disabled={!branchEnabled}
          onChange={set('historicalToolResultTailLines')}
        />
      </TwoCol>
      <TwoCol>
        <TogField
          label="移除历史思考内容"
          desc="移除旧消息里的思考内容，避免它们反复占用请求体积。"
          checked={shaping.discardHistoricalThinking}
          disabled={!branchEnabled}
          onChange={set('discardHistoricalThinking')}
        />
        <TogField
          label="压缩工具说明"
          desc="工具说明太大时精简描述和参数说明，减少发送体积。"
          checked={shaping.compressToolDefinitions}
          disabled={!branchEnabled}
          onChange={set('compressToolDefinitions')}
        />
      </TwoCol>
      <TwoCol>
        <NumField
          label="工具说明大小上限"
          desc="所有工具说明合计尽量控制在这个大小以内；越小越省体积，但工具说明会更短。"
          value={shaping.toolDefinitionsBudgetBytes}
          min={0}
          suffix="字节"
          disabled={!branchEnabled}
          onChange={set('toolDefinitionsBudgetBytes')}
        />
        <NumField
          label="单工具描述上限"
          desc="单个工具的文字描述最多保留多少字符。"
          value={shaping.toolDescriptionMaxChars}
          min={0}
          suffix="字符"
          disabled={!branchEnabled}
          onChange={set('toolDescriptionMaxChars')}
        />
      </TwoCol>
      <TwoCol>
        <NumField
          label="工具参数说明上限"
          desc="单个工具参数里的说明文字最多保留多少字符。"
          value={shaping.toolSchemaAnnotationMaxChars}
          min={0}
          suffix="字符"
          disabled={!branchEnabled}
          onChange={set('toolSchemaAnnotationMaxChars')}
        />
        <TogField
          label="清理网页抓取历史"
          desc="历史里的网页抓取正文太长时截短。"
          checked={shaping.webFetchTrimEnabled}
          disabled={!branchEnabled}
          onChange={set('webFetchTrimEnabled')}
        />
      </TwoCol>
      <TwoCol>
        <NumField
          label="网页抓取正文保留字符"
          desc="每段网页抓取正文最多保留多少字符。"
          value={shaping.webFetchBodyMaxChars}
          min={0}
          suffix="字符"
          disabled={!branchEnabled}
          onChange={set('webFetchBodyMaxChars')}
        />
      </TwoCol>
    </div>
  )
}

// ─── 当前内容兜底(payloadFallback) ────────────────────────────────────────────

export function PayloadFallbackSection({
  shaping, payloadShapingBranchEnabled, onChange,
}: {
  shaping: PayloadShapingConfig
  payloadShapingBranchEnabled: boolean
  onChange: (next: PayloadShapingConfig) => void
}) {
  const set = <K extends keyof PayloadShapingConfig>(key: K) => (v: PayloadShapingConfig[K]) =>
    onChange({ ...shaping, [key]: v })
  const dis = !payloadShapingBranchEnabled
  return (
    <div className="space-y-4">
      <TogField
        label="自动压缩当前内容"
        desc="当前请求仍然超限时，才会按下面规则处理本轮内容。"
        checked={shaping.fitCurrentPayloadToBudget}
        disabled={dis}
        onChange={set('fitCurrentPayloadToBudget')}
      />
      <TwoCol>
        <TogField
          label="截短当前工具结果"
          desc="本轮工具输出太长时截短，减少本次请求体积。"
          checked={shaping.truncateCurrentToolResults}
          disabled={dis}
          onChange={set('truncateCurrentToolResults')}
        />
        <NumField
          label="当前工具结果保留字符"
          desc="本轮每条工具结果最多保留多少字符。"
          value={shaping.currentToolResultMaxChars}
          min={0}
          suffix="字符"
          disabled={dis}
          onChange={set('currentToolResultMaxChars')}
        />
      </TwoCol>
      <TwoCol>
        <TogField
          label="截短当前用户文本"
          desc="用户本轮输入太长时截短；可能会损失本轮请求的细节。"
          checked={shaping.truncateCurrentUserContent}
          disabled={dis}
          onChange={set('truncateCurrentUserContent')}
        />
        <NumField
          label="当前用户文本保留字符"
          desc="本轮用户文本最多保留多少字符。"
          value={shaping.currentUserContentMaxChars}
          min={0}
          suffix="字符"
          disabled={dis}
          onChange={set('currentUserContentMaxChars')}
        />
      </TwoCol>
      <TwoCol>
        <TogField
          label="截短当前文档"
          desc="本轮带上的文档内容太长时截短。"
          checked={shaping.truncateCurrentDocuments}
          disabled={dis}
          onChange={set('truncateCurrentDocuments')}
        />
        <NumField
          label="当前文档保留字符"
          desc="本轮每份文档最多保留多少字符。"
          value={shaping.currentDocumentMaxChars}
          min={0}
          suffix="字符"
          disabled={dis}
          onChange={set('currentDocumentMaxChars')}
        />
      </TwoCol>
      <TwoCol>
        <TogField
          label="移除当前图片"
          desc="本轮图片整体太大时，移除超过限制的图片内容。"
          checked={shaping.truncateCurrentImages}
          disabled={dis}
          onChange={set('truncateCurrentImages')}
        />
        <NumField
          label="当前图片保留大小"
          desc="本轮图片合计尽量控制在这个大小以内。"
          value={shaping.currentImagesMaxBytes}
          min={0}
          suffix="字节"
          disabled={dis}
          onChange={set('currentImagesMaxBytes')}
        />
      </TwoCol>
      <div className="space-y-1.5">
        <div className="text-sm font-semibold text-foreground">单图超 5MB 处理</div>
        <div className="text-xs leading-relaxed text-muted-foreground">单张图片超过 Claude 接口限制时，选择移除并给模型占位说明，或直接返回请求错误。</div>
        <Select
          value={shaping.oversizedImageHandling ?? 'drop-with-placeholder'}
          disabled={dis}
          onValueChange={(v) => set('oversizedImageHandling')(v as PayloadShapingConfig['oversizedImageHandling'])}
        >
          <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value="drop-with-placeholder">移除图片并占位</SelectItem>
            <SelectItem value="reject">直接报错</SelectItem>
          </SelectContent>
        </Select>
      </div>
    </div>
  )
}

// ─── 缓存创建频次(cacheCreate) ────────────────────────────────────────────────

export function CacheCreationSection({
  control, onChange,
}: {
  control: PromptCacheCreationControlConfig
  onChange: (next: PromptCacheCreationControlConfig) => void
}) {
  const set = <K extends keyof PromptCacheCreationControlConfig>(key: K) => (v: PromptCacheCreationControlConfig[K]) =>
    onChange({ ...control, [key]: v })
  return (
    <div className="space-y-4">
      <TogField
        label="启用缓存创建展示频次控制"
        desc="控制什么时候显示缓存写入数字，避免每次成功请求都显示写入；只影响对外显示的用量。"
        checked={control.enabled}
        onChange={set('enabled')}
      />
      <TwoCol>
        <NumField label="最小成功请求间隔" desc="两次显示缓存写入之间至少隔多少次成功请求；0 表示不按次数限制。" value={control.minSuccessfulRequestsBetweenCreation} min={0} suffix="次" disabled={!control.enabled} onChange={set('minSuccessfulRequestsBetweenCreation')} />
        <NumField label="最小时间间隔" desc="两次显示缓存写入之间至少间隔多久；0 表示不按时间限制。" value={control.minCreationIntervalSecs} min={0} suffix="秒" disabled={!control.enabled} onChange={set('minCreationIntervalSecs')} />
      </TwoCol>
      <TwoCol>
        <NumField label="最小累计增量" desc="累计新增输入达到多少 Token 后，才允许再次显示缓存写入；0 表示不限制。" value={control.minCreationDeltaTokens} min={0} suffix="Token" disabled={!control.enabled} onChange={set('minCreationDeltaTokens')} />
        <NumField label="单次展示上限" desc="一次响应里最多显示多少缓存写入 Token；0 表示不限制。" value={control.maxCreationTokensPerEvent} min={0} suffix="Token" disabled={!control.enabled} onChange={set('maxCreationTokensPerEvent')} />
      </TwoCol>
      <TwoCol>
        <NumField label="额度窗口长度" desc="按多长时间统计一次缓存写入展示额度；0 表示关闭窗口额度控制。" value={control.creationBudgetWindowSecs} min={0} suffix="秒" disabled={!control.enabled} onChange={set('creationBudgetWindowSecs')} />
        <NumField label="窗口展示额度" desc="一个时间窗口内最多显示多少缓存写入 Token；0 表示不限制。" value={control.maxCreationTokensPerWindow} min={0} suffix="Token" disabled={!control.enabled} onChange={set('maxCreationTokensPerWindow')} />
      </TwoCol>
      <TwoCol>
        <NumField label="空闲后清理状态" desc="这个路径多久没有新请求后清理临时计数，减少长期占用；0 表示不按空闲时间清理。" value={control.expireAfterIdleSecs} min={0} suffix="秒" disabled={!control.enabled} onChange={set('expireAfterIdleSecs')} />
      </TwoCol>
    </div>
  )
}

// ─── 用量上报字段策略 ─────────────────────────────────────────────────────────

const FIELD_MODE_LABELS: Record<string, string> = {
  raw: '使用原始值',
  preserve: '使用当前值',
  'sample-max': '压到上限内',
  'sample-target': '压到目标附近',
}

const FIELD_MODE_DESCS: Record<string, string> = {
  raw: '直接使用上游原始返回的这个数字。',
  preserve: '保留前面缓存策略已经算出来的这个数字。',
  'sample-max': '当这个数字超过上限时，改成一个不超过上限的展示值。',
  'sample-target': '把这个数字压到目标值附近；目标值越大，显示出来的数字通常越高。',
}

function fieldNeedsMax(policy: ReportedUsageFieldPolicy): boolean {
  return policy.mode === 'sample-max'
}

function fieldNeedsTarget(policy: ReportedUsageFieldPolicy): boolean {
  return policy.mode === 'sample-target'
}

function FieldPolicyEditor({
  title, policy, allowMoveDelta, onChange,
}: {
  title: string
  policy: ReportedUsageFieldPolicy
  allowMoveDelta?: boolean
  onChange: (next: ReportedUsageFieldPolicy) => void
}) {
  const set = <K extends keyof ReportedUsageFieldPolicy>(key: K) => (v: ReportedUsageFieldPolicy[K]) =>
    onChange({ ...policy, [key]: v })
  return (
    <div className="min-w-0 space-y-4 border-t border-border/60 pt-4">
      <div className="flex flex-col gap-2 sm:flex-row sm:items-start sm:justify-between">
        <div className="min-w-0">
          <div className="text-sm font-semibold">{title}</div>
          <div className="mt-1 text-xs leading-5 text-muted-foreground">{FIELD_MODE_DESCS[policy.mode]}</div>
        </div>
        <Select value={policy.mode} onValueChange={(v) => set('mode')(v as ReportedUsageFieldPolicy['mode'])}>
          <SelectTrigger size="sm" className="w-full max-w-36 shrink-0"><SelectValue /></SelectTrigger>
          <SelectContent>
            {Object.entries(FIELD_MODE_LABELS).map(([k, label]) => (
              <SelectItem key={k} value={k}>{label}</SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
      {fieldNeedsMax(policy) && (
        <NumField
          label="封顶 Token"
          desc="这个字段最多显示多少 Token；0 表示不改写。"
          value={policy.maxTokens}
          min={0}
          suffix="Token"
          onChange={set('maxTokens')}
        />
      )}
      {fieldNeedsTarget(policy) && (
        <TwoCol>
          <NumField
            label="目标 Token"
            desc="希望这个字段大致显示到多少 Token 附近；0 表示不改写。"
            value={policy.targetTokens}
            min={0}
            suffix="Token"
            onChange={set('targetTokens')}
          />
          <NumField
            label="常规上限倍数"
            desc="正常情况下最多显示到目标值的多少倍；例如 1.2 表示最多约 120%。"
            value={policy.normalMaxMultiplier}
            min={0}
            step={0.1}
            suffix="倍"
            onChange={set('normalMaxMultiplier')}
          />
        </TwoCol>
      )}
      {allowMoveDelta && (
        <TogField
          label="差额计入缓存读取"
          desc="如果输入显示被压低，且响应已有缓存读取证据，少掉的部分加到缓存读取里；没有读取证据时差额计入缓存写入，不伪造缓存读取。"
          checked={policy.moveDeltaToCacheRead}
          disabled={policy.mode === 'preserve' || policy.mode === 'raw'}
          onChange={set('moveDeltaToCacheRead')}
        />
      )}
    </div>
  )
}

function FinalOutputGuardForm({
  policy,
  onChange,
}: {
  policy: ReportedUsagePathPolicy
  onChange: (next: ReportedUsagePathPolicy) => void
}) {
  const set = <K extends keyof ReportedUsagePathPolicy>(key: K) => (v: ReportedUsagePathPolicy[K]) =>
    onChange({ ...policy, [key]: v })
  const enabled = policy.finalOutputGuardEnabled ?? true
  return (
    <div className="space-y-5 border-t border-border/60 pt-4">
      <TogField
        label="启用最终输出限制"
        desc="先按阈值百分比放大 output_tokens，再用最终上限和扣减值限制结果。"
        checked={enabled}
        onChange={set('finalOutputGuardEnabled')}
      />
      <FieldGrid>
        <NumField
          label="输出放大阈值"
          desc="展示输出大于这个值后才按百分比放大；0 表示大于 0 即生效。"
          value={policy.outputUpliftMinTokens ?? 0}
          min={0}
          disabled={!enabled}
          suffix="Token"
          onChange={set('outputUpliftMinTokens')}
        />
        <NumField
          label="输出放大百分比"
          desc="输出超过阈值后增加的百分比；0 表示关闭。"
          value={policy.outputUpliftPercent ?? 0}
          min={0}
          max={200}
          disabled={!enabled}
          suffix="%"
          onChange={set('outputUpliftPercent')}
        />
        <NumField
          label="输出最终上限"
          desc="放大后最多显示多少 Token；0 表示不限制。"
          value={policy.finalOutputMaxTokens ?? 0}
          min={0}
          disabled={!enabled}
          suffix="Token"
          onChange={set('finalOutputMaxTokens')}
        />
        <NumField
          label="输出扣减下限"
          desc="输出触顶时至少从最终上限里扣掉多少。"
          value={policy.finalOutputJitterMinTokens ?? 0}
          min={0}
          disabled={!enabled}
          suffix="Token"
          onChange={set('finalOutputJitterMinTokens')}
        />
        <NumField
          label="输出扣减上限"
          desc="输出触顶时最多从最终上限里扣掉多少。"
          value={policy.finalOutputJitterMaxTokens ?? 0}
          min={0}
          disabled={!enabled}
          suffix="Token"
          onChange={set('finalOutputJitterMaxTokens')}
        />
      </FieldGrid>
    </div>
  )
}

function PathPolicyEditor({
  policy, onChange,
}: {
  policy: ReportedUsagePathPolicy
  onChange: (next: ReportedUsagePathPolicy) => void
}) {
  const set = <K extends keyof ReportedUsagePathPolicy>(key: K) => (v: ReportedUsagePathPolicy[K]) =>
    onChange({ ...policy, [key]: v })
  return (
    <div className="space-y-8">
      <ParameterGroup title="Usage 总开关" desc="先决定这条路径是否参与展示用量整理，以及非流式请求是否直接透传上游结果。">
        <div className="grid gap-x-8 gap-y-5 lg:grid-cols-2">
          <TogField
            label="启用本规则"
            desc="控制最终返回给客户端和后台记录的用量数字；关闭后尽量保留原始数字。"
            checked={policy.enabled}
            onChange={set('enabled')}
          />
          <TogField
            label="非流式 Usage 透传"
            desc="开启：所有非流式请求透传上游 usage，不推进本地缓存。关闭：只有按官方规则会有缓存的非流式请求（带 cache_control 且达到模型最小缓存长度）才整理，其余照样透传。"
            checked={Boolean(policy.skipNonStreamUsageProjection)}
            onChange={set('skipNonStreamUsageProjection')}
          />
        </div>
        {!policy.enabled && (
          <div className="rounded-md bg-warning/5 px-3 py-2 text-xs leading-5 text-warning">
            当前入口会尽量使用原始用量显示。重新开启后才会使用下面的展示规则。
          </div>
        )}
      </ParameterGroup>
      {policy.enabled && (
        <>
          <ParameterGroup title="字段展示规则" desc="分别决定 input、output、cache_read 和 cache_creation 的展示方式；字段之间互不挤在同一行。">
            <div className="grid gap-x-8 gap-y-8 xl:grid-cols-2">
              <FieldPolicyEditor title="展示输入" policy={policy.input} allowMoveDelta onChange={set('input')} />
              <FieldPolicyEditor title="展示输出" policy={policy.output} onChange={set('output')} />
              <FieldPolicyEditor title="展示缓存读取" policy={policy.cacheRead} onChange={set('cacheRead')} />
              <FieldPolicyEditor title="展示缓存写入" policy={policy.cacheCreation} onChange={set('cacheCreation')} />
            </div>
          </ParameterGroup>
          <ParameterGroup title="输出保护" desc="字段展示完成后，最后统一处理 output_tokens 的放大和上限，避免规则散落在输出字段里。">
            <FinalOutputGuardForm policy={policy} onChange={onChange} />
          </ParameterGroup>
          <ParameterGroup title="缓存读写上限" desc="只压低已经算出的缓存读取 / 写入，不会把较小的值抬高。">
            <div className="grid gap-x-8 gap-y-8 xl:grid-cols-2">
              <div className="space-y-4 border-t border-border/60 pt-4">
                <div>
                  <div className="text-sm font-semibold">缓存写入 cache_creation</div>
                  <div className="mt-1 text-xs leading-5 text-muted-foreground">触顶时按扣减范围随机下修，避免每次都显示同一个最大值。</div>
                </div>
                <div className="grid gap-x-5 gap-y-5 sm:grid-cols-2">
                  <NumField label="最终上限" desc="0 表示不限制。" value={policy.finalCacheCreationMaxTokens} min={0} suffix="Token" onChange={set('finalCacheCreationMaxTokens')} />
                  <NumField label="扣减下限" desc="触顶时至少扣掉多少。" value={policy.finalCacheCreationJitterMinTokens} min={0} suffix="Token" onChange={set('finalCacheCreationJitterMinTokens')} />
                  <NumField label="扣减上限" desc="触顶时最多扣掉多少。" value={policy.finalCacheCreationJitterMaxTokens} min={0} suffix="Token" onChange={set('finalCacheCreationJitterMaxTokens')} />
                </div>
              </div>
              <div className="space-y-4 border-t border-border/60 pt-4">
                <div>
                  <div className="text-sm font-semibold">缓存读取 cache_read</div>
                  <div className="mt-1 text-xs leading-5 text-muted-foreground">触顶时按扣减范围随机下修，避免每次都显示同一个最大值。</div>
                </div>
                <div className="grid gap-x-5 gap-y-5 sm:grid-cols-2">
                  <NumField label="最终上限" desc="0 表示不限制。" value={policy.finalCacheReadMaxTokens} min={0} suffix="Token" onChange={set('finalCacheReadMaxTokens')} />
                  <NumField label="扣减下限" desc="触顶时至少扣掉多少。" value={policy.finalCacheReadJitterMinTokens} min={0} suffix="Token" onChange={set('finalCacheReadJitterMinTokens')} />
                  <NumField label="扣减上限" desc="触顶时最多扣掉多少。" value={policy.finalCacheReadJitterMaxTokens} min={0} suffix="Token" onChange={set('finalCacheReadJitterMaxTokens')} />
                </div>
              </div>
            </div>
          </ParameterGroup>
        </>
      )}
    </div>
  )
}

// ─── 模型映射(modelMapping) ──────────────────────────────────────────────────

function versionEquivalentSource(model: string): string | null {
  const match = model.match(/^claude-(opus|sonnet|haiku)-(\d+)([.-])(\d{1,3})(-\d{6,})?(-thinking)?$/)
  if (!match) return null
  const [, family, major, separator, minor, , thinking = ''] = match
  return separator === '.'
    ? `claude-${family}-${major}-${minor}${thinking}`
    : `claude-${family}-${major}.${minor}${thinking}`
}
function modelVersionNumbers(model: string): number[] {
  return (model.match(/\d+/g) || []).map((part) => Number(part))
}
function compareModelId(a: string, b: string): number {
  const av = modelVersionNumbers(a)
  const bv = modelVersionNumbers(b)
  const len = Math.max(av.length, bv.length)
  for (let i = 0; i < len; i += 1) {
    const delta = (av[i] || 0) - (bv[i] || 0)
    if (delta !== 0) return delta
  }
  if (a.endsWith('-thinking') !== b.endsWith('-thinking')) return a.endsWith('-thinking') ? -1 : 1
  return a.localeCompare(b)
}
function addModelRule(rules: ModelMappingRule[], rule: ModelMappingRule) {
  const source = rule.source.trim().toLowerCase()
  const target = rule.target.trim().toLowerCase()
  if (!source || !target || source === target) return
  if (rules.some((it) => it.source === source && it.target === target && it.kind === rule.kind)) return
  rules.push({ ...rule, source, target, enabled: rule.enabled !== false })
}
export function generateDefaultModelMappingRules(status?: ModelCapabilitiesStatus): ModelMappingRule[] {
  const models = (status?.models || []).map((it) => it.model.trim().toLowerCase()).filter(Boolean)
  const rules: ModelMappingRule[] = []
  for (const model of models) {
    const source = versionEquivalentSource(model)
    if (source) addModelRule(rules, { enabled: true, source, target: model, kind: 'version_equivalent', note: '由当前可用模型列表生成的版本名兼容规则' })
  }
  const pickFamily = (family: 'opus' | 'sonnet' | 'haiku') => {
    const sorted = models.filter((m) => m === family || m.startsWith(`claude-${family}`)).sort(compareModelId)
    return sorted[sorted.length - 1]
  }
  const opus = pickFamily('opus'), sonnet = pickFamily('sonnet'), haiku = pickFamily('haiku')
  for (const source of ['opus', 'opusplan', 'best', 'default', 'auto']) {
    if (opus) addModelRule(rules, { enabled: true, source, target: opus, kind: 'alias', note: '由当前可用 Opus 模型生成的默认别名' })
  }
  if (sonnet) addModelRule(rules, { enabled: true, source: 'sonnet', target: sonnet, kind: 'alias', note: '由当前可用 Sonnet 模型生成的默认别名' })
  if (haiku) addModelRule(rules, { enabled: true, source: 'haiku', target: haiku, kind: 'alias', note: '由当前可用 Haiku 模型生成的默认别名' })
  return rules
}

export function ModelMappingSection({
  mapping, capabilities, onChange,
}: {
  mapping: ModelMappingConfig
  capabilities?: ModelCapabilitiesStatus
  onChange: (next: ModelMappingConfig) => void
}) {
  const [text, setText] = useState(() => JSON.stringify(mapping.rules, null, 2))
  const [error, setError] = useState<string | null>(null)

  const defaultRules = useMemo(() => generateDefaultModelMappingRules(capabilities), [capabilities])

  useEffect(() => {
    setText(JSON.stringify(mapping.rules, null, 2))
  }, [mapping.rules])

  const commitRules = (rules: ModelMappingRule[]) => {
    setText(JSON.stringify(rules, null, 2))
    setError(null)
    onChange({ ...mapping, rules })
  }

  const updateRule = (index: number, patch: Partial<ModelMappingRule>) => {
    const rules = mapping.rules.map((rule, ruleIndex) =>
      ruleIndex === index ? { ...rule, ...patch } : rule
    )
    commitRules(rules)
  }

  const removeRule = (index: number) => {
    commitRules(mapping.rules.filter((_, ruleIndex) => ruleIndex !== index))
  }

  const appendRule = () => {
    commitRules([
      ...mapping.rules,
      { enabled: true, source: '', target: '', kind: 'alias', note: '' },
    ])
  }

  const commitText = (value: string) => {
    setText(value)
    try {
      const parsed = JSON.parse(value) as ModelMappingRule[]
      if (!Array.isArray(parsed)) throw new Error('规则需为数组')
      setError(null)
      onChange({ ...mapping, rules: parsed })
    } catch (e) {
      setError(e instanceof Error ? e.message : '规则 JSON 解析失败')
    }
  }
  const fillDefault = () => {
    const merged = [...mapping.rules]
    for (const rule of defaultRules) addModelRule(merged, rule)
    setText(JSON.stringify(merged, null, 2))
    setError(null)
    onChange({ ...mapping, enabled: true, autoGenerateRules: true, rules: merged })
  }

  return (
    <div className="space-y-4">
      <TwoCol>
        <TogField
          label="启用模型映射"
          desc="客户端传来的模型名如果不是上游能识别的名字，就按下面规则改成真实模型名。"
          checked={mapping.enabled}
          onChange={(v) => onChange({ ...mapping, enabled: v })}
        />
        <TogField
          label="自动生成规则"
          desc="根据当前可用模型，自动补充常见别名和版本写法。"
          checked={mapping.autoGenerateRules}
          onChange={(v) => onChange({ ...mapping, autoGenerateRules: v })}
        />
      </TwoCol>
      <TwoCol>
        <TogField
          label="允许同族跨版本解析"
          desc="同一模型族的旧版本不可用时，解析到当前列表中最近的可用版本。默认开启。"
          checked={mapping.allowVersionFallback}
          onChange={(v) => onChange({ ...mapping, allowVersionFallback: v })}
          disabled={!mapping.enabled || !mapping.autoGenerateRules}
        />
        <TogField
          label="允许跨族兜底解析"
          desc="当前模型族完全没有可用模型时，才允许切换到其他模型族。默认关闭。"
          checked={mapping.allowCrossFamilyFallback}
          onChange={(v) => onChange({ ...mapping, allowCrossFamilyFallback: v })}
          disabled={!mapping.enabled || !mapping.autoGenerateRules}
        />
      </TwoCol>
      <div className="space-y-2">
        <div className="flex items-center justify-between gap-2">
          <span className="text-sm font-semibold">映射规则</span>
          <div className="flex items-center gap-2 text-xs text-muted-foreground">
            <span>当前 {mapping.rules.length} 条 · 可生成 {defaultRules.length} 条</span>
            <Button size="sm" variant="outline" onClick={appendRule}>添加规则</Button>
            <Button size="sm" variant="outline" onClick={fillDefault}>填充默认规则</Button>
          </div>
        </div>
        <div className="text-xs leading-5 text-muted-foreground">
          source 是客户端传来的模型名，target 是实际发送给上游的模型名。
        </div>
        <div className="overflow-hidden rounded-lg border bg-card">
          <div className="hidden grid-cols-[72px_1fr_1fr_150px_1fr_48px] gap-2 border-b bg-muted/40 px-3 py-2 text-xs font-medium text-muted-foreground lg:grid">
            <div>启用</div>
            <div>Source</div>
            <div>Target</div>
            <div>类型</div>
            <div>备注</div>
            <div />
          </div>
          <div className="divide-y">
            {mapping.rules.length === 0 ? (
              <div className="px-3 py-6 text-center text-sm text-muted-foreground">暂无显式规则，可添加或填充默认规则。</div>
            ) : mapping.rules.map((rule, index) => (
              <div key={index} className="grid gap-2 px-3 py-3 lg:grid-cols-[72px_1fr_1fr_150px_1fr_48px] lg:items-center">
                <label className="flex items-center justify-between gap-2 text-xs text-muted-foreground lg:block">
                  <span className="lg:hidden">启用</span>
                  <Switch checked={rule.enabled !== false} onCheckedChange={(enabled) => updateRule(index, { enabled })} />
                </label>
                <Input className="font-mono text-xs" value={rule.source} placeholder="sonnet"
                  onChange={(e) => updateRule(index, { source: e.target.value })} />
                <Input className="font-mono text-xs" value={rule.target} placeholder="claude-sonnet-4.5"
                  onChange={(e) => updateRule(index, { target: e.target.value })} />
                <Select value={rule.kind || 'alias'} onValueChange={(kind) => updateRule(index, { kind: kind as ModelMappingRule['kind'] })}>
                  <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                  <SelectContent>
                    <SelectItem value="alias">别名</SelectItem>
                    <SelectItem value="version_equivalent">版本等价</SelectItem>
                    <SelectItem value="fallback">兜底</SelectItem>
                  </SelectContent>
                </Select>
                <Input value={rule.note ?? ''} placeholder="备注" onChange={(e) => updateRule(index, { note: e.target.value })} />
                <Button variant="ghost" size="icon-sm" className="text-destructive hover:text-destructive"
                  onClick={() => removeRule(index)} title="删除规则">
                  <Trash2 className="size-4" />
                </Button>
              </div>
            ))}
          </div>
        </div>
      </div>
      <div className="space-y-2">
        <span className="text-sm font-semibold">批量编辑 JSON</span>
        <Textarea value={text} rows={12} className="font-mono text-xs"
          onChange={(e) => commitText(e.target.value)} />
        {error && <div className="text-xs text-destructive">规则解析错误: {error}</div>}
        <div className="text-xs leading-5 text-muted-foreground">
          每条规则包含：source（客户端传来的名称）、target（实际发送的模型）、kind（规则类型）、enabled（是否启用）、note（备注）。
        </div>
      </div>
    </div>
  )
}

// ─── /dfcache 路径归一化 ─────────────────────────────────────────────────────

/** 归一化为合法的 /dfcache/<name>，非法返回 null。name 仅允许 a-z 0-9 . _ -，≤64 */
export function normalizeDefinedCacheRoute(route: string): string | null {
  const trimmed = route.trim()
  if (!trimmed) return null
  const withSlash = trimmed.startsWith('/') ? trimmed : `/${trimmed}`
  const normalized = withSlash.replace(/\/+$/, '').toLowerCase()
  const name = normalized.startsWith(DFCACHE_ROUTE_PREFIX)
    ? normalized.slice(DFCACHE_ROUTE_PREFIX.length)
    : ''
  const normalizedName = normalizeDefinedCacheRouteName(name)
  if (!normalizedName) {
    return null
  }
  return `${DFCACHE_ROUTE_PREFIX}${normalizedName}`
}

export function normalizeDefinedCacheRoutes(routes: string[]): string[] {
  const seen = new Set<string>()
  const out: string[] = []
  for (const route of routes) {
    const value = normalizeDefinedCacheRoute(route)
    if (value && !seen.has(value)) {
      seen.add(value)
      out.push(value)
    }
  }
  return out
}
