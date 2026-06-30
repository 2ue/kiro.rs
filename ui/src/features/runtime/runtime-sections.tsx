import { useEffect, useMemo, useState } from 'react'
import { Plus, Trash2 } from 'lucide-react'
import { Input, Select, SelectContent, SelectItem, SelectTrigger, SelectValue, Switch, Textarea, Button } from '@/components/ui'
import { defaultPromptCacheCreationControl, inputSamplePolicy, pathPolicy, preserveFieldPolicy } from '@/lib/runtime-config-defaults'
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

// ─── 统一缓存策略(cachePolicy + legacy defaults) ─────────────────────────────

type CacheSimulationPatch = NonNullable<CacheRoutePolicyPatch['simulation']>
type CachePointPatch = NonNullable<CacheRoutePolicyPatch['cachePoint']>
type CacheBoundsPatch = NonNullable<CacheRoutePolicyPatch['bounds']>

function normalizeCachePolicyPath(prefix: string): string | null {
  const trimmed = prefix.trim()
  if (!trimmed) return null
  const withSlash = trimmed.startsWith('/') ? trimmed : `/${trimmed}`
  return withSlash.replace(/\/+$/, '') || '/'
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

function defaultCachePointPatch(): CachePointPatch {
  return {
    enabled: false,
    toolsOnly: true,
    recordPlan: true,
  }
}

function defaultBoundsPatch(): CacheBoundsPatch {
  return {
    maxEntriesPerAccount: 200,
    maxEntriesGlobal: 20000,
    entryTtlSecs: 86400,
    estimatedBytesLimit: 268435456,
  }
}

function defaultUsagePatch(prefix: string): ReportedUsagePathPolicy {
  return normalizeDefinedCacheRoute(prefix)
    ? pathPolicy(true, inputSamplePolicy(96), preserveFieldPolicy())
    : pathPolicy()
}

function defaultPathCachePatch(prefix: string): CacheRoutePolicyPatch {
  return {
    simulation: defaultSimulationPatch(),
    creationControl: defaultPromptCacheCreationControl(),
    reportedUsage: defaultUsagePatch(prefix),
    cachePoint: defaultCachePointPatch(),
    bounds: defaultBoundsPatch(),
  }
}

function isEmptyRoutePatch(policy: CacheRoutePolicyPatch): boolean {
  return !policy.simulation && !policy.creationControl && !policy.reportedUsage && !policy.cachePoint && !policy.bounds
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
    <div className="space-y-4 rounded-lg border border-border bg-muted/20 p-4">
      <TogField
        label="启用高缓存模拟"
        desc="只覆盖当前路径的高缓存模拟开关，不影响其他入口。"
        checked={merged.enabled ?? true}
        onChange={set('enabled')}
      />
      <TwoCol>
        <NumField label="目标缓存读取比例" value={merged.targetReadRatio ?? 0.98} min={0} max={0.99} step={0.01} suffix="比例" onChange={set('targetReadRatio')} />
        <NumField label="输入放大倍数" value={merged.tokenScale ?? 1.6} min={1} max={3} step={0.1} suffix="倍" onChange={set('tokenScale')} />
      </TwoCol>
      <TwoCol>
        <NumField label="模拟输入上限" value={merged.maxSimulatedInputTokens ?? 300000} min={0} suffix="Token" onChange={set('maxSimulatedInputTokens')} />
        <NumField label="放大生效门槛" value={merged.scaleMinInputTokens ?? 20000} min={0} suffix="Token" onChange={set('scaleMinInputTokens')} />
      </TwoCol>
      <TwoCol>
        <NumField label="上限扣减下限" value={merged.capJitterMinTokens ?? 12000} min={0} suffix="Token" onChange={set('capJitterMinTokens')} />
        <NumField label="上限扣减上限" value={merged.capJitterMaxTokens ?? 24000} min={0} suffix="Token" onChange={set('capJitterMaxTokens')} />
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
    <div className="space-y-4 rounded-lg border border-border bg-muted/20 p-4">
      <TogField
        label="启用缓存创建频次控制"
        desc="只覆盖当前路径的缓存写入展示节奏。"
        checked={merged.enabled}
        onChange={set('enabled')}
      />
      <div className="space-y-1.5">
        <div className="text-sm font-semibold">控制维度</div>
        <Select value={merged.scopeMode} disabled={!merged.enabled} onValueChange={(v) => set('scopeMode')(v as PromptCacheCreationControlConfig['scopeMode'])}>
          <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value="credential_conversation_model">账号 + 会话 + 模型</SelectItem>
            <SelectItem value="conversation_model">会话 + 模型</SelectItem>
          </SelectContent>
        </Select>
      </div>
      <TwoCol>
        <NumField label="最小成功请求间隔" value={merged.minSuccessfulRequestsBetweenCreation} min={0} suffix="次" disabled={!merged.enabled} onChange={set('minSuccessfulRequestsBetweenCreation')} />
        <NumField label="最小时间间隔" value={merged.minCreationIntervalSecs} min={0} suffix="秒" disabled={!merged.enabled} onChange={set('minCreationIntervalSecs')} />
      </TwoCol>
      <TwoCol>
        <NumField label="最小累计增量" value={merged.minCreationDeltaTokens} min={0} suffix="Token" disabled={!merged.enabled} onChange={set('minCreationDeltaTokens')} />
        <NumField label="单次展示上限" value={merged.maxCreationTokensPerEvent} min={0} suffix="Token" disabled={!merged.enabled} onChange={set('maxCreationTokensPerEvent')} />
      </TwoCol>
      <TwoCol>
        <NumField label="额度窗口长度" value={merged.creationBudgetWindowSecs} min={0} suffix="秒" disabled={!merged.enabled} onChange={set('creationBudgetWindowSecs')} />
        <NumField label="窗口展示额度" value={merged.maxCreationTokensPerWindow} min={0} suffix="Token" disabled={!merged.enabled} onChange={set('maxCreationTokensPerWindow')} />
      </TwoCol>
      <TwoCol>
        <NumField label="空闲后清理状态" value={merged.expireAfterIdleSecs} min={0} suffix="秒" disabled={!merged.enabled} onChange={set('expireAfterIdleSecs')} />
      </TwoCol>
    </div>
  )
}

