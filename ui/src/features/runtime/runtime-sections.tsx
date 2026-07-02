import { useEffect, useMemo, useState } from 'react'
import { Plus, Trash2 } from 'lucide-react'
import { Input, Select, SelectContent, SelectItem, SelectTrigger, SelectValue, Switch, Textarea, Button } from '@/components/ui'
import { defaultPromptCacheCreationControl, inputSamplePolicy, pathPolicy, preserveFieldPolicy, writerSamplePolicy } from '@/lib/runtime-config-defaults'
import type {
  ModelCapabilitiesStatus,
  CachePolicyConfig,
  CacheRoutePolicyPatch,
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
    <div className="space-y-1.5">
      <div className="text-sm font-semibold text-foreground">{label}</div>
      {desc && <div className="text-xs leading-relaxed text-muted-foreground">{desc}</div>}
      <div className="flex items-center gap-2">
        <Input type="number" className="w-full" value={value} min={min} max={max} step={step}
          inputMode={step && step < 1 ? 'decimal' : 'numeric'} disabled={disabled}
          onChange={(e) => onChange(numberValue(e.target.value, min ?? 0))} />
        {suffix && <span className="min-w-[5rem] shrink-0 text-sm text-muted-foreground">{suffix}</span>}
      </div>
    </div>
  )
}

export function TogField({
  label, desc, checked, disabled, onChange,
}: {
  label: string; desc?: string; checked: boolean; disabled?: boolean; onChange: (v: boolean) => void
}) {
  return (
    <div className="flex items-start justify-between gap-4">
      <div className="min-w-0">
        <div className="text-sm font-semibold text-foreground">{label}</div>
        {desc && <div className="mt-0.5 text-xs leading-relaxed text-muted-foreground">{desc}</div>}
      </div>
      <Switch checked={checked} disabled={disabled} onCheckedChange={onChange} className="mt-0.5 shrink-0" />
    </div>
  )
}

function TwoCol({ children }: { children: React.ReactNode }) {
  return <div className="grid gap-4 md:grid-cols-2">{children}</div>
}

// ─── 缓存策略与路径绑定(cachePolicy + legacy defaults) ───────────────────────

type CacheSimulationPatch = NonNullable<CacheRoutePolicyPatch['simulation']>
type KiroRsToolPatch = NonNullable<CacheRoutePolicyPatch['kiroRsTool']>
type CacheStrategyType = NonNullable<CacheRoutePolicyPatch['cacheType']>

const BUILT_IN_CACHE_PREFIXES = ['/v1', '/cc', '/ha', '/na'] as const
const DFCACHE_ROUTE_PREFIX = '/dfcache/'
const CACHE_ENDPOINT_LABELS: Record<string, string> = {
  '/v1': '/v1/messages',
  '/cc': '/cc/v1/messages',
  '/ha': '/ha/v1/messages',
  '/na': '/na/v1/messages',
}

