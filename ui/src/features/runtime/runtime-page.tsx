import { useEffect, useState } from 'react'
import {
  ChevronDown,
  ChevronRight,
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
import { cn } from '@/lib/utils'
import { extractErrorMessage } from '@/lib/utils'
import {
  defaultPayloadShaping,
  defaultPromptCacheCreationControl,
  defaultReportedUsage,
  emptyRuntimeConfig,
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
  CacheCreationSection,
  DefinedCacheRoutesSection,
  normalizeDefinedCacheRoutes,
  ModelMappingSection,
  PayloadFallbackSection,
  PayloadHistorySection,
  ReportedUsageSection,
} from './runtime-sections'

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

function TwoCol({ children }: { children: React.ReactNode }) {
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

  useEffect(() => {
    if (config.data) {
      setDraft({
        ...emptyRuntimeConfig,
        ...config.data,
        payloadShaping: { ...defaultPayloadShaping(), ...config.data.payloadShaping },
        promptCacheCreationControl: { ...defaultPromptCacheCreationControl(), ...config.data.promptCacheCreationControl },
        reportedUsage: config.data.reportedUsage ?? defaultReportedUsage(),
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

      {/* 负载均衡模式 — 高优先级，展开显示 */}
      <CollapseSection icon={<Gauge />} title="负载均衡模式" desc="控制请求分配给账号的策略" defaultOpen>
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
      </CollapseSection>

      {/* 请求容量 */}
      <CollapseSection icon={<Gauge />} title="请求容量" desc="并发、排队、重试、超时" defaultOpen>
        <TwoCol>
          <NumField label="单账号每分钟请求上限" desc="0 表示关闭本地限速" value={draft.credentialRpm} min={0} suffix="次/分钟" onChange={set('credentialRpm')} />
          <NumField label="单账号最大并发" desc="0 表示不限制" value={draft.credentialMaxConcurrentRequests} min={0} suffix="并发" onChange={set('credentialMaxConcurrentRequests')} />
          <NumField label="全局最大并发" desc="0 表示不限制" value={draft.dispatchGlobalMaxConcurrentRequests} min={0} suffix="并发" onChange={set('dispatchGlobalMaxConcurrentRequests')} />
          <NumField label="最大排队请求数" desc="0 表示不限制" value={draft.dispatchMaxQueuedRequests} min={0} suffix="请求" onChange={set('dispatchMaxQueuedRequests')} />
          <NumField label="单请求最长排队等待" desc="0 表示不限制" value={draft.credentialDispatchMaxWaitSecs} min={0} suffix="秒" onChange={set('credentialDispatchMaxWaitSecs')} />
          <NumField label="开始响应等待时间" desc="0 表示使用默认超时" value={draft.kiroUpstreamResponseTimeoutSecs} min={0} suffix="秒" onChange={set('kiroUpstreamResponseTimeoutSecs')} />
          <NumField label="流式静默超时" desc="流式响应长时间没有新内容时结束本次请求" value={draft.kiroUpstreamStreamIdleTimeoutSecs} min={0} suffix="秒" onChange={set('kiroUpstreamStreamIdleTimeoutSecs')} />
          <NumField label="单请求最大重试次数" desc="0 表示系统自动决定" value={draft.credentialRetryMaxAttempts} min={0} suffix="次" onChange={set('credentialRetryMaxAttempts')} />
          <NumField label="异常并发自动回收" desc="0 表示关闭" value={draft.credentialInFlightLeaseMaxSecs} min={0} suffix="秒" onChange={set('credentialInFlightLeaseMaxSecs')} />
        </TwoCol>
      </CollapseSection>

      {/* 错误恢复 */}
      <CollapseSection icon={<Shield />} title="错误恢复 / 冷却" desc="不同错误类型的暂停策略与退避" defaultOpen>
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
          <NumField label="恢复时间错开比例" desc="防止多账号同时恢复" value={draft.credentialCooldownJitterPercent} min={0} max={100} suffix="%" onChange={set('credentialCooldownJitterPercent')} />
          <NumField label="恢复观察时间" desc="降频使用直到稳定" value={draft.credentialProbationSecs} min={0} suffix="秒" onChange={set('credentialProbationSecs')} />
        </TwoCol>
      </CollapseSection>

      {/* 账号调度权重 */}
      <CollapseSection icon={<Gauge />} title="账号选择权重" desc="优先使用哪些账号的调度参数">
        <TwoCol>
          <NumField label="近期错误敏感度" desc="越高近期错误越快影响选择" value={draft.schedulerErrorEwmaAlpha} min={0.01} max={1} step={0.01} suffix="系数" onChange={set('schedulerErrorEwmaAlpha')} />
          <NumField label="优先级权重" desc="数值越大，账号优先级差异对选择的影响越显著" value={draft.schedulerPriorityWeight} min={0} step={0.1} suffix="权重" onChange={set('schedulerPriorityWeight')} />
          <NumField label="当前负载权重" desc="数值越大，负载轻的账号越优先被选中" value={draft.schedulerLoadWeight} min={0} step={1} suffix="权重" onChange={set('schedulerLoadWeight')} />
          <NumField label="近期错误权重" desc="数值越大，近期出错多的账号越少被选中" value={draft.schedulerErrorWeight} min={0} step={1} suffix="权重" onChange={set('schedulerErrorWeight')} />
          <NumField label="响应耗时权重" desc="数值越大，响应慢的账号越少被选中；通常设较小值" value={draft.schedulerLatencyWeight} min={0} step={0.001} suffix="权重" onChange={set('schedulerLatencyWeight')} />
          <NumField label="恢复期降权" desc="处于观察期的账号被降低权重的程度" value={draft.schedulerProbationWeight} min={0} step={1} suffix="权重" onChange={set('schedulerProbationWeight')} />
          <NumField label="短时集中降权" desc="避免请求集中单一账号" value={draft.schedulerSelectionPressureWeight} min={0} step={1} suffix="权重" onChange={set('schedulerSelectionPressureWeight')} />
          <NumField label="长期使用次数权重" desc="数值越大，历史调度次数多的账号越少被选中，促进均衡" value={draft.schedulerTotalSelectionWeight} min={0} step={0.001} suffix="权重" onChange={set('schedulerTotalSelectionWeight')} />
          <NumField label="候选账号数量" desc="数值越大越分散" value={draft.schedulerTopK} min={1} max={100} suffix="个" onChange={set('schedulerTopK')} />
          <NumField label="失败诊断样本数" desc="调度失败时最多记录多少个账号样本，用于后台排查；0 表示不记录样本。" value={draft.selectionFailureSampleLimit} min={0} max={1000} suffix="个" onChange={set('selectionFailureSampleLimit')} />
          <TogField label="记录失败样本" desc="关闭后只保留失败原因统计，不记录具体账号样本。" checked={draft.selectionFailureRecordEnabled} onChange={set('selectionFailureRecordEnabled')} />
        </TwoCol>
      </CollapseSection>

      {/* 新账号预热 */}
      <CollapseSection icon={<Sparkles />} title="新账号预热" desc="新账号逐步参与请求，稳定后恢复正常">
        <TwoCol>
          <NumField label="预热请求数" desc="0 表示不预热" value={draft.credentialWarmupRequests} min={0} suffix="次" onChange={set('credentialWarmupRequests')} />
          <NumField label="单个预热账号参与比例" desc="每次调度时该预热账号被选中的概率上限" value={draft.credentialWarmupSelectionPercent} min={0} max={100} suffix="%" onChange={set('credentialWarmupSelectionPercent')} />
          <NumField label="预热账号总占比上限" desc="所有预热中账号合计流量不超过此比例" value={draft.credentialWarmupMaxSelectionPercent} min={0} max={100} suffix="%" onChange={set('credentialWarmupMaxSelectionPercent')} />
        </TwoCol>
      </CollapseSection>

      {/* 请求大小保护 */}
      <CollapseSection icon={<Wand2 />} title="请求大小保护" desc="压缩、大小阈值和处理时机">
        <div className="space-y-4">
          <TwoCol>
            <TogField label="启用请求压缩" desc="发送前尽量减少冗余内容" checked={draft.compressionEnabled} onChange={set('compressionEnabled')} />
            <TogField label="仅压缩空白字符" desc="只处理多余空白，风险低" checked={draft.whitespaceCompression} disabled={!draft.compressionEnabled} onChange={set('whitespaceCompression')} />
            <TogField label="启用大小保护" desc="统计请求大小并修正格式问题" checked={draft.payloadGuardEnabled} onChange={set('payloadGuardEnabled')} />
            <TogField label="外部账号也应用大小保护" desc="" checked={draft.payloadGuardExternalEnabled} disabled={!draft.payloadGuardEnabled} onChange={set('payloadGuardExternalEnabled')} />
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
          </div>
          <TwoCol>
            <NumField label="请求大小阈值" desc="超过此大小才触发处理（如 1048576 = 1 MB）；0 表示不按大小处理" value={draft.payloadGuardMaxBytes} min={0} suffix="字节" onChange={set('payloadGuardMaxBytes')} />
            <NumField label="安全余量" desc="处理目标比阈值小出的缓冲（如 65536 = 64 KB），避免裁剪后仍超限" value={draft.payloadGuardSafetyMarginBytes} min={0} suffix="字节" disabled={!payloadSizeLimitEnabled} onChange={set('payloadGuardSafetyMarginBytes')} />
          </TwoCol>
        </div>
      </CollapseSection>

      {/* 缓存展示控制（合并：命中展示 + 创建频次） */}
      <CollapseSection icon={<Zap />} title="缓存展示控制" desc="缓存读取口径与写入展示节奏">
        <div className="space-y-6">
          <div className="space-y-3">
            <div className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">命中展示参数</div>
            <TwoCol>
              <NumField label="缓存读取目标比例" desc="建议 0.95~0.99" value={draft.promptCacheTargetReadRatio} min={0} max={0.99} step={0.01} suffix="比例" onChange={set('promptCacheTargetReadRatio')} />
              <NumField label="输入估算放大倍数" desc="" value={draft.promptCacheTokenScale} min={1} max={3} step={0.1} suffix="倍" onChange={set('promptCacheTokenScale')} />
              <NumField label="输入展示上限" desc="0 表示不设上限" value={draft.promptCacheMaxSimulatedInputTokens} min={0} suffix="Token" onChange={set('promptCacheMaxSimulatedInputTokens')} />
              <NumField label="放大启用门槛" desc="" value={draft.promptCacheScaleMinInputTokens} min={0} suffix="Token" onChange={set('promptCacheScaleMinInputTokens')} />
              <NumField label="触顶扣减下限" desc="" value={draft.promptCacheCapJitterMinTokens} min={0} suffix="Token" onChange={set('promptCacheCapJitterMinTokens')} />
              <NumField label="触顶扣减上限" desc="" value={draft.promptCacheCapJitterMaxTokens} min={0} suffix="Token" onChange={set('promptCacheCapJitterMaxTokens')} />
            </TwoCol>
          </div>
          <div className="border-t border-border" />
          <div className="space-y-3">
            <div className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">本地缓存边界</div>
            <TwoCol>
              <NumField label="单账号条目上限" desc="每个账号最多保留多少个可复用缓存指纹，防止长会话无限增长。" value={draft.promptCacheMaxEntriesPerAccount} min={0} suffix="条" onChange={set('promptCacheMaxEntriesPerAccount')} />
              <NumField label="全局条目上限" desc="所有账号合计最多保留多少个缓存指纹，0 表示不按条目数限制。" value={draft.promptCacheMaxEntriesGlobal} min={0} suffix="条" onChange={set('promptCacheMaxEntriesGlobal')} />
              <NumField label="最长保留时间" desc="单条缓存指纹最多保留多久；实际不会超过上游缓存标记的时间。" value={draft.promptCacheEntryTtlSecs} min={1} suffix="秒" onChange={set('promptCacheEntryTtlSecs')} />
              <NumField label="估算内存上限" desc="达到估算上限后优先移除最久未使用的缓存指纹，0 表示不按内存估算限制。" value={draft.promptCacheEstimatedBytesLimit} min={0} suffix="字节" onChange={set('promptCacheEstimatedBytesLimit')} />
            </TwoCol>
          </div>
          <div className="border-t border-border" />
          <div className="space-y-3">
            <div className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">写入频次控制</div>
            <CacheCreationSection control={draft.promptCacheCreationControl} onChange={set('promptCacheCreationControl')} />
          </div>
        </div>
      </CollapseSection>

      {/* 自定义缓存路由 */}
      <CollapseSection icon={<Zap />} title="自定义缓存路由" desc="定义 /dfcache/{名称} 高缓存路由，固定前缀 /dfcache/">
        <DefinedCacheRoutesSection
          routes={draft.definedCacheRoutes}
          reported={draft.reportedUsage}
          onChange={(routes, reported) => setDraft((prev) => ({ ...prev, definedCacheRoutes: routes, reportedUsage: reported }))}
        />
      </CollapseSection>

      {/* 内容体积优化（合并：旧内容清理 + 当前内容兜底） */}
      <CollapseSection icon={<Wand2 />} title="内容体积优化" desc="历史消息与当前请求的压缩、裁剪策略">
        <div className="space-y-6">
          <div className="space-y-3">
            <div className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">历史消息清理</div>
            <PayloadHistorySection shaping={draft.payloadShaping} payloadSizeLimitEnabled={payloadSizeLimitEnabled} onChange={set('payloadShaping')} />
          </div>
          <div className="border-t border-border" />
          <div className="space-y-3">
            <div className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">当前请求兜底</div>
            <PayloadFallbackSection shaping={draft.payloadShaping} payloadShapingBranchEnabled={payloadSizeLimitEnabled && draft.payloadShaping.enabled} onChange={set('payloadShaping')} />
          </div>
        </div>
      </CollapseSection>

      {/* 用量展示规则 */}
      <CollapseSection icon={<Gauge />} title="用量展示规则" desc="不同入口返回给客户端的用量口径">
        <ReportedUsageSection reported={draft.reportedUsage} onChange={set('reportedUsage')} />
      </CollapseSection>

      {/* 模型映射 */}
      <CollapseSection icon={<Shield />} title="模型映射" desc="客户端模型名到实际模型的映射规则">
        <ModelMappingSection
          mapping={draft.modelMapping}
          capabilities={modelCapabilities.data}
          onChange={(m: ModelMappingConfig) => set('modelMapping')(m)}
        />
      </CollapseSection>

      {/* 兼容模式 */}
      <CollapseSection icon={<Shield />} title="兼容与统计" desc="接口兼容模式、模型解析策略">
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
                  : '按 Claude Code CLI 语义触发：thinking 模型、显式参数或本轮深度思考信号才会输出思考内容。'}
              </p>
            </div>
          </div>
          <TwoCol>
            <TogField label="整理思考内容" desc="把响应里的思考内容单独整理出来" checked={draft.extractThinking} onChange={set('extractThinking')} />
            <TogField label="显示处理告警" desc="把排查提示返回给客户端" checked={draft.exposeProxyWarnings} onChange={set('exposeProxyWarnings')} />
          </TwoCol>
          <TwoCol>
            <TogField label="发送真实 cachePoint" desc="把带缓存标记的工具发送给 Kiro 上游；上游不接受时会自动去掉后重试一次。" checked={draft.kiroCachePointEnabled} onChange={set('kiroCachePointEnabled')} />
            <TogField label="只处理工具缓存标记" desc="只根据工具上的缓存标记插入 cachePoint，不改写系统消息或历史消息。" checked={draft.kiroCachePointToolsOnly} disabled={!draft.kiroCachePointEnabled} onChange={set('kiroCachePointToolsOnly')} />
          </TwoCol>
          <TwoCol>
            <TogField label="记录 cachePoint 计划" desc="在系统日志中记录插入数量，方便排查上游请求体错误。" checked={draft.kiroCachePointRecordPlan} disabled={!draft.kiroCachePointEnabled} onChange={set('kiroCachePointRecordPlan')} />
          </TwoCol>
          <TwoCol>
            <NumField label="缓存命中判定阈值" desc="多少 Token 以上算缓存命中较高" value={draft.highCacheThreshold} min={0} suffix="Token" onChange={set('highCacheThreshold')} />
          </TwoCol>
        </div>
      </CollapseSection>

      {/* 底部操作栏 */}
      <div className={cn('flex items-center justify-between rounded-xl border border-border bg-muted/30 px-4 py-3')}>
        <span className="text-xs text-muted-foreground">保存后，新的请求会立即使用这些配置。</span>
        <Button size="sm" onClick={save} disabled={updateConfig.isPending}>
          {updateConfig.isPending ? <Spinner size="sm" /> : <Save className="h-4 w-4" />}
          保存配置
        </Button>
      </div>
    </PageContainer>
  )
}

// ─── 折叠分区 ─────────────────────────────────────────────────────────────────

function CollapseSection({
  icon,
  title,
  desc,
  defaultOpen = false,
  children,
}: {
  icon: React.ReactNode
  title: string
  desc: string
  defaultOpen?: boolean
  children: React.ReactNode
}) {
  const [open, setOpen] = useState(defaultOpen)
  return (
    <div className="rounded-xl border border-border bg-card overflow-hidden">
      <button
        type="button"
        className="flex w-full items-center gap-3 px-4 py-3 text-left transition-colors hover:bg-muted/40"
        onClick={() => setOpen((v) => !v)}
      >
        <span className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg border border-border bg-muted text-muted-foreground [&_svg]:size-4">
          {icon}
        </span>
        <div className="min-w-0 flex-1">
          <div className="text-sm font-semibold">{title}</div>
          <div className="text-xs text-muted-foreground">{desc}</div>
        </div>
        <span className="shrink-0 text-muted-foreground">
          {open ? <ChevronDown className="size-4" /> : <ChevronRight className="size-4" />}
        </span>
      </button>
      {open && <div className="border-t border-border px-4 pb-4 pt-3 space-y-4 animate-in fade-in-0 slide-in-from-top-1 duration-200">{children}</div>}
    </div>
  )
}
