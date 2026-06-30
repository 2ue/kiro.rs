import { Copy, Edit3, Eye, EyeOff, Gauge, KeyRound, Plus, Save, Shield, Sparkles, Trash2, Wand2, X, Zap } from 'lucide-react'
import { useEffect, useState } from 'react'
import { toast } from 'sonner'
import { Alert, Button, Input, Join, Loading, Toggle } from 'react-daisyui'
import { ErrorState, FieldLabel, Select, useConfirm } from '@/components/ui'
import {
  defaultModelMappingConfig,
  defaultPayloadShaping,
  defaultExternalPoolsConfig,
  defaultPromptCacheCreationControl,
  DFCACHE_ROUTE_PREFIX,
  emptyRuntimeConfig,
  fieldNeedsMax,
  fieldNeedsTarget,
  inputSamplePolicy,
  normalizeDefinedCacheRoute,
  normalizeDefinedCacheRoutes,
  normalizePayloadShaping,
  normalizeCachePolicy,
  normalizePromptCacheCreationControl,
  normalizeReportedUsage,
  pathPolicy,
  preserveFieldPolicy,
  reportedUsageModeDescription,
  toRatio,
  toScale,
  toWhole,
} from '@/lib/runtime-config-defaults'
import { extractErrorMessage } from '@/lib/utils'
import { createRequestApiKey, deleteRequestApiKey, getAccessKeys, updateAdminApiKey, updateRequestApiKey } from '@/api/credentials'
import { useRuntimeConfig, useUpdateRuntimeConfig } from '@/hooks/use-credentials'
import { useModelCapabilities } from '@/hooks/use-usage'
import { storage } from '@/lib/storage'
import type {
  AccessKeysResponse,
  CachePolicyConfig,
  CacheRoutePolicyPatch,
  CompatProfile,
  KiroAgentModeStrategy,
  ModelCapabilitiesStatus,
  ModelMappingConfig,
  ModelMappingRule,
  ModelResolutionMode,
  PayloadGuardMode,
  PayloadShapingConfig,
  PromptCacheCreationControlConfig,
  ReportedUsageFieldMode,
  ReportedUsageFieldPolicy,
  ReportedUsagePathPolicy,
  RequestApiKeyItem,
  RuntimeConfig,
} from '@/types/api'

type ConfigTab =
  | 'access'
  | 'limits'
  | 'cooldown'
  | 'scheduler'
  | 'warmup'
  | 'payload'
  | 'payloadHistory'
  | 'payloadFallback'
  | 'cachePolicy'
  | 'compat'
  | 'stats'

const configTabs: Array<{ key: ConfigTab; label: string; description: string; icon: React.ReactNode }> = [
  { key: 'access', label: '接入与登录', description: '客户端 Key、后台登录密码', icon: <KeyRound className="h-4 w-4" /> },
  { key: 'limits', label: '请求容量', description: '并发、排队、重试、超时', icon: <Gauge className="h-4 w-4" /> },
  { key: 'cooldown', label: '错误恢复', description: '不同错误后的暂停策略', icon: <Shield className="h-4 w-4" /> },
  { key: 'scheduler', label: '账号选择', description: '负载、错误、延迟权重', icon: <Gauge className="h-4 w-4" /> },
  { key: 'warmup', label: '新账号预热', description: '新账号逐步参与请求', icon: <Sparkles className="h-4 w-4" /> },
  { key: 'payload', label: '大小保护', description: '压缩、阈值和处理时机', icon: <Wand2 className="h-4 w-4" /> },
  { key: 'payloadHistory', label: '旧内容清理', description: '历史消息、工具和网页内容', icon: <Wand2 className="h-4 w-4" /> },
  { key: 'payloadFallback', label: '当前内容兜底', description: '当前消息、文档和图片', icon: <Wand2 className="h-4 w-4" /> },
  { key: 'cachePolicy', label: '缓存策略', description: '默认配置和路径覆盖', icon: <Zap className="h-4 w-4" /> },
  { key: 'compat', label: '兼容与模型', description: '兼容模式和模型映射', icon: <Shield className="h-4 w-4" /> },
  { key: 'stats', label: '后台统计', description: '页面统计判断标准', icon: <Gauge className="h-4 w-4" /> },
]

function normalizeModelMapping(config?: Partial<ModelMappingConfig> | null): ModelMappingConfig {
  return {
    ...defaultModelMappingConfig(),
    ...(config || {}),
    rules: (config?.rules || [])
      .map((rule) => ({
        enabled: rule.enabled !== false,
        source: rule.source.trim().toLowerCase(),
        target: rule.target.trim().toLowerCase(),
        kind: rule.kind || 'alias',
        note: rule.note?.trim() || null,
      }))
      .filter((rule) => rule.source && rule.target),
  }
}

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
  for (let index = 0; index < len; index += 1) {
    const delta = (av[index] || 0) - (bv[index] || 0)
    if (delta !== 0) return delta
  }
  if (a.endsWith('-thinking') !== b.endsWith('-thinking')) return a.endsWith('-thinking') ? -1 : 1
  return a.localeCompare(b)
}

function addModelRule(rules: ModelMappingRule[], rule: ModelMappingRule) {
  const source = rule.source.trim().toLowerCase()
  const target = rule.target.trim().toLowerCase()
  if (!source || !target || source === target) return
  if (rules.some((item) => item.source === source && item.target === target && item.kind === rule.kind)) return
  rules.push({ ...rule, source, target, enabled: rule.enabled !== false })
}

function generateDefaultModelMappingRules(status?: ModelCapabilitiesStatus): ModelMappingRule[] {
  const models = (status?.models || []).map((item) => item.model.trim().toLowerCase()).filter(Boolean)
  const rules: ModelMappingRule[] = []
  for (const model of models) {
    const source = versionEquivalentSource(model)
    if (source) {
      addModelRule(rules, {
        enabled: true,
        source,
        target: model,
        kind: 'version_equivalent',
        note: '由当前可用模型列表生成的版本名兼容规则',
      })
    }
  }
  const pickFamily = (family: 'opus' | 'sonnet' | 'haiku') => {
    const sorted = models
      .filter((model) => model === family || model.startsWith(`claude-${family}`))
      .sort(compareModelId)
    return sorted[sorted.length - 1]
  }
  const opus = pickFamily('opus')
  const sonnet = pickFamily('sonnet')
  const haiku = pickFamily('haiku')
  for (const source of ['opus', 'opusplan', 'best', 'default', 'auto']) {
    if (opus) addModelRule(rules, { enabled: true, source, target: opus, kind: 'alias', note: '由当前可用 Opus 模型生成的默认别名' })
  }
  if (sonnet) addModelRule(rules, { enabled: true, source: 'sonnet', target: sonnet, kind: 'alias', note: '由当前可用 Sonnet 模型生成的默认别名' })
  if (haiku) addModelRule(rules, { enabled: true, source: 'haiku', target: haiku, kind: 'alias', note: '由当前可用 Haiku 模型生成的默认别名' })
  return rules
}

function numberValue(value: string, fallback: number): number {
  const parsed = Number(value)
  return Number.isFinite(parsed) ? parsed : fallback
}

function NumberField({
  title,
  description,
  value,
  disabled,
  min,
  max,
  step,
  suffix,
  onChange,
}: {
  title: string
  description: string
  value: number
  disabled?: boolean
  min?: number
  max?: number
  step?: number
  suffix: string
  onChange: (value: number) => void
}) {
  return (
    <FieldLabel title={title} description={description}>
      <Join className="w-full">
        <Input
          bordered
          size="sm"
          type="number"
          className="join-item w-full"
          value={value}
          min={min}
          max={max}
          step={step}
          inputMode={step ? 'decimal' : 'numeric'}
          disabled={disabled}
          onChange={(event) => onChange(numberValue(event.target.value, min ?? 0))}
        />
        <span className="join-item unit-addon min-w-20">{suffix}</span>
      </Join>
    </FieldLabel>
  )
}

function ToggleField({
  title,
  description,
  checked,
  disabled,
  onChange,
}: {
  title: string
  description: string
  checked: boolean
  disabled?: boolean
  onChange: (value: boolean) => void
}) {
  return (
    <div className="settings-row">
      <div className="min-w-0">
        <div className="text-sm font-semibold">{title}</div>
        <div className="mt-0.5 text-xs leading-4 text-base-content/60">{description}</div>
      </div>
      <Toggle color="primary" size="sm" className="shrink-0" checked={checked} disabled={disabled} onChange={(event) => onChange(event.target.checked)} />
    </div>
  )
}

function normalizeCachePolicyPathPrefix(prefix: string): string | null {
  const trimmed = prefix.trim()
  if (!trimmed) return null
  const withSlash = trimmed.startsWith('/') ? trimmed : `/${trimmed}`
  return withSlash.replace(/\/+$/, '') || '/'
}

type CacheSimulationPatch = NonNullable<CacheRoutePolicyPatch['simulation']>
type CachePointPatch = NonNullable<CacheRoutePolicyPatch['cachePoint']>
type CacheBoundsPatch = NonNullable<CacheRoutePolicyPatch['bounds']>

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
    <div className="space-y-3 rounded-lg border border-base-300 bg-base-100/70 p-4">
      <ToggleField title="启用高缓存模拟" description="只覆盖当前入口的高缓存模拟开关。" checked={merged.enabled ?? true} onChange={set('enabled')} />
      <div className="grid gap-3 xl:grid-cols-2">
        <NumberField title="目标缓存读取比例" description="当前入口希望展示为缓存读取的比例。" value={merged.targetReadRatio ?? 0.98} min={0} max={0.99} step={0.01} suffix="比例" onChange={set('targetReadRatio')} />
        <NumberField title="输入放大倍数" description="当前入口高缓存模拟时 total input 的放大程度。" value={merged.tokenScale ?? 1.6} min={1} max={3} step={0.1} suffix="倍" onChange={set('tokenScale')} />
        <NumberField title="模拟输入上限" description="当前入口模拟后的 total input 最高值，填 0 表示不限制。" value={merged.maxSimulatedInputTokens ?? 300000} min={0} suffix="Token" onChange={set('maxSimulatedInputTokens')} />
        <NumberField title="放大生效门槛" description="输入达到多少 Token 后才进行放大模拟。" value={merged.scaleMinInputTokens ?? 20000} min={0} suffix="Token" onChange={set('scaleMinInputTokens')} />
        <NumberField title="上限扣减下限" description="触顶后随机扣减的最小 Token 数。" value={merged.capJitterMinTokens ?? 12000} min={0} suffix="Token" onChange={set('capJitterMinTokens')} />
        <NumberField title="上限扣减上限" description="触顶后随机扣减的最大 Token 数。" value={merged.capJitterMaxTokens ?? 24000} min={0} suffix="Token" onChange={set('capJitterMaxTokens')} />
      </div>
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
    <div className="space-y-3 rounded-lg border border-base-300 bg-base-100/70 p-4">
      <ToggleField title="启用缓存创建频次控制" description="只覆盖当前入口的缓存写入展示节奏。" checked={merged.enabled} onChange={set('enabled')} />
      <FieldLabel title="控制维度" description="决定缓存创建频次按什么范围累计。">
        <Select value={merged.scopeMode} disabled={!merged.enabled} size="sm" onChange={(event) => set('scopeMode')(event.target.value as PromptCacheCreationControlConfig['scopeMode'])}>
          <Select.Option value="conversation_model">会话 + 模型</Select.Option>
          <Select.Option value="credential_conversation_model">账号 + 会话 + 模型</Select.Option>
        </Select>
      </FieldLabel>
      <div className="grid gap-3 xl:grid-cols-2">
        <NumberField title="最小成功请求间隔" description="两次缓存创建展示之间至少间隔多少次成功请求。" value={merged.minSuccessfulRequestsBetweenCreation} disabled={!merged.enabled} min={0} suffix="次" onChange={set('minSuccessfulRequestsBetweenCreation')} />
        <NumberField title="最小时间间隔" description="两次缓存创建展示之间至少间隔多少秒。" value={merged.minCreationIntervalSecs} disabled={!merged.enabled} min={0} suffix="秒" onChange={set('minCreationIntervalSecs')} />
        <NumberField title="最小累计增量" description="累计新增缓存写入达到多少 Token 后才展示。" value={merged.minCreationDeltaTokens} disabled={!merged.enabled} min={0} suffix="Token" onChange={set('minCreationDeltaTokens')} />
        <NumberField title="单次展示上限" description="单次最多展示多少缓存创建 Token。" value={merged.maxCreationTokensPerEvent} disabled={!merged.enabled} min={0} suffix="Token" onChange={set('maxCreationTokensPerEvent')} />
        <NumberField title="额度窗口长度" description="统计缓存创建展示额度的时间窗口。" value={merged.creationBudgetWindowSecs} disabled={!merged.enabled} min={0} suffix="秒" onChange={set('creationBudgetWindowSecs')} />
        <NumberField title="窗口展示额度" description="一个窗口内最多展示多少缓存创建 Token。" value={merged.maxCreationTokensPerWindow} disabled={!merged.enabled} min={0} suffix="Token" onChange={set('maxCreationTokensPerWindow')} />
        <NumberField title="空闲后清理状态" description="入口长时间没有请求后，清理累计状态。" value={merged.expireAfterIdleSecs} disabled={!merged.enabled} min={0} suffix="秒" onChange={set('expireAfterIdleSecs')} />
      </div>
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
    <div className="space-y-3 rounded-lg border border-base-300 bg-base-100/70 p-4">
      <ToggleField title="发送真实 cachePoint" description="把带缓存标记的工具发送给 Kiro 上游；上游不接受时会自动去掉后重试一次。" checked={merged.enabled ?? false} onChange={set('enabled')} />
      <div className="grid gap-3 xl:grid-cols-2">
        <ToggleField title="只处理工具缓存标记" description="只根据工具上的缓存标记插入 cachePoint，不改写系统消息或历史消息。" checked={merged.toolsOnly ?? true} disabled={!merged.enabled} onChange={set('toolsOnly')} />
        <ToggleField title="记录 cachePoint 计划" description="在系统日志中记录插入数量，方便排查上游请求体错误。" checked={merged.recordPlan ?? true} disabled={!merged.enabled} onChange={set('recordPlan')} />
      </div>
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
    <div className="space-y-3 rounded-lg border border-base-300 bg-base-100/70 p-4">
      <div className="grid gap-3 xl:grid-cols-2">
        <NumberField title="单账号条目上限" description="当前入口每个账号最多保留多少个可复用缓存指纹。" value={merged.maxEntriesPerAccount ?? 200} min={0} suffix="条" onChange={set('maxEntriesPerAccount')} />
        <NumberField title="全局条目上限" description="当前入口所有账号合计最多保留多少个缓存指纹。" value={merged.maxEntriesGlobal ?? 20000} min={0} suffix="条" onChange={set('maxEntriesGlobal')} />
        <NumberField title="最长保留时间" description="当前入口单条缓存指纹最多保留多久。" value={merged.entryTtlSecs ?? 86400} min={1} suffix="秒" onChange={set('entryTtlSecs')} />
        <NumberField title="估算内存上限" description="当前入口达到估算上限后优先移除最久未使用的缓存指纹。" value={merged.estimatedBytesLimit ?? 268435456} min={0} suffix="字节" onChange={set('estimatedBytesLimit')} />
      </div>
    </div>
  )
}