function CachePointOverrideForm({
  value,
  onChange,
}: {
  value: CachePointPatch
  onChange: (next: CachePointPatch) => void
}) {
  const merged = { ...defaultCachePointPatch(), ...value }
  const set = <K extends keyof CachePointPatch>(key: K) => (nextValue: CachePointPatch[K]) =>
    onChange({ ...merged, [key]: nextValue })

  return (
    <div className="space-y-4 rounded-lg border border-border bg-muted/20 p-4">
      <TogField
        label="发送真实 cachePoint"
        desc="把带缓存标记的工具发送给 Kiro 上游；上游不接受时会自动去掉后重试一次。"
        checked={merged.enabled ?? false}
        onChange={set('enabled')}
      />
      <TwoCol>
        <TogField
          label="只处理工具缓存标记"
          desc="只根据工具上的缓存标记插入 cachePoint，不改写系统消息或历史消息。"
          checked={merged.toolsOnly ?? true}
          disabled={!merged.enabled}
          onChange={set('toolsOnly')}
        />
        <TogField
          label="记录 cachePoint 计划"
          desc="在系统日志中记录插入数量，方便排查上游请求体错误。"
          checked={merged.recordPlan ?? true}
          disabled={!merged.enabled}
          onChange={set('recordPlan')}
        />
      </TwoCol>
    </div>
  )
}

function CacheBoundsOverrideForm({
  value,
  onChange,
}: {
  value: CacheBoundsPatch
  onChange: (next: CacheBoundsPatch) => void
}) {
  const merged = { ...defaultBoundsPatch(), ...value }
  const set = <K extends keyof CacheBoundsPatch>(key: K) => (nextValue: CacheBoundsPatch[K]) =>
    onChange({ ...merged, [key]: nextValue })

  return (
    <div className="space-y-4 rounded-lg border border-border bg-muted/20 p-4">
      <TwoCol>
        <NumField label="单账号条目上限" value={merged.maxEntriesPerAccount ?? 200} min={0} suffix="条" onChange={set('maxEntriesPerAccount')} />
        <NumField label="全局条目上限" value={merged.maxEntriesGlobal ?? 20000} min={0} suffix="条" onChange={set('maxEntriesGlobal')} />
      </TwoCol>
      <TwoCol>
        <NumField label="最长保留时间" value={merged.entryTtlSecs ?? 86400} min={1} suffix="秒" onChange={set('entryTtlSecs')} />
        <NumField label="估算内存上限" value={merged.estimatedBytesLimit ?? 268435456} min={0} suffix="字节" onChange={set('estimatedBytesLimit')} />
      </TwoCol>
    </div>
  )
}

