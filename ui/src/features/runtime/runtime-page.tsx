import type { ReactNode } from 'react'
import { useEffect, useState } from 'react'
import {
  Gauge,
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
} from '@/components/ui'
import { extractErrorMessage } from '@/lib/utils'
import {
  defaultPayloadShaping,
  defaultPromptCacheCreationControl,
  defaultReportedUsage,
  emptyRuntimeConfig,
  normalizeCachePolicy,
  normalizePromptCacheCreationControl,
  normalizeReportedUsage,
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
  normalizeDefinedCacheRoutes,
  ModelMappingSection,
  PayloadFallbackSection,
  PayloadHistorySection,
} from './runtime-sections'

type RuntimeSectionKey =
  | 'loadBalancing'
  | 'capacity'
  | 'cooldown'
  | 'scheduler'
  | 'warmup'
  | 'payload'
  | 'cachePolicy'
  | 'modelMapping'
  | 'compat'

const runtimeSections: Array<{
  key: RuntimeSectionKey
  title: string
  desc: string
  icon: ReactNode
}> = [
  { key: 'loadBalancing', title: '负载均衡模式', desc: '请求分配给账号的策略', icon: <Gauge className="h-4 w-4" /> },
  { key: 'capacity', title: '请求容量', desc: '并发、排队、重试、超时', icon: <Gauge className="h-4 w-4" /> },
  { key: 'cooldown', title: '错误恢复 / 冷却', desc: '不同错误类型的暂停策略与退避', icon: <Shield className="h-4 w-4" /> },
  { key: 'scheduler', title: '账号选择权重', desc: '优先使用哪些账号的调度参数', icon: <Gauge className="h-4 w-4" /> },
  { key: 'warmup', title: '新账号预热', desc: '新账号逐步参与请求，稳定后恢复正常', icon: <Sparkles className="h-4 w-4" /> },
  { key: 'payload', title: '请求体处理', desc: '请求大小保护、历史清理和当前请求兜底', icon: <Wand2 className="h-4 w-4" /> },
  { key: 'cachePolicy', title: '缓存策略', desc: '策略模板默认参数和路径绑定', icon: <Zap className="h-4 w-4" /> },
  { key: 'modelMapping', title: '模型映射', desc: '客户端模型名到实际模型的映射规则', icon: <Shield className="h-4 w-4" /> },
  { key: 'compat', title: '兼容行为', desc: '接口兼容模式、模型解析和思考内容行为', icon: <Shield className="h-4 w-4" /> },
]

// ─── 原子组件 ──────────────────────────────────────────────────────────────────

function numberValue(value: string, fallback: number): number {
  const parsed = Number(value)
  return Number.isFinite(parsed) ? parsed : fallback
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

// ─── normalizeConfig ──────────────────────────────────────────────────────────

function normalizeConfig(draft: RuntimeConfig): RuntimeConfig {
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
    reportedUsage: normalizeReportedUsage(draft.reportedUsage),
    cachePolicy: normalizeCachePolicy(draft.cachePolicy),
    definedCacheRoutes: normalizeDefinedCacheRoutes(draft.definedCacheRoutes),
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

  useEffect(() => {
    if (config.data) {
      setDraft({
        ...emptyRuntimeConfig,
        ...config.data,
        payloadShaping: { ...defaultPayloadShaping(), ...config.data.payloadShaping },
        promptCacheCreationControl: { ...defaultPromptCacheCreationControl(), ...config.data.promptCacheCreationControl },
        reportedUsage: config.data.reportedUsage ?? defaultReportedUsage(),
        cachePolicy: normalizeCachePolicy(config.data.cachePolicy),
      })
    }
  }, [config.data])

  const set = <K extends keyof RuntimeConfig>(k: K) => (v: RuntimeConfig[K]) =>
    setDraft((prev) => ({ ...prev, [k]: v }))

  const save = () => {
    const next = normalizeConfig(draft)
    if (next.promptCacheCapJitterMinTokens > next.promptCacheCapJitterMaxTokens)
      return toast.error('触顶扣减下限不能大于上限')
    if (next.payloadGuardMaxBytes > 0 && next.payloadGuardMaxBytes < 65536)
      return toast.error('处理阈值必须为 0 或不小于 65536 字节')
    if (next.payloadGuardMaxBytes - next.payloadGuardSafetyMarginBytes < 65536 && next.payloadGuardMaxBytes > 0)
      return toast.error('安全余量不能过大,处理阈值减去安全余量需不小于 65536 字节')
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
                <NumField label="单账号每分钟请求上限" desc="每个账号一分钟最多接多少个请求；0 表示不做本地限速。" value={draft.credentialRpm} min={0} suffix="次/分钟" onChange={set('credentialRpm')} />
                <NumField label="单账号最大并发" desc="每个账号同一时间最多处理多少个请求；0 表示不限制。" value={draft.credentialMaxConcurrentRequests} min={0} suffix="并发" onChange={set('credentialMaxConcurrentRequests')} />
                <NumField label="全局最大并发" desc="整个服务同一时间最多处理多少个请求；0 表示不限制。" value={draft.dispatchGlobalMaxConcurrentRequests} min={0} suffix="并发" onChange={set('dispatchGlobalMaxConcurrentRequests')} />
                <NumField label="最大排队请求数" desc="账号忙不过来时最多让多少个请求排队；0 表示不限制。" value={draft.dispatchMaxQueuedRequests} min={0} suffix="请求" onChange={set('dispatchMaxQueuedRequests')} />
                <NumField label="单请求最长排队等待" desc="一个请求最多等账号空闲多久；0 表示不限制。" value={draft.credentialDispatchMaxWaitSecs} min={0} suffix="秒" onChange={set('credentialDispatchMaxWaitSecs')} />
                <NumField label="开始响应等待时间" desc="发给上游后，多久还没开始返回就认为超时；0 表示使用默认超时。" value={draft.kiroUpstreamResponseTimeoutSecs} min={0} suffix="秒" onChange={set('kiroUpstreamResponseTimeoutSecs')} />
                <NumField label="流式静默超时" desc="流式响应长时间没有新内容时，结束本次请求。" value={draft.kiroUpstreamStreamIdleTimeoutSecs} min={0} suffix="秒" onChange={set('kiroUpstreamStreamIdleTimeoutSecs')} />
                <NumField label="单请求最大重试次数" desc="一个请求失败后最多重试几次；0 表示系统自动决定。" value={draft.credentialRetryMaxAttempts} min={0} suffix="次" onChange={set('credentialRetryMaxAttempts')} />
                <NumField label="异常并发自动回收" desc="请求长时间没有结束时自动释放占用，避免账号并发数被卡住；0 表示关闭。" value={draft.credentialInFlightLeaseMaxSecs} min={0} suffix="秒" onChange={set('credentialInFlightLeaseMaxSecs')} />
              </TwoCol>
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
              <ModelMappingSection
                mapping={draft.modelMapping}
                capabilities={modelCapabilities.data}
                onChange={(m: ModelMappingConfig) => set('modelMapping')(m)}
              />
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
                    <div className="text-sm font-semibold">模型解析策略</div>
                    <Select value={draft.modelResolutionMode} onValueChange={(v) => set('modelResolutionMode')(v as RuntimeConfig['modelResolutionMode'])}>
                      <SelectTrigger size="sm"><SelectValue /></SelectTrigger>
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