function CachePatchBlock({
  title,
  description,
  enabled,
  onSet,
  onClear,
  children,
}: {
  title: string
  description: string
  enabled: boolean
  onSet: () => void
  onClear: () => void
  children: React.ReactNode
}) {
  return (
    <div className="space-y-3 rounded-lg border border-base-300 bg-base-200/40 p-4">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h4 className="text-sm font-semibold">{title}</h4>
          <p className="mt-1 text-xs leading-5 text-base-content/60">{description}</p>
        </div>
        {enabled ? (
          <Button type="button" variant="outline" size="xs" onClick={onClear}>清除覆盖</Button>
        ) : (
          <Button type="button" variant="outline" size="xs" onClick={onSet}>设置覆盖</Button>
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
    const normalized = normalizeCachePolicyPathPrefix(draftPrefix)
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
    <div className="space-y-4 rounded-lg border border-base-300 bg-base-100 p-4">
      <div className="flex flex-col gap-3 lg:flex-row lg:items-end lg:justify-between">
        <FieldLabel title="路径前缀">
          <Input
            bordered
            size="sm"
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
          {prefixError && <div className="mt-2 text-xs text-error">{prefixError}</div>}
        </FieldLabel>
        <Button type="button" color="error" variant="outline" size="sm" onClick={onDelete}>
          <Trash2 className="h-4 w-4" />
          删除路径
        </Button>
      </div>

      {isDfcachePath && (
        <div className="rounded-lg border border-base-300 bg-base-200/40 p-4">
          <ToggleField
            title="注册为 /dfcache 路由"
            description={normalizedDefinedRoute ? '开启后允许客户端访问这个 /dfcache/{name} 入口。' : '路径必须是 /dfcache/{name}，name 仅允许小写字母、数字、点、下划线或短横线。'}
            checked={isRouteRegistered}
            disabled={!normalizedDefinedRoute}
            onChange={onDefinedRouteChange}
          />
        </div>
      )}

      <CachePatchBlock
        title="高缓存模拟"
        description="不设置时沿用默认策略里的高缓存模拟参数。"
        enabled={Boolean(policy.simulation)}
        onSet={() => setSimulation(defaultSimulationPatch())}
        onClear={() => setSimulation(undefined)}
      >
        {policy.simulation && <SimulationOverrideForm value={policy.simulation} onChange={setSimulation} />}
      </CachePatchBlock>

      <CachePatchBlock
        title="缓存创建频次"
        description="不设置时沿用默认策略里的缓存创建频次。"
        enabled={Boolean(policy.creationControl)}
        onSet={() => setCreationControl(defaultPromptCacheCreationControl())}
        onClear={() => setCreationControl(undefined)}
      >
        {policy.creationControl && <CreationControlOverrideForm value={policy.creationControl} onChange={setCreationControl} />}
      </CachePatchBlock>

      <CachePatchBlock
        title="用量展示"
        description="控制这个路径返回给客户端和后台记录的 input、output、cache read、cache write 口径。"
        enabled={Boolean(policy.reportedUsage)}
        onSet={() => setReportedUsage(defaultUsagePatch(prefix))}
        onClear={() => setReportedUsage(undefined)}
      >
        {policy.reportedUsage && (
          <ReportedUsagePathEditor
            title={`${prefix || '/'} 用量展示`}
            description="只影响这个路径前缀匹配到的请求。"
            value={policy.reportedUsage}
            onChange={setReportedUsage}
          />
        )}
      </CachePatchBlock>

      <CachePatchBlock
        title="真实 cachePoint"
        description="不设置时沿用默认策略里的 cachePoint 开关。"
        enabled={Boolean(policy.cachePoint)}
        onSet={() => setCachePoint(defaultCachePointPatch())}
        onClear={() => setCachePoint(undefined)}
      >
        {policy.cachePoint && <CachePointOverrideForm value={policy.cachePoint} onChange={setCachePoint} />}
      </CachePatchBlock>

      <CachePatchBlock
        title="缓存边界"
        description="按路径覆盖缓存指纹条目、保留时间和估算内存上限。"
        enabled={Boolean(policy.bounds)}
        onSet={() => setBounds(defaultBoundsPatch())}
        onClear={() => setBounds(undefined)}
      >
        {policy.bounds && <CacheBoundsOverrideForm value={policy.bounds} onChange={setBounds} />}
      </CachePatchBlock>
    </div>
  )
}

function CachePolicyEditor({
  value,
  onChange,
}: {
  value: RuntimeConfig
  onChange: (value: RuntimeConfig) => void
}) {
  const [newPath, setNewPath] = useState('')
  const [error, setError] = useState<string | null>(null)
  const cachePolicy = value.cachePolicy
  const paths = Array.from(new Set([
    ...Object.keys(cachePolicy.pathOverrides ?? {}),
    ...Object.keys(value.reportedUsage.pathOverrides ?? {}),
    ...value.definedCacheRoutes,
  ])).sort()

  const updateCachePolicy = (
    nextCachePolicy: CachePolicyConfig,
    nextReportedUsage = value.reportedUsage,
    nextDefinedRoutes = value.definedCacheRoutes
  ) => {
    onChange({
      ...value,
      cachePolicy: nextCachePolicy,
      reportedUsage: nextReportedUsage,
      definedCacheRoutes: nextDefinedRoutes,
    })
  }

  const setRuntime = <K extends keyof RuntimeConfig>(key: K) => (nextValue: RuntimeConfig[K]) => {
    onChange({ ...value, [key]: nextValue })
  }

  const mergedPolicyForPath = (prefix: string): CacheRoutePolicyPatch => ({
    ...(cachePolicy.pathOverrides?.[prefix] ?? {}),
    reportedUsage: cachePolicy.pathOverrides?.[prefix]?.reportedUsage ?? value.reportedUsage.pathOverrides[prefix],
  })

  const setPathPolicy = (prefix: string, nextPolicy: CacheRoutePolicyPatch) => {
    const pathOverrides = { ...(cachePolicy.pathOverrides ?? {}) }
    const reportedPathOverrides = { ...value.reportedUsage.pathOverrides }
    delete reportedPathOverrides[prefix]
    if (isEmptyRoutePatch(nextPolicy)) {
      delete pathOverrides[prefix]
    } else {
      pathOverrides[prefix] = nextPolicy
    }
    updateCachePolicy(
      { ...cachePolicy, pathOverrides },
      { ...value.reportedUsage, pathOverrides: reportedPathOverrides }
    )
  }

  const addPath = () => {
    const prefix = normalizeCachePolicyPathPrefix(newPath)
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
      value.reportedUsage,
      normalizedDefinedRoutesWith(value.definedCacheRoutes, prefix, Boolean(normalizeDefinedCacheRoute(prefix)))
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
    const reportedPathOverrides = { ...value.reportedUsage.pathOverrides }
    delete reportedPathOverrides[oldPrefix]
    setError(null)
    updateCachePolicy(
      { ...cachePolicy, pathOverrides },
      { ...value.reportedUsage, pathOverrides: reportedPathOverrides },
      moveDefinedRoute(value.definedCacheRoutes, oldPrefix, nextPrefix)
    )
  }

  const deletePath = (prefix: string) => {
    const pathOverrides = { ...(cachePolicy.pathOverrides ?? {}) }
    delete pathOverrides[prefix]
    const reportedPathOverrides = { ...value.reportedUsage.pathOverrides }
    delete reportedPathOverrides[prefix]
    updateCachePolicy(
      { ...cachePolicy, pathOverrides },
      { ...value.reportedUsage, pathOverrides: reportedPathOverrides },
      normalizedDefinedRoutesWith(value.definedCacheRoutes, prefix, false)
    )
  }

  const setDefinedRoute = (prefix: string, enabled: boolean) => {
    updateCachePolicy(cachePolicy, value.reportedUsage, normalizedDefinedRoutesWith(value.definedCacheRoutes, prefix, enabled))
  }

  return (
    <div className="space-y-5">
      <div className="space-y-4 rounded-lg border border-base-300 bg-base-100/70 p-4">
        <div>
          <h3 className="text-sm font-semibold">默认缓存策略</h3>
          <p className="mt-1 text-xs leading-5 text-base-content/60">
            所有入口先使用这里的默认值；下方路径覆盖按最长前缀匹配后覆盖对应字段。
          </p>
        </div>
        <div className="config-inline-note md:col-span-2">
          <span className="config-note-label">高缓存模拟</span>
        </div>
        <div className="grid gap-3 xl:grid-cols-2">
          <NumberField title="缓存读取目标比例" description="希望缓存读取量大约占输入量的比例，常用 0.95 到 0.99。" value={value.promptCacheTargetReadRatio} min={0} max={0.99} step={0.01} suffix="比例" onChange={setRuntime('promptCacheTargetReadRatio')} />
          <NumberField title="输入估算放大倍数" description="用于估算缓存展示时的输入规模，不代表真实请求内容一定变大。" value={value.promptCacheTokenScale} min={1} max={3} step={0.1} suffix="倍" onChange={setRuntime('promptCacheTokenScale')} />
          <NumberField title="输入展示上限" description="估算后的输入量最高显示到多少。填 0 表示不设上限。" value={value.promptCacheMaxSimulatedInputTokens} min={0} suffix="Token" onChange={setRuntime('promptCacheMaxSimulatedInputTokens')} />
          <NumberField title="放大启用门槛" description="输入量达到多少后才开始放大估算。" value={value.promptCacheScaleMinInputTokens} min={0} suffix="Token" onChange={setRuntime('promptCacheScaleMinInputTokens')} />
          <NumberField title="触顶扣减下限" description="达到上限时，至少从上限扣掉多少，避免数值总是贴边。" value={value.promptCacheCapJitterMinTokens} min={0} suffix="Token" onChange={setRuntime('promptCacheCapJitterMinTokens')} />
          <NumberField title="触顶扣减上限" description="达到上限时，最多从上限扣掉多少。" value={value.promptCacheCapJitterMaxTokens} min={0} suffix="Token" onChange={setRuntime('promptCacheCapJitterMaxTokens')} />
        </div>
        <div className="border-t border-base-300" />
        <div className="config-inline-note md:col-span-2">
          <span className="config-note-label">缓存边界</span>
        </div>
        <div className="grid gap-3 xl:grid-cols-2">
          <NumberField title="单账号缓存条目上限" description="每个账号最多保留多少个可复用缓存指纹，防止长会话无限增长。" value={value.promptCacheMaxEntriesPerAccount} min={0} suffix="条" onChange={setRuntime('promptCacheMaxEntriesPerAccount')} />
          <NumberField title="全局缓存条目上限" description="所有账号合计最多保留多少个缓存指纹。填 0 表示不按条目数限制。" value={value.promptCacheMaxEntriesGlobal} min={0} suffix="条" onChange={setRuntime('promptCacheMaxEntriesGlobal')} />
          <NumberField title="缓存指纹保留时间" description="单条缓存指纹最多保留多久，实际不会超过上游缓存标记的时间。" value={value.promptCacheEntryTtlSecs} min={1} suffix="秒" onChange={setRuntime('promptCacheEntryTtlSecs')} />
          <NumberField title="缓存估算内存上限" description="达到估算上限后优先移除最久未使用的缓存指纹。填 0 表示不按内存估算限制。" value={value.promptCacheEstimatedBytesLimit} min={0} suffix="字节" onChange={setRuntime('promptCacheEstimatedBytesLimit')} />
        </div>
        <div className="border-t border-base-300" />
        <div className="config-inline-note md:col-span-2">
          <span className="config-note-label">真实 cachePoint</span>
        </div>
        <div className="grid gap-3 xl:grid-cols-2">
          <ToggleField title="发送真实 cachePoint" description="把带缓存标记的工具发送给 Kiro 上游；上游不接受时会自动去掉后重试一次。" checked={value.kiroCachePointEnabled} onChange={setRuntime('kiroCachePointEnabled')} />
          <ToggleField title="只处理工具缓存标记" description="只根据工具上的缓存标记插入 cachePoint，不改写系统消息或历史消息。" checked={value.kiroCachePointToolsOnly} disabled={!value.kiroCachePointEnabled} onChange={setRuntime('kiroCachePointToolsOnly')} />
          <ToggleField title="记录 cachePoint 计划" description="在系统日志中记录插入数量，方便排查上游请求体错误。" checked={value.kiroCachePointRecordPlan} disabled={!value.kiroCachePointEnabled} onChange={setRuntime('kiroCachePointRecordPlan')} />
        </div>
        <div className="border-t border-base-300" />
        <div className="config-inline-note md:col-span-2">
          <span className="config-note-label">缓存创建频次</span>
        </div>
        <div className="grid gap-3 xl:grid-cols-2">
          <ToggleField title="启用缓存创建频次控制" description="开启后，只影响缓存创建数值的展示频率。" checked={value.promptCacheCreationControl.enabled} onChange={(enabled) => onChange({ ...value, promptCacheCreationControl: { ...value.promptCacheCreationControl, enabled } })} />
          <FieldLabel title="控制维度" description="选择按账号分别控制，还是按同一个会话和模型统一控制。">
            <Select bordered size="sm" className="w-full" value={value.promptCacheCreationControl.scopeMode} disabled={!value.promptCacheCreationControl.enabled} onChange={(event) => onChange({ ...value, promptCacheCreationControl: { ...value.promptCacheCreationControl, scopeMode: event.target.value as 'credential_conversation_model' | 'conversation_model' } })}>
              <Select.Option value="credential_conversation_model">账号 + 会话 + 模型</Select.Option>
              <Select.Option value="conversation_model">会话 + 模型</Select.Option>
            </Select>
          </FieldLabel>
          <NumberField title="最小成功请求间隔" description="两次缓存创建展示之间至少间隔多少次成功请求。填 0 表示不限制。" value={value.promptCacheCreationControl.minSuccessfulRequestsBetweenCreation} disabled={!value.promptCacheCreationControl.enabled} min={0} suffix="次" onChange={(minSuccessfulRequestsBetweenCreation) => onChange({ ...value, promptCacheCreationControl: { ...value.promptCacheCreationControl, minSuccessfulRequestsBetweenCreation } })} />
          <NumberField title="最小时间间隔" description="两次缓存创建展示之间至少间隔多少秒。填 0 表示不限制。" value={value.promptCacheCreationControl.minCreationIntervalSecs} disabled={!value.promptCacheCreationControl.enabled} min={0} suffix="秒" onChange={(minCreationIntervalSecs) => onChange({ ...value, promptCacheCreationControl: { ...value.promptCacheCreationControl, minCreationIntervalSecs } })} />
          <NumberField title="最小累计增量" description="累计变化达到多少后，才允许下一次展示缓存创建数值。" value={value.promptCacheCreationControl.minCreationDeltaTokens} disabled={!value.promptCacheCreationControl.enabled} min={0} suffix="Token" onChange={(minCreationDeltaTokens) => onChange({ ...value, promptCacheCreationControl: { ...value.promptCacheCreationControl, minCreationDeltaTokens } })} />
          <NumberField title="单次展示上限" description="一次响应最多展示多少缓存创建量。填 0 表示不限制。" value={value.promptCacheCreationControl.maxCreationTokensPerEvent} disabled={!value.promptCacheCreationControl.enabled} min={0} suffix="Token" onChange={(maxCreationTokensPerEvent) => onChange({ ...value, promptCacheCreationControl: { ...value.promptCacheCreationControl, maxCreationTokensPerEvent } })} />
          <NumberField title="额度窗口长度" description="在这个时间窗口内统计缓存创建展示额度。填 0 表示关闭窗口限制。" value={value.promptCacheCreationControl.creationBudgetWindowSecs} disabled={!value.promptCacheCreationControl.enabled} min={0} suffix="秒" onChange={(creationBudgetWindowSecs) => onChange({ ...value, promptCacheCreationControl: { ...value.promptCacheCreationControl, creationBudgetWindowSecs } })} />
          <NumberField title="窗口展示额度" description="单个时间窗口内最多展示多少缓存创建量。填 0 表示不限制。" value={value.promptCacheCreationControl.maxCreationTokensPerWindow} disabled={!value.promptCacheCreationControl.enabled} min={0} suffix="Token" onChange={(maxCreationTokensPerWindow) => onChange({ ...value, promptCacheCreationControl: { ...value.promptCacheCreationControl, maxCreationTokensPerWindow } })} />
          <NumberField title="空闲后清理状态" description="长时间没有请求后清理控制状态。填 0 表示不按空闲时间清理。" value={value.promptCacheCreationControl.expireAfterIdleSecs} disabled={!value.promptCacheCreationControl.enabled} min={0} suffix="秒" onChange={(expireAfterIdleSecs) => onChange({ ...value, promptCacheCreationControl: { ...value.promptCacheCreationControl, expireAfterIdleSecs } })} />
        </div>
        <div className="border-t border-base-300" />
        <ReportedUsagePathEditor
          title="默认用量展示"
          description="没有匹配到单独路径策略时，使用这里的默认设置。"
          value={value.reportedUsage.default}
          onChange={(defaultPolicy) => onChange({ ...value, reportedUsage: { ...value.reportedUsage, default: defaultPolicy } })}
        />
      </div>

      <div className="space-y-3 rounded-lg border border-base-300 bg-base-100/70 p-4">
        <div className="flex flex-col gap-3 lg:flex-row lg:items-end">
          <FieldLabel title="新增路径策略">
            <Input
              bordered
              size="sm"
              placeholder="/cc、/ha 或 /dfcache/team-a"
              value={newPath}
              onChange={(event) => setNewPath(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === 'Enter') addPath()
              }}
            />
          </FieldLabel>
          <Button type="button" color="primary" size="sm" onClick={addPath}>
            <Plus className="h-4 w-4" />
            新增路径
          </Button>
        </div>
      </div>
      {error && <div className="text-xs text-error">{error}</div>}
      {paths.length === 0 ? (
        <div className="rounded-lg border border-dashed border-base-300 p-6 text-sm text-base-content/60">
          暂无路径覆盖。当前所有入口都会使用默认缓存策略。
        </div>
      ) : (
        <div className="space-y-4">
          {paths.map((prefix) => (
            <PathCachePolicyCard
              key={prefix}
              prefix={prefix}
              policy={mergedPolicyForPath(prefix)}
              definedRoutes={value.definedCacheRoutes}
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

function ConfigGroup({
  children,
}: {
  icon?: React.ReactNode
  title?: string
  description?: string
  children: React.ReactNode
}) {
  return (
    <section className="config-group">
      <div className="config-group-body">{children}</div>
    </section>
  )
}

function ImpactGroupHeader({
  label,
  title,
  description,
  muted = false,
}: {
  label: string
  title: string
  description: string
  muted?: boolean
}) {
  return (
    <div className={`config-inline-note md:col-span-2 ${muted ? 'is-muted' : ''}`}>
      <div className="mb-1 flex flex-wrap items-center gap-2">
        <span className="config-note-label">{label}</span>
        <span className="text-sm font-semibold">{title}</span>
      </div>
      <p className="text-xs leading-5 text-base-content/60">{description}</p>
    </div>
  )
}

function accessKeyItems(response: AccessKeysResponse | null): RequestApiKeyItem[] {
  if (!response) return []
  if (response.requestApiKeys?.length) return response.requestApiKeys
  if (!response.requestApiKey) return []
  return [{ id: 'legacy-primary', apiKey: response.requestApiKey, maskedApiKey: response.maskedRequestApiKey, primary: true }]
}

const REQUEST_API_KEY_PREFIX = 'sk-kiro-rs-'

function generateLocalRequestApiKey(): string {
  const bytes = new Uint8Array(32)
  const cryptoApi = globalThis.crypto
  if (cryptoApi?.getRandomValues) {
    cryptoApi.getRandomValues(bytes)
  } else {
    for (let index = 0; index < bytes.length; index += 1) {
      bytes[index] = Math.floor(Math.random() * 256)
    }
  }
  const binary = Array.from(bytes, (byte) => String.fromCharCode(byte)).join('')
  return `${REQUEST_API_KEY_PREFIX}${btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')}`
}

function AccessKeysPanel() {
  const [keys, setKeys] = useState<AccessKeysResponse | null>(null)
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [showAdminKey, setShowAdminKey] = useState(false)
  const [creating, setCreating] = useState(false)
  const [processingKeyId, setProcessingKeyId] = useState<string | null>(null)
  const [manualRequestApiKey, setManualRequestApiKey] = useState('')
  const [visibleRequestKeyIds, setVisibleRequestKeyIds] = useState<Set<string>>(new Set())
  const [editingRequestKeyId, setEditingRequestKeyId] = useState<string | null>(null)
  const [requestKeyDraft, setRequestKeyDraft] = useState('')
  const [nextAdminApiKey, setNextAdminApiKey] = useState('')
  const confirmDialog = useConfirm()

  const loadKeys = async () => {
    setLoading(true)
    try {
      setKeys(await getAccessKeys())
    } catch (error) {
      toast.error(`读取访问密钥失败: ${extractErrorMessage(error)}`)
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    void loadKeys()
  }, [])

  const copy = async (label: string, value?: string) => {
    if (!value) {
      toast.error(`${label} 为空，无法复制`)
      return
    }
    try {
      await navigator.clipboard.writeText(value)
      toast.success(`${label} 已复制`)
    } catch (error) {
      toast.error(`复制 ${label} 失败: ${extractErrorMessage(error)}`)
    }
  }

  const saveAdminApiKey = async () => {
    const adminApiKey = nextAdminApiKey.trim()
    if (!adminApiKey) {
      toast.error('请输入新的登录 Key')
      return
    }
    if (adminApiKey.length < 8) {
      toast.error('登录 Key 至少需要 8 个字符')
      return
    }
    setSaving(true)
    try {
      const response = await updateAdminApiKey({ adminApiKey })
      storage.setApiKey(response.adminApiKey)
      window.dispatchEvent(new CustomEvent('kiro-admin-key-updated'))
      setKeysAndResetDrafts(response)
      setNextAdminApiKey('')
      toast.success('登录 Key 已更新，后续后台请求会使用新 Key')
    } catch (error) {
      toast.error(`更新登录 Key 失败: ${extractErrorMessage(error)}`)
    } finally {
      setSaving(false)
    }
  }

  const requestKeys = accessKeyItems(keys)
  const adminApiKeyValue = showAdminKey ? keys?.adminApiKey : keys?.maskedAdminApiKey

  const setKeysAndResetDrafts = (response: AccessKeysResponse) => {
    setKeys(response)
    setEditingRequestKeyId(null)
    setRequestKeyDraft('')
    setVisibleRequestKeyIds((prev) => {
      const valid = new Set(accessKeyItems(response).map((item) => item.id))
      return new Set(Array.from(prev).filter((id) => valid.has(id)))
    })
  }

  const generateRequestKey = async () => {
    setCreating(true)
    try {
      const before = new Set(requestKeys.map((item) => item.id))
      const response = await createRequestApiKey({})
      setKeysAndResetDrafts(response)
      const created = accessKeyItems(response).find((item) => !before.has(item.id))
      if (created) {
        setVisibleRequestKeyIds((prev) => new Set(prev).add(created.id))
        await copy('新请求 Key', created.apiKey)
      }
      toast.success('请求 Key 已生成并立即生效')
    } catch (error) {
      toast.error(`生成请求 Key 失败: ${extractErrorMessage(error)}`)
    } finally {
      setCreating(false)
    }
  }

  const addManualRequestKey = async () => {
    const apiKey = manualRequestApiKey.trim()
    if (!apiKey) return toast.error('请输入要新增的请求 Key')
    if (apiKey.length < 8) return toast.error('请求 Key 至少需要 8 个字符')
    setCreating(true)
    try {
      const response = await createRequestApiKey({ apiKey })
      setKeysAndResetDrafts(response)
      setManualRequestApiKey('')
      toast.success('请求 Key 已新增并立即生效')
    } catch (error) {
      toast.error(`新增请求 Key 失败: ${extractErrorMessage(error)}`)
    } finally {
      setCreating(false)
    }
  }

  const startEditRequestKey = (item: RequestApiKeyItem) => {
    setEditingRequestKeyId(item.id)
    setRequestKeyDraft(item.apiKey)
  }

  const cancelEditRequestKey = () => {
    setEditingRequestKeyId(null)
    setRequestKeyDraft('')
  }

  const saveEditedRequestKey = async (item: RequestApiKeyItem) => {
    const apiKey = requestKeyDraft.trim()
    if (!apiKey) return toast.error('请输入新的请求 Key')
    if (apiKey.length < 8) return toast.error('请求 Key 至少需要 8 个字符')
    if (apiKey === item.apiKey) {
      cancelEditRequestKey()
      return
    }
    setProcessingKeyId(item.id)
    try {
      const response = await updateRequestApiKey(item.id, { apiKey })
      setKeysAndResetDrafts(response)
      toast.success('请求 Key 已保存，旧 Key 立即失效')
    } catch (error) {
      toast.error(`保存请求 Key 失败: ${extractErrorMessage(error)}`)
    } finally {
      setProcessingKeyId(null)
    }
  }

  const removeRequestKey = async (item: RequestApiKeyItem) => {
    if (requestKeys.length <= 1) return toast.error('至少需要保留一个请求 Key')
    const confirmed = await confirmDialog({
      title: '删除请求 Key',
      message: `确认删除 ${item.maskedApiKey}？删除后，使用该 Key 的客户端会立即认证失败。`,
      confirmText: '删除',
      tone: 'danger',
    })
    if (!confirmed) return
    setProcessingKeyId(item.id)
    try {
      const response = await deleteRequestApiKey(item.id)
      setKeysAndResetDrafts(response)
      toast.success('请求 Key 已删除')
    } catch (error) {
      toast.error(`删除请求 Key 失败: ${extractErrorMessage(error)}`)
    } finally {
      setProcessingKeyId(null)
    }
  }

  const toggleRequestKeyVisible = (id: string) => {
    setVisibleRequestKeyIds((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })
  }

  return (
    <ConfigGroup
      icon={<KeyRound className="h-4 w-4" />}
      title="接入与登录 Key"
      description="请求 Key 可配置多个，供客户端调用模型接口；登录 Key 仍只有一个，用于进入管理后台。"
    >
      <div className="settings-subsection md:col-span-2">
        <div className="settings-subsection-header">
          <div className="min-w-0">
            <div className="flex flex-wrap items-center gap-2">
              <h3 className="text-sm font-semibold">请求调用 Key</h3>
              <span className="config-badge">客户端调用</span>
              <span className="config-badge is-success">{requestKeys.length} 个可用</span>
            </div>
            <p className="mt-1 text-xs leading-5 text-base-content/60">
              给客户端调用模型接口时使用。可以按客户端分配不同 Key，新增、编辑、删除后立即生效。
            </p>
          </div>
          <Button type="button" color="primary" size="sm" className="shrink-0" disabled={loading || creating} onClick={generateRequestKey}>
            {creating ? <Loading size="xs" /> : <Wand2 className="h-4 w-4" />}
            随机生成并新增
          </Button>
        </div>

        <div className="settings-inline-form">
          <Input
            bordered
            size="sm"
            className="w-full min-w-0 font-mono text-xs"
            value={manualRequestApiKey}
            placeholder="手动输入要新增的请求 Key"
            disabled={loading || creating}
            onChange={(event) => setManualRequestApiKey(event.target.value)}
          />
          <Button type="button" variant="outline" size="sm" className="shrink-0" disabled={loading || creating} onClick={() => setManualRequestApiKey(generateLocalRequestApiKey())}>
            <Wand2 className="h-4 w-4" />
            随机生成
          </Button>
          <Button type="button" size="sm" className="shrink-0" disabled={loading || creating || !manualRequestApiKey.trim()} onClick={addManualRequestKey}>
            <Plus className="h-4 w-4" />
            新增 Key
          </Button>
        </div>

        <div className="settings-list">
          {loading && <div className="settings-list-row text-sm text-base-content/60">加载中...</div>}
          {!loading && requestKeys.length === 0 && <ErrorState title="未配置请求 Key" message="请先生成或手动新增一个请求 Key。" />}
          {!loading && requestKeys.map((item) => {
            const visible = visibleRequestKeyIds.has(item.id)
            const busy = processingKeyId === item.id
            const editing = editingRequestKeyId === item.id
            return (
              <div key={item.id} className="settings-list-row">
                <div className="mb-2 flex flex-col gap-2 md:flex-row md:items-center md:justify-between">
                  <div className="flex min-w-0 flex-wrap items-center gap-2">
                    <span className="text-sm font-semibold">请求 Key</span>
                    {item.primary && <span className="config-badge is-primary">主 Key</span>}
                    <span className="font-mono text-[0.68rem] text-base-content/50">{item.id.slice(0, 12)}</span>
                  </div>
                  <div className="flex flex-wrap gap-2">
                    <Button type="button" size="xs" disabled={busy || editing} onClick={() => toggleRequestKeyVisible(item.id)} title={visible ? '隐藏请求 Key' : '显示完整请求 Key'}>
                      {visible ? <EyeOff className="h-3.5 w-3.5" /> : <Eye className="h-3.5 w-3.5" />}
                      {visible ? '隐藏' : '显示'}
                    </Button>
                    <Button type="button" size="xs" disabled={busy || editing} onClick={() => copy('请求 Key', item.apiKey)}>
                      <Copy className="h-3.5 w-3.5" />
                      复制
                    </Button>
                    {!editing && (
                      <Button type="button" color="ghost" size="xs" disabled={busy || Boolean(editingRequestKeyId)} onClick={() => startEditRequestKey(item)}>
                        <Edit3 className="h-3.5 w-3.5" />
                        编辑
                      </Button>
                    )}
                    <Button type="button" color="error" variant="outline" size="xs" disabled={busy || editing || requestKeys.length <= 1} onClick={() => removeRequestKey(item)}>
                      <Trash2 className="h-3.5 w-3.5" />
                      删除
                    </Button>
                  </div>
                </div>
                <div className="space-y-2">
                  <Input
                    bordered
                    readOnly={!editing}
                    size="sm"
                    aria-label="请求调用 Key"
                    className="w-full min-w-0 font-mono text-xs"
                    value={editing ? requestKeyDraft : visible ? item.apiKey : item.maskedApiKey}
                    disabled={busy}
                    onChange={(event) => setRequestKeyDraft(event.target.value)}
                  />
                  {editing && (
                    <div className="flex flex-wrap justify-end gap-2">
                      <Button type="button" variant="outline" size="sm" disabled={busy} onClick={() => setRequestKeyDraft(generateLocalRequestApiKey())}>
                        <Wand2 className="h-4 w-4" />
                        随机生成
                      </Button>
                      <Button type="button" color="primary" size="sm" disabled={busy || !requestKeyDraft.trim()} onClick={() => saveEditedRequestKey(item)}>
                        {busy ? <Loading size="xs" /> : <Save className="h-4 w-4" />}
                        保存
                      </Button>
                      <Button type="button" variant="outline" size="sm" disabled={busy} onClick={cancelEditRequestKey}>
                        <X className="h-4 w-4" />
                        取消
                      </Button>
                    </div>
                  )}
                </div>
              </div>
            )
          })}
        </div>
      </div>

      <div className="settings-subsection md:col-span-2">
        <div className="settings-subsection-header">
          <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <h3 className="text-sm font-semibold">后台登录 Key</h3>
            <span className="config-badge">管理后台</span>
            <span className="config-badge is-info">登录密码</span>
          </div>
          <div className="mt-1 text-xs leading-5 text-base-content/60">
            这是登录页输入的密码，也用于管理后台的后续操作。修改成功后，当前浏览器会自动切换到新 Key。
          </div>
          </div>
        </div>

        <div className="flex flex-col gap-2 sm:flex-row">
          <Input
            bordered
            readOnly
            size="sm"
            aria-label="当前后台登录 Key"
            className="w-full min-w-0 flex-1 font-mono text-xs"
            value={loading ? '加载中...' : adminApiKeyValue || '未配置'}
          />
          <div className="flex gap-2 sm:shrink-0">
            <Button
              type="button"
              size="sm"
              className="flex-1 sm:flex-none"
              onClick={() => setShowAdminKey((value) => !value)}
              title={showAdminKey ? '隐藏登录 Key' : '显示完整登录 Key'}
            >
              {showAdminKey ? <EyeOff className="h-4 w-4" /> : <Eye className="h-4 w-4" />}
              {showAdminKey ? '隐藏' : '显示'}
            </Button>
            <Button type="button" size="sm" className="flex-1 sm:flex-none" onClick={() => copy('登录 Key', keys?.adminApiKey)}>
              <Copy className="h-4 w-4" />
              复制登录 Key
            </Button>
          </div>
        </div>

        <div className="settings-subsection mt-4">
          <div className="mb-2">
            <div className="text-sm font-semibold">修改登录 Key</div>
            <div className="mt-1 text-xs leading-4 text-base-content/60">
              保存后旧登录 Key 立即失效；当前页面会自动写入新 Key，不需要重新登录。
            </div>
          </div>
          <div className="flex flex-col gap-2 sm:flex-row">
            <Input
              bordered
              type="password"
              size="sm"
              className="w-full min-w-0 flex-1"
              value={nextAdminApiKey}
              placeholder="输入新的登录 Key（至少 8 个字符）"
              disabled={saving}
              onChange={(event) => setNextAdminApiKey(event.target.value)}
            />
            <Button type="button" color="primary" size="sm" className="shrink-0" disabled={saving || !nextAdminApiKey.trim()} onClick={saveAdminApiKey}>
              {saving ? '保存中...' : '保存登录 Key'}
            </Button>
          </div>
        </div>
      </div>
    </ConfigGroup>
  )
}

function ModeSelect({ value, disabled, onChange }: { value: ReportedUsageFieldMode; disabled?: boolean; onChange: (value: ReportedUsageFieldMode) => void }) {
  return (
    <Select bordered size="sm" className="w-full" value={value} disabled={disabled} onChange={(event) => onChange(event.target.value as ReportedUsageFieldMode)}>
      <Select.Option value="raw">显示原始值</Select.Option>
      <Select.Option value="preserve">保留当前计算结果</Select.Option>
      <Select.Option value="sample-max">按上限自动调整</Select.Option>
      <Select.Option value="sample-target">按目标自动调整</Select.Option>
    </Select>
  )
}

function PolicyNumberInput({
  title,
  description,
  value,
  min,
  step,
  suffix,
  disabled,
  onChange,
}: {
  title: string
  description: string
  value: number
  min?: number
  step?: number
  suffix: string
  disabled?: boolean
  onChange: (value: number) => void
}) {
  return (
    <FieldLabel title={title} description={description}>
      <Join className="w-full">
        <Input
          bordered
          size="sm"
          className="join-item w-full"
          type="number"
          value={value}
          min={min}
          step={step}
          inputMode={step ? 'decimal' : 'numeric'}
          disabled={disabled}
          onChange={(event) => onChange(numberValue(event.target.value, min ?? 0))}
        />
        <span className="join-item unit-addon min-w-16">{suffix}</span>
      </Join>
    </FieldLabel>
  )
}

function ReportedUsageFieldEditor({
  title,
  description,
  value,
  allowMoveDelta,
  disabled,
  onChange,
}: {
  title: string
  description: string
  value: ReportedUsageFieldPolicy
  allowMoveDelta?: boolean
  disabled?: boolean
  onChange: (value: ReportedUsageFieldPolicy) => void
}) {
  return (
    <div className="settings-policy-field">
      <div className="mb-2">
        <div className="text-sm font-semibold">{title}</div>
        <div className="mt-0.5 text-xs leading-4 text-base-content/60">{description}</div>
      </div>
      <div className="space-y-2.5">
        <ModeSelect value={value.mode} disabled={disabled} onChange={(mode) => onChange({ ...value, mode })} />
        <div className="settings-note">{reportedUsageModeDescription(value.mode)}</div>
        {fieldNeedsMax(value) && (
          <PolicyNumberInput
            title="展示上限"
            description="展示值不会超过这个上限，并会在范围内自然浮动。"
            value={value.maxTokens}
            min={0}
            suffix="Token"
            disabled={disabled}
            onChange={(maxTokens) => onChange({ ...value, maxTokens })}
          />
        )}
        {fieldNeedsTarget(value) && (
          <div className="grid gap-3 md:grid-cols-2">
            <PolicyNumberInput
              title="目标值"
              description="展示值会尽量围绕这个目标自然浮动。"
              value={value.targetTokens}
              min={0}
              suffix="Token"
              disabled={disabled}
              onChange={(targetTokens) => onChange({ ...value, targetTokens })}
            />
            <PolicyNumberInput
              title="常规最大倍率"
              description="控制展示值的常规浮动上限。"
              value={value.normalMaxMultiplier}
              min={1}
              step={0.1}
              suffix="倍"
              disabled={disabled}
              onChange={(normalMaxMultiplier) => onChange({ ...value, normalMaxMultiplier })}
            />
          </div>
        )}
        {allowMoveDelta && (
          <ToggleField
            title="差值计入缓存读取"
            description="开启后，输入展示值减少的部分会转到缓存读取展示里。"
            checked={value.moveDeltaToCacheRead}
            disabled={disabled || value.mode === 'preserve' || value.mode === 'raw'}
            onChange={(moveDeltaToCacheRead) => onChange({ ...value, moveDeltaToCacheRead })}
          />
        )}
      </div>
    </div>
  )
}

function ReportedUsagePathEditor({
  title,
  description,
  value,
  onDelete,
  onChange,
}: {
  title: string
  description: string
  value: ReportedUsagePathPolicy
  onDelete?: () => void
  onChange: (value: ReportedUsagePathPolicy) => void
}) {
  return (
    <div className="reported-usage-path">
      <div className="mb-3 flex flex-col gap-2 sm:flex-row sm:items-start sm:justify-between">
        <div className="min-w-0">
          <h4 className="text-sm font-semibold">{title}</h4>
          <p className="mt-0.5 text-xs leading-4 text-base-content/60">{description}</p>
        </div>
        <div className="flex shrink-0 items-center justify-between gap-2 sm:justify-end">
          {onDelete && (
            <Button type="button" color="error" variant="outline" size="xs" onClick={onDelete} title="删除这条入口规则">
              <Trash2 className="h-3.5 w-3.5" />
              删除规则
            </Button>
          )}
          <Toggle color="primary" size="sm" className="shrink-0" checked={value.enabled} onChange={(event) => onChange({ ...value, enabled: event.target.checked })} />
        </div>
      </div>
      {!value.enabled && (
        <Alert status="warning" className="mb-3 py-2 text-xs leading-5">
          当前入口会尽量使用原始用量显示。重新开启后才会使用下面的展示规则。
        </Alert>
      )}
      {value.enabled && (
        <>
          <div className="grid gap-3 xl:grid-cols-2">
            <ReportedUsageFieldEditor
              title="输入用量展示"
              description="控制输入用量如何展示。可以保留原始值，也可以按缓存展示规则调整。"
              value={value.input}
              allowMoveDelta
              onChange={(input) => onChange({ ...value, input })}
            />
            <ReportedUsageFieldEditor
              title="输出用量展示"
              description="控制输出用量如何展示。通常建议保持原始值。"
              value={value.output}
              onChange={(output) => onChange({ ...value, output })}
            />
            <ReportedUsageFieldEditor
              title="缓存读取展示"
              description="控制缓存读取用量如何展示。"
              value={value.cacheRead}
              onChange={(cacheRead) => onChange({ ...value, cacheRead })}
            />
            <ReportedUsageFieldEditor
              title="缓存写入展示"
              description="控制缓存写入用量如何展示。"
              value={value.cacheCreation}
              onChange={(cacheCreation) => onChange({ ...value, cacheCreation })}
            />
          </div>
          <div className="mt-3 grid gap-3 xl:grid-cols-3">
            <PolicyNumberInput
              title="读取缓存最终上限"
              description="缓存读取展示值最多显示到多少。填 0 表示不限制。"
              value={value.finalCacheReadMaxTokens ?? 700000}
              min={0}
              suffix="Token"
              onChange={(finalCacheReadMaxTokens) =>
                onChange({ ...value, finalCacheReadMaxTokens })
              }
            />
            <PolicyNumberInput
              title="最终上限扣减下限"
              description="达到上限时，至少从上限扣掉多少，让数值不要总是贴边。"
              value={value.finalCacheReadJitterMinTokens ?? 0}
              min={0}
              suffix="Token"
              onChange={(finalCacheReadJitterMinTokens) =>
                onChange({ ...value, finalCacheReadJitterMinTokens })
              }
            />
            <PolicyNumberInput
              title="最终上限扣减上限"
              description="达到上限时，最多从上限扣掉多少。"
              value={value.finalCacheReadJitterMaxTokens ?? 0}
              min={0}
              suffix="Token"
              onChange={(finalCacheReadJitterMaxTokens) =>
                onChange({ ...value, finalCacheReadJitterMaxTokens })
              }
            />
          </div>
        </>
      )}
    </div>
  )
}

export function ConfigPanel() {
  const config = useRuntimeConfig()
  const updateConfig = useUpdateRuntimeConfig()
  const modelCapabilities = useModelCapabilities()
  const [draft, setDraft] = useState<RuntimeConfig>(emptyRuntimeConfig)
  const [activeTab, setActiveTab] = useState<ConfigTab>('access')

  useEffect(() => {
    if (config.data) {
      setDraft({
        ...emptyRuntimeConfig,
        ...config.data,
        payloadShaping: {
          ...defaultPayloadShaping(),
          ...config.data.payloadShaping,
        },
        externalPools: {
          ...defaultExternalPoolsConfig(),
          ...config.data.externalPools,
        },
        promptCacheCreationControl: {
          ...defaultPromptCacheCreationControl(),
          ...config.data.promptCacheCreationControl,
        },
        cachePolicy: normalizeCachePolicy(config.data.cachePolicy),
        definedCacheRoutes: normalizeDefinedCacheRoutes(config.data.definedCacheRoutes || []),
        modelMapping: normalizeModelMapping(config.data.modelMapping),
      })
    }
  }, [config.data])

  const save = () => {
    const invalidDefinedCacheRoute = (draft.definedCacheRoutes || []).find((route) => route.trim() && !normalizeDefinedCacheRoute(route))
    if (invalidDefinedCacheRoute) {
      toast.error('缓存策略里的 /dfcache 路径必须是 /dfcache/{name}，name 只能包含字母、数字、点、下划线或短横线')
      return
    }
    const definedCacheRoutes = normalizeDefinedCacheRoutes(draft.definedCacheRoutes || [])
    const next: RuntimeConfig = {
      ...draft,
      credentialRpm: toWhole(draft.credentialRpm),
      credentialMaxConcurrentRequests: toWhole(draft.credentialMaxConcurrentRequests),
      credentialTransientCooldownSecs: toWhole(draft.credentialTransientCooldownSecs, 1),
      credentialRateLimitCooldownSecs: toWhole(draft.credentialRateLimitCooldownSecs, 1),
      credentialServerErrorCooldownSecs: toWhole(draft.credentialServerErrorCooldownSecs, 1),
      credentialNetworkErrorCooldownSecs: toWhole(draft.credentialNetworkErrorCooldownSecs, 1),
      credentialStreamErrorCooldownSecs: toWhole(draft.credentialStreamErrorCooldownSecs, 1),
      credentialProtocolErrorCooldownSecs: toWhole(draft.credentialProtocolErrorCooldownSecs, 1),
      credentialAuthErrorCooldownSecs: toWhole(draft.credentialAuthErrorCooldownSecs, 1),
      credentialCooldownBackoffMultiplier: Math.max(1, Number(draft.credentialCooldownBackoffMultiplier.toFixed(2))),
      credentialCooldownJitterPercent: toWhole(draft.credentialCooldownJitterPercent, 0, 100),
      credentialProbationSecs: toWhole(draft.credentialProbationSecs),
      credentialMaxCooldownSecs: toWhole(draft.credentialMaxCooldownSecs, 1),
      credentialDispatchMaxWaitSecs: toWhole(draft.credentialDispatchMaxWaitSecs),
      kiroUpstreamResponseTimeoutSecs: toWhole(draft.kiroUpstreamResponseTimeoutSecs),
      kiroUpstreamStreamIdleTimeoutSecs: toWhole(draft.kiroUpstreamStreamIdleTimeoutSecs),
      credentialRetryMaxAttempts: toWhole(draft.credentialRetryMaxAttempts),
      credentialInFlightLeaseMaxSecs: toWhole(draft.credentialInFlightLeaseMaxSecs),
      dispatchGlobalMaxConcurrentRequests: toWhole(draft.dispatchGlobalMaxConcurrentRequests),
      dispatchMaxQueuedRequests: toWhole(draft.dispatchMaxQueuedRequests),
      credentialWarmupRequests: toWhole(draft.credentialWarmupRequests),
      credentialWarmupSelectionPercent: toWhole(draft.credentialWarmupSelectionPercent, 0, 100),
      credentialWarmupMaxSelectionPercent: toWhole(draft.credentialWarmupMaxSelectionPercent, 0, 100),
      schedulerErrorEwmaAlpha: Math.min(1, Math.max(0.01, Number(draft.schedulerErrorEwmaAlpha.toFixed(2)))),
      schedulerPriorityWeight: Math.max(0, Number(draft.schedulerPriorityWeight.toFixed(2))),
      schedulerLoadWeight: Math.max(0, Number(draft.schedulerLoadWeight.toFixed(2))),
      schedulerErrorWeight: Math.max(0, Number(draft.schedulerErrorWeight.toFixed(2))),
      schedulerLatencyWeight: Math.max(0, Number(draft.schedulerLatencyWeight.toFixed(4))),
      schedulerProbationWeight: Math.max(0, Number(draft.schedulerProbationWeight.toFixed(2))),
      schedulerSelectionPressureWeight: Math.max(0, Number(draft.schedulerSelectionPressureWeight.toFixed(2))),
      schedulerTotalSelectionWeight: Math.max(0, Number(draft.schedulerTotalSelectionWeight.toFixed(4))),
      schedulerTopK: toWhole(draft.schedulerTopK, 1, 100),
      selectionFailureSampleLimit: toWhole(draft.selectionFailureSampleLimit, 0, 1000),
      payloadGuardMaxBytes: toWhole(draft.payloadGuardMaxBytes),
      payloadGuardSafetyMarginBytes: toWhole(draft.payloadGuardSafetyMarginBytes),
      payloadShaping: normalizePayloadShaping(draft.payloadShaping),
      promptCacheTargetReadRatio: toRatio(draft.promptCacheTargetReadRatio),
      promptCacheTokenScale: toScale(draft.promptCacheTokenScale),
      promptCacheMaxSimulatedInputTokens: toWhole(draft.promptCacheMaxSimulatedInputTokens),
      promptCacheCapJitterMinTokens: toWhole(draft.promptCacheCapJitterMinTokens),
      promptCacheCapJitterMaxTokens: toWhole(draft.promptCacheCapJitterMaxTokens),
      promptCacheScaleMinInputTokens: toWhole(draft.promptCacheScaleMinInputTokens),
      promptCacheCreationControl: normalizePromptCacheCreationControl(draft.promptCacheCreationControl),
      promptCacheMaxEntriesPerAccount: toWhole(draft.promptCacheMaxEntriesPerAccount),
      promptCacheMaxEntriesGlobal: toWhole(draft.promptCacheMaxEntriesGlobal),
      promptCacheEntryTtlSecs: toWhole(draft.promptCacheEntryTtlSecs, 1),
      promptCacheEstimatedBytesLimit: toWhole(draft.promptCacheEstimatedBytesLimit),
      reportedUsage: normalizeReportedUsage(draft.reportedUsage),
      cachePolicy: normalizeCachePolicy(draft.cachePolicy),
      definedCacheRoutes,
      modelMapping: normalizeModelMapping(draft.modelMapping),
      externalPools: {
        ...defaultExternalPoolsConfig(),
        ...draft.externalPools,
        externalPoolGlobalMaxConcurrentRequests: toWhole(draft.externalPools.externalPoolGlobalMaxConcurrentRequests),
        externalPoolMaxQueuedRequests: toWhole(draft.externalPools.externalPoolMaxQueuedRequests),
        externalPoolDispatchMaxWaitSecs: toWhole(draft.externalPools.externalPoolDispatchMaxWaitSecs),
        externalPoolRetryMaxAttempts: toWhole(draft.externalPools.externalPoolRetryMaxAttempts),
        externalPoolLocalRescueMaxWaitSecs: toWhole(draft.externalPools.externalPoolLocalRescueMaxWaitSecs),
        localPoolCircuitWindowSecs: toWhole(draft.externalPools.localPoolCircuitWindowSecs, 1),
        localPoolCircuitOpenAfterFailures: toWhole(draft.externalPools.localPoolCircuitOpenAfterFailures, 1),
        localPoolCircuitRequireDistinctCredentials: toWhole(draft.externalPools.localPoolCircuitRequireDistinctCredentials),
        localPoolCircuitOpenSecs: toWhole(draft.externalPools.localPoolCircuitOpenSecs, 1),
        externalPoolAutoDisableFailureThreshold: toWhole(draft.externalPools.externalPoolAutoDisableFailureThreshold, 1),
        externalPoolAutoDisableWindowSecs: toWhole(draft.externalPools.externalPoolAutoDisableWindowSecs, 1),
        externalPoolAutoDisableDurationSecs: toWhole(draft.externalPools.externalPoolAutoDisableDurationSecs),
        externalPoolRateLimitCooldownSecs: toWhole(draft.externalPools.externalPoolRateLimitCooldownSecs, 1),
        externalPoolServerErrorCooldownSecs: toWhole(draft.externalPools.externalPoolServerErrorCooldownSecs, 1),
        externalPoolNetworkErrorCooldownSecs: toWhole(draft.externalPools.externalPoolNetworkErrorCooldownSecs, 1),
        externalPoolProtocolErrorCooldownSecs: toWhole(draft.externalPools.externalPoolProtocolErrorCooldownSecs, 1),
        externalPoolRequestTimeoutSecs: toWhole(draft.externalPools.externalPoolRequestTimeoutSecs),
        externalPoolStreamRequestTimeoutSecs: toWhole(draft.externalPools.externalPoolStreamRequestTimeoutSecs),
        externalPoolStreamIdleTimeoutSecs: toWhole(draft.externalPools.externalPoolStreamIdleTimeoutSecs),
        externalPoolUsageProjectionUpliftPercent: toWhole(draft.externalPools.externalPoolUsageProjectionUpliftPercent),
        externalPoolUsageProjectionOutputUpliftMinTokens: toWhole(draft.externalPools.externalPoolUsageProjectionOutputUpliftMinTokens),
        externalPoolUsageProjectionOutputUpliftPercent: toWhole(draft.externalPools.externalPoolUsageProjectionOutputUpliftPercent),
      },
      highCacheThreshold: toWhole(draft.highCacheThreshold),
    }
    if (next.credentialTransientCooldownSecs > next.credentialMaxCooldownSecs) return toast.error('临时冷却秒数不能大于最大冷却秒数')
    if ([next.credentialRateLimitCooldownSecs, next.credentialServerErrorCooldownSecs, next.credentialNetworkErrorCooldownSecs, next.credentialStreamErrorCooldownSecs, next.credentialProtocolErrorCooldownSecs, next.credentialAuthErrorCooldownSecs].some((value) => value > next.credentialMaxCooldownSecs)) return toast.error('错误类型基础冷却秒数不能大于最大冷却秒数')
    if (next.promptCacheCapJitterMinTokens > next.promptCacheCapJitterMaxTokens) return toast.error('触顶扣减下限不能大于上限')
    if (next.payloadGuardEnabled && next.payloadGuardMaxBytes > 0 && next.payloadGuardMaxBytes < 65536) return toast.error('请求大小处理阈值必须为 0 或不小于 65536 字节')
    if (next.payloadGuardEnabled && next.payloadGuardMaxBytes > 0 && next.payloadGuardMaxBytes - next.payloadGuardSafetyMarginBytes < 65536) return toast.error('安全余量不能过大，实际处理目标不能小于 65536 字节')
    const editableConfig = { ...next }
    delete editableConfig.proxyUrl
    delete editableConfig.proxyUsername
    delete editableConfig.proxyPassword
    updateConfig.mutate(editableConfig, {
      onSuccess: () => toast.success('配置已更新'),
      onError: (error) => toast.error(`更新失败: ${extractErrorMessage(error)}`),
    })
  }

  const payloadSizeLimitEnabled = draft.payloadGuardEnabled && draft.payloadGuardMaxBytes > 0
  const payloadShapingBranchEnabled = payloadSizeLimitEnabled && draft.payloadShaping.enabled
  const payloadGuardMode = draft.payloadGuardMode ?? 'preemptive'
  const payloadGuardRetryMode = payloadGuardMode === 'on_too_long'
  const defaultModelMappingRules = generateDefaultModelMappingRules(modelCapabilities.data)
  const payloadConditionTitle = payloadGuardRetryMode
    ? '仅在内容过长并重试时执行'
    : '仅当发送前内容超过上方阈值时执行'
  const payloadConditionDescription = payloadSizeLimitEnabled
    ? payloadGuardRetryMode
      ? '第一次会先正常发送；如果返回内容过长错误，再按设置裁剪并重试一次。'
      : '发送前会检查请求大小；只有超过阈值时才会处理内容。'
    : '当前未启用大小阈值，因此下面这些按大小触发的处理不会运行。'
  const activeTabMeta = configTabs.find((tab) => tab.key === activeTab) ?? configTabs[0]
  const showRuntimeSave = activeTab !== 'access'

  if (config.isLoading) return <div className="py-10 text-center text-base-content/60">加载中...</div>
  if (config.error) return <ErrorState text={extractErrorMessage(config.error)} />

  return (
    <div className="config-page">
      <div className="config-shell">
        <aside className="config-side-nav" aria-label="配置分类">
          <div className="config-side-nav-list" role="tablist" aria-label="配置分类">
          {configTabs.map((tab) => (
            <button
              key={tab.key}
              type="button"
              role="tab"
              aria-selected={activeTab === tab.key}
              className={`config-side-nav-item ${activeTab === tab.key ? 'is-active' : ''}`}
              onClick={() => setActiveTab(tab.key)}
            >
              <span className="config-side-nav-icon">{tab.icon}</span>
              <span className="min-w-0">
                <span className="block truncate text-sm font-semibold">{tab.label}</span>
                <span className="mt-0.5 block truncate text-[0.68rem] text-base-content/55">{tab.description}</span>
              </span>
            </button>
          ))}
          </div>
        </aside>

        <section className="config-content">
          <div className="config-content-head">
            <div className="config-content-title">
              <span className="config-content-icon">{activeTabMeta.icon}</span>
              <div className="min-w-0">
                <h3 className="text-base font-semibold">{activeTabMeta.label}</h3>
                <p className="mt-1 text-xs leading-5 text-base-content/60">{activeTabMeta.description}</p>
              </div>
            </div>
            {showRuntimeSave && (
              <div className="config-save-bar">
                <span className="min-w-0 text-xs leading-5 text-base-content/60">保存后，新请求会使用这里的设置。</span>
                <Button type="button" color="primary" size="sm" className="shrink-0" onClick={save} disabled={updateConfig.isPending}>
                  {updateConfig.isPending ? <Loading size="xs" /> : <Save className="h-4 w-4" />}
                  保存
                </Button>
              </div>
            )}
          </div>

          <div className="config-section-stack">
        {activeTab === 'access' && <AccessKeysPanel />}
        {activeTab === 'limits' && (
          <ConfigGroup icon={<Gauge className="h-4 w-4" />} title="请求容量" description="控制每个账号和全局的承载量，以及请求等待、重试和超时。">
            <NumberField title="单账号每分钟请求上限" description="控制每个账号每分钟最多承接多少请求。填 0 表示关闭本地限速。" value={draft.credentialRpm} min={0} suffix="次/分钟" onChange={(credentialRpm) => setDraft((prev) => ({ ...prev, credentialRpm }))} />
            <NumberField title="单账号最大并发请求数" description="控制同一个账号同时处理多少个请求。填 0 表示不限制。" value={draft.credentialMaxConcurrentRequests} min={0} suffix="并发" onChange={(credentialMaxConcurrentRequests) => setDraft((prev) => ({ ...prev, credentialMaxConcurrentRequests }))} />
            <NumberField title="全局最大并发请求数" description="控制所有账号合计可同时处理的请求数。填 0 表示不限制。" value={draft.dispatchGlobalMaxConcurrentRequests} min={0} suffix="并发" onChange={(dispatchGlobalMaxConcurrentRequests) => setDraft((prev) => ({ ...prev, dispatchGlobalMaxConcurrentRequests }))} />
            <NumberField title="最大等待队列请求数" description="调度容量已满时允许排队等待的请求数量。填 0 表示不限制。" value={draft.dispatchMaxQueuedRequests} min={0} suffix="请求" onChange={(dispatchMaxQueuedRequests) => setDraft((prev) => ({ ...prev, dispatchMaxQueuedRequests }))} />
            <NumberField title="单请求最长排队等待" description="所有可用账号都处于冷却、限速或并发占满时最多等待多久。填 0 表示不限制。" value={draft.credentialDispatchMaxWaitSecs} min={0} suffix="秒" onChange={(credentialDispatchMaxWaitSecs) => setDraft((prev) => ({ ...prev, credentialDispatchMaxWaitSecs }))} />
            <NumberField title="开始响应等待时间" description="请求发出后最多等多久开始返回内容。填 0 表示使用默认超时。" value={draft.kiroUpstreamResponseTimeoutSecs} min={0} suffix="秒" onChange={(kiroUpstreamResponseTimeoutSecs) => setDraft((prev) => ({ ...prev, kiroUpstreamResponseTimeoutSecs }))} />
            <NumberField title="流式静默超时" description="流式响应长时间没有新内容时结束本次请求。填 0 表示不按流式空闲时间主动结束。" value={draft.kiroUpstreamStreamIdleTimeoutSecs} min={0} suffix="秒" onChange={(kiroUpstreamStreamIdleTimeoutSecs) => setDraft((prev) => ({ ...prev, kiroUpstreamStreamIdleTimeoutSecs }))} />
            <NumberField title="单请求最大重试次数" description="一次请求失败后最多换几个账号再试。填 0 表示由系统自动决定。" value={draft.credentialRetryMaxAttempts} min={0} suffix="次" onChange={(credentialRetryMaxAttempts) => setDraft((prev) => ({ ...prev, credentialRetryMaxAttempts }))} />
            <NumberField title="异常并发自动回收" description="单个并发占用超过多久未活跃时自动释放。填 0 表示关闭。" value={draft.credentialInFlightLeaseMaxSecs} min={0} suffix="秒" onChange={(credentialInFlightLeaseMaxSecs) => setDraft((prev) => ({ ...prev, credentialInFlightLeaseMaxSecs }))} />
          </ConfigGroup>
        )}

        {activeTab === 'cooldown' && (
          <ConfigGroup icon={<Shield className="h-4 w-4" />} title="错误恢复" description="设置账号遇到不同错误后暂停多久，以及连续失败时如何延长暂停时间。">
            <NumberField title="默认暂停时间" description="遇到未细分的临时错误时，账号暂停使用多久。" value={draft.credentialTransientCooldownSecs} min={1} suffix="秒" onChange={(credentialTransientCooldownSecs) => setDraft((prev) => ({ ...prev, credentialTransientCooldownSecs }))} />
            <NumberField title="限流后暂停" description="遇到限流时，账号暂停使用多久。" value={draft.credentialRateLimitCooldownSecs} min={1} suffix="秒" onChange={(credentialRateLimitCooldownSecs) => setDraft((prev) => ({ ...prev, credentialRateLimitCooldownSecs }))} />
            <NumberField title="服务繁忙后暂停" description="遇到服务繁忙或超时时，账号暂停使用多久。" value={draft.credentialServerErrorCooldownSecs} min={1} suffix="秒" onChange={(credentialServerErrorCooldownSecs) => setDraft((prev) => ({ ...prev, credentialServerErrorCooldownSecs }))} />
            <NumberField title="网络错误基础冷却" description="发送失败、连接中断等网络错误首次触发的冷却时长。" value={draft.credentialNetworkErrorCooldownSecs} min={1} suffix="秒" onChange={(credentialNetworkErrorCooldownSecs) => setDraft((prev) => ({ ...prev, credentialNetworkErrorCooldownSecs }))} />
            <NumberField title="流式中断后暂停" description="流式响应中断或长时间没有内容时，账号暂停使用多久。" value={draft.credentialStreamErrorCooldownSecs} min={1} suffix="秒" onChange={(credentialStreamErrorSecs) => setDraft((prev) => ({ ...prev, credentialStreamErrorCooldownSecs: credentialStreamErrorSecs }))} />
            <NumberField title="格式异常后暂停" description="遇到可重试的请求格式异常时，账号暂停使用多久。" value={draft.credentialProtocolErrorCooldownSecs} min={1} suffix="秒" onChange={(credentialProtocolErrorCooldownSecs) => setDraft((prev) => ({ ...prev, credentialProtocolErrorCooldownSecs }))} />
            <NumberField title="授权异常后暂停" description="账号授权异常处理期间，暂停继续使用该账号。" value={draft.credentialAuthErrorCooldownSecs} min={1} suffix="秒" onChange={(credentialAuthErrorCooldownSecs) => setDraft((prev) => ({ ...prev, credentialAuthErrorCooldownSecs }))} />
            <NumberField title="连续失败延长倍率" description="同一账号连续出错时，用这个倍率逐步延长暂停时间。" value={draft.credentialCooldownBackoffMultiplier} min={1} max={10} step={0.1} suffix="倍" onChange={(credentialCooldownBackoffMultiplier) => setDraft((prev) => ({ ...prev, credentialCooldownBackoffMultiplier }))} />
            <NumberField title="恢复时间错开比例" description="给恢复时间加一点随机偏移，避免多个账号同时恢复造成波动。" value={draft.credentialCooldownJitterPercent} min={0} max={100} suffix="%" onChange={(credentialCooldownJitterPercent) => setDraft((prev) => ({ ...prev, credentialCooldownJitterPercent }))} />
            <NumberField title="恢复观察时间" description="账号恢复后先降低使用频率，稳定后再恢复正常。" value={draft.credentialProbationSecs} min={0} suffix="秒" onChange={(credentialProbationSecs) => setDraft((prev) => ({ ...prev, credentialProbationSecs }))} />
            <NumberField title="最大冷却秒数" description="控制单个账号最长冷却时间。" value={draft.credentialMaxCooldownSecs} min={1} suffix="秒" onChange={(credentialMaxCooldownSecs) => setDraft((prev) => ({ ...prev, credentialMaxCooldownSecs }))} />
          </ConfigGroup>
        )}

        {activeTab === 'scheduler' && (
          <ConfigGroup icon={<Gauge className="h-4 w-4" />} title="账号选择权重" description="决定系统优先使用哪些账号，让请求尽量分散在更稳定的账号上。">
              <NumberField title="近期错误敏感度" description="数值越高，最近出现的错误越快影响账号选择。" value={draft.schedulerErrorEwmaAlpha} min={0.01} max={1} step={0.01} suffix="系数" onChange={(schedulerErrorEwmaAlpha) => setDraft((prev) => ({ ...prev, schedulerErrorEwmaAlpha }))} />
              <NumberField title="优先级权重" description="账号优先级对选择结果的影响。" value={draft.schedulerPriorityWeight} min={0} step={0.1} suffix="权重" onChange={(schedulerPriorityWeight) => setDraft((prev) => ({ ...prev, schedulerPriorityWeight }))} />
              <NumberField title="当前负载权重" description="账号当前越忙，是否越少继续分配给它。" value={draft.schedulerLoadWeight} min={0} step={1} suffix="权重" onChange={(schedulerLoadWeight) => setDraft((prev) => ({ ...prev, schedulerLoadWeight }))} />
              <NumberField title="近期错误权重" description="近期错误越多，是否越少继续使用该账号。" value={draft.schedulerErrorWeight} min={0} step={1} suffix="权重" onChange={(schedulerErrorWeight) => setDraft((prev) => ({ ...prev, schedulerErrorWeight }))} />
              <NumberField title="响应耗时权重" description="响应越慢，是否越少继续使用该账号。" value={draft.schedulerLatencyWeight} min={0} step={0.001} suffix="权重" onChange={(schedulerLatencyWeight) => setDraft((prev) => ({ ...prev, schedulerLatencyWeight }))} />
              <NumberField title="恢复期降权" description="账号刚恢复时先少用一点，观察稳定后再恢复正常。" value={draft.schedulerProbationWeight} min={0} step={1} suffix="权重" onChange={(schedulerProbationWeight) => setDraft((prev) => ({ ...prev, schedulerProbationWeight }))} />
              <NumberField title="短时间集中使用降权" description="最近一分钟被选中过多时降低使用频率，避免请求集中到单个账号。" value={draft.schedulerSelectionPressureWeight} min={0} step={1} suffix="权重" onChange={(schedulerSelectionPressureWeight) => setDraft((prev) => ({ ...prev, schedulerSelectionPressureWeight }))} />
              <NumberField title="长期使用次数权重" description="根据历史使用次数做轻微均衡。通常保持 0 即可。" value={draft.schedulerTotalSelectionWeight} min={0} step={0.001} suffix="权重" onChange={(schedulerTotalSelectionWeight) => setDraft((prev) => ({ ...prev, schedulerTotalSelectionWeight }))} />
              <NumberField title="候选账号数量" description="每次从排名靠前的几个账号里选择，数值越大越分散。" value={draft.schedulerTopK} min={1} max={100} suffix="个" onChange={(schedulerTopK) => setDraft((prev) => ({ ...prev, schedulerTopK }))} />
              <NumberField title="失败诊断样本数" description="调度失败时最多记录多少个账号样本，用于后台排查；0 表示不记录样本。" value={draft.selectionFailureSampleLimit} min={0} max={1000} suffix="个" onChange={(selectionFailureSampleLimit) => setDraft((prev) => ({ ...prev, selectionFailureSampleLimit }))} />
              <ToggleField title="记录失败样本" description="关闭后只保留失败原因统计，不记录具体账号样本。" checked={draft.selectionFailureRecordEnabled} onChange={(selectionFailureRecordEnabled) => setDraft((prev) => ({ ...prev, selectionFailureRecordEnabled }))} />
            </ConfigGroup>
        )}

        {activeTab === 'warmup' && (
            <ConfigGroup icon={<Sparkles className="h-4 w-4" />} title="新账号预热" description="让新账号先少量参与服务，确认稳定后再逐步恢复正常使用。">
              <NumberField title="预热请求数" description="新账号先参与多少次请求后结束预热。填 0 表示不预热。" value={draft.credentialWarmupRequests} min={0} suffix="次" onChange={(credentialWarmupRequests) => setDraft((prev) => ({ ...prev, credentialWarmupRequests }))} />
              <NumberField title="单个预热账号参与比例" description="每个预热账号希望承接的请求比例。" value={draft.credentialWarmupSelectionPercent} min={0} max={100} suffix="%" onChange={(credentialWarmupSelectionPercent) => setDraft((prev) => ({ ...prev, credentialWarmupSelectionPercent }))} />
              <NumberField title="预热账号总占比上限" description="所有预热账号合计最多承接多少请求。" value={draft.credentialWarmupMaxSelectionPercent} min={0} max={100} suffix="%" onChange={(credentialWarmupMaxSelectionPercent) => setDraft((prev) => ({ ...prev, credentialWarmupMaxSelectionPercent }))} />
            </ConfigGroup>
        )}

        {activeTab === 'payload' && (
            <ConfigGroup
              icon={<Wand2 className="h-4 w-4" />}
              title="请求大小保护"
              description="设置请求过大时的处理方式、触发阈值和基础压缩开关。"
            >
              <ImpactGroupHeader
                label="全局影响"
                title="每次请求发送前都会检查"
                description="开启后，系统会在发送前做必要的压缩、格式修正和大小检查。"
              />
              <ToggleField title="启用请求压缩" description="开启后会尽量减少请求里的冗余内容；关闭时不改动请求内容。" checked={draft.compressionEnabled} onChange={(compressionEnabled) => setDraft((prev) => ({ ...prev, compressionEnabled }))} />
              <ToggleField title="仅压缩空白字符" description="只处理多余空白，风险较低，适合默认开启。" checked={draft.whitespaceCompression} disabled={!draft.compressionEnabled} onChange={(whitespaceCompression) => setDraft((prev) => ({ ...prev, whitespaceCompression }))} />
              <ToggleField title="启用大小保护" description="统计请求大小，并修正常见的格式问题，减少请求被拒绝的概率。" checked={draft.payloadGuardEnabled} onChange={(payloadGuardEnabled) => setDraft((prev) => ({ ...prev, payloadGuardEnabled }))} />
              <ToggleField title="外部账号也应用大小保护" description="开启后，外部账号请求也使用同一套大小保护设置。" checked={draft.payloadGuardExternalEnabled} disabled={!draft.payloadGuardEnabled} onChange={(payloadGuardExternalEnabled) => setDraft((prev) => ({ ...prev, payloadGuardExternalEnabled }))} />
              <FieldLabel title="过大请求处理方式" description="可选择发送前先裁剪，也可以在收到“内容过长”错误后再裁剪并重试。">
                <Select bordered size="sm" className="w-full" value={payloadGuardMode} disabled={!draft.payloadGuardEnabled} onChange={(event) => setDraft((prev) => ({ ...prev, payloadGuardMode: event.target.value as PayloadGuardMode }))}>
                  <Select.Option value="preemptive">发送前先处理</Select.Option>
                  <Select.Option value="on_too_long">失败后再处理并重试</Select.Option>
                </Select>
              </FieldLabel>
              <ImpactGroupHeader
                label="条件阈值"
                title="控制什么时候开始处理过大请求"
                description="这里设置的是本地处理阈值，不是模型上下文上限。填 0 表示关闭按大小触发的处理。"
              />
              <NumberField title="请求大小处理阈值" description="请求超过这个大小后才会触发裁剪或压缩。填 0 表示不按大小处理。" value={draft.payloadGuardMaxBytes} min={0} suffix="字节" onChange={(payloadGuardMaxBytes) => setDraft((prev) => ({ ...prev, payloadGuardMaxBytes }))} />
              <NumberField title="安全余量" description="实际处理目标会比上面的阈值更小一点，给系统追加内容留出空间。" value={draft.payloadGuardSafetyMarginBytes} min={0} suffix="字节" disabled={!payloadSizeLimitEnabled} onChange={(payloadGuardSafetyMarginBytes) => setDraft((prev) => ({ ...prev, payloadGuardSafetyMarginBytes }))} />
              <ImpactGroupHeader
                label="条件分支"
                title={payloadConditionTitle}
                description={payloadConditionDescription}
                muted={!payloadSizeLimitEnabled}
              />
            </ConfigGroup>
        )}

        {activeTab === 'payloadHistory' && (
            <ConfigGroup
              icon={<Wand2 className="h-4 w-4" />}
              title="旧内容清理"
              description="当请求超过阈值时，先处理旧对话、历史工具结果和网页抓取内容。"
            >
              <ImpactGroupHeader
                label="生效条件"
                title={payloadConditionTitle}
                description={payloadConditionDescription}
                muted={!payloadSizeLimitEnabled}
              />
              <ToggleField title="优先裁剪旧历史" description="内容太长时，优先缩短较早的对话历史，尽量保留当前请求。" checked={draft.payloadGuardTrimHistory} disabled={!payloadSizeLimitEnabled} onChange={(payloadGuardTrimHistory) => setDraft((prev) => ({ ...prev, payloadGuardTrimHistory }))} />
              <ToggleField title="启用内容清理" description="内容太长时，允许系统清理历史内容、工具说明和网页抓取结果。" checked={draft.payloadShaping.enabled} disabled={!payloadSizeLimitEnabled} onChange={(enabled) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, enabled } }))} />
              <ToggleField title="截短历史工具结果" description="历史工具结果很长时，只保留开头和结尾。" checked={draft.payloadShaping.truncateHistoricalToolResults} disabled={!payloadShapingBranchEnabled} onChange={(truncateHistoricalToolResults) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, truncateHistoricalToolResults } }))} />
              <NumberField title="历史工具结果保留字符" description="单条历史工具结果最多保留多少字符。" value={draft.payloadShaping.historicalToolResultMaxChars} disabled={!payloadShapingBranchEnabled} min={0} suffix="字符" onChange={(historicalToolResultMaxChars) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, historicalToolResultMaxChars } }))} />
              <ToggleField title="移除历史思考内容" description="只清理旧对话里的思考内容，不处理当前问题。" checked={draft.payloadShaping.discardHistoricalThinking} disabled={!payloadShapingBranchEnabled} onChange={(discardHistoricalThinking) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, discardHistoricalThinking } }))} />
              <ToggleField title="压缩工具说明" description="缩短工具描述文字，但保留工具结构和必要参数。" checked={draft.payloadShaping.compressToolDefinitions} disabled={!payloadShapingBranchEnabled} onChange={(compressToolDefinitions) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, compressToolDefinitions } }))} />
              <NumberField title="工具说明大小上限" description="工具说明超过这个大小后才会压缩。填 0 表示不按此项压缩。" value={draft.payloadShaping.toolDefinitionsBudgetBytes} disabled={!payloadShapingBranchEnabled} min={0} suffix="字节" onChange={(toolDefinitionsBudgetBytes) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, toolDefinitionsBudgetBytes } }))} />
              <ToggleField title="清理网页抓取历史" description="清理历史网页抓取结果里的图片、重复行和明显噪声。" checked={draft.payloadShaping.webFetchTrimEnabled} disabled={!payloadShapingBranchEnabled} onChange={(webFetchTrimEnabled) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, webFetchTrimEnabled } }))} />
              <NumberField title="网页抓取正文保留字符" description="网页抓取结果清理后最多保留多少字符。填 0 表示不裁剪正文。" value={draft.payloadShaping.webFetchBodyMaxChars} disabled={!payloadShapingBranchEnabled} min={0} suffix="字符" onChange={(webFetchBodyMaxChars) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, webFetchBodyMaxChars } }))} />
            </ConfigGroup>
        )}

        {activeTab === 'payloadFallback' && (
            <ConfigGroup
              icon={<Wand2 className="h-4 w-4" />}
              title="当前内容兜底"
              description="旧内容清理后仍然过大时，再处理当前消息、当前文档和图片。"
            >
              <ImpactGroupHeader
                label="兜底分支"
                title="历史内容处理后仍然太长时执行"
                description={
                  payloadShapingBranchEnabled
                    ? '这是最后一步保护：只有旧内容处理后仍然太长，才会处理当前消息、文档或图片。'
                    : '当前没有启用按大小处理，因此这些兜底配置不会运行。'
                }
                muted={!payloadShapingBranchEnabled}
              />
              <ToggleField title="自动压缩当前内容" description="旧内容处理后仍然太长时，允许继续缩短当前内容。" checked={draft.payloadShaping.fitCurrentPayloadToBudget} disabled={!payloadShapingBranchEnabled} onChange={(fitCurrentPayloadToBudget) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, fitCurrentPayloadToBudget } }))} />
              <ToggleField title="截短当前工具结果" description="当前工具结果很长时，只保留开头和结尾。" checked={draft.payloadShaping.truncateCurrentToolResults} disabled={!payloadShapingBranchEnabled} onChange={(truncateCurrentToolResults) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, truncateCurrentToolResults } }))} />
              <NumberField title="当前工具结果保留字符" description="单条当前工具结果最多保留多少字符。" value={draft.payloadShaping.currentToolResultMaxChars} disabled={!payloadShapingBranchEnabled} min={0} suffix="字符" onChange={(currentToolResultMaxChars) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, currentToolResultMaxChars } }))} />
              <ToggleField title="截短当前用户文本" description="当前用户文本很长时，只保留主要内容。" checked={draft.payloadShaping.truncateCurrentUserContent} disabled={!payloadShapingBranchEnabled} onChange={(truncateCurrentUserContent) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, truncateCurrentUserContent } }))} />
              <NumberField title="当前用户文本保留字符" description="当前用户文本最多保留多少字符。" value={draft.payloadShaping.currentUserContentMaxChars} disabled={!payloadShapingBranchEnabled} min={0} suffix="字符" onChange={(currentUserContentMaxChars) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, currentUserContentMaxChars } }))} />
              <ToggleField title="截短当前文档" description="当前文档太长时缩短正文，保留文档结构。" checked={draft.payloadShaping.truncateCurrentDocuments} disabled={!payloadShapingBranchEnabled} onChange={(truncateCurrentDocuments) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, truncateCurrentDocuments } }))} />
              <NumberField title="当前文档保留字符" description="单个当前文档最多保留多少字符。" value={draft.payloadShaping.currentDocumentMaxChars} disabled={!payloadShapingBranchEnabled} min={0} suffix="字符" onChange={(currentDocumentMaxChars) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, currentDocumentMaxChars } }))} />
              <ToggleField title="移除当前图片" description="图片太大且请求仍然超限时，可按体积优先移除大图。" checked={draft.payloadShaping.truncateCurrentImages} disabled={!payloadShapingBranchEnabled} onChange={(truncateCurrentImages) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, truncateCurrentImages } }))} />
              <NumberField title="当前图片保留大小" description="当前图片数据最多保留多少字节。" value={draft.payloadShaping.currentImagesMaxBytes} disabled={!payloadShapingBranchEnabled} min={0} suffix="字节" onChange={(currentImagesMaxBytes) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, currentImagesMaxBytes } }))} />
              <div className="rounded-box border border-base-300 bg-base-100 p-4">
                <FieldLabel title="单图超 5MB 处理" description="图片超过上游单图限制时，选择移除并给模型占位说明，或直接返回请求错误。">
                  <Select bordered size="sm" className="w-full" value={draft.payloadShaping.oversizedImageHandling ?? 'drop-with-placeholder'} disabled={!payloadShapingBranchEnabled} onChange={(event) => setDraft((prev) => ({ ...prev, payloadShaping: { ...prev.payloadShaping, oversizedImageHandling: event.target.value as PayloadShapingConfig['oversizedImageHandling'] } }))}>
                    <Select.Option value="drop-with-placeholder">移除图片并占位</Select.Option>
                    <Select.Option value="reject">直接报错</Select.Option>
                  </Select>
                </FieldLabel>
              </div>
            </ConfigGroup>
        )}

        {activeTab === 'cachePolicy' && (
          <ConfigGroup icon={<Zap className="h-4 w-4" />} title="缓存策略" description="默认缓存策略和路径覆盖在这里统一维护。">
            <CachePolicyEditor
              value={draft}
              onChange={setDraft}
            />
          </ConfigGroup>
        )}

        {activeTab === 'compat' && (
            <ConfigGroup icon={<Shield className="h-4 w-4" />} title="兼容与模型" description="选择接口兼容方式，并维护模型名称映射。">
              <FieldLabel title="兼容模式" description="日常使用建议保持兼容模式；严格模式会尽量减少代理侧处理；调试模式会输出更多排查信息。">
                <Select bordered size="sm" value={draft.compatProfile} onChange={(event) => setDraft((prev) => ({ ...prev, compatProfile: event.target.value as CompatProfile }))}>
                  <Select.Option value="claude-code">Claude Code 兼容</Select.Option>
                  <Select.Option value="anthropic-strict">Anthropic 严格模式</Select.Option>
                  <Select.Option value="debug">调试模式</Select.Option>
                </Select>
              </FieldLabel>
              <FieldLabel title="Kiro 工作模式" description="控制 Kiro 侧使用的工作模式。一般保持默认即可，只有需要特定模式时再调整。">
                <Select bordered size="sm" value={draft.kiroAgentModeStrategy} onChange={(event) => setDraft((prev) => ({ ...prev, kiroAgentModeStrategy: event.target.value as KiroAgentModeStrategy }))}>
                  <Select.Option value="vibe">vibe（默认兼容）</Select.Option>
                  <Select.Option value="spec">spec（强制规格模式）</Select.Option>
                  <Select.Option value="auto">auto（按账号协议自动）</Select.Option>
                </Select>
              </FieldLabel>
              <FieldLabel title="模型解析策略" description="控制模型名称如何匹配。越严格越少自动转换，越兼容越适合常见简称。">
                <Select bordered size="sm" value={draft.modelResolutionMode} onChange={(event) => setDraft((prev) => ({ ...prev, modelResolutionMode: event.target.value as ModelResolutionMode }))}>
                  <Select.Option value="compatible">默认兼容解析</Select.Option>
                  <Select.Option value="alias_only">仅精确与显式别名</Select.Option>
                  <Select.Option value="exact_only">仅完整模型名</Select.Option>
                </Select>
              </FieldLabel>
              <FieldLabel title="模型映射规则" description="把常见模型简称或别名转换成实际可用的模型名。">
                <div className="space-y-3">
                  <div className="grid gap-3 lg:grid-cols-2">
                    <ToggleField title="启用模型映射" description="开启后，系统会按规则转换模型名称。" checked={draft.modelMapping.enabled} onChange={(enabled) => setDraft((prev) => ({ ...prev, modelMapping: { ...prev.modelMapping, enabled } }))} />
                    <ToggleField title="自动生成规则" description="根据当前可用模型自动生成常用别名规则。" checked={draft.modelMapping.autoGenerateRules} onChange={(autoGenerateRules) => setDraft((prev) => ({ ...prev, modelMapping: { ...prev.modelMapping, autoGenerateRules } }))} />
                  </div>
                  <div className="flex flex-wrap gap-2">
                    <Button size="sm" variant="outline" disabled={modelCapabilities.isLoading} onClick={() => {
                      if (!defaultModelMappingRules.length) {
                        toast.error('当前可用模型列表为空，无法生成默认规则')
                        return
                      }
                      setDraft((prev) => ({ ...prev, modelMapping: { ...prev.modelMapping, enabled: true, autoGenerateRules: true, rules: defaultModelMappingRules } }))
                      toast.success(`已填充 ${defaultModelMappingRules.length} 条默认模型映射规则`)
                    }}>
                      <Wand2 className="h-4 w-4" />
                      填充默认规则
                    </Button>
                  </div>
                  <textarea
                    className="textarea textarea-bordered min-h-40 w-full font-mono text-xs"
                    value={JSON.stringify(draft.modelMapping.rules, null, 2)}
                    onChange={(event) => {
                      try {
                        const rules = JSON.parse(event.target.value)
                        if (Array.isArray(rules)) {
                          setDraft((prev) => ({ ...prev, modelMapping: { ...prev.modelMapping, rules } }))
                        }
                      } catch {
                        // 保持输入态，保存前不会应用非法 JSON。
                      }
                    }}
                  />
                  <div className="text-xs text-base-content/60">当前规则 {draft.modelMapping.rules.length} 条；可生成默认规则 {defaultModelMappingRules.length} 条。</div>
                </div>
              </FieldLabel>
              <FieldLabel title="思考触发策略" description="按请求触发会遵循 Claude Code CLI 的 thinking 语义；总是触发会在请求没有明确关闭时启用思考输出。">
                <Select bordered size="sm" value={draft.thinkingTriggerMode} onChange={(event) => setDraft((prev) => ({ ...prev, thinkingTriggerMode: event.target.value as RuntimeConfig['thinkingTriggerMode'] }))}>
                  <Select.Option value="real_request">按请求触发</Select.Option>
                  <Select.Option value="always">总是触发</Select.Option>
                </Select>
              </FieldLabel>
              <ToggleField title="整理思考内容" description="开启后，会把响应里的思考内容单独整理出来。" checked={draft.extractThinking} onChange={(extractThinking) => setDraft((prev) => ({ ...prev, extractThinking }))} />
              <ToggleField title="显示处理告警" description="开启后，会把排查提示返回给客户端，方便定位兼容问题。" checked={draft.exposeProxyWarnings} onChange={(exposeProxyWarnings) => setDraft((prev) => ({ ...prev, exposeProxyWarnings }))} />
            </ConfigGroup>
        )}

        {activeTab === 'stats' && (
            <ConfigGroup icon={<Gauge className="h-4 w-4" />} title="后台统计" description="控制页面统计的判断标准，不改变实际请求。">
              <NumberField title="缓存命中判定阈值" description="缓存读取量达到多少时，页面把这次请求算作缓存命中较高的请求。" value={draft.highCacheThreshold} min={0} suffix="Token" onChange={(highCacheThreshold) => setDraft((prev) => ({ ...prev, highCacheThreshold }))} />
            </ConfigGroup>
        )}

            <div className="config-footnote">
              <Shield className="h-4 w-4" />
              <span>保存前会检查数值范围；保存后，新的请求会使用这些设置。</span>
            </div>
          </div>
        </section>
      </div>
    </div>
  )
}
