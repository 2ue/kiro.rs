import type { ReactNode } from 'react'
import { useEffect, useState } from 'react'
import {
  Gauge,
  Eye,
  EyeOff,
  Router,
  Save,
  Shield,
  Sparkles,
  Wand2,
  Zap,
} from 'lucide-react'
import { toast } from 'sonner'
import {
  ErrorState,
  LoadingState,
  PageContainer,
  PageHeader,
} from '@/components/patterns'
import {
  Button,
  Input,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  Spinner,
  Switch,
  Textarea,
} from '@/components/ui'
import { extractErrorMessage } from '@/lib/utils'
import {
  defaultImageProcessing,
  defaultBodyConversion,
  defaultExternalPoolsConfig,
  defaultMissingMaxTokens,
  defaultPromptSteering,
  defaultPromptCacheCreationControl,
  defaultReportedUsage,
  defaultWeightedCapacity,
  emptyRuntimeConfig,
  normalizeCachePolicy,
  normalizeBodyConversion,
  normalizeImageProcessing,
  normalizeMissingMaxTokens,
  normalizeModelMapping,
  normalizePayloadShaping,
  normalizePromptSteering,
  normalizePromptCacheCreationControl,
  normalizeReportedUsage,
  normalizeWeightedCapacity,
  toRatio,
  toScale,
  toWhole,
} from '@/lib/runtime-config-defaults'
import {
  useLoadBalancingMode,
  useRuntimeConfig,
  useSetLoadBalancingMode,
  useUpdateRuntimeConfig,
} from '@/hooks/use-credentials'
import { useModelCapabilities } from '@/hooks/use-usage'
import type { KiroAgentModeStrategy, LoadBalancingMode, ModelMappingConfig, PayloadGuardMode, RuntimeConfig } from '@/types/api'
import {
  CachePolicySettingsSection,
  normalizeDefinedCacheRoute,
  normalizeDefinedCacheRoutes,
  ModelMappingSection,
  PayloadFallbackSection,
  PayloadHistorySection,
} from './runtime-sections'

type RuntimeSectionKey =
  | 'loadBalancing'
  | 'capacity'
  | 'externalPools'
  | 'cooldown'
  | 'scheduler'
  | 'warmup'
  | 'payload'
  | 'cachePolicy'
  | 'modelMapping'
  | 'startupProxy'
  | 'compat'

const runtimeSections: Array<{
  key: RuntimeSectionKey
  title: string
  desc: string
  icon: ReactNode
}> = [
  { key: 'loadBalancing', title: '负载均衡模式', desc: '请求分配给账号的策略', icon: <Gauge className="h-4 w-4" /> },
  { key: 'capacity', title: '请求容量', desc: '并发、排队、重试、超时', icon: <Gauge className="h-4 w-4" /> },
  { key: 'externalPools', title: '路由策略', desc: '控制哪些入口进入本地账号或外部池', icon: <Router className="h-4 w-4" /> },
  { key: 'cooldown', title: '错误恢复 / 冷却', desc: '不同错误类型的暂停策略与退避', icon: <Shield className="h-4 w-4" /> },
  { key: 'scheduler', title: '账号选择权重', desc: '优先使用哪些账号的调度参数', icon: <Gauge className="h-4 w-4" /> },
  { key: 'warmup', title: '新账号预热', desc: '新账号逐步参与请求，稳定后恢复正常', icon: <Sparkles className="h-4 w-4" /> },
  { key: 'payload', title: '请求体处理', desc: '协议转换、大小保护、历史清理和当前请求兜底', icon: <Wand2 className="h-4 w-4" /> },
  { key: 'cachePolicy', title: '缓存策略', desc: '策略模板默认参数和路径绑定', icon: <Zap className="h-4 w-4" /> },
  { key: 'modelMapping', title: '模型解析', desc: '模型名解析策略和映射规则', icon: <Shield className="h-4 w-4" /> },
  { key: 'startupProxy', title: '启动代理', desc: '启动期全局代理只读状态', icon: <Router className="h-4 w-4" /> },
  { key: 'compat', title: '接口兼容', desc: '兼容模式、Kiro 工作模式和响应诊断', icon: <Shield className="h-4 w-4" /> },
]

// ─── 原子组件 ──────────────────────────────────────────────────────────────────

function numberValue(value: string, fallback: number): number {
  const parsed = Number(value)
  return Number.isFinite(parsed) ? parsed : fallback
}

function normalizeExternalPoolRouteMode(value?: string): RuntimeConfig['externalPools']['externalPoolRouteMode'] {
  if (value === 'allow_list' || value === 'deny_list') return value
  return 'allow_all'
}

function normalizeRuleList(value?: string[] | null): string[] {
  return (value ?? []).map((rule) => rule.trim()).filter(Boolean)
}

function parseRuleText(value: string): string[] {
  return value.split(/[\r\n,]+/).map((rule) => rule.trim()).filter(Boolean)
}

function parseStatusCodeList(value: string): number[] {
  const seen = new Set<number>()
  const codes: number[] = []
  for (const raw of value.split(/[\s,，;；]+/)) {
    const code = Number(raw.trim())
    if (!Number.isInteger(code) || code < 100 || code > 599 || seen.has(code)) continue
    seen.add(code)
    codes.push(code)
  }
  return codes
}

function joinStatusCodeList(value: number[] = []): string {
  return value
    .filter((code) => Number.isInteger(code) && code >= 100 && code <= 599)
    .join(', ')
}

function ruleText(value?: string[] | null): string {
  return normalizeRuleList(value).join('\n')
}

function NumField({
  label,
  desc,
  value,
  min,
  max,
  step,
  suffix,
  disabled,
  onChange,
}: {
  label: string
  desc: string
  value: number
  min?: number
  max?: number
  step?: number
  suffix: string
  disabled?: boolean
  onChange: (v: number) => void
}) {
  return (
    <div className="space-y-1.5">
      <div className="text-sm font-semibold text-foreground">{label}</div>
      <div className="text-xs text-muted-foreground leading-relaxed">{desc}</div>
      <div className="flex items-center gap-2">
        <Input
          type="number"
          className="w-full"
          value={value}
          min={min}
          max={max}
          step={step}
          inputMode={step && step < 1 ? 'decimal' : 'numeric'}
          disabled={disabled}
          onChange={(e) => onChange(numberValue(e.target.value, min ?? 0))}
        />
        <span className="min-w-[5rem] shrink-0 text-sm text-muted-foreground">{suffix}</span>
      </div>
    </div>
  )
}

function TogField({
  label,
  desc,
  checked,
  disabled,
  onChange,
}: {
  label: string
  desc: string
  checked: boolean
  disabled?: boolean
  onChange: (v: boolean) => void
}) {
  return (
    <div className="flex items-start justify-between gap-4">
      <div className="min-w-0">
        <div className="text-sm font-semibold text-foreground">{label}</div>
        <div className="mt-0.5 text-xs leading-relaxed text-muted-foreground">{desc}</div>
      </div>
      <Switch checked={checked} disabled={disabled} onCheckedChange={onChange} className="mt-0.5 shrink-0" />
    </div>
  )
}

function TwoCol({ children }: { children: ReactNode }) {
  return <div className="grid gap-4 md:grid-cols-2">{children}</div>
}

function maskSecret(value?: string | null): string {
  if (!value) return '-'
  if (value.length <= 6) return '******'
  return `${value.slice(0, 3)}...${value.slice(-3)}`
}

function ReadOnlySecretField({
  label,
  value,
}: {
  label: string
  value?: string | null
}) {
  const [visible, setVisible] = useState(false)
  return (
    <div className="space-y-1.5">
      <div className="text-sm font-semibold">{label}</div>
      <div className="relative">
        <Input readOnly className="pr-10 font-mono text-xs" value={visible ? value || '-' : maskSecret(value)} />
        <Button
          type="button"
          variant="ghost"
          size="icon-xs"
          className="absolute right-1 top-1/2 -translate-y-1/2"
          onClick={() => setVisible((v) => !v)}
          title={visible ? '隐藏' : '显示'}
        >
          {visible ? <EyeOff className="size-3.5" /> : <Eye className="size-3.5" />}
        </Button>
      </div>
    </div>
  )
}

function StartupProxySection({ config }: { config: RuntimeConfig }) {
  const hasGlobalProxy = Boolean(config.proxyUrl)
  return (
    <div className="space-y-4">
      <div className="rounded-xl border bg-card p-4">
        <div className="mb-4 flex flex-wrap items-center gap-2">
          <div>
            <div className="text-sm font-semibold">全局代理（启动期配置，只读）</div>
            <div className="mt-1 text-xs leading-5 text-muted-foreground">
              它作为未配置账号直连代理、也未绑定代理资源时的默认代理；修改需要调整启动配置并重启服务。
            </div>
          </div>
          <span className={`ml-auto rounded-lg border px-2 py-1 text-xs font-medium ${hasGlobalProxy ? 'border-success/30 bg-success/10 text-success' : 'bg-muted text-muted-foreground'}`}>
            {hasGlobalProxy ? '已配置' : '未配置'}
          </span>
          <span className="rounded-lg border bg-muted px-2 py-1 text-xs font-medium text-muted-foreground">只读</span>
        </div>
        <div className="grid gap-4 md:grid-cols-2">
          <div className="space-y-1.5 md:col-span-2">
            <div className="text-sm font-semibold">代理 URL</div>
            <Input readOnly className="font-mono text-xs" value={config.proxyUrl || '-'} />
          </div>
          <ReadOnlySecretField label="代理用户名" value={config.proxyUsername} />
          <ReadOnlySecretField label="代理密码" value={config.proxyPassword} />
        </div>
      </div>
    </div>
  )
}

// ─── normalizeConfig ──────────────────────────────────────────────────────────