function normalizeCachePolicyPath(prefix: string): string | null {
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

function defaultUsagePatch(prefix: string): ReportedUsagePathPolicy {
  if (prefix === '/cc') return pathPolicy(true, inputSamplePolicy(96), writerSamplePolicy(3000))
  if (prefix === '/ha') return pathPolicy(true, inputSamplePolicy(96), preserveFieldPolicy())
  return normalizeDefinedCacheRoute(prefix)
    ? pathPolicy(true, inputSamplePolicy(96), preserveFieldPolicy())
    : pathPolicy()
}

function defaultPathCachePatch(prefix: string, cacheType: CacheStrategyType): CacheRoutePolicyPatch {
  if (cacheType === 'no_cache') {
    return { cacheType: 'no_cache' }
  }
  if (cacheType === 'kiro_rs_tool') {
    return { cacheType: 'kiro_rs_tool', kiroRsTool: defaultKiroRsToolPatch() }
  }
  return {
    cacheType: 'current_high_cache',
    simulation: defaultSimulationPatch(),
    creationControl: defaultPromptCacheCreationControl(),
    reportedUsage: defaultUsagePatch(prefix),
  }
}

function cacheTypeDesc(cacheType: CacheRoutePolicyPatch['cacheType']): string {
  if (cacheType === 'no_cache') {
    return '这个路径不进入缓存计算，直接使用原始用量返回和记录，CPU 和内存开销最低。'
  }
  if (cacheType === 'kiro_rs_tool') {
    return '按 Kiro-RS Tool 的会话和路径规则计算缓存；第一次请求不会显示缓存读取，失败请求不会写入缓存。'
  }
  return '使用当前系统的本地模拟缓存逻辑，把原始用量换算成对外显示的缓存用量。'
}

function isBuiltInCachePrefix(prefix: string): boolean {
  return (BUILT_IN_CACHE_PREFIXES as readonly string[]).includes(prefix)
}

function canonicalCachePolicyPath(prefix: string): string {
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

function routeOverrideForPrefix(
  pathOverrides: Record<string, CacheRoutePolicyPatch> | undefined,
  prefix: string
): CacheRoutePolicyPatch | undefined {
  for (const alias of cachePolicyPathAliases(prefix)) {
    const policy = pathOverrides?.[alias]
    if (policy) return policy
  }
  return undefined
}

function reportedUsageForPrefix(
  pathOverrides: Record<string, ReportedUsagePathPolicy> | undefined,
  prefix: string
): ReportedUsagePathPolicy | undefined {
  for (const alias of cachePolicyPathAliases(prefix)) {
    const policy = pathOverrides?.[alias]
    if (policy) return policy
  }
  return undefined
}

function deletePrefixAliases<T>(record: Record<string, T>, prefix: string) {
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

function normalizeDefinedCacheRouteName(name: string): string | null {
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

function buildDefinedCacheRoute(name: string): string | null {
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
      <TwoCol>
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
      </TwoCol>
      <TwoCol>
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
      </TwoCol>
      <TwoCol>
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
      </TwoCol>
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
      <TwoCol>
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
      </TwoCol>
      <TwoCol>
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
      </TwoCol>
      <TwoCol>
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
      </TwoCol>
      <TwoCol>
        <NumField
          label="空闲后清理状态"
          desc="这个路径多久没有新请求后清理临时计数，减少长期占用；0 表示不按空闲时间清理。"
          value={merged.expireAfterIdleSecs}
          min={0}
          suffix="秒"
          disabled={!merged.enabled}
          onChange={set('expireAfterIdleSecs')}
        />
      </TwoCol>
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
      <TwoCol>
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
      </TwoCol>
      <TwoCol>
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
      </TwoCol>
      <TwoCol>
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
      </TwoCol>
    </div>
  )
}

function cachePolicyForStrategyTemplate(policy: CacheRoutePolicyPatch, cacheType: CacheStrategyType): CacheRoutePolicyPatch {
  if (cacheType === 'no_cache') return { cacheType: 'no_cache' }
  if (cacheType === 'kiro_rs_tool') return { cacheType: 'kiro_rs_tool', kiroRsTool: policy.kiroRsTool ?? defaultKiroRsToolPatch() }
  return {
    cacheType: 'current_high_cache',
    simulation: policy.simulation ?? defaultSimulationPatch(),
    creationControl: policy.creationControl ?? defaultPromptCacheCreationControl(),
    reportedUsage: policy.reportedUsage ?? defaultUsagePatch('/v1'),
  }
}

function currentHighCachePathDefaults(prefix: string, reportedUsage?: ReportedUsagePathPolicy): CacheRoutePolicyPatch {
  return {
    cacheType: 'current_high_cache',
    simulation: defaultSimulationPatch(),
    creationControl: defaultPromptCacheCreationControl(),
    reportedUsage: reportedUsage ?? defaultUsagePatch(prefix),
  }
}

function StrategyTemplateEditor({
  title,
  desc,
  cacheType,
  policy,
  onChange,
}: {
  title: string
  desc: string
  cacheType: CacheStrategyType
  policy: CacheRoutePolicyPatch
  onChange: (next: CacheRoutePolicyPatch) => void
}) {
  const template = cachePolicyForStrategyTemplate(policy, cacheType)
  const setSimulation = (simulation: CacheSimulationPatch) => onChange({ ...template, simulation })
  const setCreationControl = (creationControl: PromptCacheCreationControlConfig) => onChange({ ...template, creationControl })
  const setReportedUsage = (reportedUsage: ReportedUsagePathPolicy) => onChange({ ...template, reportedUsage })
  const setKiroRsTool = (kiroRsTool: KiroRsToolPatch) => onChange({ ...template, kiroRsTool })

  return (
    <div className="space-y-4 rounded-lg bg-background p-4 shadow-sm">
      <div>
        <div className="text-sm font-semibold">{title}</div>
        <div className="mt-1 text-xs leading-5 text-muted-foreground">{desc}</div>
      </div>
      {cacheType === 'current_high_cache' ? (
        <>
          <div className="space-y-3">
            <div className="text-sm font-semibold">模拟读取参数</div>
            <SimulationOverrideForm value={template.simulation ?? defaultSimulationPatch()} onChange={setSimulation} />
          </div>
          <div className="space-y-3">
            <div className="text-sm font-semibold">缓存创建展示频次</div>
            <CreationControlOverrideForm value={template.creationControl ?? defaultPromptCacheCreationControl()} onChange={setCreationControl} />
          </div>
          <div className="space-y-3">
            <div className="text-sm font-semibold">最终用量显示</div>
            <PathPolicyEditor policy={template.reportedUsage ?? defaultUsagePatch('/v1')} onChange={setReportedUsage} />
          </div>
        </>
      ) : (
        <KiroRsToolPolicyForm
          value={template.kiroRsTool ?? defaultKiroRsToolPatch()}
          onChange={setKiroRsTool}
        />
      )}
    </div>
  )
}

function pathPolicyWithStrategyDefaults(
  cachePolicy: CachePolicyConfig,
  prefix: string,
  policy: CacheRoutePolicyPatch
): CacheRoutePolicyPatch {
  const cacheType = policy.cacheType ?? 'no_cache'
  if (cacheType === 'no_cache') return { cacheType: 'no_cache' }
  const template = cacheType === 'kiro_rs_tool'
    ? cachePolicyForStrategyTemplate(cachePolicy.kiroRsTool ?? {}, 'kiro_rs_tool')
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
    ...(cacheType === 'current_high_cache'
      ? {
          simulation: policy.simulation ?? template.simulation ?? defaultSimulationPatch(),
          creationControl: policy.creationControl ?? template.creationControl ?? defaultPromptCacheCreationControl(),
          reportedUsage: policy.reportedUsage ?? template.reportedUsage ?? defaultUsagePatch(prefix),
        }
      : {
          kiroRsTool: policy.kiroRsTool ?? template.kiroRsTool ?? defaultKiroRsToolPatch(),
        }),
  }
}

function PathStrategyBindingCard({
  prefix,
  policy,
  cachePolicy,
  definedRoutes,
  builtIn,
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
    onChange(defaultPathCachePatch(prefix, cacheType))
  }

  const patch = (next: Partial<CacheRoutePolicyPatch>) => {
    onChange({ ...effectivePolicy, ...next, cacheType: effectiveCacheType })
  }

  return (
    <div className="space-y-4 rounded-lg bg-background p-4 shadow-sm">
      <div className="flex flex-col gap-3 md:flex-row md:items-end md:justify-between">
        <div className="min-w-0 flex-1 space-y-1.5">
          <div className="text-sm font-semibold">{isDfcachePath ? '自定义路径后缀' : '路径前缀'}</div>
          {builtIn ? (
            <div className="rounded-md bg-muted/20 px-3 py-2 font-mono text-sm">{prefix}</div>
          ) : isDfcachePath ? (
            <div className="flex items-stretch overflow-hidden rounded-md border border-input bg-background">
              <div className="flex items-center border-r border-input bg-muted/30 px-3 font-mono text-sm text-muted-foreground">
                {DFCACHE_ROUTE_PREFIX}
              </div>
              <Input
                className="border-0 shadow-none focus-visible:ring-0"
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
          <div className="text-xs text-muted-foreground">
            {isDfcachePath
              ? '固定前缀用于避开内置路由，只能修改后缀名。'
              : `请求路径以 ${cacheEndpointLabel(prefix)} 开头时使用这条规则；多个规则都能匹配时，用最长的那个。`}
          </div>
          {prefixError && <div className="text-xs text-destructive">{prefixError}</div>}
        </div>
        {!builtIn && (
          <Button type="button" variant="outline" size="sm" className="text-destructive" onClick={onDelete}>
            <Trash2 className="mr-1 h-4 w-4" />
            删除路径
          </Button>
        )}
      </div>

      {isDfcachePath && (
        <TogField
          label="注册为 /dfcache 路由"
          desc={normalizedDefinedRoute ? '开启后允许客户端访问这个 /dfcache/{name} 入口。' : '路径必须是 /dfcache/{name}，name 仅允许小写字母、数字、点、下划线或短横线。'}
          checked={isRouteRegistered}
          disabled={!normalizedDefinedRoute}
          onChange={onDefinedRouteChange}
        />
      )}

      <div className="space-y-2">
        <div className="text-sm font-semibold">缓存策略</div>
        <Select value={effectiveCacheType} onValueChange={(value) => setCacheType(value as CacheStrategyType)}>
          <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value="no_cache">无缓存</SelectItem>
            <SelectItem value="current_high_cache">本地模拟缓存策略</SelectItem>
            <SelectItem value="kiro_rs_tool">Kiro-RS Tool 缓存策略</SelectItem>
          </SelectContent>
        </Select>
        <div className="text-xs leading-5 text-muted-foreground">{cacheTypeDesc(effectiveCacheType)}</div>
      </div>

      {effectiveCacheType === 'no_cache' ? (
        <div className="rounded-md bg-muted/30 px-3 py-2 text-xs leading-5 text-muted-foreground">
          这个路径直接走无缓存逻辑，不进入缓存计算，也不展示缓存参数。
        </div>
      ) : effectiveCacheType === 'current_high_cache' ? (
        <div className="space-y-4">
          <div className="text-sm font-semibold">本路径策略参数</div>
          <SimulationOverrideForm
            value={effectivePolicy.simulation ?? defaultSimulationPatch()}
            onChange={(simulation) => patch({ simulation })}
          />
          <CreationControlOverrideForm
            value={effectivePolicy.creationControl ?? defaultPromptCacheCreationControl()}
            onChange={(creationControl) => patch({ creationControl })}
          />
          <PathPolicyEditor
            policy={effectivePolicy.reportedUsage ?? defaultUsagePatch(prefix)}
            onChange={(reportedUsage) => patch({ reportedUsage })}
          />
        </div>
      ) : (
        <div className="space-y-4">
          <div>
            <div className="text-sm font-semibold">本路径策略参数</div>
            <div className="mt-1 text-xs leading-5 text-muted-foreground">
              这里只展示 Kiro-RS Tool 自己需要的参数，不读取本地模拟缓存策略的参数。
            </div>
          </div>
          <KiroRsToolPolicyForm
            value={effectivePolicy.kiroRsTool ?? defaultKiroRsToolPatch()}
            onChange={(kiroRsTool) => patch({ kiroRsTool })}
          />
        </div>
      )}
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
    if (normalizedPrefix === '/na') {
      return { cacheType: 'no_cache' }
    }
    if (existing) {
      if (existing.cacheType === 'no_cache' || existing.cacheType === 'kiro_rs_tool') {
        return existing
      }
      return legacyReportedUsage ? { ...existing, reportedUsage: existing.reportedUsage ?? legacyReportedUsage } : existing
    }
    if (legacyReportedUsage) {
      return currentHighCachePathDefaults(normalizedPrefix, legacyReportedUsage)
    }
    if (isBuiltInCachePrefix(normalizedPrefix)) {
      return currentHighCachePathDefaults(normalizedPrefix)
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
    pathOverrides[normalizedPrefix] = nextPolicy.cacheType === 'no_cache'
      ? { cacheType: 'no_cache' }
      : nextPolicy
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
  const kiroTemplate = cachePolicyForStrategyTemplate(cachePolicy.kiroRsTool ?? {}, 'kiro_rs_tool')

  return (
    <div className="space-y-6">
      <div className="grid gap-4 xl:grid-cols-2">
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
      </div>

      <div className="space-y-4">
        <div>
          <div className="text-sm font-semibold">路径绑定</div>
          <div className="mt-1 text-xs leading-5 text-muted-foreground">
            每个路径都显式选择无缓存、本地模拟缓存策略或 Kiro-RS Tool 缓存策略。
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
    <div className="space-y-3">
      <div className="flex items-center justify-between gap-2">
        <span className="text-sm font-semibold">{title}</span>
        <Select value={policy.mode} onValueChange={(v) => set('mode')(v as ReportedUsageFieldPolicy['mode'])}>
          <SelectTrigger size="sm" className="w-[9rem]"><SelectValue /></SelectTrigger>
          <SelectContent>
            {Object.entries(FIELD_MODE_LABELS).map(([k, label]) => (
              <SelectItem key={k} value={k}>{label}</SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
      <div className="text-xs leading-5 text-muted-foreground">{FIELD_MODE_DESCS[policy.mode]}</div>
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
          desc="如果输入显示被压低，少掉的部分加到缓存读取里，让总输入更接近原本的规模。"
          checked={policy.moveDeltaToCacheRead}
          disabled={policy.mode === 'preserve' || policy.mode === 'raw'}
          onChange={set('moveDeltaToCacheRead')}
        />
      )}
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
    <div className="space-y-3">
      <TogField
        label="启用本规则"
        desc="控制最终返回给客户端和后台记录的用量数字；关闭后尽量保留原始数字。"
        checked={policy.enabled}
        onChange={set('enabled')}
      />
      {!policy.enabled && (
        <div className="rounded-md border border-warning/30 bg-warning/10 px-3 py-2 text-xs leading-5 text-warning">
          当前入口会尽量使用原始用量显示。重新开启后才会使用下面的展示规则。
        </div>
      )}
      {policy.enabled && (
        <>
          <FieldPolicyEditor title="展示输入" policy={policy.input} allowMoveDelta onChange={set('input')} />
          <FieldPolicyEditor title="展示输出" policy={policy.output} onChange={set('output')} />
          <FieldPolicyEditor title="展示缓存读取" policy={policy.cacheRead} onChange={set('cacheRead')} />
          <FieldPolicyEditor title="展示缓存写入" policy={policy.cacheCreation} onChange={set('cacheCreation')} />
          <TwoCol>
            <NumField
              label="读取缓存最终上限"
              desc="缓存读取最终最多显示多少 Token；0 表示不限制。只会压低过大的值，不会把小值抬高。"
              value={policy.finalCacheReadMaxTokens}
              min={0}
              suffix="Token"
              onChange={set('finalCacheReadMaxTokens')}
            />
            <NumField
              label="最终上限扣减下限"
              desc="触顶时至少从上限里扣掉多少 Token，避免每次都显示同一个最大值。"
              value={policy.finalCacheReadJitterMinTokens}
              min={0}
              suffix="Token"
              onChange={set('finalCacheReadJitterMinTokens')}
            />
          </TwoCol>
          <TwoCol>
            <NumField
              label="最终上限扣减上限"
              desc="触顶时最多从上限里扣掉多少 Token；必须大于等于扣减下限。"
              value={policy.finalCacheReadJitterMaxTokens}
              min={0}
              suffix="Token"
              onChange={set('finalCacheReadJitterMaxTokens')}
            />
          </TwoCol>
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
      <div className="space-y-2">
        <div className="flex items-center justify-between gap-2">
          <span className="text-sm font-semibold">映射规则（JSON）</span>
          <div className="flex items-center gap-2 text-xs text-muted-foreground">
            <span>当前 {mapping.rules.length} 条 · 可生成 {defaultRules.length} 条</span>
            <Button size="sm" variant="outline" onClick={fillDefault}>填充默认规则</Button>
          </div>
        </div>
        <div className="text-xs leading-5 text-muted-foreground">
          source 是客户端传来的模型名，target 是实际发送给上游的模型名。
        </div>
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