function CachePatchBlock({
  title,
  desc,
  enabled,
  onSet,
  onClear,
  children,
}: {
  title: string
  desc: string
  enabled: boolean
  onSet: () => void
  onClear: () => void
  children: React.ReactNode
}) {
  return (
    <div className="space-y-3 rounded-lg border border-border bg-muted/10 p-4">
      <div className="flex items-center justify-between gap-3">
        <div>
          <div className="text-sm font-semibold">{title}</div>
          <div className="text-xs text-muted-foreground">{desc}</div>
        </div>
        {enabled ? (
          <Button type="button" variant="ghost" size="sm" onClick={onClear}>清除覆盖</Button>
        ) : (
          <Button type="button" variant="outline" size="sm" onClick={onSet}>设置覆盖</Button>
        )}
      </div>
      {enabled && children}
    </div>
  )
}

function PathCachePolicyCard({
  prefix,
  policy,
  definedRoutes,
  onPrefixChange,
  onDelete,
  onChange,
  onDefinedRouteChange,
}: {
  prefix: string
  policy: CacheRoutePolicyPatch
  definedRoutes: string[]
  onPrefixChange: (nextPrefix: string) => void
  onDelete: () => void
  onChange: (next: CacheRoutePolicyPatch) => void
  onDefinedRouteChange: (enabled: boolean) => void
}) {
  const [draftPrefix, setDraftPrefix] = useState(prefix)
  const [prefixError, setPrefixError] = useState<string | null>(null)
  const normalizedDefinedRoute = normalizeDefinedCacheRoute(prefix)
  const isDfcachePath = prefix.toLowerCase().startsWith(DFCACHE_ROUTE_PREFIX)
  const isRouteRegistered = Boolean(normalizedDefinedRoute && definedRoutes.includes(normalizedDefinedRoute))

  useEffect(() => {
    setDraftPrefix(prefix)
    setPrefixError(null)
  }, [prefix])

  const commitPrefix = () => {
    const normalized = normalizeCachePolicyPath(draftPrefix)
    if (!normalized) {
      setPrefixError('路径不能为空')
      setDraftPrefix(prefix)
      return
    }
    setPrefixError(null)
    onPrefixChange(normalized)
  }

  const setSimulation = (simulation?: CacheSimulationPatch) => {
    onChange({ ...policy, simulation })
  }
  const setCreationControl = (creationControl?: PromptCacheCreationControlConfig) => {
    onChange({ ...policy, creationControl })
  }
  const setReportedUsage = (reportedUsage?: ReportedUsagePathPolicy) => {
    onChange({ ...policy, reportedUsage })
  }
  const setCachePoint = (cachePoint?: CachePointPatch) => {
    onChange({ ...policy, cachePoint })
  }
  const setBounds = (bounds?: CacheBoundsPatch) => {
    onChange({ ...policy, bounds })
  }

  return (
    <div className="space-y-4 rounded-lg border border-border bg-background p-4">
      <div className="flex flex-col gap-3 md:flex-row md:items-end md:justify-between">
        <div className="min-w-0 flex-1 space-y-1.5">
          <div className="text-sm font-semibold">路径前缀</div>
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
          {prefixError && <div className="text-xs text-destructive">{prefixError}</div>}
        </div>
        <Button type="button" variant="outline" size="sm" className="text-destructive" onClick={onDelete}>
          <Trash2 className="mr-1 h-4 w-4" />
          删除路径
        </Button>
      </div>

      {isDfcachePath && (
        <div className="rounded-lg border border-border bg-muted/10 p-4">
          <TogField
            label="注册为 /dfcache 路由"
            desc={normalizedDefinedRoute ? '开启后允许客户端访问这个 /dfcache/{name} 入口。' : '路径必须是 /dfcache/{name}，name 仅允许小写字母、数字、点、下划线或短横线。'}
            checked={isRouteRegistered}
            disabled={!normalizedDefinedRoute}
            onChange={onDefinedRouteChange}
          />
        </div>
      )}

      <CachePatchBlock
        title="高缓存模拟"
        desc="不设置时沿用默认策略里的高缓存模拟参数。"
        enabled={Boolean(policy.simulation)}
        onSet={() => setSimulation(defaultSimulationPatch())}
        onClear={() => setSimulation(undefined)}
      >
        {policy.simulation && <SimulationOverrideForm value={policy.simulation} onChange={setSimulation} />}
      </CachePatchBlock>

      <CachePatchBlock
        title="缓存创建频次"
        desc="不设置时沿用默认策略里的缓存创建频次。"
        enabled={Boolean(policy.creationControl)}
        onSet={() => setCreationControl(defaultPromptCacheCreationControl())}
        onClear={() => setCreationControl(undefined)}
      >
        {policy.creationControl && <CreationControlOverrideForm value={policy.creationControl} onChange={setCreationControl} />}
      </CachePatchBlock>

      <CachePatchBlock
        title="用量展示"
        desc="控制这个路径返回给客户端和后台记录的 input、output、cache read、cache write 口径。"
        enabled={Boolean(policy.reportedUsage)}
        onSet={() => setReportedUsage(defaultUsagePatch(prefix))}
        onClear={() => setReportedUsage(undefined)}
      >
        {policy.reportedUsage && <PathPolicyEditor policy={policy.reportedUsage} onChange={setReportedUsage} />}
      </CachePatchBlock>

      <CachePatchBlock
        title="真实 cachePoint"
        desc="不设置时沿用默认策略里的 cachePoint 开关。"
        enabled={Boolean(policy.cachePoint)}
        onSet={() => setCachePoint(defaultCachePointPatch())}
        onClear={() => setCachePoint(undefined)}
      >
        {policy.cachePoint && <CachePointOverrideForm value={policy.cachePoint} onChange={setCachePoint} />}
      </CachePatchBlock>

      <CachePatchBlock
        title="缓存边界"
        desc="按路径覆盖缓存指纹条目、保留时间和估算内存上限。"
        enabled={Boolean(policy.bounds)}
        onSet={() => setBounds(defaultBoundsPatch())}
        onClear={() => setBounds(undefined)}
      >
        {policy.bounds && <CacheBoundsOverrideForm value={policy.bounds} onChange={setBounds} />}
      </CachePatchBlock>
    </div>
  )
}