function normalizeConfig(draft: RuntimeConfig): RuntimeConfig {
  const next: RuntimeConfig = {
    ...draft,
    credentialRpm: toWhole(draft.credentialRpm),
    requestAdmission: {
      rpm: toWhole(draft.requestAdmission.rpm, 0, 1_000_000),
      maxConcurrentRequests: toWhole(draft.requestAdmission.maxConcurrentRequests, 0, 10_000),
      maxQueuedRequests: toWhole(draft.requestAdmission.maxQueuedRequests, 0, 100_000),
      queueTimeoutMs: toWhole(draft.requestAdmission.queueTimeoutMs, 0, 300_000),
    },
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
    kiroUpstreamStreamRetryEnabled: Boolean(draft.kiroUpstreamStreamRetryEnabled),
    kiroUpstreamStreamRetryMaxAttempts: toWhole(draft.kiroUpstreamStreamRetryMaxAttempts, 1, 100),
    inferenceUpstreamMaxAttempts: toWhole(draft.inferenceUpstreamMaxAttempts, 1, 10),
    auxiliaryUpstreamMaxAttempts: toWhole(draft.auxiliaryUpstreamMaxAttempts, 1, 10),
    auxiliaryUpstreamMaxConcurrentRequests: toWhole(draft.auxiliaryUpstreamMaxConcurrentRequests, 1, 256),
    tokenRefreshMaxRpm: toWhole(draft.tokenRefreshMaxRpm, 1, 6000),
    tokenRefreshBurst: toWhole(draft.tokenRefreshBurst, 1, 256),
    kiroUpstreamStreamRetryOnIdleTimeout: Boolean(draft.kiroUpstreamStreamRetryOnIdleTimeout),
    kiroUpstreamStreamRetryOnReadError: Boolean(draft.kiroUpstreamStreamRetryOnReadError),
    kiroUpstreamStreamRetryOnStatusError: Boolean(draft.kiroUpstreamStreamRetryOnStatusError),
    credentialRetryMaxAttempts: toWhole(draft.credentialRetryMaxAttempts),
    credentialPromptLogicRetryEnabled: Boolean(draft.credentialPromptLogicRetryEnabled),
    credentialPromptLogicRetryMaxAttempts: toWhole(draft.credentialPromptLogicRetryMaxAttempts),
    credentialInFlightLeaseMaxSecs: toWhole(draft.credentialInFlightLeaseMaxSecs),
    dispatchGlobalMaxConcurrentRequests: toWhole(draft.dispatchGlobalMaxConcurrentRequests),
    dispatchMaxQueuedRequests: toWhole(draft.dispatchMaxQueuedRequests),
    weightedCapacity: normalizeWeightedCapacity(draft.weightedCapacity),
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
    payloadShaping: normalizePayloadShaping(draft.payloadShaping),
    payloadGuardMaxBytes: toWhole(draft.payloadGuardMaxBytes),
    payloadGuardSafetyMarginBytes: toWhole(draft.payloadGuardSafetyMarginBytes),
    promptCacheTargetReadRatio: toRatio(draft.promptCacheTargetReadRatio),
    promptCacheTokenScale: toScale(draft.promptCacheTokenScale),
    promptCacheMaxSimulatedInputTokens: toWhole(draft.promptCacheMaxSimulatedInputTokens),
    promptCacheCapJitterMinTokens: toWhole(draft.promptCacheCapJitterMinTokens),
    promptCacheCapJitterMaxTokens: toWhole(draft.promptCacheCapJitterMaxTokens),
    promptCacheScaleMinInputTokens: toWhole(draft.promptCacheScaleMinInputTokens),
    promptCacheMaxEntriesPerAccount: toWhole(draft.promptCacheMaxEntriesPerAccount),
    promptCacheMaxEntriesGlobal: toWhole(draft.promptCacheMaxEntriesGlobal),
    promptCacheEntryTtlSecs: toWhole(draft.promptCacheEntryTtlSecs, 1),
    promptCacheEstimatedBytesLimit: toWhole(draft.promptCacheEstimatedBytesLimit),
    highCacheThreshold: toWhole(draft.highCacheThreshold),
    promptCacheCreationControl: normalizePromptCacheCreationControl(draft.promptCacheCreationControl),
    imageProcessing: normalizeImageProcessing(draft.imageProcessing),
    bodyConversion: normalizeBodyConversion(draft.bodyConversion),
    promptSteering: normalizePromptSteering(draft.promptSteering),
    missingMaxTokens: normalizeMissingMaxTokens(draft.missingMaxTokens),
    reportedUsage: normalizeReportedUsage(draft.reportedUsage),
    cachePolicy: normalizeCachePolicy(draft.cachePolicy),
    definedCacheRoutes: normalizeDefinedCacheRoutes(draft.definedCacheRoutes),
    modelMapping: normalizeModelMapping(draft.modelMapping),
    externalPools: {
      ...defaultExternalPoolsConfig(),
      ...draft.externalPools,
      externalPoolGlobalMaxConcurrentRequests: toWhole(draft.externalPools.externalPoolGlobalMaxConcurrentRequests),
      externalPoolMaxQueuedRequests: toWhole(draft.externalPools.externalPoolMaxQueuedRequests),
      externalPoolMaxInputTokens: toWhole(draft.externalPools.externalPoolMaxInputTokens),
      externalPoolDispatchMaxWaitSecs: toWhole(draft.externalPools.externalPoolDispatchMaxWaitSecs, 1),
      externalPoolRetryMaxAttempts: toWhole(draft.externalPools.externalPoolRetryMaxAttempts),
      externalPoolRetryStatusCodes: parseStatusCodeList(joinStatusCodeList(draft.externalPools.externalPoolRetryStatusCodes)),
      externalPoolRetryOnNetworkError: Boolean(draft.externalPools.externalPoolRetryOnNetworkError),
      externalPoolRetryOnProtocolError: Boolean(draft.externalPools.externalPoolRetryOnProtocolError),
      externalPoolSamePoolRetryCount: toWhole(draft.externalPools.externalPoolSamePoolRetryCount),
      externalPoolSamePoolRetryStatusCodes: parseStatusCodeList(joinStatusCodeList(draft.externalPools.externalPoolSamePoolRetryStatusCodes)),
      externalPoolSamePoolRetryDelayMs: toWhole(draft.externalPools.externalPoolSamePoolRetryDelayMs),
      externalPoolTransientFailurePriorityPenalty: toWhole(draft.externalPools.externalPoolTransientFailurePriorityPenalty),
      externalPoolTransientFailureCooldownThreshold: toWhole(draft.externalPools.externalPoolTransientFailureCooldownThreshold),
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
      externalPoolModelUnavailableCooldownMode: draft.externalPools.externalPoolModelUnavailableCooldownMode,
      externalPoolModelUnavailableCooldownSecs: toWhole(draft.externalPools.externalPoolModelUnavailableCooldownSecs, 1),
      externalPoolRequestTimeoutSecs: toWhole(draft.externalPools.externalPoolRequestTimeoutSecs),
      externalPoolStreamRequestTimeoutSecs: toWhole(draft.externalPools.externalPoolStreamRequestTimeoutSecs),
      externalPoolStreamIdleTimeoutSecs: toWhole(draft.externalPools.externalPoolStreamIdleTimeoutSecs),
      externalPoolRouteMode: normalizeExternalPoolRouteMode(draft.externalPools.externalPoolRouteMode),
      externalPoolRouteRules: normalizeRuleList(draft.externalPools.externalPoolRouteRules),
      localPoolRouteMode: normalizeExternalPoolRouteMode(draft.externalPools.localPoolRouteMode),
      localPoolRouteRules: normalizeRuleList(draft.externalPools.localPoolRouteRules),
      externalPoolUsageProjectionUpliftPercent: toWhole(draft.externalPools.externalPoolUsageProjectionUpliftPercent),
      externalPoolUsageProjectionCostFloorEnabled: Boolean(draft.externalPools.externalPoolUsageProjectionCostFloorEnabled),
      externalPoolUsageProjectionCostFloorMarginPercent: toWhole(draft.externalPools.externalPoolUsageProjectionCostFloorMarginPercent),
      externalPoolUsageProjectionOutputUpliftMinTokens: toWhole(draft.externalPools.externalPoolUsageProjectionOutputUpliftMinTokens),
      externalPoolUsageProjectionOutputUpliftPercent: toWhole(draft.externalPools.externalPoolUsageProjectionOutputUpliftPercent),
      externalPoolUsageDebugEnabled: Boolean(draft.externalPools.externalPoolUsageDebugEnabled),
      externalPoolUsageDebugDir: String(draft.externalPools.externalPoolUsageDebugDir || '').trim(),
      externalPoolUsageDebugMaxBodyBytes: toWhole(draft.externalPools.externalPoolUsageDebugMaxBodyBytes),
      externalPoolUsageDebugMaxFiles: toWhole(draft.externalPools.externalPoolUsageDebugMaxFiles),
    },
  }
  return next
}

// ─── RuntimePage ──────────────────────────────────────────────────────────────

export function RuntimePage() {
  const config = useRuntimeConfig()
  const updateConfig = useUpdateRuntimeConfig()
  const loadBalancing = useLoadBalancingMode()
  const setLbMode = useSetLoadBalancingMode()
  const modelCapabilities = useModelCapabilities()
  const [draft, setDraft] = useState<RuntimeConfig>(emptyRuntimeConfig)
  const [activeSection, setActiveSection] = useState<RuntimeSectionKey>('loadBalancing')
  const [externalRouteRulesText, setExternalRouteRulesText] = useState('')
  const [localRouteRulesText, setLocalRouteRulesText] = useState('')
  const [promptSteeringRouteRulesText, setPromptSteeringRouteRulesText] = useState('')

  useEffect(() => {
    if (config.data) {
      const bodyConversion = normalizeBodyConversion(config.data.bodyConversion ?? defaultBodyConversion())
      const rawPromptSteering = (config.data as Partial<RuntimeConfig>).promptSteering
      const promptSteering = normalizePromptSteering(rawPromptSteering ?? {
        ...defaultPromptSteering(),
        toolChoice: { enabled: bodyConversion.toolChoiceSteering },
        chunkedWrite: {
          ...defaultPromptSteering().chunkedWrite,
          enabled: bodyConversion.chunkedToolPolicy,
        },
        thinking: { enabled: bodyConversion.thinkingPromptControls },
      })
      setDraft({
        ...emptyRuntimeConfig,
        ...config.data,
        requestAdmission: {
          ...emptyRuntimeConfig.requestAdmission,
          ...config.data.requestAdmission,
        },
        auxiliaryUpstreamRuntime: {
          ...emptyRuntimeConfig.auxiliaryUpstreamRuntime,
          ...config.data.auxiliaryUpstreamRuntime,
        },
        tokenRefreshAdmissionRuntime: {
          ...emptyRuntimeConfig.tokenRefreshAdmissionRuntime,
          ...config.data.tokenRefreshAdmissionRuntime,
        },
        imageProcessing: normalizeImageProcessing(config.data.imageProcessing ?? defaultImageProcessing()),
        bodyConversion,
        promptSteering,
        missingMaxTokens: normalizeMissingMaxTokens(config.data.missingMaxTokens ?? defaultMissingMaxTokens()),
        weightedCapacity: normalizeWeightedCapacity(config.data.weightedCapacity ?? defaultWeightedCapacity()),
        externalPools: {
          ...defaultExternalPoolsConfig(),
          ...config.data.externalPools,
          externalPoolRouteMode: normalizeExternalPoolRouteMode(config.data.externalPools?.externalPoolRouteMode),
          externalPoolRouteRules: normalizeRuleList(config.data.externalPools?.externalPoolRouteRules),
          localPoolRouteMode: normalizeExternalPoolRouteMode(config.data.externalPools?.localPoolRouteMode),
          localPoolRouteRules: normalizeRuleList(config.data.externalPools?.localPoolRouteRules),
        },
        payloadShaping: normalizePayloadShaping(config.data.payloadShaping),
        promptCacheCreationControl: { ...defaultPromptCacheCreationControl(), ...config.data.promptCacheCreationControl },
        reportedUsage: normalizeReportedUsage(config.data.reportedUsage ?? defaultReportedUsage()),
        cachePolicy: normalizeCachePolicy(config.data.cachePolicy),
        definedCacheRoutes: normalizeDefinedCacheRoutes(config.data.definedCacheRoutes || []),
        modelMapping: normalizeModelMapping(config.data.modelMapping),
      })
      setExternalRouteRulesText(ruleText(config.data.externalPools?.externalPoolRouteRules))
      setLocalRouteRulesText(ruleText(config.data.externalPools?.localPoolRouteRules))
      setPromptSteeringRouteRulesText(ruleText(promptSteering.routeRules))
    }
  }, [config.data])

  const set = <K extends keyof RuntimeConfig>(k: K) => (v: RuntimeConfig[K]) =>
    setDraft((prev) => ({ ...prev, [k]: v }))

  const setRequestAdmission = <K extends keyof RuntimeConfig['requestAdmission']>(k: K) => (v: RuntimeConfig['requestAdmission'][K]) =>
    setDraft((prev) => ({
      ...prev,
      requestAdmission: { ...prev.requestAdmission, [k]: v },
    }))

  const setImageProcessing = <K extends keyof RuntimeConfig['imageProcessing']>(k: K) => (v: RuntimeConfig['imageProcessing'][K]) =>
    setDraft((prev) => ({
      ...prev,
      imageProcessing: {
        ...defaultImageProcessing(),
        ...prev.imageProcessing,
        [k]: v,
      },
    }))

  const setExternalPools = <K extends keyof RuntimeConfig['externalPools']>(k: K) => (v: RuntimeConfig['externalPools'][K]) =>
    setDraft((prev) => ({
      ...prev,
      externalPools: {
        ...defaultExternalPoolsConfig(),
        ...prev.externalPools,
        [k]: v,
      },
    }))

  const setBodyConversion = <K extends keyof RuntimeConfig['bodyConversion']>(k: K) => (v: RuntimeConfig['bodyConversion'][K]) =>
    setDraft((prev) => ({
      ...prev,
      bodyConversion: {
        ...defaultBodyConversion(),
        ...prev.bodyConversion,
        [k]: v,
      },
    }))

  const setPromptSteering = <K extends keyof RuntimeConfig['promptSteering']>(k: K) => (v: RuntimeConfig['promptSteering'][K]) =>
    setDraft((prev) => ({
      ...prev,
      promptSteering: {
        ...defaultPromptSteering(),
        ...prev.promptSteering,
        [k]: v,
      },
    }))

  const setPromptSteeringText =
    (block: 'languageConstraint' | 'taskQuality' | 'custom', key: 'enabled' | 'prompt') =>
    (v: boolean | string) =>
      setDraft((prev) => ({
        ...prev,
        promptSteering: {
          ...defaultPromptSteering(),
          ...prev.promptSteering,
          [block]: {
            ...defaultPromptSteering()[block],
            ...prev.promptSteering?.[block],
            [key]: v,
          },
        },
      }))

  const setPromptSteeringToggle = (key: 'toolChoice' | 'thinking') => (enabled: boolean) =>
    setDraft((prev) => ({
      ...prev,
      promptSteering: {
        ...defaultPromptSteering(),
        ...prev.promptSteering,
        [key]: { enabled },
      },
    }))

  const setChunkedWriteSteering =
    (key: keyof RuntimeConfig['promptSteering']['chunkedWrite']) =>
    (value: boolean) =>
      setDraft((prev) => ({
        ...prev,
        promptSteering: {
          ...defaultPromptSteering(),
          ...prev.promptSteering,
          chunkedWrite: {
            ...defaultPromptSteering().chunkedWrite,
            ...prev.promptSteering?.chunkedWrite,
            [key]: value,
          },
        },
      }))

  const setMissingMaxTokens = <K extends keyof RuntimeConfig['missingMaxTokens']>(k: K) => (v: RuntimeConfig['missingMaxTokens'][K]) =>
    setDraft((prev) => ({
      ...prev,
      missingMaxTokens: {
        ...defaultMissingMaxTokens(),
        ...prev.missingMaxTokens,
        [k]: v,
      },
    }))

  const save = () => {
    const invalidDefinedCacheRoute = (draft.definedCacheRoutes || []).find((route) => route.trim() && !normalizeDefinedCacheRoute(route))
    if (invalidDefinedCacheRoute)
      return toast.error('缓存策略里的 /dfcache 路径必须是 /dfcache/{name}，name 只能包含字母、数字、点、下划线或短横线')
    const next = normalizeConfig(draft)
    if (next.credentialTransientCooldownSecs > next.credentialMaxCooldownSecs)
      return toast.error('临时冷却秒数不能大于最大冷却秒数')
    if ([next.credentialRateLimitCooldownSecs, next.credentialServerErrorCooldownSecs, next.credentialNetworkErrorCooldownSecs, next.credentialStreamErrorCooldownSecs, next.credentialProtocolErrorCooldownSecs, next.credentialAuthErrorCooldownSecs].some((value) => value > next.credentialMaxCooldownSecs))
      return toast.error('错误类型基础冷却秒数不能大于最大冷却秒数')
    if (next.promptCacheCapJitterMinTokens > next.promptCacheCapJitterMaxTokens)
      return toast.error('触顶扣减下限不能大于上限')
    if (next.payloadGuardMaxBytes > 0 && next.payloadGuardMaxBytes < 65536)
      return toast.error('处理阈值必须为 0 或不小于 65536 字节')
    if (next.payloadGuardMaxBytes - next.payloadGuardSafetyMarginBytes < 65536 && next.payloadGuardMaxBytes > 0)
      return toast.error('安全余量不能过大,处理阈值减去安全余量需不小于 65536 字节')
    if (next.missingMaxTokens.defaultValue < 1 || next.missingMaxTokens.defaultValue > 200000)
      return toast.error('缺失 max_tokens 的补充值必须在 1 到 200000 之间')
    const editable = { ...next }
    delete editable.proxyUrl
    delete editable.proxyUsername
    delete editable.proxyPassword
    updateConfig.mutate(editable, {
      onSuccess: () => toast.success('配置已保存，新请求立即生效'),
      onError: (e) => toast.error(`保存失败: ${extractErrorMessage(e)}`),
    })
  }

  const handleLbMode = (mode: LoadBalancingMode) => {
    const label = mode === 'priority'
      ? '优先级'
      : mode === 'balanced'
        ? '均衡负载'
        : mode === 'health_balanced'
          ? '健康均衡'
          : '低负载优先'
    setLbMode.mutate(mode, {
      onSuccess: () => toast.success(`已切换为${label}模式`),
      onError: (e) => toast.error(`切换失败: ${extractErrorMessage(e)}`),
    })
  }

  if (config.isLoading) return <LoadingState text="加载运行配置..." />
  if (config.error) return <ErrorState message={extractErrorMessage(config.error)} />

  const payloadSizeLimitEnabled = draft.payloadGuardEnabled && draft.payloadGuardMaxBytes > 0
  const payloadGuardMode = (draft.payloadGuardMode ?? 'preemptive') as PayloadGuardMode
  const imageProcessingMode = draft.imageProcessing?.mode ?? 'safe'
  const activeMeta = runtimeSections.find((section) => section.key === activeSection) ?? runtimeSections[0]!

  return (
    <PageContainer>
      <PageHeader
        title="运行配置"
        subtitle="调度、限流、冷却、缓存与兼容等运行时参数，保存后新请求立即生效"
        actions={
          <Button size="sm" onClick={save} disabled={updateConfig.isPending}>
            {updateConfig.isPending ? <Spinner size="sm" /> : <Save className="h-4 w-4" />}
            保存配置
          </Button>
        }
      />

      <div className="grid gap-4 lg:grid-cols-[17rem_minmax(0,1fr)]">
        <aside className="rounded-xl bg-card p-2 shadow-sm lg:sticky lg:top-4 lg:self-start" aria-label="运行配置分类">
          <div className="flex gap-2 overflow-x-auto lg:flex-col lg:overflow-visible" role="tablist" aria-label="运行配置分类">
            {runtimeSections.map((section) => {
              const active = section.key === activeSection
              return (
                <button
                  key={section.key}
                  type="button"
                  role="tab"
                  aria-selected={active}
                  className={`flex min-w-[12rem] items-start gap-3 rounded-lg px-3 py-2.5 text-left transition-colors lg:min-w-0 ${
                    active ? 'bg-primary/10 text-primary' : 'text-muted-foreground hover:bg-muted/60 hover:text-foreground'
                  }`}
                  onClick={() => setActiveSection(section.key)}
                >
                  <span className={`mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-md ${
                    active ? 'bg-primary/15 text-primary' : 'bg-muted text-muted-foreground'
                  }`}>
                    {section.icon}
                  </span>
                  <span className="min-w-0">
                    <span className="block truncate text-sm font-semibold">{section.title}</span>
                    <span className={`mt-0.5 block truncate text-xs ${active ? 'text-primary/70' : 'text-muted-foreground'}`}>
                      {section.desc}
                    </span>
                  </span>
                </button>
              )
            })}
          </div>
        </aside>

        <section className="rounded-xl bg-card p-4 shadow-sm">
          <div className="mb-5 flex items-start gap-3">
            <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-muted text-muted-foreground [&_svg]:size-4">
              {activeMeta.icon}
            </span>
            <div className="min-w-0">
              <h2 className="text-base font-semibold text-foreground">{activeMeta.title}</h2>
              <p className="mt-1 text-xs leading-5 text-muted-foreground">{activeMeta.desc}</p>
            </div>
          </div>

          <div className="space-y-4">
            {activeSection === 'loadBalancing' && (
              <div className="space-y-3">
                <div className="text-xs text-muted-foreground">
                  切换后立即生效，无需点击保存。
                </div>
                <Select
                  value={loadBalancing.data?.mode ?? 'priority'}
                  onValueChange={(v) => handleLbMode(v as LoadBalancingMode)}
                  disabled={setLbMode.isPending || loadBalancing.isLoading}
                >
                  <SelectTrigger className="w-56">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="priority">优先级模式 — 高优先级账号优先</SelectItem>
                    <SelectItem value="balanced">均衡负载 — 平均分配请求</SelectItem>
                    <SelectItem value="health_balanced">健康均衡 — 综合健康度平衡</SelectItem>
                    <SelectItem value="weighted_least_inflight">低负载优先 — 高并发时优先空闲账号</SelectItem>
                  </SelectContent>
                </Select>
              </div>
            )}

            {activeSection === 'capacity' && (
              <TwoCol>
                <NumField label="每实例 · 每 Key RPM" desc="每个实例分别限制每个已认证请求 API Key；多实例总量最多可近似放大为实例数倍。0 表示关闭。" value={draft.requestAdmission.rpm} min={0} max={1_000_000} suffix="次/分钟" onChange={setRequestAdmission('rpm')} />
                <NumField label="每实例 · 每 Key 并发" desc="每个实例分别统计同一请求 API Key 持有的 /messages response body；多实例不是全局聚合。0 表示关闭。" value={draft.requestAdmission.maxConcurrentRequests} min={0} max={10_000} suffix="并发" onChange={setRequestAdmission('maxConcurrentRequests')} />
                <NumField label="每实例 · 每 Key 队列" desc="每个实例内同一 Key 并发占满时最多等待的请求数；0 表示不排队并立即返回 429。" value={draft.requestAdmission.maxQueuedRequests} min={0} max={100_000} suffix="请求" onChange={setRequestAdmission('maxQueuedRequests')} />
                <NumField label="每实例 · 每 Key 等待" desc="每个实例内同一 Key 等待并发名额的最长时间；0 表示不排队并立即返回 429。" value={draft.requestAdmission.queueTimeoutMs} min={0} max={300_000} suffix="毫秒" onChange={setRequestAdmission('queueTimeoutMs')} />
                <NumField label="单账号每分钟请求上限" desc="每个账号一分钟最多接多少个请求；0 表示不做本地限速。" value={draft.credentialRpm} min={0} suffix="次/分钟" onChange={set('credentialRpm')} />
                <NumField label="单账号最大并发" desc="每个账号同一时间最多处理多少个请求；0 表示不限制。" value={draft.credentialMaxConcurrentRequests} min={0} suffix="并发" onChange={set('credentialMaxConcurrentRequests')} />
                <NumField label="全局最大并发" desc="整个服务同一时间最多处理多少个请求；0 表示不限制。" value={draft.dispatchGlobalMaxConcurrentRequests} min={0} suffix="并发" onChange={set('dispatchGlobalMaxConcurrentRequests')} />
                <NumField label="最大排队请求数" desc="账号忙不过来时最多让多少个请求排队；0 表示不限制。" value={draft.dispatchMaxQueuedRequests} min={0} suffix="请求" onChange={set('dispatchMaxQueuedRequests')} />
                <TogField
                  label="按 token 重量计算本地容量"
                  desc="默认关闭。关闭时不改变本地并发/RPM 口径；开启后只使用请求链路已有的粗略输入 token，不为容量单独遍历 body。"
                  checked={draft.weightedCapacity.enabled}
                  onChange={(enabled) =>
                    setDraft((prev) => ({
                      ...prev,
                      weightedCapacity: { ...prev.weightedCapacity, enabled },
                    }))
                  }
                />
                <NumField
                  label="单请求最大容量单位"
                  desc="限制超长上下文最多占用多少本地并发/RPM 单位；只影响本地账号，不影响外部账号。"
                  value={draft.weightedCapacity.maxUnitsPerRequest}
                  min={1}
                  max={64}
                  suffix="单位"
                  disabled={!draft.weightedCapacity.enabled}
                  onChange={(maxUnitsPerRequest) =>
                    setDraft((prev) => ({
                      ...prev,
                      weightedCapacity: {
                        ...prev.weightedCapacity,
                        maxUnitsPerRequest,
                        tiers: prev.weightedCapacity.tiers.map((tier) => ({
                          ...tier,
                          units: Math.min(Math.max(1, tier.units), Math.max(1, maxUnitsPerRequest)),
                        })),
                      },
                    }))
                  }
                />
                <div className="space-y-3 rounded-lg border bg-muted/20 p-4 md:col-span-2">
                  <div>
                    <div className="text-sm font-semibold">容量分档</div>
                    <div className="mt-1 text-xs leading-5 text-muted-foreground">
                      命中不超过当前输入 token 的最高分档。默认 0=1、100k=2、300k=4、700k=8。
                    </div>
                  </div>
                  <div className="grid gap-3 md:grid-cols-2">
                    {draft.weightedCapacity.tiers.map((tier, index) => (
                      <div key={index} className="grid grid-cols-2 gap-3 rounded-lg border bg-background p-3">
                        <NumField
                          label="起始 token"
                          desc="大于等于该值"
                          value={tier.minTokens}
                          min={0}
                          suffix="token"
                          disabled={!draft.weightedCapacity.enabled}
                          onChange={(minTokens) =>
                            setDraft((prev) => ({
                              ...prev,
                              weightedCapacity: {
                                ...prev.weightedCapacity,
                                tiers: prev.weightedCapacity.tiers.map((item, itemIndex) =>
                                  itemIndex === index ? { ...item, minTokens } : item,
                                ),
                              },
                            }))
                          }
                        />
                        <NumField
                          label="容量单位"
                          desc="并发/RPM 权重"
                          value={tier.units}
                          min={1}
                          max={draft.weightedCapacity.maxUnitsPerRequest}
                          suffix="单位"
                          disabled={!draft.weightedCapacity.enabled}
                          onChange={(units) =>
                            setDraft((prev) => ({
                              ...prev,
                              weightedCapacity: {
                                ...prev.weightedCapacity,
                                tiers: prev.weightedCapacity.tiers.map((item, itemIndex) =>
                                  itemIndex === index ? { ...item, units } : item,
                                ),
                              },
                            }))
                          }
                        />
                      </div>
                    ))}
                  </div>
                </div>
                <NumField label="单请求最长排队等待" desc="一个请求最多等账号空闲多久；0 表示不限制。" value={draft.credentialDispatchMaxWaitSecs} min={0} suffix="秒" onChange={set('credentialDispatchMaxWaitSecs')} />
                <NumField label="开始响应等待时间" desc="发给上游后，多久还没开始返回就认为超时；0 表示使用默认超时。" value={draft.kiroUpstreamResponseTimeoutSecs} min={0} suffix="秒" onChange={set('kiroUpstreamResponseTimeoutSecs')} />
                <NumField label="流式静默超时" desc="流式响应长时间没有新内容时，结束本次请求。" value={draft.kiroUpstreamStreamIdleTimeoutSecs} min={0} suffix="秒" onChange={set('kiroUpstreamStreamIdleTimeoutSecs')} />
                <TogField label="首输出前流式换号" desc="仅在还没向客户端发送任何 SSE 事件前生效；已输出 message_start、文本或工具调用后不会重试。" checked={draft.kiroUpstreamStreamRetryEnabled} onChange={set('kiroUpstreamStreamRetryEnabled')} />
                <NumField label="首输出前最多尝试" desc="包含第一次调用；默认 2。只用于流读取错误、流静默超时或 2xx JSON 错误体等首输出前失败。" value={draft.kiroUpstreamStreamRetryMaxAttempts} min={1} max={100} suffix="次" disabled={!draft.kiroUpstreamStreamRetryEnabled} onChange={set('kiroUpstreamStreamRetryMaxAttempts')} />
                <NumField label="单请求推理发送硬上限" desc="本地换号、首输出前重试、请求体重试、外部池故障转移和本地救援共享；默认 4，与账号数量无关。" value={draft.inferenceUpstreamMaxAttempts} min={1} max={10} suffix="次" onChange={set('inferenceUpstreamMaxAttempts')} />
                <NumField label="单请求辅助发送硬上限" desc="Token 刷新与企业 Profile 探测共享；默认 2，与账号数量无关，不计入推理发送次数。" value={draft.auxiliaryUpstreamMaxAttempts} min={1} max={10} suffix="次" onChange={set('auxiliaryUpstreamMaxAttempts')} />
                <NumField label="单实例辅助并发上限" desc="限制同时进行的 Token 刷新、Profile 探测和模型目录请求；饱和时立即拒绝，不进入无界等待队列。" value={draft.auxiliaryUpstreamMaxConcurrentRequests} min={1} max={256} suffix="路" onChange={set('auxiliaryUpstreamMaxConcurrentRequests')} />
                <NumField label="Token 刷新 RPM 上限" desc="Redis 可用时为跨实例共享上限；未配置 Redis 时为单进程上限。" value={draft.tokenRefreshMaxRpm} min={1} max={6000} suffix="RPM" onChange={set('tokenRefreshMaxRpm')} />
                <NumField label="Token 刷新突发容量" desc="允许立即发送的刷新数量；之后按 RPM 速率补充。" value={draft.tokenRefreshBurst} min={1} max={256} suffix="次" onChange={set('tokenRefreshBurst')} />
                <div className="rounded-lg border bg-muted/20 p-4 text-xs leading-5 text-muted-foreground md:col-span-2">
                  当前辅助通道：进行中 {draft.auxiliaryUpstreamRuntime.inFlight}，历史峰值 {draft.auxiliaryUpstreamRuntime.peakInFlight}，饱和拒绝 {draft.auxiliaryUpstreamRuntime.rejected}。Refresh client 缓存 {draft.auxiliaryUpstreamRuntime.refreshClientCacheEntries}/{draft.auxiliaryUpstreamRuntime.refreshClientCacheMaxEntries}，构建 {draft.auxiliaryUpstreamRuntime.refreshClientBuilds}，命中 {draft.auxiliaryUpstreamRuntime.refreshClientHits}，未命中 {draft.auxiliaryUpstreamRuntime.refreshClientMisses}，容量拒绝 {draft.auxiliaryUpstreamRuntime.refreshClientCacheSaturated}。
                  <br />Token refresh authority {draft.tokenRefreshAdmissionRuntime.authority}，准入 {draft.tokenRefreshAdmissionRuntime.admitted}，RPM 拒绝 {draft.tokenRefreshAdmissionRuntime.rateLimited}，协调拒绝 {draft.tokenRefreshAdmissionRuntime.coordinationRejected}，Redis 错误 {draft.tokenRefreshAdmissionRuntime.redisErrors}，剩余 {draft.tokenRefreshAdmissionRuntime.remainingMilliTokens / 1000} tokens。
                </div>
                <TogField label="静默超时可换号" desc="上游流在首输出前长时间无内容时允许换号。" checked={draft.kiroUpstreamStreamRetryOnIdleTimeout} disabled={!draft.kiroUpstreamStreamRetryEnabled} onChange={set('kiroUpstreamStreamRetryOnIdleTimeout')} />
                <TogField label="读取错误可换号" desc="首输出前连接中断、流读取失败时允许换号。" checked={draft.kiroUpstreamStreamRetryOnReadError} disabled={!draft.kiroUpstreamStreamRetryEnabled} onChange={set('kiroUpstreamStreamRetryOnReadError')} />
                <TogField label="状态错误可换号" desc="首输出前收到 2xx JSON 错误体或上游错误状态事件时允许换号；请求体 400 仍按请求错误处理。" checked={draft.kiroUpstreamStreamRetryOnStatusError} disabled={!draft.kiroUpstreamStreamRetryEnabled} onChange={set('kiroUpstreamStreamRetryOnStatusError')} />
                <NumField label="本地 provider 尝试上限" desc="单次本地调用最多尝试多少个凭据；0 表示默认 3，且仍受共享硬上限约束。" value={draft.credentialRetryMaxAttempts} min={0} suffix="次" onChange={set('credentialRetryMaxAttempts')} />
                <TogField label="提示逻辑错误换号" desc="开启后，部分模型已解析成功但上游返回提示/工具协议 400 的请求，会换未尝试账号重试。" checked={draft.credentialPromptLogicRetryEnabled} onChange={set('credentialPromptLogicRetryEnabled')} />
                <NumField label="提示逻辑最多换号" desc="仅在上方开关开启时生效；0 表示默认 1 次。" value={draft.credentialPromptLogicRetryMaxAttempts} min={0} suffix="次" disabled={!draft.credentialPromptLogicRetryEnabled} onChange={set('credentialPromptLogicRetryMaxAttempts')} />
                <NumField label="异常并发自动回收" desc="请求长时间没有结束时自动释放占用，避免账号并发数被卡住；0 表示关闭。" value={draft.credentialInFlightLeaseMaxSecs} min={0} suffix="秒" onChange={set('credentialInFlightLeaseMaxSecs')} />
              </TwoCol>
            )}

            {activeSection === 'externalPools' && (
              <div className="space-y-4">
                <TwoCol>
                  <TogField
                    label="启用外部池"
                    desc="允许请求在本地凭据不可调度或策略命中时进入外部池；下方路由规则会继续限制入口。"
                    checked={draft.externalPools.externalPoolsEnabled}
                    onChange={setExternalPools('externalPoolsEnabled')}
                  />
                  <TogField
                    label="外部直连策略"
                    desc="开启后命中直连模型或路径规则的请求直接走外部池；关闭时外部池只作为本地不可用后的兜底。"
                    checked={draft.externalPools.externalDirectPolicyEnabled}
                    onChange={setExternalPools('externalDirectPolicyEnabled')}
                  />
                  <TogField
                    label="本地不可调度时预检外部池"
                    desc="本地账号容量不足、无可用账号或调度 Redis 退化时，允许请求在解析后优先尝试外部池。"
                    checked={draft.externalPools.localPoolPreflightEnabled}
                    onChange={setExternalPools('localPoolPreflightEnabled')}
                  />
                  <TogField
                    label="外部池失败后本地救援"
                    desc="外部池作为兜底路径失败后，允许最后再尝试一次本地凭据；外部直连策略命中时不会启用该救援。"
                    checked={draft.externalPools.externalPoolLocalRescueEnabled}
                    onChange={setExternalPools('externalPoolLocalRescueEnabled')}
                  />
                </TwoCol>

                <div className="rounded-lg border border-warning/30 bg-warning/5 p-4">
                  <div className="grid gap-4 md:grid-cols-2">
                    <TogField
                      label="外部池 usage 原始数据诊断"
                      desc="临时记录外部池上游原始响应/SSE usage 样本、请求关联信息和本系统解析结果；默认关闭。"
                      checked={draft.externalPools.externalPoolUsageDebugEnabled}
                      onChange={setExternalPools('externalPoolUsageDebugEnabled')}
                    />
                    <label className="space-y-1.5 text-sm">
                      <span className="text-muted-foreground">诊断目录</span>
                      <Input
                        className="font-mono text-xs"
                        value={draft.externalPools.externalPoolUsageDebugDir}
                        placeholder="/tmp/kiro-rs/external-pool-usage-debug"
                        onChange={(event) => setExternalPools('externalPoolUsageDebugDir')(event.target.value)}
                      />
                      <span className="block text-xs leading-4 text-muted-foreground">
                        写入容器内路径；记录失败只写服务日志，不影响请求。
                      </span>
                    </label>
                    <NumField
                      label="单条原始片段上限"
                      desc="限制原始请求/响应 body 与 SSE 前缀保存大小。"
                      value={draft.externalPools.externalPoolUsageDebugMaxBodyBytes}
                      min={0}
                      max={1024 * 1024}
                      suffix="Bytes"
                      disabled={!draft.externalPools.externalPoolUsageDebugEnabled}
                      onChange={setExternalPools('externalPoolUsageDebugMaxBodyBytes')}
                    />
                    <NumField
                      label="最多诊断文件"
                      desc="进程启动后最多写入的诊断 JSON 文件数；超出后跳过写入。"
                      value={draft.externalPools.externalPoolUsageDebugMaxFiles}
                      min={0}
                      max={100_000}
                      suffix="个"
                      disabled={!draft.externalPools.externalPoolUsageDebugEnabled}
                      onChange={setExternalPools('externalPoolUsageDebugMaxFiles')}
                    />
                  </div>
                </div>

                <div className="space-y-4">
                  <div className="grid gap-4 md:grid-cols-[minmax(16rem,22rem)_1fr]">
                    <div className="space-y-1.5">
                      <div className="text-sm font-semibold">本地池路由模式</div>
                      <Select
                        value={draft.externalPools.localPoolRouteMode}
                        onValueChange={(v) =>
                          setExternalPools('localPoolRouteMode')(
                            normalizeExternalPoolRouteMode(v),
                          )
                        }
                      >
                        <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                        <SelectContent>
                          <SelectItem value="allow_all">全部入口允许进入本地账号</SelectItem>
                          <SelectItem value="allow_list">只允许下列入口进入本地账号</SelectItem>
                          <SelectItem value="deny_list">禁止下列入口进入本地账号</SelectItem>
                        </SelectContent>
                      </Select>
                      <p className="text-xs leading-5 text-muted-foreground">
                        默认允许全部入口，保持当前行为；切换为允许列表或禁止列表后，右侧规则才会参与判断。
                      </p>
                    </div>

                    <div className="space-y-1.5">
                      <div className="text-sm font-semibold">本地池路由规则</div>
                      <Textarea
                        className="min-h-40 font-mono text-xs"
                        value={localRouteRulesText}
                        disabled={draft.externalPools.localPoolRouteMode === 'allow_all'}
                        placeholder={'/v1\n/cc\n/ha\n/na\n/dfcache/team'}
                        onChange={(e) => {
                          setLocalRouteRulesText(e.target.value)
                          setExternalPools('localPoolRouteRules')(parseRuleText(e.target.value))
                        }}
                      />
                      <p className="text-xs leading-5 text-muted-foreground">
                        每行一条，可填 /v1、/cc、/ha、/na、/dfcache/team，或完整 /cc/v1/messages。规则按大小写不敏感的精确或路径前缀匹配；* 表示全部入口。
                      </p>
                    </div>
                  </div>

                  <div className="grid gap-4 md:grid-cols-[minmax(16rem,22rem)_1fr]">
                    <div className="space-y-1.5">
                      <div className="text-sm font-semibold">外部池路由模式</div>
                      <Select
                        value={draft.externalPools.externalPoolRouteMode}
                        onValueChange={(v) =>
                          setExternalPools('externalPoolRouteMode')(
                            normalizeExternalPoolRouteMode(v),
                          )
                        }
                      >
                        <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                        <SelectContent>
                          <SelectItem value="allow_all">全部入口允许进入外部池</SelectItem>
                          <SelectItem value="allow_list">只允许下列入口进入外部池</SelectItem>
                          <SelectItem value="deny_list">禁止下列入口进入外部池</SelectItem>
                        </SelectContent>
                      </Select>
                      <p className="text-xs leading-5 text-muted-foreground">
                        默认允许全部入口，保持升级前行为；切换为允许列表或禁止列表后，右侧规则才会参与判断。
                      </p>
                    </div>

                    <div className="space-y-1.5">
                      <div className="text-sm font-semibold">外部池路由规则</div>
                      <Textarea
                        className="min-h-40 font-mono text-xs"
                        value={externalRouteRulesText}
                        disabled={draft.externalPools.externalPoolRouteMode === 'allow_all'}
                        placeholder={'/v1\n/cc\n/ha\n/na\n/dfcache/team'}
                        onChange={(e) => {
                          setExternalRouteRulesText(e.target.value)
                          setExternalPools('externalPoolRouteRules')(parseRuleText(e.target.value))
                        }}
                      />
                      <p className="text-xs leading-5 text-muted-foreground">
                        每行一条，可填 /v1、/cc、/ha、/na、/dfcache/team，或完整 /cc/v1/messages。规则按大小写不敏感的精确或路径前缀匹配；* 表示全部入口。
                      </p>
                    </div>
                  </div>
                </div>

                <TwoCol>
                  <NumField
                    label="外部池全局最大并发"
                    desc="所有外部池合计同时处理的请求上限；0 表示不限制。"
                    value={draft.externalPools.externalPoolGlobalMaxConcurrentRequests}
                    min={0}
                    max={100_000}
                    suffix="并发"
                    onChange={setExternalPools('externalPoolGlobalMaxConcurrentRequests')}
                  />
                  <NumField
                    label="外部池最大排队请求数"
                    desc="外部池并发占满时最多允许排队的请求数；0 表示不排队。"
                    value={draft.externalPools.externalPoolMaxQueuedRequests}
                    min={0}
                    max={100_000}
                    suffix="请求"
                    onChange={setExternalPools('externalPoolMaxQueuedRequests')}
                  />
                  <NumField
                    label="外部池最长排队等待"
                    desc="外部池等待并发名额的最长时间；0 会按后端安全默认值处理。"
                    value={draft.externalPools.externalPoolDispatchMaxWaitSecs}
                    min={0}
                    max={86_400}
                    suffix="秒"
                    onChange={setExternalPools('externalPoolDispatchMaxWaitSecs')}
                  />
                  <NumField
                    label="外部池最多故障转移"
                    desc="同一请求在多个外部池之间最多尝试多少次；0 表示不重试其它外部池。"
                    value={draft.externalPools.externalPoolRetryMaxAttempts}
                    min={0}
                    max={10_000}
                    suffix="次"
                    onChange={setExternalPools('externalPoolRetryMaxAttempts')}
                  />
                  <TogField
                    label="网络错误跨池重试"
                    desc="连接、DNS、超时等没有 HTTP 状态码的错误，是否允许切换其他外部池。"
                    checked={draft.externalPools.externalPoolRetryOnNetworkError}
                    onChange={setExternalPools('externalPoolRetryOnNetworkError')}
                  />
                  <TogField
                    label="协议错误跨池重试"
                    desc="上游返回成功状态码但内容是错误信封或协议污染时，是否允许切换其他外部池。"
                    checked={draft.externalPools.externalPoolRetryOnProtocolError}
                    onChange={setExternalPools('externalPoolRetryOnProtocolError')}
                  />
                  <label className="space-y-1.5 text-sm md:col-span-2">
                    <span className="text-muted-foreground">跨池重试状态码</span>
                    <Input
                      className="font-mono text-xs"
                      value={joinStatusCodeList(draft.externalPools.externalPoolRetryStatusCodes)}
                      onChange={(event) => setExternalPools('externalPoolRetryStatusCodes')(parseStatusCodeList(event.target.value))}
                    />
                    <span className="block text-xs leading-4 text-muted-foreground">
                      控制普通 HTTP 错误是否继续尝试其他外部池；认证、配额、渠道禁用等已分类错误仍按错误分类切换。
                    </span>
                  </label>
                  <NumField
                    label="同池重试次数"
                    desc="命中下面状态码时，先在同一个外部池上重试；重试耗尽后才冷却并尝试其他外部池。"
                    value={draft.externalPools.externalPoolSamePoolRetryCount}
                    min={0}
                    max={10}
                    suffix="次"
                    onChange={setExternalPools('externalPoolSamePoolRetryCount')}
                  />
                  <NumField
                    label="同池重试间隔"
                    desc="同一个外部池重试之间的等待时间。"
                    value={draft.externalPools.externalPoolSamePoolRetryDelayMs}
                    min={0}
                    max={60_000}
                    suffix="毫秒"
                    onChange={setExternalPools('externalPoolSamePoolRetryDelayMs')}
                  />
                  <NumField
                    label="失败池临时降权"
                    desc="外部池出现可重试瞬态失败后，在失败窗口内临时增加有效优先级；默认 20 可让优先级 1 的故障池让位给 10/20 的健康池，0 表示关闭。"
                    value={draft.externalPools.externalPoolTransientFailurePriorityPenalty}
                    min={0}
                    max={10_000}
                    suffix="优先级"
                    onChange={setExternalPools('externalPoolTransientFailurePriorityPenalty')}
                  />
                  <NumField
                    label="连续失败冷却阈值"
                    desc="同一外部池同一错误原因连续达到该次数后，才按对应冷却秒数临时避开；0 表示关闭。"
                    value={draft.externalPools.externalPoolTransientFailureCooldownThreshold}
                    min={0}
                    max={1000}
                    suffix="次"
                    onChange={setExternalPools('externalPoolTransientFailureCooldownThreshold')}
                  />
                  <label className="space-y-1.5 text-sm md:col-span-2">
                    <span className="text-muted-foreground">同池重试状态码</span>
                    <Input
                      className="font-mono text-xs"
                      value={joinStatusCodeList(draft.externalPools.externalPoolSamePoolRetryStatusCodes)}
                      onChange={(event) => setExternalPools('externalPoolSamePoolRetryStatusCodes')(parseStatusCodeList(event.target.value))}
                    />
                    <span className="block text-xs leading-4 text-muted-foreground">支持用逗号、空格或换行分隔；默认值用于 401、403、429、500、502、503、504。</span>
                  </label>
                  <NumField
                    label="外部池失败后本地等待"
                    desc="触发本地救援时，最多等待本地账号空闲多久。"
                    value={draft.externalPools.externalPoolLocalRescueMaxWaitSecs}
                    min={0}
                    max={300}
                    suffix="秒"
                    disabled={!draft.externalPools.externalPoolLocalRescueEnabled}
                    onChange={setExternalPools('externalPoolLocalRescueMaxWaitSecs')}
                  />
                  <NumField
                    label="外部池估算输入上限（兼容）"
                    desc="保留历史配置；当前不再用它做本地发送前拒绝，真实上下文超限以上游响应和请求大小保护为准。"
                    value={draft.externalPools.externalPoolMaxInputTokens}
                    min={0}
                    suffix="tokens"
                    onChange={setExternalPools('externalPoolMaxInputTokens')}
                  />
                </TwoCol>
              </div>
            )}

            {activeSection === 'cooldown' && (
              <TwoCol>
                <NumField label="默认暂停时间" desc="临时错误后暂停多久" value={draft.credentialTransientCooldownSecs} min={1} suffix="秒" onChange={set('credentialTransientCooldownSecs')} />
                <NumField label="限流后暂停" desc="收到 429 限流响应后的初始冷却，受最大冷却时长约束" value={draft.credentialRateLimitCooldownSecs} min={1} suffix="秒" onChange={set('credentialRateLimitCooldownSecs')} />
                <NumField label="服务繁忙后暂停" desc="收到 5xx 服务端错误后的初始冷却" value={draft.credentialServerErrorCooldownSecs} min={1} suffix="秒" onChange={set('credentialServerErrorCooldownSecs')} />
                <NumField label="网络错误基础冷却" desc="连接超时、DNS 失败等网络层错误后的冷却" value={draft.credentialNetworkErrorCooldownSecs} min={1} suffix="秒" onChange={set('credentialNetworkErrorCooldownSecs')} />
                <NumField label="流式中断后暂停" desc="流式响应中途中断后的冷却" value={draft.credentialStreamErrorCooldownSecs} min={1} suffix="秒" onChange={set('credentialStreamErrorCooldownSecs')} />
                <NumField label="格式异常后暂停" desc="响应格式/协议异常后的冷却" value={draft.credentialProtocolErrorCooldownSecs} min={1} suffix="秒" onChange={set('credentialProtocolErrorCooldownSecs')} />
                <NumField label="授权异常后暂停" desc="收到 401/403 授权失败后的冷却" value={draft.credentialAuthErrorCooldownSecs} min={1} suffix="秒" onChange={set('credentialAuthErrorCooldownSecs')} />
                <NumField label="最大冷却时长" desc="连续出错时最多暂停多久" value={draft.credentialMaxCooldownSecs} min={1} suffix="秒" onChange={set('credentialMaxCooldownSecs')} />
                <NumField label="退避倍率" desc="连续出错时逐步延长" value={draft.credentialCooldownBackoffMultiplier} min={1} max={10} step={0.1} suffix="倍" onChange={set('credentialCooldownBackoffMultiplier')} />
                <NumField label="恢复时间错开比例" desc="给恢复时间加一点随机错开，避免多个账号同时恢复后又同时出错。" value={draft.credentialCooldownJitterPercent} min={0} max={100} suffix="%" onChange={set('credentialCooldownJitterPercent')} />
                <NumField label="恢复观察时间" desc="账号刚恢复时先少量使用，稳定后再恢复正常调度。" value={draft.credentialProbationSecs} min={0} suffix="秒" onChange={set('credentialProbationSecs')} />
              </TwoCol>
            )}

            {activeSection === 'scheduler' && (
              <TwoCol>
                <NumField label="近期错误敏感度" desc="越高，刚发生的错误越快影响账号选择；不确定就保持默认。" value={draft.schedulerErrorEwmaAlpha} min={0.01} max={1} step={0.01} suffix="系数" onChange={set('schedulerErrorEwmaAlpha')} />
                <NumField label="优先级权重" desc="数值越大，账号优先级越能影响选择结果。" value={draft.schedulerPriorityWeight} min={0} step={0.1} suffix="权重" onChange={set('schedulerPriorityWeight')} />
                <NumField label="当前负载权重" desc="数值越大，当前更空闲的账号越容易被选中。" value={draft.schedulerLoadWeight} min={0} step={1} suffix="权重" onChange={set('schedulerLoadWeight')} />
                <NumField label="近期错误权重" desc="数值越大，最近出错多的账号越不容易被选中。" value={draft.schedulerErrorWeight} min={0} step={1} suffix="权重" onChange={set('schedulerErrorWeight')} />
                <NumField label="响应耗时权重" desc="数值越大，响应慢的账号越少被选中；通常设较小值" value={draft.schedulerLatencyWeight} min={0} step={0.001} suffix="权重" onChange={set('schedulerLatencyWeight')} />
                <NumField label="恢复期降权" desc="账号刚从错误中恢复时，数值越大越少被选中。" value={draft.schedulerProbationWeight} min={0} step={1} suffix="权重" onChange={set('schedulerProbationWeight')} />
                <NumField label="短时集中降权" desc="短时间内同一个账号被选得太多时，数值越大越快分散到其他账号。" value={draft.schedulerSelectionPressureWeight} min={0} step={1} suffix="权重" onChange={set('schedulerSelectionPressureWeight')} />
                <NumField label="长期使用次数权重" desc="数值越大，历史调度次数多的账号越少被选中，促进均衡" value={draft.schedulerTotalSelectionWeight} min={0} step={0.001} suffix="权重" onChange={set('schedulerTotalSelectionWeight')} />
                <NumField label="候选账号数量" desc="每次从前几个合适账号里挑选；数值越大越分散，但也会让选择更宽松。" value={draft.schedulerTopK} min={1} max={100} suffix="个" onChange={set('schedulerTopK')} />
                <NumField label="失败诊断样本数" desc="调度失败时最多记录多少个账号样本，用于后台排查；0 表示不记录样本。" value={draft.selectionFailureSampleLimit} min={0} max={1000} suffix="个" onChange={set('selectionFailureSampleLimit')} />
                <TogField label="记录失败样本" desc="关闭后只保留失败原因统计，不记录具体账号样本。" checked={draft.selectionFailureRecordEnabled} onChange={set('selectionFailureRecordEnabled')} />
              </TwoCol>
            )}

            {activeSection === 'warmup' && (
              <TwoCol>
                <NumField label="预热请求数" desc="0 表示不预热" value={draft.credentialWarmupRequests} min={0} suffix="次" onChange={set('credentialWarmupRequests')} />
                <NumField label="单个预热账号参与比例" desc="每次调度时该预热账号被选中的概率上限" value={draft.credentialWarmupSelectionPercent} min={0} max={100} suffix="%" onChange={set('credentialWarmupSelectionPercent')} />
                <NumField label="预热账号总占比上限" desc="所有预热中账号合计流量不超过此比例" value={draft.credentialWarmupMaxSelectionPercent} min={0} max={100} suffix="%" onChange={set('credentialWarmupMaxSelectionPercent')} />
              </TwoCol>
            )}

            {activeSection === 'payload' && (
              <div className="space-y-6">
                <div className="space-y-4">
                  <div>
                    <div className="text-sm font-semibold">请求大小保护</div>
                    <div className="mt-1 text-xs leading-5 text-muted-foreground">
                      负责发送前的大小判断、压缩和过大请求处理，和缓存策略分开配置。
                    </div>
                  </div>
                  <TwoCol>
                    <TogField label="启用请求压缩" desc="发送前尽量去掉冗余内容，减少请求体积。" checked={draft.compressionEnabled} onChange={set('compressionEnabled')} />
                    <TogField label="仅压缩空白字符" desc="只处理多余空格和换行，改动最小，风险较低。" checked={draft.whitespaceCompression} disabled={!draft.compressionEnabled} onChange={set('whitespaceCompression')} />
                    <TogField label="启用大小保护" desc="发送前检查请求大小，超过限制时按规则清理内容。" checked={draft.payloadGuardEnabled} onChange={set('payloadGuardEnabled')} />
                    <TogField label="外部账号也应用大小保护" desc="请求转发到外部账号前，也执行同样的大小保护。" checked={draft.payloadGuardExternalEnabled} disabled={!draft.payloadGuardEnabled} onChange={set('payloadGuardExternalEnabled')} />
                    <TogField label="优先裁剪旧历史" desc="内容太长时，优先缩短较早的对话历史，尽量保留当前请求" checked={draft.payloadGuardTrimHistory} disabled={!payloadSizeLimitEnabled} onChange={set('payloadGuardTrimHistory')} />
                  </TwoCol>
                  <div className="space-y-1.5">
                    <div className="text-sm font-semibold">过大请求处理方式</div>
                    <Select
                      value={payloadGuardMode}
                      disabled={!draft.payloadGuardEnabled}
                      onValueChange={(v) => set('payloadGuardMode')(v as PayloadGuardMode)}
                    >
                      <SelectTrigger size="sm" className="w-72"><SelectValue /></SelectTrigger>
                      <SelectContent>
                        <SelectItem value="preemptive">发送前先处理</SelectItem>
                        <SelectItem value="on_too_long">失败后再处理并重试</SelectItem>
                      </SelectContent>
                    </Select>
                    <div className="text-xs leading-5 text-muted-foreground">
                      发送前先处理更稳；失败后再处理会尽量保留原文，但可能多一次重试。
                    </div>
                  </div>
                  <TwoCol>
                    <NumField label="请求大小阈值" desc="超过此大小才触发处理（如 1048576 = 1 MB）；0 表示不按大小处理" value={draft.payloadGuardMaxBytes} min={0} suffix="字节" onChange={set('payloadGuardMaxBytes')} />
                    <NumField label="安全余量" desc="处理目标比阈值小出的缓冲（如 65536 = 64 KB），避免裁剪后仍超限" value={draft.payloadGuardSafetyMarginBytes} min={0} suffix="字节" disabled={!payloadSizeLimitEnabled} onChange={set('payloadGuardSafetyMarginBytes')} />
                  </TwoCol>
                </div>
                <div className="space-y-3">
                  <div>
                    <div className="text-sm font-semibold">图片处理</div>
                    <div className="mt-1 text-xs leading-5 text-muted-foreground">
                      控制图片、文件和远程资源在发送上游前是否由本地展开或修正。
                    </div>
                  </div>
                  <TwoCol>
                    <div className="space-y-1.5">
                      <div className="text-sm font-semibold">处理模式</div>
                      <Select
                        value={imageProcessingMode}
                        onValueChange={(v) => setImageProcessing('mode')(v as RuntimeConfig['imageProcessing']['mode'])}
                      >
                        <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                        <SelectContent>
                          <SelectItem value="safe">Safe：兼容修复</SelectItem>
                          <SelectItem value="light">Light：轻量透传</SelectItem>
                        </SelectContent>
                      </Select>
                      <div className="text-xs leading-5 text-muted-foreground">
                        {imageProcessingMode === 'light'
                          ? '不展开 file_id，不下载远程 URL，不解码修正 base64 媒体类型；只接受 inline base64 或 data URL。'
                          : '保持现有兼容行为，可按下面开关展开文件、下载远程资源和修正图片 media_type。'}
                      </div>
                    </div>
                    <TogField
                      label="展开本地文件 source"
                      desc="把已上传文件引用展开为可发送给 Kiro 的 inline 内容。"
                      checked={Boolean(draft.imageProcessing?.safeMaterializeFileSources)}
                      disabled={imageProcessingMode !== 'safe'}
                      onChange={setImageProcessing('safeMaterializeFileSources')}
                    />
                    <TogField
                      label="下载远程图片和文档"
                      desc="把请求里的远程 URL 下载后转成 inline 内容，便于上游识别。"
                      checked={Boolean(draft.imageProcessing?.safeDownloadRemoteSources)}
                      disabled={imageProcessingMode !== 'safe'}
                      onChange={setImageProcessing('safeDownloadRemoteSources')}
                    />
                    <TogField
                      label="修正 base64 图片类型"
                      desc="根据图片字节修正错误的 image/png、image/jpeg 等 media_type。"
                      checked={Boolean(draft.imageProcessing?.safeNormalizeBase64MediaTypes)}
                      disabled={imageProcessingMode !== 'safe'}
                      onChange={setImageProcessing('safeNormalizeBase64MediaTypes')}
                    />
                  </TwoCol>
                </div>
                <div className="space-y-3">
                  <div>
                    <div className="text-sm font-semibold">提示词引导</div>
                    <div className="mt-1 text-xs leading-5 text-muted-foreground">
                      管理代理新增的语言、任务质量、tool_choice、thinking 与分块兼容提示；关闭总开关后不新增任何这类提示，客户端原始结构化字段仍保留。
                    </div>
                  </div>
                  <TwoCol>
                    <TogField
                      label="启用提示词引导"
                      desc="总开关。关闭后不会注入语言约束、任务质量、tool_choice、thinking 或分块写入提示；客户端已提供的结构化字段仍按原语义处理。"
                      checked={draft.promptSteering.enabled}
                      onChange={setPromptSteering('enabled')}
                    />
                    <div className="space-y-1.5">
                      <div className="text-sm font-semibold">生效范围</div>
                      <Select value={draft.promptSteering.scope} onValueChange={(v) => setPromptSteering('scope')(v as RuntimeConfig['promptSteering']['scope'])}>
                        <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                        <SelectContent>
                          <SelectItem value="route_rules">按路径规则</SelectItem>
                          <SelectItem value="claude_code_profile">Claude Code / Debug profile</SelectItem>
                          <SelectItem value="all_routes">全部 messages 路由</SelectItem>
                        </SelectContent>
                      </Select>
                      <div className="text-xs leading-5 text-muted-foreground">anthropic-strict profile 始终不注入 synthetic prompt。</div>
                    </div>
                    <div className="space-y-1.5">
                      <div className="text-sm font-semibold">提示词路径模式</div>
                      <Select
                        value={draft.promptSteering.routeMode}
                        disabled={draft.promptSteering.scope !== 'route_rules'}
                        onValueChange={(v) => setPromptSteering('routeMode')(v as RuntimeConfig['promptSteering']['routeMode'])}
                      >
                        <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                        <SelectContent>
                          <SelectItem value="allow_list">只对规则命中的入口生效</SelectItem>
                          <SelectItem value="deny_list">对规则外的入口生效</SelectItem>
                          <SelectItem value="allow_all">全部入口生效</SelectItem>
                        </SelectContent>
                      </Select>
                      <div className="text-xs leading-5 text-muted-foreground">仅在“按路径规则”范围下参与判断。</div>
                    </div>
                    <div className="space-y-1.5 md:col-span-2">
                      <div className="text-sm font-semibold">提示词路径规则</div>
                      <Textarea
                        className="min-h-28 font-mono text-xs"
                        value={promptSteeringRouteRulesText}
                        disabled={draft.promptSteering.scope !== 'route_rules' || draft.promptSteering.routeMode === 'allow_all'}
                        placeholder={'/cc\n/v1\n/ha\n/na\n/dfcache/team'}
                        onChange={(e) => {
                          setPromptSteeringRouteRulesText(e.target.value)
                          setPromptSteering('routeRules')(parseRuleText(e.target.value))
                        }}
                      />
                      <div className="text-xs leading-5 text-muted-foreground">
                        每行一条，可填入口前缀或完整 messages / count_tokens 路径；默认规则是 /cc，但可以改成任意内置或自定义路由。
                      </div>
                    </div>
                    <TogField
                      label="应用到外部池"
                      desc="开启后，请求进入外部池 raw passthrough 时也按同一提示词路径规则处理增强后的 system。"
                      checked={draft.promptSteering.applyToExternalPool}
                      onChange={setPromptSteering('applyToExternalPool')}
                    />
                    <TogField
                      label="count_tokens 同步计入"
                      desc="count_tokens 使用同一提示词路径规则，避免估算低于真实请求。"
                      checked={draft.promptSteering.applyToCountTokens}
                      onChange={setPromptSteering('applyToCountTokens')}
                    />
                    <TogField
                      label="语言约束"
                      desc="减少“让me / let我 / 我will / 日语葡语串台”这类非自然语言拼接；不禁止正常技术英文。"
                      checked={draft.promptSteering.languageConstraint.enabled}
                      onChange={setPromptSteeringText('languageConstraint', 'enabled')}
                    />
                    <TogField
                      label="任务质量"
                      desc="强调最新用户消息、仅分析/真实执行/发布等任务边界，以及已验证必须有证据。"
                      checked={draft.promptSteering.taskQuality.enabled}
                      onChange={setPromptSteeringText('taskQuality', 'enabled')}
                    />
                    <TogField
                      label="tool_choice 引导"
                      desc="控制本地 Kiro 的 tool_choice 兼容提示；总开关关闭时不注入提示，但结构化 0/N/1 工具过滤仍按请求执行。"
                      checked={draft.promptSteering.toolChoice.enabled}
                      onChange={setPromptSteeringToggle('toolChoice')}
                    />
                    <TogField
                      label="thinking 提示控制"
                      desc="控制 synthetic thinking 兼容提示；总开关关闭时不注入提示，客户端显式 thinking 仍保留。"
                      checked={draft.promptSteering.thinking.enabled}
                      onChange={setPromptSteeringToggle('thinking')}
                    />
                    <TogField
                      label="分块写入提示"
                      desc="控制 Write/Edit 分块兼容提示及其两个提示位置；总开关关闭时不注入这些提示。"
                      checked={draft.promptSteering.chunkedWrite.enabled}
                      onChange={setChunkedWriteSteering('enabled')}
                    />
                    <TogField
                      label="分块 system 提示"
                      desc="在 system 中要求模型遵守 Write/Edit 分块限制。"
                      checked={draft.promptSteering.chunkedWrite.systemPromptEnabled}
                      disabled={!draft.promptSteering.chunkedWrite.enabled}
                      onChange={setChunkedWriteSteering('systemPromptEnabled')}
                    />
                    <TogField
                      label="分块工具描述"
                      desc="给 Write/Edit 工具 description 追加分块限制说明。"
                      checked={draft.promptSteering.chunkedWrite.toolDescriptionEnabled}
                      disabled={!draft.promptSteering.chunkedWrite.enabled}
                      onChange={setChunkedWriteSteering('toolDescriptionEnabled')}
                    />
                    <TogField
                      label="自定义追加提示词"
                      desc="追加 operator 自定义 system prompt；默认关闭。"
                      checked={draft.promptSteering.custom.enabled}
                      onChange={setPromptSteeringText('custom', 'enabled')}
                    />
                  </TwoCol>
                  <div className="grid gap-3">
                    <div className="rounded-md border bg-background p-4">
                      <div className="mb-3 flex items-center justify-between gap-3">
                        <div>
                          <div className="text-sm font-medium">语言约束提示词</div>
                          <div className="mt-1 text-xs leading-5 text-muted-foreground">目标是减少非自然语言的跨语言语法拼接，不是禁止正常技术英文。</div>
                        </div>
                        <Button type="button" variant="outline" size="sm" onClick={() => setPromptSteeringText('languageConstraint', 'prompt')(defaultPromptSteering().languageConstraint.prompt)}>
                          恢复默认
                        </Button>
                      </div>
                      <Textarea
                        className="min-h-48 font-mono text-xs"
                        value={draft.promptSteering.languageConstraint.prompt}
                        onChange={(event) => setPromptSteeringText('languageConstraint', 'prompt')(event.target.value)}
                      />
                    </div>
                    <div className="rounded-md border bg-background p-4">
                      <div className="mb-3 flex items-center justify-between gap-3">
                        <div>
                          <div className="text-sm font-medium">任务质量提示词</div>
                          <div className="mt-1 text-xs leading-5 text-muted-foreground">用于减少追问被忽视、任务边界错误、没有真实证据却声称完成等问题。</div>
                        </div>
                        <Button type="button" variant="outline" size="sm" onClick={() => setPromptSteeringText('taskQuality', 'prompt')(defaultPromptSteering().taskQuality.prompt)}>
                          恢复默认
                        </Button>
                      </div>
                      <Textarea
                        className="min-h-48 font-mono text-xs"
                        value={draft.promptSteering.taskQuality.prompt}
                        onChange={(event) => setPromptSteeringText('taskQuality', 'prompt')(event.target.value)}
                      />
                    </div>
                    <div className="rounded-md border bg-background p-4">
                      <div className="mb-3">
                        <div className="text-sm font-medium">自定义追加提示词</div>
                        <div className="mt-1 text-xs leading-5 text-muted-foreground">仅在上方“自定义追加提示词”开启时注入；不要写动态 request id、时间或账号信息。</div>
                      </div>
                      <Textarea
                        className="min-h-32 font-mono text-xs"
                        value={draft.promptSteering.custom.prompt}
                        onChange={(event) => setPromptSteeringText('custom', 'prompt')(event.target.value)}
                      />
                    </div>
                  </div>
                </div>
                <div className="space-y-3">
                  <div>
                    <div className="text-sm font-semibold">本地协议转换</div>
                    <div className="mt-1 text-xs leading-5 text-muted-foreground">
                      这些开关会改变本地凭据路径最终发往 Kiro 的请求体；外部池 raw body 透传不会进入这些阶段。
                    </div>
                  </div>
                  <TwoCol>
                    <TogField label="工具 schema 规范化" desc="清理 OpenAPI、Zod、MCP 等工具 schema 中上游容易拒绝的字段。" checked={draft.bodyConversion.toolSchemaNormalization} onChange={setBodyConversion('toolSchemaNormalization')} />
                    <TogField label="工具名映射" desc="清洗或缩短不符合 Kiro 工具名约束的名称，并记录响应反向映射。" checked={draft.bodyConversion.toolNameMapping} onChange={setBodyConversion('toolNameMapping')} />
                    <div className="rounded-md border bg-background p-4">
                      <div className="mb-3">
                        <div className="text-sm font-medium">schema key 映射</div>
                        <div className="mt-1 text-xs leading-5 text-muted-foreground">sanitize 只清洗不符合正则的 property key 并在响应中映射回原 key；reject 明确拒绝；disabled 保持旧行为。</div>
                      </div>
                      <Select value={draft.bodyConversion.toolSchemaKeyMapping} onValueChange={(v) => setBodyConversion('toolSchemaKeyMapping')(v as RuntimeConfig['bodyConversion']['toolSchemaKeyMapping'])}>
                        <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                        <SelectContent>
                          <SelectItem value="sanitize">sanitize：清洗并反向映射</SelectItem>
                          <SelectItem value="reject">reject：非法 key 明确报错</SelectItem>
                          <SelectItem value="disabled">disabled：不处理</SelectItem>
                        </SelectContent>
                      </Select>
                    </div>
                    <div className="rounded-md border bg-background p-4">
                      <div className="mb-3">
                        <div className="text-sm font-medium">schema key 合法性正则</div>
                        <div className="mt-1 text-xs leading-5 text-muted-foreground">仅 schema key 映射为 sanitize/reject 时使用。默认来自问题分析文档。</div>
                      </div>
                      <Input
                        className="font-mono text-xs"
                        value={draft.bodyConversion.toolSchemaKeyValidationRegex}
                        onChange={(event) => setBodyConversion('toolSchemaKeyValidationRegex')(event.target.value)}
                      />
                    </div>
                    <TogField label="原生 reasoning 字段" desc="对支持的 Kiro 模型上报 additionalModelRequestFields。" checked={draft.bodyConversion.nativeReasoningFields} onChange={setBodyConversion('nativeReasoningFields')} />
                    <TogField label="结构化 tool_choice" desc="按请求语义执行 none=0、any=N、named=1 工具过滤；提示词总开关只控制额外提示，不删除结构化语义。" checked={draft.bodyConversion.toolChoiceSteering} onChange={setBodyConversion('toolChoiceSteering')} />
                    <TogField label="thinking 转换能力" desc="允许本地 Kiro 生成原生 thinking 字段；客户端显式字段仍按能力合同映射，额外兼容提示受上方总开关控制。" checked={draft.bodyConversion.thinkingPromptControls} onChange={setBodyConversion('thinkingPromptControls')} />
                    <TogField label="分块工具策略" desc="定义 Write/Edit 分块协议能力；额外 system/工具描述提示受上方总开关控制。" checked={draft.bodyConversion.chunkedToolPolicy} onChange={setBodyConversion('chunkedToolPolicy')} />
                    <TogField label="工具配对修复" desc="清理不严格配对、重复或孤立的 tool_use/tool_result；不会把被拒绝的结果原文转成普通文本。" checked={draft.bodyConversion.toolPairingRepair} onChange={setBodyConversion('toolPairingRepair')} />
                    <TogField label="历史工具占位" desc="历史里出现但当前 tools 缺失时补充占位工具定义。" checked={draft.bodyConversion.historyPlaceholderTools} onChange={setBodyConversion('historyPlaceholderTools')} />
                  </TwoCol>
                </div>
                <div className="space-y-3">
                  <div>
                    <div className="text-sm font-semibold">历史消息清理</div>
                    <div className="mt-1 text-xs leading-5 text-muted-foreground">
                      负责历史消息和旧工具结果的体积优化。
                    </div>
                  </div>
                  <PayloadHistorySection shaping={draft.payloadShaping} payloadSizeLimitEnabled={payloadSizeLimitEnabled} onChange={set('payloadShaping')} />
                </div>
                <div className="space-y-3">
                  <div>
                    <div className="text-sm font-semibold">当前请求兜底</div>
                    <div className="mt-1 text-xs leading-5 text-muted-foreground">
                      当前请求仍然超过限制时，按这里的规则处理当前内容。
                    </div>
                  </div>
                  <PayloadFallbackSection shaping={draft.payloadShaping} payloadShapingBranchEnabled={payloadSizeLimitEnabled && draft.payloadShaping.enabled} onChange={set('payloadShaping')} />
                </div>
              </div>
            )}

            {activeSection === 'cachePolicy' && (
              <CachePolicySettingsSection config={draft} onChange={setDraft} />
            )}

            {activeSection === 'modelMapping' && (
              <div className="space-y-6">
                <div className="space-y-3">
                  <div>
                    <div className="text-sm font-semibold">模型解析策略</div>
                    <div className="mt-1 text-xs leading-5 text-muted-foreground">
                      控制请求模型名在发送上游前如何匹配，不改变响应格式，也不改变账号调度规则。
                    </div>
                  </div>
                  <div className="space-y-1.5">
                    <Select value={draft.modelResolutionMode} onValueChange={(v) => set('modelResolutionMode')(v as RuntimeConfig['modelResolutionMode'])}>
                      <SelectTrigger size="sm" className="w-72"><SelectValue /></SelectTrigger>
                      <SelectContent>
                        <SelectItem value="compatible">默认兼容解析</SelectItem>
                        <SelectItem value="alias_only">仅精确与显式别名</SelectItem>
                        <SelectItem value="exact_only">仅完整模型名</SelectItem>
                      </SelectContent>
                    </Select>
                    <p className="text-xs leading-5 text-muted-foreground">
                      {draft.modelResolutionMode === 'compatible'
                        ? '允许常见别名和版本写法，兼容性最好。'
                        : draft.modelResolutionMode === 'alias_only'
                          ? '只接受完整模型名和明确配置过的别名。'
                          : '只接受完整模型名，最严格。'}
                    </p>
                  </div>
                </div>
                <ModelMappingSection
                  mapping={draft.modelMapping}
                  capabilities={modelCapabilities.data}
                  onChange={(m: ModelMappingConfig) => set('modelMapping')(m)}
                />
              </div>
            )}

            {activeSection === 'startupProxy' && (
              <StartupProxySection config={draft} />
            )}

            {activeSection === 'compat' && (
              <div className="space-y-4">
                <div className="grid gap-4 md:grid-cols-2">
                  <div className="space-y-1.5">
                    <div className="text-sm font-semibold">兼容模式</div>
                    <Select value={draft.compatProfile} onValueChange={(v) => set('compatProfile')(v as RuntimeConfig['compatProfile'])}>
                      <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                      <SelectContent>
                        <SelectItem value="claude-code">Claude Code 兼容</SelectItem>
                        <SelectItem value="anthropic-strict">Anthropic 严格模式</SelectItem>
                        <SelectItem value="debug">调试模式</SelectItem>
                      </SelectContent>
                    </Select>
                    <p className="text-xs leading-5 text-muted-foreground">
                      {draft.compatProfile === 'claude-code'
                        ? '面向 Claude Code CLI，尽量按它期望的格式返回。'
                        : draft.compatProfile === 'anthropic-strict'
                          ? '尽量保持 Anthropic 原始接口格式，少做兼容修正。'
                          : '保留更多排查信息，适合本地调试，不建议用于正式流量。'}
                    </p>
                  </div>
                  <div className="space-y-1.5">
                    <div className="text-sm font-semibold">Kiro 工作模式</div>
                    <Select value={draft.kiroAgentModeStrategy} onValueChange={(v) => set('kiroAgentModeStrategy')(v as KiroAgentModeStrategy)}>
                      <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                      <SelectContent>
                        <SelectItem value="vibe">Vibe</SelectItem>
                        <SelectItem value="spec">Spec</SelectItem>
                        <SelectItem value="auto">自动</SelectItem>
                      </SelectContent>
                    </Select>
                    <p className="text-xs leading-5 text-muted-foreground">
                      {draft.kiroAgentModeStrategy === 'auto'
                        ? '根据请求内容自动选择工作方式。'
                        : draft.kiroAgentModeStrategy === 'spec'
                          ? '偏向规格化流程，适合明确需求和分步骤实现。'
                          : '偏向自由对话流程，适合快速探索和直接执行。'}
                    </p>
                  </div>
                  <div className="space-y-1.5">
                    <div className="text-sm font-semibold">思考触发</div>
                    <Select value={draft.thinkingTriggerMode} onValueChange={(v) => set('thinkingTriggerMode')(v as RuntimeConfig['thinkingTriggerMode'])}>
                      <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                      <SelectContent>
                        <SelectItem value="real_request">按请求触发</SelectItem>
                        <SelectItem value="always">总是触发</SelectItem>
                      </SelectContent>
                    </Select>
                    <p className="text-xs text-muted-foreground">
                      {draft.thinkingTriggerMode === 'always'
                        ? '除非请求明确关闭，否则每次调用都会输出思考内容。'
                        : '按 Claude Code CLI 的习惯触发：模型或请求明确需要深度思考时，才会输出思考内容。'}
                    </p>
                  </div>
                  <div className="space-y-1.5">
                    <div className="text-sm font-semibold">缺失 max_tokens</div>
                    <Select
                      value={draft.missingMaxTokens.policy}
                      onValueChange={(v) =>
                        setMissingMaxTokens('policy')(v as RuntimeConfig['missingMaxTokens']['policy'])
                      }
                    >
                      <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                      <SelectContent>
                        <SelectItem value="default_value">自动补全</SelectItem>
                        <SelectItem value="reject">直接拒绝</SelectItem>
                      </SelectContent>
                    </Select>
                    <p className="text-xs leading-5 text-muted-foreground">
                      自动补全只处理顶层缺少 max_tokens 的 Messages 请求；无效 JSON 和空模型仍会拒绝并记录到用量日志。
                    </p>
                  </div>
                  <NumField
                    label="max_tokens 补充值"
                    desc="自动补全时写入的输出上限；默认 20480，避免补 0 或过大值改变客户端语义。"
                    value={draft.missingMaxTokens.defaultValue}
                    min={1}
                    max={200000}
                    suffix="tokens"
                    disabled={draft.missingMaxTokens.policy === 'reject'}
                    onChange={setMissingMaxTokens('defaultValue')}
                  />
                  <div className="space-y-1.5">
                    <div className="text-sm font-semibold">外部池默认流式 SSE 转发</div>
                    <Select
                      value={draft.externalPools.externalPoolStreamResponseMode}
                      onValueChange={(v) =>
                        setExternalPools('externalPoolStreamResponseMode')(
                          v as RuntimeConfig['externalPools']['externalPoolStreamResponseMode'],
                        )
                      }
                    >
                      <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
                      <SelectContent>
                        <SelectItem value="event_passthrough">SSE 事件级透传</SelectItem>
                      </SelectContent>
                    </Select>
                    <p className="text-xs leading-5 text-muted-foreground">
                      作为外部池默认值；单个外部账号可以覆盖。只决定 stream=true 的 SSE 事件转发方式；usage 是透传上游还是按入口路径整理，由外部账号的“下游 usage 口径”决定。
                    </p>
                  </div>
                </div>
                <TwoCol>
                  <TogField label="整理思考内容" desc="把响应里的思考内容单独整理出来，方便客户端按固定格式展示。" checked={draft.extractThinking} onChange={set('extractThinking')} />
                  <TogField label="显示处理告警" desc="把代理处理中的提醒返回给客户端，方便排查问题。" checked={draft.exposeProxyWarnings} onChange={set('exposeProxyWarnings')} />
                </TwoCol>
              </div>
            )}
          </div>
        </section>
      </div>

      {/* 底部操作栏 */}
      <div className="flex items-center justify-between rounded-xl bg-muted/30 px-4 py-3">
        <span className="text-xs text-muted-foreground">保存后，新的请求会立即使用这些配置。</span>
        <Button size="sm" onClick={save} disabled={updateConfig.isPending}>
          {updateConfig.isPending ? <Spinner size="sm" /> : <Save className="h-4 w-4" />}
          保存配置
        </Button>
      </div>
    </PageContainer>
  )
}