export function CachePolicySection({
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
    ...Object.keys(cachePolicy.pathOverrides ?? {}),
    ...Object.keys(config.reportedUsage.pathOverrides ?? {}),
    ...config.definedCacheRoutes,
  ])).sort()

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

  const setRuntime = <K extends keyof RuntimeConfig>(key: K) => (value: RuntimeConfig[K]) => {
    onChange({ ...config, [key]: value })
  }

  const mergedPolicyForPath = (prefix: string): CacheRoutePolicyPatch => ({
    ...(cachePolicy.pathOverrides?.[prefix] ?? {}),
    reportedUsage: cachePolicy.pathOverrides?.[prefix]?.reportedUsage ?? config.reportedUsage.pathOverrides[prefix],
  })

  const setPathPolicy = (prefix: string, nextPolicy: CacheRoutePolicyPatch) => {
    const pathOverrides = { ...(cachePolicy.pathOverrides ?? {}) }
    const reportedPathOverrides = { ...config.reportedUsage.pathOverrides }
    delete reportedPathOverrides[prefix]
    if (isEmptyRoutePatch(nextPolicy)) {
      delete pathOverrides[prefix]
    } else {
      pathOverrides[prefix] = nextPolicy
    }
    updateCachePolicy(
      { ...cachePolicy, pathOverrides },
      { ...config.reportedUsage, pathOverrides: reportedPathOverrides }
    )
  }

  const addPath = () => {
    const prefix = normalizeCachePolicyPath(newPath)
    if (!prefix) {
      setError('请输入路径前缀，例如 /cc 或 /dfcache/team-a')
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
          [prefix]: defaultPathCachePatch(prefix),
        },
      },
      config.reportedUsage,
      normalizedDefinedRoutesWith(config.definedCacheRoutes, prefix, Boolean(normalizeDefinedCacheRoute(prefix)))
    )
  }

  const renamePath = (oldPrefix: string, nextPrefix: string) => {
    if (oldPrefix === nextPrefix) return
    if (paths.includes(nextPrefix)) {
      setError(`${nextPrefix} 已存在`)
      return
    }
    const pathOverrides = { ...(cachePolicy.pathOverrides ?? {}) }
    const policy = mergedPolicyForPath(oldPrefix)
    delete pathOverrides[oldPrefix]
    if (!isEmptyRoutePatch(policy)) {
      pathOverrides[nextPrefix] = policy
    }
    const reportedPathOverrides = { ...config.reportedUsage.pathOverrides }
    delete reportedPathOverrides[oldPrefix]
    setError(null)
    updateCachePolicy(
      { ...cachePolicy, pathOverrides },
      { ...config.reportedUsage, pathOverrides: reportedPathOverrides },
      moveDefinedRoute(config.definedCacheRoutes, oldPrefix, nextPrefix)
    )
  }

  const deletePath = (prefix: string) => {
    const pathOverrides = { ...(cachePolicy.pathOverrides ?? {}) }
    delete pathOverrides[prefix]
    const reportedPathOverrides = { ...config.reportedUsage.pathOverrides }
    delete reportedPathOverrides[prefix]
    updateCachePolicy(
      { ...cachePolicy, pathOverrides },
      { ...config.reportedUsage, pathOverrides: reportedPathOverrides },
      normalizedDefinedRoutesWith(config.definedCacheRoutes, prefix, false)
    )
  }

  const setDefinedRoute = (prefix: string, enabled: boolean) => {
    updateCachePolicy(cachePolicy, config.reportedUsage, normalizedDefinedRoutesWith(config.definedCacheRoutes, prefix, enabled))
  }

  return (
    <div className="space-y-6">
      <div className="space-y-5 rounded-lg border border-border bg-background p-4">
        <div>
          <div className="text-sm font-semibold">默认缓存策略</div>
          <div className="mt-1 text-xs leading-5 text-muted-foreground">
            所有入口先使用这里的默认值；下方路径覆盖按最长前缀匹配后覆盖对应字段。
          </div>
        </div>
        <div className="space-y-3">
          <div className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">高缓存模拟</div>
          <TwoCol>
            <NumField label="缓存读取目标比例" desc="建议 0.95~0.99" value={config.promptCacheTargetReadRatio} min={0} max={0.99} step={0.01} suffix="比例" onChange={setRuntime('promptCacheTargetReadRatio')} />
            <NumField label="输入估算放大倍数" value={config.promptCacheTokenScale} min={1} max={3} step={0.1} suffix="倍" onChange={setRuntime('promptCacheTokenScale')} />
            <NumField label="输入展示上限" desc="0 表示不设上限" value={config.promptCacheMaxSimulatedInputTokens} min={0} suffix="Token" onChange={setRuntime('promptCacheMaxSimulatedInputTokens')} />
            <NumField label="放大启用门槛" value={config.promptCacheScaleMinInputTokens} min={0} suffix="Token" onChange={setRuntime('promptCacheScaleMinInputTokens')} />
            <NumField label="触顶扣减下限" value={config.promptCacheCapJitterMinTokens} min={0} suffix="Token" onChange={setRuntime('promptCacheCapJitterMinTokens')} />
            <NumField label="触顶扣减上限" value={config.promptCacheCapJitterMaxTokens} min={0} suffix="Token" onChange={setRuntime('promptCacheCapJitterMaxTokens')} />
          </TwoCol>
        </div>
        <div className="border-t border-border" />
        <div className="space-y-3">
          <div className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">缓存边界</div>
          <TwoCol>
            <NumField label="单账号条目上限" desc="每个账号最多保留多少个可复用缓存指纹。" value={config.promptCacheMaxEntriesPerAccount} min={0} suffix="条" onChange={setRuntime('promptCacheMaxEntriesPerAccount')} />
            <NumField label="全局条目上限" desc="所有账号合计最多保留多少个缓存指纹，0 表示不按条目数限制。" value={config.promptCacheMaxEntriesGlobal} min={0} suffix="条" onChange={setRuntime('promptCacheMaxEntriesGlobal')} />
            <NumField label="最长保留时间" desc="单条缓存指纹最多保留多久。" value={config.promptCacheEntryTtlSecs} min={1} suffix="秒" onChange={setRuntime('promptCacheEntryTtlSecs')} />
            <NumField label="估算内存上限" desc="达到估算上限后优先移除最久未使用的缓存指纹，0 表示不按内存估算限制。" value={config.promptCacheEstimatedBytesLimit} min={0} suffix="字节" onChange={setRuntime('promptCacheEstimatedBytesLimit')} />
          </TwoCol>
        </div>
        <div className="border-t border-border" />
        <div className="space-y-3">
          <div className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">真实 cachePoint</div>
          <TwoCol>
            <TogField label="发送真实 cachePoint" desc="把带缓存标记的工具发送给 Kiro 上游；上游不接受时会自动去掉后重试一次。" checked={config.kiroCachePointEnabled} onChange={setRuntime('kiroCachePointEnabled')} />
            <TogField label="只处理工具缓存标记" desc="只根据工具上的缓存标记插入 cachePoint，不改写系统消息或历史消息。" checked={config.kiroCachePointToolsOnly} disabled={!config.kiroCachePointEnabled} onChange={setRuntime('kiroCachePointToolsOnly')} />
            <TogField label="记录 cachePoint 计划" desc="在系统日志中记录插入数量，方便排查上游请求体错误。" checked={config.kiroCachePointRecordPlan} disabled={!config.kiroCachePointEnabled} onChange={setRuntime('kiroCachePointRecordPlan')} />
          </TwoCol>
        </div>
        <div className="border-t border-border" />
        <div className="space-y-3">
          <div className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">缓存创建频次</div>
          <CacheCreationSection control={config.promptCacheCreationControl} onChange={setRuntime('promptCacheCreationControl')} />
        </div>
        <div className="border-t border-border" />
        <div className="space-y-3">
          <div className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">用量展示</div>
          <PathPolicyEditor
            policy={config.reportedUsage.default}
            onChange={(defaultPolicy) => onChange({ ...config, reportedUsage: { ...config.reportedUsage, default: defaultPolicy } })}
          />
        </div>
      </div>

      <div className="flex flex-col gap-2 rounded-lg border border-border bg-muted/20 p-4 md:flex-row md:items-end">
        <div className="min-w-0 flex-1 space-y-1.5">
          <div className="text-sm font-semibold">新增路径策略</div>
          <Input
            placeholder="/cc、/ha 或 /dfcache/team-a"
            value={newPath}
            onChange={(event) => setNewPath(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === 'Enter') addPath()
            }}
          />
        </div>
        <Button type="button" onClick={addPath}>
          <Plus className="mr-1 h-4 w-4" />
          新增路径
        </Button>
      </div>
      {error && <div className="text-xs text-destructive">{error}</div>}
      {paths.length === 0 ? (
        <div className="rounded-lg border border-dashed border-border p-6 text-sm text-muted-foreground">
          暂无路径覆盖。当前所有入口都会使用默认缓存策略。
        </div>
      ) : (
        <div className="space-y-4">
          {paths.map((prefix) => (
            <PathCachePolicyCard
              key={prefix}
              prefix={prefix}
              policy={mergedPolicyForPath(prefix)}
              definedRoutes={config.definedCacheRoutes}
              onPrefixChange={(nextPrefix) => renamePath(prefix, nextPrefix)}
              onDelete={() => deletePath(prefix)}
              onChange={(nextPolicy) => setPathPolicy(prefix, nextPolicy)}
              onDefinedRouteChange={(enabled) => setDefinedRoute(prefix, enabled)}
            />
          ))}
        </div>
      )}
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
      <TogField label="启用内容清理" desc="对历史消息做体积优化,降低上游压力" checked={shaping.enabled} disabled={!payloadSizeLimitEnabled} onChange={set('enabled')} />
      <TwoCol>
        <TogField label="截短历史工具结果" desc="保留头尾,中间省略" checked={shaping.truncateHistoricalToolResults} disabled={!branchEnabled} onChange={set('truncateHistoricalToolResults')} />
        <NumField label="历史工具结果保留字符" value={shaping.historicalToolResultMaxChars} min={0} suffix="字符" disabled={!branchEnabled} onChange={set('historicalToolResultMaxChars')} />
      </TwoCol>
      <TwoCol>
        <NumField label="保留头部行数" value={shaping.historicalToolResultHeadLines} min={0} suffix="行" disabled={!branchEnabled} onChange={set('historicalToolResultHeadLines')} />
        <NumField label="保留尾部行数" value={shaping.historicalToolResultTailLines} min={0} suffix="行" disabled={!branchEnabled} onChange={set('historicalToolResultTailLines')} />
      </TwoCol>
      <TwoCol>
        <TogField label="移除历史思考内容" desc="丢弃历史消息里的 thinking 块" checked={shaping.discardHistoricalThinking} disabled={!branchEnabled} onChange={set('discardHistoricalThinking')} />
        <TogField label="压缩工具说明" desc="精简工具定义体积" checked={shaping.compressToolDefinitions} disabled={!branchEnabled} onChange={set('compressToolDefinitions')} />
      </TwoCol>
      <TwoCol>
        <NumField label="工具说明大小上限" value={shaping.toolDefinitionsBudgetBytes} min={0} suffix="字节" disabled={!branchEnabled} onChange={set('toolDefinitionsBudgetBytes')} />
        <NumField label="单工具描述上限" value={shaping.toolDescriptionMaxChars} min={0} suffix="字符" disabled={!branchEnabled} onChange={set('toolDescriptionMaxChars')} />
      </TwoCol>
      <TwoCol>
        <NumField label="工具 Schema 注解上限" value={shaping.toolSchemaAnnotationMaxChars} min={0} suffix="字符" disabled={!branchEnabled} onChange={set('toolSchemaAnnotationMaxChars')} />
        <TogField label="清理网页抓取历史" desc="截短历史 web fetch 正文" checked={shaping.webFetchTrimEnabled} disabled={!branchEnabled} onChange={set('webFetchTrimEnabled')} />
      </TwoCol>
      <TwoCol>
        <NumField label="网页抓取正文保留字符" value={shaping.webFetchBodyMaxChars} min={0} suffix="字符" disabled={!branchEnabled} onChange={set('webFetchBodyMaxChars')} />
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
      <TogField label="自动压缩当前内容" desc="当前请求超阈值时,按下列规则压缩当前消息" checked={shaping.fitCurrentPayloadToBudget} disabled={dis} onChange={set('fitCurrentPayloadToBudget')} />
      <TwoCol>
        <TogField label="截短当前工具结果" checked={shaping.truncateCurrentToolResults} disabled={dis} onChange={set('truncateCurrentToolResults')} />
        <NumField label="当前工具结果保留字符" value={shaping.currentToolResultMaxChars} min={0} suffix="字符" disabled={dis} onChange={set('currentToolResultMaxChars')} />
      </TwoCol>
      <TwoCol>
        <TogField label="截短当前用户文本" checked={shaping.truncateCurrentUserContent} disabled={dis} onChange={set('truncateCurrentUserContent')} />
        <NumField label="当前用户文本保留字符" value={shaping.currentUserContentMaxChars} min={0} suffix="字符" disabled={dis} onChange={set('currentUserContentMaxChars')} />
      </TwoCol>
      <TwoCol>
        <TogField label="截短当前文档" checked={shaping.truncateCurrentDocuments} disabled={dis} onChange={set('truncateCurrentDocuments')} />
        <NumField label="当前文档保留字符" value={shaping.currentDocumentMaxChars} min={0} suffix="字符" disabled={dis} onChange={set('currentDocumentMaxChars')} />
      </TwoCol>
      <TwoCol>
        <TogField label="移除当前图片" checked={shaping.truncateCurrentImages} disabled={dis} onChange={set('truncateCurrentImages')} />
        <NumField label="当前图片保留大小" value={shaping.currentImagesMaxBytes} min={0} suffix="字节" disabled={dis} onChange={set('currentImagesMaxBytes')} />
      </TwoCol>
      <div className="space-y-1.5">
        <div className="text-sm font-semibold text-foreground">单图超 5MB 处理</div>
        <div className="text-xs leading-relaxed text-muted-foreground">图片超过上游单图限制时，选择移除并给模型占位说明，或直接返回请求错误。</div>
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
      <TogField label="启用缓存创建频次控制" desc="限制缓存写入展示的节奏" checked={control.enabled} onChange={set('enabled')} />
      <div className="space-y-1.5">
        <div className="text-sm font-semibold">控制维度</div>
        <Select value={control.scopeMode} disabled={!control.enabled} onValueChange={(v) => set('scopeMode')(v as PromptCacheCreationControlConfig['scopeMode'])}>
          <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value="credential_conversation_model">账号 + 会话 + 模型</SelectItem>
            <SelectItem value="conversation_model">会话 + 模型</SelectItem>
          </SelectContent>
        </Select>
      </div>
      <TwoCol>
        <NumField label="最小成功请求间隔" value={control.minSuccessfulRequestsBetweenCreation} min={0} suffix="次" disabled={!control.enabled} onChange={set('minSuccessfulRequestsBetweenCreation')} />
        <NumField label="最小时间间隔" value={control.minCreationIntervalSecs} min={0} suffix="秒" disabled={!control.enabled} onChange={set('minCreationIntervalSecs')} />
      </TwoCol>
      <TwoCol>
        <NumField label="最小累计增量" value={control.minCreationDeltaTokens} min={0} suffix="Token" disabled={!control.enabled} onChange={set('minCreationDeltaTokens')} />
        <NumField label="单次展示上限" value={control.maxCreationTokensPerEvent} min={0} suffix="Token" disabled={!control.enabled} onChange={set('maxCreationTokensPerEvent')} />
      </TwoCol>
      <TwoCol>
        <NumField label="额度窗口长度" value={control.creationBudgetWindowSecs} min={0} suffix="秒" disabled={!control.enabled} onChange={set('creationBudgetWindowSecs')} />
        <NumField label="窗口展示额度" value={control.maxCreationTokensPerWindow} min={0} suffix="Token" disabled={!control.enabled} onChange={set('maxCreationTokensPerWindow')} />
      </TwoCol>
      <TwoCol>
        <NumField label="空闲后清理状态" value={control.expireAfterIdleSecs} min={0} suffix="秒" disabled={!control.enabled} onChange={set('expireAfterIdleSecs')} />
      </TwoCol>
    </div>
  )
}

// ─── 用量上报字段策略 ─────────────────────────────────────────────────────────

const FIELD_MODE_LABELS: Record<string, string> = {
  raw: '原始返回',
  preserve: '保留口径',
  'sample-max': '采样封顶',
  'sample-target': '采样目标',
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
    <div className="rounded-lg border border-border bg-muted/20 p-3 space-y-3">
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
      {fieldNeedsMax(policy) && (
        <NumField label="封顶 Token" value={policy.maxTokens} min={0} suffix="Token" onChange={set('maxTokens')} />
      )}
      {fieldNeedsTarget(policy) && (
        <TwoCol>
          <NumField label="目标 Token" value={policy.targetTokens} min={0} suffix="Token" onChange={set('targetTokens')} />
          <NumField label="常规上限倍数" value={policy.normalMaxMultiplier} min={0} step={0.1} suffix="倍" onChange={set('normalMaxMultiplier')} />
        </TwoCol>
      )}
      {allowMoveDelta && (
        <TogField
          label="差额计入缓存读取"
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
      <TogField label="启用本规则" checked={policy.enabled} onChange={set('enabled')} />
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
            <NumField label="读取缓存最终上限" value={policy.finalCacheReadMaxTokens} min={0} suffix="Token" onChange={set('finalCacheReadMaxTokens')} />
            <NumField label="最终上限扣减下限" value={policy.finalCacheReadJitterMinTokens} min={0} suffix="Token" onChange={set('finalCacheReadJitterMinTokens')} />
          </TwoCol>
          <TwoCol>
            <NumField label="最终上限扣减上限" value={policy.finalCacheReadJitterMaxTokens} min={0} suffix="Token" onChange={set('finalCacheReadJitterMaxTokens')} />
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
        <TogField label="启用模型映射" desc="把客户端模型名映射到实际模型" checked={mapping.enabled} onChange={(v) => onChange({ ...mapping, enabled: v })} />
        <TogField label="自动生成规则" desc="按可用模型自动补充版本兼容规则" checked={mapping.autoGenerateRules} onChange={(v) => onChange({ ...mapping, autoGenerateRules: v })} />
      </TwoCol>
      <div className="space-y-2">
        <div className="flex items-center justify-between gap-2">
          <span className="text-sm font-semibold">映射规则(JSON)</span>
          <div className="flex items-center gap-2 text-xs text-muted-foreground">
            <span>当前 {mapping.rules.length} 条 · 可生成 {defaultRules.length} 条</span>
            <Button size="sm" variant="outline" onClick={fillDefault}>填充默认规则</Button>
          </div>
        </div>
        <Textarea value={text} rows={12} className="font-mono text-xs"
          onChange={(e) => commitText(e.target.value)} />
        {error && <div className="text-xs text-destructive">规则解析错误:{error}</div>}
        <div className="text-xs text-muted-foreground">
          每条规则字段:source(来源名)、target(目标模型)、kind(version_equivalent / alias / fallback)、enabled、note。
        </div>
      </div>
    </div>
  )
}

// ─── /dfcache 路径归一化 ─────────────────────────────────────────────────────

const DFCACHE_ROUTE_PREFIX = '/dfcache/'

/** 归一化为合法的 /dfcache/<name>，非法返回 null。name 仅允许 a-z 0-9 . _ -，≤64 */
export function normalizeDefinedCacheRoute(route: string): string | null {
  const trimmed = route.trim()
  if (!trimmed) return null
  const withSlash = trimmed.startsWith('/') ? trimmed : `/${trimmed}`
  const normalized = withSlash.replace(/\/+$/, '').toLowerCase()
  const name = normalized.startsWith(DFCACHE_ROUTE_PREFIX)
    ? normalized.slice(DFCACHE_ROUTE_PREFIX.length)
    : ''
  if (!name || name.includes('/') || name.length > 64 || !/^[a-z0-9._-]+$/.test(name)) {
    return null
  }
  return `${DFCACHE_ROUTE_PREFIX}${name}`
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
